//! Ford Money (Ford Bank GmbH, ebanking.fordmoney.de): balances and bookings
//! of the savings accounts (Tagesgeld/Festgeld) and the PDF documents of the
//! Postbox. There is no API: the connector drives the JavaServer Faces pages
//! of the online banking (unofficial, the pages may change at any time).
//! Page shapes checked live 2026-11-17.
//!
//! Every action is a POST of a page form with its hidden fields
//! (`javax.faces.ViewState` …) plus the name of the link that was "clicked"
//! (`mojarra.jsfcljs(form, {'<name>':'<name>'})`). Those names are generated
//! (`contentForm:j_idt331:0:j_idt340`), so they are read from each page.
//!
//!  GET  …/pages/login/login.jsf          -> `contentForm` (Benutzerkennung, password)
//!  POST contentForm + submitLogin         -> login_2fa.jsf (approval in the Security-App)
//!                                            or the login page again = wrong credentials
//!  POST login_2fa.jsf (JSF ajax, click on `…:btnConfirm2FA`)
//!                                         -> the bank holds the answer, for some customers for
//!                                            minutes; `btnConfirmed1` = approved. It runs in the
//!                                            background and is never sent twice at the same time
//!  POST login_2fa.jsf + `…:btnConfirmed1` -> account-overview_08.jsf
//!  GET  …/pages/konten/account-overview_08.jsf
//!                                         -> one `section.accordion` per account (IBAN, balance)
//!  POST overview + "Umsätze anzeigen"     -> transactions-10.jsf
//!  POST contentForm + dates + btnLoad     -> list for the date range (first 5 rows)
//!  POST contentForm (JSF ajax, `contentForm:limit` = 1000)
//!                                         -> partial response with the whole list
//!  POST navForm + "subnav.Neu" / "subnav.Gelesen"
//!                                         -> Postbox lists; POST contentForm + row link = PDF.
//!                                            Downloading an unread document marks it read.
//!  POST headerForm + loginBeanLogout      -> best effort
//!
//! The bank delivers no booking ids, so bookings get content ids. Documents
//! have no ids either: they are remembered by date, subject and product.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use async_trait::async_trait;
use regex::Regex;
use tokio::task::JoinHandle;
use zeroize::Zeroize;

use super::html::{
    Form, INPUT, attr, attrs, de_date, form_by_id, parse_amount, parse_date, partial_updates, text,
};
use super::{
    BankConnector, Challenge, ChallengeKind, ChallengeResponse, ConnectContext, ContentIds,
    Documents, FetchResult, KnownDocuments, Phase, RemoteAccount, RemoteTransaction,
};
use crate::documents::RemoteDocument;
use crate::error::{CoreError, CoreResult};
use crate::i18n::tr;
use crate::models::AccountKind;

const HOST: &str = "ebanking.fordmoney.de";
const BASE: &str = "https://ebanking.fordmoney.de";
const LOGIN: &str = "/eBankingClient/pages/login/login.jsf";
const OVERVIEW: &str = "/eBankingClient/pages/konten/account-overview_08.jsf";
/// Account external refs: `fordmoney:<IBAN>`.
pub(crate) const REF_PREFIX: &str = "fordmoney:";
/// How long the bank may hold the approval request. A shorter limit would drop
/// a request the bank is still answering and lose the approval with it.
const APPROVAL_POLL_TIMEOUT: Duration = Duration::from_secs(300);
/// Rows requested when the transaction list is expanded.
const LIST_LIMIT: u32 = 1000;
/// Start of the date range for the read Postbox documents.
const ARCHIVE_START: &str = "01.01.2010";

static A_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<a\b([^>]*)>").unwrap());
static SPAN_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<span\b([^>]*)>").unwrap());
/// First parameter of `mojarra.jsfcljs(form, {'<name>':'<name>'}, …)` (quotes
/// may be escaped inside `jsf.util.chain`).
static JSF_PARAM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{\\?'([^'\\]+)\\?'\s*:").unwrap());
static ACCOUNT_NO: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"collapse_(\d{5,})").unwrap());
static PRODUCT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?s)col-sm-10 col-xs-8">\s*<div>([^<]*)</div>"#).unwrap());
static LABEL_VALUE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)eb-label">([^<]*)</div>\s*<div[^>]*eb-text">([^<]*)</div>"#).unwrap()
});
static RIGHT_SUM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"content-header__right-sum">([^<]*)<"#).unwrap());
static LI_ROW: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?is)<li class="row">(.*?)</li>"#).unwrap());
static KIND: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?s)blue-bold">([^<]*)<"#).unwrap());
static SUBTEXT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?s)transactions-overview-subtext">(.*?)</div>"#).unwrap());
static PURPOSE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)<div class="col-xs-12 col-sm-3">(.*?)</div>\s*<div class="col-xs-12 col-sm-3 transaction-amount"#)
        .unwrap()
});
static AMOUNT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?s)transaction-amount hidden-xs">(.*?)</div>"#).unwrap());
static SUBJECT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?s)col-sm-5 blue hidden-xs">([^<]*)<"#).unwrap());
static PRODUCT_COL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?s)<div class="col-sm-4[^"]*">(.*?)</div>"#).unwrap());
static ROW_LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(contentForm:[A-Za-z0-9_]+:\d+:[A-Za-z0-9_]+)").unwrap());
static DIGITS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(\d{6,})\b").unwrap());

fn page_changed() -> CoreError {
    CoreError::protocol(tr(
        "Ford Money: Seite nicht erkannt (Online-Banking geändert?).",
    ))
}

fn invalid() -> CoreError {
    CoreError::invalid(tr(
        "Ford Money: Unvollständige oder ungültige Antwort. Es wurden keine Daten übernommen.",
    ))
}

fn wrong_credentials() -> CoreError {
    CoreError::Authentication {
        message: tr(
            "Ford Money: Anmeldung fehlgeschlagen. Bitte Kundennummer und Passwort prüfen.",
        )
        .into(),
    }
}

fn session_ended() -> CoreError {
    CoreError::Authentication {
        message: tr("Ford Money: Die Sitzung wurde beendet. Bitte den Abruf erneut starten.")
            .into(),
    }
}

fn not_approved() -> CoreError {
    CoreError::Authentication {
        message: tr("Ford Money: Die Anmeldung wurde in der Security-App nicht bestätigt.").into(),
    }
}

/// Only `https://ebanking.fordmoney.de` may receive the password or cookies.
fn ensure_host(url: &url::Url) -> CoreResult<()> {
    if url.scheme() == "https" && url.host_str() == Some(HOST) {
        return Ok(());
    }
    Err(CoreError::protocol(tr(
        "Ford Money: Unerwartete Weiterleitung zu einer fremden Adresse – abgebrochen.",
    )))
}

