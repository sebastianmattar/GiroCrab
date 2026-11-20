//! Vault merge for tags (iCloud peer snapshot, #228).
//!
//! Tags are matched by `uuid`. Per tag the newest edit (`updated_ms`) wins for
//! name, colour and order, a deletion stays. Two live tags with the same name
//! but different uuids (created on two devices after a rename or deletion)
//! become one: the smaller uuid survives, the other turns into a tombstone
//! merged into it. Nothing here is stamped with the current time, so both
//! merge orders reach the same state. The tag sets of the bookings follow in
//! `sync_merge::merge_bookings` ([`TagIds`]).

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};

use rusqlite::{Connection, params};

use super::{fold, normalize_links};
use crate::error::CoreResult;
use crate::schema::has_table;

#[derive(Clone, PartialEq)]
struct Row {
    name: String,
    color_hex: String,
    sort_order: i32,
    updated_ms: i64,
    deleted_ms: Option<i64>,
    merged_into: Option<String>,
}

impl Row {
    /// Joins `o` into `self`; the same result in either order.
    fn merge(&mut self, o: &Row) {
        let key = |r: &Row| {
            (
                r.updated_ms,
                r.name.clone(),
                r.color_hex.clone(),
                r.sort_order,
            )
        };
        if key(o) > key(self) {
            self.name = o.name.clone();
            self.color_hex = o.color_hex.clone();
            self.sort_order = o.sort_order;
            self.updated_ms = o.updated_ms;
        }
        self.deleted_ms = self.deleted_ms.max(o.deleted_ms);
        self.merged_into = self.merged_into.clone().max(o.merged_into.clone());
    }
}

fn load(conn: &Connection, schema: &str) -> CoreResult<BTreeMap<String, Row>> {
    if !has_table(conn, schema, "tags")? {
        return Ok(BTreeMap::new());
    }
    let rows = conn
        // includes tombstones: deletions travel
        .prepare(&format!(
            "SELECT uuid, name, color_hex, sort_order, updated_ms, deleted_ms, merged_into
               FROM {schema}.tags"
        ))?
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                Row {
                    name: r.get(1)?,
                    color_hex: r.get(2)?,
                    sort_order: r.get(3)?,
                    updated_ms: r.get(4)?,
                    deleted_ms: r.get(5)?,
                    merged_into: r.get(6)?,
                },
            ))
        })?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

/// A peer's tag with the local rules applied: name collapsed and cut to the
/// length limit (a row without a usable name is dropped), colour reset when it
/// is not `#RRGGBB`.
fn sanitize_peer(mut r: Row) -> Option<Row> {
    let name = r.name.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        return None;
    }
    r.name = name.chars().take(super::MAX_NAME).collect();
    r.color_hex = super::clean_color(&r.color_hex).unwrap_or_default();
    Some(r)
}

/// Merges `other.tags` into `main.tags`, then moves booking links off merged
/// and deleted tags.
pub(crate) fn merge_attached(conn: &Connection) -> CoreResult<()> {
    let local = load(conn, "main")?;
    let mut state = local.clone();
    for (uuid, o) in load(conn, "other")? {
        // A peer's rows get the checks local edits and the local API enforce:
        // a name that is empty or absurdly long, or a colour that is no #RRGGBB,
        // must not arrive through a sync folder.
        let Some(o) = sanitize_peer(o) else { continue };
        state.entry(uuid).and_modify(|r| r.merge(&o)).or_insert(o);
    }
    // same name, different uuid: the smaller uuid survives (BTreeMap order)
    let mut survivors: HashMap<String, (String, i64)> = HashMap::new();
    for (uuid, r) in &mut state {
        if r.deleted_ms.is_some() {
            continue;
        }
        match survivors.entry(fold(&r.name)) {
            Entry::Occupied(e) => {
                let (survivor, survivor_ms) = e.get();
                r.deleted_ms = Some(r.updated_ms.max(*survivor_ms));
                r.merged_into = Some(survivor.clone());
            }
            Entry::Vacant(e) => {
                e.insert((uuid.clone(), r.updated_ms));
            }
        }
    }
    for (uuid, r) in &state {
        match local.get(uuid) {
            Some(old) if old == r => {}
            Some(_) => {
                conn.execute(
                    "UPDATE tags SET name = ?2, color_hex = ?3, sort_order = ?4, updated_ms = ?5,
                            deleted_ms = ?6, merged_into = ?7
                      WHERE uuid = ?1",
                    params![
                        uuid,
                        r.name,
                        r.color_hex,
                        r.sort_order,
                        r.updated_ms,
                        r.deleted_ms,
                        r.merged_into
                    ],
                )?;
            }
            None => {
                conn.execute(
                    "INSERT INTO tags(uuid, name, color_hex, sort_order, updated_ms, deleted_ms, merged_into)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        uuid,
                        r.name,
                        r.color_hex,
                        r.sort_order,
                        r.updated_ms,
                        r.deleted_ms,
                        r.merged_into
                    ],
                )?;
            }
        }
    }
    normalize_links(conn)
}

