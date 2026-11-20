//! Hanseatic Bank (meine.hanseaticbank.de): balances and bookings of the
//! credit cards and savings products (SparBrief, Tagesgeld, Festgeld) and the
//! PDF documents of the Postbox. The online banking is a single-page app on a
//! JSON API (`connecthb.hanseaticbank.de`); the connector calls the same
//! endpoints (unofficial, they may change at any time). Shapes checked against
//! the public web app on 2026-11-19.
//!
//!  GET  meine.hanseaticbank.de/de/register/sign-in
//!                                      -> `window.__NUXT__.config.public`: the web app's public
//!                                         client key/secret, its Basic credentials and the API base
//!  POST {BASE}/token client_credentials -> app token (only for the approval status)
//!  POST {BASE}/token hbSCACustomPassword (Basic, loginId + password, header DEVICETOKEN if known)
//!                                      -> `access_token` (known device) or an `id_token` whose
//!                                         claims carry `sca_id` and `sca_type` (APP / SMS)
//!  GET  {BASE}/openScaBroker/1.0/customer/<loginId>/status/<sca_id>
//!                                      -> `status` open … complete; `resultData.DEVICETOKEN`
//!  POST {BASE}/token hbSCACustomPassword + DEVICETOKEN -> `access_token`
//!       (SMS: loginId + otp + scaId instead)
//!  GET  {BASE}/customerportal/1.0/accounts?skipCache=false
//!  GET  {BASE}/transaction/1.0/transactionsEnriched/<accountNumber>?page=N&withReservations=true
//!                                      -> `{transactions, more, moreWithSCA}`; older bookings need
//!                                         a further approval and are not read
//!  GET  {BASE}/postbox/1.0/messages    -> the messages of the last 90 days
//!  GET  {BASE}/postbox/1.0/messages/<id>
//!                                      -> `body.content` = the PDF (base64); opening marks it read
//!
//! The device token is kept in the connection state (like the web app's
//! cookie), so later syncs log in without a new approval while the bank
//! accepts it.

use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine;
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;
use zeroize::Zeroize;

use super::{
    BankConnector, Challenge, ChallengeKind, ChallengeResponse, ConnectContext, ContentIds,
    Documents, FetchResult, KnownDocuments, Phase, RemoteAccount, RemoteTransaction,
};
use crate::documents::RemoteDocument;
use crate::error::{CoreError, CoreResult};
use crate::i18n::tr;
use crate::models::AccountKind;
use crate::secret::{SecretJson, SecretString};

const PORTAL: &str = "https://meine.hanseaticbank.de/de/register/sign-in";
const PORTAL_HOST: &str = "meine.hanseaticbank.de";
const WEB_ORIGIN: &str = "https://meine.hanseaticbank.de";
/// The only host that receives the password and the tokens.
const API_HOST: &str = "connecthb.hanseaticbank.de";
/// Account external refs: `hanseatic:<accountNumber>`.
pub(crate) const REF_PREFIX: &str = "hanseatic:";
/// Reservations (not yet booked); removed once the bank no longer lists them.
const HOLD_PREFIX: &str = "pending-hold-hanseatic-";
/// Upper bound for the pages of one booking list (25 bookings each).
const MAX_PAGES: usize = 200;
/// How long the app approval is waited for. Past it, a still open approval or
/// an unreachable bank ends the login instead of keeping the sync waiting.
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(300);

static CONFIG_VALUE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"\b(NORTHLAYER_CLIENT_KEY|NORTHLAYER_CLIENT_SECRET|BASIC_AUTH|NORTHLAYER_BASE_URL)\s*:\s*"((?:[^"\\]|\\.)*)""#,
    )
    .unwrap()
});

fn page_changed() -> CoreError {
    CoreError::protocol(tr(
        "Hanseatic Bank: Antwort nicht erkannt (Online-Banking geändert?).",
    ))
}

fn invalid() -> CoreError {
    CoreError::invalid(tr(
        "Hanseatic Bank: Unvollständige oder ungültige Antwort. Es wurden keine Daten übernommen.",
    ))
}

fn wrong_credentials() -> CoreError {
    CoreError::Authentication {
        message: tr(
            "Hanseatic Bank: Anmeldung fehlgeschlagen. Bitte Kundennummer und Passwort prüfen.",
        )
        .into(),
    }
}

fn locked() -> CoreError {
    CoreError::Authentication {
        message: tr("Hanseatic Bank: Der Zugang ist gesperrt. Bitte an die Bank wenden.").into(),
    }
}

fn session_ended() -> CoreError {
    CoreError::Authentication {
        message: tr("Hanseatic Bank: Die Sitzung wurde beendet. Bitte den Abruf erneut starten.")
            .into(),
    }
}

