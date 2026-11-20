//! Manually configured loans (GiroCrab/GiroCrab#36), e.g. a car loan paid
//! from the Girokonto without an account of its own at the bank.
//!
//! A manual loan stores a *Stand*: the outstanding balance at the end of an
//! anchor date (`loan_terms`). Bookings of other accounts are linked to it
//! (`loan_links`), either by hand or by the loan's search expression. The
//! outstanding balance is the anchor balance minus the repayments booked after
//! the anchor date; with an interest rate, only the principal share of each
//! payment counts (interest accrues day by day, actual/365). Anything missing
//! or off is fixed by setting a new Stand, so the loan never depends on old
//! bookings still being in the vault.

use std::collections::{HashMap, HashSet};

use jiff::civil::Date;
use rusqlite::{Connection, OptionalExtension, params};

use crate::dates::parse_iso;
use crate::error::{CoreError, CoreResult};
use crate::i18n::{tr, trf};
use crate::models::*;
use crate::rules::RuleRow;
use crate::schema::has_table;
use crate::search_expr::SearchExpr;
use crate::vault::{LIVE_BOOKINGS_SQL, Vault, sql_int_list};

pub(crate) const LOAN_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS loan_terms (
    account_id           INTEGER PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
    anchor_date          TEXT    NOT NULL,           -- YYYY-MM-DD, Stand at the end of this day
    anchor_balance_cents INTEGER NOT NULL,           -- outstanding balance (positive, i64 cents)
    interest_rate_ppm    INTEGER,                    -- nominal rate p.a. in millionths (4.99 % = 49900)
    match_expr           TEXT    NOT NULL DEFAULT '' -- search expression that links bookings ('' = none)
);
CREATE TABLE IF NOT EXISTS loan_links (
    transaction_id  INTEGER PRIMARY KEY REFERENCES transactions(id) ON DELETE CASCADE,
    loan_account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    source          TEXT    NOT NULL DEFAULT 'manual'  -- manual | rule | excluded
);
CREATE INDEX IF NOT EXISTS loan_links_loan ON loan_links(loan_account_id);";

/// Highest accepted interest rate (30 % p.a.).
const MAX_RATE_PPM: i64 = 300_000;
/// Longest accepted loan name (characters).
const MAX_NAME_CHARS: usize = 100;
/// Upper bound for the remaining-term estimate (100 years of monthly payments).
const MAX_MONTHS: u32 = 1200;

/// What the user configures for a manual loan.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct LoanSetup {
    pub name: String,
    /// Original loan amount (drives the progress bar); `None` = the anchor balance.
    pub original_cents: Option<i64>,
    /// Stand date, `YYYY-MM-DD`; payments booked after this day reduce the balance.
    pub anchor_date: String,
    /// Outstanding balance at the end of `anchor_date` (positive cents).
    pub anchor_balance_cents: i64,
    /// Nominal interest rate p.a. in millionths (4.99 % = 49_900); `None` = no interest.
    pub interest_rate_ppm: Option<i64>,
    /// Search expression (rule syntax) that links bookings automatically; "" = none.
    pub match_expr: String,
}

/// Computed state of a manual loan.
#[derive(Debug, Clone, PartialEq, Default, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct LoanStatus {
    /// Outstanding balance after all counted payments (positive, never below 0).
    pub remaining_cents: i64,
    /// Date the balance refers to: the last counted payment, else the anchor date.
    pub as_of_date: String,
    /// Linked payments booked after the anchor date.
    pub payment_count: u32,
    /// Sum of those payments (positive = paid to the lender).
    pub paid_cents: i64,
    /// Part of the payments that reduced the debt.
    pub principal_cents: i64,
    /// Part of the payments that went to interest (0 without a rate).
    pub interest_cents: i64,
    /// Linked bookings on or before the anchor date (shown, not counted).
    pub before_anchor_count: u32,
    pub last_payment_cents: Option<i64>,
    pub last_payment_date: String,
    /// Monthly payments still needed at the last payment's amount; `None` when
    /// there is no payment yet or it doesn't cover the interest.
    pub months_left: Option<u32>,
}

