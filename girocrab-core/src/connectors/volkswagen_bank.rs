//! Volkswagen Bank online banking (`banking.volkswagenbank.de`) with pushTAN.
//! No PSD2 access is wired in.
//!
//! The portal is a single-page app on top of a JSON API at
//! `services.banking.volkswagenbank.de`. The flow below follows a browser
//! network capture of two real pushTAN sign-ins:
//!
//! 1. `POST AuthenticationService/authentication/check` with user name and
//!    password. `SecondFactorAuthenticationRequired` comes with the offered
//!    second factors in the `X-CHALLENGES` header (`PhotoTan`, `PushTan`).
//! 2. `POST TransactionAuthorizationService/pushtan/transaction/authorize`
//!    starts the pushTAN, `…/checkstate` is polled until `TransactionConfirmed`.
//! 3. `POST AuthenticationService/authentication/execute` (header
//!    `X-REQUEST-KEY`) finishes the sign-in.
//! 4. `GET AccountListService/accountlist`, then per account
//!    `POST CashFlowService/cashflow/list/get` for the bookings.
//!
//! Every response carries a fresh `Authorization: bearer …` header; the newest
//! one is sent with the next request. The browser capture hides request
//! headers with credentials, so sending that token back as `Authorization` is
//! an assumption until a live test confirms it.
//!
//! photoTAN is part of the dialogue (`LoginStep::PhotoTan`) but not wired to
//! the portal: the capture only holds pushTAN sign-ins, and the photoTAN
//! graphic is built from `ImageCreationChallenge` by the portal's JavaScript.
//! When pushTAN is offered it is always chosen.
//!
//! Rules that carry over from the other portal connectors: money is `i64`
//! cents, debits are negative, secrets are never logged, and bookings without
//! a stable bank id leave `external_id` empty so `store_fetch` derives a
//! content id.
use async_trait::async_trait;
use jiff::civil::{Date, DateTime, Weekday};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, ORIGIN, REFERER};
use serde_json::{Value, json};

use super::{
    BankConnector, Challenge, ChallengeKind, ChallengeResponse, ConnectContext, FetchResult, Phase,
    RemoteAccount, RemoteTransaction, http_client_same_host,
};
use crate::{
    error::{CoreError, CoreResult},
    i18n::{tr, trf},
    models::AccountKind,
    secret::SecretJson,
};

const BASE: &str = "https://banking.volkswagenbank.de";
const API: &str = "https://services.banking.volkswagenbank.de";

/// What the portal answered to a sign-in step.
#[derive(Debug)]
pub(crate) enum LoginStep {
    /// Signed in.
    Authenticated,
    /// The portal wants a photoTAN: scan the image with the photoTAN app and
    /// type the code the app shows. Not produced by [`HttpPortal`] yet.
    #[allow(dead_code)]
    PhotoTan { image: Vec<u8> },
    /// The portal sent a pushTAN: approve it in the banking app.
    PushTan,
    /// pushTAN not approved yet.
    PushPending,
}

/// The bank side of the connector. Everything that needs the portal's
/// endpoints lives behind this trait so the dialogue logic stays testable.
#[async_trait]
pub(crate) trait Portal: Send {
    /// User name + password → first sign-in step. `state` is the connection
    /// state saved by the last sync (unused: the portal asks for a second
    /// factor on every sign-in).
    async fn start_login(
        &mut self,
        user: &str,
        password: &str,
        state: &SecretJson,
    ) -> CoreResult<LoginStep>;
    /// Ask whether the pushTAN was approved.
    async fn poll_push(&mut self) -> CoreResult<LoginStep>;
    /// Send the code typed from the photoTAN app.
    async fn submit_photo_tan(&mut self, tan: &str) -> CoreResult<LoginStep>;
    /// Accounts and bookings since `ctx.since` / `ctx.since_by_account`.
    async fn load(&mut self, ctx: &ConnectContext) -> CoreResult<FetchResult>;
    /// Sign out. Best effort.
    async fn logout(&mut self) {}
}

fn bad_response() -> CoreError {
    CoreError::Protocol {
        message: tr("Volkswagen Bank: Unvollständige oder ungültige Portalantwort. Es wurden keine Daten übernommen.").into(),
    }
}

