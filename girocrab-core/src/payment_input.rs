//! Payment data from outside the app (#663): free text from the clipboard or a
//! PDF text layer, EPC QR payloads ("GiroCode", EPC069-12) and the QR code for
//! a SEPA credit transfer that a bank app scans ("Fotoüberweisung").
//!
//! Every front end parses and renders through here, so they behave the same.
//! Amounts are cents; the platforms format them.

use regex::Regex;
use std::sync::LazyLock;

use crate::connectors::fints::sepa::{format_amount, validate_bic};
use crate::iban::{mod97, normalize_iban, validate_iban};
use crate::money::parse_cents;

#[cfg(test)]
mod tests;

/// Payment fields found in a text, QR payload or invoice; `None` = not found.
#[derive(Debug, Clone, Default, PartialEq, Eq, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PaymentDetails {
    pub name: Option<String>,
    pub iban: Option<String>,
    pub bic: Option<String>,
    pub amount_cents: Option<i64>,
    pub purpose: Option<String>,
}

/// QR modules including the quiet zone, row-major (`width` × `width`),
/// `true` = dark: draw one cell per module.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct GiroCodeMatrix {
    pub width: u32,
    pub modules: Vec<bool>,
}

/// Light modules around the QR code (ISO/IEC 18004).
const GIROCODE_QUIET_ZONE: usize = 4;

/// Max. lengths from EPC069-12 (characters).
pub const GIROCODE_MAX_NAME: usize = 70;
pub const GIROCODE_MAX_REMITTANCE: usize = 140;
/// Max. amount: EUR 999,999,999.99.
pub const GIROCODE_MAX_CENTS: i64 = 99_999_999_999;
/// EPC069-12: the whole payload is at most 331 bytes (UTF-8).
pub const GIROCODE_MAX_BYTES: usize = 331;

// MARK: free text (transfer from clipboard)

const IBAN_PATTERN: &str =
    r"\b[A-Za-z]{2}\d{2}(?:[ \t]?[A-Za-z0-9]{4}){2,7}(?:[ \t]?[A-Za-z0-9]{1,3})?\b";

static IBAN: LazyLock<Regex> = LazyLock::new(|| Regex::new(IBAN_PATTERN).unwrap());
// "EUR" as a whole word or "€", the number on the same line.
static CURRENCY_BEFORE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:\bEUR\b|€)[ \t]*(\d[\d.,]*\d)").unwrap());
static CURRENCY_AFTER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(\d[\d.,]*\d)[ \t]*(?:\bEUR\b|€)").unwrap());
static AMOUNT_LABEL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)betrag|summe|total|amount|zu zahlen|gesamt").unwrap());
// The keyword as a whole word, the number on the same line; [`invoice_number`]
// also wants a digit in it.
static INVOICE_NUMBER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(?:Rechnung(?:s)?(?:nummer|-?nr)?|Invoice(?:[ \t]+No)?|RE)\b\.?[ \t]*[:#]?[ \t]*([A-Z0-9][A-Z0-9\-/\.]{2,})",
    )
    .unwrap()
});
/// A decimal digit (Unicode Nd), not "²" or "½".
static DIGIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d$").unwrap());
/// Decimal amounts "1.234,56", "1,234.56" and "12.50", tried in this order at
/// each start; [`amount_in`] checks the neighbours (the `regex` crate has no
/// look-around).
static AMOUNT_ALTERNATIVES: LazyLock<[Regex; 3]> = LazyLock::new(|| {
    [
        r"^\d{1,3}(?:\.\d{3})+,\d{2}",
        r"^\d{1,3}(?:,\d{3})+\.\d{2}",
        r"^\d+[.,]\d{2}",
    ]
    .map(|p| Regex::new(p).unwrap())
});

const BIC_LABELS: &[&str] = &["BIC", "SWIFT"];
const NAME_LABELS: &[&str] = &[
    "Empfänger",
    "Zahlungsempfänger",
    "Kontoinhaber",
    "Begünstigter",
    "Name",
    "Beneficiary",
    "Recipient",
    "Payee",
];
const PURPOSE_LABELS: &[&str] = &[
    "Verwendungszweck",
    "Zahlungsreferenz",
    "Rechnungsnummer",
    "Rechnung Nr.",
    "Rechnungs-Nr.",
    "Referenz",
    "Reference",
    "Betreff",
    "Purpose",
];