fn push_challenge() -> Challenge {
    Challenge {
        kind: ChallengeKind::DecoupledPush,
        title: "Ford Money Security-App".into(),
        message: tr("Bitte die Anmeldung in der Ford-Money-Security-App bestätigen.").into(),
        method_name: "Security-App".into(),
        redirect_url: None,
        image: None,
        expires_in_seconds: None,
        poll_interval_seconds: 2,
    }
}

// ------------------------------------------------------------------ page helpers

/// Link parameter of the first `<a>` whose `data-wt` matches.
fn link_param(html: &str, data_wt: &str) -> Option<String> {
    A_TAG.captures_iter(html).find_map(|c| {
        let a = attrs(&c[1]);
        if !attr(&a, "data-wt").is_some_and(|w| w.eq_ignore_ascii_case(data_wt)) {
            return None;
        }
        JSF_PARAM
            .captures(attr(&a, "onclick")?)
            .map(|m| m[1].to_string())
    })
}

fn is_login_page(html: &str) -> bool {
    form_by_id(html, "contentForm").is_some_and(|f| f.get("contentForm:Benutzerkennung").is_some())
        || html.contains("name=\"contentForm:password\"")
}

/// What the approval poll answered.
#[derive(Debug, PartialEq)]
enum Approval {
    Approved,
    Rejected,
    Pending,
}

fn approval_of(partial: &str) -> Approval {
    if partial.contains("btnConfirmed1") {
        Approval::Approved
    } else if partial.contains("btnConfirmed2") || partial.contains("btnConfirmed3") {
        Approval::Rejected
    } else {
        Approval::Pending
    }
}

/// Fields of a JSF ajax request.
fn ajax_fields(form: &Form, source: &str, render: &str, click: bool) -> Vec<(String, String)> {
    let mut fields = form.fields.clone();
    if click {
        fields.push((source.to_string(), String::new()));
        fields.push(("javax.faces.behavior.event".into(), "click".into()));
        fields.push(("javax.faces.partial.event".into(), "click".into()));
    }
    fields.push(("javax.faces.source".into(), source.to_string()));
    fields.push(("javax.faces.partial.execute".into(), source.to_string()));
    fields.push(("javax.faces.partial.render".into(), render.to_string()));
    fields.push(("javax.faces.partial.ajax".into(), "true".into()));
    fields
}

/// The approval request, which the bank answers only when it has a result.
async fn approval_request(
    client: reqwest::Client,
    form: Form,
    poll: String,
    render: String,
) -> CoreResult<String> {
    let target = url::Url::parse(&format!("{BASE}{}", form.action)).map_err(|_| page_changed())?;
    ensure_host(&target)?;
    let resp = client
        .post(target)
        .header("Faces-Request", "partial/ajax")
        .timeout(APPROVAL_POLL_TIMEOUT)
        .form(&ajax_fields(&form, &poll, &render, true))
        .send()
        .await?;
    ensure_host(resp.url())?;
    let status = resp.status();
    let body = super::read_body_limited(resp, super::MAX_DOC_BYTES).await?;
    if !status.is_success() {
        return Err(CoreError::Bank {
            code: status.as_u16().to_string(),
            message: tr("Ford Money: Die Anfrage wurde abgelehnt.").into(),
        });
    }
    let xml = crate::text::decode_bank_bytes(&body);
    if xml.contains("<redirect") && xml.contains("/login/") {
        return Err(session_ended());
    }
    Ok(xml)
}

/// State of the approval request after one poll of the app.
#[derive(Debug)]
enum PollStep {
    Waiting,
    Answer(CoreResult<String>),
}

/// Starts the request if none is open and reports its answer once it is there.
/// The app polls every few seconds, but the bank sees one request at a time.
async fn poll_step<F>(
    open: &mut Option<JoinHandle<CoreResult<String>>>,
    start: impl FnOnce() -> F,
) -> PollStep
where
    F: Future<Output = CoreResult<String>> + Send + 'static,
{
    let running = open.get_or_insert_with(|| tokio::spawn(start()));
    if !running.is_finished() {
        return PollStep::Waiting;
    }
    let Some(done) = open.take() else {
        return PollStep::Waiting;
    };
    PollStep::Answer(done.await.unwrap_or_else(|e| {
        Err(CoreError::Network {
            message: e.to_string(),
        })
    }))
}

/// The approval step of the login: its form and the generated names.
#[derive(Debug, Clone, PartialEq)]
struct TwoFactor {
    form: Form,
    /// Hidden button the page clicks every 10 s (JSF ajax).
    poll: String,
    /// Area the poll re-renders.
    render: String,
    /// Hidden button the page clicks once the approval came in.
    confirmed: String,
}

fn two_factor(html: &str) -> Option<TwoFactor> {
    let form = form_by_id(html, "contentForm")?;
    let names: Vec<String> = INPUT
        .captures_iter(html)
        .filter_map(|c| attr(&attrs(&c[1]), "name").map(str::to_string))
        .collect();
    let poll = names
        .iter()
        .find(|n| n.ends_with(":btnConfirm2FA"))?
        .clone();
    let confirmed = names
        .iter()
        .find(|n| n.ends_with(":btnConfirmed1"))?
        .clone();
    let render = SPAN_TAG
        .captures_iter(html)
        .filter_map(|c| attr(&attrs(&c[1]), "id").map(str::to_string))
        .find(|id| id.ends_with(":statusConfirmed"))?;
    Some(TwoFactor {
        form,
        poll,
        render,
        confirmed,
    })
}

// ------------------------------------------------------------------ accounts and bookings

#[derive(Debug, Clone, PartialEq)]
struct Account {
    number: String,
    iban: String,
    product: String,
    owner: String,
    balance_cents: i64,
    /// Link parameter of "Umsätze anzeigen".
    bookings_link: Option<String>,
}

fn parse_accounts(html: &str) -> CoreResult<Vec<Account>> {
    let mut out = Vec::new();
    for part in html.split("<section class=\"accordion\">").skip(1) {
        let Some(number) = ACCOUNT_NO.captures(part).map(|c| c[1].to_string()) else {
            continue;
        };
        let labels: HashMap<String, String> = LABEL_VALUE
            .captures_iter(part)
            .map(|c| (text(&c[1]), text(&c[2])))
            .collect();
        let iban: String = labels
            .get("IBAN")
            .map(|v| v.chars().filter(|c| !c.is_whitespace()).collect())
            .unwrap_or_default();
        if iban.len() < 15 || !iban.ends_with(&number) {
            return Err(invalid());
        }
        let balance_cents = RIGHT_SUM
            .captures(part)
            .and_then(|c| parse_amount(&c[1]))
            .ok_or_else(invalid)?;
        let bookings_link = A_TAG.captures_iter(part).find_map(|c| {
            let a = attrs(&c[1]);
            let wt = attr(&a, "data-wt")?.to_lowercase();
            if !wt.contains("umsätze") && !wt.contains("umsaetze") {
                return None;
            }
            JSF_PARAM
                .captures(attr(&a, "onclick")?)
                .map(|m| m[1].to_string())
        });
        out.push(Account {
            number,
            iban,
            product: PRODUCT
                .captures(part)
                .map(|c| text(&c[1]))
                .filter(|p| !p.is_empty())
                .unwrap_or_else(|| "Tagesgeld".into()),
            owner: labels.get("Kontoinhaber").cloned().unwrap_or_default(),
            balance_cents,
            bookings_link,
        });
    }
    Ok(out)
}

