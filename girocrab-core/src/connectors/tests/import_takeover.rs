//! A direct connector takes over accounts a MoneyMoney import parked on a
//! PSD2 connection that never synced, instead of adding a second set.
//! Invented data only.
use rusqlite::params;

use crate::connectors::*;
use crate::models::{AccountKind, ConnectorKind};
use crate::vault::Vault;

const CARD_IBAN: &str = "DE00000000001000000001";
const SAVINGS_IBAN: &str = "DE00000000001000000002";

fn remote(r: &str, iban: &str, kind: AccountKind) -> RemoteAccount {
    RemoteAccount {
        external_ref: r.into(),
        name: format!("Konto {r}"),
        owner: String::new(),
        iban: iban.into(),
        account_number: String::new(),
        currency: "EUR".into(),
        kind,
        balance_cents: -1000,
        balance_date: "2026-11-12".into(),
    }
}

fn vault(name: &str) -> (std::sync::Arc<Vault>, std::path::PathBuf) {
    let dir = crate::test_support::TempDir::new(name);
    let path = dir.join("v.db");
    let v = Vault::open(path.to_string_lossy().to_string(), vec![7u8; 32]).unwrap();
    v.db()
        .unwrap()
        .execute(
            "INSERT OR IGNORE INTO account_groups(name) VALUES ('Test')",
            [],
        )
        .unwrap();
    (v, path)
}

fn psd2(v: &Vault) -> i64 {
    v.add_bank_connection(
        ConnectorKind::EnableBanking,
        "enablebanking:DE:Test Bank".into(),
        String::new(),
    )
    .unwrap()
}

/// An imported account (`moneymoney-db:` key) on `connection`.
fn imported(v: &Vault, connection: Option<i64>, key: &str, iban: &str, kind: &str) -> i64 {
    let db = v.db().unwrap();
    db.execute(
        "INSERT INTO accounts(connection_id, group_id, external_ref, name, iban, currency, kind)
         VALUES (?1, (SELECT id FROM account_groups LIMIT 1), ?2, 'Imported', ?3, 'EUR', ?4)",
        params![connection, key, iban, kind],
    )
    .unwrap();
    db.last_insert_rowid()
}

