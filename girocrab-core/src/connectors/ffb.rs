//! FFB (FIL Fondsbank, www.ffb.de) fund depots: positions and depot
//! transactions. The settlement account (Abwicklungskonto) is deliberately
//! left out: only the depot is synced. There is no API: the
//! connector reads the HTML pages of the customer area (unofficial, the pages
//! may change at any time). Page shapes checked live 2026-11-17.
//!
//!  GET  /login/login.jsp                         -> login form (hidden `CSRF_TOKEN`, `login`, `pin`)
//!  POST <form action>                            -> redirects to `/diverse/indexKunde.jsp;jsessionid=…`;
//!                                                   the login form again = wrong credentials
//!  GET  /api/kundenansicht/depotuebersicht/single/overview
//!                                                -> depot number, "Gesamtbestand im Depot"
//!  GET  …/single/fundholdings/, …/single/vlfundholdings/
//!                                                -> table `#view`: positions (VL = savings contracts)
//!  GET  /api/kundenansicht/depotentwicklung/positionenView?…
//!                                                -> invested capital per position (best effort)
//!  GET  /api/kundenansicht/transaktion/query/?datumVon=…&datumBis=…
//!                                                -> table `#view`, 20 orders per page, link "Nächste"
//!  GET  /api/kundenansicht/postfach/overview, …/postfach/query?datumVon=…&datumBis=…
//!                                                -> table `#view`, 50 documents per page, link "Nächste"
//!  GET  /api/kundenansicht/postfach/document/<id> -> the PDF
//!  GET  /action/logout                           -> best effort
//!
//! The session id travels in the URL (`;jsessionid=…`), like the site's own
//! links. Order rows are `tr.parentRow` (id = order number); an order with
//! several bookings (income + reinvestment, internal transfer, cancellation)
//! has them as rows with `name=<order number>`, and only those are booked.
//! Amounts and quantities carry no sign: the direction comes from the
//! transaction type (`direction_of`).

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use async_trait::async_trait;
use regex::Regex;
use zeroize::Zeroize;

use super::html::{
    FORM, INPUT, LINK, attr, attrs, de_date, element_by_id, parse_date, table_cells, table_texts,
    text,
};
use super::{
    BankConnector, ChallengeResponse, ConnectContext, Documents, FetchResult, KnownDocuments,
    Phase, RemoteAccount, RemoteHolding, RemoteSecurityTransaction, http_client_same_host,
};
use crate::documents::RemoteDocument;
use crate::error::{CoreError, CoreResult};
use crate::i18n::tr;
use crate::models::{AccountKind, Holding, SecurityDirection, SecurityTransaction};

const HOST: &str = "www.ffb.de";
const BASE: &str = "https://www.ffb.de";
/// Account external refs: `ffb:depot:<depot no.>`, `ffb:konto:<account no.>`.
pub(crate) const REF_PREFIX: &str = "ffb:";
/// Upper bound for paged lists (20 rows per page).
const MAX_PAGES: usize = 500;
/// Earliest start the depot development pages accept.
const DEVELOPMENT_START: &str = "01.01.2009";
/// Start of the document search ("Gesamt" in the Postfach date picker).
const POSTFACH_START: &str = "01.01.2000";
/// PDFs per run; the rest follows with the next fetch.
const MAX_DOCUMENT_DOWNLOADS: usize = 200;

static DOCUMENT_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"/api/kundenansicht/postfach/document/(\d+)").unwrap());
static ROW: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<tr\b([^>]*)>(.*?)</tr\s*>").unwrap());
static SESSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r";jsessionid=([A-Za-z0-9_.!\-]+)").unwrap());
static DEPOT_NO: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Depotübersicht\s+Nr\.\s*(\d{4,})").unwrap());
static TOTAL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Gesamtbestand im Depot:\s*([\d.]+,\d{2})\s*EUR").unwrap());
static WKN_ISIN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b([A-Z0-9]{6})\s*/\s*([A-Z]{2}[A-Z0-9]{9}[0-9])\b").unwrap());
/// A number followed by its currency ("1.234,56 EUR", "-0,01 €", "67,6300 USD").
static MONEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"([-+\u{2212}]?\s?\d[\d.]*(?:,\d+)?)\s*(€|[A-Z]{3}\b)").unwrap());
static TITLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?is)\btitle\s*=\s*"([^"]*)""#).unwrap());
static DETAIL_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"showDetailsModal\(\s*(\d+)\s*\)").unwrap());
/// `fonwknext=A0TEST&depktoart=DE&depunterkto=0` in a position link.
static POSITION_LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"fonwknext=([A-Z0-9]+)&(?:amp;)?depktoart=([A-Z]+)&(?:amp;)?depunterkto=(\d+)")
        .unwrap()
});
/// `'<depot>-DE-0-A0TEST'` in the development page.
static POSITION_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"SinglePosition\(\s*'\d+-([A-Z]+)-(\d+)-([A-Z0-9]+)'").unwrap());

fn page_changed() -> CoreError {
    CoreError::protocol(tr("FFB: Anmeldeseite nicht erkannt (Webseite geändert?)."))
}

fn invalid() -> CoreError {
    CoreError::invalid(tr(
        "FFB: Unvollständige oder ungültige Antwort. Es wurden keine Daten übernommen.",
    ))
}

fn wrong_credentials() -> CoreError {
    CoreError::Authentication {
        message: tr("FFB: Anmeldung fehlgeschlagen. Bitte Login und Passwort/PIN prüfen.").into(),
    }
}

fn session_ended() -> CoreError {
    CoreError::Authentication {
        message: tr("FFB: Die Sitzung wurde beendet. Bitte den Abruf erneut starten.").into(),
    }
}

fn rejected(status: reqwest::StatusCode) -> CoreError {
    CoreError::Bank {
        code: status.as_u16().to_string(),
        message: tr("FFB: Die Anfrage wurde abgelehnt.").into(),
    }
}

/// Only `https://www.ffb.de` may receive the password, cookies or session id.
fn ensure_ffb(url: &url::Url) -> CoreResult<()> {
    if url.scheme() == "https" && url.host_str() == Some(HOST) {
        return Ok(());
    }
    Err(CoreError::protocol(tr(
        "FFB: Unerwartete Weiterleitung zu einer fremden Adresse – abgebrochen.",
    )))
}

// ------------------------------------------------------------------ parsing helpers

/// German decimal ("1.234,567") → f64.
fn parse_decimal(s: &str) -> Option<f64> {
    let t: String = s
        .trim()
        .replace('\u{2212}', "-")
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '.')
        .collect();
    if !t.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    t.replace(',', ".").parse().ok()
}

/// Every `(cents, currency)` in a text, in order. Prices with four decimals
/// are rounded to cents.
fn money(s: &str) -> Vec<(i64, String)> {
    MONEY
        .captures_iter(s)
        .filter_map(|c| {
            let n: String = c[1]
                .replace('\u{2212}', "-")
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            let cents = crate::money::parse_cents_rounded(&n).ok()?;
            let cur = if &c[2] == "€" { "EUR" } else { &c[2] };
            Some((cents, cur.to_string()))
        })
        .collect()
}

