//! Connection and account merge by sync key, with joined life (creation vs. deletion).

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension, params};

use super::{key_of_connection, twins};
use crate::error::{CoreError, CoreResult};
use crate::secret::SecretJson;

/// Joins the life of a connection or account: the newest `created_ms` and
/// the newest deletion (`extra`: the deletion of its connection) count, and
/// it is deleted when that deletion is not older than the creation (a row
/// added again after a deletion stays live). The same in any merge order: a
/// deletion dropped here is older than `created_ms`, which only grows.
fn join_life(
    local: (i64, Option<i64>),
    other: (i64, Option<i64>),
    extra: Option<i64>,
) -> (i64, Option<i64>) {
    let created = local.0.max(other.0);
    let deleted = local.1.max(other.1).max(extra);
    (created, deleted.filter(|&d| d >= created))
}

/// Applies an account's changed deletion to its bookings, as
/// `soft_delete_account` / `revive_account_if_deleted` do (derived, no
/// stamps): its cascade tombstones (`deleted_ms` = the old deletion) and live
/// bookings take the new deletion; on a revive the cascade tombstones come
/// back. Individually deleted bookings keep their own stamp.
fn recascade_account(
    conn: &Connection,
    id: i64,
    old: Option<i64>,
    new: Option<i64>,
) -> CoreResult<()> {
    if old == new {
        return Ok(());
    }
    if let Some(new) = new {
        conn.execute(
            "DELETE FROM loan_links WHERE loan_account_id = ?1
                OR transaction_id IN (SELECT id FROM transactions WHERE account_id = ?1
                                      AND (deleted_ms IS NULL OR deleted_ms IS ?2))",
            params![id, old],
        )?;
        conn.execute(
            "UPDATE transactions SET deleted_ms = ?3
              WHERE account_id = ?1 AND (deleted_ms IS NULL OR deleted_ms IS ?2)",
            params![id, old, new],
        )?;
    } else {
        conn.execute(
            "UPDATE transactions SET deleted_ms = NULL WHERE account_id = ?1 AND deleted_ms = ?2",
            params![id, old],
        )?;
    }
    Ok(())
}

/// Merges `other.bank_connections` by sync key: missing ones are inserted
/// with their `created_ms` / `deleted_ms`, existing ones take the joined
/// life (`join_life`). Returns other id -> local id.
pub(crate) fn merge_connections(conn: &Connection) -> CoreResult<HashMap<i64, i64>> {
    // local key -> id; a live row wins over a deleted one, then the lowest id
    // (rows read in reverse order of preference, the last one stays)
    let mut by_key: HashMap<String, (i64, i64, Option<i64>)> = HashMap::new();
    {
        // includes soft-deleted rows
        let mut st = conn.prepare(
            "SELECT id, connector, login_name, bank_code, created_ms, deleted_ms
               FROM main.bank_connections ORDER BY deleted_ms IS NULL, id DESC",
        )?;
        let mut rows = st.query([])?;
        while let Some(r) = rows.next()? {
            let key = key_of_connection(
                &r.get::<_, String>(1)?,
                &r.get::<_, String>(2)?,
                &r.get::<_, String>(3)?,
            );
            by_key.insert(key, (r.get(0)?, r.get(4)?, r.get(5)?));
        }
    }
    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        i64,
        String,
        String,
        String,
        String,
        String,
        SecretJson,
        Option<i64>,
        i64,
        Option<i64>,
    )> = conn
        .prepare(
            // includes soft-deleted rows: deleted connections travel as tombstones
            "SELECT id, connector, display_name, login_name, bank_code, sf_symbol, state_json,
                    last_sync_unix, created_ms, deleted_ms
               FROM other.bank_connections ORDER BY id",
        )?
        .query_map([], |r| {
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
                r.get(9)?,
            ))
        })?
        .collect::<Result<_, _>>()?;
    let mut map = HashMap::new();
    for (oid, connector, name, login, bank, sym, state, last, created, deleted) in rows {
        let key = key_of_connection(&connector, &login, &bank);
        let id = match by_key.get(&key).copied() {
            Some((id, lc, ld)) => {
                let joined = join_life((lc, ld), (created, deleted), None);
                if joined != (lc, ld) {
                    conn.execute(
                        "UPDATE bank_connections SET created_ms = ?2, deleted_ms = ?3 WHERE id = ?1",
                        params![id, joined.0, joined.1],
                    )?;
                    by_key.insert(key, (id, joined.0, joined.1));
                }
                if joined.1.is_some() {
                    // a deleted connection keeps no bank session and no stored secrets
                    conn.execute(
                        "UPDATE bank_connections SET state_json = '{}' WHERE id = ?1 AND state_json <> '{}'",
                        [id],
                    )?;
                    crate::vault::Vault::purge_connection_secrets(conn, id)?;
                }
                id
            }
            None => {
                conn.execute(
                    "INSERT INTO bank_connections(connector, display_name, login_name, bank_code, sf_symbol,
                         state_json, last_sync_unix, created_ms, deleted_ms)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                    params![
                        connector,
                        name,
                        login,
                        bank,
                        sym,
                        // a deleted connection keeps no bank session
                        if deleted.is_some() { SecretJson::from_text("{}".into()) } else { state },
                        last,
                        created,
                        deleted
                    ],
                )?;
                let id = conn.last_insert_rowid();
                by_key.insert(key, (id, created, deleted));
                id
            }
        };
        map.insert(oid, id);
    }
    Ok(map)
}

