//! Opel Direktbank (Stellantis Bank, ebanking.opel-direktbank.de): balances
//! and bookings of the savings accounts (Tagesgeld/Festgeld) and the PDF
//! documents of the Postbox. The online banking is a PrimeFaces (JSF)
//! application without an API; the connector drives its pages (unofficial,
//! they may change at any time). Page shapes checked live 2026-11-17.
//!
//! The bank also runs FinTS (BLZ 50025000), but offers no statements there,
//! so this connector reads everything from the web banking with one login.
//!
//!  GET  /eBankingClient/login.jsf             -> `loginForm` (username, password)
//!  POST loginForm + loginBtn                  -> the same page with "Bestätigung erforderlich"
//!                                                and the poller `approvalPoller` (TwoVerify app),
//!                                                or the overview, or an error message
//!  POST login.jsf?cid=1 (ajax, the poller)    -> `<redirect url=…account-overview…>` once approved
//!  GET  …/pages/konten/account-overview_08.jsf -> `a.account-row-link` per account
//!  GET  …/konten/daily-deposit/daily-deposit-overview.jsf (Tagesgeld),
//!       …/konten/fixed-deposit/fixed-deposit-overview.jsf (Festgeld)
//!                                             -> `contentHead` filter (account, from, to) and the
//!                                                table `…transactionTable` (10 rows per page)
//!  POST contentHead (ajax, filter button)     -> filter applied; then the table is asked for
//!                                                all rows at once (ajax pagination, `_rows`)
//!  GET  …/pages/postbox/postbox.jsf           -> tabs "Ungelesen" (default) / "Gelesen",
//!                                                table `…postboxTable`; POST contentForm +
//!                                                `…:<row>:rowAction` = the PDF. Downloading an
//!                                                unread document marks it read.
//!  POST navForm-right + logoutButton          -> best effort
//!
//! The bank delivers no booking ids, so bookings get content ids. Documents
//! are remembered by date, subject and account.

use std::collections::HashMap;
use std::sync::LazyLock;

use async_trait::async_trait;
use regex::Regex;
use zeroize::Zeroize;

use super::html::{
    Form, LINK, attr, attrs, de_date, form_by_id, parse_amount, parse_date, partial_updates,
    select_options, text,
};
use super::{
    BankConnector, Challenge, ChallengeKind, ChallengeResponse, ConnectContext, ContentIds,
    Documents, FetchResult, KnownDocuments, Phase, RemoteAccount, RemoteTransaction,
    http_client_same_host,
};
use crate::documents::RemoteDocument;
use crate::error::{CoreError, CoreResult};
use crate::i18n::tr;
use crate::models::AccountKind;

const HOST: &str = "ebanking.opel-direktbank.de";
const BASE: &str = "https://ebanking.opel-direktbank.de";
const LOGIN: &str = "/eBankingClient/login.jsf";
const OVERVIEW: &str = "/eBankingClient/pages/konten/account-overview_08.jsf";
const DAILY: &str = "/eBankingClient/pages/konten/daily-deposit/daily-deposit-overview.jsf";
const FIXED: &str = "/eBankingClient/pages/konten/fixed-deposit/fixed-deposit-overview.jsf";
const POSTBOX: &str = "/eBankingClient/pages/postbox/postbox.jsf";
/// Bank code; the IBAN of a Festgeld account (the overview shows only its
/// number) is derived from it.
const BLZ: &str = "50025000";
/// Account external refs: `opeldirektbank:<IBAN>`.
pub(crate) const REF_PREFIX: &str = "opeldirektbank:";
/// Upper bound for one table request (rows).
const MAX_ROWS: usize = 5000;
/// Start of the date range for the Postbox lists.
const POSTBOX_START: &str = "01.01.2010";

