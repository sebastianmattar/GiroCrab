//! Bausparkasse Schwäbisch Hall – customer portal "Mein Konto"
//! (`www.schwaebisch-hall.de`). Bausparverträge with balance and bookings,
//! optionally the PDF attachments of the portal's postbox (#290).
//!
//! The portal has no public API. The sign-in runs in the embedded browser
//! (Azure AD B2C at `login.schwaebisch-hall.de`, policy
//! `B2C_1A_SEAMLESSMIGRATION_SIGNUPORSIGNIN`, started by
//! `GET /bin/bshweb/authentication`); afterwards the portal works with session
//! cookies on `www.schwaebisch-hall.de`. The app's browser script
//! (`SchwaebischHallUI.swift`) calls the portal's own JSON endpoints in the
//! signed-in page and hands only the fields below to the core; cookies and
//! credentials never cross the bridge:
//!
//!  GET /bin/bshweb/accountcontracts
//!      -> {status:"success", data:{contracts:[{type, bsn, accountBalance, vtn, …}]}}
//!         (without a session: {status:"error", errors:[{code:"timeout"}]})
//!  GET /content/bshweb/startseite/mein-konto/vertraege/_jcr_content/root/container/par/
//!      container/myaccount_accounts.account_details.json?vtn={vtn}
//!      -> {contracttype, contractphase, transactions, …}; bookings grouped by
//!         year and month, each with `day`, `subject` and `amount`
//!  GET /content/bshweb/startseite/mein-konto/nachrichten.bin.postboxmessage?command=getmessages
//!  GET /bin/bshweb/postbox/attachment?id={attachment id}
//!
//! Amounts are formatted German strings, positive ones with an HTML entity
//! (`&#43; 1.234,56`). The year/month grouping of the bookings was only seen in
//! descriptions, not in a full response, so [`collect_bookings`] walks the tree
//! and takes year and month from keys or fields. Bookings carry no id: they get
//! content ids in `store_fetch`.
//!
//! Unofficial use of the portal with the user's own sign-in; it may change
//! without notice.

use std::{collections::HashSet, sync::Arc};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rusqlite::{OptionalExtension, params};
use serde_json::{Map, Value, json};

use super::{FetchResult, RemoteAccount, RemoteTransaction, SyncSummary, store_fetch};
use crate::{
    dates::month_from_name,
    error::{CoreError, CoreResult},
    i18n::tr,
    models::{AccountKind, ConnectorKind},
    text::decode_html_entities,
    vault::Vault,
};

/// Account external refs: `bsh:<vtn>`.
pub(crate) const REF_PREFIX: &str = "bsh:";
/// Key of the banking gate (`ConnectorKind::as_str`).
const GATE: &str = "schwaebischhall";
/// Upper bound for bookings per contract: a monthly savings rate over 80 years
/// stays far below; more means the walk went astray.
const MAX_BOOKINGS: usize = 20_000;

fn invalid() -> CoreError {
    CoreError::invalid(tr(
        "Schwäbisch Hall: Unvollständige oder ungültige Portalantwort. Es wurden keine Daten übernommen.",
    ))
}

/// Trimmed string field with whitespace collapsed and HTML entities decoded.
fn text(v: &Value, key: &str) -> String {
    let raw = match &v[key] {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    };
    decode_html_entities(&raw)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// `vtn`: the contract key of the detail call, digits only.
fn vtn(v: &Value) -> CoreResult<String> {
    let s = text(v, "vtn");
    if (6..=24).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit()) {
        Ok(s)
    } else {
        Err(invalid())
    }
}

/// Portal amount (`&#43; 1.234,56`, `- 12,00 €`, `−5,00`, or a JSON number) in cents.
/// Strict: anything but a leading sign, digits, separators, spaces and a
/// currency (`€`/`EUR`) is rejected, so a debit/credit marker such as a
/// trailing `S`/`H` or `-` can never be dropped and flip the sign.
fn amount(v: &Value) -> CoreResult<i64> {
    match v {
        Value::Number(_) => crate::money::json_number_to_cents_rounded(v),
        Value::String(s) => {
            let decoded = decode_html_entities(s).replace(['\u{2212}', '\u{2013}'], "-");
            let mut t = decoded.trim();
            for currency in ["EUR", "eur", "€"] {
                t = t.strip_suffix(currency).unwrap_or(t).trim_end();
                t = t.strip_prefix(currency).unwrap_or(t).trim_start();
            }
            let cleaned: String = t.chars().filter(|c| !c.is_whitespace()).collect();
            let allowed = |c: char| c.is_ascii_digit() || matches!(c, '+' | '-' | ',' | '.');
            if !cleaned.chars().all(allowed) || !cleaned.bytes().any(|b| b.is_ascii_digit()) {
                return Err(invalid());
            }
            crate::money::parse_cents(&cleaned).map_err(|_| invalid())
        }
        _ => Err(invalid()),
    }
}

fn year_of(v: &Value) -> Option<i32> {
    let n = match v {
        Value::Number(n) => n.as_i64()?,
        Value::String(s) => {
            let s = s.trim();
            if s.len() != 4 {
                return None;
            }
            s.parse().ok()?
        }
        _ => return None,
    };
    (1950..=2200).contains(&n).then_some(n as i32)
}

/// Month of a `month`/`monat` field: 1–12 as number or string, or a month name.
fn month_of(v: &Value) -> Option<u32> {
    let n = match v {
        Value::Number(n) => n.as_u64()?,
        Value::String(s) => {
            let t = s.trim();
            if !t.is_empty() && t.len() <= 2 && t.bytes().all(|b| b.is_ascii_digit()) {
                t.parse().ok()?
            } else {
                return month_from_name(t);
            }
        }
        _ => return None,
    };
    (1..=12).contains(&n).then_some(n as u32)
}

