//! bunq – official public API (doc.bunq.com), using the user's own API key.
//!
//! The user creates a personal API key in the bunq app (Profile → Settings →
//! Developers → API keys), so GiroCrab needs no PSD2/OAuth registration.
//!
//!  POST /v1/installation     RSA public key  -> installation token + server key
//!  POST /v1/device-server    API key          -> registers this Mac as a device
//!  POST /v1/session-server   API key (signed) -> session token + user id
//!  GET  /v1/user/{u}/monetary-account?count=200
//!  GET  /v1/user/{u}/monetary-account/{a}/payment?count=200 (+ Pagination.older_url)
//!
//! Requests with a body are signed (SHA-256, RSA PKCS#1 v1.5, base64 in
//! `X-Bunq-Client-Signature`); responses are verified against the server key
//! (`X-Bunq-Server-Signature`). Installation, device and session are kept in
//! the encrypted connector state so a sync normally needs one session check.

mod parse;
mod protocol;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{
    http_client, BankConnector, CancelOutcome, Challenge, ChallengeKind, ChallengeResponse,
    ConnectContext, FetchResult, Phase, RemoteAccount, RemoteTransaction, TransferJob,
    TransferResult,
};
use crate::error::{CoreError, CoreResult};
use crate::i18n::{tr, trf};

use parse::*;
use protocol::*;

const PRODUCTION: &str = "https://api.bunq.com";
const SANDBOX: &str = "https://public-api.sandbox.bunq.com";
/// bunq allows 3 GET requests per 3 seconds per endpoint.
const MIN_GAP: Duration = Duration::from_millis(1050);
/// Safety cap so a broken pagination link can't loop forever.
const MAX_PAGES_PER_ACCOUNT: usize = 200;
/// Card holds and foreign-currency details are read for this many days back.
const HOLD_WINDOW_DAYS: i64 = 45;
/// At most this many extra calls per account to link foreign card actions to payments.
const MAX_FOREIGN_LOOKUPS: usize = 30;
/// external_id prefix of card holds; the vault replaces them on every sync.
pub(crate) const HOLD_PREFIX: &str = "pending-hold-bunq-";
/// Receipts (note attachments) are looked for on payments this many days old.
const RECEIPT_WINDOW_DAYS: i64 = 60;
/// Payments this recent are re-checked for new receipts on every sync.
const RECEIPT_RECHECK_DAYS: i64 = 7;
/// At most this many receipt lookups per sync (each is one rate-limited call).
const MAX_RECEIPT_LOOKUPS: usize = 30;
/// Remembered receipt checks (newest payment ids).
const MAX_RECEIPT_STATE: usize = 1000;
/// Monthly PDF statements: look back this many completed months …
const STATEMENT_MONTHS: u32 = 12;
/// … and create at most this many per sync (each export takes a few calls).
const MAX_STATEMENTS_PER_SYNC: usize = 3;

/// Persisted connector state (inside the encrypted vault).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct BunqState {
    /// "production" or "sandbox" – a change starts over.
    env: String,
    /// SHA-256 prefix of the API key – a new key starts over.
    key_fp: String,
    private_key_pem: String,
    installation_token: String,
    server_public_key: String,
    device_registered: bool,
    session_token: String,
    user_id: i64,
    /// Payment id -> `updated` timestamp when its receipts were last checked.
    receipts_checked: BTreeMap<i64, String>,
    /// Account id -> payment id where the rotating re-check of older
    /// payments stopped (continues with older ids on the next sync).
    receipt_cursor: BTreeMap<i64, i64>,
    /// Attachment ids already imported (skip the download next time).
    attachments_done: BTreeSet<i64>,
    /// Monthly statements already imported ("{account id}:{YYYY-MM}").
    statements_done: BTreeSet<String>,
}

pub struct BunqConnector {
    client: Option<reqwest::Client>,
    base: String,
    state: BunqState,
    last_request: Option<Instant>,
    /// Transfer waiting for approval in the bunq app.
    draft: Option<PendingDraft>,
    /// A draft may exist that the user can still approve in the app: set
    /// when the draft-payment POST is sent (a dropped connection may still
    /// have created it), cleared once nothing can be paid any more (bunq
    /// refused the POST, or the draft was rejected/expired). While true, the
    /// session reports errors as "Status unklar", never as "failed".
    order_submitted: bool,
    /// Result once the transfer reached `Phase::Ready`.
    result: TransferResult,
    /// Account id -> local creation date (statements start there).
    account_created: BTreeMap<i64, String>,
}