/// Bookings of an expanded transaction list, newest first as the bank
/// shows them. A row with a date but no readable amount rejects the list.
fn parse_bookings(
    list: &str,
    account_ref: &str,
    since: &str,
    ids: &mut ContentIds,
) -> CoreResult<Vec<RemoteTransaction>> {
    let mut out = Vec::new();
    for row in LI_ROW.captures_iter(list) {
        let row = &row[1];
        let Some(date) = parse_date(&text(row)) else {
            continue;
        };
        let amount_cents = AMOUNT
            .captures(row)
            .and_then(|c| parse_amount(&c[1]))
            .ok_or_else(invalid)?;
        let kind = KIND.captures(row).map(|c| text(&c[1])).unwrap_or_default();
        let purpose = [
            PURPOSE.captures(row).map(|c| text(&c[1])),
            SUBTEXT.captures(row).map(|c| text(&c[1])),
        ]
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
        let mut t = RemoteTransaction {
            account_ref: account_ref.to_string(),
            booking_date: date.clone(),
            value_date: date,
            amount_cents,
            currency: "EUR".into(),
            purpose: if purpose.is_empty() {
                kind.clone()
            } else {
                purpose
            },
            booking_text: kind,
            ..Default::default()
        };
        t.external_id = ids.next(&t);
        if t.booking_date.as_str() >= since {
            out.push(t);
        }
    }
    Ok(out)
}

// ------------------------------------------------------------------ Postbox

#[derive(Debug, Clone, PartialEq)]
struct PostboxRow {
    /// Link parameter that downloads the PDF.
    link: String,
    date: String,
    subject: String,
    /// "Tagesgeld 1000000001", "Info".
    product: String,
    /// Account number from `product`, if any.
    account_number: Option<String>,
}

impl PostboxRow {
    fn key(&self) -> String {
        format!("{}|{}|{}", self.date, self.subject, self.product)
    }
}

fn parse_postbox(html: &str) -> Vec<PostboxRow> {
    let mut out = Vec::new();
    for row in LI_ROW.captures_iter(html) {
        let row = &row[1];
        let Some(link) = ROW_LINK.captures(row).map(|c| c[1].to_string()) else {
            continue;
        };
        let Some(date) = parse_date(&text(row)) else {
            continue;
        };
        let subject = SUBJECT
            .captures(row)
            .map(|c| text(&c[1]))
            .unwrap_or_default();
        let product = PRODUCT_COL
            .captures_iter(row)
            .last()
            .map(|c| text(&c[1]))
            .unwrap_or_default();
        out.push(PostboxRow {
            link,
            date,
            subject,
            account_number: DIGITS.captures(&product).map(|c| c[1].to_string()),
            product,
        });
    }
    out
}

fn document_type(subject: &str) -> &'static str {
    let s = subject.to_lowercase();
    if s.contains("kontoauszug") {
        "kontoauszug"
    } else if s.contains("steuer") {
        "steuer"
    } else {
        "postbox"
    }
}

/// `YYYY-MM-DD_<subject>.pdf`, without path separators.
fn document_file_name(row: &PostboxRow) -> String {
    let subject: String = row
        .subject
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '.' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{}_{}.pdf", row.date, subject.trim().replace(' ', "_"))
}

// ------------------------------------------------------------------ connector

/// One response: final URL, content type and body.
struct Page {
    url: url::Url,
    content_type: String,
    body: Vec<u8>,
}

impl Page {
    fn html(&self) -> String {
        crate::text::decode_bank_bytes(&self.body)
    }
}

#[derive(Default)]
pub struct FordMoneyConnector {
    client: Option<reqwest::Client>,
    two_factor: Option<TwoFactor>,
    /// The approval request the bank has not answered yet.
    approval_poll: Option<JoinHandle<CoreResult<String>>>,
    /// ViewState of the last page, for the logout.
    last_view_state: Option<String>,
}

fn client() -> CoreResult<reqwest::Client> {
    // The login page reloads itself until the cookie `js=true` is set.
    let jar = Arc::new(reqwest::cookie::Jar::default());
    let base = url::Url::parse(BASE).map_err(|_| page_changed())?;
    jar.add_cookie_str("js=true; Path=/; Secure", &base);
    Ok(reqwest::Client::builder()
        .user_agent(concat!(
            "GiroCrab/",
            env!("CARGO_PKG_VERSION"),
            " (open source; +https://github.com/)"
        ))
        .timeout(std::time::Duration::from_secs(60))
        .cookie_provider(jar)
        .redirect(super::same_host_redirects())
        .build()?)
}

impl FordMoneyConnector {
    pub fn new() -> Self {
        Self::default()
    }

    fn http(&self) -> CoreResult<&reqwest::Client> {
        self.client.as_ref().ok_or(CoreError::SessionClosed)
    }

    async fn read(&mut self, resp: reqwest::Response) -> CoreResult<Page> {
        let url = resp.url().clone();
        ensure_host(&url)?;
        let status = resp.status();
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        let body = super::read_body_limited(resp, super::MAX_DOC_BYTES).await?;
        if !status.is_success() {
            return Err(CoreError::Bank {
                code: status.as_u16().to_string(),
                message: tr("Ford Money: Die Anfrage wurde abgelehnt.").into(),
            });
        }
        let page = Page {
            url,
            content_type,
            body,
        };
        if page.content_type.contains("html") {
            let html = page.html();
            if let Some(vs) = form_by_id(&html, "contentForm")
                .and_then(|f| f.get("javax.faces.ViewState").map(str::to_string))
            {
                self.last_view_state = Some(vs);
            }
        }
        Ok(page)
    }

    /// GET a page of the logged-in area; the login page instead = session ended.
    async fn get(&mut self, path: &str) -> CoreResult<String> {
        let resp = self.http()?.get(format!("{BASE}{path}")).send().await?;
        let page = self.read(resp).await?;
        let html = page.html();
        if page.url.path().contains("/login/") || is_login_page(&html) {
            return Err(session_ended());
        }
        Ok(html)
    }

