use std::collections::HashMap;

use rsa::PrivateKey;
use rsa::pkcs8::{EncodePrivateKey, LineEnding};

use super::*;
use crate::connectors::enable_banking::EnableBankingConnector;

fn vault(name: &str) -> (crate::test_support::TempDir, std::sync::Arc<Vault>) {
    let dir = crate::test_support::TempDir::new(name);
    let v = Vault::open(
        dir.join("v.db").to_string_lossy().to_string(),
        vec![7u8; 32],
    )
    .unwrap();
    (dir, v)
}

fn pem() -> String {
    let key = PrivateKey::new(&mut getrandom::SysRng, 1024).unwrap();
    key.to_pkcs8_pem(LineEnding::LF).unwrap().to_string()
}

fn login(v: &Vault, connector: &str, name: &str) -> i64 {
    let db = v.db().unwrap();
    db.execute(
        "INSERT INTO bank_connections(connector, display_name, login_name) VALUES (?1, ?2, ?2)",
        params![connector, name],
    )
    .unwrap();
    db.last_insert_rowid()
}

fn set_default(v: &Vault, id: &str, pem: &str) {
    v.set_setting(APP_ID_KEY.into(), id.into()).unwrap();
    v.set_setting(PEM_KEY.into(), pem.into()).unwrap();
}

/// `kid` of the JWT the login's sync would sign: the context settings as
/// `begin_sync` builds them (global keys + the login's own overlay).
fn kid_for(v: &Vault, connection_id: i64) -> String {
    let db = v.db().unwrap();
    let mut settings = HashMap::new();
    for k in [APP_ID_KEY, PEM_KEY] {
        if let Some(val) = Vault::setting_inner(&db, k).unwrap() {
            settings.insert(k.to_string(), SecretString::new(val));
        }
    }
    overlay(&db, connection_id, &mut settings).unwrap();
    let jwt = EnableBankingConnector::make_jwt_for(
        settings[APP_ID_KEY].expose(),
        settings[PEM_KEY].expose(),
    )
    .unwrap();
    jsonwebtoken::decode_header(&jwt).unwrap().kid.unwrap()
}

fn set_session(v: &Vault, connection_id: i64) {
    v.db()
        .unwrap()
        .execute(
            "UPDATE bank_connections SET state_json = '{\"session_id\":\"s-1\",\"valid_until\":\"2027-01-01\"}' WHERE id = ?1",
            [connection_id],
        )
        .unwrap();
}

fn has_session(v: &Vault, connection_id: i64) -> bool {
    let state: String = v
        .db()
        .unwrap()
        .query_row(
            "SELECT state_json FROM bank_connections WHERE id = ?1",
            [connection_id],
            |r| r.get(0),
        )
        .unwrap();
    state.contains("session_id")
}

/// The scenario of #824: bank 1 runs with application A, bank 2 is set up
/// with application B. Bank 1 must keep signing with A.
#[test]
fn second_application_does_not_move_existing_logins() {
    let (_d, v) = vault("girocrab-eb-apps-824");
    let (pem_a, pem_b) = (pem(), pem());
    set_default(&v, "app-a", &pem_a);
    let bank1 = login(&v, "enablebanking:DE:C24", "Bank 1");
    assert_eq!(kid_for(&v, bank1), "app-a");

    let bank2 = login(&v, "enablebanking:DE:N26", "Bank 2");
    assert!(
        v.set_connection_enable_banking_app(bank2, "app-b".into(), pem_b.clone())
            .unwrap()
    );
    assert_eq!(kid_for(&v, bank1), "app-a");
    assert_eq!(kid_for(&v, bank2), "app-b");

    // even when B is written over the default (settings form, keychain)
    set_default(&v, "app-b", &pem_b);
    assert_eq!(kid_for(&v, bank1), "app-a");
    assert_eq!(kid_for(&v, bank2), "app-b");
    let app1 = v.connection_enable_banking_app(bank1).unwrap().unwrap();
    assert!(app1.own, "bank 1 was pinned to the old default");
    assert_eq!(app1.application_id, "app-a");
    assert_eq!(
        v.connection_enable_banking_key(bank1).unwrap().as_deref(),
        Some(pem_a.trim())
    );
}

#[test]
fn default_change_pins_only_enable_banking_logins() {
    let (_d, v) = vault("girocrab-eb-apps-pin");
    let pem_a = pem();
    let fints = login(&v, "fints:52090000", "VB");
    let amex_portal = login(&v, "enablebanking:amex", "Amex");
    let generic = login(&v, "enablebanking:DE:C24", "C24");

    // no complete default yet: nothing to pin
    v.set_setting(APP_ID_KEY.into(), "app-a".into()).unwrap();
    v.set_setting(PEM_KEY.into(), pem_a.clone()).unwrap();
    assert!(v.list_connection_enable_banking_apps().unwrap().is_empty());

    // same value again: no change, nothing pinned
    v.set_setting(APP_ID_KEY.into(), "app-a".into()).unwrap();
    assert!(v.list_connection_enable_banking_apps().unwrap().is_empty());

    v.set_setting(APP_ID_KEY.into(), "app-b".into()).unwrap();
    let own: Vec<i64> = v
        .list_connection_enable_banking_apps()
        .unwrap()
        .into_iter()
        .map(|a| a.connection_id)
        .collect();
    assert_eq!(own, vec![generic]);
    for id in [fints, amex_portal] {
        assert!(!v.connection_enable_banking_app(id).unwrap().unwrap().own);
    }
}

