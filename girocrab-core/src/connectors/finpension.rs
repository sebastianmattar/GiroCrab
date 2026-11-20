//! Finpension (Switzerland): pillar 3a, vested benefits (Freizügigkeit) and 1e
//! portfolios. Pure parser for the portfolio holdings and cash transactions
//! of a Finpension export (CSV or JSON). It does no network access; every URL
//! that is accepted for a download must pass [`validate_finpension_url`].
//!
//! CSV: header row, `;` or `,` separated, quoted fields allowed, German or
//! English column names. Amounts may use Swiss notation (`1'234.50`).
//! JSON: `{"holdings": [...], "transactions": [...]}` (`positions` is accepted
//! for `holdings`), keys in camelCase or snake_case.
use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;

use super::{ContentIds, RemoteHolding, RemoteTransaction, SyncSummary, persist::store_fetch};
use crate::{
    error::{CoreError, CoreResult},
    i18n::tr,
    models::{ConnectorKind, Holding},
    vault::Vault,
};

fn invalid(message: impl Into<String>) -> CoreError {
    CoreError::invalid(format!("Finpension: {}", message.into()))
}

/// Host every Finpension URL must belong to.
const HOST: &str = "finpension.ch";

/// Accepts only `https://finpension.ch/…` or `https://<sub>.finpension.ch/…`
/// (port 443 at most). Rejects other schemes, credentials in the URL,
/// look-alike hosts (`finpension.ch.evil.com`, `evilfinpension.ch`), IP
/// addresses, backslashes, whitespace and control characters.
pub fn validate_finpension_url(url: &str) -> CoreResult<()> {
    let bad = || invalid("only https URLs on finpension.ch are allowed");
    if url
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || c == '\\')
        || !url.is_ascii()
    {
        return Err(bad());
    }
    let rest = url
        .get(..8)
        .filter(|p| p.eq_ignore_ascii_case("https://"))
        .map(|_| &url[8..])
        .ok_or_else(bad)?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains('@') {
        return Err(bad());
    }
    let host = match authority.rsplit_once(':') {
        Some((h, "443")) => h,
        Some(_) => return Err(bad()),
        None => authority,
    };
    let host = host.to_ascii_lowercase();
    let ok = host == HOST
        || host
            .strip_suffix(".finpension.ch")
            .is_some_and(|sub| !sub.is_empty() && sub.split('.').all(valid_label));
    if ok { Ok(()) } else { Err(bad()) }
}