fn rejected(status: &str) -> CoreError {
    CoreError::Authentication {
        message: trf(
            "Volkswagen Bank: Anmeldung abgelehnt (Status: {0}).",
            &[&status],
        ),
    }
}

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Talks to `services.banking.volkswagenbank.de`.
#[derive(Default)]
struct HttpPortal {
    client: Option<reqwest::Client>,
    /// Newest `Authorization` header the portal sent.
    token: Option<String>,
    /// `X-REQUEST-KEY` of the running sign-in, sent with `execute`.
    request_key: Option<String>,
    /// `PushTan.AuthorizationChallengeKey` of the running pushTAN.
    push_key: Option<String>,
}

impl HttpPortal {
    fn client(&mut self) -> CoreResult<reqwest::Client> {
        if self.client.is_none() {
            self.client = Some(http_client_same_host()?);
        }
        self.client.clone().ok_or_else(bad_response)
    }

    /// One call to the services API. `body: None` on a POST sends an empty
    /// body. The newest `Authorization` response header is kept for the next
    /// call.
    async fn call(
        &mut self,
        post: bool,
        path: &str,
        body: Option<Value>,
        request_key: Option<&str>,
    ) -> CoreResult<(Value, HeaderMap)> {
        let client = self.client()?;
        let url = format!("{API}/{path}");
        let mut req = if post {
            client.post(url)
        } else {
            client.get(url)
        };
        req = req
            .header(ACCEPT, "application/json")
            .header(ORIGIN, BASE)
            .header(REFERER, format!("{BASE}/"))
            .header("X-CLIENT-INFO", r#"{"ClientType":"Browser"}"#);
        if let Some(token) = &self.token {
            req = req.header(AUTHORIZATION, token.as_str());
        }
        if let Some(key) = request_key {
            req = req.header("X-REQUEST-KEY", key);
        }
        req = match body {
            Some(b) => req.json(&b),
            None if post => req.header(CONTENT_TYPE, "application/json").body(""),
            None => req.header(CONTENT_TYPE, "application/json"),
        };
        let resp = req.send().await?;
        let status = resp.status();
        let headers = resp.headers().clone();
        if let Some(token) = headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .filter(|t| !t.is_empty())
        {
            self.token = Some(token.to_string());
        }
        if status.as_u16() == 401 || status.as_u16() == 403 {
            return Err(CoreError::Authentication {
                message: trf(
                    "Volkswagen Bank: Zugriff abgelehnt (HTTP {0}).",
                    &[&status.as_u16()],
                ),
            });
        }
        if !status.is_success() {
            return Err(CoreError::Protocol {
                message: trf(
                    "Volkswagen Bank: Unerwartete Antwort (HTTP {0}).",
                    &[&status.as_u16()],
                ),
            });
        }
        let raw = super::read_text_limited(resp).await?;
        let value: Value = serde_json::from_str(&raw).map_err(|_| bad_response())?;
        Ok((value, headers))
    }

    /// Finishes the sign-in after the second factor was confirmed.
    async fn execute(&mut self) -> CoreResult<LoginStep> {
        let key = self.request_key.clone().unwrap_or_default();
        let (body, _) = self
            .call(
                true,
                "AuthenticationService/authentication/execute",
                None,
                Some(&key),
            )
            .await?;
        match text(&body, "Status") {
            "Success" => Ok(LoginStep::Authenticated),
            other => Err(rejected(other)),
        }
    }
}

#[async_trait]
impl Portal for HttpPortal {
    async fn start_login(
        &mut self,
        user: &str,
        password: &str,
        _state: &SecretJson,
    ) -> CoreResult<LoginStep> {
        self.token = None;
        self.request_key = None;
        self.push_key = None;
        let (body, headers) = self
            .call(
                true,
                "AuthenticationService/authentication/check",
                Some(json!({ "UserName": user, "Password": password })),
                None,
            )
            .await?;
        let status = text(&body, "Status");
        if status != "SecondFactorAuthenticationRequired" {
            return Err(rejected(status));
        }
        let challenges: Value = headers
            .get("X-CHALLENGES")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(Value::Null);
        let Some(key) = challenges
            .pointer("/PushTan/AuthorizationChallengeKey")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            return Err(CoreError::Protocol {
                message: tr("Volkswagen Bank: photoTAN wird noch nicht unterstützt. Bitte pushTAN in der Volkswagen-Bank-App einrichten.").into(),
            });
        };
        self.request_key = headers
            .get("X-REQUEST-KEY")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
            .or_else(|| Some(key.clone()));
        self.push_key = Some(key.clone());
        let (resp, _) = self
            .call(
                true,
                "TransactionAuthorizationService/pushtan/transaction/authorize",
                Some(json!({ "RequestKey": key, "WaitForUserInteraction": false })),
                None,
            )
            .await?;
        match text(&resp, "Status") {
            "AuthorizationPending" => Ok(LoginStep::PushTan),
            "TransactionConfirmed" => self.execute().await,
            other => Err(rejected(other)),
        }
    }

