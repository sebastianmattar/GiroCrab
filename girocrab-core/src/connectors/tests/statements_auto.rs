use crate::connectors::*;
use crate::vault::Vault;

#[test]
fn auto_statements_are_due_weekly_and_only_when_enabled() {
    let dir = std::env::temp_dir().join(format!("gc-stmt-auto-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let v = Vault::open(dir.join("v.db").to_string_lossy().into(), vec![4u8; 32]).unwrap();
    let (conn, giro, loan) = {
        let c = v.db().unwrap();
        c.execute(
            "INSERT INTO bank_connections(connector, login_name, display_name) VALUES ('fints:12345678', 'x', 'Bank')",
            [],
        )
        .unwrap();
        let conn = c.last_insert_rowid();
        c.execute("INSERT INTO account_groups(name) VALUES ('G')", [])
            .unwrap();
        c.execute(
            "INSERT INTO accounts(group_id, connection_id, external_ref, name) VALUES ((SELECT id FROM account_groups), ?1, 'giro', 'Giro')",
            [conn],
        )
        .unwrap();
        let giro = c.last_insert_rowid();
        c.execute(
            "INSERT INTO accounts(group_id, connection_id, external_ref, name, kind) VALUES ((SELECT id FROM account_groups), ?1, 'loan', 'Kredit', 'loan')",
            [conn],
        )
        .unwrap();
        (conn, giro, c.last_insert_rowid())
    };
    assert!(v.statements_due(conn).unwrap().is_empty());
    v.set_auto_statements(giro, true).unwrap();
    v.set_auto_statements(loan, true).unwrap();
    assert!(v.auto_statements_enabled(giro).unwrap());
    assert_eq!(v.statements_due(conn).unwrap(), vec!["giro".to_string()]);
    // fetched today -> not due; 8 days ago -> due again
    v.set_setting(statements_last_key(giro), today()).unwrap();
    assert!(v.statements_due(conn).unwrap().is_empty());
    let old = crate::dates::iso(crate::dates::today() - crate::dates::days(8));
    v.set_setting(statements_last_key(giro), old).unwrap();
    assert_eq!(v.statements_due(conn).unwrap().len(), 1);
    // a regular sync asks for the due ones, an explicit fetch only for its account
    assert_eq!(
        v.documents_request(conn, None).unwrap(),
        Documents::On {
            statements: vec!["giro".into()],
            catchup_since: None,
        }
    );
    assert_eq!(
        v.documents_request(conn, Some("loan".into())).unwrap(),
        Documents::StatementsOnly("loan".into())
    );
    v.set_auto_statements(giro, false).unwrap();
    assert!(!v.auto_statements_enabled(giro).unwrap());
    assert!(v.statements_due(conn).unwrap().is_empty());
    drop(v);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn document_retrieval_off_and_back_on() {
    let dir = std::env::temp_dir().join(format!("gc-docs-request-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let v = Vault::open(dir.join("v.db").to_string_lossy().into(), vec![4u8; 32]).unwrap();
    let conn = {
        let c = v.db().unwrap();
        c.execute(
            "INSERT INTO bank_connections(connector, login_name, display_name) VALUES ('fints:12345678', 'x', 'Bank')",
            [],
        )
        .unwrap();
        c.last_insert_rowid()
    };
    v.set_documents_fetch(conn, false).unwrap();
    assert_eq!(v.documents_request(conn, None).unwrap(), Documents::Off);
    // an explicit statements fetch would bring nothing that is stored
    assert!(v.documents_request(conn, Some("giro".into())).is_err());
    v.set_documents_fetch(conn, true).unwrap();
    let request = v.documents_request(conn, None).unwrap();
    assert!(request.enabled());
    assert_eq!(request.catchup_since(), Some(today().as_str()));
    assert!(request.due_statements().is_empty());
    drop(v);
    let _ = std::fs::remove_dir_all(dir);
}