/// A draft payment the user still has to accept in the bunq app.
#[derive(Debug, Clone)]
struct PendingDraft {
    account_id: i64,
    draft_id: i64,
    challenge: Challenge,
    /// Status polls that failed in a row (tolerated up to
    /// `MAX_DRAFT_POLL_FAILURES`, then the outcome is unknown).
    poll_failures: u8,
}

/// Failed status polls in a row before the outcome is reported as unknown
/// (the user may have approved the draft meanwhile; never "failed").
const MAX_DRAFT_POLL_FAILURES: u8 = 3;

/// What a draft-payment status means for the transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DraftState {
    /// Approved in the bunq app: the payment was made.
    Accepted,
    /// Still waiting for the approval (an empty status counts as pending).
    Pending,
    /// Definitely not executed (rejected, cancelled, expired).
    Refused,
    /// Any other status: may or may not have been executed.
    Unclear,
}

fn classify_draft_status(status: &str) -> DraftState {
    match status {
        "ACCEPTED" | "DEPRECATED_ACCEPTED_PENDING_PAYMENT" => DraftState::Accepted,
        "PENDING" | "" => DraftState::Pending,
        "REJECTED" | "CANCELLED" | "EXPIRED" => DraftState::Refused,
        _ => DraftState::Unclear,
    }
}

impl Default for BunqConnector {
    fn default() -> Self {
        Self::new()
    }
}

impl BunqConnector {
    pub fn new() -> Self {
        BunqConnector {
            client: None,
            base: PRODUCTION.into(),
            state: BunqState::default(),
            last_request: None,
            draft: None,
            order_submitted: false,
            result: TransferResult::default(),
            account_created: BTreeMap::new(),
        }
    }

    async fn accounts(&mut self) -> CoreResult<Vec<(i64, RemoteAccount)>> {
        let path = format!("/v1/user/{}/monetary-account?count=200", self.state.user_id);
        let v = self.get(&path).await.map_err(ApiError::into_core)?;
        let today = super::today();
        let mut out = Vec::new();
        for item in v["Response"].as_array().into_iter().flatten() {
            if let Some((id, acc)) = parse_account(item, &today) {
                let created = item
                    .as_object()
                    .and_then(|o| o.values().next())
                    .and_then(|a| a["created"].as_str())
                    .and_then(local_date);
                if let Some(c) = created {
                    self.account_created.insert(id, c);
                }
                out.push((id, acc));
            }
        }
        Ok(out)
    }

    /// Pages through a bunq list (newest first) and returns the objects under
    /// `key` created on/after `since` (local date).
    async fn list_since(
        &mut self,
        first: String,
        key: &str,
        since: &str,
    ) -> CoreResult<Vec<Value>> {
        self.list_since_api(first, key, since)
            .await
            .map_err(ApiError::into_core)
    }

    /// `list_since` keeping the HTTP status of a failed page.
    async fn list_since_api(
        &mut self,
        first: String,
        key: &str,
        since: &str,
    ) -> ApiResult<Vec<Value>> {
        let mut out = Vec::new();
        let mut path = Some(first);
        let mut pages = 0;
        while let Some(p) = path.take() {
            pages += 1;
            let v = self.get(&p).await?;
            let mut reached_since = false;
            for item in v["Response"].as_array().into_iter().flatten() {
                let obj = &item[key];
                let Some(date) = obj["created"].as_str().and_then(local_date) else {
                    continue;
                };
                if date.as_str() < since {
                    reached_since = true; // newest first: everything after is older
                    continue;
                }
                out.push(obj.clone());
            }
            if !reached_since && pages < MAX_PAGES_PER_ACCOUNT {
                // Anything but a plain API path ends paging instead of the sync.
                path = v["Pagination"]["older_url"]
                    .as_str()
                    .filter(|u| is_api_path(u))
                    .map(str::to_string);
            }
        }
        Ok(out)
    }

    /// Raw `Payment` objects created on/after `since`.
    async fn raw_payments(&mut self, account_id: i64, since: &str) -> CoreResult<Vec<Value>> {
        let first = format!(
            "/v1/user/{}/monetary-account/{account_id}/payment?count=200",
            self.state.user_id
        );
        self.list_since(first, "Payment", since).await
    }