    async fn poll_push(&mut self) -> CoreResult<LoginStep> {
        let Some(key) = self.push_key.clone() else {
            return Err(CoreError::SessionClosed);
        };
        let (resp, _) = self
            .call(
                true,
                "TransactionAuthorizationService/pushtan/transaction/checkstate",
                Some(json!({ "RequestKey": key })),
                None,
            )
            .await?;
        match text(&resp, "Status") {
            "AuthorizationPending" => Ok(LoginStep::PushPending),
            "TransactionConfirmed" => self.execute().await,
            other => Err(CoreError::Authentication {
                message: trf(
                    "Volkswagen Bank: Die pushTAN wurde abgelehnt oder ist abgelaufen (Status: {0}).",
                    &[&other],
                ),
            }),
        }
    }

    async fn submit_photo_tan(&mut self, _tan: &str) -> CoreResult<LoginStep> {
        Err(CoreError::Protocol {
            message: tr("Volkswagen Bank: photoTAN wird noch nicht unterstützt. Bitte pushTAN in der Volkswagen-Bank-App einrichten.").into(),
        })
    }

    async fn load(&mut self, ctx: &ConnectContext) -> CoreResult<FetchResult> {
        let today = berlin_today();
        let (list, _) = self
            .call(false, "AccountListService/accountlist", None, None)
            .await?;
        let accounts = parse_accounts(&list, today)?;
        let mut out = FetchResult::default();
        for account in accounts {
            if account.cash_flow {
                let from = ctx.since_for(&account.remote.external_ref);
                let (flow, _) = self
                    .call(
                        true,
                        "CashFlowService/cashflow/list/get",
                        Some(json!({
                            "accountNumber": account.remote.account_number,
                            "dateFrom": format!("{from}Z"),
                            "dateTo": format!("{today}Z"),
                        })),
                        None,
                    )
                    .await?;
                out.transactions
                    .extend(parse_cashflow(&flow, &account.remote.external_ref)?);
            }
            out.accounts.push(account.remote);
        }
        Ok(out)
    }
}

struct PortalAccount {
    remote: RemoteAccount,
    /// The portal offers a booking list for this account.
    cash_flow: bool,
}

fn account_kind(category: &str) -> AccountKind {
    let c = category.to_ascii_lowercase();
    if c.contains("creditcard") || c.contains("cardaccount") {
        AccountKind::CreditCard
    } else if c.contains("current") || c.contains("giro") || c.contains("checking") {
        AccountKind::Checking
    } else if c.contains("loan") || c.contains("installment") || c.contains("credit") {
        AccountKind::Loan
    } else {
        // CallMoneyAccount (Tagesgeld), SavingAccount (Sparbrief), …
        AccountKind::Savings
    }
}

/// `AccountListService/accountlist` → accounts. Any missing field fails the
/// whole call so a changed portal never imports half a picture.
fn parse_accounts(v: &Value, today: Date) -> CoreResult<Vec<PortalAccount>> {
    let list = v
        .get("Accounts")
        .and_then(Value::as_array)
        .ok_or_else(bad_response)?;
    let mut out = Vec::new();
    for a in list {
        let number = text(a, "AccountNumber").trim();
        let iban = text(a, "Iban").trim();
        if number.is_empty() {
            return Err(bad_response());
        }
        let balance_cents = parse_cents(text(a, "Balance")).ok_or_else(bad_response)?;
        let owner = a
            .get("Owners")
            .and_then(Value::as_array)
            .and_then(|o| o.first())
            .map(|o| {
                format!("{} {}", text(o, "FirstName"), text(o, "LastName"))
                    .trim()
                    .to_string()
            })
            .unwrap_or_default();
        let name = match text(a, "ProductName").trim() {
            "" => text(a, "Description").trim(),
            n => n,
        };
        let currency = match text(a, "Currency").trim() {
            "" => "EUR",
            c => c,
        };
        out.push(PortalAccount {
            remote: RemoteAccount {
                external_ref: if iban.is_empty() { number } else { iban }.to_string(),
                name: name.to_string(),
                owner,
                iban: iban.to_string(),
                account_number: number.to_string(),
                currency: currency.to_string(),
                kind: account_kind(text(a, "Category")),
                balance_cents,
                balance_date: portal_date(text(a, "DateOfAccountBalance"))
                    .unwrap_or(today)
                    .to_string(),
            },
            cash_flow: a
                .pointer("/Features/CashFlow")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        });
    }
    Ok(out)
}

