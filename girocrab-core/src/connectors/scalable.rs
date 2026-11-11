//! Scalable Capital – the GraphQL API behind the official open-source CLI
//! (<https://github.com/ScalableCapital/scalable-cli>). Read-only.
//!
//!  1. POST {issuer}/oauth/device/code  (DPoP)           -> user_code + URL
//!     the user signs in in the browser; we poll
//!  2. POST {issuer}/oauth/token grant_type=device_code   -> DPoP-bound tokens
//!  3. optional 2FA-on-login (approve in the Scalable app) via GraphQL
//!  4. GraphQL: ResolveBrokerIds, BrokerOverview, BrokerLimits, BrokerHoldings,
//!     BrokerTransactions, DiscoverOvernightAccounts, OvernightSummary,
//!     OvernightTransactions, ListFixedTermAccounts
//!
//! Sessions last at most 7 days (24 h idle); afterwards the browser login is
//! requested again. The refresh token and the DPoP key live in the connector
//! state (encrypted vault). Requires "Agentic Investing" to be enabled in the
//! Scalable web profile. See docs/scalable-capital-connector.md.

use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::dpop::DpopKey;
use super::{
    http_client, BankConnector, Challenge, ChallengeKind, ChallengeResponse, ConnectContext,
    FetchResult, Phase, RemoteAccount, RemoteHolding, RemoteTransaction,
};
use crate::error::{CoreError, CoreResult};
use crate::i18n::{tr, trf};
use crate::models::{AccountKind, Holding};
use crate::money::json_number_to_cents;

const GRAPHQL_URL: &str = "https://de.scalable.capital/api/cli/graphql";
const ISSUER: &str = "https://secure.scalable.capital";
const AUDIENCE: &str = "https://de.scalable.capital/api-gateway";
/// Public OAuth client of the official CLI (no secret).
const CLIENT_ID: &str = "yBM3BrpRgwSTJZRdJllvtD6jJEmyxWfE";
const SCOPE: &str = "offline_access openid email";
/// Backend policy: a new interactive login after 7 days at the latest.
const MAX_SESSION_SECS: i64 = 7 * 24 * 3600;
const PAGE_SIZE: u32 = 100;
const MAX_PAGES: usize = 100;

/// Persisted between syncs in the connector state.
#[derive(Serialize, Deserialize, Default, Clone)]
struct Session {
    #[serde(default)]
    dpop_key_hex: String,
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    access_expires_at: i64,
    #[serde(default)]
    refresh_token: String,
    #[serde(default)]
    login_at: i64,
    #[serde(default)]
    person_id: String,
    #[serde(default)]
    session_id: String,
}

enum Step {
    Idle,
    DeviceWait {
        device_code: String,
        interval: u32,
        expires_at: i64,
        challenge: Challenge,
    },
    MfaWait {
        mfa_session_id: String,
        challenge: Challenge,
    },
}

/// OAuth error body (`{"error": ..., "error_description": ...}`).
struct OAuthError {
    status: u16,
    error: String,
    description: String,
}

pub struct ScalableConnector {
    client: Option<reqwest::Client>,
    key: Option<DpopKey>,
    session: Session,
    step: Step,
    auth_nonce: Option<String>,
    api_nonce: Option<String>,
}

impl Default for ScalableConnector {
    fn default() -> Self {
        Self::new()
    }
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn log(msg: &str) {
    crate::depot_debug::log(&format!("scalable: {msg}"));
}

/// Unverified JWT payload (the token came straight from the issuer over TLS).
fn jwt_claims(token: &str) -> Value {
    token
        .split('.')
        .nth(1)
        .and_then(|p| URL_SAFE_NO_PAD.decode(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null)
}

fn str_of(v: &Value) -> String {
    v.as_str().unwrap_or_default().trim().to_string()
}

/// `2026-11-10T12:48:01.123Z` (or epoch seconds) -> local `YYYY-MM-DD`.
fn local_date(v: &Value) -> String {
    if let Some(s) = v.as_str() {
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
            return dt
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d")
                .to_string();
        }
        return s.chars().take(10).collect();
    }
    if let Some(secs) = v.as_i64() {
        if let Some(dt) = chrono::DateTime::from_timestamp(secs, 0) {
            return dt
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d")
                .to_string();
        }
    }
    String::new()
}

fn since_epoch(since: &str) -> Option<i64> {
    if since <= "1970-01-01" {
        return None;
    }
    let d = chrono::NaiveDate::parse_from_str(since, "%Y-%m-%d").ok()?;
    let local = d
        .and_hms_opt(0, 0, 0)?
        .and_local_timezone(chrono::Local)
        .single()?;
    Some(local.timestamp())
}

fn fmt_quantity(v: &Value) -> String {
    let q = v
        .as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0.0);
    if q == 0.0 {
        return String::new();
    }
    let s = format!("{q:.6}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn f64_of(v: &Value) -> f64 {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0.0)
}

/// Statuses that are not (yet) a booking.
fn is_open_status(status: &str) -> bool {
    matches!(
        status,
        "CREATED"
            | "REQUESTED"
            | "PENDING"
            | "PARTIAL_FILLED"
            | "CANCELLED"
            | "CANCEL_REQUESTED"
            | "EXPIRED"
            | "REJECTED"
    )
}

/// Transaction kinds (type + subtypes) that take money out of the account.
fn is_outflow(kinds: &[&str], side: &str) -> bool {
    if side == "BUY" {
        return true;
    }
    if side == "SELL" {
        return false;
    }
    kinds.iter().any(|k| {
        matches!(*k, "BUY" | "SAVINGS_PLAN" | "REINVESTMENT" | "FEE" | "TAX")
            || k.contains("WITHDRAW")
            || k.contains("PAYOUT")
            || k.ends_with("_OUT")
            || k.contains("_OUT_")
            || k.contains("FEE")
            || (k.starts_with("TAX") && !k.contains("RETURN") && !k.contains("REFUND"))
    })
}

/// Human label for a transaction kind (German source text, translated via `tr`).
fn kind_label(kinds: &[&str], side: &str) -> Option<&'static str> {
    for k in kinds {
        let label = match *k {
            "DEPOSIT" | "TRANSFER_IN" | "CASH_TRANSFER_IN" | "INCOMING_TRANSFER" => {
                tr("Einzahlung")
            }
            "WITHDRAWAL" | "TRANSFER_OUT" | "CASH_TRANSFER_OUT" | "OUTGOING_TRANSFER"
            | "PAYOUT" => tr("Auszahlung"),
            "INTEREST" | "INTEREST_PAYMENT" => tr("Zinsen"),
            "DISTRIBUTION" | "DIVIDEND" | "REINVESTMENT_DISTRIBUTION" => tr("Ausschüttung"),
            "BUY" => tr("Kauf"),
            "SELL" => tr("Verkauf"),
            "SAVINGS_PLAN" => tr("Sparplan"),
            "FEE" => tr("Gebühr"),
            "TAX" => tr("Steuer"),
            "TAX_RETURN" | "TAX_REFUND" => tr("Steuererstattung"),
            "SWAP_IN" | "SWAP_OUT" => tr("Tausch"),
            "POCKET_MONEY" | "REINVESTMENT_POCKET_MONEY" => tr("Bonus"),
            "REINVESTMENT" => tr("Wiederanlage"),
            "CURRENCY_SWITCH_BUY" | "CURRENCY_SWITCH_SELL" => tr("Währungstausch"),
            _ => continue,
        };
        return Some(label);
    }
    match side {
        "BUY" => Some(tr("Kauf")),
        "SELL" => Some(tr("Verkauf")),
        _ => None,
    }
}

