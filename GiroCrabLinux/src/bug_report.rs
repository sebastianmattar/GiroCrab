//! BugReport.swift: Hilfe › Problem melden … (GiroCrab/GiroCrab#301) without the UI.
//!
//! The diagnostics block and the prefilled `issues/new` URL. Only the log messages carry
//! user data: they pass the core sanitizer (`sanitize_report_text`) with every term the
//! vault knows to be personal. Nothing is sent from here: the window opens the URL in the
//! browser, where the user still edits and submits the issue.

use std::collections::{HashMap, HashSet};

use girocrab_core::sync_log::{SyncLogEntry, SyncLogFilter};
use girocrab_core::{BankDescriptor, Vault};
use serde_json::{Value, json};

use crate::i18n::tr;

pub const NEW_ISSUE: &str = "https://github.com/GiroCrab/GiroCrab/issues/new";
/// GitHub answers longer `issues/new` URLs with an error; measured on the
/// percent-encoded URL (umlauts and "·" take 6 bytes there).
pub const MAX_URL_LENGTH: usize = 7500;
/// Abrufprotokoll entries in the diagnostics.
pub const LOG_ENTRIES: u32 = 15;
/// Counterparty names read from the vault at most (newest first).
pub const MAX_COUNTERPARTY_TERMS: u32 = 5000;

/// One Abrufprotokoll entry: `head` is built by the app from time, connector
/// kind, trigger and result (no user data); `message` is the bank's text and
/// the only part that passes the sanitizer.
#[derive(Debug, Clone, PartialEq)]
pub struct LogLine {
    pub head: String,
    pub message: String,
}

/// What the diagnostics block shows.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Info {
    pub app_version: String,
    pub system: String,
    pub architecture: String,
    pub language: String,
    pub demo: bool,
    pub connectors: Vec<String>,
}

/// The log lines with sanitized messages (IBANs, e-mails, amounts, secrets,
/// long numbers and the vault's own names are replaced). The head is left
/// alone: an account named like its bank ("PayPal") must not blank out the
/// connector column.
pub fn sanitized_log(lines: &[LogLine], known_terms: &[String]) -> Vec<String> {
    if lines.is_empty() {
        return Vec::new();
    }
    // One call for all messages (one regex build). The closing "." keeps a
    // trailing empty message from being dropped by the line split.
    let joined = lines
        .iter()
        .map(|l| l.message.as_str())
        .chain(std::iter::once("."))
        .collect::<Vec<_>>()
        .join("\n");
    let sanitized =
        girocrab_core::report_sanitizer::sanitize_report_text(joined, known_terms.to_vec());
    let mut messages: Vec<&str> = sanitized.split('\n').collect();
    messages.pop();
    if messages.len() != lines.len() {
        // never expected; fall back to one call per message
        return lines
            .iter()
            .map(|l| {
                line(
                    &l.head,
                    &girocrab_core::report_sanitizer::sanitize_report_text(
                        l.message.clone(),
                        known_terms.to_vec(),
                    ),
                )
            })
            .collect();
    }
    lines
        .iter()
        .zip(messages)
        .map(|(l, m)| line(&l.head, m))
        .collect()
}

fn line(head: &str, message: &str) -> String {
    if message.is_empty() {
        head.to_string()
    } else {
        format!("{head} · {message}")
    }
}

/// The diagnostics block (header plus already sanitized log lines).
pub fn render(info: &Info, log: &[String]) -> String {
    let yes_no = if info.demo { tr("ja") } else { tr("nein") };
    let connectors = if info.connectors.is_empty() {
        "–".to_string()
    } else {
        info.connectors.join(", ")
    };
    let mut lines = vec![
        format!("GiroCrab {}", info.app_version),
        format!("{} · {}", info.system, info.architecture),
        tr("Sprache: %1 · Demo-Modus: %2")
            .replace("%1", &info.language)
            .replace("%2", &yes_no),
        tr("Anbindungen: %1").replace("%1", &connectors),
    ];
    if !log.is_empty() {
        lines.push(String::new());
        lines.push(tr("Abruf-Protokoll (neueste zuerst):"));
        lines.extend(log.iter().cloned());
    }
    lines.join("\n")
}

