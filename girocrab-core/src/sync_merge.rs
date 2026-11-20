//! Merging two device vaults: newest edit wins, deletions stay deleted.
//!
//! This module holds the groundwork shared by the merge steps: the v8 schema
//! (per-field edit stamps, soft-delete columns, uuids), the sync keys that
//! identify a row on every device, and deterministic uuids so that two vaults
//! which never met still agree on what "Lebensmittel > Supermarkt" is. It
//! also holds the soft-delete and revive helpers every write path uses, so
//! that a deletion reaches the other devices.
//!
//! Sync keys:
//! - category, rule: `uuid`
//! - connection: `connector|normalized login|bank_code`
//! - account: `<connection key>|external_ref` (`|external_ref` without connection)
//! - booking: `<account key>|external_id`
//!
//! Stamps are milliseconds from [`crate::dates::now_ms`]; `deleted_ms`
//! NULL means live. A merge never stamps anything itself.
//!
//! The merge steps live in one submodule per entity and run in this order
//! (`vault::merge`): `accounts` (connections, then accounts), `categories`,
//! `rules`, `bookings`. Each step translates the other vault's ids with the
//! maps the earlier steps returned.

use rusqlite::{Connection, OptionalExtension, params};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use crate::dates::now_ms;
use crate::error::{CoreError, CoreResult};
use crate::models::ConnectorKind;
use crate::schema::{has_column, has_table};

mod accounts;
mod bookings;
mod categories;
mod rules;
pub(crate) mod twins;

pub(crate) use accounts::{merge_accounts, merge_connections};
pub(crate) use bookings::merge_bookings;
pub(crate) use categories::merge_categories;
pub(crate) use rules::merge_rules;

/// Fixed namespace for the deterministic (v5) uuids.
const NAMESPACE: Uuid = Uuid::from_bytes([
    0x67, 0x69, 0x72, 0x6f, 0x63, 0x72, 0x61, 0x62, 0x73, 0x79, 0x6e, 0x63, 0x6d, 0x72, 0x67, 0x31,
]);

const SEP: char = '\u{1f}';

/// (table, column, definition)
const COLUMNS: &[(&str, &str, &str)] = &[
    ("transactions", "category_ms", "INTEGER NOT NULL DEFAULT 0"),
    ("transactions", "note_ms", "INTEGER NOT NULL DEFAULT 0"),
    ("transactions", "checked_ms", "INTEGER NOT NULL DEFAULT 0"),
    ("transactions", "text_ms", "INTEGER NOT NULL DEFAULT 0"),
    ("transactions", "splits_ms", "INTEGER NOT NULL DEFAULT 0"),
    ("transactions", "deleted_ms", "INTEGER"),
    ("categories", "uuid", "TEXT"),
    ("categories", "name_ms", "INTEGER NOT NULL DEFAULT 0"),
    ("categories", "parent_ms", "INTEGER NOT NULL DEFAULT 0"),
    ("categories", "symbol_ms", "INTEGER NOT NULL DEFAULT 0"),
    ("categories", "color_ms", "INTEGER NOT NULL DEFAULT 0"),
    ("categories", "budget_ms", "INTEGER NOT NULL DEFAULT 0"),
    ("categories", "order_ms", "INTEGER NOT NULL DEFAULT 0"),
    ("categories", "deleted_ms", "INTEGER"),
    ("categories", "merged_into", "TEXT"),
    ("rules", "uuid", "TEXT"),
    ("rules", "updated_ms", "INTEGER NOT NULL DEFAULT 0"),
    ("rules", "deleted_ms", "INTEGER"),
    ("rules", "merged_into", "TEXT"),
    (
        "bank_connections",
        "created_ms",
        "INTEGER NOT NULL DEFAULT 0",
    ),
    ("bank_connections", "deleted_ms", "INTEGER"),
    ("accounts", "created_ms", "INTEGER NOT NULL DEFAULT 0"),
    ("accounts", "deleted_ms", "INTEGER"),
];