/// First amount in EUR and first amount in another currency of a cell.
fn eur_and_native(s: &str) -> (Option<i64>, Option<(i64, String)>) {
    let all = money(s);
    let eur = all.iter().find(|(_, c)| c == "EUR").map(|(v, _)| *v);
    let native = all.into_iter().find(|(_, c)| c != "EUR");
    (eur, native)
}

/// Session id from a URL or page (`;jsessionid=…`).
fn session_id(s: &str) -> Option<String> {
    SESSION.captures(s).map(|c| c[1].to_string())
}

/// Link of the "Nächste" pager entry, if there is a next page.
fn next_page(html: &str) -> Option<String> {
    LINK.find_iter(html).find_map(|m| {
        let a = m.as_str();
        if text(a) != "Nächste" {
            return None;
        }
        let open = &a[..a.find('>')?];
        attr(&attrs(open), "href")
            .filter(|h| h.starts_with('/'))
            .map(str::to_string)
    })
}

// ------------------------------------------------------------------ login form

#[derive(Debug, PartialEq)]
struct LoginForm {
    action: url::Url,
    /// Hidden fields (CSRF token), as the browser sends them.
    fields: Vec<(String, String)>,
    login_field: String,
    password_field: String,
}

impl LoginForm {
    fn filled(&self, login: &str, password: &str) -> Vec<(String, String)> {
        let mut out = self.fields.clone();
        out.push((self.login_field.clone(), login.to_string()));
        out.push((self.password_field.clone(), password.to_string()));
        out
    }
}

fn has_password_field(form: &str) -> bool {
    INPUT
        .captures_iter(form)
        .any(|c| attr(&attrs(&c[1]), "type").is_some_and(|t| t.eq_ignore_ascii_case("password")))
}

/// The site shows its login form (wrong credentials or session ended).
fn is_login_page(html: &str) -> bool {
    FORM.captures_iter(html).any(|f| {
        has_password_field(&f[2])
            && INPUT
                .captures_iter(&f[2])
                .any(|i| attr(&attrs(&i[1]), "name") == Some("login"))
    })
}

fn login_form(html: &str, page: &url::Url) -> CoreResult<LoginForm> {
    let form = FORM
        .captures_iter(html)
        .find(|f| has_password_field(&f[2]))
        .ok_or_else(page_changed)?;
    let form_attrs = attrs(&form[1]);
    if !attr(&form_attrs, "method").is_some_and(|m| m.eq_ignore_ascii_case("post")) {
        return Err(page_changed()); // never put the password into a URL
    }
    let action = match attr(&form_attrs, "action").map(str::trim) {
        Some(a) if !a.is_empty() => page.join(a).map_err(|_| page_changed())?,
        _ => page.clone(),
    };
    ensure_ffb(&action)?;
    let mut fields = Vec::new();
    let mut login_field = None;
    let mut password_field = None;
    for input in INPUT.captures_iter(&form[2]) {
        let a = attrs(&input[1]);
        let Some(name) = attr(&a, "name").filter(|n| !n.is_empty()) else {
            continue;
        };
        match attr(&a, "type")
            .unwrap_or("text")
            .to_ascii_lowercase()
            .as_str()
        {
            "password" => password_field = Some(name.to_string()),
            "hidden" => fields.push((name.to_string(), attr(&a, "value").unwrap_or("").into())),
            "text" | "email" if login_field.is_none() => login_field = Some(name.to_string()),
            _ => {}
        }
    }
    Ok(LoginForm {
        action,
        fields,
        login_field: login_field.ok_or_else(page_changed)?,
        password_field: password_field.ok_or_else(page_changed)?,
    })
}

// ------------------------------------------------------------------ depot pages

/// Depot number and "Gesamtbestand im Depot" (cents) of the overview page.
fn parse_overview(html: &str) -> CoreResult<(String, Option<i64>)> {
    let plain = text(html);
    let no = DEPOT_NO
        .captures(&plain)
        .map(|c| c[1].to_string())
        .ok_or_else(invalid)?;
    let total = TOTAL
        .captures(&plain)
        .and_then(|c| crate::money::parse_cents(&c[1]).ok());
    Ok((no, total))
}

/// Positions of a holdings fragment as `(position key "DE-0-<WKN>", holding)`.
/// No table = no positions; a position with unreadable figures rejects the page.
fn parse_holdings(html: &str, savings_plan: bool) -> CoreResult<Vec<(String, Holding)>> {
    let mut out = Vec::new();
    let Some(table) = element_by_id(html, "view") else {
        return Ok(out);
    };
    for r in ROW.captures_iter(table) {
        let row = r.get(0).map_or("", |m| m.as_str());
        let cells = table_cells(row).into_iter().next().unwrap_or_default();
        if cells.len() < 5 {
            continue; // header, sum row
        }
        let first = text(cells[0]);
        let Some(ids) = WKN_ISIN.captures(&first) else {
            continue;
        };
        let (wkn, isin) = (ids[1].to_string(), ids[2].to_string());
        let mut name = TITLE
            .captures(cells[0])
            .map(|c| crate::text::decode_html_entities(c[1].trim()))
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| {
                first[..ids.get(0).map_or(0, |m| m.start())]
                    .trim()
                    .to_string()
            });
        if savings_plan {
            name.push_str(" (VL)");
        }
        let quantity = parse_decimal(&text(cells[1])).ok_or_else(invalid)?;
        let (price_eur, price_native) = eur_and_native(&text(cells[2]));
        let (value, value_native) = eur_and_native(&text(cells[4]));
        let value_cents = value.ok_or_else(invalid)?;
        let foreign = price_native.as_ref().map(|(_, c)| c.clone());
        let current_price_cents = match price_eur {
            Some(p) if foreign.is_none() => p,
            _ if quantity > 0.0 => (value_cents as f64 / quantity).round() as i64,
            _ => 0,
        };
        let key = POSITION_LINK
            .captures(row)
            .map(|c| format!("{}-{}-{}", &c[2], &c[3], &c[1]))
            .unwrap_or_else(|| format!("{}-{wkn}", if savings_plan { "VL" } else { "DE" }));
        out.push((
            key,
            Holding {
                isin,
                wkn,
                name,
                quantity,
                current_price_cents,
                value_cents,
                currency: foreign.clone().unwrap_or_default(),
                native_price_cents: foreign.as_ref().and(price_native.map(|(v, _)| v)),
                native_value_cents: foreign.as_ref().and(
                    value_native
                        .filter(|(_, c)| Some(c) == foreign.as_ref())
                        .map(|(v, _)| v),
                ),
                ..Default::default()
            },
        ));
    }
    Ok(out)
}

/// Invested capital ("Eingesetztes Kapital", cents) per position key
/// `DE-0-<WKN>` from the development page's fund view.
fn parse_invested(html: &str) -> HashMap<String, i64> {
    let mut out = HashMap::new();
    for part in html.split("class='detailRow'").skip(1) {
        let Some(id) = POSITION_ID.captures(part) else {
            continue;
        };
        let Some(at) = part.find("width:140px") else {
            continue;
        };
        let rest = &part[at..];
        let cell = rest.find('>').map_or(rest, |i| &rest[i + 1..]);
        if let Some((v, _)) = money(&text(cell)).into_iter().find(|(_, c)| c == "EUR") {
            out.insert(format!("{}-{}-{}", &id[1], &id[2], &id[3]), v);
        }
    }
    out
}

