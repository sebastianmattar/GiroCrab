//! easybank (formerly Barclays Deutschland, BAWAG Group) credit cards via the
//! web portal's JSON API (`banking.easybank.de`, VeriBranch). No PSD2 access.
//!
//! Login: user name + password, then an SMS-TAN unless the portal recognises
//! the client by its tracking cookie (sent back as `BrowserTrackingToken`);
//! the cookies are persisted in the connection state so later syncs skip the
//! TAN. Every request needs `UniqueKey` (from the login page / login
//! response), the `XSRF-TOKEN` cookie and an Application-Insights style
//! `Request-Id` (`<trace uuid>.2.<uuid>`); other ids are rejected with
//! `VINZ_PV_001`. Details: `docs/easybank-connector.md`.
use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use reqwest::cookie::{CookieStore, Jar};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    BankConnector, Challenge, ChallengeKind, ChallengeResponse, ConnectContext, Documents,
    FetchResult, Phase, RemoteAccount, RemoteTransaction,
};
use crate::{
    error::{CoreError, CoreResult},
    i18n::tr,
    models::AccountKind,
    secret::{SecretJson, SecretString},
};

const BASE: &str = "https://banking.easybank.de";
/// The portal keeps at most two years of history.
const MAX_DAYS: i64 = 730;
/// Without a fresh SCA the portal serves 90 days (PSD2).
const NO_TAN_DAYS: i64 = 89;
/// Session cookies are recreated on every login; everything else (the
/// browser-tracking cookie) is kept so the portal recognises GiroCrab.
const SESSION_COOKIES: [&str; 6] = [
    "ASP.NET_SessionId",
    ".ASPXAUTH",
    "XSRF-TOKEN",
    "__RequestVerificationToken",
    "ARRAffinity",
    "ARRAffinitySameSite",
];

fn invalid() -> CoreError {
    CoreError::InvalidInput {
        message: tr("easybank: Unvollständige oder ungültige Portalantwort. Es wurden keine Daten übernommen.").into(),
    }
}

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().map(str::trim).unwrap_or("")
}

fn required<'a>(v: &'a Value, key: &str) -> CoreResult<&'a str> {
    Some(text(v, key))
        .filter(|s| !s.is_empty())
        .ok_or_else(invalid)
}

/// `YYYY-MM-DD` of an ISO timestamp; `None` for the portal's empty `0001-01-01`.
fn date(v: &Value, key: &str) -> Option<String> {
    let s = text(v, key).get(..10)?;
    jiff::civil::Date::strptime("%Y-%m-%d", s).ok()?;
    (!s.starts_with("0001-")).then(|| s.to_string())
}

/// `{ "Value": 12.34, "Currency": { "Code": "EUR" } }` → (cents, currency).
/// The portal returns binary-float artefacts; round once at the boundary.
fn money(v: &Value, key: &str) -> CoreResult<(i64, String)> {
    let m = &v[key];
    let value = m["Value"]
        .as_f64()
        .filter(|n| n.is_finite() && n.abs() < 1e12)
        .ok_or_else(invalid)?;
    Ok((
        (value * 100.0).round() as i64,
        text(&m["Currency"], "Code").to_uppercase(),
    ))
}

fn account(a: &Value) -> CoreResult<RemoteAccount> {
    let number = required(a, "Number")?;
    let (balance_cents, currency) = money(a, "CurrentBalance")?;
    let name = [text(a, "ProductName"), text(a, "AccountName")]
        .into_iter()
        .find(|s| !s.is_empty())
        .unwrap_or("easybank Kreditkarte");
    Ok(RemoteAccount {
        external_ref: format!("card:{number}"),
        name: name.into(),
        owner: String::new(),
        iban: text(a, "IBAN").replace(' ', ""),
        account_number: number.into(),
        currency: if currency.is_empty() {
            "EUR".into()
        } else {
            currency
        },
        kind: AccountKind::CreditCard,
        balance_cents,
        balance_date: super::today(),
    })
}

