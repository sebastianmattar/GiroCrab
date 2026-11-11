//! Keyless import for card issuers without a usable public API
//! (American Express, Amazon Visa / Open Bank):
//!
//! * **Web portal sync** – the app logs in inside an embedded WKWebView,
//!   captures the portal's authenticated JSON responses and/or the visible
//!   transaction table and hands both to [`Vault::import_portal_capture`].
//! * **Statement import** – official CSV/PDF statements via
//!   [`Vault::import_statement`].
//!
//! The portals' JSON schemas are undocumented and change, so parsing is
//! schema-agnostic: any array of objects carrying a date, an amount and a
//! description is treated as a transaction list.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;

use super::{store_fetch, FetchResult, RemoteAccount, RemoteTransaction, SyncSummary};
use crate::error::CoreError;
use crate::i18n::{tr, trf};
use crate::models::{AccountKind, ConnectorKind};
use crate::vault::Vault;

#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    pub date: String,
    pub text: String,
    pub cents: i64,
    pub id: Option<String>,
}

/// Amex reports charges as positive numbers; Open Bank uses signed amounts.
fn charges_positive(kind: ConnectorKind) -> bool {
    kind == ConnectorKind::AmericanExpress
}

// ------------------------------------------------------------------ primitives

/// "1.234,56", "1,234.56", "-12,30", "12,30-", "€ 5", "(5,00)" -> cents.
pub fn parse_amount(s: &str) -> Option<i64> {
    let mut t: String = s
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '€' && *c != '\u{a0}')
        .collect();
    t = t.replace("EUR", "").replace("eur", "");
    let mut neg = false;
    if t.starts_with('(') && t.ends_with(')') {
        neg = true;
        t = t[1..t.len() - 1].to_string();
    }
    if t.ends_with('-') {
        neg = true;
        t.pop();
    }
    if t.ends_with('+') {
        t.pop();
    }
    if let Some(r) = t.strip_prefix('-') {
        neg = !neg;
        t = r.to_string();
    }
    if let Some(r) = t.strip_prefix('+') {
        t = r.to_string();
    }
    if t.is_empty()
        || !t
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == ',')
        || !t.chars().any(|c| c.is_ascii_digit())
    {
        return None;
    }
    let last_dot = t.rfind('.');
    let last_comma = t.rfind(',');
    let dec_pos = match (last_dot, last_comma) {
        (Some(d), Some(c)) => Some(d.max(c)),
        (Some(p), None) | (None, Some(p)) => {
            let digits_after = t.len() - p - 1;
            let sep_count = t
                .matches(if t.as_bytes()[p] == b'.' { '.' } else { ',' })
                .count();
            if sep_count == 1 && digits_after != 3 {
                Some(p)
            } else if sep_count == 1 && digits_after == 3 && t.as_bytes()[p] == b',' && p <= 1 {
                Some(p)
            } else {
                None
            }
        }
        _ => None,
    };
    let (int_part, frac_part) = match dec_pos {
        Some(p) => (&t[..p], &t[p + 1..]),
        None => (&t[..], ""),
    };
    let int_digits: String = int_part.chars().filter(|c| c.is_ascii_digit()).collect();
    let mut frac = frac_part.to_string();
    if !frac.chars().all(|c| c.is_ascii_digit())
        || frac.len() > 2 && frac[2..].chars().any(|c| c != '0')
    {
        return None;
    }
    frac.truncate(2);
    while frac.len() < 2 {
        frac.push('0');
    }
    let int: i64 = if int_digits.is_empty() {
        0
    } else {
        int_digits.parse().ok()?
    };
    let cents = int
        .checked_mul(100)?
        .checked_add(frac.parse::<i64>().ok()?)?;
    Some(if neg { -cents } else { cents })
}

fn month_num(m: &str) -> Option<u32> {
    let m = m.trim_end_matches('.').to_lowercase();
    let m = m.replace('ä', "a");
    const NAMES: [&str; 12] = [
        "jan", "feb", "mar", "apr", "ma", "jun", "jul", "aug", "sep", "okt", "nov", "dez",
    ];
    // "mai"/"may" both start with "ma"; disambiguate from "mar"
    if m.starts_with("mar") {
        return Some(3);
    }
    if m.starts_with("mai") || m.starts_with("may") {
        return Some(5);
    }
    if m.starts_with("oct") {
        return Some(10);
    }
    if m.starts_with("dec") {
        return Some(12);
    }
    NAMES
        .iter()
        .position(|n| *n != "ma" && m.starts_with(n))
        .map(|i| i as u32 + 1)
}

