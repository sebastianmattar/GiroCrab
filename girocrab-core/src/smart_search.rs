//! Intelligente Transaktionssuche (#552): deterministic parser that turns
//! structured filter tokens (`betrag:>100`, `kat:Versicherung`, `tag:Kinder`,
//! `datum:2025`) and simple German questions ("Was habe ich 2025 für Amazon
//! ausgegeben?") into filter criteria a SQL query can use.
//!
//! No model, no network: pure string handling, so it is safe to call on the
//! main thread. Amount bounds are magnitudes in cents (sign-independent, the
//! direction is carried by `only_expenses`) and both inclusive.

use crate::money::parse_cents;

/// Filter criteria extracted from a search string. All fields are optional;
/// the default value matches everything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SmartSearchQuery {
    /// Merchant name or free text to look for in counterparty/purpose.
    pub merchant_or_text: Option<String>,
    /// Category name (or prefix) to filter by.
    pub category_hint: Option<String>,
    /// Tag name to filter by.
    pub tag_hint: Option<String>,
    /// Calendar year of the booking date.
    pub year: Option<i32>,
    /// Smallest absolute amount in cents (inclusive).
    pub min_amount_cents: Option<i64>,
    /// Largest absolute amount in cents (inclusive).
    pub max_amount_cents: Option<i64>,
    /// Only outgoing bookings.
    pub only_expenses: bool,
}

const EXPENSE_WORDS: &[&str] = &[
    "ausgegeben",
    "ausgaben",
    "ausgabe",
    "gezahlt",
    "bezahlt",
    "gekauft",
    "kosten",
    "zahlungen",
];

/// Words that end a "für …"/"bei …" capture.
const STOP_WORDS: &[&str] = &[
    "in",
    "im",
    "aus",
    "von",
    "seit",
    "über",
    "ueber",
    "unter",
    "mit",
    "für",
    "fuer",
    "bei",
    "habe",
    "hab",
    "ich",
    "ausgegeben",
    "bezahlt",
    "gezahlt",
    "gekauft",
    "mehr",
    "weniger",
    "mindestens",
    "höchstens",
    "hoechstens",
    "jahr",
];

const ARTICLES: &[&str] = &["die", "den", "das", "der", "dem", "meine", "meinen"];

/// Parses `input` into filter criteria. Structured tokens take precedence
/// over natural-language cues; unknown words become free text.
pub fn parse_smart_query(input: &str) -> SmartSearchQuery {
    let mut q = SmartSearchQuery::default();
    let mut words: Vec<String> = Vec::new();

    for raw in input.split_whitespace() {
        if !apply_structured_token(&mut q, raw) {
            let w = trim_punct(raw);
            if !w.is_empty() {
                words.push(w.to_string());
            }
        }
    }

    parse_natural(&mut q, &words);
    q
}

fn trim_punct(s: &str) -> &str {
    s.trim_matches(|c: char| matches!(c, '?' | '!' | '.' | ',' | ';' | ':' | '"' | '„' | '“' | '”'))
}

/// Handles `key:value`; returns false when `raw` is not a known token.
fn apply_structured_token(q: &mut SmartSearchQuery, raw: &str) -> bool {
    let Some((key, value)) = raw.split_once(':') else {
        return false;
    };
    let value = value.trim_matches('"');
    if value.is_empty() {
        return false;
    }
    match key.to_lowercase().as_str() {
        "betrag" | "amount" => apply_amount(q, value),
        "kat" | "kategorie" => {
            q.category_hint = Some(value.replace('_', " "));
            true
        }
        "tag" => {
            q.tag_hint = Some(value.replace('_', " "));
            true
        }
        "datum" | "jahr" => match year_of(value) {
            Some(y) => {
                q.year = Some(y);
                true
            }
            None => false,
        },
        "haendler" | "händler" | "von" => {
            q.merchant_or_text = Some(value.replace('_', " "));
            true
        }
        _ => false,
    }
}

fn apply_amount(q: &mut SmartSearchQuery, value: &str) -> bool {
    let (op, num) = if let Some(r) = value.strip_prefix(">=") {
        (">=", r)
    } else if let Some(r) = value.strip_prefix("<=") {
        ("<=", r)
    } else if let Some(r) = value.strip_prefix('>') {
        (">", r)
    } else if let Some(r) = value.strip_prefix('<') {
        ("<", r)
    } else if let Some(r) = value.strip_prefix('=') {
        ("=", r)
    } else {
        ("=", value)
    };
    let Ok(cents) = parse_cents(num.trim_start_matches(['-', '+'])) else {
        return false;
    };
    match op {
        ">" => q.min_amount_cents = Some(cents.saturating_add(1)),
        ">=" => q.min_amount_cents = Some(cents),
        "<" => q.max_amount_cents = Some(cents.saturating_sub(1).max(0)),
        "<=" => q.max_amount_cents = Some(cents),
        _ => {
            q.min_amount_cents = Some(cents);
            q.max_amount_cents = Some(cents);
        }
    }
    true
}