fn label_regexes(labels: &[&str]) -> Vec<Regex> {
    labels
        .iter()
        .map(|l| Regex::new(&format!(r"^(?i){}\s*[:=]\s*(.+)$", regex::escape(l))).unwrap())
        .collect()
}

static BIC_LABEL_RX: LazyLock<Vec<Regex>> = LazyLock::new(|| label_regexes(BIC_LABELS));
static NAME_LABEL_RX: LazyLock<Vec<Regex>> = LazyLock::new(|| label_regexes(NAME_LABELS));
static PURPOSE_LABEL_RX: LazyLock<Vec<Regex>> = LazyLock::new(|| label_regexes(PURPOSE_LABELS));

/// Unicode line breaks (LF, VT, FF, CR, NEL, LS, PS).
fn is_newline(c: char) -> bool {
    matches!(
        c,
        '\n' | '\u{0B}' | '\u{0C}' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}'
    )
}

/// Whitespace other than line breaks (spaces, tab).
fn is_space(c: char) -> bool {
    c.is_whitespace() && !is_newline(c)
}

/// Payment fields in free text (clipboard, PDF text layer): IBAN with a valid
/// checksum, labelled BIC, amount, name and purpose.
pub fn parse_payment_text(text: String) -> PaymentDetails {
    let lines: Vec<&str> = text
        .split(is_newline)
        .map(|l| l.trim_matches(is_space))
        .filter(|l| !l.is_empty())
        .collect();
    // Lines without a valid IBAN (name and amount fallbacks skip IBAN lines).
    let plain: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| valid_iban_in(l).is_none())
        .collect();
    let mut p = PaymentDetails {
        // IBAN: first candidate with a valid checksum.
        iban: valid_iban_in(&text),
        ..Default::default()
    };
    // BIC (labelled only, to avoid matching random words).
    if let Some(v) = labelled(&lines, &BIC_LABEL_RX).filter(|v| validate_bic(v)) {
        p.bic = Some(v.split_whitespace().collect::<String>().to_uppercase());
    }
    // Amount: prefer labelled / currency-marked numbers.
    p.amount_cents = amount(&text, &lines, &plain);
    p.name = labelled(&lines, &NAME_LABEL_RX);
    p.purpose = labelled(&lines, &PURPOSE_LABEL_RX);
    if p.name.is_none() {
        // First line that is neither IBAN, amount, nor a "label: value" pair.
        p.name = plain
            .iter()
            .find(|l| {
                !l.contains(':')
                    && amount_in(l).is_none()
                    && l.chars().any(char::is_alphabetic)
                    && l.chars().count() <= 70
            })
            .map(|l| l.to_string());
    }
    if p.purpose.is_none()
        && let Some(rn) = invoice_number(&text)
    {
        p.purpose = Some(format!("Rechnung {rn}"));
    }
    p
}

/// The first invoice number after "Rechnung", "Rechnungsnummer", "Invoice No."
/// or "RE" that contains a digit.
fn invoice_number(text: &str) -> Option<&str> {
    INVOICE_NUMBER
        .captures_iter(text)
        .filter_map(|c| c.get(1))
        .map(|m| m.as_str())
        .find(|n| n.chars().any(|c| c.is_ascii_digit()))
}

fn labelled(lines: &[&str], labels: &[Regex]) -> Option<String> {
    lines.iter().find_map(|l| {
        labels.iter().find_map(|rx| {
            first_match(rx, l)
                .map(|v| v.trim_matches(is_space).to_string())
                .filter(|v| !v.is_empty())
        })
    })
}

/// Capture group 1 of the first match.
fn first_match<'a>(rx: &Regex, s: &'a str) -> Option<&'a str> {
    rx.captures(s).and_then(|c| c.get(1)).map(|m| m.as_str())
}

