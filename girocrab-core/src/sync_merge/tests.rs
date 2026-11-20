use std::sync::Arc;

use rusqlite::{Connection, params};

use super::*;
use crate::vault::Vault;

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "gc-sync-{tag}-{}-{}",
        std::process::id(),
        Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn open(dir: &std::path::Path, file: &str) -> Arc<Vault> {
    Vault::open(dir.join(file).to_string_lossy().into(), vec![7u8; 32]).unwrap()
}

fn uuids(v: &Vault, sql: &str) -> Vec<String> {
    let db = v.db().unwrap();
    let mut st = db.prepare(sql).unwrap();
    st.query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

#[test]
fn migration_is_idempotent() {
    let dir = temp_dir("idem");
    let v = open(&dir, "a.db");
    v.seed_starter_categories().unwrap();
    let before = uuids(&v, "SELECT uuid FROM categories ORDER BY id");
    {
        let db = v.db().unwrap();
        migrate(&db).unwrap();
        migrate(&db).unwrap();
    }
    assert_eq!(before, uuids(&v, "SELECT uuid FROM categories ORDER BY id"));
    drop(v);
    let v = open(&dir, "a.db");
    assert_eq!(before, uuids(&v, "SELECT uuid FROM categories ORDER BY id"));
}

/// The rule merge clears `rule_id` per deleted rule on every merge.
#[test]
fn bookings_are_indexed_by_rule() {
    let v = open(&temp_dir("ruleidx"), "a.db");
    let db = v.db().unwrap();
    migrate(&db).unwrap();
    let idx: Vec<String> = {
        let mut st = db
            .prepare(
                "SELECT il.name FROM pragma_index_list('transactions') il
                       JOIN pragma_index_info(il.name) ii WHERE ii.name = 'rule_id'",
            )
            .unwrap();
        let r = st.query_map([], |r| r.get(0)).unwrap();
        r.map(|r| r.unwrap()).collect()
    };
    assert_eq!(idx, ["tx_rule"]);
}

/// Counts filter `deleted_ms IS NULL`: the booking indexes they use cover it,
/// also in vaults whose indexes predate the column (upgraded once, then kept).
#[test]
fn booking_count_indexes_cover_deleted_ms() {
    let v = open(&temp_dir("coveridx"), "a.db");
    let db = v.db().unwrap();
    let cols = |name: &str| -> Vec<String> {
        let mut st = db
            .prepare("SELECT name FROM pragma_index_info(?1) ORDER BY seqno")
            .unwrap();
        let r = st.query_map([name], |r| r.get(0)).unwrap();
        r.map(|r| r.unwrap()).collect()
    };
    db.execute_batch(
        "DROP INDEX tx_account_checked; CREATE INDEX tx_account_checked ON transactions(account_id, checked);",
    )
    .unwrap();
    migrate(&db).unwrap();
    migrate(&db).unwrap();
    assert_eq!(cols("tx_rule"), ["rule_id", "deleted_ms"]);
    assert_eq!(
        cols("tx_account_category"),
        ["account_id", "category_id", "deleted_ms"]
    );
    assert_eq!(
        cols("tx_account_checked"),
        ["account_id", "checked", "deleted_ms"]
    );
}

/// A vault that ever had categories is never reseeded: the app seeds on
/// every unlock, and new starter categories would spread to the other devices.
#[test]
fn deleting_all_categories_does_not_reseed() {
    let v = open(&temp_dir("noreseed"), "a.db");
    assert!(v.seed_starter_categories().unwrap() > 0);
    let tops: Vec<i64> = {
        let db = v.db().unwrap();
        let mut st = db
            .prepare("SELECT id FROM categories WHERE parent_id IS NULL")
            .unwrap();
        let r = st.query_map([], |r| r.get(0)).unwrap();
        r.map(|r| r.unwrap()).collect()
    };
    for id in tops {
        v.delete_category(id).unwrap();
    }
    let live = "SELECT count(*) FROM categories WHERE deleted_ms IS NULL";
    assert_eq!(n(&v, live), 0);
    let all = n(&v, "SELECT count(*) FROM categories");
    assert_eq!(v.seed_starter_categories().unwrap(), 0);
    assert_eq!(n(&v, live), 0);
    assert_eq!(n(&v, "SELECT count(*) FROM categories"), all);
}

#[test]
fn same_category_path_gets_same_uuid_on_two_vaults() {
    let dir = temp_dir("path");
    let (a, b) = (open(&dir, "a.db"), open(&dir, "b.db"));
    a.seed_starter_categories().unwrap();
    b.seed_starter_categories().unwrap();
    let sql = "SELECT c.uuid FROM categories c ORDER BY c.name, c.id";
    let ua = uuids(&a, sql);
    assert!(!ua.is_empty());
    assert_eq!(ua, uuids(&b, sql));
    let distinct: std::collections::HashSet<_> = ua.iter().collect();
    assert_eq!(distinct.len(), ua.len());
    assert_eq!(
        category_uuid_for_path(&[" Lebensmittel ", "SUPERMARKT"]),
        category_uuid_for_path(&["lebensmittel", "supermarkt"])
    );
    assert_ne!(
        category_uuid_for_path(&["a", "b"]),
        category_uuid_for_path(&["a b"])
    );
}

#[test]
fn rule_uuid_depends_on_content_only() {
    let u = |p: &str, cs: bool, acc: Option<&str>| {
        rule_uuid_for_content("any", "keyword", p, cs, Some(1), None, acc, "cat")
    };
    assert_eq!(u("x", false, None), u("x", false, None));
    assert_ne!(u("x", false, None), u("y", false, None));
    assert_ne!(u("x", false, None), u("x", true, None));
    assert_ne!(u("x", false, None), u("x", false, Some("k|1")));
    assert_ne!(
        rule_uuid_for_content("any", "keyword", "x", false, Some(1), None, None, "c1"),
        rule_uuid_for_content("any", "keyword", "x", false, Some(1), None, None, "c2")
    );
}

#[test]
fn v7_vault_upgrades_and_keeps_rows() {
    let dir = temp_dir("v7");
    let path = dir.join("old.db").to_string_lossy().to_string();
    {
        let conn = Connection::open(&path).unwrap();
        Vault::apply_key(&conn, &[7u8; 32]).unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        conn.execute_batch(crate::vault::SCHEMA).unwrap();
        crate::user_groups::migrate(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO account_groups(id, name) VALUES (1, 'G');
             INSERT INTO accounts(id, group_id, external_ref, name) VALUES (1, 1, 'DE1', 'Giro');
             INSERT INTO categories(id, name) VALUES (1, 'Food');
             INSERT INTO rules(name, category_id, pattern) VALUES ('r', 1, 'lidl');
             INSERT INTO transactions(account_id, external_id, booking_date, amount_cents)
                 VALUES (1, 'e1', '2026-01-01', -100);
             PRAGMA user_version = 7;",
        )
        .unwrap();
    }
    let v = Vault::open(path, vec![7u8; 32]).unwrap();
    let db = v.db().unwrap();
    let version: i64 = db
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, crate::vault::SCHEMA_VERSION);
    let n = |sql: &str| db.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
    assert_eq!(
        n("SELECT count(*) FROM categories WHERE uuid IS NOT NULL"),
        1
    );
    assert_eq!(n("SELECT count(*) FROM rules WHERE uuid IS NOT NULL"), 1);
    assert_eq!(
        n("SELECT count(*) FROM transactions WHERE deleted_ms IS NULL"),
        1
    );
    assert_eq!(n("SELECT count(*) FROM accounts"), 1);
}

#[test]
fn sync_keys_are_stable() {
    let dir = temp_dir("keys");
    let (a, b) = (open(&dir, "a.db"), open(&dir, "b.db"));
    let seed = |v: &Vault, pad: i64| {
        let db = v.db().unwrap();
        db.execute_batch("INSERT INTO account_groups(id, name) VALUES (1, 'G');")
            .unwrap();
        for i in 0..pad {
            db.execute(
                "INSERT INTO bank_connections(connector, display_name, login_name) VALUES ('paypal', 'x', ?1)",
                [format!("pad{i}")],
            )
            .unwrap();
        }
        db.execute(
            "INSERT INTO bank_connections(connector, display_name, login_name, bank_code)
             VALUES ('traderepublic', 'TR', '+49 151 234567', '')",
            [],
        )
        .unwrap();
        let c = db.last_insert_rowid();
        db.execute(
            "INSERT INTO accounts(connection_id, group_id, external_ref, name) VALUES (?1, 1, 'DE9', 'A')",
            [c],
        )
        .unwrap();
        let acc = db.last_insert_rowid();
        db.execute(
            "INSERT INTO accounts(group_id, external_ref, name) VALUES (1, 'loose', 'L')",
            [],
        )
        .unwrap();
        let loose = db.last_insert_rowid();
        db.execute(
            "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents) VALUES (?1, 'e1', '2026-01-01', 5)",
            params![acc],
        )
        .unwrap();
        let t = db.last_insert_rowid();
        (
            connection_key(&db, c).unwrap(),
            account_key(&db, acc).unwrap(),
            account_key(&db, loose).unwrap(),
            booking_key(&db, t).unwrap(),
        )
    };
    let ka = seed(&a, 0);
    let kb = seed(&b, 3);
    assert_eq!(ka, kb);
    assert_eq!(ka.2, "|loose");
    assert!(ka.3.ends_with("|DE9|e1"));
    assert!(ka.0.starts_with("traderepublic|+"));
    // identical content converges even with different local ids
    let (c, d) = (open(&dir, "c.db"), open(&dir, "d.db"));
    seed(&c, 0);
    seed(&d, 0);
    assert_converged(&c, &d);
    assert_ne!(synced_state(&a), synced_state(&b));
}

#[test]
fn deleted_category_name_can_be_reused() {
    let dir = temp_dir("reuse");
    let v = open(&dir, "a.db");
    let db = v.db().unwrap();
    db.execute("INSERT INTO categories(name) VALUES ('Food')", [])
        .unwrap();
    assert!(
        db.execute("INSERT INTO categories(name) VALUES ('Food')", [])
            .is_err()
    );
    db.execute(
        "UPDATE categories SET deleted_ms = 1 WHERE name = 'Food'",
        [],
    )
    .unwrap();
    db.execute("INSERT INTO categories(name) VALUES ('Food')", [])
        .unwrap();
    fill_missing_uuids(&db).unwrap();
    let u: Vec<String> = db
        .prepare("SELECT uuid FROM categories ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(u.len(), 2);
    // the deleted row keeps the path uuid, the new one gets a random one
    assert_eq!(u[0], category_uuid_for_path(&["Food"]));
    assert_ne!(u[0], u[1]);
}

/// Two vaults built the same way, with a category, a booking and its stamps.
fn identical_pair(dir: &std::path::Path) -> (Arc<Vault>, Arc<Vault>) {
    let pair = (open(dir, "p1.db"), open(dir, "p2.db"));
    for v in [&pair.0, &pair.1] {
        let db = v.db().unwrap();
        db.execute_batch(
            "INSERT INTO account_groups(id, name) VALUES (1, 'G');
             INSERT INTO accounts(id, group_id, external_ref, name) VALUES (1, 1, 'DE1', 'A');
             INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, note_ms)
                 VALUES (1, 'e1', '2026-01-01', 5, 10);
             INSERT INTO categories(name) VALUES ('Food');",
        )
        .unwrap();
        fill_missing_uuids(&db).unwrap();
    }
    pair
}

#[test]
fn assert_converged_accepts_identical_vaults() {
    let dir = temp_dir("conv-ok");
    let (a, b) = identical_pair(&dir);
    assert_converged(&a, &b);
}

#[test]
fn assert_converged_panics_on_one_differing_stamp() {
    let dir = temp_dir("conv-stamp");
    let (a, b) = identical_pair(&dir);
    b.db()
        .unwrap()
        .execute("UPDATE transactions SET note_ms = 11", [])
        .unwrap();
    assert!(std::panic::catch_unwind(|| assert_converged(&a, &b)).is_err());
}

#[test]
fn assert_converged_panics_on_one_differing_deletion() {
    let dir = temp_dir("conv-del");
    let (a, b) = identical_pair(&dir);
    b.db()
        .unwrap()
        .execute("UPDATE categories SET deleted_ms = 5", [])
        .unwrap();
    assert!(std::panic::catch_unwind(|| assert_converged(&a, &b)).is_err());
}

const STAMP_COLS: [&str; 5] = [
    "category_ms",
    "note_ms",
    "checked_ms",
    "text_ms",
    "splits_ms",
];

fn stamps(v: &Vault) -> Vec<i64> {
    let db = v.db().unwrap();
    STAMP_COLS
        .iter()
        .map(|c| {
            db.query_row(
                &format!("SELECT {c} FROM transactions WHERE id = 1"),
                [],
                |r| r.get(0),
            )
            .unwrap()
        })
        .collect()
}

fn stamp_vault(tag: &str) -> Arc<Vault> {
    let v = open(&temp_dir(tag), "s.db");
    v.db()
        .unwrap()
        .execute_batch(
            "INSERT INTO account_groups(id, name) VALUES (1, 'G');
             INSERT INTO accounts(id, group_id, external_ref, name) VALUES (1, 1, 'DE1', 'A');
             INSERT INTO transactions(id, account_id, external_id, booking_date, amount_cents, counterparty)
                 VALUES (1, 1, 'e1', '2026-01-01', 5, 'Shop');",
        )
        .unwrap();
    v
}

#[test]
fn editing_a_booking_stamps_only_that_field() {
    let v = stamp_vault("stamp-fields");
    assert_eq!(stamps(&v), vec![0; 5]);
    let cat = v
        .create_category("Food".into(), None, String::new(), String::new())
        .unwrap();
    // (edit, index of the one stamp that must move)
    type Edit = Box<dyn Fn(&Vault)>;
    let edits: Vec<(usize, Edit)> = vec![
        (
            0,
            Box::new(move |v| v.set_transaction_category(1, Some(cat)).unwrap()),
        ),
        (
            1,
            Box::new(|v| v.update_transaction_note(1, "n".into()).unwrap()),
        ),
        (2, Box::new(|v| v.set_transaction_checked(1, true).unwrap())),
        (
            3,
            Box::new(|v| {
                v.update_transaction_text(1, "X".into(), "Y".into())
                    .unwrap()
            }),
        ),
        (
            4,
            Box::new(move |v| {
                v.set_transaction_splits(
                    1,
                    vec![
                        crate::models::TransactionSplitInput {
                            category_id: Some(cat),
                            amount_cents: 3,
                            note: String::new(),
                        },
                        crate::models::TransactionSplitInput {
                            category_id: None,
                            amount_cents: 2,
                            note: String::new(),
                        },
                    ],
                )
                .unwrap()
            }),
        ),
    ];
    for (idx, edit) in edits {
        let before = stamps(&v);
        edit(&v);
        let after = stamps(&v);
        for i in 0..5 {
            if i == idx {
                assert!(
                    after[i] > 0 && after[i] >= before[i],
                    "{} not stamped",
                    STAMP_COLS[i]
                );
            } else {
                assert_eq!(after[i], before[i], "{} must not move", STAMP_COLS[i]);
            }
        }
    }
}

