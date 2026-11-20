//! Pure logic of the Windows front end for tags, split bookings, category budgets and transfer
//! templates: amount parsing, form validation and the tag arithmetic. No Slint and no vault access
//! here, so everything is unit-tested on every platform.

use girocrab_core::{Category, TransactionSplitInput, TransferTemplate};

/// Parses a German or plain amount ("1.234,56", "12,5", "-3.20", "7") into cents.
pub fn parse_cents(text: &str) -> Option<i64> {
    let t = text.trim().replace(['€', ' ', '\u{a0}'], "");
    let (neg, t) = match t.strip_prefix('-') {
        Some(rest) => (true, rest.to_string()),
        None => (false, t.trim_start_matches('+').to_string()),
    };
    if t.is_empty() {
        return None;
    }
    // The last separator is the decimal one when 1-2 digits follow it; the others group thousands.
    let dec_pos = t.rfind([',', '.']);
    let (int_part, frac) = match dec_pos {
        Some(p) if t.len() - p - 1 <= 2 && (t.len() - p - 1 > 0) => (&t[..p], &t[p + 1..]),
        Some(p) if t.len() - p - 1 == 3 && t[..p].chars().all(|c| c.is_ascii_digit()) => {
            (&t[..], "")
        }
        Some(_) => return None,
        None => (&t[..], ""),
    };
    let digits: String = int_part
        .chars()
        .filter(|c| *c != '.' && *c != ',')
        .collect();
    if digits.is_empty() && frac.is_empty() {
        return None;
    }
    if !digits.chars().all(|c| c.is_ascii_digit()) || !frac.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let whole: i64 = if digits.is_empty() {
        0
    } else {
        digits.parse().ok()?
    };
    let cents: i64 = match frac.len() {
        0 => 0,
        1 => frac.parse::<i64>().ok()? * 10,
        _ => frac.parse().ok()?,
    };
    let v = whole.checked_mul(100)?.checked_add(cents)?;
    Some(if neg { -v } else { v })
}

/// `-1234` -> `-12,34` (no currency; for edit fields).
pub fn format_plain(cents: i64) -> String {
    let abs = cents.unsigned_abs();
    format!(
        "{}{},{:02}",
        if cents < 0 { "-" } else { "" },
        abs / 100,
        abs % 100
    )
}

/// A tag name: trimmed, not empty, at most 60 characters.
pub fn validate_tag_name(name: &str) -> Result<String, String> {
    let n = name.trim();
    if n.is_empty() {
        return Err("Bitte einen Namen für den Tag eingeben.".into());
    }
    if n.chars().count() > 60 {
        return Err("Der Tag-Name ist zu lang (höchstens 60 Zeichen).".into());
    }
    Ok(n.to_string())
}

/// The palette entry `index` (negative = no colour chosen -> "" = default colour).
pub fn tag_color(palette: &[String], index: i32) -> String {
    usize::try_from(index)
        .ok()
        .and_then(|i| palette.get(i))
        .cloned()
        .unwrap_or_default()
}

/// Index of `hex` in the palette (case-insensitive), -1 when absent.
pub fn color_index(palette: &[String], hex: &str) -> i32 {
    palette
        .iter()
        .position(|c| c.eq_ignore_ascii_case(hex))
        .map_or(-1, |i| i as i32)
}

/// The id order after moving the element at `index` one step up (`-1`) or down (`+1`);
/// `None` when it is already at the edge.
pub fn moved_order(ids: &[i64], index: usize, step: i32) -> Option<Vec<i64>> {
    let to = index as i64 + i64::from(step);
    if index >= ids.len() || to < 0 || to as usize >= ids.len() {
        return None;
    }
    let mut out = ids.to_vec();
    out.swap(index, to as usize);
    Some(out)
}

/// One row of the split form as typed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SplitDraft {
    /// 0 = no category, n = the n-th category (1-based).
    pub category_index: i32,
    pub amount: String,
    pub note: String,
}

/// Cents still to distribute: booking amount minus the parseable amounts typed so far.
pub fn split_remaining(booking_cents: i64, drafts: &[SplitDraft]) -> i64 {
    drafts
        .iter()
        .filter_map(|d| parse_cents(&d.amount))
        .fold(booking_cents, |rest, c| rest.saturating_sub(c))
}

