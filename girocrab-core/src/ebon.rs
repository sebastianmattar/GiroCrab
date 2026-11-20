//! Deterministic local parser for digital receipts (eBons) and ZUGFeRD/Factur-X invoices (#657).
//!
//! Supported text receipts: REWE, Lidl Plus, Edeka, dm, Rossmann. ZUGFeRD / Factur-X input is the
//! embedded `CrossIndustryInvoice` XML. Nothing leaves the device: no network, no model, only
//! regular expressions on the text. [`match_bookings`] finds the bank booking for a receipt
//! (±2 days, exact amount) and [`propose_splits`] builds a balanced category split.

use regex::Regex;
use std::collections::BTreeMap;
use std::sync::LazyLock;

/// Maximum distance in days between receipt date and booking date.
pub const MATCH_WINDOW_DAYS: i64 = 2;

/// Category used for deposit (`Pfand`) lines.
pub const CATEGORY_DEPOSIT: &str = "Pfand";
/// Fallback category when nothing matches.
pub const CATEGORY_OTHER: &str = "Sonstiges";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EbonError {
    #[error("kein Gesamtbetrag im Beleg gefunden")]
    MissingTotal,
    #[error("kein Datum im Beleg gefunden")]
    MissingDate,
    #[error("kein Händler erkannt")]
    UnknownMerchant,
    #[error("ungültiges ZUGFeRD-/Factur-X-XML")]
    InvalidXml,
}

/// One receipt line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EbonItem {
    pub name: String,
    /// Quantity in thousandths (1000 = one piece); 1000 if not stated.
    pub quantity_milli: i64,
    /// Line total in cents (negative for discounts and returned deposit).
    pub total_cents: i64,
    /// VAT rate in hundredths of a percent (700 = 7 %), if known.
    pub vat_rate_bp: Option<u32>,
    pub is_deposit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ebon {
    pub merchant: String,
    /// ISO date `YYYY-MM-DD`.
    pub date: String,
    pub total_cents: i64,
    pub items: Vec<EbonItem>,
    /// Sum of all deposit lines in cents (returned deposit reduces it).
    pub deposit_cents: i64,
}

impl Ebon {
    pub fn items_sum_cents(&self) -> i64 {
        self.items.iter().map(|i| i.total_cents).sum()
    }
}

macro_rules! re {
    ($name:ident, $pat:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| Regex::new($pat).expect("valid regex"));
    };
}

re!(
    RE_ITEM,
    r"^\s*(?P<name>.*?\S)\s+(?P<amt>-?\d{1,3}(?:\.\d{3})*,\d{2}-?)\s*(?P<tax>[A-D*])?\s*$"
);
re!(
    RE_QTY,
    r"^\s*(?P<q>\d+(?:,\d{1,3})?)\s*(?:Stk\.?|St\.?)?\s*[xX*]\s*-?\d{1,3}(?:\.\d{3})*,\d{2}\s*(?:EUR)?\s*$"
);
re!(
    RE_TOTAL,
    r"(?i)^\s*(?:summe|gesamtsumme|gesamtbetrag|zu\s+zahlen|total)\b[^\d\-]*(?P<amt>-?\d{1,3}(?:\.\d{3})*,\d{2})"
);
re!(RE_DATE, r"\b(\d{2})\.(\d{2})\.(\d{4}|\d{2})\b");
re!(
    RE_VAT_LINE,
    r"^\s*(?P<l>[A-D])\b[^\d%\n]*?(?P<r>\d{1,2}(?:,\d{1,2})?)\s*%"
);

/// Known merchants: (display name, regex on the receipt text).
static MERCHANTS: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    [
        ("REWE", r"(?i)\brewe\b"),
        ("Lidl", r"(?i)\blidl\b"),
        ("Edeka", r"(?i)\bedeka\b"),
        ("Rossmann", r"(?i)\brossmann\b"),
        ("dm", r"(?i)\bdm[- ](?:drogerie|markt)|(?m)^\s*dm\s*$"),
    ]
    .into_iter()
    .map(|(n, p)| (n, Regex::new(p).expect("valid regex")))
    .collect()
});