/// One booking; `None` for pending (pre-authorised) card payments, which the
/// portal re-issues with a new id once they are booked.
fn transaction(account_ref: &str, t: &Value) -> CoreResult<Option<RemoteTransaction>> {
    if t["IsPending"].as_bool().unwrap_or(false) {
        return Ok(None);
    }
    let booking_date = date(t, "PostingDate")
        .or_else(|| date(t, "TransactionDate"))
        .ok_or_else(invalid)?;
    let value_date = date(t, "TransactionDate")
        .or_else(|| date(t, "ValueDate"))
        .unwrap_or_else(|| booking_date.clone());
    // `Amount` is the original currency and may be 0 without a currency;
    // `LocalCurrencyAmount` is always the card currency. Both are unsigned.
    let (local, local_currency) = money(t, "LocalCurrencyAmount")?;
    let sign = match text(t, "TransactionNature") {
        "Debit" => -1,
        "Credit" => 1,
        _ => return Err(invalid()),
    };
    let merchant = text(t, "MerchantName");
    let description = text(t, "Description");
    let counterparty = if description.is_empty() {
        merchant
    } else {
        description
    };
    let mut purpose = if merchant != counterparty {
        merchant.to_string()
    } else {
        String::new()
    };
    if let Ok((original, currency)) = money(t, "Amount")
        && original != 0
        && !currency.is_empty()
        && currency != local_currency
    {
        let foreign = format!(
            "{} {}.{:02}",
            currency,
            original.abs() / 100,
            original.abs() % 100
        );
        purpose = [purpose.as_str(), &foreign]
            .iter()
            .filter(|s| !s.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(" · ");
    }
    let booking_text = [
        text(t, "TransactionCodeDescription"),
        text(t, "TransactionCode"),
    ]
    .into_iter()
    .find(|s| !s.is_empty())
    .unwrap_or("")
    .to_string();
    Ok(Some(RemoteTransaction {
        account_ref: account_ref.into(),
        // `InternalID` is regenerated on every login (live-tested: a second
        // sync duplicated every booking) and the portal's reference numbers
        // repeat, so `store_fetch` derives a content id (dates, amount,
        // counterparty, purpose; identical bookings are numbered).
        external_id: String::new(),
        booking_date,
        value_date,
        amount_cents: sign * local.abs(),
        currency: if local_currency.is_empty() {
            "EUR".into()
        } else {
            local_currency
        },
        counterparty: counterparty.into(),
        counterparty_iban: String::new(),
        purpose,
        booking_text,
        ..Default::default()
    }))
}

/// Maps collected cards: `{ "version": 1, "cards": [ { "account": {…},
/// "transactions": [ … ] } ] }` (field names as the portal returns them).
pub(crate) fn snapshot(json: &str) -> CoreResult<FetchResult> {
    let v: Value = serde_json::from_str(json).map_err(|_| invalid())?;
    if v["version"].as_u64() != Some(1) {
        return Err(invalid());
    }
    let cards = v["cards"].as_array().ok_or_else(invalid)?;
    let mut out = FetchResult::default();
    for card in cards {
        let acc = account(&card["account"])?;
        for t in card["transactions"].as_array().ok_or_else(invalid)? {
            if let Some(tx) = transaction(&acc.external_ref, t)? {
                out.transactions.push(tx);
            }
        }
        out.accounts.push(acc);
    }
    if out.accounts.is_empty() {
        return Err(invalid());
    }
    Ok(out)
}

fn bank_error(item: &Value) -> CoreError {
    let e = &item["Result"]["Error"];
    let message = [text(e, "DisplayMessage"), text(e, "Description")]
        .into_iter()
        .find(|s| !s.is_empty())
        .unwrap_or("")
        .to_string();
    CoreError::Bank {
        code: format!("easybank.{}", text(e, "Code")),
        message: if message.is_empty() {
            tr("easybank: Die Anfrage wurde abgelehnt.").into()
        } else {
            message
        },
    }
}

fn captcha() -> CoreError {
    CoreError::Authentication {
        message: tr("easybank verlangt ein Captcha. Bitte einmal auf banking.easybank.de anmelden und den Abruf danach wiederholen.").into(),
    }
}

/// `UniqueKey` the login page embeds: `VeriBranch.Config.UniqueKey = '…';`.
fn page_unique_key(html: &str) -> Option<String> {
    let rest = html.split("VeriBranch.Config.UniqueKey").nth(1)?;
    let start = rest.find('\'')? + 1;
    let end = start + rest[start..].find('\'')?;
    let key = rest[start..end].trim();
    (!key.is_empty() && key.len() < 100).then(|| key.to_string())
}

/// `%XX` decoding as `decodeURIComponent` does for the XSRF cookie ('+' stays).
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Persistable cookies (`name=value`) from a `Cookie` header value.
/// A persisted `name=value` pair is only replayed when it cannot smuggle
/// attributes (`;`) or extra header lines (control characters) into the jar.
fn valid_cookie(c: &str) -> bool {
    c.split_once('=')
        .is_some_and(|(name, _)| !name.trim().is_empty())
        && !c.contains(';')
        && !c.chars().any(|ch| ch.is_control())
}

fn persistent_cookies(header: &str) -> Vec<String> {
    header
        .split(';')
        .map(str::trim)
        .filter(|c| {
            let name = c.split('=').next().unwrap_or("");
            !c.is_empty() && !SESSION_COOKIES.contains(&name) && !name.starts_with("ai_")
        })
        .map(str::to_string)
        .collect()
}

/// Request body for one page of bookings (all fields are required).
fn transactions_query(account: &Value, days: i64, page: u32) -> Value {
    let end = jiff::Timestamp::now();
    let start = end - jiff::SignedDuration::from_hours(days * 24);
    let millis = |t: jiff::Timestamp| t.strftime("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    json!({
        "SelectedApprovalRule": 0, "SelectedApprovalSubRule": 0, "DisableNonStp": false,
        "IsSourceFutureDated": false, "OpenCaseAndExecuteTransaction": false,
        "OpenConditionalCase": false, "OpenCaseAndNotExecute": false,
        "Account": account,
        "StartDate": millis(start),
        "EndDate": millis(end),
        "HasStartAmountValue": false, "HasEndAmountValue": false, "TransactionFlag": "All",
        "TransactionTypeIndicator": 0, "Status": "ALL", "OTPAlreadyChecked": false,
        "IsExporting": false, "hasOptionDetail": false, "CardNetwork": "Undefined",
        "IsDisputeOnly": false, "IsStatementDateSelected": false,
        "StatementDate": "0001-01-01T00:00:00+00:00", "FetchAll": false, "HasFilters": false,
        "HasSorting": false, "PageSize": 50, "PageIndex": page, "SortingOrder": "Ascending",
        "IncludeTotalCount": false, "Take": 50, "Skip": page * 50,
        "IsDisputeCategorySelection": false, "TwoFactorAuthentication": { "Password": "" },
        "Currency": "EUR", "Description": ""
    })
}