/// Validates the split form. An empty (all rows blank) form removes the split.
pub fn build_splits(
    drafts: &[SplitDraft],
    categories: &[Category],
    booking_cents: i64,
) -> Result<Vec<TransactionSplitInput>, String> {
    let filled: Vec<&SplitDraft> = drafts
        .iter()
        .filter(|d| !(d.amount.trim().is_empty() && d.note.trim().is_empty()))
        .collect();
    if filled.is_empty() {
        return Ok(Vec::new());
    }
    if filled.len() < 2 {
        return Err("Eine Aufteilung braucht mindestens zwei Teilbeträge.".into());
    }
    let mut out = Vec::new();
    for (n, d) in filled.iter().enumerate() {
        let cents = parse_cents(&d.amount)
            .ok_or_else(|| format!("Teilbetrag {}: ungültiger Betrag.", n + 1))?;
        if cents == 0 {
            return Err(format!("Teilbetrag {} darf nicht 0 sein.", n + 1));
        }
        if (cents < 0) != (booking_cents < 0) {
            return Err(format!(
                "Teilbetrag {} braucht dasselbe Vorzeichen wie die Buchung.",
                n + 1
            ));
        }
        let category_id = match usize::try_from(d.category_index) {
            Ok(0) | Err(_) => None,
            Ok(i) => Some(
                categories
                    .get(i - 1)
                    .ok_or_else(|| format!("Teilbetrag {}: unbekannte Kategorie.", n + 1))?
                    .id,
            ),
        };
        out.push(TransactionSplitInput {
            category_id,
            amount_cents: cents,
            note: d.note.trim().to_string(),
        });
    }
    let sum: i64 = out.iter().map(|s| s.amount_cents).sum();
    if sum != booking_cents {
        return Err(format!(
            "Die Teilbeträge ({}) ergeben nicht den Buchungsbetrag ({}).",
            format_plain(sum),
            format_plain(booking_cents)
        ));
    }
    Ok(out)
}

/// A budget field: empty clears the budget, otherwise a positive amount.
pub fn parse_budget(text: &str) -> Result<Option<i64>, String> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    match parse_cents(text) {
        Some(c) if c > 0 => Ok(Some(c)),
        Some(_) => Err("Das Budget muss größer als 0 sein.".into()),
        None => Err("Ungültiger Betrag.".into()),
    }
}

/// Form values of a transfer template.
#[derive(Debug, Clone, Default)]
pub struct TemplateForm {
    pub name: String,
    pub recipient: String,
    pub iban: String,
    pub bic: String,
    pub amount: String,
    pub purpose: String,
}

/// Builds the template to save (`id` 0 = new). The IBAN is checked for shape only; the core and
/// the bank validate it on the actual transfer.
pub fn build_template(form: &TemplateForm, id: i64) -> Result<TransferTemplate, String> {
    let name = form.name.trim();
    if name.is_empty() {
        return Err("Bitte einen Namen für die Vorlage eingeben.".into());
    }
    if form.recipient.trim().is_empty() {
        return Err("Bitte den Empfänger eingeben.".into());
    }
    let iban: String = form.iban.chars().filter(|c| !c.is_whitespace()).collect();
    let iban = iban.to_uppercase();
    if iban.len() < 15 || iban.len() > 34 || !iban.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err("Die IBAN ist ungültig.".into());
    }
    let amount = if form.amount.trim().is_empty() {
        None
    } else {
        match parse_cents(&form.amount) {
            Some(c) if c > 0 => Some(c),
            _ => return Err("Der Betrag muss größer als 0 sein.".into()),
        }
    };
    Ok(TransferTemplate {
        id,
        name: name.to_string(),
        recipient_name: form.recipient.trim().to_string(),
        recipient_iban: iban,
        recipient_bic: form.bic.trim().to_uppercase(),
        default_amount_cents: amount,
        default_purpose: form.purpose.trim().to_string(),
    })
}

/// "12,50 €" for the budget / template lists.
pub fn euro(cents: i64) -> String {
    format!("{} €", format_plain(cents))
}

