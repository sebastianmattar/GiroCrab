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
use std::sync::{Arc, LazyLock};

use regex::Regex;
use rusqlite::{OptionalExtension, params};
use serde_json::Value;

use crate::connectors::{FetchResult, RemoteAccount, RemoteTransaction, SyncSummary, store_fetch};
use crate::dates::{day_month_on_or_before, de_dates, iso, month_from_name};
use crate::error::CoreError;
use crate::i18n::{tr, trf};
use crate::models::{AccountKind, ConnectorKind};
use crate::vault::Vault;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Parsed {
    pub date: String,
    pub text: String,
    pub cents: i64,
    pub id: Option<String>,
    /// Card holder the booking was made by, e.g. a partner card of the same
    /// Amex account ("Erika Mustermann"); `None` = unknown (#243).
    pub card: Option<String>,
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

/// Many date spellings -> "YYYY-MM-DD".
pub fn parse_date(s: &str) -> Option<String> {
    static ISO: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^(\d{4})[-/.](\d{1,2})[-/.](\d{1,2})").unwrap());
    static DMY: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^(\d{1,2})[./](\d{1,2})[./](\d{4}|\d{2})\b").unwrap());
    static DAY_MONTH_NAME: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^(\d{1,2})\.?\s+([A-Za-zäÄ]{3,9}\.?)\s+(\d{4})").unwrap()
    });
    static MONTH_NAME_DAY: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)^([A-Za-z]{3,9})\.?\s+(\d{1,2}),?\s+(\d{4})").unwrap());
    let s = s.trim();
    let ymd = |y, m, d| crate::dates::ymd(y, m, d).map(iso);
    if let Some(c) = ISO.captures(s) {
        return ymd(c[1].parse().ok()?, c[2].parse().ok()?, c[3].parse().ok()?);
    }
    if let Some(c) = DMY.captures(s) {
        return ymd(
            crate::dates::year(&c[3])?,
            c[2].parse().ok()?,
            c[1].parse().ok()?,
        );
    }
    if let Some(c) = DAY_MONTH_NAME.captures(s) {
        return ymd(
            c[3].parse().ok()?,
            month_from_name(&c[2])?,
            c[1].parse().ok()?,
        );
    }
    if let Some(c) = MONTH_NAME_DAY.captures(s) {
        return ymd(
            c[3].parse().ok()?,
            month_from_name(&c[1])?,
            c[2].parse().ok()?,
        );
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
    "signedamount",
    "amount",
    "amountvalue",
    "transactionamount",
    "transaction_amount",
    "betrag",
    "value",
    "billingamount",
    "billing_amount",
    "localamount",
    // Open Bank card portal: `amounts: [{currency, totalAmount}]`
    "amounts",
    "totalamount",
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
        if let Some((_, v)) = obj.iter().find(|(name, _)| name.to_lowercase() == *k)
            && !v.is_null()
        {
            return Some(v);
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

/// Largest amount accepted from portal JSON: 10 trillion euro in cents.
const MAX_PORTAL_CENTS: i64 = 1_000_000_000_000_000;

fn value_cents(v: &Value) -> Option<i64> {
    match v {
        // Rounded from the number's text (1.005 -> 101, not 100 as in f64).
        // No real account holds more than the cap; the sign handling (`abs`,
        // `-`) downstream would overflow near i64::MAX.
        Value::Number(_) => crate::money::json_number_to_cents_rounded(v)
            .ok()
            .filter(|c| c.abs() <= MAX_PORTAL_CENTS),
        Value::String(s) => parse_amount(s),
        // a single-currency list such as Open Bank's `amounts: [{…}]`
        Value::Array(a) => a.first().and_then(value_cents),
        Value::Object(o) => get_ci(
            o,
            &[
                "amount",
                "totalamount",
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
        card: card_label(obj).or_else(|| card_index(obj).map(|i| format!("#{i}"))),
    })
}

// ------------------------------------------------------------------ cards (#243)

/// Name on the card: Amex `embossed_name` (bookings and `card_member` entries).
const CARD_NAME_KEYS: &[&str] = &[
    "embossedname",
    "cardmembername",
    "cardholdername",
    "cardholder",
];
/// Amex numbers the main card and its partner cards per account (`supplementary_index`).
const CARD_INDEX_KEYS: &[&str] = &["supplementaryindex"];

/// Prefix of a portal booking text that names the card ("Kartenumsatz · Erika Mustermann").
pub const CARD_BOOKING_TEXT: &str = "Kartenumsatz";
pub const CARD_SEPARATOR: &str = " · ";

fn get_norm<'a>(obj: &'a serde_json::Map<String, Value>, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|k| {
        obj.iter()
            .find(|(name, v)| norm_key(name) == *k && !v.is_null())
            .map(|(_, v)| v)
    })
}

/// The object itself and objects nested one level below it under a card key
/// (`card_member: {…}`, `card: {…}`).
fn card_scopes(obj: &serde_json::Map<String, Value>) -> Vec<&serde_json::Map<String, Value>> {
    let mut out = vec![obj];
    for (k, v) in obj {
        let k = norm_key(k);
        if let (true, Some(o)) = (k.contains("card") || k.contains("member"), v.as_object()) {
            out.push(o);
        }
    }
    out
}

/// "MAX MUSTERMANN" -> "Max Mustermann"; mixed case stays as it is.
fn title_case(s: &str) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().any(|c| c.is_lowercase()) {
        return s;
    }
    s.split(' ')
        .map(|w| {
            let lower = w.to_lowercase();
            let mut c = lower.chars();
            c.next()
                .map(|f| f.to_uppercase().chain(c).collect())
                .unwrap_or_default()
        })
        .collect::<Vec<String>>()
        .join(" ")
}

/// Card holder of a booking or card-member object.
fn card_label(obj: &serde_json::Map<String, Value>) -> Option<String> {
    let scopes = card_scopes(obj);
    scopes
        .iter()
        .find_map(|o| get_norm(o, CARD_NAME_KEYS).and_then(Value::as_str))
        .map(title_case)
        .or_else(|| {
            scopes.iter().find_map(|o| {
                let part = |k: &str| get_norm(o, &[k]).and_then(Value::as_str).map(str::trim);
                let full = [part("firstname"), part("lastname")]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" ");
                (!full.trim().is_empty()).then(|| title_case(&full))
            })
        })
        .filter(|n| !n.is_empty())
}

fn card_index(obj: &serde_json::Map<String, Value>) -> Option<String> {
    card_scopes(obj)
        .iter()
        .find_map(|o| get_norm(o, CARD_INDEX_KEYS).and_then(value_text))
}

/// `supplementary_index` -> card label from the portal's card-member lists
/// (`card_member_totals`, `supplementary_accounts`), for bookings that only
/// carry the index.
fn card_directory(v: &Value, out: &mut HashMap<String, String>) {
    match v {
        Value::Object(o) => {
            if let (Some(i), Some(l)) = (card_index(o), card_label(o)) {
                out.entry(i).or_insert(l);
            }
            o.values().for_each(|x| card_directory(x, out));
        }
        Value::Array(a) => a.iter().for_each(|x| card_directory(x, out)),
        _ => {}
    }
}

/// What one portal capture holds: posted bookings, pending (not yet posted)
/// card charges and the card balance.
#[derive(Debug, Default)]
pub(crate) struct Capture {
    pub posted: Vec<Parsed>,
    pub pending: Vec<Parsed>,
    /// A pending list (possibly empty) was among the captured responses (diagnostics).
    pub pending_listed: bool,
    pub balance: Option<(String, i64)>,
}

/// Status fields that mark a single item as pending.
const STATUS_KEYS: &[&str] = &["status", "transaction_status", "transactionstatus"];

/// A pending charge outside a `pending…` list: a status field says so, or (Amex) the
/// id is an authorisation number. Amex delivers open charges in a plain `transactions`
/// list; their ids are `P` + digits, posted ones never are (`AT…`, `1000…`).
fn item_is_pending(obj: &serde_json::Map<String, Value>, p: &Parsed, kind: ConnectorKind) -> bool {
    let status = get_ci(obj, STATUS_KEYS)
        .and_then(value_text)
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    if ["pending", "pdng", "authori", "vorgemerkt"]
        .iter()
        .any(|w| status.contains(w))
    {
        return true;
    }
    kind == ConnectorKind::AmericanExpress
        && p.id.as_deref().is_some_and(|id| {
            id.len() >= 11 && id.starts_with('P') && id[1..].chars().all(|c| c.is_ascii_digit())
        })
}

fn walk(v: &Value, kind: ConnectorKind, pending: bool, out: &mut Capture) {
    match v {
        Value::Array(items) => {
            let parsed: Vec<(Parsed, bool)> = items
                .iter()
                .filter_map(|i| {
                    let o = i.as_object()?;
                    let p = parse_tx_object(o, kind)?;
                    let is_pending = pending || item_is_pending(o, &p, kind);
                    Some((p, is_pending))
                })
                .collect();
            if !parsed.is_empty() {
                for (p, is_pending) in parsed {
                    if is_pending {
                        out.pending.push(p)
                    } else {
                        out.posted.push(p)
                    }
                }
            } else {
                items.iter().for_each(|i| walk(i, kind, pending, out));
            }
        }
        Value::Object(o) => {
            for (k, val) in o {
                let pending_key = k.to_lowercase().contains("pending");
                if pending_key && val.is_array() {
                    out.pending_listed = true;
                }
                walk(val, kind, pending || pending_key, out);
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

/// An object that lists several balance fields side by side (the card's balance
/// summary): a zero there is a real balance, e.g. Amex `statement_balance_amount`
/// once the last statement is paid. A lone zero elsewhere is a placeholder.
fn is_balance_summary(o: &serde_json::Map<String, Value>) -> bool {
    o.keys().filter(|k| norm_key(k).contains("balance")).count() >= 3
}

fn find_key<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::Object(o) => {
            let zero_counts = is_balance_summary(o);
            if let Some((_, x)) = o.iter().find(|(k, x)| {
                norm_key(k) == key && value_cents(x).is_some_and(|c| c != 0 || zero_counts)
            }) {
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
                        && let Some(c) = value_cents(val).filter(|c| *c != 0)
                    {
                        return Some((k.clone(), c));
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
                    out.insert(safe_key(k));
                }
                candidate_keys(val, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| candidate_keys(x, out)),
        _ => {}
    }
}

/// Posted bookings and balance of a capture (pending charges are left out).
pub fn parse_json_bodies(bodies: &[String], kind: ConnectorKind) -> (Vec<Parsed>, Option<i64>) {
    let c = parse_capture(bodies, kind);
    (c.posted, c.balance.map(|(_, c)| c))
}

pub(crate) fn parse_capture(bodies: &[String], kind: ConnectorKind) -> Capture {
    let mut c = Capture::default();
    let mut cards = HashMap::new();
    for b in bodies {
        let Ok(v) = serde_json::from_str::<Value>(b) else {
            continue;
        };
        walk(&v, kind, false, &mut c);
        card_directory(&v, &mut cards);
        if c.balance.is_none() {
            c.balance = find_balance(&v, kind);
        }
    }
    // Bookings with only a card index: name them from the card-member list (#243).
    for t in c.posted.iter_mut().chain(c.pending.iter_mut()) {
        if let Some(i) = t.card.as_deref().and_then(|c| c.strip_prefix('#')) {
            t.card = Some(
                cards
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| format!("Karte {i}")),
            );
        }
    }
    // The portal re-sends overlapping pages; drop exact repeats of the same source id.
    // Pending charges get a new id (and often a longer text) once posted, so they are
    // never stored as regular bookings: only as holds, see `to_fetch`.
    let mut seen = std::collections::HashSet::new();
    c.posted
        .retain(|t| t.id.as_ref().is_none_or(|id| seen.insert(id.clone())));
    c.pending
        .retain(|t| t.id.as_ref().is_none_or(|id| seen.insert(id.clone())));
    c
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
                .map(|(k, x)| (safe_key(k), schema_of(x, depth + 1)))
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
            card: None,
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
            card: None,
        });
    }
    out
}