    /// POST a form with the given fields.
    async fn post(&mut self, action: &str, fields: &[(String, String)]) -> CoreResult<Page> {
        let target = url::Url::parse(&format!("{BASE}{action}")).map_err(|_| page_changed())?;
        ensure_host(&target)?;
        let resp = self.http()?.post(target).form(fields).send().await?;
        let page = self.read(resp).await?;
        if page.url.path().contains("/login/login.jsf") {
            return Err(session_ended());
        }
        Ok(page)
    }

    /// POST a form and expect a page of the logged-in area.
    async fn post_page(&mut self, action: &str, fields: &[(String, String)]) -> CoreResult<String> {
        let page = self.post(action, fields).await?;
        let html = page.html();
        if is_login_page(&html) {
            return Err(session_ended());
        }
        Ok(html)
    }

    /// JSF ajax request: the partial-response XML.
    async fn ajax(
        &mut self,
        form: &Form,
        source: &str,
        render: &str,
        click: bool,
    ) -> CoreResult<String> {
        let fields = ajax_fields(form, source, render, click);
        let target =
            url::Url::parse(&format!("{BASE}{}", form.action)).map_err(|_| page_changed())?;
        ensure_host(&target)?;
        let resp = self
            .http()?
            .post(target)
            .header("Faces-Request", "partial/ajax")
            .form(&fields)
            .send()
            .await?;
        let page = self.read(resp).await?;
        let xml = page.html();
        if xml.contains("<redirect") && xml.contains("/login/") {
            return Err(session_ended());
        }
        Ok(xml)
    }

    async fn overview(&mut self) -> CoreResult<String> {
        self.get(OVERVIEW).await
    }

    /// All bookings of one account since `since`.
    async fn bookings(
        &mut self,
        account: &Account,
        account_ref: &str,
        since: &str,
        today: &str,
    ) -> CoreResult<Vec<RemoteTransaction>> {
        let overview = self.overview().await?;
        let link = account.bookings_link.as_deref().ok_or_else(page_changed)?;
        let form = form_by_id(&overview, "contentForm").ok_or_else(page_changed)?;
        let page = self.post_page(&form.action, &form.clicked(link)).await?;

        let mut form = form_by_id(&page, "contentForm").ok_or_else(page_changed)?;
        let start = form
            .field_ending(":inbox-datetimepicker-start")
            .ok_or_else(page_changed)?;
        let end = form
            .field_ending(":inbox-datetimepicker-end")
            .ok_or_else(page_changed)?;
        form.set(&start, &de_date(since).ok_or_else(invalid)?);
        form.set(&end, &de_date(today).ok_or_else(invalid)?);
        let page = self
            .post_page(&form.action, &form.clicked("contentForm:btnLoad"))
            .await?;

        let mut form = form_by_id(&page, "contentForm").ok_or_else(page_changed)?;
        let limit = form.field_ending(":limit").ok_or_else(page_changed)?;
        let mut wanted = LIST_LIMIT;
        let mut list = String::new();
        for _ in 0..4 {
            form.set(&limit, &wanted.to_string());
            let xml = self
                .ajax(
                    &form,
                    &limit,
                    "contentForm:showMore contentForm:limitValue contentForm:transactionList",
                    false,
                )
                .await?;
            let updates = partial_updates(&xml);
            list = updates
                .iter()
                .find(|(id, _)| id.ends_with(":transactionList"))
                .map(|(_, c)| c.clone())
                .ok_or_else(invalid)?;
            let more = updates
                .iter()
                .any(|(id, c)| id.ends_with(":showMore") && c.contains("moreTransactions"));
            if !more {
                break;
            }
            wanted *= 10;
        }
        let mut ids = ContentIds::default();
        parse_bookings(&list, account_ref, since, &mut ids)
    }

    /// Downloads one Postbox row of `page` (whose `contentForm` lists it).
    async fn download(&mut self, page: &str, row: &PostboxRow) -> CoreResult<Option<Vec<u8>>> {
        let form = form_by_id(page, "contentForm").ok_or_else(page_changed)?;
        let resp = self.post(&form.action, &form.clicked(&row.link)).await?;
        if resp.body.starts_with(b"%PDF") {
            Ok(Some(resp.body))
        } else {
            Ok(None)
        }
    }

    /// Opens a Postbox list via the navigation ("subnav.Neu" / "subnav.Gelesen").
    async fn postbox_page(&mut self, from: &str, data_wt: &str) -> CoreResult<String> {
        let link = link_param(from, data_wt).ok_or_else(page_changed)?;
        let nav = form_by_id(from, "navForm").ok_or_else(page_changed)?;
        self.post_page(&nav.action, &nav.clicked(&link)).await
    }

    /// PDF documents of the accounts in `wanted` (account number → external
    /// ref) that are not in `known`: first the unread ones (downloading marks
    /// them read, so the list is reloaded after each), then the read ones.
    async fn documents(
        &mut self,
        wanted: &HashMap<String, String>,
        known: &mut KnownDocuments,
    ) -> CoreResult<Vec<RemoteDocument>> {
        let mut out = Vec::new();
        let mut take = |row: &PostboxRow, bytes: Vec<u8>, known: &mut KnownDocuments| {
            let account_ref = row
                .account_number
                .as_ref()
                .and_then(|n| wanted.get(n))
                .cloned()
                .unwrap_or_default();
            known.insert(row.key());
            out.push(RemoteDocument {
                account_ref,
                booking_external_id: None,
                title: row.subject.clone(),
                doc_type: document_type(&row.subject).into(),
                mime_type: "application/pdf".into(),
                file_name: document_file_name(row),
                bytes,
                source_url: format!("fordmoney:postbox:{}", row.key()),
                date: row.date.clone(),
            });
        };
        let is_wanted = |row: &PostboxRow, known: &KnownDocuments| {
            row.account_number
                .as_ref()
                .is_some_and(|n| wanted.contains_key(n))
                && !known.contains(&row.key())
        };

        let overview = self.overview().await?;
        let mut page = self.postbox_page(&overview, "subnav.Neu").await?;
        let mut failed: Vec<String> = Vec::new();
        for _ in 0..50 {
            let Some(row) = parse_postbox(&page)
                .into_iter()
                .find(|r| is_wanted(r, known) && !failed.contains(&r.key()))
            else {
                break;
            };
            match self.download(&page, &row).await? {
                Some(bytes) => take(&row, bytes, known),
                None => failed.push(row.key()),
            }
            page = self.postbox_page(&page, "subnav.Neu").await?;
        }

        let archive = self.postbox_page(&page, "subnav.Gelesen").await?;
        let mut rows = parse_postbox(&archive);
        let mut list_page = archive.clone();
        // The list starts one year back; ask for the whole range.
        if let (Some(mut form), Some(refresh)) = (
            form_by_id(&archive, "contentForm"),
            link_param(&archive, ".button-anzeigen"),
        ) && let (Some(start), Some(end)) = (
            form.field_ending(":inbox-datetimepicker-start"),
            form.field_ending(":inbox-datetimepicker-end"),
        ) {
            form.set(&start, ARCHIVE_START);
            form.set(&end, &de_date(&super::today()).unwrap_or_default());
            if let Ok(wide) = self.post_page(&form.action, &form.clicked(&refresh)).await {
                let wide_rows = parse_postbox(&wide);
                if wide_rows.len() >= rows.len() {
                    rows = wide_rows;
                    list_page = wide;
                }
            }
        }
        for row in rows {
            if !is_wanted(&row, known) {
                continue;
            }
            if let Some(bytes) = self.download(&list_page, &row).await? {
                take(&row, bytes, known);
            }
        }
        Ok(out)
    }

