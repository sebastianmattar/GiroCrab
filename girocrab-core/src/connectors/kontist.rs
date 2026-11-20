//! Kontist – official GraphQL API (kontist.dev).
//!
//!  POST /api/oauth/token                 grant_type=password (own OAuth2 client)
//!                                        or grant_type=refresh_token (stored)
//!  POST /api/user/mfa/challenges         push approval in the Kontist app
//!  GET  /api/user/mfa/challenges/{id}    PENDING → VERIFIED / DENIED
//!  POST /api/user/mfa/challenges/{id}/token  confirmed access + refresh token
//!  POST /api/graphql                     viewer.mainAccount + transactions
//!                                        (Relay paging, at most 50 per page)
//!
//! The user registers their own client on kontist.dev (client management:
//! name, redirect URI, scopes below; Kontist shows no grant types and usually
//! no secret, the password grant works for such a client). After the first approval in the app the confirmed refresh
//! token is kept in the (encrypted) connection state, so later syncs need no
//! approval until Kontist lets it expire. Only `https://api.kontist.com`
//! receives the password and the tokens.

use async_trait::async_trait;
use serde_json::{Value, json};

use super::{
    BankConnector, Challenge, ChallengeKind, ChallengeResponse, ConnectContext, FetchResult, Phase,
    RemoteAccount, RemoteTransaction,
};
use crate::error::{CoreError, CoreResult};
use crate::i18n::{tr, trf};
use crate::models::AccountKind;
use crate::secret::{SecretJson, SecretString};

const BASE: &str = "https://api.kontist.com";
const SCOPES: &str = "offline accounts transactions users";

/// What survives between syncs: the confirmed refresh token, so later syncs
/// need neither the password nor a new approval in the app.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct KontistState {
    #[serde(default)]
    refresh_token: SecretString,
}
/// Kontist caps `first` at 50.
const PAGE_SIZE: u32 = 50;
/// Stop after this many pages (25 000 bookings) instead of looping forever on
/// a cursor that does not move.
const MAX_PAGES: usize = 500;

const ACCOUNT_QUERY: &str = "query GiroCrabAccount { viewer { firstName lastName mainAccount { id iban bic name balance } } }";

const TRANSACTIONS_QUERY: &str = "query GiroCrabTransactions($first: Int, $after: String, $filter: TransactionFilter) { viewer { mainAccount { transactions(first: $first, after: $after, filter: $filter) { edges { node { id amount name iban purpose bookingDate valutaDate type paymentMethod e2eId mandateNumber } } pageInfo { hasNextPage endCursor } } } } }";

pub struct KontistConnector {
    client: Option<reqwest::Client>,
    access_token: SecretString,
    refresh_token: SecretString,
    /// Pending push approval (`/api/user/mfa/challenges/{id}`).
    challenge_id: Option<String>,
}

impl KontistConnector {
    pub fn new() -> Self {
        KontistConnector {
            client: None,
            access_token: SecretString::default(),
            refresh_token: SecretString::default(),
            challenge_id: None,
        }
    }

    fn client(&self) -> CoreResult<&reqwest::Client> {
        self.client.as_ref().ok_or(CoreError::SessionClosed)
    }