/// Tag ids of both sides as uuids of live local tags, after [`merge_attached`].
pub(crate) struct TagIds {
    /// live local tag: uuid -> id
    id_of: HashMap<String, i64>,
    /// other tag id -> its live local survivor's uuid
    other: HashMap<i64, String>,
    /// local tag id -> uuid (live only)
    local: HashMap<i64, String>,
    other_has_tags: bool,
}

/// Sorted tag uuids of one booking.
pub(crate) type TagSet = Vec<String>;

impl TagIds {
    pub(crate) fn load(conn: &Connection) -> CoreResult<Self> {
        let main: Vec<(i64, String, Option<i64>, Option<String>)> = conn
            .prepare("SELECT id, uuid, deleted_ms, merged_into FROM main.tags")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<Result<_, _>>()?;
        let by_uuid: HashMap<&str, (bool, Option<&str>)> = main
            .iter()
            .map(|(_, u, d, m)| (u.as_str(), (d.is_none(), m.as_deref())))
            .collect();
        // a uuid's live survivor (bounded against merge cycles)
        let live = |uuid: &str| -> Option<String> {
            let mut cur = uuid;
            for _ in 0..32 {
                match by_uuid.get(cur)? {
                    (true, _) => return Some(cur.to_string()),
                    (false, next) => cur = (*next)?,
                }
            }
            None
        };
        let local: HashMap<i64, String> = main
            .iter()
            .filter(|(_, _, d, _)| d.is_none())
            .map(|(id, u, _, _)| (*id, u.clone()))
            .collect();
        let id_of = local.iter().map(|(id, u)| (u.clone(), *id)).collect();
        let other_has_tags =
            has_table(conn, "other", "tags")? && has_table(conn, "other", "transaction_tags")?;
        let mut other = HashMap::new();
        if other_has_tags {
            let rows: Vec<(i64, String)> = conn
                .prepare("SELECT id, uuid FROM other.tags")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_, _>>()?;
            for (id, uuid) in rows {
                if let Some(u) = live(&uuid) {
                    other.insert(id, u);
                }
            }
        }
        Ok(Self {
            id_of,
            other,
            local,
            other_has_tags,
        })
    }

    /// booking id -> its tag set, for `schema` ("main" or "other").
    pub(crate) fn sets(&self, conn: &Connection, schema: &str) -> CoreResult<HashMap<i64, TagSet>> {
        let map = if schema == "main" {
            &self.local
        } else {
            if !self.other_has_tags {
                return Ok(HashMap::new());
            }
            &self.other
        };
        let rows: Vec<(i64, i64)> = conn
            .prepare(&format!(
                "SELECT transaction_id, tag_id FROM {schema}.transaction_tags"
            ))?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let mut out: HashMap<i64, TagSet> = HashMap::new();
        for (t, tag) in rows {
            if let Some(u) = map.get(&tag) {
                out.entry(t).or_default().push(u.clone());
            }
        }
        for set in out.values_mut() {
            set.sort();
            set.dedup();
        }
        Ok(out)
    }

    /// Replaces the tags of local booking `id` by `set`.
    pub(crate) fn write(&self, conn: &Connection, id: i64, set: &TagSet) -> CoreResult<()> {
        conn.execute(
            "DELETE FROM transaction_tags WHERE transaction_id = ?1",
            [id],
        )?;
        for uuid in set {
            if let Some(tag) = self.id_of.get(uuid) {
                conn.execute(
                    "INSERT OR IGNORE INTO transaction_tags(transaction_id, tag_id) VALUES (?1, ?2)",
                    params![id, tag],
                )?;
            }
        }
        Ok(())
    }
}
