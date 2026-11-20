//! Booking list (filter, paging, enrichment), booking edits and CSV export.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rusqlite::{ToSql, params, params_from_iter};

use super::Vault;
use super::cache::DataStamp;
use crate::error::CoreError;
use crate::i18n::trf;
use crate::iban::normalize_iban;
use crate::merchants::{FlowKind, MerchantResolver, classify, clean_purpose, payment_method};
use crate::models::*;
use crate::money::parse_cents;

/// Booking between two own accounts. A transfer-named category always wins.
/// Both transfer heuristics — the counterparty-IBAN check and `classify()`'s
/// transfer wording in the booking text (MoneyMoney types internal transfers
/// "Umbuchung", comdirect "Übertrag / Überweisung") — only apply to bookings
/// the user has not categorised and that `classify()` did not already see as
/// investment / perk: an explicit category ("Sonstige Einnahmen" on a child's
/// account) or an ETF savings plan on a depot settlement account must keep its
/// flow (Issue #94).
fn is_own_transfer(t: &Booking, own_ibans: &HashSet<String>, transfer_category: bool) -> bool {
    if transfer_category {
        return true;
    }
    if t.category_id.is_some() || matches!(t.flow, FlowKind::Investment | FlowKind::Perk) {
        return false;
    }
    let iban = normalize_iban(&t.counterparty_iban);
    !iban.is_empty() && own_ibans.contains(&iban)
}

/// Compact search text that is an IBAN (CCdd…) or a digit-only account number.
fn is_account_number_query(compact: &str) -> bool {
    if compact.len() < 6 || !compact.chars().all(|c| c.is_ascii_alphanumeric()) {
        return false;
    }
    let b = compact.as_bytes();
    if b.len() >= 15
        && b[0].is_ascii_uppercase()
        && b[1].is_ascii_uppercase()
        && b[2].is_ascii_digit()
        && b[3].is_ascii_digit()
    {
        return true;
    }
    compact.chars().all(|c| c.is_ascii_digit())
}

/// WHERE clause "booking `t` belongs to the categories `cat_match`" (e.g. `= ?3`,
/// `IS NULL`), split-aware like `planning::effective_sql`: an unsplit booking by
/// its own category, a split booking only by the categories of its parts.
fn category_match_sql(cat_match: &str) -> String {
    format!(
        "((t.category_id {cat_match} AND NOT EXISTS (SELECT 1 FROM transaction_splits sp WHERE sp.transaction_id = t.id)) \
         OR EXISTS (SELECT 1 FROM transaction_splits sp WHERE sp.transaction_id = t.id AND sp.category_id {cat_match}))"
    )
}

/// Adds merchant, flow kind, payment method and clean purpose to a booking.
/// `paypal`: its account is a PayPal account (see `BookingFinisher`).
pub(crate) fn enrich(t: &mut Booking, paypal: bool, top_role: &str) {
    t.merchant = MerchantResolver::resolve(&t.counterparty, &t.purpose);
    t.merchant_name = t.merchant.name.clone();
    t.merchant_domain = Some(t.merchant.domain.clone()).filter(|d| !d.is_empty());
    t.merchant_svg_path = Some(t.merchant.svg_path.clone()).filter(|d| !d.is_empty());
    t.merchant_hex_color = t.merchant.color_hex.clone();
    // Only the category role classifies: a custom category named like a built-in has no role
    // and must not be treated as an investment by its name.
    let role = top_role;
    t.flow = classify(
        t.amount_cents,
        &t.counterparty,
        &t.purpose,
        &t.booking_text,
        &t.merchant,
        role,
        t.account_kind,
    );
    t.payment_method = payment_method(
        &t.booking_text,
        &t.purpose,
        t.account_kind,
        t.amount_cents,
        paypal,
    );
    t.clean_purpose = clean_purpose(&t.purpose);
    t.recurrence_key = crate::recurring::series_key(t);
}

/// Tables behind a booking row; a deleted category or rule reads as none
/// (what the old SET NULL did).
const BOOKING_FROM_SQL: &str = "FROM transactions t JOIN accounts a ON a.id = t.account_id
     LEFT JOIN categories cat ON cat.id = t.category_id AND cat.deleted_ms IS NULL
     LEFT JOIN rules r ON r.id = t.rule_id AND r.deleted_ms IS NULL";

/// SELECT list read by `booking_from_row`; `share_sql` is the KPI share (column 21).
fn booking_columns(share_sql: &str) -> String {
    format!(
        "t.id, t.account_id, a.name, t.booking_date, t.value_date, t.amount_cents, t.currency, t.counterparty,
         t.counterparty_iban, t.purpose, t.booking_text, cat.id, t.category_source, r.id, t.checked, a.kind,
         (SELECT count(*) FROM booking_documents d WHERE d.booking_id = t.id), t.note,
         t.purpose_code, t.gvc, t.salary_flag, {share_sql},
         ((t.external_id LIKE 'pending-%' OR t.external_id LIKE 'advanzia-hold-%')), t.item_refs, t.end_to_end_id, t.mandate_id, t.creditor_id, t.external_id"
    )
}

/// (KPI share, raw booking) of one `booking_columns` row; `BookingFinisher` fills the rest.
fn booking_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<(i64, Booking)> {
    let cat: Option<i64> = r.get(11)?;
    let src: String = r.get(12)?;
    let share: i64 = r.get(21)?;
    Ok((
        share,
        Booking {
            id: r.get(0)?,
            account_id: r.get(1)?,
            account_name: r.get(2)?,
            booking_date: r.get(3)?,
            value_date: r.get(4)?,
            amount_cents: r.get(5)?,
            currency: r.get(6)?,
            counterparty: r.get(7)?,
            counterparty_iban: r.get(8)?,
            purpose: r.get(9)?,
            booking_text: r.get(10)?,
            category_id: cat,
            category_name: String::new(),
            category_path: String::new(),
            category_color_hex: String::new(),
            category_symbol: String::new(),
            category_source: match src.as_str() {
                "rule" => CategorySource::Rule,
                "manual" => CategorySource::Manual,
                _ => CategorySource::None,
            },
            rule_id: r.get(13)?,
            checked: r.get::<_, i64>(14)? != 0,
            merchant: MerchantResolver::placeholder(),
            merchant_name: String::new(),
            merchant_domain: None,
            merchant_svg_path: None,
            merchant_hex_color: String::new(),
            payment_method: String::new(),
            flow: FlowKind::Consumption,
            clean_purpose: String::new(),
            account_kind: AccountKind::from_str(&r.get::<_, String>(15)?),
            document_count: r.get::<_, i64>(16)? as u32,
            note: r.get(17)?,
            purpose_code: r.get(18)?,
            gvc: r.get(19)?,
            salary_flag: r.get::<_, Option<i64>>(20)?.map(|f| f != 0),
            is_salary: false,
            recurrence_key: String::new(),
            is_pending: r.get::<_, i64>(22)? != 0,
            item_refs: r.get(23)?,
            end_to_end_id: r.get(24)?,
            mandate_id: r.get(25)?,
            creditor_id: r.get(26)?,
            external_id: r.get(27)?,
            tags: Vec::new(),
        },
    ))
}

/// Fills the tags of `bookings` with one query per 500 rows (never per row).
pub(crate) fn attach_tags(
    db: &rusqlite::Connection,
    bookings: &mut [Booking],
) -> Result<(), CoreError> {
    if bookings.is_empty() {
        return Ok(());
    }
    let ids: Vec<i64> = bookings.iter().map(|b| b.id).collect();
    let mut tags = crate::tags::tags_for_bookings(db, &ids)?;
    for b in bookings {
        b.tags = tags.remove(&b.id).unwrap_or_default();
    }
    Ok(())
}

/// A split booking has no category of its own (#654): its row names the categories of its
/// parts instead of reading "Ohne Kategorie". One query per 500 rows, only for bookings
/// without a category.
pub(crate) fn attach_split_categories(
    db: &rusqlite::Connection,
    bookings: &mut [Booking],
) -> Result<(), CoreError> {
    for chunk in bookings.chunks_mut(500) {
        let ids: Vec<String> = chunk
            .iter()
            .filter(|b| b.category_id.is_none())
            .map(|b| b.id.to_string())
            .collect();
        if ids.is_empty() {
            continue;
        }
        let mut st = db.prepare(&format!(
            "SELECT sp.transaction_id, c.name, c.color_hex, c.sf_symbol
               FROM transaction_splits sp JOIN categories c ON c.id = sp.category_id
              WHERE c.deleted_ms IS NULL AND sp.transaction_id IN ({}) ORDER BY sp.id",
            ids.join(",")
        ))?;
        let mut names: HashMap<i64, (Vec<String>, String, String)> = HashMap::new();
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            ))
        })?;
        for row in rows {
            let (tx, name, color, symbol) = row?;
            let e = names.entry(tx).or_default();
            if !e.0.contains(&name) {
                e.0.push(name);
            }
            if e.1.is_empty() {
                e.1 = color;
            }
            if e.2.is_empty() {
                e.2 = symbol;
            }
        }
        for b in chunk.iter_mut().filter(|b| b.category_id.is_none()) {
            if let Some((list, color, symbol)) = names.remove(&b.id) {
                b.category_name = list.join(", ");
                b.category_path = b.category_name.clone();
                b.category_color_hex = color;
                b.category_symbol = symbol;
            }
        }
    }
    Ok(())
}

/// Everything a raw booking row needs to read like the booking list: category
/// names, merchant, flow (salary, own transfers, loans), domain overrides.
/// Only depends on the vault's data, so `ListCache` keeps it between calls.
pub(super) struct BookingFinisher {
    language: crate::i18n::Lang,
    category_flows: HashMap<i64, super::categories::CategoryFlow>,
    paths: HashMap<i64, (String, String, String, String)>,
    salary_ibans: HashSet<String>,
    overrides: HashMap<String, String>,
    own_ibans: HashSet<String>,
    /// Both legs of detected bank ↔ payment-service transfers (#302).
    pair_transfers: HashSet<i64>,
    loan_links: HashMap<i64, i64>,
    paypal_accounts: HashSet<i64>,
}

impl BookingFinisher {
    fn load(db: &rusqlite::Connection) -> Result<Self, CoreError> {
        let paths = Vault::category_paths(db)?;
        let category_flows = Vault::category_flows(db)?;
        let loan_links = crate::loans::link_map(db)?;
        let salary_ibans = crate::salary::marked_salary_ibans(db)?;
        let overrides = crate::insights::load_overrides(db)?;
        // Own IBANs: uncategorised bookings to / from them are transfers (Issue #94).
        let own_ibans = Vault::own_ibans(db)?;
        let pair_transfers = Vault::payment_service_transfer_ids(db)?;
        // PayPal accounts: linked to a PayPal connection, or imported without
        // one (MoneyMoney) and named after PayPal. Other payment services
        // (Uberspace, Amazon orders) must not read "PayPal".
        let paypal_accounts: HashSet<i64> = {
            let mut st = db.prepare(
                "SELECT a.id FROM accounts a
                   LEFT JOIN bank_connections c ON c.id = a.connection_id
                  WHERE a.kind = 'payment_service'
                    AND (c.connector = 'paypal'
                         OR (c.id IS NULL AND (lower(a.name) LIKE '%paypal%'
                                               OR lower(a.bank_name) LIKE '%paypal%')))",
            )?;

            st.query_map([], |r| r.get::<_, i64>(0))?
                .collect::<Result<_, _>>()?
        };
        Ok(Self {
            language: crate::i18n::current(),
            category_flows,
            paths,
            salary_ibans,
            overrides,
            own_ibans,
            pair_transfers,
            loan_links,
            paypal_accounts,
        })
    }

