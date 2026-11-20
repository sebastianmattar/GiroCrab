//! Per-login account selection, shared by all connector imports. No ledger migration.
use super::FetchResult;
use crate::{
    error::{CoreError, CoreResult},
    models::ConnectorKind,
    vault::Vault,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, sync::Arc};

#[derive(Clone, Debug, Serialize, Deserialize, uniffi::Record)]
pub struct ConnectionAccountChoice {
    pub account_ref: String,
    pub name: String,
    pub number: String,
    pub owner: String,
    pub enabled: bool,
    pub pending: bool,
    pub existing_connection: String,
}
pub(crate) fn key(db: &Connection, id: i64) -> CoreResult<String> {
    Ok(format!(
        "account_selection:{}",
        crate::sync_merge::connection_key(db, id)?
    ))
}
fn load(db: &Connection, id: i64) -> CoreResult<Vec<ConnectionAccountChoice>> {
    let raw: Option<String> = db
        .query_row(
            "SELECT value FROM settings WHERE key=?1",
            [key(db, id)?],
            |r| r.get(0),
        )
        .optional()?;
    raw.map(|s| {
        serde_json::from_str(&s).map_err(|_| CoreError::invalid("Invalid account selection"))
    })
    .unwrap_or_else(|| Ok(vec![]))
}
fn save(db: &Connection, id: i64, rows: &[ConnectionAccountChoice]) -> CoreResult<()> {
    let raw = serde_json::to_string(rows).map_err(|e| CoreError::invalid(e.to_string()))?;
    db.execute("INSERT INTO settings(key,value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key(db,id)?,raw])?;
    Ok(())
}
fn normalized(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_uppercase)
        .collect()
}

pub(super) fn prepare(
    db: &Connection,
    kind: ConnectorKind,
    id: i64,
    data: &FetchResult,
) -> CoreResult<(HashSet<String>, HashSet<String>)> {
    let mut choices = load(db, id)?;
    // A held-back account this fetch no longer delivers and that was never
    // stored (a card matched to another account since) is dropped.
    let mut stale = Vec::new();
    for c in choices.iter().filter(|c| c.pending) {
        let stored: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM accounts WHERE connection_id=?1 AND external_ref=?2)",
            params![id, c.account_ref],
            |r| r.get(0),
        )?;
        if !stored
            && !data
                .accounts
                .iter()
                .any(|a| a.external_ref == c.account_ref)
        {
            stale.push(c.account_ref.clone());
        }
    }
    choices.retain(|c| !stale.contains(&c.account_ref));
    let bank_code: String = db.query_row(
        "SELECT bank_code FROM bank_connections WHERE id=?1",
        [id],
        |r| r.get(0),
    )?;
    let bank_name = data.bank_name.as_deref().unwrap_or("");
    for a in &data.accounts {
        if let Some(c) = choices.iter_mut().find(|c| c.account_ref == a.external_ref) {
            c.name = a.name.clone();
            c.number = a.account_number.clone();
            c.owner = a.owner.clone();
            continue;
        }
        let already: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM accounts WHERE connection_id=?1 AND external_ref=?2)",
            params![id, a.external_ref],
            |r| r.get(0),
        )?;
        let mut existing = String::new();
        // An imported account with this IBAN, parked on a PSD2 connection that
        // never synced, is not "another connection's account": the fetch takes
        // it over (`moneymoney_db::adopt_account` matches by the same IBAN), so
        // it is not held back for it. Without an IBAN nothing is taken over and
        // the account is held back as before.
        if !already {
            let mut q = db.prepare("SELECT a.iban,a.account_number,a.bank_name,c.bank_code,c.connector,c.display_name FROM accounts a JOIN bank_connections c ON c.id=a.connection_id WHERE a.connection_id<>?1 AND a.deleted_ms IS NULL AND a.archived_unix IS NULL AND a.access_revoked=0 AND c.deleted_ms IS NULL AND a.currency=?2 AND a.kind=?3 AND NOT (a.external_ref LIKE 'moneymoney-db:%' AND c.connector LIKE 'enablebanking%' AND c.last_sync_unix IS NULL AND ?4 <> '' AND upper(replace(a.iban, ' ', '')) = ?4) ORDER BY c.id,a.id")?;
            let candidates = q.query_map(
                params![
                    id,
                    a.currency,
                    a.kind.as_str(),
                    crate::iban::normalize_iban(&a.iban)
                ],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                    ))
                },
            )?;
            for row in candidates {
                let (iban, number, bank, code, connector, name) = row?;
                let same_iban =
                    !normalized(&a.iban).is_empty() && normalized(&a.iban) == normalized(&iban);
                let same_bank = if !bank_code.is_empty() && !code.is_empty() {
                    bank_code == code
                } else {
                    connector == kind.as_str()
                        && !matches!(
                            kind,
                            ConnectorKind::EnableBanking | ConnectorKind::FinTsGeneric
                        )
                        && !bank_name.is_empty()
                        && bank == bank_name
                };
                if same_iban
                    || (same_bank
                        && !normalized(&a.account_number).is_empty()
                        && normalized(&a.account_number) == normalized(&number))
                {
                    existing = name;
                    break;
                }
            }
        }
        let pending = !existing.is_empty() || (data.confirm_new_accounts && !already);
        choices.push(ConnectionAccountChoice {
            account_ref: a.external_ref.clone(),
            name: a.name.clone(),
            number: a.account_number.clone(),
            owner: a.owner.clone(),
            enabled: !pending,
            pending,
            existing_connection: existing,
        });
    }
    save(db, id, &choices)?;
    let blocked = choices
        .iter()
        .filter(|c| !c.enabled || c.pending)
        .map(|c| c.account_ref.clone())
        .collect();
    let pending = choices
        .iter()
        .filter(|c| c.pending)
        .map(|c| c.account_ref.clone())
        .collect();
    Ok((blocked, pending))
}

