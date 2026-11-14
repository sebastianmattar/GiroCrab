//! Sanitizer for in-app bug reports (GiroCrab/GiroCrab#301).
//!
//! The "Problem melden" sheet puts a diagnostics block into a *public* GitHub
//! issue. Everything that goes there passes [`sanitize_report_text`] first:
//! it removes IBANs completely (also the `DE12…3456` form stored in the
//! Abrufprotokoll), e-mail addresses, amounts (with a currency, German
//! `1.234,56` / `1 234,56` / `1.234`, or `1234.56` as in API texts), URL query
//! strings and every term the caller knows to be personal (account owners,
//! account names, IBANs, payees; also in the genitive), and then runs the
//! Abrufprotokoll scrubber ([`crate::sync_log::scrub`]: secrets, tokens, long
//! digit runs).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};

use regex::{Regex, RegexBuilder};

use crate::sync_log::{re, replace_ibans, scrub};

/// Replacement for a known personal term.
pub const REDACTED: &str = "[REDACTED]";

/// Terms shorter than this are not replaced (would hit ordinary words).
const MIN_TERM_CHARS: usize = 3;

/// Terms per compiled pattern: keeps each one far below the regex size limit.
const TERMS_PER_PATTERN: usize = 300;

/// Cleans `text` line by line for a public bug report. `known_terms` are
/// replaced case-insensitively by `[REDACTED]` where they stand as whole words
/// (longest first; terms under three characters are ignored). Fails closed:
/// if the terms cannot be compiled, every non-empty line becomes `[REDACTED]`.
#[uniffi::export]
pub fn sanitize_report_text(text: String, known_terms: Vec<String>) -> String {
    let terms = cached_terms_regexes(&known_terms);
    text.lines()
        .map(|line| match &terms {
            Ok(terms) => sanitize_line(line, terms),
            Err(_) if line.trim().is_empty() => String::new(),
            Err(_) => REDACTED.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The compiled terms of the last call: the report sheet sanitizes again for
/// the same vault (log toggle, unlock), and compiling thousands of terms is
/// the expensive part.
fn cached_terms_regexes(known_terms: &[String]) -> Result<Arc<Vec<Regex>>, regex::Error> {
    static CACHE: Mutex<Option<(u64, Arc<Vec<Regex>>)>> = Mutex::new(None);
    let key = terms_key(known_terms);
    if let Some((cached_key, regexes)) = CACHE.lock().ok().as_deref().and_then(Option::as_ref) {
        if *cached_key == key {
            return Ok(Arc::clone(regexes));
        }
    }
    let regexes = Arc::new(terms_regexes(known_terms)?);
    if let Ok(mut cache) = CACHE.lock() {
        *cache = Some((key, Arc::clone(&regexes)));
    }
    Ok(regexes)
}

fn terms_key(known_terms: &[String]) -> u64 {
    let mut hasher = DefaultHasher::new();
    known_terms.hash(&mut hasher);
    hasher.finish()
}

/// One case-insensitive pattern for all terms, longest first. A term that
/// starts or ends with a letter or digit must stand as a whole word there
/// (an account "Giro" must not turn "GiroCrab" into "[REDACTED]Crab"); where
/// a longer term stops inside a word, a shorter one still matches. A word
/// ending also takes a genitive "s" ("Mustermanns Konto").
fn terms_regexes(known_terms: &[String]) -> Result<Vec<Regex>, regex::Error> {
    let mut terms: Vec<&str> = known_terms
        .iter()
        .map(|t| t.trim())
        .filter(|t| t.chars().count() >= MIN_TERM_CHARS)
        .collect();
    terms.sort_by_cached_key(|t| (std::cmp::Reverse(t.chars().count()), t.to_lowercase()));
    terms.dedup_by(|a, b| a.to_lowercase() == b.to_lowercase());
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    // Chunks keep the longest-first order: earlier patterns hold longer terms.
    terms
        .chunks(TERMS_PER_PATTERN)
        .map(|chunk| {
            let alternation = chunk
                .iter()
                .map(|t| {
                    let start = if t.starts_with(is_word) { r"\b" } else { "" };
                    let end = if t.ends_with(is_word) {
                        r"(?:s|'s|’s)?\b"
                    } else {
                        ""
                    };
                    format!("{start}{}{end}", regex::escape(t))
                })
                .collect::<Vec<_>>()
                .join("|");
            RegexBuilder::new(&format!("(?i)(?:{alternation})"))
                .size_limit(64 << 20)
                .build()
        })
        .collect()
}

/// Words before a two-part version number ("FinTS 3.00", "TLS 1.20").
const VERSION_WORDS: &[&str] = &[
    "version", "ver", "v", "fints", "hbci", "tls", "ssl", "http", "api", "ios", "ipados", "macos",
    "os", "build", "release", "xcode", "swift", "rust", "psd2", "girocrab", "pain", "camt", "sepa",
];

/// "1234.56" and "1,234.56" as in API and JSON texts, and German thousands
/// without cents ("1.234"). A match that is part of a dotted run
/// ("14.11.2026", "192.168.10.10") is a date or address; one right after a
/// version word is a version.
fn mask_dot_amounts(s: &str) -> String {
    static DOT_AMOUNT: OnceLock<Regex> = OnceLock::new();
    let pattern = re(
        &DOT_AMOUNT,
        r"[-+−]?\b(?:(?:\d{1,3}(?:,\d{3})+|\d+)\.\d{2}|\d{1,3}(?:\.\d{3})+)\b",
    );
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    for m in pattern.find_iter(s) {
        let dotted_before = s[..m.start()].ends_with('.');
        let mut after = s[m.end()..].chars();
        let dotted_after =
            after.next() == Some('.') && after.next().is_some_and(|c| c.is_ascii_digit());
        let previous_word = s[..m.start()]
            .trim_end()
            .rsplit(|c: char| !c.is_alphanumeric())
            .next()
            .unwrap_or("")
            .to_lowercase();
        let version = !m.as_str().starts_with(['-', '+', '−'])
            && VERSION_WORDS.contains(&previous_word.as_str());
        if dotted_before || dotted_after || version {
            continue;
        }
        out.push_str(&s[last..m.start()]);
        out.push_str("[Betrag]");
        last = m.end();
    }
    out.push_str(&s[last..]);
    out
}

fn sanitize_line(line: &str, terms: &[Regex]) -> String {
    static SCRUBBED_AMOUNT: OnceLock<Regex> = OnceLock::new();
    static EMAIL: OnceLock<Regex> = OnceLock::new();
    static SHORT_IBAN: OnceLock<Regex> = OnceLock::new();
    static URL_QUERY: OnceLock<Regex> = OnceLock::new();
    static AMOUNT_AFTER: OnceLock<Regex> = OnceLock::new();
    static AMOUNT_BEFORE: OnceLock<Regex> = OnceLock::new();
    static BARE_AMOUNT: OnceLock<Regex> = OnceLock::new();

    if line.trim().is_empty() {
        return String::new();
    }
    let mut s = re(
        &EMAIL,
        r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}",
    )
    .replace_all(line, "[E-Mail]")
    .into_owned();
    s = re(&URL_QUERY, r"(https?://[^\s?#]+)[?#]\S*")
        .replace_all(&s, "$1?…")
        .into_owned();
    for pattern in terms {
        s = pattern.replace_all(&s, REDACTED).into_owned();
    }
    s = replace_ibans(&s, |_| "[IBAN]".to_string());
    // Amounts before `scrub`, whose digit rule would cut "1234567,89" apart.
    // German form, also without a currency: "1.234,56", "1 234,56", "-12,30".
    s = re(
        &BARE_AMOUNT,
        r"[-+−]?\b(?:\d{1,3}(?:[ .\u{00A0}\u{202F}]\d{3})+|\d+),\d{2}\b",
    )
    .replace_all(&s, "[Betrag]")
    .into_owned();
    // Thousands with a currency: dot, comma, apostrophe or a non-breaking
    // space, never a plain one ("HTTP 403 500 EUR" is a status code and an
    // amount). The currency may be glued on ("USD1,000.00", "12EUR").
    const NUMBER: &str = r"\d{1,3}(?:[.,\u{00A0}\u{202F}']?\d{3})*(?:[.,]\d{1,2})?";
    s = re(
        &AMOUNT_AFTER,
        &format!(r"[-+−]?\b{NUMBER}\s?(?:€|EUR\b|USD\b|\$|CHF\b|GBP\b|£)"),
    )
    .replace_all(&s, "[Betrag]")
    .into_owned();
    s = re(
        &AMOUNT_BEFORE,
        &format!(r"(?:€|\bEUR|\bUSD|\$|\bCHF|\bGBP|£)\s?[-+−]?\s?{NUMBER}\b"),
    )
    .replace_all(&s, "[Betrag]")
    .into_owned();
    // Secrets, tokens, IBANs and long digit runs (also trims the line).
    s = scrub(&s, "");
    // The Abrufprotokoll stores IBANs as `DE12…3456`; no boundary after it:
    // the IBAN may be glued to text ("DE12…3456abc").
    s = re(&SHORT_IBAN, r"(?i)\b[A-Z]{2}\d{2}…[A-Z0-9]{4}")
        .replace_all(&s, "[IBAN]")
        .into_owned();
    // A stored log line may carry a large amount already cut by `scrub`.
    s = re(&SCRUBBED_AMOUNT, r"[-+−]?•••[.,]\d{2}\b")
        .replace_all(&s, "[Betrag]")
        .into_owned();
    mask_dot_amounts(&s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean(text: &str) -> String {
        sanitize_report_text(text.into(), vec![])
    }

    #[test]
    fn removes_ibans_in_full_and_spaced_form() {
        let s = clean("Konto DE89370400440532013000 und DE89 3704 0044 0532 0130 00 gesperrt");
        assert_eq!(s, "Konto [IBAN] und [IBAN] gesperrt");
    }

    #[test]
    fn keeps_the_word_after_an_iban_and_matches_any_case() {
        assert_eq!(
            clean("Konto DE89 3704 0044 0532 0130 00 HTTP 403"),
            "Konto [IBAN] HTTP 403"
        );
        assert_eq!(
            clean("konto de89 3704 0044 0532 0130 00 gesperrt"),
            "konto [IBAN] gesperrt"
        );
    }

    #[test]
    fn terms_with_punctuation_and_duplicates_still_match() {
        let s = sanitize_report_text(
            "Überweisung an Max M. abgelehnt, Konto Giro, Bank BANK".into(),
            vec![
                "Giro".into(),
                "Bank".into(),
                "giro".into(),
                "Max M.".into(),
                "BANK".into(),
            ],
        );
        assert_eq!(
            s,
            "Überweisung an [REDACTED] abgelehnt, Konto [REDACTED], [REDACTED] [REDACTED]"
        );
    }

    #[test]
    fn masks_dot_decimal_amounts_but_not_dates_versions_or_addresses() {
        let s = clean(
            r#"PayPal: {"amount":{"value":"1234.56","currency":"EUR"}}, amount 99.90 exceeds limit, total 1,234.56"#,
        );
        assert!(
            !s.contains("1234.56") && !s.contains("99.90") && !s.contains("234.56"),
            "{s}"
        );
        let keep = "Abruf 14.11.2026, Version 0.6.2, Host 192.168.10.10, Build 15.10.1";
        assert_eq!(clean(keep), keep);
    }

    #[test]
    fn keeps_versions_and_status_codes_but_masks_large_amounts() {
        let keep = "FinTS 3.00, TLS 1.20, Version 1.10, macOS 15.10 arm64";
        assert_eq!(clean(keep), keep);
        assert_eq!(clean("HTTP 403 500 EUR"), "HTTP 403 [Betrag]");
        assert_eq!(
            clean("Gebühr 1234567,89 und 1234567.89"),
            "Gebühr [Betrag] und [Betrag]"
        );
        assert_eq!(
            clean("amount 99.90 exceeds limit"),
            "amount [Betrag] exceeds limit"
        );
        assert_eq!(clean("EUROPA 12"), "EUROPA 12");
    }

    #[test]
    fn masks_spaced_glued_and_cent_less_amounts() {
        assert_eq!(clean("Saldo 1 234,56 EUR"), "Saldo [Betrag] EUR");
        assert_eq!(
            clean("Betrag USD1,000.00 und 12EUR"),
            "Betrag [Betrag] und [Betrag]"
        );
        assert_eq!(
            clean("Saldo 1.234 nicht gedeckt"),
            "Saldo [Betrag] nicht gedeckt"
        );
        assert_eq!(clean("Gebühr 1234567,89"), "Gebühr [Betrag]");
        // stored log lines: already cut by scrub
        assert_eq!(clean("Gebühr •••,89"), "Gebühr [Betrag]");
        assert_eq!(
            clean("Abruf 14.11.2026, Version 0.6.2"),
            "Abruf 14.11.2026, Version 0.6.2"
        );
    }

    #[test]
    fn genitive_and_iban_forms_of_known_terms() {
        let s = sanitize_report_text(
            "Müllers Konto, Erika’s Karte, Müllerstraße".into(),
            vec!["Müller".into(), "Erika".into()],
        );
        assert_eq!(s, "[REDACTED] Konto, [REDACTED] Karte, Müllerstraße");
    }

    #[test]
    fn the_cache_key_follows_the_terms() {
        // (pointer equality of two calls would race with tests running in parallel)
        let terms: Vec<String> = (0..2000).map(|i| format!("Person Nummer{i}")).collect();
        let mut other = terms.clone();
        other[1999] = "Erika Mustermann".into();
        assert_eq!(terms_key(&terms), terms_key(&terms.clone()));
        assert_ne!(terms_key(&terms), terms_key(&other));
    }

    #[test]
    fn many_terms_are_split_into_several_patterns() {
        let mut terms: Vec<String> = (0..2500).map(|i| format!("Person Nummer{i}")).collect();
        terms.push("Erika Mustermann".into());
        let s = sanitize_report_text(
            "an Erika Mustermann und Person Nummer2499 abgelehnt".into(),
            terms,
        );
        assert_eq!(s, "an [REDACTED] und [REDACTED] abgelehnt");
    }

    #[test]
    fn masks_bare_amounts_in_the_german_form() {
        let s =
            clean("Überweisung über 1.234,56 an Empfänger abgelehnt, Saldo -12,30, Gebühr 1234,50");
        assert!(!s.contains("1.234,56"), "{s}");
        assert!(!s.contains("12,30"), "{s}");
        assert!(!s.contains("1234,50"), "{s}");
        assert!(s.contains("abgelehnt"), "{s}");
        // dates, times and versions stay
        let s = clean("Abruf 14.11.2026 10:00, Version 0.6.2");
        assert_eq!(s, "Abruf 14.11.2026 10:00, Version 0.6.2");
    }

    #[test]
    fn removes_glued_and_short_ibans_completely() {
        assert_eq!(clean("DE89370400440532013000abc"), "[IBAN]abc");
        assert_eq!(
            clean("Konto DE89 3704 0044 0532 Konto gesperrt"),
            "Konto [IBAN] Konto gesperrt"
        );
    }

    #[test]
    fn removes_ibans_already_shortened_by_the_sync_log() {
        let s = clean("Abruf DE89…3000 fehlgeschlagen");
        assert_eq!(s, "Abruf [IBAN] fehlgeschlagen");
    }

    #[test]
    fn removes_emails_and_url_queries() {
        let s = clean(
            "Login max.mustermann@example.de abgelehnt, Redirect https://bank.example/cb?code=abc123&state=xyz",
        );
        assert!(!s.contains("mustermann"), "{s}");
        assert!(!s.contains("abc123"), "{s}");
        assert!(s.contains("[E-Mail]"), "{s}");
        assert!(s.contains("https://bank.example/cb?…"), "{s}");
    }

    #[test]
    fn masks_amounts_with_a_currency_but_keeps_versions_and_dates() {
        let s = clean(
            "Saldo 1.234,56 € statt -12,30 EUR, Limit EUR 500 (v0.6.1, 14.11.2026, HTTP 403)",
        );
        assert!(!s.contains("1.234,56"), "{s}");
        assert!(!s.contains("12,30"), "{s}");
        assert!(!s.contains("500"), "{s}");
        assert!(s.contains("v0.6.1"), "{s}");
        assert!(s.contains("14.11.2026"), "{s}");
        assert!(s.contains("HTTP 403"), "{s}");
    }

    #[test]
    fn replaces_known_terms_case_insensitively_longest_first() {
        let s = sanitize_report_text(
            "Girokonto Erika Mustermann: Erika hat ERIKA MUSTERMANN überwiesen".into(),
            vec![
                "Erika".into(),
                "Erika Mustermann".into(),
                "".into(),
                "ab".into(),
            ],
        );
        assert_eq!(
            s,
            "Girokonto [REDACTED]: [REDACTED] hat [REDACTED] überwiesen"
        );
    }

    #[test]
    fn leaves_known_terms_inside_longer_words_alone() {
        let s = sanitize_report_text(
            "GiroCrab: Konto Giro (Giro) nicht erreichbar".into(),
            vec!["Giro".into()],
        );
        assert_eq!(
            s,
            "GiroCrab: Konto [REDACTED] ([REDACTED]) nicht erreichbar"
        );
        let s = sanitize_report_text(
            "Erika Mustermanns Konto".into(),
            vec!["Erika Mustermann".into(), "Erika".into()],
        );
        assert_eq!(s, "[REDACTED] Konto");
    }

    #[test]
    fn reuses_the_log_scrubber_for_secrets_and_long_numbers() {
        let s = clean(
            "PIN=12345 falsch, TAN 987654 abgelaufen, Authorization: Bearer abcdefgh12345678",
        );
        assert!(!s.contains("12345"), "{s}");
        assert!(!s.contains("987654"), "{s}");
        assert!(!s.contains("abcdefgh"), "{s}");
        assert!(s.contains("falsch"), "{s}");
    }

    #[test]
    fn keeps_lines_and_harmless_text() {
        let s = clean("GiroCrab 0.6.1 (14)\n\nmacOS 15.1 arm64\nFehler: Zeitüberschreitung");
        assert_eq!(
            s,
            "GiroCrab 0.6.1 (14)\n\nmacOS 15.1 arm64\nFehler: Zeitüberschreitung"
        );
    }
}
