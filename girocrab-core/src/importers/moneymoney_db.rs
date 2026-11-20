//! MoneyMoney migration straight from MoneyMoney's own SQLCipher database.
//!
//! The file is opened **read-only** with the MoneyMoney password (SQLCipher
//! defaults, fallback `cipher_compatibility = 3` for older versions); nothing is
//! ever written to it. Only explicit, harmless columns are selected – login
//! secrets (`banks.credential`, keys, BPD/UPD, `accounts.cred` …) are never
//! read, and neither is the `pins` table.
//!
//! Bookings go through the same pipeline as the CSV/XML import
//! ([`moneymoney_import::merge_rows`]): fingerprint de-duplication, category
//! tree, notes. Accounts are recognised again by IBAN or by the stored
//! MoneyMoney account id, so a second run changes nothing.
//!
//! Balance: MoneyMoney keeps the balance it displays in `accounts.balances`
//! (a length-prefixed text blob such as `11:4094.320000,3:EUR,`); that value is
//! used whenever present (it is the only source for depots, which have no
//! bookings). Without it: `balance = initial balance + Σ booked bookings`,
//! where the initial balance is the newest `balances.initial_balance` row of
//! the account/currency (then only bookings from its date on are added),
//! falling back to `accounts.initial_balance` (all bookings). Only bookings in
//! the account currency count. Pending
//! bookings (`booked = 0`) are neither imported nor counted – GiroCrab has no
//! "vorgemerkt" state and the bank delivers them again once they are booked.
//!
//! Depots: the positions in `securities` (ISIN, quantity, price, cost basis,
//! value) are copied into `holdings`, replacing earlier positions of the same
//! account on every run.
//!
//! Statements and bank documents: the `statements` table only lists them
//! (account, bank folder, file name, date, SHA-256); the PDFs are plain files
//! in `Statements/<bank folder>/` next to the database. So the import takes
//! either MoneyMoney's data folder (database and PDFs are found in it) or a
//! single database file, e.g. a backup, plus optionally a folder with the
//! PDFs. A file is looked up by its original path, then by its name, then by
//! its SHA-256 anywhere in that folder – a renamed or re-sorted manual backup
//! still matches, and a wrong file never does. Files are read one at a time.
//!
//! Transfer templates: rows of type 12 in `payments`, see [`templates`]; the
//! blobs of sent orders in that table are never read.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, params};
use zeroize::Zeroizing;

use super::moneymoney_import::{
    KnownKeys, MmRow, MmSplit, Stats, find_or_create_category, merge_rows, with_known_parts,
};
use crate::asset_classification::{AssetClass, ClassificationValues, InstrumentKind};
use crate::connectors::bank_directory::{Psd2Bank, match_psd2_bank, psd2_catalog};
use crate::connectors::group_for;
use crate::dates::now_ms;
use crate::depot_values::observations::{self, ObservedValue};
use crate::error::{CoreError, CoreResult};
use crate::i18n::{tr, trf};
use crate::iban::{normalize_iban, validate_iban};
use crate::models::{AccountKind, ConnectorKind, Holding};
use crate::secret::SecretString;
use crate::vault::Vault;

mod templates;
use templates::SrcTemplate;

/// `accounts.external_ref` prefix of accounts created by this importer.
const EXT_PREFIX: &str = "moneymoney-db:";

/// Bit of `accounts.refresh` that MoneyMoney sets for a closed account
/// (found with a real database: ticking and unticking the checkbox flips only it).
const MM_CLOSED: i64 = 16;

/// Vault setting (this device only, not merged): ids of imported accounts whose
/// archive state nobody chose, so the next import gives them MoneyMoney's. The
/// #998 repair (schema step 34) reactivates accounts the import of v0.8.5
/// archived wrongly, and some of them are closed in MoneyMoney after all. Until
/// then a sync's takeover prefers the other accounts (#816).
pub(crate) const ARCHIVE_UNKNOWN_KEY: &str = "moneymoney_db.archive_unknown";

/// The accounts in [`ARCHIVE_UNKNOWN_KEY`].
pub(crate) fn archive_unknown(conn: &Connection) -> CoreResult<Vec<i64>> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            [ARCHIVE_UNKNOWN_KEY],
            |r| r.get(0),
        )
        .optional()?;
    Ok(value
        .unwrap_or_default()
        .split(',')
        .filter_map(|id| id.trim().parse().ok())
        .collect())
}

/// Adds `ids` to [`ARCHIVE_UNKNOWN_KEY`].
pub(crate) fn note_archive_unknown(conn: &Connection, ids: &[i64]) -> CoreResult<()> {
    let mut all = archive_unknown(conn)?;
    all.extend(
        ids.iter()
            .filter(|id| !all.contains(id))
            .collect::<Vec<_>>(),
    );
    store_archive_unknown(conn, &all)
}

fn store_archive_unknown(conn: &Connection, ids: &[i64]) -> CoreResult<()> {
    if ids.is_empty() {
        conn.execute("DELETE FROM settings WHERE key = ?1", [ARCHIVE_UNKNOWN_KEY])?;
        return Ok(());
    }
    let value = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
    conn.execute(
        "INSERT INTO settings(key, value, is_secret) VALUES (?1, ?2, 0)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![ARCHIVE_UNKNOWN_KEY, value],
    )?;
    Ok(())
}

/// Takes `id` out of [`ARCHIVE_UNKNOWN_KEY`]; `true` if it was in there.
fn take_archive_unknown(conn: &Connection, id: i64) -> CoreResult<bool> {
    let mut ids = archive_unknown(conn)?;
    let Some(i) = ids.iter().position(|x| *x == id) else {
        return Ok(false);
    };
    ids.remove(i);
    store_archive_unknown(conn, &ids)?;
    Ok(true)
}

/// Archives an account closed in MoneyMoney (offline, stays readable). Time and
/// stamp come from the same second like `set_account_archived`, so the #998
/// repair never takes it for a wrong one ([`crate::schema::ARCHIVE_STAMP_GAP_MS`]).
fn archive_closed(conn: &Connection, account_id: i64) -> CoreResult<()> {
    let now = now_ms();
    conn.execute(
        "UPDATE accounts SET archived_unix = ?1, archived_ms = ?2 WHERE id = ?3 AND archived_unix IS NULL",
        params![now / 1000, now, account_id],
    )?;
    Ok(())
}

// ------------------------------------------------------------------ FFI types

/// One MoneyMoney account as found in the database.
#[derive(Debug, Clone, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MoneyMoneyDbAccount {
    pub mm_id: i64,
    pub name: String,
    pub iban: String,
    pub account_number: String,
    pub currency: String,
    pub bank_code: String,
    pub bank_name: String,
    pub owner: String,
    /// MoneyMoney sidebar group (taken over with `import_groups`); nested
    /// groups as a path, e.g. "Immobilien › ETW Neckarsulm".
    pub group_name: String,
    pub mm_bank_id: Option<i64>,
    pub is_portfolio: bool,
    /// Best guess; the user can override it per account.
    pub suggested_kind: AccountKind,
    /// GiroCrab account with the same IBAN (or imported from this MoneyMoney
    /// account before) – the suggested merge target.
    pub existing_account_id: Option<i64>,
    pub existing_account_name: Option<String>,
    /// Booked bookings that would be imported (a split booking once, #877).
    pub booking_count: u32,
    /// Pending (vorgemerkte) bookings – not imported.
    pub pending_count: u32,
    /// Securities positions (depots) that would be imported.
    pub holding_count: u32,
    /// Values of the depot history (total and positions per day) that would be imported.
    #[uniffi(default = 0)]
    pub value_history_count: u32,
    pub notes_count: u32,
    pub first_date: Option<String>,
    pub last_date: Option<String>,
    /// Statements / bank documents MoneyMoney lists for this account.
    pub statement_count: u32,
    /// Balance GiroCrab will show (see module docs for the assumption).
    pub computed_balance_cents: i64,
    /// Where the starting value of the balance came from (German, for the UI).
    pub balance_source: String,
}

/// One bank login in MoneyMoney. Secrets are never part of this record.
#[derive(Debug, Clone, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MoneyMoneyDbBank {
    pub mm_id: i64,
    pub bank_code: String,
    pub name: String,
    pub login_name: String,
    pub customer_id: String,
    /// GiroCrab connector for this bank, `None` = not supported (skipped).
    pub connector: Option<ConnectorKind>,
    pub existing_connection_id: Option<i64>,
    pub account_count: u32,
    /// Every way GiroCrab can connect to this bank, best first: FinTS, then
    /// the bank's own API, then Enable Banking (from the PSD2 catalogue; the
    /// app may add one from the live list, `match_psd2_bank_by_name`). The
    /// first entry is the default. Empty = not supported.
    pub connection_options: Vec<MoneyMoneyDbConnectionOption>,
}

/// How a bank login is connected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum MoneyMoneyDbConnectionVia {
    FinTs,
    /// The bank's own API (comdirect, bunq, PayPal, Trade Republic, …).
    OwnApi,
    /// PSD2 through the user's Enable Banking application.
    EnableBanking,
}

/// One way to connect a bank login.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MoneyMoneyDbConnectionOption {
    pub via: MoneyMoneyDbConnectionVia,
    pub connector: ConnectorKind,
    /// Connector key as stored (`fints:<blz>`, `comdirect`,
    /// `enablebanking:<CC>:<bank>`).
    pub key: String,
    /// The Enable Banking bank (only for `EnableBanking`).
    pub psd2_bank: Option<Psd2Bank>,
}

impl MoneyMoneyDbConnectionOption {
    fn direct(kind: ConnectorKind, key: String) -> Self {
        let via = match kind {
            ConnectorKind::FinTsGeneric
            | ConnectorKind::VolksbankKasselGoettingen
            | ConnectorKind::Direkt1822 => MoneyMoneyDbConnectionVia::FinTs,
            ConnectorKind::EnableBanking => MoneyMoneyDbConnectionVia::EnableBanking,
            _ => MoneyMoneyDbConnectionVia::OwnApi,
        };
        Self {
            via,
            connector: kind,
            key,
            psd2_bank: None,
        }
    }

    fn enable_banking(bank: Psd2Bank) -> Self {
        Self {
            via: MoneyMoneyDbConnectionVia::EnableBanking,
            connector: ConnectorKind::EnableBanking,
            key: ConnectorKind::enable_banking_key(&bank.country, &bank.aspsp),
            psd2_bank: Some(bank),
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MoneyMoneyDbPreview {
    pub accounts: Vec<MoneyMoneyDbAccount>,
    pub banks: Vec<MoneyMoneyDbBank>,
    /// Category paths ("Haushalt › Lebensmittel").
    pub category_paths: Vec<String>,
    pub total_bookings: u32,
    pub pending_bookings: u32,
    pub notes_count: u32,
    /// Parts of split bookings imported as bookings of their own (#877): their
    /// sum differs from the split booking's, a part has another currency, or the
    /// split booking itself is not imported (pending).
    pub split_parts: u32,
    pub date_range_start: Option<String>,
    pub date_range_end: Option<String>,
    /// The vault has no bookings yet, so MoneyMoney's categories replace
    /// GiroCrab's starter categories (and their rules) instead of being added.
    pub replaces_starter_categories: bool,
    /// The database file that was read (inside the folder, if one was given).
    pub database_path: String,
    /// Statements / bank documents (PDF) listed in the database.
    pub statement_count: u32,
    /// Folder the PDFs are taken from: the one passed in, else `Statements`
    /// in the MoneyMoney folder. `None` = no folder, documents are left out.
    pub statements_dir: Option<String>,
    /// Of `statement_count`, files found in `statements_dir` …
    pub statements_found: u32,
    /// … and their total size in bytes.
    pub statements_bytes: u64,
    /// Transfer templates GiroCrab can take over (recipient and valid IBAN,
    /// euro).
    pub transfer_template_count: u32,
}

#[derive(Debug, Clone, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MoneyMoneyDbAccountChoice {
    pub mm_id: i64,
    pub import: bool,
    pub kind: AccountKind,
    /// `None` = new account (or the one created by an earlier run).
    pub target_account_id: Option<i64>,
}

#[derive(Debug, Clone, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MoneyMoneyDbImportOptions {
    /// Accounts not listed here are not imported.
    pub accounts: Vec<MoneyMoneyDbAccountChoice>,
    pub import_categories: bool,
    pub import_notes: bool,
    pub create_connections: bool,
    /// Put the accounts into GiroCrab groups named like their MoneyMoney
    /// groups (created if missing, nested and ordered like in MoneyMoney).
    /// Accounts the user already placed keep their group (or "Ohne
    /// Gruppe"); groups that already exist stay where the user put them.
    pub import_groups: bool,
    /// Copy statements / bank documents (PDF) into the document archive.
    pub import_statements: bool,
    /// Folder with the PDFs, for a database without MoneyMoney's folder
    /// around it (backup). `None` = `Statements` in the MoneyMoney folder.
    pub statements_dir: Option<String>,
    /// How to connect each bank login (only with `create_connections`). A
    /// bank without an entry gets its default (FinTS / own API; never
    /// Enable Banking unasked).
    pub bank_choices: Vec<MoneyMoneyDbBankChoice>,
    /// Take over MoneyMoney's transfer templates (recipient, IBAN, BIC,
    /// amount, purpose). A template that already exists is left alone.
    pub import_transfer_templates: bool,
}

/// The connection chosen for one MoneyMoney bank login.
#[derive(Debug, Clone, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MoneyMoneyDbBankChoice {
    pub mm_bank_id: i64,
    /// `None` = don't set up a connection for this login.
    pub connection: Option<MoneyMoneyDbConnectionOption>,
}

/// An Enable Banking connection the import set up, with the accounts that
/// must be linked to the user's Enable Banking application before the
/// first consent (restricted applications only see linked accounts).
#[derive(Debug, Clone, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MoneyMoneyDbPsd2Connection {
    pub connection_id: i64,
    pub bank_name: String,
    /// `false`: the connection existed already (an earlier import or set up
    /// by hand).
    pub created: bool,
    pub accounts: Vec<MoneyMoneyDbPsd2Account>,
}

#[derive(Debug, Clone, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MoneyMoneyDbPsd2Account {
    pub name: String,
    pub iban: String,
    pub account_number: String,
}

/// Check of one imported account after the import: does GiroCrab now hold
/// what MoneyMoney showed? Read back from the vault, independent of the
/// import's own counting.
#[derive(Debug, Clone, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MoneyMoneyDbAccountCheck {
    pub mm_id: i64,
    pub account_id: i64,
    pub name: String,
    pub currency: String,
    /// Booked bookings MoneyMoney has for the account (a split booking once,
    /// #877) – what the import had to bring over.
    pub mm_bookings: u32,
    /// Of those, found in the GiroCrab account.
    pub found_bookings: u32,
    /// Of those, found but deleted in GiroCrab earlier (they stay deleted).
    pub deleted_bookings: u32,
    /// Bookings in the GiroCrab account that MoneyMoney doesn't have (e.g.
    /// fetched from the bank before) – informational.
    pub other_bookings: u32,
    /// Balance as MoneyMoney shows it (or computed, see `mm_balance_shown`).
    pub mm_balance_cents: i64,
    /// `mm_balance_cents` is MoneyMoney's own displayed balance; `false` =
    /// computed from opening balance + bookings.
    pub mm_balance_shown: bool,
    pub girocrab_balance_cents: i64,
    /// The account is synced with a bank in GiroCrab, which keeps the
    /// bank's balance – a difference is expected then.
    pub balance_from_bank: bool,
    /// Every MoneyMoney booking is there and the balances agree (or the
    /// balance comes from the bank).
    pub ok: bool,
}

