use std::sync::Arc;

use crate::connectors::*;
use crate::models::{AccountKind, ConnectorKind};
use crate::vault::Vault;

fn tx(date: &str, cents: i64, who: &str) -> RemoteTransaction {
    RemoteTransaction {
        account_ref: "ACC".into(),
        booking_date: date.into(),
        value_date: date.into(),
        amount_cents: cents,
        currency: "EUR".into(),
        counterparty: who.into(),
        purpose: "Einkauf".into(),
        ..Default::default()
    }
}

#[test]
fn content_ids_carry_the_prefix() {
    let id = tx("2026-11-01", -350, "Bäckerei").content_id(0);
    assert!(is_content_id(&id, CONTENT_ID_PREFIX), "{id}");
}

#[test]
fn ids_count_per_content_not_per_position() {
    let coffee = tx("2026-11-05", -350, "Café");
    let rent = tx("2026-11-01", -90_000, "Vermieter");
    let ids = |list: &[&RemoteTransaction]| {
        let mut c = ContentIds::default();
        list.iter().map(|t| c.next(t)).collect::<Vec<_>>()
    };
    // two identical coffees on one day stay two ids
    let a = ids(&[&coffee, &coffee, &rent]);
    assert_ne!(a[0], a[1]);
    // a moved window / new booking in front changes nothing
    let new = tx("2026-11-10", -1_000, "Kiosk");
    let b = ids(&[&new, &rent, &coffee, &coffee]);
    assert_eq!((&b[2], &b[3], &b[1]), (&a[0], &a[1], &a[2]));
}

#[test]
fn since_is_per_account_with_a_common_floor() {
    let d = |s: &str| jiff::civil::Date::strptime("%Y-%m-%d", s).unwrap();
    let newest = vec![
        ("A".to_string(), Some("2026-11-01".to_string())),
        ("B".to_string(), Some("2026-10-01".to_string())), // lagging behind
        ("C".to_string(), None),                           // no bookings (depot)
    ];
    let (since, per) = since_window(d("2026-11-12"), 90, &newest);
    assert_eq!(per["A"], "2026-10-22");
    assert_eq!(per["B"], "2026-09-21");
    assert_eq!(per["C"], "2026-08-14");
    // connectors fetching everything at once start at the earliest account
    assert_eq!(since, "2026-09-21");
    // whole connection not synced for long: older than `days` is kept
    let (since, per) = since_window(
        d("2026-11-12"),
        90,
        &[("A".to_string(), Some("2026-01-01".to_string()))],
    );
    assert_eq!(
        (since.as_str(), per["A"].as_str()),
        ("2025-12-22", "2025-12-22")
    );
    let (since, _) = since_window(d("2026-11-12"), 90, &[]);
    assert_eq!(since, "2026-08-14");
}

fn setup() -> (Arc<Vault>, i64) {
    let p_dir = crate::test_support::TempDir::new("girocrab-cid");
    let p = p_dir.join("v.db");
    let v = Vault::open(p.to_string_lossy().to_string(), vec![5u8; 32]).unwrap();
    let conn = v
        .add_bank_connection(ConnectorKind::Comdirect, String::new(), "login".into())
        .unwrap();
    (v, conn)
}

fn fetch(txs: Vec<RemoteTransaction>) -> FetchResult {
    FetchResult {
        accounts: vec![RemoteAccount {
            external_ref: "ACC".into(),
            name: "Giro".into(),
            owner: String::new(),
            iban: String::new(),
            account_number: String::new(),
            currency: "EUR".into(),
            kind: AccountKind::Checking,
            balance_cents: 0,
            balance_date: "2026-11-10".into(),
        }],
        transactions: txs,
        ..Default::default()
    }
}

