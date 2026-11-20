//! Beleg-Abruf & PostBox: encrypted document storage inside the vault.
//!
//! All bank statements, ETF trade confirmations and receipts live as BLOBs in
//! the SQLCipher table `booking_documents`. Documents arrive from the bank
//! connectors (Trade Republic timelineDetailV2, comdirect PostBox, FinTS
//! HKEKA) or by drag & drop; dropped files are matched to a booking by
//! amount, date and merchant name.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;
use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{CoreError, CoreResult};
use crate::i18n::{tr, trf};
use crate::models::Booking;
use crate::money::format_cents;
use crate::vault::{Vault, sql_int_list};

pub const DOC_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS booking_documents (
    id          INTEGER PRIMARY KEY,
    booking_id  INTEGER REFERENCES transactions(id) ON DELETE SET NULL,
    account_id  INTEGER REFERENCES accounts(id) ON DELETE SET NULL,
    title       TEXT    NOT NULL DEFAULT '',
    doc_type    TEXT    NOT NULL DEFAULT 'beleg',   -- beleg | abrechnung | kontoauszug | postbox | steuer
    mime_type   TEXT    NOT NULL DEFAULT 'application/pdf',
    file_name   TEXT    NOT NULL DEFAULT '',
    pdf_bytes   BLOB    NOT NULL,
    source_url  TEXT    NOT NULL DEFAULT '',
    doc_date    TEXT    NOT NULL DEFAULT '',
    created_at  INTEGER NOT NULL DEFAULT (strftime('%s','now')),
    exported_at INTEGER                                -- copied to the mirror folder (GitHub #57)
);
CREATE UNIQUE INDEX IF NOT EXISTS booking_documents_source ON booking_documents(source_url) WHERE source_url != '';
CREATE INDEX IF NOT EXISTS booking_documents_booking ON booking_documents(booking_id);
CREATE INDEX IF NOT EXISTS booking_documents_account ON booking_documents(account_id);
-- Everything the document list shows, so that it is read from this index and
-- never from the table: there the columns after pdf_bytes (source_url,
-- doc_date, created_at) are only reached by reading through the whole PDF.
CREATE INDEX IF NOT EXISTS booking_documents_info ON booking_documents(
    doc_date DESC, id DESC, booking_id, account_id, title, doc_type, mime_type, file_name,
    length(pdf_bytes), source_url, created_at);
"#;

#[derive(Debug, Clone, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DocumentInfo {
    pub id: i64,
    pub booking_id: Option<i64>,
    pub account_id: Option<i64>,
    pub title: String,
    pub doc_type: String,
    pub mime_type: String,
    pub file_name: String,
    pub size_bytes: u64,
    pub source_url: String,
    pub doc_date: String,
    pub created_at: i64,
    /// "METRO · −56,13 EUR · 2026-11-07" (empty if not linked)
    pub booking_label: String,
    pub account_name: String,
    /// Read: opened in the preview, marked by hand, or old on arrival (see [`UNSEEN_DAYS`]).
    pub seen: bool,
}

#[derive(Debug, Clone, Default, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DocumentFilter {
    pub account_id: Option<i64>,
    pub doc_type: Option<String>,
    pub search: Option<String>,
    /// Only documents not linked to a booking (PostBox / unmatched).
    pub unlinked_only: bool,
    /// Only documents of accounts of this bank connection (`accounts.connection_id` of the
    /// document's account; documents without an account never match).
    pub connection_id: Option<i64>,
}

/// Document delivered by a bank connector.
#[derive(Debug, Clone)]
pub struct RemoteDocument {
    pub account_ref: String,
    /// external_id of the booking it belongs to (None = account / PostBox).
    pub booking_external_id: Option<String>,
    pub title: String,
    pub doc_type: String,
    pub mime_type: String,
    pub file_name: String,
    pub bytes: Vec<u8>,
    pub source_url: String,
    pub date: String,
}

pub fn sniff_mime(bytes: &[u8], file_name: &str) -> String {
    let lower = file_name.to_lowercase();
    if bytes.starts_with(b"%PDF") || lower.ends_with(".pdf") {
        "application/pdf"
    } else if bytes.starts_with(&[0x89, b'P', b'N', b'G']) || lower.ends_with(".png") {
        "image/png"
    } else if bytes.starts_with(&[0xFF, 0xD8])
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
    {
        "image/jpeg"
    } else if lower.ends_with(".heic") {
        "image/heic"
    } else {
        "application/octet-stream"
    }
    .to_string()
}

pub(crate) fn title_from_file(file_name: &str) -> String {
    let stem = file_name
        .rsplit_once('.')
        .map(|(s, _)| s)
        .unwrap_or(file_name);
    stem.replace(['_', '-'], " ").trim().to_string()
}

/// A document that arrives dated longer ago than this is "read" from the start: a history fetch
/// or an archive import must not flood the unread count. Newer ones are unread until opened.
pub(crate) const UNSEEN_DAYS: i64 = 45;

/// `seen_at` of a document arriving with this date: now if it is old, else NULL (unread). A
/// document without a date counts as new.
fn seen_on_arrival(date: Option<jiff::civil::Date>) -> Option<i64> {
    let old = crate::dates::days_between(date?, crate::dates::today()) > UNSEEN_DAYS;
    old.then(|| jiff::Timestamp::now().as_second())
}

/// Marks documents read (`seen` true) or unread; returns how many changed.
pub(crate) fn mark_seen(conn: &Connection, ids: &[i64], seen: bool) -> CoreResult<u32> {
    let mut changed = 0;
    for id in ids {
        changed += conn.execute(
            "UPDATE booking_documents SET seen_at = CASE WHEN ?2 THEN strftime('%s','now') ELSE NULL END
              WHERE id = ?1 AND (seen_at IS NOT NULL) != ?2",
            params![id, seen],
        )? as u32;
    }
    Ok(changed)
}

pub(crate) fn insert_doc(
    conn: &Connection,
    booking_id: Option<i64>,
    account_id: Option<i64>,
    title: &str,
    doc_type: &str,
    mime: &str,
    file_name: &str,
    bytes: &[u8],
    source_url: &str,
    date: &str,
) -> CoreResult<Option<i64>> {
    if bytes.is_empty() {
        return Err(CoreError::InvalidInput {
            message: tr("Leere Datei").into(),
        });
    }
    // The date comes from bank or PDF text: anything but a real day is stored empty, so the
    // document falls back to its booking's date.
    let date = crate::dates::parse_iso(date);
    let n = conn.execute(
        "INSERT OR IGNORE INTO booking_documents(booking_id, account_id, title, doc_type, mime_type, file_name, pdf_bytes, source_url, doc_date, seen_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![booking_id, account_id, title, doc_type, mime, file_name, bytes, source_url, date.map(crate::dates::iso).unwrap_or_default(), seen_on_arrival(date)],
    )?;
    Ok(if n > 0 {
        Some(conn.last_insert_rowid())
    } else {
        None
    })
}

// ------------------------------------------------------------------ PDF text extraction

fn literal_strings(content: &[u8], out: &mut String) {
    let mut i = 0;
    while i < content.len() {
        if content[i] == b'(' {
            let mut depth = 1;
            let mut s = Vec::new();
            i += 1;
            while i < content.len() && depth > 0 {
                match content[i] {
                    b'\\' if i + 1 < content.len() => {
                        i += 1;
                        let c = match content[i] {
                            b'n' => b'\n',
                            b'r' => b' ',
                            b't' => b' ',
                            other => other,
                        };
                        s.push(c);
                    }
                    b'(' => {
                        depth += 1;
                        s.push(b'(');
                    }
                    b')' => {
                        depth -= 1;
                        if depth > 0 {
                            s.push(b')');
                        }
                    }
                    c => s.push(c),
                }
                i += 1;
            }
            // PDFDocEncoding ≈ Latin-1 for the characters we need (€ 0xA0 → handled by "EUR")
            out.extend(s.iter().map(|&b| if b == 0x80 { '€' } else { b as char }));
            out.push(' ');
        } else {
            i += 1;
        }
    }
}

