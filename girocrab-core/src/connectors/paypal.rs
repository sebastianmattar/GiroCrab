//! PayPal – official REST "Transaction Search" API
//! (developer.paypal.com/docs/api/transaction-search/v1/).
//!
//!  POST /v1/oauth2/token (client credentials)  -> bearer token
//!  GET  /v1/reporting/balances?currency_code=ALL
//!  GET  /v1/reporting/transactions?start_date&end_date&fields=all&page_size=500&page=N
//!       (maximum window: 31 days, so history is requested month by month)
//!
//! No interactive 2FA: authentication uses the user's own REST app.
//!
//! With `paypal.api_signature` set, the classic NVP API is used instead
//! (API username/password/signature, see [`super::paypal_nvp`]); account refs
//! and transaction ids are the same, so switching keeps the stored bookings.

use async_trait::async_trait;
use jiff::civil::Date;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::paypal_nvp::{NvpClient, NvpCredentials};
use super::{
    BankConnector, ChallengeResponse, ConnectContext, FetchResult, Phase, RemoteAccount,
    RemoteTransaction, http_client_same_host,
};
use crate::error::{CoreError, CoreResult};
use crate::models::AccountKind;
use crate::money::json_number_to_cents;
use crate::secret::{SecretJson, SecretString};

pub struct PayPalConnector {
    client: Option<reqwest::Client>,
    base: String,
    token: SecretString,
    nvp: Option<NvpClient>,
}

/// Connector state of the classic NVP API.
#[derive(Default, Serialize, Deserialize)]
struct NvpState {
    /// Ids whose GetTransactionDetails were already looked up.
    #[serde(default)]
    nvp_details_done: Vec<String>,
}

/// True when the classic NVP API (API signature) is configured.
pub(crate) fn nvp_configured(settings: &std::collections::HashMap<String, SecretString>) -> bool {
    settings
        .get("paypal.api_signature")
        .is_some_and(|s| !s.expose().trim().is_empty())
}

impl PayPalConnector {
    pub fn new() -> Self {
        PayPalConnector {
            client: None,
            base: String::new(),
            token: SecretString::default(),
            nvp: None,
        }
    }

    async fn get(&self, path_and_query: &str) -> CoreResult<Value> {
        let client = self.client.as_ref().ok_or(CoreError::SessionClosed)?;
        let resp = client
            .get(format!("{}{}", self.base, path_and_query))
            .bearer_auth(self.token.expose())
            .header("Accept", "application/json")
            .send()
            .await?;
        let status = resp.status();
        let v: Value = super::read_json_or_null(resp).await;
        if !status.is_success() {
            return Err(CoreError::Bank {
                code: status.as_u16().to_string(),
                message: v["message"]
                    .as_str()
                    .or(v["error_description"].as_str())
                    .unwrap_or("PayPal error")
                    .to_string(),
            });
        }
        Ok(v)
    }
}

impl Default for PayPalConnector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl BankConnector for PayPalConnector {
    async fn begin(&mut self, ctx: &ConnectContext) -> CoreResult<Phase> {
        let sandbox = ctx.setting_or("paypal.environment", "live") == "sandbox";
        if nvp_configured(&ctx.settings) {
            let nvp = NvpClient::new(
                sandbox,
                NvpCredentials {
                    user: ctx.setting("paypal.api_username")?.trim().to_string(),
                    password: SecretString::new(ctx.setting("paypal.api_password")?.to_string()),
                    signature: SecretString::new(
                        ctx.setting("paypal.api_signature")?.trim().to_string(),
                    ),
                },
            )?;
            // Checks the credentials before the sync goes on.
            nvp.balances(&ctx.login_name).await?;
            self.nvp = Some(nvp);
            return Ok(Phase::Ready);
        }
        self.base = if sandbox {
            "https://api-m.sandbox.paypal.com".into()
        } else {
            "https://api-m.paypal.com".into()
        };
        let client = http_client_same_host()?;
        let resp = client
            .post(format!("{}/v1/oauth2/token", self.base))
            .basic_auth(
                ctx.setting("paypal.client_id")?,
                Some(ctx.setting("paypal.client_secret")?),
            )
            .header("Accept", "application/json")
            .form(&[("grant_type", "client_credentials")])
            .send()
            .await?;
        let status = resp.status();
        let v: Value = super::read_json_or_null(resp).await;
        if !status.is_success() {
            return Err(CoreError::Authentication {
                message: v["error_description"]
                    .as_str()
                    .unwrap_or("PayPal login failed")
                    .to_string(),
            });
        }
        self.token = SecretString::new(v["access_token"].as_str().unwrap_or_default().to_string());
        self.client = Some(client);
        Ok(Phase::Ready)
    }