// ------------------------------------------------------------------ transactions

/// Direction of a transaction type; `None` = not booked (cancellations).
fn direction_of(kind: &str) -> Option<SecurityDirection> {
    let k = kind.to_lowercase();
    Some(if k.contains("storn") {
        return None;
    } else if k.contains("wiederanlage") {
        SecurityDirection::Buy
    } else if k.contains("entgelt") {
        SecurityDirection::Fee
    } else if k.contains("vorabpauschale") || k.contains("steuer") {
        SecurityDirection::Tax
    } else if k.starts_with("ertr") {
        SecurityDirection::Dividend
    } else if k.contains("verkauf") || k.contains("rücknahme") {
        SecurityDirection::Sell
    } else if k.contains("kauf") {
        SecurityDirection::Buy
    } else {
        // Einlieferung, Übertrag, Delta-Korrektur, Fondsmaßnahme …
        SecurityDirection::Other
    })
}

/// Security transactions of one page of the transaction search, and the link
/// to the next page. `names` maps ISIN → full fund name (the list truncates).
fn parse_transactions(
    html: &str,
    depot_ref: &str,
    since: &str,
    names: &HashMap<String, String>,
) -> CoreResult<(Vec<RemoteSecurityTransaction>, Option<String>)> {
    let mut out = Vec::new();
    let Some(table) = element_by_id(html, "view") else {
        return Ok((out, None));
    };
    let all = ROW
        .captures_iter(table)
        .map(|r| {
            let row = r.get(0).map_or("", |m| m.as_str());
            let a = attrs(&r[1]);
            let cells = table_texts(row).into_iter().next().unwrap_or_default();
            let class = attr(&a, "class").unwrap_or("").to_string();
            let order = attr(&a, "name")
                .or_else(|| attr(&a, "id"))
                .unwrap_or("")
                .to_string();
            let child = attr(&a, "name").is_some();
            let detail = DETAIL_ID.captures(row).map(|c| c[1].to_string());
            (class, order, child, detail, cells)
        })
        .filter(|(_, _, _, _, cells)| cells.len() >= 6)
        .collect::<Vec<_>>();
    let with_children: HashSet<&str> = all
        .iter()
        .filter(|(_, _, child, _, _)| *child)
        .map(|(_, order, _, _, _)| order.as_str())
        .collect();
    let mut seen: HashMap<String, u32> = HashMap::new();
    for (class, order, child, detail, c) in &all {
        let n = seen.entry(order.clone()).or_insert(0);
        let idx = *n;
        *n += 1;
        if !child && class.contains("parentRow") && with_children.contains(order.as_str()) {
            continue; // summary row of an order with its own booking rows
        }
        let Some(date) = parse_date(&c[0]) else {
            continue;
        };
        let Some(direction) = direction_of(&c[1]) else {
            continue;
        };
        let (eur, native) = eur_and_native(&c[5]);
        let value = eur.unwrap_or(0).abs();
        let quantity = parse_decimal(&c[4]).unwrap_or(0.0).abs();
        if value == 0 && quantity == 0.0 {
            continue; // zero corrections, empty tax statements
        }
        let (wkn, isin, short) = match WKN_ISIN.captures(&c[2]) {
            Some(m) => (
                m[1].to_string(),
                m[2].to_string(),
                c[2][..m.get(0).map_or(0, |x| x.start())].trim().to_string(),
            ),
            None => (String::new(), String::new(), c[2].trim().to_string()),
        };
        let name = names.get(&isin).cloned().unwrap_or(short);
        let fx_rate = native
            .filter(|(v, _)| *v != 0 && value != 0)
            .map(|(v, _)| v.abs() as f64 / value as f64);
        let currency_native = eur_and_native(&c[3]).1.map(|(_, cur)| cur);
        let id = match detail {
            Some(d) => format!("{REF_PREFIX}{d}"),
            None if !order.is_empty() => format!("{REF_PREFIX}o{order}:{idx}"),
            None => return Err(invalid()),
        };
        if date.as_str() < since {
            continue;
        }
        let mut booking_text = c[1].clone();
        if let Some(cur) = currency_native {
            booking_text.push_str(&format!(" ({cur})"));
        }
        out.push(RemoteSecurityTransaction {
            account_ref: depot_ref.to_string(),
            external_id: id,
            cash_external_id: String::new(),
            transaction: SecurityTransaction {
                settlement_date: parse_date(&c[3]).unwrap_or_else(|| date.clone()),
                booking_date: date,
                isin,
                wkn,
                name,
                direction,
                quantity,
                price_cents: if quantity > 0.0 {
                    (value as f64 / quantity).round() as i64
                } else {
                    0
                },
                currency: "EUR".into(),
                value_cents: value,
                fx_rate,
                booking_text,
                ..Default::default()
            },
        });
    }
    Ok((out, next_page(html)))
}

// ------------------------------------------------------------------ Postfach

#[derive(Debug, Clone, PartialEq)]
struct PostfachRow {
    id: String,
    date: String,
    category: String,
    kind: String,
}

/// Rows of the Postfach result table: read mark, date, category, document
/// type, WKNs, PDF link, postage order.
fn parse_postfach(html: &str) -> Vec<PostfachRow> {
    let Some(table) = element_by_id(html, "view") else {
        return Vec::new();
    };
    ROW.captures_iter(table)
        .filter_map(|r| {
            let row = r.get(0)?.as_str();
            let id = DOCUMENT_ID.captures(row)?[1].to_string();
            let cells = table_cells(row).into_iter().next()?;
            if cells.len() < 6 {
                return None;
            }
            Some(PostfachRow {
                id,
                date: parse_date(&text(cells[1]))?,
                category: text(cells[2]),
                kind: text(cells[3]),
            })
        })
        .collect()
}

fn document_type(row: &PostfachRow) -> &'static str {
    let kind = row.kind.to_lowercase();
    if row.category == "Steuerunterlagen" || kind.contains("steuer") {
        "steuer"
    } else if kind.contains("auszug") || kind.contains("auszuege") {
        "kontoauszug"
    } else {
        "postbox"
    }
}

/// `YYYY-MM-DD_<type>_<id>.pdf`, without path separators.
fn document_file_name(row: &PostfachRow) -> String {
    let kind: String = row
        .kind
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect();
    format!("{}_{}_{}.pdf", row.date, kind, row.id)
}

// ------------------------------------------------------------------ connector

#[derive(Default)]
pub struct FfbConnector {
    client: Option<reqwest::Client>,
    session: String,
}

impl FfbConnector {
    pub fn new() -> Self {
        Self::default()
    }