/// Booking texts that stand where a payee would ("VISA", "Zinsen",
/// "Entgelt"): as terms they would blank ordinary words in every log line.
const BOOKING_WORDS: &[&str] = &[
    "abbuchung",
    "abrechnung",
    "abschluss",
    "auszahlung",
    "bank",
    "bargeld",
    "bonus",
    "dauerauftrag",
    "einzahlung",
    "entgelt",
    "gebühr",
    "gebühren",
    "gehalt",
    "girocard",
    "gutschrift",
    "habenzinsen",
    "kartenzahlung",
    "kasse",
    "konto",
    "kontoführung",
    "kreditkarte",
    "lastschrift",
    "lohn",
    "mastercard",
    "miete",
    "online",
    "paypal",
    "rechnungsabschluss",
    "rente",
    "rücklastschrift",
    "sepa",
    "sollzinsen",
    "steuer",
    "storno",
    "transfer",
    "umbuchung",
    "visa",
    "zins",
    "zinsen",
    "überweisung",
];

/// Counterparties as redaction terms: every name of two words or more, and
/// single words of four letters or more ("Müller", "Meier") unless they are
/// booking words. Better a blanked word than a published payee.
pub fn counterparty_terms(names: &[String]) -> Vec<String> {
    names
        .iter()
        .filter(|name| {
            let words: Vec<&str> = name
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| !w.is_empty())
                .collect();
            match words.as_slice() {
                [] => false,
                [word] => {
                    word.chars().count() >= 4
                        && !BOOKING_WORDS.contains(&word.to_lowercase().as_str())
                }
                _ => true,
            }
        })
        .cloned()
        .collect()
}

fn normalized_term(s: &str) -> String {
    s.trim().to_lowercase()
}

/// Drops empty terms, bank names and duplicates (case-insensitive, first wins).
pub fn public_terms_removed(terms: Vec<String>, banks: &HashSet<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    terms
        .into_iter()
        .filter(|t| {
            let key = normalized_term(t);
            !key.is_empty() && !banks.contains(&key) && seen.insert(key)
        })
        .collect()
}

/// Everything personal the vault knows that might turn up in a log line: own
/// accounts, owners, IBANs and logins, and the counterparties of all bookings.
pub fn known_terms(
    v: &Vault,
    counterparties: &[String],
    catalog: &[BankDescriptor],
) -> Result<Vec<String>, String> {
    let accounts = v.list_accounts().map_err(|e| e.to_string())?;
    let connections = v.list_bank_connections().map_err(|e| e.to_string())?;
    let mut terms: Vec<String> = Vec::new();
    for a in &accounts {
        terms.extend([
            a.name.clone(),
            a.owner.clone(),
            a.iban.clone(),
            a.account_number.clone(),
        ]);
    }
    for c in &connections {
        terms.extend([c.display_name.clone(), c.login_name.clone()]);
    }
    terms.extend(counterparty_terms(counterparties));
    // Bank and connector names are not personal, and an account named like its
    // bank ("PayPal") must not blank the bank out of every message.
    let banks: HashSet<String> = catalog
        .iter()
        .map(|d| normalized_term(&d.name))
        .chain(accounts.iter().map(|a| normalized_term(&a.bank_name)))
        .collect();
    Ok(public_terms_removed(terms, &banks))
}

/// Swift's `"\(kind)"` of a UniFFI enum: the case name in lower camel case.
fn case_name<T: serde::Serialize>(v: &T) -> String {
    let name = serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();
    let mut chars = name.chars();
    chars
        .next()
        .map(|c| c.to_lowercase().chain(chars).collect())
        .unwrap_or_default()
}

fn connector_name(kind: &girocrab_core::ConnectorKind, catalog: &[BankDescriptor]) -> String {
    match catalog.iter().find(|d| &d.kind == kind) {
        None => case_name(kind),
        Some(d) if d.protocol.is_empty() || d.protocol == d.name => d.name.clone(),
        Some(d) => format!("{} ({})", d.name, d.protocol),
    }
}

