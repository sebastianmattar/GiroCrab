//! Modular two-phase bank connectors.
//!
//! Phase 1 `begin()` logs in. It either finishes (`Phase::Ready`) or returns a
//! `Challenge` the user must answer: a decoupled push-TAN (approve in the
//! banking app, then we poll), a code (SMS/app TAN, 2FA PIN) or a redirect
//! (PSD2 consent in the browser).
//!
//! Phase 2 `respond()` answers the challenge (or polls the decoupled one) and
//! again returns `Ready` or another `Challenge`.
//!
//! Once ready, `fetch()` downloads accounts and transactions, which the
//! `SyncSession` merges into the vault and runs the categorisation rules over.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::{params, OptionalExtension};
use tokio::sync::Mutex as AsyncMutex;

use crate::error::{CoreError, CoreResult};
use crate::i18n::{tr, trf};
use crate::models::{AccountKind, ConnectorKind, KnownRecipient, SepaTransferRequest};
use crate::secret::SecretString;
use crate::vault::Vault;

pub mod amazon_orders;
pub mod bank_directory;
mod bic_directory_data;
pub mod bunq;
pub mod comdirect;
pub mod demo;
pub mod dpop;
pub mod enable_banking;
pub mod fints;
pub mod fints_directory;
mod fints_directory_data;
pub mod fints_presets;
pub mod moneymoney_db;
pub mod moneymoney_import;
pub mod paypal;
pub mod portal_import;
pub mod scalable;
pub mod template;
pub mod trade_republic;

// ------------------------------------------------------------------ FFI types

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
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
}

#[derive(Debug, Clone, uniffi::Record)]
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
}

#[derive(Debug, Clone, uniffi::Enum)]
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
    /// Bank accepted the transfer; an optimistic booking was stored.
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
    /// booking marked "Status unklar" was stored; the user has to check the
    /// account before trying again. Never resend automatically.
    OutcomeUnknown {
        reason: String,
        end_to_end_id: String,
    },
}

/// Result of the recipient name check (Verification of Payee).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
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
pub struct TransferOutcome {
    /// true = accepted as SEPA Instant, false = regular SEPA transfer.
    pub instant: bool,
    /// EndToEndId of the order (shows up in the bank statement).
    pub end_to_end_id: String,
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

/// The supported institutions with their public protocol facts.
#[uniffi::export]
pub fn bank_catalog() -> Vec<BankDescriptor> {
    ConnectorKind::all().into_iter().map(descriptor).collect()
}

pub fn descriptor(kind: ConnectorKind) -> BankDescriptor {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    match kind {
        ConnectorKind::VolksbankKasselGoettingen => BankDescriptor {
            kind,
            name: "Volksbank Kassel Göttingen".into(),
            protocol: "FinTS 3.0 PIN/TAN".into(),
            bank_code: "52090000".into(),
            sf_symbol: "building.columns.fill".into(),
            login_label: tr("VR-NetKey oder Alias").into(),
            secret_label: tr("Online-Banking-PIN").into(),
            required_settings: s(&["fints.product_id", "fints.url.52090000"]),
            note: tr("Freigabe in der SecureGo-plus-App (Decoupled, TAN-Prozess 4, HKTAN v7). Server: fints1.atruvia.de").into(),
        },
        ConnectorKind::Direkt1822 => BankDescriptor {
            kind,
            name: "1822direkt".into(),
            protocol: "FinTS 3.0 PIN/TAN".into(),
            bank_code: "50050201".into(),
            sf_symbol: "building.columns".into(),
            login_label: tr("Anmeldename / Kontonummer").into(),
            secret_label: tr("Online-Banking-PIN").into(),
            required_settings: s(&["fints.product_id", "fints.url.50050201"]),
            note: tr("pushTAN-App (Decoupled) oder chipTAN. Server: banking-hs5.s-fints-pt-hs.de").into(),
        },
        ConnectorKind::FinTsGeneric => BankDescriptor {
            kind,
            name: tr("FinTS-Bank").into(),
            protocol: "FinTS 3.0 PIN/TAN".into(),
            bank_code: String::new(),
            sf_symbol: "building.columns".into(),
            login_label: tr("Anmeldename / VR-NetKey / Kontonummer").into(),
            secret_label: tr("Online-Banking-PIN").into(),
            required_settings: s(&["fints.product_id"]),
            note: tr("Beliebige FinTS-3.0-Bank (PIN/TAN). BLZ und FinTS-URL stehen im Bankzugang.").into(),
        },
        ConnectorKind::Comdirect => BankDescriptor {
            kind,
            name: "comdirect".into(),
            protocol: tr("comdirect REST-API (OAuth2 + Session-TAN)").into(),
            bank_code: "20041111".into(),
            sf_symbol: "building.2".into(),
            login_label: tr("Zugangsnummer").into(),
            secret_label: "PIN".into(),
            required_settings: s(&["comdirect.client_id", "comdirect.client_secret"]),
            note: tr("Session-TAN per photoTAN-Push, photoTAN-Grafik oder mobileTAN.").into(),
        },
        ConnectorKind::PayPal => BankDescriptor {
            kind,
            name: "PayPal".into(),
            protocol: tr("PayPal Transaction Search REST-API").into(),
            bank_code: String::new(),
            sf_symbol: "p.circle".into(),
            login_label: tr("E-Mail (nur Anzeige)").into(),
            secret_label: tr("(nicht nötig – nutzt Ihre REST-App)").into(),
            required_settings: s(&["paypal.client_id", "paypal.client_secret"]),
            note: tr("OAuth2 Client Credentials; Historie in 31-Tage-Fenstern.").into(),
        },
        ConnectorKind::TradeRepublic => BankDescriptor {
            kind,
            name: "Trade Republic".into(),
            protocol: tr("WebSocket-API + 2FA").into(),
            bank_code: String::new(),
            sf_symbol: "chart.line.uptrend.xyaxis.circle".into(),
            login_label: tr("Telefonnummer (+49…)").into(),
            secret_label: tr("4-stellige App-PIN").into(),
            required_settings: vec![],
            note: tr("Web-Login mit 4-stelligem Code aus der App/SMS. Inoffizielle Schnittstelle – kann sich jederzeit ändern.").into(),
        },
        ConnectorKind::AmericanExpress => BankDescriptor {
            kind,
            name: "American Express".into(),
            protocol: tr("Direkt-Login (Web-Session) oder CSV/PDF-Kontoauszug").into(),
            bank_code: String::new(),
            sf_symbol: "creditcard.fill".into(),
            login_label: tr("Kartenbezeichnung").into(),
            secret_label: tr("(Anmeldung im Browser-Fenster)").into(),
            required_settings: vec![],
            note: tr("Anmeldung im eingebetteten Browser (Benutzername, Passwort, 2FA wie gewohnt) – keine API-Schlüssel nötig. Optional: eigene PSD2-/Enable-Banking-API unter „Erweitert“.").into(),
        },
        ConnectorKind::AmazonVisa => BankDescriptor {
            kind,
            name: "Amazon Visa".into(),
            protocol: tr("Direkt-Login (Web-Session) oder CSV/PDF-Kontoauszug").into(),
            bank_code: String::new(),
            sf_symbol: "creditcard".into(),
            login_label: tr("Kartenbezeichnung").into(),
            secret_label: tr("(Anmeldung im Browser-Fenster)").into(),
            required_settings: vec![],
            note: tr("Herausgeber: Open Bank S.A. (Zinia). Anmeldung im eingebetteten Browser – keine API-Schlüssel nötig. Optional: eigene PSD2-/Enable-Banking-API unter „Erweitert“.").into(),
        },
        ConnectorKind::AmazonOrders => BankDescriptor {
            kind,
            name: "Amazon Bestellungen".into(),
            protocol: tr("Direkt-Login (Web-Session)").into(),
            bank_code: String::new(),
            sf_symbol: "shippingbox.fill".into(),
            login_label: tr("Kontobezeichnung").into(),
            secret_label: tr("(Anmeldung im Browser-Fenster)").into(),
            required_settings: vec![],
            note: tr("Liest Ihre Bestellungen aus dem Amazon-Kundenkonto (amazon.de). Anmeldung im eingebetteten Browser inkl. Captcha/2FA – keine Zugangsdaten werden gespeichert.").into(),
        },
        ConnectorKind::EnableBanking => BankDescriptor {
            kind,
            name: tr("Bank per PSD2 (Enable Banking)").into(),
            protocol: tr("PSD2 über Enable Banking (eigene Anwendung)").into(),
            bank_code: String::new(),
            sf_symbol: "building.columns.circle.fill".into(),
            login_label: tr("Bezeichnung (z. B. Kontoname)").into(),
            secret_label: tr("(Zustimmung im Browser)").into(),
            required_settings: s(&[
                "enablebanking.application_id",
                "enablebanking.private_key_pem",
                "enablebanking.redirect_url",
            ]),
            note: tr("Banken wie N26, Revolut oder C24 per PSD2. Benötigt eine kostenlose eigene Anwendung auf enablebanking.com (Application-ID, privater Schlüssel, Redirect-URL). Die Zustimmung erteilen Sie im Browser Ihrer Bank.").into(),
        },
        ConnectorKind::ScalableCapital => BankDescriptor {
            kind,
            name: "Scalable Capital".into(),
            protocol: tr("OAuth-Geräteanmeldung + GraphQL-API der offiziellen CLI").into(),
            bank_code: String::new(),
            sf_symbol: "chart.bar.xaxis".into(),
            login_label: tr("E-Mail (nur Anzeige)").into(),
            secret_label: tr("(nicht nötig – Anmeldung im Browser)").into(),
            required_settings: vec![],
            note: tr("Vorher auf scalable.capital unter Profil → Sicherheit → „Agentic Investing“ den CLI-Zugriff einschalten. Die Anmeldung läuft im Browser und muss alle 7 Tage erneuert werden. Nur Lesezugriff.").into(),
        },
        ConnectorKind::Bunq => BankDescriptor {
            kind,
            name: "bunq".into(),
            protocol: tr("bunq Public API (eigener API-Schlüssel)").into(),
            bank_code: String::new(),
            sf_symbol: "building.columns.circle".into(),
            login_label: tr("E-Mail (nur Anzeige)").into(),
            secret_label: tr("API-Schlüssel").into(),
            required_settings: vec![],
            note: tr("API-Schlüssel in der bunq-App unter Profil → Einstellungen → Entwickler → API-Schlüssel erstellen und „Alle IP-Adressen erlauben“ aktivieren. Keine PSD2-Registrierung nötig.").into(),
        },
    }
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
pub struct ConnectContext {
    pub connection_id: i64,
    pub login_name: String,
    pub secret: SecretString,
    /// Resolved settings (product ID, API keys ...), secrets included.
    pub settings: HashMap<String, SecretString>,
    /// Opaque connector state persisted between syncs (FinTS system id, PSD2 session ...).
    pub state: serde_json::Value,
    /// Fetch transactions booked on or after this date (YYYY-MM-DD).
    pub since: String,
}

impl ConnectContext {
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
#[derive(Debug, Clone)]
pub struct RemoteHolding {
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
}

impl RemoteTransaction {
    /// Content-based id for banks that don't deliver a transaction id.
    pub fn content_id(&self, occurrence: u32) -> String {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (
            &self.booking_date,
            &self.value_date,
            self.amount_cents,
            &self.counterparty,
            &self.purpose,
            occurrence,
        )
            .hash(&mut h);
        format!("h{:016x}", h.finish())
    }
}

#[derive(Debug, Default)]
pub struct FetchResult {
    pub accounts: Vec<RemoteAccount>,
    pub transactions: Vec<RemoteTransaction>,
    /// Depot positions; replace the stored holdings of each listed account.
    pub holdings: Vec<RemoteHolding>,
    /// Updated connector state to persist.
    pub state: Option<serde_json::Value>,
    /// Keep stored balances and add newly imported bookings to them
    /// (for sources that don't report a balance).
    pub adjust_balances: bool,
    /// Accounts (external refs) whose statements were fetched automatically.
    pub statements_fetched: Vec<String>,
    /// PDFs fetched from the bank (trade confirmations, PostBox, statements).
    pub documents: Vec<crate::documents::RemoteDocument>,
    /// external_id prefix of provisional bookings (card holds) this fetch
    /// lists completely: stored ones missing from it are removed.
    pub hold_prefix: Option<String>,
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

pub fn make_connector(kind: ConnectorKind, demo: bool) -> Box<dyn BankConnector> {
    make_connector_for(kind, kind.as_str(), demo)
}

/// Like `make_connector`, but with the stored connector key so generic FinTS
/// connections get their BLZ (`fints:<blz>`).
pub fn make_connector_for(kind: ConnectorKind, key: &str, demo: bool) -> Box<dyn BankConnector> {
    if demo {
        return Box::new(demo::DemoConnector::new(kind));
    }
    match kind {
        ConnectorKind::FinTsGeneric => Box::new(fints::FinTsConnector::new(
            ConnectorKind::fints_blz(key).unwrap_or(""),
        )),
        ConnectorKind::VolksbankKasselGoettingen => {
            Box::new(fints::FinTsConnector::new("52090000"))
        }
        ConnectorKind::Direkt1822 => Box::new(fints::FinTsConnector::new("50050201")),
        ConnectorKind::Comdirect => Box::new(comdirect::ComdirectConnector::new()),
        ConnectorKind::PayPal => Box::new(paypal::PayPalConnector::new()),
        ConnectorKind::TradeRepublic => Box::new(trade_republic::TradeRepublicConnector::new()),
        ConnectorKind::AmericanExpress => Box::new(enable_banking::EnableBankingConnector::new(
            "enablebanking.aspsp.amex",
            "American Express",
            "enablebanking.country.amex",
        )),
        ConnectorKind::AmazonVisa => Box::new(enable_banking::EnableBankingConnector::new(
            "enablebanking.aspsp.amazonvisa",
            "Open Bank",
            "enablebanking.country.amazonvisa",
        )),
        ConnectorKind::AmazonOrders => Box::new(amazon_orders::WebOnlyConnector),
        ConnectorKind::Bunq => Box::new(bunq::BunqConnector::new()),
        ConnectorKind::ScalableCapital => Box::new(scalable::ScalableConnector::new()),
        ConnectorKind::EnableBanking => {
            let (country, aspsp) = ConnectorKind::enable_banking_target(key).unwrap_or(("DE", ""));
            Box::new(enable_banking::EnableBankingConnector::for_aspsp(
                aspsp, country,
            ))
        }
    }
}

pub(crate) fn http_client() -> CoreResult<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!(
            "GiroCrab/",
            env!("CARGO_PKG_VERSION"),
            " (open source; +https://github.com/)"
        ))
        .timeout(std::time::Duration::from_secs(60))
        .cookie_store(true)
        .build()?)
}

pub(crate) fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

// ------------------------------------------------------------------ sync session (FFI object)

struct SessionState {
    connector: Box<dyn BankConnector>,
    ctx: ConnectContext,
    done: bool,
}

/// One sync run for one bank connection. Created by `Vault::begin_sync`.
#[derive(uniffi::Object)]
pub struct SyncSession {
    vault: Arc<Vault>,
    kind: ConnectorKind,
    state: AsyncMutex<SessionState>,
}

impl SyncSession {
    async fn advance(
        &self,
        phase: CoreResult<Phase>,
        st: &mut SessionState,
    ) -> CoreResult<SyncStep> {
        match phase {
            Err(e) => {
                st.done = true;
                st.connector.finish(&st.ctx).await;
                Err(e)
            }
            Ok(Phase::Challenge(c)) => Ok(SyncStep::NeedsChallenge { challenge: c }),
            Ok(Phase::Pending(c)) => Ok(SyncStep::StillWaiting { challenge: c }),
            Ok(Phase::ConfirmPayee(_)) => {
                st.done = true;
                st.connector.finish(&st.ctx).await;
                Err(CoreError::Protocol {
                    message: "payee confirmation outside of a transfer".into(),
                })
            }
            Ok(Phase::Ready) => {
                crate::depot_debug::log(&format!(
                    "sync advance: Ready, calling fetch for {:?}",
                    self.kind
                ));
                let fetched = st.connector.fetch(&st.ctx).await;
                st.connector.finish(&st.ctx).await;
                st.done = true;
                let fetched = fetched?;
                let summary = self.store(st.ctx.connection_id, fetched)?;
                Ok(SyncStep::Finished { summary })
            }
        }
    }