/// Schema v8 – idempotent, also for vaults that skipped versions.
/// Repair step up to schema 9 (`schema::repair_to_v8`); new changes go in `schema.rs`.
pub(crate) fn migrate(conn: &Connection) -> CoreResult<()> {
    for (table, col, def) in COLUMNS {
        if !has_column(conn, "main", table, col)? {
            conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {col} {def};"))?;
        }
    }
    // a deleted category's name can be used again
    let partial: bool = conn
        .query_row(
            "SELECT sql LIKE '%deleted_ms%' FROM sqlite_master
             WHERE type = 'index' AND name = 'categories_unique_name'",
            [],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(false);
    if !partial {
        conn.execute_batch(
            "DROP INDEX IF EXISTS categories_unique_name;
             CREATE UNIQUE INDEX categories_unique_name
                 ON categories(IFNULL(parent_id, 0), name) WHERE deleted_ms IS NULL;",
        )?;
    }
    conn.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS categories_uuid ON categories(uuid);
         CREATE UNIQUE INDEX IF NOT EXISTS rules_uuid ON rules(uuid);",
    )?;
    // Every count over bookings filters `deleted_ms IS NULL` now. Without the
    // column in the index SQLite reads (and decrypts) each matching row: rule
    // match counts and the per-account counts took ~40–85 ms each on 30k bookings.
    for (name, cols) in COVERING_INDEXES {
        let covers: i64 = conn.query_row(
            "SELECT count(*) FROM pragma_index_info(?1) WHERE name = 'deleted_ms'",
            [name],
            |r| r.get(0),
        )?;
        if covers == 0 {
            conn.execute_batch(&format!(
                "DROP INDEX IF EXISTS {name}; CREATE INDEX {name} ON transactions({cols});"
            ))?;
        }
    }
    fill_missing_uuids(conn)
}

/// Booking indexes widened by `deleted_ms` (vaults from before schema v8 have them without).
/// `tx_rule` also serves the rule merge, which clears rule_id per deleted rule.
const COVERING_INDEXES: &[(&str, &str)] = &[
    ("tx_rule", "rule_id, deleted_ms"),
    ("tx_account_category", "account_id, category_id, deleted_ms"),
    ("tx_account_checked", "account_id, checked, deleted_ms"),
];

fn fold(name: &str) -> String {
    name.trim().to_lowercase()
}

/// Same path (case-folded, trimmed names from the root) -> same uuid.
pub(crate) fn category_uuid_for_path(path: &[&str]) -> String {
    let joined = path
        .iter()
        .map(|p| fold(p))
        .collect::<Vec<_>>()
        .join(&SEP.to_string());
    Uuid::new_v5(&NAMESPACE, format!("category{SEP}{joined}").as_bytes()).to_string()
}

/// Same tag name (case-folded, whitespace collapsed) -> same uuid (#228).
pub(crate) fn tag_uuid_for_name(name: &str) -> String {
    let key = crate::tags::fold(name);
    Uuid::new_v5(&NAMESPACE, format!("tag{SEP}{key}").as_bytes()).to_string()
}

/// Same content -> same uuid. `account_key` is the sync key of the account a
/// rule is restricted to.
#[allow(clippy::too_many_arguments)]
pub(crate) fn rule_uuid_for_content(
    field: &str,
    match_kind: &str,
    pattern: &str,
    case_sensitive: bool,
    min: Option<i64>,
    max: Option<i64>,
    account_key: Option<&str>,
    category_uuid: &str,
) -> String {
    let opt = |v: Option<i64>| v.map(|v| v.to_string()).unwrap_or_default();
    let parts = [
        "rule".to_string(),
        field.to_string(),
        match_kind.to_string(),
        pattern.to_string(),
        (case_sensitive as i32).to_string(),
        opt(min),
        opt(max),
        account_key.unwrap_or_default().to_string(),
        category_uuid.to_string(),
    ];
    Uuid::new_v5(&NAMESPACE, parts.join(&SEP.to_string()).as_bytes()).to_string()
}

/// The category part of a rule's content identity: the category uuid, plus the
/// rule's tags (#580) when it has any, so rules without tags keep the identity
/// they had before tags existed (older devices compute the same uuid).
fn rule_target_key(category_uuid: &str, tag_uuids: &str) -> String {
    if tag_uuids.is_empty() {
        category_uuid.to_string()
    } else {
        format!("{category_uuid}{SEP}tags{SEP}{tag_uuids}")
    }
}

