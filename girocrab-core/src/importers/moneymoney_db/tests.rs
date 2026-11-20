//! Tests with fake MoneyMoney databases (real column layout, invented data,
//! SQLCipher-encrypted with a test password).

use super::*;
use crate::iban::{normalize_iban, validate_iban};
use crate::models::{BookingFilter, CategorySource, MatchKind};
use std::path::PathBuf;
use std::sync::Arc;

pub(crate) const PW: &str = "Test-Passwort 'mit' Anführungszeichen";

/// The relevant MoneyMoney tables with their real columns.
const SCHEMA: &str = r#"
CREATE TABLE account_groups (rowid INTEGER PRIMARY KEY, name TEXT, perspective TEXT, group_key INTEGER, sort INTEGER, icon TEXT, expanded INTEGER, sidebar INTEGER, userdata BLOB);
CREATE TABLE accounts (rowid INTEGER PRIMARY KEY, bank_key INTEGER, account TEXT, subaccount TEXT, bankcode TEXT, portfolio INTEGER, iban TEXT, bic TEXT, currency TEXT, name TEXT, owner TEXT, kind INTEGER, type INTEGER, sort INTEGER, icon TEXT, balances BLOB, pending_balances BLOB, overdraft_limit REAL, available_funds REAL, initial_balance REAL, cub INTEGER, cub_tsp INTEGER, recipient_key INTEGER, refresh_tsp INTEGER, balance_tsp INTEGER, transaction_tsp INTEGER, payment_tsp INTEGER, error_tsp INTEGER, cancel_tsp INTEGER, statements INTEGER, refresh_actions INTEGER, refresh INTEGER, userdata BLOB, cred TEXT, transientdata BLOB, perspective TEXT, group_key INTEGER, error TEXT, web_ops BLOB);
CREATE TABLE balances (account_key INTEGER, currency TEXT, timestamp INTEGER, initial_balance REAL, PRIMARY KEY (account_key, currency, timestamp DESC));
CREATE TABLE banks (rowid INTEGER PRIMARY KEY, country TEXT, bankcode TEXT, username TEXT, customer TEXT, credential TEXT, name TEXT, server TEXT, protocol REAL, tanmethod TEXT, tanmedium TEXT, systemid TEXT, tanmethods TEXT, bpd BLOB, upd BLOB, spa INTEGER, spa_tsp INTEGER, local_storage TEXT, pubkey BLOB, privkey BLOB, userdata BLOB);
CREATE TABLE categories (rowid INTEGER PRIMARY KEY, name TEXT, type INTEGER, perspective TEXT, group_key INTEGER, sort INTEGER, color TEXT, rules TEXT, apply_to_account TEXT, currency TEXT, is_valid_sum INTEGER, userdata BLOB);
CREATE TABLE category_groups (rowid INTEGER PRIMARY KEY, name TEXT, perspective TEXT, group_key INTEGER, sort INTEGER, color TEXT, expanded INTEGER, currency TEXT, userdata BLOB);
CREATE TABLE payments (rowid INTEGER PRIMARY KEY, version REAL, uuid BLOB, tombstone INTEGER, type INTEGER, sender INTEGER, recipient INTEGER, name TEXT, purpose TEXT, amount REAL, currency TEXT, category INTEGER, scheduled_or_transmitted INTEGER, standing_order_end INTEGER, "transaction" INTEGER, template_group INTEGER, data BLOB);
CREATE TABLE pins (bank_key INTEGER PRIMARY KEY, pin TEXT);
CREATE TABLE recipients (local_account_key INTEGER, name TEXT, owner TEXT, account TEXT, subaccount TEXT, bankcode TEXT, iban TEXT, bic TEXT, type INTEGER);
CREATE TABLE securities (rowid INTEGER PRIMARY KEY, account_key INTEGER, ident TEXT, isin TEXT, wpn TEXT, symbol TEXT, quantity REAL, quantity_cur TEXT, amount REAL, amount_cur TEXT, ocmt REAL, ocmt_cur TEXT, purchase REAL, purchase_cur TEXT, price REAL, price_cur TEXT, exch REAL, trade_date INTEGER, trade_tsp INTEGER, purchase_tsp INTEGER, market TEXT, comment TEXT, userdata BLOB, visited INTEGER, deleted INTEGER, asset_class INTEGER);
CREATE TABLE template_groups (rowid INTEGER PRIMARY KEY, version REAL, uuid BLOB, tombstone INTEGER, data BLOB);
CREATE TABLE statements (rowid INTEGER PRIMARY KEY, account_key INTEGER, name TEXT, directory TEXT, filename TEXT, created_tsp INTEGER, fetched_tsp INTEGER, orderid INTEGER, userorder INTEGER, imported INTEGER, visited INTEGER, deleted INTEGER, number TEXT, year TEXT, identifier BLOB, digest TEXT);
CREATE TABLE transactions (rowid INTEGER PRIMARY KEY, local_account_key INTEGER, account TEXT, bankcode TEXT, amount REAL, currency TEXT, timestamp INTEGER, value_timestamp INTEGER, pn TEXT, eref TEXT, mref TEXT, kref TEXT, cred TEXT, dept TEXT, rtcd TEXT, rtrn TEXT, ocmt REAL, ocmt_cur TEXT, chgs REAL, chgs_cur TEXT, coam REAL, coam_cur TEXT, oamt REAL, oamt_cur TEXT, exch REAL, abwe TEXT, abwa TEXT, seq TEXT, comment TEXT, attachment TEXT, booked INTEGER, nopretty INTEGER, unformatted_type TEXT, unformatted_name TEXT, unformatted_purpose TEXT, identifier TEXT, name TEXT, a1490107618 TEXT, visited INTEGER, checkmark INTEGER, splitted INTEGER, split_key INTEGER, reverse_key INTEGER, category_key INTEGER);
"#;

// Invented data only (public example IBANs, fictional names).
const DATA: &str = r#"
INSERT INTO banks (rowid, country, bankcode, username, customer, credential, name, systemid, local_storage) VALUES
  (1, 'DE', '20041111', '1234567', '1234567', 'GEHEIM-1', 'comdirect', 'SYS', 'LS'),
  (2, 'DE', '52090000', 'VRNK4711', 'VRNK4711', 'GEHEIM-2', 'Volksbank Kassel Göttingen', 'SYS', 'LS'),
  (3, 'DE', '12345678', 'max.m', '', 'GEHEIM-3', 'Sparkasse Musterstadt', 'SYS', 'LS');
INSERT INTO pins (bank_key, pin) VALUES (1, '11111'), (2, '22222'), (3, '33333');
INSERT INTO account_groups (rowid, name, group_key, sort) VALUES (1, 'Privat', 0, 1);
INSERT INTO accounts (rowid, bank_key, account, bankcode, portfolio, iban, currency, name, owner, kind, type, sort, initial_balance, group_key, cred) VALUES
  (1, 1, '0532013000', '20041111', 0, 'DE89 3704 0044 0532 0130 00', 'EUR', 'Girokonto', 'Max Mustermann', 0, 0, 1, 50.0, 1, 'GEHEIM'),
  (2, 1, '4711', '20041111', 0, '', 'EUR', 'Visa-Karte', 'Max Mustermann', 0, 3, 2, 0, 1, 'GEHEIM'),
  (3, 1, '9999', '20041111', 1, '', 'EUR', 'Depot', 'Max Mustermann', 0, 7, 3, 0, 0, 'GEHEIM'),
  (4, 3, '1000', '12345678', 0, '', 'USD', 'Dollar-Konto', 'Max Mustermann', 0, 0, 4, 10.5, 0, 'GEHEIM');
INSERT INTO balances (account_key, currency, timestamp, initial_balance) VALUES
  (1, 'EUR', 1104580800, 80.0),
  (1, 'EUR', 1577880000, 100.0);
INSERT INTO category_groups (rowid, name, group_key, sort) VALUES
  (1, 'Haushalt', 0, 1), (2, 'Wohnen', 0, 2), (3, 'Nebenkosten', 2, 1);
INSERT INTO categories (rowid, name, type, group_key, sort) VALUES
  (1, '', 0, 0, 0),
  (2, 'Supermarkt', 1, 1, 1),
  (3, 'Gehalt', 1, 0, 2),
  (4, 'Strom', 1, 3, 1),
  (5, 'Steuern / Büro', 1, 0, 3);
INSERT INTO transactions (rowid, local_account_key, account, bankcode, amount, currency, timestamp, value_timestamp, pn, eref, mref, cred, comment, booked, unformatted_type, unformatted_purpose, name, checkmark, splitted, split_key, category_key) VALUES
  (1, 1, 'DE02120300000000202051', 'BYLADEM1001', 2500.0, 'EUR', 1104580800, 1104580800, 'X', '', '', '', 'erstes Gehalt', 1, 'GUTSCHRIFT', 'Gehalt Januar', 'Arbeitgeber GmbH', 1, 0, 1, 3),
  (2, 1, 'DE02100100100006820101', 'PBNKDEFFXXX', -45.67, 'EUR', 1772712000, 1772712000, '', 'E2E-1', 'MANDAT-1', 'DE98ZZZ09999999999', '', 1, 'LASTSCHRIFT', 'Einkauf Filiale 12', 'REWE Markt', 0, 0, 2, 2),
  (3, 1, '12345', '10010010', -12.3, 'EUR', 1772798400, 0, '', '', '', '', '', 1, 'KARTENZAHLUNG', 'Brötchen', 'Bäckerei Schmidt', 0, 0, 3, 1),
  (4, 1, '', '', -9.99, 'EUR', 1772971200, 0, '', '', '', '', '', 0, 'VORMERKUNG', 'Vorgemerkt', 'Streaming AG', 0, 0, 4, 1),
  (5, 1, '', '', -100.0, 'EUR', 1772884800, 0, '', '', '', '', '', 1, 'KARTENZAHLUNG', 'Einkauf', 'Kaufhaus Muster', 0, 1, 5, 1),
  (6, 1, '', '', -60.0, 'EUR', 1772884800, 0, '', '', '', '', '', 1, 'KARTENZAHLUNG', 'Einkauf Lebensmittel', 'Kaufhaus Muster', 0, 0, 5, 2),
  (7, 1, '', '', -40.0, 'EUR', 1772884800, 0, '', '', '', '', '', 1, 'KARTENZAHLUNG', 'Einkauf Lampe', 'Kaufhaus Muster', 0, 0, 5, 4),
  (8, 2, '', '', -19.99, 'EUR', 1709208000, 0, '', '', '', '', 'Abo kündigen', 1, 'KARTENZAHLUNG', 'Monatsabo', 'Streaming AG', 0, 0, 8, 5),
  (9, 4, '', '', 25.255, 'USD', 1772712000, 0, '', '', '', '', '', 1, 'GUTSCHRIFT', 'Dividende', 'Muster Inc', 0, 0, 9, 1);
INSERT INTO recipients (local_account_key, name, iban) VALUES (1, 'Erika Mustermann', 'DE02120300000000202051');
-- Depot: balance only in the displayed-balance blob, positions in securities (one sold)
UPDATE accounts SET balances = CAST('12:12345.670000,3:EUR,' AS BLOB), balance_tsp = 1772884800 WHERE rowid = 3;
INSERT INTO securities (rowid, account_key, ident, isin, wpn, quantity, amount, amount_cur, purchase, price, trade_date, deleted) VALUES
  (1, 3, 'Muster World ETF', 'IE00B4L5Y983', 'A0RPWH', 12.5, 1000.0, 'EUR', 70.0, 80.0, 1772884800, 0),
  (2, 3, 'Liquidität', '', '', NULL, 345.67, 'EUR', NULL, NULL, 1772884800, 0),
  (3, 3, 'Verkauft AG', 'DE0000000001', '', 1, 1, 'EUR', 1, 1, 1772884800, 1);
"#;

fn temp(name: &str) -> PathBuf {
    crate::test_support::thread_temp_path(&format!("mmdb-{name}"))
}

fn fake_db_with(compat3: bool, extra: &str) -> String {
    let path = temp("src.sqlite");
    let c = Connection::open(&path).unwrap();
    c.execute_batch(&format!("PRAGMA key = '{}';", PW.replace('\'', "''")))
        .unwrap();
    if compat3 {
        c.execute_batch("PRAGMA cipher_compatibility = 3;").unwrap();
    }
    c.execute_batch(SCHEMA).unwrap();
    c.execute_batch(DATA).unwrap();
    c.execute_batch(extra).unwrap();
    path.to_string_lossy().to_string()
}

fn fake_db_unencrypted() -> String {
    let path = temp("plain-src.sqlite");
    let c = Connection::open(&path).unwrap();
    c.execute_batch(SCHEMA).unwrap();
    c.execute_batch(DATA).unwrap();
    path.to_string_lossy().to_string()
}

pub(crate) fn fake_db() -> String {
    fake_db_with(false, "")
}

fn vault() -> Arc<Vault> {
    Vault::open(
        temp("vault.db").to_string_lossy().to_string(),
        vec![7u8; 32],
    )
    .unwrap()
}

/// Options as the UI would send them: every account, suggested kind and target.
pub(crate) fn options_from(
    p: &MoneyMoneyDbPreview,
    connections: bool,
) -> MoneyMoneyDbImportOptions {
    MoneyMoneyDbImportOptions {
        accounts: p
            .accounts
            .iter()
            .map(|a| MoneyMoneyDbAccountChoice {
                mm_id: a.mm_id,
                import: true,
                kind: a.suggested_kind,
                target_account_id: a.existing_account_id,
            })
            .collect(),
        import_categories: true,
        import_notes: true,
        create_connections: connections,
        import_groups: true,
        import_statements: true,
        statements_dir: None,
        bank_choices: vec![],
        import_transfer_templates: true,
    }
}

/// The Enable Banking option the preview offers for a bank, if any.
fn eb_option(p: &MoneyMoneyDbPreview, bank: i64) -> Option<MoneyMoneyDbConnectionOption> {
    p.banks
        .iter()
        .find(|b| b.mm_id == bank)?
        .connection_options
        .iter()
        .find(|o| o.via == MoneyMoneyDbConnectionVia::EnableBanking)
        .cloned()
}

/// (amount, category name, note) of the splits of booking `id`.
fn split_lines(v: &Vault, id: i64) -> Vec<(i64, String, String)> {
    let db = v.db().unwrap();
    let mut q = db
        .prepare(
            "SELECT s.amount_cents, IFNULL(c.name, ''), s.note FROM transaction_splits s
               LEFT JOIN categories c ON c.id = s.category_id
              WHERE s.transaction_id = ?1 ORDER BY s.id",
        )
        .unwrap();
    q.query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn bookings(v: &Vault, account_id: i64) -> Vec<crate::models::Booking> {
    v.list_transactions(BookingFilter {
        account_ids: vec![account_id],
        ..Default::default()
    })
    .unwrap()
    .transactions
}

fn account_by_name(v: &Vault, name: &str) -> crate::models::Account {
    v.list_accounts()
        .unwrap()
        .into_iter()
        .find(|a| a.name == name)
        .unwrap()
}

#[test]
fn amount_and_date_conversion_edge_cases() {
    assert_eq!(real_to_cents(1234.56), Some(123_456));
    assert_eq!(real_to_cents(-45.67), Some(-4567));
    assert_eq!(real_to_cents(0.285), Some(29)); // (0.285 * 100.0).round() would give 28
    assert_eq!(real_to_cents(-0.285), Some(-29));
    assert_eq!(real_to_cents(0.1 + 0.2), Some(30));
    assert_eq!(real_to_cents(-0.004), Some(0));
    assert_eq!(real_to_cents(-0.005), Some(-1));
    assert_eq!(real_to_cents(1e-9), Some(0));
    assert_eq!(real_to_cents(99_999_999.99), Some(9_999_999_999));
    assert_eq!(real_to_cents(f64::NAN), None);
    assert_eq!(real_to_cents(f64::INFINITY), None);
    assert_eq!(unix_to_date(1104580800).as_deref(), Some("2005-01-01"));
    assert_eq!(unix_to_date(1709208000).as_deref(), Some("2024-02-29"));
    // 12:00 UTC is the convention; the whole UTC day maps to the same date
    assert_eq!(
        unix_to_date(1104580800 - 12 * 3600).as_deref(),
        Some("2005-01-01")
    );
    assert_eq!(
        unix_to_date(1104580800 + 12 * 3600 - 1).as_deref(),
        Some("2005-01-01")
    );
    assert_eq!(unix_to_date(0).as_deref(), Some("1970-01-01"));
}

#[test]
fn wrong_password_and_missing_file() {
    let path = fake_db();
    let v = vault();
    let err = v
        .preview_moneymoney_database(path.clone(), "falsch".into(), None)
        .unwrap_err();
    assert!(matches!(err, CoreError::Authentication { .. }));
    assert!(err.to_string().contains("Falsches Passwort"));
    assert!(!err.to_string().contains("falsch\""));
    let err = v
        .preview_moneymoney_database(path, String::new(), None)
        .unwrap_err();
    assert!(matches!(err, CoreError::Authentication { .. }));
    let err = v
        .preview_moneymoney_database("/nonexistent/MoneyMoney.sqlite".into(), PW.into(), None)
        .unwrap_err();
    assert!(matches!(err, CoreError::NotFound { .. }));
    // an unencrypted SQLite file that is not MoneyMoney's
    let plain = temp("plain.sqlite");
    Connection::open(&plain)
        .unwrap()
        .execute_batch("CREATE TABLE x (a);")
        .unwrap();
    let err = v
        .preview_moneymoney_database(plain.to_string_lossy().to_string(), PW.into(), None)
        .unwrap_err();
    assert!(matches!(err, CoreError::Authentication { .. }));
}

#[test]
fn unencrypted_database_opens_with_empty_password() {
    let path = fake_db_unencrypted();
    let v = vault();
    let p = v
        .preview_moneymoney_database(path.clone(), String::new(), None)
        .unwrap();
    assert_eq!(p.accounts.len(), 4);
    // a password typed for an unencrypted file is ignored, not "wrong"
    let p = v
        .preview_moneymoney_database(path, PW.into(), None)
        .unwrap();
    assert_eq!(p.accounts.len(), 4);
}

#[test]
fn encrypted_database_without_password_explains_moneymoney_key() {
    // MoneyMoney encrypts with a key of its own when no database password
    // is set; an empty password must say so instead of "wrong password".
    let enc = fake_db();
    let err = vault()
        .preview_moneymoney_database(enc, String::new(), None)
        .unwrap_err();
    assert!(matches!(err, CoreError::Authentication { .. }));
    let msg = err.to_string();
    assert!(
        msg.contains("(nicht in GiroCrab)") && msg.contains("Datenbank-Passwort"),
        "{msg}"
    );
    assert!(!msg.contains("Falsches Passwort"), "{msg}");
}

#[test]
fn plain_sqlite_header_detection() {
    let plain = fake_db_unencrypted();
    assert!(is_plain_sqlite(&plain));
    assert!(!is_plain_sqlite(&fake_db()));
    // too short / missing files are not plain SQLite
    let short = temp("short.sqlite");
    std::fs::write(&short, b"SQLite").unwrap();
    assert!(!is_plain_sqlite(&short.to_string_lossy()));
    assert!(!is_plain_sqlite("/nonexistent/MoneyMoney.sqlite"));
}

#[test]
fn older_cipher_compatibility_3_opens() {
    let path = fake_db_with(true, "");
    let p = vault()
        .preview_moneymoney_database(path, PW.into(), None)
        .unwrap();
    assert_eq!(p.accounts.len(), 4);
}

#[test]
fn source_file_bytes_unchanged_after_preview_and_import() {
    let path = fake_db();
    let before = std::fs::read(&path).unwrap();
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    let v = vault();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    v.import_moneymoney_database(path.clone(), PW.into(), options_from(&p, true))
        .unwrap();
    v.import_moneymoney_database(path.clone(), PW.into(), options_from(&p, true))
        .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        modified
    );
    // and the connection really is read-only
    let c = open_source(&path, &SecretString::new(PW.into())).unwrap();
    assert!(c.execute("DELETE FROM pins", []).is_err());
}

