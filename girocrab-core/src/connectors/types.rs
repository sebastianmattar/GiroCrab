//! Connector trait and the data types exchanged with connectors and the app.

use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::{CoreError, CoreResult};
use crate::i18n::tr;
use crate::models::{AccountKind, ConnectorKind};
use crate::secret::{SecretJson, SecretString};

// ------------------------------------------------------------------ FFI types

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ChallengeKind {
    /// Approve in the bank's app (FinTS decoupled, comdirect P_TAN_PUSH).
    /// The UI shows a spinner and calls `poll()` every few seconds.
    DecoupledPush,
    /// Type a code (SMS/app TAN, Trade Republic 2FA PIN).
    Code,
    /// Scan a photoTAN/chipTAN image, then type the code.
    ImageCode,
    /// Open `redirect_url` in the browser, then paste the resulting URL/code.
    Redirect,
    /// Nothing to do: the approval was given and a longer download runs
    /// (`message` tells how far). The UI shows it and calls `poll()` again
    /// after `poll_interval_seconds` (comdirect PostBox, #838).
    Progress,
}

#[derive(Debug, Clone, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Challenge {
    pub kind: ChallengeKind,
    pub title: String,
    pub message: String,
    /// Name of the TAN method ("SecureGo plus", "pushTAN", "SMS") if known.
    pub method_name: String,
    pub redirect_url: Option<String>,
    /// PNG/JPEG bytes for photoTAN (ImageCode).
    pub image: Option<Vec<u8>>,
    /// Seconds until the challenge expires, if the bank says so.
    pub expires_in_seconds: Option<u32>,
    /// Recommended poll interval for DecoupledPush.
    pub poll_interval_seconds: u32,
}

#[derive(Debug, Clone, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SyncSummary {
    pub accounts_updated: u32,
    pub transactions_imported: u32,
    pub transactions_categorized: u32,
    /// New bookings the bank delivered for archived accounts (not stored).
    pub transactions_skipped_archived: u32,
    /// Accounts this sync added to a connection that already had accounts
    /// (e.g. included in a renewed PSD2 consent) – named in the sync notice.
    pub new_accounts: Vec<String>,
    /// Accounts the bank / consent no longer grants access to (newly marked).
    pub revoked_accounts: Vec<String>,
    /// New documents from the comdirect PostBox when this sync fetched it
    /// next to FinTS (#838); `None` = not fetched (not due, off or failed).
    pub postbox_documents: Option<u32>,
    /// Parts of this sync that failed while the rest was stored (#1083), one
    /// text each, accounts by name: e.g. bookings the bank wanted a TAN for.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, uniffi::Enum)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SyncStep {
    NeedsChallenge {
        challenge: Challenge,
    },
    /// Decoupled approval not yet given; keep polling.
    StillWaiting {
        challenge: Challenge,
    },
    Finished {
        summary: SyncSummary,
    },
}

/// Progress of a SEPA transfer (`TransferSession`).
#[derive(Debug, Clone, uniffi::Enum)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum TransferStep {
    NeedsChallenge {
        challenge: Challenge,
    },
    StillWaiting {
        challenge: Challenge,
    },
    /// Name check (Verification of Payee) did not confirm the recipient:
    /// show `check`, then `confirm_payee()` to send anyway or `cancel()`.
    NeedsPayeeConfirmation {
        check: PayeeCheck,
    },
    /// Bank accepted the transfer and an optimistic booking was stored (see
    /// `TransferOutcome::record_error`).
    Finished {
        outcome: TransferOutcome,
    },
    /// SEPA Instant was not possible and nothing was approved or executed
    /// (bank does not offer it, or rejected the order before any approval).
    /// No second order is sent automatically: the app may offer to send it
    /// as a regular transfer, which is a new order with its own approval.
    InstantDeclined {
        reason: String,
    },
    /// The order reached the bank but its result is unclear (ambiguous reply
    /// after the approval, connection lost). It MAY have been executed. A
    /// booking marked "Status unklar" was stored (unless the write failed);
    /// the user has to check the account before trying again. Never resend
    /// automatically, and `cancel()` reports `CancellationUnknown`.
    OutcomeUnknown {
        reason: String,
        end_to_end_id: String,
    },
}