/// Whether `schema.rules` has `tag_uuids` (a vault from before step 28 has not).
fn rules_have_tags(conn: &Connection, schema: &str) -> CoreResult<bool> {
    Ok(conn.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM pragma_table_info('rules', '{schema}') WHERE name = 'tag_uuids')"),
        [],
        |r| r.get(0),
    )?)
}

type RuleRow = (
    i64,
    String,
    String,
    String,
    bool,
    Option<i64>,
    Option<i64>,
    rusqlite::types::Value,
    Option<i64>,
    String,
);

/// Gives every category and rule without a uuid one: the deterministic uuid if
/// no row (live or deleted) has it yet, else a random one.
pub(crate) fn fill_missing_uuids(conn: &Connection) -> CoreResult<()> {
    // includes soft-deleted rows
    let missing: i64 = conn.query_row(
        "SELECT (SELECT count(*) FROM categories WHERE uuid IS NULL)
              + (SELECT count(*) FROM rules WHERE uuid IS NULL)",
        [],
        |r| r.get(0),
    )?;
    if missing == 0 {
        return Ok(());
    }
    // categories
    let rows: Vec<(i64, Option<i64>, String, Option<String>)> = conn
        .prepare("SELECT id, parent_id, name, uuid FROM categories ORDER BY id")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<Result<_, _>>()?;
    let by_id: HashMap<i64, (Option<i64>, &str)> = rows
        .iter()
        .map(|(id, p, n, _)| (*id, (*p, n.as_str())))
        .collect();
    let mut used: HashSet<String> = rows.iter().filter_map(|r| r.3.clone()).collect();
    for (id, _, _, uuid) in &rows {
        if uuid.is_some() {
            continue;
        }
        let mut path = Vec::new();
        let mut cur = Some(*id);
        while let Some(c) = cur {
            let Some((p, n)) = by_id.get(&c) else { break };
            path.push(*n);
            if path.len() > 64 {
                break;
            }
            cur = *p;
        }
        path.reverse();
        let mut u = category_uuid_for_path(&path);
        if used.contains(&u) {
            u = Uuid::new_v4().to_string();
        }
        used.insert(u.clone());
        conn.execute(
            "UPDATE categories SET uuid = ?1 WHERE id = ?2",
            params![u, id],
        )?;
    }
    // rules
    // runs during migrations too: before step 24 the column is the account set
    let has_set = has_table(conn, "main", "rule_accounts")?;
    let accounts = if has_set {
        "COALESCE((SELECT group_concat(account_id) FROM rule_accounts
                   WHERE rule_id = rules.id), account_id)"
    } else {
        "account_id"
    };
    // and before step 28 there are no rule tags
    let tags = if rules_have_tags(conn, "main")? {
        "tag_uuids"
    } else {
        "''"
    };
    let rules: Vec<RuleRow> = conn
        .prepare(&format!(
            "SELECT id, field, match_kind, pattern, case_sensitive, min_amount_cents,
                    max_amount_cents, {accounts}, category_id, {tags}
             FROM rules WHERE uuid IS NULL ORDER BY id"
        ))?
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
    let mut used: HashSet<String> = conn
        .prepare("SELECT uuid FROM rules WHERE uuid IS NOT NULL")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    for (id, field, kind, pattern, cs, min, max, account, category, tag_uuids) in rules {
        let cat_uuid: String = match category {
            Some(category) => conn
                .query_row(
                    "SELECT uuid FROM categories WHERE id = ?1",
                    [category],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or_default(),
            None => String::new(),
        };
        let account_key = accounts_key(conn, &crate::rules::parse_account_ids(&account))?;
        let mut u = rule_uuid_for_content(
            &field,
            &kind,
            &pattern,
            cs,
            min,
            max,
            account_key.as_deref(),
            &rule_target_key(&cat_uuid, &tag_uuids),
        );
        if used.contains(&u) {
            u = Uuid::new_v4().to_string();
        }
        used.insert(u.clone());
        conn.execute("UPDATE rules SET uuid = ?1 WHERE id = ?2", params![u, id])?;
    }
    Ok(())
}

