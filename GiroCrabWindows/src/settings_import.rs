//! Pure texts and checks behind the "Import, Export & Einstellungen" dialog
//! (`ui/settings_import_dialog.slint`, wired in `settings_import_ui.rs`): file checks before an
//! import, the Amazon capture check, export rows, backup rules, settings lines and the
//! Abrufprotokoll. No vault access here, so everything is unit-testable.

use std::collections::HashSet;

use girocrab_core::export::{ExportCsvRow, esc};
use girocrab_core::iban::has_iban_shape;
use girocrab_core::importers::amazon_orders::{
    Capture, bookings, is_asin, parse_date, price_cents,
};
use girocrab_core::importers::moneymoney_db::{
    MoneyMoneyDbImportProgress, MoneyMoneyDbImportResult,
};
use girocrab_core::importers::moneymoney_import::{
    MmRow, fingerprint, parse_camt_statement, parse_csv_export, parse_export,
    parse_mt940_statement, parse_xml_export, split_category,
};
use girocrab_core::sync_log::{SyncLogEntry, SyncLogOutcome, SyncLogSource};
use girocrab_core::{Booking, MoneyMoneyImportPreview, MoneyMoneyImportResult, SettingEntry};

/// One list line of the dialog.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Line {
    pub title: String,
    pub detail: String,
}

/// FinTS product registration choices, in menu order (stored values of the core).
pub const PRODUCT_MODES: [(&str, &str); 3] = [
    ("", "Noch nicht festgelegt"),
    ("builtin", "Eingebaute Registrierung"),
    ("own", "Eigene Produkt-ID"),
];

pub fn product_mode_names() -> Vec<String> {
    PRODUCT_MODES.iter().map(|m| m.1.to_string()).collect()
}

pub fn product_mode_index(mode: &str) -> i32 {
    PRODUCT_MODES.iter().position(|m| m.0 == mode).unwrap_or(0) as i32
}

pub fn product_mode_value(index: i32) -> &'static str {
    usize::try_from(index)
        .ok()
        .and_then(|i| PRODUCT_MODES.get(i))
        .map_or("", |m| m.0)
}

pub fn product_source_text(source: &str) -> String {
    match source {
        "builtin" => "Verwendet wird die eingebaute Registrierung.".into(),
        "own" => "Verwendet wird die eigene Produkt-ID.".into(),
        _ => "Es ist keine Registrierung verfügbar: eigene Produkt-ID eintragen.".into(),
    }
}

// ---- import file check ---------------------------------------------------------------------------

/// What a statement file contains, before the core's dry run.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FileCheck {
    pub format: &'static str,
    pub rows: usize,
    pub with_valid_iban: usize,
    /// Rows whose fingerprint already occurred earlier in the same file.
    pub repeated_in_file: usize,
    pub categories: Vec<String>,
}

/// Parses a statement with the parser of its format; unknown content falls back to detection.
pub fn parse_statement(name: &str, data: &[u8]) -> Result<(&'static str, Vec<MmRow>), String> {
    let lower = name.to_lowercase();
    let text = String::from_utf8_lossy(data);
    let (format, rows) = if lower.ends_with(".sta") || lower.ends_with(".mt940") {
        ("MT940", parse_mt940_statement(&text))
    } else if lower.ends_with(".csv") {
        ("CSV", parse_csv_export(&text).map_err(|e| e.to_string())?)
    } else if lower.ends_with(".xml") && text.contains("camt.053") {
        ("CAMT.053", parse_camt_statement(&text))
    } else if lower.ends_with(".xml") {
        ("XML", parse_xml_export(&text).map_err(|e| e.to_string())?)
    } else {
        ("Erkannt", parse_export(data).map_err(|e| e.to_string())?)
    };
    if rows.is_empty() {
        // The content may differ from the extension: let the core detect it.
        let rows = parse_export(data).map_err(|e| e.to_string())?;
        return Ok(("Erkannt", rows));
    }
    Ok((format, rows))
}