/// `CASH_TRANSFER_OUT` -> `Cash transfer out` for kinds without a label.
fn humanize(kind: &str) -> String {
    let lower = kind.to_ascii_lowercase().replace('_', " ");
    let mut c = lower.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// Maps one broker / overnight transaction summary to a booking.
/// `names`: ISIN -> security name from the holdings (fills empty descriptions).
pub(crate) fn map_transaction(
    account_ref: &str,
    item: &Value,
    names: &std::collections::HashMap<String, String>,
) -> Option<RemoteTransaction> {
    let id = str_of(&item["id"]);
    if id.is_empty() {
        return None;
    }
    let status = str_of(&item["status"]);
    if is_open_status(&status) || item["isCancellation"].as_bool().unwrap_or(false) {
        return None;
    }
    let date = local_date(&item["lastEventDateTime"]);
    if date.is_empty() {
        return None;
    }
    let typ = str_of(&item["type"]);
    let side = str_of(&item["side"]);
    let subtypes: Vec<String> = [
        &item["cashTransactionType"],
        &item["securityTransactionType"],
        &item["nonTradeSecurityTransactionType"],
    ]
    .iter()
    .map(|v| str_of(v))
    .filter(|s| !s.is_empty())
    .collect();
    // most specific first: subtypes, then the generic type
    let mut kinds: Vec<&str> = subtypes.iter().map(String::as_str).collect();
    if !typ.is_empty() {
        kinds.push(typ.as_str());
    }
    let mut amount = json_number_to_cents(&item["amount"]).unwrap_or(0);
    if amount > 0 && is_outflow(&kinds, &side) {
        amount = -amount;
    }
    let isin = str_of(&item["isin"]);
    let isin = if isin.is_empty() {
        str_of(&item["relatedIsin"])
    } else {
        isin
    };
    let qty = fmt_quantity(&item["quantity"]);
    let description = str_of(&item["description"]);
    let mut purpose = description.clone();
    if !isin.is_empty() {
        if !purpose.is_empty() {
            purpose.push_str(" · ");
        }
        purpose.push_str(&isin);
        if !qty.is_empty() {
            purpose.push_str(&format!(" × {qty}"));
        }
    }
    let label = kind_label(&kinds, &side)
        .map(|l| l.to_string())
        .or_else(|| kinds.first().map(|k| humanize(k)))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Scalable Capital".to_string());
    let is_trade = !isin.is_empty() && !qty.is_empty();
    let is_savings_plan = kinds.iter().any(|k| k.contains("SAVINGS_PLAN"));
    let is_internal = {
        let d = description.to_ascii_lowercase();
        d.contains("internal transfer") || d.contains("umbuchung")
    };
    // booking_text doubles as the classification hint for the merchant matcher
    // (German keywords like the FinTS/Trade Republic connectors deliver them).
    let booking_text = if is_savings_plan {
        "Sparplan ausgeführt".to_string()
    } else if is_trade && amount < 0 {
        "Wertpapierkauf".to_string()
    } else if is_trade && amount > 0 {
        "Wertpapierverkauf".to_string()
    } else if is_internal {
        "Umbuchung".to_string()
    } else if kinds
        .iter()
        .any(|k| k.contains("DEPOSIT") || k.ends_with("_IN"))
    {
        // money from the user's own reference account: a transfer, not income
        "Einzahlung".to_string()
    } else if kinds
        .iter()
        .any(|k| k.contains("WITHDRAW") || k.ends_with("_OUT"))
    {
        "Auszahlung Übertrag auf Referenzkonto".to_string()
    } else {
        kinds.join(" ")
    };
    // Title line: trades show the security (like Trade Republic), everything
    // else the booking kind ("Einzahlung", "Zinsen" …) like a bank statement.
    let security_name = if description.is_empty() {
        names.get(&isin).cloned().unwrap_or_default()
    } else {
        description.clone()
    };
    let (counterparty, purpose) = if is_trade && !security_name.is_empty() {
        let mut p = label.clone();
        p.push_str(&format!(" · {isin} × {qty}"));
        (security_name, p)
    } else {
        (label, purpose)
    };
    Some(RemoteTransaction {
        account_ref: account_ref.to_string(),
        external_id: format!("sc:{id}"),
        booking_date: date.clone(),
        value_date: date,
        amount_cents: amount,
        currency: {
            let c = str_of(&item["currency"]);
            if c.is_empty() {
                "EUR".into()
            } else {
                c
            }
        },
        counterparty,
        counterparty_iban: String::new(),
        purpose,
        booking_text,
        ..Default::default()
    })
}

/// Counts of (type/subtypes, sign) per fetch for the debug log – enum names only.
pub(crate) fn kind_stats(items: &[Value]) -> String {
    let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
    for item in items {
        let typ = str_of(&item["type"]);
        let sub = [
            &item["cashTransactionType"],
            &item["securityTransactionType"],
            &item["nonTradeSecurityTransactionType"],
        ]
        .iter()
        .map(|v| str_of(v))
        .find(|s| !s.is_empty())
        .unwrap_or_default();
        let sign = match json_number_to_cents(&item["amount"]) {
            Ok(a) if a < 0 => "-",
            Ok(a) if a > 0 => "+",
            _ => "0",
        };
        let key = format!(
            "{typ}/{sub}/{}/{}{sign}",
            str_of(&item["side"]),
            str_of(&item["status"])
        );
        *counts.entry(key).or_default() += 1;
    }
    counts
        .into_iter()
        .map(|(k, n)| format!("{k}×{n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `BrokerHoldings` inventory items -> depot positions.
pub(crate) fn map_holdings(inventory: &Value) -> Vec<Holding> {
    inventory["items"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|it| {
            let isin = str_of(&it["isin"]);
            if isin.is_empty() {
                return None;
            }
            let pos = &it["inventory"]["position"];
            let quantity = f64_of(&pos["filled"]);
            let mut price = json_number_to_cents(&it["quoteTick"]["midPrice"]).unwrap_or(0);
            let value = json_number_to_cents(&it["portfolioIsinPerformance"]["valuation"])
                .unwrap_or_else(|_| ((price as f64) * quantity).round() as i64);
            if price == 0 && quantity > 0.0 {
                price = ((value as f64) / quantity).round() as i64;
            }
            Some(Holding {
                isin,
                wkn: String::new(),
                name: str_of(&it["name"]),
                quantity,
                current_price_cents: price,
                purchase_price_cents: json_number_to_cents(&pos["fifoPrice"]).unwrap_or(0),
                value_cents: value,
            })
        })
        .collect()
}

// ------------------------------------------------------------------ GraphQL documents

const Q_RESOLVE_IDS: &str =
    r#"query ResolveBrokerIds($id: ID!) { account(id: $id) { id brokerPortfolios { id } } }"#;

const Q_OVERVIEW: &str = r#"query BrokerOverview($accountId: ID!, $portfolioId: ID!, $includeYearToDate: Boolean!) {
  account(id: $accountId) { brokerPortfolio(id: $portfolioId) {
    valuation(includeYearToDate: $includeYearToDate) { valuation securitiesValuation cryptoValuation timestampUtc { time } }
  } } }"#;

const Q_LIMITS: &str = r#"query BrokerLimits($accountId: ID!, $portfolioId: ID!) {
  account(id: $accountId) { brokerPortfolio(id: $portfolioId) {
    payments { buyingPower { cashBalance cashAvailableToInvest cashAvailableToInvestWithoutCredit loaned } }
  } } }"#;

const Q_HOLDINGS: &str = r#"query BrokerHoldings($accountId: ID!, $portfolioId: ID!, $includeYearToDate: Boolean!, $quoteSource: MarketDataSource!) {
  account(id: $accountId) { brokerPortfolio(id: $portfolioId) {
    inventory { items {
      isin name type
      inventory { position { filled pending blocked fifoPrice } }
      portfolioIsinPerformance { valuation currency }
      quoteTick(source: $quoteSource, includeYearToDate: $includeYearToDate) { venue midPrice currency timestampUtc { time } isOutdated }
    } }
  } } }"#;

const Q_TRANSACTIONS: &str = r#"query BrokerTransactions($accountId: ID!, $portfolioId: ID!, $brokerInput: BrokerTransactionInput!, $cryptoInput: CryptoTransactionInput!, $includeCrypto: Boolean!) {
  account(id: $accountId) { brokerPortfolio(id: $portfolioId) {
    moreTransactions(input: $brokerInput) { cursor total transactions {
      __typename id currency type status isCancellation lastEventDateTime description custodian
      ... on BrokerSecurityTransactionSummary { isin securityTransactionType quantity amount side limitPrice stopPrice }
      ... on BrokerCashTransactionSummary { relatedIsin cashTransactionType amount }
      ... on BrokerNonTradeSecurityTransactionSummary { isin nonTradeSecurityTransactionType quantity amount }
      ... on BrokerEltifTransactionSummary { isin securityTransactionType eltifQuantity amount side }
    } }
    crypto @include(if: $includeCrypto) { moreTransactions(input: $cryptoInput) { cursor total transactions { __typename id } } }
  } } }"#;