    fn finish_login(&mut self, html: &str) -> CoreResult<Phase> {
        if is_login_page(html) {
            return Err(wrong_credentials());
        }
        if let Some(tf) = two_factor(html) {
            self.two_factor = Some(tf);
            return Ok(Phase::Challenge(push_challenge()));
        }
        if html.contains("accordion") || html.contains("FINANZSTATUS") {
            return Ok(Phase::Ready);
        }
        Err(page_changed())
    }
}

#[async_trait]
impl BankConnector for FordMoneyConnector {
    async fn begin(&mut self, ctx: &ConnectContext) -> CoreResult<Phase> {
        let login = ctx.login_name.trim();
        if login.is_empty() || ctx.secret.expose().is_empty() {
            return Err(CoreError::Authentication {
                message: tr("Ford Money: Kundennummer und Passwort erforderlich.").into(),
            });
        }
        self.client = Some(client()?);
        let resp = self.http()?.get(format!("{BASE}{LOGIN}")).send().await?;
        let page = self.read(resp).await?;
        let form = form_by_id(&page.html(), "contentForm").ok_or_else(page_changed)?;
        let user = form
            .field_ending(":Benutzerkennung")
            .ok_or_else(page_changed)?;
        let mut fields = form.clicked("contentForm:submitLogin");
        fields.retain(|(n, _)| n != &user);
        fields.push((user, login.to_string()));
        fields.push((
            "contentForm:password".into(),
            ctx.secret.expose().to_string(),
        ));
        let target =
            url::Url::parse(&format!("{BASE}{}", form.action)).map_err(|_| page_changed())?;
        ensure_host(&target)?;
        let sent = self.http()?.post(target).form(&fields).send().await;
        for (_, v) in fields.iter_mut() {
            v.zeroize();
        }
        let page = self.read(sent?).await?;
        let html = page.html();
        self.finish_login(&html)
    }

    async fn respond(&mut self, _ctx: &ConnectContext, _r: ChallengeResponse) -> CoreResult<Phase> {
        let mut tf = self.two_factor.clone().ok_or(CoreError::SessionClosed)?;
        let client = self.http()?.clone();
        let (form, poll, render) = (tf.form.clone(), tf.poll.clone(), tf.render.clone());
        let step = poll_step(&mut self.approval_poll, || {
            approval_request(client, form, poll, render)
        })
        .await;
        let xml = match step {
            PollStep::Waiting => return Ok(Phase::Pending(push_challenge())),
            PollStep::Answer(Ok(xml)) => xml,
            PollStep::Answer(Err(CoreError::Network { .. })) => {
                return Ok(Phase::Pending(push_challenge()));
            }
            PollStep::Answer(Err(e)) => return Err(e),
        };
        if let Some((_, vs)) = partial_updates(&xml)
            .into_iter()
            .find(|(id, _)| id.contains("javax.faces.ViewState"))
        {
            tf.form.set("javax.faces.ViewState", vs.trim());
            self.two_factor = Some(tf.clone());
        }
        match approval_of(&xml) {
            Approval::Pending => Ok(Phase::Pending(push_challenge())),
            Approval::Rejected => {
                self.two_factor = None;
                Err(not_approved())
            }
            Approval::Approved => {
                self.two_factor = None;
                let mut fields = tf.form.fields.clone();
                fields.push((tf.confirmed.clone(), String::new()));
                self.post(&tf.form.action, &fields).await?;
                let html = self.overview().await?;
                match self.finish_login(&html)? {
                    Phase::Ready => Ok(Phase::Ready),
                    _ => Err(page_changed()),
                }
            }
        }
    }

    async fn fetch(&mut self, ctx: &ConnectContext) -> CoreResult<FetchResult> {
        let today = super::today();
        let overview = self.overview().await?;
        let accounts = parse_accounts(&overview)?;
        if accounts.is_empty() {
            return Err(page_changed());
        }
        let refs: HashMap<String, String> = accounts
            .iter()
            .map(|a| (a.number.clone(), format!("{REF_PREFIX}{}", a.iban)))
            .collect();
        let mut known = KnownDocuments::from_state(&ctx.state);

        if let Documents::StatementsOnly(only) = &ctx.documents {
            let wanted: HashMap<String, String> =
                refs.into_iter().filter(|(_, r)| r == only).collect();
            let documents = self.documents(&wanted, &mut known).await?;
            return Ok(FetchResult {
                documents,
                state: known.to_state(),
                ..Default::default()
            });
        }

        let mut out = FetchResult::default();
        for account in &accounts {
            let account_ref = format!("{REF_PREFIX}{}", account.iban);
            let since = ctx.since_for(&account_ref).to_string();
            let bookings = self.bookings(account, &account_ref, &since, &today).await?;
            out.transactions.extend(bookings);
            out.accounts.push(RemoteAccount {
                external_ref: account_ref,
                name: format!("Ford Money {}", account.product),
                owner: account.owner.clone(),
                iban: account.iban.clone(),
                account_number: account.number.clone(),
                currency: "EUR".into(),
                kind: AccountKind::Savings,
                balance_cents: account.balance_cents,
                balance_date: today.clone(),
            });
        }

        let due = ctx.documents.due_statements();
        if !due.is_empty() {
            let wanted: HashMap<String, String> =
                refs.into_iter().filter(|(_, r)| due.contains(r)).collect();
            // Optional: a failed download never fails the sync. On failure the
            // ids of what was downloaded so far are dropped with the documents.
            let mut fetched = known.clone();
            if let Ok(documents) = self.documents(&wanted, &mut fetched).await {
                out.documents = documents;
                out.statements_fetched = due.to_vec();
                known = fetched;
            }
        }
        out.state = known.to_state();
        Ok(out)
    }

