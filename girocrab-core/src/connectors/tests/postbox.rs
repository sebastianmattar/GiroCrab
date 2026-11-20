//! comdirect PostBox next to a FinTS login (#838): switch, due date, storage.

use jiff::civil::Date;

use crate::connectors::*;
use crate::documents::RemoteDocument;
use crate::models::{AccountKind, ConnectorKind};
use crate::vault::Vault;

fn vault(name: &str) -> (crate::test_support::TempDir, std::sync::Arc<Vault>) {
    let dir = crate::test_support::TempDir::new(name);
    let v = Vault::open(
        dir.join("v.db").to_string_lossy().to_string(),
        vec![7u8; 32],
    )
    .unwrap();
    (dir, v)
}

fn connection(v: &Vault, connector: &str) -> i64 {
    let db = v.db().unwrap();
    db.execute(
        "INSERT INTO bank_connections(connector, display_name, login_name) VALUES (?1, 'comdirect', 'x')",
        [connector],
    )
    .unwrap();
    db.last_insert_rowid()
}

fn doc(account_ref: &str, id: &str) -> RemoteDocument {
    RemoteDocument {
        account_ref: account_ref.into(),
        booking_external_id: None,
        title: format!("Dokument {id}"),
        doc_type: "postbox".into(),
        mime_type: "application/pdf".into(),
        file_name: format!("{id}.pdf"),
        bytes: b"%PDF-1.4 test".to_vec(),
        source_url: format!("comdirect:postbox:{id}"),
        date: "2026-11-10".into(),
    }
}

fn day(s: &str) -> Date {
    Date::strptime("%Y-%m-%d", s).unwrap()
}

#[test]
fn postbox_needs_comdirect_fints_and_api_credentials() {
    let (_dir, v) = vault("girocrab-postbox-switch");
    let other = connection(&v, "fints:12345678");
    let api = connection(&v, "comdirect");
    let c = connection(&v, "fints:20041155");
    assert!(v.set_comdirect_postbox(other, true).is_err());
    assert!(v.set_comdirect_postbox(api, true).is_err());
    assert!(matches!(
        v.set_comdirect_postbox(c, true),
        Err(crate::error::CoreError::MissingSetting { .. })
    ));
    v.set_setting("comdirect.client_id".into(), "id".into())
        .unwrap();
    v.set_setting("comdirect.client_secret".into(), "secret".into())
        .unwrap();
    v.set_comdirect_postbox(c, true).unwrap();
    assert!(v.comdirect_postbox_enabled(c).unwrap());
    assert!(!v.comdirect_postbox_enabled(other).unwrap());
    // switching off forgets the last fetch: switched on again, everything is fetched
    v.set_setting(postbox_last_key(c), "2026-11-01".into())
        .unwrap();
    v.set_comdirect_postbox(c, false).unwrap();
    assert!(!v.comdirect_postbox_enabled(c).unwrap());
    assert_eq!(v.get_setting(postbox_last_key(c)).unwrap(), None);
    // "PostBox jetzt abrufen" only when it is on
    assert!(
        v.clone()
            .fetch_comdirect_postbox(c, String::new(), false)
            .is_err()
    );
}

#[test]
fn postbox_and_statements_are_refused_while_document_retrieval_is_off() {
    let (_dir, v) = vault("girocrab-postbox-docs-off");
    let c = connection(&v, "fints:20041155");
    v.set_setting("comdirect.client_id".into(), "id".into())
        .unwrap();
    v.set_setting("comdirect.client_secret".into(), "secret".into())
        .unwrap();
    v.set_comdirect_postbox(c, true).unwrap();
    store_fetch(
        &v,
        ConnectorKind::FinTsGeneric,
        c,
        FetchResult {
            accounts: vec![RemoteAccount {
                external_ref: "DE02200411550123456700".into(),
                name: "Girokonto".into(),
                owner: String::new(),
                iban: "DE02200411550123456700".into(),
                account_number: "123456700".into(),
                currency: "EUR".into(),
                kind: AccountKind::Checking,
                balance_cents: 0,
                balance_date: "2026-11-17".into(),
            }],
            ..Default::default()
        },
    )
    .unwrap();
    let account: i64 = v
        .db()
        .unwrap()
        .query_row(
            "SELECT id FROM accounts WHERE connection_id = ?1",
            [c],
            |r| r.get(0),
        )
        .unwrap();
    v.set_documents_fetch(c, false).unwrap();
    // an explicit request must not run for nothing: what it brings would not be stored
    let err = v
        .clone()
        .fetch_account_statements(account, "pin".into(), true)
        .map(|_| ())
        .unwrap_err();
    assert!(err.to_string().contains("ausgeschaltet"), "{err}");
    let err = v
        .clone()
        .fetch_comdirect_postbox(c, "pin".into(), true)
        .map(|_| ())
        .unwrap_err();
    assert!(err.to_string().contains("ausgeschaltet"), "{err}");
    // a regular sync leaves the PostBox out
    let ctx_free = v.clone().begin_sync(c, "pin".into(), true);
    assert!(ctx_free.is_ok());
    v.set_documents_fetch(c, true).unwrap();
    assert!(
        v.clone()
            .fetch_comdirect_postbox(c, "pin".into(), true)
            .is_ok()
    );
    assert!(
        v.clone()
            .fetch_account_statements(account, "pin".into(), true)
            .is_ok()
    );
}