    /// Token endpoint. The client secret (if the client has one) goes as HTTP
    /// Basic auth, a public client sends only its id in the body.
    async fn token(&self, ctx: &ConnectContext, form: &[(&str, &str)]) -> CoreResult<Value> {
        let client_id = ctx.setting("kontist.client_id")?;
        let client_secret = ctx.setting_or("kontist.client_secret", "");
        let mut body: Vec<(&str, &str)> = form.to_vec();
        let mut req = self
            .client()?
            .post(format!("{BASE}/api/oauth/token"))
            .header("Accept", "application/json");
        if client_secret.is_empty() {
            body.push(("client_id", client_id));
        } else {
            req = req.basic_auth(client_id, Some(client_secret));
        }
        let resp = req.form(&body).send().await?;
        let status = resp.status();
        let v = super::read_json_or_null(resp).await;
        if status.is_success() && v["access_token"].is_string() {
            return Ok(v);
        }
        // Server trouble or rate limit: not a verdict on the credentials, so a
        // stored refresh token stays and nothing falls back to the password.
        if status.is_server_error() || status.as_u16() == 429 {
            return Err(CoreError::Network {
                message: trf(
                    "Kontist: Der Anmeldedienst antwortet nicht (HTTP {0}). Bitte später erneut versuchen.",
                    &[&status.as_u16()],
                ),
            });
        }
        Err(CoreError::Authentication {
            message: match v["error"].as_str() {
                Some("invalid_client" | "unauthorized_client") => tr("Kontist: Die OAuth2-Client-ID (oder das Secret) wurde abgelehnt. Bitte die Client-ID in den Einstellungen mit kontist.dev (Client Management) vergleichen; ohne angezeigtes Secret das Secret-Feld leer lassen.").into(),
                Some("invalid_grant") => tr("Kontist: Anmeldung fehlgeschlagen. Bitte E-Mail-Adresse und Passwort prüfen.").into(),
                Some("invalid_scope") => tr("Kontist: Dem OAuth2-Client fehlen Berechtigungen (benötigt: offline, accounts, transactions, users).").into(),
                _ => trf("Kontist: Anmeldung fehlgeschlagen (HTTP {0}).", &[&status.as_u16()]),
            },
        })
    }

    fn take_tokens(&mut self, v: &Value, access_key: &str) {
        self.access_token =
            SecretString::new(v[access_key].as_str().unwrap_or_default().to_string());
        if let Some(r) = v["refresh_token"].as_str().filter(|r| !r.is_empty()) {
            self.refresh_token = SecretString::new(r.to_string());
        }
    }

    /// Authenticated REST call to the MFA endpoints.
    async fn mfa(&self, post: bool, path: &str) -> CoreResult<Value> {
        let url = format!("{BASE}/api/user/mfa/challenges{path}");
        let client = self.client()?;
        let req = if post {
            client.post(url)
        } else {
            client.get(url)
        };
        let resp = req
            .bearer_auth(self.access_token.expose())
            .header("Accept", "application/json")
            .send()
            .await?;
        let status = resp.status();
        let v = super::read_json_or_null(resp).await;
        if !status.is_success() {
            return Err(CoreError::Bank {
                code: status.as_u16().to_string(),
                message: v["message"]
                    .as_str()
                    .unwrap_or(tr(
                        "Kontist: Die Freigabe in der App konnte nicht gestartet werden.",
                    ))
                    .to_string(),
            });
        }
        Ok(v)
    }

    async fn graphql(&self, query: &str, variables: Value) -> CoreResult<Value> {
        let resp = self
            .client()?
            .post(format!("{BASE}/api/graphql"))
            .bearer_auth(self.access_token.expose())
            .header("Accept", "application/json")
            .json(&json!({ "query": query, "variables": variables }))
            .send()
            .await?;
        let status = resp.status();
        let v = super::read_json_or_null(resp).await;
        if status.as_u16() == 401 || status.as_u16() == 403 {
            return Err(CoreError::Authentication {
                message: tr("Kontist: Zugriff verweigert. Bitte den Abruf erneut starten und in der Kontist-App freigeben.").into(),
            });
        }
        if let Some(msg) = graphql_error(&v) {
            return Err(CoreError::Bank {
                code: status.as_u16().to_string(),
                message: msg,
            });
        }
        if !status.is_success() || v["data"].is_null() {
            return Err(CoreError::Protocol {
                message: trf(
                    "Kontist: Unerwartete Antwort der API (HTTP {0}).",
                    &[&status.as_u16()],
                ),
            });
        }
        Ok(v)
    }
}

impl Default for KontistConnector {
    fn default() -> Self {
        Self::new()
    }
}

fn push_challenge() -> Challenge {
    Challenge {
        kind: ChallengeKind::DecoupledPush,
        title: tr("Kontist-App").into(),
        message: tr("Bitte die Anmeldung in der Kontist-App bestätigen.").into(),
        method_name: tr("Kontist-App").into(),
        redirect_url: None,
        image: None,
        expires_in_seconds: None,
        poll_interval_seconds: 3,
    }
}

