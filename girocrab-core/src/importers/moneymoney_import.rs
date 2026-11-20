//! Import of MoneyMoney CSV / XML exports and C24 CSV exports.
//!
//! * Parses German and English CSV headers (`;` `,` or tab) and the XML
//!   `<transactions><transaction>…` export.
//! * Reads C24 category / subcategory columns as literal tree components.
//! * Keeps hierarchical categories (`Haushalt\Lebensmittel`, `Steuern / Büro`)
//!   as a category tree and the user's notes in `transactions.note`.
//! * Idempotent: every row gets a deterministic fingerprint
//!   (`booking_date + amount + counterparty IBAN/name + purpose`). Re-importing a
//!   file, or importing a range that overlaps bookings already fetched via
//!   FinTS, never duplicates – existing rows are enriched instead. A later
//!   FinTS sync adopts imported rows (see [`adopt_import`]).

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use rusqlite::{Connection, OptionalExtension, params};

use super::portal_import::{parse_date, split_csv};
use crate::dates::now_ms;
use crate::error::{CoreError, CoreResult};
use crate::fnv::fnv1a64;
use crate::i18n::{tr, trf};
use crate::iban::{normalize_iban, validate_iban};
use crate::merchants::{FlowKind, MerchantResolver};
use crate::models::*;
use crate::money::parse_cents;
use crate::text::parse_bool_loose;
use crate::vault::{Vault, enrich};

const ARCHIVE_GROUP: &str = "Importierte Konten";

/// One normalised MoneyMoney booking.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MmRow {
    pub booking_date: String,
    pub value_date: String,
    pub amount_cents: i64,
    pub currency: String,
    pub name: String,
    pub iban: String,
    pub bic: String,
    pub purpose: String,
    pub booking_text: String,
    /// Raw category as exported (`A\B`, `A / B`).
    pub category: String,
    /// Exact category path when known (database import); takes precedence over
    /// splitting `category`, so names containing " / " stay intact.
    pub category_parts: Vec<String>,
    pub note: String,
    pub checkmark: bool,
    pub mandate_ref: String,
    pub creditor_id: String,
    pub end_to_end: String,
    /// Parts of a split booking (database import, #877): the row is stored as a
    /// GiroCrab split booking. Empty = no split.
    pub splits: Vec<MmSplit>,
    /// The rows the parts were imported as before #877. Taken instead of this
    /// row when one of them is already in the account (earlier import), so the
    /// whole booking is never added next to its parts.
    pub parts: Vec<MmRow>,
}

/// One part of a split booking.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MmSplit {
    pub amount_cents: i64,
    /// Category path (empty = uncategorised).
    pub category_parts: Vec<String>,
    /// The part's own note.
    pub note: String,
    /// The part's name / purpose where it differs from the booking's.
    pub text: String,
}

// ------------------------------------------------------------ fingerprinting

fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// [`norm`] without accents, for comparing names and purposes of one booking
/// from two sources: MoneyMoney writes "Muller" where the bank sends "Müller".
/// Only for comparisons – fingerprints keep [`norm`], so stored import keys
/// stay valid.
fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in norm(s).chars() {
        match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ą' | 'ă' => out.push('a'),
            'ç' | 'č' | 'ć' => out.push('c'),
            'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ę' | 'ě' | 'ė' => out.push('e'),
            'ì' | 'í' | 'î' | 'ï' | 'ī' | 'į' | 'ı' => out.push('i'),
            'ñ' | 'ń' | 'ň' => out.push('n'),
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ő' => out.push('o'),
            'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' | 'ű' | 'ų' => out.push('u'),
            'ý' | 'ÿ' => out.push('y'),
            'š' | 'ś' => out.push('s'),
            'ž' | 'ź' | 'ż' => out.push('z'),
            'ł' => out.push('l'),
            'ß' => out.push_str("ss"),
            'đ' => out.push('d'),
            'æ' => out.push_str("ae"),
            'œ' => out.push_str("oe"),
            'ř' | 'ŕ' => out.push('r'),
            'ť' | 'ț' => out.push('t'),
            'ď' => out.push('d'),
            'ğ' => out.push('g'),
            'ş' | 'ș' => out.push('s'),
            'ľ' | 'ĺ' => out.push('l'),
            _ => out.push(c),
        }
    }
    out
}

/// Deterministic de-duplication key (stable across Rust versions: FNV-1a 64).
pub fn fingerprint(date: &str, cents: i64, iban: &str, name: &str, purpose: &str) -> String {
    let who = if iban.trim().is_empty() {
        norm(name)
    } else {
        norm(iban)
    };
    let key = format!("{date}|{cents}|{who}|{}", norm(purpose));
    format!("{:016x}", fnv1a64(key))
}

impl MmRow {
    /// Category path parts (empty = uncategorised).
    pub(crate) fn category_path(&self) -> Vec<String> {
        if self.category_parts.is_empty() {
            split_category(&self.category)
        } else {
            self.category_parts.clone()
        }
    }

    pub(crate) fn fp(&self) -> String {
        fingerprint(
            &self.booking_date,
            self.amount_cents,
            &self.iban,
            &self.name,
            &self.purpose,
        )
    }

    /// Key without the purpose (see `Existing::weak`).
    fn weak_fp(&self) -> String {
        fingerprint(
            &self.booking_date,
            self.amount_cents,
            &self.iban,
            &self.name,
            "",
        )
    }

    /// Date, amount and name (see `Existing::named`); only for a row without
    /// counterparty IBAN but with a name.
    fn named_fp(&self) -> Option<String> {
        (self.iban.trim().is_empty() && !self.name.trim().is_empty())
            .then(|| fingerprint(&self.booking_date, self.amount_cents, "", &self.name, ""))
    }

    /// Purpose as stored: the original text plus SEPA references that MoneyMoney
    /// keeps in separate columns (nothing is lost).
    fn stored_purpose(&self) -> String {
        let mut p = self.purpose.trim().to_string();
        for (label, v) in [
            ("EREF", &self.end_to_end),
            ("MREF", &self.mandate_ref),
            ("CRED", &self.creditor_id),
        ] {
            let v = v.trim();
            if !v.is_empty() && !p.contains(v) {
                if !p.is_empty() {
                    p.push(' ');
                }
                p.push_str(&format!("{label}: {v}"));
            }
        }
        p
    }
}

/// Splits `Haushalt\Lebensmittel`, `Haushalt/Lebensmittel`, `Steuern › Büro`,
/// `A : B`, `A -> B`, `A > B` into path parts (parent first).
/// `find_or_create_category` resolves the parts recursively via `parent_id`.
pub fn split_category(raw: &str) -> Vec<String> {
    raw.replace("->", "\\")
        .replace(" > ", "\\")
        .split(['\\', '/', '›', ':'])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

// ------------------------------------------------------------------ parsing

fn decode(data: &[u8]) -> String {
    let data = data.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(data);
    match std::str::from_utf8(data) {
        Ok(s) => s.to_string(),
        // Older MoneyMoney exports / Excel round-trips: Windows-1252.
        Err(_) => data
            .iter()
            .map(|&b| if b == 0x80 { '€' } else { b as char })
            .collect(),
    }
}

fn amount(s: &str) -> Option<i64> {
    let cleaned: String = s
        .chars()
        .filter(|c| !matches!(c, '€' | '$' | '£') && !c.is_alphabetic())
        .collect();
    let t = cleaned.trim();
    if t.is_empty() {
        return None;
    }
    if let Ok(v) = parse_cents(t) {
        return Some(v);
    }
    // "1.234" / "-1.234.567": German grouping without decimals.
    static GROUPED: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^([+-]?)(\d{1,3}(?:\.\d{3})+)$").unwrap());
    GROUPED.captures(t).and_then(|c| {
        let digits: String = c[2].chars().filter(|c| c.is_ascii_digit()).collect();
        let v: i64 = digits.parse::<i64>().ok()?.checked_mul(100)?;
        Some(if &c[1] == "-" { -v } else { v })
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Field {
    Date,
    Value,
    Category,
    Subcategory,
    Name,
    Purpose,
    Iban,
    Bic,
    Amount,
    Currency,
    Note,
    Mandate,
    Creditor,
    EndToEnd,
    Text,
    Check,
}

fn field_for(header: &str) -> Option<Field> {
    let h: String = header
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect();
    Some(match h.as_str() {
        "datum" | "buchungstag" | "buchungsdatum" | "date" | "bookingdate" | "booking" => {
            Field::Date
        }
        "wertstellung" | "wertstellungsdatum" | "valuedate" | "valuta" | "valutadatum" => {
            Field::Value
        }
        "kategorie" | "category" => Field::Category,
        "unterkategorie" | "subcategory" => Field::Subcategory,
        "name"
        | "empfänger"
        | "empfaenger"
        | "auftraggeber"
        | "zahlungsempfänger"
        | "zahlungsempfaenger"
        | "empfängerauftraggeber"
        | "empfaengerauftraggeber"
        | "payee"
        | "counterparty"
        | "payer" => Field::Name,
        "verwendungszweck" | "purpose" | "zweck" | "reference" => Field::Purpose,
        "konto" | "iban" | "account" | "kontonummer" | "accountnumber" | "gegenkonto" => {
            Field::Iban
        }
        "bank" | "bic" | "blz" | "bankcode" | "bankleitzahl" => Field::Bic,
        "betrag" | "amount" | "umsatz" | "value" => Field::Amount,
        "währung" | "waehrung" | "currency" => Field::Currency,
        "notiz" | "kommentar" | "comment" | "note" | "bemerkung" | "notizen" | "notes" => {
            Field::Note
        }
        "mandatsreferenz" | "mandatereference" | "mandate" | "mandatsref" => Field::Mandate,
        "gläubigerid" | "glaeubigerid" | "creditorid" | "gläubigeridentifikationsnummer" => {
            Field::Creditor
        }
        "endtoendreferenz" | "endtoendreference" | "endtoend" | "eref" => Field::EndToEnd,
        "buchungstext" | "bookingtext" | "art" | "transactiontype" => Field::Text,
        "haken" | "checkmark" | "geprüft" | "geprueft" | "checked" => Field::Check,
        _ => return None,
    })
}

fn detect_delimiter(text: &str) -> char {
    let first = text.lines().next().unwrap_or("");
    if let Some(rest) = first.strip_prefix("sep=")
        && let Some(c) = rest.chars().next()
    {
        return c;
    }
    let header = text
        .lines()
        .filter(|l| !l.starts_with("sep="))
        .find(|l| {
            let low = l.to_lowercase();
            ["datum", "date", "buchungstag", "betrag", "amount"]
                .iter()
                .any(|k| low.contains(k))
        })
        .unwrap_or(first);
    [';', '\t', ',']
        .into_iter()
        .max_by_key(|d| header.matches(*d).count())
        .unwrap_or(';')
}

fn build_row(get: impl Fn(Field) -> String) -> Option<MmRow> {
    let date = parse_date(&get(Field::Date))?;
    let cents = amount(&get(Field::Amount))?;
    let value = parse_date(&get(Field::Value)).unwrap_or_default();
    let clean = |s: String| crate::text::clean_bank_text(s.trim());
    Some(MmRow {
        booking_date: date,
        value_date: value,
        amount_cents: cents,
        currency: {
            let c = get(Field::Currency).trim().to_uppercase();
            if c.is_empty() { "EUR".into() } else { c }
        },
        name: clean(get(Field::Name)),
        iban: normalize_iban(&get(Field::Iban)),
        bic: get(Field::Bic).trim().to_string(),
        purpose: clean(get(Field::Purpose)),
        booking_text: clean(get(Field::Text)),
        category: get(Field::Category).trim().to_string(),
        category_parts: Vec::new(),
        note: get(Field::Note).trim().to_string(),
        checkmark: parse_bool_loose(&get(Field::Check)),
        mandate_ref: get(Field::Mandate).trim().to_string(),
        creditor_id: get(Field::Creditor).trim().to_string(),
        end_to_end: get(Field::EndToEnd).trim().to_string(),
        ..Default::default()
    })
}

pub fn parse_csv_export(text: &str) -> CoreResult<Vec<MmRow>> {
    let delim = detect_delimiter(text);
    let body: String = text
        .lines()
        .filter(|l| !l.starts_with("sep="))
        .collect::<Vec<_>>()
        .join("\n");
    let rows = split_csv(&body, delim);
    let (hi, mut cols) = rows
        .iter()
        .enumerate()
        .find_map(|(i, r)| {
            let map: Vec<Option<Field>> = r.iter().map(|c| field_for(c)).collect();
            (map.contains(&Some(Field::Date)) && map.contains(&Some(Field::Amount)))
                .then_some((i, map))
        })
        .ok_or_else(|| CoreError::InvalidInput {
            message: tr("Keine Kopfzeile (Datum/Buchungstag und Betrag) gefunden.").into(),
        })?;
    // C24's Kontonummer belongs to the source account, not the counterparty.
    // Keep the two category columns as literal path components (names may
    // contain separators that MoneyMoney uses for its single category field).
    // Resolve format-specific columns once, rather than normalising and
    // searching the same headers again for every booking.
    let headers: Vec<String> = rows[hi].iter().map(|h| norm(h)).collect();
    let has_subcategory = cols.contains(&Some(Field::Subcategory));
    let is_c24 = has_subcategory && headers.iter().any(|h| h == "transaktionstyp");
    if is_c24 {
        for (header, field) in headers.iter().zip(cols.iter_mut()) {
            if header == "kontonummer" {
                *field = None;
            }
        }
    }
    let booking_text_columns =
        ["beschreibung", "transaktionstyp"].map(|name| headers.iter().position(|h| h == name));
    let mut out = Vec::new();
    for r in rows.iter().skip(hi + 1) {
        // First column wins when a header occurs twice (e.g. "Konto" + "IBAN").
        let mut seen: HashMap<Field, String> = HashMap::new();
        for (i, f) in cols.iter().enumerate() {
            if let (Some(f), Some(v)) = (f, r.get(i)) {
                let e = seen.entry(*f).or_default();
                if e.trim().is_empty() {
                    *e = v.clone();
                }
            }
        }
        if let Some(mut row) = build_row(|f| seen.get(&f).cloned().unwrap_or_default()) {
            if has_subcategory {
                row.category_parts = [Field::Category, Field::Subcategory]
                    .iter()
                    .filter_map(|f| seen.get(f))
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            if is_c24 {
                row.booking_text = booking_text_columns
                    .iter()
                    .flatten()
                    .find_map(|&i| r.get(i).filter(|s| !s.trim().is_empty()))
                    .map(|s| crate::text::clean_bank_text(s.trim()))
                    .unwrap_or_default();
            }
            out.push(row);
        }
    }
    Ok(out)
}

fn xml_unescape(s: &str) -> String {
    // One pass over all entities: a decoded `&` must not start another one
    // (`&#38;lt;` is the text "&lt;", not "<").
    static ENTITY: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"&(?:#(x?[0-9a-fA-F]+)|(lt|gt|quot|apos|amp));").unwrap());
    let s = s.trim();
    let s = s
        .strip_prefix("<![CDATA[")
        .and_then(|x| x.strip_suffix("]]>"))
        .map(|x| x.to_string())
        .unwrap_or_else(|| {
            ENTITY
                .replace_all(s, |c: &regex::Captures| {
                    if let Some(v) = c.get(1) {
                        let v = v.as_str();
                        let n = match v.strip_prefix('x') {
                            Some(h) => u32::from_str_radix(h, 16).ok(),
                            None => v.parse().ok(),
                        };
                        return n
                            .and_then(char::from_u32)
                            .map(|c| c.to_string())
                            .unwrap_or_default();
                    }
                    match &c[2] {
                        "lt" => "<",
                        "gt" => ">",
                        "quot" => "\"",
                        "apos" => "'",
                        _ => "&",
                    }
                    .to_string()
                })
                .into_owned()
        });
    s.trim().to_string()
}

pub fn parse_xml_export(text: &str) -> CoreResult<Vec<MmRow>> {
    static BLOCK: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?is)<transaction(?:\s[^>]*)?>(.*?)</transaction\s*>").unwrap()
    });
    static LEAF: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?is)<([A-Za-z_][\w.\-]*)(?:\s[^>]*)?>(.*?)</([A-Za-z_][\w.\-]*)\s*>").unwrap()
    });
    let mut out = Vec::new();
    for b in BLOCK.captures_iter(text) {
        let mut fields: HashMap<Field, String> = HashMap::new();
        for l in LEAF.captures_iter(&b[1]) {
            if !l[1].eq_ignore_ascii_case(&l[3]) {
                continue;
            }
            let tag = l[1].to_lowercase();
            let f = match tag.as_str() {
                "bookingdate" | "date" => Some(Field::Date),
                "valuedate" => Some(Field::Value),
                "accountnumber" | "iban" => Some(Field::Iban),
                "bankcode" | "bic" => Some(Field::Bic),
                "booking" | "bookingtext" => Some(Field::Text),
                "mandatereference" => Some(Field::Mandate),
                "creditorid" => Some(Field::Creditor),
                "endtoendreference" => Some(Field::EndToEnd),
                "checkmark" => Some(Field::Check),
                other => field_for(other),
            };
            if let Some(f) = f {
                fields.entry(f).or_insert_with(|| xml_unescape(&l[2]));
            }
        }
        if let Some(row) = build_row(|f| fields.get(&f).cloned().unwrap_or_default()) {
            out.push(row);
        }
    }
    Ok(out)
}