static BUTTON: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<button\b([^>]*)>").unwrap());
static POLLER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)cw\("Poll",\s*"[^"]*",\s*\{id:"([^"]+)".*?u:"([^"]+)""#).unwrap()
});
static TABLE_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"cw\("DataTable",\s*"[^"]*",\s*\{id:"([^"]+)""#).unwrap());
static ROW_COUNT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"rowCount:(\d+)").unwrap());
static TR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?is)<tr\b([^>]*\bdata-ri="(\d+)"[^>]*)>(.*?)</tr\s*>"#).unwrap()
});
static TD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<td\b[^>]*>(.*?)</td\s*>").unwrap());
static ROW_LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"id="([^"]+:\d+:rowAction)""#).unwrap());
static ACCOUNT_ROW: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)ellipsis-wrap">\s*([^<]*?)\s*</span>.*?textGrayPrimary none-display">\s*([^<]*?)\s*<.*?aria-label="Kontostand">([^<]*)<"#)
        .unwrap()
});
static DIGITS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(\d{6,})\b").unwrap());
static REDIRECT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"<redirect url="([^"]+)""#).unwrap());

fn page_changed() -> CoreError {
    CoreError::protocol(tr(
        "Opel Direktbank: Seite nicht erkannt (Online-Banking geändert?).",
    ))
}

fn invalid() -> CoreError {
    CoreError::invalid(tr(
        "Opel Direktbank: Unvollständige oder ungültige Antwort. Es wurden keine Daten übernommen.",
    ))
}

fn wrong_credentials() -> CoreError {
    CoreError::Authentication {
        message: tr(
            "Opel Direktbank: Anmeldung fehlgeschlagen. Bitte Benutzerkennung und Passwort prüfen.",
        )
        .into(),
    }
}

fn session_ended() -> CoreError {
    CoreError::Authentication {
        message: tr("Opel Direktbank: Die Sitzung wurde beendet. Bitte den Abruf erneut starten.")
            .into(),
    }
}

fn not_approved() -> CoreError {
    CoreError::Authentication {
        message: tr("Opel Direktbank: Die Anmeldung wurde in der TwoVerify-App nicht bestätigt.")
            .into(),
    }
}

/// Only `https://ebanking.opel-direktbank.de` may receive the password or cookies.
fn ensure_host(url: &url::Url) -> CoreResult<()> {
    if url.scheme() == "https" && url.host_str() == Some(HOST) {
        return Ok(());
    }
    Err(CoreError::protocol(tr(
        "Opel Direktbank: Unerwartete Weiterleitung zu einer fremden Adresse – abgebrochen.",
    )))
}

fn push_challenge() -> Challenge {
    Challenge {
        kind: ChallengeKind::DecoupledPush,
        title: "Opel Direktbank TwoVerify".into(),
        message: tr("Bitte die Anmeldung in der TwoVerify-App bestätigen.").into(),
        method_name: "TwoVerify".into(),
        redirect_url: None,
        image: None,
        expires_in_seconds: None,
        poll_interval_seconds: 5,
    }
}

// ------------------------------------------------------------------ page helpers

/// Id of the first `<button>` whose `onclick` mentions `needle`.
fn button_for(html: &str, needle: &str) -> Option<String> {
    BUTTON.captures_iter(html).find_map(|c| {
        let a = attrs(&c[1]);
        attr(&a, "onclick")
            .filter(|o| o.contains(needle))
            .and(attr(&a, "id"))
            .map(str::to_string)
    })
}

/// Id of the command link with the visible text `label` ("Gelesen").
fn link_with_text(html: &str, label: &str) -> Option<String> {
    LINK.captures_iter(html).find_map(|c| {
        if text(&c[2]) != label {
            return None;
        }
        attr(&attrs(&c[1]), "id").map(str::to_string)
    })
}

fn view_state(xml: &str) -> Option<String> {
    partial_updates(xml)
        .into_iter()
        .find(|(id, _)| id.contains("javax.faces.ViewState"))
        .map(|(_, v)| v.trim().to_string())
}

fn is_login_page(html: &str) -> bool {
    form_by_id(html, "loginForm").is_some() && html.contains("name=\"loginForm:password\"")
}

// ------------------------------------------------------------------ login

/// The approval step of the login: its form and the poller.
#[derive(Debug, Clone, PartialEq)]
struct Approval {
    form: Form,
    poller: String,
    render: String,
}

fn approval_step(html: &str) -> Option<Approval> {
    let c = POLLER.captures(html)?;
    Some(Approval {
        form: form_by_id(html, "loginForm")?,
        poller: c[1].to_string(),
        render: c[2].to_string(),
    })
}

/// What a poll answered.
#[derive(Debug, PartialEq)]
enum PollResult {
    /// Approved: the bank redirects into the online banking.
    Approved(String),
    Pending,
    Rejected,
}

fn poll_result(xml: &str) -> PollResult {
    if let Some(url) = REDIRECT.captures(xml).map(|c| c[1].to_string()) {
        if url.contains("/pages/") {
            return PollResult::Approved(url);
        }
        return PollResult::Rejected;
    }
    let pending = partial_updates(xml)
        .iter()
        .any(|(id, c)| id.ends_with("pollingMessage") && c.contains("Bestätigung erforderlich"));
    if pending {
        PollResult::Pending
    } else {
        PollResult::Rejected
    }
}

// ------------------------------------------------------------------ accounts and bookings

#[derive(Debug, Clone, PartialEq)]
struct Account {
    product: String,
    number: String,
    iban: String,
    balance_cents: i64,
}

impl Account {
    fn external_ref(&self) -> String {
        format!("{REF_PREFIX}{}", self.iban)
    }

    /// Booking page of the product, if the bank has one.
    fn bookings_page(&self) -> Option<&'static str> {
        let p = self.product.to_lowercase();
        if p.contains("tagesgeld") {
            Some(DAILY)
        } else if p.contains("festgeld") {
            Some(FIXED)
        } else {
            None
        }
    }
}

/// German IBAN of an account of `blz` (ISO 7064 MOD 97-10).
fn german_iban(blz: &str, number: &str) -> Option<String> {
    let number = format!("{:0>10}", number);
    if blz.len() != 8
        || number.len() != 10
        || !format!("{blz}{number}").bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let digits = format!("{blz}{number}131400");
    let rem = digits
        .bytes()
        .fold(0u32, |r, b| (r * 10 + u32::from(b - b'0')) % 97);
    Some(format!("DE{:02}{blz}{number}", 98 - rem))
}

fn parse_accounts(html: &str) -> CoreResult<Vec<Account>> {
    let mut out = Vec::new();
    for part in html.split("account-row-link").skip(1) {
        let Some(c) = ACCOUNT_ROW.captures(part) else {
            continue;
        };
        let product = text(&c[1]);
        let id: String = text(&c[2]).chars().filter(|c| !c.is_whitespace()).collect();
        let balance_cents = parse_amount(&c[3]).ok_or_else(invalid)?;
        let (number, iban) = if id.starts_with("DE") && id.len() == 22 {
            (id[12..].to_string(), id.clone())
        } else if id.bytes().all(|b| b.is_ascii_digit()) && !id.is_empty() {
            (id.clone(), german_iban(BLZ, &id).ok_or_else(invalid)?)
        } else {
            return Err(invalid());
        };
        if out.iter().any(|a: &Account| a.iban == iban) {
            continue; // the page lists accounts twice (desktop / mobile)
        }
        out.push(Account {
            product,
            number,
            iban,
            balance_cents,
        });
    }
    Ok(out)
}

/// Rows `(row index, cell inner HTMLs)` of a table fragment.
fn table_rows(html: &str) -> Vec<(usize, String, Vec<String>)> {
    TR.captures_iter(html)
        .map(|r| {
            let cells = TD.captures_iter(&r[3]).map(|c| c[1].to_string()).collect();
            (r[2].parse().unwrap_or(0), r[3].to_string(), cells)
        })
        .collect()
}

/// Bookings of a transaction table: "Buchungstag <br> Wertstellungstag",
/// Umsatzart, Verwendungszweck, Betrag. A dated row without a readable
/// amount rejects the table.
fn parse_bookings(
    html: &str,
    account_ref: &str,
    since: &str,
) -> CoreResult<Vec<RemoteTransaction>> {
    let mut ids = ContentIds::default();
    let mut out = Vec::new();
    for (_, _, c) in table_rows(html) {
        if c.len() < 4 {
            continue;
        }
        let dates: Vec<String> = text(&c[0])
            .split_whitespace()
            .filter_map(parse_date)
            .collect();
        let Some(booking_date) = dates.first().cloned() else {
            continue;
        };
        let amount_cents = parse_amount(&c[3]).ok_or_else(invalid)?;
        let kind = text(&c[1]);
        let purpose = text(&c[2]);
        let mut t = RemoteTransaction {
            account_ref: account_ref.to_string(),
            value_date: dates
                .get(1)
                .cloned()
                .unwrap_or_else(|| booking_date.clone()),
            booking_date,
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
    /// Name of the row's "Nachricht anzeigen" link.
    link: String,
    date: String,
    subject: String,
    /// "Tagesgeld 1000000001", "Info".
    account: String,
    category: String,
    account_number: Option<String>,
}

impl PostboxRow {
    fn key(&self) -> String {
        format!("{}|{}|{}", self.date, self.subject, self.account)
    }
}

fn parse_postbox(html: &str) -> Vec<PostboxRow> {
    let mut out = Vec::new();
    for (_, row, c) in table_rows(html) {
        if c.len() < 5 {
            continue;
        }
        let (Some(link), Some(date)) = (
            ROW_LINK.captures(&row).map(|m| m[1].to_string()),
            parse_date(&text(&c[0])),
        ) else {
            continue;
        };
        let account = text(&c[3]);
        out.push(PostboxRow {
            link,
            date,
            subject: text(&c[2]),
            account_number: DIGITS.captures(&account).map(|m| m[1].to_string()),
            account,
            category: text(&c[4]),
        });
    }
    out
}

fn document_type(row: &PostboxRow) -> &'static str {
    let s = format!("{} {}", row.subject, row.category).to_lowercase();
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
pub struct OpelDirektbankConnector {
    client: Option<reqwest::Client>,
    approval: Option<Approval>,
    /// The logout form of the last page.
    logout: Option<(Form, String)>,
}

impl OpelDirektbankConnector {
    pub fn new() -> Self {
        Self::default()
    }

    fn http(&self) -> CoreResult<&reqwest::Client> {
        self.client.as_ref().ok_or(CoreError::SessionClosed)
    }

    fn target(path: &str) -> CoreResult<url::Url> {
        let url = url::Url::parse(&format!("{BASE}{path}")).map_err(|_| page_changed())?;
        ensure_host(&url)?;
        Ok(url)
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
                message: tr("Opel Direktbank: Die Anfrage wurde abgelehnt.").into(),
            });
        }
        let page = Page {
            url,
            content_type,
            body,
        };
        if page.content_type.contains("html") {
            let html = page.html();
            if let (Some(form), Some(button)) = (
                form_by_id(&html, "navForm-right"),
                BUTTON
                    .captures_iter(&html)
                    .filter_map(|c| attr(&attrs(&c[1]), "name").map(str::to_string))
                    .find(|n| n.ends_with(":logoutButton")),
            ) {
                self.logout = Some((form, button));
            }
        }
        Ok(page)
    }

    /// GET a page of the logged-in area.
    async fn get(&mut self, path: &str) -> CoreResult<String> {
        let resp = self.http()?.get(Self::target(path)?).send().await?;
        let page = self.read(resp).await?;
        let html = page.html();
        if page.url.path().ends_with("/login.jsf")
            || page.url.path().contains("logout")
            || is_login_page(&html)
        {
            return Err(session_ended());
        }
        Ok(html)
    }

    /// Plain form POST.
    async fn post(&mut self, action: &str, fields: &[(String, String)]) -> CoreResult<Page> {
        let resp = self
            .http()?
            .post(Self::target(action)?)
            .form(fields)
            .send()
            .await?;
        let page = self.read(resp).await?;
        if page.url.path().contains("logout") {
            return Err(session_ended());
        }
        Ok(page)
    }

    /// PrimeFaces ajax request: the partial-response XML.
    async fn ajax(
        &mut self,
        form: &Form,
        source: &str,
        execute: &str,
        render: &str,
        extra: &[(String, String)],
    ) -> CoreResult<String> {
        let mut fields = form.fields.clone();
        fields.push(("javax.faces.partial.ajax".into(), "true".into()));
        fields.push(("javax.faces.source".into(), source.to_string()));
        fields.push(("javax.faces.partial.execute".into(), execute.to_string()));
        fields.push(("javax.faces.partial.render".into(), render.to_string()));
        if extra.is_empty() {
            // A command (button, link, poll) is sent with its own name; a
            // table request (pagination) is not.
            fields.push((source.to_string(), source.to_string()));
        }
        fields.extend_from_slice(extra);
        let resp = self
            .http()?
            .post(Self::target(&form.action)?)
            .header("Faces-Request", "partial/ajax")
            .header("X-Requested-With", "XMLHttpRequest")
            .form(&fields)
            .send()
            .await?;
        let xml = self.read(resp).await?.html();
        if let Some(url) = REDIRECT.captures(&xml).map(|c| c[1].to_string())
            && (url.contains("login") || url.contains("logout"))
        {
            return Err(session_ended());
        }
        Ok(xml)
    }

    /// Asks a PrimeFaces data table for `rows` rows from 0 on.
    async fn all_rows(&mut self, form: &Form, table: &str, rows: usize) -> CoreResult<String> {
        let extra = [
            (format!("{table}_pagination"), "true".to_string()),
            (format!("{table}_first"), "0".to_string()),
            (
                format!("{table}_rows"),
                rows.clamp(10, MAX_ROWS).to_string(),
            ),
            (format!("{table}_skipChildren"), "true".to_string()),
            (format!("{table}_encodeFeature"), "true".to_string()),
        ];
        let xml = self.ajax(form, table, table, table, &extra).await?;
        partial_updates(&xml)
            .into_iter()
            .find(|(id, _)| id == table)
            .map(|(_, c)| c)
            .ok_or_else(invalid)
    }

    /// Applies the `contentHead` date filter (`from`/`to` DD.MM.YYYY, and
    /// the account if the page has a selection) and returns the full table.
    async fn filtered_table(
        &mut self,
        path: &str,
        account: Option<&str>,
        from: &str,
        to: &str,
    ) -> CoreResult<String> {
        let page = self.get(path).await?;
        let mut head = form_by_id(&page, "contentHead").ok_or_else(page_changed)?;
        let table = TABLE_ID
            .captures_iter(&page)
            .map(|c| c[1].to_string())
            .find(|id| id.ends_with("Table"))
            .ok_or_else(page_changed)?;
        if let (Some(name), Some(account)) = (head.field_ending(":account_input"), account) {
            if !select_options(&page, ":account_input")
                .iter()
                .any(|o| o == account)
            {
                return Ok(String::new()); // account without bookings page entry
            }
            head.set(&name, account);
        }
        let start = head
            .field_ending(":fromDate_input")
            .ok_or_else(page_changed)?;
        let end = head
            .field_ending(":endDate_input")
            .ok_or_else(page_changed)?;
        head.set(&start, from);
        head.set(&end, to);
        let button = button_for(&page, &table).ok_or_else(page_changed)?;
        let xml = self
            .ajax(&head, &button, "contentHead", &table, &[])
            .await?;
        let rows = ROW_COUNT
            .captures(&xml)
            .and_then(|c| c[1].parse::<usize>().ok())
            .unwrap_or(MAX_ROWS);
        if let Some(vs) = view_state(&xml) {
            head.set("javax.faces.ViewState", &vs);
        }
        let mut form = form_by_id(&page, "contentForm").ok_or_else(page_changed)?;
        if let Some(vs) = head.get("javax.faces.ViewState") {
            form.set("javax.faces.ViewState", vs);
        }
        self.all_rows(&form, &table, rows).await
    }

    async fn bookings(
        &mut self,
        account: &Account,
        since: &str,
        today: &str,
    ) -> CoreResult<Vec<RemoteTransaction>> {
        let Some(path) = account.bookings_page() else {
            return Ok(Vec::new());
        };
        let table = self
            .filtered_table(
                path,
                Some(&account.number),
                &de_date(since).ok_or_else(invalid)?,
                &de_date(today).ok_or_else(invalid)?,
            )
            .await?;
        parse_bookings(&table, &account.external_ref(), since)
    }

    /// The Postbox page with the tab `tab` ("Ungelesen"/"Gelesen") opened for
    /// the whole date range: `(page, contentForm with current state, rows)`.
    async fn postbox(&mut self, tab: &str) -> CoreResult<(Form, Vec<PostboxRow>)> {
        let page = self.get(POSTBOX).await?;
        let mut head = form_by_id(&page, "contentHead").ok_or_else(page_changed)?;
        let mut form = form_by_id(&page, "contentForm").ok_or_else(page_changed)?;
        let table = TABLE_ID
            .captures_iter(&page)
            .map(|c| c[1].to_string())
            .find(|id| id.ends_with("postboxTable"))
            .ok_or_else(page_changed)?;
        // Whole range: the list starts one year back.
        if let (Some(start), Some(end), Some(button)) = (
            head.field_ending(":fromDate_input"),
            head.field_ending(":endDate_input"),
            button_for(&page, "postboxTableWrapper"),
        ) {
            head.set(&start, POSTBOX_START);
            head.set(&end, &de_date(&super::today()).unwrap_or_default());
            let xml = self
                .ajax(&head, &button, "contentHead", "contentForm", &[])
                .await?;
            if let Some(vs) = view_state(&xml) {
                form.set("javax.faces.ViewState", &vs);
            }
        }
        let tab_link = link_with_text(&page, tab).ok_or_else(page_changed)?;
        let xml = self
            .ajax(&form, &tab_link, &tab_link, "contentForm", &[])
            .await?;
        if let Some(vs) = view_state(&xml) {
            form.set("javax.faces.ViewState", &vs);
        }
        let rows = ROW_COUNT
            .captures(&xml)
            .and_then(|c| c[1].parse::<usize>().ok())
            .unwrap_or(MAX_ROWS);
        let list = self.all_rows(&form, &table, rows).await?;
        Ok((form, parse_postbox(&list)))
    }

    /// Downloads the PDF of `row` from the list state in `form`.
    async fn download(&mut self, form: &Form, row: &PostboxRow) -> CoreResult<Option<Vec<u8>>> {
        let page = self.post(&form.action, &form.clicked(&row.link)).await?;
        if page.content_type.contains("pdf") && page.body.starts_with(b"%PDF") {
            Ok(Some(page.body))
        } else {
            Ok(None)
        }
    }

    /// Documents of the accounts in `wanted` (account number → external ref)
    /// not in `known`. The list is opened anew for every download: a download
    /// changes the page state (and marks an unread document read).
    async fn documents(
        &mut self,
        wanted: &HashMap<String, String>,
        known: &mut KnownDocuments,
    ) -> CoreResult<Vec<RemoteDocument>> {
        let mut out = Vec::new();
        for tab in ["Ungelesen", "Gelesen"] {
            let mut failed: Vec<String> = Vec::new();
            for _ in 0..200 {
                let (form, rows) = self.postbox(tab).await?;
                let next = rows.into_iter().find(|r| {
                    r.account_number
                        .as_ref()
                        .is_some_and(|n| wanted.contains_key(n))
                        && !known.contains(&r.key())
                        && !failed.contains(&r.key())
                });
                let Some(row) = next else {
                    break;
                };
                let Some(bytes) = self.download(&form, &row).await? else {
                    failed.push(row.key());
                    continue;
                };
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
                    doc_type: document_type(&row).into(),
                    mime_type: "application/pdf".into(),
                    file_name: document_file_name(&row),
                    bytes,
                    source_url: format!("opeldirektbank:postbox:{}", row.key()),
                    date: row.date.clone(),
                });
            }
        }
        Ok(out)
    }

    async fn after_login(&mut self, page: &Page) -> CoreResult<Phase> {
        let html = page.html();
        if let Some(step) = approval_step(&html) {
            self.approval = Some(step);
            return Ok(Phase::Challenge(push_challenge()));
        }
        if page.url.path().contains("/pages/") && !is_login_page(&html) {
            return Ok(Phase::Ready);
        }
        if is_login_page(&html) {
            return Err(wrong_credentials());
        }
        Err(page_changed())
    }
}