pub fn check_rows(format: &'static str, rows: &[MmRow]) -> FileCheck {
    let mut seen = HashSet::new();
    let mut repeated = 0;
    let mut cats: Vec<String> = vec![];
    for r in rows {
        let key = fingerprint(
            &r.booking_date,
            r.amount_cents,
            &r.iban,
            &r.name,
            &r.purpose,
        );
        if !seen.insert(key) {
            repeated += 1;
        }
        let path = if r.category_parts.is_empty() {
            split_category(&r.category)
        } else {
            r.category_parts.clone()
        };
        if !path.is_empty() {
            cats.push(path.join(" › "));
        }
    }
    cats.sort();
    cats.dedup();
    FileCheck {
        format,
        rows: rows.len(),
        with_valid_iban: rows.iter().filter(|r| has_iban_shape(&r.iban)).count(),
        repeated_in_file: repeated,
        categories: cats,
    }
}

pub fn file_check_text(c: &FileCheck) -> String {
    let mut s = format!(
        "{}: {} Zeilen, davon {} mit gültiger IBAN.",
        c.format, c.rows, c.with_valid_iban
    );
    if c.repeated_in_file > 0 {
        s.push_str(&format!(
            " {} Zeilen kommen in der Datei doppelt vor.",
            c.repeated_in_file
        ));
    }
    if !c.categories.is_empty() {
        s.push_str(&format!(" {} Kategorien erkannt.", c.categories.len()));
    }
    s
}

pub fn preview_text(p: &MoneyMoneyImportPreview, date: &dyn Fn(&str) -> String) -> String {
    let mut s = format!(
        "{} Zeilen: {} neu, {} bereits vorhanden",
        p.total_rows, p.new_bookings, p.duplicates_skipped_or_enriched
    );
    if let (Some(a), Some(b)) = (&p.date_range_start, &p.date_range_end) {
        s.push_str(&format!(", {} bis {}", date(a), date(b)));
    }
    if p.notes_count > 0 {
        s.push_str(&format!(", {} Notizen", p.notes_count));
    }
    s.push('.');
    s
}

pub fn import_result_text(r: &MoneyMoneyImportResult) -> String {
    format!(
        "Import: {} neu, {} bereits vorhanden, {} ergänzt{}.",
        r.imported,
        r.duplicates,
        r.enriched,
        if r.account_created {
            " (neues Konto angelegt)"
        } else {
            ""
        }
    )
}

pub fn db_result_text(r: &MoneyMoneyDbImportResult) -> String {
    format!(
        "Datenimport: {} neue Umsätze, {} Duplikate, {} ergänzt, {} Konten neu, {} zusammengeführt, {} Kategorien angelegt.",
        r.bookings_new,
        r.bookings_duplicates,
        r.bookings_enriched,
        r.accounts_created,
        r.accounts_merged,
        r.categories_created
    )
}

pub fn progress_text(p: &MoneyMoneyDbImportProgress) -> String {
    if p.account_index == 0 {
        return "Datenbank wird gelesen …".into();
    }
    if p.account_name.is_empty() {
        return "Wird abgeschlossen …".into();
    }
    format!(
        "Konto {} von {}: {} ({} von {} Umsätzen)",
        p.account_index, p.account_count, p.account_name, p.bookings_done, p.bookings_total
    )
}

// ---- Amazon capture ------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Default)]
pub struct AmazonCheck {
    pub orders: usize,
    pub cancelled: usize,
    pub items: usize,
    pub items_without_asin: usize,
    pub bad_dates: usize,
    pub bookings: usize,
    pub total_cents: i64,
    /// Orders whose summary rows carry a readable price.
    pub priced_orders: usize,
}

pub fn amazon_check(json: &str) -> Result<AmazonCheck, String> {
    let capture: Capture = serde_json::from_str(json)
        .map_err(|_| "Das ist kein Bestell-Export (JSON).".to_string())?;
    if capture.orders.is_empty() {
        return Err("Keine Bestellungen erkannt.".into());
    }
    let items: Vec<_> = capture.orders.iter().flat_map(|o| &o.items).collect();
    let bks = bookings(&capture.orders);
    Ok(AmazonCheck {
        orders: capture.orders.len(),
        cancelled: capture.orders.iter().filter(|o| o.cancelled).count(),
        items: items.len(),
        items_without_asin: items.iter().filter(|i| !is_asin(&i.asin)).count(),
        bad_dates: capture
            .orders
            .iter()
            .filter(|o| parse_date(&o.date).is_none())
            .count(),
        bookings: bks.len(),
        total_cents: bks.iter().map(|b| b.cents).sum(),
        priced_orders: capture
            .orders
            .iter()
            .filter(|o| o.rows.iter().any(|r| price_cents(&r.value).is_some()))
            .count(),
    })
}

