//! SWIFT MT940 account statement parser (as delivered in HIKAZ), with the
//! German ":86:" structured purpose (GVC + ?nn sub-fields) and SEPA keys.

use crate::connectors::RemoteTransaction;
use crate::money::parse_cents;

#[derive(Debug, Default, Clone)]
pub struct Mt940Statement {
    pub account: String,
    pub closing_balance_cents: Option<i64>,
    pub closing_date: Option<String>,
    pub currency: String,
}

fn yymmdd(s: &str) -> Option<String> {
    let d = s.get(..6)?;
    if !d.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let yy: u32 = d.get(0..2)?.parse().ok()?;
    let year = if yy > 80 { 1900 + yy } else { 2000 + yy };
    Some(format!("{year}-{}-{}", d.get(2..4)?, d.get(4..6)?))
}

/// Splits the statement into (tag, value) fields; continuation lines are
/// appended to the previous field.
fn fields(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in text.split(['\n']) {
        let line = line.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix(':') {
            if let Some(end) = rest.find(':') {
                let tag = rest[..end].to_string();
                if tag.len() <= 3
                    && tag
                        .chars()
                        .next()
                        .map(|c| c.is_ascii_digit())
                        .unwrap_or(false)
                {
                    out.push((tag, line[end + 2..].to_string()));
                    continue;
                }
            }
        }
        if line == "-" || line.is_empty() {
            continue;
        }
        if let Some(last) = out.last_mut() {
            last.1.push('\n');
            last.1.push_str(line);
        }
    }
    out
}

/// Parsed ":86:" content.
#[derive(Debug, Default)]
pub struct Details {
    pub booking_text: String,
    pub purpose: String,
    pub counterparty: String,
    pub counterparty_iban: String,
    /// Geschäftsvorfallcode (first three digits of `:86:`), "" if absent.
    pub gvc: String,
    /// SEPA purpose code (`PURP+SALA` keyword), "" if absent.
    pub purpose_code: String,
}

pub fn parse_86(raw: &str) -> Details {
    let s: String = raw.replace(['\r', '\n'], "");
    let mut d = Details::default();
    if s.len() < 4
        || !s
            .as_bytes()
            .get(3)
            .map(|&b| b == b'?' || b == b'/')
            .unwrap_or(false)
    {
        d.purpose = s.trim().to_string();
        return d;
    }
    if s.as_bytes()[..3].iter().all(u8::is_ascii_digit) {
        d.gvc = s[..3].to_string();
    }
    let sep = s.as_bytes()[3] as char;
    let mut purpose_parts = Vec::new();
    let mut name_parts = Vec::new();
    for part in s[4..].split(sep) {
        if part.len() < 2 {
            continue;
        }
        let (key, val) = part.split_at(2);
        match key {
            "00" => d.booking_text = val.trim().to_string(),
            "20" | "21" | "22" | "23" | "24" | "25" | "26" | "27" | "28" | "29" | "60" | "61"
            | "62" | "63" => purpose_parts.push(val.to_string()),
            "31" => d.counterparty_iban = val.trim().to_string(),
            "32" | "33" => name_parts.push(val.to_string()),
            _ => {}
        }
    }
    d.counterparty = name_parts.concat().trim().to_string();
    let joined = purpose_parts.concat();
    d.purpose_code = sepa_purpose_code(&joined);
    d.purpose = sepa_purpose(&joined);
    d
}

/// SEPA purpose code from a `PURP+XXXX` keyword (some banks put it into `:86:`).
fn sepa_purpose_code(text: &str) -> String {
    text.find("PURP+")
        .map(|p| &text[p + 5..])
        .map(|rest| {
            rest.chars()
                .take_while(|c| c.is_ascii_alphabetic())
                .collect::<String>()
        })
        .filter(|c| c.len() == 4)
        .map(|c| c.to_ascii_uppercase())
        .unwrap_or_default()
}

/// Extracts the human purpose ("SVWZ+") from SEPA keyword text if present.
fn sepa_purpose(text: &str) -> String {
    const KEYS: [&str; 10] = [
        "EREF+", "KREF+", "MREF+", "CRED+", "DEBT+", "SVWZ+", "ABWA+", "ABWE+", "IBAN+", "PURP+",
    ];
    if let Some(pos) = text.find("SVWZ+") {
        let rest = &text[pos + 5..];
        let end = KEYS
            .iter()
            .filter_map(|k| rest.find(k))
            .min()
            .unwrap_or(rest.len());
        return rest[..end].trim().to_string();
    }
    text.trim().to_string()
}