fn ext_ids(v: &Vault) -> Vec<String> {
    let db = v.db().unwrap();
    let mut st = db
        .prepare("SELECT external_id FROM transactions ORDER BY id")
        .unwrap();
    st.query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn legacy_content_ids_are_adopted_not_duplicated() {
    let (v, conn) = setup();
    let coffee = tx("2026-11-05", -350, "Café");
    // stored by an older version: two identical coffees, DefaultHasher ids
    let mut old = vec![coffee.clone(), coffee.clone()];
    old[0].external_id = "h0123456789abcdef".into();
    old[1].external_id = "hfedcba9876543210".into();
    store_fetch(&v, ConnectorKind::Comdirect, conn, fetch(old)).unwrap();
    // same bookings with the new FNV ids
    let mut ids = ContentIds::default();
    let mut new = vec![coffee.clone(), coffee.clone()];
    for t in &mut new {
        t.external_id = ids.next(t);
    }
    let s = store_fetch(&v, ConnectorKind::Comdirect, conn, fetch(new.clone())).unwrap();
    assert_eq!(s.transactions_imported, 0);
    let stored = ext_ids(&v);
    assert_eq!(stored.len(), 2);
    assert!(stored.contains(&new[0].external_id) && stored.contains(&new[1].external_id));
    // and a third, genuinely new identical coffee is still imported
    let mut ids = ContentIds::default();
    let mut three = vec![coffee.clone(), coffee.clone(), coffee];
    for t in &mut three {
        t.external_id = ids.next(t);
    }
    let s = store_fetch(&v, ConnectorKind::Comdirect, conn, fetch(three)).unwrap();
    assert_eq!(s.transactions_imported, 1);
    assert_eq!(ext_ids(&v).len(), 3);
}

#[test]
fn deleted_legacy_booking_stays_deleted_when_adopted() {
    let (v, conn) = setup();
    let mut old = tx("2026-11-05", -350, "Café");
    old.external_id = "h0123456789abcdef".into();
    store_fetch(&v, ConnectorKind::Comdirect, conn, fetch(vec![old.clone()])).unwrap();
    v.db()
        .unwrap()
        .execute("UPDATE transactions SET deleted_ms = 5", [])
        .unwrap();
    let mut new = old;
    new.external_id = ContentIds::default().next(&new);
    let s = store_fetch(&v, ConnectorKind::Comdirect, conn, fetch(vec![new.clone()])).unwrap();
    assert_eq!(s.transactions_imported, 0);
    assert_eq!(ext_ids(&v), [new.external_id]);
    let deleted: Option<i64> = v
        .db()
        .unwrap()
        .query_row("SELECT deleted_ms FROM transactions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(deleted, Some(5));
}

#[test]
fn empty_external_ids_do_not_collapse() {
    let (v, conn) = setup();
    let coffee = tx("2026-11-05", -350, "Café");
    let txs = vec![coffee.clone(), coffee, tx("2026-11-06", -500, "Kiosk")];
    store_fetch(&v, ConnectorKind::Comdirect, conn, fetch(txs.clone())).unwrap();
    let ids = ext_ids(&v);
    assert_eq!(ids.len(), 3);
    assert!(ids.iter().all(|i| is_content_id(i, CONTENT_ID_PREFIX)));
    // deterministic: the next sync adds nothing
    let s = store_fetch(&v, ConnectorKind::Comdirect, conn, fetch(txs)).unwrap();
    assert_eq!(s.transactions_imported, 0);
}

#[test]
fn refetch_fills_empty_field_groups_and_heals_text() {
    let (v, conn) = setup();
    let row = || -> [String; 8] {
        v.db()
            .unwrap()
            .query_row(
                "SELECT counterparty, purpose, purpose_code, gvc, end_to_end_id, mandate_id, creditor_id, item_refs
                   FROM transactions",
                [],
                |r| Ok(std::array::from_fn(|i| r.get(i).unwrap())),
            )
            .unwrap()
    };
    let mut first = tx("2026-11-09", -90_000, "Caf\u{FFFD} Nord");
    first.external_id = "t1".into();
    first.gvc = "116".into();
    store_fetch(
        &v,
        ConnectorKind::Comdirect,
        conn,
        fetch(vec![first.clone()]),
    )
    .unwrap();
    // no text from the bank: the corrupted name stays rather than going blank
    let blank = RemoteTransaction {
        counterparty: String::new(),
        ..first.clone()
    };
    store_fetch(&v, ConnectorKind::Comdirect, conn, fetch(vec![blank])).unwrap();
    assert_eq!(row()[0], "Caf\u{FFFD} Nord");
    let again = RemoteTransaction {
        counterparty: "Café Nord".into(),
        purpose_code: "RINP".into(),
        gvc: "117".into(),
        end_to_end_id: "E2E-1".into(),
        mandate_id: "M-1".into(),
        creditor_id: "C-1".into(),
        item_refs: "A1".into(),
        ..first
    };
    let s = store_fetch(&v, ConnectorKind::Comdirect, conn, fetch(vec![again])).unwrap();
    assert_eq!(s.transactions_imported, 0);
    // healed and filled; the codes are a pair the bank sent together, so a
    // half-stored pair is left alone
    assert_eq!(
        row(),
        [
            "Café Nord",
            "Einkauf",
            "",
            "116",
            "E2E-1",
            "M-1",
            "C-1",
            "A1"
        ]
    );
}

#[test]
fn refetch_names_a_stored_booking_without_counterparty() {
    // comdirect card payments were stored without counterparty before #1135
    let (v, conn) = setup();
    let counterparty = || -> String {
        v.db()
            .unwrap()
            .query_row("SELECT counterparty FROM transactions", [], |r| r.get(0))
            .unwrap()
    };
    let mut first = tx("2026-11-14", -2_340, "");
    first.external_id = "t1".into();
    store_fetch(
        &v,
        ConnectorKind::Comdirect,
        conn,
        fetch(vec![first.clone()]),
    )
    .unwrap();
    let named = RemoteTransaction {
        counterparty: "Beispielladen GmbH".into(),
        ..first
    };
    let s = store_fetch(
        &v,
        ConnectorKind::Comdirect,
        conn,
        fetch(vec![named.clone()]),
    )
    .unwrap();
    assert_eq!(s.transactions_imported, 0);
    assert_eq!(counterparty(), "Beispielladen GmbH");
    // a stored name is never replaced by another one
    let other = RemoteTransaction {
        counterparty: "Anderer Name".into(),
        ..named
    };
    store_fetch(&v, ConnectorKind::Comdirect, conn, fetch(vec![other])).unwrap();
    assert_eq!(counterparty(), "Beispielladen GmbH");
}

/// A booking imported from a MoneyMoney export into the fetched account.
fn imported_row(v: &Vault, ext: &str, t: &RemoteTransaction, note: &str) {
    v.db()
        .unwrap()
        .execute(
            "INSERT INTO transactions(account_id, external_id, booking_date, value_date, amount_cents,
                                      counterparty, purpose, note)
             SELECT id, ?1, ?2, ?2, ?3, ?4, ?5, ?6 FROM accounts",
            rusqlite::params![ext, t.booking_date, t.amount_cents, t.counterparty, t.purpose, note],
        )
        .unwrap();
}

/// `(external_id, note)` of the live bookings.
fn live(v: &Vault) -> Vec<(String, String)> {
    let db = v.db().unwrap();
    let mut st = db
        .prepare("SELECT external_id, note FROM transactions WHERE deleted_ms IS NULL ORDER BY id")
        .unwrap();
    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn new_booking_adopts_its_imported_twin_but_a_hold_does_not() {
    let (v, conn) = setup();
    store_fetch(&v, ConnectorKind::Comdirect, conn, fetch(vec![])).unwrap();
    let mut coffee = tx("2026-11-05", -350, "Café");
    coffee.external_id = "t1".into();
    let mut hold = tx("2026-11-08", -1_200, "Kiosk");
    hold.external_id = "pending-hold-portal-1".into();
    imported_row(&v, "mm-1", &coffee, "Frühstück");
    imported_row(&v, "mm-2", &hold, "");
    // the same booking twice in one delivery is stored once
    let s = store_fetch(
        &v,
        ConnectorKind::Comdirect,
        conn,
        fetch(vec![coffee.clone(), hold.clone(), coffee]),
    )
    .unwrap();
    assert_eq!(s.transactions_imported, 1);
    assert_eq!(
        live(&v),
        [
            ("t1".into(), "Frühstück".into()),
            ("mm-2".into(), String::new()),
            ("pending-hold-portal-1".into(), String::new()),
        ]
    );
}

#[test]
fn refetch_merges_an_untouched_duplicate_into_its_imported_twin() {
    let (v, conn) = setup();
    let mut coffee = tx("2026-11-05", -350, "Café");
    coffee.external_id = "t1".into();
    let mut rent = tx("2026-11-01", -90_000, "Vermieter");
    rent.external_id = "t2".into();
    let mut hold = tx("2026-11-08", -1_200, "Kiosk");
    hold.external_id = "pending-hold-portal-1".into();
    let all = vec![coffee.clone(), rent.clone(), hold.clone()];
    // stored before the export was imported: the bank rows exist next to their twins
    store_fetch(&v, ConnectorKind::Comdirect, conn, fetch(all.clone())).unwrap();
    imported_row(&v, "mm-1", &coffee, "Frühstück");
    imported_row(&v, "mm-2", &rent, "");
    imported_row(&v, "mm-3", &hold, "");
    v.db()
        .unwrap()
        .execute(
            "UPDATE transactions SET note = 'meins' WHERE external_id = 't2'",
            [],
        )
        .unwrap();
    let s = store_fetch(&v, ConnectorKind::Comdirect, conn, fetch(all)).unwrap();
    assert_eq!(s.transactions_imported, 0);
    // the untouched duplicate gives way to its twin; the edited one and the hold stay
    assert_eq!(
        live(&v),
        [
            ("t2".into(), "meins".into()),
            ("pending-hold-portal-1".into(), String::new()),
            ("t1".into(), "Frühstück".into()),
            ("mm-2".into(), String::new()),
            ("mm-3".into(), String::new()),
        ]
    );
}

#[test]
fn bank_dates_are_stored_as_real_days() {
    let (v, conn) = setup();
    let with = |booking: &str, value: &str| RemoteTransaction {
        booking_date: booking.into(),
        value_date: value.into(),
        ..tx("", -100, booking)
    };
    let txs = vec![
        with("2026-11-05T10:00:00", "2026-11-04"),
        with("2026-02-30", "2026-02-27"),
        with("../x", "2026-13-01"),
    ];
    store_fetch(&v, ConnectorKind::Comdirect, conn, fetch(txs.clone())).unwrap();
    let rows: Vec<(String, String)> = {
        let db = v.db().unwrap();
        let mut st = db
            .prepare("SELECT booking_date, value_date FROM transactions ORDER BY id")
            .unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let row = |b: &str, v: &str| (b.to_string(), v.to_string());
    assert_eq!(
        rows,
        vec![
            row("2026-11-05", "2026-11-04"),
            row("2026-02-27", "2026-02-27"),
            row(&crate::dates::today_iso(), ""),
        ]
    );
    // the content ids come from what the bank sent, so the next sync adds nothing
    let s = store_fetch(&v, ConnectorKind::Comdirect, conn, fetch(txs)).unwrap();
    assert_eq!(s.transactions_imported, 0);
}
