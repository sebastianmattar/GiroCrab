use super::*;
const BANK: &str = "00000000-0000-4000-8000-000000000001";
pub(super) fn body(status: &str, reference: &str, amount: &str, merchant: &str) -> String {
    format!(
        r#"{{"transactions":[{{"amount":{amount},"currency":"EUR","transactionDate":"2026-11-12T00:00:00","status":"{status}","reference":"{reference}","uniqueReference":"{reference}","merchantName":"{merchant}"}}]}}"#
    )
}
pub(super) fn setup() -> (Arc<Vault>, i64, crate::test_support::TempDir) {
    let dir = crate::test_support::TempDir::new("advanzia");
    let v = Vault::open(
        dir.join("fake.girocrab").to_string_lossy().into(),
        vec![7; 32],
    )
    .unwrap();
    let id = v
        .add_bank_connection(
            ConnectorKind::Advanzia,
            "Testkarte".into(),
            "fictional-login".into(),
        )
        .unwrap();
    v.advanzia_profile(id).unwrap();
    (v, id, dir)
}
fn sync(v: &Arc<Vault>, id: i64, bodies: Vec<String>, balance: &str) -> CoreResult<SyncSummary> {
    v.clone().import_advanzia_capture(
        id,
        BANK.into(),
        v.advanzia_profile(id)?.profile_id,
        bodies,
        balance.into(),
        None,
    )
}
pub(super) fn rows(v: &Vault) -> Vec<(i64, String, i64, Option<i64>)> {
    let db = v.db().unwrap();
    let mut q = db
        .prepare("SELECT id,external_id,amount_cents,deleted_ms FROM transactions ORDER BY id")
        .unwrap();
    q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}
