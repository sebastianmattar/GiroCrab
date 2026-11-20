//! Managing bank logins: edit, de-duplicate, optional stored PIN, session
//! hand-over from the in-app browser login.

use rusqlite::{Connection, OptionalExtension, params};

use crate::connectors::trade_republic::normalize_phone;
use crate::error::{CoreError, CoreResult};
use crate::i18n::{tr, trf};
use crate::models::ConnectorKind;
use crate::secret::{SecretJson, SecretString, wipe};
use crate::vault::Vault;

#[derive(Debug, Clone, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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
pub(crate) fn normalize_login(kind: ConnectorKind, login: &str) -> String {
    match kind {
        ConnectorKind::TradeRepublic => normalize_phone(login),
        _ => login.trim().to_string(),
    }
}

pub(crate) fn pin_key(connection_id: i64) -> String {
    format!("pin.conn.{connection_id}")
}

/// Forgets stored sessions (cookies, PSD2 session) of a login; keeps FinTS
/// system data.
pub(crate) fn clear_session_inner(db: &Connection, connection_id: i64) -> CoreResult<()> {
    // Unlike `connection_state`, a deleted or missing connection is no error here.
    let state: Option<SecretJson> = db
        .query_row(
            "SELECT state_json FROM bank_connections WHERE id = ?1",
            [connection_id],
            |r| r.get(0),
        )
        .optional()?;
    update_state(db, connection_id, &state.unwrap_or_default(), |o| {
        for k in [
            "cookies",
            "session_token",
            "session_id",
            "valid_until",
            "saved_unix",
            "source",
            "refresh_token",
        ] {
            if let Some(mut old) = o.remove(k) {
                wipe(&mut old);
            }
        }
    })
}

/// Stored connector state of a connection that is not deleted.
fn connection_state(db: &Connection, connection_id: i64) -> CoreResult<SecretJson> {
    db.query_row(
        "SELECT state_json FROM bank_connections WHERE id = ?1 AND deleted_ms IS NULL",
        [connection_id],
        |r| r.get(0),
    )
    .optional()?
    .ok_or_else(|| CoreError::NotFound {
        what: trf("Bankzugang {0}", &[&connection_id]),
    })
}

/// Changes single keys of any connector's state and keeps the others. A state
/// that is not a JSON object starts over empty. `edit` wipes what it removes.
fn update_state(
    db: &Connection,
    connection_id: i64,
    state: &SecretJson,
    edit: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
) -> CoreResult<()> {
    let updated = state.with_value(|v| {
        if !v.is_object() {
            wipe(v);
            *v = serde_json::json!({});
        }
        if let Some(o) = v.as_object_mut() {
            edit(o);
        }
        SecretJson::encode(v)
    })?;
    store_state(db, connection_id, &updated)
}