fn year_of(s: &str) -> Option<i32> {
    let head = s.get(..4)?;
    if s.len() > 4 && s.as_bytes()[4] != b'-' {
        return None;
    }
    let y: i32 = head.parse().ok()?;
    (1900..=2100).contains(&y).then_some(y)
}

fn bare_year(w: &str) -> Option<i32> {
    if w.len() == 4 { year_of(w) } else { None }
}

fn parse_natural(q: &mut SmartSearchQuery, words: &[String]) {
    let lower: Vec<String> = words.iter().map(|w| w.to_lowercase()).collect();
    let is_question = lower.iter().any(|w| {
        matches!(
            w.as_str(),
            "welche" | "welcher" | "welchen" | "was" | "wie" | "zeige" | "zeig"
        )
    });
    let mut consumed = vec![false; words.len()];

    for (i, w) in lower.iter().enumerate() {
        if EXPENSE_WORDS.contains(&w.as_str()) {
            q.only_expenses = true;
        }
        if let Some(y) = bare_year(w) {
            if q.year.is_none() {
                q.year = Some(y);
            }
            consumed[i] = true;
        }
    }

    // Amount phrases: "über 100 Euro", "mehr als 100", "unter 50", "weniger als 50".
    for i in 0..lower.len() {
        let (is_min, skip) = match lower[i].as_str() {
            "über" | "ueber" | "mindestens" => (true, 1),
            "mehr" if lower.get(i + 1).map(String::as_str) == Some("als") => (true, 2),
            "unter" | "höchstens" | "hoechstens" => (false, 1),
            "weniger" if lower.get(i + 1).map(String::as_str) == Some("als") => (false, 2),
            _ => continue,
        };
        let Some(num) = words.get(i + skip) else {
            continue;
        };
        let Ok(cents) = parse_cents(num) else {
            continue;
        };
        if is_min {
            if q.min_amount_cents.is_none() {
                q.min_amount_cents = Some(cents);
            }
        } else if q.max_amount_cents.is_none() {
            q.max_amount_cents = Some(cents);
        }
        for c in consumed.iter_mut().skip(i).take(skip + 1) {
            *c = true;
        }
        if lower.get(i + skip + 1).map(String::as_str) == Some("euro") {
            consumed[i + skip + 1] = true;
        }
    }

    // "für X" / "bei X": merchant, or tag when an article follows ("für die Kinder").
    let mut captured = false;
    for i in 0..lower.len() {
        if !matches!(lower[i].as_str(), "für" | "fuer" | "bei") || consumed[i] {
            continue;
        }
        let mut j = i + 1;
        let with_article = lower.get(j).is_some_and(|w| ARTICLES.contains(&w.as_str()));
        if with_article {
            j += 1;
        }
        let start = j;
        while j < words.len()
            && !consumed[j]
            && !STOP_WORDS.contains(&lower[j].as_str())
            && !EXPENSE_WORDS.contains(&lower[j].as_str())
        {
            j += 1;
        }
        if j == start {
            continue;
        }
        let text = words[start..j].join(" ");
        if with_article && lower[i] != "bei" {
            q.tag_hint.get_or_insert(text);
        } else {
            q.merchant_or_text.get_or_insert(text);
        }
        for c in consumed.iter_mut().take(j).skip(i) {
            *c = true;
        }
        captured = true;
        break;
    }

    // "Welche Versicherungen habe ich?" -> category.
    if q.category_hint.is_none()
        && let Some(i) = lower
            .iter()
            .position(|w| matches!(w.as_str(), "welche" | "welcher" | "welchen"))
        && let Some(noun) = words.get(i + 1)
        && !EXPENSE_WORDS.contains(&lower[i + 1].as_str())
        && !STOP_WORDS.contains(&lower[i + 1].as_str())
    {
        q.category_hint = Some(singular(noun));
        consumed[i + 1] = true;
        captured = true;
    }

    // Plain words without any question cue are free text ("amazon prime").
    if !captured && !is_question && q.merchant_or_text.is_none() {
        let text: Vec<&str> = words
            .iter()
            .enumerate()
            .filter(|(i, _)| !consumed[*i])
            .map(|(_, w)| w.as_str())
            .filter(|w| !EXPENSE_WORDS.contains(&w.to_lowercase().as_str()))
            .collect();
        if !text.is_empty() {
            q.merchant_or_text = Some(text.join(" "));
        }
    }
}

