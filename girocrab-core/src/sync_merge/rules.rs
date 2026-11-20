//! Rule merge: the newest edit replaces the whole rule, identical rules are combined.

use std::collections::{BTreeMap, HashMap};

use rusqlite::{Connection, params};

use super::{
    ACCOUNT_KEY_SEP, account_key, merge_deletion, pick_match, rule_target_key,
    rule_uuid_for_content, rules_have_tags, survivor,
};
use crate::error::CoreResult;
use crate::schema::has_table;

/// The part of a rule that is replaced as a whole; compared for tie-breaks.
#[derive(Clone, PartialEq, PartialOrd, Debug)]
struct RuleBody {
    category: String,
    field: String,
    kind: String,
    pattern: String,
    case_sensitive: bool,
    min: Option<i64>,
    max: Option<i64>,
    account: Option<String>,
    name: String,
    priority: i64,
    enabled: bool,
    /// The tags the rule adds (#580): a JSON array of tag uuids, "" = none.
    tags: String,
}

impl RuleBody {
    fn content_uuid(&self) -> String {
        rule_uuid_for_content(
            &self.field,
            &self.kind,
            &self.pattern,
            self.case_sensitive,
            self.min,
            self.max,
            self.account.as_deref(),
            &rule_target_key(&self.category, &self.tags),
        )
    }
}

#[derive(Clone, PartialEq, Debug)]
struct MRule {
    id: i64,
    uuid: String,
    body: RuleBody,
    category_id: i64,
    account_ids: Vec<i64>,
    updated_ms: i64,
    deleted_ms: Option<i64>,
    merged_into: Option<String>,
}

impl MRule {
    fn merge(&mut self, o: MRule) {
        if o.updated_ms > self.updated_ms || (o.updated_ms == self.updated_ms && o.body > self.body)
        {
            self.body = o.body;
            self.category_id = o.category_id;
            self.account_ids = o.account_ids;
            self.updated_ms = o.updated_ms;
        }
        merge_deletion(
            &mut self.deleted_ms,
            &mut self.merged_into,
            o.deleted_ms,
            o.merged_into,
        );
    }
}

type RawRule = (
    i64,
    String,
    String,
    Option<i64>,
    String,
    String,
    String,
    bool,
    Option<i64>,
    Option<i64>,
    rusqlite::types::Value,
    i64,
    bool,
    i64,
    Option<i64>,
    Option<String>,
    String,
);

fn load_rules(conn: &Connection, schema: &str) -> CoreResult<Vec<RawRule>> {
    // includes soft-deleted rows
    // a vault from before step 24 has no `rule_accounts`: its column is the set
    let has_set = has_table(conn, schema, "rule_accounts")?;
    let accounts = if has_set {
        format!(
            "COALESCE((SELECT group_concat(account_id) FROM {schema}.rule_accounts
                       WHERE rule_id = r.id), r.account_id)"
        )
    } else {
        "r.account_id".to_string()
    };
    let tags = if rules_have_tags(conn, schema)? {
        "r.tag_uuids"
    } else {
        "''"
    };
    let sql = format!(
        "SELECT r.id, r.uuid, r.name, r.category_id, r.field, r.match_kind, r.pattern,
                r.case_sensitive, r.min_amount_cents, r.max_amount_cents, {accounts},
                r.priority, r.enabled, r.updated_ms, r.deleted_ms, r.merged_into, {tags}
         FROM {schema}.rules r ORDER BY r.id"
    );
    let rows = conn
        .prepare(&sql)?
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
                r.get(10)?,
                r.get(11)?,
                r.get(12)?,
                r.get(13)?,
                r.get(14)?,
                r.get(15)?,
                r.get(16)?,
            ))
        })?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

/// Local categories: id -> (uuid, live, merged_into).
type CatInfo = HashMap<i64, (String, bool, Option<String>)>;

fn category_info(conn: &Connection) -> CoreResult<CatInfo> {
    // includes soft-deleted rows
    let rows = conn
        .prepare("SELECT id, uuid, deleted_ms IS NULL, merged_into FROM categories")?
        .query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?, r.get(3)?))))?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

fn rule_survivor<'a>(state: &'a BTreeMap<String, MRule>, uuid: &'a str) -> Option<&'a str> {
    survivor(uuid, |u| {
        state
            .get(u)
            .map(|r| (r.deleted_ms.is_none(), r.merged_into.as_deref()))
    })
}