/// Maps a seller name to a known merchant; `None` for others.
pub fn detect_merchant(text: &str) -> Option<&'static str> {
    MERCHANTS
        .iter()
        .find(|(_, re)| re.is_match(text))
        .map(|(name, _)| *name)
}

/// `1.234,56` / `-0,25` / `0,25-` → cents.
pub fn parse_german_amount(s: &str) -> Option<i64> {
    let s = s.trim();
    let (neg_suffix, s) = match s.strip_suffix('-') {
        Some(r) => (true, r),
        None => (false, s),
    };
    let (neg_prefix, s) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s),
    };
    let cleaned: String = s.chars().filter(|c| *c != '.').collect();
    let (int, frac) = cleaned.split_once(',')?;
    if frac.len() != 2 || int.is_empty() {
        return None;
    }
    let cents = int.parse::<i64>().ok()?.checked_mul(100)? + frac.parse::<i64>().ok()?;
    Some(if neg_prefix ^ neg_suffix {
        -cents
    } else {
        cents
    })
}

/// `12.34` (XML, dot decimals) → cents.
fn parse_decimal_cents(s: &str) -> Option<i64> {
    let s = s.trim();
    let (neg, s) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s),
    };
    let (int, frac) = s.split_once('.').unwrap_or((s, ""));
    if int.is_empty() && frac.is_empty() {
        return None;
    }
    let mut frac = frac.to_string();
    // Round half up on the third decimal if present.
    let round_up = frac.len() > 2 && frac.as_bytes()[2] >= b'5';
    frac.truncate(2);
    while frac.len() < 2 {
        frac.push('0');
    }
    let int = if int.is_empty() {
        0
    } else {
        int.parse::<i64>().ok()?
    };
    let cents = int.checked_mul(100)? + frac.parse::<i64>().ok()? + i64::from(round_up);
    Some(if neg { -cents } else { cents })
}

/// `19`, `7,0`, `19.00` → hundredths of a percent.
fn parse_rate_bp(s: &str) -> Option<u32> {
    let v: f64 = s.trim().replace(',', ".").parse().ok()?;
    (0.0..=100.0)
        .contains(&v)
        .then(|| (v * 100.0).round() as u32)
}

fn iso_date(d: u32, m: u32, y: i32) -> Option<String> {
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(format!("{y:04}-{m:02}-{d:02}"))
}

fn find_date(text: &str) -> Option<String> {
    RE_DATE.captures_iter(text).find_map(|c| {
        let d = c[1].parse().ok()?;
        let m = c[2].parse().ok()?;
        let mut y: i32 = c[3].parse().ok()?;
        if c[3].len() == 2 {
            y += 2000;
        }
        iso_date(d, m, y)
    })
}

fn is_deposit_name(name: &str) -> bool {
    let n = name.to_lowercase();
    n.contains("pfand") || n.contains("leergut")
}

/// Parses the plain text of a REWE / Lidl Plus / Edeka / dm / Rossmann eBon.
pub fn parse_ebon_text(text: &str) -> Result<Ebon, EbonError> {
    let merchant = detect_merchant(text).ok_or(EbonError::UnknownMerchant)?;
    let date = find_date(text).ok_or(EbonError::MissingDate)?;

    let lines: Vec<&str> = text.lines().collect();
    let total_idx = lines
        .iter()
        .position(|l| RE_TOTAL.is_match(l))
        .ok_or(EbonError::MissingTotal)?;
    let total_cents = RE_TOTAL
        .captures(lines[total_idx])
        .and_then(|c| parse_german_amount(&c["amt"]))
        .ok_or(EbonError::MissingTotal)?;

    // VAT letters from the footer (`A  7,0 %`).
    let mut vat_by_letter: BTreeMap<char, u32> = BTreeMap::new();
    for line in &lines[total_idx..] {
        if let Some(c) = RE_VAT_LINE.captures(line)
            && let (Some(l), Some(r)) = (c["l"].chars().next(), parse_rate_bp(&c["r"]))
        {
            vat_by_letter.entry(l).or_insert(r);
        }
    }

    let mut items: Vec<EbonItem> = Vec::new();
    for line in &lines[..total_idx] {
        if let Some(q) = RE_QTY.captures(line) {
            if let (Some(last), Ok(v)) = (items.last_mut(), q["q"].replace(',', ".").parse::<f64>())
            {
                last.quantity_milli = (v * 1000.0).round() as i64;
            }
            continue;
        }
        let Some(c) = RE_ITEM.captures(line) else {
            continue;
        };
        let Some(total) = parse_german_amount(&c["amt"]) else {
            continue;
        };
        let name = c["name"].trim().to_string();
        let vat = c
            .name("tax")
            .and_then(|t| t.as_str().chars().next())
            .and_then(|l| vat_by_letter.get(&l).copied());
        items.push(EbonItem {
            is_deposit: is_deposit_name(&name),
            name,
            quantity_milli: 1000,
            total_cents: total,
            vat_rate_bp: vat,
        });
    }

    let deposit_cents = items
        .iter()
        .filter(|i| i.is_deposit)
        .map(|i| i.total_cents)
        .sum();
    Ok(Ebon {
        merchant: merchant.to_string(),
        date,
        total_cents,
        items,
        deposit_cents,
    })
}