/// First decimal amount ("1.234,56", "1,234.56", "12.50") with no digit, dot or
/// comma right before it and neither a digit nor '.'/',' plus digit right after
/// it ("12.03.2026" is no amount).
fn amount_in(s: &str) -> Option<&str> {
    let glued = |c: char| is_digit(c) || c == '.' || c == ',';
    let mut prev: Option<char> = None;
    for (i, c) in s.char_indices() {
        let before = prev;
        prev = Some(c);
        if !is_digit(c) || before.is_some_and(glued) {
            continue;
        }
        let rest = &s[i..];
        for rx in AMOUNT_ALTERNATIVES.iter() {
            if let Some(m) = rx.find(rest) {
                let mut after = rest[m.end()..].chars();
                let next = after.next();
                let glued_after = next.is_some_and(is_digit)
                    || matches!(next, Some('.' | ',')) && after.next().is_some_and(is_digit);
                if !glued_after {
                    return Some(m.as_str());
                }
            }
        }
    }
    None
}

fn is_digit(c: char) -> bool {
    c.is_ascii_digit() || (!c.is_ascii() && DIGIT.is_match(c.encode_utf8(&mut [0; 4])))
}

fn valid_iban_in(s: &str) -> Option<String> {
    IBAN.find_iter(s).find_map(|m| {
        let cand = normalize_iban(m.as_str());
        validate_iban(&cand).then_some(cand)
    })
}

fn amount(text: &str, lines: &[&str], plain: &[&str]) -> Option<i64> {
    // 1. Currency-marked ("123,45 €", "EUR 123.45").
    if let Some(v) =
        first_match(&CURRENCY_BEFORE, text).or_else(|| first_match(&CURRENCY_AFTER, text))
    {
        return normalize_amount(v);
    }
    // 2. Labelled.
    for l in lines {
        if AMOUNT_LABEL.is_match(l)
            && let Some(v) = amount_in(l)
        {
            return normalize_amount(v);
        }
    }
    // 3. Any decimal number outside IBAN lines.
    plain
        .iter()
        .find_map(|l| amount_in(l))
        .and_then(normalize_amount)
}

/// "1.234,56" / "1,234.56" / "1234.56" → cents, separators as in
/// [`parse_cents`]. Positive amounts that start with a number only.
fn normalize_amount(raw: &str) -> Option<i64> {
    let s = raw.trim_matches(is_space);
    if !s.starts_with(|c: char| c.is_ascii_digit() || matches!(c, '.' | ',' | '+')) {
        return None;
    }
    parse_cents(s).ok().filter(|&c| c > 0)
}

/// [`normalize_amount`] for the platforms (ZUGFeRD amounts): cents, or `None`
/// when the text is no positive amount.
pub fn parse_amount_loose(text: String) -> Option<i64> {
    normalize_amount(&text)
}

// MARK: EPC QR (GiroCode)

/// Fields of an EPC069-12 payload, `None` when it does not start with "BCD"
/// or has fewer than 7 lines (version and identification are not checked).
/// The purpose is the remittance text, else the structured creditor reference.
pub fn parse_epc_payload(payload: String) -> Option<PaymentDetails> {
    // EPC069-12 separates with LF or CR LF; a CR LF is one line break.
    let l: Vec<&str> = payload
        .split("\r\n")
        .flat_map(|s| s.split(is_newline))
        .map(|s| s.trim_matches(is_space))
        .collect();
    if l.len() < 7 || l[0] != "BCD" {
        return None;
    }
    let field = |i: usize| l.get(i).copied().unwrap_or("");
    let some = |s: &str| (!s.is_empty()).then(|| s.to_string());
    let iban = normalize_iban(l[6]);
    let amount = field(7);
    let amount_cents = amount
        .get(..3)
        .filter(|cur| cur.eq_ignore_ascii_case("EUR"))
        .and_then(|_| normalize_amount(&amount[3..]));
    let (reference, remittance) = (field(9), field(10));
    Some(PaymentDetails {
        name: some(l[5]),
        iban: some(&iban),
        bic: some(l[4]),
        amount_cents,
        purpose: some(if remittance.is_empty() {
            reference
        } else {
            remittance
        }),
    })
}