/// A manual loan with its setup and current state.
#[derive(Debug, Clone, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ManualLoan {
    pub account_id: i64,
    pub setup: LoanSetup,
    pub status: LoanStatus,
}

/// Live preview for the loan editor (nothing stored).
#[derive(Debug, Clone, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct LoanPreview {
    pub status: LoanStatus,
    /// Bookings the expression links (newest first, at most `limit`).
    pub matches: Vec<Booking>,
    pub match_count: u32,
}

/// A booking linked to a manual loan.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct LoanLink {
    pub transaction_id: i64,
    pub loan_account_id: i64,
    /// Linked by the loan's search expression (not by hand).
    pub by_rule: bool,
}

fn compile_expr(src: &str) -> CoreResult<Option<SearchExpr>> {
    if src.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(SearchExpr::parse_supported(src)?))
}

fn validate(setup: &LoanSetup) -> CoreResult<LoanSetup> {
    let name = setup.name.trim().to_string();
    if name.is_empty() {
        return Err(CoreError::invalid(tr(
            "Bitte einen Namen für das Darlehen angeben.",
        )));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(CoreError::invalid(trf(
            "Der Name ist zu lang (höchstens {0} Zeichen).",
            &[&MAX_NAME_CHARS],
        )));
    }
    let Some(date) = parse_iso(&setup.anchor_date) else {
        return Err(CoreError::invalid(tr("Ungültiges Stichtagsdatum.")));
    };
    if date > crate::dates::today() {
        return Err(CoreError::invalid(tr(
            "Der Stichtag darf nicht in der Zukunft liegen.",
        )));
    }
    if setup.anchor_balance_cents < 0 {
        return Err(CoreError::invalid(tr(
            "Die Restschuld darf nicht negativ sein.",
        )));
    }
    if setup.original_cents.is_some_and(|c| c <= 0) {
        return Err(CoreError::invalid(tr(
            "Der Kreditbetrag muss größer als 0 sein.",
        )));
    }
    if setup
        .interest_rate_ppm
        .is_some_and(|r| !(0..=MAX_RATE_PPM).contains(&r))
    {
        return Err(CoreError::invalid(tr(
            "Der Zinssatz muss zwischen 0 und 30 % liegen.",
        )));
    }
    compile_expr(&setup.match_expr)?;
    Ok(LoanSetup {
        name,
        anchor_date: crate::dates::iso(date),
        match_expr: setup.match_expr.trim().to_string(),
        interest_rate_ppm: setup.interest_rate_ppm.filter(|r| *r > 0),
        ..setup.clone()
    })
}

/// Interest on `balance` over `days` at `ppm` p.a. (actual/365), rounded.
fn interest(balance: i64, ppm: i64, days: i64) -> i64 {
    if balance <= 0 || ppm <= 0 || days <= 0 {
        return 0;
    }
    let num = balance as i128 * ppm as i128 * days as i128;
    let den = 1_000_000i128 * 365;
    ((num + den / 2) / den) as i64
}

/// Monthly payments of `payment` still needed to clear `balance`.
fn months_left(balance: i64, payment: i64, ppm: i64) -> Option<u32> {
    if balance <= 0 {
        return Some(0);
    }
    if payment <= 0 {
        return None;
    }
    let monthly = |b: i64| -> i64 {
        let num = b as i128 * ppm as i128;
        let den = 1_000_000i128 * 12;
        ((num + den / 2) / den) as i64
    };
    if ppm > 0 && payment <= monthly(balance) {
        return None;
    }
    let mut rest = balance;
    let mut n = 0u32;
    while rest > 0 {
        if n >= MAX_MONTHS {
            return None;
        }
        rest = rest + monthly(rest) - payment;
        n += 1;
    }
    Some(n)
}

