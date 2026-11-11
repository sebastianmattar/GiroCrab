//! PayPal – official REST "Transaction Search" API
//! (developer.paypal.com/docs/api/transaction-search/v1/).
//!
//!  POST /v1/oauth2/token (client credentials)  -> bearer token
//!  GET  /v1/reporting/balances?currency_code=ALL
//!  GET  /v1/reporting/transactions?start_date&end_date&fields=all&page_size=500&page=N
//!       (maximum window: 31 days, so history is requested month by month)
//!
//! No interactive 2FA: authentication uses the user's own REST app.

use async_trait::async_trait;
use chrono::{Duration, NaiveDate, Utc};
use serde_json::Value;

use super::{
    http_client, BankConnector, ChallengeResponse, ConnectContext, FetchResult, Phase,
    RemoteAccount, RemoteTransaction,
};
use crate::error::{CoreError, CoreResult};
use crate::models::AccountKind;
use crate::money::json_number_to_cents;
use crate::secret::SecretString;

pub struct PayPalConnector {
    client: Option<reqwest::Client>,
    base: String,
    token: SecretString,
}

impl PayPalConnector {
    pub fn new() -> Self {
        PayPalConnector {
            client: None,
            base: String::new(),
            token: SecretString::default(),
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
        let v: Value = resp.json().await.unwrap_or(Value::Null);
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
        self.base = if ctx.setting_or("paypal.environment", "live") == "sandbox" {
            "https://api-m.sandbox.paypal.com".into()
        } else {
            "https://api-m.paypal.com".into()
        };
        let client = http_client()?;
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
        let v: Value = resp.json().await.unwrap_or(Value::Null);
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
            out.accounts.push(RemoteAccount {
                external_ref: "paypal:EUR".into(),
                name: "PayPal".into(),
                owner: ctx.login_name.clone(),
                iban: String::new(),
                account_number: ctx.login_name.clone(),
                currency: "EUR".into(),
                kind: AccountKind::PaymentService,
                balance_cents: 0,
                balance_date: super::today(),
            });
        }
        let mut start = NaiveDate::parse_from_str(&ctx.since, "%Y-%m-%d")
            .unwrap_or_else(|_| Utc::now().date_naive() - Duration::days(90));
        let end_all = Utc::now().date_naive();
        while start <= end_all {
            let end = (start + Duration::days(30)).min(end_all);
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
            start = end + Duration::days(1);
        }
        // Only keep transactions for accounts we know.
        let known: Vec<String> = out
            .accounts
            .iter()
            .map(|a| a.external_ref.clone())
            .collect();
        out.transactions.retain(|t| known.contains(&t.account_ref));
        Ok(out)
    }
}