fn not_found(what: &str, id: i64) -> CoreError {
    CoreError::NotFound {
        what: format!("{what} {id}"),
    }
}

/// `connector|normalized login|bank_code`
/// Soft-deletes a category with all its descendants and their rules, and
/// uncategorises their bookings and splits (what the foreign-key cascades did
/// before deletions became soft). The uncategorising is a derived change: no
/// `category_ms` stamp.
pub(crate) fn soft_delete_categories(conn: &Connection, root: i64) -> CoreResult<()> {
    let now = now_ms();
    conn.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS doomed_categories(id INTEGER PRIMARY KEY);
         DELETE FROM doomed_categories;",
    )?;
    conn.execute(
        "WITH RECURSIVE sub(id) AS (
             SELECT id FROM categories WHERE id = ?1 AND deleted_ms IS NULL
             UNION SELECT c.id FROM categories c JOIN sub ON c.parent_id = sub.id WHERE c.deleted_ms IS NULL)
         INSERT INTO doomed_categories SELECT id FROM sub",
        [root],
    )?;
    let r = (|| -> CoreResult<()> {
        conn.execute(
            "UPDATE transactions SET category_id = NULL, rule_id = NULL, category_source = 'none'
             WHERE category_id IN (SELECT id FROM doomed_categories)
                OR rule_id IN (SELECT id FROM rules WHERE category_id IN (SELECT id FROM doomed_categories))",
            [],
        )?;
        conn.execute(
            "UPDATE transaction_splits SET category_id = NULL
             WHERE category_id IN (SELECT id FROM doomed_categories)",
            [],
        )?;
        conn.execute(
            "UPDATE rules SET deleted_ms = ?1
             WHERE deleted_ms IS NULL AND category_id IN (SELECT id FROM doomed_categories)",
            [now],
        )?;
        conn.execute(
            "UPDATE categories SET deleted_ms = ?1 WHERE id IN (SELECT id FROM doomed_categories)",
            [now],
        )?;
        Ok(())
    })();
    conn.execute("DELETE FROM doomed_categories", [])?;
    r
}

/// Soft-deletes one booking (bank-side removal, twin merge).
pub(crate) fn soft_delete_booking(conn: &Connection, id: i64) -> CoreResult<()> {
    conn.execute("DELETE FROM loan_links WHERE transaction_id = ?1", [id])?;
    conn.execute(
        "UPDATE transactions SET deleted_ms = ?1 WHERE id = ?2 AND deleted_ms IS NULL",
        params![now_ms(), id],
    )?;
    Ok(())
}

/// Soft-deletes an account and its bookings (what the FK cascade did).
pub(crate) fn soft_delete_account(conn: &Connection, id: i64) -> CoreResult<()> {
    soft_delete_account_at(conn, id, now_ms())
}

/// The cascade shares one stamp, so a revive can tell the bookings deleted
/// together with the account from individually deleted ones.
fn soft_delete_account_at(conn: &Connection, id: i64, now: i64) -> CoreResult<()> {
    // derived loan links went with the rows (FK cascade) before
    conn.execute(
        "DELETE FROM loan_links WHERE loan_account_id = ?1
            OR transaction_id IN (SELECT id FROM transactions WHERE account_id = ?1)",
        [id],
    )?;
    conn.execute(
        "UPDATE transactions SET deleted_ms = ?1 WHERE account_id = ?2 AND deleted_ms IS NULL",
        params![now, id],
    )?;
    conn.execute(
        "UPDATE accounts SET deleted_ms = ?1 WHERE id = ?2 AND deleted_ms IS NULL",
        params![now, id],
    )?;
    Ok(())
}

/// Soft-deletes a connection, its accounts and their bookings.
pub(crate) fn soft_delete_connection(conn: &Connection, id: i64) -> CoreResult<()> {
    let accounts: Vec<i64> = {
        // includes soft-deleted rows
        let mut q = conn.prepare("SELECT id FROM accounts WHERE connection_id = ?1")?;
        let rows = q.query_map([id], |r| r.get(0))?;
        rows.collect::<Result<_, _>>()?
    };
    let now = now_ms();
    for a in accounts {
        soft_delete_account_at(conn, a, now)?;
    }
    conn.execute(
        "UPDATE bank_connections SET deleted_ms = ?1 WHERE id = ?2 AND deleted_ms IS NULL",
        params![now, id],
    )?;
    // the bank session and tokens go with the connection
    conn.execute(
        "UPDATE bank_connections SET state_json = '{}' WHERE id = ?1 AND state_json <> '{}'",
        [id],
    )?;
    Ok(())
}

