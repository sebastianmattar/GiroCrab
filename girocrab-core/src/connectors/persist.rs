//! Vault side of syncs and transfers: storing fetch results, starting
//! sessions, statement automation and pending transfers.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use rusqlite::{OptionalExtension, params};

use crate::error::{CoreError, CoreResult};
use crate::i18n::{tr, trf};
use crate::iban::{normalize_iban, validate_iban};
use crate::importers::portal_import::{CARD_BOOKING_TEXT, CARD_SEPARATOR};
use crate::models::{AccountKind, BatchTransferRequest, ConnectorKind, SepaTransferRequest};
use crate::secret::{SecretJson, SecretString};
use crate::vault::Vault;

use super::*;

pub(super) fn is_content_id(id: &str, prefix: char) -> bool {
    let mut c = id.chars();
    id.len() == 17 && c.next() == Some(prefix) && c.all(|c| c.is_ascii_hexdigit())
}

/// One-time migration of ids from older versions: a booking stored under a
/// legacy content id (`h…`, `DefaultHasher` / list position) with exactly the
/// same content gets the new id instead of being imported a second time.
/// The caller checks that no row holds the new id yet: the UPDATE would hit
/// the unique key. Returns true when a row was adopted.
fn adopt_legacy_content_id(
    tx: &rusqlite::Connection,
    account_id: i64,
    t: &RemoteTransaction,
) -> CoreResult<bool> {
    if !is_content_id(&t.external_id, CONTENT_ID_PREFIX) {
        return Ok(false);
    }
    let pattern = format!("h{}", "[0-9a-f]".repeat(16));
    let n = tx
        .prepare_cached(
            "UPDATE transactions SET external_id = ?1
              WHERE id = (SELECT id FROM transactions
                           WHERE account_id = ?2 AND external_id GLOB ?3
                             AND booking_date = ?4 AND value_date = ?5 AND amount_cents = ?6
                             AND counterparty = ?7 AND purpose = ?8
                           ORDER BY id LIMIT 1)",
        )?
        .execute(params![
            t.external_id,
            account_id,
            pattern,
            t.booking_date,
            t.value_date,
            t.amount_cents,
            t.counterparty,
            t.purpose
        ])?;
    Ok(n > 0)
}

/// The row stored under a fetched booking's `(account_id, external_id)`,
/// soft-deleted ones included: the one read that decides whether the booking
/// is new, deleted or known. Holds the columns [`refresh_existing`] compares.
struct Stored {
    id: i64,
    deleted: bool,
    counterparty: String,
    purpose: String,
    purpose_code: String,
    gvc: String,
    end_to_end_id: String,
    mandate_id: String,
    creditor_id: String,
    item_refs: String,
}

impl Stored {
    fn read(
        tx: &rusqlite::Connection,
        account_id: i64,
        external_id: &str,
    ) -> CoreResult<Option<Self>> {
        Ok(tx
            .prepare_cached(
                "SELECT id, deleted_ms IS NOT NULL, counterparty, purpose, purpose_code, gvc,
                        end_to_end_id, mandate_id, creditor_id, item_refs
                   FROM transactions WHERE account_id = ?1 AND external_id = ?2",
            )?
            .query_row(params![account_id, external_id], |r| {
                Ok(Self {
                    id: r.get(0)?,
                    deleted: r.get(1)?,
                    counterparty: r.get(2)?,
                    purpose: r.get(3)?,
                    purpose_code: r.get(4)?,
                    gvc: r.get(5)?,
                    end_to_end_id: r.get(6)?,
                    mandate_id: r.get(7)?,
                    creditor_id: r.get(8)?,
                    item_refs: r.get(9)?,
                })
            })
            .optional()?)
    }
}

/// A booking the bank delivers again: fills bank fields an older version did
/// not store yet (codes, SEPA references, product ids, an empty counterparty) and heals corrupted text
/// (U+FFFD / mojibake) with the freshly decoded one. Writes only when a column
/// changes, since most bookings of every fetch are already stored.
fn refresh_existing(
    tx: &rusqlite::Connection,
    s: &Stored,
    t: &RemoteTransaction,
) -> CoreResult<()> {
    // A group is filled only while all of it is empty, so a row never mixes
    // codes or references of two deliveries.
    fn fill<'a, const N: usize>(stored: [&str; N], fresh: [&'a String; N]) -> [Option<&'a str>; N] {
        let take = stored.iter().all(|s| s.is_empty()) && fresh.iter().any(|f| !f.is_empty());
        fresh.map(|f| take.then_some(f.as_str()))
    }
    fn heal<'a>(stored: &str, fresh: &'a str) -> Option<&'a str> {
        let clean = crate::text::is_clean;
        (!clean(stored) && !fresh.is_empty() && clean(fresh)).then_some(fresh)
    }
    let [code, gvc] = fill([&s.purpose_code, &s.gvc], [&t.purpose_code, &t.gvc]);
    let [e2e, mandate, creditor] = fill(
        [&s.end_to_end_id, &s.mandate_id, &s.creditor_id],
        [&t.end_to_end_id, &t.mandate_id, &t.creditor_id],
    );
    let [items] = fill([&s.item_refs], [&t.item_refs]);
    // a counterparty the bank (or a newer version) names now fills an empty one,
    // e.g. the merchant of a comdirect card payment (#1135)
    let [named] = fill([&s.counterparty], [&t.counterparty]);
    let set = [
        heal(&s.counterparty, &t.counterparty).or(named),
        heal(&s.purpose, &t.purpose),
        code,
        gvc,
        e2e,
        mandate,
        creditor,
        items,
    ];
    if set == [None; 8] {
        return Ok(());
    }
    let [
        counterparty,
        purpose,
        code,
        gvc,
        e2e,
        mandate,
        creditor,
        items,
    ] = set;
    tx.prepare_cached(
        "UPDATE transactions SET counterparty = COALESCE(?2, counterparty), purpose = COALESCE(?3, purpose),
                purpose_code = COALESCE(?4, purpose_code), gvc = COALESCE(?5, gvc),
                end_to_end_id = COALESCE(?6, end_to_end_id), mandate_id = COALESCE(?7, mandate_id),
                creditor_id = COALESCE(?8, creditor_id), item_refs = COALESCE(?9, item_refs)
          WHERE id = ?1",
    )?
    .execute(params![
        s.id,
        counterparty,
        purpose,
        code,
        gvc,
        e2e,
        mandate,
        creditor,
        items
    ])?;
    Ok(())
}

/// Persists accounts, bookings and documents of one fetch (shared by bank
/// syncs and the keyless web-portal / statement import).
/// Bank dates as stored: real `YYYY-MM-DD` days, since period filters, sums and file names
/// read them as such. A booking date that is no day takes the value date, then today (as
/// connectors already do for an empty one); a value date that is no day is left empty.
fn store_dates(t: &mut RemoteTransaction) {
    let value = crate::dates::parse_iso(&t.value_date);
    let booking = crate::dates::parse_iso(&t.booking_date)
        .or(value)
        .unwrap_or_else(crate::dates::today);
    t.booking_date = crate::dates::iso(booking);
    t.value_date = value.map(crate::dates::iso).unwrap_or_default();
}