fn remote_to_row(t: crate::connectors::RemoteTransaction) -> MmRow {
    MmRow {
        booking_date: t.booking_date,
        value_date: t.value_date,
        amount_cents: t.amount_cents,
        currency: if t.currency.is_empty() {
            "EUR".into()
        } else {
            t.currency
        },
        name: t.counterparty,
        iban: t.counterparty_iban,
        purpose: t.purpose,
        booking_text: t.booking_text,
        ..Default::default()
    }
}

/// CAMT.053 (ISO 20022 account statement) file → rows.
pub fn parse_camt_statement(xml: &str) -> Vec<MmRow> {
    let (txs, _) = crate::connectors::fints::camt::parse(xml, "");
    txs.into_iter().map(remote_to_row).collect()
}

fn looks_like_mt940(text: &str) -> bool {
    let t = text.replace("\r", "");
    t.contains(":61:") && (t.contains(":20:") || t.contains(":25:") || t.contains(":60F:"))
}

/// MT940 (`.sta` / `.mt940`) file → rows.
pub fn parse_mt940_statement(text: &str) -> Vec<MmRow> {
    let (txs, _) = crate::connectors::fints::mt940::parse(text, "", "EUR");
    txs.into_iter().map(remote_to_row).collect()
}

/// Largest accepted export file (CAMT/MT940/CSV/XML).
const MAX_EXPORT_BYTES: usize = 50 * 1024 * 1024;

/// Detects CAMT.053 / MT940 / MoneyMoney XML / CSV by content and parses. Errors when nothing was found.
pub fn parse_export(data: &[u8]) -> CoreResult<Vec<MmRow>> {
    if data.len() > MAX_EXPORT_BYTES {
        return Err(CoreError::InvalidInput {
            message: tr("Die Datei ist zu groß (maximal 50 MB).").into(),
        });
    }
    let text = decode(data);
    let rows = if text.trim_start().starts_with('<') {
        if text.contains("<BkToCstmr") || text.contains(":BkToCstmr") {
            parse_camt_statement(&text)
        } else {
            parse_xml_export(&text)?
        }
    } else if looks_like_mt940(&text) {
        parse_mt940_statement(&text)
    } else {
        parse_csv_export(&text)?
    };
    if rows.is_empty() {
        return Err(CoreError::InvalidInput {
            message: tr("Keine Umsätze erkannt.").into(),
        });
    }
    Ok(rows)
}

// ------------------------------------------------------------------ import

#[derive(Default)]
pub(crate) struct Stats {
    pub(crate) total: u32,
    pub(crate) new: u32,
    pub(crate) duplicates: u32,
    pub(crate) enriched: u32,
    pub(crate) notes: u32,
    pub(crate) categories_created: u32,
    pub(crate) account_id: i64,
    pub(crate) account_created: bool,
    pub(crate) balance_cents: i64,
}

pub(crate) struct Existing {
    id: i64,
    /// Exact fingerprint (date, amount, counterparty, purpose).
    fp: String,
    /// Without the purpose: MoneyMoney's exports append the SEPA references
    /// to the purpose (CSV ", Mandatsref: …", camt "EREF+… SVWZ+…"), while
    /// the database keeps them apart.
    weak: String,
    /// Date, amount and name only: the database import fills in the own IBAN
    /// for transfers that MoneyMoney only knows by account number (#94), the
    /// export leaves it empty.
    named: String,
    category_source: String,
    note: String,
    /// Soft-deleted: counts as present, but is never updated.
    pub(crate) deleted: bool,
    booking_date: String,
    amount_cents: i64,
    bare_bank_purpose: Option<String>,
    /// Counterparty IBAN and accent-free name, for [`match_folded_name`].
    iban_norm: String,
    name_fold: String,
    split_set: bool,
    imported: Option<(String, i64)>,
    from_import: bool,
}

/// The account's rows an import matches against, deleted ones included (a
/// booking the user deleted is not imported again). Dropped holds are left
/// out: one carries the date, amount and text of the payment, so the posted
/// booking would match it and never be imported (#1134). That covers settled
/// holds, holds dropped by „Abrufweg umstellen“ (`switch-stale-…:pending-…`)
/// and the card holds an older switch had marked (`mm-switch-pending-hold-…`).
pub(crate) fn load_existing(conn: &Connection, account_id: i64) -> CoreResult<Vec<Existing>> {
    let mut q = conn.prepare(
        "SELECT id, booking_date, amount_cents, counterparty_iban, counterparty, purpose,
                import_fp, category_source, note, deleted_ms IS NOT NULL, external_id,
                splits_ms != 0 OR EXISTS (SELECT 1 FROM transaction_splits s WHERE s.transaction_id = transactions.id),
                external_id LIKE 'mm-%', external_id LIKE 'mm-%' OR import_fp != ''
         FROM transactions WHERE account_id = ?1
           AND NOT (deleted_ms IS NOT NULL
                    AND (external_id LIKE 'pending-%' OR external_id LIKE 'mm-switch-pending-hold-%'
                         OR (external_id LIKE 'switch-stale-%' AND external_id LIKE '%:pending-%')))
         ORDER BY id",
    )?;
    let it = q.query_map([account_id], |r| {
        let (date, cents, iban, name, purpose): (String, i64, String, String, String) =
            (r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?);
        let fp: String = r.get(6)?;
        let external_id: String = r.get(10)?;
        let bare =
            !external_id.starts_with("mm-") && norm(&name).is_empty() && norm(&iban).is_empty();
        Ok(Existing {
            id: r.get(0)?,
            fp: if fp.is_empty() {
                fingerprint(&date, cents, &iban, &name, &purpose)
            } else {
                fp
            },
            weak: fingerprint(&date, cents, &iban, &name, ""),
            named: fingerprint(&date, cents, "", &name, ""),
            category_source: r.get(7)?,
            note: r.get(8)?,
            deleted: r.get(9)?,
            bare_bank_purpose: bare.then(|| fold(&purpose)),
            iban_norm: norm(&iban),
            name_fold: fold(&name),
            booking_date: date.clone(),
            amount_cents: cents,
            split_set: r.get(11)?,
            imported: r.get::<_, bool>(12)?.then_some((date, cents)),
            from_import: r.get(13)?,
        })
    })?;
    Ok(it.collect::<Result<_, _>>()?)
}

#[derive(Default)]
pub(crate) struct KnownKeys {
    fp: HashSet<String>,
    weak: HashSet<String>,
    named: HashSet<String>,
    imported: HashMap<(String, i64), u32>,
}

impl KnownKeys {
    pub(crate) fn from_existing(existing: &[Existing]) -> Self {
        let mut keys = Self::default();
        for e in existing.iter().filter(|e| e.from_import) {
            keys.fp.insert(e.fp.clone());
            keys.weak.insert(e.weak.clone());
            keys.named.insert(e.named.clone());
            if let Some((date, cents)) = &e.imported {
                keys.insert_imported(date.clone(), *cents);
            }
        }
        keys
    }

    pub(crate) fn insert_imported(&mut self, date: String, cents: i64) {
        *self.imported.entry((date, cents)).or_default() += 1;
    }

    fn contains(&self, r: &MmRow) -> bool {
        self.fp.contains(&r.fp())
            || self.weak.contains(&r.weak_fp())
            || r.named_fp().is_some_and(|k| self.named.contains(&k))
    }

    fn has_parts(&self, parts: &[MmRow]) -> bool {
        if parts.iter().any(|p| self.contains(p)) {
            return true;
        }
        if parts.len() < 2 {
            return false;
        }
        let mut need: HashMap<(&str, i64), u32> = HashMap::new();
        for p in parts {
            *need.entry((&p.booking_date, p.amount_cents)).or_default() += 1;
        }
        need.iter().all(|(&(date, cents), n)| {
            self.imported
                .get(&(date.to_string(), cents))
                .is_some_and(|have| have >= n)
        })
    }
}

pub(crate) fn with_known_parts(
    rows: &[MmRow],
    keys: impl FnOnce() -> KnownKeys,
) -> Cow<'_, [MmRow]> {
    if rows.iter().all(|r| r.parts.is_empty()) {
        return Cow::Borrowed(rows);
    }
    let keys = keys();
    let taken_apart = |r: &MmRow| keys.has_parts(&r.parts);
    if !rows.iter().any(taken_apart) {
        return Cow::Borrowed(rows);
    }
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        if taken_apart(r) {
            out.extend(r.parts.iter().filter(|p| keys.contains(p)).cloned());
        } else {
            out.push(r.clone());
        }
    }
    Cow::Owned(out)
}

fn write_splits(
    conn: &Connection,
    id: i64,
    splits: &[MmSplit],
    preserve_notes: bool,
    cat_cache: &mut HashMap<String, i64>,
    st: &mut Stats,
) -> CoreResult<()> {
    let mut lines = Vec::with_capacity(splits.len());
    for s in splits {
        let cat = if s.category_parts.is_empty() {
            None
        } else {
            Some(find_or_create_category(
                conn,
                &s.category_parts,
                cat_cache,
                &mut st.categories_created,
            )?)
        };
        if preserve_notes && !s.note.trim().is_empty() {
            st.notes += 1;
        }
        lines.push((cat, s.amount_cents, split_note(s, preserve_notes)));
    }
    crate::planning::store_splits(conn, id, &lines)
}

fn split_note(s: &MmSplit, own_note: bool) -> String {
    let own = if own_note { s.note.trim() } else { "" };
    join_notes([own, s.text.trim()].map(str::to_string))
}