/// Pure balance computation. `bookings` are (booking_date, amount_cents) of
/// the linked bookings as seen from the paying account (debit = negative).
pub(crate) fn compute_status(setup: &LoanSetup, bookings: &[(String, i64)]) -> LoanStatus {
    let ppm = setup.interest_rate_ppm.unwrap_or(0);
    let mut events: Vec<(Date, &str, i64)> = Vec::new();
    let mut before = 0u32;
    let anchor = parse_iso(&setup.anchor_date);
    for (d, amount) in bookings {
        match (parse_iso(d), anchor) {
            (Some(date), Some(a)) if date > a => events.push((date, d.as_str(), -amount)),
            _ => before += 1,
        }
    }
    events.sort_by_key(|e| e.0);
    let mut st = LoanStatus {
        remaining_cents: setup.anchor_balance_cents,
        as_of_date: setup.anchor_date.clone(),
        before_anchor_count: before,
        ..Default::default()
    };
    let mut balance = setup.anchor_balance_cents;
    let mut last = anchor;
    for (date, raw, payment) in events {
        let days = last.map_or(0, |l| crate::dates::days_between(l, date));
        let int = interest(balance, ppm, days);
        balance = balance + int - payment;
        last = Some(date);
        st.payment_count += 1;
        st.paid_cents += payment;
        st.interest_cents += int;
        st.principal_cents += payment - int;
        st.last_payment_cents = Some(payment);
        st.last_payment_date = raw.get(..10).unwrap_or(raw).to_string();
        st.as_of_date = st.last_payment_date.clone();
    }
    st.remaining_cents = balance.max(0);
    st.months_left = st
        .last_payment_cents
        .and_then(|p| months_left(st.remaining_cents, p, ppm));
    st
}