/// Result of the recipient name check (Verification of Payee).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PayeeMatch {
    /// Name matches the account holder (RCVC).
    Match,
    /// Name is close; `bank_name` holds the account holder's name (RVMC).
    CloseMatch,
    /// Name does not match the account holder (RVNM).
    NoMatch,
    /// The check could not be carried out (RVNA, or no result).
    NotPossible,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PayeeCheck {
    pub result: PayeeMatch,
    /// Name as entered by the user.
    pub entered_name: String,
    pub iban: String,
    /// Account holder's name reported by the recipient bank (close match).
    pub bank_name: Option<String>,
    /// Why the check was not possible (RVNA reason).
    pub reason: Option<String>,
    /// Bank's text on authorising despite a mismatch (must be shown).
    pub notice: Option<String>,
}

/// How an executed transfer was sent.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TransferOutcome {
    /// true = accepted as SEPA Instant, false = regular SEPA transfer.
    pub instant: bool,
    /// EndToEndId of the order (shows up in the bank statement).
    pub end_to_end_id: String,
    /// Set when the order was executed but storing it failed: no optimistic
    /// booking, planned transfer or standing order, and for a collection the
    /// mandates are not marked as collected. Never send the order again.
    pub record_error: Option<String>,
}

/// Result of `TransferSession::cancel()`. Closing the session in GiroCrab is
/// not the same as the bank confirming that nothing can be paid any more.
/// Everything except `Cancelled` leaves the order tracked: `cancel()` can be
/// called again and re-checks the bank's status before anything is resent.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum CancelOutcome {
    /// Confirmed: no order exists at the bank, or the bank reports it as
    /// rejected/cancelled/expired.
    Cancelled,
    /// The bank took the cancellation request but still shows the order as
    /// open. Retry to re-check.
    CancellationPending { reason: String },
    /// The request or the status check got no answer (network, timeout):
    /// the order may or may not be cancelled. Retry to re-check.
    CancellationUnknown { reason: String },
    /// The bank refused the cancellation (HTTP error) and the order is
    /// still open, or it was already approved/executed. `reason` says which.
    CancellationFailed { reason: String },
}

/// Connector-side result of a transfer once it reached `Phase::Ready`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferResult {
    Executed {
        instant: bool,
    },
    /// Instant not possible, nothing approved or executed.
    InstantDeclined {
        reason: String,
    },
    /// Order sent, result unclear (may have been executed).
    Unknown {
        reason: String,
    },
}