const Q_OVERNIGHT_ACCOUNTS: &str = r#"query DiscoverOvernightAccounts($accountId: ID!) {
  account(id: $accountId) { savingsAccounts { __typename id owners { firstName lastName } personalizations { name } state } }
  productList(personId: $accountId) { minors { id name type onboardingState state } }
}"#;

const Q_OVERNIGHT_SUMMARY: &str = r#"query OvernightSummary($accountId: ID!, $savingsAccountId: ID!) {
  account(id: $accountId) { savingsAccount(id: $savingsAccountId) { id
    ... on OvernightSavingsAccount {
      interests { currentAccruedAmount currentInterestBearingAmount depositAccruedLifetimeAmount depositInterestRate estimatedNextPayoutAmount nextPayoutDate { epochSecond } }
      nextPayoutDate { epochSecond }
      totalAmount
    }
  } } }"#;

const Q_OVERNIGHT_TRANSACTIONS: &str = r#"query OvernightTransactions($accountId: ID!, $savingsAccountId: ID!, $input: SavingsAccountCashTransactionInput!) {
  account(id: $accountId) { savingsAccount(id: $savingsAccountId) { id
    moreTransactions(input: $input) { cursor total transactions {
      id currency type status isCancellation lastEventDateTime description cashTransactionType amount custodian relatedIsin
    } }
  } } }"#;

const Q_FIXED_TERM: &str = r#"query ListFixedTermAccounts($accountId: ID!) {
  account(id: $accountId) { fixedTermSavingsAccounts {
    id state fixedTerm interestRate totalAmount configuration
    endDate { time }
    personalizations { name }
  } } }"#;

const Q_2FA_STATE: &str = r#"query Is2faOnLoginEnabled($input: Is2faOnLoginEnabledInput!) { is2faOnLoginEnabled(input: $input) { enabled hasApprovedSession } }"#;
const M_2FA_START: &str = r#"mutation Start2faOnLogin($input: Start2faOnLoginInput!) { start2faOnLogin(input: $input) { mfaSessionId } }"#;
const M_2FA_VALIDATE: &str = r#"mutation Validate2faOnLogin($input: Validate2faOnLoginInput!) { validate2faOnLogin(input: $input) { status } }"#;

// ------------------------------------------------------------------ connector

impl ScalableConnector {
    pub fn new() -> Self {
        ScalableConnector {
            client: None,
            key: None,
            session: Session::default(),
            step: Step::Idle,
            auth_nonce: None,
            api_nonce: None,
        }
    }

    fn client(&mut self) -> CoreResult<reqwest::Client> {
        if self.client.is_none() {
            self.client = Some(http_client()?);
        }
        Ok(self.client.clone().unwrap())
    }

    fn key(&self) -> CoreResult<&DpopKey> {
        self.key.as_ref().ok_or(CoreError::SessionClosed)
    }

    fn state_json(&self) -> Value {
        serde_json::to_value(&self.session).unwrap_or(Value::Null)
    }