pub(crate) fn store_state(
    db: &Connection,
    connection_id: i64,
    state: &SecretJson,
) -> CoreResult<()> {
    db.execute(
        "UPDATE bank_connections SET state_json = ?1 WHERE id = ?2",
        params![state, connection_id],
    )?;
    Ok(())
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

        st.query_map(params![keep, dup], |r| {
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
        .collect::<Result<Vec<_>, _>>()?
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
        "UPDATE OR IGNORE rule_accounts SET account_id = ?1 WHERE account_id = ?2",
        params![keep, dup],
    )?;
    conn.execute("DELETE FROM rule_accounts WHERE account_id = ?1", [dup])?;
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
    conn.execute(
        "UPDATE OR IGNORE security_transactions SET account_id = ?1 WHERE account_id = ?2",
        params![keep, dup],
    )?;
    // the value history would go with the deleted account's cascade (#578)
    crate::depot_values::move_account(conn, dup, keep)?;
    // direct debit and order data (#143, #239, #240): the caller deletes `dup`, and
    // these tables reference it without a cascade (a leftover row would fail the delete)
    conn.execute(
        "UPDATE OR IGNORE mandates SET account_id = ?1 WHERE account_id = ?2",
        params![keep, dup],
    )?;
    conn.execute("DELETE FROM mandates WHERE account_id = ?1", [dup])?;
    conn.execute(
        "UPDATE OR IGNORE mandate_deletions SET account_id = ?1 WHERE account_id = ?2",
        params![keep, dup],
    )?;
    conn.execute("DELETE FROM mandate_deletions WHERE account_id = ?1", [dup])?;
    conn.execute(
        "UPDATE OR IGNORE creditor_ids SET account_id = ?1 WHERE account_id = ?2",
        params![keep, dup],
    )?;
    conn.execute("DELETE FROM creditor_ids WHERE account_id = ?1", [dup])?;
    conn.execute(
        "UPDATE OR IGNORE planned_transfers SET account_id = ?1 WHERE account_id = ?2",
        params![keep, dup],
    )?;
    // standing orders are replaced on every fetch
    conn.execute("DELETE FROM standing_orders WHERE account_id = ?1", [dup])?;
    Ok(())
}

fn deduplicate_inner(conn: &Connection) -> CoreResult<u32> {
    let rows: Vec<(i64, String, String)> = {
        let mut st =
            conn.prepare("SELECT id, connector, login_name FROM bank_connections WHERE deleted_ms IS NULL ORDER BY id")?;

        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<Vec<_>, _>>()?
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

                st.query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<Result<Vec<_>, _>>()?
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
            conn.execute(
                "DELETE FROM settings WHERE key = ?1 OR substr(key, 1, length(?1) + 1) = ?1 || '.'",
                [pin_key(id)],
            )?;
            // and its own Enable Banking application (#824)
            super::enable_banking_apps::merge_into(conn, keep, id)?;
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
        let (kind, old_name, old_login): (String, String, String) = db
            .query_row(
                "SELECT connector, display_name, login_name FROM bank_connections WHERE id = ?1 AND deleted_ms IS NULL",
                [connection_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?
            .ok_or_else(|| CoreError::NotFound {
                what: trf("Bankzugang {0}", &[&connection_id]),
            })?;
        let kind = ConnectorKind::from_str(&kind).ok_or_else(|| CoreError::invalid(kind))?;
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
        // A stored session (cookies, refresh token) belongs to the old login:
        // keeping it would fetch that login's data under the new one.
        if login != old_login {
            clear_session_inner(&db, connection_id)?;
        }
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
        let cookies = SecretString::new(cookies);
        if !cookies.expose().contains('=') {
            return Err(CoreError::invalid(tr("Keine Sitzungs-Cookies übergeben")));
        }
        let db = self.db()?;
        let state = connection_state(&db, connection_id)?;
        update_state(&db, connection_id, &state, |o| {
            let saved_unix = jiff::Timestamp::now().as_second();
            for (key, value) in [
                ("cookies", cookies.expose().into()),
                ("saved_unix", saved_unix.into()),
                ("source", "browser".into()),
            ] {
                if let Some(mut old) = o.insert(key.into(), value) {
                    wipe(&mut old);
                }
            }
        })
    }

    /// Forgets stored sessions (cookies, PSD2 session); keeps FinTS system data.
    pub fn clear_connection_session(&self, connection_id: i64) -> Result<(), CoreError> {
        let db = self.db()?;
        clear_session_inner(&db, connection_id)
    }

    pub fn connection_status(&self, connection_id: i64) -> Result<ConnectionStatus, CoreError> {
        let db = self.db()?;
        let state = connection_state(&db, connection_id)?;
        let has_pin: bool = db.query_row(
            "SELECT count(*) FROM settings WHERE key = ?1 AND value != ''",
            [pin_key(connection_id)],
            |r| r.get::<_, i64>(0),
        )? > 0;
        let (has_session, session_saved_unix, consent_valid_until) = state.with_value(|v| {
            let filled = |key: &str| v[key].as_str().is_some_and(|s| !s.is_empty());
            // A stored refresh token (Kontist) also lets a sync run without the password.
            let has_session = filled("cookies") || filled("session_id") || filled("refresh_token");
            let valid_until = v["valid_until"]
                .as_str()
                .filter(|s| !s.is_empty() && has_session)
                .map(str::to_string);
            (has_session, v["saved_unix"].as_i64(), valid_until)
        });
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
            session_saved_unix,
            account_count: accounts as u32,
            booking_count: bookings as u32,
            consent_valid_until,
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

/// comdirect bank code of a login: from the first comdirect IBAN among its
/// accounts (characters 5–12), else the stored bank code. The account's code
/// comes first: comdirect's API logins all carry 20041111. Only a first
/// guess – if comdirect rejects it for this login (9210), the FinTS connector
/// tries its other codes and keeps the one that works.
fn comdirect_blz(
    conn: &Connection,
    connection_id: i64,
    stored: &str,
) -> CoreResult<Option<String>> {
    use crate::connectors::fints_directory::COMDIRECT_BLZ;
    let mut st = conn.prepare(
        "SELECT upper(replace(iban, ' ', '')) FROM accounts
          WHERE connection_id = ?1 AND deleted_ms IS NULL ORDER BY sort_order, id",
    )?;
    let ibans: Vec<String> = st
        .query_map([connection_id], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    let from_iban = ibans.iter().find_map(|i| {
        let blz = i.get(4..12)?;
        (i.starts_with("DE") && COMDIRECT_BLZ.contains(&blz)).then(|| blz.to_string())
    });
    Ok(from_iban.or_else(|| COMDIRECT_BLZ.contains(&stored).then(|| stored.to_string())))
}

/// Reference prefix of the accounts „Abrufweg umstellen“ marks for being
/// taken over again (`<prefix><old reference>`); the sync ignores it (#537).
pub(crate) const SWITCH_MARK: &str = "moneymoney-db:switch:";

/// Repair for vaults switched before #1134 was fixed: the switch marked open
/// card holds like bookings (`mm-switch-pending-hold-…`). They count as posted, no
/// hold cleanup reaches them and the bank's real booking comes next to them.
/// Drops them like a settled hold (category and note go with it); the
/// tombstones keep their ids, so other devices drop them on merge. Transfer
/// bookings of the app the switch had marked (`mm-switch-pending-<e2e>`) stay:
/// the bank's booking takes them over by fingerprint, with category and note.
/// Returns how many were dropped.
pub(crate) fn drop_switched_holds(conn: &Connection) -> CoreResult<usize> {
    let ids: Vec<i64> = conn
        .prepare(
            "SELECT id FROM transactions
              WHERE external_id LIKE 'mm-switch-pending-hold-%' AND deleted_ms IS NULL",
        )?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    for &id in &ids {
        crate::sync_merge::soft_delete_booking(conn, id)?;
    }
    Ok(ids.len())
}

#[uniffi::export]
impl Vault {
    /// Switches a comdirect login between comdirect's own API and FinTS
    /// (`fints:<blz>`, bank code from the login's IBANs) and back, keeping
    /// its accounts and bookings. The connector session is cleared; accounts
    /// and live bookings are marked so the next fetch takes them over by
    /// IBAN and by booking (amount, date ±2 days, counterparty) instead of
    /// adding them a second time – the same path as after a database import.
    /// Open card holds are dropped: the new way reports them again (#1134).
    /// Returns the new connector key.
    pub fn switch_comdirect_access(&self, connection_id: i64) -> Result<String, CoreError> {
        let db = self.db()?;
        let tx = db.unchecked_transaction()?;
        let (key, bank_code, login): (String, String, String) = tx
            .query_row(
                "SELECT connector, bank_code, login_name FROM bank_connections
                  WHERE id = ?1 AND deleted_ms IS NULL",
                [connection_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?
            .ok_or_else(|| CoreError::NotFound {
                what: trf("Bankzugang {0}", &[&connection_id]),
            })?;
        let from_api = key == ConnectorKind::Comdirect.as_str();
        let fints_blz = ConnectorKind::fints_blz(&key).map(str::to_string);
        let is_comdirect_fints = fints_blz
            .as_deref()
            .is_some_and(|b| crate::connectors::fints_directory::COMDIRECT_BLZ.contains(&b));
        if !from_api && !is_comdirect_fints {
            return Err(CoreError::invalid(tr(
                "Nur comdirect-Zugänge lassen sich zwischen FinTS und der comdirect-Schnittstelle umstellen.",
            )));
        }
        let (kind, new_key, new_code) = if from_api {
            let blz = comdirect_blz(&tx, connection_id, &bank_code)?.ok_or_else(|| {
                CoreError::invalid(tr("Keine comdirect-IBAN gefunden: Die Bankleitzahl für FinTS lässt sich nicht bestimmen."))
            })?;
            (
                ConnectorKind::FinTsGeneric,
                ConnectorKind::fints_key(&blz),
                blz,
            )
        } else {
            // keep the bank code: an imported comdirect login has one, and the
            // other devices know the login with it (#537)
            (
                ConnectorKind::Comdirect,
                ConnectorKind::Comdirect.as_str().to_string(),
                bank_code.clone(),
            )
        };
        // the banking lock may block the new way (e.g. FinTS for this bank code)
        crate::banking_gate::check_connector(&new_key)?;
        let clash: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM bank_connections
                 WHERE connector = ?1 AND id <> ?2 AND deleted_ms IS NULL
                   AND lower(trim(login_name)) = lower(trim(?3)))",
            params![new_key, connection_id, login],
            |r| r.get(0),
        )?;
        if clash {
            return Err(CoreError::invalid(tr(
                "Für diesen Anmeldenamen gibt es den Zugang auf diesem Weg schon.",
            )));
        }
        let symbol = crate::connectors::descriptor(kind).sf_symbol;
        tx.execute(
            "UPDATE bank_connections SET connector = ?1, bank_code = ?2, sf_symbol = ?3, state_json = '{}'
              WHERE id = ?4",
            params![new_key, new_code, symbol, connection_id],
        )?;
        // FinTS address shown (and editable) in the login's settings; an
        // address the user set for this bank code before stays.
        if kind == ConnectorKind::FinTsGeneric {
            tx.execute(
                "INSERT INTO settings(key, value, is_secret) VALUES (?1, ?2, 0)
                 ON CONFLICT(key) DO NOTHING",
                params![
                    format!("fints.url.{new_code}"),
                    crate::connectors::fints_directory::COMDIRECT_FINTS_URL
                ],
            )?;
        }
        // A leftover of an earlier switch whose fetch never took it over
        // (before #537 was fixed): an account already carrying the marked
        // reference of one marked below. Its key is freed under a reference
        // no takeover looks at; without bookings it is the stale duplicate.
        let stale: Vec<(i64, bool, bool)> = tx
            .prepare(
                // includes soft-deleted rows: they hold the key just the same
                "SELECT s.id, s.deleted_ms IS NOT NULL,
                        EXISTS (SELECT 1 FROM transactions t WHERE t.account_id = s.id AND t.deleted_ms IS NULL)
                   FROM accounts s JOIN accounts a
                     ON a.connection_id = s.connection_id AND s.external_ref = ?2 || a.external_ref
                  WHERE s.connection_id = ?1 AND a.external_ref NOT LIKE 'moneymoney-db:%'",
            )?
            .query_map(params![connection_id, SWITCH_MARK], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?;
        for (id, deleted, has_bookings) in stale {
            tx.execute(
                "UPDATE accounts SET external_ref = 'switch-stale-' || id || ':' || substr(external_ref, ?2)
                  WHERE id = ?1",
                params![id, SWITCH_MARK.len() as i64 + 1],
            )?;
            if !deleted && !has_bookings {
                crate::sync_merge::soft_delete_account(&tx, id)?;
            }
        }
        // Accounts: re-adoptable by IBAN (`adopt_account` takes over
        // `moneymoney-db:*` refs of the same login on the next fetch).
        tx.execute(
            "UPDATE accounts SET external_ref = ?2 || external_ref
              WHERE connection_id = ?1 AND external_ref NOT LIKE 'moneymoney-db:%'
                AND external_ref NOT LIKE 'switch-stale-%'",
            params![connection_id, SWITCH_MARK],
        )?;
        // The same leftover for bookings: one an earlier switch marked and
        // the next fetch did not take back (outside its window), next to the
        // bank's booking that came again under its own id. Same booking: what
        // the user set on the stale twin goes over, its key is freed, it goes.
        let stale_bookings: Vec<(i64, i64, bool)> = tx
            .prepare(
                // includes soft-deleted twins: they hold the key just the same
                "SELECT s.id, b.id, s.deleted_ms IS NOT NULL
                   FROM transactions b JOIN accounts a ON a.id = b.account_id
                   JOIN transactions s ON s.account_id = b.account_id AND s.external_id = 'mm-switch-' || b.external_id
                  WHERE a.connection_id = ?1 AND b.deleted_ms IS NULL AND b.external_id NOT LIKE 'mm-%'",
            )?
            .query_map([connection_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?;
        for (stale, live, deleted) in stale_bookings {
            if !deleted {
                // SET expressions see the old values of the live booking
                tx.execute(
                    "UPDATE transactions SET
                        note = CASE WHEN transactions.note = '' THEN s.note ELSE transactions.note END,
                        note_ms = CASE WHEN transactions.note = '' THEN s.note_ms ELSE transactions.note_ms END,
                        checked = MAX(transactions.checked, s.checked),
                        checked_ms = CASE WHEN transactions.checked = 0 AND s.checked = 1
                                          THEN s.checked_ms ELSE transactions.checked_ms END,
                        category_id = CASE WHEN transactions.category_source <> 'manual' AND s.category_source = 'manual'
                                           THEN s.category_id ELSE transactions.category_id END,
                        category_ms = CASE WHEN transactions.category_source <> 'manual' AND s.category_source = 'manual'
                                           THEN s.category_ms ELSE transactions.category_ms END,
                        category_source = CASE WHEN transactions.category_source <> 'manual' AND s.category_source = 'manual'
                                               THEN 'manual' ELSE transactions.category_source END
                       FROM (SELECT note, note_ms, checked, checked_ms, category_id, category_ms, category_source
                               FROM transactions WHERE id = ?2) AS s
                      WHERE transactions.id = ?1",
                    params![live, stale],
                )?;
            }
            tx.execute(
                "UPDATE transactions SET external_id = 'switch-stale-' || id || ':' || substr(external_id, 11)
                  WHERE id = ?1",
                [stale],
            )?;
            if !deleted {
                crate::sync_merge::soft_delete_booking(&tx, stale)?;
            }
        }
        // Open card holds (#1134): the new way reports the ones still open on
        // its first fetch. Marked like bookings they would count as posted and
        // stay for good, as no hold cleanup looks at `mm-*`. Their id is freed
        // first: switching back brings the same hold id again, and a deleted id
        // stays deleted in `store_fetch`.
        let holds: Vec<i64> = tx
            .prepare(
                "SELECT t.id FROM transactions t JOIN accounts a ON a.id = t.account_id
                  WHERE a.connection_id = ?1 AND t.deleted_ms IS NULL AND t.external_id LIKE 'pending-hold-%'",
            )?
            .query_map([connection_id], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        for id in holds {
            tx.execute(
                "UPDATE transactions SET external_id = 'switch-stale-' || id || ':' || external_id
                  WHERE id = ?1",
                [id],
            )?;
            crate::sync_merge::soft_delete_booking(&tx, id)?;
        }
        // Live bookings: re-adoptable like imported ones (`mm-*`, with a
        // fingerprint). Tombstones keep their ids for multi-device merges.
        // Optimistic transfer bookings (`pending-<e2e>`) stay as they are:
        // they go when the bank's booking arrives, whichever way fetches it.
        let rows: Vec<(i64, String, i64, String, String, String)> = {
            let mut st = tx.prepare(
                "SELECT t.id, t.booking_date, t.amount_cents, t.counterparty_iban, t.counterparty, t.purpose
                   FROM transactions t JOIN accounts a ON a.id = t.account_id
                  WHERE a.connection_id = ?1 AND t.deleted_ms IS NULL AND t.external_id NOT LIKE 'mm-%'
                    AND t.external_id NOT LIKE 'switch-stale-%' AND t.external_id NOT LIKE 'pending-%'",
            )?;

            st.query_map([connection_id], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })?
            .collect::<Result<_, _>>()?
        };
        for (id, date, cents, iban, name, purpose) in rows {
            let fp = crate::importers::moneymoney_import::fingerprint(
                &date, cents, &iban, &name, &purpose,
            );
            tx.execute(
                "UPDATE transactions SET external_id = 'mm-switch-' || external_id,
                        import_fp = CASE WHEN import_fp = '' THEN ?2 ELSE import_fp END
                  WHERE id = ?1",
                params![id, fp],
            )?;
        }
        tx.commit()?;
        Ok(new_key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::BookingFilter;

    #[test]
    fn session_hand_over_and_clear_keep_the_other_state() {
        let dir = crate::test_support::TempDir::new("girocrab-session");
        let path = dir.join("v.db");
        let v = Vault::open(path.to_string_lossy().to_string(), vec![6u8; 32]).unwrap();
        let c = v
            .add_bank_connection(ConnectorKind::TradeRepublic, String::new(), "+49170".into())
            .unwrap();
        let stored = |v: &Vault| -> serde_json::Value {
            let db = v.db().unwrap();
            connection_state(&db, c).unwrap().decode().unwrap()
        };
        v.db()
            .unwrap()
            .execute(
                "UPDATE bank_connections SET state_json = '{\"doc_items\":[\"a\"]}' WHERE id = ?1",
                [c],
            )
            .unwrap();
        assert!(v.set_connection_session(c, "no-cookie".into()).is_err());

        v.set_connection_session(c, "tr_session=s".into()).unwrap();
        let status = v.connection_status(c).unwrap();
        assert!(status.has_session && status.session_saved_unix.is_some());
        let state = stored(&v);
        assert_eq!(
            (state["cookies"].as_str(), state["source"].as_str()),
            (Some("tr_session=s"), Some("browser"))
        );
        assert_eq!(state["doc_items"], serde_json::json!(["a"]));

        v.clear_connection_session(c).unwrap();
        assert!(!v.connection_status(c).unwrap().has_session);
        assert_eq!(stored(&v), serde_json::json!({"doc_items": ["a"]}));
    }

    #[test]
    fn a_stored_refresh_token_counts_as_session() {
        let dir = crate::test_support::TempDir::new("girocrab-refresh-session");
        let path = dir.join("v.db");
        let v = Vault::open(path.to_string_lossy().to_string(), vec![6u8; 32]).unwrap();
        let c = v
            .add_bank_connection(ConnectorKind::Kontist, String::new(), "a@b.de".into())
            .unwrap();
        assert!(!v.connection_status(c).unwrap().has_session);
        v.db()
            .unwrap()
            .execute(
                "UPDATE bank_connections SET state_json = '{\"refresh_token\":\"r\"}' WHERE id = ?1",
                [c],
            )
            .unwrap();
        assert!(v.connection_status(c).unwrap().has_session);
        v.clear_connection_session(c).unwrap();
        assert!(!v.connection_status(c).unwrap().has_session);
    }

    #[test]
    fn a_new_login_drops_the_stored_session_a_rename_keeps_it() {
        let dir = crate::test_support::TempDir::new("girocrab-login-change");
        let path = dir.join("v.db");
        let v = Vault::open(path.to_string_lossy().to_string(), vec![6u8; 32]).unwrap();
        let c = v
            .add_bank_connection(ConnectorKind::Kontist, String::new(), "a@b.de".into())
            .unwrap();
        let store = |v: &Vault| {
            v.db()
                .unwrap()
                .execute(
                    "UPDATE bank_connections SET state_json = '{\"refresh_token\":\"r\"}' WHERE id = ?1",
                    [c],
                )
                .unwrap();
        };
        store(&v);
        v.update_bank_connection(c, "Firma".into(), "a@b.de".into())
            .unwrap();
        assert!(v.connection_status(c).unwrap().has_session);
        v.update_bank_connection(c, "Firma".into(), "x@y.de".into())
            .unwrap();
        assert!(!v.connection_status(c).unwrap().has_session);
    }

    /// Direct debit, standing order and planned data hang on the duplicate
    /// account: merging must neither fail on the foreign keys nor lose them.
    #[test]
    fn dedup_moves_mandates_and_planned_data_to_the_kept_account() {
        let dir = crate::test_support::TempDir::new("girocrab-dedup-debit");
        let v = Vault::open(
            dir.join("v.db").to_string_lossy().to_string(),
            vec![4u8; 32],
        )
        .unwrap();
        let (keep_acc, dup_acc) = {
            let db = v.db().unwrap();
            db.execute("INSERT INTO account_groups(name) VALUES ('G')", [])
                .unwrap();
            let g = db.last_insert_rowid();
            let mut ids = Vec::new();
            for (n, id) in [(1, "a"), (2, "b")] {
                db.execute(
                    "INSERT INTO bank_connections(connector, display_name, login_name)
                     VALUES ('fints:52090000', 'VB', '1234567890')",
                    [],
                )
                .unwrap();
                let c = db.last_insert_rowid();
                db.execute(
                    "INSERT INTO accounts(connection_id, group_id, external_ref, name) VALUES (?1, ?2, 'DE1', ?3)",
                    params![c, g, format!("Giro {n}")],
                )
                .unwrap();
                ids.push((db.last_insert_rowid(), id));
            }
            (ids[0].0, ids[1].0)
        };
        {
            let db = v.db().unwrap();
            for (acc, reference) in [(keep_acc, "M-1"), (dup_acc, "M-1"), (dup_acc, "M-2")] {
                db.execute(
                    "INSERT INTO mandates(account_id, debtor_name, debtor_iban, reference, signed_date, created_ms)
                     VALUES (?1, 'X', 'DE89370400440532013000', ?2, '2025-01-01', 0)",
                    params![acc, reference],
                )
                .unwrap();
            }
            db.execute(
                "INSERT INTO creditor_ids(account_id, creditor_id, updated_ms) VALUES (?1, 'DE98ZZZ09999999999', 0)",
                [dup_acc],
            )
            .unwrap();
            db.execute(
                "INSERT INTO standing_orders(account_id, key, amount_cents, first_date, time_unit, interval, exec_day, fetched_ms)
                 VALUES (?1, 'k', 100, '2026-01-01', 'M', 1, 1, 0)",
                [dup_acc],
            )
            .unwrap();
            db.execute(
                "INSERT INTO planned_transfers(account_id, end_to_end_id, execution_date, amount_cents)
                 VALUES (?1, 'E1', '2026-12-01', 100)",
                [dup_acc],
            )
            .unwrap();
        }
        assert_eq!(v.deduplicate_bank_connections().unwrap(), 1);
        let db = v.db().unwrap();
        let one = |sql: &str, acc: i64| db.query_row(sql, [acc], |r| r.get::<_, i64>(0)).unwrap();
        let left: i64 = db
            .query_row(
                "SELECT id FROM accounts WHERE deleted_ms IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(left, keep_acc);
        // M-1 exists on both (the kept one wins), M-2 moves over
        assert_eq!(
            one("SELECT count(*) FROM mandates WHERE account_id = ?1", left),
            2
        );
        assert_eq!(
            one(
                "SELECT count(*) FROM creditor_ids WHERE account_id = ?1",
                left
            ),
            1
        );
        assert_eq!(
            one(
                "SELECT count(*) FROM planned_transfers WHERE account_id = ?1",
                left
            ),
            1
        );
    }

    #[test]
    fn comdirect_switches_between_api_and_fints_keeping_accounts_and_bookings() {
        let dir = crate::test_support::TempDir::new("girocrab-switch");
        let v = Vault::open(
            dir.join("v.db").to_string_lossy().to_string(),
            vec![9u8; 32],
        )
        .unwrap();
        let c = v
            .add_bank_connection(ConnectorKind::Comdirect, String::new(), "12345678".into())
            .unwrap();
        let (acc, booking) = {
            let db = v.db().unwrap();
            db.execute("INSERT INTO account_groups(name) VALUES ('G')", [])
                .unwrap();
            let g = db.last_insert_rowid();
            db.execute(
                "INSERT INTO accounts(connection_id, group_id, external_ref, name, iban)
                 VALUES (?1, ?2, 'A1B2C3', 'Girokonto', 'DE10 2004 1155 0123 4567 89')",
                params![c, g],
            )
            .unwrap();
            let acc = db.last_insert_rowid();
            db.execute(
                "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, counterparty, purpose, category_source, note)
                 VALUES (?1, 'rest-1', '2026-11-03', -8600, 'Vattenfall', 'Abschlag', 'manual', 'Strom')",
                [acc],
            )
            .unwrap();
            let booking = db.last_insert_rowid();
            db.execute(
                "UPDATE bank_connections SET state_json = '{\"access_token\":\"x\"}' WHERE id = ?1",
                [c],
            )
            .unwrap();
            (acc, booking)
        };

        assert_eq!(v.switch_comdirect_access(c).unwrap(), "fints:20041155");
        let conn = &v.list_bank_connections().unwrap()[0];
        assert_eq!(
            (conn.id, conn.connector, conn.bank_code.as_str()),
            (c, ConnectorKind::FinTsGeneric, "20041155")
        );
        assert_eq!(conn.login_name, "12345678");
        {
            let db = v.db().unwrap();
            let state: String = db
                .query_row(
                    "SELECT state_json FROM bank_connections WHERE id = ?1",
                    [c],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(state, "{}");
            let url: String = db
                .query_row(
                    "SELECT value FROM settings WHERE key = 'fints.url.20041155'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(url, crate::connectors::fints_directory::COMDIRECT_FINTS_URL);
            let (ext_ref, conn_id): (String, i64) = db
                .query_row(
                    "SELECT external_ref, connection_id FROM accounts WHERE id = ?1",
                    [acc],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(
                (ext_ref.as_str(), conn_id),
                ("moneymoney-db:switch:A1B2C3", c)
            );
            // the first FinTS fetch takes the account over by IBAN …
            let adopted = crate::importers::moneymoney_db::adopt_account(
                &db,
                c,
                "DE10200411550123456789",
                "DE10200411550123456789",
                "EUR",
                crate::models::AccountKind::Checking,
            )
            .unwrap();
            assert_eq!(adopted, Some(acc));
            // … and the booking, although FinTS sends another id and text
            assert!(
                crate::importers::moneymoney_import::adopt_import(
                    &db,
                    acc,
                    "fints-77",
                    "2026-11-04",
                    -8600,
                    "",
                    "VATTENFALL EUROPE SALES GMBH",
                    "EREF+123 Abschlag Strom",
                    "LASTSCHRIFT",
                )
                .unwrap()
            );
            let (ext, note, source): (String, String, String) = db
                .query_row(
                    "SELECT external_id, note, category_source FROM transactions WHERE id = ?1",
                    [booking],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            assert_eq!(
                (ext.as_str(), note.as_str(), source.as_str()),
                ("fints-77", "Strom", "manual")
            );
            let n: i64 = db
                .query_row(
                    "SELECT count(*) FROM transactions WHERE account_id = ?1",
                    [acc],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1);
        }

        // and back to the API
        assert_eq!(v.switch_comdirect_access(c).unwrap(), "comdirect");
        let conn = &v.list_bank_connections().unwrap()[0];
        assert_eq!(
            (conn.connector, conn.bank_code.as_str()),
            (ConnectorKind::Comdirect, "20041155")
        );

        // other banks are refused
        let ing = v
            .add_fints_connection(
                "50010517".into(),
                "https://fints.ing.de/fints/".into(),
                "ING".into(),
                "ing-user".into(),
                String::new(),
            )
            .unwrap();
        assert!(v.switch_comdirect_access(ing).is_err());
        assert!(v.switch_comdirect_access(9999).is_err());
    }

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
            // portal user name of this login, and the PIN of another one ("pin.conn.<id>0")
            for key in [
                format!("{}.login", pin_key(conn)),
                format!("{}0", pin_key(conn)),
            ] {
                db.execute(
                    "INSERT INTO settings(key, value, is_secret) VALUES (?1, 'x', 1)",
                    [key],
                )
                .unwrap();
            }
            for (n, sub) in [("l1", 1), ("l2", 2)] {
                db.execute("INSERT INTO accounts(connection_id, group_id, external_ref, name, kind) VALUES (?1, ?2, ?3, 'Darlehen', 'loan')", params![conn, g, n]).unwrap();
                let id = db.last_insert_rowid();
                db.execute("INSERT INTO transactions(account_id, external_id, booking_date, amount_cents) VALUES (?1, 'x', '2026-11-01', ?2)", params![id, sub]).unwrap();
                if sub == 1 { a1 = id } else { a2 = id }
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
        assert!(
            v.get_setting(format!("{}.login", pin_key(conn)))
                .unwrap()
                .is_none(),
            "portal user name removed"
        );
        assert!(
            v.get_setting(format!("{}0", pin_key(conn)))
                .unwrap()
                .is_some(),
            "other logins keep theirs"
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
            q(
                "SELECT count(*) FROM accounts a JOIN bank_connections c ON c.id = a.connection_id WHERE c.deleted_ms IS NULL AND a.deleted_ms IS NULL"
            ),
            1
        );
        let _ = std::fs::remove_file(p);
    }

    /// A comdirect login (bank code set, as after a database import) with a
    /// giro account, a depot without IBAN, a deleted imported twin depot and
    /// one booking. Returns (vault, connection, giro, depot, booking).
    fn comdirect_vault(
        dir: &std::path::Path,
        file: &str,
    ) -> (std::sync::Arc<Vault>, i64, i64, i64, i64) {
        let v = Vault::open(dir.join(file).to_string_lossy().to_string(), vec![9u8; 32]).unwrap();
        let c = v
            .add_bank_connection(ConnectorKind::Comdirect, String::new(), "12345678".into())
            .unwrap();
        let db = v.db().unwrap();
        db.execute(
            "UPDATE bank_connections SET bank_code = '20041155' WHERE id = ?1",
            [c],
        )
        .unwrap();
        db.execute("INSERT INTO account_groups(name) VALUES ('G')", [])
            .unwrap();
        let g = db.last_insert_rowid();
        let add = |ext: &str, name: &str, kind: &str, iban: &str| -> i64 {
            db.execute(
                "INSERT INTO accounts(connection_id, group_id, external_ref, name, kind, iban)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![c, g, ext, name, kind, iban],
            )
            .unwrap();
            db.last_insert_rowid()
        };
        let giro = add(
            "A1B2C3",
            "Girokonto",
            "checking",
            "DE10 2004 1155 0123 4567 89",
        );
        let depot = add("dep-1", "Depot", "brokerage", "");
        let twin = add("moneymoney-db:old-depot", "Depot", "brokerage", "");
        db.execute("UPDATE accounts SET deleted_ms = 1 WHERE id = ?1", [twin])
            .unwrap();
        db.execute(
            "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, counterparty, purpose)
             VALUES (?1, 'rest-1', '2026-11-03', -8600, 'Vattenfall', 'Abschlag')",
            [giro],
        )
        .unwrap();
        let booking = db.last_insert_rowid();
        drop(db);
        (v, c, giro, depot, booking)
    }

    /// #537: there and back again keeps the bank code, and the next fetch
    /// takes the depot (no IBAN, a deleted twin of its kind) over again.
    #[test]
    fn switching_back_keeps_bank_code_and_readopts_the_depot() {
        let dir = crate::test_support::TempDir::new("girocrab-switch-back");
        let (v, c, _, depot, _) = comdirect_vault(dir.path(), "v.db");
        v.switch_comdirect_access(c).unwrap();
        assert_eq!(v.switch_comdirect_access(c).unwrap(), "comdirect");
        let conn = &v.list_bank_connections().unwrap()[0];
        assert_eq!(conn.bank_code, "20041155");
        let db = v.db().unwrap();
        let adopted = crate::importers::moneymoney_db::adopt_account(
            &db,
            c,
            "dep-1",
            "",
            "EUR",
            crate::models::AccountKind::Brokerage,
        )
        .unwrap();
        assert_eq!(adopted, Some(depot));
        let ext: String = db
            .query_row(
                "SELECT external_ref FROM accounts WHERE id = ?1",
                [depot],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ext, "dep-1");
    }

    /// #537: device A switches to FinTS and fetches; device B never switched.
    /// Merging both ways keeps one connection, one account each, one booking.
    #[test]
    fn a_switch_on_one_device_does_not_duplicate_on_the_other() {
        let dir = crate::test_support::TempDir::new("girocrab-switch-sync");
        let (a, c, giro, _, booking) = comdirect_vault(dir.path(), "a.db");
        let b_path = dir.join("b.db").to_string_lossy().to_string();
        a.write_snapshot(b_path.clone()).unwrap();
        let b = Vault::open(b_path.clone(), vec![9u8; 32]).unwrap();

        a.switch_comdirect_access(c).unwrap();
        {
            // A's first FinTS fetch: the giro account by IBAN, the booking by fingerprint
            let db = a.db().unwrap();
            let iban = "DE10200411550123456789";
            let adopted = crate::importers::moneymoney_db::adopt_account(
                &db,
                c,
                iban,
                iban,
                "EUR",
                crate::models::AccountKind::Checking,
            )
            .unwrap();
            assert_eq!(adopted, Some(giro));
            assert!(
                crate::importers::moneymoney_import::adopt_import(
                    &db,
                    giro,
                    "fints-77",
                    "2026-11-03",
                    -8600,
                    "",
                    "VATTENFALL",
                    "Abschlag",
                    "LASTSCHRIFT",
                )
                .unwrap()
            );
            let ext: String = db
                .query_row(
                    "SELECT external_id FROM transactions WHERE id = ?1",
                    [booking],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(ext, "fints-77");
        }

        let a_path = dir.join("a-snap.db").to_string_lossy().to_string();
        a.write_snapshot(a_path.clone()).unwrap();
        b.merge_other_vault(a_path, vec![9u8; 32]).unwrap();
        let b_snap = dir.join("b-snap.db").to_string_lossy().to_string();
        b.write_snapshot(b_snap.clone()).unwrap();
        a.merge_other_vault(b_snap, vec![9u8; 32]).unwrap();

        for (name, v) in [("A", &a), ("B", &b)] {
            let db = v.db().unwrap();
            let n = |sql: &str| -> i64 { db.query_row(sql, [], |r| r.get(0)).unwrap() };
            assert_eq!(
                n("SELECT count(*) FROM bank_connections WHERE deleted_ms IS NULL"),
                1,
                "{name}: connections"
            );
            assert_eq!(
                n("SELECT count(*) FROM accounts WHERE deleted_ms IS NULL AND kind = 'checking'"),
                1,
                "{name}: giro accounts"
            );
            assert_eq!(
                n("SELECT count(*) FROM accounts WHERE deleted_ms IS NULL AND kind = 'brokerage'"),
                1,
                "{name}: depots"
            );
            assert_eq!(
                n("SELECT count(*) FROM transactions WHERE deleted_ms IS NULL"),
                1,
                "{name}: bookings"
            );
        }
    }

    /// #537, the reported case: there and back again without a FinTS fetch,
    /// the next fetch over the API, then a sync with a device that never switched.
    #[test]
    fn there_and_back_then_sync_keeps_one_of_everything() {
        let dir = crate::test_support::TempDir::new("girocrab-switch-roundtrip");
        let (a, c, giro, depot, booking) = comdirect_vault(dir.path(), "a.db");
        let b_path = dir.join("b.db").to_string_lossy().to_string();
        a.write_snapshot(b_path.clone()).unwrap();
        let b = Vault::open(b_path, vec![9u8; 32]).unwrap();

        a.switch_comdirect_access(c).unwrap();
        a.switch_comdirect_access(c).unwrap();
        {
            // the API fetch: same references and ids as before the switch
            let db = a.db().unwrap();
            let iban = "DE10200411550123456789";
            let take = |ext: &str, iban: &str, kind| {
                crate::importers::moneymoney_db::adopt_account(&db, c, ext, iban, "EUR", kind)
                    .unwrap()
            };
            assert_eq!(
                take("A1B2C3", iban, crate::models::AccountKind::Checking),
                Some(giro)
            );
            assert_eq!(
                take("dep-1", "", crate::models::AccountKind::Brokerage),
                Some(depot)
            );
            assert!(
                crate::importers::moneymoney_import::adopt_import(
                    &db,
                    giro,
                    "rest-1",
                    "2026-11-03",
                    -8600,
                    "",
                    "Vattenfall",
                    "Abschlag",
                    "",
                )
                .unwrap()
            );
            let ext: String = db
                .query_row(
                    "SELECT external_id FROM transactions WHERE id = ?1",
                    [booking],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(ext, "rest-1");
        }

        let a_snap = dir.join("a-snap.db").to_string_lossy().to_string();
        a.write_snapshot(a_snap.clone()).unwrap();
        b.merge_other_vault(a_snap, vec![9u8; 32]).unwrap();
        let b_snap = dir.join("b-snap.db").to_string_lossy().to_string();
        b.write_snapshot(b_snap.clone()).unwrap();
        a.merge_other_vault(b_snap, vec![9u8; 32]).unwrap();

        for (name, v) in [("A", &a), ("B", &b)] {
            let db = v.db().unwrap();
            let n = |sql: &str| -> i64 { db.query_row(sql, [], |r| r.get(0)).unwrap() };
            assert_eq!(
                n("SELECT count(*) FROM bank_connections WHERE deleted_ms IS NULL"),
                1,
                "{name}: connections"
            );
            assert_eq!(
                n("SELECT count(*) FROM accounts WHERE deleted_ms IS NULL"),
                2,
                "{name}: accounts"
            );
            assert_eq!(
                n("SELECT count(*) FROM transactions WHERE deleted_ms IS NULL"),
                1,
                "{name}: bookings"
            );
        }
    }

    /// Adds a live account on connection `c` with reference `ext`, optionally with one booking.
    fn add_account(v: &Vault, c: i64, ext: &str, kind: &str, booking: bool) -> i64 {
        let db = v.db().unwrap();
        db.execute(
            "INSERT INTO accounts(connection_id, group_id, external_ref, name, kind)
             VALUES (?1, (SELECT id FROM account_groups LIMIT 1), ?2, 'Depot', ?3)",
            params![c, ext, kind],
        )
        .unwrap();
        let id = db.last_insert_rowid();
        if booking {
            db.execute(
                "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, counterparty, purpose)
                 VALUES (?1, 'old-1', '2026-11-01', -100, 'X', 'Y')",
                [id],
            )
            .unwrap();
        }
        id
    }

    /// The reported case after #537: an earlier switch left a depot marked,
    /// the next fetch added a second one with the plain reference. Switching
    /// again must not fail on the unique key, and leaves one depot that the
    /// next fetch finds.
    #[test]
    fn switching_again_clears_an_empty_marked_twin_of_an_earlier_switch() {
        let dir = crate::test_support::TempDir::new("girocrab-switch-stale");
        let (v, c, _, depot, _) = comdirect_vault(dir.path(), "v.db");
        let stale = add_account(&v, c, &format!("{SWITCH_MARK}dep-1"), "brokerage", false);

        assert_eq!(v.switch_comdirect_access(c).unwrap(), "fints:20041155");
        let db = v.db().unwrap();
        let live: Vec<i64> = db
            .prepare("SELECT id FROM accounts WHERE connection_id = ?1 AND kind = 'brokerage' AND deleted_ms IS NULL")
            .unwrap()
            .query_map([c], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            live,
            vec![depot],
            "the stale twin is gone, the fetched depot stays"
        );
        let stale_ref: String = db
            .query_row(
                "SELECT external_ref FROM accounts WHERE id = ?1",
                [stale],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            !stale_ref.starts_with("moneymoney-db:"),
            "no takeover candidate any more: {stale_ref}"
        );
        // the way back finds the depot by its old reference
        drop(db);
        v.switch_comdirect_access(c).unwrap();
        let db = v.db().unwrap();
        let adopted = crate::importers::moneymoney_db::adopt_account(
            &db,
            c,
            "dep-1",
            "",
            "EUR",
            crate::models::AccountKind::Brokerage,
        )
        .unwrap();
        assert_eq!(adopted, Some(depot));
    }

    /// A marked twin that still has bookings is not thrown away: its key is
    /// freed, it stays visible.
    #[test]
    fn a_marked_twin_with_bookings_stays() {
        let dir = crate::test_support::TempDir::new("girocrab-switch-stale-bookings");
        let (v, c, _, _, _) = comdirect_vault(dir.path(), "v.db");
        let stale = add_account(&v, c, &format!("{SWITCH_MARK}dep-1"), "brokerage", true);
        v.switch_comdirect_access(c).unwrap();
        let db = v.db().unwrap();
        let deleted: Option<i64> = db
            .query_row(
                "SELECT deleted_ms FROM accounts WHERE id = ?1",
                [stale],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(deleted, None);
    }

    /// The same leftover for bookings: an earlier switch marked a booking the
    /// next fetch did not take back (outside its window); the bank's booking
    /// came again under its own id. Switching again must not fail; the stale
    /// twin goes, what the user set on it stays with the live booking.
    #[test]
    fn switching_again_merges_a_stale_marked_booking_into_the_live_one() {
        let dir = crate::test_support::TempDir::new("girocrab-switch-stale-booking");
        let (v, c, giro, _, booking) = comdirect_vault(dir.path(), "v.db");
        let stale = {
            let db = v.db().unwrap();
            db.execute(
                "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, counterparty,
                                          purpose, note, note_ms, checked, checked_ms)
                 VALUES (?1, 'mm-switch-rest-1', '2026-11-03', -8600, 'Vattenfall', 'Abschlag', 'Strom', 5, 1, 5)",
                [giro],
            )
            .unwrap();
            db.last_insert_rowid()
        };

        assert_eq!(v.switch_comdirect_access(c).unwrap(), "fints:20041155");
        let db = v.db().unwrap();
        let live: Vec<(i64, String, String, bool)> = db
            .prepare(
                "SELECT id, external_id, note, checked FROM transactions
                  WHERE account_id = ?1 AND deleted_ms IS NULL",
            )
            .unwrap()
            .query_map([giro], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            live,
            vec![(
                booking,
                "mm-switch-rest-1".to_string(),
                "Strom".to_string(),
                true
            )]
        );
        let stale_id: String = db
            .query_row(
                "SELECT external_id FROM transactions WHERE id = ?1",
                [stale],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            !stale_id.starts_with("mm-"),
            "no takeover candidate any more: {stale_id}"
        );
    }

    // ---------------------------------------------------------------- card holds (#1134)

    /// A card payment of 25,00 € the bank reports as hold `id` (no counterparty,
    /// short text) or, with a `camt:` id, as the posted booking.
    fn card(id: &str, date: &str, purpose: &str) -> crate::connectors::RemoteTransaction {
        crate::connectors::RemoteTransaction {
            account_ref: "A1B2C3".into(),
            external_id: id.into(),
            booking_date: date.into(),
            amount_cents: -2500,
            currency: "EUR".into(),
            purpose: purpose.into(),
            ..Default::default()
        }
    }

    /// One fetch of the giro account over `kind`, with the complete hold list
    /// of that way (`hold_prefix`).
    fn fetch_giro(
        v: &Vault,
        kind: ConnectorKind,
        c: i64,
        transactions: Vec<crate::connectors::RemoteTransaction>,
    ) {
        let hold_prefix = if kind == ConnectorKind::Comdirect {
            "pending-hold-comdirect-"
        } else {
            "pending-hold-fints-"
        };
        crate::connectors::store_fetch(
            v,
            kind,
            c,
            crate::connectors::FetchResult {
                accounts: vec![crate::connectors::RemoteAccount {
                    external_ref: "A1B2C3".into(),
                    name: "Girokonto".into(),
                    owner: String::new(),
                    iban: "DE10200411550123456789".into(),
                    account_number: String::new(),
                    currency: "EUR".into(),
                    kind: crate::models::AccountKind::Checking,
                    balance_cents: 0,
                    balance_date: "2026-11-20".into(),
                }],
                transactions,
                hold_prefix: Some(hold_prefix.into()),
                ..Default::default()
            },
        )
        .unwrap();
    }

    /// Live bookings of the account: (external id, booking date, purpose).
    fn live_rows(v: &Vault, account: i64) -> Vec<(String, String, String)> {
        let db = v.db().unwrap();
        let mut st = db
            .prepare(
                "SELECT external_id, booking_date, purpose FROM transactions
                  WHERE account_id = ?1 AND deleted_ms IS NULL ORDER BY id",
            )
            .unwrap();
        st.query_map([account], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    const HOLD_TEXT: &str = "SHOP, Berlin";

    /// The reported case: switching while a card payment is a hold left it as
    /// a posted booking (`mm-switch-pending-…`) next to the hold the new way
    /// reports, one more per switch. Now the hold is dropped on the switch,
    /// and switching back and forth with the same hold open keeps exactly one.
    #[test]
    fn switching_with_an_open_hold_keeps_exactly_one_hold() {
        let dir = crate::test_support::TempDir::new("girocrab-switch-hold");
        let (v, c, giro, _, _) = comdirect_vault(dir.path(), "v.db");
        let rest_hold = card("pending-hold-comdirect-h1", "2026-11-19", HOLD_TEXT);
        let fints_hold = card("pending-hold-fints-h1", "2026-11-18", HOLD_TEXT);
        // an optimistic transfer booking of the app itself
        {
            let db = v.db().unwrap();
            db.execute(
                "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, purpose)
                 VALUES (?1, 'pending-e2e-1', '2026-11-19', -5000, 'Miete')",
                [giro],
            )
            .unwrap();
        }
        fetch_giro(&v, ConnectorKind::Comdirect, c, vec![rest_hold.clone()]);
        let holds = |v: &Vault| -> Vec<String> {
            live_rows(v, giro)
                .into_iter()
                .map(|r| r.0)
                .filter(|e| e.contains("pending-hold-"))
                .collect()
        };
        assert_eq!(holds(&v), vec!["pending-hold-comdirect-h1"]);

        for round in 0..2 {
            assert_eq!(v.switch_comdirect_access(c).unwrap(), "fints:20041155");
            assert!(
                holds(&v).is_empty(),
                "round {round}: hold dropped on the switch"
            );
            fetch_giro(&v, ConnectorKind::FinTsGeneric, c, vec![fints_hold.clone()]);
            assert_eq!(
                holds(&v),
                vec!["pending-hold-fints-h1"],
                "round {round}: FinTS"
            );

            assert_eq!(v.switch_comdirect_access(c).unwrap(), "comdirect");
            assert!(
                holds(&v).is_empty(),
                "round {round}: hold dropped on the way back"
            );
            // the same hold id as before the first switch is stored again
            fetch_giro(&v, ConnectorKind::Comdirect, c, vec![rest_hold.clone()]);
            assert_eq!(
                holds(&v),
                vec!["pending-hold-comdirect-h1"],
                "round {round}: API"
            );
        }
        // the app's own transfer booking was neither marked nor dropped
        assert!(live_rows(&v, giro).iter().any(|r| r.0 == "pending-e2e-1"));
        // the dropped holds went as tombstones (other devices drop them too)
        let db = v.db().unwrap();
        let tombstones: i64 = db
            .query_row(
                "SELECT count(*) FROM transactions
                  WHERE external_id LIKE 'switch-stale-%pending-hold-%' AND deleted_ms IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tombstones, 4);
    }

    /// Device B: a copy of A's vault.
    fn second_device(dir: &std::path::Path, a: &Vault) -> std::sync::Arc<Vault> {
        let b_path = dir.join("b.db").to_string_lossy().to_string();
        a.write_snapshot(b_path.clone()).unwrap();
        Vault::open(b_path, vec![9u8; 32]).unwrap()
    }

    /// One iCloud round: A's snapshot into B, then B's into A.
    fn sync_both(dir: &std::path::Path, a: &Vault, b: &Vault, step: &str) {
        let a_snap = dir
            .join(format!("a-{step}.db"))
            .to_string_lossy()
            .to_string();
        a.write_snapshot(a_snap.clone()).unwrap();
        b.merge_other_vault(a_snap, vec![9u8; 32]).unwrap();
        let b_snap = dir
            .join(format!("b-{step}.db"))
            .to_string_lossy()
            .to_string();
        b.write_snapshot(b_snap.clone()).unwrap();
        a.merge_other_vault(b_snap, vec![9u8; 32]).unwrap();
    }

    /// Rows with external id `ext`, live or deleted.
    fn rows_with_id(v: &Vault, ext: &str, live: bool) -> i64 {
        v.db()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM transactions WHERE external_id = ?1 AND (deleted_ms IS NULL) = ?2",
                params![ext, live],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// Two devices know the same open hold, device A switches. The merge
    /// pairs A's dropped hold with B's by fingerprint; the pair keeps the
    /// freed id, so the hold the bank reports again after switching back is
    /// live on both devices, not held deleted under its own id.
    #[test]
    fn a_hold_dropped_on_one_device_comes_back_on_both_after_switching_back() {
        let dir = crate::test_support::TempDir::new("girocrab-switch-hold-sync");
        let (a, c, _, _, _) = comdirect_vault(dir.path(), "a.db");
        let hold = card("pending-hold-comdirect-h1", "2026-11-19", HOLD_TEXT);
        fetch_giro(&a, ConnectorKind::Comdirect, c, vec![hold.clone()]);
        let b = second_device(dir.path(), &a);
        let id = "pending-hold-comdirect-h1";

        a.switch_comdirect_access(c).unwrap();
        sync_both(dir.path(), &a, &b, "switch");
        for (name, v) in [("A", &a), ("B", &b)] {
            assert_eq!(rows_with_id(v, id, true), 0, "{name}: hold dropped");
            assert_eq!(
                rows_with_id(v, id, false),
                0,
                "{name}: the hold's own id is free"
            );
        }

        assert_eq!(a.switch_comdirect_access(c).unwrap(), "comdirect");
        fetch_giro(&a, ConnectorKind::Comdirect, c, vec![hold]);
        sync_both(dir.path(), &a, &b, "back");
        for (name, v) in [("A", &a), ("B", &b)] {
            assert_eq!(rows_with_id(v, id, true), 1, "{name}: the hold is back");
        }
    }

    /// The posted payment can carry the date, amount and text of the hold a
    /// switch dropped on the other device. The merge pairs the dropped hold
    /// with the hold, never with the booking: the booking stays, the hold is
    /// gone on both devices.
    #[test]
    fn a_dropped_hold_never_deletes_a_posted_booking_on_merge() {
        let dir = crate::test_support::TempDir::new("girocrab-switch-hold-posted-sync");
        let (a, c, _, _, _) = comdirect_vault(dir.path(), "a.db");
        let hold = card("pending-hold-comdirect-h1", "2026-11-19", HOLD_TEXT);
        fetch_giro(&a, ConnectorKind::Comdirect, c, vec![hold]);
        let b = second_device(dir.path(), &a);

        a.switch_comdirect_access(c).unwrap();
        // B, not switched yet, fetches the posted payment
        fetch_giro(
            &b,
            ConnectorKind::Comdirect,
            c,
            vec![card("REF1", "2026-11-19", HOLD_TEXT)],
        );
        sync_both(dir.path(), &a, &b, "posted");
        for (name, v) in [("A", &a), ("B", &b)] {
            assert_eq!(rows_with_id(v, "REF1", true), 1, "{name}: booking stays");
            assert_eq!(
                rows_with_id(v, "pending-hold-comdirect-h1", true),
                0,
                "{name}: hold gone"
            );
        }
    }

    /// The same for a hold an older switch had marked and the repair dropped:
    /// its tombstone must not take the posted booking of another device along.
    #[test]
    fn a_dropped_marked_hold_never_deletes_a_posted_booking_on_merge() {
        let dir = crate::test_support::TempDir::new("girocrab-marked-hold-posted-sync");
        let (a, _, giro, _, _) = comdirect_vault(dir.path(), "a.db");
        let b = second_device(dir.path(), &a);
        let add = |v: &Vault, ext: &str| {
            v.db()
                .unwrap()
                .execute(
                    "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, currency, purpose)
                     VALUES (?1, ?2, '2026-11-19', -2500, 'EUR', ?3)",
                    params![giro, ext, HOLD_TEXT],
                )
                .unwrap();
        };
        add(&a, "mm-switch-pending-hold-fints-h3");
        assert_eq!(drop_switched_holds(&a.db().unwrap()).unwrap(), 1);
        add(&b, "camt:REF3");

        sync_both(dir.path(), &a, &b, "posted");
        for (name, v) in [("A", &a), ("B", &b)] {
            assert_eq!(
                rows_with_id(v, "camt:REF3", true),
                1,
                "{name}: booking stays"
            );
            assert_eq!(
                rows_with_id(v, "mm-switch-pending-hold-fints-h3", true),
                0,
                "{name}: marked hold gone"
            );
        }
    }

    /// After the switch the bank posts the payment: one row with the
    /// booking's own date and text, no hold left behind.
    #[test]
    fn a_hold_posted_after_the_switch_leaves_one_booking() {
        let dir = crate::test_support::TempDir::new("girocrab-switch-hold-posted");
        let (v, c, giro, _, _) = comdirect_vault(dir.path(), "v.db");
        fetch_giro(
            &v,
            ConnectorKind::Comdirect,
            c,
            vec![card("pending-hold-comdirect-h1", "2026-11-19", HOLD_TEXT)],
        );
        v.switch_comdirect_access(c).unwrap();
        let posted = card(
            "camt:REF1",
            "2026-11-20",
            "SHOP, Berlin 19.11. 18:05 comdirect Visa-Debitkarte 1234",
        );
        fetch_giro(&v, ConnectorKind::FinTsGeneric, c, vec![posted.clone()]);
        let cards: Vec<_> = live_rows(&v, giro)
            .into_iter()
            .filter(|r| r.0 != "mm-switch-rest-1")
            .collect();
        assert_eq!(
            cards,
            vec![(posted.external_id, posted.booking_date, posted.purpose)]
        );
    }

    /// An older build on device A marked an open transfer of the app on its
    /// switch (`mm-switch-pending-<e2e>`); device B still has it unmarked.
    /// The merge pairs both forms of the one transfer: one row on each device.
    #[test]
    fn both_forms_of_a_transfer_of_the_app_stay_one_row_on_merge() {
        let dir = crate::test_support::TempDir::new("girocrab-marked-transfer-sync");
        let (a, _, giro, _, _) = comdirect_vault(dir.path(), "a.db");
        let b = second_device(dir.path(), &a);
        let add = |v: &Vault, ext: &str| {
            v.db()
                .unwrap()
                .execute(
                    "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, currency,
                                              counterparty, counterparty_iban, purpose)
                     VALUES (?1, ?2, '2026-11-19', -5000, 'EUR', 'Vermieter', 'DE02120300000000202051', 'Miete')",
                    params![giro, ext],
                )
                .unwrap();
        };
        add(&a, "mm-switch-pending-e2e-1");
        add(&b, "pending-e2e-1");

        sync_both(dir.path(), &a, &b, "transfer");
        for (name, v) in [("A", &a), ("B", &b)] {
            let n: i64 = v
                .db()
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM transactions
                      WHERE purpose = 'Miete' AND deleted_ms IS NULL",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "{name}: one row for the transfer");
        }
    }

    /// Vaults switched by an older build: their marked holds go on open (and
    /// after a merge), marked bookings and marked transfer bookings of the app
    /// stay; running it again changes nothing.
    #[test]
    fn marked_holds_of_an_older_switch_are_dropped_on_open() {
        let dir = crate::test_support::TempDir::new("girocrab-switch-hold-repair");
        let path = dir.join("v.db").to_string_lossy().to_string();
        let giro = {
            let (v, _, giro, _, booking) = comdirect_vault(dir.path(), "v.db");
            let db = v.db().unwrap();
            db.execute(
                "UPDATE transactions SET external_id = 'mm-switch-rest-1' WHERE id = ?1",
                [booking],
            )
            .unwrap();
            for ext in [
                "mm-switch-pending-hold-fints-h1",
                "mm-switch-pending-hold-comdirect-h1",
            ] {
                db.execute(
                    "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, purpose,
                                              category_source, note)
                     VALUES (?1, ?2, '2026-11-18', -2500, 'SHOP, Berlin', 'manual', 'Einkauf')",
                    params![giro, ext],
                )
                .unwrap();
            }
            // a transfer of the app, still open when the older build switched
            db.execute(
                "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, purpose,
                                          category_source, note)
                 VALUES (?1, 'mm-switch-pending-e2e-1', '2026-11-18', -5000, 'Miete', 'manual', 'Wohnung')",
                [giro],
            )
            .unwrap();
            giro
        };
        let v = Vault::open(path, vec![9u8; 32]).unwrap();
        assert_eq!(
            live_rows(&v, giro)
                .into_iter()
                .map(|r| r.0)
                .collect::<Vec<_>>(),
            vec!["mm-switch-rest-1", "mm-switch-pending-e2e-1"]
        );
        let db = v.db().unwrap();
        assert_eq!(drop_switched_holds(&db).unwrap(), 0);
    }

    /// A vault the repair has not reached yet (another device, before the
    /// merge): a booking must not take over a marked hold and keep its date
    /// and text. A marked booking and a marked transfer booking of the app
    /// are still taken over.
    #[test]
    fn a_booking_never_takes_over_a_marked_hold() {
        let dir = crate::test_support::TempDir::new("girocrab-switch-hold-twin");
        let (v, _, giro, _, _) = comdirect_vault(dir.path(), "v.db");
        let db = v.db().unwrap();
        let add = |ext: &str| {
            db.execute(
                "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, purpose)
                 VALUES (?1, ?2, '2026-11-19', -2500, 'SHOP, Berlin 19.11. comdirect Visa-Debitkarte')",
                params![giro, ext],
            )
            .unwrap();
        };
        let take = |ext: &str| {
            crate::importers::moneymoney_import::adopt_import(
                &db,
                giro,
                ext,
                "2026-11-20",
                -2500,
                "",
                "SHOP",
                "SHOP, Berlin 19.11. comdirect Visa-Debitkarte 1234",
                "",
            )
            .unwrap()
        };
        add("mm-switch-pending-hold-comdirect-h1");
        assert!(!take("camt:REF1"), "a marked hold is no twin");
        add("mm-switch-rest-9");
        assert!(take("camt:REF2"), "a marked booking still is");
        add("mm-switch-pending-e2e-1");
        assert!(take("camt:REF3"), "and a marked transfer of the app");
    }
}