    /// With `visible` accounts (local interface, #136), a payment linked to a
    /// loan outside them reads as a transfer: the loan itself must not show.
    fn finish(&self, t: &mut Booking, visible: Option<&[i64]>) {
        if let Some((name, path, color, sym)) = t.category_id.and_then(|c| self.paths.get(&c)) {
            t.category_name = name.clone();
            t.category_path = path.clone();
            t.category_color_hex = color.clone();
            t.category_symbol = sym.clone();
        }
        let category_flow = t.category_id.and_then(|c| self.category_flows.get(&c));
        let top_role = category_flow.map(|c| c.top_role.as_str()).unwrap_or("");
        let transfer_category = category_flow.is_some_and(|c| c.is_transfer);
        enrich(t, self.paypal_accounts.contains(&t.account_id), top_role);
        t.is_salary = crate::salary::is_salary(t, &self.salary_ibans);
        // A salary stays income although its booking text names a transfer
        // (comdirect: "Übertrag / Überweisung"); a transfer category still wins.
        if t.is_salary && t.flow == FlowKind::Transfer {
            t.flow = FlowKind::Income;
        }
        // An explicit category also beats the transfer wording of the booking
        // text: MoneyMoney types internal transfers "Umbuchung" (Issue #94), so
        // a categorised credit ("Sonstige Einnahmen") on the child's account
        // must stay income instead of silently dropping out of the Einnahmen
        // KPI. Uncategorised bookings keep the wording heuristic.
        if t.flow == FlowKind::Transfer && t.category_id.is_some() && !transfer_category {
            t.flow = if t.amount_cents > 0 {
                FlowKind::Income
            } else {
                FlowKind::Consumption
            };
        }
        if is_own_transfer(t, &self.own_ibans, transfer_category)
            || (self.pair_transfers.contains(&t.id)
                && t.category_id.is_none()
                && !matches!(t.flow, FlowKind::Investment | FlowKind::Perk))
        {
            t.flow = FlowKind::Transfer;
        }
        if let Some(d) = self.overrides.get(&t.merchant.key) {
            t.merchant.domain = d.clone();
            t.merchant.domain_candidates = vec![d.clone()];
            t.merchant.domain_source = "override".into();
            t.merchant_domain = Some(d.clone());
        }
        if let Some(loan) = self.loan_links.get(&t.id) {
            t.flow = match visible {
                Some(v) if !v.contains(loan) => FlowKind::Transfer,
                _ => FlowKind::Loan,
            };
        }
    }
}

impl Vault {
    /// IBANs of the live accounts, normalised (`DE89…`, no blanks); a blank
    /// one is left out, so it never matches bookings without a counterparty IBAN.
    pub(crate) fn own_ibans(
        db: &rusqlite::Connection,
    ) -> Result<std::collections::HashSet<String>, CoreError> {
        let mut st =
            db.prepare("SELECT iban FROM accounts WHERE iban IS NOT NULL AND deleted_ms IS NULL")?;
        let ibans = st
            .query_map([], |r| r.get::<_, String>(0))?
            .filter_map(Result::ok)
            .map(|i| normalize_iban(&i))
            .filter(|i| !i.is_empty())
            .collect();
        Ok(ibans)
    }

    /// WHERE clause (plus optional CTE, bound arguments and the KPI share
    /// expression) of a `BookingFilter`; shared by the list and bulk updates.
    #[allow(clippy::type_complexity)]
    pub(crate) fn filter_clauses(
        db: &rusqlite::Connection,
        filter: &BookingFilter,
    ) -> Result<(String, String, Vec<Box<dyn ToSql>>, String), CoreError> {
        let mut wh: Vec<String> = vec![LIVE_BOOKINGS_SQL.into()];
        let mut args: Vec<Box<dyn ToSql>> = vec![];
        let mut cte = String::new();
        if !filter.account_ids.is_empty() {
            let ph: Vec<String> = filter
                .account_ids
                .iter()
                .map(|id| {
                    args.push(Box::new(*id));
                    format!("?{}", args.len())
                })
                .collect();
            let mut clause = format!("t.account_id IN ({})", ph.join(","));
            // Loan view: also surface bookings of other accounts (e.g. the Girokonto's
            // monthly "Zins / Tilgung" debits) that reference the loan's IBAN / account number.
            let loans: Vec<(i64, String, String)> = {
                let mut q = db
                    .prepare("SELECT id, iban, account_number FROM accounts WHERE kind = 'loan' AND deleted_ms IS NULL")?;
                let rows = q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
                rows.collect::<Result<Vec<_>, _>>()?
            };
            let loan_ids: Vec<i64> = loans.iter().map(|l| l.0).collect();
            for (id, iban, number) in loans.iter().filter(|l| filter.account_ids.contains(&l.0)) {
                let digits: String = number.chars().filter(|c| c.is_ascii_digit()).collect();
                let trimmed = digits.trim_start_matches('0').to_string();
                let mut ors: Vec<String> = Vec::new();
                if iban.len() >= 15 {
                    args.push(Box::new(iban.clone()));
                    ors.push(format!("t.counterparty_iban = ?{0} OR REPLACE(t.purpose,' ','') LIKE '%' || ?{0} || '%'", args.len()));
                }
                if trimmed.len() >= 6 {
                    args.push(Box::new(trimmed.clone()));
                    let n = args.len();
                    ors.push(format!(
                        "t.counterparty_iban LIKE '%' || ?{n} OR t.purpose LIKE '%' || ?{n} || '%'"
                    ));
                }
                // Only exact references (IBAN / full loan number) – no keyword or short-suffix
                // heuristics: those pulled unrelated bookings of every account into a loan.
                if !ors.is_empty() {
                    let own: Vec<String> = loan_ids.iter().map(|i| i.to_string()).collect();
                    clause.push_str(&format!(
                        " OR (t.account_id NOT IN ({}) AND ({}))",
                        own.join(","),
                        ors.join(" OR ")
                    ));
                }
                let _ = id;
            }
            // manual loans (#36): bookings linked by hand or by the loan's expression
            clause.push_str(&format!(
                " OR t.id IN (SELECT transaction_id FROM loan_links WHERE source != 'excluded' AND loan_account_id IN ({}))",
                filter.account_ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",")
            ));
            wh.push(format!("({clause})"));
        }
        // Category filters follow `planning::effective_sql`: a split booking belongs
        // to the categories of its parts (never to its own category), and only the
        // parts of the filtered category count in the KPI sums (`share_sql`).
        let cat_match: Option<String> = if filter.uncategorized_only {
            Some("IS NULL".into())
        } else if let Some(cat) = filter.category_id {
            args.push(Box::new(cat));
            let n = args.len();
            if filter.include_subcategories {
                cte = format!(
                    "WITH RECURSIVE sub(id) AS (SELECT ?{n} UNION ALL SELECT c.id FROM categories c JOIN sub ON c.parent_id = sub.id WHERE c.deleted_ms IS NULL) "
                );
                Some("IN (SELECT id FROM sub)".into())
            } else {
                Some(format!("= ?{n}"))
            }
        } else {
            None
        };
        let share_sql = match &cat_match {
            Some(m) => {
                wh.push(category_match_sql(m));
                format!(
                    "COALESCE((SELECT SUM(sp.amount_cents) FROM transaction_splits sp WHERE sp.transaction_id = t.id AND sp.category_id {m}), t.amount_cents)"
                )
            }
            None => "t.amount_cents".to_string(),
        };
        if let Some(from) = filter.date_from.as_ref().filter(|s| !s.is_empty()) {
            args.push(Box::new(from.clone()));
            wh.push(format!("t.booking_date >= ?{}", args.len()));
        }
        if let Some(to) = filter.date_to.as_ref().filter(|s| !s.is_empty()) {
            args.push(Box::new(to.clone()));
            wh.push(format!("t.booking_date <= ?{}", args.len()));
        }
        // tags (#228): every listed tag must be on the booking (AND)
        if filter.untagged_only {
            wh.push(
                "NOT EXISTS (SELECT 1 FROM transaction_tags x JOIN tags g ON g.id = x.tag_id \
                 WHERE x.transaction_id = t.id AND g.deleted_ms IS NULL)"
                    .into(),
            );
        } else {
            let mut tag_ids = filter.tag_ids.clone();
            tag_ids.sort_unstable();
            tag_ids.dedup();
            for tag in tag_ids {
                args.push(Box::new(tag));
                wh.push(format!(
                    "EXISTS (SELECT 1 FROM transaction_tags x WHERE x.transaction_id = t.id AND x.tag_id = ?{})",
                    args.len()
                ));
            }
        }
        match filter.checked {
            None => {}
            Some(true) => wh.push("t.checked != 0".into()),
            Some(false) => wh.push("t.checked = 0".into()),
        }
        match filter.direction {
            BookingDirection::All => {}
            BookingDirection::Income => wh.push("t.amount_cents > 0".into()),
            BookingDirection::Expense => wh.push("t.amount_cents < 0".into()),
        }
        // amount range (#357): by the absolute amount, the direction has its own filter
        if let Some(min) = filter.amount_min_cents {
            args.push(Box::new(min));
            wh.push(format!("ABS(t.amount_cents) >= ?{}", args.len()));
        }
        if let Some(max) = filter.amount_max_cents {
            args.push(Box::new(max));
            wh.push(format!("ABS(t.amount_cents) <= ?{}", args.len()));
        }
        if let Some(q) = filter
            .search
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            // Spaced IBANs / account numbers ("DE89 3704 …"): one compact match
            // against counterparty and own-account numbers beats AND-of-fragments.
            // Only IBAN-shaped (CCdd…) or digit-only queries – not "Miete 500".
            let compact_all: String = q
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
                .to_uppercase();
            let account_like = is_account_number_query(&compact_all);
            if account_like {
                let like = format!(
                    "%{}%",
                    compact_all
                        .replace('\\', "\\\\")
                        .replace('%', "\\%")
                        .replace('_', "\\_")
                );
                args.push(Box::new(like));
                let n = args.len();
                wh.push(format!(
                    "(REPLACE(UPPER(t.counterparty_iban), ' ', '') LIKE ?{n} ESCAPE '\\' \
                     OR REPLACE(UPPER(IFNULL(a.iban, '')), ' ', '') LIKE ?{n} ESCAPE '\\' \
                     OR REPLACE(UPPER(IFNULL(a.account_number, '')), ' ', '') LIKE ?{n} ESCAPE '\\')"
                ));
            } else {
                let mut ors = Vec::new();
                for term in q.split_whitespace() {
                    let like = format!(
                        "%{}%",
                        term.replace('\\', "\\\\")
                            .replace('%', "\\%")
                            .replace('_', "\\_")
                    );
                    args.push(Box::new(like));
                    let n = args.len();
                    let mut alt = format!(
                        "(t.counterparty LIKE ?{n} ESCAPE '\\' OR t.purpose LIKE ?{n} ESCAPE '\\' OR t.booking_text LIKE ?{n} ESCAPE '\\' \
                         OR t.counterparty_iban LIKE ?{n} ESCAPE '\\' OR cat.name LIKE ?{n} ESCAPE '\\' OR a.name LIKE ?{n} ESCAPE '\\'"
                    );
                    let compact: String = term
                        .chars()
                        .filter(|c| !c.is_whitespace())
                        .collect::<String>()
                        .to_uppercase();
                    if compact.chars().any(|c| c.is_ascii_digit()) {
                        let clike = format!(
                            "%{}%",
                            compact
                                .replace('\\', "\\\\")
                                .replace('%', "\\%")
                                .replace('_', "\\_")
                        );
                        args.push(Box::new(clike));
                        let c = args.len();
                        alt.push_str(&format!(
                            " OR REPLACE(UPPER(t.counterparty_iban), ' ', '') LIKE ?{c} ESCAPE '\\' \
                             OR REPLACE(UPPER(IFNULL(a.iban, '')), ' ', '') LIKE ?{c} ESCAPE '\\' \
                             OR REPLACE(UPPER(IFNULL(a.account_number, '')), ' ', '') LIKE ?{c} ESCAPE '\\'"
                        ));
                        if let Ok(cents) = parse_cents(term) {
                            args.push(Box::new(cents.abs()));
                            alt.push_str(&format!(" OR ABS(t.amount_cents) = ?{}", args.len()));
                        }
                    }
                    alt.push(')');
                    ors.push(alt);
                }
                wh.push(ors.join(" AND "));
            }
        }
        let where_sql = wh.join(" AND ");
        Ok((cte, where_sql, args, share_sql))
    }

    fn set_checked_ids(
        tx: &rusqlite::Connection,
        ids: &[i64],
        checked: bool,
    ) -> Result<(), CoreError> {
        let now = crate::dates::now_ms();
        for chunk in ids.chunks(500) {
            let ph = vec!["?"; chunk.len()].join(",");
            let sql =
                format!("UPDATE transactions SET checked = ?1, checked_ms = ?2 WHERE id IN ({ph})");
            let mut owned: Vec<Box<dyn ToSql>> = vec![Box::new(checked as i64), Box::new(now)];
            owned.extend(chunk.iter().map(|id| Box::new(*id) as Box<dyn ToSql>));
            tx.execute(&sql, params_from_iter(owned.iter().map(|b| b.as_ref())))?;
        }
        Ok(())
    }
}