impl Default for TransferResult {
    fn default() -> Self {
        TransferResult::Executed { instant: false }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BankDescriptor {
    pub kind: ConnectorKind,
    pub name: String,
    pub protocol: String,
    pub bank_code: String,
    /// SF Symbol shown instead of a proprietary bank logo.
    pub sf_symbol: String,
    pub login_label: String,
    pub secret_label: String,
    /// Setting keys that must be filled before a sync can start.
    pub required_settings: Vec<String>,
    pub note: String,
}

// ------------------------------------------------------------------ connector trait

/// Answer to a challenge.
#[derive(Debug)]
pub enum ChallengeResponse {
    /// Ask the bank whether the decoupled approval happened.
    Poll,
    /// Code typed by the user (TAN, 2FA PIN, redirect URL/code).
    Code(SecretString),
    /// The user confirmed the transfer despite a name-check mismatch.
    ConfirmPayee,
}

#[derive(Debug)]
pub enum Phase {
    Ready,
    Challenge(Challenge),
    /// Decoupled approval still pending.
    Pending(Challenge),
    /// Transfers only: the name check needs the user's confirmation.
    ConfirmPayee(PayeeCheck),
}

/// Everything a connector needs to log in. Secrets are wiped on drop.
#[derive(Clone, Default)]
pub struct ConnectContext {
    pub connection_id: i64,
    pub login_name: String,
    pub secret: SecretString,
    /// Resolved settings (product ID, API keys ...), secrets included.
    pub settings: HashMap<String, SecretString>,
    /// Opaque connector state persisted between syncs (FinTS system id, PSD2 session ...).
    pub state: SecretJson,
    /// Fetch transactions booked on or after this date (YYYY-MM-DD): the
    /// earliest per-account start of [`since_by_account`](Self::since_by_account),
    /// so connectors that fetch all accounts at once miss none of them.
    pub since: String,
    /// Per-account start (account external ref → YYYY-MM-DD): the account's
    /// own newest booking minus 10 days. Accounts not listed use `since`.
    pub since_by_account: HashMap<String, String>,
    /// Which documents (PDF statements, receipts, PostBox) this sync fetches.
    pub documents: Documents,
    /// Accounts (external refs) whose data this sync does not store: switched
    /// off or pending in the account selection, archived or deleted.
    /// Connectors that remember what they downloaded must not remember
    /// anything of these, or it is never fetched again once the account is back.
    pub skipped_accounts: HashSet<String>,
    /// Sync of one account (external ref) of the login, from its sidebar menu
    /// (#1124): the result is cut down to it in [`FetchResult::keep_only`];
    /// connectors that request accounts one by one (FinTS) ask only for it.
    pub only_account: Option<String>,
}

/// Document retrieval of one sync, set from the connection's settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Documents {
    /// Switched off for this connection: download nothing and remember
    /// nothing, so a later sync can fetch what is missing.
    Off,
    On {
        /// Accounts (external refs) whose PDF statements are due for the
        /// opt-in automatic fetch.
        statements: Vec<String>,
        /// `YYYY-MM-DD` retrieval was switched off on, while the documents
        /// since then are not all fetched yet. Connectors that read documents
        /// from a window (timeline, receipts) reach back to it.
        catchup_since: Option<String>,
    },
    /// "Kontoauszüge abrufen" for one account (external ref): its statements,
    /// no bookings.
    StatementsOnly(String),
}

impl Default for Documents {
    fn default() -> Self {
        Documents::On {
            statements: Vec::new(),
            catchup_since: None,
        }
    }
}

impl Documents {
    pub fn enabled(&self) -> bool {
        !matches!(self, Documents::Off)
    }

    /// Accounts whose statements a regular sync also fetches.
    pub fn due_statements(&self) -> &[String] {
        match self {
            Documents::On { statements, .. } => statements,
            _ => &[],
        }
    }

    pub fn catchup_since(&self) -> Option<&str> {
        match self {
            Documents::On { catchup_since, .. } => catchup_since.as_deref(),
            _ => None,
        }
    }
}

/// Start dates of an incremental sync. `newest` = (account external ref,
/// newest stored booking date) of the connection's active accounts.
///
/// Each account starts 10 days before its *own* newest booking – one account
/// lagging behind (failed or rate-limited last time) must not be skipped
/// because its siblings moved on. The lower bound is `days` back, or earlier
/// when the whole connection has not been synced for longer. Accounts without
/// bookings (new ones, depots) get that lower bound but do not pull the
/// connection-wide `since` back.
pub(crate) fn since_window(
    today: jiff::civil::Date,
    days: i64,
    newest: &[(String, Option<String>)],
) -> (String, HashMap<String, String>) {
    let parse = |d: &Option<String>| {
        d.as_deref()
            .and_then(|d| jiff::civil::Date::strptime("%Y-%m-%d", d).ok())
    };
    let back10 = |d: jiff::civil::Date| d - crate::dates::days(10);
    let fmt = |d: jiff::civil::Date| crate::dates::iso(d);
    let default_start = today - crate::dates::days(days);
    let floor = match newest.iter().filter_map(|(_, d)| parse(d)).max() {
        Some(g) => default_start.min(back10(g)),
        None => default_start,
    };
    let mut per = HashMap::new();
    let mut earliest: Option<jiff::civil::Date> = None;
    for (r, d) in newest {
        let start = match parse(d) {
            Some(d) => {
                let s = back10(d).max(floor);
                earliest = Some(earliest.map_or(s, |e| e.min(s)));
                s
            }
            None => floor,
        };
        per.insert(r.clone(), fmt(start));
    }
    (fmt(earliest.unwrap_or(floor)), per)
}

impl ConnectContext {
    /// The one account a connector that requests its accounts one by one
    /// asks for (`refs`: all it could ask for): [`only_account`](Self::only_account)
    /// when it is among them. `None`: every account; a single-account sync
    /// is then cut down by [`FetchResult::keep_only`].
    pub fn only_among(&self, refs: &[String]) -> Option<&str> {
        self.only_account
            .as_deref()
            .filter(|r| refs.iter().any(|x| x == r))
    }

