//! Abruf-Protokoll (GiroCrab/GiroCrab#238): one row per finished bank sync, so
//! a failed fetch can be looked at afterwards ("Protokoll" window).
//!
//! The log lives in the vault (encrypted, local to this device: it is not part
//! of a vault merge). It holds the time, the connection, the result and the
//! error text – never a login, PIN, TAN or token (#196): the message is
//! scrubbed before it is stored ([`scrub`]). Rows older than seven days go,
//! and the oldest rows go once the log passes 10 MB.

use std::sync::LazyLock;

use regex::Regex;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter, types::Value};

use crate::CoreError;
use crate::dates::now_ms;
use crate::error::CoreResult;
use crate::vault::Vault;

/// How long a row is kept.
pub(crate) const KEEP_MS: i64 = 7 * 24 * 60 * 60 * 1000;
/// Size cap of the log (message + name + a fixed overhead per row).
pub(crate) const MAX_BYTES: i64 = 10 * 1024 * 1024;
/// Longest message that is stored.
const MAX_MESSAGE_CHARS: usize = 2000;
/// Per-row overhead counted against [`MAX_BYTES`].
const ROW_OVERHEAD: i64 = 64;

/// Result of a sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SyncLogOutcome {
    Ok,
    Error,
    Cancelled,
}

impl SyncLogOutcome {
    fn as_str(self) -> &'static str {
        match self {
            SyncLogOutcome::Ok => "ok",
            SyncLogOutcome::Error => "error",
            SyncLogOutcome::Cancelled => "cancelled",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "ok" => SyncLogOutcome::Ok,
            "cancelled" => SyncLogOutcome::Cancelled,
            _ => SyncLogOutcome::Error,
        }
    }
}

/// What started the sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SyncLogSource {
    /// The user asked for it (Abrufen, Sammelabruf).
    Manual,
    /// The scheduled background fetch.
    Automatic,
    /// A statement (Kontoauszug) download.
    Statements,
}