impl Vault {
    /// Bookings of `filter` after the keyset cursor `after` = (booking_date, id),
    /// newest first, at most `limit` (0 = all); for the local read-only interface
    /// (#136). SQL cuts the page, so only its rows are enriched (`list_transactions`
    /// enriches the whole filter on every page). `scope`, `merchant_key`, `limit`,
    /// `offset` and `whole_days` of `filter` are ignored. `only_accounts` is a hard
    /// limit on the booking's own account: the loan view behind
    /// `filter.account_ids` also pulls in bookings of other accounts. `visible`:
    /// the accounts the caller may see (`BookingFinisher::finish`). Takes the
    /// caller's connection, so several reads can share one lock.
    pub(crate) fn booking_page_after(
        &self,
        db: &rusqlite::Connection,
        filter: &BookingFilter,
        only_accounts: &[i64],
        visible: &[i64],
        after: Option<(&str, i64)>,
        limit: u32,
    ) -> Result<Vec<Booking>, CoreError> {
        if only_accounts.is_empty() {
            return Ok(Vec::new());
        }
        let (cte, mut where_sql, mut args, share_sql) = Self::filter_clauses(db, filter)?;
        let ph: Vec<String> = only_accounts
            .iter()
            .map(|id| {
                args.push(Box::new(*id));
                format!("?{}", args.len())
            })
            .collect();
        // `+`: walk `tx_date_id` newest first and stop at the page end, instead of
        // collecting and sorting every booking of the accounts.
        where_sql.push_str(&format!(" AND +t.account_id IN ({})", ph.join(",")));
        if let Some((date, id)) = after {
            args.push(Box::new(date.to_string()));
            let d = args.len();
            args.push(Box::new(id));
            where_sql.push_str(&format!(
                " AND (t.booking_date < ?{d} OR (t.booking_date = ?{d} AND t.id < ?{}))",
                args.len()
            ));
        }
        let mut sql = booking_select(&cte, &where_sql, &share_sql);
        if limit > 0 {
            sql.push_str(&format!(" LIMIT {limit}"));
        }
        self.finished_bookings(
            db,
            &sql,
            params_from_iter(args.iter().map(|b| b.as_ref())),
            Some(visible),
        )
    }

    /// Enriched bookings (with tags) of the rows `where_sql` selects, newest
    /// first. Only these rows are finished, so small results stay cheap
    /// (`list_transactions` finishes every row of its filter).
    pub(crate) fn bookings_where(
        &self,
        db: &rusqlite::Connection,
        where_sql: &str,
        params: impl rusqlite::Params,
    ) -> Result<Vec<Booking>, CoreError> {
        let sql = booking_select(
            "",
            &format!("{LIVE_BOOKINGS_SQL} AND ({where_sql})"),
            "t.amount_cents",
        );
        self.finished_bookings(db, &sql, params, None)
    }

    /// `bookings_where` for explicit ids, newest first.
    pub(crate) fn bookings_by_ids(
        &self,
        db: &rusqlite::Connection,
        ids: &[i64],
    ) -> Result<Vec<Booking>, CoreError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        self.bookings_where(db, &format!("t.id IN ({})", sql_int_list(ids)), [])
    }

    /// Finishes every booking of `filter` newest first and hands it to `visit`
    /// with its KPI share (the full amount, or with a category filter only the
    /// split parts of that category). Applies what SQL can't (`keeps`); when the
    /// class cache is filled, the rows it drops are never finished. Collects
    /// nothing and loads no tags, so callers that count or sum skip both.
    /// `limit`, `offset` and `whole_days` are ignored.
    pub(crate) fn visit_bookings(
        &self,
        db: &rusqlite::Connection,
        filter: &BookingFilter,
        mut visit: impl FnMut(i64, Booking),
    ) -> Result<(), CoreError> {
        let (cte, where_sql, args, share_sql) = Self::filter_clauses(db, filter)?;
        let sql = booking_select(&cte, &where_sql, &share_sql);
        let args = params_from_iter(args.iter().map(|b| b.as_ref()));
        let classes = filters_by_class(filter)
            .then(|| self.cached_booking_classes(db))
            .flatten();
        let finisher = self.booking_finisher(db)?;
        let mut stmt = db.prepare(&sql)?;
        let mut rows = stmt.query(args)?;
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let class = classes.as_ref().and_then(|c| c.get(&id));
            if class.is_some_and(|c| !keeps(filter, c.flow, &c.merchant)) {
                continue;
            }
            let (share, mut t) = booking_from_row(row)?;
            finisher.finish(&mut t, None);
            if keeps(filter, t.flow, &t.merchant.key) {
                visit(share, t);
            }
        }
        Ok(())
    }

    /// Every finished booking of `filter`, without tags.
    pub(crate) fn collect_bookings(
        &self,
        db: &rusqlite::Connection,
        filter: &BookingFilter,
    ) -> Result<Vec<Booking>, CoreError> {
        let mut out = Vec::new();
        self.visit_bookings(db, filter, |_, b| out.push(b))?;
        Ok(out)
    }

    /// Finished bookings (with tags) of a `booking_select`.
    fn finished_bookings(
        &self,
        db: &rusqlite::Connection,
        sql: &str,
        params: impl rusqlite::Params,
        visible: Option<&[i64]>,
    ) -> Result<Vec<Booking>, CoreError> {
        let finisher = self.booking_finisher(db)?;
        let mut stmt = db.prepare(sql)?;
        let mut out = stmt
            .query_map(params, booking_from_row)?
            .map(|row| {
                let (_, mut t) = row?;
                finisher.finish(&mut t, visible);
                Ok(t)
            })
            .collect::<Result<Vec<_>, CoreError>>()?;
        attach_tags(db, &mut out)?;
        attach_split_categories(db, &mut out)?;
        Ok(out)
    }
}

/// `filter` has parts SQL can't apply: they need each booking's flow or merchant.
pub(crate) fn filters_by_class(filter: &BookingFilter) -> bool {
    filter.scope != BookingScope::All || filter.merchant_key.is_some() || filter.uncategorized_only
}

/// Whether a booking of `flow` and `merchant` passes the parts of `filter` SQL
/// can't apply: scope, merchant and the own transfers "Ohne Kategorie" leaves out.
fn keeps(filter: &BookingFilter, flow: FlowKind, merchant: &str) -> bool {
    let in_scope = match filter.scope {
        BookingScope::All => true,
        BookingScope::Everyday => !matches!(flow, FlowKind::Investment | FlowKind::Perk),
        BookingScope::Investments => matches!(flow, FlowKind::Investment | FlowKind::Perk),
    };
    // Detected own transfers are no open work for "Ohne Kategorie" (#302).
    let hidden_transfer = filter.uncategorized_only && flow == FlowKind::Transfer;
    in_scope && !hidden_transfer && filter.merchant_key.as_deref().is_none_or(|k| k == merchant)
}

/// Bookings that are neither deleted nor on a deleted account (`t`, `a`).
pub(crate) const LIVE_BOOKINGS_SQL: &str = "t.deleted_ms IS NULL AND a.deleted_ms IS NULL";

/// Entries the bank reported as not booked yet (`pending-…` external id).
pub(crate) const PENDING_SQL: &str =
    "(t.external_id LIKE 'pending-%' OR t.external_id LIKE 'advanzia-hold-%')";

/// `values` as an inlined SQL list ("1,2,3"): integers can't inject, and one
/// statement keeps its ORDER BY without chunking around the parameter limit.
pub(crate) fn sql_int_list(values: &[i64]) -> String {
    let values: Vec<String> = values.iter().map(i64::to_string).collect();
    values.join(",")
}

/// The `booking_columns` SELECT of `where_sql` in list order, newest first.
fn booking_select(cte: &str, where_sql: &str, share_sql: &str) -> String {
    format!(
        "{cte}SELECT {} {BOOKING_FROM_SQL} WHERE {where_sql} ORDER BY t.booking_date DESC, t.id DESC",
        booking_columns(share_sql)
    )
}