/// A deleted connection added again comes back as a new row in time.
pub(crate) fn revive_connection(conn: &Connection, id: i64) -> CoreResult<()> {
    conn.execute(
        "UPDATE bank_connections SET deleted_ms = NULL, created_ms = ?1 WHERE id = ?2",
        params![now_ms(), id],
    )?;
    Ok(())
}

/// Whether a fetch over `connection_id` must leave the soft-deleted account
/// `account_id` alone: it was deleted while the connection existed, so the
/// bank still delivering it changes nothing (on any device). Only a connection
/// added (again) after the deletion revives it (the same millisecond counts as
/// after: on one device the re-add can only follow the delete). False for a
/// live account.
pub(crate) fn fetch_keeps_account_deleted(
    conn: &Connection,
    connection_id: i64,
    account_id: i64,
) -> CoreResult<bool> {
    Ok(conn
        .query_row(
            // includes soft-deleted rows
            "SELECT a.deleted_ms IS NOT NULL AND IFNULL(c.created_ms, 0) < a.deleted_ms
               FROM accounts a LEFT JOIN bank_connections c ON c.id = ?1
              WHERE a.id = ?2",
            params![connection_id, account_id],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(false))
}

/// A deleted account the bank (or an import) delivers again comes back with a
/// new `created_ms`, together with the bookings deleted in the same cascade;
/// individually deleted bookings stay deleted. `true` if it was deleted.
pub(crate) fn revive_account_if_deleted(conn: &Connection, id: i64) -> CoreResult<bool> {
    let stamp: Option<i64> = conn
        // includes soft-deleted rows
        .query_row("SELECT deleted_ms FROM accounts WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .optional()?
        .flatten();
    let Some(stamp) = stamp else {
        return Ok(false);
    };
    // unarchiving is stamped, so the merge carries it to the other devices
    // (otherwise they keep the revived account archived and out of totals)
    conn.execute(
        "UPDATE accounts SET deleted_ms = NULL, created_ms = ?1,
                archived_ms = CASE WHEN archived_unix IS NULL THEN archived_ms ELSE ?1 END,
                archived_unix = NULL
          WHERE id = ?2",
        params![now_ms(), id],
    )?;
    conn.execute(
        "UPDATE transactions SET deleted_ms = NULL WHERE account_id = ?1 AND deleted_ms = ?2",
        params![id, stamp],
    )?;
    Ok(true)
}

pub(crate) fn connection_key(conn: &Connection, id: i64) -> CoreResult<String> {
    let (connector, login, bank): (String, String, String) = conn
        .query_row(
            // includes soft-deleted rows
            "SELECT connector, login_name, bank_code FROM bank_connections WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
        .ok_or_else(|| not_found("connection", id))?;
    Ok(key_of_connection(&connector, &login, &bank))
}

fn key_of_connection(connector: &str, login: &str, bank: &str) -> String {
    // comdirect via its own API or via FinTS: „Abrufweg umstellen“ (#486)
    // moves a login between the two, so every device must see one login (#537)
    if is_comdirect_route(connector) {
        let login = crate::vault::connections::normalize_login(ConnectorKind::Comdirect, login);
        return format!("{}|{login}|", ConnectorKind::Comdirect.as_str());
    }
    let login = match ConnectorKind::from_str(connector) {
        Some(kind) => crate::vault::connections::normalize_login(kind, login),
        None => login.trim().to_string(),
    };
    format!("{connector}|{login}|{bank}")
}

fn is_comdirect_route(connector: &str) -> bool {
    connector == ConnectorKind::Comdirect.as_str()
        || ConnectorKind::fints_blz(connector)
            .is_some_and(|b| crate::connectors::fints_directory::COMDIRECT_BLZ.contains(&b))
}

/// An account reference without the mark „Abrufweg umstellen“ sets (#537).
fn unmarked(external_ref: &str) -> &str {
    external_ref
        .strip_prefix(crate::vault::connections::SWITCH_MARK)
        .unwrap_or(external_ref)
}

/// `<connection key>|external_ref`, or `|external_ref` without a connection.
pub(crate) fn account_key(conn: &Connection, id: i64) -> CoreResult<String> {
    let (connection, external_ref): (Option<i64>, String) = conn
        .query_row(
            // includes soft-deleted rows
            "SELECT connection_id, external_ref FROM accounts WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| not_found("account", id))?;
    let prefix = match connection {
        Some(c) => connection_key(conn, c)?,
        None => String::new(),
    };
    Ok(format!("{prefix}|{}", unmarked(&external_ref)))
}

/// Separator between the account keys of a rule restricted to several accounts.
const ACCOUNT_KEY_SEP: &str = "\u{1f}";

/// Account keys of a rule's accounts, sorted and joined; `None` = all accounts.
/// One account gives its plain key, so rule uuids of single-account rules stay.
pub(crate) fn accounts_key(conn: &Connection, ids: &[i64]) -> CoreResult<Option<String>> {
    let mut keys = ids
        .iter()
        .map(|a| account_key(conn, *a))
        .collect::<CoreResult<Vec<_>>>()?;
    keys.sort();
    Ok((!keys.is_empty()).then(|| keys.join(ACCOUNT_KEY_SEP)))
}

/// `<account key>|external_id`
#[cfg(test)]
fn booking_key(conn: &Connection, id: i64) -> CoreResult<String> {
    let (account, external_id): (i64, String) = conn
        .query_row(
            // includes soft-deleted rows
            "SELECT account_id, external_id FROM transactions WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| not_found("booking", id))?;
    Ok(format!("{}|{external_id}", account_key(conn, account)?))
}

// ------------------------------------------------------------------ merge

/// Newest stamp wins; equal stamps with different values: the larger value.
pub(crate) fn take_newer<T: PartialOrd>(val: &mut T, ms: &mut i64, other: T, other_ms: i64) {
    if other_ms > *ms || (other_ms == *ms && other > *val) {
        *val = other;
        *ms = other_ms;
    }
}

/// A deletion only ever gets set: the larger `deleted_ms` wins, and
/// `merged_into` goes with it (equal stamps: the larger pointer).
fn merge_deletion(
    deleted: &mut Option<i64>,
    merged: &mut Option<String>,
    other_deleted: Option<i64>,
    other_merged: Option<String>,
) {
    let mut ms = deleted.unwrap_or(0);
    take_newer(merged, &mut ms, other_merged, other_deleted.unwrap_or(0));
    *deleted = (*deleted).max(other_deleted);
}

/// Follows `merged_into` to the live row a deleted one was combined into.
/// `None`: deleted without a live survivor.
fn survivor<'a>(
    uuid: &'a str,
    live: impl Fn(&str) -> Option<(bool, Option<&'a str>)>,
) -> Option<&'a str> {
    let mut cur = uuid;
    for _ in 0..64 {
        let (is_live, merged) = live(cur)?;
        if is_live {
            return Some(cur);
        }
        cur = merged?;
    }
    None
}

/// Picks the local row a v7 row (no uuid of its own) stands for: live rows
/// first, then the lowest uuid. Soft-deleted rows count, so that nothing
/// deleted comes back with a new uuid.
fn pick_match<'a>(cands: &[(&'a str, bool)]) -> Option<&'a str> {
    cands
        .iter()
        .min_by_key(|(u, live)| (!live, *u))
        .map(|(u, _)| *u)
}

