//! Category suggestions from the booking history (GitHub #664): an
//! uncategorised booking is offered the category the user gave earlier
//! bookings of the same payee, without a rule and entirely offline.
//!
//! Nothing is stored. A suggestion is not a category: budgets, sums and the
//! "Ohne Kategorie" count only see confirmed ones. It is computed when asked
//! for and confirmed through `set_transaction_category`, which makes the
//! booking part of the history in turn.
//!
//! History = bookings the user categorised by hand (weight 1, including
//! categories brought along by an import) and rule bookings the user checked
//! (weight ½). `checked` alone proves nothing about the category: it is also
//! set on selection (auto review) and for a whole filter at once. A rule vote
//! only counts for a booking inside the rule's amount range and accounts:
//! outside them the rule left the booking out on purpose. Search-expression
//! rules keep such limits inside the expression, so their bookings never vote.
//! Split bookings, bookings between own accounts and those of deleted accounts
//! are left out. Targets are what "Ohne Kategorie" lists: no category, no
//! split, no transfer.
//!
//! Stages, most specific first:
//!  1. same counterparty IBAN *and* merchant: tells two payees of the same
//!     name apart. An IBAN never decides on its own, since payment services
//!     collect for many merchants through one account;
//!  2. same merchant (`MerchantResolver` key, any IBAN, also card payments),
//!     unless an earlier booking to the same IBAN had another category;
//!  3. only for a merchant without any history: the most similar bookings by
//!     their words (`similar.rs`), with stricter limits.
//!
//! Votes only come from bookings in the same direction (a refund never votes
//! for a purchase) and fade with the distance in time, so a category the user
//! switched to recently beats the one used years before.
//!
//! The history is read once and kept on the vault until its data changes
//! (`vault::cache`, like the list's `ListCache`), so moving through the
//! list costs no scan. Only the raw rows are read under the vault lock;
//! merchants are resolved after it is released.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use rusqlite::Connection;

use crate::error::{CoreError, CoreResult};
use crate::iban::normalize_iban;
use crate::merchants::{FlowKind, MerchantInfo, MerchantKind, MerchantResolver};
use crate::models::{CategorySource, MatchKind, Rule};
use crate::vault::Vault;

mod eval;
mod similar;
#[cfg(test)]
mod tests;

/// Bookings that must back the suggested category.
const MIN_SUPPORT: u32 = 2;
/// Weighted share of the suggested category among the stage's votes.
const MIN_SHARE: f64 = 0.75;
/// A vote counts half after this many days between the two bookings.
const HALF_LIFE_DAYS: f64 = 730.0;
/// Weight of a checked rule booking (a manual one counts 1).
const RULE_WEIGHT: f64 = 0.5;

/// How sure a stage must be; [`Limits::DEFAULT`] in the app.
#[derive(Clone, Copy)]
struct Limits {
    min_support: u32,
    min_share: f64,
    /// Stage 3: neighbours looked at (0 = stage off), their least similarity,
    /// and the bookings that must back the category.
    similar_k: usize,
    min_similarity: f64,
    similar_support: u32,
}

impl Limits {
    const DEFAULT: Limits = Limits {
        min_support: MIN_SUPPORT,
        min_share: MIN_SHARE,
        similar_k: 7,
        // measured on a real vault (8805 bookings, only earlier ones as history):
        // stage 3 right in 90.6 % of its suggestions, for 5.7 % of the bookings
        // whose merchant had no history
        min_similarity: 0.6,
        similar_support: 2,
    };
}

/// What a suggestion is based on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SuggestionBasis {
    /// Earlier bookings to the same IBAN and merchant.
    Iban,
    /// Earlier bookings at the same merchant.
    Merchant,
    /// Earlier bookings with similar text (the merchant has no history).
    Similar,
}

/// Category suggested for an uncategorised booking.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CategorySuggestion {
    pub booking_id: i64,
    pub category_id: i64,
    pub basis: SuggestionBasis,
    /// Other bookings with this category in the deciding stage.
    pub support: u32,
    /// Other bookings the deciding stage counted (all categories). The decision
    /// weighs them (time, manual vs. rule), so `support` may be below half.
    pub considered: u32,
}

/// An uncategorised booking to find a category for.
struct Target {
    id: i64,
    account_id: i64,
    amount_cents: i64,
    day: Option<i32>,
    iban: String,
    merchant: String,
    /// Words of counterparty, purpose and booking text (stage 3).
    words: Vec<String>,
}

/// One earlier booking's vote: by hand (`rule_id` none, weight 1) or by that
/// rule (weight [`RULE_WEIGHT`]).
#[derive(Clone, Copy)]
struct Vote {
    booking_id: i64,
    category_id: i64,
    day: Option<i32>,
    rule_id: Option<i64>,
}

/// A history row as read under the vault lock; `rule_id` as in [`Vote`].
struct Row {
    id: i64,
    date: String,
    amount_cents: i64,
    counterparty: String,
    iban: String,
    purpose: String,
    booking_text: String,
    category_id: i64,
    rule_id: Option<i64>,
}