#[uniffi::export]
impl Vault {
    /// Marks every booking matching `filter` as checked / unchecked in one
    /// SQL update (GitHub #236; with a scope or merchant filter the ids come
    /// from the enriched bookings first). Returns the ids that actually changed,
    /// so the caller can undo exactly those.
    pub fn set_checked_for_filter(
        &self,
        filter: BookingFilter,
        checked: bool,
    ) -> Result<Vec<i64>, CoreError> {
        // Filters on the enriched booking (`filters_by_class`): take the ids from
        // `visit_bookings`, so the update touches exactly what the list shows.
        if filters_by_class(&filter) {
            let mut ids = Vec::new();
            self.visit_bookings(&*self.db()?, &filter, |_, b| {
                if b.checked != checked {
                    ids.push(b.id);
                }
            })?;
            self.set_checked_for_ids(ids.clone(), checked)?;
            return Ok(ids);
        }
        let mut db = self.db()?;
        let (cte, where_sql, args, _) = Self::filter_clauses(&db, &filter)?;
        let tx = db.transaction()?;
        let ids: Vec<i64> = {
            let sql = format!(
                "{cte}SELECT t.id FROM transactions t JOIN accounts a ON a.id = t.account_id \
                 LEFT JOIN categories cat ON cat.id = t.category_id AND cat.deleted_ms IS NULL \
                 WHERE {where_sql} AND (t.checked != 0) != ?{}",
                args.len() + 1
            );
            let mut stmt = tx.prepare(&sql)?;
            let mut all: Vec<&dyn ToSql> = args.iter().map(|b| b.as_ref()).collect();
            let flag = checked as i64;
            all.push(&flag);
            let rows = stmt.query_map(all.as_slice(), |r| r.get::<_, i64>(0))?;
            rows.collect::<Result<_, _>>()?
        };
        Self::set_checked_ids(&tx, &ids, checked)?;
        tx.commit()?;
        Ok(ids)
    }

    /// Sets `checked` for explicit ids (undo of `set_checked_for_filter`).
    pub fn set_checked_for_ids(&self, ids: Vec<i64>, checked: bool) -> Result<(), CoreError> {
        let mut db = self.db()?;
        let tx = db.transaction()?;
        Self::set_checked_ids(&tx, &ids, checked)?;
        tx.commit()?;
        Ok(())
    }

    /// Number of bookings matching `filter` (`limit`/`offset` ignored) as one SQL
    /// count, without enriching every row like `list_transactions` (#266). Scope,
    /// merchant and "Ohne Kategorie" (which leaves out detected own transfers,
    /// #302) depend on each booking's class: those filters count like the list.
    pub fn count_transactions(&self, filter: BookingFilter) -> Result<u32, CoreError> {
        if filters_by_class(&filter) {
            return Ok(self.list_page(filter, false)?.total_count);
        }
        let db = self.db()?;
        let (cte, where_sql, args, _) = Self::filter_clauses(&db, &filter)?;
        let sql = format!(
            "{cte}SELECT count(*) FROM transactions t JOIN accounts a ON a.id = t.account_id \
             LEFT JOIN categories cat ON cat.id = t.category_id AND cat.deleted_ms IS NULL \
             WHERE {where_sql}"
        );
        let n: i64 = db.query_row(
            &sql,
            params_from_iter(args.iter().map(|b| b.as_ref())),
            |r| r.get(0),
        )?;
        Ok(n as u32)
    }

    pub fn list_transactions(&self, filter: BookingFilter) -> Result<BookingPage, CoreError> {
        self.list_page(filter, true)
    }

    /// `flow_other_currencies` for the plain selection of `selection_flow`: accounts and period.
    /// `respect_in_total`: in a totals context (#337) only the effective total
    /// set contributes — the same rule as the EUR sums this figure supplements.
    #[uniffi::method(default(respect_in_total = false))]
    pub fn selection_flow_other_currencies(
        &self,
        account_ids: Vec<i64>,
        date_from: Option<String>,
        date_to: Option<String>,
        respect_in_total: bool,
    ) -> Result<Vec<CurrencyFlow>, CoreError> {
        self.flow_other_currencies(BookingFilter {
            account_ids,
            date_from,
            date_to,
            respect_in_total,
            ..Default::default()
        })
    }

    /// The KPI tiles of the currencies other than EUR (#1049) for the same filter: what the page
    /// sums leave out. Empty for a single account; no scan when no booking has another currency.
    /// In a totals context (#337) only the effective total set contributes.
    pub fn flow_other_currencies(
        &self,
        filter: BookingFilter,
    ) -> Result<Vec<CurrencyFlow>, CoreError> {
        if filter.account_ids.len() == 1 {
            return Ok(Vec::new());
        }
        // one guard for both reads, so eligibility and bookings stay consistent
        let db = self.db()?;
        if !crate::planning::has_other_currencies(&db)? {
            return Ok(Vec::new());
        }
        // the same effective set as the EUR page sums (#337): the totals set
        // intersected with the selection
        let totals = totals_set(&db, &filter)?;
        let mut per: std::collections::BTreeMap<String, SelectionFlow> = Default::default();
        self.visit_bookings(&db, &filter, |share, t| {
            if !crate::planning::counts_in_sums(filter.account_ids.len(), &t.currency)
                && totals
                    .as_ref()
                    .is_none_or(|set| set.contains(&t.account_id))
            {
                per.entry(t.currency.to_uppercase())
                    .or_default()
                    .add(t.flow, share);
            }
        })?;
        Ok(per
            .into_iter()
            .map(|(currency, f)| CurrencyFlow {
                currency,
                consumption_cents: f.consumption_cents,
                invested_cents: f.invested_cents,
                income_cents: f.income_cents,
            })
            .filter(|f| f.consumption_cents != 0 || f.invested_cents != 0 || f.income_cents != 0)
            .collect())
    }
}

impl Vault {
    /// `(booking id, KPI share)` of every booking `filter` keeps, the parts SQL can't
    /// apply (`keeps`) taken from `booking_classes`; for aggregates over a filter that
    /// `filters_by_class` (tag totals, #228).
    pub(crate) fn kept_shares(
        &self,
        db: &rusqlite::Connection,
        filter: &BookingFilter,
    ) -> Result<Vec<(i64, i64)>, CoreError> {
        let classes = self.booking_classes(db)?;
        let (cte, where_sql, args, share_sql) = Self::filter_clauses(db, filter)?;
        let sql = format!("{cte}SELECT t.id, {share_sql} {BOOKING_FROM_SQL} WHERE {where_sql}");
        let mut stmt = db.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args.iter().map(|b| b.as_ref())), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, share) = row?;
            // the classes cover every live booking of this connection's data
            if classes
                .get(&id)
                .is_some_and(|c| keeps(filter, c.flow, &c.merchant))
            {
                out.push((id, share));
            }
        }
        Ok(out)
    }
}

/// What filters and sums need of a booking that only exists after
/// `BookingFinisher::finish`.
pub(crate) struct BookingClass {
    pub(crate) flow: FlowKind,
    pub(crate) merchant: Arc<str>,
}

type BookingClasses = Arc<HashMap<i64, BookingClass>>;

/// Per-vault work every booking list repeats, valid for one state of the data
/// ([`DataStamp`](super::cache::DataStamp): any write that can move a flow, a merchant or a
/// category, and a restore, changes it).
#[derive(Default)]
pub(crate) struct ListCache(Mutex<ListCacheState>);

#[derive(Default)]
struct ListCacheState {
    stamp: DataStamp,
    finisher: Option<Arc<BookingFinisher>>,
    classes: Option<BookingClasses>,
}

impl ListCache {
    /// The entries built at `stamp`; emptied first when the data changed since.
    fn at(&self, stamp: DataStamp) -> MutexGuard<'_, ListCacheState> {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if state.stamp != stamp {
            *state = ListCacheState {
                stamp,
                ..Default::default()
            };
        }
        state
    }

    /// Stores into the entries of `stamp`, unless a transaction is open: its
    /// rollback would not lower the count, so entries built from its data would
    /// outlive it.
    fn keep(
        &self,
        db: &rusqlite::Connection,
        stamp: DataStamp,
        put: impl FnOnce(&mut ListCacheState),
    ) {
        if db.is_autocommit() {
            put(&mut self.at(stamp));
        }
    }
}

impl Vault {
    /// The `BookingFinisher` of the vault's current data. Built outside the cache lock:
    /// the caller holds the connection, so nobody else builds one meanwhile.
    fn booking_finisher(
        &self,
        db: &rusqlite::Connection,
    ) -> Result<Arc<BookingFinisher>, CoreError> {
        let stamp = self.data_stamp(db);
        if let Some(finisher) = self
            .list_cache
            .at(stamp)
            .finisher
            .clone()
            .filter(|f| f.language == crate::i18n::current())
        {
            return Ok(finisher);
        }
        let finisher = Arc::new(BookingFinisher::load(db)?);
        self.list_cache
            .keep(db, stamp, |s| s.finisher = Some(finisher.clone()));
        Ok(finisher)
    }

    /// Flow and merchant key of every live booking. Built once with the full enrichment
    /// (the cost `list_transactions` used to pay on every call) and reused until a write
    /// changes the vault: switching account, period, filter or scrolling then reads ids and
    /// amounts only.
    pub(crate) fn booking_classes(
        &self,
        db: &rusqlite::Connection,
    ) -> Result<BookingClasses, CoreError> {
        if let Some(classes) = self.cached_booking_classes(db) {
            return Ok(classes);
        }
        let stamp = self.data_stamp(db);
        let finisher = self.booking_finisher(db)?;
        let (cte, where_sql, args, share_sql) =
            Self::filter_clauses(db, &BookingFilter::default())?;
        let sql = format!(
            "{cte}SELECT {} {BOOKING_FROM_SQL} WHERE {where_sql}",
            booking_columns(&share_sql)
        );
        let mut stmt = db.prepare(&sql)?;
        let rows = stmt.query_map(
            params_from_iter(args.iter().map(|b| b.as_ref())),
            booking_from_row,
        )?;
        // Thousands of bookings share a few hundred merchants: one key each.
        let mut merchants: HashSet<Arc<str>> = HashSet::new();
        let mut classes = HashMap::new();
        for row in rows {
            let (_, mut t) = row?;
            finisher.finish(&mut t, None);
            let merchant = merchants
                .get(t.merchant.key.as_str())
                .cloned()
                .unwrap_or_else(|| {
                    let key: Arc<str> = t.merchant.key.into();
                    merchants.insert(key.clone());
                    key
                });
            classes.insert(
                t.id,
                BookingClass {
                    flow: t.flow,
                    merchant,
                },
            );
        }
        let classes = Arc::new(classes);
        self.list_cache
            .keep(db, stamp, |s| s.classes = Some(classes.clone()));
        Ok(classes)
    }

    /// `booking_classes` if no write happened since they were built. For callers
    /// that only use them to skip work: building them finishes every booking.
    pub(crate) fn cached_booking_classes(
        &self,
        db: &rusqlite::Connection,
    ) -> Option<BookingClasses> {
        self.list_cache.at(self.data_stamp(db)).classes.clone()
    }

    /// `list_transactions`; `with_rows: false` returns the sums and counts without rows
    /// (`selection_flow`). The sums come from `booking_classes` and an id/amount query,
    /// and only the returned page is read in full and enriched.
    pub(crate) fn list_page(
        &self,
        filter: BookingFilter,
        with_rows: bool,
    ) -> Result<BookingPage, CoreError> {
        let db = self.db()?;
        // Exports and other (almost) unlimited lists read every row anyway.
        let big_page = with_rows && (filter.limit == 0 || filter.limit > 5_000);
        let cached = if !big_page {
            self.list_page_cached(&db, &filter, with_rows)?
        } else {
            None
        };
        let mut page = match cached {
            Some(page) => page,
            None => self.list_page_full(&db, &filter)?,
        };
        // same lock as the page: a sync in between cannot shift the balances (#577)
        if filter.with_balances {
            (page.running_balances, page.day_balances) =
                super::balances::page_balances(&db, &filter.account_ids, &page.transactions)?;
        }
        Ok(page)
    }