fn join_notes(notes: impl IntoIterator<Item = String>) -> String {
    notes
        .into_iter()
        .filter(|n| !n.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// For each row the existing booking it duplicates (index into `existing`).
/// Both passes count as multisets – every existing booking is used once, so
/// two identical coffees on one day stay two rows:
/// 1. exact fingerprint,
/// 2. rows still unmatched: same date, amount and counterparty (purpose
///    ignored), in order,
/// 3. rows without counterparty IBAN: same date, amount and name,
/// 4. [`match_folded_name`],
/// 5. [`match_bank_without_counterparty`].
pub(crate) fn match_existing(rows: &[MmRow], existing: &[Existing]) -> Vec<Option<usize>> {
    let (mut out, mut used) = match_by_fingerprint(rows, existing);
    let mut by_weak: HashMap<&str, std::collections::VecDeque<usize>> = HashMap::new();
    for (j, e) in existing.iter().enumerate().filter(|(j, _)| !used[*j]) {
        by_weak.entry(e.weak.as_str()).or_default().push_back(j);
    }
    for (r, m) in rows.iter().zip(out.iter_mut()).filter(|(_, m)| m.is_none()) {
        if let Some(j) = by_weak
            .get_mut(r.weak_fp().as_str())
            .and_then(|q| q.pop_front())
        {
            used[j] = true;
            *m = Some(j);
        }
    }
    let mut by_name: HashMap<&str, std::collections::VecDeque<usize>> = HashMap::new();
    for (j, e) in existing.iter().enumerate().filter(|(j, _)| !used[*j]) {
        by_name.entry(e.named.as_str()).or_default().push_back(j);
    }
    for (r, m) in rows.iter().zip(out.iter_mut()).filter(|(_, m)| m.is_none()) {
        if let Some(key) = r.named_fp()
            && let Some(j) = by_name.get_mut(key.as_str()).and_then(|q| q.pop_front())
        {
            used[j] = true;
            *m = Some(j);
        }
    }
    match_folded_name(rows, existing, &mut out, &mut used);
    match_bank_without_counterparty(rows, existing, &mut out, &mut used);
    out
}

/// Same date, amount and name apart from accents ("Müller" from the bank,
/// "Muller" from MoneyMoney), the IBANs not contradicting each other (one side
/// without, or both the same). Pass 3 compares names exactly and only for
/// rows without IBAN, so an accent or an IBAN on one side only kept them
/// apart. In order, like the other passes.
pub(crate) fn match_folded_name(
    rows: &[MmRow],
    existing: &[Existing],
    out: &mut [Option<usize>],
    used: &mut [bool],
) {
    for (r, m) in rows.iter().zip(out.iter_mut()) {
        if m.is_some() {
            continue;
        }
        let (name, iban) = (fold(&r.name), norm(&r.iban));
        if name.len() < 3 {
            continue;
        }
        let hit = existing.iter().enumerate().position(|(j, e)| {
            !used[j]
                && e.booking_date == r.booking_date
                && e.amount_cents == r.amount_cents
                && e.name_fold == name
                && (iban.is_empty() || e.iban_norm.is_empty() || e.iban_norm == iban)
        });
        if let Some(j) = hit {
            used[j] = true;
            *m = Some(j);
        }
    }
}

/// Pass 1 of [`match_existing`]: exact fingerprint, as a multiset. Returns
/// the matches and which existing bookings they used.
pub(crate) fn match_by_fingerprint(
    rows: &[MmRow],
    existing: &[Existing],
) -> (Vec<Option<usize>>, Vec<bool>) {
    let mut by_fp: HashMap<&str, std::collections::VecDeque<usize>> = HashMap::new();
    for (j, e) in existing.iter().enumerate() {
        by_fp.entry(e.fp.as_str()).or_default().push_back(j);
    }
    let mut used = vec![false; existing.len()];
    let out = rows
        .iter()
        .map(|r| {
            let j = by_fp.get_mut(r.fp().as_str())?.pop_front()?;
            used[j] = true;
            Some(j)
        })
        .collect();
    (out, used)
}

/// A bank booking without counterparty (no name, no IBAN) that was fetched
/// before the import, e.g. comdirect's interest statement via FinTS, whose
/// MoneyMoney twin carries the first line as name ("Abschluss Zinsen Kto …")
/// and the rest as purpose. Same rule as the sync takes ([`find_twin`]): same
/// amount, booking date within ±2 days, and the MoneyMoney name inside the
/// bank's purpose or both purposes starting alike. Only a unique candidate
/// counts – a guess would put the category on the wrong booking.
pub(crate) fn match_bank_without_counterparty(
    rows: &[MmRow],
    existing: &[Existing],
    out: &mut [Option<usize>],
    used: &mut [bool],
) {
    let days = |a: &str, b: &str| {
        let d = |s: &str| jiff::civil::Date::strptime("%Y-%m-%d", s).ok();
        Some(crate::dates::days_between(d(a)?, d(b)?).abs())
    };
    for (r, m) in rows.iter().zip(out.iter_mut()) {
        if m.is_some() {
            continue;
        }
        let (name, purpose) = (fold(&r.name), fold(&purpose_without_refs(&r.purpose)));
        let mut hits = existing.iter().enumerate().filter(|(j, e)| {
            !used[*j]
                && e.amount_cents == r.amount_cents
                && days(&e.booking_date, &r.booking_date).is_some_and(|d| d <= 2)
                && e.bare_bank_purpose.as_deref().is_some_and(|p| {
                    (name.len() >= 3 && p.contains(&name)) || purposes_alike(p, &purpose)
                })
        });
        if let (Some((j, _)), None) = (hits.next(), hits.next()) {
            used[j] = true;
            *m = Some(j);
        }
    }
}

pub(crate) fn find_or_create_category(
    conn: &Connection,
    path: &[String],
    cache: &mut HashMap<String, i64>,
    created: &mut u32,
) -> CoreResult<i64> {
    let mut parent: Option<i64> = None;
    let mut key = String::new();
    for part in path {
        key.push('\u{1}');
        key.push_str(&part.to_lowercase());
        if let Some(id) = cache.get(&key) {
            parent = Some(*id);
            continue;
        }
        let found: Option<i64> = conn
            .query_row(
                "SELECT id FROM categories WHERE parent_id IS ?1 AND name = ?2 COLLATE NOCASE AND deleted_ms IS NULL",
                params![parent, part],
                |r| r.get(0),
            )
            .optional()?;
        let id = match found {
            Some(id) => {
                // An imported path is user-owned even when it reuses a starter
                // row. Its literal name must survive a UI language switch.
                conn.execute("UPDATE categories SET builtin_role=NULL WHERE id=?1", [id])?;
                id
            }
            None => {
                let order: i32 = conn.query_row(
                    "SELECT IFNULL(MAX(sort_order), 0) + 1 FROM categories WHERE parent_id IS ?1",
                    [parent],
                    |r| r.get(0),
                )?;
                conn.execute(
                    "INSERT INTO categories(parent_id, name, sf_symbol, color_hex, sort_order, name_ms, parent_ms, symbol_ms, color_ms, order_ms)
                     VALUES (?1, ?2, 'tag', '#8E8E93', ?3, ?4, ?4, ?4, ?4, ?4)",
                    params![parent, part, order, now_ms()],
                )?;
                *created += 1;
                let id = conn.last_insert_rowid();
                crate::sync_merge::fill_missing_uuids(conn)?;
                id
            }
        };
        // Preserve the importer's established transfer semantics explicitly;
        // this role never grants localisation of an imported name.
        if matches!(
            part.trim().to_lowercase().as_str(),
            "umbuchung" | "umbuchungen" | "geldtransit" | "transfer" | "transfers"
        ) {
            conn.execute(
                "UPDATE categories SET builtin_role='import:transfers' WHERE id=?1",
                [id],
            )?;
        }
        cache.insert(key.clone(), id);
        parent = Some(id);
    }
    parent.ok_or_else(|| CoreError::InvalidInput {
        message: "empty category".into(),
    })
}

/// The own account of a single-account statement file: camt.053 `<Acct>`
/// (IBAN + currency) or MT940 `:25:` when it holds an IBAN. Files with
/// several statements name no single account.
fn statement_account(text: &str) -> Option<(String, String)> {
    static STMT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"<(?:\w+:)?(?:Stmt|Rpt|Ntfctn)>").unwrap());
    static ACCT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)<(?:\w+:)?Acct>(.*?)</(?:\w+:)?Acct>").unwrap());
    static IBAN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"<(?:\w+:)?IBAN>\s*([^<]+?)\s*<").unwrap());
    static CCY: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"<(?:\w+:)?Ccy>\s*([^<]+?)\s*<").unwrap());
    static FIELD_25: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^:25:(.+)$").unwrap());
    if text.contains("BkToCstmr") {
        // camt.053 `Stmt`, camt.052 `Rpt`, camt.054 `Ntfctn`.
        if STMT.find_iter(text).count() != 1 {
            return None;
        }
        let block = ACCT.captures(text)?.get(1)?.as_str();
        let i = normalize_iban(IBAN.captures(block)?.get(1)?.as_str());
        let c = CCY
            .captures(block)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_uppercase())
            .unwrap_or_default();
        return validate_iban(&i).then_some((i, c));
    }
    let mut it = FIELD_25.captures_iter(text);
    let first = normalize_iban(it.next()?.get(1)?.as_str().trim());
    if it.any(|c| normalize_iban(c[1].trim()) != first) {
        return None;
    }
    validate_iban(&first).then(|| (first, String::new()))
}

/// The existing account an export most likely belongs to, so the assistant
/// can preselect it instead of creating a second archive account:
/// 1. the statement's own IBAN (+ currency when several accounts share it),
/// 2. the account already holding most of the bookings (date + amount; at
///    least half the rows and clearly ahead of the next one),
/// 3. an account named like the file ("Girokonto.csv" → "Girokonto").
pub(crate) fn suggest_account(
    conn: &Connection,
    text: &str,
    rows: &[MmRow],
    file_name: &str,
) -> CoreResult<Option<i64>> {
    let mut accounts: Vec<(i64, String, String, String)> = Vec::new();
    {
        let mut q = conn.prepare(
            "SELECT id, iban, currency, name FROM accounts WHERE deleted_ms IS NULL ORDER BY id",
        )?;
        for a in q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))? {
            accounts.push(a?);
        }
    }
    if let Some((iban, ccy)) = statement_account(text) {
        let same: Vec<_> = accounts
            .iter()
            .filter(|a| normalize_iban(&a.1) == iban)
            .collect();
        let pick: Vec<_> = if same.len() > 1 && !ccy.is_empty() {
            same.iter()
                .filter(|a| a.2.eq_ignore_ascii_case(&ccy))
                .collect()
        } else {
            same.iter().collect()
        };
        if pick.len() == 1 {
            return Ok(Some(pick[0].0));
        }
    }
    let dates = rows.iter().map(|r| r.booking_date.as_str());
    if let (Some(from), Some(to)) = (dates.clone().min(), dates.max()) {
        let mut have: HashMap<i64, HashMap<(String, i64), u32>> = HashMap::new();
        let mut q = conn.prepare(
            "SELECT t.account_id, t.booking_date, t.amount_cents FROM transactions t
               JOIN accounts a ON a.id = t.account_id
              WHERE a.deleted_ms IS NULL AND t.booking_date BETWEEN ?1 AND ?2",
        )?;
        for r in q.query_map(params![from, to], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })? {
            let (a, d, c) = r?;
            *have.entry(a).or_default().entry((d, c)).or_default() += 1;
        }
        let mut score: Vec<(u32, i64)> = have
            .into_iter()
            .map(|(a, mut m)| {
                let hits = rows
                    .iter()
                    .filter(|r| {
                        m.get_mut(&(r.booking_date.clone(), r.amount_cents))
                            .filter(|n| **n > 0)
                            .map(|n| *n -= 1)
                            .is_some()
                    })
                    .count() as u32;
                (hits, a)
            })
            .collect();
        score.sort_by(|x, y| y.cmp(x));
        if let Some(&(best, a)) = score.first() {
            let next = score.get(1).map_or(0, |s| s.0);
            if best as usize * 2 >= rows.len() && best > next.saturating_mul(2) {
                return Ok(Some(a));
            }
        }
    }
    let stem = account_name_from_file(file_name);
    let stem = stem
        .strip_suffix(" (Import)")
        .or_else(|| stem.strip_suffix(" (MoneyMoney)")) // suffix of earlier versions
        .unwrap_or(&stem)
        .trim()
        .to_lowercase();
    let named: Vec<_> = accounts
        .iter()
        .filter(|a| a.3.trim().to_lowercase() == stem)
        .collect();
    Ok((named.len() == 1).then(|| named[0].0))
}

fn account_name_from_file(file_name: &str) -> String {
    let stem = file_name
        .rsplit('/')
        .next()
        .unwrap_or(file_name)
        .rsplit_once('.')
        .map(|x| x.0)
        .unwrap_or(file_name)
        .trim();
    if stem.is_empty() {
        "Datenimport".into()
    } else {
        format!("{stem} (Import)")
    }
}

fn run(
    conn: &Connection,
    rows: &[MmRow],
    file_name: &str,
    target: Option<i64>,
    preserve_categories: bool,
    preserve_notes: bool,
) -> CoreResult<Stats> {
    let mut st = Stats {
        total: rows.len() as u32,
        ..Default::default()
    };
    let account_currency;
    match target {
        Some(id) => {
            account_currency = conn
                .query_row(
                    "SELECT currency FROM accounts WHERE id = ?1 AND deleted_ms IS NULL",
                    [id],
                    |r| r.get::<_, String>(0),
                )
                .optional()?
                .ok_or_else(|| CoreError::NotFound {
                    what: trf("Konto {0}", &[&id]),
                })?;
            st.account_id = id;
        }
        None => {
            account_currency = rows
                .first()
                .map(|r| r.currency.clone())
                .unwrap_or_else(|| "EUR".into());
            let gid = Vault::ensure_group(conn, ARCHIVE_GROUP, "archivebox", 90)?;
            conn.execute(
                "INSERT INTO accounts(connection_id, group_id, external_ref, name, bank_name, currency, kind,
                                      balance_cents, balance_date, sf_symbol, sort_order, created_ms)
                 VALUES (NULL, ?1, ?2, ?3, 'MoneyMoney-Import', ?4, 'checking', 0, '', 'archivebox',
                         (SELECT IFNULL(MAX(sort_order),0)+1 FROM accounts), ?5)",
                params![
                    gid,
                    format!("moneymoney-{}", uuid::Uuid::new_v4()),
                    account_name_from_file(file_name),
                    account_currency,
                    now_ms()
                ],
            )?;
            st.account_id = conn.last_insert_rowid();
            st.account_created = true;
        }
    }

    let mut cat_cache = HashMap::new();
    merge_rows(
        conn,
        rows,
        &account_currency,
        preserve_categories,
        preserve_notes,
        &mut cat_cache,
        &mut st,
    )?;
    if st.account_created {
        let last = rows
            .iter()
            .map(|r| r.booking_date.as_str())
            .max()
            .unwrap_or("");
        conn.execute(
            "UPDATE accounts SET balance_cents = ?1, balance_date = ?2 WHERE id = ?3",
            params![st.balance_cents, last, st.account_id],
        )?;
    }
    // Rows without a MoneyMoney category get the regular rule treatment.
    Vault::apply_rules_inner(conn, true)?;
    Ok(st)
}