/// Many date spellings -> "YYYY-MM-DD".
pub fn parse_date(s: &str) -> Option<String> {
    let s = s.trim();
    let ymd = |y: i32, m: u32, d: u32| {
        chrono::NaiveDate::from_ymd_opt(y, m, d).map(|x| x.format("%Y-%m-%d").to_string())
    };
    let year = |y: &str| -> Option<i32> {
        let n: i32 = y.parse().ok()?;
        Some(if y.len() == 2 { 2000 + n } else { n })
    };
    let re_iso = regex::Regex::new(r"^(\d{4})[-/.](\d{1,2})[-/.](\d{1,2})").unwrap();
    if let Some(c) = re_iso.captures(s) {
        return ymd(c[1].parse().ok()?, c[2].parse().ok()?, c[3].parse().ok()?);
    }
    let re_dmy = regex::Regex::new(r"^(\d{1,2})[./](\d{1,2})[./](\d{4}|\d{2})\b").unwrap();
    if let Some(c) = re_dmy.captures(s) {
        return ymd(year(&c[3])?, c[2].parse().ok()?, c[1].parse().ok()?);
    }
    let re_text = regex::Regex::new(r"(?i)^(\d{1,2})\.?\s+([A-Za-zäÄ]{3,9}\.?)\s+(\d{4})").unwrap();
    if let Some(c) = re_text.captures(s) {
        return ymd(c[3].parse().ok()?, month_num(&c[2])?, c[1].parse().ok()?);
    }
    let re_us = regex::Regex::new(r"(?i)^([A-Za-z]{3,9})\.?\s+(\d{1,2}),?\s+(\d{4})").unwrap();
    if let Some(c) = re_us.captures(s) {
        return ymd(c[3].parse().ok()?, month_num(&c[1])?, c[2].parse().ok()?);
    }
    None
}

// ------------------------------------------------------------------ JSON

const DATE_KEYS: &[&str] = &[
    "charge_date",
    "chargedate",
    "booking_date",
    "bookingdate",
    "booked_date",
    "transaction_date",
    "transactiondate",
    "posting_date",
    "postingdate",
    "post_date",
    "postdate",
    "value_date",
    "valuedate",
    "operationdate",
    "operation_date",
    "date",
    "datum",
    "bookingdatetime",
    "timestamp",
];
const TEXT_KEYS: &[&str] = &[
    "description",
    "merchant",
    "merchantname",
    "merchant_name",
    "counterparty",
    "counterpartyname",
    "title",
    "name",
    "text",
    "beschreibung",
    "details",
    "remittanceinformation",
    "purpose",
    "concept",
    "extended_details",
    "extendeddetails",
    "reference",
];
const AMOUNT_KEYS: &[&str] = &[
    "amount",
    "amountvalue",
    "transactionamount",
    "transaction_amount",
    "betrag",
    "value",
    "billingamount",
    "billing_amount",
    "localamount",
];
const ID_KEYS: &[&str] = &[
    "identifier",
    "transactionid",
    "transaction_id",
    "id",
    "entryreference",
    "entry_reference",
    "referencenumber",
    "reference_number",
    "operationid",
    "movementid",
];
const INDICATOR_KEYS: &[&str] = &[
    "debit_credit_indicator",
    "credit_debit_indicator",
    "creditdebitindicator",
    "debitcreditindicator",
    "type",
    "transactiontype",
];

fn get_ci<'a>(obj: &'a serde_json::Map<String, Value>, keys: &[&str]) -> Option<&'a Value> {
    for k in keys {
        if let Some((_, v)) = obj.iter().find(|(name, _)| name.to_lowercase() == *k) {
            if !v.is_null() {
                return Some(v);
            }
        }
    }
    None
}

fn value_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Array(a) => {
            let parts: Vec<String> = a.iter().filter_map(value_text).collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join(" "))
            }
        }
        Value::Object(o) => get_ci(o, TEXT_KEYS).and_then(value_text),
        _ => None,
    }
}

fn value_cents(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => {
            let f = n.as_f64()?;
            Some((f * 100.0).round() as i64)
        }
        Value::String(s) => parse_amount(s),
        Value::Object(o) => get_ci(
            o,
            &[
                "amount",
                "value",
                "raw_amount",
                "rawamount",
                "localizedvalue",
                "raw",
            ],
        )
        .and_then(value_cents),
        _ => None,
    }
}

fn is_credit_indicator(obj: &serde_json::Map<String, Value>) -> Option<bool> {
    let v = get_ci(obj, INDICATOR_KEYS)?.as_str()?.to_uppercase();
    match v.as_str() {
        "CRDT" | "CREDIT" | "CR" | "PAYMENT" | "REFUND" | "CREDITS" => Some(true),
        "DBIT" | "DEBIT" | "DR" | "CHARGE" | "PURCHASE" | "DEBITS" => Some(false),
        _ => None,
    }
}