impl SyncLogSource {
    fn as_str(self) -> &'static str {
        match self {
            SyncLogSource::Manual => "manual",
            SyncLogSource::Automatic => "auto",
            SyncLogSource::Statements => "statements",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "auto" => SyncLogSource::Automatic,
            "statements" => SyncLogSource::Statements,
            _ => SyncLogSource::Manual,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SyncLogEntry {
    pub id: i64,
    /// Unix time in milliseconds.
    pub timestamp_ms: i64,
    /// `None` once the connection is gone; the name below stays.
    pub connection_id: Option<i64>,
    pub connection_name: String,
    pub source: SyncLogSource,
    pub outcome: SyncLogOutcome,
    /// Scrubbed error text (or a short note); empty for a plain success.
    pub message: String,
    /// New bookings of a successful sync.
    pub imported: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SyncLogFilter {
    pub connection_id: Option<i64>,
    /// Only failed syncs.
    pub only_errors: bool,
    /// Words (all must occur) in the connection name or the message.
    pub search: Option<String>,
    /// 0 = all.
    pub limit: u32,
}

/// Country, check digits, then letters and digits, in any case, with up to
/// two blanks (space, tab, non-breaking or narrow non-breaking space) between
/// any two of them. Only a candidate: see [`replace_ibans`]. The letter
/// classes are spelled out instead of `(?i)`: Unicode case folding would let
/// `[A-Z]` match the 3-byte KELVIN SIGN, and the byte slicing below would panic.
static IBAN_CANDIDATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b[A-Za-z]{2}[0-9]{2}(?:[ \t\u{00A0}\u{202F}]{0,2}[A-Za-z0-9]){11,30}").unwrap()
});

/// Every IBAN carries at least eight digits; "ab12 test case" does not.
fn looks_like_iban(compact: &str) -> bool {
    compact.chars().filter(char::is_ascii_digit).count() >= 8
}

/// Start of the last blank in `text[..end]` and where the word after it begins.
fn last_blank(text: &str, end: usize) -> Option<(usize, usize)> {
    text[..end]
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map(|(i, c)| (i, i + c.len_utf8()))
}

/// Drops trailing blank-separated words without a digit from `text[..end]`:
/// an IBAN group always has one ("… 0532 Fehler" ends after "0532").
fn without_wordy_tail(text: &str, mut end: usize) -> usize {
    while let Some((blank, word)) = last_blank(text, end) {
        if text[word..end].chars().any(|c| c.is_ascii_digit()) {
            break;
        }
        end = blank;
    }
    end
}

/// Replaces every IBAN in `s` with `f(compact IBAN)`. A known country's IBAN
/// ends after its length; a word that runs on past that point, or a trailing
/// word without a digit, is not part of it ("… 3201 Konto", "… 0532 Fehler",
/// "… 0044 ungültig"). An unspaced candidate with lower-case letters (hex IDs
/// look like that) counts only with a valid checksum and nothing glued on.
pub(crate) fn replace_ibans(s: &str, f: impl Fn(&str) -> String) -> String {
    use crate::iban::{iban_length, validate_iban};
    // Unicode: "…0044 ungültig" – the candidate (ASCII only) stops at "ü",
    // but the word goes on.
    let continues_word = |rest: &str| rest.starts_with(char::is_alphanumeric);
    let mut out = String::with_capacity(s.len());
    let (mut last, mut at) = (0, 0);
    while let Some(m) = IBAN_CANDIDATE.find_at(s, at) {
        let text = m.as_str();
        let mut end = text.len();
        if let Some(length) = iban_length(&text[..2].to_ascii_uppercase()) {
            let mut count = 0;
            for (i, c) in text.char_indices().filter(|(_, c)| !c.is_whitespace()) {
                count += 1;
                if count == length {
                    end = i + c.len_utf8();
                    break;
                }
            }
        }
        // a cut inside a word: that word is not part of the IBAN
        let spaced = text[..end].contains(char::is_whitespace);
        if spaced && continues_word(&s[m.start() + end..]) {
            end = last_blank(text, end).map_or(end, |(blank, _)| blank);
        }
        end = without_wordy_tail(text, end);
        let iban = text[..end].trim_end();
        let compact: String = iban.chars().filter(|c| !c.is_whitespace()).collect();
        let lower_token =
            !iban.contains(char::is_whitespace) && iban.chars().any(|c| c.is_ascii_lowercase());
        let glued = continues_word(&s[m.start() + iban.len()..]);
        if !looks_like_iban(&compact) || (lower_token && (glued || !validate_iban(&compact))) {
            at = m.start() + 1; // the match starts with an ASCII letter
            continue;
        }
        out.push_str(&s[last..m.start()]);
        out.push_str(&f(&compact));
        last = m.start() + iban.len();
        at = last;
    }
    out.push_str(&s[last..]);
    out
}

/// Removes what must not end up in the log (#196): the login name, values
/// that follow a secret-like key (`pin=…`, `"accessToken":"…"`,
/// `JSESSIONID=…`, `Bearer …`, `PIN 12345`), cookie headers, IBANs (kept as
/// first four and last four characters), phone numbers, long tokens such as
/// JWTs and any other run of six or more
/// digits (TANs, customer and account numbers). URLs stay whole for
/// debugging; the public report strips their paths. The result is cut to
/// [`MAX_MESSAGE_CHARS`].
pub(crate) fn scrub(message: &str, login: &str) -> String {
    let mut s = message.trim().to_string();
    let login = login.trim();
    if login.chars().count() >= 4 {
        s = s.replace(login, "•••");
    } else if login.chars().count() >= 2 {
        // A short login ("jay", "m1") is replaced only as a whole word: plain
        // `replace` would also eat the same letters inside other words.
        // (`\b` needs a word character at both ends of the login.)
        let (first, last) = (login.chars().next(), login.chars().last());
        if first.is_some_and(char::is_alphanumeric) && last.is_some_and(char::is_alphanumeric) {
            let pattern = format!(r"\b{}\b", regex::escape(login));
            if let Ok(re) = Regex::new(&pattern) {
                s = re.replace_all(&s, "•••").into_owned();
            }
        }
    }
    // before the key rule, or `Authorization: Bearer <token>` would leave the token behind
    static BEARER: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)\b(bearer|basic)\s+[A-Za-z0-9._~+/=-]{8,}").unwrap());
    s = BEARER.replace_all(&s, "$1 •••").into_owned();
    // A cookie header holds several `name=value` pairs: drop the rest of the line.
    static COOKIE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)\b(set-cookie|cookie)\s*:[^\r\n]*").unwrap());
    s = COOKIE.replace_all(&s, "$1: •••").into_owned();
    // Longer keys match as a suffix ("accessToken", "JSESSIONID"); `pin` and
    // `tan` only as whole words, or "Bestand: 12" would lose its value.
    static SECRET: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?i)("?\b(?:pin|tan|[a-z_]*(?:passwort|password|kennwort|token|secret|api[-_ ]?key|authorization|session_?id|cookie))\b"?)\s*[:=]\s*("[^"]*"|'[^']*'|\S+)"#).unwrap()
    });
    s = SECRET.replace_all(&s, "$1: •••").into_owned();
    // "PIN 12345": only a value with a digit, so "TAN eingeben" stays readable.
    static SPACED_PIN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)\b(pin|tan)\s+[a-z]*\d[a-z0-9]*\b").unwrap());
    s = SPACED_PIN.replace_all(&s, "$1 •••").into_owned();
    s = replace_ibans(&s, |compact| {
        let n = compact.chars().count();
        format!(
            "{}…{}",
            compact.chars().take(4).collect::<String>(),
            compact.chars().skip(n - 4).collect::<String>()
        )
    });
    // German and international phone numbers, also grouped ("0170 123 45 67"),
    // before the digit rule would leave the short groups behind.
    static PHONE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?:\+\d{1,3}[ /-]?|\b0)\d{2,5}(?:[ /-]\d{2,8}){1,4}\b").unwrap()
    });
    s = PHONE.replace_all(&s, "•••").into_owned();
    // JWTs and other base64 tokens: long runs mixing digits with letters
    // beyond hex. Hex ids and UUIDs (consent, request ids) stay for support;
    // plain words and digit runs fall through (the latter to the digit rule).
    static TOKEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[A-Za-z0-9_\-.]{24,}").unwrap());
    s = TOKEN
        .replace_all(&s, |c: &regex::Captures| {
            let m = c.get(0).expect("whole match");
            let t = m.as_str();
            let in_url = s[..m.start()]
                .rsplit(char::is_whitespace)
                .next()
                .is_some_and(|word| word.contains("://"));
            let has = |f: fn(&u8) -> bool| t.as_bytes().iter().any(f);
            if !in_url
                && has(u8::is_ascii_digit)
                && has(|b| b.is_ascii_alphabetic() && !b.is_ascii_hexdigit())
            {
                "•••".to_string()
            } else {
                t.to_string()
            }
        })
        .into_owned();
    static DIGITS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d{6,}").unwrap());
    s = DIGITS.replace_all(&s, "•••").into_owned();
    if s.chars().count() > MAX_MESSAGE_CHARS {
        s = s.chars().take(MAX_MESSAGE_CHARS).collect::<String>() + "…";
    }
    s
}