/// Merges normalised rows into the account `st.account_id`: new rows are
/// inserted, rows already present (same fingerprint, counted as a multiset) are
/// only enriched with category / note. Rules are *not* run here.
pub(crate) fn merge_rows(
    conn: &Connection,
    rows: &[MmRow],
    account_currency: &str,
    preserve_categories: bool,
    preserve_notes: bool,
    cat_cache: &mut HashMap<String, i64>,
    st: &mut Stats,
) -> CoreResult<()> {
    let existing = load_existing(conn, st.account_id)?;
    let rows = with_known_parts(rows, || KnownKeys::from_existing(&existing));
    let match_of = match_existing(&rows, &existing);

    let mut seen: HashMap<String, usize> = HashMap::new();
    for (i, r) in rows.iter().enumerate() {
        let fp = r.fp();
        let k = {
            let n = seen.entry(fp.clone()).or_insert(0);
            let k = *n;
            *n += 1;
            k
        };
        let cat_id = if preserve_categories {
            let path = r.category_path();
            if path.is_empty() {
                None
            } else {
                Some(find_or_create_category(
                    conn,
                    &path,
                    cat_cache,
                    &mut st.categories_created,
                )?)
            }
        } else {
            None
        };
        // #877: without categories a split booking has no splits; it takes the
        // parts' notes. `notes`: how many MoneyMoney notes `note` carries.
        let (note_text, notes): (Cow<str>, u32) = if !preserve_notes {
            (Cow::Borrowed(""), 0)
        } else if preserve_categories || r.splits.is_empty() {
            (Cow::Borrowed(&r.note), u32::from(!r.note.is_empty()))
        } else {
            let own = std::iter::once(&r.note).chain(r.splits.iter().map(|s| &s.note));
            let lines = std::iter::once(r.note.trim().to_string())
                .chain(r.splits.iter().map(|s| split_note(s, true)));
            (
                Cow::Owned(join_notes(lines)),
                own.filter(|n| !n.trim().is_empty()).count() as u32,
            )
        };
        let note: &str = &note_text;
        if let Some(e) = match_of[i].map(|j| &existing[j]) {
            st.duplicates += 1;
            if e.deleted {
                continue;
            }
            let mut changed = false;
            if let Some(c) = cat_id.filter(|_| e.category_source != "manual") {
                conn.execute(
                    "UPDATE transactions SET category_id = ?1, rule_id = NULL, category_source = 'manual', category_ms = ?3 WHERE id = ?2",
                    params![c, e.id, now_ms()],
                )?;
                changed = true;
            }
            // #877: a booking already here (e.g. from the bank) becomes the split
            // booking, unless the user categorised or split it.
            if preserve_categories
                && !r.splits.is_empty()
                && !e.split_set
                && e.category_source != "manual"
            {
                write_splits(conn, e.id, &r.splits, preserve_notes, cat_cache, st)?;
                crate::planning::clear_split_category(conn, e.id)?;
                changed = true;
            }
            if !note.is_empty() && e.note.trim().is_empty() {
                conn.execute(
                    "UPDATE transactions SET note = ?1, note_ms = ?3 WHERE id = ?2",
                    params![note, e.id, now_ms()],
                )?;
                st.notes += notes;
                changed = true;
            }
            if changed {
                st.enriched += 1;
            }
            continue;
        }
        conn.prepare_cached(
            "INSERT INTO transactions(account_id, external_id, booking_date, value_date, amount_cents, currency,
                    counterparty, counterparty_iban, purpose, booking_text, category_id, category_source, checked, note, import_fp, category_ms, note_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 1, ?13, ?14, ?15, ?16)",
        )?
        .execute(params![
            st.account_id,
            format!("mm-{fp}-{k}"),
            r.booking_date,
            r.value_date,
            r.amount_cents,
            if r.currency.is_empty() { account_currency } else { &r.currency },
            r.name,
            r.iban,
            r.stored_purpose(),
            r.booking_text,
            cat_id,
            if cat_id.is_some() { "manual" } else { "none" },
            note,
            fp,
            if cat_id.is_some() { now_ms() } else { 0 },
            if note.is_empty() { 0 } else { now_ms() }
        ])?;
        if preserve_categories && !r.splits.is_empty() {
            let id = conn.last_insert_rowid();
            write_splits(conn, id, &r.splits, preserve_notes, cat_cache, st)?;
        }
        st.new += 1;
        st.balance_cents = st.balance_cents.saturating_add(r.amount_cents);
        st.notes += notes;
    }
    Ok(())
}

/// Finds the one not-yet-adopted MoneyMoney row (`mm-*`) that is the twin of a bank
/// booking: same amount, booking date within ±2 days and a matching counterparty
/// (IBAN, name, or the MoneyMoney name appearing in the bank's name/purpose).
///
/// Several candidates (#830, e.g. two equal card payments at comdirect, where the
/// bank names itself as counterparty) are resolved only when that is safe:
/// the one closest in date wins; among equally close ones that the bank cannot tell
/// apart either (see [`same_twin`]), the first is taken – the second bank booking then
/// finds the other. Anything else stays ambiguous and yields `None`.
fn find_twin(
    conn: &Connection,
    account_id: i64,
    exclude_id: Option<i64>,
    date: &str,
    cents: i64,
    iban: &str,
    name: &str,
    purpose: &str,
) -> CoreResult<Option<i64>> {
    let mut stmt = conn.prepare_cached(TWIN_CANDIDATES_SQL)?;
    let rows = stmt.query_map(
        params![account_id, cents, exclude_id.unwrap_or(-1), date],
        |r| {
            Ok(Twin {
                id: r.get(0)?,
                name: r.get(1)?,
                iban: r.get(2)?,
                purpose: r.get(3)?,
                date: r.get(4)?,
                category: r.get(5)?,
                note: r.get(6)?,
                days: r.get::<_, f64>(7)?.round() as i64,
            })
        },
    )?;
    // names and purposes without accents: MoneyMoney writes "Muller" where the
    // bank sends "Müller"; the tax lines keep their own normalisation
    let (n_iban, n_name, n_purpose) = (norm(iban), fold(name), fold(purpose));
    let (tax_name, tax_purpose) = (norm(name), norm(purpose));
    let mut hits = Vec::new();
    for row in rows {
        let t = row?;
        let (m_iban, m_name) = (norm(&t.iban), fold(&t.name));
        let ok = if !n_iban.is_empty() && !m_iban.is_empty() {
            n_iban == m_iban
        } else {
            let long = n_name.len() >= 3 && m_name.len() >= 3;
            (long && (n_name.contains(&m_name) || m_name.contains(&n_name)))
                || (m_name.len() >= 3 && (n_purpose.contains(&m_name) || n_name.contains(&m_name)))
                // the bank sends no counterparty (comdirect REST, some debits): the
                // purposes still start the same (reference numbers, invoice text)
                || purposes_alike(&n_purpose, &fold(&t.purpose))
                || (tax_name.is_empty()
                    && same_tax_line(&tax_purpose, &norm(&t.name), &norm(&t.purpose)))
        };
        if ok {
            hits.push(t);
        }
    }
    Ok(pick_twin(&hits))
}

/// Imported rows of amount `?2` within two days of `?4`. The BETWEEN bounds, a
/// day wider than the window, let `tx_amount_date` find the candidates; the
/// julianday test still decides. Card holds an older „Abrufweg umstellen“
/// marked (`mm-switch-pending-hold-…`) are never twins: the booking taking one over
/// would keep the hold's date and text (#1134).
const TWIN_CANDIDATES_SQL: &str =
    "SELECT id, counterparty, counterparty_iban, purpose, booking_date,
        IFNULL(category_id, 0), trim(IFNULL(note, '')),
        abs(julianday(booking_date) - julianday(?4))
   FROM transactions
  WHERE account_id = ?1 AND external_id LIKE 'mm-%' AND amount_cents = ?2
    AND external_id NOT LIKE 'mm-switch-pending-hold-%'
    AND deleted_ms IS NULL AND id != ?3
    AND booking_date BETWEEN date(?4, '-3 days') AND date(?4, '+3 days')
    AND abs(julianday(booking_date) - julianday(?4)) <= 2
  ORDER BY id";

/// A MoneyMoney row that may be the twin of a bank booking.
struct Twin {
    id: i64,
    name: String,
    iban: String,
    purpose: String,
    date: String,
    category: i64,
    note: String,
    /// distance of the booking dates in days
    days: i64,
}

/// The candidate to adopt: the only one, else the unique closest in date, else the
/// first of equally close candidates that are interchangeable ([`same_twin`]).
/// Candidates come ordered by id.
fn pick_twin(hits: &[Twin]) -> Option<i64> {
    let closest = hits.iter().map(|t| t.days).min()?;
    let best: Vec<&Twin> = hits.iter().filter(|t| t.days == closest).collect();
    let first = best[0];
    best.iter().all(|t| same_twin(first, t)).then_some(first.id)
}

/// Two MoneyMoney rows the bank booking cannot tell apart: same date, counterparty,
/// IBAN, category and note, and the same purpose apart from the SEPA references
/// MoneyMoney appends (`EREF: …`, `MREF: …`, `CRED: …`), which the bank may not send
/// for card payments. Rows that differ in content (e.g. "Brötchen" / "Kuchen") are not
/// twins: guessing would put the bank's id on the wrong one.
fn same_twin(a: &Twin, b: &Twin) -> bool {
    a.date == b.date
        && norm(&a.name) == norm(&b.name)
        && norm(&a.iban) == norm(&b.iban)
        && a.category == b.category
        && a.note == b.note
        && norm(&purpose_without_refs(&a.purpose)) == norm(&purpose_without_refs(&b.purpose))
}

/// Purpose without the `EREF: …` / `MREF: …` / `CRED: …` parts that
/// [`MmRow::stored_purpose`] appends.
fn purpose_without_refs(p: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut words = p.split_whitespace();
    while let Some(w) = words.next() {
        if matches!(w, "EREF:" | "MREF:" | "CRED:") {
            words.next();
        } else {
            out.push(w);
        }
    }
    out.join(" ")
}

/// Two normalised purposes that share a long common start (≥ 24 characters).
fn purposes_alike(a: &str, b: &str) -> bool {
    const MIN: usize = 24;
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count() >= MIN
}

/// Tax lines of the first account holder: the bank's short text and the full
/// names MoneyMoney shows for it (all normalised). The second holder's lines
/// (`Kap.ertr.st2`, `SoliZuschl.2`) keep their text there and match exactly.
const TAX_LINES: [(&str, &[&str]); 2] = [
    (
        "kapertrst1",
        &["kapitalertragsteuer", "kapitalertragssteuer"],
    ),
    ("solizuschl1", &["solidaritätszuschlag"]),
];

/// The bank's short tax text and a MoneyMoney row carrying the full name.
fn same_tax_line(bank_purpose: &str, mm_name: &str, mm_purpose: &str) -> bool {
    TAX_LINES.iter().any(|(short, full)| {
        bank_purpose == *short && (full.contains(&mm_name) || full.contains(&mm_purpose))
    })
}

/// An imported row with fingerprint `?2`. `import_fp != ''` is always true for a
/// fingerprint, but SQLite uses the partial index `tx_import_fp` only when the query
/// repeats its condition: a bound `import_fp = ?2` does not imply it.
const IMPORTED_BY_FP_SQL: &str = "SELECT id FROM transactions
     WHERE account_id = ?1 AND import_fp = ?2 AND import_fp != ''
       AND external_id LIKE 'mm-%' AND external_id NOT LIKE 'mm-switch-pending-hold-%'
       AND deleted_ms IS NULL LIMIT 1";

/// A FinTS/API booking arrives that was already imported from MoneyMoney:
/// take over the row (category + note stay) instead of inserting a duplicate.
/// Exact fingerprint first, then a unique fuzzy twin (see [`find_twin`]).
/// Returns true when an imported row was adopted.
pub(crate) fn adopt_import(
    conn: &Connection,
    account_id: i64,
    external_id: &str,
    date: &str,
    cents: i64,
    iban: &str,
    name: &str,
    purpose: &str,
    booking_text: &str,
) -> CoreResult<bool> {
    // store_fetch has checked this already; other callers rely on it here:
    // taking over another row would break the unique key.
    let taken = conn
        .prepare_cached(
            // includes soft-deleted rows: (account, external_id) is unique
            "SELECT 1 FROM transactions WHERE account_id = ?1 AND external_id = ?2",
        )?
        .exists(params![account_id, external_id])?;
    if taken {
        return Ok(false);
    }
    let fp = fingerprint(date, cents, iban, name, purpose);
    let mut row: Option<i64> = conn
        .prepare_cached(IMPORTED_BY_FP_SQL)?
        .query_row(params![account_id, fp], |r| r.get(0))
        .optional()?;
    if row.is_none() {
        row = find_twin(conn, account_id, None, date, cents, iban, name, purpose)?;
    }
    let Some(id) = row else { return Ok(false) };
    take_over(conn, id, external_id, iban, booking_text)?;
    Ok(true)
}

/// Gives the MoneyMoney row the bank's external id and fills missing bank metadata;
/// the MoneyMoney counterparty name (cleaner) is kept.
fn take_over(
    conn: &Connection,
    id: i64,
    external_id: &str,
    iban: &str,
    booking_text: &str,
) -> CoreResult<()> {
    conn.prepare_cached(
        "UPDATE transactions SET external_id = ?1,
                counterparty_iban = CASE WHEN counterparty_iban = '' THEN ?3 ELSE counterparty_iban END,
                booking_text = CASE WHEN booking_text = '' THEN ?4 ELSE booking_text END
          WHERE id = ?2",
    )?
    .execute(params![external_id, id, iban, booking_text])?;
    Ok(())
}