// ------------------------------------------------------------------ PDF statements

/// Line-based best effort on extracted statement text:
/// `DD.MM.[YY[YY]]  Beschreibung …  1.234,56[-| CR| H]`.
pub fn parse_statement_text(text: &str, kind: ConnectorKind) -> Vec<Parsed> {
    static LINE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^\s*(\d{1,2}\.\d{1,2}\.(?:\d{4}|\d{2})?)\s+(?:(\d{1,2}\.\d{1,2}\.(?:\d{4}|\d{2})?)\s+)?(.+?)\s+(-?\d{1,3}(?:\.\d{3})*,\d{2}|-?\d+,\d{2})\s*(-|CR|H|S|\+)?\s*$").unwrap()
    });
    // A "DD.MM." line falls on or before the statement's latest date. Dates after
    // today are fine print (new terms, card expiry), not the statement's.
    let today = crate::dates::today();
    let latest = de_dates(text)
        .filter(|d| *d <= today)
        .max()
        .unwrap_or(today);
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(c) = LINE.captures(line) else {
            continue;
        };
        let date = match c[1].strip_suffix('.').and_then(|dm| dm.split_once('.')) {
            Some((d, m)) => d
                .parse()
                .ok()
                .zip(m.parse().ok())
                .and_then(|(d, m)| day_month_on_or_before(d, m, latest))
                .map(iso),
            None => parse_date(&c[1]),
        };
        let Some(date) = date else {
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
            card: None,
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

/// `external_id` prefix of pending card charges ("vorgemerkt"); `Booking.is_pending`
/// relies on the `pending-` start. A hold is never dropped silently: only when its
/// posted booking arrives ([`resolve_holds`]) or when the user discards it
/// ([`Vault::discard_portal_holds`]). The generic 14-day expiry in `persist.rs` skips it.
pub(crate) const HOLD_PREFIX: &str = "pending-hold-portal-";

/// Days a posted booking may lie before / after the pending charge it settles.
const SETTLE_DAYS_BEFORE: i64 = 1;
const SETTLE_DAYS_AFTER: i64 = 5;

/// A pending charge the portal no longer lists as open whose posted booking was not
/// found yet (#629): the user browses older billing periods or discards it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PortalHold {
    pub id: i64,
    pub booking_date: String,
    pub amount_cents: i64,
    pub counterparty: String,
}

/// Holds need a reported balance: without one the balance is accumulated from the
/// stored bookings, and a hold resolved later would stay in it.
fn to_fetch(
    connection_id: i64,
    name: &str,
    kind: ConnectorKind,
    parsed: Vec<Parsed>,
    holds: Vec<Parsed>,
    balance: Option<i64>,
) -> FetchResult {
    let (account, transactions) = card_account(
        &account_ref(kind, connection_id),
        name,
        "",
        parsed,
        holds,
        balance,
    );
    FetchResult {
        // a reported balance, even 0, replaces the stored one
        balances_reported: if balance.is_some() {
            vec![account.external_ref.clone()]
        } else {
            Vec::new()
        },
        accounts: vec![account],
        transactions,
        state: None,
        // no reported balance: accumulate the imported bookings instead
        adjust_balances: balance.is_none(),
        ..Default::default()
    }
}

/// One card account with its bookings and (with a reported balance) its pending
/// charges as holds.
fn card_account(
    account_ref: &str,
    name: &str,
    number: &str,
    parsed: Vec<Parsed>,
    holds: Vec<Parsed>,
    balance: Option<i64>,
) -> (RemoteAccount, Vec<RemoteTransaction>) {
    let mut transactions = Vec::new();
    let mut push = |list: Vec<Parsed>, prefix: &str, pending: bool| {
        let mut seen: HashMap<(String, i64, String), u32> = HashMap::new();
        let mut ids_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for p in list {
            let booking_text = match (&p.card, pending) {
                (_, true) => "Kartenumsatz (vorgemerkt)".to_string(),
                (Some(card), false) => format!("{CARD_BOOKING_TEXT}{CARD_SEPARATOR}{card}"),
                (None, false) => CARD_BOOKING_TEXT.to_string(),
            };
            let mut t = RemoteTransaction {
                account_ref: account_ref.to_string(),
                external_id: String::new(),
                booking_date: p.date.clone(),
                value_date: p.date.clone(),
                amount_cents: p.cents,
                currency: "EUR".into(),
                counterparty: p.text.clone(),
                counterparty_iban: String::new(),
                purpose: String::new(),
                booking_text,
                ..Default::default()
            };
            let n = seen.entry((p.date, p.cents, p.text)).or_insert(0);
            t.external_id = match p.id {
                Some(id) if ids_seen.insert(id.clone()) => format!("{prefix}id:{id}"),
                _ => format!("{prefix}{}", t.content_id(*n)),
            };
            *n += 1;
            transactions.push(t);
        }
    };
    push(parsed, "", false);
    if balance.is_some() {
        push(holds, HOLD_PREFIX, true);
    }
    let account = RemoteAccount {
        external_ref: account_ref.to_string(),
        name: name.to_string(),
        owner: String::new(),
        iban: String::new(),
        account_number: number.to_string(),
        currency: "EUR".into(),
        kind: AccountKind::CreditCard,
        balance_cents: balance.unwrap_or(0),
        balance_date: crate::connectors::today(),
    };
    (account, transactions)
}

pub(crate) fn account_ref(kind: ConnectorKind, connection_id: i64) -> String {
    format!(
        "portal-{}-{}",
        kind.as_str().replace(':', "-"),
        connection_id
    )
}

// ------------------------------------------------------------------ several cards (#243)

/// One card of a portal login, e.g. an entry of the Amex card switcher. `key`
/// tags the captured responses of that card (a hash of the portal's account
/// token, never the token itself).
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PortalCard {
    pub key: String,
    /// "Amazon Business American Express® Card"
    pub name: String,
    /// Last digits as the portal shows them, e.g. "12345".
    pub number: String,
}

/// Bodies grouped by card key, in first-seen order. Bodies without a key
/// (profile, navigation) join the first group.
fn group_by_card(bodies: &[String], keys: &[String]) -> Vec<(String, Vec<String>)> {
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    let mut unkeyed = Vec::new();
    for (i, b) in bodies.iter().enumerate() {
        let k = keys.get(i).map(|k| k.trim()).unwrap_or("");
        if k.is_empty() {
            unkeyed.push(b.clone());
            continue;
        }
        match groups.iter_mut().find(|(g, _)| g == k) {
            Some((_, v)) => v.push(b.clone()),
            None => groups.push((k.to_string(), vec![b.clone()])),
        }
    }
    match groups.first_mut() {
        Some((_, first)) => first.extend(unkeyed),
        None => groups.push((String::new(), unkeyed)),
    }
    groups
}

/// The capture of one card.
struct CardCapture {
    key: String,
    posted: Vec<Parsed>,
    pending: Vec<Parsed>,
    balance: Option<i64>,
}

/// Bookings, pending charges and balance per card; only cards with any of them.
fn parse_cards(bodies: &[String], keys: &[String], kind: ConnectorKind) -> Vec<CardCapture> {
    group_by_card(bodies, keys)
        .into_iter()
        .map(|(key, group)| {
            let c = parse_capture(&group, kind);
            CardCapture {
                key,
                posted: c.posted,
                pending: c.pending,
                balance: c.balance.map(|(_, b)| b),
            }
        })
        .filter(|c| !c.posted.is_empty() || !c.pending.is_empty() || c.balance.is_some())
        .collect()
}

/// An account this login already has (earlier portal syncs, MoneyMoney import).
struct StoredCard {
    account_ref: String,
    /// Digits of its account / card number.
    digits: String,
    /// External ids of its bookings.
    ids: std::collections::HashSet<String>,
}

/// One account per card. A card goes to the stored account with the same last
/// card digits, else to the one holding its bookings; nothing is imported
/// twice. Several cards: the ones new to this login wait for the account
/// selection.
fn cards_fetch(
    base_ref: &str,
    name: &str,
    cards: Vec<CardCapture>,
    labels: &[PortalCard],
    stored: &[StoredCard],
) -> FetchResult {
    let label = |key: &str| labels.iter().find(|c| c.key == key);
    let mut target: Vec<Option<String>> = vec![None; cards.len()];
    let mut used = std::collections::HashSet::new();
    // 1. same card number
    for (i, c) in cards.iter().enumerate() {
        let number: String = label(&c.key)
            .map(|l| l.number.chars().filter(char::is_ascii_digit).collect())
            .unwrap_or_default();
        if number.len() < 4 {
            continue;
        }
        if let Some(s) = stored
            .iter()
            .find(|s| !used.contains(&s.account_ref) && s.digits.ends_with(&number))
        {
            used.insert(s.account_ref.clone());
            target[i] = Some(s.account_ref.clone());
        }
    }
    // 2. the account that holds the card's bookings
    for (i, c) in cards.iter().enumerate() {
        if target[i].is_some() {
            continue;
        }
        let overlap = |s: &StoredCard| {
            c.posted
                .iter()
                .filter(|p| {
                    p.id.as_ref()
                        .is_some_and(|id| s.ids.contains(&format!("id:{id}")))
                })
                .count()
        };
        if let Some(s) = stored
            .iter()
            .filter(|s| !used.contains(&s.account_ref))
            .max_by_key(|s| overlap(s))
            .filter(|s| overlap(s) > 0)
        {
            used.insert(s.account_ref.clone());
            target[i] = Some(s.account_ref.clone());
        }
    }
    // 3. the login's own account: for the first card left, unless other accounts
    //    of this login are there that it may belong to
    let base_free = !used.contains(base_ref)
        && (cards.len() == 1
            || stored.is_empty()
            || stored.iter().any(|s| s.account_ref == base_ref));
    if base_free && let Some(t) = target.iter_mut().find(|t| t.is_none()) {
        *t = Some(base_ref.to_string());
    }
    let adjust_balances = cards.iter().all(|c| c.balance.is_none());
    let mut out = FetchResult {
        adjust_balances,
        confirm_new_accounts: cards.len() > 1,
        ..Default::default()
    };
    for (i, (c, t)) in cards.into_iter().zip(target).enumerate() {
        let l = label(&c.key);
        let number = l.map(|l| l.number.trim()).unwrap_or_default();
        let account_ref = t.unwrap_or_else(|| format!("{base_ref}-{}", c.key));
        // stored accounts keep their name; this one is for new ones
        let account_name = l
            .map(|l| l.name.trim().to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| {
                if i == 0 {
                    name.to_string()
                } else {
                    format!("{name} {}", i + 1)
                }
            });
        // A balance the portal reported, even 0 (statement paid), replaces the
        // stored one; a card without one keeps it.
        if c.balance.is_some() {
            out.balances_reported.push(account_ref.clone());
        }
        let (account, transactions) = card_account(
            &account_ref,
            &account_name,
            number,
            c.posted,
            c.pending,
            c.balance,
        );
        out.accounts.push(account);
        out.transactions.extend(transactions);
    }
    out
}