// ---------------------------------------------------------------------------------------------
// ZUGFeRD / Factur-X (UN/CEFACT CrossIndustryInvoice)
// ---------------------------------------------------------------------------------------------

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Inner contents of every `<prefix:Tag …>…</prefix:Tag>`, namespace prefix agnostic.
fn xml_blocks<'a>(xml: &'a str, tag: &str) -> Vec<&'a str> {
    let re = Regex::new(&format!(
        r"(?s)<(?:[\w.-]+:)?{tag}\b[^>/]*>(.*?)</(?:[\w.-]+:)?{tag}\s*>"
    ))
    .expect("valid regex");
    re.captures_iter(xml)
        .filter_map(|c| c.get(1))
        .map(|m| m.as_str())
        .collect()
}

fn xml_first<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    xml_blocks(xml, tag).into_iter().next()
}

fn xml_text(xml: &str, tag: &str) -> Option<String> {
    xml_first(xml, tag).map(|s| xml_unescape(s.trim()))
}

/// Parses a ZUGFeRD / Factur-X XML invoice.
pub fn parse_zugferd_xml(xml: &str) -> Result<Ebon, EbonError> {
    if xml_first(xml, "ExchangedDocument").is_none() {
        return Err(EbonError::InvalidXml);
    }
    let seller = xml_first(xml, "SellerTradeParty")
        .and_then(|b| xml_text(b, "Name"))
        .ok_or(EbonError::UnknownMerchant)?;
    let merchant = detect_merchant(&seller)
        .map(str::to_string)
        .unwrap_or(seller);

    let date = xml_first(xml, "IssueDateTime")
        .and_then(|b| xml_first(b, "DateTimeString"))
        .map(str::trim)
        .filter(|s| s.len() == 8 && s.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|s| {
            iso_date(
                s[6..8].parse().ok()?,
                s[4..6].parse().ok()?,
                s[..4].parse().ok()?,
            )
        })
        .ok_or(EbonError::MissingDate)?;

    let total_cents = xml_text(xml, "GrandTotalAmount")
        .or_else(|| xml_text(xml, "DuePayableAmount"))
        .and_then(|s| parse_decimal_cents(&s))
        .ok_or(EbonError::MissingTotal)?;

    let mut items = Vec::new();
    for line in xml_blocks(xml, "IncludedSupplyChainTradeLineItem") {
        let Some(total) = xml_text(line, "LineTotalAmount").and_then(|s| parse_decimal_cents(&s))
        else {
            continue;
        };
        let name = xml_text(line, "Name").unwrap_or_default();
        let quantity_milli = xml_text(line, "BilledQuantity")
            .and_then(|s| s.parse::<f64>().ok())
            .map(|v| (v * 1000.0).round() as i64)
            .unwrap_or(1000);
        let vat_rate_bp = xml_text(line, "RateApplicablePercent").and_then(|s| parse_rate_bp(&s));
        items.push(EbonItem {
            is_deposit: is_deposit_name(&name),
            name,
            quantity_milli,
            total_cents: total,
            vat_rate_bp,
        });
    }
    let deposit_cents = items
        .iter()
        .filter(|i| i.is_deposit)
        .map(|i| i.total_cents)
        .sum();
    Ok(Ebon {
        merchant,
        date,
        total_cents,
        items,
        deposit_cents,
    })
}