/// Re-sync repair: the bank booking `bank_id` is stored (old duplicate) while its
/// MoneyMoney twin is still un-adopted. If the bank row is untouched (no manual category,
/// note, check mark, splits or documents) it is removed and the rich MoneyMoney row takes
/// over its external id. Returns true when merged.
pub(crate) fn merge_synced_twin(
    conn: &Connection,
    bank_id: i64,
    date: &str,
    cents: i64,
    iban: &str,
    name: &str,
    purpose: &str,
) -> CoreResult<bool> {
    let bank: Option<(i64, String, String, String)> = conn
        .prepare_cached(
            "SELECT account_id, external_id, counterparty_iban, booking_text FROM transactions
              WHERE id = ?1
                AND deleted_ms IS NULL AND category_source != 'manual' AND checked = 0 AND trim(note) = ''
                AND NOT EXISTS (SELECT 1 FROM transaction_splits s WHERE s.transaction_id = transactions.id)
                AND NOT EXISTS (SELECT 1 FROM booking_documents d WHERE d.booking_id = transactions.id)",
        )?
        .query_row([bank_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .optional()?;
    let Some((account_id, external_id, bank_iban, bank_text)) = bank else {
        return Ok(false);
    };
    let Some(mm_id) = find_twin(
        conn,
        account_id,
        Some(bank_id),
        date,
        cents,
        iban,
        name,
        purpose,
    )?
    else {
        return Ok(false);
    };
    // The twin continues under the bank's external id; the deleted copy
    // keeps the twin's old MoneyMoney id as a tombstone, so another device
    // still holding that row deletes it on merge. (account, external_id) is
    // unique: the copy's id is moved aside while the two swap.
    let mm_ext: String = conn.query_row(
        "SELECT external_id FROM transactions WHERE id = ?1",
        [mm_id],
        |r| r.get(0),
    )?;
    conn.execute(
        "UPDATE transactions SET external_id = external_id || '#merged-' || id WHERE id = ?1",
        [bank_id],
    )?;
    take_over(conn, mm_id, &external_id, &bank_iban, &bank_text)?;
    conn.execute(
        "UPDATE transactions SET external_id = ?2 WHERE id = ?1",
        params![bank_id, mm_ext],
    )?;
    crate::sync_merge::soft_delete_booking(conn, bank_id)?;
    Ok(true)
}

/// Removes tax bookings that an earlier sync stored beside their MoneyMoney row
/// because the texts differ ([`TAX_LINES`]). They can be older than the sync
/// window, so the fetched bookings alone would not reach them.
pub(crate) fn merge_synced_tax_twins(conn: &Connection, account_id: i64) -> CoreResult<()> {
    let rows = conn
        .prepare_cached(
            "SELECT id, booking_date, amount_cents, counterparty_iban, counterparty, purpose
               FROM transactions
              WHERE account_id = ?1 AND deleted_ms IS NULL AND external_id NOT LIKE 'mm-%'
                AND purpose IN ('Kap.ertr.st1', 'SoliZuschl.1')",
        )?
        .query_map([account_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (id, date, cents, iban, name, purpose) in rows {
        merge_synced_twin(conn, id, &date, cents, &iban, &name, &purpose)?;
    }
    Ok(())
}

fn sample(r: &MmRow, i: i64, account_id: i64) -> Booking {
    let mut b = Booking {
        id: -(i + 1),
        account_id,
        account_name: String::new(),
        booking_date: r.booking_date.clone(),
        value_date: r.value_date.clone(),
        amount_cents: r.amount_cents,
        currency: r.currency.clone(),
        counterparty: r.name.clone(),
        counterparty_iban: r.iban.clone(),
        purpose: r.stored_purpose(),
        booking_text: r.booking_text.clone(),
        category_id: None,
        category_name: r.category_path().last().cloned().unwrap_or_default(),
        category_path: r.category_path().join(" › "),
        category_color_hex: String::new(),
        category_symbol: String::new(),
        category_source: if r.category_path().is_empty() {
            CategorySource::None
        } else {
            CategorySource::Manual
        },
        rule_id: None,
        checked: true,
        merchant: MerchantResolver::placeholder(),
        merchant_name: String::new(),
        merchant_domain: None,
        merchant_svg_path: None,
        merchant_hex_color: String::new(),
        payment_method: String::new(),
        flow: FlowKind::Consumption,
        clean_purpose: String::new(),
        account_kind: AccountKind::Checking,
        document_count: 0,
        note: r.note.clone(),
        purpose_code: String::new(),
        gvc: String::new(),
        salary_flag: None,
        is_salary: false,
        recurrence_key: String::new(),
        is_pending: false,
        item_refs: String::new(),
        end_to_end_id: r.end_to_end.clone(),
        mandate_id: r.mandate_ref.clone(),
        creditor_id: r.creditor_id.clone(),
        external_id: String::new(),
        tags: Vec::new(),
    };
    // preview of an import: no connection yet, a payment service reads as before
    let paypal = b.account_kind == AccountKind::PaymentService;
    // Preview names are imported user text, never built-in category roles.
    enrich(&mut b, paypal, "");
    b.is_salary = crate::salary::salary_signal(&b);
    b
}

#[uniffi::export]
impl Vault {
    /// Dry run: parses a MoneyMoney CSV/XML export and reports what an import
    /// into `target_account_id` (`None` = new archive account) would do.
    /// Nothing is stored.
    pub fn preview_moneymoney_import(
        &self,
        data: Vec<u8>,
        file_name: String,
        target_account_id: Option<i64>,
    ) -> Result<MoneyMoneyImportPreview, CoreError> {
        let rows = parse_export(&data)?;
        let db = self.db()?;
        let tx = db.unchecked_transaction()?;
        // before the dry run, which may add a provisional archive account
        let suggested_account_id = suggest_account(&tx, &decode(&data), &rows, &file_name)?;
        let st = run(&tx, &rows, &file_name, target_account_id, true, true)?;
        let mut cats: Vec<String> = rows
            .iter()
            .map(|r| r.category_path().join(" › "))
            .filter(|c| !c.is_empty())
            .collect();
        cats.sort();
        cats.dedup();
        let preview = MoneyMoneyImportPreview {
            total_rows: st.total,
            new_bookings: st.new,
            duplicates_skipped_or_enriched: st.duplicates,
            notes_count: rows.iter().filter(|r| !r.note.is_empty()).count() as u32,
            date_range_start: rows.iter().map(|r| r.booking_date.clone()).min(),
            date_range_end: rows.iter().map(|r| r.booking_date.clone()).max(),
            detected_categories: cats,
            sample_bookings: rows
                .iter()
                .take(6)
                .enumerate()
                .map(|(i, r)| sample(r, i as i64, target_account_id.unwrap_or(0)))
                .collect(),
            suggested_account_name: account_name_from_file(&file_name),
            suggested_account_id,
        };
        drop(tx); // rollback: this was only a dry run
        Ok(preview)
    }

    /// Imports a MoneyMoney export. Idempotent (see module docs).
    pub fn import_moneymoney_data(
        &self,
        data: Vec<u8>,
        file_name: String,
        target_account_id: Option<i64>,
        preserve_categories: bool,
        preserve_notes: bool,
    ) -> Result<MoneyMoneyImportResult, CoreError> {
        let rows = parse_export(&data)?;
        let db = self.db()?;
        let tx = db.unchecked_transaction()?;
        let st = run(
            &tx,
            &rows,
            &file_name,
            target_account_id,
            preserve_categories,
            preserve_notes,
        )?;
        tx.commit()?;
        // Fold the import into the main file; best effort, it is committed.
        let _ = db.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
        Ok(MoneyMoneyImportResult {
            account_id: st.account_id,
            account_created: st.account_created,
            total_rows: st.total,
            imported: st.new,
            duplicates: st.duplicates,
            enriched: st.enriched,
            notes_imported: st.notes,
            categories_created: st.categories_created,
            balance_cents: st.balance_cents,
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn fingerprint_never_changes() {
        // Stored import keys are built from it.
        assert_eq!(
            super::fingerprint("2026-11-01", -350, "", "Bäckerei Müller", "Einkauf"),
            "79b1d135e9d7e169"
        );
    }

    use super::*;
    use std::sync::Arc;

    /// Keep the returned guard alive for the whole test (`let (_dir, v) = vault();`).
    fn vault() -> (crate::test_support::TempDir, Arc<Vault>) {
        let dir = crate::test_support::TempDir::new("mm");
        let path = dir.join("v.db");
        let v = Vault::open(path.to_string_lossy().to_string(), vec![5u8; 32]).unwrap();
        (dir, v)
    }

    const CSV: &str = "\"Datum\";\"Wertstellung\";\"Kategorie\";\"Name\";\"Verwendungszweck\";\"Konto\";\"Bank\";\"Betrag\";\"Währung\";\"Notiz\"\n\
\"15.03.2012\";\"15.03.2012\";\"Haushalt\\Supermarkt\";\"REWE Markt\";\"Einkauf Kartenzahlung\";\"DE02120300000000202051\";\"BYLADEM1001\";\"-1.234,56\";\"EUR\";\"Wocheneinkauf\"\n\
\"01.04.2026\";\"01.04.2026\";\"Steuern / Büro\";\"Finanzamt\";\"Erstattung\";\"\";\"\";\"2.000,00\";\"EUR\";\"\"\n";

    // Synthetic booking rows using the headers supplied in #705. The attached
    // category-only sample has no actual dates/amounts and must not create bookings.
    const C24: &str = "Transaktionstyp;Buchungsdatum;Karteneinsatz;Betrag;Zahlungsempfänger;IBAN;BIC;Verwendungszweck;Beschreibung;Kontonummer;Kontoname;Kategorie;Unterkategorie;Bargeldabhebung\n\
Kartenzahlung;01.01.2030;;-5,00;Bäcker;;;Brötchen;Kartenzahlung;123456;Pocket;Essen;Backwaren;Nein\n\
Überweisung;02.01.2030;;100,00;Alice;DE02120300000000202051;;Erstattung;;123456;Pocket;Freizeit & Unterhaltung;Streaming & Pay TV;Nein\n";

    #[test]
    fn c24_reporter_fixture_preserves_all_category_pairs() {
        let data = include_bytes!("../../tests/fixtures/c24_categories.csv");
        // Category-only rows must never create zero-value / phantom bookings.
        assert!(parse_export(data).is_err());
        let text = decode(data);
        let mut records = split_csv(&text, ',');
        assert_eq!(records.len(), 66);
        for (i, record) in records.iter_mut().skip(1).enumerate() {
            record[1] = "17.11.2026".into();
            record[3] = format!("-{},00", i + 1);
            record[4] = "Testzahlung".into();
        }
        let csv = records
            .iter()
            .map(|row| {
                row.iter()
                    .map(|cell| format!("\"{}\"", cell.replace('"', "\"\"")))
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .collect::<Vec<_>>()
            .join("\n");
        let rows = parse_export(csv.as_bytes()).unwrap();
        assert_eq!(rows.len(), 65);
        for (row, record) in rows.iter().zip(records.iter().skip(1)) {
            assert_eq!(
                row.category_path(),
                vec![record[11].clone(), record[12].clone()]
            );
        }
        let (_dir, v) = vault();
        let result = v
            .import_moneymoney_data(csv.as_bytes().to_vec(), "c24.csv".into(), None, true, true)
            .unwrap();
        assert_eq!(result.imported, 65);
        let again = v
            .import_moneymoney_data(
                csv.as_bytes().to_vec(),
                "c24.csv".into(),
                Some(result.account_id),
                true,
                true,
            )
            .unwrap();
        assert_eq!((again.imported, again.duplicates), (0, 65));
        let without_categories = v
            .import_moneymoney_data(
                csv.as_bytes().to_vec(),
                "plain.csv".into(),
                None,
                false,
                false,
            )
            .unwrap();
        assert_eq!(without_categories.categories_created, 0);
        let page = v
            .list_transactions(BookingFilter {
                account_ids: vec![without_categories.account_id],
                ..Default::default()
            })
            .unwrap();
        assert!(page.transactions.iter().all(|b| b.category_id.is_none()));
    }

    #[test]
    fn c24_columns_preserve_category_components_and_counterparty_iban() {
        let rows = parse_export(C24.as_bytes()).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].category_path(), vec!["Essen", "Backwaren"]);
        assert_eq!(rows[0].iban, "");
        assert_eq!(rows[0].amount_cents, -500);
        assert_eq!(rows[0].booking_text, "Kartenzahlung");
        assert_eq!(rows[1].iban, "DE02120300000000202051");
        assert_eq!(rows[1].amount_cents, 10000);
        let without_child = C24.replace("Essen;Backwaren", "Essen;");
        assert_eq!(
            parse_export(without_child.as_bytes()).unwrap()[0].category_path(),
            vec!["Essen"]
        );
        assert_eq!(
            rows[1].category_path(),
            vec!["Freizeit & Unterhaltung", "Streaming & Pay TV"]
        );
        let literal = C24.replace("Essen;Backwaren", "Essen / Trinken;Backwaren / Café");
        assert_eq!(
            parse_export(literal.as_bytes()).unwrap()[0].category_path(),
            vec!["Essen / Trinken", "Backwaren / Café"]
        );
        let only_categories = "Transaktionstyp;Buchungsdatum;Betrag;Kategorie;Unterkategorie\n;;;Freizeit;Streaming\n";
        assert!(parse_export(only_categories.as_bytes()).is_err());
    }

    #[test]
    fn c24_header_mapping_handles_column_order_and_short_rows() {
        // Source account comes before the (empty) counterparty IBAN. A short
        // row has no description and must fall back to the transaction type.
        let csv = "Kontonummer;IBAN;Transaktionstyp;Betrag;Buchungsdatum;Kategorie;Unterkategorie;Beschreibung\n\
123456;;Kartenzahlung;-5,00;01.01.2030;Essen;Backwaren;Beim Bäcker\n\
123456;;Überweisung;10,00;02.01.2030;Einkommen;Erstattung\n";
        let rows = parse_export(csv.as_bytes()).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.iban.is_empty()));
        assert_eq!(rows[0].booking_text, "Beim Bäcker");
        assert_eq!(rows[1].booking_text, "Überweisung");
        assert_eq!(rows[1].category_path(), vec!["Einkommen", "Erstattung"]);
    }

    #[test]
    fn c24_enriches_api_booking_and_reimport_keeps_manual_category() {
        let (_dir, v) = vault();
        let account = v
            .import_moneymoney_data(
                CSV.as_bytes().to_vec(),
                "account.csv".into(),
                None,
                true,
                true,
            )
            .unwrap()
            .account_id;
        {
            let db = v.db().unwrap();
            db.execute("INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, counterparty, purpose)
                VALUES (?1, 'enable-banking-1', '2030-01-01', -500, 'Bäcker', 'Brötchen')", [account]).unwrap();
        }
        let preview = v
            .preview_moneymoney_import(C24.as_bytes().to_vec(), "c24.csv".into(), Some(account))
            .unwrap();
        assert_eq!(
            (preview.new_bookings, preview.duplicates_skipped_or_enriched),
            (1, 1)
        );
        assert!(
            preview
                .detected_categories
                .contains(&"Essen › Backwaren".into())
        );
        assert_eq!(
            preview.sample_bookings[0].category_path,
            "Essen › Backwaren"
        );
        let result = v
            .import_moneymoney_data(
                C24.as_bytes().to_vec(),
                "c24.csv".into(),
                Some(account),
                true,
                true,
            )
            .unwrap();
        assert_eq!(
            (result.imported, result.duplicates, result.enriched),
            (1, 1, 1)
        );
        let changed = C24.replace("Essen;Backwaren", "Andere;Kategorie");
        let result = v
            .import_moneymoney_data(
                changed.as_bytes().to_vec(),
                "c24.csv".into(),
                Some(account),
                true,
                true,
            )
            .unwrap();
        assert_eq!(
            (result.imported, result.duplicates, result.enriched),
            (0, 2, 0)
        );
        let page = v
            .list_transactions(BookingFilter {
                account_ids: vec![account],
                search: Some("Bäcker".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(page.total_count, 1);
        assert_eq!(page.transactions[0].category_path, "Essen › Backwaren");
    }

    const XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<transactions>
  <transaction>
    <bookingDate>2020-05-04</bookingDate><valueDate>2020-05-05</valueDate>
    <amount>-1234.56</amount><currency>EUR</currency>
    <name>Strom &amp; Gas AG</name><purpose>Abschlag Mai</purpose>
    <accountNumber>DE02120300000000202051</accountNumber><bankCode>BYLADEM1001</bankCode>
    <category>Wohnen\Energie</category><comment>Jahresabrechnung prüfen</comment>
    <bookingText>LASTSCHRIFT</bookingText><endToEndReference>E2E1</endToEndReference>
    <mandateReference>M1</mandateReference><creditorId>DE98ZZZ09999999999</creditorId>
    <checkmark>1</checkmark>
  </transaction>
</transactions>"#;

    #[test]
    fn csv_german_headers_amounts_categories_notes() {
        let rows = parse_export(CSV.as_bytes()).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].amount_cents, -123_456);
        assert_eq!(rows[0].booking_date, "2012-03-15");
        assert_eq!(rows[0].note, "Wocheneinkauf");
        assert_eq!(
            split_category(&rows[0].category),
            vec!["Haushalt", "Supermarkt"]
        );
        assert_eq!(split_category(&rows[1].category), vec!["Steuern", "Büro"]);
        assert_eq!(rows[1].amount_cents, 200_000);
        // other delimiters / English headers
        let en = "Date,Value date,Category,Name,Purpose,Account,Amount,Currency,Note\n2024-01-02,2024-01-02,Food,Shop,Milk,,-1234.56,EUR,hi\n";
        let r = parse_export(en.as_bytes()).unwrap();
        assert_eq!((r[0].amount_cents, r[0].note.as_str()), (-123_456, "hi"));
        let tab = "Buchungstag\tBetrag\tName\n03.02.2023\t-5,00\tX\n";
        assert_eq!(parse_export(tab.as_bytes()).unwrap()[0].amount_cents, -500);
    }

    #[test]
    fn split_category_handles_all_separators() {
        for raw in [
            "Haushalt\\Lebensmittel",
            "Haushalt/Lebensmittel",
            "Haushalt › Lebensmittel",
            "Haushalt : Lebensmittel",
            "Haushalt -> Lebensmittel",
        ] {
            assert_eq!(
                split_category(raw),
                vec!["Haushalt", "Lebensmittel"],
                "{raw}"
            );
        }
        assert_eq!(
            split_category("Finanzen\\Investments\\Kauf"),
            vec!["Finanzen", "Investments", "Kauf"]
        );
    }

    #[test]
    fn subcategories_get_parent_ids() {
        let (_dir, v) = vault();
        let conn = v.db().unwrap();
        let (mut cache, mut created) = (HashMap::new(), 0u32);
        let leaf = find_or_create_category(
            &conn,
            &split_category("Finanzen\\Investments\\Kauf"),
            &mut cache,
            &mut created,
        )
        .unwrap();
        let again = find_or_create_category(
            &conn,
            &split_category("Haushalt\\Lebensmittel"),
            &mut cache,
            &mut created,
        )
        .unwrap();
        let parent_name = |id: i64| -> (String, Option<i64>) {
            conn.query_row(
                "SELECT name, parent_id FROM categories WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
        };
        let (n, p) = parent_name(leaf);
        assert_eq!(n, "Kauf");
        let (n, p) = parent_name(p.unwrap());
        assert_eq!(n, "Investments");
        let (n, p) = parent_name(p.unwrap());
        assert_eq!((n.as_str(), p), ("Finanzen", None));
        let (n, p) = parent_name(again);
        assert_eq!(n, "Lebensmittel");
        assert_eq!(parent_name(p.unwrap()).0, "Haushalt");
    }

    #[test]
    fn xml_export_parses_all_fields() {
        let rows = parse_export(XML.as_bytes()).unwrap();
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.amount_cents, -123_456);
        assert_eq!(r.name, "Strom & Gas AG");
        assert_eq!(r.category, "Wohnen\\Energie");
        assert_eq!(r.note, "Jahresabrechnung prüfen");
        assert!(r.checkmark);
        assert_eq!(r.end_to_end, "E2E1");
        assert!(r.stored_purpose().contains("MREF: M1"));
    }

    #[test]
    fn import_creates_archive_account_categories_and_notes() {
        let (_dir, v) = vault();
        let res = v
            .import_moneymoney_data(
                CSV.as_bytes().to_vec(),
                "Giro alt.csv".into(),
                None,
                true,
                true,
            )
            .unwrap();
        assert!(res.account_created);
        assert_eq!(
            (res.imported, res.duplicates, res.notes_imported),
            (2, 0, 1)
        );
        assert_eq!(res.balance_cents, -123_456 + 200_000);
        let page = v
            .list_transactions(BookingFilter {
                account_ids: vec![res.account_id],
                ..Default::default()
            })
            .unwrap();
        assert_eq!(page.total_count, 2);
        let b = page
            .transactions
            .iter()
            .find(|b| b.counterparty == "REWE Markt")
            .unwrap();
        assert_eq!(b.category_path, "Haushalt › Supermarkt");
        assert_eq!(b.note, "Wocheneinkauf");
        let acc = v
            .list_accounts()
            .unwrap()
            .into_iter()
            .find(|a| a.id == res.account_id)
            .unwrap();
        assert_eq!(acc.balance_cents, 76_544);
    }

    /// MoneyMoney's exports append the SEPA references to the purpose, the
    /// database keeps them apart: an export over a database import must not
    /// duplicate (seen with a real 1,522-booking account). Still a multiset.
    #[test]
    fn export_with_sepa_references_matches_bookings_without_them() {
        let (_dir, v) = vault();
        let head = "\"Datum\";\"Wertstellung\";\"Kategorie\";\"Name\";\"Verwendungszweck\";\"Konto\";\"Bank\";\"Betrag\";\"Währung\"\n";
        let row = |purpose: &str| {
            format!(
                "\"02.01.2026\";\"\";\"\";\"Stadtwerke\";\"{purpose}\";\"DE02120300000000202051\";\"\";\"-50,00\";\"EUR\"\n"
            )
        };
        // as from the database: two payments, references not in the purpose
        let db = format!(
            "{head}{}{}",
            row("Abschlag Januar"),
            row("Abschlag Nachzahlung")
        );
        let a = v
            .import_moneymoney_data(db.into_bytes(), "Giro.csv".into(), None, true, true)
            .unwrap();
        assert_eq!(a.imported, 2);
        // the export: references appended, plus one booking more
        let export = format!(
            "{head}{}{}{}",
            row("Abschlag Januar, Mandatsref: M-1, Gläubiger: DE98ZZZ09999999999"),
            row("Abschlag Nachzahlung, Mandatsref: M-1, Gläubiger: DE98ZZZ09999999999"),
            row("Abschlag Februar, Mandatsref: M-1, Gläubiger: DE98ZZZ09999999999"),
        );
        let p = v
            .preview_moneymoney_import(export.into_bytes(), "Giro.csv".into(), Some(a.account_id))
            .unwrap();
        assert_eq!((p.new_bookings, p.duplicates_skipped_or_enriched), (1, 2));

        // own transfer: the database import filled in the IBAN, the export has none
        let own = |iban: &str| {
            format!(
                "{head}\"05.01.2026\";\"\";\"\";\"Tagesgeld\";\"Umbuchung\";\"{iban}\";\"\";\"-100,00\";\"EUR\"\n"
            )
        };
        v.import_moneymoney_data(
            own("DE89370400440532013000").into_bytes(),
            "Giro.csv".into(),
            Some(a.account_id),
            true,
            true,
        )
        .unwrap();
        let p = v
            .preview_moneymoney_import(own("").into_bytes(), "Giro.csv".into(), Some(a.account_id))
            .unwrap();
        assert_eq!((p.new_bookings, p.duplicates_skipped_or_enriched), (0, 1));
    }

    #[test]
    fn reimport_creates_no_duplicates_and_keeps_identical_rows() {
        let (_dir, v) = vault();
        let twin = format!(
            "{CSV}\"15.03.2012\";\"\";\"Haushalt\\Supermarkt\";\"REWE Markt\";\"Einkauf Kartenzahlung\";\"DE02120300000000202051\";\"\";\"-1.234,56\";\"EUR\";\"\"\n"
        );
        let a = v
            .import_moneymoney_data(twin.as_bytes().to_vec(), "x.csv".into(), None, true, true)
            .unwrap();
        assert_eq!(a.imported, 3); // two genuinely identical bookings both kept
        let b = v
            .import_moneymoney_data(
                twin.as_bytes().to_vec(),
                "x.csv".into(),
                Some(a.account_id),
                true,
                true,
            )
            .unwrap();
        assert_eq!((b.imported, b.duplicates), (0, 3));
        let p = v
            .preview_moneymoney_import(twin.as_bytes().to_vec(), "x.csv".into(), Some(a.account_id))
            .unwrap();
        assert_eq!((p.new_bookings, p.duplicates_skipped_or_enriched), (0, 3));
        assert_eq!(p.date_range_start.as_deref(), Some("2012-03-15"));
        assert_eq!(p.date_range_end.as_deref(), Some("2026-04-01"));
    }

    /// A dropped card hold carries date, amount and text of the payment: the
    /// posted booking from an export must still be imported. A booking the
    /// user deleted is still not imported again (#1134).
    #[test]
    fn dropped_holds_do_not_hide_the_posted_booking() {
        let (_dir, v) = vault();
        let a = v
            .import_moneymoney_data(CSV.as_bytes().to_vec(), "a.csv".into(), None, true, true)
            .unwrap();
        {
            let db = v.db().unwrap();
            for (ext, date, cents, name, purpose) in [
                // settled hold
                (
                    "pending-hold-fints-h1",
                    "2030-01-01",
                    -500,
                    "Bäcker",
                    "Brötchen",
                ),
                // dropped by „Abrufweg umstellen“
                (
                    "switch-stale-7:pending-hold-comdirect-h2",
                    "2030-01-03",
                    -700,
                    "Laden",
                    "Einkauf",
                ),
                // marked by an older switch, dropped on open
                (
                    "mm-switch-pending-hold-fints-h3",
                    "2030-01-04",
                    -900,
                    "Kino",
                    "Karte",
                ),
                // deleted by the user
                ("fints-9", "2030-01-02", -300, "Kiosk", "Zeitung"),
            ] {
                db.execute(
                    "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, counterparty,
                                              counterparty_iban, purpose, deleted_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, '', ?6, 1)",
                    params![a.account_id, ext, date, cents, name, purpose],
                )
                .unwrap();
            }
        }
        let csv = "Datum;Name;Verwendungszweck;Betrag\n\
                   01.01.2030;Bäcker;Brötchen;-5,00\n\
                   02.01.2030;Kiosk;Zeitung;-3,00\n\
                   03.01.2030;Laden;Einkauf;-7,00\n\
                   04.01.2030;Kino;Karte;-9,00\n";
        let r = v
            .import_moneymoney_data(
                csv.as_bytes().to_vec(),
                "b.csv".into(),
                Some(a.account_id),
                true,
                true,
            )
            .unwrap();
        assert_eq!((r.imported, r.duplicates), (3, 1));
    }

    #[test]
    fn overlap_with_fints_booking_enriches_and_fints_adopts_import() {
        let (_dir, v) = vault();
        let a = v
            .import_moneymoney_data(CSV.as_bytes().to_vec(), "a.csv".into(), None, true, true)
            .unwrap();
        // Existing FinTS booking (same content, no category / note).
        {
            let db = v.db().unwrap();
            db.execute(
                "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, counterparty, counterparty_iban, purpose)
                 VALUES (?1, 'fints-1', '2030-01-01', -500, 'Bäcker', '', 'Brötchen')",
                [a.account_id],
            )
            .unwrap();
        }
        let csv = "Datum;Name;Verwendungszweck;Betrag;Kategorie;Notiz\n01.01.2030;Bäcker;Brötchen;-5,00;Essen\\Backwaren;lecker\n";
        let r = v
            .import_moneymoney_data(
                csv.as_bytes().to_vec(),
                "b.csv".into(),
                Some(a.account_id),
                true,
                true,
            )
            .unwrap();
        assert_eq!(
            (r.imported, r.duplicates, r.enriched, r.notes_imported),
            (0, 1, 1, 1)
        );
        let page = v
            .list_transactions(BookingFilter {
                account_ids: vec![a.account_id],
                search: Some("Bäcker".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(page.total_count, 1);
        assert_eq!(page.transactions[0].category_path, "Essen › Backwaren");
        assert_eq!(page.transactions[0].note, "lecker");

        // A later FinTS sync delivers the REWE booking (new external id): adopted, not duplicated.
        let db = v.db().unwrap();
        let adopted = adopt_import(
            &db,
            a.account_id,
            "fints-2",
            "2012-03-15",
            -123_456,
            "DE02120300000000202051",
            "REWE Markt",
            "Einkauf Kartenzahlung",
            "",
        )
        .unwrap();
        assert!(adopted);
        let n: i64 = db
            .query_row(
                "SELECT count(*) FROM transactions WHERE account_id = ?1",
                [a.account_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 3);
    }

    fn mm_acct(v: &Vault, rows: &str) -> i64 {
        let csv = format!("Datum;Name;Verwendungszweck;Betrag;Kategorie;Notiz\n{rows}");
        v.import_moneymoney_data(csv.into_bytes(), "t.csv".into(), None, true, true)
            .unwrap()
            .account_id
    }

    fn ext_ids(db: &Connection, acc: i64) -> Vec<String> {
        let mut st = db
            .prepare("SELECT external_id FROM transactions WHERE account_id = ?1 ORDER BY id")
            .unwrap();
        st.query_map([acc], |r| r.get(0))
            .unwrap()
            .map(|x| x.unwrap())
            .collect()
    }

    #[test]
    fn adopt_fallback_on_differing_purpose_and_comdirect_name_in_purpose() {
        let (_dir, v) = vault();
        let acc = mm_acct(
            &v,
            "10.05.2026;REWE Markt;Einkauf;-12,50;Essen;n1\n11.05.2026;Stadtwerke Köln;Strom;-80,00;Wohnen;n2\n",
        );
        let db = v.db().unwrap();
        // purpose differs (SEPA tags), date off by one day, name matches
        assert!(
            adopt_import(
                &db,
                acc,
                "f1",
                "2026-05-11",
                -1250,
                "",
                "REWE",
                "EREF+123 Einkauf",
                ""
            )
            .unwrap()
        );
        // comdirect: partner field holds booking text, MM name sits in the purpose
        assert!(
            adopt_import(
                &db,
                acc,
                "f2",
                "2026-05-10",
                -8000,
                "",
                "Lastschrift",
                "Stadtwerke Köln MREF+9 Strom",
                "LASTSCHRIFT"
            )
            .unwrap()
        );
        assert_eq!(ext_ids(&db, acc), vec!["f1", "f2"]);
        let cp: String = db
            .query_row(
                "SELECT counterparty FROM transactions WHERE external_id='f2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cp, "Stadtwerke Köln");
        // out of the ±2 day window: no adoption
        assert!(!adopt_import(&db, acc, "f3", "2026-06-01", -1250, "", "REWE", "x", "").unwrap());
    }

    #[test]
    fn fingerprint_lookup_uses_the_partial_index() {
        let (_dir, v) = vault();
        let p =
            crate::test_support::query_plan(&v.db().unwrap(), IMPORTED_BY_FP_SQL, params![1, "x"]);
        assert!(
            p.contains("INDEX tx_import_fp (account_id=? AND import_fp=?)"),
            "{p}"
        );
    }

    #[test]
    fn twin_search_uses_the_amount_date_index() {
        let (_dir, v) = vault();
        let p = crate::test_support::query_plan(
            &v.db().unwrap(),
            TWIN_CANDIDATES_SQL,
            params![1, -1250, -1, "2026-05-10"],
        );
        assert!(
            p.contains(
                "INDEX tx_amount_date (amount_cents=? AND booking_date>? AND booking_date<?)"
            ),
            "{p}"
        );
    }

    #[test]
    fn adopt_import_matches_by_purpose_when_bank_sends_no_counterparty() {
        let (_dir, v) = vault();
        let acc = mm_acct(
            &v,
            "10.11.2026;Easybell GmbH;easybell.de Kundennummer 4077068 Rechnungsnummer R2026003619640 MREF: MND-2841562-1 CRED: DE74ZZZ00000150248;-1,80;Telekom;n1\n",
        );
        let db = v.db().unwrap();
        // comdirect REST: no creditor name, the purpose carries the text
        assert!(adopt_import(
            &db,
            acc,
            "c1",
            "2026-11-10",
            -180,
            "",
            "",
            "easybell.de Kundennummer 4077068 Rechnungsnummer R2026003619640 End-to-End-Ref.: R2026003619640 CORE / Mandatsref.: MND-2841562-1",
            "Buchung"
        )
        .unwrap());
        assert_eq!(ext_ids(&db, acc), vec!["c1"]);
        // a different short purpose with the same amount is not adopted
        assert!(
            !adopt_import(
                &db,
                acc,
                "c2",
                "2026-11-10",
                -180,
                "",
                "",
                "Kartenzahlung",
                "Buchung"
            )
            .unwrap()
        );
    }

    /// comdirect's interest statement as FinTS delivers it: no counterparty,
    /// the first line at the start of the purpose.
    const BANK_INTEREST: &str = "Abschluss Zinsen Kto 1234567EUR von 30.06.2026 bis 30.09.2026 Habenzinsen 1,000% ab 30.06.     12,34  EUR Abschlussrechnung     45,67  EUR";
    /// The same booking in MoneyMoney: first line as name, EREF appended.
    const MM_INTEREST: &str = "30.09.2026;Abschluss Zinsen Kto 1234567EUR;von 30.06.2026 bis 30.09.2026 Habenzinsen 1,000% ab 30.06. 12,34 EUR Abschlussrechnung 45,67 EUR EREF: ABC123TEST/0001;45,67;Zinsen;\n";

    fn bank_row(db: &Connection, acc: i64, ext: &str, date: &str, cents: i64, purpose: &str) {
        db.execute(
            "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, counterparty, purpose)
             VALUES (?1, ?2, ?3, ?4, '', ?5)",
            params![acc, ext, date, cents, purpose],
        )
        .unwrap();
    }

    fn live(db: &Connection, acc: i64) -> Vec<(String, Option<i64>)> {
        let mut st = db
            .prepare("SELECT external_id, category_id FROM transactions WHERE account_id = ?1 AND deleted_ms IS NULL ORDER BY id")
            .unwrap();
        st.query_map([acc], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|x| x.unwrap())
            .collect()
    }

    #[test]
    fn import_takes_a_fetched_booking_without_counterparty_as_the_same() {
        let (_dir, v) = vault();
        let acc = mm_acct(&v, "01.09.2026;Arbeitgeber;Gehalt;100,00;;\n");
        bank_row(
            &v.db().unwrap(),
            acc,
            "fints-1",
            "2026-09-30",
            4567,
            BANK_INTEREST,
        );
        let csv = format!("Datum;Name;Verwendungszweck;Betrag;Kategorie;Notiz\n{MM_INTEREST}");
        v.import_moneymoney_data(csv.into_bytes(), "t.csv".into(), Some(acc), true, true)
            .unwrap();
        let db = v.db().unwrap();
        let rows = live(&db, acc);
        assert_eq!(rows.len(), 2, "{rows:?}");
        // the bank's booking stays and gets MoneyMoney's category
        assert_eq!(rows[1].0, "fints-1");
        assert!(rows[1].1.is_some(), "{rows:?}");
    }

    #[test]
    fn import_does_not_guess_between_two_bank_bookings_without_counterparty() {
        let (_dir, v) = vault();
        let acc = mm_acct(&v, "01.09.2026;Arbeitgeber;Gehalt;100,00;;\n");
        {
            let db = v.db().unwrap();
            bank_row(&db, acc, "fints-1", "2026-09-30", 4567, BANK_INTEREST);
            bank_row(&db, acc, "fints-2", "2026-10-01", 4567, BANK_INTEREST);
        }
        let csv = format!("Datum;Name;Verwendungszweck;Betrag;Kategorie;Notiz\n{MM_INTEREST}");
        v.import_moneymoney_data(csv.into_bytes(), "t.csv".into(), Some(acc), true, true)
            .unwrap();
        assert_eq!(live(&v.db().unwrap(), acc).len(), 4);
    }

    #[test]
    fn import_keeps_a_different_booking_of_the_same_amount() {
        let (_dir, v) = vault();
        let acc = mm_acct(&v, "01.09.2026;Arbeitgeber;Gehalt;100,00;;\n");
        bank_row(
            &v.db().unwrap(),
            acc,
            "fints-1",
            "2026-09-30",
            4567,
            "Kartenzahlung Elektromarkt",
        );
        let csv = format!("Datum;Name;Verwendungszweck;Betrag;Kategorie;Notiz\n{MM_INTEREST}");
        v.import_moneymoney_data(csv.into_bytes(), "t.csv".into(), Some(acc), true, true)
            .unwrap();
        assert_eq!(live(&v.db().unwrap(), acc).len(), 3);
    }

    #[test]
    fn fold_drops_accents_only() {
        assert_eq!(fold("Hannah Müller"), fold("Hannah Muller"));
        assert_eq!(fold("Weiß-Straße"), "weissstrasse");
        assert_eq!(fold("Café Noël"), "cafenoel");
        assert_eq!(fold("Dvořák"), fold("Dvorak"));
        assert_ne!(fold("Müller"), fold("Maier"));
        // fingerprints keep the accents: stored import keys stay valid
        assert_ne!(
            fingerprint("2026-03-23", 100, "", "Müller", ""),
            fingerprint("2026-03-23", 100, "", "Muller", "")
        );
    }

    /// The bank's credit as FinTS delivers it: accented name, long purpose.
    fn bank_credit(db: &Connection, acc: i64, iban: &str, name: &str) {
        db.execute(
            "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, counterparty, counterparty_iban, purpose)
             VALUES (?1, 'fints-1', '2026-03-23', 120000, ?2, ?3,
                     'Fahrrad X3 Hannah Müller Fahrrad X3 End-to-End-Ref.: NOTPROVIDED Kundenreferenz: TEST0001')",
            params![acc, name, iban],
        )
        .unwrap();
    }

    #[test]
    fn import_matches_a_fetched_booking_whose_name_differs_by_accents() {
        let (_dir, v) = vault();
        let acc = mm_acct(&v, "01.03.2026;Arbeitgeber;Gehalt;100,00;;\n");
        bank_credit(
            &v.db().unwrap(),
            acc,
            "DE02120300000000202051",
            "Hannah Müller",
        );
        // MoneyMoney's export: name without accents, no IBAN column
        let csv = "Datum;Name;Verwendungszweck;Betrag;Kategorie;Notiz\n23.03.2026;Hannah Muller;Fahrrad X3;1200,00;Einzahlung;\n";
        v.import_moneymoney_data(
            csv.as_bytes().to_vec(),
            "t.csv".into(),
            Some(acc),
            true,
            true,
        )
        .unwrap();
        let rows = live(&v.db().unwrap(), acc);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(rows[1].0, "fints-1");
        assert!(rows[1].1.is_some(), "category carried over: {rows:?}");
    }

    #[test]
    fn import_keeps_a_different_name_apart() {
        let (_dir, v) = vault();
        let acc = mm_acct(&v, "01.03.2026;Arbeitgeber;Gehalt;100,00;;\n");
        bank_credit(&v.db().unwrap(), acc, "", "Hannah Maier");
        let csv = "Datum;Name;Verwendungszweck;Betrag;Kategorie;Notiz\n23.03.2026;Hannah Muller;Fahrrad X3;1200,00;Einzahlung;\n";
        v.import_moneymoney_data(
            csv.as_bytes().to_vec(),
            "t.csv".into(),
            Some(acc),
            true,
            true,
        )
        .unwrap();
        assert_eq!(live(&v.db().unwrap(), acc).len(), 3);
    }

    #[test]
    fn fetch_adopts_an_imported_row_whose_name_differs_by_accents() {
        let (_dir, v) = vault();
        let acc = mm_acct(
            &v,
            "23.03.2026;Hannah Muller;Fahrrad X3;1200,00;Einzahlung;\n",
        );
        let db = v.db().unwrap();
        assert!(
            adopt_import(
                &db,
                acc,
                "fints-1",
                "2026-03-23",
                120000,
                "",
                "Hannah Müller",
                "Fahrrad X3 Hannah Müller Fahrrad X3 End-to-End-Ref.: NOTPROVIDED",
                "Gutschrift"
            )
            .unwrap()
        );
        assert_eq!(ext_ids(&db, acc), vec!["fints-1"]);
    }

    #[test]
    fn adopt_fallback_is_skipped_when_ambiguous() {
        let (_dir, v) = vault();
        let acc = mm_acct(
            &v,
            "10.05.2026;Bäckerei Müller;Brötchen;-3,00;;\n10.05.2026;Bäckerei Müller;Kuchen;-3,00;;\n",
        );
        let db = v.db().unwrap();
        assert!(
            !adopt_import(
                &db,
                acc,
                "f1",
                "2026-05-10",
                -300,
                "",
                "Bäckerei Müller",
                "EREF+1",
                ""
            )
            .unwrap()
        );
        assert!(ext_ids(&db, acc).iter().all(|e| e.starts_with("mm-")));
    }

    /// Active rows of an account as (booking date, external id), by date then id.
    fn date_ids(db: &Connection, acc: i64) -> Vec<(String, String)> {
        let mut st = db
            .prepare(
                "SELECT booking_date, external_id FROM transactions
                  WHERE account_id = ?1 AND deleted_ms IS NULL ORDER BY booking_date, id",
            )
            .unwrap();
        st.query_map([acc], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|x| x.unwrap())
            .collect()
    }

    // #830: comdirect card payments via FinTS name the bank as counterparty and put the
    // merchant into the purpose; MoneyMoney has the merchant as counterparty and an
    // EREF the bank does not send. Invented data.

    #[test]
    fn identical_card_payments_on_one_day_are_both_adopted() {
        let (_dir, v) = vault();
        let acc = mm_acct(
            &v,
            "17.09.2026;Bäckerei Sonne;Kartenzahlung EREF: 1111;-4,20;Essen;\n17.09.2026;Bäckerei Sonne;Kartenzahlung EREF: 2222;-4,20;Essen;\n",
        );
        let db = v.db().unwrap();
        for ext in ["f1", "f2"] {
            assert!(
                adopt_import(
                    &db,
                    acc,
                    ext,
                    "2026-09-17",
                    -420,
                    "",
                    "comdirect",
                    "Bäckerei Sonne Berlin Kartenzahlung",
                    "",
                )
                .unwrap(),
                "{ext} adopted"
            );
        }
        assert_eq!(ext_ids(&db, acc), vec!["f1", "f2"]);
        // a third bank booking finds nothing left: no wrong adoption
        assert!(
            !adopt_import(
                &db,
                acc,
                "f3",
                "2026-09-17",
                -420,
                "",
                "comdirect",
                "Bäckerei Sonne Berlin Kartenzahlung",
                ""
            )
            .unwrap()
        );
    }

    #[test]
    fn equal_payments_two_days_apart_go_to_the_closest_date() {
        let (_dir, v) = vault();
        let acc = mm_acct(
            &v,
            "07.09.2026;Bäckerei Sonne;Kartenzahlung EREF: 1111;-4,20;;\n09.09.2026;Bäckerei Sonne;Kartenzahlung EREF: 2222;-4,20;;\n",
        );
        let db = v.db().unwrap();
        // the later one first: it must not take the 7.9. row
        for (ext, date) in [("f9", "2026-09-09"), ("f7", "2026-09-07")] {
            assert!(
                adopt_import(
                    &db,
                    acc,
                    ext,
                    date,
                    -420,
                    "",
                    "comdirect",
                    "Bäckerei Sonne Berlin Kartenzahlung",
                    "",
                )
                .unwrap()
            );
        }
        assert_eq!(
            date_ids(&db, acc),
            vec![
                ("2026-09-07".to_string(), "f7".to_string()),
                ("2026-09-09".to_string(), "f9".to_string())
            ]
        );
    }

    #[test]
    fn twins_with_different_category_stay_ambiguous() {
        let (_dir, v) = vault();
        let acc = mm_acct(
            &v,
            "17.09.2026;Bäckerei Sonne;Kartenzahlung EREF: 1111;-4,20;Essen;\n17.09.2026;Bäckerei Sonne;Kartenzahlung EREF: 2222;-4,20;Geschenke;\n",
        );
        let db = v.db().unwrap();
        assert!(
            !adopt_import(
                &db,
                acc,
                "f1",
                "2026-09-17",
                -420,
                "",
                "comdirect",
                "Bäckerei Sonne Berlin Kartenzahlung",
                ""
            )
            .unwrap()
        );
        assert!(ext_ids(&db, acc).iter().all(|e| e.starts_with("mm-")));
    }

    #[test]
    fn existing_twin_duplicates_are_repaired_on_resync() {
        // a vault synced before #830: both bank rows exist next to the MoneyMoney twins
        let (_dir, v) = vault();
        let acc = mm_acct(
            &v,
            "17.09.2026;Bäckerei Sonne;Kartenzahlung EREF: 1111;-4,20;Essen;\n17.09.2026;Bäckerei Sonne;Kartenzahlung EREF: 2222;-4,20;Essen;\n",
        );
        let db = v.db().unwrap();
        let mut bank_ids = Vec::new();
        for ext in ["f1", "f2"] {
            db.execute(
                "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, counterparty, purpose)
                 VALUES (?1, ?2, '2026-09-17', -420, 'comdirect', 'Bäckerei Sonne Berlin Kartenzahlung')",
                params![acc, ext],
            )
            .unwrap();
            bank_ids.push(db.last_insert_rowid());
        }
        for bank_id in bank_ids {
            assert!(
                merge_synced_twin(
                    &db,
                    bank_id,
                    "2026-09-17",
                    -420,
                    "",
                    "comdirect",
                    "Bäckerei Sonne Berlin Kartenzahlung"
                )
                .unwrap()
            );
        }
        let active: Vec<String> = date_ids(&db, acc).into_iter().map(|(_, e)| e).collect();
        assert_eq!(
            active,
            vec!["f1", "f2"],
            "two rows left, both under the bank ids"
        );
    }

    #[test]
    fn purpose_without_refs_drops_only_moneymoney_references() {
        assert_eq!(
            purpose_without_refs("Kartenzahlung EREF: 1111 MREF: M-1 CRED: DE98ZZZ09999999999"),
            "Kartenzahlung"
        );
        assert_eq!(purpose_without_refs("EREF+123 Einkauf"), "EREF+123 Einkauf");
    }

    #[test]
    fn merge_synced_twin_repairs_untouched_duplicate_and_spares_edited() {
        let (_dir, v) = vault();
        let acc = mm_acct(
            &v,
            "10.05.2026;REWE Markt;Einkauf;-12,50;Essen;wichtig\n12.05.2026;Aldi;Einkauf;-7,00;Essen;\n",
        );
        let db = v.db().unwrap();
        let ins = |ext: &str, date: &str, cents: i64, name: &str| {
            db.execute(
                "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, counterparty, purpose)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'EREF+1')",
                params![acc, ext, date, cents, name],
            )
            .unwrap();
            db.last_insert_rowid()
        };
        let f1 = ins("f1", "2026-05-10", -1250, "REWE");
        let f2 = ins("f2", "2026-05-12", -700, "ALDI");
        db.execute(
            "UPDATE transactions SET note = 'meins' WHERE external_id = 'f2'",
            [],
        )
        .unwrap();

        assert!(merge_synced_twin(&db, f1, "2026-05-10", -1250, "", "REWE", "EREF+1").unwrap());
        assert!(!merge_synced_twin(&db, f2, "2026-05-12", -700, "", "ALDI", "EREF+1").unwrap());

        let n: i64 = db
            .query_row(
                "SELECT count(*) FROM transactions WHERE account_id = ?1 AND deleted_ms IS NULL",
                [acc],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 3); // REWE merged (2 rows -> 1), Aldi pair kept (edited) -> 1 + 2
        let note: String = db
            .query_row(
                "SELECT note FROM transactions WHERE external_id = 'f1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(note, "wichtig");
    }

    #[test]
    fn short_tax_texts_adopt_the_full_names_of_the_first_holder_only() {
        let (_dir, v) = vault();
        let acc = mm_acct(
            &v,
            "15.03.2026;Kapitalertragsteuer;;-21,37;Zins-Kosten;\n\
             15.03.2026;;Solidaritätszuschlag;-1,17;Zins-Kosten;\n\
             15.03.2026;;Kap.ertr.st2;-21,37;Zins-Kosten;\n",
        );
        let db = v.db().unwrap();
        let adopt = |id: &str, cents: i64, text: &str| {
            adopt_import(&db, acc, id, "2026-03-15", cents, "", "", text, text).unwrap()
        };
        assert!(adopt("c2", -2137, "Kap.ertr.st2"));
        assert!(adopt("c1", -2137, "Kap.ertr.st1"));
        assert!(adopt("s1", -117, "SoliZuschl.1"));
        let name: String = db
            .query_row(
                "SELECT counterparty FROM transactions WHERE external_id = 'c1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(name, "Kapitalertragsteuer");

        let (_dir2, v2) = vault();
        let other = mm_acct(&v2, "15.03.2026;Kapitalertragsteuer;;-21,37;;\n");
        let db2 = v2.db().unwrap();
        assert!(
            !adopt_import(
                &db2,
                other,
                "c2",
                "2026-03-15",
                -2137,
                "",
                "",
                "Kap.ertr.st2",
                "Kap.ertr.st2"
            )
            .unwrap(),
            "the second holder's tax is another booking"
        );
    }

    #[test]
    fn old_tax_duplicates_are_merged_unless_edited() {
        let (_dir, v) = vault();
        let acc = mm_acct(
            &v,
            "17.02.2025;Kapitalertragsteuer;;-34,56;Zins-Kosten;wichtig\n\
             17.02.2025;Solidaritätszuschlag;;-1,90;Zins-Kosten;\n",
        );
        let db = v.db().unwrap();
        let ins = |ext: &str, cents: i64, purpose: &str, note: &str| {
            db.execute(
                "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, purpose, booking_text, note)
                 VALUES (?1, ?2, '2025-02-17', ?3, ?4, ?4, ?5)",
                params![acc, ext, cents, purpose, note],
            )
            .unwrap();
        };
        ins("c1", -3456, "Kap.ertr.st1", "");
        ins("s1", -190, "SoliZuschl.1", "meins");

        merge_synced_tax_twins(&db, acc).unwrap();

        let rows: Vec<(String, String)> = db
            .prepare(
                "SELECT external_id, note FROM transactions
                  WHERE account_id = ?1 AND deleted_ms IS NULL ORDER BY amount_cents",
            )
            .unwrap()
            .query_map([acc], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows.len(), 3, "{rows:?}");
        assert_eq!(rows[0], ("c1".to_string(), "wichtig".to_string()));
        assert!(rows[1].0.starts_with("mm-") || rows[2].0.starts_with("mm-"));
        assert!(rows.iter().any(|r| r == &("s1".into(), "meins".into())));
    }

    #[test]
    fn xml_import_into_existing_account() {
        let (_dir, v) = vault();
        let first = v
            .import_moneymoney_data(CSV.as_bytes().to_vec(), "a.csv".into(), None, true, true)
            .unwrap();
        let r = v
            .import_moneymoney_data(
                XML.as_bytes().to_vec(),
                "b.xml".into(),
                Some(first.account_id),
                true,
                false,
            )
            .unwrap();
        assert_eq!(
            (r.imported, r.notes_imported, r.categories_created),
            (1, 0, 2)
        );
        assert!(!r.account_created);
    }

    const CAMT053: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Document xmlns="urn:iso:std:iso:20022:tech:xsd:camt.053.001.02">
 <BkToCstmrStmt><Stmt>
  <Acct><Id><IBAN>DE44520900000012345601</IBAN></Id></Acct>
  <Ntry>
   <Amt Ccy="EUR">31.96</Amt><CdtDbtInd>DBIT</CdtDbtInd><Sts>BOOK</Sts>
   <BookgDt><Dt>2026-11-07</Dt></BookgDt><ValDt><Dt>2026-11-07</Dt></ValDt>
   <NtryDtls><TxDtls>
     <RltdPties><Cdtr><Nm>Beispiel Markt GmbH</Nm></Cdtr><CdtrAcct><Id><IBAN>DE12300700100123456700</IBAN></Id></CdtrAcct></RltdPties>
     <RmtInf><Ustrd>Einkauf 42</Ustrd></RmtInf>
   </TxDtls></NtryDtls>
  </Ntry>
  <Ntry>
   <Amt Ccy="EUR">100.00</Amt><CdtDbtInd>CRDT</CdtDbtInd><Sts>BOOK</Sts>
   <BookgDt><Dt>2026-11-08</Dt></BookgDt>
   <NtryDtls><TxDtls>
     <RltdPties><Dbtr><Nm>Erika Muster</Nm></Dbtr></RltdPties>
     <RmtInf><Ustrd>Rueckzahlung</Ustrd></RmtInf>
   </TxDtls></NtryDtls>
  </Ntry>
 </Stmt></BkToCstmrStmt>
</Document>"#;

    /// The assistant preselects the account a file belongs to.
    #[test]
    fn preview_suggests_the_matching_account() {
        let (_dir, v) = vault();
        let set = |id: i64, name: &str, iban: &str| {
            v.db()
                .unwrap()
                .execute(
                    "UPDATE accounts SET name = ?2, iban = ?3 WHERE id = ?1",
                    params![id, name, iban],
                )
                .unwrap();
        };
        // 1. bookings already in an account
        let a = v
            .import_moneymoney_data(CSV.as_bytes().to_vec(), "a.csv".into(), None, true, true)
            .unwrap();
        let p = v
            .preview_moneymoney_import(CSV.as_bytes().to_vec(), "Export.csv".into(), None)
            .unwrap();
        assert_eq!(p.suggested_account_id, Some(a.account_id));

        // 2. camt: the statement IBAN wins, even without matching bookings
        let camt = CAMT053.replace("DE44520900000012345601", "DE89370400440532013000");
        let b = v
            .import_moneymoney_data(CSV.as_bytes().to_vec(), "b.csv".into(), None, true, true)
            .unwrap();
        set(b.account_id, "Tagesgeld", "DE89 3704 0044 0532 0130 00");
        let p = v
            .preview_moneymoney_import(camt.clone().into_bytes(), "x.xml".into(), None)
            .unwrap();
        assert_eq!(p.suggested_account_id, Some(b.account_id));

        // 3. nothing in common: the file name names the account
        set(a.account_id, "Girokonto", "");
        let other = "\"Datum\";\"Betrag\"\n\"01.01.2001\";\"1,00\"\n";
        let p = v
            .preview_moneymoney_import(other.as_bytes().to_vec(), "Girokonto.csv".into(), None)
            .unwrap();
        assert_eq!(p.suggested_account_id, Some(a.account_id));
        let p = v
            .preview_moneymoney_import(other.as_bytes().to_vec(), "Unbekannt.csv".into(), None)
            .unwrap();
        assert_eq!(p.suggested_account_id, None);
    }

    #[test]
    fn export_detection_camt053() {
        let rows = parse_export(CAMT053.as_bytes()).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].amount_cents, -3196);
        assert_eq!(rows[0].booking_date, "2026-11-07");
        assert_eq!(rows[0].name, "Beispiel Markt GmbH");
        assert_eq!(rows[0].iban, "DE12300700100123456700");
        assert_eq!(rows[0].purpose, "Einkauf 42");
        assert_eq!(rows[1].amount_cents, 10000);
        assert_eq!(rows[1].name, "Erika Muster");
    }

    #[test]
    fn export_detection_mt940() {
        let mt = ":20:STARTUMS\r\n:25:52090000/0012345678\r\n:28C:00000/001\r\n:60F:C250101EUR1000,00\r\n\
:61:2501020102DR23,45NDDTNONREF\r\n:86:105?00FOLGELASTSCHRIFT?20SVWZ+Einkauf Markt?32Markt GmbH\r\n\
:61:2501030103CR2500,00NTRFNONREF\r\n:86:166?00GUTSCHRIFT?20SVWZ+Gehalt?32Muster GmbH\r\n:62F:C250103EUR3476,55\r\n-\r\n";
        let rows = parse_export(mt.as_bytes()).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].amount_cents, -2345);
        assert_eq!(rows[0].booking_date, "2025-01-02");
        assert_eq!(rows[0].name, "Markt GmbH");
        assert_eq!(rows[1].amount_cents, 250000);
    }

    #[test]
    fn xml_values_unescape_entities_and_cdata() {
        assert_eq!(
            xml_unescape(" Strom &amp; Gas &#65;&#x47; "),
            "Strom & Gas AG"
        );
        assert_eq!(
            xml_unescape("&lt;b&gt; &quot;x&quot; &apos;y&apos;"),
            "<b> \"x\" 'y'"
        );
        assert_eq!(xml_unescape("<![CDATA[ a &amp; b ]]>"), "a &amp; b");
        // an invalid code point is dropped
        assert_eq!(xml_unescape("a&#xD800;b"), "ab");
        // no double decoding
        assert_eq!(xml_unescape("&amp;#38;lt;"), "&#38;lt;");
        assert_eq!(xml_unescape("&#38;lt;"), "&lt;");
        assert_eq!(xml_unescape("&amp;lt;"), "&lt;");
    }

    #[test]
    fn amounts_with_german_grouping_and_no_cents() {
        assert_eq!(amount("1.234"), Some(123_400));
        assert_eq!(amount("-1.234.567"), Some(-123_456_700));
        assert_eq!(amount("EUR"), None);
    }

    #[test]
    fn statement_account_from_camt_and_mt940() {
        let camt = CAMT053
            .replace("DE44520900000012345601", "DE89370400440532013000")
            .replace("</Id></Acct>", "</Id><Ccy>eur</Ccy></Acct>");
        assert_eq!(
            statement_account(&camt),
            Some(("DE89370400440532013000".into(), "EUR".into()))
        );
        let two = camt.replace("</Stmt>", "</Stmt><Stmt></Stmt>");
        assert_eq!(statement_account(&two), None);
        for (open, close) in [("Rpt", "Rpt"), ("Ntfctn", "Ntfctn")] {
            let other = camt
                .replace("<Stmt>", &format!("<{open}>"))
                .replace("</Stmt>", &format!("</{close}>"));
            assert_eq!(
                statement_account(&other),
                Some(("DE89370400440532013000".into(), "EUR".into())),
                "{open}"
            );
        }

        let mt = |accounts: &[&str]| -> String {
            accounts
                .iter()
                .map(|a| {
                    format!(":20:STARTUMS\r\n:25:{a}\r\n:61:2501020102DR23,45NDDTNONREF\r\n-\r\n")
                })
                .collect()
        };
        let iban = "DE89 3704 0044 0532 0130 00";
        assert_eq!(
            statement_account(&mt(&[iban, iban])),
            Some(("DE89370400440532013000".into(), String::new()))
        );
        assert_eq!(statement_account(&mt(&["52090000/0012345678"])), None);
        assert_eq!(
            statement_account(&mt(&[iban, "DE02120300000000202051"])),
            None
        );
    }
}
