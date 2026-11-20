//! Enable Banking application per bank login (#824).
//!
//! Application-ID and private key used to be one global pair, so every Enable
//! Banking login signed its JWT with whatever was stored last: setting up a
//! bank with a second application moved all older logins (and their consents)
//! to it. A login can now carry its own pair
//! (`enablebanking.application_id.<conn>` / `enablebanking.private_key_pem.<conn>`).
//! The global pair stays the default for logins without one and for the bank
//! list. Before the default changes, every login still relying on it gets the
//! old pair as its own ([`pin_default_app`]), whichever front end or path
//! (settings form, key file, iCloud Keychain) writes the new one.

use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashMap;
use zeroize::Zeroizing;

use crate::error::{CoreError, CoreResult};
use crate::i18n::{tr, trf};
use crate::models::ConnectorKind;
use crate::secret::SecretString;
use crate::vault::Vault;

/// Global (default) Application-ID, used as the JWT `kid`.
pub(crate) const APP_ID_KEY: &str = "enablebanking.application_id";
/// Global (default) private key (PEM) that signs the RS256 JWT.
pub(crate) const PEM_KEY: &str = "enablebanking.private_key_pem";

/// Settings keys of one login's own application: `[application id, PEM]`.
pub(crate) fn own_keys(connection_id: i64) -> [String; 2] {
    [
        format!("{APP_ID_KEY}.{connection_id}"),
        format!("{PEM_KEY}.{connection_id}"),
    ]
}

/// The Enable Banking application a bank login signs with. Never carries the
/// private key (see [`Vault::connection_enable_banking_key`]).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ConnectionEnableBankingApp {
    pub connection_id: i64,
    pub application_id: String,
    /// A private key is stored for this application.
    pub has_key: bool,
    /// The login's own application; `false` = the default (global) one.
    pub own: bool,
}

/// The pair under `keys`, only when both parts are set.
fn pair(db: &Connection, keys: [&str; 2]) -> CoreResult<Option<(String, String)>> {
    let id = Vault::setting_inner(db, keys[0])?;
    let pem = Vault::setting_inner(db, keys[1])?;
    Ok(id.zip(pem))
}

fn own_pair(db: &Connection, connection_id: i64) -> CoreResult<Option<(String, String)>> {
    let [id, pem] = own_keys(connection_id);
    pair(db, [&id, &pem])
}

fn default_pair(db: &Connection) -> CoreResult<Option<(String, String)>> {
    pair(db, [APP_ID_KEY, PEM_KEY])
}

/// `(application id, PEM, own)` the login signs with: its own pair, else the
/// default one.
pub(crate) fn effective(
    db: &Connection,
    connection_id: i64,
) -> CoreResult<Option<(String, String, bool)>> {
    if let Some((id, pem)) = own_pair(db, connection_id)? {
        return Ok(Some((id, pem, true)));
    }
    Ok(default_pair(db)?.map(|(id, pem)| (id, pem, false)))
}

/// Puts the login's own application under the global keys of a sync
/// context, so the connector signs with it.
pub(crate) fn overlay(
    db: &Connection,
    connection_id: i64,
    settings: &mut HashMap<String, SecretString>,
) -> CoreResult<()> {
    if let Some((id, pem)) = own_pair(db, connection_id)? {
        settings.insert(APP_ID_KEY.to_string(), SecretString::new(id));
        settings.insert(PEM_KEY.to_string(), SecretString::new(pem));
    }
    Ok(())
}

/// An application for calls that belong to no login (the bank list): the
/// default one, else any login's own.
pub(crate) fn any_app(db: &Connection) -> CoreResult<Option<(String, String)>> {
    if let Some(p) = default_pair(db)? {
        return Ok(Some(p));
    }
    let ids: Vec<i64> = {
        let mut st = db.prepare(
            "SELECT CAST(substr(key, length(?1) + 2) AS INTEGER) FROM settings
              WHERE substr(key, 1, length(?1) + 1) = ?1 || '.' ORDER BY key",
        )?;

        st.query_map([APP_ID_KEY], |r| r.get(0))?
            .collect::<Result<_, _>>()?
    };
    for id in ids {
        if let Some(p) = own_pair(db, id)? {
            return Ok(Some(p));
        }
    }
    Ok(None)
}

/// Logins that sign with Enable Banking: generic PSD2 banks, and American
/// Express / Amazon Visa switched to PSD2 under "Erweitert".
fn uses_enable_banking(db: &Connection, connector: &str) -> CoreResult<bool> {
    let opt_in = match ConnectorKind::from_str(connector) {
        Some(ConnectorKind::EnableBanking) => return Ok(true),
        Some(ConnectorKind::AmericanExpress) => "enablebanking.use.amex",
        Some(ConnectorKind::AmazonVisa) => "enablebanking.use.amazonvisa",
        _ => return Ok(false),
    };
    Ok(Vault::setting_inner(db, opt_in)?.as_deref().map(str::trim) == Some("1"))
}

/// Gives every Enable Banking login without an application of its own the
/// current default pair, so a new default never moves it. Returns how many
/// logins were pinned. Nothing happens while the default is incomplete.
pub(crate) fn pin_default_app(db: &Connection) -> CoreResult<u32> {
    let Some((id, pem)) = default_pair(db)? else {
        return Ok(0);
    };
    let pem = Zeroizing::new(pem);
    let logins: Vec<(i64, String)> = {
        let mut st =
            db.prepare("SELECT id, connector FROM bank_connections WHERE deleted_ms IS NULL")?;

        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?
    };
    let mut pinned = 0;
    for (conn_id, connector) in logins {
        if !uses_enable_banking(db, &connector)? || own_pair(db, conn_id)?.is_some() {
            continue;
        }
        let [id_key, pem_key] = own_keys(conn_id);
        Vault::set_setting_inner(db, &id_key, &id)?;
        Vault::set_setting_inner(db, &pem_key, &pem)?;
        pinned += 1;
    }
    Ok(pinned)
}