/// Zip-bomb guards for [`extract_pdf_text`]. The total keeps a hostile PDF from
/// inflating `MAX_PDF_STREAMS` streams of `MAX_INFLATED_STREAM` each (2 GB).
const MAX_INFLATED_STREAM: usize = 8 * 1024 * 1024;
const MAX_INFLATED_TOTAL: usize = 32 * 1024 * 1024;
const MAX_PDF_STREAMS: usize = 256;
const MAX_PDF_TEXT: usize = 4 * 1024 * 1024;

/// Best-effort text of a PDF: inflates FlateDecode streams and collects the
/// literal strings of text operators. Good enough for receipts and bank PDFs;
/// the app passes PDFKit/Vision text as an additional hint.
pub fn extract_pdf_text(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut streams = 0usize;
    let mut inflate_budget = MAX_INFLATED_TOTAL;
    let mut i = 0;
    while let Some(pos) = find(bytes, b"stream", i) {
        let dict_start = bytes[..pos]
            .iter()
            .rposition(|&b| b == b'<')
            .map(|p| p.saturating_sub(400))
            .unwrap_or(0);
        let dict = &bytes[dict_start..pos];
        let mut start = pos + 6;
        if bytes.get(start) == Some(&b'\r') {
            start += 1;
        }
        if bytes.get(start) == Some(&b'\n') {
            start += 1;
        }
        let Some(end) = find(bytes, b"endstream", start) else {
            break;
        };
        let data = &bytes[start..end];
        i = end + 9;
        if out.len() >= MAX_PDF_TEXT || streams >= MAX_PDF_STREAMS {
            break;
        }
        streams += 1;
        // Images hold no text; skipping them before inflation keeps them out of the budget.
        if find(dict, b"Image", 0).is_some() {
            continue;
        }
        if find(dict, b"FlateDecode", 0).is_some() {
            let limit = MAX_INFLATED_STREAM.min(inflate_budget);
            if limit > 0 {
                let used =
                    match miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(data, limit) {
                        Ok(inflated) => {
                            literal_strings(&inflated, &mut out);
                            inflated.len()
                        }
                        // A stream over the limit is dropped, but its inflation still counts.
                        Err(e) => e.output.len(),
                    };
                // `used <= limit <= inflate_budget`: no underflow.
                inflate_budget -= used;
            }
        } else {
            literal_strings(data, &mut out);
        }
    }
    out
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from >= hay.len() {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

// ------------------------------------------------------------------ matching

/// Amounts (cents, positive) mentioned in a text: "56,13", "1.234,56 €", "EUR 12.50".
pub fn amounts_in(text: &str) -> Vec<i64> {
    static AMOUNT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?:^|[^\d,.])(\d{1,3}(?:\.\d{3})+,\d{2}|\d+,\d{2}|\d+\.\d{2})").unwrap()
    });
    let mut out: Vec<i64> = Vec::new();
    for c in AMOUNT.captures_iter(text) {
        // The trailing separator is not part of the match (so "12,50 13,00"
        // finds both); only a following digit disqualifies it.
        if text[c.get(1).map_or(0, |m| m.end())..]
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_digit())
        {
            continue;
        }
        if let Ok(v) = crate::money::parse_cents(&c[1]) {
            let v = v.abs();
            if v > 0 && !out.contains(&v) {
                out.push(v);
            }
        }
    }
    out
}

/// Dates mentioned in a text as ISO strings.
pub fn dates_in(text: &str) -> Vec<String> {
    static GERMAN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\b(\d{1,2})\.(\d{1,2})\.(\d{4}|\d{2})\b").unwrap());
    static ISO: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?:^|\D)(20\d{2})[-_]?(\d{2})[-_]?(\d{2})").unwrap());
    let mut out = Vec::new();
    for c in GERMAN.captures_iter(text) {
        let (d, m): (u32, u32) = (c[1].parse().unwrap_or(0), c[2].parse().unwrap_or(0));
        let Some(date) = crate::dates::year(&c[3]).and_then(|y| crate::dates::ymd(y, m, d)) else {
            continue;
        };
        let iso = crate::dates::iso(date);
        if !out.contains(&iso) {
            out.push(iso);
        }
    }
    for c in ISO.captures_iter(text) {
        if text[c.get(3).map_or(0, |m| m.end())..]
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_digit())
        {
            continue;
        }
        let s = format!("{}-{}-{}", &c[1], &c[2], &c[3]);
        if jiff::civil::Date::strptime("%Y-%m-%d", &s).is_ok() && !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

fn days_between(a: &str, b: &str) -> Option<i64> {
    let a = jiff::civil::Date::strptime("%Y-%m-%d", a).ok()?;
    let b = jiff::civil::Date::strptime("%Y-%m-%d", b).ok()?;
    Some(crate::dates::days_between(b, a).abs())
}

/// Days between a receipt date and a booking that score full date points; a
/// booking without an amount hit needs them (name 3 + date 3 >= 6).
const CLOSE_DAYS: i64 = 3;

/// Picks the booking a dropped receipt belongs to. Returns (booking, score).
pub fn best_match<'a>(bookings: &'a [Booking], text: &str) -> Option<(&'a Booking, i32)> {
    let amounts = amounts_in(text);
    let dates = dates_in(text);
    let lower = text.to_lowercase();
    let mut best: Option<(&Booking, i32)> = None;
    for b in bookings {
        let mut score = 0;
        let amount_hit = amounts.contains(&b.amount_cents.abs());
        if amount_hit {
            score += 5;
        }
        let name_hit = {
            let n = b.merchant.name.to_lowercase();
            let first = n.split_whitespace().next().unwrap_or("").to_string();
            (!first.is_empty() && first.len() >= 3 && lower.contains(&first))
                || lower.contains(&b.merchant.key.replace('-', " "))
        };
        if name_hit {
            score += 3;
        }
        if let Some(min_days) = dates
            .iter()
            .filter_map(|d| days_between(d, &b.booking_date))
            .min()
        {
            score += match min_days {
                d if d <= CLOSE_DAYS => 3,
                d if d <= 10 => 2,
                d if d <= 45 => 0,
                _ => -4,
            };
        }
        if !(amount_hit || (name_hit && score >= 6)) {
            continue;
        }
        if best.map(|(_, s)| score > s).unwrap_or(true) {
            best = Some((b, score));
        }
    }
    best.filter(|(_, s)| *s >= 5)
}

fn label(b: &Booking) -> String {
    format!(
        "{} · {} · {}",
        b.merchant.name,
        format_cents(b.amount_cents, b.currency.clone()),
        b.booking_date
    )
}

fn row_to_info(r: &rusqlite::Row) -> rusqlite::Result<DocumentInfo> {
    let cp: Option<String> = r.get(12)?;
    let amount: Option<i64> = r.get(13)?;
    let date: Option<String> = r.get(14)?;
    let booking_label = match (cp, amount, date) {
        (Some(cp), Some(a), Some(d)) => {
            let m = crate::merchants::MerchantResolver::resolve(&cp, "");
            format!("{} · {} · {}", m.name, format_cents(a, "EUR".into()), d)
        }
        _ => String::new(),
    };
    Ok(DocumentInfo {
        id: r.get(0)?,
        booking_id: r.get(1)?,
        account_id: r.get(2)?,
        title: r.get(3)?,
        doc_type: r.get(4)?,
        mime_type: r.get(5)?,
        file_name: r.get(6)?,
        size_bytes: r.get::<_, i64>(7)? as u64,
        source_url: r.get(8)?,
        doc_date: r.get(9)?,
        created_at: r.get(10)?,
        account_name: r.get::<_, Option<String>>(11)?.unwrap_or_default(),
        booking_label,
        seen: r.get(15)?,
    })
}