fn not_approved() -> CoreError {
    CoreError::Authentication {
        message: tr("Hanseatic Bank: Die Anmeldung wurde in der App nicht bestätigt.").into(),
    }
}

fn approval_expired() -> CoreError {
    CoreError::Authentication {
        message: tr(
            "Hanseatic Bank: Die Freigabe in der App ist abgelaufen. Bitte den Abruf erneut starten.",
        )
        .into(),
    }
}

/// Keeps waiting for the app until `deadline`, then gives up.
fn still_waiting(deadline: Option<Instant>, now: Instant) -> CoreResult<Phase> {
    match deadline {
        Some(d) if now >= d => Err(approval_expired()),
        _ => Ok(Phase::Pending(app_challenge())),
    }
}

fn wrong_tan() -> CoreError {
    CoreError::Authentication {
        message: tr("Hanseatic Bank: Die smsTAN ist ungültig.").into(),
    }
}

/// The bank's error code of a refused request (`error_code`, `errorCode`,
/// `error`), reduced to the characters such codes use: it names the cause
/// without carrying personal data into the message.
fn bank_error_code(body: &Value) -> String {
    ["error_code", "errorCode", "error", "code"]
        .iter()
        .filter_map(|k| body[*k].as_str())
        .map(|c| {
            c.chars()
                .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
                .take(40)
                .collect::<String>()
        })
        .find(|c| !c.is_empty())
        .unwrap_or_default()
}

/// A refused request; `step` (a fixed technical name) and the bank's error
/// code tell where it failed.
fn rejected(status: reqwest::StatusCode, step: &str, body: &Value) -> CoreError {
    let code = bank_error_code(body);
    let detail = if code.is_empty() {
        step.to_string()
    } else {
        format!("{step}, {code}")
    };
    CoreError::Bank {
        code: status.as_u16().to_string(),
        message: format!(
            "{} ({detail})",
            tr("Hanseatic Bank: Die Anfrage wurde abgelehnt.")
        ),
    }
}

/// Only `https://meine.hanseaticbank.de` (config page) and
/// `https://connecthb.hanseaticbank.de` (API) are ever talked to.
fn ensure_host(url: &url::Url) -> CoreResult<()> {
    if url.scheme() == "https" && matches!(url.host_str(), Some(PORTAL_HOST | API_HOST)) {
        return Ok(());
    }
    Err(CoreError::protocol(tr(
        "Hanseatic Bank: Unerwartete Weiterleitung zu einer fremden Adresse – abgebrochen.",
    )))
}

fn app_challenge() -> Challenge {
    Challenge {
        kind: ChallengeKind::DecoupledPush,
        title: "Hanseatic Bank App".into(),
        message: tr("Bitte die Anmeldung in der App der Hanseatic Bank bestätigen.").into(),
        method_name: "Hanseatic Bank App".into(),
        redirect_url: None,
        image: None,
        expires_in_seconds: None,
        poll_interval_seconds: 2,
    }
}

fn sms_challenge() -> Challenge {
    Challenge {
        kind: ChallengeKind::Code,
        title: "smsTAN".into(),
        message: tr("Bitte die smsTAN eingeben, die die Hanseatic Bank an die hinterlegte Mobilnummer gesendet hat.").into(),
        method_name: "smsTAN".into(),
        redirect_url: None,
        image: None,
        expires_in_seconds: None,
        poll_interval_seconds: 0,
    }
}

// ------------------------------------------------------------------ web app config

/// The public settings of the web app (the same for every customer).
struct Config {
    client_key: SecretString,
    client_secret: SecretString,
    /// Complete `Authorization` header value (`Basic …`).
    basic_auth: SecretString,
    /// API base, always `https://connecthb.hanseaticbank.de[/…]` without a trailing slash.
    base: String,
}

/// Undoes the escapes of a JavaScript string literal (`/`, `\/`, `\"`).
fn js_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    out.push(ch);
                }
            }
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

fn parse_config(html: &str) -> CoreResult<Config> {
    let mut values: HashMap<&str, String> = HashMap::new();
    let start = html.find("__NUXT__").ok_or_else(page_changed)?;
    for c in CONFIG_VALUE.captures_iter(&html[start..]) {
        let key = c.get(1).map_or("", |m| m.as_str());
        values
            .entry(key)
            .or_insert_with(|| js_unescape(c.get(2).map_or("", |m| m.as_str())));
    }
    let mut take = |k: &str| -> CoreResult<String> {
        values
            .remove(k)
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(page_changed)
    };
    let base = take("NORTHLAYER_BASE_URL")?;
    let url = url::Url::parse(base.trim()).map_err(|_| page_changed())?;
    if url.scheme() != "https" || url.host_str() != Some(API_HOST) {
        return Err(page_changed());
    }
    let basic = take("BASIC_AUTH")?;
    if !basic.starts_with("Basic ") {
        return Err(page_changed());
    }
    Ok(Config {
        client_key: take("NORTHLAYER_CLIENT_KEY")?.into(),
        client_secret: take("NORTHLAYER_CLIENT_SECRET")?.into(),
        basic_auth: basic.into(),
        base: url.as_str().trim_end_matches('/').to_string(),
    })
}