/// Cards from a flow start's `InitialCallResponses` (`…/GetAccountList`).
fn initial_account_list(flow: &Value) -> Vec<Value> {
    flow["InitialCallResponses"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|c| text(&c["Config"], "Url").ends_with("GetAccountList"))
        .and_then(|c| c["Response"]["Item"]["AccountList"].as_array().cloned())
        .unwrap_or_default()
}

/// Days from `since` (YYYY-MM-DD) until today, within the portal's limits.
fn days_since(since: &str) -> i64 {
    jiff::civil::Date::strptime("%Y-%m-%d", since)
        .map(|d| crate::dates::days_between(d, crate::dates::today()) + 1)
        .unwrap_or(MAX_DAYS)
        .clamp(1, MAX_DAYS)
}

#[derive(Default)]
pub struct EasybankConnector {
    jar: Arc<Jar>,
    client: Option<reqwest::Client>,
    unique_key: String,
    trace_id: String,
    /// `Item.Request` of the last login step, echoed to the next one.
    login_request: Value,
    logged_in: bool,
}

impl EasybankConnector {
    pub fn new() -> Self {
        Self::default()
    }

    fn url() -> reqwest::Url {
        reqwest::Url::parse(BASE).expect("static url")
    }

    fn client(&mut self) -> CoreResult<reqwest::Client> {
        if let Some(c) = &self.client {
            return Ok(c.clone());
        }
        let c = reqwest::Client::builder()
            .cookie_provider(self.jar.clone())
            .user_agent(super::BROWSER_UA)
            .timeout(std::time::Duration::from_secs(60))
            .redirect(super::same_host_redirects())
            .build()?;
        self.client = Some(c.clone());
        Ok(c)
    }

    fn restore_cookies(&self, cookies: &[SecretString]) {
        let url = Self::url();
        for c in cookies {
            let c = c.expose();
            if valid_cookie(c) {
                self.jar.add_cookie_str(
                    &format!("{c}; Domain=banking.easybank.de; Path=/; Secure; HttpOnly"),
                    &url,
                );
            }
        }
    }

    fn cookie_header(&self) -> String {
        self.jar
            .cookies(&Self::url())
            .and_then(|h| h.to_str().ok().map(str::to_string))
            .unwrap_or_default()
    }

    fn xsrf(&self) -> String {
        self.cookie_header()
            .split(';')
            .map(str::trim)
            .find_map(|c| c.strip_prefix("XSRF-TOKEN="))
            .map(percent_decode)
            .unwrap_or_default()
    }

