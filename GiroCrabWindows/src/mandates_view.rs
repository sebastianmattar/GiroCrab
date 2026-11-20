//! Pure logic and texts of the "Lastschriften & Mandate" dialog (`ui/mandates_dialog.slint`):
//! form parsing, sequence texts, list lines and summaries. No window, no vault, unit-tested.

use girocrab_core::batch_csv::{BatchCsvImport, BatchCsvRow};
use girocrab_core::connectors::fints::pain008::SequenceType;
use girocrab_core::mandates::{
    CollectionItem, DebitFile, Mandate, MandateBooking, MandateCsvRow, MandateImportResult,
    MandateInput, MandateScheme, next_sequence, return_reason,
};

pub const SCHEME_NAMES: [&str; 3] = [
    "Basislastschrift (CORE)",
    "Basislastschrift (COR1)",
    "Firmenlastschrift (B2B)",
];

/// One list row of the dialog.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Line {
    pub title: String,
    pub detail: String,
    pub amount: String,
    pub checked: bool,
    pub negative: bool,
}

/// The mandate form as typed.
#[derive(Debug, Clone, Default)]
pub struct Form {
    pub name: String,
    pub iban: String,
    pub bic: String,
    pub reference: String,
    pub date: String,
    pub scheme: i32,
    pub one_off: bool,
    pub amount: String,
    pub purpose: String,
    pub note: String,
    pub active: bool,
}

/// One debtor of the collection run as edited in the dialog.
#[derive(Debug, Clone, PartialEq)]
pub struct Pick {
    pub mandate_id: i64,
    pub checked: bool,
    pub amount_cents: i64,
    pub purpose: String,
    pub last: bool,
}

pub fn scheme_index(s: MandateScheme) -> i32 {
    match s {
        MandateScheme::Core => 0,
        MandateScheme::Cor1 => 1,
        MandateScheme::B2b => 2,
    }
}

pub fn scheme_from_index(i: i32) -> MandateScheme {
    match i {
        1 => MandateScheme::Cor1,
        2 => MandateScheme::B2b,
        _ => MandateScheme::Core,
    }
}

fn scheme_name(s: MandateScheme) -> &'static str {
    SCHEME_NAMES[scheme_index(s) as usize]
}

/// `15.03.2026` or `2026-03-15` -> `2026-03-15`; a real calendar date or an error.
pub fn parse_date(text: &str) -> Result<String, String> {
    let t = text.trim();
    let bad = || format!("Das Datum „{t}“ ist ungültig (TT.MM.JJJJ).");
    let (y, m, d) = if let Some((d, rest)) = t.split_once('.') {
        let (m, y) = rest.split_once('.').ok_or_else(bad)?;
        (y, m, d)
    } else {
        let mut it = t.split('-');
        match (it.next(), it.next(), it.next(), it.next()) {
            (Some(y), Some(m), Some(d), None) => (y, m, d),
            _ => return Err(bad()),
        }
    };
    let num = |s: &str| s.trim().parse::<u32>().map_err(|_| bad());
    let (y, m, d) = (num(y)?, num(m)?, num(d)?);
    let leap = (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400);
    let days = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return Err(bad()),
    };
    if !(1000..=9999).contains(&y) || d == 0 || d > days {
        return Err(bad());
    }
    Ok(format!("{y:04}-{m:02}-{d:02}"))
}

/// Empty = 0 (no default amount); otherwise the amount in cents, never negative.
pub fn parse_amount(text: &str) -> Result<i64, String> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(0);
    }
    let cents = girocrab_core::money::parse_cents(t).map_err(|e| e.to_string())?;
    if cents < 0 {
        return Err("Der Betrag darf nicht negativ sein.".into());
    }
    Ok(cents)
}

pub fn form_input(account_id: i64, f: &Form) -> Result<MandateInput, String> {
    Ok(MandateInput {
        account_id,
        debtor_name: f.name.trim().to_string(),
        debtor_iban: f.iban.trim().to_string(),
        debtor_bic: f.bic.trim().to_string(),
        reference: f.reference.trim().to_string(),
        signed_date: parse_date(&f.date)?,
        scheme: scheme_from_index(f.scheme),
        one_off: f.one_off,
        default_amount_cents: parse_amount(&f.amount)?,
        default_purpose: f.purpose.trim().to_string(),
        note: f.note.trim().to_string(),
        active: f.active,
    })
}