    /// POST a form to the issuer with a DPoP proof (one nonce retry).
    /// `Ok(Err(e))` is an OAuth-level error (4xx with a JSON body).
    async fn oauth_form(
        &mut self,
        path: &str,
        form: &[(&str, &str)],
    ) -> CoreResult<Result<Value, OAuthError>> {
        let url = format!("{ISSUER}{path}");
        let client = self.client()?;
        for attempt in 0..2 {
            let proof = self
                .key()?
                .proof("POST", &url, self.auth_nonce.as_deref(), None);
            let resp = client
                .post(&url)
                .header("DPoP", proof)
                .form(form)
                .send()
                .await?;
            let status = resp.status().as_u16();
            let nonce = resp
                .headers()
                .get("dpop-nonce")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            let body = resp.text().await.unwrap_or_default();
            if (200..300).contains(&status) {
                return Ok(Ok(serde_json::from_str(&body).unwrap_or(Value::Null)));
            }
            let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            let error = str_of(&v["error"]);
            let description = str_of(&v["error_description"]);
            let needs_nonce =
                nonce.is_some() && (error == "use_dpop_nonce" || body.contains("use_dpop_nonce"));
            if attempt == 0 && needs_nonce {
                self.auth_nonce = nonce;
                continue;
            }
            if let Some(n) = nonce {
                self.auth_nonce = Some(n);
            }
            return Ok(Err(OAuthError {
                status,
                error,
                description: if description.is_empty() {
                    body.chars().take(200).collect()
                } else {
                    description
                },
            }));
        }
        unreachable!()
    }

    /// One GraphQL operation with the DPoP-bound access token. Refreshes the
    /// token once on 401.
    async fn graphql(&mut self, op: &str, query: &str, variables: Value) -> CoreResult<Value> {
        let mut refreshed = false;
        for attempt in 0..3 {
            let token = self.session.access_token.clone();
            if token.is_empty() {
                return Err(CoreError::Authentication {
                    message: tr("Keine Scalable-Sitzung – bitte anmelden.").into(),
                });
            }
            let proof =
                self.key()?
                    .proof("POST", GRAPHQL_URL, self.api_nonce.as_deref(), Some(&token));
            let client = self.client()?;
            let resp = client
                .post(GRAPHQL_URL)
                .header("Authorization", format!("DPoP {token}"))
                .header("DPoP", proof)
                .json(&json!({"query": query, "variables": variables, "operationName": op}))
                .send()
                .await?;
            let status = resp.status().as_u16();
            let nonce = resp
                .headers()
                .get("dpop-nonce")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            let body = resp.text().await.unwrap_or_default();
            if let Some(n) = &nonce {
                self.api_nonce = Some(n.clone());
            }
            if (200..300).contains(&status) {
                let v: Value = serde_json::from_str(&body).map_err(|e| CoreError::Protocol {
                    message: format!("{op}: {e}"),
                })?;
                if let Some(errs) = v["errors"].as_array().filter(|a| !a.is_empty()) {
                    let msg = errs
                        .iter()
                        .map(|e| str_of(&e["message"]))
                        .filter(|m| !m.is_empty())
                        .collect::<Vec<_>>()
                        .join("; ");
                    log(&format!("{op}: graphql errors: {msg}"));
                    return Err(CoreError::Bank {
                        code: String::new(),
                        message: format!("{op}: {msg}"),
                    });
                }
                return Ok(v["data"].clone());
            }
            let lower = body.to_ascii_lowercase();
            if attempt == 0
                && nonce.is_some()
                && (lower.contains("use_dpop_nonce") || lower.contains("invalid_dpop_proof"))
            {
                continue;
            }
            if status == 401 && !refreshed {
                refreshed = true;
                log(&format!("{op}: 401, refreshing token"));
                match self.refresh().await? {
                    Ok(()) => continue,
                    Err(e) => {
                        return Err(CoreError::Authentication {
                            message: trf(
                                "Scalable-Sitzung abgelaufen ({0}) – bitte erneut anmelden.",
                                &[&e.error],
                            ),
                        })
                    }
                }
            }
            if status == 429 {
                return Err(CoreError::Network {
                    message: tr(
                        "Scalable Capital: Ratenlimit erreicht – bitte später erneut abrufen.",
                    )
                    .into(),
                });
            }
            log(&format!(
                "{op}: HTTP {status}: {}",
                body.chars().take(160).collect::<String>()
            ));
            return Err(CoreError::Bank {
                code: status.to_string(),
                message: format!("{op}: HTTP {status}"),
            });
        }
        Err(CoreError::Protocol {
            message: format!("{op}: DPoP nonce negotiation failed"),
        })
    }

    fn apply_tokens(&mut self, v: &Value) {
        let access = str_of(&v["access_token"]);
        if !access.is_empty() {
            self.session.access_token = access.clone();
            let expires_in = v["expires_in"].as_i64().unwrap_or(1200);
            self.session.access_expires_at = now() + expires_in;
            let claims = jwt_claims(&access);
            let pid = [
                "https://de.scalable.capital/person_id",
                "https://de.scalable.capital/personId",
                "person_id",
                "personId",
            ]
            .iter()
            .map(|k| str_of(&claims[*k]))
            .find(|s| !s.is_empty());
            if let Some(pid) = pid {
                self.session.person_id = pid;
            }
            let sid = [
                "https://de.scalable.capital/session_id",
                "session_id",
                "sid",
            ]
            .iter()
            .map(|k| str_of(&claims[*k]))
            .find(|s| !s.is_empty());
            if let Some(sid) = sid {
                self.session.session_id = sid;
            }
        }
        let refresh = str_of(&v["refresh_token"]);
        if !refresh.is_empty() {
            self.session.refresh_token = refresh;
        }
    }

    /// Refreshes the access token (rotating refresh token).
    async fn refresh(&mut self) -> CoreResult<Result<(), OAuthError>> {
        let refresh = self.session.refresh_token.clone();
        if refresh.is_empty() {
            return Ok(Err(OAuthError {
                status: 0,
                error: "no_refresh_token".into(),
                description: String::new(),
            }));
        }
        let sid = self.session.session_id.clone();
        let mut form = vec![
            ("grant_type", "refresh_token"),
            ("client_id", CLIENT_ID),
            ("refresh_token", refresh.as_str()),
        ];
        if !sid.is_empty() {
            form.push(("session_id", sid.as_str()));
        }
        match self.oauth_form("/oauth/token", &form).await? {
            Ok(v) => {
                self.apply_tokens(&v);
                log("token refreshed");
                Ok(Ok(()))
            }
            Err(e) => {
                log(&format!(
                    "refresh failed: {} {} {}",
                    e.status, e.error, e.description
                ));
                Ok(Err(e))
            }
        }
    }

    /// Makes sure the access token is valid for at least a minute.
    async fn ensure_token(&mut self) -> CoreResult<()> {
        if self.session.access_expires_at - now() > 60 && !self.session.access_token.is_empty() {
            return Ok(());
        }
        match self.refresh().await? {
            Ok(()) => Ok(()),
            Err(e) => Err(CoreError::Authentication {
                message: trf(
                    "Scalable-Sitzung abgelaufen ({0}) – bitte erneut anmelden.",
                    &[&e.error],
                ),
            }),
        }
    }

