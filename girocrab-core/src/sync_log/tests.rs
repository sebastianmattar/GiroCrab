use std::sync::Arc;

use rusqlite::params;

use super::*;

fn vault(tag: &str) -> Arc<Vault> {
    Vault::open(
        crate::test_support::thread_temp_path(tag)
            .to_string_lossy()
            .into(),
        vec![7u8; 32],
    )
    .unwrap()
}

fn connection(v: &Vault, name: &str, login: &str) -> i64 {
    let db = v.db().unwrap();
    db.execute(
        "INSERT INTO bank_connections(connector, display_name, login_name) VALUES ('fints', ?1, ?2)",
        params![name, login],
    )
    .unwrap();
    db.last_insert_rowid()
}

fn all(v: &Vault) -> Vec<SyncLogEntry> {
    v.list_sync_log(SyncLogFilter::default()).unwrap()
}

#[test]
fn scrub_removes_secrets_but_keeps_the_error() {
    let s = scrub(
        "Anmeldung für Kunde max.muster fehlgeschlagen: PIN=12345 abgelehnt, \"token\":\"abc.def\", Authorization: Bearer abcdefgh12345678",
        "max.muster",
    );
    assert!(!s.contains("max.muster"), "{s}");
    assert!(!s.contains("12345"), "{s}");
    assert!(!s.contains("abc.def"), "{s}");
    assert!(!s.contains("abcdefgh"), "{s}");
    assert!(s.contains("fehlgeschlagen"), "{s}");
    assert!(s.contains("abgelehnt"), "{s}");
}

#[test]
fn scrub_masks_ibans_and_long_numbers_only() {
    let s = scrub(
        "Konto DE89 3704 0044 0532 0130 00 und Kundennummer 1234567 (Code 9050, Zeile 12)",
        "",
    );
    assert!(s.contains("DE89…3000"), "{s}");
    assert!(!s.contains("1234567"), "{s}");
    // short codes stay readable
    assert!(s.contains("9050") && s.contains("Zeile 12"), "{s}");
}

#[test]
fn scrub_iban_stops_at_the_next_word_and_ignores_case() {
    let s = scrub("Konto DE89 3704 0044 0532 0130 00 HTTP 403", "");
    assert_eq!(s, "Konto DE89…3000 HTTP 403");
    let s = scrub("konto de89 3704 0044 0532 0130 00 gesperrt", "");
    assert_eq!(s, "konto de89…3000 gesperrt");
    // letters and digits in groups, but no IBAN
    assert_eq!(scrub("ab12 test case", ""), "ab12 test case");
    // AT/BE end on a full group of four: the next word must stay
    assert_eq!(
        scrub("Konto AT61 1904 3002 3457 3201 Konto gesperrt", ""),
        "Konto AT61…3201 Konto gesperrt"
    );
    assert_eq!(
        scrub("BE68 5390 0754 7034 Code 9050", ""),
        "BE68…7034 Code 9050"
    );
    // irregular spacing
    assert_eq!(
        scrub("an DE8937 0400 4405 3201 3000 ok", ""),
        "an DE89…3000 ok"
    );
    // two IBANs in a row
    assert_eq!(
        scrub("von DE89370400440532013000 AT611904300234573201", ""),
        "von DE89…3000 AT61…3201"
    );
}

#[test]
fn scrub_ends_a_short_iban_at_its_last_group_and_keeps_hex_ids() {
    assert_eq!(
        scrub("Konto DE89 3704 0044 0532 Konto gesperrt", ""),
        "Konto DE89…0532 Konto gesperrt"
    );
    // lower-case, unspaced: only with a valid checksum
    let id = "consent de12a3b4c5d6e7f8a9b0c1d2 ok";
    assert_eq!(scrub(id, ""), id);
    assert_eq!(
        scrub("iban de89370400440532013000 ok", ""),
        "iban de89…3000 ok"
    );
    // upper-case and glued: still an IBAN (compact form pasted into text)
    assert_eq!(scrub("DE89370400440532013000abc", ""), "DE89…3000abc");
}

#[test]
fn scrub_keeps_the_words_after_a_short_iban_and_umlaut_words() {
    assert_eq!(
        scrub("Konto DE12 3456 7890 fehlt", ""),
        "Konto DE12…7890 fehlt"
    );
    assert_eq!(
        scrub("IBAN DE89 3704 0044 ungültig", ""),
        "IBAN DE89…0044 ungültig"
    );
    assert_eq!(
        scrub("IBAN DE89 3704 0044 0532 0130 00 ungültig", ""),
        "IBAN DE89…3000 ungültig"
    );
}

#[test]
fn scrub_handles_other_blanks_and_a_word_at_the_cut() {
    assert_eq!(
        scrub("Konto DE89 3704 0044 0532 Fehler 401", ""),
        "Konto DE89…0532 Fehler 401"
    );
    assert_eq!(
        scrub(
            "Konto DE89\u{a0}3704\u{a0}0044\u{a0}0532\u{a0}0130\u{a0}00 gesperrt",
            ""
        ),
        "Konto DE89…3000 gesperrt"
    );
    assert_eq!(
        scrub("Konto DE89  3704  0044  0532  0130  00 gesperrt", ""),
        "Konto DE89…3000 gesperrt"
    );
    assert_eq!(
        scrub("Konto DE89\t3704 0044 0532 0130 00", ""),
        "Konto DE89…3000"
    );
}