    /// Start date for one account (see [`since_by_account`](Self::since_by_account)).
    pub fn since_for(&self, account_ref: &str) -> &str {
        self.since_by_account
            .get(account_ref)
            .map(String::as_str)
            .unwrap_or(&self.since)
    }

    pub fn setting(&self, key: &str) -> CoreResult<&str> {
        self.settings
            .get(key)
            .map(|s| s.expose())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| CoreError::MissingSetting {
                key: key.to_string(),
            })
    }
    pub fn setting_or<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.settings
            .get(key)
            .map(|s| s.expose())
            .filter(|s| !s.is_empty())
            .unwrap_or(default)
    }
}

#[derive(Debug, Clone)]
pub struct RemoteAccount {
    /// Stable id at the bank (IBAN, account id, PSD2 uid).
    pub external_ref: String,
    pub name: String,
    pub owner: String,
    pub iban: String,
    pub account_number: String,
    pub currency: String,
    pub kind: AccountKind,
    pub balance_cents: i64,
    pub balance_date: String,
}

/// Securities position delivered by a connector for a depot account.
#[derive(Debug, Clone, Default)]
pub struct RemoteHolding {
    pub classification: crate::asset_classification::ClassificationValues,
    pub direct_crypto: bool,
    pub account_ref: String,
    pub holding: crate::models::Holding,
}

#[derive(Debug, Clone, Default)]
pub struct RemoteTransaction {
    pub account_ref: String,
    /// De-duplication key; derived from bank ids or a content hash.
    pub external_id: String,
    pub booking_date: String,
    pub value_date: String,
    pub amount_cents: i64,
    pub currency: String,
    pub counterparty: String,
    pub counterparty_iban: String,
    pub purpose: String,
    pub booking_text: String,
    /// SEPA purpose code (ISO 20022 `Purp/Cd`, e.g. `SALA`); "" = unknown.
    pub purpose_code: String,
    /// German Geschäftsvorfallcode (3 digits, e.g. `153`); "" = unknown.
    pub gvc: String,
    /// SEPA end-to-end reference (`EndToEndId`); "" = unknown or `NOTPROVIDED`.
    pub end_to_end_id: String,
    /// SEPA direct-debit mandate reference; "" = unknown.
    pub mandate_id: String,
    /// SEPA creditor identifier (Gläubiger-ID) of a direct debit; "" = unknown.
    pub creditor_id: String,
    /// Product ids (ASIN) of an order's items, comma separated, one entry per
    /// item in purpose order; "" = none (#354).
    pub item_refs: String,
}

impl RemoteTransaction {
    /// Booking content without the occurrence counter (dates, amount,
    /// counterparty, purpose), fields separated by U+001F.
    pub fn content_key(&self) -> String {
        format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
            self.booking_date, self.value_date, self.amount_cents, self.counterparty, self.purpose
        )
    }

    /// Content-based id for banks that don't deliver a transaction id.
    /// `occurrence` numbers identical bookings (same [`content_key`](Self::content_key))
    /// within one fetch, so two real, identical payments stay two rows.
    /// FNV-1a 64: stable across Rust versions (unlike `DefaultHasher`).
    pub fn content_id(&self, occurrence: u32) -> String {
        format!(
            "{CONTENT_ID_PREFIX}{:016x}",
            crate::fnv::fnv1a64(format!("{}\u{1f}{occurrence}", self.content_key()))
        )
    }
}

