//! User-defined account groups ("Kontogruppen") for the "Nach Gruppe"
//! sidebar view – e.g. "Privat", "Geschäft", "Kinder".
//!
//! Independent of the kind-based `account_groups` (the "Nach Kontoart" view),
//! which stay untouched. An account belongs to at most one user group
//! (`accounts.user_group_uuid`); accounts without one are listed under
//! "Ohne Gruppe".
//!
//! iCloud sync: groups are keyed by a random uuid. Every user edit stamps a
//! millisecond timestamp per field (`user_groups.name_ms` / `symbol_ms` /
//! `total_ms` / `order_ms`, `accounts.placement_ms` / `name_ms` /
//! `color_ms` / `note_ms`); when merging two vaults the newer edit wins field by field, so
//! renaming a group on one device and switching its total flag on another
//! keeps both. Equal stamps with different values (rare) resolve to the larger
//! value, so both sides still end up identical. Deleted
//! groups stay as tombstones (`deleted_ms`): a merge never brings them back,
//! even if another device edited them later (delete wins).
//! Automatic placements use timestamp 0, so any explicit user choice wins.
//!
//! The name is a group's identity for the user, so live names are unique
//! (case-insensitive) across devices too: if two devices each created
//! "Privat", the merge combines them into the one with the lower uuid. A
//! combined group is a tombstone with `merged_into`; accounts placed in it –
//! also later, from a device that hasn't merged yet – follow that pointer.
//! The user can combine groups the same way (`merge_user_groups`).
//!
//! Groups can be nested (`parent_uuid`, stamped with `parent_ms` and merged
//! like the other fields). A group's sum, booking list and balance chart
//! include its subgroups. The stored parent is only what the user chose; the
//! tree shown is derived from it on every read ([`effective_parents`]), the
//! same way on every device: a parent that was deleted is skipped (the
//! subgroup moves up), one that was combined is followed to the surviving
//! group, and a loop – two devices nesting two groups into each other – is
//! cut at its lowest uuid. Nothing of this is written back, so devices
//! never race over it.
//!
//! Derived changes are not stamped: moving accounts out of a deleted or
//! combined group keeps their `placement_ms`, because every device derives
//! the same result from the same tombstones; stamping them with "now" would
//! make the devices race. Tombstones are kept forever on purpose: they are a
//! few tiny rows, and pruning one would let a device that was offline long
//! enough bring the group back.
//!
//! Older builds (schema v6) ignore all of this. They keep `user_version` at 7
//! and write accounts with explicit column lists, so groups, colours and
//! placements survive a round trip. A rename made by such a build doesn't
//! stamp `name_ms`, though, so a stamped rename from another device wins the
//! next merge even if it is older.
//!
//! Account colours (booking-list markers, sidebar dot) follow the same rule:
//! without a user choice the colour is derived from the account's sync
//! identity, so every device shows the same colour without storing it.

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension, params};

use crate::connectors::group_for;
use crate::dates::now_ms;
use crate::error::{CoreError, CoreResult};
use crate::fnv::Fnv1a64;
use crate::i18n::{tr, trf};
use crate::models::{Account, UserGroup};
use crate::schema::has_column;
use crate::vault::Vault;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS user_groups (
    uuid             TEXT    PRIMARY KEY,
    name             TEXT    NOT NULL,
    sf_symbol        TEXT    NOT NULL DEFAULT 'folder',
    sort_order       INTEGER NOT NULL DEFAULT 0,
    include_in_total INTEGER NOT NULL DEFAULT 1,
    updated_ms       INTEGER NOT NULL DEFAULT 0,     -- last edit of any field
    name_ms          INTEGER NOT NULL DEFAULT 0,
    symbol_ms        INTEGER NOT NULL DEFAULT 0,
    total_ms         INTEGER NOT NULL DEFAULT 0,
    order_ms         INTEGER NOT NULL DEFAULT 0,
    deleted_ms       INTEGER,                      -- tombstone, kept for sync
    merged_into      TEXT,                         -- tombstone of a combined group → survivor
    parent_uuid      TEXT,                         -- subgroup of this group (NULL = top level)
    parent_ms        INTEGER NOT NULL DEFAULT 0
);";

/// New `accounts` columns (name, definition).
const ACCOUNT_COLUMNS: &[(&str, &str)] = &[
    ("user_group_uuid", "TEXT"),
    ("user_sort_order", "INTEGER NOT NULL DEFAULT 0"),
    ("placement_ms", "INTEGER NOT NULL DEFAULT 0"),
    ("name_ms", "INTEGER NOT NULL DEFAULT 0"),
    ("color_hex", "TEXT"),
    ("color_ms", "INTEGER NOT NULL DEFAULT 0"),
    // the user's own symbol (`sf_symbol` itself is part of the base table)
    ("symbol_ms", "INTEGER NOT NULL DEFAULT 0"),
    // archived accounts (GiroCrab/GiroCrab#43): time of archiving, NULL = active
    ("archived_unix", "INTEGER"),
    ("archived_ms", "INTEGER NOT NULL DEFAULT 0"),
];

/// `user_groups` columns added after the first v7 table (name, definition).
const GROUP_COLUMNS: &[(&str, &str)] = &[
    ("name_ms", "INTEGER NOT NULL DEFAULT 0"),
    ("symbol_ms", "INTEGER NOT NULL DEFAULT 0"),
    ("total_ms", "INTEGER NOT NULL DEFAULT 0"),
    ("order_ms", "INTEGER NOT NULL DEFAULT 0"),
    ("merged_into", "TEXT"),
    // nested groups (GiroCrab/GiroCrab#58)
    ("parent_uuid", "TEXT"),
    ("parent_ms", "INTEGER NOT NULL DEFAULT 0"),
    // group colour (like the account colour); NULL = none chosen
    ("color_hex", "TEXT"),
    ("color_ms", "INTEGER NOT NULL DEFAULT 0"),
];

/// Group fields that merge independently, each with its own stamp.
const GROUP_FIELDS: &[(&str, &str)] = &[
    ("name", "name_ms"),
    ("sf_symbol", "symbol_ms"),
    ("include_in_total", "total_ms"),
    ("sort_order", "order_ms"),
    ("parent_uuid", "parent_ms"),
    ("color_hex", "color_ms"),
];

