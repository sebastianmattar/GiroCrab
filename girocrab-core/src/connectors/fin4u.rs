//! Alte Leipziger (ALH Gruppe) – customer portal "fin4u" (app.fin4u.de).
//! Bausparverträge, Bauspardarlehen and other accounts shown in fin4u.
//!
//! The web app talks to a JSON API behind a Keycloak login; the endpoints
//! below are the ones the official web app (app.fin4u.de) uses:
//!
//!  POST /auth/realms/fin4u/protocol/openid-connect/token
//!       grant_type=password, client_id=fin4u-portal, username,
//!       password = base64(RSA-OAEP(SHA-256, MGF1-SHA-1) with the app's public key)
//!  GET  /api/20210906/bankaccounts/bank-account        -> { bankAccounts: [...], totalBalance }
//!  GET  /api/20210906/bankaccounts/bank-account/{id}                -> account incl. `additionalAttributes`
//!  GET  /api/20210906/bankaccounts/bank-account/{id}/transaction
//!  GET  /api/20210906/insurances/insurance/document/{documentReferenceId} -> PDF as base64 text
//!       (the documents are listed in `additionalAttributes`, urn FIN4U:DOCUMENTS)
//!  POST /auth/realms/fin4u/protocol/openid-connect/logout
//!
//! Only accounts delivered by ALH itself (`tenantDelivered`, or category
//! BAUSPARVERTRAG) are imported; bank accounts the user linked inside fin4u
//! via a third party would duplicate GiroCrab's own connections.
//!
//! Unofficial use of the portal API with the user's own credentials; it may
//! change without notice.

use async_trait::async_trait;
use serde_json::Value;

use super::{
    BankConnector, ChallengeResponse, ConnectContext, FetchResult, KnownDocuments, Phase,
    RemoteAccount, RemoteTransaction, http_client_same_host,
};
use crate::error::{CoreError, CoreResult};
use crate::i18n::{tr, trf};
use crate::iban::{normalize_iban, validate_iban};
use crate::models::AccountKind;
use crate::money::{format_cents, json_number_to_cents};
use crate::secret::SecretString;

const BASE: &str = "https://app.fin4u.de";
/// What the web app uses today. If fin4u changes them with a new release,
/// the connector reports that clearly (`interface_changed`).
const CLIENT_ID: &str = "fin4u-portal";
const API: &str = "/api/20210906";
const TOKEN_PATH: &str = "/auth/realms/fin4u/protocol/openid-connect/token";
const LOGOUT_PATH: &str = "/auth/realms/fin4u/protocol/openid-connect/logout";

/// Public key the fin4u web app encrypts the password with before login
/// (`encryptWithPublicKey`, RSA-OAEP, SHA-256 label hash, MGF1 with SHA-1).
const PASSWORD_PUBLIC_KEY: &str = "-----BEGIN PUBLIC KEY-----
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAtxy/uQQXfDTB3VxH8hSF
k5kqEtB0zpsdRztPYX/+E0pPnd4hD1G4hRH7+Ou1+1YdLjQGI85LwQZmpoVA/Qae
7dMO2j6ri23VFQrhoBWCp5cuifwJgMDUwc3pKhBL0IQ3443YhBgDZ9f2rv2tM0r3
j0ch3O1DvtzpM3I3Wv2k5RmISgVR/Srx8Ik8x88pWdd2Sy5xmSpdLxuJxgN4Zmm6
N/uNNIi061dW3WWWK8Ex1OIaXTN9lUxotGjspRu2FDdLUOwYz5pSA2oH0uRHu3dv
Kvxz6iV4YsGyYNyCKfR7qyY6G1keFNSX0SJUzTowCLIOJ6I/GGgRvcSDmzzMFmtu
6QIDAQAB
-----END PUBLIC KEY-----";

/// The portal no longer answers as expected: most likely a new fin4u release
/// changed the interface.
fn interface_changed(what: &str) -> CoreError {
    CoreError::protocol(trf(
        "fin4u hat unerwartet geantwortet ({0}). Wahrscheinlich wurde die Portal-Schnittstelle geändert; bitte GiroCrab aktualisieren.",
        &[&what],
    ))
}

/// Login failures that are no credential problem: the Keycloak client or
/// realm is gone (a new fin4u release). `Ok` = ordinary failure.
fn login_error_kind(status: u16, v: &Value) -> CoreResult<()> {
    match v["error"].as_str().unwrap_or_default() {
        "invalid_client" | "unauthorized_client" => {
            Err(interface_changed("Anmeldung: Client nicht zugelassen"))
        }
        _ if status == 404 => Err(interface_changed("Anmeldung: HTTP 404")),
        _ => Ok(()),
    }
}

/// Password login at fin4u's Keycloak; the token response on success.
async fn login(client: &reqwest::Client, username: &str, password: &str) -> CoreResult<Value> {
    let encrypted = SecretString::new(encrypt_password(password)?);
    let resp = client
        .post(format!("{BASE}{TOKEN_PATH}"))
        .form(&[
            ("grant_type", "password"),
            ("client_id", CLIENT_ID),
            ("username", username),
            ("password", encrypted.expose()),
        ])
        .send()
        .await?;
    let status = resp.status();
    let v = super::read_json_or_null(resp).await;
    if !status.is_success() {
        login_error_kind(status.as_u16(), &v)?;
        if matches!(status.as_u16(), 400 | 401) {
            let base = tr("Anmeldung bei fin4u fehlgeschlagen. Bitte Zugangsdaten prüfen.");
            return Err(CoreError::Authentication {
                message: match problem_message(&v) {
                    Some(reason) => format!("{base} ({reason})"),
                    None => base.into(),
                },
            });
        }
        return Err(CoreError::Bank {
            code: status.as_u16().to_string(),
            message: problem_message(&v).unwrap_or_else(|| "fin4u login error".into()),
        });
    }
    if v["access_token"].as_str().unwrap_or_default().is_empty() {
        return Err(CoreError::protocol(tr(
            "fin4u hat kein Zugriffstoken geliefert",
        )));
    }
    Ok(v)
}