// ------------------------------------------------------------------ tokens

/// Claims of an unverified JWT (only read to route the login, never trusted
/// for anything else).
fn jwt_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum ScaKind {
    App,
    Sms,
}

#[derive(Debug, Clone, PartialEq)]
struct Sca {
    id: String,
    kind: ScaKind,
}

/// Answer of the password token request.
#[derive(Debug, PartialEq)]
enum Login {
    Token(String),
    Sca(Sca),
}

fn login_of(v: &Value) -> CoreResult<Login> {
    if let Some(t) = v["access_token"].as_str().filter(|t| !t.is_empty()) {
        return Ok(Login::Token(t.to_string()));
    }
    let claims = v["id_token"]
        .as_str()
        .and_then(jwt_claims)
        .ok_or_else(page_changed)?;
    let id = claims["sca_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(page_changed)?;
    let kind = match claims["sca_type"].as_str().unwrap_or("") {
        "APP" => ScaKind::App,
        "SMS" => ScaKind::Sms,
        _ => return Err(page_changed()),
    };
    Ok(Login::Sca(Sca {
        id: id.to_string(),
        kind,
    }))
}

/// Error of a refused token request (`error_code` HBAUTH… / HBSCA…).
fn token_error(status: reqwest::StatusCode, body: &Value, step: &str) -> CoreError {
    match body["error_code"].as_str().unwrap_or("") {
        "HBAUTH400" | "HBAUTH401" => wrong_credentials(),
        "HBAUTH412" | "HBAUTH423" | "HBSCA423" => locked(),
        "HBSCA422" => wrong_tan(),
        _ if status == reqwest::StatusCode::UNAUTHORIZED => wrong_credentials(),
        _ if status == reqwest::StatusCode::LOCKED => locked(),
        _ => rejected(status, step, body),
    }
}

/// State of the approval in the app.
#[derive(Debug, PartialEq)]
enum Approval {
    Pending,
    /// The device token the approval issued.
    Approved(String),
    Rejected,
    Locked,
}

fn approval_of(v: &Value) -> CoreResult<Approval> {
    Ok(match v["status"].as_str().unwrap_or("") {
        "open" | "accepted" | "pending" => Approval::Pending,
        "complete" => Approval::Approved(
            v["resultData"]["DEVICETOKEN"]
                .as_str()
                .filter(|t| !t.is_empty())
                .ok_or_else(page_changed)?
                .to_string(),
        ),
        "canceled" | "failed" | "timeout" => Approval::Rejected,
        "locked" => Approval::Locked,
        _ => return Err(page_changed()),
    })
}

/// The part of the connection state this connector adds to the known documents.
#[derive(Deserialize, Default)]
struct SavedLogin {
    #[serde(default)]
    device_token: Option<SecretString>,
}

/// `documents` (known Postbox ids) plus the device token.
fn state_with(known: &KnownDocuments, device_token: Option<&SecretString>) -> Option<SecretJson> {
    let docs = known.to_state()?;
    let Some(token) = device_token else {
        return Some(docs);
    };
    docs.with_value(|v| {
        if !v.is_object() {
            *v = Value::Object(Default::default());
        }
        v["device_token"] = Value::String(token.expose().to_string());
        SecretJson::encode(v).ok()
    })
}

// ------------------------------------------------------------------ accounts and bookings

/// `12.5`, `-1234.56`, `"1.234,56"`, `"-7,5"` → cents (no floats).
fn cents(v: &Value) -> Option<i64> {
    let raw = match v {
        Value::Number(n) => n.to_string(),
        Value::String(s) => {
            let s = s.trim().replace(['\u{2212}', '\u{2013}'], "-");
            if s.contains(',') {
                s.replace('.', "").replace(',', ".")
            } else {
                s
            }
        }
        _ => return None,
    };
    let raw = raw.trim().trim_end_matches(['€', ' ']).trim();
    let (neg, digits) = match raw.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, raw.strip_prefix('+').unwrap_or(raw)),
    };
    let (int, frac) = digits.split_once('.').unwrap_or((digits, ""));
    if int.is_empty() && frac.is_empty()
        || !int.chars().all(|c| c.is_ascii_digit())
        || !frac.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    let mut f = frac.chars();
    let tens = f.next().and_then(|c| c.to_digit(10)).unwrap_or(0) as i64;
    let ones = f.next().and_then(|c| c.to_digit(10)).unwrap_or(0) as i64;
    let round = i64::from(f.next().and_then(|c| c.to_digit(10)).unwrap_or(0) >= 5);
    let whole: i64 = if int.is_empty() { 0 } else { int.parse().ok()? };
    let value = whole
        .checked_mul(100)?
        .checked_add(tens * 10 + ones + round)?;
    Some(if neg { -value } else { value })
}

