//! PSD2 accounts through the Enable Banking aggregation API
//! (enablebanking.com/docs/api/reference).
//!
//! Two modes share this connector:
//!  * **Card mode** – American Express / Amazon Visa (`ConnectorKind::AmericanExpress`,
//!    `AmazonVisa`; the ASPSP name comes from a setting). Behaviour is frozen:
//!    accounts keyed by session uid, card balance logic, 90-day consent.
//!  * **Generic mode** – any bank from `GET /aspsps` (`ConnectorKind::EnableBanking`,
//!    key `enablebanking:<CC>:<ASPSP>`), e.g. C24, N26. Accounts are typed from
//!    `cash_account_type`, keyed by IBAN / `identification_hash` (stable across
//!    re-consents), consent as long as the bank allows (≤ 180 days).
//!
//!  Auth: every request carries a JWT (RS256) signed with the user's own
//!        application key: header kid=<application id>,
//!        claims iss="enablebanking.com", aud="api.enablebanking.com".
//!  GET  /aspsps?country=CC         -> exact name, maximum_consent_validity
//!  POST /auth      {access{valid_until,balances,transactions}, aspsp{name,country}, state, redirect_url, psu_type}
//!                  -> {url}   (user gives consent at the bank)
//!  POST /sessions  {code}     -> {session_id, accounts[]}
//!  GET  /sessions/{id}        -> status (AUTHORIZED / EXPIRED / REVOKED …)
//!  GET  /accounts/{uid}/details | /balances
//!  GET  /accounts/{uid}/transactions?date_from=…|strategy=longest[&continuation_key=…]
//!
//! The PSD2 session is stored (encrypted) and reused until it expires, so the
//! browser consent is only needed every ~90-180 days.

use async_trait::async_trait;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};

use super::{
    BankConnector, Challenge, ChallengeKind, ChallengeResponse, ConnectContext, FetchResult, Paged,
    Phase, RemoteAccount, RemoteTransaction, http_client_same_host, paginate,
};
use crate::error::{CoreError, CoreResult};
use crate::i18n::{tr, trf};
use crate::models::AccountKind;
use crate::money::json_number_to_cents;
use crate::secret::{SecretJson, SecretString};
use crate::vault::Vault;
use std::collections::HashMap;
use std::sync::Arc;

const API: &str = "https://api.enablebanking.com";
/// Page limits of a transaction listing; the pending one is only summed
/// (card balance), so it spends less of the bank's daily request quota.
const TX_MAX_PAGES: usize = 100;
const PENDING_MAX_PAGES: usize = 20;
/// external_id prefix of pending (not yet booked) bookings in generic mode.
const HOLD_PREFIX: &str = "pending-hold-eb-";
/// Upper bound for a consent, whatever the bank reports.
const MAX_CONSENT_SECS: i64 = 180 * 86_400;
/// Consent length when the bank's maximum is unknown (and always in card mode).
const DEFAULT_CONSENT_DAYS: i64 = 90;

#[derive(Serialize)]
struct Claims<'a> {
    iss: &'a str,
    aud: &'a str,
    iat: i64,
    exp: i64,
}

/// One account of the PSD2 session.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct EbAccount {
    /// Session-scoped account id for API calls.
    uid: String,
    /// Key stored in the vault (`accounts.external_ref`).
    #[serde(default)]
    ext_ref: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    iban: String,
    #[serde(default)]
    currency: String,
    /// CACC / SVGS / CARD / LOAN / CASH / OTHR (empty = unknown).
    #[serde(default)]
    cash_account_type: String,
    #[serde(default)]
    identification_hash: String,
    /// BBAN / other account id – for accounts without IBAN (e.g. C24 savings).
    #[serde(default)]
    account_number: String,
    /// Bank's product label and PRIV/ORGA usage (used to recognise account kinds).
    #[serde(default)]
    product: String,
    #[serde(default)]
    usage: String,
    /// Account holder(s) as returned in `name` (C24: the holder, not an
    /// account name) and the account description set by PSU / bank.
    #[serde(default)]
    holder: String,
    #[serde(default)]
    details: String,
    /// Full `AccountResource` data was read (session object or `/details`).
    #[serde(default)]
    details_done: bool,
    /// The longest available history was fetched once (generic mode).
    #[serde(default)]
    history_done: bool,
    /// Read from the old `(uid, name, iban, currency)` state: keyed by uid,
    /// needs details + a stable key (generic mode).
    #[serde(skip)]
    legacy: bool,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StoredAccount {
    // Legacy first: serde would also read a JSON array into the struct
    // (positionally), mixing up uid / name / iban.
    Legacy((String, String, String, String)),
    Full(EbAccount),
}

fn de_accounts<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<EbAccount>, D::Error> {
    let raw = Vec::<StoredAccount>::deserialize(d)?;
    Ok(raw
        .into_iter()
        .map(|s| match s {
            StoredAccount::Full(a) => a,
            StoredAccount::Legacy((uid, name, iban, currency)) => EbAccount {
                ext_ref: uid.clone(),
                uid,
                name,
                iban,
                currency,
                // the old connector already imported the history
                history_done: true,
                legacy: true,
                ..Default::default()
            },
        })
        .collect())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PsdState {
    // Optional: "Neu verbinden" / "Sitzung löschen" remove both from the
    // stored state; the account list must still be read (reconnect compare).
    #[serde(default)]
    session_id: SecretString,
    #[serde(default)]
    valid_until: String,
    #[serde(default, deserialize_with = "de_accounts")]
    accounts: Vec<EbAccount>,
    /// `(old, new)` external refs still to be applied to the vault.
    #[serde(default)]
    renames: Vec<(String, String)>,
    /// External refs of accounts no longer in the consent, still to be
    /// marked as revoked in the vault.
    #[serde(default)]
    revoked: Vec<String>,
    /// Bank name from the consent (`aspsp.name`), e.g. "C24".
    #[serde(default)]
    aspsp_name: String,
}

pub struct EnableBankingConnector {
    /// Setting that may override the ASPSP name; empty = fixed bank (generic kind).
    aspsp_setting: &'static str,
    aspsp_default: String,
    country_setting: &'static str,
    country_default: String,
    client: Option<reqwest::Client>,
    jwt: String,
    expected_state: String,
    /// Sanitized `/auth` context (matched ASPSP + request), for error messages.
    auth_diag: String,
    st: PsdState,
}

/// Vault key of an account: card mode keeps the session uid (unchanged
/// behaviour); generic mode uses IBAN, else `identification_hash`, else uid.
/// Currency wallets sharing one IBAN (Finom EUR/USD) get ":<currency>"
/// appended, so `all` (the consent's accounts) is needed to spot them.
fn stable_ref(a: &EbAccount, all: &[EbAccount], card_mode: bool) -> String {
    if card_mode {
        return a.uid.clone();
    }
    let key = [&a.iban, &a.identification_hash, &a.uid]
        .into_iter()
        .map(|s| s.trim())
        .find(|s| !s.is_empty())
        .unwrap_or_default()
        .replace(' ', "");
    let shared_iban = !a.iban.trim().is_empty()
        && all
            .iter()
            .any(|o| o.uid != a.uid && same_iban(o, a) && o.currency != a.currency);
    if shared_iban {
        format!("{key}:{}", a.currency)
    } else {
        key
    }
}

/// Same IBAN (spaces ignored).
fn same_iban(p: &EbAccount, n: &EbAccount) -> bool {
    !p.iban.is_empty() && p.iban.replace(' ', "") == n.iban.replace(' ', "")
}

/// Same currency when both are known: wallets in other currencies under one
/// IBAN are different accounts.
fn same_currency(p: &EbAccount, n: &EbAccount) -> bool {
    p.currency.is_empty() || n.currency.is_empty() || p.currency == n.currency
}

/// GiroCrab account kind from the PSD2 `cash_account_type`.
/// Product labels of savings accounts that banks report as `CACC` anyway
/// (C24 "PocketZINS Smart" comes as CACC without IBAN).
const SAVINGS_PRODUCT_WORDS: [&str; 7] = [
    "tagesgeld",
    "festgeld",
    "sparkonto",
    "sparbuch",
    "savings",
    "pocketzins",
    "zinskonto",
];

/// Account kind of a generic-mode account: the type code, corrected by the
/// product label for savings accounts reported as current/other accounts.
fn kind_of(a: &EbAccount) -> AccountKind {
    let kind = account_kind(&a.cash_account_type, false);
    if kind == AccountKind::Checking {
        let label = format!("{} {}", a.product, a.name).to_lowercase();
        if SAVINGS_PRODUCT_WORDS.iter().any(|w| label.contains(w)) {
            return AccountKind::Savings;
        }
    }
    kind
}

fn account_kind(cash_account_type: &str, card_mode: bool) -> AccountKind {
    if card_mode {
        return AccountKind::CreditCard;
    }
    match cash_account_type.to_ascii_uppercase().as_str() {
        "CARD" => AccountKind::CreditCard,
        "SVGS" => AccountKind::Savings,
        "LOAN" => AccountKind::Loan,
        _ => AccountKind::Checking,
    }
}

/// Maps a previous session's accounts onto a new session's: by IBAN, by
/// identification hash, or 1:1 when both sessions have a single account.
/// Returns the vault renames and carries `history_done` over.
fn carry_over(prev: &[EbAccount], new: &mut [EbAccount]) -> Vec<(String, String)> {
    let mut renames = Vec::new();
    let single = prev.len() == 1 && new.len() == 1;
    for n in new.iter_mut() {
        let p = prev.iter().find(|p| same_account(p, n));
        let p = p.or(if single { prev.first() } else { None });
        if let Some(p) = p {
            n.history_done |= p.history_done;
            if !p.ext_ref.is_empty() && p.ext_ref != n.ext_ref {
                renames.push((p.ext_ref.clone(), n.ext_ref.clone()));
            }
        }
    }
    renames
}

/// Name shown for a generic-mode account: the account description, else the
/// bank's product ("C24 PocketZINS Smart"), else the holder. Enable Banking's
/// `name` is the account holder, so on its own it can't tell accounts apart.
fn display_name(a: &EbAccount) -> String {
    [&a.details, &a.product, &a.holder, &a.name]
        .into_iter()
        .map(|s| s.trim())
        .find(|s| !s.is_empty())
        .unwrap_or_default()
        .to_string()
}

fn paypal_account_label<'a>(
    aspsp: &str,
    account_count: usize,
    login_name: &'a str,
) -> Option<&'a str> {
    let label = login_name.trim();
    (aspsp.eq_ignore_ascii_case("PayPal")
        && account_count == 1
        && !label.is_empty()
        && !label.eq_ignore_ascii_case(aspsp)
        && !crate::demo::is_magic_demo_login(label))
    .then_some(label)
}

/// Same account in two sessions: by IBAN or identification hash, in the
/// same currency.
fn same_account(p: &EbAccount, n: &EbAccount) -> bool {
    same_currency(p, n)
        && (same_iban(p, n)
            || (!p.identification_hash.is_empty()
                && p.identification_hash == n.identification_hash))
}

/// Accounts of the previous consent that the new one no longer includes
/// (their vault refs). Uses the same matching as `carry_over`.
fn dropped_refs(prev: &[EbAccount], new: &[EbAccount]) -> Vec<String> {
    if prev.len() == 1 && new.len() == 1 {
        return Vec::new();
    }
    prev.iter()
        .filter(|p| !p.ext_ref.is_empty() && !new.iter().any(|n| same_account(p, n)))
        .map(|p| p.ext_ref.clone())
        .collect()
}

/// Non-IBAN account id of an `AccountResource`: `account_id.other`, else a
/// BBAN from `all_account_ids` (accounts without IBAN, e.g. C24 savings).
fn other_account_id(a: &Value) -> String {
    let clean = |v: &Value| {
        v.as_str()
            .unwrap_or_default()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
    };
    let iban = clean(&a["account_id"]["iban"]);
    let direct = clean(&a["account_id"]["other"]["identification"]);
    if !direct.is_empty() && direct != iban {
        return direct;
    }
    a["all_account_ids"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|x| {
            x["scheme_name"]
                .as_str()
                .is_some_and(|s| s.eq_ignore_ascii_case("BBAN"))
        })
        .map(|x| clean(&x["identification"]))
        .find(|x| !x.is_empty() && *x != iban)
        .unwrap_or_default()
}

