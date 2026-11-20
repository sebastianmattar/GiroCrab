use super::tests::{body, rows, setup};
use super::*;
const BANK: &str = "00000000-0000-4000-8000-000000000001";
fn legacy(v: &Vault, id: i64) -> i64 {
    let db = v.db().unwrap();
    db.execute(
        "INSERT INTO account_groups(name) VALUES ('Fictional group')",
        [],
    )
    .unwrap();
    let group = db.last_insert_rowid();
    db.execute("INSERT INTO accounts(connection_id,group_id,external_ref,name,owner,kind,currency,balance_cents) VALUES (?1,?2,?3,'Existing test card','Invented owner','credit_card','EUR',-2000)", params![id, group, format!("portal-advanzia-{id}")]).unwrap();
    db.last_insert_rowid()
}
fn old_row(v: &Vault, account: i64, pending: bool, reference: &str) -> i64 {
    let db = v.db().unwrap();
    db.execute("INSERT INTO transactions(account_id,external_id,booking_date,value_date,amount_cents,currency,counterparty,booking_text,note,checked) VALUES (?1,?2,'2026-11-12','2026-11-12',-2098,'EUR','TEST SHOP','Kartenumsatz','invented note',1)",params![account, compatibility::legacy_id(pending, reference)]).unwrap();
    db.last_insert_rowid()
}
fn import(
    v: &Arc<Vault>,
    id: i64,
    target: Option<i64>,
    captures: Vec<String>,
) -> CoreResult<SyncSummary> {
    v.clone().import_advanzia_capture(
        id,
        BANK.into(),
        v.advanzia_profile(id)?.profile_id,
        captures,
        "-20.98".into(),
        target,
    )
}
fn posted(reference: &str) -> String {
    body("APPROVED", reference, "-20.98", "TEST SHOP")
}
fn pending(reference: &str) -> String {
    body("PENDING", reference, "-20.98", "TEST SHOP")
}