/// Month of an object key: a month name or exactly two digits (`"03"`). A
/// bare `"3"` is not taken: it may just be a list index.
fn month_of_key(k: &str) -> Option<u32> {
    if k.len() == 2 && k.bytes().all(|b| b.is_ascii_digit()) {
        return k.parse().ok().filter(|m| (1..=12).contains(m));
    }
    month_from_name(k)
}

/// Year/month an object states in its own fields: `year`/`jahr`, and `month`/
/// `monat` or – as the portal's month groups do (`{name: "Nov", fullname:
/// "November", entries: […]}`, #290) – `fullname`/`name`. Each must hold an
/// exact month name or number; anything else (e.g. `month` holding the list
/// of month groups) is ignored.
fn level_date(obj: &Map<String, Value>) -> (Option<i32>, Option<u32>) {
    let year = ["year", "jahr"]
        .iter()
        .find_map(|n| obj.get(*n))
        .and_then(year_of);
    let month = ["month", "monat", "fullname", "name"]
        .iter()
        .filter_map(|n| obj.get(*n))
        .find_map(month_of);
    (year, month)
}

/// Applies a year/month found at one level: a new year starts without the
/// month of the outer level.
fn narrow(
    (year, month): (Option<i32>, Option<u32>),
    (lvl_year, lvl_month): (Option<i32>, Option<u32>),
) -> (Option<i32>, Option<u32>) {
    match lvl_year {
        Some(y) => (Some(y), lvl_month),
        None => (year, lvl_month.or(month)),
    }
}

/// `dd.mm.yyyy`, `dd.mm.` or `yyyy-mm-dd` inside a day/date field; a bare day otherwise.
fn day_parts(v: &Value) -> (Option<u32>, Option<u32>, Option<i32>) {
    let s = match v {
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.trim().to_string(),
        _ => return (None, None, None),
    };
    if let Some(iso) = s.get(..10).filter(|p| p.as_bytes().get(4) == Some(&b'-'))
        && let Ok(d) = jiff::civil::Date::strptime("%Y-%m-%d", iso)
    {
        return (
            Some(d.day() as u32),
            Some(d.month() as u32),
            Some(d.year().into()),
        );
    }
    let parts: Vec<&str> = s.split('.').map(str::trim).collect();
    let num = |p: Option<&&str>| {
        p.filter(|p| !p.is_empty())
            .and_then(|p| p.parse::<u32>().ok())
    };
    let day = num(parts.first());
    let month = num(parts.get(1)).filter(|m| (1..=12).contains(m));
    let year = parts
        .get(2)
        .and_then(|p| year_of(&Value::String((*p).into())));
    (day, month, year)
}

/// A booking: an object with an amount and a day or date. It must also carry
/// a `subject`; one without is rejected (see `collect_bookings`), so a total
/// that looks like a booking can never be imported as one.
fn is_booking(obj: &Map<String, Value>) -> bool {
    obj.contains_key("amount") && (obj.contains_key("day") || obj.contains_key("date"))
}

/// Keys of year/month totals, never bookings.
fn is_total_key(k: &str) -> bool {
    ["saldo", "summe", "sum", "total", "gesamt"]
        .iter()
        .any(|t| k.eq_ignore_ascii_case(t))
}

struct Booking {
    date: String,
    amount_cents: i64,
    subject: String,
}

