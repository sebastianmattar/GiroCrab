//! SEPA direct debit mandates and creditor IDs (GitHub #143).
//!
//! A mandate belongs to the account that collects (the creditor account). The
//! sequence type of the next collection follows from the mandate: a one-off
//! mandate is `OOFF`, otherwise the first collection is `FRST` and every later
//! one `RCUR`. The creditor ID is kept per account.

use rusqlite::{Connection, OptionalExtension, params};

use crate::connectors::fints::pain008::{DebitScheme, SequenceType, validate_creditor_id};
use crate::connectors::fints::sepa::{is_sepa_identifier, validate_bic};
use crate::dates::now_ms;
use crate::error::{CoreError, CoreResult};
use crate::i18n::tr;
use crate::iban::{normalize_iban, validate_iban};
use crate::vault::Vault;

mod collect;
mod csv;
mod merge;
mod pain;
mod returns;
pub use collect::{CollectionItem, DebitFile, DirectDebitRequest};
pub use csv::{MandateCsvImport, MandateCsvRow, MandateImportResult, parse_mandate_csv};
pub(crate) use merge::merge_attached;
pub use returns::{MandateBooking, return_reason};
pub(crate) use returns::{mandate_return_ack, merge_ack};

const MANDATES: &str = "
CREATE TABLE IF NOT EXISTS mandates (
    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id           INTEGER NOT NULL REFERENCES accounts(id),
    debtor_name          TEXT    NOT NULL,
    debtor_iban          TEXT    NOT NULL,
    debtor_bic           TEXT    NOT NULL DEFAULT '',
    reference            TEXT    NOT NULL,              -- Mandatsreferenz
    signed_date          TEXT    NOT NULL,              -- YYYY-MM-DD
    scheme               TEXT    NOT NULL DEFAULT 'CORE', -- CORE | COR1 | B2B
    one_off              INTEGER NOT NULL DEFAULT 0,
    default_amount_cents INTEGER NOT NULL DEFAULT 0,
    default_purpose      TEXT    NOT NULL DEFAULT '',
    note                 TEXT    NOT NULL DEFAULT '',
    active               INTEGER NOT NULL DEFAULT 1,
    last_collected       TEXT,                          -- due date of the last collection
    created_ms           INTEGER NOT NULL,
    updated_ms           INTEGER NOT NULL DEFAULT 0,    -- last edit, for the vault merge
    UNIQUE(account_id, reference)
);
-- a deleted mandate leaves a mark so that a merge does not bring it back
CREATE TABLE IF NOT EXISTS mandate_deletions (
    account_id   INTEGER NOT NULL,
    reference    TEXT    NOT NULL,
    deleted_ms   INTEGER NOT NULL,
    PRIMARY KEY (account_id, reference)
);
CREATE INDEX IF NOT EXISTS mandates_account ON mandates(account_id);
CREATE TABLE IF NOT EXISTS creditor_ids (
    account_id   INTEGER PRIMARY KEY REFERENCES accounts(id),
    creditor_id  TEXT    NOT NULL,
    updated_ms   INTEGER NOT NULL
);
";

/// Step 20: mandates and creditor IDs (guarded like steps 17/18).
pub(crate) fn mandates_tables(conn: &Connection) -> CoreResult<()> {
    conn.execute_batch(MANDATES)?;
    Ok(())
}

/// Direct debit scheme of a mandate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum MandateScheme {
    Core,
    Cor1,
    B2b,
}

impl MandateScheme {
    pub(crate) fn debit(self) -> DebitScheme {
        match self {
            MandateScheme::Core => DebitScheme::Core,
            MandateScheme::Cor1 => DebitScheme::Cor1,
            MandateScheme::B2b => DebitScheme::B2b,
        }
    }

    fn from_code(code: &str) -> Self {
        match DebitScheme::from_code(code) {
            Some(DebitScheme::Cor1) => MandateScheme::Cor1,
            Some(DebitScheme::B2b) => MandateScheme::B2b,
            _ => MandateScheme::Core,
        }
    }
}

/// What the user enters for a mandate.
#[derive(Debug, Clone, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MandateInput {
    pub account_id: i64,
    pub debtor_name: String,
    pub debtor_iban: String,
    pub debtor_bic: String,
    pub reference: String,
    /// `YYYY-MM-DD`
    pub signed_date: String,
    pub scheme: MandateScheme,
    pub one_off: bool,
    /// 0 = none.
    pub default_amount_cents: i64,
    pub default_purpose: String,
    pub note: String,
    pub active: bool,
}

