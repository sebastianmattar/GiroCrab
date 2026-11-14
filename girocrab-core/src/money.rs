//! Integer money helpers. Amounts are i64 cents; parsing never uses floats.

use crate::error::{CoreError, CoreResult};
use crate::i18n::{tr, trf};

/// Parses a decimal amount ("1234,56", "-12.5", "1.234,56", "+0,07", "12")
/// into cents without any floating point arithmetic.
///
/// Rules: the *last* `,` or `.` followed by 1-2 digits is the decimal
/// separator; every other `.`/`,`/space/apostrophe is a grouping separator.
#[uniffi::export]
pub fn parse_amount_to_cents(text: String) -> Result<i64, CoreError> {
    parse_cents(&text)
}

pub fn parse_cents(text: &str) -> CoreResult<i64> {
    parse_cents_with(text, false)
}

/// Like [`parse_cents`], but rounds extra decimals half away from zero
/// instead of rejecting them ("412.3871" -> 41239, "1.234,5678" -> 123457).
/// For computed values such as depot prices and values; bookings stay strict.
pub fn parse_cents_rounded(text: &str) -> CoreResult<i64> {
    parse_cents_with(text, true)
}

fn parse_cents_with(text: &str, round: bool) -> CoreResult<i64> {
    let t = text.trim();
    if t.is_empty() {
        return Err(CoreError::InvalidInput {
            message: tr("Kein Betrag angegeben").into(),
        });
    }
    let (neg, body) = match t.as_bytes()[0] {
        b'-' => (true, &t[1..]),
        b'+' => (false, &t[1..]),
        _ => (false, t),
    };
    let body = body.trim();
    // Decimal separator rules (predictable, no locale guessing):
    //  * both '.' and ',' present -> the last one is the decimal separator
    //  * one kind present exactly once -> it is the decimal separator
    //    (FinTS "1234,5", JSON "1234.5")
    //  * one kind present several times -> grouping only, no decimals
    let dots = body.matches('.').count();
    let commas = body.matches(',').count();
    let sep_pos = if dots > 0 && commas > 0 {
        body.rfind(['.', ','])
    } else if dots == 1 {
        body.rfind('.')
    } else if commas == 1 {
        body.rfind(',')
    } else {
        None
    };
    let (int_part, frac_part) = match sep_pos {
        Some(p) => (&body[..p], &body[p + 1..]),
        None => (body, ""),
    };
    let int_digits: String = int_part.chars().filter(|c| c.is_ascii_digit()).collect();
    if int_part
        .chars()
        .any(|c| !(c.is_ascii_digit() || matches!(c, '.' | ',' | ' ' | '\'' | '\u{a0}')))
    {
        return Err(CoreError::InvalidInput {
            message: trf("Kein gültiger Betrag: „{0}“", &[&text]),
        });
    }
    if !frac_part.chars().all(|c| c.is_ascii_digit()) {
        return Err(CoreError::InvalidInput {
            message: trf("Kein gültiger Betrag: „{0}“", &[&text]),
        });
    }
    // Third decimal >= 5 rounds the cents up (away from zero, sign applied last).
    let round_up = round && frac_part.as_bytes().get(2).is_some_and(|&d| d >= b'5');
    if frac_part.len() > 2 && !round {
        // Allow trailing zeros only ("1.500" from some JSON APIs)
        if frac_part[2..].chars().any(|c| c != '0') {
            return Err(CoreError::InvalidInput {
                message: trf("Zu viele Nachkommastellen in „{0}“", &[&text]),
            });
        }
    }
    let mut frac = frac_part.chars().take(2).collect::<String>();
    while frac.len() < 2 {
        frac.push('0');
    }
    let int_val: i64 = if int_digits.is_empty() {
        0
    } else {
        int_digits.parse().map_err(|_| CoreError::InvalidInput {
            message: trf("Betrag zu groß: „{0}“", &[&text]),
        })?
    };
    let frac_val: i64 = frac.parse().unwrap_or(0);
    let cents = int_val
        .checked_mul(100)
        .and_then(|v| v.checked_add(frac_val + i64::from(round_up)))
        .ok_or_else(|| CoreError::InvalidInput {
            message: trf("Betrag zu groß: „{0}“", &[&text]),
        })?;
    Ok(if neg { -cents } else { cents })
}