fn valid_label(l: &str) -> bool {
    !l.is_empty()
        && !l.starts_with('-')
        && !l.ends_with('-')
        && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// Result of parsing an export.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct FinpensionExport {
    pub holdings: Vec<FinpensionHolding>,
    pub transactions: Vec<FinpensionTransaction>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FinpensionHolding {
    pub isin: String,
    pub name: String,
    pub quantity: f64,
    pub price_cents: i64,
    pub value_cents: i64,
    /// `CHF` or `EUR`.
    pub currency: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FinpensionTransaction {
    /// `YYYY-MM-DD`.
    pub date: String,
    pub description: String,
    pub amount_cents: i64,
    pub currency: String,
    pub reference: String,
}

impl FinpensionHolding {
    pub fn into_remote(self, account_ref: &str) -> RemoteHolding {
        RemoteHolding {
            account_ref: account_ref.into(),
            holding: Holding {
                isin: self.isin,
                name: self.name,
                quantity: self.quantity,
                current_price_cents: self.price_cents,
                value_cents: self.value_cents,
                currency: self.currency,
                ..Default::default()
            },
            ..Default::default()
        }
    }
}

/// Converts transactions to connector rows with stable content ids.
pub fn remote_transactions(
    account_ref: &str,
    transactions: &[FinpensionTransaction],
) -> Vec<RemoteTransaction> {
    let mut ids = ContentIds::default();
    transactions
        .iter()
        .map(|t| {
            let mut r = RemoteTransaction {
                account_ref: account_ref.into(),
                booking_date: t.date.clone(),
                value_date: t.date.clone(),
                amount_cents: t.amount_cents,
                currency: t.currency.clone(),
                purpose: t.description.clone(),
                end_to_end_id: t.reference.clone(),
                ..Default::default()
            };
            r.external_id = ids.next(&r);
            r
        })
        .collect()
}

/// Finpension has no API: the export file is imported through
/// [`Vault::import_finpension_export`], a sync only points the user there.
pub struct FinpensionConnector;

#[async_trait::async_trait]
impl super::BankConnector for FinpensionConnector {
    async fn begin(&mut self, _ctx: &super::ConnectContext) -> CoreResult<super::Phase> {
        Err(invalid(tr(
            "Bitte die Exportdatei über „Datei importieren“ einlesen.",
        )))
    }
    async fn respond(
        &mut self,
        ctx: &super::ConnectContext,
        _response: super::ChallengeResponse,
    ) -> CoreResult<super::Phase> {
        self.begin(ctx).await
    }
    async fn fetch(&mut self, ctx: &super::ConnectContext) -> CoreResult<super::FetchResult> {
        self.begin(ctx).await.map(|_| Default::default())
    }
}

/// Builds the fetch of one import: a depot (holdings) and a cash account
/// (transactions) per currency present in the export.
pub fn to_fetch(connection_id: i64, name: &str, export: FinpensionExport) -> super::FetchResult {
    use super::{FetchResult, RemoteAccount};
    use crate::models::AccountKind;
    let mut out = FetchResult::default();
    let mut currencies: Vec<String> = export
        .holdings
        .iter()
        .map(|h| h.currency.clone())
        .chain(export.transactions.iter().map(|t| t.currency.clone()))
        .collect();
    currencies.sort();
    currencies.dedup();
    let multi = currencies.len() > 1;
    for cur in currencies {
        let label = |what: &str| {
            if multi {
                format!("{name} {what} ({cur})")
            } else {
                format!("{name} {what}")
            }
        };
        let holdings: Vec<_> = export
            .holdings
            .iter()
            .filter(|h| h.currency == cur)
            .collect();
        if !holdings.is_empty() {
            let depot_ref = format!("finpension-{connection_id}-depot-{cur}");
            out.accounts.push(RemoteAccount {
                external_ref: depot_ref.clone(),
                name: label("Depot"),
                currency: cur.clone(),
                kind: AccountKind::Brokerage,
                balance_cents: holdings.iter().map(|h| h.value_cents).sum(),
                balance_date: crate::dates::today_iso(),
                owner: String::new(),
                iban: String::new(),
                account_number: String::new(),
            });
            out.holdings_fetched.push(depot_ref.clone());
            out.balances_reported.push(depot_ref.clone());
            out.holdings.extend(
                holdings
                    .into_iter()
                    .cloned()
                    .map(|h| h.into_remote(&depot_ref)),
            );
        }
        let txs: Vec<_> = export
            .transactions
            .iter()
            .filter(|t| t.currency == cur)
            .cloned()
            .collect();
        if !txs.is_empty() {
            let cash_ref = format!("finpension-{connection_id}-cash-{cur}");
            out.accounts.push(RemoteAccount {
                external_ref: cash_ref.clone(),
                name: label("Cash"),
                currency: cur.clone(),
                kind: AccountKind::Cash,
                balance_cents: 0,
                balance_date: crate::dates::today_iso(),
                owner: String::new(),
                iban: String::new(),
                account_number: String::new(),
            });
            out.transactions
                .extend(remote_transactions(&cash_ref, &txs));
            // an export carries no cash balance: add the bookings to the stored one
            out.adjust_balances = true;
        }
    }
    out
}

#[uniffi::export]
impl Vault {
    /// Imports a Finpension CSV/JSON export (holdings and cash transactions)
    /// into the connection. Local only, no network access.
    pub fn import_finpension_export(
        self: Arc<Self>,
        connection_id: i64,
        bytes: Vec<u8>,
    ) -> Result<SyncSummary, CoreError> {
        let (kind, name) = self.portal_connection(connection_id)?;
        if kind != ConnectorKind::Finpension {
            return Err(invalid(tr("Die Verbindung gehört nicht zu Finpension.")));
        }
        let text = crate::text::decode_bank_bytes(&bytes);
        let export = parse_export(&text)?;
        if export.holdings.is_empty() && export.transactions.is_empty() {
            return Err(invalid(tr("Keine Positionen oder Buchungen erkannt.")));
        }
        store_fetch(
            &self,
            kind,
            connection_id,
            to_fetch(connection_id, &name, export),
        )
    }
}

/// Parses an export; JSON when it starts with `{` or `[`, otherwise CSV.
/// A CSV is read as holdings when it has an ISIN column, else as transactions.
pub fn parse_export(input: &str) -> CoreResult<FinpensionExport> {
    let input = input.trim_start_matches('\u{feff}').trim();
    if input.starts_with('{') || input.starts_with('[') {
        parse_json(input)
    } else {
        let rows = parse_csv_rows(input)?;
        let is_holdings = rows
            .first()
            .is_some_and(|h| h.iter().any(|c| matches!(norm(c).as_str(), "isin")));
        let mut out = FinpensionExport::default();
        if is_holdings {
            out.holdings = holdings_from_csv_rows(&rows)?;
        } else {
            out.transactions = transactions_from_csv_rows(&rows)?;
        }
        Ok(out)
    }
}

pub fn parse_holdings_csv(csv: &str) -> CoreResult<Vec<FinpensionHolding>> {
    holdings_from_csv_rows(&parse_csv_rows(csv.trim_start_matches('\u{feff}'))?)
}

pub fn parse_transactions_csv(csv: &str) -> CoreResult<Vec<FinpensionTransaction>> {
    transactions_from_csv_rows(&parse_csv_rows(csv.trim_start_matches('\u{feff}'))?)
}

// ---------- CSV ----------

fn parse_csv_rows(csv: &str) -> CoreResult<Vec<Vec<String>>> {
    let first = csv.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let delim = if first.matches(';').count() >= first.matches(',').count() && first.contains(';') {
        ';'
    } else {
        ','
    };
    let mut rows = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = csv.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            c if c == delim && !quoted => row.push(std::mem::take(&mut field)),
            '\r' if !quoted => {}
            '\n' if !quoted => {
                row.push(std::mem::take(&mut field));
                push_row(&mut rows, std::mem::take(&mut row));
            }
            c => field.push(c),
        }
    }
    if quoted {
        return Err(invalid("unterminated quote in CSV"));
    }
    row.push(field);
    push_row(&mut rows, row);
    if rows.len() < 2 {
        return Err(invalid("CSV has no data rows"));
    }
    Ok(rows)
}