pub(crate) fn store_fetch(
    vault: &Vault,
    kind: ConnectorKind,
    connection_id: i64,
    mut data: FetchResult,
) -> CoreResult<SyncSummary> {
    {
        let db = vault.db()?;
        // Document retrieval switched off: whatever a connector still delivers is not stored.
        if !documents_fetch_on(&db, connection_id)? {
            data.documents.clear();
        }
        let tx = db.unchecked_transaction()?;
        if kind == ConnectorKind::Advanzia {
            super::advanzia::prepare_import(&tx, connection_id, &mut data)?;
        }
        let generic_bank_name = descriptor(kind).name;
        // No name from the bank: the connection's display name (the bank picked
        // when adding it, or renamed later) beats the generic "FinTS-Bank".
        let display_name: Option<String> = tx
            .query_row(
                "SELECT display_name FROM bank_connections WHERE id = ?1",
                [connection_id],
                |r| r.get(0),
            )
            .optional()?;
        let bank_name = data
            .bank_name
            .clone()
            .into_iter()
            .chain(display_name)
            .map(|b| b.trim().to_string())
            .find(|b| !b.is_empty())
            .unwrap_or_else(|| generic_bank_name.clone());
        let (blocked_refs, pending_refs) =
            super::account_selection::prepare(&tx, kind, connection_id, &data)?;
        let deferred = (!pending_refs.is_empty())
            .then(|| super::account_selection::deferred(&data, &pending_refs));
        let mut data = data;
        // safety net for every connector: repair UTF-8/Latin-1 mojibake
        for a in &mut data.accounts {
            a.name = crate::text::clean_bank_text(&a.name);
            a.owner = crate::text::clean_bank_text(&a.owner);
        }
        for t in &mut data.transactions {
            t.counterparty = crate::text::clean_bank_text(&t.counterparty);
            t.purpose = crate::text::clean_bank_text(&t.purpose);
            t.booking_text = crate::text::clean_bank_text(&t.booking_text);
        }
        // A connector without a bank id must not collapse bookings into one
        // `(account, "")` row: give them a deterministic content id instead.
        let mut missing_ids: HashMap<String, ContentIds> = HashMap::new();
        for t in &mut data.transactions {
            if t.external_id.trim().is_empty() {
                t.external_id = missing_ids
                    .entry(t.account_ref.clone())
                    .or_default()
                    .next(t);
            }
            // After the content ids, so they stay what earlier syncs computed.
            store_dates(t);
        }
        for (old, new) in &data.renamed_refs {
            if old.is_empty() || new.is_empty() || old == new {
                continue;
            }
            // includes soft-deleted rows (the new ref must stay unique)
            tx.execute(
                "UPDATE accounts SET external_ref = ?3
                 WHERE connection_id = ?1 AND external_ref = ?2
                   AND NOT EXISTS (SELECT 1 FROM accounts WHERE connection_id = ?1 AND external_ref = ?3)",
                params![connection_id, old, new],
            )?;
        }
        for (r, old_name, new_name) in &data.name_updates {
            if new_name.trim().is_empty() || old_name == new_name {
                continue;
            }
            tx.execute(
                "UPDATE accounts SET name = ?4 WHERE connection_id = ?1 AND external_ref = ?2 AND name = ?3",
                params![connection_id, r, old_name, new_name],
            )?;
        }
        let mut account_ids: HashMap<String, i64> = HashMap::new();
        // Archived accounts the bank still delivers: nothing is stored for
        // them (no balance, bookings, positions or documents).
        let mut archived_ids: HashMap<String, i64> = HashMap::new();
        // the deleted ones among them (their bookings are not "skipped")
        let mut deleted_refs: HashSet<String> = HashSet::new();
        let mut accounts_updated = 0u32;
        let had_accounts: i64 = tx.query_row(
            "SELECT count(*) FROM accounts WHERE connection_id = ?1 AND deleted_ms IS NULL",
            [connection_id],
            |r| r.get(0),
        )?;
        let mut new_accounts: Vec<String> = Vec::new();
        for acc in &data.accounts {
            if blocked_refs.contains(&acc.external_ref) {
                continue;
            }
            let existing: Option<i64> = tx
                .query_row(
                    // includes soft-deleted rows
                    "SELECT id FROM accounts WHERE connection_id = ?1 AND external_ref = ?2",
                    params![connection_id, acc.external_ref],
                    |r| r.get(0),
                )
                .optional()?;
            // First sync after a MoneyMoney database import: take over the
            // unconnected imported account with the same IBAN (its bookings are
            // then adopted via the import fingerprint) instead of a second account.
            let existing = match existing {
                Some(id) => Some(id),
                None => crate::importers::moneymoney_db::adopt_account(
                    &tx,
                    connection_id,
                    &acc.external_ref,
                    &acc.iban,
                    &acc.currency,
                    acc.kind,
                )?,
            };
            // Sub-accounts known from an earlier import/sync without a connection
            // (e.g. comdirect Depot / Tagesgeld): link them to this connection.
            let existing = match existing {
                Some(id) => Some(id),
                None => adopt_unlinked(&tx, connection_id, &bank_name, acc)?,
            };
            // A deleted duplicate of an imported account (created before the
            // import could be taken over) gives way to the imported account,
            // also when the connection was added again (which would revive it).
            let existing = match existing {
                Some(id)
                    if tx.query_row(
                        // includes soft-deleted rows
                        "SELECT deleted_ms IS NOT NULL FROM accounts WHERE id = ?1",
                        [id],
                        |r| r.get::<_, bool>(0),
                    )? =>
                {
                    Some(adopt_past_deleted_twin(&tx, connection_id, id, acc)?.unwrap_or(id))
                }
                other => other,
            };
            if let Some(id) = existing {
                // Deleted accounts stay deleted (like archived ones, nothing is
                // stored), unless the connection was added again since.
                if crate::sync_merge::fetch_keeps_account_deleted(&tx, connection_id, id)? {
                    deleted_refs.insert(acc.external_ref.clone());
                    archived_ids.insert(acc.external_ref.clone(), id);
                    continue;
                }
                crate::sync_merge::revive_account_if_deleted(&tx, id)?;
                let archived: bool = tx.query_row(
                    "SELECT archived_unix IS NOT NULL FROM accounts WHERE id = ?1",
                    [id],
                    |r| r.get(0),
                )?;
                if archived {
                    archived_ids.insert(acc.external_ref.clone(), id);
                    continue;
                }
            }
            let is_new = existing.is_none();
            let id = match existing {
                Some(id) => {
                    if !data.adjust_balances {
                        // A card reporting 0 (no /balances data) must not wipe a known balance,
                        // and neither must an account whose balance was not read at all.
                        let keep = data.balances_unknown.contains(&acc.external_ref)
                            || !data.balances_reported.contains(&acc.external_ref)
                                && acc.balance_cents == 0
                                && acc.kind == AccountKind::CreditCard
                                && tx.query_row(
                                    "SELECT balance_cents FROM accounts WHERE id = ?1",
                                    [id],
                                    |r| r.get::<_, i64>(0),
                                )? != 0;
                        if keep {
                            tx.execute(
                                "UPDATE accounts SET currency = ?1 WHERE id = ?2",
                                params![acc.currency, id],
                            )?;
                        } else {
                            tx.execute(
                                "UPDATE accounts SET balance_cents = ?1, balance_date = ?2, currency = ?3 WHERE id = ?4",
                                params![acc.balance_cents, acc.balance_date, acc.currency, id],
                            )?;
                        }
                    }
                    // Bank name / owner learned later (e.g. "C24" instead of the
                    // generic "Bank per PSD2"); never overwrite what is set.
                    if bank_name != generic_bank_name {
                        tx.execute(
                            "UPDATE accounts SET bank_name = ?1 WHERE id = ?2 AND bank_name IN (?3, '')",
                            params![bank_name, id, generic_bank_name],
                        )?;
                    }
                    if !acc.owner.is_empty() {
                        tx.execute(
                            "UPDATE accounts SET owner = ?1 WHERE id = ?2 AND IFNULL(owner, '') = ''",
                            params![acc.owner, id],
                        )?;
                    }
                    // Fill in an account number learned later (e.g. a BBAN of a
                    // PSD2 account without IBAN); never overwrite a known one.
                    if !acc.account_number.is_empty() {
                        tx.execute(
                            "UPDATE accounts SET account_number = ?1 WHERE id = ?2 AND IFNULL(account_number, '') = ''",
                            params![acc.account_number, id],
                        )?;
                    }
                    // Trade Republic cash side is a clearing account, not a Girokonto.
                    if kind == ConnectorKind::TradeRepublic
                        && acc.kind != AccountKind::Brokerage
                        && acc.name == "Verrechnungskonto"
                    {
                        tx.execute(
                            "UPDATE accounts SET name = ?1 WHERE id = ?2 AND name = 'Girokonto'",
                            params![acc.name, id],
                        )?;
                    }
                    // Brokerage: depots found later. Savings: PSD2 savings accounts
                    // first stored as checking (e.g. C24 PocketZINS, typed by product).
                    // Loan: a Schwäbisch Hall Bausparvertrag moving on to its loan phase
                    // (same contract id), which must not stay a savings account.
                    if acc.kind == AccountKind::Brokerage
                        || acc.kind == AccountKind::Savings
                        || (acc.kind == AccountKind::Loan && kind == ConnectorKind::SchwaebischHall)
                    {
                        let (group, sym, order) = group_for(acc.kind);
                        let gid = Vault::ensure_group(&tx, group, sym, order)?;
                        let changed = tx.execute(
                            // a symbol the user picked (symbol_ms > 0) stays
                            "UPDATE accounts SET kind = ?1, sf_symbol = CASE WHEN symbol_ms > 0 THEN sf_symbol ELSE ?2 END, group_id = ?3 WHERE id = ?4 AND kind <> ?1",
                            params![acc.kind.as_str(), acc.kind.sf_symbol(), gid, id],
                        )?;
                        if changed > 0 {
                            // a history imported while it was another kind counts now (#578)
                            crate::depot_values::rebuild(&tx, id, None)?;
                        }
                    }
                    id
                }
                None => {
                    let (group, sym, order) = group_for(acc.kind);
                    let gid = Vault::ensure_group(&tx, group, sym, order)?;
                    // Amazon's order account is a proof account: it starts
                    // outside the overall money totals (#337); the user can
                    // switch it back on. Stamp 0, so any explicit choice wins
                    // on sync. Only on creation — a re-fetch never resets a
                    // stored decision.
                    let include_in_total = kind != ConnectorKind::AmazonOrders;
                    tx.execute(
                        "INSERT INTO accounts(connection_id, group_id, external_ref, name, bank_name, owner, iban, account_number, currency, kind,
                                              balance_cents, balance_date, sf_symbol, sort_order, created_ms, include_in_total, total_ms)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, (SELECT IFNULL(MAX(sort_order),0)+1 FROM accounts), ?14, ?15, 0)",
                        params![connection_id, gid, acc.external_ref, acc.name, bank_name, acc.owner, acc.iban, acc.account_number,
                                acc.currency, acc.kind.as_str(), acc.balance_cents, acc.balance_date, acc.kind.sf_symbol(),
                                crate::dates::now_ms(), include_in_total],
                    )?;
                    let id = tx.last_insert_rowid();
                    crate::user_groups::place_new_account(&tx, id)?;
                    id
                }
            };
            accounts_updated += 1;
            account_ids.insert(acc.external_ref.clone(), id);
            if is_new && had_accounts > 0 {
                new_accounts.push(acc.name.clone());
            }
            // Delivered again: access is back (e.g. included in a new consent).
            tx.execute(
                "UPDATE accounts SET access_revoked = 0 WHERE id = ?1 AND access_revoked != 0",
                [id],
            )?;
        }
        let mut revoked_accounts: Vec<String> = Vec::new();
        let mut revoked_refs = data.revoked_refs.clone();
        // Archived accounts are the user's business: skipped above even when
        // the bank still delivers them, so they must not count as "missing".
        let delivered = |r: &str| {
            account_ids.contains_key(r) || archived_ids.contains_key(r) || blocked_refs.contains(r)
        };
        if data.accounts_complete && !data.accounts.is_empty() {
            let mut q = tx.prepare(
                "SELECT external_ref FROM accounts
                 WHERE connection_id = ?1 AND access_revoked = 0 AND archived_unix IS NULL AND deleted_ms IS NULL",
            )?;
            let stored: Vec<String> = q
                .query_map([connection_id], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            drop(q);
            revoked_refs.extend(stored.into_iter().filter(|r| !delivered(r)));
        }
        revoked_refs.sort();
        revoked_refs.dedup();
        for r in revoked_refs
            .iter()
            .filter(|r| !r.is_empty() && !delivered(r))
        {
            let name: Option<String> = tx
                .query_row(
                    "SELECT name FROM accounts WHERE connection_id = ?1 AND external_ref = ?2
                       AND access_revoked = 0 AND archived_unix IS NULL AND deleted_ms IS NULL",
                    params![connection_id, r],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(name) = name {
                tx.execute(
                    "UPDATE accounts SET access_revoked = 1 WHERE connection_id = ?1 AND external_ref = ?2",
                    params![connection_id, r],
                )?;
                revoked_accounts.push(name);
            }
        }
        // #212: a connector without balances accumulates the bookings; such an
        // account's balance can start from an opening balance.
        if !data.simulated {
            for acc in &data.accounts {
                if let Some(&id) = account_ids.get(&acc.external_ref) {
                    tx.execute(
                        "UPDATE accounts SET balance_derived = ?1 WHERE id = ?2",
                        params![data.adjust_balances, id],
                    )?;
                }
            }
        }
        // Credit cards without a connector balance: derive the open amount.
        if !data.adjust_balances {
            for acc in data.accounts.iter().filter(|a| {
                a.balance_cents == 0
                    && !data.balances_reported.contains(&a.external_ref)
                    && !data.balances_unknown.contains(&a.external_ref)
            }) {
                let Some(&id) = account_ids.get(&acc.external_ref) else {
                    continue;
                };
                let stored: i64 = tx.query_row(
                    "SELECT balance_cents FROM accounts WHERE id = ?1",
                    [id],
                    |r| r.get(0),
                )?;
                if stored != 0 {
                    continue;
                }
                let is_card = acc.kind == AccountKind::CreditCard
                    || is_amex_name(&acc.name)
                    || is_amex_name(bank_name.as_str());
                if !is_card {
                    continue;
                }
                if let Some(open) = card_open_balance(&tx, id)? {
                    tx.execute(
                        "UPDATE accounts SET balance_cents = ?1 WHERE id = ?2",
                        params![open, id],
                    )?;
                }
            }
        }
        // Resolve each position to a stored account: external ref, else IBAN /
        // account number (depots often have no IBAN, numbers differ in leading
        // zeros), else the only depot of this fetch.
        let norm = |x: &str| {
            x.chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
                .trim_start_matches('0')
                .to_uppercase()
        };
        let depots: Vec<&RemoteAccount> = data
            .accounts
            .iter()
            .filter(|a| !archived_ids.contains_key(&a.external_ref))
            .filter(|a| a.kind == AccountKind::Brokerage || a.name.to_lowercase().contains("depot"))
            .collect();
        let resolve = |r: &str| -> Option<i64> {
            if let Some(id) = account_ids.get(r) {
                return Some(*id);
            }
            let n = norm(r);
            if !n.is_empty() {
                for a in &data.accounts {
                    if [&a.external_ref, &a.iban, &a.account_number]
                        .iter()
                        .any(|f| !f.is_empty() && norm(f) == n)
                    {
                        return account_ids.get(&a.external_ref).copied();
                    }
                }
            }
            match depots.as_slice() {
                [only] => account_ids.get(&only.external_ref).copied(),
                _ => None,
            }
        };
        let mut by_account: HashMap<i64, Vec<&RemoteHolding>> = HashMap::new();
        // An explicitly empty inventory must remove stale positions too.
        for account_ref in &data.holdings_fetched {
            if let Some(id) = account_ids.get(account_ref) {
                by_account.entry(*id).or_default();
            }
        }
        let mut unresolved = 0usize;
        for h in &data.holdings {
            if let Some(id) = resolve(&h.account_ref) {
                by_account.entry(id).or_default().push(h);
            } else {
                unresolved += 1;
            }
        }
        crate::depot_debug::log(&format!(
            "store_fetch {kind:?}: incoming_holdings={} depot_accounts={} resolved_accounts={} unresolved={unresolved}",
            data.holdings.len(),
            depots.len(),
            by_account.len()
        ));
        // the inventory before this fetch: a position missing from the new one was sold (#578)
        let mut previous: HashMap<i64, Vec<String>> = HashMap::new();
        for acc_id in by_account.keys() {
            let held = crate::depot_values::positions::held_keys(&tx, *acc_id)?;
            previous.insert(*acc_id, held.into_iter().collect());
        }
        for (acc_id, list) in &by_account {
            tx.execute("DELETE FROM holdings WHERE account_id = ?1", [acc_id])?;
            for remote in list {
                let h = &remote.holding;
                tx.execute(
                    "INSERT INTO holdings(account_id, isin, wkn, name, quantity, current_price_cents, purchase_price_cents, value_cents,
                            prev_day_price_cents, prev_day_value_cents, profit_loss_purchase_cents, profit_loss_purchase_rel,
                            profit_loss_prev_day_cents, profit_loss_prev_day_rel, custody_type,
                            currency, native_price_cents, native_purchase_price_cents, native_value_cents)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
                    params![acc_id, h.isin, h.wkn, h.name, h.quantity, h.current_price_cents, h.purchase_price_cents, h.value_cents,
                            h.prev_day_price_cents, h.prev_day_value_cents, h.profit_loss_purchase_cents, h.profit_loss_purchase_rel,
                            h.profit_loss_prev_day_cents, h.profit_loss_prev_day_rel, h.custody_type,
                            h.currency, h.native_price_cents, h.native_purchase_price_cents, h.native_value_cents],
                )?;
                tx.execute("INSERT INTO holding_classifications(holding_id,asset_class,instrument,direct_crypto) VALUES (?1,?2,?3,?4)",
                    params![tx.last_insert_rowid(),remote.classification.asset_class.map(|a|a.as_str()),remote.classification.instrument.map(|a|a.as_str()),remote.direct_crypto])?;
            }
        }
        let mut imported = 0u32;
        let mut skipped_archived = 0u32;
        {
            let mut ins = tx.prepare(
                "INSERT INTO transactions(account_id, external_id, booking_date, value_date, amount_cents, currency,
                         counterparty, counterparty_iban, purpose, booking_text, purpose_code, gvc,
                         end_to_end_id, mandate_id, creditor_id, item_refs)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            )?;
            for t in &data.transactions {
                let Some(acc_id) = account_ids.get(&t.account_ref) else {
                    if let Some(id) = archived_ids.get(&t.account_ref)
                        && !deleted_refs.contains(&t.account_ref)
                    {
                        let known = Stored::read(&tx, *id, &t.external_id)?.is_some();
                        skipped_archived += u32::from(!known);
                    }
                    continue;
                };
                let stored = match Stored::read(&tx, *acc_id, &t.external_id)? {
                    None if adopt_legacy_content_id(&tx, *acc_id, t)? => {
                        Stored::read(&tx, *acc_id, &t.external_id)?
                    }
                    stored => stored,
                };
                // A hold never takes over or merges with an imported row: it
                // is dropped once posted and would take the row with it.
                let hold = t.external_id.starts_with("pending-")
                    || super::advanzia::is_advanzia_hold(&t.external_id);
                match stored {
                    // Deleted bookings stay deleted: neither inserted nor updated again.
                    Some(stored) if stored.deleted => {}
                    // Known: an untouched duplicate of an imported row gives way to
                    // it, anything else is refreshed.
                    Some(stored) => {
                        if hold
                            || !crate::importers::moneymoney_import::merge_synced_twin(
                                &tx,
                                stored.id,
                                &t.booking_date,
                                t.amount_cents,
                                &t.counterparty_iban,
                                &t.counterparty,
                                &t.purpose,
                            )?
                        {
                            refresh_existing(&tx, &stored, t)?;
                        }
                    }
                    // New: adopt the row imported from MoneyMoney (keeps category +
                    // note), else insert it.
                    None => {
                        if hold
                            || !crate::importers::moneymoney_import::adopt_import(
                                &tx,
                                *acc_id,
                                &t.external_id,
                                &t.booking_date,
                                t.amount_cents,
                                &t.counterparty_iban,
                                &t.counterparty,
                                &t.purpose,
                                &t.booking_text,
                            )?
                        {
                            ins.execute(params![
                                acc_id,
                                t.external_id,
                                t.booking_date,
                                t.value_date,
                                t.amount_cents,
                                t.currency,
                                t.counterparty,
                                t.counterparty_iban,
                                t.purpose,
                                t.booking_text,
                                t.purpose_code,
                                t.gvc,
                                t.end_to_end_id,
                                t.mandate_id,
                                t.creditor_id,
                                t.item_refs
                            ])?;
                            imported += 1;
                            if data.adjust_balances {
                                tx.prepare_cached(
                                    "UPDATE accounts SET balance_cents = balance_cents + ?1, balance_date = ?2 WHERE id = ?3",
                                )?
                                .execute(params![t.amount_cents, t.booking_date, acc_id])?;
                            }
                        }
                    }
                }
                // Portal card bookings stored before their card was known (#243).
                if t.booking_text
                    .starts_with(&format!("{CARD_BOOKING_TEXT}{CARD_SEPARATOR}"))
                {
                    tx.execute(
                        "UPDATE transactions SET booking_text = ?1
                          WHERE account_id = ?2 AND external_id = ?3 AND booking_text = ?4",
                        params![t.booking_text, acc_id, t.external_id, CARD_BOOKING_TEXT],
                    )?;
                }
            }
        }
        // These banks abbreviate the tax lines that MoneyMoney shows in full.
        if matches!(
            kind,
            ConnectorKind::FordMoney | ConnectorKind::OpelDirektbank
        ) {
            for id in account_ids.values() {
                crate::importers::moneymoney_import::merge_synced_tax_twins(&tx, *id)?;
            }
        }
        // Securities transactions (#314): beside the cash bookings, then matched to them.
        // Standing orders: complete per account, so the stored ones are replaced.
        for r in &data.standing_orders_fetched {
            if let Some(id) = resolve(r) {
                let orders: Vec<&RemoteStandingOrder> = data
                    .standing_orders
                    .iter()
                    .filter(|o| &o.account_ref == r)
                    .collect();
                crate::standing_orders::replace_for_account(&tx, id, &orders)?;
            }
        }
        for st in &data.security_transactions {
            if let Some(id) = resolve(&st.account_ref) {
                let mut t = st.transaction.clone();
                t.external_id = st.external_id.clone();
                crate::security_transactions::upsert(&tx, id, &t)?;
                crate::security_transactions::link_by_external_id(
                    &tx,
                    id,
                    &t.external_id,
                    &st.cash_external_id,
                )?;
            }
        }
        if !data.security_transactions.is_empty() || !data.transactions.is_empty() {
            crate::security_transactions::link_cash_bookings(&tx)?;
        }
        // Drop optimistic transfer bookings once the bank's real booking arrived
        // (same account, amount and recipient IBAN; a batch: amount only) or after 14 days.
        // Connectors that report their complete open-hold list (`hold_prefix`) manage
        // those holds below (#1111) so same-amount payments or >14d holds are not dropped early.
        let managed_hold_like = data
            .hold_prefix
            .as_deref()
            .filter(|p| !p.is_empty())
            .map(|p| format!("{}%", p.replace('%', "").replace('_', "\\_")))
            .unwrap_or_default();
        tx.execute(
            "UPDATE transactions SET deleted_ms = ?1
              WHERE external_id LIKE 'pending-%' AND deleted_ms IS NULL
                -- web-portal holds (#629) are settled or discarded explicitly
                AND external_id NOT LIKE 'pending-hold-portal-%'
                AND (?2 = '' OR external_id NOT LIKE ?2 ESCAPE '\\') AND (
                imported_unix < strftime('%s','now') - 14*86400 OR EXISTS (
                    SELECT 1 FROM transactions r WHERE r.account_id = transactions.account_id
                      AND r.deleted_ms IS NULL
                      AND r.external_id NOT LIKE 'pending-%' AND r.external_id NOT LIKE 'advanzia-hold-%' AND r.amount_cents = transactions.amount_cents
                      AND (r.counterparty_iban = transactions.counterparty_iban
                           -- a batch booking carries no recipient IBAN; banks put
                           -- whatever they like there (often the own account). So a
                           -- batch only matches a booking that says \"Sammel…\" itself,
                           -- shortly after it was placed: a same-sized single payment
                           -- must not hide it.
                           OR (transactions.booking_text LIKE 'Sammelüberweisung%'
                               AND (r.booking_text LIKE '%Sammel%' OR r.purpose LIKE '%Sammel%')
                               AND r.booking_date <= date(transactions.booking_date, '+7 days')))
                      AND r.booking_date >= transactions.booking_date))",
            params![crate::dates::now_ms(), managed_hold_like],
        )?;
        // Entries the connector recognised as no real movement (stored by an older version).
        for (account_ref, external_id) in &data.withdrawn {
            let Some(acc_id) = account_ids.get(account_ref) else {
                continue;
            };
            let row: Option<i64> = tx
                .query_row(
                    "SELECT id FROM transactions WHERE account_id = ?1 AND external_id = ?2 AND deleted_ms IS NULL",
                    params![acc_id, external_id],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(row_id) = row {
                crate::sync_merge::soft_delete_booking(&tx, row_id)?;
            }
        }
        // Card holds: keep exactly the ones the bank still reports as open.
        if let Some(prefix) = data.hold_prefix.as_deref().filter(|p| !p.is_empty()) {
            let current: std::collections::HashSet<&str> = data
                .transactions
                .iter()
                .filter(|t| t.external_id.starts_with(prefix))
                .map(|t| t.external_id.as_str())
                .collect();
            let like = format!("{}%", prefix.replace('%', "").replace('_', "\\_"));
            for acc_id in account_ids.values() {
                let stale: Vec<(i64, String)> = {
                    let mut q = tx.prepare(
                        "SELECT id, external_id FROM transactions
                         WHERE account_id = ?1 AND external_id LIKE ?2 ESCAPE '\\' AND deleted_ms IS NULL",
                    )?;
                    let rows =
                        q.query_map(params![acc_id, like], |r| Ok((r.get(0)?, r.get(1)?)))?;
                    rows.collect::<Result<_, _>>()?
                };
                for (row_id, ext) in stale {
                    if !current.contains(ext.as_str()) {
                        crate::sync_merge::soft_delete_booking(&tx, row_id)?;
                    }
                }
            }
        }
        let mut postbox_added = 0u32;
        let mut documents_stored = 0u32;
        let documents_incomplete = data.documents_incomplete;
        for d in &data.documents {
            if archived_ids.contains_key(&d.account_ref) || blocked_refs.contains(&d.account_ref) {
                continue;
            }
            let by_ref = |tx: &rusqlite::Transaction| -> Option<i64> {
                tx.query_row(
                    // includes soft-deleted rows
                    "SELECT id FROM accounts WHERE connection_id = ?1 AND external_ref = ?2",
                    params![connection_id, d.account_ref],
                    |r| r.get(0),
                )
                .optional()
                .ok()
                .flatten()
            };
            // no account given: the document stays without one (PostBox, #838)
            let acc_id = if d.account_ref.is_empty() {
                None
            } else {
                let Some(id) = account_ids
                    .get(&d.account_ref)
                    .copied()
                    .or_else(|| by_ref(&tx))
                    .or_else(|| account_ids.values().next().copied())
                else {
                    continue;
                };
                Some(id)
            };
            let booking_id: Option<i64> = match (&d.booking_external_id, acc_id) {
                (Some(ext), Some(acc_id)) => tx
                    .query_row(
                        // includes soft-deleted rows
                        "SELECT id FROM transactions WHERE account_id = ?1 AND external_id = ?2",
                        params![acc_id, ext],
                        |r| r.get(0),
                    )
                    .optional()?,
                _ => None,
            };
            // without bytes (download failed) only a stored copy gets its account
            let added = if d.bytes.is_empty() {
                Ok(None)
            } else {
                crate::documents::insert_doc(
                    &tx,
                    booking_id,
                    acc_id,
                    &d.title,
                    &d.doc_type,
                    &d.mime_type,
                    &d.file_name,
                    &d.bytes,
                    &d.source_url,
                    &d.date,
                )
            };
            if matches!(added, Ok(Some(_))) {
                documents_stored += 1;
            }
            match (added, acc_id) {
                (Ok(Some(_)), _)
                    if d.source_url.starts_with("comdirect:postbox:")
                        || d.source_url
                            .starts_with(super::consorsbank_archive::SOURCE_PREFIX) =>
                {
                    postbox_added += 1
                }
                // stored before without an account: it gets the one found now
                (Ok(None), Some(acc_id)) if !d.source_url.is_empty() => {
                    tx.execute(
                        "UPDATE booking_documents SET account_id = ?1
                          WHERE source_url = ?2 AND account_id IS NULL AND booking_id IS NULL",
                        params![acc_id, d.source_url],
                    )?;
                }
                _ => {}
            }
        }
        let postbox_documents = data.postbox_fetched.then_some(postbox_added);
        if data.postbox_fetched {
            tx.execute(
                "INSERT INTO settings(key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![postbox_last_key(connection_id), today()],
            )?;
        }
        for r in &data.statements_fetched {
            if let Some(id) = account_ids.get(r) {
                tx.execute(
                    "INSERT INTO settings(key, value) VALUES (?1, ?2)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![statements_last_key(*id), today()],
                )?;
            }
        }
        for acc in data.accounts.iter().filter(|a| a.kind == AccountKind::Loan) {
            if let Some(id) = account_ids.get(&acc.external_ref) {
                Vault::derive_initial_balance(&tx, *id)?;
            }
        }
        // Value history of depots (#578), after every balance update above: what this fetch
        // reported per depot. A depot whose inventory came (maybe an empty one) confirms a 0 and
        // closes sold positions; one whose total came without positions (an overview) records the
        // total alone. An unread balance records no total (its positions still count).
        let observed_ms = crate::dates::now_ms();
        let unknown: std::collections::HashSet<i64> = data
            .balances_unknown
            .iter()
            .filter_map(|r| account_ids.get(r).copied())
            .collect();
        // every fetched account once: with its inventory, or (an overview) the total alone
        let fetched = data
            .accounts
            .iter()
            .filter_map(|acc| account_ids.get(&acc.external_ref).copied())
            .chain(by_account.keys().copied())
            .collect::<std::collections::BTreeSet<i64>>();
        for id in fetched {
            let positions: Option<Vec<&crate::models::Holding>> = by_account
                .get(&id)
                .map(|list| list.iter().map(|r| &r.holding).collect());
            crate::depot_values::record_fetch(
                &tx,
                id,
                positions.as_deref(),
                previous.get(&id).map_or(&[][..], |p| p.as_slice()),
                !unknown.contains(&id),
                observed_ms,
            )?;
        }
        if kind == ConnectorKind::Advanzia {
            super::advanzia::settle_holds(&tx, &account_ids, &mut data)?;
        }
        let categorized = Vault::apply_rules_inner(&tx, true)?;
        if let Some(state) = &data.state {
            crate::vault::connections::store_state(&tx, connection_id, state)?;
        }
        // one account: the others are as old as before (automatic sync, freshness)
        if !data.single_account {
            tx.execute(
                "UPDATE bank_connections SET last_sync_unix = strftime('%s','now') WHERE id = ?1",
                [connection_id],
            )?;
        }
        // Accounts by the name they carry here instead of their bank reference (an IBAN).
        let mut warnings = std::mem::take(&mut data.warnings);
        if !warnings.is_empty() {
            for acc in data.accounts.iter().filter(|a| !a.external_ref.is_empty()) {
                let name: Option<String> = tx
                    .query_row(
                        "SELECT name FROM accounts WHERE connection_id = ?1 AND external_ref = ?2 AND deleted_ms IS NULL",
                        params![connection_id, acc.external_ref],
                        |r| r.get(0),
                    )
                    .optional()?;
                let name = name.unwrap_or_else(|| acc.name.clone());
                for w in &mut warnings {
                    *w = w.replace(&acc.external_ref, &name);
                }
            }
        }
        tx.commit()?;
        if let Some(data) = deferred {
            vault
                .pending_account_fetches
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(connection_id, (kind, data));
        }
        // The catch-up after switching retrieval on again is done once a sync brings no new document
        // and left none out (a failed request, a limit).
        if documents_stored == 0
            && !documents_incomplete
            && !data.single_account
            && documents_fetch_on(&db, connection_id)?
        {
            db.execute(
                "DELETE FROM settings WHERE key = ?1",
                [documents_catchup_key(connection_id)],
            )?;
        }
        Ok(SyncSummary {
            accounts_updated,
            transactions_imported: imported,
            transactions_categorized: categorized,
            transactions_skipped_archived: skipped_archived,
            new_accounts,
            revoked_accounts,
            postbox_documents,
            warnings,
        })
    }
}