/// Error code of an Enable Banking error body (`{"error": "EXPIRED_SESSION", …}`).
fn error_code(v: &Value) -> &str {
    v["error"].as_str().unwrap_or_default()
}

/// Session gone at the bank or at Enable Banking: a new consent is needed.
fn is_session_error(code: &str) -> bool {
    matches!(
        code,
        "EXPIRED_SESSION"
            | "REVOKED_SESSION"
            | "CLOSED_SESSION"
            | "WRONG_SESSION_STATUS"
            | "SESSION_DOES_NOT_EXIST"
            | "ASPSP_PSU_ACTION_REQUIRED"
    )
}

const RATE_LIMIT: &str = "ASPSP_RATE_LIMIT_EXCEEDED";

fn is_rate_limit(e: &CoreError) -> bool {
    matches!(e, CoreError::Bank { code, .. } if code == RATE_LIMIT)
}

fn is_account_unavailable(e: &CoreError) -> bool {
    matches!(e, CoreError::Bank { code, .. }
        if code == "ASPSP_ACCOUNT_NOT_ACCESSIBLE" || code == "ACCOUNT_DOES_NOT_EXIST")
}

/// Turns a non-2xx Enable Banking answer into a `CoreError`.
fn api_error(status: u16, v: &Value) -> CoreError {
    let msg = v["message"]
        .as_str()
        .or(v["detail"].as_str())
        .unwrap_or("Enable Banking error")
        .to_string();
    let code = error_code(v);
    if is_session_error(code) {
        return CoreError::Authentication {
            message: tr(
                "PSD2-Zustimmung abgelaufen oder widerrufen – bitte die Verbindung neu verbinden",
            )
            .into(),
        };
    }
    if code == RATE_LIMIT {
        return CoreError::Bank {
            code: RATE_LIMIT.into(),
            message: tr(
                "Abruflimit der Bank für heute erreicht – bitte später erneut synchronisieren",
            )
            .into(),
        };
    }
    if code == "ASPSP_ACCOUNT_NOT_ACCESSIBLE" || code == "ACCOUNT_DOES_NOT_EXIST" {
        return CoreError::Bank {
            code: code.into(),
            message: msg,
        };
    }
    if status == 401 || status == 403 {
        CoreError::Authentication { message: msg }
    } else {
        CoreError::Bank {
            code: status.to_string(),
            message: msg,
        }
    }
}

/// `maximum_consent_validity` (seconds) of `name` in a `GET /aspsps` answer.
fn max_consent_secs(list: &Value, name: &str) -> Option<i64> {
    list["aspsps"].as_array()?.iter().find(|b| {
        b["name"]
            .as_str()
            .is_some_and(|n| n.eq_ignore_ascii_case(name))
    })?["maximum_consent_validity"]
        .as_i64()
        .filter(|s| *s > 0)
}

/// `psu_type` for the consent of `name` in a `GET /aspsps` answer: "business"
/// only for banks without personal accounts (e.g. Finom), else "personal".
fn psu_type_for(list: &Value, name: &str) -> &'static str {
    let business_only = list["aspsps"]
        .as_array()
        .and_then(|a| {
            a.iter().find(|b| {
                b["name"]
                    .as_str()
                    .is_some_and(|n| n.eq_ignore_ascii_case(name))
            })
        })
        .and_then(|b| b["psu_types"].as_array())
        .is_some_and(|t| {
            t.iter().any(|x| x.as_str() == Some("business"))
                && !t.iter().any(|x| x.as_str() == Some("personal"))
        });
    if business_only {
        "business"
    } else {
        "personal"
    }
}

