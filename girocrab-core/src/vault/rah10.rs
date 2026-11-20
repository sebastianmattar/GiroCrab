//! Commerzbank RAH-10 keyfile import and separately confirmed bank keys.
use super::Vault;
use crate::{error::CoreError, i18n::tr, models::ConnectorKind};
use rusqlite::params;
use zeroize::Zeroizing;

#[uniffi::export]
impl Vault {
    pub fn is_rah10_connection(&self, connection_id: i64) -> Result<bool, CoreError> {
        let db = self.db()?;
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM bank_connections WHERE id = ?1 AND connector LIKE 'fints:%:rah10' AND deleted_ms IS NULL)",
            [connection_id],
            |r| r.get(0),
        )
        .map_err(Into::into)
    }

    /// Imports an existing Commerzbank profile-10 keyfile into the encrypted vault.
    /// The original file is never opened or changed by this API. Password is used
    /// only to validate the import; future syncs use the normal secret prompt.
    pub fn add_rah10_connection(
        &self,
        bytes: Vec<u8>,
        password: String,
        display_name: String,
    ) -> Result<i64, CoreError> {
        self.add_rah10_connection_with_bank_keys(bytes, password, display_name, vec![], vec![])
    }

    /// Imports separately confirmed public bank keys without rewriting the keyfile.
    pub fn add_rah10_connection_with_bank_keys(
        &self,
        bytes: Vec<u8>,
        password: String,
        display_name: String,
        bank_keys: Vec<crate::connectors::fints::bank_keys::Rah10BankKeyCandidate>,
        expected_fingerprints: Vec<String>,
    ) -> Result<i64, CoreError> {
        use base64::Engine;
        let bytes = Zeroizing::new(bytes);
        let password = Zeroizing::new(password);
        let mut file =
            crate::connectors::fints::keyfile::KeyFile::open(&bytes, password.as_bytes())?;
        let identity = crate::connectors::fints::bank_keys::supported_identity(&file)?;
        let blz = identity.blz.clone();
        let login = identity.user_id.clone();
        let first = u64::from(identity.signature_id) + 1;
        let saved_keys = if !bank_keys.is_empty() || !expected_fingerprints.is_empty() {
            let verified = crate::connectors::fints::bank_keys::confirmed_keys(
                &blz,
                &bank_keys,
                &expected_fingerprints,
            )?;
            file.bank_keys.retain(|key| key.blz != blz);
            file.bank_keys.extend(verified);
            Some(crate::connectors::fints::bank_keys::SavedBankKeys {
                candidates: bank_keys,
                expected: expected_fingerprints,
            })
        } else {
            None
        };
        // Check both pinned bank keys exist before storing the connection.
        crate::connectors::fints::rah10::Rah10::new(file, &login, first, first + 10_000)?;
        let key = format!("{}:rah10", ConnectorKind::fints_key(&blz));
        crate::banking_gate::check_connector(&key)?;
        let db = self.db()?;
        let tx = db.unchecked_transaction()?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM bank_connections WHERE connector = ?1 AND login_name = ?2 AND deleted_ms IS NULL)",
            params![key, login], |r| r.get(0))?;
        if exists {
            return Err(CoreError::invalid(tr(
                "Dieser Bankzugang ist bereits vorhanden.",
            )));
        }
        let name = if display_name.trim().is_empty() {
            "Commerzbank (RAH-10)".to_string()
        } else {
            display_name.trim().to_string()
        };
        let (id, _) = Self::add_bank_connection_keyed(
            &tx,
            ConnectorKind::FinTsGeneric,
            &key,
            Some(&blz),
            name,
            login,
        )?;
        let encoded =
            Zeroizing::new(base64::engine::general_purpose::STANDARD.encode(bytes.as_slice()));
        tx.execute(
            "INSERT INTO settings(key, value, is_secret) VALUES (?1, ?2, 1)",
            params![format!("fints.rah10.private.{id}"), encoded.as_str()],
        )?;
        tx.execute(
            "INSERT INTO settings(key, value, is_secret) VALUES (?1, ?2, 0)",
            params![format!("fints.rah10.counter.{id}"), first.to_string()],
        )?;
        if let Some(keys) = saved_keys {
            tx.execute(
                "INSERT INTO settings(key, value, is_secret) VALUES (?1, ?2, 0)",
                params![
                    format!("fints.rah10.bank_keys.{id}"),
                    serde_json::to_string(&keys)?
                ],
            )?;
        }
        // Existing FinTS setup expects a URL. RAH-10 itself uses the fixed TCP endpoint.
        tx.execute("INSERT INTO settings(key, value, is_secret) VALUES (?1, ?2, 0) ON CONFLICT(key) DO NOTHING",
            params![format!("fints.url.{blz}"), "https://fints.commerzbank.de/fints"])?;
        tx.commit()?;
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rah10_cannot_be_used_as_payment_source() {
        let dir = crate::test_support::TempDir::new("rah10-payment-source");
        let vault = Vault::open(
            dir.path().join("vault.db").to_string_lossy().into(),
            vec![7; 32],
        )
        .unwrap();
        let account = {
            let db = vault.db().unwrap();
            db.execute("INSERT INTO bank_connections(connector, login_name, display_name) VALUES ('fints:20040000:rah10', 'demo', 'Test')", []).unwrap();
            let connection = db.last_insert_rowid();
            db.execute("INSERT INTO account_groups(name) VALUES ('Test')", [])
                .unwrap();
            let group = db.last_insert_rowid();
            db.execute("INSERT INTO accounts(group_id, connection_id, name, iban) VALUES (?1, ?2, 'Test', 'DE02120300000000202051')", params![group, connection]).unwrap();
            db.last_insert_rowid()
        };
        // All payment types share this guard, including the newly added batch,
        // standing-order and direct-debit paths. Demo mode must not bypass it.
        for demo in [false, true] {
            assert!(
                matches!(vault.transfer_source(account, demo), Err(CoreError::InvalidInput { message }) if message.contains("RAH-10"))
            );
        }
    }
}