fn log_lines(
    entries: &[SyncLogEntry],
    connectors: &HashMap<i64, girocrab_core::ConnectorKind>,
    catalog: &[BankDescriptor],
) -> Vec<LogLine> {
    entries
        .iter()
        .map(|e| {
            // The connection's own name is the user's; the connector kind is enough.
            let kind = e
                .connection_id
                .and_then(|id| connectors.get(&id))
                .map(|k| connector_name(k, catalog))
                .unwrap_or_else(|| "–".to_string());
            let date = jiff::Timestamp::from_millisecond(e.timestamp_ms)
                .map(|t| {
                    t.to_zoned_datetime(jiff::tz::TimeZone::system())
                        .strftime("%Y-%m-%d %H:%M")
                        .to_string()
                })
                .unwrap_or_default();
            let mut head = format!(
                "{date} · {kind} · {} · {}",
                case_name(&e.source),
                case_name(&e.outcome)
            );
            if e.imported > 0 {
                head += &format!(" · +{}", e.imported);
            }
            LogLine {
                head,
                message: e.message.replace('\n', " "),
            }
        })
        .collect()
}

/// `AppVersionDisplay.text`: the core version, plus the app's when they differ.
fn app_version() -> String {
    let core = girocrab_core::core_version();
    let app = env!("GIROCRAB_VERSION");
    if app.is_empty() || app == core {
        core
    } else {
        format!("{core} (App {app})")
    }
}

fn system_description() -> String {
    let os = std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("PRETTY_NAME="))
                .map(|v| v.trim_matches('"').to_string())
        })
        .unwrap_or_default();
    if os.is_empty() {
        "Linux".to_string()
    } else {
        format!("Linux · {os}")
    }
}

/// `Locale.current.identifier`: the messages locale without encoding ("de_DE").
fn language() -> String {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|v| !v.is_empty())
        .map(|v| v.split(['.', '@']).next().unwrap_or_default().to_string())
        .filter(|v| !v.is_empty() && v != "C" && v != "POSIX")
        .unwrap_or_else(|| crate::i18n::running().to_string())
}

/// BugReportView.prepareDiagnostics: header and sanitized log lines. Log
/// entries and payee names load together, or the log is left out: without
/// the names a log line could carry a payee into the issue.
pub fn prepare(v: Option<&Vault>, include_log: bool, demo: bool) -> Value {
    let catalog = girocrab_core::bank_catalog();
    let connections = v
        .and_then(|v| v.list_bank_connections().ok())
        .unwrap_or_default();
    let by_connection: HashMap<i64, girocrab_core::ConnectorKind> =
        connections.iter().map(|c| (c.id, c.connector)).collect();
    let mut connectors: Vec<String> = connections
        .iter()
        .map(|c| connector_name(&c.connector, &catalog))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    connectors.sort();
    let info = Info {
        app_version: app_version(),
        system: system_description(),
        architecture: std::env::consts::ARCH.to_string(),
        language: language(),
        demo,
        connectors,
    };
    let mut log = Vec::new();
    let mut log_unavailable = false;
    if include_log {
        let loaded = v.ok_or_else(String::new).and_then(|v| {
            let filter = SyncLogFilter {
                connection_id: None,
                only_errors: false,
                search: None,
                limit: LOG_ENTRIES,
            };
            let entries = v.list_sync_log(filter).map_err(|e| e.to_string())?;
            let names = v
                .counterparty_names(MAX_COUNTERPARTY_TERMS)
                .map_err(|e| e.to_string())?;
            let terms = known_terms(v, &names, &catalog)?;
            Ok((entries, terms))
        });
        match loaded {
            Ok((entries, terms)) => {
                log = sanitized_log(&log_lines(&entries, &by_connection, &catalog), &terms)
            }
            Err(_) => log_unavailable = v.is_some(),
        }
    }
    json!({ "info": info, "log": log, "log_unavailable": log_unavailable,
            "text": render(&info, &log) })
}