/// Picks the parser: XML input → ZUGFeRD, otherwise receipt text.
pub fn parse_document(input: &str) -> Result<Ebon, EbonError> {
    if input.trim_start().starts_with('<') {
        parse_zugferd_xml(input)
    } else {
        parse_ebon_text(input)
    }
}

// ---------------------------------------------------------------------------------------------
// Booking match
// ---------------------------------------------------------------------------------------------

/// The part of a bank booking needed for matching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookingCandidate {
    pub id: String,
    /// ISO date `YYYY-MM-DD`.
    pub date: String,
    /// Signed amount in cents; purchases are negative.
    pub amount_cents: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookingMatch {
    pub booking_id: String,
    pub day_distance: i64,
}

/// Days since 1970-01-01 for an ISO date.
fn days_from_iso(date: &str) -> Option<i64> {
    let mut it = date.get(..10)?.split('-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: i64 = it.next()?.parse().ok()?;
    let d: i64 = it.next()?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// Bookings within ±[`MATCH_WINDOW_DAYS`] and with exactly the receipt amount (as an expense),
/// closest first, then by id. A positive receipt total (credit note) matches a positive booking.
pub fn match_bookings(ebon: &Ebon, bookings: &[BookingCandidate]) -> Vec<BookingMatch> {
    let Some(day) = days_from_iso(&ebon.date) else {
        return Vec::new();
    };
    let mut out: Vec<BookingMatch> = bookings
        .iter()
        .filter(|b| b.amount_cents == -ebon.total_cents)
        .filter_map(|b| {
            let distance = (days_from_iso(&b.date)? - day).abs();
            (distance <= MATCH_WINDOW_DAYS).then(|| BookingMatch {
                booking_id: b.id.clone(),
                day_distance: distance,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        a.day_distance
            .cmp(&b.day_distance)
            .then_with(|| a.booking_id.cmp(&b.booking_id))
    });
    out
}

// ---------------------------------------------------------------------------------------------
// Split proposal
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitProposal {
    pub category: String,
    /// Positive cents of the expense share.
    pub cents: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitProposals {
    pub splits: Vec<SplitProposal>,
    /// Difference receipt total − sum of lines (discounts, rounding) that was added to the
    /// largest split so the splits add up to the total.
    pub residual_cents: i64,
}

const CATEGORY_KEYWORDS: &[(&str, &[&str])] = &[
    (
        "Getränke",
        &[
            "wasser", "saft", "cola", "limo", "bier", "wein", "sekt", "tee", "kaffee", "schorle",
            "getränk",
        ],
    ),
    (
        "Drogerie",
        &[
            "shampoo",
            "duschgel",
            "zahn",
            "seife",
            "deo",
            "creme",
            "windel",
            "tampon",
            "waschmittel",
            "spülmittel",
            "toilettenpapier",
            "taschentücher",
            "binden",
            "pflege",
        ],
    ),
    (
        "Lebensmittel",
        &[
            "milch",
            "brot",
            "käse",
            "butter",
            "ei",
            "obst",
            "gemüse",
            "fleisch",
            "wurst",
            "nudel",
            "reis",
            "joghurt",
            "apfel",
            "banane",
            "tomate",
            "kartoffel",
            "mehl",
            "zucker",
            "schoko",
            "müsli",
            "bio",
        ],
    ),
];

/// Default keyword classifier for item names.
pub fn default_category(item: &EbonItem) -> String {
    if item.is_deposit {
        return CATEGORY_DEPOSIT.to_string();
    }
    let n = item.name.to_lowercase();
    for (cat, words) in CATEGORY_KEYWORDS {
        if words.iter().any(|w| {
            if w.len() <= 2 {
                n.split(|c: char| !c.is_alphanumeric()).any(|t| t == *w)
            } else {
                n.contains(w)
            }
        }) {
            return (*cat).to_string();
        }
    }
    CATEGORY_OTHER.to_string()
}

/// Groups the lines by category (`classify`, e.g. [`default_category`] or user rules) and
/// returns splits that add up to exactly the receipt total (absolute value). Deposit is its own
/// split unless it nets to zero. Any residual is added to the largest non-deposit split.
pub fn propose_splits_with(ebon: &Ebon, classify: &dyn Fn(&EbonItem) -> String) -> SplitProposals {
    let mut by_cat: BTreeMap<String, i64> = BTreeMap::new();
    for item in &ebon.items {
        let cat = if item.is_deposit {
            CATEGORY_DEPOSIT.to_string()
        } else {
            classify(item)
        };
        *by_cat.entry(cat).or_default() += item.total_cents;
    }
    by_cat.retain(|_, v| *v != 0);

    let residual = ebon.total_cents - by_cat.values().sum::<i64>();
    if residual != 0 {
        let target = by_cat
            .iter()
            .filter(|(k, _)| k.as_str() != CATEGORY_DEPOSIT)
            .max_by_key(|(_, v)| v.abs())
            .or_else(|| by_cat.iter().max_by_key(|(_, v)| v.abs()))
            .map(|(k, _)| k.clone())
            .unwrap_or_else(|| CATEGORY_OTHER.to_string());
        *by_cat.entry(target).or_default() += residual;
    }
    by_cat.retain(|_, v| *v != 0);

    let mut splits: Vec<SplitProposal> = by_cat
        .into_iter()
        .map(|(category, cents)| SplitProposal { category, cents })
        .collect();
    splits.sort_by(|a, b| {
        b.cents
            .cmp(&a.cents)
            .then_with(|| a.category.cmp(&b.category))
    });
    SplitProposals {
        splits,
        residual_cents: residual,
    }
}

/// [`propose_splits_with`] using [`default_category`].
pub fn propose_splits(ebon: &Ebon) -> SplitProposals {
    propose_splits_with(ebon, &default_category)
}

#[cfg(test)]
mod tests {
    use super::*;

    const REWE: &str = "REWE Markt GmbH\nHauptstr. 12\n12345 Berlin\n\
        BIO MILCH 1,5%            1,09 B\n\
        2 x 0,99\n\
        BANANEN                   1,98 B\n\
        SHAMPOO                   2,95 A\n\
        PFAND 0,25                0,25 A\n\
        LEERGUT                  -0,25 A\n\
        PFAND MEHRWEG             0,15 A\n\
        -------------------------\n\
        SUMME            EUR      6,17\n\
        Geg. Karte                6,17\n\
        Steuer %   Netto  Steuer  Brutto\n\
        A  19,0%   3,10   0,59    3,69\n\
        B   7,0%   2,87   0,20    3,07\n\
        Datum: 14.03.2025  18:42\n";

    #[test]
    fn amounts() {
        assert_eq!(parse_german_amount("1.234,56"), Some(123_456));
        assert_eq!(parse_german_amount("-0,25"), Some(-25));
        assert_eq!(parse_german_amount("0,25-"), Some(-25));
        assert_eq!(parse_german_amount("1,5"), None);
        assert_eq!(parse_decimal_cents("12.3"), Some(1230));
        assert_eq!(parse_decimal_cents("-4.567"), Some(-457));
    }

    #[test]
    fn parses_rewe_text() {
        let e = parse_ebon_text(REWE).unwrap();
        assert_eq!(e.merchant, "REWE");
        assert_eq!(e.date, "2025-03-14");
        assert_eq!(e.total_cents, 617);
        assert_eq!(e.items.len(), 6);
        assert_eq!(e.items[0].vat_rate_bp, Some(700));
        assert_eq!(e.items[2].vat_rate_bp, Some(1900));
        assert_eq!(e.items[0].quantity_milli, 2000);
        assert_eq!(e.deposit_cents, 15);
        assert!(e.items[4].is_deposit && e.items[4].total_cents == -25);
        assert_eq!(e.items_sum_cents(), 617);
    }

    #[test]
    fn detects_other_merchants() {
        for (txt, name) in [
            ("Lidl Plus\n01.02.25\nSumme 1,00\n", "Lidl"),
            ("EDEKA Müller\n01.02.2025\nSumme 1,00\n", "Edeka"),
            ("dm-drogerie markt\n01.02.2025\nSumme 1,00\n", "dm"),
            ("ROSSMANN\n01.02.2025\nSumme 1,00\n", "Rossmann"),
        ] {
            let e = parse_ebon_text(txt).unwrap();
            assert_eq!(e.merchant, name);
            assert_eq!(e.date, "2025-02-01");
        }
    }

    #[test]
    fn text_errors() {
        assert_eq!(parse_ebon_text("Hallo"), Err(EbonError::UnknownMerchant));
        assert_eq!(
            parse_ebon_text("REWE\nSumme 1,00"),
            Err(EbonError::MissingDate)
        );
        assert_eq!(
            parse_ebon_text("REWE\n01.01.2025"),
            Err(EbonError::MissingTotal)
        );
    }

    const XML: &str = r#"<?xml version="1.0"?>
<rsm:CrossIndustryInvoice xmlns:rsm="urn:x" xmlns:ram="urn:y" xmlns:udt="urn:z">
 <rsm:ExchangedDocument><ram:ID>1</ram:ID>
  <ram:IssueDateTime><udt:DateTimeString format="102">20250314</udt:DateTimeString></ram:IssueDateTime>
 </rsm:ExchangedDocument>
 <rsm:SupplyChainTradeTransaction>
  <ram:IncludedSupplyChainTradeLineItem>
   <ram:SpecifiedTradeProduct><ram:Name>Mineralwasser &amp; Co</ram:Name></ram:SpecifiedTradeProduct>
   <ram:SpecifiedLineTradeAgreement/>
   <ram:SpecifiedLineTradeDelivery><ram:BilledQuantity unitCode="C62">6.000</ram:BilledQuantity></ram:SpecifiedLineTradeDelivery>
   <ram:SpecifiedLineTradeSettlement>
    <ram:ApplicableTradeTax><ram:RateApplicablePercent>19.00</ram:RateApplicablePercent></ram:ApplicableTradeTax>
    <ram:SpecifiedTradeSettlementLineMonetarySummation><ram:LineTotalAmount>5.94</ram:LineTotalAmount></ram:SpecifiedTradeSettlementLineMonetarySummation>
   </ram:SpecifiedLineTradeSettlement>
  </ram:IncludedSupplyChainTradeLineItem>
  <ram:IncludedSupplyChainTradeLineItem>
   <ram:SpecifiedTradeProduct><ram:Name>Pfand Kasten</ram:Name></ram:SpecifiedTradeProduct>
   <ram:SpecifiedLineTradeSettlement>
    <ram:ApplicableTradeTax><ram:RateApplicablePercent>0</ram:RateApplicablePercent></ram:ApplicableTradeTax>
    <ram:SpecifiedTradeSettlementLineMonetarySummation><ram:LineTotalAmount>1.50</ram:LineTotalAmount></ram:SpecifiedTradeSettlementLineMonetarySummation>
   </ram:SpecifiedLineTradeSettlement>
  </ram:IncludedSupplyChainTradeLineItem>
  <ram:ApplicableHeaderTradeAgreement>
   <ram:SellerTradeParty><ram:Name>Rossmann GmbH</ram:Name></ram:SellerTradeParty>
  </ram:ApplicableHeaderTradeAgreement>
  <ram:ApplicableHeaderTradeSettlement>
   <ram:SpecifiedTradeSettlementHeaderMonetarySummation>
    <ram:GrandTotalAmount>7.44</ram:GrandTotalAmount>
   </ram:SpecifiedTradeSettlementHeaderMonetarySummation>
  </ram:ApplicableHeaderTradeSettlement>
 </rsm:SupplyChainTradeTransaction>
</rsm:CrossIndustryInvoice>"#;

    #[test]
    fn parses_zugferd() {
        let e = parse_document(XML).unwrap();
        assert_eq!(e.merchant, "Rossmann");
        assert_eq!(e.date, "2025-03-14");
        assert_eq!(e.total_cents, 744);
        assert_eq!(e.items.len(), 2);
        assert_eq!(e.items[0].name, "Mineralwasser & Co");
        assert_eq!(e.items[0].quantity_milli, 6000);
        assert_eq!(e.items[0].vat_rate_bp, Some(1900));
        assert_eq!(e.deposit_cents, 150);
        assert_eq!(parse_zugferd_xml("<a/>"), Err(EbonError::InvalidXml));
    }

    fn booking(id: &str, date: &str, amount: i64) -> BookingCandidate {
        BookingCandidate {
            id: id.into(),
            date: date.into(),
            amount_cents: amount,
        }
    }

    #[test]
    fn matches_bookings() {
        let e = parse_ebon_text(REWE).unwrap();
        let bookings = [
            booking("far", "2025-03-18", -617),
            booking("wrong-amount", "2025-03-14", -618),
            booking("positive", "2025-03-14", 617),
            booking("d2", "2025-03-16", -617),
            booking("d1", "2025-03-13", -617),
            booking("d0", "2025-03-14", -617),
        ];
        let m = match_bookings(&e, &bookings);
        let ids: Vec<_> = m.iter().map(|m| m.booking_id.as_str()).collect();
        assert_eq!(ids, ["d0", "d1", "d2"]);
        assert_eq!(m[2].day_distance, 2);
    }

    #[test]
    fn day_arithmetic_across_month_and_year() {
        let e = Ebon {
            merchant: "REWE".into(),
            date: "2024-12-31".into(),
            total_cents: 100,
            items: vec![],
            deposit_cents: 0,
        };
        let m = match_bookings(
            &e,
            &[
                booking("a", "2025-01-02", -100),
                booking("b", "2025-01-03", -100),
            ],
        );
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].booking_id, "a");
        assert_eq!(days_from_iso("1970-01-01"), Some(0));
        assert_eq!(
            days_from_iso("2024-03-01").unwrap() - days_from_iso("2024-02-28").unwrap(),
            2
        );
    }

    #[test]
    fn splits_are_balanced() {
        let e = parse_ebon_text(REWE).unwrap();
        let p = propose_splits(&e);
        assert_eq!(p.residual_cents, 0);
        assert_eq!(p.splits.iter().map(|s| s.cents).sum::<i64>(), e.total_cents);
        let get = |c: &str| p.splits.iter().find(|s| s.category == c).map(|s| s.cents);
        assert_eq!(get("Lebensmittel"), Some(307));
        assert_eq!(get("Drogerie"), Some(295));
        assert_eq!(get(CATEGORY_DEPOSIT), Some(15));
    }

    #[test]
    fn residual_goes_to_largest_non_deposit_split() {
        let mut e = parse_ebon_text(REWE).unwrap();
        e.total_cents = 600; // e.g. a discount not listed as a line
        let p = propose_splits(&e);
        assert_eq!(p.residual_cents, -17);
        assert_eq!(p.splits.iter().map(|s| s.cents).sum::<i64>(), 600);
        assert_eq!(
            p.splits
                .iter()
                .find(|s| s.category == "Lebensmittel")
                .unwrap()
                .cents,
            307 - 17
        );
    }

    #[test]
    fn no_items_gives_single_split() {
        let e = Ebon {
            merchant: "dm".into(),
            date: "2025-01-01".into(),
            total_cents: 500,
            items: vec![],
            deposit_cents: 0,
        };
        let p = propose_splits(&e);
        assert_eq!(
            p.splits,
            vec![SplitProposal {
                category: CATEGORY_OTHER.into(),
                cents: 500
            }]
        );
    }

    #[test]
    fn custom_classifier() {
        let e = parse_ebon_text(REWE).unwrap();
        let p = propose_splits_with(&e, &|_| "Haushalt".to_string());
        assert_eq!(p.splits.len(), 2); // Haushalt + Pfand
        assert_eq!(p.splits.iter().map(|s| s.cents).sum::<i64>(), 617);
    }
}