#[async_trait]
impl BankConnector for OpelDirektbankConnector {
    async fn begin(&mut self, ctx: &ConnectContext) -> CoreResult<Phase> {
        let login = ctx.login_name.trim();
        if login.is_empty() || ctx.secret.expose().is_empty() {
            return Err(CoreError::Authentication {
                message: tr("Opel Direktbank: Benutzerkennung und Passwort erforderlich.").into(),
            });
        }
        self.client = Some(http_client_same_host()?);
        let resp = self.http()?.get(Self::target(LOGIN)?).send().await?;
        let page = self.read(resp).await?;
        let form = form_by_id(&page.html(), "loginForm").ok_or_else(page_changed)?;
        let mut fields = form.fields.clone();
        fields.retain(|(n, _)| n != "loginForm:username");
        fields.push(("loginForm:username".into(), login.to_string()));
        fields.push(("loginForm:password".into(), ctx.secret.expose().to_string()));
        fields.push(("loginForm:loginBtn".into(), String::new()));
        let sent = self
            .http()?
            .post(Self::target(&form.action)?)
            .form(&fields)
            .send()
            .await;
        for (_, v) in fields.iter_mut() {
            v.zeroize();
        }
        let page = self.read(sent?).await?;
        self.after_login(&page).await
    }

    async fn respond(&mut self, _ctx: &ConnectContext, _r: ChallengeResponse) -> CoreResult<Phase> {
        let mut step = self.approval.clone().ok_or(CoreError::SessionClosed)?;
        let xml = self
            .ajax(&step.form, &step.poller, &step.poller, &step.render, &[])
            .await;
        let xml = match xml {
            Ok(xml) => xml,
            Err(CoreError::Network { .. }) => return Ok(Phase::Pending(push_challenge())),
            Err(e) => return Err(e),
        };
        if let Some(vs) = view_state(&xml) {
            step.form.set("javax.faces.ViewState", &vs);
            self.approval = Some(step);
        }
        match poll_result(&xml) {
            PollResult::Pending => Ok(Phase::Pending(push_challenge())),
            PollResult::Rejected => {
                self.approval = None;
                Err(not_approved())
            }
            PollResult::Approved(url) => {
                self.approval = None;
                let path = url::Url::parse(BASE)
                    .and_then(|b| b.join(&url))
                    .map_err(|_| page_changed())?;
                ensure_host(&path)?;
                self.get(path.path()).await?;
                Ok(Phase::Ready)
            }
        }
    }