/// Prefix of [`RemoteTransaction::content_id`] ids. Ids of older versions
/// (`h` + 16 hex digits, `DefaultHasher`) are adopted on the next sync, see
/// [`adopt_legacy_content_id`].
pub(crate) const CONTENT_ID_PREFIX: char = 'c';

/// Hands out content ids, counting occurrences per identical content (not
/// per list position): the ids stay the same when the fetch window moves or
/// other bookings appear in between.
#[derive(Default)]
pub(crate) struct ContentIds(HashMap<String, u32>);

impl ContentIds {
    pub(crate) fn next(&mut self, t: &RemoteTransaction) -> String {
        let n = self.0.entry(t.content_key()).or_insert(0);
        let id = t.content_id(*n);
        *n += 1;
        id
    }
}

/// Ids of documents a connector already downloaded, kept in its state under
/// `"documents"` so later syncs skip them. Only the newest ids are kept: a
/// portal lists far fewer documents than that.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct KnownDocuments(Vec<String>);

impl KnownDocuments {
    const MAX: usize = 2000;

    pub(crate) fn from_state(state: &SecretJson) -> Self {
        let stored = state.decode::<StoredDocuments<Vec<String>>>();
        Self(stored.map(|s| s.documents).unwrap_or_default())
    }

    pub(crate) fn contains(&self, id: &str) -> bool {
        self.0.iter().any(|k| k == id)
    }

    pub(crate) fn insert(&mut self, id: String) {
        self.0.push(id);
    }

    pub(crate) fn to_state(&self) -> Option<SecretJson> {
        let keep = self.0.len().saturating_sub(Self::MAX);
        SecretJson::encode(&StoredDocuments {
            documents: &self.0[keep..],
        })
        .ok()
    }
}

/// State shape of [`KnownDocuments`]: owned to read, borrowed to write.
#[derive(Serialize, Deserialize)]
struct StoredDocuments<D> {
    #[serde(default)]
    documents: D,
}

/// A securities transaction of a depot (#314). The cash side is a normal
/// [`RemoteTransaction`] of the settlement account; this one is the execution.
#[derive(Debug, Clone, Default)]
pub struct RemoteSecurityTransaction {
    pub account_ref: String,
    /// De-duplication key per account: the bank's id or a content hash.
    pub external_id: String,
    pub transaction: crate::models::SecurityTransaction,
    /// `external_id` of the matching booking on a cash account of the same
    /// login, when the bank names it (IBKR, Scalable …). Empty: the amount
    /// match in `link_cash_bookings` decides.
    pub cash_external_id: String,
}