    fn store(&self, connection_id: i64, data: FetchResult) -> CoreResult<SyncSummary> {
        store_fetch(&self.vault, self.kind, connection_id, data)
    }
}

/// Persists accounts, bookings and documents of one fetch (shared by bank
/// syncs and the keyless web-portal / statement import).
pub(crate) fn store_fetch(
    vault: &Vault,
    kind: ConnectorKind,
    connection_id: i64,
    data: FetchResult,
) -> CoreResult<SyncSummary> {
    {
        let db = vault.db()?;
        let tx = db.unchecked_transaction()?;
        let generic_bank_name = descriptor(kind).name;
        let bank_name = data
            .bank_name
            .clone()
            .filter(|b| !b.trim().is_empty())
            .unwrap_or_else(|| generic_bank_name.clone());
        let mut data = data;
        // safety net for every connector: repair UTF-8/Latin-1 mojibake
        for a in &mut data.accounts {
            a.name = crate::text::clean_bank_text(&a.name);
            a.owner = crate::text::clean_bank_text(&a.owner);
        }
        for t in &mut data.transactions {
            t.counterparty = crate::text::clean_bank_text(&t.counterparty);
            t.purpose = crate::text::clean_bank_text(&t.purpose);
            t.booking_text = crate::text::clean_bank_text(&t.booking_text);
        }
        for (old, new) in &data.renamed_refs {
            if old.is_empty() || new.is_empty() || old == new {
                continue;
            }
            tx.execute(
                "UPDATE accounts SET external_ref = ?3
                 WHERE connection_id = ?1 AND external_ref = ?2
                   AND NOT EXISTS (SELECT 1 FROM accounts WHERE connection_id = ?1 AND external_ref = ?3)",
                params![connection_id, old, new],
            )?;
        }
        for (r, old_name, new_name) in &data.name_updates {
            if new_name.trim().is_empty() || old_name == new_name {
                continue;
            }
            tx.execute(
                "UPDATE accounts SET name = ?4 WHERE connection_id = ?1 AND external_ref = ?2 AND name = ?3",
                params![connection_id, r, old_name, new_name],
            )?;
        }
        let mut account_ids: HashMap<String, i64> = HashMap::new();
        // Archived accounts the bank still delivers: nothing is stored for
        // them (no balance, bookings, positions or documents).
        let mut archived_ids: HashMap<String, i64> = HashMap::new();
        let mut accounts_updated = 0u32;
        let had_accounts: i64 = tx.query_row(
            "SELECT count(*) FROM accounts WHERE connection_id = ?1",
            [connection_id],
            |r| r.get(0),
        )?;
        let mut new_accounts: Vec<String> = Vec::new();
        for acc in &data.accounts {
            let existing: Option<i64> = tx
                .query_row(
                    "SELECT id FROM accounts WHERE connection_id = ?1 AND external_ref = ?2",
                    params![connection_id, acc.external_ref],
                    |r| r.get(0),
                )
                .optional()?;
            // First sync after a MoneyMoney database import: take over the
            // unconnected imported account with the same IBAN (its bookings are
            // then adopted via the import fingerprint) instead of a second account.
            let existing = match existing {
                Some(id) => Some(id),
                None => moneymoney_db::adopt_account(
                    &tx,
                    connection_id,
                    &acc.external_ref,
                    &acc.iban,
                    acc.kind,
                )?,
            };
            // Sub-accounts known from an earlier import/sync without a connection
            // (e.g. comdirect Depot / Tagesgeld): link them to this connection.
            let existing = match existing {
                Some(id) => Some(id),
                None => adopt_unlinked(&tx, connection_id, &bank_name, acc)?,
            };
            if let Some(id) = existing {
                let archived: bool = tx.query_row(
                    "SELECT archived_unix IS NOT NULL FROM accounts WHERE id = ?1",
                    [id],
                    |r| r.get(0),
                )?;
                if archived {
                    archived_ids.insert(acc.external_ref.clone(), id);
                    continue;
                }
            }
            let is_new = existing.is_none();
            let id = match existing {
                Some(id) => {
                    if !data.adjust_balances {
                        // A card reporting 0 (no /balances data) must not wipe a known balance.
                        let keep = acc.balance_cents == 0
                            && acc.kind == AccountKind::CreditCard
                            && tx.query_row(
                                "SELECT balance_cents FROM accounts WHERE id = ?1",
                                [id],
                                |r| r.get::<_, i64>(0),
                            )? != 0;
                        if keep {
                            tx.execute(
                                "UPDATE accounts SET currency = ?1 WHERE id = ?2",
                                params![acc.currency, id],
                            )?;
                        } else {
                            tx.execute(
                                "UPDATE accounts SET balance_cents = ?1, balance_date = ?2, currency = ?3 WHERE id = ?4",
                                params![acc.balance_cents, acc.balance_date, acc.currency, id],
                            )?;
                        }
                    }
                    // Bank name / owner learned later (e.g. "C24" instead of the
                    // generic "Bank per PSD2"); never overwrite what is set.
                    if bank_name != generic_bank_name {
                        tx.execute(
                            "UPDATE accounts SET bank_name = ?1 WHERE id = ?2 AND bank_name IN (?3, '')",
                            params![bank_name, id, generic_bank_name],
                        )?;
                    }
                    if !acc.owner.is_empty() {
                        tx.execute(
                            "UPDATE accounts SET owner = ?1 WHERE id = ?2 AND IFNULL(owner, '') = ''",
                            params![acc.owner, id],
                        )?;
                    }
                    // Fill in an account number learned later (e.g. a BBAN of a
                    // PSD2 account without IBAN); never overwrite a known one.
                    if !acc.account_number.is_empty() {
                        tx.execute(
                            "UPDATE accounts SET account_number = ?1 WHERE id = ?2 AND IFNULL(account_number, '') = ''",
                            params![acc.account_number, id],
                        )?;
                    }
                    // Trade Republic cash side is a clearing account, not a Girokonto.
                    if kind == ConnectorKind::TradeRepublic
                        && acc.kind != AccountKind::Brokerage
                        && acc.name == "Verrechnungskonto"
                    {
                        tx.execute(
                            "UPDATE accounts SET name = ?1 WHERE id = ?2 AND name = 'Girokonto'",
                            params![acc.name, id],
                        )?;
                    }
                    // Brokerage: depots found later. Savings: PSD2 savings accounts
                    // first stored as checking (e.g. C24 PocketZINS, typed by product).
                    if acc.kind == AccountKind::Brokerage || acc.kind == AccountKind::Savings {
                        let (group, sym, order) = group_for(acc.kind);
                        let gid = Vault::ensure_group(&tx, group, sym, order)?;
                        tx.execute(
                            "UPDATE accounts SET kind = ?1, sf_symbol = ?2, group_id = ?3 WHERE id = ?4 AND kind <> ?1",
                            params![acc.kind.as_str(), acc.kind.sf_symbol(), gid, id],
                        )?;
                    }
                    id
                }
                None => {
                    let (group, sym, order) = group_for(acc.kind);
                    let gid = Vault::ensure_group(&tx, group, sym, order)?;
                    tx.execute(
                        "INSERT INTO accounts(connection_id, group_id, external_ref, name, bank_name, owner, iban, account_number, currency, kind,
                                              balance_cents, balance_date, sf_symbol, sort_order)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, (SELECT IFNULL(MAX(sort_order),0)+1 FROM accounts))",
                        params![connection_id, gid, acc.external_ref, acc.name, bank_name, acc.owner, acc.iban, acc.account_number,
                                acc.currency, acc.kind.as_str(), acc.balance_cents, acc.balance_date, acc.kind.sf_symbol()],
                    )?;
                    let id = tx.last_insert_rowid();
                    crate::user_groups::place_new_account(&tx, id)?;
                    id
                }
            };
            accounts_updated += 1;
            account_ids.insert(acc.external_ref.clone(), id);
            if is_new && had_accounts > 0 {
                new_accounts.push(acc.name.clone());
            }
            // Delivered again: access is back (e.g. included in a new consent).
            tx.execute(
                "UPDATE accounts SET access_revoked = 0 WHERE id = ?1 AND access_revoked != 0",
                [id],
            )?;
        }
        let mut revoked_accounts: Vec<String> = Vec::new();
        let mut revoked_refs = data.revoked_refs.clone();
        // Archived accounts are the user's business: skipped above even when
        // the bank still delivers them, so they must not count as "missing".
        let delivered = |r: &str| account_ids.contains_key(r) || archived_ids.contains_key(r);
        if data.accounts_complete && !data.accounts.is_empty() {
            let mut q = tx.prepare(
                "SELECT external_ref FROM accounts
                 WHERE connection_id = ?1 AND access_revoked = 0 AND archived_unix IS NULL",
            )?;
            let stored: Vec<String> = q
                .query_map([connection_id], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            drop(q);
            revoked_refs.extend(stored.into_iter().filter(|r| !delivered(r)));
        }
        revoked_refs.sort();
        revoked_refs.dedup();
        for r in revoked_refs
            .iter()
            .filter(|r| !r.is_empty() && !delivered(r))
        {
            let name: Option<String> = tx
                .query_row(
                    "SELECT name FROM accounts WHERE connection_id = ?1 AND external_ref = ?2
                       AND access_revoked = 0 AND archived_unix IS NULL",
                    params![connection_id, r],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(name) = name {
                tx.execute(
                    "UPDATE accounts SET access_revoked = 1 WHERE connection_id = ?1 AND external_ref = ?2",
                    params![connection_id, r],
                )?;
                revoked_accounts.push(name);
            }
        }
        // Credit cards without a connector balance: derive the open amount.
        if !data.adjust_balances {
            for acc in data.accounts.iter().filter(|a| a.balance_cents == 0) {
                let Some(&id) = account_ids.get(&acc.external_ref) else {
                    continue;
                };
                let stored: i64 = tx.query_row(
                    "SELECT balance_cents FROM accounts WHERE id = ?1",
                    [id],
                    |r| r.get(0),
                )?;
                if stored != 0 {
                    continue;
                }
                let is_card = acc.kind == AccountKind::CreditCard
                    || is_amex_name(&acc.name)
                    || is_amex_name(bank_name.as_str());
                if !is_card {
                    continue;
                }
                let mut st = tx.prepare(
                    "SELECT booking_date, amount_cents, counterparty || ' ' || purpose || ' ' || booking_text
                     FROM transactions WHERE account_id = ?1 ORDER BY booking_date, id",
                )?;
                let rows: Vec<(String, i64, String)> = st
                    .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                    .collect::<Result<_, _>>()?;
                drop(st);
                if let Some(open) = open_card_balance(&rows) {
                    tx.execute(
                        "UPDATE accounts SET balance_cents = ?1 WHERE id = ?2",
                        params![open, id],
                    )?;
                }
            }
        }
        // Resolve each position to a stored account: external ref, else IBAN /
        // account number (depots often have no IBAN, numbers differ in leading
        // zeros), else the only depot of this fetch.
        let norm = |x: &str| {
            x.chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
                .trim_start_matches('0')
                .to_uppercase()
        };
        let depots: Vec<&RemoteAccount> = data
            .accounts
            .iter()
            .filter(|a| !archived_ids.contains_key(&a.external_ref))
            .filter(|a| a.kind == AccountKind::Brokerage || a.name.to_lowercase().contains("depot"))
            .collect();
        let resolve = |r: &str| -> Option<i64> {
            if let Some(id) = account_ids.get(r) {
                return Some(*id);
            }
            let n = norm(r);
            if !n.is_empty() {
                for a in &data.accounts {
                    if [&a.external_ref, &a.iban, &a.account_number]
                        .iter()
                        .any(|f| !f.is_empty() && norm(f) == n)
                    {
                        return account_ids.get(&a.external_ref).copied();
                    }
                }
            }
            match depots.as_slice() {
                [only] => account_ids.get(&only.external_ref).copied(),
                _ => None,
            }
        };
        let mut by_account: HashMap<i64, Vec<&crate::models::Holding>> = HashMap::new();
        let mut unresolved = 0usize;
        for h in &data.holdings {
            if let Some(id) = resolve(&h.account_ref) {
                by_account.entry(id).or_default().push(&h.holding);
            } else {
                unresolved += 1;
            }
        }
        crate::depot_debug::log(&format!(
            "store_fetch {kind:?}: incoming_holdings={} depot_accounts={} resolved_accounts={} unresolved={unresolved}",
            data.holdings.len(),
            depots.len(),
            by_account.len()
        ));
        for (acc_id, list) in &by_account {
            tx.execute("DELETE FROM holdings WHERE account_id = ?1", [acc_id])?;
            for h in list {
                tx.execute(
                    "INSERT INTO holdings(account_id, isin, wkn, name, quantity, current_price_cents, purchase_price_cents, value_cents)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![acc_id, h.isin, h.wkn, h.name, h.quantity, h.current_price_cents, h.purchase_price_cents, h.value_cents],
                )?;
            }
        }
        let mut imported = 0u32;
        let mut skipped_archived = 0u32;
        {
            let mut ins = tx.prepare(
                "INSERT OR IGNORE INTO transactions(account_id, external_id, booking_date, value_date, amount_cents, currency,
                         counterparty, counterparty_iban, purpose, booking_text, purpose_code, gvc)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            )?;
            for t in &data.transactions {
                let Some(acc_id) = account_ids.get(&t.account_ref) else {
                    if let Some(id) = archived_ids.get(&t.account_ref) {
                        let known: bool = tx.query_row(
                            "SELECT EXISTS (SELECT 1 FROM transactions WHERE account_id = ?1 AND external_id = ?2)",
                            params![id, t.external_id],
                            |r| r.get(0),
                        )?;
                        skipped_archived += u32::from(!known);
                    }
                    continue;
                };
                // Already imported from MoneyMoney? Adopt that row (keeps category + note).
                if moneymoney_import::adopt_import(
                    &tx,
                    *acc_id,
                    &t.external_id,
                    &t.booking_date,
                    t.amount_cents,
                    &t.counterparty_iban,
                    &t.counterparty,
                    &t.purpose,
                    &t.booking_text,
                )? {
                    continue;
                }
                let n = ins.execute(params![
                    acc_id,
                    t.external_id,
                    t.booking_date,
                    t.value_date,
                    t.amount_cents,
                    t.currency,
                    t.counterparty,
                    t.counterparty_iban,
                    t.purpose,
                    t.booking_text,
                    t.purpose_code,
                    t.gvc
                ])? as u32;
                if n > 0 && data.adjust_balances {
                    tx.execute(
                        "UPDATE accounts SET balance_cents = balance_cents + ?1, balance_date = ?2 WHERE id = ?3",
                        params![t.amount_cents, t.booking_date, acc_id],
                    )?;
                }
                imported += n;
                if n == 0 {
                    // Old duplicate: bank row exists although an MoneyMoney twin does too.
                    if moneymoney_import::merge_synced_twin(
                        &tx,
                        *acc_id,
                        &t.external_id,
                        &t.booking_date,
                        t.amount_cents,
                        &t.counterparty_iban,
                        &t.counterparty,
                        &t.purpose,
                    )? {
                        continue;
                    }
                    // Existing row: heal corrupted text (U+FFFD / mojibake) with freshly decoded text.
                    let bad = |s: &str| s.contains('\u{FFFD}') || crate::text::has_mojibake(s);
                    let good = |s: &str| !bad(s);
                    let existing: Option<(i64, String, String)> = tx
                        .query_row(
                            "SELECT id, counterparty, purpose FROM transactions WHERE account_id = ?1 AND external_id = ?2",
                            params![acc_id, t.external_id],
                            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                        )
                        .ok();
                    if let Some((row_id, cp, pu)) = existing {
                        // Backfill bank codes for bookings imported before they were stored.
                        if !t.purpose_code.is_empty() || !t.gvc.is_empty() {
                            tx.execute(
                                "UPDATE transactions SET purpose_code = ?1, gvc = ?2
                                 WHERE id = ?3 AND purpose_code = '' AND gvc = ''",
                                params![t.purpose_code, t.gvc, row_id],
                            )?;
                        }
                        if bad(&cp) && good(&t.counterparty) {
                            tx.execute(
                                "UPDATE transactions SET counterparty = ?1 WHERE id = ?2",
                                params![t.counterparty, row_id],
                            )?;
                        }
                        if bad(&pu) && good(&t.purpose) {
                            tx.execute(
                                "UPDATE transactions SET purpose = ?1 WHERE id = ?2",
                                params![t.purpose, row_id],
                            )?;
                        }
                    }
                }
            }
        }
        // Drop optimistic transfer bookings once the bank's real booking arrived
        // (same account, amount and recipient IBAN) or after 14 days.
        tx.execute(
            "DELETE FROM transactions WHERE external_id LIKE 'pending-%' AND (
                imported_unix < strftime('%s','now') - 14*86400 OR EXISTS (
                    SELECT 1 FROM transactions r WHERE r.account_id = transactions.account_id
                      AND r.external_id NOT LIKE 'pending-%' AND r.amount_cents = transactions.amount_cents
                      AND r.counterparty_iban = transactions.counterparty_iban
                      AND r.booking_date >= transactions.booking_date))",
            [],
        )?;
        // Card holds: keep exactly the ones the bank still reports as open.
        if let Some(prefix) = data.hold_prefix.as_deref().filter(|p| !p.is_empty()) {
            let current: std::collections::HashSet<&str> = data
                .transactions
                .iter()
                .filter(|t| t.external_id.starts_with(prefix))
                .map(|t| t.external_id.as_str())
                .collect();
            let like = format!("{}%", prefix.replace('%', "").replace('_', "\\_"));
            for acc_id in account_ids.values() {
                let stale: Vec<(i64, String)> = {
                    let mut q = tx.prepare(
                        "SELECT id, external_id FROM transactions
                         WHERE account_id = ?1 AND external_id LIKE ?2 ESCAPE '\\'",
                    )?;
                    let rows =
                        q.query_map(params![acc_id, like], |r| Ok((r.get(0)?, r.get(1)?)))?;
                    rows.collect::<Result<_, _>>()?
                };
                for (row_id, ext) in stale {
                    if !current.contains(ext.as_str()) {
                        tx.execute("DELETE FROM transactions WHERE id = ?1", [row_id])?;
                    }
                }
            }
        }
        for d in &data.documents {
            if archived_ids.contains_key(&d.account_ref) {
                continue;
            }
            let by_ref = |tx: &rusqlite::Transaction| -> Option<i64> {
                tx.query_row(
                    "SELECT id FROM accounts WHERE connection_id = ?1 AND external_ref = ?2",
                    params![connection_id, d.account_ref],
                    |r| r.get(0),
                )
                .optional()
                .ok()
                .flatten()
            };
            let Some(acc_id) = account_ids
                .get(&d.account_ref)
                .copied()
                .or_else(|| by_ref(&tx))
                .or_else(|| account_ids.values().next().copied())
            else {
                continue;
            };
            let booking_id: Option<i64> = match &d.booking_external_id {
                Some(ext) => tx
                    .query_row(
                        "SELECT id FROM transactions WHERE account_id = ?1 AND external_id = ?2",
                        params![acc_id, ext],
                        |r| r.get(0),
                    )
                    .optional()?,
                None => None,
            };
            let _ = crate::documents::insert_doc(
                &tx,
                booking_id,
                Some(acc_id),
                &d.title,
                &d.doc_type,
                &d.mime_type,
                &d.file_name,
                &d.bytes,
                &d.source_url,
                &d.date,
            );
        }
        for r in &data.statements_fetched {
            if let Some(id) = account_ids.get(r) {
                tx.execute(
                    "INSERT INTO settings(key, value) VALUES (?1, ?2)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![statements_last_key(*id), today()],
                )?;
            }
        }
        for acc in data.accounts.iter().filter(|a| a.kind == AccountKind::Loan) {
            if let Some(id) = account_ids.get(&acc.external_ref) {
                Vault::derive_initial_balance(&tx, *id)?;
            }
        }
        let categorized = Vault::apply_rules_inner(&tx, true)?;
        if let Some(state) = data.state {
            tx.execute(
                "UPDATE bank_connections SET state_json = ?1 WHERE id = ?2",
                params![state.to_string(), connection_id],
            )?;
        }
        tx.execute(
            "UPDATE bank_connections SET last_sync_unix = strftime('%s','now') WHERE id = ?1",
            [connection_id],
        )?;
        tx.commit()?;
        Ok(SyncSummary {
            accounts_updated,
            transactions_imported: imported,
            transactions_categorized: categorized,
            transactions_skipped_archived: skipped_archived,
            new_accounts,
            revoked_accounts,
        })
    }
}