/// Crude German plural → singular for category nouns ("Versicherungen" → "Versicherung").
fn singular(noun: &str) -> String {
    let l = noun.to_lowercase();
    for suffix in ["ungen", "ionen", "heiten", "keiten", "schaften"] {
        if l.ends_with(suffix) {
            return noun[..noun.len() - 2].to_string();
        }
    }
    noun.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_tokens() {
        let q = parse_smart_query("betrag:>100 kat:Versicherung tag:Kinder datum:2025");
        assert_eq!(q.min_amount_cents, Some(10_001));
        assert_eq!(q.max_amount_cents, None);
        assert_eq!(q.category_hint.as_deref(), Some("Versicherung"));
        assert_eq!(q.tag_hint.as_deref(), Some("Kinder"));
        assert_eq!(q.year, Some(2025));
        assert_eq!(q.merchant_or_text, None);
        assert!(!q.only_expenses);
    }

    #[test]
    fn amount_operators() {
        assert_eq!(
            parse_smart_query("betrag:<50").max_amount_cents,
            Some(4_999)
        );
        assert_eq!(
            parse_smart_query("betrag:<=50,5").max_amount_cents,
            Some(5_050)
        );
        assert_eq!(
            parse_smart_query("betrag:>=20").min_amount_cents,
            Some(2_000)
        );
        let q = parse_smart_query("betrag:12,34");
        assert_eq!(
            (q.min_amount_cents, q.max_amount_cents),
            (Some(1_234), Some(1_234))
        );
    }

    #[test]
    fn structured_with_free_text() {
        let q = parse_smart_query("amazon betrag:>100");
        assert_eq!(q.merchant_or_text.as_deref(), Some("amazon"));
        assert_eq!(q.min_amount_cents, Some(10_001));
    }

    #[test]
    fn invalid_token_stays_text() {
        let q = parse_smart_query("betrag:abc");
        assert_eq!(q.min_amount_cents, None);
        assert_eq!(q.merchant_or_text.as_deref(), Some("betrag:abc"));
    }

    #[test]
    fn question_amazon_2025() {
        let q = parse_smart_query("Was habe ich 2025 für Amazon ausgegeben?");
        assert_eq!(q.year, Some(2025));
        assert_eq!(q.merchant_or_text.as_deref(), Some("Amazon"));
        assert!(q.only_expenses);
        assert_eq!(q.category_hint, None);
        assert_eq!(q.tag_hint, None);
    }

    #[test]
    fn question_insurances() {
        let q = parse_smart_query("Welche Versicherungen habe ich?");
        assert_eq!(q.category_hint.as_deref(), Some("Versicherung"));
        assert_eq!(q.merchant_or_text, None);
        assert!(!q.only_expenses);
    }

    #[test]
    fn question_children_tag() {
        let q = parse_smart_query("Zeige mir alle Ausgaben für die Kinder");
        assert_eq!(q.tag_hint.as_deref(), Some("Kinder"));
        assert_eq!(q.merchant_or_text, None);
        assert!(q.only_expenses);
    }

    #[test]
    fn natural_amount_phrases() {
        let q = parse_smart_query("Ausgaben bei Rewe über 100 Euro in 2024");
        assert_eq!(q.merchant_or_text.as_deref(), Some("Rewe"));
        assert_eq!(q.min_amount_cents, Some(10_000));
        assert_eq!(q.year, Some(2024));
        let q = parse_smart_query("Was habe ich für Netflix unter 20 Euro bezahlt?");
        assert_eq!(q.merchant_or_text.as_deref(), Some("Netflix"));
        assert_eq!(q.max_amount_cents, Some(2_000));
        let q = parse_smart_query("mehr als 50 weniger als 80");
        assert_eq!(
            (q.min_amount_cents, q.max_amount_cents),
            (Some(5_000), Some(8_000))
        );
    }

    #[test]
    fn structured_overrides_natural() {
        let q = parse_smart_query("Ausgaben für die Kinder tag:Schule");
        assert_eq!(q.tag_hint.as_deref(), Some("Schule"));
    }

    #[test]
    fn multi_word_merchant() {
        let q = parse_smart_query("Was habe ich für Deutsche Bahn ausgegeben?");
        assert_eq!(q.merchant_or_text.as_deref(), Some("Deutsche Bahn"));
    }

    #[test]
    fn plain_text_and_empty() {
        assert_eq!(
            parse_smart_query("amazon prime")
                .merchant_or_text
                .as_deref(),
            Some("amazon prime")
        );
        assert_eq!(parse_smart_query("   "), SmartSearchQuery::default());
        assert_eq!(parse_smart_query("2023").year, Some(2023));
    }
}