/// Encrypts the password exactly like the web app (node-forge
/// `encrypt(utf8, "RSA-OAEP", {md: sha256, mgf1: {md: sha1}})`, base64).
pub(crate) fn encrypt_password(password: &str) -> CoreResult<String> {
    use base64::Engine;
    use rsa::pkcs8::DecodePublicKey;
    let key = rsa::PublicKey::from_public_key_pem(PASSWORD_PUBLIC_KEY)
        .map_err(|e| CoreError::protocol(format!("fin4u public key: {e}")))?;
    let cipher = oaep_encrypt(&key, password)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(cipher))
}

/// RSA-OAEP with SHA-256 and MGF1-SHA-1, as node-forge in the web app.
fn oaep_encrypt(key: &rsa::PublicKey, password: &str) -> CoreResult<Vec<u8>> {
    let padding = rsa::Oaep::<sha2::Sha256, sha1::Sha1>::new_with_mgf_hash();
    key.encrypt(&mut getrandom::SysRng, padding, password.as_bytes())
        .map_err(|e| CoreError::protocol(format!("fin4u password encryption: {e}")))
}

pub struct Fin4uConnector {
    client: Option<reqwest::Client>,
    access_token: SecretString,
    refresh_token: SecretString,
}

impl Fin4uConnector {
    pub fn new() -> Self {
        Fin4uConnector {
            client: None,
            access_token: SecretString::default(),
            refresh_token: SecretString::default(),
        }
    }

    async fn get(&self, path: &str) -> CoreResult<Value> {
        let client = self.client.as_ref().ok_or(CoreError::SessionClosed)?;
        let resp = client
            .get(format!("{BASE}{API}{path}"))
            .bearer_auth(self.access_token.expose())
            .header("Accept", "application/json")
            .send()
            .await?;
        let status = resp.status();
        let body = super::read_body_limited(resp, super::MAX_TEXT_BYTES).await?;
        let parsed = serde_json::from_slice::<Value>(&body);
        if status.is_success() {
            // An HTML page (maintenance, moved app) or garbage instead of JSON.
            return parsed.map_err(|_| interface_changed(&format!("{path}: keine JSON-Antwort")));
        }
        let v = parsed.unwrap_or(Value::Null);
        if status.as_u16() == 404 && problem_message(&v).is_none() {
            return Err(interface_changed(&format!("{path}: HTTP 404")));
        }
        if status.as_u16() == 401 {
            return Err(CoreError::Authentication {
                message: tr("fin4u-Sitzung abgelaufen. Bitte erneut abrufen.").into(),
            });
        }
        if !status.is_success() {
            return Err(CoreError::Bank {
                code: status.as_u16().to_string(),
                message: format!(
                    "{path}: {}",
                    problem_message(&v).unwrap_or_else(|| "fin4u error".into())
                ),
            });
        }
        Ok(v)
    }
}

impl Fin4uConnector {
    /// A portal document as PDF bytes. The web app reads the response as
    /// text and saves it as `data:application/pdf;base64,…`; raw PDF bytes
    /// are accepted as well.
    async fn document(&self, reference_id: &str) -> CoreResult<Vec<u8>> {
        let client = self.client.as_ref().ok_or(CoreError::SessionClosed)?;
        let resp = client
            .get(format!(
                "{BASE}{API}/insurances/insurance/document/{reference_id}"
            ))
            .bearer_auth(self.access_token.expose())
            .header("Accept", "application/octet-stream")
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(CoreError::Bank {
                code: status.as_u16().to_string(),
                message: "fin4u document download failed".into(),
            });
        }
        let body = super::read_body_limited(resp, super::MAX_DOC_BYTES).await?;
        Ok(pdf_from_body(&body))
    }

    /// All bookings of an account. The response is `{ items, total }`; if
    /// `total` says there are more, follow with `?offset=` until nothing
    /// new arrives (servers that ignore the parameter end the loop).
    async fn transactions_of(&self, id: &str) -> CoreResult<Vec<Value>> {
        let path = format!("/bankaccounts/bank-account/{id}/transaction");
        let first = self.get(&path).await?;
        let total = first["total"].as_u64().unwrap_or(0) as usize;
        let mut items = transaction_items(&first);
        let mut seen: std::collections::HashSet<String> =
            items.iter().map(|t| t.to_string()).collect();
        for _ in 0..50 {
            if items.is_empty() || items.len() >= total {
                break;
            }
            let Ok(next) = self.get(&format!("{path}?offset={}", items.len())).await else {
                break;
            };
            let new: Vec<Value> = transaction_items(&next)
                .into_iter()
                .filter(|t| seen.insert(t.to_string()))
                .collect();
            if new.is_empty() {
                break;
            }
            items.extend(new);
        }
        Ok(items)
    }
}

impl Default for Fin4uConnector {
    fn default() -> Self {
        Self::new()
    }
}

/// Text of a Keycloak error or RFC 7807 problem response.
fn problem_message(v: &Value) -> Option<String> {
    ["error_description", "detail", "title", "message", "error"]
        .iter()
        .find_map(|k| v[*k].as_str())
        .map(str::to_string)
}