fn parse_tx_object(obj: &serde_json::Map<String, Value>, kind: ConnectorKind) -> Option<Parsed> {
    let date = get_ci(obj, DATE_KEYS)
        .and_then(value_text)
        .and_then(|d| parse_date(&d))?;
    let raw = get_ci(obj, AMOUNT_KEYS).and_then(value_cents)?;
    let text = get_ci(obj, TEXT_KEYS)
        .and_then(value_text)
        .unwrap_or_default();
    let cents = match is_credit_indicator(obj) {
        Some(true) => raw.abs(),
        Some(false) => -raw.abs(),
        None if charges_positive(kind) => -raw,
        None => raw,
    };
    let id = get_ci(obj, ID_KEYS).and_then(value_text);
    Some(Parsed {
        date,
        text,
        cents,
        id,
    })
}

fn walk(
    v: &Value,
    kind: ConnectorKind,
    pending: bool,
    out: &mut Vec<Parsed>,
    out_pending: &mut Vec<Parsed>,
) {
    match v {
        Value::Array(items) => {
            let parsed: Vec<Parsed> = items
                .iter()
                .filter_map(|i| i.as_object().and_then(|o| parse_tx_object(o, kind)))
                .collect();
            if !parsed.is_empty() {
                if pending {
                    out_pending.extend(parsed)
                } else {
                    out.extend(parsed)
                }
            } else {
                items
                    .iter()
                    .for_each(|i| walk(i, kind, pending, out, out_pending));
            }
        }
        Value::Object(o) => {
            for (k, val) in o {
                walk(
                    val,
                    kind,
                    pending || k.to_lowercase().contains("pending"),
                    out,
                    out_pending,
                );
            }
        }
        _ => {}
    }
}

/// Preferred balance fields (normalised: lowercase, no `_`/`-`), strict priority order.
/// Live total debt first; statement/billing-cycle amounts only as a last resort.
const BALANCE_KEYS: &[&str] = &[
    "statementbalanceamount",
    "currentbalance",
    "totalbalance",
    "outstandingbalance",
    "balanceamount",
    "accountbalance",
    "laststatementbalanceamount",
];

fn norm_key(k: &str) -> String {
    k.chars()
        .filter(|c| *c != '_' && *c != '-')
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn find_key<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::Object(o) => {
            if let Some((_, x)) = o
                .iter()
                .find(|(k, x)| norm_key(k) == key && value_cents(x).is_some_and(|c| c != 0))
            {
                return Some(x);
            }
            o.values().find_map(|x| find_key(x, key))
        }
        Value::Array(a) => a.iter().find_map(|x| find_key(x, key)),
        _ => None,
    }
}

/// Finds a card balance (owed amount, stored negative) in a JSON document.
/// Returns the matched key and the balance in cents.
fn find_balance(v: &Value, kind: ConnectorKind) -> Option<(String, i64)> {
    let owed = |c: i64| {
        if charges_positive(kind) || c > 0 {
            -c.abs()
        } else {
            c
        }
    };
    for key in BALANCE_KEYS {
        if let Some(c) = find_key(v, key).and_then(value_cents) {
            return Some((key.to_string(), owed(c)));
        }
    }
    fn generic(v: &Value) -> Option<(String, i64)> {
        match v {
            Value::Object(o) => {
                for (k, val) in o {
                    let kl = norm_key(k);
                    if kl.contains("balance")
                        && ![
                            "available",
                            "minimum",
                            "statement",
                            "previous",
                            "last",
                            "payment",
                            "due",
                            "debit",
                            "charge",
                        ]
                        .iter()
                        .any(|x| kl.contains(x))
                    {
                        if let Some(c) = value_cents(val).filter(|c| *c != 0) {
                            return Some((k.clone(), c));
                        }
                    }
                }
                o.values().find_map(generic)
            }
            Value::Array(a) => a.iter().find_map(generic),
            _ => None,
        }
    }
    generic(v).map(|(k, c)| (k, owed(c)))
}

