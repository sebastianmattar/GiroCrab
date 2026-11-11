//! The encrypted ledger ("vault"): one SQLCipher database file.
//!
//! Schema (clean-room design, see `SCHEMA` below):
//!   bank_connections  1─n accounts 1─n transactions n─1 categories (tree)
//!   rules             n─1 categories, optional n─1 accounts
//!   settings          key/value, secrets flagged, all inside the encrypted file
//!   account_groups    sidebar grouping by account kind (Giro, Credit cards, ...)
//!   user_groups       user-defined sidebar groups ("Privat", "Geschäft", ...), synced by uuid

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::{params, params_from_iter, Connection, OptionalExtension, ToSql};
use zeroize::Zeroizing;

use crate::error::{CoreError, CoreResult};
use crate::i18n::{display_name, display_path, tr, trf};
use crate::merchants::{classify, clean_purpose, payment_method, FlowKind, MerchantResolver};
use crate::models::*;
use crate::money::parse_cents;
use crate::rules::{RuleInput, RuleSet};

/// Adds merchant, flow kind, payment method and clean purpose to a booking.
pub(crate) fn enrich(t: &mut Booking) {
    t.merchant = MerchantResolver::resolve(&t.counterparty, &t.purpose);
    t.merchant_name = t.merchant.name.clone();
    t.merchant_domain = Some(t.merchant.domain.clone()).filter(|d| !d.is_empty());
    t.merchant_svg_path = Some(t.merchant.svg_path.clone()).filter(|d| !d.is_empty());
    t.merchant_hex_color = t.merchant.color_hex.clone();
    let top = t
        .category_path
        .split(" › ")
        .next()
        .unwrap_or("")
        .to_string();
    t.flow = classify(
        t.amount_cents,
        &t.counterparty,
        &t.purpose,
        &t.booking_text,
        &t.merchant,
        &top,
        t.account_kind,
    );
    t.payment_method = payment_method(&t.booking_text, &t.purpose, t.account_kind, t.amount_cents);
    t.clean_purpose = clean_purpose(&t.purpose);
    t.recurrence_key = crate::recurring::series_key(t);
}