/// SQL condition for the open holds of an account (`t` = transactions). Amex syncs
/// before #629 stored pending charges as regular bookings (`id:P…`): they are
/// treated like holds, under the hold id they would have today.
fn hold_sql(legacy: bool) -> &'static str {
    if legacy {
        "(t.external_id LIKE 'pending-hold-portal-%' OR t.external_id LIKE 'id:P%')"
    } else {
        "t.external_id LIKE 'pending-hold-portal-%'"
    }
}

/// Settles holds whose posted booking has arrived: same amount, posted from
/// [`SETTLE_DAYS_BEFORE`] days before to [`SETTLE_DAYS_AFTER`] days after the charge,
/// stored after the hold, each posted booking settles one hold. Category, note and
/// check mark go to the posted booking when it has none; the hold is deleted (syncs).
/// A pre-#629 copy of a charge that is a hold again merges into that hold.
/// Holds with splits or documents stay (user work). Returns the number settled.
pub(crate) fn resolve_holds(
    db: &rusqlite::Connection,
    account_id: i64,
    legacy: bool,
) -> Result<u32, CoreError> {
    let mut settled = 0u32;
    if legacy {
        let pairs: Vec<(i64, i64)> = {
            let mut q = db.prepare(
                "SELECT c.id, h.id FROM transactions c JOIN transactions h
                    ON h.account_id = c.account_id AND h.deleted_ms IS NULL
                   AND h.external_id = 'pending-hold-portal-' || c.external_id
                 WHERE c.account_id = ?1 AND c.deleted_ms IS NULL AND c.external_id LIKE 'id:P%'",
            )?;
            let rows = q.query_map([account_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<Result<_, _>>()?
        };
        for (copy, hold) in pairs {
            hand_over(db, copy, hold)?;
            settled += 1;
        }
    }
    let holds: Vec<(i64, String, i64, String)> = {
        let mut q = db.prepare(&format!(
            "SELECT t.id, t.booking_date, t.amount_cents, t.counterparty FROM transactions t
              WHERE t.account_id = ?1 AND t.deleted_ms IS NULL AND {}
                AND NOT EXISTS (SELECT 1 FROM transaction_splits s WHERE s.transaction_id = t.id)
                AND NOT EXISTS (SELECT 1 FROM booking_documents d WHERE d.booking_id = t.id)
              ORDER BY t.booking_date, t.id",
            hold_sql(legacy)
        ))?;
        let rows = q.query_map([account_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
        rows.collect::<Result<_, _>>()?
    };
    // (posted id, amount, counterparty) stored after `hold` within the settle window
    let candidates = |hold: i64, date: &str| -> Result<Vec<(i64, i64, String)>, CoreError> {
        let mut q = db.prepare(
            "SELECT p.id, p.amount_cents, p.counterparty FROM transactions p
              WHERE p.account_id = ?1 AND p.deleted_ms IS NULL AND p.id > ?2
                AND p.external_id NOT LIKE 'pending-%' AND p.external_id NOT LIKE 'id:P%'
                AND p.booking_date BETWEEN date(?3, ?4) AND date(?3, ?5)
              ORDER BY abs(julianday(p.booking_date) - julianday(?3)), p.id",
        )?;
        let rows = q.query_map(
            params![
                account_id,
                hold,
                date,
                format!("-{SETTLE_DAYS_BEFORE} days"),
                format!("+{SETTLE_DAYS_AFTER} days")
            ],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        Ok(rows.collect::<Result<_, _>>()?)
    };
    let mut used = std::collections::HashSet::new();
    let mut open = Vec::new();
    // 1. the same amount (closest date first), so exact matches claim their posting first
    for (hold, date, cents, text) in holds {
        let posted = candidates(hold, &date)?
            .into_iter()
            .find(|(p, c, _)| *c == cents && !used.contains(p));
        match posted {
            Some((p, _, _)) => {
                used.insert(p);
                hand_over(db, hold, p)?;
                settled += 1;
            }
            None => open.push((hold, date, cents, text)),
        }
    }
    // 2. a pre-authorisation settled lower (hotel, fuel): same merchant, smaller or equal
    //    amount of the same sign, and only when exactly one posting fits
    for (hold, date, cents, text) in open {
        let Some(key) = merchant_key(&text) else {
            continue;
        };
        let fits: Vec<i64> = candidates(hold, &date)?
            .into_iter()
            .filter(|(p, c, t)| {
                !used.contains(p)
                    && c.signum() == cents.signum()
                    && c.abs() <= cents.abs()
                    && merchant_key(t).as_deref() == Some(key.as_str())
            })
            .map(|(p, _, _)| p)
            .collect();
        if let [p] = fits[..] {
            used.insert(p);
            hand_over(db, hold, p)?;
            settled += 1;
        }
    }
    if legacy {
        convert_legacy_copies(db, account_id)?;
    }
    Ok(settled)
}

/// Pre-#629 copies (`id:P…`) still open after settling become regular holds, so they
/// are shown as pending ("vorgemerkt"). The id is not changed in place (the iCloud
/// merge would bring the old row back): the row is copied under its hold id with all
/// its data and the old row is deleted (tombstone). A copy whose hold id is taken
/// (a discarded hold) is deleted. Copies with splits or documents stay.
fn convert_legacy_copies(db: &rusqlite::Connection, account_id: i64) -> Result<u32, CoreError> {
    let cols: Vec<String> = {
        let mut q = db.prepare("SELECT name FROM pragma_table_info('transactions')")?;
        let rows = q.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|c| !["id", "external_id", "booking_text", "deleted_ms"].contains(&c.as_str()))
            .collect()
    };
    let list = cols.join(", ");
    let copy_sql = format!(
        "INSERT OR IGNORE INTO transactions(external_id, booking_text, {list})
         SELECT '{HOLD_PREFIX}' || external_id, 'Kartenumsatz (vorgemerkt)', {list}
           FROM transactions WHERE id = ?1"
    );
    let copies: Vec<i64> = {
        let mut q = db.prepare(
            "SELECT t.id FROM transactions t
              WHERE t.account_id = ?1 AND t.deleted_ms IS NULL AND t.external_id LIKE 'id:P%'
                AND NOT EXISTS (SELECT 1 FROM transaction_splits s WHERE s.transaction_id = t.id)
                AND NOT EXISTS (SELECT 1 FROM booking_documents d WHERE d.booking_id = t.id)",
        )?;
        let rows = q.query_map([account_id], |r| r.get(0))?;
        rows.collect::<Result<_, _>>()?
    };
    for id in &copies {
        db.execute(&copy_sql, [id])?;
        crate::sync_merge::soft_delete_booking(db, *id)?;
    }
    Ok(copies.len() as u32)
}

/// First word of a card counterparty, lower case, letters and digits only
/// ("ARAL PAYBACK PAY  BOCHUM" -> "aral"); `None` when shorter than 3 characters.
fn merchant_key(text: &str) -> Option<String> {
    let word: String = text
        .split_whitespace()
        .next()?
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    (word.chars().count() >= 3).then_some(word)
}

/// The Amex booking reference in an imported purpose ("… EREF: AT2625100450…").
fn eref_of(purpose: &str) -> Option<&str> {
    let rest = &purpose[purpose.find("EREF: ")? + "EREF: ".len()..];
    let r = rest.split(|c: char| c.is_whitespace() || c == ',').next()?;
    (r.len() >= 10 && r.chars().all(|c| c.is_ascii_alphanumeric())).then_some(r)
}

/// Amex (#871): the database import brings a split booking over as its parts, each a
/// booking of its own carrying the Amex reference (`EREF: <ref>`). The portal then
/// delivers the whole booking (`id:<ref>---…`), which matches no single part. When at
/// least two imported parts carry the reference of exactly one portal booking and add
/// up to it exactly, the portal booking becomes a split booking (`transaction_splits`:
/// the parts' categories, amounts and notes; tags carried over; checked when all parts
/// were) and the parts are deleted (tombstones, iCloud-safe). Its own category goes
/// unless set by hand: a split booking is shown by the categories of its parts.
/// Parts with splits or documents and parts that don't add up stay as they are.
/// Idempotent; returns the number of merged bookings.
pub(crate) fn merge_imported_split_parts(
    db: &rusqlite::Connection,
    account_id: i64,
) -> Result<u32, CoreError> {
    // (id, amount, category, note, checked) of imported parts, by reference
    type Part = (i64, i64, Option<i64>, String, bool);
    let mut by_ref: std::collections::BTreeMap<String, Vec<Part>> = Default::default();
    {
        let mut q = db.prepare(
            "SELECT t.id, t.purpose, t.amount_cents, t.category_id, t.note, t.checked FROM transactions t
              WHERE t.account_id = ?1 AND t.deleted_ms IS NULL AND t.external_id LIKE 'mm-%'
                AND t.purpose LIKE '%EREF: %'
                AND NOT EXISTS (SELECT 1 FROM transaction_splits s WHERE s.transaction_id = t.id)
                AND NOT EXISTS (SELECT 1 FROM booking_documents d WHERE d.booking_id = t.id)
              ORDER BY t.id",
        )?;
        let rows = q.query_map([account_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, bool>(5)?,
            ))
        })?;
        for row in rows {
            let (id, purpose, cents, cat, note, checked) = row?;
            if let Some(r) = eref_of(&purpose) {
                by_ref
                    .entry(r.to_string())
                    .or_default()
                    .push((id, cents, cat, note, checked));
            }
        }
    }
    let now = crate::dates::now_ms();
    let mut merged = 0u32;
    for (reference, parts) in by_ref {
        if parts.len() < 2 || parts.iter().any(|p| p.1 == 0) {
            continue;
        }
        // the portal booking with this reference (exactly one, not split yet)
        let bank: Vec<(i64, i64, bool)> = {
            let mut q = db.prepare(
                "SELECT t.id, t.amount_cents, t.checked FROM transactions t
                  WHERE t.account_id = ?1 AND t.deleted_ms IS NULL AND substr(t.external_id, 1, ?3) = ?2
                    AND NOT EXISTS (SELECT 1 FROM transaction_splits s WHERE s.transaction_id = t.id)",
            )?;
            let prefix = format!("id:{reference}---");
            let rows = q.query_map(params![account_id, prefix, prefix.len() as i64], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
            rows.collect::<Result<_, _>>()?
        };
        let [(bank_id, bank_cents, bank_checked)] = bank[..] else {
            continue;
        };
        if parts.iter().map(|p| p.1).sum::<i64>() != bank_cents {
            continue;
        }
        let lines: Vec<_> = parts
            .iter()
            .map(|(_, cents, cat, note, _)| (*cat, *cents, note.as_str()))
            .collect();
        crate::planning::store_splits(db, bank_id, &lines)?;
        crate::planning::clear_split_category(db, bank_id)?;
        if !bank_checked && parts.iter().all(|p| p.4) {
            db.execute(
                "UPDATE transactions SET checked = 1, checked_ms = ?2 WHERE id = ?1",
                params![bank_id, now],
            )?;
        }
        let ids = parts
            .iter()
            .map(|p| p.0.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let tagged = db.execute(
            &format!(
                "INSERT OR IGNORE INTO transaction_tags(transaction_id, tag_id)
                 SELECT ?1, tag_id FROM transaction_tags WHERE transaction_id IN ({ids})"
            ),
            [bank_id],
        )?;
        if tagged > 0 {
            db.execute(
                "UPDATE transactions SET tags_ms = ?2 WHERE id = ?1",
                params![bank_id, now],
            )?;
        }
        for (id, ..) in &parts {
            crate::sync_merge::soft_delete_booking(db, *id)?;
        }
        merged += 1;
    }
    Ok(merged)
}

/// Moves category, note and check mark from `from` to `to` where `to` has none,
/// then deletes `from` (tombstone, so other devices drop it too).
pub(crate) fn hand_over(db: &rusqlite::Connection, from: i64, to: i64) -> Result<(), CoreError> {
    let now = crate::dates::now_ms();
    db.execute(
        "UPDATE transactions SET (category_id, category_source, rule_id, category_ms) =
                (SELECT f.category_id, f.category_source, f.rule_id, ?3 FROM transactions f WHERE f.id = ?1)
          WHERE id = ?2 AND category_id IS NULL
            AND (SELECT category_id FROM transactions WHERE id = ?1) IS NOT NULL",
        params![from, to, now],
    )?;
    db.execute(
        "UPDATE transactions SET (note, note_ms) = (SELECT f.note, ?3 FROM transactions f WHERE f.id = ?1)
          WHERE id = ?2 AND trim(note) = ''
            AND trim((SELECT note FROM transactions WHERE id = ?1)) != ''",
        params![from, to, now],
    )?;
    db.execute(
        "UPDATE transactions SET checked = 1, checked_ms = ?3
          WHERE id = ?2 AND checked = 0 AND (SELECT checked FROM transactions WHERE id = ?1) = 1",
        params![from, to, now],
    )?;
    crate::sync_merge::soft_delete_booking(db, from)
}

impl Vault {
    pub(crate) fn portal_connection(
        &self,
        connection_id: i64,
    ) -> Result<(ConnectorKind, String), CoreError> {
        let db = self.db()?;
        let (kind, name): (String, String) = db
            .query_row(
                "SELECT connector, display_name FROM bank_connections WHERE id = ?1 AND deleted_ms IS NULL",
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

    /// The connection's card account, if it exists yet.
    fn portal_account(
        &self,
        kind: ConnectorKind,
        connection_id: i64,
    ) -> Result<Option<i64>, CoreError> {
        let db = self.db()?;
        Ok(db
            .query_row(
                "SELECT id FROM accounts WHERE connection_id = ?1 AND external_ref = ?2 AND deleted_ms IS NULL",
                params![connection_id, account_ref(kind, connection_id)],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Accounts of a login with their card digits and booking ids.
    fn portal_stored_cards(&self, connection_id: i64) -> Result<Vec<StoredCard>, CoreError> {
        let db = self.db()?;
        let mut q = db.prepare(
            "SELECT id, external_ref, IFNULL(account_number, '') FROM accounts
              WHERE connection_id = ?1 AND deleted_ms IS NULL ORDER BY id",
        )?;
        let rows: Vec<(i64, String, String)> = q
            .query_map([connection_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?;
        let mut ids_q = db.prepare("SELECT external_id FROM transactions WHERE account_id = ?1")?;
        rows.into_iter()
            .map(|(id, account_ref, number)| {
                let ids = ids_q
                    .query_map([id], |r| r.get::<_, String>(0))?
                    .collect::<Result<_, _>>()?;
                Ok(StoredCard {
                    account_ref,
                    digits: number.chars().filter(char::is_ascii_digit).collect(),
                    ids,
                })
            })
            .collect()
    }

    /// Settles the pending charges of the given accounts of a login (see [`resolve_holds`]).
    fn settle_holds(
        &self,
        kind: ConnectorKind,
        connection_id: i64,
        refs: &[String],
    ) -> Result<(), CoreError> {
        let db = self.db()?;
        let tx = db.unchecked_transaction()?;
        let mut n = 0;
        for r in refs {
            let account: Option<i64> = tx
                .query_row(
                    "SELECT id FROM accounts WHERE connection_id = ?1 AND external_ref = ?2 AND deleted_ms IS NULL",
                    params![connection_id, r],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(a) = account {
                n += resolve_holds(&tx, a, kind == ConnectorKind::AmericanExpress)?;
            }
        }
        tx.commit()?;
        if n > 0 {
            crate::depot_debug::log(&format!("portal import: settled {n} pending charge(s)"));
        }
        Ok(())
    }

    fn import_parsed(
        &self,
        connection_id: i64,
        parsed: Vec<Parsed>,
        holds: Vec<Parsed>,
        balance: Option<i64>,
    ) -> Result<SyncSummary, CoreError> {
        let (kind, name) = self.portal_connection(connection_id)?;
        if parsed.is_empty() && (holds.is_empty() || balance.is_none()) {
            return Err(CoreError::InvalidInput {
                message: tr("Keine Umsätze erkannt.").into(),
            });
        }
        let fetch = to_fetch(connection_id, &name, kind, parsed, holds, balance);
        let summary = store_fetch(self, kind, connection_id, fetch)?;
        if let Some(account) = self.portal_account(kind, connection_id)? {
            let db = self.db()?;
            let tx = db.unchecked_transaction()?;
            let n = resolve_holds(&tx, account, kind == ConnectorKind::AmericanExpress)?;
            let merged = if kind == ConnectorKind::AmericanExpress {
                merge_imported_split_parts(&tx, account)?
            } else {
                0
            };
            tx.commit()?;
            if n > 0 {
                crate::depot_debug::log(&format!("portal import: settled {n} pending charge(s)"));
            }
            if merged > 0 {
                crate::depot_debug::log(&format!(
                    "portal import: merged {merged} imported split booking(s)"
                ));
            }
        }
        // Fold the import into the main file; best effort, it is committed.
        let _ = self.checkpoint();
        Ok(summary)
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
        self.import_portal_capture_cards(connection_id, json_bodies, vec![], vec![], dom_rows_json)
    }

    /// [`Self::import_portal_capture`] for a login with several cards (#243):
    /// `card_keys[i]` names the card `json_bodies[i]` belongs to ("" = unknown),
    /// `cards` labels the keys. Each card becomes an account (or goes to the stored
    /// account with its card digits / bookings); cards new to this login wait for
    /// the account selection.
    pub fn import_portal_capture_cards(
        self: Arc<Self>,
        connection_id: i64,
        json_bodies: Vec<String>,
        card_keys: Vec<String>,
        cards: Vec<PortalCard>,
        dom_rows_json: String,
    ) -> Result<SyncSummary, CoreError> {
        let (kind, name) = self.portal_connection(connection_id)?;
        let mut per_card = parse_cards(&json_bodies, &card_keys, kind);
        // a card seen only in side requests (no bookings) counts only when the
        // card switcher named it
        per_card.retain(|c| !c.posted.is_empty() || cards.iter().any(|l| l.key == c.key));
        if per_card.iter().any(|c| !c.key.is_empty()) {
            crate::depot_debug::log(&format!(
                "portal import: {:?} cards={} named={} bodies={}",
                kind,
                per_card.len(),
                cards.len(),
                json_bodies.len()
            ));
            let base_ref = account_ref(kind, connection_id);
            let stored = self.portal_stored_cards(connection_id)?;
            let fetch = cards_fetch(&base_ref, &name, per_card, &cards, &stored);
            let refs: Vec<String> = fetch
                .accounts
                .iter()
                .map(|a| a.external_ref.clone())
                .collect();
            let summary = store_fetch(&self, kind, connection_id, fetch)?;
            self.settle_holds(kind, connection_id, &refs)?;
            let _ = self.checkpoint();
            return Ok(summary);
        }
        let capture = parse_capture(&json_bodies, kind);
        let balance = capture.balance.as_ref().map(|(_, c)| *c);
        match &capture.balance {
            Some((k, _)) => {
                crate::depot_debug::log(&format!("portal matched balance: key={}", safe_key(k)))
            }
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
        let mut hints = std::collections::BTreeMap::new();
        for b in &json_bodies {
            if let Ok(v) = serde_json::from_str::<Value>(b) {
                value_hints(&v, &mut hints);
            }
        }
        if !hints.is_empty() {
            // code values, masked date shapes and amount signs only – never amounts or names
            crate::depot_debug::log(&format!(
                "portal value hints: {}",
                serde_json::to_string(&hints).unwrap_or_default()
            ));
        }
        let Capture {
            posted: mut parsed,
            pending,
            pending_listed,
            ..
        } = capture;
        let from_json = parsed.len();
        // The visible table only when the JSON held no bookings at all (it may mix in pending rows).
        if parsed.is_empty() && pending.is_empty() {
            let rows: Vec<Vec<String>> = serde_json::from_str(&dom_rows_json).unwrap_or_default();
            parsed = parse_dom_rows(&rows, kind);
        }
        crate::depot_debug::log(&format!(
            "portal import: {:?} bodies={} json_txs={} total_txs={} pending={} pending_listed={} balance_found={}",
            kind,
            json_bodies.len(),
            from_json,
            parsed.len(),
            pending.len(),
            pending_listed,
            balance.is_some()
        ));
        self.import_parsed(connection_id, parsed, pending, balance)
    }

    /// How many bookings `import_portal_capture` would extract right now (nothing is stored).
    pub fn preview_portal_capture(
        self: Arc<Self>,
        connection_id: i64,
        json_bodies: Vec<String>,
        dom_rows_json: String,
    ) -> Result<u32, CoreError> {
        let (kind, _) = self.portal_connection(connection_id)?;
        let c = parse_capture(&json_bodies, kind);
        // pending charges count only where they would be stored (as holds)
        let holds = if c.balance.is_some() {
            c.pending.len()
        } else {
            0
        };
        let mut n = c.posted.len() + holds;
        if c.posted.is_empty() && c.pending.is_empty() {
            let rows: Vec<Vec<String>> = serde_json::from_str(&dom_rows_json).unwrap_or_default();
            n = parse_dom_rows(&rows, kind).len();
        }
        Ok(n as u32)
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
        self.import_parsed(connection_id, parsed, Vec::new(), None)
    }

    /// Pending charges (#629) the captured portal pages no longer list as open and whose
    /// posted booking was not found: the posted booking may sit in an older billing
    /// period. Empty while the capture holds no posted bookings (portal not loaded yet).
    pub fn portal_open_holds(
        self: Arc<Self>,
        connection_id: i64,
        json_bodies: Vec<String>,
    ) -> Result<Vec<PortalHold>, CoreError> {
        let (kind, name) = self.portal_connection(connection_id)?;
        let c = parse_capture(&json_bodies, kind);
        if c.posted.is_empty() {
            return Ok(Vec::new());
        }
        let Some(account) = self.portal_account(kind, connection_id)? else {
            return Ok(Vec::new());
        };
        // ids the charges still listed as pending have as holds
        let listed: std::collections::HashSet<String> =
            to_fetch(connection_id, &name, kind, Vec::new(), c.pending, Some(0))
                .transactions
                .into_iter()
                .map(|t| t.external_id)
                .collect();
        let db = self.db()?;
        let mut q = db.prepare(&format!(
            "SELECT t.id, t.external_id, t.booking_date, t.amount_cents, t.counterparty FROM transactions t
              WHERE t.account_id = ?1 AND t.deleted_ms IS NULL AND {}
              ORDER BY t.booking_date, t.id",
            hold_sql(kind == ConnectorKind::AmericanExpress)
        ))?;
        let rows = q.query_map([account], |r| {
            Ok((
                r.get::<_, String>(1)?,
                PortalHold {
                    id: r.get(0)?,
                    booking_date: r.get(2)?,
                    amount_cents: r.get(3)?,
                    counterparty: r.get(4)?,
                },
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (ext, hold) = row?;
            let key = if ext.starts_with(HOLD_PREFIX) {
                ext
            } else {
                format!("{HOLD_PREFIX}{ext}")
            };
            if !listed.contains(&key) {
                out.push(hold);
            }
        }
        Ok(out)
    }

    /// Deletes pending charges the user gave up on (`portal_open_holds`); only holds of
    /// this connection's card. The deletion syncs. Returns the number deleted.
    pub fn discard_portal_holds(
        self: Arc<Self>,
        connection_id: i64,
        ids: Vec<i64>,
    ) -> Result<u32, CoreError> {
        let (kind, _) = self.portal_connection(connection_id)?;
        let Some(account) = self.portal_account(kind, connection_id)? else {
            return Ok(0);
        };
        let db = self.db()?;
        let tx = db.unchecked_transaction()?;
        let sql = format!(
            "SELECT 1 FROM transactions t WHERE t.id = ?1 AND t.account_id = ?2 AND t.deleted_ms IS NULL AND {}",
            hold_sql(kind == ConnectorKind::AmericanExpress)
        );
        let mut n = 0u32;
        for id in ids {
            if tx
                .query_row(&sql, params![id, account], |_| Ok(()))
                .optional()?
                .is_some()
            {
                crate::sync_merge::soft_delete_booking(&tx, id)?;
                n += 1;
            }
        }
        tx.commit()?;
        Ok(n)
    }
}

/// Sanitized keys-only structure dump of captured portal JSON (for debugging a changed portal).
pub fn portal_schema_dump(json_bodies: Vec<String>, dom_rows_json: String) -> String {
    let bodies: Vec<Value> = json_bodies
        .iter()
        .filter_map(|b| serde_json::from_str::<Value>(b).ok())
        .map(|v| schema_of(&v, 0))
        .collect();
    let dom: Vec<Vec<String>> = serde_json::from_str(&dom_rows_json).unwrap_or_default();
    let dom_shape: Vec<usize> = dom.iter().map(|r| r.len()).collect();
    let mut hints = std::collections::BTreeMap::new();
    for b in &json_bodies {
        if let Ok(v) = serde_json::from_str::<Value>(b) {
            value_hints(&v, &mut hints);
        }
    }
    serde_json::to_string_pretty(&serde_json::json!({
        "json_responses": bodies,
        "dom_row_cell_counts": dom_shape,
        "value_hints": hints,
    }))
    .unwrap_or_default()
}

/// Letters → `a`, digits → `9` (runs of letters collapsed): the shape of a value, not the value.
fn mask_text(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        let m = if c.is_numeric() {
            '9'
        } else if c.is_alphabetic() {
            'a'
        } else {
            c
        };
        if !(m == 'a' && out.ends_with('a')) {
            out.push(m);
        }
    }
    out
}

/// Personal-data markers in key names: such fields are left out of the value hints.
const HINT_SKIP: &[&str] = &[
    "gender",
    "marital",
    "birth",
    "document",
    "aml",
    "nationality",
    "language",
    "name",
    "address",
    "email",
    "phone",
    "mobile",
    "tax",
    "login",
    "customer",
    "person",
    "occupation",
    "income",
    "residence",
    "country",
];
/// Booking-related key parts whose code values may be logged verbatim.
const HINT_VERBATIM: &[&str] = &[
    "transaction",
    "txn",
    "denial",
    "status",
    "category",
    "currency",
    "payment",
    "epp",
    "indicator",
    "creditdebit",
    "debitcredit",
];

/// JSON object keys can carry data themselves (maps keyed by card number,
/// account id, booking id or date). Only plain identifier-like names (ASCII
/// letters and `_ - . $ @`, at most 48 characters) are shown; anything with
/// digits or other characters is masked like a value.
fn safe_key(k: &str) -> String {
    let plain = !k.is_empty()
        && k.len() <= 48
        && k.chars()
            .all(|c| c.is_ascii_alphabetic() || matches!(c, '_' | '-' | '.' | '$' | '@'));
    if plain {
        k.to_string()
    } else {
        format!("<{}>", mask_text(k))
    }
}

/// Code values shown verbatim: short, letters-only enums (`BOOK`, `PDNG`,
/// `DBIT`, `D`, `SETTLED` …). Anything with digits (references, ids) or
/// longer than 16 characters is masked.
fn is_enum_value(s: &str) -> bool {
    !s.is_empty() && s.len() <= 16 && s.chars().all(|c| c.is_ascii_uppercase() || c == '_')
}

/// Value hints that are safe to share: the distinct values of code-like fields
/// (`…Code`, `…Status`, `…Type`, `…Indicator`; only letters-only enums of
/// booking-related keys, anything else is masked), masked shapes of date/time
/// fields and sign counts of amount/balance numbers. Keys go through [`safe_key`].
fn value_hints(
    v: &Value,
    out: &mut std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
) {
    match v {
        Value::Object(o) => {
            for (k, x) in o {
                let kl = norm_key(k);
                // customer-profile fields never appear, not even masked
                if HINT_SKIP.iter().any(|s| kl.contains(s)) {
                    continue;
                }
                let codeish = ["code", "status", "type", "indicator"]
                    .iter()
                    .any(|s| kl.ends_with(s));
                // only booking-related codes verbatim; any other code is masked
                let booking_code = HINT_VERBATIM.iter().any(|s| kl.contains(s));
                let dateish = kl.contains("date") || kl.contains("time");
                let moneyish = ["amount", "balance", "limit", "debt"]
                    .iter()
                    .any(|s| kl.contains(s));
                let hint = match x {
                    Value::String(s) if codeish => Some(if booking_code && is_enum_value(s) {
                        s.clone()
                    } else {
                        mask_text(s)
                    }),
                    Value::String(s) if dateish => Some(mask_text(s)),
                    Value::Number(n) if moneyish => Some(
                        match n.as_f64().unwrap_or(0.0) {
                            f if f < 0.0 => "negative",
                            f if f > 0.0 => "positive",
                            _ => "zero",
                        }
                        .to_string(),
                    ),
                    _ => None,
                };
                if let Some(h) = hint {
                    let set = out.entry(safe_key(k)).or_default();
                    if set.len() < 30 {
                        set.insert(h);
                    }
                }
                value_hints(x, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| value_hints(x, out)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn extreme_json_amounts_are_rejected_not_saturated() {
        use serde_json::json;
        assert_eq!(super::value_cents(&json!(12.5)), Some(1250));
        assert_eq!(super::value_cents(&json!(1e30)), None);
        assert_eq!(super::value_cents(&json!(-1e30)), None);
        assert_eq!(super::value_cents(&json!(9.3e16)), None);
    }

    #[test]
    fn json_amounts_round_from_their_text_not_from_f64() {
        use serde_json::json;
        // 1.005 * 100.0 is 100.4999… in f64
        assert_eq!(super::value_cents(&json!(1.005)), Some(101));
        assert_eq!(super::value_cents(&json!(-0.285)), Some(-29));
        assert_eq!(super::value_cents(&json!(42)), Some(4200));
        assert_eq!(super::value_cents(&json!(1e10)), Some(1_000_000_000_000));
    }

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
    fn amex_paid_statement_leaves_a_zero_balance() {
        // the last statement is paid by direct debit: nothing is owed any more
        let b = r#"{"cash_advance_balance_amount":0,"charges_amount":0,"debits_balance_amount":0,"disputed_amount":0,"last_statement_balance_amount":123.45,"payment_due_amount":0,"payments_credits_amount":-123.45,"remaining_statement_balance_amount":0,"statement_balance_amount":0,"total_amount":0,"total_debits_balance_amount":0,"total_payments_credits_amount":-123.45}"#;
        let (_, bal) =
            super::parse_json_bodies(&[b.to_string()], super::ConnectorKind::AmericanExpress);
        assert_eq!(bal, Some(0));
        // a lone zero outside a balance summary is still no balance
        let lone = r#"{"statement_balance_amount":0,"last_statement_balance_amount":123.45}"#;
        let (_, bal) =
            super::parse_json_bodies(&[lone.to_string()], super::ConnectorKind::AmericanExpress);
        assert_eq!(bal, Some(-12345));
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
        // #629: pending charges are never regular bookings, not even without posted ones
        let only_pending = r#"{"pending_transactions":[{"charge_date":"2025-04-05","amount":9.9,"description":"X","identifier":"P1"}]}"#;
        assert!(
            parse_json_bodies(&[only_pending.to_string()], ConnectorKind::AmericanExpress)
                .0
                .is_empty()
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
    fn openbank_card_transactions_shape() {
        // shape of kunden.openbankpay.com …/cards/transactions (invented values)
        let page1 = r#"{"elements":[{"id":"T1","date":"2025-04-03","dateTime":"2025-04-03T10:15:00Z","description":"BEISPIEL SHOP","amounts":[{"currency":"EUR","totalAmount":-19.99}],"merchantDetails":{"id":"M1","name":"Beispiel Shop"},"transactionStatusCode":"X"},{"id":"T2","date":"2025-04-04","description":"GUTSCHRIFT","amounts":[{"currency":"EUR","totalAmount":50}]}],"hasMultipleCards":false,"metadata":{"hasPendingRecords":false,"nextCursor":"c1"}}"#;
        // next page overlaps by one booking
        let page2 = r#"{"elements":[{"id":"T2","date":"2025-04-04","description":"GUTSCHRIFT","amounts":[{"currency":"EUR","totalAmount":50}]},{"id":"T3","date":"2025-03-30","description":"ANDERER SHOP","amounts":[{"currency":"EUR","totalAmount":-5.5}]}],"metadata":{"hasPendingRecords":false}}"#;
        let position = r#"{"cards":[{"currentBalance":-123.45,"availableBalance":876.55}]}"#;
        let (p, bal) = parse_json_bodies(
            &[page1.into(), page2.into(), position.into()],
            ConnectorKind::AmazonVisa,
        );
        assert_eq!(p.len(), 3, "overlapping page deduplicated by id");
        assert_eq!(
            (
                p[0].date.as_str(),
                p[0].cents,
                p[0].text.as_str(),
                p[0].id.as_deref()
            ),
            ("2025-04-03", -1999, "BEISPIEL SHOP", Some("T1"))
        );
        assert_eq!(p[1].cents, 5000);
        assert_eq!(p[2].cents, -550);
        assert_eq!(bal, Some(-12345));
    }

    #[test]
    fn schema_dump_value_hints_hold_no_values() {
        let d = portal_schema_dump(
            vec![r#"{"elements":[{"date":"2025-04-03","transactionStatusCode":"SETTLED","denialCode":"Karte gesperrt 12","amounts":[{"totalAmount":-19.99}],"description":"SECRET SHOP"}]}"#.into()],
            "[]".into(),
        );
        assert!(d.contains("SETTLED") && d.contains("9999-99-99") && d.contains("negative"));
        assert!(
            !d.contains("SECRET")
                && !d.contains("19.99")
                && !d.contains("gesperrt")
                && !d.contains("2025")
        );
    }

    #[test]
    fn value_hints_leave_out_profile_fields() {
        let d = portal_schema_dump(
            vec![r#"{"genderTCode":"OTHER","maritalStatus":"SINGLE","documentType":"DA","productTypeCode":"ABC","transactionTypeCode":"D"}"#.into()],
            "[]".into(),
        );
        assert!(!d.contains("OTHER") && !d.contains("SINGLE") && !d.contains("\"DA\""));
        assert!(!d.contains("\"ABC\""), "non-booking codes are masked");
        assert!(d.contains("\"D\""), "booking codes stay readable");
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
    fn pdf_lines_without_year_before_the_statement_date() {
        let dates = |t: &str| -> Vec<String> {
            parse_statement_text(t, ConnectorKind::AmericanExpress)
                .into_iter()
                .map(|p| p.date)
                .collect()
        };
        assert_eq!(
            dates("Abrechnung vom 05.01.2026\n28.12. Hotel 120,00\n03.01. Taxi 12,00\n"),
            ["2025-12-28", "2026-01-03"]
        );
        // A period names its start first; the end date decides.
        assert_eq!(
            dates("Zeitraum 01.12.2025 - 05.01.2026\n28.12. Hotel 120,00\n03.01. Taxi 12,00\n"),
            ["2025-12-28", "2026-01-03"]
        );
        assert_eq!(
            dates("Abrechnung vom 15.12.2025\n01.12. Hotel 120,00\n15.12. Taxi 12,00\n"),
            ["2025-12-01", "2025-12-15"]
        );
        assert_eq!(
            dates("Abrechnung vom 05.01.2026\n28.12. Hotel 120,00\nZinsänderung ab 01.04.2099\n"),
            ["2025-12-28"]
        );
    }

    #[test]
    fn dom_rows() {
        let rows = vec![vec!["03.04.2025".into(), "REWE".into(), "23,50 €".into()]];
        let p = parse_dom_rows(&rows, ConnectorKind::AmazonVisa);
        assert_eq!((p[0].cents, p[0].text.as_str()), (2350, "REWE"));
    }
}

#[cfg(test)]
mod privacy_tests {
    use super::*;

    #[test]
    fn data_carrying_keys_are_masked() {
        assert_eq!(
            safe_key("statement_balance_amount"),
            "statement_balance_amount"
        );
        assert_eq!(safe_key("4871780012341234"), "<9999999999999999>");
        assert_eq!(safe_key("2026-11-07"), "<9999-99-99>");
        assert_eq!(
            safe_key("DE89370400440532013000"),
            format!("<a{}>", "9".repeat(20))
        );
    }

    #[test]
    fn schema_dump_hides_ids_in_keys_and_values() {
        let d = portal_schema_dump(
            vec![r#"{"cards":{"4871780012341234":{"transactionStatus":"BOOKED","transactionReferenceCode":"AB12CD34EF"}}}"#.into()],
            "[]".into(),
        );
        assert!(!d.contains("4871780012341234"), "{d}");
        assert!(!d.contains("AB12CD34EF"), "{d}");
        assert!(
            d.contains("BOOKED"),
            "letters-only status stays readable: {d}"
        );
    }
}

/// #629: pending card charges of the web portal become holds that are settled by
/// their posted booking or discarded by the user, never dropped silently. Invented data.
#[cfg(test)]
mod hold_tests {
    use super::*;

    const KIND: ConnectorKind = ConnectorKind::AmericanExpress;

    fn vault() -> (Arc<Vault>, i64, crate::test_support::TempDir) {
        let dir = crate::test_support::TempDir::new("girocrab-portal-holds");
        let v = Vault::open(
            dir.join("v.db").to_string_lossy().to_string(),
            vec![7u8; 32],
        )
        .unwrap();
        let conn = v
            .add_bank_connection(KIND, "Karte".into(), "max".into())
            .unwrap();
        (v, conn, dir)
    }

    /// external ids of the live bookings, by date.
    fn ids(v: &Vault) -> Vec<String> {
        let db = v.db().unwrap();
        let mut q = db
            .prepare(
                "SELECT external_id FROM transactions WHERE deleted_ms IS NULL
                  ORDER BY booking_date, external_id",
            )
            .unwrap();
        q.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn posted(id: &str, date: &str, amount: &str, text: &str) -> String {
        format!(
            r#"{{"post_date":"{date}","billing_amount":"{amount}","description":"{text}","reference_number":"{id}"}}"#
        )
    }

    fn pending(id: &str, date: &str, amount: &str, text: &str) -> String {
        format!(
            r#"{{"charge_date":"{date}","amount":"{amount}","description":"{text}","identifier":"{id}"}}"#
        )
    }

    fn body(posted: &[String], pending: Option<&[String]>) -> String {
        let mut b = format!(
            r#"{{"summary":{{"total_balance":{{"amount":"50,00"}}}},"transactions":[{}]"#,
            posted.join(",")
        );
        if let Some(p) = pending {
            b.push_str(&format!(r#","pending_transactions":[{}]"#, p.join(",")));
        }
        b.push('}');
        b
    }

    fn sync(v: &Arc<Vault>, conn: i64, body: &str) -> Result<SyncSummary, CoreError> {
        v.clone()
            .import_portal_capture(conn, vec![body.to_string()], "[]".into())
    }

    fn open_holds(v: &Arc<Vault>, conn: i64, body: &str) -> Vec<String> {
        v.clone()
            .portal_open_holds(conn, vec![body.to_string()])
            .unwrap()
            .into_iter()
            .map(|h| format!("{} {}", h.booking_date, h.amount_cents))
            .collect()
    }

    fn sql(v: &Vault, sql: &str) {
        v.db().unwrap().execute_batch(sql).unwrap();
    }

    /// Two parts of an imported split booking (#871), as the database import stores them.
    fn imported_parts(v: &Vault, reference: &str, a: i64, b: i64, checked: bool) {
        sql(
            v,
            &format!(
                "INSERT INTO transactions(account_id, external_id, booking_date, value_date, amount_cents,
                        currency, counterparty, counterparty_iban, purpose, booking_text, note, checked, category_id)
                 SELECT a.id, 'mm-' || x.k, '2026-01-05', '2026-01-05', x.c, 'EUR', 'SHOP', '',
                        'SHOP, CITY EREF: {reference}', '', x.n, {checked},
                        (SELECT min(id) FROM categories WHERE deleted_ms IS NULL)
                   FROM accounts a, (SELECT 'aaaa' k, {a} c, 'Teil eins' n UNION ALL SELECT 'bbbb', {b}, '') x",
                checked = i64::from(checked)
            ),
        );
    }

    fn split_lines(v: &Vault, external_id: &str) -> Vec<(i64, String)> {
        let db = v.db().unwrap();
        let mut q = db
            .prepare(
                "SELECT s.amount_cents, s.note FROM transaction_splits s JOIN transactions t ON t.id = s.transaction_id
                  WHERE t.external_id = ?1 ORDER BY s.id",
            )
            .unwrap();
        q.query_map([external_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    #[test]
    fn imported_split_parts_become_a_split_booking() {
        let (v, conn, _dir) = vault();
        let old = posted("R1", "2026-01-02", "10,00", "SHOP A");
        sync(&v, conn, &body(std::slice::from_ref(&old), None)).unwrap();
        imported_parts(&v, "AT260000000000000000001", -4996, -3999, true);
        let whole = posted(
            "AT260000000000000000001---0005---20260105",
            "2026-01-05",
            "89,95",
            "SHOP   CITY",
        );
        let b = body(&[old, whole], None);
        sync(&v, conn, &b).unwrap();
        let bank = "id:AT260000000000000000001---0005---20260105";
        assert_eq!(
            ids(&v),
            ["id:R1", bank],
            "the parts are gone, the booking is there once"
        );
        assert_eq!(
            split_lines(&v, bank),
            [(-4996, "Teil eins".to_string()), (-3999, String::new())]
        );
        let (cat, checked): (Option<i64>, bool) = v
            .db()
            .unwrap()
            .query_row(
                "SELECT category_id, checked FROM transactions WHERE external_id = ?1",
                [bank],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (cat, checked),
            (None, true),
            "shown by its parts, checked like them"
        );
        // idempotent: a further sync changes nothing
        sync(&v, conn, &b).unwrap();
        assert_eq!(split_lines(&v, bank).len(), 2);
    }

    #[test]
    fn imported_parts_that_do_not_add_up_stay() {
        let (v, conn, _dir) = vault();
        let old = posted("R1", "2026-01-02", "10,00", "SHOP A");
        sync(&v, conn, &body(std::slice::from_ref(&old), None)).unwrap();
        imported_parts(&v, "AT260000000000000000001", -5000, -3999, false);
        let whole = posted(
            "AT260000000000000000001---0005---20260105",
            "2026-01-05",
            "89,95",
            "SHOP   CITY",
        );
        sync(&v, conn, &body(&[old, whole], None)).unwrap();
        assert_eq!(ids(&v).len(), 4, "not certain: nothing merged");
        assert!(split_lines(&v, "id:AT260000000000000000001---0005---20260105").is_empty());
    }

    #[test]
    fn eref_references() {
        assert_eq!(
            eref_of("HM.COM E-COMMERCE, HAMBURG EREF: AT262510045000010029820"),
            Some("AT262510045000010029820")
        );
        assert_eq!(eref_of("X EREF: AT2600000001, MORE"), Some("AT2600000001"));
        assert_eq!(eref_of("EREF: NOTPROVIDED"), Some("NOTPROVIDED"));
        assert_eq!(eref_of("EREF: 123"), None);
        assert_eq!(eref_of("no reference"), None);
    }

    #[test]
    fn amex_pending_charges_in_the_posted_list_are_recognised() {
        // Amex lists open charges in a plain `transactions` list (no `pending…` key)
        let b = body(
            &[
                posted("AT000000000000000000001", "2026-01-02", "10,00", "SHOP A"),
                posted("P0000000000000000000001", "2026-01-05", "1,87", "SHOP B"),
                posted("10000000000000000000001", "2026-01-03", "-24,09", "ZAHLUNG"),
            ],
            None,
        );
        let c = parse_capture(std::slice::from_ref(&b), KIND);
        assert_eq!(
            c.pending
                .iter()
                .map(|p| p.id.as_deref())
                .collect::<Vec<_>>(),
            [Some("P0000000000000000000001")]
        );
        assert_eq!(c.posted.len(), 2);
        // other portals: a `P…` id is just an id
        let c = parse_capture(&[b], ConnectorKind::AmazonVisa);
        assert!(c.pending.is_empty());
        // an explicit status counts everywhere
        let s = r#"{"items":[{"date":"2026-01-05","amount":-5,"description":"X","id":"T1","status":"PENDING"}]}"#;
        let c = parse_capture(&[s.to_string()], ConnectorKind::AmazonVisa);
        assert_eq!(c.pending.len(), 1);
    }

    #[test]
    fn a_charge_pending_in_this_sync_is_never_offered_for_discarding() {
        // regression: it was stored as `id:P…`, turned into a hold and reported unresolved
        let (v, conn, _dir) = vault();
        let b = body(
            &[
                posted("AT1", "2026-01-02", "10,00", "SHOP A"),
                posted("P0000000000000000000001", "2026-01-05", "4,05", "SHOP B"),
            ],
            None,
        );
        sync(&v, conn, &b).unwrap();
        assert_eq!(
            ids(&v),
            [
                "id:AT1",
                &format!("{HOLD_PREFIX}id:P0000000000000000000001")
            ]
        );
        assert!(
            open_holds(&v, conn, &b).is_empty(),
            "still open, nothing to ask"
        );
        // a vault hit before the fix (tombstoned `id:P…` + hold) heals on the next sync
        sync(&v, conn, &b).unwrap();
        assert!(open_holds(&v, conn, &b).is_empty());
        assert_eq!(ids(&v).len(), 2);
    }

    #[test]
    fn capture_separates_pending_and_notes_the_list() {
        let old = posted("R1", "2026-01-02", "10,00", "SHOP A");
        let c = parse_capture(&[body(std::slice::from_ref(&old), None)], KIND);
        assert_eq!(
            (c.posted.len(), c.pending.len(), c.pending_listed),
            (1, 0, false)
        );
        let c = parse_capture(&[body(std::slice::from_ref(&old), Some(&[]))], KIND);
        assert!(c.pending_listed, "an empty pending list counts as listed");
        let p = pending("P1", "2026-01-05", "1,87", "SHOP B");
        let c = parse_capture(&[body(&[old], Some(&[p]))], KIND);
        assert_eq!((c.posted.len(), c.pending.len()), (1, 1));
        assert_eq!(c.pending[0].id.as_deref(), Some("P1"));
    }

    #[test]
    fn pending_charge_is_a_hold_until_its_posted_booking_arrives() {
        let (v, conn, _dir) = vault();
        let old = posted("R1", "2026-01-02", "10,00", "SHOP A");
        let p = pending("P1", "2026-01-05", "1,87", "SHOP B");
        let day1 = body(std::slice::from_ref(&old), Some(&[p]));
        sync(&v, conn, &day1).unwrap();
        assert_eq!(ids(&v), ["id:R1", &format!("{HOLD_PREFIX}id:P1")]);
        assert!(
            open_holds(&v, conn, &day1).is_empty(),
            "still listed as pending"
        );
        sql(
            &v,
            "UPDATE transactions SET note = 'Notiz', checked = 1 WHERE external_id LIKE 'pending-%';
             -- older than the generic 14-day expiry of pending rows: must not matter
             UPDATE transactions SET imported_unix = imported_unix - 30 * 86400;",
        );
        // neither listed nor posted (the posting is in another billing period)
        let day3 = body(std::slice::from_ref(&old), Some(&[]));
        sync(&v, conn, &day3).unwrap();
        assert_eq!(ids(&v).len(), 2, "never dropped silently");
        assert_eq!(open_holds(&v, conn, &day3), ["2026-01-05 -187"]);
        // the older period holds the posting: a day before the charge, longer text
        let older = body(
            &[old, posted("AT9", "2026-01-04", "1,87", "SHOP B   CITY")],
            Some(&[]),
        );
        sync(&v, conn, &older).unwrap();
        assert_eq!(ids(&v), ["id:R1", "id:AT9"]);
        assert!(open_holds(&v, conn, &older).is_empty());
        let (note, checked): (String, bool) = v
            .db()
            .unwrap()
            .query_row(
                "SELECT note, checked FROM transactions WHERE external_id = 'id:AT9'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((note.as_str(), checked), ("Notiz", true));
    }

    #[test]
    fn an_older_posting_with_the_same_amount_does_not_settle_a_hold() {
        let (v, conn, _dir) = vault();
        // posted before the charge was even pending: a different purchase
        let same = posted("AT1", "2026-01-05", "1,87", "SHOP B");
        sync(&v, conn, &body(std::slice::from_ref(&same), Some(&[]))).unwrap();
        let p = pending("P1", "2026-01-05", "1,87", "SHOP B");
        sync(&v, conn, &body(&[same], Some(&[p]))).unwrap();
        assert_eq!(ids(&v).len(), 2);
    }

    #[test]
    fn a_reduced_pre_authorisation_settles_by_merchant() {
        let (v, conn, _dir) = vault();
        let old = posted("R1", "2026-01-02", "10,00", "SHOP A");
        let p = pending("P1", "2026-01-05", "100,00", "ARAL STATION 12");
        sync(&v, conn, &body(std::slice::from_ref(&old), Some(&[p]))).unwrap();
        let b = body(
            &[
                old,
                posted("AT9", "2026-01-06", "72,54", "ARAL PAYBACK   CITY"),
            ],
            Some(&[]),
        );
        sync(&v, conn, &b).unwrap();
        assert_eq!(ids(&v), ["id:R1", "id:AT9"]);
        assert!(open_holds(&v, conn, &b).is_empty());
    }

    #[test]
    fn two_fitting_merchant_postings_settle_nothing() {
        let (v, conn, _dir) = vault();
        let old = posted("R1", "2026-01-02", "10,00", "SHOP A");
        let p = pending("P1", "2026-01-05", "100,00", "ARAL STATION 12");
        sync(&v, conn, &body(std::slice::from_ref(&old), Some(&[p]))).unwrap();
        let b = body(
            &[
                old,
                posted("AT8", "2026-01-05", "40,00", "ARAL STATION 12"),
                posted("AT9", "2026-01-06", "72,54", "ARAL STATION 12"),
            ],
            Some(&[]),
        );
        sync(&v, conn, &b).unwrap();
        assert_eq!(
            open_holds(&v, conn, &b),
            ["2026-01-05 -10000"],
            "ambiguous: ask the user"
        );
    }

    #[test]
    fn merchant_keys() {
        assert_eq!(
            merchant_key("ARAL PAYBACK PAY  BOCHUM").as_deref(),
            Some("aral")
        );
        assert_eq!(merchant_key("LIDL 2038").as_deref(), Some("lidl"));
        assert_eq!(merchant_key("DM-DROGERIE").as_deref(), Some("dmdrogerie"));
        assert_eq!(merchant_key("H&M").as_deref(), None);
        assert_eq!(merchant_key("").as_deref(), None);
    }

    #[test]
    fn discarding_removes_only_holds() {
        let (v, conn, _dir) = vault();
        let old = posted("R1", "2026-01-02", "10,00", "SHOP A");
        let p = pending("P1", "2026-01-05", "1,87", "SHOP B");
        sync(&v, conn, &body(std::slice::from_ref(&old), Some(&[p]))).unwrap();
        let gone = body(&[old], Some(&[]));
        let all: Vec<i64> = v
            .db()
            .unwrap()
            .prepare("SELECT id FROM transactions")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(v.clone().discard_portal_holds(conn, all).unwrap(), 1);
        assert_eq!(ids(&v), ["id:R1"]);
        assert!(open_holds(&v, conn, &gone).is_empty());
        // deleted holds stay deleted, the posted booking still arrives later
        sync(&v, conn, &gone).unwrap();
        assert_eq!(ids(&v), ["id:R1"]);
    }

    #[test]
    fn open_holds_wait_for_posted_bookings() {
        let (v, conn, _dir) = vault();
        let p = pending("P1", "2026-01-05", "1,87", "SHOP B");
        sync(
            &v,
            conn,
            &body(&[posted("R1", "2026-01-02", "10,00", "SHOP A")], Some(&[p])),
        )
        .unwrap();
        // login page / summary only: nothing posted captured yet, no verdict
        assert!(open_holds(&v, conn, &body(&[], Some(&[]))).is_empty());
    }

    #[test]
    fn holds_need_a_reported_balance() {
        let (v, conn, _dir) = vault();
        let p = pending("P1", "2026-01-05", "1,87", "SHOP B");
        let no_balance = format!(r#"{{"pending_transactions":[{p}]}}"#);
        assert_eq!(
            v.clone()
                .preview_portal_capture(conn, vec![no_balance.clone()], "[]".into())
                .unwrap(),
            0
        );
        assert!(sync(&v, conn, &no_balance).is_err(), "nothing to store");
        assert!(ids(&v).is_empty());
    }

    #[test]
    fn hold_never_adopts_an_imported_row() {
        let (v, conn, _dir) = vault();
        let old = posted("R1", "2026-01-02", "10,00", "SHOP A");
        sync(&v, conn, &body(std::slice::from_ref(&old), Some(&[]))).unwrap();
        sql(
            &v,
            "INSERT INTO transactions(account_id, external_id, booking_date, value_date, amount_cents,
                    currency, counterparty, counterparty_iban, purpose, booking_text)
             SELECT id, 'mm-0123456789abcdef', '2026-01-05', '2026-01-05', -187, 'EUR', 'SHOP B', '', '', ''
               FROM accounts",
        );
        let p = pending("P1", "2026-01-05", "1,87", "SHOP B");
        let b = body(&[old], Some(&[p]));
        sync(&v, conn, &b).unwrap();
        sync(&v, conn, &b).unwrap();
        let ids = ids(&v);
        assert!(
            ids.contains(&"mm-0123456789abcdef".to_string())
                && ids.contains(&format!("{HOLD_PREFIX}id:P1")),
            "imported row and hold side by side: {ids:?}"
        );
    }

    #[test]
    fn pre_629_copies_are_settled_like_holds() {
        let (v, conn, _dir) = vault();
        let old = posted("R1", "2026-01-02", "10,00", "SHOP A");
        sync(&v, conn, &body(std::slice::from_ref(&old), Some(&[]))).unwrap();
        // what versions before #629 stored for a pending-only capture
        sql(
            &v,
            "INSERT INTO transactions(account_id, external_id, booking_date, value_date, amount_cents,
                    currency, counterparty, counterparty_iban, purpose, booking_text, note, checked)
             SELECT id, 'id:P1', '2026-01-05', '2026-01-05', -187, 'EUR', 'SHOP', '', '', 'Kartenumsatz', 'Notiz', 1
               FROM accounts;
             INSERT INTO transactions(account_id, external_id, booking_date, value_date, amount_cents,
                    currency, counterparty, counterparty_iban, purpose, booking_text)
             SELECT id, 'id:P2', '2026-01-06', '2026-01-06', -999, 'EUR', 'SHOP', '', '', 'Kartenumsatz'
               FROM accounts;
             INSERT INTO transactions(account_id, external_id, booking_date, value_date, amount_cents,
                    currency, counterparty, counterparty_iban, purpose, booking_text)
             SELECT id, 'id:P3', '2026-01-07', '2026-01-07', -500, 'EUR', 'SHOP', '', '', 'Kartenumsatz'
               FROM accounts;",
        );
        // P1 posts under a new id; P2 is gone without a posting; P3 is still pending
        let b = body(
            &[old, posted("AT9", "2026-01-07", "1,87", "SHOP   CITY")],
            Some(&[pending("P3", "2026-01-07", "5,00", "SHOP")]),
        );
        sync(&v, conn, &b).unwrap();
        assert_eq!(
            ids(&v),
            [
                "id:R1",
                &format!("{HOLD_PREFIX}id:P2"),
                "id:AT9",
                &format!("{HOLD_PREFIX}id:P3")
            ],
            "P1 settled, P3 merged into its hold"
        );
        assert_eq!(open_holds(&v, conn, &b), ["2026-01-06 -999"]);
        // the open copy P2 became a hold (shown as pending); the old row is a tombstone
        let db = v.db().unwrap();
        let (text, old_deleted): (String, bool) = db
            .query_row(
                "SELECT h.booking_text, o.deleted_ms IS NOT NULL FROM transactions h, transactions o
                  WHERE h.external_id = 'pending-hold-portal-id:P2' AND o.external_id = 'id:P2'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (text.as_str(), old_deleted),
            ("Kartenumsatz (vorgemerkt)", true)
        );
        drop(db);
        let note: String = v
            .db()
            .unwrap()
            .query_row(
                "SELECT note FROM transactions WHERE external_id = 'id:AT9'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(note, "Notiz");
    }
}

/// Several cards and partner cards behind one portal login (#243).
#[cfg(test)]
mod card_tests {
    use super::*;

    #[test]
    fn amex_partner_cards_are_named() {
        // bookings carry `supplementary_index`; names come from the card-member list
        let b = r#"{"card_member_totals":[
                {"supplementary_index":"00","embossed_name":"MAX MUSTERMANN","account_number":"XXXX-XXXXXX-11111"},
                {"supplementary_index":"01","embossed_name":"ERIKA MUSTERMANN","account_number":"XXXX-XXXXXX-22222"}],
            "transactions":[
                {"charge_date":"2026-11-02","amount":12.5,"description":"REWE","identifier":"T1","supplementary_index":"00"},
                {"charge_date":"2026-11-03","amount":7,"description":"BÄCKER","identifier":"T2","supplementary_index":"01"},
                {"charge_date":"2026-11-04","amount":3,"description":"KIOSK","identifier":"T3","supplementary_index":"02"},
                {"charge_date":"2026-11-05","amount":4,"description":"CAFE","identifier":"T4","embossed_name":"Erika Mustermann"},
                {"charge_date":"2026-11-06","amount":5,"description":"BAR","identifier":"T5"}]}"#;
        let (p, _) = parse_json_bodies(&[b.to_string()], ConnectorKind::AmericanExpress);
        let cards: Vec<Option<&str>> = p.iter().map(|t| t.card.as_deref()).collect();
        assert_eq!(
            cards,
            vec![
                Some("Max Mustermann"),
                Some("Erika Mustermann"),
                Some("Karte 02"),
                Some("Erika Mustermann"),
                None,
            ]
        );
    }

    #[test]
    fn card_label_is_the_name_without_card_number() {
        let o = serde_json::json!({"first_name":"MAX","last_name":"MUSTERMANN","display_account_number":"374912345612345"});
        assert_eq!(
            card_label(o.as_object().unwrap()).as_deref(),
            Some("Max Mustermann")
        );
    }

    #[test]
    fn portal_import_names_card_and_backfills_old_bookings() {
        let dir = crate::test_support::TempDir::new("girocrab-amex-cards");
        let v = Vault::open(
            dir.join("v.db").to_string_lossy().to_string(),
            vec![3u8; 32],
        )
        .unwrap();
        let conn = v
            .add_bank_connection(ConnectorKind::AmericanExpress, String::new(), "Amex".into())
            .unwrap();
        let body = |card: &str| {
            format!(
                r#"{{"transactions":[{{"charge_date":"2026-11-03","amount":19.99,"description":"PAYPAL *HAENDLER 1234567890","identifier":"T1"{card}}}]}}"#
            )
        };
        // first sync: no card fields yet (as before #243)
        v.clone()
            .import_portal_capture(conn, vec![body("")], "[]".into())
            .unwrap();
        // later sync with the card: the existing row gets it, no second row
        v.clone()
            .import_portal_capture(
                conn,
                vec![body(r#","embossed_name":"ERIKA MUSTERMANN""#)],
                "[]".into(),
            )
            .unwrap();
        let rows: Vec<String> = {
            let db = v.db().unwrap();
            let mut st = db
                .prepare("SELECT booking_text FROM transactions WHERE deleted_ms IS NULL AND counterparty LIKE 'PAYPAL%'")
                .unwrap();
            st.query_map([], |r| r.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        assert_eq!(rows, vec!["Kartenumsatz · Erika Mustermann".to_string()]);
        assert_eq!(
            crate::merchants::payment_method(&rows[0], "", AccountKind::CreditCard, -1999, false),
            "Erika Mustermann"
        );
    }

    #[test]
    fn a_paid_statement_brings_the_stored_card_balance_to_zero() {
        let dir = crate::test_support::TempDir::new("girocrab-amex-paid");
        let v = Vault::open(
            dir.join("v.db").to_string_lossy().to_string(),
            vec![3u8; 32],
        )
        .unwrap();
        let conn = v
            .add_bank_connection(ConnectorKind::AmericanExpress, String::new(), "Amex".into())
            .unwrap();
        let summary = |statement: f64, last: f64, payments: f64| {
            format!(
                r#"{{"cash_advance_balance_amount":0,"debits_balance_amount":0,"last_statement_balance_amount":{last},"remaining_statement_balance_amount":{statement},"statement_balance_amount":{statement},"total_payments_credits_amount":{payments}}}"#
            )
        };
        let tx = r#"{"transactions":[{"charge_date":"2026-10-03","amount":10,"description":"REWE","identifier":"A1"}]}"#;
        let paid = r#"{"transactions":[{"charge_date":"2026-11-12","amount":-123.45,"description":"ZAHLUNG/ÜBERWEISUNG ERHALTEN BESTEN DANK","identifier":"A2"}]}"#;
        // stored and shown: a card stored with 0 shows its open amount, here
        // nothing after the payment (the older charge is outside the fetch)
        let balance = |v: &Vault| -> (i64, i64) {
            let stored = v
                .db()
                .unwrap()
                .query_row(
                    "SELECT balance_cents FROM accounts WHERE deleted_ms IS NULL",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            (stored, v.list_accounts().unwrap()[0].balance_cents)
        };
        // a statement of 123.45 is open
        v.clone()
            .import_portal_capture(
                conn,
                vec![tx.into(), summary(123.45, 123.45, 0.0)],
                "[]".into(),
            )
            .unwrap();
        assert_eq!(balance(&v), (-12345, -12345));
        // paid by direct debit: nothing is owed any more
        v.clone()
            .import_portal_capture(
                conn,
                vec![paid.into(), summary(0.0, 123.45, -123.45)],
                "[]".into(),
            )
            .unwrap();
        assert_eq!(balance(&v), (0, 0));
    }

    #[test]
    fn second_card_gets_its_own_account_after_the_choice() {
        let dir = crate::test_support::TempDir::new("girocrab-amex-two-cards");
        let v = Vault::open(
            dir.join("v.db").to_string_lossy().to_string(),
            vec![3u8; 32],
        )
        .unwrap();
        let conn = v
            .add_bank_connection(ConnectorKind::AmericanExpress, String::new(), "Amex".into())
            .unwrap();
        let tx = |id: &str, text: &str| {
            format!(
                r#"{{"transactions":[{{"charge_date":"2026-11-03","amount":10,"description":"{text}","identifier":"{id}"}}]}}"#
            )
        };
        let rows = |v: &Vault, account_ref: &str| -> Vec<String> {
            let db = v.db().unwrap();
            let mut q = db
                .prepare(
                    "SELECT t.external_id FROM transactions t JOIN accounts a ON a.id = t.account_id
                      WHERE a.external_ref = ?1 AND t.deleted_ms IS NULL ORDER BY t.external_id",
                )
                .unwrap();
            q.query_map([account_ref], |r| r.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        let base = account_ref(ConnectorKind::AmericanExpress, conn);
        // before: one card, no card keys
        v.clone()
            .import_portal_capture(conn, vec![tx("A1", "REWE")], "[]".into())
            .unwrap();
        // now both cards; the business card's responses come first
        let cards = vec![
            PortalCard {
                key: "aaaa".into(),
                name: "American Express® Card".into(),
                number: "11111".into(),
            },
            PortalCard {
                key: "bbbb".into(),
                name: "Amazon Business American Express® Card".into(),
                number: "22222".into(),
            },
        ];
        v.clone()
            .import_portal_capture_cards(
                conn,
                vec![tx("B1", "OFFICE"), tx("A1", "REWE"), tx("A2", "BAKERY")],
                vec!["bbbb".into(), "aaaa".into(), "aaaa".into()],
                cards.clone(),
                "[]".into(),
            )
            .unwrap();
        // the stored card keeps its account, the new card waits for the choice
        assert_eq!(rows(&v, &base), vec!["id:A1", "id:A2"]);
        let new_ref = format!("{base}-bbbb");
        assert!(rows(&v, &new_ref).is_empty());
        let choices = v.connection_account_choices(conn).unwrap();
        let new_card = choices.iter().find(|c| c.account_ref == new_ref).unwrap();
        assert!(new_card.pending && new_card.existing_connection.is_empty());
        assert_eq!(
            (new_card.name.as_str(), new_card.number.as_str()),
            ("Amazon Business American Express® Card", "22222")
        );
        assert_eq!(
            v.pending_account_selection_connections().unwrap(),
            vec![conn]
        );
        // ticked: its bookings are imported into its own account
        v.clone()
            .set_connection_account_choices(conn, vec![base.clone(), new_ref.clone()])
            .unwrap();
        assert_eq!(rows(&v, &new_ref), vec!["id:B1"]);
        assert_eq!(rows(&v, &base), vec!["id:A1", "id:A2"]);
        // a later sync with both cards asks nothing and imports into both
        v.clone()
            .import_portal_capture_cards(
                conn,
                vec![tx("A3", "CAFE"), tx("B2", "PAPER")],
                vec!["aaaa".into(), "bbbb".into()],
                cards,
                "[]".into(),
            )
            .unwrap();
        assert_eq!(rows(&v, &new_ref), vec!["id:B1", "id:B2"]);
        assert_eq!(rows(&v, &base), vec!["id:A1", "id:A2", "id:A3"]);
        assert!(
            v.pending_account_selection_connections()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn one_card_key_keeps_the_single_account() {
        let groups = parse_cards(
            &[
                r#"{"transactions":[{"charge_date":"2026-11-03","amount":1,"description":"X","identifier":"1"}]}"#.into(),
                r#"{"profile":{"x":1}}"#.into(),
            ],
            &["aaaa".into(), String::new()],
            ConnectorKind::AmericanExpress,
        );
        assert_eq!(groups.len(), 1);
    }

    #[test]
    fn cards_go_to_the_stored_accounts_with_their_digits() {
        let dir = crate::test_support::TempDir::new("girocrab-amex-stored-cards");
        let v = Vault::open(
            dir.join("v.db").to_string_lossy().to_string(),
            vec![3u8; 32],
        )
        .unwrap();
        let conn = v
            .add_bank_connection(
                ConnectorKind::AmericanExpress,
                String::new(),
                "American Express".into(),
            )
            .unwrap();
        // two card accounts of this login from elsewhere (e.g. a MoneyMoney import)
        let stored = |r: &str, number: &str| RemoteAccount {
            external_ref: r.into(),
            name: "American Express".into(),
            owner: String::new(),
            iban: String::new(),
            account_number: number.into(),
            currency: "EUR".into(),
            kind: AccountKind::CreditCard,
            balance_cents: 0,
            balance_date: "2026-11-01".into(),
        };
        store_fetch(
            &v,
            ConnectorKind::AmericanExpress,
            conn,
            FetchResult {
                accounts: vec![
                    stored("mm:green", "3742-XXXXXX-11111"),
                    stored("mm:business", "3799-XXXXXX-22222"),
                ],
                ..Default::default()
            },
        )
        .unwrap();
        let tx = |id: &str| {
            format!(
                r#"{{"transactions":[{{"charge_date":"2026-11-03","amount":10,"description":"SHOP {id}","identifier":"{id}"}}]}}"#
            )
        };
        let bodies = vec![tx("A1"), tx("B1")];
        let keys = vec!["aaaa".to_string(), "bbbb".to_string()];
        // a capture without card names cannot be placed: held back, nothing stored
        v.clone()
            .import_portal_capture_cards(conn, bodies.clone(), keys.clone(), vec![], "[]".into())
            .unwrap();
        assert_eq!(
            v.pending_account_selection_connections().unwrap(),
            vec![conn]
        );
        // with the switcher's names each card finds its account; the held-back
        // entries of the first try disappear, no account is added
        let cards = vec![
            PortalCard {
                key: "aaaa".into(),
                name: "American Express® Card".into(),
                number: "11111".into(),
            },
            PortalCard {
                key: "bbbb".into(),
                name: "Amazon Business American Express® Card".into(),
                number: "22222".into(),
            },
        ];
        v.clone()
            .import_portal_capture_cards(conn, bodies, keys, cards, "[]".into())
            .unwrap();
        assert!(
            v.pending_account_selection_connections()
                .unwrap()
                .is_empty()
        );
        let choices = v.connection_account_choices(conn).unwrap();
        assert_eq!(
            choices
                .iter()
                .map(|c| c.account_ref.as_str())
                .collect::<Vec<_>>(),
            vec!["mm:green", "mm:business"]
        );
        let db = v.db().unwrap();
        let on = |id: &str| -> String {
            db.query_row(
                "SELECT a.external_ref FROM transactions t JOIN accounts a ON a.id = t.account_id
                  WHERE t.external_id = ?1",
                [format!("id:{id}")],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            (on("A1"), on("B1")),
            ("mm:green".into(), "mm:business".into())
        );
    }
}