/// The issue text the Linux app shows: Strg+V instead of ⌘V.
pub fn paste_hint() -> String {
    tr(
        "📋 Der geschwärzte Screenshot liegt in deiner Zwischenablage – klicke hier ins Textfeld und drücke **Strg+V**, um ihn einzufügen.",
    )
}

/// Markdown body of the new issue. With `with_screenshot` false the paste
/// hint is left out.
pub fn issue_body(diagnostics: &str, with_screenshot: bool) -> String {
    let mut parts = vec![
        format!("### {}", tr("Was ist passiert?")),
        String::new(),
        tr(
            "<!-- Beschreibe kurz das Problem: Was hast du getan, was hast du erwartet, was ist stattdessen passiert? Bitte keine echten IBANs, Namen oder Buchungsdaten. -->",
        ),
        String::new(),
    ];
    if with_screenshot {
        parts.extend([
            format!("### {}", tr("Screenshot")),
            String::new(),
            paste_hint(),
            String::new(),
        ]);
    }
    // A fence longer than any backtick run in the text: a log line with "```"
    // must not end the block (and turn "@name" or "#123" live).
    let fence = "`".repeat(3.max(longest_backtick_run(diagnostics) + 1));
    parts.extend([
        format!("### {}", tr("Diagnose (automatisch bereinigt)")),
        String::new(),
        format!("{fence}text"),
        diagnostics.to_string(),
        fence,
    ]);
    parts.join("\n")
}

pub fn longest_backtick_run(s: &str) -> usize {
    let (mut longest, mut run) = (0, 0);
    for c in s.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    longest
}

/// Query component encoding: everything but the unreserved characters, so
/// "+" becomes `%2B` (GitHub decodes a bare "+" as a space).
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn issue_url_for_body(body: &str) -> String {
    format!(
        "{NEW_ISSUE}?title={}&body={}",
        encode("[BUG] "),
        encode(body)
    )
}

/// The prefilled `issues/new` URL, with log lines already passed through
/// [`sanitized_log`]. Drops log lines from the end (the oldest) until the
/// encoded URL fits [`MAX_URL_LENGTH`].
pub fn issue_url(info: &Info, sanitized_log: &[String], with_screenshot: bool) -> String {
    let mut log = sanitized_log.to_vec();
    let (mut body, mut url);
    loop {
        body = issue_body(&render(info, &log), with_screenshot);
        url = issue_url_for_body(&body);
        if url.len() <= MAX_URL_LENGTH || log.is_empty() {
            break;
        }
        log.pop();
    }
    // Without any log line still too long (not expected): cut the text.
    while url.len() > MAX_URL_LENGTH && !body.is_empty() {
        let keep = body.chars().count() - (body.chars().count() / 10).max(1);
        body = body.chars().take(keep).collect();
        url = issue_url_for_body(&body);
    }
    url
}

/// Private folder for the captured picture: `$XDG_RUNTIME_DIR/girocrab-report`
/// or a per-process folder in the temp dir, mode 0700. Emptied when the report
/// window closes and when the app quits.
pub fn temp_dir() -> Result<std::path::PathBuf, String> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute() && p.is_dir())
        .map(|p| p.join("girocrab-report"))
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("girocrab-report-{}", std::process::id()))
        });
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }
    Ok(dir)
}