/// Sidebar sections (fixed order).
pub(crate) fn group_for(kind: AccountKind) -> (&'static str, &'static str, i32) {
    match kind {
        AccountKind::Checking
        | AccountKind::Savings
        | AccountKind::Cash
        | AccountKind::PaymentService => ("Giro & Tagesgeld", "building.columns", 1),
        AccountKind::CreditCard => ("Kreditkarten", "creditcard", 2),
        AccountKind::Brokerage => ("Depots & ETFs", "chart.pie", 3),
        AccountKind::Loan => ("Darlehen & Kredite", "house", 4),
    }
}

/// Day retrieval was switched off; moved to the catch-up key once it is switched on again.
fn documents_off_since_key(connection_id: i64) -> String {
    format!("documents.fetch.off_since.{connection_id}")
}

/// Oldest day still to catch up on after retrieval was switched on again.
fn documents_catchup_key(connection_id: i64) -> String {
    format!("documents.fetch.catchup.{connection_id}")
}

/// Vault setting key: document retrieval of this connection (`0` = off; absent = on).
pub(crate) fn documents_fetch_key(connection_id: i64) -> String {
    format!("documents.fetch.{connection_id}")
}

/// Forgets the document retrieval settings of a deleted connection.
pub(crate) fn clear_documents_settings(
    db: &rusqlite::Connection,
    connection_id: i64,
) -> CoreResult<()> {
    db.execute(
        "DELETE FROM settings WHERE key IN (?1, ?2, ?3)",
        params![
            documents_fetch_key(connection_id),
            documents_off_since_key(connection_id),
            documents_catchup_key(connection_id)
        ],
    )?;
    Ok(())
}