/// Securities positions of depot accounts (replaced on every sync).
const HOLDINGS_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS holdings (
    id                   INTEGER PRIMARY KEY,
    account_id           INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    isin                 TEXT    NOT NULL DEFAULT '',
    wkn                  TEXT    NOT NULL DEFAULT '',
    name                 TEXT    NOT NULL DEFAULT '',
    quantity             REAL    NOT NULL DEFAULT 0,
    current_price_cents  INTEGER NOT NULL DEFAULT 0,
    purchase_price_cents INTEGER NOT NULL DEFAULT 0,
    value_cents          INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS holdings_account ON holdings(account_id);
";

const SCHEMA_VERSION: i64 = 7;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS bank_connections (
    id              INTEGER PRIMARY KEY,
    connector       TEXT    NOT NULL,              -- ConnectorKind::as_str()
    display_name    TEXT    NOT NULL,
    login_name      TEXT    NOT NULL DEFAULT '',
    bank_code       TEXT    NOT NULL DEFAULT '',
    sf_symbol       TEXT    NOT NULL DEFAULT 'building.columns',
    state_json      TEXT    NOT NULL DEFAULT '{}',  -- connector state (FinTS system id, BPD, PSD2 session ...)
    last_sync_unix  INTEGER,
    created_unix    INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);

CREATE TABLE IF NOT EXISTS account_groups (
    id          INTEGER PRIMARY KEY,
    name        TEXT    NOT NULL UNIQUE,
    sf_symbol   TEXT    NOT NULL DEFAULT 'folder',
    sort_order  INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS accounts (
    id              INTEGER PRIMARY KEY,
    connection_id   INTEGER REFERENCES bank_connections(id) ON DELETE SET NULL,
    group_id        INTEGER NOT NULL REFERENCES account_groups(id),
    external_ref    TEXT    NOT NULL DEFAULT '',   -- IBAN / bank account id / PSD2 uid
    name            TEXT    NOT NULL,
    bank_name       TEXT    NOT NULL DEFAULT '',
    owner           TEXT    NOT NULL DEFAULT '',
    iban            TEXT    NOT NULL DEFAULT '',
    account_number  TEXT    NOT NULL DEFAULT '',
    currency        TEXT    NOT NULL DEFAULT 'EUR',
    kind            TEXT    NOT NULL DEFAULT 'checking',
    balance_cents   INTEGER NOT NULL DEFAULT 0,    -- i64 cents
    balance_date    TEXT    NOT NULL DEFAULT '',
    sf_symbol       TEXT    NOT NULL DEFAULT 'building.columns',
    sort_order      INTEGER NOT NULL DEFAULT 0,
    initial_balance_cents INTEGER,                 -- original amount of a Darlehen (i64 cents)
    access_revoked  INTEGER NOT NULL DEFAULT 0,    -- bank / PSD2 consent no longer grants access
    UNIQUE (connection_id, external_ref)
);

CREATE TABLE IF NOT EXISTS categories (
    id            INTEGER PRIMARY KEY,
    parent_id     INTEGER REFERENCES categories(id) ON DELETE CASCADE,
    name          TEXT    NOT NULL,
    sf_symbol     TEXT    NOT NULL DEFAULT 'tag',
    color_hex     TEXT    NOT NULL DEFAULT '#8E8E93',
    budget_cents  INTEGER,                         -- optional monthly budget, i64 cents
    sort_order    INTEGER NOT NULL DEFAULT 0
);
CREATE UNIQUE INDEX IF NOT EXISTS categories_unique_name ON categories(IFNULL(parent_id, 0), name);

CREATE TABLE IF NOT EXISTS rules (
    id                INTEGER PRIMARY KEY,
    name              TEXT    NOT NULL,
    category_id       INTEGER NOT NULL REFERENCES categories(id) ON DELETE CASCADE,
    field             TEXT    NOT NULL DEFAULT 'any',
    match_kind        TEXT    NOT NULL DEFAULT 'keyword',   -- keyword | regex | expression
    pattern           TEXT    NOT NULL,
    case_sensitive    INTEGER NOT NULL DEFAULT 0,
    min_amount_cents  INTEGER,
    max_amount_cents  INTEGER,
    account_id        INTEGER REFERENCES accounts(id) ON DELETE CASCADE,
    priority          INTEGER NOT NULL DEFAULT 100,
    enabled           INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS transactions (
    id                 INTEGER PRIMARY KEY,
    account_id         INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    external_id        TEXT    NOT NULL,           -- de-duplication key from the bank
    booking_date       TEXT    NOT NULL,           -- YYYY-MM-DD
    value_date         TEXT    NOT NULL DEFAULT '',
    amount_cents       INTEGER NOT NULL,           -- i64 cents, negative = debit
    currency           TEXT    NOT NULL DEFAULT 'EUR',
    counterparty       TEXT    NOT NULL DEFAULT '',
    counterparty_iban  TEXT    NOT NULL DEFAULT '',
    purpose            TEXT    NOT NULL DEFAULT '',
    booking_text       TEXT    NOT NULL DEFAULT '',
    category_id        INTEGER REFERENCES categories(id) ON DELETE SET NULL,
    category_source    TEXT    NOT NULL DEFAULT 'none',   -- none | rule | manual
    rule_id            INTEGER REFERENCES rules(id) ON DELETE SET NULL,
    checked            INTEGER NOT NULL DEFAULT 0,
    imported_unix      INTEGER NOT NULL DEFAULT (strftime('%s','now')),
    note               TEXT    NOT NULL DEFAULT '',        -- user note (MoneyMoney import / detail view)
    import_fp          TEXT    NOT NULL DEFAULT '',        -- MoneyMoney de-duplication fingerprint
    purpose_code       TEXT    NOT NULL DEFAULT '',        -- SEPA purpose code (SALA, PENS, …)
    gvc                TEXT    NOT NULL DEFAULT '',        -- German Geschäftsvorfallcode (153, …)
    salary_flag        INTEGER,                            -- NULL = auto, 1 = salary, 0 = not salary (user)
    UNIQUE (account_id, external_id)
);
CREATE INDEX IF NOT EXISTS tx_account_date ON transactions(account_id, booking_date DESC);
CREATE INDEX IF NOT EXISTS tx_date ON transactions(booking_date DESC);
CREATE INDEX IF NOT EXISTS tx_category ON transactions(category_id);
CREATE INDEX IF NOT EXISTS tx_import_fp ON transactions(account_id, import_fp) WHERE import_fp != '';

CREATE TABLE IF NOT EXISTS merchant_logo_cache (
    domain        TEXT PRIMARY KEY,
    image_bytes   BLOB,                            -- PNG/ICO; NULL/empty = nothing found (negative cache)
    updated_at    INTEGER NOT NULL DEFAULT (strftime('%s','now')),
    source        TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS settings (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    is_secret  INTEGER NOT NULL DEFAULT 0
);
"#;

/// Known settings shown in the Settings UI. Nothing is pre-filled: every user
/// registers their own FinTS product and API applications.
/// (key, label, help, group, is_secret, default)
pub const KNOWN_SETTINGS: &[(&str, &str, &str, &str, bool, &str)] = &[
    ("fints.product_id", "FinTS-Produktregistrierung", "Kostenlos bei der Deutschen Kreditwirtschaft registrieren (fints.org › Hersteller › Produktregistrierung). Pflicht für alle FinTS-Banken.", "FinTS 3.0", true, ""),
    ("fints.product_version", "FinTS-Produktversion", "Versionsangabe, die mit HKVVB gesendet wird.", "FinTS 3.0", false, "0.1"),
    ("fints.url.52090000", "Server Volksbank Kassel Göttingen (BLZ 52090000)", "FinTS-PIN/TAN-Adresse laut öffentlicher DK-Institutsliste.", "FinTS 3.0", false, "https://fints1.atruvia.de/cgi-bin/hbciservlet"),
    ("fints.url.50050201", "Server 1822direkt (BLZ 50050201)", "FinTS-PIN/TAN-Adresse laut öffentlicher DK-Institutsliste.", "FinTS 3.0", false, "https://banking-hs5.s-fints-pt-hs.de/fints30"),
    ("comdirect.client_id", "comdirect Client-ID", "Aus dem comdirect-Entwicklerportal (Persönlicher Bereich › Einstellungen › API).", "comdirect REST-API", false, ""),
    ("comdirect.client_secret", "comdirect Client-Secret", "Wird beim Freischalten des API-Zugangs einmalig angezeigt.", "comdirect REST-API", true, ""),
    ("paypal.client_id", "PayPal Client-ID", "REST-App auf developer.paypal.com mit aktivierter „Transaction Search“ anlegen.", "PayPal", false, ""),
    ("paypal.client_secret", "PayPal Secret", "Secret derselben REST-App.", "PayPal", true, ""),
    ("paypal.environment", "PayPal-Umgebung", "live oder sandbox", "PayPal", false, "live"),
    ("bunq.environment", "bunq-Umgebung", "production oder sandbox (Test-API von bunq).", "bunq", false, "production"),
    ("enablebanking.application_id", "Enable Banking Application-ID", "Anwendung auf enablebanking.com registrieren (wird als JWT-„kid“ verwendet).", "Enable Banking (PSD2)", false, ""),
    ("enablebanking.private_key_pem", "Enable Banking Private Key (PEM)", "RSA-Schlüssel der Anwendung; signiert das RS256-JWT.", "Enable Banking (PSD2)", true, ""),
    ("enablebanking.redirect_url", "Redirect-URL", "Muss in Ihrer Enable-Banking-Anwendung freigegeben sein.", "Enable Banking (PSD2)", false, "https://localhost/girocrab/callback"),
    ("enablebanking.aspsp.amex", "ASPSP-Name American Express", "Exakter Name aus GET /aspsps.", "Enable Banking (PSD2)", false, "American Express"),
    ("enablebanking.country.amex", "Land American Express", "Zweistelliger Ländercode.", "Enable Banking (PSD2)", false, "DE"),
    ("enablebanking.aspsp.amazonvisa", "ASPSP-Name Amazon Visa", "Herausgeber in Deutschland: Open Bank S.A. (Marke Zinia). Je nach Listung „Open Bank“ oder „Zinia“ eintragen.", "Enable Banking (PSD2)", false, "Open Bank"),
    ("enablebanking.country.amazonvisa", "Land Amazon Visa", "Zweistelliger Ländercode.", "Enable Banking (PSD2)", false, "DE"),
    // Opt-in per card: the Enable Banking credentials are shared with PSD2
    // banks (C24, N26 …), so having them must not switch the cards away from
    // the web login.
    ("enablebanking.use.amex", "American Express per Enable Banking abrufen", "1 = PSD2-API statt Web-Login (eigene Enable-Banking-Anwendung nötig). Leer oder 0 = Web-Login.", "Enable Banking (PSD2)", false, ""),
    ("enablebanking.use.amazonvisa", "Amazon Visa per Enable Banking abrufen", "1 = PSD2-API statt Web-Login (eigene Enable-Banking-Anwendung nötig). Leer oder 0 = Web-Login.", "Enable Banking (PSD2)", false, ""),
    ("merchants.online_logos", "Händler-Logos online laden", "Anbieter für Händler-Logos: off, duckduckgo oder google. Logos werden verschlüsselt zwischengespeichert; übertragen wird nur die Händler-Domain (z. B. lidl.de). Standard: off.", "Händler-Logos", false, "off"),
    ("sync.psd2_auto_hours", "Automatischer PSD2-Abruf (Stunden)", "Solange GiroCrab läuft, werden PSD2-Bankzugänge (Enable Banking) alle N Stunden ohne TAN abgerufen, solange die Zustimmung gilt. Banken erlauben meist nur 4 Abrufe pro Tag. 0 = aus.", "Allgemein", false, "6"),
    ("sync.days_back", "Tage beim ersten Abruf", "Ältere Umsätze werden nur einmal abgefragt.", "Allgemein", false, "90"),
];

#[derive(uniffi::Object)]
pub struct Vault {
    conn: Mutex<Connection>,
    path: String,
}

impl Vault {
    pub(crate) fn db(&self) -> CoreResult<MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(|_| CoreError::Database {
            message: "vault lock poisoned".into(),
        })
    }

    fn apply_key(conn: &Connection, key: &[u8]) -> CoreResult<()> {
        if key.len() < 16 {
            return Err(CoreError::InvalidInput {
                message: "master key must be at least 16 bytes".into(),
            });
        }
        // Raw key syntax: x'..hex..' -> no PBKDF2 on every open, key comes from Keychain.
        let pragma = Zeroizing::new(format!("PRAGMA key = \"x'{}'\";", hex::encode(key)));
        conn.execute_batch(&pragma)?;
        // Best effort: lock + wipe SQLCipher's in-memory key/page buffers.
        let _ = conn.query_row("PRAGMA cipher_log_level = NONE;", [], |_| Ok(()));
        let _ = conn.query_row("PRAGMA cipher_memory_security = ON;", [], |_| Ok(()));
        Ok(())
    }

    /// True if the file starts with the plaintext SQLite header.
    fn is_plaintext_sqlite(path: &str) -> bool {
        use std::io::Read;
        let mut buf = [0u8; 16];
        std::fs::File::open(path)
            .and_then(|mut f| f.read_exact(&mut buf))
            .map(|_| &buf == b"SQLite format 3\0")
            .unwrap_or(false)
    }

    fn migrate(conn: &Connection) -> CoreResult<()> {
        // Fails with NotADatabase if the key is wrong.
        conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
            r.get::<_, i64>(0)
        })?;
        conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version == 0 {
            conn.execute_batch(SCHEMA)?;
        } else {
            if version == 1 {
                conn.execute_batch(
                    "ALTER TABLE accounts ADD COLUMN initial_balance_cents INTEGER;",
                )?;
            } else if version <= 4 {
                // v5: loan_principal_cents -> initial_balance_cents
                conn.execute_batch("ALTER TABLE accounts RENAME COLUMN loan_principal_cents TO initial_balance_cents;")?;
            }
            if version <= 2 {
                // v3: merchant_logo_cache(domain, image_bytes, updated_at)
                conn.execute_batch(
                    "DROP TABLE IF EXISTS merchant_logo_cache;
                     CREATE TABLE merchant_logo_cache (domain TEXT PRIMARY KEY, image_bytes BLOB,
                        updated_at INTEGER NOT NULL DEFAULT (strftime('%s','now')), source TEXT NOT NULL DEFAULT '');",
                )?;
            }
        }
        // v6: user notes + import fingerprint (idempotent, also for pre-v5 files)
        if version > 0 && version < 6 {
            let has: i64 = conn.query_row(
                "SELECT count(*) FROM pragma_table_info('transactions') WHERE name = 'note'",
                [],
                |r| r.get(0),
            )?;
            if has == 0 {
                conn.execute_batch(
                    "ALTER TABLE transactions ADD COLUMN note TEXT NOT NULL DEFAULT '';
                     ALTER TABLE transactions ADD COLUMN import_fp TEXT NOT NULL DEFAULT '';
                     CREATE INDEX IF NOT EXISTS tx_import_fp ON transactions(account_id, import_fp) WHERE import_fp != '';",
                )?;
            }
        }
        // accounts.access_revoked (PSD2 access withdrawn) – idempotent
        if version > 0 {
            let has: i64 = conn.query_row(
                "SELECT count(*) FROM pragma_table_info('accounts') WHERE name = 'access_revoked'",
                [],
                |r| r.get(0),
            )?;
            if has == 0 {
                conn.execute_batch(
                    "ALTER TABLE accounts ADD COLUMN access_revoked INTEGER NOT NULL DEFAULT 0;",
                )?;
            }
        }
        // transactions.purpose_code / gvc / salary_flag (salary detection) – idempotent
        if version > 0 {
            for (col, ddl) in [
                (
                    "purpose_code",
                    "ALTER TABLE transactions ADD COLUMN purpose_code TEXT NOT NULL DEFAULT '';",
                ),
                (
                    "gvc",
                    "ALTER TABLE transactions ADD COLUMN gvc TEXT NOT NULL DEFAULT '';",
                ),
                (
                    "salary_flag",
                    "ALTER TABLE transactions ADD COLUMN salary_flag INTEGER;",
                ),
            ] {
                let has: i64 = conn.query_row(
                    "SELECT count(*) FROM pragma_table_info('transactions') WHERE name = ?1",
                    [col],
                    |r| r.get(0),
                )?;
                if has == 0 {
                    conn.execute_batch(ddl)?;
                }
            }
        }
        // v4: encrypted documents (Beleg-Abruf & PostBox) – idempotent
        conn.execute_batch(crate::documents::DOC_SCHEMA)?;
        let has_export: i64 = conn.query_row(
            "SELECT count(*) FROM pragma_table_info('booking_documents') WHERE name = 'exported_at'",
            [],
            |r| r.get(0),
        )?;
        if has_export == 0 {
            conn.execute_batch("ALTER TABLE booking_documents ADD COLUMN exported_at INTEGER;")?;
        }
        conn.execute_batch(crate::insights::OVERRIDE_SCHEMA)?;
        conn.execute_batch(HOLDINGS_SCHEMA)?;
        conn.execute_batch(crate::planning::PLANNING_SCHEMA)?;
        conn.execute_batch(crate::recurring::RECURRING_SCHEMA)?;
        // v7: user-defined account groups – idempotent
        crate::user_groups::migrate(conn)?;
        if version < SCHEMA_VERSION {
            conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))?;
        }
        Ok(())
    }

    /// Fills `initial_balance_cents` from the largest historical absolute
    /// balance, reconstructed backwards from the current balance over the
    /// account's own bookings. Never overwrites an existing value.
    pub(crate) fn derive_initial_balance(conn: &Connection, account_id: i64) -> CoreResult<()> {
        let (balance, existing): (i64, Option<i64>) = conn.query_row(
            "SELECT balance_cents, initial_balance_cents FROM accounts WHERE id = ?1",
            [account_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if existing.is_some() {
            return Ok(());
        }
        let mut stmt = conn.prepare("SELECT amount_cents FROM transactions WHERE account_id = ?1 ORDER BY booking_date DESC, id DESC")?;
        let amounts: Vec<i64> = stmt
            .query_map([account_id], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        if amounts.is_empty() {
            return Ok(());
        }
        let mut running = balance;
        let mut max_abs = running.abs();
        for a in amounts {
            running -= a; // balance before this booking
            max_abs = max_abs.max(running.abs());
        }
        if max_abs > 0 {
            conn.execute(
                "UPDATE accounts SET initial_balance_cents = ?1 WHERE id = ?2",
                params![max_abs, account_id],
            )?;
        }
        Ok(())
    }

    pub(crate) fn rule_set(conn: &Connection) -> CoreResult<RuleSet> {
        // Disabled rules never match; skipping them also keeps a switched-off
        // rule with an invalid pattern (e.g. imported) from breaking the set.
        let mut rules = Self::load_rules(conn)?;
        rules.retain(|r| r.enabled);
        RuleSet::new(rules)
    }

    fn load_rules(conn: &Connection) -> CoreResult<Vec<Rule>> {
        let mut stmt = conn.prepare(
            "SELECT r.id, r.name, r.category_id, r.field, r.match_kind, r.pattern, r.case_sensitive,
                    r.min_amount_cents, r.max_amount_cents, r.account_id, r.priority, r.enabled,
                    (SELECT count(*) FROM transactions t WHERE t.rule_id = r.id)
             FROM rules r ORDER BY r.priority, r.id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Rule {
                id: r.get(0)?,
                name: r.get(1)?,
                category_id: r.get(2)?,
                field: RuleField::from_str(&r.get::<_, String>(3)?),
                match_kind: match r.get::<_, String>(4)?.as_str() {
                    "regex" => MatchKind::Regex,
                    "expression" => MatchKind::Expression,
                    _ => MatchKind::Keyword,
                },
                pattern: r.get(5)?,
                case_sensitive: r.get::<_, i64>(6)? != 0,
                min_amount_cents: r.get(7)?,
                max_amount_cents: r.get(8)?,
                account_id: r.get(9)?,
                priority: r.get(10)?,
                enabled: r.get::<_, i64>(11)? != 0,
                match_count: r.get::<_, i64>(12)? as u32,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Re-runs rules over transactions that are not manually categorised.
    /// Returns the number of transactions whose category changed.
    /// Bookings of archived accounts are left alone (GiroCrab/GiroCrab#43).
    pub(crate) fn apply_rules_inner(
        conn: &Connection,
        only_uncategorised: bool,
    ) -> CoreResult<u32> {
        let set = Self::rule_set(conn)?;
        let sql = if only_uncategorised {
            "SELECT t.id, t.account_id, t.amount_cents, t.counterparty, t.counterparty_iban, t.purpose, t.booking_text, t.category_id, t.rule_id,
                    t.note, t.currency, IFNULL(a.iban, '')
             FROM transactions t LEFT JOIN accounts a ON a.id = t.account_id WHERE a.archived_unix IS NULL AND t.category_source = 'none'"
        } else {
            "SELECT t.id, t.account_id, t.amount_cents, t.counterparty, t.counterparty_iban, t.purpose, t.booking_text, t.category_id, t.rule_id,
                    t.note, t.currency, IFNULL(a.iban, '')
             FROM transactions t LEFT JOIN accounts a ON a.id = t.account_id WHERE a.archived_unix IS NULL AND t.category_source != 'manual'"
        };
        let mut stmt = conn.prepare(sql)?;
        // (note, currency, own account IBAN) ride along for search expressions
        #[allow(clippy::type_complexity)]
        let rows: Vec<(
            (
                i64,
                i64,
                i64,
                String,
                String,
                String,
                String,
                Option<i64>,
                Option<i64>,
            ),
            (String, String, String),
        )> = stmt
            .query_map([], |r| {
                Ok((
                    (
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                        r.get(8)?,
                    ),
                    (r.get(9)?, r.get(10)?, r.get(11)?),
                ))
            })?
            .collect::<Result<_, _>>()?;
        let mut changed = 0u32;
        let mut upd_rule = conn.prepare("UPDATE transactions SET category_id = ?1, rule_id = ?2, category_source = 'rule' WHERE id = ?3")?;
        let mut upd_none = conn.prepare("UPDATE transactions SET category_id = NULL, rule_id = NULL, category_source = 'none' WHERE id = ?1")?;
        for (
            (id, account_id, amount, cp, iban, purpose, bt, cat, rule),
            (note, currency, own_iban),
        ) in rows
        {
            let input = RuleInput {
                account_id,
                amount_cents: amount,
                counterparty: &cp,
                counterparty_iban: &iban,
                purpose: &purpose,
                booking_text: &bt,
                note: &note,
                currency: &currency,
                account_iban: &own_iban,
            };
            match set.categorise(&input) {
                Some((rule_id, cat_id)) => {
                    if cat != Some(cat_id) || rule != Some(rule_id) {
                        upd_rule.execute(params![cat_id, rule_id, id])?;
                        changed += 1;
                    }
                }
                None => {
                    if rule.is_some() {
                        upd_none.execute(params![id])?;
                        changed += 1;
                    }
                }
            }
        }
        Ok(changed)
    }

    pub(crate) fn setting_inner(conn: &Connection, key: &str) -> CoreResult<Option<String>> {
        let stored: Option<String> = conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()?;
        let stored = stored.filter(|v| !v.is_empty());
        if stored.is_some() {
            return Ok(stored);
        }
        Ok(KNOWN_SETTINGS
            .iter()
            .find(|s| s.0 == key)
            .map(|s| s.5.to_string())
            .filter(|v| !v.is_empty()))
    }

    fn category_rows(
        conn: &Connection,
    ) -> CoreResult<Vec<(i64, Option<i64>, String, String, String, i32, Option<i64>)>> {
        let mut stmt = conn.prepare("SELECT id, parent_id, name, sf_symbol, color_hex, sort_order, budget_cents FROM categories")?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn category_paths(
        conn: &Connection,
    ) -> CoreResult<HashMap<i64, (String, String, String, String)>> {
        // id -> (name, path, color, symbol)
        let rows = Self::category_rows(conn)?;
        let by_id: HashMap<i64, &(i64, Option<i64>, String, String, String, i32, Option<i64>)> =
            rows.iter().map(|r| (r.0, r)).collect();
        let mut out = HashMap::new();
        for r in &rows {
            let mut parts = vec![r.2.clone()];
            let mut cur = r.1;
            let mut guard = 0;
            while let Some(pid) = cur {
                guard += 1;
                if guard > 32 {
                    break;
                }
                if let Some(p) = by_id.get(&pid) {
                    parts.push(p.2.clone());
                    cur = p.1;
                } else {
                    break;
                }
            }
            parts.reverse();
            out.insert(
                r.0,
                (r.2.clone(), parts.join(" › "), r.4.clone(), r.3.clone()),
            );
        }
        Ok(out)
    }

    pub(crate) fn ensure_group(
        conn: &Connection,
        name: &str,
        symbol: &str,
        order: i32,
    ) -> CoreResult<i64> {
        if let Some(id) = conn
            .query_row(
                "SELECT id FROM account_groups WHERE name = ?1",
                [name],
                |r| r.get(0),
            )
            .optional()?
        {
            return Ok(id);
        }
        conn.execute(
            "INSERT INTO account_groups(name, sf_symbol, sort_order) VALUES (?1, ?2, ?3)",
            params![name, symbol, order],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Adds a bank login or returns the existing one with the same provider and
    /// login (`(id, created)`).
    pub(crate) fn add_bank_connection_inner(
        conn: &Connection,
        connector: ConnectorKind,
        display_name: String,
        login_name: String,
    ) -> CoreResult<(i64, bool)> {
        Self::add_bank_connection_keyed(
            conn,
            connector,
            connector.as_str(),
            None,
            display_name,
            login_name,
        )
    }

    /// Like `add_bank_connection_inner`, with an explicit connector key
    /// (`fints:<blz>` for generic FinTS banks) and bank code.
    pub(crate) fn add_bank_connection_keyed(
        conn: &Connection,
        connector: ConnectorKind,
        key: &str,
        bank_code: Option<&str>,
        display_name: String,
        login_name: String,
    ) -> CoreResult<(i64, bool)> {
        let info = crate::connectors::descriptor(connector);
        let login_name = crate::connections::normalize_login(connector, &login_name);
        // Same provider + same login: update instead of creating a duplicate.
        let existing: Vec<(i64, String)> = {
            let mut st =
                conn.prepare("SELECT id, login_name FROM bank_connections WHERE connector = ?1")?;
            let r = st
                .query_map([key], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            r
        };
        if let Some((id, _)) = existing.iter().find(|(_, l)| {
            !login_name.is_empty()
                && crate::connections::normalize_login(connector, l)
                    .eq_ignore_ascii_case(&login_name)
        }) {
            if !display_name.trim().is_empty() {
                conn.execute(
                    "UPDATE bank_connections SET display_name = ?1 WHERE id = ?2",
                    params![display_name.trim(), id],
                )?;
            }
            return Ok((*id, false));
        }
        conn.execute(
            "INSERT INTO bank_connections(connector, display_name, login_name, bank_code, sf_symbol) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                key,
                if display_name.trim().is_empty() { info.name.clone() } else { display_name },
                login_name,
                bank_code.map(str::to_string).unwrap_or(info.bank_code),
                info.sf_symbol
            ],
        )?;
        Ok((conn.last_insert_rowid(), true))
    }

    pub fn path(&self) -> &str {
        &self.path
    }
}

impl Vault {
    fn merge_attached(conn: &Connection) -> CoreResult<MergeStats> {
        let mut st = MergeStats::default();
        let tx = conn.unchecked_transaction()?;

        // account groups (by name)
        let mut group_map: HashMap<i64, i64> = HashMap::new();
        {
            let rows: Vec<(i64, String, String, i64)> = tx
                .prepare("SELECT id, name, sf_symbol, sort_order FROM other.account_groups")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<Result<_, _>>()?;
            for (oid, name, sym, ord) in rows {
                tx.execute(
                    "INSERT OR IGNORE INTO account_groups(name, sf_symbol, sort_order) VALUES (?1,?2,?3)",
                    params![name, sym, ord],
                )?;
                let id: i64 = tx.query_row(
                    "SELECT id FROM account_groups WHERE name = ?1",
                    [&name],
                    |r| r.get(0),
                )?;
                group_map.insert(oid, id);
            }
        }

        // bank connections (by connector + login + bank code)
        let mut conn_map: HashMap<i64, i64> = HashMap::new();
        {
            let rows: Vec<(i64, String, String, String, String, String, String, Option<i64>)> = tx
                .prepare(
                    "SELECT id, connector, display_name, login_name, bank_code, sf_symbol, state_json, last_sync_unix
                     FROM other.bank_connections",
                )?
                .query_map([], |r| {
                    Ok((
                        r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?,
                        r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?,
                    ))
                })?
                .collect::<Result<_, _>>()?;
            for (oid, connector, name, login, bank, sym, state, last) in rows {
                let existing: Option<i64> = tx
                    .query_row(
                        "SELECT id FROM bank_connections WHERE connector=?1 AND login_name=?2 AND bank_code=?3",
                        params![connector, login, bank],
                        |r| r.get(0),
                    )
                    .optional()?;
                let id = match existing {
                    Some(id) => id,
                    None => {
                        tx.execute(
                            "INSERT INTO bank_connections(connector, display_name, login_name, bank_code, sf_symbol, state_json, last_sync_unix)
                             VALUES (?1,?2,?3,?4,?5,?6,?7)",
                            params![connector, name, login, bank, sym, state, last],
                        )?;
                        tx.last_insert_rowid()
                    }
                };
                conn_map.insert(oid, id);
            }
        }

        // accounts (by connection + external_ref)
        let mut acc_map: HashMap<i64, i64> = HashMap::new();
        let mut acc_inserted: std::collections::HashSet<i64> = Default::default();
        {
            #[allow(clippy::type_complexity)]
            let rows: Vec<(i64, Option<i64>, i64, String, String, String, String, String, String, String, String, i64, String, String, i64, Option<i64>)> = tx
                .prepare(
                    "SELECT id, connection_id, group_id, external_ref, name, bank_name, owner, iban,
                            account_number, currency, kind, balance_cents, balance_date, sf_symbol,
                            sort_order, initial_balance_cents FROM other.accounts",
                )?
                .query_map([], |r| {
                    Ok((
                        r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?,
                        r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?, r.get(11)?,
                        r.get(12)?, r.get(13)?, r.get(14)?, r.get(15)?,
                    ))
                })?
                .collect::<Result<_, _>>()?;
            for (
                oid,
                ocon,
                ogrp,
                ext,
                name,
                bank,
                owner,
                iban,
                num,
                cur,
                kind,
                bal,
                bdate,
                sym,
                ord,
                init,
            ) in rows
            {
                let con = ocon.and_then(|c| conn_map.get(&c).copied());
                let existing: Option<i64> = tx
                    .query_row(
                        "SELECT id FROM accounts WHERE connection_id IS ?1 AND external_ref = ?2",
                        params![con, ext],
                        |r| r.get(0),
                    )
                    .optional()?;
                let id = match existing {
                    Some(id) => id,
                    None => {
                        let grp =
                            group_map
                                .get(&ogrp)
                                .copied()
                                .ok_or_else(|| CoreError::Database {
                                    message: "merge: unknown account group".into(),
                                })?;
                        tx.execute(
                            "INSERT INTO accounts(connection_id, group_id, external_ref, name, bank_name, owner, iban,
                                account_number, currency, kind, balance_cents, balance_date, sf_symbol, sort_order,
                                initial_balance_cents)
                             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
                            params![con, grp, ext, name, bank, owner, iban, num, cur, kind, bal, bdate, sym, ord, init],
                        )?;
                        st.accounts_added += 1;
                        acc_inserted.insert(tx.last_insert_rowid());
                        tx.last_insert_rowid()
                    }
                };
                acc_map.insert(oid, id);
            }
        }
        // user groups, account placements and names (newest edit wins)
        st.groups_updated = crate::user_groups::merge_attached(&tx, &acc_map, &acc_inserted)?;

        // categories (parents first, by parent + name)
        let mut cat_map: HashMap<i64, i64> = HashMap::new();
        {
            let mut pending: Vec<(i64, Option<i64>, String, String, String, Option<i64>, i64)> = tx
                .prepare(
                    "SELECT id, parent_id, name, sf_symbol, color_hex, budget_cents, sort_order
                     FROM other.categories ORDER BY id",
                )?
                .query_map([], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                })?
                .collect::<Result<_, _>>()?;
            while !pending.is_empty() {
                let before = pending.len();
                let mut rest = Vec::new();
                for row in pending {
                    let (oid, opar, name, sym, color, budget, ord) = row.clone();
                    let par = match opar {
                        None => None,
                        Some(p) => match cat_map.get(&p) {
                            Some(&m) => Some(m),
                            None => {
                                rest.push(row);
                                continue;
                            }
                        },
                    };
                    let existing: Option<i64> = tx
                        .query_row(
                            "SELECT id FROM categories WHERE IFNULL(parent_id,0) = IFNULL(?1,0) AND name = ?2",
                            params![par, name],
                            |r| r.get(0),
                        )
                        .optional()?;
                    let id = match existing {
                        Some(id) => id,
                        None => {
                            tx.execute(
                                "INSERT INTO categories(parent_id, name, sf_symbol, color_hex, budget_cents, sort_order)
                                 VALUES (?1,?2,?3,?4,?5,?6)",
                                params![par, name, sym, color, budget, ord],
                            )?;
                            st.categories_added += 1;
                            tx.last_insert_rowid()
                        }
                    };
                    cat_map.insert(oid, id);
                }
                if rest.len() == before {
                    break; // orphaned categories (broken parent) – skip
                }
                pending = rest;
            }
        }

        // rules (by name + field + kind + pattern + category)
        let mut rule_map: HashMap<i64, i64> = HashMap::new();
        {
            #[allow(clippy::type_complexity)]
            let rows: Vec<(
                i64,
                String,
                i64,
                String,
                String,
                String,
                bool,
                Option<i64>,
                Option<i64>,
                Option<i64>,
                i64,
                bool,
            )> = tx
                .prepare(
                    "SELECT id, name, category_id, field, match_kind, pattern, case_sensitive,
                            min_amount_cents, max_amount_cents, account_id, priority, enabled
                     FROM other.rules ORDER BY id",
                )?
                .query_map([], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                        r.get(8)?,
                        r.get(9)?,
                        r.get(10)?,
                        r.get(11)?,
                    ))
                })?
                .collect::<Result<_, _>>()?;
            for (oid, name, ocat, field, kind, pat, cs, min, max, oacc, prio, en) in rows {
                let Some(&cat) = cat_map.get(&ocat) else {
                    continue;
                };
                if pat.len() > crate::rules::MAX_PATTERN_LEN {
                    continue;
                }
                let acc = oacc.and_then(|a| acc_map.get(&a).copied());
                if oacc.is_some() && acc.is_none() {
                    continue;
                }
                let existing: Option<i64> = tx
                    .query_row(
                        "SELECT id FROM rules WHERE name=?1 AND field=?2 AND match_kind=?3 AND pattern=?4
                         AND category_id=?5 AND account_id IS ?6",
                        params![name, field, kind, pat, cat, acc],
                        |r| r.get(0),
                    )
                    .optional()?;
                let id = match existing {
                    Some(id) => id,
                    None => {
                        tx.execute(
                            "INSERT INTO rules(name, category_id, field, match_kind, pattern, case_sensitive,
                                min_amount_cents, max_amount_cents, account_id, priority, enabled)
                             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                            params![name, cat, field, kind, pat, cs, min, max, acc, prio, en],
                        )?;
                        st.rules_added += 1;
                        tx.last_insert_rowid()
                    }
                };
                rule_map.insert(oid, id);
            }
        }

        // bookings
        #[allow(clippy::type_complexity)]
        let rows: Vec<(
            i64,
            String,
            String,
            String,
            i64,
            String,
            String,
            String,
            String,
            String,
            Option<i64>,
            String,
            Option<i64>,
            bool,
            i64,
            String,
            String,
        )> = tx
            .prepare(
                "SELECT account_id, external_id, booking_date, value_date, amount_cents, currency,
                        counterparty, counterparty_iban, purpose, booking_text, category_id,
                        category_source, rule_id, checked, imported_unix, note, import_fp
                 FROM other.transactions",
            )?
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                    r.get(10)?,
                    r.get(11)?,
                    r.get(12)?,
                    r.get(13)?,
                    r.get(14)?,
                    r.get(15)?,
                    r.get(16)?,
                ))
            })?
            .collect::<Result<_, _>>()?;
        for (
            oacc,
            ext,
            bd,
            vd,
            amt,
            cur,
            cp,
            cpi,
            purpose,
            text,
            ocat,
            csrc,
            orule,
            checked,
            imp,
            note,
            fp,
        ) in rows
        {
            let Some(&acc) = acc_map.get(&oacc) else {
                continue;
            };
            let cat = ocat.and_then(|c| cat_map.get(&c).copied());
            let rule = orule.and_then(|c| rule_map.get(&c).copied());
            let existing: Option<i64> = tx
                .query_row(
                    "SELECT id FROM transactions WHERE account_id = ?1
                       AND (external_id = ?2 OR (?3 != '' AND import_fp = ?3)) LIMIT 1",
                    params![acc, ext, fp],
                    |r| r.get(0),
                )
                .optional()?;
            match existing {
                Some(id) => {
                    let n = tx.execute(
                        "UPDATE transactions SET
                            checked = CASE WHEN ?2 THEN 1 ELSE checked END,
                            category_id = CASE WHEN category_id IS NULL AND ?3 IS NOT NULL THEN ?3 ELSE category_id END,
                            category_source = CASE WHEN category_id IS NULL AND ?3 IS NOT NULL THEN ?4 ELSE category_source END,
                            rule_id = CASE WHEN category_id IS NULL AND ?3 IS NOT NULL THEN ?5 ELSE rule_id END,
                            note = CASE WHEN note = '' AND ?6 != '' THEN ?6 ELSE note END
                         WHERE id = ?1
                           AND ((?2 AND checked = 0)
                             OR (category_id IS NULL AND ?3 IS NOT NULL)
                             OR (note = '' AND ?6 != ''))",
                        params![id, checked, cat, csrc, rule, note],
                    )?;
                    st.bookings_updated += n as u32;
                }
                None => {
                    tx.execute(
                        "INSERT INTO transactions(account_id, external_id, booking_date, value_date, amount_cents,
                            currency, counterparty, counterparty_iban, purpose, booking_text, category_id,
                            category_source, rule_id, checked, imported_unix, note, import_fp)
                         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
                        params![acc, ext, bd, vd, amt, cur, cp, cpi, purpose, text, cat, csrc, rule, checked, imp, note, fp],
                    )?;
                    st.bookings_added += 1;
                }
            }
        }
        tx.commit()?;
        Ok(st)
    }
}