pub fn amazon_text(c: &AmazonCheck, eur: &dyn Fn(i64) -> String) -> String {
    let mut s = format!(
        "{} Bestellungen ({} storniert), {} Artikel, {} Buchungen über {}.",
        c.orders,
        c.cancelled,
        c.items,
        c.bookings,
        eur(c.total_cents)
    );
    if c.bad_dates > 0 {
        s.push_str(&format!(
            " {} Bestellungen ohne lesbares Datum.",
            c.bad_dates
        ));
    }
    if c.items_without_asin > 0 {
        s.push_str(&format!(
            " {} Artikel ohne Produkt-ID.",
            c.items_without_asin
        ));
    }
    s
}

// ---- export ----------------------------------------------------------------------------------------

pub fn export_rows(list: &[Booking]) -> Vec<ExportCsvRow> {
    list.iter()
        .map(|b| ExportCsvRow {
            date: b.booking_date.chars().take(10).collect(),
            account_name: b.account_name.clone(),
            counterparty: b.counterparty.clone(),
            counterparty_iban: b.counterparty_iban.clone(),
            purpose: b.purpose.clone(),
            category: b.category_path.clone(),
            note: b.note.clone(),
            tags: crate::planning::tag_summary(&b.tags),
            amount_cents: b.amount_cents,
            currency: b.currency.clone(),
            day_balance_cents: None,
            balance_cents: None,
        })
        .collect()
}

/// `YYYY-MM-DD` as typed; empty = no limit. Anything else is an error.
pub fn parse_day(text: &str) -> Result<Option<String>, String> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(None);
    }
    let ok = t.len() == 10
        && t.char_indices().all(|(i, c)| match i {
            4 | 7 => c == '-',
            _ => c.is_ascii_digit(),
        })
        && matches!(t[5..7].parse::<u32>(), Ok(1..=12))
        && matches!(t[8..10].parse::<u32>(), Ok(1..=31));
    if ok {
        Ok(Some(t.to_string()))
    } else {
        Err(format!("„{t}“ ist kein Datum (JJJJ-MM-TT)."))
    }
}

pub fn export_file_name(kind: &str, today: &str) -> String {
    match kind {
        "camt" => format!("girocrab-camt053-{today}.xml"),
        _ => format!("girocrab-umsaetze-{today}.csv"),
    }
}

/// Days since 1970-01-01 → (year, month, day), proleptic Gregorian.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// `2025-03-31T12:00:00+00:00` for the `CreDtTm` of a CAMT export.
pub fn iso_utc(unix_secs: i64) -> String {
    let (y, m, d) = civil(unix_secs.div_euclid(86_400));
    let s = unix_secs.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}+00:00",
        s / 3600,
        s % 3600 / 60,
        s % 60
    )
}

pub fn today(unix_secs: i64) -> String {
    iso_utc(unix_secs)[..10].to_string()
}

// ---- backup ------------------------------------------------------------------------------------------

pub fn validate_backup_password(pw: &str, repeat: &str, writing: bool) -> Result<(), String> {
    if pw.is_empty() {
        return Err("Bitte ein Passwort für die Sicherung angeben.".into());
    }
    // Same floor as the core's `write_backup` (and the Mac dialog): the backup is only
    // as strong as this password. Restoring takes any password, older backups may be shorter.
    if writing && pw.chars().count() < 8 {
        return Err("Das Passwort für die Sicherung muss mindestens 8 Zeichen lang sein.".into());
    }
    if writing && pw != repeat {
        return Err("Die beiden Passwörter stimmen nicht überein.".into());
    }
    Ok(())
}

// ---- settings --------------------------------------------------------------------------------------------