/// First GraphQL error message, if any.
fn graphql_error(v: &Value) -> Option<String> {
    let first = v["errors"].as_array()?.first()?;
    Some(
        first["message"]
            .as_str()
            .unwrap_or("GraphQL error")
            .to_string(),
    )
}

/// `2026-11-03T00:00:00.000Z` → `2026-11-03`.
fn date_part(v: &Value) -> String {
    v.as_str()
        .filter(|s| s.len() >= 10)
        .map(|s| s[..10].to_string())
        .unwrap_or_default()
}

fn opt_str(v: &Value) -> String {
    v.as_str().unwrap_or_default().trim().to_string()
}

/// German booking text for the Kontist transaction `type`.
fn booking_text(kind: &str) -> String {
    match kind {
        "SEPA_CREDIT_TRANSFER" | "SEPA_INSTANT_CREDIT_TRANSFER" => tr("Überweisung").into(),
        "SEPA_DIRECT_DEBIT" | "DIRECT_DEBIT" => tr("Lastschrift").into(),
        "SEPA_DIRECT_DEBIT_RETURN" | "DIRECT_DEBIT_RETURN" => tr("Lastschriftrückgabe").into(),
        "CARD_TRANSACTION" | "CARD_USAGE" => tr("Kartenzahlung").into(),
        "ATM" => tr("Bargeldauszahlung").into(),
        "INTEREST_ACCRUED" => tr("Zinsen").into(),
        "CHARGE" => tr("Entgelt").into(),
        other => other.to_string(),
    }
}

/// `viewer` of [`ACCOUNT_QUERY`] → main account.
fn parse_account(viewer: &Value, today: &str) -> CoreResult<RemoteAccount> {
    let a = &viewer["mainAccount"];
    let iban = opt_str(&a["iban"]);
    let Some(balance) = a["balance"].as_i64() else {
        return Err(CoreError::Protocol {
            message: tr("Kontist: Kein Konto in der Antwort gefunden.").into(),
        });
    };
    if iban.is_empty() {
        return Err(CoreError::Protocol {
            message: tr("Kontist: Kein Konto in der Antwort gefunden.").into(),
        });
    }
    let owner = [opt_str(&viewer["firstName"]), opt_str(&viewer["lastName"])]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let name = opt_str(&a["name"]);
    Ok(RemoteAccount {
        external_ref: format!("kontist:{iban}"),
        name: if name.is_empty() {
            "Kontist".into()
        } else {
            name
        },
        owner,
        iban,
        account_number: String::new(),
        currency: "EUR".into(),
        kind: AccountKind::Checking,
        balance_cents: balance,
        balance_date: today.to_string(),
    })
}

/// One `Transaction` node. `amount` is signed cents (debits negative).
fn parse_transaction(n: &Value, account_ref: &str) -> Option<RemoteTransaction> {
    let id = opt_str(&n["id"]);
    let amount = n["amount"].as_i64()?;
    let booking_date = date_part(&n["bookingDate"]);
    if id.is_empty() || booking_date.is_empty() {
        return None;
    }
    let value_date = Some(date_part(&n["valutaDate"]))
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| booking_date.clone());
    let e2e = opt_str(&n["e2eId"]);
    Some(RemoteTransaction {
        account_ref: account_ref.to_string(),
        external_id: format!("kontist:{id}"),
        booking_date,
        value_date,
        amount_cents: amount,
        currency: "EUR".into(),
        counterparty: opt_str(&n["name"]),
        counterparty_iban: opt_str(&n["iban"]).replace(' ', ""),
        purpose: opt_str(&n["purpose"]),
        booking_text: booking_text(n["type"].as_str().unwrap_or_default()),
        end_to_end_id: if e2e == "NOTPROVIDED" {
            String::new()
        } else {
            e2e
        },
        mandate_id: opt_str(&n["mandateNumber"]),
        ..Default::default()
    })
}

