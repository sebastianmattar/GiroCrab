//! View-models of the list views without Slint: Geplant (recurring bookings), Lastschriften
//! (mandates), Belege & PostBox (documents) and the rules dialog. Pure functions over core
//! records, unit-tested on every platform.

use girocrab_core::documents::DocumentInfo;
use girocrab_core::mandates::{Mandate, MandateScheme};
use girocrab_core::recurring::{RecurrenceInterval, RecurringSeries};
use girocrab_core::{Category, MatchKind, Rule, RuleField};

use crate::portfolio::Cells;

pub const PLANNED_COLUMNS: [&str; 5] = [
    "Name",
    "Intervall",
    "Betrag",
    "Nächste Fälligkeit",
    "Zuletzt",
];
pub const MANDATE_COLUMNS: [&str; 5] = [
    "Zahlungspflichtiger",
    "Referenz",
    "Betrag",
    "Nächster Einzug",
    "Zuletzt eingezogen",
];
pub const DOCUMENT_COLUMNS: [&str; 5] = ["Titel", "Art", "Datum", "Umsatz", "Größe"];

/// Entries of the interval selector of "Geplant"; index 0 = no filter.
pub const INTERVAL_LABELS: [&str; 6] = [
    "Alle Intervalle",
    "Monatlich",
    "Vierteljährlich",
    "Halbjährlich",
    "Jährlich",
    "Feste Tage",
];

pub fn interval_label(i: RecurrenceInterval) -> &'static str {
    match i {
        RecurrenceInterval::Monthly => INTERVAL_LABELS[1],
        RecurrenceInterval::Quarterly => INTERVAL_LABELS[2],
        RecurrenceInterval::HalfYearly => INTERVAL_LABELS[3],
        RecurrenceInterval::Yearly => INTERVAL_LABELS[4],
        RecurrenceInterval::Custom => INTERVAL_LABELS[5],
    }
}

fn interval_filter(index: i32) -> Option<RecurrenceInterval> {
    match index {
        1 => Some(RecurrenceInterval::Monthly),
        2 => Some(RecurrenceInterval::Quarterly),
        3 => Some(RecurrenceInterval::HalfYearly),
        4 => Some(RecurrenceInterval::Yearly),
        5 => Some(RecurrenceInterval::Custom),
        _ => None,
    }
}

/// Recurring bookings of the chosen interval, next due first; rows without a date go last.
pub fn planned_rows(
    series: &[RecurringSeries],
    interval_index: i32,
    money: &dyn Fn(i64) -> String,
    date: &dyn Fn(&str) -> String,
) -> Vec<Cells> {
    let want = interval_filter(interval_index);
    let mut v: Vec<&RecurringSeries> = series
        .iter()
        .filter(|s| want.is_none_or(|w| s.interval == w))
        .collect();
    v.sort_by(|a, b| {
        let (x, y) = (a.next_due.is_empty(), b.next_due.is_empty());
        x.cmp(&y)
            .then_with(|| a.next_due.cmp(&b.next_due))
            .then_with(|| a.name.cmp(&b.name))
    });
    v.iter()
        .enumerate()
        .map(|(i, s)| Cells {
            id: i as i64,
            cells: [
                s.name.clone(),
                interval_label(s.interval).into(),
                money(s.amount_cents),
                date(&s.next_due),
                date(&s.last_date),
                String::new(),
            ],
            negative: s.amount_cents < 0,
        })
        .collect()
}

pub fn scheme_label(s: MandateScheme) -> &'static str {
    match s {
        MandateScheme::Core => "Basis",
        MandateScheme::Cor1 => "COR1",
        MandateScheme::B2b => "Firmen",
    }
}

fn sequence_label(seq: &str) -> &str {
    match seq {
        "FRST" => "Erstlastschrift",
        "RCUR" => "Folgelastschrift",
        "OOFF" => "Einmalig",
        other => other,
    }
}

