//! Category merge: newest edit per field wins, same-named live siblings are combined.

use std::collections::{BTreeMap, HashMap};

use rusqlite::{Connection, params};

use super::{
    category_uuid_for_path, fill_missing_uuids, fold, merge_deletion, pick_match, survivor,
    take_newer,
};
use crate::error::CoreResult;

#[derive(Clone, PartialEq, Debug)]
struct Cat {
    role: Option<String>,
    id: i64,
    uuid: String,
    name: String,
    parent: Option<String>,
    symbol: String,
    color: String,
    budget: Option<i64>,
    order: i64,
    name_ms: i64,
    parent_ms: i64,
    symbol_ms: i64,
    color_ms: i64,
    budget_ms: i64,
    order_ms: i64,
    deleted_ms: Option<i64>,
    merged_into: Option<String>,
}

impl Cat {
    fn merge(&mut self, o: Cat) {
        // Identity is immutable; when combining a custom entry and a built-in,
        // custom provenance wins so its name can never become translated.
        merge_category_role(&mut self.role, o.role);
        take_newer(&mut self.name, &mut self.name_ms, o.name, o.name_ms);
        take_newer(&mut self.parent, &mut self.parent_ms, o.parent, o.parent_ms);
        take_newer(&mut self.symbol, &mut self.symbol_ms, o.symbol, o.symbol_ms);
        take_newer(&mut self.color, &mut self.color_ms, o.color, o.color_ms);
        take_newer(&mut self.budget, &mut self.budget_ms, o.budget, o.budget_ms);
        take_newer(&mut self.order, &mut self.order_ms, o.order, o.order_ms);
        merge_deletion(
            &mut self.deleted_ms,
            &mut self.merged_into,
            o.deleted_ms,
            o.merged_into,
        );
    }
}

pub(super) fn merge_category_role(role: &mut Option<String>, other: Option<String>) {
    if *role != other {
        // Only the two transfer roles share semantics. All other conflicts
        // become custom. This rule is commutative, associative and idempotent,
        // so three-way device merges converge regardless of their order.
        *role = match (role.as_deref(), other.as_deref()) {
            (Some("Umbuchungen"), Some("import:transfers"))
            | (Some("import:transfers"), Some("Umbuchungen")) => Some("import:transfers".into()),
            _ => None,
        };
    }
}

