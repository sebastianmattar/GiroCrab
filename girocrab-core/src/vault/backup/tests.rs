use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::Connection;

use crate::error::CoreError;
use crate::vault::{SCHEMA_VERSION, Vault};

const PASSWORD: &str = "Krabbe am Strand 7";

fn temp_path(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("girocrab-backup-{tag}-{}.db", uuid::Uuid::new_v4()))
}

fn path(p: &Path) -> String {
    p.to_string_lossy().to_string()
}

/// A vault with demo data (accounts, bookings) under key `fill`.
fn filled(fill: u8) -> (Arc<Vault>, PathBuf) {
    let p = temp_path("src");
    let v = Vault::open(path(&p), vec![fill; 32]).unwrap();
    v.seed_demo_data().unwrap();
    (v, p)
}

fn bookings(v: &Vault) -> i64 {
    v.db()
        .unwrap()
        .query_row("SELECT count(*) FROM transactions", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn restore_brings_back_accounts_and_bookings_under_the_device_key() {
    let (src, _) = filled(1);
    let backup = temp_path("bak");
    src.write_backup(path(&backup), PASSWORD.into()).unwrap();

    // another device: its own key, an empty vault
    let dest_path = temp_path("dest");
    let dest = Vault::open(path(&dest_path), vec![2u8; 32]).unwrap();
    assert_eq!(dest.account_count().unwrap(), 0);
    dest.restore_backup(path(&backup), PASSWORD.into()).unwrap();
    assert_eq!(dest.account_count().unwrap(), src.account_count().unwrap());
    assert_eq!(bookings(&dest), bookings(&src));
    assert!(bookings(&dest) > 0);

    // on disk, under the device key, after reopening
    drop(dest);
    let reopened = Vault::open(path(&dest_path), vec![2u8; 32]).unwrap();
    assert_eq!(
        reopened.account_count().unwrap(),
        src.account_count().unwrap()
    );
}

#[test]
fn restore_replaces_what_the_vault_held() {
    let (src, _) = filled(1);
    let backup = temp_path("bak");
    let empty = Vault::open(path(&temp_path("empty")), vec![3u8; 32]).unwrap();
    empty.write_backup(path(&backup), PASSWORD.into()).unwrap();
    src.restore_backup(path(&backup), PASSWORD.into()).unwrap();
    assert_eq!(src.account_count().unwrap(), 0);
    assert_eq!(bookings(&src), 0);
}

#[test]
fn backup_needs_the_password_not_the_vault_key() {
    let (src, _) = filled(1);
    let backup = temp_path("bak");
    src.write_backup(path(&backup), PASSWORD.into()).unwrap();
    assert!(std::fs::metadata(&backup).unwrap().len() > 0);
    assert!(Vault::open(path(&backup), vec![1u8; 32]).is_err());
}

#[test]
fn wrong_password_is_refused_and_the_vault_stays() {
    let (src, _) = filled(1);
    let backup = temp_path("bak");
    src.write_backup(path(&backup), PASSWORD.into()).unwrap();
    let dest = Vault::open(path(&temp_path("dest")), vec![2u8; 32]).unwrap();
    dest.seed_demo_data().unwrap();
    let before = bookings(&dest);
    let err = dest
        .restore_backup(path(&backup), "falsch".into())
        .unwrap_err();
    assert!(matches!(err, CoreError::InvalidInput { .. }), "{err:?}");
    assert_eq!(bookings(&dest), before);
}

#[test]
fn not_a_backup_is_refused() {
    let junk = temp_path("junk");
    std::fs::write(&junk, b"kein Tresor").unwrap();
    let dest = Vault::open(path(&temp_path("dest")), vec![2u8; 32]).unwrap();
    let err = dest
        .restore_backup(path(&junk), PASSWORD.into())
        .unwrap_err();
    assert!(matches!(err, CoreError::InvalidInput { .. }), "{err:?}");
}

fn set_user_version(db: &Path, password: &str, version: i64) {
    let c = Connection::open(db).unwrap();
    c.pragma_update(None, "key", password).unwrap();
    c.execute_batch(&format!("PRAGMA user_version = {version}"))
        .unwrap();
}

/// Another app's encrypted database opened with its own password also holds
/// `accounts` and `transactions`: it is named as such, never restored, also
/// when its version number is above ours.
#[test]
fn other_apps_database_is_refused_with_its_own_message() {
    use crate::importers::moneymoney_db::tests::{PW, fake_db};
    let (dest, _) = filled(2);
    let before = bookings(&dest);
    let db = fake_db();
    for version in [None, Some(SCHEMA_VERSION + 1)] {
        if let Some(v) = version {
            set_user_version(Path::new(&db), PW, v);
        }
        let err = dest.restore_backup(db.clone(), PW.into()).unwrap_err();
        assert!(
            matches!(&err, CoreError::InvalidInput { message } if message.contains("anderen App")),
            "{err:?}"
        );
        assert_eq!(bookings(&dest), before);
    }
}

#[test]
fn empty_password_is_refused() {
    let (src, _) = filled(1);
    let err = src
        .write_backup(path(&temp_path("bak")), String::new())
        .unwrap_err();
    assert!(matches!(err, CoreError::InvalidInput { .. }), "{err:?}");
}

#[test]
fn backup_from_a_newer_app_is_refused() {
    let (src, _) = filled(1);
    let backup = temp_path("bak");
    src.write_backup(path(&backup), PASSWORD.into()).unwrap();
    set_user_version(&backup, PASSWORD, SCHEMA_VERSION + 1);
    let dest = Vault::open(path(&temp_path("dest")), vec![2u8; 32]).unwrap();
    let err = dest
        .restore_backup(path(&backup), PASSWORD.into())
        .unwrap_err();
    assert!(matches!(err, CoreError::SchemaTooNew { .. }), "{err:?}");
    assert_eq!(dest.account_count().unwrap(), 0);
}

fn forget_marker(v: &Vault) {
    v.db()
        .unwrap()
        .execute(
            "DELETE FROM settings WHERE key = 'pre_0_8_0_fints_backup_done'",
            [],
        )
        .unwrap();
}

#[test]
fn pre_fints_reg_backup_is_made_once_and_never_overwritten() {
    let key = vec![5u8; 32];
    let p = temp_path("prefints");
    let bak = Vault::pre_fints_reg_backup_path(&path(&p));

    // new vault: nothing to preserve, only the marker is set
    let v = Vault::open(path(&p), key.clone()).unwrap();
    assert!(!Path::new(&bak).exists());
    assert!(
        v.get_setting("pre_0_8_0_fints_backup_done".into())
            .unwrap()
            .is_some()
    );

    // an existing vault from before the update: has data, no marker
    v.seed_demo_data().unwrap();
    let before = bookings(&v);
    assert!(before > 0);
    forget_marker(&v);
    drop(v);

    let v = Vault::open(path(&p), key.clone()).unwrap();
    assert!(Path::new(&bak).exists(), "backup written on first open");
    assert!(!Path::new(&format!("{bak}.tmp")).exists());
    assert!(
        v.get_setting("pre_0_8_0_fints_backup_done".into())
            .unwrap()
            .is_some()
    );
    let copy = Vault::open(bak.clone(), key.clone()).unwrap();
    assert_eq!(bookings(&copy), before);
    drop(copy);

    // later changes must not touch the safety copy, with or without marker
    v.db()
        .unwrap()
        .execute("DELETE FROM transactions", [])
        .unwrap();
    drop(v);
    let v = Vault::open(path(&p), key.clone()).unwrap();
    forget_marker(&v);
    drop(v);
    let v = Vault::open(path(&p), key.clone()).unwrap();
    drop(v);
    let copy = Vault::open(bak, key).unwrap();
    assert_eq!(bookings(&copy), before, "backup was not overwritten");
}

#[test]
fn short_password_is_refused() {
    let (src, _) = filled(1);
    let err = src
        .write_backup(path(&temp_path("bak")), "1234567".into())
        .unwrap_err();
    assert!(matches!(err, CoreError::InvalidInput { .. }), "{err:?}");
}

#[test]
fn restore_keeps_this_devices_endpoints_pins_and_fetch_interval() {
    let (src, _) = filled(1);
    for (k, v) in [
        ("fints.url.12345678", "https://evil.example/fints"),
        ("pin.conn.1", "backup-pin"),
        ("sync.psd2_auto_hours", "1"),
        ("fints.tan_medium.1", "Evil"),
        ("ui.note", "from backup"),
    ] {
        src.set_setting(k.into(), v.into()).unwrap();
    }
    let backup = temp_path("bak");
    src.write_backup(path(&backup), PASSWORD.into()).unwrap();

    let (dst, _) = filled(2);
    dst.set_setting(
        "fints.url.12345678".into(),
        "https://bank.example/fints".into(),
    )
    .unwrap();
    dst.set_setting("sync.psd2_auto_hours".into(), "6".into())
        .unwrap();
    dst.restore_backup(path(&backup), PASSWORD.into()).unwrap();

    let get = |k: &str| dst.get_setting(k.into()).unwrap();
    assert_eq!(
        get("fints.url.12345678").as_deref(),
        Some("https://bank.example/fints")
    );
    assert_eq!(get("sync.psd2_auto_hours").as_deref(), Some("6"));
    assert_eq!(get("pin.conn.1"), None);
    assert_eq!(get("fints.tan_medium.1"), None);
    assert_eq!(get("ui.note").as_deref(), Some("from backup"));
}

#[test]
fn checkpoint_succeeds_on_an_idle_vault() {
    let (src, _) = filled(1);
    src.checkpoint().unwrap();
}

#[cfg(unix)]
#[test]
fn backup_file_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let (src, _) = filled(1);
    let backup = temp_path("bak");
    src.write_backup(path(&backup), PASSWORD.into()).unwrap();
    let mode = std::fs::metadata(&backup).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

fn triggers_and_views(v: &Vault) -> Vec<String> {
    v.db()
        .unwrap()
        .prepare("SELECT name FROM sqlite_master WHERE type IN ('trigger', 'view') ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn restore_drops_triggers_and_views_from_the_backup() {
    let (src, _) = filled(1);
    src.db()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER repoint AFTER INSERT ON settings BEGIN
                 UPDATE settings SET value = 'https://evil.example/fints'
                 WHERE key LIKE 'fints.url.%';
             END;
             CREATE TRIGGER repoint_on_sync AFTER UPDATE ON bank_connections BEGIN
                 UPDATE settings SET value = 'https://evil.example/fints'
                 WHERE key LIKE 'fints.url.%';
             END;
             CREATE VIEW extra AS SELECT 1;",
        )
        .unwrap();
    let backup = temp_path("bak");
    src.write_backup(path(&backup), PASSWORD.into()).unwrap();

    let (dst, dst_path) = filled(2);
    dst.set_setting(
        "fints.url.12345678".into(),
        "https://bank.example/fints".into(),
    )
    .unwrap();
    dst.restore_backup(path(&backup), PASSWORD.into()).unwrap();
    assert!(triggers_and_views(&dst).is_empty());

    // a later settings write keeps this device's endpoint
    dst.set_setting("ui.note".into(), "x".into()).unwrap();
    assert_eq!(
        dst.get_setting("fints.url.12345678".into())
            .unwrap()
            .as_deref(),
        Some("https://bank.example/fints")
    );
    drop(dst);
    let reopened = Vault::open(path(&dst_path), vec![2; 32]).unwrap();
    assert!(triggers_and_views(&reopened).is_empty());
}

#[test]
fn reopen_drops_triggers_left_in_the_vault() {
    let (v, p) = filled(3);
    v.db()
        .unwrap()
        .execute_batch("CREATE TRIGGER stray AFTER INSERT ON settings BEGIN SELECT 1; END;")
        .unwrap();
    drop(v);
    let reopened = Vault::open(path(&p), vec![3; 32]).unwrap();
    assert!(triggers_and_views(&reopened).is_empty());
}