/// "Urlaub, Steuer" for the inspector line; empty when there are no tags.
pub fn tag_summary(tags: &[girocrab_core::tags::BookingTag]) -> String {
    tags.iter()
        .map(|t| t.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// "3 Buchungen" / "1 Buchung" for the tag list.
pub fn tag_detail(booking_count: u32) -> String {
    match booking_count {
        0 => "unbenutzt".into(),
        1 => "1 Buchung".into(),
        n => format!("{n} Buchungen"),
    }
}

/// The tag ids after adding (`add`) or removing `tag` from `ids`; no duplicates, order kept.
pub fn with_tag(ids: &[i64], tag: i64, add: bool) -> Vec<i64> {
    let mut out: Vec<i64> = ids.iter().copied().filter(|i| *i != tag).collect();
    if add {
        out.push(tag);
    }
    out
}

/// Indexes of the tags a tag can be merged into: all but the selected one, in list order.
pub fn merge_candidates(count: usize, selected: usize) -> Vec<usize> {
    (0..count).filter(|i| *i != selected).collect()
}

/// "Juli 2026" for the budget dialog.
pub fn month_title(year: i32, month: u32) -> String {
    const NAMES: [&str; 12] = [
        "Januar",
        "Februar",
        "März",
        "April",
        "Mai",
        "Juni",
        "Juli",
        "August",
        "September",
        "Oktober",
        "November",
        "Dezember",
    ];
    let name = NAMES.get((month as usize).wrapping_sub(1)).unwrap_or(&"");
    format!("{name} {year}")
}

/// Share of the budget spent, 0.0 and up (1.0 = used up); 0 for a budget of 0.
pub fn budget_progress(spent_cents: i64, budget_cents: i64) -> f32 {
    if budget_cents <= 0 {
        return 0.0;
    }
    (spent_cents.max(0) as f64 / budget_cents as f64) as f32
}

/// "120,00 € von 300,00 €" for a budget row.
pub fn budget_detail(spent_cents: i64, budget_cents: i64) -> String {
    format!("{} von {}", euro(spent_cents), euro(budget_cents))
}

/// Second line of a template row: recipient, IBAN and the default amount when there is one.
pub fn template_detail(t: &TransferTemplate) -> String {
    let mut s = format!("{} · {}", t.recipient_name, t.recipient_iban);
    if let Some(c) = t.default_amount_cents {
        s.push_str(" · ");
        s.push_str(&euro(c));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_helpers() {
        use girocrab_core::tags::BookingTag;
        let t = |n: &str| BookingTag {
            id: 1,
            name: n.into(),
            color_hex: String::new(),
        };
        assert_eq!(tag_summary(&[]), "");
        assert_eq!(tag_summary(&[t("Urlaub"), t("Steuer")]), "Urlaub, Steuer");
        assert_eq!(tag_detail(0), "unbenutzt");
        assert_eq!(tag_detail(1), "1 Buchung");
        assert_eq!(tag_detail(7), "7 Buchungen");
        assert_eq!(with_tag(&[1, 2], 3, true), vec![1, 2, 3]);
        assert_eq!(with_tag(&[1, 2, 3], 2, true), vec![1, 3, 2]);
        assert_eq!(with_tag(&[1, 2, 3], 2, false), vec![1, 3]);
        assert_eq!(with_tag(&[1], 9, false), vec![1]);
        assert_eq!(merge_candidates(4, 1), vec![0, 2, 3]);
        assert!(merge_candidates(1, 0).is_empty());
    }

    #[test]
    fn budget_and_template_text() {
        assert_eq!(month_title(2026, 7), "Juli 2026");
        assert_eq!(month_title(2026, 12), "Dezember 2026");
        assert_eq!(budget_progress(15_000, 30_000), 0.5);
        assert!(budget_progress(45_000, 30_000) > 1.0);
        assert_eq!(budget_progress(-500, 30_000), 0.0);
        assert_eq!(budget_progress(100, 0), 0.0);
        assert_eq!(budget_detail(12_000, 30_000), "120,00 € von 300,00 €");
        let t = TransferTemplate {
            id: 1,
            name: "Miete".into(),
            recipient_name: "Vermieter".into(),
            recipient_iban: "DE02120300000000202051".into(),
            recipient_bic: String::new(),
            default_amount_cents: Some(85_000),
            default_purpose: String::new(),
        };
        assert_eq!(
            template_detail(&t),
            "Vermieter · DE02120300000000202051 · 850,00 €"
        );
    }

    fn cat(id: i64) -> Category {
        Category {
            id,
            parent_id: None,
            name: format!("c{id}"),
            sf_symbol: String::new(),
            color_hex: String::new(),
            sort_order: 0,
            depth: 0,
            path: format!("c{id}"),
            own_total_cents: 0,
            total_cents: 0,
            transaction_count: 0,
            budget_cents: None,
        }
    }

    #[test]
    fn parses_german_and_plain_amounts() {
        assert_eq!(parse_cents("12,50"), Some(1250));
        assert_eq!(parse_cents("1.234,56"), Some(123_456));
        assert_eq!(parse_cents("-3.20"), Some(-320));
        assert_eq!(parse_cents("7"), Some(700));
        assert_eq!(parse_cents("12,5 €"), Some(1250));
        assert_eq!(parse_cents("1.234"), Some(123_400));
        assert_eq!(parse_cents(",5"), Some(50));
        assert_eq!(parse_cents(""), None);
        assert_eq!(parse_cents("abc"), None);
        assert_eq!(parse_cents("1,2,3x"), None);
    }

    #[test]
    fn formats_plain() {
        assert_eq!(format_plain(-1234), "-12,34");
        assert_eq!(format_plain(5), "0,05");
    }

    #[test]
    fn tag_name_rules() {
        assert_eq!(validate_tag_name("  Urlaub "), Ok("Urlaub".into()));
        assert!(validate_tag_name("  ").is_err());
        assert!(validate_tag_name(&"x".repeat(61)).is_err());
    }

    #[test]
    fn palette_lookup() {
        let p = vec!["#FF0000".to_string(), "#00FF00".to_string()];
        assert_eq!(tag_color(&p, 1), "#00FF00");
        assert_eq!(tag_color(&p, -1), "");
        assert_eq!(tag_color(&p, 9), "");
        assert_eq!(color_index(&p, "#00ff00"), 1);
        assert_eq!(color_index(&p, "#123456"), -1);
    }

    #[test]
    fn reorders_within_bounds() {
        assert_eq!(moved_order(&[1, 2, 3], 1, -1), Some(vec![2, 1, 3]));
        assert_eq!(moved_order(&[1, 2, 3], 1, 1), Some(vec![1, 3, 2]));
        assert_eq!(moved_order(&[1, 2, 3], 0, -1), None);
        assert_eq!(moved_order(&[1, 2, 3], 2, 1), None);
        assert_eq!(moved_order(&[1, 2, 3], 5, -1), None);
    }

    fn draft(c: i32, a: &str) -> SplitDraft {
        SplitDraft {
            category_index: c,
            amount: a.into(),
            note: String::new(),
        }
    }

    #[test]
    fn remaining_counts_valid_rows_only() {
        assert_eq!(
            split_remaining(-1000, &[draft(0, "-4"), draft(0, "x")]),
            -600
        );
    }

    #[test]
    fn splits_must_add_up() {
        let cats: Vec<Category> = Vec::new();
        let with_cats = vec![cat(7), cat(8)];
        let named = build_splits(&[draft(2, "-4"), draft(1, "-6")], &with_cats, -1000).unwrap();
        assert_eq!(named[0].category_id, Some(8));
        assert_eq!(named[1].category_id, Some(7));
        assert!(build_splits(&[], &cats, -1000).unwrap().is_empty());
        assert!(
            build_splits(&[draft(0, ""), draft(0, " ")], &cats, -1000)
                .unwrap()
                .is_empty()
        );
        assert!(build_splits(&[draft(0, "-10")], &cats, -1000).is_err());
        assert!(build_splits(&[draft(0, "-4"), draft(0, "-5")], &cats, -1000).is_err());
        assert!(build_splits(&[draft(0, "4"), draft(0, "-14")], &cats, -1000).is_err());
        assert!(build_splits(&[draft(0, "0"), draft(0, "-10")], &cats, -1000).is_err());
        assert!(build_splits(&[draft(0, "-4"), draft(3, "-6")], &cats, -1000).is_err());
        let ok = build_splits(&[draft(0, "-4"), draft(0, "-6")], &cats, -1000).unwrap();
        assert_eq!(ok.len(), 2);
        assert_eq!(ok[0].amount_cents, -400);
        assert_eq!(ok[0].category_id, None);
    }

    #[test]
    fn budget_field() {
        assert_eq!(parse_budget(""), Ok(None));
        assert_eq!(parse_budget("250"), Ok(Some(25_000)));
        assert!(parse_budget("0").is_err());
        assert!(parse_budget("-5").is_err());
        assert!(parse_budget("x").is_err());
    }

    #[test]
    fn template_validation() {
        let mut f = TemplateForm {
            name: "Miete".into(),
            recipient: "Vermieter".into(),
            iban: "de89 3704 0044 0532 0130 00".into(),
            bic: "cobadeff".into(),
            amount: "800".into(),
            purpose: " Miete ".into(),
        };
        let t = build_template(&f, 0).unwrap();
        assert_eq!(t.recipient_iban, "DE89370400440532013000");
        assert_eq!(t.recipient_bic, "COBADEFF");
        assert_eq!(t.default_amount_cents, Some(80_000));
        assert_eq!(t.default_purpose, "Miete");
        f.amount.clear();
        assert_eq!(build_template(&f, 4).unwrap().default_amount_cents, None);
        f.iban = "DE12".into();
        assert!(build_template(&f, 0).is_err());
        f.iban = "DE89370400440532013000".into();
        f.name = " ".into();
        assert!(build_template(&f, 0).is_err());
    }
}