#[test]
fn postbox_is_fetched_weekly_with_a_week_of_overlap() {
    let today = day("2026-11-17");
    // never fetched: everything
    assert_eq!(
        postbox_window(None, today, PostboxMode::Due).as_deref(),
        Some("")
    );
    // fetched 3 days ago: not due, unless asked for now
    let last = Some(day("2026-11-14"));
    assert_eq!(postbox_window(last, today, PostboxMode::Due), None);
    assert_eq!(
        postbox_window(last, today, PostboxMode::Now).as_deref(),
        Some("2026-11-07")
    );
    // fetched 7 days ago: due again
    let last = Some(day("2026-11-10"));
    assert_eq!(
        postbox_window(last, today, PostboxMode::Due).as_deref(),
        Some("2026-11-03")
    );
    // an automatic sync never fetches it
    assert_eq!(postbox_window(None, today, PostboxMode::Off), None);
}

#[test]
fn documents_without_account_are_kept_and_the_fetch_date_moves_on() {
    let (_dir, v) = vault("girocrab-postbox-store");
    let c = connection(&v, "fints:20041155");
    let data = FetchResult {
        accounts: vec![RemoteAccount {
            external_ref: "DE02200411550123456700".into(),
            name: "Girokonto".into(),
            owner: String::new(),
            iban: "DE02200411550123456700".into(),
            account_number: "123456700".into(),
            currency: "EUR".into(),
            kind: AccountKind::Checking,
            balance_cents: 0,
            balance_date: "2026-11-17".into(),
        }],
        documents: vec![doc("DE02200411550123456700", "a"), doc("", "b")],
        postbox_fetched: true,
        ..Default::default()
    };
    store_fetch(&v, ConnectorKind::FinTsGeneric, c, data).unwrap();
    let rows: Vec<(String, Option<i64>)> = {
        let db = v.db().unwrap();
        let mut st = db
            .prepare("SELECT source_url, account_id FROM booking_documents ORDER BY source_url")
            .unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert_eq!(rows.len(), 2);
    assert!(rows[0].1.is_some(), "assigned document is on the account");
    assert_eq!(rows[1], ("comdirect:postbox:b".to_string(), None));
    assert_eq!(v.get_setting(postbox_last_key(c)).unwrap(), Some(today()));
    // the same documents again: nothing new, but "b" now gets the account found for
    // it, even when its download failed (no bytes); a new one without bytes is not stored
    let mut b = doc("DE02200411550123456700", "b");
    b.bytes.clear();
    let mut c_new = doc("DE02200411550123456700", "c");
    c_new.bytes.clear();
    let again = FetchResult {
        documents: vec![doc("", "a"), b, c_new],
        ..Default::default()
    };
    store_fetch(&v, ConnectorKind::FinTsGeneric, c, again).unwrap();
    let unassigned: i64 = v
        .db()
        .unwrap()
        .query_row(
            "SELECT count(*) FROM booking_documents WHERE account_id IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        unassigned, 0,
        "an assigned document never loses its account"
    );
    let n: i64 = v
        .db()
        .unwrap()
        .query_row("SELECT count(*) FROM booking_documents", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 2);
}

#[test]
fn documents_can_be_moved_to_another_account_or_none() {
    let (_dir, v) = vault("girocrab-postbox-move");
    let c = connection(&v, "fints:20041155");
    let acc = |r: &str, kind| RemoteAccount {
        external_ref: r.into(),
        name: r.into(),
        owner: String::new(),
        iban: String::new(),
        account_number: String::new(),
        currency: "EUR".into(),
        kind,
        balance_cents: 0,
        balance_date: "2026-11-17".into(),
    };
    let data = FetchResult {
        accounts: vec![
            acc("giro", AccountKind::Checking),
            acc("depot", AccountKind::Brokerage),
        ],
        documents: vec![doc("giro", "x")],
        ..Default::default()
    };
    store_fetch(&v, ConnectorKind::FinTsGeneric, c, data).unwrap();
    let (doc_id, depot): (i64, i64) = {
        let db = v.db().unwrap();
        (
            db.query_row("SELECT id FROM booking_documents", [], |r| r.get(0))
                .unwrap(),
            db.query_row(
                "SELECT id FROM accounts WHERE external_ref = 'depot'",
                [],
                |r| r.get(0),
            )
            .unwrap(),
        )
    };
    let account_of = || -> Option<i64> {
        v.db()
            .unwrap()
            .query_row(
                "SELECT account_id FROM booking_documents WHERE id = ?1",
                [doc_id],
                |r| r.get(0),
            )
            .unwrap()
    };
    v.set_document_account(doc_id, Some(depot)).unwrap();
    assert_eq!(account_of(), Some(depot));
    v.set_document_account(doc_id, None).unwrap();
    assert_eq!(account_of(), None);
    assert!(v.set_document_account(doc_id, Some(9999)).is_err());
    assert!(v.set_document_account(9999, Some(depot)).is_err());
}

/// Consorsbank Online-Archiv (#1024): same switch, no API credentials; documents
/// stored before (fetched or imported under the bank's file name) are known.
#[test]
fn consorsbank_archive_switch_and_known_documents() {
    let (_dir, v) = vault("girocrab-consorsbank-archive");
    let c = connection(&v, "fints:76030080");
    let other = connection(&v, "fints:12345678");
    v.set_comdirect_postbox(c, true).unwrap();
    assert!(v.comdirect_postbox_enabled(c).unwrap());
    assert!(v.set_comdirect_postbox(other, true).is_err());
    // switching off works the same way and forgets the last fetch
    v.set_setting(postbox_last_key(c), "2026-11-01".into())
        .unwrap();
    v.set_comdirect_postbox(c, false).unwrap();
    assert!(!v.comdirect_postbox_enabled(c).unwrap());
    assert_eq!(v.get_setting(postbox_last_key(c)).unwrap(), None);
    assert!(
        v.clone()
            .fetch_comdirect_postbox(c, String::new(), false)
            .is_err()
    );
    v.set_comdirect_postbox(c, true).unwrap();
    let account = |iban: &str| RemoteAccount {
        external_ref: iban.into(),
        name: "Verrechnungskonto".into(),
        owner: String::new(),
        iban: iban.into(),
        account_number: iban[12..].into(),
        currency: "EUR".into(),
        kind: AccountKind::Checking,
        balance_cents: 0,
        balance_date: "2026-11-17".into(),
    };
    let mut imported = doc("DE00760300800111111111", "imported");
    imported.file_name = "KAUF_111111111_dat20261102_id1111111111.pdf".into();
    imported.source_url = "moneymoney:1:abcdef".into();
    let mut fetched = doc("", "fetched");
    fetched.source_url = "consorsbank:archive:2222222222".into();
    let data = FetchResult {
        accounts: vec![account("DE00760300800111111111")],
        documents: vec![imported, fetched],
        postbox_fetched: true,
        ..Default::default()
    };
    let summary = store_fetch(&v, ConnectorKind::FinTsGeneric, c, data).unwrap();
    assert_eq!(
        summary.postbox_documents,
        Some(1),
        "only archive documents are counted"
    );
    assert_eq!(v.get_setting(postbox_last_key(c)).unwrap(), Some(today()));
    // a document with a bank file name at another login is not this login's
    let mut foreign = doc("DE00123456780333333333", "foreign");
    foreign.file_name = "KONTOAUSZUG_x_id3333333333.pdf".into();
    let data = FetchResult {
        accounts: vec![account("DE00123456780333333333")],
        documents: vec![foreign],
        ..Default::default()
    };
    store_fetch(&v, ConnectorKind::FinTsGeneric, other, data).unwrap();
    let mut known: Vec<String> = v
        .consorsbank_known_documents(c)
        .unwrap()
        .into_iter()
        .collect();
    known.sort();
    assert_eq!(known, ["1111111111", "2222222222"]);
}
