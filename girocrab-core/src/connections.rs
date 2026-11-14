//! Managing bank logins: edit, de-duplicate, optional stored PIN, session
//! hand-over from the in-app browser login.

use rusqlite::{params, Connection, OptionalExtension};

use crate::connectors::trade_republic::normalize_phone;
use crate::error::{CoreError, CoreResult};
use crate::i18n::{tr, trf};
use crate::models::ConnectorKind;
use crate::vault::Vault;

#[derive(Debug, Clone, uniffi::Record)]
pub struct ConnectionStatus {
    pub connection_id: i64,
    /// A PIN is stored in the encrypted vault ("PIN im Tresor speichern").
    pub has_pin: bool,
    /// Session cookies (Trade Republic) / PSD2 session stored.
    pub has_session: bool,
    pub session_saved_unix: Option<i64>,
    pub account_count: u32,
    pub booking_count: u32,
    /// End of the stored PSD2 consent (RFC 3339), e.g. Enable Banking.
    pub consent_valid_until: Option<String>,
    /// Accounts of this connection the bank no longer grants access to.
    pub revoked_account_count: u32,
}

/// Login names are compared normalised (phone numbers in E.164).
pub fn normalize_login(kind: ConnectorKind, login: &str) -> String {
    match kind {
        ConnectorKind::TradeRepublic => normalize_phone(login),
        _ => login.trim().to_string(),
    }
}

pub(crate) fn pin_key(connection_id: i64) -> String {
    format!("pin.conn.{connection_id}")
}

/// Merges duplicate logins (same connector + same normalised login) into the
/// oldest one, keeping every account and booking. Soft-deleted connections are
/// left alone. Returns the number removed.
/// All or nothing: runs in a savepoint (works inside a caller's transaction too).
pub(crate) fn deduplicate(conn: &Connection) -> CoreResult<u32> {
    conn.execute_batch("SAVEPOINT dedup_logins")?;
    match deduplicate_inner(conn) {
        Ok(n) => {
            conn.execute_batch("RELEASE dedup_logins")?;
            Ok(n)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK TO dedup_logins; RELEASE dedup_logins");
            Err(e)
        }
    }
}

/// Moves the user data of the duplicate account `dup` onto `keep` (same
/// connection + external_ref) before `dup` is deleted: bookings that exist in
/// both keep `keep`'s values and only gain what is missing there (category,
/// note, checked, salary flag, splits, loan link, documents).
fn merge_duplicate_account(conn: &Connection, keep: i64, dup: i64) -> CoreResult<()> {
    // bookings present in both accounts (same bank id)
    #[allow(clippy::type_complexity)]
    let pairs: Vec<(
        i64,
        i64,
        bool,
        Option<i64>,
        String,
        Option<i64>,
        bool,
        String,
        Option<i64>,
    )> = {
        let mut st = conn.prepare(
            "SELECT k.id, o.id, k.amount_cents = o.amount_cents, o.category_id, o.category_source,
                    o.rule_id, o.checked != 0, o.note, o.salary_flag
               FROM transactions o JOIN transactions k
                 ON k.account_id = ?1 AND k.external_id = o.external_id
              WHERE o.account_id = ?2",
        )?;
        let r = st
            .query_map(params![keep, dup], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        r
    };
    for (k, o, same_amount, cat, src, rule, checked, note, salary) in pairs {
        conn.execute(
            "UPDATE transactions SET
                checked = CASE WHEN ?2 THEN 1 ELSE checked END,
                category_id = COALESCE(category_id, ?3),
                category_source = CASE WHEN category_id IS NULL AND ?3 IS NOT NULL THEN ?4 ELSE category_source END,
                rule_id = CASE WHEN category_id IS NULL AND ?3 IS NOT NULL THEN ?5 ELSE rule_id END,
                note = CASE WHEN note = '' THEN ?6 ELSE note END,
                salary_flag = COALESCE(salary_flag, ?7)
             WHERE id = ?1",
            params![k, checked, cat, src, rule, note, salary],
        )?;
        // splits only onto a booking without own splits and with the same amount
        let keep_has_splits: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM transaction_splits WHERE transaction_id = ?1)",
            [k],
            |r| r.get(0),
        )?;
        if same_amount && !keep_has_splits {
            conn.execute(
                "UPDATE transaction_splits SET transaction_id = ?1 WHERE transaction_id = ?2",
                params![k, o],
            )?;
        }
        // a loan link of `keep` wins (transaction_id is the key)
        conn.execute(
            "UPDATE OR IGNORE loan_links SET transaction_id = ?1 WHERE transaction_id = ?2",
            params![k, o],
        )?;
        conn.execute(
            "UPDATE booking_documents SET booking_id = ?1 WHERE booking_id = ?2",
            params![k, o],
        )?;
    }
    // bookings only in `dup` move over unchanged (with splits / links / documents)
    conn.execute(
        "UPDATE OR IGNORE transactions SET account_id = ?1 WHERE account_id = ?2",
        params![keep, dup],
    )?;
    // account-level data
    conn.execute(
        "UPDATE rules SET account_id = ?1 WHERE account_id = ?2",
        params![keep, dup],
    )?;
    conn.execute(
        "UPDATE booking_documents SET account_id = ?1 WHERE account_id = ?2",
        params![keep, dup],
    )?;
    conn.execute(
        "UPDATE loan_links SET loan_account_id = ?1 WHERE loan_account_id = ?2",
        params![keep, dup],
    )?;
    conn.execute(
        "UPDATE OR IGNORE loan_terms SET account_id = ?1 WHERE account_id = ?2",
        params![keep, dup],
    )?;
    let keep_has_holdings: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM holdings WHERE account_id = ?1)",
        [keep],
        |r| r.get(0),
    )?;
    if !keep_has_holdings {
        conn.execute(
            "UPDATE holdings SET account_id = ?1 WHERE account_id = ?2",
            params![keep, dup],
        )?;
    }
    Ok(())
}