#[test]
fn preview_lists_accounts_banks_categories_and_totals() {
    let v = vault();
    let p = v
        .preview_moneymoney_database(fake_db(), PW.into(), None)
        .unwrap();
    let giro = p.accounts.iter().find(|a| a.mm_id == 1).unwrap();
    assert_eq!(giro.iban, "DE89370400440532013000");
    assert_eq!(giro.group_name, "Privat");
    assert_eq!(giro.bank_name, "comdirect");
    assert_eq!(giro.suggested_kind, AccountKind::Checking);
    // 4 = salary, REWE, bakery, the split booking (its parts + pending not counted)
    assert_eq!((giro.booking_count, giro.pending_count), (4, 1));
    assert_eq!(giro.first_date.as_deref(), Some("2005-01-01"));
    assert_eq!(giro.last_date.as_deref(), Some("2026-03-07"));
    // newest `balances` row (100,00, 2020-01-01) + booked bookings from that
    // date on (parent, not parts); the 2005 salary is part of the initial balance
    assert_eq!(giro.computed_balance_cents, 10_000 - 4567 - 1230 - 10_000);
    assert!(giro.balance_source.contains("Saldenverlauf"));
    let kind = |id: i64| {
        p.accounts
            .iter()
            .find(|a| a.mm_id == id)
            .unwrap()
            .suggested_kind
    };
    assert_eq!(kind(2), AccountKind::CreditCard);
    assert_eq!(kind(3), AccountKind::Brokerage);
    let usd = p.accounts.iter().find(|a| a.mm_id == 4).unwrap();
    assert_eq!(usd.currency, "USD");
    // accounts.initial_balance (10,50) + 25,255 → 25,26
    assert_eq!(usd.computed_balance_cents, 1050 + 2526);
    assert_eq!(
        p.category_paths,
        vec![
            "Gehalt",
            "Steuern / Büro",
            "Haushalt › Supermarkt",
            "Wohnen › Nebenkosten › Strom"
        ]
    );
    assert_eq!(
        (p.total_bookings, p.pending_bookings, p.split_parts),
        (6, 1, 0)
    );
    assert_eq!(p.notes_count, 2);
    assert_eq!(p.date_range_start.as_deref(), Some("2005-01-01"));
    let bank = |id: i64| p.banks.iter().find(|b| b.mm_id == id).unwrap();
    assert_eq!(bank(1).connector, Some(ConnectorKind::Comdirect));
    assert_eq!(bank(1).login_name, "1234567");
    assert_eq!(
        bank(2).connector,
        Some(ConnectorKind::VolksbankKasselGoettingen)
    );
    assert_eq!(bank(3).connector, None);
    assert_eq!(bank(1).account_count, 3);
    // the preview never stores anything
    assert!(v.list_accounts().unwrap().is_empty());
    assert!(v.list_bank_connections().unwrap().is_empty());
}

#[test]
fn import_categories_splits_pending_notes_and_balance() {
    let v = vault();
    let path = fake_db();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    let r = v
        .import_moneymoney_database(path, PW.into(), options_from(&p, false))
        .unwrap();
    assert_eq!((r.accounts_created, r.accounts_merged), (4, 0));
    assert_eq!(
        (r.bookings_new, r.bookings_duplicates, r.pending_skipped),
        (6, 0, 1)
    );
    assert_eq!(r.notes_imported, 2);
    // Haushalt, Supermarkt, Gehalt, Wohnen, Nebenkosten, Strom, Steuern / Büro
    assert_eq!(r.categories_created, 7);
    assert_eq!(r.connections_created, 0);

    let giro = account_by_name(&v, "Girokonto");
    assert_eq!(giro.iban, "DE89370400440532013000");
    assert_eq!(giro.bank_name, "comdirect");
    assert_eq!(giro.balance_cents, 10_000 - 4567 - 1230 - 10_000);
    assert_eq!(giro.balance_date, "2026-03-07");
    let bs = bookings(&v, giro.id);
    assert_eq!(bs.len(), 4);
    let find = |name: &str, cents: i64| {
        bs.iter()
            .find(|b| b.counterparty == name && b.amount_cents == cents)
            .unwrap_or_else(|| panic!("{name} {cents}"))
    };
    // grouped category path
    let rewe = find("REWE Markt", -4567);
    assert_eq!(rewe.category_path, "Haushalt › Supermarkt");
    assert_eq!(rewe.counterparty_iban, "DE02100100100006820101");
    assert_eq!(rewe.booking_text, "LASTSCHRIFT");
    assert!(rewe.purpose.starts_with("Einkauf Filiale 12"));
    assert!(rewe.purpose.contains("MREF: MANDAT-1"));
    assert!(rewe.purpose.contains("CRED: DE98ZZZ09999999999"));
    assert!(rewe.purpose.contains("EREF: E2E-1"));
    assert_eq!(rewe.booking_date, "2026-03-05");
    // uncategorised in MoneyMoney → no MoneyMoney category
    let bakery = find("Bäckerei Schmidt", -1230);
    assert_ne!(bakery.category_source, CategorySource::Manual);
    assert_eq!(bakery.counterparty_iban, ""); // "12345" is no IBAN
    assert_eq!(bakery.value_date, "");
    // note
    let salary = find("Arbeitgeber GmbH", 250_000);
    assert_eq!(salary.note, "erstes Gehalt");
    assert_eq!(salary.category_path, "Gehalt");
    assert_eq!(salary.booking_date, "2005-01-01");
    // #877: the split booking is one booking, its parts are splits with their
    // categories and, as note, the purpose where it differs from the booking's
    let whole = find("Kaufhaus Muster", -10_000);
    assert_eq!(
        split_lines(&v, whole.id),
        vec![
            (-6000, "Supermarkt".into(), "Einkauf Lebensmittel".into()),
            (-4000, "Strom".into(), "Einkauf Lampe".into()),
        ]
    );
    assert!(
        !bs.iter()
            .any(|b| b.amount_cents == -6000 || b.amount_cents == -4000)
    );
    // pending not imported
    assert!(!bs.iter().any(|b| b.counterparty == "Streaming AG"));

    // category name containing " / " stays one category
    let visa = account_by_name(&v, "Visa-Karte");
    assert_eq!(visa.kind, AccountKind::CreditCard);
    let vb = bookings(&v, visa.id);
    assert_eq!(vb[0].category_path, "Steuern / Büro");
    assert_eq!(vb[0].note, "Abo kündigen");
    let groups = v.list_account_groups().unwrap();
    assert!(
        groups
            .iter()
            .any(|g| g.name == "Kreditkarten" && g.accounts.iter().any(|a| a.id == visa.id))
    );
    assert!(
        groups
            .iter()
            .any(|g| g.name == "Depots & ETFs" && g.accounts.iter().any(|a| a.name == "Depot"))
    );
    let usd = account_by_name(&v, "Dollar-Konto");
    assert_eq!(usd.currency, "USD");
    assert_eq!(bookings(&v, usd.id)[0].amount_cents, 2526);

    // #212: MoneyMoney's opening balances carry over undated (MoneyMoney has
    // no cut-off date) and keep driving the balance. The balance-history row
    // (100 € on 2020-01-01) is moved before the 2005 salary of 2500 €.
    assert!(giro.balance_derived);
    assert_eq!(
        (
            giro.opening_balance_cents,
            giro.opening_balance_date.as_str()
        ),
        (Some(10_000 - 250_000), "")
    );
    assert_eq!(giro.balance_cents, 10_000 - 4567 - 1230 - 10_000);
    assert_eq!(
        (usd.opening_balance_cents, usd.opening_balance_date.as_str()),
        (Some(1050), "")
    );
    assert_eq!(usd.balance_cents, 1050 + 2526);
    assert_eq!(visa.opening_balance_cents, None);
    assert_eq!(account_by_name(&v, "Depot").opening_balance_cents, None);
    v.calibrate_account_opening_balance(giro.id, 0).unwrap();
    let giro = account_by_name(&v, "Girokonto");
    assert_eq!(
        giro.opening_balance_cents,
        Some(4567 + 1230 + 10_000 - 250_000)
    );
    assert_eq!(giro.balance_cents, 0);
}

#[test]
fn reimport_is_idempotent() {
    let v = vault();
    let path = fake_db();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    let first = v
        .import_moneymoney_database(path.clone(), PW.into(), options_from(&p, true))
        .unwrap();
    let count = |v: &Vault| {
        let db = v.db().unwrap();
        let q = |sql: &str| db.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
        (
            q("SELECT count(*) FROM accounts"),
            q("SELECT count(*) FROM transactions"),
            q("SELECT count(*) FROM categories"),
            q("SELECT count(*) FROM bank_connections"),
            q("SELECT count(*) FROM settings WHERE key LIKE 'pin.conn.%'"),
        )
    };
    let after_first = count(&v);
    // second run with the options a fresh preview suggests …
    let p2 = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    assert!(p2.accounts.iter().all(|a| a.existing_account_id.is_some()));
    assert!(
        p2.banks[..2]
            .iter()
            .all(|b| b.existing_connection_id.is_some())
    );
    let second = v
        .import_moneymoney_database(path.clone(), PW.into(), options_from(&p2, true))
        .unwrap();
    // … and a third one that still says "new account" everywhere
    let third = v
        .import_moneymoney_database(path, PW.into(), options_from(&p, true))
        .unwrap();
    for r in [&second, &third] {
        assert_eq!((r.accounts_created, r.accounts_merged), (0, 4));
        assert_eq!(
            (r.bookings_new, r.bookings_duplicates),
            (0, first.bookings_new)
        );
        assert_eq!((r.categories_created, r.connections_created), (0, 0));
        assert_eq!(r.connections_existing, 2);
    }
    assert_eq!(count(&v), after_first);
    assert_eq!(
        account_by_name(&v, "Girokonto").balance_cents,
        10_000 - 4567 - 1230 - 10_000
    );
}

/// Imports `path` into a fresh vault; the Girokonto's id.
fn import_giro(v: &Vault, path: &str, categories: bool) -> i64 {
    import_with(v, path, |o| o.import_categories = categories);
    account_by_name(v, "Girokonto").id
}

/// Preview + import of `path` with the UI's options, adjusted by `tweak`.
fn import_with(
    v: &Vault,
    path: &str,
    tweak: impl FnOnce(&mut MoneyMoneyDbImportOptions),
) -> MoneyMoneyDbImportResult {
    let p = v
        .preview_moneymoney_database(path.into(), PW.into(), None)
        .unwrap();
    let mut o = options_from(&p, false);
    tweak(&mut o);
    v.import_moneymoney_database(path.into(), PW.into(), o)
        .unwrap()
}

/// The Girokonto's only booking of `cents`.
fn one_booking(v: &Vault, account: i64, cents: i64) -> crate::models::Booking {
    let mut bs: Vec<_> = bookings(v, account)
        .into_iter()
        .filter(|b| b.amount_cents == cents)
        .collect();
    assert_eq!(bs.len(), 1, "{cents}");
    bs.remove(0)
}

#[test]
fn bank_adopts_the_split_booking() {
    let v = vault();
    let giro = import_giro(&v, &fake_db(), true);
    let whole = one_booking(&v, giro, -10_000);
    let db = v.db().unwrap();
    // the bank delivers the whole amount, which no single part had (#877)
    assert!(
        crate::importers::moneymoney_import::adopt_import(
            &db,
            giro,
            "bank-9",
            "2026-03-07",
            -10_000,
            "",
            "Kaufhaus Muster",
            "Einkauf",
            "KARTENZAHLUNG"
        )
        .unwrap()
    );
    drop(db);
    let after = one_booking(&v, giro, -10_000);
    assert_eq!(after.id, whole.id);
    assert_eq!(split_lines(&v, whole.id).len(), 2);
}

#[test]
fn split_booking_goes_onto_a_bank_booking_already_there() {
    let v = vault();
    let acc = {
        let db = v.db().unwrap();
        let gid = Vault::ensure_group(&db, "Giro & Tagesgeld", "building.columns", 1).unwrap();
        db.execute(
            "INSERT INTO accounts(group_id, external_ref, name, iban, currency, kind, balance_cents)
             VALUES (?1, 'abc-123', 'Mein Giro', 'DE89370400440532013000', 'EUR', 'checking', 0)",
            [gid],
        )
        .unwrap();
        let id = db.last_insert_rowid();
        db.execute(
            "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, counterparty, purpose)
             VALUES (?1, 'bank-2', '2026-03-07', -10000, 'Kaufhaus Muster', 'Einkauf')",
            [id],
        )
        .unwrap();
        id
    };
    let path = fake_db();
    let import = || import_with(&v, &path, |o| o.accounts.retain(|c| c.mm_id == 1));
    import();
    let whole = one_booking(&v, acc, -10_000);
    assert_eq!(whole.external_id, "bank-2");
    assert_eq!(
        split_lines(&v, whole.id)
            .iter()
            .map(|l| (l.0, l.1.as_str()))
            .collect::<Vec<_>>(),
        vec![(-6000, "Supermarkt"), (-4000, "Strom")]
    );
    // splits the user removed are not brought back
    v.set_transaction_splits(whole.id, vec![]).unwrap();
    import();
    assert_eq!(one_booking(&v, acc, -10_000).id, whole.id);
    assert!(split_lines(&v, whole.id).is_empty());
}

#[test]
fn reimport_keeps_the_parts_of_an_earlier_import() {
    let v = vault();
    let path = fake_db();
    let giro = import_giro(&v, &path, true);
    // an account imported before #877: the split booking's parts as bookings
    let whole = one_booking(&v, giro, -10_000);
    {
        let db = v.db().unwrap();
        db.execute("DELETE FROM transactions WHERE id = ?1", [whole.id])
            .unwrap();
        for (ext, cents, purpose) in [
            ("mm-a-0", -6000, "Einkauf Lebensmittel"),
            ("mm-b-0", -4000, "Einkauf Lampe"),
        ] {
            db.execute(
                "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, counterparty, purpose)
                 VALUES (?1, ?2, '2026-03-07', ?3, 'Kaufhaus Muster', ?4)",
                params![giro, ext, cents, purpose],
            )
            .unwrap();
        }
    }
    let reimport = || import_with(&v, &path, |_| {});
    let r = reimport();
    assert_eq!(r.bookings_new, 0);
    assert!(!bookings(&v, giro).iter().any(|b| b.amount_cents == -10_000));
    let check = r
        .account_checks
        .iter()
        .find(|c| c.name == "Girokonto")
        .unwrap();
    assert_eq!(check.found_bookings, check.mm_bookings, "{check:?}");
    // also when the user deleted the parts
    v.db()
        .unwrap()
        .execute("UPDATE transactions SET deleted_ms = 1 WHERE external_id LIKE 'mm-%-0' AND account_id = ?1 AND amount_cents IN (-6000, -4000)", [giro])
        .unwrap();
    assert_eq!(reimport().bookings_new, 0);
    assert!(!bookings(&v, giro).iter().any(|b| b.amount_cents == -10_000));
}

#[test]
fn split_bookings_that_cannot_be_splits() {
    let extra = r#"
INSERT INTO transactions (rowid, local_account_key, account, bankcode, amount, currency, timestamp, value_timestamp, comment, booked, unformatted_type, unformatted_purpose, name, checkmark, splitted, split_key, category_key) VALUES
  (20, 1, '', '', -50.0, 'EUR', 1772884800, 0, '', 1, 'KARTENZAHLUNG', 'Rest offen', 'Markt A', 0, 1, 20, 1),
  (21, 1, '', '', -30.0, 'EUR', 1772884800, 0, '', 1, 'KARTENZAHLUNG', 'Rest offen', 'Markt A', 0, 0, 20, 2),
  (22, 1, '', '', -10.0, 'EUR', 1772884800, 0, '', 1, 'KARTENZAHLUNG', 'Rest offen', 'Markt A', 0, 0, 20, 4),
  (23, 1, '', '', -25.0, 'EUR', 1772884800, 0, '', 1, 'KARTENZAHLUNG', 'Ein Teil', 'Markt B', 0, 1, 23, 1),
  (24, 1, '', '', -25.0, 'EUR', 1772884800, 0, 'Teilnotiz', 1, 'KARTENZAHLUNG', 'Ein Teil', 'Markt B', 0, 0, 23, 2),
  (25, 1, '', '', 0.0, 'EUR', 1772884800, 0, '', 1, 'KARTENZAHLUNG', 'Ein Teil', 'Markt B', 0, 0, 23, 4);
"#;
    let path = fake_db_with(false, extra);
    let v = vault();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    assert_eq!(p.split_parts, 2, "the parts that do not add up");
    let giro = import_giro(&v, &path, true);
    // parts not adding up to the booking stay bookings of their own
    assert!(!bookings(&v, giro).iter().any(|b| b.amount_cents == -5000));
    assert!(split_lines(&v, one_booking(&v, giro, -3000).id).is_empty());
    one_booking(&v, giro, -1000);
    // a single part ≠ 0: the booking takes its category and note, no split
    let b = one_booking(&v, giro, -2500);
    assert!(split_lines(&v, b.id).is_empty());
    assert_eq!(b.category_path, "Haushalt › Supermarkt");
    assert_eq!(b.note, "Teilnotiz");
    // a second import adds nothing (not the part of 0 either)
    let n = bookings(&v, giro).len();
    assert_eq!(import_with(&v, &path, |_| {}).bookings_new, 0);
    assert_eq!(bookings(&v, giro).len(), n);
}