/// Bookings of non-loan accounts. `unlinked_only` skips bookings that already
/// have a link row (except exclusions, which only block their own loan).
fn candidates(conn: &Connection, unlinked_only: bool) -> CoreResult<Vec<(RuleRow, Option<i64>)>> {
    let sql = format!(
        "SELECT {}, CASE WHEN l.source = 'excluded' THEN l.loan_account_id END
           FROM transactions t JOIN accounts a ON a.id = t.account_id
           LEFT JOIN loan_links l ON l.transaction_id = t.id
          WHERE a.kind != 'loan' AND a.deleted_ms IS NULL AND t.deleted_ms IS NULL {}",
        RuleRow::COLUMNS,
        if unlinked_only {
            "AND (l.transaction_id IS NULL OR l.source = 'excluded')"
        } else {
            ""
        }
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map([], |r| {
        Ok((RuleRow::read(r)?, r.get::<_, Option<i64>>(12)?))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn load_setup(conn: &Connection, account_id: i64) -> CoreResult<Option<LoanSetup>> {
    Ok(conn
        .query_row(
            "SELECT a.name, a.initial_balance_cents, l.anchor_date, l.anchor_balance_cents,
                    l.interest_rate_ppm, l.match_expr
               FROM loan_terms l JOIN accounts a ON a.id = l.account_id
              WHERE l.account_id = ?1 AND a.kind = 'loan' AND a.deleted_ms IS NULL",
            [account_id],
            |r| {
                Ok(LoanSetup {
                    name: r.get(0)?,
                    original_cents: r.get::<_, Option<i64>>(1)?.map(i64::abs),
                    anchor_date: r.get(2)?,
                    anchor_balance_cents: r.get(3)?,
                    interest_rate_ppm: r.get(4)?,
                    match_expr: r.get(5)?,
                })
            },
        )
        .optional()?)
}

fn require_setup(conn: &Connection, account_id: i64) -> CoreResult<LoanSetup> {
    load_setup(conn, account_id)?.ok_or_else(|| CoreError::NotFound {
        what: trf("Darlehen {0}", &[&account_id]),
    })
}

fn linked_bookings(conn: &Connection, loan_id: i64) -> CoreResult<Vec<(String, i64)>> {
    let mut st = conn.prepare(
        "SELECT t.booking_date, t.amount_cents FROM loan_links l JOIN transactions t ON t.id = l.transaction_id
          WHERE l.loan_account_id = ?1 AND l.source != 'excluded' AND t.deleted_ms IS NULL",
    )?;
    let rows = st.query_map([loan_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Writes the computed balance of a manual loan to its account row.
fn refresh(conn: &Connection, loan_id: i64) -> CoreResult<LoanStatus> {
    let setup = require_setup(conn, loan_id)?;
    let st = compute_status(&setup, &linked_bookings(conn, loan_id)?);
    conn.execute(
        "UPDATE accounts SET balance_cents = ?1, balance_date = ?2 WHERE id = ?3",
        params![-st.remaining_cents, st.as_of_date, loan_id],
    )?;
    Ok(st)
}

pub(crate) fn is_manual_loan(conn: &Connection, account_id: i64) -> CoreResult<bool> {
    if !has_table(conn, "main", "loan_terms")? {
        return Ok(false);
    }
    Ok(load_setup(conn, account_id)?.is_some())
}

/// A manual loan whose kind the user changed (e.g. to cash) is an ordinary
/// account afterwards: its terms stay (for a change back), but it neither
/// links bookings nor gets its balance from them.
fn manual_loan_ids(conn: &Connection) -> CoreResult<Vec<i64>> {
    let mut st = conn.prepare(
        "SELECT account_id FROM loan_terms
          WHERE account_id IN (SELECT id FROM accounts WHERE kind = 'loan' AND deleted_ms IS NULL) ORDER BY account_id",
    )?;
    let rows = st.query_map([], |r| r.get(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Links unlinked bookings to the first manual loan (by id) whose expression
/// matches, then refreshes every manual loan's balance. Called after every
/// import (via `apply_rules_inner`) and after loan edits.
pub(crate) fn apply_loan_rules(conn: &Connection) -> CoreResult<u32> {
    if !has_table(conn, "main", "loan_terms")? {
        return Ok(0);
    }
    let loans: Vec<(i64, SearchExpr)> = {
        let mut st = conn.prepare(
            "SELECT l.account_id, l.match_expr FROM loan_terms l JOIN accounts a ON a.id = l.account_id
              WHERE l.match_expr != '' AND a.kind = 'loan' AND a.archived_unix IS NULL AND a.deleted_ms IS NULL
              ORDER BY l.account_id",
        )?;
        let rows: Vec<(i64, String)> = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        // A stored expression that no longer parses links nothing.
        rows.into_iter()
            .filter_map(|(id, e)| compile_expr(&e).ok().flatten().map(|x| (id, x)))
            .collect()
    };
    let mut linked = 0u32;
    if !loans.is_empty() {
        for (c, excluded_from) in candidates(conn, true)? {
            let Some((loan, _)) = loans
                .iter()
                .filter(|(id, _)| Some(*id) != excluded_from)
                .find(|(_, e)| c.matches_expr(e))
            else {
                continue;
            };
            conn.execute(
                "INSERT OR REPLACE INTO loan_links(transaction_id, loan_account_id, source) VALUES (?1, ?2, 'rule')",
                params![c.id, loan],
            )?;
            linked += 1;
        }
    }
    for id in manual_loan_ids(conn)? {
        refresh(conn, id)?;
    }
    Ok(linked)
}

/// Takes over manual loans and their hand-made links from an attached vault
/// (`other`). Rule links are re-derived afterwards.
pub(crate) fn merge_attached(conn: &Connection, acc_map: &HashMap<i64, i64>) -> CoreResult<()> {
    if !has_table(conn, "other", "loan_terms")? || !has_table(conn, "other", "loan_links")? {
        return Ok(());
    }
    {
        let mut st = conn.prepare(
            "SELECT account_id, anchor_date, anchor_balance_cents, interest_rate_ppm, match_expr FROM other.loan_terms",
        )?;
        let rows: Vec<(i64, String, i64, Option<i64>, String)> = st
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<Result<_, _>>()?;
        for (oid, date, bal, rate, expr) in rows {
            if let Some(id) = acc_map.get(&oid) {
                conn.execute(
                    "INSERT OR IGNORE INTO loan_terms(account_id, anchor_date, anchor_balance_cents, interest_rate_ppm, match_expr)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![id, date, bal, rate, expr],
                )?;
            }
        }
    }
    // links of live bookings only; a deleted booking has none
    let mut st = conn.prepare(
        "SELECT t.account_id, t.external_id, t.import_fp, l.loan_account_id, l.source
           FROM other.loan_links l JOIN other.transactions t ON t.id = l.transaction_id
          WHERE l.source != 'rule' AND t.deleted_ms IS NULL",
    )?;
    let rows: Vec<(i64, String, String, i64, String)> = st
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<Result<_, _>>()?;
    for (oacc, ext, fp, oloan, source) in rows {
        let (Some(acc), Some(loan)) = (acc_map.get(&oacc), acc_map.get(&oloan)) else {
            continue;
        };
        // the booking's key first; the fingerprint only when exactly one
        // booking carries it (a pair the merge has not renamed yet)
        let by_key: Option<(i64, bool)> = conn
            .query_row(
                // includes soft-deleted rows: a deleted match stays unlinked
                "SELECT id, deleted_ms IS NULL FROM transactions WHERE account_id = ?1 AND external_id = ?2",
                params![acc, ext],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let tx: Option<i64> = match by_key {
            Some((id, live)) => live.then_some(id),
            None if fp.is_empty() => None,
            None => {
                // includes soft-deleted rows: an ambiguous or deleted match links nothing;
                // `import_fp != ''` lets SQLite use the partial index `tx_import_fp`
                let ids: Vec<(i64, bool)> = conn
                    .prepare_cached(
                        "SELECT id, deleted_ms IS NULL FROM transactions
                          WHERE account_id = ?1 AND import_fp = ?2 AND import_fp != '' LIMIT 2",
                    )?
                    .query_map(params![acc, fp], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<Result<_, _>>()?;
                match ids[..] {
                    [(id, true)] => Some(id),
                    _ => None,
                }
            }
        };
        let loan_live: bool = conn.query_row(
            // includes soft-deleted rows
            "SELECT deleted_ms IS NULL FROM accounts WHERE id = ?1",
            [loan],
            |r| r.get(0),
        )?;
        if let (Some(tx), true) = (tx, loan_live) {
            conn.execute(
                "INSERT OR IGNORE INTO loan_links(transaction_id, loan_account_id, source) VALUES (?1, ?2, ?3)",
                params![tx, loan, source],
            )?;
        }
    }
    drop(st);
    apply_loan_rules(conn)?;
    Ok(())
}

/// Recomputes manual loan balances after bookings were deleted.
pub(crate) fn refresh_all(conn: &Connection) -> CoreResult<()> {
    for id in manual_loan_ids(conn)? {
        refresh(conn, id)?;
    }
    Ok(())
}

impl Vault {
    fn store_setup(conn: &Connection, id: i64, s: &LoanSetup) -> CoreResult<()> {
        conn.execute(
            "UPDATE accounts SET name = ?1, name_ms = ?2, initial_balance_cents = ?3 WHERE id = ?4",
            params![
                s.name,
                crate::dates::now_ms(),
                s.original_cents.unwrap_or(s.anchor_balance_cents),
                id
            ],
        )?;
        conn.execute(
            "INSERT INTO loan_terms(account_id, anchor_date, anchor_balance_cents, interest_rate_ppm, match_expr)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(account_id) DO UPDATE SET anchor_date = ?2, anchor_balance_cents = ?3,
                 interest_rate_ppm = ?4, match_expr = ?5",
            params![id, s.anchor_date, s.anchor_balance_cents, s.interest_rate_ppm, s.match_expr],
        )?;
        // the expression may have changed: re-derive this loan's rule links
        conn.execute(
            "DELETE FROM loan_links WHERE loan_account_id = ?1 AND source = 'rule'",
            [id],
        )?;
        apply_loan_rules(conn)?;
        Ok(())
    }
}

/// [`Vault::create_manual_loan`] inside an open transaction (also used by the demo seed).
pub(crate) fn create_in(conn: &Connection, setup: &LoanSetup) -> CoreResult<i64> {
    let s = validate(setup)?;
    let (group, sym, order) = crate::connectors::group_for(AccountKind::Loan);
    let gid = Vault::ensure_group(conn, group, sym, order)?;
    conn.execute(
        "INSERT INTO accounts(connection_id, group_id, external_ref, name, kind, sf_symbol, sort_order, created_ms)
         VALUES (NULL, ?1, ?2, ?3, 'loan', ?4, (SELECT IFNULL(MAX(sort_order),0)+1 FROM accounts), ?5)",
        params![
            gid,
            format!("manual-loan-{}", uuid::Uuid::new_v4()),
            s.name,
            AccountKind::Loan.sf_symbol(),
            crate::dates::now_ms()
        ],
    )?;
    let id = conn.last_insert_rowid();
    crate::user_groups::place_new_account(conn, id)?;
    Vault::store_setup(conn, id, &s)?;
    Ok(id)
}

#[uniffi::export]
impl Vault {
    /// All manual loans (archived ones included) with their computed state.
    pub fn list_manual_loans(&self) -> Result<Vec<ManualLoan>, CoreError> {
        let db = self.db()?;
        let mut out = Vec::new();
        for id in manual_loan_ids(&db)? {
            let setup = require_setup(&db, id)?;
            let status = compute_status(&setup, &linked_bookings(&db, id)?);
            out.push(ManualLoan {
                account_id: id,
                setup,
                status,
            });
        }
        Ok(out)
    }

    /// Creates a loan account without a bank connection. Returns its account id.
    pub fn create_manual_loan(&self, setup: LoanSetup) -> Result<i64, CoreError> {
        let db = self.db()?;
        let tx = db.unchecked_transaction()?;
        let id = create_in(&tx, &setup)?;
        tx.commit()?;
        Ok(id)
    }

    /// Changes name, amounts, Stand, rate and expression of a manual loan.
    pub fn update_manual_loan(&self, account_id: i64, setup: LoanSetup) -> Result<(), CoreError> {
        let s = validate(&setup)?;
        let db = self.db()?;
        let tx = db.unchecked_transaction()?;
        require_setup(&tx, account_id)?;
        Self::store_setup(&tx, account_id, &s)?;
        tx.commit()?;
        Ok(())
    }

    /// Sets a new Stand (e.g. from the lender's annual statement).
    pub fn set_loan_anchor(
        &self,
        account_id: i64,
        anchor_date: String,
        anchor_balance_cents: i64,
    ) -> Result<(), CoreError> {
        let db = self.db()?;
        let current = require_setup(&db, account_id)?;
        drop(db);
        self.update_manual_loan(
            account_id,
            LoanSetup {
                anchor_date,
                anchor_balance_cents,
                ..current
            },
        )
    }

    /// What `setup` would show, for the editor's live preview. With
    /// `account_id`, that loan's hand-made links count too.
    pub fn preview_manual_loan(
        &self,
        account_id: Option<i64>,
        setup: LoanSetup,
        limit: u32,
    ) -> Result<LoanPreview, CoreError> {
        let expr = compile_expr(&setup.match_expr)?;
        let db = self.db()?;
        let mut ids = HashSet::new();
        if let Some(expr) = &expr {
            for (c, excluded_from) in candidates(&db, false)? {
                if (excluded_from.is_none() || excluded_from != account_id) && c.matches_expr(expr)
                {
                    ids.insert(c.id);
                }
            }
        }
        let mut st =
            db.prepare("SELECT transaction_id, loan_account_id, source FROM loan_links")?;
        let links: Vec<(i64, i64, String)> = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?;
        let mut taken = HashSet::new();
        let mut manual = HashSet::new();
        for (t, loan, src) in links {
            if Some(loan) == account_id {
                match src.as_str() {
                    "manual" => {
                        manual.insert(t);
                    }
                    "excluded" => {
                        taken.insert(t);
                    }
                    _ => {}
                }
            } else if src != "excluded" {
                taken.insert(t);
            }
        }
        // Status from (date, amount) of every match; only the listed ones are enriched.
        let wanted: Vec<i64> = manual
            .iter()
            .chain(ids.difference(&taken))
            .copied()
            .collect();
        let mut st = db.prepare(&format!(
            "SELECT t.id, t.booking_date, t.amount_cents
               FROM transactions t JOIN accounts a ON a.id = t.account_id
              WHERE {LIVE_BOOKINGS_SQL} AND t.id IN ({})
              ORDER BY t.booking_date DESC, t.id DESC",
            sql_int_list(&wanted)
        ))?;
        let n = limit.max(1) as usize;
        let mut shown = Vec::new();
        let mut pairs: Vec<(String, i64)> = Vec::new();
        for row in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, (r.get(1)?, r.get(2)?))))? {
            let (id, pair) = row?;
            if shown.len() < n {
                shown.push(id);
            }
            pairs.push(pair);
        }
        Ok(LoanPreview {
            status: compute_status(&setup, &pairs),
            matches: self.bookings_by_ids(&db, &shown)?,
            match_count: pairs.len() as u32,
        })
    }

    /// All bookings currently linked to a manual loan (exclusions left out).
    pub fn list_loan_links(&self) -> Result<Vec<LoanLink>, CoreError> {
        let db = self.db()?;
        let mut st = db.prepare(
            "SELECT l.transaction_id, l.loan_account_id, l.source = 'rule' FROM loan_links l
              JOIN transactions t ON t.id = l.transaction_id AND t.deleted_ms IS NULL
              JOIN accounts a ON a.id = l.loan_account_id AND a.deleted_ms IS NULL
             WHERE l.source != 'excluded'",
        )?;
        let rows = st.query_map([], |r| {
            Ok(LoanLink {
                transaction_id: r.get(0)?,
                loan_account_id: r.get(1)?,
                by_rule: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Links a booking to a manual loan by hand, or (`None`) unlinks it; an
    /// unlinked booking is not linked again by that loan's expression.
    pub fn set_booking_loan(
        &self,
        transaction_id: i64,
        loan_account_id: Option<i64>,
    ) -> Result<(), CoreError> {
        let db = self.db()?;
        let tx = db.unchecked_transaction()?;
        let kind: Option<String> = tx
            .query_row(
                "SELECT a.kind FROM transactions t JOIN accounts a ON a.id = t.account_id
                  WHERE t.id = ?1 AND t.deleted_ms IS NULL AND a.deleted_ms IS NULL",
                [transaction_id],
                |r| r.get(0),
            )
            .optional()?;
        match kind.as_deref() {
            None => {
                return Err(CoreError::NotFound {
                    what: trf("Buchung {0}", &[&transaction_id]),
                });
            }
            Some("loan") => {
                return Err(CoreError::invalid(tr(
                    "Buchungen eines Darlehenskontos lassen sich keinem anderen Darlehen zuordnen.",
                )));
            }
            _ => {}
        }
        let previous: Option<(i64, String)> = tx
            .query_row(
                "SELECT loan_account_id, source FROM loan_links WHERE transaction_id = ?1",
                [transaction_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match loan_account_id {
            Some(loan) => {
                require_setup(&tx, loan)?;
                tx.execute(
                    "INSERT OR REPLACE INTO loan_links(transaction_id, loan_account_id, source) VALUES (?1, ?2, 'manual')",
                    params![transaction_id, loan],
                )?;
                refresh(&tx, loan)?;
            }
            None => {
                if let Some((_, src)) = &previous {
                    if src != "excluded" {
                        tx.execute(
                            "UPDATE loan_links SET source = 'excluded' WHERE transaction_id = ?1",
                            [transaction_id],
                        )?;
                    }
                    // another loan's expression may claim it now
                    apply_loan_rules(&tx)?;
                }
            }
        }
        if let Some((prev, _)) = previous
            && Some(prev) != loan_account_id
        {
            refresh(&tx, prev)?;
        }
        tx.commit()?;
        Ok(())
    }
}

/// Booking id → manual loan for the loan view and flow classification.
pub(crate) fn link_map(conn: &Connection) -> CoreResult<HashMap<i64, i64>> {
    let mut st = conn.prepare(
        "SELECT transaction_id, loan_account_id FROM loan_links WHERE source != 'excluded'",
    )?;
    let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

#[cfg(test)]
mod tests;