    async fn respond(&mut self, _ctx: &ConnectContext, _r: ChallengeResponse) -> CoreResult<Phase> {
        Ok(Phase::Ready)
    }

    async fn fetch(&mut self, ctx: &ConnectContext) -> CoreResult<FetchResult> {
        let mut out = FetchResult::default();
        if let Some(nvp) = &self.nvp {
            out.accounts = nvp.balances(&ctx.login_name).await?;
            if out.accounts.is_empty() {
                out.accounts.push(fallback_account(ctx));
            }
            (out.transactions, out.withdrawn) = nvp.transactions(since_date(ctx)).await?;
            // Every search succeeded: the holds listed are all that are open in the period.
            out.hold_prefix = Some(super::paypal_nvp::HOLD_PREFIX.into());
            retain_known(&mut out);
            // One account (#1124): no details for bookings the vault drops,
            // their ids would count as done.
            let refs: Vec<String> = out
                .accounts
                .iter()
                .map(|a| a.external_ref.clone())
                .collect();
            if let Some(only) = ctx.only_among(&refs) {
                out.transactions.retain(|t| t.account_ref == only);
            }
            // Purpose from GetTransactionDetails, once per booking (ids kept in the state).
            let mut state: NvpState = ctx.state.decode().unwrap_or_default();
            nvp.fill_details(&mut out.transactions, &mut state.nvp_details_done)
                .await;
            let done = &mut state.nvp_details_done;
            done.drain(..done.len().saturating_sub(super::paypal_nvp::MAX_DONE_IDS));
            out.state = SecretJson::encode(&state).ok();
            return Ok(out);
        }
        let balances = self.get("/v1/reporting/balances?currency_code=ALL").await?;
        for b in balances["balances"].as_array().cloned().unwrap_or_default() {
            let cur = b["currency"].as_str().unwrap_or("EUR").to_string();
            out.accounts.push(RemoteAccount {
                external_ref: format!("paypal:{cur}"),
                name: if b["primary"].as_bool().unwrap_or(false) {
                    "PayPal".into()
                } else {
                    format!("PayPal {cur}")
                },
                owner: ctx.login_name.clone(),
                iban: String::new(),
                account_number: ctx.login_name.clone(),
                currency: cur,
                kind: AccountKind::PaymentService,
                balance_cents: json_number_to_cents(&b["total_balance"]["value"]).unwrap_or(0),
                balance_date: super::today(),
            });
        }
        if out.accounts.is_empty() {
            out.accounts.push(fallback_account(ctx));
        }
        let mut start = since_date(ctx);
        let end_all = crate::dates::utc_date(jiff::Timestamp::now());
        while start <= end_all {
            let end = (start + crate::dates::days(30)).min(end_all);
            let mut page = 1;
            loop {
                let q = format!(
                    "/v1/reporting/transactions?start_date={}T00:00:00-0000&end_date={}T23:59:59-0000&fields=all&page_size=500&page={page}",
                    start, end
                );
                let v = self.get(&q).await?;
                for d in v["transaction_details"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                {
                    let info = &d["transaction_info"];
                    let status = info["transaction_status"].as_str().unwrap_or("S");
                    if status == "D" || status == "V" {
                        continue; // denied / reversed
                    }
                    let cur = info["transaction_amount"]["currency_code"]
                        .as_str()
                        .unwrap_or("EUR")
                        .to_string();
                    let date = info["transaction_initiation_date"]
                        .as_str()
                        .unwrap_or("")
                        .chars()
                        .take(10)
                        .collect::<String>();
                    let payer = &d["payer_info"];
                    let name = payer["payer_name"]["alternate_full_name"]
                        .as_str()
                        .map(|s| s.to_string())
                        .or_else(|| payer["email_address"].as_str().map(|s| s.to_string()))
                        .or_else(|| d["shipping_info"]["name"].as_str().map(|s| s.to_string()))
                        .unwrap_or_default();
                    let items: Vec<String> = d["cart_info"]["item_details"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|i| i["item_name"].as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default();
                    let purpose = info["transaction_subject"]
                        .as_str()
                        .or(info["transaction_note"].as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| items.join(", "));
                    out.transactions.push(RemoteTransaction {
                        account_ref: format!("paypal:{cur}"),
                        external_id: info["transaction_id"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        booking_date: date.clone(),
                        value_date: date,
                        amount_cents: json_number_to_cents(&info["transaction_amount"]["value"])
                            .unwrap_or(0),
                        currency: cur,
                        counterparty: name,
                        counterparty_iban: String::new(),
                        purpose,
                        booking_text: info["transaction_event_code"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        ..Default::default()
                    });
                }
                let total_pages = v["total_pages"].as_u64().unwrap_or(1);
                if page as u64 >= total_pages {
                    break;
                }
                page += 1;
            }
            start = end + crate::dates::days(1);
        }
        retain_known(&mut out);
        Ok(out)
    }
}

fn since_date(ctx: &ConnectContext) -> Date {
    Date::strptime("%Y-%m-%d", &ctx.since)
        .unwrap_or_else(|_| crate::dates::utc_date(jiff::Timestamp::now()) - crate::dates::days(90))
}

fn fallback_account(ctx: &ConnectContext) -> RemoteAccount {
    RemoteAccount {
        external_ref: "paypal:EUR".into(),
        name: "PayPal".into(),
        owner: ctx.login_name.clone(),
        iban: String::new(),
        account_number: ctx.login_name.clone(),
        currency: "EUR".into(),
        kind: AccountKind::PaymentService,
        balance_cents: 0,
        balance_date: super::today(),
    }
}

/// Ensure every currency present in fetched transactions has a PayPal account
/// (even when its balance is 0 and omitted by GetBalance / /balances, #1110),
/// then keep only transactions belonging to known accounts.
fn retain_known(out: &mut FetchResult) {
    let owner = out
        .accounts
        .first()
        .map(|a| a.owner.clone())
        .unwrap_or_default();
    let account_number = out
        .accounts
        .first()
        .map(|a| a.account_number.clone())
        .unwrap_or_else(|| owner.clone());
    let balance_date = out
        .accounts
        .first()
        .map(|a| a.balance_date.clone())
        .unwrap_or_else(super::today);

    for t in &out.transactions {
        let Some(cur) = t.account_ref.strip_prefix("paypal:") else {
            continue;
        };
        let cur = cur.trim();
        if cur.is_empty() || out.accounts.iter().any(|a| a.external_ref == t.account_ref) {
            continue;
        }
        out.accounts.push(RemoteAccount {
            external_ref: t.account_ref.clone(),
            name: format!("PayPal {cur}"),
            owner: owner.clone(),
            iban: String::new(),
            account_number: account_number.clone(),
            currency: cur.to_string(),
            kind: AccountKind::PaymentService,
            balance_cents: 0,
            balance_date: balance_date.clone(),
        });
    }

    let known: Vec<String> = out
        .accounts
        .iter()
        .map(|a| a.external_ref.clone())
        .collect();
    out.transactions.retain(|t| known.contains(&t.account_ref));
}

#[cfg(test)]
mod currency_tests {
    use super::*;

    #[test]
    fn retain_known_creates_zero_balance_account_for_foreign_currency_transactions() {
        let mut out = FetchResult::default();
        out.accounts.push(RemoteAccount {
            external_ref: "paypal:EUR".into(),
            name: "PayPal".into(),
            owner: "alice@example.com".into(),
            iban: String::new(),
            account_number: "alice@example.com".into(),
            currency: "EUR".into(),
            kind: AccountKind::PaymentService,
            balance_cents: 50_00,
            balance_date: "2026-11-19".into(),
        });
        out.transactions.push(RemoteTransaction {
            account_ref: "paypal:USD".into(),
            external_id: "USD-PURCHASE-1".into(),
            booking_date: "2026-11-19".into(),
            value_date: "2026-11-19".into(),
            amount_cents: -11_90,
            currency: "USD".into(),
            counterparty: "US Merchant".into(),
            ..Default::default()
        });
        retain_known(&mut out);
        assert_eq!(out.transactions.len(), 1);
        let usd = out
            .accounts
            .iter()
            .find(|a| a.external_ref == "paypal:USD")
            .unwrap();
        assert_eq!(usd.currency, "USD");
        assert_eq!(usd.balance_cents, 0);
    }
}