/// The form of a stored mandate.
pub fn form_of(m: &Mandate, date: &dyn Fn(&str) -> String) -> Form {
    Form {
        name: m.debtor_name.clone(),
        iban: m.debtor_iban.clone(),
        bic: m.debtor_bic.clone(),
        reference: m.reference.clone(),
        date: date(&m.signed_date),
        scheme: scheme_index(m.scheme),
        one_off: m.one_off,
        amount: if m.default_amount_cents > 0 {
            format!("{:.2}", m.default_amount_cents as f64 / 100.0).replace('.', ",")
        } else {
            String::new()
        },
        purpose: m.default_purpose.clone(),
        note: m.note.clone(),
        active: m.active,
    }
}

/// The form of a CSV row (so a faulty row can be fixed by hand).
pub fn form_of_csv(r: &MandateCsvRow, date: &dyn Fn(&str) -> String) -> Form {
    let input = r.to_input(0);
    Form {
        name: input.debtor_name,
        iban: input.debtor_iban,
        bic: input.debtor_bic,
        reference: input.reference,
        date: if input.signed_date.is_empty() {
            String::new()
        } else {
            date(&input.signed_date)
        },
        scheme: scheme_index(input.scheme),
        one_off: input.one_off,
        amount: if input.default_amount_cents > 0 {
            format!("{:.2}", input.default_amount_cents as f64 / 100.0).replace('.', ",")
        } else {
            String::new()
        },
        purpose: input.default_purpose,
        note: input.note,
        active: input.active,
    }
}

/// Plain-language sequence type of the next collection.
pub fn sequence_text(one_off: bool, last_collected: Option<&str>) -> String {
    let seq = next_sequence(one_off, last_collected);
    let name = match seq {
        SequenceType::First => "Erstlastschrift",
        SequenceType::Recurring => "Folgelastschrift",
        SequenceType::OneOff => "Einmallastschrift",
        SequenceType::Final => "Letzte Lastschrift",
    };
    format!("Nächster Einzug: {name} ({})", seq.code())
}

pub fn mandate_line(
    m: &Mandate,
    eur: &dyn Fn(i64) -> String,
    date: &dyn Fn(&str) -> String,
) -> Line {
    let mut detail = format!(
        "{} · {} · {}",
        m.reference,
        scheme_name(m.scheme),
        m.next_sequence
    );
    if let Some(last) = &m.last_collected {
        detail.push_str(&format!(" · zuletzt {}", date(last)));
    }
    if !m.active {
        detail.push_str(" · inaktiv");
    }
    Line {
        title: m.debtor_name.clone(),
        detail,
        amount: if m.default_amount_cents > 0 {
            eur(m.default_amount_cents)
        } else {
            String::new()
        },
        ..Line::default()
    }
}

pub fn mandates_summary(all: &[Mandate]) -> String {
    let active = all.iter().filter(|m| m.active).count();
    match all.len() {
        0 => "Keine Mandate für dieses Konto.".into(),
        n => format!("{n} Mandate, davon {active} aktiv."),
    }
}

pub fn csv_line(r: &MandateCsvRow, eur: &dyn Fn(i64) -> String) -> Line {
    let ok = r.errors.is_empty();
    Line {
        title: format!("Zeile {}: {}", r.line, r.debtor_name),
        detail: if ok {
            format!("{} · {}", r.reference, r.debtor_iban)
        } else {
            r.errors.join("; ")
        },
        amount: if r.default_amount_cents > 0 {
            eur(r.default_amount_cents)
        } else {
            String::new()
        },
        negative: !ok,
        ..Line::default()
    }
}

pub fn csv_summary(rows: &[MandateCsvRow], problems: &[String]) -> String {
    let bad = rows.iter().filter(|r| !r.errors.is_empty()).count();
    let mut s = format!(
        "{} Zeilen, {} fehlerfrei, {} fehlerhaft.",
        rows.len(),
        rows.len() - bad,
        bad
    );
    for p in problems {
        s.push(' ');
        s.push_str(p);
    }
    s
}

pub fn import_summary(r: &MandateImportResult) -> String {
    let mut s = format!("{} angelegt, {} aktualisiert.", r.created, r.updated);
    if !r.failed.is_empty() {
        s.push_str(&format!(
            " {} fehlgeschlagen: {}",
            r.failed.len(),
            r.failed.join("; ")
        ));
    }
    s
}