#[derive(Debug, Default, Clone)]
pub struct FetchResult {
    pub accounts: Vec<RemoteAccount>,
    pub transactions: Vec<RemoteTransaction>,
    /// Depot positions; replace the stored holdings of each listed account.
    pub holdings: Vec<RemoteHolding>,
    /// Explicitly fetched complete inventories, including empty depots.
    pub holdings_fetched: Vec<String>,
    /// Accounts (external refs) whose balance could not be read this time: the
    /// reported `balance_cents` is a placeholder and the stored balance stays.
    pub balances_unknown: Vec<String>,
    /// Explicit bank-reported balances, including zero; never derive a replacement.
    pub balances_reported: Vec<String>,
    /// Securities transactions (buy, sell, dividend …) of depots; added or
    /// refreshed by `(account, external_id)`, never removed.
    pub security_transactions: Vec<RemoteSecurityTransaction>,
    /// Standing orders (Daueraufträge, #239) the bank reported; they replace the
    /// stored ones of each account in `standing_orders_fetched`.
    pub standing_orders: Vec<RemoteStandingOrder>,
    /// Accounts (external refs) whose standing orders were fetched completely,
    /// including the ones that have none.
    pub standing_orders_fetched: Vec<String>,
    /// Updated connector state to persist.
    pub state: Option<SecretJson>,
    /// Keep stored balances and add newly imported bookings to them
    /// (for sources that don't report a balance).
    pub adjust_balances: bool,
    /// The balances are simulated (demo bank): `adjust_balances` applies, but
    /// the accounts do not count as "balance computed from bookings" (#212).
    pub simulated: bool,
    /// Accounts (external refs) whose statements were fetched automatically.
    pub statements_fetched: Vec<String>,
    /// PDFs fetched from the bank (trade confirmations, PostBox, statements).
    /// An empty `account_ref` stores the document without an account.
    pub documents: Vec<crate::documents::RemoteDocument>,
    /// Some documents could not be looked at this time (a limit, a failed request): a catch-up
    /// after switching document retrieval on again goes on with the next sync.
    pub documents_incomplete: bool,
    /// The comdirect PostBox next to a FinTS login was fetched (#838): its
    /// last fetch date moves on.
    pub postbox_fetched: bool,
    /// external_id prefix of provisional bookings (card holds) this fetch
    /// lists completely: stored ones missing from it are removed.
    pub hold_prefix: Option<String>,
    /// `(account external ref, external_id)` of entries the bank lists that move
    /// no money (e.g. PayPal's internal offsetting pairs): not stored, and a
    /// stored one is removed (soft delete, stays deleted).
    pub withdrawn: Vec<(String, String)>,
    /// `(old, new)` account external refs to rewrite before storing (a
    /// connector switched to a stable account key).
    pub renamed_refs: Vec<(String, String)>,
    /// External refs of accounts the bank / consent no longer grants access
    /// to: marked as revoked (kept with their bookings, never deleted).
    pub revoked_refs: Vec<String>,
    /// `accounts` is the complete list the bank grants access to (e.g. every
    /// account of a PSD2 consent was fetched): the connection's other
    /// accounts are marked as revoked.
    pub accounts_complete: bool,
    /// Bank name for these accounts when it differs from the connector's
    /// generic name (e.g. "C24" for an Enable Banking connection).
    pub bank_name: Option<String>,
    /// `(external_ref, old automatic name, new name)`: rename accounts that
    /// still carry the old automatic name; names the user chose are kept.
    pub name_updates: Vec<(String, String, String)>,
    /// Ask before fetching accounts this login delivers for the first time
    /// (several cards behind one portal login, #243): they are held back
    /// like accounts another login already delivers.
    pub confirm_new_accounts: bool,
    /// Cut down to one account ([`keep_only`](Self::keep_only)): the login's
    /// last sync time and its documents catch-up stay as they are.
    pub single_account: bool,
    /// Requests that failed while others worked (#1083): the sync still counts
    /// as done and reports them. An account's external ref in a text is
    /// replaced by the account's name when storing.
    pub warnings: Vec<String>,
}

impl FetchResult {
    /// Keeps the per-account data of the accounts (external refs) `keep` accepts.
    pub fn retain_accounts(&mut self, keep: impl Fn(&str) -> bool) {
        self.accounts.retain(|a| keep(&a.external_ref));
        self.transactions.retain(|t| keep(&t.account_ref));
        self.holdings.retain(|h| keep(&h.account_ref));
        self.holdings_fetched.retain(|r| keep(r));
        self.balances_unknown.retain(|r| keep(r));
        self.balances_reported.retain(|r| keep(r));
        self.security_transactions.retain(|t| keep(&t.account_ref));
        self.standing_orders.retain(|o| keep(&o.account_ref));
        self.standing_orders_fetched.retain(|r| keep(r));
        self.statements_fetched.retain(|r| keep(r));
        self.documents.retain(|d| keep(&d.account_ref));
        self.withdrawn.retain(|(r, _)| keep(r));
        self.name_updates.retain(|(r, ..)| keep(r));
    }