/// Merges `other.accounts` by connection + external ref: missing ones are
/// inserted with their `created_ms` / `deleted_ms`, existing ones take the
/// joined life (`join_life`), and an account of a deleted connection goes with
/// it unless it was added after that deletion. A changed deletion is applied
/// to the account's bookings here (`recascade_account`). Returns other id ->
/// local id and the ids of the accounts inserted (`accounts_added` counts the
/// live ones).
/// The other device's account `other_id` under another reference on the
/// same login: that device fetches it another way (comdirect via FinTS
/// there, via its API here, #537). Same IBAN and currency – else same
/// account number – and kind. Each side keeps its own reference, which
/// its next fetch looks for.
fn same_account_on_login(
    conn: &Connection,
    connection_id: i64,
    other_id: i64,
) -> CoreResult<Option<i64>> {
    #[allow(clippy::type_complexity)]
    let (iban, number, currency, kind, ext, deleted): (
        String,
        String,
        String,
        String,
        String,
        Option<i64>,
    ) = conn.query_row(
        "SELECT upper(replace(IFNULL(iban, ''), ' ', '')), IFNULL(account_number, ''),
                    upper(IFNULL(currency, '')), kind, external_ref, deleted_ms
               FROM other.accounts WHERE id = ?1",
        [other_id],
        |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        },
    )?;
    // live bank accounts only: tombstones and imported accounts follow the
    // twin rules (`twins`), a tombstone must never delete a live account here
    if deleted.is_some()
        || ext.starts_with("moneymoney-db:")
        || (iban.is_empty() && number.trim().is_empty())
    {
        return Ok(None);
    }
    Ok(conn
        .query_row(
            "SELECT id FROM main.accounts
              WHERE connection_id = ?1 AND deleted_ms IS NULL AND kind = ?4
                AND external_ref NOT LIKE 'moneymoney-db:%'
                AND upper(IFNULL(currency, '')) = ?5
                AND CASE WHEN ?2 <> '' THEN upper(replace(IFNULL(iban, ''), ' ', '')) = ?2
                         ELSE trim(IFNULL(account_number, '')) = trim(?3) END
              ORDER BY id LIMIT 1",
            params![connection_id, iban, number, kind, currency],
            |r| r.get(0),
        )
        .optional()?)
}