    /// `path` with the session id and `query` appended.
    fn url(&self, path: &str, query: &[(&str, &str)]) -> String {
        let mut u = format!("{BASE}{path};jsessionid={}", self.session);
        if !query.is_empty() {
            let q: String = url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs(query)
                .finish();
            u.push('?');
            u.push_str(&q);
        }
        u
    }

    /// GET a page; the login form instead means the session ended.
    async fn get(&self, url: &str) -> CoreResult<String> {
        let client = self.client.as_ref().ok_or(CoreError::SessionClosed)?;
        let target = url::Url::parse(url).map_err(|_| invalid())?;
        ensure_ffb(&target)?;
        let resp = client.get(target).send().await?;
        ensure_ffb(resp.url())?;
        let status = resp.status();
        let html = super::read_text_limited(resp).await?;
        if !status.is_success() {
            return Err(rejected(status));
        }
        if is_login_page(&html) {
            return Err(session_ended());
        }
        Ok(html)
    }

    async fn page(&self, path: &str, query: &[(&str, &str)]) -> CoreResult<String> {
        self.get(&self.url(path, query)).await
    }

    /// One Postfach PDF; `None` when the bank answers with anything else.
    async fn download(&self, id: &str) -> CoreResult<Option<Vec<u8>>> {
        let client = self.client.as_ref().ok_or(CoreError::SessionClosed)?;
        let path = format!("/api/kundenansicht/postfach/document/{id}");
        let target = url::Url::parse(&self.url(&path, &[])).map_err(|_| invalid())?;
        ensure_ffb(&target)?;
        let resp = client.get(target).send().await?;
        ensure_ffb(resp.url())?;
        let status = resp.status();
        let body = super::read_body_limited(resp, super::MAX_DOC_BYTES).await?;
        if !status.is_success() {
            return Err(rejected(status));
        }
        Ok(body.starts_with(b"%PDF").then_some(body))
    }

    /// Postfach PDFs not in `known`, newest first. A failed download ends the
    /// run; what was loaded until then is kept and the rest follows next time.
    async fn documents(
        &self,
        depot_ref: &str,
        today: &str,
        known: &mut KnownDocuments,
    ) -> CoreResult<Vec<RemoteDocument>> {
        self.page("/api/kundenansicht/postfach/overview", &[])
            .await?;
        let end = de_date(today).unwrap_or_default();
        let mut next = Some(self.url(
            "/api/kundenansicht/postfach/query",
            &[
                ("datumVon", POSTFACH_START),
                ("datumBis", end.as_str()),
                ("applicationCategory", "UNDEFINED"),
                ("wkn", ""),
            ],
        ));
        let mut rows = Vec::new();
        let mut pages = 0;
        while let Some(url) = next.take() {
            let html = self.get(&url).await?;
            rows.extend(parse_postfach(&html));
            pages += 1;
            if pages < MAX_PAGES {
                next = next_page(&html).map(|href| format!("{BASE}{href}"));
            }
        }
        rows.retain(|r| !known.contains(&r.id));
        rows.truncate(MAX_DOCUMENT_DOWNLOADS);
        let mut out = Vec::new();
        for row in rows {
            let bytes = match self.download(&row.id).await {
                Ok(Some(bytes)) => bytes,
                Ok(None) => continue,
                Err(_) => break,
            };
            known.insert(row.id.clone());
            out.push(RemoteDocument {
                account_ref: depot_ref.to_string(),
                booking_external_id: None,
                title: row.kind.clone(),
                doc_type: document_type(&row).into(),
                mime_type: "application/pdf".into(),
                file_name: document_file_name(&row),
                bytes,
                source_url: format!("ffb:postfach:{}", row.id),
                date: row.date.clone(),
            });
        }
        Ok(out)
    }

    async fn fetch_depot(
        &self,
        ctx: &ConnectContext,
        today: &str,
        out: &mut FetchResult,
    ) -> CoreResult<()> {
        let overview = self
            .page("/api/kundenansicht/depotuebersicht/single/overview", &[])
            .await?;
        let (depot_no, total) = parse_overview(&overview)?;
        let depot_ref = format!("{REF_PREFIX}depot:{depot_no}");

        let mut positions = parse_holdings(
            &self
                .page(
                    "/api/kundenansicht/depotuebersicht/single/fundholdings/",
                    &[],
                )
                .await?,
            false,
        )?;
        // Customers without savings contracts may get no fragment at all.
        if let Ok(vl) = self
            .page(
                "/api/kundenansicht/depotuebersicht/single/vlfundholdings/",
                &[],
            )
            .await
        {
            positions.extend(parse_holdings(&vl, true)?);
        }
        let end = de_date(today).unwrap_or_default();
        let invested = match self
            .page(
                "/api/kundenansicht/depotentwicklung/positionenView",
                &[
                    ("startDate", DEVELOPMENT_START),
                    ("endDate", end.as_str()),
                    ("kostenSteuernImpact", "VOR_KOSTEN_STEUERN"),
                    ("filter", "MIT_BESTAND"),
                ],
            )
            .await
        {
            Ok(html) => parse_invested(&html),
            Err(_) => HashMap::new(),
        };
        let mut names = HashMap::new();
        let mut sum = 0i64;
        for (key, mut h) in positions {
            if let Some(capital) = invested.get(&key).filter(|_| h.quantity > 0.0) {
                h.purchase_price_cents = (*capital as f64 / h.quantity).round() as i64;
            }
            sum += h.value_cents;
            names
                .entry(h.isin.clone())
                .or_insert_with(|| h.name.trim_end_matches(" (VL)").to_string());
            out.holdings.push(RemoteHolding {
                account_ref: depot_ref.clone(),
                holding: h,
                ..Default::default()
            });
        }
        out.holdings_fetched.push(depot_ref.clone());
        out.accounts.push(RemoteAccount {
            external_ref: depot_ref.clone(),
            name: "FFB Fondsdepot".into(),
            owner: String::new(),
            iban: String::new(),
            account_number: depot_no,
            currency: "EUR".into(),
            kind: AccountKind::Brokerage,
            balance_cents: total.unwrap_or(sum),
            balance_date: today.to_string(),
        });

        let since = ctx.since_for(&depot_ref).to_string();
        let from = de_date(&since).unwrap_or_else(|| "01.01.2008".into());
        let mut next = Some(self.url(
            "/api/kundenansicht/transaktion/query/",
            &[
                ("transactionType", ""),
                ("datumVon", from.as_str()),
                ("product", ""),
                ("wknIsin", ""),
                ("betragVon", ""),
                ("datumBis", end.as_str()),
                ("betragBis", ""),
            ],
        ));
        let mut pages = 0;
        while let Some(url) = next.take() {
            let html = self.get(&url).await?;
            let (tx, more) = parse_transactions(&html, &depot_ref, &since, &names)?;
            out.security_transactions.extend(tx);
            pages += 1;
            if pages < MAX_PAGES {
                next = more.map(|href| format!("{BASE}{href}"));
            }
        }
        Ok(())
    }
}

