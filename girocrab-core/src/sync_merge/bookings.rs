//! Booking merge: per-field newest edit wins, splits and tags included.

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, params};

use crate::error::CoreResult;

/// One split line: (category uuid, amount, note).
type SplitLine = (Option<String>, i64, String);

/// The synced fields of a booking, categories as uuids of live local
/// categories. `cat` is the manual category; a rule result counts as none.
#[derive(Clone, PartialEq)]
struct BookingFields {
    cat: Option<String>,
    category_ms: i64,
    note: String,
    note_ms: i64,
    checked: bool,
    checked_ms: i64,
    text: (String, String),
    text_ms: i64,
    splits: Vec<SplitLine>,
    splits_ms: i64,
    /// tag uuids, sorted (#228)
    tags: crate::tags::merge::TagSet,
    tags_ms: i64,
    deleted_ms: Option<i64>,
}

/// Newest stamp wins; equal stamps with different values: the larger `rank`.
fn take_ranked<T: Clone, K: Ord>(
    val: &mut T,
    ms: &mut i64,
    other: &T,
    other_ms: i64,
    rank: impl Fn(&T) -> K,
) {
    if other_ms > *ms || (other_ms == *ms && rank(other) > rank(val)) {
        *val = other.clone();
        *ms = other_ms;
    }
}

impl BookingFields {
    /// Joins `o` into `self`; the same result in either order.
    fn merge(&mut self, o: &BookingFields) {
        // manual beats none; two manual categories: the lower uuid
        take_ranked(
            &mut self.cat,
            &mut self.category_ms,
            &o.cat,
            o.category_ms,
            |c| c.clone().map(std::cmp::Reverse),
        );
        // the longer note, ties by value
        take_ranked(&mut self.note, &mut self.note_ms, &o.note, o.note_ms, |n| {
            (n.chars().count(), n.clone())
        });
        // checked beats unchecked
        take_ranked(
            &mut self.checked,
            &mut self.checked_ms,
            &o.checked,
            o.checked_ms,
            |c| *c,
        );
        // clean text beats text with U+FFFD or mojibake (unstamped text after
        // the migration: one device may hold the healed copy), then by value
        take_ranked(&mut self.text, &mut self.text_ms, &o.text, o.text_ms, |t| {
            (
                crate::text::is_clean(&t.0) && crate::text::is_clean(&t.1),
                t.clone(),
            )
        });
        take_ranked(
            &mut self.splits,
            &mut self.splits_ms,
            &o.splits,
            o.splits_ms,
            Clone::clone,
        );
        // the tag set as a whole; equal stamps: the larger set by value
        take_ranked(
            &mut self.tags,
            &mut self.tags_ms,
            &o.tags,
            o.tags_ms,
            Clone::clone,
        );
        self.deleted_ms = self.deleted_ms.max(o.deleted_ms);
    }
}

/// transaction id -> its split lines (sorted), categories as uuids.
fn load_splits(
    conn: &Connection,
    schema: &str,
    uuid_of: impl Fn(Option<i64>) -> Option<String>,
) -> CoreResult<HashMap<i64, Vec<SplitLine>>> {
    let mut out: HashMap<i64, Vec<SplitLine>> = HashMap::new();
    let rows: Vec<(i64, Option<i64>, i64, String)> = conn
        .prepare(&format!(
            "SELECT transaction_id, category_id, amount_cents, note FROM {schema}.transaction_splits"
        ))?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<Result<_, _>>()?;
    for (t, c, amount, note) in rows {
        out.entry(t).or_default().push((uuid_of(c), amount, note));
    }
    for lines in out.values_mut() {
        lines.sort();
    }
    Ok(out)
}

/// One row of `other.transactions`.
struct OtherBooking {
    id: i64,
    account_id: i64,
    external_id: String,
    import_fp: String,
    /// `import_fp`, or one computed from the booking (see `match_fingerprint`)
    match_fp: String,
    /// booking_date .. salary_flag, copied as they are into a new row
    rest: Vec<rusqlite::types::Value>,
    /// end_to_end_id, mandate_id, creditor_id, item_refs
    sepa_refs: Vec<rusqlite::types::Value>,
    category_id: Option<i64>,
    category_source: String,
    rule_id: Option<i64>,
    fields: BookingFields,
}