/// Called before a global setting is written: pins the logins to the old
/// default when the Enable Banking application is about to change.
pub(crate) fn before_global_write(db: &Connection, key: &str, value: &str) -> CoreResult<()> {
    if key != APP_ID_KEY && key != PEM_KEY {
        return Ok(());
    }
    let old = Vault::setting_inner(db, key)?;
    let new = Some(value.trim()).filter(|v| !v.is_empty());
    if old.as_deref() != new {
        pin_default_app(db)?;
    }
    Ok(())
}

/// Removes a deleted login's own application.
pub(crate) fn forget(db: &Connection, connection_id: i64) -> CoreResult<()> {
    let [id, pem] = own_keys(connection_id);
    db.execute(
        "DELETE FROM settings WHERE key IN (?1, ?2)",
        params![id, pem],
    )?;
    Ok(())
}

/// Duplicate login `from` is merged into `keep`: `keep` takes over `from`'s
/// own application unless it has one, then `from`'s is removed.
pub(crate) fn merge_into(db: &Connection, keep: i64, from: i64) -> CoreResult<()> {
    if own_pair(db, keep)?.is_none()
        && let Some((id, pem)) = own_pair(db, from)?
    {
        let pem = Zeroizing::new(pem);
        let [id_key, pem_key] = own_keys(keep);
        Vault::set_setting_inner(db, &id_key, &id)?;
        Vault::set_setting_inner(db, &pem_key, &pem)?;
    }
    forget(db, from)
}

fn connector_of(db: &Connection, connection_id: i64) -> CoreResult<String> {
    db.query_row(
        "SELECT connector FROM bank_connections WHERE id = ?1 AND deleted_ms IS NULL",
        [connection_id],
        |r| r.get(0),
    )
    .optional()?
    .ok_or_else(|| CoreError::NotFound {
        what: trf("Bankverbindung {0}", &[&connection_id]),
    })
}

#[uniffi::export]
impl Vault {
    /// The Enable Banking application this login signs with (its own, else
    /// the default one); `None` when neither is complete.
    pub fn connection_enable_banking_app(
        &self,
        connection_id: i64,
    ) -> Result<Option<ConnectionEnableBankingApp>, CoreError> {
        let db = self.db()?;
        connector_of(&db, connection_id)?;
        Ok(
            effective(&db, connection_id)?.map(|(id, _, own)| ConnectionEnableBankingApp {
                connection_id,
                application_id: id,
                has_key: true,
                own,
            }),
        )
    }

    /// Private key (PEM) of the application this login signs with, for the
    /// "show" button of its settings.
    pub fn connection_enable_banking_key(
        &self,
        connection_id: i64,
    ) -> Result<Option<String>, CoreError> {
        let db = self.db()?;
        connector_of(&db, connection_id)?;
        Ok(effective(&db, connection_id)?.map(|(_, pem, _)| pem))
    }

    /// Logins with an Enable Banking application of their own.
    pub fn list_connection_enable_banking_apps(
        &self,
    ) -> Result<Vec<ConnectionEnableBankingApp>, CoreError> {
        let db = self.db()?;
        let ids: Vec<i64> = {
            let mut st =
                db.prepare("SELECT id FROM bank_connections WHERE deleted_ms IS NULL ORDER BY id")?;

            st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?
        };
        let mut out = Vec::new();
        for id in ids {
            if let Some((app, _)) = own_pair(&db, id)? {
                out.push(ConnectionEnableBankingApp {
                    connection_id: id,
                    application_id: app,
                    has_key: true,
                    own: true,
                });
            }
        }
        Ok(out)
    }

    /// Stores the Enable Banking application of one login only; the default
    /// and every other login stay as they are. Both empty: the login goes
    /// back to the default application. When the application the login signs
    /// with changes, its PSD2 session is dropped (sessions and consents
    /// belong to one application). Returns whether it changed.
    pub fn set_connection_enable_banking_app(
        &self,
        connection_id: i64,
        application_id: String,
        private_key_pem: String,
    ) -> Result<bool, CoreError> {
        let private_key_pem = Zeroizing::new(private_key_pem);
        let application_id = application_id.trim();
        let pem = private_key_pem.trim();
        if application_id.is_empty() != pem.is_empty() {
            return Err(CoreError::invalid(tr(
                "Application-ID und privater Schlüssel gehören zusammen: bitte beide angeben.",
            )));
        }
        if !pem.is_empty() {
            // a key that cannot sign must not replace one that can
            crate::connectors::enable_banking::EnableBankingConnector::make_jwt_for(
                application_id,
                pem,
            )?;
        }
        let db = self.db()?;
        connector_of(&db, connection_id)?;
        let before = effective(&db, connection_id)?.map(|(id, pem, _)| (id, Zeroizing::new(pem)));
        let tx = db.unchecked_transaction()?;
        let [id_key, pem_key] = own_keys(connection_id);
        Vault::set_setting_inner(&tx, &id_key, application_id)?;
        Vault::set_setting_inner(&tx, &pem_key, pem)?;
        let after = effective(&tx, connection_id)?.map(|(id, pem, _)| (id, Zeroizing::new(pem)));
        let changed = before.as_ref().map(|(i, p)| (i.as_str(), p.as_str()))
            != after.as_ref().map(|(i, p)| (i.as_str(), p.as_str()));
        if changed {
            super::connections::clear_session_inner(&tx, connection_id)?;
        }
        tx.commit()?;
        Ok(changed)
    }
}

#[cfg(test)]
mod tests;