fn push_row(rows: &mut Vec<Vec<String>>, row: Vec<String>) {
    if row.iter().any(|c| !c.trim().is_empty()) {
        rows.push(row);
    }
}

fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

struct Columns(HashMap<String, usize>);

impl Columns {
    fn new(header: &[String]) -> Self {
        let mut m = HashMap::new();
        for (i, h) in header.iter().enumerate() {
            m.entry(norm(h)).or_insert(i);
        }
        Columns(m)
    }
    fn find(&self, names: &[&str]) -> Option<usize> {
        names.iter().find_map(|n| self.0.get(*n).copied())
    }
    fn require(&self, names: &[&str]) -> CoreResult<usize> {
        self.find(names)
            .ok_or_else(|| invalid(format!("missing column {}", names[0])))
    }
}

fn cell(row: &[String], i: usize) -> &str {
    row.get(i).map(|s| s.trim()).unwrap_or("")
}

fn holdings_from_csv_rows(rows: &[Vec<String>]) -> CoreResult<Vec<FinpensionHolding>> {
    let c = Columns::new(&rows[0]);
    let isin = c.require(&["isin"])?;
    let name = c.require(&["name", "bezeichnung", "wertschrift", "security"])?;
    let qty = c.require(&["quantity", "anzahl", "menge", "stueck", "stück"])?;
    let price = c.require(&["price", "kurs", "preis", "preisprostueck"])?;
    let value = c.require(&["marketvalue", "marktwert", "value", "wert"])?;
    let cur = c.find(&["currency", "währung", "waehrung", "whg"]);
    rows[1..]
        .iter()
        .map(|r| {
            build_holding(
                cell(r, isin),
                cell(r, name),
                parse_decimal(cell(r, qty))?,
                parse_decimal(cell(r, price))?,
                parse_decimal(cell(r, value))?,
                cur.map(|i| cell(r, i)).unwrap_or(""),
            )
        })
        .collect()
}