/// Text field as one line: strings with line breaks or runs of spaces
/// (fixed-width SEPA purpose lines) and arrays of lines are joined.
fn str_of(v: &Value, key: &str) -> String {
    let raw = match &v[key] {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    };
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Counterparty = title in GiroCrab's booking list. For accounts delivered
/// by ALH the portal titles bookings `categoryTitle || counterAccountHolder
/// || purpose` ("Vermögenswirksame Leistung"); do the same. Without any of
/// them GiroCrab would build the title from the first purpose words
/// ("000/1234567/- Nachname"), so fall back to the bank / institute name.
fn counterparty_of(t: &Value, alh: bool) -> String {
    if alh {
        let category = str_of(t, "categoryTitle");
        if !category.is_empty() {
            return category;
        }
    }
    let holder = str_of(t, "counterAccountHolder");
    if !holder.is_empty() {
        return holder;
    }
    let bank = str_of(t, "bankName");
    if !bank.is_empty() {
        return bank;
    }
    "Alte Leipziger".into()
}

/// Raw text field for content ids: trimmed only, arrays ignored. Must stay
/// unchanged – existing external ids of bookings without `id` depend on it.
fn raw_text(v: &Value, key: &str) -> String {
    match &v[key] {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// First non-empty of the given text fields.
fn first_text(v: &Value, keys: &[&str]) -> String {
    keys.iter()
        .map(|k| str_of(v, k))
        .find(|s| !s.is_empty())
        .unwrap_or_default()
}

/// Amount as plain number/string, or as `{ value | amount, currency }`.
fn cents_of(v: &Value) -> Option<i64> {
    match v {
        Value::Number(_) | Value::String(_) => json_number_to_cents(v).ok(),
        Value::Object(_) => ["value", "amount"]
            .iter()
            .find_map(|k| json_number_to_cents(&v[*k]).ok()),
        _ => None,
    }
}

/// "2026-01-15", "2026-01-15T00:00:00Z" -> "2026-01-15".
/// Also accepts "15.01.2026".
fn date_of(v: &Value) -> Option<String> {
    let s = v.as_str()?.trim();
    let d = s.get(..10)?;
    jiff::civil::Date::strptime("%Y-%m-%d", d)
        .or_else(|_| jiff::civil::Date::strptime("%d.%m.%Y", d))
        .ok()
        .map(crate::dates::iso)
}

/// Booking objects of one transaction response: `{ items, total }` (what
/// the web app reads), or a bare array / `transactions` / `content`.
pub(crate) fn transaction_items(v: &Value) -> Vec<Value> {
    v["items"]
        .as_array()
        .or_else(|| v.as_array())
        .or_else(|| v["transactions"].as_array())
        .or_else(|| v["content"].as_array())
        .cloned()
        .unwrap_or_default()
}

pub(crate) fn account_kind(category: &str) -> AccountKind {
    match category {
        "GIROKONTO" => AccountKind::Checking,
        "KREDITKARTENKONTO" | "VISA" | "MASTERCARD" | "AMERICANEXPRESS" | "DINERSCLUB"
        | "SONSTIGEKARTE" => AccountKind::CreditCard,
        "KREDITKONTO" => AccountKind::Loan,
        "BAUSPARVERTRAG" | "TAGESGELDKONTO" | "SPARKONTO" | "FESTGELDKONTO" => AccountKind::Savings,
        _ => AccountKind::Checking,
    }
}

/// Accounts from `GET /bankaccounts/bank-account` (object with `bankAccounts` or a bare array).
pub(crate) fn parse_accounts(
    v: &Value,
    all_accounts: bool,
    today: &str,
) -> Vec<(String, RemoteAccount)> {
    let list = v["bankAccounts"]
        .as_array()
        .or_else(|| v.as_array())
        .cloned()
        .unwrap_or_default();
    list.iter()
        .filter(|a| {
            all_accounts
                || a["tenantDelivered"].as_bool().unwrap_or(false)
                || a["category"].as_str() == Some("BAUSPARVERTRAG")
        })
        .filter_map(|a| {
            let id = str_of(a, "id");
            if id.is_empty() {
                return None;
            }
            let category = str_of(a, "category");
            let kind = account_kind(&category);
            let iban = str_of(a, "iban").replace(' ', "");
            let number = str_of(a, "accountNumber");
            let mut name = str_of(a, "customTitle");
            if name.is_empty() {
                name = str_of(a, "title");
            }
            if name.is_empty() {
                name = if category == "BAUSPARVERTRAG" {
                    format!("Bausparvertrag {number}").trim().to_string()
                } else {
                    format!("Alte Leipziger {number}").trim().to_string()
                };
            }
            let mut balance = cents_of(&a["balance"]).unwrap_or(0);
            // fin4u may report loan balances as positive remaining debt.
            if kind == AccountKind::Loan && balance > 0 {
                balance = -balance;
            }
            let balance_date = date_of(&a["updatedAt"]).unwrap_or_else(|| today.to_string());
            let currency = match str_of(a, "currency") {
                c if c.len() == 3 => c,
                _ => "EUR".into(),
            };
            Some((
                id.clone(),
                RemoteAccount {
                    external_ref: if iban.is_empty() {
                        format!("fin4u:{id}")
                    } else {
                        iban.clone()
                    },
                    name,
                    owner: str_of(a, "accountHolder"),
                    iban,
                    account_number: number,
                    currency,
                    kind,
                    balance_cents: balance,
                    balance_date,
                },
            ))
        })
        .collect()
}

/// Number in German notation with `min..=max` decimals: 1234.5 -> "1.234,50".
fn german_number(x: f64, min: usize, max: usize) -> String {
    let mut s = format!("{:.*}", max, x.abs());
    if let Some(dot) = s.find('.') {
        let keep = s.trim_end_matches('0').len().max(dot + 1 + min);
        s.truncate(keep);
        if s.ends_with('.') {
            s.pop();
        }
    }
    let (int, frac) = s.split_once('.').unwrap_or((&s, ""));
    let mut grouped = String::new();
    for (i, ch) in int.chars().enumerate() {
        if i > 0 && (int.len() - i).is_multiple_of(3) {
            grouped.push('.');
        }
        grouped.push(ch);
    }
    let sign = if x < 0.0 && s.chars().any(|c| c.is_ascii_digit() && c != '0') {
        "-"
    } else {
        ""
    };
    if frac.is_empty() {
        format!("{sign}{grouped}")
    } else {
        format!("{sign}{grouped},{frac}")
    }
}

/// Display text of one portal attribute, formatted like the web app's
/// `app-show-simple-attribute` (`type`: MONEY_AMOUNT, PERCENT, DATE, …).
fn attribute_value(a: &Value) -> String {
    let v = &a["value"];
    let text = str_of(a, "value");
    let number = || match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().replace(',', ".").parse::<f64>().ok(),
        _ => None,
    };
    let plural = |one: &str, many: &str| {
        let n = number().unwrap_or(0.0);
        format!("{text} {}", if n == 1.0 { one } else { many })
    };
    match a["type"].as_str().unwrap_or_default() {
        "MONEY_AMOUNT" => cents_of(v)
            .map(|c| format_cents(c, "€".into()))
            .unwrap_or(text),
        "PERCENT" => number()
            .map(|x| format!("{} %", german_number(x, 2, 2)))
            .unwrap_or(text),
        "NUMBER" => number().map(|x| german_number(x, 0, 3)).unwrap_or(text),
        "DATE" => date_of(v)
            .and_then(|d| jiff::civil::Date::strptime("%Y-%m-%d", &d).ok())
            .map(|d| d.strftime("%d.%m.%Y").to_string())
            .unwrap_or(text),
        "YEARS" => plural("Jahr", "Jahre"),
        "MONTHS" => plural("Monat", "Monate"),
        _ => text,
    }
}

/// One line of fin4u's „Kontodetails“, formatted like the portal shows it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Detail {
    /// Heading of the group the line belongs to; empty = top level.
    pub section: String,
    pub label: String,
    pub value: String,
}

/// Contract details of an ALH account from its `additionalAttributes`
/// (what the portal shows under „Kontodetails“): simple attributes
/// `{ id, label, value, type }` and groups `{ label, attributes }`, nested.
/// Like the portal, attributes without id or value are skipped, as are
/// headlines, hints and rules.
pub(crate) fn parse_details(account: &Value) -> Vec<Detail> {
    fn walk(list: &Value, section: &str, out: &mut Vec<Detail>) {
        let Some(items) = list.as_array() else {
            return;
        };
        for a in items {
            if let Some(children) = a["attributes"].as_array().filter(|c| !c.is_empty()) {
                let label = str_of(a, "label");
                let sub = match (section.is_empty(), label.is_empty()) {
                    (_, true) => section.to_string(),
                    (true, false) => label,
                    (false, false) => format!("{section} › {label}"),
                };
                walk(&Value::Array(children.clone()), &sub, out);
                continue;
            }
            let skip = matches!(
                a["type"].as_str().unwrap_or_default(),
                "HEADLINE" | "HELPTEXT" | "HINT" | "HINT_WITH_ICON" | "HORIZONTAL_RULE"
            );
            if skip || str_of(a, "id").is_empty() {
                continue;
            }
            let label = str_of(a, "label");
            let value = attribute_value(a);
            if label.is_empty() || value.is_empty() {
                continue;
            }
            out.push(Detail {
                section: section.to_string(),
                label,
                value,
            });
        }
    }
    let mut out = Vec::new();
    walk(&account["additionalAttributes"], "", &mut out);
    out
}

/// Fills IBAN, account number and holder from the portal's details when
/// the account object lacks them (ALH Bausparverträge).
pub(crate) fn complete_from_details(account: &mut RemoteAccount, details: &[Detail]) {
    let find = |labels: &[&str]| {
        details
            .iter()
            .find(|d| labels.iter().any(|l| d.label.eq_ignore_ascii_case(l)))
            .map(|d| d.value.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    if account.iban.is_empty()
        && let Some(iban) = find(&["IBAN"])
    {
        let iban = normalize_iban(&iban);
        if validate_iban(&iban) {
            account.iban = iban;
        }
    }
    if account.account_number.is_empty()
        && let Some(n) = find(&["Vertragsnummer", "Kontonummer"])
    {
        account.account_number = n;
    }
    if account.owner.is_empty()
        && let Some(o) = find(&["Inhaber", "Kontoinhaber"])
    {
        account.owner = o;
    }
}

/// PDF bytes of a document response: raw, or base64 text (possibly a
/// JSON string or a data URL). Empty if neither yields a PDF.
fn pdf_from_body(body: &[u8]) -> Vec<u8> {
    use base64::Engine as _;
    if body.starts_with(b"%PDF") {
        return body.to_vec();
    }
    let text: String = String::from_utf8_lossy(body)
        .trim()
        .trim_matches('"')
        .trim_start_matches("data:application/pdf;base64,")
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '\\')
        .collect();
    match base64::engine::general_purpose::STANDARD.decode(text) {
        Ok(bytes) if bytes.starts_with(b"%PDF") => bytes,
        _ => Vec::new(),
    }
}

/// One document of „Dokumente einsehen“.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PortalDocument {
    pub reference_id: String,
    pub label: String,
    /// YYYY-MM-DD (`sendingDate`), may be empty.
    pub date: String,
}

/// Documents of an account from its `additionalAttributes` entry with urn
/// `FIN4U:DOCUMENTS`: `payload.years.{year}.processes[].documents[]`.
pub(crate) fn parse_documents(account: &Value) -> Vec<PortalDocument> {
    let Some(attr) = account["additionalAttributes"].as_array().and_then(|a| {
        a.iter()
            .find(|a| a["urn"].as_str() == Some("FIN4U:DOCUMENTS"))
    }) else {
        return Vec::new();
    };
    let mut out: Vec<PortalDocument> = Vec::new();
    let years = attr["payload"]["years"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    for year in years.values() {
        for process in year["processes"].as_array().into_iter().flatten() {
            for d in process["documents"].as_array().into_iter().flatten() {
                let reference_id = str_of(d, "documentReferenceId");
                if reference_id.is_empty() || out.iter().any(|o| o.reference_id == reference_id) {
                    continue;
                }
                let mut label = str_of(d, "label");
                if label.is_empty() {
                    label = "fin4u-Dokument".into();
                }
                out.push(PortalDocument {
                    reference_id,
                    label,
                    date: date_of(&d["sendingDate"]).unwrap_or_default(),
                });
            }
        }
    }
    out.sort_by(|a, b| a.date.cmp(&b.date).then(a.label.cmp(&b.label)));
    out
}

/// GiroCrab document type from the portal title.
fn document_type(label: &str) -> &'static str {
    let l = label.to_lowercase();
    if l.contains("kontoauszug") {
        "kontoauszug"
    } else if l.contains("steuer") {
        "steuer"
    } else if l.contains("abrechnung") {
        "abrechnung"
    } else {
        "postbox"
    }
}

/// File name: date and title, without path separators.
fn document_file_name(d: &PortalDocument) -> String {
    let title: String = d
        .label
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c == ':' || c.is_whitespace() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let base = if d.date.is_empty() {
        title
    } else {
        format!("{}_{title}", d.date)
    };
    if base.to_lowercase().ends_with(".pdf") {
        base
    } else {
        format!("{base}.pdf")
    }
}

/// Bookings from `GET /bankaccounts/bank-account/{id}/transaction`.
#[cfg(test)]
pub(crate) fn parse_transactions(
    v: &Value,
    account_ref: &str,
    currency: &str,
    since: &str,
) -> Vec<RemoteTransaction> {
    parse_transactions_for(v, account_ref, currency, since, false)
}

/// Like `parse_transactions`; `alh` = account delivered by ALH itself
/// (titles like the portal, see `counterparty_of`).
pub(crate) fn parse_transactions_for(
    v: &Value,
    account_ref: &str,
    currency: &str,
    since: &str,
    alh: bool,
) -> Vec<RemoteTransaction> {
    let list = transaction_items(v);
    let mut occurrences: std::collections::HashMap<String, u32> = Default::default();
    list.iter()
        .filter_map(|t| {
            let booking_date = date_of(&t["bookingDate"]).or_else(|| date_of(&t["valueDate"]))?;
            let amount_cents = cents_of(&t["amount"])?;
            // The web app falls back the same way: ALH's own bookings often
            // carry their text only in `bookingText`.
            let mut purpose =
                first_text(t, &["purpose", "bookingText", "reference", "categoryTitle"]);
            // A booking text that adds something ("Abschlussgebühr" next to a
            // contract number) is kept in front of the purpose.
            let booking_text = first_text(t, &["bookingText", "type"]);
            let bt = str_of(t, "bookingText");
            if !bt.is_empty() && !purpose.contains(&bt) {
                purpose = format!("{bt} – {purpose}");
            }
            let mut rt = RemoteTransaction {
                account_ref: account_ref.to_string(),
                external_id: String::new(),
                value_date: date_of(&t["valueDate"]).unwrap_or_else(|| booking_date.clone()),
                booking_date,
                amount_cents,
                currency: currency.to_string(),
                counterparty: counterparty_of(t, alh),
                counterparty_iban: str_of(t, "counterAccountIban").replace(' ', ""),
                purpose,
                booking_text,
                ..Default::default()
            };
            // fin4u's own booking `id` is not stable: it changes whenever
            // fin4u reloads the account from its source (which the detail
            // GET in `fetch` triggers), so every sync imported the bookings
            // again. Hash only the raw API fields instead: purpose and
            // counterparty shown in the list may be refined later without
            // changing the id.
            let raw_purpose = match raw_text(t, "purpose") {
                p if p.is_empty() => raw_text(t, "reference"),
                p => p,
            };
            let key = format!("{}|{}|{}", rt.booking_date, rt.amount_cents, raw_purpose);
            let n = occurrences.entry(key).or_insert(0);
            let stable = RemoteTransaction {
                counterparty: raw_text(t, "counterAccountHolder"),
                purpose: raw_purpose,
                ..rt.clone()
            };
            rt.external_id = format!("fin4u:{account_ref}:{}", stable.content_id(*n));
            *n += 1;
            Some(rt)
        })
        .filter(|t| t.booking_date.as_str() >= since)
        .collect()
}

#[async_trait]
impl BankConnector for Fin4uConnector {
    async fn begin(&mut self, ctx: &ConnectContext) -> CoreResult<Phase> {
        if ctx.login_name.trim().is_empty() || ctx.secret.expose().is_empty() {
            return Err(CoreError::Authentication {
                message: tr("Benutzername (E-Mail) und Passwort für fin4u erforderlich").into(),
            });
        }
        let client = http_client_same_host()?;
        let username = ctx.login_name.trim();
        let v = login(&client, username, ctx.secret.expose()).await?;
        self.access_token =
            SecretString::new(v["access_token"].as_str().unwrap_or_default().to_string());
        self.refresh_token =
            SecretString::new(v["refresh_token"].as_str().unwrap_or_default().to_string());
        self.client = Some(client);
        Ok(Phase::Ready)
    }

    async fn respond(
        &mut self,
        _ctx: &ConnectContext,
        _response: ChallengeResponse,
    ) -> CoreResult<Phase> {
        Ok(Phase::Ready)
    }

    async fn fetch(&mut self, ctx: &ConnectContext) -> CoreResult<FetchResult> {
        let list = self.get("/bankaccounts/bank-account").await?;
        if !list["bankAccounts"].is_array() && !list.is_array() {
            return Err(interface_changed("Kontoliste ohne „bankAccounts“"));
        }
        let alh_ids: std::collections::HashSet<String> = list["bankAccounts"]
            .as_array()
            .or_else(|| list.as_array())
            .into_iter()
            .flatten()
            .filter(|a| {
                a["tenantDelivered"].as_bool().unwrap_or(false)
                    || a["category"].as_str() == Some("BAUSPARVERTRAG")
            })
            .map(|a| str_of(a, "id"))
            .collect();
        let accounts = parse_accounts(&list, false, &super::today());
        let mut known = KnownDocuments::from_state(&ctx.state);
        let mut out = FetchResult::default();
        for (mut id, mut account) in accounts {
            let alh = alh_ids.contains(&id);
            // Like the web app's account page: GET the account first – fin4u
            // refreshes it from the source then – and only afterwards ask
            // for its bookings. A failed refresh keeps the list data.
            if let Ok(detail) = self.get(&format!("/bankaccounts/bank-account/{id}")).await {
                let details = parse_details(&detail);
                // „Dokumente einsehen“: download the ones not fetched before –
                // but only for accounts that are stored now: remembered
                // documents of a switched-off account would never come back.
                let documents = if ctx.skipped_accounts.contains(&account.external_ref)
                    || !ctx.documents.enabled()
                {
                    Vec::new()
                } else {
                    parse_documents(&detail)
                };
                for d in documents {
                    if known.contains(&d.reference_id) {
                        continue;
                    }
                    let Ok(bytes) = self.document(&d.reference_id).await else {
                        continue;
                    };
                    if bytes.is_empty() {
                        continue;
                    }
                    out.documents.push(crate::documents::RemoteDocument {
                        account_ref: account.external_ref.clone(),
                        booking_external_id: None,
                        title: d.label.clone(),
                        doc_type: document_type(&d.label).into(),
                        mime_type: "application/pdf".into(),
                        file_name: document_file_name(&d),
                        bytes,
                        source_url: format!("fin4u:document:{}", d.reference_id),
                        date: d.date.clone(),
                    });
                    known.insert(d.reference_id);
                }
                let refreshed = detail["reloadAccountDetailSuccessful"]
                    .as_bool()
                    .unwrap_or(true);
                let has_title = !str_of(&detail, "customTitle").is_empty()
                    || !str_of(&detail, "title").is_empty();
                if refreshed
                    && let Some((did, mut acc)) =
                        parse_accounts(&Value::Array(vec![detail]), true, &super::today())
                            .into_iter()
                            .next()
                {
                    // keep the stable reference from the list entry
                    acc.external_ref = account.external_ref.clone();
                    if !has_title {
                        acc.name = account.name.clone();
                    }
                    id = did;
                    account = acc;
                }
                // IBAN, contract number and holder of Bausparverträge are only
                // in the details; the external ref stays the list's.
                complete_from_details(&mut account, &details);
            }
            // A missing booking list must not hide the balance.
            // fin4u always returns the whole history, and Bauspar accounts
            // book rarely – a 90-day first sync would often show nothing.
            // Import everything; external ids keep re-syncs duplicate-free.
            // An interface change must not pass as "no bookings" though.
            let items = match self.transactions_of(&id).await {
                Ok(items) => items,
                Err(e @ CoreError::Protocol { .. }) => return Err(e),
                Err(_) => Vec::new(),
            };
            out.transactions.extend(parse_transactions_for(
                &Value::Array(items),
                &account.external_ref,
                &account.currency,
                "",
                alh,
            ));
            out.accounts.push(account);
        }
        out.state = known.to_state();
        Ok(out)
    }

    async fn finish(&mut self, _ctx: &ConnectContext) {
        if let Some(client) = &self.client {
            let _ = client
                .post(format!("{BASE}{LOGOUT_PATH}"))
                .form(&[
                    ("client_id", CLIENT_ID),
                    ("refresh_token", self.refresh_token.expose()),
                ])
                .send()
                .await;
        }
        self.client = None;
        self.access_token = SecretString::default();
        self.refresh_token = SecretString::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Invented fixtures shaped like the fin4u API – no real data.
    fn accounts_json() -> Value {
        json!({
            "totalBalance": 4321.10,
            "bankAccounts": [
                {"id": "a1", "title": "Bausparvertrag Classic", "category": "BAUSPARVERTRAG",
                 "accountNumber": "1234567", "balance": 4321.10, "tenantDelivered": true,
                 "accountHolder": "Erika Mustermann", "updatedAt": "2026-10-31T00:00:00Z"},
                {"id": "a2", "title": "", "category": "KREDITKONTO", "accountNumber": "7654321",
                 "balance": 12000.00, "tenantDelivered": true},
                {"id": "a3", "title": "Girokonto", "category": "GIROKONTO",
                 "iban": "DE89 3704 0044 0532 0130 00", "balance": 99.5, "tenantDelivered": false}
            ]
        })
    }

    #[test]
    fn parses_alh_accounts_only_by_default() {
        let a = parse_accounts(&accounts_json(), false, "2026-11-10");
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].1.kind, AccountKind::Savings);
        assert_eq!(a[0].1.balance_cents, 432_110);
        assert_eq!(a[0].1.balance_date, "2026-10-31");
        assert_eq!(a[0].1.external_ref, "fin4u:a1");
        assert_eq!(a[1].1.kind, AccountKind::Loan);
        assert_eq!(a[1].1.balance_cents, -1_200_000);
        assert_eq!(a[1].1.name, "Alte Leipziger 7654321");
        let all = parse_accounts(&accounts_json(), true, "2026-11-10");
        assert_eq!(all.len(), 3);
        assert_eq!(all[2].1.external_ref, "DE89370400440532013000");
        assert_eq!(all[2].1.balance_date, "2026-11-10");
    }

    #[test]
    fn parses_transactions_with_and_without_ids() {
        let v = json!([
            {"id": "t1", "bookingDate": "2026-10-01", "valueDate": "2026-10-01",
             "amount": 150.0, "purpose": "Sparrate", "counterAccountHolder": "Erika Mustermann",
             "counterAccountIban": "DE89 3704 0044 0532 0130 00", "type": "GUTSCHRIFT"},
            {"bookingDate": "2026-09-30T00:00:00", "amount": -12.5, "reference": "Kontogebühr"},
            {"bookingDate": "2026-09-30T00:00:00", "amount": -12.5, "reference": "Kontogebühr"},
            {"bookingDate": "2025-12-31", "amount": 1.0, "purpose": "alt"}
        ]);
        let t = parse_transactions(&v, "fin4u:a1", "EUR", "2026-01-01");
        assert_eq!(t.len(), 3);
        // fin4u's own id is ignored: it changes when fin4u reloads the account
        assert!(t[0].external_id.starts_with("fin4u:fin4u:a1:c"));
        let mut reloaded = v.clone();
        reloaded[0]["id"] = json!("t1-after-reload");
        let r = parse_transactions(&reloaded, "fin4u:a1", "EUR", "2026-01-01");
        assert_eq!(r[0].external_id, t[0].external_id);
        assert_eq!(t[0].amount_cents, 15_000);
        assert_eq!(t[0].counterparty_iban, "DE89370400440532013000");
        assert_eq!(t[1].purpose, "Kontogebühr");
        assert_eq!(t[1].booking_date, "2026-09-30");
        assert_ne!(t[1].external_id, t[2].external_id);
        let again = parse_transactions(&v, "fin4u:a1", "EUR", "2026-01-01");
        assert_eq!(t[2].external_id, again[2].external_id);
        // shape the web app reads: { items, total }
        let w = parse_transactions(
            &json!({"items": v, "total": 4}),
            "fin4u:a1",
            "EUR",
            "2026-01-01",
        );
        assert_eq!(w.len(), 3);
        let g = parse_transactions(
            &json!({"items": [{"bookingDate": "15.01.2026", "amount": 2.0}]}),
            "fin4u:a1",
            "EUR",
            "2026-01-01",
        );
        assert_eq!(g[0].booking_date, "2026-01-15");
        // wrapped variant
        let w = parse_transactions(&json!({"transactions": v}), "fin4u:a1", "EUR", "2026-01-01");
        assert_eq!(w.len(), 3);
    }

    #[test]
    fn password_is_rsa_oaep_base64() {
        use base64::Engine;
        let a = encrypt_password("geheim").unwrap();
        let b = encrypt_password("geheim").unwrap();
        let raw = base64::engine::general_purpose::STANDARD
            .decode(&a)
            .unwrap();
        assert_eq!(raw.len(), 256); // 2048-bit key
        assert_ne!(a, b); // OAEP is randomised
    }

    #[test]
    fn password_oaep_uses_sha256_with_mgf1_sha1() {
        let private = rsa::PrivateKey::new(&mut getrandom::SysRng, 1024).unwrap();
        let cipher = oaep_encrypt(&private.to_public_key(), "geheim").unwrap();
        let plain = private
            .decrypt(
                rsa::Oaep::<sha2::Sha256, sha1::Sha1>::new_with_mgf_hash(),
                &cipher,
            )
            .unwrap();
        assert_eq!(plain, b"geheim");
        // The server would reject MGF1-SHA-256.
        assert!(
            private
                .decrypt(
                    rsa::Oaep::<sha2::Sha256, sha2::Sha256>::new_with_mgf_hash(),
                    &cipher
                )
                .is_err()
        );
    }

    #[test]
    fn purpose_falls_back_and_joins_lines() {
        let v = json!({"items": [
            {"id": "1", "bookingDate": "2026-01-02", "amount": 5.0, "bookingText": "Zinsgutschrift"},
            {"id": "2", "bookingDate": "2026-01-03", "amount": -1.0,
             "purpose": ["Vertrag 1234567", "Abschluss 2025"], "bookingText": "Kontoführung"},
            {"id": "3", "bookingDate": "2026-01-04", "amount": 9.0,
             "purpose": "SVWZ+Sparrate    Januar\n2026", "bookingText": "Sparrate"}
        ]});
        let t = parse_transactions(&v, "fin4u:a1", "EUR", "");
        assert_eq!(t[0].purpose, "Zinsgutschrift");
        assert_eq!(t[0].counterparty, "Alte Leipziger");
        assert_eq!(t[0].booking_text, "Zinsgutschrift");
        assert_eq!(
            t[1].purpose,
            "Kontoführung – Vertrag 1234567 Abschluss 2025"
        );
        assert_eq!(t[2].purpose, "SVWZ+Sparrate Januar 2026");
    }

    #[test]
    fn alh_bookings_are_titled_like_the_portal() {
        let v = json!({"items": [
            {"id": "1", "bookingDate": "2026-11-01", "amount": 40.0,
             "categoryTitle": "Vermögenswirksame Leistung",
             "purpose": "000/1234567/ - MUSTERMANN, ERIKA -VL FUER MONAT 11/2026"}
        ]});
        let alh = parse_transactions_for(&v, "fin4u:a1", "EUR", "", true);
        assert_eq!(alh[0].counterparty, "Vermögenswirksame Leistung");
        assert!(alh[0].purpose.contains("VL FUER MONAT 11/2026"));
        let other = parse_transactions_for(&v, "fin4u:a1", "EUR", "", false);
        assert_eq!(other[0].counterparty, "Alte Leipziger");
    }

    #[test]
    fn content_ids_ignore_display_fields() {
        let plain =
            json!([{"bookingDate": "2026-09-30", "amount": -12.5, "reference": "Kontogebühr"}]);
        let rich = json!([{"bookingDate": "2026-09-30", "amount": -12.5, "reference": "Kontogebühr",
                           "bookingText": "Kontoführung", "categoryTitle": "Gebühren",
                           "bankName": "Alte Leipziger Bausparkasse"}]);
        let a = parse_transactions_for(&plain, "fin4u:a1", "EUR", "", false);
        let b = parse_transactions_for(&rich, "fin4u:a1", "EUR", "", true);
        assert_ne!(a[0].purpose, b[0].purpose);
        assert_ne!(a[0].counterparty, b[0].counterparty);
        assert_eq!(a[0].external_id, b[0].external_id);
    }

    #[test]
    fn contract_details_from_additional_attributes() {
        let detail = json!({"id": "a1", "additionalAttributes": [
            {"id": "1", "label": "Vertragsnummer", "value": "1234567", "type": "TEXT"},
            {"id": "h", "label": "Vertragsdaten", "value": "", "type": "HEADLINE"},
            {"label": "Vertrag", "attributes": [
                {"id": "2", "label": "Bausparsumme", "value": 50000, "type": "MONEY_AMOUNT"},
                {"id": "3", "label": "Guthabenzinssatz", "value": 0.1, "type": "PERCENT"},
                {"id": "4", "label": "Bewertungszahl", "value": 1234.5678, "type": "NUMBER"},
                {"id": "5", "label": "Abschlussdatum", "value": "2020-03-01T00:00:00Z", "type": "DATE"},
                {"id": "6", "label": "Laufzeit", "value": "1", "type": "YEARS"},
                {"id": "7", "label": "Leer", "value": null},
                {"label": "ohne id", "value": "x"}
            ]},
            {"label": "leere Gruppe", "attributes": []}
        ]});
        let d = parse_details(&detail);
        let rows: Vec<(&str, &str, &str)> = d
            .iter()
            .map(|d| (d.section.as_str(), d.label.as_str(), d.value.as_str()))
            .collect();
        assert_eq!(
            rows,
            vec![
                ("", "Vertragsnummer", "1234567"),
                ("Vertrag", "Bausparsumme", "50.000,00 €"),
                ("Vertrag", "Guthabenzinssatz", "0,10 %"),
                ("Vertrag", "Bewertungszahl", "1.234,568"),
                ("Vertrag", "Abschlussdatum", "01.03.2020"),
                ("Vertrag", "Laufzeit", "1 Jahr"),
            ]
        );
        assert!(parse_details(&json!({"id": "a1"})).is_empty());
    }

    #[test]
    fn details_complete_iban_number_and_holder() {
        let mut acc = parse_accounts(&accounts_json(), false, "2026-11-10")
            .into_iter()
            .next()
            .unwrap()
            .1;
        acc.owner.clear();
        let d = |label: &str, value: &str| Detail {
            section: String::new(),
            label: label.into(),
            value: value.into(),
        };
        complete_from_details(
            &mut acc,
            &[
                d("IBAN", "DE89 3704 0044 0532 0130 00"),
                d("Vertragsnummer", "999"),
                d("Inhaber", "Erika Mustermann"),
            ],
        );
        assert_eq!(acc.iban, "DE89370400440532013000");
        // a number from the account list is kept
        assert_eq!(acc.account_number, "1234567");
        assert_eq!(acc.owner, "Erika Mustermann");
        assert_eq!(acc.external_ref, "fin4u:a1");
    }

    #[test]
    fn documents_from_additional_attributes() {
        let detail = json!({"id": "a1", "additionalAttributes": [
            {"id": "1", "label": "IBAN", "value": "DE89370400440532013000"},
            {"urn": "FIN4U:DOCUMENTS", "payload": {"years": {
                "2025": {"processes": [{"id": "p1", "documents": [
                    {"id": "d1", "documentReferenceId": "ref-1", "label": "Jahreskontoauszug 2025",
                     "sendingDate": "2026-01-15T00:00:00Z", "read": false}
                ]}]},
                "2024": {"processes": [{"id": "p2", "documents": [
                    {"id": "d2", "documentReferenceId": "ref-2", "label": "Vertragsänderung/Tarif",
                     "sendingDate": "2024-05-02"},
                    {"id": "d3", "label": "ohne Referenz"}
                ]}]}
            }}}
        ]});
        let docs = parse_documents(&detail);
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].reference_id, "ref-2");
        assert_eq!(
            document_file_name(&docs[0]),
            "2024-05-02_Vertragsänderung_Tarif.pdf"
        );
        assert_eq!(document_type(&docs[1].label), "kontoauszug");
        assert_eq!(docs[1].date, "2026-01-15");
        // the documents entry is no detail line
        assert_eq!(parse_details(&detail).len(), 1);
        assert!(parse_documents(&json!({"id": "a1"})).is_empty());
    }

    #[test]
    fn document_body_raw_or_base64() {
        use base64::Engine as _;
        let pdf = b"%PDF-1.4 test".to_vec();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&pdf);
        assert_eq!(pdf_from_body(&pdf), pdf);
        assert_eq!(pdf_from_body(b64.as_bytes()), pdf);
        assert_eq!(pdf_from_body(format!("\"{b64}\"").as_bytes()), pdf);
        assert!(pdf_from_body(b"<html>").is_empty());
    }

    #[test]
    fn login_errors_name_interface_changes() {
        assert!(login_error_kind(400, &json!({"error": "invalid_grant"})).is_ok());
        assert!(login_error_kind(401, &json!({"error": "invalid_client"})).is_err());
        assert!(login_error_kind(404, &Value::Null).is_err());
    }

    #[test]
    fn amounts_in_objects() {
        assert_eq!(
            cents_of(&json!({"value": 1.5, "currency": "EUR"})),
            Some(150)
        );
        assert_eq!(cents_of(&json!("-3.10")), Some(-310));
        assert_eq!(cents_of(&Value::Null), None);
    }
}