/// Whether the connection fetches documents (default: yes).
pub(crate) fn documents_fetch_on(
    db: &rusqlite::Connection,
    connection_id: i64,
) -> CoreResult<bool> {
    let value: Option<String> = db
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            [documents_fetch_key(connection_id)],
            |r| r.get(0),
        )
        .optional()?;
    Ok(value.as_deref() != Some("0"))
}
/// Days between two automatic statement fetches of one account.
const STATEMENTS_AUTO_DAYS: i64 = 7;

/// Vault setting key: fetch PDF statements of this account automatically.
pub(crate) fn statements_auto_key(account_id: i64) -> String {
    format!("statements.auto.{account_id}")
}

/// Vault setting key: fetch the comdirect PostBox through the REST API next
/// to this FinTS login (#838).
pub(crate) fn postbox_key(connection_id: i64) -> String {
    format!("comdirect.postbox.{connection_id}")
}

/// Vault setting key: date (YYYY-MM-DD) of the last PostBox fetch of a login.
pub(crate) fn postbox_last_key(connection_id: i64) -> String {
    format!("comdirect.postbox.last.{connection_id}")
}

/// Days between two PostBox fetches during regular syncs: each needs its own
/// approval in the photoTAN app.
const POSTBOX_DAYS: i64 = 7;

/// Whether a sync may add the PostBox fetch (#838).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PostboxMode {
    /// Never: an automatic sync must not ask for an approval.
    Off,
    /// When it is on for the login and due.
    Due,
    /// Now ("PostBox jetzt abrufen").
    Now,
}

/// Vault setting key: date (YYYY-MM-DD) of the last automatic statement fetch.
pub(crate) fn statements_last_key(account_id: i64) -> String {
    format!("statements.last.{account_id}")
}

#[uniffi::export]
impl Vault {
    /// Prepares a sync for one bank connection. `secret` is the PIN/password
    /// (wiped from memory when the session ends). With `demo = true` the bank
    /// is simulated (push-TAN / code challenge included) – no network.
    pub fn begin_sync(
        self: Arc<Self>,
        connection_id: i64,
        secret: String,
        demo: bool,
    ) -> Result<Arc<SyncSession>, CoreError> {
        self.begin_sync_inner(connection_id, secret, demo, None, PostboxMode::Due, None)
    }

    /// Like [`begin_sync`](Self::begin_sync) for an automatic sync in the
    /// background: nothing in it may need a new approval, so the comdirect
    /// PostBox fetch (#838) is left out.
    pub fn begin_quiet_sync(
        self: Arc<Self>,
        connection_id: i64,
        secret: String,
        demo: bool,
    ) -> Result<Arc<SyncSession>, CoreError> {
        self.begin_sync_inner(connection_id, secret, demo, None, PostboxMode::Off, None)
    }

    /// Like [`begin_sync`](Self::begin_sync) for one account of its login
    /// ("<Konto> abrufen", #1124): only its balance and bookings are fetched
    /// and stored; the login's other accounts stay as they are. An account
    /// still under a MoneyMoney ref (imported and not yet taken over, or
    /// marked by „Abrufweg umstellen“) is not known to the bank under it, so
    /// it gets a sync of the whole login, documents included (see
    /// `single_account_ref`, #1417).
    /// Documents
    /// wait for the next full sync: connectors remember what they downloaded,
    /// and a document of another account would be remembered but not stored.
    pub fn begin_account_sync(
        self: Arc<Self>,
        account_id: i64,
        secret: String,
        demo: bool,
    ) -> Result<Arc<SyncSession>, CoreError> {
        let (conn_id, ext) = self.account_login(account_id)?;
        let only = single_account_ref(ext, || {
            super::account_selection::skipped_refs(&*self.db()?, conn_id)
        })?;
        self.begin_sync_inner(conn_id, secret, demo, None, PostboxMode::Off, only)
    }

