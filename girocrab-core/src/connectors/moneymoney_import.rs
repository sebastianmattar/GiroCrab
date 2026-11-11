//! MoneyMoney migration: lossless import of MoneyMoney CSV / XML exports.
//!
//! * Parses German and English CSV headers (`;` `,` or tab) and the XML
//!   `<transactions><transaction>…` export.
//! * Keeps hierarchical categories (`Haushalt\Lebensmittel`, `Steuern / Büro`)
//!   as a category tree and the user's notes in `transactions.note`.
//! * Idempotent: every row gets a deterministic fingerprint
//!   (`booking_date + amount + counterparty IBAN/name + purpose`). Re-importing a
//!   file, or importing a range that overlaps bookings already fetched via
//!   FinTS, never duplicates – existing rows are enriched instead. A later
//!   FinTS sync adopts imported rows (see [`adopt_import`]).

use std::collections::HashMap;

use regex::Regex;
use rusqlite::{params, Connection, OptionalExtension};

use crate::connectors::portal_import::{parse_date, split_csv};
use crate::error::{CoreError, CoreResult};
use crate::i18n::{tr, trf};
use crate::merchants::{FlowKind, MerchantResolver};
use crate::models::*;
use crate::money::parse_cents;
use crate::vault::{enrich, Vault};

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
}

// ------------------------------------------------------------ fingerprinting

fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Deterministic de-duplication key (stable across Rust versions: FNV-1a 64).
pub fn fingerprint(date: &str, cents: i64, iban: &str, name: &str, purpose: &str) -> String {
    let who = if iban.trim().is_empty() {
        norm(name)
    } else {
        norm(iban)
    };
    let key = format!("{date}|{cents}|{who}|{}", norm(purpose));
    let mut h: u64 = 0xcbf29ce484222325;
    for b in key.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
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

    fn fp(&self) -> String {
        fingerprint(
            &self.booking_date,
            self.amount_cents,
            &self.iban,
            &self.name,
            &self.purpose,
        )
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

/// Splits `Haushalt\Lebensmittel`, `Steuern / Büro`, `A > B` into path parts.
pub fn split_category(raw: &str) -> Vec<String> {
    raw.replace(" / ", "\\")
        .replace(" > ", "\\")
        .replace(" › ", "\\")
        .split('\\')
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
    let re = Regex::new(r"^([+-]?)(\d{1,3}(?:\.\d{3})+)$").unwrap();
    re.captures(t).and_then(|c| {
        let digits: String = c[2].chars().filter(|c| c.is_ascii_digit()).collect();
        let v: i64 = digits.parse::<i64>().ok()?.checked_mul(100)?;
        Some(if &c[1] == "-" { -v } else { v })
    })
}

fn truthy(s: &str) -> bool {
    matches!(
        s.trim().to_lowercase().as_str(),
        "1" | "true" | "yes" | "ja" | "x" | "✓" | "✔"
    )
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Field {
    Date,
    Value,
    Category,
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
    if let Some(rest) = first.strip_prefix("sep=") {
        if let Some(c) = rest.chars().next() {
            return c;
        }
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
            if c.is_empty() {
                "EUR".into()
            } else {
                c
            }
        },
        name: clean(get(Field::Name)),
        iban: get(Field::Iban)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>(),
        bic: get(Field::Bic).trim().to_string(),
        purpose: clean(get(Field::Purpose)),
        booking_text: clean(get(Field::Text)),
        category: get(Field::Category).trim().to_string(),
        category_parts: Vec::new(),
        note: get(Field::Note).trim().to_string(),
        checkmark: truthy(&get(Field::Check)),
        mandate_ref: get(Field::Mandate).trim().to_string(),
        creditor_id: get(Field::Creditor).trim().to_string(),
        end_to_end: get(Field::EndToEnd).trim().to_string(),
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
    let (hi, cols) = rows
        .iter()
        .enumerate()
        .find_map(|(i, r)| {
            let map: Vec<Option<Field>> = r.iter().map(|c| field_for(c)).collect();
            (map.contains(&Some(Field::Date)) && map.contains(&Some(Field::Amount)))
                .then_some((i, map))
        })
        .ok_or_else(|| CoreError::InvalidInput {
            message: tr("Keine MoneyMoney-Kopfzeile (Datum/Buchungstag und Betrag) gefunden.")
                .into(),
        })?;
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
        if let Some(row) = build_row(|f| seen.get(&f).cloned().unwrap_or_default()) {
            out.push(row);
        }
    }
    Ok(out)
}

fn xml_unescape(s: &str) -> String {
    let s = s.trim();
    let s = s
        .strip_prefix("<![CDATA[")
        .and_then(|x| x.strip_suffix("]]>"))
        .map(|x| x.to_string())
        .unwrap_or_else(|| {
            let re = Regex::new(r"&#(x?[0-9a-fA-F]+);").unwrap();
            let t = re.replace_all(s, |c: &regex::Captures| {
                let v = &c[1];
                let n = match v.strip_prefix('x') {
                    Some(h) => u32::from_str_radix(h, 16).ok(),
                    None => v.parse().ok(),
                };
                n.and_then(char::from_u32)
                    .map(|c| c.to_string())
                    .unwrap_or_default()
            });
            t.replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&quot;", "\"")
                .replace("&apos;", "'")
                .replace("&amp;", "&")
        });
    s.trim().to_string()
}

pub fn parse_xml_export(text: &str) -> CoreResult<Vec<MmRow>> {
    let block = Regex::new(r"(?is)<transaction(?:\s[^>]*)?>(.*?)</transaction\s*>").unwrap();
    let leaf = Regex::new(r"(?is)<([A-Za-z_][\w.\-]*)(?:\s[^>]*)?>(.*?)</([A-Za-z_][\w.\-]*)\s*>")
        .unwrap();
    let mut out = Vec::new();
    for b in block.captures_iter(text) {
        let mut fields: HashMap<Field, String> = HashMap::new();
        for l in leaf.captures_iter(&b[1]) {
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
    let (txs, _) = super::fints::camt::parse(xml, "");
    txs.into_iter().map(remote_to_row).collect()
}

fn looks_like_mt940(text: &str) -> bool {
    let t = text.replace("\r", "");
    t.contains(":61:") && (t.contains(":20:") || t.contains(":25:") || t.contains(":60F:"))
}

/// MT940 (`.sta` / `.mt940`) file → rows.
pub fn parse_mt940_statement(text: &str) -> Vec<MmRow> {
    let (txs, _) = super::fints::mt940::parse(text, "", "EUR");
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

struct Existing {
    id: i64,
    category_source: String,
    note: String,
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
                "SELECT id FROM categories WHERE parent_id IS ?1 AND name = ?2 COLLATE NOCASE",
                params![parent, part],
                |r| r.get(0),
            )
            .optional()?;
        let id = match found {
            Some(id) => id,
            None => {
                let order: i32 = conn.query_row(
                    "SELECT IFNULL(MAX(sort_order), 0) + 1 FROM categories WHERE parent_id IS ?1",
                    [parent],
                    |r| r.get(0),
                )?;
                conn.execute(
                    "INSERT INTO categories(parent_id, name, sf_symbol, color_hex, sort_order) VALUES (?1, ?2, 'tag', '#8E8E93', ?3)",
                    params![parent, part, order],
                )?;
                *created += 1;
                conn.last_insert_rowid()
            }
        };
        cache.insert(key.clone(), id);
        parent = Some(id);
    }
    parent.ok_or_else(|| CoreError::InvalidInput {
        message: "empty category".into(),
    })
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
        "MoneyMoney-Import".into()
    } else {
        format!("{stem} (MoneyMoney)")
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
                .query_row("SELECT currency FROM accounts WHERE id = ?1", [id], |r| {
                    r.get::<_, String>(0)
                })
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
                                      balance_cents, balance_date, sf_symbol, sort_order)
                 VALUES (NULL, ?1, ?2, ?3, 'MoneyMoney-Import', ?4, 'checking', 0, '', 'archivebox',
                         (SELECT IFNULL(MAX(sort_order),0)+1 FROM accounts))",
                params![
                    gid,
                    format!("moneymoney-{}", uuid::Uuid::new_v4()),
                    account_name_from_file(file_name),
                    account_currency
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
    // Existing bookings of the account, bucketed by fingerprint (multiset: two
    // identical coffees on one day stay two rows).
    let mut existing: HashMap<String, Vec<Existing>> = HashMap::new();
    {
        let mut q = conn.prepare(
            "SELECT id, booking_date, amount_cents, counterparty_iban, counterparty, purpose,
                    import_fp, category_source, note
             FROM transactions WHERE account_id = ?1 ORDER BY id",
        )?;
        let it = q.query_map([st.account_id], |r| {
            let fp: String = r.get(6)?;
            let fp = if fp.is_empty() {
                fingerprint(
                    &r.get::<_, String>(1)?,
                    r.get(2)?,
                    &r.get::<_, String>(3)?,
                    &r.get::<_, String>(4)?,
                    &r.get::<_, String>(5)?,
                )
            } else {
                fp
            };
            Ok((
                fp,
                Existing {
                    id: r.get(0)?,
                    category_source: r.get(7)?,
                    note: r.get(8)?,
                },
            ))
        })?;
        for e in it {
            let (fp, e) = e?;
            existing.entry(fp).or_default().push(e);
        }
    }

    let mut seen: HashMap<String, usize> = HashMap::new();
    for r in rows {
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
        let note = if preserve_notes { r.note.as_str() } else { "" };
        if let Some(e) = existing.get(&fp).and_then(|v| v.get(k)) {
            st.duplicates += 1;
            let mut changed = false;
            if let Some(c) = cat_id.filter(|_| e.category_source != "manual") {
                conn.execute(
                    "UPDATE transactions SET category_id = ?1, rule_id = NULL, category_source = 'manual' WHERE id = ?2",
                    params![c, e.id],
                )?;
                changed = true;
            }
            if !note.is_empty() && e.note.trim().is_empty() {
                conn.execute(
                    "UPDATE transactions SET note = ?1 WHERE id = ?2",
                    params![note, e.id],
                )?;
                st.notes += 1;
                changed = true;
            }
            if changed {
                st.enriched += 1;
            }
            continue;
        }
        conn.execute(
            "INSERT INTO transactions(account_id, external_id, booking_date, value_date, amount_cents, currency,
                    counterparty, counterparty_iban, purpose, booking_text, category_id, category_source, checked, note, import_fp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 1, ?13, ?14)",
            params![
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
                fp
            ],
        )?;
        st.new += 1;
        st.balance_cents += r.amount_cents;
        if !note.is_empty() {
            st.notes += 1;
        }
    }
    Ok(())
}

/// Finds the one not-yet-adopted MoneyMoney row (`mm-*`) that is the twin of a bank
/// booking: same amount, booking date within ±2 days and a matching counterparty
/// (IBAN, name, or the MoneyMoney name appearing in the bank's name/purpose).
/// Ambiguous (≥ 2) or missing candidates yield `None`.
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
    let mut stmt = conn.prepare(
        "SELECT id, counterparty, counterparty_iban FROM transactions
          WHERE account_id = ?1 AND external_id LIKE 'mm-%' AND amount_cents = ?2
            AND id != ?3 AND abs(julianday(booking_date) - julianday(?4)) <= 2",
    )?;
    let rows = stmt.query_map(
        params![account_id, cents, exclude_id.unwrap_or(-1), date],
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        },
    )?;
    let (n_iban, n_name, n_purpose) = (norm(iban), norm(name), norm(purpose));
    let mut hits = Vec::new();
    for row in rows {
        let (id, mm_name, mm_iban) = row?;
        let (m_iban, m_name) = (norm(&mm_iban), norm(&mm_name));
        let ok = if !n_iban.is_empty() && !m_iban.is_empty() {
            n_iban == m_iban
        } else {
            let long = n_name.len() >= 3 && m_name.len() >= 3;
            (long && (n_name.contains(&m_name) || m_name.contains(&n_name)))
                || (m_name.len() >= 3 && (n_purpose.contains(&m_name) || n_name.contains(&m_name)))
        };
        if ok {
            hits.push(id);
        }
    }
    Ok(if hits.len() == 1 { Some(hits[0]) } else { None })
}

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
    let fp = fingerprint(date, cents, iban, name, purpose);
    let mut row: Option<i64> = conn
        .query_row(
            "SELECT id FROM transactions WHERE account_id = ?1 AND import_fp = ?2 AND external_id LIKE 'mm-%' LIMIT 1",
            params![account_id, fp],
            |r| r.get(0),
        )
        .optional()?;
    if row.is_none() {
        row = find_twin(conn, account_id, None, date, cents, iban, name, purpose)?;
    }
    let Some(id) = row else { return Ok(false) };
    let taken: bool = conn
        .query_row(
            "SELECT 1 FROM transactions WHERE account_id = ?1 AND external_id = ?2",
            params![account_id, external_id],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);
    if taken {
        return Ok(false);
    }
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
    conn.execute(
        "UPDATE transactions SET external_id = ?1,
                counterparty_iban = CASE WHEN counterparty_iban = '' THEN ?3 ELSE counterparty_iban END,
                booking_text = CASE WHEN booking_text = '' THEN ?4 ELSE booking_text END
          WHERE id = ?2",
        params![external_id, id, iban, booking_text],
    )?;
    Ok(())
}