#[derive(Debug, Clone, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Mandate {
    pub id: i64,
    pub account_id: i64,
    pub debtor_name: String,
    pub debtor_iban: String,
    pub debtor_bic: String,
    pub reference: String,
    pub signed_date: String,
    pub scheme: MandateScheme,
    pub one_off: bool,
    pub default_amount_cents: i64,
    pub default_purpose: String,
    pub note: String,
    pub active: bool,
    /// Due date of the last collection, if any.
    pub last_collected: Option<String>,
    /// Sequence type the next collection gets (`FRST`, `RCUR`, `OOFF`).
    pub next_sequence: String,
}

/// Sequence type of the next collection under a mandate.
pub fn next_sequence(one_off: bool, last_collected: Option<&str>) -> SequenceType {
    if one_off {
        SequenceType::OneOff
    } else if last_collected.is_some_and(|d| !d.is_empty()) {
        SequenceType::Recurring
    } else {
        SequenceType::First
    }
}

fn scheme_code(s: MandateScheme) -> &'static str {
    s.debit().code()
}

fn check(input: &MandateInput) -> CoreResult<()> {
    if input.debtor_name.trim().is_empty() {
        return Err(CoreError::invalid(tr(
            "Der Name des Zahlungspflichtigen fehlt.",
        )));
    }
    if !validate_iban(&input.debtor_iban) {
        return Err(CoreError::invalid(tr(
            "Die IBAN des Zahlungspflichtigen ist ungültig.",
        )));
    }
    if !input.debtor_bic.trim().is_empty() && !validate_bic(&input.debtor_bic) {
        return Err(CoreError::invalid(tr("Die BIC ist ungültig.")));
    }
    let r = input.reference.trim();
    if r.is_empty() || r.chars().count() > 35 {
        return Err(CoreError::invalid(tr(
            "Die Mandatsreferenz fehlt oder ist länger als 35 Zeichen.",
        )));
    }
    if !is_sepa_identifier(r) {
        return Err(CoreError::invalid(tr(
            "Die Mandatsreferenz enthält Zeichen, die SEPA nicht erlaubt (nur Buchstaben ohne Umlaute, Ziffern und / - ? : ( ) . , ' + Leerzeichen).",
        )));
    }
    if jiff::civil::Date::strptime("%Y-%m-%d", &input.signed_date).is_err() {
        return Err(CoreError::invalid(tr(
            "Das Datum der Mandatsunterschrift fehlt.",
        )));
    }
    if input.default_amount_cents < 0 {
        return Err(CoreError::invalid(tr("Der Betrag muss größer als 0 sein.")));
    }
    if input.default_amount_cents > crate::connectors::fints::sepa::MAX_AMOUNT_CENTS {
        return Err(CoreError::invalid(tr(
            "Der Betrag ist größer als SEPA erlaubt (999.999.999,99 €).",
        )));
    }
    Ok(())
}

const COLUMNS: &str =
    "id, account_id, debtor_name, debtor_iban, debtor_bic, reference, signed_date,
    scheme, one_off, default_amount_cents, default_purpose, note, active, last_collected";

fn row(r: &rusqlite::Row) -> rusqlite::Result<Mandate> {
    let one_off: i64 = r.get(8)?;
    let last: Option<String> = r.get(13)?;
    let scheme: String = r.get(7)?;
    Ok(Mandate {
        id: r.get(0)?,
        account_id: r.get(1)?,
        debtor_name: r.get(2)?,
        debtor_iban: r.get(3)?,
        debtor_bic: r.get(4)?,
        reference: r.get(5)?,
        signed_date: r.get(6)?,
        scheme: MandateScheme::from_code(&scheme),
        one_off: one_off != 0,
        default_amount_cents: r.get(9)?,
        default_purpose: r.get(10)?,
        note: r.get(11)?,
        active: r.get::<_, i64>(12)? != 0,
        next_sequence: next_sequence(one_off != 0, last.as_deref())
            .code()
            .to_string(),
        last_collected: last,
    })
}

impl Vault {
    pub(crate) fn mandate(&self, id: i64) -> CoreResult<Option<Mandate>> {
        let db = self.db()?;
        Ok(db
            .query_row(
                &format!("SELECT {COLUMNS} FROM mandates WHERE id = ?1"),
                [id],
                row,
            )
            .optional()?)
    }
}

