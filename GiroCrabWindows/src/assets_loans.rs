//! Pure logic of the Windows front end for Sachwerte (tangible assets), the classification of
//! positions and manual loans: number / date parsing, form validation, texts. No Slint and no vault
//! access here, so everything is unit-tested on every platform.

use girocrab_core::asset_classification::{AssetClass, InstrumentKind};
use girocrab_core::assets::{AssetInterpolation, AssetItem, AssetItemSetup, MANUAL_SOURCE};
use girocrab_core::loans::{LoanLink, LoanSetup, LoanStatus, ManualLoan};

use crate::planning::{euro, format_plain, parse_cents};

/// Asset classes the override menu offers (index 0 = automatic).
pub const CLASS_KEYS: [&str; 9] = [
    "unclassified",
    "equity",
    "bond",
    "real_estate",
    "commodity",
    "crypto",
    "cash",
    "multi_asset",
    "other",
];

/// Instrument kinds the override menu offers (index 0 = automatic).
pub const INSTRUMENT_KEYS: [&str; 8] = [
    "unclassified",
    "single",
    "etf",
    "fund",
    "etc_etn",
    "derivative",
    "direct",
    "other",
];

pub fn instrument_label(k: InstrumentKind) -> &'static str {
    match k {
        InstrumentKind::Unclassified => "Nicht klassifiziert",
        InstrumentKind::SingleSecurity => "Einzeltitel",
        InstrumentKind::Etf => "ETF",
        InstrumentKind::Fund => "Fonds",
        InstrumentKind::EtcEtn => "ETC / ETN",
        InstrumentKind::Derivative => "Derivat",
        InstrumentKind::DirectHolding => "Direktbestand",
        InstrumentKind::Other => "Sonstiges",
    }
}

/// Menu entries "Automatisch" + one per class, in `CLASS_KEYS` order after the first.
pub fn class_menu() -> Vec<String> {
    std::iter::once("Automatisch".to_string())
        .chain(
            CLASS_KEYS
                .iter()
                .skip(1)
                .map(|k| crate::portfolio::class_label(AssetClass::from_db(k)).to_string()),
        )
        .collect()
}

pub fn instrument_menu() -> Vec<String> {
    std::iter::once("Automatisch".to_string())
        .chain(
            INSTRUMENT_KEYS
                .iter()
                .skip(1)
                .map(|k| instrument_label(InstrumentKind::from_db(k)).to_string()),
        )
        .collect()
}

/// Menu index -> override (`None` = automatic).
pub fn class_at(index: i32) -> Option<AssetClass> {
    usize::try_from(index)
        .ok()
        .filter(|i| *i >= 1)
        .and_then(|i| CLASS_KEYS.get(i))
        .map(|k| AssetClass::from_db(k))
}

pub fn instrument_at(index: i32) -> Option<InstrumentKind> {
    usize::try_from(index)
        .ok()
        .filter(|i| *i >= 1)
        .and_then(|i| INSTRUMENT_KEYS.get(i))
        .map(|k| InstrumentKind::from_db(k))
}