    /// Downloads one attachment. Only the user-level path works for reading
    /// (`…/monetary-account/{a}/attachment/{id}/content` answers 405).
    async fn attachment(&mut self, attachment_id: i64) -> CoreResult<(Vec<u8>, String)> {
        let path = format!(
            "/v1/user/{}/attachment/{attachment_id}/content",
            self.state.user_id
        );
        self.get_raw(&path).await.map_err(ApiError::into_core)
    }

    /// Receipts of recent payments: files sent with the payment plus the
    /// ones the user attached in the bunq app (note attachments).
    async fn receipts(
        &mut self,
        account_id: i64,
        account_ref: &str,
        payments: &[Value],
    ) -> Vec<crate::documents::RemoteDocument> {
        let today = chrono::Local::now().date_naive();
        let days_ago = |p: &Value| {
            p["created"]
                .as_str()
                .and_then(local_date)
                .and_then(|d| chrono::NaiveDate::parse_from_str(&d, "%Y-%m-%d").ok())
                .map(|d| (today - d).num_days())
                .unwrap_or(i64::MAX)
        };
        // Receipt lookups cost one call each: never-checked payments first,
        // then recent ones again (receipts are usually added within days;
        // adding one does not change the payment's `updated`), then the
        // older ones in rotation so a late receipt is still found.
        let mut fresh = Vec::new();
        let mut recheck = Vec::new();
        let mut older = Vec::new();
        for p in payments {
            let (Some(pid), age) = (p["id"].as_i64(), days_ago(p)) else {
                continue;
            };
            let updated = p["updated"].as_str().unwrap_or_default();
            if age > RECEIPT_WINDOW_DAYS {
                continue;
            }
            if self.state.receipts_checked.get(&pid).map(String::as_str) != Some(updated) {
                fresh.push(pid);
            } else if age <= RECEIPT_RECHECK_DAYS {
                recheck.push(pid);
            } else {
                older.push(pid);
            }
        }
        let cursor = self.state.receipt_cursor.get(&account_id).copied();
        let (rotated, next_cursor) = rotate_older(
            older,
            cursor,
            MAX_RECEIPT_LOOKUPS.saturating_sub(fresh.len() + recheck.len()),
        );
        match next_cursor {
            Some(c) => self.state.receipt_cursor.insert(account_id, c),
            None => self.state.receipt_cursor.remove(&account_id),
        };
        let lookup: BTreeSet<i64> = fresh
            .into_iter()
            .chain(recheck)
            .chain(rotated)
            .take(MAX_RECEIPT_LOOKUPS)
            .collect();
        // Payments of a request (direct debits, paid bunq requests) do not
        // take notes: the app stores them on the accepted RequestResponse,
        // which the payment does not reference. One listing per account,
        // only when needed.
        let needs_requests = payments.iter().any(|p| {
            p["id"].as_i64().is_some_and(|id| lookup.contains(&id)) && is_request_payment(p)
        });
        let responses: Vec<Value> = if needs_requests {
            let path = format!(
                "/v1/user/{}/monetary-account/{account_id}/request-response?count=200",
                self.state.user_id
            );
            match self.get(&path).await {
                Ok(v) => v["Response"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|i| i["RequestResponse"].clone())
                    .collect(),
                Err(_) => Vec::new(),
            }
        } else {
            Vec::new()
        };
        let mut drafts: Option<Vec<Value>> = None; // listed on first need
        let mut docs = Vec::new();
        for p in payments {
            let Some(pid) = p["id"].as_i64() else {
                continue;
            };
            let date = p["created"]
                .as_str()
                .and_then(local_date)
                .unwrap_or_default();
            let ext = format!("bunq-{pid}");
            let label = receipt_label(p);
            // 1) attachments sent along with the payment (no extra listing call)
            let mut found: Vec<(i64, String)> = attachment_items(&p["attachment"])
                .into_iter()
                .filter_map(|a| Some((a["id"].as_i64()?, String::new())))
                .collect();
            // 2) receipts added in the app (one call per payment, so limited)
            let updated = p["updated"].as_str().unwrap_or_default().to_string();
            if lookup.contains(&pid) {
                let object = if is_request_payment(p) {
                    match_request_response(p, &responses)
                        .map(|rid| format!("request-response/{rid}"))
                } else {
                    Some(format!("payment/{pid}"))
                };
                let user_id = self.state.user_id;
                let notes_of = |object: &str| {
                    format!(
                        "/v1/user/{user_id}/monetary-account/{account_id}/{object}/note-attachment"
                    )
                };
                let mut result = match object {
                    Some(object) => self.get(&notes_of(&object)).await,
                    // No matching request: nothing to look up (counts as checked).
                    None => Ok(Value::Null),
                };
                // Payments made from an accepted draft (transfers sent via
                // the API, e.g. by GiroCrab) refuse notes: the app stores
                // them on the DraftPayment, which names the payment's id.
                if matches!(&result, Err(e) if e.status == 404) && !is_request_payment(p) {
                    if drafts.is_none() {
                        let path = format!(
                            "/v1/user/{}/monetary-account/{account_id}/draft-payment?count=200",
                            self.state.user_id
                        );
                        // Some accounts refuse the draft list (404) although
                        // they have drafts; the event feed carries them too.
                        let events = format!(
                            "/v1/user/{user_id}/event?monetary_account_id={account_id}&count=200&display_user_event=false"
                        );
                        drafts = Some(match self.get(&path).await {
                            Ok(v) => v["Response"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .map(|i| i["DraftPayment"].clone())
                                .collect(),
                            Err(_) => match self.get(&events).await {
                                Ok(v) => v["Response"]
                                    .as_array()
                                    .into_iter()
                                    .flatten()
                                    .map(|i| i["Event"]["object"]["DraftPayment"].clone())
                                    .filter(|d| !d.is_null())
                                    .collect(),
                                Err(_) => Vec::new(),
                            },
                        });
                    }
                    let draft = drafts.as_deref().and_then(|d| draft_of_payment(pid, d));
                    result = match draft {
                        Some(did) => self.get(&notes_of(&format!("draft-payment/{did}"))).await,
                        None => Ok(Value::Null),
                    };
                }
                if let Ok(v) = result {
                    for item in v["Response"].as_array().into_iter().flatten() {
                        let note = item
                            .as_object()
                            .and_then(|o| o.values().next())
                            .unwrap_or(&Value::Null);
                        let desc = note["description"].as_str().unwrap_or_default().to_string();
                        // The API returns one object here (the spec says array).
                        for a in attachment_items(&note["attachment"]) {
                            if let Some(aid) = a["id"].as_i64() {
                                found.push((aid, desc.clone()));
                            }
                        }
                    }
                    self.state.receipts_checked.insert(pid, updated);
                }
            }
            found.dedup_by_key(|(aid, _)| *aid);
            for (aid, desc) in found {
                if self.state.attachments_done.contains(&aid) {
                    continue;
                }
                let source_url = format!("bunq:attachment:{aid}");
                let Ok((bytes, content_type)) = self.attachment(aid).await else {
                    continue;
                };
                if bytes.is_empty() {
                    continue;
                }
                self.state.attachments_done.insert(aid);
                docs.push(receipt_document(
                    account_ref,
                    &ext,
                    &date,
                    &label,
                    &desc,
                    aid,
                    bytes,
                    &content_type,
                    source_url,
                ));
            }
        }
        // Keep the state small: bunq ids grow over time, so drop the oldest.
        while self.state.receipts_checked.len() > MAX_RECEIPT_STATE {
            self.state.receipts_checked.pop_first();
        }
        while self.state.attachments_done.len() > MAX_RECEIPT_STATE {
            self.state.attachments_done.pop_first();
        }
        docs
    }

    /// Monthly PDF statements of completed months not imported yet.
    async fn statements(
        &mut self,
        account_id: i64,
        account: &RemoteAccount,
    ) -> Vec<crate::documents::RemoteDocument> {
        let mut docs = Vec::new();
        let today = chrono::Local::now().date_naive();
        let mut created = 0;
        for (first, last) in completed_months(today, STATEMENT_MONTHS) {
            if created >= MAX_STATEMENTS_PER_SYNC {
                break;
            }
            // No statements for months before the account was opened.
            let opened = self
                .account_created
                .get(&account_id)
                .cloned()
                .unwrap_or_default();
            if last.format("%Y-%m-%d").to_string() < opened {
                break; // older months are earlier still
            }
            let key = format!("{account_id}:{}", first.format("%Y-%m"));
            if self.state.statements_done.contains(&key) {
                continue;
            }
            created += 1;
            match self.export_statement(account_id, first, last).await {
                Ok(bytes) => {
                    docs.push(statement_document(account, account_id, first, last, bytes));
                    self.state.statements_done.insert(key);
                }
                // Months before the account existed are refused: don't retry them.
                Err(e) if e.status == 400 => {
                    self.state.statements_done.insert(key);
                }
                Err(_) => break, // try again next sync
            }
        }
        docs
    }

    /// POST customer-statement, wait until bunq has rendered it, download the PDF.
    async fn export_statement(
        &mut self,
        account_id: i64,
        first: chrono::NaiveDate,
        last: chrono::NaiveDate,
    ) -> ApiResult<Vec<u8>> {
        let base = format!(
            "/v1/user/{}/monetary-account/{account_id}/customer-statement",
            self.state.user_id
        );
        let token = self.state.session_token.clone();
        let body = json!({
            "statement_format": "PDF",
            "date_start": first.format("%Y-%m-%d").to_string(),
            "date_end": last.format("%Y-%m-%d").to_string(),
            "include_attachment": false,
        });
        let v = self
            .call(reqwest::Method::POST, &base, Some(&body), Some(&token))
            .await?;
        let id = find(&v, "Id")["id"].as_i64().ok_or_else(|| ApiError {
            status: 0,
            message: "bunq statement: missing id".into(),
        })?;
        let mut result = Err(ApiError {
            status: 0,
            message: "bunq statement not ready".into(),
        });
        for attempt in 0..8u64 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            match self.get_raw(&format!("{base}/{id}/content")).await {
                Ok((bytes, _)) if bytes.starts_with(b"%PDF") => {
                    result = Ok(bytes);
                    break;
                }
                Ok(_) => continue, // not rendered yet
                Err(e) if e.status >= 500 || e.status == 404 || e.status == 400 => continue,
                Err(e) => {
                    result = Err(e);
                    break;
                }
            }
        }
        // The export itself is no longer needed at bunq.
        let _ = self
            .call(
                reqwest::Method::DELETE,
                &format!("{base}/{id}"),
                None,
                Some(&token),
            )
            .await;
        result
    }

