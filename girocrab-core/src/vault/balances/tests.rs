use std::collections::HashMap;
use std::sync::Arc;

use rusqlite::params;

use crate::models::{BookingFilter, BookingPage};
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

/// A checking account; `bank` = with a connection, so its balance is the
/// bank's (`balance_cents` as of `balance_date`), else computed.
fn account(v: &Vault, name: &str, bank: Option<(i64, &str)>) -> i64 {
    let db = v.db().unwrap();
    db.execute(
        "INSERT INTO account_groups(name, sf_symbol, sort_order) VALUES ('G', 'tray', 1)
         ON CONFLICT DO NOTHING",
        [],
    )
    .unwrap();
    let connection = bank.map(|_| {
        db.execute(
            "INSERT INTO bank_connections(connector, display_name) VALUES ('fints', 'Bank')",
            [],
        )
        .unwrap();
        db.last_insert_rowid()
    });
    let (cents, date) = bank.unwrap_or((0, ""));
    db.execute(
        "INSERT INTO accounts(connection_id, group_id, external_ref, name, kind, balance_cents, balance_date)
         VALUES (?1, (SELECT min(id) FROM account_groups), ?2, ?2, 'checking', ?3, ?4)",
        params![connection, name, cents, date],
    )
    .unwrap();
    db.last_insert_rowid()
}

/// Adds a booking; returns its id.
fn add(v: &Vault, account: i64, ext: &str, date: &str, cents: i64) -> i64 {
    let db = v.db().unwrap();
    db.execute(
        "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, currency, counterparty)
         VALUES (?1, ?2, ?3, ?4, 'EUR', ?2)",
        params![account, ext, date, cents],
    )
    .unwrap();
    db.last_insert_rowid()
}

fn page(v: &Vault, filter: BookingFilter) -> BookingPage {
    v.list_transactions(BookingFilter {
        with_balances: true,
        ..filter
    })
    .unwrap()
}

fn after(p: &BookingPage) -> HashMap<i64, i64> {
    p.running_balances
        .iter()
        .map(|b| (b.booking_id, b.balance_cents))
        .collect()
}

fn days(p: &BookingPage) -> Vec<(String, i64)> {
    p.day_balances
        .iter()
        .map(|d| (d.date.clone(), d.balance_cents))
        .collect()
}

#[test]
fn bank_balance_walks_back_and_adds_bookings_after_its_date() {
    let v = vault("rb-bank");
    // the bank reported 1000.00 at the end of 2026-03-02
    let a = account(&v, "Giro", Some((100_000, "2026-03-02")));
    let b1 = add(&v, a, "b1", "2026-03-01", -2_000);
    let b2 = add(&v, a, "b2", "2026-03-01", 5_000);
    let b3 = add(&v, a, "b3", "2026-03-02", -1_000);
    let b4 = add(&v, a, "b4", "2026-03-04", -500);
    let p4 = add(&v, a, "pending-1", "2026-03-05", -9_999);

    let p = page(
        &v,
        BookingFilter {
            account_ids: vec![a],
            ..Default::default()
        },
    );
    let m = after(&p);
    assert_eq!(m[&b4], 99_500);
    assert_eq!(m[&b3], 100_000);
    assert_eq!(m[&b2], 101_000);
    assert_eq!(m[&b1], 96_000);
    assert!(!m.contains_key(&p4), "pending bookings have no balance");
    assert_eq!(
        days(&p),
        vec![
            ("2026-03-05".into(), 99_500),
            ("2026-03-04".into(), 99_500),
            ("2026-03-02".into(), 100_000),
            ("2026-03-01".into(), 101_000),
        ]
    );
    // without the flag nothing is computed
    let plain = v
        .list_transactions(BookingFilter {
            account_ids: vec![a],
            ..Default::default()
        })
        .unwrap();
    assert!(plain.running_balances.is_empty() && plain.day_balances.is_empty());
}

#[test]
fn filters_and_pages_keep_the_balance_of_the_whole_account() {
    let v = vault("rb-filter");
    let a = account(&v, "Giro", Some((10_000, "2026-04-30")));
    let mut ids = Vec::new();
    for n in 0..12 {
        let date = format!("2026-04-{:02}", n + 1);
        ids.push(add(&v, a, &format!("x{n}"), &date, 100 * (n + 1)));
    }
    let all = after(&page(
        &v,
        BookingFilter {
            account_ids: vec![a],
            ..Default::default()
        },
    ));
    // a search that hides most rows: same balances as the full list
    let found = page(
        &v,
        BookingFilter {
            account_ids: vec![a],
            search: Some("x1".into()),
            ..Default::default()
        },
    );
    assert!(!found.running_balances.is_empty());
    for b in &found.running_balances {
        assert_eq!(all[&b.booking_id], b.balance_cents);
    }
    // a later page starts where the first one ended
    let second = page(
        &v,
        BookingFilter {
            account_ids: vec![a],
            limit: 5,
            offset: 5,
            ..Default::default()
        },
    );
    assert_eq!(second.running_balances.len(), 5);
    for b in &second.running_balances {
        assert_eq!(all[&b.booking_id], b.balance_cents);
    }
}

