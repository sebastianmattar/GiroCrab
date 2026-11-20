use std::sync::Arc;

use rusqlite::params;

use super::READ;
use crate::models::KnownRecipient;
use crate::test_support::{TempDir, query_plan};
use crate::vault::Vault;

const KEY: [u8; 32] = [7u8; 32];
/// Martin's old and current IBAN, and one of a shop.
const OLD: &str = "DE89370400440532013000";
const NEW: &str = "DE02120300000000202051";
const SHOP: &str = "DE12500105170648489890";

fn open(dir: &TempDir, file: &str) -> Arc<Vault> {
    Vault::open(dir.join(file).to_string_lossy().into(), KEY.to_vec()).unwrap()
}

/// A booking on the one account (created on first use).
fn book(v: &Vault, name: &str, iban: &str, date: &str, amount_cents: i64) -> i64 {
    let c = v.db().unwrap();
    c.execute(
        "INSERT OR IGNORE INTO account_groups(name) VALUES ('G')",
        [],
    )
    .unwrap();
    c.execute(
        "INSERT OR IGNORE INTO accounts(group_id, external_ref, name) \
         VALUES ((SELECT id FROM account_groups WHERE name='G'), 'A1', 'Konto')",
        [],
    )
    .unwrap();
    c.execute(
        "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, counterparty, counterparty_iban) \
         VALUES ((SELECT id FROM accounts LIMIT 1), lower(hex(randomblob(8))), ?1, ?2, ?3, ?4)",
        params![date, amount_cents, name, iban],
    )
    .unwrap();
    c.last_insert_rowid()
}

fn ibans(list: &[KnownRecipient]) -> Vec<&str> {
    list.iter().map(|r| r.iban.as_str()).collect()
}

/// Martin, as in #978: years of transfers to the old IBAN under two spellings, then the new one.
fn martin(v: &Vault) {
    for d in ["2020-01-05", "2020-02-05", "2020-03-05"] {
        book(v, "MARTIN MUSTERMANN", OLD, d, -100);
    }
    book(
        v,
        "Martin Mustermann",
        "DE89 3704 0044 0532 0130 00",
        "2021-06-05",
        -100,
    );
    book(v, "Martin Mustermann", NEW, "2026-10-05", -100);
}

#[test]
fn one_entry_per_iban_newest_first_under_the_most_used_name() {
    let dir = TempDir::new("recipients-order");
    let v = open(&dir, "v.db");
    martin(&v);
    let s = v.suggest_recipients("mustermann".into(), 5).unwrap();
    assert_eq!(ibans(&s), [NEW, OLD]);
    assert_eq!(
        s[1],
        KnownRecipient {
            name: "MARTIN MUSTERMANN".into(),
            iban: OLD.into(),
            times_used: 4,
            last_used: "2021-06-05".into(),
            hidden: false,
        }
    );
    // the limit counts IBANs, not spellings
    assert_eq!(
        ibans(&v.suggest_recipients("martin".into(), 2).unwrap()),
        [NEW, OLD]
    );
}

/// One name under both spellings of an IBAN counts once: 3 + 3 uses beat a rival's 4.
#[test]
fn a_name_under_both_iban_spellings_sums_its_uses() {
    let dir = TempDir::new("recipients-spellings");
    let v = open(&dir, "v.db");
    for d in ["2020-01-05", "2020-02-05", "2020-03-05"] {
        book(&v, "Alice", OLD, d, -100);
        book(&v, "Alice", "DE89 3704 0044 0532 0130 00", d, -100);
    }
    for d in ["2020-01-06", "2020-02-06", "2020-03-06", "2020-04-06"] {
        book(&v, "Bob", OLD, d, -100);
    }
    let s = v.suggest_recipients(String::new(), 5).unwrap();
    assert_eq!(ibans(&s), [OLD]);
    assert_eq!((s[0].name.as_str(), s[0].times_used), ("Alice", 10));
}

#[test]
fn search_matches_every_spelling_and_the_iban() {
    let dir = TempDir::new("recipients-search");
    let v = open(&dir, "v.db");
    martin(&v);
    book(&v, "M. Mustermann", OLD, "2019-01-01", -100);
    book(&v, "Laden GmbH", SHOP, "2026-01-01", -100);
    let found = |q: &str| ibans(&v.suggest_recipients(q.into(), 5).unwrap()).join(",");
    assert_eq!(found("m.muster"), OLD);
    assert_eq!(found("de89 3704"), OLD);
    assert_eq!(found("laden"), SHOP);
    assert_eq!(found(""), [NEW, SHOP, OLD].join(","));
}

#[test]
fn only_live_outgoing_bookings_with_a_valid_iban_count() {
    let dir = TempDir::new("recipients-filter");
    let v = open(&dir, "v.db");
    book(&v, "Arbeitgeber", OLD, "2026-01-01", 250_000);
    book(&v, "Kaputt", "DE00123", "2026-01-01", -100);
    book(&v, "", NEW, "2026-01-01", -100);
    let gone = book(&v, "Gelöscht", SHOP, "2026-01-01", -100);
    v.db()
        .unwrap()
        .execute(
            "UPDATE transactions SET deleted_ms = 1 WHERE id = ?1",
            [gone],
        )
        .unwrap();
    assert!(v.list_known_recipients(String::new()).unwrap().is_empty());
}