    /// Card authorisations (MasterCardAction) of the last weeks.
    async fn card_actions(&mut self, account_id: i64, since: &str) -> ApiResult<Vec<Value>> {
        let first = format!(
            "/v1/user/{}/monetary-account/{account_id}/mastercard-action?count=200",
            self.state.user_id
        );
        self.list_since_api(first, "MasterCardAction", since).await
    }

    /// Payment ids booked for a card action (`…/mastercard-action/{id}/payment`).
    async fn payments_of_action(
        &mut self,
        account_id: i64,
        action_id: i64,
    ) -> CoreResult<Vec<i64>> {
        let path = format!(
            "/v1/user/{}/monetary-account/{account_id}/mastercard-action/{action_id}/payment",
            self.state.user_id
        );
        let v = self.get(&path).await.map_err(ApiError::into_core)?;
        Ok(v["Response"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|item| item.as_object()?.values().next()?["id"].as_i64())
            .collect())
    }

    /// Open card holds as "vorgemerkt" bookings plus the original currency
    /// of foreign card payments (appended to the purpose).
    async fn card_details(
        &mut self,
        account_id: i64,
        account: &RemoteAccount,
        payments: &mut [RemoteTransaction],
    ) -> ApiResult<Vec<RemoteTransaction>> {
        // Always the full window: an old hold (hotel, rental car) must not
        // vanish just because this sync only asks for recent bookings.
        // Foreign-currency notes are therefore added for recent payments only.
        let window = chrono::Local::now().date_naive() - chrono::Duration::days(HOLD_WINDOW_DAYS);
        let window = window.format("%Y-%m-%d").to_string();
        let actions = self.card_actions(account_id, &window).await?;
        let now = chrono::Utc::now().naive_utc();
        let mut holds = Vec::new();
        let mut lookups = 0;
        for a in &actions {
            if is_open_hold(a, now) {
                if let Some(t) = parse_hold(a, &account.external_ref) {
                    holds.push(t);
                }
                continue;
            }
            let Some(note) = foreign_amount_note(a, &account.currency) else {
                continue;
            };
            if lookups >= MAX_FOREIGN_LOOKUPS {
                continue;
            }
            lookups += 1;
            let Some(action_id) = a["id"].as_i64() else {
                continue;
            };
            for pid in self
                .payments_of_action(account_id, action_id)
                .await
                .unwrap_or_default()
            {
                let ext = format!("bunq-{pid}");
                if let Some(t) = payments.iter_mut().find(|t| t.external_id == ext) {
                    if !t.purpose.contains(&note) {
                        t.purpose = if t.purpose.is_empty() {
                            note.clone()
                        } else {
                            format!("{} · {note}", t.purpose)
                        };
                    }
                }
            }
        }
        Ok(holds)
    }

