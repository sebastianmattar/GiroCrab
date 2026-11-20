use std::sync::Arc;

use rusqlite::params;

use crate::error::CoreError;
use crate::vault::Vault;

fn vault(tag: &str) -> Arc<Vault> {
    Vault::open(
        crate::test_support::thread_temp_path(tag)
            .to_string_lossy()
            .into(),
        vec![7u8; 32],
    )
    .unwrap()
}

/// An imported account (no connection) with `(date, cents)` bookings.
fn imported(v: &Vault, ext: &str, bookings: &[(&str, i64)]) -> i64 {
    let db = v.db().unwrap();
    db.execute(
        "INSERT INTO account_groups(name, sf_symbol, sort_order) VALUES ('G', 'tray', 1)
         ON CONFLICT DO NOTHING",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO accounts(group_id, external_ref, name, kind)
         VALUES ((SELECT min(id) FROM account_groups), ?1, 'Altkonto', 'checking')",
        [ext],
    )
    .unwrap();
    let id = db.last_insert_rowid();
    drop(db);
    for (n, (d, c)) in bookings.iter().enumerate() {
        add(v, id, &format!("b{n}"), d, *c, "EUR");
    }
    id
}

fn add(v: &Vault, account: i64, ext: &str, date: &str, cents: i64, cur: &str) {
    v.db()
        .unwrap()
        .execute(
            "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, currency)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![account, ext, date, cents, cur],
        )
        .unwrap();
}

fn account(v: &Vault, id: i64) -> crate::models::Account {
    v.list_accounts()
        .unwrap()
        .into_iter()
        .find(|a| a.id == id)
        .unwrap()
}

#[test]
fn opening_balance_plus_bookings_from_its_date() {
    let v = vault("ob-basic");
    let id = imported(
        &v,
        "A",
        &[
            ("2026-01-15", -1000),
            ("2026-02-01", 500),
            ("2026-03-01", -200),
        ],
    );
    let a = account(&v, id);
    assert!(a.balance_derived);
    assert_eq!(a.opening_balance_cents, None);

    // no date: every booking counts
    v.set_account_opening_balance(id, Some(10_000), String::new())
        .unwrap();
    let a = account(&v, id);
    assert_eq!(a.balance_cents, 10_000 - 1000 + 500 - 200);
    assert_eq!(a.balance_date, "2026-03-01");

    // start of 2026-02-01: the booking on that day counts, the earlier one not
    v.set_account_opening_balance(id, Some(10_000), " 2026-02-01 ".into())
        .unwrap();
    let a = account(&v, id);
    assert_eq!(a.opening_balance_cents, Some(10_000));
    assert_eq!(a.opening_balance_date, "2026-02-01");
    assert_eq!(a.balance_cents, 10_000 + 500 - 200);

    // deleted bookings and foreign-currency bookings don't count
    add(&v, id, "usd", "2026-03-02", 99_999, "USD");
    add(&v, id, "gone", "2026-03-02", 77_777, "EUR");
    v.db()
        .unwrap()
        .execute(
            "UPDATE transactions SET deleted_ms = 1 WHERE external_id = 'gone'",
            [],
        )
        .unwrap();
    // an import ends with the rule run, which refreshes the balance
    add(&v, id, "new", "2026-03-05", -300, "EUR");
    Vault::apply_rules_inner(&v.db().unwrap(), true).unwrap();
    let a = account(&v, id);
    assert_eq!(a.balance_cents, 10_000 + 500 - 200 - 300);
    assert_eq!(a.balance_date, "2026-03-05");
}

#[test]
fn preview_counts_without_storing() {
    let v = vault("ob-preview");
    let id = imported(
        &v,
        "A",
        &[
            ("2026-01-15", -1000),
            ("2026-02-01", 500),
            ("2026-03-01", -200),
        ],
    );
    let p = v
        .preview_account_opening_balance(id, "2026-02-01".into())
        .unwrap();
    assert_eq!(
        p,
        super::OpeningBalancePreview {
            sum_cents: 300,
            counted: 2,
            older: 1
        }
    );
    let all = v
        .preview_account_opening_balance(id, String::new())
        .unwrap();
    assert_eq!((all.sum_cents, all.counted, all.older), (-700, 3, 0));
    assert_eq!(account(&v, id).opening_balance_cents, None);
    assert!(
        v.preview_account_opening_balance(id, "1.2.2026".into())
            .is_err()
    );
}

#[test]
fn calibrate_hits_the_target_and_keeps_the_date() {
    let v = vault("ob-cal");
    let id = imported(&v, "A", &[("2020-05-01", -4000), ("2026-02-01", 1500)]);
    // without an opening balance: from the first booking
    assert_eq!(
        v.calibrate_account_opening_balance(id, 50_000).unwrap(),
        50_000 + 4000 - 1500
    );
    assert_eq!(account(&v, id).balance_cents, 50_000);

    v.set_account_opening_balance(id, Some(0), "2026-01-01".into())
        .unwrap();
    assert_eq!(account(&v, id).balance_cents, 1500);
    assert_eq!(
        v.calibrate_account_opening_balance(id, 12_345).unwrap(),
        12_345 - 1500
    );
    let a = account(&v, id);
    assert_eq!(a.balance_cents, 12_345);
    assert_eq!(a.opening_balance_date, "2026-01-01");
}