/// Account colours (the palette of GiroCrab/GiroCrab#26), readable in light
/// and dark mode. Ordered by hue spread: the first [`DISTINCT`] (blue, amber,
/// green, red, fuchsia, lime) are far apart and used first; the rest (pink,
/// violet, cyan, orange, indigo, teal) each sit close to one of them – e.g.
/// blue and violet both turn lavender in dark mode.
pub const ACCOUNT_COLOR_PALETTE: [&str; 12] = [
    "#2563EB", "#D97706", "#059669", "#DC2626", "#C026D3", "#65A30D", // distinct
    "#DB2777", "#7C3AED", "#0891B2", "#EA580C", "#4F46E5", "#0D9488",
];

/// Hash of what the iCloud merge identifies an account by (bank login +
/// external ref): the same on every device. FNV-1a, so it never changes
/// with the Rust version.
pub(crate) fn account_color_hash(
    connector: &str,
    bank_code: &str,
    login: &str,
    external_ref: &str,
) -> u64 {
    [connector, bank_code, login, external_ref]
        .into_iter()
        .fold(Fnv1a64::default(), |h, part| h.field(part))
        .finish()
}

/// Default colours for accounts without a chosen one (`hashes`: account id →
/// [`account_color_hash`]). Chosen colours are reserved first; each other
/// account starts at its hashed palette slot and takes the next free one –
/// the six clearly distinct hues first, so a few accounts never get look-
/// alikes like amber and orange. Up to 12 accounts get distinct colours,
/// the same on every device with the same accounts; beyond 12 the
/// least-used colours repeat.
pub(crate) fn assign_default_colors(accounts: &mut [Account], hashes: &HashMap<i64, u64>) {
    let n = ACCOUNT_COLOR_PALETTE.len();
    let mut used = [0u32; ACCOUNT_COLOR_PALETTE.len()];
    for a in accounts.iter().filter(|a| !hashes.contains_key(&a.id)) {
        if let Some(i) = ACCOUNT_COLOR_PALETTE
            .iter()
            .position(|c| c.eq_ignore_ascii_case(&a.color_hex))
        {
            used[i] += 1;
        }
    }
    let mut order: Vec<(u64, i64)> = hashes.iter().map(|(id, h)| (*h, *id)).collect();
    order.sort_unstable();
    let mut chosen: HashMap<i64, usize> = HashMap::new();
    for (h, id) in order {
        // least-used colours, and among those the clearly distinct first six
        let least = *used.iter().min().unwrap_or(&0);
        let tier = (0..n)
            .filter(|&i| used[i] == least)
            .map(|i| i / DISTINCT)
            .min()
            .unwrap_or(0);
        let start = (h % n as u64) as usize;
        let slot = (0..n)
            .map(|k| (start + k) % n)
            .find(|&i| used[i] == least && i / DISTINCT == tier)
            .unwrap_or(start);
        used[slot] += 1;
        chosen.insert(id, slot);
    }
    for a in accounts.iter_mut() {
        if let Some(&slot) = chosen.get(&a.id) {
            a.color_hex = ACCOUNT_COLOR_PALETTE[slot].to_string();
        }
    }
}

/// Number of clearly distinct palette colours handed out first.
const DISTINCT: usize = 6;

/// The palette offered in the colour picker.
pub fn account_color_palette() -> Vec<String> {
    ACCOUNT_COLOR_PALETTE
        .iter()
        .map(|c| c.to_string())
        .collect()
}

/// Idempotent schema setup of schema 7.
/// Repair step up to schema 9 (`schema::repair_to_v8`); new changes go in `schema.rs`.
pub(crate) fn migrate(conn: &Connection) -> CoreResult<()> {
    conn.execute_batch(SCHEMA)?;
    for (col, def) in ACCOUNT_COLUMNS {
        if !has_column(conn, "main", "accounts", col)? {
            conn.execute_batch(&format!("ALTER TABLE accounts ADD COLUMN {col} {def};"))?;
        }
    }
    // added during review, for vaults that already had the first v7 table
    for (col, def) in GROUP_COLUMNS {
        if !has_column(conn, "main", "user_groups", col)? {
            conn.execute_batch(&format!("ALTER TABLE user_groups ADD COLUMN {col} {def};"))?;
            // (a parent was never chosen in such a vault: its stamp stays 0)
            if col.ends_with("_ms") && *col != "parent_ms" {
                conn.execute_batch(&format!("UPDATE user_groups SET {col} = updated_ms;"))?;
            }
        }
    }
    Ok(())
}

fn group_exists(conn: &Connection, uuid: &str) -> CoreResult<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM user_groups WHERE uuid = ?1 AND deleted_ms IS NULL",
            [uuid],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn not_found(uuid: &str) -> CoreError {
    CoreError::NotFound {
        what: trf("Gruppe {0}", &[&uuid]),
    }
}

/// Trimmed, non-empty name that no other live group uses (case-insensitive).
fn valid_name(conn: &Connection, name: &str, except: Option<&str>) -> CoreResult<String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(CoreError::invalid(tr(
            "Der Gruppenname darf nicht leer sein",
        )));
    }
    let clash: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM user_groups WHERE deleted_ms IS NULL AND lower(name) = lower(?1) AND uuid IS NOT ?2",
            params![name, except],
            |r| r.get(0),
        )
        .optional()?;
    if clash.is_some() {
        return Err(CoreError::invalid(trf(
            "Es gibt schon eine Gruppe „{0}“",
            &[&name],
        )));
    }
    Ok(name)
}

/// Finds a live group by name (case-insensitive) or creates it.
/// Returns `(uuid, created)`.
pub(crate) fn find_or_create(
    conn: &Connection,
    name: &str,
    symbol: &str,
) -> CoreResult<(String, bool)> {
    if let Some(uuid) = conn
        .query_row(
            "SELECT uuid FROM user_groups WHERE deleted_ms IS NULL AND lower(name) = lower(?1)",
            [name.trim()],
            |r| r.get::<_, String>(0),
        )
        .optional()?
    {
        return Ok((uuid, false));
    }
    let name = valid_name(conn, name, None)?;
    let uuid = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO user_groups(uuid, name, sf_symbol, sort_order, updated_ms, name_ms, symbol_ms, total_ms, order_ms)
         VALUES (?1, ?2, ?3, (SELECT IFNULL(MAX(sort_order), -1) + 1 FROM user_groups WHERE deleted_ms IS NULL), ?4, ?4, ?4, ?4, ?4)",
        params![uuid, name, symbol, now_ms()],
    )?;
    Ok((uuid, true))
}