/// Consent end to request: the bank's maximum (≤ 180 days) minus an hour of
/// slack, else 90 days.
fn consent_valid_until(now: jiff::Timestamp, max_secs: Option<i64>) -> String {
    let secs = match max_secs {
        Some(s) => s.min(MAX_CONSENT_SECS) - 3600,
        None => DEFAULT_CONSENT_DAYS * 86_400,
    }
    .max(3600);
    // Whole seconds and a "Z" suffix: some banks (Revolut) reject fractions.
    (now + jiff::SignedDuration::from_secs(secs))
        .strftime("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

/// Revolut grants at most 90 days: never ask for more, whatever the list says.
fn is_revolut(name: &str) -> bool {
    name.to_lowercase().contains("revolut")
}

/// Spuerkeess (BCEE, via LUXHUB) lists 180 days but refuses the authorisation
/// with `invalid_request`: ask for 90 days and send no `language`.
fn is_spuerkeess(name: &str) -> bool {
    let n = name.to_lowercase();
    n.contains("spuerkeess") || n.contains("bcee")
}

/// N26 lists 180 days in `/aspsps` but rejects `/auth` with `invalid_request`
/// on restricted applications unless capped at 90 days without `language`.
fn is_n26(name: &str) -> bool {
    name.trim().eq_ignore_ascii_case("n26") || name.to_lowercase().starts_with("n26 ")
}

/// Banks that want the leanest request: 90 days at most, no `language`.
fn needs_lean_request(name: &str) -> bool {
    is_revolut(name) || is_spuerkeess(name) || is_n26(name)
}

fn effective_max_secs(name: &str, max: Option<i64>) -> Option<i64> {
    let cap = 90 * 86_400;
    if needs_lean_request(name) {
        Some(max.map_or(cap, |m| m.min(cap)))
    } else {
        max
    }
}

/// Matched `/aspsps` entry and the `/auth` request, without the `state`.
fn auth_diagnostics(list: &Value, name: &str, body: &Value) -> String {
    let entry = list["aspsps"]
        .as_array()
        .and_then(|a| {
            a.iter().find(|b| {
                b["name"]
                    .as_str()
                    .is_some_and(|n| n.eq_ignore_ascii_case(name))
            })
        })
        .map(|b| {
            json!({
                "name": b["name"],
                "country": b["country"],
                "psu_types": b["psu_types"],
                "maximum_consent_validity": b["maximum_consent_validity"],
            })
        })
        .unwrap_or(Value::Null);
    let mut req = body.clone();
    if let Some(o) = req.as_object_mut()
        && o.contains_key("state")
    {
        o.insert("state".into(), json!("<redacted>"));
    }
    format!("aspsp={entry} auth_request={req}")
}

/// Appends the `/auth` context to a consent error and logs it.
fn with_auth_diag(e: CoreError, diag: &str) -> CoreError {
    if diag.is_empty() {
        return e;
    }
    crate::depot_debug::log(&format!("enable_banking consent error: {e} [{diag}]"));
    match e {
        CoreError::Authentication { message } => CoreError::Authentication {
            message: format!("{message} [{diag}]"),
        },
        other => other,
    }
}

/// Balance of a bank account (not a card): booked balances first, the sign
/// from `credit_debit_indicator` when the amount itself is unsigned.
fn pick_account_balance(balances: &[Value]) -> (i64, Option<String>) {
    const ORDER: [&str; 7] = ["CLBD", "ITBD", "XPCD", "OPBD", "ITAV", "CLAV", "OTHR"];
    let pick = ORDER
        .iter()
        .find_map(|t| {
            balances
                .iter()
                .find(|b| b["balance_type"].as_str() == Some(t))
        })
        .or(balances.first());
    let Some(b) = pick else {
        return (0, None);
    };
    let mut cents = json_number_to_cents(&b["balance_amount"]["amount"]).unwrap_or(0);
    let ind = b["credit_debit_indicator"].as_str().unwrap_or("");
    if (ind.eq_ignore_ascii_case("DBIT") || ind.eq_ignore_ascii_case("DEBIT")) && cents > 0 {
        cents = -cents;
    }
    (cents, b["reference_date"].as_str().map(str::to_string))
}

/// One Enable Banking transaction as a GiroCrab booking. `external_id` is the
/// bank's `entry_reference` (stable across sessions), else empty – the caller
/// fills in a content id. `transaction_id` is not used: it "may change".
/// `None` when the amount is missing or unreadable (never stored as 0 €).
fn map_transaction(t: &Value, account_ref: &str) -> Option<RemoteTransaction> {
    let mut amount = json_number_to_cents(&t["transaction_amount"]["amount"])
        .ok()?
        .abs();
    if t["credit_debit_indicator"].as_str() == Some("DBIT") {
        amount = -amount;
    }
    let (party, party_acc) = if amount < 0 {
        (&t["creditor"], &t["creditor_account"])
    } else {
        (&t["debtor"], &t["debtor_account"])
    };
    let date = ["booking_date", "transaction_date", "value_date"]
        .iter()
        .find_map(|k| t[*k].as_str())
        .map(str::to_string)
        .unwrap_or_else(super::today);
    let mut rt = RemoteTransaction {
        account_ref: account_ref.to_string(),
        external_id: t["entry_reference"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        booking_date: date.clone(),
        value_date: t["value_date"].as_str().unwrap_or(&date).to_string(),
        amount_cents: amount,
        currency: t["transaction_amount"]["currency"]
            .as_str()
            .unwrap_or("EUR")
            .to_string(),
        counterparty: party["name"].as_str().unwrap_or_default().to_string(),
        counterparty_iban: party_acc["iban"].as_str().unwrap_or_default().to_string(),
        purpose: t["remittance_information"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default(),
        booking_text: t["bank_transaction_code"]["description"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        purpose_code: eb_purpose_code(t),
        // The API has no end-to-end id or creditor id on transactions; a direct
        // debit's mandate reference comes as `reference_number` with scheme SDDM.
        mandate_id: if t["reference_number_schema"].as_str() == Some("SDDM") {
            t["reference_number"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .to_string()
        } else {
            String::new()
        },
        ..Default::default()
    };
    if rt.counterparty.is_empty() {
        rt.counterparty = rt
            .purpose
            .split_whitespace()
            .take(4)
            .collect::<Vec<_>>()
            .join(" ");
    }
    Some(rt)
}

/// Splits a transaction listing into booked and pending bookings (pending
/// ones get the hold prefix) and fills in missing ids.
fn split_transactions(items: &[Value], account_ref: &str) -> Vec<RemoteTransaction> {
    let mut out = Vec::new();
    let mut ids = super::ContentIds::default();
    let mut unreadable = 0u32;
    for t in items {
        let status = t["status"].as_str().unwrap_or("BOOK");
        let pending = match status {
            "BOOK" => false,
            "PDNG" | "HOLD" => true,
            _ => continue, // CNCL, RJCT, SCHD, OTHR
        };
        let Some(mut rt) = map_transaction(t, account_ref) else {
            unreadable += 1;
            continue;
        };
        if rt.external_id.is_empty() {
            rt.external_id = ids.next(&rt);
        }
        if pending {
            rt.external_id = format!("{HOLD_PREFIX}{}", rt.external_id);
        }
        out.push(rt);
    }
    if unreadable > 0 {
        crate::depot_debug::log(&format!(
            "enable_banking: skipped {unreadable} booking(s) without a readable amount"
        ));
    }
    out
}

impl EnableBankingConnector {
    pub fn new(
        aspsp_setting: &'static str,
        aspsp_default: &'static str,
        country_setting: &'static str,
    ) -> Self {
        EnableBankingConnector {
            aspsp_setting,
            aspsp_default: aspsp_default.to_string(),
            country_setting,
            country_default: "DE".to_string(),
            client: None,
            jwt: String::new(),
            expected_state: String::new(),
            auth_diag: String::new(),
            st: PsdState::default(),
        }
    }

    /// Connector for one fixed bank (generic `ConnectorKind::EnableBanking`).
    pub fn for_aspsp(aspsp: &str, country: &str) -> Self {
        EnableBankingConnector {
            aspsp_setting: "",
            aspsp_default: aspsp.to_string(),
            country_setting: "",
            country_default: country.to_uppercase(),
            client: None,
            jwt: String::new(),
            expected_state: String::new(),
            auth_diag: String::new(),
            st: PsdState::default(),
        }
    }

    /// Amex / Amazon Visa: the frozen card behaviour.
    fn card_mode(&self) -> bool {
        !self.aspsp_setting.is_empty()
    }

    fn make_jwt(ctx: &ConnectContext) -> CoreResult<String> {
        Self::make_jwt_for(
            ctx.setting("enablebanking.application_id")?,
            ctx.setting("enablebanking.private_key_pem")?,
        )
    }

    /// RS256 JWT for the Enable Banking API from the user's own application.
    pub(crate) fn make_jwt_for(app_id: &str, private_key_pem: &str) -> CoreResult<String> {
        let key = EncodingKey::from_rsa_pem(private_key_pem.as_bytes())
            .map_err(|e| CoreError::invalid(format!("Enable Banking private key: {e}")))?;
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(app_id.to_string());
        let now = jiff::Timestamp::now().as_second();
        encode(
            &header,
            &Claims {
                iss: "enablebanking.com",
                aud: "api.enablebanking.com",
                iat: now,
                exp: now + 3600,
            },
            &key,
        )
        .map_err(|e| CoreError::invalid(format!("JWT: {e}")))
    }

    /// Accounts of a session / account-details response. Accepts account
    /// objects in `accounts_data` / `accounts` (merged by uid); bare uid
    /// strings in `accounts` are kept with default name/currency.
    fn parse_accounts(&self, v: &Value) -> Vec<EbAccount> {
        let mut list: Vec<Value> = Vec::new();
        for key in ["accounts_data", "accounts"] {
            if let Some(a) = v[key].as_array() {
                list.extend(a.iter().cloned());
            }
        }
        let card_mode = self.card_mode();
        let mut out: Vec<EbAccount> = Vec::new();
        for a in &list {
            let parsed = if let Some(u) = a.as_str() {
                EbAccount {
                    uid: u.to_string(),
                    ..Default::default()
                }
            } else {
                let mut acc = EbAccount::default();
                self.merge_account_json(&mut acc, a);
                acc
            };
            if parsed.uid.is_empty() {
                continue;
            }
            if let Some(e) = out.iter_mut().find(|e| e.uid == parsed.uid) {
                // merge: prefer richer object data over a bare uid
                for (dst, src) in [
                    (&mut e.name, &parsed.name),
                    (&mut e.iban, &parsed.iban),
                    (&mut e.currency, &parsed.currency),
                    (&mut e.cash_account_type, &parsed.cash_account_type),
                    (&mut e.identification_hash, &parsed.identification_hash),
                    (&mut e.account_number, &parsed.account_number),
                    (&mut e.product, &parsed.product),
                    (&mut e.usage, &parsed.usage),
                ] {
                    if !src.is_empty() {
                        *dst = src.clone();
                    }
                }
                e.details_done |= parsed.details_done;
            } else {
                out.push(parsed);
            }
        }
        for a in &mut out {
            if a.name.is_empty() {
                a.name = self.aspsp_default.clone();
            }
            if a.currency.is_empty() || a.currency.eq_ignore_ascii_case("XXX") {
                a.currency = "EUR".into();
            }
        }
        let refs: Vec<String> = out.iter().map(|a| stable_ref(a, &out, card_mode)).collect();
        for (a, r) in out.iter_mut().zip(refs) {
            a.ext_ref = r;
        }
        out
    }

    /// Copies the non-empty fields of an `AccountResource` JSON into `acc`.
    fn merge_account_json(&self, acc: &mut EbAccount, a: &Value) {
        let s = |v: &Value| v.as_str().unwrap_or_default().trim().to_string();
        let fields = [
            (&mut acc.uid, s(&a["uid"])),
            (
                &mut acc.name,
                a["name"]
                    .as_str()
                    .or(a["product"].as_str())
                    .or(a["details"].as_str())
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
            ),
            (&mut acc.iban, s(&a["account_id"]["iban"]).replace(' ', "")),
            (&mut acc.currency, s(&a["currency"]).to_uppercase()),
            (&mut acc.cash_account_type, s(&a["cash_account_type"])),
            (&mut acc.identification_hash, s(&a["identification_hash"])),
            (&mut acc.account_number, other_account_id(a)),
            (&mut acc.product, s(&a["product"])),
            (&mut acc.usage, s(&a["usage"])),
            (&mut acc.holder, s(&a["name"])),
            (&mut acc.details, s(&a["details"])),
        ];
        for (dst, src) in fields {
            if !src.is_empty() {
                *dst = src;
            }
        }
        // a full account object, not just {uid, identification_hash}
        if a.get("cash_account_type").is_some() || a.get("account_id").is_some() {
            acc.details_done = true;
        }
    }

    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> CoreResult<Value> {
        let client = self.client.as_ref().ok_or(CoreError::SessionClosed)?;
        let mut req = client
            .request(method, format!("{API}{path}"))
            .bearer_auth(&self.jwt)
            .header("Accept", "application/json");
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await?;
        let status = resp.status();
        let v: Value = super::read_json_or_null(resp).await;
        if !status.is_success() {
            return Err(api_error(status.as_u16(), &v));
        }
        Ok(v)
    }

    /// Up to `max_pages` pages of `/accounts/{uid}/transactions?{query}` (see
    /// [`paginate`] for a failing page).
    async fn list_transactions(
        &self,
        uid: &str,
        query: &str,
        max_pages: usize,
    ) -> CoreResult<Paged<Value>> {
        let listed = paginate(max_pages, None, |continuation: Option<String>| async move {
            let mut path = format!("/accounts/{uid}/transactions?{query}");
            if let Some(c) = continuation {
                path.push_str("&continuation_key=");
                path.extend(url::form_urlencoded::byte_serialize(c.as_bytes()));
            }
            let mut page = self.call(reqwest::Method::GET, &path, None).await?;
            // the cursor is the continuation key, `None` on the first page
            let next = page["continuation_key"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(|s| Some(s.to_string()));
            // `get_mut`, not `page["transactions"]`: `IndexMut` panics on a
            // non-object body (array, string)
            let items = match page.get_mut("transactions").map(Value::take) {
                Some(Value::Array(items)) => items,
                _ => Vec::new(),
            };
            Ok((items, next))
        })
        .await?;
        if let Some(e) = &listed.stopped {
            crate::depot_debug::log(&format!(
                "enable_banking transactions ({query}): stopped after {} items: {e}",
                listed.items.len()
            ));
        }
        Ok(listed)
    }

    /// Bank account (generic mode, not a card): balance, booked and pending
    /// bookings. Nothing is added to `out` unless the account succeeded.
    /// Returns whether the history is complete (every page read), i.e. whether
    /// later syncs may switch to incremental `date_from` listings.
    async fn fetch_bank_account(
        &self,
        ctx: &ConnectContext,
        acc: &EbAccount,
        out: &mut FetchResult,
    ) -> CoreResult<bool> {
        let bal = self
            .call(
                reqwest::Method::GET,
                &format!("/accounts/{}/balances", acc.uid),
                None,
            )
            .await?;
        let balances = bal["balances"].as_array().cloned().unwrap_or_default();
        let (balance_cents, ref_date) = pick_account_balance(&balances);
        let since_query = format!("date_from={}", ctx.since_for(&acc.ext_ref));
        let listed = if acc.history_done {
            self.list_transactions(&acc.uid, &since_query, TX_MAX_PAGES)
                .await?
        } else {
            // First sync: as much history as the consent allows.
            match self
                .list_transactions(&acc.uid, "strategy=longest", TX_MAX_PAGES)
                .await
            {
                Ok(v) => v,
                Err(e @ CoreError::Authentication { .. }) => return Err(e),
                Err(e) if is_rate_limit(&e) => return Err(e),
                Err(e) => {
                    crate::depot_debug::log(&format!(
                        "enable_banking strategy=longest failed ({e}); falling back to {since_query}"
                    ));
                    self.list_transactions(&acc.uid, &since_query, TX_MAX_PAGES)
                        .await?
                }
            }
        };
        let complete = listed.stopped.is_none();
        let items = listed.items;
        let txs = split_transactions(&items, &acc.ext_ref);
        // Bank labels only (no names / numbers): to learn how a bank types its accounts.
        crate::depot_debug::log(&format!(
            "enable_banking account kind={:?} cash_account_type={:?} product={:?} usage={:?} has_iban={} has_other_id={} items={} kept={} pending={}",
            kind_of(acc),
            acc.cash_account_type,
            acc.product,
            acc.usage,
            !acc.iban.is_empty(),
            !acc.account_number.is_empty(),
            items.len(),
            txs.len(),
            txs.iter()
                .filter(|t| t.external_id.starts_with(HOLD_PREFIX))
                .count()
        ));
        out.accounts.push(RemoteAccount {
            external_ref: acc.ext_ref.clone(),
            name: display_name(acc),
            owner: acc.holder.clone(),
            iban: acc.iban.clone(),
            account_number: acc.account_number.clone(),
            currency: acc.currency.clone(),
            kind: kind_of(acc),
            balance_cents,
            balance_date: ref_date.unwrap_or_else(super::today),
        });
        out.transactions.extend(txs);
        Ok(complete)
    }

    /// Generic mode: read `/details` once for accounts whose full data is
    /// missing (old uid-keyed state, or state saved before BBAN/product were
    /// kept), give them their stable key and queue the vault renames.
    async fn upgrade_legacy_accounts(&mut self) {
        if self.card_mode() {
            for a in &mut self.st.accounts {
                a.legacy = false;
                a.ext_ref = a.uid.clone();
            }
            return;
        }
        for i in 0..self.st.accounts.len() {
            let a = &self.st.accounts[i];
            if !a.legacy && !a.ext_ref.is_empty() && a.details_done {
                continue;
            }
            let uid = a.uid.clone();
            if a.legacy || !a.details_done {
                match self
                    .call(
                        reqwest::Method::GET,
                        &format!("/accounts/{uid}/details"),
                        None,
                    )
                    .await
                {
                    Ok(d) => {
                        let mut acc = self.st.accounts[i].clone();
                        self.merge_account_json(&mut acc, &d);
                        acc.uid = uid.clone();
                        acc.details_done = true;
                        self.st.accounts[i] = acc;
                    }
                    Err(e) => crate::depot_debug::log(&format!(
                        "enable_banking account details failed: {e}"
                    )),
                }
            }
            let r = stable_ref(&self.st.accounts[i], &self.st.accounts, false);
            let a = &mut self.st.accounts[i];
            let old = a.ext_ref.clone();
            a.ext_ref = r;
            if !old.is_empty() && old != a.ext_ref {
                self.st.renames.push((old, a.ext_ref.clone()));
            }
            a.legacy = false;
        }
    }
}

/// A bank as listed by `GET /aspsps` (only what the picker needs). Banks with
/// only business accounts are kept; `psu_type_for` picks the consent type.
pub(crate) fn parse_aspsps(v: &Value) -> Vec<(String, String)> {
    v["aspsps"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|b| {
                    Some((
                        b["name"].as_str()?.to_string(),
                        b["country"].as_str().unwrap_or_default().to_uppercase(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Exact ASPSP spelling for `wanted` from a `GET /aspsps` answer: a
/// case-insensitive match, else the single bank whose name contains / is
/// contained in `wanted`. `None` when nothing (or more than one) fits.
pub(crate) fn pick_aspsp_name(list: &Value, wanted: &str) -> Option<String> {
    let w = wanted.trim().to_lowercase();
    if w.is_empty() {
        return None;
    }
    // (name, supports personal accounts); no `psu_types` counts as personal.
    let entries: Vec<(String, bool)> = list["aspsps"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|b| {
                    let personal = b["psu_types"]
                        .as_array()
                        .is_none_or(|t| t.iter().any(|x| x.as_str() == Some("personal")));
                    Some((b["name"].as_str()?.to_string(), personal))
                })
                .collect()
        })
        .unwrap_or_default();
    let exact: Vec<&(String, bool)> = entries
        .iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case(wanted.trim()))
        .collect();
    if let Some((n, _)) = exact.iter().find(|(_, p)| *p).or(exact.first()) {
        return Some(n.clone());
    }
    let fuzzy: Vec<&(String, bool)> = entries
        .iter()
        .filter(|(n, _)| {
            let l = n.to_lowercase();
            l.contains(&w) || w.contains(&l)
        })
        .collect();
    let personal: Vec<&&(String, bool)> = fuzzy.iter().filter(|(_, p)| *p).collect();
    let pool: Vec<&String> = if personal.is_empty() {
        fuzzy.iter().map(|(n, _)| n).collect()
    } else {
        personal.iter().map(|(n, _)| n).collect()
    };
    match pool.as_slice() {
        [n] => Some((*n).clone()),
        _ => None,
    }
}

/// The consent went through, but Enable Banking returned no account.
fn no_linked_accounts() -> CoreError {
    CoreError::Bank {
        code: "EB_NO_ACCOUNTS".into(),
        message: tr("Die Bank hat die Freigabe erteilt, Enable Banking liefert aber keine Konten. Bei einer Anwendung im eingeschränkten Modus müssen die Konten zuerst auf enablebanking.com mit der Anwendung verknüpft werden („Linked accounts“). Danach die Freigabe erneut starten.").into(),
    }
}

/// The exact ASPSP name for a generic bank from the live list. When the list
/// has banks but none fits, the bank isn't offered through Enable Banking in
/// that country: say so instead of letting `POST /auth` fail with
/// "Wrong ASPSP name provided". An empty / unreadable list gives `None`
/// (then the configured name is tried as it is).
pub(crate) fn resolve_generic_aspsp(
    list: &Value,
    wanted: &str,
    country: &str,
) -> CoreResult<Option<String>> {
    if let Some(exact) = pick_aspsp_name(list, wanted) {
        return Ok(Some(exact));
    }
    if parse_aspsps(list).is_empty() {
        return Ok(None);
    }
    Err(CoreError::Bank {
        code: "EB_UNKNOWN_ASPSP".into(),
        message: trf(
            "„{0}“ ist über Enable Banking ({1}) nicht verfügbar. Für diese Bank gibt es in GiroCrab derzeit keinen Abruf.",
            &[&wanted, &country],
        ),
    })
}

/// The `error` of a consent redirect (OAuth 2.0 codes), in words: only
/// `access_denied` means the user said no; `server_error` and
/// `temporarily_unavailable` are failures at the bank or Enable Banking.
fn consent_error(code: &str, description: &str) -> CoreError {
    let detail = if description.trim().is_empty() {
        code.to_string()
    } else {
        format!("{code}: {}", description.trim())
    };
    let message = match code {
        "access_denied" => trf(
            "Die Freigabe wurde bei der Bank abgelehnt ({0}).",
            &[&detail],
        ),
        "server_error" | "temporarily_unavailable" => trf(
            "Die Bank hat die Freigabe mit einem Fehler auf ihrer Seite beendet ({0}). Das liegt nicht an Ihren Angaben – bitte später erneut versuchen, gegebenenfalls mit weniger ausgewählten Konten.",
            &[&detail],
        ),
        _ => trf("Die Freigabe ist fehlgeschlagen ({0}).", &[&detail]),
    };
    CoreError::Authentication { message }
}

/// Accepts a full redirect URL ("https://…/callback?code=…&state=…") or a bare code.
fn extract_code(input: &str, expected_state: &str) -> CoreResult<String> {
    let input = input.trim();
    if let Ok(u) = url::Url::parse(input) {
        let mut code = None;
        let mut state = None;
        let mut error = None;
        let mut description = String::new();
        for (k, v) in u.query_pairs() {
            match k.as_ref() {
                "code" => code = Some(v.to_string()),
                "state" => state = Some(v.to_string()),
                "error" => error = Some(v.to_string()),
                "error_description" => description = v.to_string(),
                _ => {}
            }
        }
        if let Some(e) = error {
            return Err(consent_error(&e, &description));
        }
        // A redirect URL must carry the state of this consent: without it the
        // callback is not bound to the session we started (CSRF / replay).
        if !expected_state.is_empty() && state.as_deref() != Some(expected_state) {
            return Err(CoreError::Authentication {
                message: tr("Status stimmt nicht überein – bitte die Zustimmung neu starten")
                    .into(),
            });
        }
        return code
            .ok_or_else(|| CoreError::invalid(tr("Kein „code“ in der eingefügten Adresse")));
    }
    if input.is_empty() {
        return Err(CoreError::invalid(tr(
            "Bitte die Adresse einfügen, auf die Sie weitergeleitet wurden",
        )));
    }
    Ok(input.to_string())
}

#[async_trait]
impl BankConnector for EnableBankingConnector {
    async fn begin(&mut self, ctx: &ConnectContext) -> CoreResult<Phase> {
        crate::depot_debug::log("enable_banking begin: entered");
        self.jwt = Self::make_jwt(ctx)?;
        self.client = Some(http_client_same_host()?);
        if let Ok(st) = ctx.state.decode::<PsdState>() {
            self.st = st;
        }
        let now = jiff::Timestamp::now().to_string();
        if !self.st.session_id.is_empty() && self.st.valid_until > now {
            if self.card_mode() {
                return Ok(Phase::Ready);
            }
            // Generic mode: make sure the consent was not revoked or closed
            // early (Enable Banking call, does not count against the bank's limit).
            let path = format!("/sessions/{}", self.st.session_id.expose());
            match self.call(reqwest::Method::GET, &path, None).await {
                Ok(s) if s["status"].as_str().is_none_or(|st| st == "AUTHORIZED") => {
                    return Ok(Phase::Ready);
                }
                Ok(s) => crate::depot_debug::log(&format!(
                    "enable_banking begin: session status {} -> new consent",
                    s["status"].as_str().unwrap_or("?")
                )),
                Err(CoreError::Authentication { .. }) => {
                    crate::depot_debug::log("enable_banking begin: session gone -> new consent")
                }
                Err(e) => return Err(e),
            }
        }
        self.expected_state = uuid::Uuid::new_v4().to_string();
        self.auth_diag.clear();
        let configured = ctx
            .setting_or(self.aspsp_setting, &self.aspsp_default)
            .to_string();
        let country = ctx
            .setting_or(self.country_setting, &self.country_default)
            .to_string();
        // Amazon Visa is issued by Open Bank S.A. under the brand "Zinia": try both names.
        let mut names = vec![configured.clone()];
        let mut aspsp_list = Value::Null;
        // Generic banks come from our built-in catalog, whose names are
        // best-effort: ask the live ASPSP list for the exact spelling first.
        if !self.card_mode()
            && let Ok(list) = self
                .call(
                    reqwest::Method::GET,
                    &format!("/aspsps?country={country}"),
                    None,
                )
                .await
        {
            if let Some(exact) = resolve_generic_aspsp(&list, &configured, &country)?
                && exact != configured
            {
                names.insert(0, exact);
            }
            aspsp_list = list;
        }
        for alt in ["Open Bank", "Zinia"] {
            if self.aspsp_setting.ends_with("amazonvisa")
                && !names.iter().any(|n| n.eq_ignore_ascii_case(alt))
            {
                names.push(alt.to_string());
            }
        }
        let language = match crate::i18n::current() {
            crate::i18n::Lang::De => "de",
            crate::i18n::Lang::En => "en",
        };
        let mut last_err = None;
        let mut v = serde_json::Value::Null;
        for name in &names {
            let body = if self.card_mode() {
                // unchanged card request
                json!({
                    "access": {"valid_until": consent_valid_until(jiff::Timestamp::now(), None)},
                    "aspsp": {"name": name, "country": country},
                    "state": self.expected_state,
                    "redirect_url": ctx.setting("enablebanking.redirect_url")?,
                    "psu_type": "personal",
                })
            } else {
                let max = effective_max_secs(name, max_consent_secs(&aspsp_list, name));
                let mut body = json!({
                    "access": {
                        "valid_until": consent_valid_until(jiff::Timestamp::now(), max),
                        "balances": true,
                        "transactions": true,
                    },
                    "aspsp": {"name": name, "country": country},
                    "state": self.expected_state,
                    "redirect_url": ctx.setting("enablebanking.redirect_url")?,
                    "psu_type": psu_type_for(&aspsp_list, name),
                });
                // Revolut's OAuth server and Spuerkeess get the leanest request: no language.
                if !needs_lean_request(name) {
                    body["language"] = json!(language);
                }
                body
            };
            if !self.card_mode() {
                self.auth_diag = auth_diagnostics(&aspsp_list, name, &body);
            }
            match self.call(reqwest::Method::POST, "/auth", Some(body)).await {
                Ok(r) => {
                    v = r;
                    last_err = None;
                    break;
                }
                // 4xx other than auth errors: probably unknown ASPSP name -> try the next one
                Err(CoreError::Bank { code, message }) if code.starts_with('4') => {
                    let diag = if self.auth_diag.is_empty() {
                        String::new()
                    } else {
                        format!(" [{}]", self.auth_diag)
                    };
                    last_err = Some(CoreError::Bank {
                        code,
                        message: format!("{}{diag}", trf("ASPSP „{0}“: {1}", &[&name, &message])),
                    });
                }
                Err(e) => return Err(e),
            }
        }
        if let Some(e) = last_err {
            return Err(e);
        }
        let url = v["url"]
            .as_str()
            .ok_or_else(|| CoreError::protocol("no consent URL"))?;
        Ok(Phase::Challenge(Challenge {
            kind: ChallengeKind::Redirect,
            title: if self.card_mode() {
                tr("Zustimmung beim Kartenherausgeber erteilen")
            } else {
                tr("Zustimmung bei der Bank erteilen")
            }
            .into(),
            message: tr("Seite öffnen, anmelden und Zugriff erlauben. Danach die Adresse einfügen, auf die der Browser weitergeleitet wurde.").into(),
            method_name: tr("PSD2-Weiterleitung").into(),
            redirect_url: Some(url.to_string()),
            image: None,
            expires_in_seconds: Some(900),
            poll_interval_seconds: 0,
        }))
    }

    async fn respond(
        &mut self,
        _ctx: &ConnectContext,
        response: ChallengeResponse,
    ) -> CoreResult<Phase> {
        let ChallengeResponse::Code(input) = response else {
            return Err(CoreError::invalid(tr(
                "Bitte die Weiterleitungsadresse einfügen",
            )));
        };
        let code = extract_code(input.expose(), &self.expected_state)
            .map_err(|e| with_auth_diag(e, &self.auth_diag))?;
        let v = self
            .call(
                reqwest::Method::POST,
                "/sessions",
                Some(json!({"code": code})),
            )
            .await?;
        self.st.session_id = v["session_id"].as_str().unwrap_or_default().into();
        self.st.valid_until = v["access"]["valid_until"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let mut accounts = self.parse_accounts(&v);
        if !self.card_mode() && accounts.is_empty() {
            // A restricted application (activated by linking accounts) only
            // sees accounts linked to it: the bank's consent then yields an
            // empty list instead of an error.
            return Err(no_linked_accounts());
        }
        if !self.card_mode() {
            // New consent: keep the vault accounts of the previous session.
            let renames = carry_over(&self.st.accounts, &mut accounts);
            self.st.renames.extend(renames);
            // Not in the new consent any more: mark, never delete.
            let dropped = dropped_refs(&self.st.accounts, &accounts);
            self.st.revoked.extend(dropped);
        }
        self.st.accounts = accounts;
        if let Some(bank) = v["aspsp"]["name"].as_str().filter(|s| !s.is_empty()) {
            self.st.aspsp_name = bank.to_string();
        }
        Ok(Phase::Ready)
    }

    async fn fetch(&mut self, ctx: &ConnectContext) -> CoreResult<FetchResult> {
        crate::depot_debug::log(&format!(
            "enable_banking fetch: entered session_present={} accounts_in_state={} card_mode={}",
            !self.st.session_id.is_empty(),
            self.st.accounts.len(),
            self.card_mode()
        ));
        if self.st.accounts.is_empty() && !self.st.session_id.is_empty() {
            // State lost its accounts (e.g. saved by an older build): re-read them
            // from the session instead of silently syncing nothing.
            let path = format!("/sessions/{}", self.st.session_id.expose());
            let sess = self.call(reqwest::Method::GET, &path, None).await?;
            self.st.accounts = self.parse_accounts(&sess);
            crate::depot_debug::log(&format!(
                "enable_banking fetch: refreshed accounts from session -> {}",
                self.st.accounts.len()
            ));
        }
        self.upgrade_legacy_accounts().await;
        let mut out = FetchResult::default();
        // One account (#1124): its requests only, for the PSD2 daily limit
        // and the per-account history flags.
        let refs: Vec<String> = self.st.accounts.iter().map(|a| a.ext_ref.clone()).collect();
        let only = ctx.only_among(&refs);
        let mut wanted = self.st.accounts.clone();
        wanted.retain(|a| only.is_none_or(|o| a.ext_ref == o));
        if self.card_mode() {
            for a in wanted {
                self.fetch_card(ctx, &mut out, &a).await?;
            }
            out.state = Some(SecretJson::encode(&self.st)?);
            return Ok(out);
        }
        out.hold_prefix = Some(HOLD_PREFIX.into());
        out.renamed_refs = self.st.renames.clone();
        out.revoked_refs = self.st.revoked.clone();
        out.bank_name = Some(if self.st.aspsp_name.is_empty() {
            self.aspsp_default.clone()
        } else {
            self.st.aspsp_name.clone()
        });
        // Accounts first stored under the old automatic name (the holder)
        // get the new one – only if the user hasn't renamed them.
        out.name_updates = self
            .st
            .accounts
            .iter()
            .filter(|a| !a.name.is_empty() && a.name != display_name(a))
            .map(|a| (a.ext_ref.clone(), a.name.clone(), display_name(a)))
            .collect();
        // uid → every page of the account's bookings was read
        let mut listed: HashMap<String, bool> = HashMap::new();
        let mut stopped_early = false;
        for a in wanted {
            let res = if kind_of(&a) == AccountKind::CreditCard {
                let named = EbAccount {
                    name: display_name(&a),
                    ..a.clone()
                };
                self.fetch_card(ctx, &mut out, &named).await.map(|()| true)
            } else {
                self.fetch_bank_account(ctx, &a, &mut out).await
            };
            match res {
                // a cut-off listing was logged by `list_transactions`
                Ok(complete) => {
                    listed.insert(a.uid.clone(), complete);
                }
                Err(e) if is_account_unavailable(&e) => {
                    crate::depot_debug::log(&format!(
                        "enable_banking: account not accessible, marked as revoked ({e})"
                    ));
                    out.revoked_refs.push(a.ext_ref.clone());
                }
                // Bank's daily limit: keep what was fetched so far.
                Err(e) if is_rate_limit(&e) && !out.accounts.is_empty() => {
                    crate::depot_debug::log("enable_banking: rate limit reached, partial sync");
                    stopped_early = true;
                    break;
                }
                Err(e) => return Err(e),
            }
        }
        note_history(&mut self.st.accounts, &listed);
        // The vault uses the fetched name when inserting a new account.
        if let Some(label) =
            paypal_account_label(&self.aspsp_default, self.st.accounts.len(), &ctx.login_name)
            && let [account] = out.accounts.as_mut_slice()
        {
            account.name = label.to_owned();
            out.name_updates.clear();
        }
        // Every account of the consent was looked at: the vault may mark the
        // connection's other accounts as no longer accessible.
        out.accounts_complete = !stopped_early && !out.accounts.is_empty();
        crate::depot_debug::log(&format!(
            "enable_banking fetch: delivered={} complete={} revoked_refs={}",
            out.accounts.len(),
            out.accounts_complete,
            out.revoked_refs.len()
        ));
        // Renames travel with this result; the vault applies them.
        self.st.renames.clear();
        self.st.revoked.clear();
        out.state = Some(SecretJson::encode(&self.st)?);
        Ok(out)
    }
}

impl EnableBankingConnector {
    /// Credit card: the Amex connector's logic, unchanged (session-uid or
    /// stable key as `external_ref`, card balance incl. pending authorisations).
    async fn fetch_card(
        &self,
        ctx: &ConnectContext,
        out: &mut FetchResult,
        acc: &EbAccount,
    ) -> CoreResult<()> {
        let uid = acc.uid.clone();
        let name = acc.name.clone();
        let iban = acc.iban.clone();
        let currency = if acc.currency.trim().is_empty() || acc.currency.eq_ignore_ascii_case("XXX")
        {
            "EUR".to_string()
        } else {
            acc.currency.clone()
        };
        let external_ref = acc.ext_ref.clone();
        {
            crate::depot_debug::log(&format!(
                "enable_banking fetch: processing account uid_len={} currency={currency}",
                uid.len()
            ));
            let bal = self
                .call(
                    reqwest::Method::GET,
                    &format!("/accounts/{uid}/balances"),
                    None,
                )
                .await?;
            let balances = bal["balances"].as_array().cloned().unwrap_or_default();
            let (mut balance_cents, ref_date, picked_type) = pick_balance_typed(&balances);
            for b in &balances {
                crate::depot_debug::log(&format!(
                    "amex raw balance: type={} nonzero={} indicator={} currency={}",
                    b["balance_type"].as_str().unwrap_or("?"),
                    json_number_to_cents(&b["balance_amount"]["amount"]).is_ok_and(|c| c != 0),
                    b["credit_debit_indicator"].as_str().unwrap_or("none"),
                    b["balance_amount"]["currency"].as_str().unwrap_or("?"),
                ));
            }
            crate::depot_debug::log(&format!(
                "amex balances: entries={} picked_sign={}",
                balances.len(),
                balance_cents.signum()
            ));
            let acct_idx = out.accounts.len();
            out.accounts.push(RemoteAccount {
                external_ref: external_ref.clone(),
                name: name.clone(),
                owner: String::new(),
                iban: iban.clone(),
                account_number: String::new(),
                currency: currency.clone(),
                kind: AccountKind::CreditCard,
                balance_cents,
                balance_date: ref_date.unwrap_or_else(super::today),
            });
            // Card cycles span months: look back at least 180 days.
            let floor = crate::dates::iso(crate::dates::today() - crate::dates::days(180));
            let since = if ctx.since > floor {
                floor
            } else {
                ctx.since.clone()
            };
            let mut n = 0u32;
            let mut skipped_non_book = 0u32;
            let mut skipped_unreadable = 0u32;
            let mut ids = super::ContentIds::default();
            // A later page failing keeps the earlier ones (logged by `list_transactions`).
            let booked = self
                .list_transactions(&uid, &format!("date_from={since}"), TX_MAX_PAGES)
                .await?
                .items;
            for t in &booked {
                if t["status"].as_str().map(|s| s != "BOOK").unwrap_or(false) {
                    skipped_non_book += 1;
                    continue;
                }
                // Unreadable amount or no date: skip (counted, logged) instead
                // of storing a 0 € booking without a date.
                let Ok(amount) = json_number_to_cents(&t["transaction_amount"]["amount"]) else {
                    skipped_unreadable += 1;
                    continue;
                };
                let mut amount = amount.abs();
                if t["credit_debit_indicator"].as_str() == Some("DBIT") {
                    amount = -amount;
                }
                let party = if amount < 0 {
                    &t["creditor"]
                } else {
                    &t["debtor"]
                };
                let party_acc = if amount < 0 {
                    &t["creditor_account"]
                } else {
                    &t["debtor_account"]
                };
                let Some(date) = t["booking_date"]
                    .as_str()
                    .or(t["transaction_date"].as_str())
                    .map(str::trim)
                    .filter(|d| !d.is_empty())
                    .map(str::to_string)
                else {
                    skipped_unreadable += 1;
                    continue;
                };
                let mut rt = RemoteTransaction {
                    account_ref: external_ref.clone(),
                    external_id: t["entry_reference"]
                        .as_str()
                        .or(t["transaction_id"].as_str())
                        .unwrap_or_default()
                        .to_string(),
                    booking_date: date.clone(),
                    value_date: t["value_date"].as_str().unwrap_or(&date).to_string(),
                    amount_cents: amount,
                    currency: t["transaction_amount"]["currency"]
                        .as_str()
                        .unwrap_or("EUR")
                        .to_string(),
                    counterparty: party["name"].as_str().unwrap_or_default().to_string(),
                    counterparty_iban: party_acc["iban"].as_str().unwrap_or_default().to_string(),
                    purpose: t["remittance_information"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str())
                                .collect::<Vec<_>>()
                                .join(" ")
                        })
                        .unwrap_or_default(),
                    booking_text: t["bank_transaction_code"]["description"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    ..Default::default()
                };
                if rt.counterparty.is_empty() {
                    rt.counterparty = rt
                        .purpose
                        .split_whitespace()
                        .take(4)
                        .collect::<Vec<_>>()
                        .join(" ");
                }
                if rt.external_id.is_empty() {
                    rt.external_id = ids.next(&rt);
                }
                n += 1;
                out.transactions.push(rt);
            }
            // Pending authorizations (not yet booked) are part of what the card
            // currently owes. Summed only (never stored as bookings), so nothing
            // has to be reconciled when they get booked later.
            let mut pending_sum = 0i64;
            let mut pending_n = 0u32;
            // Optional: without them the balance only lacks the pending part.
            let pending = self
                .list_transactions(
                    &uid,
                    &format!("date_from={since}&transaction_status=PDNG"),
                    PENDING_MAX_PAGES,
                )
                .await
                .map(|p| p.items)
                .unwrap_or_default();
            for t in &pending {
                if t["status"].as_str().is_some_and(|s| s == "BOOK") {
                    continue;
                }
                let a = json_number_to_cents(&t["transaction_amount"]["amount"])
                    .unwrap_or(0)
                    .abs();
                pending_sum += if t["credit_debit_indicator"].as_str() == Some("DBIT") {
                    -a
                } else {
                    a
                };
                pending_n += 1;
            }
            // Booked-type balances exclude pending authorizations; XPCD/OTHR include them.
            let excludes_pending = matches!(picked_type.as_deref(), Some("ITBD" | "CLBD" | "OPBD"));
            if balance_cents != 0 && excludes_pending {
                balance_cents += pending_sum;
            } else if balance_cents == 0 {
                // No usable balance reported. Do NOT sum the (possibly truncated)
                // booking history: the sync keeps the stored balance instead.
                crate::depot_debug::log("amex balance: none reported, keeping stored balance");
                balance_cents = 0;
            }
            out.accounts[acct_idx].balance_cents = balance_cents;
            crate::depot_debug::log(&format!(
                "amex transactions: since={since} raw={} skipped_non_book={skipped_non_book} skipped_unreadable={skipped_unreadable} booked_kept={n} pending={pending_n} pending_sign={} final_balance_sign={}",
                booked.len(),
                pending_sum.signum(),
                balance_cents.signum()
            ));
        }
        Ok(())
    }
}

/// Records whose booking history is complete after a fetch (`listed`: uid →
/// every page was read). A cut-off listing reads the whole history again next
/// time instead of only `date_from`, which could skip the bookings behind the
/// failed page for good (see [`Paged::complete`]).
fn note_history(accounts: &mut [EbAccount], listed: &HashMap<String, bool>) {
    for a in accounts {
        if let Some(&complete) = listed.get(&a.uid) {
            a.history_done = complete;
        }
    }
}

/// SEPA purpose code of an Enable Banking transaction (`purpose_code`, or the
/// ISO bank transaction sub-family `SALA`).
fn eb_purpose_code(t: &serde_json::Value) -> String {
    let code = t["purpose_code"].as_str().unwrap_or_default().trim();
    if code.len() == 4 {
        return code.to_ascii_uppercase();
    }
    match t["bank_transaction_code"]["sub_code"].as_str() {
        Some(c) if c.eq_ignore_ascii_case("SALA") => "SALA".into(),
        _ => String::new(),
    }
}

/// Picks the best balance from an Enable Banking `/balances` array and returns
/// it signed (`DBIT` => negative, i.e. open card debt) plus its reference date.
/// Returns `(0, None)` when nothing usable is present.
#[cfg(test)]
fn pick_balance(balances: &[Value]) -> (i64, Option<String>) {
    let (c, d, _) = pick_balance_typed(balances);
    (c, d)
}

/// Like `pick_balance`, additionally returning the chosen `balance_type`.
fn pick_balance_typed(balances: &[Value]) -> (i64, Option<String>, Option<String>) {
    const ORDER: [&str; 7] = ["ITBD", "CLBD", "XPCD", "ITAV", "OPBD", "FWAV", "OTHR"];
    let signed = |b: &Value| {
        let mut cents = json_number_to_cents(&b["balance_amount"]["amount"]).unwrap_or(0);
        let ind = b["credit_debit_indicator"]
            .as_str()
            .unwrap_or("")
            .to_ascii_uppercase();
        if (ind == "DBIT" || ind == "DEBIT") && cents > 0 {
            cents = -cents;
        }
        cents
    };
    // Booked-balance types first; of those prefer a non-zero one (a 0 sub-balance
    // may sit next to the real total), the largest magnitude winning.
    let owed = ["ITBD", "CLBD", "XPCD", "OTHR"];
    let best = balances
        .iter()
        .filter(|b| {
            b["balance_type"]
                .as_str()
                .is_some_and(|t| owed.contains(&t))
        })
        .filter(|b| signed(b) != 0)
        .max_by_key(|b| signed(b).unsigned_abs());
    let pick = best
        .or_else(|| {
            ORDER.iter().find_map(|t| {
                balances
                    .iter()
                    .find(|b| b["balance_type"].as_str() == Some(t))
            })
        })
        .or(balances.first());
    let Some(pick) = pick else {
        return (0, None, None);
    };
    let mut cents = signed(pick);
    // A credit card reports owed money as a positive number without CRDT: treat as debt.
    let ind = pick["credit_debit_indicator"].as_str().unwrap_or("");
    if cents > 0 && !ind.eq_ignore_ascii_case("CRDT") && !ind.eq_ignore_ascii_case("CREDIT") {
        cents = -cents;
    }
    (
        cents,
        pick["reference_date"].as_str().map(str::to_string),
        pick["balance_type"].as_str().map(str::to_string),
    )
}

/// Result of checking the user's Enable Banking application before the first
/// bank login (`GET /application`).
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct EnableBankingAppCheck {
    pub name: String,
    /// "PRODUCTION" or "SANDBOX".
    pub environment: String,
    pub active: bool,
    pub redirect_urls: Vec<String>,
    /// What still has to be fixed, in plain words; empty = ready.
    pub problems: Vec<String>,
}

fn same_redirect(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.trim().trim_end_matches('/').to_lowercase();
    norm(a) == norm(b)
}

/// Turns a `GET /application` answer into the check result for `redirect`.
pub(crate) fn evaluate_application(v: &Value, redirect: &str) -> EnableBankingAppCheck {
    let redirect_urls: Vec<String> = v["redirect_urls"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let environment = v["environment"].as_str().unwrap_or_default().to_uppercase();
    let active = v["active"].as_bool().unwrap_or(false);
    let mut problems = Vec::new();
    if environment != "PRODUCTION" {
        problems.push(
            tr("Das ist eine Sandbox-Anwendung. Für echte Konten auf enablebanking.com eine Anwendung mit „Production“ registrieren.")
                .to_string(),
        );
    }
    if !active {
        problems.push(
            tr("Die Anwendung ist noch nicht aktiv. Auf enablebanking.com bei der Anwendung „Activate by linking accounts“ wählen und alle gewünschten Konten verknüpfen.")
                .to_string(),
        );
    }
    if !redirect.trim().is_empty() && !redirect_urls.iter().any(|u| same_redirect(u, redirect)) {
        problems.push(trf(
            "Die Redirect-URL „{0}“ ist in der Anwendung nicht eingetragen. Bitte auf enablebanking.com unter „Allowed redirect URLs“ ergänzen.",
            &[&redirect.trim()],
        ));
    }
    EnableBankingAppCheck {
        name: v["name"].as_str().unwrap_or_default().to_string(),
        environment,
        active,
        redirect_urls,
        problems,
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl Vault {
    /// Checks an Enable Banking application before it is saved / used: the
    /// key and ID must belong together, and the application must be active,
    /// in production and allow `redirect_url`.
    pub async fn check_enable_banking_application(
        self: Arc<Self>,
        application_id: String,
        private_key_pem: String,
        redirect_url: String,
    ) -> Result<EnableBankingAppCheck, CoreError> {
        let jwt = EnableBankingConnector::make_jwt_for(application_id.trim(), &private_key_pem)?;
        let resp = http_client_same_host()?
            .get(format!("{API}/application"))
            .bearer_auth(jwt)
            .header("Accept", "application/json")
            .send()
            .await?;
        let status = resp.status();
        let v: Value = super::read_json_or_null(resp).await;
        if matches!(status.as_u16(), 401 | 403) {
            return Err(CoreError::Authentication {
                message: tr("Enable Banking kennt diese Kombination aus Application-ID und Schlüssel nicht. Bitte die .pem-Datei der Anwendung wählen.")
                    .into(),
            });
        }
        if !status.is_success() {
            return Err(api_error(status.as_u16(), &v));
        }
        Ok(evaluate_application(&v, &redirect_url))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn application_check_explains_what_is_missing() {
        let redirect = "https://localhost/girocrab/callback";
        let ready = evaluate_application(
            &json!({"name": "GiroCrab privat", "environment": "PRODUCTION", "active": true,
                    "redirect_urls": ["https://localhost/girocrab/callback/"]}),
            redirect,
        );
        assert!(ready.problems.is_empty(), "{:?}", ready.problems);
        assert_eq!(ready.name, "GiroCrab privat");
        // the three things that went wrong for real users: pending app,
        // sandbox app, redirect URL missing
        let bad = evaluate_application(
            &json!({"environment": "SANDBOX", "active": false, "redirect_urls": ["https://example.org/cb"]}),
            redirect,
        );
        assert_eq!(bad.problems.len(), 3);
        assert!(bad.problems.iter().any(|p| p.contains("Sandbox")));
        assert!(
            bad.problems
                .iter()
                .any(|p| p.contains("Activate by linking accounts"))
        );
        assert!(
            bad.problems
                .iter()
                .any(|p| p.contains("localhost/girocrab/callback"))
        );
        // empty answer: not active, not production
        assert_eq!(
            evaluate_application(&Value::Null, redirect).problems.len(),
            3
        );
    }

    fn generic() -> EnableBankingConnector {
        EnableBankingConnector::for_aspsp("C24", "DE")
    }

    fn amex() -> EnableBankingConnector {
        EnableBankingConnector::new(
            "enablebanking.aspsp.amex",
            "American Express",
            "enablebanking.country.amex",
        )
    }

    #[test]
    fn legacy_tuple_state_is_read() {
        let st: PsdState = serde_json::from_value(json!({
            "session_id": "s1",
            "valid_until": "2027-01-01T00:00:00+00:00",
            "accounts": [["uid-1", "Gold Card", "", "EUR"]]
        }))
        .unwrap();
        assert_eq!(st.accounts.len(), 1);
        let a = &st.accounts[0];
        assert!(a.legacy && a.history_done);
        assert_eq!((a.uid.as_str(), a.ext_ref.as_str()), ("uid-1", "uid-1"));
        // new format round-trips (legacy flag is not persisted)
        let back: PsdState = serde_json::from_value(serde_json::to_value(&st).unwrap()).unwrap();
        assert_eq!(back.accounts[0].uid, "uid-1");
        assert!(!back.accounts[0].legacy);
        // missing fields of an old state
        let empty: PsdState =
            serde_json::from_value(json!({"session_id": "", "valid_until": ""})).unwrap();
        assert!(empty.accounts.is_empty() && empty.renames.is_empty());
    }

    #[test]
    fn card_mode_keeps_uid_and_card_kind() {
        let a = EbAccount {
            uid: "u".into(),
            iban: "DE02 1203 0000 0000 2020 51".into(),
            identification_hash: "h".into(),
            cash_account_type: "CACC".into(),
            ..Default::default()
        };
        assert_eq!(stable_ref(&a, &[], true), "u");
        assert_eq!(account_kind("CACC", true), AccountKind::CreditCard);
        assert_eq!(stable_ref(&a, &[], false), "DE02120300000000202051");
        let no_iban = EbAccount {
            iban: String::new(),
            ..a.clone()
        };
        assert_eq!(stable_ref(&no_iban, &[], false), "h");
        let bare = EbAccount {
            uid: "u".into(),
            ..Default::default()
        };
        assert_eq!(stable_ref(&bare, &[], false), "u");
    }

    #[test]
    fn currency_wallets_sharing_an_iban_stay_apart() {
        // Finom: EUR and USD wallets share one IBAN, the main account has its own
        let acc = |uid: &str, iban: &str, cur: &str| EbAccount {
            uid: uid.into(),
            iban: iban.into(),
            currency: cur.into(),
            ..Default::default()
        };
        let all = vec![
            acc("m", "DE69 1001 8000 0000 0000 01", "EUR"),
            acc("e", "GB51TCCL00000000000001", "EUR"),
            acc("u", "GB51TCCL00000000000001", "USD"),
        ];
        let refs: Vec<String> = all.iter().map(|a| stable_ref(a, &all, false)).collect();
        assert_eq!(
            refs,
            [
                "DE69100180000000000001",
                "GB51TCCL00000000000001:EUR",
                "GB51TCCL00000000000001:USD"
            ]
        );
        // a new consent maps each wallet onto its own previous account
        assert!(same_account(&all[1], &all[1]));
        assert!(!same_account(&all[1], &all[2]));
        let prev: Vec<EbAccount> = all
            .iter()
            .map(|a| EbAccount {
                ext_ref: stable_ref(a, &all, false),
                ..a.clone()
            })
            .collect();
        let mut new: Vec<EbAccount> = prev
            .iter()
            .map(|a| EbAccount {
                uid: format!("{}2", a.uid),
                ..a.clone()
            })
            .collect();
        assert!(carry_over(&prev, &mut new).is_empty());
        assert!(dropped_refs(&prev, &new).is_empty());
    }

    #[test]
    fn savings_recognised_by_product_label() {
        let acc = |t: &str, product: &str| EbAccount {
            cash_account_type: t.into(),
            product: product.into(),
            ..Default::default()
        };
        // real C24 labels (sync log 2026-11-11)
        assert_eq!(
            kind_of(&acc("CACC", "C24 PocketZINS Smart")),
            AccountKind::Savings
        );
        assert_eq!(
            kind_of(&acc("CACC", "C24 Smartkonto")),
            AccountKind::Checking
        );
        assert_eq!(
            kind_of(&acc("OTHR", "Tagesgeldkonto")),
            AccountKind::Savings
        );
        assert_eq!(kind_of(&acc("", "Sparkonto")), AccountKind::Savings);
        // a bank name containing "Spar…" is not a savings product
        assert_eq!(
            kind_of(&acc("CACC", "Sparkasse Giro")),
            AccountKind::Checking
        );
        // explicit type codes win
        assert_eq!(
            kind_of(&acc("CARD", "Tagesgeld Card")),
            AccountKind::CreditCard
        );
        assert_eq!(kind_of(&acc("SVGS", "")), AccountKind::Savings);
    }

    #[test]
    fn account_kinds_from_cash_account_type() {
        assert_eq!(account_kind("CACC", false), AccountKind::Checking);
        assert_eq!(account_kind("svgs", false), AccountKind::Savings);
        assert_eq!(account_kind("CARD", false), AccountKind::CreditCard);
        assert_eq!(account_kind("LOAN", false), AccountKind::Loan);
        assert_eq!(account_kind("OTHR", false), AccountKind::Checking);
        assert_eq!(account_kind("", false), AccountKind::Checking);
    }

    #[test]
    fn session_accounts_are_parsed_and_merged() {
        let v = json!({
            "accounts": [
                {"uid": "u1", "account_id": {"iban": "DE11 5001 0517 0000 0000 01"},
                 "name": "Girokonto", "currency": "EUR", "cash_account_type": "CACC",
                 "identification_hash": "h1"},
                {"uid": "u2", "product": "Pocket", "currency": "EUR",
                 "cash_account_type": "SVGS", "identification_hash": "h2"},
                {"cash_account_type": "CACC", "currency": "EUR"}
            ],
            "accounts_data": [{"uid": "u2", "identification_hash": "h2b"}]
        });
        let acc = generic().parse_accounts(&v);
        assert_eq!(acc.len(), 2, "account without uid is skipped");
        let by = |u: &str| acc.iter().find(|a| a.uid == u).unwrap().clone();
        assert_eq!(by("u1").ext_ref, "DE11500105170000000001");
        assert_eq!(by("u1").name, "Girokonto");
        assert_eq!(by("u2").name, "Pocket");
        // the full account object wins over the accounts_data summary
        assert_eq!(by("u2").ext_ref, "h2");
        assert_eq!(by("u2").cash_account_type, "SVGS");
        // Amex keeps the uid
        let card = amex().parse_accounts(&v);
        assert!(card.iter().all(|a| a.ext_ref == a.uid));
    }

    #[test]
    fn bban_and_product_are_kept() {
        // savings account without IBAN, BBAN in account_id.other
        let v = json!({"accounts": [
            {"uid": "s1", "account_id": {"other": {"identification": "5002 4024 0000 0000 99", "scheme_name": "BBAN"}},
             "cash_account_type": "CACC", "product": "Tagesgeld", "usage": "PRIV",
             "currency": "EUR", "identification_hash": "hs"},
            {"uid": "s2", "account_id": {"iban": "DE11500105170000000001"},
             "all_account_ids": [{"identification": "DE11500105170000000001", "scheme_name": "IBAN"},
                                 {"identification": "500105170000000001", "scheme_name": "BBAN"}],
             "cash_account_type": "CACC", "currency": "EUR", "identification_hash": "hg"},
            {"uid": "s3", "identification_hash": "h3"}
        ]});
        let acc = generic().parse_accounts(&v);
        let by = |u: &str| acc.iter().find(|a| a.uid == u).unwrap().clone();
        let s = by("s1");
        assert_eq!(s.account_number, "500240240000000099");
        assert_eq!(
            (s.product.as_str(), s.usage.as_str()),
            ("Tagesgeld", "PRIV")
        );
        assert_eq!(
            s.ext_ref, "hs",
            "key unchanged: no IBAN -> identification hash"
        );
        assert!(s.details_done);
        // BBAN next to an IBAN is still read; the IBAN stays the key
        assert_eq!(by("s2").account_number, "500105170000000001");
        assert_eq!(by("s2").ext_ref, "DE11500105170000000001");
        // summary-only entry: details still to be read
        assert!(!by("s3").details_done);
        // state saved before these fields existed
        let st: PsdState = serde_json::from_value(json!({
            "session_id": "x", "valid_until": "",
            "accounts": [{"uid": "u", "ext_ref": "h", "identification_hash": "h"}]
        }))
        .unwrap();
        assert!(!st.accounts[0].details_done && st.accounts[0].account_number.is_empty());
    }

    #[test]
    fn accounts_are_named_by_product_not_holder() {
        // real C24 shape: `name` is the holder, `product` the account type
        let v = json!({"accounts": [
            {"uid": "g", "name": "Jay Licina", "product": "C24 Smartkonto",
             "account_id": {"iban": "DE23500240240000000001"}, "currency": "EUR"},
            {"uid": "p", "name": "Jay Licina", "product": "C24 PocketZINS Smart",
             "details": "Urlaub", "currency": "EUR", "identification_hash": "hp"},
            {"uid": "x", "name": "Jay Licina", "currency": "EUR", "identification_hash": "hx"}
        ]});
        let acc = generic().parse_accounts(&v);
        let by = |u: &str| acc.iter().find(|a| a.uid == u).unwrap().clone();
        assert_eq!(by("g").holder, "Jay Licina");
        assert_eq!(display_name(&by("g")), "C24 Smartkonto");
        // the PSU's own description wins over the product
        assert_eq!(display_name(&by("p")), "Urlaub");
        // nothing better known: the holder
        assert_eq!(display_name(&by("x")), "Jay Licina");
        // card mode keeps its old name field (Amex unchanged)
        assert_eq!(amex().parse_accounts(&v)[0].name, "Jay Licina");
        // state saved before holder/details existed: product still names it
        let old = EbAccount {
            name: "Jay Licina".into(),
            product: "C24 Smartkonto".into(),
            ..Default::default()
        };
        assert_eq!(display_name(&old), "C24 Smartkonto");
    }

    #[test]
    fn paypal_connection_label_names_only_a_single_account() {
        assert_eq!(
            paypal_account_label("PayPal", 1, "  Privat  "),
            Some("Privat")
        );
        assert_eq!(paypal_account_label("PayPal", 1, "PayPal"), None);
        assert_eq!(paypal_account_label("PayPal", 2, "Privat"), None);
        assert_eq!(paypal_account_label("C24", 1, "Privat"), None);
    }

    #[test]
    fn account_list_survives_a_deleted_session() {
        // what "Neu verbinden" / clear_connection_session leaves behind
        // (session_id and valid_until removed) – bug seen on 2026-11-11
        let st: PsdState = serde_json::from_value(json!({
            "accounts": [
                {"uid": "u1", "ext_ref": "DE23", "iban": "DE23", "history_done": true},
                {"uid": "u2", "ext_ref": "hs", "identification_hash": "hs", "history_done": true}
            ]
        }))
        .unwrap();
        assert!(st.session_id.is_empty() && st.valid_until.is_empty());
        assert_eq!(st.accounts.len(), 2);
        assert!(st.accounts.iter().all(|a| a.history_done));
    }

    #[test]
    fn reconnect_reports_accounts_left_out_of_the_consent() {
        let acc = |r: &str, iban: &str, hash: &str| EbAccount {
            uid: format!("u-{r}"),
            ext_ref: r.into(),
            iban: iban.into(),
            identification_hash: hash.into(),
            ..Default::default()
        };
        // real C24 case: main account kept, savings + shared account unlinked
        let prev = vec![
            acc("DE23", "DE23", "h1"),
            acc("hs", "", "hs"),
            acc("DE48", "DE48", "h3"),
        ];
        let new = vec![acc("DE23", "DE23", "h1-new-session")];
        assert_eq!(
            dropped_refs(&prev, &new),
            vec!["hs".to_string(), "DE48".to_string()]
        );
        // nothing dropped when all come back (matched by IBAN or hash)
        let all = vec![
            acc("DE23", "DE23", "x"),
            acc("hs", "", "hs"),
            acc("DE48", "DE48", "y"),
        ];
        assert!(dropped_refs(&prev, &all).is_empty());
        // first consent / single account on both sides: nothing to report
        assert!(dropped_refs(&[], &new).is_empty());
        assert!(dropped_refs(&[acc("a", "", "")], &[acc("b", "", "")]).is_empty());
    }

    #[test]
    fn new_consent_carries_accounts_over() {
        let prev = vec![
            EbAccount {
                uid: "old1".into(),
                ext_ref: "old1".into(),
                iban: "DE1".into(),
                history_done: true,
                ..Default::default()
            },
            EbAccount {
                uid: "old2".into(),
                ext_ref: "h2".into(),
                identification_hash: "h2".into(),
                history_done: true,
                ..Default::default()
            },
        ];
        let mut new = vec![
            EbAccount {
                uid: "n1".into(),
                ext_ref: "DE1".into(),
                iban: "DE1".into(),
                ..Default::default()
            },
            EbAccount {
                uid: "n2".into(),
                ext_ref: "h2".into(),
                identification_hash: "h2".into(),
                ..Default::default()
            },
            EbAccount {
                uid: "n3".into(),
                ext_ref: "DE3".into(),
                iban: "DE3".into(),
                ..Default::default()
            },
        ];
        let renames = carry_over(&prev, &mut new);
        assert_eq!(renames, vec![("old1".to_string(), "DE1".to_string())]);
        assert!(new[0].history_done && new[1].history_done && !new[2].history_done);
        // single account on both sides: matched even without IBAN/hash
        let prev = vec![EbAccount {
            uid: "a".into(),
            ext_ref: "a".into(),
            ..Default::default()
        }];
        let mut new = vec![EbAccount {
            uid: "b".into(),
            ext_ref: "hb".into(),
            ..Default::default()
        }];
        assert_eq!(
            carry_over(&prev, &mut new),
            vec![("a".to_string(), "hb".to_string())]
        );
    }

    #[test]
    fn a_cut_off_listing_reads_the_whole_history_again() {
        let account = |uid: &str| EbAccount {
            uid: uid.into(),
            history_done: true,
            ..Default::default()
        };
        let mut accounts = vec![account("ok"), account("cut"), account("unread")];
        let listed = HashMap::from([("ok".to_string(), true), ("cut".to_string(), false)]);
        note_history(&mut accounts, &listed);
        let done: Vec<bool> = accounts.iter().map(|a| a.history_done).collect();
        assert_eq!(done, [true, false, true]);
    }

    #[test]
    fn error_codes_are_mapped() {
        for code in [
            "EXPIRED_SESSION",
            "REVOKED_SESSION",
            "CLOSED_SESSION",
            "ASPSP_PSU_ACTION_REQUIRED",
        ] {
            let e = api_error(400, &json!({"error": code, "message": "x"}));
            assert!(matches!(e, CoreError::Authentication { .. }), "{code}");
        }
        let e = api_error(429, &json!({"error": "ASPSP_RATE_LIMIT_EXCEEDED"}));
        assert!(is_rate_limit(&e));
        let e = api_error(404, &json!({"error": "ASPSP_ACCOUNT_NOT_ACCESSIBLE"}));
        assert!(is_account_unavailable(&e));
        // unknown errors keep the HTTP status as code (begin() retries other ASPSP names on 4xx)
        let e = api_error(
            422,
            &json!({"error": "WRONG_ASPSP_PROVIDED", "message": "m"}),
        );
        assert!(matches!(e, CoreError::Bank { ref code, .. } if code == "422"));
        assert!(matches!(
            api_error(401, &Value::Null),
            CoreError::Authentication { .. }
        ));
    }

    #[test]
    fn consent_length_follows_the_bank() {
        let now: jiff::Timestamp = "2026-11-11T00:00:00+00:00".parse().unwrap();
        let list = json!({"aspsps": [
            {"name": "C24", "country": "DE", "maximum_consent_validity": 15552000},
            {"name": "Long", "country": "DE", "maximum_consent_validity": 31536000}
        ]});
        assert_eq!(max_consent_secs(&list, "c24"), Some(15552000));
        assert_eq!(max_consent_secs(&list, "Nope"), None);
        // 180 days minus one hour
        assert!(consent_valid_until(now, Some(15552000)).starts_with("2027-05-09T23:00:00"));
        // capped at 180 days
        assert_eq!(
            consent_valid_until(now, Some(31536000)),
            consent_valid_until(now, Some(15552000))
        );
        // unknown -> 90 days (card mode always uses this)
        assert!(consent_valid_until(now, None).starts_with("2027-02-09T00:00:00"));
    }

    #[test]
    fn bank_account_balance_and_bookings() {
        let b = vec![
            json!({"balance_type":"ITAV","balance_amount":{"amount":"900.00","currency":"EUR"}}),
            json!({"balance_type":"CLBD","balance_amount":{"amount":"-12.30","currency":"EUR"},"reference_date":"2026-11-10"}),
        ];
        assert_eq!(pick_account_balance(&b), (-1230, Some("2026-11-10".into())));
        // positive bank balance stays positive (no card "debt" heuristic)
        let b = vec![
            json!({"balance_type":"CLBD","balance_amount":{"amount":"250.00"},"credit_debit_indicator":"CRDT"}),
        ];
        assert_eq!(pick_account_balance(&b).0, 25000);
        let b = vec![
            json!({"balance_type":"ITBD","balance_amount":{"amount":"5.00"},"credit_debit_indicator":"DBIT"}),
        ];
        assert_eq!(pick_account_balance(&b).0, -500);
        assert_eq!(pick_account_balance(&[]), (0, None));

        let items = vec![
            json!({"entry_reference": "E1", "transaction_id": "T1", "status": "BOOK",
                   "transaction_amount": {"amount": "19.99", "currency": "EUR"},
                   "credit_debit_indicator": "DBIT", "booking_date": "2026-11-09",
                   "creditor": {"name": "REWE"}, "creditor_account": {"iban": "DE99"},
                   "remittance_information": ["Einkauf", "Karte 1234"]}),
            json!({"status": "BOOK", "transaction_id": "T2",
                   "transaction_amount": {"amount": "1000.00", "currency": "EUR"},
                   "credit_debit_indicator": "CRDT", "booking_date": "2026-11-01",
                   "debtor": {"name": "Arbeitgeber GmbH"}, "remittance_information": ["Gehalt"]}),
            json!({"entry_reference": "P1", "status": "PDNG",
                   "transaction_amount": {"amount": "5.00", "currency": "EUR"},
                   "credit_debit_indicator": "DBIT", "transaction_date": "2026-11-10"}),
            json!({"entry_reference": "X", "status": "CNCL",
                   "transaction_amount": {"amount": "1.00", "currency": "EUR"},
                   "credit_debit_indicator": "DBIT", "booking_date": "2026-11-10"}),
        ];
        let t = split_transactions(&items, "DE11");
        assert_eq!(t.len(), 3, "cancelled booking is dropped");
        assert_eq!(t[0].external_id, "E1");
        assert_eq!(t[0].amount_cents, -1999);
        assert_eq!(t[0].counterparty, "REWE");
        assert_eq!(t[0].counterparty_iban, "DE99");
        assert_eq!(t[0].purpose, "Einkauf Karte 1234");
        assert_eq!(t[0].account_ref, "DE11");
        // no entry_reference: content id, never the unstable transaction_id
        assert_ne!(t[1].external_id, "T2");
        assert!(!t[1].external_id.is_empty());
        assert_eq!(t[1].amount_cents, 100000);
        assert_eq!(t[1].counterparty, "Arbeitgeber GmbH");
        // pending -> hold
        assert_eq!(t[2].external_id, format!("{HOLD_PREFIX}P1"));
        assert_eq!(t[2].booking_date, "2026-11-10");
        assert!(
            HOLD_PREFIX.starts_with("pending-"),
            "store_fetch cleans up pending-*"
        );
    }

    #[test]
    fn psu_type_follows_the_bank() {
        let list = json!({"aspsps": [
            {"name": "N26", "country": "DE", "psu_types": ["personal", "business"]},
            {"name": "Finom", "country": "DE", "psu_types": ["business"]},
            {"name": "No Types", "country": "DE"}
        ]});
        assert_eq!(psu_type_for(&list, "N26"), "personal");
        assert_eq!(psu_type_for(&list, "finom"), "business");
        assert_eq!(psu_type_for(&list, "No Types"), "personal");
        assert_eq!(psu_type_for(&list, "Nope"), "personal");
        assert_eq!(psu_type_for(&Value::Null, "Finom"), "personal");
    }

    #[test]
    fn aspsp_name_resolution() {
        use serde_json::json;
        let list = json!({"aspsps": [
            {"name": "N26 Bank", "country": "DE"},
            {"name": "Revolut", "country": "DE"},
            {"name": "Santander Consumer Bank", "country": "DE"},
            {"name": "Santander Bank Polska", "country": "DE"}
        ]});
        // exact, case-insensitive
        assert_eq!(
            super::pick_aspsp_name(&list, "revolut").as_deref(),
            Some("Revolut")
        );
        // unique partial match
        assert_eq!(
            super::pick_aspsp_name(&list, "N26").as_deref(),
            Some("N26 Bank")
        );
        // ambiguous / unknown / empty
        assert_eq!(super::pick_aspsp_name(&list, "Santander"), None);
        assert_eq!(super::pick_aspsp_name(&list, "Nope"), None);
        assert_eq!(super::pick_aspsp_name(&list, " "), None);
        // consent errors: only access_denied means "refused"
        let e = super::extract_code(
            "https://localhost/girocrab/callback?state=s&error=server_error",
            "s",
        )
        .unwrap_err()
        .to_string();
        assert!(
            e.contains("Fehler auf ihrer Seite") && !e.contains("abgelehnt"),
            "{e}"
        );
        let e = super::extract_code(
            "https://localhost/girocrab/callback?error=access_denied&error_description=User+cancelled",
            "",
        )
        .unwrap_err()
        .to_string();
        assert!(
            e.contains("abgelehnt") && e.contains("User cancelled"),
            "{e}"
        );
        // a bank the live list doesn't have: a clear error, not a 422 later
        let err = super::resolve_generic_aspsp(&list, "Advanzia", "DE").unwrap_err();
        assert!(err.to_string().contains("nicht verfügbar"), "{err}");
        assert_eq!(
            super::resolve_generic_aspsp(&list, "N26", "DE")
                .unwrap()
                .as_deref(),
            Some("N26 Bank")
        );
        // no list (network error, empty answer): try the name as it is
        assert_eq!(
            super::resolve_generic_aspsp(&serde_json::Value::Null, "Advanzia", "DE").unwrap(),
            None
        );
        assert_eq!(super::pick_aspsp_name(&json!({}), "N26"), None);
    }

    #[test]
    fn consent_valid_until_has_whole_seconds_and_z() {
        let now: jiff::Timestamp = "2026-11-11T00:00:00.123456789+00:00".parse().unwrap();
        for max in [None, Some(7_776_000), Some(31_536_000)] {
            let v = consent_valid_until(now, max);
            assert!(
                v.ends_with('Z') && !v.contains('.') && !v.contains('+'),
                "{v}"
            );
        }
        // N26 is capped at 90 days despite advertising 180 days
        assert_eq!(
            effective_max_secs("N26", Some(15_552_000)),
            Some(90 * 86_400)
        );
        assert!(needs_lean_request("N26"));
        // Revolut never exceeds 90 days, with or without a list entry
        assert_eq!(effective_max_secs("Revolut", None), Some(90 * 86_400));
        assert_eq!(
            effective_max_secs("Revolut", Some(180 * 86_400)),
            Some(90 * 86_400)
        );
        assert_eq!(effective_max_secs("N26", None), Some(90 * 86_400));
        // Spuerkeess lists 180 days but only accepts 90 (and no language)
        assert_eq!(
            effective_max_secs("Spuerkeess", Some(15_552_000)),
            Some(90 * 86_400)
        );
        assert!(needs_lean_request("Spuerkeess") && needs_lean_request("BCEE"));
        assert!(!needs_lean_request("C24"));
    }

    #[test]
    fn exact_aspsp_beats_business_variant() {
        let list = json!({"aspsps": [
            {"name": "Revolut Business", "country": "LT", "psu_types": ["business"]},
            {"name": "Revolut", "country": "LT", "psu_types": ["personal"]}
        ]});
        assert_eq!(
            pick_aspsp_name(&list, "revolut").as_deref(),
            Some("Revolut")
        );
        // partial match prefers the personal bank over the business-only one
        assert_eq!(
            pick_aspsp_name(&list, "Revolut Bank").as_deref(),
            Some("Revolut")
        );
    }

    #[test]
    fn callback_error_carries_sanitized_auth_context() {
        let list = json!({"aspsps": [{"name": "Revolut", "country": "LT",
            "psu_types": ["personal"], "maximum_consent_validity": 7776000}]});
        let body = json!({"state": "secret-state", "aspsp": {"name": "Revolut", "country": "LT"}});
        let diag = auth_diagnostics(&list, "Revolut", &body);
        assert!(
            diag.contains("7776000") && diag.contains("<redacted>"),
            "{diag}"
        );
        assert!(!diag.contains("secret-state"), "{diag}");
        let e = extract_code(
            "https://localhost/cb?error=server_error&error_description=boom",
            "",
        )
        .map_err(|e| with_auth_diag(e, &diag))
        .unwrap_err()
        .to_string();
        assert!(
            e.contains("boom") && e.contains("auth_request") && e.contains("Revolut"),
            "{e}"
        );
    }

    #[test]
    fn generic_connector_key() {
        use crate::models::ConnectorKind;
        let k = ConnectorKind::enable_banking_key("de", "N26");
        assert_eq!(k, "enablebanking:DE:N26");
        assert_eq!(
            ConnectorKind::from_str(&k),
            Some(ConnectorKind::EnableBanking)
        );
        assert_eq!(
            ConnectorKind::enable_banking_target(&k),
            Some(("DE", "N26"))
        );
        assert_eq!(
            ConnectorKind::enable_banking_target("enablebanking:AT:Erste Bank"),
            Some(("AT", "Erste Bank"))
        );
        // legacy keys keep their dedicated kinds
        assert_eq!(
            ConnectorKind::from_str("enablebanking:amex"),
            Some(ConnectorKind::AmericanExpress)
        );
        assert_eq!(
            ConnectorKind::enable_banking_target("enablebanking:amex"),
            None
        );
        assert_eq!(ConnectorKind::from_str("enablebanking:DEU:N26"), None);
        assert_eq!(ConnectorKind::from_str("enablebanking:DE:"), None);
    }
    #[test]
    fn extracts_code_from_redirect() {
        assert_eq!(
            super::extract_code("https://localhost/cb?code=abc&state=s1", "s1").unwrap(),
            "abc"
        );
        assert!(super::extract_code("https://localhost/cb?code=abc&state=bad", "s1").is_err());
        // a callback without any state is refused when one was expected
        assert!(super::extract_code("https://localhost/cb?code=abc", "s1").is_err());
        assert_eq!(
            super::extract_code("https://localhost/cb?code=abc", "").unwrap(),
            "abc"
        );
        assert_eq!(super::extract_code(" xyz ", "s1").unwrap(), "xyz");
    }

    #[test]
    fn amex_dbit_balance_is_negative() {
        use serde_json::json;
        let b = vec![
            json!({"balance_type":"ITAV","balance_amount":{"amount":"5000.00"},"credit_debit_indicator":"CRDT"}),
            json!({"balance_type":"ITBD","balance_amount":{"amount":"123.45"},"credit_debit_indicator":"DBIT","reference_date":"2026-11-01"}),
        ];
        assert_eq!(super::pick_balance(&b), (-12345, Some("2026-11-01".into())));
        let single = vec![
            json!({"balance_type":"XYZ","balance_amount":{"amount":"10.00"},"credit_debit_indicator":"DBIT"}),
        ];
        assert_eq!(super::pick_balance(&single).0, -1000);
        assert_eq!(super::pick_balance(&[]), (0, None));
    }
}

#[cfg(test)]
mod content_id_tests {
    use super::*;

    fn item(date: &str, amount: &str, name: &str) -> Value {
        json!({"status": "BOOK", "transaction_amount": {"amount": amount, "currency": "EUR"},
               "credit_debit_indicator": "DBIT", "booking_date": date,
               "creditor": {"name": name}})
    }

    #[test]
    fn mandate_reference_only_from_the_sddm_scheme() {
        let base = |extra: Value| {
            let mut t = json!({"entry_reference": "E1", "status": "BOOK",
                "credit_debit_indicator": "DBIT",
                "transaction_amount": {"currency": "EUR", "amount": "5.00"}});
            t.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            map_transaction(&t, "a").unwrap()
        };
        let dd = base(json!({"reference_number": " MANDAT-7 ", "reference_number_schema": "SDDM"}));
        assert_eq!(dd.mandate_id, "MANDAT-7");
        let rf = base(
            json!({"reference_number": "RF18539007547034", "reference_number_schema": "INTL"}),
        );
        assert_eq!(rf.mandate_id, "");
        assert_eq!(base(json!({})).mandate_id, "");
    }

    #[test]
    fn ids_without_entry_reference_survive_a_moving_window() {
        let a = item("2026-11-05", "3.50", "Café");
        let b = item("2026-11-01", "900.00", "Vermieter");
        let first = split_transactions(&[a.clone(), a.clone(), b.clone()], "DE11");
        assert_ne!(
            first[0].external_id, first[1].external_id,
            "two real coffees"
        );
        // newer booking in front, list order changed: same ids
        let later = split_transactions(
            &[item("2026-11-10", "1.00", "Kiosk"), b, a.clone(), a],
            "DE11",
        );
        assert_eq!(later[2].external_id, first[0].external_id);
        assert_eq!(later[3].external_id, first[1].external_id);
        assert_eq!(later[1].external_id, first[2].external_id);
    }

    #[test]
    fn unreadable_amount_is_skipped_not_zero() {
        let bad = json!({"status": "BOOK", "transaction_amount": {"amount": "n/a"},
                         "booking_date": "2026-11-05"});
        let missing = json!({"status": "BOOK", "booking_date": "2026-11-05"});
        let t = split_transactions(&[bad, missing, item("2026-11-05", "1.00", "X")], "DE11");
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].amount_cents, -100);
    }
}