fn deduplicate_inner(conn: &Connection) -> CoreResult<u32> {
    let rows: Vec<(i64, String, String)> = {
        let mut st =
            conn.prepare("SELECT id, connector, login_name FROM bank_connections WHERE deleted_ms IS NULL ORDER BY id")?;
        let r = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        r
    };
    let mut removed = 0u32;
    let mut keepers: Vec<(String, String, i64)> = Vec::new();
    for (id, kind_s, login) in rows {
        let Some(kind) = ConnectorKind::from_str(&kind_s) else {
            continue;
        };
        let norm = normalize_login(kind, &login);
        if norm.is_empty() {
            continue;
        }
        if let Some((_, _, keep)) = keepers
            .iter()
            .find(|(k, l, _)| *k == kind_s && l.eq_ignore_ascii_case(&norm))
        {
            let keep = *keep;
            // move accounts; merge accounts with the same external_ref
            let accs: Vec<(i64, String)> = {
                let mut st =
                    // includes soft-deleted rows
                    conn.prepare("SELECT id, external_ref FROM accounts WHERE connection_id = ?1")?;
                let r = st
                    .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<Result<Vec<_>, _>>()?;
                r
            };
            for (acc_id, ext) in accs {
                let target: Option<i64> = conn
                    .query_row(
                        // includes soft-deleted rows
                        "SELECT id FROM accounts WHERE connection_id = ?1 AND external_ref = ?2",
                        params![keep, ext],
                        |r| r.get(0),
                    )
                    .optional()?;
                match target {
                    Some(t) => {
                        merge_duplicate_account(conn, t, acc_id)?;
                        // only true duplicates are left; their data now lives on `t`
                        conn.execute("DELETE FROM accounts WHERE id = ?1", [acc_id])?;
                    }
                    None => {
                        conn.execute(
                            "UPDATE accounts SET connection_id = ?1 WHERE id = ?2",
                            params![keep, acc_id],
                        )?;
                    }
                }
            }
            // keep a stored PIN if the keeper has none
            conn.execute(
                "UPDATE settings SET key = ?1 WHERE key = ?2 AND NOT EXISTS (SELECT 1 FROM settings WHERE key = ?1)",
                params![pin_key(keep), pin_key(id)],
            )?;
            conn.execute("DELETE FROM settings WHERE key = ?1", [pin_key(id)])?;
            conn.execute("DELETE FROM bank_connections WHERE id = ?1", [id])?;
            removed += 1;
        } else {
            if norm != login {
                conn.execute(
                    "UPDATE bank_connections SET login_name = ?1 WHERE id = ?2",
                    params![norm, id],
                )?;
            }
            keepers.push((kind_s, norm, id));
        }
    }
    Ok(removed)
}