/// `TT.MM.JJJJ` or `JJJJ-MM-TT[T…]` → `JJJJ-MM-TT`.
fn iso_date(s: &str) -> Option<String> {
    let s = s.trim();
    if let Some(d) = s
        .get(..10)
        .and_then(|p| jiff::civil::Date::strptime("%Y-%m-%d", p).ok())
    {
        return Some(crate::dates::iso(d));
    }
    jiff::civil::Date::strptime("%d.%m.%Y", s.get(..10)?)
        .ok()
        .map(crate::dates::iso)
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().map(str::trim).unwrap_or("")
}

/// Account number as the API writes it (string or number).
fn number_of(v: &Value, key: &str) -> String {
    match &v[key] {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Account {
    number: String,
    iban: String,
    label: String,
    owner: String,
    kind: AccountKind,
    balance_cents: i64,
}

fn account_kind(product_type: &str) -> Option<AccountKind> {
    Some(match product_type {
        "Kreditkarte" => AccountKind::CreditCard,
        "Spar" | "Tagesgeld" | "Festgeld" => AccountKind::Savings,
        "Giro" => AccountKind::Checking,
        "Kredit" | "Darlehen" => AccountKind::Loan,
        // Insurances and unknown products carry no bookings.
        _ => return None,
    })
}

/// Balance of a product. A credit card whose `saldo` is the amount owed
/// (`availableAmount` = `limit` − `saldo`) is turned into a negative balance.
fn balance_of(a: &Value, kind: AccountKind) -> Option<i64> {
    let saldo = cents(&a["saldo"])?;
    if kind == AccountKind::CreditCard
        && saldo > 0
        && let (Some(limit), Some(available)) = (cents(&a["limit"]), cents(&a["availableAmount"]))
        && limit - saldo == available
        && limit + saldo != available
    {
        return Some(-saldo);
    }
    Some(saldo)
}

fn parse_accounts(v: &Value) -> CoreResult<Vec<Account>> {
    let list = v.as_array().ok_or_else(invalid)?;
    let mut out = Vec::new();
    for a in list {
        let Some(kind) = account_kind(str_of(a, "productType")) else {
            continue;
        };
        let number = number_of(a, "accountNumber");
        if number.is_empty() {
            return Err(invalid());
        }
        let label = [str_of(a, "productLabel"), str_of(a, "productClass")]
            .into_iter()
            .find(|s| !s.is_empty())
            .unwrap_or(str_of(a, "productType"))
            .to_string();
        out.push(Account {
            iban: str_of(a, "iban").replace(' ', ""),
            owner: str_of(a, "nameOnCard").to_string(),
            balance_cents: balance_of(a, kind).ok_or_else(invalid)?,
            number,
            label,
            kind,
        });
    }
    Ok(out)
}

/// One page of `transactionsEnriched`: its bookings and whether another page
/// can be read without a further approval.
fn page_of(v: &Value) -> CoreResult<(Vec<Value>, bool)> {
    let list = v["transactions"].as_array().ok_or_else(invalid)?;
    Ok((list.clone(), v["more"].as_bool().unwrap_or(false)))
}

fn is_booked(t: &Value) -> bool {
    t["booked"].as_bool().unwrap_or(true)
}

fn booking_date(t: &Value) -> Option<String> {
    iso_date(str_of(t, "date")).or_else(|| iso_date(str_of(t, "transactionDate")))
}

/// Oldest booking date of a page (reservations do not count).
fn oldest_booked(page: &[Value]) -> Option<String> {
    page.iter()
        .filter(|t| is_booked(t))
        .filter_map(booking_date)
        .min()
}

fn parse_bookings(
    raw: &[Value],
    account_ref: &str,
    since: &str,
) -> CoreResult<Vec<RemoteTransaction>> {
    let mut ids = ContentIds::default();
    let mut hold_ids = ContentIds::default();
    let mut seen_ids = std::collections::HashSet::new();
    let mut out = Vec::new();
    for t in raw {
        let booked = is_booked(t);
        let date = booking_date(t).ok_or_else(invalid)?;
        let value_date = iso_date(str_of(t, "transactionDate")).unwrap_or_else(|| date.clone());
        let amount_cents = cents(&t["amount"]).ok_or_else(invalid)?;
        let merchant = str_of(t, "merchantName");
        let recipient = str_of(t, "recipientName");
        let counterparty = if merchant.is_empty() {
            recipient
        } else {
            merchant
        };
        let description = str_of(t, "description");
        let city = str_of(t, "city");
        let purpose = if city.is_empty() || description.contains(city) {
            description.to_string()
        } else if description.is_empty() {
            city.to_string()
        } else {
            format!("{description} {city}")
        };
        let mut rt = RemoteTransaction {
            account_ref: account_ref.to_string(),
            booking_date: date,
            value_date,
            amount_cents,
            currency: "EUR".into(),
            counterparty: counterparty.to_string(),
            counterparty_iban: str_of(t, "recipientIban").replace(' ', ""),
            purpose,
            mandate_id: str_of(t, "mandateReference").to_string(),
            creditor_id: str_of(t, "creditorID").to_string(),
            booking_text: if booked {
                String::new()
            } else {
                tr("Vorgemerkt").into()
            },
            ..Default::default()
        };
        if !booked {
            rt.external_id = format!("{HOLD_PREFIX}{}", hold_ids.next(&rt));
            out.push(rt);
            continue;
        }
        let bank_id = number_of(t, "transactionId");
        rt.external_id = if !bank_id.is_empty() && seen_ids.insert(bank_id.clone()) {
            format!("hanseatic-{bank_id}")
        } else {
            ids.next(&rt)
        };
        if rt.booking_date.as_str() >= since {
            out.push(rt);
        }
    }
    Ok(out)
}

// ------------------------------------------------------------------ Postbox

#[derive(Debug, Clone, PartialEq)]
struct Message {
    id: String,
    account: String,
    kind: String,
    date: String,
}

fn parse_messages(v: &Value) -> CoreResult<Vec<Message>> {
    let list = v.as_array().ok_or_else(invalid)?;
    Ok(list
        .iter()
        .filter_map(|m| {
            let id = number_of(m, "id");
            if id.is_empty() {
                return None;
            }
            Some(Message {
                id,
                account: number_of(m, "accountId"),
                kind: str_of(m, "type").to_string(),
                date: iso_date(str_of(m, "created")).unwrap_or_else(super::today),
            })
        })
        .collect())
}

/// "VertragSparbrief" → "Vertrag Sparbrief".
fn message_title(kind: &str) -> String {
    let mut out = String::new();
    let mut prev_lower = false;
    for c in kind.chars() {
        if c.is_uppercase() && prev_lower {
            out.push(' ');
        }
        prev_lower = c.is_lowercase();
        out.push(c);
    }
    if out.is_empty() {
        "Postbox".into()
    } else {
        out
    }
}

fn document_type(kind: &str) -> &'static str {
    let k = kind.to_lowercase();
    if k.contains("kontoauszug") {
        "kontoauszug"
    } else if k.contains("steuer") {
        "steuer"
    } else {
        "postbox"
    }
}

/// `YYYY-MM-DD_<title>_<id>.pdf`, without path separators.
fn document_file_name(m: &Message) -> String {
    let clean = |s: &str| -> String {
        s.chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    };
    let id: String = clean(&m.id).chars().take(12).collect();
    format!("{}_{}_{id}.pdf", m.date, clean(&message_title(&m.kind)))
}

/// The PDF of a message (`body.content`, base64); `None` if it is none.
fn message_pdf(v: &Value) -> Option<Vec<u8>> {
    let content: String = v["body"]["content"]
        .as_str()?
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(content)
        .ok()?;
    bytes.starts_with(b"%PDF").then_some(bytes)
}

// ------------------------------------------------------------------ connector

/// HTTP client that presents itself like the web app in a browser: the bank's
/// approval service records the device from these headers and fails the
/// login (`HBSCA500`) for clients it does not recognise. Redirects stay on
/// the host, so the password never goes elsewhere.
fn client() -> CoreResult<reqwest::Client> {
    use reqwest::header::{ACCEPT_LANGUAGE, HeaderMap, HeaderValue, ORIGIN, REFERER};
    let mut headers = HeaderMap::new();
    headers.insert(ORIGIN, HeaderValue::from_static(WEB_ORIGIN));
    headers.insert(
        REFERER,
        HeaderValue::from_static("https://meine.hanseaticbank.de/"),
    );
    headers.insert(
        ACCEPT_LANGUAGE,
        HeaderValue::from_static("de-DE,de;q=0.9,en-US;q=0.8,en;q=0.7"),
    );
    Ok(reqwest::Client::builder()
        .user_agent(super::BROWSER_UA)
        .default_headers(headers)
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(60))
        .cookie_store(true)
        .redirect(super::same_host_redirects())
        .build()?)
}