/// Everything suggestions need from one state of the database.
pub(crate) struct History {
    by_merchant: HashMap<(bool, String), Vec<Vote>>,
    by_iban: HashMap<(bool, String, String), Vec<Vote>>,
    text: similar::TextIndex,
    /// Keyword and regex rules by id (their limits decide which votes count).
    rules: HashMap<i64, Rule>,
}

/// Key a merchant can be matched by; `None` for a payment service with
/// nothing behind it (it collects for any merchant) and for empty texts.
fn merchant_key(m: &MerchantInfo) -> Option<String> {
    let named = m
        .key
        .strip_prefix("x:")
        .unwrap_or(&m.key)
        .chars()
        .any(char::is_alphanumeric);
    (named && m.kind != MerchantKind::PaymentService).then(|| m.key.clone())
}

fn day_number(date: &str) -> Option<i32> {
    jiff::civil::Date::strptime("%Y-%m-%d", date)
        .ok()
        .map(crate::dates::day_number)
}

fn load_targets(vault: &Vault, conn: &Connection, ids: &[i64]) -> CoreResult<Vec<Target>> {
    let mut seen = HashSet::new();
    let ids: Vec<i64> = ids.iter().copied().filter(|id| seen.insert(*id)).collect();
    let mut split =
        conn.prepare("SELECT EXISTS(SELECT 1 FROM transaction_splits WHERE transaction_id = ?1)")?;
    let mut out = Vec::new();
    let mut bookings = vault.bookings_by_ids(conn, &ids)?;
    // in the order asked (the list's order is newest first)
    bookings.sort_by_key(|b| ids.iter().position(|id| *id == b.id));
    for b in bookings {
        // the flow is the list's: own IBAN, transfer wording, payment-service pairs
        if b.category_id.is_some()
            || !matches!(b.category_source, CategorySource::None)
            || b.flow == FlowKind::Transfer
            || split.query_row([b.id], |r| r.get::<_, bool>(0))?
        {
            continue;
        }
        if let Some(merchant) = merchant_key(&b.merchant) {
            out.push(Target {
                id: b.id,
                account_id: b.account_id,
                amount_cents: b.amount_cents,
                day: day_number(&b.booking_date),
                iban: normalize_iban(&b.counterparty_iban),
                merchant,
                words: similar::words(&[&b.counterparty, &b.purpose, &b.booking_text]),
            });
        }
    }
    Ok(out)
}

