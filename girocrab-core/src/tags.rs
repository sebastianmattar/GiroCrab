//! Tags on bookings (GitHub #228): free labels across all categories and
//! accounts ("Urlaub 2026", "Sohn", "Steuer").
//!
//! A booking keeps exactly one category (or one per split), so the category
//! sums still add up to 100 %; tags are a second dimension beside it, any
//! number per booking. A tag belongs to the whole booking, not to a split.
//!
//! Sync: a tag is identified by its `uuid`, derived from its first name (two
//! devices that create "Urlaub" independently create the same tag); a rename
//! keeps the uuid. The newest edit (`updated_ms`) wins for name, colour and
//! order, a deletion stays. The tag set of a booking is one synced field
//! (`transactions.tags_ms`, like `splits_ms`): the newest edit wins as a whole.
//! A deleted tag drops off every booking without a stamp (like a deleted
//! category); a tag merged into another (`merged_into`) moves its bookings to
//! the survivor, on every device.

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension, params, params_from_iter};

use crate::dates::now_ms;
use crate::error::{CoreError, CoreResult};
use crate::i18n::tr;
use crate::schema::has_column;
use crate::vault::Vault;

pub(crate) mod merge;
mod totals;
pub use totals::{TagAccountCount, TagTotal};
#[cfg(test)]
mod tests;

const TAGS: &str = "
CREATE TABLE IF NOT EXISTS tags (
    id          INTEGER PRIMARY KEY,
    uuid        TEXT    NOT NULL UNIQUE,            -- sync key
    name        TEXT    NOT NULL,
    color_hex   TEXT    NOT NULL DEFAULT '',        -- '' = default colour
    sort_order  INTEGER NOT NULL DEFAULT 0,
    updated_ms  INTEGER NOT NULL DEFAULT 0,         -- newest edit wins in a vault merge
    deleted_ms  INTEGER,                            -- tombstone, kept for sync
    merged_into TEXT                                -- tombstone of a merged tag -> survivor uuid
);
CREATE TABLE IF NOT EXISTS transaction_tags (
    transaction_id INTEGER NOT NULL REFERENCES transactions(id) ON DELETE CASCADE,
    tag_id         INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    PRIMARY KEY (transaction_id, tag_id)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS transaction_tags_tag ON transaction_tags(tag_id, transaction_id);
";

/// Longest tag name (characters).
const MAX_NAME: usize = 64;

/// Step 23 (#228): tables `tags`, `transaction_tags` and the edit stamp
/// `transactions.tags_ms`. Idempotent like the steps before it.
pub(crate) fn tags_tables(conn: &Connection) -> CoreResult<()> {
    conn.execute_batch(TAGS)?;
    if !has_column(conn, "main", "transactions", "tags_ms")? {
        conn.execute_batch(
            "ALTER TABLE transactions ADD COLUMN tags_ms INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    Ok(())
}

/// A tag with the number of live bookings that carry it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Tag {
    pub id: i64,
    pub name: String,
    /// `#RRGGBB`, "" = default colour.
    pub color_hex: String,
    pub sort_order: i32,
    /// Live bookings with this tag (all accounts, all dates).
    pub booking_count: u32,
}

/// A tag as a booking shows it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BookingTag {
    pub id: i64,
    pub name: String,
    pub color_hex: String,
}

/// Trimmed name, inner runs of whitespace collapsed; checked for length.
fn clean_name(name: &str) -> CoreResult<String> {
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        return Err(CoreError::invalid(tr("Der Name des Tags fehlt.")));
    }
    if name.chars().count() > MAX_NAME {
        return Err(CoreError::invalid(tr(
            "Der Name des Tags ist länger als 64 Zeichen.",
        )));
    }
    Ok(name)
}

fn clean_color(color: &str) -> CoreResult<String> {
    let c = color.trim();
    let ok = c.is_empty()
        || (c.len() == 7 && c.starts_with('#') && c[1..].chars().all(|h| h.is_ascii_hexdigit()));
    if !ok {
        return Err(CoreError::invalid(tr("Die Farbe ist ungültig (#RRGGBB).")));
    }
    Ok(c.to_uppercase())
}