    fn draft_path(&self, account_id: i64, draft_id: Option<i64>) -> String {
        let base = format!(
            "/v1/user/{}/monetary-account/{account_id}/draft-payment",
            self.state.user_id
        );
        match draft_id {
            Some(id) => format!("{base}/{id}"),
            None => base,
        }
    }

    /// Current status of the pending draft: PENDING, ACCEPTED, REJECTED, EXPIRED.
    async fn draft_status(&mut self, d: &PendingDraft) -> CoreResult<String> {
        let path = self.draft_path(d.account_id, Some(d.draft_id));
        let v = self.get(&path).await.map_err(ApiError::into_core)?;
        Ok(find(&v, "DraftPayment")["status"]
            .as_str()
            .unwrap_or_default()
            .to_string())
    }

    /// Rejects the pending draft at bunq and re-queries its status. The draft
    /// is dropped only once bunq confirms it is no longer payable, so a
    /// failed or unanswered request can be retried: calling this again
    /// sends the (idempotent) rejection again and re-checks the status, it
    /// never creates anything new. A rejection error of an already rejected
    /// draft resolves through the status query.
    async fn withdraw_draft(&mut self) -> CancelOutcome {
        let Some(d) = self.draft.clone() else {
            return CancelOutcome::Cancelled;
        };
        let path = self.draft_path(d.account_id, Some(d.draft_id));
        let token = self.state.session_token.clone();
        let put = self
            .call(
                reqwest::Method::PUT,
                &path,
                Some(&json!({"status": "REJECTED"})),
                Some(&token),
            )
            .await;
        let status = self.draft_status(&d).await;
        let put_err = put_error_reason(&put);
        match (put_err.is_none(), status) {
            (_, Ok(s)) => match classify_draft_status(&s) {
                DraftState::Refused => {
                    self.draft = None;
                    self.order_submitted = false;
                    CancelOutcome::Cancelled
                }
                DraftState::Accepted => {
                    // Approved in the app before the rejection arrived.
                    self.draft = None;
                    self.result = TransferResult::Executed { instant: false };
                    CancelOutcome::CancellationFailed {
                        reason: tr("Die Überweisung wurde in der bunq-App bereits freigegeben.")
                            .into(),
                    }
                }
                DraftState::Pending => match put_err {
                    // bunq accepted the request but still shows it as open.
                    None => CancelOutcome::CancellationPending {
                        reason: tr("bunq zeigt die Überweisung noch als offen.").into(),
                    },
                    Some(reason) => CancelOutcome::CancellationFailed { reason },
                },
                DraftState::Unclear => CancelOutcome::CancellationUnknown {
                    reason: trf("bunq meldet den Status „{0}“.", &[&s]),
                },
            },
            // Status unavailable: an acknowledged request is pending, an
            // unanswered or refused one leaves the result unknown.
            (true, Err(_)) => CancelOutcome::CancellationPending {
                reason: tr(
                    "bunq hat die Stornierung angenommen, der Status ist noch nicht bestätigt.",
                )
                .into(),
            },
            (false, Err(e)) => CancelOutcome::CancellationUnknown {
                reason: format!("{} / {}", put_err.unwrap_or_default(), e),
            },
        }
    }
}