/// A link to a deleted booking or account reads as none (what the old
/// `ON DELETE SET NULL` did): `t` and `da` only join live rows.
const SELECT_INFO: &str = "SELECT d.id, t.id, COALESCE(da.id, t.account_id), d.title, d.doc_type, d.mime_type, d.file_name,
        length(d.pdf_bytes), d.source_url, COALESCE(NULLIF(d.doc_date,''), t.booking_date, ''), d.created_at, a.name,
        t.counterparty, t.amount_cents, t.booking_date, d.seen_at IS NOT NULL
   FROM booking_documents d
   LEFT JOIN transactions t ON t.id = d.booking_id AND t.deleted_ms IS NULL
   LEFT JOIN accounts da ON da.id = d.account_id AND da.deleted_ms IS NULL
   LEFT JOIN accounts a ON a.id = COALESCE(da.id, t.account_id)";

fn safe_name(s: &str) -> String {
    let name = s
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '.' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("_");
    // No leading dots: "." / ".." would leave the target folder, ".x" is hidden.
    let name = name.trim_start_matches('.');
    if name.is_empty() {
        "_".into()
    } else {
        name.to_string()
    }
}

/// Shortens a file name to `max` characters (never splits a UTF-8 character).
fn truncate_chars(s: &mut String, max: usize) {
    if let Some((i, _)) = s.char_indices().nth(max) {
        s.truncate(i);
    }
}

impl Vault {
    /// The bookings `best_match` can accept for `text`: those with one of its
    /// amounts, or (for a name hit) booked within `CLOSE_DAYS` of one of its dates.
    fn bookings_for_matching(&self, text: &str) -> CoreResult<Vec<Booking>> {
        // Inlined values, all built here from integers and parsed dates. Two IN
        // lists keep the SQL flat however many amounts and dates a long PDF names.
        let amounts: Vec<i64> = amounts_in(text).iter().flat_map(|a| [*a, -a]).collect();
        let mut days: Vec<String> = dates_in(text)
            .iter()
            .filter_map(|d| jiff::civil::Date::strptime("%Y-%m-%d", d).ok())
            // "gültig bis 31.12.9999" is common: jiff dates end there, so skip what lies past it.
            .flat_map(|d| {
                (-CLOSE_DAYS..=CLOSE_DAYS).filter_map(move |k| crate::dates::add_days(d, k))
            })
            .map(|d| d.strftime("'%Y-%m-%d'").to_string())
            .collect();
        days.sort_unstable();
        days.dedup();
        let mut ors = Vec::new();
        if !amounts.is_empty() {
            // `amount_cents IN` (not `ABS(…)`) can use `tx_amount_date`.
            ors.push(format!("t.amount_cents IN ({})", sql_int_list(&amounts)));
        }
        if !days.is_empty() {
            ors.push(format!("t.booking_date IN ({})", days.join(",")));
        }
        if ors.is_empty() {
            return Ok(Vec::new());
        }
        self.bookings_where(&*self.db()?, &ors.join(" OR "), [])
    }
}

/// The document list. `INDEXED BY` makes SQLite answer it from
/// `booking_documents_info` alone; on its own the planner scans the table and
/// reads through every stored PDF (all columns used here must be in that
/// index – a test checks the plan).
fn list_all_documents_sql() -> String {
    let select = SELECT_INFO.replace(
        "FROM booking_documents d",
        "FROM booking_documents d INDEXED BY booking_documents_info",
    );
    format!(
        "{select} WHERE (?1 IS NULL OR COALESCE(da.id, t.account_id) = ?1)
            AND (?2 IS NULL OR d.doc_type = ?2)
            AND (?3 IS NULL OR d.title LIKE ?3 OR d.file_name LIKE ?3 OR t.counterparty LIKE ?3)
            AND (?4 = 0 OR t.id IS NULL)
            AND (?5 IS NULL OR a.connection_id = ?5)
         ORDER BY COALESCE(NULLIF(d.doc_date,''), t.booking_date) DESC, d.id DESC"
    )
}