/// Diagnostic: names of keys with a non-zero numeric value that mention
/// balance/amount. Key names only – the debug log never carries amounts.
fn candidate_keys(v: &Value, out: &mut std::collections::BTreeSet<String>) {
    match v {
        Value::Object(o) => {
            for (k, val) in o {
                let kl = k.to_lowercase();
                if (kl.contains("balance") || kl.contains("amount"))
                    && val.as_f64().is_some_and(|f| f != 0.0)
                {
                    out.insert(k.clone());
                }
                candidate_keys(val, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| candidate_keys(x, out)),
        _ => {}
    }
}

pub fn parse_json_bodies(bodies: &[String], kind: ConnectorKind) -> (Vec<Parsed>, Option<i64>) {
    let (txs, bal) = parse_json_bodies_keyed(bodies, kind);
    (txs, bal.map(|(_, c)| c))
}

fn parse_json_bodies_keyed(
    bodies: &[String],
    kind: ConnectorKind,
) -> (Vec<Parsed>, Option<(String, i64)>) {
    let mut txs = Vec::new();
    let mut pending = Vec::new();
    let mut balance = None;
    for b in bodies {
        let Ok(v) = serde_json::from_str::<Value>(b) else {
            continue;
        };
        walk(&v, kind, false, &mut txs, &mut pending);
        if balance.is_none() {
            balance = find_balance(&v, kind);
        }
    }
    // Pending items change id/amount once posted – only use them when nothing is posted yet.
    if txs.is_empty() {
        txs = pending;
    }
    // The portal re-sends overlapping pages; drop exact repeats of the same source id.
    let mut seen = std::collections::HashSet::new();
    txs.retain(|t| t.id.as_ref().is_none_or(|id| seen.insert(id.clone())));
    (txs, balance)
}

/// Keys-only structure of JSON bodies: every string/number/bool is masked, so the
/// dump reveals field names and types but no personal data.
pub fn schema_of(v: &Value, depth: usize) -> Value {
    match v {
        Value::Null => Value::String("null".into()),
        Value::Bool(_) => Value::String("bool".into()),
        Value::Number(_) => Value::String("number".into()),
        Value::String(_) => Value::String("string".into()),
        _ if depth > 12 => Value::String("…".into()),
        Value::Array(a) => {
            let first = a
                .first()
                .map(|x| schema_of(x, depth + 1))
                .unwrap_or(Value::Null);
            serde_json::json!({ "array_len": a.len(), "item": first })
        }
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, x)| (k.clone(), schema_of(x, depth + 1)))
                .collect(),
        ),
    }
}

// ------------------------------------------------------------------ DOM rows

/// `rows` = visible table rows, each a list of cell texts.
pub fn parse_dom_rows(rows: &[Vec<String>], kind: ConnectorKind) -> Vec<Parsed> {
    let mut out = Vec::new();
    for row in rows {
        let cells: Vec<&str> = row
            .iter()
            .map(|c| c.trim())
            .filter(|c| !c.is_empty())
            .collect();
        let date = cells.iter().find_map(|c| parse_date(c));
        // amount: last cell that parses as a money value with a decimal part
        let amount = cells
            .iter()
            .rev()
            .find(|c| c.contains(',') || c.contains('.'))
            .and_then(|c| parse_amount(c));
        let (Some(date), Some(raw)) = (date, amount) else {
            continue;
        };
        let text = cells
            .iter()
            .filter(|c| parse_date(c).is_none() && parse_amount(c).is_none())
            .max_by_key(|c| c.len())
            .map(|c| c.to_string())
            .unwrap_or_default();
        out.push(Parsed {
            date,
            text,
            cents: if charges_positive(kind) { -raw } else { raw },
            id: None,
        });
    }
    out
}

// ------------------------------------------------------------------ CSV

pub fn split_csv(text: &str, delim: char) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut cell = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    cell.push('"');
                    chars.next();
                } else {
                    quoted = false;
                }
            } else {
                cell.push(c);
            }
        } else if c == '"' {
            quoted = true;
        } else if c == delim {
            row.push(std::mem::take(&mut cell));
        } else if c == '\n' || c == '\r' {
            if c == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            row.push(std::mem::take(&mut cell));
            if row.iter().any(|x| !x.trim().is_empty()) {
                rows.push(std::mem::take(&mut row));
            } else {
                row.clear();
            }
        } else {
            cell.push(c);
        }
    }
    row.push(cell);
    if row.iter().any(|x| !x.trim().is_empty()) {
        rows.push(row);
    }
    rows
}