#[derive(Default)]
pub struct HanseaticConnector {
    client: Option<reqwest::Client>,
    config: Option<Config>,
    app_token: Option<SecretString>,
    access_token: Option<SecretString>,
    sca: Option<Sca>,
    /// End of the wait for the app approval.
    sca_deadline: Option<Instant>,
    /// Device token sent with the password (stored or just issued).
    device_token: Option<SecretString>,
}

impl HanseaticConnector {
    pub fn new() -> Self {
        Self::default()
    }

    fn http(&self) -> CoreResult<&reqwest::Client> {
        self.client.as_ref().ok_or(CoreError::SessionClosed)
    }

    fn config(&self) -> CoreResult<&Config> {
        self.config.as_ref().ok_or(CoreError::SessionClosed)
    }

    fn url(&self, path: &str) -> CoreResult<url::Url> {
        let url = url::Url::parse(&format!("{}{path}", self.config()?.base))
            .map_err(|_| page_changed())?;
        ensure_host(&url)?;
        Ok(url)
    }

    /// Sends a request and reads the answer (status, JSON or `Null`).
    async fn send(req: reqwest::RequestBuilder) -> CoreResult<(reqwest::StatusCode, Value)> {
        let resp = req.header("Accept", "application/json").send().await?;
        ensure_host(resp.url())?;
        let status = resp.status();
        let body = super::read_body_limited(resp, super::MAX_DOC_BYTES).await?;
        Ok((status, serde_json::from_slice(&body).unwrap_or(Value::Null)))
    }