    async fn finish(&mut self, _ctx: &ConnectContext) {
        if let Some(open) = self.approval_poll.take() {
            open.abort();
        }
        if let (Some(client), Some(vs)) = (self.client.take(), self.last_view_state.take()) {
            let fields = [
                ("headerForm", "headerForm"),
                ("javax.faces.ViewState", vs.as_str()),
                ("headerForm:loginBeanLogout", "headerForm:loginBeanLogout"),
            ];
            let _ = client
                .post(format!("{BASE}{OVERVIEW}"))
                .form(&fields)
                .send()
                .await;
        }
        self.two_factor = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[tokio::test]
    async fn login_client_does_not_follow_redirects_to_another_origin() {
        let client = client().unwrap();
        crate::connectors::tests::redirects::assert_no_cross_origin_replay(&client).await;
    }

    // Invented pages in the shape of the Ford Money online banking; no real data.
    const LOGIN_PAGE: &str = r#"<html><body>
      <form id="contentForm" name="contentForm" method="post" action="/eBankingClient/pages/login/login.jsf" enctype="application/x-www-form-urlencoded" role="form">
        <input type="hidden" name="contentForm" value="contentForm" />
        <input type="hidden" name="contentForm:j_idt58" value="aaaa-bbbb" />
        <input id="contentForm:Benutzerkennung" type="text" name="contentForm:Benutzerkennung" value="" placeholder="Kundennummer" />
        <input id="contentForm:password" type="password" name="contentForm:password" value="" />
        <a id="contentForm:submitLogin" href="javascript:void(0)" onclick="jsf.util.chain(this,event,'x','mojarra.jsfcljs(document.getElementById(\'contentForm\'),{\'contentForm:submitLogin\':\'contentForm:submitLogin\'},\'\')');return false">Einloggen</a>
        <input type="hidden" name="javax.faces.ViewState" id="j_id1:javax.faces.ViewState:0" value="1:2" autocomplete="off" />
      </form></body></html>"#;

    const TWO_FA_PAGE: &str = r#"<html><body>
      <div class="sa-confirmation">Security-App Bestätigung</div>
      <form id="contentForm" name="contentForm" method="post" action="/eBankingClient/pages/login/login_2fa.jsf">
        <input type="hidden" name="contentForm" value="contentForm" />
        <input type="hidden" name="contentForm:j_idt65" value="aaaa-bbbb" />
        <input class="form-control" id="kundennummer" name="kundennummer" type="text" value="" disabled="disabled" />
        <span id="contentForm:j_idt70:j_idt72:statusConfirmed"><script>var x;</script></span>
        <input id="contentForm:j_idt70:j_idt72:btnConfirmed1" type="submit" name="contentForm:j_idt70:j_idt72:btnConfirmed1" value="" style="display:none" />
        <input id="contentForm:j_idt70:j_idt72:btnConfirmed2" type="submit" name="contentForm:j_idt70:j_idt72:btnConfirmed2" value="" style="display:none" />
        <input id="contentForm:j_idt70:j_idt72:btnConfirm2FA" type="submit" name="contentForm:j_idt70:j_idt72:btnConfirm2FA" value="" />
        <input type="hidden" name="javax.faces.ViewState" id="j_id1:javax.faces.ViewState:0" value="3:4" autocomplete="off" />
      </form></body></html>"#;

    fn account_section(number: &str, iban: &str, sum: &str, link: &str) -> String {
        format!(
            r##"<section class="accordion">
              <a class="row accordion-container__header collapsed" data-bs-toggle="collapse" href="#collapse_{number}">
                <div class="col-sm-10 col-xs-8"><div>Tagesgeld</div><div class="tg-savings-headline-blue"> {iban}</div></div>
                <div class="col-sm-12 col xs-5 text-right"><span class="content-header__right-sum">{sum}</span></div>
              </a>
              <div id="collapse_{number}" class="accordion-container__collapse collapse ">
                <a href="javascript:void(0)" onclick="mojarra.jsfcljs(document.getElementById('contentForm'),{{'{link}':'{link}'}},'');return false" class="btn btn-light-blue" data-wt=".button-umsätze-anzeigen"><span class="btn-value">Umsätze anzeigen</span></a>
                <div class="row eb-row__v-separator-normal">
                  <div class="col-sm-6 col-xs-12 col-xs-push-1 eb-label">Kontoinhaber</div>
                  <div class="col-sm-6 col-xs-12 col-xs-push-1 eb-text">Erika Mustermann</div>
                </div>
                <div class="row eb-row__v-separator-small">
                  <div class="col-sm-6 col-xs-12 col-xs-push-1 eb-label">IBAN</div>
                  <div class="col-sm-6 col-xs-12 col-xs-push-1 eb-text">{iban}</div>
                </div>
              </div>
            </section>"##
        )
    }

    fn overview() -> String {
        format!(
            r#"<html><body>
            <form id="navForm" name="navForm" method="post" action="/eBankingClient/pages/konten/account-overview_08.jsf">
              <input type="hidden" name="navForm" value="navForm" />
              <a href="javascript:void(0)" onclick="mojarra.jsfcljs(document.getElementById('navForm'),{{'navForm:j_idt75:3:j_idt101:0:j_idt103':'navForm:j_idt75:3:j_idt101:0:j_idt103'}},'');return false" class="dropdown-menu__item" data-wt="subnav.Neu"><span>Neu</span></a>
              <a href="javascript:void(0)" onclick="mojarra.jsfcljs(document.getElementById('navForm'),{{'navForm:j_idt75:3:j_idt101:1:j_idt103':'navForm:j_idt75:3:j_idt101:1:j_idt103'}},'');return false" class="dropdown-menu__item" data-wt="subnav.Gelesen"><span>Gelesen</span></a>
              <input type="hidden" name="javax.faces.ViewState" id="j_id1:javax.faces.ViewState:2" value="5:6" autocomplete="off" />
            </form>
            <span class="content-header__right-sum content-header__right-sum-gesamtSaldo">1.300,00 €</span>
            <form id="contentForm" name="contentForm" method="post" action="/eBankingClient/pages/konten/account-overview_08.jsf" class="form-horizontal">
              <input type="hidden" name="contentForm" value="contentForm" />
              {}{}
              <input type="hidden" name="javax.faces.ViewState" id="j_id1:javax.faces.ViewState:5" value="5:6" autocomplete="off" />
            </form></body></html>"#,
            account_section(
                "1000000001",
                "DE00 1234 5678 1000 0000 01",
                "1.234,56 €",
                "contentForm:j_idt331:0:j_idt340"
            ),
            account_section(
                "1000000002",
                "DE00 1234 5678 1000 0000 02",
                "65,44 €",
                "contentForm:j_idt331:1:j_idt340"
            ),
        )
    }

    fn booking(date: &str, kind: &str, amount_html: &str) -> String {
        format!(
            r#"<li class="row"><div class="li-wrapper">
              <div class="col-xs-12 col-sm-2"><div class="text-supplemental">{date}</div></div>
              <div class="col-sm-12 transaction-amount-mobile visible-xs bold">{amount_html}</div>
              <div class="col-sm-12 col-sm-4 hidden-xs">
                <div class="col-sm-12 blue-bold">{kind}</div>
                <div class="col-sm-12 transactions-overview-subtext"><br /></div>
              </div>
              <div class="col-xs-12 blue-bold visible-xs"><div class="col-sm-12 blue-bold">{kind}</div></div>
              <div class="col-xs-12 col-sm-3">
              </div><div class="col-xs-12 col-sm-3 transaction-amount hidden-xs">{amount_html}</div>
            </div></li>"#
        )
    }

    fn postbox_row(i: u32, date: &str, subject: &str, product: &str) -> String {
        format!(
            r#"<li class="row"><a href="javascript:void(0)" onclick="jsf.util.chain(this,event,'changeDocumentStatus(event, {i})','mojarra.jsfcljs(document.getElementById(\'contentForm\'),{{\'contentForm:j_idt283:{i}:j_idt285\':\'contentForm:j_idt283:{i}:j_idt285\'}},\'_blank\')');return false" class="inbox-row">
              <div class="li-wrapper">
                <div class="col-sm-3"><div>{date}</div></div>
                <div class="row visible-xs"><div class="col-sm-4 col-xs-6 blue">{subject}</div></div>
                <div class="col-sm-5 blue hidden-xs">{subject}</div>
                <div class="col-sm-4 tg-color">
                  {product}
                </div>
              </div></a></li>"#
        )
    }

    #[test]
    fn reads_the_login_form_and_the_approval_step() {
        let form = form_by_id(LOGIN_PAGE, "contentForm").unwrap();
        assert_eq!(form.action, "/eBankingClient/pages/login/login.jsf");
        assert_eq!(form.get("contentForm:j_idt58"), Some("aaaa-bbbb"));
        assert_eq!(form.get("javax.faces.ViewState"), Some("1:2"));
        assert_eq!(
            form.get("contentForm:password"),
            None,
            "passwords are not copied"
        );
        assert!(is_login_page(LOGIN_PAGE));
        assert!(!is_login_page(TWO_FA_PAGE));

        let tf = two_factor(TWO_FA_PAGE).unwrap();
        assert_eq!(tf.poll, "contentForm:j_idt70:j_idt72:btnConfirm2FA");
        assert_eq!(tf.confirmed, "contentForm:j_idt70:j_idt72:btnConfirmed1");
        assert_eq!(tf.render, "contentForm:j_idt70:j_idt72:statusConfirmed");
        assert_eq!(
            tf.form.get("kundennummer"),
            None,
            "disabled inputs are not sent"
        );
        assert!(two_factor(LOGIN_PAGE).is_none());
    }

    #[test]
    fn approval_poll_results() {
        let approved = r#"<partial-response><changes><update id="contentForm:j_idt70:j_idt72:statusConfirmed"><![CDATA[<span><script>clickHiddenButton("btnConfirmed1", true)</script></span>]]></update><update id="j_id1:javax.faces.ViewState:0"><![CDATA[7:8]]></update></changes></partial-response>"#;
        assert_eq!(approval_of(approved), Approval::Approved);
        assert_eq!(
            partial_updates(approved)[1],
            (
                "j_id1:javax.faces.ViewState:0".to_string(),
                "7:8".to_string()
            )
        );
        assert_eq!(
            approval_of(&approved.replace("btnConfirmed1", "btnConfirmed2")),
            Approval::Rejected
        );
        assert_eq!(
            approval_of(
                r#"<partial-response><changes><update id="x"><![CDATA[<script>setTimeout(function(){clickHiddenButton("btnConfirm2FA", false)}, 10000)</script>]]></update></changes></partial-response>"#
            ),
            Approval::Pending
        );
    }

    async fn held_step(
        open: &mut Option<JoinHandle<CoreResult<String>>>,
        held: &mut Option<tokio::sync::oneshot::Receiver<String>>,
        started: &Arc<AtomicU32>,
    ) -> PollStep {
        let (held, started) = (held.take(), started.clone());
        poll_step(open, || async move {
            started.fetch_add(1, Ordering::SeqCst);
            Ok(held.expect("only one request").await.unwrap())
        })
        .await
    }

    /// A bank that answers only after the former 60 s limit: the request stays
    /// open and no second one is sent, so the approval cannot get lost.
    #[tokio::test]
    async fn a_held_approval_request_stays_open_until_the_bank_answers() {
        assert!(APPROVAL_POLL_TIMEOUT > Duration::from_secs(60));
        let (answer, held) = tokio::sync::oneshot::channel::<String>();
        let mut held = Some(held);
        let started = Arc::new(AtomicU32::new(0));
        let mut open = None;
        assert!(matches!(
            held_step(&mut open, &mut held, &started).await,
            PollStep::Waiting
        ));
        tokio::task::yield_now().await;
        for _ in 0..3 {
            assert!(matches!(
                held_step(&mut open, &mut held, &started).await,
                PollStep::Waiting
            ));
        }
        assert_eq!(started.load(Ordering::SeqCst), 1);

        answer.send("btnConfirmed1".to_string()).unwrap();
        let xml = loop {
            match held_step(&mut open, &mut held, &started).await {
                PollStep::Waiting => tokio::task::yield_now().await,
                PollStep::Answer(r) => break r.unwrap(),
            }
        };
        assert_eq!(approval_of(&xml), Approval::Approved);
        assert!(open.is_none());
    }

    #[test]
    fn reads_accounts_with_iban_balance_and_bookings_link() {
        let accounts = parse_accounts(&overview()).unwrap();
        assert_eq!(accounts.len(), 2);
        let a = &accounts[0];
        assert_eq!(a.number, "1000000001");
        assert_eq!(a.iban, "DE00123456781000000001");
        assert_eq!(a.product, "Tagesgeld");
        assert_eq!(a.owner, "Erika Mustermann");
        assert_eq!(a.balance_cents, 123_456);
        assert_eq!(
            a.bookings_link.as_deref(),
            Some("contentForm:j_idt331:0:j_idt340")
        );
        assert_eq!(accounts[1].balance_cents, 6_544);
        assert!(parse_accounts("<p>keine Konten</p>").unwrap().is_empty());
        let foreign =
            overview().replace("DE00 1234 5678 1000 0000 01", "DE00 9999 9999 9999 9999 99");
        assert!(
            parse_accounts(&foreign).is_err(),
            "IBAN must belong to the account number"
        );
    }

    #[test]
    fn navigation_links_come_from_the_page() {
        let html = overview();
        assert_eq!(
            link_param(&html, "subnav.Gelesen").as_deref(),
            Some("navForm:j_idt75:3:j_idt101:1:j_idt103")
        );
        assert_eq!(link_param(&html, "subnav.Fehlt"), None);
        let nav = form_by_id(&html, "navForm").unwrap();
        assert_eq!(
            nav.clicked("navForm:x"),
            vec![
                ("navForm".to_string(), "navForm".to_string()),
                ("javax.faces.ViewState".into(), "5:6".into()),
                ("navForm:x".into(), "navForm:x".into()),
            ]
        );
    }

    #[test]
    fn reads_signed_bookings_with_stable_content_ids() {
        let list = format!(
            r#"<ul id="contentForm:transactionList"><div style="display: none"><input id="contentForm:limit" type="text" name="contentForm:limit" value="1000" /></div>{}{}{}{}</ul>"#,
            booking("15.03.2026", "SoliZuschl.1", "<em>- 1,17 €</em>"),
            booking("15.03.2026", "Kap.ertr.st1", "<em>- 21,37 €</em>"),
            booking("15.03.2026", "Abschluss   ", "+            5,55 €"),
            booking("15.03.2026", "Abschluss   ", "+            5,55 €"),
        );
        let mut ids = ContentIds::default();
        let tx = parse_bookings(&list, "fordmoney:DE00", "2000-01-01", &mut ids).unwrap();
        assert_eq!(tx.len(), 4);
        assert_eq!(
            (tx[0].amount_cents, tx[0].booking_text.as_str()),
            (-117, "SoliZuschl.1")
        );
        assert_eq!(tx[0].purpose, "SoliZuschl.1");
        assert_eq!(tx[0].booking_date, "2026-03-15");
        assert_eq!(tx[1].amount_cents, -2_137);
        assert_eq!(
            (tx[2].amount_cents, tx[2].booking_text.as_str()),
            (555, "Abschluss")
        );
        assert_ne!(
            tx[2].external_id, tx[3].external_id,
            "identical bookings stay two"
        );
        let mut again = ContentIds::default();
        let tx2 = parse_bookings(&list, "fordmoney:DE00", "2000-01-01", &mut again).unwrap();
        assert_eq!(tx2[3].external_id, tx[3].external_id);
        let mut recent = ContentIds::default();
        assert!(
            parse_bookings(&list, "a", "2026-11-01", &mut recent)
                .unwrap()
                .is_empty()
        );
        let broken = list.replace("- 21,37 €", "n/a");
        assert!(parse_bookings(&broken, "a", "2000-01-01", &mut ContentIds::default()).is_err());
    }

    #[test]
    fn reads_postbox_rows_with_account_and_link() {
        let html = format!(
            "<ul>{}{}{}</ul>",
            postbox_row(
                0,
                "15.03.2026",
                "Kontoauszug Tagesgeld lfd. Nr. 10/2026",
                "Tagesgeld 1000000001"
            ),
            postbox_row(
                1,
                "07.09.2026",
                "Information zur gesetzlichen Einlagensicherung",
                "Info"
            ),
            postbox_row(
                2,
                "18.02.2026",
                "Jahressteuerbescheinigung 2025",
                "Tagesgeld 1000000002"
            ),
        );
        let rows = parse_postbox(&html);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].link, "contentForm:j_idt283:0:j_idt285");
        assert_eq!(rows[0].date, "2026-03-15");
        assert_eq!(rows[0].subject, "Kontoauszug Tagesgeld lfd. Nr. 10/2026");
        assert_eq!(rows[0].product, "Tagesgeld 1000000001");
        assert_eq!(rows[0].account_number.as_deref(), Some("1000000001"));
        assert_eq!(
            rows[1].account_number, None,
            "general information has no account"
        );
        assert_eq!(document_type(&rows[0].subject), "kontoauszug");
        assert_eq!(document_type(&rows[2].subject), "steuer");
        assert_eq!(
            document_type("Anpassung Zinskonditionen Tagesgeld"),
            "postbox"
        );
        assert_eq!(
            document_file_name(&rows[0]),
            "2026-03-15_Kontoauszug_Tagesgeld_lfd._Nr._10_2026.pdf"
        );
        assert_eq!(
            rows[2].key(),
            "2026-02-18|Jahressteuerbescheinigung 2025|Tagesgeld 1000000002"
        );
    }

    #[test]
    fn selects_and_text_fields_are_sent_with_their_values() {
        let html = r#"<form id="contentForm" method="post" action="/eBankingClient/pages/postbox/postboxArchiv.jsf">
            <input type="hidden" name="contentForm" value="contentForm" />
            <select id="contentForm:inbox-filter-produktauswahl" name="contentForm:inbox-filter-produktauswahl" size="1"> <option value="Allgemein" selected="selected">Allgemein</option> <option value="Tagesgeld">Tagesgeld</option> </select>
            <select name="contentForm:inbox-filter-reihenfolge"> <option value="0">Absteigend</option> <option value="1">Aufsteigend</option> </select>
            <input id="contentForm:inbox-datetimepicker-start" type="text" name="contentForm:inbox-datetimepicker-start" value="17.11.2025" />
            <a href="javascript:void(0)" onclick="jsf.util.chain(this,event,'x','mojarra.jsfcljs(document.getElementById(\'contentForm\'),{\'contentForm:j_idt296\':\'contentForm:j_idt296\'},\'\')');return false" class="btn" data-wt=".button-anzeigen">Aktualisieren</a>
          </form>"#;
        let mut form = form_by_id(html, "contentForm").unwrap();
        assert_eq!(
            form.get("contentForm:inbox-filter-produktauswahl"),
            Some("Allgemein")
        );
        assert_eq!(
            form.get("contentForm:inbox-filter-reihenfolge"),
            Some("0"),
            "first option without selection"
        );
        let start = form.field_ending(":inbox-datetimepicker-start").unwrap();
        form.set(&start, ARCHIVE_START);
        assert_eq!(form.get(&start), Some(ARCHIVE_START));
        assert_eq!(
            link_param(html, ".button-anzeigen").as_deref(),
            Some("contentForm:j_idt296")
        );
    }

    #[test]
    fn forms_never_post_elsewhere() {
        let page = LOGIN_PAGE.replace(
            r#"action="/eBankingClient/pages/login/login.jsf""#,
            r#"action="https://evil.example/x""#,
        );
        assert!(form_by_id(&page, "contentForm").is_none());
        assert!(ensure_host(&url::Url::parse("https://evil.example/").unwrap()).is_err());
        assert!(ensure_host(&url::Url::parse("http://ebanking.fordmoney.de/").unwrap()).is_err());
        assert!(ensure_host(&url::Url::parse("https://ebanking.fordmoney.de/x").unwrap()).is_ok());
    }

    #[test]
    fn amounts_and_dates() {
        assert_eq!(parse_amount("<em>- 1,17 €</em>"), Some(-117));
        assert_eq!(parse_amount("+            88,88 €"), Some(8_888));
        assert_eq!(parse_amount("12.345,67 €"), Some(1_234_567));
        assert_eq!(parse_amount("—"), None);
    }
}
