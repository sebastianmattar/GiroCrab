//! Interactive Brokers (IBKR) via the official **Flex Web Service** (v3).
//! Read-only: the user creates an "Activity Flex Query" (XML) and a Flex
//! token in the Client Portal (Performance & Reports → Flex Queries).
//!
//! Two HTTP GETs:
//!   1. `SendRequest?t=<token>&q=<query id>&v=3` → `<ReferenceCode>` + `<Url>`
//!   2. `<Url>?q=<reference code>&t=<token>&v=3` → `<FlexQueryResponse>`
//!      (error 1019 = "statement generation in progress": wait and retry).
//!
//! Credentials: login name = Flex token, PIN field = Flex query id.
//! The token travels in the URL, so errors never carry the request URL.
//! Per `FlexStatement` two accounts are created: the securities depot
//! (positions, value = sum of the positions) and the cash account
//! ("Verrechnungskonto", dividends, interest, fees, deposits, trades), both
//! in the statement's base currency. Foreign-currency amounts are converted
//! with `fxRateToBase`; the original amount is kept in the purpose.
//!
//! No XML crate: Flex XML is flat (elements with attributes), so a small
//! tolerant tag scanner is enough and adds no dependency.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use async_trait::async_trait;

use super::{
    BankConnector, ChallengeResponse, ConnectContext, ContentIds, FetchResult, Phase,
    RemoteAccount, RemoteHolding, RemoteSecurityTransaction, RemoteTransaction,
};
use crate::error::{CoreError, CoreResult};
use crate::i18n::{tr, trf};
use crate::models::{AccountKind, Holding, SecurityDirection, SecurityTransaction};

const SEND_URL: &str =
    "https://ndcdyn.interactivebrokers.com/AccountManagement/FlexWebService/SendRequest";
/// Attempts at `GetStatement` while IBKR is still generating the statement.
const MAX_POLLS: u32 = 8;
const POLL_DELAY: Duration = Duration::from_secs(4);
/// Attempts per HTTP request on transient network errors, with exponential backoff.
const NET_ATTEMPTS: u32 = 3;
const RETRY_DELAY: Duration = Duration::from_secs(2);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

pub struct IbkrConnector {
    client: Option<reqwest::Client>,
    send_url: String,
    poll_delay: Duration,
    max_polls: u32,
    retry_delay: Duration,
}

impl Default for IbkrConnector {
    fn default() -> Self {
        Self::new()
    }
}

impl IbkrConnector {
    pub fn new() -> Self {
        IbkrConnector {
            client: None,
            send_url: SEND_URL.into(),
            poll_delay: POLL_DELAY,
            max_polls: MAX_POLLS,
            retry_delay: RETRY_DELAY,
        }
    }

    fn client(&mut self) -> CoreResult<reqwest::Client> {
        if let Some(c) = &self.client {
            return Ok(c.clone());
        }
        // The Flex token is in the query: no redirect to another origin, no Referer.
        let c = super::client_builder(Duration::from_secs(60))
            .connect_timeout(CONNECT_TIMEOUT)
            .redirect(super::same_host_redirects())
            .referer(false)
            .build()
            .map_err(net_err)?;
        Ok(self.client.insert(c).clone())
    }

    async fn get(
        &mut self,
        url: &str,
        params: &[(&str, &str)],
    ) -> CoreResult<(reqwest::StatusCode, String)> {
        let client = self.client()?;
        let mut attempt = 0;
        let resp = loop {
            attempt += 1;
            match client.get(url).query(params).send().await {
                Ok(r) => break r,
                Err(e) if attempt < NET_ATTEMPTS && is_transient(&e) => {
                    tokio::time::sleep(self.retry_delay * 2u32.pow(attempt - 1)).await;
                }
                Err(e) => return Err(net_err(e)),
            }
        };
        let status = resp.status();
        let body = super::read_text_limited(resp).await.map_err(|e| match e {
            CoreError::Network { message } => CoreError::Network {
                message: strip_url(&message),
            },
            other => other,
        })?;
        Ok((status, body))
    }

    /// Step 1 + 2: returns the raw `<FlexQueryResponse>` XML.
    async fn download(&mut self, token: &str, query_id: &str) -> CoreResult<String> {
        let send_url = self.send_url.clone();
        let (status, body) = self
            .get(&send_url, &[("t", token), ("q", query_id), ("v", "3")])
            .await?;
        let resp = parse_status_response(&body).ok_or_else(|| CoreError::Protocol {
            message: trf(
                "Interactive Brokers: unerwartete Antwort (HTTP {0}).",
                &[&status.as_u16()],
            ),
        })?;
        if !resp.ok {
            return Err(flex_error(&resp));
        }
        let (Some(reference), Some(url)) = (resp.reference_code, resp.url) else {
            return Err(CoreError::Protocol {
                message: tr("Interactive Brokers: Antwort ohne Referenzcode.").into(),
            });
        };
        if !trusted_statement_url(&url) {
            return Err(CoreError::Protocol {
                message: tr("Interactive Brokers: unerwartete Abruf-Adresse.").into(),
            });
        }

        for attempt in 0..self.max_polls {
            let (status, body) = self
                .get(&url, &[("q", &reference), ("t", token), ("v", "3")])
                .await?;
            if body.contains("<FlexQueryResponse") {
                return Ok(body);
            }
            let Some(resp) = parse_status_response(&body) else {
                return Err(CoreError::Protocol {
                    message: trf(
                        "Interactive Brokers: unerwartete Antwort (HTTP {0}).",
                        &[&status.as_u16()],
                    ),
                });
            };
            // 1019 = still generating; 1001/1018 = busy, try again shortly.
            let retry = matches!(resp.error_code.as_deref(), Some("1019" | "1001" | "1018"));
            if !retry || attempt + 1 == self.max_polls {
                return Err(if retry {
                    CoreError::Bank {
                        code: resp.error_code.unwrap_or_default(),
                        message: tr(
                            "Interactive Brokers erstellt den Bericht noch. Bitte in einer Minute erneut synchronisieren.",
                        )
                        .into(),
                    }
                } else {
                    flex_error(&resp)
                });
            }
            tokio::time::sleep(self.poll_delay).await;
        }
        Err(CoreError::Protocol {
            message: tr("Interactive Brokers: kein Bericht erhalten.").into(),
        })
    }
}

#[async_trait]
impl BankConnector for IbkrConnector {
    async fn begin(&mut self, ctx: &ConnectContext) -> CoreResult<Phase> {
        if ctx.login_name.trim().is_empty() || ctx.secret.expose().trim().is_empty() {
            return Err(CoreError::Authentication {
                message: tr("Bitte Flex-Token und Flex-Query-ID eingeben.").into(),
            });
        }
        Ok(Phase::Ready)
    }

    async fn respond(
        &mut self,
        _ctx: &ConnectContext,
        _response: ChallengeResponse,
    ) -> CoreResult<Phase> {
        Ok(Phase::Ready)
    }

    async fn fetch(&mut self, ctx: &ConnectContext) -> CoreResult<FetchResult> {
        let token = ctx.login_name.trim().to_string();
        let query_id = ctx.secret.expose().trim().to_string();
        let xml = self.download(&token, &query_id).await?;
        build_fetch(&xml, &ctx.since)
    }
}

// ------------------------------------------------------------------ errors

fn net_err(e: reqwest::Error) -> CoreError {
    let e = e.without_url();
    let kind = if e.is_timeout() {
        " (Zeitüberschreitung)"
    } else if e.is_connect() {
        " (Verbindung fehlgeschlagen)"
    } else {
        ""
    };
    CoreError::Network {
        message: format!("{}{kind}", error_chain(&e)),
    }
}