#[test]
fn reimport_keeps_parts_whose_texts_changed() {
    let v = vault();
    let path = fake_db();
    let giro = import_giro(&v, &path, true);
    // parts of an earlier import whose texts were cleaned differently back then
    let whole = one_booking(&v, giro, -10_000);
    {
        let db = v.db().unwrap();
        db.execute("DELETE FROM transactions WHERE id = ?1", [whole.id])
            .unwrap();
        for (ext, cents) in [("mm-a-0", -6000), ("mm-b-0", -4000)] {
            db.execute(
                "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, counterparty, purpose)
                 VALUES (?1, ?2, '2026-03-07', ?3, 'KAUFHAUS MUSTER GMBH', 'alt')",
                params![giro, ext, cents],
            )
            .unwrap();
        }
    }
    assert_eq!(import_with(&v, &path, |_| {}).bookings_new, 0);
    assert!(!bookings(&v, giro).iter().any(|b| b.amount_cents == -10_000));
}

#[test]
fn split_parts_inherit_currency_and_pending_rules() {
    let extra = r#"
INSERT INTO transactions (rowid, local_account_key, account, bankcode, amount, currency, timestamp, value_timestamp, comment, booked, unformatted_type, unformatted_purpose, name, checkmark, splitted, split_key, category_key) VALUES
  (30, 1, '', '', -80.0, 'EUR', 1772884800, 0, '', 1, 'KARTENZAHLUNG', 'Wocheneinkauf', 'Markt C', 0, 1, 30, 2),
  (31, 1, '', '', -50.0, 'EUR', 1772884800, 0, 'Notiz A', 1, 'KARTENZAHLUNG', 'Wocheneinkauf', 'Markt C', 0, 0, 30, 1),
  (32, 1, '', '', -30.0, 'EUR', 1772884800, 0, '', 1, 'KARTENZAHLUNG', 'Batterien', 'Markt C', 0, 0, 30, 4),
  (33, 1, '', '', -20.0, 'EUR', 1772884800, 0, '', 1, 'KARTENZAHLUNG', 'Devisen', 'Markt D', 0, 1, 33, 1),
  (34, 1, '', '', -12.0, 'USD', 1772884800, 0, '', 1, 'KARTENZAHLUNG', 'Devisen', 'Markt D', 0, 0, 33, 1),
  (35, 1, '', '', -8.0, 'EUR', 1772884800, 0, '', 1, 'KARTENZAHLUNG', 'Devisen', 'Markt D', 0, 0, 33, 1),
  (36, 1, '', '', -15.0, 'EUR', 1772884800, 0, '', 0, 'VORMERKUNG', 'Offen', 'Markt E', 0, 1, 36, 1),
  (37, 1, '', '', -9.0, 'EUR', 1772884800, 0, '', 1, 'KARTENZAHLUNG', 'Offen', 'Markt E', 0, 0, 36, 1),
  (38, 1, '', '', -6.0, 'EUR', 1772884800, 0, '', 1, 'KARTENZAHLUNG', 'Offen', 'Markt E', 0, 0, 36, 1);
"#;
    let path = fake_db_with(false, extra);
    let v = vault();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    assert_eq!(p.split_parts, 4, "another currency, pending parent");
    let giro = import_giro(&v, &path, true);
    // the uncategorised part takes the booking's category, notes and texts kept
    let c = one_booking(&v, giro, -8000);
    assert_eq!(
        split_lines(&v, c.id),
        vec![
            (-5000, "Supermarkt".into(), "Notiz A".into()),
            (-3000, "Strom".into(), "Batterien".into()),
        ]
    );
    // a part in another currency: parts as bookings
    assert!(!bookings(&v, giro).iter().any(|b| b.amount_cents == -2000));
    one_booking(&v, giro, -1200);
    one_booking(&v, giro, -800);
    // pending split booking: its booked parts as bookings
    one_booking(&v, giro, -900);
    one_booking(&v, giro, -600);

    // without notes the part's own note is left out, its differing purpose stays
    let v = vault();
    import_with(&v, &path, |o| o.import_notes = false);
    let giro = account_by_name(&v, "Girokonto").id;
    let c = one_booking(&v, giro, -8000);
    assert_eq!(
        split_lines(&v, c.id),
        vec![
            (-5000, "Supermarkt".into(), String::new()),
            (-3000, "Strom".into(), "Batterien".into()),
        ]
    );
}

#[test]
fn split_booking_without_categories_has_no_splits() {
    let v = vault();
    let giro = import_giro(&v, &fake_db(), false);
    let whole = one_booking(&v, giro, -10_000);
    assert!(split_lines(&v, whole.id).is_empty());
    assert!(!bookings(&v, giro).iter().any(|b| b.amount_cents == -6000));
    // the parts' texts are kept in the booking's note
    assert_eq!(whole.note, "Einkauf Lebensmittel · Einkauf Lampe");
}

#[test]
fn split_notes_are_counted_like_the_preview() {
    let extra = r#"
INSERT INTO transactions (rowid, local_account_key, account, bankcode, amount, currency, timestamp, value_timestamp, comment, booked, unformatted_type, unformatted_purpose, name, checkmark, splitted, split_key, category_key) VALUES
  (40, 1, '', '', -30.0, 'EUR', 1772884800, 0, '', 1, 'KARTENZAHLUNG', 'Markt', 'Markt D', 0, 1, 40, 1),
  (41, 1, '', '', -20.0, 'EUR', 1772884800, 0, 'für Oma', 1, 'KARTENZAHLUNG', 'Markt', 'Markt D', 0, 0, 40, 2),
  (42, 1, '', '', -10.0, 'EUR', 1772884800, 0, '', 1, 'KARTENZAHLUNG', 'Markt', 'Markt D', 0, 0, 40, 4);
"#;
    let path = fake_db_with(false, extra);
    let v = vault();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    let r = import_with(&v, &path, |_| {});
    assert_eq!(r.notes_imported, p.notes_count);
    let giro = account_by_name(&v, "Girokonto").id;
    let lines = split_lines(&v, one_booking(&v, giro, -3000).id);
    assert_eq!(lines[0].2, "für Oma");
}

#[test]
fn a_bank_booking_like_a_part_does_not_take_the_split_apart() {
    let v = vault();
    let acc = {
        let db = v.db().unwrap();
        let gid = Vault::ensure_group(&db, "Giro & Tagesgeld", "building.columns", 1).unwrap();
        db.execute(
            "INSERT INTO accounts(group_id, external_ref, name, iban, currency, kind, balance_cents)
             VALUES (?1, 'abc-123', 'Mein Giro', 'DE89370400440532013000', 'EUR', 'checking', 0)",
            [gid],
        )
        .unwrap();
        let id = db.last_insert_rowid();
        // fetched from the bank: another purchase, same day, name and amount as a part
        db.execute(
            "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, counterparty, purpose)
             VALUES (?1, 'bank-3', '2026-03-07', -6000, 'Kaufhaus Muster', 'Einkauf Lebensmittel')",
            [id],
        )
        .unwrap();
        id
    };
    import_with(&v, &fake_db(), |o| o.accounts.retain(|c| c.mm_id == 1));
    let whole = one_booking(&v, acc, -10_000);
    assert_eq!(split_lines(&v, whole.id).len(), 2);
    assert_eq!(one_booking(&v, acc, -6000).external_id, "bank-3");
}

#[test]
fn iban_match_merges_into_existing_synced_account() {
    let v = vault();
    let conn_id = v
        .add_bank_connection(
            ConnectorKind::Comdirect,
            "comdirect".into(),
            "1234567".into(),
        )
        .unwrap();
    let existing = {
        let db = v.db().unwrap();
        let gid = Vault::ensure_group(&db, "Giro & Tagesgeld", "building.columns", 1).unwrap();
        db.execute(
            "INSERT INTO accounts(connection_id, group_id, external_ref, name, iban, currency, kind, balance_cents)
             VALUES (?1, ?2, 'abc-123', 'Mein Giro', 'DE89 3704 0044 0532 0130 00', 'EUR', 'checking', 123456)",
            params![conn_id, gid],
        )
        .unwrap();
        let id = db.last_insert_rowid();
        // already fetched by the bank: the REWE booking without category
        db.execute(
            "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, counterparty, counterparty_iban, purpose)
             VALUES (?1, 'bank-1', '2026-03-05', -4567, 'REWE Markt', 'DE02100100100006820101', 'Einkauf Filiale 12')",
            [id],
        )
        .unwrap();
        id
    };
    let path = fake_db();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    let giro = p.accounts.iter().find(|a| a.mm_id == 1).unwrap();
    assert_eq!(giro.existing_account_id, Some(existing));
    assert_eq!(giro.existing_account_name.as_deref(), Some("Mein Giro"));
    assert_eq!(p.banks[0].existing_connection_id, Some(conn_id));
    let mut o = options_from(&p, true);
    o.accounts.retain(|c| c.mm_id == 1);
    let r = v.import_moneymoney_database(path, PW.into(), o).unwrap();
    assert_eq!((r.accounts_created, r.accounts_merged), (0, 1));
    assert_eq!(
        (r.bookings_new, r.bookings_duplicates, r.bookings_enriched),
        (3, 1, 1)
    );
    assert_eq!((r.connections_created, r.connections_existing), (1, 1));
    let acc = v
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|a| a.id == existing)
        .unwrap();
    // the bank's balance stays authoritative for synced accounts
    assert_eq!(acc.balance_cents, 123_456);
    assert_eq!(acc.name, "Mein Giro");
    let bs = bookings(&v, existing);
    assert_eq!(bs.len(), 4);
    let rewe = bs.iter().find(|b| b.counterparty == "REWE Markt").unwrap();
    assert_eq!(rewe.category_path, "Haushalt › Supermarkt");
    assert_eq!(v.list_accounts().unwrap().len(), 1);
}