#[async_trait]
impl BankConnector for KontistConnector {
    async fn begin(&mut self, ctx: &ConnectContext) -> CoreResult<Phase> {
        self.client = Some(
            super::client_builder(std::time::Duration::from_secs(60))
                .redirect(super::same_host_redirects())
                .build()?,
        );
        // A confirmed refresh token from an earlier sync: no password, no approval.
        let stored = ctx
            .state
            .decode::<KontistState>()
            .unwrap_or_default()
            .refresh_token;
        if !stored.is_empty() {
            let refreshed = self
                .token(
                    ctx,
                    &[
                        ("grant_type", "refresh_token"),
                        ("refresh_token", stored.expose()),
                    ],
                )
                .await;
            match refreshed {
                Ok(v) => {
                    self.refresh_token = stored;
                    self.take_tokens(&v, "access_token");
                    return Ok(Phase::Ready);
                }
                // Rejected (expired or revoked): log in again with the password.
                Err(CoreError::Authentication { .. }) => {}
                // Timeout, network, server: keep the stored token, try again later.
                Err(e) => return Err(e),
            }
        }
        if ctx.login_name.trim().is_empty() || ctx.secret.expose().is_empty() {
            return Err(CoreError::Authentication {
                message: tr("Kontist: E-Mail-Adresse und Passwort erforderlich.").into(),
            });
        }
        let v = self
            .token(
                ctx,
                &[
                    ("grant_type", "password"),
                    ("username", ctx.login_name.trim()),
                    ("password", ctx.secret.expose()),
                    ("scope", SCOPES),
                ],
            )
            .await?;
        self.take_tokens(&v, "access_token");
        let challenge = self.mfa(true, "").await?;
        let id = opt_str(&challenge["id"]);
        if id.is_empty() {
            return Err(CoreError::Protocol {
                message: tr("Kontist: Die Freigabe in der App konnte nicht gestartet werden.")
                    .into(),
            });
        }
        self.challenge_id = Some(id);
        Ok(Phase::Challenge(push_challenge()))
    }

    async fn respond(&mut self, _ctx: &ConnectContext, _r: ChallengeResponse) -> CoreResult<Phase> {
        let Some(id) = self.challenge_id.clone() else {
            return Ok(Phase::Ready);
        };
        let c = self.mfa(false, &format!("/{id}")).await?;
        let expired = c["expiresAt"]
            .as_str()
            .and_then(|s| s.parse::<jiff::Timestamp>().ok())
            .is_some_and(|t| t < jiff::Timestamp::now());
        match c["status"].as_str() {
            Some("VERIFIED") => {
                let t = self.mfa(true, &format!("/{id}/token")).await?;
                if !t["token"].is_string() {
                    return Err(CoreError::Protocol {
                        message: tr(
                            "Kontist: Die Freigabe in der App konnte nicht gestartet werden.",
                        )
                        .into(),
                    });
                }
                self.take_tokens(&t, "token");
                self.challenge_id = None;
                Ok(Phase::Ready)
            }
            Some("DENIED") => {
                self.challenge_id = None;
                Err(CoreError::Authentication {
                    message: tr("Kontist: Die Anmeldung wurde in der Kontist-App abgelehnt.")
                        .into(),
                })
            }
            _ if expired => {
                self.challenge_id = None;
                Err(CoreError::Authentication {
                    message: tr(
                        "Kontist: Die Freigabe ist abgelaufen. Bitte den Abruf erneut starten.",
                    )
                    .into(),
                })
            }
            _ => Ok(Phase::Pending(push_challenge())),
        }
    }