#[test]
fn parser_rejects_schema_status_dates_currency_and_conflicting_ids() {
    for raw in [
        "{}".into(),
        body("SETTLED", "a", "1.00", "TEST SHOP"),
        body("PENDING", "", "1.00", "TEST SHOP"),
        body("APPROVED", "a", "1.001", "TEST SHOP"),
        body("APPROVED", "a", "1.00", "TEST SHOP").replace("EUR", "USD"),
        body("APPROVED", "a", "1.00", "TEST SHOP").replace("2026-11-12", "2026-02-30"),
    ] {
        assert!(parse_captures(&[raw]).is_err());
    }
    assert!(
        parse_captures(&[
            body("APPROVED", "same", "1", "TEST SHOP"),
            body("APPROVED", "same", "2", "TEST SHOP")
        ])
        .is_err()
    );
    assert!(parse_captures(&[]).is_err());
}
#[test]
fn parser_signed_cents_and_balance_are_strict_and_errors_are_value_free() {
    for (s, n) in [("-1265.23", -126523), ("0", 0), ("49.79", 4979)] {
        assert_eq!(parse_balance(s).unwrap(), n);
    }
    for s in [
        "null",
        "true",
        "\"12.34\"",
        "{}",
        "[]",
        "1.001",
        "1000000000",
    ] {
        assert!(parse_balance(s).is_err());
    }
    let e = parse_captures(&[body(
        "APPROVED",
        "secret-reference",
        "{}",
        "PRIVATE MERCHANT",
    )])
    .unwrap_err()
    .to_string();
    assert!(!e.contains("PRIVATE"));
    assert!(!e.contains("secret-reference"));
}
#[test]
fn repeat_import_is_idempotent_and_zero_balance_is_authoritative() {
    let (v, id, _d) = setup();
    let a = body("APPROVED", "demo-post", "-20.98", "TEST SHOP");
    assert_eq!(
        sync(&v, id, vec![a.clone()], "-500")
            .unwrap()
            .transactions_imported,
        1
    );
    assert_eq!(sync(&v, id, vec![a], "0").unwrap().transactions_imported, 0);
    assert_eq!(rows(&v).len(), 1);
    assert_eq!(v.list_accounts().unwrap()[0].balance_cents, 0);
    assert_eq!(
        sync(&v, id, vec![r#"{"transactions":[]}"#.into()], "0")
            .unwrap()
            .accounts_updated,
        1
    );
}
#[test]
fn unique_posting_settles_hold_preserving_note_and_checked() {
    let (v, id, _d) = setup();
    sync(
        &v,
        id,
        vec![body("PENDING", "demo-hold", "-20.98", "TEST SHOP")],
        "-100",
    )
    .unwrap();
    let hold = rows(&v)[0].0;
    v.db()
        .unwrap()
        .execute(
            "UPDATE transactions SET note='invented note',checked=1 WHERE id=?1",
            [hold],
        )
        .unwrap();
    sync(
        &v,
        id,
        vec![body("APPROVED", "demo-post", "-20.98", "TEST SHOP")],
        "-120.98",
    )
    .unwrap();
    let r = rows(&v);
    assert!(r[0].3.is_some());
    assert!(r[1].3.is_none());
    let (note, checked): (String, i64) = v
        .db()
        .unwrap()
        .query_row(
            "SELECT note,checked FROM transactions WHERE id=?1",
            [r[1].0],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(note, "invented note");
    assert_eq!(checked, 1);
}
#[test]
fn incomplete_or_ambiguous_snapshot_never_deletes_holds() {
    let (v, id, _d) = setup();
    let a = body("PENDING", "hold-a", "-10", "TEST SHOP");
    let b = body("PENDING", "hold-b", "-10", "TEST SHOP");
    let mut both: Value = serde_json::from_str(&a).unwrap();
    both["transactions"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::from_str::<Value>(&b).unwrap()["transactions"][0].clone());
    sync(&v, id, vec![both.to_string()], "-100").unwrap();
    sync(&v, id, vec![r#"{"transactions":[]}"#.into()], "-100").unwrap();
    assert_eq!(v.advanzia_open_holds(id).unwrap().len(), 2);
    sync(
        &v,
        id,
        vec![body("APPROVED", "post", "-10", "TEST SHOP")],
        "-110",
    )
    .unwrap();
    assert_eq!(v.advanzia_open_holds(id).unwrap().len(), 2);
}
#[test]
fn old_posting_or_other_merchant_does_not_settle_hold() {
    let (v, id, _d) = setup();
    sync(
        &v,
        id,
        vec![body("APPROVED", "old", "-10", "TEST SHOP")],
        "-100",
    )
    .unwrap();
    sync(
        &v,
        id,
        vec![body("PENDING", "hold", "-10", "TEST SHOP")],
        "-100",
    )
    .unwrap();
    sync(
        &v,
        id,
        vec![body("APPROVED", "other", "-10", "OTHER SHOP")],
        "-100",
    )
    .unwrap();
    assert_eq!(v.advanzia_open_holds(id).unwrap().len(), 1);
}
#[test]
fn two_vaults_and_two_logins_have_distinct_persistent_namespaces() {
    let (a, id, _d) = setup();
    let (b, id_b, _e) = setup();
    let p = a.advanzia_profile(id).unwrap();
    assert_eq!(p.profile_id, a.advanzia_profile(id).unwrap().profile_id);
    assert_ne!(p.profile_id, b.advanzia_profile(id_b).unwrap().profile_id);
    let other = a
        .add_bank_connection(
            ConnectorKind::Advanzia,
            "Other".into(),
            "another-fictional-login".into(),
        )
        .unwrap();
    assert_ne!(p.profile_id, a.advanzia_profile(other).unwrap().profile_id);
    sync(
        &a,
        id,
        vec![body("APPROVED", "same", "-1", "TEST SHOP")],
        "-1",
    )
    .unwrap();
    sync(
        &a,
        other,
        vec![body("APPROVED", "same", "-1", "TEST SHOP")],
        "-1",
    )
    .unwrap();
    assert_eq!(rows(&a).len(), 2);
}
#[test]
fn wrong_card_or_invalid_balance_does_not_partially_import() {
    let (v, id, _d) = setup();
    sync(&v, id, vec![body("APPROVED", "a", "-1", "TEST SHOP")], "-1").unwrap();
    assert!(
        v.clone()
            .import_advanzia_capture(
                id,
                "00000000-0000-4000-8000-000000000002".into(),
                v.advanzia_profile(id).unwrap().profile_id,
                vec![body("APPROVED", "b", "-2", "TEST SHOP")],
                "-3".into(),
                None
            )
            .is_err()
    );
    assert!(
        sync(
            &v,
            id,
            vec![body("APPROVED", "b", "-2", "TEST SHOP")],
            "null"
        )
        .is_err()
    );
    assert_eq!(rows(&v).len(), 1);
    assert_eq!(v.list_accounts().unwrap()[0].balance_cents, -100);
}
#[test]
fn explicit_target_is_bound_atomically_and_existing_work_is_kept() {
    let (v, id, _d) = setup();
    let target = {
        let db = v.db().unwrap();
        db.execute(
            "INSERT INTO account_groups(name) VALUES ('Invented cards')",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO accounts(group_id,name,kind,currency,external_ref) VALUES ((SELECT id FROM account_groups LIMIT 1),'Invented imported card','credit_card','EUR','imported-test')",[]).unwrap();
        db.last_insert_rowid()
    };
    v.clone()
        .import_advanzia_capture(
            id,
            BANK.into(),
            v.advanzia_profile(id).unwrap().profile_id,
            vec![body("APPROVED", "p", "-1", "TEST SHOP")],
            "-1".into(),
            Some(target),
        )
        .unwrap();
    let a = v.list_accounts().unwrap();
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].id, target);
    assert_eq!(a[0].name, "Invented imported card");
    assert!(
        v.clone()
            .import_advanzia_capture(
                id,
                BANK.into(),
                v.advanzia_profile(id).unwrap().profile_id,
                vec![body("APPROVED", "q", "-2", "TEST SHOP")],
                "-2".into(),
                Some(target)
            )
            .is_err()
    );
    assert_eq!(rows(&v).len(), 1);
}
#[test]
fn discard_is_explicit_scoped_and_tombstoned() {
    let (v, id, _d) = setup();
    let p = body("PENDING", "hold", "-1", "TEST SHOP");
    sync(&v, id, vec![p.clone()], "0").unwrap();
    let hold = v.advanzia_open_holds(id).unwrap()[0].id;
    let other = v
        .add_bank_connection(ConnectorKind::Advanzia, "Other".into(), "other-test".into())
        .unwrap();
    assert!(v.discard_advanzia_hold(other, hold).is_err());
    v.discard_advanzia_hold(id, hold).unwrap();
    sync(&v, id, vec![p], "0").unwrap();
    assert!(v.advanzia_open_holds(id).unwrap().is_empty());
}
#[tokio::test]
async fn adapter_never_logs_in_or_fetches_without_browser() {
    let (v, id, _d) = setup();
    let s = v.begin_sync(id, "".into(), false).unwrap();
    assert!(matches!(
        s.start().await,
        Err(CoreError::BrowserLoginRequired { .. })
    ));
    assert!(
        super::super::bank_catalog()
            .iter()
            .any(|b| b.kind == ConnectorKind::Advanzia)
    );
    assert_eq!(
        ConnectorKind::from_str("advanzia"),
        Some(ConnectorKind::Advanzia)
    );
}

#[test]
fn changing_login_rotates_profile_and_rejects_the_previous_session() {
    let (v, id, _d) = setup();
    let old = v.advanzia_profile(id).unwrap().profile_id;
    v.update_bank_connection(id, "Testkarte".into(), "changed-fictional-login".into())
        .unwrap();
    let new = v.advanzia_profile(id).unwrap().profile_id;
    assert_ne!(old, new);
    assert!(
        v.clone()
            .import_advanzia_capture(
                id,
                BANK.into(),
                old,
                vec![body("APPROVED", "p", "-1", "TEST SHOP")],
                "-1".into(),
                None
            )
            .is_err()
    );
    assert!(rows(&v).is_empty());
}
#[test]
fn a_posting_cannot_settle_two_holds_on_later_syncs() {
    let (v, id, _d) = setup();
    sync(&v, id, vec![body("PENDING", "h1", "-10", "TEST SHOP")], "0").unwrap();
    sync(
        &v,
        id,
        vec![body("APPROVED", "p1", "-10", "TEST SHOP")],
        "-10",
    )
    .unwrap();
    sync(
        &v,
        id,
        vec![body("PENDING", "h2", "-10", "TEST SHOP")],
        "-10",
    )
    .unwrap();
    sync(
        &v,
        id,
        vec![body("APPROVED", "p1", "-10", "TEST SHOP")],
        "-10",
    )
    .unwrap();
    assert_eq!(v.advanzia_open_holds(id).unwrap().len(), 1);
}
#[test]
fn a_still_reported_hold_is_not_settled_and_old_snapshots_do_not_keep_it_alive() {
    let (v, id, _d) = setup();
    let h = body("PENDING", "hold", "-10", "TEST SHOP");
    let p = body("APPROVED", "post", "-10", "TEST SHOP");
    sync(&v, id, vec![h.clone()], "0").unwrap();
    let mut both: Value = serde_json::from_str(&h).unwrap();
    both["transactions"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::from_str::<Value>(&p).unwrap()["transactions"][0].clone());
    sync(&v, id, vec![both.to_string()], "-10").unwrap();
    assert_eq!(v.advanzia_open_holds(id).unwrap().len(), 1);
    sync(&v, id, vec![h, p], "-10").unwrap();
    assert!(v.advanzia_open_holds(id).unwrap().is_empty());
}
#[test]
fn ambiguous_postings_do_not_settle_one_hold() {
    let (v, id, _d) = setup();
    sync(
        &v,
        id,
        vec![body("PENDING", "hold", "-10", "TEST SHOP")],
        "0",
    )
    .unwrap();
    sync(
        &v,
        id,
        vec![
            body("APPROVED", "p1", "-10", "TEST SHOP"),
            body("APPROVED", "p2", "-10", "TEST SHOP"),
        ],
        "-20",
    )
    .unwrap();
    assert_eq!(v.advanzia_open_holds(id).unwrap().len(), 1);
}
#[test]
fn category_is_preserved_on_a_unique_settlement() {
    let (v, id, _d) = setup();
    sync(
        &v,
        id,
        vec![body("PENDING", "hold", "-10", "TEST SHOP")],
        "0",
    )
    .unwrap();
    let category = {
        let db = v.db().unwrap();
        db.execute(
            "INSERT INTO categories(name) VALUES ('Invented category')",
            [],
        )
        .unwrap();
        let cat = db.last_insert_rowid();
        db.execute(
            "UPDATE transactions SET category_id=?1,category_source='manual'",
            [cat],
        )
        .unwrap();
        cat
    };
    sync(
        &v,
        id,
        vec![body("APPROVED", "post", "-10", "TEST SHOP")],
        "-10",
    )
    .unwrap();
    let db = v.db().unwrap();
    let cat: i64 = db
        .query_row(
            "SELECT category_id FROM transactions WHERE external_id LIKE 'advanzia-post-%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(cat, category);
}
#[test]
fn archived_account_is_not_changed_by_a_fetch() {
    let (v, id, _d) = setup();
    sync(
        &v,
        id,
        vec![body("APPROVED", "p1", "-10", "TEST SHOP")],
        "-10",
    )
    .unwrap();
    v.db()
        .unwrap()
        .execute("UPDATE accounts SET archived_unix=1", [])
        .unwrap();
    sync(
        &v,
        id,
        vec![body("APPROVED", "p2", "-20", "TEST SHOP")],
        "-30",
    )
    .unwrap();
    assert_eq!(rows(&v).len(), 1);
    assert_eq!(v.list_accounts().unwrap()[0].balance_cents, -1000);
}

#[test]
fn authentication_method_is_persisted_per_connection_and_survives_import() {
    let (v, id, _d) = setup();
    let other = v
        .add_bank_connection(ConnectorKind::Advanzia, "Other".into(), "other-test".into())
        .unwrap();
    assert_eq!(v.advanzia_profile(id).unwrap().auth_method, "sms");
    v.set_advanzia_auth_method(id, "app".into()).unwrap();
    sync(
        &v,
        id,
        vec![body("APPROVED", "test", "-1", "TEST SHOP")],
        "-1",
    )
    .unwrap();
    assert_eq!(v.advanzia_profile(id).unwrap().auth_method, "app");
    assert_eq!(v.advanzia_profile(other).unwrap().auth_method, "sms");
    assert!(v.set_advanzia_auth_method(id, "untrusted".into()).is_err());
    assert_eq!(v.advanzia_profile(id).unwrap().auth_method, "app");
}

#[test]
fn bank_picker_finds_the_supported_portal_without_claiming_all_mastercards() {
    for query in ["Advanzia", "mein.advanzia.com"] {
        assert!(
            super::super::bank_directory::search(query, vec![], 10)
                .iter()
                .any(|bank| bank.connector == Some(ConnectorKind::Advanzia))
        );
    }
    assert!(
        !super::super::bank_directory::search("Gebuehrenfrei", vec![], 10)
            .iter()
            .any(|bank| bank.connector == Some(ConnectorKind::Advanzia))
    );
}