/// Keeps the entries of an other-id -> local-id map whose local row is live.
pub(crate) fn live_targets(
    conn: &Connection,
    table: &str,
    map: &HashMap<i64, i64>,
) -> CoreResult<HashMap<i64, i64>> {
    // includes soft-deleted rows
    let deleted: HashSet<i64> = conn
        .prepare(&format!(
            "SELECT id FROM {table} WHERE deleted_ms IS NOT NULL"
        ))?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(map
        .iter()
        .filter(|(_, l)| !deleted.contains(l))
        .map(|(o, l)| (*o, *l))
        .collect())
}

/// The synced state of a vault, one line per row, ordered by sync key and
/// including soft-deleted rows.
#[cfg(test)]
fn synced_state(v: &crate::vault::Vault) -> Vec<String> {
    use rusqlite::types::Value;
    let db = v.db().unwrap();
    // includes soft-deleted rows
    let rows = |sql: &str| -> Vec<Vec<Value>> {
        let mut st = db.prepare(sql).unwrap();
        let n = st.column_count();
        st.query_map([], |r| (0..n).map(|i| r.get::<_, Value>(i)).collect())
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    let show = |vals: &[Value]| format!("{vals:?}");
    // replaces the leading local id by the row's sync key
    let keyed = |sql: &str, key: &dyn Fn(i64) -> String| -> Vec<(String, String)> {
        rows(sql)
            .into_iter()
            .map(|r| {
                let Value::Integer(id) = r[0] else {
                    unreachable!()
                };
                let mut line = vec![Value::Text(key(id))];
                line.extend_from_slice(&r[1..]);
                (show(&line[..1]), show(&line))
            })
            .collect()
    };
    let mut out = Vec::new();
    let mut section = |name: &str, mut lines: Vec<(String, String)>| {
        lines.sort();
        out.extend(lines.into_iter().map(|(_, l)| format!("{name}: {l}")));
    };
    section(
        "category",
        rows(
            "SELECT c.uuid, c.name, p.uuid, c.sf_symbol, c.color_hex, c.budget_cents, c.sort_order,
                    c.name_ms, c.parent_ms, c.symbol_ms, c.color_ms, c.budget_ms, c.order_ms,
                    c.deleted_ms, c.merged_into
             FROM categories c LEFT JOIN categories p ON p.id = c.parent_id",
        )
        .iter()
        .map(|r| (show(&r[..1]), show(r)))
        .collect(),
    );
    section(
        "rule",
        rows(
            "SELECT r.uuid, r.name, c.uuid, r.field, r.match_kind, r.pattern, r.case_sensitive,
                    r.min_amount_cents, r.max_amount_cents,
                    COALESCE((SELECT group_concat(account_id) FROM rule_accounts
                              WHERE rule_id = r.id), r.account_id),
                    r.priority, r.enabled,
                    r.updated_ms, r.deleted_ms, r.merged_into, r.tag_uuids
             FROM rules r LEFT JOIN categories c ON c.id = r.category_id",
        )
        .into_iter()
        .map(|mut r| {
            let ids = crate::rules::parse_account_ids(&r[9]);
            if !ids.is_empty() {
                r[9] = Value::Text(accounts_key(&db, &ids).unwrap().unwrap());
            }
            (show(&r[..1]), show(&r))
        })
        .collect(),
    );
    section(
        "booking",
        keyed(
            "SELECT t.id, CASE WHEN t.category_source = 'manual' THEN c.uuid END, t.note,
                    t.checked, t.counterparty, t.purpose, t.category_ms, t.note_ms, t.checked_ms,
                    t.text_ms, t.splits_ms, t.deleted_ms
             FROM transactions t LEFT JOIN categories c ON c.id = t.category_id",
            &|id| booking_key(&db, id).unwrap(),
        ),
    );
    section(
        "split",
        keyed(
            "SELECT s.transaction_id, c.uuid, s.amount_cents, s.note
             FROM transaction_splits s LEFT JOIN categories c ON c.id = s.category_id",
            &|id| booking_key(&db, id).unwrap(),
        )
        .into_iter()
        .map(|(_, l)| (l.clone(), l))
        .collect(),
    );
    section(
        "account",
        keyed("SELECT id, created_ms, deleted_ms FROM accounts", &|id| {
            account_key(&db, id).unwrap()
        }),
    );
    section(
        "connection",
        keyed(
            "SELECT id, created_ms, deleted_ms FROM bank_connections",
            &|id| connection_key(&db, id).unwrap(),
        ),
    );
    out
}

/// Test helper: two vaults hold the same synced state.
#[cfg(test)]
pub(crate) fn assert_converged(a: &crate::vault::Vault, b: &crate::vault::Vault) {
    assert_eq!(synced_state(a), synced_state(b), "vaults did not converge");
}

#[cfg(test)]
mod tests;