/// Starting picks of a run: every active mandate, its default amount, nothing checked yet
/// when it has no amount.
pub fn picks_of(mandates: &[Mandate]) -> Vec<Pick> {
    mandates
        .iter()
        .map(|m| Pick {
            mandate_id: m.id,
            checked: m.active && m.default_amount_cents > 0,
            amount_cents: m.default_amount_cents,
            purpose: String::new(),
            last: false,
        })
        .collect()
}

pub fn pick_line(m: &Mandate, p: &Pick, eur: &dyn Fn(i64) -> String) -> Line {
    let purpose = if p.purpose.is_empty() {
        m.default_purpose.as_str()
    } else {
        p.purpose.as_str()
    };
    let seq = if p.last && !m.one_off {
        "FNAL".to_string()
    } else {
        m.next_sequence.clone()
    };
    Line {
        title: m.debtor_name.clone(),
        detail: format!("{} · {seq} · {purpose}", scheme_name(m.scheme)),
        amount: if p.amount_cents > 0 {
            eur(p.amount_cents)
        } else {
            "kein Betrag".into()
        },
        checked: p.checked,
        negative: p.checked && p.amount_cents <= 0,
    }
}

pub fn picks_summary(picks: &[Pick], eur: &dyn Fn(i64) -> String) -> String {
    let on: Vec<&Pick> = picks.iter().filter(|p| p.checked).collect();
    let total: i64 = on.iter().map(|p| p.amount_cents).sum();
    format!("{} Einzüge ausgewählt, Summe {}.", on.len(), eur(total))
}

/// The checked picks as collection items; a checked pick without an amount is an error.
pub fn collection_items(picks: &[Pick]) -> Result<Vec<CollectionItem>, String> {
    let items: Vec<CollectionItem> = picks
        .iter()
        .filter(|p| p.checked)
        .map(|p| CollectionItem {
            mandate_id: p.mandate_id,
            amount_cents: p.amount_cents,
            purpose: p.purpose.trim().to_string(),
            last_collection: p.last,
        })
        .collect();
    if items.is_empty() {
        return Err("Bitte mindestens ein Mandat auswählen.".into());
    }
    if items.iter().any(|i| i.amount_cents <= 0) {
        return Err("Für jeden gewählten Einzug ist ein Betrag nötig.".into());
    }
    Ok(items)
}

pub fn file_line(f: &DebitFile, eur: &dyn Fn(i64) -> String) -> Line {
    Line {
        title: f.file_name.clone(),
        detail: format!(
            "{} · {} · {} Einzüge",
            scheme_name(f.scheme),
            f.sequence,
            f.count
        ),
        amount: eur(f.total_cents),
        ..Line::default()
    }
}

/// File name safe for every file system.
pub fn safe_file_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() || s.starts_with('.') {
        format!("lastschrift{s}")
    } else {
        s
    }
}

/// Reason text of a return: the core's text, else the plain-language text of the code.
pub fn reason_text(b: &MandateBooking) -> String {
    if !b.reason.is_empty() {
        return b.reason.clone();
    }
    if b.reason_code.is_empty() {
        return "kein Grund angegeben".into();
    }
    return_reason(&b.reason_code).unwrap_or_else(|| b.reason_code.clone())
}

pub fn booking_line(
    b: &MandateBooking,
    name: &str,
    eur: &dyn Fn(i64) -> String,
    date: &dyn Fn(&str) -> String,
) -> Line {
    let detail = if b.is_return {
        let code = if b.reason_code.is_empty() {
            String::new()
        } else {
            format!("{}: ", b.reason_code)
        };
        format!(
            "{} · Rücklastschrift – {code}{}",
            date(&b.date),
            reason_text(b)
        )
    } else {
        format!("{} · {}", date(&b.date), b.purpose)
    };
    Line {
        title: name.to_string(),
        detail,
        amount: eur(b.amount_cents),
        negative: b.is_return,
        ..Line::default()
    }
}

pub fn returns_summary(n: usize) -> String {
    match n {
        0 => "Keine offenen Rücklastschriften.".into(),
        1 => "1 offene Rücklastschrift.".into(),
        n => format!("{n} offene Rücklastschriften."),
    }
}

pub fn batch_line(r: &BatchCsvRow, eur: &dyn Fn(i64) -> String) -> Line {
    let ok = r.errors.is_empty();
    Line {
        title: format!("Zeile {}: {}", r.line, r.name),
        detail: if ok {
            format!("{} · {}", r.iban, r.purpose)
        } else {
            r.errors.join("; ")
        },
        amount: if r.amount_cents > 0 {
            eur(r.amount_cents)
        } else {
            r.amount_text.clone()
        },
        negative: !ok,
        ..Line::default()
    }
}