/// Puts an account at the end of a group (`None` = "Ohne Gruppe").
/// `stamp` = 0 for automatic placements, so an explicit user choice wins on sync.
pub(crate) fn assign(
    conn: &Connection,
    account_id: i64,
    uuid: Option<&str>,
    stamp: i64,
) -> CoreResult<()> {
    conn.execute(
        "UPDATE accounts SET user_group_uuid = ?1,
                user_sort_order = (SELECT IFNULL(MAX(user_sort_order), -1) + 1 FROM accounts
                                   WHERE user_group_uuid IS ?1 AND id != ?2),
                placement_ms = ?3
         WHERE id = ?2",
        params![uuid, account_id, stamp],
    )?;
    Ok(())
}

/// A freshly created bank account joins the group most of its bank login's
/// other accounts are in (a family member's login stays with their group).
/// Otherwise it stays in "Ohne Gruppe".
pub(crate) fn place_new_account(conn: &Connection, account_id: i64) -> CoreResult<()> {
    let target: Option<String> = conn
        .query_row(
            "SELECT a.user_group_uuid FROM accounts a
               JOIN user_groups g ON g.uuid = a.user_group_uuid AND g.deleted_ms IS NULL
             WHERE a.id != ?1 AND a.deleted_ms IS NULL
               AND a.connection_id = (SELECT connection_id FROM accounts WHERE id = ?1)
             GROUP BY a.user_group_uuid ORDER BY count(*) DESC, MIN(a.id) LIMIT 1",
            [account_id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(uuid) = target {
        assign(conn, account_id, Some(&uuid), 0)?;
    }
    Ok(())
}

/// `note_ms` of an account note that schema step 32 moved out of the settings
/// (#980): older than any edit. Old builds kept no edit time, so two moved
/// notes that differ are combined instead of one of them winning.
pub(crate) const MOVED_NOTE_MS: i64 = 1;

/// The snapshot of a device still on an older build, just migrated for a
/// merge: that device keeps editing its notes without a stamp, so they get
/// stamp 0 (never edited) and only fill notes this device lacks. Once it
/// updates, step 32 moves them there and they count like moved notes.
pub(crate) fn moved_notes_key(account_id: i64) -> String {
    format!("account.moved_notes.{account_id}")
}

pub(crate) fn mark_old_build_notes(conn: &Connection) -> CoreResult<()> {
    conn.execute(
        "UPDATE accounts SET note_ms = 0 WHERE note_ms = ?1",
        [MOVED_NOTE_MS],
    )?;
    conn.execute(
        "DELETE FROM settings WHERE key LIKE 'account.moved_notes.%'",
        [],
    )?;
    Ok(())
}

/// Where an incoming note in `merge_note_from` came from, so constituent moved
/// notes can be read from the matching vault's `settings`.
pub(crate) enum NoteOrigin<'a> {
    None,
    Schema(&'a str, i64),
}

fn read_moved_parts(conn: &Connection, schema: &str, id: i64, fallback: &str) -> Vec<String> {
    let sql = format!("SELECT value FROM {schema}.settings WHERE key = ?1");
    if let Ok(Some(raw)) = conn
        .query_row(&sql, [moved_notes_key(id)], |r| r.get::<_, String>(0))
        .optional()
        && let Ok(parts) = serde_json::from_str::<Vec<String>>(&raw)
    {
        let clean: Vec<String> = parts
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if !clean.is_empty() {
            return clean;
        }
    }
    let trimmed = fallback.trim();
    if trimmed.is_empty() {
        Vec::new()
    } else {
        vec![trimmed.to_string()]
    }
}

fn write_moved_parts(conn: &Connection, id: i64, parts: &[String]) -> CoreResult<()> {
    let key = moved_notes_key(id);
    if parts.len() > 1 {
        let json = serde_json::to_string(parts).unwrap_or_default();
        conn.execute(
            "INSERT INTO settings(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, json],
        )?;
    } else {
        conn.execute("DELETE FROM settings WHERE key = ?1", [key])?;
    }
    Ok(())
}

/// The note of account `id` and its stamp.
fn account_note(conn: &Connection, id: i64) -> CoreResult<(String, i64)> {
    Ok(conn.query_row(
        "SELECT note, note_ms FROM accounts WHERE id = ?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?)
}

/// Merges `(text, ms)` into the note of account `id` like the other account
/// fields (`take_newer`); moved notes keep every constituent text in a
/// merge-order-independent order, and stamp-0 notes from old builds only fill
/// genuinely missing notes. `fresh` takes it unconditionally.
#[allow(dead_code)]
pub(crate) fn merge_note(
    conn: &Connection,
    id: i64,
    (text, ms): (String, i64),
    fresh: bool,
) -> CoreResult<u32> {
    merge_note_from(conn, id, (text, ms), fresh, NoteOrigin::None)
}

pub(crate) fn merge_note_from(
    conn: &Connection,
    id: i64,
    (text, ms): (String, i64),
    fresh: bool,
    origin: NoteOrigin<'_>,
) -> CoreResult<u32> {
    let (mine, mine_ms) = account_note(conn, id)?;
    let mut combined_parts: Option<Vec<String>> = None;
    let (note, note_ms) = if fresh {
        if ms == MOVED_NOTE_MS
            && let NoteOrigin::Schema(schema, src_id) = origin
        {
            let parts = read_moved_parts(conn, schema, src_id, &text);
            if parts.len() > 1 {
                combined_parts = Some(parts);
            }
        }
        (text, ms)
    } else if ms == 0 {
        if mine.is_empty() && mine_ms == 0 {
            (text, 0)
        } else {
            (mine.clone(), mine_ms)
        }
    } else if ms == MOVED_NOTE_MS && mine_ms == MOVED_NOTE_MS {
        let mut parts = read_moved_parts(conn, "main", id, &mine);
        match origin {
            NoteOrigin::Schema(schema, src_id) => {
                parts.extend(read_moved_parts(conn, schema, src_id, &text));
            }
            NoteOrigin::None => {
                let t = text.trim();
                if !t.is_empty() {
                    parts.push(t.to_string());
                }
            }
        }
        let (joined, norm) = combine_moved_parts(parts.iter().map(String::as_str));
        combined_parts = Some(norm);
        (joined, MOVED_NOTE_MS)
    } else {
        let (mut note, mut note_ms) = (mine.clone(), mine_ms);
        crate::sync_merge::take_newer(&mut note, &mut note_ms, text.clone(), ms);
        if ms == MOVED_NOTE_MS
            && note_ms == MOVED_NOTE_MS
            && let NoteOrigin::Schema(schema, src_id) = origin
        {
            let parts = read_moved_parts(conn, schema, src_id, &text);
            if parts.len() > 1 {
                combined_parts = Some(parts);
            }
        }
        (note, note_ms)
    };
    if note_ms != MOVED_NOTE_MS {
        conn.execute("DELETE FROM settings WHERE key = ?1", [moved_notes_key(id)])?;
    } else if let Some(ref parts) = combined_parts {
        write_moved_parts(conn, id, parts)?;
    }
    if note == mine && note_ms == mine_ms {
        return Ok(0);
    }
    Ok(conn.execute(
        "UPDATE accounts SET note = ?2, note_ms = ?3 WHERE id = ?1",
        params![id, note, note_ms],
    )? as u32)
}

/// The note of account `from` merged into account `to` (a folded twin's note
/// goes to its bank account).
pub(crate) fn pass_note_on(conn: &Connection, from: i64, to: i64) -> CoreResult<()> {
    merge_note_from(
        conn,
        to,
        account_note(conn, from)?,
        false,
        NoteOrigin::Schema("main", from),
    )?;
    Ok(())
}

/// Combines constituent moved notes whole and trimmed: drops any note already
/// contained in another constituent, sorts the remaining whole notes, and joins
/// them with a line break. Associative, commutative, and idempotent.
fn combine_moved_parts<'a>(raw: impl IntoIterator<Item = &'a str>) -> (String, Vec<String>) {
    let mut unique: Vec<String> = raw
        .into_iter()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    unique.sort();
    unique.dedup();
    let kept: Vec<String> = unique
        .iter()
        .filter(|p| {
            !unique
                .iter()
                .any(|q| q.as_str() != p.as_str() && q.contains(p.as_str()))
        })
        .cloned()
        .collect();
    (kept.join("\n"), kept)
}

#[cfg(test)]
fn combine_moved_notes(a: &str, b: &str) -> String {
    combine_moved_parts([a, b]).0
}

/// Merges user groups, account placements and account fields (name, colour,
/// archived, symbol, note) from the attached `other` vault (newest edit wins).
/// `acc_map` maps other → local account ids; accounts in `inserted` were just
/// created by the merge and take the other side's values unconditionally.
pub(crate) fn merge_attached(
    tx: &Connection,
    acc_map: &HashMap<i64, i64>,
    inserted: &HashSet<i64>,
) -> CoreResult<u32> {
    // Field by field the newer edit wins (equal stamps: the larger value);
    // tombstones only ever get set (delete wins). Both rules are symmetric, so
    // A←B and B←A end up identical.
    // (`merge_other_vault` migrates the other file first, so it has all columns.)
    let mut changed = tx.execute(&group_upsert_sql(), [])? as u32;
    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        (i64, Option<String>, i64, i64, String, i64, Option<String>, i64),
        (Option<i64>, i64, String, i64, String, i64),
    )> = tx
        .prepare(
            // includes soft-deleted rows (vault merge)
            "SELECT id, user_group_uuid, user_sort_order, placement_ms, name, name_ms, color_hex, color_ms,
                    archived_unix, archived_ms, sf_symbol, symbol_ms, note, note_ms
             FROM other.accounts",
        )?
        .query_map([], |r| {
            Ok((
                (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?),
                (r.get(8)?, r.get(9)?, r.get(10)?, r.get(11)?, r.get(12)?, r.get(13)?),
            ))
        })?
        .collect::<Result<_, _>>()?;
    for (
        (oid, uuid, order, placed, name, named, color, colored),
        (archived, archived_ms, symbol, symbol_ms, note, noted),
    ) in rows
    {
        let Some(&id) = acc_map.get(&oid) else {
            continue;
        };
        let fresh = inserted.contains(&id);
        changed += tx.execute(
            "UPDATE accounts SET user_group_uuid = ?2, user_sort_order = ?3, placement_ms = ?4
             WHERE id = ?1 AND (?5 OR placement_ms < ?4)",
            params![id, uuid, order, placed, fresh],
        )? as u32;
        // counted like the other fields: a merge that only renames reloads the app
        changed += tx.execute(
            "UPDATE accounts SET name = ?2, name_ms = ?3 WHERE id = ?1 AND (?4 OR name_ms < ?3)",
            params![id, name, named, fresh],
        )? as u32;
        changed += tx.execute(
            "UPDATE accounts SET color_hex = ?2, color_ms = ?3 WHERE id = ?1 AND (?4 OR color_ms < ?3)",
            params![id, color, colored, fresh],
        )? as u32;
        changed += tx.execute(
            "UPDATE accounts SET archived_unix = ?2, archived_ms = ?3 WHERE id = ?1 AND (?4 OR archived_ms < ?3)",
            params![id, archived, archived_ms, fresh],
        )? as u32;
        // only a user's choice travels; the type's default stays local
        changed += tx.execute(
            "UPDATE accounts SET sf_symbol = ?2, symbol_ms = ?3 WHERE id = ?1 AND symbol_ms < ?3",
            params![id, symbol, symbol_ms],
        )? as u32;
        let note_rows = merge_note_from(
            tx,
            id,
            (note, noted),
            fresh,
            NoteOrigin::Schema("other", oid),
        )?;
        // a MoneyMoney twin folded here and edited on a device that hadn't
        // folded it yet: the note goes on to the bank account, like the fold
        if note_rows > 0
            && let Some(bank) = crate::sync_merge::twins::folded_into(tx, id)?
        {
            pass_note_on(tx, id, bank)?;
        }
        changed += note_rows;
    }
    changed += combine_same_names(tx)?;
    changed += follow_merges(tx)?;
    // Placements pointing at a group deleted on either side → "Ohne Gruppe".
    tx.execute(
        "UPDATE accounts SET user_group_uuid = NULL
         WHERE user_group_uuid IN (SELECT uuid FROM user_groups WHERE deleted_ms IS NOT NULL)",
        [],
    )?;
    Ok(changed)
}

/// `INSERT … ON CONFLICT DO UPDATE` for all groups of the attached vault.
fn group_upsert_sql() -> String {
    let wins = |f: &str, ms: &str| {
        format!(
            "(excluded.{ms} > user_groups.{ms} OR (excluded.{ms} = user_groups.{ms} AND IFNULL(excluded.{f}, '') > IFNULL(user_groups.{f}, '')))"
        )
    };
    let mut cols = vec!["uuid", "updated_ms", "deleted_ms", "merged_into"];
    let mut sets = vec![
        "updated_ms = max(excluded.updated_ms, user_groups.updated_ms)".to_string(),
        "deleted_ms = coalesce(min(user_groups.deleted_ms, excluded.deleted_ms), user_groups.deleted_ms, excluded.deleted_ms)".into(),
        "merged_into = coalesce(min(user_groups.merged_into, excluded.merged_into), user_groups.merged_into, excluded.merged_into)".into(),
    ];
    let mut when = vec![
        "(excluded.deleted_ms IS NOT NULL AND user_groups.deleted_ms IS NULL)".to_string(),
        "(excluded.merged_into IS NOT NULL AND user_groups.merged_into IS NOT excluded.merged_into)".into(),
    ];
    for (f, ms) in GROUP_FIELDS {
        cols.extend([*f, *ms]);
        let w = wins(f, ms);
        sets.push(format!(
            "{f} = CASE WHEN {w} THEN excluded.{f} ELSE user_groups.{f} END"
        ));
        sets.push(format!("{ms} = max(excluded.{ms}, user_groups.{ms})"));
        when.push(w);
    }
    let cols = cols.join(", ");
    format!(
        "INSERT INTO user_groups({cols}) SELECT {cols} FROM other.user_groups WHERE true
         ON CONFLICT(uuid) DO UPDATE SET {} WHERE {}",
        sets.join(", "),
        when.join(" OR ")
    )
}

/// Live groups with the same name (case-insensitive, e.g. "Privat" created on
/// two devices) become one: the lower uuid survives – the same choice on every
/// device – and takes each field from the duplicate that edited it last (the
/// same rule as the merge). The others become tombstones pointing at it.
/// Nothing is stamped with the current time, so every device computes exactly
/// the same result.
fn combine_same_names(tx: &Connection) -> CoreResult<u32> {
    let rows: Vec<(String, String)> = tx
        .prepare(
            "SELECT lower(trim(name)), uuid FROM user_groups g
             WHERE deleted_ms IS NULL AND EXISTS (
                SELECT 1 FROM user_groups o WHERE o.deleted_ms IS NULL AND o.uuid != g.uuid
                  AND lower(trim(o.name)) = lower(trim(g.name)))
             ORDER BY 1, uuid",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let mut changed = 0;
    for dups in rows.chunk_by(|a, b| a.0 == b.0) {
        let survivor = &dups[0].1;
        for (f, ms) in GROUP_FIELDS {
            // the winning value among all duplicates, by (stamp, value)
            tx.execute(
                &format!(
                    "UPDATE user_groups SET ({f}, {ms}) = (
                        SELECT d.{f}, d.{ms} FROM user_groups d
                        WHERE d.deleted_ms IS NULL AND lower(trim(d.name)) = ?2
                        ORDER BY d.{ms} DESC, IFNULL(d.{f}, '') DESC LIMIT 1)
                     WHERE uuid = ?1"
                ),
                params![survivor, dups[0].0],
            )?;
        }
        let stamp: i64 = tx.query_row(
            "SELECT max(updated_ms) FROM user_groups WHERE deleted_ms IS NULL AND lower(trim(name)) = ?1",
            [&dups[0].0],
            |r| r.get(0),
        )?;
        tx.execute(
            "UPDATE user_groups SET updated_ms = ?2 WHERE uuid = ?1",
            params![survivor, stamp],
        )?;
        for d in &dups[1..] {
            retire_into(tx, &d.1, survivor, stamp)?;
            changed += 1;
        }
    }
    Ok(changed)
}

/// Turns `from` into a tombstone that points at `into`.
fn retire_into(tx: &Connection, from: &str, into: &str, stamp: i64) -> CoreResult<()> {
    tx.execute(
        "UPDATE user_groups SET deleted_ms = coalesce(deleted_ms, ?3),
                updated_ms = max(updated_ms, ?3), merged_into = ?2
         WHERE uuid = ?1",
        params![from, into, stamp],
    )?;
    Ok(())
}

/// Moves the accounts of combined groups to the end of the group they were
/// combined into (following chains). Placement stamps stay as they are: this
/// is derived from the tombstones, not a new user edit.
fn follow_merges(tx: &Connection) -> CoreResult<u32> {
    let mut moved = 0;
    for _ in 0..8 {
        let pairs: Vec<(String, String)> = tx
            .prepare(
                // includes soft-deleted rows: a revived account lands in the right group
                "SELECT DISTINCT g.uuid, g.merged_into FROM accounts a
                   JOIN user_groups g ON g.uuid = a.user_group_uuid
                 WHERE g.merged_into IS NOT NULL ORDER BY g.uuid",
            )?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        if pairs.is_empty() {
            break;
        }
        for (from, into) in pairs {
            let ids: Vec<i64> = tx
                .prepare(
                    // includes soft-deleted rows
                    "SELECT id FROM accounts WHERE user_group_uuid = ?1
                     ORDER BY user_sort_order, name, id",
                )?
                .query_map([&from], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            let base: i64 = tx.query_row(
                "SELECT IFNULL(MAX(user_sort_order), -1) + 1 FROM accounts WHERE user_group_uuid = ?1",
                [&into],
                |r| r.get(0),
            )?;
            for (i, id) in ids.iter().enumerate() {
                tx.execute(
                    "UPDATE accounts SET user_group_uuid = ?2, user_sort_order = ?3 WHERE id = ?1",
                    params![id, into, base + i as i64],
                )?;
                moved += 1;
            }
        }
    }
    Ok(moved)
}

/// The group tree as shown: live group uuid → its parent (`None` = top
/// level). Derived from the stored parents, identically on every device:
/// deleted parents are skipped, combined ones followed to the survivor, and
/// a loop is cut at its lowest uuid.
pub(crate) fn effective_parents(conn: &Connection) -> CoreResult<HashMap<String, Option<String>>> {
    struct Row {
        parent: Option<String>,
        live: bool,
        merged_into: Option<String>,
    }
    let rows: HashMap<String, Row> = conn
        .prepare("SELECT uuid, parent_uuid, deleted_ms IS NULL, merged_into FROM user_groups")?
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                Row {
                    parent: r.get(1)?,
                    live: r.get(2)?,
                    merged_into: r.get(3)?,
                },
            ))
        })?
        .collect::<Result<_, _>>()?;
    let mut out: HashMap<String, Option<String>> = HashMap::new();
    for (uuid, row) in rows.iter().filter(|(_, r)| r.live) {
        let mut next = row.parent.clone();
        let mut parent = None;
        for _ in 0..64 {
            let Some(p) = next else { break };
            match rows.get(&p) {
                Some(r) if r.live => {
                    parent = Some(p);
                    break;
                }
                // tombstone: the group it was combined into, else its own parent
                Some(r) => next = r.merged_into.clone().or_else(|| r.parent.clone()),
                None => break,
            }
        }
        out.insert(uuid.clone(), parent.filter(|p| p != uuid));
    }
    // cut loops, always at the lowest uuid of the loop
    let mut uuids: Vec<String> = out.keys().cloned().collect();
    uuids.sort_unstable();
    for start in &uuids {
        let mut path = vec![start.clone()];
        while let Some(Some(p)) = out.get(path.last().unwrap()).cloned() {
            if let Some(i) = path.iter().position(|x| *x == p) {
                let cut = path[i..].iter().min().unwrap().clone();
                out.insert(cut, None);
                break;
            }
            path.push(p);
        }
    }
    Ok(out)
}