/// Removes the pictures of the report window.
pub fn clear_temp_dir() {
    if let Ok(dir) = temp_dir() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(message: &str) -> LogLine {
        LogLine {
            head: "2026-11-14 10:00 · FinTS · manual · failed".into(),
            message: message.into(),
        }
    }

    fn info() -> Info {
        Info {
            app_version: "0.7.0".into(),
            system: "Linux · Debian GNU/Linux 13".into(),
            architecture: "x86_64".into(),
            language: "de_DE".into(),
            demo: false,
            connectors: vec!["Comdirect (REST)".into(), "Sparkasse (FinTS)".into()],
        }
    }

    /// Decodes the `body` query item of an issue URL.
    fn body_of(url: &str) -> String {
        let q = url.split("&body=").nth(1).unwrap();
        let bytes = q.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' {
                out.push(u8::from_str_radix(&q[i + 1..i + 3], 16).unwrap());
                i += 3;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn log_lines_lose_ibans_names_amounts_and_emails() {
        let log = [entry(
            "Konto DE89370400440532013000 von Erika Mustermann: Saldo 1.234,56 €, Kontakt erika@example.de",
        )];
        let lines = sanitized_log(&log, &["Erika Mustermann".into()]);
        let text = render(&info(), &lines);
        for leak in ["DE89", "0532013000", "Erika", "1.234,56", "example.de"] {
            assert!(!text.contains(leak), "{leak} in {text}");
        }
        assert!(text.contains("FinTS · manual · failed"), "{text}");
        assert!(
            text.contains("Sparkasse (FinTS)"),
            "header untouched: {text}"
        );
    }

    #[test]
    fn head_is_never_sanitized_and_empty_messages_keep_their_line() {
        let terms = public_terms_removed(
            vec![
                "PayPal".into(),
                "Erika Mustermann".into(),
                "erika mustermann".into(),
                " ".into(),
            ],
            &HashSet::from(["paypal".to_string()]),
        );
        assert_eq!(terms, vec!["Erika Mustermann".to_string()]);
        let log = [
            LogLine {
                head: "2026-11-14 10:00 · PayPal · manual · failed".into(),
                message: "PayPal API: Zugriff verweigert".into(),
            },
            entry(""),
        ];
        let lines = sanitized_log(&log, &["PayPal".into()]);
        assert!(lines[0].starts_with("2026-11-14 10:00 · PayPal · manual"));
        assert_eq!(lines[1], "2026-11-14 10:00 · FinTS · manual · failed");
    }

    #[test]
    fn only_counterparties_that_can_be_names_become_terms() {
        let names: Vec<String> = [
            "Erika Mustermann",
            "Mustermann",
            "Müller",
            "Meier",
            "VISA",
            "Zinsen",
            "Entgelt",
            "Online",
            "Gutschrift",
            "Kontoführung",
            "Lohn",
            "Ali",
            "REWE Markt GmbH",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            counterparty_terms(&names),
            [
                "Erika Mustermann",
                "Mustermann",
                "Müller",
                "Meier",
                "REWE Markt GmbH"
            ]
        );
    }

    #[test]
    fn issue_url_fits_and_drops_oldest_log_lines_first() {
        let log: Vec<LogLine> = (0..400)
            .map(|i| {
                entry(&format!(
                    "Zeile {i} · Zeitüberschreitung beim Verbindungsaufbau"
                ))
            })
            .collect();
        let url = issue_url(&info(), &sanitized_log(&log, &[]), true);
        assert!(url.starts_with(
            "https://github.com/GiroCrab/GiroCrab/issues/new?title=%5BBUG%5D%20&body="
        ));
        assert!(url.len() <= MAX_URL_LENGTH);
        let body = body_of(&url);
        assert!(body.contains("Zeile 0 "), "newest line kept");
        assert!(!body.contains("Zeile 399 "), "oldest line dropped");
        assert!(body.contains(&paste_hint()));
        assert!(body.ends_with("```"), "code block closed");
    }

    #[test]
    fn plus_signs_and_backticks_cannot_break_the_issue() {
        let log = [LogLine {
            head: "2026-11-14 10:00 · FinTS · manual · ok · +12".into(),
            message: "Antwort: ```json {} ``` @maintainer siehe #123".into(),
        }];
        let url = issue_url(&info(), &sanitized_log(&log, &[]), false);
        assert!(!url.contains('+') && url.contains("%2B12"), "{url}");
        let body = body_of(&url);
        assert!(
            body.contains("````text\n") && body.ends_with("\n````"),
            "{body}"
        );
        assert!(
            !body.contains(&paste_hint()),
            "no screenshot, no paste hint"
        );
    }
}