#[test]
fn several_accounts_sum_and_skip_other_accounts_rows() {
    let v = vault("rb-sum");
    let a = account(&v, "Giro", Some((10_000, "2026-05-31")));
    let b = account(&v, "Tagesgeld", Some((50_000, "2026-05-31")));
    let c = account(&v, "Andere", Some((7_000, "2026-05-31")));
    let a1 = add(&v, a, "a1", "2026-05-01", -1_000);
    let b1 = add(&v, b, "b1", "2026-05-02", 2_000);
    let c1 = add(&v, c, "c1", "2026-05-03", 300);
    let p = page(
        &v,
        BookingFilter {
            account_ids: vec![a, b],
            ..Default::default()
        },
    );
    let m = after(&p);
    assert_eq!(m[&b1], 60_000);
    assert_eq!(m[&a1], 58_000);
    assert!(!m.contains_key(&c1));

    // all accounts: every counted account
    let m = after(&page(&v, BookingFilter::default()));
    assert_eq!(m[&c1], 67_000);
    assert_eq!(m[&b1], 66_700);
    assert_eq!(m[&a1], 64_700);
}

#[test]
fn computed_balance_counts_pending_and_stops_at_the_opening_date() {
    let v = vault("rb-opening");
    let a = account(&v, "Import", None);
    let old = add(&v, a, "o1", "2026-01-10", -4_000);
    let b1 = add(&v, a, "b1", "2026-02-01", -1_000);
    let b2 = add(&v, a, "b2", "2026-02-03", 300);
    let pend = add(&v, a, "pending-1", "2026-02-04", -50);
    v.set_account_opening_balance(a, Some(20_000), "2026-02-01".into())
        .unwrap();
    let p = page(
        &v,
        BookingFilter {
            account_ids: vec![a],
            ..Default::default()
        },
    );
    let m = after(&p);
    // balance = 20_000 - 1_000 + 300 - 50 (pending counts in a computed balance)
    assert!(!m.contains_key(&pend));
    assert_eq!(m[&b2], 19_300);
    assert_eq!(m[&b1], 19_000);
    assert!(!m.contains_key(&old), "before the opening date: unknown");
    assert_eq!(
        days(&p),
        vec![
            ("2026-02-04".into(), 19_250),
            ("2026-02-03".into(), 19_300),
            ("2026-02-01".into(), 19_000),
        ]
    );
}

#[test]
fn header_chart_agrees_with_the_list_on_a_computed_balance() {
    let v = vault("rb-chart");
    let a = account(&v, "Import", None);
    let today = crate::dates::today();
    let day = |back: i64| crate::dates::iso(today - crate::dates::days(back));
    add(&v, a, "b1", &day(3), -1_000);
    add(&v, a, "b2", &day(2), 400);
    add(&v, a, "pending-1", &day(0), -50);
    v.set_account_opening_balance(a, Some(10_000), String::new())
        .unwrap();
    let list: HashMap<String, i64> = days(&page(
        &v,
        BookingFilter {
            account_ids: vec![a],
            ..Default::default()
        },
    ))
    .into_iter()
    .collect();
    let chart: HashMap<String, i64> = v
        .balance_history(vec![a], 5)
        .unwrap()
        .into_iter()
        .map(|p| (p.date, p.balance_cents))
        .collect();
    assert_eq!(list.len(), 3);
    for (date, cents) in &list {
        assert_eq!(chart[date], *cents, "{date}");
    }
    assert_eq!(chart[&day(4)], 10_000);
}

#[test]
fn legacy_advanzia_hold_never_changes_a_reported_balance_or_running_history() {
    let v = vault("rb-advanzia-legacy-hold");
    let a = account(&v, "Fictional card", Some((-10_000, "2026-03-02")));
    let posted = add(&v, a, "advanzia-post-fictional", "2026-03-02", -1_000);
    let held = add(&v, a, "advanzia-hold-fictional", "2026-03-03", -9_999);
    let p = page(
        &v,
        BookingFilter {
            account_ids: vec![a],
            ..Default::default()
        },
    );
    assert_eq!(after(&p)[&posted], -10_000);
    assert!(
        !after(&p).contains_key(&held),
        "pending bookings have no running balance"
    );
    assert!(
        p.transactions
            .iter()
            .find(|b| b.id == held)
            .unwrap()
            .is_pending
    );
    let db = v.db().unwrap();
    assert_eq!(super::booked_after(&db, a, "2026-03-02").unwrap(), 0);
    assert!(
        super::booked_per_day(&db, &[a], "2026-03-02", None)
            .unwrap()
            .is_empty()
    );
}