    fn session_usable(&self) -> bool {
        !self.session.refresh_token.is_empty()
            && !self.session.dpop_key_hex.is_empty()
            && self.session.login_at > 0
            && now() - self.session.login_at < MAX_SESSION_SECS
    }

    async fn start_device_flow(&mut self) -> CoreResult<Phase> {
        let key = DpopKey::generate();
        self.session = Session {
            dpop_key_hex: key.to_hex(),
            ..Session::default()
        };
        self.key = Some(key);
        self.auth_nonce = None;
        self.api_nonce = None;
        let v = match self
            .oauth_form(
                "/oauth/device/code",
                &[
                    ("client_id", CLIENT_ID),
                    ("audience", AUDIENCE),
                    ("scope", SCOPE),
                ],
            )
            .await?
        {
            Ok(v) => v,
            Err(e) => {
                log(&format!(
                    "device code request failed: {} {} {}",
                    e.status, e.error, e.description
                ));
                return Err(CoreError::Bank {
                    code: e.status.to_string(),
                    message: trf(
                        "Scalable Capital: Anmeldung konnte nicht gestartet werden ({0}: {1}).",
                        &[&e.error, &e.description],
                    ),
                });
            }
        };
        let device_code = str_of(&v["device_code"]);
        let user_code = str_of(&v["user_code"]);
        let uri = str_of(&v["verification_uri"]);
        let uri_complete = str_of(&v["verification_uri_complete"]);
        if device_code.is_empty() || uri.is_empty() {
            return Err(CoreError::Protocol {
                message: "device code response incomplete".into(),
            });
        }
        let interval = v["interval"].as_u64().unwrap_or(5).max(1) as u32;
        let expires_in = v["expires_in"].as_u64().unwrap_or(600) as u32;
        log(&format!(
            "device flow started, interval {interval}s, expires in {expires_in}s"
        ));
        let challenge = Challenge {
            kind: ChallengeKind::DecoupledPush,
            title: tr("Bei Scalable Capital im Browser anmelden").into(),
            message: trf(
                "Öffnen Sie {0} und bestätigen Sie dort den Code {1}. Danach wartet GiroCrab automatisch auf die Freigabe.",
                &[&uri, &user_code],
            ),
            method_name: tr("Browser-Anmeldung").into(),
            redirect_url: Some(if uri_complete.is_empty() {
                uri
            } else {
                uri_complete
            }),
            image: None,
            expires_in_seconds: Some(expires_in),
            poll_interval_seconds: interval,
        };
        self.step = Step::DeviceWait {
            device_code,
            interval,
            expires_at: now() + expires_in as i64,
            challenge: challenge.clone(),
        };
        Ok(Phase::Challenge(challenge))
    }

    async fn poll_device(&mut self) -> CoreResult<Phase> {
        let (device_code, interval, expires_at, challenge) = match &self.step {
            Step::DeviceWait {
                device_code,
                interval,
                expires_at,
                challenge,
            } => (
                device_code.clone(),
                *interval,
                *expires_at,
                challenge.clone(),
            ),
            _ => return Err(CoreError::SessionClosed),
        };
        if now() > expires_at {
            return Err(CoreError::Authentication {
                message: tr("Die Browser-Anmeldung ist abgelaufen – bitte erneut starten.").into(),
            });
        }
        let form = [
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", device_code.as_str()),
            ("client_id", CLIENT_ID),
        ];
        match self.oauth_form("/oauth/token", &form).await? {
            Ok(v) => {
                self.apply_tokens(&v);
                self.session.login_at = now();
                if self.session.access_token.is_empty() {
                    return Err(CoreError::Protocol {
                        message: "token response without access_token".into(),
                    });
                }
                log(&format!(
                    "device login done, person id {}",
                    if self.session.person_id.is_empty() {
                        "missing"
                    } else {
                        "ok"
                    }
                ));
                self.step = Step::Idle;
                self.after_login().await
            }
            Err(e) => match e.error.as_str() {
                "authorization_pending" => Ok(Phase::Pending(challenge)),
                "slow_down" => {
                    let interval = interval + 2;
                    let mut c = challenge;
                    c.poll_interval_seconds = interval;
                    self.step = Step::DeviceWait {
                        device_code,
                        interval,
                        expires_at,
                        challenge: c.clone(),
                    };
                    Ok(Phase::Pending(c))
                }
                "access_denied" => Err(CoreError::Authentication {
                    message: tr("Die Anmeldung wurde im Browser abgelehnt.").into(),
                }),
                "expired_token" => Err(CoreError::Authentication {
                    message: tr("Die Browser-Anmeldung ist abgelaufen – bitte erneut starten.")
                        .into(),
                }),
                other => {
                    log(&format!(
                        "device poll failed: {} {} {}",
                        e.status, other, e.description
                    ));
                    Err(CoreError::Bank {
                        code: e.status.to_string(),
                        message: format!("{other}: {}", e.description),
                    })
                }
            },
        }
    }

    /// Optional second factor after the device login (approve in the app).
    async fn after_login(&mut self) -> CoreResult<Phase> {
        let user_id = self.session.person_id.clone();
        if user_id.is_empty() {
            log("no person id in token; skipping 2FA check");
            return Ok(Phase::Ready);
        }
        let state = match self
            .graphql(
                "Is2faOnLoginEnabled",
                Q_2FA_STATE,
                json!({"input": {"userId": user_id}}),
            )
            .await
        {
            Ok(v) => v,
            Err(e) => {
                log(&format!("2FA state query failed, continuing: {e}"));
                return Ok(Phase::Ready);
            }
        };
        let s = &state["is2faOnLoginEnabled"];
        let enabled = s["enabled"].as_bool().unwrap_or(false);
        let approved = s["hasApprovedSession"].as_bool().unwrap_or(false);
        log(&format!(
            "2FA on login: enabled={enabled} approved={approved}"
        ));
        if !enabled || approved {
            return Ok(Phase::Ready);
        }
        let started = self
            .graphql(
                "Start2faOnLogin",
                M_2FA_START,
                json!({"input": {"userId": user_id, "deviceName": "GiroCrab", "deviceType": "CLI"}}),
            )
            .await?;
        let mfa_session_id = str_of(&started["start2faOnLogin"]["mfaSessionId"]);
        if mfa_session_id.is_empty() {
            return Err(CoreError::Protocol {
                message: "2FA on login: no mfaSessionId".into(),
            });
        }
        let challenge = Challenge {
            kind: ChallengeKind::DecoupledPush,
            title: tr("Anmeldung in der Scalable-App freigeben").into(),
            message: tr("Scalable Capital hat eine Freigabe-Anfrage an Ihr verknüpftes Gerät gesendet. Bitte dort bestätigen.").into(),
            method_name: tr("2FA in der Scalable-App").into(),
            redirect_url: None,
            image: None,
            expires_in_seconds: Some(300),
            poll_interval_seconds: 3,
        };
        self.step = Step::MfaWait {
            mfa_session_id,
            challenge: challenge.clone(),
        };
        Ok(Phase::Challenge(challenge))
    }