/// Merges `other.rules`: upsert by uuid, the newer `updated_ms` replaces the
/// whole rule, deletions win, rules of a deleted category are deleted with
/// it, identical live rules are combined into the lowest uuid. `cat_map` and
/// `acc_map` translate the other vault's ids. Returns other id -> local id
/// (combined rules map to their survivor).
pub(crate) fn merge_rules(
    conn: &Connection,
    cat_map: &HashMap<i64, i64>,
    acc_map: &HashMap<i64, i64>,
    legacy: bool,
    stats: &mut crate::vault::MergeStats,
) -> CoreResult<HashMap<i64, i64>> {
    let cats = category_info(conn)?;
    let cat_id_of: HashMap<&str, i64> = cats.iter().map(|(id, c)| (c.0.as_str(), *id)).collect();
    let mut keys: HashMap<i64, String> = HashMap::new();
    let mut key_of = |conn: &Connection, id: i64| -> CoreResult<String> {
        if let Some(k) = keys.get(&id) {
            return Ok(k.clone());
        }
        let k = account_key(conn, id)?;
        keys.insert(id, k.clone());
        Ok(k)
    };
    let mut build =
        |conn: &Connection, raw: RawRule, cat: i64, acc: Vec<i64>| -> CoreResult<MRule> {
            let (
                id,
                uuid,
                name,
                _,
                field,
                kind,
                pattern,
                cs,
                min,
                max,
                _,
                prio,
                en,
                upd,
                del,
                merged,
                tags,
            ) = raw;
            let mut keys_of = acc
                .iter()
                .map(|a| key_of(conn, *a))
                .collect::<CoreResult<Vec<_>>>()?;
            keys_of.sort();
            let account = (!keys_of.is_empty()).then(|| keys_of.join(ACCOUNT_KEY_SEP));
            Ok(MRule {
                id,
                uuid,
                body: RuleBody {
                    category: cats.get(&cat).map(|c| c.0.clone()).unwrap_or_default(),
                    field,
                    kind,
                    pattern,
                    case_sensitive: cs,
                    min,
                    max,
                    account,
                    name,
                    priority: prio,
                    enabled: en,
                    tags,
                },
                category_id: cat,
                account_ids: acc,
                updated_ms: upd,
                deleted_ms: del,
                merged_into: merged,
            })
        };

    let mut local = Vec::new();
    for raw in load_rules(conn, "main")? {
        // 0 = no category: a rule that only adds tags (#580)
        let (cat, acc) = (raw.3.unwrap_or(0), crate::rules::parse_account_ids(&raw.10));
        local.push(build(conn, raw, cat, acc)?);
    }
    let mut other = Vec::new();
    for raw in load_rules(conn, "other")? {
        let cat = match raw.3 {
            None => 0,
            Some(theirs) => match cat_map.get(&theirs) {
                Some(&cat) => cat,
                None => continue,
            },
        };
        if raw.6.len() > crate::rules::MAX_PATTERN_LEN {
            continue;
        }
        let theirs = crate::rules::parse_account_ids(&raw.10);
        let acc: Vec<i64> = theirs
            .iter()
            .filter_map(|a| acc_map.get(a).copied())
            .collect();
        if acc.len() != theirs.len() {
            continue;
        }
        other.push(build(conn, raw, cat, acc)?);
    }
    let original: HashMap<String, MRule> =
        local.iter().map(|r| (r.uuid.clone(), r.clone())).collect();
    let mut state: BTreeMap<String, MRule> =
        local.iter().map(|r| (r.uuid.clone(), r.clone())).collect();

    // v7 rows: matched by content against every local rule, deleted ones too
    let mut alias: HashMap<String, String> = HashMap::new();
    if legacy {
        let mut by_content: HashMap<String, Vec<(&str, bool)>> = HashMap::new();
        for r in &local {
            by_content
                .entry(r.body.content_uuid())
                .or_default()
                .push((r.uuid.as_str(), r.deleted_ms.is_none()));
        }
        for o in other.iter().filter(|o| !original.contains_key(&o.uuid)) {
            if let Some(m) = by_content
                .get(&o.body.content_uuid())
                .and_then(|c| pick_match(c))
            {
                alias.insert(o.uuid.clone(), m.to_string());
            }
        }
    }

    let mut other_ids = Vec::new();
    for mut row in other {
        other_ids.push((row.id, row.uuid.clone()));
        row.merged_into = row.merged_into.map(|u| alias.get(&u).cloned().unwrap_or(u));
        match alias.get(&row.uuid) {
            Some(target) => {
                let t = state.get_mut(target).unwrap();
                t.merge(MRule {
                    uuid: target.clone(),
                    ..row.clone()
                });
                let ms = t.updated_ms;
                row.id = 0;
                row.deleted_ms = Some(ms.max(1));
                row.merged_into = Some(target.clone());
                state.insert(row.uuid.clone(), row);
            }
            None => match state.get_mut(&row.uuid) {
                Some(r) => r.merge(row),
                None => {
                    row.id = 0;
                    state.insert(row.uuid.clone(), row);
                }
            },
        }
    }

    // a rule follows its category's survivor, or is deleted with it
    for r in state.values_mut() {
        let Some((_, false, _)) = cats.get(&r.category_id) else {
            continue;
        };
        let s = survivor(&r.body.category, |u| {
            cat_id_of
                .get(u)
                .and_then(|id| cats.get(id))
                .map(|c| (c.1, c.2.as_deref()))
        })
        .map(str::to_string);
        match s {
            Some(s) => {
                r.category_id = cat_id_of[s.as_str()];
                r.body.category = s;
            }
            None if r.deleted_ms.is_none() => {
                let del: Option<i64> = conn.query_row(
                    // includes soft-deleted rows
                    "SELECT deleted_ms FROM categories WHERE id = ?1",
                    [r.category_id],
                    |q| q.get(0),
                )?;
                r.deleted_ms = del;
            }
            None => {}
        }
    }

    // identical live rules are combined into the lowest uuid
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for r in state.values().filter(|r| r.deleted_ms.is_none()) {
        groups
            .entry(r.body.content_uuid())
            .or_default()
            .push(r.uuid.clone());
    }
    for uuids in groups.into_values().filter(|g| g.len() > 1) {
        let keep = uuids[0].clone();
        let keep_ms = state[&keep].updated_ms;
        for u in &uuids[1..] {
            let r = state.get_mut(u).unwrap();
            r.deleted_ms = Some(r.updated_ms.max(keep_ms).max(1));
            r.merged_into = Some(keep.clone());
        }
    }

    let mut ids: HashMap<String, i64> = state.values().map(|r| (r.uuid.clone(), r.id)).collect();
    for r in state.values().filter(|r| original.get(&r.uuid) != Some(r)) {
        let b = &r.body;
        if r.id == 0 {
            conn.execute(
                "INSERT INTO rules(uuid, name, category_id, pattern) VALUES (?1, ?2, ?3, ?4)",
                params![
                    r.uuid,
                    b.name,
                    (r.category_id != 0).then_some(r.category_id),
                    b.pattern
                ],
            )?;
            ids.insert(r.uuid.clone(), conn.last_insert_rowid());
            if r.deleted_ms.is_none() {
                stats.rules_added += 1;
            }
        }
        conn.execute(
            "UPDATE rules SET name = ?2, category_id = ?3, field = ?4, match_kind = ?5,
                    pattern = ?6, case_sensitive = ?7, min_amount_cents = ?8,
                    max_amount_cents = ?9, account_id = ?10, priority = ?11, enabled = ?12,
                    updated_ms = ?13, deleted_ms = ?14, merged_into = ?15, tag_uuids = ?16
             WHERE id = ?1",
            params![
                ids[&r.uuid],
                b.name,
                (r.category_id != 0).then_some(r.category_id),
                b.field,
                b.kind,
                b.pattern,
                b.case_sensitive,
                b.min,
                b.max,
                r.account_ids.iter().min(),
                b.priority,
                b.enabled,
                r.updated_ms,
                r.deleted_ms,
                r.merged_into,
                b.tags
            ],
        )?;
        crate::rules::store_rule_accounts(conn, ids[&r.uuid], &r.account_ids)?;
    }

    // bookings follow a combined rule; a deleted rule's bookings are
    // uncategorised (as `delete_rule` does); derived, no stamps
    for r in state.values().filter(|r| r.deleted_ms.is_some()) {
        let id = ids[&r.uuid];
        match rule_survivor(&state, &r.uuid) {
            Some(s) => {
                conn.execute(
                    "UPDATE transactions SET rule_id = ?2 WHERE rule_id = ?1",
                    [id, ids[s]],
                )?;
            }
            None => {
                conn.execute(
                    "UPDATE transactions SET category_id = NULL, category_source = 'none', rule_id = NULL
                     WHERE rule_id = ?1",
                    [id],
                )?;
            }
        }
    }

    Ok(other_ids
        .iter()
        .map(|(oid, u)| {
            let u = alias.get(u).unwrap_or(u);
            let target = rule_survivor(&state, u).unwrap_or(u.as_str());
            (*oid, ids[target])
        })
        .collect())
}