    /// POST `path` with the headers the portal checks; returns the JSON
    /// envelope (`Item` + `Result`). An HTML answer means the session is gone.
    async fn post(&mut self, path: &str, body: &Value) -> CoreResult<Value> {
        let client = self.client()?;
        let request_id = format!("{}.2.{}", self.trace_id, uuid::Uuid::new_v4());
        let resp = client
            .post(format!("{BASE}/{path}"))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/plain, */*")
            .header("Accept-Language", "de-DE")
            .header("X-Requested-With", "XMLHttpRequest")
            .header("UniqueKey", &self.unique_key)
            .header("X-XSRF-TOKEN", self.xsrf())
            .header("Request-Id", request_id)
            .header("Origin", BASE)
            .header("Referer", format!("{BASE}/"))
            .body(body.to_string())
            .send()
            .await?;
        let status = resp.status();
        let text = super::read_text_limited(resp).await?;
        serde_json::from_str::<Value>(&text).map_err(|_| CoreError::Protocol {
            message: format!(
                "easybank: {path}: HTTP {} {}",
                status.as_u16(),
                tr("ohne JSON-Antwort (Sitzung abgelaufen oder Portal geändert)")
            ),
        })
    }

    /// Like `post`, but fails unless `Result.IsSuccess`; returns `Item`.
    async fn call(&mut self, path: &str, body: &Value) -> CoreResult<Value> {
        let v = self.post(path, body).await?;
        if v["Result"]["IsSuccess"].as_bool() != Some(true) {
            return Err(bank_error(&v));
        }
        Ok(v["Item"].clone())
    }

    /// Reads a login step's answer and decides what comes next.
    async fn after_step(&mut self, item: Value) -> CoreResult<Phase> {
        if item["Request"]["NeedCaptcha"].as_bool() == Some(true) {
            return Err(captcha());
        }
        let response = &item["Response"];
        let key = text(response, "UniqueKey");
        if !key.is_empty() {
            self.unique_key = key.to_string();
        }
        self.login_request = item["Request"].clone();
        match text(response, "NextFlowItemType") {
            "OTPSecurity" => {
                // Sending the SMS is a separate call; if it fails the user can
                // still get a code (the portal may already have sent one).
                let body = self.login_request.clone();
                let _ = self
                    .call("call/directive/VbOtpboxDirective/SendOTP", &body)
                    .await;
                let phone = text(&response["Customer"], "MaskedMobilePhoneNumber");
                let digits: String = phone.chars().filter(|c| c.is_ascii_digit()).collect();
                Ok(Phase::Challenge(Challenge {
                    kind: ChallengeKind::Code,
                    title: "easybank SMS-TAN".into(),
                    message: if digits.is_empty() {
                        tr("Bitte die SMS-TAN eingeben, die easybank an Ihre Mobilnummer gesendet hat.").into()
                    } else {
                        crate::i18n::trf(
                            "Bitte die SMS-TAN eingeben, die easybank an die Mobilnummer mit der Endung {0} gesendet hat.",
                            &[&digits],
                        )
                    },
                    method_name: "SMS-TAN".into(),
                    redirect_url: None,
                    image: None,
                    expires_in_seconds: Some(300),
                    poll_interval_seconds: 0,
                }))
            }
            "SuccessfulAuthentication" | "MainPage" | "" => {
                self.finish_login().await?;
                Ok(Phase::Ready)
            }
            _ => Err(CoreError::Authentication {
                message: tr("easybank verlangt einen zusätzlichen Schritt (z. B. neue AGB oder Passwortwechsel). Bitte einmal auf banking.easybank.de anmelden und den Abruf danach wiederholen.").into(),
            }),
        }
    }

    async fn finish_login(&mut self) -> CoreResult<()> {
        let mut body = self.login_request.clone();
        if body.is_object() {
            body["Password"] = Value::Null;
            body["OTPPassword"] = json!("");
        }
        // The web app continues to its dashboard (`RetailLanding`) here. The
        // data calls don't need it and the server may answer "Account
        // configuration not found for transactionName:RetailLanding", so this
        // step is best effort; `fetch` opens the transactions flow itself.
        let _ = self.post("call/flow/Login/AfterLogin", &body).await;
        self.logged_in = true;
        Ok(())
    }

    async fn bookings(&mut self, account: &Value, days: i64) -> CoreResult<Option<Vec<Value>>> {
        let mut out = Vec::new();
        for page in 0..60 {
            let item = self
                .call(
                    "call/directive/TransactionViewDirective/GetTransactions",
                    &transactions_query(account, days, page),
                )
                .await?;
            if item["OTPRequired"].as_bool() == Some(true) {
                return Ok(None);
            }
            let list = item["Transactions"].as_array().cloned().unwrap_or_default();
            let n = list.len();
            out.extend(list);
            if item["HasMore"].as_bool() != Some(true) || n == 0 {
                return Ok(Some(out));
            }
        }
        Err(invalid())
    }
}

/// 12 statements per page; 10 pages cover the portal's whole archive.
const MAX_STATEMENT_PAGES: u32 = 10;
const MAX_PDF_BYTES: usize = 20 * 1024 * 1024;

#[async_trait]
impl BankConnector for EasybankConnector {
    async fn begin(&mut self, ctx: &ConnectContext) -> CoreResult<Phase> {
        if ctx.login_name.trim().is_empty() || ctx.secret.expose().is_empty() {
            return Err(CoreError::Authentication {
                message: tr("Benutzername und Passwort erforderlich").into(),
            });
        }
        self.restore_cookies(&stored_state(&ctx.state).cookies);
        self.trace_id = uuid::Uuid::new_v4().to_string();
        let client = self.client()?;
        let html =
            super::read_text_limited(client.get(format!("{BASE}/Login")).send().await?).await?;
        self.unique_key = page_unique_key(&html).ok_or_else(|| CoreError::Protocol {
            message: tr("easybank: Anmeldeseite nicht erkannt (Portal geändert?).").into(),
        })?;
        self.call("services/flow/LoginTransaction", &json!({}))
            .await?;
        let body = json!({
            "DefineLater": false, "DisableNonStp": false, "IsCaptchaRequired": true,
            "IsNotLoginTransaction": false, "IsSourceFutureDated": false, "OTPPassword": "",
            "OpenCaseAndExecuteTransaction": false, "OpenCaseAndNotExecute": false,
            "OpenConditionalCase": false, "Password": ctx.secret.expose(),
            "SelectedApprovalRule": 0, "SelectedApprovalSubRule": 0,
            "SkipAuthenticationItem": false, "UserName": ctx.login_name.trim()
        });
        let v = self
            .post("services/flow/logintransaction/firstlevel/next", &body)
            .await?;
        if v["Item"]["Request"]["NeedCaptcha"].as_bool() == Some(true) {
            return Err(captcha());
        }
        if v["Result"]["IsSuccess"].as_bool() != Some(true) {
            let err = bank_error(&v);
            return Err(match err {
                CoreError::Bank { message, .. } => CoreError::Authentication { message },
                e => e,
            });
        }
        self.after_step(v["Item"].clone()).await
    }

    async fn respond(
        &mut self,
        _ctx: &ConnectContext,
        response: ChallengeResponse,
    ) -> CoreResult<Phase> {
        let ChallengeResponse::Code(tan) = response else {
            return Err(CoreError::InvalidInput {
                message: tr("Bitte die SMS-TAN eingeben.").into(),
            });
        };
        let tan = tan.expose().trim().to_string();
        if tan.is_empty() {
            return Err(CoreError::InvalidInput {
                message: tr("Bitte die SMS-TAN eingeben.").into(),
            });
        }
        let mut body = self.login_request.clone();
        if !body.is_object() {
            return Err(invalid());
        }
        body["OTPPassword"] = json!(tan);
        body["Password"] = Value::Null;
        let v = self
            .post("services/flow/logintransaction/otpsecurity/next", &body)
            .await?;
        if v["Result"]["IsSuccess"].as_bool() != Some(true) {
            let err = bank_error(&v);
            return Err(match err {
                CoreError::Bank { message, .. } => CoreError::Authentication { message },
                e => e,
            });
        }
        let mut item = v["Item"].clone();
        if text(&item["Response"], "NextFlowItemType") == "OTPSecurity" {
            return Err(CoreError::Authentication {
                message: tr("easybank: Die SMS-TAN wurde nicht akzeptiert.").into(),
            });
        }
        if item["Response"]["NextFlowItemType"].is_null() {
            item["Response"]["NextFlowItemType"] = json!("SuccessfulAuthentication");
        }
        self.after_step(item).await
    }

    async fn fetch(&mut self, ctx: &ConnectContext) -> CoreResult<FetchResult> {
        if !self.logged_in {
            return Err(CoreError::SessionClosed);
        }
        // "Kontoauszüge abrufen" for one card: statements only, no bookings.
        if let Documents::StatementsOnly(card) = &ctx.documents {
            let mut known = stored_state(&ctx.state).statements;
            let documents = self
                .statements(std::slice::from_ref(card), &mut known, true)
                .await?;
            return Ok(FetchResult {
                documents,
                state: self.state(known),
                ..Default::default()
            });
        }
        // Opening the transactions flow sets the server-side context the
        // booking calls run in and delivers the card list as an initial call.
        let flow = self
            .call("services/flow/AccountTransactionHistory", &json!({}))
            .await?;
        let mut accounts = initial_account_list(&flow);
        if accounts.is_empty() {
            let list = self
                .call("call/directive/CustomAccount/GetAccountList", &json!({}))
                .await?;
            accounts = list["AccountList"].as_array().cloned().unwrap_or_default();
        }
        let mut cards = Vec::new();
        for account in accounts
            .iter()
            .filter(|a| !text(a, "Number").is_empty() && text(a, "AccountStatus") != "Closed")
        {
            let external = format!("card:{}", text(account, "Number"));
            let days = days_since(ctx.since_for(&external));
            let transactions = match self.bookings(account, days).await? {
                Some(t) => t,
                None => self
                    .bookings(account, NO_TAN_DAYS)
                    .await?
                    .unwrap_or_default(),
            };
            cards.push(json!({ "account": account, "transactions": transactions }));
        }
        let mut out = snapshot(&json!({ "version": 1, "cards": cards }).to_string())?;
        let due = ctx.documents.due_statements();
        let mut known = stored_state(&ctx.state).statements;
        if !due.is_empty() {
            // Optional: a failed download never fails the sync. On failure the
            // dates of what was downloaded so far are dropped with the documents.
            let mut fetched = known.clone();
            if let Ok(docs) = self.statements(due, &mut fetched, false).await {
                out.documents = docs;
                out.statements_fetched = due.to_vec();
                known = fetched;
            }
        }
        out.state = self.state(known);
        Ok(out)
    }
}

/// Connection state saved between syncs (inside the encrypted vault).
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct StoredState {
    cookies: Vec<SecretString>,
    /// Statement dates already downloaded per card (`card:<n>` → `[YYYY-MM-DD]`).
    statements: HashMap<String, Vec<String>>,
}

fn stored_state(state: &SecretJson) -> StoredState {
    state.decode().unwrap_or_default()
}

impl EasybankConnector {
    fn state(&self, statements: HashMap<String, Vec<String>>) -> Option<SecretJson> {
        let header = SecretString::from(self.cookie_header());
        let cookies = persistent_cookies(header.expose());
        SecretJson::encode(&StoredState {
            cookies: cookies.into_iter().map(SecretString::from).collect(),
            statements,
        })
        .ok()
    }

    /// Monthly statements (PDF) of the given cards. Opens the statements
    /// flow (cards + first page), pages through the list and downloads every
    /// statement not in `known` (all of them when `all`).
    async fn statements(
        &mut self,
        cards: &[String],
        known: &mut HashMap<String, Vec<String>>,
        all: bool,
    ) -> CoreResult<Vec<crate::documents::RemoteDocument>> {
        let flow = self
            .call("services/flow/GetStatementsTransaction", &json!({}))
            .await?;
        let first_page = flow["InitialCallResponses"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|c| text(&c["Config"], "Url").ends_with("GetAccountStatementList"))
            .map(|c| c["Response"]["Item"].clone());
        let accounts = initial_account_list(&flow);
        let mut docs = Vec::new();
        let mut first_error: Option<CoreError> = None;
        for account in &accounts {
            let number = text(account, "Number");
            let card = format!("card:{number}");
            if number.is_empty() || !cards.contains(&card) {
                continue;
            }
            let mut page_item = match (&first_page, accounts.len()) {
                (Some(p), 1) => p.clone(),
                _ => {
                    self.call(
                        "call/flow/GetStatementsTransaction/GetAccountStatementList",
                        &json!({ "Account": account, "PageIndex": 0 }),
                    )
                    .await?
                }
            };
            let have = known.entry(card.clone()).or_default();
            for page in 1..=MAX_STATEMENT_PAGES {
                for (date, st) in pending_statements(&page_item, have, all) {
                    // One broken PDF must not cost the other statements (or
                    // the transactions already fetched); it is retried on the
                    // next sync because its date stays unknown.
                    let bytes = match self.download_statement(account, &st, &date).await {
                        Ok(b) => b,
                        Err(CoreError::SessionClosed) => return Err(CoreError::SessionClosed),
                        Err(e) => {
                            first_error.get_or_insert(e);
                            continue;
                        }
                    };
                    docs.push(statement_document(&card, number, &date, bytes));
                    if !have.contains(&date) {
                        have.push(date);
                    }
                }
                if !statements_has_more(&page_item) {
                    break;
                }
                page_item = self
                    .call(
                        "call/flow/GetStatementsTransaction/GetAccountStatementList",
                        &json!({ "Account": account, "PageIndex": page }),
                    )
                    .await?;
            }
            have.sort();
        }
        match first_error {
            // nothing came through: tell the caller why
            Some(e) if docs.is_empty() => Err(e),
            _ => Ok(docs),
        }
    }

    /// The web app posts a classic form (it triggers a browser download);
    /// the answer is the PDF itself.
    async fn download_statement(
        &mut self,
        account: &Value,
        statement: &Value,
        date: &str,
    ) -> CoreResult<Vec<u8>> {
        self.post("services/export/CheckAuthorization", &json!({}))
            .await?;
        let file_name = format!("Kontoauszug_{date}.pdf");
        let options = json!({
            "exportRequest": { "Account": account, "StatementDocument": statement },
            "isLandscape": false,
            "fileName": file_name,
            "uniqueID": crate::dates::now_ms(),
        });
        let client = self.client()?;
        let resp = client
            .post(format!(
                "{BASE}/call/flow/GetStatementsTransaction/DownloadStatement"
            ))
            .header("Origin", BASE)
            .header("Referer", format!("{BASE}/getstatementstransaction/start"))
            .form(&[
                ("fileName", file_name.as_str()),
                ("receivers", ""),
                ("options", &options.to_string()),
                ("UniqueKey", self.unique_key.as_str()),
                ("pcid", ""),
            ])
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(CoreError::Protocol {
                message: format!(
                    "easybank: {} HTTP {}",
                    tr("Der Kontoauszug konnte nicht geladen werden."),
                    resp.status().as_u16()
                ),
            });
        }
        // size-limited while streaming, not after the whole body is in memory
        let bytes = super::read_body_limited(resp, MAX_PDF_BYTES).await?;
        if !bytes.starts_with(b"%PDF-") {
            return Err(CoreError::Protocol {
                message: tr("easybank: Der Kontoauszug ist kein PDF.").into(),
            });
        }
        Ok(bytes)
    }
}

/// Statements of one list page that still have to be downloaded: those with
/// a date that are not in `have` (all of them when `all`).
fn pending_statements(page: &Value, have: &[String], all: bool) -> Vec<(String, Value)> {
    page["Statements"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|st| date(st, "StatementDate").map(|d| (d, st.clone())))
        .filter(|(d, _)| all || !have.contains(d))
        .collect()
}

fn statements_has_more(page: &Value) -> bool {
    page["HasMore"].as_bool() == Some(true)
}

fn statement_document(
    card: &str,
    number: &str,
    date: &str,
    bytes: Vec<u8>,
) -> crate::documents::RemoteDocument {
    crate::documents::RemoteDocument {
        account_ref: card.into(),
        booking_external_id: None,
        title: crate::i18n::trf("Kontoauszug {0}", &[&date]),
        doc_type: "kontoauszug".into(),
        mime_type: "application/pdf".into(),
        file_name: format!("Kontoauszug_easybank_{date}.pdf"),
        // Stable across sessions (the portal's DocumentID is opaque and may change).
        source_url: format!("easybank:statement:{number}:{date}"),
        bytes,
        date: date.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn eur(v: f64) -> Value {
        json!({"Value": v, "Currency": {"ID": 0, "Code": "EUR"}})
    }

    /// Invented data only (Max Mustermann, public example IBAN).
    fn fixture() -> Value {
        json!({"version": 1, "cards": [{
            "account": {"Number": "1234567890", "IBAN": "DE89370400440532013000",
                "AccountName": "easybank Kreditkarte", "ProductName": "easybank Kreditkarte",
                "CurrentBalance": eur(-123.45)},
            "transactions": [
                {"InternalID": "11111111-1111-4111-8111-111111111111", "PostingDate": "2026-11-04T00:00:00+00:00",
                 "TransactionDate": "2026-11-03T21:27:55+00:00", "ValueDate": "2026-11-03T21:27:55+00:00",
                 "BookingDate": "0001-01-01T00:00:00+00:00", "TransactionNature": "Debit",
                 "Amount": eur(19.990000000000002), "LocalCurrencyAmount": eur(19.990000000000002),
                 "Description": "Beispiel Shop", "MerchantName": "PAYPAL *BEISPIEL SHOP 0123 DE",
                 "TransactionCode": "6107", "TransactionCodeDescription": "", "IsPending": false},
                {"InternalID": "22222222-2222-4222-8222-222222222222", "PostingDate": "2026-11-03T00:00:00+00:00",
                 "TransactionDate": "2026-11-03T00:00:00+00:00", "TransactionNature": "Credit",
                 "Amount": {"Value": 0.0, "Currency": {"Code": ""}}, "LocalCurrencyAmount": eur(100.0),
                 "Description": "Gutschrift Lastschrift", "MerchantName": "Gutschrift Lastschrift",
                 "TransactionCode": "6550", "IsPending": false},
                {"InternalID": "33333333-3333-4333-8333-333333333333", "PostingDate": "2026-11-02T00:00:00+00:00",
                 "TransactionDate": "2026-11-01T12:00:00+00:00", "TransactionNature": "Debit",
                 "Amount": {"Value": 25.0, "Currency": {"Code": "USD"}}, "LocalCurrencyAmount": eur(23.17),
                 "Description": "Example Store", "MerchantName": "Example Store", "IsPending": false},
                {"InternalID": "44444444-4444-4444-8444-444444444444", "PostingDate": "0001-01-01T00:00:00+00:00",
                 "TransactionDate": "2026-11-12T08:00:00+00:00", "TransactionNature": "Debit",
                 "Amount": eur(5.0), "LocalCurrencyAmount": eur(5.0), "Description": "Café", "IsPending": true}
            ]
        }]})
    }

    #[test]
    fn maps_card_signs_dates_and_foreign_currency() {
        let r = snapshot(&fixture().to_string()).unwrap();
        assert_eq!(r.accounts.len(), 1);
        let a = &r.accounts[0];
        assert_eq!(
            (a.external_ref.as_str(), a.kind, a.balance_cents),
            ("card:1234567890", AccountKind::CreditCard, -12345)
        );
        assert_eq!(r.transactions.len(), 3, "pending booking is skipped");
        let t = &r.transactions;
        assert_eq!(t[0].amount_cents, -1999);
        assert_eq!(
            (t[0].booking_date.as_str(), t[0].value_date.as_str()),
            ("2026-11-04", "2026-11-03")
        );
        assert_eq!(t[0].counterparty, "Beispiel Shop");
        assert_eq!(t[0].purpose, "PAYPAL *BEISPIEL SHOP 0123 DE");
        assert_eq!(t[0].booking_text, "6107");
        assert!(
            t.iter().all(|t| t.external_id.is_empty()),
            "content ids, not InternalID"
        );
        assert_eq!((t[1].amount_cents, t[1].currency.as_str()), (10000, "EUR"));
        assert_eq!(t[1].purpose, "");
        assert_eq!(t[2].amount_cents, -2317);
        assert_eq!(t[2].purpose, "USD 25.00");
    }

    #[test]
    fn rejects_incomplete_snapshots() {
        for bad in [
            json!({"version": 2, "cards": []}),
            json!({"version": 1, "cards": []}),
            json!({"version": 1, "cards": [{"account": {"CurrentBalance": eur(0.0)}, "transactions": []}]}),
        ] {
            assert!(snapshot(&bad.to_string()).is_err(), "{bad}");
        }
        let mut f = fixture();
        f["cards"][0]["transactions"][0]["TransactionNature"] = json!("Unknown");
        assert!(snapshot(&f.to_string()).is_err());
    }

    #[test]
    fn restore_cookies_rejects_attribute_and_header_injection() {
        let c = EasybankConnector::new();
        let cookies = [
            "good=1",
            "evil=1; Domain=attacker.example",
            "crlf=1\r\nX-Injected: 1",
            "=novalue",
            "noequals",
        ];
        c.restore_cookies(&cookies.map(SecretString::from));
        let header = c.cookie_header();
        assert!(header.contains("good=1"), "{header}");
        for bad in ["evil", "crlf", "Injected", "noequals", "novalue"] {
            assert!(!header.contains(bad), "{bad} in {header}");
        }
    }

    #[test]
    fn statement_pages_skip_known_and_undated_unless_all() {
        let page = json!({"HasMore": true, "Statements": [
            {"StatementDate": "2026-10-10T00:00:00+00:00", "DocumentID": "a"},
            {"StatementDate": "2026-09-10T00:00:00+00:00", "DocumentID": "b"},
            {"DocumentID": "no-date"},
        ]});
        let have = vec!["2026-10-10".to_string()];
        let new: Vec<_> = pending_statements(&page, &have, false)
            .into_iter()
            .map(|(d, _)| d)
            .collect();
        assert_eq!(new, ["2026-09-10"]);
        assert_eq!(pending_statements(&page, &have, true).len(), 2);
        assert!(statements_has_more(&page));
        assert!(!statements_has_more(&json!({"HasMore": false})));
        assert!(!statements_has_more(&json!({})));
        assert!(pending_statements(&json!({}), &[], true).is_empty());
    }

    #[test]
    fn reads_unique_key_and_keeps_only_persistent_cookies() {
        let html = "<script>VeriBranch.Config.PCID = '';\n VeriBranch.Config.UniqueKey = 'abc-123';</script>";
        assert_eq!(page_unique_key(html).as_deref(), Some("abc-123"));
        assert_eq!(page_unique_key("<html></html>"), None);
        assert_eq!(percent_decode("a%2Bb%3D+c%"), "a+b=+c%");
        assert_eq!(
            persistent_cookies(
                "ASP.NET_SessionId=x; XSRF-TOKEN=y; ai_user=z; Tracker=keep; ARRAffinity=a"
            ),
            vec!["Tracker=keep".to_string()]
        );
    }

    #[test]
    fn new_internal_ids_on_next_login_do_not_duplicate() {
        use crate::models::ConnectorKind;
        let dir = crate::test_support::TempDir::new("easybank");
        let vault =
            crate::vault::Vault::open(dir.join("vault.db").to_string_lossy().into(), vec![7; 32])
                .unwrap();
        let card = vault
            .add_bank_connection(ConnectorKind::Easybank, "Karte".into(), "max".into())
            .unwrap();
        let first = fixture();
        let mut second = fixture();
        for (i, t) in second["cards"][0]["transactions"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .enumerate()
        {
            t["InternalID"] = json!(format!("99999999-0000-4000-8000-00000000000{i}"));
        }
        let store = |v: &Value| {
            super::super::store_fetch(
                &vault,
                ConnectorKind::Easybank,
                card,
                snapshot(&v.to_string()).unwrap(),
            )
            .unwrap()
            .transactions_imported
        };
        assert_eq!(store(&first), 3);
        assert_eq!(store(&second), 0, "same bookings, new ids → no duplicates");
    }

    #[test]
    fn statements_have_stable_source_and_remembered_dates() {
        let d = statement_document(
            "card:1234567890",
            "1234567890",
            "2026-11-10",
            b"%PDF-1.4".to_vec(),
        );
        assert_eq!(d.source_url, "easybank:statement:1234567890:2026-11-10");
        assert_eq!(
            (d.doc_type.as_str(), d.date.as_str()),
            ("kontoauszug", "2026-11-10")
        );
        assert_eq!(d.account_ref, "card:1234567890");
        let state = json!({"cookies": ["c=1"], "statements": {"card:1234567890": ["2026-10-10"]}});
        let stored = stored_state(&state.clone().into());
        assert_eq!(stored.statements["card:1234567890"], ["2026-10-10"]);
        assert_eq!(serde_json::to_value(&stored).unwrap(), state);
        assert!(stored_state(&SecretJson::default()).statements.is_empty());
    }

    #[test]
    fn finds_cards_in_initial_calls() {
        let flow = json!({"InitialCallResponses": [
            {"Config": {"Url": "vbm/execute/GetOpenCycleInfoTransaction"}, "Response": {"Item": {}}},
            {"Config": {"Url": "CustomAccount/GetAccountList"},
             "Response": {"Item": {"AccountList": [{"Number": "1234567890"}]}}}
        ]});
        assert_eq!(initial_account_list(&flow).len(), 1);
        assert!(initial_account_list(&json!({})).is_empty());
    }

    #[test]
    fn query_has_paging_and_range() {
        let q = transactions_query(&json!({"Number": "1"}), 30, 2);
        assert_eq!(
            (q["PageIndex"].as_u64(), q["Skip"].as_u64()),
            (Some(2), Some(100))
        );
        assert_eq!(q["TransactionTypeIndicator"], json!(0));
        assert!(q["StartDate"].as_str().unwrap() < q["EndDate"].as_str().unwrap());
        assert_eq!(days_since("1999-01-01"), MAX_DAYS);
        assert_eq!(days_since(&super::super::today()), 1);
    }
}