/// Reason text if the rejection request itself failed.
fn put_error_reason(put: &ApiResult<Value>) -> Option<String> {
    put.as_ref().err().map(|e| e.message.clone())
}

#[async_trait]
impl BankConnector for BunqConnector {
    async fn begin(&mut self, ctx: &ConnectContext) -> CoreResult<Phase> {
        let api_key = ctx.secret.expose().trim().to_string();
        if api_key.is_empty() {
            return Err(CoreError::Authentication {
                message: tr("Bitte den bunq-API-Schlüssel eingeben.").into(),
            });
        }
        let env = if ctx.setting_or("bunq.environment", "production") == "sandbox" {
            "sandbox"
        } else {
            "production"
        };
        self.base = if env == "sandbox" {
            SANDBOX
        } else {
            PRODUCTION
        }
        .into();
        self.client = Some(http_client()?);

        let fp = key_fingerprint(&api_key);
        let stored: BunqState = serde_json::from_value(ctx.state.clone()).unwrap_or_default();
        self.state = if stored.env == env && stored.key_fp == fp {
            stored
        } else {
            BunqState {
                env: env.into(),
                key_fp: fp,
                ..Default::default()
            }
        };

        if self.session_alive().await {
            return Ok(Phase::Ready);
        }
        self.state.session_token.clear();
        if !self.state.installation_token.is_empty()
            && self.state.device_registered
            && self.open_session(&api_key).await.is_ok()
        {
            return Ok(Phase::Ready);
        }
        // Start over: new key pair, installation, device and session.
        self.install().await?;
        self.register_device(&api_key).await?;
        self.open_session(&api_key)
            .await
            .map_err(ApiError::into_core)?;
        Ok(Phase::Ready)
    }