/// External refs of a connection whose fetched data is not stored: switched
/// off or pending in the account selection, archived or deleted accounts.
pub(super) fn skipped_refs(db: &Connection, connection_id: i64) -> CoreResult<Vec<String>> {
    let mut refs: Vec<String> = load(db, connection_id)?
        .into_iter()
        .filter(|c| !c.enabled || c.pending)
        .map(|c| c.account_ref)
        .collect();
    let mut st = db.prepare(
        "SELECT external_ref FROM accounts
          WHERE connection_id = ?1 AND (archived_unix IS NOT NULL OR deleted_ms IS NOT NULL)",
    )?;
    for r in st.query_map([connection_id], |r| r.get::<_, String>(0))? {
        let r = r?;
        if !refs.contains(&r) {
            refs.push(r);
        }
    }
    refs.sort();
    Ok(refs)
}

/// Retain only pending accounts for an immediate replay after the user chooses.
pub(super) fn deferred(data: &FetchResult, pending: &HashSet<String>) -> FetchResult {
    let mut copy = data.clone();
    copy.retain_accounts(|r| pending.contains(r));
    copy.accounts_complete = false;
    copy.state = None;
    copy.revoked_refs.clear();
    copy.renamed_refs.clear();
    copy.name_updates.clear();
    copy
}