/// Mandates, active first. A mandate has no due date of its own: the next collection is
/// described by its sequence type, the last one by its due date (empty = never collected).
pub fn mandate_rows(
    mandates: &[Mandate],
    money: &dyn Fn(i64) -> String,
    date: &dyn Fn(&str) -> String,
) -> Vec<Cells> {
    let mut v: Vec<&Mandate> = mandates.iter().collect();
    v.sort_by(|a, b| {
        b.active.cmp(&a.active).then_with(|| {
            a.debtor_name
                .to_lowercase()
                .cmp(&b.debtor_name.to_lowercase())
        })
    });
    v.iter()
        .map(|m| Cells {
            id: m.id,
            cells: [
                if m.active {
                    m.debtor_name.clone()
                } else {
                    format!("{} (inaktiv)", m.debtor_name)
                },
                format!("{} · {}", m.reference, scheme_label(m.scheme)),
                money(m.default_amount_cents),
                if m.active {
                    sequence_label(&m.next_sequence).to_string()
                } else {
                    String::new()
                },
                m.last_collected.as_deref().map(date).unwrap_or_default(),
                String::new(),
            ],
            negative: false,
        })
        .collect()
}

pub fn size_text(bytes: u64) -> String {
    match bytes {
        0 => String::new(),
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{} kB", (b + 512) / 1024),
        b => format!("{},{} MB", b / 1_048_576, (b % 1_048_576) * 10 / 1_048_576),
    }
}

/// Documents newest first (by document date, then id).
pub fn document_rows(docs: &[DocumentInfo], date: &dyn Fn(&str) -> String) -> Vec<Cells> {
    let mut v: Vec<&DocumentInfo> = docs.iter().collect();
    v.sort_by(|a, b| b.doc_date.cmp(&a.doc_date).then(b.id.cmp(&a.id)));
    v.iter()
        .map(|d| Cells {
            id: d.id,
            cells: [
                if d.title.trim().is_empty() {
                    d.file_name.clone()
                } else {
                    d.title.clone()
                },
                if d.doc_type.is_empty() {
                    d.mime_type.clone()
                } else {
                    d.doc_type.clone()
                },
                date(&d.doc_date),
                if d.booking_label.is_empty() {
                    "nicht verknüpft".into()
                } else {
                    d.booking_label.clone()
                },
                size_text(d.size_bytes),
                String::new(),
            ],
            negative: false,
        })
        .collect()
}

/// File name safe to write into a temp folder: no path parts, no characters Windows rejects.
pub fn safe_file_name(name: &str, fallback: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').to_string();
    if cleaned.is_empty() {
        fallback.to_string()
    } else {
        cleaned.chars().take(120).collect()
    }
}

/// Title of a document stored from a file path.
pub fn document_title(path: &std::path::Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Beleg")
        .to_string()
}

// ------------------------------------------------------------------------------- rules

pub const RULE_FIELD_LABELS: [&str; 5] = [
    "Alle Felder",
    "Gegenpartei",
    "Verwendungszweck",
    "Buchungstext",
    "IBAN der Gegenpartei",
];
pub const RULE_KIND_LABELS: [&str; 3] = ["Stichwörter (a|b)", "Regulärer Ausdruck", "Suchausdruck"];

pub fn rule_field(i: i32) -> RuleField {
    match i {
        1 => RuleField::Counterparty,
        2 => RuleField::Purpose,
        3 => RuleField::BookingText,
        4 => RuleField::CounterpartyIban,
        _ => RuleField::Any,
    }
}

pub fn rule_field_index(f: RuleField) -> i32 {
    match f {
        RuleField::Any => 0,
        RuleField::Counterparty => 1,
        RuleField::Purpose => 2,
        RuleField::BookingText => 3,
        RuleField::CounterpartyIban => 4,
    }
}

pub fn rule_kind(i: i32) -> MatchKind {
    match i {
        1 => MatchKind::Regex,
        2 => MatchKind::Expression,
        _ => MatchKind::Keyword,
    }
}

pub fn rule_kind_index(k: MatchKind) -> i32 {
    match k {
        MatchKind::Keyword => 0,
        MatchKind::Regex => 1,
        MatchKind::Expression => 2,
    }
}

/// The rules dialog's form, as the Slint controls hold it.
#[derive(Debug, Clone, PartialEq)]
pub struct RuleForm {
    pub name: String,
    pub pattern: String,
    /// Index into the category list of the dialog (0-based; no "ohne" entry).
    pub category_index: i32,
    pub field_index: i32,
    pub kind_index: i32,
    pub enabled: bool,
    pub case_sensitive: bool,
    /// Amount range in euros as typed ("-50", "12,5"); empty = no bound. Compared with the signed booking amount.
    pub min_amount: String,
    pub max_amount: String,
}