#[test]
fn a_hidden_iban_is_no_longer_suggested_until_shown_again() {
    let dir = TempDir::new("recipients-hide");
    let v = open(&dir, "v.db");
    martin(&v);
    v.set_recipient_hidden("de89 3704 0044 0532 0130 00".into(), true)
        .unwrap();
    assert_eq!(
        ibans(&v.suggest_recipients("martin".into(), 5).unwrap()),
        [NEW]
    );
    let all = v.list_known_recipients("martin".into()).unwrap();
    assert_eq!(ibans(&all), [NEW, OLD]);
    assert_eq!(
        all.iter().map(|r| r.hidden).collect::<Vec<_>>(),
        [false, true]
    );
    // hiding again writes nothing
    let before = v.change_count();
    v.set_recipient_hidden(OLD.into(), true).unwrap();
    assert_eq!(v.change_count(), before);

    v.set_recipient_hidden(OLD.into(), false).unwrap();
    assert_eq!(
        ibans(&v.suggest_recipients("martin".into(), 5).unwrap()),
        [NEW, OLD]
    );
    // the bookings were never touched
    assert_eq!(
        v.list_known_recipients(String::new()).unwrap()[1].times_used,
        4
    );
    assert!(v.set_recipient_hidden("DE00123".into(), true).is_err());
}

#[test]
fn a_new_booking_reaches_the_cached_list() {
    let dir = TempDir::new("recipients-cache");
    let v = open(&dir, "v.db");
    martin(&v);
    assert_eq!(v.suggest_recipients("laden".into(), 5).unwrap().len(), 0);
    book(&v, "Laden GmbH", SHOP, "2026-01-01", -100);
    assert_eq!(
        ibans(&v.suggest_recipients("laden".into(), 5).unwrap()),
        [SHOP]
    );
}

#[test]
fn a_restored_backup_replaces_the_cached_list() {
    let dir = TempDir::new("recipients-restore");
    let v = open(&dir, "v.db");
    martin(&v);
    v.set_recipient_hidden(OLD.into(), true).unwrap();
    assert_eq!(v.list_known_recipients(String::new()).unwrap().len(), 2);
    let backup = dir
        .join("empty.girocrab-backup")
        .to_string_lossy()
        .to_string();
    open(&dir, "empty.db")
        .write_backup(backup.clone(), "backup-passwort".into())
        .unwrap();
    // the backup API replaces the pages without moving the change count
    let before = v.change_count();
    v.restore_backup(backup, "backup-passwort".into()).unwrap();
    assert_eq!(v.change_count(), before);
    assert!(v.list_known_recipients(String::new()).unwrap().is_empty());
    assert!(v.suggest_recipients("martin".into(), 5).unwrap().is_empty());
}

#[test]
fn the_read_stays_on_the_covering_index() {
    let dir = TempDir::new("recipients-plan");
    let v = open(&dir, "v.db");
    let plan = query_plan(&v.db().unwrap(), READ, []);
    assert!(plan.contains("COVERING INDEX tx_recipients"), "{plan}");
    assert!(!plan.contains("TEMP B-TREE"), "{plan}");
}

fn hidden_rows(v: &Vault) -> Vec<(String, i64)> {
    v.db()
        .unwrap()
        .prepare("SELECT iban, hidden FROM hidden_recipients ORDER BY iban")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn hiding_and_showing_again_reach_the_other_device() {
    let dir = TempDir::new("recipients-merge");
    let a = open(&dir, "a.db");
    let b = open(&dir, "b.db");
    b.set_recipient_hidden(OLD.into(), true).unwrap();
    b.set_recipient_hidden(SHOP.into(), true).unwrap();
    a.merge_peer_snapshot(b.file_path()).unwrap();
    assert_eq!(hidden_rows(&a), [(SHOP.into(), 1), (OLD.into(), 1)]);

    // shown again on a: the newer change wins on b, and a repeated merge changes nothing
    a.set_recipient_hidden(OLD.into(), false).unwrap();
    b.merge_peer_snapshot(a.file_path()).unwrap();
    assert_eq!(hidden_rows(&b), [(SHOP.into(), 1), (OLD.into(), 0)]);
    assert_eq!(b.merge_peer_snapshot(a.file_path()).unwrap().changes, 0);
    a.merge_peer_snapshot(b.file_path()).unwrap();
    assert_eq!(hidden_rows(&a), hidden_rows(&b));
}

#[test]
fn hiding_wins_an_equal_stamp_in_either_merge_order() {
    let dir = TempDir::new("recipients-tie");
    let a = open(&dir, "a.db");
    let b = open(&dir, "b.db");
    for (v, hidden) in [(&a, 1), (&b, 0)] {
        v.db()
            .unwrap()
            .execute(
                "INSERT INTO hidden_recipients(iban, hidden, changed_ms) VALUES (?1, ?2, 5)",
                params![OLD, hidden],
            )
            .unwrap();
    }
    a.merge_peer_snapshot(b.file_path()).unwrap();
    b.merge_peer_snapshot(a.file_path()).unwrap();
    assert_eq!(hidden_rows(&a), [(OLD.into(), 1)]);
    assert_eq!(hidden_rows(&b), [(OLD.into(), 1)]);
}