/// Result of [`Vault::merge_other_vault`].
#[derive(Debug, Clone, Default, uniffi::Record)]
pub struct MergeStats {
    pub accounts_added: u32,
    pub categories_added: u32,
    pub rules_added: u32,
    pub bookings_added: u32,
    pub bookings_updated: u32,
    /// User groups and account placements taken over from the other vault.
    pub groups_updated: u32,
}

#[uniffi::export]
impl Vault {
    /// Opens (or creates) the encrypted vault at `path` with the raw 32-byte
    /// master key. The key bytes are wiped from memory after use.
    #[uniffi::constructor]
    pub fn open(path: String, key: Vec<u8>) -> Result<Arc<Self>, CoreError> {
        let key = Zeroizing::new(key);
        // Default (SQLCipher 4 settings) first; if the key check fails, retry with
        // explicit compatibility modes for vaults written by older SQLCipher builds.
        // The cipher_compatibility pragma must be issued before the key.
        let mut tried: Vec<&str> = Vec::new();
        let mut opened: Option<Connection> = None;
        let mut first_err: Option<CoreError> = None;
        for compat in [None, Some(4u8), Some(3u8)] {
            let conn = Connection::open(&path)?;
            Self::apply_key(&conn, &key)?;
            if let Some(c) = compat {
                let _ = conn.query_row(&format!("PRAGMA cipher_compatibility = {c};"), [], |_| {
                    Ok(())
                });
            }
            match Self::migrate(&conn) {
                Ok(()) => {
                    if compat.is_some() {
                        tried.push("ok");
                        eprintln!("vault: opened with cipher_compatibility={compat:?}");
                    }
                    opened = Some(conn);
                    break;
                }
                Err(e) if matches!(e, CoreError::WrongKey) => {
                    tried.push(match compat {
                        None => "default",
                        Some(4) => "compat4",
                        _ => "compat3",
                    });
                    first_err.get_or_insert(e);
                }
                // Any other error is real (not a key/format mismatch): report as is.
                Err(e) => return Err(e),
            }
        }
        let Some(conn) = opened else {
            if Self::is_plaintext_sqlite(&path) {
                return Err(CoreError::InvalidInput {
                    message: "Die Tresordatei ist unverschlüsselt (Klartext-SQLite) und passt nicht zum Schlüssel. Sie wurde nicht verändert.".into(),
                });
            }
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            let wal = std::fs::metadata(format!("{path}-wal"))
                .map(|m| m.len())
                .unwrap_or(0);
            let name = std::path::Path::new(&path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            crate::depot_debug::log(&format!(
                "vault open failed: HMAC check on page 1 failed for all modes tried: {} (file={name} size={size} wal_size={wal} key_len={})",
                tried.join(", "),
                key.len()
            ));
            return Err(first_err.unwrap_or(CoreError::WrongKey));
        };
        // merge duplicate bank logins left by older versions (keeps bookings)
        let _ = crate::connections::deduplicate(&conn);
        // repair "MÃ¼ller" -> "Müller" stored by older versions
        let _ = crate::text::repair_stored_text(&conn);
        // one-time purge of favicons that don't belong to a known brand
        let _ = crate::insights::purge_untrusted_logos(&conn);
        Ok(Arc::new(Vault {
            conn: Mutex::new(conn),
            path,
        }))
    }

    /// Changes the master key (SQLCipher `PRAGMA rekey`).
    pub fn rekey(&self, new_key: Vec<u8>) -> Result<(), CoreError> {
        let key = Zeroizing::new(new_key);
        if key.len() < 16 {
            return Err(CoreError::InvalidInput {
                message: "master key must be at least 16 bytes".into(),
            });
        }
        let pragma = Zeroizing::new(format!("PRAGMA rekey = \"x'{}'\";", hex::encode(&*key)));
        self.db()?.execute_batch(&pragma)?;
        Ok(())
    }

    /// Merges another vault file (e.g. a copy synced via iCloud) into this one.
    /// Missing connections, accounts, categories, rules and bookings are added;
    /// existing bookings are never duplicated (matched by account + external id
    /// or import fingerprint) but gain `checked`, category and note if the
    /// other side has them and this side does not.
    pub fn merge_other_vault(
        &self,
        other_path: String,
        key: Vec<u8>,
    ) -> Result<MergeStats, CoreError> {
        let key = Zeroizing::new(key);
        if other_path == self.path {
            return Err(CoreError::InvalidInput {
                message: "cannot merge a vault into itself".into(),
            });
        }
        // Validate key and bring the other file up to the current schema.
        {
            let o = Connection::open(&other_path)?;
            Self::apply_key(&o, &key)?;
            Self::migrate(&o)?;
        }
        let conn = self.db()?;
        let raw_key = Zeroizing::new(format!("x'{}'", hex::encode(&*key)));
        conn.execute(
            "ATTACH DATABASE ?1 AS other KEY ?2",
            params![other_path, &*raw_key],
        )?;
        let res = Self::merge_attached(&conn);
        let detach_res = conn.execute_batch("DETACH DATABASE other");
        // Fold WAL frames into the main file so a plain file copy is complete.
        let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
        let stats = res?;
        detach_res?;
        Ok(stats)
    }

    /// Flushes the WAL into the main vault file (`wal_checkpoint(TRUNCATE)`),
    /// so copying the file (e.g. to iCloud) never misses committed frames.
    pub fn checkpoint(&self) -> Result<(), CoreError> {
        let conn = self.db()?;
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        Ok(())
    }

    pub fn file_path(&self) -> String {
        self.path.clone()
    }

    pub fn is_empty(&self) -> Result<bool, CoreError> {
        let db = self.db()?;
        let n: i64 = db.query_row(
            "SELECT (SELECT count(*) FROM accounts) + (SELECT count(*) FROM categories)",
            [],
            |r| r.get(0),
        )?;
        Ok(n == 0)
    }

    // ---------------------------------------------------------------- bank connections

    pub fn list_bank_connections(&self) -> Result<Vec<BankConnection>, CoreError> {
        let db = self.db()?;
        let mut stmt = db.prepare(
            "SELECT id, connector, display_name, login_name, bank_code, sf_symbol, last_sync_unix,
                    EXISTS (SELECT 1 FROM accounts a WHERE a.connection_id = bank_connections.id)
                      AND NOT EXISTS (SELECT 1 FROM accounts a WHERE a.connection_id = bank_connections.id AND a.archived_unix IS NULL)
             FROM bank_connections ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| {
            let kind: String = r.get(1)?;
            Ok((
                kind,
                BankConnection {
                    id: r.get(0)?,
                    connector: ConnectorKind::VolksbankKasselGoettingen,
                    display_name: r.get(2)?,
                    login_name: r.get(3)?,
                    bank_code: r.get(4)?,
                    sf_symbol: r.get(5)?,
                    last_sync_unix: r.get(6)?,
                    all_accounts_archived: r.get(7)?,
                },
            ))
        })?;
        let mut out = vec![];
        for row in rows {
            let (kind, mut c) = row?;
            if let Some(k) = ConnectorKind::from_str(&kind) {
                c.connector = k;
                out.push(c);
            }
        }
        Ok(out)
    }

    /// Adds a bank login. Credentials (PIN/password) are *not* stored here;
    /// they are passed to `begin_sync` each time.
    pub fn add_bank_connection(
        &self,
        connector: ConnectorKind,
        display_name: String,
        login_name: String,
    ) -> Result<i64, CoreError> {
        let db = self.db()?;
        Ok(Self::add_bank_connection_inner(&db, connector, display_name, login_name)?.0)
    }

    /// Adds a login at any FinTS 3.0 PIN/TAN bank and stores the bank's URL
    /// (`fints.url.<blz>`). `blz` must be 8 digits, `url` an https URL.
    /// `tan_medium` is optional (stored as `fints.tan_medium.<connection id>`).
    pub fn add_fints_connection(
        &self,
        blz: String,
        url: String,
        display_name: String,
        login_name: String,
        tan_medium: String,
    ) -> Result<i64, CoreError> {
        let blz = blz.trim().to_string();
        let url = url.trim().to_string();
        if ConnectorKind::fints_blz(&ConnectorKind::fints_key(&blz)).is_none() {
            return Err(CoreError::InvalidInput {
                message: tr("Die BLZ muss aus genau 8 Ziffern bestehen.").into(),
            });
        }
        if !crate::connectors::fints_presets::is_valid_fints_url(&url) {
            return Err(CoreError::InvalidInput {
                message: tr("Die FinTS-URL muss mit https:// beginnen.").into(),
            });
        }
        let db = self.db()?;
        let key = ConnectorKind::fints_key(&blz);
        let (id, _) = Self::add_bank_connection_keyed(
            &db,
            ConnectorKind::FinTsGeneric,
            &key,
            Some(&blz),
            display_name,
            login_name,
        )?;
        db.execute(
            "INSERT INTO settings(key, value, is_secret) VALUES (?1, ?2, 0)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![format!("fints.url.{blz}"), url],
        )?;
        // Optional TAN medium name (HKTAN), e.g. Postbank BestSign "SO:Gerät".
        let tan_medium = tan_medium.trim();
        if !tan_medium.is_empty() {
            db.execute(
                "INSERT INTO settings(key, value, is_secret) VALUES (?1, ?2, 0)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![format!("fints.tan_medium.{id}"), tan_medium],
            )?;
        }
        Ok(id)
    }

    /// Adds a login at a bank reached through Enable Banking (PSD2). `aspsp`
    /// is the Enable Banking bank name, `country` its ISO alpha-2 code.
    /// `login_name` is only a label (defaults to the bank name).
    pub fn add_enable_banking_connection(
        &self,
        aspsp: String,
        country: String,
        display_name: String,
        login_name: String,
    ) -> Result<i64, CoreError> {
        let aspsp = aspsp.trim().to_string();
        let country = country.trim().to_uppercase();
        let key = ConnectorKind::enable_banking_key(&country, &aspsp);
        if aspsp.is_empty() || ConnectorKind::enable_banking_target(&key).is_none() {
            return Err(CoreError::InvalidInput {
                message: tr("Bank oder Ländercode ungültig.").into(),
            });
        }
        let login = if login_name.trim().is_empty() {
            aspsp.clone()
        } else {
            login_name
        };
        let display = if display_name.trim().is_empty() {
            aspsp.clone()
        } else {
            display_name
        };
        let db = self.db()?;
        let (id, _) = Self::add_bank_connection_keyed(
            &db,
            ConnectorKind::EnableBanking,
            &key,
            None,
            display,
            login,
        )?;
        Ok(id)
    }

    pub fn delete_bank_connection(&self, connection_id: i64) -> Result<(), CoreError> {
        let db = self.db()?;
        let tx = db.unchecked_transaction()?;
        Self::purge_connection(&tx, connection_id)?;
        tx.commit()?;
        Ok(())
    }

    /// Deletes one account with its bookings, documents and holdings. When it was the last
    /// account of its connection, the connection and its stored secrets (PIN, session) go too.
    /// Only archived accounts can be deleted (`set_account_archived` first).
    pub fn delete_account(&self, account_id: i64) -> Result<(), CoreError> {
        let db = self.db()?;
        let tx = db.unchecked_transaction()?;
        let Some((conn, archived)) = tx
            .query_row(
                "SELECT connection_id, archived_unix IS NOT NULL FROM accounts WHERE id = ?1",
                [account_id],
                |r| Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, bool>(1)?)),
            )
            .optional()?
        else {
            return Err(CoreError::NotFound {
                what: trf("Konto {0}", &[&account_id]),
            });
        };
        if !archived {
            return Err(CoreError::InvalidInput {
                message: tr("Nur archivierte Konten können gelöscht werden. Bitte das Konto zuerst archivieren.")
                    .to_string(),
            });
        }
        Self::delete_account_rows(&tx, account_id)?;
        if let Some(c) = conn {
            let left: i64 = tx.query_row(
                "SELECT count(*) FROM accounts WHERE connection_id = ?1",
                [c],
                |r| r.get(0),
            )?;
            if left == 0 {
                Self::purge_connection(&tx, c)?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    // ---------------------------------------------------------------- holdings

    pub fn list_holdings(&self, account_id: i64) -> Result<Vec<Holding>, CoreError> {
        let db = self.db()?;
        let mut q = db.prepare(
            "SELECT isin, wkn, name, quantity, current_price_cents, purchase_price_cents, value_cents
             FROM holdings WHERE account_id = ?1 ORDER BY value_cents DESC, name",
        )?;
        let rows = q.query_map([account_id], |r| {
            Ok(Holding {
                isin: r.get(0)?,
                wkn: r.get(1)?,
                name: r.get(2)?,
                quantity: r.get(3)?,
                current_price_cents: r.get(4)?,
                purchase_price_cents: r.get(5)?,
                value_cents: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    // ---------------------------------------------------------------- accounts

    pub fn list_accounts(&self) -> Result<Vec<Account>, CoreError> {
        let db = self.db()?;
        let mut stmt = db.prepare(
            "SELECT a.id, a.connection_id, c.connector, a.group_id, a.name, a.bank_name, a.owner, a.iban, a.account_number,
                    a.currency, a.kind, a.balance_cents, a.balance_date, a.sf_symbol, a.sort_order,
                    (SELECT count(*) FROM transactions t WHERE t.account_id = a.id),
                    (SELECT count(*) FROM transactions t WHERE t.account_id = a.id AND t.category_id IS NULL),
                    (SELECT count(*) FROM transactions t WHERE t.account_id = a.id AND t.checked = 0),
                    a.initial_balance_cents, g.uuid, IFNULL(g.include_in_total, 1) AND a.archived_unix IS NULL,
                    a.color_hex, IFNULL(c.connector, ''), IFNULL(c.bank_code, ''), IFNULL(c.login_name, ''), a.external_ref,
                    a.archived_unix, a.access_revoked
             FROM accounts a LEFT JOIN bank_connections c ON c.id = a.connection_id
             LEFT JOIN user_groups g ON g.uuid = a.user_group_uuid AND g.deleted_ms IS NULL
             ORDER BY a.sort_order, a.id",
        )?;
        let mut color_hashes: HashMap<i64, u64> = HashMap::new();
        let rows = stmt.query_map([], |r| {
            Ok(Account {
                id: r.get(0)?,
                connection_id: r.get(1)?,
                connector: r
                    .get::<_, Option<String>>(2)?
                    .and_then(|s| ConnectorKind::from_str(&s)),
                group_id: r.get(3)?,
                name: r.get(4)?,
                bank_name: r.get(5)?,
                owner: r.get(6)?,
                iban: r.get(7)?,
                account_number: r.get(8)?,
                currency: r.get(9)?,
                kind: AccountKind::from_str(&r.get::<_, String>(10)?),
                balance_cents: r.get(11)?,
                balance_date: r.get(12)?,
                sf_symbol: r.get(13)?,
                sort_order: r.get(14)?,
                transaction_count: r.get::<_, i64>(15)? as u32,
                uncategorized_count: r.get::<_, i64>(16)? as u32,
                new_count: r.get::<_, i64>(17)? as u32,
                initial_balance_cents: r
                    .get::<_, Option<i64>>(18)?
                    .map(i64::abs)
                    .filter(|c| *c > 0),
                user_group_uuid: r.get(19)?,
                in_total: r.get(20)?,
                // filled in below for accounts without a chosen colour
                color_hex: match r.get::<_, Option<String>>(21)? {
                    Some(c) => c,
                    None => {
                        color_hashes.insert(
                            r.get(0)?,
                            crate::user_groups::account_color_hash(
                                &r.get::<_, String>(22)?,
                                &r.get::<_, String>(23)?,
                                &r.get::<_, String>(24)?,
                                &r.get::<_, String>(25)?,
                            ),
                        );
                        String::new()
                    }
                },
                archived_unix: r.get(26)?,
                access_revoked: r.get::<_, i64>(27)? != 0,
            })
        })?;
        let mut accounts = rows.collect::<Result<Vec<_>, _>>()?;
        // a subgroup of an excluded group is excluded too
        let excluded = crate::user_groups::excluded_from_total(&db)?;
        if !excluded.is_empty() {
            for a in accounts.iter_mut() {
                if a.user_group_uuid
                    .as_ref()
                    .is_some_and(|u| excluded.contains(u))
                {
                    a.in_total = false;
                }
            }
        }
        crate::user_groups::assign_default_colors(&mut accounts, &color_hashes);
        // Credit cards stored with balance 0: derive the open amount from bookings.
        for a in accounts.iter_mut().filter(|a| {
            a.kind == AccountKind::CreditCard && a.balance_cents == 0 && a.transaction_count > 0
        }) {
            let mut st = db.prepare(
                "SELECT booking_date, amount_cents, counterparty || ' ' || purpose || ' ' || booking_text
                 FROM transactions WHERE account_id = ?1 ORDER BY booking_date, id",
            )?;
            let tx_rows: Vec<(String, i64, String)> = st
                .query_map([a.id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<Result<_, _>>()?;
            if let Some(open) = crate::connectors::open_card_balance(&tx_rows) {
                a.balance_cents = open;
            }
        }
        Ok(accounts)
    }

    /// Sidebar data: groups with their accounts and summed balances.
    pub fn list_account_groups(&self) -> Result<Vec<AccountGroup>, CoreError> {
        let accounts = self.list_accounts()?;
        let db = self.db()?;
        let mut stmt = db.prepare(
            "SELECT id, name, sf_symbol, sort_order FROM account_groups ORDER BY sort_order, id",
        )?;
        let groups = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i32>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(groups
            .into_iter()
            .map(|(id, name, sym, order)| {
                let accs: Vec<Account> = accounts
                    .iter()
                    .filter(|a| a.group_id == id)
                    .cloned()
                    .collect();
                let balance = accs
                    .iter()
                    .filter(|a| a.currency == "EUR")
                    .map(|a| a.balance_cents)
                    .sum();
                AccountGroup {
                    id,
                    name: display_name(&name),
                    sf_symbol: sym,
                    sort_order: order,
                    balance_cents: balance,
                    accounts: accs,
                }
            })
            .filter(|g| !g.accounts.is_empty())
            .collect())
    }

    /// End-of-day balances for the last `days` days (oldest first), for the
    /// header chart. Empty `account_ids` = all EUR accounts. Computed backwards
    /// from the current balance, so it is exact in cents.
    pub fn balance_history(
        &self,
        account_ids: Vec<i64>,
        days: u32,
    ) -> Result<Vec<DailyBalance>, CoreError> {
        let accounts = self.list_accounts()?;
        let chosen: Vec<&Account> = accounts
            .iter()
            .filter(|a| {
                a.currency == "EUR"
                    && if account_ids.is_empty() {
                        a.in_total
                    } else {
                        account_ids.contains(&a.id)
                    }
            })
            .collect();
        let mut balance: i64 = chosen.iter().map(|a| a.balance_cents).sum();
        let ids: Vec<i64> = chosen.iter().map(|a| a.id).collect();
        let today = chrono::Local::now().date_naive();
        let start = today - chrono::Duration::days(days.max(1) as i64 - 1);
        let mut per_day: HashMap<String, i64> = HashMap::new();
        if !ids.is_empty() {
            let db = self.db()?;
            let list = (0..ids.len())
                .map(|n| format!("?{}", n + 2))
                .collect::<Vec<_>>()
                .join(",");
            let mut stmt = db.prepare(&format!(
                "SELECT booking_date, SUM(amount_cents) FROM transactions WHERE account_id IN ({list}) AND booking_date > ?1 GROUP BY booking_date"
            ))?;
            let date = start.format("%Y-%m-%d").to_string();
            let args: Vec<rusqlite::types::Value> = std::iter::once(date.into())
                .chain(ids.iter().map(|&i| i.into()))
                .collect();
            let rows = stmt.query_map(rusqlite::params_from_iter(args), |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?;
            for row in rows {
                let (d, s) = row?;
                per_day.insert(d, s);
            }
        }
        let mut out = Vec::with_capacity(days as usize);
        let mut day = today;
        while day >= start {
            let key = day.format("%Y-%m-%d").to_string();
            out.push(DailyBalance {
                date: key.clone(),
                balance_cents: balance,
            });
            balance -= per_day.get(&key).copied().unwrap_or(0);
            day -= chrono::Duration::days(1);
        }
        out.reverse();
        Ok(out)
    }

    pub fn rename_account(&self, account_id: i64, name: String) -> Result<(), CoreError> {
        let n = self.db()?.execute(
            "UPDATE accounts SET name = ?1, name_ms = ?3 WHERE id = ?2",
            params![name, account_id, crate::user_groups::now_ms()],
        )?;
        if n == 0 {
            return Err(CoreError::NotFound {
                what: trf("Konto {0}", &[&account_id]),
            });
        }
        Ok(())
    }

    /// Sets (or clears with `None`) the original loan amount of an account.
    pub fn set_account_initial_balance(
        &self,
        account_id: i64,
        cents: Option<i64>,
    ) -> Result<(), CoreError> {
        let cents = cents.map(|c| c.abs()).filter(|c| *c > 0);
        let n = self.db()?.execute(
            "UPDATE accounts SET initial_balance_cents = ?1 WHERE id = ?2",
            params![cents, account_id],
        )?;
        if n == 0 {
            return Err(CoreError::NotFound {
                what: trf("Konto {0}", &[&account_id]),
            });
        }
        Ok(())
    }

    // ---------------------------------------------------------------- categories

    /// All categories in tree order (depth-first, siblings by sort order) with
    /// totals for the optional period.
    pub fn list_categories(
        &self,
        date_from: Option<String>,
        date_to: Option<String>,
    ) -> Result<Vec<Category>, CoreError> {
        let db = self.db()?;
        let rows = Self::category_rows(&db)?;
        let mut totals: HashMap<Option<i64>, (i64, u32)> = HashMap::new();
        {
            let mut stmt = db.prepare(&format!(
                "SELECT category_id, SUM(amount_cents), COUNT(*) FROM {}
                 WHERE (?1 IS NULL OR booking_date >= ?1) AND (?2 IS NULL OR booking_date <= ?2)
                 GROUP BY category_id",
                crate::planning::EFFECTIVE_SQL
            ))?;
            let it = stmt.query_map(params![date_from, date_to], |r| {
                Ok((
                    r.get::<_, Option<i64>>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)? as u32,
                ))
            })?;
            for row in it {
                let (c, s, n) = row?;
                totals.insert(c, (s, n));
            }
        }
        let mut children: HashMap<
            Option<i64>,
            Vec<&(i64, Option<i64>, String, String, String, i32, Option<i64>)>,
        > = HashMap::new();
        for r in &rows {
            children.entry(r.1).or_default().push(r);
        }
        for list in children.values_mut() {
            list.sort_by(|a, b| {
                a.5.cmp(&b.5)
                    .then_with(|| a.2.to_lowercase().cmp(&b.2.to_lowercase()))
            });
        }
        fn subtotal(
            id: i64,
            children: &HashMap<
                Option<i64>,
                Vec<&(i64, Option<i64>, String, String, String, i32, Option<i64>)>,
            >,
            totals: &HashMap<Option<i64>, (i64, u32)>,
            depth: u32,
        ) -> (i64, u32) {
            let own = totals.get(&Some(id)).copied().unwrap_or((0, 0));
            if depth > 32 {
                return own;
            }
            let mut sum = own;
            if let Some(kids) = children.get(&Some(id)) {
                for k in kids {
                    let (s, n) = subtotal(k.0, children, totals, depth + 1);
                    sum.0 += s;
                    sum.1 += n;
                }
            }
            sum
        }
        let mut out = Vec::with_capacity(rows.len());
        fn walk(
            parent: Option<i64>,
            depth: u32,
            prefix: &str,
            children: &HashMap<
                Option<i64>,
                Vec<&(i64, Option<i64>, String, String, String, i32, Option<i64>)>,
            >,
            totals: &HashMap<Option<i64>, (i64, u32)>,
            out: &mut Vec<Category>,
        ) {
            if depth > 32 {
                return;
            }
            if let Some(kids) = children.get(&parent) {
                for k in kids {
                    let path = if prefix.is_empty() {
                        k.2.clone()
                    } else {
                        format!("{prefix} › {}", k.2)
                    };
                    let own = totals.get(&Some(k.0)).copied().unwrap_or((0, 0));
                    let (total, count) = subtotal(k.0, children, totals, 0);
                    out.push(Category {
                        id: k.0,
                        parent_id: k.1,
                        name: display_name(&k.2),
                        sf_symbol: k.3.clone(),
                        color_hex: k.4.clone(),
                        sort_order: k.5,
                        depth,
                        path: display_path(&path),
                        own_total_cents: own.0,
                        total_cents: total,
                        transaction_count: count,
                        budget_cents: k.6,
                    });
                    walk(Some(k.0), depth + 1, &path, children, totals, out);
                }
            }
        }
        walk(None, 0, "", &children, &totals, &mut out);
        Ok(out)
    }

    pub fn create_category(
        &self,
        name: String,
        parent_id: Option<i64>,
        sf_symbol: String,
        color_hex: String,
    ) -> Result<i64, CoreError> {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err(CoreError::InvalidInput {
                message: tr("Der Kategoriename ist leer.").into(),
            });
        }
        let db = self.db()?;
        let order: i32 = db.query_row(
            "SELECT IFNULL(MAX(sort_order), 0) + 1 FROM categories WHERE parent_id IS ?1",
            [parent_id],
            |r| r.get(0),
        )?;
        db.execute(
            "INSERT INTO categories(parent_id, name, sf_symbol, color_hex, sort_order) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![parent_id, name, if sf_symbol.is_empty() { "tag".to_string() } else { sf_symbol }, color_hex, order],
        )?;
        Ok(db.last_insert_rowid())
    }

    pub fn update_category(
        &self,
        id: i64,
        name: String,
        parent_id: Option<i64>,
        sf_symbol: String,
        color_hex: String,
    ) -> Result<(), CoreError> {
        let db = self.db()?;
        // Refuse moving a category below itself.
        if let Some(mut p) = parent_id {
            let mut guard = 0;
            loop {
                if p == id {
                    return Err(CoreError::InvalidInput {
                        message: tr("Eine Kategorie kann nicht in sich selbst verschoben werden.")
                            .into(),
                    });
                }
                guard += 1;
                match db
                    .query_row("SELECT parent_id FROM categories WHERE id = ?1", [p], |r| {
                        r.get::<_, Option<i64>>(0)
                    })
                    .optional()?
                {
                    Some(Some(next)) if guard < 32 => p = next,
                    _ => break,
                }
            }
        }
        // The UI shows built-in names translated; saving that unchanged
        // display name keeps the stored German default (and its logic).
        let stored: Option<String> = db
            .query_row("SELECT name FROM categories WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        let name = match stored {
            Some(stored) if stored != name && display_name(&stored) == name => stored,
            _ => name,
        };
        db.execute(
            "UPDATE categories SET name = ?1, parent_id = ?2, sf_symbol = ?3, color_hex = ?4 WHERE id = ?5",
            params![name, parent_id, sf_symbol, color_hex, id],
        )?;
        Ok(())
    }

    /// Deletes a category and its sub-categories; their transactions become
    /// uncategorised.
    pub fn delete_category(&self, id: i64) -> Result<(), CoreError> {
        let db = self.db()?;
        db.execute("DELETE FROM categories WHERE id = ?1", [id])?;
        db.execute("UPDATE transactions SET category_source = 'none', rule_id = NULL WHERE category_id IS NULL AND category_source != 'none'", [])?;
        Ok(())
    }

    // ---------------------------------------------------------------- transactions

    pub fn list_transactions(&self, filter: BookingFilter) -> Result<BookingPage, CoreError> {
        let db = self.db()?;
        let paths = Self::category_paths(&db)?;
        let mut wh: Vec<String> = vec!["1=1".into()];
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
                    .prepare("SELECT id, iban, account_number FROM accounts WHERE kind = 'loan'")?;
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
            wh.push(format!("({clause})"));
        }
        if filter.uncategorized_only {
            wh.push("t.category_id IS NULL".into());
        } else if let Some(cat) = filter.category_id {
            args.push(Box::new(cat));
            let n = args.len();
            if filter.include_subcategories {
                cte = format!(
                    "WITH RECURSIVE sub(id) AS (SELECT ?{n} UNION ALL SELECT c.id FROM categories c JOIN sub ON c.parent_id = sub.id) "
                );
                wh.push(
                    "(t.category_id IN (SELECT id FROM sub) OR EXISTS (SELECT 1 FROM transaction_splits sp WHERE sp.transaction_id = t.id AND sp.category_id IN (SELECT id FROM sub)))"
                        .into(),
                );
            } else {
                wh.push(format!(
                    "(t.category_id = ?{n} OR EXISTS (SELECT 1 FROM transaction_splits sp WHERE sp.transaction_id = t.id AND sp.category_id = ?{n}))"
                ));
            }
        }
        if let Some(from) = filter.date_from.as_ref().filter(|s| !s.is_empty()) {
            args.push(Box::new(from.clone()));
            wh.push(format!("t.booking_date >= ?{}", args.len()));
        }
        if let Some(to) = filter.date_to.as_ref().filter(|s| !s.is_empty()) {
            args.push(Box::new(to.clone()));
            wh.push(format!("t.booking_date <= ?{}", args.len()));
        }
        if let Some(q) = filter
            .search
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
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
                if term.chars().any(|c| c.is_ascii_digit()) {
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
        let where_sql = wh.join(" AND ");
        let from_sql = "FROM transactions t JOIN accounts a ON a.id = t.account_id LEFT JOIN categories cat ON cat.id = t.category_id";
        let sql = format!(
            "{cte}SELECT t.id, t.account_id, a.name, t.booking_date, t.value_date, t.amount_cents, t.currency, t.counterparty,
                    t.counterparty_iban, t.purpose, t.booking_text, t.category_id, t.category_source, t.rule_id, t.checked, a.kind,
                    (SELECT count(*) FROM booking_documents d WHERE d.booking_id = t.id), t.note,
                    t.purpose_code, t.gvc, t.salary_flag
             {from_sql} WHERE {where_sql} ORDER BY t.booking_date DESC, t.id DESC"
        );
        let mut stmt = db.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args.iter().map(|b| b.as_ref())), |r| {
            let cat: Option<i64> = r.get(11)?;
            let src: String = r.get(12)?;
            Ok(Booking {
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
                merchant: MerchantResolver::resolve("", ""),
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
            })
        })?;
        let mut txs = Vec::new();
        let salary_ibans = crate::salary::marked_salary_ibans(&db)?;
        let overrides = crate::insights::load_overrides(&db).unwrap_or_default();
        let (mut income, mut expense, mut consumption, mut investment, mut perk) =
            (0i64, 0i64, 0i64, 0i64, 0i64);
        for row in rows {
            let mut t = row?;
            if let Some((name, path, color, sym)) = t.category_id.and_then(|c| paths.get(&c)) {
                t.category_name = name.clone();
                t.category_path = path.clone();
                t.category_color_hex = color.clone();
                t.category_symbol = sym.clone();
            }
            enrich(&mut t);
            t.is_salary = crate::salary::is_salary(&t, &salary_ibans);
            // Localise after `enrich`: flow classification needs the stored names.
            t.category_name = display_name(&t.category_name);
            t.category_path = display_path(&t.category_path);
            if let Some(d) = overrides.get(&t.merchant.key) {
                t.merchant.domain = d.clone();
                t.merchant.domain_candidates = vec![d.clone()];
                t.merchant.domain_source = "override".into();
                t.merchant_domain = Some(d.clone());
            }
            let keep = match filter.scope {
                BookingScope::All => true,
                BookingScope::Everyday => !matches!(t.flow, FlowKind::Investment | FlowKind::Perk),
                BookingScope::Investments => {
                    matches!(t.flow, FlowKind::Investment | FlowKind::Perk)
                }
            } && filter
                .merchant_key
                .as_ref()
                .map(|k| *k == t.merchant.key)
                .unwrap_or(true);
            if !keep {
                continue;
            }
            if t.amount_cents > 0 {
                income += t.amount_cents;
            } else {
                expense += t.amount_cents;
            }
            match t.flow {
                FlowKind::Consumption => consumption += t.amount_cents,
                FlowKind::Investment => investment += t.amount_cents,
                FlowKind::Perk => perk += t.amount_cents,
                _ => {}
            }
            txs.push(t);
        }
        let total = txs.len() as u32;
        let limit = if filter.limit == 0 {
            usize::MAX
        } else {
            filter.limit as usize
        };
        let txs: Vec<Booking> = txs
            .into_iter()
            .skip(filter.offset as usize)
            .take(limit)
            .collect();
        Ok(BookingPage {
            transactions: txs,
            total_count: total,
            income_cents: income,
            expense_cents: expense,
            consumption_cents: consumption,
            investment_cents: investment,
            perk_cents: perk,
        })
    }

    /// Manual categorisation (wins over rules). `None` = uncategorised.
    pub fn set_transaction_category(
        &self,
        transaction_id: i64,
        category_id: Option<i64>,
    ) -> Result<(), CoreError> {
        let db = self.db()?;
        let n = db.execute(
            "UPDATE transactions SET category_id = ?1, rule_id = NULL, category_source = CASE WHEN ?1 IS NULL THEN 'none' ELSE 'manual' END WHERE id = ?2",
            params![category_id, transaction_id],
        )?;
        if n == 0 {
            return Err(CoreError::NotFound {
                what: trf("Buchung {0}", &[&transaction_id]),
            });
        }
        Ok(())
    }

    pub fn set_transaction_checked(
        &self,
        transaction_id: i64,
        checked: bool,
    ) -> Result<(), CoreError> {
        self.db()?.execute(
            "UPDATE transactions SET checked = ?1 WHERE id = ?2",
            params![checked as i64, transaction_id],
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
            "UPDATE transactions SET counterparty = ?1, purpose = ?2 WHERE id = ?3",
            params![counterparty.trim(), purpose.trim(), transaction_id],
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
            "UPDATE transactions SET note = ?1 WHERE id = ?2",
            params![note.trim(), transaction_id],
        )?;
        if n == 0 {
            return Err(CoreError::NotFound {
                what: trf("Buchung {0}", &[&transaction_id]),
            });
        }
        Ok(())
    }

    // ---------------------------------------------------------------- rules

    pub fn list_rules(&self) -> Result<Vec<Rule>, CoreError> {
        let db = self.db()?;
        Self::load_rules(&db)
    }

    /// Creates (id == 0) or updates a rule. Validates the pattern first.
    pub fn save_rule(&self, rule: Rule) -> Result<i64, CoreError> {
        if rule.pattern.len() > crate::rules::MAX_PATTERN_LEN {
            return Err(CoreError::InvalidInput {
                message: tr("Das Suchmuster ist zu lang.").into(),
            });
        }
        crate::rules::CompiledRule::compile(rule.clone())?;
        if rule.pattern.trim().is_empty() {
            return Err(CoreError::InvalidInput {
                message: tr("Das Suchmuster ist leer.").into(),
            });
        }
        let db = self.db()?;
        let kind = match rule.match_kind {
            MatchKind::Keyword => "keyword",
            MatchKind::Regex => "regex",
            MatchKind::Expression => {
                let expr = crate::search_expr::SearchExpr::parse(&rule.pattern)
                    .map_err(|message| CoreError::InvalidInput { message })?;
                if let Some(f) = expr.unsupported_fields().first() {
                    return Err(CoreError::InvalidInput {
                        message: trf("Das Feld „{0}“ wird nicht unterstützt.", &[f]),
                    });
                }
                "expression"
            }
        };
        if rule.id == 0 {
            db.execute(
                "INSERT INTO rules(name, category_id, field, match_kind, pattern, case_sensitive, min_amount_cents, max_amount_cents, account_id, priority, enabled)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![rule.name, rule.category_id, rule.field.as_str(), kind, rule.pattern, rule.case_sensitive as i64,
                        rule.min_amount_cents, rule.max_amount_cents, rule.account_id, rule.priority, rule.enabled as i64],
            )?;
            Ok(db.last_insert_rowid())
        } else {
            db.execute(
                "UPDATE rules SET name=?1, category_id=?2, field=?3, match_kind=?4, pattern=?5, case_sensitive=?6, min_amount_cents=?7,
                        max_amount_cents=?8, account_id=?9, priority=?10, enabled=?11 WHERE id=?12",
                params![rule.name, rule.category_id, rule.field.as_str(), kind, rule.pattern, rule.case_sensitive as i64,
                        rule.min_amount_cents, rule.max_amount_cents, rule.account_id, rule.priority, rule.enabled as i64, rule.id],
            )?;
            Ok(rule.id)
        }
    }

    pub fn delete_rule(&self, rule_id: i64) -> Result<(), CoreError> {
        let db = self.db()?;
        db.execute("UPDATE transactions SET category_id = NULL, category_source = 'none', rule_id = NULL WHERE rule_id = ?1", [rule_id])?;
        db.execute("DELETE FROM rules WHERE id = ?1", [rule_id])?;
        Ok(())
    }

    /// Transactions the (unsaved) rule would match, for the live preview in the
    /// rule editor.
    pub fn preview_rule(&self, rule: Rule, limit: u32) -> Result<Vec<Booking>, CoreError> {
        let compiled = crate::rules::CompiledRule::compile(Rule {
            enabled: true,
            ..rule
        })?;
        let page = self.list_transactions(BookingFilter {
            limit: 20000,
            ..Default::default()
        })?;
        let ibans: HashMap<i64, String> = {
            let db = self.db()?;
            let mut st = db.prepare("SELECT id, iban FROM accounts")?;
            let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<Result<_, _>>()?
        };
        Ok(page
            .transactions
            .into_iter()
            .filter(|t| {
                compiled.matches(&RuleInput {
                    account_id: t.account_id,
                    amount_cents: t.amount_cents,
                    counterparty: &t.counterparty,
                    counterparty_iban: &t.counterparty_iban,
                    purpose: &t.purpose,
                    booking_text: &t.booking_text,
                    note: &t.note,
                    currency: &t.currency,
                    account_iban: ibans.get(&t.account_id).map_or("", String::as_str),
                })
            })
            .take(limit.max(1) as usize)
            .collect())
    }

    /// Runs all rules. `only_uncategorized = false` also re-evaluates
    /// transactions previously categorised by a rule (never manual ones).
    pub fn apply_rules(&self, only_uncategorized: bool) -> Result<u32, CoreError> {
        let db = self.db()?;
        let tx = db.unchecked_transaction()?;
        let n = Self::apply_rules_inner(&tx, only_uncategorized)?;
        tx.commit()?;
        Ok(n)
    }

    // ---------------------------------------------------------------- settings

    pub fn list_settings(&self) -> Result<Vec<SettingEntry>, CoreError> {
        let db = self.db()?;
        let mut out = vec![];
        for (key, label, help, group, secret, default) in KNOWN_SETTINGS {
            let stored: Option<String> = db
                .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                    r.get(0)
                })
                .optional()?;
            let value = stored.clone().unwrap_or_else(|| default.to_string());
            out.push(SettingEntry {
                key: key.to_string(),
                label: tr(label).to_string(),
                help: tr(help).to_string(),
                group: group.to_string(),
                group_title: tr(group).to_string(),
                value: if *secret {
                    String::new()
                } else {
                    value.clone()
                },
                is_secret: *secret,
                has_value: !value.is_empty(),
            });
        }
        Ok(out)
    }

    pub fn get_setting(&self, key: String) -> Result<Option<String>, CoreError> {
        let db = self.db()?;
        Self::setting_inner(&db, &key)
    }

    /// Stores a setting inside the encrypted database. Empty value deletes it.
    pub fn set_setting(&self, key: String, value: String) -> Result<(), CoreError> {
        let value = Zeroizing::new(value);
        let db = self.db()?;
        if value.trim().is_empty() {
            db.execute("DELETE FROM settings WHERE key = ?1", [&key])?;
        } else {
            let secret = KNOWN_SETTINGS.iter().any(|s| s.0 == key && s.4)
                || key.contains("secret")
                || key.contains("private")
                || key.starts_with("pin.");
            db.execute(
                "INSERT INTO settings(key, value, is_secret) VALUES (?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value, is_secret = excluded.is_secret",
                params![key, value.trim(), secret as i64],
            )?;
        }
        Ok(())
    }
}

impl Vault {
    fn delete_account_rows(db: &Connection, account_id: i64) -> Result<(), CoreError> {
        for sql in [
            "DELETE FROM holdings WHERE account_id = ?1",
            "DELETE FROM booking_documents WHERE account_id = ?1",
            "DELETE FROM transactions WHERE account_id = ?1",
            "DELETE FROM accounts WHERE id = ?1",
        ] {
            db.execute(sql, [account_id])?;
        }
        Ok(())
    }

    fn purge_connection(db: &Connection, connection_id: i64) -> Result<(), CoreError> {
        let ids: Vec<i64> = {
            let mut q = db.prepare("SELECT id FROM accounts WHERE connection_id = ?1")?;
            let rows = q.query_map([connection_id], |r| r.get(0))?;
            rows.collect::<Result<_, _>>()?
        };
        for id in ids {
            Self::delete_account_rows(db, id)?;
        }
        db.execute(
            "DELETE FROM bank_connections WHERE id = ?1",
            [connection_id],
        )?;
        // stored secrets of this connection (PIN); the session lives in state_json
        db.execute(
            "DELETE FROM settings WHERE key = ?1",
            [crate::connections::pin_key(connection_id)],
        )?;
        Ok(())
    }
}

/// Quotes a CSV field (semicolon dialect); text fields are neutralised against
/// spreadsheet formula injection.
fn csv_field(v: &str, text: bool) -> String {
    let mut s = v.replace(['\r', '\n'], " ");
    if text && s.starts_with(['=', '+', '@', '\t']) {
        s.insert(0, '\'');
    }
    if s.contains([';', '"']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s
    }
}

/// Formats cents as `-1234,56` (German decimal comma, no grouping).
fn csv_amount(cents: i64) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    let a = cents.unsigned_abs();
    format!("{sign}{},{:02}", a / 100, a % 100)
}

#[uniffi::export]
impl Vault {
    /// Exports all bookings matching `filter` (its paging is ignored) as UTF-8 CSV
    /// with BOM, semicolon-separated, for Excel.
    pub fn export_bookings_csv(&self, filter: BookingFilter) -> Result<String, CoreError> {
        let mut out = String::from(
            "\u{FEFF}Datum;Konto;Name;IBAN;Verwendungszweck;Kategorie;Betrag;Währung\r\n",
        );
        let page_size = 1000u32;
        let mut offset = 0u32;
        loop {
            let mut f = filter.clone();
            f.limit = page_size;
            f.offset = offset;
            let page = self.list_transactions(f)?;
            let n = page.transactions.len() as u32;
            for b in &page.transactions {
                let row = [
                    csv_field(&b.booking_date, false),
                    csv_field(&b.account_name, true),
                    csv_field(&b.counterparty, true),
                    csv_field(&b.counterparty_iban, false),
                    csv_field(&b.purpose, true),
                    csv_field(&b.category_path, true),
                    csv_amount(b.amount_cents),
                    csv_field(&b.currency, false),
                ];
                out.push_str(&row.join(";"));
                out.push_str("\r\n");
            }
            if n < page_size {
                break;
            }
            offset += n;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod merge_tests {
    use super::*;

    fn seed(v: &Vault, checked: bool, note: &str, extra: bool) {
        let c = v.db().unwrap();
        c.execute(
            "INSERT OR IGNORE INTO account_groups(name) VALUES ('G')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT OR IGNORE INTO accounts(group_id, external_ref, name) \
             VALUES ((SELECT id FROM account_groups WHERE name='G'), 'A1', 'Konto')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, checked, note) \
             VALUES ((SELECT id FROM accounts LIMIT 1), 'T1', '2024-01-01', -100, ?1, ?2)",
            params![checked, note],
        )
        .unwrap();
        if extra {
            c.execute("INSERT INTO categories(name) VALUES ('Food')", [])
                .unwrap();
            c.execute(
                "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents) \
                 VALUES ((SELECT id FROM accounts LIMIT 1), 'T2', '2024-01-02', -5)",
                [],
            )
            .unwrap();
        }
    }

    #[test]
    fn merges_without_duplicates() {
        let dir = std::env::temp_dir().join(format!("gc-merge-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let key = vec![7u8; 32];
        let a = Vault::open(dir.join("a.db").to_string_lossy().into(), key.clone()).unwrap();
        let b = Vault::open(dir.join("b.db").to_string_lossy().into(), key.clone()).unwrap();
        seed(&a, false, "", false);
        seed(&b, true, "hello", true);
        let st = a.merge_other_vault(b.file_path(), key.clone()).unwrap();
        assert_eq!(st.bookings_added, 1);
        assert_eq!(st.bookings_updated, 1);
        assert_eq!(st.categories_added, 1);
        assert_eq!(st.accounts_added, 0);
        let (n, checked, note): (i64, bool, String) = a
            .db()
            .unwrap()
            .query_row(
                "SELECT (SELECT count(*) FROM transactions), checked, note FROM transactions WHERE external_id='T1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((n, checked, note.as_str()), (2, true, "hello"));
        // idempotent
        let st2 = a.merge_other_vault(b.file_path(), key).unwrap();
        assert_eq!((st2.bookings_added, st2.bookings_updated), (0, 0));
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod expression_rule_tests {
    use super::*;

    #[test]
    fn expression_rules_round_trip_and_apply() {
        let p = std::env::temp_dir().join(format!("girocrab-expr-{}.db", uuid::Uuid::new_v4()));
        let v = Vault::open(p.to_string_lossy().to_string(), vec![5u8; 32]).unwrap();
        let cat = {
            let c = v.db().unwrap();
            c.execute(
                "INSERT OR IGNORE INTO account_groups(name) VALUES ('G')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO accounts(group_id, external_ref, name, iban) \
                 VALUES ((SELECT id FROM account_groups LIMIT 1), 'A1', 'Konto', 'DE02120300000000202051')",
                [],
            )
            .unwrap();
            c.execute("INSERT INTO categories(name) VALUES ('Lebensmittel')", [])
                .unwrap();
            for (ext, cp, amount) in [
                ("T1", "REWE Markt", -2350),
                ("T2", "REWE Markt", 500),
                ("T3", "Lidl", -100),
            ] {
                c.execute(
                    "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, counterparty) \
                     VALUES ((SELECT id FROM accounts LIMIT 1), ?1, '2026-01-02', ?2, ?3)",
                    params![ext, amount, cp],
                )
                .unwrap();
            }
            c.query_row(
                "SELECT id FROM categories WHERE name = 'Lebensmittel'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
        };
        let rule = Rule {
            id: 0,
            name: "REWE".into(),
            category_id: cat,
            field: RuleField::Any,
            match_kind: MatchKind::Expression,
            pattern: "name:REWE AND amount<0 AND local_account:DE02*".into(),
            case_sensitive: false,
            min_amount_cents: None,
            max_amount_cents: None,
            account_id: None,
            priority: 0,
            enabled: true,
            match_count: 0,
        };
        let id = v.save_rule(rule.clone()).unwrap();
        let listed = v.list_rules().unwrap();
        let saved = listed.iter().find(|r| r.id == id).unwrap();
        assert_eq!(saved.match_kind, MatchKind::Expression);
        assert_eq!(v.preview_rule(rule.clone(), 10).unwrap().len(), 1);
        v.apply_rules(true).unwrap();
        let hits: i64 = v
            .db()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM transactions WHERE rule_id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1, "only the REWE debit");
        let err = v.save_rule(Rule {
            pattern: "reference:X".into(),
            ..rule
        });
        assert!(err.is_err(), "unsupported field can't be saved");
        let _ = std::fs::remove_file(p);
    }
}

#[cfg(test)]
mod upgrade_tests {
    use super::*;

    /// A vault written before #35/#37/#57 (no bank codes, salary flag,
    /// recurring table or mirror column) opens, migrates and keeps its data.
    #[test]
    fn pre_feature_vault_is_migrated_in_place() {
        let dir = std::env::temp_dir().join(format!("gc-upgrade-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("v.db").to_string_lossy().to_string();
        {
            let v = Vault::open(path.clone(), vec![2u8; 32]).unwrap();
            let c = v.db().unwrap();
            c.execute("INSERT INTO account_groups(name) VALUES ('G')", [])
                .unwrap();
            c.execute(
                "INSERT INTO accounts(group_id, external_ref, name) VALUES ((SELECT id FROM account_groups), 'A', 'Giro')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, counterparty, purpose)
                 VALUES ((SELECT id FROM accounts), 'x', date('now','-3 days'), 250000, 'ACME', 'Gehalt')",
                [],
            )
            .unwrap();
            c.execute_batch(
                "ALTER TABLE transactions DROP COLUMN purpose_code;
                 ALTER TABLE transactions DROP COLUMN gvc;
                 ALTER TABLE transactions DROP COLUMN salary_flag;
                 ALTER TABLE booking_documents DROP COLUMN exported_at;
                 DROP TABLE recurring_series;",
            )
            .unwrap();
        }
        let v = Vault::open(path, vec![2u8; 32]).unwrap();
        let cols: Vec<String> = {
            let c = v.db().unwrap();
            let mut st = c
                .prepare("SELECT name FROM pragma_table_info('transactions')")
                .unwrap();
            let names = st
                .query_map([], |r| r.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            names
        };
        for col in ["purpose_code", "gvc", "salary_flag"] {
            assert!(cols.iter().any(|c| c == col), "{col} missing");
        }
        let page = v.list_transactions(BookingFilter::default()).unwrap();
        assert_eq!(page.transactions.len(), 1);
        assert!(page.transactions[0].is_salary);
        assert!(v.salary_cycle(vec![]).unwrap().is_some());
        assert!(v.list_recurring(vec![]).unwrap().is_empty());
        v.set_booking_salary_flag(page.transactions[0].id, Some(false))
            .unwrap();
        v.reset_document_mirror().unwrap();
        drop(v);
        let _ = std::fs::remove_dir_all(dir);
    }
}
