//! comdirect – official REST API (developer.comdirect.de, public PDF
//! "comdirect REST API Dokumentation").
//!
//!  1. POST /oauth/token  grant_type=password            -> pre-session token
//!  2. GET  /api/session/clients/user/v1/sessions          -> session identifier
//!  3. POST .../sessions/{id}/validate                     -> header x-once-authentication-info
//!     typ P_TAN_PUSH (decoupled) | P_TAN (photoTAN image) | M_TAN (SMS)
//!  4. PATCH .../sessions/{id} (+ x-once-authentication: TAN for P_TAN/M_TAN)
//!  5. POST /oauth/token  grant_type=cd_secondary          -> banking token
//!  6. GET  /api/banking/clients/user/v2/accounts/balances
//!     GET  /api/banking/v1/accounts/{accountId}/transactions
//!     GET  /api/brokerage/clients/user/v3/depots (+ positions)

use async_trait::async_trait;
use base64::Engine;
use serde_json::{json, Value};

use super::{
    http_client, BankConnector, Challenge, ChallengeKind, ChallengeResponse, ConnectContext,
    FetchResult, Phase, RemoteAccount, RemoteTransaction,
};
use crate::error::{CoreError, CoreResult};
use crate::i18n::{tr, trf};
use crate::models::AccountKind;
use crate::money::{json_number_to_cents, json_number_to_cents_rounded};
use crate::secret::SecretString;

const BASE: &str = "https://api.comdirect.de";
/// Provisional (not yet booked) bookings; removed once the bank no longer lists them.
const HOLD_PREFIX: &str = "pending-hold-comdirect-";

pub struct ComdirectConnector {
    client: Option<reqwest::Client>,
    token: SecretString,
    session_uuid: String,
    session_identifier: String,
    challenge_id: String,
    challenge_link: Option<String>,
    challenge: Option<Challenge>,
}

impl ComdirectConnector {
    pub fn new() -> Self {
        ComdirectConnector {
            client: None,
            token: SecretString::default(),
            session_uuid: uuid::Uuid::new_v4().simple().to_string(),
            session_identifier: String::new(),
            challenge_id: String::new(),
            challenge_link: None,
            challenge: None,
        }
    }

    fn client(&mut self) -> CoreResult<reqwest::Client> {
        if self.client.is_none() {
            self.client = Some(http_client()?);
        }
        Ok(self.client.clone().unwrap())
    }

    fn request_info(&self) -> String {
        let req_id = chrono::Local::now().format("%H%M%S%3f").to_string();
        json!({"clientRequestId": {"sessionId": self.session_uuid, "requestId": req_id}})
            .to_string()
    }

    async fn json_or_error(
        resp: reqwest::Response,
    ) -> CoreResult<(reqwest::header::HeaderMap, Value)> {
        let status = resp.status();
        let headers = resp.headers().clone();
        let text = super::read_text_limited(resp).await?;
        if !status.is_success() {
            let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            let msg = v["messages"][0]["message"]
                .as_str()
                .or(v["error_description"].as_str())
                .unwrap_or(&text)
                .to_string();
            return Err(if status.as_u16() == 401 {
                CoreError::Authentication { message: msg }
            } else {
                CoreError::Bank {
                    code: status.as_u16().to_string(),
                    message: msg,
                }
            });
        }
        Ok((
            headers,
            if text.is_empty() {
                Value::Null
            } else {
                serde_json::from_str(&text)?
            },
        ))
    }

    async fn secondary_token(&mut self, ctx: &ConnectContext) -> CoreResult<()> {
        let client = self.client()?;
        let resp = client
            .post(format!("{BASE}/oauth/token"))
            .header("Accept", "application/json")
            .form(&[
                ("client_id", ctx.setting("comdirect.client_id")?),
                ("client_secret", ctx.setting("comdirect.client_secret")?),
                ("grant_type", "cd_secondary"),
                ("token", self.token.expose()),
            ])
            .send()
            .await?;
        let (_, v) = Self::json_or_error(resp).await?;
        self.token = SecretString::new(v["access_token"].as_str().unwrap_or_default().to_string());
        Ok(())
    }