// includes soft-deleted rows: deleted bookings travel as tombstones
const OTHER_BOOKINGS: &str = "SELECT id, account_id, external_id, import_fp,
        booking_date, value_date, amount_cents, currency, counterparty_iban, booking_text,
        imported_unix, purpose_code, gvc, salary_flag,
        category_id, category_source, rule_id, category_ms, note, note_ms, checked, checked_ms,
        counterparty, purpose, text_ms, splits_ms, deleted_ms,
        end_to_end_id, mandate_id, creditor_id, item_refs, tags_ms
     FROM other.transactions ORDER BY account_id, external_id";

/// The id both sides keep for one booking known under two ids: the bank's
/// (not a MoneyMoney `mm-` id), else the smaller. A card hold „Abrufweg
/// umstellen“ dropped keeps its freed id (`switch-stale-<id>:pending-hold-…`):
/// taking the hold's own id back would leave it deleted, and the bank's open
/// hold, reported again after switching back, would stay deleted (#1134).
fn canonical_external_id<'a>(a: &'a str, b: &'a str) -> &'a str {
    let freed_hold = |s: &str| s.starts_with("switch-stale-") && s.contains(":pending-hold-");
    match (freed_hold(a), freed_hold(b)) {
        (true, false) => return a,
        (false, true) => return b,
        _ => {}
    }
    match (a.starts_with("mm-"), b.starts_with("mm-")) {
        (true, false) => b,
        (false, true) => a,
        _ => a.min(b),
    }
}

/// A card hold of the bank in any of its forms: open or settled
/// (`pending-hold-…`), dropped by „Abrufweg umstellen“
/// (`switch-stale-<id>:pending-hold-…`) or marked by an older switch
/// (`mm-switch-pending-hold-…`). The fingerprint pairs card holds only with
/// card holds: a hold (often deleted) has the date, amount and text of its
/// payment, and pairing it with the posted booking would delete the booking
/// (#1134). Transfer bookings of the app (`pending-<e2e>`, marked
/// `mm-switch-pending-<e2e>`) pair as before, both forms of one transfer alike.
fn is_hold_id(ext: &str) -> bool {
    ext.starts_with("pending-hold-")
        || ext.starts_with("mm-switch-pending-hold-")
        || (ext.starts_with("switch-stale-") && ext.contains(":pending-hold-"))
}

/// Merges `other.transactions` and their splits: bookings are matched by
/// account + external id (else import fingerprint), new ones are inserted
/// with their stamps and deletion, existing ones take the newest edit per
/// field. A won manual clear makes the booking uncategorised and lets the
/// rules refill it. Afterwards nothing points at a deleted category. Never
/// stamps anything itself.
/// The fingerprint a booking is paired by when the two sides know it under
/// different ids: the stored one (MoneyMoney import, „Abrufweg umstellen“),
/// else computed the same way from the booking. So a booking fetched over
/// the bank's API on one device pairs with the same booking another device
/// fetched via FinTS after switching (#537).
fn match_fingerprint(
    stored: String,
    date: &str,
    cents: i64,
    iban: &str,
    name: &str,
    purpose: &str,
) -> String {
    if !stored.is_empty() {
        return stored;
    }
    crate::importers::moneymoney_import::fingerprint(date, cents, iban, name, purpose)
}

