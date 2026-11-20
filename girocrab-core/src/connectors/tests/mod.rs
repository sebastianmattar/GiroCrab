use std::sync::Arc;

use rusqlite::params;

use super::*;
use crate::error::CoreError;
use crate::models::{AccountKind, BookingFilter, BookingScope, ConnectorKind, SepaTransferRequest};
use crate::vault::Vault;

#[test]
fn errors_after_submit_leave_outcome_unknown() {
    use super::{CoreError, error_leaves_outcome_unknown as unknown};
    assert!(unknown(&CoreError::Network {
        message: "x".into()
    }));
    assert!(unknown(&CoreError::Protocol {
        message: "x".into()
    }));
    assert!(!unknown(&CoreError::Authentication {
        message: "x".into()
    }));
    assert!(!unknown(&CoreError::InvalidInput {
        message: "x".into()
    }));
}

#[test]
fn store_fetch_matches_depot_holdings_without_iban() {
    let p_dir = crate::test_support::TempDir::new("girocrab-depot");
    let p = p_dir.join("v.db");
    let v = Vault::open(p.to_string_lossy().to_string(), vec![3u8; 32]).unwrap();
    let conn = v
        .add_bank_connection(ConnectorKind::Comdirect, String::new(), "login".into())
        .unwrap();
    let mk = |r: &str| RemoteHolding {
        classification: Default::default(),
        direct_crypto: false,

        account_ref: r.into(),
        holding: crate::models::Holding {
            isin: "DE0001234567".into(),
            name: "X".into(),
            quantity: 2.0,
            value_cents: 100,
            ..Default::default()
        },
    };
    let fetch = |hold_ref: &str| FetchResult {
        accounts: vec![RemoteAccount {
            external_ref: "0012345678".into(),
            name: "Depot".into(),
            owner: String::new(),
            iban: String::new(),
            account_number: "0012345678".into(),
            currency: "EUR".into(),
            kind: AccountKind::Brokerage,
            balance_cents: 5000,
            balance_date: "2026-11-10".into(),
        }],
        holdings: vec![mk(hold_ref)],
        ..Default::default()
    };
    // differently formatted ref (no leading zeros) and an unknown ref (single-depot fallback)
    for r in ["12345678", "unknown"] {
        store_fetch(&v, ConnectorKind::Comdirect, conn, fetch(r)).unwrap();
        let acc = v.list_accounts().unwrap().pop().unwrap();
        assert_eq!(v.list_holdings(acc.id).unwrap().len(), 1, "ref {r}");
    }
}

#[test]
fn unread_depot_keeps_its_positions_and_value() {
    // A sync that could not read the depot (Trade Republic: portfolio request failed)
    // still lists the account, with a placeholder balance of 0.
    let p_dir = crate::test_support::TempDir::new("girocrab-depot-unread");
    let p = p_dir.join("v.db");
    let v = Vault::open(p.to_string_lossy().to_string(), vec![3u8; 32]).unwrap();
    let conn = v
        .add_bank_connection(ConnectorKind::TradeRepublic, String::new(), "login".into())
        .unwrap();
    let depot = |balance_cents: i64| RemoteAccount {
        external_ref: "tr:depot".into(),
        name: "Depot".into(),
        owner: String::new(),
        iban: String::new(),
        account_number: String::new(),
        currency: "EUR".into(),
        kind: AccountKind::Brokerage,
        balance_cents,
        balance_date: "2026-11-10".into(),
    };
    let holding = RemoteHolding {
        account_ref: "tr:depot".into(),
        holding: crate::models::Holding {
            isin: "DE0001234567".into(),
            name: "X".into(),
            quantity: 2.0,
            value_cents: 5000,
            ..Default::default()
        },
        ..Default::default()
    };
    let read = FetchResult {
        accounts: vec![depot(5000)],
        holdings: vec![holding],
        holdings_fetched: vec!["tr:depot".into()],
        ..Default::default()
    };
    store_fetch(&v, ConnectorKind::TradeRepublic, conn, read).unwrap();

    let unread = FetchResult {
        accounts: vec![depot(0)],
        balances_unknown: vec!["tr:depot".into()],
        ..Default::default()
    };
    store_fetch(&v, ConnectorKind::TradeRepublic, conn, unread).unwrap();
    let acc = v.list_accounts().unwrap().pop().unwrap();
    assert_eq!(acc.balance_cents, 5000);
    assert_eq!(v.list_holdings(acc.id).unwrap().len(), 1);

    // read and empty: everything was sold
    let sold = FetchResult {
        accounts: vec![depot(0)],
        holdings_fetched: vec!["tr:depot".into()],
        ..Default::default()
    };
    store_fetch(&v, ConnectorKind::TradeRepublic, conn, sold).unwrap();
    let acc = v.list_accounts().unwrap().pop().unwrap();
    assert_eq!(acc.balance_cents, 0);
    assert!(v.list_holdings(acc.id).unwrap().is_empty());
}

#[test]
fn fetch_warnings_name_accounts_by_their_stored_name() {
    // #1083: a FinTS failure names the account by its IBAN; the summary uses
    // the name the account carries here (renamed by the user), never the IBAN.
    let p_dir = crate::test_support::TempDir::new("girocrab-fetch-warnings");
    let p = p_dir.join("v.db");
    let v = Vault::open(p.to_string_lossy().to_string(), vec![3u8; 32]).unwrap();
    let conn = v
        .add_bank_connection(
            ConnectorKind::FinTsGeneric,
            "fints:50010517".into(),
            "login".into(),
        )
        .unwrap();
    let iban = "DE02120300000000202051";
    let account = |balance_cents: i64| RemoteAccount {
        external_ref: iban.into(),
        name: "Verrechnungskonto".into(),
        owner: String::new(),
        iban: iban.into(),
        account_number: String::new(),
        currency: "EUR".into(),
        kind: AccountKind::Checking,
        balance_cents,
        balance_date: "2026-11-10".into(),
    };
    let first = FetchResult {
        accounts: vec![account(250_00)],
        ..Default::default()
    };
    let s = store_fetch(&v, ConnectorKind::FinTsGeneric, conn, first).unwrap();
    assert!(s.warnings.is_empty());
    let id = v.list_accounts().unwrap().pop().unwrap().id;
    v.rename_account(id, "Cash".into()).unwrap();

    let failed = FetchResult {
        accounts: vec![account(0)],
        balances_unknown: vec![iban.into()],
        warnings: vec![
            format!("{iban}: Umsätze (HKCAZ/camt.052) – TAN verlangt"),
            "Kontenliste (HKSPA) – abgelehnt".into(),
        ],
        ..Default::default()
    };
    let s = store_fetch(&v, ConnectorKind::FinTsGeneric, conn, failed).unwrap();
    assert_eq!(
        s.warnings,
        vec![
            "Cash: Umsätze (HKCAZ/camt.052) – TAN verlangt".to_string(),
            "Kontenliste (HKSPA) – abgelehnt".to_string(),
        ]
    );
    // the balance that was not read stays
    assert_eq!(
        v.list_accounts().unwrap().pop().unwrap().balance_cents,
        250_00
    );
}