/// Connection drops, timeouts and failed connects are worth another attempt.
fn is_transient(e: &reqwest::Error) -> bool {
    e.is_timeout() || e.is_connect() || e.is_request()
}

/// `error` plus its whole `source()` chain, joined with ": " (URLs removed).
fn error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(e) = cur {
        let text = strip_url(&e.to_string());
        if !text.is_empty() && parts.last() != Some(&text) {
            parts.push(text);
        }
        cur = e.source();
    }
    parts.join(": ")
}

/// Drops `(url: …)`-style fragments (they would contain the token).
fn strip_url(message: &str) -> String {
    match message.find("http") {
        Some(i) => message[..i]
            .trim_end()
            .trim_end_matches(['(', ':'])
            .to_string(),
        None => message.to_string(),
    }
}

fn flex_error(r: &StatusResponse) -> CoreError {
    let code = r.error_code.clone().unwrap_or_default();
    let text = r.error_message.clone().unwrap_or_default();
    match code.as_str() {
        // 1012 token expired, 1015 token invalid, 1016 account invalid, 1014 query invalid
        "1012" | "1015" | "1016" => CoreError::Authentication {
            message: trf(
                "Interactive Brokers lehnt das Flex-Token ab ({0}): {1}",
                &[&code, &text],
            ),
        },
        "1014" | "1013" => CoreError::Bank {
            message: trf(
                "Interactive Brokers: Flex-Query-ID ungültig ({0}): {1}",
                &[&code, &text],
            ),
            code,
        },
        _ => CoreError::Bank {
            message: trf("Interactive Brokers meldet {0}: {1}", &[&code, &text]),
            code,
        },
    }
}

/// The statement URL must stay at IBKR (the token is sent along).
fn trusted_statement_url(url: &str) -> bool {
    let Ok(u) = url::Url::parse(url) else {
        return false;
    };
    #[cfg(test)]
    if u.scheme() == "http" && u.host_str() == Some("127.0.0.1") {
        return true;
    }
    u.scheme() == "https"
        && u.host_str().is_some_and(|h| {
            h == "interactivebrokers.com" || h.ends_with(".interactivebrokers.com")
        })
}

// ------------------------------------------------------------------ XML scanner

#[derive(Debug, Clone, Copy, PartialEq)]
enum TagKind {
    Open,
    Close,
    SelfClose,
}

#[derive(Debug)]
struct Tag {
    name: String,
    attrs: HashMap<String, String>,
    kind: TagKind,
}

fn parse_attrs(s: &str) -> HashMap<String, String> {
    let mut attrs = HashMap::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b'/') {
            i += 1;
        }
        let start = i;
        while i < b.len() && b[i] != b'=' && !b[i].is_ascii_whitespace() {
            i += 1;
        }
        let name = &s[start..i];
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= b.len() || b[i] != b'=' {
            continue;
        }
        i += 1;
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= b.len() || (b[i] != b'"' && b[i] != b'\'') {
            continue;
        }
        let quote = b[i];
        i += 1;
        let vstart = i;
        while i < b.len() && b[i] != quote {
            i += 1;
        }
        if !name.is_empty() {
            attrs.insert(
                name.to_string(),
                crate::text::decode_html_entities(&s[vstart..i.min(b.len())]),
            );
        }
        i += 1;
    }
    attrs
}

fn scan_tags(xml: &str) -> Vec<Tag> {
    let b = xml.as_bytes();
    let mut tags = Vec::new();
    let mut i = 0;
    while let Some(p) = xml[i..].find('<') {
        let s = i + p;
        let rest = &xml[s..];
        if rest.starts_with("<!--") {
            i = rest.find("-->").map_or(xml.len(), |e| s + e + 3);
            continue;
        }
        if rest.starts_with("<![CDATA[") {
            i = rest.find("]]>").map_or(xml.len(), |e| s + e + 3);
            continue;
        }
        let mut j = s + 1;
        let mut quote: Option<u8> = None;
        while j < b.len() {
            match (quote, b[j]) {
                (None, b'>') => break,
                (None, c @ (b'"' | b'\'')) => quote = Some(c),
                (Some(q), c) if q == c => quote = None,
                _ => {}
            }
            j += 1;
        }
        if j >= b.len() {
            break;
        }
        i = j + 1;
        let inner = &xml[s + 1..j];
        if inner.starts_with('?') || inner.starts_with('!') {
            continue;
        }
        let (kind, inner) = if let Some(c) = inner.strip_prefix('/') {
            (TagKind::Close, c)
        } else if let Some(c) = inner.strip_suffix('/') {
            (TagKind::SelfClose, c)
        } else {
            (TagKind::Open, inner)
        };
        let name_end = inner
            .find(|c: char| c.is_whitespace() || c == '/')
            .unwrap_or(inner.len());
        tags.push(Tag {
            name: inner[..name_end].to_string(),
            attrs: parse_attrs(&inner[name_end..]),
            kind,
        });
    }
    tags
}

/// Text of the first `<name>…</name>` element.
fn tag_text(xml: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&format!("</{name}>"))? + start;
    Some(crate::text::decode_html_entities(xml[start..end].trim()))
}

// ------------------------------------------------------------------ SendRequest / status responses

#[derive(Debug, Default)]
struct StatusResponse {
    ok: bool,
    reference_code: Option<String>,
    url: Option<String>,
    error_code: Option<String>,
    error_message: Option<String>,
}

/// Parses `<FlexStatementResponse>`; `None` if it is not one.
fn parse_status_response(xml: &str) -> Option<StatusResponse> {
    if !xml.contains("<FlexStatementResponse") {
        return None;
    }
    let status = tag_text(xml, "Status")?;
    Some(StatusResponse {
        ok: status.eq_ignore_ascii_case("Success"),
        reference_code: tag_text(xml, "ReferenceCode").filter(|s| !s.is_empty()),
        url: tag_text(xml, "Url").filter(|s| !s.is_empty()),
        error_code: tag_text(xml, "ErrorCode").filter(|s| !s.is_empty()),
        error_message: tag_text(xml, "ErrorMessage").filter(|s| !s.is_empty()),
    })
}

// ------------------------------------------------------------------ numbers and dates

/// Decimal string → cents (half away from zero), no floats. Accepts
/// `1234.5`, `-12.345`, `1,234.50` and `12,5`.
fn parse_cents(s: &str) -> Option<i64> {
    let mut t = s.trim().replace([' ', '\''], "");
    if t.is_empty() {
        return None;
    }
    if t.contains('.') {
        t = t.replace(',', "");
    } else {
        t = t.replace(',', ".");
    }
    let (neg, t) = match t.strip_prefix('-') {
        Some(r) => (true, r.to_string()),
        None => (false, t.trim_start_matches('+').to_string()),
    };
    let (int, frac) = t.split_once('.').unwrap_or((&t, ""));
    if int.is_empty() && frac.is_empty() {
        return None;
    }
    if !int.bytes().all(|c| c.is_ascii_digit()) || !frac.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let mut cents: i64 = if int.is_empty() { 0 } else { int.parse().ok()? };
    cents = cents.checked_mul(100)?;
    let fb = frac.as_bytes();
    let d = |i: usize| fb.get(i).map_or(0, |c| i64::from(c - b'0'));
    cents = cents.checked_add(d(0) * 10 + d(1))?;
    if d(2) >= 5 {
        cents = cents.checked_add(1)?;
    }
    Some(if neg { -cents } else { cents })
}

fn attr<'a>(a: &'a HashMap<String, String>, key: &str) -> &'a str {
    a.get(key).map(|s| s.trim()).unwrap_or("")
}