fn transactions_from_csv_rows(rows: &[Vec<String>]) -> CoreResult<Vec<FinpensionTransaction>> {
    let c = Columns::new(&rows[0]);
    let date = c.require(&["date", "datum", "buchungsdatum", "valuta"])?;
    let desc = c.require(&[
        "description",
        "beschreibung",
        "text",
        "buchungstext",
        "verwendungszweck",
    ])?;
    let amount = c.require(&["amount", "betrag"])?;
    let cur = c.find(&["currency", "währung", "waehrung", "whg"]);
    let reference = c.find(&["reference", "referenz", "id"]);
    rows[1..]
        .iter()
        .map(|r| {
            build_transaction(
                cell(r, date),
                cell(r, desc),
                parse_decimal(cell(r, amount))?,
                cur.map(|i| cell(r, i)).unwrap_or(""),
                reference.map(|i| cell(r, i)).unwrap_or(""),
            )
        })
        .collect()
}

// ---------- JSON ----------

fn parse_json(input: &str) -> CoreResult<FinpensionExport> {
    let root: Value = serde_json::from_str(input).map_err(|_| invalid("invalid JSON"))?;
    let (holdings, transactions) = match &root {
        Value::Array(_) => (None, Some(&root)),
        Value::Object(o) => (
            o.get("holdings").or_else(|| o.get("positions")),
            o.get("transactions"),
        ),
        _ => return Err(invalid("unexpected JSON structure")),
    };
    let mut out = FinpensionExport::default();
    for h in rows_of(holdings)? {
        out.holdings.push(build_holding(
            str_of(h, &["isin"]),
            str_of(h, &["name"]),
            num_of(h, &["quantity", "amount"])?,
            num_of(h, &["price", "currentPrice", "current_price"])?,
            num_of(h, &["marketValue", "market_value", "value"])?,
            str_of(h, &["currency"]),
        )?);
    }
    for t in rows_of(transactions)? {
        out.transactions.push(build_transaction(
            str_of(t, &["date", "bookingDate", "booking_date"]),
            str_of(t, &["description", "text"]),
            num_of(t, &["amount"])?,
            str_of(t, &["currency"]),
            str_of(t, &["reference", "id"]),
        )?);
    }
    if out.holdings.is_empty() && out.transactions.is_empty() {
        return Err(invalid("export contains no holdings or transactions"));
    }
    Ok(out)
}

fn rows_of(v: Option<&Value>) -> CoreResult<&[Value]> {
    match v {
        None | Some(Value::Null) => Ok(&[]),
        Some(Value::Array(a)) => Ok(a),
        Some(_) => Err(invalid("expected a list")),
    }
}

fn str_of<'a>(v: &'a Value, keys: &[&str]) -> &'a str {
    keys.iter()
        .find_map(|k| v.get(*k).and_then(Value::as_str))
        .unwrap_or("")
        .trim()
}