/// Stores (Some) or removes (None / empty) the PIN of a login (encrypted
/// vault `settings`, flagged secret). Never logged.
pub(crate) fn set_pin_inner(
    conn: &Connection,
    connection_id: i64,
    pin: Option<&str>,
) -> CoreResult<()> {
    match pin.filter(|p| !p.trim().is_empty()) {
        Some(p) => {
            conn.execute(
                "INSERT INTO settings(key, value, is_secret) VALUES (?1, ?2, 1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value, is_secret = 1",
                params![pin_key(connection_id), p.trim()],
            )?;
        }
        None => {
            conn.execute(
                "DELETE FROM settings WHERE key = ?1",
                [pin_key(connection_id)],
            )?;
        }
    }
    Ok(())
}

#[uniffi::export]
impl Vault {
    /// Edits a bank login (display name, Benutzerkennung / phone number).
    /// Phone numbers are stored in E.164 (+49…).
    pub fn update_bank_connection(
        &self,
        connection_id: i64,
        display_name: String,
        login_name: String,
    ) -> Result<(), CoreError> {
        let db = self.db()?;
        let (kind, old_name): (String, String) = db
            .query_row(
                "SELECT connector, display_name FROM bank_connections WHERE id = ?1 AND deleted_ms IS NULL",
                [connection_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| CoreError::NotFound {
                what: trf("Bankzugang {0}", &[&connection_id]),
            })?;
        let kind = ConnectorKind::from_str(&kind).ok_or_else(|| CoreError::InvalidInput {
            message: kind.clone(),
        })?;
        let login = normalize_login(kind, &login_name);
        let generic = crate::connectors::descriptor(kind).name;
        let name = if display_name.trim().is_empty() {
            generic.clone()
        } else {
            display_name.trim().to_string()
        };
        db.execute(
            "UPDATE bank_connections SET display_name = ?1, login_name = ?2 WHERE id = ?3",
            params![name, login, connection_id],
        )?;
        // The sidebar shows `accounts.bank_name`: follow the rename where it
        // still carries the old or generic name (never a name the bank sent).
        db.execute(
            "UPDATE accounts SET bank_name = ?1
             WHERE connection_id = ?2 AND bank_name IN (?3, ?4, '') AND bank_name <> ?1",
            params![name, connection_id, old_name, generic],
        )?;
        deduplicate(&db)?;
        Ok(())
    }

    /// Stores (Some) or removes (None / empty) the PIN of a login inside the
    /// encrypted vault. Used by `begin_sync` when no PIN is typed.
    pub fn set_connection_pin(
        &self,
        connection_id: i64,
        pin: Option<String>,
    ) -> Result<(), CoreError> {
        let db = self.db()?;
        set_pin_inner(&db, connection_id, pin.as_deref())
    }

    /// Hands over session cookies, e.g. from the in-app browser login
    /// (WKWebView): "tr_session=…; tr_refresh=…".
    pub fn set_connection_session(
        &self,
        connection_id: i64,
        cookies: String,
    ) -> Result<(), CoreError> {
        if !cookies.contains('=') {
            return Err(CoreError::InvalidInput {
                message: tr("Keine Sitzungs-Cookies übergeben").into(),
            });
        }
        let db = self.db()?;
        let state: String = db
            .query_row(
                "SELECT state_json FROM bank_connections WHERE id = ?1 AND deleted_ms IS NULL",
                [connection_id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| CoreError::NotFound {
                what: trf("Bankzugang {0}", &[&connection_id]),
            })?;
        let mut v: serde_json::Value =
            serde_json::from_str(&state).unwrap_or(serde_json::json!({}));
        v["cookies"] = serde_json::json!(cookies);
        v["saved_unix"] = serde_json::json!(chrono::Utc::now().timestamp());
        v["source"] = serde_json::json!("browser");
        db.execute(
            "UPDATE bank_connections SET state_json = ?1 WHERE id = ?2",
            params![v.to_string(), connection_id],
        )?;
        Ok(())
    }

    /// Forgets stored sessions (cookies, PSD2 session); keeps FinTS system data.
    pub fn clear_connection_session(&self, connection_id: i64) -> Result<(), CoreError> {
        let db = self.db()?;
        let state: Option<String> = db
            .query_row(
                "SELECT state_json FROM bank_connections WHERE id = ?1",
                [connection_id],
                |r| r.get(0),
            )
            .optional()?;
        let mut v: serde_json::Value = state
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(serde_json::json!({}));
        if let Some(o) = v.as_object_mut() {
            for k in [
                "cookies",
                "session_token",
                "session_id",
                "valid_until",
                "saved_unix",
                "source",
            ] {
                o.remove(k);
            }
        }
        db.execute(
            "UPDATE bank_connections SET state_json = ?1 WHERE id = ?2",
            params![v.to_string(), connection_id],
        )?;
        Ok(())
    }

    pub fn connection_status(&self, connection_id: i64) -> Result<ConnectionStatus, CoreError> {
        let db = self.db()?;
        let state: String = db
            .query_row(
                "SELECT state_json FROM bank_connections WHERE id = ?1 AND deleted_ms IS NULL",
                [connection_id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| CoreError::NotFound {
                what: trf("Bankzugang {0}", &[&connection_id]),
            })?;
        let v: serde_json::Value = serde_json::from_str(&state).unwrap_or(serde_json::json!({}));
        let has_pin: bool = db.query_row(
            "SELECT count(*) FROM settings WHERE key = ?1 AND value != ''",
            [pin_key(connection_id)],
            |r| r.get::<_, i64>(0),
        )? > 0;
        let has_session = v["cookies"]
            .as_str()
            .map(|s| !s.is_empty())
            .unwrap_or(false)
            || v["session_id"]
                .as_str()
                .map(|s| !s.is_empty())
                .unwrap_or(false);
        let (accounts, bookings, revoked): (i64, i64, i64) = db.query_row(
            "SELECT (SELECT count(*) FROM accounts WHERE connection_id = ?1 AND deleted_ms IS NULL),
                    (SELECT count(*) FROM transactions t JOIN accounts a ON a.id = t.account_id
                      WHERE a.connection_id = ?1 AND a.deleted_ms IS NULL AND t.deleted_ms IS NULL),
                    (SELECT count(*) FROM accounts WHERE connection_id = ?1 AND access_revoked != 0 AND deleted_ms IS NULL)",
            [connection_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        Ok(ConnectionStatus {
            connection_id,
            has_pin,
            has_session,
            session_saved_unix: v["saved_unix"].as_i64(),
            account_count: accounts as u32,
            booking_count: bookings as u32,
            consent_valid_until: v["valid_until"]
                .as_str()
                .filter(|s| !s.is_empty() && has_session)
                .map(str::to_string),
            revoked_account_count: revoked as u32,
        })
    }

    /// Merges duplicate logins; returns how many were merged.
    pub fn deduplicate_bank_connections(&self) -> Result<u32, CoreError> {
        let db = self.db()?;
        let tx = db.unchecked_transaction()?;
        let n = deduplicate(&tx)?;
        tx.commit()?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::BookingFilter;

    #[test]
    fn dedups_and_keeps_bookings() {
        let p_dir = crate::test_support::TempDir::new("girocrab-dedup");
        let p = p_dir.join("v.db");
        let v = Vault::open(p.to_string_lossy().to_string(), vec![4u8; 32]).unwrap();
        let a = v
            .add_bank_connection(
                ConnectorKind::VolksbankKasselGoettingen,
                String::new(),
                "1234567890".into(),
            )
            .unwrap();
        let b = v
            .add_bank_connection(
                ConnectorKind::VolksbankKasselGoettingen,
                String::new(),
                " 1234567890 ".into(),
            )
            .unwrap();
        assert_eq!(
            a, b,
            "adding the same login again updates instead of duplicating"
        );
        // simulate an old duplicate row with its own account + booking
        {
            let db = v.db().unwrap();
            db.execute("INSERT INTO bank_connections(connector, display_name, login_name) VALUES ('fints:52090000', 'VB', '1234567890')", []).unwrap();
            let dup = db.last_insert_rowid();
            db.execute("INSERT INTO account_groups(name) VALUES ('G')", [])
                .unwrap();
            let g = db.last_insert_rowid();
            db.execute("INSERT INTO accounts(connection_id, group_id, external_ref, name) VALUES (?1, ?2, 'DE1', 'Giro')", params![dup, g]).unwrap();
            let acc = db.last_insert_rowid();
            db.execute("INSERT INTO transactions(account_id, external_id, booking_date, amount_cents) VALUES (?1, 'x1', '2026-11-01', -100)", [acc]).unwrap();
        }
        assert_eq!(v.deduplicate_bank_connections().unwrap(), 1);
        let conns = v.list_bank_connections().unwrap();
        assert_eq!(conns.len(), 1);
        let st = v.connection_status(conns[0].id).unwrap();
        assert_eq!((st.account_count, st.booking_count), (1, 1));
        // PIN + TR phone normalisation + session hand-over
        v.set_connection_pin(a, Some("1234".into())).unwrap();
        assert!(v.connection_status(a).unwrap().has_pin);
        let tr = v
            .add_bank_connection(
                ConnectorKind::TradeRepublic,
                String::new(),
                "017012345678".into(),
            )
            .unwrap();
        assert_eq!(
            v.list_bank_connections()
                .unwrap()
                .iter()
                .find(|c| c.id == tr)
                .unwrap()
                .login_name,
            "+4917012345678"
        );
        assert_eq!(
            v.add_bank_connection(
                ConnectorKind::TradeRepublic,
                String::new(),
                "17012345678".into()
            )
            .unwrap(),
            tr
        );
        v.set_connection_session(tr, "tr_session=abc; tr_refresh=def".into())
            .unwrap();
        assert!(v.connection_status(tr).unwrap().has_session);
        v.clear_connection_session(tr).unwrap();
        assert!(!v.connection_status(tr).unwrap().has_session);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn loan_initial_balance_and_giro_crossmatch() {
        let p_dir = crate::test_support::TempDir::new("girocrab-loan");
        let p = p_dir.join("v.db");
        let v = Vault::open(p.to_string_lossy().to_string(), vec![5u8; 32]).unwrap();
        let (giro, loan);
        {
            let db = v.db().unwrap();
            db.execute("INSERT INTO account_groups(name) VALUES ('G')", [])
                .unwrap();
            let g = db.last_insert_rowid();
            db.execute("INSERT INTO accounts(group_id, external_ref, name, kind) VALUES (?1, 'giro', 'Giro', 'checking')", [g]).unwrap();
            giro = db.last_insert_rowid();
            db.execute("INSERT INTO accounts(group_id, external_ref, name, kind, account_number, balance_cents) VALUES (?1, 'l1', 'Darlehen', 'loan', '0098765400', -90000)", [g]).unwrap();
            loan = db.last_insert_rowid();
            // Loan own bookings: repayments of +5000 each -> before them the balance was -100000
            for (i, d) in ["2026-09-01", "2026-10-01"].iter().enumerate() {
                db.execute("INSERT INTO transactions(account_id, external_id, booking_date, amount_cents) VALUES (?1, ?2, ?3, 5000)", params![loan, format!("l{i}"), d]).unwrap();
            }
            db.execute("INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, purpose) VALUES (?1, 'g1', '2026-10-02', -5000, 'Zins/Tilgung Darlehen 98765400')", [giro]).unwrap();
            db.execute("INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, purpose) VALUES (?1, 'g2', '2026-10-03', -1000, 'Rewe')", [giro]).unwrap();
            db.execute("INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, purpose) VALUES (?1, 'g3', '2026-10-04', -300, 'Zinsen Tagesgeld Darlehen')", [giro]).unwrap();
            Vault::derive_initial_balance(&db, loan).unwrap();
        }
        let acc = v
            .list_accounts()
            .unwrap()
            .into_iter()
            .find(|a| a.id == loan)
            .unwrap();
        assert_eq!(acc.initial_balance_cents, Some(100000));
        v.set_account_initial_balance(loan, Some(-120000)).unwrap();
        let acc = v
            .list_accounts()
            .unwrap()
            .into_iter()
            .find(|a| a.id == loan)
            .unwrap();
        assert_eq!(acc.initial_balance_cents, Some(120000));
        let page = v
            .list_transactions(BookingFilter {
                account_ids: vec![loan],
                limit: 100,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            page.transactions.len(),
            3,
            "2 own + 1 matching giro booking"
        );
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn delete_account_cascades_and_cleans_connection() {
        let p_dir = crate::test_support::TempDir::new("girocrab-del");
        let p = p_dir.join("v.db");
        let v = Vault::open(p.to_string_lossy().to_string(), vec![6u8; 32]).unwrap();
        let (mut a1, mut a2) = (0i64, 0i64);
        let conn;
        {
            let db = v.db().unwrap();
            db.execute("INSERT INTO account_groups(name) VALUES ('G')", [])
                .unwrap();
            let g = db.last_insert_rowid();
            db.execute("INSERT INTO bank_connections(connector, display_name, login_name) VALUES ('fints:52090000', 'VB', '1')", []).unwrap();
            conn = db.last_insert_rowid();
            db.execute(
                "INSERT INTO settings(key, value, is_secret) VALUES (?1, 'pin', 1)",
                [pin_key(conn)],
            )
            .unwrap();
            for (n, sub) in [("l1", 1), ("l2", 2)] {
                db.execute("INSERT INTO accounts(connection_id, group_id, external_ref, name, kind) VALUES (?1, ?2, ?3, 'Darlehen', 'loan')", params![conn, g, n]).unwrap();
                let id = db.last_insert_rowid();
                db.execute("INSERT INTO transactions(account_id, external_id, booking_date, amount_cents) VALUES (?1, 'x', '2026-11-01', ?2)", params![id, sub]).unwrap();
                if sub == 1 {
                    a1 = id
                } else {
                    a2 = id
                }
            }
        }
        // same external_id in two accounts must not collide
        let n = |id: i64| {
            v.list_transactions(BookingFilter {
                account_ids: vec![id],
                limit: 10,
                ..Default::default()
            })
            .unwrap()
            .transactions
            .len()
        };
        assert_eq!((n(a1), n(a2)), (1, 1));
        // only archived accounts can be deleted
        assert!(v.delete_account(a1).is_err());
        assert_eq!(n(a1), 1);
        v.set_account_archived(a1, true).unwrap();
        v.set_account_archived(a2, true).unwrap();
        v.delete_account(a1).unwrap();
        assert_eq!(v.list_accounts().unwrap().len(), 1);
        assert_eq!(
            v.list_bank_connections().unwrap().len(),
            1,
            "connection kept while accounts remain"
        );
        v.delete_account(a2).unwrap();
        assert!(
            v.list_bank_connections().unwrap().is_empty(),
            "empty connection removed"
        );
        assert!(
            v.get_setting(pin_key(conn)).unwrap().is_none(),
            "secret removed"
        );
        let left: i64 = v
            .db()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM transactions WHERE deleted_ms IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(left, 0);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn deduplicate_leaves_soft_deleted_twin_alone() {
        let p =
            std::env::temp_dir().join(format!("girocrab-dedup-del-{}.db", uuid::Uuid::new_v4()));
        let v = Vault::open(p.to_string_lossy().to_string(), vec![4u8; 32]).unwrap();
        {
            let db = v.db().unwrap();
            let g: i64 = {
                db.execute("INSERT INTO account_groups(name) VALUES ('G')", [])
                    .unwrap();
                db.last_insert_rowid()
            };
            // the deleted twin has the lower id
            db.execute("INSERT INTO bank_connections(connector, display_name, login_name, deleted_ms) VALUES ('fints:52090000', 'VB', '1234567890', 9)", []).unwrap();
            db.execute("INSERT INTO bank_connections(connector, display_name, login_name) VALUES ('fints:52090000', 'VB', '1234567890')", []).unwrap();
            let live = db.last_insert_rowid();
            db.execute("INSERT INTO accounts(connection_id, group_id, external_ref, name) VALUES (?1, ?2, 'DE1', 'Giro')", params![live, g]).unwrap();
        }
        let removed = deduplicate(&v.db().unwrap()).unwrap();
        assert_eq!(removed, 0);
        let db = v.db().unwrap();
        let q = |sql: &str| db.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(q("SELECT count(*) FROM bank_connections"), 2);
        assert_eq!(
            q("SELECT count(*) FROM bank_connections WHERE deleted_ms IS NULL"),
            1
        );
        assert_eq!(
            q("SELECT count(*) FROM accounts a JOIN bank_connections c ON c.id = a.connection_id WHERE c.deleted_ms IS NULL AND a.deleted_ms IS NULL"),
            1
        );
        let _ = std::fs::remove_file(p);
    }
}