/// Drops rows past the retention time, then the oldest rows until the log is
/// within `max_bytes`.
fn prune(conn: &Connection, now: i64, keep_ms: i64, max_bytes: i64) -> CoreResult<()> {
    conn.execute("DELETE FROM sync_log WHERE ts_ms < ?1", [now - keep_ms])?;
    conn.execute(
        "DELETE FROM sync_log WHERE id IN (
             SELECT id FROM (
                 SELECT id, SUM(length(message) + length(connection_name) + ?2)
                            OVER (ORDER BY id DESC) AS cum
                   FROM sync_log)
              WHERE cum > ?1)",
        params![max_bytes, ROW_OVERHEAD],
    )?;
    Ok(())
}

fn entry_from_row(r: &rusqlite::Row) -> rusqlite::Result<SyncLogEntry> {
    Ok(SyncLogEntry {
        id: r.get(0)?,
        timestamp_ms: r.get(1)?,
        connection_id: r.get(2)?,
        connection_name: r.get(3)?,
        source: SyncLogSource::parse(&r.get::<_, String>(4)?),
        outcome: SyncLogOutcome::parse(&r.get::<_, String>(5)?),
        message: r.get(6)?,
        imported: r.get::<_, i64>(7)?.max(0) as u32,
    })
}

impl Vault {
    /// Stores one row. `now`, `keep_ms` and `max_bytes` are parameters so tests can
    /// age the log.
    pub(crate) fn log_sync_at(
        &self,
        entry: (Option<i64>, SyncLogSource, SyncLogOutcome, &str, u32),
        now: i64,
        keep_ms: i64,
        max_bytes: i64,
    ) -> CoreResult<()> {
        let (connection_id, source, outcome, message, imported) = entry;
        let db = self.db()?;
        let (name, login): (String, String) = match connection_id {
            Some(id) => db
                .query_row(
                    "SELECT display_name, login_name FROM bank_connections WHERE id = ?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?
                .unwrap_or_default(),
            None => Default::default(),
        };
        db.execute(
            "INSERT INTO sync_log(ts_ms, connection_id, connection_name, source, outcome, message, imported)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                now,
                connection_id,
                name,
                source.as_str(),
                outcome.as_str(),
                scrub(message, &login),
                imported
            ],
        )?;
        prune(&db, now, keep_ms, max_bytes)
    }
}