    async fn activate_session(&mut self, tan: Option<&str>) -> CoreResult<()> {
        let client = self.client()?;
        let mut req = client
            .patch(format!("{BASE}/api/session/clients/user/v1/sessions/{}", self.session_identifier))
            .bearer_auth(self.token.expose())
            .header("Accept", "application/json")
            .header("x-http-request-info", self.request_info())
            .header("x-once-authentication-info", json!({"id": self.challenge_id}).to_string())
            .json(&json!({"identifier": self.session_identifier, "sessionTanActive": true, "activated2FA": true}));
        if let Some(t) = tan {
            req = req.header("x-once-authentication", t);
        }
        Self::json_or_error(req.send().await?).await?;
        Ok(())
    }

    async fn get(&mut self, path: &str) -> CoreResult<Value> {
        if !is_api_path(path) {
            return Err(CoreError::InvalidInput {
                message: "unsafe comdirect API path".into(),
            });
        }
        let client = self.client()?;
        let resp = client
            .get(format!("{BASE}{path}"))
            .bearer_auth(self.token.expose())
            .header("Accept", "application/json")
            .header("x-http-request-info", self.request_info())
            .send()
            .await?;
        Ok(Self::json_or_error(resp).await?.1)
    }
}

impl ComdirectConnector {
    async fn get_bytes(&mut self, path: &str, accept: &str) -> CoreResult<Vec<u8>> {
        let client = self.client()?;
        let resp = client
            .get(format!("{BASE}{path}"))
            .bearer_auth(self.token.expose())
            .header("Accept", accept)
            .header("x-http-request-info", self.request_info())
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(CoreError::Bank {
                code: resp.status().as_u16().to_string(),
                message: format!("Download {path}"),
            });
        }
        super::read_body_limited(resp, super::MAX_DOC_BYTES).await
    }

    /// PostBox: recent PDF documents (Finanzreport, Wertpapierabrechnungen …).
    async fn postbox(
        &mut self,
        account_ref: &str,
        since: &str,
    ) -> Vec<crate::documents::RemoteDocument> {
        let mut out = Vec::new();
        let mut entries: Vec<Value> = Vec::new();
        // The v2 route may be missing for some contracts: the first page picks the route.
        let mut route = None;
        let mut first = 0usize;
        for _ in 0..POSTBOX_MAX_PAGES {
            let page = match route {
                Some(r) => self.get(&postbox_path(r, first)).await.ok(),
                None => {
                    let mut found = None;
                    for r in [POSTBOX_V2, POSTBOX_V1] {
                        if let Ok(v) = self.get(&postbox_path(r, first)).await {
                            route = Some(r);
                            found = Some(v);
                            break;
                        }
                    }
                    found
                }
            };
            let Some(page) = page else { break };
            let got = page["values"].as_array().map_or(0, Vec::len);
            entries.extend(page["values"].as_array().cloned().unwrap_or_default());
            first += got;
            if !has_more_pages(&page, first, got, POSTBOX_PAGE) {
                break;
            }
        }
        for d in entries {
            let mime = d["mimeType"].as_str().unwrap_or("application/pdf");
            let date: String = d["dateCreation"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(10)
                .collect();
            if mime != "application/pdf"
                || d["advertisement"].as_bool().unwrap_or(false)
                || (!date.is_empty() && date.as_str() < since)
            {
                continue;
            }
            let id = d["documentId"].as_str().unwrap_or_default().to_string();
            if id.is_empty() {
                continue;
            }
            let mut bytes = Vec::new();
            for path in [
                format!("/api/messages/v2/documents/{id}"),
                format!("/api/messages/v1/documents/{id}"),
            ] {
                if let Ok(b) = self.get_bytes(&path, "application/pdf").await {
                    bytes = b;
                    break;
                }
            }
            if !bytes.starts_with(b"%PDF") {
                continue;
            }
            let name = d["name"].as_str().unwrap_or("PostBox-Dokument").to_string();
            let lower = name.to_lowercase();
            out.push(crate::documents::RemoteDocument {
                account_ref: account_ref.to_string(),
                booking_external_id: None,
                title: name.clone(),
                doc_type: if lower.contains("steuer") {
                    "steuer".into()
                } else if lower.contains("abrechnung") {
                    "abrechnung".into()
                } else {
                    "postbox".into()
                },
                mime_type: "application/pdf".into(),
                file_name: format!("{}_{}.pdf", date, name.replace(' ', "_")),
                bytes,
                source_url: format!("comdirect:postbox:{id}"),
                date,
            });
        }
        out
    }
}