pub(crate) fn merge_bookings(
    conn: &Connection,
    acc_map: &HashMap<i64, i64>,
    cat_map: &HashMap<i64, i64>,
    rule_map: &HashMap<i64, i64>,
    stats: &mut crate::vault::MergeStats,
) -> CoreResult<()> {
    // live local categories: id <-> uuid
    let live_cats: Vec<(i64, String)> = conn
        .prepare("SELECT id, uuid FROM categories WHERE deleted_ms IS NULL AND uuid IS NOT NULL")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let uuid_of_id: HashMap<i64, String> = live_cats.iter().cloned().collect();
    let id_of_uuid: HashMap<String, i64> = live_cats.into_iter().map(|(i, u)| (u, i)).collect();
    let local_uuid = |c: Option<i64>| c.and_then(|c| uuid_of_id.get(&c).cloned());
    let other_uuid = |c: Option<i64>| c.and_then(|c| local_uuid(cat_map.get(&c).copied()));
    let cat_id = |u: &Option<String>| u.as_ref().and_then(|u| id_of_uuid.get(u).copied());

    // tags: merged before the bookings, as uuids of live local tags
    let tag_ids = crate::tags::merge::TagIds::load(conn)?;
    let mut local_tags = tag_ids.sets(conn, "main")?;
    let mut other_tags = tag_ids.sets(conn, "other")?;

    // local bookings, deleted ones included
    let mut local_splits = load_splits(conn, "main", local_uuid)?;
    let mut by_key: HashMap<(i64, String), i64> = HashMap::new();
    // (account, import_fp) -> (external_id, id) of local fingerprinted rows
    let mut fp_rows: Vec<((i64, String, bool), (String, i64))> = Vec::new();
    let mut local: HashMap<i64, BookingFields> = HashMap::new();
    {
        let mut st = conn.prepare(
            // includes soft-deleted rows
            "SELECT id, account_id, external_id, import_fp, category_id, category_source,
                    category_ms, note, note_ms, checked, checked_ms, counterparty, purpose,
                    text_ms, splits_ms, deleted_ms, booking_date, amount_cents, counterparty_iban,
                    tags_ms
             FROM main.transactions ORDER BY id",
        )?;
        let mut rows = st.query([])?;
        while let Some(r) = rows.next()? {
            let id: i64 = r.get(0)?;
            let acc: i64 = r.get(1)?;
            let ext: String = r.get(2)?;
            by_key.insert((acc, ext.clone()), id);
            let fp = match_fingerprint(
                r.get(3)?,
                &r.get::<_, String>(16)?,
                r.get(17)?,
                &r.get::<_, String>(18)?,
                &r.get::<_, String>(11)?,
                &r.get::<_, String>(12)?,
            );
            fp_rows.push(((acc, fp, is_hold_id(&ext)), (ext, id)));
            let source: String = r.get(5)?;
            let manual = if source == "manual" { r.get(4)? } else { None };
            local.insert(
                id,
                BookingFields {
                    cat: local_uuid(manual),
                    category_ms: r.get(6)?,
                    note: r.get(7)?,
                    note_ms: r.get(8)?,
                    checked: r.get(9)?,
                    checked_ms: r.get(10)?,
                    text: (r.get(11)?, r.get(12)?),
                    text_ms: r.get(13)?,
                    splits: local_splits.remove(&id).unwrap_or_default(),
                    splits_ms: r.get(14)?,
                    tags: local_tags.remove(&id).unwrap_or_default(),
                    tags_ms: r.get(19)?,
                    deleted_ms: r.get(15)?,
                },
            );
        }
    }

    let mut other_splits = load_splits(conn, "other", other_uuid)?;
    let others: Vec<OtherBooking> = conn
        .prepare(OTHER_BOOKINGS)?
        .query_map([], |r| {
            let id: i64 = r.get(0)?;
            let source: String = r.get(15)?;
            let cat: Option<i64> = r.get(14)?;
            Ok(OtherBooking {
                id,
                account_id: r.get(1)?,
                external_id: r.get(2)?,
                import_fp: r.get(3)?,
                match_fp: match_fingerprint(
                    r.get(3)?,
                    &r.get::<_, String>(4)?,
                    r.get(6)?,
                    &r.get::<_, String>(8)?,
                    &r.get::<_, String>(22)?,
                    &r.get::<_, String>(23)?,
                ),
                rest: (4..14).map(|i| r.get(i)).collect::<Result<_, _>>()?,
                sepa_refs: (27..31).map(|i| r.get(i)).collect::<Result<_, _>>()?,
                category_id: cat,
                rule_id: r.get(16)?,
                fields: BookingFields {
                    cat: if source == "manual" {
                        other_uuid(cat)
                    } else {
                        None
                    },
                    category_ms: r.get(17)?,
                    note: r.get(18)?,
                    note_ms: r.get(19)?,
                    checked: r.get(20)?,
                    checked_ms: r.get(21)?,
                    text: (r.get(22)?, r.get(23)?),
                    text_ms: r.get(24)?,
                    splits: Vec::new(),
                    splits_ms: r.get(25)?,
                    tags: Vec::new(),
                    tags_ms: r.get(31)?,
                    deleted_ms: r.get(26)?,
                },
                category_source: source,
            })
        })?
        .collect::<Result<_, _>>()?;

    // The fingerprint stands in for the external id where the two sides gave
    // one booking different ids (MoneyMoney import here, bank fetch there):
    // only local rows whose key the other side lacks take part. It is a
    // multiset (two identical coffees share one), so the unmatched rows of a
    // fingerprint are paired in external-id order, the i-th local with the
    // i-th incoming, each used once; leftovers are inserted. The pairing
    // ignores deletions (the pair stays deleted if either side deleted it):
    // pairing by status first would let the merge order decide which rows
    // pair, because a merged pair's status changes between the two merges.
    // Both sides then keep one id for the pair (`canonical_external_id`), so
    // the next merge matches it by key. Holds pair only with holds (`is_hold_id`).
    let other_keys: HashSet<(i64, &str)> = others
        .iter()
        .filter_map(|o| Some((*acc_map.get(&o.account_id)?, o.external_id.as_str())))
        .collect();
    let mut by_fp: HashMap<(i64, String, bool), Vec<(String, i64)>> = HashMap::new();
    for (k, (ext, id)) in fp_rows {
        if !other_keys.contains(&(k.0, ext.as_str())) {
            by_fp.entry(k).or_default().push((ext, id));
        }
    }
    drop(other_keys);
    for bucket in by_fp.values_mut() {
        bucket.sort();
    }

    // A booking deleted together with its account (its `deleted_ms` is the
    // account's) follows the merged account: deleted with it, back when the
    // account was added again after that deletion. Only individual deletions
    // join as such, so an older cascade tombstone on the other side never
    // deletes the bookings of a revived account again.
    let deletions = |schema: &str| -> CoreResult<HashMap<i64, Option<i64>>> {
        Ok(conn
            // includes soft-deleted rows
            .prepare(&format!("SELECT id, deleted_ms FROM {schema}.accounts"))?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?)
    };
    let local_acc_deleted = deletions("main")?;
    let other_acc_deleted = deletions("other")?;
    let individual = |d: Option<i64>, account: Option<i64>| d.filter(|_| d != account);

    let mut cleared: HashSet<i64> = HashSet::new();
    for mut o in others {
        let Some(&acc) = acc_map.get(&o.account_id) else {
            continue;
        };
        let acc_deleted = local_acc_deleted.get(&acc).copied().flatten();
        o.fields.deleted_ms = individual(
            o.fields.deleted_ms,
            other_acc_deleted.get(&o.account_id).copied().flatten(),
        );
        o.fields.splits = other_splits.remove(&o.id).unwrap_or_default();
        o.fields.tags = other_tags.remove(&o.id).unwrap_or_default();
        let existing = match by_key.get(&(acc, o.external_id.clone())) {
            Some(&id) => Some((id, false)),
            None => match by_fp.get_mut(&(acc, o.match_fp.clone(), is_hold_id(&o.external_id))) {
                Some(bucket) if !bucket.is_empty() => {
                    let (ext, id) = bucket.remove(0);
                    let key = canonical_external_id(&ext, &o.external_id).to_string();
                    let renamed = key != ext;
                    if renamed {
                        conn.execute(
                            "UPDATE transactions SET external_id = ?2 WHERE id = ?1",
                            params![id, key],
                        )?;
                        by_key.remove(&(acc, ext));
                        by_key.insert((acc, key), id);
                    }
                    Some((id, renamed))
                }
                _ => None,
            },
        };
        match existing {
            Some((id, renamed)) => {
                let old = local[&id].clone();
                let mut m = old.clone();
                m.deleted_ms = individual(m.deleted_ms, acc_deleted);
                m.merge(&o.fields);
                m.deleted_ms = m.deleted_ms.or(acc_deleted);
                if m == old {
                    if renamed {
                        stats.bookings_updated += 1;
                    }
                    continue;
                }
                write_booking(conn, id, &old, &m, &cat_id, &tag_ids, &mut cleared)?;
                local.insert(id, m);
                stats.bookings_updated += 1;
            }
            None => {
                o.fields.deleted_ms = o.fields.deleted_ms.or(acc_deleted);
                let f = &o.fields;
                let (cat, source, rule) = match (f.cat.is_some(), o.category_source.as_str()) {
                    (true, _) => (cat_id(&f.cat), "manual", None),
                    (false, "rule") => match (
                        o.category_id.and_then(|c| cat_map.get(&c)),
                        o.rule_id.and_then(|r| rule_map.get(&r)),
                    ) {
                        (Some(&c), Some(&r)) => (Some(c), "rule", Some(r)),
                        _ => (None, "none", None),
                    },
                    _ => (None, "none", None),
                };
                let mut p: Vec<rusqlite::types::Value> = vec![
                    acc.into(),
                    o.external_id.clone().into(),
                    o.import_fp.clone().into(),
                ];
                p.extend(o.rest.iter().cloned());
                p.extend([
                    cat.into(),
                    source.to_string().into(),
                    rule.into(),
                    f.category_ms.into(),
                    f.note.clone().into(),
                    f.note_ms.into(),
                    f.checked.into(),
                    f.checked_ms.into(),
                    f.text.0.clone().into(),
                    f.text.1.clone().into(),
                    f.text_ms.into(),
                    f.splits_ms.into(),
                    f.deleted_ms.into(),
                ]);
                p.extend(o.sepa_refs.iter().cloned());
                p.push(f.tags_ms.into());
                conn.execute(
                    "INSERT INTO transactions(account_id, external_id, import_fp,
                        booking_date, value_date, amount_cents, currency, counterparty_iban, booking_text,
                        imported_unix, purpose_code, gvc, salary_flag,
                        category_id, category_source, rule_id, category_ms, note, note_ms, checked, checked_ms,
                        counterparty, purpose, text_ms, splits_ms, deleted_ms,
                        end_to_end_id, mandate_id, creditor_id, item_refs, tags_ms)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29,?30,?31)",
                    rusqlite::params_from_iter(p),
                )?;
                let id = conn.last_insert_rowid();
                insert_splits(conn, id, &f.splits, &cat_id)?;
                if !f.tags.is_empty() {
                    tag_ids.write(conn, id, &f.tags)?;
                }
                by_key.insert((acc, o.external_id), id);
                local.insert(id, o.fields);
                if local[&id].deleted_ms.is_none() {
                    stats.bookings_added += 1;
                }
            }
        }
    }

    // nothing points at a deleted category (derived, no stamps)
    conn.execute(
        "UPDATE transactions SET category_id = NULL, rule_id = NULL, category_source = 'none'
         WHERE category_id IN (SELECT id FROM categories WHERE deleted_ms IS NOT NULL)",
        [],
    )?;
    conn.execute(
        "UPDATE transaction_splits SET category_id = NULL
         WHERE category_id IN (SELECT id FROM categories WHERE deleted_ms IS NOT NULL)",
        [],
    )?;
    // a won manual clear: the rules refill those bookings, as on the sender
    if !cleared.is_empty() {
        crate::vault::Vault::apply_rules_to(conn, &cleared)?;
    }
    Ok(())
}