    /// Cuts the result down to one account (external ref, #1124): the
    /// login's other accounts keep what they have and none of them counts as
    /// missing. Renames and accounts the bank reported as revoked stay: the
    /// connector does not report them again.
    pub fn keep_only(&mut self, account_ref: &str) {
        let renamed = self
            .renamed_refs
            .iter()
            .find(|(old, _)| old == account_ref)
            .map(|(_, new)| new.clone());
        self.retain_accounts(|r| r == account_ref || renamed.as_deref() == Some(r));
        self.accounts_complete = false;
        self.single_account = true;
    }
}

#[async_trait]
pub trait BankConnector: Send {
    /// Phase 1: log in, possibly returning a challenge.
    async fn begin(&mut self, ctx: &ConnectContext) -> CoreResult<Phase>;
    /// Phase 2: answer or poll a challenge.
    async fn respond(
        &mut self,
        ctx: &ConnectContext,
        response: ChallengeResponse,
    ) -> CoreResult<Phase>;
    /// Download accounts + transactions (only valid after `Ready`).
    async fn fetch(&mut self, ctx: &ConnectContext) -> CoreResult<FetchResult>;
    /// Close the bank session (FinTS dialog end, logout). Best effort.
    async fn finish(&mut self, _ctx: &ConnectContext) {}
    /// User cancelled a transfer that is not finished. Connectors whose order
    /// can outlive the session (bunq draft) override this to withdraw it and
    /// report whether the bank confirmed. Default: `finish`, nothing to withdraw.
    async fn cancel_transfer(&mut self, ctx: &ConnectContext) -> CancelOutcome {
        self.finish(ctx).await;
        CancelOutcome::Cancelled
    }
    /// Submit a SEPA credit transfer (only valid after `Ready`). `Ready`
    /// means the bank accepted it; challenges are answered via `respond`.
    async fn send_transfer(
        &mut self,
        _ctx: &ConnectContext,
        _job: &TransferJob,
    ) -> CoreResult<Phase> {
        Err(CoreError::InvalidInput {
            message: tr("Für diese Bank werden Überweisungen nicht unterstützt.").into(),
        })
    }
    /// Submit a batch transfer (Sammelüberweisung, HKCCM). Same session rules
    /// as `send_transfer`.
    async fn send_batch_transfer(
        &mut self,
        _ctx: &ConnectContext,
        _job: &BatchTransferJob,
    ) -> CoreResult<Phase> {
        Err(CoreError::InvalidInput {
            message: tr("Für diese Bank werden Sammelüberweisungen nicht unterstützt.").into(),
        })
    }
    /// Create, change or delete a standing order (HKCDE/HKCDN/HKCDL). Same
    /// session rules as `send_transfer`.
    async fn send_standing_order(
        &mut self,
        _ctx: &ConnectContext,
        _job: &StandingOrderJob,
    ) -> CoreResult<Phase> {
        Err(CoreError::InvalidInput {
            message: tr("Für diese Bank werden Daueraufträge nicht unterstützt.").into(),
        })
    }
    /// Submit a direct debit collection (SEPA-Lastschrift, HKDSE/HKDME/…).
    /// Same session rules as `send_transfer`.
    async fn send_direct_debit(
        &mut self,
        _ctx: &ConnectContext,
        _job: &DebitJob,
    ) -> CoreResult<Phase> {
        Err(CoreError::InvalidInput {
            message: tr("Für diese Bank werden Lastschriften nicht unterstützt.").into(),
        })
    }
    /// After `send_transfer` reached `Ready`: executed (instant or regular),
    /// instant declined before any approval, or result unknown.
    /// Default: executed as a regular transfer.
    fn transfer_result(&self) -> TransferResult {
        TransferResult::default()
    }
    /// true once the order itself was handed to the bank. A connection error
    /// after that point means the result is unknown, not "not executed".
    fn order_submitted(&self) -> bool {
        false
    }
    /// true while the bank may still hold an order that could be paid
    /// (bunq: draft not confirmed rejected). Lets `cancel()` retry.
    fn has_open_order(&self) -> bool {
        false
    }
}