#[uniffi::export]
impl Vault {
    /// Mandates, active first, then by debtor. `account_id` = one creditor
    /// account, `None` = all.
    pub fn list_mandates(&self, account_id: Option<i64>) -> Result<Vec<Mandate>, CoreError> {
        let db = self.db()?;
        let mut stmt = db.prepare(&format!(
            "SELECT {COLUMNS} FROM mandates WHERE (?1 IS NULL OR account_id = ?1)
             ORDER BY active DESC, debtor_name COLLATE NOCASE, id"
        ))?;
        let rows = stmt.query_map([account_id], row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Number of active mandates over all accounts (sidebar badge): one `COUNT(*)`.
    pub fn count_active_mandates(&self) -> Result<u32, CoreError> {
        let db = self.db()?;
        let n: i64 = db.query_row(
            "SELECT count(*) FROM mandates m JOIN accounts a ON a.id = m.account_id
             WHERE a.deleted_ms IS NULL AND m.active = 1",
            [],
            |r| r.get(0),
        )?;
        Ok(u32::try_from(n).unwrap_or(u32::MAX))
    }

    /// Creates (`id` = `None`) or changes a mandate. The reference must be
    /// unique per creditor account.
    pub fn save_mandate(&self, id: Option<i64>, input: MandateInput) -> Result<Mandate, CoreError> {
        let new_id = {
            let db = self.db()?;
            store_mandate(&db, id, &input)?
        };
        self.mandate(new_id)?
            .ok_or_else(|| CoreError::invalid(tr("Das Mandat gibt es nicht mehr.")))
    }

    /// Saves imported rows under the creditor account `account_id`: a row whose
    /// reference exists already updates that mandate (and keeps its last
    /// collection), the others are created. Rows with errors are skipped.
    pub fn import_mandate_rows(
        &self,
        account_id: i64,
        rows: Vec<MandateCsvRow>,
    ) -> Result<MandateImportResult, CoreError> {
        let mut result = MandateImportResult::default();
        let db = self.db()?;
        // one transaction: thousands of members import in one commit
        let tx = db.unchecked_transaction()?;
        let mut known = mandate_index(&tx, account_id)?;
        for r in rows {
            if let Some(e) = r.errors.first() {
                result.failed.push(format!("{}: {e}", r.line));
                continue;
            }
            let reference = r.reference.trim().to_string();
            let existing = known.get(&reference).map(|(id, _, _)| *id);
            // a failing row must not poison the transaction: it stops at its own check
            match store_mandate(&tx, existing, &r.to_input(account_id)) {
                Ok(id) => {
                    if existing.is_some() {
                        result.updated += 1;
                    } else {
                        result.created += 1;
                    }
                    known.insert(reference, (id, String::new(), r.active));
                }
                Err(e) => result.failed.push(format!("{}: {e}", r.line)),
            }
        }
        tx.commit()?;
        Ok(result)
    }

    /// All mandates of the account as CSV text, in the layout the import reads.
    pub fn export_mandates_csv(&self, account_id: i64) -> Result<String, CoreError> {
        Ok(csv::mandates_to_csv(&self.list_mandates(Some(account_id))?))
    }

    /// Removes a mandate for good. Collections already submitted stay in the
    /// bookings; the mandate reference remains in their text.
    pub fn delete_mandate(&self, id: i64) -> Result<(), CoreError> {
        let db = self.db()?;
        let key: Option<(i64, String)> = db
            .query_row(
                "SELECT account_id, reference FROM mandates WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        db.execute("DELETE FROM mandates WHERE id = ?1", [id])?;
        if let Some((account_id, reference)) = key {
            merge::mark_deleted(&db, account_id, &reference, now_ms())?;
        }
        Ok(())
    }

    /// Creditor ID of an account, "" if none was set.
    pub fn creditor_id(&self, account_id: i64) -> Result<String, CoreError> {
        let db = self.db()?;
        Ok(db
            .query_row(
                "SELECT creditor_id FROM creditor_ids WHERE account_id = ?1",
                [account_id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or_default())
    }

    /// Sets the creditor ID of an account; "" removes it. A wrong check digit
    /// is refused.
    pub fn set_creditor_id(&self, account_id: i64, creditor_id: String) -> Result<(), CoreError> {
        let id = creditor_id.trim().to_ascii_uppercase();
        let db = self.db()?;
        if id.is_empty() {
            // kept as an empty value with a stamp: a merge then sees the removal
            db.execute(
                "UPDATE creditor_ids SET creditor_id = '', updated_ms = ?2 WHERE account_id = ?1",
                params![account_id, now_ms()],
            )?;
            return Ok(());
        }
        if !validate_creditor_id(&id) {
            return Err(CoreError::invalid(tr("Die Gläubiger-ID ist ungültig.")));
        }
        db.execute(
            "INSERT INTO creditor_ids(account_id, creditor_id, updated_ms) VALUES (?1, ?2, ?3)
             ON CONFLICT(account_id) DO UPDATE SET creditor_id = ?2, updated_ms = ?3",
            params![account_id, id, now_ms()],
        )?;
        Ok(())
    }

    /// The creditor ID set most recently on any account, to prefill the field
    /// for another account ("" if none).
    pub fn suggested_creditor_id(&self) -> Result<String, CoreError> {
        let db = self.db()?;
        Ok(db
            .query_row(
                "SELECT creditor_id FROM creditor_ids WHERE creditor_id <> '' ORDER BY updated_ms DESC, account_id LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or_default())
    }
}

/// Validates and writes one mandate on `db`; returns its id. Shared by the
/// single save and the bulk imports (which run many of these in one transaction).
pub(crate) fn store_mandate(
    db: &Connection,
    id: Option<i64>,
    input: &MandateInput,
) -> CoreResult<i64> {
    check(input)?;
    let iban = normalize_iban(&input.debtor_iban);
    let bic = normalize_iban(&input.debtor_bic);
    let reference = input.reference.trim().to_string();
    let clash: Option<i64> = db
        .query_row(
            "SELECT id FROM mandates WHERE account_id = ?1 AND reference = ?2",
            params![input.account_id, reference],
            |r| r.get(0),
        )
        .optional()?;
    if clash.is_some_and(|c| Some(c) != id) {
        return Err(CoreError::invalid(tr(
            "Diese Mandatsreferenz gibt es für dieses Konto schon.",
        )));
    }
    let now = now_ms();
    Ok(match id {
        Some(id) => {
            // a changed reference or account is a deletion of the old key for the merge
            let old: Option<(i64, String)> = db
                .query_row(
                    "SELECT account_id, reference FROM mandates WHERE id = ?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let n = db.execute(
                "UPDATE mandates SET account_id=?2, debtor_name=?3, debtor_iban=?4, debtor_bic=?5,
                    reference=?6, signed_date=?7, scheme=?8, one_off=?9, default_amount_cents=?10,
                    default_purpose=?11, note=?12, active=?13, updated_ms=?14 WHERE id = ?1",
                params![
                    id,
                    input.account_id,
                    input.debtor_name.trim(),
                    iban,
                    bic,
                    reference,
                    input.signed_date,
                    scheme_code(input.scheme),
                    input.one_off,
                    input.default_amount_cents,
                    input.default_purpose.trim(),
                    input.note.trim(),
                    input.active,
                    now
                ],
            )?;
            if n == 0 {
                return Err(CoreError::invalid(tr("Das Mandat gibt es nicht mehr.")));
            }
            if let Some((acc, old_ref)) = old
                && (acc != input.account_id || old_ref != reference)
            {
                merge::mark_deleted(db, acc, &old_ref, now)?;
            }
            merge::unmark_deleted(db, input.account_id, &reference)?;
            id
        }
        None => {
            db.execute(
                "INSERT INTO mandates(account_id, debtor_name, debtor_iban, debtor_bic, reference,
                    signed_date, scheme, one_off, default_amount_cents, default_purpose, note,
                    active, created_ms, updated_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13)",
                params![
                    input.account_id,
                    input.debtor_name.trim(),
                    iban,
                    bic,
                    reference,
                    input.signed_date,
                    scheme_code(input.scheme),
                    input.one_off,
                    input.default_amount_cents,
                    input.default_purpose.trim(),
                    input.note.trim(),
                    input.active,
                    now
                ],
            )?;
            merge::unmark_deleted(db, input.account_id, &reference)?;
            db.last_insert_rowid()
        }
    })
}

/// (id, note, active) of the mandates of one account by reference: the
/// bulk imports look rows up here instead of listing all mandates per row.
pub(crate) fn mandate_index(
    db: &Connection,
    account_id: i64,
) -> CoreResult<std::collections::HashMap<String, (i64, String, bool)>> {
    let mut q =
        db.prepare("SELECT reference, id, note, active FROM mandates WHERE account_id = ?1")?;
    let rows = q.query_map([account_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            (r.get(1)?, r.get(2)?, r.get::<_, i64>(3)? != 0),
        ))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

#[cfg(test)]
mod tests;