#[async_trait]
impl BankConnector for FfbConnector {
    async fn begin(&mut self, ctx: &ConnectContext) -> CoreResult<Phase> {
        let login = ctx.login_name.trim();
        if login.is_empty() || ctx.secret.expose().is_empty() {
            return Err(CoreError::Authentication {
                message: tr("FFB: Login/Benutzername und Passwort/PIN erforderlich.").into(),
            });
        }
        let client = http_client_same_host()?;
        let resp = client.get(format!("{BASE}/login/login.jsp")).send().await?;
        let page_url = resp.url().clone();
        ensure_ffb(&page_url)?;
        let html = super::read_text_limited(resp).await?;
        let form = login_form(&html, &page_url)?;

        let mut fields = form.filled(login, ctx.secret.expose());
        let sent = client.post(form.action.clone()).form(&fields).send().await;
        for (_, v) in fields.iter_mut() {
            v.zeroize();
        }
        let resp = sent?;
        ensure_ffb(resp.url())?;
        let status = resp.status();
        let final_url = resp.url().to_string();
        let html = super::read_text_limited(resp).await?;
        if is_login_page(&html) {
            return Err(wrong_credentials());
        }
        if !status.is_success() {
            return Err(rejected(status));
        }
        let session = session_id(&final_url)
            .or_else(|| session_id(&html))
            .ok_or_else(|| CoreError::protocol(tr("FFB: Anmeldung nicht abgeschlossen. Die FFB verlangt möglicherweise eine zusätzliche Bestätigung, die GiroCrab noch nicht unterstützt.")))?;
        self.client = Some(client);
        self.session = session;
        Ok(Phase::Ready)
    }

    async fn respond(&mut self, _ctx: &ConnectContext, _r: ChallengeResponse) -> CoreResult<Phase> {
        Ok(Phase::Ready)
    }

    async fn fetch(&mut self, ctx: &ConnectContext) -> CoreResult<FetchResult> {
        let today = super::today();
        let mut known = KnownDocuments::from_state(&ctx.state);

        // "Kontoauszüge abrufen": documents only.
        if let Documents::StatementsOnly(only) = &ctx.documents {
            let overview = self
                .page("/api/kundenansicht/depotuebersicht/single/overview", &[])
                .await?;
            let (depot_no, _) = parse_overview(&overview)?;
            if *only != format!("{REF_PREFIX}depot:{depot_no}") {
                return Ok(FetchResult::default());
            }
            let documents = self.documents(only, &today, &mut known).await?;
            return Ok(FetchResult {
                documents,
                state: known.to_state(),
                ..Default::default()
            });
        }

        let mut out = FetchResult::default();
        self.fetch_depot(ctx, &today, &mut out).await?;
        let due = ctx.documents.due_statements();
        let depot_ref = out.accounts.first().map(|a| a.external_ref.clone());
        if let Some(depot_ref) = depot_ref.filter(|r| due.contains(r)) {
            // Optional: a failed document list never fails the sync.
            if let Ok(documents) = self.documents(&depot_ref, &today, &mut known).await {
                out.documents = documents;
                out.statements_fetched = due.to_vec();
                out.state = known.to_state();
            }
        }
        Ok(out)
    }