    /// Turns the PostBox fetch through comdirect's REST API on or off for a
    /// comdirect FinTS login (#838). Needs the API's client id and secret.
    /// For a Consorsbank FinTS login it switches the Online-Archiv (#1024),
    /// which logs in with the login's own account number and PIN.
    pub fn set_comdirect_postbox(
        &self,
        connection_id: i64,
        enabled: bool,
    ) -> Result<(), CoreError> {
        let db = self.db()?;
        if !enabled {
            db.execute(
                "DELETE FROM settings WHERE key IN (?1, ?2)",
                params![postbox_key(connection_id), postbox_last_key(connection_id)],
            )?;
            return Ok(());
        }
        let key: String = db
            .query_row(
                "SELECT connector FROM bank_connections WHERE id = ?1 AND deleted_ms IS NULL",
                [connection_id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| CoreError::NotFound {
                what: trf("Bankzugang {0}", &[&connection_id]),
            })?;
        if is_consorsbank_fints(&key) {
            db.execute(
                "INSERT INTO settings(key, value) VALUES (?1, '1')
                 ON CONFLICT(key) DO UPDATE SET value = '1'",
                [postbox_key(connection_id)],
            )?;
            return Ok(());
        }
        if !is_comdirect_fints(&key) {
            return Err(CoreError::invalid(tr(
                "Die PostBox lässt sich nur für comdirect-Zugänge per FinTS zusätzlich abrufen.",
            )));
        }
        for k in ["comdirect.client_id", "comdirect.client_secret"] {
            if Vault::setting_inner(&db, k)?.is_none_or(|v| v.trim().is_empty()) {
                return Err(CoreError::MissingSetting { key: k.into() });
            }
        }
        db.execute(
            "INSERT INTO settings(key, value) VALUES (?1, '1')
             ON CONFLICT(key) DO UPDATE SET value = '1'",
            [postbox_key(connection_id)],
        )?;
        Ok(())
    }

    /// Is the PostBox fetch through comdirect's REST API on for this login?
    pub fn comdirect_postbox_enabled(&self, connection_id: i64) -> Result<bool, CoreError> {
        let db = self.db()?;
        Ok(Vault::setting_inner(&db, &postbox_key(connection_id))?.as_deref() == Some("1"))
    }

    /// Syncs a comdirect FinTS login and fetches its PostBox now, whenever it
    /// was fetched last (#838). Same session / TAN flow as `begin_sync`, with
    /// the PostBox approval first.
    pub fn fetch_comdirect_postbox(
        self: Arc<Self>,
        connection_id: i64,
        secret: String,
        demo: bool,
    ) -> Result<Arc<SyncSession>, CoreError> {
        if !self.comdirect_postbox_enabled(connection_id)? {
            return Err(CoreError::invalid(tr(
                "Der PostBox-Abruf ist für diesen Zugang nicht eingeschaltet.",
            )));
        }
        if !documents_fetch_on(&*self.db()?, connection_id)? {
            return Err(CoreError::invalid(tr(
                "Der Dokumentenabruf ist für diesen Zugang ausgeschaltet.",
            )));
        }
        self.begin_sync_inner(connection_id, secret, demo, None, PostboxMode::Now, None)
    }

    /// Whether this connection's bank delivers documents at all (`Capability::Documents` of its
    /// row in the bank matrix): only then the settings offer to switch the retrieval.
    pub fn documents_fetch_supported(&self, connection_id: i64) -> Result<bool, CoreError> {
        let (kind, _) = self.portal_connection(connection_id)?;
        Ok(super::catalog::BANK_MATRIX.iter().any(|e| {
            e.kind == Some(kind)
                && e.capabilities
                    .contains(&super::catalog::Capability::Documents)
        }))
    }

    /// Whether syncs of this connection fetch documents (PDFs: receipts, statements, PostBox).
    /// On unless switched off; what is stored stays either way.
    pub fn documents_fetch_enabled(&self, connection_id: i64) -> Result<bool, CoreError> {
        let db = self.db()?;
        documents_fetch_on(&db, connection_id)
    }

    /// Switches the document retrieval of a connection on or off. Off: no new documents, nothing
    /// remembered of them. On again: the next syncs reach back to the day it was switched off
    /// (until a sync brings no new document), so what was left out is fetched.
    pub fn set_documents_fetch(&self, connection_id: i64, enabled: bool) -> Result<(), CoreError> {
        let db = self.db()?;
        let upsert = "INSERT INTO settings(key, value) VALUES (?1, ?2)
                      ON CONFLICT(key) DO UPDATE SET value = excluded.value";
        if enabled {
            let off_since = Vault::setting_inner(&db, &documents_off_since_key(connection_id))?;
            if let Some(off) = off_since {
                // keep the oldest day if an earlier catch-up is still running
                let oldest = match Vault::setting_inner(&db, &documents_catchup_key(connection_id))?
                {
                    Some(c) if c < off => c,
                    _ => off,
                };
                db.execute(
                    upsert,
                    params![documents_catchup_key(connection_id), oldest],
                )?;
            }
            db.execute(
                "DELETE FROM settings WHERE key IN (?1, ?2)",
                params![
                    documents_fetch_key(connection_id),
                    documents_off_since_key(connection_id)
                ],
            )?;
        } else {
            db.execute(upsert, params![documents_fetch_key(connection_id), "0"])?;
            db.execute(
                "INSERT OR IGNORE INTO settings(key, value) VALUES (?1, ?2)",
                params![documents_off_since_key(connection_id), today()],
            )?;
        }
        Ok(())
    }

    /// Turns the automatic PDF statement fetch (FinTS HKEKA, at most weekly,
    /// during regular syncs) of one account on or off.
    pub fn set_auto_statements(&self, account_id: i64, enabled: bool) -> Result<(), CoreError> {
        let db = self.db()?;
        if enabled {
            db.execute(
                "INSERT INTO settings(key, value) VALUES (?1, '1')
                 ON CONFLICT(key) DO UPDATE SET value = '1'",
                [statements_auto_key(account_id)],
            )?;
        } else {
            db.execute(
                "DELETE FROM settings WHERE key IN (?1, ?2)",
                params![
                    statements_auto_key(account_id),
                    statements_last_key(account_id)
                ],
            )?;
        }
        Ok(())
    }

    /// Is the automatic statement fetch on for this account?
    pub fn auto_statements_enabled(&self, account_id: i64) -> Result<bool, CoreError> {
        let db = self.db()?;
        Ok(Vault::setting_inner(&db, &statements_auto_key(account_id))?.as_deref() == Some("1"))
    }

    /// Explicitly fetches the electronic bank statements (FinTS HKEKA, PDF) of ONE account
    /// into the document archive. Same session / TAN flow as `begin_sync`; bookings and
    /// balances stay untouched. Regular syncs never fetch statements.
    pub fn fetch_account_statements(
        self: Arc<Self>,
        account_id: i64,
        secret: String,
        demo: bool,
    ) -> Result<Arc<SyncSession>, CoreError> {
        let (conn_id, ext) = self.account_login(account_id)?;
        let kind: String = self.db()?.query_row(
            "SELECT connector FROM bank_connections WHERE id = ?1",
            [conn_id],
            |r| r.get(0),
        )?;
        let supports_statements = ConnectorKind::from_str(&kind).is_some_and(|k| {
            matches!(
                k,
                ConnectorKind::FinTsGeneric
                    | ConnectorKind::VolksbankKasselGoettingen
                    | ConnectorKind::Direkt1822
                    | ConnectorKind::Easybank
                    | ConnectorKind::FordMoney
                    | ConnectorKind::OpelDirektbank
                    | ConnectorKind::Bhw
                    | ConnectorKind::HanseaticBank
                    | ConnectorKind::Ffb
            )
        });
        if !supports_statements {
            return Err(CoreError::invalid(tr(
                "Für diese Bank können keine Kontoauszüge abgerufen werden.",
            )));
        }
        self.begin_sync_inner(conn_id, secret, demo, Some(ext), PostboxMode::Off, None)
    }
}

impl Vault {
    /// Login (connection id) and external ref of a synced account.
    fn account_login(&self, account_id: i64) -> CoreResult<(i64, String)> {
        let (conn_id, ext): (Option<i64>, String) = self
            .db()?
            .query_row(
                "SELECT connection_id, external_ref FROM accounts WHERE id = ?1 AND deleted_ms IS NULL",
                [account_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| CoreError::NotFound {
                what: trf("Konto {0}", &[&account_id]),
            })?;
        let conn_id = conn_id
            .ok_or_else(|| CoreError::invalid(tr("Dieses Konto hat keine Bankverbindung.")))?;
        Ok((conn_id, ext))
    }

    /// The documents a sync of this connection fetches; `statements_only` =
    /// external ref of the account whose statements were asked for.
    pub(super) fn documents_request(
        &self,
        connection_id: i64,
        statements_only: Option<String>,
    ) -> CoreResult<Documents> {
        if !documents_fetch_on(&*self.db()?, connection_id)? {
            // an explicit request must not run for nothing: store_fetch would drop what it brings
            if statements_only.is_some() {
                return Err(CoreError::invalid(tr(
                    "Der Dokumentenabruf ist für diesen Zugang ausgeschaltet.",
                )));
            }
            return Ok(Documents::Off);
        }
        if let Some(account) = statements_only {
            return Ok(Documents::StatementsOnly(account));
        }
        Ok(Documents::On {
            statements: self.statements_due(connection_id)?,
            catchup_since: Vault::setting_inner(
                &*self.db()?,
                &documents_catchup_key(connection_id),
            )?,
        })
    }

    /// External refs of this connection's accounts whose automatic statement
    /// fetch is on and due (never, or more than `STATEMENTS_AUTO_DAYS` ago).
    pub(super) fn statements_due(&self, connection_id: i64) -> CoreResult<Vec<String>> {
        let db = self.db()?;
        let mut stmt = db.prepare(
            "SELECT id, external_ref FROM accounts WHERE connection_id = ?1 AND kind != 'loan' AND deleted_ms IS NULL",
        )?;
        let accounts: Vec<(i64, String)> = stmt
            .query_map([connection_id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let today = crate::dates::today();
        let mut due = Vec::new();
        for (id, ext) in accounts {
            if Vault::setting_inner(&db, &statements_auto_key(id))?.as_deref() != Some("1") {
                continue;
            }
            let last = Vault::setting_inner(&db, &statements_last_key(id))?
                .and_then(|d| jiff::civil::Date::strptime("%Y-%m-%d", &d).ok());
            if last.is_none_or(|d| crate::dates::days_between(d, today) >= STATEMENTS_AUTO_DAYS) {
                due.push(ext);
            }
        }
        Ok(due)
    }

    fn begin_sync_inner(
        self: Arc<Self>,
        connection_id: i64,
        secret: String,
        demo: bool,
        statements_only: Option<String>,
        postbox: PostboxMode,
        only_account: Option<String>,
    ) -> Result<Arc<SyncSession>, CoreError> {
        // No PIN typed: use the one stored in the vault ("PIN im Tresor speichern").
        let secret = if secret.is_empty() {
            let db = self.db()?;
            Vault::setting_inner(&db, &crate::vault::connections::pin_key(connection_id))?
                .unwrap_or_default()
        } else {
            secret
        };
        let secret = SecretString::new(secret);
        let (kind, key_str, login, state, ctx_settings, since, skipped, demo) = {
            let db = self.db()?;
            let (kind, login, state): (String, String, SecretJson) = db
                .query_row(
                    "SELECT connector, login_name, state_json FROM bank_connections WHERE id = ?1 AND deleted_ms IS NULL",
                    [connection_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?
                .ok_or_else(|| CoreError::NotFound {
                    what: trf("Bankverbindung {0}", &[&connection_id]),
                })?;
            let key_str = kind.clone();
            // Magic demo login ("demo"): simulated bank, no network, no FinTS product ID.
            let magic = crate::demo::is_magic_demo_login(&login);
            let demo = demo || magic;
            if !magic {
                crate::banking_gate::check_connector(&key_str)?;
            }
            let kind = ConnectorKind::from_str(&kind)
                .ok_or_else(|| CoreError::invalid(format!("unknown connector {kind}")))?;
            let mut settings = HashMap::new();
            for (key, ..) in crate::vault::KNOWN_SETTINGS {
                if let Some(v) = Vault::setting_inner(&db, key)? {
                    settings.insert(key.to_string(), SecretString::new(v));
                }
            }
            if descriptor(kind)
                .required_settings
                .iter()
                .any(|k| k == fints::product_id::OWN_KEY)
            {
                let mode =
                    Vault::setting_inner(&db, fints::product_id::MODE_KEY)?.unwrap_or_default();
                let own =
                    Vault::setting_inner(&db, fints::product_id::OWN_KEY)?.unwrap_or_default();
                if let Some((_, id)) = fints::product_id::resolve(&mode, &own) {
                    settings.insert(
                        fints::product_id::OWN_KEY.to_string(),
                        SecretString::new(id),
                    );
                }
            }
            if let Some(blz) = ConnectorKind::fints_blz(&key_str) {
                let k = format!("fints.url.{blz}");
                match Vault::setting_inner(&db, &k)? {
                    Some(v) if !v.is_empty() => {
                        // stored before the directory fix: move to the bank's current endpoint
                        let v = match fints_directory::replacement_for_retired_url(blz, &v) {
                            Some(url) => {
                                db.execute(
                                    "UPDATE settings SET value = ?1 WHERE key = ?2",
                                    params![url, k],
                                )?;
                                url.to_string()
                            }
                            None => v,
                        };
                        settings.insert(k, SecretString::new(v));
                    }
                    // e.g. a MoneyMoney import created the login without a URL
                    _ => {
                        if let Some(url) = fints_directory::url_for_blz(blz) {
                            settings.insert(k, SecretString::new(url.to_string()));
                        }
                    }
                }
            }
            let rah_key = format!("fints.rah10.private.{connection_id}");
            if let Some(value) = Vault::setting_inner(&db, &rah_key)? {
                // Reserve before any network access, even if sync is cancelled or fails.
                // The vault mutex serializes reservations for concurrent local sessions.
                let counter_key = format!("fints.rah10.counter.{connection_id}");
                let first: u64 = Vault::setting_inner(&db, &counter_key)?
                    .and_then(|v| v.parse().ok())
                    .ok_or(CoreError::SessionClosed)?;
                let limit = first
                    .checked_add(10_000)
                    .filter(|n| *n < 10_000_000_000_000_000)
                    .ok_or(CoreError::SessionClosed)?;
                db.execute(
                    "UPDATE settings SET value = ?1 WHERE key = ?2",
                    params![limit.to_string(), counter_key],
                )?;
                let bank_keys_key = format!("fints.rah10.bank_keys.{connection_id}");
                if let Some(keys) = Vault::setting_inner(&db, &bank_keys_key)? {
                    settings.insert(bank_keys_key, SecretString::new(keys));
                }
                settings.insert(rah_key, SecretString::new(value));
                settings.insert(
                    "fints.rah10.counter_start".into(),
                    SecretString::new(first.to_string()),
                );
            }
            let tm_key = format!("fints.tan_medium.{connection_id}");
            if let Some(v) = Vault::setting_inner(&db, &tm_key)?
                && !v.trim().is_empty()
            {
                settings.insert(tm_key, SecretString::new(v));
            }
            // this login's own Enable Banking application, if it has one (#824)
            crate::vault::enable_banking_apps::overlay(&db, connection_id, &mut settings)?;
            // PayPal via API signature needs no REST app.
            let nvp = kind == ConnectorKind::PayPal && super::paypal::nvp_configured(&settings);
            if !demo && !nvp {
                for key in descriptor(kind).required_settings {
                    if !settings.contains_key(&key) {
                        return Err(CoreError::MissingSetting { key });
                    }
                }
            }
            // Incremental, per account: from its newest booking minus 10 days.
            let newest: Vec<(String, Option<String>)> = {
                let mut st = db.prepare(
                    "SELECT a.external_ref,
                            (SELECT MAX(t.booking_date) FROM transactions t
                              WHERE t.account_id = a.id AND t.external_id NOT LIKE 'pending-%' AND t.deleted_ms IS NULL)
                       FROM accounts a
                      WHERE a.connection_id = ?1 AND a.archived_unix IS NULL AND a.deleted_ms IS NULL",
                )?;
                let rows = st
                    .query_map([connection_id], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<Result<Vec<(String, Option<String>)>, _>>()?;
                // one account: the window starts at its own newest booking
                rows.into_iter()
                    .filter(|(r, _)| only_account.as_ref().is_none_or(|o| o == r))
                    .collect()
            };
            let skipped = super::account_selection::skipped_refs(&db, connection_id)?;
            // one account, archived or switched off: store_fetch would drop all it brings
            if only_account.as_ref().is_some_and(|r| skipped.contains(r)) {
                return Err(CoreError::invalid(tr(
                    "Dieses Konto ist archiviert oder vom Abruf ausgenommen.",
                )));
            }
            let days: i64 = Vault::setting_inner(&db, "sync.days_back")?
                .and_then(|d| d.parse().ok())
                .unwrap_or(90);
            let since = since_window(crate::dates::today(), days, &newest);
            (kind, key_str, login, state, settings, since, skipped, demo)
        };
        let (since, since_by_account) = since;
        let documents = match only_account {
            // see begin_account_sync
            Some(_) => Documents::Off,
            None => self.documents_request(connection_id, statements_only)?,
        };
        let ctx = ConnectContext {
            connection_id,
            login_name: login,
            secret,
            settings: ctx_settings,
            state,
            since,
            since_by_account,
            documents,
            skipped_accounts: skipped.into_iter().collect(),
            only_account,
        };
        let mut connector = make_connector_for(kind, &key_str, demo);
        if !demo && is_comdirect_fints(&key_str) {
            if let Some(since) = self.postbox_since(connection_id, &ctx, postbox, true)? {
                connector = Box::new(super::comdirect_postbox::FintsWithPostbox::new(
                    connector, since,
                ));
            }
        } else if !demo
            && is_consorsbank_fints(&key_str)
            && let Some(since) = self.postbox_since(connection_id, &ctx, postbox, false)?
        {
            let known = self.consorsbank_known_documents(connection_id)?;
            connector = Box::new(super::comdirect_postbox::FintsWithPostbox::with_source(
                connector,
                Box::new(super::consorsbank_archive::ConsorsbankArchive::new(known)),
                since,
            ));
        }
        Ok(Arc::new(SyncSession::new(
            self.clone(),
            kind,
            connector,
            ctx,
        )))
    }

    /// `Some(oldest document date)` when this sync also fetches the comdirect
    /// PostBox (#838): switched on, client id and secret set, and due (never
    /// fetched, or `POSTBOX_DAYS` ago) or asked for now. A week of overlap
    /// covers documents dated a little before they appear; the unique
    /// `source_url` keeps them from being stored twice.
    fn postbox_since(
        &self,
        connection_id: i64,
        ctx: &ConnectContext,
        mode: PostboxMode,
        needs_api_keys: bool,
    ) -> CoreResult<Option<String>> {
        if mode == PostboxMode::Off
            || !documents_fetch_on(&*self.db()?, connection_id)?
            || !self.comdirect_postbox_enabled(connection_id)?
        {
            return Ok(None);
        }
        let api_keys: &[&str] = if needs_api_keys {
            &["comdirect.client_id", "comdirect.client_secret"]
        } else {
            &[]
        };
        for &k in api_keys {
            if let Err(e) = ctx.setting(k) {
                return if mode == PostboxMode::Now {
                    Err(e)
                } else {
                    Ok(None)
                };
            }
        }
        let last = {
            let db = self.db()?;
            Vault::setting_inner(&db, &postbox_last_key(connection_id))?
                .and_then(|d| jiff::civil::Date::strptime("%Y-%m-%d", &d).ok())
        };
        Ok(postbox_window(last, crate::dates::today(), mode))
    }
}

/// Is `key` a comdirect login via FinTS?
fn is_comdirect_fints(key: &str) -> bool {
    ConnectorKind::fints_blz(key)
        .is_some_and(|b| super::fints_directory::COMDIRECT_BLZ.contains(&b))
}

/// Is `key` a Consorsbank login via FinTS (Online-Archiv, #1024)?
fn is_consorsbank_fints(key: &str) -> bool {
    ConnectorKind::fints_blz(key) == Some("76030080")
}

impl Vault {
    /// Stable ids (`_id<n>` of the bank's file name) of the Consorsbank
    /// archive documents already stored for this login: fetched before, or
    /// imported under the bank's file name. They are not downloaded again,
    /// which would also mark them read at the bank.
    pub(crate) fn consorsbank_known_documents(
        &self,
        connection_id: i64,
    ) -> CoreResult<std::collections::HashSet<String>> {
        let db = self.db()?;
        // from the covering index only: the PDF bytes are never read
        let mut st = db.prepare(
            "SELECT d.file_name, d.source_url FROM booking_documents d INDEXED BY booking_documents_info
              WHERE d.account_id IN (SELECT id FROM accounts WHERE connection_id = ?1)
                 OR d.source_url LIKE 'consorsbank:archive:%'",
        )?;
        let rows = st.query_map([connection_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut known = std::collections::HashSet::new();
        for row in rows {
            let (file, source) = row?;
            if let Some(id) = source.strip_prefix(super::consorsbank_archive::SOURCE_PREFIX) {
                known.insert(id.to_string());
            } else if let Some(id) = super::consorsbank_archive::stable_id(&file) {
                known.insert(id.to_string());
            }
        }
        Ok(known)
    }
}

/// The PostBox window of a sync (see [`Vault::postbox_since`]).
pub(crate) fn postbox_window(
    last: Option<jiff::civil::Date>,
    today: jiff::civil::Date,
    mode: PostboxMode,
) -> Option<String> {
    match (mode, last) {
        (PostboxMode::Off, _) => None,
        (_, None) => Some(String::new()),
        (PostboxMode::Due, Some(d)) if crate::dates::days_between(d, today) < POSTBOX_DAYS => None,
        (_, Some(d)) => Some(crate::dates::iso(d - crate::dates::days(POSTBOX_DAYS))),
    }
}

#[uniffi::export]
impl Vault {
    /// Prepares a SEPA transfer from a FinTS account. `secret` = PIN (empty =
    /// PIN stored in the vault). `demo` simulates the bank.
    pub fn begin_transfer(
        self: Arc<Self>,
        request: SepaTransferRequest,
        secret: String,
        demo: bool,
    ) -> Result<Arc<TransferSession>, CoreError> {
        use crate::connectors::fints::sepa;
        if request.amount_cents <= 0 {
            return Err(CoreError::invalid(tr("Der Betrag muss größer als 0 sein.")));
        }
        if request.amount_cents > sepa::MAX_AMOUNT_CENTS {
            return Err(CoreError::invalid(tr(
                "Der Betrag ist größer als SEPA erlaubt (999.999.999,99 €).",
            )));
        }
        if !validate_iban(&request.recipient_iban) {
            return Err(CoreError::invalid(tr("Die Empfänger-IBAN ist ungültig.")));
        }
        if let Some(b) = request
            .recipient_bic
            .as_deref()
            .filter(|b| !b.trim().is_empty())
            && !sepa::validate_bic(b)
        {
            return Err(CoreError::invalid(tr("Die BIC ist ungültig.")));
        }
        if request.recipient_name.trim().is_empty() {
            return Err(CoreError::invalid(tr("Der Empfängername fehlt.")));
        }
        if request.purpose.chars().count() > 140 {
            return Err(CoreError::invalid(tr(
                "Der Verwendungszweck darf höchstens 140 Zeichen lang sein.",
            )));
        }
        let (connection_id, iban, owner, key_str, kind, demo) =
            self.transfer_source(request.account_id, demo)?;
        let exec_date = request
            .execution_date
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .map(str::to_string);
        if request.instant && exec_date.is_some() {
            return Err(CoreError::invalid(tr(
                "Echtzeitüberweisungen können nicht terminiert werden.",
            )));
        }
        if let Some(d) = &exec_date {
            let parsed = jiff::civil::Date::strptime("%Y-%m-%d", d).map_err(|_| {
                CoreError::invalid(tr("Das Ausführungsdatum ist ungültig (JJJJ-MM-TT)."))
            })?;
            if parsed <= crate::dates::today() {
                return Err(CoreError::invalid(tr(
                    "Das Ausführungsdatum muss in der Zukunft liegen.",
                )));
            }
        }
        if (request.instant || exec_date.is_some()) && matches!(kind, ConnectorKind::Bunq) {
            return Err(CoreError::invalid(tr(
                "Echtzeit- und Terminüberweisungen sind nur für FinTS-Konten möglich.",
            )));
        }
        // Reuse the sync session set-up (settings, PIN, connector state).
        let sync = self.clone().begin_sync(connection_id, secret, demo)?;
        let ctx = sync.clone_context()?;
        let job = TransferJob {
            source_iban: iban,
            source_owner: owner,
            recipient_name: request.recipient_name.trim().to_string(),
            recipient_iban: normalize_iban(&request.recipient_iban),
            recipient_bic: request
                .recipient_bic
                .filter(|b| !b.trim().is_empty())
                .map(|b| normalize_iban(&b)),
            amount_cents: request.amount_cents,
            purpose: request.purpose.trim().to_string(),
            instant: request.instant,
            execution_date: exec_date,
            end_to_end_id: new_end_to_end_id(),
        };
        Ok(Arc::new(TransferSession::new(
            self.clone(),
            make_connector_for(kind, &key_str, demo),
            ctx,
            OrderKind::Single(job),
            request.account_id,
        )))
    }

    /// Prepares a batch transfer (Sammelüberweisung, HKCCM). Validates every
    /// payment; the first faulty one aborts with its position.
    pub fn begin_batch_transfer(
        self: Arc<Self>,
        request: BatchTransferRequest,
        secret: String,
        demo: bool,
    ) -> Result<Arc<TransferSession>, CoreError> {
        use crate::connectors::fints::sepa;
        if request.payments.is_empty() {
            return Err(CoreError::invalid(tr(
                "Die Sammelüberweisung enthält keine Zahlung.",
            )));
        }
        let mut payments = Vec::with_capacity(request.payments.len());
        for (i, p) in request.payments.iter().enumerate() {
            let payment = sepa::BatchPayment {
                name: p.name.trim().to_string(),
                iban: normalize_iban(&p.iban),
                bic: p
                    .bic
                    .as_deref()
                    .map(str::trim)
                    .filter(|b| !b.is_empty())
                    .map(normalize_iban),
                amount_cents: p.amount_cents,
                purpose: p.purpose.trim().to_string(),
                end_to_end_id: new_end_to_end_id(),
            };
            sepa::validate_batch_payment(i + 1, &payment)?;
            payments.push(payment);
        }
        let total = sepa::batch_total_cents(&payments).ok_or_else(|| {
            CoreError::invalid(tr("Die Summe der Sammelüberweisung ist zu groß."))
        })?;
        let (connection_id, iban, owner, key_str, kind, demo) =
            self.transfer_source(request.account_id, demo)?;
        if !matches!(
            kind,
            ConnectorKind::VolksbankKasselGoettingen
                | ConnectorKind::Direkt1822
                | ConnectorKind::FinTsGeneric
        ) {
            return Err(CoreError::invalid(tr(
                "Sammelüberweisungen sind nur für FinTS-Konten möglich.",
            )));
        }
        let exec_date = request
            .execution_date
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .map(str::to_string);
        if let Some(d) = &exec_date {
            let parsed = jiff::civil::Date::strptime("%Y-%m-%d", d).map_err(|_| {
                CoreError::invalid(tr("Das Ausführungsdatum ist ungültig (JJJJ-MM-TT)."))
            })?;
            if parsed <= crate::dates::today() {
                return Err(CoreError::invalid(tr(
                    "Das Ausführungsdatum muss in der Zukunft liegen.",
                )));
            }
        }
        let sync = self.clone().begin_sync(connection_id, secret, demo)?;
        let ctx = sync.clone_context()?;
        let summary = TransferJob {
            source_iban: iban.clone(),
            source_owner: owner.clone(),
            recipient_name: trf("Sammelüberweisung ({0} Zahlungen)", &[&payments.len()]),
            recipient_iban: String::new(),
            recipient_bic: None,
            amount_cents: total,
            purpose: String::new(),
            instant: false,
            execution_date: exec_date.clone(),
            end_to_end_id: new_end_to_end_id(),
        };
        let batch = BatchTransferJob {
            source_iban: iban,
            source_owner: owner,
            payments,
            batch_booking: request.batch_booking,
            execution_date: exec_date,
        };
        Ok(Arc::new(TransferSession::new(
            self.clone(),
            make_connector_for(kind, &key_str, demo),
            ctx,
            OrderKind::Batch { batch, summary },
            request.account_id,
        )))
    }

    /// Prepares creating, changing or deleting a standing order at the bank
    /// (HKCDE / HKCDN / HKCDL), confirmed with a TAN like a transfer. FinTS
    /// accounts only. Change and delete refer to a stored order by row id; the
    /// bank's order id and, for a deletion, the data to send come from the vault.
    pub fn begin_standing_order(
        self: Arc<Self>,
        request: crate::models::StandingOrderRequest,
        secret: String,
        demo: bool,
    ) -> Result<Arc<TransferSession>, CoreError> {
        use crate::models::StandingOrderAction as Action;
        let (connection_id, iban, owner, key_str, kind, demo) =
            self.transfer_source(request.account_id, demo)?;
        if !matches!(
            kind,
            ConnectorKind::VolksbankKasselGoettingen
                | ConnectorKind::Direkt1822
                | ConnectorKind::FinTsGeneric
        ) {
            return Err(CoreError::invalid(tr(
                "Daueraufträge lassen sich nur bei FinTS-Konten ändern.",
            )));
        }
        let order = match request.action {
            Action::Create | Action::Change => {
                let name = request.recipient_name.trim().to_string();
                if name.is_empty() || name.chars().count() > 70 {
                    return Err(CoreError::invalid(tr(
                        "Der Empfängername muss 1 bis 70 Zeichen lang sein.",
                    )));
                }
                let purpose = request.purpose.trim().to_string();
                if purpose.chars().count() > 140 {
                    return Err(CoreError::invalid(tr(
                        "Der Verwendungszweck darf höchstens 140 Zeichen lang sein.",
                    )));
                }
                let recipient_iban = normalize_iban(&request.recipient_iban);
                if !validate_iban(&recipient_iban) {
                    return Err(CoreError::invalid(tr("Die Empfänger-IBAN ist ungültig.")));
                }
                if request.amount_cents <= 0 {
                    return Err(CoreError::invalid(tr("Der Betrag muss größer als 0 sein.")));
                }
                if request.amount_cents > crate::connectors::fints::sepa::MAX_AMOUNT_CENTS {
                    return Err(CoreError::invalid(tr(
                        "Der Betrag ist größer als SEPA erlaubt (999.999.999,99 €).",
                    )));
                }
                let today = crate::dates::today();
                let date = |d: &str| jiff::civil::Date::strptime("%Y-%m-%d", d.trim()).ok();
                let first = date(&request.first_date).ok_or_else(|| {
                    CoreError::invalid(tr("Der erste Termin ist ungültig (JJJJ-MM-TT)."))
                })?;
                if first <= today {
                    return Err(CoreError::invalid(tr(
                        "Der erste Termin muss in der Zukunft liegen.",
                    )));
                }
                let last = match request.last_date.as_deref().map(str::trim) {
                    None | Some("") => None,
                    Some(l) => {
                        let l = date(l).ok_or_else(|| {
                            CoreError::invalid(tr("Der letzte Termin ist ungültig (JJJJ-MM-TT)."))
                        })?;
                        if l < first {
                            return Err(CoreError::invalid(tr(
                                "Der letzte Termin darf nicht vor dem ersten liegen.",
                            )));
                        }
                        Some(l.to_string())
                    }
                };
                let max_day = match request.time_unit.as_str() {
                    "M" => 31,
                    "W" => 7,
                    _ => {
                        return Err(CoreError::invalid(tr(
                            "Der Rhythmus muss Monate oder Wochen sein.",
                        )));
                    }
                };
                if !(1..=99).contains(&request.interval) {
                    return Err(CoreError::invalid(tr(
                        "Der Rhythmus muss zwischen 1 und 99 liegen.",
                    )));
                }
                if !(1..=max_day).contains(&request.exec_day) {
                    return Err(CoreError::invalid(tr("Der Ausführungstag ist ungültig.")));
                }
                if !crate::standing_orders::first_date_fits(
                    first,
                    &request.time_unit,
                    request.exec_day,
                ) {
                    return Err(CoreError::invalid(tr(
                        "Die erste Ausführung passt nicht zum Ausführungstag.",
                    )));
                }
                let order_id = if request.action == Action::Change {
                    self.stored_order(&request)?.order_id
                } else {
                    String::new()
                };
                crate::connectors::StandingOrderJob {
                    action: request.action,
                    source_iban: iban,
                    source_owner: owner,
                    order_id,
                    recipient_name: name,
                    recipient_iban,
                    recipient_bic: request
                        .recipient_bic
                        .as_deref()
                        .map(str::trim)
                        .filter(|b| !b.is_empty())
                        .map(normalize_iban),
                    amount_cents: request.amount_cents,
                    purpose,
                    first_date: first.to_string(),
                    last_date: last,
                    time_unit: request.time_unit.clone(),
                    interval: request.interval,
                    exec_day: request.exec_day,
                }
            }
            Action::Delete => {
                let o = self.stored_order(&request)?;
                if !o.can_delete {
                    return Err(CoreError::invalid(tr(
                        "Die Bank erlaubt nicht, diesen Dauerauftrag zu löschen.",
                    )));
                }
                crate::connectors::StandingOrderJob {
                    action: Action::Delete,
                    source_iban: iban,
                    source_owner: owner,
                    order_id: o.order_id,
                    recipient_name: o.recipient_name,
                    recipient_iban: o.recipient_iban,
                    recipient_bic: None,
                    amount_cents: o.amount_cents,
                    purpose: o.purpose,
                    first_date: o.first_date,
                    last_date: o.last_date,
                    time_unit: o.time_unit,
                    interval: o.interval,
                    exec_day: o.exec_day,
                }
            }
        };
        let sync = self.clone().begin_sync(connection_id, secret, demo)?;
        let ctx = sync.clone_context()?;
        Ok(Arc::new(TransferSession::new(
            self.clone(),
            make_connector_for(kind, &key_str, demo),
            ctx,
            OrderKind::Standing(order),
            request.account_id,
        )))
    }

    /// The stored order a change or deletion refers to: it must exist, belong to
    /// the account, carry the bank's order id and (for a change) be changeable.
    fn stored_order(
        &self,
        request: &crate::models::StandingOrderRequest,
    ) -> CoreResult<crate::standing_orders::StandingOrderInfo> {
        let o = request
            .order_row_id
            .map(|id| self.standing_order(id))
            .transpose()?
            .flatten()
            .filter(|o| o.account_id == request.account_id)
            .ok_or_else(|| {
                CoreError::invalid(tr(
                    "Der Dauerauftrag ist nicht mehr vorhanden. Bitte die Konten neu abrufen.",
                ))
            })?;
        if o.order_id.trim().is_empty() {
            return Err(CoreError::invalid(tr(
                "Die Bank hat für diesen Dauerauftrag keine Auftragsnummer gemeldet. Bitte in der Banking-App ändern.",
            )));
        }
        if request.action == crate::models::StandingOrderAction::Change && !o.can_change {
            return Err(CoreError::invalid(tr(
                "Die Bank erlaubt nicht, diesen Dauerauftrag zu ändern.",
            )));
        }
        Ok(o)
    }

    /// Prepares handing one file of a collection run (see
    /// `build_collection_files`) to the bank: HKDME/HKDSE and the COR1/B2B
    /// variants, confirmed with a TAN like a transfer. FinTS accounts only.
    pub fn begin_direct_debit(
        self: Arc<Self>,
        request: crate::mandates::DirectDebitRequest,
        secret: String,
        demo: bool,
    ) -> Result<Arc<TransferSession>, CoreError> {
        use crate::connectors::fints::pain008::DebitScheme;
        let plan =
            self.plan_collection(request.account_id, &request.items, &request.collection_date)?;
        // the items are one file's mandates: exactly one group, with the sequence
        // type the user saw; anything else means the mandates changed meanwhile
        let mut groups = plan.groups.into_iter();
        let group = match (groups.next(), groups.next()) {
            (Some(g), None) if g.sequence.code() == request.sequence => g,
            _ => {
                return Err(CoreError::invalid(tr(
                    "Die Mandate haben sich seit dem Erzeugen der Dateien geändert. Bitte die Dateien neu erzeugen.",
                )));
            }
        };
        let scheme = group.scheme.debit();
        if scheme == DebitScheme::B2b && !request.b2b_confirmed {
            return Err(CoreError::invalid(tr(
                "Bei Firmen-Lastschriften (B2B) muss bestätigt werden, dass die Mandate bei den Banken der Zahlungspflichtigen hinterlegt sind.",
            )));
        }
        let (connection_id, iban, _, key_str, kind, demo) =
            self.transfer_source(request.account_id, demo)?;
        if !matches!(
            kind,
            ConnectorKind::VolksbankKasselGoettingen
                | ConnectorKind::Direkt1822
                | ConnectorKind::FinTsGeneric
        ) {
            return Err(CoreError::invalid(tr(
                "Lastschriften können nur über FinTS-Konten eingereicht werden.",
            )));
        }
        let (ids, collections): (Vec<i64>, Vec<_>) = group.rows.into_iter().unzip();
        let sync = self.clone().begin_sync(connection_id, secret, demo)?;
        let ctx = sync.clone_context()?;
        let job = DebitJob {
            source_iban: iban,
            creditor_name: plan.creditor_name,
            creditor_id: plan.creditor_id,
            scheme,
            sequence: group.sequence,
            collection_date: request.collection_date,
            collections,
            batch_booking: request.batch_booking,
        };
        Ok(Arc::new(TransferSession::new(
            self.clone(),
            make_connector_for(kind, &key_str, demo),
            ctx,
            OrderKind::Debit(DebitSubmission {
                job,
                mandate_ids: ids,
            }),
            request.account_id,
        )))
    }
}

impl Vault {
    /// Debit account of a transfer: connection, IBAN, owner, connector and the
    /// gate/kind checks shared by single and batch transfers.
    #[allow(clippy::type_complexity)]
    pub(crate) fn transfer_source(
        &self,
        account_id: i64,
        demo: bool,
    ) -> CoreResult<(i64, String, String, String, ConnectorKind, bool)> {
        let (connection_id, iban, owner) = {
            let db = self.db()?;
            let row: (Option<i64>, String, String) = db
                .query_row(
                    "SELECT connection_id, iban, owner FROM accounts WHERE id = ?1 AND deleted_ms IS NULL",
                    [account_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?
                .ok_or_else(|| CoreError::NotFound {
                    what: trf("Konto {0}", &[&account_id]),
                })?;
            let cid = row
                .0
                .ok_or_else(|| CoreError::invalid(tr("Dieses Konto hat keine Bankverbindung.")))?;
            (cid, row.1, row.2)
        };
        if self.is_rah10_connection(connection_id)? {
            return Err(CoreError::invalid(tr(
                "RAH-10 unterstützt zunächst nur Kontoabrufe.",
            )));
        }
        let (key_str, login_name): (String, String) = {
            let db = self.db()?;
            db.query_row(
                "SELECT connector, login_name FROM bank_connections WHERE id = ?1",
                [connection_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?
        };
        let magic = crate::demo::is_magic_demo_login(&login_name);
        let demo = demo || magic;
        if !magic {
            crate::banking_gate::check_connector(&key_str)?;
        }
        let kind = ConnectorKind::from_str(&key_str)
            .ok_or_else(|| CoreError::invalid(format!("unknown connector {key_str}")))?;
        if !matches!(
            kind,
            ConnectorKind::VolksbankKasselGoettingen
                | ConnectorKind::Direkt1822
                | ConnectorKind::FinTsGeneric
                | ConnectorKind::Bunq
        ) {
            return Err(CoreError::invalid(tr(
                "Überweisungen sind nur für FinTS- und bunq-Konten möglich.",
            )));
        }
        if iban.is_empty() {
            return Err(CoreError::invalid(tr("Das Konto hat keine IBAN.")));
        }
        Ok((connection_id, iban, owner, key_str, kind, demo))
    }
}

impl Vault {
    /// Optimistic booking after a confirmed transfer. It disappears as soon
    /// as the bank's own booking for it arrives in a sync.
    pub(crate) fn record_pending_transfer(
        &self,
        account_id: i64,
        job: &TransferJob,
        booking_text: &str,
    ) -> CoreResult<()> {
        let db = self.db()?;
        db.execute(
            "INSERT INTO transactions(account_id, external_id, booking_date, value_date, amount_cents, currency,
                     counterparty, counterparty_iban, purpose, booking_text)
             VALUES (?1, ?2, ?3, ?3, ?4, 'EUR', ?5, ?6, ?7, ?8)",
            params![
                account_id,
                format!("pending-{}", job.end_to_end_id),
                today(),
                -job.amount_cents,
                job.recipient_name,
                job.recipient_iban,
                job.purpose,
                booking_text
            ],
        )?;
        Vault::apply_rules_inner(&db, true)?;
        Ok(())
    }
}

impl Vault {
    /// After a confirmed batch transfer: scheduled ones go to "Geplant", the
    /// rest become optimistic bookings (one for the sum or one per payment).
    pub(crate) fn record_batch_transfer(
        &self,
        account_id: i64,
        batch: &BatchTransferJob,
        summary: &TransferJob,
    ) -> CoreResult<()> {
        if batch.execution_date.is_some() {
            return self.record_batch_planned(account_id, batch, summary);
        }
        self.record_batch_pending(account_id, batch, summary, "(vorgemerkt)")
    }

    fn record_batch_planned(
        &self,
        account_id: i64,
        batch: &BatchTransferJob,
        summary: &TransferJob,
    ) -> CoreResult<()> {
        if batch.batch_booking {
            return self.record_planned_transfer(account_id, summary);
        }
        for p in &batch.payments {
            self.record_planned_transfer(account_id, &payment_job(batch, p))?;
        }
        Ok(())
    }

    pub(crate) fn record_batch_pending(
        &self,
        account_id: i64,
        batch: &BatchTransferJob,
        summary: &TransferJob,
        state: &str,
    ) -> CoreResult<()> {
        if batch.batch_booking {
            return self.record_pending_transfer(
                account_id,
                summary,
                &format!("Sammelüberweisung {state}"),
            );
        }
        for p in &batch.payments {
            self.record_pending_transfer(
                account_id,
                &payment_job(batch, p),
                &format!("Überweisung {state}"),
            )?;
        }
        Ok(())
    }
}

impl Vault {
    /// After the bank accepted a collection: shows it under "Geplant" (as
    /// incoming, one sum or one line per collection) and advances the
    /// mandates (`RCUR` next; after `FNAL` they are switched off).
    pub(crate) fn record_direct_debit(
        &self,
        account_id: i64,
        d: &DebitSubmission,
    ) -> CoreResult<()> {
        use crate::connectors::fints::pain008::SequenceType;
        let job = &d.job;
        let db = self.db()?;
        // all or nothing: a half-recorded run would offer the same mandates again
        let tx = db.unchecked_transaction()?;
        let plan = |e2e: &str, name: &str, iban: &str, cents: i64, purpose: &str| {
            Vault::insert_planned_debit(
                &tx,
                account_id,
                e2e,
                &job.collection_date,
                name,
                iban,
                cents,
                purpose,
            )
        };
        if job.batch_booking {
            let total: i64 = job.collections.iter().map(|c| c.amount_cents).sum();
            let first = job.collections.first().map(|c| c.end_to_end_id.as_str());
            plan(
                &format!("{}-SUM", first.unwrap_or("GC")),
                &trf(
                    "Lastschrift-Einzug ({0} Lastschriften)",
                    &[&job.collections.len()],
                ),
                "",
                total,
                "",
            )?;
        } else {
            for c in &job.collections {
                plan(
                    &c.end_to_end_id,
                    &c.debtor_name,
                    &c.debtor_iban,
                    c.amount_cents,
                    &c.purpose,
                )?;
            }
        }
        for id in &d.mandate_ids {
            tx.execute(
                "UPDATE mandates SET last_collected = ?2 WHERE id = ?1
                   AND (last_collected IS NULL OR last_collected < ?2)",
                params![id, job.collection_date],
            )?;
            if job.sequence == SequenceType::Final {
                // the switch-off is an edit: the stamp lets a vault merge carry it over
                tx.execute(
                    "UPDATE mandates SET active = 0, updated_ms = ?2 WHERE id = ?1",
                    params![id, crate::dates::now_ms()],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}

/// One payment of a batch as a single-transfer job (for per-payment bookings).
fn payment_job(
    batch: &BatchTransferJob,
    p: &crate::connectors::fints::sepa::BatchPayment,
) -> TransferJob {
    TransferJob {
        source_iban: batch.source_iban.clone(),
        source_owner: batch.source_owner.clone(),
        recipient_name: p.name.clone(),
        recipient_iban: p.iban.clone(),
        recipient_bic: p.bic.clone(),
        amount_cents: p.amount_cents,
        purpose: p.purpose.clone(),
        instant: false,
        execution_date: batch.execution_date.clone(),
        end_to_end_id: p.end_to_end_id.clone(),
    }
}

/// `deleted` is a deleted account of this connection with `acc`'s external
/// ref: a duplicate the user removed because the connector had not taken over
/// the imported account. Only that case is handled: a live imported account
/// (`moneymoney-db:`) with the same IBAN and currency, parked on a PSD2
/// connection that never synced. The deleted row moves aside (its key gets a
/// suffix) and the imported one takes its place; it is stamped as created now,
/// so on other devices it wins over the deletion of that key. Any other
/// deleted account keeps its key and its fate (stays deleted or is revived).
fn adopt_past_deleted_twin(
    tx: &rusqlite::Connection,
    connection_id: i64,
    deleted: i64,
    acc: &RemoteAccount,
) -> CoreResult<Option<i64>> {
    let iban = crate::iban::normalize_iban(&acc.iban);
    if iban.is_empty() {
        return Ok(None);
    }
    // Only the connection's own row under this key (not an unlinked account
    // `adopt_unlinked` found, which keeps its key).
    let own: bool = tx.query_row(
        // includes soft-deleted rows
        "SELECT connection_id IS ?1 AND external_ref = ?2 FROM accounts WHERE id = ?3",
        params![connection_id, acc.external_ref, deleted],
        |r| r.get(0),
    )?;
    if !own {
        return Ok(None);
    }
    let currency = acc.currency.trim().to_uppercase();
    let twin: Option<i64> = tx
        .query_row(
            "SELECT a.id FROM accounts a JOIN bank_connections c ON c.id = a.connection_id
              WHERE a.deleted_ms IS NULL AND a.external_ref LIKE 'moneymoney-db:%'
                AND upper(replace(a.iban, ' ', '')) = ?1
                AND (?2 = '' OR upper(a.currency) IN ('', ?2))
                AND c.id <> ?3 AND c.connector LIKE 'enablebanking%' AND c.last_sync_unix IS NULL
              ORDER BY (upper(a.currency) <> ?2), a.id LIMIT 1",
            params![iban, currency, connection_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(twin) = twin else {
        return Ok(None);
    };
    tx.execute(
        "UPDATE accounts SET external_ref = ?1 WHERE id = ?2",
        params![format!("{}#deleted-{deleted}", acc.external_ref), deleted],
    )?;
    tx.execute(
        "UPDATE accounts SET connection_id = ?1, external_ref = ?2, created_ms = ?3 WHERE id = ?4",
        params![
            connection_id,
            acc.external_ref,
            crate::dates::now_ms(),
            twin
        ],
    )?;
    Ok(Some(twin))
}

/// Links an unconnected stored account to `connection_id` when it matches the
/// remote one by account number / IBAN (ignoring spaces and leading zeros) or is
/// the single unlinked depot of the same bank. Sets `connection_id`, `external_ref`, `kind`.
fn adopt_unlinked(
    tx: &rusqlite::Connection,
    connection_id: i64,
    bank_name: &str,
    acc: &RemoteAccount,
) -> CoreResult<Option<i64>> {
    let norm = |x: &str| {
        x.chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .trim_start_matches('0')
            .to_uppercase()
    };
    let mut st = tx.prepare(
        // includes soft-deleted rows
        "SELECT id, IFNULL(iban,''), IFNULL(account_number,''), name, IFNULL(bank_name,'') FROM accounts WHERE connection_id IS NULL",
    )?;
    let rows: Vec<(i64, String, String, String, String)> = st
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<Result<_, _>>()?;
    drop(st);
    let ours = [
        norm(&acc.iban),
        norm(&acc.account_number),
        norm(&acc.external_ref),
    ];
    let mut found = rows
        .iter()
        .find(|(_, iban, num, _, _)| {
            [norm(iban), norm(num)]
                .iter()
                .any(|f| !f.is_empty() && ours.contains(f))
        })
        .map(|r| r.0);
    if found.is_none()
        && (acc.kind == AccountKind::Brokerage || acc.name.to_lowercase().contains("depot"))
    {
        let same_bank = |b: &str| b.eq_ignore_ascii_case(bank_name);
        let depots: Vec<i64> = rows
            .iter()
            .filter(|(_, _, _, name, bank)| {
                name.to_lowercase().contains("depot") && same_bank(bank)
            })
            .map(|r| r.0)
            .collect();
        if let [only] = depots.as_slice() {
            found = Some(*only);
        }
    }
    if let Some(id) = found {
        // a deleted account that stays deleted keeps its sync key
        if crate::sync_merge::fetch_keeps_account_deleted(tx, connection_id, id)? {
            return Ok(found);
        }
        tx.execute(
            "UPDATE accounts SET connection_id = ?1, external_ref = ?2, kind = ?3 WHERE id = ?4",
            params![connection_id, acc.external_ref, acc.kind.as_str(), id],
        )?;
    }
    Ok(found)
}

fn is_amex_name(s: &str) -> bool {
    let l = s.to_lowercase();
    l.contains("amex") || l.contains("american express")
}

/// `open_card_balance` of an account's live bookings; `None` without bookings.
pub(crate) fn card_open_balance(
    conn: &rusqlite::Connection,
    account_id: i64,
) -> CoreResult<Option<i64>> {
    let mut st = conn.prepare_cached(
        "SELECT booking_date, amount_cents, counterparty || ' ' || purpose || ' ' || booking_text
         FROM transactions WHERE account_id = ?1 AND deleted_ms IS NULL ORDER BY booking_date, id",
    )?;
    let rows: Vec<(String, i64, String)> = st
        .query_map([account_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    Ok(open_card_balance(&rows))
}

/// Open card balance (negative = owed) from `(date, amount_cents, text)` rows
/// sorted ascending: sum of everything after the latest settlement booking
/// (a positive KARTENABRECHNUNG / LASTSCHRIFT / AUSGLEICH / ZAHLUNG ERHALTEN
/// entry, also Amex's „Zahlung/Überweisung erhalten“). Without a settlement,
/// the sum of all bookings.
pub(crate) fn open_card_balance(rows: &[(String, i64, String)]) -> Option<i64> {
    if rows.is_empty() {
        return None;
    }
    let is_settlement = |(_, amt, text): &(String, i64, String)| {
        let t = text.to_uppercase();
        *amt > 0
            && (["KARTENABRECHNUNG", "LASTSCHRIFT", "AUSGLEICH"]
                .iter()
                .any(|k| t.contains(k))
                // „ZAHLUNG ERHALTEN“, „ZAHLUNG/ÜBERWEISUNG ERHALTEN BESTEN DANK“
                || t.contains("ZAHLUNG/ÜBERWEISUNG ERHALTEN") || t.contains("ZAHLUNG/UEBERWEISUNG ERHALTEN"))
    };
    let start = match rows.iter().rposition(is_settlement) {
        Some(i) => i + 1,
        // Billing cycles cross month boundaries and a refund is no settlement:
        // without one, everything fetched belongs to the open cycle.
        None => 0,
    };
    Some(rows[start..].iter().map(|r| r.1).sum())
}

/// The ref a single-account sync filters on (#1417). An account still under
/// a MoneyMoney ref – imported and not yet taken over, or marked by
/// "Abrufweg umstellen" (`moneymoney-db:switch:…`) – is not known to the bank
/// under it: filtering on it would drop everything and report success with 0
/// bookings. Only a sync of the whole login takes such an account over, so
/// it gets one (`None`). An archived or excluded account is refused as for a
/// single-account sync, before it would turn into a full one.
pub(super) fn single_account_ref(
    ext: String,
    skipped: impl FnOnce() -> CoreResult<Vec<String>>,
) -> CoreResult<Option<String>> {
    if !ext.starts_with("moneymoney-db:") {
        return Ok(Some(ext));
    }
    let unmarked = ext.strip_prefix(crate::vault::connections::SWITCH_MARK);
    let skipped = skipped()?;
    if skipped.contains(&ext) || unmarked.is_some_and(|u| skipped.iter().any(|s| s == u)) {
        return Err(CoreError::invalid(tr(
            "Dieses Konto ist archiviert oder vom Abruf ausgenommen.",
        )));
    }
    Ok(None)
}