    async fn fetch_app_token(&mut self) -> CoreResult<()> {
        let url = self.url("/token")?;
        let cfg = self.config()?;
        let mut fields = [
            ("grant_type", "client_credentials".to_string()),
            ("client_id", cfg.client_key.expose().to_string()),
            ("client_secret", cfg.client_secret.expose().to_string()),
        ];
        let sent = Self::send(self.http()?.post(url).form(&fields)).await;
        for (_, v) in fields.iter_mut() {
            v.zeroize();
        }
        let (status, v) = sent?;
        if !status.is_success() {
            return Err(rejected(status, "app-token", &v));
        }
        let token = v["access_token"].as_str().ok_or_else(page_changed)?;
        self.app_token = Some(token.to_string().into());
        Ok(())
    }

    /// `POST /token` with Basic credentials and the given form fields.
    async fn user_token(
        &self,
        fields: &mut [(&str, String)],
        device_token: Option<&SecretString>,
    ) -> CoreResult<Value> {
        let url = self.url("/token")?;
        let mut req = self
            .http()?
            .post(url)
            .header("Authorization", self.config()?.basic_auth.expose())
            .form(&*fields);
        if let Some(t) = device_token {
            req = req.header("DEVICETOKEN", t.expose());
        }
        let sent = Self::send(req).await;
        for (_, v) in fields.iter_mut() {
            v.zeroize();
        }
        let (status, v) = sent?;
        if !status.is_success() {
            return Err(token_error(status, &v, "login"));
        }
        Ok(v)
    }

    async fn password_login(
        &self,
        ctx: &ConnectContext,
        device_token: Option<&SecretString>,
    ) -> CoreResult<Login> {
        let mut fields = [
            ("grant_type", "hbSCACustomPassword".to_string()),
            ("password", ctx.secret.expose().to_string()),
            ("loginId", ctx.login_name.trim().to_string()),
        ];
        let v = self.user_token(&mut fields, device_token).await?;
        login_of(&v)
    }

    /// Status of the approval; one retry with a fresh app token on 401.
    async fn sca_status(&mut self, login_id: &str, sca_id: &str) -> CoreResult<Value> {
        let mut url = self.url("")?;
        url.path_segments_mut()
            .map_err(|_| page_changed())?
            .pop_if_empty()
            .extend([
                "openScaBroker",
                "1.0",
                "customer",
                login_id,
                "status",
                sca_id,
            ]);
        for attempt in 0..2 {
            if self.app_token.is_none() {
                self.fetch_app_token().await?;
            }
            let token = self.app_token.as_ref().ok_or(CoreError::SessionClosed)?;
            let req = self.http()?.get(url.clone()).bearer_auth(token.expose());
            let (status, v) = Self::send(req).await?;
            if status == reqwest::StatusCode::UNAUTHORIZED && attempt == 0 {
                self.app_token = None;
                continue;
            }
            if !status.is_success() {
                return Err(token_error(status, &v, "sca-status"));
            }
            return Ok(v);
        }
        Err(session_ended())
    }