pub fn batch_summary(b: &BatchCsvImport, eur: &dyn Fn(i64) -> String) -> String {
    let good: Vec<&BatchCsvRow> = b.rows.iter().filter(|r| r.errors.is_empty()).collect();
    let total: i64 = good.iter().map(|r| r.amount_cents).sum();
    let mut s = format!(
        "{} Zeilen, {} fehlerfrei (Summe {}), {} fehlerhaft.",
        b.rows.len(),
        good.len(),
        eur(total),
        b.rows.len() - good.len()
    );
    for p in &b.problems {
        s.push(' ');
        s.push_str(p);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eur(c: i64) -> String {
        format!("{},{:02} €", c / 100, c % 100)
    }
    fn date(iso: &str) -> String {
        iso.to_string()
    }

    fn mandate(id: i64, active: bool, cents: i64) -> Mandate {
        Mandate {
            id,
            account_id: 1,
            debtor_name: format!("Zahler {id}"),
            debtor_iban: "DE02120300000000202051".into(),
            debtor_bic: String::new(),
            reference: format!("M-{id}"),
            signed_date: "2026-01-02".into(),
            scheme: MandateScheme::Core,
            one_off: false,
            default_amount_cents: cents,
            default_purpose: "Beitrag".into(),
            note: String::new(),
            active,
            last_collected: None,
            next_sequence: "FRST".into(),
        }
    }

    #[test]
    fn dates_accept_german_and_iso() {
        assert_eq!(parse_date("15.03.2026").unwrap(), "2026-03-15");
        assert_eq!(parse_date("2026-03-15").unwrap(), "2026-03-15");
        assert_eq!(parse_date("29.02.2028").unwrap(), "2028-02-29");
        assert!(parse_date("29.02.2027").is_err());
        assert!(parse_date("31.04.2026").is_err());
        assert!(parse_date("").is_err());
        assert!(parse_date("morgen").is_err());
    }

    #[test]
    fn amount_is_empty_zero_or_positive() {
        assert_eq!(parse_amount("").unwrap(), 0);
        assert_eq!(parse_amount("12,50").unwrap(), 1250);
        assert!(parse_amount("-1").is_err());
        assert!(parse_amount("abc").is_err());
    }

    #[test]
    fn form_becomes_input_and_back() {
        let f = Form {
            name: " Erika ".into(),
            iban: "DE02120300000000202051".into(),
            reference: "R1".into(),
            date: "01.02.2026".into(),
            scheme: 2,
            one_off: true,
            amount: "9,90".into(),
            active: true,
            ..Form::default()
        };
        let i = form_input(7, &f).unwrap();
        assert_eq!(i.account_id, 7);
        assert_eq!(i.debtor_name, "Erika");
        assert_eq!(i.signed_date, "2026-02-01");
        assert_eq!(i.scheme, MandateScheme::B2b);
        assert_eq!(i.default_amount_cents, 990);
        let mut bad = f.clone();
        bad.date = "x".into();
        assert!(form_input(7, &bad).is_err());
        let m = mandate(1, true, 990);
        assert_eq!(form_of(&m, &date).amount, "9,90");
    }

    #[test]
    fn sequence_text_names_the_type() {
        assert!(sequence_text(false, None).contains("FRST"));
        assert!(sequence_text(false, Some("2026-01-01")).contains("RCUR"));
        assert!(sequence_text(true, None).contains("OOFF"));
    }

    #[test]
    fn mandate_lines_and_summary() {
        let all = vec![mandate(1, true, 500), mandate(2, false, 0)];
        assert_eq!(mandates_summary(&all), "2 Mandate, davon 1 aktiv.");
        assert_eq!(mandates_summary(&[]), "Keine Mandate für dieses Konto.");
        let l = mandate_line(&all[1], &eur, &date);
        assert!(l.detail.ends_with("inaktiv"));
        assert_eq!(l.amount, "");
        assert_eq!(mandate_line(&all[0], &eur, &date).amount, "5,00 €");
    }

    #[test]
    fn picks_start_with_active_mandates_that_have_an_amount() {
        let all = vec![
            mandate(1, true, 500),
            mandate(2, true, 0),
            mandate(3, false, 100),
        ];
        let p = picks_of(&all);
        assert_eq!(
            p.iter().map(|p| p.checked).collect::<Vec<_>>(),
            [true, false, false]
        );
        assert_eq!(
            picks_summary(&p, &eur),
            "1 Einzüge ausgewählt, Summe 5,00 €."
        );
    }

    #[test]
    fn collection_items_need_a_selection_and_amounts() {
        let all = vec![mandate(1, true, 500), mandate(2, true, 0)];
        let mut p = picks_of(&all);
        let items = collection_items(&p).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].mandate_id, 1);
        p[1].checked = true;
        assert!(collection_items(&p).is_err());
        p[0].checked = false;
        p[1].checked = false;
        assert!(collection_items(&p).is_err());
    }

    #[test]
    fn last_collection_shows_fnal_except_one_off() {
        let mut m = mandate(1, true, 500);
        let mut p = picks_of(std::slice::from_ref(&m))[0].clone();
        p.last = true;
        assert!(pick_line(&m, &p, &eur).detail.contains("FNAL"));
        m.one_off = true;
        assert!(!pick_line(&m, &p, &eur).detail.contains("FNAL"));
    }

    #[test]
    fn file_names_are_safe() {
        assert_eq!(
            safe_file_name("lastschrift-CORE-FRST-2026-11-30.xml"),
            "lastschrift-CORE-FRST-2026-11-30.xml"
        );
        assert_eq!(safe_file_name("../a b:c"), "lastschrift.._a_b_c");
        assert!(!safe_file_name("a/b\\c").contains(['/', '\\']));
        assert_eq!(safe_file_name(""), "lastschrift");
    }

    fn booking(reason_code: &str, reason: &str, is_return: bool) -> MandateBooking {
        MandateBooking {
            booking_id: 1,
            mandate_id: 1,
            date: "2026-03-01".into(),
            amount_cents: if is_return { -500 } else { 500 },
            purpose: "Beitrag".into(),
            is_return,
            reason_code: reason_code.into(),
            reason: reason.into(),
            acknowledged: false,
        }
    }

    #[test]
    fn return_reason_falls_back_to_the_code_text() {
        assert_eq!(
            reason_text(&booking("AC04", "Konto aufgelöst", true)),
            "Konto aufgelöst"
        );
        assert_eq!(reason_text(&booking("", "", true)), "kein Grund angegeben");
        assert_eq!(reason_text(&booking("ZZ99", "", true)), "ZZ99");
        assert!(!reason_text(&booking("AC04", "", true)).is_empty());
        let l = booking_line(
            &booking("AC04", "Konto aufgelöst", true),
            "Erika",
            &eur,
            &date,
        );
        assert!(l.negative && l.detail.contains("AC04"));
        assert!(!booking_line(&booking("", "", false), "Erika", &eur, &date).negative);
    }

    #[test]
    fn returns_summary_pluralises() {
        assert_eq!(returns_summary(0), "Keine offenen Rücklastschriften.");
        assert_eq!(returns_summary(1), "1 offene Rücklastschrift.");
        assert_eq!(returns_summary(3), "3 offene Rücklastschriften.");
    }

    #[test]
    fn csv_and_batch_previews_mark_faulty_rows() {
        let imp = girocrab_core::mandates::parse_mandate_csv(
            "Name;IBAN;Referenz\nErika;DE02120300000000202051;R1\n;;\n".into(),
        );
        let s = csv_summary(&imp.rows, &imp.problems);
        assert!(s.starts_with(&format!("{} Zeilen", imp.rows.len())));
        for r in &imp.rows {
            assert_eq!(csv_line(r, &eur).negative, !r.errors.is_empty());
        }
        let b = girocrab_core::batch_csv::parse_batch_csv(
            "Name;IBAN;Betrag;Zweck\nErika;DE02120300000000202051;12,50;Miete\nBob;XX;abc;x\n"
                .into(),
        );
        assert_eq!(b.rows.len(), 2);
        assert!(batch_summary(&b, &eur).contains("1 fehlerfrei (Summe 12,50 €)"));
        assert!(!batch_line(&b.rows[0], &eur).negative);
        assert!(batch_line(&b.rows[1], &eur).negative);
    }

    #[test]
    fn import_summary_lists_failures() {
        let r = MandateImportResult {
            created: 2,
            updated: 1,
            failed: vec!["3: IBAN".into()],
        };
        assert_eq!(
            import_summary(&r),
            "2 angelegt, 1 aktualisiert. 1 fehlgeschlagen: 3: IBAN"
        );
    }
}