/// JSON numbers or numeric strings.
fn num_of(v: &Value, keys: &[&str]) -> CoreResult<f64> {
    let x = keys
        .iter()
        .find_map(|k| v.get(*k))
        .ok_or_else(|| invalid(format!("missing field {}", keys[0])))?;
    match x {
        Value::Number(n) => n.as_f64().ok_or_else(|| invalid("invalid number")),
        Value::String(s) => parse_decimal(s),
        _ => Err(invalid("invalid number")),
    }
}

// ---------- shared validation ----------

/// Parses `1'234.50`, `1’234.50`, `1 234,50`, `1.234,50`, `-12.5`.
/// The last of `.`/`,` is the decimal separator; a lone `,` is decimal.
fn parse_decimal(s: &str) -> CoreResult<f64> {
    let t: String = s
        .chars()
        .filter(|c| !matches!(c, '\'' | '’' | '‘' | ' ' | '\u{a0}' | '\u{202f}'))
        .collect();
    let normalized = match (t.rfind('.'), t.rfind(',')) {
        (Some(d), Some(c)) if c > d => t.replace('.', "").replace(',', "."),
        (Some(_), Some(_)) => t.replace(',', ""),
        (None, Some(_)) => t.replace(',', "."),
        _ => t,
    };
    normalized
        .parse::<f64>()
        .ok()
        .filter(|n| n.is_finite() && n.abs() < 1e12)
        .ok_or_else(|| invalid(format!("invalid number '{s}'")))
}

fn cents(x: f64) -> i64 {
    (x * 100.0).round() as i64
}

fn currency(s: &str) -> CoreResult<String> {
    let c = if s.is_empty() { "CHF" } else { s };
    match c.to_ascii_uppercase().as_str() {
        "CHF" => Ok("CHF".into()),
        "EUR" => Ok("EUR".into()),
        _ => Err(invalid(format!("unsupported currency '{c}'"))),
    }
}

fn valid_isin(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 12
        && b[..2].iter().all(u8::is_ascii_uppercase)
        && b[2..11]
            .iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        && b[11].is_ascii_digit()
}

fn build_holding(
    isin: &str,
    name: &str,
    quantity: f64,
    price: f64,
    value: f64,
    cur: &str,
) -> CoreResult<FinpensionHolding> {
    let isin = isin.to_ascii_uppercase();
    if !valid_isin(&isin) {
        return Err(invalid(format!("invalid ISIN '{isin}'")));
    }
    if name.is_empty() {
        return Err(invalid(format!("missing name for {isin}")));
    }
    if quantity < 0.0 || price < 0.0 || value < 0.0 {
        return Err(invalid(format!("negative figures for {isin}")));
    }
    Ok(FinpensionHolding {
        isin,
        name: name.into(),
        quantity,
        price_cents: cents(price),
        value_cents: cents(value),
        currency: currency(cur)?,
    })
}

fn build_transaction(
    date: &str,
    description: &str,
    amount: f64,
    cur: &str,
    reference: &str,
) -> CoreResult<FinpensionTransaction> {
    Ok(FinpensionTransaction {
        date: parse_date(date)?,
        description: description.into(),
        amount_cents: cents(amount),
        currency: currency(cur)?,
        reference: reference.into(),
    })
}