    /// GET of the logged-in API; 401 = session ended.
    async fn get_json(&self, path: &str) -> CoreResult<Value> {
        let token = self.access_token.as_ref().ok_or(CoreError::SessionClosed)?;
        let req = self
            .http()?
            .get(self.url(path)?)
            .bearer_auth(token.expose());
        let (status, v) = Self::send(req).await?;
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(session_ended());
        }
        if !status.is_success() {
            let step = path
                .trim_start_matches('/')
                .split('/')
                .next()
                .unwrap_or("api");
            return Err(rejected(status, step, &v));
        }
        Ok(v)
    }

    async fn bookings(&self, account: &Account, since: &str) -> CoreResult<Vec<Value>> {
        let mut out = Vec::new();
        for page in 1..=MAX_PAGES {
            let path = format!(
                "/transaction/1.0/transactionsEnriched/{}?page={page}&withReservations=true&withEnrichments=false",
                url::form_urlencoded::byte_serialize(account.number.as_bytes()).collect::<String>()
            );
            let (items, more) = page_of(&self.get_json(&path).await?)?;
            let reached_since = oldest_booked(&items).is_some_and(|d| d.as_str() < since);
            let empty = items.is_empty();
            out.extend(items);
            if !more || reached_since || empty {
                return Ok(out);
            }
        }
        Ok(out)
    }

    /// PDFs of the Postbox messages of the accounts in `wanted` (account
    /// number → external ref) that are not in `known`.
    async fn documents(
        &self,
        wanted: &HashMap<String, String>,
        known: &mut KnownDocuments,
    ) -> CoreResult<Vec<RemoteDocument>> {
        let messages = parse_messages(&self.get_json("/postbox/1.0/messages").await?)?;
        let mut out = Vec::new();
        for m in messages {
            let Some(account_ref) = wanted.get(&m.account) else {
                continue;
            };
            if known.contains(&m.id) {
                continue;
            }
            let path = format!(
                "/postbox/1.0/messages/{}",
                url::form_urlencoded::byte_serialize(m.id.as_bytes()).collect::<String>()
            );
            let Some(bytes) = message_pdf(&self.get_json(&path).await?) else {
                continue;
            };
            known.insert(m.id.clone());
            out.push(RemoteDocument {
                account_ref: account_ref.clone(),
                booking_external_id: None,
                title: message_title(&m.kind),
                doc_type: document_type(&m.kind).into(),
                mime_type: "application/pdf".into(),
                file_name: document_file_name(&m),
                bytes,
                source_url: format!("hanseatic:postbox:{}", m.id),
                date: m.date.clone(),
            });
        }
        Ok(out)
    }

    fn logged_in(&mut self, token: String) -> Phase {
        self.access_token = Some(token.into());
        self.sca = None;
        self.sca_deadline = None;
        Phase::Ready
    }

    /// After an approval: log in again with the issued device token.
    async fn login_with_device(
        &mut self,
        ctx: &ConnectContext,
        device_token: String,
    ) -> CoreResult<Phase> {
        let token: SecretString = device_token.into();
        let login = self.password_login(ctx, Some(&token)).await?;
        self.device_token = Some(token);
        match login {
            Login::Token(t) => Ok(self.logged_in(t)),
            Login::Sca(_) => Err(page_changed()),
        }
    }
}

#[async_trait]
impl BankConnector for HanseaticConnector {
    async fn begin(&mut self, ctx: &ConnectContext) -> CoreResult<Phase> {
        if ctx.login_name.trim().is_empty() || ctx.secret.expose().is_empty() {
            return Err(CoreError::Authentication {
                message: tr("Hanseatic Bank: Kundennummer und Passwort erforderlich.").into(),
            });
        }
        self.client = Some(client()?);
        let resp = self.http()?.get(PORTAL).send().await?;
        ensure_host(resp.url())?;
        if !resp.status().is_success() {
            return Err(rejected(resp.status(), "sign-in-page", &Value::Null));
        }
        let html = super::read_text_limited(resp).await?;
        self.config = Some(parse_config(&html)?);

        let saved: SavedLogin = ctx.state.decode().unwrap_or_default();
        let stored = saved.device_token.filter(|t| !t.is_empty());
        let login = match self.password_login(ctx, stored.as_ref()).await {
            // A device token the bank no longer knows: log in as a new device.
            Err(CoreError::Bank { .. }) if stored.is_some() => {
                self.password_login(ctx, None).await?
            }
            other => {
                self.device_token = stored;
                other?
            }
        };
        match login {
            Login::Token(t) => Ok(self.logged_in(t)),
            Login::Sca(sca) => {
                let challenge = match sca.kind {
                    ScaKind::App => app_challenge(),
                    ScaKind::Sms => sms_challenge(),
                };
                self.sca = Some(sca);
                self.sca_deadline = Some(Instant::now() + APPROVAL_TIMEOUT);
                Ok(Phase::Challenge(challenge))
            }
        }
    }