/// Case-insensitive key of a tag name.
pub(crate) fn fold(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn not_found(id: i64) -> CoreError {
    CoreError::NotFound {
        what: format!("tag {id}"),
    }
}

/// The live tag with this name (case-insensitive), if any.
fn live_by_name(conn: &Connection, name: &str) -> CoreResult<Option<i64>> {
    let key = fold(name);
    let rows: Vec<(i64, String)> = conn
        .prepare("SELECT id, name FROM tags WHERE deleted_ms IS NULL")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    Ok(rows.into_iter().find(|(_, n)| fold(n) == key).map(|r| r.0))
}

fn ensure_live(conn: &Connection, id: i64) -> CoreResult<()> {
    let live: Option<i64> = conn
        .query_row(
            "SELECT id FROM tags WHERE id = ?1 AND deleted_ms IS NULL",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    live.map(|_| ()).ok_or_else(|| not_found(id))
}

fn tag_row(conn: &Connection, id: i64) -> CoreResult<Tag> {
    conn.query_row(
        "SELECT g.id, g.name, g.color_hex, g.sort_order,
                (SELECT count(*) FROM transaction_tags x JOIN transactions t ON t.id = x.transaction_id
                  WHERE x.tag_id = g.id AND t.deleted_ms IS NULL)
           FROM tags g WHERE g.id = ?1 AND g.deleted_ms IS NULL",
        [id],
        |r| {
            Ok(Tag {
                id: r.get(0)?,
                name: r.get(1)?,
                color_hex: r.get(2)?,
                sort_order: r.get(3)?,
                booking_count: r.get::<_, i64>(4)? as u32,
            })
        },
    )
    .optional()?
    .ok_or_else(|| not_found(id))
}

/// Stamps the tag sets of `ids` as edited now.
fn stamp_bookings(conn: &Connection, ids: &[i64], now: i64) -> CoreResult<()> {
    for chunk in ids.chunks(500) {
        let ph = vec!["?"; chunk.len()].join(",");
        let mut args: Vec<i64> = vec![now];
        args.extend_from_slice(chunk);
        conn.execute(
            &format!("UPDATE transactions SET tags_ms = ?1 WHERE id IN ({ph})"),
            params_from_iter(args),
        )?;
    }
    Ok(())
}

/// Live bookings among `ids` (a deleted booking is never tagged).
fn live_bookings(conn: &Connection, ids: &[i64]) -> CoreResult<Vec<i64>> {
    let mut out = Vec::new();
    for chunk in ids.chunks(500) {
        let ph = vec!["?"; chunk.len()].join(",");
        let found: Vec<i64> = conn
            .prepare(&format!(
                "SELECT id FROM transactions WHERE deleted_ms IS NULL AND id IN ({ph})"
            ))?
            .query_map(params_from_iter(chunk), |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        out.extend(found);
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

/// Tags of the bookings `ids`, sorted like the tag list.
pub(crate) fn tags_for_bookings(
    conn: &Connection,
    ids: &[i64],
) -> CoreResult<HashMap<i64, Vec<BookingTag>>> {
    const SELECT: &str = "SELECT x.transaction_id, g.id, g.name, g.color_hex
               FROM transaction_tags x JOIN tags g ON g.id = x.tag_id AND g.deleted_ms IS NULL";
    const ORDER: &str = "ORDER BY g.sort_order, g.name COLLATE NOCASE, g.id";
    fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<(i64, BookingTag)> {
        Ok((
            r.get(0)?,
            BookingTag {
                id: r.get(1)?,
                name: r.get(2)?,
                color_hex: r.get(3)?,
            },
        ))
    }
    let mut out: HashMap<i64, Vec<BookingTag>> = HashMap::new();
    if ids.len() <= 500 {
        if ids.is_empty() {
            return Ok(out);
        }
        let ph = vec!["?"; ids.len()].join(",");
        let mut st = conn.prepare(&format!(
            "{SELECT} WHERE x.transaction_id IN ({ph}) {ORDER}"
        ))?;
        for r in st.query_map(params_from_iter(ids), row)? {
            let (t, tag) = r?;
            out.entry(t).or_default().push(tag);
        }
        return Ok(out);
    }
    // Large pages (dashboard, "all bookings"): one pass over the links,
    // which costs as much as there are tagged bookings, not as many chunked
    // IN lookups as there are rows.
    let wanted: HashSet<i64> = ids.iter().copied().collect();
    let mut st = conn.prepare(&format!("{SELECT} {ORDER}"))?;
    for r in st.query_map([], row)? {
        let (t, tag) = r?;
        if wanted.contains(&t) {
            out.entry(t).or_default().push(tag);
        }
    }
    Ok(out)
}

/// Moves the links of merged tags to their live survivor and drops links to
/// deleted tags. Derived state: stamps nothing.
pub(crate) fn normalize_links(conn: &Connection) -> CoreResult<()> {
    let rows: Vec<(i64, String, Option<i64>, Option<String>)> = conn
        .prepare("SELECT id, uuid, deleted_ms, merged_into FROM tags")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<Result<_, _>>()?;
    let by_uuid: HashMap<&str, (i64, bool, Option<&str>)> = rows
        .iter()
        .map(|(id, u, d, m)| (u.as_str(), (*id, d.is_none(), m.as_deref())))
        .collect();
    for (id, _, deleted, merged) in &rows {
        if deleted.is_none() {
            continue;
        }
        // follow the merge chain to a live tag (bounded against cycles)
        let mut target = None;
        let mut cur = merged.as_deref();
        let mut seen = HashSet::new();
        while let Some(u) = cur {
            if !seen.insert(u) {
                break;
            }
            match by_uuid.get(u) {
                Some(&(tid, true, _)) => {
                    target = Some(tid);
                    break;
                }
                Some(&(_, false, next)) => cur = next,
                None => break,
            }
        }
        if let Some(tid) = target {
            conn.execute(
                "INSERT OR IGNORE INTO transaction_tags(transaction_id, tag_id)
                 SELECT transaction_id, ?2 FROM transaction_tags WHERE tag_id = ?1",
                params![id, tid],
            )?;
        }
        conn.execute("DELETE FROM transaction_tags WHERE tag_id = ?1", [id])?;
    }
    Ok(())
}

/// Every tag uuid (deleted and merged ones too) -> the id of its live tag:
/// itself, or the survivor its merge chain leads to. Deleted tags without a
/// survivor are missing. Rules keep their tags as uuids (#580), so a rename or
/// a merge on any device carries over.
pub(crate) fn live_tag_ids_by_uuid(conn: &Connection) -> CoreResult<HashMap<String, i64>> {
    let rows: Vec<(i64, String, bool, Option<String>)> = conn
        .prepare("SELECT id, uuid, deleted_ms IS NULL, merged_into FROM tags")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<Result<_, _>>()?;
    let by_uuid: HashMap<&str, (i64, bool, Option<&str>)> = rows
        .iter()
        .map(|(id, u, live, m)| (u.as_str(), (*id, *live, m.as_deref())))
        .collect();
    let mut out = HashMap::new();
    for (_, uuid, _, _) in &rows {
        let mut cur = Some(uuid.as_str());
        let mut seen = HashSet::new();
        while let Some(u) = cur {
            if !seen.insert(u) {
                break;
            }
            match by_uuid.get(u) {
                Some(&(id, true, _)) => {
                    out.insert(uuid.clone(), id);
                    break;
                }
                Some(&(_, false, next)) => cur = next,
                None => break,
            }
        }
    }
    Ok(out)
}

/// The uuids of the live tags `ids` as a rule stores them (JSON array, sorted);
/// an id that is no live tag is refused.
pub(crate) fn rule_tag_uuids(conn: &Connection, ids: &[i64]) -> CoreResult<String> {
    let mut uuids = Vec::new();
    for id in ids {
        let uuid: Option<String> = conn
            .query_row(
                "SELECT uuid FROM tags WHERE id = ?1 AND deleted_ms IS NULL",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        uuids.push(uuid.ok_or_else(|| not_found(*id))?);
    }
    uuids.sort();
    uuids.dedup();
    if uuids.is_empty() {
        return Ok(String::new());
    }
    Ok(serde_json::to_string(&uuids)?)
}

/// The live tag ids of a rule's stored `tag_uuids`, sorted; tags deleted since
/// drop out, merged ones become their survivor.
pub(crate) fn rule_tag_ids(stored: &str, live: &HashMap<String, i64>) -> Vec<i64> {
    let uuids: Vec<String> = serde_json::from_str(stored).unwrap_or_default();
    let mut ids: Vec<i64> = uuids.iter().filter_map(|u| live.get(u).copied()).collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// Adds the tags of the matching rules to bookings (#580); never removes one.
/// `all`: every live booking of an active account ("Regeln anwenden", saving a
/// rule); otherwise only bookings whose tags nobody has touched yet
/// (`tags_ms = 0`), so a tag removed by hand stays removed on the next fetch.
/// `only` restricts the run to these bookings. Changed tag sets are stamped like
/// an edit, so they sync. Returns the bookings that got a tag.
pub(crate) fn tag_by_rules(
    conn: &Connection,
    set: &crate::rules::RuleSet,
    all: bool,
    only: Option<&HashSet<i64>>,
) -> CoreResult<HashSet<i64>> {
    let mut changed = HashSet::new();
    if !set.sets_tags() {
        return Ok(changed);
    }
    let scope = if all { "" } else { " AND t.tags_ms = 0" };
    let ids_json = only.map(serde_json::to_string).transpose()?;
    let ids_sql = if ids_json.is_some() {
        " AND t.id IN (SELECT value FROM json_each(?1))"
    } else {
        ""
    };
    let mut have: HashMap<i64, HashSet<i64>> = HashMap::new();
    for row in conn
        .prepare("SELECT transaction_id, tag_id FROM transaction_tags")?
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?
    {
        let (b, t) = row?;
        have.entry(b).or_default().insert(t);
    }
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM transactions t JOIN accounts a ON a.id = t.account_id
         WHERE a.archived_unix IS NULL AND a.deleted_ms IS NULL AND t.deleted_ms IS NULL{scope}{ids_sql}",
        crate::rules::RuleRow::COLUMNS
    ))?;
    let mut add = conn.prepare(
        "INSERT OR IGNORE INTO transaction_tags(transaction_id, tag_id) VALUES (?1, ?2)",
    )?;
    let mut rows = stmt.query(params_from_iter(ids_json))?;
    while let Some(row) = rows.next()? {
        let input = crate::rules::RuleRow::borrowed_input(row)?;
        let id: i64 = row.get(0)?;
        let current = have.get(&id);
        for tag in set.tags_for(&input) {
            if current.is_some_and(|c| c.contains(&tag)) {
                continue;
            }
            add.execute(params![id, tag])?;
            changed.insert(id);
        }
    }
    let ids: Vec<i64> = changed.iter().copied().collect();
    stamp_bookings(conn, &ids, now_ms())?;
    Ok(changed)
}

impl Vault {
    /// The id of the live tag named `name`, created if missing; for importers
    /// and the "new tag" field of the booking detail.
    pub(crate) fn ensure_tag(conn: &Connection, name: &str) -> CoreResult<i64> {
        let name = clean_name(name)?;
        if let Some(id) = live_by_name(conn, &name)? {
            return Ok(id);
        }
        // The name's uuid, unless a row holds it already: a renamed tag keeps
        // its first name's uuid, and a deleted one stays deleted on every
        // device. Then a fresh uuid; the merge combines same-named tags.
        let mut uuid = crate::sync_merge::tag_uuid_for_name(&name);
        let taken: bool = conn.query_row(
            "SELECT count(*) > 0 FROM tags WHERE uuid = ?1",
            [&uuid],
            |r| r.get(0),
        )?;
        if taken {
            uuid = uuid::Uuid::new_v4().to_string();
        }
        let order: i32 = conn.query_row(
            "SELECT IFNULL(MAX(sort_order) + 1, 0) FROM tags WHERE deleted_ms IS NULL",
            [],
            |r| r.get(0),
        )?;
        conn.execute(
            "INSERT INTO tags(uuid, name, sort_order, updated_ms) VALUES (?1, ?2, ?3, ?4)",
            params![uuid, name, order, now_ms()],
        )?;
        Ok(conn.last_insert_rowid())
    }
}

#[uniffi::export]
impl Vault {
    /// All live tags with their booking counts, in list order.
    pub fn list_tags(&self) -> Result<Vec<Tag>, CoreError> {
        let db = self.db()?;
        // All links per tag minus the links of deleted bookings, both from indexes:
        // looking up the booking row of every link took ~80 ms on 41k bookings with a
        // third tagged (the main thread calls this after every tag change), this ~2 ms.
        // CROSS JOIN keeps the bookings outside: few are deleted, so few links to probe.
        let mut st = db.prepare(
            "SELECT g.id, g.name, g.color_hex, g.sort_order, IFNULL(a.n, 0) - IFNULL(d.n, 0)
               FROM tags g
               LEFT JOIN (SELECT tag_id, count(*) AS n FROM transaction_tags GROUP BY tag_id) a
                      ON a.tag_id = g.id
               LEFT JOIN (SELECT x.tag_id, count(*) AS n
                            FROM transactions t CROSS JOIN transaction_tags x
                                 ON x.transaction_id = t.id
                           WHERE t.deleted_ms IS NOT NULL
                           GROUP BY x.tag_id) d
                      ON d.tag_id = g.id
              WHERE g.deleted_ms IS NULL
              ORDER BY g.sort_order, g.name COLLATE NOCASE, g.id",
        )?;
        let tags = st
            .query_map([], |r| {
                Ok(Tag {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    color_hex: r.get(2)?,
                    sort_order: r.get(3)?,
                    booking_count: r.get::<_, i64>(4)? as u32,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(tags)
    }

    /// Creates a tag; an existing live tag of the same name (any case) is
    /// returned instead, so typing a known name never makes a twin.
    pub fn create_tag(&self, name: String, color_hex: String) -> Result<Tag, CoreError> {
        let color = clean_color(&color_hex)?;
        let db = self.db()?;
        let tx = db.unchecked_transaction()?;
        let existed = live_by_name(&tx, &clean_name(&name)?)?;
        let id = Vault::ensure_tag(&tx, &name)?;
        if existed.is_none() && !color.is_empty() {
            tx.execute(
                "UPDATE tags SET color_hex = ?2 WHERE id = ?1",
                params![id, color],
            )?;
        }
        let tag = tag_row(&tx, id)?;
        tx.commit()?;
        Ok(tag)
    }

    /// Renames a tag; a name another live tag already has is refused (merge
    /// the two instead, `merge_tag_into`).
    pub fn rename_tag(&self, id: i64, name: String) -> Result<Tag, CoreError> {
        let name = clean_name(&name)?;
        let db = self.db()?;
        ensure_live(&db, id)?;
        if live_by_name(&db, &name)?.is_some_and(|other| other != id) {
            return Err(CoreError::invalid(tr(
                "Es gibt schon einen Tag mit diesem Namen.",
            )));
        }
        db.execute(
            "UPDATE tags SET name = ?2, updated_ms = ?3 WHERE id = ?1",
            params![id, name, now_ms()],
        )?;
        tag_row(&db, id)
    }

    /// Sets the colour (`#RRGGBB`, "" = default).
    pub fn set_tag_color(&self, id: i64, color_hex: String) -> Result<(), CoreError> {
        let color = clean_color(&color_hex)?;
        let db = self.db()?;
        ensure_live(&db, id)?;
        db.execute(
            "UPDATE tags SET color_hex = ?2, updated_ms = ?3 WHERE id = ?1",
            params![id, color, now_ms()],
        )?;
        Ok(())
    }

    /// Puts the tags in this order (ids not listed keep their place after them).
    pub fn reorder_tags(&self, ids: Vec<i64>) -> Result<(), CoreError> {
        let db = self.db()?;
        let tx = db.unchecked_transaction()?;
        let now = now_ms();
        for (i, id) in ids.iter().enumerate() {
            tx.execute(
                "UPDATE tags SET sort_order = ?2, updated_ms = ?3
                  WHERE id = ?1 AND deleted_ms IS NULL AND sort_order != ?2",
                params![id, i as i32, now],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Deletes a tag: it drops off every booking; the bookings stay.
    pub fn delete_tag(&self, id: i64) -> Result<(), CoreError> {
        let db = self.db()?;
        ensure_live(&db, id)?;
        let tx = db.unchecked_transaction()?;
        tx.execute(
            "UPDATE tags SET deleted_ms = ?2, updated_ms = ?2 WHERE id = ?1",
            params![id, now_ms()],
        )?;
        normalize_links(&tx)?;
        tx.commit()?;
        Ok(())
    }

    /// Merges tag `source` into `target`: every booking of `source` carries
    /// `target` afterwards, `source` is gone (on every device after sync).
    pub fn merge_tag_into(&self, source: i64, target: i64) -> Result<(), CoreError> {
        if source == target {
            return Err(CoreError::invalid(tr(
                "Ein Tag kann nicht mit sich selbst zusammengeführt werden.",
            )));
        }
        let db = self.db()?;
        ensure_live(&db, source)?;
        ensure_live(&db, target)?;
        let tx = db.unchecked_transaction()?;
        let now = now_ms();
        tx.execute(
            "UPDATE tags SET deleted_ms = ?3, updated_ms = ?3,
                    merged_into = (SELECT uuid FROM tags WHERE id = ?2)
              WHERE id = ?1",
            params![source, target, now],
        )?;
        normalize_links(&tx)?;
        tx.commit()?;
        Ok(())
    }

    /// Replaces the tags of one booking.
    pub fn set_booking_tags(&self, booking_id: i64, tag_ids: Vec<i64>) -> Result<(), CoreError> {
        let db = self.db()?;
        if live_bookings(&db, &[booking_id])?.is_empty() {
            return Err(CoreError::NotFound {
                what: format!("booking {booking_id}"),
            });
        }
        let mut wanted: Vec<i64> = tag_ids;
        wanted.sort_unstable();
        wanted.dedup();
        for id in &wanted {
            ensure_live(&db, *id)?;
        }
        let current: Vec<i64> = db
            .prepare(
                "SELECT x.tag_id FROM transaction_tags x JOIN tags g ON g.id = x.tag_id
                  WHERE x.transaction_id = ?1 AND g.deleted_ms IS NULL ORDER BY x.tag_id",
            )?
            .query_map([booking_id], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        if current == wanted {
            return Ok(());
        }
        let tx = db.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM transaction_tags WHERE transaction_id = ?1",
            [booking_id],
        )?;
        for id in &wanted {
            tx.execute(
                "INSERT INTO transaction_tags(transaction_id, tag_id) VALUES (?1, ?2)",
                params![booking_id, id],
            )?;
        }
        stamp_bookings(&tx, &[booking_id], now_ms())?;
        tx.commit()?;
        Ok(())
    }

    /// Adds tag `tag_id` to the bookings `booking_ids` (multi-selection).
    /// Returns the bookings that did not carry it yet, so the caller can undo
    /// exactly those with `remove_tag_from_bookings`.
    pub fn add_tag_to_bookings(
        &self,
        booking_ids: Vec<i64>,
        tag_id: i64,
    ) -> Result<Vec<i64>, CoreError> {
        let db = self.db()?;
        ensure_live(&db, tag_id)?;
        let tx = db.unchecked_transaction()?;
        let mut changed = Vec::new();
        for id in live_bookings(&tx, &booking_ids)? {
            let n = tx.execute(
                "INSERT OR IGNORE INTO transaction_tags(transaction_id, tag_id) VALUES (?1, ?2)",
                params![id, tag_id],
            )?;
            if n > 0 {
                changed.push(id);
            }
        }
        stamp_bookings(&tx, &changed, now_ms())?;
        tx.commit()?;
        Ok(changed)
    }

    /// Removes tag `tag_id` from the bookings `booking_ids`; returns the
    /// bookings that carried it.
    pub fn remove_tag_from_bookings(
        &self,
        booking_ids: Vec<i64>,
        tag_id: i64,
    ) -> Result<Vec<i64>, CoreError> {
        let db = self.db()?;
        let tx = db.unchecked_transaction()?;
        let mut changed = Vec::new();
        for id in live_bookings(&tx, &booking_ids)? {
            let n = tx.execute(
                "DELETE FROM transaction_tags WHERE transaction_id = ?1 AND tag_id = ?2",
                params![id, tag_id],
            )?;
            if n > 0 {
                changed.push(id);
            }
        }
        stamp_bookings(&tx, &changed, now_ms())?;
        tx.commit()?;
        Ok(changed)
    }
}