#[test]
fn card_switched_to_psd2_is_pinned() {
    let (_d, v) = vault("girocrab-eb-apps-card");
    set_default(&v, "app-a", &pem());
    let visa = login(&v, "enablebanking:amazonvisa", "Visa");
    v.set_setting("enablebanking.use.amazonvisa".into(), "1".into())
        .unwrap();
    v.set_setting(APP_ID_KEY.into(), String::new()).unwrap();
    assert_eq!(kid_for_own(&v, visa).as_deref(), Some("app-a"));
}

fn kid_for_own(v: &Vault, connection_id: i64) -> Option<String> {
    let db = v.db().unwrap();
    own_pair(&db, connection_id).unwrap().map(|(id, _)| id)
}

#[test]
fn changing_a_logins_application_drops_only_its_session() {
    let (_d, v) = vault("girocrab-eb-apps-session");
    let pem_a = pem();
    set_default(&v, "app-a", &pem_a);
    let bank1 = login(&v, "enablebanking:DE:C24", "Bank 1");
    let bank2 = login(&v, "enablebanking:DE:N26", "Bank 2");
    set_session(&v, bank1);
    set_session(&v, bank2);

    // the same application as before: nothing changes
    assert!(
        !v.set_connection_enable_banking_app(bank1, " app-a ".into(), pem_a.clone())
            .unwrap()
    );
    assert!(has_session(&v, bank1));

    assert!(
        v.set_connection_enable_banking_app(bank1, "app-b".into(), pem())
            .unwrap()
    );
    assert!(!has_session(&v, bank1));
    assert!(has_session(&v, bank2));

    // back to the default
    assert!(
        v.set_connection_enable_banking_app(bank1, String::new(), String::new())
            .unwrap()
    );
    let app = v.connection_enable_banking_app(bank1).unwrap().unwrap();
    assert!(!app.own);
    assert_eq!(app.application_id, "app-a");
}

#[test]
fn invalid_input_is_rejected_and_stores_nothing() {
    let (_d, v) = vault("girocrab-eb-apps-invalid");
    let bank = login(&v, "enablebanking:DE:C24", "Bank");
    assert!(matches!(
        v.set_connection_enable_banking_app(bank, "app-a".into(), String::new()),
        Err(CoreError::InvalidInput { .. })
    ));
    assert!(matches!(
        v.set_connection_enable_banking_app(bank, "app-a".into(), "not a key".into()),
        Err(CoreError::InvalidInput { .. })
    ));
    assert!(matches!(
        v.set_connection_enable_banking_app(9999, "app-a".into(), pem()),
        Err(CoreError::NotFound { .. })
    ));
    assert!(v.connection_enable_banking_app(bank).unwrap().is_none());
}

#[test]
fn bank_list_falls_back_to_a_logins_application() {
    let (_d, v) = vault("girocrab-eb-apps-any");
    let db_none = { any_app(&v.db().unwrap()).unwrap() };
    assert!(db_none.is_none());
    let bank = login(&v, "enablebanking:DE:C24", "Bank");
    v.set_connection_enable_banking_app(bank, "app-own".into(), pem())
        .unwrap();
    assert_eq!(any_app(&v.db().unwrap()).unwrap().unwrap().0, "app-own");
    set_default(&v, "app-default", &pem());
    assert_eq!(any_app(&v.db().unwrap()).unwrap().unwrap().0, "app-default");
}

#[test]
fn deleting_and_merging_logins_moves_or_removes_their_application() {
    let (_d, v) = vault("girocrab-eb-apps-delete");
    let bank = login(&v, "enablebanking:DE:C24", "Bank");
    v.set_connection_enable_banking_app(bank, "app-a".into(), pem())
        .unwrap();
    v.delete_bank_connection(bank).unwrap();
    assert!(own_pair(&v.db().unwrap(), bank).unwrap().is_none());

    // duplicate logins: the kept one takes over the application
    let keep = login(&v, "enablebanking:DE:N26", "same");
    let dup = login(&v, "enablebanking:DE:N26", "same");
    v.set_connection_enable_banking_app(dup, "app-dup".into(), pem())
        .unwrap();
    assert_eq!(v.deduplicate_bank_connections().unwrap(), 1);
    let db = v.db().unwrap();
    assert_eq!(own_pair(&db, keep).unwrap().unwrap().0, "app-dup");
    assert!(own_pair(&db, dup).unwrap().is_none());
}

#[test]
fn own_private_key_is_stored_as_secret() {
    let (_d, v) = vault("girocrab-eb-apps-secret");
    let bank = login(&v, "enablebanking:DE:C24", "Bank");
    v.set_connection_enable_banking_app(bank, "app-a".into(), pem())
        .unwrap();
    let [id_key, pem_key] = own_keys(bank);
    let db = v.db().unwrap();
    let secret = |k: &str| -> i64 {
        db.query_row("SELECT is_secret FROM settings WHERE key = ?1", [k], |r| {
            r.get(0)
        })
        .unwrap()
    };
    assert_eq!(secret(&pem_key), 1);
    assert_eq!(secret(&id_key), 0);
}