/// Batch transfer (Sammelüberweisung): one debit account, many payments.
#[derive(Debug, Clone)]
pub struct BatchTransferJob {
    pub source_iban: String,
    pub source_owner: String,
    pub payments: Vec<crate::connectors::fints::sepa::BatchPayment>,
    /// true = the bank books one sum, false = one booking per payment
    /// (only if the bank allows it, see `BatchParams`).
    pub batch_booking: bool,
    pub execution_date: Option<String>,
}

/// One collection (Einzug) to hand to the bank: all lines share scheme,
/// sequence type and due date, as in one pain.008 message.
#[derive(Debug, Clone)]
pub struct DebitJob {
    pub source_iban: String,
    pub creditor_name: String,
    pub creditor_id: String,
    pub scheme: crate::connectors::fints::pain008::DebitScheme,
    pub sequence: crate::connectors::fints::pain008::SequenceType,
    /// Due date, `YYYY-MM-DD`.
    pub collection_date: String,
    pub collections: Vec<crate::connectors::fints::pain008::Collection>,
    /// true = the bank books one sum, false = one booking per collection.
    pub batch_booking: bool,
}

/// A standing order (Dauerauftrag) held by the bank, read-only (#239).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoteStandingOrder {
    pub account_ref: String,
    /// The bank's order id; may be empty (then the content identifies it).
    pub order_id: String,
    pub recipient_name: String,
    pub recipient_iban: String,
    /// Positive = outgoing amount.
    pub amount_cents: i64,
    pub purpose: String,
    /// `YYYY-MM-DD`.
    pub first_date: String,
    pub last_date: Option<String>,
    /// `M` (months) or `W` (weeks).
    pub time_unit: String,
    /// Every n-th month / week.
    pub interval: u32,
    /// Day of the month (1–31, 31 = last day) or weekday (1 = Monday … 7).
    pub exec_day: u32,
    pub can_change: bool,
    pub can_delete: bool,
}

/// A standing order to create, change or delete at the bank (#496). The data
/// is what goes into the order: new values for `Create`/`Change`, the stored
/// ones for `Delete`.
#[derive(Debug, Clone)]
pub struct StandingOrderJob {
    pub action: crate::models::StandingOrderAction,
    pub source_iban: String,
    pub source_owner: String,
    /// The bank's order id (`Change`/`Delete`), else empty.
    pub order_id: String,
    pub recipient_name: String,
    pub recipient_iban: String,
    pub recipient_bic: Option<String>,
    pub amount_cents: i64,
    pub purpose: String,
    pub first_date: String,
    pub last_date: Option<String>,
    pub time_unit: String,
    pub interval: u32,
    pub exec_day: u32,
}

/// Transfer request plus the source account data resolved from the vault.
#[derive(Debug, Clone)]
pub struct TransferJob {
    pub source_iban: String,
    pub source_owner: String,
    pub recipient_name: String,
    pub recipient_iban: String,
    pub recipient_bic: Option<String>,
    pub amount_cents: i64,
    pub purpose: String,
    /// SEPA Instant requested. If the bank does not offer it, the connector
    /// reports `TransferResult::InstantDeclined`; it never resends on its own.
    pub instant: bool,
    pub execution_date: Option<String>,
    /// Stable per-transfer EndToEndId (max. 35 chars, `[A-Z0-9]`).
    pub end_to_end_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_documents_keep_the_newest_ids() {
        assert_eq!(
            KnownDocuments::from_state(&SecretJson::default()),
            KnownDocuments::default()
        );
        let mut known = KnownDocuments::from_state(&serde_json::json!({"documents": ["a"]}).into());
        assert!(known.contains("a") && !known.contains("b"));
        for i in 0..KnownDocuments::MAX {
            known.insert(i.to_string());
        }
        let kept = KnownDocuments::from_state(&known.to_state().unwrap());
        assert!(!kept.contains("a"));
        assert!(kept.contains("0") && kept.contains(&(KnownDocuments::MAX - 1).to_string()));
    }
}