fn insert_splits(
    conn: &Connection,
    id: i64,
    lines: &[SplitLine],
    cat_id: &impl Fn(&Option<String>) -> Option<i64>,
) -> CoreResult<()> {
    for (cat, amount, note) in lines {
        conn.execute(
            "INSERT INTO transaction_splits(transaction_id, category_id, amount_cents, note)
             VALUES (?1, ?2, ?3, ?4)",
            params![id, cat_id(cat), amount, note],
        )?;
    }
    Ok(())
}

/// Writes the merged fields `m` over the local booking `id` (was `old`).
fn write_booking(
    conn: &Connection,
    id: i64,
    old: &BookingFields,
    m: &BookingFields,
    cat_id: &impl Fn(&Option<String>) -> Option<i64>,
    tag_ids: &crate::tags::merge::TagIds,
    cleared: &mut HashSet<i64>,
) -> CoreResult<()> {
    if m.cat != old.cat {
        if m.cat.is_some() {
            conn.execute(
                "UPDATE transactions SET category_id = ?2, category_source = 'manual', rule_id = NULL
                 WHERE id = ?1",
                params![id, cat_id(&m.cat)],
            )?;
        } else {
            conn.execute(
                "UPDATE transactions SET category_id = NULL, category_source = 'none', rule_id = NULL
                 WHERE id = ?1",
                [id],
            )?;
            cleared.insert(id);
        }
    }
    if m.splits != old.splits {
        conn.execute(
            "DELETE FROM transaction_splits WHERE transaction_id = ?1",
            [id],
        )?;
        insert_splits(conn, id, &m.splits, cat_id)?;
    }
    if m.tags != old.tags {
        tag_ids.write(conn, id, &m.tags)?;
    }
    if m.deleted_ms.is_some() && old.deleted_ms.is_none() {
        // what a deletion does besides the stamp (see soft_delete_booking)
        conn.execute("DELETE FROM loan_links WHERE transaction_id = ?1", [id])?;
    }
    conn.execute(
        "UPDATE transactions SET category_ms = ?2, note = ?3, note_ms = ?4, checked = ?5,
            checked_ms = ?6, counterparty = ?7, purpose = ?8, text_ms = ?9, splits_ms = ?10,
            deleted_ms = ?11, tags_ms = ?12
         WHERE id = ?1",
        params![
            id,
            m.category_ms,
            m.note,
            m.note_ms,
            m.checked,
            m.checked_ms,
            m.text.0,
            m.text.1,
            m.text_ms,
            m.splits_ms,
            m.deleted_ms,
            m.tags_ms
        ],
    )?;
    Ok(())
}