/// `YYYY-MM-DD` (a time suffix is ignored) or `DD.MM.YYYY`.
fn parse_date(s: &str) -> CoreResult<String> {
    let s = s.trim();
    let parsed = s
        .get(..10)
        .and_then(|p| jiff::civil::Date::strptime("%Y-%m-%d", p).ok())
        .or_else(|| jiff::civil::Date::strptime("%d.%m.%Y", s).ok());
    parsed
        .map(crate::dates::iso)
        .ok_or_else(|| invalid(format!("invalid date '{s}'")))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOLDINGS_CSV: &str = "\u{feff}ISIN;Name;Anzahl;Kurs;Marktwert;Währung\n\
CH0000000011;\"Synthetic Equity Fund; World\";12.5;100.20;1'252.50;CHF\n\
IE0000000022;Synthetic Bond Fund;3;49.99;149.97;EUR\n";

    const TX_CSV: &str = "Datum,Beschreibung,Betrag,Währung,Referenz\n\
15.03.2026,\"Einzahlung Säule 3a, März\",\"6'000.00\",CHF,R-1\n\
2026-03-31,Verwaltungsgebühr,-12.34,CHF,R-2\n";

    #[test]
    fn urls_accepted() {
        for u in [
            "https://finpension.ch/",
            "https://finpension.ch/export?x=1",
            "HTTPS://app.finpension.ch/api/v1",
            "https://finpension.ch:443/a",
            "https://a.b.finpension.ch#frag",
        ] {
            assert!(validate_finpension_url(u).is_ok(), "{u}");
        }
    }

    #[test]
    fn urls_rejected() {
        for u in [
            "http://finpension.ch/",
            "https://finpension.com/",
            "https://finpension.ch.evil.com/",
            "https://evilfinpension.ch/",
            "https://evil.com/finpension.ch",
            "https://evil.com?u=finpension.ch",
            "https://finpension.ch@evil.com/",
            "https://evil.com@finpension.ch/",
            "https://finpension.ch:8443/",
            "https://finpension.ch\\@evil.com/",
            "https://.finpension.ch/",
            "https://-a.finpension.ch/",
            "https://fin pension.ch/",
            "https://finpensión.ch/",
            "ftp://finpension.ch/",
            "//finpension.ch/",
            "finpension.ch",
            "",
        ] {
            assert!(validate_finpension_url(u).is_err(), "{u}");
        }
    }

    #[test]
    fn holdings_csv() {
        let h = parse_holdings_csv(HOLDINGS_CSV).unwrap();
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].isin, "CH0000000011");
        assert_eq!(h[0].name, "Synthetic Equity Fund; World");
        assert_eq!(h[0].quantity, 12.5);
        assert_eq!(h[0].price_cents, 10020);
        assert_eq!(h[0].value_cents, 125250);
        assert_eq!(h[1].currency, "EUR");
        let r = h[1].clone().into_remote("depot:1");
        assert_eq!(r.account_ref, "depot:1");
        assert_eq!(r.holding.value_cents, 14997);
    }

    #[test]
    fn transactions_csv_and_ids() {
        let t = parse_transactions_csv(TX_CSV).unwrap();
        assert_eq!(t[0].date, "2026-03-15");
        assert_eq!(t[0].description, "Einzahlung Säule 3a, März");
        assert_eq!(t[0].amount_cents, 600_000);
        assert_eq!(t[1].amount_cents, -1234);
        let r = remote_transactions("cash:1", &t);
        assert_eq!(r.len(), 2);
        assert_ne!(r[0].external_id, r[1].external_id);
        assert_eq!(
            r[0].external_id,
            remote_transactions("cash:1", &t)[0].external_id
        );
    }

    #[test]
    fn identical_transactions_get_distinct_ids() {
        let t = FinpensionTransaction {
            date: "2026-01-01".into(),
            description: "Gebühr".into(),
            amount_cents: -100,
            currency: "CHF".into(),
            reference: String::new(),
        };
        let r = remote_transactions("c", &[t.clone(), t]);
        assert_ne!(r[0].external_id, r[1].external_id);
    }

    #[test]
    fn export_detects_kind() {
        assert_eq!(parse_export(HOLDINGS_CSV).unwrap().holdings.len(), 2);
        assert_eq!(parse_export(TX_CSV).unwrap().transactions.len(), 2);
    }

    #[test]
    fn json_export() {
        let j = r#"{"positions":[{"isin":"ch0000000011","name":"Fund","quantity":"2",
            "currentPrice":10.005,"marketValue":20.01,"currency":"chf"}],
            "transactions":[{"bookingDate":"2026-02-01T10:00:00Z","description":"Zins",
            "amount":1.5,"currency":"CHF","id":"x"}]}"#;
        let e = parse_export(j).unwrap();
        assert_eq!(e.holdings[0].isin, "CH0000000011");
        assert_eq!(e.holdings[0].currency, "CHF");
        assert_eq!(e.holdings[0].value_cents, 2001);
        assert_eq!(e.transactions[0].date, "2026-02-01");
        assert_eq!(e.transactions[0].amount_cents, 150);
    }

    #[test]
    fn decimals() {
        assert_eq!(parse_decimal("1'234.50").unwrap(), 1234.5);
        assert_eq!(parse_decimal("1’234.50").unwrap(), 1234.5);
        assert_eq!(parse_decimal("1.234,50").unwrap(), 1234.5);
        assert_eq!(parse_decimal("1,234.50").unwrap(), 1234.5);
        assert_eq!(parse_decimal("12,5").unwrap(), 12.5);
        assert_eq!(parse_decimal("-3").unwrap(), -3.0);
        assert!(parse_decimal("abc").is_err());
        assert!(parse_decimal("NaN").is_err());
        assert!(parse_decimal("").is_err());
    }

    #[test]
    fn rejects_bad_input() {
        let bad_isin = "ISIN;Name;Anzahl;Kurs;Marktwert\nXX;A;1;1;1\n";
        assert!(parse_holdings_csv(bad_isin).is_err());
        let usd = "ISIN;Name;Anzahl;Kurs;Marktwert;Währung\nCH0000000011;A;1;1;1;USD\n";
        assert!(parse_holdings_csv(usd).is_err());
        let neg = "ISIN;Name;Anzahl;Kurs;Marktwert\nCH0000000011;A;-1;1;1\n";
        assert!(parse_holdings_csv(neg).is_err());
        assert!(parse_holdings_csv("ISIN;Name\nCH0000000011;A\n").is_err());
        assert!(parse_transactions_csv("Datum;Text;Betrag\n31.02.2026;x;1\n").is_err());
        assert!(parse_transactions_csv("Datum;Text;Betrag\n").is_err());
        assert!(parse_export("{\"holdings\": 5}").is_err());
        assert!(parse_export("{}").is_err());
        assert!(parse_export("{not json").is_err());
        assert!(parse_csv_rows("a;b\n\"x;y\n").is_err());
    }

    #[test]
    fn import_creates_depot_and_cash_accounts_per_currency() {
        let dir = crate::test_support::TempDir::new("girocrab-finpension-import");
        let v = Vault::open(
            dir.join("v.db").to_string_lossy().to_string(),
            vec![7u8; 32],
        )
        .unwrap();
        let conn = v
            .add_bank_connection(ConnectorKind::Finpension, "Finpension".into(), "3a".into())
            .unwrap();
        let json = r#"{"holdings":[{"isin":"CH0000000011","name":"Fund","quantity":2,"price":10.5,"value":21.0,"currency":"CHF"}],
            "transactions":[{"date":"2026-03-15","description":"Einzahlung","amount":100.0,"currency":"CHF","reference":"R1"}]}"#;
        let s = v
            .clone()
            .import_finpension_export(conn, json.as_bytes().to_vec())
            .unwrap();
        assert_eq!(s.transactions_imported, 1);
        let accounts = v.list_accounts().unwrap();
        assert_eq!(accounts.len(), 2);
        let depot = accounts
            .iter()
            .find(|a| a.kind == crate::models::AccountKind::Brokerage)
            .unwrap();
        assert_eq!(depot.balance_cents, 2100);
        assert_eq!(v.list_holdings(depot.id).unwrap().len(), 1);
        // importing again does not duplicate bookings
        let again = v
            .clone()
            .import_finpension_export(conn, json.as_bytes().to_vec())
            .unwrap();
        assert_eq!(again.transactions_imported, 0);
        // garbage and wrong connector
        assert!(
            v.clone()
                .import_finpension_export(conn, b"x".to_vec())
                .is_err()
        );
    }
}
