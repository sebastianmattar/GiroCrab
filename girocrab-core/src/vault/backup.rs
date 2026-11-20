//! „Tresor sichern / wiederherstellen“: a password-protected copy of the vault
//! that opens on any device, also without this device's keychain.
//!
//! The backup is an ordinary SQLCipher database keyed with the password
//! (SQLCipher derives the key with PBKDF2), so no extra crypto is needed.

use std::time::Duration;

use rusqlite::backup::Backup;
use rusqlite::{Connection, params};
use zeroize::Zeroizing;

use super::Vault;
use crate::error::{CoreError, CoreResult};
use crate::i18n::tr;
use crate::schema::has_table;

/// Shortest password `write_backup` accepts (the macOS dialog requires the same).
pub(crate) const MIN_BACKUP_PASSWORD_CHARS: usize = 8;

/// Settings that belong to this device, not to the data: where logins are sent,
/// stored PINs and the unattended-fetch interval. A restore keeps this device's
/// values and drops the backup's, so a backup file can never repoint a bank
/// endpoint that later receives a PIN.
const DEVICE_SETTINGS_WHERE: &str = "key LIKE 'fints.url.%' OR key LIKE 'fints.tan_medium.%' \
     OR key LIKE 'pin.%' OR key = 'sync.psd2_auto_hours'";

fn not_a_backup() -> CoreError {
    CoreError::invalid(tr("Falsches Passwort oder keine GiroCrab-Sicherung."))
}

/// The password opened the file, but it is another app's database (which may
/// also have `accounts` and `transactions` tables).
fn other_apps_database() -> CoreError {
    CoreError::invalid(tr(
        "Diese Datei ist keine GiroCrab-Sicherung, sondern die Datenbank einer anderen App. Zum Übernehmen bitte als Art des Imports diese App wählen.",
    ))
}

/// Drops every trigger and view in `schema`. The app defines neither, so any
/// found came from outside its own code paths (for example a crafted backup) and
/// could rewrite rows on later writes, such as repointing a FinTS endpoint
/// after the device-settings carve-out ran.
pub(crate) fn drop_triggers_and_views(conn: &Connection, schema: &str) -> CoreResult<()> {
    let objects: Vec<(String, String)> = conn
        .prepare(&format!(
            "SELECT type, name FROM {schema}.sqlite_master WHERE type IN ('trigger', 'view')"
        ))?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    for (kind, name) in objects {
        let name = name.replace('"', "\"\"");
        let kind = if kind == "view" { "VIEW" } else { "TRIGGER" };
        conn.execute_batch(&format!("DROP {kind} IF EXISTS {schema}.\"{name}\""))?;
    }
    Ok(())
}

fn remove_with_sidecars(path: &str) {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let _ = std::fs::remove_file(format!("{path}{suffix}"));
    }
}

#[uniffi::export]
impl Vault {
    /// Writes a consistent copy of this vault to `dest_path` (replacing it),
    /// encrypted with `password` instead of the device key.
    pub fn write_backup(&self, dest_path: String, password: String) -> CoreResult<()> {
        let password = Zeroizing::new(password);
        if password.is_empty() {
            return Err(CoreError::invalid(tr(
                "Bitte ein Passwort für die Sicherung angeben.",
            )));
        }
        // The backup is only as strong as this password (PBKDF2 over a guessable
        // string is cheap to brute-force), so every front end gets the same floor.
        if password.chars().count() < MIN_BACKUP_PASSWORD_CHARS {
            return Err(CoreError::invalid(tr(
                "Das Passwort für die Sicherung muss mindestens 8 Zeichen lang sein.",
            )));
        }
        self.export_to(&dest_path, &password)
    }

    /// Replaces this vault's contents with the backup at `backup_path`
    /// (written by [`Vault::write_backup`], opened with `password`). The data
    /// stays under this device's key; an older backup is migrated, one from a
    /// newer app is refused. Nothing changes if the backup cannot be read.
    pub fn restore_backup(&self, backup_path: String, password: String) -> CoreResult<()> {
        let password = Zeroizing::new(password);
        let staging = format!("{}.restore", self.path);
        remove_with_sidecars(&staging);
        let res = self.restore_via(&staging, &backup_path, &password);
        remove_with_sidecars(&staging);
        res
    }
}

/// Setting key marking that the one-time safety copy before the built-in FinTS
/// registration (0.8.0) was handled (made, or not needed for an empty vault).
pub(crate) const PRE_FINTS_REG_BACKUP_KEY: &str = "pre_0_8_0_fints_backup_done";

impl Vault {
    /// Path of the safety copy next to the vault file.
    pub(crate) fn pre_fints_reg_backup_path(vault_path: &str) -> String {
        format!("{vault_path}.pre-0.8.0-fints.bak")
    }