fn load_categories(conn: &Connection, schema: &str) -> CoreResult<Vec<Cat>> {
    // includes soft-deleted rows
    let sql = format!(
        "SELECT c.id, c.uuid, c.name, p.uuid, c.sf_symbol, c.color_hex, c.budget_cents,
                c.sort_order, c.name_ms, c.parent_ms, c.symbol_ms, c.color_ms, c.budget_ms,
                c.order_ms, c.deleted_ms, c.merged_into, c.builtin_role
         FROM {schema}.categories c LEFT JOIN {schema}.categories p ON p.id = c.parent_id
         ORDER BY c.id"
    );
    let rows = conn
        .prepare(&sql)?
        .query_map([], |r| {
            Ok(Cat {
                role: r.get(16)?,
                id: r.get(0)?,
                uuid: r.get(1)?,
                name: r.get(2)?,
                parent: r.get(3)?,
                symbol: r.get(4)?,
                color: r.get(5)?,
                budget: r.get(6)?,
                order: r.get(7)?,
                name_ms: r.get(8)?,
                parent_ms: r.get(9)?,
                symbol_ms: r.get(10)?,
                color_ms: r.get(11)?,
                budget_ms: r.get(12)?,
                order_ms: r.get(13)?,
                deleted_ms: r.get(14)?,
                merged_into: r.get(15)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

/// uuid -> uuid of the category's current path (names from the root).
fn path_uuids(cats: &[Cat]) -> HashMap<String, String> {
    let by_uuid: HashMap<&str, &Cat> = cats.iter().map(|c| (c.uuid.as_str(), c)).collect();
    cats.iter()
        .map(|c| {
            let mut path = vec![c.name.as_str()];
            let mut cur = c.parent.as_deref();
            while let Some(p) = cur.and_then(|p| by_uuid.get(p)) {
                if path.len() > 64 {
                    break;
                }
                path.push(p.name.as_str());
                cur = p.parent.as_deref();
            }
            path.reverse();
            (c.uuid.clone(), category_uuid_for_path(&path))
        })
        .collect()
}

fn cat_survivor<'a>(state: &'a BTreeMap<String, Cat>, uuid: &'a str) -> Option<&'a str> {
    survivor(uuid, |u| {
        state
            .get(u)
            .map(|c| (c.deleted_ms.is_none(), c.merged_into.as_deref()))
    })
}

/// A parent that was combined into another moves its children to the
/// survivor; a live child of a deleted parent moves to the top level (a
/// deleted child keeps it). A cycle from crossing moves is cut at its oldest
/// move (smallest `parent_ms`, then uuid): that child goes to the top level.
/// Derived: no stamps.
fn resolve_parents(state: &mut BTreeMap<String, Cat>) {
    let all: Vec<String> = state.keys().cloned().collect();
    for u in &all {
        let c = &state[u];
        let parent = c
            .parent
            .as_deref()
            .and_then(|p| match cat_survivor(state, p) {
                Some(s) => Some(s),
                None if c.deleted_ms.is_some() => Some(p),
                None => None,
            })
            .filter(|p| *p != u.as_str())
            .map(str::to_string);
        state.get_mut(u).unwrap().parent = parent;
    }
    let live: Vec<String> = state
        .values()
        .filter(|c| c.deleted_ms.is_none())
        .map(|c| c.uuid.clone())
        .collect();
    loop {
        let mut cycle: Option<Vec<String>> = None;
        for u in &live {
            let mut seen = vec![u.clone()];
            let mut cur = state[u].parent.clone();
            while let Some(p) = cur {
                if p == *u {
                    cycle = Some(seen);
                    break;
                }
                if seen.len() > state.len() {
                    break;
                }
                cur = state.get(&p).and_then(|c| c.parent.clone());
                seen.push(p);
            }
            if cycle.is_some() {
                break;
            }
        }
        let Some(cycle) = cycle else { break };
        // the older move loses (ties: lower uuid); its stamp stays
        let oldest = cycle
            .iter()
            .min_by_key(|u| (state[*u].parent_ms, (*u).clone()))
            .unwrap()
            .clone();
        state.get_mut(&oldest).unwrap().parent = None;
    }
}

/// Live categories with the same parent and case-folded name are combined
/// into the lowest uuid. Returns whether anything was combined.
fn combine_categories(state: &mut BTreeMap<String, Cat>) -> bool {
    let mut groups: BTreeMap<(Option<String>, String), Vec<String>> = BTreeMap::new();
    for c in state.values().filter(|c| c.deleted_ms.is_none()) {
        groups
            .entry((c.parent.clone(), fold(&c.name)))
            .or_default()
            .push(c.uuid.clone());
    }
    let mut any = false;
    for uuids in groups.into_values().filter(|g| g.len() > 1) {
        // BTreeMap order: the first is the lowest uuid
        let keep = &uuids[0];
        for u in &uuids[1..] {
            let role = state[u].role.clone();
            merge_category_role(&mut state.get_mut(keep).unwrap().role, role);
        }
        let keep_ms = state[keep].name_ms;
        for u in &uuids[1..] {
            let c = state.get_mut(u).unwrap();
            c.deleted_ms = Some(c.name_ms.max(keep_ms).max(1));
            c.merged_into = Some(keep.clone());
            any = true;
        }
    }
    any
}

/// Merges `other.categories` into this vault: upsert by uuid, newest stamp per
/// field, deletions win, same-named live siblings combined. Returns other id
/// -> local id (combined rows map to their survivor; a deleted row without a
/// survivor maps to its own deleted local row).
///
/// `legacy`: the other vault had no uuids (v7); its rows were given path
/// uuids and are matched by their path against every local row, deleted ones
/// included, so they cannot revive anything.
pub(crate) fn merge_categories(
    conn: &Connection,
    legacy: bool,
    stats: &mut crate::vault::MergeStats,
) -> CoreResult<HashMap<i64, i64>> {
    fill_missing_uuids(conn)?;
    let local = load_categories(conn, "main")?;
    let other = load_categories(conn, "other")?;
    let original: HashMap<String, Cat> =
        local.iter().map(|c| (c.uuid.clone(), c.clone())).collect();
    let mut state: BTreeMap<String, Cat> =
        local.iter().map(|c| (c.uuid.clone(), c.clone())).collect();

    // v7 rows: other uuid -> the local uuid they stand for
    let mut alias: HashMap<String, String> = HashMap::new();
    if legacy {
        let mut by_path: HashMap<String, Vec<(&str, bool)>> = HashMap::new();
        for (u, p) in path_uuids(&local) {
            let live = original[&u].deleted_ms.is_none();
            let u = original.get_key_value(&u).unwrap().0.as_str();
            by_path.entry(p).or_default().push((u, live));
        }
        for (u, p) in path_uuids(&other) {
            if original.contains_key(&u) {
                continue;
            }
            if let Some(m) = by_path.get(&p).and_then(|c| pick_match(c)) {
                alias.insert(u, m.to_string());
            }
        }
    }
    let local_uuid = |u: Option<String>| u.map(|u| alias.get(&u).cloned().unwrap_or(u));

    for o in &other {
        let mut row = o.clone();
        row.parent = local_uuid(row.parent);
        row.merged_into = local_uuid(row.merged_into);
        match alias.get(&o.uuid) {
            Some(target) => {
                // merged into its match; the row itself stays as a tombstone
                let t = state.get_mut(target).unwrap();
                t.merge(Cat {
                    uuid: target.clone(),
                    ..row.clone()
                });
                let ms = t.name_ms;
                row.id = 0;
                row.deleted_ms = Some(ms.max(1));
                row.merged_into = Some(target.clone());
                state.insert(row.uuid.clone(), row);
            }
            None => match state.get_mut(&o.uuid) {
                Some(c) => c.merge(row),
                None => {
                    row.id = 0;
                    state.insert(row.uuid.clone(), row);
                }
            },
        }
    }
    loop {
        resolve_parents(&mut state);
        if !combine_categories(&mut state) {
            break;
        }
    }

    // write: new rows first (parked as deleted = -1, outside the unique name
    // index), then every changed row, then unpark the live ones at once
    let mut ids: HashMap<String, i64> = state.values().map(|c| (c.uuid.clone(), c.id)).collect();
    let changed: Vec<&Cat> = state
        .values()
        .filter(|c| original.get(&c.uuid) != Some(c))
        .collect();
    for c in &changed {
        if c.id == 0 {
            conn.execute(
                "INSERT INTO categories(uuid, name, deleted_ms) VALUES (?1, ?2, -1)",
                params![c.uuid, c.name],
            )?;
            ids.insert(c.uuid.clone(), conn.last_insert_rowid());
            if c.deleted_ms.is_none() {
                stats.categories_added += 1;
            }
        } else {
            conn.execute(
                "UPDATE categories SET deleted_ms = -1 WHERE id = ?1",
                [c.id],
            )?;
        }
    }
    for c in &changed {
        let parent = c.parent.as_ref().and_then(|p| ids.get(p));
        conn.execute(
            "UPDATE categories SET name = ?2, parent_id = ?3, sf_symbol = ?4, color_hex = ?5,
                    budget_cents = ?6, sort_order = ?7, name_ms = ?8, parent_ms = ?9,
                    symbol_ms = ?10, color_ms = ?11, budget_ms = ?12, order_ms = ?13,
                    deleted_ms = IFNULL(?14, -1), merged_into = ?15, builtin_role = ?16
             WHERE id = ?1",
            params![
                ids[&c.uuid],
                c.name,
                parent,
                c.symbol,
                c.color,
                c.budget,
                c.order,
                c.name_ms,
                c.parent_ms,
                c.symbol_ms,
                c.color_ms,
                c.budget_ms,
                c.order_ms,
                c.deleted_ms,
                c.merged_into,
                c.role
            ],
        )?;
    }
    conn.execute(
        "UPDATE categories SET deleted_ms = NULL WHERE deleted_ms = -1",
        [],
    )?;

    // whatever points at a deleted category follows its survivor or becomes
    // uncategorised (as a deletion does); derived, no stamps
    for c in state.values().filter(|c| c.deleted_ms.is_some()) {
        let id = ids[&c.uuid];
        match cat_survivor(&state, &c.uuid) {
            Some(s) => {
                let s = ids[s];
                conn.execute(
                    "UPDATE transactions SET category_id = ?2 WHERE category_id = ?1",
                    [id, s],
                )?;
                conn.execute(
                    "UPDATE transaction_splits SET category_id = ?2 WHERE category_id = ?1",
                    [id, s],
                )?;
                conn.execute(
                    "UPDATE rules SET category_id = ?2 WHERE category_id = ?1",
                    [id, s],
                )?;
            }
            None => {
                conn.execute(
                    "UPDATE transactions SET category_id = NULL, rule_id = NULL, category_source = 'none'
                     WHERE category_id = ?1",
                    [id],
                )?;
                conn.execute(
                    "UPDATE transaction_splits SET category_id = NULL WHERE category_id = ?1",
                    [id],
                )?;
            }
        }
    }

    Ok(other
        .iter()
        .map(|o| {
            let u = alias.get(&o.uuid).unwrap_or(&o.uuid);
            let target = cat_survivor(&state, u).unwrap_or(u.as_str());
            (o.id, ids[target])
        })
        .collect())
}