/// Parses "12", "12,5", "1.234,5" or "0.5" (a pure dot with one to three digits after it counts
/// as a decimal point unless thousands grouping fits) into a number.
pub fn parse_number(text: &str) -> Option<f64> {
    let t = text.trim().replace([' ', '\u{a0}'], "");
    if t.is_empty() || t.starts_with('-') {
        return None;
    }
    let normalized = match (t.rfind(','), t.rfind('.')) {
        (Some(c), Some(d)) if c > d => t.replace('.', "").replace(',', "."),
        (Some(_), Some(_)) => t.replace(',', ""),
        (Some(_), None) => t.replace(',', "."),
        _ => t,
    };
    normalized.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// `1.5` -> `1,5` (edit fields); whole numbers without a decimal part.
pub fn format_number(v: f64) -> String {
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    s.replace('.', ",")
}

/// Valid `YYYY-MM-DD` (or German `TT.MM.JJJJ`) -> ISO.
pub fn parse_date(text: &str) -> Option<String> {
    let t = text.trim();
    let nums = |sep: char| -> Option<Vec<u32>> {
        t.split(sep)
            .map(|p| {
                (!p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
                    .then(|| p.parse().ok())
                    .flatten()
            })
            .collect()
    };
    let (y, m, d) = match (nums('-').as_deref(), nums('.').as_deref()) {
        (Some([y, m, d]), _) if t.len() == 10 => (*y, *m, *d),
        (_, Some([d, m, y])) if t.len() == 10 => (*y, *m, *d),
        _ => return None,
    };
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let days = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    (y >= 1000 && (1..=days).contains(&d)).then(|| format!("{y:04}-{m:02}-{d:02}"))
}

pub fn require_date(text: &str, what: &str) -> Result<String, String> {
    parse_date(text)
        .ok_or_else(|| format!("{what}: Bitte ein Datum wie 2026-11-16 oder 16.11.2026 eingeben."))
}

pub fn require_cents(text: &str, what: &str) -> Result<i64, String> {
    parse_cents(text).ok_or_else(|| format!("{what}: Bitte einen Betrag wie 1.234,56 eingeben."))
}

pub fn optional_cents(text: &str, what: &str) -> Result<Option<i64>, String> {
    if text.trim().is_empty() {
        Ok(None)
    } else {
        require_cents(text, what).map(Some)
    }
}

/// Interest rate "4,99" (% p.a.) -> millionths (49 900); empty = none.
pub fn parse_rate_ppm(text: &str) -> Result<Option<i64>, String> {
    let t = text.trim().trim_end_matches('%').trim();
    if t.is_empty() {
        return Ok(None);
    }
    let v = parse_number(t)
        .ok_or_else(|| "Zinssatz: Bitte einen Prozentwert wie 4,99 eingeben.".to_string())?;
    Ok(Some((v * 10_000.0).round() as i64))
}

pub fn format_rate(ppm: Option<i64>) -> String {
    ppm.map(|p| format_number(p as f64 / 10_000.0))
        .unwrap_or_default()
}

// ------------------------------------------------------------------------------ Sachwerte

/// Raw text of the item form.
#[derive(Debug, Clone, Default)]
pub struct ItemForm {
    pub name: String,
    /// `"manual"` or a price symbol.
    pub source: String,
    pub weight: String,
    pub purity: String,
    pub linear: bool,
    pub date: String,
    pub quantity: String,
    pub price: String,
    pub start_value: String,
    pub note: String,
    pub symbol: String,
}

pub fn build_item(f: &ItemForm) -> Result<AssetItemSetup, String> {
    let name = f.name.trim();
    if name.is_empty() {
        return Err("Bitte einen Namen für den Sachwert eingeben.".into());
    }
    let manual = f.source == MANUAL_SOURCE;
    let quantity = parse_number(&f.quantity)
        .filter(|q| *q > 0.0)
        .ok_or("Anzahl: Bitte eine Zahl größer als 0 eingeben.")?;
    let (weight, purity) = if manual {
        (0.0, 1000.0)
    } else {
        let w = parse_number(&f.weight)
            .filter(|w| *w > 0.0)
            .ok_or("Gewicht: Bitte das Gewicht eines Stücks in Gramm eingeben.")?;
        let p = if f.purity.trim().is_empty() {
            1000.0
        } else {
            parse_number(&f.purity)
                .filter(|p| *p > 0.0 && *p <= 1000.0)
                .ok_or("Feingehalt: Bitte einen Wert zwischen 1 und 1000 (Promille) eingeben.")?
        };
        (w, p)
    };
    let start = if manual {
        Some(require_cents(&f.start_value, "Wert je Stück")?)
    } else {
        None
    };
    Ok(AssetItemSetup {
        name: name.to_string(),
        price_source: f.source.clone(),
        weight_grams: weight,
        purity_permille: purity,
        interpolation: if f.linear {
            AssetInterpolation::Linear
        } else {
            AssetInterpolation::Step
        },
        purchase_date: require_date(&f.date, "Kaufdatum")?,
        quantity,
        purchase_price_cents: optional_cents(&f.price, "Kaufpreis")?,
        start_value_cents: start,
        note: f.note.trim().to_string(),
        symbol: f.symbol.trim().to_string(),
    })
}

pub fn item_form(setup: &AssetItemSetup) -> ItemForm {
    ItemForm {
        name: setup.name.clone(),
        source: setup.price_source.clone(),
        weight: if setup.weight_grams > 0.0 {
            format_number(setup.weight_grams)
        } else {
            String::new()
        },
        purity: format_number(setup.purity_permille),
        linear: setup.interpolation == AssetInterpolation::Linear,
        date: setup.purchase_date.clone(),
        quantity: format_number(setup.quantity),
        price: setup
            .purchase_price_cents
            .map(format_plain)
            .unwrap_or_default(),
        start_value: setup
            .start_value_cents
            .map(format_plain)
            .unwrap_or_default(),
        note: setup.note.clone(),
        symbol: setup.symbol.clone(),
    }
}

/// "3 Stück · 1.234,00 € · Stand 2026-11-16" for the list.
pub fn item_detail(it: &AssetItem) -> String {
    if it.sold {
        return "verkauft".into();
    }
    let mut parts = vec![format!("{} Stück", format_number(it.holding.quantity))];
    parts.push(euro(it.holding.value_cents));
    if !it.value_date.is_empty() {
        parts.push(format!("Stand {}", it.value_date));
    }
    if let Some(d) = &it.missing_price_date {
        parts.push(format!("Kurs für {d} fehlt"));
    }
    parts.join(" · ")
}

/// Lowest, highest and latest value of a daily series, as one line.
pub fn history_summary(points: &[(String, i64)]) -> String {
    let (Some(first), Some(last)) = (points.first(), points.last()) else {
        return "Kein Verlauf.".into();
    };
    let min = points.iter().map(|p| p.1).min().unwrap_or(0);
    let max = points.iter().map(|p| p.1).max().unwrap_or(0);
    format!(
        "{} Tage ab {}: niedrigster Wert {}, höchster {}, aktuell {}",
        points.len(),
        first.0,
        euro(min),
        euro(max),
        euro(last.1)
    )
}

// -------------------------------------------------------------------------------- Darlehen

/// Raw text of the loan form.
#[derive(Debug, Clone, Default)]
pub struct LoanForm {
    pub name: String,
    pub original: String,
    pub anchor_date: String,
    pub anchor_balance: String,
    pub rate: String,
    pub expr: String,
}

pub fn build_loan(f: &LoanForm) -> Result<LoanSetup, String> {
    let name = f.name.trim();
    if name.is_empty() {
        return Err("Bitte einen Namen für das Darlehen eingeben.".into());
    }
    let balance = require_cents(&f.anchor_balance, "Restschuld")?;
    if balance < 0 {
        return Err("Restschuld: Bitte den offenen Betrag positiv eingeben.".into());
    }
    let original = optional_cents(&f.original, "Ursprünglicher Betrag")?;
    if original.is_some_and(|o| o <= 0) {
        return Err("Ursprünglicher Betrag: Bitte einen Betrag größer als 0 eingeben.".into());
    }
    Ok(LoanSetup {
        name: name.to_string(),
        original_cents: original,
        anchor_date: require_date(&f.anchor_date, "Stand-Datum")?,
        anchor_balance_cents: balance,
        interest_rate_ppm: parse_rate_ppm(&f.rate)?,
        match_expr: f.expr.trim().to_string(),
    })
}

pub fn loan_form(s: &LoanSetup) -> LoanForm {
    LoanForm {
        name: s.name.clone(),
        original: s.original_cents.map(format_plain).unwrap_or_default(),
        anchor_date: s.anchor_date.clone(),
        anchor_balance: format_plain(s.anchor_balance_cents),
        rate: format_rate(s.interest_rate_ppm),
        expr: s.match_expr.clone(),
    }
}

pub fn loan_detail(l: &ManualLoan) -> String {
    format!(
        "Rest {} · {} Zahlungen",
        euro(l.status.remaining_cents),
        l.status.payment_count
    )
}

/// Multi-line state of a loan: remaining debt, payments, split into principal and interest, term.
pub fn status_text(s: &LoanStatus) -> String {
    let mut lines = vec![format!(
        "Restschuld {} (Stand {})",
        euro(s.remaining_cents),
        s.as_of_date
    )];
    lines.push(format!(
        "{} Zahlungen nach dem Stand-Datum: {} gezahlt, davon {} Tilgung und {} Zinsen",
        s.payment_count,
        euro(s.paid_cents),
        euro(s.principal_cents),
        euro(s.interest_cents)
    ));
    if s.before_anchor_count > 0 {
        lines.push(format!(
            "{} verknüpfte Buchungen vor dem Stand-Datum werden nicht gezählt",
            s.before_anchor_count
        ));
    }
    if let (Some(c), false) = (s.last_payment_cents, s.last_payment_date.is_empty()) {
        lines.push(format!(
            "Letzte Zahlung {} am {}",
            euro(c),
            s.last_payment_date
        ));
    }
    if let Some(m) = s.months_left {
        lines.push(format!(
            "Bei gleicher Rate noch ca. {m} Monate ({})",
            years_months(m)
        ));
    }
    lines.join("\n")
}

/// "5 verknüpfte Buchungen (3 per Suchausdruck, 2 von Hand)" for one loan, or all loans.
pub fn link_summary(links: &[LoanLink], loan: Option<i64>) -> String {
    let mine: Vec<&LoanLink> = links
        .iter()
        .filter(|l| loan.is_none_or(|id| l.loan_account_id == id))
        .collect();
    if mine.is_empty() {
        return "Noch keine Buchungen verknüpft.".into();
    }
    let rule = mine.iter().filter(|l| l.by_rule).count();
    format!(
        "{} verknüpfte Buchungen ({rule} per Suchausdruck, {} von Hand)",
        mine.len(),
        mine.len() - rule
    )
}

fn years_months(m: u32) -> String {
    match (m / 12, m % 12) {
        (0, mo) => format!("{mo} Mon."),
        (y, 0) => format!("{y} J."),
        (y, mo) => format!("{y} J. {mo} Mon."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form() -> ItemForm {
        ItemForm {
            name: "Krügerrand".into(),
            source: "XAU".into(),
            weight: "31,1".into(),
            purity: "916,7".into(),
            linear: false,
            date: "2024-03-01".into(),
            quantity: "2".into(),
            price: "".into(),
            start_value: "".into(),
            note: "".into(),
            symbol: "".into(),
        }
    }

    #[test]
    fn numbers_accept_german_and_plain() {
        assert_eq!(parse_number("12"), Some(12.0));
        assert_eq!(parse_number("12,5"), Some(12.5));
        assert_eq!(parse_number("1.234,5"), Some(1234.5));
        assert_eq!(parse_number("0.5"), Some(0.5));
        assert_eq!(parse_number("1,234.5"), Some(1234.5));
        assert_eq!(parse_number(""), None);
        assert_eq!(parse_number("-1"), None);
        assert_eq!(parse_number("abc"), None);
    }

    #[test]
    fn numbers_format_without_trailing_zeros() {
        assert_eq!(format_number(2.0), "2");
        assert_eq!(format_number(31.1035), "31,1035");
        assert_eq!(format_number(916.7), "916,7");
    }

    #[test]
    fn dates_accept_iso_and_german() {
        assert_eq!(parse_date("2026-11-16").as_deref(), Some("2026-11-16"));
        assert_eq!(parse_date(" 16.11.2026 ").as_deref(), Some("2026-11-16"));
        assert_eq!(parse_date("2026-13-01"), None);
        assert!(
            require_date("x", "Kaufdatum")
                .unwrap_err()
                .starts_with("Kaufdatum:")
        );
    }

    #[test]
    fn rate_round_trips_through_ppm() {
        assert_eq!(parse_rate_ppm("4,99"), Ok(Some(49_900)));
        assert_eq!(parse_rate_ppm("4,99 %"), Ok(Some(49_900)));
        assert_eq!(parse_rate_ppm(""), Ok(None));
        assert!(parse_rate_ppm("viel").is_err());
        assert_eq!(format_rate(Some(49_900)), "4,99");
        assert_eq!(format_rate(None), "");
    }

    #[test]
    fn automatic_item_needs_weight_and_defaults_to_pure() {
        let s = build_item(&form()).unwrap();
        assert_eq!(s.price_source, "XAU");
        assert_eq!(s.weight_grams, 31.1);
        assert_eq!(s.purity_permille, 916.7);
        assert_eq!(s.quantity, 2.0);
        assert_eq!(s.start_value_cents, None);
        let mut f = form();
        f.purity = "".into();
        assert_eq!(build_item(&f).unwrap().purity_permille, 1000.0);
        f.weight = "".into();
        assert!(build_item(&f).unwrap_err().starts_with("Gewicht"));
    }

    #[test]
    fn manual_item_needs_start_value() {
        let mut f = form();
        f.source = MANUAL_SOURCE.into();
        f.weight = "".into();
        assert!(build_item(&f).unwrap_err().starts_with("Wert je Stück"));
        f.start_value = "1.500,00".into();
        f.linear = true;
        let s = build_item(&f).unwrap();
        assert_eq!(s.start_value_cents, Some(150_000));
        assert_eq!(s.interpolation, AssetInterpolation::Linear);
        assert_eq!(s.weight_grams, 0.0);
    }

    #[test]
    fn item_form_validation_messages() {
        let mut f = form();
        f.name = "  ".into();
        assert!(build_item(&f).is_err());
        let mut f = form();
        f.quantity = "0".into();
        assert!(build_item(&f).unwrap_err().starts_with("Anzahl"));
        let mut f = form();
        f.purity = "1200".into();
        assert!(build_item(&f).unwrap_err().starts_with("Feingehalt"));
        let mut f = form();
        f.date = "gestern".into();
        assert!(build_item(&f).unwrap_err().starts_with("Kaufdatum"));
    }

    #[test]
    fn item_setup_round_trips_into_the_form() {
        let mut f = form();
        f.price = "2.000,00".into();
        let s = build_item(&f).unwrap();
        let back = item_form(&s);
        assert_eq!(back.price, "2000,00");
        assert_eq!(back.weight, "31,1");
        assert_eq!(build_item(&back).unwrap(), s);
    }

    #[test]
    fn loan_form_validates_and_round_trips() {
        let f = LoanForm {
            name: "Baukredit".into(),
            original: "150.000,00".into(),
            anchor_date: "31.12.2025".into(),
            anchor_balance: "120.000,00".into(),
            rate: "3,5".into(),
            expr: "Sparkasse".into(),
        };
        let s = build_loan(&f).unwrap();
        assert_eq!(s.anchor_date, "2025-12-31");
        assert_eq!(s.anchor_balance_cents, 12_000_000);
        assert_eq!(s.interest_rate_ppm, Some(35_000));
        assert_eq!(build_loan(&loan_form(&s)).unwrap(), s);
        let mut bad = f.clone();
        bad.anchor_balance = "-5".into();
        assert!(build_loan(&bad).unwrap_err().starts_with("Restschuld"));
        let mut bad = f.clone();
        bad.original = "0".into();
        assert!(build_loan(&bad).is_err());
        let mut bad = f;
        bad.name = "".into();
        assert!(build_loan(&bad).is_err());
    }

    #[test]
    fn status_text_lists_split_and_term() {
        let s = LoanStatus {
            remaining_cents: 100_000,
            as_of_date: "2026-10-01".into(),
            payment_count: 3,
            paid_cents: 30_000,
            principal_cents: 25_000,
            interest_cents: 5_000,
            before_anchor_count: 2,
            last_payment_cents: Some(10_000),
            last_payment_date: "2026-10-01".into(),
            months_left: Some(14),
        };
        let t = status_text(&s);
        assert!(t.contains("Restschuld 1000,00 € (Stand 2026-10-01)"));
        assert!(t.contains("250,00 € Tilgung und 50,00 € Zinsen"));
        assert!(t.contains("2 verknüpfte Buchungen"));
        assert!(t.contains("ca. 14 Monate (1 J. 2 Mon.)"));
        assert!(!status_text(&LoanStatus::default()).contains("Letzte Zahlung"));
    }

    #[test]
    fn link_summary_counts_by_source() {
        let l = |t, loan, by_rule| LoanLink {
            transaction_id: t,
            loan_account_id: loan,
            by_rule,
        };
        let links = [l(1, 7, true), l(2, 7, false), l(3, 8, true)];
        assert_eq!(
            link_summary(&links, Some(7)),
            "2 verknüpfte Buchungen (1 per Suchausdruck, 1 von Hand)"
        );
        assert!(link_summary(&links, None).starts_with("3 verknüpfte"));
        assert_eq!(
            link_summary(&links, Some(9)),
            "Noch keine Buchungen verknüpft."
        );
    }

    #[test]
    fn leap_days_are_checked() {
        assert!(parse_date("2024-02-29").is_some());
        assert!(parse_date("2025-02-29").is_none());
        assert!(parse_date("31.04.2026").is_none());
        assert!(parse_date("1.1.2026").is_none());
    }

    #[test]
    fn history_summary_reports_range() {
        let pts = vec![
            ("2026-01-01".to_string(), 100),
            ("2026-01-02".to_string(), 300),
            ("2026-01-03".to_string(), 200),
        ];
        let t = history_summary(&pts);
        assert!(t.starts_with("3 Tage ab 2026-01-01"));
        assert!(t.contains("niedrigster Wert 1,00 €"));
        assert_eq!(history_summary(&[]), "Kein Verlauf.");
    }

    #[test]
    fn override_menus_start_with_automatic() {
        assert_eq!(class_menu().len(), CLASS_KEYS.len());
        assert_eq!(instrument_menu().len(), INSTRUMENT_KEYS.len());
        assert_eq!(class_menu()[0], "Automatisch");
        assert_eq!(class_at(0), None);
        assert_eq!(class_at(1), Some(AssetClass::Equity));
        assert_eq!(class_at(99), None);
        assert_eq!(instrument_at(2), Some(InstrumentKind::Etf));
        assert_eq!(instrument_at(-1), None);
    }
}