#[uniffi::export]
impl Vault {
    pub fn list_documents_for_booking(
        &self,
        booking_id: i64,
    ) -> Result<Vec<DocumentInfo>, CoreError> {
        let db = self.db()?;
        let mut st = db.prepare(&format!(
            "{SELECT_INFO} WHERE d.booking_id = ?1 AND t.id IS NOT NULL ORDER BY d.doc_date DESC, d.id DESC"
        ))?;
        let rows = st
            .query_map([booking_id], row_to_info)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn list_all_documents(
        &self,
        filter: DocumentFilter,
    ) -> Result<Vec<DocumentInfo>, CoreError> {
        let db = self.db()?;
        let mut st = db.prepare(&list_all_documents_sql())?;
        let search = filter
            .search
            .filter(|s| !s.trim().is_empty())
            .map(|s| format!("%{}%", s.trim()));
        let rows = st
            .query_map(
                params![
                    filter.account_id,
                    filter.doc_type,
                    search,
                    filter.unlinked_only as i64,
                    filter.connection_id
                ],
                row_to_info,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Marks documents read (`seen` true) or unread. Returns how many changed.
    pub fn mark_documents_seen(&self, doc_ids: Vec<i64>, seen: bool) -> Result<u32, CoreError> {
        let db = self.db()?;
        mark_seen(&db, &doc_ids, seen)
    }

    /// Marks every unread document read; with `account_id` only those of that account (its own
    /// and those of its bookings). Returns how many changed.
    pub fn mark_all_documents_seen(&self, account_id: Option<i64>) -> Result<u32, CoreError> {
        let db = self.db()?;
        Ok(db.execute(
            "UPDATE booking_documents SET seen_at = strftime('%s','now')
              WHERE seen_at IS NULL
                AND (?1 IS NULL OR account_id = ?1
                     OR booking_id IN (SELECT id FROM transactions WHERE account_id = ?1))",
            [account_id],
        )? as u32)
    }

    /// Number of unread documents (for the badge in the sidebar).
    pub fn unseen_document_count(&self) -> Result<u32, CoreError> {
        let db = self.db()?;
        Ok(db.query_row(
            "SELECT count(*) FROM booking_documents WHERE seen_at IS NULL",
            [],
            |r| r.get::<_, i64>(0),
        )? as u32)
    }

    pub fn get_document_bytes(&self, doc_id: i64) -> Result<Vec<u8>, CoreError> {
        let db = self.db()?;
        db.query_row(
            "SELECT pdf_bytes FROM booking_documents WHERE id = ?1",
            [doc_id],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| CoreError::NotFound {
            what: trf("Beleg {0}", &[&doc_id]),
        })
    }

    /// Attaches a PDF/image to a booking (drag & drop onto a row / the inspector).
    pub fn attach_document_to_booking(
        &self,
        booking_id: i64,
        file_name: String,
        bytes: Vec<u8>,
    ) -> Result<i64, CoreError> {
        let db = self.db()?;
        let (account_id, date): (i64, String) = db
            .query_row(
                "SELECT account_id, booking_date FROM transactions WHERE id = ?1 AND deleted_ms IS NULL",
                [booking_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| CoreError::NotFound {
                what: trf("Buchung {0}", &[&booking_id]),
            })?;
        let mime = sniff_mime(&bytes, &file_name);
        let id = insert_doc(
            &db,
            Some(booking_id),
            Some(account_id),
            &title_from_file(&file_name),
            "beleg",
            &mime,
            &file_name,
            &bytes,
            "",
            &date,
        )?
        .ok_or_else(|| CoreError::InvalidInput {
            message: tr("Beleg bereits vorhanden").into(),
        })?;
        // added by hand: the user knows it
        mark_seen(&db, &[id], true)?;
        Ok(id)
    }

    /// Stores a document for an account without a booking (PostBox).
    pub fn attach_document_to_account(
        &self,
        account_id: i64,
        title: String,
        file_name: String,
        bytes: Vec<u8>,
        doc_type: String,
    ) -> Result<i64, CoreError> {
        let db = self.db()?;
        let mime = sniff_mime(&bytes, &file_name);
        let today = crate::dates::today_iso();
        let id = insert_doc(
            &db,
            None,
            Some(account_id),
            &title,
            &doc_type,
            &mime,
            &file_name,
            &bytes,
            "",
            &today,
        )?
        .ok_or_else(|| CoreError::InvalidInput {
            message: tr("Dokument bereits vorhanden").into(),
        })?;
        mark_seen(&db, &[id], true)?;
        Ok(id)
    }

    /// Re-links (or unlinks) a document.
    pub fn link_document(&self, doc_id: i64, booking_id: Option<i64>) -> Result<(), CoreError> {
        let db = self.db()?;
        db.execute(
            "UPDATE booking_documents SET booking_id = ?1,
                account_id = COALESCE((SELECT account_id FROM transactions WHERE id = ?1), account_id) WHERE id = ?2",
            params![booking_id, doc_id],
        )?;
        Ok(())
    }

    /// Moves a document to another account, or to none (`None`). A booking of
    /// another account it was linked to is unlinked; one of the new account
    /// stays (#838: PostBox documents GiroCrab put on the wrong account).
    pub fn set_document_account(
        &self,
        doc_id: i64,
        account_id: Option<i64>,
    ) -> Result<(), CoreError> {
        let db = self.db()?;
        if let Some(a) = account_id {
            let exists: bool = db.query_row(
                "SELECT EXISTS (SELECT 1 FROM accounts WHERE id = ?1 AND deleted_ms IS NULL)",
                [a],
                |r| r.get(0),
            )?;
            if !exists {
                return Err(CoreError::NotFound {
                    what: trf("Konto {0}", &[&a]),
                });
            }
        }
        let n = db.execute(
            "UPDATE booking_documents SET account_id = ?1,
                booking_id = CASE WHEN ?1 IS NOT NULL AND
                    (SELECT account_id FROM transactions WHERE id = booking_id) = ?1
                    THEN booking_id ELSE NULL END
              WHERE id = ?2",
            params![account_id, doc_id],
        )?;
        if n == 0 {
            return Err(CoreError::NotFound {
                what: trf("Dokument {0}", &[&doc_id]),
            });
        }
        Ok(())
    }

    pub fn delete_document(&self, doc_id: i64) -> Result<(), CoreError> {
        self.db()?
            .execute("DELETE FROM booking_documents WHERE id = ?1", [doc_id])?;
        Ok(())
    }

    /// Drop anywhere: reads amount/date/merchant from the file name and the
    /// PDF text, links the file to the best matching booking and returns a
    /// summary ("METRO · −56,13 EUR · 2026-11-07"). Unmatched files are kept
    /// unlinked in the PostBox (returns None).
    pub fn auto_match_dropped_pdf(
        &self,
        file_name: String,
        bytes: Vec<u8>,
    ) -> Result<Option<String>, CoreError> {
        self.auto_match_document(file_name, bytes, None)
    }

    /// Like `auto_match_dropped_pdf`, with extra text extracted by the app
    /// (PDFKit text layer, Vision OCR for photos of receipts).
    pub fn auto_match_document(
        &self,
        file_name: String,
        bytes: Vec<u8>,
        text_hint: Option<String>,
    ) -> Result<Option<String>, CoreError> {
        let mut text = format!(
            "{} {}",
            file_name.replace(['_', '-'], " "),
            text_hint.unwrap_or_default()
        );
        if bytes.starts_with(b"%PDF") {
            text.push(' ');
            text.push_str(&extract_pdf_text(&bytes));
        }
        let bookings = self.bookings_for_matching(&text)?;
        let mime = sniff_mime(&bytes, &file_name);
        let found = best_match(&bookings, &text)
            .map(|(b, _)| (b.id, b.account_id, b.booking_date.clone(), label(b)));
        let db = self.db()?;
        match found {
            Some((id, acc, date, lbl)) => {
                let added = insert_doc(
                    &db,
                    Some(id),
                    Some(acc),
                    &title_from_file(&file_name),
                    "beleg",
                    &mime,
                    &file_name,
                    &bytes,
                    "",
                    &date,
                )?;
                // dropped by hand: the user knows it
                if let Some(added) = added {
                    mark_seen(&db, &[added], true)?;
                }
                Ok(Some(lbl))
            }
            None => {
                let date = dates_in(&text).into_iter().next().unwrap_or_default();
                let added = insert_doc(
                    &db,
                    None,
                    None,
                    &title_from_file(&file_name),
                    "beleg",
                    &mime,
                    &file_name,
                    &bytes,
                    "",
                    &date,
                )?;
                if let Some(added) = added {
                    mark_seen(&db, &[added], true)?;
                }
                Ok(None)
            }
        }
    }

    /// Writes one document. `dest_path` may be a folder or a full file path.
    pub fn export_document(&self, doc_id: i64, dest_path: String) -> Result<String, CoreError> {
        let db = self.db()?;
        let (name, bytes): (String, Vec<u8>) = db
            .query_row(
                "SELECT file_name, pdf_bytes FROM booking_documents WHERE id = ?1",
                [doc_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| CoreError::NotFound {
                what: trf("Beleg {0}", &[&doc_id]),
            })?;
        let p = std::path::Path::new(&dest_path);
        let target = if p.is_dir() {
            p.join(if name.is_empty() {
                format!("beleg-{doc_id}.pdf")
            } else {
                safe_name(&name)
            })
        } else {
            p.to_path_buf()
        };
        crate::fsutil::write_private(&target, bytes).map_err(|e| CoreError::InvalidInput {
            message: trf("Export fehlgeschlagen: {0}", &[&e]),
        })?;
        Ok(target.to_string_lossy().to_string())
    }

    /// Exports every document into `dest_dir/<Jahr>/<Datum>_<Händler>_<Titel>.pdf`
    /// (für die Steuererklärung). Returns the number of files written.
    pub fn export_all_documents(&self, dest_dir: String) -> Result<u32, CoreError> {
        self.export_documents(dest_dir, Vec::new(), tr("GiroCrab-Belege").to_string())
    }

    /// Like [`export_all_documents`](Self::export_all_documents), but only the documents with these
    /// ids (the list the user filtered); an empty list exports everything.
    pub fn export_documents(
        &self,
        dest_dir: String,
        doc_ids: Vec<i64>,
        subfolder: String,
    ) -> Result<u32, CoreError> {
        let wanted: HashSet<i64> = doc_ids.into_iter().collect();
        let docs: Vec<DocumentInfo> = self
            .list_all_documents(DocumentFilter::default())?
            .into_iter()
            .filter(|d| wanted.is_empty() || wanted.contains(&d.id))
            .collect();
        // The year folders go straight into the chosen folder, or into `subfolder` (e.g. the account).
        let subfolder: String = subfolder
            .chars()
            .map(|c| {
                if "/\\:".contains(c) || c.is_control() {
                    '-'
                } else {
                    c
                }
            })
            .collect();
        let subfolder = subfolder.trim().trim_start_matches('.').to_string();
        let base = if subfolder.is_empty() {
            std::path::PathBuf::from(&dest_dir)
        } else {
            std::path::Path::new(&dest_dir).join(subfolder)
        };
        let mut used = HashSet::new();
        let mut n = 0u32;
        for d in docs {
            let year = year_folder(&d.doc_date);
            let dir = base.join(&year);
            std::fs::create_dir_all(&dir).map_err(|e| CoreError::InvalidInput {
                message: format!("{e}"),
            })?;
            let ext = match d.mime_type.as_str() {
                "image/png" => "png",
                "image/jpeg" => "jpg",
                "image/heic" => "heic",
                _ => "pdf",
            };
            let who = d
                .booking_label
                .split(" · ")
                .next()
                .unwrap_or("")
                .to_string();
            let mut file = safe_name(&format!("{}_{}_{}", d.doc_date, who, d.title));
            truncate_chars(&mut file, 120);
            let mut candidate = format!("{file}.{ext}");
            let mut k = 2;
            while !used.insert(dir.join(&candidate)) {
                candidate = format!("{file}_{k}.{ext}");
                k += 1;
            }
            let bytes = self.get_document_bytes(d.id)?;
            crate::fsutil::write_private(dir.join(&candidate), bytes).map_err(|e| {
                CoreError::InvalidInput {
                    message: format!("{e}"),
                }
            })?;
            n += 1;
        }
        Ok(n)
    }
}

/// The year folder a document is exported into. Only a parsed date may name a directory:
/// `doc_date` falls back to the booking date, which comes from bank data ("../x" would leave
/// the export folder).
fn year_folder(doc_date: &str) -> String {
    crate::dates::parse_iso(doc_date).map_or_else(
        || tr("ohne-Datum").to_string(),
        |d| format!("{:04}", d.year()),
    )
}

/// File name of a document in the mirror folder: `<Datum>_<Händler>_<Titel>.<ext>`.
fn mirror_file_name(d: &DocumentInfo) -> String {
    let ext = match d.mime_type.as_str() {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/heic" => "heic",
        _ => "pdf",
    };
    let who = d.booking_label.split(" · ").next().unwrap_or("");
    let stem: Vec<&str> = [d.doc_date.as_str(), who, d.title.as_str()]
        .into_iter()
        .filter(|p| !p.trim().is_empty())
        .collect();
    let mut file = safe_name(&stem.join("_"));
    if file.trim_matches('_').is_empty() {
        file = format!("Dokument_{}", d.id);
    }
    truncate_chars(&mut file, 120);
    format!("{file}.{ext}")
}

#[uniffi::export]
impl Vault {
    /// Copies every document not mirrored yet into
    /// `dest_dir/<Konto>/<Jahr>/<Datum>_<Händler>_<Titel>.pdf` (GitHub #57,
    /// e.g. a folder in iCloud Drive). Existing files are never overwritten.
    /// Returns the number of files written.
    pub fn mirror_new_documents(&self, dest_dir: String) -> Result<u32, CoreError> {
        let base = std::path::Path::new(&dest_dir);
        if dest_dir.trim().is_empty() || !base.is_dir() {
            return Err(CoreError::InvalidInput {
                message: trf("Ordner nicht gefunden: {0}", &[&dest_dir]),
            });
        }
        let pending: HashSet<i64> = {
            let db = self.db()?;
            let mut st =
                db.prepare("SELECT id FROM booking_documents WHERE exported_at IS NULL")?;

            st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?
        };
        if pending.is_empty() {
            return Ok(0);
        }
        let io = |e: std::io::Error| CoreError::InvalidInput {
            message: format!("{e}"),
        };
        let mut written = 0u32;
        for d in self
            .list_all_documents(DocumentFilter::default())?
            .into_iter()
            .filter(|d| pending.contains(&d.id))
        {
            let account = if d.account_name.trim().is_empty() {
                tr("Ohne Konto").to_string()
            } else {
                safe_name(&d.account_name)
            };
            let year = year_folder(&d.doc_date);
            let dir = base.join(account).join(year);
            std::fs::create_dir_all(&dir).map_err(io)?;
            let bytes = self.get_document_bytes(d.id)?;
            let name = mirror_file_name(&d);
            let (stem, ext) = name.rsplit_once('.').unwrap_or((name.as_str(), "pdf"));
            let mut target = dir.join(&name);
            let mut k = 2;
            let mut already = false;
            while target.exists() {
                // same file from an earlier mirror run (e.g. after a folder reset)
                if std::fs::read(&target).map(|b| b == bytes).unwrap_or(false) {
                    already = true;
                    break;
                }
                target = dir.join(format!("{stem}_{k}.{ext}"));
                k += 1;
            }
            if !already {
                crate::fsutil::write_private(&target, &bytes).map_err(io)?;
                written += 1;
            }
            self.db()?.execute(
                "UPDATE booking_documents SET exported_at = strftime('%s','now') WHERE id = ?1",
                [d.id],
            )?;
        }
        Ok(written)
    }

    /// Marks all documents as not mirrored (new mirror folder chosen).
    pub fn reset_document_mirror(&self) -> Result<(), CoreError> {
        self.db()?
            .execute("UPDATE booking_documents SET exported_at = NULL", [])?;
        Ok(())
    }
}

// ------------------------------------------------------------------ demo PDFs

/// Minimal valid one-page PDF (Helvetica) – used for demo receipts.
pub fn simple_pdf(lines: &[String]) -> Vec<u8> {
    fn esc(s: &str) -> String {
        s.chars()
            .map(|c| match c {
                '(' => "\\(".to_string(),
                ')' => "\\)".to_string(),
                '\\' => "\\\\".to_string(),
                '€' => "EUR".to_string(),
                c if (c as u32) < 256 => c.to_string(),
                _ => "?".to_string(),
            })
            .collect()
    }
    let mut content = String::from("BT /F1 11 Tf 56 780 Td 15 TL\n");
    for (i, l) in lines.iter().enumerate() {
        if i == 0 {
            content.push_str(&format!("/F1 16 Tf ({}) Tj T* T* /F1 11 Tf\n", esc(l)));
        } else {
            content.push_str(&format!("({}) Tj T*\n", esc(l)));
        }
    }
    content.push_str("ET\n");
    let latin: Vec<u8> = content
        .chars()
        .map(|c| if (c as u32) < 256 { c as u8 } else { b'?' })
        .collect();
    let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    let objs: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_vec(),
        [format!("<< /Length {} >>\nstream\n", latin.len()).into_bytes(), latin.clone(), b"\nendstream".to_vec()].concat(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_vec(),
    ];
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n", i + 1).into_bytes());
        out.extend(o);
        out.extend(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).into_bytes());
    for o in offsets {
        out.extend(format!("{o:010} 00000 n \n").into_bytes());
    }
    out.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objs.len() + 1
        )
        .into_bytes(),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::query_plan;

    #[test]
    fn file_names_cut_on_char_boundaries_and_stay_in_folder() {
        // 119 ASCII + "ü" (2 bytes): byte 120 is inside the umlaut
        let mut s = format!("{}ü{}", "a".repeat(119), "b".repeat(10));
        truncate_chars(&mut s, 120);
        assert_eq!(s.chars().count(), 120);
        assert!(s.ends_with('ü'));
        let mut short = "Müller".to_string();
        truncate_chars(&mut short, 120);
        assert_eq!(short, "Müller");
        assert_eq!(safe_name(".."), "_");
        assert_eq!(safe_name("."), "_");
        assert_eq!(safe_name("../x"), "_x");
        assert_eq!(safe_name(".hidden"), "hidden");
        assert_eq!(safe_name("Beleg 1.pdf"), "Beleg_1.pdf");
    }

    /// The document list must come from the covering index: a table scan
    /// reads through every stored PDF to reach the columns behind the blob.
    #[test]
    fn document_list_is_answered_from_the_index() {
        let path_dir = crate::test_support::TempDir::new("gc-docs-plan");
        let path = path_dir.join("v.db");
        let v = Vault::open(path.to_string_lossy().into(), vec![5u8; 32]).unwrap();
        let acc = {
            let db = v.db().unwrap();
            db.execute("INSERT INTO account_groups(name) VALUES ('G')", [])
                .unwrap();
            db.execute(
                "INSERT INTO accounts(group_id, external_ref, name) VALUES ((SELECT id FROM account_groups), 'A', 'Giro')",
                [],
            )
            .unwrap();
            db.last_insert_rowid()
        };
        for (i, date) in ["2026-01-05", "2026-03-01", "2026-02-10"]
            .iter()
            .enumerate()
        {
            let id = v
                .attach_document_to_account(
                    acc,
                    format!("Auszug {i}"),
                    format!("auszug_{i}.pdf"),
                    simple_pdf(&[format!("Kontoauszug {i}")]),
                    "kontoauszug".into(),
                )
                .unwrap();
            v.db()
                .unwrap()
                .execute(
                    "UPDATE booking_documents SET doc_date = ?2 WHERE id = ?1",
                    params![id, date],
                )
                .unwrap();
        }
        {
            let db = v.db().unwrap();
            let p = query_plan(
                &db,
                &list_all_documents_sql(),
                params![None::<i64>, None::<String>, None::<String>, 0, None::<i64>],
            );
            assert!(p.contains("COVERING INDEX booking_documents_info"), "{p}");
        }
        // same result as before: newest first, with size and account
        let docs = v.list_all_documents(DocumentFilter::default()).unwrap();
        let titles: Vec<&str> = docs.iter().map(|d| d.title.as_str()).collect();
        assert_eq!(titles, ["Auszug 1", "Auszug 2", "Auszug 0"]);
        assert!(docs.iter().all(|d| d.account_id == Some(acc)));
        let one = v
            .list_all_documents(DocumentFilter {
                account_id: Some(acc + 1),
                ..Default::default()
            })
            .unwrap();
        assert!(one.is_empty());
        let _ = std::fs::remove_file(path);
    }

    /// Bank connection filter (#1042): goes through the document's account, needs no
    /// `booking_documents` column, and documents without an account never match.
    #[test]
    fn filters_documents_by_bank_connection() {
        let dir = crate::test_support::TempDir::new("gc-docs-conn");
        let path = dir.join("v.db");
        let v = Vault::open(path.to_string_lossy().into(), vec![5u8; 32]).unwrap();
        let ((a, conn_a), (b, conn_b)) = {
            let db = v.db().unwrap();
            db.execute("INSERT INTO account_groups(name) VALUES ('G')", [])
                .unwrap();
            let mut ids = vec![];
            for r in ["A", "B"] {
                db.execute(
                    "INSERT INTO bank_connections(connector, display_name) VALUES ('demo', ?1)",
                    [r],
                )
                .unwrap();
                let conn = db.last_insert_rowid();
                db.execute(
                    "INSERT INTO accounts(group_id, external_ref, name, connection_id)
                     VALUES ((SELECT id FROM account_groups), ?1, ?1, ?2)",
                    params![r, conn],
                )
                .unwrap();
                ids.push((db.last_insert_rowid(), conn));
            }
            (ids[0], ids[1])
        };
        for (acc, title) in [(a, "Von A"), (b, "Von B")] {
            v.attach_document_to_account(
                acc,
                title.into(),
                format!("{title}.pdf"),
                simple_pdf(&[title.to_string()]),
                "kontoauszug".into(),
            )
            .unwrap();
        }
        let by = |connection_id, account_id| {
            v.list_all_documents(DocumentFilter {
                connection_id,
                account_id,
                ..Default::default()
            })
            .unwrap()
            .into_iter()
            .map(|d| d.title)
            .collect::<Vec<_>>()
        };
        assert_eq!(by(Some(conn_a), None), ["Von A"]);
        assert_eq!(by(Some(conn_b), None), ["Von B"]);
        assert!(by(Some(conn_b + 100), None).is_empty());
        assert_eq!(by(None, None).len(), 2);
        // both filters combine (AND)
        assert_eq!(by(Some(conn_a), Some(a)), ["Von A"]);
        assert!(by(Some(conn_a), Some(b)).is_empty());
        {
            let db = v.db().unwrap();
            let p = query_plan(
                &db,
                &list_all_documents_sql(),
                params![None::<i64>, None::<String>, None::<String>, 0, Some(conn_a)],
            );
            assert!(p.contains("COVERING INDEX booking_documents_info"), "{p}");
        }
    }

    #[test]
    fn reads_amounts_and_dates() {
        assert_eq!(amounts_in("Summe EUR 56,13 bar 60,00"), vec![56_13, 60_00]);
        assert_eq!(amounts_in("Total 1.234,56 €"), vec![1_234_56]);
        assert!(dates_in("Datum 07.11.2026 / 2026-11-08").contains(&"2026-11-07".to_string()));
        assert!(dates_in("Rechnung_2026-11-08_Metro.pdf").contains(&"2026-11-08".to_string()));
        assert_eq!(dates_in("vom 31.12.99"), vec!["1999-12-31".to_string()]);
        assert_eq!(dates_in("vom 02.01.26"), vec!["2026-01-02".to_string()]);
    }

    #[test]
    fn a_document_date_that_is_no_day_is_stored_empty() {
        let dir = crate::test_support::TempDir::new("girocrab-docs-date");
        let v = Vault::open(
            dir.join("v.db").to_string_lossy().to_string(),
            vec![8u8; 32],
        )
        .unwrap();
        let db = v.db().unwrap();
        let stored = |date: &str| {
            let id = insert_doc(
                &db,
                None,
                None,
                "t",
                "beleg",
                "application/pdf",
                "x.pdf",
                b"%PDF-1.4 x",
                date,
                date,
            )
            .unwrap()
            .unwrap();
            db.query_row(
                "SELECT doc_date FROM booking_documents WHERE id = ?1",
                [id],
                |r| r.get::<_, String>(0),
            )
            .unwrap()
        };
        assert_eq!(stored("2025-03-02"), "2025-03-02");
        assert_eq!(stored("2025-03-04T10:00:00Z"), "2025-03-04");
        assert_eq!(stored("2025-02-30"), "");
        assert_eq!(stored("../x"), "");
    }

    #[test]
    fn export_folders_are_years_only() {
        assert_eq!(year_folder("2025-03-02"), "2025");
        assert_eq!(year_folder("../x"), tr("ohne-Datum"));
        assert_eq!(year_folder("2025-02-30"), tr("ohne-Datum"));
        assert_eq!(year_folder(""), tr("ohne-Datum"));
    }

    #[test]
    fn adjacent_amounts_and_dates_are_all_found() {
        assert_eq!(amounts_in("12,50 13,00"), vec![12_50, 13_00]);
        assert_eq!(amounts_in("12,50;13,00"), vec![12_50, 13_00]);
        assert_eq!(
            dates_in("2025-01-02_2025-01-31"),
            vec!["2025-01-02".to_string(), "2025-01-31".to_string()]
        );
        assert!(dates_in("am 31.02.2025 fällig").is_empty());
        assert!(dates_in("Rechnung_2025-02-30.pdf").is_empty());
    }

    fn demo_vault() -> (crate::test_support::TempDir, std::sync::Arc<Vault>) {
        let dir = crate::test_support::TempDir::new("girocrab-docs-match");
        let path = dir.join("v.db").to_string_lossy().to_string();
        let v = Vault::open(path, vec![6u8; 32]).unwrap();
        v.seed_demo_data().unwrap();
        (dir, v)
    }

    #[test]
    fn prefiltered_bookings_match_like_all_bookings() {
        let (_dir, v) = demo_vault();
        let all = v
            .list_transactions(crate::models::BookingFilter::default())
            .unwrap()
            .transactions;
        let de = |iso: &str, shift: i64| {
            let d = jiff::civil::Date::strptime("%Y-%m-%d", iso).unwrap();
            (d + crate::dates::days(shift))
                .strftime("%d.%m.%Y")
                .to_string()
        };
        let mut name_date_hits = 0;
        for b in &all {
            let amount = format_cents(b.amount_cents.abs(), "EUR".into());
            for text in [
                format!("Summe {amount}"),
                format!("{} Datum {}", b.merchant.name, de(&b.booking_date, 2)),
                format!("{} Datum {}", b.merchant.name, de(&b.booking_date, 4)),
                format!("Summe {amount} Datum {}", de(&b.booking_date, -60)),
            ] {
                let expected = best_match(&all, &text).map(|(m, s)| (m.id, s));
                let candidates = v.bookings_for_matching(&text).unwrap();
                let found = best_match(&candidates, &text).map(|(m, s)| (m.id, s));
                assert_eq!(found, expected, "{text}");
                if text.starts_with(&b.merchant.name) && found.is_some() {
                    name_date_hits += 1;
                }
            }
        }
        assert!(name_date_hits > 0, "a match by name and date alone");
    }

    #[test]
    fn a_date_at_the_end_of_the_calendar_does_not_panic() {
        // jiff dates stop at 9999-12-31, so the days around it must be skipped.
        let text = "Freistellungsauftrag gültig bis 31.12.9999";
        assert!(dates_in(text).contains(&"9999-12-31".to_string()));
        let (_dir, v) = demo_vault();
        v.bookings_for_matching(text).unwrap();
    }

    #[test]
    fn thousands_of_dates_keep_the_prefilter_sql_valid() {
        let (_dir, v) = demo_vault();
        let start = jiff::civil::date(2000, 1, 1);
        let text: String = (0..3000)
            .map(|k| {
                format!(
                    "{} ",
                    (start + crate::dates::days(k * 2)).strftime("%d.%m.%Y")
                )
            })
            .collect();
        assert!(v.bookings_for_matching(&text).is_ok());
    }

    #[test]
    fn simple_pdf_text_roundtrip() {
        let pdf = simple_pdf(&[
            "Kassenbon METRO".into(),
            "Datum: 08.11.2026".into(),
            "Summe: 56,13 EUR".into(),
        ]);
        assert!(pdf.starts_with(b"%PDF"));
        let t = extract_pdf_text(&pdf);
        assert!(t.contains("METRO") && t.contains("56,13"), "{t}");
    }

    #[test]
    fn unlinking_a_document_clears_its_booking() {
        let p_dir = crate::test_support::TempDir::new("girocrab-docs-unlink");
        let p = p_dir.join("v.db");
        let v = Vault::open(p.to_string_lossy().to_string(), vec![6u8; 32]).unwrap();
        v.seed_demo_data().unwrap();
        let yesterday = (crate::dates::today() - crate::dates::days(1))
            .strftime("%d.%m.%Y")
            .to_string();
        let pdf = simple_pdf(&[
            "REWE Markt GmbH".into(),
            format!("Datum {yesterday}"),
            "Summe 47,82 EUR".into(),
        ]);
        v.auto_match_dropped_pdf("scan.pdf".into(), pdf).unwrap();
        let doc = v
            .list_all_documents(DocumentFilter::default())
            .unwrap()
            .into_iter()
            .find(|d| d.booking_id.is_some())
            .unwrap();
        v.link_document(doc.id, None).unwrap();
        let after = v
            .list_all_documents(DocumentFilter::default())
            .unwrap()
            .into_iter()
            .find(|d| d.id == doc.id)
            .unwrap();
        assert_eq!(after.booking_id, None);
        assert!(after.booking_label.is_empty());
    }

    #[test]
    fn documents_are_unread_until_opened_and_old_ones_arrive_read() {
        let dir = crate::test_support::TempDir::new("girocrab-docs-seen");
        let v = Vault::open(
            dir.join("v.db").to_string_lossy().to_string(),
            vec![8u8; 32],
        )
        .unwrap();
        let (acc, recent, old, undated) = {
            let db = v.db().unwrap();
            db.execute("INSERT INTO account_groups(name) VALUES ('G')", [])
                .unwrap();
            let g = db.last_insert_rowid();
            db.execute(
                "INSERT INTO accounts(group_id, external_ref, name) VALUES (?1, 'x', 'Depot')",
                [g],
            )
            .unwrap();
            let acc = db.last_insert_rowid();
            let day = |ago: i64| crate::dates::iso(crate::dates::today() - crate::dates::days(ago));
            let add = |title: &str, url: &str, date: &str| {
                insert_doc(
                    &db,
                    None,
                    Some(acc),
                    title,
                    "postbox",
                    "application/pdf",
                    "x.pdf",
                    b"%PDF-1.4 x",
                    url,
                    date,
                )
                .unwrap()
                .unwrap()
            };
            (
                acc,
                add("Neu", "u1", &day(3)),
                add("Alt", "u2", &day(200)),
                add("Ohne Datum", "u3", ""),
            )
        };
        let seen = |id: i64| {
            v.list_all_documents(DocumentFilter::default())
                .unwrap()
                .into_iter()
                .find(|d| d.id == id)
                .unwrap()
                .seen
        };
        // a recent document is unread, an old one arrives read, an unreadable date counts as new
        assert!(!seen(recent) && seen(old) && !seen(undated));
        assert_eq!(v.unseen_document_count().unwrap(), 2);
        // read and unread again
        assert_eq!(v.mark_documents_seen(vec![recent], true).unwrap(), 1);
        assert_eq!(v.mark_documents_seen(vec![recent], true).unwrap(), 0);
        assert!(seen(recent));
        assert_eq!(v.mark_documents_seen(vec![recent, old], false).unwrap(), 2);
        assert_eq!(v.unseen_document_count().unwrap(), 3);
        // all read, here for another account nothing changes
        assert_eq!(v.mark_all_documents_seen(Some(acc + 99)).unwrap(), 0);
        assert_eq!(v.mark_all_documents_seen(Some(acc)).unwrap(), 3);
        assert_eq!(v.unseen_document_count().unwrap(), 0);
        // added by hand: read at once
        let by_hand = v
            .attach_document_to_account(
                acc,
                "Von Hand".into(),
                "a.pdf".into(),
                b"%PDF-1.4 y".to_vec(),
                "beleg".into(),
            )
            .unwrap();
        assert!(seen(by_hand));
        assert_eq!(v.unseen_document_count().unwrap(), 0);
    }

    #[test]
    fn the_unread_count_matches_what_the_list_shows_for_odd_rows() {
        let dir = crate::test_support::TempDir::new("girocrab-docs-odd");
        let v = Vault::open(
            dir.join("v.db").to_string_lossy().to_string(),
            vec![9u8; 32],
        )
        .unwrap();
        {
            let db = v.db().unwrap();
            db.execute("INSERT INTO account_groups(name) VALUES ('G')", [])
                .unwrap();
            let g = db.last_insert_rowid();
            db.execute(
                "INSERT INTO accounts(group_id, external_ref, name) VALUES (?1, 'x', 'Konto')",
                [g],
            )
            .unwrap();
            let acc = db.last_insert_rowid();
            db.execute("INSERT INTO accounts(group_id, external_ref, name, deleted_ms) VALUES (?1, 'y', 'Weg', 1)", [g]).unwrap();
            let gone = db.last_insert_rowid();
            db.execute(
                "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, deleted_ms) VALUES (?1, 't', '2026-11-01', 100, 1)",
                [acc],
            )
            .unwrap();
            let deleted_booking = db.last_insert_rowid();
            let today = crate::dates::today_iso();
            let add = |booking: Option<i64>, account: Option<i64>, url: &str| {
                insert_doc(
                    &db,
                    booking,
                    account,
                    "T",
                    "postbox",
                    "application/pdf",
                    "x.pdf",
                    b"%PDF-1.4 x",
                    url,
                    &today,
                )
                .unwrap()
                .unwrap()
            };
            add(Some(deleted_booking), Some(acc), "a");
            add(None, Some(gone), "b");
            add(None, None, "c");
            add(None, Some(acc), "d");
        }
        let listed = v
            .list_all_documents(DocumentFilter::default())
            .unwrap()
            .into_iter()
            .filter(|d| !d.seen)
            .count();
        assert_eq!(listed as u32, v.unseen_document_count().unwrap());
        assert_eq!(listed, 4);
    }

    #[test]
    fn drop_matches_metro_receipt_and_stores_encrypted() {
        let p_dir = crate::test_support::TempDir::new("girocrab-docs");
        let p = p_dir.join("v.db");
        let v = Vault::open(p.to_string_lossy().to_string(), vec![6u8; 32]).unwrap();
        v.seed_demo_data().unwrap();
        let yesterday = (crate::dates::today() - crate::dates::days(1))
            .strftime("%d.%m.%Y")
            .to_string();
        let pdf = simple_pdf(&[
            "REWE Markt GmbH".into(),
            format!("Datum {yesterday}"),
            "Summe 47,82 EUR".into(),
        ]);
        let hit = v.auto_match_dropped_pdf("scan.pdf".into(), pdf).unwrap();
        assert!(hit.as_deref().unwrap_or("").starts_with("REWE"), "{hit:?}");
        let docs = v.list_all_documents(DocumentFilter::default()).unwrap();
        assert!(docs.iter().any(|d| d.booking_label.starts_with("REWE")));
        // unmatched -> PostBox
        assert!(
            v.auto_match_dropped_pdf("irgendwas.pdf".into(), simple_pdf(&["Hallo".into()]))
                .unwrap()
                .is_none()
        );
        assert!(
            !v.list_all_documents(DocumentFilter {
                unlinked_only: true,
                ..Default::default()
            })
            .unwrap()
            .is_empty()
        );
        // export
        let export = crate::test_support::TempDir::new("export");
        let dir = export.path();
        assert!(
            v.export_all_documents(dir.to_string_lossy().to_string())
                .unwrap()
                >= 2
        );
        // only the listed documents (the list the user filtered)
        let one = docs.iter().find(|d| d.booking_id.is_some()).unwrap().id;
        let part = crate::test_support::TempDir::new("export-part");
        assert_eq!(
            v.export_documents(
                part.path().to_string_lossy().to_string(),
                vec![one],
                String::new(),
            )
            .unwrap(),
            1
        );
        // without a subfolder the year folders lie directly in the chosen folder
        let written: usize = std::fs::read_dir(part.path())
            .unwrap()
            .flatten()
            .inspect(|y| assert!(y.path().is_dir() && y.file_name() != "_"))
            .flat_map(|y| std::fs::read_dir(y.path()).unwrap().flatten())
            .inspect(|f| assert!(f.path().is_file()))
            .count();
        assert_eq!(written, 1);
        // with one (the account) they lie inside it
        let named = crate::test_support::TempDir::new("export-named");
        v.export_documents(
            named.path().to_string_lossy().to_string(),
            vec![one],
            "TR Verrechnungskonto".into(),
        )
        .unwrap();
        assert!(named.path().join("TR Verrechnungskonto").is_dir());
        let _ = std::fs::remove_file(p);
    }
}

#[cfg(test)]
mod pdf_limit_tests {
    use super::*;

    fn zlib(data: &[u8]) -> Vec<u8> {
        miniz_oxide::deflate::compress_to_vec_zlib(data, 6)
    }

    fn pdf_with(streams: &[Vec<u8>]) -> Vec<u8> {
        let mut pdf = b"%PDF-1.4\n".to_vec();
        for s in streams {
            push_stream(&mut pdf, "/Filter /FlateDecode", s);
        }
        pdf
    }

    fn push_stream(pdf: &mut Vec<u8>, dict: &str, data: &[u8]) {
        pdf.extend_from_slice(format!("<< {dict} >>\nstream\n").as_bytes());
        pdf.extend_from_slice(data);
        pdf.extend_from_slice(b"\nendstream\n");
    }

    #[test]
    fn small_stream_is_extracted() {
        let pdf = pdf_with(&[zlib(b"BT (Hello) Tj ET")]);
        assert!(extract_pdf_text(&pdf).contains("Hello"));
    }

    #[test]
    fn oversized_stream_is_skipped() {
        let mut big = b"BT (Bomb) Tj ET".to_vec();
        big.resize(MAX_INFLATED_STREAM + 1024, b' ');
        let pdf = pdf_with(&[zlib(&big)]);
        assert!(!extract_pdf_text(&pdf).contains("Bomb"));
    }

    #[test]
    fn total_inflated_size_is_capped() {
        let mut big = b"BT (Chunk) Tj ET".to_vec();
        big.resize(MAX_INFLATED_STREAM - 1024, b' ');
        let n = MAX_INFLATED_TOTAL / big.len() + 2;
        let pdf = pdf_with(&vec![zlib(&big); n]);
        let extracted = extract_pdf_text(&pdf).matches("Chunk").count();
        assert_eq!(extracted, MAX_INFLATED_TOTAL / big.len());
    }

    #[test]
    fn image_streams_leave_the_budget_to_text() {
        let mut image = b"BT (Pixel) Tj ET".to_vec();
        image.resize(MAX_INFLATED_STREAM - 1024, b' ');
        let image = zlib(&image);
        let mut pdf = b"%PDF-1.4\n".to_vec();
        for _ in 0..MAX_INFLATED_TOTAL / MAX_INFLATED_STREAM + 1 {
            push_stream(&mut pdf, "/Subtype /Image /Filter /FlateDecode", &image);
        }
        push_stream(&mut pdf, "/Filter /FlateDecode", &zlib(b"BT (Text) Tj ET"));
        let text = extract_pdf_text(&pdf);
        assert!(text.contains("Text"));
        assert!(!text.contains("Pixel"));
    }

    #[test]
    fn stream_count_is_capped() {
        let one = zlib(b"BT (Tick) Tj ET");
        let pdf = pdf_with(&vec![one; MAX_PDF_STREAMS + 50]);
        let n = extract_pdf_text(&pdf).matches("Tick").count();
        assert_eq!(n, MAX_PDF_STREAMS);
    }
}

#[cfg(test)]
mod mirror_tests {
    use super::*;

    #[test]
    fn mirrors_each_document_once_into_account_and_year_folders() {
        let root = std::env::temp_dir().join(format!("gc-mirror-{}", std::process::id()));
        let out = root.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let v = Vault::open(root.join("v.db").to_string_lossy().into(), vec![9u8; 32]).unwrap();
        let pdf = simple_pdf(&["Kontoauszug".into()]);
        {
            let c = v.db().unwrap();
            c.execute("INSERT INTO account_groups(name) VALUES ('G')", [])
                .unwrap();
            c.execute(
                "INSERT INTO accounts(group_id, external_ref, name) VALUES ((SELECT id FROM account_groups), 'A', 'Giro/Konto')",
                [],
            )
            .unwrap();
            let acc = c.last_insert_rowid();
            for (src, date) in [("s1", "2026-09-30"), ("s2", "2026-10-31")] {
                insert_doc(
                    &c,
                    None,
                    Some(acc),
                    "Kontoauszug",
                    "kontoauszug",
                    "application/pdf",
                    "k.pdf",
                    &pdf,
                    src,
                    date,
                )
                .unwrap();
            }
        }
        let dest = out.to_string_lossy().to_string();
        assert!(v.mirror_new_documents("/nonexistent/gc".into()).is_err());
        assert_eq!(v.mirror_new_documents(dest.clone()).unwrap(), 2);
        let year = out.join("Giro_Konto").join("2026");
        assert!(year.join("2026-09-30_Kontoauszug.pdf").is_file());
        assert!(year.join("2026-10-31_Kontoauszug.pdf").is_file());
        // nothing new -> nothing written; after a reset identical files are kept, not duplicated
        assert_eq!(v.mirror_new_documents(dest.clone()).unwrap(), 0);
        v.reset_document_mirror().unwrap();
        assert_eq!(v.mirror_new_documents(dest).unwrap(), 0);
        assert_eq!(std::fs::read_dir(&year).unwrap().count(), 2);
        drop(v);
        let _ = std::fs::remove_dir_all(root);
    }
}