#[test]
fn rule_assignment_does_not_stamp() {
    let v = stamp_vault("stamp-rule");
    let cat = v
        .create_category("Food".into(), None, String::new(), String::new())
        .unwrap();
    v.save_rule(crate::models::Rule {
        id: 0,
        name: "shop".into(),
        category_id: cat,
        field: crate::models::RuleField::Counterparty,
        match_kind: crate::models::MatchKind::Keyword,
        pattern: "Shop".into(),
        case_sensitive: false,
        min_amount_cents: None,
        max_amount_cents: None,
        account_ids: Vec::new(),
        priority: 0,
        enabled: true,
        match_count: 0,
        tag_ids: Vec::new(),
    })
    .unwrap();
    v.apply_rules(false).unwrap();
    let src: String = v
        .db()
        .unwrap()
        .query_row(
            "SELECT category_source FROM transactions WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(src, "rule");
    assert_eq!(stamps(&v), vec![0; 5]);
}

#[test]
fn manual_clear_stamps_and_lets_rules_refill() {
    let v = stamp_vault("stamp-clear");
    let cat = v
        .create_category("Food".into(), None, String::new(), String::new())
        .unwrap();
    v.save_rule(crate::models::Rule {
        id: 0,
        name: "shop".into(),
        category_id: cat,
        field: crate::models::RuleField::Counterparty,
        match_kind: crate::models::MatchKind::Keyword,
        pattern: "Shop".into(),
        case_sensitive: false,
        min_amount_cents: None,
        max_amount_cents: None,
        account_ids: Vec::new(),
        priority: 0,
        enabled: true,
        match_count: 0,
        tag_ids: Vec::new(),
    })
    .unwrap();
    v.set_transaction_category(1, None).unwrap();
    let ms = stamps(&v)[0];
    assert!(ms > 0);
    let row = |v: &Vault| -> (Option<i64>, String) {
        v.db()
            .unwrap()
            .query_row(
                "SELECT category_id, category_source FROM transactions WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    };
    assert_eq!(row(&v), (None, "none".to_string()));
    v.apply_rules(true).unwrap();
    assert_eq!(row(&v), (Some(cat), "rule".to_string()));
    assert_eq!(stamps(&v)[0], ms);
}

fn cat_stamps(v: &Vault, id: i64) -> Vec<i64> {
    let db = v.db().unwrap();
    db.query_row(
        "SELECT name_ms, parent_ms, symbol_ms, color_ms, budget_ms, order_ms FROM categories WHERE id = ?1",
        [id],
        |r| (0..6).map(|i| r.get(i)).collect::<Result<Vec<i64>, _>>(),
    )
    .unwrap()
}

fn plain_rule(category_id: i64, pattern: &str) -> crate::models::Rule {
    crate::models::Rule {
        id: 0,
        name: "r".into(),
        category_id,
        field: crate::models::RuleField::Any,
        match_kind: crate::models::MatchKind::Keyword,
        pattern: pattern.into(),
        case_sensitive: false,
        min_amount_cents: None,
        max_amount_cents: None,
        account_ids: Vec::new(),
        priority: 10,
        enabled: true,
        match_count: 0,
        tag_ids: Vec::new(),
    }
}

#[test]
fn rename_stamps_name_only() {
    let v = open(&temp_dir("rename"), "a.db");
    let id = v
        .create_category("Food".into(), None, "tag".into(), "#111111".into())
        .unwrap();
    // seeded stamps are 0 for a category without user edits
    v.db().unwrap().execute("UPDATE categories SET name_ms=0, parent_ms=0, symbol_ms=0, color_ms=0, budget_ms=0, order_ms=0", []).unwrap();
    v.update_category(id, "Essen".into(), None, "tag".into(), "#111111".into())
        .unwrap();
    let s = cat_stamps(&v, id);
    assert!(s[0] > 0, "{s:?}");
    assert_eq!(&s[1..], &[0, 0, 0, 0, 0]);
    // unchanged save stamps nothing more
    let before = s[0];
    v.update_category(id, "Essen".into(), None, "tag".into(), "#111111".into())
        .unwrap();
    assert_eq!(cat_stamps(&v, id)[0], before);
    // colour change stamps colour only
    v.update_category(id, "Essen".into(), None, "tag".into(), "#222222".into())
        .unwrap();
    let s = cat_stamps(&v, id);
    assert!(s[3] > 0);
    assert_eq!((s[1], s[2], s[4], s[5]), (0, 0, 0, 0));
}

#[test]
fn budget_stamps_budget_only() {
    let v = open(&temp_dir("budget"), "a.db");
    let id = v
        .create_category("Food".into(), None, "tag".into(), "#111111".into())
        .unwrap();
    v.db().unwrap().execute("UPDATE categories SET name_ms=0, parent_ms=0, symbol_ms=0, color_ms=0, budget_ms=0, order_ms=0", []).unwrap();
    v.set_category_budget(id, Some(5000)).unwrap();
    let s = cat_stamps(&v, id);
    assert!(s[4] > 0, "{s:?}");
    assert_eq!((s[0], s[1], s[2], s[3], s[5]), (0, 0, 0, 0, 0));
    // clearing is an edit too
    v.db()
        .unwrap()
        .execute("UPDATE categories SET budget_ms = 1", [])
        .unwrap();
    v.set_category_budget(id, None).unwrap();
    assert!(cat_stamps(&v, id)[4] > 1);
}

#[test]
fn deleting_a_category_soft_deletes_it_its_children_and_rules() {
    let v = open(&temp_dir("catdel"), "a.db");
    let parent = v
        .create_category("Home".into(), None, "tag".into(), "#111111".into())
        .unwrap();
    let child = v
        .create_category("Power".into(), Some(parent), "tag".into(), "#111111".into())
        .unwrap();
    let other = v
        .create_category("Other".into(), None, "tag".into(), "#111111".into())
        .unwrap();
    let rule = v.save_rule(plain_rule(child, "stadtwerke")).unwrap();
    let keep_rule = v.save_rule(plain_rule(other, "lidl")).unwrap();
    {
        let db = v.db().unwrap();
        db.execute_batch(
            "INSERT INTO account_groups(id, name) VALUES (1, 'G');
             INSERT INTO accounts(id, group_id, external_ref, name) VALUES (1, 1, 'DE1', 'A');",
        )
        .unwrap();
        for (i, c) in [("e1", child), ("e2", other)] {
            db.execute(
                "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, category_id, category_source, category_ms)
                 VALUES (1, ?1, '2026-01-01', -5, ?2, 'manual', 77)",
                params![i, c],
            )
            .unwrap();
        }
        db.execute(
            "INSERT INTO transaction_splits(id, transaction_id, category_id, amount_cents) VALUES (1, 2, ?1, -3)",
            [child],
        )
        .unwrap();
        db.execute(
            "INSERT INTO transaction_splits(id, transaction_id, category_id, amount_cents) VALUES (2, 2, ?1, -2)",
            [other],
        )
        .unwrap();
    }
    v.delete_category(parent).unwrap();
    let db = v.db().unwrap();
    let del = |t: &str, id: i64| -> Option<i64> {
        db.query_row(
            &format!("SELECT deleted_ms FROM {t} WHERE id = ?1"),
            [id],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert!(del("categories", parent).unwrap() > 0);
    assert!(del("categories", child).unwrap() > 0);
    assert!(del("categories", other).is_none());
    assert!(del("rules", rule).unwrap() > 0);
    assert!(del("rules", keep_rule).is_none());
    let (cat, src, ms): (Option<i64>, String, i64) = db
        .query_row("SELECT category_id, category_source, category_ms FROM transactions WHERE external_id='e1'", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap();
    assert_eq!((cat, src.as_str(), ms), (None, "none", 77));
    let (cat, src): (Option<i64>, String) = db
        .query_row(
            "SELECT category_id, category_source FROM transactions WHERE external_id='e2'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((cat, src.as_str()), (Some(other), "manual"));
    let split_cat = |id: i64| -> Option<i64> {
        db.query_row(
            "SELECT category_id FROM transaction_splits WHERE id = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap()
    };
    // the split row stays, uncategorised; the unrelated split is untouched
    assert_eq!(split_cat(1), None);
    assert_eq!(split_cat(2), Some(other));
    drop(db);
    let ids: Vec<i64> = v
        .list_categories(None, None)
        .unwrap()
        .iter()
        .map(|c| c.id)
        .collect();
    assert_eq!(ids, vec![other]);
    let rules: Vec<i64> = v.list_rules().unwrap().iter().map(|r| r.id).collect();
    assert_eq!(rules, vec![keep_rule]);
}

#[test]
fn save_rule_stamps_and_keeps_uuid_on_edit() {
    let v = open(&temp_dir("rule"), "a.db");
    let cat = v
        .create_category("Food".into(), None, "tag".into(), "#111111".into())
        .unwrap();
    let id = v.save_rule(plain_rule(cat, "lidl")).unwrap();
    let get = |v: &Vault| -> (String, i64) {
        v.db()
            .unwrap()
            .query_row(
                "SELECT uuid, updated_ms FROM rules WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    };
    let (uuid, ms) = get(&v);
    assert!(ms > 0);
    v.db()
        .unwrap()
        .execute("UPDATE rules SET updated_ms = 1", [])
        .unwrap();
    let mut r = plain_rule(cat, "lidl");
    r.id = id;
    r.pattern = "edeka".into();
    v.save_rule(r).unwrap();
    let (uuid2, ms2) = get(&v);
    assert_eq!(uuid, uuid2);
    assert!(ms2 > 1);
    // same content as a deleted rule: random uuid, content uuid stays taken
    v.delete_rule(id).unwrap();
    let (_, del_ms): (String, i64) = v
        .db()
        .unwrap()
        .query_row(
            "SELECT uuid, deleted_ms FROM rules WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(del_ms > 0);
    assert!(v.list_rules().unwrap().is_empty());
    let id2 = v.save_rule(plain_rule(cat, "lidl")).unwrap();
    let u2: String = v
        .db()
        .unwrap()
        .query_row("SELECT uuid FROM rules WHERE id = ?1", [id2], |r| r.get(0))
        .unwrap();
    assert_ne!(u2, uuid);
}

fn n(v: &Vault, sql: &str) -> i64 {
    v.db()
        .unwrap()
        .query_row(sql, [], |r| r.get::<_, i64>(0))
        .unwrap()
}

/// One connection `c` with two accounts and one booking each, plus an unrelated
/// loose account with a booking. Returns (connection, account 1, account 2, loose).
fn bank_fixture(v: &Vault) -> (i64, i64, i64, i64) {
    let db = v.db().unwrap();
    db.execute_batch("INSERT INTO account_groups(id, name) VALUES (1, 'G');")
        .unwrap();
    db.execute(
        "INSERT INTO bank_connections(connector, display_name, login_name, bank_code)
         VALUES ('fints:12345678', 'Bank', 'me', '12345678')",
        [],
    )
    .unwrap();
    let c = db.last_insert_rowid();
    let mut ids = Vec::new();
    for (conn, ext) in [(Some(c), "A1"), (Some(c), "A2"), (None, "loose")] {
        db.execute(
            "INSERT INTO accounts(connection_id, group_id, external_ref, name) VALUES (?1, 1, ?2, ?2)",
            params![conn, ext],
        )
        .unwrap();
        let a = db.last_insert_rowid();
        db.execute(
            "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents)
             VALUES (?1, 'e1', '2026-01-01', -5)",
            [a],
        )
        .unwrap();
        ids.push(a);
    }
    (c, ids[0], ids[1], ids[2])
}

#[test]
fn deleting_connection_soft_deletes_it_accounts_and_bookings() {
    let v = open(&temp_dir("conndel"), "a.db");
    let (c, a1, _, loose) = bank_fixture(&v);
    v.delete_bank_connection(c).unwrap();
    assert_eq!(n(&v, "SELECT count(*) FROM bank_connections"), 1);
    assert_eq!(
        n(
            &v,
            "SELECT count(*) FROM bank_connections WHERE deleted_ms > 0"
        ),
        1
    );
    assert_eq!(n(&v, "SELECT count(*) FROM accounts"), 3);
    assert_eq!(
        n(&v, "SELECT count(*) FROM accounts WHERE deleted_ms > 0"),
        2
    );
    assert_eq!(n(&v, "SELECT count(*) FROM transactions"), 3);
    assert_eq!(
        n(&v, "SELECT count(*) FROM transactions WHERE deleted_ms > 0"),
        2
    );
    assert_eq!(
        n(
            &v,
            &format!(
                "SELECT count(*) FROM transactions WHERE account_id = {loose} AND deleted_ms IS NULL"
            )
        ),
        1
    );
    // the accounts keep their connection, so their sync keys stay stable
    assert_eq!(
        n(
            &v,
            &format!("SELECT connection_id FROM accounts WHERE id = {a1}")
        ),
        c
    );
}

#[test]
fn deleting_account_soft_deletes_it_and_its_bookings() {
    let v = open(&temp_dir("accdel"), "a.db");
    let (c, a1, a2, _) = bank_fixture(&v);
    v.set_account_archived(a1, true).unwrap();
    v.delete_account(a1).unwrap();
    assert_eq!(n(&v, "SELECT count(*) FROM accounts"), 3);
    assert_eq!(
        n(
            &v,
            &format!(
                "SELECT count(*) FROM transactions WHERE account_id = {a1} AND deleted_ms > 0"
            )
        ),
        1
    );
    assert_eq!(
        n(
            &v,
            "SELECT count(*) FROM bank_connections WHERE deleted_ms IS NULL"
        ),
        1
    );
    // the last account of a connection takes the connection along
    v.set_account_archived(a2, true).unwrap();
    v.delete_account(a2).unwrap();
    assert_eq!(
        n(
            &v,
            &format!("SELECT deleted_ms > 0 FROM bank_connections WHERE id = {c}")
        ),
        1
    );
}

#[test]
fn readding_connection_revives_it_with_new_created_ms() {
    let v = open(&temp_dir("revive"), "a.db");
    let id = v
        .add_bank_connection(ConnectorKind::Comdirect, String::new(), "Login".into())
        .unwrap();
    assert!(n(&v, "SELECT created_ms FROM bank_connections") > 1);
    v.db()
        .unwrap()
        .execute("UPDATE bank_connections SET created_ms = 1", [])
        .unwrap();
    v.delete_bank_connection(id).unwrap();
    let again = v
        .add_bank_connection(ConnectorKind::Comdirect, String::new(), "login".into())
        .unwrap();
    assert_eq!(again, id);
    assert_eq!(n(&v, "SELECT count(*) FROM bank_connections"), 1);
    assert_eq!(
        n(
            &v,
            "SELECT count(*) FROM bank_connections WHERE deleted_ms IS NULL"
        ),
        1
    );
    assert!(n(&v, "SELECT created_ms FROM bank_connections") > 1);
}

#[test]
fn deleted_connection_keeps_no_session_here_or_on_the_other_vault() {
    let dir = temp_dir("connsession");
    let (a, b) = (open(&dir, "a.db"), open(&dir, "b.db"));
    let id = a
        .add_bank_connection(ConnectorKind::Comdirect, String::new(), "login".into())
        .unwrap();
    exec(
        &a,
        r#"UPDATE bank_connections SET state_json = '{"token":"secret"}'"#,
    );
    both_ways(&a, &b);
    assert_eq!(
        text(&b, "SELECT state_json FROM bank_connections"),
        r#"{"token":"secret"}"#
    );
    a.delete_bank_connection(id).unwrap();
    assert_eq!(text(&a, "SELECT state_json FROM bank_connections"), "{}");
    both_ways(&a, &b);
    for v in [&a, &b] {
        assert_eq!(
            n(
                v,
                "SELECT count(*) FROM bank_connections WHERE deleted_ms > 0"
            ),
            1
        );
        assert_eq!(text(v, "SELECT state_json FROM bank_connections"), "{}");
    }
    // a vault that never saw the connection gets a tombstone without session,
    // even from a tombstone that still carries one (older build)
    let c = open(&dir, "c.db");
    exec(
        &a,
        r#"UPDATE bank_connections SET state_json = '{"token":"secret"}'"#,
    );
    merge(&c, &a);
    assert_eq!(text(&c, "SELECT state_json FROM bank_connections"), "{}");
    exec(&a, "UPDATE bank_connections SET state_json = '{}'");
    // re-added, the connection starts without the old session
    let again = a
        .add_bank_connection(ConnectorKind::Comdirect, String::new(), "login".into())
        .unwrap();
    assert_eq!(again, id);
    assert_eq!(text(&a, "SELECT state_json FROM bank_connections"), "{}");
    both_ways(&a, &b);
    both_ways(&a, &c);
}

fn remote_account(ext: &str) -> crate::connectors::RemoteAccount {
    crate::connectors::RemoteAccount {
        external_ref: ext.into(),
        name: "Giro".into(),
        owner: String::new(),
        iban: String::new(),
        account_number: String::new(),
        currency: "EUR".into(),
        kind: crate::models::AccountKind::Checking,
        balance_cents: 0,
        balance_date: "2026-11-10".into(),
    }
}

fn remote_tx(ext: &str, id: &str) -> crate::connectors::RemoteTransaction {
    crate::connectors::RemoteTransaction {
        account_ref: ext.into(),
        external_id: id.into(),
        booking_date: "2026-11-10".into(),
        value_date: "2026-11-10".into(),
        amount_cents: -500,
        currency: "EUR".into(),
        counterparty: "Shop".into(),
        purpose: "Kauf".into(),
        ..Default::default()
    }
}

#[test]
fn fetch_does_not_resurrect_deleted_booking() {
    let v = open(&temp_dir("noresurrect"), "a.db");
    let c = v
        .add_bank_connection(ConnectorKind::Comdirect, String::new(), "login".into())
        .unwrap();
    let fetch = |ids: &[&str]| crate::connectors::FetchResult {
        accounts: vec![remote_account("DE1")],
        transactions: ids.iter().map(|i| remote_tx("DE1", i)).collect(),
        adjust_balances: true,
        ..Default::default()
    };
    crate::connectors::store_fetch(&v, ConnectorKind::Comdirect, c, fetch(&["b1", "b2"])).unwrap();
    assert_eq!(n(&v, "SELECT created_ms > 1 FROM accounts"), 1);
    v.db()
        .unwrap()
        .execute(
            "UPDATE transactions SET deleted_ms = 5 WHERE external_id = 'b1'",
            [],
        )
        .unwrap();
    let before = n(&v, "SELECT balance_cents FROM accounts");
    let s = crate::connectors::store_fetch(&v, ConnectorKind::Comdirect, c, fetch(&["b1", "b2"]))
        .unwrap();
    assert_eq!(s.transactions_imported, 0);
    assert_eq!(n(&v, "SELECT count(*) FROM transactions"), 2);
    assert_eq!(
        n(
            &v,
            "SELECT deleted_ms FROM transactions WHERE external_id = 'b1'"
        ),
        5
    );
    assert_eq!(n(&v, "SELECT balance_cents FROM accounts"), before);
}

#[test]
fn fetch_revives_a_deleted_account_it_delivers() {
    // deleted before the connection was added (again): the fetch revives it
    let v = open(&temp_dir("accrevive"), "a.db");
    let c = v
        .add_bank_connection(ConnectorKind::Comdirect, String::new(), "login".into())
        .unwrap();
    let fetch = || crate::connectors::FetchResult {
        accounts: vec![remote_account("DE1")],
        ..Default::default()
    };
    crate::connectors::store_fetch(&v, ConnectorKind::Comdirect, c, fetch()).unwrap();
    v.db()
        .unwrap()
        .execute("UPDATE accounts SET created_ms = 1, deleted_ms = 2", [])
        .unwrap();
    crate::connectors::store_fetch(&v, ConnectorKind::Comdirect, c, fetch()).unwrap();
    assert_eq!(n(&v, "SELECT count(*) FROM accounts"), 1);
    assert_eq!(
        n(&v, "SELECT count(*) FROM accounts WHERE deleted_ms IS NULL"),
        1
    );
    assert!(n(&v, "SELECT created_ms FROM accounts") > 2);
}

#[test]
fn deleted_account_stays_deleted_when_another_device_fetches_it() {
    let dir = temp_dir("accfetch");
    let (a, b) = (open(&dir, "a.db"), open(&dir, "b.db"));
    let c_a = a
        .add_bank_connection(ConnectorKind::Comdirect, String::new(), "login".into())
        .unwrap();
    let fetch = || crate::connectors::FetchResult {
        accounts: vec![remote_account("DE1"), remote_account("DE2")],
        transactions: vec![remote_tx("DE1", "b1"), remote_tx("DE2", "b2")],
        adjust_balances: true,
        ..Default::default()
    };
    crate::connectors::store_fetch(&a, ConnectorKind::Comdirect, c_a, fetch()).unwrap();
    exec(&a, "UPDATE bank_connections SET created_ms = 1");
    exec(&a, "UPDATE accounts SET created_ms = 1");
    both_ways(&a, &b);
    let de1 = "SELECT id FROM accounts WHERE external_ref = 'DE1'";
    a.set_account_archived(n(&a, de1), true).unwrap();
    a.delete_account(n(&a, de1)).unwrap();
    both_ways(&a, &b);
    let c_b = n(&b, "SELECT id FROM bank_connections");
    // B's next fetch (and A's) still delivers DE1: it stays deleted
    crate::connectors::store_fetch(&b, ConnectorKind::Comdirect, c_b, fetch()).unwrap();
    crate::connectors::store_fetch(&a, ConnectorKind::Comdirect, c_a, fetch()).unwrap();
    both_ways(&a, &b);
    for v in [&a, &b] {
        assert_eq!(n(v, "SELECT count(*) FROM accounts"), 2);
        assert_eq!(
            n(
                v,
                "SELECT deleted_ms > 0 FROM accounts WHERE external_ref = 'DE1'"
            ),
            1
        );
        assert_eq!(
            n(
                v,
                "SELECT count(*) FROM transactions WHERE deleted_ms IS NULL"
            ),
            1
        );
    }
    // the connection deleted and added again on B: the fetch revives DE1
    // together with the bookings deleted with it
    b.delete_bank_connection(c_b).unwrap();
    let again = b
        .add_bank_connection(ConnectorKind::Comdirect, String::new(), "login".into())
        .unwrap();
    assert_eq!(again, c_b);
    crate::connectors::store_fetch(&b, ConnectorKind::Comdirect, c_b, fetch()).unwrap();
    both_ways(&a, &b);
    for v in [&a, &b] {
        assert_eq!(
            n(v, "SELECT count(*) FROM accounts WHERE deleted_ms IS NULL"),
            2
        );
        assert_eq!(
            n(
                v,
                "SELECT count(*) FROM transactions WHERE deleted_ms IS NULL"
            ),
            2
        );
    }
}

/// An unconnected account (e.g. from an import) the user deleted: a fetch
/// over a connection that already existed then neither adopts nor recreates it.
#[test]
fn fetch_does_not_adopt_a_deleted_unlinked_account() {
    let v = open(&temp_dir("adoptdel"), "a.db");
    let c = v
        .add_bank_connection(ConnectorKind::Comdirect, String::new(), "login".into())
        .unwrap();
    exec(&v, "UPDATE bank_connections SET created_ms = 1");
    exec(&v, "INSERT INTO account_groups(id, name) VALUES (1, 'G')");
    exec(
        &v,
        "INSERT INTO accounts(group_id, external_ref, name, iban, created_ms, deleted_ms)
         VALUES (1, 'imported', 'Giro', 'DE02120300000000202051', 1, 5)",
    );
    let mut acc = remote_account("DE1");
    acc.iban = "DE02120300000000202051".into();
    let fetch = crate::connectors::FetchResult {
        accounts: vec![acc],
        transactions: vec![remote_tx("DE1", "b1")],
        ..Default::default()
    };
    crate::connectors::store_fetch(&v, ConnectorKind::Comdirect, c, fetch).unwrap();
    assert_eq!(n(&v, "SELECT count(*) FROM accounts"), 1);
    assert_eq!(text(&v, "SELECT external_ref FROM accounts"), "imported");
    assert_eq!(n(&v, "SELECT deleted_ms FROM accounts"), 5);
    assert_eq!(n(&v, "SELECT count(*) FROM transactions"), 0);
}

#[test]
fn pending_booking_removed_by_fetch_is_soft_deleted() {
    let v = open(&temp_dir("pending"), "a.db");
    let c = v
        .add_bank_connection(ConnectorKind::Comdirect, String::new(), "login".into())
        .unwrap();
    let fetch = |txs: Vec<crate::connectors::RemoteTransaction>| crate::connectors::FetchResult {
        accounts: vec![remote_account("DE1")],
        transactions: txs,
        hold_prefix: Some("hold-".into()),
        ..Default::default()
    };
    crate::connectors::store_fetch(
        &v,
        ConnectorKind::Comdirect,
        c,
        fetch(vec![remote_tx("DE1", "hold-1")]),
    )
    .unwrap();
    // an optimistic transfer booking older than 14 days
    v.db()
        .unwrap()
        .execute(
            "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, imported_unix)
             VALUES ((SELECT id FROM accounts), 'pending-e2e', '2026-01-01', -9, 1)",
            [],
        )
        .unwrap();
    crate::connectors::store_fetch(&v, ConnectorKind::Comdirect, c, fetch(vec![])).unwrap();
    assert_eq!(n(&v, "SELECT count(*) FROM transactions"), 2);
    assert_eq!(
        n(&v, "SELECT count(*) FROM transactions WHERE deleted_ms > 0"),
        2
    );
}

#[test]
fn twin_merge_soft_deletes_the_bank_copy() {
    let v = open(&temp_dir("twin"), "a.db");
    let csv = "Datum;Name;Verwendungszweck;Betrag;Kategorie;Notiz\n10.05.2026;REWE Markt;Einkauf;-12,50;;\n";
    let acc = v
        .import_moneymoney_data(csv.as_bytes().to_vec(), "t.csv".into(), None, true, true)
        .unwrap()
        .account_id;
    let db = v.db().unwrap();
    db.execute(
        "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, counterparty, purpose)
         VALUES (?1, 'f1', '2026-05-10', -1250, 'REWE', 'Einkauf')",
        [acc],
    )
    .unwrap();
    let bank_id = db.last_insert_rowid();
    assert!(
        crate::importers::moneymoney_import::merge_synced_twin(
            &db,
            bank_id,
            "2026-05-10",
            -1250,
            "",
            "REWE",
            "Einkauf"
        )
        .unwrap()
    );
    let deleted: Option<i64> = db
        .query_row(
            "SELECT deleted_ms FROM transactions WHERE id = ?1",
            [bank_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(deleted.unwrap() > 0);
    // the import row carries the bank's id and stays live
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM transactions WHERE external_id = 'f1' AND deleted_ms IS NULL",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
}

#[test]
fn revive_brings_back_cascade_bookings_but_not_individually_deleted_ones() {
    let v = open(&temp_dir("cascade"), "a.db");
    let c = v
        .add_bank_connection(ConnectorKind::Comdirect, String::new(), "login".into())
        .unwrap();
    let fetch = || crate::connectors::FetchResult {
        accounts: vec![remote_account("DE1")],
        transactions: ["b1", "b2", "b3"]
            .iter()
            .map(|i| remote_tx("DE1", i))
            .collect(),
        ..Default::default()
    };
    crate::connectors::store_fetch(&v, ConnectorKind::Comdirect, c, fetch()).unwrap();
    let live = |v: &Vault| {
        let mut ids = uuids(
            v,
            "SELECT external_id FROM transactions WHERE deleted_ms IS NULL ORDER BY 1",
        );
        ids.sort();
        ids
    };
    v.db()
        .unwrap()
        .execute(
            "UPDATE transactions SET deleted_ms = 5 WHERE external_id = 'b1'",
            [],
        )
        .unwrap();
    // connection delete + re-add + fetch
    v.delete_bank_connection(c).unwrap();
    assert!(live(&v).is_empty());
    let again = v
        .add_bank_connection(ConnectorKind::Comdirect, String::new(), "login".into())
        .unwrap();
    assert_eq!(again, c);
    assert!(live(&v).is_empty(), "bookings wait for the account");
    crate::connectors::store_fetch(&v, ConnectorKind::Comdirect, c, fetch()).unwrap();
    assert_eq!(live(&v), vec!["b2", "b3"]);
    // account delete (takes the connection along) + fetch: stays deleted
    let acc = n(&v, "SELECT id FROM accounts");
    v.set_account_archived(acc, true).unwrap();
    v.delete_account(acc).unwrap();
    assert!(live(&v).is_empty());
    crate::connectors::store_fetch(&v, ConnectorKind::Comdirect, c, fetch()).unwrap();
    assert!(live(&v).is_empty());
    // connection re-added + fetch: the cascade bookings come back
    v.add_bank_connection(ConnectorKind::Comdirect, String::new(), "login".into())
        .unwrap();
    crate::connectors::store_fetch(&v, ConnectorKind::Comdirect, c, fetch()).unwrap();
    assert_eq!(live(&v), vec!["b2", "b3"]);
    assert_eq!(
        n(
            &v,
            "SELECT deleted_ms FROM transactions WHERE external_id = 'b1'"
        ),
        5
    );
}

/// The demo vault plus what the demo lacks (bookings on the Tagesgeld,
/// a holding, splits, documents, budgets, an IBAN for transfer suggestions),
/// before anything is deleted.
fn soft_delete_world(v: &Vault) {
    v.seed_demo_data().unwrap();
    let db = v.db().unwrap();
    let id = |sql: &str| -> i64 { db.query_row(sql, [], |r| r.get(0)).unwrap() };
    let tagesgeld = id("SELECT id FROM accounts WHERE external_ref = 'demo-tagesgeld'");
    let depot = id("SELECT id FROM accounts WHERE external_ref = 'demo-depot'");
    let today = crate::dates::today();
    for (i, (ago, cents, cp, iban, purpose)) in [
        (70, 1250, "Musterbank", "", "Zinsgutschrift"),
        (40, 1250, "Musterbank", "", "Zinsgutschrift"),
        (10, 1250, "Musterbank", "", "Zinsgutschrift"),
        (
            15,
            -20000,
            "Max Muster",
            "DE02120300000000202051",
            "Umbuchung",
        ),
        (
            5,
            380000,
            "TechNova GmbH",
            "DE75512108001245126199",
            "Gehalt Bonus",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let date = crate::dates::iso(today - crate::dates::days(ago));
        db.execute(
            "INSERT INTO transactions(account_id, external_id, booking_date, value_date, amount_cents,
                                      counterparty, counterparty_iban, purpose, booking_text)
             VALUES (?1, ?2, ?3, ?3, ?4, ?5, ?6, ?7, 'GUTSCHRIFT')",
            params![tagesgeld, format!("tg-{i}"), date, cents, cp, iban, purpose],
        )
        .unwrap();
    }
    for acc in [tagesgeld, depot] {
        db.execute(
            "INSERT INTO holdings(account_id, isin, name, quantity, current_price_cents, value_cents)
             VALUES (?1, 'IE00B4L5Y983', 'MSCI World', 10, 9000, 90000)",
            [acc],
        )
        .unwrap();
    }
    db.execute(
        "UPDATE transactions SET counterparty_iban = 'DE89370400440532013000'
         WHERE counterparty = 'Autobank AG'",
        [],
    )
    .unwrap();
    // the tax refund on the giro counts as salary only through the Tagesgeld
    // booking marked as salary (same payer IBAN)
    db.execute(
        "UPDATE transactions SET counterparty_iban = 'DE75512108001245126199'
         WHERE counterparty = 'Finanzamt Musterstadt'",
        [],
    )
    .unwrap();
    let bonus = id("SELECT id FROM transactions WHERE external_id = 'tg-4'");
    let autobank = id("SELECT max(id) FROM transactions WHERE counterparty = 'Autobank AG'");
    let rewe = id("SELECT min(id) FROM transactions WHERE counterparty = 'REWE Markt GmbH'");
    let interest = id("SELECT min(id) FROM transactions WHERE external_id = 'tg-0'");
    let lebensmittel = id("SELECT id FROM categories WHERE name = 'Lebensmittel'");
    let supermarkt = id("SELECT id FROM categories WHERE name = 'Supermarkt'");
    let wohnen = id("SELECT id FROM categories WHERE name = 'Wohnen & Energie'");
    drop(db);
    v.set_booking_salary_flag(bonus, Some(true)).unwrap();
    v.set_category_budget(lebensmittel, Some(400_00)).unwrap();
    v.set_category_budget(supermarkt, Some(250_00)).unwrap();
    v.set_category_budget(wohnen, Some(1500_00)).unwrap();
    let split = |cat: Option<i64>, cents: i64| crate::models::TransactionSplitInput {
        category_id: cat,
        amount_cents: cents,
        note: "Teil".into(),
    };
    v.set_transaction_splits(
        autobank,
        vec![split(Some(wohnen), -20000), split(None, -8900)],
    )
    .unwrap();
    v.set_transaction_splits(
        rewe,
        vec![split(Some(supermarkt), -3000), split(Some(wohnen), -1782)],
    )
    .unwrap();
    v.set_transaction_splits(interest, vec![split(Some(wohnen), 1000), split(None, 250)])
        .unwrap();
    let pdf = |s: &str| crate::documents::simple_pdf(&[s.to_string()]);
    v.attach_document_to_booking(autobank, "autobank.pdf".into(), pdf("Autobank Rate"))
        .unwrap();
    v.attach_document_to_booking(interest, "zinsen.pdf".into(), pdf("Zinsen"))
        .unwrap();
    v.attach_document_to_account(
        tagesgeld,
        "Kontoauszug".into(),
        "auszug.pdf".into(),
        pdf("Kontoauszug Tagesgeld"),
        "kontoauszug".into(),
    )
    .unwrap();
}

/// Deletes one booking (the newer Autobank rate), one connection (the Amex
/// with the Visa account and its bookings), two accounts (the Tagesgeld with
/// bookings, a holding, splits and documents; the manual car loan), one
/// category tree (Lebensmittel with its subcategories and rules) and one rule.
fn soft_delete_items(v: &Vault) {
    let id = |sql: &str| -> i64 { v.db().unwrap().query_row(sql, [], |r| r.get(0)).unwrap() };
    let autobank = id("SELECT max(id) FROM transactions WHERE counterparty = 'Autobank AG'");
    soft_delete_booking(&v.db().unwrap(), autobank).unwrap();
    v.delete_bank_connection(id(&format!(
        "SELECT id FROM bank_connections WHERE connector = '{}'",
        ConnectorKind::AmericanExpress.as_str()
    )))
    .unwrap();
    for r in ["demo-tagesgeld", "demo-autokredit"] {
        let a = id(&format!(
            "SELECT id FROM accounts WHERE external_ref = '{r}' OR (name = 'Autokredit' AND '{r}' = 'demo-autokredit')"
        ));
        v.set_account_archived(a, true).unwrap();
        v.delete_account(a).unwrap();
    }
    v.delete_category(id("SELECT id FROM categories WHERE name = 'Lebensmittel'"))
        .unwrap();
    v.delete_rule(id(
        "SELECT r.id FROM rules r JOIN categories c ON c.id = r.category_id
         WHERE c.name = 'Internet & Telefon' ORDER BY r.id LIMIT 1",
    ))
    .unwrap();
}

/// Collects differences between the vault with soft-deleted rows and the one
/// where those rows never existed.
struct Mismatches(Vec<String>);

impl Mismatches {
    fn cmp<T: std::fmt::Debug>(&mut self, api: &str, a: T, b: T) {
        let (a, b) = (format!("{a:?}"), format!("{b:?}"));
        if a != b {
            // show the neighbourhood of the first difference
            let at = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
            let cut = |s: &str| {
                let start = s.floor_char_boundary(at.saturating_sub(200));
                let end = s.floor_char_boundary((at + 200).min(s.len()));
                s[start..end].to_string()
            };
            self.0
                .push(format!("{api}:\n  soft: {}\n  gone: {}", cut(&a), cut(&b)));
        }
    }
}

fn files_under(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p.strip_prefix(dir).unwrap().to_string_lossy().into_owned());
            }
        }
    }
    out.sort();
    out
}

/// A vault with soft-deleted rows (`soft_delete_items`) answers every public
/// read API exactly like a copy where those rows were removed for good (the
/// old hard delete with its foreign-key cascades). APIs covered, each called
/// with every account / booking / connection / category / document id that
/// existed before the deletions where it takes one:
///
/// - vault: `list_bank_connections`, `list_accounts`, `list_account_groups`,
///   `list_holdings`, `balance_history`, `list_categories` (all time and a
///   date range), `list_transactions` (all, per account, per category with and
///   without subcategories, uncategorised, search, everyday/investment scope,
///   merchant), `list_rules`, `preview_rule`, `apply_rules` (count and the
///   bookings afterwards), `list_settings`, `export_bookings_csv`, `is_empty`
///   and `account_count` (also on a vault whose only rows are deleted)
/// - connections: `connection_status`
/// - planning: `list_category_budget_progress` (this and last month),
///   `list_transaction_splits`, `cashflow_by_period` (month, quarter, year),
///   `list_transfer_templates`
/// - recurring / salary: `list_recurring`, `recurring_month`,
///   `list_ignored_recurring`, `salary_cycle`
/// - loans: `list_manual_loans`, `preview_manual_loan`, `list_loan_links`
/// - insights: `merchant_stats` (every merchant key), `investment_dashboard`
/// - user groups: `list_user_groups`
/// - documents: `list_documents_for_booking`, `list_all_documents` (all, per
///   account, unlinked, search), `get_document_bytes`, `export_all_documents`,
///   `mirror_new_documents`, `auto_match_document` (text that fits the deleted
///   booking best)
/// - transfers / statements: `suggest_recipients`, `list_known_recipients`,
///   `auto_statements_enabled`
/// - imports: `preview_moneymoney_import` (rows unrelated to the deleted
///   bookings; rows that hit a deleted booking are kept from resurrecting it on
///   purpose, see Task 4)
///
/// Not compared: `preview_moneymoney_database` (its read of "no bookings yet"
/// is covered by `moneymoney_db::tests::deleted_bookings_do_not_count_as_existing`),
/// `preview_portal_capture` (only dedup by key, which must see deleted rows),
/// the placement written by `move_account_to_group` (see
/// `moving_an_account_ignores_deleted_neighbours`), and the APIs that only
/// write or return no stored data (setters, imports, settings getters,
/// catalogues, parsers, connector sessions, logos).
#[test]
fn soft_deleted_rows_appear_in_no_public_api() {
    use crate::models::{BookingFilter, BookingScope, PeriodGranularity};
    let dir = temp_dir("invisible");
    let a = open(&dir, "soft.db");
    soft_delete_world(&a);
    a.checkpoint().unwrap();
    std::fs::copy(dir.join("soft.db"), dir.join("gone.db")).unwrap();
    let b = open(&dir, "gone.db");

    // every id that existed before the deletions
    let all = |sql: &str| -> Vec<i64> {
        let db = a.db().unwrap();
        let mut st = db.prepare(sql).unwrap();
        st.query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    let accounts = all("SELECT id FROM accounts ORDER BY id");
    let bookings = all("SELECT id FROM transactions ORDER BY id");
    let connections = all("SELECT id FROM bank_connections ORDER BY id");
    let categories = all("SELECT id FROM categories ORDER BY id");
    let documents = all("SELECT id FROM booking_documents ORDER BY id");
    let merchant_keys: Vec<String> = {
        let mut k: Vec<String> = a
            .list_transactions(BookingFilter::default())
            .unwrap()
            .transactions
            .into_iter()
            .map(|t| t.merchant.key)
            .collect();
        k.sort();
        k.dedup();
        k
    };

    soft_delete_items(&a);
    soft_delete_items(&b);
    b.db()
        .unwrap()
        .execute_batch(
            "DELETE FROM transactions WHERE deleted_ms IS NOT NULL;
             DELETE FROM rules WHERE deleted_ms IS NOT NULL;
             DELETE FROM accounts WHERE deleted_ms IS NOT NULL;
             DELETE FROM bank_connections WHERE deleted_ms IS NOT NULL;
             DELETE FROM categories WHERE deleted_ms IS NOT NULL;",
        )
        .unwrap();
    assert!(n(&a, "SELECT count(*) FROM transactions WHERE deleted_ms > 0") > 10);

    let mut m = Mismatches(Vec::new());
    macro_rules! both {
        ($label:expr, |$v:ident| $call:expr) => {{
            let r_a = {
                let $v = &a;
                $call
            };
            let r_b = {
                let $v = &b;
                $call
            };
            m.cmp(&$label, r_a, r_b);
        }};
    }

    both!("list_bank_connections", |v| v.list_bank_connections());
    both!("list_accounts", |v| v.list_accounts());
    both!("list_account_groups", |v| v.list_account_groups());
    both!("list_user_groups", |v| v.list_user_groups());
    both!("account_count", |v| v.account_count());
    both!("is_empty", |v| v.is_empty());
    both!("list_rules", |v| v.list_rules());
    both!("list_settings", |v| v.list_settings());
    both!("list_categories", |v| v.list_categories(None, None));
    let today = crate::dates::today();
    let ago = |d: i64| crate::dates::iso(today - crate::dates::days(d));
    both!("list_categories(range)", |v| v
        .list_categories(Some(ago(40)), Some(ago(0))));
    both!("balance_history(all)", |v| v.balance_history(vec![], 120));
    both!("list_manual_loans", |v| v.list_manual_loans());
    both!("list_loan_links", |v| v.list_loan_links());
    both!("list_transfer_templates", |v| v.list_transfer_templates());
    both!("investment_dashboard", |v| v.investment_dashboard(6));
    both!("list_recurring(all)", |v| v.list_recurring(vec![]));
    both!("list_ignored_recurring", |v| v
        .list_ignored_recurring(vec![]));
    both!("salary_cycle(all)", |v| v.salary_cycle(vec![]));
    let (year, month) = (i32::from(today.year()), today.month() as u32);
    for (y, mo) in [
        (year, month),
        if month == 1 {
            (year - 1, 12)
        } else {
            (year, month - 1)
        },
    ] {
        both!(format!("list_category_budget_progress({y}-{mo})"), |v| v
            .list_category_budget_progress(y, mo));
        both!(format!("recurring_month({y}-{mo})"), |v| v.recurring_month(
            vec![],
            y,
            mo
        ));
    }
    for g in [
        PeriodGranularity::Month,
        PeriodGranularity::Quarter,
        PeriodGranularity::Year,
    ] {
        both!(format!("cashflow_by_period({g:?})"), |v| v
            .cashflow_by_period(g, 6, None, true));
    }
    both!("suggest_recipients", |v| v
        .suggest_recipients(String::new(), 20));
    both!("suggest_recipients(query)", |v| v
        .suggest_recipients("Autobank".into(), 20));
    both!("list_known_recipients", |v| v
        .list_known_recipients(String::new()));

    for &id in &accounts {
        both!(format!("list_holdings({id})"), |v| v.list_holdings(id));
        both!(format!("balance_history({id})"), |v| v
            .balance_history(vec![id], 120));
        both!(format!("list_recurring({id})"), |v| v
            .list_recurring(vec![id]));
        both!(format!("salary_cycle({id})"), |v| v.salary_cycle(vec![id]));
        both!(format!("auto_statements_enabled({id})"), |v| v
            .auto_statements_enabled(id));
        both!(format!("list_transactions(account {id})"), |v| v
            .list_transactions(BookingFilter {
                account_ids: vec![id],
                ..Default::default()
            }));
        both!(format!("list_all_documents(account {id})"), |v| v
            .list_all_documents(crate::documents::DocumentFilter {
                account_id: Some(id),
                doc_type: None,
                search: None,
                unlinked_only: false,
                connection_id: None,
            }));
        both!(format!("preview_manual_loan({id})"), |v| v
            .preview_manual_loan(
                Some(id),
                crate::loans::LoanSetup {
                    name: "Autokredit".into(),
                    original_cents: Some(15_000_00),
                    anchor_date: ago(60),
                    anchor_balance_cents: 9_400_00,
                    interest_rate_ppm: Some(49_900),
                    match_expr: "Autobank".into(),
                },
                12
            ));
    }
    for &id in &bookings {
        both!(format!("list_transaction_splits({id})"), |v| v
            .list_transaction_splits(id));
        both!(format!("list_documents_for_booking({id})"), |v| v
            .list_documents_for_booking(id));
    }
    for &id in &connections {
        both!(format!("connection_status({id})"), |v| v
            .connection_status(id));
    }
    for &id in &categories {
        for sub in [false, true] {
            both!(
                format!("list_transactions(category {id}, sub {sub})"),
                |v| v.list_transactions(BookingFilter {
                    category_id: Some(id),
                    include_subcategories: sub,
                    ..Default::default()
                })
            );
        }
    }
    for &id in &documents {
        both!(format!("get_document_bytes({id})"), |v| v
            .get_document_bytes(id));
    }
    for key in &merchant_keys {
        both!(format!("merchant_stats({key})"), |v| v
            .merchant_stats(key.clone()));
        both!(format!("list_transactions(merchant {key})"), |v| v
            .list_transactions(BookingFilter {
                merchant_key: Some(key.clone()),
                ..Default::default()
            }));
    }
    let filters = [
        BookingFilter::default(),
        BookingFilter {
            uncategorized_only: true,
            ..Default::default()
        },
        BookingFilter {
            search: Some("Autobank".into()),
            ..Default::default()
        },
        BookingFilter {
            search: Some("Musterbank".into()),
            ..Default::default()
        },
        BookingFilter {
            scope: BookingScope::Everyday,
            date_from: Some(ago(30)),
            ..Default::default()
        },
        BookingFilter {
            scope: BookingScope::Investments,
            ..Default::default()
        },
    ];
    for (i, f) in filters.iter().enumerate() {
        both!(format!("list_transactions(filter {i})"), |v| v
            .list_transactions(f.clone()));
        both!(format!("export_bookings_csv(filter {i})"), |v| v
            .export_bookings_csv(f.clone()));
    }
    for (i, f) in [
        crate::documents::DocumentFilter {
            account_id: None,
            doc_type: None,
            search: None,
            unlinked_only: false,
            connection_id: None,
        },
        crate::documents::DocumentFilter {
            account_id: None,
            doc_type: None,
            search: None,
            unlinked_only: true,
            connection_id: None,
        },
        crate::documents::DocumentFilter {
            account_id: None,
            doc_type: None,
            search: Some("Tagesgeld".into()),
            unlinked_only: false,
            connection_id: None,
        },
    ]
    .into_iter()
    .enumerate()
    {
        both!(format!("list_all_documents(filter {i})"), |v| v
            .list_all_documents(f.clone()));
    }
    let any_rule = |pattern: &str| crate::models::Rule {
        id: 0,
        name: "x".into(),
        category_id: n(&b, "SELECT min(id) FROM categories"),
        field: crate::models::RuleField::Any,
        match_kind: crate::models::MatchKind::Keyword,
        pattern: pattern.into(),
        case_sensitive: false,
        min_amount_cents: None,
        max_amount_cents: None,
        account_ids: Vec::new(),
        priority: 100,
        enabled: true,
        match_count: 0,
        tag_ids: Vec::new(),
    };
    for p in ["Autobank", "Musterbank", "a"] {
        both!(format!("preview_rule({p})"), |v| v
            .preview_rule(any_rule(p), 500));
    }
    let mm = "\"Datum\";\"Wertstellung\";\"Kategorie\";\"Name\";\"Verwendungszweck\";\"Konto\";\"Bank\";\"Betrag\";\"Währung\";\"Notiz\"\n\
\"15.03.2012\";\"15.03.2012\";\"Haushalt\\Supermarkt\";\"REWE Markt\";\"Einkauf\";\"\";\"\";\"-12,34\";\"EUR\";\"\"\n";
    let giro = n(
        &b,
        "SELECT id FROM accounts WHERE external_ref = 'demo-giro'",
    );
    for target in [None, Some(giro)] {
        both!(format!("preview_moneymoney_import({target:?})"), |v| v
            .preview_moneymoney_import(
                mm.as_bytes().to_vec(),
                "x.csv".into(),
                target
            ));
    }
    both!("auto_match_document", |v| v.auto_match_document(
        "autobank.pdf".into(),
        Vec::new(),
        Some(format!(
            "Autobank AG Rate Kreditvertrag 289,00 EUR {}",
            ago(24)
        ))
    ));
    for (api, run) in [("export_all_documents", 0), ("mirror_new_documents", 1)] {
        let out = |tag: &str| {
            let d = dir.join(format!("{api}-{tag}"));
            std::fs::create_dir_all(&d).unwrap();
            d
        };
        let (da, db) = (out("soft"), out("gone"));
        let call = |v: &Vault, d: &std::path::Path| {
            let d = d.to_string_lossy().into_owned();
            if run == 0 {
                v.export_all_documents(d)
            } else {
                v.mirror_new_documents(d)
            }
        };
        m.cmp(api, call(&a, &da), call(&b, &db));
        m.cmp(&format!("{api} files"), files_under(&da), files_under(&db));
    }
    // apply last: it writes
    both!("apply_rules", |v| v.apply_rules(false));
    both!("list_transactions after apply_rules", |v| v
        .list_transactions(BookingFilter::default()));

    // a vault whose only rows are deleted is empty
    let e = open(&dir, "empty.db");
    let cat = e
        .create_category("X".into(), None, String::new(), String::new())
        .unwrap();
    let conn = e
        .add_bank_connection(ConnectorKind::Comdirect, String::new(), "login".into())
        .unwrap();
    e.db()
        .unwrap()
        .execute_batch(&format!(
            "INSERT INTO account_groups(id, name) VALUES (1, 'G');
             INSERT INTO accounts(connection_id, group_id, external_ref, name) VALUES ({conn}, 1, 'A', 'A');"
        ))
        .unwrap();
    e.delete_category(cat).unwrap();
    e.delete_bank_connection(conn).unwrap();
    m.cmp("is_empty(only deleted rows)", e.is_empty(), Ok(true));
    m.cmp("account_count(only deleted rows)", e.account_count(), Ok(0));

    assert!(
        m.0.is_empty(),
        "{} APIs show soft-deleted rows:\n{}",
        m.0.len(),
        m.0.join("\n")
    );
}

/// The drop index of a move counts the accounts the user sees, so a deleted
/// account in the target group must not shift it.
#[test]
fn moving_an_account_ignores_deleted_neighbours() {
    let v = open(&temp_dir("moveidx"), "a.db");
    let (_, a1, a2, loose) = bank_fixture(&v);
    let g = v.create_user_group("G".into(), "folder".into()).unwrap();
    v.move_account_to_group(a1, Some(g.clone()), None).unwrap();
    v.move_account_to_group(a2, Some(g.clone()), None).unwrap();
    v.set_account_archived(a1, true).unwrap();
    v.delete_account(a1).unwrap();
    // visible group: [a2]; index 1 = after a2
    v.move_account_to_group(loose, Some(g.clone()), Some(1))
        .unwrap();
    let group = v
        .list_user_groups()
        .unwrap()
        .into_iter()
        .find(|x| x.uuid == g)
        .unwrap();
    let ids: Vec<i64> = group.accounts.iter().map(|a| a.id).collect();
    assert_eq!(ids, [a2, loose]);
}

// ------------------------------------------------------------ category / rule merge

fn merge(into: &Vault, from: &Vault) -> crate::vault::MergeStats {
    into.merge_other_vault(from.file_path(), vec![7u8; 32])
        .unwrap()
}

/// Merges both ways and checks that the vaults agree.
fn both_ways(a: &Vault, b: &Vault) {
    merge(a, b);
    merge(b, a);
    assert_converged(a, b);
}

fn text(v: &Vault, sql: &str) -> String {
    v.db().unwrap().query_row(sql, [], |r| r.get(0)).unwrap()
}

/// Same account group and account on both vaults (bookings need one).
fn shared_account(v: &Vault) {
    v.db()
        .unwrap()
        .execute_batch(
            "INSERT INTO account_groups(id, name) VALUES (1, 'G');
             INSERT INTO accounts(id, group_id, external_ref, name) VALUES (1, 1, 'DE1', 'A');",
        )
        .unwrap();
}

/// A creates "Food" with old stamps, B gets it by a merge.
fn shared_food(tag: &str) -> (Arc<Vault>, Arc<Vault>, i64, i64) {
    let dir = temp_dir(tag);
    let (a, b) = (open(&dir, "a.db"), open(&dir, "b.db"));
    let id_a = a
        .create_category("Food".into(), None, "tag".into(), "#111111".into())
        .unwrap();
    a.db()
        .unwrap()
        .execute(
            "UPDATE categories SET name_ms=1, parent_ms=1, symbol_ms=1, color_ms=1, budget_ms=1, order_ms=1",
            [],
        )
        .unwrap();
    merge(&b, &a);
    let id_b = n(&b, "SELECT id FROM categories WHERE name = 'Food'");
    (a, b, id_a, id_b)
}

#[test]
fn rename_on_a_and_color_on_b_both_survive() {
    let (a, b, id_a, id_b) = shared_food("cat-fields");
    a.update_category(id_a, "Essen".into(), None, "tag".into(), "#111111".into())
        .unwrap();
    b.update_category(id_b, "Food".into(), None, "tag".into(), "#222222".into())
        .unwrap();
    both_ways(&a, &b);
    for v in [&a, &b] {
        assert_eq!(
            text(v, "SELECT name || ' ' || color_hex FROM categories"),
            "Essen #222222"
        );
        assert_eq!(n(v, "SELECT count(*) FROM categories"), 1);
    }
}

#[test]
fn newer_rename_wins_in_both_merge_orders() {
    for a_first in [true, false] {
        let (a, b, id_a, id_b) = shared_food("cat-newer");
        // the newer rename has the smaller value, so a tie-break can't fake it
        a.db()
            .unwrap()
            .execute(
                "UPDATE categories SET name = 'Zzz', name_ms = 100 WHERE id = ?1",
                [id_a],
            )
            .unwrap();
        b.db()
            .unwrap()
            .execute(
                "UPDATE categories SET name = 'Aaa', name_ms = 200 WHERE id = ?1",
                [id_b],
            )
            .unwrap();
        // equal stamps, different values: the larger value wins
        a.db()
            .unwrap()
            .execute(
                "UPDATE categories SET color_hex = '#aaaaaa', color_ms = 50",
                [],
            )
            .unwrap();
        b.db()
            .unwrap()
            .execute(
                "UPDATE categories SET color_hex = '#bbbbbb', color_ms = 50",
                [],
            )
            .unwrap();
        if a_first {
            both_ways(&a, &b);
        } else {
            both_ways(&b, &a);
        }
        for v in [&a, &b] {
            assert_eq!(
                text(v, "SELECT name || ' ' || color_hex FROM categories"),
                "Aaa #bbbbbb"
            );
        }
    }
}

#[test]
fn deleted_category_stays_deleted_against_older_edit() {
    let (a, b, id_a, id_b) = shared_food("cat-del");
    for v in [&a, &b] {
        shared_account(v);
        let cat = n(v, "SELECT id FROM categories WHERE name = 'Food'");
        v.db()
            .unwrap()
            .execute(
                "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents,
                     category_id, category_source, category_ms)
                 VALUES (1, 'e1', '2026-01-01', -5, ?1, 'manual', 7)",
                [cat],
            )
            .unwrap();
    }
    // B: a rule and a child on Food, and an edit older than A's deletion
    b.save_rule(plain_rule(id_b, "lidl")).unwrap();
    b.create_category("Bio".into(), Some(id_b), "tag".into(), "#111111".into())
        .unwrap();
    b.update_category(id_b, "Essen".into(), None, "tag".into(), "#111111".into())
        .unwrap();
    b.db()
        .unwrap()
        .execute("UPDATE categories SET name_ms = 2 WHERE id = ?1", [id_b])
        .unwrap();
    a.delete_category(id_a).unwrap();
    both_ways(&a, &b);
    // a later edit loses as well: delete wins
    b.db()
        .unwrap()
        .execute(
            "UPDATE categories SET name = 'Later', name_ms = 9999999999999 WHERE id = ?1",
            [id_b],
        )
        .unwrap();
    both_ways(&a, &b);
    for v in [&a, &b] {
        assert_eq!(
            n(
                v,
                "SELECT count(*) FROM categories WHERE deleted_ms IS NULL AND name != 'Bio'"
            ),
            0
        );
        // the child moved to the top level, the rule went with its category
        assert_eq!(
            n(
                v,
                "SELECT count(*) FROM categories WHERE name = 'Bio' AND parent_id IS NULL AND deleted_ms IS NULL"
            ),
            1
        );
        assert_eq!(
            n(v, "SELECT count(*) FROM rules WHERE deleted_ms IS NULL"),
            0
        );
        assert_eq!(
            n(
                v,
                "SELECT count(*) FROM transactions WHERE category_id IS NULL AND category_source = 'none'"
            ),
            1
        );
        assert!(v.list_categories(None, None).unwrap().len() == 1);
    }
}

#[test]
fn same_import_on_two_vaults_has_no_duplicate_categories() {
    let dir = temp_dir("cat-import");
    let (a, b) = (open(&dir, "a.db"), open(&dir, "b.db"));
    for v in [&a, &b] {
        v.seed_starter_categories().unwrap();
        v.create_category("Hobby".into(), None, "tag".into(), "#111111".into())
            .unwrap();
    }
    let cats = n(&a, "SELECT count(*) FROM categories");
    let rules = n(&a, "SELECT count(*) FROM rules");
    assert!(cats > 1 && rules > 0);
    both_ways(&a, &b);
    for v in [&a, &b] {
        assert_eq!(n(v, "SELECT count(*) FROM categories"), cats);
        assert_eq!(n(v, "SELECT count(*) FROM rules"), rules);
        assert_eq!(
            n(
                v,
                "SELECT count(*) FROM categories WHERE deleted_ms IS NOT NULL"
            ),
            0
        );
    }
}

#[test]
fn independently_created_same_name_categories_are_combined() {
    let dir = temp_dir("cat-combine");
    let (a, b) = (open(&dir, "a.db"), open(&dir, "b.db"));
    for (v, name, food, bio, rule) in [
        (&a, "Food", "aaaa", "cccc", "r-1"),
        (&b, "food", "bbbb", "dddd", "r-2"),
    ] {
        shared_account(v);
        let f = v
            .create_category(name.into(), None, "tag".into(), "#111111".into())
            .unwrap();
        let c = v
            .create_category("Bio".into(), Some(f), "tag".into(), "#111111".into())
            .unwrap();
        let r = v.save_rule(plain_rule(f, "lidl")).unwrap();
        let db = v.db().unwrap();
        db.execute(
            "UPDATE categories SET uuid = ?1 WHERE id = ?2",
            params![food, f],
        )
        .unwrap();
        db.execute(
            "UPDATE categories SET uuid = ?1 WHERE id = ?2",
            params![bio, c],
        )
        .unwrap();
        db.execute(
            "UPDATE rules SET uuid = ?1, updated_ms = 5 WHERE id = ?2",
            params![rule, r],
        )
        .unwrap();
        db.execute(
            "INSERT INTO transactions(id, account_id, external_id, booking_date, amount_cents,
                 category_id, category_source, category_ms)
             VALUES (1, 1, 'e1', '2026-01-01', -5, ?1, 'manual', 7)",
            [f],
        )
        .unwrap();
        db.execute(
            "INSERT INTO transaction_splits(transaction_id, category_id, amount_cents) VALUES (1, ?1, -5)",
            [c],
        )
        .unwrap();
    }
    let stamp = n(&b, "SELECT name_ms FROM categories WHERE uuid = 'bbbb'")
        .max(n(&a, "SELECT name_ms FROM categories WHERE uuid = 'aaaa'"));
    both_ways(&a, &b);
    for v in [&a, &b] {
        assert_eq!(
            uuids(
                v,
                "SELECT uuid FROM categories WHERE deleted_ms IS NULL ORDER BY uuid"
            ),
            vec!["aaaa", "cccc"]
        );
        assert_eq!(
            text(v, "SELECT merged_into FROM categories WHERE uuid = 'bbbb'"),
            "aaaa"
        );
        assert_eq!(
            n(v, "SELECT deleted_ms FROM categories WHERE uuid = 'bbbb'"),
            stamp
        );
        assert_eq!(
            text(v, "SELECT merged_into FROM categories WHERE uuid = 'dddd'"),
            "cccc"
        );
        // bookings, splits, rules and children follow the survivor
        assert_eq!(
            text(
                v,
                "SELECT c.uuid FROM transactions t JOIN categories c ON c.id = t.category_id"
            ),
            "aaaa"
        );
        assert_eq!(
            text(
                v,
                "SELECT c.uuid FROM transaction_splits s JOIN categories c ON c.id = s.category_id"
            ),
            "cccc"
        );
        assert_eq!(
            uuids(v, "SELECT uuid FROM rules WHERE deleted_ms IS NULL"),
            vec!["r-1"]
        );
        assert_eq!(
            text(v, "SELECT merged_into FROM rules WHERE uuid = 'r-2'"),
            "r-1"
        );
        assert_eq!(
            text(
                v,
                "SELECT c.uuid FROM rules r JOIN categories c ON c.id = r.category_id WHERE r.uuid = 'r-2'"
            ),
            "aaaa"
        );
    }
}

#[test]
fn rule_edit_newest_wins_and_deleted_rule_stays_deleted() {
    let dir = temp_dir("rule-merge");
    let (a, b) = (open(&dir, "a.db"), open(&dir, "b.db"));
    let cat = a
        .create_category("Food".into(), None, "tag".into(), "#111111".into())
        .unwrap();
    let edited = a.save_rule(plain_rule(cat, "lidl")).unwrap();
    let doomed = a.save_rule(plain_rule(cat, "aldi")).unwrap();
    a.db()
        .unwrap()
        .execute("UPDATE rules SET updated_ms = 1", [])
        .unwrap();
    merge(&b, &a);
    let id = |v: &Vault, p: &str| n(v, &format!("SELECT id FROM rules WHERE pattern = '{p}'"));
    let (b_edited, b_doomed) = (id(&b, "lidl"), id(&b, "aldi"));
    let b_cat = n(&b, "SELECT id FROM categories WHERE name = 'Food'");
    // A edits first (older), B later; the newer one has the smaller value
    let mut r = plain_rule(cat, "zzz");
    r.id = edited;
    a.save_rule(r).unwrap();
    a.db()
        .unwrap()
        .execute("UPDATE rules SET updated_ms = 100 WHERE id = ?1", [edited])
        .unwrap();
    let mut r = plain_rule(b_cat, "aaa");
    r.id = b_edited;
    r.priority = 3;
    b.save_rule(r).unwrap();
    b.db()
        .unwrap()
        .execute(
            "UPDATE rules SET updated_ms = 200 WHERE id = ?1",
            [b_edited],
        )
        .unwrap();
    // A deletes the other rule, B edits it later: delete wins
    a.delete_rule(doomed).unwrap();
    let mut r = plain_rule(b_cat, "aldi süd");
    r.id = b_doomed;
    b.save_rule(r).unwrap();
    b.db()
        .unwrap()
        .execute(
            "UPDATE rules SET updated_ms = 9999999999999 WHERE id = ?1",
            [b_doomed],
        )
        .unwrap();
    both_ways(&a, &b);
    for v in [&a, &b] {
        let live: Vec<(String, i32)> = v
            .list_rules()
            .unwrap()
            .into_iter()
            .map(|r| (r.pattern, r.priority))
            .collect();
        assert_eq!(live, vec![("aaa".to_string(), 3)]);
        assert_eq!(n(v, "SELECT count(*) FROM rules"), 2);
    }
}

#[test]
fn v7_snapshot_cannot_revive_deleted_category() {
    let dir = temp_dir("v7-revive");
    let a = open(&dir, "a.db");
    // A created "Essen", renamed it "Food" (uuid stays the "Essen" path uuid)
    let food = a
        .create_category("Essen".into(), None, "tag".into(), "#111111".into())
        .unwrap();
    a.update_category(food, "Food".into(), None, "tag".into(), "#111111".into())
        .unwrap();
    a.save_rule(plain_rule(food, "lidl")).unwrap();
    // an old (v7) device got "Food" and its rule before A deleted them
    let path = dir.join("old.db").to_string_lossy().to_string();
    {
        let conn = Connection::open(&path).unwrap();
        Vault::apply_key(&conn, &[7u8; 32]).unwrap();
        conn.execute_batch(crate::vault::SCHEMA).unwrap();
        crate::user_groups::migrate(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO categories(id, name, color_hex) VALUES (1, 'Food', '#111111');
             INSERT INTO rules(name, category_id, pattern, priority) VALUES ('r', 1, 'lidl', 10);
             PRAGMA user_version = 7;",
        )
        .unwrap();
    }
    a.delete_category(food).unwrap();
    a.merge_other_vault(path.clone(), vec![7u8; 32]).unwrap();
    assert_eq!(
        n(
            &a,
            "SELECT count(*) FROM categories WHERE deleted_ms IS NULL"
        ),
        0
    );
    assert_eq!(
        n(&a, "SELECT count(*) FROM rules WHERE deleted_ms IS NULL"),
        0
    );
    // the old device updates and syncs: nothing comes back on either side
    let b = Vault::open(path, vec![7u8; 32]).unwrap();
    both_ways(&a, &b);
    for v in [&a, &b] {
        assert!(v.list_categories(None, None).unwrap().is_empty());
        assert!(v.list_rules().unwrap().is_empty());
    }
}

/// X and Y at the top level on A, B and a third vault C (pre-move state);
/// Y has the lower uuid, so cutting the cycle by uuid alone would drop the
/// newer move.
fn crossing_setup(tag: &str) -> (Arc<Vault>, Arc<Vault>, Arc<Vault>) {
    let dir = temp_dir(tag);
    let (a, b, c) = (open(&dir, "a.db"), open(&dir, "b.db"), open(&dir, "c.db"));
    for (name, uuid) in [("X", "bbbb"), ("Y", "aaaa")] {
        let id = a
            .create_category(name.into(), None, "tag".into(), "#111111".into())
            .unwrap();
        a.db()
            .unwrap()
            .execute(
                "UPDATE categories SET uuid = ?1, name_ms=1, parent_ms=1, symbol_ms=1, color_ms=1,
                        budget_ms=1, order_ms=1 WHERE id = ?2",
                params![uuid, id],
            )
            .unwrap();
    }
    merge(&b, &a);
    merge(&c, &a);
    let id = |v: &Vault, name: &str| {
        n(
            v,
            &format!("SELECT id FROM categories WHERE name = '{name}'"),
        )
    };
    // A moves X under Y (older), B moves Y under X (newer)
    a.update_category(
        id(&a, "X"),
        "X".into(),
        Some(id(&a, "Y")),
        "tag".into(),
        "#111111".into(),
    )
    .unwrap();
    a.db()
        .unwrap()
        .execute("UPDATE categories SET parent_ms = 100 WHERE name = 'X'", [])
        .unwrap();
    b.update_category(
        id(&b, "Y"),
        "Y".into(),
        Some(id(&b, "X")),
        "tag".into(),
        "#111111".into(),
    )
    .unwrap();
    b.db()
        .unwrap()
        .execute("UPDATE categories SET parent_ms = 200 WHERE name = 'Y'", [])
        .unwrap();
    (a, b, c)
}

fn assert_newer_move_kept(v: &Vault) {
    assert_eq!(
        text(
            v,
            "SELECT c.name || '>' || IFNULL(p.name, '-') FROM categories c
             LEFT JOIN categories p ON p.id = c.parent_id ORDER BY c.name LIMIT 1"
        ),
        "X>-"
    );
    assert_eq!(
        text(
            v,
            "SELECT c.name || '>' || IFNULL(p.name, '-') FROM categories c
             LEFT JOIN categories p ON p.id = c.parent_id WHERE c.name = 'Y'"
        ),
        "Y>X"
    );
}

#[test]
fn crossing_moves_do_not_create_a_cycle() {
    for a_first in [true, false] {
        let (a, b, _) = crossing_setup("cycle");
        if a_first {
            both_ways(&a, &b);
        } else {
            both_ways(&b, &a);
        }
        assert_newer_move_kept(&a);
        assert_newer_move_kept(&b);
    }
    // a stale third copy with the pre-move state does not bring the cycle back
    let (a, b, c) = crossing_setup("cycle3");
    both_ways(&a, &b);
    merge(&c, &a);
    merge(&a, &c);
    merge(&b, &c);
    assert_converged(&a, &c);
    assert_converged(&b, &c);
    for v in [&a, &b, &c] {
        assert_newer_move_kept(v);
    }
    // and the other way round: the stale copy merges each mover first
    let (a, b, c) = crossing_setup("cycle3b");
    merge(&c, &a);
    merge(&c, &b);
    merge(&a, &c);
    merge(&b, &c);
    assert_converged(&a, &c);
    assert_converged(&b, &c);
    for v in [&a, &b, &c] {
        assert_newer_move_kept(v);
    }
}

// ------------------------------------------------------------ booking merge

fn exec(v: &Vault, sql: &str) {
    v.db().unwrap().execute_batch(sql).unwrap();
}

fn cat(v: &Vault, name: &str) -> i64 {
    v.db()
        .unwrap()
        .query_row(
            "SELECT id FROM categories WHERE name = ?1 AND deleted_ms IS NULL",
            [name],
            |r| r.get(0),
        )
        .unwrap()
}

/// `<category name>/<source>` of booking `id` ("-" for none).
fn bk_cat(v: &Vault, id: i64) -> String {
    text(
        v,
        &format!(
            "SELECT IFNULL(c.name, '-') || '/' || t.category_source FROM transactions t
             LEFT JOIN categories c ON c.id = t.category_id WHERE t.id = {id}"
        ),
    )
}

/// A and B share account 1 with bookings 1 ("e1", Shop) and 2 ("e2", Kiosk)
/// and the categories Food and Fun (A created them, B got them by a merge).
fn booking_pair(tag: &str) -> (Arc<Vault>, Arc<Vault>) {
    let dir = temp_dir(tag);
    let (a, b) = (open(&dir, "a.db"), open(&dir, "b.db"));
    for v in [&a, &b] {
        shared_account(v);
        exec(
            v,
            "INSERT INTO transactions(id, account_id, external_id, booking_date, amount_cents, counterparty)
                 VALUES (1, 1, 'e1', '2026-01-01', -500, 'Shop'), (2, 1, 'e2', '2026-01-02', -300, 'Kiosk');",
        );
    }
    a.create_category("Food".into(), None, String::new(), String::new())
        .unwrap();
    a.create_category("Fun".into(), None, String::new(), String::new())
        .unwrap();
    merge(&b, &a);
    (a, b)
}

/// Runs `setup` on a fresh pair, merges both ways (in both orders) and hands
/// both vaults to `check`.
fn each_order(tag: &str, setup: impl Fn(&Vault, &Vault), check: impl Fn(&Vault)) {
    for a_first in [true, false] {
        let (a, b) = booking_pair(tag);
        setup(&a, &b);
        if a_first {
            both_ways(&a, &b);
        } else {
            both_ways(&b, &a);
        }
        check(&a);
        check(&b);
    }
}

fn split(category_id: Option<i64>, amount_cents: i64) -> crate::models::TransactionSplitInput {
    crate::models::TransactionSplitInput {
        category_id,
        amount_cents,
        note: String::new(),
    }
}

/// `<category name>:<amount>` of booking `id`'s splits, sorted.
fn splits_of(v: &Vault, id: i64) -> Vec<String> {
    let db = v.db().unwrap();
    let mut st = db
        .prepare(
            "SELECT IFNULL(c.name, '-') || ':' || s.amount_cents FROM transaction_splits s
             LEFT JOIN categories c ON c.id = s.category_id WHERE s.transaction_id = ?1",
        )
        .unwrap();
    let mut out: Vec<String> = st
        .query_map([id], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    out.sort();
    out
}

#[test]
fn newer_note_wins_including_empty() {
    each_order(
        "bk-note",
        |a, b| {
            a.update_transaction_note(1, "old".into()).unwrap();
            exec(a, "UPDATE transactions SET note_ms = 100 WHERE id = 1");
            // B clears the note later
            exec(
                b,
                "UPDATE transactions SET note = '', note_ms = 200 WHERE id = 1",
            );
            // and writes a note A changed later again
            exec(
                b,
                "UPDATE transactions SET note = 'bbbbbb', note_ms = 100 WHERE id = 2",
            );
            exec(
                a,
                "UPDATE transactions SET note = 'a', note_ms = 300 WHERE id = 2",
            );
        },
        |v| {
            assert_eq!(text(v, "SELECT note FROM transactions WHERE id = 1"), "");
            assert_eq!(text(v, "SELECT note FROM transactions WHERE id = 2"), "a");
        },
    );
    // the merge counts the bookings it changed
    let (a, b) = booking_pair("bk-note-count");
    exec(
        a.as_ref(),
        "UPDATE transactions SET note = 'x', note_ms = 5 WHERE id = 1",
    );
    assert_eq!(merge(&b, &a).bookings_updated, 1);
    assert_eq!(merge(&b, &a).bookings_updated, 0);
}

#[test]
fn newer_manual_category_beats_older_and_rule() {
    each_order(
        "bk-cat",
        |a, b| {
            a.set_transaction_category(1, Some(cat(a, "Food"))).unwrap();
            exec(a, "UPDATE transactions SET category_ms = 100 WHERE id = 1");
            b.set_transaction_category(1, Some(cat(b, "Fun"))).unwrap();
            exec(b, "UPDATE transactions SET category_ms = 200 WHERE id = 1");
            // booking 2: manual (stamped) on A, a rule result on B
            a.set_transaction_category(2, Some(cat(a, "Food"))).unwrap();
            exec(a, "UPDATE transactions SET category_ms = 100 WHERE id = 2");
            b.save_rule(plain_rule(cat(b, "Fun"), "Kiosk")).unwrap();
            b.apply_rules(false).unwrap();
            assert_eq!(bk_cat(b, 2), "Fun/rule");
        },
        |v| {
            assert_eq!(bk_cat(v, 1), "Fun/manual");
            assert_eq!(bk_cat(v, 2), "Food/manual");
        },
    );
}

#[test]
fn rule_category_never_overwrites_manual_in_either_direction() {
    each_order(
        "bk-rule",
        |a, b| {
            // unstamped manual values (imported before stamps existed)
            exec(
                a,
                &format!(
                    "UPDATE transactions SET category_id = {}, category_source = 'manual' WHERE id = 2",
                    cat(a, "Food")
                ),
            );
            exec(
                b,
                &format!(
                    "UPDATE transactions SET category_id = {}, category_source = 'manual' WHERE id = 1",
                    cat(b, "Food")
                ),
            );
            for v in [a, b] {
                v.save_rule(plain_rule(cat(v, "Fun"), "Shop")).unwrap();
                v.save_rule(plain_rule(cat(v, "Fun"), "Kiosk")).unwrap();
                v.apply_rules(true).unwrap();
            }
            assert_eq!(bk_cat(a, 1), "Fun/rule");
            assert_eq!(bk_cat(b, 2), "Fun/rule");
        },
        |v| {
            assert_eq!(bk_cat(v, 1), "Food/manual");
            assert_eq!(bk_cat(v, 2), "Food/manual");
        },
    );
}

#[test]
fn manual_clear_converges_with_rules_refilling() {
    each_order(
        "bk-clear",
        |a, b| {
            a.save_rule(plain_rule(cat(a, "Fun"), "Shop")).unwrap();
            merge(b, a);
            for v in [a, b] {
                for id in [1, 2] {
                    v.set_transaction_category(id, Some(cat(v, "Food")))
                        .unwrap();
                }
                exec(v, "UPDATE transactions SET category_ms = 100");
            }
            // A clears both later; its rule refills booking 1 (Shop)
            for id in [1, 2] {
                a.set_transaction_category(id, None).unwrap();
            }
            exec(a, "UPDATE transactions SET category_ms = 200");
            a.apply_rules(true).unwrap();
            assert_eq!(bk_cat(a, 1), "Fun/rule");
        },
        |v| {
            assert_eq!(bk_cat(v, 1), "Fun/rule");
            assert_eq!(bk_cat(v, 2), "-/none");
            assert_eq!(n(v, "SELECT min(category_ms) FROM transactions"), 200);
        },
    );
}

#[test]
fn unstamped_conflict_keeps_longer_note() {
    each_order(
        "bk-longer",
        |a, b| {
            exec(a, "UPDATE transactions SET note = 'kurz' WHERE id = 1");
            exec(
                b,
                "UPDATE transactions SET note = 'viel länger' WHERE id = 1",
            );
            // same length: the larger value
            exec(a, "UPDATE transactions SET note = 'abc' WHERE id = 2");
            exec(b, "UPDATE transactions SET note = 'abd' WHERE id = 2");
        },
        |v| {
            assert_eq!(
                text(v, "SELECT note FROM transactions WHERE id = 1"),
                "viel länger"
            );
            assert_eq!(text(v, "SELECT note FROM transactions WHERE id = 2"), "abd");
        },
    );
}

#[test]
fn checked_syncs_both_ways() {
    each_order(
        "bk-checked",
        |a, b| {
            a.set_transaction_checked(1, true).unwrap();
            exec(a, "UPDATE transactions SET checked_ms = 100 WHERE id = 1");
            for v in [a, b] {
                exec(
                    v,
                    "UPDATE transactions SET checked = 1, checked_ms = 100 WHERE id = 2",
                );
            }
            b.set_transaction_checked(2, false).unwrap();
            exec(b, "UPDATE transactions SET checked_ms = 200 WHERE id = 2");
        },
        |v| {
            assert_eq!(n(v, "SELECT checked FROM transactions WHERE id = 1"), 1);
            assert_eq!(n(v, "SELECT checked FROM transactions WHERE id = 2"), 0);
        },
    );
}

#[test]
fn unstamped_text_prefers_clean_over_broken_text() {
    each_order(
        "bk-text-clean",
        |a, b| {
            exec(
                a,
                "UPDATE transactions SET counterparty = 'Müller', purpose = 'Miete' WHERE id = 1",
            );
            exec(
                b,
                "UPDATE transactions SET counterparty = 'M\u{FFFD}ller', purpose = 'Miete' WHERE id = 1",
            );
            exec(
                a,
                "UPDATE transactions SET purpose = 'Geb\u{FFFD}hr' WHERE id = 2",
            );
            exec(b, "UPDATE transactions SET purpose = 'Gebühr' WHERE id = 2");
            for v in [a, b] {
                assert_eq!(n(v, "SELECT sum(text_ms) FROM transactions"), 0);
            }
        },
        |v| {
            assert_eq!(
                text(v, "SELECT counterparty FROM transactions WHERE id = 1"),
                "Müller"
            );
            assert_eq!(
                text(v, "SELECT purpose FROM transactions WHERE id = 2"),
                "Gebühr"
            );
        },
    );
}

#[test]
fn edited_text_syncs() {
    each_order(
        "bk-text",
        |a, b| {
            a.update_transaction_text(1, "Alpha".into(), "newer".into())
                .unwrap();
            exec(a, "UPDATE transactions SET text_ms = 200 WHERE id = 1");
            b.update_transaction_text(1, "Zulu".into(), "older".into())
                .unwrap();
            exec(b, "UPDATE transactions SET text_ms = 100 WHERE id = 1");
            b.update_transaction_text(2, "Kiosk 2".into(), "x".into())
                .unwrap();
        },
        |v| {
            assert_eq!(
                text(
                    v,
                    "SELECT counterparty || '/' || purpose FROM transactions WHERE id = 1"
                ),
                "Alpha/newer"
            );
            assert_eq!(
                text(
                    v,
                    "SELECT counterparty || '/' || purpose FROM transactions WHERE id = 2"
                ),
                "Kiosk 2/x"
            );
        },
    );
}

#[test]
fn newer_split_set_replaces_older() {
    each_order(
        "bk-splits",
        |a, b| {
            a.set_transaction_splits(
                1,
                vec![split(Some(cat(a, "Food")), -300), split(None, -200)],
            )
            .unwrap();
            exec(a, "UPDATE transactions SET splits_ms = 100 WHERE id = 1");
            b.set_transaction_splits(
                1,
                vec![
                    split(Some(cat(b, "Fun")), -250),
                    split(Some(cat(b, "Food")), -250),
                ],
            )
            .unwrap();
            exec(b, "UPDATE transactions SET splits_ms = 200 WHERE id = 1");
            // booking 2: only A splits it
            a.set_transaction_splits(2, vec![split(Some(cat(a, "Fun")), -100), split(None, -200)])
                .unwrap();
        },
        |v| {
            assert_eq!(splits_of(v, 1), ["Food:-250", "Fun:-250"]);
            assert_eq!(splits_of(v, 2), ["-:-200", "Fun:-100"]);
        },
    );
    // a newer removal of the split wins too
    each_order(
        "bk-splits-rm",
        |a, b| {
            for v in [a, b] {
                v.set_transaction_splits(1, vec![split(None, -300), split(None, -200)])
                    .unwrap();
                exec(v, "UPDATE transactions SET splits_ms = 100 WHERE id = 1");
            }
            b.set_transaction_splits(1, vec![]).unwrap();
        },
        |v| assert!(splits_of(v, 1).is_empty()),
    );
}

#[test]
fn deleted_booking_stays_deleted() {
    each_order(
        "bk-deleted",
        |a, b| {
            // A deletes booking 1, B edits it later
            soft_delete_booking(&a.db().unwrap(), 1).unwrap();
            exec(a, "UPDATE transactions SET deleted_ms = 100 WHERE id = 1");
            b.update_transaction_note(1, "later".into()).unwrap();
            // a booking A got and deleted before B ever saw it
            exec(
                a,
                "INSERT INTO transactions(id, account_id, external_id, booking_date, amount_cents, deleted_ms)
                     VALUES (3, 1, 'e3', '2026-01-03', -1, 50)",
            );
        },
        |v| {
            assert_eq!(
                n(
                    v,
                    "SELECT count(*) FROM transactions WHERE deleted_ms IS NULL"
                ),
                1
            );
            let live: Vec<String> = v
                .list_transactions(crate::models::BookingFilter {
                    limit: 100,
                    ..Default::default()
                })
                .unwrap()
                .transactions
                .into_iter()
                .map(|t| t.counterparty)
                .collect();
            assert_eq!(live, ["Kiosk"]);
        },
    );
}

#[test]
fn twin_tombstone_is_not_revived_by_merge() {
    let dir = temp_dir("bk-twin");
    let (a, b) = (open(&dir, "a.db"), open(&dir, "b.db"));
    // A: MoneyMoney import, bank copy, twin merge (bank copy -> tombstone)
    let csv = "Datum;Name;Verwendungszweck;Betrag;Kategorie;Notiz\n10.05.2026;REWE Markt;Einkauf;-12,50;;\n";
    let acc = a
        .import_moneymoney_data(csv.as_bytes().to_vec(), "t.csv".into(), None, true, true)
        .unwrap()
        .account_id;
    {
        let db = a.db().unwrap();
        db.execute(
            "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, counterparty, purpose)
             VALUES (?1, 'f1', '2026-05-10', -1250, 'REWE', 'Einkauf')",
            [acc],
        )
        .unwrap();
        assert!(
            crate::importers::moneymoney_import::merge_synced_twin(
                &db,
                db.last_insert_rowid(),
                "2026-05-10",
                -1250,
                "",
                "REWE",
                "Einkauf"
            )
            .unwrap()
        );
    }
    // B knows the same account and only the bank's booking
    let ext = text(
        &a,
        &format!("SELECT external_ref FROM accounts WHERE id = {acc}"),
    );
    let created = n(
        &a,
        &format!("SELECT created_ms FROM accounts WHERE id = {acc}"),
    );
    {
        let db = b.db().unwrap();
        db.execute_batch("INSERT INTO account_groups(id, name) VALUES (1, 'G');")
            .unwrap();
        db.execute(
            "INSERT INTO accounts(id, group_id, external_ref, name, created_ms) VALUES (1, 1, ?1, 'A', ?2)",
            params![ext, created],
        )
        .unwrap();
        db.execute(
            "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, counterparty, purpose)
             VALUES (1, 'f1', '2026-05-10', -1250, 'REWE', 'Einkauf')",
            [],
        )
        .unwrap();
    }
    both_ways(&a, &b);
    for v in [&a, &b] {
        assert_eq!(
            n(
                v,
                "SELECT count(*) FROM transactions WHERE deleted_ms IS NULL"
            ),
            1
        );
        assert_eq!(
            n(
                v,
                "SELECT count(*) FROM transactions WHERE external_id LIKE 'mm-%' AND deleted_ms IS NOT NULL"
            ),
            1
        );
    }
    assert_eq!(merge(&b, &a).bookings_added, 0);
}

#[test]
fn booking_on_deleted_category_ends_uncategorised() {
    each_order(
        "bk-delcat",
        |a, b| {
            b.set_transaction_category(1, Some(cat(b, "Food"))).unwrap();
            b.set_transaction_splits(
                2,
                vec![split(Some(cat(b, "Food")), -100), split(None, -200)],
            )
            .unwrap();
            exec(
                b,
                "UPDATE transactions SET category_ms = 100, splits_ms = 100",
            );
            exec(
                b,
                "UPDATE categories SET name_ms = 1, parent_ms = 1, symbol_ms = 1, color_ms = 1, budget_ms = 1, order_ms = 1",
            );
            a.delete_category(cat(a, "Food")).unwrap();
        },
        |v| {
            assert_eq!(bk_cat(v, 1), "-/none");
            assert_eq!(splits_of(v, 2), ["-:-100", "-:-200"]);
            assert_eq!(
                n(
                    v,
                    "SELECT (SELECT count(*) FROM transactions t JOIN categories c ON c.id = t.category_id
                              WHERE c.deleted_ms IS NOT NULL)
                          + (SELECT count(*) FROM transaction_splits s JOIN categories c ON c.id = s.category_id
                              WHERE c.deleted_ms IS NOT NULL)"
                ),
                0
            );
        },
    );
}

#[test]
fn v7_snapshot_does_not_override_stamped_note() {
    let dir = temp_dir("bk-v7");
    let a = open(&dir, "a.db");
    shared_account(&a);
    exec(
        &a,
        "INSERT INTO transactions(id, account_id, external_id, booking_date, amount_cents, note, note_ms)
             VALUES (1, 1, 'e1', '2026-01-01', -500, 'mine', 100),
                    (2, 1, 'e2', '2026-01-02', -300, '', 0);",
    );
    let path = dir.join("old.db").to_string_lossy().to_string();
    {
        let conn = Connection::open(&path).unwrap();
        Vault::apply_key(&conn, &[7u8; 32]).unwrap();
        conn.execute_batch(crate::vault::SCHEMA).unwrap();
        crate::user_groups::migrate(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO account_groups(id, name) VALUES (1, 'G');
             INSERT INTO accounts(id, group_id, external_ref, name) VALUES (1, 1, 'DE1', 'A');
             INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, note)
                 VALUES (1, 'e1', '2026-01-01', -500, 'a much longer old note'),
                        (1, 'e2', '2026-01-02', -300, 'filled');
             PRAGMA user_version = 7;",
        )
        .unwrap();
    }
    a.merge_other_vault(path.clone(), vec![7u8; 32]).unwrap();
    assert_eq!(
        text(&a, "SELECT note FROM transactions WHERE id = 1"),
        "mine"
    );
    assert_eq!(
        text(&a, "SELECT note FROM transactions WHERE id = 2"),
        "filled"
    );
    let b = Vault::open(path, vec![7u8; 32]).unwrap();
    both_ways(&a, &b);
    for v in [&a, &b] {
        assert_eq!(
            text(v, "SELECT note FROM transactions WHERE external_id = 'e1'"),
            "mine"
        );
    }
}

#[test]
fn identical_imported_bookings_stay_two() {
    for delete_first in [false, true] {
        let dir = temp_dir("bk-fp");
        let (a, b) = (open(&dir, "a.db"), open(&dir, "b.db"));
        shared_account(&a);
        shared_account(&b);
        // two identical coffees on one day: same fingerprint, two rows
        exec(
            &a,
            "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, import_fp, note)
                 VALUES (1, 'mm-F-0', '2026-01-01', -300, 'F', 'one'),
                        (1, 'mm-F-1', '2026-01-01', -300, 'F', 'two');",
        );
        if delete_first {
            exec(
                &a,
                "UPDATE transactions SET deleted_ms = 50 WHERE external_id = 'mm-F-0'",
            );
        }
        both_ways(&a, &b);
        let live = if delete_first { 1 } else { 2 };
        for v in [&a, &b] {
            assert_eq!(n(v, "SELECT count(*) FROM transactions"), 2);
            assert_eq!(
                n(
                    v,
                    "SELECT count(*) FROM transactions WHERE deleted_ms IS NULL"
                ),
                live
            );
            assert_eq!(
                text(
                    v,
                    "SELECT note FROM transactions WHERE external_id = 'mm-F-0'"
                ),
                "one"
            );
            assert_eq!(
                text(
                    v,
                    "SELECT note FROM transactions WHERE external_id = 'mm-F-1'"
                ),
                "two"
            );
            assert_eq!(
                n(
                    v,
                    "SELECT count(*) FROM transactions WHERE external_id = 'mm-F-1' AND deleted_ms IS NULL"
                ),
                1
            );
        }
    }
}

/// Runs `setup` on a fresh pair with only the shared account, merges both
/// ways starting with A or with B, and returns the synced state of each run.
fn fp_runs(tag: &str, setup: impl Fn(&Vault, &Vault)) -> Vec<Vec<String>> {
    [true, false]
        .into_iter()
        .map(|a_first| {
            let dir = temp_dir(tag);
            let (a, b) = (open(&dir, "a.db"), open(&dir, "b.db"));
            shared_account(&a);
            shared_account(&b);
            setup(&a, &b);
            if a_first {
                both_ways(&a, &b);
            } else {
                both_ways(&b, &a);
            }
            synced_state(&a)
        })
        .collect()
}

#[test]
fn imported_and_fetched_copy_become_one_booking() {
    let runs = fp_runs("bk-fp-one", |a, b| {
        exec(
            a,
            "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, import_fp, note, checked)
                 VALUES (1, 'mm-F-0', '2026-01-01', -300, 'F', 'imported', 1);",
        );
        exec(
            b,
            "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, import_fp)
                 VALUES (1, 'f1', '2026-01-01', -300, 'F');",
        );
    });
    assert_eq!(runs[0], runs[1], "merge order changed the result");
    let bookings: Vec<&String> = runs[0]
        .iter()
        .filter(|l| l.starts_with("booking"))
        .collect();
    assert_eq!(bookings.len(), 1, "{bookings:?}");
    // the bank's id is kept, the note and check mark come along
    assert!(bookings[0].contains("|DE1|f1\""), "{}", bookings[0]);
    assert!(bookings[0].contains("imported"), "{}", bookings[0]);
}

#[test]
fn identical_rows_with_a_deletion_pair_the_same_in_both_orders() {
    let runs = fp_runs("bk-fp-mixed", |a, b| {
        exec(
            a,
            "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, import_fp, deleted_ms)
                 VALUES (1, 'mm-F-0', '2026-01-01', -300, 'F', 50),
                        (1, 'mm-F-1', '2026-01-01', -300, 'F', NULL);",
        );
        exec(
            b,
            "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, import_fp)
                 VALUES (1, 'b1', '2026-01-01', -300, 'F'), (1, 'b2', '2026-01-01', -300, 'F');",
        );
    });
    assert_eq!(runs[0], runs[1], "merge order changed the result");
    let bookings: Vec<&String> = runs[0]
        .iter()
        .filter(|l| l.starts_with("booking"))
        .collect();
    assert_eq!(bookings.len(), 2, "{bookings:?}");
    // A's deletion survives: one of the two stays deleted
    assert_eq!(
        bookings.iter().filter(|l| l.ends_with("Null]")).count(),
        1,
        "{bookings:?}"
    );
}

// ------------------------------------------------------------ accounts, connections, convergence

/// The `bank_fixture` on both vaults with the same creation stamps (1).
fn bank_pair(tag: &str) -> (Arc<Vault>, Arc<Vault>) {
    let dir = temp_dir(tag);
    let (a, b) = (open(&dir, "a.db"), open(&dir, "b.db"));
    for v in [&a, &b] {
        bank_fixture(v);
        exec(
            v,
            "UPDATE accounts SET created_ms = 1; UPDATE bank_connections SET created_ms = 1;",
        );
    }
    (a, b)
}

fn assert_zero(st: &crate::vault::MergeStats) {
    assert_eq!(
        format!("{st:?}"),
        format!("{:?}", crate::vault::MergeStats::default()),
        "a repeated merge changed something"
    );
}

/// `deleted_ms` of the account (and its bookings) with this external ref.
fn acc_deleted(v: &Vault, ext: &str) -> (Option<i64>, Vec<Option<i64>>) {
    let db = v.db().unwrap();
    let acc: Option<i64> = db
        .query_row(
            "SELECT deleted_ms FROM accounts WHERE external_ref = ?1",
            [ext],
            |r| r.get(0),
        )
        .unwrap();
    let mut st = db
        .prepare(
            "SELECT t.deleted_ms FROM transactions t JOIN accounts a ON a.id = t.account_id
             WHERE a.external_ref = ?1 ORDER BY t.external_id",
        )
        .unwrap();
    let bk = st
        .query_map([ext], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    (acc, bk)
}

#[test]
fn deleted_account_is_soft_deleted_on_the_other_vault() {
    for a_first in [true, false] {
        let (a, b) = bank_pair("acc-del");
        // B fetched one more booking the deleting side never saw
        exec(
            &b,
            "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents)
             SELECT id, 'e2', '2026-01-02', -7 FROM accounts WHERE external_ref = 'A1';",
        );
        let a1 = n(&a, "SELECT id FROM accounts WHERE external_ref = 'A1'");
        a.set_account_archived(a1, true).unwrap();
        a.delete_account(a1).unwrap();
        let stamp = n(
            &a,
            &format!("SELECT deleted_ms FROM accounts WHERE id = {a1}"),
        );
        if a_first {
            both_ways(&a, &b);
        } else {
            both_ways(&b, &a);
        }
        for v in [&a, &b] {
            assert_eq!(
                acc_deleted(v, "A1"),
                (Some(stamp), vec![Some(stamp), Some(stamp)])
            );
            assert_eq!(acc_deleted(v, "A2"), (None, vec![None]));
        }
        assert_zero(&merge(&a, &b));
        assert_zero(&merge(&b, &a));
    }
}

/// A deleted the connection at 20 (cascade); B took the deletion over and
/// then added the connection again and fetched account A1 (revive).
fn readded_pair(tag: &str, revive_ms: i64) -> (Arc<Vault>, Arc<Vault>) {
    let (a, b) = bank_pair(tag);
    // both know a booking A deleted on its own before the connection went
    for v in [&a, &b] {
        exec(
            v,
            "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents)
             SELECT id, 'e0', '2026-01-03', -9 FROM accounts WHERE external_ref = 'A1';",
        );
    }
    exec(
        &a,
        "UPDATE transactions SET deleted_ms = 10 WHERE external_id = 'e0';",
    );
    let c = n(&a, "SELECT id FROM bank_connections");
    a.delete_bank_connection(c).unwrap();
    exec(
        &a,
        "UPDATE transactions SET deleted_ms = 20 WHERE deleted_ms > 10;
         UPDATE accounts SET deleted_ms = 20 WHERE deleted_ms IS NOT NULL;
         UPDATE bank_connections SET deleted_ms = 20;",
    );
    merge(&b, &a);
    let c = n(&b, "SELECT id FROM bank_connections");
    let a1 = n(&b, "SELECT id FROM accounts WHERE external_ref = 'A1'");
    {
        let db = b.db().unwrap();
        revive_connection(&db, c).unwrap();
        revive_account_if_deleted(&db, a1).unwrap();
    }
    exec(
        &b,
        &format!(
            "UPDATE bank_connections SET created_ms = {revive_ms};
             UPDATE accounts SET created_ms = {revive_ms} WHERE deleted_ms IS NULL AND external_ref = 'A1';"
        ),
    );
    (a, b)
}

#[test]
fn readded_connection_beats_older_deletion() {
    for a_first in [true, false] {
        let (a, b) = readded_pair("conn-readd", 30);
        if a_first {
            both_ways(&a, &b);
        } else {
            both_ways(&b, &a);
        }
        for v in [&a, &b] {
            assert_eq!(
                n(
                    v,
                    "SELECT count(*) FROM bank_connections WHERE deleted_ms IS NULL AND created_ms = 30"
                ),
                1
            );
            // the revived account keeps its cascade-deleted bookings back,
            // the one deleted on its own stays deleted
            assert_eq!(acc_deleted(v, "A1"), (None, vec![Some(10), None]));
            assert_eq!(acc_deleted(v, "A2"), (Some(20), vec![Some(20)]));
            assert_eq!(acc_deleted(v, "loose"), (None, vec![None]));
        }
        assert_zero(&merge(&a, &b));
        assert_zero(&merge(&b, &a));
    }
}

#[test]
fn deletion_newer_than_readd_wins() {
    for a_first in [true, false] {
        let (a, b) = readded_pair("conn-readd-old", 15);
        if a_first {
            both_ways(&a, &b);
        } else {
            both_ways(&b, &a);
        }
        for v in [&a, &b] {
            assert_eq!(
                n(
                    v,
                    "SELECT count(*) FROM bank_connections WHERE deleted_ms = 20"
                ),
                1
            );
            assert_eq!(acc_deleted(v, "A1"), (Some(20), vec![Some(10), Some(20)]));
        }
    }
}

#[test]
fn deleted_category_never_returns_via_third_vault() {
    let dir = temp_dir("cat-third");
    let (a, b, c) = (open(&dir, "a.db"), open(&dir, "b.db"), open(&dir, "c.db"));
    a.create_category("Food".into(), None, String::new(), "#111111".into())
        .unwrap();
    exec(
        &a,
        "UPDATE categories SET name_ms=1, parent_ms=1, symbol_ms=1, color_ms=1, order_ms=1;",
    );
    merge(&b, &a);
    merge(&c, &a);
    a.delete_category(cat(&a, "Food")).unwrap();
    merge(&b, &a);
    // C still holds the old live copy and edits it
    let food_c = cat(&c, "Food");
    c.update_category(food_c, "Food".into(), None, String::new(), "#222222".into())
        .unwrap();
    merge(&a, &c);
    merge(&c, &b);
    merge(&b, &c);
    merge(&a, &b);
    for v in [&a, &b, &c] {
        assert_eq!(
            n(
                v,
                "SELECT count(*) FROM categories WHERE deleted_ms IS NULL"
            ),
            0
        );
    }
    assert_converged(&a, &b);
    assert_converged(&b, &c);
}

/// A, B and C start from A's base, then each edits and deletes something.
fn three_vaults(tag: &str) -> [Arc<Vault>; 3] {
    let dir = temp_dir(tag);
    let (a, b, c) = (open(&dir, "a.db"), open(&dir, "b.db"), open(&dir, "c.db"));
    bank_fixture(&a);
    let food = a
        .create_category("Food".into(), None, String::new(), "#111111".into())
        .unwrap();
    a.create_category("Fun".into(), None, String::new(), "#333333".into())
        .unwrap();
    a.save_rule(plain_rule(food, "rewe")).unwrap();
    exec(
        &a,
        "UPDATE categories SET name_ms=1, parent_ms=1, symbol_ms=1, color_ms=1, order_ms=1;
         UPDATE rules SET updated_ms = 1;
         UPDATE accounts SET created_ms = 1; UPDATE bank_connections SET created_ms = 1;",
    );
    merge(&b, &a);
    merge(&c, &a);
    let bk = |v: &Vault, acc: &str| {
        n(
            v,
            &format!(
                "SELECT t.id FROM transactions t JOIN accounts a ON a.id = t.account_id
                 WHERE a.external_ref = '{acc}' AND t.external_id = 'e1'"
            ),
        )
    };
    // A: rename, note, deletes account A2
    a.update_category(
        cat(&a, "Food"),
        "Essen".into(),
        None,
        String::new(),
        "#111111".into(),
    )
    .unwrap();
    a.update_transaction_note(bk(&a, "A1"), "lunch".into())
        .unwrap();
    let a2 = n(&a, "SELECT id FROM accounts WHERE external_ref = 'A2'");
    a.set_account_archived(a2, true).unwrap();
    a.delete_account(a2).unwrap();
    // B: recolour, category, deletes the rule, fetches a booking on A2
    b.update_category(
        cat(&b, "Food"),
        "Food".into(),
        None,
        String::new(),
        "#222222".into(),
    )
    .unwrap();
    b.set_transaction_category(bk(&b, "A1"), Some(cat(&b, "Fun")))
        .unwrap();
    b.delete_rule(n(&b, "SELECT id FROM rules")).unwrap();
    exec(
        &b,
        "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents)
         SELECT id, 'e9', '2026-02-01', -9 FROM accounts WHERE external_ref = 'A2';",
    );
    // C: deletes Fun, checks a booking, adds Travel, deletes a booking
    c.delete_category(cat(&c, "Fun")).unwrap();
    c.set_transaction_checked(bk(&c, "A1"), true).unwrap();
    c.create_category("Travel".into(), None, String::new(), String::new())
        .unwrap();
    let loose = bk(&c, "loose");
    soft_delete_booking(&c.db().unwrap(), loose).unwrap();
    [a, b, c]
}

#[test]
fn three_vaults_converge_in_any_merge_order() {
    let orders: [&[(usize, usize)]; 3] = [
        &[(0, 1), (0, 2), (1, 0), (2, 0)],
        &[(2, 1), (2, 0), (0, 2), (1, 2)],
        &[(1, 2), (0, 1), (2, 0), (1, 2)],
    ];
    for order in orders {
        let v = three_vaults("three");
        for &(into, from) in order {
            merge(&v[into], &v[from]);
        }
        assert_converged(&v[0], &v[1]);
        assert_converged(&v[1], &v[2]);
        for x in &v {
            let x: &Vault = x;
            assert_eq!(
                text(
                    x,
                    "SELECT name || color_hex FROM categories WHERE deleted_ms IS NULL AND name != 'Travel'"
                ),
                "Essen#222222"
            );
            assert_eq!(
                n(
                    x,
                    "SELECT count(*) FROM categories WHERE deleted_ms IS NULL"
                ),
                2
            );
            assert_eq!(
                n(x, "SELECT count(*) FROM rules WHERE deleted_ms IS NULL"),
                0
            );
            let (acc, bks) = acc_deleted(x, "A2");
            assert!(acc.is_some());
            assert_eq!(bks, vec![acc, acc]);
            assert_eq!(acc_deleted(x, "loose").1.iter().flatten().count(), 1);
            assert_eq!(
                text(
                    x,
                    "SELECT t.note || t.checked || t.category_source FROM transactions t
                     JOIN accounts a ON a.id = t.account_id WHERE a.external_ref = 'A1'"
                ),
                "lunch1none"
            );
        }
        // a second round changes nothing
        for into in 0..3 {
            for from in 0..3 {
                if into != from {
                    assert_zero(&merge(&v[into], &v[from]));
                }
            }
        }
        assert_converged(&v[0], &v[1]);
        assert_converged(&v[1], &v[2]);
    }
}

/// Sum of every sync stamp and deletion in the vault.
fn stamp_sum(v: &Vault) -> i64 {
    n(
        v,
        "SELECT (SELECT IFNULL(sum(category_ms + note_ms + checked_ms + text_ms + splits_ms
                                   + IFNULL(deleted_ms, 0)), 0) FROM transactions)
              + (SELECT IFNULL(sum(name_ms + parent_ms + symbol_ms + color_ms + budget_ms + order_ms
                                   + IFNULL(deleted_ms, 0)), 0) FROM categories)
              + (SELECT IFNULL(sum(updated_ms + IFNULL(deleted_ms, 0)), 0) FROM rules)
              + (SELECT IFNULL(sum(created_ms + IFNULL(deleted_ms, 0)), 0) FROM accounts)
              + (SELECT IFNULL(sum(created_ms + IFNULL(deleted_ms, 0)), 0) FROM bank_connections)",
    )
}

/// Every public function from Tasks 2-4 that changes synced state raises a
/// stamp or sets a deletion; otherwise the change would never reach another
/// device. A new mutating function missing from this list is a review finding.
#[test]
fn every_mutating_api_stamps_or_soft_deletes() {
    let v = open(&temp_dir("coverage"), "a.db");
    let (c, a1, a2, _) = bank_fixture(&v);
    let food = v
        .create_category("Food".into(), None, String::new(), String::new())
        .unwrap();
    let fun = v
        .create_category("Fun".into(), None, String::new(), String::new())
        .unwrap();
    let rule = v.save_rule(plain_rule(food, "rewe")).unwrap();
    let t = n(
        &v,
        &format!("SELECT id FROM transactions WHERE account_id = {a1}"),
    );
    type Op = Box<dyn Fn(&Vault)>;
    let ops: Vec<(&str, Op)> = vec![
        (
            "set_transaction_category",
            Box::new(move |v| v.set_transaction_category(t, Some(food)).unwrap()),
        ),
        (
            "set_transaction_category(None)",
            Box::new(move |v| v.set_transaction_category(t, None).unwrap()),
        ),
        (
            "set_transaction_checked",
            Box::new(move |v| v.set_transaction_checked(t, true).unwrap()),
        ),
        (
            "update_transaction_text",
            Box::new(move |v| {
                v.update_transaction_text(t, "X".into(), "Y".into())
                    .unwrap()
            }),
        ),
        (
            "update_transaction_note",
            Box::new(move |v| v.update_transaction_note(t, "n".into()).unwrap()),
        ),
        (
            "set_transaction_splits",
            Box::new(move |v| {
                v.set_transaction_splits(t, vec![split(Some(food), -2), split(None, -3)])
                    .unwrap()
            }),
        ),
        (
            "create_category",
            Box::new(|v| {
                v.create_category("New".into(), None, String::new(), String::new())
                    .unwrap();
            }),
        ),
        (
            "update_category",
            Box::new(move |v| {
                v.update_category(fun, "Fun2".into(), None, String::new(), String::new())
                    .unwrap()
            }),
        ),
        (
            "set_category_budget",
            Box::new(move |v| v.set_category_budget(fun, Some(500)).unwrap()),
        ),
        (
            "save_rule (new)",
            Box::new(move |v| {
                v.save_rule(plain_rule(fun, "kino")).unwrap();
            }),
        ),
        (
            "save_rule (edit)",
            Box::new(move |v| {
                let mut r = plain_rule(food, "lidl");
                r.id = rule;
                v.save_rule(r).unwrap();
            }),
        ),
        (
            "delete_rule",
            Box::new(move |v| v.delete_rule(rule).unwrap()),
        ),
        (
            "delete_category",
            Box::new(move |v| v.delete_category(fun).unwrap()),
        ),
        (
            "add_bank_connection",
            Box::new(|v| {
                v.add_bank_connection(ConnectorKind::Comdirect, String::new(), "me".into())
                    .unwrap();
            }),
        ),
        (
            "delete_account",
            Box::new(move |v| {
                v.set_account_archived(a2, true).unwrap();
                v.delete_account(a2).unwrap();
            }),
        ),
        (
            "delete_bank_connection",
            Box::new(move |v| v.delete_bank_connection(c).unwrap()),
        ),
        (
            "add_bank_connection (revive)",
            Box::new(|v| {
                let id = n(
                    v,
                    "SELECT id FROM bank_connections WHERE connector = 'comdirect'",
                );
                v.delete_bank_connection(id).unwrap();
                exec(
                    v,
                    "UPDATE bank_connections SET deleted_ms = 1, created_ms = 1 WHERE connector = 'comdirect'",
                );
                v.add_bank_connection(ConnectorKind::Comdirect, String::new(), "me".into())
                    .unwrap();
            }),
        ),
        (
            "import_moneymoney_data",
            Box::new(|v| {
                let csv = "Datum;Name;Verwendungszweck;Betrag;Kategorie;Notiz\n10.05.2026;REWE;Einkauf;-12,50;Essen;wichtig\n";
                v.import_moneymoney_data(csv.as_bytes().to_vec(), "t.csv".into(), None, true, true)
                    .unwrap();
            }),
        ),
        (
            "import_moneymoney_database",
            Box::new(|v| {
                use crate::importers::moneymoney_db::tests::{PW, fake_db, options_from};
                let path = fake_db();
                let p = v
                    .preview_moneymoney_database(path.clone(), PW.into(), None)
                    .unwrap();
                v.import_moneymoney_database(path, PW.into(), options_from(&p, false))
                    .unwrap();
            }),
        ),
        (
            "create_manual_loan",
            Box::new(|v| {
                v.create_manual_loan(crate::loans::LoanSetup {
                    name: "Kredit".into(),
                    original_cents: None,
                    anchor_date: "2026-01-01".into(),
                    anchor_balance_cents: 100_000,
                    interest_rate_ppm: None,
                    match_expr: String::new(),
                })
                .unwrap();
            }),
        ),
        (
            "add_fints_connection",
            Box::new(|v| {
                v.add_fints_connection(
                    "87654321".into(),
                    "https://fints.example.com/x".into(),
                    "Bank".into(),
                    "me".into(),
                    String::new(),
                )
                .unwrap();
            }),
        ),
        (
            "add_enable_banking_connection",
            Box::new(|v| {
                v.add_enable_banking_connection(
                    "N26".into(),
                    "DE".into(),
                    "N26".into(),
                    "me".into(),
                )
                .unwrap();
            }),
        ),
    ];
    for (name, op) in ops {
        // older stamps, so that an edit in the same millisecond still counts
        exec(
            &v,
            "UPDATE transactions SET category_ms = min(category_ms, 1), note_ms = min(note_ms, 1),
                 checked_ms = min(checked_ms, 1), text_ms = min(text_ms, 1), splits_ms = min(splits_ms, 1);
             UPDATE categories SET name_ms = min(name_ms, 1), parent_ms = min(parent_ms, 1),
                 symbol_ms = min(symbol_ms, 1), color_ms = min(color_ms, 1),
                 budget_ms = min(budget_ms, 1), order_ms = min(order_ms, 1);
             UPDATE rules SET updated_ms = min(updated_ms, 1);
             UPDATE accounts SET created_ms = min(created_ms, 1);
             UPDATE bank_connections SET created_ms = min(created_ms, 1);",
        );
        let before = stamp_sum(&v);
        op(&v);
        assert!(
            stamp_sum(&v) > before,
            "{name} changed nothing a merge would see"
        );
    }
}

#[test]
fn twin_merge_leaves_a_tombstone_for_the_moneymoney_key() {
    for a_first in [true, false] {
        let dir = temp_dir("twin-key");
        let (a, b) = (open(&dir, "a.db"), open(&dir, "b.db"));
        let csv = "Datum;Name;Verwendungszweck;Betrag;Kategorie;Notiz\n10.05.2026;REWE Markt;Einkauf;-12,50;;wichtig\n";
        let acc = a
            .import_moneymoney_data(csv.as_bytes().to_vec(), "t.csv".into(), None, true, true)
            .unwrap()
            .account_id;
        // B holds the import row, then fetches the bank's copy itself
        merge(&b, &a);
        for v in [&a, &b] {
            exec(
                v,
                "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, counterparty, purpose)
                 SELECT id, 'f1', '2026-05-10', -1250, 'REWE', 'Einkauf' FROM accounts;",
            );
        }
        // A repairs the duplicate
        let bank_id = n(
            &a,
            &format!("SELECT id FROM transactions WHERE account_id = {acc} AND external_id = 'f1'"),
        );
        assert!(
            crate::importers::moneymoney_import::merge_synced_twin(
                &a.db().unwrap(),
                bank_id,
                "2026-05-10",
                -1250,
                "",
                "REWE",
                "Einkauf"
            )
            .unwrap()
        );
        if a_first {
            both_ways(&a, &b);
        } else {
            both_ways(&b, &a);
        }
        for v in [&a, &b] {
            assert_eq!(
                text(
                    v,
                    "SELECT external_id || ':' || note FROM transactions WHERE deleted_ms IS NULL"
                ),
                "f1:wichtig"
            );
        }
    }
}

#[test]
fn loan_links_skip_deleted_bookings_and_match_by_key() {
    let dir = temp_dir("loan-links");
    let (a, b) = (open(&dir, "a.db"), open(&dir, "b.db"));
    for v in [&a, &b] {
        shared_account(v);
        exec(
            v,
            "INSERT INTO accounts(id, group_id, external_ref, name) VALUES (2, 1, 'L1', 'Loan');
             INSERT INTO transactions(id, account_id, external_id, booking_date, amount_cents, import_fp)
                 VALUES (1, 1, 'mm-F-0', '2026-01-01', -300, 'F'),
                        (2, 1, 'mm-F-1', '2026-01-01', -300, 'F'),
                        (3, 1, 'e3', '2026-01-02', -100, '');",
        );
    }
    exec(
        &a,
        "INSERT INTO loan_links(transaction_id, loan_account_id, source)
             VALUES (2, 2, 'manual'), (3, 2, 'manual');",
    );
    exec(&b, "UPDATE transactions SET deleted_ms = 5 WHERE id = 3;");
    both_ways(&a, &b);
    for v in [&a, &b] {
        assert_eq!(
            text(
                v,
                "SELECT group_concat(t.external_id) FROM loan_links l
                 JOIN transactions t ON t.id = l.transaction_id"
            ),
            "mm-F-1"
        );
    }
}

/// A archived and deleted account A1; B took that over and then got A1 again
/// from the bank (revive). Both end with A1 live, unarchived and in totals.
#[test]
fn revived_account_is_unarchived_on_both_vaults() {
    for a_first in [true, false] {
        let (a, b) = bank_pair("acc-unarchive");
        let a1 = n(&a, "SELECT id FROM accounts WHERE external_ref = 'A1'");
        a.set_account_archived(a1, true).unwrap();
        a.delete_account(a1).unwrap();
        exec(
            &a,
            "UPDATE accounts SET archived_ms = 5 WHERE external_ref = 'A1';
             UPDATE transactions SET deleted_ms = 20 WHERE deleted_ms IS NOT NULL;
             UPDATE accounts SET deleted_ms = 20 WHERE deleted_ms IS NOT NULL;",
        );
        merge(&b, &a);
        let b1 = n(&b, "SELECT id FROM accounts WHERE external_ref = 'A1'");
        revive_account_if_deleted(&b.db().unwrap(), b1).unwrap();
        if a_first {
            both_ways(&a, &b);
        } else {
            both_ways(&b, &a);
        }
        for v in [&a, &b] {
            assert_eq!(
                n(
                    v,
                    "SELECT count(*) FROM accounts
                     WHERE external_ref = 'A1' AND deleted_ms IS NULL AND archived_unix IS NULL"
                ),
                1
            );
            let acc = v
                .list_accounts()
                .unwrap()
                .into_iter()
                .find(|x| x.name == "A1")
                .expect("A1 listed");
            assert!(acc.in_total);
        }
        assert_zero(&merge(&a, &b));
        assert_zero(&merge(&b, &a));
    }
}

// ------------------------------------------------------- MoneyMoney twins

const MM_GIRO: &str = "moneymoney-db:5:4de55d4e";
const GIRO_IBAN: &str = "DE53200411110102211000";

/// Account `ext` on the bank login `c` with the IBAN above and one booking
/// per external id (`mm-` ids carry their import fingerprint).
fn giro(v: &Vault, c: i64, ext: &str, bookings: &[&str]) -> i64 {
    let db = v.db().unwrap();
    db.execute_batch("INSERT OR IGNORE INTO account_groups(id, name) VALUES (1, 'G')")
        .unwrap();
    db.execute(
        "INSERT INTO accounts(connection_id, group_id, external_ref, name, iban, account_number, currency, kind)
         VALUES (?1, 1, ?2, 'Girokonto', ?3, '102211000', 'EUR', 'checking')",
        params![c, ext, GIRO_IBAN],
    )
    .unwrap();
    let id = db.last_insert_rowid();
    for (i, b) in bookings.iter().enumerate() {
        db.execute(
            "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, import_fp)
             VALUES (?1, ?2, '2026-01-01', ?3, ?4)",
            params![id, b, -100 - i as i64, b.strip_prefix("mm-").unwrap_or("")],
        )
        .unwrap();
    }
    id
}

fn comdirect(v: &Vault) -> i64 {
    v.add_bank_connection(ConnectorKind::Comdirect, String::new(), "login".into())
        .unwrap()
}

/// external_id:note:checked of the live bookings, sorted.
fn live_bookings(v: &Vault) -> String {
    let db = v.db().unwrap();
    let mut st = db
        .prepare(
            "SELECT external_id || ':' || note || ':' || checked FROM transactions
              WHERE deleted_ms IS NULL ORDER BY external_id",
        )
        .unwrap();
    let rows: Vec<String> = st
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    rows.join(" ")
}

/// A MoneyMoney account that a bank fetch took over on one device came back
/// from another device as a second account (same login, IBAN and MoneyMoney
/// bookings). Opening the vault folds it into the bank account: shared
/// bookings once, the newer edit wins, bookings only the twin has move over.
#[test]
fn moneymoney_twin_of_a_bank_account_is_folded_on_open() {
    let dir = temp_dir("twin-fold");
    let v = open(&dir, "a.db");
    let c = comdirect(&v);
    let bank = giro(&v, c, "API1", &["mm-1", "mm-2", "b1"]);
    let twin = giro(&v, c, MM_GIRO, &["mm-1", "mm-2", "mm-3"]);
    exec(
        &v,
        &format!(
            "UPDATE transactions SET checked = 1, checked_ms = 5 WHERE account_id = {bank} AND external_id = 'mm-1'"
        ),
    );
    exec(
        &v,
        &format!(
            "UPDATE transactions SET note = 'Miete', note_ms = 9 WHERE account_id = {twin} AND external_id = 'mm-2'"
        ),
    );
    exec(
        &v,
        &format!("UPDATE accounts SET note = 'Haushalt', note_ms = 7 WHERE id = {twin}"),
    );
    drop(v);
    let v = open(&dir, "a.db");
    assert_eq!(
        n(&v, "SELECT count(*) FROM accounts WHERE deleted_ms IS NULL"),
        1
    );
    assert_eq!(
        text(&v, &format!("SELECT note FROM accounts WHERE id = {bank}")),
        "Haushalt",
        "the account note goes with the twin's bookings"
    );
    assert_eq!(
        n(
            &v,
            &format!("SELECT deleted_ms > 0 FROM accounts WHERE id = {twin}")
        ),
        1
    );
    assert_eq!(live_bookings(&v), "b1::0 mm-1::1 mm-2:Miete:0 mm-3::0");
    assert_eq!(
        n(
            &v,
            &format!(
                "SELECT count(DISTINCT account_id) FROM transactions WHERE deleted_ms IS NULL AND account_id = {bank}"
            )
        ),
        1
    );
}

/// Issue #94: MoneyMoney keeps a closed account with the same IBAN. Without
/// shared bookings it is a different account and stays.
#[test]
fn moneymoney_account_with_same_iban_but_other_bookings_stays() {
    let dir = temp_dir("twin-keep");
    let v = open(&dir, "a.db");
    let c = comdirect(&v);
    giro(&v, c, "API1", &["b1", "mm-1"]);
    giro(&v, c, "moneymoney-db:7:4de55d4e", &["mm-9"]);
    drop(v);
    let v = open(&dir, "a.db");
    assert_eq!(
        n(&v, "SELECT count(*) FROM accounts WHERE deleted_ms IS NULL"),
        2
    );
    assert_eq!(live_bookings(&v), "b1::0 mm-1::0 mm-9::0");
}

/// A depot twin has no IBAN and no bookings: same kind, number and name.
#[test]
fn empty_depot_twin_is_folded() {
    let dir = temp_dir("twin-depot");
    let v = open(&dir, "a.db");
    let c = comdirect(&v);
    exec(&v, "INSERT INTO account_groups(id, name) VALUES (1, 'G')");
    for ext in ["depot:E5AA", "moneymoney-db:22:4de55d4e"] {
        v.db()
            .unwrap()
            .execute(
                "INSERT INTO accounts(connection_id, group_id, external_ref, name, account_number, currency, kind)
                 VALUES (?1, 1, ?2, 'Depot', '102211000', 'EUR', 'brokerage')",
                params![c, ext],
            )
            .unwrap();
    }
    drop(v);
    let v = open(&dir, "a.db");
    assert_eq!(
        text(
            &v,
            "SELECT group_concat(external_ref) FROM accounts WHERE deleted_ms IS NULL"
        ),
        "depot:E5AA"
    );
}

fn ffb_login(v: &Vault, login: &str) -> i64 {
    v.add_bank_connection(ConnectorKind::Ffb, String::new(), login.into())
        .unwrap()
}

fn depot(v: &Vault, c: i64, ext: &str, name: &str) {
    v.db()
        .unwrap()
        .execute(
            "INSERT INTO accounts(connection_id, group_id, external_ref, name, account_number, currency, kind)
             VALUES (?1, 1, ?2, ?3, '1000000001', 'EUR', 'brokerage')",
            params![c, ext, name],
        )
        .unwrap();
}

fn live_refs(v: &Vault) -> String {
    text(
        v,
        "SELECT group_concat(external_ref) FROM accounts WHERE deleted_ms IS NULL",
    )
}

/// MoneyMoney writes "FFB-Fondsdepot", the connector "FFB Fondsdepot", and
/// the import may sit on another login of the same bank.
#[test]
fn depot_twin_with_other_punctuation_or_login_is_folded() {
    for second_login in [false, true] {
        let dir = temp_dir(if second_login { "depot-b" } else { "depot-a" });
        let v = open(&dir, "a.db");
        exec(&v, "INSERT INTO account_groups(id, name) VALUES (1, 'G')");
        let bank = ffb_login(&v, "login-a");
        let imported = if second_login {
            ffb_login(&v, "login-b")
        } else {
            bank
        };
        depot(&v, bank, "ffb:depot:1000000001", "FFB Fondsdepot");
        depot(&v, imported, "moneymoney-db:22:4de55d4e", "FFB-Fondsdepot");
        drop(v);
        let v = open(&dir, "a.db");
        assert_eq!(live_refs(&v), "ffb:depot:1000000001");
    }
}

/// The MoneyMoney depot was imported without a login: number, name and bank
/// identify it, as long as only one bank depot fits.
#[test]
fn unlinked_depot_twin_is_folded_when_unambiguous() {
    for (case, mm_bank, folded) in [
        ("unlinked-a", "FIL Fondsbank GmbH", true),
        ("unlinked-b", "FIL Fondsbank GmbH", false),
        ("unlinked-c", "Andere Bank AG", false),
    ] {
        let second_bank_depot = case == "unlinked-b";
        let dir = temp_dir(case);
        let v = open(&dir, "a.db");
        exec(&v, "INSERT INTO account_groups(id, name) VALUES (1, 'G')");
        let bank = ffb_login(&v, "login-a");
        depot(&v, bank, "ffb:depot:1000000001", "FFB Fondsdepot");
        if second_bank_depot {
            let other = ffb_login(&v, "login-b");
            depot(&v, other, "ffb:depot:x", "FFB Fondsdepot");
        }
        v.db()
            .unwrap()
            .execute(
                "INSERT INTO accounts(connection_id, group_id, external_ref, name, bank_name, account_number, currency, kind)
                 VALUES (NULL, 1, 'moneymoney-db:22:4de55d4e', 'FFB-Fondsdepot', ?1, '1000000001', 'EUR', 'brokerage')",
                [mm_bank],
            )
            .unwrap();
        exec(
            &v,
            "UPDATE accounts SET bank_name = 'FFB (FIL Fondsbank)' WHERE external_ref LIKE 'ffb:%'",
        );
        drop(v);
        let v = open(&dir, "a.db");
        let mm_live = text(
            &v,
            "SELECT CAST(count(*) AS TEXT) FROM accounts
              WHERE external_ref LIKE 'moneymoney-db:%' AND deleted_ms IS NULL",
        );
        assert_eq!(mm_live, if folded { "0" } else { "1" }, "{case}");
    }
}

#[test]
fn depot_with_another_name_or_connector_stays() {
    let dir = temp_dir("depot-other");
    let v = open(&dir, "a.db");
    exec(&v, "INSERT INTO account_groups(id, name) VALUES (1, 'G')");
    let bank = ffb_login(&v, "login-a");
    let other = comdirect(&v);
    depot(&v, bank, "ffb:depot:1000000001", "FFB Fondsdepot");
    depot(&v, bank, "moneymoney-db:22:aaaaaaaa", "Altersvorsorge");
    depot(&v, other, "moneymoney-db:23:bbbbbbbb", "FFB Fondsdepot");
    drop(v);
    let v = open(&dir, "a.db");
    assert_eq!(
        text(
            &v,
            "SELECT CAST(count(*) AS TEXT) FROM accounts WHERE deleted_ms IS NULL"
        ),
        "3"
    );
}

/// B never fetched: it still holds the MoneyMoney account that A's fetch
/// took over. The merge maps the two onto each other instead of adding the
/// bank account a second time, and both devices converge on the bank's key.
#[test]
fn merge_matches_the_moneymoney_account_a_fetch_took_over() {
    let dir = temp_dir("twin-merge");
    let (a, b) = (open(&dir, "a.db"), open(&dir, "b.db"));
    let c = comdirect(&a);
    giro(&a, c, MM_GIRO, &["mm-1", "mm-2"]);
    exec(&a, "UPDATE bank_connections SET created_ms = 1");
    exec(&a, "UPDATE accounts SET created_ms = 1");
    both_ways(&a, &b);
    let mut acc = remote_account("API1");
    acc.iban = GIRO_IBAN.into();
    let fetch = crate::connectors::FetchResult {
        accounts: vec![acc],
        transactions: vec![remote_tx("API1", "b1")],
        ..Default::default()
    };
    crate::connectors::store_fetch(&a, ConnectorKind::Comdirect, c, fetch).unwrap();
    assert_eq!(text(&a, "SELECT external_ref FROM accounts"), "API1");
    exec(
        &b,
        "UPDATE transactions SET note = 'Miete', note_ms = 9 WHERE external_id = 'mm-2'",
    );
    both_ways(&a, &b);
    for v in [&a, &b] {
        assert_eq!(
            text(
                v,
                "SELECT group_concat(external_ref) FROM accounts WHERE deleted_ms IS NULL"
            ),
            "API1"
        );
        assert_eq!(live_bookings(v), "b1::0 mm-1::0 mm-2:Miete:0");
    }
}

/// A folded first (its twin has the lower id), B still has only the
/// MoneyMoney account with a booking of its own. B takes over the bank key
/// before A's tombstone for the MoneyMoney key arrives, so nothing is lost.
#[test]
fn folded_twin_tombstone_does_not_delete_the_other_devices_account() {
    let dir = temp_dir("twin-tomb");
    let (a, b) = (open(&dir, "a.db"), open(&dir, "b.db"));
    let ca = comdirect(&a);
    let cb = comdirect(&b);
    giro(&a, ca, MM_GIRO, &["mm-1", "mm-2"]);
    giro(&a, ca, "API1", &["mm-1", "mm-2", "b1"]);
    giro(&b, cb, MM_GIRO, &["mm-1", "mm-2", "mm-4"]);
    for v in [&a, &b] {
        exec(v, "UPDATE bank_connections SET created_ms = 1");
        exec(v, "UPDATE accounts SET created_ms = 1");
    }
    drop(a);
    let a = open(&dir, "a.db");
    assert_eq!(
        n(&a, "SELECT count(*) FROM accounts WHERE deleted_ms IS NULL"),
        1
    );
    merge(&b, &a);
    assert_eq!(
        text(
            &b,
            "SELECT group_concat(external_ref) FROM accounts WHERE deleted_ms IS NULL"
        ),
        "API1"
    );
    both_ways(&a, &b);
    for v in [&a, &b] {
        assert_eq!(live_bookings(v), "b1::0 mm-1::0 mm-2::0 mm-4::0");
    }
}

/// #980 review: a note edited on the twin by a device that hadn't folded it
/// yet reaches the bank account the twin was folded into here.
#[test]
fn note_on_an_already_folded_twin_reaches_the_bank_account() {
    let dir = temp_dir("twin-note");
    let (a, b) = (open(&dir, "a.db"), open(&dir, "b.db"));
    let ca = comdirect(&a);
    let cb = comdirect(&b);
    giro(&a, ca, MM_GIRO, &["mm-1", "mm-2"]);
    let bank = giro(&a, ca, "API1", &["mm-1", "mm-2", "b1"]);
    let twin = giro(&b, cb, MM_GIRO, &["mm-1", "mm-2"]);
    for v in [&a, &b] {
        exec(v, "UPDATE bank_connections SET created_ms = 1");
        exec(v, "UPDATE accounts SET created_ms = 1");
    }
    drop(a);
    let a = open(&dir, "a.db");
    b.set_account_note(twin, "Haushalt".into()).unwrap();
    merge(&a, &b);
    assert_eq!(
        text(&a, &format!("SELECT note FROM accounts WHERE id = {bank}")),
        "Haushalt"
    );
}

/// Issue #94: a closed MoneyMoney account and its successor share the
/// bookings MoneyMoney fetched again for the new one (`mm-` ids are content
/// hashes). A few shared bookings do not make them the same account.
#[test]
fn moneymoney_account_sharing_only_a_few_bookings_stays() {
    let dir = temp_dir("twin-few");
    let v = open(&dir, "a.db");
    let c = comdirect(&v);
    giro(&v, c, "API1", &["mm-1", "b1"]);
    giro(&v, c, MM_GIRO, &["mm-1", "mm-7", "mm-8", "mm-9"]);
    drop(v);
    let v = open(&dir, "a.db");
    assert_eq!(
        n(&v, "SELECT count(*) FROM accounts WHERE deleted_ms IS NULL"),
        2
    );
}

/// A depot without IBAN and account number is not told apart by its name.
#[test]
fn empty_depot_without_account_number_stays() {
    let dir = temp_dir("twin-depot-nonum");
    let v = open(&dir, "a.db");
    let c = comdirect(&v);
    exec(&v, "INSERT INTO account_groups(id, name) VALUES (1, 'G')");
    for ext in ["depot:E5AA", "moneymoney-db:22:4de55d4e"] {
        v.db()
            .unwrap()
            .execute(
                "INSERT INTO accounts(connection_id, group_id, external_ref, name, currency, kind)
                 VALUES (?1, 1, ?2, 'Depot', 'EUR', 'brokerage')",
                params![c, ext],
            )
            .unwrap();
    }
    drop(v);
    let v = open(&dir, "a.db");
    assert_eq!(
        n(&v, "SELECT count(*) FROM accounts WHERE deleted_ms IS NULL"),
        2
    );
}

/// Rules, documents and loan links of the twin go with it to the bank
/// account; the moved rule is stamped, so a merge does not move it back.
#[test]
fn rule_account_set_merges_across_vaults() {
    let dir = temp_dir("rule-accounts-merge");
    let (a, b) = (open(&dir, "a.db"), open(&dir, "b.db"));
    exec(
        &a,
        "INSERT INTO account_groups(id, name) VALUES (1, 'G');
         INSERT INTO accounts(group_id, external_ref, name) VALUES (1, 'A1', 'Eins'), (1, 'A2', 'Zwei'), (1, 'A3', 'Drei');",
    );
    let cat = a
        .create_category("Food".into(), None, "tag".into(), "#111111".into())
        .unwrap();
    let ids: Vec<i64> = (1..=3)
        .map(|i| {
            n(
                &a,
                &format!("SELECT id FROM accounts WHERE external_ref = 'A{i}'"),
            )
        })
        .collect();
    let mut r = plain_rule(cat, "lidl");
    r.account_ids = vec![ids[2], ids[0]];
    a.save_rule(r).unwrap();
    merge(&b, &a);
    let theirs = b.list_rules().unwrap();
    assert_eq!(theirs.len(), 1);
    let refs: Vec<String> = theirs[0]
        .account_ids
        .iter()
        .map(|id| {
            text(
                &b,
                &format!("SELECT external_ref FROM accounts WHERE id = {id}"),
            )
        })
        .collect();
    assert_eq!(refs, vec!["A1", "A3"]);
    both_ways(&a, &b);
    assert_eq!(a.list_rules().unwrap()[0].account_ids, vec![ids[0], ids[2]]);
}

#[test]
fn folded_twin_hands_over_rules_documents_and_loan_links() {
    let dir = temp_dir("twin-extras");
    let v = open(&dir, "a.db");
    let c = comdirect(&v);
    let bank = giro(&v, c, "API1", &["mm-1", "mm-2"]);
    let twin = giro(&v, c, MM_GIRO, &["mm-1", "mm-2"]);
    exec(
        &v,
        &format!(
            "INSERT INTO categories(name) VALUES ('Miete');
             INSERT INTO rules(name, category_id, pattern, account_id, updated_ms)
                 SELECT 'r', id, 'x', {twin}, 1 FROM categories WHERE name = 'Miete';
             INSERT INTO accounts(group_id, external_ref, name, kind) VALUES (1, 'L1', 'Kredit', 'loan');
             INSERT INTO booking_documents(booking_id, account_id, pdf_bytes)
                 SELECT id, {twin}, x'00' FROM transactions WHERE account_id = {twin} AND external_id = 'mm-1';
             INSERT INTO loan_links(transaction_id, loan_account_id)
                 SELECT t.id, a.id FROM transactions t, accounts a
                  WHERE t.account_id = {twin} AND t.external_id = 'mm-2' AND a.external_ref = 'L1';"
        ),
    );
    drop(v);
    let v = open(&dir, "a.db");
    assert_eq!(
        n(
            &v,
            &format!("SELECT account_id = {bank} AND updated_ms > 1 FROM rules")
        ),
        1
    );
    let bank_booking = |ext: &str| {
        n(
            &v,
            &format!(
                "SELECT id FROM transactions WHERE account_id = {bank} AND external_id = '{ext}'"
            ),
        )
    };
    assert_eq!(
        text(
            &v,
            "SELECT booking_id || ':' || account_id FROM booking_documents"
        ),
        format!("{}:{bank}", bank_booking("mm-1"))
    );
    assert_eq!(
        n(&v, "SELECT transaction_id FROM loan_links"),
        bank_booking("mm-2")
    );
}

// ------------------------------------- MoneyMoney twins without a bank login

const FINOM_IBAN: &str = "DE69100180000752999428";
const WALLET_IBAN: &str = "GB51TCCL04140405131923";

/// Account `ext` on login `c` (or none) with an IBAN and currency.
fn wallet(v: &Vault, c: Option<i64>, ext: &str, iban: &str, currency: &str) -> i64 {
    let db = v.db().unwrap();
    db.execute_batch("INSERT OR IGNORE INTO account_groups(id, name) VALUES (1, 'G')")
        .unwrap();
    db.execute(
        "INSERT INTO accounts(connection_id, group_id, external_ref, name, iban, currency, kind)
         VALUES (?1, 1, ?2, 'Girokonto', ?3, ?4, 'checking')",
        params![c, ext, iban, currency],
    )
    .unwrap();
    db.last_insert_rowid()
}

fn booking(v: &Vault, account: i64, ext: &str, fp: &str) {
    v.db()
        .unwrap()
        .execute(
            "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, import_fp)
             VALUES (?1, ?2, '2026-01-01', -100, ?3)",
            params![account, ext, fp],
        )
        .unwrap();
}

/// A login set up after the MoneyMoney import (Enable Banking) took over an
/// unlinked MoneyMoney account; another device brought the unlinked copy
/// back. It is a twin by IBAN and bookings: the fetch renamed most of them
/// to its own ids but kept their import fingerprints.
#[test]
fn unlinked_moneymoney_twin_is_folded_into_the_bank_account() {
    let dir = temp_dir("twin-unlinked");
    let v = open(&dir, "a.db");
    let c = comdirect(&v);
    let bank = wallet(&v, Some(c), FINOM_IBAN, FINOM_IBAN, "EUR");
    booking(&v, bank, "eb-1", "F1");
    booking(&v, bank, "eb-2", "F2");
    booking(&v, bank, "mm-F3-0", "F3");
    let twin = wallet(&v, None, "moneymoney-db:25:ba5a6b58", FINOM_IBAN, "EUR");
    booking(&v, twin, "mm-F1-0", "F1");
    booking(&v, twin, "mm-F2-0", "F2");
    booking(&v, twin, "mm-F3-0", "F3");
    drop(v);
    let v = open(&dir, "a.db");
    assert_eq!(
        n(&v, "SELECT count(*) FROM accounts WHERE deleted_ms IS NULL"),
        1
    );
    assert_eq!(
        n(
            &v,
            "SELECT count(*) FROM transactions WHERE deleted_ms IS NULL"
        ),
        3
    );
    let db = v.db().unwrap();
    assert_eq!(
        crate::sync_merge::twins::folded_into(&db, twin).unwrap(),
        Some(bank)
    );
}

/// Finom's EUR and USD wallets share one IBAN: each unlinked empty copy goes
/// to the wallet in its own currency.
#[test]
fn unlinked_empty_wallets_fold_by_currency() {
    let dir = temp_dir("twin-wallets");
    let v = open(&dir, "a.db");
    let c = comdirect(&v);
    let usd = wallet(&v, Some(c), "GB51:USD", WALLET_IBAN, "USD");
    let eur = wallet(&v, Some(c), "GB51:EUR", WALLET_IBAN, "EUR");
    wallet(&v, None, "moneymoney-db:26:ed66cd04", WALLET_IBAN, "USD");
    wallet(&v, None, "moneymoney-db:27:ed66cd04", WALLET_IBAN, "EUR");
    drop(v);
    let v = open(&dir, "a.db");
    assert_eq!(
        text(
            &v,
            "SELECT group_concat(id) FROM (SELECT id FROM accounts WHERE deleted_ms IS NULL ORDER BY id)"
        ),
        format!("{usd},{eur}")
    );
}

/// Without IBAN an unlinked account is not tied to any bank account.
#[test]
fn unlinked_moneymoney_account_without_iban_stays() {
    let dir = temp_dir("twin-noiban");
    let v = open(&dir, "a.db");
    let c = comdirect(&v);
    exec(&v, "INSERT INTO account_groups(id, name) VALUES (1, 'G')");
    exec(
        &v,
        &format!(
            "INSERT INTO accounts(connection_id, group_id, external_ref, name, account_number, kind)
             VALUES ({c}, 1, 'depot:1', 'Depot', '42', 'brokerage'),
                    (NULL, 1, 'moneymoney-db:9:x', 'Depot', '42', 'brokerage')"
        ),
    );
    drop(v);
    let v = open(&dir, "a.db");
    assert_eq!(
        n(&v, "SELECT count(*) FROM accounts WHERE deleted_ms IS NULL"),
        2
    );
}

/// B never fetched and holds the unlinked MoneyMoney account that A's fetch
/// took over: the merge maps them onto each other, B links it to the login.
#[test]
fn merge_matches_an_unlinked_moneymoney_account_a_fetch_took_over() {
    let dir = temp_dir("twin-merge-unlinked");
    let (a, b) = (open(&dir, "a.db"), open(&dir, "b.db"));
    let acc = wallet(&a, None, "moneymoney-db:25:ba5a6b58", FINOM_IBAN, "EUR");
    booking(&a, acc, "mm-F1-0", "F1");
    booking(&a, acc, "mm-F2-0", "F2");
    exec(&a, "UPDATE accounts SET created_ms = 1");
    both_ways(&a, &b);
    let c = comdirect(&a);
    exec(&a, "UPDATE bank_connections SET created_ms = 1");
    let mut remote = remote_account(FINOM_IBAN);
    remote.iban = FINOM_IBAN.into();
    let fetch = crate::connectors::FetchResult {
        accounts: vec![remote],
        transactions: vec![remote_tx(FINOM_IBAN, "b1")],
        ..Default::default()
    };
    crate::connectors::store_fetch(&a, ConnectorKind::Comdirect, c, fetch).unwrap();
    assert_eq!(text(&a, "SELECT external_ref FROM accounts"), FINOM_IBAN);
    both_ways(&a, &b);
    for v in [&a, &b] {
        assert_eq!(
            text(
                v,
                "SELECT group_concat(external_ref) FROM accounts WHERE deleted_ms IS NULL"
            ),
            FINOM_IBAN
        );
        assert_eq!(
            n(
                v,
                "SELECT count(*) FROM transactions WHERE deleted_ms IS NULL"
            ),
            3
        );
    }
}
#[test]
fn category_role_merge_converges_in_every_order() {
    let roles = [
        None,
        Some("Freizeit"),
        Some("Einnahmen"),
        Some("Umbuchungen"),
        Some("import:transfers"),
    ];
    let merge = |a: Option<&str>, b: Option<&str>| {
        let mut role = a.map(str::to_owned);
        super::categories::merge_category_role(&mut role, b.map(str::to_owned));
        role
    };
    for a in roles {
        assert_eq!(merge(a, a).as_deref(), a);
        for b in roles {
            assert_eq!(merge(a, b), merge(b, a));
            for c in roles {
                assert_eq!(
                    merge(merge(a, b).as_deref(), c),
                    merge(a, merge(b, c).as_deref()),
                    "{a:?}, {b:?}, {c:?}"
                );
            }
        }
    }
}

#[test]
fn deleting_the_last_account_of_a_login_drops_its_portal_cookies() {
    let v = open(&temp_dir("cookiedel"), "a.db");
    let (c, a1, a2, _) = bank_fixture(&v);
    let key = format!("web.secret_cookies.{c}");
    v.db()
        .unwrap()
        .execute(
            "INSERT INTO settings(key, value, is_secret) VALUES (?1, '[]', 1)",
            [&key],
        )
        .unwrap();
    let cookies = format!("SELECT count(*) FROM settings WHERE key = '{key}'");
    v.set_account_archived(a1, true).unwrap();
    v.set_account_archived(a2, true).unwrap();
    v.delete_account(a1).unwrap();
    assert_eq!(n(&v, &cookies), 1, "login still has an account");
    v.delete_account(a2).unwrap();
    assert_eq!(n(&v, &cookies), 0);
}

fn connection_settings(v: &Vault, c: i64) -> i64 {
    n(
        v,
        &format!(
            "SELECT count(*) FROM settings WHERE key IN ('pin.conn.{c}', 'pin.conn.{c}.login',
                 'fints.rah10.private.{c}', 'fints.rah10.counter.{c}', 'fints.rah10.bank_keys.{c}')"
        ),
    )
}

fn store_connection_secrets(v: &Vault, c: i64) {
    v.set_connection_pin(c, Some("1234".into())).unwrap();
    let db = v.db().unwrap();
    for key in [
        format!("pin.conn.{c}.login"),
        format!("fints.rah10.private.{c}"),
        format!("fints.rah10.counter.{c}"),
        format!("fints.rah10.bank_keys.{c}"),
    ] {
        db.execute(
            "INSERT INTO settings(key, value, is_secret) VALUES (?1, 'dummy', 1)",
            [key],
        )
        .unwrap();
    }
}

#[test]
fn merged_connection_deletion_purges_the_stored_secrets() {
    let dir = temp_dir("conndel-merge");
    let a = open(&dir, "a.db");
    let b = open(&dir, "b.db");
    let (ca, ..) = bank_fixture(&a);
    let (cb, ..) = bank_fixture(&b);
    store_connection_secrets(&b, cb);
    assert_eq!(connection_settings(&b, cb), 5);

    a.delete_bank_connection(ca).unwrap();
    merge(&b, &a);

    assert_eq!(
        n(
            &b,
            &format!("SELECT count(*) FROM bank_connections WHERE id = {cb} AND deleted_ms > 0")
        ),
        1
    );
    assert_eq!(connection_settings(&b, cb), 0);
}

#[test]
fn opening_purges_secrets_of_connections_deleted_earlier() {
    let dir = temp_dir("conndel-open");
    let v = open(&dir, "a.db");
    let (c, ..) = bank_fixture(&v);
    store_connection_secrets(&v, c);
    // a deletion merged by an older version: tombstone, secrets still there
    v.db()
        .unwrap()
        .execute(
            "UPDATE bank_connections SET deleted_ms = 1 WHERE id = ?1",
            [c],
        )
        .unwrap();
    assert_eq!(connection_settings(&v, c), 5);
    drop(v);
    let v = open(&dir, "a.db");
    assert_eq!(connection_settings(&v, c), 0);
}