/// `CashFlowService/cashflow/list/get` → bookings of one account.
fn parse_cashflow(v: &Value, account_ref: &str) -> CoreResult<Vec<RemoteTransaction>> {
    if text(v, "Status") != "Success" {
        return Err(bad_response());
    }
    let list = v
        .get("CashflowList")
        .and_then(Value::as_array)
        .ok_or_else(bad_response)?;
    let mut out = Vec::new();
    for it in list {
        let posting = portal_date(text(it, "PostingDate")).ok_or_else(bad_response)?;
        let value = portal_date(text(it, "ValueDate")).unwrap_or(posting);
        let credit = amount_or_zero(text(it, "CreditAmount")).ok_or_else(bad_response)?;
        let debit = amount_or_zero(text(it, "DebitAmount")).ok_or_else(bad_response)?;
        let reference = it.get("ReferenceAccount").unwrap_or(&Value::Null);
        let purpose = it
            .get("PaymentNotes")
            .and_then(Value::as_array)
            .map(|notes| {
                notes
                    .iter()
                    .map(|n| text(n, "Text").trim())
                    .filter(|t| !t.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        out.push(RemoteTransaction {
            account_ref: account_ref.to_string(),
            // `DocumentNumber` can repeat within one document; the content id
            // from `store_fetch` is the safer key.
            external_id: String::new(),
            booking_date: posting.to_string(),
            value_date: value.to_string(),
            amount_cents: credit - debit,
            currency: match text(it, "Currency").trim() {
                "" => "EUR".to_string(),
                c => c.to_string(),
            },
            counterparty: text(reference, "AccountHolder").trim().to_string(),
            counterparty_iban: text(reference, "Iban").trim().to_string(),
            purpose,
            booking_text: text(it, "TransactionTypeDescription").trim().to_string(),
            mandate_id: text(it, "MandateId").trim().to_string(),
            creditor_id: text(it, "UniqueCreditorId").trim().to_string(),
            ..Default::default()
        });
    }
    Ok(out)
}

/// `"71.01"` → 7101. Plain decimal with a dot, optional minus sign.
fn parse_cents(s: &str) -> Option<i64> {
    let s = s.trim();
    let (negative, rest) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s),
    };
    let (int, frac) = rest.split_once('.').unwrap_or((rest, ""));
    if int.is_empty()
        || frac.len() > 2
        || !int.bytes().all(|b| b.is_ascii_digit())
        || !frac.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let int: i64 = int.parse().ok()?;
    let frac_cents: i64 = match frac.len() {
        0 => 0,
        1 => frac.parse::<i64>().ok()? * 10,
        _ => frac.parse::<i64>().ok()?,
    };
    let cents = int.checked_mul(100)?.checked_add(frac_cents)?;
    Some(if negative { -cents } else { cents })
}

/// The booking list leaves the unused side of credit/debit empty.
fn amount_or_zero(s: &str) -> Option<i64> {
    if s.trim().is_empty() {
        Some(0)
    } else {
        parse_cents(s)
    }
}

fn last_sunday(year: i32, month: u32) -> Option<Date> {
    crate::dates::ymd(year, month, 1)?
        .nth_weekday_of_month(-1, Weekday::Sunday)
        .ok()
}

/// Hours Berlin is ahead of UTC: +2 from the last Sunday of March 01:00 UTC to
/// the last Sunday of October 01:00 UTC, otherwise +1.
fn berlin_offset_hours(utc: DateTime) -> i64 {
    let start = last_sunday(utc.year().into(), 3).map(|d| d.at(1, 0, 0, 0));
    let end = last_sunday(utc.year().into(), 10).map(|d| d.at(1, 0, 0, 0));
    match (start, end) {
        (Some(s), Some(e)) if utc >= s && utc < e => 2,
        _ => 1,
    }
}

/// Portal timestamps are UTC (`2026-10-22T22:00:00.0000000Z`) but mark a
/// Berlin calendar day (that one is 23 October), so convert before cutting
/// off the time.
fn portal_date(s: &str) -> Option<Date> {
    let utc = jiff::civil::DateTime::strptime("%Y-%m-%dT%H:%M:%S", s.get(..19)?).ok()?;
    // Portals write 9999-12-31T23:00:00Z for "no date": checked, it is past jiff's range.
    let berlin = utc.checked_add(jiff::Span::new().hours(berlin_offset_hours(utc)));
    Some(berlin.ok()?.date())
}

fn berlin_today() -> Date {
    let now = crate::dates::now_utc();
    (now + jiff::Span::new().hours(berlin_offset_hours(now))).date()
}

fn photo_tan_challenge(image: Vec<u8>) -> Challenge {
    Challenge {
        kind: ChallengeKind::ImageCode,
        title: "Volkswagen Bank photoTAN".into(),
        message: tr(
            "Bitte die Grafik mit der photoTAN-App scannen und den angezeigten Code eingeben.",
        )
        .into(),
        method_name: "photoTAN".into(),
        redirect_url: None,
        image: Some(image),
        expires_in_seconds: None,
        poll_interval_seconds: 0,
    }
}

fn push_tan_challenge() -> Challenge {
    Challenge {
        kind: ChallengeKind::DecoupledPush,
        title: "Volkswagen Bank pushTAN".into(),
        message: tr("Bitte den Auftrag in der Volkswagen-Bank-App per pushTAN freigeben.").into(),
        method_name: "pushTAN".into(),
        redirect_url: None,
        image: None,
        expires_in_seconds: None,
        poll_interval_seconds: 3,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Idle,
    AwaitingPhotoTan,
    AwaitingPush,
    Ready,
}

pub struct VolkswagenBankConnector {
    portal: Box<dyn Portal>,
    stage: Stage,
}

impl VolkswagenBankConnector {
    pub fn new() -> Self {
        Self::with_portal(Box::<HttpPortal>::default())
    }

    fn with_portal(portal: Box<dyn Portal>) -> Self {
        Self {
            portal,
            stage: Stage::Idle,
        }
    }

    /// Moves the dialogue forward and tells the app what to show next.
    fn advance(&mut self, step: LoginStep) -> Phase {
        match step {
            LoginStep::Authenticated => {
                self.stage = Stage::Ready;
                Phase::Ready
            }
            LoginStep::PhotoTan { image } => {
                self.stage = Stage::AwaitingPhotoTan;
                Phase::Challenge(photo_tan_challenge(image))
            }
            LoginStep::PushTan => {
                self.stage = Stage::AwaitingPush;
                Phase::Challenge(push_tan_challenge())
            }
            LoginStep::PushPending => {
                self.stage = Stage::AwaitingPush;
                Phase::Pending(push_tan_challenge())
            }
        }
    }
}

impl Default for VolkswagenBankConnector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl BankConnector for VolkswagenBankConnector {
    async fn begin(&mut self, ctx: &ConnectContext) -> CoreResult<Phase> {
        if ctx.login_name.trim().is_empty() || ctx.secret.expose().is_empty() {
            return Err(CoreError::Authentication {
                message: tr("Benutzername und Passwort erforderlich").into(),
            });
        }
        self.stage = Stage::Idle;
        let step = self
            .portal
            .start_login(ctx.login_name.trim(), ctx.secret.expose(), &ctx.state)
            .await?;
        Ok(self.advance(step))
    }

    async fn respond(
        &mut self,
        _ctx: &ConnectContext,
        response: ChallengeResponse,
    ) -> CoreResult<Phase> {
        let step = match (self.stage, response) {
            (Stage::AwaitingPush, ChallengeResponse::Poll) => self.portal.poll_push().await?,
            (Stage::AwaitingPhotoTan, ChallengeResponse::Code(tan)) => {
                let tan = tan.expose().trim().to_string();
                if tan.is_empty() {
                    return Err(CoreError::InvalidInput {
                        message: tr("Bitte die photoTAN eingeben.").into(),
                    });
                }
                self.portal.submit_photo_tan(&tan).await?
            }
            _ => {
                return Err(CoreError::InvalidInput {
                    message: tr("Volkswagen Bank: Unerwartete Antwort auf die TAN-Abfrage.").into(),
                });
            }
        };
        Ok(self.advance(step))
    }

    async fn fetch(&mut self, ctx: &ConnectContext) -> CoreResult<FetchResult> {
        if self.stage != Stage::Ready {
            return Err(CoreError::SessionClosed);
        }
        self.portal.load(ctx).await
    }

    async fn finish(&mut self, _ctx: &ConnectContext) {
        self.portal.logout().await;
        self.stage = Stage::Idle;
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use crate::secret::SecretString;

    /// Answers each call with the next scripted step.
    struct Scripted {
        steps: VecDeque<LoginStep>,
        loaded: bool,
        logged_out: bool,
    }

    impl Scripted {
        fn new(steps: Vec<LoginStep>) -> Self {
            Self {
                steps: steps.into(),
                loaded: false,
                logged_out: false,
            }
        }
        fn next(&mut self) -> CoreResult<LoginStep> {
            self.steps.pop_front().ok_or(CoreError::SessionClosed)
        }
    }

    #[async_trait]
    impl Portal for Scripted {
        async fn start_login(
            &mut self,
            _user: &str,
            _password: &str,
            _state: &SecretJson,
        ) -> CoreResult<LoginStep> {
            self.next()
        }
        async fn poll_push(&mut self) -> CoreResult<LoginStep> {
            self.next()
        }
        async fn submit_photo_tan(&mut self, tan: &str) -> CoreResult<LoginStep> {
            if tan == "000000" {
                return Err(CoreError::Authentication {
                    message: "wrong tan".into(),
                });
            }
            self.next()
        }
        async fn load(&mut self, _ctx: &ConnectContext) -> CoreResult<FetchResult> {
            self.loaded = true;
            Ok(FetchResult::default())
        }
        async fn logout(&mut self) {
            self.logged_out = true;
        }
    }

    fn ctx(user: &str, password: &str) -> ConnectContext {
        ConnectContext {
            connection_id: 1,
            login_name: user.into(),
            secret: SecretString::new(password.into()),
            since: "2026-01-01".into(),
            ..Default::default()
        }
    }

    fn connector(steps: Vec<LoginStep>) -> VolkswagenBankConnector {
        VolkswagenBankConnector::with_portal(Box::new(Scripted::new(steps)))
    }

    #[tokio::test]
    async fn photo_tan_flow_shows_the_image_then_accepts_the_code() {
        let mut c = connector(vec![
            LoginStep::PhotoTan {
                image: vec![1, 2, 3],
            },
            LoginStep::Authenticated,
        ]);
        let ctx = ctx("demo", "geheim");
        let Phase::Challenge(ch) = c.begin(&ctx).await.unwrap() else {
            panic!("expected a challenge");
        };
        assert_eq!(ch.kind, ChallengeKind::ImageCode);
        assert_eq!(ch.image, Some(vec![1, 2, 3]));
        assert_eq!(ch.method_name, "photoTAN");
        let done = c
            .respond(
                &ctx,
                ChallengeResponse::Code(SecretString::new(" 123456 ".into())),
            )
            .await
            .unwrap();
        assert!(matches!(done, Phase::Ready));
        assert!(c.fetch(&ctx).await.is_ok());
    }

    #[tokio::test]
    async fn push_tan_flow_polls_until_approved() {
        let mut c = connector(vec![
            LoginStep::PushTan,
            LoginStep::PushPending,
            LoginStep::Authenticated,
        ]);
        let ctx = ctx("demo", "geheim");
        let Phase::Challenge(ch) = c.begin(&ctx).await.unwrap() else {
            panic!("expected a challenge");
        };
        assert_eq!(ch.kind, ChallengeKind::DecoupledPush);
        assert!(ch.image.is_none() && ch.poll_interval_seconds > 0);
        assert!(matches!(
            c.respond(&ctx, ChallengeResponse::Poll).await.unwrap(),
            Phase::Pending(_)
        ));
        assert!(matches!(
            c.respond(&ctx, ChallengeResponse::Poll).await.unwrap(),
            Phase::Ready
        ));
    }

    #[tokio::test]
    async fn trusted_device_skips_the_second_factor() {
        let mut c = connector(vec![LoginStep::Authenticated]);
        let ctx = ctx("demo", "geheim");
        assert!(matches!(c.begin(&ctx).await.unwrap(), Phase::Ready));
        assert!(c.fetch(&ctx).await.is_ok());
    }

    #[tokio::test]
    async fn rejects_missing_credentials_and_wrong_answers() {
        let mut c = connector(vec![]);
        for (u, p) in [("", "x"), ("  ", "x"), ("demo", "")] {
            assert!(matches!(
                c.begin(&ctx(u, p)).await,
                Err(CoreError::Authentication { .. })
            ));
        }
        let ctx = ctx("demo", "geheim");
        // answering before a challenge exists
        assert!(matches!(
            c.respond(&ctx, ChallengeResponse::Poll).await,
            Err(CoreError::InvalidInput { .. })
        ));
        // a photoTAN challenge cannot be polled; an empty or wrong code fails
        let mut c = connector(vec![LoginStep::PhotoTan { image: vec![0] }]);
        c.begin(&ctx).await.unwrap();
        assert!(matches!(
            c.respond(&ctx, ChallengeResponse::Poll).await,
            Err(CoreError::InvalidInput { .. })
        ));
        assert!(matches!(
            c.respond(
                &ctx,
                ChallengeResponse::Code(SecretString::new("  ".into()))
            )
            .await,
            Err(CoreError::InvalidInput { .. })
        ));
        assert!(matches!(
            c.respond(
                &ctx,
                ChallengeResponse::Code(SecretString::new("000000".into()))
            )
            .await,
            Err(CoreError::Authentication { .. })
        ));
    }

    #[tokio::test]
    async fn fetch_needs_a_finished_login_and_finish_signs_out() {
        let mut c = connector(vec![LoginStep::PushTan]);
        let ctx = ctx("demo", "geheim");
        assert!(matches!(c.fetch(&ctx).await, Err(CoreError::SessionClosed)));
        c.begin(&ctx).await.unwrap();
        assert!(matches!(c.fetch(&ctx).await, Err(CoreError::SessionClosed)));
        c.finish(&ctx).await;
        assert_eq!(c.stage, Stage::Idle);
    }

    #[test]
    fn decimal_strings_become_cents() {
        assert_eq!(parse_cents("71.01"), Some(7101));
        assert_eq!(parse_cents("80000.00"), Some(8_000_000));
        assert_eq!(parse_cents("5.6"), Some(560));
        assert_eq!(parse_cents("-0.31"), Some(-31));
        assert_eq!(parse_cents("7"), Some(700));
        for bad in ["", "-", "1,00", "1.005", "abc", ".5", "1.0.0"] {
            assert_eq!(parse_cents(bad), None, "{bad:?}");
        }
        assert_eq!(amount_or_zero(""), Some(0));
        assert_eq!(amount_or_zero(" "), Some(0));
        assert_eq!(amount_or_zero("5.66"), Some(566));
        assert_eq!(amount_or_zero("x"), None);
    }

    #[test]
    fn portal_timestamps_are_converted_to_the_berlin_day() {
        let day = |s: &str| portal_date(s).map(|d| d.to_string());
        // 22:00/23:00 UTC is already the next day in Berlin.
        assert_eq!(
            day("2026-10-22T22:00:00.0000000Z").as_deref(),
            Some("2026-10-23")
        );
        assert_eq!(
            day("2026-10-24T22:00:00.0000000Z").as_deref(),
            Some("2026-10-25")
        );
        assert_eq!(
            day("2026-02-10T23:00:00.0000000Z").as_deref(),
            Some("2026-02-11")
        );
        assert_eq!(
            day("2026-11-16T09:17:30.0000000Z").as_deref(),
            Some("2026-11-16")
        );
        // Daylight saving switches: 29 March 2026 and 25 October 2026, 01:00 UTC.
        assert_eq!(day("2026-03-28T23:30:00Z").as_deref(), Some("2026-03-29"));
        assert_eq!(day("2026-03-29T22:30:00Z").as_deref(), Some("2026-03-30"));
        assert_eq!(day("2026-10-25T00:30:00Z").as_deref(), Some("2026-10-25"));
        assert_eq!(day("2026-10-25T23:30:00Z").as_deref(), Some("2026-10-26"));
        assert_eq!(day("x"), None);
        assert_eq!(day(""), None);
    }

    #[test]
    fn account_categories_map_to_kinds() {
        assert!(matches!(
            account_kind("CallMoneyAccount"),
            AccountKind::Savings
        ));
        assert!(matches!(
            account_kind("SavingAccount"),
            AccountKind::Savings
        ));
        assert!(matches!(
            account_kind("CurrentAccount"),
            AccountKind::Checking
        ));
        assert!(matches!(
            account_kind("CreditCardAccount"),
            AccountKind::CreditCard
        ));
        assert!(matches!(account_kind("InstallmentLoan"), AccountKind::Loan));
    }

    #[test]
    fn account_list_is_parsed_strictly() {
        let today = jiff::civil::date(2026, 11, 16);
        let v = json!({"Accounts": [
            {"AccountNumber": "123", "Iban": "DE00123", "Balance": "71.01",
             "ProductName": "Plus Konto online", "Currency": "EUR",
             "Category": "CallMoneyAccount",
             "Owners": [{"FirstName": "Erika", "LastName": "Mustermann"}],
             "DateOfAccountBalance": "2026-11-16T09:17:30.0000000Z",
             "Features": {"CashFlow": true}},
            {"AccountNumber": "456", "Iban": "", "Balance": "1000.00",
             "ProductName": "", "Description": "Sparbrief",
             "Category": "SavingAccount", "Features": {"CashFlow": false}},
        ]});
        let accounts = parse_accounts(&v, today).unwrap();
        assert_eq!(accounts.len(), 2);
        let a = &accounts[0];
        assert_eq!(a.remote.external_ref, "DE00123");
        assert_eq!(a.remote.account_number, "123");
        assert_eq!(a.remote.name, "Plus Konto online");
        assert_eq!(a.remote.owner, "Erika Mustermann");
        assert_eq!(a.remote.balance_cents, 7101);
        assert_eq!(a.remote.balance_date, "2026-11-16");
        assert!(a.cash_flow);
        let b = &accounts[1];
        assert_eq!(b.remote.external_ref, "456");
        assert_eq!(b.remote.name, "Sparbrief");
        assert_eq!(b.remote.currency, "EUR");
        assert_eq!(b.remote.balance_date, "2026-11-16");
        assert!(!b.cash_flow);

        assert!(parse_accounts(&json!({}), today).is_err());
        let no_balance = json!({"Accounts": [{"AccountNumber": "1", "Features": {}}]});
        assert!(parse_accounts(&no_balance, today).is_err());
        let no_number = json!({"Accounts": [{"Balance": "1.00"}]});
        assert!(parse_accounts(&no_number, today).is_err());
    }

    #[test]
    fn cash_flow_becomes_signed_bookings() {
        let v = json!({"Status": "Success", "CashflowList": [
            {"PostingDate": "2026-10-22T22:00:00.0000000Z",
             "ValueDate": "2026-10-24T22:00:00.0000000Z",
             "TransactionTypeDescription": "Habenzinsen",
             "ReferenceAccount": {"AccountHolder": "", "Iban": ""},
             "PaymentNotes": [{"Text": "25.09.26 - 25.10.26"}, {"Text": "Zinssatz 2,5 %"}],
             "CreditAmount": "156.92", "DebitAmount": "", "Currency": "EUR"},
            {"PostingDate": "2026-02-10T23:00:00.0000000Z",
             "ValueDate": "2026-02-10T23:00:00.0000000Z",
             "TransactionTypeDescription": "Lastschrift",
             "ReferenceAccount": {"AccountHolder": "Muster GmbH", "Iban": "DE99"},
             "MandateId": "M-1", "UniqueCreditorId": "DE98ZZZ0",
             "PaymentNotes": [], "CreditAmount": "", "DebitAmount": "5.66"},
        ]});
        let tx = parse_cashflow(&v, "DE00123").unwrap();
        assert_eq!(tx.len(), 2);
        assert_eq!(tx[0].account_ref, "DE00123");
        assert_eq!(tx[0].amount_cents, 15_692);
        assert_eq!(tx[0].booking_date, "2026-10-23");
        assert_eq!(tx[0].value_date, "2026-10-25");
        assert_eq!(tx[0].purpose, "25.09.26 - 25.10.26 Zinssatz 2,5 %");
        assert_eq!(tx[0].booking_text, "Habenzinsen");
        assert_eq!(tx[0].currency, "EUR");
        assert!(tx[0].external_id.is_empty());
        assert_eq!(tx[1].amount_cents, -566);
        assert_eq!(tx[1].booking_date, "2026-02-11");
        assert_eq!(tx[1].counterparty, "Muster GmbH");
        assert_eq!(tx[1].counterparty_iban, "DE99");
        assert_eq!(tx[1].mandate_id, "M-1");
        assert_eq!(tx[1].creditor_id, "DE98ZZZ0");

        assert!(parse_cashflow(&json!({"Status": "Error"}), "x").is_err());
        assert!(parse_cashflow(&json!({"Status": "Success"}), "x").is_err());
        let bad_amount = json!({"Status": "Success", "CashflowList": [
            {"PostingDate": "2026-02-10T23:00:00Z", "CreditAmount": "1,00"}]});
        assert!(parse_cashflow(&bad_amount, "x").is_err());
        let no_date = json!({"Status": "Success", "CashflowList": [{"CreditAmount": "1.00"}]});
        assert!(parse_cashflow(&no_date, "x").is_err());
    }
}