/// Groups whose accounts don't count towards the overall total: excluded
/// themselves or inside an excluded group.
pub(crate) fn excluded_from_total(conn: &Connection) -> CoreResult<HashSet<String>> {
    let parents = effective_parents(conn)?;
    let own: HashSet<String> = conn
        .prepare("SELECT uuid FROM user_groups WHERE deleted_ms IS NULL AND include_in_total = 0")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    if own.is_empty() {
        return Ok(own);
    }
    Ok(parents
        .keys()
        .filter(|u| {
            let mut cur = Some((*u).clone());
            while let Some(c) = cur {
                if own.contains(&c) {
                    return true;
                }
                cur = parents.get(&c).cloned().flatten();
            }
            false
        })
        .cloned()
        .collect())
}

/// `uuid` and all groups below it.
fn subtree(parents: &HashMap<String, Option<String>>, uuid: &str) -> HashSet<String> {
    parents
        .keys()
        .filter(|u| {
            let mut cur = Some((*u).clone());
            while let Some(c) = cur {
                if c == uuid {
                    return true;
                }
                cur = parents.get(&c).cloned().flatten();
            }
            false
        })
        .cloned()
        .collect()
}

/// Makes `uuid` a subgroup of `parent` (`None` = top level).
pub(crate) fn set_parent(
    conn: &Connection,
    uuid: &str,
    parent: Option<&str>,
    stamp: i64,
) -> CoreResult<()> {
    conn.execute(
        "UPDATE user_groups SET parent_uuid = ?2, parent_ms = ?3, updated_ms = max(updated_ms, ?3)
         WHERE uuid = ?1 AND (parent_uuid IS NOT ?2 OR parent_ms < ?3)",
        params![uuid, parent, stamp],
    )?;
    Ok(())
}