    async fn fetch(&mut self, ctx: &ConnectContext) -> CoreResult<FetchResult> {
        let today = super::today();
        let overview = self.get(OVERVIEW).await?;
        let accounts = parse_accounts(&overview)?;
        if accounts.is_empty() {
            return Err(page_changed());
        }
        let refs: HashMap<String, String> = accounts
            .iter()
            .map(|a| (a.number.clone(), a.external_ref()))
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
            let account_ref = account.external_ref();
            let since = ctx.since_for(&account_ref).to_string();
            out.transactions
                .extend(self.bookings(account, &since, &today).await?);
            out.accounts.push(RemoteAccount {
                external_ref: account_ref,
                name: format!("Opel Direktbank {}", account.product),
                owner: String::new(),
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
        if let (Some(client), Some((form, button))) = (self.client.take(), self.logout.take())
            && let Ok(url) = Self::target(&form.action)
        {
            let mut fields = form.fields.clone();
            fields.push((button, String::new()));
            let _ = client.post(url).form(&fields).send().await;
        }
        self.approval = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Invented pages in the shape of the Opel Direktbank online banking; no real data.
    const LOGIN_PAGE: &str = r##"<html><body>
      <form id="loginForm" name="loginForm" method="post" action="/eBankingClient/login.jsf" enctype="application/x-www-form-urlencoded">
        <input type="hidden" name="loginForm" value="loginForm" />
        <div id="loginForm:messages" class="ui-messages ui-widget mb-3 text-center" role="alert"></div>
        <input id="loginForm:username" name="loginForm:username" type="text" value="" class="ui-inputfield" />
        <span class="ui-password"><input id="loginForm:password" name="loginForm:password" type="password" class="ui-inputfield" /></span>
        <button id="loginForm:loginBtn" name="loginForm:loginBtn" onclick="PrimeFaces.bcn(this,event,[]);" type="submit"><span>Sicher anmelden</span></button>
        <input type="hidden" name="javax.faces.ViewState" id="j_id1:javax.faces.ViewState:0" value="1:2" autocomplete="off" />
      </form></body></html>"##;

    const APPROVAL_PAGE: &str = r##"<html><body>
      <form id="loginForm" name="loginForm" method="post" action="/eBankingClient/login.jsf?cid=1">
        <input type="hidden" name="loginForm" value="loginForm" />
        <div id="loginForm:j_idt31:pollingMessage" class="ui-outputpanel"><span class="transaction-approval-message__headline">Bestätigung erforderlich</span></div>
        <div id="loginForm:j_idt31:j_idt36" style="display: none;"></div>
        <script id="loginForm:j_idt31:j_idt36_s">$(function(){PrimeFaces.cw("Poll","approvalPoller",{id:"loginForm:j_idt31:j_idt36",frequency:5,autoStart:true,fn:function(){PrimeFaces.ab({s:"loginForm:j_idt31:j_idt36",f:"loginForm",u:"loginForm:j_idt31:pollingMessage"});}});});</script>
        <input id="loginForm:username" name="loginForm:username" type="text" value="" disabled="disabled" />
        <input id="loginForm:password" name="loginForm:password" type="password" disabled="disabled" />
        <input type="hidden" name="javax.faces.ViewState" id="j_id1:javax.faces.ViewState:0" value="3:4" autocomplete="off" />
      </form></body></html>"##;

    fn account_row(i: u32, product: &str, id: &str, balance: &str) -> String {
        format!(
            r##"<a id="contentForm:j_idt118:0:j_idt120:j_idt126:{i}:open-acc-overview-mobile" href="javascript:void(0)" class="ui-commandlink ui-widget account-row-link w-full" type="button">
              <div class="md:col-4"><div class="flex"><span class="inline-block py-1 border-round-sm text-lg font-normal md:font-bold ellipsis-wrap"> {product} </span></div>
              <div class="text-sm textGrayPrimary none-display"> {id} <span class="font-bold textGrayPrimary"> </span></div></div>
              <div class="col-12 md:col-3"><span class="inline-block py-1 text-lg font-bold" aria-label="Kontostand">{balance}&nbsp;€ </span></div></a>"##
        )
    }

    fn booking(ri: usize, dates: &str, kind: &str, purpose: &str, amount: &str) -> String {
        format!(
            r#"<tr data-ri="{ri}" class="ui-widget-content ui-datatable-even"><td role="gridcell">{dates}</td><td role="gridcell">{kind} </td><td role="gridcell">{purpose}</td><td role="gridcell">{amount}</td></tr>"#
        )
    }

    fn postbox_row(ri: usize, date: &str, subject: &str, account: &str, category: &str) -> String {
        format!(
            r##"<tr data-ri="{ri}" class="ui-widget-content"><td role="gridcell">{date}</td><td role="gridcell"></td><td role="gridcell">{subject}</td><td role="gridcell"> {account}</td><td role="gridcell">{category}</td><td role="gridcell" class="rowActionCol"><a id="contentForm:j_idt116:postboxTable:{ri}:rowAction" href="javascript:void(0)" class="ui-commandlink ui-widget rowActionLink" onclick="PrimeFaces.addSubmitParam('contentForm',{{'contentForm:j_idt116:postboxTable:{ri}:rowAction':'contentForm:j_idt116:postboxTable:{ri}:rowAction'}}).submit('contentForm');return false;" aria-label="Nachricht anzeigen"><i class="faw-solid faw-eye"></i></a></td></tr>"##
        )
    }

    #[test]
    fn login_and_approval_pages() {
        let form = form_by_id(LOGIN_PAGE, "loginForm").unwrap();
        assert_eq!(form.action, "/eBankingClient/login.jsf");
        assert_eq!(form.get("javax.faces.ViewState"), Some("1:2"));
        assert_eq!(
            form.get("loginForm:password"),
            None,
            "passwords are not copied"
        );
        assert!(is_login_page(LOGIN_PAGE));
        assert!(approval_step(LOGIN_PAGE).is_none());

        let step = approval_step(APPROVAL_PAGE).unwrap();
        assert_eq!(step.form.action, "/eBankingClient/login.jsf?cid=1");
        assert_eq!(step.poller, "loginForm:j_idt31:j_idt36");
        assert_eq!(step.render, "loginForm:j_idt31:pollingMessage");
        assert_eq!(
            step.form.get("loginForm:username"),
            None,
            "disabled fields are not sent"
        );
        assert_eq!(step.form.get("javax.faces.ViewState"), Some("3:4"));
    }

    #[test]
    fn poll_answers() {
        let pending = r#"<partial-response><changes><update id="loginForm:j_idt31:pollingMessage"><![CDATA[<div><span>Bestätigung erforderlich</span></div>]]></update><update id="j_id1:javax.faces.ViewState:0"><![CDATA[5:6]]></update></changes></partial-response>"#;
        assert_eq!(poll_result(pending), PollResult::Pending);
        assert_eq!(view_state(pending).as_deref(), Some("5:6"));
        let approved = r#"<partial-response><redirect url="/eBankingClient/pages/konten/account-overview_08.jsf"></redirect></partial-response>"#;
        assert_eq!(
            poll_result(approved),
            PollResult::Approved("/eBankingClient/pages/konten/account-overview_08.jsf".into())
        );
        let rejected = pending.replace("Bestätigung erforderlich", "Die Anmeldung wurde abgelehnt");
        assert_eq!(poll_result(&rejected), PollResult::Rejected);
        let logout = r#"<partial-response><redirect url="/eBankingClient/logout-sec.xhtml"></redirect></partial-response>"#;
        assert_eq!(poll_result(logout), PollResult::Rejected);
    }

    #[test]
    fn reads_accounts_and_derives_missing_ibans() {
        let html = format!(
            "{}{}{}",
            account_row(0, "Tagesgeld", "DE88 5002 5000 1000 0000 01", "2.222,22"),
            account_row(1, "Festgeld", "1000000002", "50.000,00"),
            account_row(0, "Tagesgeld", "DE88 5002 5000 1000 0000 01", "2.222,22"),
        );
        let accounts = parse_accounts(&html).unwrap();
        assert_eq!(accounts.len(), 2, "duplicate rows are listed once");
        assert_eq!(accounts[0].product, "Tagesgeld");
        assert_eq!(accounts[0].iban, "DE88500250001000000001");
        assert_eq!(accounts[0].number, "1000000001");
        assert_eq!(accounts[0].balance_cents, 222_222);
        assert_eq!(accounts[0].bookings_page(), Some(DAILY));
        assert_eq!(accounts[1].number, "1000000002");
        assert_eq!(accounts[1].iban, german_iban(BLZ, "1000000002").unwrap());
        assert_eq!(accounts[1].balance_cents, 5_000_000);
        assert_eq!(accounts[1].bookings_page(), Some(FIXED));
        assert!(parse_accounts(&account_row(0, "Tagesgeld", "kaputt", "1,00")).is_err());
    }

    #[test]
    fn german_ibans() {
        // Public example account of the IBAN standard documentation.
        assert_eq!(
            german_iban("37040044", "532013000").as_deref(),
            Some("DE89370400440532013000")
        );
        assert_eq!(german_iban("1234", "1"), None);
    }

    #[test]
    fn reads_bookings_with_value_dates_and_signs() {
        let table = [
            booking(
                0,
                "12.02.2025 <br />14.02.2025",
                "Abschluss",
                "",
                "+ 5,55 €",
            ),
            booking(
                1,
                "12.02.2025 <br />14.02.2025",
                "Abschluss",
                "",
                "+ 5,55 €",
            ),
            booking(
                2,
                "12.02.2025 <br />14.02.2025",
                "Kap.ertr.st1",
                "",
                "<span class=\"transaction-negative\">- 1,17 €</span>",
            ),
            booking(
                3,
                "19.05.2026 <br />19.05.2026",
                "Zinsen",
                "Festgeld Ablauf",
                "+ 3.333,33 €",
            ),
        ]
        .join("");
        let tx = parse_bookings(&table, "opeldirektbank:DE00", "2000-01-01").unwrap();
        assert_eq!(tx.len(), 4);
        assert_eq!(
            (tx[0].booking_date.as_str(), tx[0].value_date.as_str()),
            ("2025-02-12", "2025-02-14")
        );
        assert_eq!(
            (
                tx[0].amount_cents,
                tx[0].booking_text.as_str(),
                tx[0].purpose.as_str()
            ),
            (555, "Abschluss", "Abschluss")
        );
        assert_ne!(
            tx[0].external_id, tx[1].external_id,
            "identical bookings stay two"
        );
        assert_eq!(tx[2].amount_cents, -117);
        assert_eq!(
            (tx[3].amount_cents, tx[3].purpose.as_str()),
            (333_333, "Festgeld Ablauf")
        );
        let again = parse_bookings(&table, "opeldirektbank:DE00", "2000-01-01").unwrap();
        assert_eq!(again[1].external_id, tx[1].external_id);
        let recent = parse_bookings(&table, "opeldirektbank:DE00", "2026-01-01").unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].external_id, tx[3].external_id);
        assert!(parse_bookings(&table.replace("- 1,17 €", "n/a"), "a", "2000-01-01").is_err());
    }

    #[test]
    fn reads_postbox_rows() {
        let table = [
            postbox_row(
                0,
                "15.03.2026",
                "Kontoauszug Nr. 10",
                "<span class=\"\">Tagesgeld </span> <span>1000000001 </span>",
                "Kontoauszug",
            ),
            postbox_row(
                1,
                "24.07.2026",
                "Zustimmung der Geschäftsbedingungen",
                "Info",
                "Allgemein",
            ),
            postbox_row(
                2,
                "30.03.2026",
                "Elektronische Jahressteuerbescheinigung (Jahr 2025)",
                "Tagesgeld 1000000001",
                "Steuerrelevante Daten",
            ),
        ]
        .join("");
        let rows = parse_postbox(&table);
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows[0].link,
            "contentForm:j_idt116:postboxTable:0:rowAction"
        );
        assert_eq!(
            (rows[0].date.as_str(), rows[0].subject.as_str()),
            ("2026-03-15", "Kontoauszug Nr. 10")
        );
        assert_eq!(rows[0].account, "Tagesgeld 1000000001");
        assert_eq!(rows[0].account_number.as_deref(), Some("1000000001"));
        assert_eq!(rows[1].account_number, None);
        assert_eq!(document_type(&rows[0]), "kontoauszug");
        assert_eq!(document_type(&rows[1]), "postbox");
        assert_eq!(document_type(&rows[2]), "steuer");
        assert_eq!(
            document_file_name(&rows[0]),
            "2026-03-15_Kontoauszug_Nr._10.pdf"
        );
        assert_eq!(
            rows[0].key(),
            "2026-03-15|Kontoauszug Nr. 10|Tagesgeld 1000000001"
        );
    }