/// Re-sync repair: the bank booking `external_id` already exists (old duplicate) while its
/// MoneyMoney twin is still un-adopted. If the bank row is untouched (no manual category,
/// note, check mark, splits or documents) it is removed and the rich MoneyMoney row takes
/// over its external id. Returns true when merged.
pub(crate) fn merge_synced_twin(
    conn: &Connection,
    account_id: i64,
    external_id: &str,
    date: &str,
    cents: i64,
    iban: &str,
    name: &str,
    purpose: &str,
) -> CoreResult<bool> {
    let bank: Option<(i64, String, String)> = conn
        .query_row(
            "SELECT id, counterparty_iban, booking_text FROM transactions
              WHERE account_id = ?1 AND external_id = ?2
                AND category_source != 'manual' AND checked = 0 AND trim(note) = ''
                AND NOT EXISTS (SELECT 1 FROM transaction_splits s WHERE s.transaction_id = transactions.id)
                AND NOT EXISTS (SELECT 1 FROM booking_documents d WHERE d.booking_id = transactions.id)",
            params![account_id, external_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((bank_id, bank_iban, bank_text)) = bank else {
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
    conn.execute("DELETE FROM transactions WHERE id = ?1", [bank_id])?;
    take_over(conn, mm_id, external_id, &bank_iban, &bank_text)?;
    Ok(true)
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
        category_name: split_category(&r.category)
            .last()
            .cloned()
            .unwrap_or_default(),
        category_path: split_category(&r.category).join(" › "),
        category_color_hex: String::new(),
        category_symbol: String::new(),
        category_source: if r.category.is_empty() {
            CategorySource::None
        } else {
            CategorySource::Manual
        },
        rule_id: None,
        checked: true,
        merchant: MerchantResolver::resolve("", ""),
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
    };
    enrich(&mut b);
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
        let st = run(&tx, &rows, &file_name, target_account_id, true, true)?;
        let mut cats: Vec<String> = rows
            .iter()
            .map(|r| split_category(&r.category).join(" › "))
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
    use super::*;
    use std::sync::Arc;

    fn vault() -> Arc<Vault> {
        let path = std::env::temp_dir().join(format!("girocrab-mm-{}.db", uuid::Uuid::new_v4()));
        Vault::open(path.to_string_lossy().to_string(), vec![5u8; 32]).unwrap()
    }

    const CSV: &str = "\"Datum\";\"Wertstellung\";\"Kategorie\";\"Name\";\"Verwendungszweck\";\"Konto\";\"Bank\";\"Betrag\";\"Währung\";\"Notiz\"\n\
\"15.03.2012\";\"15.03.2012\";\"Haushalt\\Supermarkt\";\"REWE Markt\";\"Einkauf Kartenzahlung\";\"DE02120300000000202051\";\"BYLADEM1001\";\"-1.234,56\";\"EUR\";\"Wocheneinkauf\"\n\
\"01.04.2026\";\"01.04.2026\";\"Steuern / Büro\";\"Finanzamt\";\"Erstattung\";\"\";\"\";\"2.000,00\";\"EUR\";\"\"\n";

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
        let v = vault();
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

    #[test]
    fn reimport_creates_no_duplicates_and_keeps_identical_rows() {
        let v = vault();
        let twin = format!("{CSV}\"15.03.2012\";\"\";\"Haushalt\\Supermarkt\";\"REWE Markt\";\"Einkauf Kartenzahlung\";\"DE02120300000000202051\";\"\";\"-1.234,56\";\"EUR\";\"\"\n");
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

    #[test]
    fn overlap_with_fints_booking_enriches_and_fints_adopts_import() {
        let v = vault();
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
        let v = vault();
        let acc = mm_acct(
            &v,
            "10.05.2026;REWE Markt;Einkauf;-12,50;Essen;n1\n11.05.2026;Stadtwerke Köln;Strom;-80,00;Wohnen;n2\n",
        );
        let db = v.db().unwrap();
        // purpose differs (SEPA tags), date off by one day, name matches
        assert!(adopt_import(
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
        .unwrap());
        // comdirect: partner field holds booking text, MM name sits in the purpose
        assert!(adopt_import(
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
        .unwrap());
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
    fn adopt_fallback_is_skipped_when_ambiguous() {
        let v = vault();
        let acc = mm_acct(
            &v,
            "10.05.2026;Bäckerei Müller;Brötchen;-3,00;;\n10.05.2026;Bäckerei Müller;Kuchen;-3,00;;\n",
        );
        let db = v.db().unwrap();
        assert!(!adopt_import(
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
        .unwrap());
        assert!(ext_ids(&db, acc).iter().all(|e| e.starts_with("mm-")));
    }

    #[test]
    fn merge_synced_twin_repairs_untouched_duplicate_and_spares_edited() {
        let v = vault();
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
        };
        ins("f1", "2026-05-10", -1250, "REWE");
        ins("f2", "2026-05-12", -700, "ALDI");
        db.execute(
            "UPDATE transactions SET note = 'meins' WHERE external_id = 'f2'",
            [],
        )
        .unwrap();

        assert!(
            merge_synced_twin(&db, acc, "f1", "2026-05-10", -1250, "", "REWE", "EREF+1").unwrap()
        );
        assert!(
            !merge_synced_twin(&db, acc, "f2", "2026-05-12", -700, "", "ALDI", "EREF+1").unwrap()
        );

        let n: i64 = db
            .query_row(
                "SELECT count(*) FROM transactions WHERE account_id = ?1",
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
    fn xml_import_into_existing_account() {
        let v = vault();
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
}
