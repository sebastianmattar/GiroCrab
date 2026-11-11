//! Beleg-Abruf & PostBox: encrypted document storage inside the vault.
//!
//! All bank statements, ETF trade confirmations and receipts live as BLOBs in
//! the SQLCipher table `booking_documents`. Documents arrive from the bank
//! connectors (Trade Republic timelineDetailV2, comdirect PostBox, FinTS
//! HKEKA) or by drag & drop; dropped files are matched to a booking by
//! amount, date and merchant name.

use std::collections::HashSet;

use regex::Regex;
use rusqlite::{params, Connection, OptionalExtension};

use crate::error::{CoreError, CoreResult};
use crate::i18n::{tr, trf};
use crate::models::{Booking, BookingFilter};
use crate::money::format_cents;
use crate::vault::Vault;

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
"#;

#[derive(Debug, Clone, uniffi::Record)]
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
}

#[derive(Debug, Clone, Default, uniffi::Record)]
pub struct DocumentFilter {
    pub account_id: Option<i64>,
    pub doc_type: Option<String>,
    pub search: Option<String>,
    /// Only documents not linked to a booking (PostBox / unmatched).
    pub unlinked_only: bool,
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
    let n = conn.execute(
        "INSERT OR IGNORE INTO booking_documents(booking_id, account_id, title, doc_type, mime_type, file_name, pdf_bytes, source_url, doc_date)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![booking_id, account_id, title, doc_type, mime, file_name, bytes, source_url, date],
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

/// Zip-bomb guards for [`extract_pdf_text`].
const MAX_INFLATED_STREAM: usize = 8 * 1024 * 1024;
const MAX_PDF_STREAMS: usize = 256;
const MAX_PDF_TEXT: usize = 4 * 1024 * 1024;

/// Best-effort text of a PDF: inflates FlateDecode streams and collects the
/// literal strings of text operators. Good enough for receipts and bank PDFs;
/// the app passes PDFKit/Vision text as an additional hint.
pub fn extract_pdf_text(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut streams = 0usize;
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
        if out.len() >= MAX_PDF_TEXT || streams >= MAX_PDF_STREAMS {
            break;
        }
        streams += 1;
        if find(dict, b"FlateDecode", 0).is_some() {
            if let Ok(inflated) =
                miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(data, MAX_INFLATED_STREAM)
            {
                literal_strings(&inflated, &mut out);
            }
        } else if find(dict, b"Image", 0).is_none() {
            literal_strings(data, &mut out);
        }
        i = end + 9;
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
    let re =
        Regex::new(r"(?:^|[^\d,.])(\d{1,3}(?:\.\d{3})+,\d{2}|\d+,\d{2}|\d+\.\d{2})(?:[^\d]|$)")
            .unwrap();
    let mut out: Vec<i64> = Vec::new();
    for c in re.captures_iter(text) {
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
    let mut out = Vec::new();
    let de = Regex::new(r"\b(\d{1,2})\.(\d{1,2})\.(\d{4}|\d{2})\b").unwrap();
    for c in de.captures_iter(text) {
        let (d, m): (u32, u32) = (c[1].parse().unwrap_or(0), c[2].parse().unwrap_or(0));
        let y: i32 = if c[3].len() == 2 {
            2000 + c[3].parse::<i32>().unwrap_or(0)
        } else {
            c[3].parse().unwrap_or(0)
        };
        if (1..=31).contains(&d) && (1..=12).contains(&m) {
            out.push(format!("{y:04}-{m:02}-{d:02}"));
        }
    }
    let iso = Regex::new(r"(?:^|\D)(20\d{2})[-_]?(\d{2})[-_]?(\d{2})(?:\D|$)").unwrap();
    for c in iso.captures_iter(text) {
        let s = format!("{}-{}-{}", &c[1], &c[2], &c[3]);
        if chrono::NaiveDate::parse_from_str(&s, "%Y-%m-%d").is_ok() && !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

fn days_between(a: &str, b: &str) -> Option<i64> {
    let a = chrono::NaiveDate::parse_from_str(a, "%Y-%m-%d").ok()?;
    let b = chrono::NaiveDate::parse_from_str(b, "%Y-%m-%d").ok()?;
    Some((a - b).num_days().abs())
}

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
                0..=3 => 3,
                4..=10 => 2,
                11..=45 => 0,
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
    })
}

const SELECT_INFO: &str = "SELECT d.id, d.booking_id, COALESCE(d.account_id, t.account_id), d.title, d.doc_type, d.mime_type, d.file_name,
        length(d.pdf_bytes), d.source_url, COALESCE(NULLIF(d.doc_date,''), t.booking_date, ''), d.created_at, a.name,
        t.counterparty, t.amount_cents, t.booking_date
   FROM booking_documents d
   LEFT JOIN transactions t ON t.id = d.booking_id
   LEFT JOIN accounts a ON a.id = COALESCE(d.account_id, t.account_id)";

fn safe_name(s: &str) -> String {
    s.chars()
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
        .join("_")
}

impl Vault {
    fn bookings_for_matching(&self) -> CoreResult<Vec<Booking>> {
        Ok(self
            .list_transactions(BookingFilter::default())?
            .transactions)
    }
}

#[uniffi::export]
impl Vault {
    pub fn list_documents_for_booking(
        &self,
        booking_id: i64,
    ) -> Result<Vec<DocumentInfo>, CoreError> {
        let db = self.db()?;
        let mut st = db.prepare(&format!(
            "{SELECT_INFO} WHERE d.booking_id = ?1 ORDER BY d.doc_date DESC, d.id DESC"
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
        let mut st = db.prepare(&format!(
            "{SELECT_INFO} WHERE (?1 IS NULL OR COALESCE(d.account_id, t.account_id) = ?1)
                AND (?2 IS NULL OR d.doc_type = ?2)
                AND (?3 IS NULL OR d.title LIKE ?3 OR d.file_name LIKE ?3 OR t.counterparty LIKE ?3)
                AND (?4 = 0 OR d.booking_id IS NULL)
             ORDER BY COALESCE(NULLIF(d.doc_date,''), t.booking_date) DESC, d.id DESC"
        ))?;
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
                    filter.unlinked_only as i64
                ],
                row_to_info,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
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
                "SELECT account_id, booking_date FROM transactions WHERE id = ?1",
                [booking_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| CoreError::NotFound {
                what: trf("Buchung {0}", &[&booking_id]),
            })?;
        let mime = sniff_mime(&bytes, &file_name);
        insert_doc(
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
        })
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
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        insert_doc(
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
        })
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
        let bookings = self.bookings_for_matching()?;
        let mime = sniff_mime(&bytes, &file_name);
        let found = best_match(&bookings, &text)
            .map(|(b, _)| (b.id, b.account_id, b.booking_date.clone(), label(b)));
        let db = self.db()?;
        match found {
            Some((id, acc, date, lbl)) => {
                insert_doc(
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
                Ok(Some(lbl))
            }
            None => {
                let date = dates_in(&text).into_iter().next().unwrap_or_default();
                insert_doc(
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
        std::fs::write(&target, bytes).map_err(|e| CoreError::InvalidInput {
            message: trf("Export fehlgeschlagen: {0}", &[&e]),
        })?;
        Ok(target.to_string_lossy().to_string())
    }

    /// Exports every document into `dest_dir/<Jahr>/<Datum>_<Händler>_<Titel>.pdf`
    /// (für die Steuererklärung). Returns the number of files written.
    pub fn export_all_documents(&self, dest_dir: String) -> Result<u32, CoreError> {
        let docs = self.list_all_documents(DocumentFilter::default())?;
        let base = std::path::Path::new(&dest_dir).join(tr("GiroCrab-Belege"));
        let mut used = HashSet::new();
        let mut n = 0u32;
        for d in docs {
            let year = d.doc_date.get(..4).unwrap_or(tr("ohne-Datum")).to_string();
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
            if file.len() > 120 {
                file.truncate(120);
            }
            let mut candidate = format!("{file}.{ext}");
            let mut k = 2;
            while !used.insert(dir.join(&candidate)) {
                candidate = format!("{file}_{k}.{ext}");
                k += 1;
            }
            let bytes = self.get_document_bytes(d.id)?;
            std::fs::write(dir.join(&candidate), bytes).map_err(|e| CoreError::InvalidInput {
                message: format!("{e}"),
            })?;
            n += 1;
        }
        Ok(n)
    }
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
    if file.is_empty() {
        file = format!("Dokument_{}", d.id);
    }
    file.truncate(120);
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
            let ids = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
            ids
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
            let year = d.doc_date.get(..4).unwrap_or(tr("ohne-Datum")).to_string();
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
                std::fs::write(&target, &bytes).map_err(io)?;
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

    #[test]
    fn reads_amounts_and_dates() {
        assert_eq!(amounts_in("Summe EUR 56,13 bar 60,00"), vec![56_13, 60_00]);
        assert_eq!(amounts_in("Total 1.234,56 €"), vec![1_234_56]);
        assert!(dates_in("Datum 07.11.2026 / 2026-11-08").contains(&"2026-11-07".to_string()));
        assert!(dates_in("Rechnung_2026-11-08_Metro.pdf").contains(&"2026-11-08".to_string()));
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
    fn drop_matches_metro_receipt_and_stores_encrypted() {
        let p = std::env::temp_dir().join(format!("girocrab-docs-{}.db", uuid::Uuid::new_v4()));
        let v = Vault::open(p.to_string_lossy().to_string(), vec![6u8; 32]).unwrap();
        v.seed_demo_data().unwrap();
        let yesterday = (chrono::Local::now().date_naive() - chrono::Duration::days(1))
            .format("%d.%m.%Y")
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
        assert!(v
            .auto_match_dropped_pdf("irgendwas.pdf".into(), simple_pdf(&["Hallo".into()]))
            .unwrap()
            .is_none());
        assert!(!v
            .list_all_documents(DocumentFilter {
                unlinked_only: true,
                ..Default::default()
            })
            .unwrap()
            .is_empty());
        // export
        let dir = std::env::temp_dir().join(format!("girocrab-export-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(
            v.export_all_documents(dir.to_string_lossy().to_string())
                .unwrap()
                >= 2
        );
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
            pdf.extend_from_slice(b"<< /Filter /FlateDecode >>\nstream\n");
            pdf.extend_from_slice(s);
            pdf.extend_from_slice(b"\nendstream\n");
        }
        pdf
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