    /// Polls the draft payment until it is accepted in the bunq app.
    ///
    /// Once the draft exists the user can approve it in the app at any time,
    /// so only a clear REJECTED/CANCELLED/EXPIRED counts as "not executed".
    /// An unknown status, or polls that keep failing, end as
    /// `TransferResult::Unknown`; `finish` then still tries to withdraw the
    /// draft. Nothing is ever resent.
    async fn respond(
        &mut self,
        _ctx: &ConnectContext,
        _response: ChallengeResponse,
    ) -> CoreResult<Phase> {
        let Some(mut d) = self.draft.clone() else {
            return Ok(Phase::Ready);
        };
        let status = match self.draft_status(&d).await {
            Ok(s) => s,
            Err(e) => {
                d.poll_failures += 1;
                if d.poll_failures < MAX_DRAFT_POLL_FAILURES {
                    let challenge = d.challenge.clone();
                    self.draft = Some(d);
                    return Ok(Phase::Pending(challenge));
                }
                self.result = TransferResult::Unknown {
                    reason: e.to_string(),
                };
                return Ok(Phase::Ready);
            }
        };
        d.poll_failures = 0;
        match classify_draft_status(&status) {
            DraftState::Accepted => {
                self.draft = None;
                // bunq sends SEPA payments as instant on its own where
                // possible; the API does not say which, so report regular.
                self.result = TransferResult::Executed { instant: false };
                Ok(Phase::Ready)
            }
            DraftState::Pending => {
                let challenge = d.challenge.clone();
                self.draft = Some(d);
                Ok(Phase::Pending(challenge))
            }
            DraftState::Refused => {
                self.draft = None;
                // Definitely not executed: a plain error, not "Status unklar".
                self.order_submitted = false;
                Err(CoreError::Bank {
                    code: "draft".into(),
                    message: tr("bunq hat die Überweisung abgelehnt oder sie wurde abgebrochen.")
                        .into(),
                })
            }
            DraftState::Unclear => {
                // Keep the draft: `finish` tries to withdraw it.
                self.result = TransferResult::Unknown {
                    reason: trf("bunq meldet den Status „{0}“.", &[&status]),
                };
                Ok(Phase::Ready)
            }
        }
    }

    /// Creates a draft payment; the user approves it in the bunq app, so
    /// GiroCrab can never move money without a tap on the phone.
    async fn send_transfer(
        &mut self,
        _ctx: &ConnectContext,
        job: &TransferJob,
    ) -> CoreResult<Phase> {
        let source = job.source_iban.replace(' ', "").to_uppercase();
        let account_id = self
            .accounts()
            .await?
            .into_iter()
            .find(|(_, a)| a.iban.replace(' ', "").to_uppercase() == source)
            .map(|(id, _)| id)
            .ok_or_else(|| CoreError::InvalidInput {
                message: tr("Das Quellkonto wurde bei bunq nicht gefunden.").into(),
            })?;
        let body = json!({
            "entries": [{
                "amount": {"value": cents_to_decimal(job.amount_cents), "currency": "EUR"},
                "counterparty_alias": {
                    "type": "IBAN",
                    "value": job.recipient_iban,
                    "name": job.recipient_name,
                },
                "description": job.purpose,
            }],
            "number_of_required_accepts": 1,
        });
        let token = self.state.session_token.clone();
        let path = self.draft_path(account_id, None);
        self.result = TransferResult::default();
        // From here on a dropped connection may have left a draft at bunq.
        self.order_submitted = true;
        let v = match self
            .call(reqwest::Method::POST, &path, Some(&body), Some(&token))
            .await
        {
            Ok(v) => v,
            Err(e) => {
                // An HTTP error from bunq means no draft was created; only a
                // request without an answer (status 0) leaves it open.
                if e.status != 0 {
                    self.order_submitted = false;
                }
                return Err(e.into_core());
            }
        };
        let draft_id = find(&v, "Id")["id"]
            .as_i64()
            .ok_or_else(|| CoreError::Protocol {
                message: "bunq draft-payment: missing id".into(),
            })?;
        let challenge = Challenge {
            kind: ChallengeKind::DecoupledPush,
            title: tr("Überweisung in der bunq-App freigeben").into(),
            message: trf(
                "{0} an {1}: Bitte öffnen Sie die bunq-App und bestätigen Sie die Zahlung.",
                &[
                    &crate::money::format_cents(job.amount_cents, "EUR".into()),
                    &job.recipient_name,
                ],
            ),
            method_name: tr("Freigabe in der bunq-App").into(),
            redirect_url: None,
            image: None,
            expires_in_seconds: None,
            poll_interval_seconds: 3,
        };
        self.draft = Some(PendingDraft {
            account_id,
            draft_id,
            challenge: challenge.clone(),
            poll_failures: 0,
        });
        Ok(Phase::Challenge(challenge))
    }