#[uniffi::export]
impl Vault {
    /// Appends a finished sync to the Abrufprotokoll. The text is scrubbed of
    /// logins, PINs, TANs, tokens, IBANs and long numbers first.
    pub fn log_sync(
        &self,
        connection_id: Option<i64>,
        source: SyncLogSource,
        outcome: SyncLogOutcome,
        message: String,
        imported: u32,
    ) -> Result<(), CoreError> {
        self.log_sync_at(
            (connection_id, source, outcome, &message, imported),
            now_ms(),
            KEEP_MS,
            MAX_BYTES,
        )?;
        Ok(())
    }

    /// The log, newest first.
    pub fn list_sync_log(&self, filter: SyncLogFilter) -> Result<Vec<SyncLogEntry>, CoreError> {
        let db = self.db()?;
        let mut wh: Vec<String> = vec!["1 = 1".into()];
        let mut args: Vec<Value> = vec![];
        if let Some(id) = filter.connection_id {
            args.push(Value::Integer(id));
            wh.push(format!("connection_id = ?{}", args.len()));
        }
        if filter.only_errors {
            wh.push("outcome = 'error'".into());
        }
        if let Some(q) = filter.search.as_deref() {
            for term in q.split_whitespace() {
                let like = format!(
                    "%{}%",
                    term.replace('\\', "\\\\")
                        .replace('%', "\\%")
                        .replace('_', "\\_")
                );
                args.push(Value::Text(like));
                let n = args.len();
                wh.push(format!(
                    "(message LIKE ?{n} ESCAPE '\\' OR connection_name LIKE ?{n} ESCAPE '\\')"
                ));
            }
        }
        let limit = if filter.limit == 0 {
            String::new()
        } else {
            format!(" LIMIT {}", filter.limit)
        };
        let sql = format!(
            "SELECT id, ts_ms, connection_id, connection_name, source, outcome, message, imported
               FROM sync_log WHERE {} ORDER BY ts_ms DESC, id DESC{limit}",
            wh.join(" AND ")
        );
        let mut stmt = db.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args.iter()), entry_from_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Empties the log.
    pub fn clear_sync_log(&self) -> Result<(), CoreError> {
        self.db()?.execute("DELETE FROM sync_log", [])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