/// Converts cents in `currency` to the base currency via `fxRateToBase`.
fn to_base(cents: i64, a: &HashMap<String, String>, base: &str) -> i64 {
    let cur = attr(a, "currency");
    if cur.is_empty() || cur.eq_ignore_ascii_case(base) {
        return cents;
    }
    match attr(a, "fxRateToBase").parse::<f64>() {
        Ok(fx) if fx > 0.0 => (cents as f64 * fx).round() as i64,
        _ => cents,
    }
}

/// `20240115`, `2024-01-15`, `20240115;143000`, `2024-01-15, 14:30:00` → `2024-01-15`.
fn norm_date(s: &str) -> Option<String> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() >= 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b[..4]
            .iter()
            .chain(&b[5..7])
            .chain(&b[8..10])
            .all(u8::is_ascii_digit)
    {
        return Some(s[..10].to_string());
    }
    if b.len() >= 8 && b[..8].iter().all(u8::is_ascii_digit) {
        return Some(format!("{}-{}-{}", &s[..4], &s[4..6], &s[6..8]));
    }
    None
}

fn first_date(a: &HashMap<String, String>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| norm_date(attr(a, k)))
}

// ------------------------------------------------------------------ statements

type Attrs = HashMap<String, String>;

#[derive(Debug, Default)]
struct Statement {
    account_id: String,
    base_currency: String,
    to_date: Option<String>,
    positions: Vec<Attrs>,
    cash_txs: Vec<Attrs>,
    trades: Vec<Attrs>,
    /// `CashReportCurrency currency="BASE_SUMMARY"` `endingCash`.
    cash_base: Option<i64>,
    /// Latest `EquitySummaryByReportDateInBase`: (report date, cash, total).
    equity: Option<(String, Option<i64>, Option<i64>)>,
}