#[test]
fn cleared_opening_balance_falls_back_to_the_booking_sum() {
    let v = vault("ob-clear");
    let id = imported(&v, "A", &[("2026-01-15", -1000)]);
    v.set_account_opening_balance(id, Some(5000), String::new())
        .unwrap();
    v.set_account_opening_balance(id, None, String::new())
        .unwrap();
    let a = account(&v, id);
    assert_eq!(a.opening_balance_cents, None);
    // back to the pure sum of the bookings (5000 - 1000 before)
    assert_eq!(a.balance_cents, -1000);
    // a later rule run leaves it alone
    Vault::apply_rules_inner(&v.db().unwrap(), true).unwrap();
    assert_eq!(account(&v, id).balance_cents, -1000);
}

#[test]
fn bank_balances_depots_and_bad_dates_are_refused() {
    let v = vault("ob-bank");
    let id = imported(&v, "A", &[("2026-01-15", -1000)]);
    let db = v.db().unwrap();
    db.execute(
        "INSERT INTO bank_connections(display_name, connector) VALUES ('Bank', 'comdirect')",
        [],
    )
    .unwrap();
    let c = db.last_insert_rowid();
    db.execute(
        "UPDATE accounts SET connection_id = ?1, balance_cents = 777 WHERE id = ?2",
        params![c, id],
    )
    .unwrap();
    drop(db);
    assert!(!account(&v, id).balance_derived);
    let bank = v.set_account_opening_balance(id, Some(1), String::new());
    assert!(
        matches!(bank, Err(CoreError::InvalidInput { .. })),
        "{bank:?}"
    );
    assert!(v.calibrate_account_opening_balance(id, 1).is_err());
    assert_eq!(account(&v, id).balance_cents, 777);

    // a connector without balances (adjust_balances) marks the account derived
    v.db()
        .unwrap()
        .execute(
            "UPDATE accounts SET balance_derived = 1 WHERE id = ?1",
            [id],
        )
        .unwrap();
    v.set_account_opening_balance(id, Some(2000), String::new())
        .unwrap();
    assert_eq!(account(&v, id).balance_cents, 1000);

    let depot = imported(&v, "D", &[]);
    v.db()
        .unwrap()
        .execute(
            "UPDATE accounts SET kind = 'brokerage' WHERE id = ?1",
            [depot],
        )
        .unwrap();
    assert!(!account(&v, depot).balance_derived);
    assert!(
        v.set_account_opening_balance(depot, Some(1), String::new())
            .is_err()
    );

    let bad = v.set_account_opening_balance(id, Some(1), "31.12.2025".into());
    assert!(
        matches!(bad, Err(CoreError::InvalidInput { .. })),
        "{bad:?}"
    );
    assert!(matches!(
        v.set_account_opening_balance(9999, Some(1), String::new()),
        Err(CoreError::NotFound { .. })
    ));
}

#[test]
fn merge_takes_the_newer_opening_balance() {
    let a = vault("ob-merge-a");
    let b = vault("ob-merge-b");
    let ia = imported(&a, "A", &[("2026-01-15", -1000)]);
    let ib = imported(&b, "A", &[("2026-01-15", -1000)]);
    b.set_account_opening_balance(ib, Some(9000), "2026-01-01".into())
        .unwrap();
    a.merge_other_vault(b.file_path(), vec![7u8; 32]).unwrap();
    let acc = account(&a, ia);
    assert_eq!(
        (acc.opening_balance_cents, acc.opening_balance_date.as_str()),
        (Some(9000), "2026-01-01")
    );
    assert_eq!(acc.balance_cents, 8000);

    // an older edit on the other side does not win, a newer clear does
    a.set_account_opening_balance(ia, Some(100), String::new())
        .unwrap();
    b.db()
        .unwrap()
        .execute(
            "UPDATE opening_balances SET opening_cents = 5, updated_ms = 1",
            [],
        )
        .unwrap();
    a.merge_other_vault(b.file_path(), vec![7u8; 32]).unwrap();
    assert_eq!(account(&a, ia).opening_balance_cents, Some(100));
    b.db()
        .unwrap()
        .execute(
            "UPDATE opening_balances SET opening_cents = NULL, updated_ms = ?1",
            [crate::dates::now_ms() + 1000],
        )
        .unwrap();
    a.merge_other_vault(b.file_path(), vec![7u8; 32]).unwrap();
    let cleared = account(&a, ia);
    assert_eq!(cleared.opening_balance_cents, None);
    // a removal arriving by merge also falls back to the booking sum
    assert_eq!(cleared.balance_cents, -1000);
}

#[test]
fn simulated_bank_sync_does_not_mark_accounts_derived() {
    use crate::connectors::{FetchResult, RemoteAccount, store_fetch};
    use crate::models::ConnectorKind;
    let v = vault("ob-simulated");
    let c = v
        .add_bank_connection(ConnectorKind::Comdirect, String::new(), "login".into())
        .unwrap();
    let fetch = |simulated: bool| FetchResult {
        accounts: vec![RemoteAccount {
            external_ref: "DE1".into(),
            name: "Demo".into(),
            owner: String::new(),
            iban: String::new(),
            account_number: String::new(),
            currency: "EUR".into(),
            kind: crate::models::AccountKind::Checking,
            balance_cents: 0,
            balance_date: "2026-11-10".into(),
        }],
        adjust_balances: true,
        simulated,
        ..Default::default()
    };
    store_fetch(&v, ConnectorKind::Comdirect, c, fetch(true)).unwrap();
    assert!(!v.list_accounts().unwrap()[0].balance_derived);
    // a real connector without balances still marks it
    store_fetch(&v, ConnectorKind::Comdirect, c, fetch(false)).unwrap();
    assert!(v.list_accounts().unwrap()[0].balance_derived);
}