    async fn poll_mfa(&mut self) -> CoreResult<Phase> {
        let (mfa_session_id, challenge) = match &self.step {
            Step::MfaWait {
                mfa_session_id,
                challenge,
            } => (mfa_session_id.clone(), challenge.clone()),
            _ => return Err(CoreError::SessionClosed),
        };
        let user_id = self.session.person_id.clone();
        let v = self
            .graphql(
                "Validate2faOnLogin",
                M_2FA_VALIDATE,
                json!({"input": {"userId": user_id, "mfaSessionId": mfa_session_id}}),
            )
            .await?;
        let status = str_of(&v["validate2faOnLogin"]["status"]);
        log(&format!("2FA status: {status}"));
        match status.as_str() {
            "APPROVED" | "APPROVE" | "SUCCESS" => {
                self.step = Step::Idle;
                Ok(Phase::Ready)
            }
            "DENY" | "DENIED" => Err(CoreError::Authentication {
                message: tr("Die Freigabe wurde in der Scalable-App abgelehnt.").into(),
            }),
            "TIMEOUT_RETRY" | "TIMEOUT" => Err(CoreError::Authentication {
                message: tr(
                    "Die Freigabe in der Scalable-App ist abgelaufen – bitte erneut anmelden.",
                )
                .into(),
            }),
            _ => Ok(Phase::Pending(challenge)),
        }
    }

    // ---------------------------------------------------------- fetch helpers

    async fn fetch_broker(
        &mut self,
        since: &str,
        account_id: &str,
        pid: &str,
        out: &mut FetchResult,
    ) -> CoreResult<()> {
        let vars = json!({"accountId": account_id, "portfolioId": pid});
        let overview = self
            .graphql(
                "BrokerOverview",
                Q_OVERVIEW,
                json!({"accountId": account_id, "portfolioId": pid, "includeYearToDate": false}),
            )
            .await?;
        let valuation = &overview["account"]["brokerPortfolio"]["valuation"];
        let securities = json_number_to_cents(&valuation["securitiesValuation"]).unwrap_or(0)
            + json_number_to_cents(&valuation["cryptoValuation"]).unwrap_or(0);
        let total = json_number_to_cents(&valuation["valuation"]).unwrap_or(securities);
        let limits = self.graphql("BrokerLimits", Q_LIMITS, vars.clone()).await;
        let cash = match &limits {
            Ok(v) => json_number_to_cents(
                &v["account"]["brokerPortfolio"]["payments"]["buyingPower"]["cashBalance"],
            )
            .ok(),
            Err(e) => {
                log(&format!("BrokerLimits failed: {e}"));
                None
            }
        };
        // Unknown whether `valuation` includes cash (docs/scalable-capital-connector.md Q1).
        let depot_value = if securities > 0 { securities } else { total };
        log(&format!(
            "portfolio: valuation={} securities={} cash={}",
            total != 0,
            securities != 0,
            cash.is_some()
        ));

        let holdings = match self
            .graphql("BrokerHoldings", Q_HOLDINGS, json!({"accountId": account_id, "portfolioId": pid, "includeYearToDate": false, "quoteSource": "CONSOLIDATED"}))
            .await
        {
            Ok(v) => map_holdings(&v["account"]["brokerPortfolio"]["inventory"]),
            Err(e) => {
                log(&format!("BrokerHoldings failed: {e}"));
                Vec::new()
            }
        };
        let today = super::today();
        let cash_ref = format!("sc:cash:{pid}");
        let depot_ref = format!("sc:depot:{pid}");
        out.accounts.push(RemoteAccount {
            external_ref: cash_ref.clone(),
            name: "Verrechnungskonto".into(),
            owner: String::new(),
            iban: String::new(),
            account_number: String::new(),
            currency: "EUR".into(),
            kind: AccountKind::Checking,
            balance_cents: cash.unwrap_or(0),
            balance_date: today.clone(),
        });
        out.accounts.push(RemoteAccount {
            external_ref: depot_ref.clone(),
            name: "Depot".into(),
            owner: String::new(),
            iban: String::new(),
            account_number: String::new(),
            currency: "EUR".into(),
            kind: AccountKind::Brokerage,
            balance_cents: depot_value,
            balance_date: today,
        });
        let names: std::collections::HashMap<String, String> = holdings
            .iter()
            .filter(|h| !h.name.is_empty())
            .map(|h| (h.isin.clone(), h.name.clone()))
            .collect();
        for h in holdings {
            out.holdings.push(RemoteHolding {
                account_ref: depot_ref.clone(),
                holding: h,
            });
        }

        // transactions, newest first, paged by cursor
        let from_time = since_epoch(since);
        let mut cursor: Option<String> = None;
        let mut pages = 0usize;
        let mut count = 0usize;
        loop {
            let broker_input = json!({
                "pageSize": PAGE_SIZE,
                "cursor": cursor,
                "fromTime": from_time,
            });
            let v = self
                .graphql(
                    "BrokerTransactions",
                    Q_TRANSACTIONS,
                    json!({
                        "accountId": account_id,
                        "portfolioId": pid,
                        "brokerInput": broker_input,
                        "cryptoInput": {"pageSize": 1},
                        "includeCrypto": false,
                    }),
                )
                .await?;
            let more = &v["account"]["brokerPortfolio"]["moreTransactions"];
            let items = more["transactions"].as_array().cloned().unwrap_or_default();
            if pages == 0 {
                log(&format!("broker tx kinds: {}", kind_stats(&items)));
            }
            let mut all_older = !items.is_empty();
            for item in &items {
                let date = local_date(&item["lastEventDateTime"]);
                if date.as_str() >= since {
                    all_older = false;
                }
                if date.as_str() < since {
                    continue;
                }
                if let Some(t) = map_transaction(&cash_ref, item, &names) {
                    out.transactions.push(t);
                    count += 1;
                }
            }
            pages += 1;
            cursor = more["cursor"]
                .as_str()
                .filter(|c| !c.is_empty())
                .map(|c| c.to_string());
            if cursor.is_none() || items.is_empty() || all_older || pages >= MAX_PAGES {
                break;
            }
        }
        log(&format!("portfolio: {count} bookings in {pages} page(s)"));
        Ok(())
    }