    #[test]
    fn page_elements_are_found_by_role() {
        let page = r##"<form id="contentHead" method="post" action="/eBankingClient/pages/konten/daily-deposit/daily-deposit-overview.jsf">
            <input type="hidden" name="contentHead" value="contentHead" />
            <select id="contentHead:filterDateForm:j_idt123:account_input" name="contentHead:filterDateForm:j_idt123:account_input"><option value="Eigene Geldanlagen" disabled="disabled">Eigene Geldanlagen</option><option value="1000000001" selected="selected">DE88 …</option><option value="1000000003">DE…</option></select>
            <input id="contentHead:filterDateForm:j_idt123:fromDate_input" name="contentHead:filterDateForm:j_idt123:fromDate_input" type="text" value="17.11.2025" />
            <button id="contentHead:filterDateForm:j_idt123:j_idt137" name="contentHead:filterDateForm:j_idt123:j_idt137" onclick="PrimeFaces.ab({s:&quot;contentHead:filterDateForm:j_idt123:j_idt137&quot;,f:&quot;contentHead&quot;,u:&quot;contentForm:dailyTableWrapper:transactionTable&quot;});return false;" type="submit"></button>
          </form>
          <script>$(function(){PrimeFaces.cw("DataTable","widget_x",{id:"contentForm:dailyTableWrapper:transactionTable",paginator:{rows:10,rowCount:38}});});</script>
          <div id="viewTabs"><a id="contentForm:j_idt111" href="javascript:void(0)" class="ui-commandlink"> Ungelesen </a><a id="contentForm:j_idt113" href="javascript:void(0)" class="ui-commandlink"> Gelesen </a></div>"##;
        let head = form_by_id(page, "contentHead").unwrap();
        assert_eq!(
            head.get("contentHead:filterDateForm:j_idt123:account_input"),
            Some("1000000001")
        );
        assert_eq!(
            select_options(page, ":account_input"),
            vec!["1000000001", "1000000003"]
        );
        assert_eq!(
            button_for(page, "contentForm:dailyTableWrapper:transactionTable").as_deref(),
            Some("contentHead:filterDateForm:j_idt123:j_idt137")
        );
        assert_eq!(
            TABLE_ID.captures(page).map(|c| c[1].to_string()).as_deref(),
            Some("contentForm:dailyTableWrapper:transactionTable")
        );
        assert_eq!(
            ROW_COUNT
                .captures(page)
                .map(|c| c[1].to_string())
                .as_deref(),
            Some("38")
        );
        assert_eq!(
            link_with_text(page, "Gelesen").as_deref(),
            Some("contentForm:j_idt113")
        );
    }