    /// The light path of `list_page`; `None` when the full one has to answer (a booking the
    /// class cache does not know).
    fn list_page_cached(
        &self,
        db: &rusqlite::Connection,
        filter: &BookingFilter,
        with_rows: bool,
    ) -> Result<Option<BookingPage>, CoreError> {
        let classes = self.booking_classes(db)?;
        let (cte, where_sql, args, share_sql) = Self::filter_clauses(db, filter)?;
        let order = if with_rows {
            " ORDER BY t.booking_date DESC, t.id DESC"
        } else {
            ""
        };
        let sql = format!(
            "{cte}SELECT t.id, t.booking_date, t.account_id, {share_sql}, t.currency {BOOKING_FROM_SQL} WHERE {where_sql}{order}"
        );
        let mut stmt = db.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args.iter().map(|b| b.as_ref())), |r| {
            // the date only cuts the page: counts and sums skip the String
            let date = if with_rows { r.get(1)? } else { String::new() };
            Ok((
                r.get::<_, i64>(0)?,
                date,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        let (mut income, mut expense, mut consumption, mut investment, mut perk) =
            (0i64, 0i64, 0i64, 0i64, 0i64);
        let mut flow = SelectionFlow::default();
        let mut kept: Vec<(i64, String)> = Vec::new();
        let mut total = 0u32;
        let totals = totals_set(db, filter)?;
        for row in rows {
            let (id, date, account_id, share, currency) = row?;
            let Some(class) = classes.get(&id) else {
                return Ok(None);
            };
            if !keeps(filter, class.flow, &class.merchant) {
                continue;
            }
            // Sums skip other currencies unless one account is selected (#1049),
            // and only accounts of the totals set (#337) contribute to the sums;
            // counts and rows stay complete either way.
            let counts_for_totals = totals.as_ref().is_none_or(|set| set.contains(&account_id));
            if counts_for_totals
                && crate::planning::counts_in_sums(filter.account_ids.len(), &currency)
            {
                match kpi_side(class.flow, share) {
                    KpiSide::Income => income += share,
                    KpiSide::Expense => expense += share,
                    KpiSide::Neutral => {}
                }
                match class.flow {
                    FlowKind::Consumption => consumption += share,
                    FlowKind::Investment => investment += share,
                    FlowKind::Perk => perk += share,
                    _ => {}
                }
                flow.add(class.flow, share);
            }
            total += 1;
            if with_rows {
                kept.push((id, date));
            }
        }
        let mut txs: Vec<Booking> = Vec::new();
        if with_rows {
            let limit = if filter.limit == 0 {
                usize::MAX
            } else {
                filter.limit as usize
            };
            let start = (filter.offset as usize).min(kept.len());
            let mut end = start.saturating_add(limit).min(kept.len());
            if filter.whole_days && end > start {
                // sorted by day: the rest of the page's last day follows directly
                while end < kept.len() && kept[end].1 == kept[end - 1].1 {
                    end += 1;
                }
            }
            let page_ids: Vec<i64> = kept[start..end].iter().map(|(id, _)| *id).collect();
            if !page_ids.is_empty() {
                let (cte, mut where_sql, mut args, share_sql) = Self::filter_clauses(db, filter)?;
                // One JSON parameter instead of one per id: no variable limit to hit.
                let json = format!(
                    "[{}]",
                    page_ids
                        .iter()
                        .map(i64::to_string)
                        .collect::<Vec<_>>()
                        .join(",")
                );
                args.push(Box::new(json));
                where_sql.push_str(&format!(
                    " AND t.id IN (SELECT value FROM json_each(?{}))",
                    args.len()
                ));
                txs = self.finished_bookings(
                    db,
                    &booking_select(&cte, &where_sql, &share_sql),
                    params_from_iter(args.iter().map(|b| b.as_ref())),
                    None,
                )?;
            }
        }
        Ok(Some(BookingPage {
            transactions: txs,
            total_count: total,
            income_cents: income,
            expense_cents: expense,
            consumption_cents: consumption,
            investment_cents: investment,
            perk_cents: perk,
            flow,
            running_balances: Vec::new(),
            day_balances: Vec::new(),
        }))
    }

    /// Every row of the filter read and enriched; the reference the light path must match.
    fn list_page_full(
        &self,
        db: &rusqlite::Connection,
        filter: &BookingFilter,
    ) -> Result<BookingPage, CoreError> {
        let start = filter.offset as usize;
        let end = match filter.limit {
            0 => usize::MAX,
            limit => start.saturating_add(limit as usize),
        };
        let (mut income, mut expense, mut consumption, mut investment, mut perk) =
            (0i64, 0i64, 0i64, 0i64, 0i64);
        let mut flow = SelectionFlow::default();
        let mut txs: Vec<Booking> = Vec::new();
        let mut total = 0usize;
        let totals = totals_set(db, filter)?;
        self.visit_bookings(db, filter, |share, t| {
            // One KPI definition (`kpi_side`) for the page sums and `SelectionFlow`;
            // transfers between own accounts are neutral (else counted on both sides).
            // `share` is the full amount, or with a category filter only the split
            // parts of that category. Only accounts of the totals set (#337)
            // contribute to the sums — the booking itself always stays in the
            // list and in the counts — and sums skip other currencies unless
            // one account is selected (#1049); the rows stay.
            let counts_for_totals = totals
                .as_ref()
                .is_none_or(|set| set.contains(&t.account_id));
            if counts_for_totals
                && crate::planning::counts_in_sums(filter.account_ids.len(), &t.currency)
            {
                match kpi_side(t.flow, share) {
                    KpiSide::Income => income += share,
                    KpiSide::Expense => expense += share,
                    KpiSide::Neutral => {}
                }
                match t.flow {
                    FlowKind::Consumption => consumption += share,
                    FlowKind::Investment => investment += share,
                    FlowKind::Perk => perk += share,
                    _ => {}
                }
                flow.add(t.flow, share);
            }
            // Only the page is kept. `whole_days` adds the rest of its last day:
            // sorted by day, it follows directly.
            let in_page = (start..end).contains(&total)
                || (filter.whole_days
                    && txs.last().is_some_and(|l| l.booking_date == t.booking_date));
            if in_page {
                txs.push(t);
            }
            total += 1;
        })?;
        // only the returned page carries its tags
        attach_tags(db, &mut txs)?;
        attach_split_categories(db, &mut txs)?;
        Ok(BookingPage {
            transactions: txs,
            total_count: total as u32,
            income_cents: income,
            expense_cents: expense,
            consumption_cents: consumption,
            investment_cents: investment,
            perk_cents: perk,
            flow,
            running_balances: Vec::new(),
            day_balances: Vec::new(),
        })
    }
}

/// The effective total set behind `BookingFilter.respect_in_total` (#337):
/// `None` for a proof context (the default). An explicit `account_ids`
/// selection narrows the set — an empty result means "none" (zero sums),
/// never "all accounts".
fn totals_set(
    db: &rusqlite::Connection,
    filter: &BookingFilter,
) -> Result<Option<std::collections::HashSet<i64>>, CoreError> {
    if !filter.respect_in_total {
        return Ok(None);
    }
    let allowed = crate::user_groups::total_account_ids(db)?;
    if filter.account_ids.is_empty() {
        return Ok(Some(allowed));
    }
    let want: std::collections::HashSet<i64> = filter.account_ids.iter().copied().collect();
    Ok(Some(allowed.intersection(&want).copied().collect()))
}

#[uniffi::export]
impl Vault {
    /// Manual categorisation (wins over rules). `None` = uncategorised.
    pub fn set_transaction_category(
        &self,
        transaction_id: i64,
        category_id: Option<i64>,
    ) -> Result<(), CoreError> {
        let db = self.db()?;
        let n = db.execute(
            "UPDATE transactions SET category_id = ?1, rule_id = NULL, category_source = CASE WHEN ?1 IS NULL THEN 'none' ELSE 'manual' END, category_ms = ?3 WHERE id = ?2",
            params![category_id, transaction_id, crate::dates::now_ms()],
        )?;
        if n == 0 {
            return Err(CoreError::NotFound {
                what: trf("Buchung {0}", &[&transaction_id]),
            });
        }
        Ok(())
    }

    /// Booking day of one booking ("2026-11-09"); `None` if it does not exist.
    /// Lets the list load far enough back to show a booking it is asked to reveal.
    pub fn booking_date(&self, booking_id: i64) -> Result<Option<String>, CoreError> {
        let db = self.db()?;
        let mut stmt = db.prepare("SELECT booking_date FROM transactions WHERE id = ?1")?;
        let mut rows = stmt.query_map([booking_id], |r| r.get::<_, String>(0))?;
        Ok(rows.next().transpose()?)
    }

    /// Distinct counterparty names of all bookings, most recent first, at most
    /// `limit` (bug report #301: a log line naming a payee is scrubbed of it,
    /// whatever the list currently shows).
    pub fn counterparty_names(&self, limit: u32) -> Result<Vec<String>, CoreError> {
        let db = self.db()?;
        let mut stmt = db.prepare(
            "SELECT counterparty FROM transactions
              WHERE deleted_ms IS NULL AND counterparty <> ''
              GROUP BY counterparty ORDER BY MAX(booking_date) DESC LIMIT ?1",
        )?;
        let names = stmt
            .query_map([limit], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(names)
    }

    pub fn set_transaction_checked(
        &self,
        transaction_id: i64,
        checked: bool,
    ) -> Result<(), CoreError> {
        self.db()?.execute(
            "UPDATE transactions SET checked = ?1, checked_ms = ?3 WHERE id = ?2",
            params![checked as i64, transaction_id, crate::dates::now_ms()],
        )?;
        Ok(())
    }

    /// Manual text correction of a booking's counterparty / purpose.
    pub fn update_transaction_text(
        &self,
        transaction_id: i64,
        counterparty: String,
        purpose: String,
    ) -> Result<(), CoreError> {
        let n = self.db()?.execute(
            "UPDATE transactions SET counterparty = ?1, purpose = ?2, text_ms = ?4 WHERE id = ?3",
            params![
                counterparty.trim(),
                purpose.trim(),
                transaction_id,
                crate::dates::now_ms()
            ],
        )?;
        if n == 0 {
            return Err(CoreError::NotFound {
                what: trf("Buchung {0}", &[&transaction_id]),
            });
        }
        Ok(())
    }

    /// Free-text user note of a booking (empty clears it).
    pub fn update_transaction_note(
        &self,
        transaction_id: i64,
        note: String,
    ) -> Result<(), CoreError> {
        let n = self.db()?.execute(
            "UPDATE transactions SET note = ?1, note_ms = ?3 WHERE id = ?2",
            params![note.trim(), transaction_id, crate::dates::now_ms()],
        )?;
        if n == 0 {
            return Err(CoreError::NotFound {
                what: trf("Buchung {0}", &[&transaction_id]),
            });
        }
        Ok(())
    }

    /// Exports all bookings matching `filter` (its paging is ignored) as UTF-8 CSV
    /// with BOM, semicolon-separated, for Excel.
    pub fn export_bookings_csv(&self, filter: BookingFilter) -> Result<String, CoreError> {
        let mut out = String::from(
            "\u{FEFF}Datum;Konto;Name;IBAN;Verwendungszweck;Kategorie;Betrag;Währung;Tags\r\n",
        );
        // One pass: paging through `list_transactions` would finish the whole
        // filter again for every page.
        let db = self.db()?;
        let mut bookings = self.collect_bookings(&db, &filter)?;
        attach_tags(&db, &mut bookings)?;
        attach_split_categories(&db, &mut bookings)?;
        for b in &bookings {
            let row = [
                csv_field(&b.booking_date),
                csv_text(&b.account_name),
                csv_text(&b.counterparty),
                csv_text(&b.counterparty_iban),
                csv_text(&b.purpose),
                csv_text(&b.category_path),
                csv_amount(b.amount_cents),
                csv_text(&b.currency),
                csv_text(
                    &b.tags
                        .iter()
                        .map(|t| t.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                ),
            ];
            out.push_str(&row.join(";"));
            out.push_str("\r\n");
        }
        Ok(out)
    }
}

use crate::export::{csv_amount, csv_field, csv_text};

#[cfg(test)]
mod paging_tests {
    use super::*;
    use std::sync::Arc;

    fn demo() -> (Arc<Vault>, std::path::PathBuf) {
        let p = std::env::temp_dir().join(format!("girocrab-page-{}.db", uuid::Uuid::new_v4()));
        let v = Vault::open(p.to_string_lossy().to_string(), vec![7u8; 32]).unwrap();
        v.seed_demo_data().unwrap();
        (v, p)
    }

    #[test]
    fn counterparty_names_are_distinct_newest_first_and_limited() {
        let (v, p) = demo();
        let all = v
            .list_transactions(BookingFilter::default())
            .unwrap()
            .transactions;
        let names = v.counterparty_names(1000).unwrap();
        let mut unique = names.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), names.len(), "distinct");
        assert!(!names.iter().any(String::is_empty));
        for b in all.iter().filter(|b| !b.counterparty.is_empty()) {
            assert!(
                names.contains(&b.counterparty),
                "{} missing",
                b.counterparty
            );
        }
        let newest = all.iter().find(|b| !b.counterparty.is_empty()).unwrap();
        let newest_date = &newest.booking_date;
        let first = all.iter().find(|b| b.counterparty == names[0]).unwrap();
        assert_eq!(&first.booking_date, newest_date);
        assert_eq!(v.counterparty_names(3).unwrap().len(), 3);
        let _ = std::fs::remove_file(p);
    }

    fn debug(bookings: &[Booking]) -> Vec<String> {
        bookings.iter().map(|b| format!("{b:?}")).collect()
    }

    #[test]
    fn a_page_is_a_slice_of_the_full_list() {
        let (v, p) = demo();
        let all = v.list_transactions(BookingFilter::default()).unwrap();
        let page = |offset, whole_days| {
            v.list_transactions(BookingFilter {
                limit: 5,
                offset,
                whole_days,
                ..Default::default()
            })
            .unwrap()
        };
        let third = page(3, false);
        assert_eq!(debug(&third.transactions), debug(&all.transactions[3..8]));
        assert_eq!(third.total_count, all.total_count);
        assert_eq!(third.income_cents, all.income_cents);
        assert_eq!(third.expense_cents, all.expense_cents);
        assert!(page(all.total_count + 1, true).transactions.is_empty());
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn bookings_by_ids_read_like_the_list_newest_first() {
        let (v, p) = demo();
        let all = v.list_transactions(BookingFilter::default()).unwrap();
        let picked: Vec<Booking> = all.transactions.iter().step_by(3).cloned().collect();
        let ids: Vec<i64> = picked.iter().rev().map(|b| b.id).collect();
        let by_ids = v.bookings_by_ids(&v.db().unwrap(), &ids).unwrap();
        assert_eq!(debug(&by_ids), debug(&picked));
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn selection_flow_sums_like_the_list() {
        let (v, p) = demo();
        let all = v.list_transactions(BookingFilter::default()).unwrap();
        let flow = v.selection_flow(vec![], None, None).unwrap();
        assert_eq!(format!("{flow:?}"), format!("{:?}", all.flow));
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn whole_day_pages_never_split_a_day_and_add_up() {
        let (v, p) = demo();
        let all = v
            .list_transactions(BookingFilter::default())
            .unwrap()
            .transactions;
        assert!(all.len() > 40);
        let mut loaded: Vec<Booking> = Vec::new();
        let mut pages = 0;
        while loaded.len() < all.len() {
            let page = v
                .list_transactions(BookingFilter {
                    limit: 7,
                    offset: loaded.len() as u32,
                    whole_days: true,
                    ..Default::default()
                })
                .unwrap();
            assert_eq!(page.total_count as usize, all.len());
            assert!(
                page.transactions.len() >= 7 || loaded.len() + page.transactions.len() == all.len()
            );
            loaded.extend(page.transactions);
            // the next booking, if any, belongs to another day
            if let Some(next) = all.get(loaded.len()) {
                assert_ne!(next.booking_date, loaded.last().unwrap().booking_date);
            }
            pages += 1;
            assert!(pages < 1000);
        }
        let ids = |l: &[Booking]| l.iter().map(|b| b.id).collect::<Vec<_>>();
        assert_eq!(ids(&loaded), ids(&all));
        // without the flag the limit is exact
        let exact = v
            .list_transactions(BookingFilter {
                limit: 7,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(exact.transactions.len(), 7);
        // an offset behind the end is an empty page, not an error
        let past = v
            .list_transactions(BookingFilter {
                limit: 7,
                offset: all.len() as u32 + 5,
                whole_days: true,
                ..Default::default()
            })
            .unwrap();
        assert!(past.transactions.is_empty());
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn amount_range_filters_by_absolute_amount() {
        let (v, p) = demo();
        let all = v
            .list_transactions(BookingFilter::default())
            .unwrap()
            .transactions;
        let (min, max) = (5_000, 20_000);
        let range = |lo, hi| BookingFilter {
            amount_min_cents: lo,
            amount_max_cents: hi,
            ..Default::default()
        };
        let expected = |keep: &dyn Fn(i64) -> bool| {
            all.iter().filter(|b| keep(b.amount_cents.abs())).count() as u32
        };
        let both = v.list_transactions(range(Some(min), Some(max))).unwrap();
        assert!(both.total_count > 0, "the demo vault has bookings in range");
        assert_eq!(both.total_count, expected(&|a| (min..=max).contains(&a)));
        assert!(
            both.transactions
                .iter()
                .all(|b| (min..=max).contains(&b.amount_cents.abs()))
        );
        // the sign is ignored: income and expenses both count
        assert!(both.transactions.iter().any(|b| b.amount_cents > 0));
        assert!(both.transactions.iter().any(|b| b.amount_cents < 0));
        // one-sided ranges
        assert_eq!(
            v.count_transactions(range(Some(min), None)).unwrap(),
            expected(&|a| a >= min)
        );
        assert_eq!(
            v.count_transactions(range(None, Some(max))).unwrap(),
            expected(&|a| a <= max)
        );
        // together with the direction filter
        let expenses = v
            .count_transactions(BookingFilter {
                direction: BookingDirection::Expense,
                ..range(Some(min), Some(max))
            })
            .unwrap();
        let expected_expenses = all
            .iter()
            .filter(|b| b.amount_cents < 0 && (min..=max).contains(&b.amount_cents.abs()))
            .count() as u32;
        assert_eq!(expenses, expected_expenses);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn count_matches_the_list_total() {
        let (v, p) = demo();
        let all = v
            .list_transactions(BookingFilter::default())
            .unwrap()
            .transactions;
        let some = &all[all.len() / 2];
        let filters = [
            BookingFilter::default(),
            BookingFilter {
                checked: Some(false),
                ..Default::default()
            },
            BookingFilter {
                date_from: Some(some.booking_date.clone()),
                direction: BookingDirection::Expense,
                limit: 3,
                ..Default::default()
            },
            BookingFilter {
                account_ids: vec![some.account_id],
                search: Some(some.counterparty.clone()),
                ..Default::default()
            },
            BookingFilter {
                category_id: all.iter().find_map(|b| b.category_id),
                include_subcategories: true,
                ..Default::default()
            },
            BookingFilter {
                uncategorized_only: true,
                ..Default::default()
            },
            BookingFilter {
                scope: BookingScope::Investments,
                ..Default::default()
            },
            BookingFilter {
                merchant_key: Some(some.merchant.key.clone()),
                ..Default::default()
            },
        ];
        for f in filters {
            let listed = v.list_transactions(f.clone()).unwrap().total_count;
            assert_eq!(v.count_transactions(f.clone()).unwrap(), listed, "{f:?}");
        }
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn mark_all_checked_by_filter_and_undo() {
        let (v, p) = demo();
        let unchecked = BookingFilter {
            checked: Some(false),
            ..Default::default()
        };
        let before = v.list_transactions(unchecked.clone()).unwrap().total_count;
        assert!(before > 1);
        let ids = v.set_checked_for_filter(unchecked.clone(), true).unwrap();
        assert_eq!(ids.len() as u32, before);
        assert_eq!(
            v.list_transactions(unchecked.clone()).unwrap().total_count,
            0
        );
        // a second run changes nothing
        assert!(
            v.set_checked_for_filter(unchecked.clone(), true)
                .unwrap()
                .is_empty()
        );
        // undo restores exactly the touched bookings
        v.set_checked_for_ids(ids, false).unwrap();
        assert_eq!(v.list_transactions(unchecked).unwrap().total_count, before);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn mark_all_checked_respects_search_category_and_period() {
        let (v, p) = demo();
        v.set_checked_for_filter(BookingFilter::default(), false)
            .unwrap();
        let all = v.list_transactions(BookingFilter::default()).unwrap();
        let cat = all.transactions.iter().find_map(|b| b.category_id).unwrap();
        let mid = all.transactions[all.transactions.len() / 2]
            .booking_date
            .clone();
        let filters = [
            BookingFilter {
                search: Some("rewe".into()),
                ..Default::default()
            },
            BookingFilter {
                category_id: Some(cat),
                include_subcategories: true,
                ..Default::default()
            },
            BookingFilter {
                date_from: Some(mid.clone()),
                direction: BookingDirection::Expense,
                ..Default::default()
            },
        ];
        for f in filters {
            v.set_checked_for_filter(BookingFilter::default(), false)
                .unwrap();
            let shown = v
                .list_transactions(BookingFilter {
                    checked: Some(false),
                    ..f.clone()
                })
                .unwrap()
                .total_count;
            assert!(
                shown > 0 && shown < all.total_count,
                "filter must narrow: {shown}"
            );
            let ids = v
                .set_checked_for_filter(
                    BookingFilter {
                        checked: Some(false),
                        ..f
                    },
                    true,
                )
                .unwrap();
            assert_eq!(ids.len() as u32, shown);
            let left = v
                .list_transactions(BookingFilter {
                    checked: Some(false),
                    ..Default::default()
                })
                .unwrap()
                .total_count;
            assert_eq!(left, all.total_count - shown);
        }
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn mark_all_checked_respects_scope() {
        let (v, p) = demo();
        // start with everything new
        v.set_checked_for_filter(BookingFilter::default(), false)
            .unwrap();
        let count = |scope: BookingScope| {
            v.list_transactions(BookingFilter {
                checked: Some(false),
                scope,
                ..Default::default()
            })
            .unwrap()
            .total_count
        };
        let (all, everyday) = (count(BookingScope::All), count(BookingScope::Everyday));
        assert!(everyday < all, "demo has investments");
        let ids = v
            .set_checked_for_filter(
                BookingFilter {
                    checked: Some(false),
                    scope: BookingScope::Everyday,
                    ..Default::default()
                },
                true,
            )
            .unwrap();
        assert_eq!(ids.len() as u32, everyday);
        // what is outside the scope stays new
        assert_eq!(count(BookingScope::All), all - everyday);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn uncategorized_leaves_out_detected_own_transfers() {
        let (v, p) = demo();
        let all = v.list_transactions(BookingFilter::default()).unwrap();
        let open: Vec<_> = all
            .transactions
            .iter()
            .filter(|b| b.category_id.is_none() && b.flow != FlowKind::Transfer)
            .collect();
        let unc = BookingFilter {
            uncategorized_only: true,
            ..Default::default()
        };
        let page = v.list_transactions(unc.clone()).unwrap();
        assert!(
            page.transactions
                .iter()
                .all(|b| b.flow != FlowKind::Transfer)
        );
        assert_eq!(page.total_count as usize, open.len());
        assert_eq!(
            v.count_transactions(unc.clone()).unwrap() as usize,
            open.len()
        );
        // "mark all as checked" touches exactly the listed bookings, never the hidden transfers
        let made_transfer = open
            .iter()
            .find(|b| !b.checked && b.flow == FlowKind::Consumption)
            .unwrap()
            .id;
        v.db()
            .unwrap()
            .execute(
                "UPDATE transactions SET counterparty_iban = 'DE89370400440532013000' WHERE id = ?1",
                [made_transfer],
            )
            .unwrap();
        assert_eq!(
            v.count_transactions(unc.clone()).unwrap() as usize,
            open.len() - 1
        );
        let mut changed = v.set_checked_for_filter(unc, true).unwrap();
        changed.sort_unstable();
        let mut listed: Vec<i64> = open
            .iter()
            .filter(|b| !b.checked && b.id != made_transfer)
            .map(|b| b.id)
            .collect();
        listed.sort_unstable();
        assert_eq!(changed, listed);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn search_finds_spaced_iban_and_own_account_number() {
        let (v, p) = demo();
        let accounts = v.list_accounts().unwrap();
        let giro = accounts
            .iter()
            .find(|a| a.iban == "DE89370400440532013000")
            .expect("demo giro");
        let spaced = "DE89 3704 0044 0532 0130 00";
        let by_iban = v
            .list_transactions(BookingFilter {
                search: Some(spaced.into()),
                ..Default::default()
            })
            .unwrap();
        assert!(
            by_iban.total_count > 0,
            "spaced IBAN should match the own account"
        );
        assert!(
            by_iban.transactions.iter().all(|b| b.account_id == giro.id),
            "hits must belong to the IBAN's account"
        );
        let by_number = v
            .list_transactions(BookingFilter {
                search: Some("0532 0130 00".into()),
                ..Default::default()
            })
            .unwrap();
        assert!(by_number.total_count > 0);
        assert!(
            by_number
                .transactions
                .iter()
                .all(|b| b.account_id == giro.id)
        );
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn checked_filter_selects_new_or_checked_bookings() {
        let (v, p) = demo();
        let all = v
            .list_transactions(BookingFilter::default())
            .unwrap()
            .transactions;
        let first = all.first().unwrap();
        assert_eq!(
            v.booking_date(first.id).unwrap().as_deref(),
            Some(first.booking_date.as_str())
        );
        assert_eq!(v.booking_date(-1).unwrap(), None);
        let pick = all.iter().find(|b| !b.checked).expect("a new booking").id;
        v.set_transaction_checked(pick, true).unwrap();
        let of = |checked: Option<bool>| {
            v.list_transactions(BookingFilter {
                checked,
                ..Default::default()
            })
            .unwrap()
        };
        let (new, done) = (of(Some(false)), of(Some(true)));
        assert!(new.transactions.iter().all(|b| !b.checked));
        assert!(done.transactions.iter().all(|b| b.checked));
        assert!(done.transactions.iter().any(|b| b.id == pick));
        assert_eq!(
            new.total_count + done.total_count,
            of(None).total_count,
            "the count follows the filter"
        );
        let _ = std::fs::remove_file(p);
    }
}

#[cfg(test)]
mod own_transfer_tests {
    use super::*;
    use crate::test_support::TempDir;
    use rusqlite::params;
    use std::sync::Arc;

    const GIRO_IBAN: &str = "DE89370400440532013000";
    const KIND_IBAN: &str = "DE02120300000000202051";

    /// Two own accounts (parent Giro, child account) and one booking per case on
    /// the child account, all coming from the parent's IBAN.
    fn vault_with(cases: &[(&str, i64, &str, Option<&str>)]) -> (TempDir, Arc<Vault>, Vec<i64>) {
        let dir = TempDir::new("own-transfer");
        let v = Vault::open(dir.join("v.db").to_string_lossy().into(), vec![7u8; 32]).unwrap();
        let db = v.db().unwrap();
        db.execute(
            "INSERT INTO bank_connections(connector, display_name, login_name) VALUES ('fints:12345678', 'Bank', 'me')",
            [],
        )
        .unwrap();
        let con = db.last_insert_rowid();
        db.execute("INSERT INTO account_groups(name) VALUES ('G')", [])
            .unwrap();
        let group = db.last_insert_rowid();
        let mut accounts = vec![];
        for (ext, name, iban) in [("A1", "Eltern", GIRO_IBAN), ("A2", "Kind", KIND_IBAN)] {
            db.execute(
                "INSERT INTO accounts(connection_id, group_id, external_ref, name, kind, iban) VALUES (?1, ?5, ?2, ?3, 'checking', ?4)",
                params![con, ext, name, iban, group],
            )
            .unwrap();
            accounts.push(db.last_insert_rowid());
        }
        let mut ids = vec![];
        for (i, (cp, cents, purpose, cat)) in cases.iter().enumerate() {
            let cat_id: Option<i64> = cat.map(|path| {
                let mut parent: Option<i64> = None;
                for seg in path.split('/') {
                    let found: Option<i64> = db
                        .query_row(
                            "SELECT id FROM categories WHERE name = ?1 AND parent_id IS ?2",
                            params![seg, parent],
                            |r| r.get(0),
                        )
                        .ok();
                    parent = Some(found.unwrap_or_else(|| {
                        db.execute(
                            "INSERT INTO categories(name, parent_id) VALUES (?1, ?2)",
                            params![seg, parent],
                        )
                        .unwrap();
                        db.last_insert_rowid()
                    }));
                }
                parent.unwrap()
            });
            db.execute(
                "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, counterparty, counterparty_iban, purpose, category_id, category_source)
                 VALUES (?1, ?2, '2024-03-05', ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    accounts[1],
                    format!("t{i}"),
                    cents,
                    cp,
                    GIRO_IBAN,
                    purpose,
                    cat_id,
                    if cat_id.is_some() { "manual" } else { "none" }
                ],
            )
            .unwrap();
            ids.push(db.last_insert_rowid());
        }
        drop(db);
        (dir, v, ids)
    }

    fn flow_of(v: &Vault, id: i64) -> FlowKind {
        v.list_transactions(BookingFilter::default())
            .unwrap()
            .transactions
            .into_iter()
            .find(|b| b.id == id)
            .unwrap()
            .flow
    }

    #[test]
    fn uncategorised_booking_between_own_accounts_is_a_transfer() {
        let (_d, v, ids) = vault_with(&[("Papa", 500_00, "Taschengeld", None)]);
        assert_eq!(flow_of(&v, ids[0]), FlowKind::Transfer);
        let page = v.list_transactions(BookingFilter::default()).unwrap();
        assert_eq!(page.income_cents, 0);
        assert_eq!(page.flow.income_cents, 0);
    }

    #[test]
    fn explicit_non_transfer_category_keeps_its_flow() {
        let (_d, v, ids) =
            vault_with(&[("Papa", 500_00, "Taschengeld", Some("Sonstige Einnahmen"))]);
        assert_eq!(flow_of(&v, ids[0]), FlowKind::Income);
        let f = v.selection_flow(vec![], None, None).unwrap();
        assert_eq!(f.income_cents, 500_00);
        let page = v.list_transactions(BookingFilter::default()).unwrap();
        assert_eq!(page.income_cents, 500_00);
    }

    /// Issue #94 follow-up (NiTRoeSE, 2026-11-15): MoneyMoney types internal
    /// transfers "Umbuchung" (`unformatted_type` → booking text, comdirect
    /// "Übertrag / Überweisung"). An explicit non-transfer category must keep
    /// its flow despite that wording — otherwise the categorised credits on a
    /// child's account silently drop out of the Einnahmen KPI.
    #[test]
    fn explicit_category_beats_umbuchung_booking_text() {
        let (_d, v, ids) = vault_with(&[
            ("Mama", 500_00, "Taschengeld", Some("Einnahmen/Sonstiges")),
            ("Papa", -40_00, "Miete", Some("Wohnen")),
        ]);
        v.db()
            .unwrap()
            .execute(
                "UPDATE transactions SET booking_text = CASE WHEN amount_cents > 0 \
                 THEN 'Umbuchung' ELSE 'Übertrag / Überweisung' END",
                [],
            )
            .unwrap();
        assert_eq!(flow_of(&v, ids[0]), FlowKind::Income);
        assert_eq!(flow_of(&v, ids[1]), FlowKind::Consumption);
        let page = v.list_transactions(BookingFilter::default()).unwrap();
        assert_eq!(page.income_cents, 500_00);
        assert_eq!(page.expense_cents, -40_00);
    }

    /// Without an explicit category the transfer wording of the booking text
    /// still decides (comdirect books incoming transfers "Übertrag /
    /// Überweisung") — the rescue above must not weaken that heuristic.
    #[test]
    fn uncategorised_transfer_wording_stays_a_transfer() {
        let (_d, v, ids) = vault_with(&[("Mama", 500_00, "Taschengeld", None)]);
        v.db()
            .unwrap()
            .execute(
                "UPDATE transactions SET booking_text = 'Übertrag / Überweisung', \
                 counterparty_iban = 'DE75512108001245126199'",
                [],
            )
            .unwrap();
        assert_eq!(flow_of(&v, ids[0]), FlowKind::Transfer);
        let page = v.list_transactions(BookingFilter::default()).unwrap();
        assert_eq!(page.income_cents, 0);
    }

    #[test]
    fn expense_with_category_to_own_account_counts_as_consumption() {
        let (_d, v, ids) = vault_with(&[("Papa", -40_00, "Kino", Some("Freizeit"))]);
        assert_eq!(flow_of(&v, ids[0]), FlowKind::Consumption);
        assert_eq!(
            v.selection_flow(vec![], None, None)
                .unwrap()
                .consumption_cents,
            40_00
        );
    }

    #[test]
    fn investment_to_own_account_keeps_investment_flow() {
        let (_d, v, ids) = vault_with(&[
            ("Depot", -100_00, "Sparplan MSCI World", None),
            (
                "Depot",
                -100_00,
                "Sparplan MSCI World",
                Some("Sparen & Investieren"),
            ),
        ]);
        assert_eq!(flow_of(&v, ids[0]), FlowKind::Investment);
        assert_eq!(flow_of(&v, ids[1]), FlowKind::Investment);
        assert_eq!(
            v.selection_flow(vec![], None, None).unwrap().invested_cents,
            200_00
        );
    }

    #[test]
    fn explicit_imported_transfer_role_is_a_transfer() {
        let (_d, v, ids) = vault_with(&[
            ("Papa", 500_00, "x", Some("Umbuchung")),
            ("Papa", 500_00, "x", Some("Privat/Geldtransit")),
            // no own IBAN involved: the category alone decides
        ]);
        v.db()
            .unwrap()
            .execute(
                "UPDATE categories SET builtin_role='import:transfers'
            WHERE name IN ('Umbuchung','Geldtransit')",
                [],
            )
            .unwrap();
        assert_eq!(flow_of(&v, ids[0]), FlowKind::Transfer);
        assert_eq!(flow_of(&v, ids[1]), FlowKind::Transfer);
        assert_eq!(
            v.selection_flow(vec![], None, None).unwrap().income_cents,
            0
        );
    }

    #[test]
    fn custom_transfer_name_has_no_builtin_role() {
        let (_d, v, ids) = vault_with(&[("Papa", 500_00, "x", Some("Umbuchungen"))]);
        assert_eq!(flow_of(&v, ids[0]), FlowKind::Income);
    }

    /// comdirect books incoming transfers as "Übertrag / Überweisung": a
    /// salary must not become a transfer by that booking text.
    #[test]
    fn salary_with_transfer_booking_text_is_income() {
        let (_d, v, ids) = vault_with(&[
            (
                "UHB Software",
                4254_94,
                "LOHN / GEHALT 10/26 EREF:",
                Some("Salary"),
            ),
            ("UHB Software", 4254_94, "LOHN / GEHALT 09/26 EREF:", None),
        ]);
        v.db()
            .unwrap()
            .execute_batch(
                "UPDATE transactions SET booking_text = 'Übertrag / Überweisung',
                     counterparty_iban = 'DE75512108001245126199'",
            )
            .unwrap();
        assert_eq!(flow_of(&v, ids[0]), FlowKind::Income);
        assert_eq!(flow_of(&v, ids[1]), FlowKind::Income);
        let page = v.list_transactions(BookingFilter::default()).unwrap();
        assert_eq!(page.income_cents, 2 * 4254_94);
    }
}

/// The light path of `list_page` (class cache + id/amount query) must answer exactly like
/// the full one that enriches every row, and cached work must never outlive a write.
#[cfg(test)]
mod list_cache_tests {
    use super::*;
    use crate::test_support::TempDir;

    fn demo(name: &str) -> (Arc<Vault>, TempDir) {
        let dir = TempDir::new(name);
        let v = Vault::open(dir.join("v.db").to_string_lossy().into(), vec![7u8; 32]).unwrap();
        v.seed_demo_data().unwrap();
        (v, dir)
    }

    fn ids(bookings: &[Booking]) -> Vec<i64> {
        bookings.iter().map(|b| b.id).collect()
    }

    /// `collect_bookings` of `filter` must keep exactly what finishing every
    /// booking of the filter's SQL part and applying `keeps` keeps.
    fn visits_like_a_full_pass(v: &Vault, filter: &BookingFilter) {
        let unclassed = BookingFilter {
            scope: BookingScope::All,
            merchant_key: None,
            ..filter.clone()
        };
        let db = v.db().unwrap();
        let reference: Vec<Booking> = v
            .collect_bookings(&db, &unclassed)
            .unwrap()
            .into_iter()
            .filter(|b| keeps(filter, b.flow, &b.merchant.key))
            .collect();
        let visited = v.collect_bookings(&db, filter).unwrap();
        assert!(!visited.is_empty(), "{filter:?}");
        assert_eq!(ids(&visited), ids(&reference), "{filter:?}");
    }

    fn same(v: &Vault, filter: BookingFilter) {
        let light = v.list_transactions(filter.clone()).unwrap();
        let full = {
            let db = v.db().unwrap();
            v.list_page_full(&db, &filter).unwrap()
        };
        assert_eq!(
            v.count_transactions(filter.clone()).unwrap(),
            full.total_count,
            "{filter:?}"
        );
        assert_eq!(
            ids(&light.transactions),
            ids(&full.transactions),
            "{filter:?}"
        );
        assert_eq!(light.total_count, full.total_count, "{filter:?}");
        assert_eq!(light.income_cents, full.income_cents, "{filter:?}");
        assert_eq!(light.expense_cents, full.expense_cents, "{filter:?}");
        assert_eq!(
            light.consumption_cents, full.consumption_cents,
            "{filter:?}"
        );
        assert_eq!(light.investment_cents, full.investment_cents, "{filter:?}");
        assert_eq!(light.perk_cents, full.perk_cents, "{filter:?}");
        let flows = |p: &BookingPage| p.transactions.iter().map(|b| b.flow).collect::<Vec<_>>();
        assert_eq!(flows(&light), flows(&full), "{filter:?}");
        let cats = |p: &BookingPage| {
            p.transactions
                .iter()
                .map(|b| b.category_path.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(cats(&light), cats(&full), "{filter:?}");
    }

    #[test]
    fn light_path_matches_the_full_list() {
        let (v, _dir) = demo("list-cache");
        let first_cat = v.list_categories(None, None).unwrap()[3].id;
        let accounts: Vec<i64> = v
            .list_account_groups()
            .unwrap()
            .iter()
            .flat_map(|g| g.accounts.iter().map(|a| a.id))
            .collect();
        for scope in [
            BookingScope::All,
            BookingScope::Everyday,
            BookingScope::Investments,
        ] {
            for (limit, offset, whole_days) in [
                (0, 0, false),
                (10, 0, false),
                (10, 7, true),
                (3, 100, false),
            ] {
                same(
                    &v,
                    BookingFilter {
                        scope,
                        limit,
                        offset,
                        whole_days,
                        ..Default::default()
                    },
                );
            }
        }
        same(
            &v,
            BookingFilter {
                uncategorized_only: true,
                limit: 20,
                ..Default::default()
            },
        );
        same(
            &v,
            BookingFilter {
                category_id: Some(first_cat),
                include_subcategories: true,
                ..Default::default()
            },
        );
        same(
            &v,
            BookingFilter {
                account_ids: vec![accounts[0]],
                direction: BookingDirection::Expense,
                ..Default::default()
            },
        );
        same(
            &v,
            BookingFilter {
                checked: Some(false),
                search: Some("rewe".into()),
                ..Default::default()
            },
        );
        same(
            &v,
            BookingFilter {
                date_from: Some("2000-01-01".into()),
                date_to: Some("2100-01-01".into()),
                limit: 5,
                ..Default::default()
            },
        );
    }

    /// The merchant filter answers from the class cache, and `visit_bookings` skips
    /// the rows a class drops before finishing them.
    #[test]
    fn class_filters_match_finishing_every_booking() {
        let (v, _dir) = demo("class-filters");
        let mut merchants: Vec<String> = v
            .list_transactions(BookingFilter::default())
            .unwrap()
            .transactions
            .into_iter()
            .map(|b| b.merchant.key)
            .collect();
        merchants.sort();
        merchants.dedup();
        assert!(merchants.len() > 3);
        for key in merchants {
            let filter = BookingFilter {
                merchant_key: Some(key),
                ..Default::default()
            };
            same(&v, filter.clone());
            same(
                &v,
                BookingFilter {
                    scope: BookingScope::Everyday,
                    limit: 1,
                    offset: 1,
                    ..filter.clone()
                },
            );
            visits_like_a_full_pass(&v, &filter);
        }
        for scope in [BookingScope::Everyday, BookingScope::Investments] {
            visits_like_a_full_pass(
                &v,
                &BookingFilter {
                    scope,
                    ..Default::default()
                },
            );
        }
    }

    /// Small results reuse the cached `BookingFinisher`: a renamed category must
    /// show on the next read.
    #[test]
    fn a_write_invalidates_the_cached_finisher() {
        let (v, _dir) = demo("finisher-cache");
        let booking = v
            .list_transactions(BookingFilter::default())
            .unwrap()
            .transactions
            .into_iter()
            .find(|b| b.category_id.is_some())
            .unwrap();
        let read = || {
            let db = v.db().unwrap();
            v.bookings_by_ids(&db, &[booking.id]).unwrap().remove(0)
        };
        assert_eq!(read().category_name, booking.category_name);
        let cat = v
            .list_categories(None, None)
            .unwrap()
            .into_iter()
            .find(|c| Some(c.id) == booking.category_id)
            .unwrap();
        v.update_category(
            cat.id,
            "Umbenannt".into(),
            cat.parent_id,
            cat.sf_symbol,
            cat.color_hex,
        )
        .unwrap();
        assert_eq!(read().category_name, "Umbenannt");
    }

    #[test]
    fn a_write_invalidates_the_classes() {
        let (v, _dir) = demo("classes-write");
        let before = v.list_transactions(BookingFilter::default()).unwrap();
        let cat = v.list_categories(None, None).unwrap();
        let target = before
            .transactions
            .iter()
            .find(|b| b.category_id.is_none())
            .unwrap()
            .id;
        v.set_transaction_category(target, Some(cat[3].id)).unwrap();
        same(&v, BookingFilter::default());
        let after = v.list_transactions(BookingFilter::default()).unwrap();
        let b = after.transactions.iter().find(|b| b.id == target).unwrap();
        assert_eq!(b.category_id, Some(cat[3].id));
    }

    /// #357: the category sums of the sidebar follow the same amount range as the list.
    #[test]
    fn category_sums_follow_the_amount_range() {
        let dir = TempDir::new("amount-filter-categories");
        let v = Vault::open(dir.join("v.db").to_string_lossy().into(), vec![7u8; 32]).unwrap();
        v.seed_demo_data().unwrap();
        let sum = |cats: &[Category]| {
            cats.iter().filter(|c| c.depth == 0).fold((0, 0), |a, c| {
                (a.0 + c.total_cents, a.1 + c.transaction_count)
            })
        };
        let all = v.list_categories(None, None).unwrap();
        let ranged = |min, max| {
            sum(&v
                .list_categories_filtered_amount(
                    None,
                    None,
                    vec![],
                    None,
                    BookingDirection::All,
                    min,
                    max,
                )
                .unwrap())
        };
        // no range = the plain listing
        assert_eq!(ranged(None, None), sum(&all));
        // below and above a threshold together are everything
        let (lo, hi) = (ranged(None, Some(10_000)), ranged(Some(10_001), None));
        assert!(lo.1 > 0 && hi.1 > 0);
        assert_eq!((lo.0 + hi.0, lo.1 + hi.1), sum(&all));
        // an impossible range leaves nothing
        assert_eq!(ranged(Some(i64::MAX / 2), None), (0, 0));
    }
}