    async fn finish(&mut self, _ctx: &ConnectContext) {
        if self.client.is_some() {
            let url = self.url("/action/logout", &[]);
            if let Some(client) = self.client.take() {
                let _ = client.get(url).send().await;
            }
        }
        self.session.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Invented pages in the shape of the FFB customer area; no real data.
    const LOGIN: &str = r#"<html><body>
      <form method="post" action='/login;jsessionid=AbC-1!42' name="loginform" class="loginForm">
        <input type="hidden" name="CSRF_TOKEN" value="tok-123"/>
        <table><tr><td><input name="login" id="login" class="login" maxlength="50" type="text"></td></tr>
        <tr><td><input name="pin" id="pin" class="login" type="password" autocomplete="off"></td></tr>
        <tr><td><input class="valid" type="submit" value="Login"/></td></tr></table>
      </form>
      <span class="loginStatus" data-status="1">Wartungsarbeiten</span></body></html>"#;

    const OVERVIEW: &str = r#"<div class='fIndentHor tabellenkopf'>
      Depot&uuml;bersicht Nr. 1234567890 </div>
      <div style="display:table-cell"><b>Gesamtbestand im Depot:&nbsp;</b></div>
      <div style="display:table-cell"><b>12.345,67 EUR</b></div>"#;

    #[allow(clippy::too_many_arguments)]
    fn holding_row(
        name: &str,
        wkn: &str,
        isin: &str,
        art: &str,
        sub: &str,
        qty: &str,
        price: &str,
        value: &str,
        native: &str,
    ) -> String {
        let link = format!(
            "/api/kundenansicht/transaktion/position;jsessionid=X?fonwknext={wkn}&depktoart={art}&depunterkto={sub}"
        );
        format!(
            r#"<tr><td class="fLeft"><div><a href="javascript:openCPFenster('/action/fundfactsheet?wkn={wkn}')" title="{name}"> <b>{short}</b> </a></div>
              <div> {wkn} / {isin} </div><div><a class="nonline" href="javascript:void(0)"> Beispiel KVG </a></div></td>
              <td class="fRight"><a href="{link}"> {qty} </a></td>
              <td class="fRight"><a href="{link}"> {price} </a></td>
              <td class="fRight"> 10.03.2026 </td>
              <td class="fRight"><a href="{link}"><div>{value}</div><div class="cTextGrau">{native}</div></a></td>
              <td class="fRight"><a href="/action/enterWarenkorbKauf?wkn={wkn}"><img alt="Kaufen"/></a></td></tr>"#,
            short = if name.len() > 20 {
                format!("{}...", &name[..20])
            } else {
                name.to_string()
            },
        )
    }

    fn holdings_page(rows: &[String], sum: &str) -> String {
        format!(
            r#"<div id="fundholdingsSection"><table cellspacing="0" id="view" class="liste">
              <thead><tr><th>Fondsname<br/>WKN / ISIN</th><th>Bestand</th><th>Preis</th><th>Preisdatum</th><th>Gesamtwert</th><th>&nbsp;</th></tr></thead>
              <tfoot><tr class="summaryLine"><td class="summaryHeadline" colspan="4">Summe</td><td>{sum}</td></tr></tfoot>
              <tbody>{}</tbody></table></div>"#,
            rows.join("\n")
        )
    }

    fn tx_row(attrs: &str, detail: Option<&str>, cells: [&str; 7]) -> String {
        let link = detail
            .map(|d| {
                format!(r#"<a href="javascript:showDetailsModal({d})"><img alt="Details"/></a>"#)
            })
            .unwrap_or_default();
        format!(
            "<tr {attrs}><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{link}</td></tr>",
            cells[0], cells[1], cells[2], cells[3], cells[4], cells[5], cells[6]
        )
    }

    const FUND_A: &str =
        "Beispiel Welt Aktien ... <div>A0TEST / IE00TEST0001</div><div>Beispiel KVG</div>";
    const FUND_B: &str =
        "Beispiel Euro Anleihen <div>B0TEST / DE00TEST0002</div><div>Beispiel KVG</div>";

    fn transactions_page(rows: &[String], next: Option<u32>) -> String {
        let pager = next
            .map(|p| {
                format!(
                    r#"Seite: <a href="/api/kundenansicht/transaktion/query/;jsessionid=X?transactionType=&amp;d-49751-p={p}&amp;datumVon=01.01.2026">{p}</a>
                    <a href="/api/kundenansicht/transaktion/query/;jsessionid=X?transactionType=&amp;d-49751-p={p}&amp;datumVon=01.01.2026">N&auml;chste</a>"#
                )
            })
            .unwrap_or_default();
        format!(
            r#"<table id="fundsTable"></table>
            Ergebnis Ihrer Suche: {} Aufträge {pager}
            <table id="view" class="liste fNoBorderLines"><thead><tr><th>Buchungs-datum</th><th>Transaktion</th><th>Fonds</th>
            <th>Preisdatum<br>Preis</th><th>Anteile</th><th>Abrechnungs-betrag</th><th>Erwerbs-kosten</th><th></th></tr></thead>
            <tbody>{}</tbody></table>"#,
            rows.len(),
            rows.join("\n")
        )
    }

    fn sample_transactions() -> Vec<String> {
        vec![
            tx_row(
                r#"class="parentRow" id="9000000001""#,
                Some("2026900000000101"),
                [
                    "05.03.2026",
                    "Kauf aus Sparplan",
                    FUND_A,
                    "04.03.2026 55,5500 USD",
                    "1,875",
                    "120,00 EUR 133,33 USD",
                    "0,00 % 100 %",
                ],
            ),
            tx_row(
                r#"class="grauezeile parentRow" id="9000000002""#,
                None,
                ["17.02.2026", "Erträgnis", FUND_A, "", "", "31,17 EUR", ""],
            ),
            tx_row(
                r#"name="9000000002""#,
                Some("2026900000000200"),
                [
                    "17.02.2026",
                    "Ertrag",
                    FUND_A,
                    "17.02.2026 0,0000 USD",
                    "0,000",
                    "31,17 EUR 36,12 USD",
                    "",
                ],
            ),
            tx_row(
                r#"name="9000000002""#,
                Some("2026900000000201"),
                [
                    "17.02.2026",
                    "Ertrag Wiederanlage",
                    FUND_A,
                    "17.02.2026 88,2000 USD",
                    "0,512",
                    "27,44 EUR 37,39 USD",
                    "0,00 % 100 %",
                ],
            ),
            tx_row(
                r#"class="parentRow" id="9000000003""#,
                Some("2026900000000301"),
                [
                    "08.10.2026",
                    "Entgeltbelastung",
                    FUND_B,
                    "01.03.2026 77,3300 EUR",
                    "0,641",
                    "19,84 EUR",
                    "",
                ],
            ),
            tx_row(
                r#"class="parentRow" id="9000000004""#,
                Some("2026900000000401"),
                [
                    "21.01.2026",
                    "Vorabpauschale Abrechnung Lastschrift",
                    FUND_B,
                    "21.01.2026 0,0000 EUR",
                    "0,000",
                    "0,00 EUR",
                    "",
                ],
            ),
            tx_row(
                r#"class="parentRow" id="9000000005""#,
                None,
                ["12.05.2020", "Entgelt", "", "", "", "3,55 EUR", ""],
            ),
            tx_row(
                r#"name="9000000005""#,
                Some("2020900000000501"),
                [
                    "12.05.2020",
                    "Entgeltbelastung (Storniert)",
                    FUND_B,
                    "11.05.2020 199,1234 EUR",
                    "0,042",
                    "3,55 EUR",
                    "",
                ],
            ),
            tx_row(
                r#"name="9000000005""#,
                Some("2020900000000502"),
                [
                    "11.12.2020",
                    "Storno Verkauf",
                    FUND_B,
                    "11.05.2020 199,1234 EUR",
                    "0,042",
                    "3,55 EUR",
                    "",
                ],
            ),
            tx_row(
                r#"class="parentRow" id="9000000006""#,
                None,
                ["06.01.2026", "Übertragung", "", "", "", "", ""],
            ),
            tx_row(
                r#"name="9000000006""#,
                Some("2026900000000601"),
                [
                    "06.01.2026",
                    "Übertrag intern Abgang",
                    FUND_B,
                    "06.01.2026 0,0000 EUR",
                    "19,377",
                    "2.468,02 EUR",
                    "",
                ],
            ),
            tx_row(
                r#"name="9000000006""#,
                Some("2026900000000602"),
                [
                    "06.01.2026",
                    "Übertrag intern Zugang unentgeltlich",
                    FUND_B,
                    "06.01.2026 0,0000 EUR",
                    "19,377",
                    "2.468,02 EUR",
                    "",
                ],
            ),
            tx_row(
                r#"class="parentRow" id="9000000007""#,
                Some("2013900000000701"),
                [
                    "27.12.2013",
                    "Steuerforderung",
                    "",
                    "27.12.2013 0,0000",
                    "0,000",
                    "-0,01 EUR",
                    "",
                ],
            ),
        ]
    }

    #[test]
    fn login_form_keeps_csrf_token_and_resolves_action() {
        let page = url::Url::parse("https://www.ffb.de/login/login.jsp").unwrap();
        let f = login_form(LOGIN, &page).unwrap();
        assert_eq!(
            f.action.as_str(),
            "https://www.ffb.de/login;jsessionid=AbC-1!42"
        );
        assert_eq!(
            f.filled("12345678", "geheim"),
            vec![
                ("CSRF_TOKEN".to_string(), "tok-123".to_string()),
                ("login".into(), "12345678".into()),
                ("pin".into(), "geheim".into()),
            ]
        );
        assert!(is_login_page(LOGIN));
        assert!(!is_login_page(OVERVIEW));
    }

    #[test]
    fn login_form_never_posts_elsewhere_or_via_get() {
        let page = url::Url::parse("https://www.ffb.de/login/login.jsp").unwrap();
        let foreign = LOGIN.replace("action='/login;", "action='https://evil.example/login;");
        assert!(login_form(&foreign, &page).is_err());
        let plain = LOGIN.replace("action='/login;", "action='http://www.ffb.de/login;");
        assert!(login_form(&plain, &page).is_err());
        let get = LOGIN.replace(r#"method="post""#, r#"method="get""#);
        assert!(login_form(&get, &page).is_err());
        let no_method = LOGIN.replace(r#"method="post" "#, "");
        assert!(login_form(&no_method, &page).is_err());
        assert!(login_form("<html>Störung</html>", &page).is_err());
    }

    #[test]
    fn session_id_comes_from_url_or_page() {
        assert_eq!(
            session_id("https://www.ffb.de:/diverse/indexKunde.jsp;jsessionid=2uL-pu_X!1721")
                .as_deref(),
            Some("2uL-pu_X!1721")
        );
        assert_eq!(
            session_id(r#"<frame src="../action/userEntranceInit;jsessionid=Zz9?x=1">"#).as_deref(),
            Some("Zz9")
        );
        assert_eq!(session_id("https://www.ffb.de/"), None);
    }

    #[test]
    fn reads_depot_number_and_total() {
        assert_eq!(
            parse_overview(OVERVIEW).unwrap(),
            ("1234567890".to_string(), Some(1_234_567))
        );
        assert!(parse_overview("<p>leer</p>").is_err());
    }

    #[test]
    fn reads_positions_with_full_names_and_foreign_currency() {
        let page = holdings_page(
            &[
                holding_row(
                    "Beispiel Deutschland Aktien",
                    "A0TES1",
                    "DE000TEST001",
                    "DE",
                    "0",
                    "120,500",
                    "150,2500 EUR",
                    "18.105,13 EUR",
                    "",
                ),
                holding_row(
                    "Beispiel Welt Aktien UCITS ETF (Dist)",
                    "A0TEST",
                    "IE00TEST0001",
                    "DE",
                    "0",
                    "250,125",
                    "98,7654 USD",
                    "21.111,22 EUR",
                    "24.444,55 USD",
                ),
            ],
            "39.216,35 EUR",
        );
        let h = parse_holdings(&page, false).unwrap();
        assert_eq!(h.len(), 2, "header and sum row are skipped");
        let (key, a) = &h[0];
        assert_eq!(key, "DE-0-A0TES1");
        assert_eq!(
            (a.wkn.as_str(), a.isin.as_str()),
            ("A0TES1", "DE000TEST001")
        );
        assert_eq!(a.name, "Beispiel Deutschland Aktien");
        assert!((a.quantity - 120.5).abs() < 1e-9);
        assert_eq!((a.current_price_cents, a.value_cents), (15_025, 1_810_513));
        assert_eq!(a.currency, "");
        assert_eq!(a.native_value_cents, None);
        let (_, b) = &h[1];
        assert_eq!(
            b.name, "Beispiel Welt Aktien UCITS ETF (Dist)",
            "full name from the title, not the truncated text"
        );
        assert_eq!(b.value_cents, 2_111_122);
        assert_eq!(
            b.current_price_cents,
            (2_111_122.0f64 / 250.125).round() as i64,
            "EUR price from the EUR value"
        );
        assert_eq!(b.currency, "USD");
        assert_eq!(b.native_price_cents, Some(9_877));
        assert_eq!(b.native_value_cents, Some(2_444_455));
    }

    #[test]
    fn savings_contract_positions_are_marked_and_empty_fragments_are_empty() {
        let page = holdings_page(
            &[holding_row(
                "Beispiel Deutschland Aktien",
                "A0TES1",
                "DE000TEST001",
                "VL",
                "4",
                "3,333",
                "150,2500 EUR",
                "500,79 EUR",
                "",
            )],
            "500,79 EUR",
        );
        let h = parse_holdings(&page, true).unwrap();
        assert_eq!(h[0].0, "VL-4-A0TES1");
        assert_eq!(h[0].1.name, "Beispiel Deutschland Aktien (VL)");
        assert!(
            parse_holdings("<div>Keine Bestände</div>", false)
                .unwrap()
                .is_empty()
        );
        let broken = page.replace("3,333", "n/a");
        assert!(parse_holdings(&broken, true).is_err());
    }

    #[test]
    fn reads_invested_capital_per_position() {
        let row = |id: &str, capital: &str| {
            format!(
                r#"<div class='detailRow'><div class="selectionRow" id="x">
                <div class="cell betragColSubRow" style="width:125px;">01.01.2009<br>17.11.2026</div>
                <div class="cell betragColSubRow" style="width:140px;" > {capital} &euro; <br> 99,00 &euro;<br> + 1,00 &euro; </div>
                <img onclick="javascript:showBestandsVerlaufSinglePosition( '1234567890-{id}', 'W', 'I', 'N' );"/></div></div>"#
            )
        };
        let html = format!(
            "{}{}",
            row("DE-0-A0TES1", "11.111,11"),
            row("VL-4-A0TES1", "747,90")
        );
        let m = parse_invested(&html);
        assert_eq!(m.get("DE-0-A0TES1"), Some(&1_111_111));
        assert_eq!(m.get("VL-4-A0TES1"), Some(&74_790));
    }

    #[test]
    fn maps_transaction_types_to_directions() {
        use SecurityDirection::*;
        for (kind, want) in [
            ("Kauf", Some(Buy)),
            ("Kauf aus Sparplan", Some(Buy)),
            ("Kauf aus VL-Sparplan", Some(Buy)),
            ("Ertrag Wiederanlage", Some(Buy)),
            ("Ertrag", Some(Dividend)),
            ("Erträgnis", Some(Dividend)),
            ("Ertrag Auszahlung", Some(Dividend)),
            ("Verkauf", Some(Sell)),
            ("Entgeltbelastung", Some(Fee)),
            ("Entgelt", Some(Fee)),
            ("Vorabpauschale Abrechnung Lastschrift", Some(Tax)),
            ("Steuerforderung", Some(Tax)),
            ("Einlieferung", Some(Other)),
            ("Übertrag intern Abgang", Some(Other)),
            ("Delta-Korrektur Abgang", Some(Other)),
            ("Entgeltbelastung (Storniert)", None),
            ("Storno Verkauf", None),
        ] {
            assert_eq!(direction_of(kind), want, "{kind}");
        }
    }

    #[test]
    fn books_child_rows_instead_of_order_summaries_and_skips_cancellations() {
        let names = HashMap::from([(
            "IE00TEST0001".to_string(),
            "Beispiel Welt Aktien UCITS ETF".to_string(),
        )]);
        let (tx, next) = parse_transactions(
            &transactions_page(&sample_transactions(), None),
            "ffb:depot:1",
            "2000-01-01",
            &names,
        )
        .unwrap();
        assert_eq!(next, None);
        let kinds: Vec<&str> = tx
            .iter()
            .map(|t| t.transaction.booking_text.as_str())
            .collect();
        assert_eq!(
            kinds,
            vec![
                "Kauf aus Sparplan (USD)",
                "Ertrag (USD)",
                "Ertrag Wiederanlage (USD)",
                "Entgeltbelastung",
                "Übertrag intern Abgang",
                "Übertrag intern Zugang unentgeltlich",
                "Steuerforderung",
            ],
            "summaries (Erträgnis, Übertragung, Entgelt), cancelled pairs and zero tax rows are left out"
        );
        let buy = &tx[0];
        assert_eq!(buy.external_id, "ffb:2026900000000101");
        assert_eq!(buy.account_ref, "ffb:depot:1");
        let t = &buy.transaction;
        assert_eq!(
            (t.booking_date.as_str(), t.settlement_date.as_str()),
            ("2026-03-05", "2026-03-04")
        );
        assert_eq!(
            (t.wkn.as_str(), t.isin.as_str()),
            ("A0TEST", "IE00TEST0001")
        );
        assert_eq!(
            t.name, "Beispiel Welt Aktien UCITS ETF",
            "full name from the holdings"
        );
        assert_eq!(t.direction, SecurityDirection::Buy);
        assert!((t.quantity - 1.875).abs() < 1e-9);
        assert_eq!((t.value_cents, t.currency.as_str()), (12_000, "EUR"));
        assert_eq!(t.price_cents, (12_000.0f64 / 1.875).round() as i64);
        assert!((t.fx_rate.unwrap() - 133.33 / 120.0).abs() < 1e-9);

        let dividend = &tx[1].transaction;
        assert_eq!(
            (dividend.direction, dividend.value_cents, dividend.quantity),
            (SecurityDirection::Dividend, 3_117, 0.0)
        );
        assert_eq!(tx[1].external_id, "ffb:2026900000000200");
        let fee = &tx[3].transaction;
        assert_eq!(
            (fee.direction, fee.value_cents),
            (SecurityDirection::Fee, 1_984)
        );
        assert_eq!(
            fee.name, "Beispiel Euro Anleihen",
            "name without WKN/ISIN when the fund is not held"
        );
        let tax = &tx[6].transaction;
        assert_eq!(
            (tax.direction, tax.value_cents, tax.isin.as_str()),
            (SecurityDirection::Tax, 1, "")
        );
    }

    #[test]
    fn ids_are_stable_and_since_filters() {
        let page = transactions_page(&sample_transactions(), None);
        let none = HashMap::new();
        let (all, _) = parse_transactions(&page, "d", "2000-01-01", &none).unwrap();
        let (again, _) = parse_transactions(&page, "d", "2000-01-01", &none).unwrap();
        let ids = |v: &[RemoteSecurityTransaction]| {
            v.iter().map(|t| t.external_id.clone()).collect::<Vec<_>>()
        };
        assert_eq!(ids(&all), ids(&again));
        let unique: HashSet<_> = ids(&all).into_iter().collect();
        assert_eq!(unique.len(), all.len());
        let (recent, _) = parse_transactions(&page, "d", "2026-02-20", &none).unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].external_id, all[0].external_id);
    }

    #[test]
    fn rows_without_detail_link_get_order_based_ids() {
        let rows = vec![tx_row(
            r#"class="parentRow" id="77""#,
            None,
            [
                "01.02.2026",
                "Kauf",
                FUND_B,
                "01.02.2026 10,0000 EUR",
                "1,000",
                "10,00 EUR",
                "",
            ],
        )];
        let (tx, _) = parse_transactions(
            &transactions_page(&rows, None),
            "d",
            "2000-01-01",
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(tx[0].external_id, "ffb:o77:0");
    }

    #[test]
    fn follows_the_next_page_link() {
        let (_, next) = parse_transactions(
            &transactions_page(&sample_transactions(), Some(2)),
            "d",
            "2000-01-01",
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(
            next.as_deref(),
            Some(
                "/api/kundenansicht/transaktion/query/;jsessionid=X?transactionType=&d-49751-p=2&datumVon=01.01.2026"
            )
        );
        assert!(
            parse_transactions(
                "<p>Es wurden keine Transaktionen gefunden</p>",
                "d",
                "2000-01-01",
                &HashMap::new()
            )
            .unwrap()
            .0
            .is_empty()
        );
    }

    const POSTFACH: &str = r#"<div class='tabellenkopf'>Ergebnis Ihrer Suche: 3 Dokumente<br/>
      <a href="/api/kundenansicht/postfach/query/;jsessionid=X?applicationCategory=UNDEFINED&amp;d-49751-p=2&amp;datumVon=01.01.2000" class="pagingbanner">N&auml;chste&nbsp;<img src="/images/pfeil_re_w.gif" /></a></div>
      <table cellspacing="1" id="view" class="liste"><thead><tr><th>Gelesen</th><th>Datum</th><th>Kategorie</th><th>Dokumenttyp</th><th>WKN</th><th></th><th>Bestellung</th></tr></thead><tbody>
      <tr><td><div class="icon-haken" /></td><td><span class="">06.02.2026</span></td><td><span class="">Depot</span></td><td><span class="">Quartalsdepotauszug</span></td><td><span title=""></span></td>
        <td><a href="/api/kundenansicht/postfach/document/900000001;jsessionid=X" target="_top"><img src="/images/buttons/icon_pdf.gif" /></a></td><td><input type="checkbox" onchange="javascript:postageOrderSelection( this, '900000001' );" /></td></tr>
      <tr class="grauezeile"><td></td><td><span class="unread">05.02.2026</span></td><td><span>Steuerunterlagen</span></td><td><span>Steuerbescheinigung PA (Steuerinland)</span></td><td></td>
        <td><a href="/api/kundenansicht/postfach/document/900000002;jsessionid=X"><img /></a></td><td></td></tr>
      <tr><td></td><td><span>04.02.2026</span></td><td><span>Abwicklungskonto</span></td><td><span>Konto Auszuege</span></td><td></td>
        <td><a href="/api/kundenansicht/postfach/document/900000003;jsessionid=X"><img /></a></td><td></td></tr>
      <tr><td></td><td><span>03.02.2026</span></td><td><span>Depot</span></td><td><span>Fondsabrechnung</span></td><td><span title="A0TEST">A0TEST</span></td>
        <td><a href="/api/kundenansicht/postfach/document/900000004;jsessionid=X"><img /></a></td><td></td></tr>
      </tbody></table>"#;

    #[test]
    fn reads_postfach_documents_and_the_next_page() {
        let rows = parse_postfach(POSTFACH);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].id, "900000001");
        assert_eq!(rows[0].date, "2026-02-06");
        let types: Vec<_> = rows.iter().map(document_type).collect();
        assert_eq!(types, ["kontoauszug", "steuer", "kontoauszug", "postbox"]);
        assert_eq!(
            document_file_name(&rows[1]),
            "2026-02-05_Steuerbescheinigung_PA__Steuerinland__900000002.pdf"
        );
        let next = next_page(POSTFACH).unwrap();
        assert!(next.starts_with("/api/kundenansicht/postfach/query/;jsessionid=X?"));
        assert!(next.contains("&d-49751-p=2&"), "{next}");
        assert!(parse_postfach("<p>Es wurden keine Dokumente gefunden</p>").is_empty());
    }

    #[test]
    fn numbers_and_dates() {
        assert_eq!(parse_decimal("1.234,567"), Some(1234.567));
        assert_eq!(parse_decimal(" 0,042 "), Some(0.042));
        assert_eq!(parse_decimal(""), None);
        assert_eq!(
            money("120,00 EUR 133,33 USD"),
            vec![(12_000, "EUR".into()), (13_333, "USD".into())]
        );
        assert_eq!(money("04.03.2026 55,5500 USD"), vec![(5_555, "USD".into())]);
        assert_eq!(money("-0,01 EUR"), vec![(-1, "EUR".into())]);
        assert_eq!(money("11.111,11 €"), vec![(1_111_111, "EUR".into())]);
    }

    #[test]
    fn urls_carry_the_session_id_before_the_query() {
        let c = FfbConnector {
            client: None,
            session: "S1".into(),
        };
        assert_eq!(
            c.url(
                "/api/kundenansicht/transaktion/query/",
                &[("datumVon", "01.01.2026"), ("product", "")]
            ),
            "https://www.ffb.de/api/kundenansicht/transaktion/query/;jsessionid=S1?datumVon=01.01.2026&product="
        );
    }
}