pub fn parse_csv(text: &str, kind: ConnectorKind) -> Vec<Parsed> {
    let text = text.trim_start_matches('\u{feff}');
    let first = text.lines().next().unwrap_or("");
    let delim = [';', ',', '\t']
        .into_iter()
        .max_by_key(|d| first.matches(*d).count())
        .unwrap_or(',');
    let rows = split_csv(text, delim);
    // header = first row that names a date and an amount column
    let find = |row: &[String], names: &[&str]| {
        row.iter().position(|c| {
            names
                .iter()
                .any(|n| c.trim().to_lowercase().trim_matches('"') == *n)
        })
    };
    let mut header_idx = None;
    for (i, r) in rows.iter().enumerate().take(15) {
        let d = find(
            r,
            &[
                "datum",
                "date",
                "buchungstag",
                "buchungsdatum",
                "belegdatum",
                "transaktionsdatum",
                "transaction date",
            ],
        );
        let a = find(
            r,
            &["betrag", "amount", "umsatz", "betrag (eur)", "amount (eur)"],
        );
        if let (Some(d), Some(a)) = (d, a) {
            header_idx = Some((i, d, a, r.clone()));
            break;
        }
    }
    let Some((hi, di, ai, header)) = header_idx else {
        return vec![];
    };
    let ti = find(
        &header,
        &[
            "beschreibung",
            "description",
            "verwendungszweck",
            "buchungstext",
            "händler",
            "merchant",
            "text",
            "beschreibung/verwendungszweck",
        ],
    );
    let pi = find(
        &header,
        &[
            "erscheint auf ihrer abrechnung als",
            "appears on your statement as",
            "details",
            "zusatzinformationen",
        ],
    );
    let ci = find(&header, &["soll/haben", "debit/credit", "type", "typ"]);
    let mut out = Vec::new();
    for r in rows.iter().skip(hi + 1) {
        let (Some(d), Some(a)) = (
            r.get(di).and_then(|x| parse_date(x)),
            r.get(ai).and_then(|x| parse_amount(x)),
        ) else {
            continue;
        };
        let mut text = ti
            .and_then(|i| r.get(i))
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if let Some(extra) = pi
            .and_then(|i| r.get(i))
            .map(|s| s.trim())
            .filter(|s| !s.is_empty() && !text.contains(*s))
        {
            text = format!("{text} {extra}").trim().to_string();
        }
        let cents = match ci.and_then(|i| r.get(i)).map(|s| s.trim().to_uppercase()) {
            Some(s) if s == "S" || s == "DEBIT" || s == "D" => -a.abs(),
            Some(s) if s == "H" || s == "CREDIT" || s == "C" => a.abs(),
            _ if charges_positive(kind) => -a,
            _ => a,
        };
        out.push(Parsed {
            date: d,
            text,
            cents,
            id: None,
        });
    }
    out
}

// ------------------------------------------------------------------ PDF statements

/// Line-based best effort on extracted statement text:
/// `DD.MM.[YY[YY]]  Beschreibung …  1.234,56[-| CR| H]`.
pub fn parse_statement_text(text: &str, kind: ConnectorKind) -> Vec<Parsed> {
    let re = regex::Regex::new(r"^\s*(\d{1,2}\.\d{1,2}\.(?:\d{4}|\d{2})?)\s+(?:(\d{1,2}\.\d{1,2}\.(?:\d{4}|\d{2})?)\s+)?(.+?)\s+(-?\d{1,3}(?:\.\d{3})*,\d{2}|-?\d+,\d{2})\s*(-|CR|H|S|\+)?\s*$").unwrap();
    // statement year from any full date or "Abrechnung … 2025"
    let year_re = regex::Regex::new(r"\b(\d{1,2})\.(\d{1,2})\.(\d{4})\b").unwrap();
    let default_year: i32 = year_re
        .captures(text)
        .and_then(|c| c[3].parse().ok())
        .unwrap_or_else(|| {
            use chrono::Datelike;
            chrono::Local::now().year()
        });
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(c) = re.captures(line) else { continue };
        let date_s = if c[1].matches('.').count() == 2 && c[1].ends_with('.') {
            format!("{}{}", &c[1], default_year)
        } else {
            c[1].to_string()
        };
        let Some(date) = parse_date(&date_s) else {
            continue;
        };
        let Some(raw) = parse_amount(&c[4]) else {
            continue;
        };
        let suffix = c.get(5).map(|m| m.as_str()).unwrap_or("");
        let credit_marked = matches!(suffix, "-" | "CR" | "H");
        let cents = if credit_marked {
            raw.abs()
        } else if suffix == "S" || suffix == "+" {
            -raw.abs()
        } else if charges_positive(kind) {
            -raw
        } else {
            raw
        };
        out.push(Parsed {
            date,
            text: c[3].trim().to_string(),
            cents,
            id: None,
        });
    }
    out
}