fn read_rows(conn: &Connection) -> CoreResult<Vec<Row>> {
    let mut stmt = conn.prepare(
        "SELECT t.booking_date, t.amount_cents, t.counterparty, t.counterparty_iban, t.purpose,
                t.category_id, CASE WHEN t.category_source = 'rule' THEN t.rule_id END, t.id,
                t.booking_text
         FROM transactions t
         JOIN accounts a ON a.id = t.account_id
         JOIN categories c ON c.id = t.category_id
         WHERE t.deleted_ms IS NULL AND a.deleted_ms IS NULL AND c.deleted_ms IS NULL
           AND (t.category_source = 'manual'
                OR (t.category_source = 'rule' AND t.checked = 1 AND t.rule_id IS NOT NULL))
           AND NOT EXISTS (SELECT 1 FROM transaction_splits s WHERE s.transaction_id = t.id)",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Row {
                date: r.get(0)?,
                amount_cents: r.get(1)?,
                counterparty: r.get(2)?,
                iban: r.get(3)?,
                purpose: r.get(4)?,
                category_id: r.get(5)?,
                rule_id: r.get(6)?,
                id: r.get(7)?,
                booking_text: r.get(8)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

impl History {
    fn build(rows: Vec<Row>, rules: HashMap<i64, Rule>, own_ibans: &HashSet<String>) -> Self {
        let mut by_merchant: HashMap<(bool, String), Vec<Vote>> = HashMap::new();
        let mut by_iban: HashMap<(bool, String, String), Vec<Vote>> = HashMap::new();
        let mut texts = Vec::with_capacity(rows.len());
        for row in rows {
            let iban = normalize_iban(&row.iban);
            if own_ibans.contains(&iban) {
                continue;
            }
            let income = row.amount_cents > 0;
            let vote = Vote {
                booking_id: row.id,
                category_id: row.category_id,
                day: day_number(&row.date),
                rule_id: row.rule_id,
            };
            let words = similar::words(&[&row.counterparty, &row.purpose, &row.booking_text]);
            texts.push((vote, income, words));
            let Some(merchant) =
                merchant_key(&MerchantResolver::resolve(&row.counterparty, &row.purpose))
            else {
                continue;
            };
            if !iban.is_empty() {
                by_iban
                    .entry((income, iban, merchant.clone()))
                    .or_default()
                    .push(vote);
            }
            by_merchant
                .entry((income, merchant))
                .or_default()
                .push(vote);
        }
        History {
            by_merchant,
            by_iban,
            text: similar::TextIndex::build(texts),
            rules,
        }
    }

    fn suggest(&self, t: &Target) -> Option<CategorySuggestion> {
        self.suggest_with(t, Limits::DEFAULT, |_| false)
    }

    /// `suggest` with other limits, leaving out the votes `skip` names.
    fn suggest_with(
        &self,
        t: &Target,
        limits: Limits,
        skip: impl Fn(&Vote) -> bool,
    ) -> Option<CategorySuggestion> {
        let admit = |v: &&Vote| {
            !skip(v)
                && v.rule_id.is_none_or(|id| {
                    self.rules
                        .get(&id)
                        .is_some_and(|r| r.admits(t.account_id, t.amount_cents))
                })
        };
        let income = t.amount_cents > 0;
        let iban_votes: Vec<&Vote> = self
            .by_iban
            .get(&(income, t.iban.clone(), t.merchant.clone()))
            .map(|v| v.iter().filter(admit).collect())
            .unwrap_or_default();
        let (support, share) = (limits.min_support, limits.min_share);
        if let Some(s) = decide(
            iban_votes.iter().map(|v| (*v, 1.0)),
            t,
            SuggestionBasis::Iban,
            support,
            share,
        ) {
            return Some(s);
        }
        let votes: Vec<&Vote> = self
            .by_merchant
            .get(&(income, t.merchant.clone()))
            .map(|v| v.iter().filter(admit).collect())
            .unwrap_or_default();
        if votes.is_empty() {
            // a merchant without history: bookings with similar words, weighted by similarity
            let near = self.text.nearest(
                &t.words,
                income,
                limits.similar_k,
                limits.min_similarity,
                |v| admit(&v),
            );
            return decide(
                near.into_iter(),
                t,
                SuggestionBasis::Similar,
                limits.similar_support,
                share,
            );
        }
        let s = decide(
            votes.iter().map(|v| (*v, 1.0)),
            t,
            SuggestionBasis::Merchant,
            support,
            share,
        )?;
        // a booking to this very IBAN with another category outweighs the name
        iban_votes
            .iter()
            .all(|v| v.category_id == s.category_id)
            .then_some(s)
    }
}

/// The category the votes agree on for `t`; `None` when too few or too mixed.
/// Each vote comes with a factor (stage 3: its similarity, else 1).
fn decide<'a>(
    votes: impl Iterator<Item = (&'a Vote, f64)>,
    t: &Target,
    basis: SuggestionBasis,
    min_support: u32,
    min_share: f64,
) -> Option<CategorySuggestion> {
    let mut tally: HashMap<i64, (f64, u32)> = HashMap::new();
    let (mut total, mut considered) = (0.0, 0u32);
    for (v, factor) in votes {
        let fade = match (t.day, v.day) {
            (Some(a), Some(b)) => 0.5f64.powf(f64::from((a - b).abs()) / HALF_LIFE_DAYS),
            _ => 1.0,
        };
        let w = factor
            * fade
            * if v.rule_id.is_some() {
                RULE_WEIGHT
            } else {
                1.0
            };
        total += w;
        considered += 1;
        let e = tally.entry(v.category_id).or_default();
        e.0 += w;
        e.1 += 1;
    }
    // heaviest first; equal weight: more bookings, then the lower id (stable)
    let (category_id, (weight, support)) = tally.into_iter().max_by(|a, b| {
        a.1.0
            .total_cmp(&b.1.0)
            .then(a.1.1.cmp(&b.1.1))
            .then(b.0.cmp(&a.0))
    })?;
    (support >= min_support && total > 0.0 && weight / total >= min_share).then_some(
        CategorySuggestion {
            booking_id: t.id,
            category_id,
            basis,
            support,
            considered,
        },
    )
}

impl Vault {
    /// The history for the current data: the cached one, or read again after
    /// a change (only the SQL part under the vault lock).
    fn suggestion_history(&self) -> CoreResult<Arc<History>> {
        let db = self.db()?;
        let stamp = self.data_stamp(&db);
        if let Some(history) = self.suggestions.get(stamp) {
            return Ok(history);
        }
        let rows = read_rows(&db)?;
        let rules = Vault::load_rules(&db)?
            .into_iter()
            .filter(|r| matches!(r.match_kind, MatchKind::Keyword | MatchKind::Regex))
            .map(|r| (r.id, r))
            .collect();
        let own_ibans = Vault::own_ibans(&db)?;
        // inside an open transaction a rollback would not lower the count
        let keep = db.is_autocommit();
        drop(db);
        let history = Arc::new(History::build(rows, rules, &own_ibans));
        if keep {
            self.suggestions.put(stamp, history.clone());
        }
        Ok(history)
    }
}

#[uniffi::export]
impl Vault {
    /// Category suggestions from the booking history for the given bookings
    /// (#664). Only uncategorised bookings with a clear history get one; the
    /// others are left out of the result. The first call after a change of
    /// the data reads the whole history (off the main thread); later calls
    /// reuse it.
    pub fn suggest_categories(
        &self,
        booking_ids: Vec<i64>,
    ) -> Result<Vec<CategorySuggestion>, CoreError> {
        if booking_ids.is_empty() {
            return Ok(Vec::new());
        }
        let history = self.suggestion_history()?;
        let targets = load_targets(self, &*self.db()?, &booking_ids)?;
        Ok(targets.iter().filter_map(|t| history.suggest(t)).collect())
    }
}