    async fn respond(&mut self, ctx: &ConnectContext, r: ChallengeResponse) -> CoreResult<Phase> {
        let sca = self.sca.clone().ok_or(CoreError::SessionClosed)?;
        let login_id = ctx.login_name.trim().to_string();
        match (sca.kind, r) {
            (ScaKind::Sms, ChallengeResponse::Code(otp)) => {
                let mut fields = [
                    ("grant_type", "hbSCACustomPassword".to_string()),
                    ("loginId", login_id.clone()),
                    ("otp", otp.expose().trim().to_string()),
                    ("scaId", sca.id.clone()),
                ];
                let v = self.user_token(&mut fields, None).await?;
                let Login::Token(token) = login_of(&v)? else {
                    return Err(page_changed());
                };
                // The approval also issues a device token (best effort).
                if let Ok(Approval::Approved(dt)) = self
                    .sca_status(&login_id, &sca.id)
                    .await
                    .and_then(|v| approval_of(&v))
                {
                    self.device_token = Some(dt.into());
                }
                Ok(self.logged_in(token))
            }
            (ScaKind::Sms, _) => Ok(Phase::Pending(sms_challenge())),
            (ScaKind::App, _) => {
                let status = match self.sca_status(&login_id, &sca.id).await {
                    Ok(v) => v,
                    Err(CoreError::Network { .. }) => {
                        return still_waiting(self.sca_deadline, Instant::now());
                    }
                    Err(e) => return Err(e),
                };
                match approval_of(&status)? {
                    Approval::Pending => still_waiting(self.sca_deadline, Instant::now()),
                    Approval::Rejected => {
                        self.sca = None;
                        Err(not_approved())
                    }
                    Approval::Locked => {
                        self.sca = None;
                        Err(locked())
                    }
                    Approval::Approved(dt) => self.login_with_device(ctx, dt).await,
                }
            }
        }
    }

    async fn fetch(&mut self, ctx: &ConnectContext) -> CoreResult<FetchResult> {
        let today = super::today();
        let accounts = parse_accounts(
            &self
                .get_json("/customerportal/1.0/accounts?skipCache=false")
                .await?,
        )?;
        let refs: HashMap<String, String> = accounts
            .iter()
            .map(|a| (a.number.clone(), format!("{REF_PREFIX}{}", a.number)))
            .collect();
        let mut known = KnownDocuments::from_state(&ctx.state);

        if let Documents::StatementsOnly(only) = &ctx.documents {
            let wanted: HashMap<String, String> =
                refs.into_iter().filter(|(_, r)| r == only).collect();
            let documents = self.documents(&wanted, &mut known).await?;
            return Ok(FetchResult {
                documents,
                state: state_with(&known, self.device_token.as_ref()),
                ..Default::default()
            });
        }

        let mut out = FetchResult::default();
        for account in &accounts {
            let account_ref = format!("{REF_PREFIX}{}", account.number);
            let since = ctx.since_for(&account_ref).to_string();
            let raw = self.bookings(account, &since).await?;
            out.transactions
                .extend(parse_bookings(&raw, &account_ref, &since)?);
            out.balances_reported.push(account_ref.clone());
            out.accounts.push(RemoteAccount {
                external_ref: account_ref,
                name: format!("Hanseatic Bank {}", account.label),
                owner: account.owner.clone(),
                iban: account.iban.clone(),
                account_number: account.number.clone(),
                currency: "EUR".into(),
                kind: account.kind,
                balance_cents: account.balance_cents,
                balance_date: today.clone(),
            });
        }
        // Every booking list was read completely: reservations the bank no
        // longer lists are gone.
        out.hold_prefix = Some(HOLD_PREFIX.into());

        let due = ctx.documents.due_statements();
        if !due.is_empty() {
            let wanted: HashMap<String, String> =
                refs.into_iter().filter(|(_, r)| due.contains(r)).collect();
            // Optional: a failed download never fails the sync.
            let mut fetched = known.clone();
            if let Ok(documents) = self.documents(&wanted, &mut fetched).await {
                out.documents = documents;
                out.statements_fetched = due.to_vec();
                known = fetched;
            }
        }
        out.state = state_with(&known, self.device_token.as_ref());
        Ok(out)
    }

    async fn finish(&mut self, _ctx: &ConnectContext) {
        self.access_token = None;
        self.app_token = None;
        self.sca = None;
        self.config = None;
        self.client = None;
    }
}

#[cfg(test)]
mod tests;