#[test]
fn old_hash_matches_the_archived_fnv_contract() {
    assert_eq!(
        compatibility::legacy_id(false, "hello"),
        "advanzia-post-a430d84680aabd0b"
    );
    assert_eq!(
        compatibility::legacy_id(true, "hello"),
        "advanzia-hold-a430d84680aabd0b"
    );
}
#[test]
fn first_adoption_requires_explicit_confirmation_and_preserves_account_identity() {
    let (v, id, _dir) = setup();
    let account = legacy(&v, id);
    let profile = v.advanzia_profile(id).unwrap();
    assert_eq!(profile.legacy_account_id, Some(account));
    assert!(!profile.has_account);
    assert!(import(&v, id, None, vec![posted("old")]).is_err());
    assert!(rows(&v).is_empty());
    assert_eq!(v.list_accounts().unwrap()[0].balance_cents, -2000);
    import(&v, id, Some(account), vec![posted("old")]).unwrap();
    let profile = v.advanzia_profile(id).unwrap();
    assert!(profile.has_account);
    assert_eq!(profile.bank_account_id.as_deref(), Some(BANK));
    let accounts = v.list_accounts().unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].id, account);
    assert_eq!(accounts[0].name, "Existing test card");
    let reference: String = v
        .db()
        .unwrap()
        .query_row(
            "SELECT external_ref FROM accounts WHERE id=?1",
            [account],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(reference, format!("portal-advanzia-{id}"));
    let state: String = v
        .db()
        .unwrap()
        .query_row(
            "SELECT state_json FROM bank_connections WHERE id=?1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!state.contains("import_aliases"));
    assert!(!state.contains("target_account_id"));
}
#[test]
fn old_postings_keep_their_ids_edits_splits_and_documents_without_duplicates() {
    let (v, id, _dir) = setup();
    let account = legacy(&v, id);
    let row = old_row(&v, account, false, "old");
    {
        let db = v.db().unwrap();
        db.execute("INSERT INTO transaction_splits(transaction_id,amount_cents,note) VALUES (?1,-2098,'invented split')",[row]).unwrap();
        db.execute("INSERT INTO booking_documents(booking_id,account_id,title,pdf_bytes) VALUES (?1,?2,'fictional attachment',X'00')",params![row,account]).unwrap();
    }
    assert_eq!(
        import(&v, id, Some(account), vec![posted("old"), posted("new")])
            .unwrap()
            .transactions_imported,
        1
    );
    assert_eq!(
        import(&v, id, None, vec![posted("old"), posted("new")])
            .unwrap()
            .transactions_imported,
        0
    );
    let stored = rows(&v);
    assert_eq!(stored.len(), 2);
    assert_eq!(stored[0].0, row);
    assert!(
        stored
            .iter()
            .all(|r| r.1.len() == "advanzia-post-".len() + 16)
    );
    let db = v.db().unwrap();
    let edits: (String, i64, i64, i64) = db.query_row("SELECT note,checked,(SELECT count(*) FROM transaction_splits WHERE transaction_id=t.id),(SELECT count(*) FROM booking_documents WHERE booking_id=t.id) FROM transactions t WHERE id=?1",[row],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
    assert_eq!(edits, ("invented note".into(), 1, 1, 1));
    // Simulate a subsequent archived-version import: its exact ID is still present.
    assert_eq!(db.execute("INSERT OR IGNORE INTO transactions(account_id,external_id,booking_date,amount_cents) VALUES (?1,?2,'2026-11-12',-2098)",params![account,compatibility::legacy_id(false,"new")]).unwrap(),0);
}
#[test]
fn old_holds_are_pending_and_settle_once_with_their_metadata() {
    let (v, id, _dir) = setup();
    let account = legacy(&v, id);
    let hold = old_row(&v, account, true, "hold");
    import(&v, id, Some(account), vec![pending("hold")]).unwrap();
    assert_eq!(rows(&v).len(), 1);
    let page = v
        .list_transactions(crate::models::BookingFilter {
            account_ids: vec![account],
            ..Default::default()
        })
        .unwrap();
    assert!(page.transactions[0].is_pending);
    assert_eq!(v.advanzia_open_holds(id).unwrap().len(), 1);
    import(&v, id, None, vec![posted("post")]).unwrap();
    let stored = rows(&v);
    assert_eq!(stored.len(), 2);
    assert!(stored[0].3.is_some());
    assert!(stored[1].3.is_none());
    let db = v.db().unwrap();
    let edits: (String, i64) = db
        .query_row(
            "SELECT note,checked FROM transactions WHERE id=?1",
            [stored[1].0],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(edits, ("invented note".into(), 1));
    drop(db);
    import(&v, id, None, vec![pending("hold"), posted("post")]).unwrap();
    assert_eq!(rows(&v).len(), 2);
    assert!(rows(&v).iter().find(|r| r.0 == hold).unwrap().3.is_some());
}
#[test]
fn old_deleted_postings_and_discarded_holds_stay_deleted() {
    let (v, id, _dir) = setup();
    let account = legacy(&v, id);
    let post = old_row(&v, account, false, "post");
    let hold = old_row(&v, account, true, "hold");
    {
        let db = v.db().unwrap();
        crate::sync_merge::soft_delete_booking(&db, post).unwrap();
    }
    import(&v, id, Some(account), vec![posted("post"), pending("hold")]).unwrap();
    v.discard_advanzia_hold(id, hold).unwrap();
    import(&v, id, None, vec![posted("post"), pending("hold")]).unwrap();
    assert_eq!(rows(&v).len(), 2);
    assert!(rows(&v).iter().all(|r| r.3.is_some()));
}
#[test]
fn invalid_balance_or_late_failure_does_not_partially_adopt_the_account() {
    let (v, id, _dir) = setup();
    let account = legacy(&v, id);
    assert!(
        v.clone()
            .import_advanzia_capture(
                id,
                BANK.into(),
                v.advanzia_profile(id).unwrap().profile_id,
                vec![posted("p")],
                "null".into(),
                Some(account)
            )
            .is_err()
    );
    v.db().unwrap().execute_batch("CREATE TRIGGER reject_fake_import BEFORE INSERT ON transactions BEGIN SELECT RAISE(ABORT,'fictional failure'); END;").unwrap();
    assert!(import(&v, id, Some(account), vec![posted("p")]).is_err());
    assert!(!v.advanzia_profile(id).unwrap().has_account);
    assert!(rows(&v).is_empty());
    assert_eq!(v.list_accounts().unwrap()[0].balance_cents, -2000);
}
#[test]
fn foreign_target_or_different_bank_uuid_cannot_reassign_the_legacy_account() {
    let (v, id, _dir) = setup();
    let account = legacy(&v, id);
    let other = v
        .add_bank_connection(
            ConnectorKind::Advanzia,
            "Other test".into(),
            "another-fictional-login".into(),
        )
        .unwrap();
    assert!(import(&v, other, Some(account), vec![posted("p")]).is_err());
    import(&v, id, Some(account), vec![posted("p")]).unwrap();
    assert!(
        v.clone()
            .import_advanzia_capture(
                id,
                "00000000-0000-4000-8000-000000000002".into(),
                v.advanzia_profile(id).unwrap().profile_id,
                vec![posted("q")],
                "0".into(),
                None
            )
            .is_err()
    );
    assert_eq!(rows(&v).len(), 1);
    assert_eq!(v.list_accounts().unwrap()[0].balance_cents, -2098);
}
#[test]
fn conflicting_legacy_reference_never_silently_discards_bank_rows() {
    let (v, id, _dir) = setup();
    let account = legacy(&v, id);
    old_row(&v, account, false, "p");
    assert!(
        import(
            &v,
            id,
            Some(account),
            vec![body("APPROVED", "p", "-99", "TEST SHOP")]
        )
        .is_err()
    );
    assert_eq!(rows(&v).len(), 1);
    assert!(!v.advanzia_profile(id).unwrap().has_account);
}
#[test]
fn unknown_multiple_archived_or_deleted_accounts_are_not_automatically_adopted() {
    for change in [
        "external_ref='unrecognized-test'",
        "archived_unix=1",
        "deleted_ms=1",
        "currency='USD'",
        "kind='checking'",
    ] {
        let (v, id, _dir) = setup();
        let account = legacy(&v, id);
        v.db()
            .unwrap()
            .execute(
                &format!("UPDATE accounts SET {change} WHERE id=?1"),
                [account],
            )
            .unwrap();
        assert_eq!(v.advanzia_profile(id).unwrap().legacy_account_id, None);
        assert!(import(&v, id, Some(account), vec![posted("p")]).is_err());
        assert!(rows(&v).is_empty());
    }
}

#[test]
fn a_current_account_can_reuse_exact_legacy_booking_ids() {
    let (v, id, _dir) = setup();
    let account = legacy(&v, id);
    old_row(&v, account, false, "old");
    v.db()
        .unwrap()
        .execute(
            "UPDATE accounts SET external_ref=?1 WHERE id=?2",
            params![account_ref(BANK), account],
        )
        .unwrap();
    assert_eq!(
        import(&v, id, None, vec![posted("old"), posted("new")])
            .unwrap()
            .transactions_imported,
        1
    );
    assert_eq!(
        import(&v, id, None, vec![posted("old"), posted("new")])
            .unwrap()
            .transactions_imported,
        0
    );
    assert_eq!(rows(&v).len(), 2);
    assert!(
        rows(&v)
            .iter()
            .any(|r| r.1 == compatibility::legacy_id(false, "old"))
    );
}
#[test]
fn two_existing_identifiers_for_one_reference_fail_without_choosing_a_row() {
    let (v, id, _dir) = setup();
    let account = legacy(&v, id);
    old_row(&v, account, false, "old");
    let canonical = parser::parse_captures(&[posted("old")]).unwrap()[0]
        .id
        .clone();
    v.db().unwrap().execute("INSERT INTO transactions(account_id,external_id,booking_date,amount_cents,currency) VALUES (?1,?2,'2026-11-12',-2098,'EUR')",params![account,canonical]).unwrap();
    assert!(import(&v, id, Some(account), vec![posted("old"), posted("new")]).is_err());
    assert_eq!(rows(&v).len(), 2);
    assert!(!v.advanzia_profile(id).unwrap().has_account);
}
#[test]
fn multiple_bound_legacy_accounts_cannot_be_silently_combined() {
    let (v, id, _dir) = setup();
    let account = legacy(&v, id);
    v.db().unwrap().execute("INSERT INTO accounts(connection_id,group_id,external_ref,name,kind,currency) SELECT connection_id,group_id,'portal-advanzia-999999','Other fictional card','credit_card','EUR' FROM accounts WHERE id=?1",[account]).unwrap();
    assert_eq!(v.advanzia_profile(id).unwrap().legacy_account_id, None);
    assert!(import(&v, id, Some(account), vec![posted("p")]).is_err());
    assert!(rows(&v).is_empty());
}
#[test]
fn partial_history_keeps_old_holds_and_splits_prevent_automatic_settlement() {
    let (v, id, _dir) = setup();
    let account = legacy(&v, id);
    let hold = old_row(&v, account, true, "hold");
    v.db()
        .unwrap()
        .execute(
            "INSERT INTO transaction_splits(transaction_id,amount_cents) VALUES (?1,-2098)",
            [hold],
        )
        .unwrap();
    import(&v, id, Some(account), vec![r#"{"transactions":[]}"#.into()]).unwrap();
    assert_eq!(v.advanzia_open_holds(id).unwrap().len(), 1);
    import(&v, id, None, vec![posted("new")]).unwrap();
    assert!(rows(&v).iter().all(|r| r.3.is_none()));
    assert!(v.discard_advanzia_hold(id, hold).is_err());
}

#[test]
fn settlement_preserves_tags_on_the_posted_booking() {
    let (v, id, _dir) = setup();
    let account = legacy(&v, id);
    let hold = old_row(&v, account, true, "tagged-hold");
    let tag = {
        let db = v.db().unwrap();
        db.execute("INSERT INTO tags(uuid,name,sort_order,updated_ms) VALUES ('fictional-tag','Fictional tag',0,1)", []).unwrap();
        db.last_insert_rowid()
    };
    v.set_booking_tags(hold, vec![tag]).unwrap();
    import(&v, id, Some(account), vec![posted("posted-with-tag")]).unwrap();
    let db = v.db().unwrap();
    let moved: (i64, i64) = db.query_row(
        "SELECT x.tag_id,t.tags_ms FROM transactions t JOIN transaction_tags x ON x.transaction_id=t.id WHERE t.account_id=?1 AND t.deleted_ms IS NULL",
        [account], |r| Ok((r.get(0)?,r.get(1)?)),
    ).unwrap();
    assert_eq!(moved.0, tag);
    assert!(moved.1 > 1);
    assert!(
        db.query_row(
            "SELECT deleted_ms IS NOT NULL FROM transactions WHERE id=?1",
            [hold],
            |r| r.get::<_, bool>(0)
        )
        .unwrap()
    );
}

#[test]
fn legacy_holds_never_adopt_or_replace_an_imported_posting() {
    for existing_hold in [false, true] {
        let (v, id, _dir) = setup();
        let account = legacy(&v, id);
        let imported = {
            let db = v.db().unwrap();
            db.execute(
                "INSERT INTO transactions(account_id,external_id,booking_date,value_date,amount_cents,currency,counterparty,note)
                 VALUES (?1,'mm-fictional-posting','2026-11-12','2026-11-12',-2098,'EUR','TEST SHOP','keep imported note')",
                [account],
            ).unwrap();
            db.last_insert_rowid()
        };
        if existing_hold {
            let hold = old_row(&v, account, true, "separate-hold");
            v.db()
                .unwrap()
                .execute(
                    "UPDATE transactions SET note='',checked=0 WHERE id=?1",
                    [hold],
                )
                .unwrap();
        }
        // Matching amount, date and merchant must not turn the imported posting
        // into a provisional hold, either on first fetch or when fetched again.
        for target in [Some(account), None] {
            import(&v, id, target, vec![pending("separate-hold")]).unwrap();
            let stored = rows(&v);
            assert_eq!(stored.len(), 2);
            assert!(stored.iter().all(|r| r.3.is_none()));
            assert!(
                stored
                    .iter()
                    .any(|r| r.0 == imported && r.1 == "mm-fictional-posting")
            );
            assert_eq!(v.advanzia_open_holds(id).unwrap().len(), 1);
            let note: String = v
                .db()
                .unwrap()
                .query_row(
                    "SELECT note FROM transactions WHERE id=?1",
                    [imported],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(note, "keep imported note");
        }
    }
}

#[test]
fn readding_a_login_after_deleting_its_legacy_account_imports_to_a_new_account() {
    let (v, id, _dir) = setup();
    let account = legacy(&v, id);
    let old = old_row(&v, account, false, "old-posting");
    v.set_account_archived(account, true).unwrap();
    v.delete_account(account).unwrap();
    let readded = v
        .add_bank_connection(
            ConnectorKind::Advanzia,
            "Testkarte".into(),
            "fictional-login".into(),
        )
        .unwrap();
    assert_eq!(readded, id, "the same login reuses its existing connection");
    import(&v, readded, None, vec![posted("new-posting")]).unwrap();
    let accounts = v.list_accounts().unwrap();
    assert_eq!(accounts.len(), 1);
    assert_ne!(accounts[0].id, account);
    assert!(rows(&v).iter().any(|row| row.0 == old && row.3.is_some()));
    assert!(v.advanzia_profile(id).unwrap().has_account);
    assert_eq!(
        import(&v, id, None, vec![posted("new-posting")])
            .unwrap()
            .transactions_imported,
        0
    );
}