/// Turns the form into a rule for `save_rule`. `existing` is the rule being edited (its id,
/// priority and account restriction stay); a new rule gets id 0 and the next priority.
pub fn validate_rule(
    form: &RuleForm,
    existing: Option<&Rule>,
    categories: &[Category],
    next_priority: i32,
) -> Result<Rule, String> {
    let pattern = form.pattern.trim();
    if pattern.is_empty() {
        return Err("Bitte ein Suchmuster eingeben.".into());
    }
    let category = usize::try_from(form.category_index)
        .ok()
        .and_then(|i| categories.get(i))
        .ok_or("Bitte eine Kategorie auswählen.")?;
    let name = form.name.trim();
    let name = if name.is_empty() { pattern } else { name };
    if name.chars().count() > 80 || name.contains(['\n', '\r']) {
        return Err("Der Name der Regel ist zu lang (höchstens 80 Zeichen).".into());
    }
    let mut rule = existing.cloned().unwrap_or(Rule {
        id: 0,
        name: String::new(),
        category_id: 0,
        field: RuleField::Any,
        match_kind: MatchKind::Keyword,
        pattern: String::new(),
        case_sensitive: false,
        min_amount_cents: None,
        max_amount_cents: None,
        account_ids: Vec::new(),
        priority: next_priority,
        enabled: true,
        match_count: 0,
        tag_ids: Vec::new(),
    });
    rule.name = name.to_string();
    rule.pattern = pattern.to_string();
    rule.category_id = category.id;
    rule.field = rule_field(form.field_index);
    rule.match_kind = rule_kind(form.kind_index);
    rule.enabled = form.enabled;
    rule.case_sensitive = form.case_sensitive;
    rule.min_amount_cents = parse_bound(&form.min_amount, "Mindestbetrag")?;
    rule.max_amount_cents = parse_bound(&form.max_amount, "Höchstbetrag")?;
    if let (Some(lo), Some(hi)) = (rule.min_amount_cents, rule.max_amount_cents)
        && lo > hi
    {
        return Err("Der Mindestbetrag darf nicht über dem Höchstbetrag liegen.".into());
    }
    Ok(rule)
}

fn parse_bound(text: &str, what: &str) -> Result<Option<i64>, String> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    crate::planning::parse_cents(text)
        .map(Some)
        .ok_or_else(|| format!("{what}: ungültiger Betrag."))
}

/// Amount bound back into the form's text field.
pub fn bound_text(cents: Option<i64>) -> String {
    cents.map_or_else(String::new, crate::planning::format_plain)
}

/// One list line of the rules dialog.
pub fn rule_line(rule: &Rule, categories: &[Category]) -> (String, String) {
    let cat = categories
        .iter()
        .find(|c| c.id == rule.category_id)
        .map_or("(Kategorie gelöscht)", |c| c.path.as_str());
    let title = if rule.enabled {
        rule.name.clone()
    } else {
        format!("{} (aus)", rule.name)
    };
    (
        title,
        format!(
            "{} → {}{} · {} Treffer",
            rule.pattern,
            cat,
            match (rule.min_amount_cents, rule.max_amount_cents) {
                (None, None) => String::new(),
                (lo, hi) => format!(" · Betrag {} … {}", bound_text(lo), bound_text(hi)),
            },
            rule.match_count
        ),
    )
}