/// Order of kinds inside a group: Giro → Karten → Depots → Darlehen.
fn kind_rank(a: &Account) -> i32 {
    group_for(a.kind).2
}

#[uniffi::export]
impl Vault {
    /// Sidebar data for the "Nach Gruppe" view: all user groups (also empty
    /// ones) as a flattened tree – every group is followed by its subgroups
    /// (`depth`, `parent_uuid`) – then, if any account has no group, an
    /// "Ohne Gruppe" entry (`is_ungrouped`, empty uuid). `balance_cents`
    /// includes the subgroups; `accounts` are the group's own.
    pub fn list_user_groups(&self) -> Result<Vec<UserGroup>, CoreError> {
        let accounts = self.list_accounts()?;
        let order: HashMap<i64, i64> = {
            let db = self.db()?;
            let mut stmt = db.prepare("SELECT id, user_sort_order FROM accounts")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<Result<_, _>>()?
        };
        let groups: Vec<(String, String, String, i32, bool, Option<String>)> = {
            let db = self.db()?;
            let mut stmt = db.prepare(
                "SELECT uuid, name, sf_symbol, sort_order, include_in_total, color_hex FROM user_groups
                 WHERE deleted_ms IS NULL ORDER BY sort_order, name COLLATE NOCASE",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })?;
            rows.collect::<Result<_, _>>()?
        };
        let members = |uuid: Option<&str>| -> Vec<Account> {
            let mut v: Vec<Account> = accounts
                .iter()
                .filter(|a| a.user_group_uuid.as_deref() == uuid)
                .cloned()
                .collect();
            v.sort_by_key(|a| {
                (
                    order.get(&a.id).copied().unwrap_or(0),
                    kind_rank(a),
                    // then synced values only (local ids differ between devices)
                    a.name.to_lowercase(),
                    a.iban.clone(),
                    a.account_number.clone(),
                    a.id,
                )
            });
            v
        };
        // archived accounts stay listed but don't count (GiroCrab/GiroCrab#208)
        let sum = |v: &[Account]| {
            v.iter()
                .filter(|a| a.currency == "EUR" && a.archived_unix.is_none())
                .map(|a| a.balance_cents)
                .sum()
        };
        let (parents, excluded) = {
            let db = self.db()?;
            (effective_parents(&db)?, excluded_from_total(&db)?)
        };
        // children in list order (`groups` is sorted), then depth-first
        let mut children: HashMap<Option<&str>, Vec<usize>> = HashMap::new();
        for (i, g) in groups.iter().enumerate() {
            let parent = parents.get(&g.0).and_then(|p| p.as_deref());
            children.entry(parent).or_default().push(i);
        }
        let mut out: Vec<UserGroup> = Vec::with_capacity(groups.len() + 1);
        // (index into `groups`, depth); a group's subgroups follow it
        let mut stack: Vec<(usize, u32)> = children
            .get(&None)
            .map(|v| v.iter().rev().map(|i| (*i, 0)).collect())
            .unwrap_or_default();
        while let Some((i, depth)) = stack.pop() {
            let (uuid, name, sf_symbol, sort_order, include_in_total, color) = &groups[i];
            let accs = members(Some(uuid));
            if let Some(kids) = children.get(&Some(uuid.as_str())) {
                stack.extend(kids.iter().rev().map(|k| (*k, depth + 1)));
            }
            out.push(UserGroup {
                balance_cents: sum(&accs),
                uuid: uuid.clone(),
                name: name.clone(),
                sf_symbol: sf_symbol.clone(),
                color_hex: color.clone().unwrap_or_default(),
                sort_order: *sort_order,
                include_in_total: *include_in_total,
                in_total: !excluded.contains(uuid),
                is_ungrouped: false,
                parent_uuid: parents.get(uuid).cloned().flatten(),
                depth,
                accounts: accs,
            });
        }
        // sums include the subgroups: add each group to its parent, deepest
        // first – except subgroups taken out of the total themselves, which
        // keep their own sum but don't count for the groups above
        // (GiroCrab/GiroCrab#208)
        let index: HashMap<String, usize> = out
            .iter()
            .enumerate()
            .map(|(i, g)| (g.uuid.clone(), i))
            .collect();
        for i in (0..out.len()).rev() {
            if !out[i].include_in_total {
                continue;
            }
            if let Some(p) = out[i].parent_uuid.as_ref().and_then(|p| index.get(p)) {
                out[*p].balance_cents += out[i].balance_cents;
            }
        }
        let rest = members(None);
        if !rest.is_empty() {
            out.push(UserGroup {
                uuid: String::new(),
                name: tr("Ohne Gruppe").into(),
                sf_symbol: "tray".into(),
                color_hex: String::new(),
                sort_order: i32::MAX,
                include_in_total: true,
                in_total: true,
                is_ungrouped: true,
                parent_uuid: None,
                depth: 0,
                balance_cents: sum(&rest),
                accounts: rest,
            });
        }
        Ok(out)
    }

    /// Creates a group at the end of the list and returns its uuid.
    pub fn create_user_group(&self, name: String, sf_symbol: String) -> Result<String, CoreError> {
        let db = self.db()?;
        let name = valid_name(&db, &name, None)?;
        Ok(find_or_create(&db, &name, &sf_symbol)?.0)
    }

    /// Renames a group, changes its symbol and whether it counts towards the total.
    pub fn update_user_group(
        &self,
        uuid: String,
        name: String,
        sf_symbol: String,
        include_in_total: bool,
    ) -> Result<(), CoreError> {
        let db = self.db()?;
        if !group_exists(&db, &uuid)? {
            return Err(not_found(&uuid));
        }
        let same: bool = db.query_row(
            "SELECT lower(name) = lower(trim(?2)) FROM user_groups WHERE uuid = ?1",
            params![uuid, name],
            |r| r.get(0),
        )?;
        let name = if same && !name.trim().is_empty() {
            name.trim().to_string()
        } else {
            valid_name(&db, &name, Some(&uuid))?
        };
        // only fields that actually change get a new stamp, so a concurrent
        // edit of another field on another device survives the merge
        db.execute(
            "UPDATE user_groups SET
                name_ms   = CASE WHEN name IS NOT ?2 THEN ?5 ELSE name_ms END,
                symbol_ms = CASE WHEN sf_symbol IS NOT ?3 THEN ?5 ELSE symbol_ms END,
                total_ms  = CASE WHEN include_in_total IS NOT ?4 THEN ?5 ELSE total_ms END,
                updated_ms = CASE WHEN name IS NOT ?2 OR sf_symbol IS NOT ?3 OR include_in_total IS NOT ?4
                                  THEN ?5 ELSE updated_ms END,
                name = ?2, sf_symbol = ?3, include_in_total = ?4
             WHERE uuid = ?1",
            params![uuid, name, sf_symbol, include_in_total, now_ms()],
        )?;
        Ok(())
    }

    /// Makes a group a subgroup of `parent_uuid` (`None` = top level). A group
    /// can't be moved into itself or one of its own subgroups.
    pub fn set_user_group_parent(
        &self,
        uuid: String,
        parent_uuid: Option<String>,
    ) -> Result<(), CoreError> {
        let db = self.db()?;
        for u in std::iter::once(&uuid).chain(parent_uuid.iter()) {
            if !group_exists(&db, u)? {
                return Err(not_found(u));
            }
        }
        if let Some(p) = &parent_uuid
            && subtree(&effective_parents(&db)?, &uuid).contains(p)
        {
            return Err(CoreError::invalid(tr(
                "Eine Gruppe kann nicht in sich selbst oder eine ihrer Untergruppen verschoben werden",
            )));
        }
        set_parent(&db, &uuid, parent_uuid.as_deref(), now_ms())
    }

    /// Deletes a group. Its accounts and subgroups are kept and move up into
    /// the group it was in (accounts of a top-level group: "Ohne Gruppe").
    pub fn delete_user_group(&self, uuid: String) -> Result<(), CoreError> {
        let db = self.db()?;
        if !group_exists(&db, &uuid)? {
            return Err(not_found(&uuid));
        }
        let now = now_ms();
        let tx = db.unchecked_transaction()?;
        let parents = effective_parents(&tx)?;
        let parent = parents.get(&uuid).cloned().flatten();
        for (child, _) in parents
            .iter()
            .filter(|(_, p)| p.as_deref() == Some(uuid.as_str()))
        {
            set_parent(&tx, child, parent.as_deref(), now)?;
        }
        tx.execute(
            "UPDATE user_groups SET deleted_ms = ?2, updated_ms = ?2 WHERE uuid = ?1",
            params![uuid, now],
        )?;
        let ids: Vec<i64> = tx
            .prepare(
                // includes soft-deleted rows: none may keep pointing at the deleted group
                "SELECT id FROM accounts WHERE user_group_uuid = ?1 ORDER BY user_sort_order, id",
            )?
            .query_map([&uuid], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        for id in ids {
            assign(&tx, id, parent.as_deref(), now)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Combines two groups: the accounts of `from` move to the end of `into`,
    /// its subgroups become subgroups of `into` and `from` is removed. Other
    /// devices follow on their next sync.
    pub fn merge_user_groups(&self, from_uuid: String, into_uuid: String) -> Result<(), CoreError> {
        let db = self.db()?;
        for u in [&from_uuid, &into_uuid] {
            if !group_exists(&db, u)? {
                return Err(not_found(u));
            }
        }
        if from_uuid == into_uuid {
            return Ok(());
        }
        let tx = db.unchecked_transaction()?;
        let now = now_ms();
        let parents = effective_parents(&tx)?;
        // `into` inside `from`: it takes the place of `from`
        if subtree(&parents, &from_uuid).contains(&into_uuid) {
            let up = parents.get(&from_uuid).cloned().flatten();
            set_parent(&tx, &into_uuid, up.as_deref(), now)?;
        }
        for (child, _) in parents
            .iter()
            .filter(|(c, p)| p.as_deref() == Some(from_uuid.as_str()) && **c != into_uuid)
        {
            set_parent(&tx, child, Some(&into_uuid), now)?;
        }
        retire_into(&tx, &from_uuid, &into_uuid, now)?;
        follow_merges(&tx)?;
        tx.commit()?;
        Ok(())
    }

    /// Moves an account into a group (`None` = "Ohne Gruppe"), at `index`
    /// within that group (`None` = at the end).
    pub fn move_account_to_group(
        &self,
        account_id: i64,
        group_uuid: Option<String>,
        index: Option<u32>,
    ) -> Result<(), CoreError> {
        let db = self.db()?;
        if let Some(u) = &group_uuid
            && !group_exists(&db, u)?
        {
            return Err(not_found(u));
        }
        let mut ids: Vec<i64> = db
            .prepare(
                "SELECT id FROM accounts WHERE user_group_uuid IS ?1 AND id != ?2 AND deleted_ms IS NULL
                 ORDER BY user_sort_order, id",
            )?
            .query_map(params![group_uuid, account_id], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        let at = index.map_or(ids.len(), |i| (i as usize).min(ids.len()));
        ids.insert(at, account_id);
        let tx = db.unchecked_transaction()?;
        let n = tx.execute(
            "UPDATE accounts SET user_group_uuid = ?2 WHERE id = ?1",
            params![account_id, group_uuid],
        )?;
        if n == 0 {
            return Err(CoreError::NotFound {
                what: trf("Konto {0}", &[&account_id]),
            });
        }
        renumber(&tx, &ids)?;
        tx.commit()?;
        Ok(())
    }

    /// Archives an account ("Das Konto wurde aufgelöst") or reactivates it.
    /// Archived accounts keep their bookings, categories, notes, documents
    /// and group, but syncs no longer store bookings or balances for them,
    /// rule runs skip their bookings and they leave the overall total. The
    /// state syncs like the account's name (newest change wins).
    pub fn set_account_archived(&self, account_id: i64, archived: bool) -> Result<(), CoreError> {
        // keeps the original date when archiving an already archived account
        let n = self.db()?.execute(
            "UPDATE accounts SET archived_unix = CASE WHEN ?2 THEN IFNULL(archived_unix, strftime('%s','now')) END,
                                 archived_ms = ?3
             WHERE id = ?1",
            params![account_id, archived, now_ms()],
        )?;
        if n == 0 {
            return Err(CoreError::NotFound {
                what: trf("Konto {0}", &[&account_id]),
            });
        }
        Ok(())
    }

    /// Sets an account's SF Symbol (chosen by the user instead of the one
    /// from its account type); syncs like the colour, the newer choice wins.
    pub fn set_account_symbol(&self, account_id: i64, sf_symbol: String) -> Result<(), CoreError> {
        let symbol = sf_symbol.trim();
        if symbol.is_empty() {
            return Err(CoreError::invalid(tr("Kein Symbol angegeben")));
        }
        let n = self.db()?.execute(
            "UPDATE accounts SET sf_symbol = ?2, symbol_ms = ?3 WHERE id = ?1",
            params![account_id, symbol, now_ms()],
        )?;
        if n == 0 {
            return Err(CoreError::NotFound {
                what: trf("Konto {0}", &[&account_id]),
            });
        }
        Ok(())
    }

    /// Back to the symbol of the account's kind (the "Standardsymbol" button).
    /// Syncs like a choice: the newer change wins.
    pub fn reset_account_symbol(&self, account_id: i64) -> Result<(), CoreError> {
        let db = self.db()?;
        let kind: Option<String> = db
            .query_row(
                "SELECT kind FROM accounts WHERE id = ?1 AND deleted_ms IS NULL",
                [account_id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(kind) = kind else {
            return Err(CoreError::NotFound {
                what: trf("Konto {0}", &[&account_id]),
            });
        };
        db.execute(
            "UPDATE accounts SET sf_symbol = ?2, symbol_ms = ?3 WHERE id = ?1",
            params![
                account_id,
                crate::models::AccountKind::from_str(&kind).sf_symbol(),
                now_ms()
            ],
        )?;
        Ok(())
    }

    /// Sets a group's colour (`#RRGGBB`); `None` = none. Syncs like the
    /// account colour: the newer choice wins.
    pub fn set_user_group_color(
        &self,
        uuid: String,
        color_hex: Option<String>,
    ) -> Result<(), CoreError> {
        let hex = valid_color(color_hex)?;
        let db = self.db()?;
        if !group_exists(&db, &uuid)? {
            return Err(not_found(&uuid));
        }
        let now = now_ms();
        db.execute(
            "UPDATE user_groups SET color_hex = ?2, color_ms = ?3, updated_ms = ?3 WHERE uuid = ?1",
            params![uuid, hex, now],
        )?;
        Ok(())
    }

    /// Sets an account's colour (`#RRGGBB`); `None` = back to its default.
    pub fn set_account_color(
        &self,
        account_id: i64,
        color_hex: Option<String>,
    ) -> Result<(), CoreError> {
        let hex = valid_color(color_hex)?;
        let n = self.db()?.execute(
            "UPDATE accounts SET color_hex = ?2, color_ms = ?3 WHERE id = ?1",
            params![account_id, hex, now_ms()],
        )?;
        if n == 0 {
            return Err(CoreError::NotFound {
                what: trf("Konto {0}", &[&account_id]),
            });
        }
        Ok(())
    }

    /// Sets the order of all groups (uuids in the new order).
    pub fn reorder_user_groups(&self, uuids: Vec<String>) -> Result<(), CoreError> {
        let db = self.db()?;
        let now = now_ms();
        let tx = db.unchecked_transaction()?;
        for (i, u) in uuids.iter().enumerate() {
            tx.execute(
                "UPDATE user_groups SET sort_order = ?2, order_ms = ?3, updated_ms = ?3
                 WHERE uuid = ?1 AND (sort_order != ?2)",
                params![u, i as i64, now],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Sets the order of the accounts inside one group (`None` = "Ohne Gruppe").
    /// Ids not in that group are ignored.
    pub fn reorder_accounts_in_group(
        &self,
        group_uuid: Option<String>,
        account_ids: Vec<i64>,
    ) -> Result<(), CoreError> {
        let db = self.db()?;
        let members: HashSet<i64> = db
            .prepare("SELECT id FROM accounts WHERE user_group_uuid IS ?1")?
            .query_map([&group_uuid], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        let ids: Vec<i64> = account_ids
            .into_iter()
            .filter(|i| members.contains(i))
            .collect();
        let tx = db.unchecked_transaction()?;
        renumber(&tx, &ids)?;
        tx.commit()?;
        Ok(())
    }
}

/// Gives `ids` the sort orders 0, 1, 2 … and stamps changed placements.
fn renumber(tx: &Connection, ids: &[i64]) -> CoreResult<()> {
    let now = now_ms();
    for (i, id) in ids.iter().enumerate() {
        tx.execute(
            "UPDATE accounts SET user_sort_order = ?2, placement_ms = ?3 WHERE id = ?1",
            params![id, i as i64, now],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;

/// A colour as `#RRGGBB` (upper case); `None` stays `None`.
fn valid_color(color_hex: Option<String>) -> Result<Option<String>, CoreError> {
    match color_hex.map(|h| h.trim().to_uppercase()) {
        Some(h)
            if h.len() == 7
                && h.starts_with('#')
                && h[1..].chars().all(|c| c.is_ascii_hexdigit()) =>
        {
            Ok(Some(h))
        }
        Some(h) => Err(CoreError::invalid(trf("Ungültige Farbe: {0}", &[&h]))),
        None => Ok(None),
    }
}