    async fn fetch(&mut self, ctx: &ConnectContext) -> CoreResult<FetchResult> {
        let today = super::today();
        let v = self.graphql(ACCOUNT_QUERY, json!({})).await?;
        let account = parse_account(&v["data"]["viewer"], &today)?;
        let since = ctx.since_for(&account.external_ref).to_string();
        let filter = json!({ "bookingDate_gte": format!("{since}T00:00:00.000Z") });

        let mut out = FetchResult::default();
        let mut after: Option<String> = None;
        let mut complete = false;
        for _ in 0..MAX_PAGES {
            let page = self
                .graphql(
                    TRANSACTIONS_QUERY,
                    json!({ "first": PAGE_SIZE, "after": after, "filter": filter }),
                )
                .await?;
            let conn = &page["data"]["viewer"]["mainAccount"]["transactions"];
            for edge in conn["edges"].as_array().into_iter().flatten() {
                if let Some(t) = parse_transaction(&edge["node"], &account.external_ref) {
                    out.transactions.push(t);
                }
            }
            if conn["pageInfo"]["hasNextPage"].as_bool() != Some(true) {
                complete = true;
                break;
            }
            let next = conn["pageInfo"]["endCursor"].as_str().map(str::to_string);
            if next.is_none() || next == after {
                break;
            }
            after = next;
        }
        if !complete {
            return Err(CoreError::Protocol {
                message: tr("Kontist: Die Umsatzliste wurde nicht vollständig geliefert. Bitte den Abruf erneut starten.").into(),
            });
        }
        out.balances_reported.push(account.external_ref.clone());
        out.accounts.push(account);
        if !self.refresh_token.expose().is_empty() {
            out.state = SecretJson::encode(&KontistState {
                refresh_token: self.refresh_token.clone(),
            })
            .ok();
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_main_account_with_owner() {
        let viewer = json!({
            "firstName": "Erika", "lastName": "Mustermann",
            "mainAccount": { "id": 1, "iban": "DE89370400440532013000", "bic": "SOBKDEB2XXX", "name": null, "balance": 123456 }
        });
        let a = parse_account(&viewer, "2026-11-17").unwrap();
        assert_eq!(a.external_ref, "kontist:DE89370400440532013000");
        assert_eq!(a.name, "Kontist");
        assert_eq!(a.owner, "Erika Mustermann");
        assert_eq!(a.balance_cents, 123456);
        assert!(parse_account(&json!({ "mainAccount": null }), "2026-11-17").is_err());
    }

    #[test]
    fn parses_transaction_fields() {
        let n = json!({
            "id": "abc-1", "amount": -1999, "name": "Beispiel Shop GmbH",
            "iban": "DE89 3704 0044 0532 0130 00", "purpose": "Rechnung 42",
            "bookingDate": "2026-11-03T00:00:00.000Z", "valutaDate": "2026-11-04T00:00:00.000Z",
            "type": "SEPA_DIRECT_DEBIT", "paymentMethod": "bank_account",
            "e2eId": "NOTPROVIDED", "mandateNumber": "M-1"
        });
        let t = parse_transaction(&n, "kontist:X").unwrap();
        assert_eq!(t.external_id, "kontist:abc-1");
        assert_eq!(t.amount_cents, -1999);
        assert_eq!(
            (t.booking_date.as_str(), t.value_date.as_str()),
            ("2026-11-03", "2026-11-04")
        );
        assert_eq!(t.counterparty_iban, "DE89370400440532013000");
        assert_eq!(t.booking_text, tr("Lastschrift"));
        assert_eq!(t.end_to_end_id, "");
        assert_eq!(t.mandate_id, "M-1");
    }

    #[test]
    fn transaction_without_valuta_uses_booking_date() {
        let n = json!({ "id": "x", "amount": 500, "bookingDate": "2026-01-02T10:00:00Z", "type": "CARD_TRANSACTION" });
        let t = parse_transaction(&n, "a").unwrap();
        assert_eq!(t.value_date, "2026-01-02");
        assert!(parse_transaction(&json!({ "id": "y", "amount": 1 }), "a").is_none());
    }

    #[test]
    fn graphql_error_message_is_reported() {
        let v = json!({ "errors": [{ "message": "Not authorized" }], "data": null });
        assert_eq!(graphql_error(&v).as_deref(), Some("Not authorized"));
        assert_eq!(graphql_error(&json!({ "data": {} })), None);
    }
}