#[test]
fn first_bank_sync_adopts_imported_account_by_iban() {
    let v = vault();
    let path = fake_db();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    v.import_moneymoney_database(path, PW.into(), options_from(&p, true))
        .unwrap();
    let giro = account_by_name(&v, "Girokonto");
    let conn_id = v.list_bank_connections().unwrap()[0].id;
    let db = v.db().unwrap();
    assert_eq!(
        adopt_account(&db, conn_id, "remote-1", "", "", AccountKind::Checking).unwrap(),
        None
    );
    let adopted = adopt_account(
        &db,
        conn_id,
        "remote-1",
        "DE89 3704 0044 0532 0130 00",
        "",
        AccountKind::Checking,
    )
    .unwrap();
    assert_eq!(adopted, Some(giro.id));
    let (c, e): (Option<i64>, String) = db
        .query_row(
            "SELECT connection_id, external_ref FROM accounts WHERE id = ?1",
            [giro.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((c, e.as_str()), (Some(conn_id), "remote-1"));
    // only once
    assert_eq!(
        adopt_account(
            &db,
            conn_id,
            "remote-2",
            "DE89370400440532013000",
            "",
            AccountKind::Checking
        )
        .unwrap(),
        None
    );
}

#[test]
fn adopting_wallets_sharing_an_iban_follows_the_currency() {
    let v = vault();
    let path = fake_db();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    v.import_moneymoney_database(path, PW.into(), options_from(&p, true))
        .unwrap();
    let conn_id = v.list_bank_connections().unwrap()[0].id;
    let db = v.db().unwrap();
    // two imported wallets under one IBAN, the USD one imported first
    let mut ids = Vec::new();
    for (n, cur) in [("1", "USD"), ("2", "EUR")] {
        db.execute(
            "INSERT INTO accounts(group_id, external_ref, name, iban, currency)
             VALUES ((SELECT id FROM account_groups LIMIT 1), ?1, 'Wallet', 'GB51TCCL00000000000001', ?2)",
            params![format!("moneymoney-db:w{n}"), cur],
        )
        .unwrap();
        ids.push(db.last_insert_rowid());
    }
    let (usd, eur) = (ids[0], ids[1]);
    let iban = "GB51 TCCL 0000 0000 0000 01";
    assert_eq!(
        adopt_account(&db, conn_id, "w:EUR", iban, "EUR", AccountKind::Checking).unwrap(),
        Some(eur)
    );
    assert_eq!(
        adopt_account(&db, conn_id, "w:USD", iban, "USD", AccountKind::Checking).unwrap(),
        Some(usd)
    );
    // a third currency finds no wallet to take over
    assert_eq!(
        adopt_account(&db, conn_id, "w:GBP", iban, "GBP", AccountKind::Checking).unwrap(),
        None
    );
}

/// An imported account parked on a PSD2 connection that never synced is
/// taken over by a direct connector with the same IBAN; once the PSD2
/// connection synced, its accounts stay where they are.
#[test]
fn a_direct_connector_takes_over_accounts_of_an_unsynced_psd2_connection() {
    let v = vault();
    let psd2 = v
        .add_bank_connection(
            ConnectorKind::EnableBanking,
            "enablebanking:DE:Test Bank".into(),
            String::new(),
        )
        .unwrap();
    let direct = v
        .add_bank_connection(ConnectorKind::FordMoney, String::new(), "1000000099".into())
        .unwrap();
    let db = v.db().unwrap();
    db.execute(
        "INSERT OR IGNORE INTO account_groups(name) VALUES ('Test')",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO accounts(connection_id, group_id, external_ref, name, iban, currency)
         VALUES (?1, (SELECT id FROM account_groups LIMIT 1), 'moneymoney-db:hb1', 'Test Card',
                 'DE00 0000 0000 1000 0000 01', 'EUR')",
        params![psd2],
    )
    .unwrap();
    let imported = db.last_insert_rowid();
    db.execute(
        "INSERT INTO accounts(connection_id, group_id, external_ref, name, iban, currency)
         VALUES (?1, (SELECT id FROM account_groups LIMIT 1), 'moneymoney-db:hb2', 'Test Savings',
                 'DE00000000001000000002', 'EUR')",
        params![psd2],
    )
    .unwrap();
    let second = db.last_insert_rowid();

    assert_eq!(
        adopt_account(
            &db,
            direct,
            "fordmoney:1",
            "DE00000000001000000001",
            "EUR",
            AccountKind::CreditCard
        )
        .unwrap(),
        Some(imported)
    );
    let (cid, ext): (i64, String) = db
        .query_row(
            "SELECT connection_id, external_ref FROM accounts WHERE id = ?1",
            [imported],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((cid, ext.as_str()), (direct, "fordmoney:1"));

    // After the PSD2 connection synced, its accounts are its own.
    db.execute(
        "UPDATE bank_connections SET last_sync_unix = 1 WHERE id = ?1",
        [psd2],
    )
    .unwrap();
    assert_eq!(
        adopt_account(
            &db,
            direct,
            "fordmoney:2",
            "DE00000000001000000002",
            "EUR",
            AccountKind::Savings
        )
        .unwrap(),
        None
    );
    let still: i64 = db
        .query_row(
            "SELECT connection_id FROM accounts WHERE id = ?1",
            [second],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(still, psd2);
}

/// comdirect: its own API stays the default, FinTS (bank code of the login)
/// is offered next to it and, when chosen, becomes the login's connection.
#[test]
fn comdirect_can_be_connected_via_fints() {
    let v = vault();
    let path = fake_db();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    let bank = p.banks.iter().find(|b| b.mm_id == 1).unwrap();
    assert_eq!(bank.connector, Some(ConnectorKind::Comdirect));
    let vias: Vec<_> = bank
        .connection_options
        .iter()
        .map(|o| (o.via, o.connector, o.key.as_str()))
        .collect();
    assert_eq!(
        vias,
        vec![
            (
                MoneyMoneyDbConnectionVia::OwnApi,
                ConnectorKind::Comdirect,
                "comdirect"
            ),
            (
                MoneyMoneyDbConnectionVia::FinTs,
                ConnectorKind::FinTsGeneric,
                "fints:20041111"
            ),
        ]
    );
    // no other bank gains an option
    let vb = p.banks.iter().find(|b| b.mm_id == 2).unwrap();
    assert_eq!(vb.connection_options.len(), 1);

    let mut o = options_from(&p, true);
    o.bank_choices = vec![MoneyMoneyDbBankChoice {
        mm_bank_id: 1,
        connection: Some(bank.connection_options[1].clone()),
    }];
    v.import_moneymoney_database(path, PW.into(), o).unwrap();
    let conns = v.list_bank_connections().unwrap();
    assert!(
        conns
            .iter()
            .all(|c| c.connector != ConnectorKind::Comdirect)
    );
    let fints = conns
        .iter()
        .find(|c| c.connector == ConnectorKind::FinTsGeneric)
        .expect("FinTS connection for comdirect");
    assert_eq!(fints.login_name, "1234567");
    assert_eq!(
        v.get_setting("fints.url.20041111".into())
            .unwrap()
            .as_deref(),
        Some(crate::connectors::fints_directory::COMDIRECT_FINTS_URL)
    );
    // the imported accounts hang on the FinTS login; the first sync adopts them
    let giro = v
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|a| a.name == "Girokonto")
        .unwrap();
    assert_eq!(giro.connection_id, Some(fints.id));
}

/// #1377: the FinTS endpoint URL MoneyMoney stores in `banks.server` is
/// taken over to `fints.url.<blz>`. Without this the connection settings
/// show an empty URL field and the user has to copy it from the DK list by
/// hand. The server URL wins over GiroCrab's built-in directory entry, so a
/// bank that moved to a new host keeps the MoneyMoney address.
#[test]
fn fints_server_url_is_imported_from_moneymoney() {
    // Sparkasse Rosenheim-Bad/Aibling (BLZ 71150000) is in the FinTS directory,
    // so it maps to `ConnectorKind::FinTsGeneric`. MoneyMoney stores its own
    // URL which may differ from the bundled directory (bank-side migrations).
    const MM_URL: &str = "https://banking-by2.s-fints-pt-by.de/fints30";
    let extra = format!(
        "INSERT INTO banks (rowid, country, bankcode, username, customer, credential, name, server, systemid, local_storage) \
         VALUES (4, 'DE', '71150000', 'max.m', 'max.m', 'GEHEIM-4', 'Sparkasse Rosenheim', '{MM_URL}', 'SYS', 'LS');"
    );
    let path = fake_db_with(false, &extra);
    let v = vault();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    let bank = p.banks.iter().find(|b| b.mm_id == 4).unwrap();
    assert_eq!(bank.connector, Some(ConnectorKind::FinTsGeneric));
    v.import_moneymoney_database(path, PW.into(), options_from(&p, true))
        .unwrap();
    assert_eq!(
        v.get_setting("fints.url.71150000".into())
            .unwrap()
            .as_deref(),
        Some(MM_URL),
        "MoneyMoney's banks.server URL must be stored as fints.url.<blz>"
    );
}

/// A bogus or http URL in `banks.server` must not be written through. The
/// connector then falls back to the built-in FinTS directory at sync time.
#[test]
fn fints_server_url_is_rejected_when_not_https() {
    let extra = "INSERT INTO banks (rowid, country, bankcode, username, customer, credential, name, server, systemid, local_storage) \
         VALUES (4, 'DE', '71150000', 'max.m', 'max.m', 'GEHEIM-4', 'Sparkasse Rosenheim', 'http://evil.example/fints', 'SYS', 'LS');";
    let path = fake_db_with(false, extra);
    let v = vault();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    v.import_moneymoney_database(path, PW.into(), options_from(&p, true))
        .unwrap();
    assert_eq!(
        v.get_setting("fints.url.71150000".into()).unwrap(),
        None,
        "an http URL must not become a FinTS endpoint setting"
    );
}

#[test]
fn connections_for_supported_banks_only_and_never_a_pin() {
    let v = vault();
    let path = fake_db();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    let r = v
        .import_moneymoney_database(path.clone(), PW.into(), options_from(&p, true))
        .unwrap();
    assert_eq!((r.connections_created, r.banks_skipped_unsupported), (2, 1));
    let conns = v.list_bank_connections().unwrap();
    assert_eq!(conns.len(), 2);
    let comdirect = conns
        .iter()
        .find(|c| c.connector == ConnectorKind::Comdirect)
        .unwrap();
    assert_eq!(comdirect.login_name, "1234567");
    let vb = conns
        .iter()
        .find(|c| c.connector == ConnectorKind::VolksbankKasselGoettingen)
        .unwrap();
    assert_eq!(vb.login_name, "VRNK4711");
    for c in &conns {
        assert!(!v.connection_status(c.id).unwrap().has_pin);
    }
    // a second run changes nothing and still stores no PIN
    let r = v
        .import_moneymoney_database(path, PW.into(), options_from(&p, true))
        .unwrap();
    assert_eq!(r.connections_created, 0);
    let db = v.db().unwrap();
    let n: i64 = db
        .query_row(
            "SELECT count(*) FROM settings WHERE key LIKE 'pin.conn.%' OR value IN ('11111', '22222', '33333')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 0);
}

/// Replaces the secret columns of `banks`, `accounts` and `pins.pin` by views
/// whose secret columns raise an error when evaluated. The importer must still
/// work – proving it never reads them (nor the `pins` table).
const TRAP: &str = r#"
ALTER TABLE banks RENAME TO banks_data;
CREATE VIEW banks AS SELECT rowid, country, bankcode, username, customer, json('x') AS credential, name, server,
  protocol, tanmethod, tanmedium, json('x') AS systemid, tanmethods, json('x') AS bpd, json('x') AS upd, spa, spa_tsp,
  json('x') AS local_storage, json('x') AS pubkey, json('x') AS privkey, json('x') AS userdata FROM banks_data;
ALTER TABLE accounts RENAME TO accounts_data;
CREATE VIEW accounts AS SELECT rowid, bank_key, account, subaccount, bankcode, portfolio, iban, bic, currency, name,
  owner, kind, type, sort, icon, balances, pending_balances, overdraft_limit, available_funds, initial_balance, cub,
  cub_tsp, recipient_key, refresh_tsp, balance_tsp, transaction_tsp, payment_tsp, error_tsp, cancel_tsp, statements,
  refresh_actions, refresh, json('x') AS userdata, json('x') AS cred, json('x') AS transientdata, perspective,
  group_key, error, json('x') AS web_ops FROM accounts_data;
ALTER TABLE pins RENAME TO pins_data;
CREATE VIEW pins AS SELECT bank_key, json('x') AS pin FROM pins_data;
"#;

#[test]
fn secret_columns_and_pins_are_never_read() {
    let path = fake_db_with(false, TRAP);
    // positive control: evaluating a trapped column fails
    let c = open_source(&path, &SecretString::new(PW.into())).unwrap();
    assert!(
        c.query_row("SELECT pin FROM pins LIMIT 1", [], |r| r
            .get::<_, String>(0))
            .is_err()
    );
    assert!(
        c.query_row("SELECT credential FROM banks LIMIT 1", [], |r| r
            .get::<_, String>(0))
            .is_err()
    );
    drop(c);
    let v = vault();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    // the pin column and the pins table stay untouched, with or without connections
    v.import_moneymoney_database(path.clone(), PW.into(), options_from(&p, true))
        .unwrap();
    v.import_moneymoney_database(path, PW.into(), options_from(&p, false))
        .unwrap();
}

#[test]
fn kind_override_on_reimport() {
    let v = vault();
    let path = fake_db();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    let mut o = options_from(&p, false);
    for c in &mut o.accounts {
        c.import = c.mm_id == 2;
        c.kind = AccountKind::Checking;
    }
    let r = v
        .import_moneymoney_database(path.clone(), PW.into(), o.clone())
        .unwrap();
    assert_eq!((r.accounts_created, r.bookings_new), (1, 1));
    assert_eq!(
        account_by_name(&v, "Visa-Karte").kind,
        AccountKind::Checking
    );
    // changing the kind on a later run re-files the importer's own account
    o.accounts
        .iter_mut()
        .for_each(|c| c.kind = AccountKind::CreditCard);
    v.import_moneymoney_database(path, PW.into(), o).unwrap();
    assert_eq!(
        account_by_name(&v, "Visa-Karte").kind,
        AccountKind::CreditCard
    );
    assert_eq!(v.list_accounts().unwrap().len(), 1);
}

#[test]
fn imported_accounts_are_linked_to_their_bank_login() {
    let v = vault();
    let path = fake_db();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    v.import_moneymoney_database(path, PW.into(), options_from(&p, true))
        .unwrap();
    let comdirect = v.list_bank_connections().unwrap()[0].id;
    for name in ["Girokonto", "Visa-Karte", "Depot"] {
        assert_eq!(
            account_by_name(&v, name).connection_id,
            Some(comdirect),
            "{name}"
        );
    }
    // unsupported bank: no login, account stays on its own
    assert_eq!(account_by_name(&v, "Dollar-Konto").connection_id, None);
}

#[test]
fn opening_balance_applies_to_accounts_linked_to_a_bank_login() {
    // #212: the import shows the accounts under their bank login right away;
    // until a sync takes one over, its balance is still computed here.
    let v = vault();
    let path = fake_db();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    v.import_moneymoney_database(path, PW.into(), options_from(&p, true))
        .unwrap();
    let giro = account_by_name(&v, "Girokonto");
    assert!(giro.connection_id.is_some());
    assert!(giro.balance_derived);
    assert_eq!(giro.opening_balance_cents, Some(10_000 - 250_000));
    v.calibrate_account_opening_balance(giro.id, 0).unwrap();
    assert_eq!(account_by_name(&v, "Girokonto").balance_cents, 0);
    // taken over by a sync: the bank's balance counts, not the Startsaldo
    v.db()
        .unwrap()
        .execute(
            "UPDATE accounts SET external_ref = 'bank:1' WHERE id = ?1",
            [giro.id],
        )
        .unwrap();
    assert!(!account_by_name(&v, "Girokonto").balance_derived);
}

#[test]
fn accounts_stay_unlinked_without_bank_logins() {
    let v = vault();
    let path = fake_db();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    v.import_moneymoney_database(path, PW.into(), options_from(&p, false))
        .unwrap();
    assert!(
        v.list_accounts()
            .unwrap()
            .iter()
            .all(|a| a.connection_id.is_none())
    );
}

#[test]
fn first_bank_sync_adopts_imported_card_without_iban_by_kind() {
    let v = vault();
    let path = fake_db();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    v.import_moneymoney_database(path, PW.into(), options_from(&p, true))
        .unwrap();
    let visa = account_by_name(&v, "Visa-Karte");
    let conn_id = v.list_bank_connections().unwrap()[0].id;
    let db = v.db().unwrap();
    // a fetched account with an IBAN never takes over an account by kind
    assert_eq!(
        adopt_account(
            &db,
            conn_id,
            "card-x",
            "DE02120300000000202051",
            "",
            AccountKind::CreditCard
        )
        .unwrap(),
        None
    );
    assert_eq!(
        adopt_account(&db, conn_id, "card-1", "", "", AccountKind::CreditCard).unwrap(),
        Some(visa.id)
    );
    // only once, and never for another login
    assert_eq!(
        adopt_account(&db, conn_id, "card-2", "", "", AccountKind::CreditCard).unwrap(),
        None
    );
    assert_eq!(
        adopt_account(&db, conn_id + 1, "dep-1", "", "", AccountKind::Brokerage).unwrap(),
        None
    );
}

#[test]
fn amazon_visa_filed_as_openbank_gets_its_connector_and_is_adopted() {
    // MoneyMoney files the Amazon Visa as "Openbank" (bank code 50034200) and
    // stores an IBAN for the card; an Open Bank current account stays unsupported.
    let extra = "
INSERT INTO banks (rowid, country, bankcode, username, customer, credential, name, systemid, local_storage) VALUES
  (20, 'DE', '50034200', 'card-login', '', 'GEHEIM-20', 'Openbank', 'SYS', 'LS'),
  (21, 'DE', '50034200', 'giro-login', '', 'GEHEIM-21', 'Openbank', 'SYS', 'LS');
INSERT INTO accounts (rowid, bank_key, account, bankcode, portfolio, iban, currency, name, owner, kind, type, sort, initial_balance, group_key, cred) VALUES
  (20, 20, '', '50034200', 0, 'DE12 5003 4200 0000 0000 01', 'EUR', 'Amazon Visa', 'Max Mustermann', 0, 3, 20, 0, 1, 'GEHEIM'),
  (21, 21, '', '50034200', 0, 'DE12 5003 4200 0000 0000 02', 'EUR', 'Girokonto', 'Max Mustermann', 0, 0, 21, 0, 1, 'GEHEIM');
";
    let v = vault();
    let path = fake_db_with(false, extra);
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    let bank = |id| p.banks.iter().find(|b| b.mm_id == id).unwrap().connector;
    assert_eq!(bank(20), Some(ConnectorKind::AmazonVisa));
    assert_eq!(bank(21), None);
    v.import_moneymoney_database(path, PW.into(), options_from(&p, true))
        .unwrap();
    let conn = v
        .list_bank_connections()
        .unwrap()
        .into_iter()
        .find(|c| c.connector == ConnectorKind::AmazonVisa)
        .unwrap();
    assert_eq!(conn.login_name, "card-login");
    let card = account_by_name(&v, "Amazon Visa");
    assert_eq!(card.connection_id, Some(conn.id));
    // the web login reports the card without an IBAN: still taken over, once
    let db = v.db().unwrap();
    assert_eq!(
        adopt_account(&db, conn.id, "amazon-1", "", "", AccountKind::CreditCard).unwrap(),
        Some(card.id)
    );
    db.execute(
        "UPDATE accounts SET external_ref = 'amazon-1' WHERE id = ?1",
        [card.id],
    )
    .unwrap();
    assert_eq!(
        adopt_account(&db, conn.id, "amazon-2", "", "", AccountKind::CreditCard).unwrap(),
        None
    );
}

#[test]
fn starter_categories_are_replaced_in_an_empty_vault() {
    let v = vault();
    assert!(v.seed_starter_categories().unwrap() > 0);
    assert!(!v.list_rules().unwrap().is_empty());
    let path = fake_db();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    assert!(p.replaces_starter_categories);
    v.import_moneymoney_database(path, PW.into(), options_from(&p, false))
        .unwrap();
    let mut names: Vec<String> = v
        .list_categories(None, None)
        .unwrap()
        .into_iter()
        .map(|c| c.name)
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "Gehalt",
            "Haushalt",
            "Nebenkosten",
            "Steuern / Büro",
            "Strom",
            "Supermarkt",
            "Wohnen"
        ]
    );
    assert!(v.list_rules().unwrap().is_empty());
}

#[test]
fn category_colors_are_taken_over_without_overwriting_own_ones() {
    let v = vault();
    let path = fake_db_with(
        false,
        "UPDATE category_groups SET color = '#ff9500' WHERE rowid = 1;  -- Haushalt
         UPDATE category_groups SET color = '#0a0' WHERE rowid = 2;     -- Wohnen (#rgb)
         UPDATE categories SET color = '#34C759ff' WHERE rowid = 2;     -- Supermarkt (#rrggbbaa)
         UPDATE categories SET color = '''#ff3b30''' WHERE rowid = 4;   -- Strom (quoted)
         UPDATE categories SET color = 'blau' WHERE rowid = 3;          -- Gehalt (invalid)
         INSERT INTO category_groups (rowid, name, group_key, sort, color)
           VALUES (9, 'Leer', 0, 9, '#123456');                         -- no categories",
    );
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    v.import_moneymoney_database(path.clone(), PW.into(), options_from(&p, false))
        .unwrap();
    let color = |name: &str| {
        v.list_categories(None, None)
            .unwrap()
            .into_iter()
            .find(|c| c.name == name)
            .map(|c| c.color_hex)
    };
    assert_eq!(color("Haushalt").as_deref(), Some("#FF9500"));
    assert_eq!(color("Wohnen").as_deref(), Some("#00AA00"));
    assert_eq!(color("Supermarkt").as_deref(), Some("#34C759"));
    assert_eq!(color("Strom").as_deref(), Some("#FF3B30"));
    {
        // taken-over colours are stamped for the sync merge, grey ones not touched
        let db = v.db().unwrap();
        let ms = |name: &str| -> i64 {
            db.query_row(
                "SELECT color_ms FROM categories WHERE name = ?1 AND deleted_ms IS NULL",
                [name],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert!(
            ms("Strom") >= ms("Gehalt"),
            "colour stamp not older than creation"
        );
        assert!(ms("Strom") > 0);
    }
    assert_eq!(color("Gehalt").as_deref(), Some(DEFAULT_CATEGORY_COLOR));
    assert_eq!(
        color("Nebenkosten").as_deref(),
        Some(DEFAULT_CATEGORY_COLOR)
    );
    assert_eq!(color("Leer"), None, "empty groups are not created");

    // a colour chosen in GiroCrab survives a second import
    let strom = v
        .list_categories(None, None)
        .unwrap()
        .into_iter()
        .find(|c| c.name == "Strom")
        .unwrap();
    v.update_category(
        strom.id,
        strom.name,
        strom.parent_id,
        strom.sf_symbol,
        "#5856D6".into(),
    )
    .unwrap();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    v.import_moneymoney_database(path, PW.into(), options_from(&p, false))
        .unwrap();
    assert_eq!(color("Strom").as_deref(), Some("#5856D6"));
    assert_eq!(color("Haushalt").as_deref(), Some("#FF9500"));
}

#[test]
fn mm_color_formats() {
    assert_eq!(mm_color("#ff9500").as_deref(), Some("#FF9500"));
    assert_eq!(mm_color(" '#FF9500' ").as_deref(), Some("#FF9500"));
    assert_eq!(mm_color("#abc").as_deref(), Some("#AABBCC"));
    assert_eq!(mm_color("#11223344").as_deref(), Some("#112233"));
    for bad in ["", "ff9500", "#ff95", "#gg9500", "rot", "#"] {
        assert_eq!(mm_color(bad), None, "{bad:?}");
    }
}

#[test]
fn starter_categories_stay_when_the_vault_has_bookings() {
    let v = vault();
    let starter = v.seed_starter_categories().unwrap() as usize;
    let path = fake_db();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    v.import_moneymoney_database(path.clone(), PW.into(), options_from(&p, false))
        .unwrap();
    // second run: the vault now has bookings, nothing is replaced
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    assert!(!p.replaces_starter_categories);
    let before = v.list_categories(None, None).unwrap().len();
    v.import_moneymoney_database(path, PW.into(), options_from(&p, false))
        .unwrap();
    assert_eq!(v.list_categories(None, None).unwrap().len(), before);

    let v = vault();
    v.seed_starter_categories().unwrap();
    let path = fake_db();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    // categories not wanted: the starter set stays
    let mut o = options_from(&p, false);
    o.import_categories = false;
    v.import_moneymoney_database(path, PW.into(), o).unwrap();
    assert_eq!(v.list_categories(None, None).unwrap().len(), starter);
}

/// Soft-deleted bookings are no bookings: the vault counts as empty.
#[test]
fn deleted_bookings_do_not_count_as_existing() {
    let v = vault();
    v.seed_starter_categories().unwrap();
    {
        let db = v.db().unwrap();
        db.execute_batch(
            "INSERT INTO account_groups(id, name) VALUES (1, 'G');
             INSERT INTO accounts(id, group_id, external_ref, name) VALUES (1, 1, 'x', 'X');
             INSERT INTO transactions(account_id, external_id, booking_date, amount_cents)
             VALUES (1, 'e1', '2026-01-01', -5);",
        )
        .unwrap();
    }
    v.set_account_archived(1, true).unwrap();
    v.delete_account(1).unwrap();
    let p = v
        .preview_moneymoney_database(fake_db(), PW.into(), None)
        .unwrap();
    assert!(p.replaces_starter_categories);
}

#[test]
fn directory_banks_map_to_fints_keys() {
    // (bank code, name) -> stored connector key
    for (code, name, key) in [
        ("50010517", "ING", "fints:50010517"),
        ("43060967", "GLS Gemeinschaftsbank", "fints:43060967"),
        ("12030000", "DKB", "fints:12030000"),
        ("76030080", "Consorsbank", "fints:76030080"),
        ("10077777", "Norisbank", "fints:10077777"),
        ("66090800", "BBBank", "fints:66090800"),
        ("30060601", "apoBank", "fints:30060601"),
        ("83094495", "EthikBank", "fints:83094495"),
        ("10050000", "Berliner Sparkasse", "fints:10050000"),
        ("10070000", "Deutsche Bank", "fints:10070000"),
        ("70020270", "HypoVereinsbank", "fints:70020270"),
        ("1005 0000", "Berliner Sparkasse", "fints:10050000"),
    ] {
        let (kind, k) = connector_key_for(code, name).unwrap_or_else(|| panic!("{code}"));
        assert_eq!(kind, ConnectorKind::FinTsGeneric, "{code}");
        assert_eq!(k, key);
    }
    // dedicated connectors keep precedence over the directory
    assert_eq!(
        connector_key_for("20041111", "comdirect").unwrap().0,
        ConnectorKind::Comdirect
    );
    assert_eq!(
        connector_key_for("52090000", "Volksbank Kassel Göttingen")
            .unwrap()
            .1,
        "fints:52090000"
    );
    // unknown German bank code stays unsupported
    assert!(connector_key_for("12345678", "Sparkasse Musterstadt").is_none());
}

#[test]
fn generic_enable_banking_connection_is_stored_once() {
    let v = vault();
    let a = v
        .add_enable_banking_connection("N26".into(), "de".into(), "".into(), "".into())
        .unwrap();
    let b = v
        .add_enable_banking_connection("N26".into(), "DE".into(), "N26".into(), "".into())
        .unwrap();
    assert_eq!(a, b);
    assert!(
        v.add_enable_banking_connection("".into(), "DE".into(), "".into(), "".into())
            .is_err()
    );
    assert!(
        v.add_enable_banking_connection("N26".into(), "DEU".into(), "".into(), "".into())
            .is_err()
    );
    let c = v.list_bank_connections().unwrap();
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].connector, ConnectorKind::EnableBanking);
    assert_eq!(c[0].display_name, "N26");
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect()
}

fn pdf(text: &str) -> Vec<u8> {
    format!("%PDF-1.4\n{text}\n%%EOF").into_bytes()
}

/// A MoneyMoney database listing four documents (and a deleted one) the way
/// MoneyMoney does: bank folder, file name, date, upper-case SHA-256.
fn statements_db() -> String {
    let (a, b, c, d) = (pdf("A"), pdf("B"), pdf("C"), pdf("D"));
    fake_db_with(
        false,
        &format!(
            "INSERT INTO statements (rowid, account_key, name, directory, filename, created_tsp, deleted, digest) VALUES
               (1, 1, 'Girokonto', 'Musterbank', 'Kontoauszug_2026_001.pdf', 1767225600, 0, '{}'),
               (2, 1, 'Girokonto', 'Musterbank', 'Steuerbescheinigung_2025.pdf', 1767312000, 0, '{}'),
               (3, 1, 'Girokonto', 'Musterbank', 'Kreditkartenabrechnung_2026_01.pdf', 1767398400, 0, '{}'),
               (4, 1, 'Girokonto', 'Musterbank', 'Geloescht.pdf', 1767398400, 1, ''),
               (5, 2, 'Visa-Karte', 'Musterbank', 'Vertragsinformationen.pdf', 1767484800, 0, '{}');",
            sha256(&a),
            sha256(&b),
            sha256(&c),
            sha256(&d)
        ),
    )
}

fn docs(v: &Vault) -> Vec<(String, String, String, String)> {
    let mut out: Vec<_> = v
        .list_all_documents(crate::documents::DocumentFilter::default())
        .unwrap()
        .into_iter()
        .map(|d| (d.file_name, d.doc_type, d.doc_date, d.title))
        .collect();
    out.sort();
    out
}

#[test]
fn statements_are_imported_from_the_moneymoney_folder() {
    // <folder>/Database/MoneyMoney.sqlite + <folder>/Statements/<bank>/<file>
    let folder = temp("mm-folder");
    let bank = folder.join("Statements").join("Musterbank");
    std::fs::create_dir_all(&bank).unwrap();
    std::fs::create_dir_all(folder.join("Database")).unwrap();
    std::fs::rename(
        statements_db(),
        folder.join("Database").join("MoneyMoney.sqlite"),
    )
    .unwrap();
    std::fs::write(bank.join("Kontoauszug_2026_001.pdf"), pdf("A")).unwrap();
    // B is missing, C has other content than the database says, D is fine
    std::fs::write(bank.join("Kreditkartenabrechnung_2026_01.pdf"), pdf("X")).unwrap();
    std::fs::write(bank.join("Vertragsinformationen.pdf"), pdf("D")).unwrap();
    std::fs::write(bank.join("Fremd.pdf"), pdf("F")).unwrap();
    let path = folder.to_string_lossy().to_string();

    let v = vault();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    assert!(p.database_path.ends_with("MoneyMoney.sqlite"));
    assert!(p.statements_dir.as_deref().unwrap().ends_with("Statements"));
    assert_eq!((p.statement_count, p.statements_found), (4, 3));
    assert_eq!(p.statements_bytes, 3 * pdf("A").len() as u64);
    let giro = p.accounts.iter().find(|a| a.name == "Girokonto").unwrap();
    assert_eq!(giro.statement_count, 3, "the deleted one is not counted");

    let r = v
        .import_moneymoney_database(path.clone(), PW.into(), options_from(&p, false))
        .unwrap();
    assert_eq!(
        (
            r.documents_imported,
            r.documents_missing,
            r.documents_mismatched,
            r.documents_unlisted
        ),
        (2, 1, 1, 1)
    );
    assert_eq!(
        docs(&v),
        vec![
            (
                "Kontoauszug_2026_001.pdf".to_string(),
                "kontoauszug".to_string(),
                "2026-01-01".to_string(),
                "Kontoauszug 2026 001".to_string()
            ),
            (
                "Vertragsinformationen.pdf".to_string(),
                "postbox".to_string(),
                "2026-01-04".to_string(),
                "Vertragsinformationen".to_string()
            ),
        ]
    );
    // linked to their accounts
    let giro_id = account_by_name(&v, "Girokonto").id;
    let of_giro = v
        .list_all_documents(crate::documents::DocumentFilter {
            account_id: Some(giro_id),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(of_giro.len(), 1);

    // again: nothing twice; and without the option nothing at all
    let r = v
        .import_moneymoney_database(path.clone(), PW.into(), options_from(&p, false))
        .unwrap();
    assert_eq!((r.documents_imported, r.documents_existing), (0, 2));
    let mut off = options_from(&p, false);
    off.import_statements = false;
    let r = v.import_moneymoney_database(path, PW.into(), off).unwrap();
    assert_eq!(
        (
            r.documents_imported,
            r.documents_existing,
            r.documents_missing
        ),
        (0, 0, 0)
    );
    assert_eq!(docs(&v).len(), 2);
    let _ = std::fs::remove_dir_all(folder);
}

#[test]
fn statements_of_a_backup_come_from_a_separately_saved_folder() {
    // a single database file somewhere: documents are listed, but no files
    let path = statements_db();
    let v = vault();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    assert_eq!(
        (
            p.statement_count,
            p.statements_found,
            p.statements_dir.clone()
        ),
        (4, 0, None)
    );
    let r = v
        .import_moneymoney_database(path.clone(), PW.into(), options_from(&p, false))
        .unwrap();
    assert_eq!((r.documents_imported, r.documents_missing), (0, 4));

    // later, with a manual backup of the PDFs: A renamed in a subfolder (found
    // by checksum), B by its name without the bank folder, C and D not saved
    let saved = temp("mm-saved");
    std::fs::create_dir_all(saved.join("alt")).unwrap();
    std::fs::write(saved.join("alt").join("scan 1.PDF"), pdf("A")).unwrap();
    std::fs::write(saved.join("Steuerbescheinigung_2025.pdf"), pdf("B")).unwrap();
    std::fs::write(saved.join("notiz.txt"), b"kein PDF").unwrap();
    let dir = saved.to_string_lossy().to_string();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), Some(dir.clone()))
        .unwrap();
    assert_eq!((p.statement_count, p.statements_found), (4, 2));
    let mut options = options_from(&p, false);
    options.statements_dir = Some(dir);
    let r = v
        .import_moneymoney_database(path, PW.into(), options)
        .unwrap();
    assert_eq!(
        (
            r.documents_imported,
            r.documents_missing,
            r.documents_mismatched,
            r.documents_unlisted
        ),
        (2, 2, 0, 0)
    );
    let d = docs(&v);
    assert_eq!(d.len(), 2);
    // the original file name and date come from the database
    assert_eq!(d[0].0, "Kontoauszug_2026_001.pdf");
    assert_eq!(
        (d[1].0.as_str(), d[1].1.as_str(), d[1].2.as_str()),
        ("Steuerbescheinigung_2025.pdf", "steuer", "2026-01-02")
    );
    let _ = std::fs::remove_dir_all(saved);
}

#[test]
fn brand_banks_with_pseudo_bank_code_map_to_their_connector() {
    assert_eq!(
        connector_for("10012345", "Trade Republic"),
        Some(ConnectorKind::TradeRepublic)
    );
    assert_eq!(connector_for("", "PayPal"), Some(ConnectorKind::PayPal));
    assert_eq!(connector_for("12345678", "Sparkasse Musterstadt"), None);
    assert_eq!(
        connector_for("20041111", "comdirect"),
        Some(ConnectorKind::Comdirect)
    );
    // bunq: MoneyMoney files it under its German bank code; the login is an
    // API key, never a stored PIN, so no PIN is carried over.
    assert_eq!(connector_for("37019000", "bunq"), Some(ConnectorKind::Bunq));
    assert_eq!(connector_for("", "bunq"), Some(ConnectorKind::Bunq));
    assert_eq!(connector_for("BUNQNL2A", ""), Some(ConnectorKind::Bunq));
}

#[test]
fn bunq_accounts_are_recognised_in_every_country() {
    // BIC of any bunq branch
    for bic in [
        "BUNQNL2A",
        "BUNQDE82",
        "BUNQFRP2",
        "BUNQESM2",
        "bunqitm2",
        "BUNQNL2AXXX",
    ] {
        assert!(is_bunq_account(bic, "", ""), "{bic}");
    }
    // bank code: German branch, or a BIC MoneyMoney put in the bank code field
    assert!(is_bunq_account("", "", "370 190 00"));
    assert!(is_bunq_account("", "", "BUNQFRP2"));
    // IBAN alone: Dutch (bank id BUNQ) and German (bank code 37019000)
    assert!(is_bunq_account("", "NL12 BUNQ 0123 4567 89", ""));
    assert!(is_bunq_account("", "DE12 3701 9000 1234 5678 90", ""));
    // not bunq
    assert!(!is_bunq_account("INGBNL2A", "NL12 INGB 0123 4567 89", ""));
    assert!(!is_bunq_account(
        "",
        "DE89 3704 0044 0532 0130 00",
        "37040044"
    ));
    assert!(!is_bunq_account("BUNQ", "", "")); // too short for a BIC
    assert!(!is_bunq_account("", "", ""));
}

#[test]
fn bunq_logins_under_foreign_codes_get_a_bunq_connection_without_pin() {
    // Two bunq logins MoneyMoney filed without the German bank code: one
    // recognised by a Dutch IBAN, one by a French branch BIC only.
    let extra = "
INSERT INTO banks (rowid, country, bankcode, username, customer, credential, name, systemid, local_storage) VALUES
  (10, 'NL', '', 'me@example.com', '', 'GEHEIM-10', 'Mein Bankkonto', 'SYS', 'LS'),
  (11, 'FR', '', 'other@example.com', '', 'GEHEIM-11', 'Compte', 'SYS', 'LS');
INSERT INTO pins (bank_key, pin) VALUES (10, '1010');
INSERT INTO accounts (rowid, bank_key, account, bankcode, portfolio, iban, bic, currency, name, owner, kind, type, sort, initial_balance, group_key, cred) VALUES
  (10, 10, '', '', 0, 'NL12 BUNQ 0123 4567 89', '', 'EUR', 'Alltag', 'Max Mustermann', 0, 0, 10, 0, 1, 'GEHEIM'),
  (11, 11, '', '', 0, 'FR76 1234 5123 4512 3456 7890 123', 'BUNQFRP2', 'EUR', 'Courant', 'Max Mustermann', 0, 0, 11, 0, 1, 'GEHEIM');
";
    let v = vault();
    let path = fake_db_with(false, extra);
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    for id in [10, 11] {
        let b = p.banks.iter().find(|b| b.mm_id == id).unwrap();
        assert_eq!(b.connector, Some(ConnectorKind::Bunq), "bank {id}");
    }
    let r = v
        .import_moneymoney_database(path, PW.into(), options_from(&p, true))
        .unwrap();
    let bunq: Vec<_> = v
        .list_bank_connections()
        .unwrap()
        .into_iter()
        .filter(|c| c.connector == ConnectorKind::Bunq)
        .collect();
    let mut logins: Vec<_> = bunq.iter().map(|c| c.login_name.as_str()).collect();
    logins.sort();
    assert_eq!(logins, ["me@example.com", "other@example.com"]);
    // bunq signs in with an API key: no MoneyMoney PIN is carried over
    for c in &bunq {
        assert!(!v.connection_status(c.id).unwrap().has_pin);
    }
    assert_eq!(r.banks_skipped_unsupported, 1); // only the Sparkasse
    assert_eq!(r.banks_skipped_names.len(), 1);
}

#[test]
fn asset_class_numbers_map_only_the_confirmed_axis() {
    for (number, asset_class, instrument) in [
        (1, Some(AssetClass::Equity), None),
        (2, None, Some(InstrumentKind::Fund)),
        (3, Some(AssetClass::Bond), None),
        (4, Some(AssetClass::RealEstate), None),
        (5, Some(AssetClass::Commodity), None),
        (6, None, Some(InstrumentKind::Etf)),
        (7, Some(AssetClass::Crypto), None),
    ] {
        assert_eq!(
            moneymoney_classification(Some(number)),
            ClassificationValues {
                asset_class,
                instrument
            }
        );
    }
    for unknown in [None, Some(0), Some(-1), Some(99)] {
        assert_eq!(
            moneymoney_classification(unknown),
            ClassificationValues::default()
        );
    }
}

#[test]
fn imported_classification_refreshes_without_losing_user_choices() {
    use crate::asset_classification::ClassificationSource as S;
    let v = vault();
    // Deliberately differs from the dictionary: an explicit imported class wins.
    let path = fake_db_with(false, "UPDATE securities SET asset_class=3 WHERE rowid=1;");
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    v.import_moneymoney_database(path.clone(), PW.into(), options_from(&p, false))
        .unwrap();
    let account = account_by_name(&v, "Depot");
    let classification = || {
        v.classified_assets(account.id)
            .unwrap()
            .into_iter()
            .find(|a| a.holding.isin == "IE00B4L5Y983")
            .unwrap()
            .classification
    };
    let first = classification();
    assert_eq!(
        (first.asset_class, first.asset_class_source),
        (AssetClass::Bond, S::Import)
    );
    assert_eq!(
        (first.instrument, first.instrument_source),
        (InstrumentKind::Etf, S::Rule)
    );
    let key = first.security_key.unwrap();
    v.set_asset_class_override(key.clone(), Some(AssetClass::Commodity))
        .unwrap();
    {
        let db = Connection::open(&path).unwrap();
        db.execute_batch(&format!(
            "PRAGMA key='{}'; UPDATE securities SET asset_class=2 WHERE rowid=1;",
            PW.replace('\'', "''")
        ))
        .unwrap();
    }
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    v.import_moneymoney_database(path, PW.into(), options_from(&p, false))
        .unwrap();
    let refreshed = classification();
    assert_eq!(
        (refreshed.asset_class, refreshed.asset_class_source),
        (AssetClass::Commodity, S::User)
    );
    assert_eq!(
        (refreshed.instrument, refreshed.instrument_source),
        (InstrumentKind::Fund, S::Import)
    );
    v.set_asset_class_override(key, None).unwrap();
    assert_eq!(
        (
            classification().asset_class,
            classification().asset_class_source
        ),
        (AssetClass::Equity, S::Rule)
    );
    // Replacing two holdings also removes their obsolete cached classification rows.
    assert_eq!(
        v.db()
            .unwrap()
            .query_row("SELECT count(*) FROM holding_classifications", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
}

#[test]
fn missing_asset_class_column_preserves_older_database_support() {
    let path = fake_db_with(false, "ALTER TABLE securities DROP COLUMN asset_class;");
    let source = read_source(&path, &SecretString::new(PW.into())).unwrap();
    let holdings = source.holdings.get(&3).unwrap();
    assert_eq!(holdings.len(), 2);
    assert!(
        holdings
            .iter()
            .all(|h| h.classification == ClassificationValues::default())
    );
}

#[test]
fn depot_balance_from_shown_balance_and_holdings() {
    let v = vault();
    let path = fake_db();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    let depot = p.accounts.iter().find(|a| a.name == "Depot").unwrap();
    assert!(depot.is_portfolio);
    assert_eq!(depot.computed_balance_cents, 1_234_567);
    assert!(
        depot.balance_source.contains("Saldo laut Datenbank"),
        "{}",
        depot.balance_source
    );
    assert_eq!(depot.holding_count, 2, "sold position skipped");
    // accounts without the blob keep the derived balance
    let giro = p.accounts.iter().find(|a| a.name == "Girokonto").unwrap();
    assert!(giro.balance_source.contains("Saldenverlauf"));

    let r = v
        .import_moneymoney_database(path.clone(), PW.into(), options_from(&p, false))
        .unwrap();
    assert_eq!(r.holdings_imported, 2);
    let acc = account_by_name(&v, "Depot");
    assert_eq!(acc.balance_cents, 1_234_567);
    assert_eq!(acc.balance_date, "2026-03-07");
    let h = v.list_holdings(acc.id).unwrap();
    assert_eq!(h.len(), 2);
    let etf = h.iter().find(|x| x.isin == "IE00B4L5Y983").unwrap();
    assert_eq!(
        (
            etf.name.as_str(),
            etf.wkn.as_str(),
            etf.quantity,
            etf.current_price_cents,
            etf.purchase_price_cents,
            etf.value_cents
        ),
        ("Muster World ETF", "A0RPWH", 12.5, 8000, 7000, 100_000)
    );
    let cash = h.iter().find(|x| x.name == "Liquidität").unwrap();
    assert_eq!((cash.quantity, cash.value_cents), (0.0, 34_567));

    // second run replaces instead of duplicating
    let p2 = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    let r2 = v
        .import_moneymoney_database(path, PW.into(), options_from(&p2, false))
        .unwrap();
    assert_eq!(r2.holdings_imported, 2);
    assert_eq!(v.list_holdings(acc.id).unwrap().len(), 2);
}

#[test]
fn balances_blob_parsing() {
    assert_eq!(
        parse_balances_blob(Value::Text("11:4094.320000,3:EUR,".into())),
        Some((409_432, "EUR".into()))
    );
    assert_eq!(
        parse_balances_blob(Value::Blob(b"11:-671.230000,3:EUR,".to_vec())),
        Some((-67_123, "EUR".into()))
    );
    assert_eq!(parse_balances_blob(Value::Text(String::new())), None);
    assert_eq!(parse_balances_blob(Value::Null), None);
}

#[test]
fn reimport_of_emptied_depot_removes_stale_holdings() {
    let v = vault();
    let p = v
        .preview_moneymoney_database(fake_db(), PW.into(), None)
        .unwrap();
    v.import_moneymoney_database(fake_db(), PW.into(), options_from(&p, false))
        .unwrap();
    let depot = account_by_name(&v, "Depot");
    assert_eq!(v.list_holdings(depot.id).unwrap().len(), 2);

    // MoneyMoney later: all positions sold, balance down to the cash remainder
    let sold = fake_db_with(
        false,
        "UPDATE securities SET deleted = 1 WHERE account_key = 3;
         UPDATE accounts SET balances = CAST('6:0.010000,3:EUR,' AS BLOB) WHERE rowid = 3;",
    );
    let p2 = v
        .preview_moneymoney_database(sold.clone(), PW.into(), None)
        .unwrap();
    let d2 = p2.accounts.iter().find(|a| a.name == "Depot").unwrap();
    assert_eq!(d2.holding_count, 0);
    assert_eq!(
        d2.existing_account_id,
        Some(depot.id),
        "recognised by external ref"
    );
    let r = v
        .import_moneymoney_database(sold, PW.into(), options_from(&p2, false))
        .unwrap();
    assert_eq!(r.holdings_imported, 0);
    assert!(
        v.list_holdings(depot.id).unwrap().is_empty(),
        "stale positions removed"
    );
    assert_eq!(account_by_name(&v, "Depot").balance_cents, 1);
}

#[test]
fn shown_balance_also_wins_for_checking_accounts() {
    // MoneyMoney's displayed balance is authoritative for every account type:
    // it is what the user sees in MoneyMoney, independent of how far back the
    // booking history goes. The derived balance (initial balance + bookings)
    // remains the fallback for databases without the blob.
    let v = vault();
    let path = fake_db_with(
        false,
        "UPDATE accounts SET balances = CAST('10:1234.560000,3:EUR,' AS BLOB), balance_tsp = 1772971200 WHERE rowid = 1;",
    );
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    let giro = p.accounts.iter().find(|a| a.name == "Girokonto").unwrap();
    assert_eq!(giro.computed_balance_cents, 123_456);
    assert!(
        giro.balance_source.contains("Saldo laut Datenbank"),
        "{}",
        giro.balance_source
    );
    // other accounts (no blob) keep the derived value and its source text
    let visa = p.accounts.iter().find(|a| a.name == "Visa-Karte").unwrap();
    assert!(
        visa.balance_source.contains("Umsätze"),
        "{}",
        visa.balance_source
    );

    v.import_moneymoney_database(path, PW.into(), options_from(&p, false))
        .unwrap();
    let acc = account_by_name(&v, "Girokonto");
    assert_eq!(acc.balance_cents, 123_456);
    // balance date = max(newest booking, balance timestamp)
    assert_eq!(acc.balance_date, "2026-03-08");
    // #212: the opening balance is calibrated to MoneyMoney's own balance,
    // undated (before every booking), so later bookings continue from the
    // shown value
    assert_eq!(acc.opening_balance_date, "");
    assert_eq!(
        acc.opening_balance_cents,
        Some(123_456 + 4567 + 1230 + 10_000 - 250_000)
    );
}

#[test]
fn import_takes_over_moneymoney_groups() {
    let v = vault();
    let path = fake_db();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    let r = v
        .import_moneymoney_database(path.clone(), PW.into(), options_from(&p, false))
        .unwrap();
    assert_eq!(r.groups_created, 1);
    let gs = v.list_user_groups().unwrap();
    let privat = gs
        .iter()
        .find(|g| g.name == "Privat")
        .expect("group 'Privat'");
    let mut n: Vec<_> = privat.accounts.iter().map(|a| a.name.as_str()).collect();
    n.sort();
    assert_eq!(n, ["Girokonto", "Visa-Karte"]);
    assert!(
        gs.iter().any(|g| g.is_ungrouped),
        "accounts without MoneyMoney group stay ungrouped"
    );
    // idempotent: no second group, a user move is kept on re-import
    let giro = privat
        .accounts
        .iter()
        .find(|a| a.name == "Girokonto")
        .unwrap()
        .id;
    v.move_account_to_group(giro, None, None).unwrap();
    let r = v
        .import_moneymoney_database(path.clone(), PW.into(), options_from(&p, false))
        .unwrap();
    assert_eq!(r.groups_created, 0);
    let gs = v.list_user_groups().unwrap();
    assert!(
        gs.iter()
            .find(|g| g.is_ungrouped)
            .unwrap()
            .accounts
            .iter()
            .any(|a| a.id == giro)
    );
}

#[test]
fn import_takes_over_nested_groups_in_moneymoney_order() {
    let v = vault();
    // "Immobilien" (before "Privat") with two subgroups, listed Heilbronn before Neckarsulm
    let path = fake_db_with(
        false,
        "INSERT INTO account_groups (rowid, name, group_key, sort) VALUES
           (2, 'Immobilien', 0, 0), (3, 'ETW Neckarsulm', 2, 2), (4, 'ETW Heilbronn', 2, 1);
         UPDATE accounts SET group_key = 3 WHERE rowid = 3;
         UPDATE accounts SET group_key = 4 WHERE rowid = 4;",
    );
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    let shown = |name: &str| {
        p.accounts
            .iter()
            .find(|a| a.name == name)
            .unwrap()
            .group_name
            .clone()
    };
    assert_eq!(shown("Depot"), "Immobilien › ETW Neckarsulm");
    assert_eq!(shown("Girokonto"), "Privat");
    let r = v
        .import_moneymoney_database(path.clone(), PW.into(), options_from(&p, false))
        .unwrap();
    assert_eq!(r.groups_created, 4);
    let tree = |v: &Vault| -> Vec<(String, u32, Vec<String>)> {
        v.list_user_groups()
            .unwrap()
            .into_iter()
            .map(|g| {
                let mut names: Vec<String> = g.accounts.into_iter().map(|a| a.name).collect();
                names.sort();
                (g.name, g.depth, names)
            })
            .collect()
    };
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    assert_eq!(
        tree(&v),
        vec![
            ("Immobilien".to_string(), 0, vec![]),
            ("ETW Heilbronn".to_string(), 1, s(&["Dollar-Konto"])),
            ("ETW Neckarsulm".to_string(), 1, s(&["Depot"])),
            ("Privat".to_string(), 0, s(&["Girokonto", "Visa-Karte"])),
        ]
    );
    // the parent's sum covers its subgroups
    let groups = v.list_user_groups().unwrap();
    assert_eq!(
        groups[0].balance_cents,
        groups[1].balance_cents + groups[2].balance_cents
    );

    // re-import: nothing new, and a group the user moved stays where it is
    let heilbronn = groups[1].uuid.clone();
    v.set_user_group_parent(heilbronn, None).unwrap();
    let r = v
        .import_moneymoney_database(path, PW.into(), options_from(&p, false))
        .unwrap();
    assert_eq!(r.groups_created, 0);
    let after = tree(&v);
    assert_eq!(after.iter().find(|g| g.0 == "ETW Heilbronn").unwrap().1, 0);
    assert_eq!(after.iter().find(|g| g.0 == "ETW Neckarsulm").unwrap().1, 1);
}

#[test]
fn imports_category_rules() {
    let v = vault();
    let path = fake_db_with(
        false,
        "UPDATE categories SET rules = 'name:REWE OR name:EDEKA' WHERE rowid = 2;
         UPDATE categories SET rules = '(Gehalt' WHERE rowid = 3;
         UPDATE categories SET rules = 'purpose:Strom* AND amount<0' WHERE rowid = 4;
         UPDATE categories SET rules = 'name:Finanzamt', apply_to_account = '1' WHERE rowid = 5;
         INSERT INTO categories (rowid, name, type, group_key, sort, rules) VALUES
           (6, 'Spenden', 1, 0, 4, 'reference:X'),
           (7, 'Sonstiges', 1, 0, 5, '   ');",
    );
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    let r = v
        .import_moneymoney_database(path.clone(), PW.into(), options_from(&p, false))
        .unwrap();
    assert_eq!((r.rules_created, r.rules_disabled), (5, 3));

    let rules = v.list_rules().unwrap();
    let by_name = |n: &str| rules.iter().find(|r| r.name == n).unwrap().clone();
    let supermarkt = by_name("Supermarkt");
    assert_eq!(supermarkt.match_kind, MatchKind::Expression);
    assert_eq!(supermarkt.pattern, "name:REWE OR name:EDEKA");
    assert!(supermarkt.enabled);
    assert!(supermarkt.priority >= 1000);
    assert!(by_name("Strom").enabled);
    assert!(
        by_name("Strom").priority > supermarkt.priority,
        "MoneyMoney order"
    );
    assert!(!by_name("Gehalt").enabled, "broken syntax");
    assert!(
        !by_name("Steuern / Büro (nur für bestimmte Konten)").enabled,
        "account restriction is named in the rule"
    );
    assert!(!by_name("Spenden").enabled, "unsupported field");
    assert!(
        rules.iter().all(|r| r.name != "Sonstiges"),
        "blank rule skipped"
    );

    let again = v
        .import_moneymoney_database(path, PW.into(), options_from(&p, false))
        .unwrap();
    assert_eq!((again.rules_created, again.rules_disabled), (0, 0));
    assert_eq!(v.list_rules().unwrap().len(), rules.len());
}

/// Records every progress report; cancels after `cancel_after` of them.
struct Recorder {
    seen: std::sync::Mutex<Vec<MoneyMoneyDbImportProgress>>,
    cancel_after: Option<usize>,
}

impl MoneyMoneyImportObserver for Recorder {
    fn progress(&self, progress: MoneyMoneyDbImportProgress) -> bool {
        let mut seen = self.seen.lock().unwrap();
        seen.push(progress);
        self.cancel_after.is_none_or(|n| seen.len() < n)
    }
}

#[test]
fn import_reports_progress_and_can_be_cancelled() {
    let folder = temp("mm-progress");
    let bank = folder.join("Statements").join("Musterbank");
    std::fs::create_dir_all(&bank).unwrap();
    std::fs::create_dir_all(folder.join("Database")).unwrap();
    std::fs::rename(
        statements_db(),
        folder.join("Database").join("MoneyMoney.sqlite"),
    )
    .unwrap();
    std::fs::write(bank.join("Kontoauszug_2026_001.pdf"), pdf("A")).unwrap();
    let path = folder.to_string_lossy().to_string();
    let v = vault();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();

    // cancelled in the middle: an error, and nothing at all is stored
    let stop = Arc::new(Recorder {
        seen: Default::default(),
        cancel_after: Some(4),
    });
    let r = v.import_moneymoney_database_with_progress(
        path.clone(),
        PW.into(),
        options_from(&p, true),
        stop.clone(),
    );
    assert!(matches!(r, Err(CoreError::InvalidInput { .. })));
    assert_eq!(stop.seen.lock().unwrap().len(), 4);
    assert!(v.list_accounts().unwrap().is_empty());
    assert!(v.list_bank_connections().unwrap().is_empty());
    assert!(docs(&v).is_empty());

    // a full run: from "reading" over every account to the end
    let all = Arc::new(Recorder {
        seen: Default::default(),
        cancel_after: None,
    });
    let r = v
        .import_moneymoney_database_with_progress(
            path,
            PW.into(),
            options_from(&p, false),
            all.clone(),
        )
        .unwrap();
    assert_eq!(r.documents_imported, 1);
    let seen = all.seen.lock().unwrap();
    let (first, last) = (seen.first().unwrap(), seen.last().unwrap());
    assert_eq!((first.account_index, first.account_count), (0, 0));
    let n = p.accounts.len() as u32;
    assert_eq!((last.account_index, last.account_count), (n + 1, n));
    assert_eq!(last.fraction, 1.0);
    assert_eq!(last.bookings_done, last.bookings_total);
    assert_eq!((last.documents_done, last.documents_total), (4, 4));
    assert!(last.account_name.is_empty());
    assert!(
        seen.windows(2)
            .all(|w| w[0].fraction <= w[1].fraction && w[0].account_index <= w[1].account_index)
    );
    assert!(seen.iter().any(|s| s.account_name == "Girokonto"));
    let _ = std::fs::remove_dir_all(folder);
}

/// The "already imported?" lookup runs once per document: it must use the
/// index on `source_url`, not scan the table (and with it every stored PDF).
#[test]
fn document_lookup_uses_the_source_index() {
    let v = vault();
    let db = v.db().unwrap();
    let plan = crate::test_support::query_plan(&db, DOCUMENT_EXISTS_SQL, ["moneymoney:1:abc"]);
    assert!(plan.contains("booking_documents_source"), "{plan}");
    assert!(!plan.contains("SCAN booking_documents"), "{plan}");
}

/// Issue #94: transfers between own imported accounts (matched by IBAN or a
/// MoneyMoney "Umbuchung" category) must not inflate income / expenses.
#[test]
fn transfers_between_own_accounts_are_neutral_in_kpis() {
    let kpis = |path: String| {
        let v = vault();
        let p = v
            .preview_moneymoney_database(path.clone(), PW.into(), None)
            .unwrap();
        v.import_moneymoney_database(path, PW.into(), options_from(&p, false))
            .unwrap();
        let page = v.list_transactions(BookingFilter::default()).unwrap();
        (page.income_cents, page.expense_cents, page.total_count)
    };
    let base = kpis(fake_db());
    let extra = r#"
INSERT INTO accounts (rowid, bank_key, account, bankcode, portfolio, iban, currency, name, owner, kind, type, sort, initial_balance, group_key, cred) VALUES
  (5, 1, '0648489890', '20041111', 0, 'DE12 5001 0517 0648 4898 90', 'EUR', 'Tagesgeld', 'Max Mustermann', 0, 1, 5, 0, 1, '');
INSERT INTO categories (rowid, name, type, group_key, sort) VALUES (6, 'Umbuchung', 1, 7, 1);
INSERT INTO category_groups (rowid, name, group_key, sort) VALUES (7, 'Finanzen', 0, 3);
INSERT INTO transactions (rowid, local_account_key, account, bankcode, amount, currency, timestamp, value_timestamp, comment, booked, unformatted_type, unformatted_purpose, name, checkmark, splitted, split_key, category_key) VALUES
  (10, 1, 'DE12500105170648489890', 'COBADEFFXXX', -500.0, 'EUR', 1772712000, 0, '', 1, 'UEBERWEISUNG', 'Sparen', 'Max Mustermann', 0, 0, 10, 1),
  (11, 5, 'DE89370400440532013000', 'COBADEFFXXX', 500.0, 'EUR', 1772712000, 0, '', 1, 'GUTSCHRIFT', 'Sparen', 'Max Mustermann', 0, 0, 11, 1),
  (12, 1, '', '', -200.0, 'EUR', 1772798400, 0, '', 1, 'UEBERWEISUNG', 'Ausgleich Karte', 'comdirect', 0, 0, 12, 6),
  (13, 2, '', '', 200.0, 'EUR', 1772798400, 0, '', 1, 'GUTSCHRIFT', 'Ausgleich Karte', 'comdirect', 0, 0, 13, 6);
"#;
    let with = kpis(fake_db_with(false, extra));
    assert_eq!(with.2, base.2 + 4, "transfers are still listed");
    assert_eq!(
        (with.0, with.1),
        (base.0, base.1),
        "but don't count as income / expense"
    );
}

fn src_acc(id: i64, account: &str, bankcode: &str, iban: &str, currency: &str) -> SrcAccount {
    SrcAccount {
        id,
        account: account.into(),
        bankcode: bankcode.into(),
        iban: normalize_iban(iban),
        currency: currency.into(),
        ..Default::default()
    }
}

fn src_tx(id: i64, account: i64, cp: &str, cp_bank: &str, amount: f64, ts: i64) -> SrcTx {
    SrcTx {
        id,
        account_key: account,
        cp_account: cp.into(),
        cp_bankcode: cp_bank.into(),
        amount: Some(amount),
        ts: Some(ts),
        booked: true,
        ..Default::default()
    }
}

#[test]
fn own_accounts_are_found_by_account_number_and_bank_code() {
    // Issue #94: pre-SEPA bookings / cards / depots carry only Kto + BLZ
    let giro = src_acc(
        1,
        "0532013000",
        "37040044",
        "DE89 3704 0044 0532 0130 00",
        "EUR",
    );
    let tagesgeld = src_acc(2, "0098765432", "37040044", "", "EUR");
    let card = src_acc(3, "4711", "37040044", "", "EUR");
    let src = Source {
        accounts: vec![giro.clone(), tagesgeld.clone(), card.clone()],
        ..Default::default()
    };
    let own = |from: &SrcAccount, cp: &str, bank: &str| {
        src.own_counter_account(from, cp, bank).map(|a| a.id)
    };
    // Kto/BLZ against the BLZ/Kto inside the own German IBAN
    assert_eq!(own(&tagesgeld, "532013000", "37040044"), Some(1));
    // Kto without IBAN on the own side, leading zeros ignored
    assert_eq!(own(&giro, "98765432", "37040044"), Some(2));
    assert_eq!(own(&giro, "4711", ""), Some(3));
    // German IBAN pointing at an own account that only has Kto/BLZ
    let iban = "DE77370400440098765432";
    assert!(validate_iban(iban));
    assert_eq!(own(&giro, iban, ""), Some(2));
    // other bank code, foreign account, too short, itself: no match
    assert_eq!(own(&giro, "98765432", "10010010"), None);
    assert_eq!(own(&giro, "12345678", "37040044"), None);
    assert_eq!(own(&giro, "47", ""), None);
    assert_eq!(own(&giro, "532013000", "37040044"), None);
}

#[test]
fn own_transfers_get_the_own_iban_and_the_transfer_category() {
    let giro = src_acc(
        1,
        "0532013000",
        "37040044",
        "DE89 3704 0044 0532 0130 00",
        "EUR",
    );
    let tagesgeld = src_acc(2, "0098765432", "37040044", "", "EUR");
    let src = Source {
        accounts: vec![giro.clone(), tagesgeld.clone()],
        txs: vec![
            src_tx(1, 2, "532013000", "37040044", 100.0, 1_600_000_000),
            src_tx(2, 1, "98765432", "37040044", -100.0, 1_600_000_000),
            src_tx(3, 1, "12345", "10010010", -5.0, 1_600_000_000),
        ],
        ..Default::default()
    };
    let rows = src.rows_for(&tagesgeld).rows;
    assert_eq!(rows[0].iban, "DE89370400440532013000");
    assert_eq!(rows[0].category_parts, vec!["Umbuchungen".to_string()]);
    let rows = src.rows_for(&giro).rows;
    let to_tg = rows.iter().find(|r| r.amount_cents == -10_000).unwrap();
    assert_eq!(to_tg.iban, "", "own account without IBAN: no invented IBAN");
    assert_eq!(to_tg.category_parts, vec!["Umbuchungen".to_string()]);
    let other = rows.iter().find(|r| r.amount_cents == -500).unwrap();
    assert!(other.category_parts.is_empty());
}

#[test]
fn fallback_balance_counts_account_currency_from_the_initial_balance_date_on() {
    let acc = src_acc(1, "1", "", "", "EUR");
    let mut usd = src_tx(3, 1, "", "", -50.0, 1_700_000_000);
    usd.currency = "USD".into();
    let mut balances = HashMap::new();
    // initial balance 100,00 as of 2020-09-13 (1_600_000_000)
    balances.insert((1, "EUR".to_string()), (1_600_000_000, 100.0));
    let src = Source {
        accounts: vec![acc.clone()],
        txs: vec![
            src_tx(1, 1, "", "", 2500.0, 1_500_000_000), // before: part of the 100,00
            src_tx(2, 1, "", "", -20.0, 1_600_000_000),  // same day: counts
            usd,                                         // other currency: not summed
        ],
        balances,
        ..Default::default()
    };
    let r = src.rows_for(&acc);
    assert_eq!(r.rows.len(), 3, "all bookings are still imported");
    assert_eq!(r.balance_cents, 10_000 - 2_000);
}

#[test]
fn starter_wipe_on_import_soft_deletes_starter_categories() {
    let v = vault();
    let starter = v.seed_starter_categories().unwrap() as i64;
    let path = fake_db();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    v.import_moneymoney_database(path, PW.into(), options_from(&p, false))
        .unwrap();
    let db = v.db().unwrap();
    let n = |sql: &str| db.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
    assert_eq!(
        n("SELECT count(*) FROM categories WHERE deleted_ms IS NOT NULL"),
        starter
    );
    assert_eq!(n("SELECT count(*) FROM rules WHERE deleted_ms IS NULL"), 0);
    assert!(n("SELECT count(*) FROM rules WHERE deleted_ms IS NOT NULL") > 0);
    // the imported tree is live and stamped, with unique uuids
    assert_eq!(
        n("SELECT count(*) FROM categories WHERE deleted_ms IS NULL"),
        7
    );
    assert_eq!(
        n(
            "SELECT count(*) FROM categories WHERE deleted_ms IS NULL AND name_ms > 0 AND uuid IS NOT NULL"
        ),
        7
    );
    assert_eq!(
        n("SELECT count(DISTINCT uuid) FROM categories"),
        starter + 7
    );
}

#[test]
fn import_stamps_manual_category_and_note() {
    let v = vault();
    let path = fake_db();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    v.import_moneymoney_database(path.clone(), PW.into(), options_from(&p, false))
        .unwrap();
    {
        let db = v.db().unwrap();
        let n = |sql: &str| db.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
        assert!(
            n(
                "SELECT count(*) FROM transactions WHERE category_source = 'manual' AND category_ms > 0"
            ) > 0
        );
        assert_eq!(
            n(
                "SELECT count(*) FROM transactions WHERE category_source = 'manual' AND category_ms = 0"
            ),
            0
        );
        assert_eq!(
            n("SELECT count(*) FROM transactions WHERE note != '' AND note_ms = 0"),
            0
        );
        assert!(n("SELECT count(*) FROM transactions WHERE note != '' AND note_ms > 0") > 0);
        // pretend the bookings existed without category and note: the re-import enriches them
        db.execute("UPDATE transactions SET category_id = NULL, category_source = 'none', note = '', category_ms = 0, note_ms = 0", []).unwrap();
    }
    v.import_moneymoney_database(path, PW.into(), options_from(&p, false))
        .unwrap();
    let db = v.db().unwrap();
    let n = |sql: &str| db.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
    assert!(
        n("SELECT count(*) FROM transactions WHERE category_source = 'manual' AND category_ms > 0")
            > 0
    );
    assert_eq!(
        n("SELECT count(*) FROM transactions WHERE category_source = 'manual' AND category_ms = 0"),
        0
    );
    assert!(n("SELECT count(*) FROM transactions WHERE note != '' AND note_ms > 0") > 0);
    assert_eq!(
        n("SELECT count(*) FROM transactions WHERE note != '' AND note_ms = 0"),
        0
    );
}

#[test]
fn reimport_revives_deleted_account_with_its_cascade_bookings_only() {
    let v = vault();
    let path = fake_db();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    v.import_moneymoney_database(path.clone(), PW.into(), options_from(&p, true))
        .unwrap();
    let giro = account_by_name(&v, "Girokonto").id;
    let live = |v: &Vault| -> i64 {
        v.db()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM transactions WHERE account_id = ?1 AND deleted_ms IS NULL",
                [giro],
                |r| r.get(0),
            )
            .unwrap()
    };
    let total = live(&v);
    assert!(total >= 2);
    v.db()
        .unwrap()
        .execute(
            "UPDATE transactions SET deleted_ms = 5 WHERE id = (SELECT min(id) FROM transactions WHERE account_id = ?1)",
            [giro],
        )
        .unwrap();
    v.set_account_archived(giro, true).unwrap();
    v.delete_account(giro).unwrap();
    assert_eq!(live(&v), 0);
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    v.import_moneymoney_database(path, PW.into(), options_from(&p, true))
        .unwrap();
    assert_eq!(
        v.list_accounts()
            .unwrap()
            .iter()
            .filter(|a| a.id == giro)
            .count(),
        1
    );
    assert_eq!(
        live(&v),
        total - 1,
        "individually deleted booking stays deleted"
    );
}

#[test]
fn account_checks_confirm_a_complete_import() {
    let v = vault();
    let path = fake_db();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    let r = v
        .import_moneymoney_database(path.clone(), PW.into(), options_from(&p, false))
        .unwrap();
    assert_eq!(r.account_checks.len(), 4);
    for c in &r.account_checks {
        assert!(c.ok, "{c:?}");
        assert_eq!(c.found_bookings, c.mm_bookings, "{c:?}");
        assert_eq!((c.deleted_bookings, c.other_bookings), (0, 0), "{c:?}");
        assert_eq!(c.girocrab_balance_cents, c.mm_balance_cents, "{c:?}");
    }
    let giro = r
        .account_checks
        .iter()
        .find(|c| c.name == "Girokonto")
        .unwrap();
    assert_eq!(giro.mm_bookings, 4);
    let total: u32 = r.account_checks.iter().map(|c| c.mm_bookings).sum();
    assert_eq!(total, r.bookings_new);

    // a booking deleted in GiroCrab stays deleted – and still counts as there
    v.db()
        .unwrap()
        .execute(
            "UPDATE transactions SET deleted_ms = 1 WHERE id = (SELECT min(id) FROM transactions WHERE account_id = ?1)",
            [giro.account_id],
        )
        .unwrap();
    let r = v
        .import_moneymoney_database(path, PW.into(), options_from(&p, false))
        .unwrap();
    let giro = r
        .account_checks
        .iter()
        .find(|c| c.name == "Girokonto")
        .unwrap();
    assert!(giro.ok, "{giro:?}");
    assert_eq!((giro.found_bookings, giro.deleted_bookings), (3, 1));
}

#[test]
fn account_check_flags_missing_bookings_and_a_wrong_balance() {
    let v = vault();
    let path = fake_db();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    let r = v
        .import_moneymoney_database(path.clone(), PW.into(), options_from(&p, false))
        .unwrap();
    let giro = r
        .account_checks
        .iter()
        .find(|c| c.name == "Girokonto")
        .unwrap()
        .clone();
    // something went wrong behind the import's back: a booking vanished,
    // the balance is off
    {
        let db = v.db().unwrap();
        db.execute(
            "DELETE FROM transactions WHERE id = (SELECT min(id) FROM transactions WHERE account_id = ?1)",
            [giro.account_id],
        )
        .unwrap();
        db.execute(
            "UPDATE accounts SET balance_cents = balance_cents + 1 WHERE id = ?1",
            [giro.account_id],
        )
        .unwrap();
    }
    let src = read_source(&path, &SecretString::new(PW.into())).unwrap();
    let a = src.accounts.iter().find(|a| a.id == giro.mm_id).unwrap();
    let db = v.db().unwrap();
    let c = check_account(
        &db,
        a,
        "Girokonto",
        giro.account_id,
        &src.rows_for(a),
        false,
    )
    .unwrap();
    assert!(!c.ok);
    assert_eq!((c.mm_bookings, c.found_bookings), (4, 3));
    assert_eq!(c.girocrab_balance_cents, c.mm_balance_cents + 1);
    // a bank-synced account keeps the bank's balance: only the bookings count
    let c = check_account(&db, a, "Girokonto", giro.account_id, &src.rows_for(a), true).unwrap();
    assert!(!c.ok, "a missing booking is still flagged");
}

#[test]
fn banks_without_own_connector_are_set_up_through_enable_banking() {
    let v = vault();
    let path = fake_db_with(
        false,
        "INSERT INTO banks (rowid, country, bankcode, username, customer, credential, name, systemid, local_storage)
           VALUES (30, 'DE', '50024024', 'c24-login', '', 'GEHEIM-30', 'C24 Bank', 'SYS', 'LS');
         INSERT INTO accounts (rowid, bank_key, account, bankcode, portfolio, iban, currency, name, owner, kind, type, sort, initial_balance, group_key, cred)
           VALUES (30, 30, '1234567890', '50024024', 0, 'DE02 5002 4024 1234 5678 90', 'EUR', 'C24 Smartkonto', 'Max Mustermann', 0, 0, 30, 0, 0, 'GEHEIM'),
                  (31, 30, '0987654321', '50024024', 0, '', 'EUR', 'C24 Pocket', 'Max Mustermann', 0, 0, 31, 0, 0, 'GEHEIM');",
    );
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    let c24 = p.banks.iter().find(|b| b.mm_id == 30).unwrap();
    assert_eq!(c24.connector, None);
    assert_eq!(c24.connection_options.len(), 1);
    let eb = eb_option(&p, 30).expect("C24 is in the PSD2 catalogue");
    let psd2 = eb.psd2_bank.clone().unwrap();
    assert_eq!((psd2.aspsp.as_str(), psd2.country.as_str()), ("C24", "DE"));
    assert_eq!(eb.key, "enablebanking:DE:C24");
    // the Sparkasse stays unsupported: neither FinTS-known here nor PSD2
    let spk = p.banks.iter().find(|b| b.mm_id == 3).unwrap();
    assert!(spk.connection_options.is_empty());

    let mut o = options_from(&p, true);
    o.bank_choices = vec![MoneyMoneyDbBankChoice {
        mm_bank_id: 30,
        connection: Some(eb),
    }];
    let r = v
        .import_moneymoney_database(path.clone(), PW.into(), o.clone())
        .unwrap();
    assert_eq!(r.psd2_connections.len(), 1);
    let pc = &r.psd2_connections[0];
    assert!(pc.created);
    assert_eq!(pc.bank_name, "C24 Bank");
    let mut names: Vec<&str> = pc.accounts.iter().map(|a| a.name.as_str()).collect();
    names.sort();
    assert_eq!(names, ["C24 Pocket", "C24 Smartkonto"]);
    assert!(
        pc.accounts
            .iter()
            .any(|a| a.iban.replace(' ', "") == "DE02500240241234567890")
    );
    let conn = v
        .list_bank_connections()
        .unwrap()
        .into_iter()
        .find(|c| c.id == pc.connection_id)
        .unwrap();
    assert_eq!(conn.connector, ConnectorKind::EnableBanking);
    // Sparkasse: no choice → still skipped, by name
    assert_eq!(r.banks_skipped_unsupported, 1);
    assert_eq!(r.banks_skipped_names.len(), 1);
    assert!(
        !r.psd2_connections
            .iter()
            .any(|p| r.banks_skipped_names.contains(&p.bank_name))
    );

    // second run: the same connection, nothing new
    let r = v.import_moneymoney_database(path, PW.into(), o).unwrap();
    assert_eq!(r.psd2_connections.len(), 1);
    assert!(!r.psd2_connections[0].created);
    assert_eq!(r.psd2_connections[0].connection_id, pc.connection_id);
    let eb = v
        .list_bank_connections()
        .unwrap()
        .into_iter()
        .filter(|c| c.connector == ConnectorKind::EnableBanking)
        .count();
    assert_eq!(eb, 1);
}

#[test]
fn psd2_choice_without_create_connections_does_nothing() {
    let v = vault();
    let path = fake_db_with(
        false,
        "INSERT INTO banks (rowid, country, bankcode, username, customer, credential, name, systemid, local_storage)
           VALUES (30, 'DE', '50024024', 'c24-login', '', 'GEHEIM-30', 'C24 Bank', 'SYS', 'LS');",
    );
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    let mut o = options_from(&p, false);
    o.bank_choices = vec![MoneyMoneyDbBankChoice {
        mm_bank_id: 30,
        connection: eb_option(&p, 30),
    }];
    let r = v.import_moneymoney_database(path, PW.into(), o).unwrap();
    assert!(r.psd2_connections.is_empty());
    assert!(v.list_bank_connections().unwrap().is_empty());
}

#[test]
fn connection_priority_fints_then_own_api_then_enable_banking() {
    let v = vault();
    let path = fake_db_with(
        false,
        "INSERT INTO banks (rowid, country, bankcode, username, customer, credential, name, systemid, local_storage) VALUES
           (40, 'DE', '12030000', 'hb-login', '', 'X', 'Hanseatic Bank', 'SYS', 'LS'),
           (41, 'DE', '50024024', 'c24-login', '', 'X', 'C24 Bank', 'SYS', 'LS'),
           (42, 'DE', '', 'pp-login', '', 'X', 'PayPal', 'SYS', 'LS');",
    );
    let p = v
        .preview_moneymoney_database(path, PW.into(), None)
        .unwrap();
    let bank = |id: i64| p.banks.iter().find(|b| b.mm_id == id).unwrap();
    // 1. FinTS wins: the bank code is FinTS-known, although the name is in
    //    the PSD2 catalogue too
    assert_eq!(bank(40).connector, Some(ConnectorKind::FinTsGeneric));
    let vias: Vec<_> = bank(40).connection_options.iter().map(|o| o.via).collect();
    assert_eq!(
        vias,
        [
            MoneyMoneyDbConnectionVia::FinTs,
            MoneyMoneyDbConnectionVia::EnableBanking
        ]
    );
    // 2. own API: comdirect (no FinTS entry on purpose), PayPal
    assert_eq!(bank(1).connector, Some(ConnectorKind::Comdirect));
    assert_eq!(bank(42).connector, Some(ConnectorKind::PayPal));
    assert_eq!(
        bank(42).connection_options[0].via,
        MoneyMoneyDbConnectionVia::OwnApi
    );
    // 3. Enable Banking only without either
    assert_eq!(bank(41).connector, None);
    assert_eq!(
        eb_option(&p, 41).and_then(|o| o.psd2_bank).map(|b| b.aspsp),
        Some("C24".to_string())
    );
}

#[test]
fn a_bank_can_be_switched_to_enable_banking_or_left_out() {
    let v = vault();
    // Hanseatic Bank: FinTS-known code, but also in the PSD2 catalogue
    let path = fake_db_with(
        false,
        "INSERT INTO banks (rowid, country, bankcode, username, customer, credential, name, systemid, local_storage) VALUES
           (40, 'DE', '12030000', 'hb-login', '', 'X', 'Hanseatic Bank', 'SYS', 'LS');
         INSERT INTO accounts (rowid, bank_key, account, bankcode, portfolio, iban, currency, name, owner, kind, type, sort, initial_balance, group_key, cred)
           VALUES (40, 40, '1234567890', '12030000', 0, 'DE02 1203 0000 1234 5678 90', 'EUR', 'Hanseatic Giro', 'Max', 0, 0, 40, 0, 0, 'X');",
    );
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    let mut o = options_from(&p, true);
    o.bank_choices = vec![
        // FinTS bank → Enable Banking instead
        MoneyMoneyDbBankChoice {
            mm_bank_id: 40,
            connection: eb_option(&p, 40),
        },
        // comdirect → not connected at all
        MoneyMoneyDbBankChoice {
            mm_bank_id: 1,
            connection: None,
        },
    ];
    let r = v.import_moneymoney_database(path, PW.into(), o).unwrap();
    let conns = v.list_bank_connections().unwrap();
    assert!(
        conns
            .iter()
            .all(|c| c.connector != ConnectorKind::Comdirect)
    );
    assert!(conns.iter().all(
        |c| c.connector != ConnectorKind::FinTsGeneric || !c.display_name.contains("Hanseatic")
    ));
    assert_eq!(r.psd2_connections.len(), 1);
    assert_eq!(r.psd2_connections[0].bank_name, "Hanseatic Bank");
    assert_eq!(r.psd2_connections[0].accounts.len(), 1);
    // the comdirect accounts are still imported, just without a login
    assert!(
        r.account_checks
            .iter()
            .any(|c| c.name == "Girokonto" && c.ok)
    );
}

/// #201: a database locked by a running MoneyMoney is reported as such –
/// quickly and not as a wrong password – and reads fine once released.
#[test]
fn database_locked_by_running_moneymoney_is_not_a_wrong_password() {
    let path = fake_db();
    let v = vault();
    let holder = Connection::open(&path).unwrap();
    holder
        .execute_batch(&format!("PRAGMA key = '{}';", PW.replace('\'', "''")))
        .unwrap();
    holder
        .execute_batch(
            "PRAGMA locking_mode = EXCLUSIVE; BEGIN; UPDATE accounts SET sort = sort; COMMIT;",
        )
        .unwrap();
    let started = std::time::Instant::now();
    let err = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap_err();
    assert!(started.elapsed() < std::time::Duration::from_secs(3));
    assert!(matches!(err, CoreError::Authentication { .. }));
    let msg = err.to_string();
    assert!(msg.contains("geöffnet und gesperrt"), "{msg}");
    assert!(!msg.contains("Falsches Passwort"), "{msg}");
    drop(holder);
    let p = v
        .preview_moneymoney_database(path, PW.into(), None)
        .unwrap();
    assert_eq!(p.accounts.len(), 4);
}

/// Issue #94: MoneyMoney keeps closed accounts with the same IBAN; they must
/// not all be merged into the one vault account that has this IBAN.
#[test]
fn accounts_sharing_an_iban_are_merged_into_different_vault_accounts() {
    let v = vault();
    let existing = {
        let db = v.db().unwrap();
        let gid = Vault::ensure_group(&db, "Giro & Tagesgeld", "building.columns", 1).unwrap();
        db.execute(
            "INSERT INTO accounts(connection_id, group_id, external_ref, name, iban, currency, kind, balance_cents)
             VALUES (NULL, ?1, 'abc-123', 'Mein Giro', 'DE89370400440532013000', 'EUR', 'checking', 0)",
            params![gid],
        )
        .unwrap();
        db.last_insert_rowid()
    };
    let path = temp("dup-iban.db").to_string_lossy().to_string();
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(SCHEMA).unwrap();
    conn.execute_batch(
        "INSERT INTO accounts (rowid, account, bankcode, iban, currency, name, kind, type, sort) VALUES
           (1, '0532013000', '20041111', 'DE89 3704 0044 0532 0130 00', 'EUR', 'Sparbrief alt', 0, 0, 1),
           (2, '0532013000', '20041111', 'DE89 3704 0044 0532 0130 00', 'EUR', 'Sparbrief neu', 0, 0, 2);",
    )
    .unwrap();
    drop(conn);
    let p = v
        .preview_moneymoney_database(path, String::new(), None)
        .unwrap();
    let ids: Vec<Option<i64>> = p.accounts.iter().map(|a| a.existing_account_id).collect();
    assert_eq!(ids, vec![Some(existing), None]);
}

#[cfg(unix)]
#[test]
fn statement_lookup_rejects_symlinks_leaving_the_import_folder() {
    use std::os::unix::fs::symlink;
    let outside = temp("mm-symlink-outside");
    std::fs::create_dir_all(&outside).unwrap();
    let secret = pdf("GEHEIM");
    std::fs::write(outside.join("secret.pdf"), &secret).unwrap();
    let folder = temp("mm-symlink-root");
    let bank = folder.join("Statements").join("Bank");
    std::fs::create_dir_all(&bank).unwrap();
    std::fs::write(bank.join("ok.pdf"), pdf("OK")).unwrap();
    // a file link, a folder link and a link found only by checksum
    symlink(outside.join("secret.pdf"), bank.join("link.pdf")).unwrap();
    symlink(&outside, folder.join("Statements").join("Linked")).unwrap();
    let doc = |dir: &str, name: &str, digest: String| SrcDoc {
        account_key: 1,
        directory: dir.into(),
        filename: name.into(),
        created_ts: None,
        digest,
    };
    let mut files = StatementFiles::new(folder.join("Statements"));
    assert!(files.find(&doc("Bank", "ok.pdf", String::new())).is_some());
    assert!(
        files
            .find(&doc("Bank", "link.pdf", String::new()))
            .is_none()
    );
    assert!(
        files
            .find(&doc("Linked", "secret.pdf", String::new()))
            .is_none()
    );
    // the digest fallback must not hand out the linked outside file either
    let digest = sha256_hex(&secret);
    assert!(files.find(&doc("", "other.pdf", digest)).is_none());
}

/// A MoneyMoney account folded into the bank account that took it over (see
/// `sync_merge::twins`): importing MoneyMoney again goes to the bank account
/// instead of reviving the twin.
#[test]
fn import_goes_to_the_bank_account_a_twin_was_folded_into() {
    let v = vault();
    let c = v
        .add_bank_connection(ConnectorKind::Comdirect, String::new(), "login".into())
        .unwrap();
    let ext = format!("{EXT_PREFIX}1:{}", fnv("20041111|0102211000"));
    let mut ids = Vec::new();
    {
        let db = v.db().unwrap();
        let gid = Vault::ensure_group(&db, "Giro & Tagesgeld", "building.columns", 1).unwrap();
        for r in ["API1", ext.as_str()] {
            db.execute(
                "INSERT INTO accounts(connection_id, group_id, external_ref, name, iban, currency, kind, balance_cents)
                 VALUES (?1, ?2, ?3, 'Girokonto', 'DE53200411110102211000', 'EUR', 'checking', 0)",
                params![c, gid, r],
            )
            .unwrap();
            let id = db.last_insert_rowid();
            db.execute(
                "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents)
                 VALUES (?1, 'mm-1', '2026-01-01', -5)",
                [id],
            )
            .unwrap();
            ids.push(id);
        }
        assert_eq!(crate::sync_merge::twins::fold_twins(&db).unwrap(), 1);
    }
    let path = temp("twin-src.db").to_string_lossy().to_string();
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(SCHEMA).unwrap();
    conn.execute_batch(
        "INSERT INTO accounts (rowid, account, bankcode, iban, currency, name, kind, type, sort) VALUES
           (1, '0102211000', '20041111', 'DE53 2004 1111 0102 2110 00', 'EUR', 'Girokonto', 0, 0, 1);",
    )
    .unwrap();
    drop(conn);
    let p = v
        .preview_moneymoney_database(path, String::new(), None)
        .unwrap();
    assert_eq!(p.accounts[0].existing_account_id, Some(ids[0]));
}

// ------------------------------------------------------- transfer templates (#579)

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// One `payments` row; `data` is a MoneyMoney-style template blob.
#[allow(clippy::too_many_arguments)]
fn payment_row(
    rowid: i64,
    typ: i64,
    tombstone: i64,
    name: &str,
    iban: &str,
    bic: &str,
    amount: f64,
    currency: &str,
    purpose: &str,
) -> String {
    let cents = (amount.abs() * 100.0).round() as i64;
    let blob = templates::encode_template_blob(name, iban, bic, cents, purpose);
    let (sched, group) = if typ == 12 {
        ("NULL", 1)
    } else {
        ("1772712000", 0)
    };
    format!(
        "INSERT INTO payments (rowid, version, tombstone, type, sender, recipient, name, purpose, amount, currency, \
         category, scheduled_or_transmitted, \"transaction\", template_group, data) VALUES \
         ({rowid}, 1.0, {tombstone}, {typ}, 1, 0, '{name}', '{purpose}', {amount}, '{currency}', 0, {sched}, 0, {group}, X'{}');\n",
        hex(&blob)
    )
}

fn templates_db() -> String {
    let mut extra = String::from(
        "INSERT INTO template_groups (rowid, version, tombstone) VALUES (1, 1.0, 0);\n",
    );
    // usable: with BIC and amount / without amount and BIC
    extra += &payment_row(
        1,
        12,
        0,
        "Erika Mustermann",
        "DE89 3704 0044 0532 0130 00",
        "COBADEFFXXX",
        -720.0,
        "EUR",
        "Miete Mai",
    );
    extra += &payment_row(
        2,
        12,
        0,
        "Muster Verein e.V.",
        "DE02120300000000202051",
        "",
        0.0,
        "EUR",
        "Mitgliedsbeitrag",
    );
    // not usable: wrong checksum, foreign currency
    extra += &payment_row(
        3,
        12,
        0,
        "Tippfehler GmbH",
        "DE89370400440532013001",
        "",
        -1.0,
        "EUR",
        "x",
    );
    extra += &payment_row(
        4,
        12,
        0,
        "Muster Inc",
        "DE02120300000000202051",
        "",
        -10.0,
        "USD",
        "y",
    );
    // deleted template, sent order (type 7)
    extra += &payment_row(
        5,
        12,
        1,
        "Gelöscht AG",
        "DE02120300000000202051",
        "",
        -5.0,
        "EUR",
        "z",
    );
    extra += &payment_row(
        6,
        7,
        0,
        "Gesendet GmbH",
        "DE02120300000000202051",
        "",
        -9.0,
        "EUR",
        "Rechnung 1",
    );
    fake_db_with(false, &extra)
}

fn templates_of(v: &Vault) -> Vec<(String, String, String, String, Option<i64>, String)> {
    v.list_transfer_templates()
        .unwrap()
        .into_iter()
        .map(|t| {
            (
                t.name,
                t.recipient_name,
                t.recipient_iban,
                t.recipient_bic,
                t.default_amount_cents,
                t.default_purpose,
            )
        })
        .collect()
}

#[test]
fn transfer_templates_are_taken_over_once() {
    let path = templates_db();
    let v = vault();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    assert_eq!(p.transfer_template_count, 2);

    let r = v
        .import_moneymoney_database(path.clone(), PW.into(), options_from(&p, false))
        .unwrap();
    assert_eq!(
        (
            r.transfer_templates_created,
            r.transfer_templates_existing,
            r.transfer_templates_skipped
        ),
        (2, 0, 2)
    );
    let expected = vec![
        (
            "Erika Mustermann".to_string(),
            "Erika Mustermann".to_string(),
            "DE89370400440532013000".to_string(),
            "COBADEFFXXX".to_string(),
            Some(72_000),
            "Miete Mai".to_string(),
        ),
        (
            "Muster Verein e.V.".to_string(),
            "Muster Verein e.V.".to_string(),
            "DE02120300000000202051".to_string(),
            String::new(),
            None,
            "Mitgliedsbeitrag".to_string(),
        ),
    ];
    assert_eq!(templates_of(&v), expected);

    // second run: nothing new
    let r = v
        .import_moneymoney_database(path, PW.into(), options_from(&p, false))
        .unwrap();
    assert_eq!(
        (r.transfer_templates_created, r.transfer_templates_existing),
        (0, 2)
    );
    assert_eq!(templates_of(&v), expected);
}

#[test]
fn transfer_templates_only_when_chosen() {
    let path = templates_db();
    let v = vault();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    let options = MoneyMoneyDbImportOptions {
        import_transfer_templates: false,
        ..options_from(&p, false)
    };
    let r = v
        .import_moneymoney_database(path, PW.into(), options)
        .unwrap();
    assert_eq!(
        r.transfer_templates_created + r.transfer_templates_skipped,
        0
    );
    assert!(v.list_transfer_templates().unwrap().is_empty());
}

#[test]
fn databases_without_payments_have_no_templates() {
    let path = fake_db_with(false, "DROP TABLE payments;");
    let v = vault();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    assert_eq!(p.transfer_template_count, 0);
    let r = v
        .import_moneymoney_database(path, PW.into(), options_from(&p, false))
        .unwrap();
    assert_eq!(r.transfer_templates_created, 0);
}

#[test]
fn template_name_and_purpose_fall_back_to_the_blob() {
    let mut extra = payment_row(
        1,
        12,
        0,
        "Erika Mustermann",
        "DE89370400440532013000",
        "",
        -1.5,
        "EUR",
        "Miete",
    );
    extra += "UPDATE payments SET name = NULL, purpose = NULL;";
    let path = fake_db_with(false, &extra);
    let v = vault();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    v.import_moneymoney_database(path, PW.into(), options_from(&p, false))
        .unwrap();
    let t = &v.list_transfer_templates().unwrap()[0];
    assert_eq!(
        (
            t.recipient_name.as_str(),
            t.default_purpose.as_str(),
            t.default_amount_cents
        ),
        ("Erika Mustermann", "Miete", Some(150))
    );
}

/// Sent orders carry complete SEPA messages in `data`: their blobs must
/// never be evaluated, only those of templates.
#[test]
fn blobs_of_sent_orders_are_never_read() {
    let mut extra = String::new();
    extra += &payment_row(
        1,
        12,
        0,
        "Erika Mustermann",
        "DE89370400440532013000",
        "",
        -1.0,
        "EUR",
        "Miete",
    );
    extra += &payment_row(
        2,
        7,
        0,
        "Gesendet GmbH",
        "DE02120300000000202051",
        "",
        -9.0,
        "EUR",
        "Rechnung",
    );
    extra += r#"
ALTER TABLE payments RENAME TO payments_data;
CREATE VIEW payments AS SELECT rowid, version, uuid, tombstone, type, sender, recipient, name, purpose, amount,
  currency, category, scheduled_or_transmitted, standing_order_end, "transaction", template_group,
  CASE WHEN type = 12 THEN data ELSE json('x') END AS data FROM payments_data;
"#;
    let path = fake_db_with(false, &extra);
    // positive control: evaluating the blob of the sent order fails
    let c = open_source(&path, &SecretString::new(PW.into())).unwrap();
    assert!(
        c.query_row("SELECT data FROM payments WHERE type = 7", [], |r| r
            .get::<_, Vec<u8>>(0))
            .is_err()
    );
    drop(c);
    let v = vault();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    assert_eq!(p.transfer_template_count, 1);
    let r = v
        .import_moneymoney_database(path, PW.into(), options_from(&p, false))
        .unwrap();
    assert_eq!(r.transfer_templates_created, 1);
}

/// `portfolio_chart` of the depot (account 3): days at 12:00, starting 2026-03-07.
const VALUE_HISTORY: &str = "
CREATE TABLE portfolio_chart (account_key INTEGER, currency TEXT, timestamp INTEGER, balance REAL, securities_key INTEGER);
-- a position sold and bought again: a second row of the same ISIN
INSERT INTO securities (rowid, account_key, ident, isin, quantity, amount, deleted) VALUES
  (4, 3, 'Muster World ETF', 'IE00B4L5Y983', 0, 0, 1);
-- depot total: sold out on day 4, then zeros on
INSERT INTO portfolio_chart VALUES
  (3,'EUR',1772884800,1005,0),(3,'EUR',1772971200,1015,0),(3,'EUR',1773057600,1020,0),
  (3,'EUR',1773144000,0,0),(3,'EUR',1773230400,0,0),(3,'EUR',1773316800,0,0);
-- ETF: row 1 and the sold twin (row 4) disagree on day 2, the larger one wins
INSERT INTO portfolio_chart VALUES
  (3,'EUR',1772884800,700,1),(3,'EUR',1772971200,710,1),(3,'EUR',1773057600,720,1),
  (3,'EUR',1773144000,0,1),(3,'EUR',1773230400,0,1),
  (3,'EUR',1772884800,0,4),(3,'EUR',1772971200,800,4);
-- cash without ISIN; a 0 before its first value is no point
INSERT INTO portfolio_chart VALUES
  (3,'EUR',1772798400,0,2),(3,'EUR',1772884800,300,2),(3,'EUR',1772971200,300,2),(3,'EUR',1773057600,300,2),(3,'EUR',1773144000,0,2);
-- sold position: its closing 0 is a point, the zeros after it are not
INSERT INTO portfolio_chart VALUES
  (3,'EUR',1772884800,5,3),(3,'EUR',1772971200,5,3),(3,'EUR',1773057600,0,3),(3,'EUR',1773144000,0,3);
";

#[test]
fn value_history_of_a_depot_is_imported_as_delivered_and_once() {
    let v = vault();
    let path = fake_db_with(false, VALUE_HISTORY);
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    let depot = p.accounts.iter().find(|a| a.name == "Depot").unwrap();
    // every row: total 6 + ETF 7 + cash 5 + sold 4
    assert_eq!(depot.value_history_count, 22);
    let r = v
        .import_moneymoney_database(path.clone(), PW.into(), options_from(&p, false))
        .unwrap();
    assert_eq!(r.value_history_imported, 22);
    let id = account_by_name(&v, "Depot").id;
    let total = v.depot_value_history(vec![id], 10_000).unwrap();
    assert_eq!(total[0].date, "2026-03-07");
    assert_eq!(
        total[..4].iter().map(|p| p.value_cents).collect::<Vec<_>>(),
        [100_500, 101_500, 102_000, 0]
    );
    assert!(
        total[4..].iter().all(|p| p.value_cents == 0),
        "a sold-out depot stays at 0"
    );
    let series = |key: &str| -> Vec<i64> {
        v.position_value_history(id, key.into())
            .unwrap()
            .iter()
            .map(|p| p.value_cents)
            .collect()
    };
    assert_eq!(
        series("IE00B4L5Y983"),
        [70_000, 80_000, 72_000, 0],
        "the larger of two rows of one ISIN; the series ends with the sale"
    );
    assert_eq!(
        series("cash"),
        [30_000, 30_000, 30_000, 0],
        "no point for the 0 before the first value"
    );
    // no valid ISIN: keyed by its name
    assert_eq!(
        series("name:verkauft ag"),
        [500, 500, 0],
        "one closing zero"
    );

    // a second run adds nothing
    let r2 = v
        .import_moneymoney_database(path, PW.into(), options_from(&p, false))
        .unwrap();
    assert_eq!(r2.value_history_imported, 0);
    let n: i64 = v
        .db()
        .unwrap()
        .query_row("SELECT count(*) FROM depot_observations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 22);
}

#[test]
fn a_database_without_value_history_imports_as_before() {
    let v = vault();
    let path = fake_db();
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    assert!(p.accounts.iter().all(|a| a.value_history_count == 0));
    let r = v
        .import_moneymoney_database(path, PW.into(), options_from(&p, false))
        .unwrap();
    assert_eq!(r.value_history_imported, 0);
}

#[test]
fn portal_sync_adopts_the_active_card_next_to_closed_ones() {
    // #816: two closed cards (archived) next to the active one under one Amex
    // login; the first portal sync takes the active card over instead of adding
    // a fourth account.
    let extra = "
INSERT INTO banks (rowid, country, bankcode, username, customer, credential, name, systemid, local_storage) VALUES
  (30, 'DE', '', 'amex-login', '', 'GEHEIM-30', 'American Express', 'SYS', 'LS');
INSERT INTO accounts (rowid, bank_key, account, bankcode, portfolio, iban, currency, name, owner, kind, type, sort, initial_balance, group_key, cred) VALUES
  (30, 30, 'xxxx-11001', '', 0, '', 'EUR', 'Amex Alt 1', 'Max Mustermann', 0, 3, 30, 0, 1, 'GEHEIM'),
  (31, 30, 'xxxx-21002', '', 0, '', 'EUR', 'Amex Alt 2', 'Max Mustermann', 0, 3, 31, 0, 1, 'GEHEIM'),
  (32, 30, 'xxxx-31003', '', 0, '', 'EUR', 'Amex Aktiv', 'Max Mustermann', 0, 3, 32, 0, 1, 'GEHEIM');
";
    let v = vault();
    let path = fake_db_with(false, extra);
    let p = v
        .preview_moneymoney_database(path.clone(), PW.into(), None)
        .unwrap();
    v.import_moneymoney_database(path, PW.into(), options_from(&p, true))
        .unwrap();
    // `cancel_tsp` says nothing about a closed account (#998): the user archives them
    for name in ["Amex Alt 1", "Amex Alt 2"] {
        v.set_account_archived(account_by_name(&v, name).id, true)
            .unwrap();
    }
    let active = account_by_name(&v, "Amex Aktiv");
    let conn = active.connection_id.unwrap();
    let before = v.list_accounts().unwrap().len();
    let body = r#"{"summary":{"total_balance":{"amount":"50,00"}},"transactions":[{"post_date":"2026-11-01","billing_amount":"12,34","description":"Shop","reference_number":"R1"}]}"#;
    v.clone()
        .import_portal_capture(conn, vec![body.to_string()], "[]".into())
        .unwrap();
    assert_eq!(v.list_accounts().unwrap().len(), before);
    assert_eq!(bookings(&v, active.id).len(), 1);
}

#[test]
fn external_refs_never_change() {
    // Imported accounts are matched again by this ref on the next import.
    assert_eq!(fnv("12345678|0532013000"), "663cb359");
}

#[test]
fn adopt_account_reuses_imported_trade_republic_checking_account_without_iban() {
    let _p_dir = crate::test_support::TempDir::new("girocrab-tr-1114");
    let v = Vault::open(
        _p_dir.join("v.db").to_string_lossy().to_string(),
        vec![3u8; 32],
    )
    .unwrap();
    let conn_id = v
        .add_bank_connection(
            ConnectorKind::TradeRepublic,
            String::new(),
            "+491701234567".into(),
        )
        .unwrap();
    {
        let db = v.db().unwrap();
        let gid = Vault::ensure_group(&db, "Girokonten", "banknote", 1).unwrap();
        db.execute(
            "INSERT INTO accounts(connection_id, group_id, external_ref, name, bank_name, iban, currency, kind, balance_cents, balance_date, sf_symbol, sort_order, created_ms)
             VALUES (?1, ?2, 'moneymoney-db:tr-cash-1', 'Verrechnungskonto', 'Trade Republic', 'DE02100123450000000001', 'EUR', 'checking', 12345, '2026-11-18', 'banknote', 1, 1000)",
            params![conn_id, gid],
        )
        .unwrap();
        let adopted =
            super::adopt_account(&db, conn_id, "tr:cash", "", "EUR", AccountKind::Checking)
                .unwrap();
        assert!(
            adopted.is_some(),
            "imported TR checking account must be adopted"
        );
        let ext: String = db
            .query_row(
                "SELECT external_ref FROM accounts WHERE id = ?1",
                params![adopted.unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ext, "tr:cash");
    }
}