#[test]
fn depot_value_points_follow_what_a_fetch_confirms() {
    // total without positions, unknown balance, unconfirmed 0, confirmed 0
    let p_dir = crate::test_support::TempDir::new("girocrab-depot-points");
    let p = p_dir.join("v.db");
    let v = Vault::open(p.to_string_lossy().to_string(), vec![3u8; 32]).unwrap();
    let conn = v
        .add_bank_connection(ConnectorKind::TradeRepublic, String::new(), "login".into())
        .unwrap();
    let depot = |balance_cents: i64, date: &str| RemoteAccount {
        external_ref: "tr:depot".into(),
        name: "Depot".into(),
        owner: String::new(),
        iban: String::new(),
        account_number: String::new(),
        currency: "EUR".into(),
        kind: AccountKind::Brokerage,
        balance_cents,
        balance_date: date.into(),
    };
    let points = |v: &Vault| -> Vec<(String, i64)> {
        let db = v.db().unwrap();
        let mut q = db
            .prepare("SELECT value_date, value_cents FROM depot_values ORDER BY value_date")
            .unwrap();
        q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let fetch = |accounts, unknown: bool, fetched: bool| FetchResult {
        accounts,
        balances_unknown: if unknown {
            vec!["tr:depot".into()]
        } else {
            vec![]
        },
        holdings_fetched: if fetched {
            vec!["tr:depot".into()]
        } else {
            vec![]
        },
        ..Default::default()
    };

    // 1. a total without positions (an overview import) is a value
    let f = fetch(vec![depot(7_000_00, "2026-11-01")], false, false);
    store_fetch(&v, ConnectorKind::TradeRepublic, conn, f).unwrap();
    assert_eq!(points(&v), vec![("2026-11-01".to_string(), 7_000_00)]);

    // 2. a balance that was not read leaves no point
    let f = fetch(vec![depot(0, "2026-11-02")], true, false);
    store_fetch(&v, ConnectorKind::TradeRepublic, conn, f).unwrap();
    assert_eq!(points(&v).len(), 1);

    // 3. a 0 without a confirmed inventory may only mean "not reported": no point
    let f = fetch(vec![depot(0, "2026-11-03")], false, false);
    store_fetch(&v, ConnectorKind::TradeRepublic, conn, f).unwrap();
    assert_eq!(points(&v).len(), 1);

    // 4. an inventory that was read and is empty: the 0 is confirmed
    let f = fetch(vec![depot(0, "2026-11-04")], false, true);
    store_fetch(&v, ConnectorKind::TradeRepublic, conn, f).unwrap();
    assert_eq!(
        points(&v),
        vec![
            ("2026-11-01".to_string(), 7_000_00),
            ("2026-11-04".to_string(), 0)
        ]
    );
}

#[test]
fn archived_psd2_account_is_not_reported_as_revoked() {
    // #65 (archive) + #66 (revoked marking): an account the user archived
    // is skipped by store_fetch, so it must not look "missing" from a
    // complete PSD2 sync.
    let p_dir = crate::test_support::TempDir::new("girocrab-arch-eb");
    let p = p_dir.join("v.db");
    let v = Vault::open(p.to_string_lossy().to_string(), vec![3u8; 32]).unwrap();
    let conn = v
        .add_bank_connection(ConnectorKind::EnableBanking, String::new(), "C24".into())
        .unwrap();
    let acc = |r: &str, name: &str| RemoteAccount {
        external_ref: r.into(),
        name: name.into(),
        owner: String::new(),
        iban: String::new(),
        account_number: String::new(),
        currency: "EUR".into(),
        kind: AccountKind::Checking,
        balance_cents: 100,
        balance_date: "2026-11-11".into(),
    };
    let both = || vec![acc("G", "C24 Smartkonto"), acc("P", "C24 Pocket")];
    store_fetch(
        &v,
        ConnectorKind::EnableBanking,
        conn,
        FetchResult {
            accounts: both(),
            accounts_complete: true,
            ..Default::default()
        },
    )
    .unwrap();
    let pocket = v
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|a| a.name == "C24 Pocket")
        .unwrap();
    v.set_account_archived(pocket.id, true).unwrap();
    // the bank still delivers both
    let s = store_fetch(
        &v,
        ConnectorKind::EnableBanking,
        conn,
        FetchResult {
            accounts: both(),
            accounts_complete: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(s.revoked_accounts.is_empty(), "{:?}", s.revoked_accounts);
    // the bank no longer delivers the archived one: still no notice
    let s = store_fetch(
        &v,
        ConnectorKind::EnableBanking,
        conn,
        FetchResult {
            accounts: vec![acc("G", "C24 Smartkonto")],
            accounts_complete: true,
            revoked_refs: vec!["P".into()],
            ..Default::default()
        },
    )
    .unwrap();
    assert!(s.revoked_accounts.is_empty(), "{:?}", s.revoked_accounts);
    let st = v.connection_status(conn).unwrap();
    assert_eq!(st.revoked_account_count, 0);
    std::fs::remove_file(p).ok();
}

#[test]
fn store_fetch_skips_archived_accounts() {
    let p_dir = crate::test_support::TempDir::new("girocrab-arch");
    let p = p_dir.join("v.db");
    let v = Vault::open(p.to_string_lossy().to_string(), vec![3u8; 32]).unwrap();
    let conn = v
        .add_bank_connection(ConnectorKind::Comdirect, String::new(), "login".into())
        .unwrap();
    let acc = |r: &str, name: &str, balance: i64| RemoteAccount {
        external_ref: r.into(),
        name: name.into(),
        owner: String::new(),
        iban: String::new(),
        account_number: r.into(),
        currency: "EUR".into(),
        kind: AccountKind::Checking,
        balance_cents: balance,
        balance_date: "2026-11-10".into(),
    };
    let tx = |r: &str, id: &str| RemoteTransaction {
        account_ref: r.into(),
        external_id: id.into(),
        booking_date: "2026-11-09".into(),
        value_date: "2026-11-09".into(),
        amount_cents: -500,
        currency: "EUR".into(),
        counterparty: "Laden".into(),
        counterparty_iban: String::new(),
        purpose: String::new(),
        booking_text: String::new(),
        ..Default::default()
    };
    let fetch = |balance: i64, ids: &[&str]| FetchResult {
        accounts: vec![
            acc("alt", "Altes Konto", balance),
            acc("neu", "Neues Konto", balance),
        ],
        transactions: ids
            .iter()
            .flat_map(|id| [tx("alt", id), tx("neu", id)])
            .collect(),
        ..Default::default()
    };
    let s = store_fetch(&v, ConnectorKind::Comdirect, conn, fetch(1000, &["t1"])).unwrap();
    assert_eq!(
        (s.transactions_imported, s.transactions_skipped_archived),
        (2, 0)
    );
    let old = v
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|a| a.name == "Altes Konto")
        .unwrap();
    v.set_account_archived(old.id, true).unwrap();
    assert!(!v.list_bank_connections().unwrap()[0].all_accounts_archived);

    // t1 is known, t2 is new: only the active account takes it
    let s = store_fetch(
        &v,
        ConnectorKind::Comdirect,
        conn,
        fetch(2000, &["t1", "t2"]),
    )
    .unwrap();
    assert_eq!(
        (
            s.accounts_updated,
            s.transactions_imported,
            s.transactions_skipped_archived
        ),
        (1, 1, 1)
    );
    let accs = v.list_accounts().unwrap();
    assert_eq!(accs.len(), 2, "no second account for the archived one");
    let by = |n: &str| accs.iter().find(|a| a.name == n).unwrap();
    assert_eq!(
        (
            by("Altes Konto").balance_cents,
            by("Altes Konto").transaction_count
        ),
        (1000, 1)
    );
    assert_eq!(
        (
            by("Neues Konto").balance_cents,
            by("Neues Konto").transaction_count
        ),
        (2000, 2)
    );

    v.set_account_archived(by("Neues Konto").id, true).unwrap();
    assert!(v.list_bank_connections().unwrap()[0].all_accounts_archived);
    let _ = std::fs::remove_file(p);
}

#[test]
fn card_open_balance_since_settlement() {
    let r = |d: &str, a: i64, t: &str| (d.to_string(), a, t.to_string());
    let rows = vec![
        r("2026-09-20", -5000, "Shop"),
        r("2026-10-05", 5000, "LASTSCHRIFT Kartenabrechnung"),
        r("2026-10-10", -1200, "Cafe"),
        r("2026-11-02", -3400, "Hotel"),
    ];
    assert_eq!(open_card_balance(&rows), Some(-4600));
    let no_settle = vec![
        r("2026-10-30", -100, "x"),
        r("2026-11-01", -200, "y"),
        r("2026-11-03", -300, "z"),
    ];
    assert_eq!(open_card_balance(&no_settle), Some(-600));
    let with_refund = vec![
        r("2026-10-20", -170224, "Shops"),
        r("2026-11-02", 33853, "Refund Merchant"),
        r("2026-11-05", -1000, "Cafe"),
    ];
    assert_eq!(open_card_balance(&with_refund), Some(-137371));
    // Amex: the statement paid by direct debit, nothing charged since
    let paid = vec![
        r("2026-10-20", -12345, "Shop"),
        r(
            "2026-11-12",
            12345,
            "Zahlung/Überweisung Erhalten Besten Dank",
        ),
    ];
    assert_eq!(open_card_balance(&paid), Some(0));
    assert_eq!(open_card_balance(&[]), None);
}

fn demo_vault() -> (Arc<Vault>, std::path::PathBuf) {
    let path_dir = crate::test_support::TempDir::new("girocrab-sync");
    let path = path_dir.join("v.db");
    let v = Vault::open(path.to_string_lossy().to_string(), vec![3u8; 32]).unwrap();
    v.seed_demo_data().unwrap();
    (v, path)
}

#[test]
fn card_holds_are_replaced_on_each_sync() {
    let (v, path) = demo_vault();
    let conn = v
        .add_bank_connection(ConnectorKind::Bunq, String::new(), "me@example.com".into())
        .unwrap();
    let acc = RemoteAccount {
        external_ref: "bunq:1".into(),
        name: "bunq".into(),
        owner: String::new(),
        iban: "NL00BUNQ0000000000".into(),
        account_number: String::new(),
        currency: "EUR".into(),
        kind: AccountKind::Checking,
        balance_cents: 0,
        balance_date: "2026-11-10".into(),
    };
    let hold = |id: &str, cents: i64| RemoteTransaction {
        account_ref: "bunq:1".into(),
        external_id: format!("pending-hold-bunq-{id}"),
        booking_date: "2026-11-09".into(),
        value_date: "2026-11-09".into(),
        amount_cents: cents,
        currency: "EUR".into(),
        counterparty: "Hotel".into(),
        counterparty_iban: String::new(),
        purpose: String::new(),
        booking_text: "Kartenzahlung (vorgemerkt)".into(),
        ..Default::default()
    };
    let fetch = |txs: Vec<RemoteTransaction>| FetchResult {
        accounts: vec![acc.clone()],
        transactions: txs,
        hold_prefix: Some("pending-hold-bunq-".into()),
        ..Default::default()
    };
    let holds = || -> Vec<String> {
        let db = v.db().unwrap();
        let mut q = db
            .prepare("SELECT external_id FROM transactions WHERE external_id LIKE 'pending-hold-%' AND deleted_ms IS NULL ORDER BY 1")
            .unwrap();
        q.query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    store_fetch(
        &v,
        ConnectorKind::Bunq,
        conn,
        fetch(vec![hold("1", -50_00), hold("2", -7_10)]),
    )
    .unwrap();
    assert_eq!(holds(), vec!["pending-hold-bunq-1", "pending-hold-bunq-2"]);
    // Hold 1 cleared (booked elsewhere), hold 2 still open.
    store_fetch(&v, ConnectorKind::Bunq, conn, fetch(vec![hold("2", -7_10)])).unwrap();
    assert_eq!(holds(), vec!["pending-hold-bunq-2"]);
    std::fs::remove_file(path).ok();
}

#[test]
fn pending_batch_booking_is_replaced_by_the_banks_booking_whatever_its_iban() {
    let (v, path) = demo_vault();
    let conn = v
        .add_bank_connection(ConnectorKind::FinTsGeneric, String::new(), "u".into())
        .unwrap();
    let acc = RemoteAccount {
        external_ref: "f:1".into(),
        name: "Giro".into(),
        owner: String::new(),
        iban: "DE02120300000000202051".into(),
        account_number: String::new(),
        currency: "EUR".into(),
        kind: AccountKind::Checking,
        balance_cents: 0,
        balance_date: "2026-11-10".into(),
    };
    let fetch = |txs: Vec<RemoteTransaction>| FetchResult {
        accounts: vec![acc.clone()],
        transactions: txs,
        ..Default::default()
    };
    store_fetch(&v, ConnectorKind::FinTsGeneric, conn, fetch(vec![])).unwrap();
    let account_id: i64 = v
        .db()
        .unwrap()
        .query_row(
            "SELECT id FROM accounts WHERE external_ref = 'f:1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let summary = |e2e: &str, cents: i64| TransferJob {
        source_iban: acc.iban.clone(),
        source_owner: "A".into(),
        recipient_name: "Sammelüberweisung (3 Zahlungen)".into(),
        recipient_iban: String::new(),
        recipient_bic: None,
        amount_cents: cents,
        purpose: String::new(),
        instant: false,
        execution_date: None,
        end_to_end_id: e2e.into(),
    };
    v.record_pending_transfer(
        account_id,
        &summary("A", 3_550),
        "Sammelüberweisung (vorgemerkt)",
    )
    .unwrap();
    v.record_pending_transfer(
        account_id,
        &summary("B", 9_900),
        "Sammelüberweisung (vorgemerkt)",
    )
    .unwrap();
    let pending = || -> i64 {
        v.db()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM transactions WHERE external_id LIKE 'pending-%' AND deleted_ms IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert_eq!(pending(), 2);
    // A single payment of the same size is no batch booking: both stay pending.
    let today = crate::dates::today().to_string();
    let other = RemoteTransaction {
        account_ref: "f:1".into(),
        external_id: "bank-0".into(),
        booking_date: today.clone(),
        value_date: today,
        amount_cents: -3_550,
        currency: "EUR".into(),
        counterparty: "Shop".into(),
        counterparty_iban: "DE89370400440532013000".into(),
        booking_text: "Kartenzahlung".into(),
        ..Default::default()
    };
    store_fetch(&v, ConnectorKind::FinTsGeneric, conn, fetch(vec![other])).unwrap();
    assert_eq!(pending(), 2);
    // The bank books 35,50 € and names the own IBAN as counterparty.
    let booked = RemoteTransaction {
        account_ref: "f:1".into(),
        external_id: "bank-1".into(),
        booking_date: crate::dates::today().to_string(),
        value_date: crate::dates::today().to_string(),
        amount_cents: -3_550,
        currency: "EUR".into(),
        counterparty: "A".into(),
        counterparty_iban: acc.iban.clone(),
        booking_text: "SAMMELÜBERWEISUNG".into(),
        ..Default::default()
    };
    store_fetch(&v, ConnectorKind::FinTsGeneric, conn, fetch(vec![booked])).unwrap();
    // Only the matching one is gone; the other batch is still pending.
    assert_eq!(pending(), 1);
    std::fs::remove_file(path).ok();
}

#[test]
fn standing_orders_are_replaced_only_for_completely_fetched_accounts() {
    let (v, path) = demo_vault();
    let conn = v
        .add_bank_connection(ConnectorKind::FinTsGeneric, String::new(), "u".into())
        .unwrap();
    let acc = RemoteAccount {
        external_ref: "f:1".into(),
        name: "Giro".into(),
        owner: String::new(),
        iban: "DE02120300000000202051".into(),
        account_number: String::new(),
        currency: "EUR".into(),
        kind: AccountKind::Checking,
        balance_cents: 0,
        balance_date: "2026-11-10".into(),
    };
    let order = RemoteStandingOrder {
        account_ref: "f:1".into(),
        order_id: "O1".into(),
        recipient_name: "Vermieter".into(),
        recipient_iban: "DE89370400440532013000".into(),
        amount_cents: 85_000,
        purpose: "Miete".into(),
        first_date: "2026-01-01".into(),
        last_date: None,
        time_unit: "M".into(),
        interval: 1,
        exec_day: 1,
        ..Default::default()
    };
    let count = || -> i64 {
        v.db()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM standing_orders WHERE key = 'O1'",
                [],
                |r| r.get(0),
            )
            .unwrap()
    };
    let fetch = |orders: Vec<RemoteStandingOrder>, complete: bool| FetchResult {
        accounts: vec![acc.clone()],
        standing_orders: orders,
        standing_orders_fetched: if complete { vec!["f:1".into()] } else { vec![] },
        ..Default::default()
    };
    store_fetch(
        &v,
        ConnectorKind::FinTsGeneric,
        conn,
        fetch(vec![order], true),
    )
    .unwrap();
    assert_eq!(count(), 1);
    // A sync without the standing-order request (bank lacks it, or it failed) keeps them.
    store_fetch(&v, ConnectorKind::FinTsGeneric, conn, fetch(vec![], false)).unwrap();
    assert_eq!(count(), 1);
    // A complete answer without orders means: the user deleted them at the bank.
    store_fetch(&v, ConnectorKind::FinTsGeneric, conn, fetch(vec![], true)).unwrap();
    assert_eq!(count(), 0);
    std::fs::remove_file(path).ok();
}

#[test]
fn revoked_accounts_are_marked_and_new_ones_named() {
    let (v, path) = demo_vault();
    let conn = v
        .add_bank_connection(ConnectorKind::EnableBanking, String::new(), "C24".into())
        .unwrap();
    let acc = |r: &str, name: &str| RemoteAccount {
        external_ref: r.into(),
        name: name.into(),
        owner: String::new(),
        iban: String::new(),
        account_number: String::new(),
        currency: "EUR".into(),
        kind: AccountKind::Checking,
        balance_cents: 100_00,
        balance_date: "2026-11-11".into(),
    };
    let revoked = || -> Vec<(String, bool)> {
        let db = v.db().unwrap();
        let mut q = db
            .prepare("SELECT external_ref, access_revoked FROM accounts WHERE connection_id = ?1 ORDER BY external_ref")
            .unwrap();
        q.query_map([conn], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? != 0)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    // first sync: nothing is "new"
    let s = store_fetch(
        &v,
        ConnectorKind::EnableBanking,
        conn,
        FetchResult {
            accounts: vec![
                acc("A", "Girokonto"),
                acc("B", "Pocket"),
                acc("C", "Gemeinschaftskonto"),
            ],
            ..Default::default()
        },
    )
    .unwrap();
    assert!(s.new_accounts.is_empty() && s.revoked_accounts.is_empty());
    // reconnect with only A: B and C marked, named once, bookings kept
    let s = store_fetch(
        &v,
        ConnectorKind::EnableBanking,
        conn,
        FetchResult {
            accounts: vec![acc("A", "Girokonto")],
            revoked_refs: vec!["B".into(), "C".into()],
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(s.revoked_accounts, vec!["Pocket", "Gemeinschaftskonto"]);
    assert_eq!(
        revoked(),
        vec![("A".into(), false), ("B".into(), true), ("C".into(), true)]
    );
    let st = v.connection_status(conn).unwrap();
    assert_eq!((st.account_count, st.revoked_account_count), (3, 2));
    // repeated report: no second notice
    let s = store_fetch(
        &v,
        ConnectorKind::EnableBanking,
        conn,
        FetchResult {
            accounts: vec![acc("A", "Girokonto")],
            revoked_refs: vec!["B".into()],
            ..Default::default()
        },
    )
    .unwrap();
    assert!(s.revoked_accounts.is_empty());
    // later consent includes B again plus a new account D
    let s = store_fetch(
        &v,
        ConnectorKind::EnableBanking,
        conn,
        FetchResult {
            accounts: vec![
                acc("A", "Girokonto"),
                acc("B", "Pocket"),
                acc("D", "GLS Giro"),
            ],
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(s.new_accounts, vec!["GLS Giro"]);
    assert_eq!(
        revoked(),
        vec![
            ("A".into(), false),
            ("B".into(), false),
            ("C".into(), true),
            ("D".into(), false)
        ]
    );
    let accs = v.list_accounts().unwrap();
    assert!(
        accs.iter()
            .any(|a| a.name == "Gemeinschaftskonto" && a.access_revoked)
    );
    std::fs::remove_file(path).ok();
}

#[test]
fn automatic_names_and_bank_name_are_updated_user_names_kept() {
    let (v, path) = demo_vault();
    let conn = v
        .add_bank_connection(ConnectorKind::EnableBanking, String::new(), "C24".into())
        .unwrap();
    let acc = |r: &str, name: &str, owner: &str| RemoteAccount {
        external_ref: r.into(),
        name: name.into(),
        owner: owner.into(),
        iban: String::new(),
        account_number: String::new(),
        currency: "EUR".into(),
        kind: AccountKind::Checking,
        balance_cents: 0,
        balance_date: "2026-11-11".into(),
    };
    // as stored by the earlier build: holder as name, generic bank name
    store_fetch(
        &v,
        ConnectorKind::EnableBanking,
        conn,
        FetchResult {
            accounts: vec![acc("G", "Jay Licina", ""), acc("S", "Jay Licina", "")],
            ..Default::default()
        },
    )
    .unwrap();
    let db_rename = |r: &str, n: &str| {
        v.db()
            .unwrap()
            .execute(
                "UPDATE accounts SET name = ?1 WHERE external_ref = ?2",
                params![n, r],
            )
            .unwrap();
    };
    db_rename("S", "Mein Sparkonto"); // the user renamed this one
    store_fetch(
        &v,
        ConnectorKind::EnableBanking,
        conn,
        FetchResult {
            accounts: vec![
                acc("G", "C24 Smartkonto", "Jay Licina"),
                acc("S", "C24 PocketZINS Smart", "Jay Licina"),
            ],
            bank_name: Some("C24".into()),
            name_updates: vec![
                ("G".into(), "Jay Licina".into(), "C24 Smartkonto".into()),
                (
                    "S".into(),
                    "Jay Licina".into(),
                    "C24 PocketZINS Smart".into(),
                ),
            ],
            ..Default::default()
        },
    )
    .unwrap();
    let rows: Vec<(String, String, String, String)> = {
        let db = v.db().unwrap();
        let mut q = db
            .prepare("SELECT external_ref, name, bank_name, owner FROM accounts WHERE connection_id = ?1 ORDER BY external_ref")
            .unwrap();
        q.query_map([conn], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    assert_eq!(
        rows,
        vec![
            (
                "G".into(),
                "C24 Smartkonto".into(),
                "C24".into(),
                "Jay Licina".into()
            ),
            (
                "S".into(),
                "Mein Sparkonto".into(),
                "C24".into(),
                "Jay Licina".into()
            ),
        ]
    );
    std::fs::remove_file(path).ok();
}

#[test]
fn complete_account_list_marks_the_missing_ones() {
    let (v, path) = demo_vault();
    let conn = v
        .add_bank_connection(ConnectorKind::EnableBanking, String::new(), "C24".into())
        .unwrap();
    let acc = |r: &str, name: &str| RemoteAccount {
        external_ref: r.into(),
        name: name.into(),
        owner: String::new(),
        iban: String::new(),
        account_number: String::new(),
        currency: "EUR".into(),
        kind: AccountKind::Checking,
        balance_cents: 0,
        balance_date: "2026-11-11".into(),
    };
    let three = || {
        vec![
            acc("DE23", "Girokonto"),
            acc("hs", "PocketZINS"),
            acc("DE48", "Gemeinschaftskonto"),
        ]
    };
    store_fetch(
        &v,
        ConnectorKind::EnableBanking,
        conn,
        FetchResult {
            accounts: three(),
            accounts_complete: true,
            ..Default::default()
        },
    )
    .unwrap();
    // a partial sync (e.g. bank's daily limit) marks nothing
    let s = store_fetch(
        &v,
        ConnectorKind::EnableBanking,
        conn,
        FetchResult {
            accounts: vec![acc("DE23", "Girokonto")],
            accounts_complete: false,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(s.revoked_accounts.is_empty());
    // an empty "complete" list is never trusted
    let s = store_fetch(
        &v,
        ConnectorKind::EnableBanking,
        conn,
        FetchResult {
            accounts_complete: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(s.revoked_accounts.is_empty());
    // the real case: new consent with only the main account
    let s = store_fetch(
        &v,
        ConnectorKind::EnableBanking,
        conn,
        FetchResult {
            accounts: vec![acc("DE23", "Girokonto")],
            accounts_complete: true,
            ..Default::default()
        },
    )
    .unwrap();
    let mut names = s.revoked_accounts.clone();
    names.sort();
    assert_eq!(names, vec!["Gemeinschaftskonto", "PocketZINS"]);
    assert_eq!(v.connection_status(conn).unwrap().revoked_account_count, 2);
    // next sync: no repeated notice
    let s = store_fetch(
        &v,
        ConnectorKind::EnableBanking,
        conn,
        FetchResult {
            accounts: vec![acc("DE23", "Girokonto")],
            accounts_complete: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(s.revoked_accounts.is_empty());
    // all three granted again: marks cleared
    store_fetch(
        &v,
        ConnectorKind::EnableBanking,
        conn,
        FetchResult {
            accounts: three(),
            accounts_complete: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(v.connection_status(conn).unwrap().revoked_account_count, 0);
    std::fs::remove_file(path).ok();
}

#[test]
fn renamed_refs_keep_the_account() {
    let (v, path) = demo_vault();
    let conn = v
        .add_bank_connection(ConnectorKind::EnableBanking, String::new(), "C24".into())
        .unwrap();
    let acc = |r: &str| RemoteAccount {
        external_ref: r.into(),
        name: "Girokonto".into(),
        owner: String::new(),
        iban: "DE00100000000000000001".into(),
        account_number: String::new(),
        currency: "EUR".into(),
        kind: AccountKind::Checking,
        balance_cents: 100_00,
        balance_date: "2026-11-10".into(),
    };
    let count = || -> i64 {
        v.db()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM accounts WHERE connection_id = ?1",
                [conn],
                |r| r.get(0),
            )
            .unwrap()
    };
    let uid = "3fa85f64-5717-4562-b3fc-2c963f66afa6";
    let stable = "DE00100000000000000001";
    store_fetch(
        &v,
        ConnectorKind::EnableBanking,
        conn,
        FetchResult {
            accounts: vec![acc(uid)],
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(count(), 1);
    store_fetch(
        &v,
        ConnectorKind::EnableBanking,
        conn,
        FetchResult {
            accounts: vec![acc(stable)],
            renamed_refs: vec![(uid.into(), stable.into())],
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(count(), 1, "rename must not create a second account");
    // applying the same rename again is harmless
    store_fetch(
        &v,
        ConnectorKind::EnableBanking,
        conn,
        FetchResult {
            accounts: vec![acc(stable)],
            renamed_refs: vec![(uid.into(), stable.into())],
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(count(), 1);
    // a later sync recognises it as savings and learns its BBAN
    let mut savings = acc(stable);
    savings.kind = AccountKind::Savings;
    savings.account_number = "500240240000000099".into();
    store_fetch(
        &v,
        ConnectorKind::EnableBanking,
        conn,
        FetchResult {
            accounts: vec![savings],
            ..Default::default()
        },
    )
    .unwrap();
    let (kind, number): (String, String) = v
        .db()
        .unwrap()
        .query_row(
            "SELECT kind, IFNULL(account_number, '') FROM accounts WHERE connection_id = ?1",
            [conn],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (kind.as_str(), number.as_str()),
        ("savings", "500240240000000099")
    );
    assert_eq!(count(), 1);
    std::fs::remove_file(path).ok();
}

#[tokio::test]
async fn magic_demo_login_syncs_without_network_or_product_id() {
    let dir = std::env::temp_dir().join(format!("gc-magic-demo-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("v.girocrab");
    let v = Vault::open(path.to_str().unwrap().into(), vec![9u8; 32]).unwrap();
    // Real (non-demo) flag, no FinTS product ID set: only the login "Demo" makes it work.
    let id = v
        .add_fints_connection(
            "99999999".into(),
            "https://demo.invalid/fints".into(),
            "Demo-Bank".into(),
            " Demo ".into(),
            String::new(),
        )
        .unwrap();
    let s = v.clone().begin_sync(id, "12345".into(), false).unwrap();
    assert!(matches!(
        s.start().await.unwrap(),
        SyncStep::NeedsChallenge { .. }
    ));
    let _ = s.poll().await.unwrap();
    match s.poll().await.unwrap() {
        SyncStep::Finished { .. } => {}
        other => panic!("expected finish, got {other:?}"),
    }
    assert!(
        v.list_accounts()
            .unwrap()
            .iter()
            .any(|a| a.connection_id == Some(id))
    );
    // Any other login still needs the product ID.
    let other = v
        .add_fints_connection(
            "88888888".into(),
            "https://x.example/fints".into(),
            "X".into(),
            "someone".into(),
            String::new(),
        )
        .unwrap();
    // (Only without an embedded GiroCrab registration: release builds carry one.)
    if !fints::product_id::builtin_available() {
        assert!(matches!(
            v.clone().begin_sync(other, "1".into(), false),
            Err(CoreError::MissingSetting { .. })
        ));
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn demo_push_tan_flow_imports_and_categorises() {
    let (v, path) = demo_vault();
    let conn = v
        .list_bank_connections()
        .unwrap()
        .into_iter()
        .find(|c| c.connector == ConnectorKind::VolksbankKasselGoettingen)
        .unwrap();
    let before = v
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|a| a.name == "Musterbank Girokonto" && a.connection_id == Some(conn.id))
        .unwrap();
    let s = v.clone().begin_sync(conn.id, "pin".into(), true).unwrap();
    match s.start().await.unwrap() {
        SyncStep::NeedsChallenge { challenge } => {
            assert_eq!(challenge.kind, ChallengeKind::DecoupledPush)
        }
        other => panic!("expected push challenge, got {other:?}"),
    }
    assert!(matches!(
        s.poll().await.unwrap(),
        SyncStep::StillWaiting { .. }
    ));
    match s.poll().await.unwrap() {
        SyncStep::Finished { summary } => {
            assert_eq!(summary.transactions_imported, 1);
            assert_eq!(
                summary.transactions_categorized, 1,
                "REWE booking is categorised by a rule"
            );
        }
        other => panic!("expected finish, got {other:?}"),
    }
    let after = v
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|a| a.id == before.id)
        .unwrap();
    assert_eq!(after.balance_cents, before.balance_cents - 27_65);
    assert!(matches!(s.poll().await, Err(CoreError::SessionClosed)));
    let _ = std::fs::remove_file(path);
}

/// "<Konto> abrufen" (#1124): only that account of the login is stored.
#[tokio::test]
async fn account_sync_stores_only_that_account() {
    let (v, path) = demo_vault();
    let conn = v
        .list_bank_connections()
        .unwrap()
        .into_iter()
        .find(|c| c.connector == ConnectorKind::VolksbankKasselGoettingen)
        .unwrap();
    let accounts: Vec<_> = v
        .list_accounts()
        .unwrap()
        .into_iter()
        .filter(|a| a.connection_id == Some(conn.id))
        .collect();
    assert!(accounts.len() > 1, "the demo login has several accounts");
    let giro = accounts
        .iter()
        .find(|a| a.name == "Musterbank Girokonto")
        .unwrap();
    let other = accounts.iter().find(|a| a.id != giro.id).unwrap();
    let last_sync = |v: &Arc<Vault>| {
        v.list_bank_connections()
            .unwrap()
            .into_iter()
            .find(|c| c.id == conn.id)
            .unwrap()
            .last_sync_unix
    };
    let synced_before = last_sync(&v);
    async fn run(s: &SyncSession) -> SyncSummary {
        let mut step = s.start().await.unwrap();
        loop {
            match step {
                SyncStep::Finished { summary } => return summary,
                _ => step = s.poll().await.unwrap(),
            }
        }
    }
    // The demo bank books its new entry on the giro account: syncing
    // another account of the login leaves it out.
    let s = v
        .clone()
        .begin_account_sync(other.id, "pin".into(), true)
        .unwrap();
    assert_eq!(run(&s).await.transactions_imported, 0);
    let stored = |id: i64| {
        v.list_accounts()
            .unwrap()
            .into_iter()
            .find(|a| a.id == id)
            .unwrap()
    };
    assert_eq!(stored(giro.id).balance_cents, giro.balance_cents);
    assert!(accounts.iter().all(|a| !stored(a.id).access_revoked));
    // the login counts as synced only after a full sync (automatic sync)
    assert_eq!(last_sync(&v), synced_before);
    let s = v
        .clone()
        .begin_account_sync(giro.id, "pin".into(), true)
        .unwrap();
    assert_eq!(run(&s).await.transactions_imported, 1);
    assert_eq!(stored(giro.id).balance_cents, giro.balance_cents - 27_65);
    // an archived account is not synced at all: everything would be dropped
    v.set_account_archived(other.id, true).unwrap();
    assert!(matches!(
        v.clone().begin_account_sync(other.id, "pin".into(), true),
        Err(CoreError::InvalidInput { .. })
    ));
    let _ = std::fs::remove_file(path);
}

#[test]
fn single_account_sync_of_a_moneymoney_ref_syncs_the_whole_login() {
    use super::persist::single_account_ref;
    let none = || -> CoreResult<Vec<String>> { Ok(vec![]) };
    assert_eq!(
        single_account_ref("DE02120300000000202051".into(), none).unwrap(),
        Some("DE02120300000000202051".into())
    );
    // marked by "Abrufweg umstellen", or imported and not yet taken over (#1417)
    for r in [
        "moneymoney-db:switch:DE02120300000000202051",
        "moneymoney-db:A1B2C3",
    ] {
        assert_eq!(single_account_ref(r.into(), none).unwrap(), None, "{r}");
        let skipped = || -> CoreResult<Vec<String>> { Ok(vec![r.to_string()]) };
        assert!(matches!(
            single_account_ref(r.into(), skipped),
            Err(CoreError::InvalidInput { .. })
        ));
    }
}

#[tokio::test]
async fn account_sync_after_switching_takes_the_account_over() {
    let (v, path) = demo_vault();
    let conn = v
        .list_bank_connections()
        .unwrap()
        .into_iter()
        .find(|c| c.connector == ConnectorKind::VolksbankKasselGoettingen)
        .unwrap();
    let giro = v
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|a| a.connection_id == Some(conn.id) && a.name == "Musterbank Girokonto")
        .unwrap();
    let ext = |v: &Arc<Vault>| -> String {
        v.db()
            .unwrap()
            .query_row(
                "SELECT external_ref FROM accounts WHERE id = ?1",
                [giro.id],
                |r| r.get(0),
            )
            .unwrap()
    };
    let old = ext(&v);
    // as "Abrufweg umstellen" leaves it: the account under the marked ref
    v.db()
        .unwrap()
        .execute(
            "UPDATE accounts SET external_ref = ?1 WHERE id = ?2",
            params![format!("moneymoney-db:switch:{old}"), giro.id],
        )
        .unwrap();
    let s = v
        .clone()
        .begin_account_sync(giro.id, "pin".into(), true)
        .unwrap();
    let mut step = s.start().await.unwrap();
    let summary = loop {
        match step {
            SyncStep::Finished { summary } => break summary,
            _ => step = s.poll().await.unwrap(),
        }
    };
    // the whole login was synced: the account is taken over and gets its booking
    assert_eq!(ext(&v), old);
    assert_eq!(summary.transactions_imported, 1);
    let _ = std::fs::remove_file(path);
}

#[test]
fn only_among_names_the_account_of_a_single_account_sync() {
    let refs = vec!["a".to_string(), "b".to_string()];
    let mut ctx = ConnectContext::default();
    assert_eq!(ctx.only_among(&refs), None);
    ctx.only_account = Some("b".into());
    assert_eq!(ctx.only_among(&refs), Some("b"));
    // not among the bank's accounts under its stored ref: all, cut down later
    ctx.only_account = Some("old-b".into());
    assert_eq!(ctx.only_among(&refs), None);
}

#[test]
fn keep_only_follows_a_renamed_ref_and_keeps_revocations() {
    let account = |r: &str| RemoteAccount {
        external_ref: r.into(),
        name: String::new(),
        owner: String::new(),
        iban: String::new(),
        account_number: String::new(),
        currency: "EUR".into(),
        kind: AccountKind::Checking,
        balance_cents: 0,
        balance_date: String::new(),
    };
    let mut data = FetchResult {
        accounts: vec![account("new-a"), account("b")],
        renamed_refs: vec![("a".into(), "new-a".into())],
        revoked_refs: vec!["c".into()],
        accounts_complete: true,
        ..Default::default()
    };
    data.keep_only("a");
    assert_eq!(data.accounts.len(), 1);
    assert_eq!(data.accounts[0].external_ref, "new-a");
    assert_eq!(data.revoked_refs, ["c"]);
    assert!(!data.accounts_complete && data.single_account);
}

#[tokio::test]
async fn demo_code_and_redirect_flows() {
    let (v, path) = demo_vault();
    let conns = v.list_bank_connections().unwrap();
    let tr = conns
        .iter()
        .find(|c| c.connector == ConnectorKind::TradeRepublic)
        .unwrap();
    let s = v.clone().begin_sync(tr.id, "1234".into(), true).unwrap();
    assert!(
        matches!(s.start().await.unwrap(), SyncStep::NeedsChallenge { challenge } if challenge.kind == ChallengeKind::Code)
    );
    assert!(
        s.submit_code("12".into()).await.is_err(),
        "bad code rejected"
    );
    let amex = conns
        .iter()
        .find(|c| c.connector == ConnectorKind::AmericanExpress)
        .unwrap();
    let s = v.clone().begin_sync(amex.id, String::new(), true).unwrap();
    assert!(
        matches!(s.start().await.unwrap(), SyncStep::NeedsChallenge { challenge } if challenge.kind == ChallengeKind::Redirect)
    );
    assert!(matches!(
        s.submit_code("https://localhost/cb?code=x".into())
            .await
            .unwrap(),
        SyncStep::Finished { .. }
    ));
    let pp_id = v
        .add_bank_connection(
            ConnectorKind::PayPal,
            String::new(),
            "demo@example.org".into(),
        )
        .unwrap();
    let pp = v
        .list_bank_connections()
        .unwrap()
        .into_iter()
        .find(|c| c.id == pp_id)
        .unwrap();
    let s = v.clone().begin_sync(pp.id, String::new(), true).unwrap();
    assert!(matches!(
        s.start().await.unwrap(),
        SyncStep::Finished { .. }
    ));
    // Real (non-demo) sync refuses to start without the user's own keys.
    assert!(matches!(
        v.clone().begin_sync(pp.id, String::new(), false),
        Err(CoreError::MissingSetting { .. })
    ));
    let _ = std::fs::remove_file(path);
}

#[test]
fn amounts_above_the_sepa_limit_are_rejected_before_the_tan() {
    use crate::connectors::fints::sepa::MAX_AMOUNT_CENTS;
    use crate::models::{StandingOrderAction, StandingOrderRequest};
    let (v, path) = demo_vault();
    let acc = v
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|a| a.name == "Musterbank Girokonto" && a.connection_id.is_some())
        .unwrap();
    let transfer =
        |amount: i64, instant: bool, execution_date: Option<String>| SepaTransferRequest {
            account_id: acc.id,
            recipient_name: "Müller GmbH".into(),
            recipient_iban: "DE89370400440532013000".into(),
            recipient_bic: None,
            amount_cents: amount,
            purpose: "Rechnung 42".into(),
            instant,
            execution_date,
        };
    let later = crate::dates::iso(crate::dates::today() + crate::dates::days(10));
    for (instant, date) in [(false, None), (true, None), (false, Some(later.clone()))] {
        assert!(
            v.clone()
                .begin_transfer(
                    transfer(MAX_AMOUNT_CENTS, instant, date.clone()),
                    "pin".into(),
                    true
                )
                .is_ok()
        );
        assert!(matches!(
            v.clone().begin_transfer(
                transfer(MAX_AMOUNT_CENTS + 1, instant, date),
                "pin".into(),
                true
            ),
            Err(CoreError::InvalidInput { .. })
        ));
    }
    // A valid monthly schedule (first date on the execution day), so only the
    // amount decides.
    let first_of_month = (crate::dates::today() + crate::dates::days(40)).first_of_month();
    let order = |amount: i64| StandingOrderRequest {
        account_id: acc.id,
        action: StandingOrderAction::Create,
        order_row_id: None,
        recipient_name: "Vermieter".into(),
        recipient_iban: "DE89370400440532013000".into(),
        recipient_bic: None,
        amount_cents: amount,
        purpose: "Miete".into(),
        first_date: crate::dates::iso(first_of_month),
        last_date: None,
        time_unit: "M".into(),
        interval: 1,
        exec_day: 1,
    };
    assert!(
        v.clone()
            .begin_standing_order(order(MAX_AMOUNT_CENTS), "pin".into(), true)
            .is_ok()
    );
    assert!(matches!(
        v.clone()
            .begin_standing_order(order(MAX_AMOUNT_CENTS + 1), "pin".into(), true),
        Err(CoreError::InvalidInput { .. })
    ));
    let _ = std::fs::remove_file(path);
}

#[tokio::test(start_paused = true)]
async fn demo_transfer_flow_adds_pending_booking_and_suggests_recipient() {
    let (v, path) = demo_vault();
    let acc = v
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|a| a.name == "Musterbank Girokonto" && a.connection_id.is_some())
        .unwrap();
    let req = |iban: &str, amount: i64| SepaTransferRequest {
        account_id: acc.id,
        recipient_name: "Müller GmbH".into(),
        recipient_iban: iban.into(),
        recipient_bic: None,
        amount_cents: amount,
        purpose: "Rechnung 42".into(),
        instant: true,
        execution_date: None,
    };
    assert!(
        v.clone()
            .begin_transfer(req("DE89370400440532013001", 100), "pin".into(), true)
            .is_err()
    );
    assert!(
        v.clone()
            .begin_transfer(req("DE89370400440532013000", 0), "pin".into(), true)
            .is_err()
    );
    let s = v
        .clone()
        .begin_transfer(req("DE89 3704 0044 0532 0130 00", 1250), "pin".into(), true)
        .unwrap();
    let mut step = s.start().await.unwrap();
    let mut challenges = 0;
    while matches!(
        step,
        TransferStep::NeedsChallenge { .. } | TransferStep::StillWaiting { .. }
    ) {
        challenges += 1;
        step = s.poll().await.unwrap();
    }
    assert!(challenges >= 2, "login + transfer challenge");
    let TransferStep::Finished { outcome } = &step else {
        panic!("{step:?}")
    };
    assert!(outcome.instant);
    assert!(outcome.end_to_end_id.starts_with("GC") && outcome.end_to_end_id.len() == 32);
    let page = v
        .list_transactions(BookingFilter {
            account_ids: vec![acc.id],
            category_id: None,
            include_subcategories: false,
            uncategorized_only: false,
            search: Some("Müller GmbH".into()),
            date_from: None,
            date_to: None,
            limit: 10,
            offset: 0,
            scope: BookingScope::All,
            merchant_key: None,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(page.transactions.len(), 1);
    assert_eq!(page.transactions[0].amount_cents, -1250);
    assert_eq!(
        page.transactions[0].counterparty_iban,
        "DE89370400440532013000"
    );
    let sug = v.suggest_recipients("müller".into(), 5).unwrap();
    assert_eq!(sug.len(), 1);
    assert_eq!(sug[0].iban, "DE89370400440532013000");
    assert!(matches!(s.poll().await, Err(CoreError::SessionClosed)));

    // Runs a transfer to the end, approving every challenge.
    async fn run(s: &TransferSession) -> (TransferStep, Vec<String>) {
        let mut step = s.start().await.unwrap();
        let mut titles = Vec::new();
        while let TransferStep::NeedsChallenge { challenge }
        | TransferStep::StillWaiting { challenge } = &step
        {
            titles.push(challenge.title.clone());
            step = s.poll().await.unwrap();
        }
        (step, titles)
    }
    let bookings = |name: &str| {
        v.list_transactions(BookingFilter {
            account_ids: vec![acc.id],
            category_id: None,
            include_subcategories: false,
            uncategorized_only: false,
            search: Some(name.into()),
            date_from: None,
            date_to: None,
            limit: 10,
            offset: 0,
            scope: BookingScope::All,
            merchant_key: None,
            ..Default::default()
        })
        .unwrap()
        .transactions
    };

    // Bank does not offer instant: declined, nothing sent, no booking,
    // and no automatic regular transfer.
    let mut slow = req("DE89370400440532013000", 990);
    slow.recipient_name = "Langsam AG".into();
    let s = v.clone().begin_transfer(slow, "pin".into(), true).unwrap();
    let (step, titles) = run(&s).await;
    assert!(
        matches!(step, TransferStep::InstantDeclined { .. }),
        "{step:?}"
    );
    assert!(
        !titles.iter().any(|t| t.contains("Überweisung")),
        "only the login, no transfer approval: {titles:?}"
    );
    assert!(bookings("Langsam AG").is_empty());
    assert!(matches!(s.poll().await, Err(CoreError::SessionClosed)));

    // Ambiguous reply after the approval: unknown, booking marked as
    // unclear, session over (nothing resent).
    let mut unclear = req("DE89370400440532013000", 770);
    unclear.recipient_name = "Unklar KG".into();
    let s = v
        .clone()
        .begin_transfer(unclear, "pin".into(), true)
        .unwrap();
    let (step, _) = run(&s).await;
    let TransferStep::OutcomeUnknown { end_to_end_id, .. } = &step else {
        panic!("{step:?}")
    };
    assert!(end_to_end_id.starts_with("GC"));
    let b = bookings("Unklar KG");
    assert_eq!(b.len(), 1);
    assert!(
        b[0].booking_text.contains("Status unklar"),
        "{}",
        b[0].booking_text
    );
    assert!(matches!(s.poll().await, Err(CoreError::SessionClosed)));

    // Connection lost after the approval: unknown as well, not an error.
    let mut lost = req("DE89370400440532013000", 660);
    lost.recipient_name = "Netzfehler GmbH".into();
    let s = v.clone().begin_transfer(lost, "pin".into(), true).unwrap();
    let (step, _) = run(&s).await;
    assert!(
        matches!(step, TransferStep::OutcomeUnknown { .. }),
        "{step:?}"
    );
    assert_eq!(bookings("Netzfehler GmbH").len(), 1);

    // Regular transfer: never a second order either.
    let mut regular = req("DE89370400440532013000", 550);
    regular.instant = false;
    regular.recipient_name = "Regulär OHG".into();
    let s = v
        .clone()
        .begin_transfer(regular, "pin".into(), true)
        .unwrap();
    let (step, _) = run(&s).await;
    let TransferStep::Finished { outcome } = &step else {
        panic!("{step:?}")
    };
    assert!(!outcome.instant);

    // Name check: close match needs confirmation, then the approval.
    let mut close = req("DE89370400440532013000", 500);
    close.recipient_name = "Fast Mustermann".into();
    let s = v.clone().begin_transfer(close, "pin".into(), true).unwrap();
    let mut step = s.start().await.unwrap();
    while matches!(
        step,
        TransferStep::NeedsChallenge { .. } | TransferStep::StillWaiting { .. }
    ) {
        step = s.poll().await.unwrap();
    }
    let TransferStep::NeedsPayeeConfirmation { check } = step else {
        panic!("{step:?}")
    };
    assert_eq!(check.result, PayeeMatch::CloseMatch);
    assert_eq!(check.bank_name.as_deref(), Some("Erika Mustermann"));
    let mut step = s.confirm_payee().await.unwrap();
    while matches!(
        step,
        TransferStep::NeedsChallenge { .. } | TransferStep::StillWaiting { .. }
    ) {
        step = s.poll().await.unwrap();
    }
    assert!(matches!(step, TransferStep::Finished { .. }));

    // Cancelling at the name check sends nothing.
    let mut wrong = req("DE89370400440532013000", 700);
    wrong.recipient_name = "Falsch GmbH".into();
    let s = v.clone().begin_transfer(wrong, "pin".into(), true).unwrap();
    let mut step = s.start().await.unwrap();
    while matches!(
        step,
        TransferStep::NeedsChallenge { .. } | TransferStep::StillWaiting { .. }
    ) {
        step = s.poll().await.unwrap();
    }
    assert!(matches!(
        step,
        TransferStep::NeedsPayeeConfirmation {
            check: PayeeCheck {
                result: PayeeMatch::NoMatch,
                ..
            }
        }
    ));
    s.cancel().await;
    assert!(matches!(
        s.confirm_payee().await,
        Err(CoreError::SessionClosed)
    ));
    let _ = std::fs::remove_file(path);
}

/// Order accounts (Amazon) are proof accounts: they start outside the overall
/// money totals (#337), only on creation — a re-fetch keeps the user's choice.
#[test]
fn amazon_accounts_start_outside_the_totals_only_on_creation() {
    let p_dir = crate::test_support::TempDir::new("girocrab-amz-total");
    let p = p_dir.join("v.db");
    let v = Vault::open(p.to_string_lossy().to_string(), vec![3u8; 32]).unwrap();
    let conn = v
        .add_bank_connection(ConnectorKind::AmazonOrders, String::new(), "login".into())
        .unwrap();
    let mk = || FetchResult {
        accounts: vec![RemoteAccount {
            external_ref: "orders".into(),
            name: "Amazon Bestellungen".into(),
            owner: String::new(),
            iban: String::new(),
            account_number: String::new(),
            currency: "EUR".into(),
            kind: AccountKind::PaymentService,
            balance_cents: 0,
            balance_date: "2026-11-16".into(),
        }],
        ..Default::default()
    };
    store_fetch(&v, ConnectorKind::AmazonOrders, conn, mk()).unwrap();
    let acc = v.list_accounts().unwrap().pop().unwrap();
    assert!(!acc.include_in_total, "new order account starts excluded");
    assert!(!acc.in_total, "effective flag follows");
    // the user switches it back on; the next fetch keeps that choice
    v.set_account_include_in_total(acc.id, true).unwrap();
    store_fetch(&v, ConnectorKind::AmazonOrders, conn, mk()).unwrap();
    let acc = v.list_accounts().unwrap().pop().unwrap();
    assert!(acc.include_in_total, "re-fetch keeps the user's choice");
    assert!(acc.in_total);
}

mod account_selection;
mod bank_name;
mod banking_gate;
mod batch_transfer;
mod body_limit;
mod content_id;
mod direct_debit;
mod import_takeover;
mod payment_method;
mod postbox;
pub(crate) mod redirects;
mod standing_order;
mod statements_auto;
mod transfer_cancel;

/// Local server answering every request with a redirect to `location(port)`;
/// returns (port, request counter).
fn redirect_server(
    location: fn(u16) -> String,
) -> (u16, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = hits.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { break };
            let n = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut buf = [0u8; 8192];
            let _ = s.read(&mut buf);
            let resp = if n == 0 {
                format!(
                    "HTTP/1.1 307 Moved\r\nLocation: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    location(port)
                )
            } else {
                "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
            };
            let _ = s.write_all(resp.as_bytes());
        }
    });
    (port, hits)
}

#[tokio::test]
async fn credential_clients_do_not_follow_redirects_to_another_host() {
    let (port, hits) = redirect_server(|p| format!("http://localhost:{p}/other"));
    let c = super::http_client_same_host().unwrap();
    let r = c
        .post(format!("http://127.0.0.1:{port}/login"))
        .body("user=a&pin=secret")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 307);
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn credential_clients_still_follow_redirects_inside_the_host() {
    let (port, hits) = redirect_server(|p| format!("http://127.0.0.1:{p}/next"));
    let c = super::http_client_same_host().unwrap();
    let r = c
        .post(format!("http://127.0.0.1:{port}/login"))
        .body("x")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 2);
}

fn fetch_with_document() -> FetchResult {
    FetchResult {
        accounts: vec![RemoteAccount {
            external_ref: "tr:cash".into(),
            name: "Verrechnungskonto".into(),
            owner: String::new(),
            iban: String::new(),
            account_number: String::new(),
            currency: "EUR".into(),
            kind: AccountKind::Checking,
            balance_cents: 100,
            balance_date: "2026-11-10".into(),
        }],
        documents: vec![crate::documents::RemoteDocument {
            account_ref: "tr:cash".into(),
            booking_external_id: None,
            title: "Kosteninformation".into(),
            doc_type: "postbox".into(),
            mime_type: "application/pdf".into(),
            file_name: "k.pdf".into(),
            bytes: b"%PDF-1.4 x".to_vec(),
            source_url: "tr:doc:1".into(),
            date: "2026-11-01".into(),
        }],
        ..Default::default()
    }
}

#[test]
fn document_retrieval_is_on_by_default_and_per_connection() {
    let dir = crate::test_support::TempDir::new("girocrab-docs-switch");
    let v = Vault::open(
        dir.join("v.db").to_string_lossy().to_string(),
        vec![3u8; 32],
    )
    .unwrap();
    let a = v
        .add_bank_connection(ConnectorKind::TradeRepublic, String::new(), "a".into())
        .unwrap();
    let b = v
        .add_bank_connection(ConnectorKind::TradeRepublic, String::new(), "b".into())
        .unwrap();
    assert!(v.documents_fetch_enabled(a).unwrap());
    // only banks that deliver documents offer the switch
    assert!(v.documents_fetch_supported(a).unwrap());
    let ibkr = v
        .add_bank_connection(ConnectorKind::Ibkr, String::new(), "c".into())
        .unwrap();
    assert!(!v.documents_fetch_supported(ibkr).unwrap());
    v.set_documents_fetch(a, false).unwrap();
    assert!(!v.documents_fetch_enabled(a).unwrap());
    assert!(
        v.documents_fetch_enabled(b).unwrap(),
        "another connection is not affected"
    );
    v.set_documents_fetch(a, true).unwrap();
    assert!(v.documents_fetch_enabled(a).unwrap());
}

#[test]
fn switching_retrieval_on_again_catches_up_until_a_sync_brings_nothing_new() {
    let dir = crate::test_support::TempDir::new("girocrab-docs-catchup");
    let v = Vault::open(
        dir.join("v.db").to_string_lossy().to_string(),
        vec![5u8; 32],
    )
    .unwrap();
    let conn = v
        .add_bank_connection(ConnectorKind::TradeRepublic, String::new(), "login".into())
        .unwrap();
    let catchup = format!("documents.fetch.catchup.{conn}");
    let off_since = format!("documents.fetch.off_since.{conn}");
    // never switched off: nothing to catch up
    v.set_documents_fetch(conn, true).unwrap();
    assert_eq!(v.get_setting(catchup.clone()).unwrap(), None);
    // off: the day is noted; on again: it becomes the catch-up day
    v.set_documents_fetch(conn, false).unwrap();
    let day = v.get_setting(off_since.clone()).unwrap();
    assert!(day.is_some());
    v.set_documents_fetch(conn, true).unwrap();
    assert_eq!(v.get_setting(catchup.clone()).unwrap(), day);
    assert_eq!(v.get_setting(off_since.clone()).unwrap(), None);
    // switched off again (and on) while it runs: the oldest day stays
    v.set_setting(catchup.clone(), "2026-01-01".into()).unwrap();
    v.set_documents_fetch(conn, false).unwrap();
    v.set_documents_fetch(conn, true).unwrap();
    assert_eq!(
        v.get_setting(catchup.clone()).unwrap().as_deref(),
        Some("2026-01-01")
    );
    // switched off twice in a row: the first day counts
    v.set_documents_fetch(conn, false).unwrap();
    v.set_setting(off_since.clone(), "2026-02-02".into())
        .unwrap();
    v.set_documents_fetch(conn, false).unwrap();
    assert_eq!(
        v.get_setting(off_since.clone()).unwrap().as_deref(),
        Some("2026-02-02")
    );
    v.set_documents_fetch(conn, true).unwrap();
    assert_eq!(
        v.get_setting(catchup.clone()).unwrap().as_deref(),
        Some("2026-01-01")
    );
    v.set_setting(catchup.clone(), day.clone().unwrap())
        .unwrap();
    // a sync that stores documents keeps the catch-up going …
    store_fetch(
        &v,
        ConnectorKind::TradeRepublic,
        conn,
        fetch_with_document(),
    )
    .unwrap();
    assert_eq!(v.get_setting(catchup.clone()).unwrap(), day);
    // a sync that left documents out (failed request, limit) does not end it either
    let mut left_out = fetch_with_document();
    left_out.documents.clear();
    left_out.documents_incomplete = true;
    store_fetch(&v, ConnectorKind::TradeRepublic, conn, left_out).unwrap();
    assert_eq!(v.get_setting(catchup.clone()).unwrap(), day);
    // … and the first one with nothing new ends it
    store_fetch(
        &v,
        ConnectorKind::TradeRepublic,
        conn,
        fetch_with_document(),
    )
    .unwrap();
    assert_eq!(v.get_setting(catchup.clone()).unwrap(), None);
    // while it is off, a sync does not end a running catch-up
    v.set_documents_fetch(conn, false).unwrap();
    v.set_documents_fetch(conn, true).unwrap();
    v.set_documents_fetch(conn, false).unwrap();
    store_fetch(
        &v,
        ConnectorKind::TradeRepublic,
        conn,
        fetch_with_document(),
    )
    .unwrap();
    assert!(v.get_setting(catchup).unwrap().is_some());
}

#[test]
fn a_deleted_connection_forgets_its_document_retrieval_settings() {
    let dir = crate::test_support::TempDir::new("girocrab-docs-forget");
    let v = Vault::open(
        dir.join("v.db").to_string_lossy().to_string(),
        vec![6u8; 32],
    )
    .unwrap();
    let conn = v
        .add_bank_connection(ConnectorKind::TradeRepublic, String::new(), "login".into())
        .unwrap();
    v.set_documents_fetch(conn, false).unwrap();
    v.set_documents_fetch(conn, true).unwrap();
    v.set_documents_fetch(conn, false).unwrap();
    let keys = [
        format!("documents.fetch.{conn}"),
        format!("documents.fetch.off_since.{conn}"),
        format!("documents.fetch.catchup.{conn}"),
    ];
    for key in &keys {
        assert!(v.get_setting(key.clone()).unwrap().is_some(), "{key}");
    }
    v.delete_bank_connection(conn).unwrap();
    for key in &keys {
        assert_eq!(v.get_setting(key.clone()).unwrap(), None, "{key}");
    }
}

#[test]
fn a_fetch_stores_no_documents_while_retrieval_is_off() {
    let dir = crate::test_support::TempDir::new("girocrab-docs-off");
    let v = Vault::open(
        dir.join("v.db").to_string_lossy().to_string(),
        vec![4u8; 32],
    )
    .unwrap();
    let conn = v
        .add_bank_connection(ConnectorKind::TradeRepublic, String::new(), "login".into())
        .unwrap();
    v.set_documents_fetch(conn, false).unwrap();
    store_fetch(
        &v,
        ConnectorKind::TradeRepublic,
        conn,
        fetch_with_document(),
    )
    .unwrap();
    assert!(
        v.list_all_documents(crate::documents::DocumentFilter::default())
            .unwrap()
            .is_empty()
    );
    // on again: the same delivery is stored
    v.set_documents_fetch(conn, true).unwrap();
    store_fetch(
        &v,
        ConnectorKind::TradeRepublic,
        conn,
        fetch_with_document(),
    )
    .unwrap();
    assert_eq!(
        v.list_all_documents(crate::documents::DocumentFilter::default())
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn portal_connections_list_nothing_to_download_while_retrieval_is_off() {
    let dir = crate::test_support::TempDir::new("girocrab-docs-portal");
    let v = Vault::open(
        dir.join("v.db").to_string_lossy().to_string(),
        vec![5u8; 32],
    )
    .unwrap();
    let conn = v
        .add_bank_connection(ConnectorKind::Fnz, String::new(), "login".into())
        .unwrap();
    v.set_documents_fetch(conn, false).unwrap();
    // not even a list that would be invalid is looked at
    assert!(
        v.clone()
            .pending_fnz_documents(conn, "kein json".into())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn managed_hold_prefix_survives_same_amount_payment_until_removed_from_list() {
    let _p_dir = crate::test_support::TempDir::new("girocrab-hold-1111");
    let v = Vault::open(
        _p_dir.join("v.db").to_string_lossy().to_string(),
        vec![3u8; 32],
    )
    .unwrap();
    let conn = v
        .add_bank_connection(
            ConnectorKind::PayPal,
            String::new(),
            "user@example.com".into(),
        )
        .unwrap();
    let mut f1 = FetchResult::default();
    f1.accounts.push(RemoteAccount {
        external_ref: "paypal:EUR".into(),
        name: "PayPal".into(),
        owner: "user@example.com".into(),
        iban: String::new(),
        account_number: "user@example.com".into(),
        currency: "EUR".into(),
        kind: AccountKind::PaymentService,
        balance_cents: 100_00,
        balance_date: "2026-11-18".into(),
    });
    f1.hold_prefix = Some("pending-hold-paypal-".into());
    f1.transactions.push(RemoteTransaction {
        account_ref: "paypal:EUR".into(),
        external_id: "pending-hold-paypal-H1".into(),
        booking_date: "2026-11-18".into(),
        value_date: "2026-11-18".into(),
        amount_cents: -30_00,
        currency: "EUR".into(),
        counterparty: "Hotel".into(),
        ..Default::default()
    });
    // Gleich hoher anderer Kauf am Folgetag ohne Gegen-IBAN
    f1.transactions.push(RemoteTransaction {
        account_ref: "paypal:EUR".into(),
        external_id: "TX-OTHER-30".into(),
        booking_date: "2026-11-19".into(),
        value_date: "2026-11-19".into(),
        amount_cents: -30_00,
        currency: "EUR".into(),
        counterparty: "Shop".into(),
        ..Default::default()
    });
    let saved_accounts = f1.accounts.clone();
    store_fetch(&v, ConnectorKind::PayPal, conn, f1).unwrap();

    let active_holds = || -> Vec<String> {
        let db = v.db().unwrap();
        let mut q = db
            .prepare("SELECT external_id FROM transactions WHERE external_id LIKE 'pending-hold-paypal-%' AND deleted_ms IS NULL")
            .unwrap();
        q.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert_eq!(active_holds(), vec!["pending-hold-paypal-H1"]);

    // Erst wenn die Reservierung nicht mehr in der offenen Liste steht, wird sie gelöscht
    let f2 = FetchResult {
        accounts: saved_accounts,
        hold_prefix: Some("pending-hold-paypal-".into()),
        ..Default::default()
    };
    store_fetch(&v, ConnectorKind::PayPal, conn, f2).unwrap();
    assert!(active_holds().is_empty());
}

#[test]
fn sync_moves_a_retired_fints_url_to_the_current_endpoint() {
    let (v, _path) = demo_vault();
    let id = v
        .add_fints_connection(
            "30060601".into(),
            "https://fints1.atruvia.de/cgi-bin/hbciservlet".into(),
            "apoBank".into(),
            "nutzer".into(),
            String::new(),
        )
        .unwrap();
    let _ = v.clone().begin_sync(id, "12345".into(), true);
    assert_eq!(
        v.get_setting("fints.url.30060601".into()).unwrap(),
        Some("https://fints.apobank.de/FinTs30PinTanHttpGate".into())
    );
}

#[test]
fn single_account_ref_refuses_switched_account_excluded_in_selection() {
    let res = crate::connectors::persist::single_account_ref(
        "moneymoney-db:switch:DE89370400440532013000".into(),
        || Ok(vec!["DE89370400440532013000".into()]),
    );
    assert!(res.is_err());
    let ok = crate::connectors::persist::single_account_ref(
        "moneymoney-db:switch:DE89370400440532013000".into(),
        || Ok(vec!["OTHER".into()]),
    )
    .unwrap();
    assert_eq!(ok, None);
}