/// Next free priority: after the last existing rule.
pub fn next_priority(rules: &[Rule]) -> i32 {
    rules.iter().map(|r| r.priority).max().map_or(0, |p| p + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use girocrab_core::documents::DocumentInfo;

    fn eur(c: i64) -> String {
        format!("{c}€")
    }
    fn plain(d: &str) -> String {
        d.to_string()
    }

    fn series(name: &str, interval: RecurrenceInterval, next: &str, cents: i64) -> RecurringSeries {
        RecurringSeries {
            series_key: name.into(),
            base_key: name.into(),
            amount_specific: false,
            name: name.into(),
            category_name: String::new(),
            category_symbol: String::new(),
            merchant_domain: None,
            account_id: 1,
            interval,
            custom_dates: vec![],
            amount_cents: cents,
            last_date: "2026-10-01".into(),
            last_booking_id: 1,
            next_due: next.into(),
            occurrences: 3,
            source: girocrab_core::recurring::RecurringSource::Detected,
        }
    }

    #[test]
    fn planned_sorts_by_due_date_and_filters_interval() {
        let all = [
            series("Zeitung", RecurrenceInterval::Yearly, "2027-01-05", -9000),
            series("Strom", RecurrenceInterval::Monthly, "2026-12-01", -8000),
            series("Miete", RecurrenceInterval::Monthly, "2026-11-30", -90000),
            series("Alt", RecurrenceInterval::Monthly, "", -100),
        ];
        let rows = planned_rows(&all, 0, &eur, &plain);
        let names: Vec<&str> = rows.iter().map(|r| r.cells[0].as_str()).collect();
        assert_eq!(names, ["Miete", "Strom", "Zeitung", "Alt"]);
        assert_eq!(rows[0].cells[1], "Monatlich");
        assert_eq!(rows[0].cells[3], "2026-11-30");
        assert!(rows[0].negative);
        let yearly = planned_rows(&all, 4, &eur, &plain);
        assert_eq!(yearly.len(), 1);
        assert_eq!(yearly[0].cells[0], "Zeitung");
        assert!(planned_rows(&all, 3, &eur, &plain).is_empty());
    }

    fn mandate(id: i64, name: &str, active: bool, last: Option<&str>) -> Mandate {
        Mandate {
            id,
            account_id: 1,
            debtor_name: name.into(),
            debtor_iban: "DE00".into(),
            debtor_bic: String::new(),
            reference: format!("REF{id}"),
            signed_date: "2025-01-01".into(),
            scheme: MandateScheme::Core,
            one_off: false,
            default_amount_cents: 1500,
            default_purpose: String::new(),
            note: String::new(),
            active,
            last_collected: last.map(String::from),
            next_sequence: if last.is_some() { "RCUR" } else { "FRST" }.into(),
        }
    }

    #[test]
    fn mandates_list_active_first_without_inventing_due_dates() {
        let rows = mandate_rows(
            &[
                mandate(1, "Zora", false, None),
                mandate(2, "Anton", true, Some("2026-10-15")),
                mandate(3, "Berta", true, None),
            ],
            &eur,
            &plain,
        );
        let names: Vec<&str> = rows.iter().map(|r| r.cells[0].as_str()).collect();
        assert_eq!(names, ["Anton", "Berta", "Zora (inaktiv)"]);
        assert_eq!(rows[0].cells[3], "Folgelastschrift");
        assert_eq!(rows[0].cells[4], "2026-10-15");
        assert_eq!(rows[1].cells[3], "Erstlastschrift");
        assert_eq!(rows[1].cells[4], "");
        assert_eq!(rows[2].cells[3], "");
        assert_eq!(rows[0].cells[1], "REF2 · Basis");
    }

    fn doc(id: i64, title: &str, date: &str, booking: &str, size: u64) -> DocumentInfo {
        DocumentInfo {
            id,
            booking_id: (!booking.is_empty()).then_some(1),
            account_id: Some(1),
            title: title.into(),
            doc_type: "Rechnung".into(),
            mime_type: "application/pdf".into(),
            file_name: format!("{id}.pdf"),
            size_bytes: size,
            source_url: String::new(),
            doc_date: date.into(),
            created_at: 0,
            seen: true,
            booking_label: booking.into(),
            account_name: "Giro".into(),
        }
    }

    #[test]
    fn documents_show_link_state_and_size() {
        let rows = document_rows(
            &[
                doc(1, "Alt", "2026-01-01", "", 500),
                doc(2, "", "2026-11-01", "METRO · -5€", 2_500_000),
            ],
            &plain,
        );
        assert_eq!(rows[0].id, 2);
        assert_eq!(rows[0].cells[0], "2.pdf");
        assert_eq!(rows[0].cells[3], "METRO · -5€");
        assert_eq!(rows[0].cells[4], "2,3 MB");
        assert_eq!(rows[1].cells[3], "nicht verknüpft");
        assert_eq!(rows[1].cells[4], "500 B");
        assert_eq!(size_text(0), "");
        assert_eq!(size_text(2048), "2 kB");
    }

    #[test]
    fn file_names_cannot_escape_the_temp_folder() {
        assert_eq!(safe_file_name("../../etc/passwd", "x"), "passwd");
        assert_eq!(safe_file_name("C:\\a\\b:c?.pdf", "x"), "b_c_.pdf");
        assert_eq!(safe_file_name("..", "beleg.bin"), "beleg.bin");
        assert_eq!(safe_file_name("", "beleg.bin"), "beleg.bin");
        assert_eq!(
            document_title(std::path::Path::new("/tmp/Rechnung 7.pdf")),
            "Rechnung 7"
        );
    }

    fn cats() -> Vec<Category> {
        ["Lebensmittel", "Miete"]
            .iter()
            .enumerate()
            .map(|(i, n)| Category {
                id: 10 + i as i64,
                parent_id: None,
                name: (*n).into(),
                sf_symbol: String::new(),
                color_hex: String::new(),
                sort_order: 0,
                depth: 0,
                path: (*n).into(),
                own_total_cents: 0,
                total_cents: 0,
                transaction_count: 0,
                budget_cents: None,
            })
            .collect()
    }

    fn form() -> RuleForm {
        RuleForm {
            name: " Supermarkt ".into(),
            pattern: " REWE|EDEKA ".into(),
            category_index: 1,
            field_index: 1,
            kind_index: 0,
            enabled: true,
            case_sensitive: false,
            min_amount: String::new(),
            max_amount: String::new(),
        }
    }

    #[test]
    fn rule_form_amount_range() {
        let mut f = form();
        f.min_amount = "-50,5".into();
        f.max_amount = "10".into();
        let r = validate_rule(&f, None, &cats(), 0).unwrap();
        assert_eq!(
            (r.min_amount_cents, r.max_amount_cents),
            (Some(-5050), Some(1000))
        );
        f.min_amount = "20".into();
        assert!(validate_rule(&f, None, &cats(), 0).is_err());
        f.min_amount = "abc".into();
        assert!(validate_rule(&f, None, &cats(), 0).is_err());
    }

    #[test]
    fn rule_form_builds_a_new_rule() {
        let r = validate_rule(&form(), None, &cats(), 3).unwrap();
        assert_eq!(r.id, 0);
        assert_eq!(r.name, "Supermarkt");
        assert_eq!(r.pattern, "REWE|EDEKA");
        assert_eq!(r.category_id, 11);
        assert_eq!(r.field, RuleField::Counterparty);
        assert_eq!(r.match_kind, MatchKind::Keyword);
        assert_eq!(r.priority, 3);
    }

    #[test]
    fn rule_edit_keeps_id_priority_and_accounts() {
        let mut old = validate_rule(&form(), None, &cats(), 3).unwrap();
        old.id = 42;
        old.account_ids = vec![5];
        old.match_count = 9;
        let mut f = form();
        f.name = String::new();
        f.kind_index = 1;
        f.enabled = false;
        let r = validate_rule(&f, Some(&old), &cats(), 99).unwrap();
        assert_eq!(r.id, 42);
        assert_eq!(r.priority, 3);
        assert_eq!(r.account_ids, vec![5]);
        // Empty name falls back to the pattern.
        assert_eq!(r.name, "REWE|EDEKA");
        assert_eq!(r.match_kind, MatchKind::Regex);
        assert!(!r.enabled);
    }

    #[test]
    fn rule_form_rejects_bad_input() {
        let mut f = form();
        f.pattern = "  ".into();
        assert!(validate_rule(&f, None, &cats(), 0).is_err());
        let mut f = form();
        f.category_index = -1;
        assert!(validate_rule(&f, None, &cats(), 0).is_err());
        f.category_index = 5;
        assert!(validate_rule(&f, None, &cats(), 0).is_err());
        let mut f = form();
        f.name = "x".repeat(81);
        assert!(validate_rule(&f, None, &cats(), 0).is_err());
        assert!(validate_rule(&form(), None, &[], 0).is_err());
    }

    #[test]
    fn rule_labels_round_trip_and_list_lines_read_well() {
        for i in 0..5 {
            assert_eq!(rule_field_index(rule_field(i)), i);
        }
        for i in 0..3 {
            assert_eq!(rule_kind_index(rule_kind(i)), i);
        }
        let mut r = validate_rule(&form(), None, &cats(), 0).unwrap();
        r.match_count = 4;
        let (t, d) = rule_line(&r, &cats());
        assert_eq!(t, "Supermarkt");
        assert_eq!(d, "REWE|EDEKA → Miete · 4 Treffer");
        r.enabled = false;
        r.category_id = 999;
        let (t, d) = rule_line(&r, &cats());
        assert_eq!(t, "Supermarkt (aus)");
        assert!(d.contains("Kategorie gelöscht"));
        assert_eq!(next_priority(&[]), 0);
        assert_eq!(next_priority(&[r]), 1);
    }
}