pub(crate) fn merge_accounts(
    conn: &Connection,
    conn_map: &HashMap<i64, i64>,
    group_map: &HashMap<i64, i64>,
    stats: &mut crate::vault::MergeStats,
) -> CoreResult<(HashMap<i64, i64>, HashSet<i64>)> {
    // local id -> (created, deleted) before this merge
    let before: HashMap<i64, (i64, Option<i64>)> = conn
        // includes soft-deleted rows
        .prepare("SELECT id, created_ms, deleted_ms FROM main.accounts")?
        .query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?
        .collect::<Result<_, _>>()?;
    let mut now = before.clone();
    // what the table holds
    let mut stored = before.clone();

    let mut acc_map: HashMap<i64, i64> = HashMap::new();
    let mut inserted: HashSet<i64> = HashSet::new();
    {
        let mut st = conn.prepare(
            // includes soft-deleted rows: deleted accounts travel as tombstones.
            // Bank accounts first: one may take over a local MoneyMoney account
            // (twins) before a tombstone for that MoneyMoney key reaches it.
            "SELECT id, connection_id, group_id, external_ref, created_ms, deleted_ms
               FROM other.accounts ORDER BY external_ref LIKE 'moneymoney-db:%', id",
        )?;
        #[allow(clippy::type_complexity)]
        let rows: Vec<(i64, Option<i64>, i64, String, i64, Option<i64>)> = st
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })?
            .collect::<Result<_, _>>()?;
        for (oid, ocon, ogrp, ext, created, deleted) in rows {
            let con = ocon.and_then(|c| conn_map.get(&c).copied());
            let existing: Option<i64> = conn
                .query_row(
                    // includes soft-deleted rows; a reference marked by
                    // „Abrufweg umstellen“ on one side is the same account (#537)
                    "SELECT id FROM accounts WHERE connection_id IS ?1
                       AND (external_ref = ?2 OR external_ref = ?3 || ?2 OR ?3 || external_ref = ?2)
                     ORDER BY external_ref = ?2 DESC, id LIMIT 1",
                    params![con, ext, crate::vault::connections::SWITCH_MARK],
                    |r| r.get(0),
                )
                .optional()?;
            let existing = match (existing, con) {
                (None, Some(c)) => match twins::adopt_on_merge(conn, c, oid)? {
                    Some(id) => Some(id),
                    None => same_account_on_login(conn, c, oid)?,
                },
                (found, _) => found,
            };
            let id = match existing {
                Some(id) => {
                    let joined = join_life(now[&id], (created, deleted), None);
                    now.insert(id, joined);
                    id
                }
                None => {
                    let grp = group_map
                        .get(&ogrp)
                        .copied()
                        .ok_or_else(|| CoreError::Database {
                            message: "merge: unknown account group".into(),
                        })?;
                    conn.execute(
                        "INSERT INTO accounts(connection_id, group_id, external_ref, name, bank_name, owner,
                             iban, account_number, currency, kind, balance_cents, balance_date, sf_symbol,
                             sort_order, initial_balance_cents, balance_derived, include_in_total, total_ms,
                             created_ms, deleted_ms)
                         SELECT ?2, ?3, external_ref, name, bank_name, owner, iban, account_number, currency,
                             kind, balance_cents, balance_date, sf_symbol, sort_order, initial_balance_cents,
                             balance_derived, include_in_total, total_ms, created_ms, deleted_ms
                           FROM other.accounts WHERE id = ?1",
                        params![oid, con, grp],
                    )?;
                    let id = conn.last_insert_rowid();
                    if deleted.is_none() {
                        stats.accounts_added += 1;
                    }
                    inserted.insert(id);
                    now.insert(id, (created, deleted));
                    stored.insert(id, (created, deleted));
                    id
                }
            };
            acc_map.insert(oid, id);
        }
    }

    // an account goes with its deleted connection (unless added after it)
    let con_deleted: HashMap<i64, Option<i64>> = conn
        // includes soft-deleted rows
        .prepare("SELECT a.id, c.deleted_ms FROM accounts a JOIN bank_connections c ON c.id = a.connection_id")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    for (id, life) in now.iter_mut() {
        if let Some(&d) = con_deleted.get(id) {
            *life = join_life(*life, *life, d);
        }
    }

    let mut ids: Vec<i64> = now.keys().copied().collect();
    ids.sort();
    for id in ids {
        let life = now[&id];
        if stored[&id] != life {
            conn.execute(
                "UPDATE accounts SET created_ms = ?2, deleted_ms = ?3 WHERE id = ?1",
                params![id, life.0, life.1],
            )?;
        }
        // inserted accounts have no local bookings yet
        if let Some(&(_, old)) = before.get(&id) {
            recascade_account(conn, id, old, life.1)?;
        }
    }
    Ok((acc_map, inserted))
}