impl Default for ComdirectConnector {
    fn default() -> Self {
        Self::new()
    }
}

fn strip_line_numbers(info: &str) -> String {
    // comdirect sends fixed-width lines (35 characters) prefixed "01", "02", ...
    // A line shorter than that is padded with spaces, a full one is directly
    // followed by the next number ("…Re02chnungsnummer …").
    const LINE: usize = 35;
    if !info.starts_with("01") {
        return info.trim().to_string();
    }
    let chars: Vec<char> = info.chars().collect();
    let mut out = String::new();
    let mut pos = 2;
    let mut n = 2;
    loop {
        let marker: Vec<char> = format!("{n:02}").chars().collect();
        let rest = &chars[pos..];
        // a space followed by the next number ends a padded line ...
        let spaced =
            (0..rest.len()).find(|&i| rest[i] == ' ' && rest[i + 1..].starts_with(&marker));
        // ... a full line is directly followed by it and may end mid-word
        let full = (rest.len() >= LINE + marker.len() && rest[LINE..].starts_with(&marker))
            .then_some(LINE);
        let (end, skip, is_full) = match (spaced, full) {
            (Some(i), Some(f)) if i < f => (i, 1 + marker.len(), false),
            (_, Some(f)) => (f, marker.len(), true),
            (Some(i), None) => (i, 1 + marker.len(), false),
            (None, None) => {
                out.extend(rest);
                break;
            }
        };
        let line: String = rest[..end].iter().collect();
        if is_full {
            out.push_str(&line); // the line breaks are arbitrary: join without a gap
        } else {
            out.push_str(line.trim_end());
            out.push(' ');
        }
        pos += end + skip;
        n += 1;
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[async_trait]
impl BankConnector for ComdirectConnector {
    async fn begin(&mut self, ctx: &ConnectContext) -> CoreResult<Phase> {
        let client = self.client()?;
        // 1. password grant
        let resp = client
            .post(format!("{BASE}/oauth/token"))
            .header("Accept", "application/json")
            .form(&[
                ("client_id", ctx.setting("comdirect.client_id")?),
                ("client_secret", ctx.setting("comdirect.client_secret")?),
                ("grant_type", "password"),
                ("username", ctx.login_name.as_str()),
                ("password", ctx.secret.expose()),
            ])
            .send()
            .await?;
        let (_, v) = Self::json_or_error(resp).await?;
        self.token = SecretString::new(
            v["access_token"]
                .as_str()
                .ok_or_else(|| CoreError::Authentication {
                    message: "no access token".into(),
                })?
                .to_string(),
        );
        // 2. session status
        let sessions = self.get("/api/session/clients/user/v1/sessions").await?;
        self.session_identifier = sessions[0]["identifier"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        // 3. validate -> TAN challenge
        let resp = client
            .post(format!("{BASE}/api/session/clients/user/v1/sessions/{}/validate", self.session_identifier))
            .bearer_auth(self.token.expose())
            .header("Accept", "application/json")
            .header("x-http-request-info", self.request_info())
            .json(&json!({"identifier": self.session_identifier, "sessionTanActive": true, "activated2FA": true}))
            .send()
            .await?;
        let (headers, _) = Self::json_or_error(resp).await?;
        let info: Value = headers
            .get("x-once-authentication-info")
            .and_then(|h| h.to_str().ok())
            .map(serde_json::from_str)
            .transpose()?
            .ok_or_else(|| CoreError::Protocol {
                message: "missing x-once-authentication-info".into(),
            })?;
        self.challenge_id = info["id"].as_str().unwrap_or_default().to_string();
        self.challenge_link = info["link"]["href"].as_str().map(|s| s.to_string());
        let typ = info["typ"].as_str().unwrap_or("");
        let c = match typ {
            "P_TAN_PUSH" => Challenge {
                kind: ChallengeKind::DecoupledPush,
                title: tr("In der comdirect photoTAN-App freigeben").into(),
                message: tr("Öffnen Sie die photoTAN-App und geben Sie die Session-TAN frei.")
                    .into(),
                method_name: "photoTAN push".into(),
                redirect_url: None,
                image: None,
                expires_in_seconds: Some(300),
                poll_interval_seconds: 2,
            },
            "P_TAN" => Challenge {
                kind: ChallengeKind::ImageCode,
                title: tr("photoTAN-Grafik scannen").into(),
                message: tr(
                    "Scannen Sie die Grafik mit der photoTAN-App und geben Sie die TAN ein.",
                )
                .into(),
                method_name: "photoTAN".into(),
                redirect_url: None,
                image: info["challenge"]
                    .as_str()
                    .and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok()),
                expires_in_seconds: Some(300),
                poll_interval_seconds: 0,
            },
            "M_TAN" => Challenge {
                kind: ChallengeKind::Code,
                title: tr("mobileTAN eingeben").into(),
                message: trf(
                    "Eine TAN wurde an {0} gesendet.",
                    &[&info["challenge"].as_str().unwrap_or(tr("Ihr Telefon"))],
                ),
                method_name: "mobileTAN".into(),
                redirect_url: None,
                image: None,
                expires_in_seconds: Some(300),
                poll_interval_seconds: 0,
            },
            other => {
                return Err(CoreError::Protocol {
                    message: format!("unsupported comdirect TAN type '{other}'"),
                })
            }
        };
        self.challenge = Some(c.clone());
        Ok(Phase::Challenge(c))
    }

    async fn respond(
        &mut self,
        ctx: &ConnectContext,
        response: ChallengeResponse,
    ) -> CoreResult<Phase> {
        match response {
            ChallengeResponse::Poll => {
                if let Some(link) = self.challenge_link.clone() {
                    let status = self.get(&link).await?;
                    if status["status"].as_str() == Some("PENDING") {
                        return Ok(Phase::Pending(
                            self.challenge.clone().ok_or(CoreError::SessionClosed)?,
                        ));
                    }
                }
                self.activate_session(None).await?;
            }
            ChallengeResponse::Code(tan) => self.activate_session(Some(tan.expose())).await?,
            ChallengeResponse::ConfirmPayee => {
                return Err(CoreError::Protocol {
                    message: "comdirect: no payee confirmation pending".into(),
                })
            }
        }
        self.secondary_token(ctx).await?;
        Ok(Phase::Ready)
    }

    async fn fetch(&mut self, ctx: &ConnectContext) -> CoreResult<FetchResult> {
        let mut out = FetchResult::default();
        let mut holds_ok = true;
        let balances = self
            .get("/api/banking/clients/user/v2/accounts/balances")
            .await?;
        for item in balances["values"].as_array().cloned().unwrap_or_default() {
            let acc = &item["account"];
            let id = item["accountId"]
                .as_str()
                .or(acc["accountId"].as_str())
                .unwrap_or_default()
                .to_string();
            let type_text = acc["accountType"]["text"]
                .as_str()
                .unwrap_or("Konto")
                .to_string();
            let key = acc["accountType"]["key"].as_str().unwrap_or("");
            let lower = type_text.to_lowercase();
            let kind = if lower.contains("visa") || lower.contains("kredit") || key == "CCA" {
                AccountKind::CreditCard
            } else if lower.contains("tagesgeld") || lower.contains("spar") || key == "DAS" {
                AccountKind::Savings
            } else {
                AccountKind::Checking
            };
            out.accounts.push(RemoteAccount {
                external_ref: id.clone(),
                name: type_text,
                owner: String::new(),
                iban: acc["iban"].as_str().unwrap_or_default().to_string(),
                account_number: acc["accountDisplayId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                currency: item["balance"]["unit"]
                    .as_str()
                    .unwrap_or("EUR")
                    .to_string(),
                kind,
                balance_cents: json_number_to_cents(&item["balance"]["value"]).unwrap_or(0),
                balance_date: super::today(),
            });
            let mut index = 0usize;
            let mut ids = super::ContentIds::default();
            loop {
                let page = self
                    .get(&format!(
                        "/api/banking/v1/accounts/{id}/transactions?transactionState=BOOKED&paging-first={index}&paging-count={TX_PAGE}&min-bookingDate={}",
                        ctx.since
                    ))
                    .await?;
                let values = page["values"].as_array().cloned().unwrap_or_default();
                for t in &values {
                    let mut rt = map_transaction(t, &id);
                    if rt.external_id.is_empty() {
                        rt.external_id = ids.next(&rt);
                    }
                    out.transactions.push(rt);
                    index += 1;
                }
                if !has_more_pages(&page, index, values.len(), TX_PAGE) {
                    break;
                }
            }
            // Not yet booked (card holds, pending transfers). Best effort: an error or a
            // missing right just means no holds; the booked bookings stay untouched.
            if let Ok(page) = self
                .get(&format!(
                    "/api/banking/v1/accounts/{id}/transactions?transactionState=NOTBOOKED&paging-first=0&paging-count=500"
                ))
                .await
            {
                let mut hold_ids = super::ContentIds::default();
                for t in page["values"].as_array().cloned().unwrap_or_default() {
                    let mut rt = map_transaction(&t, &id);
                    if rt.booking_date.is_empty() {
                        rt.booking_date = if rt.value_date.is_empty() {
                            super::today()
                        } else {
                            rt.value_date.clone()
                        };
                    }
                    if rt.booking_text.is_empty() {
                        rt.booking_text = tr("Vorgemerkt").into();
                    }
                    rt.external_id = format!("{HOLD_PREFIX}{}", hold_ids.next(&rt));
                    out.transactions.push(rt);
                }
            } else {
                holds_ok = false;
            }
        }
        // PostBox documents (optional, errors are ignored)
        if let Some(first) = out.accounts.first().map(|a| a.external_ref.clone()) {
            let docs = self.postbox(&first, &ctx.since).await;
            out.documents.extend(docs);
        }
        // Securities accounts (value only)
        crate::depot_debug::log("comdirect REST: fetching depots");
        let depots_res = self.get("/api/brokerage/clients/user/v3/depots").await;
        if let Err(e) = &depots_res {
            crate::depot_debug::log(&format!(
                "comdirect REST: depots request failed ({})",
                e.to_string().chars().take(60).collect::<String>()
            ));
        }
        if let Ok(depots) = depots_res {
            crate::depot_debug::log(&format!(
                "comdirect REST: {} depots",
                depots["values"].as_array().map(|a| a.len()).unwrap_or(0)
            ));
            for d in depots["values"].as_array().cloned().unwrap_or_default() {
                let depot_id = d["depotId"].as_str().unwrap_or_default().to_string();
                let pos_res = self
                    .get(&format!("/api/brokerage/v3/depots/{depot_id}/positions"))
                    .await;
                if let Err(e) = &pos_res {
                    crate::depot_debug::log(&format!(
                        "comdirect REST: positions request failed ({})",
                        e.to_string().chars().take(60).collect::<String>()
                    ));
                }
                let pos = pos_res.unwrap_or(Value::Null);
                crate::depot_debug::log(&format!(
                    "comdirect REST: positions values={} parsed={}",
                    pos["values"].as_array().map(|a| a.len()).unwrap_or(0),
                    parse_positions(&pos).len()
                ));
                let external_ref = format!("depot:{depot_id}");
                let holdings = parse_positions(&pos);
                let balance_cents = depot_value_cents(&pos, &holdings);
                for h in holdings {
                    out.holdings.push(super::RemoteHolding {
                        account_ref: external_ref.clone(),
                        holding: h,
                    });
                }
                out.accounts.push(RemoteAccount {
                    external_ref,
                    name: "Depot".into(),
                    owner: String::new(),
                    iban: String::new(),
                    account_number: d["depotDisplayId"].as_str().unwrap_or_default().to_string(),
                    currency: "EUR".into(),
                    kind: AccountKind::Brokerage,
                    balance_cents,
                    balance_date: super::today(),
                });
            }
        }
        // Claim the hold list as complete only when every NOTBOOKED request worked;
        // otherwise the stored holds stay as they are.
        if holds_ok {
            out.hold_prefix = Some(HOLD_PREFIX.into());
        }
        Ok(out)
    }

    async fn finish(&mut self, _ctx: &ConnectContext) {
        if !self.token.is_empty() {
            if let Ok(c) = self.client() {
                let _ = c
                    .delete(format!("{BASE}/oauth/revoke"))
                    .bearer_auth(self.token.expose())
                    .send()
                    .await;
            }
        }
        self.token = SecretString::default();
    }
}

const TX_PAGE: usize = 500;
const POSTBOX_PAGE: usize = 50;
/// Upper bound for PostBox listing pages (500 documents).
const POSTBOX_MAX_PAGES: usize = 10;
const POSTBOX_V2: &str = "/api/messages/clients/user/v2/documents";
const POSTBOX_V1: &str = "/api/messages/v1/documents";

fn postbox_path(route: &str, first: usize) -> String {
    format!("{route}?paging-first={first}&paging-count={POSTBOX_PAGE}")
}

/// Whether another page follows. `fetched` = entries received so far, `got` = size
/// of this page. Without `paging.matches` a full page means "maybe more" (the
/// next request then ends with an empty page).
fn has_more_pages(page: &Value, fetched: usize, got: usize, page_size: usize) -> bool {
    if got == 0 {
        return false;
    }
    match page["paging"]["matches"].as_u64() {
        Some(total) => (fetched as u64) < total,
        None => got >= page_size,
    }
}

/// A SEPA reference; the placeholder `NOTPROVIDED` counts as "unknown".
fn sepa_ref(v: &Value) -> String {
    let s = v.as_str().unwrap_or_default().trim();
    if s.eq_ignore_ascii_case("NOTPROVIDED") {
        String::new()
    } else {
        s.to_string()
    }
}

/// Maps one `/accounts/{id}/transactions` entry. Not yet booked entries have no
/// `reference` and often no `bookingDate`; the caller fills in an id and date.
fn map_transaction(t: &Value, account_ref: &str) -> RemoteTransaction {
    let amount = json_number_to_cents(&t["amount"]["value"]).unwrap_or(0);
    let party = if amount < 0 {
        &t["creditor"]
    } else {
        &t["remitter"]
    };
    let text = |v: &Value| v.as_str().unwrap_or_default().to_string();
    RemoteTransaction {
        account_ref: account_ref.to_string(),
        external_id: text(&t["reference"]),
        booking_date: text(&t["bookingDate"]),
        value_date: text(&t["valutaDate"]),
        amount_cents: amount,
        currency: t["amount"]["unit"].as_str().unwrap_or("EUR").to_string(),
        counterparty: text(&party["holderName"]),
        counterparty_iban: text(&party["iban"]),
        purpose: strip_line_numbers(t["remittanceInfo"].as_str().unwrap_or_default()),
        booking_text: text(&t["transactionType"]["text"]),
        end_to_end_id: sepa_ref(&t["endToEndReference"]),
        mandate_id: sepa_ref(&t["directDebitMandateId"]),
        creditor_id: sepa_ref(&t["directDebitCreditorId"]),
        ..Default::default()
    }
}

/// Depot total: `aggregated.currentValue`, else the sum of the position values.
fn depot_value_cents(pos: &Value, holdings: &[crate::models::Holding]) -> i64 {
    json_number_to_cents_rounded(&pos["aggregated"]["currentValue"]["value"])
        .unwrap_or_else(|_| holdings.iter().map(|h| h.value_cents).sum())
}

/// Parses `/brokerage/v3/depots/{id}/positions` into holdings (amounts in cents).
/// Prices and computed values carry up to 4+ decimals (fractional savings-plan
/// shares), so they are rounded rather than rejected.
pub(crate) fn parse_positions(pos: &Value) -> Vec<crate::models::Holding> {
    let cents = |v: &Value| json_number_to_cents_rounded(&v["value"]).unwrap_or(0);
    pos["values"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .map(|p| {
            let ins = &p["instrument"];
            let s = |v: &Value| v.as_str().unwrap_or_default().to_string();
            let first = |a: String, b: String| if a.is_empty() { b } else { a };
            let qty = &p["quantity"]["value"];
            let quantity = qty
                .as_f64()
                .or_else(|| qty.as_str().and_then(|x| x.parse().ok()))
                .unwrap_or(0.0);
            crate::models::Holding {
                isin: first(s(&ins["isin"]), s(&p["isin"])),
                wkn: first(s(&p["wkn"]), s(&ins["wkn"])),
                name: first(s(&ins["name"]), s(&ins["shortName"])),
                quantity,
                current_price_cents: cents(&p["currentPrice"]["price"]),
                purchase_price_cents: cents(&p["purchasePrice"]),
                value_cents: cents(&p["currentValue"]),
            }
        })
        .collect()
}

/// A path that stays on the comdirect host when appended to `BASE`: starts
/// with `/api/`, no `..`, `//`, userinfo (`@`), backslash, whitespace or
/// control characters.
fn is_api_path(path: &str) -> bool {
    path.starts_with("/api/")
        && !path.contains("..")
        && !path.contains('@')
        && !path.contains("//")
        && !path.contains('\\')
        && !path.chars().any(|c| c.is_whitespace() || c.is_control())
}

#[cfg(test)]
mod api_path_tests {
    use super::is_api_path;

    #[test]
    fn only_plain_api_paths_are_sent() {
        assert!(is_api_path("/api/something/v1/sessions/1?x=2"));
        for bad in [
            "",
            "api/x",
            "/v1/x",
            "@evil.com/api/x",
            "/api/x@evil.com",
            "//evil.com/api/x",
            "/api//evil.com",
            "https://evil.com/api/x",
            "/api/../x",
            "/api/x\\y",
            "/api/x y",
            "/api/x\n",
        ] {
            assert!(!is_api_path(bad), "{bad:?}");
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn paging_continues_without_matches_only_on_full_page() {
        let with = |m: u64| serde_json::json!({"paging": {"matches": m}});
        assert!(super::has_more_pages(&with(1200), 500, 500, 500));
        assert!(!super::has_more_pages(&with(500), 500, 500, 500));
        let none = serde_json::json!({});
        assert!(super::has_more_pages(&none, 500, 500, 500));
        assert!(!super::has_more_pages(&none, 120, 120, 500));
        assert!(!super::has_more_pages(&none, 500, 0, 500));
    }

    #[test]
    fn postbox_path_pages_by_offset() {
        assert_eq!(
            super::postbox_path(super::POSTBOX_V2, 100),
            "/api/messages/clients/user/v2/documents?paging-first=100&paging-count=50"
        );
    }

    #[test]
    fn maps_sepa_references_of_a_direct_debit() {
        let j: serde_json::Value = serde_json::from_str(
            r#"{"reference":"R1","bookingDate":"2026-01-05","amount":{"value":"-9.99","unit":"EUR"},
            "creditor":{"holderName":"Stadtwerke"},
            "endToEndReference":"NOTPROVIDED","directDebitMandateId":" M-123 ",
            "directDebitCreditorId":"DE98ZZZ09999999999","remittanceInfo":"Strom"}"#,
        )
        .unwrap();
        let t = super::map_transaction(&j, "acc");
        assert_eq!(t.end_to_end_id, "");
        assert_eq!(t.mandate_id, "M-123");
        assert_eq!(t.creditor_id, "DE98ZZZ09999999999");
    }

    #[test]
    fn parses_depot_positions() {
        let j: serde_json::Value = serde_json::from_str(
            r#"{"values":[
          {"positionId":"1","wkn":"A0RPWH","quantity":{"value":"12.5","unit":"XXX"},
           "currentPrice":{"price":{"value":"61.34","unit":"EUR"}},
           "purchasePrice":{"value":"55.10","unit":"EUR"},
           "currentValue":{"value":"766.75","unit":"EUR"},
           "instrument":{"name":"iShares Physical Gold","isin":"IE00B4ND3602"}}],
          "aggregated":{"currentValue":{"value":"766.75"}}}"#,
        )
        .unwrap();
        let h = super::parse_positions(&j);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].wkn, "A0RPWH");
        assert_eq!(h[0].isin, "IE00B4ND3602");
        assert_eq!(h[0].name, "iShares Physical Gold");
        assert_eq!(h[0].quantity, 12.5);
        assert_eq!(h[0].current_price_cents, 6134);
        assert_eq!(h[0].purchase_price_cents, 5510);
        assert_eq!(h[0].value_cents, 76675);
    }

    #[test]
    fn rounds_depot_values_with_extra_decimals() {
        // Issue #339: a fractional savings-plan position (4-decimal value)
        // used to be read as 0, and with it the aggregated depot total.
        let j: serde_json::Value = serde_json::from_str(
            r#"{"values":[
          {"wkn":"A0RPWH","quantity":{"value":"3.2871","unit":"XXX"},
           "currentPrice":{"price":{"value":"125.4562","unit":"EUR"}},
           "purchasePrice":{"value":"98.1234","unit":"EUR"},
           "currentValue":{"value":"412.3871","unit":"EUR"},
           "instrument":{"isin":"IE00B4L5Y983","name":"ETF"}},
          {"wkn":"716460","quantity":{"value":"10","unit":"XXX"},
           "currentPrice":{"price":{"value":"50.00","unit":"EUR"}},
           "currentValue":{"value":"500.00","unit":"EUR"},
           "instrument":{"isin":"DE0007164600","name":"SAP"}}],
          "aggregated":{"currentValue":{"value":"912.3871","unit":"EUR"}}}"#,
        )
        .unwrap();
        let h = super::parse_positions(&j);
        assert_eq!(h[0].value_cents, 41239);
        assert_eq!(h[0].current_price_cents, 12546);
        assert_eq!(h[0].purchase_price_cents, 9812);
        assert_eq!(super::depot_value_cents(&j, &h), 91239);
        // No usable aggregate: sum of the positions.
        let mut j = j;
        j["aggregated"] = serde_json::Value::Null;
        assert_eq!(super::depot_value_cents(&j, &h), 91239);
    }

    #[test]
    fn strips_remittance_line_numbers() {
        assert_eq!(
            super::strip_line_numbers("01Miete Januar                     02Wohnung 3"),
            "Miete Januar Wohnung 3"
        );
        assert_eq!(super::strip_line_numbers("Kartenzahlung"), "Kartenzahlung");
        // full 35-character lines carry no padding before the next number
        assert_eq!(
            super::strip_line_numbers(
                "01easybell.de Kundennummer 4077068 Re02chnungsnummer R2026003619640 03End-to-End-Ref.: 04R2026003619640 05CORE / Mandatsref.: 06MND-2841562-1 07Gläubiger-ID: 08DE74ZZZ00000150248"
            ),
            "easybell.de Kundennummer 4077068 Rechnungsnummer R2026003619640 End-to-End-Ref.: R2026003619640 CORE / Mandatsref.: MND-2841562-1 Gläubiger-ID: DE74ZZZ00000150248"
        );
    }

    #[test]
    fn maps_not_booked_transaction_without_reference() {
        let j: serde_json::Value = serde_json::from_str(
            r#"{"bookingDate":null,"valutaDate":"2026-11-12",
                "amount":{"value":"-12.50","unit":"EUR"},
                "creditor":{"holderName":"REWE"},
                "remittanceInfo":"01Kartenzahlung",
                "transactionType":{"key":"CARD","text":"Kartenumsatz"}}"#,
        )
        .unwrap();
        let t = super::map_transaction(&j, "acc1");
        assert_eq!(t.amount_cents, -1250);
        assert_eq!(t.counterparty, "REWE");
        assert_eq!(t.booking_date, "");
        assert_eq!(t.value_date, "2026-11-12");
        assert!(t.external_id.is_empty());
    }
}