#[uniffi::export]
impl Vault {
    pub fn connection_account_choices(
        &self,
        connection_id: i64,
    ) -> CoreResult<Vec<ConnectionAccountChoice>> {
        let db = self.db()?;
        // Backfill existing connections, so settings work before their next sync.
        let mut rows = load(&db, connection_id)?;
        let mut q=db.prepare("SELECT external_ref,name,account_number,owner FROM accounts WHERE connection_id=?1 AND deleted_ms IS NULL ORDER BY name,id")?;
        for a in q.query_map([connection_id], |r| {
            Ok(ConnectionAccountChoice {
                account_ref: r.get(0)?,
                name: r.get(1)?,
                number: r.get(2)?,
                owner: r.get(3)?,
                enabled: true,
                pending: false,
                existing_connection: String::new(),
            })
        })? {
            let a = a?;
            if !rows.iter().any(|c| c.account_ref == a.account_ref) {
                rows.push(a);
            }
        }
        // An archived account is not fetched, so the list shows it as switched off.
        let mut archived = db.prepare(
            "SELECT external_ref FROM accounts WHERE connection_id=?1 AND deleted_ms IS NULL AND archived_unix IS NOT NULL",
        )?;
        let archived: HashSet<String> = archived
            .query_map([connection_id], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        for c in &mut rows {
            if archived.contains(&c.account_ref) {
                c.enabled = false;
            }
        }
        Ok(rows)
    }
    pub fn set_connection_account_choices(
        self: Arc<Self>,
        connection_id: i64,
        enabled_refs: Vec<String>,
    ) -> CoreResult<()> {
        let mut rows = self.connection_account_choices(connection_id)?;
        if enabled_refs
            .iter()
            .any(|r| !rows.iter().any(|c| &c.account_ref == r))
        {
            return Err(CoreError::invalid("Unknown account selection"));
        }
        for c in &mut rows {
            c.enabled = enabled_refs.contains(&c.account_ref);
            c.pending = false;
        }
        // Switching an account off archives it (dimmed, not fetched, not in the total, rules skip
        // it; the sidebar can hide archived accounts); switching it on reactivates it. All data
        // stays. Accounts that are not stored yet (held back for a choice) have no row to archive.
        let changes: Vec<(i64, bool)> = {
            let db = self.db()?;
            save(&db, connection_id, &rows)?;
            let mut q = db.prepare(
                "SELECT id, external_ref, archived_unix IS NOT NULL FROM accounts WHERE connection_id=?1 AND deleted_ms IS NULL",
            )?;
            let stored = q
                .query_map([connection_id], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, bool>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            stored
                .into_iter()
                .filter_map(|(id, account_ref, archived)| {
                    let wanted = rows
                        .iter()
                        .find(|c| c.account_ref == account_ref)
                        .map(|c| !c.enabled)?;
                    (wanted != archived).then_some((id, wanted))
                })
                .collect()
        };
        for (id, archive) in changes {
            self.set_account_archived(id, archive)?;
        }
        // Never hold the cache lock while importing (store_fetch also uses it).
        let deferred = self
            .pending_account_fetches
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&connection_id);
        if let Some((kind, data)) = deferred {
            super::store_fetch(&self, kind, connection_id, data)?;
        }
        self.checkpoint()?;
        Ok(())
    }
    /// Demo only (`--demo-sheet account-selection`, screenshots): a second login of the
    /// demo bank that also reaches the shared joint account. Returns its connection id.
    pub fn seed_demo_account_selection(&self) -> CoreResult<i64> {
        let db = self.db()?;
        let (kind, code, symbol, first_name): (String, String, String, String) = db.query_row(
            "SELECT connector, bank_code, sf_symbol, display_name FROM bank_connections
             WHERE deleted_ms IS NULL ORDER BY id LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        db.execute(
            "INSERT INTO bank_connections(connector, display_name, login_name, bank_code, sf_symbol, last_sync_unix)
             VALUES (?1, ?2, 'demo-partner', ?3, ?4, strftime('%s','now') - 600)",
            params![kind, format!("{first_name} (Partner)"), code, symbol],
        )?;
        let id = db.last_insert_rowid();
        let row = |r: &str, name: &str, number: &str, owner: &str, pending: bool| {
            ConnectionAccountChoice {
                account_ref: r.into(),
                name: name.into(),
                number: number.into(),
                owner: owner.into(),
                enabled: !pending,
                pending,
                existing_connection: if pending {
                    first_name.clone()
                } else {
                    String::new()
                },
            }
        };
        let rows = vec![
            row(
                "P1",
                "Girokonto Partner",
                "1002003004",
                "Anna Muster",
                false,
            ),
            row(
                "P2",
                "Tagesgeld Partner",
                "1002003005",
                "Anna Muster",
                false,
            ),
            row(
                "J1",
                "Gemeinschaftskonto",
                "1002003006",
                "Anna Muster, Max Muster",
                true,
            ),
            row("K1", "Kinderdepot", "99135505031", "Mia Muster", true),
        ];
        save(&db, id, &rows)?;
        Ok(id)
    }
    pub fn pending_account_selection_connections(&self) -> CoreResult<Vec<i64>> {
        let db = self.db()?;
        let mut q =
            db.prepare("SELECT id FROM bank_connections WHERE deleted_ms IS NULL ORDER BY id")?;
        let ids = q
            .query_map([], |r| r.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        ids.into_iter()
            .filter_map(|id| match load(&db, id) {
                Ok(c) if c.iter().any(|c| c.pending) => Some(Ok(id)),
                Ok(_) => None,
                Err(e) => Some(Err(e)),
            })
            .collect()
    }
}