pub fn setting_lines(list: &[SettingEntry]) -> Vec<Line> {
    list.iter()
        .map(|s| Line {
            title: s.label.clone(),
            detail: format!(
                "{} · {}",
                s.group_title,
                if s.is_secret {
                    if s.has_value {
                        "gesetzt (verborgen)".to_string()
                    } else {
                        "nicht gesetzt".to_string()
                    }
                } else if s.value.is_empty() {
                    "leer".to_string()
                } else {
                    s.value.clone()
                }
            ),
        })
        .collect()
}

// ---- Abrufprotokoll ----------------------------------------------------------------------------------------

pub fn outcome_name(o: SyncLogOutcome) -> &'static str {
    match o {
        SyncLogOutcome::Ok => "Erfolgreich",
        SyncLogOutcome::Error => "Fehler",
        SyncLogOutcome::Cancelled => "Abgebrochen",
    }
}

pub fn source_name(s: SyncLogSource) -> &'static str {
    match s {
        SyncLogSource::Manual => "Manuell",
        SyncLogSource::Automatic => "Automatisch",
        SyncLogSource::Statements => "Kontoauszüge",
    }
}

/// `dd.mm.yyyy hh:mm` (UTC) of a log timestamp.
pub fn stamp(ms: i64) -> String {
    let iso = iso_utc(ms.div_euclid(1000));
    format!(
        "{}.{}.{} {}",
        &iso[8..10],
        &iso[5..7],
        &iso[..4],
        &iso[11..16]
    )
}

pub fn log_lines(list: &[SyncLogEntry]) -> Vec<Line> {
    list.iter()
        .map(|e| {
            let mut detail = format!(
                "{} · {} · {}",
                stamp(e.timestamp_ms),
                outcome_name(e.outcome),
                source_name(e.source)
            );
            if e.outcome == SyncLogOutcome::Ok {
                detail.push_str(&format!(" · {} neu", e.imported));
            }
            if !e.message.is_empty() {
                detail.push_str(&format!(" · {}", e.message));
            }
            Line {
                title: e.connection_name.clone(),
                detail,
            }
        })
        .collect()
}