/// The QR payload of a SEPA credit transfer (EPC069-12 v002, UTF-8), or `None`
/// when IBAN, name or amount are unusable. BIC is optional.
pub fn girocode_payload(
    name: String,
    iban: String,
    bic: String,
    amount_cents: i64,
    purpose: String,
) -> Option<String> {
    let alnum = |s: &str| -> String {
        s.to_uppercase()
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect()
    };
    let (iban, bic) = (alnum(&iban), alnum(&bic));
    let name: String = one_line(&name).chars().take(GIROCODE_MAX_NAME).collect();
    if !(15..=34).contains(&iban.len())
        || name.is_empty()
        || !(1..=GIROCODE_MAX_CENTS).contains(&amount_cents)
    {
        return None;
    }
    let amount = format!("EUR{}", format_amount(amount_cents));
    let purpose = one_line(&purpose);
    // EPC069-12: an ISO 11649 creditor reference ("RF18 5390 0754 7034") goes
    // into the structured reference field, with the remittance text empty.
    let (reference, remittance) = match rf_reference(&purpose) {
        Some(rf) => (rf, String::new()),
        None => (
            String::new(),
            purpose.chars().take(GIROCODE_MAX_REMITTANCE).collect(),
        ),
    };
    let build = |name: &str, remittance: &str| {
        let mut fields = vec![
            "BCD", // service tag
            "002", // version (BIC optional within the EEA)
            "1",   // character set: UTF-8
            "SCT", // SEPA credit transfer
            &bic, name, &iban, &amount, "", // purpose code
            &reference, remittance,
        ];
        // EPC069-12: no trailing empty fields.
        while fields.last().is_some_and(|f| f.is_empty()) {
            fields.pop();
        }
        fields.join("\n")
    };
    // EPC069-12: at most 331 bytes (UTF-8). The other fields are fixed, so the
    // excess comes off the end of the remittance and, when that is not enough,
    // of the name, at a char boundary (an emptied remittance also drops its
    // separator, which only frees more room).
    let over = |p: &str| p.len().saturating_sub(GIROCODE_MAX_BYTES);
    let cut = |s: &str, excess: usize| {
        s[..s.floor_char_boundary(s.len().saturating_sub(excess))].to_string()
    };
    let payload = build(&name, &remittance);
    if over(&payload) == 0 {
        return Some(payload);
    }
    let remittance = cut(&remittance, over(&payload));
    let payload = build(&name, &remittance);
    Some(match over(&payload) {
        0 => payload,
        excess => build(&cut(&name, excess), &remittance),
    })
}

fn one_line(s: &str) -> String {
    s.split(is_newline)
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(is_space)
        .to_string()
}

/// `s` as an ISO 11649 creditor reference without spaces, uppercased: "RF",
/// two check digits and 1–21 alphanumerics with mod 97 == 1, written as one
/// token or in groups of four ("RF18 5390 0754 7034"). Anything else is free text.
fn rf_reference(s: &str) -> Option<String> {
    let groups: Vec<&str> = s.trim().split(' ').collect();
    let (last, full) = groups.split_last()?;
    if groups.len() > 1 && (full.iter().any(|g| g.len() != 4) || !(1..=4).contains(&last.len())) {
        return None;
    }
    let r = groups.concat().to_ascii_uppercase();
    let b = r.as_bytes();
    if !(5..=25).contains(&b.len())
        || !r.starts_with("RF")
        || !b[2..4].iter().all(u8::is_ascii_digit)
        || !b[4..].iter().all(u8::is_ascii_alphanumeric)
    {
        return None;
    }
    (mod97(r[4..].chars().chain(r[..4].chars())) == 1).then_some(r)
}

/// QR modules for `payload` with error correction "M" (EPC069-12) and a
/// 4-module quiet zone, `None` when it does not fit into a QR code.
pub fn girocode_matrix(payload: String) -> Option<GiroCodeMatrix> {
    use qrcode::{Color, EcLevel, QrCode};
    let code = QrCode::with_error_correction_level(payload.as_bytes(), EcLevel::M).ok()?;
    let (inner, q) = (code.width(), GIROCODE_QUIET_ZONE);
    let width = inner + 2 * q;
    let mut modules = vec![false; width * width];
    for (i, c) in code.to_colors().into_iter().enumerate() {
        modules[(i / inner + q) * width + i % inner + q] = c == Color::Dark;
    }
    Some(GiroCodeMatrix {
        width: width as u32,
        modules,
    })
}