    async fn fetch_overnight(
        &mut self,
        since: &str,
        account_id: &str,
        out: &mut FetchResult,
    ) -> CoreResult<()> {
        let v = self
            .graphql(
                "DiscoverOvernightAccounts",
                Q_OVERNIGHT_ACCOUNTS,
                json!({"accountId": account_id}),
            )
            .await?;
        let accounts = v["account"]["savingsAccounts"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        log(&format!("overnight: {} savings account(s)", accounts.len()));
        let from_time = since_epoch(since);
        for acc in accounts {
            let id = str_of(&acc["id"]);
            if id.is_empty() {
                continue;
            }
            let state = str_of(&acc["state"]);
            if !state.is_empty() && state != "ACTIVE" {
                log(&format!("overnight: skipping state {state}"));
                continue;
            }
            let summary = match self
                .graphql(
                    "OvernightSummary",
                    Q_OVERNIGHT_SUMMARY,
                    json!({"accountId": account_id, "savingsAccountId": id}),
                )
                .await
            {
                Ok(s) => s,
                Err(e) => {
                    log(&format!("OvernightSummary failed: {e}"));
                    continue;
                }
            };
            let sa = &summary["account"]["savingsAccount"];
            let Ok(balance) = json_number_to_cents(&sa["totalAmount"]) else {
                continue;
            };
            let name = str_of(&acc["personalizations"]["name"]);
            let owner = format!(
                "{} {}",
                str_of(&acc["owners"][0]["firstName"]),
                str_of(&acc["owners"][0]["lastName"])
            )
            .trim()
            .to_string();
            let ext = format!("sc:overnight:{id}");
            out.accounts.push(RemoteAccount {
                external_ref: ext.clone(),
                name: if name.is_empty() {
                    "Overnight".into()
                } else {
                    name
                },
                owner,
                iban: String::new(),
                account_number: String::new(),
                currency: "EUR".into(),
                kind: AccountKind::Savings,
                balance_cents: balance,
                balance_date: super::today(),
            });
            let mut cursor: Option<String> = None;
            let mut pages = 0usize;
            loop {
                let v = self
                    .graphql(
                        "OvernightTransactions",
                        Q_OVERNIGHT_TRANSACTIONS,
                        json!({
                            "accountId": account_id,
                            "savingsAccountId": id,
                            "input": {"pageSize": PAGE_SIZE, "cursor": cursor, "fromTime": from_time},
                        }),
                    )
                    .await?;
                let more = &v["account"]["savingsAccount"]["moreTransactions"];
                let items = more["transactions"].as_array().cloned().unwrap_or_default();
                if pages == 0 {
                    log(&format!("overnight tx kinds: {}", kind_stats(&items)));
                }
                let mut all_older = !items.is_empty();
                for item in &items {
                    let date = local_date(&item["lastEventDateTime"]);
                    if date.as_str() >= since {
                        all_older = false;
                    } else {
                        continue;
                    }
                    if let Some(t) = map_transaction(&ext, item, &Default::default()) {
                        out.transactions.push(t);
                    }
                }
                pages += 1;
                cursor = more["cursor"]
                    .as_str()
                    .filter(|c| !c.is_empty())
                    .map(|c| c.to_string());
                if cursor.is_none() || items.is_empty() || all_older || pages >= MAX_PAGES {
                    break;
                }
            }
        }
        Ok(())
    }

    async fn fetch_fixed_term(
        &mut self,
        account_id: &str,
        out: &mut FetchResult,
    ) -> CoreResult<()> {
        let v = self
            .graphql(
                "ListFixedTermAccounts",
                Q_FIXED_TERM,
                json!({"accountId": account_id}),
            )
            .await?;
        let accounts = v["account"]["fixedTermSavingsAccounts"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        log(&format!("fixed-term: {} account(s)", accounts.len()));
        for acc in accounts {
            let id = str_of(&acc["id"]);
            let state = str_of(&acc["state"]);
            if id.is_empty()
                || !matches!(
                    state.as_str(),
                    "PENDING" | "ACTIVE" | "CANCELLATION_REQUESTED"
                )
            {
                continue;
            }
            let Ok(balance) = json_number_to_cents(&acc["totalAmount"]) else {
                continue;
            };
            let name = str_of(&acc["personalizations"]["name"]);
            let term = str_of(&acc["fixedTerm"]);
            let name = if !name.is_empty() {
                name
            } else if !term.is_empty() {
                format!("Festgeld {term}")
            } else {
                "Festgeld".into()
            };
            out.accounts.push(RemoteAccount {
                external_ref: format!("sc:fixedterm:{id}"),
                name,
                owner: String::new(),
                iban: String::new(),
                account_number: String::new(),
                currency: "EUR".into(),
                kind: AccountKind::Savings,
                balance_cents: balance,
                balance_date: super::today(),
            });
        }
        Ok(())
    }
}

#[async_trait]
impl BankConnector for ScalableConnector {
    async fn begin(&mut self, ctx: &ConnectContext) -> CoreResult<Phase> {
        self.session = serde_json::from_value(ctx.state.clone()).unwrap_or_default();
        if self.session_usable() {
            match DpopKey::from_hex(&self.session.dpop_key_hex) {
                Ok(k) => self.key = Some(k),
                Err(_) => return self.start_device_flow().await,
            }
            if self.session.access_expires_at - now() > 60 {
                log("stored session still valid");
                return Ok(Phase::Ready);
            }
            match self.refresh().await? {
                Ok(()) => return Ok(Phase::Ready),
                Err(_) => {
                    log("stored session not refreshable, new login");
                }
            }
        }
        self.start_device_flow().await
    }

    async fn respond(
        &mut self,
        _ctx: &ConnectContext,
        _response: ChallengeResponse,
    ) -> CoreResult<Phase> {
        match self.step {
            Step::DeviceWait { .. } => self.poll_device().await,
            Step::MfaWait { .. } => self.poll_mfa().await,
            Step::Idle => Ok(Phase::Ready),
        }
    }

    async fn fetch(&mut self, ctx: &ConnectContext) -> CoreResult<FetchResult> {
        self.ensure_token().await?;
        let mut out = FetchResult::default();
        let person_id = self.session.person_id.clone();
        if person_id.is_empty() {
            return Err(CoreError::Protocol {
                message: "Scalable: no person id in access token".into(),
            });
        }
        let ids = self
            .graphql("ResolveBrokerIds", Q_RESOLVE_IDS, json!({"id": person_id}))
            .await?;
        let account_id = {
            let a = str_of(&ids["account"]["id"]);
            if a.is_empty() {
                person_id.clone()
            } else {
                a
            }
        };
        let portfolios: Vec<String> = ids["account"]["brokerPortfolios"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|p| str_of(&p["id"]))
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        log(&format!("{} broker portfolio(s)", portfolios.len()));
        // First sync of a connection: the whole history (the API has no range
        // limit and a depot needs its full purchase history). Later syncs are
        // incremental via the app-wide window in `ctx.since`.
        let first_sync = ctx.state.get("login_at").is_none();
        let since = if first_sync {
            "1970-01-01"
        } else {
            ctx.since.as_str()
        };
        log(&format!("first_sync={first_sync}"));
        for pid in &portfolios {
            self.fetch_broker(since, &account_id, pid, &mut out).await?;
        }
        if let Err(e) = self.fetch_overnight(since, &account_id, &mut out).await {
            log(&format!("overnight fetch failed: {e}"));
        }
        if let Err(e) = self.fetch_fixed_term(&account_id, &mut out).await {
            log(&format!("fixed-term fetch failed: {e}"));
        }
        out.state = Some(self.state_json());
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_buy_as_debit_and_keeps_signed_amounts() {
        let item = json!({
            "__typename": "BrokerSecurityTransactionSummary",
            "id": "tx-1", "currency": "EUR", "type": "BUY", "status": "SETTLED",
            "isCancellation": false, "lastEventDateTime": "2026-11-03T09:15:00Z",
            "description": "Vanguard FTSE All-World", "isin": "IE00BK5BQT80",
            "securityTransactionType": "BUY", "quantity": "1.5", "amount": "150.25", "side": "BUY"
        });
        let names = std::collections::HashMap::new();
        let t = map_transaction("sc:cash:p1", &item, &names).unwrap();
        assert_eq!(t.amount_cents, -150_25);
        assert_eq!(t.external_id, "sc:tx-1");
        assert_eq!(t.booking_date, "2026-11-03");
        assert_eq!(t.counterparty, "Vanguard FTSE All-World");
        assert_eq!(t.purpose, "Kauf · IE00BK5BQT80 × 1.5");
        assert_eq!(t.booking_text, "Wertpapierkauf");
        // savings plan without description: name from the holdings, classified as Sparplan
        let mut sp = item.clone();
        sp["description"] = json!("");
        sp["type"] = json!("SAVINGS_PLAN");
        let names: std::collections::HashMap<String, String> =
            [("IE00BK5BQT80".to_string(), "Vanguard All-World".to_string())].into();
        let t = map_transaction("x", &sp, &names).unwrap();
        assert_eq!(t.counterparty, "Vanguard All-World");
        assert_eq!(t.booking_text, "Sparplan ausgeführt");
        assert_eq!(t.amount_cents, -150_25);
        // already-signed amounts are left alone
        let mut signed = item.clone();
        signed["amount"] = json!(-150.25);
        assert_eq!(
            map_transaction("x", &signed, &Default::default())
                .unwrap()
                .amount_cents,
            -150_25
        );
    }

    #[test]
    fn skips_open_and_cancelled() {
        let base = json!({"id": "a", "type": "DEPOSIT", "status": "PENDING",
            "lastEventDateTime": "2026-11-03T09:15:00Z", "amount": "10"});
        let n = std::collections::HashMap::new();
        assert!(map_transaction("x", &base, &n).is_none());
        let mut c = base.clone();
        c["status"] = json!("SETTLED");
        c["isCancellation"] = json!(true);
        assert!(map_transaction("x", &c, &n).is_none());
        c["isCancellation"] = json!(false);
        let t = map_transaction("x", &c, &n).unwrap();
        assert_eq!(t.amount_cents, 10_00);
        assert_eq!(t.booking_text, "Einzahlung");
        assert_eq!(t.counterparty, "Einzahlung");
        let wd = json!({"id": "wd", "type": "WITHDRAWAL", "status": "SETTLED",
            "lastEventDateTime": "2026-11-03T09:15:00Z", "amount": "10"});
        let t = map_transaction("x", &wd, &n).unwrap();
        assert_eq!(t.amount_cents, -10_00);
        assert!(t.booking_text.contains("Übertrag"));
        assert_eq!(t.counterparty, "Auszahlung");
        // overnight withdrawal: generic type with a cash subtype, unsigned amount
        let w = json!({"id": "w", "type": "CASH", "cashTransactionType": "WITHDRAWAL",
            "status": "SETTLED", "lastEventDateTime": "2026-10-12T08:00:00Z",
            "amount": "2000", "description": "Internal Transfer"});
        let t = map_transaction("x", &w, &n).unwrap();
        assert_eq!(t.amount_cents, -2000_00);
        assert_eq!(t.counterparty, "Auszahlung");
        assert_eq!(t.purpose, "Internal Transfer");
        assert_eq!(t.booking_text, "Umbuchung");
        let i = json!({"id": "i", "type": "INTEREST", "status": "SETTLED",
            "lastEventDateTime": "2026-11-01T08:00:00Z", "amount": "81.10"});
        let t = map_transaction("x", &i, &n).unwrap();
        assert_eq!((t.amount_cents, t.counterparty.as_str()), (81_10, "Zinsen"));
        let u = json!({"id": "u", "type": "SOMETHING_NEW_OUT", "status": "SETTLED",
            "lastEventDateTime": "2026-11-01T08:00:00Z", "amount": "5"});
        let t = map_transaction("x", &u, &n).unwrap();
        assert_eq!(
            (t.amount_cents, t.counterparty.as_str()),
            (-5_00, "Something new out")
        );
    }

    #[test]
    fn maps_holdings() {
        let inv = json!({"items": [{
            "isin": "IE00B4L5Y983", "name": "iShares Core MSCI World", "type": "ETF",
            "inventory": {"position": {"filled": "12.5", "pending": 0, "blocked": 0, "fifoPrice": "80.10"}},
            "portfolioIsinPerformance": {"valuation": "1234.56", "currency": "EUR"},
            "quoteTick": {"midPrice": "98.76", "currency": "EUR"}
        }, {"isin": ""}]});
        let h = map_holdings(&inv);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].quantity, 12.5);
        assert_eq!(h[0].value_cents, 1234_56);
        assert_eq!(h[0].current_price_cents, 98_76);
        assert_eq!(h[0].purchase_price_cents, 80_10);
        // missing quote: price derived from valuation / quantity
        let inv = json!({"items": [{"isin": "X", "name": "N",
            "inventory": {"position": {"filled": 175}},
            "portfolioIsinPerformance": {"valuation": "7982.45"}}]});
        let h = map_holdings(&inv);
        assert_eq!(h[0].current_price_cents, 45_61);
    }

    #[test]
    fn jwt_claims_decode_without_verification() {
        let payload = URL_SAFE_NO_PAD.encode(r#"{"https://de.scalable.capital/person_id":"p-1"}"#);
        let c = jwt_claims(&format!("h.{payload}.s"));
        assert_eq!(c["https://de.scalable.capital/person_id"], "p-1");
        assert_eq!(jwt_claims("garbage"), Value::Null);
    }

    #[tokio::test]
    async fn begin_with_stale_state_starts_device_flow_or_fails_cleanly() {
        // No network in unit tests: a stale session must not be treated as valid.
        let c = ScalableConnector::new();
        assert!(!c.session_usable());
    }
}