pub fn log_xml(list: &[SyncLogEntry]) -> String {
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<syncLog>\n");
    for e in list {
        out.push_str(&format!(
            "  <entry time=\"{}\" connection=\"{}\" source=\"{}\" outcome=\"{}\" imported=\"{}\">{}</entry>\n",
            esc(&stamp(e.timestamp_ms)),
            esc(&e.connection_name),
            source_name(e.source),
            outcome_name(e.outcome),
            e.imported,
            esc(&e.message)
        ));
    }
    out.push_str("</syncLog>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(date: &str, cents: i64, iban: &str, cat: &str) -> MmRow {
        MmRow {
            booking_date: date.into(),
            amount_cents: cents,
            iban: iban.into(),
            name: "Shop".into(),
            purpose: "Kauf".into(),
            category: cat.into(),
            ..Default::default()
        }
    }

    #[test]
    fn dates_and_stamps() {
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00+00:00");
        assert_eq!(iso_utc(1_743_422_400), "2025-03-31T12:00:00+00:00");
        assert_eq!(today(1_743_422_400), "2025-03-31");
        assert_eq!(stamp(1_743_422_400_000), "31.03.2025 12:00");
        assert_eq!(iso_utc(-1), "1969-12-31T23:59:59+00:00");
    }

    #[test]
    fn day_input() {
        assert_eq!(parse_day("  "), Ok(None));
        assert_eq!(parse_day("2025-03-01"), Ok(Some("2025-03-01".into())));
        assert!(parse_day("01.03.2025").is_err());
        assert!(parse_day("2025-13-01").is_err());
    }

    #[test]
    fn file_check_counts() {
        let rows = vec![
            row("2025-01-01", -500, "DE89370400440532013000", "Essen / Café"),
            row("2025-01-01", -500, "DE89370400440532013000", "Essen / Café"),
            row("2025-01-02", 100, "kaputt", ""),
        ];
        let c = check_rows("CSV", &rows);
        assert_eq!(c.rows, 3);
        assert_eq!(c.with_valid_iban, 2);
        assert_eq!(c.repeated_in_file, 1);
        assert_eq!(c.categories, vec!["Essen › Café".to_string()]);
        let t = file_check_text(&c);
        assert!(t.contains("3 Zeilen") && t.contains("1 Zeilen kommen"));
    }

    #[test]
    fn statement_parsing_by_name() {
        assert!(parse_statement("x.csv", b"nonsense").is_err());
        let csv = "\"Datum\";\"Wertstellung\";\"Kategorie\";\"Name\";\"Verwendungszweck\";\"Konto\";\"Bank\";\"Betrag\";\"Währung\"\n\"01.02.2025\";\"01.02.2025\";\"\";\"Shop\";\"Kauf\";\"DE89370400440532013000\";\"\";\"-5,00\";\"EUR\"\n";
        let (f, rows) = parse_statement("auszug.csv", csv.as_bytes()).unwrap();
        assert!(f == "CSV" || f == "Erkannt");
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn amazon_capture() {
        assert!(amazon_check("kein json").is_err());
        assert!(amazon_check("{\"orders\":[]}").is_err());
        let json = r#"{"orders":[{"code":"028-1234567-1234567","date":"12. März 2025",
            "items":[{"title":"A","qty":1,"asin":"B012345678"},{"title":"B","asin":""}],
            "rows":[{"label":"Gesamtsumme","value":"45,98 €"}]}]}"#;
        let c = amazon_check(json).unwrap();
        assert_eq!((c.orders, c.items, c.items_without_asin), (1, 2, 1));
        assert_eq!(c.bad_dates, 0);
        assert_eq!(c.priced_orders, 1);
        assert_eq!(c.total_cents, -4598);
        assert!(amazon_text(&c, &|v| v.to_string()).contains("1 Bestellungen"));
    }

    #[test]
    fn backup_rules() {
        assert!(validate_backup_password("", "", true).is_err());
        assert!(validate_backup_password("geheim123", "geheim124", true).is_err());
        assert!(validate_backup_password("a", "", false).is_ok());
        // writing needs 8 characters, like the core
        assert!(validate_backup_password("a", "a", true).is_err());
        assert!(validate_backup_password("1234567", "1234567", true).is_err());
        assert!(validate_backup_password("geheim123", "geheim123", true).is_ok());
    }

    #[test]
    fn product_modes() {
        assert_eq!(product_mode_index("own"), 2);
        assert_eq!(product_mode_index("?"), 0);
        assert_eq!(product_mode_value(1), "builtin");
        assert_eq!(product_mode_value(-1), "");
        assert!(product_source_text("").contains("keine"));
    }

    #[test]
    fn settings_hide_secrets() {
        let e = |secret, has, value: &str| SettingEntry {
            key: "k".into(),
            label: "Label".into(),
            help: String::new(),
            group: "g".into(),
            group_title: "Gruppe".into(),
            value: value.into(),
            is_secret: secret,
            has_value: has,
        };
        let l = setting_lines(&[e(true, true, ""), e(false, true, "x"), e(false, false, "")]);
        assert!(l[0].detail.contains("verborgen") && !l[0].detail.contains(" x"));
        assert!(l[1].detail.ends_with("x"));
        assert!(l[2].detail.ends_with("leer"));
    }

    #[test]
    fn log_text_and_xml_escape() {
        let e = SyncLogEntry {
            id: 1,
            timestamp_ms: 1_743_422_400_000,
            connection_id: None,
            connection_name: "A & B".into(),
            source: SyncLogSource::Automatic,
            outcome: SyncLogOutcome::Error,
            message: "<boom>".into(),
            imported: 0,
        };
        let l = log_lines(std::slice::from_ref(&e));
        assert!(l[0].detail.contains("Fehler") && l[0].detail.contains("Automatisch"));
        let x = log_xml(&[e]);
        assert!(x.contains("A &amp; B") && x.contains("&lt;boom&gt;"));
    }

    #[test]
    fn progress_phases() {
        let mut p = MoneyMoneyDbImportProgress::default();
        assert!(progress_text(&p).contains("gelesen"));
        p.account_index = 2;
        p.account_count = 3;
        p.account_name = "Giro".into();
        assert!(progress_text(&p).contains("Konto 2 von 3"));
    }
}