#[derive(Debug, Clone, Default, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MoneyMoneyDbImportResult {
    pub accounts_created: u32,
    pub accounts_merged: u32,
    /// GiroCrab groups created from MoneyMoney groups.
    pub groups_created: u32,
    pub bookings_new: u32,
    pub bookings_duplicates: u32,
    /// Existing bookings that received a category and/or note.
    pub bookings_enriched: u32,
    pub pending_skipped: u32,
    pub notes_imported: u32,
    pub categories_created: u32,
    pub connections_created: u32,
    pub connections_existing: u32,
    pub banks_skipped_unsupported: u32,
    /// Names of those bank logins (MoneyMoney's bank name), same order as
    /// in the database.
    pub banks_skipped_names: Vec<String>,
    /// PDF statements / documents newly stored in the document archive.
    pub documents_imported: u32,
    /// Already in the archive from an earlier run.
    pub documents_existing: u32,
    /// Listed in the database, but no file found (or no folder given).
    pub documents_missing: u32,
    /// File found, but its content differs from the database's checksum
    /// or it is not a PDF: not imported.
    pub documents_mismatched: u32,
    /// Larger than the per-file limit: not imported.
    pub documents_too_large: u32,
    /// PDFs in the folder that the database doesn't list (not imported:
    /// they belong to no account).
    pub documents_unlisted: u32,
    /// Securities positions stored for depot accounts (replaced per account).
    pub holdings_imported: u32,
    /// Values of the depot history taken over (total and positions per day, #578).
    #[uniffi(default = 0)]
    pub value_history_imported: u32,
    /// Category rules taken over as search-expression rules.
    pub rules_created: u32,
    /// Of those, switched off: unsupported fields, account restriction or
    /// invalid syntax.
    pub rules_disabled: u32,
    /// Transfer templates newly created.
    pub transfer_templates_created: u32,
    /// Already in GiroCrab (same recipient, IBAN, amount and purpose).
    pub transfer_templates_existing: u32,
    /// Not taken over: no recipient, invalid IBAN or not in euro.
    pub transfer_templates_skipped: u32,
    /// One check per imported account, in import order.
    pub account_checks: Vec<MoneyMoneyDbAccountCheck>,
    /// Enable Banking connections set up for banks without own connector.
    pub psd2_connections: Vec<MoneyMoneyDbPsd2Connection>,
}

/// Where a running import is, for a progress bar.
#[derive(Debug, Clone, Default, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MoneyMoneyDbImportProgress {
    /// 0.0 ..= 1.0 over the whole import (bookings and documents weighted).
    pub fraction: f64,
    /// Account being imported, 1-based; 0 = still reading the database,
    /// `account_count + 1` = finishing (bank logins, groups).
    pub account_index: u32,
    pub account_count: u32,
    /// Name of the account being imported ("" before and after the accounts).
    pub account_name: String,
    pub bookings_done: u32,
    pub bookings_total: u32,
    /// Statements / bank documents handled so far (imported or skipped).
    pub documents_done: u32,
    pub documents_total: u32,
}

/// Receives the progress of `import_moneymoney_database_with_progress`.
/// Called on the importing thread, often (once per document).
#[uniffi::export(with_foreign)]
pub trait MoneyMoneyImportObserver: Send + Sync {
    /// Returns `false` to cancel: the import stops and nothing is stored.
    fn progress(&self, progress: MoneyMoneyDbImportProgress) -> bool;
}

/// One document counts as much as this many bookings (file read + checksum).
const DOCUMENT_WEIGHT: u64 = 25;

/// Tells the observer where the import is; turns its "cancel" into an error.
struct Reporter<'a> {
    observer: Option<&'a dyn MoneyMoneyImportObserver>,
    state: MoneyMoneyDbImportProgress,
}

impl Reporter<'_> {
    fn emit(&mut self) -> CoreResult<()> {
        let Some(observer) = self.observer else {
            return Ok(());
        };
        let p = &mut self.state;
        let total = p.bookings_total as u64 + DOCUMENT_WEIGHT * p.documents_total as u64;
        let done = p.bookings_done as u64 + DOCUMENT_WEIGHT * p.documents_done as u64;
        p.fraction = if total == 0 {
            0.0
        } else {
            (done as f64 / total as f64).min(1.0)
        };
        if observer.progress(p.clone()) {
            Ok(())
        } else {
            Err(cancelled())
        }
    }
}

fn cancelled() -> CoreError {
    CoreError::InvalidInput {
        message: tr("Import abgebrochen – es wurde nichts übernommen.").into(),
    }
}

// ------------------------------------------------------------------ source model

#[derive(Debug, Clone, Default)]
struct SrcAccount {
    id: i64,
    bank_key: Option<i64>,
    account: String,
    bankcode: String,
    portfolio: bool,
    iban: String,
    bic: String,
    currency: String,
    name: String,
    owner: String,
    initial_balance: Option<f64>,
    group_key: Option<i64>,
    /// Balance MoneyMoney displays (`accounts.balances` blob): (cents, currency).
    shown_balance: Option<(i64, String)>,
    balance_ts: Option<i64>,
    /// "Account is closed" in MoneyMoney's account settings: bit 16 of
    /// `accounts.refresh` (the low bits are its refresh settings). MoneyMoney
    /// keeps no date for it. Not `cancel_tsp`, a cancelled refresh (#998).
    closed: bool,
}

#[derive(Debug, Clone, Default)]
struct SrcBank {
    id: i64,
    bankcode: String,
    username: String,
    customer: String,
    name: String,
    /// MoneyMoney's `banks.server` column: the FinTS/HBCI endpoint URL that
    /// MoneyMoney itself talks to. Empty for non-FinTS logins (PSD2, Trade
    /// Republic, PayPal, …) or when the column is missing in older schemas.
    server: String,
}

#[derive(Debug, Clone, Default)]
struct SrcTx {
    id: i64,
    account_key: i64,
    cp_account: String,
    cp_bankcode: String,
    amount: Option<f64>,
    currency: String,
    ts: Option<i64>,
    value_ts: Option<i64>,
    eref: String,
    mref: String,
    cred: String,
    comment: String,
    booked: bool,
    booking_text: String,
    purpose: String,
    name: String,
    checkmark: bool,
    split_key: Option<i64>,
    category_key: Option<i64>,
}

/// Statement / bank document listed in the MoneyMoney database (`statements`).
/// The PDF itself is a file: `Statements/<directory>/<filename>`.
#[derive(Debug, Clone, Default)]
struct SrcDoc {
    account_key: i64,
    directory: String,
    filename: String,
    created_ts: Option<i64>,
    /// SHA-256 of the file, lower-case hex ("" if MoneyMoney stored none).
    digest: String,
}

struct SrcHolding {
    holding: Holding,
    classification: ClassificationValues,
}

/// Correlated locally with MoneyMoney's named AppleScript exports (2.5.4).
/// These categories mix exposure and wrapper: ETF/fund do not imply equities,
/// and crypto does not prove direct coin ownership. Sonstige/unknown stay unset.
fn moneymoney_classification(value: Option<i64>) -> ClassificationValues {
    let mut result = ClassificationValues::default();
    match value {
        Some(1) => result.asset_class = Some(AssetClass::Equity),
        Some(2) => result.instrument = Some(InstrumentKind::Fund),
        Some(3) => result.asset_class = Some(AssetClass::Bond),
        Some(4) => result.asset_class = Some(AssetClass::RealEstate),
        Some(5) => result.asset_class = Some(AssetClass::Commodity),
        Some(6) => result.instrument = Some(InstrumentKind::Etf),
        Some(7) => result.asset_class = Some(AssetClass::Crypto),
        _ => {}
    }
    result
}

/// Categories as paths of names, outermost group first.
#[derive(Default)]
struct SrcCategories {
    /// category rowid -> path (uncategorised / invalid entries are absent)
    by_id: HashMap<i64, Vec<String>>,
    /// all category paths in MoneyMoney order
    order: Vec<Vec<String>>,
    /// (category or category-group path, `#RRGGBB`) – groups first
    colors: Vec<(Vec<String>, String)>,
    /// (category path, rule expression, restricted to certain accounts), in
    /// MoneyMoney order; only categories with a non-blank rule
    rules: Vec<(Vec<String>, String, bool)>,
}

#[derive(Default)]
struct Source {
    docs: Vec<SrcDoc>,
    accounts: Vec<SrcAccount>,
    banks: Vec<SrcBank>,
    /// rowid -> (name, parent group rowid or 0, sort)
    account_groups: HashMap<i64, (String, i64, i64)>,
    categories: SrcCategories,
    txs: Vec<SrcTx>,
    /// newest `balances` row per (account, currency): (timestamp, initial balance)
    balances: HashMap<(i64, String), (i64, f64)>,
    /// securities positions per MoneyMoney account id (depots)
    holdings: HashMap<i64, Vec<SrcHolding>>,
    /// transfer templates (not deleted), in MoneyMoney order
    templates: Vec<SrcTemplate>,
    /// value history of depots per MoneyMoney account id (`portfolio_chart`, #578)
    value_history: HashMap<i64, Vec<ObservedValue>>,
}

// ------------------------------------------------------------------ helpers

fn vtext(v: Value) -> String {
    match v {
        Value::Text(s) => s.trim().to_string(),
        Value::Integer(i) => i.to_string(),
        Value::Real(f) => f.to_string(),
        Value::Null | Value::Blob(_) => String::new(),
    }
}