/// Sidebar sections (fixed order).
pub(crate) fn group_for(kind: AccountKind) -> (&'static str, &'static str, i32) {
    match kind {
        AccountKind::Checking
        | AccountKind::Savings
        | AccountKind::Cash
        | AccountKind::PaymentService => ("Giro & Tagesgeld", "building.columns", 1),
        AccountKind::CreditCard => ("Kreditkarten", "creditcard", 2),
        AccountKind::Brokerage => ("Depots & ETFs", "chart.pie", 3),
        AccountKind::Loan => ("Darlehen & Kredite", "house", 4),
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl SyncSession {
    /// Phase 1: log in. Returns a challenge or finishes the sync.
    pub async fn start(&self) -> Result<SyncStep, CoreError> {
        let mut st = self.state.lock().await;
        if st.done {
            return Err(CoreError::SessionClosed);
        }
        let SessionState { connector, ctx, .. } = &mut *st;
        let phase = connector.begin(ctx).await;
        self.advance(phase, &mut st).await
    }

    /// Phase 2a: submit the code / redirect URL the user typed.
    pub async fn submit_code(&self, code: String) -> Result<SyncStep, CoreError> {
        let mut st = self.state.lock().await;
        if st.done {
            return Err(CoreError::SessionClosed);
        }
        let SessionState { connector, ctx, .. } = &mut *st;
        let phase = connector
            .respond(ctx, ChallengeResponse::Code(SecretString::new(code)))
            .await;
        self.advance(phase, &mut st).await
    }

    /// Phase 2b: ask whether the decoupled push approval happened.
    pub async fn poll(&self) -> Result<SyncStep, CoreError> {
        let mut st = self.state.lock().await;
        if st.done {
            return Err(CoreError::SessionClosed);
        }
        let SessionState { connector, ctx, .. } = &mut *st;
        let phase = connector.respond(ctx, ChallengeResponse::Poll).await;
        self.advance(phase, &mut st).await
    }

    /// Cancels the sync and closes the bank dialog.
    pub async fn cancel(&self) {
        let mut st = self.state.lock().await;
        if !st.done {
            let SessionState { connector, ctx, .. } = &mut *st;
            connector.finish(ctx).await;
            st.done = true;
        }
    }

    pub fn bank_name(&self) -> String {
        descriptor(self.kind).name
    }
}

/// Setting key marking a statements-only session (value = external_ref of the account).
pub(crate) const STATEMENTS_ONLY: &str = "__statements_only";
/// Setting listing the accounts (external refs, comma-separated) whose PDF
/// statements a regular sync should also fetch (opt-in, GitHub #57).
pub(crate) const STATEMENTS_AUTO: &str = "__statements_auto";
/// Days between two automatic statement fetches of one account.
const STATEMENTS_AUTO_DAYS: i64 = 7;

/// Vault setting key: fetch PDF statements of this account automatically.
pub(crate) fn statements_auto_key(account_id: i64) -> String {
    format!("statements.auto.{account_id}")
}

/// Vault setting key: date (YYYY-MM-DD) of the last automatic statement fetch.
pub(crate) fn statements_last_key(account_id: i64) -> String {
    format!("statements.last.{account_id}")
}

#[uniffi::export]
impl Vault {
    /// Prepares a sync for one bank connection. `secret` is the PIN/password
    /// (wiped from memory when the session ends). With `demo = true` the bank
    /// is simulated (push-TAN / code challenge included) – no network.
    pub fn begin_sync(
        self: Arc<Self>,
        connection_id: i64,
        secret: String,
        demo: bool,
    ) -> Result<Arc<SyncSession>, CoreError> {
        self.begin_sync_inner(connection_id, secret, demo, None)
    }

    /// Turns the automatic PDF statement fetch (FinTS HKEKA, at most weekly,
    /// during regular syncs) of one account on or off.
    pub fn set_auto_statements(&self, account_id: i64, enabled: bool) -> Result<(), CoreError> {
        let db = self.db()?;
        if enabled {
            db.execute(
                "INSERT INTO settings(key, value) VALUES (?1, '1')
                 ON CONFLICT(key) DO UPDATE SET value = '1'",
                [statements_auto_key(account_id)],
            )?;
        } else {
            db.execute(
                "DELETE FROM settings WHERE key IN (?1, ?2)",
                params![
                    statements_auto_key(account_id),
                    statements_last_key(account_id)
                ],
            )?;
        }
        Ok(())
    }

    /// Is the automatic statement fetch on for this account?
    pub fn auto_statements_enabled(&self, account_id: i64) -> Result<bool, CoreError> {
        let db = self.db()?;
        Ok(Vault::setting_inner(&db, &statements_auto_key(account_id))?.as_deref() == Some("1"))
    }

    /// Explicitly fetches the electronic bank statements (FinTS HKEKA, PDF) of ONE account
    /// into the document archive. Same session / TAN flow as `begin_sync`; bookings and
    /// balances stay untouched. Regular syncs never fetch statements.
    pub fn fetch_account_statements(
        self: Arc<Self>,
        account_id: i64,
        secret: String,
        demo: bool,
    ) -> Result<Arc<SyncSession>, CoreError> {
        let (conn_id, ext): (Option<i64>, String) = {
            let db = self.db()?;
            db.query_row(
                "SELECT connection_id, external_ref FROM accounts WHERE id = ?1",
                [account_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| CoreError::NotFound {
                what: trf("Konto {0}", &[&account_id]),
            })?
        };
        let conn_id = conn_id.ok_or_else(|| CoreError::InvalidInput {
            message: tr("Dieses Konto hat keine Bankverbindung.").into(),
        })?;
        let kind: String = self.db()?.query_row(
            "SELECT connector FROM bank_connections WHERE id = ?1",
            [conn_id],
            |r| r.get(0),
        )?;
        let is_fints = ConnectorKind::from_str(&kind).is_some_and(|k| {
            matches!(
                k,
                ConnectorKind::FinTsGeneric
                    | ConnectorKind::VolksbankKasselGoettingen
                    | ConnectorKind::Direkt1822
            )
        });
        if !is_fints {
            return Err(CoreError::InvalidInput {
                message: tr("Kontoauszüge können nur per FinTS abgerufen werden.").into(),
            });
        }
        self.begin_sync_inner(conn_id, secret, demo, Some(ext))
    }
}

impl Vault {
    /// External refs of this connection's accounts whose automatic statement
    /// fetch is on and due (never, or more than `STATEMENTS_AUTO_DAYS` ago).
    fn statements_due(&self, connection_id: i64) -> CoreResult<Vec<String>> {
        let db = self.db()?;
        let mut stmt = db.prepare(
            "SELECT id, external_ref FROM accounts WHERE connection_id = ?1 AND kind != 'loan'",
        )?;
        let accounts: Vec<(i64, String)> = stmt
            .query_map([connection_id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let today = chrono::Local::now().date_naive();
        let mut due = Vec::new();
        for (id, ext) in accounts {
            if Vault::setting_inner(&db, &statements_auto_key(id))?.as_deref() != Some("1") {
                continue;
            }
            let last = Vault::setting_inner(&db, &statements_last_key(id))?
                .and_then(|d| chrono::NaiveDate::parse_from_str(&d, "%Y-%m-%d").ok());
            if last.is_none_or(|d| (today - d).num_days() >= STATEMENTS_AUTO_DAYS) {
                due.push(ext);
            }
        }
        Ok(due)
    }

    fn begin_sync_inner(
        self: Arc<Self>,
        connection_id: i64,
        secret: String,
        demo: bool,
        statements_only: Option<String>,
    ) -> Result<Arc<SyncSession>, CoreError> {
        // No PIN typed: use the one stored in the vault ("PIN im Tresor speichern").
        let secret = if secret.is_empty() {
            let db = self.db()?;
            Vault::setting_inner(&db, &crate::connections::pin_key(connection_id))?
                .unwrap_or_default()
        } else {
            secret
        };
        let secret = SecretString::new(secret);
        let (kind, key_str, login, state_json, ctx_settings, since) = {
            let db = self.db()?;
            let (kind, login, state): (String, String, String) = db
                .query_row(
                    "SELECT connector, login_name, state_json FROM bank_connections WHERE id = ?1",
                    [connection_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?
                .ok_or_else(|| CoreError::NotFound {
                    what: trf("Bankverbindung {0}", &[&connection_id]),
                })?;
            let key_str = kind.clone();
            let kind = ConnectorKind::from_str(&kind).ok_or_else(|| CoreError::InvalidInput {
                message: format!("unknown connector {kind}"),
            })?;
            let mut settings = HashMap::new();
            for (key, ..) in crate::vault::KNOWN_SETTINGS {
                if let Some(v) = Vault::setting_inner(&db, key)? {
                    settings.insert(key.to_string(), SecretString::new(v));
                }
            }
            if let Some(blz) = ConnectorKind::fints_blz(&key_str) {
                let k = format!("fints.url.{blz}");
                match Vault::setting_inner(&db, &k)? {
                    Some(v) if !v.is_empty() => {
                        settings.insert(k, SecretString::new(v));
                    }
                    // e.g. a MoneyMoney import created the login without a URL
                    _ => {
                        if let Some(url) = fints_directory::url_for_blz(blz) {
                            settings.insert(k, SecretString::new(url.to_string()));
                        }
                    }
                }
            }
            let tm_key = format!("fints.tan_medium.{connection_id}");
            if let Some(v) = Vault::setting_inner(&db, &tm_key)? {
                if !v.trim().is_empty() {
                    settings.insert(tm_key, SecretString::new(v));
                }
            }
            if !demo {
                for key in descriptor(kind).required_settings {
                    if !settings.contains_key(&key) {
                        return Err(CoreError::MissingSetting { key });
                    }
                }
            }
            // Incremental: from the newest booking minus 10 days, else N days back.
            let newest: Option<String> = db
                .query_row(
                    "SELECT MAX(t.booking_date) FROM transactions t JOIN accounts a ON a.id = t.account_id WHERE a.connection_id = ?1",
                    [connection_id],
                    |r| r.get(0),
                )
                .optional()?
                .flatten();
            let days: i64 = Vault::setting_inner(&db, "sync.days_back")?
                .and_then(|d| d.parse().ok())
                .unwrap_or(90);
            let since =
                match newest.and_then(|d| chrono::NaiveDate::parse_from_str(&d, "%Y-%m-%d").ok()) {
                    Some(d) => (d - chrono::Duration::days(10))
                        .format("%Y-%m-%d")
                        .to_string(),
                    None => (chrono::Local::now().date_naive() - chrono::Duration::days(days))
                        .format("%Y-%m-%d")
                        .to_string(),
                };
            (kind, key_str, login, state, settings, since)
        };
        let mut ctx_settings = ctx_settings;
        if let Some(ext) = statements_only {
            ctx_settings.insert(STATEMENTS_ONLY.into(), SecretString::new(ext));
        } else {
            let due = self.statements_due(connection_id)?;
            if !due.is_empty() {
                ctx_settings.insert(STATEMENTS_AUTO.into(), SecretString::new(due.join(",")));
            }
        }
        let ctx = ConnectContext {
            connection_id,
            login_name: login,
            secret,
            settings: ctx_settings,
            state: serde_json::from_str(&state_json).unwrap_or(serde_json::json!({})),
            since,
        };
        Ok(Arc::new(SyncSession {
            vault: self.clone(),
            kind,
            state: AsyncMutex::new(SessionState {
                connector: make_connector_for(kind, &key_str, demo),
                ctx,
                done: false,
            }),
        }))
    }
}

// ------------------------------------------------------------------ SEPA transfer session

struct TransferState {
    connector: Box<dyn BankConnector>,
    ctx: ConnectContext,
    job: TransferJob,
    account_id: i64,
    /// false = still logging in, true = transfer submitted (waiting for TAN).
    submitted: bool,
    done: bool,
}

/// One SEPA transfer. Created by `Vault::begin_transfer`; same two-phase
/// challenge handling as `SyncSession` (login SCA, then transfer SCA).
/// After the order was handed to the bank, only errors that prove it was
/// refused (wrong credentials, invalid input) mean "not executed". Anything
/// else (network, garbled reply, ...) leaves the result unknown.
fn error_leaves_outcome_unknown(e: &CoreError) -> bool {
    !matches!(
        e,
        CoreError::Authentication { .. } | CoreError::InvalidInput { .. }
    )
}

#[derive(uniffi::Object)]
pub struct TransferSession {
    vault: Arc<Vault>,
    state: AsyncMutex<TransferState>,
}

impl TransferSession {
    async fn advance(
        &self,
        phase: CoreResult<Phase>,
        st: &mut TransferState,
    ) -> CoreResult<TransferStep> {
        let mut phase = phase;
        loop {
            match phase {
                Err(e) => {
                    st.done = true;
                    // A dropped connection after the order reached the bank:
                    // it may have been executed, so never report "failed".
                    let unknown = st.submitted
                        && st.connector.order_submitted()
                        && error_leaves_outcome_unknown(&e);
                    st.connector.finish(&st.ctx).await;
                    if unknown {
                        return self.outcome_unknown(st, e.to_string());
                    }
                    return Err(e);
                }
                Ok(Phase::Challenge(c)) => {
                    return Ok(TransferStep::NeedsChallenge { challenge: c })
                }
                Ok(Phase::Pending(c)) => return Ok(TransferStep::StillWaiting { challenge: c }),
                Ok(Phase::ConfirmPayee(check)) => {
                    return Ok(TransferStep::NeedsPayeeConfirmation { check })
                }
                Ok(Phase::Ready) if !st.submitted => {
                    st.submitted = true;
                    phase = st.connector.send_transfer(&st.ctx, &st.job).await;
                }
                Ok(Phase::Ready) => {
                    st.connector.finish(&st.ctx).await;
                    st.done = true;
                    return match st.connector.transfer_result() {
                        TransferResult::Executed { instant } => {
                            self.vault.record_pending_transfer(
                                st.account_id,
                                &st.job,
                                if instant {
                                    "Echtzeitüberweisung (vorgemerkt)"
                                } else {
                                    "Überweisung (vorgemerkt)"
                                },
                            )?;
                            Ok(TransferStep::Finished {
                                outcome: TransferOutcome {
                                    instant,
                                    end_to_end_id: st.job.end_to_end_id.clone(),
                                },
                            })
                        }
                        TransferResult::InstantDeclined { reason } => {
                            Ok(TransferStep::InstantDeclined { reason })
                        }
                        TransferResult::Unknown { reason } => self.outcome_unknown(st, reason),
                    };
                }
            }
        }
    }

    /// Stores a booking marked "Status unklar" so the possibly executed order
    /// stays visible (until the bank's booking arrives or 14 days pass).
    fn outcome_unknown(&self, st: &TransferState, reason: String) -> CoreResult<TransferStep> {
        self.vault.record_pending_transfer(
            st.account_id,
            &st.job,
            if st.job.instant {
                "Echtzeitüberweisung (Status unklar)"
            } else {
                "Überweisung (Status unklar)"
            },
        )?;
        Ok(TransferStep::OutcomeUnknown {
            reason,
            end_to_end_id: st.job.end_to_end_id.clone(),
        })
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl TransferSession {
    pub async fn start(&self) -> Result<TransferStep, CoreError> {
        let mut st = self.state.lock().await;
        if st.done {
            return Err(CoreError::SessionClosed);
        }
        let TransferState { connector, ctx, .. } = &mut *st;
        let phase = connector.begin(ctx).await;
        self.advance(phase, &mut st).await
    }

    pub async fn submit_code(&self, code: String) -> Result<TransferStep, CoreError> {
        let mut st = self.state.lock().await;
        if st.done {
            return Err(CoreError::SessionClosed);
        }
        let TransferState { connector, ctx, .. } = &mut *st;
        let phase = connector
            .respond(ctx, ChallengeResponse::Code(SecretString::new(code)))
            .await;
        self.advance(phase, &mut st).await
    }

    pub async fn poll(&self) -> Result<TransferStep, CoreError> {
        let mut st = self.state.lock().await;
        if st.done {
            return Err(CoreError::SessionClosed);
        }
        let TransferState { connector, ctx, .. } = &mut *st;
        let phase = connector.respond(ctx, ChallengeResponse::Poll).await;
        self.advance(phase, &mut st).await
    }

    /// Send the transfer although the name check did not confirm the
    /// recipient (after `NeedsPayeeConfirmation`).
    pub async fn confirm_payee(&self) -> Result<TransferStep, CoreError> {
        let mut st = self.state.lock().await;
        if st.done {
            return Err(CoreError::SessionClosed);
        }
        let TransferState { connector, ctx, .. } = &mut *st;
        let phase = connector
            .respond(ctx, ChallengeResponse::ConfirmPayee)
            .await;
        self.advance(phase, &mut st).await
    }

    pub async fn cancel(&self) {
        let mut st = self.state.lock().await;
        if !st.done {
            let TransferState { connector, ctx, .. } = &mut *st;
            connector.finish(ctx).await;
            st.done = true;
        }
    }
}

#[uniffi::export]
impl Vault {
    /// Prepares a SEPA transfer from a FinTS account. `secret` = PIN (empty =
    /// PIN stored in the vault). `demo` simulates the bank.
    pub fn begin_transfer(
        self: Arc<Self>,
        request: SepaTransferRequest,
        secret: String,
        demo: bool,
    ) -> Result<Arc<TransferSession>, CoreError> {
        use crate::connectors::fints::sepa;
        if request.amount_cents <= 0 {
            return Err(CoreError::InvalidInput {
                message: tr("Der Betrag muss größer als 0 sein.").into(),
            });
        }
        if !sepa::validate_iban(&request.recipient_iban) {
            return Err(CoreError::InvalidInput {
                message: tr("Die Empfänger-IBAN ist ungültig.").into(),
            });
        }
        if let Some(b) = request
            .recipient_bic
            .as_deref()
            .filter(|b| !b.trim().is_empty())
        {
            if !sepa::validate_bic(b) {
                return Err(CoreError::InvalidInput {
                    message: tr("Die BIC ist ungültig.").into(),
                });
            }
        }
        if request.recipient_name.trim().is_empty() {
            return Err(CoreError::InvalidInput {
                message: tr("Der Empfängername fehlt.").into(),
            });
        }
        if request.purpose.chars().count() > 140 {
            return Err(CoreError::InvalidInput {
                message: tr("Der Verwendungszweck darf höchstens 140 Zeichen lang sein.").into(),
            });
        }
        let (connection_id, iban, owner) = {
            let db = self.db()?;
            let row: (Option<i64>, String, String) = db
                .query_row(
                    "SELECT connection_id, iban, owner FROM accounts WHERE id = ?1",
                    [request.account_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?
                .ok_or_else(|| CoreError::NotFound {
                    what: trf("Konto {0}", &[&request.account_id]),
                })?;
            let cid = row.0.ok_or_else(|| CoreError::InvalidInput {
                message: tr("Dieses Konto hat keine Bankverbindung.").into(),
            })?;
            (cid, row.1, row.2)
        };
        let key_str: String = {
            let db = self.db()?;
            db.query_row(
                "SELECT connector FROM bank_connections WHERE id = ?1",
                [connection_id],
                |r| r.get(0),
            )?
        };
        let kind = ConnectorKind::from_str(&key_str).ok_or_else(|| CoreError::InvalidInput {
            message: format!("unknown connector {key_str}"),
        })?;
        if !matches!(
            kind,
            ConnectorKind::VolksbankKasselGoettingen
                | ConnectorKind::Direkt1822
                | ConnectorKind::FinTsGeneric
                | ConnectorKind::Bunq
        ) {
            return Err(CoreError::InvalidInput {
                message: tr("Überweisungen sind nur für FinTS- und bunq-Konten möglich.").into(),
            });
        }
        let exec_date = request
            .execution_date
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .map(str::to_string);
        if request.instant && exec_date.is_some() {
            return Err(CoreError::InvalidInput {
                message: tr("Echtzeitüberweisungen können nicht terminiert werden.").into(),
            });
        }
        if let Some(d) = &exec_date {
            let parsed = chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").map_err(|_| {
                CoreError::InvalidInput {
                    message: tr("Das Ausführungsdatum ist ungültig (JJJJ-MM-TT).").into(),
                }
            })?;
            if parsed <= chrono::Local::now().date_naive() {
                return Err(CoreError::InvalidInput {
                    message: tr("Das Ausführungsdatum muss in der Zukunft liegen.").into(),
                });
            }
        }
        if (request.instant || exec_date.is_some()) && matches!(kind, ConnectorKind::Bunq) {
            return Err(CoreError::InvalidInput {
                message: tr("Echtzeit- und Terminüberweisungen sind nur für FinTS-Konten möglich.")
                    .into(),
            });
        }
        if iban.is_empty() {
            return Err(CoreError::InvalidInput {
                message: tr("Das Konto hat keine IBAN.").into(),
            });
        }
        // Reuse the sync session set-up (settings, PIN, connector state).
        let sync = self.clone().begin_sync(connection_id, secret, demo)?;
        let ctx = {
            let st = sync
                .state
                .try_lock()
                .map_err(|_| CoreError::SessionClosed)?;
            ConnectContext {
                connection_id: st.ctx.connection_id,
                login_name: st.ctx.login_name.clone(),
                secret: SecretString::new(st.ctx.secret.expose().to_string()),
                settings: st.ctx.settings.clone(),
                state: st.ctx.state.clone(),
                since: st.ctx.since.clone(),
            }
        };
        let job = TransferJob {
            source_iban: iban,
            source_owner: owner,
            recipient_name: request.recipient_name.trim().to_string(),
            recipient_iban: sepa::normalize_iban(&request.recipient_iban),
            recipient_bic: request
                .recipient_bic
                .filter(|b| !b.trim().is_empty())
                .map(|b| sepa::normalize_iban(&b)),
            amount_cents: request.amount_cents,
            purpose: request.purpose.trim().to_string(),
            instant: request.instant,
            execution_date: exec_date,
            end_to_end_id: new_end_to_end_id(),
        };
        Ok(Arc::new(TransferSession {
            vault: self.clone(),
            state: AsyncMutex::new(TransferState {
                connector: make_connector_for(kind, &key_str, demo),
                ctx,
                job,
                account_id: request.account_id,
                submitted: false,
                done: false,
            }),
        }))
    }

    /// Known recipients (name + IBAN) from past outgoing bookings, most used
    /// first. `query` filters by name/IBAN substring (empty = all).
    pub fn suggest_recipients(
        &self,
        query: String,
        limit: u32,
    ) -> Result<Vec<KnownRecipient>, CoreError> {
        let db = self.db()?;
        let q = query.trim().replace(' ', "").to_lowercase();
        let mut stmt = db.prepare(
            "SELECT counterparty, counterparty_iban, count(*) AS n FROM transactions
             WHERE counterparty_iban <> '' AND counterparty <> '' AND amount_cents < 0
             GROUP BY counterparty, counterparty_iban
             ORDER BY n DESC, MAX(booking_date) DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(KnownRecipient {
                name: r.get(0)?,
                iban: r.get(1)?,
                times_used: r.get(2)?,
            })
        })?;
        Ok(rows
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|r| {
                q.is_empty()
                    || r.name.replace(' ', "").to_lowercase().contains(&q)
                    || r.iban.replace(' ', "").to_lowercase().contains(&q)
            })
            .filter(|r| crate::connectors::fints::sepa::validate_iban(&r.iban))
            .take(limit as usize)
            .collect())
    }
}

impl Vault {
    /// Optimistic booking after a confirmed transfer. It disappears as soon
    /// as the bank's own booking for it arrives in a sync.
    pub(crate) fn record_pending_transfer(
        &self,
        account_id: i64,
        job: &TransferJob,
        booking_text: &str,
    ) -> CoreResult<()> {
        let db = self.db()?;
        db.execute(
            "INSERT INTO transactions(account_id, external_id, booking_date, value_date, amount_cents, currency,
                     counterparty, counterparty_iban, purpose, booking_text)
             VALUES (?1, ?2, ?3, ?3, ?4, 'EUR', ?5, ?6, ?7, ?8)",
            params![
                account_id,
                format!("pending-{}", job.end_to_end_id),
                today(),
                -job.amount_cents,
                job.recipient_name,
                job.recipient_iban,
                job.purpose,
                booking_text
            ],
        )?;
        Vault::apply_rules_inner(&db, true)?;
        Ok(())
    }
}

/// "GC" + 30 hex digits: unique, within the 35-char SEPA limit and the
/// restricted character set.
fn new_end_to_end_id() -> String {
    format!(
        "GC{}",
        &uuid::Uuid::new_v4().simple().to_string().to_uppercase()[..30]
    )
}

/// ISO 13616 Mod-97 IBAN check (whitespace and case ignored).
#[uniffi::export]
pub fn validate_iban(iban: String) -> bool {
    crate::connectors::fints::sepa::validate_iban(&iban)
}

/// Links an unconnected stored account to `connection_id` when it matches the
/// remote one by account number / IBAN (ignoring spaces and leading zeros) or is
/// the single unlinked depot of the same bank. Sets `connection_id`, `external_ref`, `kind`.
fn adopt_unlinked(
    tx: &rusqlite::Connection,
    connection_id: i64,
    bank_name: &str,
    acc: &RemoteAccount,
) -> CoreResult<Option<i64>> {
    let norm = |x: &str| {
        x.chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .trim_start_matches('0')
            .to_uppercase()
    };
    let mut st = tx.prepare(
        "SELECT id, IFNULL(iban,''), IFNULL(account_number,''), name, IFNULL(bank_name,'') FROM accounts WHERE connection_id IS NULL",
    )?;
    let rows: Vec<(i64, String, String, String, String)> = st
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<Result<_, _>>()?;
    drop(st);
    let ours = [
        norm(&acc.iban),
        norm(&acc.account_number),
        norm(&acc.external_ref),
    ];
    let mut found = rows
        .iter()
        .find(|(_, iban, num, _, _)| {
            [norm(iban), norm(num)]
                .iter()
                .any(|f| !f.is_empty() && ours.contains(f))
        })
        .map(|r| r.0);
    if found.is_none()
        && (acc.kind == AccountKind::Brokerage || acc.name.to_lowercase().contains("depot"))
    {
        let same_bank = |b: &str| b.eq_ignore_ascii_case(bank_name);
        let depots: Vec<i64> = rows
            .iter()
            .filter(|(_, _, _, name, bank)| {
                name.to_lowercase().contains("depot") && same_bank(bank)
            })
            .map(|r| r.0)
            .collect();
        if let [only] = depots.as_slice() {
            found = Some(*only);
        }
    }
    if let Some(id) = found {
        tx.execute(
            "UPDATE accounts SET connection_id = ?1, external_ref = ?2, kind = ?3 WHERE id = ?4",
            params![connection_id, acc.external_ref, acc.kind.as_str(), id],
        )?;
    }
    Ok(found)
}

fn is_amex_name(s: &str) -> bool {
    let l = s.to_lowercase();
    l.contains("amex") || l.contains("american express")
}

/// Open card balance (negative = owed) from `(date, amount_cents, text)` rows
/// sorted ascending: sum of everything after the latest settlement booking
/// (a positive KARTENABRECHNUNG / LASTSCHRIFT / AUSGLEICH / ZAHLUNG ERHALTEN
/// entry). Without a settlement, the sum of all bookings.
pub(crate) fn open_card_balance(rows: &[(String, i64, String)]) -> Option<i64> {
    if rows.is_empty() {
        return None;
    }
    let is_settlement = |(_, amt, text): &(String, i64, String)| {
        let t = text.to_uppercase();
        *amt > 0
            && [
                "KARTENABRECHNUNG",
                "LASTSCHRIFT",
                "AUSGLEICH",
                "ZAHLUNG ERHALTEN",
            ]
            .iter()
            .any(|k| t.contains(k))
    };
    let start = match rows.iter().rposition(is_settlement) {
        Some(i) => i + 1,
        // Billing cycles cross month boundaries and a refund is no settlement:
        // without one, everything fetched belongs to the open cycle.
        None => 0,
    };
    Some(rows[start..].iter().map(|r| r.1).sum())
}

#[cfg(test)]
mod tests {
    #[test]
    fn errors_after_submit_leave_outcome_unknown() {
        use super::{error_leaves_outcome_unknown as unknown, CoreError};
        assert!(unknown(&CoreError::Network {
            message: "x".into()
        }));
        assert!(unknown(&CoreError::Protocol {
            message: "x".into()
        }));
        assert!(!unknown(&CoreError::Authentication {
            message: "x".into()
        }));
        assert!(!unknown(&CoreError::InvalidInput {
            message: "x".into()
        }));
    }

    #[test]
    fn store_fetch_matches_depot_holdings_without_iban() {
        let p = std::env::temp_dir().join(format!("girocrab-depot-{}.db", uuid::Uuid::new_v4()));
        let v = Vault::open(p.to_string_lossy().to_string(), vec![3u8; 32]).unwrap();
        let conn = v
            .add_bank_connection(ConnectorKind::Comdirect, String::new(), "login".into())
            .unwrap();
        let mk = |r: &str| RemoteHolding {
            account_ref: r.into(),
            holding: crate::models::Holding {
                isin: "DE0001234567".into(),
                name: "X".into(),
                quantity: 2.0,
                value_cents: 100,
                ..Default::default()
            },
        };
        let fetch = |hold_ref: &str| FetchResult {
            accounts: vec![RemoteAccount {
                external_ref: "0012345678".into(),
                name: "Depot".into(),
                owner: String::new(),
                iban: String::new(),
                account_number: "0012345678".into(),
                currency: "EUR".into(),
                kind: AccountKind::Brokerage,
                balance_cents: 5000,
                balance_date: "2026-11-10".into(),
            }],
            holdings: vec![mk(hold_ref)],
            ..Default::default()
        };
        // differently formatted ref (no leading zeros) and an unknown ref (single-depot fallback)
        for r in ["12345678", "unknown"] {
            store_fetch(&v, ConnectorKind::Comdirect, conn, fetch(r)).unwrap();
            let acc = v.list_accounts().unwrap().pop().unwrap();
            assert_eq!(v.list_holdings(acc.id).unwrap().len(), 1, "ref {r}");
        }
    }

    #[test]
    fn archived_psd2_account_is_not_reported_as_revoked() {
        // #65 (archive) + #66 (revoked marking): an account the user archived
        // is skipped by store_fetch, so it must not look "missing" from a
        // complete PSD2 sync.
        let p = std::env::temp_dir().join(format!("girocrab-arch-eb-{}.db", uuid::Uuid::new_v4()));
        let v = Vault::open(p.to_string_lossy().to_string(), vec![3u8; 32]).unwrap();
        let conn = v
            .add_bank_connection(ConnectorKind::EnableBanking, String::new(), "C24".into())
            .unwrap();
        let acc = |r: &str, name: &str| RemoteAccount {
            external_ref: r.into(),
            name: name.into(),
            owner: String::new(),
            iban: String::new(),
            account_number: String::new(),
            currency: "EUR".into(),
            kind: AccountKind::Checking,
            balance_cents: 100,
            balance_date: "2026-11-11".into(),
        };
        let both = || vec![acc("G", "C24 Smartkonto"), acc("P", "C24 Pocket")];
        store_fetch(
            &v,
            ConnectorKind::EnableBanking,
            conn,
            FetchResult {
                accounts: both(),
                accounts_complete: true,
                ..Default::default()
            },
        )
        .unwrap();
        let pocket = v
            .list_accounts()
            .unwrap()
            .into_iter()
            .find(|a| a.name == "C24 Pocket")
            .unwrap();
        v.set_account_archived(pocket.id, true).unwrap();
        // the bank still delivers both
        let s = store_fetch(
            &v,
            ConnectorKind::EnableBanking,
            conn,
            FetchResult {
                accounts: both(),
                accounts_complete: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(s.revoked_accounts.is_empty(), "{:?}", s.revoked_accounts);
        // the bank no longer delivers the archived one: still no notice
        let s = store_fetch(
            &v,
            ConnectorKind::EnableBanking,
            conn,
            FetchResult {
                accounts: vec![acc("G", "C24 Smartkonto")],
                accounts_complete: true,
                revoked_refs: vec!["P".into()],
                ..Default::default()
            },
        )
        .unwrap();
        assert!(s.revoked_accounts.is_empty(), "{:?}", s.revoked_accounts);
        let st = v.connection_status(conn).unwrap();
        assert_eq!(st.revoked_account_count, 0);
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn store_fetch_skips_archived_accounts() {
        let p = std::env::temp_dir().join(format!("girocrab-arch-{}.db", uuid::Uuid::new_v4()));
        let v = Vault::open(p.to_string_lossy().to_string(), vec![3u8; 32]).unwrap();
        let conn = v
            .add_bank_connection(ConnectorKind::Comdirect, String::new(), "login".into())
            .unwrap();
        let acc = |r: &str, name: &str, balance: i64| RemoteAccount {
            external_ref: r.into(),
            name: name.into(),
            owner: String::new(),
            iban: String::new(),
            account_number: r.into(),
            currency: "EUR".into(),
            kind: AccountKind::Checking,
            balance_cents: balance,
            balance_date: "2026-11-10".into(),
        };
        let tx = |r: &str, id: &str| RemoteTransaction {
            account_ref: r.into(),
            external_id: id.into(),
            booking_date: "2026-11-09".into(),
            value_date: "2026-11-09".into(),
            amount_cents: -500,
            currency: "EUR".into(),
            counterparty: "Laden".into(),
            counterparty_iban: String::new(),
            purpose: String::new(),
            booking_text: String::new(),
            ..Default::default()
        };
        let fetch = |balance: i64, ids: &[&str]| FetchResult {
            accounts: vec![
                acc("alt", "Altes Konto", balance),
                acc("neu", "Neues Konto", balance),
            ],
            transactions: ids
                .iter()
                .flat_map(|id| [tx("alt", id), tx("neu", id)])
                .collect(),
            ..Default::default()
        };
        let s = store_fetch(&v, ConnectorKind::Comdirect, conn, fetch(1000, &["t1"])).unwrap();
        assert_eq!(
            (s.transactions_imported, s.transactions_skipped_archived),
            (2, 0)
        );
        let old = v
            .list_accounts()
            .unwrap()
            .into_iter()
            .find(|a| a.name == "Altes Konto")
            .unwrap();
        v.set_account_archived(old.id, true).unwrap();
        assert!(!v.list_bank_connections().unwrap()[0].all_accounts_archived);

        // t1 is known, t2 is new: only the active account takes it
        let s = store_fetch(
            &v,
            ConnectorKind::Comdirect,
            conn,
            fetch(2000, &["t1", "t2"]),
        )
        .unwrap();
        assert_eq!(
            (
                s.accounts_updated,
                s.transactions_imported,
                s.transactions_skipped_archived
            ),
            (1, 1, 1)
        );
        let accs = v.list_accounts().unwrap();
        assert_eq!(accs.len(), 2, "no second account for the archived one");
        let by = |n: &str| accs.iter().find(|a| a.name == n).unwrap();
        assert_eq!(
            (
                by("Altes Konto").balance_cents,
                by("Altes Konto").transaction_count
            ),
            (1000, 1)
        );
        assert_eq!(
            (
                by("Neues Konto").balance_cents,
                by("Neues Konto").transaction_count
            ),
            (2000, 2)
        );

        v.set_account_archived(by("Neues Konto").id, true).unwrap();
        assert!(v.list_bank_connections().unwrap()[0].all_accounts_archived);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn card_open_balance_since_settlement() {
        let r = |d: &str, a: i64, t: &str| (d.to_string(), a, t.to_string());
        let rows = vec![
            r("2026-09-20", -5000, "Shop"),
            r("2026-10-05", 5000, "LASTSCHRIFT Kartenabrechnung"),
            r("2026-10-10", -1200, "Cafe"),
            r("2026-11-02", -3400, "Hotel"),
        ];
        assert_eq!(open_card_balance(&rows), Some(-4600));
        let no_settle = vec![
            r("2026-10-30", -100, "x"),
            r("2026-11-01", -200, "y"),
            r("2026-11-03", -300, "z"),
        ];
        assert_eq!(open_card_balance(&no_settle), Some(-600));
        let with_refund = vec![
            r("2026-10-20", -170224, "Shops"),
            r("2026-11-02", 33853, "Refund Merchant"),
            r("2026-11-05", -1000, "Cafe"),
        ];
        assert_eq!(open_card_balance(&with_refund), Some(-137371));
        assert_eq!(open_card_balance(&[]), None);
    }

    use super::*;
    use crate::models::{BookingFilter, BookingScope};
    use crate::vault::Vault;

    fn demo_vault() -> (Arc<Vault>, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!("girocrab-sync-{}.db", uuid::Uuid::new_v4()));
        let v = Vault::open(path.to_string_lossy().to_string(), vec![3u8; 32]).unwrap();
        v.seed_demo_data().unwrap();
        (v, path)
    }

    #[test]
    fn card_holds_are_replaced_on_each_sync() {
        let (v, path) = demo_vault();
        let conn = v
            .add_bank_connection(ConnectorKind::Bunq, String::new(), "me@example.com".into())
            .unwrap();
        let acc = RemoteAccount {
            external_ref: "bunq:1".into(),
            name: "bunq".into(),
            owner: String::new(),
            iban: "NL00BUNQ0000000000".into(),
            account_number: String::new(),
            currency: "EUR".into(),
            kind: AccountKind::Checking,
            balance_cents: 0,
            balance_date: "2026-11-10".into(),
        };
        let hold = |id: &str, cents: i64| RemoteTransaction {
            account_ref: "bunq:1".into(),
            external_id: format!("pending-hold-bunq-{id}"),
            booking_date: "2026-11-09".into(),
            value_date: "2026-11-09".into(),
            amount_cents: cents,
            currency: "EUR".into(),
            counterparty: "Hotel".into(),
            counterparty_iban: String::new(),
            purpose: String::new(),
            booking_text: "Kartenzahlung (vorgemerkt)".into(),
            ..Default::default()
        };
        let fetch = |txs: Vec<RemoteTransaction>| FetchResult {
            accounts: vec![acc.clone()],
            transactions: txs,
            hold_prefix: Some("pending-hold-bunq-".into()),
            ..Default::default()
        };
        let holds = || -> Vec<String> {
            let db = v.db().unwrap();
            let mut q = db
                .prepare("SELECT external_id FROM transactions WHERE external_id LIKE 'pending-hold-%' ORDER BY 1")
                .unwrap();
            q.query_map([], |r| r.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        };
        store_fetch(
            &v,
            ConnectorKind::Bunq,
            conn,
            fetch(vec![hold("1", -50_00), hold("2", -7_10)]),
        )
        .unwrap();
        assert_eq!(holds(), vec!["pending-hold-bunq-1", "pending-hold-bunq-2"]);
        // Hold 1 cleared (booked elsewhere), hold 2 still open.
        store_fetch(&v, ConnectorKind::Bunq, conn, fetch(vec![hold("2", -7_10)])).unwrap();
        assert_eq!(holds(), vec!["pending-hold-bunq-2"]);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn revoked_accounts_are_marked_and_new_ones_named() {
        let (v, path) = demo_vault();
        let conn = v
            .add_bank_connection(ConnectorKind::EnableBanking, String::new(), "C24".into())
            .unwrap();
        let acc = |r: &str, name: &str| RemoteAccount {
            external_ref: r.into(),
            name: name.into(),
            owner: String::new(),
            iban: String::new(),
            account_number: String::new(),
            currency: "EUR".into(),
            kind: AccountKind::Checking,
            balance_cents: 100_00,
            balance_date: "2026-11-11".into(),
        };
        let revoked = || -> Vec<(String, bool)> {
            let db = v.db().unwrap();
            let mut q = db
                .prepare("SELECT external_ref, access_revoked FROM accounts WHERE connection_id = ?1 ORDER BY external_ref")
                .unwrap();
            q.query_map([conn], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? != 0)))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        };
        // first sync: nothing is "new"
        let s = store_fetch(
            &v,
            ConnectorKind::EnableBanking,
            conn,
            FetchResult {
                accounts: vec![
                    acc("A", "Girokonto"),
                    acc("B", "Pocket"),
                    acc("C", "Gemeinschaftskonto"),
                ],
                ..Default::default()
            },
        )
        .unwrap();
        assert!(s.new_accounts.is_empty() && s.revoked_accounts.is_empty());
        // reconnect with only A: B and C marked, named once, bookings kept
        let s = store_fetch(
            &v,
            ConnectorKind::EnableBanking,
            conn,
            FetchResult {
                accounts: vec![acc("A", "Girokonto")],
                revoked_refs: vec!["B".into(), "C".into()],
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(s.revoked_accounts, vec!["Pocket", "Gemeinschaftskonto"]);
        assert_eq!(
            revoked(),
            vec![("A".into(), false), ("B".into(), true), ("C".into(), true)]
        );
        let st = v.connection_status(conn).unwrap();
        assert_eq!((st.account_count, st.revoked_account_count), (3, 2));
        // repeated report: no second notice
        let s = store_fetch(
            &v,
            ConnectorKind::EnableBanking,
            conn,
            FetchResult {
                accounts: vec![acc("A", "Girokonto")],
                revoked_refs: vec!["B".into()],
                ..Default::default()
            },
        )
        .unwrap();
        assert!(s.revoked_accounts.is_empty());
        // later consent includes B again plus a new account D
        let s = store_fetch(
            &v,
            ConnectorKind::EnableBanking,
            conn,
            FetchResult {
                accounts: vec![
                    acc("A", "Girokonto"),
                    acc("B", "Pocket"),
                    acc("D", "GLS Giro"),
                ],
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(s.new_accounts, vec!["GLS Giro"]);
        assert_eq!(
            revoked(),
            vec![
                ("A".into(), false),
                ("B".into(), false),
                ("C".into(), true),
                ("D".into(), false)
            ]
        );
        let accs = v.list_accounts().unwrap();
        assert!(accs
            .iter()
            .any(|a| a.name == "Gemeinschaftskonto" && a.access_revoked));
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn automatic_names_and_bank_name_are_updated_user_names_kept() {
        let (v, path) = demo_vault();
        let conn = v
            .add_bank_connection(ConnectorKind::EnableBanking, String::new(), "C24".into())
            .unwrap();
        let acc = |r: &str, name: &str, owner: &str| RemoteAccount {
            external_ref: r.into(),
            name: name.into(),
            owner: owner.into(),
            iban: String::new(),
            account_number: String::new(),
            currency: "EUR".into(),
            kind: AccountKind::Checking,
            balance_cents: 0,
            balance_date: "2026-11-11".into(),
        };
        // as stored by the earlier build: holder as name, generic bank name
        store_fetch(
            &v,
            ConnectorKind::EnableBanking,
            conn,
            FetchResult {
                accounts: vec![acc("G", "Jay Licina", ""), acc("S", "Jay Licina", "")],
                ..Default::default()
            },
        )
        .unwrap();
        let db_rename = |r: &str, n: &str| {
            v.db()
                .unwrap()
                .execute(
                    "UPDATE accounts SET name = ?1 WHERE external_ref = ?2",
                    params![n, r],
                )
                .unwrap();
        };
        db_rename("S", "Mein Sparkonto"); // the user renamed this one
        store_fetch(
            &v,
            ConnectorKind::EnableBanking,
            conn,
            FetchResult {
                accounts: vec![
                    acc("G", "C24 Smartkonto", "Jay Licina"),
                    acc("S", "C24 PocketZINS Smart", "Jay Licina"),
                ],
                bank_name: Some("C24".into()),
                name_updates: vec![
                    ("G".into(), "Jay Licina".into(), "C24 Smartkonto".into()),
                    (
                        "S".into(),
                        "Jay Licina".into(),
                        "C24 PocketZINS Smart".into(),
                    ),
                ],
                ..Default::default()
            },
        )
        .unwrap();
        let rows: Vec<(String, String, String, String)> = {
            let db = v.db().unwrap();
            let mut q = db
                .prepare("SELECT external_ref, name, bank_name, owner FROM accounts WHERE connection_id = ?1 ORDER BY external_ref")
                .unwrap();
            q.query_map([conn], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        };
        assert_eq!(
            rows,
            vec![
                (
                    "G".into(),
                    "C24 Smartkonto".into(),
                    "C24".into(),
                    "Jay Licina".into()
                ),
                (
                    "S".into(),
                    "Mein Sparkonto".into(),
                    "C24".into(),
                    "Jay Licina".into()
                ),
            ]
        );
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn complete_account_list_marks_the_missing_ones() {
        let (v, path) = demo_vault();
        let conn = v
            .add_bank_connection(ConnectorKind::EnableBanking, String::new(), "C24".into())
            .unwrap();
        let acc = |r: &str, name: &str| RemoteAccount {
            external_ref: r.into(),
            name: name.into(),
            owner: String::new(),
            iban: String::new(),
            account_number: String::new(),
            currency: "EUR".into(),
            kind: AccountKind::Checking,
            balance_cents: 0,
            balance_date: "2026-11-11".into(),
        };
        let three = || {
            vec![
                acc("DE23", "Girokonto"),
                acc("hs", "PocketZINS"),
                acc("DE48", "Gemeinschaftskonto"),
            ]
        };
        store_fetch(
            &v,
            ConnectorKind::EnableBanking,
            conn,
            FetchResult {
                accounts: three(),
                accounts_complete: true,
                ..Default::default()
            },
        )
        .unwrap();
        // a partial sync (e.g. bank's daily limit) marks nothing
        let s = store_fetch(
            &v,
            ConnectorKind::EnableBanking,
            conn,
            FetchResult {
                accounts: vec![acc("DE23", "Girokonto")],
                accounts_complete: false,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(s.revoked_accounts.is_empty());
        // an empty "complete" list is never trusted
        let s = store_fetch(
            &v,
            ConnectorKind::EnableBanking,
            conn,
            FetchResult {
                accounts_complete: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(s.revoked_accounts.is_empty());
        // the real case: new consent with only the main account
        let s = store_fetch(
            &v,
            ConnectorKind::EnableBanking,
            conn,
            FetchResult {
                accounts: vec![acc("DE23", "Girokonto")],
                accounts_complete: true,
                ..Default::default()
            },
        )
        .unwrap();
        let mut names = s.revoked_accounts.clone();
        names.sort();
        assert_eq!(names, vec!["Gemeinschaftskonto", "PocketZINS"]);
        assert_eq!(v.connection_status(conn).unwrap().revoked_account_count, 2);
        // next sync: no repeated notice
        let s = store_fetch(
            &v,
            ConnectorKind::EnableBanking,
            conn,
            FetchResult {
                accounts: vec![acc("DE23", "Girokonto")],
                accounts_complete: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(s.revoked_accounts.is_empty());
        // all three granted again: marks cleared
        store_fetch(
            &v,
            ConnectorKind::EnableBanking,
            conn,
            FetchResult {
                accounts: three(),
                accounts_complete: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(v.connection_status(conn).unwrap().revoked_account_count, 0);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn renamed_refs_keep_the_account() {
        let (v, path) = demo_vault();
        let conn = v
            .add_bank_connection(ConnectorKind::EnableBanking, String::new(), "C24".into())
            .unwrap();
        let acc = |r: &str| RemoteAccount {
            external_ref: r.into(),
            name: "Girokonto".into(),
            owner: String::new(),
            iban: "DE00100000000000000001".into(),
            account_number: String::new(),
            currency: "EUR".into(),
            kind: AccountKind::Checking,
            balance_cents: 100_00,
            balance_date: "2026-11-10".into(),
        };
        let count = || -> i64 {
            v.db()
                .unwrap()
                .query_row(
                    "SELECT COUNT(*) FROM accounts WHERE connection_id = ?1",
                    [conn],
                    |r| r.get(0),
                )
                .unwrap()
        };
        let uid = "3fa85f64-5717-4562-b3fc-2c963f66afa6";
        let stable = "DE00100000000000000001";
        store_fetch(
            &v,
            ConnectorKind::EnableBanking,
            conn,
            FetchResult {
                accounts: vec![acc(uid)],
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(count(), 1);
        store_fetch(
            &v,
            ConnectorKind::EnableBanking,
            conn,
            FetchResult {
                accounts: vec![acc(stable)],
                renamed_refs: vec![(uid.into(), stable.into())],
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(count(), 1, "rename must not create a second account");
        // applying the same rename again is harmless
        store_fetch(
            &v,
            ConnectorKind::EnableBanking,
            conn,
            FetchResult {
                accounts: vec![acc(stable)],
                renamed_refs: vec![(uid.into(), stable.into())],
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(count(), 1);
        // a later sync recognises it as savings and learns its BBAN
        let mut savings = acc(stable);
        savings.kind = AccountKind::Savings;
        savings.account_number = "500240240000000099".into();
        store_fetch(
            &v,
            ConnectorKind::EnableBanking,
            conn,
            FetchResult {
                accounts: vec![savings],
                ..Default::default()
            },
        )
        .unwrap();
        let (kind, number): (String, String) = v
            .db()
            .unwrap()
            .query_row(
                "SELECT kind, IFNULL(account_number, '') FROM accounts WHERE connection_id = ?1",
                [conn],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (kind.as_str(), number.as_str()),
            ("savings", "500240240000000099")
        );
        assert_eq!(count(), 1);
        std::fs::remove_file(path).ok();
    }

    #[tokio::test]
    async fn demo_push_tan_flow_imports_and_categorises() {
        let (v, path) = demo_vault();
        let conn = v
            .list_bank_connections()
            .unwrap()
            .into_iter()
            .find(|c| c.connector == ConnectorKind::VolksbankKasselGoettingen)
            .unwrap();
        let before = v
            .list_accounts()
            .unwrap()
            .into_iter()
            .find(|a| a.name == "Musterbank Girokonto" && a.connection_id == Some(conn.id))
            .unwrap();
        let s = v.clone().begin_sync(conn.id, "pin".into(), true).unwrap();
        match s.start().await.unwrap() {
            SyncStep::NeedsChallenge { challenge } => {
                assert_eq!(challenge.kind, ChallengeKind::DecoupledPush)
            }
            other => panic!("expected push challenge, got {other:?}"),
        }
        assert!(matches!(
            s.poll().await.unwrap(),
            SyncStep::StillWaiting { .. }
        ));
        match s.poll().await.unwrap() {
            SyncStep::Finished { summary } => {
                assert_eq!(summary.transactions_imported, 1);
                assert_eq!(
                    summary.transactions_categorized, 1,
                    "REWE booking is categorised by a rule"
                );
            }
            other => panic!("expected finish, got {other:?}"),
        }
        let after = v
            .list_accounts()
            .unwrap()
            .into_iter()
            .find(|a| a.id == before.id)
            .unwrap();
        assert_eq!(after.balance_cents, before.balance_cents - 27_65);
        assert!(matches!(s.poll().await, Err(CoreError::SessionClosed)));
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn demo_code_and_redirect_flows() {
        let (v, path) = demo_vault();
        let conns = v.list_bank_connections().unwrap();
        let tr = conns
            .iter()
            .find(|c| c.connector == ConnectorKind::TradeRepublic)
            .unwrap();
        let s = v.clone().begin_sync(tr.id, "1234".into(), true).unwrap();
        assert!(
            matches!(s.start().await.unwrap(), SyncStep::NeedsChallenge { challenge } if challenge.kind == ChallengeKind::Code)
        );
        assert!(
            s.submit_code("12".into()).await.is_err(),
            "bad code rejected"
        );
        let amex = conns
            .iter()
            .find(|c| c.connector == ConnectorKind::AmericanExpress)
            .unwrap();
        let s = v.clone().begin_sync(amex.id, String::new(), true).unwrap();
        assert!(
            matches!(s.start().await.unwrap(), SyncStep::NeedsChallenge { challenge } if challenge.kind == ChallengeKind::Redirect)
        );
        assert!(matches!(
            s.submit_code("https://localhost/cb?code=x".into())
                .await
                .unwrap(),
            SyncStep::Finished { .. }
        ));
        let pp_id = v
            .add_bank_connection(
                ConnectorKind::PayPal,
                String::new(),
                "demo@example.org".into(),
            )
            .unwrap();
        let pp = v
            .list_bank_connections()
            .unwrap()
            .into_iter()
            .find(|c| c.id == pp_id)
            .unwrap();
        let s = v.clone().begin_sync(pp.id, String::new(), true).unwrap();
        assert!(matches!(
            s.start().await.unwrap(),
            SyncStep::Finished { .. }
        ));
        // Real (non-demo) sync refuses to start without the user's own keys.
        assert!(matches!(
            v.clone().begin_sync(pp.id, String::new(), false),
            Err(CoreError::MissingSetting { .. })
        ));
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn demo_transfer_flow_adds_pending_booking_and_suggests_recipient() {
        let (v, path) = demo_vault();
        let acc = v
            .list_accounts()
            .unwrap()
            .into_iter()
            .find(|a| a.name == "Musterbank Girokonto" && a.connection_id.is_some())
            .unwrap();
        let req = |iban: &str, amount: i64| SepaTransferRequest {
            account_id: acc.id,
            recipient_name: "Müller GmbH".into(),
            recipient_iban: iban.into(),
            recipient_bic: None,
            amount_cents: amount,
            purpose: "Rechnung 42".into(),
            instant: true,
            execution_date: None,
        };
        assert!(v
            .clone()
            .begin_transfer(req("DE89370400440532013001", 100), "pin".into(), true)
            .is_err());
        assert!(v
            .clone()
            .begin_transfer(req("DE89370400440532013000", 0), "pin".into(), true)
            .is_err());
        let s = v
            .clone()
            .begin_transfer(req("DE89 3704 0044 0532 0130 00", 1250), "pin".into(), true)
            .unwrap();
        let mut step = s.start().await.unwrap();
        let mut challenges = 0;
        while matches!(
            step,
            TransferStep::NeedsChallenge { .. } | TransferStep::StillWaiting { .. }
        ) {
            challenges += 1;
            step = s.poll().await.unwrap();
        }
        assert!(challenges >= 2, "login + transfer challenge");
        let TransferStep::Finished { outcome } = &step else {
            panic!("{step:?}")
        };
        assert!(outcome.instant);
        assert!(outcome.end_to_end_id.starts_with("GC") && outcome.end_to_end_id.len() == 32);
        let page = v
            .list_transactions(BookingFilter {
                account_ids: vec![acc.id],
                category_id: None,
                include_subcategories: false,
                uncategorized_only: false,
                search: Some("Müller GmbH".into()),
                date_from: None,
                date_to: None,
                limit: 10,
                offset: 0,
                scope: BookingScope::All,
                merchant_key: None,
            })
            .unwrap();
        assert_eq!(page.transactions.len(), 1);
        assert_eq!(page.transactions[0].amount_cents, -1250);
        assert_eq!(
            page.transactions[0].counterparty_iban,
            "DE89370400440532013000"
        );
        let sug = v.suggest_recipients("müller".into(), 5).unwrap();
        assert_eq!(sug.len(), 1);
        assert_eq!(sug[0].iban, "DE89370400440532013000");
        assert!(matches!(s.poll().await, Err(CoreError::SessionClosed)));

        // Runs a transfer to the end, approving every challenge.
        async fn run(s: &TransferSession) -> (TransferStep, Vec<String>) {
            let mut step = s.start().await.unwrap();
            let mut titles = Vec::new();
            while let TransferStep::NeedsChallenge { challenge }
            | TransferStep::StillWaiting { challenge } = &step
            {
                titles.push(challenge.title.clone());
                step = s.poll().await.unwrap();
            }
            (step, titles)
        }
        let bookings = |name: &str| {
            v.list_transactions(BookingFilter {
                account_ids: vec![acc.id],
                category_id: None,
                include_subcategories: false,
                uncategorized_only: false,
                search: Some(name.into()),
                date_from: None,
                date_to: None,
                limit: 10,
                offset: 0,
                scope: BookingScope::All,
                merchant_key: None,
            })
            .unwrap()
            .transactions
        };

        // Bank does not offer instant: declined, nothing sent, no booking,
        // and no automatic regular transfer.
        let mut slow = req("DE89370400440532013000", 990);
        slow.recipient_name = "Langsam AG".into();
        let s = v.clone().begin_transfer(slow, "pin".into(), true).unwrap();
        let (step, titles) = run(&s).await;
        assert!(
            matches!(step, TransferStep::InstantDeclined { .. }),
            "{step:?}"
        );
        assert!(
            !titles.iter().any(|t| t.contains("Überweisung")),
            "only the login, no transfer approval: {titles:?}"
        );
        assert!(bookings("Langsam AG").is_empty());
        assert!(matches!(s.poll().await, Err(CoreError::SessionClosed)));

        // Ambiguous reply after the approval: unknown, booking marked as
        // unclear, session over (nothing resent).
        let mut unclear = req("DE89370400440532013000", 770);
        unclear.recipient_name = "Unklar KG".into();
        let s = v
            .clone()
            .begin_transfer(unclear, "pin".into(), true)
            .unwrap();
        let (step, _) = run(&s).await;
        let TransferStep::OutcomeUnknown { end_to_end_id, .. } = &step else {
            panic!("{step:?}")
        };
        assert!(end_to_end_id.starts_with("GC"));
        let b = bookings("Unklar KG");
        assert_eq!(b.len(), 1);
        assert!(
            b[0].booking_text.contains("Status unklar"),
            "{}",
            b[0].booking_text
        );
        assert!(matches!(s.poll().await, Err(CoreError::SessionClosed)));

        // Connection lost after the approval: unknown as well, not an error.
        let mut lost = req("DE89370400440532013000", 660);
        lost.recipient_name = "Netzfehler GmbH".into();
        let s = v.clone().begin_transfer(lost, "pin".into(), true).unwrap();
        let (step, _) = run(&s).await;
        assert!(
            matches!(step, TransferStep::OutcomeUnknown { .. }),
            "{step:?}"
        );
        assert_eq!(bookings("Netzfehler GmbH").len(), 1);

        // Regular transfer: never a second order either.
        let mut regular = req("DE89370400440532013000", 550);
        regular.instant = false;
        regular.recipient_name = "Regulär OHG".into();
        let s = v
            .clone()
            .begin_transfer(regular, "pin".into(), true)
            .unwrap();
        let (step, _) = run(&s).await;
        let TransferStep::Finished { outcome } = &step else {
            panic!("{step:?}")
        };
        assert!(!outcome.instant);

        // Name check: close match needs confirmation, then the approval.
        let mut close = req("DE89370400440532013000", 500);
        close.recipient_name = "Fast Mustermann".into();
        let s = v.clone().begin_transfer(close, "pin".into(), true).unwrap();
        let mut step = s.start().await.unwrap();
        while matches!(
            step,
            TransferStep::NeedsChallenge { .. } | TransferStep::StillWaiting { .. }
        ) {
            step = s.poll().await.unwrap();
        }
        let TransferStep::NeedsPayeeConfirmation { check } = step else {
            panic!("{step:?}")
        };
        assert_eq!(check.result, PayeeMatch::CloseMatch);
        assert_eq!(check.bank_name.as_deref(), Some("Erika Mustermann"));
        let mut step = s.confirm_payee().await.unwrap();
        while matches!(
            step,
            TransferStep::NeedsChallenge { .. } | TransferStep::StillWaiting { .. }
        ) {
            step = s.poll().await.unwrap();
        }
        assert!(matches!(step, TransferStep::Finished { .. }));

        // Cancelling at the name check sends nothing.
        let mut wrong = req("DE89370400440532013000", 700);
        wrong.recipient_name = "Falsch GmbH".into();
        let s = v.clone().begin_transfer(wrong, "pin".into(), true).unwrap();
        let mut step = s.start().await.unwrap();
        while matches!(
            step,
            TransferStep::NeedsChallenge { .. } | TransferStep::StillWaiting { .. }
        ) {
            step = s.poll().await.unwrap();
        }
        assert!(matches!(
            step,
            TransferStep::NeedsPayeeConfirmation {
                check: PayeeCheck {
                    result: PayeeMatch::NoMatch,
                    ..
                }
            }
        ));
        s.cancel().await;
        assert!(matches!(
            s.confirm_payee().await,
            Err(CoreError::SessionClosed)
        ));
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod statements_auto_tests {
    use super::*;

    #[test]
    fn auto_statements_are_due_weekly_and_only_when_enabled() {
        let dir = std::env::temp_dir().join(format!("gc-stmt-auto-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let v = Vault::open(dir.join("v.db").to_string_lossy().into(), vec![4u8; 32]).unwrap();
        let (conn, giro, loan) = {
            let c = v.db().unwrap();
            c.execute(
                "INSERT INTO bank_connections(connector, login_name, display_name) VALUES ('fints:12345678', 'x', 'Bank')",
                [],
            )
            .unwrap();
            let conn = c.last_insert_rowid();
            c.execute("INSERT INTO account_groups(name) VALUES ('G')", [])
                .unwrap();
            c.execute(
                "INSERT INTO accounts(group_id, connection_id, external_ref, name) VALUES ((SELECT id FROM account_groups), ?1, 'giro', 'Giro')",
                [conn],
            )
            .unwrap();
            let giro = c.last_insert_rowid();
            c.execute(
                "INSERT INTO accounts(group_id, connection_id, external_ref, name, kind) VALUES ((SELECT id FROM account_groups), ?1, 'loan', 'Kredit', 'loan')",
                [conn],
            )
            .unwrap();
            (conn, giro, c.last_insert_rowid())
        };
        assert!(v.statements_due(conn).unwrap().is_empty());
        v.set_auto_statements(giro, true).unwrap();
        v.set_auto_statements(loan, true).unwrap();
        assert!(v.auto_statements_enabled(giro).unwrap());
        assert_eq!(v.statements_due(conn).unwrap(), vec!["giro".to_string()]);
        // fetched today -> not due; 8 days ago -> due again
        v.set_setting(statements_last_key(giro), today()).unwrap();
        assert!(v.statements_due(conn).unwrap().is_empty());
        let old = (chrono::Local::now().date_naive() - chrono::Duration::days(8))
            .format("%Y-%m-%d")
            .to_string();
        v.set_setting(statements_last_key(giro), old).unwrap();
        assert_eq!(v.statements_due(conn).unwrap().len(), 1);
        v.set_auto_statements(giro, false).unwrap();
        assert!(!v.auto_statements_enabled(giro).unwrap());
        assert!(v.statements_due(conn).unwrap().is_empty());
        drop(v);
        let _ = std::fs::remove_dir_all(dir);
    }
}
