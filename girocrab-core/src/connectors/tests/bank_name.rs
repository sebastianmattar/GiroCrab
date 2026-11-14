use crate::connectors::*;
use crate::models::{AccountKind, ConnectorKind};
use crate::vault::Vault;

fn fetch() -> FetchResult {
    FetchResult {
        accounts: vec![RemoteAccount {
            external_ref: "DE1".into(),
            name: "Giro".into(),
            owner: String::new(),
            iban: String::new(),
            account_number: String::new(),
            currency: "EUR".into(),
            kind: AccountKind::Checking,
            balance_cents: 0,
            balance_date: "2026-11-10".into(),
        }],
        ..Default::default()
    }
}

fn bank_name(v: &Vault) -> String {
    v.list_accounts().unwrap()[0].bank_name.clone()
}

/// FinTS sends no bank name: accounts show the connection's display name
/// (not the generic "FinTS-Bank") and follow a later rename (#355).
#[test]
fn fints_accounts_use_and_follow_the_display_name() {
    let p_dir = crate::test_support::TempDir::new("girocrab-bankname");
    let v = Vault::open(
        p_dir.join("v.db").to_string_lossy().to_string(),
        vec![6u8; 32],
    )
    .unwrap();
    let conn = {
        let db = v.db().unwrap();
        db.execute(
            "INSERT INTO bank_connections(connector, display_name, login_name) VALUES ('fints:12345678', 'Sparkasse Musterstadt', 'x')",
            [],
        )
        .unwrap();
        db.last_insert_rowid()
    };
    store_fetch(&v, ConnectorKind::FinTsGeneric, conn, fetch()).unwrap();
    assert_eq!(bank_name(&v), "Sparkasse Musterstadt");

    v.update_bank_connection(conn, "Meine Sparkasse".into(), "x".into())
        .unwrap();
    assert_eq!(bank_name(&v), "Meine Sparkasse");

    // accounts stored by an older version with the generic name are repaired
    v.db()
        .unwrap()
        .execute("UPDATE accounts SET bank_name = 'FinTS-Bank'", [])
        .unwrap();
    store_fetch(&v, ConnectorKind::FinTsGeneric, conn, fetch()).unwrap();
    assert_eq!(bank_name(&v), "Meine Sparkasse");

    // a name the user never gave the connection is kept on rename
    v.db()
        .unwrap()
        .execute("UPDATE accounts SET bank_name = 'Aus MoneyMoney'", [])
        .unwrap();
    v.update_bank_connection(conn, "Spk".into(), "x".into())
        .unwrap();
    assert_eq!(bank_name(&v), "Aus MoneyMoney");
}