    fn transfer_result(&self) -> TransferResult {
        self.result.clone()
    }

    fn order_submitted(&self) -> bool {
        self.order_submitted
    }

    fn has_open_order(&self) -> bool {
        self.draft.is_some()
    }

    /// Session ends: try to withdraw a still pending draft so it can't be
    /// approved later by accident. Best effort; the draft stays tracked
    /// unless bunq confirmed the rejection (see `cancel_transfer`).
    async fn finish(&mut self, _ctx: &ConnectContext) {
        let _ = self.withdraw_draft().await;
    }

    async fn cancel_transfer(&mut self, _ctx: &ConnectContext) -> CancelOutcome {
        self.withdraw_draft().await
    }

    async fn fetch(&mut self, ctx: &ConnectContext) -> CoreResult<FetchResult> {
        let mut out = FetchResult::default();
        let mut holds_ok = true;
        // Incremental syncs only ask for the last days, but receipts can be
        // added to older payments: list the whole receipt window.
        let receipt_since = (chrono::Local::now().date_naive()
            - chrono::Duration::days(RECEIPT_WINDOW_DAYS))
        .format("%Y-%m-%d")
        .to_string();
        let list_since = receipt_since.as_str().min(ctx.since.as_str()).to_string();
        for (id, account) in self.accounts().await? {
            let raw = self.raw_payments(id, &list_since).await?;
            // Bookings keep the sync's own range (pending-transfer matching
            // in `store_fetch` must not see older look-alike payments).
            let mut txs: Vec<RemoteTransaction> = raw
                .iter()
                .filter(|p| {
                    p["created"]
                        .as_str()
                        .and_then(local_date)
                        .is_some_and(|d| d.as_str() >= ctx.since.as_str())
                })
                .filter_map(|p| parse_payment(p, &account.external_ref))
                .collect();
            // Receipts and statements are extras: a failure must not break the sync.
            let receipts = self.receipts(id, &account.external_ref, &raw).await;
            out.documents.extend(receipts);
            let statements = self.statements(id, &account).await;
            out.documents.extend(statements);
            let holds = match self.card_details(id, &account, &mut txs).await {
                Ok(holds) => holds,
                // Accounts without a card can refuse this endpoint: they have no holds.
                Err(e) if refuses_card_actions(e.status) => Vec::new(),
                // Network or server trouble: the list is incomplete, keep the stored holds.
                Err(_) => {
                    holds_ok = false;
                    Vec::new()
                }
            };
            out.transactions.extend(txs);
            out.transactions.extend(holds);
            out.accounts.push(account);
        }
        // Claim the hold list as complete (stale holds are removed) only when
        // every card-action request worked.
        if holds_ok {
            out.hold_prefix = Some(HOLD_PREFIX.into());
        }
        out.state = Some(serde_json::to_value(&self.state).unwrap_or(Value::Null));
        Ok(out)
    }
}

/// HTTP status meaning "this account has no card actions" rather than a failure.
/// 429 (rate limit) and 401 (session gone) are failures: the holds may still exist.
fn refuses_card_actions(status: u16) -> bool {
    matches!(status, 400 | 403 | 404 | 405)
}