fn vint(v: Value) -> Option<i64> {
    match v {
        Value::Integer(i) => Some(i),
        Value::Real(f) if f.is_finite() => Some(f as i64),
        Value::Text(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn vreal(v: Value) -> Option<f64> {
    match v {
        Value::Integer(i) => Some(i as f64),
        Value::Real(f) => Some(f),
        Value::Text(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn get(r: &Row, i: usize) -> rusqlite::Result<Value> {
    r.get::<_, Value>(i)
}

/// MoneyMoney stores amounts as REAL. Converted via the shortest decimal
/// representation, rounded half away from zero (0.285 → 29 cents, not 28).
pub(crate) fn real_to_cents(x: f64) -> Option<i64> {
    if !x.is_finite() {
        return None;
    }
    let s = format!("{}", x.abs());
    let (int, frac) = s.split_once('.').unwrap_or((s.as_str(), ""));
    let d: Vec<i64> = frac
        .bytes()
        .chain(std::iter::repeat(b'0'))
        .take(3)
        .map(|b| (b - b'0') as i64)
        .collect();
    let mut cents = int
        .parse::<i64>()
        .ok()?
        .checked_mul(100)?
        .checked_add(d[0] * 10 + d[1])?;
    if d[2] >= 5 {
        cents += 1;
    }
    Some(if x < 0.0 { -cents } else { cents })
}

/// MoneyMoney dates are Unix seconds at 12:00 UTC → `YYYY-MM-DD` (UTC).
pub(crate) fn unix_to_date(ts: i64) -> Option<String> {
    jiff::Timestamp::from_second(ts)
        .ok()
        .map(|t| crate::dates::iso(crate::dates::utc_date(t)))
}

/// GiroCrab's colour for categories nobody has given one.
const DEFAULT_CATEGORY_COLOR: &str = "#8E8E93";

/// MoneyMoney colour (`#rrggbb`, also `#rgb` / `#rrggbbaa`, optionally
/// quoted) → `#RRGGBB`; `None` when empty or not a colour.
fn mm_color(s: &str) -> Option<String> {
    let hex = s
        .trim()
        .trim_matches(['\'', '"'])
        .trim()
        .strip_prefix('#')?;
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let rgb: String = match hex.len() {
        3 => hex.chars().flat_map(|c| [c, c]).collect(),
        6 | 8 => hex[..6].to_string(),
        _ => return None,
    };
    Some(format!("#{}", rgb.to_ascii_uppercase()))
}

/// Gives imported categories MoneyMoney's colours – only where the category
/// still has GiroCrab's default grey, so colours chosen in GiroCrab stay.
/// Paths missing in the tree (e.g. empty category groups) are skipped.
fn apply_category_colors(
    conn: &Connection,
    src: &Source,
    cache: &HashMap<String, i64>,
) -> CoreResult<()> {
    // the colour is a field edit: stamp it, so a sync merge keeps it
    // ("newest edit wins") instead of reverting it to another device's grey
    let now = now_ms();
    for (path, color) in &src.categories.colors {
        let key: String = path
            .iter()
            .map(|p| format!("\u{1}{}", p.to_lowercase()))
            .collect();
        if let Some(id) = cache.get(&key) {
            conn.execute(
                "UPDATE categories SET color_hex = ?1, color_ms = ?4
                 WHERE id = ?2 AND upper(color_hex) = ?3 AND deleted_ms IS NULL",
                params![color, id, DEFAULT_CATEGORY_COLOR, now],
            )?;
        }
    }
    Ok(())
}

/// `11:4094.320000,3:EUR,` (length-prefixed fields) → (cents, currency).
fn parse_balances_blob(v: Value) -> Option<(i64, String)> {
    let text = match v {
        Value::Text(t) => t,
        Value::Blob(b) => crate::text::decode_bank_bytes(&b),
        _ => return None,
    };
    let mut fields = text
        .split(',')
        .filter(|f| !f.is_empty())
        .map(|f| f.split_once(':').map(|x| x.1).unwrap_or(f).trim());
    let amount: f64 = fields.next()?.parse().ok()?;
    let currency = fields.next().unwrap_or("").to_uppercase();
    Some((real_to_cents(amount)?, currency))
}

/// Account number / bank code for comparisons: alphanumerics only, upper
/// case, leading zeros of purely numeric values dropped.
fn norm_account_id(s: &str) -> String {
    let s: String = s
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_uppercase())
        .collect();
    if s.bytes().all(|b| b.is_ascii_digit()) {
        s.trim_start_matches('0').to_string()
    } else {
        s
    }
}

/// (account number, BLZ) of a German IBAN, normalised like [`norm_account_id`].
fn de_iban_parts(iban: &str) -> Option<(String, String)> {
    let iban = normalize_iban(iban);
    if iban.len() != 22 || !iban.starts_with("DE") {
        return None;
    }
    let (blz, kto) = (iban.get(4..12)?, iban.get(12..22)?);
    Some((norm_account_id(kto), norm_account_id(blz)))
}

fn looks_like_bic(s: &str) -> bool {
    let s = s.trim();
    (s.len() == 8 || s.len() == 11)
        && s.chars().all(|c| c.is_ascii_alphanumeric())
        && s.chars().take(6).all(|c| c.is_ascii_alphabetic())
}

/// Low 32 bits of FNV-1a 64 as hex.
fn fnv(s: &str) -> String {
    format!("{:08x}", crate::fnv::fnv1a64(s) as u32)
}

fn ext_ref(a: &SrcAccount) -> String {
    format!(
        "{EXT_PREFIX}{}:{}",
        a.id,
        fnv(&format!("{}|{}", a.bankcode, a.account))
    )
}

fn has_any(hay: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| hay.contains(n))
}

/// Suggested GiroCrab kind. MoneyMoney's `type` codes are undocumented, so the
/// guess uses `portfolio` and keywords in the account / bank name.
fn suggest_kind(a: &SrcAccount, bank_name: &str) -> AccountKind {
    let name = a.name.to_lowercase();
    let both = format!("{name} {}", bank_name.to_lowercase());
    if a.portfolio {
        AccountKind::Brokerage
    } else if has_any(
        &both,
        &[
            "kreditkarte",
            "credit card",
            "creditcard",
            "visa",
            "mastercard",
            "american express",
            "amex",
        ],
    ) {
        AccountKind::CreditCard
    } else if has_any(&both, &["paypal"]) {
        AccountKind::PaymentService
    } else if has_any(
        &name,
        &[
            "tagesgeld",
            "sparkonto",
            "sparbuch",
            "festgeld",
            "geldmarkt",
            "savings",
            "extra-konto",
        ],
    ) {
        AccountKind::Savings
    } else if has_any(
        &name,
        &["darlehen", "baufinanzierung", "hypothek", "kredit", "loan"],
    ) {
        AccountKind::Loan
    } else if has_any(
        &name,
        &["bargeld", "kasse", "portemonnaie", "geldbörse", "cash"],
    ) {
        AccountKind::Cash
    } else if has_any(&name, &["depot", "wertpapier"]) {
        AccountKind::Brokerage
    } else {
        AccountKind::Checking
    }
}

/// GiroCrab connector for a MoneyMoney bank login: bank code first, name for
/// institutions without a German bank code.
#[cfg(test)]
fn connector_for(bankcode: &str, name: &str) -> Option<ConnectorKind> {
    connector_key_for(bankcode, name).map(|(k, _)| k)
}

/// Like `connector_for`, plus the stored connector key (`fints:<blz>` for
/// FinTS banks that have a preset, e.g. Postbank or Sparda).
fn connector_key_for(bankcode: &str, name: &str) -> Option<(ConnectorKind, String)> {
    let kind = connector_for_inner(bankcode, name)?;
    let code: String = bankcode.chars().filter(|c| c.is_ascii_digit()).collect();
    let key = if kind == ConnectorKind::FinTsGeneric {
        ConnectorKind::fints_key(&code)
    } else {
        kind.as_str().to_string()
    };
    Some((kind, key))
}

/// GiroCrab connector for a MoneyMoney bank login, in this order of
/// preference:
///  1. FinTS – the bank code is in GiroCrab's FinTS directory (incl. the
///     dedicated FinTS connectors Volksbank Kassel Göttingen, 1822direkt);
///  2. the bank's own API, where GiroCrab has a connector (comdirect – not in
///     the FinTS directory on purpose –, bunq, PayPal, Trade Republic, …);
///  3. otherwise `None`: the preview then suggests Enable Banking (PSD2)
///     from the catalogue (`MoneyMoneyDbBank::psd2_bank`).
fn connector_for_inner(bankcode: &str, name: &str) -> Option<ConnectorKind> {
    let code: String = bankcode.chars().filter(|c| c.is_ascii_digit()).collect();
    match code.as_str() {
        "52090000" => return Some(ConnectorKind::VolksbankKasselGoettingen),
        "50050201" => return Some(ConnectorKind::Direkt1822),
        c if crate::connectors::fints_directory::COMDIRECT_BLZ.contains(&c) => {
            return Some(ConnectorKind::Comdirect);
        }
        // bunq's German branch code; GiroCrab talks to the bunq Public API.
        BUNQ_DE_BLZ => return Some(ConnectorKind::Bunq),
        c if c.len() == 8 && crate::connectors::fints_presets::known_fints_blz(c) => {
            return Some(ConnectorKind::FinTsGeneric);
        }
        _ => {}
    }
    // Brands MoneyMoney files under a pseudo bank code (Trade Republic: 10012345).
    let n = name.to_lowercase();
    if n.contains("paypal") {
        Some(ConnectorKind::PayPal)
    } else if n.contains("trade republic") {
        Some(ConnectorKind::TradeRepublic)
    } else if n.contains("american express") || n.contains("amex") {
        Some(ConnectorKind::AmericanExpress)
    } else if n.contains("amazon") {
        Some(ConnectorKind::AmazonVisa)
    } else if n.contains("comdirect") {
        Some(ConnectorKind::Comdirect)
    } else if n.contains("bunq") || is_bunq_bic(bankcode) {
        Some(ConnectorKind::Bunq)
    } else {
        None // incl. any other German bank with an 8-digit code
    }
}

/// bunq's German bank code (BUNQDE82).
const BUNQ_DE_BLZ: &str = "37019000";

/// Open Bank S.A. (Santander, issuer of the Amazon Visa via Zinia): German
/// bank code 50034200 (OPENDEFF), or the login's name.
fn is_openbank(bankcode: &str, name: &str) -> bool {
    let code: String = bankcode.chars().filter(|c| c.is_ascii_digit()).collect();
    let n = name.to_lowercase();
    code == "50034200" || n.contains("openbank") || n.contains("open bank") || n.contains("zinia")
}

/// Every bunq branch BIC starts with `BUNQ` (BUNQNL2A, BUNQDE82, BUNQFRP2,
/// BUNQESM2, BUNQITM2, …).
fn is_bunq_bic(s: &str) -> bool {
    let s = s.trim();
    (s.len() == 8 || s.len() == 11) && s.get(..4).is_some_and(|p| p.eq_ignore_ascii_case("BUNQ"))
}

/// Whether a MoneyMoney account is held at bunq, from its BIC, its bank code
/// or its IBAN: Dutch IBANs carry the bank's BIC prefix (`NLkk BUNQ …`),
/// German ones the bank code (`DEkk 37019000 …`). For other countries the
/// IBAN holds a numeric national code, so the BIC decides there.
fn is_bunq_account(bic: &str, iban: &str, bankcode: &str) -> bool {
    if is_bunq_bic(bic) || is_bunq_bic(bankcode) {
        return true;
    }
    let code: String = bankcode.chars().filter(|c| c.is_ascii_digit()).collect();
    if code == BUNQ_DE_BLZ {
        return true;
    }
    let iban = normalize_iban(iban);
    match iban.get(..2) {
        Some("NL") => iban.get(4..8) == Some("BUNQ"),
        Some("DE") => iban.get(4..12) == Some(BUNQ_DE_BLZ),
        _ => false,
    }
}

fn wrong_password() -> CoreError {
    CoreError::Authentication {
        message: tr("Falsches Passwort oder keine unterstützte Datenbank.").into(),
    }
}

/// MoneyMoney holds a lock on its database while it runs; reading then fails
/// with `SQLITE_BUSY`, which is not a wrong password (#201).
fn locked_by_moneymoney() -> CoreError {
    CoreError::Authentication {
        message: tr("Die Datenbank ist noch in einem anderen Programm geöffnet und gesperrt. Beenden Sie dieses Programm vollständig und versuchen Sie es erneut.").into(),
    }
}

/// How long to wait for MoneyMoney's lock before reporting it.
const LOCK_WAIT: std::time::Duration = std::time::Duration::from_millis(500);

/// Maps a failure while unlocking/probing the source: only "not a database"
/// (wrong key or no SQLite file) is a wrong password; a lock is reported as
/// such, anything else as what it is.
fn open_failure(e: rusqlite::Error) -> CoreError {
    match e.sqlite_error_code() {
        Some(rusqlite::ErrorCode::NotADatabase) | None => wrong_password(),
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => {
            locked_by_moneymoney()
        }
        Some(_) => CoreError::Database {
            message: e.to_string(),
        },
    }
}

/// MoneyMoney encrypts its database even when no database password is set
/// (with a key of its own that GiroCrab cannot know). Such a file can only
/// be read after the user sets a password in MoneyMoney.
fn encrypted_without_password() -> CoreError {
    CoreError::Authentication {
        message: tr("Die Datenbank ist mit einem programmeigenen Schlüssel verschlüsselt, weil kein Datenbank-Passwort festgelegt ist. Legen Sie in dem Programm, das die Datenbank erstellt hat (nicht in GiroCrab), ein Passwort fest, beenden Sie es und geben Sie das Passwort dann im Feld „Datenbank-Passwort“ dieses Dialogs ein.").into(),
    }
}

/// True if the file starts with the plain SQLite header, i.e. is not
/// encrypted (SQLCipher files start with a random salt instead).
fn is_plain_sqlite(path: &str) -> bool {
    use std::io::Read;
    let mut head = [0u8; 16];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok_and(|_| &head == b"SQLite format 3\0")
}

// ------------------------------------------------------------------ reading

/// Opens the MoneyMoney database read-only and unlocks it. The password is
/// only placed in a zeroized `PRAGMA key` statement and never logged.
fn open_source(path: &str, password: &SecretString) -> CoreResult<Connection> {
    if !std::path::Path::new(path).is_file() {
        return Err(CoreError::NotFound {
            what: tr("Datenbankdatei").into(),
        });
    }
    // Older MoneyMoney databases can be unencrypted: such a file is opened
    // without a key, whatever was typed into the password field. An
    // encrypted file without a password is MoneyMoney's own key (no database
    // password set in MoneyMoney) – say so instead of "wrong password".
    let plain = is_plain_sqlite(path);
    if password.is_empty() && !plain {
        return Err(encrypted_without_password());
    }
    if plain {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(open_failure)?;
        let _ = conn.busy_timeout(LOCK_WAIT);
        return conn
            .query_row("SELECT count(*) FROM sqlite_master", [], |r| {
                r.get::<_, i64>(0)
            })
            .map(|_| conn)
            .map_err(open_failure);
    }
    let mut pragma = Zeroizing::new(String::from("PRAGMA key = '"));
    for c in password.expose().chars() {
        if c == '\'' {
            pragma.push('\'');
        }
        pragma.push(c);
    }
    pragma.push_str("';");
    let mut failure = wrong_password();
    for compat in [None, Some(3)] {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(open_failure)?;
        let _ = conn.busy_timeout(LOCK_WAIT);
        // Errors of the key statement are swallowed: they must never carry the SQL.
        if conn.execute_batch(&pragma).is_err() {
            continue;
        }
        let _ = conn.query_row("PRAGMA cipher_log_level = NONE;", [], |_| Ok(()));
        // Not in unit tests, see `Vault::apply_key`.
        if !cfg!(test) {
            let _ = conn.query_row("PRAGMA cipher_memory_security = ON;", [], |_| Ok(()));
        }
        if let Some(v) = compat
            && conn
                .execute_batch(&format!("PRAGMA cipher_compatibility = {v};"))
                .is_err()
        {
            continue;
        }
        match conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
            r.get::<_, i64>(0)
        }) {
            Ok(_) => return Ok(conn),
            // MoneyMoney holds the file: another cipher setting would only
            // wait for the same lock again
            Err(e)
                if matches!(
                    e.sqlite_error_code(),
                    Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
                ) =>
            {
                return Err(locked_by_moneymoney());
            }
            Err(e) => failure = open_failure(e),
        }
    }
    Err(failure)
}

fn objects(conn: &Connection) -> CoreResult<HashSet<String>> {
    let mut st =
        conn.prepare("SELECT lower(name) FROM sqlite_master WHERE type IN ('table', 'view')")?;
    let names = st
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<HashSet<_>, _>>()?;
    Ok(names)
}

/// `SELECT <explicit columns> FROM <table>`; columns missing in older
/// MoneyMoney versions read as NULL. Never `SELECT *`.
fn select(conn: &Connection, table: &str, cols: &[&str], tail: &str) -> CoreResult<String> {
    let have: HashSet<String> = {
        let mut st = conn.prepare("SELECT lower(name) FROM pragma_table_info(?1)")?;

        st.query_map([table], |r| r.get::<_, String>(0))?
            .collect::<Result<_, _>>()?
    };
    let list = cols
        .iter()
        .map(|c| {
            if *c == "rowid" || have.contains(*c) {
                format!("\"{c}\"")
            } else {
                "NULL".to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    Ok(format!("SELECT {list} FROM \"{table}\" {tail}"))
}

/// The daily value history of depots (`portfolio_chart`, #578) as MoneyMoney stores it: per
/// position (`securities_key` = its `securities` row, sold ones included) and the depot total
/// (key 0), one row per day at noon. Duplicates (two rows of one ISIN) and zeros are kept; the
/// derivation resolves them (`depot_values::observations::day_values`).
fn read_value_history(
    conn: &Connection,
    currencies: &HashMap<i64, String>,
) -> CoreResult<HashMap<i64, Vec<ObservedValue>>> {
    // rowid -> (account, position key, name)
    let mut positions: HashMap<i64, (i64, String, String)> = HashMap::new();
    {
        let sql = select(
            conn,
            "securities",
            &["rowid", "account_key", "ident", "isin"],
            "",
        )?;
        let mut st = conn.prepare(&sql)?;
        let rows = st.query_map([], |r| {
            Ok((
                vint(get(r, 0)?),
                vint(get(r, 1)?),
                vtext(get(r, 2)?),
                vtext(get(r, 3)?),
            ))
        })?;
        for row in rows {
            let (Some(id), Some(acc), ident, isin) = row? else {
                continue;
            };
            let name = crate::text::clean_bank_text(ident.trim());
            if let Some(key) = observations::position_key(&isin, &name) {
                positions.insert(id, (acc, key, name));
            }
        }
    }
    let sql = select(
        conn,
        "portfolio_chart",
        &[
            "account_key",
            "currency",
            "timestamp",
            "balance",
            "securities_key",
        ],
        "ORDER BY account_key, timestamp",
    )?;
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map([], |r| {
        Ok((
            vint(get(r, 0)?),
            vtext(get(r, 1)?).trim().to_uppercase(),
            vint(get(r, 2)?),
            vreal(get(r, 3)?),
            vint(get(r, 4)?).unwrap_or(0),
        ))
    })?;
    let mut out: HashMap<i64, Vec<ObservedValue>> = HashMap::new();
    for row in rows {
        let (Some(acc), currency, Some(ts), Some(balance), key) = row? else {
            continue;
        };
        let (Some(cents), Some(day)) = (real_to_cents(balance), unix_to_date(ts)) else {
            continue;
        };
        let currency = if currency.is_empty() {
            currencies
                .get(&acc)
                .cloned()
                .unwrap_or_else(|| "EUR".into())
        } else {
            currency
        };
        let (kind, position, source_ref, name) = if key == 0 {
            (
                observations::KIND_TOTAL,
                String::new(),
                String::new(),
                String::new(),
            )
        } else {
            match positions.get(&key) {
                Some((a, position, name)) if *a == acc => (
                    observations::KIND_POSITION,
                    position.clone(),
                    key.to_string(),
                    name.clone(),
                ),
                _ => continue,
            }
        };
        out.entry(acc).or_default().push(ObservedValue {
            kind,
            position,
            source_ref,
            name,
            currency,
            observed_ms: ts.saturating_mul(1000),
            value_date: day,
            value_cents: cents,
        });
    }
    Ok(out)
}

/// Reads everything the importer needs (never the `pins` table).
fn read_source(path: &str, password: &SecretString) -> CoreResult<Source> {
    let conn = open_source(path, password)?;
    let have = objects(&conn)?;
    if !["accounts", "transactions", "categories"]
        .iter()
        .all(|t| have.contains(*t))
    {
        return Err(wrong_password());
    }
    let mut src = Source {
        accounts: read_accounts(&conn)?,
        ..Default::default()
    };
    if have.contains("securities") {
        src.holdings = read_holdings(&conn)?;
    }
    if have.contains("portfolio_chart") && have.contains("securities") {
        let currencies = src
            .accounts
            .iter()
            .map(|a| (a.id, a.currency.clone()))
            .collect();
        src.value_history = read_value_history(&conn, &currencies)?;
    }
    if have.contains("banks") {
        src.banks = read_banks(&conn)?;
    }
    if have.contains("account_groups") {
        src.account_groups = read_account_groups(&conn)?;
    }
    src.categories = read_categories(&conn, &have)?;
    src.txs = read_transactions(&conn)?;
    if have.contains("balances") {
        src.balances = read_balances(&conn)?;
    }
    src.docs = read_documents(&conn, &have)?;
    src.templates = templates::read_templates(&conn, &have)?;
    Ok(src)
}

/// Accounts – never cred / userdata / transientdata / web_ops.
fn read_accounts(conn: &Connection) -> CoreResult<Vec<SrcAccount>> {
    let sql = select(
        conn,
        "accounts",
        &[
            "rowid",
            "bank_key",
            "account",
            "bankcode",
            "portfolio",
            "iban",
            "currency",
            "name",
            "owner",
            "initial_balance",
            "group_key",
            "balances",
            "balance_tsp",
            "bic",
            "refresh",
        ],
        "ORDER BY sort, rowid",
    )?;
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map([], |r| {
        Ok(SrcAccount {
            id: vint(get(r, 0)?).unwrap_or_default(),
            bank_key: vint(get(r, 1)?).filter(|k| *k > 0),
            account: vtext(get(r, 2)?),
            bankcode: vtext(get(r, 3)?),
            portfolio: vint(get(r, 4)?).unwrap_or(0) != 0,
            iban: normalize_iban(&vtext(get(r, 5)?)),
            currency: {
                let c = vtext(get(r, 6)?).to_uppercase();
                if c.is_empty() { "EUR".into() } else { c }
            },
            name: vtext(get(r, 7)?),
            owner: vtext(get(r, 8)?),
            initial_balance: vreal(get(r, 9)?),
            group_key: vint(get(r, 10)?).filter(|k| *k > 0),
            shown_balance: parse_balances_blob(get(r, 11)?),
            balance_ts: vint(get(r, 12)?).filter(|t| *t > 0),
            bic: vtext(get(r, 13)?),
            closed: vint(get(r, 14)?).is_some_and(|f| f & MM_CLOSED != 0),
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Securities positions of depots per account (no secrets in this table).
fn read_holdings(conn: &Connection) -> CoreResult<HashMap<i64, Vec<SrcHolding>>> {
    let sql = select(
        conn,
        "securities",
        &[
            "account_key",
            "ident",
            "isin",
            "wpn",
            "quantity",
            "amount",
            "purchase",
            "price",
            "deleted",
            "asset_class",
        ],
        "ORDER BY amount DESC, rowid",
    )?;
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map([], |r| {
        Ok((
            vint(get(r, 0)?),
            vtext(get(r, 1)?),
            vtext(get(r, 2)?),
            vtext(get(r, 3)?),
            vreal(get(r, 4)?),
            vreal(get(r, 5)?),
            vreal(get(r, 6)?),
            vreal(get(r, 7)?),
            vint(get(r, 8)?).unwrap_or(0) != 0,
            vint(get(r, 9)?),
        ))
    })?;
    let mut holdings: HashMap<i64, Vec<SrcHolding>> = HashMap::new();
    for row in rows {
        let (acc, ident, isin, wpn, quantity, amount, purchase, price, deleted, asset_class) = row?;
        let (Some(acc), false) = (acc, deleted) else {
            continue;
        };
        let name = crate::text::clean_bank_text(ident.trim());
        if name.is_empty() && isin.trim().is_empty() {
            continue;
        }
        holdings.entry(acc).or_default().push(SrcHolding {
            holding: Holding {
                isin: isin.trim().to_uppercase(),
                wkn: wpn.trim().to_uppercase(),
                name,
                quantity: quantity.unwrap_or(0.0),
                current_price_cents: price.and_then(real_to_cents).unwrap_or(0),
                purchase_price_cents: purchase.and_then(real_to_cents).unwrap_or(0),
                value_cents: amount.and_then(real_to_cents).unwrap_or(0),
                ..Default::default()
            },
            classification: moneymoney_classification(asset_class),
        });
    }
    Ok(holdings)
}

/// Banks – never credential / keys / systemid / bpd / upd / local_storage / userdata.
fn read_banks(conn: &Connection) -> CoreResult<Vec<SrcBank>> {
    let sql = select(
        conn,
        "banks",
        &[
            "rowid", "bankcode", "username", "customer", "name", "server",
        ],
        "ORDER BY rowid",
    )?;
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map([], |r| {
        Ok(SrcBank {
            id: vint(get(r, 0)?).unwrap_or_default(),
            bankcode: vtext(get(r, 1)?),
            username: vtext(get(r, 2)?),
            customer: vtext(get(r, 3)?),
            name: vtext(get(r, 4)?),
            server: vtext(get(r, 5)?),
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn read_account_groups(conn: &Connection) -> CoreResult<HashMap<i64, (String, i64, i64)>> {
    let sql = select(
        conn,
        "account_groups",
        &["rowid", "name", "group_key", "sort"],
        "",
    )?;
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map([], |r| {
        Ok((
            vint(get(r, 0)?).unwrap_or_default(),
            (
                vtext(get(r, 1)?).trim().to_string(),
                vint(get(r, 2)?).unwrap_or(0),
                vint(get(r, 3)?).unwrap_or(0),
            ),
        ))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Categories with their nested groups.
fn read_categories(conn: &Connection, have: &HashSet<String>) -> CoreResult<SrcCategories> {
    let mut cats = SrcCategories::default();
    // rowid -> (name, parent group rowid or 0, colour)
    let mut groups: HashMap<i64, (String, i64, Option<String>)> = HashMap::new();
    if have.contains("category_groups") {
        let sql = select(
            conn,
            "category_groups",
            &["rowid", "name", "group_key", "color"],
            "",
        )?;
        let mut st = conn.prepare(&sql)?;
        let rows = st.query_map([], |r| {
            Ok((
                vint(get(r, 0)?).unwrap_or_default(),
                (
                    vtext(get(r, 1)?),
                    vint(get(r, 2)?).unwrap_or(0),
                    mm_color(&vtext(get(r, 3)?)),
                ),
            ))
        })?;
        groups = rows.collect::<Result<_, _>>()?;
    }
    // names of a group and its parents, outermost first
    let group_path = |mut g: i64| -> Vec<String> {
        let mut path = Vec::new();
        let mut guard = 0;
        while g > 0 && guard < 32 {
            guard += 1;
            let Some((gname, parent, _)) = groups.get(&g) else {
                break;
            };
            if !gname.is_empty() {
                path.push(gname.clone());
            }
            g = *parent;
        }
        path.reverse();
        path
    };
    let mut group_ids: Vec<&i64> = groups.keys().collect();
    group_ids.sort();
    for g in group_ids {
        if let (Some(color), path) = (&groups[g].2, group_path(*g))
            && !path.is_empty()
        {
            cats.colors.push((path, color.clone()));
        }
    }
    let sql = select(
        conn,
        "categories",
        &[
            "rowid",
            "name",
            "type",
            "group_key",
            "rules",
            "apply_to_account",
            "color",
        ],
        "ORDER BY group_key, sort, rowid",
    )?;
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map([], |r| {
        Ok((
            vint(get(r, 0)?).unwrap_or_default(),
            vtext(get(r, 1)?),
            vint(get(r, 2)?),
            vint(get(r, 3)?).unwrap_or(0),
            vtext(get(r, 4)?),
            vtext(get(r, 5)?),
            mm_color(&vtext(get(r, 6)?)),
        ))
    })?;
    for row in rows {
        let (id, name, typ, group, rules, apply_to_account, color) = row?;
        // rowid 1 / type 0 / empty name = "Ohne Kategorie"
        if name.is_empty() || typ == Some(0) {
            continue;
        }
        let mut path = group_path(group);
        path.push(name);
        let rules = rules.trim();
        if !rules.is_empty() {
            let restricted = !apply_to_account.trim().is_empty();
            cats.rules
                .push((path.clone(), rules.to_string(), restricted));
        }
        if let Some(color) = color {
            cats.colors.push((path.clone(), color));
        }
        cats.order.push(path.clone());
        cats.by_id.insert(id, path);
    }
    Ok(cats)
}

/// Transactions – counterparty, texts, references; `pn` unused.
fn read_transactions(conn: &Connection) -> CoreResult<Vec<SrcTx>> {
    let sql = select(
        conn,
        "transactions",
        &[
            "rowid",
            "local_account_key",
            "account",
            "bankcode",
            "amount",
            "currency",
            "timestamp",
            "value_timestamp",
            "eref",
            "mref",
            "cred",
            "comment",
            "booked",
            "unformatted_type",
            "unformatted_purpose",
            "name",
            "checkmark",
            "split_key",
            "category_key",
        ],
        "ORDER BY timestamp, rowid",
    )?;
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map([], |r| {
        Ok(SrcTx {
            id: vint(get(r, 0)?).unwrap_or_default(),
            account_key: vint(get(r, 1)?).unwrap_or_default(),
            cp_account: vtext(get(r, 2)?),
            cp_bankcode: vtext(get(r, 3)?),
            amount: vreal(get(r, 4)?),
            currency: vtext(get(r, 5)?).to_uppercase(),
            ts: vint(get(r, 6)?),
            value_ts: vint(get(r, 7)?).filter(|t| *t > 0),
            eref: vtext(get(r, 8)?),
            mref: vtext(get(r, 9)?),
            cred: vtext(get(r, 10)?),
            comment: vtext(get(r, 11)?),
            booked: vint(get(r, 12)?).unwrap_or(1) != 0,
            booking_text: vtext(get(r, 13)?),
            purpose: vtext(get(r, 14)?),
            name: vtext(get(r, 15)?),
            checkmark: vint(get(r, 16)?).unwrap_or(0) != 0,
            split_key: vint(get(r, 17)?).filter(|k| *k > 0),
            category_key: vint(get(r, 18)?),
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn read_balances(conn: &Connection) -> CoreResult<HashMap<(i64, String), (i64, f64)>> {
    let sql = select(
        conn,
        "balances",
        &["account_key", "currency", "timestamp", "initial_balance"],
        "ORDER BY timestamp",
    )?;
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map([], |r| {
        Ok((
            vint(get(r, 0)?),
            vtext(get(r, 1)?).to_uppercase(),
            vint(get(r, 2)?).unwrap_or(0),
            vreal(get(r, 3)?),
        ))
    })?;
    let mut balances = HashMap::new();
    for row in rows {
        if let (Some(acc), cur, ts, Some(v)) = row? {
            // ordered by timestamp: the newest row wins
            balances.insert((acc, cur), (ts, v));
        }
    }
    Ok(balances)
}

/// PDFs above this size are skipped (never loaded into memory).
const MAX_PDF_BYTES: u64 = 25 * 1024 * 1024;
/// Files looked at when searching a folder by checksum.
const MAX_FOLDER_FILES: usize = 50_000;

/// The statements / bank documents MoneyMoney lists (not the files).
fn read_documents(conn: &Connection, have: &HashSet<String>) -> CoreResult<Vec<SrcDoc>> {
    if !have.contains("statements") {
        return Ok(Vec::new());
    }
    let sql = select(
        conn,
        "statements",
        &[
            "account_key",
            "directory",
            "filename",
            "created_tsp",
            "digest",
            "deleted",
        ],
        "ORDER BY rowid",
    )?;
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map([], |r| {
        Ok((
            SrcDoc {
                account_key: vint(get(r, 0)?).unwrap_or_default(),
                directory: vtext(get(r, 1)?),
                filename: vtext(get(r, 2)?),
                created_ts: vint(get(r, 3)?).filter(|t| *t > 0),
                digest: vtext(get(r, 4)?).trim().to_lowercase(),
            },
            vint(get(r, 5)?).unwrap_or(0) != 0,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (d, deleted) = row?;
        if !deleted && !d.filename.trim().is_empty() {
            out.push(d);
        }
    }
    Ok(out)
}

/// What the user pointed the import at: MoneyMoney's data folder or a single
/// database file. Returns the database file and, for a folder, its
/// `Statements` folder (if there is one).
fn locate(path: &str) -> CoreResult<(String, Option<PathBuf>)> {
    let p = Path::new(path);
    if !p.is_dir() {
        return Ok((path.to_string(), None));
    }
    let mut found: Vec<PathBuf> = Vec::new();
    for dir in [p.join("Database"), p.to_path_buf()] {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|f| {
                f.is_file()
                    && f.extension()
                        .is_some_and(|x| x.eq_ignore_ascii_case("sqlite"))
            })
            .collect();
        // "MoneyMoney.sqlite" first, then by name
        files.sort_by_key(|f| {
            let name = f
                .file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            (name != "moneymoney.sqlite", name)
        });
        found.extend(files);
    }
    let db = found
        .into_iter()
        .next()
        .ok_or_else(|| CoreError::NotFound {
            what: tr("Datenbankdatei").into(),
        })?;
    let statements = p.join("Statements");
    Ok((
        db.to_string_lossy().into_owned(),
        statements.is_dir().then_some(statements),
    ))
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A single, harmless path component (never "..", never a nested path).
fn component(s: &str) -> Option<&str> {
    let s = s.trim();
    let ok = !s.is_empty() && s != "." && s != ".." && !s.contains(['/', '\\', '\0']);
    ok.then_some(s)
}

/// The folder the PDFs are taken from, and how to find a listed file in it.
struct StatementFiles {
    root: PathBuf,
    /// `root` with all symlinks resolved; every file handed out lies below it.
    canonical_root: Option<PathBuf>,
    /// All PDFs below `root` (built on first use).
    all: Option<Vec<PathBuf>>,
    /// SHA-256 → file, for files not at their original place (built on first use).
    by_digest: Option<HashMap<String, PathBuf>>,
    /// Files handed out by `find`.
    used: HashSet<PathBuf>,
}

impl StatementFiles {
    fn new(root: PathBuf) -> Self {
        Self {
            canonical_root: std::fs::canonicalize(&root).ok(),
            root,
            all: None,
            by_digest: None,
            used: HashSet::new(),
        }
    }

    /// All PDFs below the folder (no symlinks, bounded depth and count).
    fn all(&mut self) -> &[PathBuf] {
        if self.all.is_none() {
            let mut out = Vec::new();
            let mut stack = vec![(self.root.clone(), 0u32)];
            while let Some((dir, depth)) = stack.pop() {
                let Ok(entries) = std::fs::read_dir(&dir) else {
                    continue;
                };
                for e in entries.flatten() {
                    let Ok(kind) = e.file_type() else { continue };
                    let path = e.path();
                    if kind.is_dir() && depth < 6 {
                        stack.push((path, depth + 1));
                    } else if kind.is_file()
                        && path
                            .extension()
                            .is_some_and(|x| x.eq_ignore_ascii_case("pdf"))
                        && out.len() < MAX_FOLDER_FILES
                    {
                        out.push(path);
                    }
                }
            }
            out.sort();
            self.all = Some(out);
        }
        self.all.as_deref().unwrap_or_default()
    }

    /// A regular file that is not a symlink and, after resolving every link
    /// on the way, lies inside the selected folder (no escapes via a linked
    /// sub-folder or file).
    fn safe_file(&self, path: &Path) -> bool {
        let Some(root) = &self.canonical_root else {
            return false;
        };
        let plain = std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file());
        plain && std::fs::canonicalize(path).is_ok_and(|c| c.starts_with(root))
    }

    /// The file of a listed document: at its original place, else by name
    /// directly in the folder, else by checksum anywhere below it.
    fn find(&mut self, d: &SrcDoc) -> Option<PathBuf> {
        let name = component(&d.filename)?;
        let mut candidates = vec![self.root.join(name)];
        if let Some(dir) = component(&d.directory) {
            candidates.insert(0, self.root.join(dir).join(name));
        }
        let mut hit = candidates.into_iter().find(|c| self.safe_file(c));
        if hit.is_none() && !d.digest.is_empty() {
            if self.by_digest.is_none() {
                let mut map = HashMap::new();
                for f in self.all().to_vec() {
                    if !self.safe_file(&f) {
                        continue;
                    }
                    let small = std::fs::metadata(&f).is_ok_and(|m| m.len() <= MAX_PDF_BYTES);
                    if let (true, Ok(bytes)) = (small, std::fs::read(&f)) {
                        map.entry(sha256_hex(&bytes)).or_insert(f);
                    }
                }
                self.by_digest = Some(map);
            }
            hit = self
                .by_digest
                .as_ref()
                .and_then(|m| m.get(&d.digest))
                .cloned();
        }
        if let Some(h) = &hit {
            self.used.insert(h.clone());
        }
        hit
    }

    /// PDFs in the folder that no listed document used.
    fn unlisted(&mut self) -> u32 {
        let all = self.all().to_vec();
        all.iter().filter(|f| !self.used.contains(*f)).count() as u32
    }
}

/// Archive section for a MoneyMoney document, from its file name (MoneyMoney
/// stores no document type): kontoauszug | abrechnung | steuer | postbox.
fn doc_type_for(file_name: &str) -> &'static str {
    let n = file_name.to_lowercase();
    if has_any(
        &n,
        &[
            "steuer",
            "jstb",
            "erträgnisaufstellung",
            "ertraegnisaufstellung",
        ],
    ) {
        "steuer"
    } else if has_any(&n, &["kontoauszug", "depotauszug"]) {
        "kontoauszug"
    } else if n.contains("abrechnung") {
        "abrechnung"
    } else {
        "postbox"
    }
}

// ------------------------------------------------------------------ rows

struct AccountRows {
    rows: Vec<MmRow>,
    pending: u32,
    split_parts: u32,
    balance_cents: i64,
    /// `balance_cents` is MoneyMoney's displayed balance (not computed).
    balance_shown: bool,
    balance_source: String,
    balance_date: String,
    /// MoneyMoney has an opening balance ("Startsaldo") for the account: a
    /// usable balance-history row or the account's own initial balance.
    /// Carried over undated, its amount solved from `balance_cents` (#212).
    opening: bool,
}

impl Source {
    /// The account groups from the top level down to `key`: (rowid, name).
    /// Groups without a name are skipped.
    fn group_chain(&self, key: Option<i64>) -> Vec<(i64, String)> {
        let mut chain = Vec::new();
        let mut next = key.unwrap_or(0);
        let mut seen = HashSet::new();
        while next > 0 && seen.insert(next) {
            let Some((name, parent, _)) = self.account_groups.get(&next) else {
                break;
            };
            if !name.is_empty() {
                chain.push((next, name.clone()));
            }
            next = *parent;
        }
        chain.reverse();
        chain
    }

    /// Position of every account group in MoneyMoney's sidebar order
    /// (a group, then its subgroups; siblings by `sort`).
    fn group_ranks(&self) -> HashMap<i64, usize> {
        let mut children: HashMap<i64, Vec<i64>> = HashMap::new();
        for (id, (_, parent, _)) in &self.account_groups {
            // a parent that doesn't exist counts as top level
            let p = if self.account_groups.contains_key(parent) {
                *parent
            } else {
                0
            };
            children.entry(p).or_default().push(*id);
        }
        for list in children.values_mut() {
            // reversed: the stack below pops the first sibling first
            list.sort_by_key(|id| std::cmp::Reverse((self.account_groups[id].2, *id)));
        }
        let mut rank = HashMap::new();
        let mut stack = children.get(&0).cloned().unwrap_or_default();
        while let Some(id) = stack.pop() {
            if rank.contains_key(&id) {
                continue;
            }
            rank.insert(id, rank.len());
            stack.extend(children.get(&id).into_iter().flatten().copied());
        }
        rank
    }

    fn bank(&self, key: Option<i64>) -> Option<&SrcBank> {
        key.and_then(|k| self.banks.iter().find(|b| b.id == k))
    }

    /// GiroCrab connector (and stored key) for a MoneyMoney bank login. Like
    /// `connector_key_for`, but also looks at the login's accounts: bunq has
    /// branches in several countries (DE, NL, FR, ES, IT, …) and MoneyMoney
    /// may file the login under any of their codes, so a login whose accounts
    /// carry a bunq BIC or IBAN is mapped to the bunq connector.
    fn connector_key(&self, b: &SrcBank) -> Option<(ConnectorKind, String)> {
        let keyed = |k: ConnectorKind| (k, k.as_str().to_string());
        connector_key_for(&b.bankcode, &b.name).or_else(|| {
            let mut accounts = self.accounts.iter().filter(|a| a.bank_key == Some(b.id));
            if accounts
                .clone()
                .any(|a| is_bunq_account(&a.bic, &a.iban, &a.bankcode))
            {
                return Some(keyed(ConnectorKind::Bunq));
            }
            // The Amazon Visa is issued by Open Bank (Zinia); MoneyMoney files
            // the login as "Openbank" (bank code 50034200). Open Bank also has
            // current accounts, so only a login holding the card maps to the
            // Amazon Visa connector.
            (is_openbank(&b.bankcode, &b.name)
                && accounts.any(|a| {
                    a.name.to_lowercase().contains("amazon")
                        || suggest_kind(a, &b.name) == AccountKind::CreditCard
                }))
            .then(|| keyed(ConnectorKind::AmazonVisa))
        })
    }

    fn connector(&self, b: &SrcBank) -> Option<ConnectorKind> {
        self.connector_key(b).map(|(k, _)| k)
    }

    fn account_name(&self, a: &SrcAccount) -> String {
        if !a.name.is_empty() {
            return a.name.clone();
        }
        let bank = self.bank(a.bank_key).map(|b| b.name.as_str()).unwrap_or("");
        let s = format!("{bank} {}", a.account).trim().to_string();
        if s.is_empty() {
            format!("Konto {}", a.id)
        } else {
            s
        }
    }

    /// The other own MoneyMoney account a booking's counter account points to:
    /// by IBAN, or – for pre-SEPA bookings, cards and depots where MoneyMoney
    /// only has an account number – by account number (+ bank code / BIC when
    /// both sides have one), also against the BLZ/account number inside an
    /// own German IBAN.
    fn own_counter_account(
        &self,
        from: &SrcAccount,
        cp_account: &str,
        cp_bankcode: &str,
    ) -> Option<&SrcAccount> {
        let cp = norm_account_id(cp_account);
        if cp.len() < 4 {
            return None;
        }
        let cp_iban = validate_iban(cp_account).then(|| normalize_iban(cp_account));
        // (account number, bank code) as written in a German IBAN
        let cp_de = cp_iban.as_deref().and_then(de_iban_parts);
        let cp_bank = norm_account_id(cp_bankcode);
        let bank_ok = |own_bank: &str| {
            let own_bank = norm_account_id(own_bank);
            cp_bank.is_empty() || own_bank.is_empty() || own_bank == cp_bank
        };
        self.accounts.iter().filter(|o| o.id != from.id).find(|o| {
            if let Some(iban) = &cp_iban {
                if !o.iban.is_empty() {
                    return *iban == o.iban;
                }
                // own account without IBAN in MoneyMoney: compare the German
                // IBAN's account number / BLZ with it
                return cp_de.as_ref().is_some_and(|(kto, blz)| {
                    norm_account_id(&o.account) == *kto
                        && (o.bankcode.is_empty() || norm_account_id(&o.bankcode) == *blz)
                });
            }
            let own_acc = norm_account_id(&o.account);
            if !own_acc.is_empty()
                && own_acc == cp
                && (bank_ok(&o.bankcode) || (!o.bic.is_empty() && bank_ok(&o.bic)))
            {
                return true;
            }
            de_iban_parts(&o.iban).is_some_and(|(kto, blz)| kto == cp && bank_ok(&blz))
        })
    }

    /// Normalised bookings of one account: booked only, a split booking with its
    /// parts as splits (#877, see [`into_split`]), plus the balance (see module docs).
    fn rows_for(&self, a: &SrcAccount) -> AccountRows {
        let txs: Vec<&SrcTx> = self.txs.iter().filter(|t| t.account_key == a.id).collect();
        let ids: HashSet<i64> = txs.iter().map(|t| t.id).collect();
        let is_part = |t: &SrcTx| t.split_key.is_some_and(|k| k != t.id && ids.contains(&k));
        let parents: HashSet<i64> = txs
            .iter()
            .filter(|t| is_part(t))
            .filter_map(|t| t.split_key)
            .collect();
        let mut out = AccountRows {
            rows: Vec::new(),
            pending: 0,
            split_parts: 0,
            balance_cents: 0,
            balance_shown: false,
            balance_source: String::new(),
            balance_date: String::new(),
            opening: false,
        };
        // (date, cents) of the booked bookings that count for the balance:
        // parents (not their parts), account currency only.
        let mut counted: Vec<(String, i64)> = Vec::new();
        // rows in order, each with its id if it is a split booking; the parts by parent
        let mut staged: Vec<(MmRow, Option<i64>)> = Vec::new();
        let mut parts_of: BTreeMap<i64, Vec<MmRow>> = BTreeMap::new();
        for t in &txs {
            let (Some(ts), Some(amount)) = (t.ts, t.amount) else {
                continue;
            };
            let (Some(date), Some(cents)) = (unix_to_date(ts), real_to_cents(amount)) else {
                continue;
            };
            if !t.booked {
                out.pending += 1;
                continue;
            }
            let parent = t.split_key.filter(|_| is_part(t));
            if parent.is_none() && (t.currency.is_empty() || t.currency == a.currency) {
                counted.push((date.clone(), cents)); // the parent carries the full amount
            }
            let row = self.mm_row(a, t, date, cents);
            match parent {
                Some(k) => parts_of.entry(k).or_default().push(row),
                None => staged.push((row, parents.contains(&t.id).then_some(t.id))),
            }
        }
        for (mut row, split) in staged {
            let Some(id) = split else {
                out.rows.push(row);
                continue;
            };
            match into_split(&mut row, parts_of.remove(&id).unwrap_or_default()) {
                Ok(()) => out.rows.push(row),
                Err(parts) => {
                    out.split_parts += parts.len() as u32;
                    out.rows.extend(parts);
                }
            }
        }
        // parts of a parent that is not imported (pending, no date or amount)
        for parts in parts_of.into_values() {
            out.split_parts += parts.len() as u32;
            out.rows.extend(parts);
        }
        let last = out
            .rows
            .iter()
            .map(|r| r.booking_date.clone())
            .max()
            .unwrap_or_default();
        out.opening = match self.balances.get(&(a.id, a.currency.clone())) {
            Some((_, v)) => real_to_cents(*v).is_some(),
            None => a
                .initial_balance
                .filter(|v| *v != 0.0)
                .and_then(real_to_cents)
                .is_some(),
        };
        if let Some((cents, cur)) = &a.shown_balance
            && (cur.is_empty() || *cur == a.currency)
        {
            let d = a.balance_ts.and_then(unix_to_date).unwrap_or_default();
            out.balance_cents = *cents;
            out.balance_shown = true;
            out.balance_source = if d.is_empty() {
                tr("Saldo laut Datenbank").into()
            } else {
                trf("Saldo laut Datenbank ({0})", &[&d])
            };
            out.balance_date = last.max(d);
            return out;
        }
        let (base, source, base_date) = match self.balances.get(&(a.id, a.currency.clone())) {
            Some((ts, v)) => {
                let d = unix_to_date(*ts).unwrap_or_default();
                (
                    *v,
                    trf(
                        "Anfangssaldo laut Saldenverlauf der Datenbank ({0}) + Umsätze",
                        &[&d],
                    ),
                    d,
                )
            }
            None => match a.initial_balance {
                Some(v) if v != 0.0 => (
                    v,
                    tr("Anfangssaldo des Kontos + Umsätze").into(),
                    String::new(),
                ),
                _ => (0.0, tr("Summe der Umsätze").into(), String::new()),
            },
        };
        // An initial balance from the balance history applies from its date on:
        // earlier bookings are already part of it. (`base_date` is "" for the
        // account's own initial balance → every booking counts.)
        let sum: i64 = counted
            .iter()
            .filter(|(d, _)| *d >= base_date)
            .map(|(_, c)| c)
            .sum();
        out.balance_cents = real_to_cents(base).unwrap_or(0) + sum;
        out.balance_source = source;
        out.balance_date = last.max(base_date);
        out
    }

    /// One booked MoneyMoney booking as a row (`date` / `cents` already parsed).
    fn mm_row(&self, a: &SrcAccount, t: &SrcTx, date: String, cents: i64) -> MmRow {
        let clean = |s: &str| crate::text::clean_bank_text(s.trim());
        let own = self.own_counter_account(a, &t.cp_account, &t.cp_bankcode);
        let mut path = t
            .category_key
            .and_then(|k| self.categories.by_id.get(&k))
            .cloned()
            .unwrap_or_default();
        // Issue #94: a booking to / from another own account is a transfer,
        // also when MoneyMoney only has the account number (pre-SEPA,
        // cards, depots) – uncategorised ones go to "Umbuchungen".
        if own.is_some() && path.is_empty() {
            path = vec!["Umbuchungen".to_string()];
        }
        let iban = if validate_iban(&t.cp_account) {
            normalize_iban(&t.cp_account)
        } else {
            own.map(|o| o.iban.clone())
                .filter(|i| validate_iban(i))
                .unwrap_or_default()
        };
        MmRow {
            booking_date: date,
            value_date: t.value_ts.and_then(unix_to_date).unwrap_or_default(),
            amount_cents: cents,
            currency: if t.currency.is_empty() {
                a.currency.clone()
            } else {
                t.currency.clone()
            },
            name: clean(&t.name),
            iban,
            bic: if looks_like_bic(&t.cp_bankcode) {
                t.cp_bankcode.to_uppercase()
            } else {
                String::new()
            },
            purpose: clean(&t.purpose),
            booking_text: clean(&t.booking_text),
            category: path.join("\\"),
            category_parts: path,
            note: t.comment.clone(),
            checkmark: t.checkmark,
            mandate_ref: t.mref.clone(),
            creditor_id: t.cred.clone(),
            end_to_end: t.eref.clone(),
            ..Default::default()
        }
    }
}

/// #877: lets `row`, a MoneyMoney split booking, carry its `parts` as GiroCrab
/// splits; the booking itself keeps no category. GiroCrab needs the parts to
/// add up exactly to the booking, at least two of them ≠ 0: parts of 0 are
/// dropped, a single remaining part only passes on its category (and note).
/// Otherwise the parts come back, to be imported as bookings of their own (as
/// before #877). A part without a category takes the booking's. Only a split
/// keeps its parts ([`MmRow::parts`]): a plain booking matches a part imported
/// before by itself.
fn into_split(row: &mut MmRow, parts: Vec<MmRow>) -> Result<(), Vec<MmRow>> {
    let sum = parts
        .iter()
        .try_fold(0i64, |acc, p| acc.checked_add(p.amount_cents));
    let live: Vec<&MmRow> = parts.iter().filter(|p| p.amount_cents != 0).collect();
    if sum != Some(row.amount_cents)
        || live.is_empty()
        || parts.iter().any(|p| p.currency != row.currency)
    {
        return Err(parts);
    }
    let path = |p: &MmRow| {
        if p.category_parts.is_empty() {
            row.category_parts.clone()
        } else {
            p.category_parts.clone()
        }
    };
    if let [p] = live[..] {
        let (cat, note) = (path(p), p.note.clone());
        row.category = cat.join("\\");
        row.category_parts = cat;
        if row.note.trim().is_empty() {
            row.note = note;
        }
    } else {
        // the part's own name / purpose, where it says more than the booking's
        fn differs<'a>(own: &'a str, whole: &str) -> Option<&'a str> {
            let own = own.trim();
            (!own.is_empty() && !own.eq_ignore_ascii_case(whole.trim())).then_some(own)
        }
        let splits = live
            .iter()
            .map(|p| MmSplit {
                amount_cents: p.amount_cents,
                category_parts: path(p),
                note: p.note.trim().to_string(),
                text: [
                    differs(&p.name, &row.name),
                    differs(&p.purpose, &row.purpose),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · "),
            })
            .collect();
        row.splits = splits;
        row.category.clear();
        row.category_parts.clear();
        row.parts = parts;
    }
    Ok(())
}

// ------------------------------------------------------------------ vault side

/// The vault account each MoneyMoney account is merged into, keyed by its
/// MoneyMoney rowid. A vault account is claimed at most once: MoneyMoney keeps
/// closed / duplicate accounts with the same IBAN (issue #94), and without this
/// all of them would be merged into one account. Matches by `external_ref`
/// (an earlier import) win over IBAN matches.
fn find_existing_accounts(
    conn: &Connection,
    accounts: &[SrcAccount],
) -> CoreResult<HashMap<i64, (i64, String)>> {
    let mut out: HashMap<i64, (i64, String)> = HashMap::new();
    let mut claimed: HashSet<i64> = HashSet::new();
    for a in accounts {
        let by_ref: Option<(i64, String)> = conn
            .query_row(
                // includes soft-deleted rows: the import revives a deleted account
                "SELECT id, name FROM accounts WHERE external_ref = ?1 ORDER BY id LIMIT 1",
                [ext_ref(a)],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        // a twin folded into its bank account: the import goes there
        let by_ref = match by_ref {
            Some((id, name)) => match crate::sync_merge::twins::folded_into(conn, id)? {
                Some(bank) => Some((
                    bank,
                    conn.query_row("SELECT name FROM accounts WHERE id = ?1", [bank], |r| {
                        r.get(0)
                    })?,
                )),
                None => Some((id, name)),
            },
            None => None,
        };
        if let Some(found) = by_ref {
            claimed.insert(found.0);
            out.insert(a.id, found);
        }
    }
    for a in accounts.iter().filter(|a| !a.iban.is_empty()) {
        if out.contains_key(&a.id) {
            continue;
        }
        let mut st = conn.prepare(
            // includes soft-deleted rows
            "SELECT id, name FROM accounts WHERE upper(replace(iban, ' ', '')) = ?1
             ORDER BY (connection_id IS NULL), id",
        )?;
        let candidates: Vec<(i64, String)> = st
            .query_map([&a.iban], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        if let Some(found) = candidates.into_iter().find(|c| !claimed.contains(&c.0)) {
            claimed.insert(found.0);
            out.insert(a.id, found);
        }
    }
    Ok(out)
}

fn find_connection(
    conn: &Connection,
    kind: ConnectorKind,
    key: &str,
    login: &str,
) -> CoreResult<Option<i64>> {
    let login = crate::vault::connections::normalize_login(kind, login);
    let mut st = conn
        .prepare("SELECT id, login_name FROM bank_connections WHERE connector = ?1 AND deleted_ms IS NULL ORDER BY id")?;
    let rows: Vec<(i64, String)> = st
        .query_map([key], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    Ok(rows
        .into_iter()
        .find(|(_, l)| {
            crate::vault::connections::normalize_login(kind, l).eq_ignore_ascii_case(&login)
        })
        .map(|(id, _)| id))
}

/// Called by `store_fetch` when a synced account is not known yet: takes over
/// an account created by this importer instead of adding a second one. Matched
/// by IBAN; without one (credit cards, PayPal) only when exactly one imported
/// account of that kind is linked to this bank login.
pub(crate) fn adopt_account(
    conn: &Connection,
    connection_id: i64,
    external_ref: &str,
    iban: &str,
    currency: &str,
    kind: AccountKind,
) -> CoreResult<Option<i64>> {
    let iban = normalize_iban(iban);
    // „Abrufweg umstellen“ marked it: the same way back finds it by its old
    // reference, also without an IBAN (a depot next to a deleted twin, #537)
    let switched: Option<i64> = conn
        .query_row(
            "SELECT id FROM accounts WHERE connection_id = ?1 AND external_ref = ?2 AND deleted_ms IS NULL",
            params![connection_id, format!("{}{external_ref}", crate::vault::connections::SWITCH_MARK)],
            |r| r.get(0),
        )
        .optional()?;
    let id: Option<i64> = if switched.is_some() {
        switched
    } else if iban.is_empty() {
        // Prefer imported accounts without an IBAN; a connector that reports
        // none (Amazon Visa web login) may still find the card with the IBAN
        // MoneyMoney stored for it – if it is the only one of its kind here.
        // includes soft-deleted rows: `store_fetch` revives or skips them;
        // archived (closed in MoneyMoney) accounts are never guessed, or an old
        // card next to the active one would block the takeover (#816); nor are
        // those the #998 repair reactivated while another one fits (they may
        // be closed cards, too)
        let unknown = archive_unknown(conn)?;
        let known = if unknown.is_empty() {
            String::new()
        } else {
            let ids: Vec<String> = unknown.iter().map(i64::to_string).collect();
            format!("AND id NOT IN ({})", ids.join(","))
        };
        // exactly one account for `query` (its `{}` takes a filter), known ones first
        let only = |query: &str, args: &[&dyn rusqlite::ToSql]| -> CoreResult<Option<i64>> {
            let ids = |filter: &str| -> CoreResult<Vec<i64>> {
                let mut st = conn.prepare(&query.replace("{}", filter))?;
                Ok(st
                    .query_map(args, |r| r.get(0))?
                    .collect::<Result<_, _>>()?)
            };
            let mut found = ids(&known)?;
            if found.is_empty() && !known.is_empty() {
                found = ids("")?;
            }
            Ok((found.len() == 1).then(|| found[0]))
        };
        let single = |iban_filter: &str| -> CoreResult<Option<i64>> {
            only(
                &format!(
                    "SELECT id FROM accounts WHERE connection_id = ?1 AND external_ref LIKE 'moneymoney-db:%'
                       AND kind = ?2 AND archived_unix IS NULL {iban_filter} {{}} LIMIT 2"
                ),
                params![connection_id, kind.as_str()],
            )
        };
        let currency = currency.trim().to_uppercase();
        let single_cur = |extra_filter: &str| -> CoreResult<Option<i64>> {
            only(
                &format!(
                    "SELECT id FROM accounts WHERE connection_id = ?1 AND external_ref LIKE 'moneymoney-db:%'
                       AND kind = ?2 AND archived_unix IS NULL
                       AND (?3 = '' OR upper(IFNULL(currency, '')) IN ('', ?3)) {extra_filter} {{}} LIMIT 2"
                ),
                params![connection_id, kind.as_str(), currency],
            )
        };
        match single("AND replace(IFNULL(iban, ''), ' ', '') = ''")? {
            Some(id) => Some(id),
            // Connectors that omit the IBAN (e.g. Amazon Visa web login, Trade Republic
            // clearing account) may still adopt a single imported account of the same kind
            // and currency on this login (#1114).
            None if kind == AccountKind::CreditCard
                || (kind == AccountKind::Checking && external_ref == "tr:cash") =>
            {
                single_cur("")?
            }
            None => None,
        }
    } else {
        // Wallets in several currencies can share one IBAN (Finom EUR/USD):
        // never take over one in another currency, prefer the exact currency.
        // The import puts a bank without own connector on a PSD2 connection;
        // as long as that one never synced, a direct connector of the same
        // bank takes its accounts over instead of adding a second set.
        let currency = currency.trim().to_uppercase();
        conn.query_row(
            // includes soft-deleted rows
            "SELECT id FROM accounts WHERE (connection_id IS NULL OR connection_id = ?1
                    OR connection_id IN (SELECT id FROM bank_connections
                         WHERE id <> ?1 AND connector LIKE 'enablebanking%' AND last_sync_unix IS NULL))
               AND external_ref LIKE 'moneymoney-db:%' AND upper(replace(iban, ' ', '')) = ?2
               AND (?3 = '' OR upper(IFNULL(currency, '')) IN ('', ?3))
             ORDER BY CASE WHEN connection_id = ?1 THEN 0 WHEN connection_id IS NULL THEN 1 ELSE 2 END,
                      (upper(IFNULL(currency, '')) <> ?3), id LIMIT 1",
            params![connection_id, iban, currency],
            |r| r.get(0),
        )
        .optional()?
    };
    if let Some(id) = id {
        // a deleted account that stays deleted keeps its sync key
        if crate::sync_merge::fetch_keeps_account_deleted(conn, connection_id, id)? {
            return Ok(Some(id));
        }
        conn.execute(
            "UPDATE accounts SET connection_id = ?1, external_ref = ?2 WHERE id = ?3",
            params![connection_id, external_ref, id],
        )?;
    }
    Ok(id)
}

/// MoneyMoney's tree replaces the starter categories only in a vault without
/// bookings: nothing can be assigned to them yet, so nothing is lost.
fn replaces_starter_categories(conn: &Connection, src: &Source) -> CoreResult<bool> {
    if src.categories.order.is_empty() {
        return Ok(false);
    }
    let bookings: i64 = conn.query_row(
        "SELECT count(*) FROM transactions WHERE deleted_ms IS NULL",
        [],
        |r| r.get(0),
    )?;
    Ok(bookings == 0)
}

/// Every way to connect a MoneyMoney bank login, best first (FinTS, own
/// API, Enable Banking – see `connector_for_inner`).
fn connection_options(
    src: &Source,
    b: &SrcBank,
    catalog: &[Psd2Bank],
) -> Vec<MoneyMoneyDbConnectionOption> {
    let mut out: Vec<MoneyMoneyDbConnectionOption> = src
        .connector_key(b)
        .map(|(kind, key)| MoneyMoneyDbConnectionOption::direct(kind, key))
        .into_iter()
        .collect();
    // comdirect: FinTS as an alternative to its own API (which stays the
    // default). Only with a real bank code, FinTS needs the one of the IBAN.
    let code: String = b.bankcode.chars().filter(|c| c.is_ascii_digit()).collect();
    if out.iter().any(|o| o.connector == ConnectorKind::Comdirect)
        && crate::connectors::fints_directory::COMDIRECT_BLZ.contains(&code.as_str())
    {
        out.push(MoneyMoneyDbConnectionOption::direct(
            ConnectorKind::FinTsGeneric,
            ConnectorKind::fints_key(&code),
        ));
    }
    if let Some(bank) = match_psd2_bank(&b.name, catalog) {
        out.push(MoneyMoneyDbConnectionOption::enable_banking(bank));
    }
    out
}

fn preview(
    conn: &Connection,
    src: &Source,
    database_path: String,
    mut files: Option<StatementFiles>,
) -> CoreResult<MoneyMoneyDbPreview> {
    let mut accounts = Vec::new();
    let (mut total, mut pending, mut notes, mut parts) = (0u32, 0u32, 0u32, 0u32);
    let mut first: Option<String> = None;
    let mut last: Option<String> = None;
    let mut existing_accounts = find_existing_accounts(conn, &src.accounts)?;
    for a in &src.accounts {
        let r = src.rows_for(a);
        let bank = src.bank(a.bank_key);
        let bank_name = bank.map(|b| b.name.clone()).unwrap_or_default();
        let existing = existing_accounts.remove(&a.id);
        let f = r.rows.iter().map(|x| x.booking_date.clone()).min();
        let l = r.rows.iter().map(|x| x.booking_date.clone()).max();
        let n = r
            .rows
            .iter()
            .flat_map(|x| std::iter::once(&x.note).chain(x.splits.iter().map(|s| &s.note)))
            .filter(|n| !n.trim().is_empty())
            .count() as u32;
        total += r.rows.len() as u32;
        pending += r.pending;
        notes += n;
        parts += r.split_parts;
        first = match (first, f.clone()) {
            (Some(x), Some(y)) => Some(x.min(y)),
            (x, y) => x.or(y),
        };
        last = last.max(l.clone());
        accounts.push(MoneyMoneyDbAccount {
            mm_id: a.id,
            name: src.account_name(a),
            iban: a.iban.clone(),
            account_number: a.account.clone(),
            currency: a.currency.clone(),
            bank_code: bank
                .map(|b| b.bankcode.clone())
                .filter(|c| !c.is_empty())
                .unwrap_or_else(|| a.bankcode.clone()),
            suggested_kind: suggest_kind(a, &bank_name),
            bank_name,
            owner: a.owner.clone(),
            group_name: src
                .group_chain(a.group_key)
                .iter()
                .map(|(_, name)| name.as_str())
                .collect::<Vec<_>>()
                .join(" › "),
            mm_bank_id: a.bank_key,
            is_portfolio: a.portfolio,
            existing_account_id: existing.as_ref().map(|e| e.0),
            existing_account_name: existing.map(|e| e.1),
            booking_count: r.rows.len() as u32,
            pending_count: r.pending,
            holding_count: src.holdings.get(&a.id).map_or(0, |h| h.len() as u32),
            value_history_count: src.value_history.get(&a.id).map_or(0, |h| h.len() as u32),
            statement_count: src.docs.iter().filter(|d| d.account_key == a.id).count() as u32,
            notes_count: n,
            first_date: f,
            last_date: l,
            computed_balance_cents: r.balance_cents,
            balance_source: r.balance_source,
        });
    }
    let mut banks = Vec::new();
    let catalog = psd2_catalog();
    for b in &src.banks {
        let connector = src.connector(b);
        banks.push(MoneyMoneyDbBank {
            mm_id: b.id,
            bank_code: b.bankcode.clone(),
            name: b.name.clone(),
            login_name: b.username.clone(),
            customer_id: b.customer.clone(),
            connector,
            existing_connection_id: match connector {
                Some(k) => {
                    let key = src
                        .connector_key(b)
                        .map(|(_, key)| key)
                        .unwrap_or_else(|| k.as_str().to_string());
                    find_connection(conn, k, &key, &b.username)?
                }
                None => None,
            },
            account_count: src
                .accounts
                .iter()
                .filter(|a| a.bank_key == Some(b.id))
                .count() as u32,
            connection_options: connection_options(src, b, &catalog),
        });
    }
    let known: HashSet<i64> = src.accounts.iter().map(|a| a.id).collect();
    let listed: Vec<&SrcDoc> = src
        .docs
        .iter()
        .filter(|d| known.contains(&d.account_key))
        .collect();
    let (mut found, mut bytes) = (0u32, 0u64);
    if let Some(files) = files.as_mut() {
        for d in &listed {
            if let Some(f) = files.find(d) {
                found += 1;
                bytes += std::fs::metadata(&f).map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    Ok(MoneyMoneyDbPreview {
        database_path,
        statement_count: listed.len() as u32,
        statements_dir: files.map(|f| f.root.to_string_lossy().into_owned()),
        statements_found: found,
        statements_bytes: bytes,
        accounts,
        banks,
        category_paths: src.categories.order.iter().map(|p| p.join(" › ")).collect(),
        total_bookings: total,
        pending_bookings: pending,
        notes_count: notes,
        split_parts: parts,
        date_range_start: first,
        date_range_end: last,
        replaces_starter_categories: replaces_starter_categories(conn, src)?,
        transfer_template_count: templates::usable_count(&src.templates),
    })
}

/// "Is this document already in the archive?" – `source_url != ''` repeats
/// the condition of the partial index `booking_documents_source`; without
/// it SQLite can't use that index for a bound value and scans the whole
/// table, reading through every stored PDF (`source_url` comes after the
/// blob), once per imported document.
const DOCUMENT_EXISTS_SQL: &str =
    "SELECT EXISTS (SELECT 1 FROM booking_documents WHERE source_url = ?1 AND source_url != '')";

/// Copies one listed statement / bank document into the archive of
/// `account_id`. Idempotent per MoneyMoney account and file content.
fn import_document(
    conn: &Connection,
    mm_account: i64,
    account_id: i64,
    d: &SrcDoc,
    files: Option<&mut StatementFiles>,
    res: &mut MoneyMoneyDbImportResult,
) -> CoreResult<()> {
    let key = |digest: &str| format!("moneymoney:{mm_account}:{digest}");
    let exists = |digest: &str| -> CoreResult<bool> {
        Ok(conn.query_row(DOCUMENT_EXISTS_SQL, [key(digest)], |r| r.get(0))?)
    };
    // already there: don't read the file again
    if !d.digest.is_empty() && exists(&d.digest)? {
        res.documents_existing += 1;
        return Ok(());
    }
    let Some(path) = files.and_then(|f| f.find(d)) else {
        res.documents_missing += 1;
        return Ok(());
    };
    if std::fs::metadata(&path).map_or(true, |m| m.len() > MAX_PDF_BYTES) {
        res.documents_too_large += 1;
        return Ok(());
    }
    let Ok(bytes) = std::fs::read(&path) else {
        res.documents_missing += 1;
        return Ok(());
    };
    let digest = sha256_hex(&bytes);
    if !bytes.starts_with(b"%PDF") || (!d.digest.is_empty() && d.digest != digest) {
        res.documents_mismatched += 1;
        return Ok(());
    }
    let date = d.created_ts.and_then(unix_to_date).unwrap_or_default();
    let file = d.filename.trim().replace(['/', '\\'], "-");
    let added = crate::documents::insert_doc(
        conn,
        None,
        Some(account_id),
        &crate::documents::title_from_file(&file),
        doc_type_for(&file),
        "application/pdf",
        &file,
        &bytes,
        &key(&digest),
        &date,
    )?;
    if added.is_some() {
        res.documents_imported += 1;
    } else {
        res.documents_existing += 1;
    }
    Ok(())
}

/// Priority of the first imported category rule: after GiroCrab's own rules
/// (default 100), in MoneyMoney's category order.
const IMPORTED_RULE_PRIORITY: i32 = 1000;

/// One search-expression rule per MoneyMoney category rule. Rules GiroCrab
/// can't apply as they are (unknown fields, an account restriction, invalid
/// syntax) are kept but switched off. A rule already present for the same
/// category and expression is skipped, so a re-import adds nothing.
fn import_category_rules(
    conn: &Connection,
    src: &Source,
    cat_cache: &mut HashMap<String, i64>,
    res: &mut MoneyMoneyDbImportResult,
) -> CoreResult<()> {
    for (i, (path, expr, restricted)) in src.categories.rules.iter().enumerate() {
        let category_id =
            find_or_create_category(conn, path, cat_cache, &mut res.categories_created)?;
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM rules WHERE category_id = ?1 AND pattern = ?2 AND deleted_ms IS NULL)",
            params![category_id, expr],
            |r| r.get(0),
        )?;
        if exists {
            continue;
        }
        let usable = crate::search_expr::SearchExpr::parse_supported(expr).is_ok();
        let enabled = usable && !restricted;
        let leaf = path.last().cloned().unwrap_or_default();
        // GiroCrab can't map the restriction to accounts yet: say so in the
        // name, so nobody switches the rule on for all accounts unawares.
        let name = if *restricted {
            trf("{0} (nur für bestimmte Konten)", &[&leaf])
        } else {
            leaf
        };
        conn.execute(
            "INSERT INTO rules(name, category_id, field, match_kind, pattern, priority, enabled, updated_ms)
             VALUES (?1, ?2, 'any', 'expression', ?3, ?4, ?5, ?6)",
            params![
                name,
                category_id,
                expr,
                IMPORTED_RULE_PRIORITY + i as i32,
                enabled,
                now_ms()
            ],
        )?;
        res.rules_created += 1;
        if !enabled {
            res.rules_disabled += 1;
        }
    }
    crate::sync_merge::fill_missing_uuids(conn)?;
    Ok(())
}

/// Reads an imported account back from the vault and compares it with
/// MoneyMoney: every booking matched by its fingerprint (as a multiset, like
/// the import itself), and the balance.
fn check_account(
    conn: &Connection,
    a: &SrcAccount,
    name: &str,
    account_id: i64,
    r: &AccountRows,
    balance_from_bank: bool,
) -> CoreResult<MoneyMoneyDbAccountCheck> {
    // every MoneyMoney row matched once (multiset): exact fingerprint, plus a
    // bank booking without counterparty the import took as the same booking
    use super::moneymoney_import::{
        load_existing, match_bank_without_counterparty, match_by_fingerprint, match_folded_name,
    };
    let existing = load_existing(conn, account_id)?;
    let rows = with_known_parts(&r.rows, || KnownKeys::from_existing(&existing));
    let (mut matched, mut used) = match_by_fingerprint(&rows, &existing);
    match_folded_name(&rows, &existing, &mut matched, &mut used);
    match_bank_without_counterparty(&rows, &existing, &mut matched, &mut used);
    let live_total = existing.iter().filter(|e| !e.deleted).count() as u32;
    let (mut found, mut deleted) = (0u32, 0u32);
    for j in matched.into_iter().flatten() {
        if existing[j].deleted {
            deleted += 1;
        } else {
            found += 1;
        }
    }
    let mm_bookings = rows.len() as u32;
    let girocrab_balance_cents: i64 = conn.query_row(
        "SELECT balance_cents FROM accounts WHERE id = ?1",
        [account_id],
        |row| row.get(0),
    )?;
    let complete = found + deleted == mm_bookings;
    let balance_ok = balance_from_bank || girocrab_balance_cents == r.balance_cents;
    Ok(MoneyMoneyDbAccountCheck {
        mm_id: a.id,
        account_id,
        name: name.to_string(),
        currency: a.currency.clone(),
        mm_bookings,
        found_bookings: found,
        deleted_bookings: deleted,
        other_bookings: live_total.saturating_sub(found),
        mm_balance_cents: r.balance_cents,
        mm_balance_shown: r.balance_shown,
        girocrab_balance_cents,
        balance_from_bank,
        ok: complete && balance_ok,
    })
}

fn import(
    conn: &Connection,
    src: &Source,
    options: &MoneyMoneyDbImportOptions,
    files: Option<StatementFiles>,
    observer: Option<&dyn MoneyMoneyImportObserver>,
) -> CoreResult<MoneyMoneyDbImportResult> {
    // what there is to do, for the progress bar
    let mut plan = Vec::new();
    for choice in options.accounts.iter().filter(|c| c.import) {
        let a = src
            .accounts
            .iter()
            .find(|a| a.id == choice.mm_id)
            .ok_or_else(|| CoreError::NotFound {
                what: format!("Konto {}", choice.mm_id),
            })?;
        plan.push((choice, a, src.rows_for(a)));
    }
    let mut report = Reporter {
        observer,
        state: MoneyMoneyDbImportProgress {
            account_count: plan.len() as u32,
            bookings_total: plan.iter().map(|(_, _, r)| r.rows.len() as u32).sum(),
            documents_total: if options.import_statements {
                plan.iter()
                    .map(|(_, a, _)| {
                        src.docs.iter().filter(|d| d.account_key == a.id).count() as u32
                    })
                    .sum()
            } else {
                0
            },
            ..Default::default()
        },
    };
    report.emit()?;
    let mut cx = ImportCtx {
        conn,
        src,
        options,
        report,
        files,
        cat_cache: HashMap::new(),
        res: MoneyMoneyDbImportResult::default(),
        to_link: Vec::new(),
        created_groups: Vec::new(),
    };
    if options.import_categories {
        cx.import_categories()?;
    }
    for (choice, a, r) in plan {
        cx.report.state.account_index += 1;
        cx.report.state.account_name = src.account_name(a);
        cx.report.emit()?;
        cx.import_account(choice, a, &r)?;
    }
    cx.order_created_groups()?;

    cx.report.state.account_index += 1;
    cx.report.state.account_name.clear();
    cx.report.emit()?;
    if let (true, Some(files)) = (options.import_statements, cx.files.as_mut()) {
        cx.res.documents_unlisted = files.unlisted();
    }

    cx.connect_banks()?;
    if options.import_transfer_templates {
        let (created, existing, skipped) = templates::import_templates(conn, &src.templates)?;
        cx.res.transfer_templates_created = created;
        cx.res.transfer_templates_existing = existing;
        cx.res.transfer_templates_skipped = skipped;
    }
    Vault::apply_rules_inner(conn, true)?;
    Ok(cx.res)
}

struct ImportCtx<'a> {
    conn: &'a Connection,
    src: &'a Source,
    options: &'a MoneyMoneyDbImportOptions,
    report: Reporter<'a>,
    files: Option<StatementFiles>,
    cat_cache: HashMap<String, i64>,
    res: MoneyMoneyDbImportResult,
    /// (account id, MoneyMoney bank rowid) of the importer's own accounts,
    /// linked to the bank's login once the connections exist
    to_link: Vec<(i64, i64)>,
    /// (MoneyMoney group rowid, uuid) of the groups this import creates
    created_groups: Vec<(i64, String)>,
}

impl ImportCtx<'_> {
    fn import_categories(&mut self) -> CoreResult<()> {
        let conn = self.conn;
        if replaces_starter_categories(conn, self.src)? {
            // soft: rules and assignments go with them
            let roots: Vec<i64> = conn
                .prepare(
                    "SELECT id FROM categories WHERE parent_id IS NULL AND deleted_ms IS NULL",
                )?
                .query_map([], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            for id in roots {
                crate::sync_merge::soft_delete_categories(conn, id)?;
            }
        }
        // the whole tree, also categories without bookings
        for path in &self.src.categories.order {
            find_or_create_category(
                conn,
                path,
                &mut self.cat_cache,
                &mut self.res.categories_created,
            )?;
        }
        apply_category_colors(conn, self.src, &self.cat_cache)?;
        import_category_rules(conn, self.src, &mut self.cat_cache, &mut self.res)
    }

    fn import_account(
        &mut self,
        choice: &MoneyMoneyDbAccountChoice,
        a: &SrcAccount,
        r: &AccountRows,
    ) -> CoreResult<()> {
        let (conn, src, options) = (self.conn, self.src, self.options);
        let (account_id, currency) = self.target_account(choice, a)?;
        // Accounts owned by the importer (not synced with a bank) follow MoneyMoney:
        // chosen kind and MoneyMoney's balance. Bank-synced accounts keep theirs.
        let owned: bool = conn.query_row(
            "SELECT external_ref LIKE 'moneymoney-db:%' FROM accounts WHERE id = ?1",
            [account_id],
            |r| r.get(0),
        )?;
        let mut st = Stats {
            total: r.rows.len() as u32,
            account_id,
            ..Default::default()
        };
        merge_rows(
            conn,
            &r.rows,
            &currency,
            options.import_categories,
            options.import_notes,
            &mut self.cat_cache,
            &mut st,
        )?;
        self.report.state.bookings_done += r.rows.len() as u32;
        self.report.emit()?;
        if owned {
            self.follow_moneymoney(account_id, choice.kind, a, r)?;
        }
        if let Some(rows) = src.value_history.get(&a.id) {
            self.res.value_history_imported += observations::record_import(conn, account_id, rows)?;
            crate::depot_values::rebuild(conn, account_id, None)?;
        }
        if options.import_groups {
            self.place_in_groups(account_id, a)?;
        }
        self.res.bookings_new += st.new;
        self.res.bookings_duplicates += st.duplicates;
        self.res.bookings_enriched += st.enriched;
        self.res.notes_imported += st.notes;
        self.res.categories_created += st.categories_created;
        self.res.pending_skipped += r.pending;
        if options.import_statements {
            for d in src.docs.iter().filter(|d| d.account_key == a.id) {
                import_document(
                    conn,
                    a.id,
                    account_id,
                    d,
                    self.files.as_mut(),
                    &mut self.res,
                )?;
                self.report.state.documents_done += 1;
                self.report.emit()?;
            }
        }
        self.res.account_checks.push(check_account(
            conn,
            a,
            &src.account_name(a),
            account_id,
            r,
            !owned,
        )?);
        if let (true, Some(bank)) = (owned, a.bank_key) {
            self.to_link.push((account_id, bank));
        }
        Ok(())
    }

    /// The account the MoneyMoney account goes into: the chosen one, the one an
    /// earlier import created (revived if deleted), else a new one.
    /// Returns (account id, currency).
    fn target_account(
        &mut self,
        choice: &MoneyMoneyDbAccountChoice,
        a: &SrcAccount,
    ) -> CoreResult<(i64, String)> {
        let (conn, src) = (self.conn, self.src);
        let ext = ext_ref(a);
        let kind = choice.kind;
        let target = match choice.target_account_id {
            Some(id) => Some(id),
            None => conn
                .query_row(
                    // includes soft-deleted rows: revived below
                    "SELECT id FROM accounts WHERE external_ref = ?1 ORDER BY id LIMIT 1",
                    [&ext],
                    |r| r.get::<_, i64>(0),
                )
                .optional()?
                .map(|id| {
                    crate::sync_merge::twins::folded_into(conn, id).map(|bank| bank.unwrap_or(id))
                })
                .transpose()?,
        };
        if let Some(id) = target {
            let deleted: bool = conn
                .query_row(
                    "SELECT deleted_ms IS NOT NULL FROM accounts WHERE id = ?1",
                    [id],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or(false);
            crate::sync_merge::revive_account_if_deleted(conn, id)?;
            // Nobody chose the archive state of a revived account (it comes back
            // active) or of one the #998 repair reactivated: MoneyMoney's applies.
            let unknown = take_archive_unknown(conn, id)?;
            if a.closed && (deleted || unknown) {
                archive_closed(conn, id)?;
            }
            let cur: String = conn
                .query_row("SELECT currency FROM accounts WHERE id = ?1", [id], |r| {
                    r.get(0)
                })
                .optional()?
                .ok_or_else(|| CoreError::NotFound {
                    what: format!("account {id}"),
                })?;
            self.res.accounts_merged += 1;
            return Ok((id, cur));
        }
        let (group, sym, order) = group_for(kind);
        let gid = Vault::ensure_group(conn, group, sym, order)?;
        let bank_name = src
            .bank(a.bank_key)
            .map(|b| b.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "Datenimport".into());
        conn.execute(
            "INSERT INTO accounts(connection_id, group_id, external_ref, name, bank_name, owner, iban, account_number,
                                  currency, kind, balance_cents, balance_date, sf_symbol, sort_order, created_ms)
             VALUES (NULL, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, '', ?10,
                     (SELECT IFNULL(MAX(sort_order),0)+1 FROM accounts), ?11)",
            params![
                gid,
                ext,
                src.account_name(a),
                bank_name,
                a.owner,
                a.iban,
                a.account,
                a.currency,
                kind.as_str(),
                kind.sf_symbol(),
                now_ms()
            ],
        )?;
        self.res.accounts_created += 1;
        let id = conn.last_insert_rowid();
        // Closed in MoneyMoney: archived here. Only a new account: a re-import
        // leaves the archive state the user set (#890).
        if a.closed {
            archive_closed(conn, id)?;
        }
        Ok((id, a.currency.clone()))
    }

    /// An account the importer owns takes MoneyMoney's kind, balance, opening
    /// balance and positions.
    fn follow_moneymoney(
        &mut self,
        account_id: i64,
        kind: AccountKind,
        a: &SrcAccount,
        r: &AccountRows,
    ) -> CoreResult<()> {
        let conn = self.conn;
        let (group, sym, order) = group_for(kind);
        let gid = Vault::ensure_group(conn, group, sym, order)?;
        conn.execute(
            "UPDATE accounts SET kind = ?1, sf_symbol = CASE WHEN symbol_ms > 0 THEN sf_symbol ELSE ?2 END, group_id = ?3, balance_cents = ?4, balance_date = ?5 WHERE id = ?6",
            params![kind.as_str(), kind.sf_symbol(), gid, r.balance_cents, r.balance_date, account_id],
        )?;
        if kind == AccountKind::Loan {
            Vault::derive_initial_balance(conn, account_id)?;
        }
        // #212: MoneyMoney's opening balance becomes the account's. MoneyMoney
        // has no cut-off date: the Startsaldo applies before the first
        // booking. A balance-history row is dated, though (its value is the
        // balance at that point, a date that may lie after every booking),
        // so the undated amount is calibrated to the balance MoneyMoney
        // shows or the one computed above: after an import the balance is
        // MoneyMoney's, as the account check expects.
        if r.opening && !matches!(kind, AccountKind::Brokerage | AccountKind::Loan) {
            crate::opening_balance::store(conn, account_id, Some(0), "")?;
            crate::opening_balance::calibrate_in(conn, account_id, r.balance_cents)?;
        }
        // Positions are a snapshot: always replace, so a depot whose positions
        // were all sold in MoneyMoney ends up empty here too.
        conn.execute("DELETE FROM holdings WHERE account_id = ?1", [account_id])?;
        if let Some(list) = self.src.holdings.get(&a.id) {
            for source in list {
                let h = &source.holding;
                conn.execute(
                    "INSERT INTO holdings(account_id, isin, wkn, name, quantity, current_price_cents, purchase_price_cents, value_cents)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![account_id, h.isin, h.wkn, h.name, h.quantity, h.current_price_cents, h.purchase_price_cents, h.value_cents],
                )?;
                conn.execute(
                    "INSERT INTO holding_classifications(holding_id,asset_class,instrument) VALUES (?1,?2,?3)",
                    params![conn.last_insert_rowid(),source.classification.asset_class.map(AssetClass::as_str),source.classification.instrument.map(InstrumentKind::as_str)],
                )?;
            }
            self.res.holdings_imported += list.len() as u32;
        }
        Ok(())
    }

    /// Puts an account the user never placed (an explicit "Ohne Gruppe" is
    /// kept, too) into the MoneyMoney groups it is in, created as needed.
    fn place_in_groups(&mut self, account_id: i64, a: &SrcAccount) -> CoreResult<()> {
        let conn = self.conn;
        let a_groups = self.src.group_chain(a.group_key);
        if a_groups.is_empty() {
            return Ok(());
        }
        let ungrouped: bool = conn.query_row(
            "SELECT user_group_uuid IS NULL AND placement_ms = 0 FROM accounts WHERE id = ?1",
            [account_id],
            |r| r.get(0),
        )?;
        if !ungrouped {
            return Ok(());
        }
        let now = now_ms();
        // the MoneyMoney groups from the top level down to the account's own
        let mut parent: Option<String> = None;
        for (key, name) in &a_groups {
            let (uuid, created) = crate::user_groups::find_or_create(conn, name, "folder")?;
            if created {
                self.res.groups_created += 1;
                self.created_groups.push((*key, uuid.clone()));
                // only new groups are nested: one the user already has stays where it is
                if let Some(p) = parent.as_deref().filter(|p| *p != uuid) {
                    crate::user_groups::set_parent(conn, &uuid, Some(p), now)?;
                }
            }
            parent = Some(uuid);
        }
        crate::user_groups::assign(conn, account_id, parent.as_deref(), now)
    }

    /// The new groups take MoneyMoney's order among themselves: they share
    /// out the list positions they got (existing groups keep theirs).
    fn order_created_groups(&mut self) -> CoreResult<()> {
        if self.created_groups.len() < 2 {
            return Ok(());
        }
        let conn = self.conn;
        let rank = self.src.group_ranks();
        let mut slots: Vec<i64> = Vec::with_capacity(self.created_groups.len());
        for (_, uuid) in &self.created_groups {
            slots.push(conn.query_row(
                "SELECT sort_order FROM user_groups WHERE uuid = ?1",
                [uuid],
                |r| r.get(0),
            )?);
        }
        slots.sort_unstable();
        self.created_groups
            .sort_by_key(|(key, _)| rank.get(key).copied().unwrap_or(usize::MAX));
        let now = now_ms();
        for ((_, uuid), slot) in self.created_groups.iter().zip(slots) {
            conn.execute(
                "UPDATE user_groups SET sort_order = ?2, order_ms = ?3, updated_ms = max(updated_ms, ?3)
                 WHERE uuid = ?1 AND sort_order != ?2",
                params![uuid, slot, now],
            )?;
        }
        Ok(())
    }

    /// Finds or creates the bank logins the user chose, then links the
    /// imported accounts and lists the Enable Banking connections.
    fn connect_banks(&mut self) -> CoreResult<()> {
        let (conn, src, options) = (self.conn, self.src, self.options);
        // MoneyMoney bank rowid -> connection id
        let mut bank_connection: HashMap<i64, i64> = HashMap::new();
        // Enable Banking connections set up here: (connection id, bank name, created)
        let mut psd2 = Vec::new();
        for b in &src.banks {
            let default = src
                .connector_key(b)
                .map(|(kind, key)| MoneyMoneyDbConnectionOption::direct(kind, key));
            if default.is_none() {
                // counted unless the user connects it through Enable Banking below
                self.res.banks_skipped_unsupported += 1;
                self.res.banks_skipped_names.push(b.name.clone());
            }
            if !options.create_connections {
                continue;
            }
            let chosen = match options.bank_choices.iter().find(|c| c.mm_bank_id == b.id) {
                Some(c) => c.connection.clone(),
                None => default,
            };
            let Some(chosen) = chosen else { continue };
            if chosen.via == MoneyMoneyDbConnectionVia::EnableBanking {
                let Some(bank) = chosen.psd2_bank.as_ref() else {
                    continue;
                };
                if let Some((id, created)) = psd2_connection(conn, b, &bank.aspsp, &bank.country)? {
                    self.count_connection(created);
                    if src.connector_key(b).is_none() {
                        self.res.banks_skipped_unsupported -= 1;
                        self.res.banks_skipped_names.pop();
                    }
                    bank_connection.insert(b.id, id);
                    psd2.push((id, b.name.clone(), created));
                }
                continue;
            }
            let id = self.direct_connection(b, chosen.connector, &chosen.key)?;
            bank_connection.insert(b.id, id);
        }
        self.link_accounts(&bank_connection)?;
        self.list_psd2_connections(psd2)
    }

    fn count_connection(&mut self, created: bool) {
        if created {
            self.res.connections_created += 1;
        } else {
            self.res.connections_existing += 1;
        }
    }

    /// The login of a bank with its own connector: an existing one, else a new one.
    fn direct_connection(
        &mut self,
        b: &SrcBank,
        kind: ConnectorKind,
        key: &str,
    ) -> CoreResult<i64> {
        let conn = self.conn;
        // FinTS login (#1377): take over the FinTS endpoint URL MoneyMoney
        // stores in `banks.server` so the connection's settings show it
        // immediately – without this the user would have to copy it from the
        // bank's DK list by hand. comdirect per FinTS has no entry in the
        // generated directory, so its fixed address is used as a fallback
        // when MoneyMoney did not store a URL (e.g. PSD2 import).
        // `ON CONFLICT DO NOTHING` protects a URL the user already adjusted.
        if let Some(blz) = ConnectorKind::fints_blz(key) {
            let server = b.server.trim();
            let url: Option<&str> = if crate::connectors::fints_presets::is_valid_fints_url(server)
            {
                Some(server)
            } else if crate::connectors::fints_directory::COMDIRECT_BLZ.contains(&blz) {
                Some(crate::connectors::fints_directory::COMDIRECT_FINTS_URL)
            } else {
                None
            };
            if let Some(url) = url {
                conn.execute(
                    "INSERT INTO settings(key, value, is_secret) VALUES (?1, ?2, 0)
                     ON CONFLICT(key) DO NOTHING",
                    params![format!("fints.url.{blz}"), url],
                )?;
            }
        }
        if let Some(id) = find_connection(conn, kind, key, &b.username)? {
            self.count_connection(false);
            return Ok(id);
        }
        let (id, created) = Vault::add_bank_connection_keyed(
            conn,
            kind,
            key,
            ConnectorKind::fints_blz(key),
            b.name.clone(),
            b.username.clone(),
        )?;
        self.count_connection(created);
        Ok(id)
    }

    /// Shows imported accounts under their bank login right away; the first
    /// sync then takes them over (adopt_account) instead of adding new ones.
    fn link_accounts(&self, bank_connection: &HashMap<i64, i64>) -> CoreResult<()> {
        for (account_id, bank) in &self.to_link {
            if let Some(cid) = bank_connection.get(bank) {
                self.conn.execute(
                    "UPDATE accounts SET connection_id = ?1 WHERE id = ?2 AND connection_id IS NULL",
                    params![cid, account_id],
                )?;
            }
        }
        Ok(())
    }

    /// Lists the Enable Banking connections with their accounts, for the
    /// prompt to authorise them.
    fn list_psd2_connections(&mut self, psd2: Vec<(i64, String, bool)>) -> CoreResult<()> {
        if psd2.is_empty() {
            return Ok(());
        }
        let mut st = self.conn.prepare(
            "SELECT name, iban, account_number FROM accounts
             WHERE connection_id = ?1 AND deleted_ms IS NULL ORDER BY sort_order, id",
        )?;
        for (connection_id, bank_name, created) in psd2 {
            let accounts = st
                .query_map([connection_id], |r| {
                    Ok(MoneyMoneyDbPsd2Account {
                        name: r.get(0)?,
                        iban: r.get(1)?,
                        account_number: r.get(2)?,
                    })
                })?
                .collect::<Result<_, _>>()?;
            self.res.psd2_connections.push(MoneyMoneyDbPsd2Connection {
                connection_id,
                bank_name,
                created,
                accounts,
            });
        }
        Ok(())
    }
}

/// Enable Banking connection for a MoneyMoney bank without own connector:
/// the existing one for that ASPSP, else a new one. `None` when the banking
/// lock blocks the bank or the choice is invalid (the bank is skipped).
fn psd2_connection(
    conn: &Connection,
    b: &SrcBank,
    aspsp: &str,
    country: &str,
) -> CoreResult<Option<(i64, bool)>> {
    let aspsp = aspsp.trim();
    let country = country.trim().to_uppercase();
    let key = ConnectorKind::enable_banking_key(&country, aspsp);
    if aspsp.is_empty() || ConnectorKind::enable_banking_target(&key).is_none() {
        return Ok(None);
    }
    if crate::banking_gate::check_connector(&key).is_err() {
        return Ok(None);
    }
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM bank_connections WHERE connector = ?1 AND deleted_ms IS NULL ORDER BY id LIMIT 1",
            [&key],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = existing {
        return Ok(Some((id, false)));
    }
    let display = if b.name.trim().is_empty() {
        aspsp.to_string()
    } else {
        b.name.clone()
    };
    let (id, created) = Vault::add_bank_connection_keyed(
        conn,
        ConnectorKind::EnableBanking,
        &key,
        None,
        display,
        aspsp.to_string(),
    )?;
    Ok(Some((id, created)))
}

#[uniffi::export]
impl Vault {
    /// Dry run of a MoneyMoney import: reads the database read-only and
    /// lists accounts, bank logins, categories and documents. Nothing is
    /// stored
    ///
    /// `path` is MoneyMoney's data folder (the database and its `Statements`
    /// folder are found in it) or a single database file, e.g. a backup.
    /// `statements_dir`: folder with the statement PDFs, if they are not in
    /// the MoneyMoney folder.
    pub fn preview_moneymoney_database(
        &self,
        path: String,
        password: String,
        statements_dir: Option<String>,
    ) -> Result<MoneyMoneyDbPreview, CoreError> {
        let password = SecretString::new(password);
        let (db_path, found) = locate(&path)?;
        let src = read_source(&db_path, &password)?;
        drop(password);
        let files = statement_files(statements_dir, found);
        let db = self.db()?;
        preview(&db, &src, db_path, files)
    }

    /// Imports from MoneyMoney in one go (accounts, bookings, categories,
    /// notes, optionally bank logins and statement PDFs).
    /// Idempotent. `path` as for `preview_moneymoney_database`.
    pub fn import_moneymoney_database(
        &self,
        path: String,
        password: String,
        options: MoneyMoneyDbImportOptions,
    ) -> Result<MoneyMoneyDbImportResult, CoreError> {
        self.import_moneymoney(path, password, options, None)
    }

    /// `import_moneymoney_database` that reports its progress to `observer`
    /// and can be cancelled by it. A cancelled import stores nothing (the
    /// whole import is one transaction) and returns an error.
    pub fn import_moneymoney_database_with_progress(
        &self,
        path: String,
        password: String,
        options: MoneyMoneyDbImportOptions,
        observer: Arc<dyn MoneyMoneyImportObserver>,
    ) -> Result<MoneyMoneyDbImportResult, CoreError> {
        self.import_moneymoney(path, password, options, Some(observer.as_ref()))
    }
}

impl Vault {
    fn import_moneymoney(
        &self,
        path: String,
        password: String,
        options: MoneyMoneyDbImportOptions,
        observer: Option<&dyn MoneyMoneyImportObserver>,
    ) -> CoreResult<MoneyMoneyDbImportResult> {
        let password = SecretString::new(password);
        let (db_path, found) = locate(&path)?;
        // "reading the database" until the accounts start
        if let Some(o) = observer
            && !o.progress(MoneyMoneyDbImportProgress::default())
        {
            return Err(cancelled());
        }
        let src = read_source(&db_path, &password)?;
        drop(password);
        let files = statement_files(options.statements_dir.clone(), found);
        let db = self.db()?;
        let tx = db.unchecked_transaction()?;
        // dropped without commit on error or cancel: nothing is stored
        let res = import(&tx, &src, &options, files, observer)?;
        tx.commit()?;
        // The import is one big transaction (bookings plus statement PDFs), so
        // the WAL now holds all of it. Fold it into the main file and truncate
        // it; best effort, the data is committed either way.
        let _ = db.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
        Ok(res)
    }
}

/// The folder to take PDFs from: the explicit one, else the one found.
fn statement_files(explicit: Option<String>, found: Option<PathBuf>) -> Option<StatementFiles> {
    explicit
        .filter(|d| !d.trim().is_empty())
        .map(PathBuf::from)
        .filter(|d| d.is_dir())
        .or(found)
        .map(StatementFiles::new)
}

#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
mod closed_account_tests {
    use super::tests::{PW, fake_db, options_from};
    use super::*;

    /// The fake database with `sql` run on it.
    fn db_with(sql: &str) -> String {
        let path = fake_db();
        let c = Connection::open(&path).unwrap();
        c.execute_batch(&format!("PRAGMA key = '{}';", PW.replace('\'', "''")))
            .unwrap();
        c.execute_batch(sql).unwrap();
        path
    }

    fn vault(name: &str) -> std::sync::Arc<Vault> {
        Vault::open(
            crate::test_support::thread_temp_path(name)
                .to_string_lossy()
                .to_string(),
            vec![7u8; 32],
        )
        .unwrap()
    }

    fn import(v: &Vault, path: &str) {
        let p = v
            .preview_moneymoney_database(path.to_string(), PW.into(), None)
            .unwrap();
        v.import_moneymoney_database(path.to_string(), PW.into(), options_from(&p, false))
            .unwrap();
    }

    fn account(v: &Vault, name: &str) -> crate::models::Account {
        v.list_accounts()
            .unwrap()
            .into_iter()
            .find(|a| a.name == name)
            .unwrap()
    }

    #[test]
    fn account_with_a_cancelled_refresh_is_imported_active() {
        // #998: `cancel_tsp` is when MoneyMoney's last refresh of the account was
        // cancelled (next to `error_tsp`, `balance_tsp` …), e.g. a TAN not confirmed
        // after the start-up refresh. Such an account is in use, not closed.
        let path = db_with("UPDATE accounts SET cancel_tsp = 1700000000 WHERE rowid IN (1, 2);");
        let v = vault("mmdb-cancelled-vault.db");
        import(&v, &path);
        for name in ["Girokonto", "Visa-Karte"] {
            assert!(account(&v, name).archived_unix.is_none(), "{name}");
            assert!(account(&v, name).in_total, "{name}");
        }
    }

    #[test]
    fn account_closed_in_moneymoney_is_imported_archived() {
        // bit 16 of `refresh` is the closed flag; the low bits are refresh settings
        let path = db_with(
            "UPDATE accounts SET refresh = 19 WHERE rowid = 2;
             UPDATE accounts SET refresh = 3 WHERE rowid = 1;",
        );
        let v = vault("mmdb-closed-vault.db");
        import(&v, &path);
        let card = account(&v, "Visa-Karte");
        assert!(card.archived_unix.is_some());
        assert!(!card.in_total);
        assert!(!bookings_of(&v, card.id).is_empty(), "bookings imported");
        assert!(account(&v, "Girokonto").archived_unix.is_none());
        // stamped in one second like an archive in the app: the #998 repair
        // (schema step 34) must never take it for a wrong one
        let (unix, ms): (i64, i64) = v
            .db()
            .unwrap()
            .query_row(
                "SELECT archived_unix, archived_ms FROM accounts WHERE id = ?1",
                [card.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(
            unix * 1000 >= ms - crate::schema::ARCHIVE_STAMP_GAP_MS,
            "{unix} {ms}"
        );
    }

    #[test]
    fn reimport_keeps_the_archive_state_of_existing_accounts() {
        // #890: the state in GiroCrab is the user's choice, whatever MoneyMoney says
        let v = vault("mmdb-reimport-vault.db");
        import(&v, &fake_db());
        let card = account(&v, "Visa-Karte").id;
        let giro = account(&v, "Girokonto").id;
        v.set_account_archived(giro, true).unwrap();
        import(
            &v,
            &db_with(
                "UPDATE accounts SET refresh = 19 WHERE rowid = 2;
                 UPDATE accounts SET refresh = 3 WHERE rowid = 1;",
            ),
        );
        assert!(account(&v, "Visa-Karte").archived_unix.is_none());
        assert_eq!(account(&v, "Visa-Karte").id, card);
        assert!(account(&v, "Girokonto").archived_unix.is_some());
    }

    const CARD_CLOSED: &str = "UPDATE accounts SET refresh = 19 WHERE rowid = 2;
         UPDATE accounts SET refresh = 3 WHERE rowid = 1;";

    #[test]
    fn deleted_closed_account_comes_back_archived() {
        // archived, deleted and imported again: revived, but still closed in MoneyMoney
        let v = vault("mmdb-revive-closed-vault.db");
        import(&v, &db_with(CARD_CLOSED));
        let card = account(&v, "Visa-Karte").id;
        v.delete_account(card).unwrap();
        import(&v, &db_with(CARD_CLOSED));
        let revived = account(&v, "Visa-Karte");
        assert_eq!(revived.id, card);
        assert!(revived.archived_unix.is_some());
        assert!(!bookings_of(&v, card).is_empty());
    }

    #[test]
    fn accounts_the_repair_reactivated_take_moneymoney_state_on_the_next_import() {
        // #998 step 34 cannot tell a closed account: the next import can
        let v = vault("mmdb-unknown-vault.db");
        import(&v, &fake_db());
        let (giro, card) = (account(&v, "Girokonto").id, account(&v, "Visa-Karte").id);
        note_archive_unknown(&v.db().unwrap(), &[giro, card]).unwrap();
        import(&v, &db_with(CARD_CLOSED));
        assert!(account(&v, "Visa-Karte").archived_unix.is_some());
        assert!(account(&v, "Girokonto").archived_unix.is_none());
        assert!(archive_unknown(&v.db().unwrap()).unwrap().is_empty());
        // from now on the user's choice stays (#890)
        v.set_account_archived(card, false).unwrap();
        import(&v, &db_with(CARD_CLOSED));
        assert!(account(&v, "Visa-Karte").archived_unix.is_none());
    }

    fn bookings_of(v: &Vault, account_id: i64) -> Vec<i64> {
        let db = v.db().unwrap();
        let mut st = db
            .prepare("SELECT id FROM transactions WHERE account_id = ?1 AND deleted_ms IS NULL")
            .unwrap();
        st.query_map([account_id], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }
}