#[test]
fn scrub_survives_unicode_that_case_folds_to_ascii() {
    // U+212A KELVIN SIGN folds to "k": must neither match nor split a char
    let m = "Konto \u{212A}E89 3704 0044 0532 0130 00 und \u{212A}\u{212A}12 3456 7890 1234";
    let s = scrub(m, "");
    assert!(s.starts_with("Konto \u{212A}E89"), "{s}");
}

#[test]
fn scrub_keeps_plain_words_about_tans() {
    // no `key=value`: nothing to hide, so the text stays diagnosable
    let m = "TAN erforderlich, aber das Verfahren wird nicht unterstützt";
    assert_eq!(scrub(m, ""), m);
}

#[test]
fn scrub_cuts_long_messages() {
    let long = "x".repeat(5000);
    let s = scrub(&long, "");
    assert_eq!(s.chars().count(), MAX_MESSAGE_CHARS + 1);
    assert!(s.ends_with('…'));
}

#[test]
fn logs_lists_newest_first_and_filters() {
    let v = vault("synclog-list");
    let a = connection(&v, "Sparkasse", "kunde-a");
    let b = connection(&v, "ING", "kunde-b");
    for (i, (conn, outcome, msg)) in [
        (a, SyncLogOutcome::Ok, ""),
        (b, SyncLogOutcome::Error, "Zeitüberschreitung beim Abruf"),
        (a, SyncLogOutcome::Error, "PIN falsch (kunde-a)"),
        (b, SyncLogOutcome::Cancelled, ""),
    ]
    .into_iter()
    .enumerate()
    {
        v.log_sync_at(
            (Some(conn), SyncLogSource::Manual, outcome, msg, 0),
            1_000_000 + i as i64,
            KEEP_MS,
            MAX_BYTES,
        )
        .unwrap();
    }
    let log = all(&v);
    assert_eq!(log.len(), 4);
    assert_eq!(log[0].outcome, SyncLogOutcome::Cancelled);
    assert_eq!(log[0].connection_name, "ING");
    // the login never reaches the log
    assert!(log.iter().all(|e| !e.message.contains("kunde-a")));

    let errors = v
        .list_sync_log(SyncLogFilter {
            only_errors: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(errors.len(), 2);
    let of_a = v
        .list_sync_log(SyncLogFilter {
            connection_id: Some(a),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(of_a.len(), 2);
    let found = v
        .list_sync_log(SyncLogFilter {
            search: Some("zeitüber ing".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(found.len(), 1);
    let limited = v
        .list_sync_log(SyncLogFilter {
            limit: 2,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(limited.len(), 2);
}

#[test]
fn keeps_the_name_after_the_connection_is_deleted() {
    let v = vault("synclog-deleted");
    let a = connection(&v, "Sparkasse", "x-login");
    v.log_sync(
        Some(a),
        SyncLogSource::Automatic,
        SyncLogOutcome::Ok,
        String::new(),
        3,
    )
    .unwrap();
    v.db()
        .unwrap()
        .execute("DELETE FROM bank_connections WHERE id = ?1", [a])
        .unwrap();
    let log = all(&v);
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].connection_name, "Sparkasse");
    assert_eq!(log[0].imported, 3);
    assert_eq!(log[0].source, SyncLogSource::Automatic);
}

#[test]
fn drops_rows_older_than_seven_days() {
    let v = vault("synclog-age");
    let day = 24 * 60 * 60 * 1000;
    let now = 100 * day;
    for age_days in [9, 8, 6, 0] {
        v.log_sync_at(
            (None, SyncLogSource::Manual, SyncLogOutcome::Ok, "", 0),
            now - age_days * day,
            KEEP_MS,
            MAX_BYTES,
        )
        .unwrap();
    }
    let ages: Vec<i64> = all(&v)
        .iter()
        .map(|e| (now - e.timestamp_ms) / day)
        .collect();
    assert_eq!(ages, vec![0, 6]);
}

#[test]
fn drops_the_oldest_rows_beyond_the_size_cap() {
    let v = vault("synclog-size");
    let msg = "m".repeat(100);
    // one row counts 100 + 0 + ROW_OVERHEAD; room for three
    let cap = 3 * (100 + ROW_OVERHEAD);
    for i in 0..6 {
        v.log_sync_at(
            (None, SyncLogSource::Manual, SyncLogOutcome::Error, &msg, 0),
            1_000 + i,
            KEEP_MS * 1_000_000,
            cap,
        )
        .unwrap();
    }
    let log = all(&v);
    assert_eq!(log.len(), 3);
    assert_eq!(log[0].timestamp_ms, 1_005, "the newest rows stay");
    assert_eq!(log[2].timestamp_ms, 1_003);
}

#[test]
fn clear_empties_the_log() {
    let v = vault("synclog-clear");
    v.log_sync(
        None,
        SyncLogSource::Manual,
        SyncLogOutcome::Ok,
        String::new(),
        0,
    )
    .unwrap();
    v.clear_sync_log().unwrap();
    assert!(all(&v).is_empty());
}