fn live(v: &Vault) -> Vec<(i64, i64, String)> {
    let db = v.db().unwrap();
    let mut q = db
        .prepare(
            "SELECT id, IFNULL(connection_id, -1), name FROM accounts
              WHERE deleted_ms IS NULL ORDER BY id",
        )
        .unwrap();
    q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

/// The account selection does not hold such an account back as "already
/// reachable elsewhere", and the first sync takes both over (the card was
/// imported as a checking account).
#[test]
fn the_first_sync_takes_over_imported_accounts_of_an_unsynced_psd2_connection() {
    let (v, path) = vault("girocrab-takeover-first");
    let p = psd2(&v);
    let card = imported(
        &v,
        Some(p),
        "moneymoney-db:1",
        "DE00 0000 0000 1000 0000 01",
        "checking",
    );
    let savings = imported(&v, Some(p), "moneymoney-db:2", SAVINGS_IBAN, "savings");
    let conn = v
        .add_bank_connection(ConnectorKind::FordMoney, String::new(), "login".into())
        .unwrap();
    let fetch = FetchResult {
        accounts: vec![
            remote("fordmoney:1", CARD_IBAN, AccountKind::CreditCard),
            remote("fordmoney:2", SAVINGS_IBAN, AccountKind::Savings),
        ],
        ..Default::default()
    };
    store_fetch(&v, ConnectorKind::FordMoney, conn, fetch).unwrap();
    assert_eq!(
        live(&v),
        vec![
            (card, conn, "Imported".to_string()),
            (savings, conn, "Imported".to_string())
        ]
    );
    drop(v);
    std::fs::remove_file(path).ok();
}

/// Once the PSD2 connection synced, its accounts are its own: the new
/// connection's account is held back for the user's choice instead.
#[test]
fn accounts_of_a_synced_psd2_connection_are_not_taken_over() {
    let (v, path) = vault("girocrab-takeover-synced");
    let p = psd2(&v);
    let savings = imported(&v, Some(p), "moneymoney-db:2", SAVINGS_IBAN, "savings");
    v.db()
        .unwrap()
        .execute(
            "UPDATE bank_connections SET last_sync_unix = 1 WHERE id = ?1",
            [p],
        )
        .unwrap();
    let conn = v
        .add_bank_connection(ConnectorKind::FordMoney, String::new(), "login".into())
        .unwrap();
    let fetch = FetchResult {
        accounts: vec![remote("fordmoney:2", SAVINGS_IBAN, AccountKind::Savings)],
        ..Default::default()
    };
    store_fetch(&v, ConnectorKind::FordMoney, conn, fetch).unwrap();
    assert_eq!(live(&v), vec![(savings, p, "Imported".to_string())]);
    drop(v);
    std::fs::remove_file(path).ok();
}

/// A duplicate the user deleted (it stays deleted) gives way to the imported
/// account, which then gets the bookings.
#[test]
fn a_deleted_duplicate_gives_way_to_the_imported_account() {
    let (v, path) = vault("girocrab-takeover-deleted");
    let p = psd2(&v);
    let card = imported(&v, Some(p), "moneymoney-db:1", CARD_IBAN, "credit_card");
    let conn = v
        .add_bank_connection(ConnectorKind::FordMoney, String::new(), "login".into())
        .unwrap();
    // A second account keeps the connection alive when the duplicate goes.
    let other = imported(&v, None, "other", "", "savings");
    let dup = {
        let db = v.db().unwrap();
        db.execute("UPDATE bank_connections SET created_ms = 1", [])
            .unwrap();
        db.execute(
            "UPDATE accounts SET connection_id = ?1, external_ref = 'fordmoney:other' WHERE id = ?2",
            params![conn, other],
        )
        .unwrap();
        db.execute(
            "INSERT INTO accounts(connection_id, group_id, external_ref, name, iban, currency, kind)
             VALUES (?1, (SELECT id FROM account_groups LIMIT 1), 'fordmoney:1', 'Duplicate', ?2, 'EUR', 'credit_card')",
            params![conn, CARD_IBAN],
        )
        .unwrap();
        db.last_insert_rowid()
    };
    v.set_account_archived(dup, true).unwrap();
    v.delete_account(dup).unwrap();

    let fetch = FetchResult {
        accounts: vec![remote("fordmoney:1", CARD_IBAN, AccountKind::CreditCard)],
        transactions: vec![RemoteTransaction {
            account_ref: "fordmoney:1".into(),
            external_id: "tx-1".into(),
            booking_date: "2026-11-10".into(),
            value_date: "2026-11-10".into(),
            amount_cents: -950,
            currency: "EUR".into(),
            counterparty: "TEST SHOP".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    store_fetch(&v, ConnectorKind::FordMoney, conn, fetch).unwrap();
    let db = v.db().unwrap();
    let (cid, key): (i64, String) = db
        .query_row(
            "SELECT connection_id, external_ref FROM accounts WHERE id = ?1 AND deleted_ms IS NULL",
            [card],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((cid, key.as_str()), (conn, "fordmoney:1"));
    let bookings: i64 = db
        .query_row(
            "SELECT count(*) FROM transactions WHERE account_id = ?1 AND deleted_ms IS NULL",
            [card],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(bookings, 1);
    let (dup_key, dup_deleted): (String, bool) = db
        .query_row(
            "SELECT external_ref, deleted_ms IS NOT NULL FROM accounts WHERE id = ?1",
            [dup],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(dup_deleted);
    assert!(dup_key.starts_with("fordmoney:1#deleted-"), "{dup_key}");
    drop(db);
    drop(v);
    std::fs::remove_file(path).ok();
}

/// The real sequence behind a revived duplicate: the import's PSD2 connection
/// was deleted (its accounts stay), the first sync added a duplicate, the
/// user deleted it (and with it the connection) and added the connection
/// again. Its sync takes the imported account over instead of reviving the
/// duplicate.
#[test]
fn a_connection_added_again_takes_over_instead_of_reviving_the_duplicate() {
    let (v, path) = vault("girocrab-takeover-again");
    let p = psd2(&v);
    let savings = imported(&v, Some(p), "moneymoney-db:2", SAVINGS_IBAN, "savings");
    let conn = v
        .add_bank_connection(ConnectorKind::FordMoney, String::new(), "login".into())
        .unwrap();
    let dup = {
        let db = v.db().unwrap();
        db.execute(
            "INSERT INTO accounts(connection_id, group_id, external_ref, name, iban, currency, kind)
             VALUES (?1, (SELECT id FROM account_groups LIMIT 1), 'fordmoney:2', 'Duplicate', ?2, 'EUR', 'savings')",
            params![conn, SAVINGS_IBAN],
        )
        .unwrap();
        let dup = db.last_insert_rowid();
        db.execute(
            "UPDATE bank_connections SET deleted_ms = 1 WHERE id = ?1",
            [p],
        )
        .unwrap();
        dup
    };
    v.set_account_archived(dup, true).unwrap();
    v.delete_account(dup).unwrap();
    // Deleted long before the connection is added again (no wall-clock race).
    v.db()
        .unwrap()
        .execute("UPDATE accounts SET deleted_ms = 1 WHERE id = ?1", [dup])
        .unwrap();
    let again = v
        .add_bank_connection(ConnectorKind::FordMoney, String::new(), "login".into())
        .unwrap();
    let fetch = FetchResult {
        accounts: vec![remote("fordmoney:2", SAVINGS_IBAN, AccountKind::Savings)],
        ..Default::default()
    };
    store_fetch(&v, ConnectorKind::FordMoney, again, fetch).unwrap();
    assert_eq!(live(&v), vec![(savings, again, "Imported".to_string())]);
    drop(v);
    std::fs::remove_file(path).ok();
}

/// Without the same IBAN nothing is taken over, so the account stays held back
/// for the user's choice (no second account): here the imported account has
/// no IBAN and is matched by bank code and account number only.
#[test]
fn an_imported_account_without_the_same_iban_is_still_held_back() {
    let (v, path) = vault("girocrab-takeover-noiban");
    let p = psd2(&v);
    let savings = imported(&v, Some(p), "moneymoney-db:2", "", "savings");
    let conn = v
        .add_bank_connection(ConnectorKind::FordMoney, String::new(), "login".into())
        .unwrap();
    {
        let db = v.db().unwrap();
        db.execute(
            "UPDATE bank_connections SET bank_code = '10000000' WHERE id IN (?1, ?2)",
            params![p, conn],
        )
        .unwrap();
        db.execute(
            "UPDATE accounts SET account_number = '1000000002' WHERE id = ?1",
            [savings],
        )
        .unwrap();
    }
    let mut acc = remote("fordmoney:2", SAVINGS_IBAN, AccountKind::Savings);
    acc.account_number = "1000000002".into();
    let fetch = FetchResult {
        accounts: vec![acc],
        ..Default::default()
    };
    store_fetch(&v, ConnectorKind::FordMoney, conn, fetch).unwrap();
    assert_eq!(live(&v), vec![(savings, p, "Imported".to_string())]);
    drop(v);
    std::fs::remove_file(path).ok();
}

/// A duplicate the user deleted next to an imported account outside the PSD2
/// case (here: without a connection) stays deleted; the imported account is
/// left as it is.
#[test]
fn a_deleted_duplicate_stays_deleted_outside_the_psd2_case() {
    let (v, path) = vault("girocrab-takeover-unlinked");
    let unlinked = imported(&v, None, "moneymoney-db:1", CARD_IBAN, "credit_card");
    let conn = v
        .add_bank_connection(ConnectorKind::FordMoney, String::new(), "login".into())
        .unwrap();
    let other = imported(&v, None, "other", "", "savings");
    let dup = {
        let db = v.db().unwrap();
        db.execute("UPDATE bank_connections SET created_ms = 1", [])
            .unwrap();
        db.execute(
            "UPDATE accounts SET connection_id = ?1, external_ref = 'fordmoney:other' WHERE id = ?2",
            params![conn, other],
        )
        .unwrap();
        db.execute(
            "INSERT INTO accounts(connection_id, group_id, external_ref, name, iban, currency, kind)
             VALUES (?1, (SELECT id FROM account_groups LIMIT 1), 'fordmoney:1', 'Duplicate', ?2, 'EUR', 'credit_card')",
            params![conn, CARD_IBAN],
        )
        .unwrap();
        db.last_insert_rowid()
    };
    v.set_account_archived(dup, true).unwrap();
    v.delete_account(dup).unwrap();
    let fetch = FetchResult {
        accounts: vec![remote("fordmoney:1", CARD_IBAN, AccountKind::CreditCard)],
        ..Default::default()
    };
    store_fetch(&v, ConnectorKind::FordMoney, conn, fetch).unwrap();
    let db = v.db().unwrap();
    let (key, deleted): (String, bool) = db
        .query_row(
            "SELECT external_ref, deleted_ms IS NOT NULL FROM accounts WHERE id = ?1",
            [dup],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((key.as_str(), deleted), ("fordmoney:1", true));
    let linked: Option<i64> = db
        .query_row(
            "SELECT connection_id FROM accounts WHERE id = ?1",
            [unlinked],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(linked, None);
    drop(db);
    drop(v);
    std::fs::remove_file(path).ok();
}