pub fn parse_statement_bytes(
    file_name: &str,
    bytes: &[u8],
    pdf_hint: &str,
    kind: ConnectorKind,
) -> Vec<Parsed> {
    if bytes.starts_with(b"%PDF") || file_name.to_lowercase().ends_with(".pdf") {
        let mut text = pdf_hint.to_string();
        let mut out = parse_statement_text(&text, kind);
        if out.is_empty() {
            text = crate::documents::extract_pdf_text(bytes);
            out = parse_statement_text(&text, kind);
        }
        out
    } else {
        let text = crate::text::decode_bank_bytes(bytes);
        parse_csv(&text, kind)
    }
}

// ------------------------------------------------------------------ store

fn to_fetch(
    connection_id: i64,
    name: &str,
    kind: ConnectorKind,
    parsed: Vec<Parsed>,
    balance: Option<i64>,
) -> FetchResult {
    let account_ref = format!(
        "portal-{}-{}",
        kind.as_str().replace(':', "-"),
        connection_id
    );
    let today = super::today();
    let mut seen: HashMap<(String, i64, String), u32> = HashMap::new();
    let mut transactions = Vec::new();
    let mut ids_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for p in parsed {
        let mut t = RemoteTransaction {
            account_ref: account_ref.clone(),
            external_id: String::new(),
            booking_date: p.date.clone(),
            value_date: p.date.clone(),
            amount_cents: p.cents,
            currency: "EUR".into(),
            counterparty: p.text.clone(),
            counterparty_iban: String::new(),
            purpose: String::new(),
            booking_text: "Kartenumsatz".into(),
            ..Default::default()
        };
        let n = seen.entry((p.date, p.cents, p.text)).or_insert(0);
        t.external_id = match p.id {
            Some(id) if ids_seen.insert(id.clone()) => format!("id:{id}"),
            _ => t.content_id(*n),
        };
        *n += 1;
        transactions.push(t);
    }
    FetchResult {
        accounts: vec![RemoteAccount {
            external_ref: account_ref,
            name: name.to_string(),
            owner: String::new(),
            iban: String::new(),
            account_number: String::new(),
            currency: "EUR".into(),
            kind: AccountKind::CreditCard,
            balance_cents: balance.unwrap_or(0),
            balance_date: today,
        }],
        transactions,
        state: None,
        // no reported balance: accumulate the imported bookings instead
        adjust_balances: balance.is_none(),
        ..Default::default()
    }
}

impl Vault {
    pub(super) fn portal_connection(
        &self,
        connection_id: i64,
    ) -> Result<(ConnectorKind, String), CoreError> {
        let db = self.db()?;
        let (kind, name): (String, String) = db
            .query_row(
                "SELECT connector, display_name FROM bank_connections WHERE id = ?1",
                [connection_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|_| CoreError::NotFound {
                what: trf("Bankverbindung {0}", &[&connection_id]),
            })?;
        let kind = ConnectorKind::from_str(&kind).ok_or_else(|| CoreError::InvalidInput {
            message: format!("unknown connector {kind}"),
        })?;
        Ok((kind, name))
    }

    fn import_parsed(
        &self,
        connection_id: i64,
        parsed: Vec<Parsed>,
        balance: Option<i64>,
    ) -> Result<SyncSummary, CoreError> {
        let (kind, name) = self.portal_connection(connection_id)?;
        if parsed.is_empty() {
            return Err(CoreError::InvalidInput {
                message: tr("Keine Umsätze erkannt.").into(),
            });
        }
        store_fetch(
            self,
            kind,
            connection_id,
            to_fetch(connection_id, &name, kind, parsed, balance),
        )
    }
}

#[uniffi::export]
impl Vault {
    /// Web-portal sync: `json_bodies` are authenticated JSON responses captured in the
    /// embedded browser, `dom_rows_json` is `[[cell, …], …]` of the visible table.
    pub fn import_portal_capture(
        self: Arc<Self>,
        connection_id: i64,
        json_bodies: Vec<String>,
        dom_rows_json: String,
    ) -> Result<SyncSummary, CoreError> {
        let (kind, _) = self.portal_connection(connection_id)?;
        let (mut parsed, keyed) = parse_json_bodies_keyed(&json_bodies, kind);
        let balance = keyed.as_ref().map(|(_, c)| *c);
        match &keyed {
            Some((k, _)) => crate::depot_debug::log(&format!("portal matched balance: key={k}")),
            None => crate::depot_debug::log("portal matched balance: none"),
        }
        let mut keys = std::collections::BTreeSet::new();
        for b in &json_bodies {
            if let Ok(v) = serde_json::from_str::<Value>(b) {
                candidate_keys(&v, &mut keys);
            }
        }
        if !keys.is_empty() {
            crate::depot_debug::log(&format!(
                "portal candidate keys: {}",
                keys.into_iter().collect::<Vec<_>>().join(", ")
            ));
        }
        let from_json = parsed.len();
        if parsed.is_empty() {
            let rows: Vec<Vec<String>> = serde_json::from_str(&dom_rows_json).unwrap_or_default();
            parsed = parse_dom_rows(&rows, kind);
        }
        crate::depot_debug::log(&format!(
            "portal import: {:?} bodies={} json_txs={} total_txs={} balance_found={}",
            kind,
            json_bodies.len(),
            from_json,
            parsed.len(),
            balance.is_some()
        ));
        self.import_parsed(connection_id, parsed, balance)
    }