    /// Before the first migration/open by an app with the built-in FinTS
    /// registration: copies an existing vault (with accounts or bookings) to
    /// `<vault>.pre-0.8.0-fints.bak`, encrypted with the device key, via a
    /// temporary file and an atomic rename. Never overwrites an existing copy.
    /// Returns true if the marker setting still has to be written after the
    /// migration (false: already done, or the connection cannot read the vault).
    /// Best effort: a failed copy never blocks opening the vault.
    pub(crate) fn pre_fints_reg_backup(conn: &Connection, vault_path: &str, key: &[u8]) -> bool {
        // Wrong key / not yet readable / no settings table yet (very old vault).
        let marker: Result<Option<String>, _> = conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                [PRE_FINTS_REG_BACKUP_KEY],
                |r| r.get(0),
            )
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                e => Err(e),
            });
        let marker = match marker {
            Ok(m) => m,
            // Cannot tell: probe sqlite_master; an unreadable vault is skipped,
            // a readable one without settings table still needs the backup.
            Err(_) => {
                if conn
                    .query_row("SELECT count(*) FROM sqlite_master", [], |r| {
                        r.get::<_, i64>(0)
                    })
                    .is_err()
                {
                    return false;
                }
                None
            }
        };
        if marker.is_some() {
            return false;
        }
        let count = |table: &str| -> i64 {
            conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap_or(0)
        };
        if count("accounts") == 0 && count("transactions") == 0 {
            return true; // new/empty vault: nothing to preserve
        }
        let dest = Self::pre_fints_reg_backup_path(vault_path);
        if std::path::Path::new(&dest).exists() {
            return true;
        }
        let tmp = format!("{dest}.tmp");
        remove_with_sidecars(&tmp);
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap_or(0);
        let hex_key = Zeroizing::new(format!("x'{}'", hex::encode(key)));
        let copied = conn
            .execute(
                "ATTACH DATABASE ?1 AS prefints KEY ?2",
                params![tmp, &*hex_key],
            )
            .and_then(|_| conn.query_row("SELECT sqlcipher_export('prefints')", [], |_| Ok(())))
            .and_then(|_| conn.execute_batch(&format!("PRAGMA prefints.user_version = {version}")));
        let _ = conn.execute_batch("DETACH DATABASE prefints");
        let renamed = copied.is_ok() && std::fs::rename(&tmp, &dest).is_ok();
        remove_with_sidecars(&tmp);
        if !renamed {
            eprintln!("vault: pre-FinTS safety backup failed");
        }
        renamed
    }

    /// Copies the backup into `staging` under the device key, then over the
    /// open vault (SQLite backup API, same key on both sides).
    fn restore_via(&self, staging: &str, backup_path: &str, password: &str) -> CoreResult<()> {
        let tmp = Connection::open(staging)?;
        Self::apply_key(&tmp, &self.kept_key())?;
        // wrong password or no database: the attach or the first read fails
        tmp.execute(
            "ATTACH DATABASE ?1 AS bak KEY ?2",
            params![backup_path, password],
        )
        .map_err(|_| not_a_backup())?;
        // every GiroCrab vault since the first release has these tables
        for table in ["accounts", "transactions", "settings", "bank_connections"] {
            if !has_table(&tmp, "bak", table).map_err(|_| not_a_backup())? {
                return Err(other_apps_database());
            }
        }
        let version: i64 = tmp.query_row("PRAGMA bak.user_version", [], |r| r.get(0))?;
        if version > super::SCHEMA_VERSION {
            return Err(CoreError::SchemaTooNew {
                vault_version: version,
                app_version: super::SCHEMA_VERSION,
            });
        }
        tmp.query_row("SELECT sqlcipher_export('main', 'bak')", [], |_| Ok(()))?;
        // The export copies the backup's whole schema; only data may come along.
        drop_triggers_and_views(&tmp, "main")?;
        // Device-owned settings stay as they are on this device.
        let device: Vec<(String, String, i64)> = {
            let live = self.db()?;
            let mut stmt = live.prepare(&format!(
                "SELECT key, value, is_secret FROM settings WHERE {DEVICE_SETTINGS_WHERE}"
            ))?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            let rows: Vec<_> = rows.collect::<Result<_, _>>()?;
            drop(stmt);
            rows
        };
        tmp.execute(
            &format!("DELETE FROM main.settings WHERE {DEVICE_SETTINGS_WHERE}"),
            [],
        )?;
        for (k, v, secret) in device {
            tmp.execute(
                "INSERT INTO main.settings(key, value, is_secret) VALUES (?1, ?2, ?3)",
                params![k, v, secret],
            )?;
        }
        tmp.execute_batch(&format!("PRAGMA main.user_version = {version}"))?;
        tmp.execute_batch("DETACH DATABASE bak")?;

        let mut main = self.db()?;
        let copied = Backup::new(&tmp, &mut main)?.run_to_completion(256, Duration::ZERO, None);
        // the pages changed without moving the change count, also when the copy broke off
        self.data_replaced();
        copied?;
        Self::migrate(&mut main)?;
        Self::repair(&main);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