    #[test]
    fn forms_never_post_elsewhere() {
        let foreign = LOGIN_PAGE.replace(
            r#"action="/eBankingClient/login.jsf""#,
            r#"action="https://evil.example/x""#,
        );
        assert!(form_by_id(&foreign, "loginForm").is_none());
        let protocol_relative = LOGIN_PAGE.replace(
            r#"action="/eBankingClient/login.jsf""#,
            r#"action="//evil.example/x""#,
        );
        assert!(form_by_id(&protocol_relative, "loginForm").is_none());
        assert!(ensure_host(&url::Url::parse("https://evil.example/").unwrap()).is_err());
        assert!(
            ensure_host(&url::Url::parse("http://ebanking.opel-direktbank.de/").unwrap()).is_err()
        );
        assert!(
            ensure_host(&url::Url::parse("https://ebanking.opel-direktbank.de/x").unwrap()).is_ok()
        );
    }

    #[test]
    fn amounts_and_dates() {
        assert_eq!(parse_amount("<span>- 1,17 €</span>"), Some(-117));
        assert_eq!(parse_amount("+ 3.333,33 €"), Some(333_333));
        assert_eq!(parse_amount("50.000,00&nbsp;€"), Some(5_000_000));
        assert_eq!(parse_amount("—"), None);
    }
}