/// Parses a JSON number (as its exact textual representation) into cents.
pub fn json_number_to_cents(v: &serde_json::Value) -> CoreResult<i64> {
    match v {
        serde_json::Value::Number(n) => parse_cents(&n.to_string()),
        serde_json::Value::String(s) => parse_cents(s),
        _ => Err(CoreError::Protocol {
            message: format!("expected amount, got {v}"),
        }),
    }
}

/// Like [`json_number_to_cents`], but rounds extra decimals (see
/// [`parse_cents_rounded`]). JSON floats that print in exponent form
/// ("4e-7") fall back to `f64` rounding.
pub fn json_number_to_cents_rounded(v: &serde_json::Value) -> CoreResult<i64> {
    match v {
        serde_json::Value::Number(n) => parse_cents_rounded(&n.to_string()).or_else(|e| {
            n.as_f64()
                .map(|f| (f * 100.0).round())
                .filter(|c| c.is_finite() && c.abs() < i64::MAX as f64)
                .map(|c| c as i64)
                .ok_or(e)
        }),
        serde_json::Value::String(s) => parse_cents_rounded(s),
        _ => json_number_to_cents(v),
    }
}

/// German-style formatting used in logs/tests: "-1.234,56 EUR".
#[uniffi::export]
pub fn format_cents(cents: i64, currency: String) -> String {
    let neg = cents < 0;
    let abs = cents.unsigned_abs();
    let euros = abs / 100;
    let rest = abs % 100;
    let digits = euros.to_string();
    let mut grouped = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push('.');
        }
        grouped.push(ch);
    }
    format!(
        "{}{},{:02} {}",
        if neg { "-" } else { "" },
        grouped,
        rest,
        currency
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_extra_json_decimals() {
        use serde_json::json;
        let r = |v| json_number_to_cents_rounded(&v).unwrap();
        assert_eq!(r(json!("412.3871")), 41239);
        assert_eq!(r(json!("412.3849")), 41238);
        assert_eq!(r(json!("-0.005")), -1);
        assert_eq!(r(json!("-12.344")), -1234);
        assert_eq!(r(json!(61.3412)), 6134);
        assert_eq!(r(json!("766.75")), 76675);
        assert_eq!(r(json!("1.234,5678")), 123457);
        assert_eq!(r(json!("12,3456")), 1235);
        assert_eq!(r(json!("0.999")), 100);
        assert_eq!(r(json!(4e-7)), 0);
        assert_eq!(r(json!(2.5e16)), 2_500_000_000_000_000_000);
        assert_eq!(r(json!("12")), 1200);
        assert!(json_number_to_cents_rounded(&json!(null)).is_err());
        // The strict parser keeps rejecting them.
        assert!(json_number_to_cents(&json!("412.3871")).is_err());
    }

    #[test]
    fn parses_common_formats() {
        assert_eq!(parse_cents("1234,56").unwrap(), 123456);
        assert_eq!(parse_cents("1.234,56").unwrap(), 123456);
        assert_eq!(parse_cents("-12.5").unwrap(), -1250);
        assert_eq!(parse_cents("+0,07").unwrap(), 7);
        assert_eq!(parse_cents("12").unwrap(), 1200);
        assert_eq!(parse_cents("12,").unwrap(), 1200);
        assert_eq!(parse_cents("1,234,567.89").unwrap(), 123456789);
        assert_eq!(parse_cents("-0.01").unwrap(), -1);
        assert_eq!(parse_cents("1500.0000").unwrap(), 150000);
        assert!(parse_cents("abc").is_err());
    }

    #[test]
    fn formats() {
        assert_eq!(format_cents(-123456, "EUR".into()), "-1.234,56 EUR");
        assert_eq!(format_cents(5, "EUR".into()), "0,05 EUR");
    }
}