/// Parses MT940 text into transactions for `account_ref`.
pub fn parse(
    text: &str,
    account_ref: &str,
    currency: &str,
) -> (Vec<RemoteTransaction>, Mt940Statement) {
    let mut txs: Vec<RemoteTransaction> = Vec::new();
    let mut stmt = Mt940Statement {
        currency: currency.to_string(),
        ..Default::default()
    };
    let mut occurrences: std::collections::HashMap<String, u32> = Default::default();
    for (tag, value) in fields(text) {
        match tag.as_str() {
            "25" => stmt.account = value.trim().to_string(),
            "62F" | "62M" => {
                // C250131EUR1234,56
                let v = value.trim();
                if let (Some(date), Some(cur), Some(amt)) = (v.get(1..7), v.get(7..10), v.get(10..))
                {
                    let sign = if v.starts_with('D') { -1 } else { 1 };
                    stmt.closing_date = yymmdd(date);
                    stmt.currency = cur.to_string();
                    if let Ok(c) = parse_cents(amt) {
                        stmt.closing_balance_cents = Some(sign * c);
                    }
                }
            }
            "61" => {
                let v = value.lines().next().unwrap_or("").trim();
                let Some(value_date) = yymmdd(v) else {
                    continue;
                };
                let Some(mut rest) = v.get(6..) else {
                    continue;
                };
                let mut booking_date = value_date.clone();
                if rest
                    .get(..4)
                    .is_some_and(|d| d.chars().all(|c| c.is_ascii_digit()))
                {
                    // entry date MMDD (year from value date, adjusted around new year)
                    let year: i32 = value_date
                        .get(..4)
                        .and_then(|x| x.parse().ok())
                        .unwrap_or(2000);
                    let vm: u32 = value_date
                        .get(5..7)
                        .and_then(|x| x.parse().ok())
                        .unwrap_or(1);
                    let bm: u32 = rest.get(..2).and_then(|x| x.parse().ok()).unwrap_or(vm);
                    let y = if vm == 12 && bm == 1 {
                        year + 1
                    } else if vm == 1 && bm == 12 {
                        year - 1
                    } else {
                        year
                    };
                    booking_date = format!("{y}-{}-{}", &rest[..2], &rest[2..4]);
                    rest = &rest[4..];
                }
                let (sign, rest) = if let Some(r) = rest.strip_prefix("RC") {
                    (-1, r)
                } else if let Some(r) = rest.strip_prefix("RD") {
                    (1, r)
                } else if let Some(r) = rest.strip_prefix('C') {
                    (1, r)
                } else if let Some(r) = rest.strip_prefix('D') {
                    (-1, r)
                } else {
                    continue;
                };
                // optional funds code (third currency letter)
                let rest = if rest
                    .chars()
                    .next()
                    .map(|c| c.is_ascii_alphabetic())
                    .unwrap_or(false)
                {
                    &rest[1..]
                } else {
                    rest
                };
                let amount_end = rest
                    .find(|c: char| !(c.is_ascii_digit() || c == ','))
                    .unwrap_or(rest.len());
                let Ok(amount) = parse_cents(&rest[..amount_end]) else {
                    continue;
                };
                let reference = rest[amount_end..].to_string();
                txs.push(RemoteTransaction {
                    account_ref: account_ref.to_string(),
                    external_id: String::new(),
                    booking_date,
                    value_date,
                    amount_cents: sign * amount,
                    currency: stmt.currency.clone(),
                    counterparty: String::new(),
                    counterparty_iban: String::new(),
                    purpose: String::new(),
                    booking_text: reference
                        .split("//")
                        .next()
                        .unwrap_or("")
                        .chars()
                        .skip(4)
                        .collect(),
                    ..Default::default()
                });
            }
            "86" => {
                if let Some(t) = txs.last_mut() {
                    let d = parse_86(&value);
                    if !d.booking_text.is_empty() {
                        t.booking_text = d.booking_text;
                    }
                    t.purpose = d.purpose;
                    t.counterparty = d.counterparty;
                    t.counterparty_iban = d.counterparty_iban;
                    t.gvc = d.gvc;
                    t.purpose_code = d.purpose_code;
                }
            }
            _ => {}
        }
    }
    for t in &mut txs {
        let key = format!(
            "{}{}{}{}",
            t.booking_date, t.amount_cents, t.counterparty, t.purpose
        );
        let n = occurrences.entry(key).or_insert(0);
        t.external_id = t.content_id(*n);
        *n += 1;
    }
    (txs, stmt)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = ":20:STARTUMSE\r\n:25:52090000/0012345678\r\n:28C:00000/001\r\n:60F:C250101EUR1000,00\r\n\
:61:2501020102DR23,45NDDTNONREF\r\n:86:105?00FOLGELASTSCHRIFT?20EREF+123 MREF+M1 CRED+DE1?21SVWZ+Einkauf REWE Markt?22 Kassel?30GENODEF1S08?31DE02520900000000000001?32REWE Markt GmbH\r\n\
:61:2501030103CR2500,00NTRFNONREF\r\n:86:166?00GUTSCHRIFT?20SVWZ+Gehalt Januar?32Muster GmbH\r\n:62F:C250103EUR3476,55\r\n-\r\n";

    #[test]
    fn keeps_gvc_and_purpose_code() {
        let d = parse_86("153?00LOHN/GEHALT?20SVWZ+Gehalt 10/2026 ?21PURP+SALA?32ACME GmbH");
        assert_eq!(d.gvc, "153");
        assert_eq!(d.purpose_code, "SALA");
        assert_eq!(d.purpose, "Gehalt 10/2026");
        let (txs, _) = parse(SAMPLE, "acc", "EUR");
        assert_eq!(txs[0].gvc, "105");
        assert_eq!(txs[1].gvc, "166");
        assert!(parse_86("free text").gvc.is_empty());
    }

    #[test]
    fn parses_sample() {
        let (txs, st) = parse(SAMPLE, "acc", "EUR");
        assert_eq!(txs.len(), 2);
        assert_eq!(txs[0].amount_cents, -2345);
        assert_eq!(txs[0].booking_date, "2025-01-02");
        assert_eq!(txs[0].counterparty, "REWE Markt GmbH");
        assert_eq!(txs[0].purpose, "Einkauf REWE Markt Kassel");
        assert_eq!(txs[0].booking_text, "FOLGELASTSCHRIFT");
        assert_eq!(txs[1].amount_cents, 250000);
        assert_eq!(st.closing_balance_cents, Some(347655));
        assert_ne!(txs[0].external_id, txs[1].external_id);
    }

    #[test]
    fn non_ascii_fields_do_not_panic() {
        for t in [
            ":25:1\n:61:12345é\n:62F:C25013€EURabc",
            ":61:250131ééC1,\n:86:x",
            ":62F:Cäää0131EUR1,5",
            ":61:2501310131Cé1,",
        ] {
            let _ = parse(t, "", "EUR");
        }
    }
}