/// Walks the `transactions` tree of a contract (years › months › bookings in
/// whatever shape: objects keyed by year/month, arrays of groups with
/// `year`/`month` fields, or both). Totals (`saldo`, `summe`, `total` …) are skipped.
fn collect_bookings(
    v: &Value,
    key: Option<&str>,
    year: Option<i32>,
    month: Option<u32>,
    out: &mut Vec<Booking>,
) -> CoreResult<()> {
    if out.len() > MAX_BOOKINGS {
        return Err(invalid());
    }
    // The key this value hangs under ("2025", "Dezember", "03") dates it too.
    let from_key = key.map_or((None, None), |k| {
        (year_of(&Value::String(k.into())), month_of_key(k))
    });
    let (year, month) = narrow((year, month), from_key);
    match v {
        Value::Array(items) => {
            for item in items {
                collect_bookings(item, None, year, month, out)?;
            }
        }
        Value::Object(obj) if is_booking(obj) => {
            if !obj.get("subject").is_some_and(Value::is_string) {
                return Err(invalid());
            }
            let (year, month) = narrow((year, month), level_date(obj));
            let field = obj.get("day").or_else(|| obj.get("date"));
            let (day, d_month, d_year) = field.map_or((None, None, None), day_parts);
            let y = d_year.or(year).ok_or_else(invalid)?;
            let m = d_month.or(month).ok_or_else(invalid)?;
            let d = day.ok_or_else(invalid)?;
            let date = crate::dates::ymd(y, m, d).ok_or_else(invalid)?;
            out.push(Booking {
                date: crate::dates::iso(date),
                amount_cents: amount(&obj["amount"])?,
                subject: text(v, "subject"),
            });
        }
        Value::Object(obj) => {
            let (year, month) = narrow((year, month), level_date(obj));
            for (k, child) in obj {
                if is_total_key(k) || !(child.is_object() || child.is_array()) {
                    continue;
                }
                collect_bookings(child, Some(k), year, month, out)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn account_kind(contract: &Value, details: &Value) -> AccountKind {
    let words = [
        text(contract, "type"),
        text(details, "contracttype"),
        text(details, "contractphase"),
    ]
    .join(" ")
    .to_lowercase();
    if words.contains("darlehen") || words.contains("kredit") {
        AccountKind::Loan
    } else {
        AccountKind::Savings
    }
}

/// Account name part of a contract type without its phase ("Bausparvertrag
/// Sparphase" → "Bausparvertrag"): the name stays the same when the contract
/// moves on to the loan phase (the kind changes instead).
fn contract_label(contract_type: &str) -> String {
    let label = contract_type
        .split_whitespace()
        .filter(|w| !w.to_lowercase().ends_with("phase"))
        .collect::<Vec<_>>()
        .join(" ");
    if label.is_empty() {
        "Bausparvertrag".into()
    } else {
        label
    }
}

/// Parses the browser script's snapshot (`version: 1`).
fn snapshot(json: &str, today: &str) -> CoreResult<FetchResult> {
    let root: Value = serde_json::from_str(json).map_err(|_| invalid())?;
    if root["version"].as_u64() != Some(1) {
        return Err(invalid());
    }
    let contracts = root["contracts"].as_array().ok_or_else(invalid)?;
    let mut result = FetchResult {
        bank_name: Some("Schwäbisch Hall".into()),
        // The contract list is complete: a paid-out contract that disappears
        // is marked as no longer delivered instead of keeping its last balance.
        accounts_complete: true,
        ..Default::default()
    };
    let mut seen = HashSet::new();
    for c in contracts {
        let vtn = vtn(c)?;
        if !seen.insert(vtn.clone()) {
            continue;
        }
        let details = &c["details"];
        if !details.is_object() {
            return Err(invalid());
        }
        let kind = account_kind(c, details);
        let mut balance = amount(&c["accountBalance"])?;
        // A loan is debt: shown as a negative balance like the other loan accounts.
        if kind == AccountKind::Loan && balance > 0 {
            balance = -balance;
        }
        let bsn = text(c, "bsn");
        let label = contract_label(&text(c, "type"));
        let account = RemoteAccount {
            external_ref: format!("{REF_PREFIX}{vtn}"),
            name: format!("{label} {bsn}").trim().to_string(),
            owner: String::new(),
            iban: String::new(),
            account_number: if bsn.is_empty() { vtn.clone() } else { bsn },
            currency: "EUR".into(),
            kind,
            balance_cents: balance,
            balance_date: today.into(),
        };
        let mut bookings = Vec::new();
        match &details["transactions"] {
            Value::Null => {}
            t @ (Value::Array(_) | Value::Object(_)) => {
                collect_bookings(t, None, None, None, &mut bookings)?
            }
            _ => return Err(invalid()),
        }
        result
            .transactions
            .extend(bookings.into_iter().map(|b| RemoteTransaction {
                account_ref: account.external_ref.clone(),
                // empty: store_fetch derives a content id
                external_id: String::new(),
                booking_date: b.date.clone(),
                value_date: b.date,
                amount_cents: b.amount_cents,
                currency: "EUR".into(),
                purpose: b.subject,
                ..Default::default()
            }));
        result.accounts.push(account);
    }
    if result.accounts.is_empty() {
        return Err(invalid());
    }
    Ok(result)
}

/// Postbox attachment id: opaque, but bounded and printable.
fn attachment_id(doc: &Value) -> CoreResult<String> {
    let id = doc["id"].as_str().map(str::trim).unwrap_or_default();
    if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
        return Err(invalid());
    }
    Ok(id.to_string())
}

/// `YYYY-MM-DD` of a postbox date (`dd.mm.yyyy`, ISO, or Unix time in
/// seconds or milliseconds: below 10^11 it is seconds, i.e. before 5138).
fn document_date(v: &Value) -> Option<String> {
    if let Some(n) = v.as_i64() {
        let ms = if n.abs() < 100_000_000_000 {
            n.checked_mul(1000)?
        } else {
            n
        };
        return jiff::Timestamp::from_millisecond(ms)
            .ok()
            .map(|t| crate::dates::iso(crate::dates::utc_date(t)));
    }
    let s = v.as_str()?.trim();
    if let Some(d) = s
        .get(..10)
        .and_then(|p| jiff::civil::Date::strptime("%Y-%m-%d", p).ok())
    {
        return Some(crate::dates::iso(d));
    }
    let (d, m, y) = day_parts(&Value::String(s.into()));
    crate::dates::ymd(y?, m?, d?).map(crate::dates::iso)
}

/// Account a postbox attachment belongs to: the contract the message names
/// (`vtn`), else the connection's first contract that is still delivered (not
/// a paid-out one, see `accounts_complete`). `None` when that contract is
/// not imported (held back for the account choice, skipped, archived, deleted).
fn document_account(
    db: &rusqlite::Connection,
    connection_id: i64,
    doc: &Value,
) -> CoreResult<Option<i64>> {
    let active = "connection_id = ?1 AND deleted_ms IS NULL AND archived_unix IS NULL";
    match &doc["vtn"] {
        Value::Null => Ok(db
            .query_row(
                &format!(
                    "SELECT id FROM accounts WHERE {active} AND external_ref LIKE ?2
                     ORDER BY access_revoked, id LIMIT 1"
                ),
                params![connection_id, format!("{REF_PREFIX}%")],
                |r| r.get(0),
            )
            .optional()?),
        v => {
            let vtn = vtn(&json!({ "vtn": v }))?;
            Ok(db
                .query_row(
                    &format!("SELECT id FROM accounts WHERE {active} AND external_ref = ?2"),
                    params![connection_id, format!("{REF_PREFIX}{vtn}")],
                    |r| r.get(0),
                )
                .optional()?)
        }
    }
}

fn document_source(connection_id: i64, id: &str) -> String {
    format!("{GATE}:{connection_id}:{}", URL_SAFE_NO_PAD.encode(id))
}

impl Vault {
    fn check_schwaebisch_hall(&self, connection_id: i64) -> CoreResult<()> {
        if self.portal_connection(connection_id)?.0 != ConnectorKind::SchwaebischHall {
            return Err(invalid());
        }
        crate::banking_gate::check_connector(GATE)
    }
}

#[uniffi::export]
impl Vault {
    /// Stores the contracts and bookings the browser script read from the portal.
    pub fn import_schwaebisch_hall_snapshot(
        self: Arc<Self>,
        connection_id: i64,
        snapshot_json: String,
    ) -> CoreResult<SyncSummary> {
        self.check_schwaebisch_hall(connection_id)?;
        if snapshot_json.len() > super::MAX_TEXT_BYTES {
            return Err(invalid());
        }
        let summary = store_fetch(
            &self,
            ConnectorKind::SchwaebischHall,
            connection_id,
            snapshot(&snapshot_json, &super::today())?,
        )?;
        let _ = self.checkpoint();
        Ok(summary)
    }

    /// Postbox attachments (`[{id, date, title, vtn?}]`) not stored yet, as
    /// validated JSON objects; known ids are skipped before any download, and
    /// so are those of a contract that is not imported (held back for the
    /// account choice, skipped, archived or deleted) – like FNZ.
    pub fn pending_schwaebisch_hall_documents(
        self: Arc<Self>,
        connection_id: i64,
        documents_json: String,
    ) -> CoreResult<Vec<String>> {
        self.check_schwaebisch_hall(connection_id)?;
        // document retrieval switched off for this connection: nothing to download
        if !super::documents_fetch_on(&*self.db()?, connection_id)? {
            return Ok(Vec::new());
        }
        if documents_json.len() > super::MAX_TEXT_BYTES {
            return Err(invalid());
        }
        let docs: Vec<Value> = serde_json::from_str(&documents_json).map_err(|_| invalid())?;
        let db = self.db()?;
        let mut known = db.prepare("SELECT 1 FROM booking_documents WHERE source_url = ?1")?;
        let mut seen = HashSet::new();
        let mut pending = Vec::new();
        for doc in docs {
            let id = attachment_id(&doc)?;
            if document_account(&db, connection_id, &doc)?.is_none() {
                continue;
            }
            if seen.insert(id.clone())
                && known
                    .query_row([document_source(connection_id, &id)], |_| Ok(()))
                    .optional()?
                    .is_none()
            {
                pending.push(doc.to_string());
            }
        }
        Ok(pending)
    }

    /// Stores one downloaded attachment (PDF only) with the contract it names,
    /// or – when it names none – the connection's first contract. Returns
    /// false if it was known.
    pub fn import_schwaebisch_hall_document(
        self: Arc<Self>,
        connection_id: i64,
        document_json: String,
        bytes: Vec<u8>,
    ) -> CoreResult<bool> {
        self.check_schwaebisch_hall(connection_id)?;
        if bytes.len() > super::MAX_DOC_BYTES || !bytes.starts_with(b"%PDF-") {
            return Err(invalid());
        }
        let doc: Value = serde_json::from_str(&document_json).map_err(|_| invalid())?;
        let id = attachment_id(&doc)?;
        let title = Some(text(&doc, "title"))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| tr("Schwäbisch Hall Dokument").into());
        let date = document_date(&doc["date"]).unwrap_or_else(super::today);
        let db = self.db()?;
        // A named contract that is not imported never falls back to another one.
        let account_id = document_account(&db, connection_id, &doc)?.ok_or_else(invalid)?;
        Ok(crate::documents::insert_doc(
            &db,
            None,
            Some(account_id),
            &title,
            "postbox",
            "application/pdf",
            &format!("SchwaebischHall_{}.pdf", URL_SAFE_NO_PAD.encode(&id)),
            &bytes,
            &document_source(connection_id, &id),
            &date,
        )?
        .is_some())
    }
}

/// Background syncs cannot sign in: the B2C login runs in the browser window.
pub struct WebOnlyConnector;

#[async_trait::async_trait]
impl super::BankConnector for WebOnlyConnector {
    async fn begin(&mut self, _ctx: &super::ConnectContext) -> CoreResult<super::Phase> {
        Err(CoreError::BrowserLoginRequired {
            message: tr(
                "Bitte bei Schwäbisch Hall im Browserfenster anmelden und die Daten abrufen.",
            )
            .into(),
        })
    }
    async fn respond(
        &mut self,
        ctx: &super::ConnectContext,
        _response: super::ChallengeResponse,
    ) -> CoreResult<super::Phase> {
        self.begin(ctx).await
    }
    async fn fetch(&mut self, ctx: &super::ConnectContext) -> CoreResult<FetchResult> {
        self.begin(ctx).await.map(|_| FetchResult::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const TODAY: &str = "2026-11-16";

    /// Made-up contract numbers and bookings in the portal's shape.
    fn contract(vtn: &str, ty: &str, balance: &str, transactions: Value) -> Value {
        json!({"type": ty, "bsn": "123456 A 01", "accountBalance": balance, "coreid": "123456",
               "contractletter": "A", "contractnumber": "01", "vtn": vtn,
               "details": {"contracttype": "Bausparvertrag", "contractphase": "Sparphase",
                           "transactions": transactions}})
    }

    fn parse(contracts: Value) -> CoreResult<FetchResult> {
        snapshot(
            &json!({"version": 1, "contracts": contracts}).to_string(),
            TODAY,
        )
    }

    #[test]
    fn amounts_with_entities_signs_and_currency() {
        assert_eq!(amount(&json!("&#43; 1.234,56")).unwrap(), 123_456);
        assert_eq!(amount(&json!("+ 0,00")).unwrap(), 0);
        assert_eq!(amount(&json!("- 50,00 €")).unwrap(), -5_000);
        assert_eq!(amount(&json!("\u{2212}12,30 EUR")).unwrap(), -1_230);
        assert_eq!(amount(&json!("&minus; 7,05")).unwrap(), -705);
        assert_eq!(amount(&json!(12.5)).unwrap(), 1_250);
        assert_eq!(amount(&json!("EUR 3,10")).unwrap(), 310);
        assert_eq!(
            amount(&json!("&#43;&nbsp;1&nbsp;000,00 EUR")).unwrap(),
            100_000
        );
        assert!(amount(&json!("")).is_err());
        assert!(amount(&json!("n/a")).is_err());
        // debit/credit markers must never be dropped (the sign would flip)
        for bad in [
            "12,00 S",
            "5,00 H",
            "1.234,56-",
            "+ -5,00",
            "--5,00",
            "5,00 USD",
        ] {
            assert!(amount(&json!(bad)).is_err(), "{bad}");
        }
        assert!(amount(&Value::Null).is_err());
    }

    #[test]
    fn month_names_and_numbers() {
        assert_eq!(month_of(&json!("März")), Some(3));
        assert_eq!(month_of(&json!("Dezember")), Some(12));
        assert_eq!(month_of(&json!("Okt.")), Some(10));
        assert_eq!(month_of(&json!("05")), Some(5));
        assert_eq!(month_of(&json!(11)), Some(11));
        assert_eq!(month_of(&json!("13")), None);
        assert_eq!(month_of(&json!("Saldo")), None);
        assert_eq!(month_of(&json!("Septemberzinsen")), None);
        assert_eq!(month_of(&json!("3")), Some(3));
        // only exact names: no words that merely start like a month
        for word in ["marketing", "Maintal", "Augen", "Junior", "decimal"] {
            assert_eq!(month_of(&json!(word)), None, "{word}");
            assert_eq!(month_of_key(word), None, "{word}");
        }
        // keys: names or two digits, never a bare list index
        assert_eq!(month_of_key("Mai"), Some(5));
        assert_eq!(month_of_key("07"), Some(7));
        assert_eq!(month_of_key("7"), None);
        assert_eq!(month_of_key("13"), None);
    }

    #[test]
    fn totals_and_bookings_without_subject_are_not_bookings() {
        let tx = json!({"2025": {"saldo": {"amount": "+ 9,00", "date": "31.12.2025"},
                                 "Summe": {"amount": "+ 9,00", "day": 31},
                                 "Mai": [{"day": 3, "subject": "Rate", "amount": "+ 9,00"}]}});
        let r = parse(json!([contract(
            "0012345600101",
            "Bausparvertrag",
            "+ 9,00",
            tx
        )]))
        .unwrap();
        assert_eq!(r.transactions.len(), 1);
        // an unknown amount+date object without subject stops the import
        let tx = json!({"2025": {"Mai": [{"day": 3, "amount": "+ 9,00"}]}});
        assert!(
            parse(json!([contract(
                "0012345600101",
                "Bausparvertrag",
                "+ 9,00",
                tx
            )]))
            .is_err()
        );
        // bookings under a bare list index have no month: rejected, not January
        let tx = json!({"2025": {"1": {"day": 3, "subject": "a", "amount": "+ 1,00"}}});
        assert!(
            parse(json!([contract(
                "0012345600101",
                "Bausparvertrag",
                "+ 1,00",
                tx
            )]))
            .is_err()
        );
    }

    #[test]
    fn contract_with_bookings_keyed_by_year_and_month() {
        let tx = json!({
            "2025": {"saldo": "&#43; 1.000,00",
                     "Dezember": [{"day": "31", "subject": "Zinsen", "amount": "&#43; 2,50"}],
                     "November": [{"day": 1, "subject": "Sparrate", "amount": "&#43; 50,00"},
                                  {"day": "01.", "subject": "Sparrate", "amount": "&#43; 50,00"}]},
            "2026": {"saldo": "&#43; 1.102,50",
                     "01": [{"day": "15", "subject": "Kontogebühr", "amount": "- 12,00"}]}
        });
        let r = parse(json!([contract(
            "0012345600101",
            "Bausparvertrag Sparphase",
            "&#43; 1.090,50",
            tx
        )]))
        .unwrap();
        assert_eq!(r.accounts.len(), 1);
        let a = &r.accounts[0];
        assert_eq!(a.external_ref, "bsh:0012345600101");
        assert_eq!(a.name, "Bausparvertrag 123456 A 01");
        assert_eq!(a.account_number, "123456 A 01");
        assert_eq!(a.kind, AccountKind::Savings);
        assert_eq!((a.balance_cents, a.balance_date.as_str()), (109_050, TODAY));
        let mut got: Vec<_> = r
            .transactions
            .iter()
            .map(|t| (t.booking_date.as_str(), t.amount_cents, t.purpose.as_str()))
            .collect();
        got.sort();
        assert_eq!(
            got,
            vec![
                ("2025-11-01", 5_000, "Sparrate"),
                ("2025-11-01", 5_000, "Sparrate"),
                ("2025-12-31", 250, "Zinsen"),
                ("2026-01-15", -1_200, "Kontogebühr"),
            ]
        );
        // no bank id: store_fetch numbers the two identical rates as two content ids
        assert!(r.transactions.iter().all(|t| t.external_id.is_empty()));
        assert!(
            r.transactions
                .iter()
                .all(|t| t.value_date == t.booking_date)
        );
    }

    /// The portal's real shape (anonymised sample from the issue author, #290):
    /// numeric contract type/phase, years as list items, month groups with
    /// `name`/`fullname` and the bookings in `entries`.
    fn portal_sample_details() -> Value {
        json!({"contracttype": 1, "contractphase": 1,
               "transactions": [{"year": 2026, "saldo": "&#43; 0,00", "month": [
                   {"name": "Nov", "fullname": "November", "entries": [
                       {"amount": "&#43; 50,00", "subject": "Einzug Zahlung", "day": 3}]},
                   {"name": "Okt", "fullname": "Oktober", "entries": [
                       {"amount": "&#43; 50,00", "subject": "Einzug Zahlung", "day": 2},
                       {"amount": "- 12,00", "subject": "Kontogebühr", "day": 30}]}]},
                   {"year": 2025, "saldo": "&#43; 0,00", "month": [
                       {"name": "Dez", "fullname": "Dezember", "entries": [
                           {"amount": "&#43; 1,23", "subject": "Zinsen", "day": 31}]}]}]})
    }

    #[test]
    fn portal_sample_months_in_name_and_fullname() {
        let mut c = contract(
            "0000000000000",
            "Bausparvertrag Sparphase",
            "&#43; 0,00",
            Value::Null,
        );
        c["details"] = portal_sample_details();
        let r = parse(json!([c])).unwrap();
        assert_eq!(r.accounts[0].kind, AccountKind::Savings);
        let got: Vec<_> = r
            .transactions
            .iter()
            .map(|t| (t.booking_date.as_str(), t.amount_cents, t.purpose.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("2026-11-03", 5_000, "Einzug Zahlung"),
                ("2026-10-02", 5_000, "Einzug Zahlung"),
                ("2026-10-30", -1_200, "Kontogebühr"),
                ("2025-12-31", 123, "Zinsen"),
            ]
        );
        // `fullname` alone and `name` alone are enough
        let mut only_full = portal_sample_details();
        only_full["transactions"][0]["month"][0]
            .as_object_mut()
            .unwrap()
            .remove("name");
        only_full["transactions"][0]["month"][1]
            .as_object_mut()
            .unwrap()
            .remove("fullname");
        let mut c = contract("0000000000000", "Bausparvertrag", "+ 0,00", Value::Null);
        c["details"] = only_full;
        assert_eq!(parse(json!([c])).unwrap().transactions.len(), 4);
    }

    #[test]
    fn portal_sample_loan_contract() {
        // Darlehen as reported by the issue author: type text, contracttype 3,
        // contractphase 1, negative balance.
        let mut c = contract(
            "0000000000001",
            "Tilgungsaussetzungsdarlehen",
            "- 1111,11",
            Value::Null,
        );
        c["details"] = json!({"contracttype": 3, "contractphase": 1, "transactions": []});
        let r = parse(json!([c])).unwrap();
        assert_eq!(r.accounts[0].kind, AccountKind::Loan);
        assert_eq!(r.accounts[0].balance_cents, -111_111);
        assert_eq!(
            r.accounts[0].name,
            "Tilgungsaussetzungsdarlehen 123456 A 01"
        );
    }

    #[test]
    fn bookings_in_arrays_of_groups_with_fields() {
        let tx = json!([
            {"year": 2024, "saldo": "&#43; 10,00", "months": [
                {"month": "Juni", "transactions": [{"day": 30, "subject": "Abschluss", "amount": "&#43; 10,00"}]}
            ]},
            {"year": "2026", "months": [
                {"month": 2, "entries": [{"day": "28", "subject": "Rate", "amount": "&#43; 25,00"}]}
            ]}
        ]);
        let r = parse(json!([contract(
            "0012345600101",
            "Bausparvertrag",
            "&#43; 35,00",
            tx
        )]))
        .unwrap();
        let dates: Vec<_> = r
            .transactions
            .iter()
            .map(|t| t.booking_date.as_str())
            .collect();
        assert_eq!(dates, vec!["2024-06-30", "2026-02-28"]);
    }

    #[test]
    fn full_dates_in_bookings_win() {
        let tx = json!({"2025": {"Mai": [
            {"day": "02.05.2025", "subject": "A", "amount": "&#43; 1,00"},
            {"date": "2025-05-03", "subject": "B", "amount": "&#43; 2,00"}
        ]}});
        let r = parse(json!([contract(
            "0012345600101",
            "Bausparvertrag",
            "+ 3,00",
            tx
        )]))
        .unwrap();
        let dates: Vec<_> = r
            .transactions
            .iter()
            .map(|t| t.booking_date.as_str())
            .collect();
        assert_eq!(dates, vec!["2025-05-02", "2025-05-03"]);
    }

    #[test]
    fn loan_phase_is_a_loan_with_negative_balance() {
        let r = parse(json!([contract(
            "0099999900201",
            "Bauspardarlehen",
            "&#43; 20.000,00",
            Value::Null
        )]))
        .unwrap();
        assert_eq!(r.accounts[0].kind, AccountKind::Loan);
        assert_eq!(r.accounts[0].balance_cents, -2_000_000);
        assert!(r.transactions.is_empty());
    }

    #[test]
    fn several_contracts_and_duplicate_vtn() {
        let c1 = contract(
            "0012345600101",
            "Bausparvertrag Sparphase",
            "+ 1,00",
            json!({}),
        );
        let c2 = contract(
            "0012345600202",
            "Bausparvertrag Sparphase",
            "+ 2,00",
            json!([]),
        );
        let r = parse(json!([c1.clone(), c2, c1])).unwrap();
        let refs: Vec<_> = r.accounts.iter().map(|a| a.external_ref.as_str()).collect();
        assert_eq!(refs, vec!["bsh:0012345600101", "bsh:0012345600202"]);
    }

    #[test]
    fn incomplete_responses_import_nothing() {
        let ok = |tx: Value| contract("0012345600101", "Bausparvertrag", "+ 1,00", tx);
        // no contracts, wrong version, bad vtn, missing details
        assert!(parse(json!([])).is_err());
        assert!(snapshot(r#"{"version":2,"contracts":[]}"#, TODAY).is_err());
        assert!(snapshot("not json", TODAY).is_err());
        let mut bad = ok(Value::Null);
        bad["vtn"] = json!("12; DROP");
        assert!(parse(json!([bad])).is_err());
        let mut no_details = ok(Value::Null);
        no_details["details"] = Value::Null;
        assert!(parse(json!([no_details])).is_err());
        // a booking without year/month, with an impossible date or a broken amount
        assert!(
            parse(json!([ok(
                json!([{"day": 1, "subject": "x", "amount": "+ 1,00"}])
            )]))
            .is_err()
        );
        assert!(
            parse(json!([ok(
                json!({"2025": {"Februar": [{"day": 30, "amount": "+ 1,00"}]}})
            )]))
            .is_err()
        );
        assert!(
            parse(json!([ok(
                json!({"2025": {"Mai": [{"day": 3, "amount": "viel"}]}})
            )]))
            .is_err()
        );
        assert!(parse(json!([ok(json!("2025"))])).is_err());
    }

    #[test]
    fn import_is_idempotent_and_documents_go_to_the_named_contract() {
        let dir = crate::test_support::TempDir::new("schwaebisch_hall");
        let vault =
            Vault::open(dir.join("vault.db").to_string_lossy().into(), vec![5; 32]).unwrap();
        let connection = vault
            .add_bank_connection(ConnectorKind::SchwaebischHall, "BSH".into(), "test".into())
            .unwrap();
        let tx = json!({"2026": {"März": [
            {"day": "1", "subject": "Sparrate", "amount": "&#43; 50,00"},
            {"day": "1", "subject": "Sparrate", "amount": "&#43; 50,00"}
        ]}});
        let snap = json!({"version": 1, "contracts": [
            contract("0012345600101", "Bausparvertrag Sparphase", "&#43; 100,00", tx),
            contract("0012345600202", "Bausparvertrag Sparphase", "&#43; 0,00", Value::Null)
        ]})
        .to_string();
        let first = vault
            .clone()
            .import_schwaebisch_hall_snapshot(connection, snap.clone())
            .unwrap();
        assert_eq!(first.transactions_imported, 2);
        let second = vault
            .clone()
            .import_schwaebisch_hall_snapshot(connection, snap)
            .unwrap();
        assert_eq!(second.transactions_imported, 0);
        let accounts = vault.list_accounts().unwrap();
        assert_eq!(accounts.len(), 2);
        assert!(accounts.iter().any(|a| a.balance_cents == 10_000));

        let doc = json!({"id": "att-1", "date": "02.03.2026", "title": "Jahreskontoauszug",
                         "vtn": "0012345600202"})
        .to_string();
        let list = format!("[{doc},{doc}]");
        let pending = vault
            .clone()
            .pending_schwaebisch_hall_documents(connection, list.clone())
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert!(
            vault
                .clone()
                .import_schwaebisch_hall_document(connection, doc.clone(), b"<html>".to_vec())
                .is_err()
        );
        assert!(
            vault
                .clone()
                .import_schwaebisch_hall_document(
                    connection,
                    pending[0].clone(),
                    b"%PDF-1.7 x".to_vec()
                )
                .unwrap()
        );
        assert!(
            !vault
                .clone()
                .import_schwaebisch_hall_document(connection, doc, b"%PDF-1.7 x".to_vec())
                .unwrap()
        );
        assert!(
            vault
                .clone()
                .pending_schwaebisch_hall_documents(connection, list)
                .unwrap()
                .is_empty()
        );
        let docs = vault.list_all_documents(Default::default()).unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(
            (docs[0].title.as_str(), docs[0].doc_date.as_str()),
            ("Jahreskontoauszug", "2026-03-02")
        );
        let named = accounts.iter().find(|a| a.balance_cents == 0).map(|a| a.id);
        assert_eq!(docs[0].account_id, named);

        // A contract that is not imported (held back, skipped, archived) gets
        // none of its documents, and they never fall back to another contract.
        let unknown = json!({"id": "att-2", "title": "Fremd", "vtn": "0099999900101"}).to_string();
        assert!(
            vault
                .clone()
                .pending_schwaebisch_hall_documents(connection, format!("[{unknown}]"))
                .unwrap()
                .is_empty()
        );
        assert!(
            vault
                .clone()
                .import_schwaebisch_hall_document(connection, unknown, b"%PDF-1.7 x".to_vec())
                .is_err()
        );
        vault.set_account_archived(named.unwrap(), true).unwrap();
        let archived = json!({"id": "att-3", "vtn": "0012345600202"}).to_string();
        assert!(
            vault
                .clone()
                .pending_schwaebisch_hall_documents(connection, format!("[{archived}]"))
                .unwrap()
                .is_empty()
        );
        // without a named contract: the first imported one
        let plain = json!({"id": "att-4", "date": 1_767_225_600_i64}).to_string();
        assert_eq!(
            vault
                .clone()
                .pending_schwaebisch_hall_documents(connection, format!("[{plain}]"))
                .unwrap()
                .len(),
            1
        );
        // malformed vtn, oversized input
        let bad = json!({"id": "att-5", "vtn": "1 OR 1=1"}).to_string();
        assert!(
            vault
                .clone()
                .pending_schwaebisch_hall_documents(connection, format!("[{bad}]"))
                .is_err()
        );
        let mut huge = b"%PDF-".to_vec();
        huge.resize(super::super::MAX_DOC_BYTES + 1, b' ');
        assert!(
            vault
                .clone()
                .import_schwaebisch_hall_document(connection, plain, huge)
                .is_err()
        );
        assert!(
            vault
                .clone()
                .import_schwaebisch_hall_snapshot(
                    connection,
                    " ".repeat(super::super::MAX_TEXT_BYTES + 1)
                )
                .is_err()
        );

        // another connector's login is refused
        let wrong = vault
            .add_bank_connection(ConnectorKind::PayPal, "Other".into(), "test".into())
            .unwrap();
        assert!(
            vault
                .clone()
                .import_schwaebisch_hall_snapshot(wrong, "{}".into())
                .is_err()
        );
    }

    #[test]
    fn unnamed_documents_skip_paid_out_contracts() {
        let dir = crate::test_support::TempDir::new("schwaebisch_hall_revoked");
        let vault =
            Vault::open(dir.join("vault.db").to_string_lossy().into(), vec![8; 32]).unwrap();
        let connection = vault
            .add_bank_connection(ConnectorKind::SchwaebischHall, "BSH".into(), "test".into())
            .unwrap();
        let import = |contracts: Value| {
            vault
                .clone()
                .import_schwaebisch_hall_snapshot(
                    connection,
                    json!({"version": 1, "contracts": contracts}).to_string(),
                )
                .unwrap()
        };
        // the first contract (lowest id) is paid out and no longer delivered
        let first = contract("0012345600101", "Bausparvertrag", "+ 1,00", Value::Null);
        let mut second = contract("0012345600202", "Bausparvertrag", "+ 2,00", Value::Null);
        second["bsn"] = json!("123456 A 02");
        import(json!([first, second.clone()]));
        assert_eq!(import(json!([second])).revoked_accounts.len(), 1);
        let doc = json!({"id": "att-x", "title": "Info"}).to_string();
        assert!(
            vault
                .clone()
                .import_schwaebisch_hall_document(connection, doc, b"%PDF-1.7 x".to_vec())
                .unwrap()
        );
        let docs = vault.list_all_documents(Default::default()).unwrap();
        let delivered = vault
            .list_accounts()
            .unwrap()
            .into_iter()
            .find(|a| !a.access_revoked)
            .map(|a| a.id);
        assert_eq!(docs[0].account_id, delivered);
    }

    #[test]
    fn loan_phase_turns_the_stored_savings_account_into_a_loan() {
        let dir = crate::test_support::TempDir::new("schwaebisch_hall_phase");
        let vault =
            Vault::open(dir.join("vault.db").to_string_lossy().into(), vec![6; 32]).unwrap();
        let connection = vault
            .add_bank_connection(ConnectorKind::SchwaebischHall, "BSH".into(), "test".into())
            .unwrap();
        let import = |contracts: Value| {
            vault
                .clone()
                .import_schwaebisch_hall_snapshot(
                    connection,
                    json!({"version": 1, "contracts": contracts}).to_string(),
                )
                .unwrap()
        };
        let other = contract(
            "0012345600202",
            "Bausparvertrag Sparphase",
            "+ 5,00",
            Value::Null,
        );
        import(json!([
            contract(
                "0012345600101",
                "Bausparvertrag Sparphase",
                "+ 40.000,00",
                Value::Null
            ),
            other
        ]));
        let mut loan = contract(
            "0012345600101",
            "Bausparvertrag Darlehensphase",
            "&#43; 30.000,00",
            Value::Null,
        );
        loan["details"]["contractphase"] = json!("Darlehensphase");
        // the second contract was paid out: no longer in the list
        let summary = import(json!([loan]));
        assert_eq!(summary.revoked_accounts.len(), 1);
        let accounts = vault.list_accounts().unwrap();
        let a = accounts
            .iter()
            .find(|a| a.account_number == "123456 A 01" && !a.access_revoked)
            .unwrap();
        assert_eq!(a.kind, AccountKind::Loan);
        assert_eq!(a.balance_cents, -3_000_000);
        assert_eq!(a.name, "Bausparvertrag 123456 A 01");
        assert_eq!(accounts.iter().filter(|a| a.access_revoked).count(), 1);
    }

    #[test]
    fn contract_names_drop_the_phase() {
        assert_eq!(contract_label("Bausparvertrag Sparphase"), "Bausparvertrag");
        assert_eq!(
            contract_label("Wohn-Riester Bausparvertrag Darlehensphase"),
            "Wohn-Riester Bausparvertrag"
        );
        assert_eq!(contract_label(""), "Bausparvertrag");
        assert_eq!(contract_label("Sparphase"), "Bausparvertrag");
    }

    #[test]
    fn document_dates_and_ids() {
        assert_eq!(
            document_date(&json!("31.12.2025")).as_deref(),
            Some("2025-12-31")
        );
        assert_eq!(
            document_date(&json!("2026-01-02T08:00:00")).as_deref(),
            Some("2026-01-02")
        );
        assert_eq!(
            document_date(&json!(1_767_225_600_000_i64)).as_deref(),
            Some("2026-01-01")
        );
        // Unix seconds, not 1970
        assert_eq!(
            document_date(&json!(1_767_225_600_i64)).as_deref(),
            Some("2026-01-01")
        );
        assert_eq!(document_date(&json!("gestern")), None);
        assert!(attachment_id(&json!({"id": "AB-12_x"})).is_ok());
        assert!(attachment_id(&json!({"id": ""})).is_err());
        assert!(attachment_id(&json!({"id": "a\nb"})).is_err());
        assert!(attachment_id(&json!({"id": 5})).is_err());
        assert_ne!(document_source(1, "a"), document_source(2, "a"));
    }
}