fn parse_statements(xml: &str) -> CoreResult<Vec<Statement>> {
    if !xml.contains("<FlexQueryResponse") {
        return Err(CoreError::Protocol {
            message: tr("Interactive Brokers: Bericht ist keine Flex-Query-Antwort.").into(),
        });
    }
    let mut out: Vec<Statement> = Vec::new();
    let mut cur: Option<Statement> = None;
    for t in scan_tags(xml) {
        match (t.name.as_str(), t.kind) {
            ("FlexStatement", TagKind::Close) => out.extend(cur.take()),
            ("FlexStatement", _) => {
                out.extend(cur.take());
                let s = Statement {
                    account_id: attr(&t.attrs, "accountId").to_string(),
                    base_currency: attr(&t.attrs, "baseCurrency").to_ascii_uppercase(),
                    to_date: norm_date(attr(&t.attrs, "toDate")),
                    ..Default::default()
                };
                if t.kind == TagKind::SelfClose {
                    out.push(s);
                } else {
                    cur = Some(s);
                }
            }
            (_, TagKind::Close) => {}
            (name, _) => {
                let Some(s) = cur.as_mut() else { continue };
                let level = attr(&t.attrs, "levelOfDetail").to_ascii_uppercase();
                match name {
                    // Only the summary line per position (not the tax lots).
                    "OpenPosition" if level != "LOT" => s.positions.push(t.attrs),
                    "CashTransaction" if level != "SUMMARY" => s.cash_txs.push(t.attrs),
                    "Trade" if level.is_empty() || level == "EXECUTION" => s.trades.push(t.attrs),
                    "CashReportCurrency"
                        if attr(&t.attrs, "currency").eq_ignore_ascii_case("BASE_SUMMARY") =>
                    {
                        s.cash_base = parse_cents(attr(&t.attrs, "endingCash"));
                    }
                    "AccountInformation" if !attr(&t.attrs, "currency").is_empty() => {
                        if s.base_currency.is_empty() {
                            s.base_currency = attr(&t.attrs, "currency").to_ascii_uppercase();
                        }
                    }
                    "EquitySummaryByReportDateInBase" => {
                        let date = attr(&t.attrs, "reportDate").to_string();
                        if s.equity.as_ref().is_none_or(|(d, _, _)| date >= *d) {
                            s.equity = Some((
                                date,
                                parse_cents(attr(&t.attrs, "cash")),
                                parse_cents(attr(&t.attrs, "total")),
                            ));
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    out.extend(cur.take());
    // No base currency in the report at all: assume EUR, as before.
    for s in &mut out {
        if s.base_currency.is_empty() {
            s.base_currency = "EUR".into();
        }
    }
    if out.is_empty() {
        return Err(CoreError::Protocol {
            message: tr("Interactive Brokers: Der Bericht enthält kein FlexStatement. Bitte in der Flex Query Konten, Offene Positionen und Cash-Transaktionen aktivieren.").into(),
        });
    }
    Ok(out)
}

fn parse_holding(a: &Attrs, base: &str) -> Option<Holding> {
    let quantity: f64 = attr(a, "position").parse().ok()?;
    if quantity == 0.0 {
        return None;
    }
    let isin = [attr(a, "isin"), attr(a, "conid"), attr(a, "cusip")]
        .into_iter()
        .find(|s| !s.is_empty())
        .unwrap_or("")
        .to_string();
    let symbol = attr(a, "symbol");
    let description = attr(a, "description");
    let name = match (description.is_empty(), symbol.is_empty()) {
        (false, false) => format!("{description} ({symbol})"),
        (false, true) => description.to_string(),
        (true, _) => symbol.to_string(),
    };
    // Figures in the position's own currency (`currency`), as the report gives them.
    let native_price = parse_cents(attr(a, "markPrice"));
    let native_value = parse_cents(attr(a, "positionValue"))
        .or_else(|| native_price.map(|p| (p as f64 * quantity).round() as i64));
    let native_purchase = parse_cents(attr(a, "costBasisPrice")).or_else(|| {
        parse_cents(attr(a, "costBasisMoney")).map(|c| (c as f64 / quantity).round() as i64)
    });
    let price = native_price.map(|c| to_base(c, a, base));
    let value = native_value.map(|c| to_base(c, a, base)).unwrap_or(0);
    let price = price
        .or_else(|| (quantity != 0.0).then(|| (value as f64 / quantity).round() as i64))
        .unwrap_or(0);
    // Per-unit cost basis in the base currency; the money total is converted first.
    let purchase = parse_cents(attr(a, "costBasisPrice"))
        .map(|c| to_base(c, a, base))
        .or_else(|| {
            parse_cents(attr(a, "costBasisMoney"))
                .map(|c| (to_base(c, a, base) as f64 / quantity).round() as i64)
        })
        .unwrap_or(0);
    // Native figures only where the position is not held in the base currency.
    let cur = attr(a, "currency").to_ascii_uppercase();
    let foreign = !cur.is_empty() && !cur.eq_ignore_ascii_case(base);
    Some(Holding {
        isin,
        wkn: String::new(),
        name,
        quantity,
        current_price_cents: price,
        purchase_price_cents: purchase,
        value_cents: value,
        currency: if foreign { cur } else { String::new() },
        native_price_cents: native_price.filter(|_| foreign),
        native_purchase_price_cents: native_purchase.filter(|_| foreign),
        native_value_cents: native_value.filter(|_| foreign),
        ..Default::default()
    })
}

/// `(base-currency cents, purpose suffix)` of an amount in its own currency.
fn base_amount(a: &Attrs, cents: i64, base: &str) -> (i64, String) {
    let cur = attr(a, "currency");
    let b = to_base(cents, a, base);
    if cur.is_empty() || cur.eq_ignore_ascii_case(base) {
        (b, String::new())
    } else {
        (
            b,
            format!(" ({}.{:02} {cur})", cents / 100, cents.abs() % 100),
        )
    }
}

fn parse_cash_tx(a: &Attrs, st: &Statement, cash_ref: &str) -> Option<RemoteTransaction> {
    let cents = parse_cents(attr(a, "amount"))?;
    let (amount, orig) = base_amount(a, cents, &st.base_currency);
    if amount == 0 {
        return None;
    }
    let date = first_date(a, &["reportDate", "dateTime", "settleDate"])?;
    let value_date = first_date(a, &["settleDate"]).unwrap_or_else(|| date.clone());
    let symbol = attr(a, "symbol");
    let kind = attr(a, "type");
    let description = attr(a, "description");
    let purpose = if description.is_empty() {
        format!("{kind}{orig}")
    } else {
        format!("{description}{orig}")
    };
    let mut t = RemoteTransaction {
        account_ref: cash_ref.to_string(),
        booking_date: date,
        value_date,
        amount_cents: amount,
        currency: st.base_currency.clone(),
        counterparty: if symbol.is_empty() {
            "Interactive Brokers".into()
        } else {
            symbol.to_string()
        },
        purpose,
        booking_text: kind.to_string(),
        ..Default::default()
    };
    let id = attr(a, "transactionID");
    if !id.is_empty() {
        t.external_id = format!("ibkr:{}:{id}", st.account_id);
    }
    Some(t)
}

fn parse_trade(a: &Attrs, st: &Statement, cash_ref: &str) -> Option<RemoteTransaction> {
    // FX conversions (assetCategory CASH) only move money between currencies.
    if attr(a, "assetCategory").eq_ignore_ascii_case("CASH") {
        return None;
    }
    let cents = match parse_cents(attr(a, "netCash")) {
        Some(c) => c,
        None => {
            parse_cents(attr(a, "proceeds"))? + parse_cents(attr(a, "ibCommission")).unwrap_or(0)
        }
    };
    let (amount, orig) = base_amount(a, cents, &st.base_currency);
    if amount == 0 {
        return None;
    }
    let date = first_date(a, &["tradeDate", "dateTime", "reportDate"])?;
    let value_date = first_date(a, &["settleDateTarget"]).unwrap_or_else(|| date.clone());
    let side = attr(a, "buySell").to_ascii_uppercase();
    let symbol = attr(a, "symbol");
    let purpose = format!(
        "{side} {} {symbol} @ {} {}{orig}",
        attr(a, "quantity").trim_start_matches('-'),
        attr(a, "tradePrice"),
        attr(a, "currency")
    );
    let mut t = RemoteTransaction {
        account_ref: cash_ref.to_string(),
        booking_date: date,
        value_date,
        amount_cents: amount,
        currency: st.base_currency.clone(),
        counterparty: if symbol.is_empty() {
            "Interactive Brokers".into()
        } else {
            symbol.to_string()
        },
        purpose: purpose.split_whitespace().collect::<Vec<_>>().join(" "),
        booking_text: format!("Trade {side}").trim().to_string(),
        ..Default::default()
    };
    let id = [attr(a, "transactionID"), attr(a, "tradeID")]
        .into_iter()
        .find(|s| !s.is_empty());
    if let Some(id) = id {
        t.external_id = format!("ibkr:{}:{id}", st.account_id);
    }
    Some(t)
}

/// An ISIN inside a description such as "AAPL(US0378331005) CASH DIVIDEND …".
fn isin_in(text: &str) -> String {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .find(|w| {
            w.len() == 12
                && w[..2].chars().all(|c| c.is_ascii_uppercase())
                && w[2..]
                    .chars()
                    .all(|c| c.is_ascii_digit() || c.is_ascii_uppercase())
                && w[11..].chars().all(|c| c.is_ascii_digit())
        })
        .unwrap_or("")
        .to_string()
}

fn decimal(a: &Attrs, key: &str) -> Option<f64> {
    attr(a, key).replace(',', "").parse().ok()
}

/// A trade as a securities transaction of the depot (#314), in the trade's
/// own currency with `fx_rate` to the base currency; the cash side is
/// `parse_trade` on the settlement account (same transaction id).
fn parse_security_trade(
    a: &Attrs,
    st: &Statement,
    depot_ref: &str,
) -> Option<RemoteSecurityTransaction> {
    if attr(a, "assetCategory").eq_ignore_ascii_case("CASH") {
        return None;
    }
    let quantity = decimal(a, "quantity")?.abs();
    let date = first_date(a, &["tradeDate", "dateTime", "reportDate"])?;
    let side = attr(a, "buySell").to_ascii_uppercase();
    let direction = match side.as_str() {
        "BUY" => SecurityDirection::Buy,
        "SELL" => SecurityDirection::Sell,
        _ => SecurityDirection::Other,
    };
    let cents = |k: &str| parse_cents(attr(a, k)).map_or(0, i64::abs);
    let id = [attr(a, "transactionID"), attr(a, "tradeID")]
        .into_iter()
        .find(|s| !s.is_empty());
    let symbol = attr(a, "symbol");
    let isin = [attr(a, "isin"), attr(a, "conid"), attr(a, "cusip")]
        .into_iter()
        .find(|s| !s.is_empty())
        .unwrap_or("")
        .to_string();
    let external_id = match id {
        Some(id) => format!("ibkr:{}:{id}", st.account_id),
        None => format!(
            "ibkr:{}:{date}:{symbol}:{side}:{quantity}:{}",
            st.account_id,
            attr(a, "tradePrice")
        ),
    };
    let currency = if attr(a, "currency").is_empty() {
        &st.base_currency
    } else {
        attr(a, "currency")
    };
    Some(RemoteSecurityTransaction {
        account_ref: depot_ref.to_string(),
        external_id,
        cash_external_id: id
            .map(|i| format!("ibkr:{}:{i}", st.account_id))
            .unwrap_or_default(),
        transaction: SecurityTransaction {
            booking_date: date.clone(),
            settlement_date: first_date(a, &["settleDateTarget"]).unwrap_or_default(),
            isin,
            name: if attr(a, "description").is_empty() {
                symbol.to_string()
            } else {
                attr(a, "description").to_string()
            },
            direction,
            quantity,
            price_cents: parse_cents(attr(a, "tradePrice")).unwrap_or(0),
            currency: currency.to_string(),
            value_cents: cents("proceeds"),
            fees_cents: cents("ibCommission"),
            taxes_cents: cents("taxes"),
            fx_rate: decimal(a, "fxRateToBase"),
            booking_text: format!("Trade {side}").trim().to_string(),
            ..Default::default()
        },
    })
}

/// Dividends, withholding tax and fees of the cash report that belong to a
/// security or the account, as securities transactions of the depot.
fn parse_security_cash(
    a: &Attrs,
    st: &Statement,
    depot_ref: &str,
) -> Option<RemoteSecurityTransaction> {
    let kind = attr(a, "type");
    let direction = match kind {
        "Dividends" | "Payment In Lieu Of Dividends" => SecurityDirection::Dividend,
        "Withholding Tax" => SecurityDirection::Tax,
        "Broker Fees" | "Other Fees" | "Commission Adjustments" => SecurityDirection::Fee,
        _ => return None,
    };
    let value = parse_cents(attr(a, "amount"))?.abs();
    let id = attr(a, "transactionID");
    if value == 0 || id.is_empty() {
        return None;
    }
    let date = first_date(a, &["reportDate", "dateTime", "settleDate"])?;
    let symbol = attr(a, "symbol");
    let description = attr(a, "description");
    let isin = [attr(a, "isin"), attr(a, "conid"), attr(a, "cusip")]
        .into_iter()
        .find(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| isin_in(description));
    let ext = format!("ibkr:{}:{id}", st.account_id);
    Some(RemoteSecurityTransaction {
        account_ref: depot_ref.to_string(),
        external_id: ext.clone(),
        cash_external_id: ext,
        transaction: SecurityTransaction {
            booking_date: date,
            settlement_date: first_date(a, &["settleDate"]).unwrap_or_default(),
            isin,
            name: if description.is_empty() {
                symbol.to_string()
            } else {
                description.to_string()
            },
            direction,
            currency: if attr(a, "currency").is_empty() {
                st.base_currency.clone()
            } else {
                attr(a, "currency").to_string()
            },
            value_cents: value,
            fx_rate: decimal(a, "fxRateToBase"),
            booking_text: kind.to_string(),
            ..Default::default()
        },
    })
}

fn build_fetch(xml: &str, since: &str) -> CoreResult<FetchResult> {
    let statements = parse_statements(xml)?;
    let mut out = FetchResult::default();
    let mut seen: HashSet<String> = HashSet::new();
    let mut content_ids = ContentIds::default();
    let multi = statements.len() > 1;
    for st in &statements {
        let id = if st.account_id.is_empty() {
            "account"
        } else {
            st.account_id.as_str()
        };
        let depot_ref = format!("ibkr:{id}:depot");
        let cash_ref = format!("ibkr:{id}:cash");
        let date = st.to_date.clone().unwrap_or_else(super::today);

        let holdings: Vec<Holding> = st
            .positions
            .iter()
            .filter_map(|p| parse_holding(p, &st.base_currency))
            .collect();
        let cash = st
            .cash_base
            .or_else(|| st.equity.as_ref().and_then(|(_, c, _)| *c))
            .unwrap_or(0);
        let depot_value = if holdings.is_empty() {
            st.equity
                .as_ref()
                .and_then(|(_, _, total)| *total)
                .map_or(0, |t| (t - cash).max(0))
        } else {
            holdings.iter().map(|h| h.value_cents).sum()
        };

        let label = if multi || !id.is_empty() {
            format!(" {id}")
        } else {
            String::new()
        };
        out.accounts.push(RemoteAccount {
            external_ref: depot_ref.clone(),
            name: format!("IBKR Depot{label}"),
            owner: String::new(),
            iban: String::new(),
            account_number: st.account_id.clone(),
            currency: st.base_currency.clone(),
            kind: AccountKind::Brokerage,
            balance_cents: depot_value,
            balance_date: date.clone(),
        });
        out.accounts.push(RemoteAccount {
            external_ref: cash_ref.clone(),
            name: format!("IBKR Verrechnungskonto{label}"),
            owner: String::new(),
            iban: String::new(),
            account_number: st.account_id.clone(),
            currency: st.base_currency.clone(),
            kind: AccountKind::Checking,
            balance_cents: cash,
            balance_date: date,
        });
        out.holdings
            .extend(holdings.into_iter().map(|holding| RemoteHolding {
                classification: Default::default(),
                direct_crypto: false,

                account_ref: depot_ref.clone(),
                holding,
            }));

        let txs = st
            .cash_txs
            .iter()
            .filter_map(|a| parse_cash_tx(a, st, &cash_ref))
            .chain(
                st.trades
                    .iter()
                    .filter_map(|a| parse_trade(a, st, &cash_ref)),
            );
        let mut seen_sec: HashSet<String> = HashSet::new();
        for t in st
            .trades
            .iter()
            .filter_map(|a| parse_security_trade(a, st, &depot_ref))
            .chain(
                st.cash_txs
                    .iter()
                    .filter_map(|a| parse_security_cash(a, st, &depot_ref)),
            )
        {
            if t.transaction.booking_date.as_str() >= since
                && seen_sec.insert(t.external_id.clone())
            {
                out.security_transactions.push(t);
            }
        }
        for mut t in txs {
            if t.booking_date.as_str() < since {
                continue;
            }
            if t.external_id.is_empty() {
                t.external_id = content_ids.next(&t);
            }
            if seen.insert(format!("{}\u{1f}{}", t.account_ref, t.external_id)) {
                out.transactions.push(t);
            }
        }
    }
    Ok(out)
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn token_client_does_not_follow_redirects_to_another_origin() {
        let client = IbkrConnector::new().client().unwrap();
        crate::connectors::tests::redirects::assert_no_cross_origin_replay(&client).await;
    }

    const OK_SEND: &str = r#"<FlexStatementResponse timestamp='13 November, 2026 10:00 AM EST'>
<Status>Success</Status>
<ReferenceCode>1234567890</ReferenceCode>
<Url>https://ndcdyn.interactivebrokers.com/AccountManagement/FlexWebService/GetStatement</Url>
</FlexStatementResponse>"#;

    const IN_PROGRESS: &str = r#"<FlexStatementResponse timestamp='13 November, 2026 10:00 AM EST'>
<Status>Warn</Status>
<ErrorCode>1019</ErrorCode>
<ErrorMessage>Statement generation in progress. Please try again shortly.</ErrorMessage>
</FlexStatementResponse>"#;

    const BAD_TOKEN: &str = r#"<FlexStatementResponse timestamp='13 November, 2026 10:00 AM EST'>
<Status>Fail</Status>
<ErrorCode>1015</ErrorCode>
<ErrorMessage>Token is invalid.</ErrorMessage>
</FlexStatementResponse>"#;

    /// Base currency EUR; CHF and USD holdings, trades and dividends.
    const FLEX: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<FlexQueryResponse queryName="GiroCrab" type="AF">
<FlexStatements count="1">
<FlexStatement accountId="U1234567" fromDate="20261001" toDate="20261112" period="LastMonth" whenGenerated="20261113;040000">
<CashReport>
<CashReportCurrency accountId="U1234567" currency="BASE_SUMMARY" endingCash="1234.56" endingSettledCash="1234.56" />
<CashReportCurrency accountId="U1234567" currency="USD" endingCash="500.00" />
</CashReport>
<EquitySummaryInBase>
<EquitySummaryByReportDateInBase accountId="U1234567" reportDate="20261111" cash="1000.00" total="9000.00" />
<EquitySummaryByReportDateInBase accountId="U1234567" reportDate="20261112" cash="1234.56" total="10000.00" />
</EquitySummaryInBase>
<OpenPositions>
<OpenPosition accountId="U1234567" currency="EUR" fxRateToBase="1" assetCategory="STK" symbol="SAP" description="SAP SE" conid="14094" isin="DE0007164600" position="10" markPrice="120.50" costBasisPrice="100.25" costBasisMoney="1002.50" positionValue="1205.00" levelOfDetail="SUMMARY" />
<OpenPosition accountId="U1234567" currency="EUR" fxRateToBase="1" assetCategory="STK" symbol="SAP" description="SAP SE" isin="DE0007164600" position="4" markPrice="120.50" costBasisPrice="90" positionValue="482.00" levelOfDetail="LOT" />
<OpenPosition accountId="U1234567" currency="CHF" fxRateToBase="1.0600" assetCategory="STK" symbol="NESN" description="NESTLE SA-REG &amp; Co" conid="4661" isin="CH0038863350" position="5" markPrice="100.00" costBasisPrice="90.00" positionValue="500.00" levelOfDetail="SUMMARY" />
<OpenPosition accountId="U1234567" currency="USD" fxRateToBase="0.9" assetCategory="STK" symbol="AAPL" description="APPLE INC" cusip="037833100" position="2.5" markPrice="200.00" costBasisMoney="400.00" positionValue="500.00" levelOfDetail="SUMMARY" />
</OpenPositions>
<Trades>
<Trade accountId="U1234567" currency="USD" fxRateToBase="0.9" assetCategory="STK" symbol="AAPL" tradeID="777" transactionID="9001" dateTime="20261105;153000" tradeDate="20261105" settleDateTarget="20261107" buySell="BUY" quantity="2.5" tradePrice="160.00" proceeds="-400.00" ibCommission="-1.00" netCash="-401.00" levelOfDetail="EXECUTION" />
<Trade accountId="U1234567" currency="USD" fxRateToBase="0.9" assetCategory="STK" symbol="AAPL" tradeID="777" dateTime="20261105;153000" tradeDate="20261105" buySell="BUY" quantity="2.5" netCash="-401.00" levelOfDetail="ORDER" />
<Trade accountId="U1234567" currency="EUR" fxRateToBase="1" assetCategory="CASH" symbol="EUR.USD" transactionID="9002" tradeDate="20261105" buySell="SELL" quantity="445" proceeds="445.00" netCash="445.00" levelOfDetail="EXECUTION" />
</Trades>
<CashTransactions>
<CashTransaction accountId="U1234567" currency="USD" fxRateToBase="0.9" assetCategory="STK" symbol="AAPL" description="AAPL(US0378331005) CASH DIVIDEND USD 0.25 PER SHARE (Ordinary Dividend)" dateTime="20261110;202000" settleDate="20261112" amount="0.63" type="Dividends" transactionID="9100" reportDate="20261110" />
<CashTransaction accountId="U1234567" currency="USD" fxRateToBase="0.9" symbol="AAPL" description="AAPL(US0378331005) CASH DIVIDEND USD 0.25 PER SHARE - US TAX" dateTime="20261110;202000" amount="-0.09" type="Withholding Tax" transactionID="9101" reportDate="20261110" />
<CashTransaction accountId="U1234567" currency="EUR" fxRateToBase="1" description="EUR CREDIT INT FOR OCT-2026" dateTime="20261103;202000" amount="1.55" type="Broker Interest Received" transactionID="" reportDate="20261103" />
<CashTransaction accountId="U1234567" currency="EUR" fxRateToBase="1" description="Cash Transfer" dateTime="20261001;091500" amount="2000.00" type="Deposits/Withdrawals" transactionID="9102" reportDate="20261001" />
<CashTransaction accountId="U1234567" currency="CHF" fxRateToBase="1.06" symbol="NESN" description="NESN DIVIDEND CHF 3.00 PER SHARE" dateTime="20261020" amount="15.00" type="Dividends" transactionID="9103" reportDate="20261020" />
<CashTransaction accountId="U1234567" currency="EUR" description="old" dateTime="20250101" amount="5.00" type="Other Fees" transactionID="1" reportDate="20250101" />
</CashTransactions>
</FlexStatement>
</FlexStatements>
</FlexQueryResponse>"#;

    #[test]
    fn parses_cents_without_floats() {
        assert_eq!(parse_cents("1234.56"), Some(123_456));
        assert_eq!(parse_cents("-0.09"), Some(-9));
        assert_eq!(parse_cents("1,234.50"), Some(123_450));
        assert_eq!(parse_cents("12,5"), Some(1_250));
        assert_eq!(parse_cents("0.005"), Some(1));
        assert_eq!(parse_cents("-0.005"), Some(-1));
        assert_eq!(parse_cents("100"), Some(10_000));
        assert_eq!(parse_cents(""), None);
        assert_eq!(parse_cents("abc"), None);
    }

    #[test]
    fn normalises_dates() {
        assert_eq!(norm_date("20261105;153000").as_deref(), Some("2026-11-05"));
        assert_eq!(
            norm_date("2026-11-05, 15:30:00").as_deref(),
            Some("2026-11-05")
        );
        assert_eq!(norm_date("20261105").as_deref(), Some("2026-11-05"));
        assert_eq!(norm_date(""), None);
    }

    #[test]
    fn decodes_entities_and_attributes() {
        assert_eq!(
            crate::text::decode_html_entities("A &amp; B &lt;&#65;&#x42;&gt; &bogus;"),
            "A & B <AB> &bogus;"
        );
        let tags = scan_tags(r#"<a x="1>2" y='z &amp; w'/><!-- <b/> --><c></c>"#);
        assert_eq!(tags.len(), 3);
        assert_eq!(tags[0].attrs["x"], "1>2");
        assert_eq!(tags[0].attrs["y"], "z & w");
        assert_eq!(tags[0].kind, TagKind::SelfClose);
        assert_eq!(tags[1].kind, TagKind::Open);
        assert_eq!(tags[2].kind, TagKind::Close);
    }

    #[test]
    fn parses_send_request_responses() {
        let ok = parse_status_response(OK_SEND).unwrap();
        assert!(ok.ok);
        assert_eq!(ok.reference_code.as_deref(), Some("1234567890"));
        assert!(trusted_statement_url(ok.url.as_deref().unwrap()));

        let bad = parse_status_response(BAD_TOKEN).unwrap();
        assert!(!bad.ok);
        assert!(matches!(flex_error(&bad), CoreError::Authentication { .. }));

        let wait = parse_status_response(IN_PROGRESS).unwrap();
        assert_eq!(wait.error_code.as_deref(), Some("1019"));
        assert!(parse_status_response("<html>").is_none());
    }

    #[test]
    fn only_ibkr_hosts_get_the_token() {
        assert!(trusted_statement_url(
            "https://gdcdyn.interactivebrokers.com/Universal/servlet/FlexStatementService.GetStatement"
        ));
        assert!(!trusted_statement_url(
            "http://ndcdyn.interactivebrokers.com/x"
        ));
        assert!(!trusted_statement_url(
            "https://evil.example/interactivebrokers.com"
        ));
        assert!(!trusted_statement_url(
            "https://notinteractivebrokers.com/x"
        ));
        assert!(!trusted_statement_url("nonsense"));
    }

    #[test]
    fn maps_accounts_balances_and_holdings() {
        let r = build_fetch(FLEX, "2026-01-01").unwrap();
        assert_eq!(r.accounts.len(), 2);
        let depot = &r.accounts[0];
        assert_eq!(depot.external_ref, "ibkr:U1234567:depot");
        assert_eq!(depot.kind, AccountKind::Brokerage);
        assert_eq!(depot.currency, "EUR");
        assert_eq!(depot.balance_date, "2026-11-12");
        // 1205.00 + 500.00 CHF*1.06 = 530.00 + 500.00 USD*0.9 = 450.00
        assert_eq!(depot.balance_cents, 120_500 + 53_000 + 45_000);
        let cash = &r.accounts[1];
        assert_eq!(cash.external_ref, "ibkr:U1234567:cash");
        assert_eq!(cash.balance_cents, 123_456);

        // LOT row skipped
        assert_eq!(r.holdings.len(), 3);
        let sap = &r.holdings[0].holding;
        assert_eq!(sap.isin, "DE0007164600");
        assert_eq!(sap.name, "SAP SE (SAP)");
        assert_eq!(sap.quantity, 10.0);
        assert_eq!(sap.current_price_cents, 12_050);
        assert_eq!(sap.purchase_price_cents, 10_025);
        assert_eq!(sap.value_cents, 120_500);
        let nestle = &r.holdings[1].holding;
        assert_eq!(nestle.name, "NESTLE SA-REG & Co (NESN)");
        assert_eq!(nestle.current_price_cents, 10_600);
        assert_eq!(nestle.purchase_price_cents, 9_540);
        assert_eq!(nestle.value_cents, 53_000);
        // Positions keep their own currency next to the base-currency figures (#625).
        assert_eq!(sap.currency, "");
        assert_eq!(sap.native_value_cents, None);
        assert_eq!(nestle.currency, "CHF");
        assert_eq!(nestle.native_price_cents, Some(10_000));
        assert_eq!(nestle.native_value_cents, Some(50_000));
        let apple = &r.holdings[2].holding;
        assert_eq!(apple.isin, "037833100"); // cusip fallback
        assert_eq!(apple.value_cents, 45_000);
        assert_eq!(apple.purchase_price_cents, 14_400); // 400.00 USD*0.9 / 2.5 = 144.00
    }

    #[test]
    fn maps_trades_dividends_and_fees() {
        let r = build_fetch(FLEX, "2026-01-01").unwrap();
        let by_id = |id: &str| {
            r.transactions
                .iter()
                .find(|t| t.external_id == format!("ibkr:U1234567:{id}"))
                .unwrap_or_else(|| panic!("missing {id}"))
        };
        // trade: -401.00 USD * 0.9 = -360.90 EUR
        let trade = by_id("9001");
        assert_eq!(trade.amount_cents, -36_090);
        assert_eq!(trade.booking_date, "2026-11-05");
        assert_eq!(trade.value_date, "2026-11-07");
        assert_eq!(trade.counterparty, "AAPL");
        assert_eq!(trade.purpose, "BUY 2.5 AAPL @ 160.00 USD (-401.00 USD)");
        assert_eq!(trade.account_ref, "ibkr:U1234567:cash");
        assert_eq!(by_id("9100").amount_cents, 57); // 0.63 * 0.9 = 0.567
        assert_eq!(by_id("9101").amount_cents, -8); // -0.09 * 0.9 = -0.081
        assert_eq!(by_id("9101").booking_text, "Withholding Tax");
        assert_eq!(by_id("9102").amount_cents, 200_000);
        assert_eq!(by_id("9102").counterparty, "Interactive Brokers");
        assert_eq!(by_id("9103").amount_cents, 1_590); // CHF 15.00 * 1.06
        assert!(by_id("9103").purpose.ends_with("(15.00 CHF)"));
        // ORDER-level trade, FX (CASH) trade and pre-`since` rows are dropped:
        // 1 trade + 5 cash transactions - old fee = 6 incl. the id-less interest
        assert_eq!(r.transactions.len(), 6);
        let ids: HashSet<_> = r.transactions.iter().map(|t| &t.external_id).collect();
        assert_eq!(ids.len(), 6);
        assert!(!ids.contains(&"ibkr:U1234567:9002".to_string()));
    }

    #[test]
    fn maps_security_transactions_with_cash_links() {
        let r = build_fetch(FLEX, "2026-01-01").unwrap();
        let by_id = |id: &str| {
            r.security_transactions
                .iter()
                .find(|t| t.external_id == format!("ibkr:U1234567:{id}"))
                .unwrap_or_else(|| panic!("missing {id}"))
        };
        let buy = by_id("9001");
        assert_eq!(buy.account_ref, "ibkr:U1234567:depot");
        assert_eq!(buy.cash_external_id, "ibkr:U1234567:9001");
        let t = &buy.transaction;
        assert_eq!(t.direction, SecurityDirection::Buy);
        assert_eq!(
            (t.quantity, t.price_cents, t.value_cents, t.fees_cents),
            (2.5, 16_000, 40_000, 100)
        );
        assert_eq!(t.currency, "USD");
        assert_eq!(t.fx_rate, Some(0.9));
        assert_eq!(t.settlement_date, "2026-11-07");
        let dividend = &by_id("9100").transaction;
        assert_eq!(dividend.direction, SecurityDirection::Dividend);
        assert_eq!(dividend.isin, "US0378331005");
        assert_eq!(dividend.value_cents, 63);
        assert_eq!(by_id("9101").transaction.direction, SecurityDirection::Tax);
        // FX trade, ORDER-level trade, deposits and interest are no securities transactions
        assert!(
            r.security_transactions
                .iter()
                .all(|t| t.external_id != "ibkr:U1234567:9002")
        );
        assert!(
            r.security_transactions
                .iter()
                .all(|t| t.external_id != "ibkr:U1234567:9102")
        );
        assert_eq!(r.security_transactions.len(), 4); // buy, 2 dividends, withholding tax
    }

    #[test]
    fn id_less_transactions_get_stable_content_ids() {
        let a = build_fetch(FLEX, "2026-01-01").unwrap();
        let b = build_fetch(FLEX, "2026-01-01").unwrap();
        let interest = |r: &FetchResult| {
            r.transactions
                .iter()
                .find(|t| t.booking_text == "Broker Interest Received")
                .unwrap()
                .external_id
                .clone()
        };
        assert!(!interest(&a).starts_with("ibkr:"));
        assert!(interest(&a).starts_with('c'));
        assert_eq!(interest(&a), interest(&b));
    }

    #[test]
    fn since_filters_old_bookings() {
        let r = build_fetch(FLEX, "2026-11-01").unwrap();
        assert!(
            r.transactions
                .iter()
                .all(|t| t.booking_date.as_str() >= "2026-11-01")
        );
        assert_eq!(r.transactions.len(), 4);
    }

    #[test]
    fn falls_back_to_equity_summary() {
        let xml = r#"<FlexQueryResponse><FlexStatements><FlexStatement accountId="U1" baseCurrency="CHF" toDate="2026-11-12">
<EquitySummaryByReportDateInBase reportDate="20261112" cash="100.00" total="1100.00" />
</FlexStatement></FlexStatements></FlexQueryResponse>"#;
        let r = build_fetch(xml, "2026-01-01").unwrap();
        assert_eq!(r.accounts[0].currency, "CHF");
        assert_eq!(r.accounts[0].balance_cents, 100_000); // total - cash
        assert_eq!(r.accounts[1].balance_cents, 10_000);
        assert!(r.holdings.is_empty());
    }

    #[test]
    fn rejects_reports_without_statements() {
        assert!(matches!(
            build_fetch("<FlexQueryResponse></FlexQueryResponse>", "2026-01-01"),
            Err(CoreError::Protocol { .. })
        ));
        assert!(matches!(
            build_fetch("<html/>", "2026-01-01"),
            Err(CoreError::Protocol { .. })
        ));
    }

    // ---- end-to-end against a local server

    /// Serves `responses` in order (last one repeats); returns the base URL
    /// and a log of the request lines.
    fn serve(responses: Vec<String>) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log2 = log.clone();
        let base2 = base.clone();
        std::thread::spawn(move || {
            for (n, stream) in listener.incoming().enumerate() {
                let Ok(mut s) = stream else { break };
                let mut buf = [0u8; 8192];
                let len = s.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..len]).to_string();
                log2.lock()
                    .unwrap()
                    .push(req.lines().next().unwrap_or("").to_string());
                let body = responses[n.min(responses.len() - 1)].replace("{BASE}", &base2);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(resp.as_bytes());
            }
        });
        (base, log)
    }

    fn ctx(token: &str, query: &str) -> ConnectContext {
        ConnectContext {
            connection_id: 1,
            login_name: token.into(),
            secret: crate::secret::SecretString::new(query.into()),
            since: "2026-01-01".into(),
            ..Default::default()
        }
    }

    fn connector(base: &str) -> IbkrConnector {
        let mut c = IbkrConnector::new();
        c.send_url = format!("{base}/SendRequest");
        c.poll_delay = Duration::from_millis(5);
        c.retry_delay = Duration::from_millis(5);
        c
    }

    fn send_ok() -> String {
        OK_SEND.replace(
            "https://ndcdyn.interactivebrokers.com/AccountManagement/FlexWebService/GetStatement",
            "{BASE}/GetStatement",
        )
    }

    #[tokio::test]
    async fn two_step_flow_retries_while_generating() {
        let (base, log) = serve(vec![
            send_ok(),
            IN_PROGRESS.into(),
            IN_PROGRESS.into(),
            FLEX.into(),
        ]);
        let mut c = connector(&base);
        let ctx = ctx("TOKEN123", "98765");
        assert!(matches!(c.begin(&ctx).await, Ok(Phase::Ready)));
        let r = c.fetch(&ctx).await.unwrap();
        assert_eq!(r.accounts.len(), 2);
        assert_eq!(r.holdings.len(), 3);
        let log = log.lock().unwrap();
        assert_eq!(log.len(), 4);
        assert!(log[0].starts_with("GET /SendRequest?"));
        assert!(
            log[0].contains("t=TOKEN123") && log[0].contains("q=98765") && log[0].contains("v=3")
        );
        assert!(log[1].starts_with("GET /GetStatement?"));
        assert!(log[1].contains("q=1234567890") && log[1].contains("t=TOKEN123"));
    }

    #[test]
    fn base_currency_comes_from_account_information() {
        let xml = r#"<FlexQueryResponse><FlexStatements><FlexStatement accountId="U1" toDate="2026-11-12">
<AccountInformation accountId="U1" currency="chf" />
<OpenPosition accountId="U1" currency="USD" fxRateToBase="0.9" symbol="AAPL" position="1" markPrice="200.00" positionValue="200.00" levelOfDetail="SUMMARY" />
<CashReportCurrency accountId="U1" currency="BASE_SUMMARY" endingCash="10.00" />
</FlexStatement></FlexStatements></FlexQueryResponse>"#;
        let r = build_fetch(xml, "2026-01-01").unwrap();
        assert!(r.accounts.iter().all(|a| a.currency == "CHF"));
        // Total is converted to the base currency: 200 USD * 0.9.
        assert_eq!(r.accounts[0].balance_cents, 18000);
    }

    #[test]
    fn multi_currency_depot_keeps_native_positions_and_base_total() {
        let xml = r#"<FlexQueryResponse><FlexStatements><FlexStatement accountId="U1" baseCurrency="CHF" toDate="2026-11-12">
<CashReportCurrency accountId="U1" currency="BASE_SUMMARY" endingCash="250.00" />
<CashReportCurrency accountId="U1" currency="USD" endingCash="100.00" />
<OpenPosition accountId="U1" currency="USD" fxRateToBase="0.8" symbol="AAPL" description="APPLE INC" isin="US0378331005" position="10" markPrice="200.00" costBasisPrice="150.00" positionValue="2000.00" levelOfDetail="SUMMARY" />
<OpenPosition accountId="U1" currency="EUR" fxRateToBase="0.95" symbol="SAP" description="SAP SE" isin="DE0007164600" position="5" markPrice="120.00" costBasisMoney="500.00" positionValue="600.00" levelOfDetail="SUMMARY" />
<OpenPosition accountId="U1" currency="CHF" fxRateToBase="1" symbol="NESN" description="NESTLE" isin="CH0038863350" position="4" markPrice="100.00" costBasisPrice="90.00" positionValue="400.00" levelOfDetail="SUMMARY" />
</FlexStatement></FlexStatements></FlexQueryResponse>"#;
        let r = build_fetch(xml, "2026-01-01").unwrap();
        // Accounts and cash stay in the base currency.
        assert!(r.accounts.iter().all(|a| a.currency == "CHF"));
        assert_eq!(r.accounts[1].balance_cents, 25_000);
        // Depot total: 2000*0.8 + 600*0.95 + 400 = 1600 + 570 + 400 CHF.
        assert_eq!(r.accounts[0].balance_cents, 160_000 + 57_000 + 40_000);

        let h: Vec<_> = r.holdings.iter().map(|h| &h.holding).collect();
        let (aapl, sap, nesn) = (h[0], h[1], h[2]);
        assert_eq!(aapl.currency, "USD");
        assert_eq!(aapl.native_price_cents, Some(20_000));
        assert_eq!(aapl.native_value_cents, Some(200_000));
        assert_eq!(aapl.native_purchase_price_cents, Some(15_000));
        assert_eq!(
            (aapl.current_price_cents, aapl.value_cents),
            (16_000, 160_000)
        );
        assert_eq!(sap.currency, "EUR");
        assert_eq!(sap.native_price_cents, Some(12_000));
        assert_eq!(sap.native_value_cents, Some(60_000));
        assert_eq!(sap.native_purchase_price_cents, Some(10_000)); // 500.00 / 5
        assert_eq!((sap.current_price_cents, sap.value_cents), (11_400, 57_000));
        assert_eq!(nesn.currency, "");
        assert_eq!(nesn.native_value_cents, None);
        assert_eq!(nesn.value_cents, 40_000);
        assert_eq!(
            h.iter().map(|x| x.value_cents).sum::<i64>(),
            r.accounts[0].balance_cents
        );
    }

    #[test]
    fn base_currency_defaults_to_eur_only_when_absent() {
        let xml = r#"<FlexQueryResponse><FlexStatements><FlexStatement accountId="U1" toDate="2026-11-12"></FlexStatement></FlexStatements></FlexQueryResponse>"#;
        let r = build_fetch(xml, "2026-01-01").unwrap();
        assert_eq!(r.accounts[0].currency, "EUR");
    }

    #[test]
    fn error_chain_lists_all_causes() {
        #[derive(Debug)]
        struct Outer(std::io::Error);
        impl std::fmt::Display for Outer {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "error sending request")
            }
        }
        impl std::error::Error for Outer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }
        let e = Outer(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "connection reset by peer",
        ));
        assert_eq!(
            error_chain(&e),
            "error sending request: connection reset by peer"
        );
    }

    #[tokio::test]
    async fn network_error_after_retries_names_the_cause() {
        // Nothing listens on this port.
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        let mut c = connector(&format!("http://127.0.0.1:{port}"));
        let err = c.fetch(&ctx("T", "Q")).await.unwrap_err();
        match err {
            CoreError::Network { message } => {
                assert!(message.contains("error sending request: "), "{message}");
                assert!(message.contains("Verbindung fehlgeschlagen"), "{message}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn gives_up_when_statement_never_arrives() {
        let (base, log) = serve(vec![send_ok(), IN_PROGRESS.into()]);
        let mut c = connector(&base);
        c.max_polls = 3;
        let err = c.fetch(&ctx("T", "Q")).await.unwrap_err();
        assert!(
            matches!(err, CoreError::Bank { ref code, .. } if code == "1019"),
            "{err:?}"
        );
        assert_eq!(log.lock().unwrap().len(), 4); // SendRequest + 3 polls
    }

    #[tokio::test]
    async fn invalid_token_is_an_authentication_error_without_token() {
        let (base, _) = serve(vec![BAD_TOKEN.into()]);
        let mut c = connector(&base);
        let err = c.fetch(&ctx("SECRETTOKEN", "Q")).await.unwrap_err();
        assert!(matches!(err, CoreError::Authentication { .. }), "{err:?}");
        assert!(!format!("{err:?}").contains("SECRETTOKEN"));
    }

    #[tokio::test]
    async fn refuses_foreign_statement_url() {
        let evil = OK_SEND.replace(
            "https://ndcdyn.interactivebrokers.com/AccountManagement/FlexWebService/GetStatement",
            "https://evil.example/steal",
        );
        let (base, log) = serve(vec![evil]);
        let mut c = connector(&base);
        assert!(matches!(
            c.fetch(&ctx("T", "Q")).await,
            Err(CoreError::Protocol { .. })
        ));
        assert_eq!(log.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn begin_requires_both_credentials() {
        let mut c = IbkrConnector::new();
        assert!(matches!(
            c.begin(&ctx("", "123")).await,
            Err(CoreError::Authentication { .. })
        ));
        assert!(matches!(
            c.begin(&ctx("tok", " ")).await,
            Err(CoreError::Authentication { .. })
        ));
    }
}