    /// How many bookings `import_portal_capture` would extract right now (nothing is stored).
    pub fn preview_portal_capture(
        self: Arc<Self>,
        connection_id: i64,
        json_bodies: Vec<String>,
        dom_rows_json: String,
    ) -> Result<u32, CoreError> {
        let (kind, _) = self.portal_connection(connection_id)?;
        let (mut parsed, _) = parse_json_bodies(&json_bodies, kind);
        if parsed.is_empty() {
            let rows: Vec<Vec<String>> = serde_json::from_str(&dom_rows_json).unwrap_or_default();
            parsed = parse_dom_rows(&rows, kind);
        }
        Ok(parsed.len() as u32)
    }

    /// 1-click CSV / PDF statement import (no API keys). `pdf_text_hint` is optional
    /// text extracted by PDFKit; the core falls back to its own extractor.
    pub fn import_statement(
        self: Arc<Self>,
        connection_id: i64,
        file_name: String,
        bytes: Vec<u8>,
        pdf_text_hint: String,
    ) -> Result<SyncSummary, CoreError> {
        let (kind, _) = self.portal_connection(connection_id)?;
        let parsed = parse_statement_bytes(&file_name, &bytes, &pdf_text_hint, kind);
        self.import_parsed(connection_id, parsed, None)
    }
}

/// Sanitized keys-only structure dump of captured portal JSON (for debugging a changed portal).
#[uniffi::export]
pub fn portal_schema_dump(json_bodies: Vec<String>, dom_rows_json: String) -> String {
    let bodies: Vec<Value> = json_bodies
        .iter()
        .filter_map(|b| serde_json::from_str::<Value>(b).ok())
        .map(|v| schema_of(&v, 0))
        .collect();
    let dom: Vec<Vec<String>> = serde_json::from_str(&dom_rows_json).unwrap_or_default();
    let dom_shape: Vec<usize> = dom.iter().map(|r| r.len()).collect();
    serde_json::to_string_pretty(
        &serde_json::json!({ "json_responses": bodies, "dom_row_cell_counts": dom_shape }),
    )
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #[test]
    fn candidate_keys_are_names_only() {
        // the debug log may name fields, never their amounts
        let v = serde_json::json!({
            "statement_balance_amount": 6266.06,
            "nested": [{"charges_amount": 36.88, "description": "x"}],
            "payment_due_amount": 0,
        });
        let mut keys = std::collections::BTreeSet::new();
        super::candidate_keys(&v, &mut keys);
        let joined = keys.into_iter().collect::<Vec<_>>().join(", ");
        assert_eq!(joined, "charges_amount, statement_balance_amount");
        assert!(!joined.contains("6266") && !joined.contains("36.88"));
    }

    #[test]
    fn amex_statement_balance_wins() {
        let b = r#"{"charges_amount":1702.24,"debits_balance_amount":1702.24,"total_debits_balance_amount":1702.24,"last_statement_balance_amount":5515.59,"statement_balance_amount":6879.3,"totalAmount":0}"#;
        let (_, bal) =
            super::parse_json_bodies(&[b.to_string()], super::ConnectorKind::AmericanExpress);
        assert_eq!(bal, Some(-687930));
    }
    use super::*;

    #[test]
    fn amounts() {
        assert_eq!(parse_amount("1.234,56"), Some(123456));
        assert_eq!(parse_amount("1,234.56"), Some(123456));
        assert_eq!(parse_amount("-12,30"), Some(-1230));
        assert_eq!(parse_amount("12,30-"), Some(-1230));
        assert_eq!(parse_amount("€ 5"), Some(500));
        assert_eq!(parse_amount("1.234"), Some(123400));
        assert_eq!(parse_amount("abc"), None);
    }

    #[test]
    fn dates() {
        assert_eq!(parse_date("03.04.2025").as_deref(), Some("2025-04-03"));
        assert_eq!(parse_date("03/04/2025").as_deref(), Some("2025-04-03"));
        assert_eq!(
            parse_date("2025-04-03T10:00:00Z").as_deref(),
            Some("2025-04-03")
        );
        assert_eq!(parse_date("3. März 2025").as_deref(), Some("2025-03-03"));
        assert_eq!(parse_date("Mar 3, 2025").as_deref(), Some("2025-03-03"));
    }

    #[test]
    fn amex_csv_charges_become_negative() {
        let csv = "Datum,Beschreibung,Karteninhaber,Konto #,Betrag\n03/04/2025,\"REWE, BERLIN\",A B,-12345,\"23,50\"\n05/04/2025,PAYMENT RECEIVED,A B,-12345,\"-100,00\"\n";
        let p = parse_csv(csv, ConnectorKind::AmericanExpress);
        assert_eq!(p.len(), 2);
        assert_eq!((p[0].cents, p[0].text.as_str()), (-2350, "REWE, BERLIN"));
        assert_eq!(p[1].cents, 10000);
    }

    #[test]
    fn signed_csv_semicolon() {
        let csv = "Datum;Beschreibung;Betrag\n03.04.2025;Amazon;-19,99\n";
        let p = parse_csv(csv, ConnectorKind::AmazonVisa);
        assert_eq!(p[0].cents, -1999);
    }

    #[test]
    fn json_any_schema() {
        let b = r#"{"data":{"current_balance":"123,45","transactions":[{"charge_date":"2025-04-03","description":"REWE","amount":23.5,"identifier":"X1"},{"charge_date":"2025-04-04","description":"Refund","amount":-5.0}]}}"#;
        let (p, bal) = parse_json_bodies(&[b.to_string()], ConnectorKind::AmericanExpress);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].cents, -2350);
        assert_eq!(p[1].cents, 500);
        assert_eq!(bal, Some(-12345));
    }

    #[test]
    fn amex_portal_shape() {
        let b = r#"{"summary":{"total_balance":{"amount":"1.234,56"}},"transactions":[{"post_date":"2025-04-03","billing_amount":"23,50","extended_details":{"merchant":{"name":"REWE BERLIN"}},"reference_number":"R1"}],"pending_transactions":[{"charge_date":"2025-04-05","amount":9.9,"description":"PENDING SHOP","identifier":"P1"}]}"#;
        let (p, bal) = parse_json_bodies(&[b.to_string()], ConnectorKind::AmericanExpress);
        assert_eq!(p.len(), 1, "pending ignored while posted exist");
        assert_eq!(
            (p[0].cents, p[0].text.as_str(), p[0].id.as_deref()),
            (-2350, "REWE BERLIN", Some("R1"))
        );
        assert_eq!(bal, Some(-123456));
        let only_pending = r#"{"pending_transactions":[{"charge_date":"2025-04-05","amount":9.9,"description":"X","identifier":"P1"}]}"#;
        assert_eq!(
            parse_json_bodies(&[only_pending.to_string()], ConnectorKind::AmericanExpress)
                .0
                .len(),
            1
        );
    }

    #[test]
    fn openbank_portal_shape() {
        let b = r#"{"movements":[{"operationDate":"2025-04-03","amount":{"value":-19.99,"currency":"EUR"},"concept":"AMAZON.DE"},{"bookingDate":"2025-04-04","amount":50,"merchantName":"Einzahlung"}]}"#;
        let (p, _) = parse_json_bodies(&[b.to_string()], ConnectorKind::AmazonVisa);
        assert_eq!(p.len(), 2);
        assert_eq!((p[0].cents, p[0].text.as_str()), (-1999, "AMAZON.DE"));
        assert_eq!(p[1].cents, 5000);
    }

    #[test]
    fn schema_dump_masks_values() {
        let d = portal_schema_dump(
            vec![r#"{"transactions":[{"description":"SECRET SHOP","amount":12.3}]}"#.into()],
            "[]".into(),
        );
        assert!(d.contains("description") && d.contains("amount"));
        assert!(!d.contains("SECRET") && !d.contains("12.3"));
    }

    #[test]
    fn pdf_lines() {
        let t = "Abrechnung vom 15.04.2025\n03.04. REWE BERLIN 23,50\n05.04. 06.04. Zahlung erhalten 100,00 CR\n";
        let p = parse_statement_text(t, ConnectorKind::AmericanExpress);
        assert_eq!(p.len(), 2);
        assert_eq!((p[0].date.as_str(), p[0].cents), ("2025-04-03", -2350));
        assert_eq!(p[1].cents, 10000);
    }

    #[test]
    fn dom_rows() {
        let rows = vec![vec!["03.04.2025".into(), "REWE".into(), "23,50 €".into()]];
        let p = parse_dom_rows(&rows, ConnectorKind::AmazonVisa);
        assert_eq!((p[0].cents, p[0].text.as_str()), (2350, "REWE"));
    }
}
