//! FinTS 3.0 PIN/TAN connector (HBCI+), clean-room implementation of the
//! public DK specification ("FinTS 3.0 Formals", "Sicherheitsverfahren
//! PIN/TAN", "Messages – Multibankfähige Geschäftsvorfälle").
//!
//! Flow:
//!   1. Synchronisation dialog (first run only): HKIDN/HKVVB/HKSYN with
//!      security function 999 -> customer system ID, bank parameters (BPD) and
//!      the TAN methods allowed for this user (code 3920).
//!   2. Authenticated dialog: HKIDN/HKVVB + HKTAN process 4 (PSD2 SCA).
//!      The bank answers with
//!        * 3955 -> decoupled: approve in the app; poll with HKTAN process S
//!        * 0030 -> enter TAN; send HKTAN process 2 + PIN:TAN in HNSHA
//!        * 3076 / success -> no SCA needed.
//!   3. Accounts from the UPD (HIUPD, incl. the business transactions each
//!      account allows) merged with HKSPA; per account only what the bank
//!      allows: HKSAL (balance) and statements via HKCAZ (camt.052, preferred)
//!      or HKKAZ (MT940, fallback). Loan accounts get statements only if their HIUPD allows it.
//!      Depots (Kontoart 30-39/60-69) get their value from HKWPD (MT535 holdings).
//!      Segment versions are negotiated from the BPD ("HIxxxS" parameter
//!      segments): always the highest version both sides support.
//!   4. Any 9xxx return code is reported with the code and the offending
//!      segment ("9050 [HKKAZ #3] Auftragsart wird nicht unterstützt").
//!   5. Transfers: HKCCS (regular SEPA) or HKIPZ (SEPA Instant), each with
//!      HKTAN process 4. An instant transfer the bank does not offer or rejects
//!      (on submission or after the approval) is reported as declined; nothing
//!      is resent automatically. The app may offer a regular transfer, which
//!      is a new order with its own approval. Ambiguous replies and lost
//!      connections after submission mean "outcome unknown".
//!   6. Name check (Verification of Payee), where the BPD (HIVPPS) requires it
//!      for the order: HKVPP goes in front of the order; the bank answers with
//!      HIVPP (VOP-ID + result, possibly after polling). A match continues to
//!      the approval (HKVPA is sent with the TAN). Anything else is shown to
//!      the user; on confirmation, HKVPA + order + HKTAN is sent again. Cancel
//!      ends the dialog without HKVPA, so the order is never approved.
//!
//! The registration-bound product ID is never hardcoded; it comes from
//! Settings (`fints.product_id`).
//!
//! This file holds the connector state, the types the submodules share and the
//! `BankConnector` entry points. The other `impl FinTsConnector` blocks are split
//! by topic: `dialog`, `bpd`, `accounts`, `statements`, `standing`, `transfer`,
//! `debit` and `standing_order`.

mod accounts;
pub mod bank_keys;
mod bpd;
pub mod camt;
mod debit;
mod dialog;
pub mod keyfile;
pub mod kku;
pub mod mt535;
pub mod mt940;
pub mod pain008;
pub mod product_id;
pub(crate) mod rah10;
pub mod sepa;
mod standing;
mod standing_order;
pub(crate) use standing_order::limits_from_state as standing_limits_from_state;
mod statements;
pub mod syntax;
mod transfer;

use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use base64::Engine;
use serde::{Deserialize, Serialize};

use self::accounts::{account_label, kind_from_code};
use self::dialog::{approval_pending, has_so_prefix, is_comdirect, tan_demanded};
use self::syntax::{Segment, SegmentBuilder};
use super::{
    BankConnector, BatchTransferJob, Challenge, ChallengeResponse, ConnectContext, DebitJob,
    FetchResult, PayeeMatch, Phase, RemoteAccount, StandingOrderJob, TransferJob, TransferResult,
};
use crate::error::{CoreError, CoreResult};
use crate::i18n::{tr, trf};
use crate::models::AccountKind;
use crate::secret::SecretJson;

const COUNTRY: &str = "280";

/// Persisted between syncs (in bank_connections.state_json).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct FinTsState {
    system_id: String,
    /// Chosen two-step security function (e.g. "946"), "999" = one-step.
    sec_func: String,
    tan_method_name: String,
    tan_medium: String,
    /// Supported versions per business transaction (from BPD "...S" segments).
    versions: HashMap<String, Vec<u32>>,
    /// HKxxx -> TAN required (HIPINS).
    tan_required: HashMap<String, bool>,
    /// camt descriptors from HICAZS, best first.
    #[serde(default)]
    camt_formats: Vec<String>,
    /// Receipt codes (hex) of statements stored with this state; acknowledged
    /// with HKQTG at the next statements fetch, so the bank stops re-sending
    /// them. Saved in the same transaction as the PDFs (`store_fetch`).
    #[serde(default)]
    pending_receipts: Vec<String>,
    /// The bank accepted the TAN medium only as `SO:<name>` (BestSign security
    /// object); the user enters the plain device name.
    #[serde(default)]
    tan_medium_so: bool,
    /// HIKKUS (card transactions): the bank takes no date range / no account.
    #[serde(default)]
    kku_no_date_range: bool,
    #[serde(default)]
    kku_no_account: bool,
    /// HIKKUS: days of card history the bank keeps (0 = unknown).
    #[serde(default)]
    kku_storage_days: i64,
    /// pain.001 descriptors per transfer transaction (HKCCS/HKIPZ/HKCSE) from the BPD
    /// parameter segments HICCSS / HIIPZS / HICSES.
    #[serde(default)]
    pain_formats: HashMap<String, Vec<String>>,
    /// Batch transfer parameters from HICCMS; `None` = not offered.
    #[serde(default)]
    batch_params: Option<transfer::BatchParams>,
    /// Limits for creating and changing standing orders (HICDES / HICDNS).
    #[serde(default)]
    standing_params: standing_order::StandingParams,
    /// Collection parameters per HKDME/HKDMC/HKBME from the BPD (HIDMES, …).
    #[serde(default)]
    debit_params: HashMap<String, debit::DebitParams>,
    /// Payment status report format for the name check (HIVPPS), "" = none.
    #[serde(default)]
    vop_format: String,
    /// Orders that need a name check first (HIVPPS), e.g. HKCCS, HKIPZ.
    #[serde(default)]
    vop_orders: Vec<String>,
    /// The bank rejected the two-step dialog init (900 + HKTAN) with 9010; from then on
    /// the dialog is opened with the one-step function 999 and without HKTAN.
    #[serde(default)]
    single_step: bool,
    /// comdirect: the bank code this login is known under when it differs from
    /// the connector's (taken from an IBAN). Found by [`FinTsConnector::login`]
    /// after a 9210 "Bankleitzahl passt nicht zum Benutzer"; "" = the connector's.
    #[serde(default)]
    login_blz: String,
}

/// Account from the user parameter data (HIUPD).
#[derive(Debug, Clone, Default)]
struct UpdAccount {
    iban: String,
    account_number: String,
    sub_account: String,
    blz: String,
    kind_code: u32,
    currency: String,
    owner: String,
    product: String,
    /// Business transactions allowed for this account (empty = unknown, all).
    allowed: HashSet<String>,
}

impl UpdAccount {
    fn allows(&self, tx: &str) -> bool {
        self.allowed.is_empty() || self.allowed.contains(tx)
    }
}

/// Consorsbank (BNP Paribas): see `docs/consorsbank-connector.md`.
const CONSORSBANK_BLZ: &str = "76030080";

/// FinTS date `YYYYMMDD` → `YYYY-MM-DD`; `None` unless exactly 8 digits naming a real day.
fn fints_date(d: &str) -> Option<String> {
    if d.len() != 8 {
        return None;
    }
    crate::dates::parse_yyyymmdd(d).map(crate::dates::iso)
}

#[derive(Debug, Clone)]
struct SepaAccount {
    iban: String,
    bic: String,
    account_number: String,
    sub_account: String,
    blz: String,
}

#[derive(Debug, Clone)]
struct BankReply {
    segments: Vec<Segment>,
    /// (code, reference segment number, text, params)
    codes: Vec<(String, Option<u32>, String, Vec<String>)>,
    /// Segment number -> segment id of the request (for error messages).
    sent: HashMap<u32, String>,
}

impl BankReply {
    fn has(&self, code: &str) -> bool {
        self.codes.iter().any(|c| c.0 == code)
    }
    fn code(&self, code: &str) -> Option<&(String, Option<u32>, String, Vec<String>)> {
        self.codes.iter().find(|c| c.0 == code)
    }
    fn errors(&self) -> Vec<&(String, Option<u32>, String, Vec<String>)> {
        self.codes.iter().filter(|c| c.0.starts_with('9')).collect()
    }
    fn first(&self, id: &str) -> Option<&Segment> {
        self.segments.iter().find(|s| s.id() == id)
    }
    fn all<'a>(&'a self, id: &'a str) -> impl Iterator<Item = &'a Segment> + 'a {
        self.segments.iter().filter(move |s| s.id() == id)
    }
    /// "9050 [HKKAZ #3] Auftragsart wird nicht unterstützt"
    fn describe(&self, code: &(String, Option<u32>, String, Vec<String>)) -> String {
        let seg = match code.1 {
            Some(n) => format!(
                "[{} #{n}]",
                self.sent
                    .get(&n)
                    .cloned()
                    .unwrap_or_else(|| "Segment".into())
            ),
            None => "[Nachricht]".into(),
        };
        let params = if code.3.is_empty() {
            String::new()
        } else {
            format!(" ({})", code.3.join(", "))
        };
        format!("{} {} {}{}", code.0, seg, code.2, params)
    }

    fn check(&self) -> CoreResult<()> {
        let errs = self.errors();
        if let Some(e) = errs.first() {
            return Err(CoreError::Bank {
                code: e.0.clone(),
                message: errs
                    .iter()
                    .map(|e| self.describe(e))
                    .collect::<Vec<_>>()
                    .join(" / "),
            });
        }
        Ok(())
    }
}

pub struct FinTsConnector {
    blz: String,
    url: String,
    client: Option<reqwest::Client>,
    rah: Option<rah10::Rah10>,
    requires_rah10: bool,
    rah_stream: Option<tokio::net::TcpStream>,
    dialog_id: String,
    msg_no: u32,
    st: FinTsState,
    task_reference: Option<String>,
    challenge: Option<Challenge>,
    tan_for_next: Option<String>,
    product_id: String,
    product_version: String,
    /// Accounts from the user parameter data (HIUPD).
    upd: Vec<UpdAccount>,
    /// A SEPA transfer (HKCCS/HKIPZ) is awaiting its TAN.
    transferring: bool,
    /// TAN medium name entered by the user (e.g. Postbank BestSign `SO:Gerät`);
    /// wins over the HKTAB lookup.
    tan_medium_override: String,
    /// The bank answered a statement request with 3997 ("nur teilweise
    /// ausgeführt"): it delivered less than the requested period.
    partial_statements: bool,
    /// The order was handed to the bank: from here on, an unclear reply or a
    /// lost connection means "result unknown", never "not executed".
    order_submitted: bool,
    result: TransferResult,
    /// The order being sent.
    order: Option<PreparedOrder>,
    /// Segment number of the order in the last order message (the name check
    /// segment shifts it), to read the bank's per-segment confirmation.
    order_seg_no: u32,
    /// VOP-ID from HIVPP, sent back with HKVPA.
    vop_id: Option<Vec<u8>>,
    /// Name matched: send HKVPA along with the TAN.
    vop_with_tan: bool,
    /// Name-check result shown to the user, awaiting confirmation.
    vop_pending: Option<PayeeMatch>,
    /// The bank answered the order with 3091 ("VOP-Ausführungsauftrag nicht
    /// benötigt"): it has accepted the order already and issued its challenge
    /// (the flag says whether that approval is a pending decoupled one). After
    /// the user's confirmation this challenge continues; HKVPA and the order
    /// are not sent again, a second order in the same dialog is rejected.
    vop_resume: Option<(Challenge, bool)>,
    /// The last dialog init was rejected because the bank code does not
    /// belong to this login (9210, see [`blz_mismatch`]).
    blz_rejected: bool,
}

/// A transfer order ready to send (segment number set at send time).
#[derive(Clone)]
struct PreparedOrder {
    seg_id: &'static str,
    kti: String,
    descriptor: &'static str,
    xml: String,
    instant: bool,
    recipient_name: String,
    recipient_iban: String,
    /// HKCCM only: control sum in cents and "one booking per payment".
    batch: Option<(i64, bool)>,
    /// Direct debit batches: (send Summenfeld, send single-booking flag),
    /// from the BPD; `None` = read the transfer `batch_params`.
    debit_layout: Option<(bool, bool)>,
    /// Standing orders (HKCDE/HKCDN/HKCDL): order id and schedule after the pain.001.
    standing: Option<standing_order::StandingTail>,
}

/// Name-check part of an order message.
enum Vop {
    None,
    /// HKVPP + order + HKTAN: the first name check request.
    Check,
    /// HKVPP only, with the polling ID (and the continuation point from
    /// 3040): asks again while the result is pending. Never carries the
    /// order or HKTAN, so polling cannot create a second order.
    Poll {
        polling_id: Vec<u8>,
        touchdown: Option<String>,
    },
    /// HKVPA with the VOP-ID.
    Execute,
}

impl FinTsConnector {
    pub fn new(blz: &str) -> Self {
        FinTsConnector {
            blz: blz.to_string(),
            url: String::new(),
            client: None,
            rah: None,
            requires_rah10: false,
            rah_stream: None,
            dialog_id: "0".into(),
            msg_no: 1,
            st: FinTsState::default(),
            task_reference: None,
            challenge: None,
            tan_for_next: None,
            product_id: String::new(),
            product_version: String::new(),
            upd: Vec::new(),
            transferring: false,
            tan_medium_override: String::new(),
            partial_statements: false,
            order_submitted: false,
            result: TransferResult::default(),
            order: None,
            order_seg_no: 3,
            vop_id: None,
            vop_with_tan: false,
            vop_pending: None,
            vop_resume: None,
            blz_rejected: false,
        }
    }

    pub fn for_key(key: &str) -> Self {
        let mut connector = Self::new(crate::models::ConnectorKind::fints_blz(key).unwrap_or(""));
        connector.requires_rah10 = key.ends_with(":rah10");
        connector
    }
}

#[async_trait]
impl BankConnector for FinTsConnector {
    async fn begin(&mut self, ctx: &ConnectContext) -> CoreResult<Phase> {
        let keyfile_setting = format!("fints.rah10.private.{}", ctx.connection_id);
        let keyfile = ctx.setting_or(&keyfile_setting, "");
        if self.requires_rah10 && keyfile.is_empty() {
            return Err(CoreError::MissingSetting {
                key: "RAH-10-Schlüsseldatei fehlt auf diesem Gerät; Zugang hier neu importieren."
                    .into(),
            });
        }
        if self.requires_rah10 {
            let bytes = zeroize::Zeroizing::new(
                base64::engine::general_purpose::STANDARD
                    .decode(keyfile)
                    .map_err(|_| {
                        CoreError::invalid(tr("Ungültige gespeicherte Schlüsseldatei."))
                    })?,
            );
            let mut file = keyfile::KeyFile::open(&bytes, ctx.secret.expose().as_bytes())?;
            let saved = ctx.setting_or(&format!("fints.rah10.bank_keys.{}", ctx.connection_id), "");
            if !saved.is_empty() {
                let keys: bank_keys::SavedBankKeys = serde_json::from_str(saved)?;
                let verified =
                    bank_keys::confirmed_keys(&self.blz, &keys.candidates, &keys.expected)?;
                file.bank_keys.retain(|key| key.blz != self.blz);
                file.bank_keys.extend(verified);
            }
            let first: u64 = ctx
                .setting("fints.rah10.counter_start")?
                .parse()
                .map_err(|_| CoreError::SessionClosed)?;
            self.rah = Some(rah10::Rah10::new(
                file,
                &ctx.login_name,
                first,
                first + 10_000,
            )?);
            if self.rah.as_ref().is_none_or(|r| r.identity.blz != self.blz) {
                return Err(CoreError::SessionClosed);
            }
        } else {
            self.url = ctx.setting(&format!("fints.url.{}", self.blz))?.to_string();
        }
        self.product_id = ctx.setting("fints.product_id")?.to_string();
        self.product_version = ctx.setting_or("fints.product_version", "0.1").to_string();
        if ctx.secret.is_empty() {
            return Err(CoreError::Authentication {
                message: tr("PIN ist leer").into(),
            });
        }
        if let Ok(st) = ctx.state.decode::<FinTsState>() {
            self.st = st;
        }
        // comdirect: log in under the bank code found last time (the address
        // above is the same for all of comdirect's codes)
        if self.rah.is_none() && is_comdirect(&self.blz) && is_comdirect(&self.st.login_blz) {
            self.blz = self.st.login_blz.clone();
        }
        if let Some(rah) = &self.rah {
            if self.st.system_id.is_empty() {
                self.st.system_id = rah.identity.system_id.clone();
            }
            self.st.sec_func = "999".into();
        }
        self.tan_medium_override = ctx
            .setting_or(&format!("fints.tan_medium.{}", ctx.connection_id), "")
            .trim()
            .to_string();
        if !self.tan_medium_override.is_empty() {
            self.st.tan_medium =
                if self.st.tan_medium_so && !has_so_prefix(&self.tan_medium_override) {
                    format!("SO:{}", self.tan_medium_override)
                } else {
                    self.tan_medium_override.clone()
                };
        }
        self.login(ctx).await
    }

    async fn respond(
        &mut self,
        ctx: &ConnectContext,
        response: ChallengeResponse,
    ) -> CoreResult<Phase> {
        if let ChallengeResponse::ConfirmPayee = response {
            return self.confirm_payee(ctx).await;
        }
        let task = self
            .task_reference
            .clone()
            .ok_or_else(|| CoreError::protocol("no pending TAN task"))?;
        let sec = self.sec();
        // Name matched: HKVPA (VOP-ID) goes along with the approval.
        let mut payload = Vec::new();
        // Only with the first step after the name check, not with the
        // following decoupled status polls.
        if std::mem::take(&mut self.vop_with_tan) {
            payload.push(self.hkvpa(3)?);
        }
        let n = 3 + payload.len() as u32;
        match response {
            ChallengeResponse::ConfirmPayee => unreachable!(),
            ChallengeResponse::Poll => {
                let seg = SegmentBuilder::new("HKTAN", n, self.version("HKTAN"))
                    .raw("S")
                    .raw("")
                    .raw("")
                    .raw("")
                    .de(&[&task])
                    .raw("N")
                    .build();
                payload.push(seg);
                let reply = self.send(ctx, payload, &sec, None).await?;
                if approval_pending(&reply) {
                    let c = self.challenge.clone().ok_or(CoreError::SessionClosed)?;
                    return Ok(Phase::Pending(c));
                }
                self.after_approval(ctx, reply).await
            }
            ChallengeResponse::Code(tan) => {
                let seg = SegmentBuilder::new("HKTAN", n, self.version("HKTAN"))
                    .raw("2")
                    .raw("")
                    .raw("")
                    .raw("")
                    .de(&[&task])
                    .raw("N")
                    .build();
                self.tan_for_next = None;
                payload.push(seg);
                let reply = self.send(ctx, payload, &sec, Some(tan.expose())).await?;
                self.after_approval(ctx, reply).await
            }
        }
    }

    async fn fetch(&mut self, ctx: &ConnectContext) -> CoreResult<FetchResult> {
        let mut errors: Vec<String> = Vec::new();
        let accounts = self.account_list(ctx, &mut errors).await;
        let mut result = FetchResult::default();
        let mut ok_any = false;
        // not yet booked entries ("vorgemerkt"), see `camt::HOLD_PREFIX`
        let (mut holds_listed, mut holds_complete) = (false, true);
        // Explicit statements fetch (Vault::fetch_account_statements): HKEKA of one account only.
        let auto_statements = ctx.documents.due_statements();
        if let super::Documents::StatementsOnly(wanted) = &ctx.documents {
            let Some((acc, upd)) = accounts.iter().find(|(a, _)| account_label(a) == *wanted)
            else {
                return Err(CoreError::NotFound {
                    what: trf("Konto {0}", &[wanted]),
                });
            };
            // HKEKA, or HKEKP when the bank offers only that (DKB, #449)
            let tx = self.statement_tx();
            let upd_allows = upd.as_ref().map(|u| u.allows(tx.unwrap_or("HKEKA")));
            self.log_statement_support(upd_allows);
            // Not an error (#449): the app reports "no new statements" with a hint.
            let Some(tx) = tx.filter(|_| upd_allows.unwrap_or(true)) else {
                crate::depot_debug::log("statements: no PDF statements offered for this account");
                return Ok(result);
            };
            result.documents = match self.statements_pdf(ctx, acc, wanted).await {
                Ok(d) => d,
                Err(CoreError::Bank { code, .. }) if matches!(code.as_str(), "9130" | "9140") => {
                    crate::depot_debug::log(&format!("statements: {tx} unsupported ({code})"));
                    return Ok(result);
                }
                Err(e) => return Err(e),
            };
            result.state = Some(SecretJson::encode(&self.st)?);
            return Ok(result);
        }
        // One account (#1124): ask the bank only for it.
        let labels: Vec<String> = accounts.iter().map(|(a, _)| account_label(a)).collect();
        let only = ctx.only_among(&labels);
        for ((acc, upd), label) in accounts.iter().zip(&labels) {
            if only.is_some_and(|o| o != label) {
                continue;
            }
            let allowed = |tx: &str| upd.as_ref().is_none_or(|u| u.allows(tx));
            let mut kind = upd
                .as_ref()
                .map(|u| kind_from_code(u.kind_code))
                .unwrap_or(AccountKind::Checking);
            // Some banks report depots with a generic Kontoart; trust the product name.
            if kind == AccountKind::Checking
                && (upd
                    .as_ref()
                    .is_some_and(|u| u.product.to_lowercase().contains("depot"))
                    || label.to_lowercase().contains("depot"))
            {
                kind = AccountKind::Brokerage;
            }
            crate::depot_debug::log(&format!(
                "fints account: kind={kind:?} kind_code={:?} allowed=[{}] is_depot={} upd_found={} hkwpd_allowed={} bank_supports_hkwpd={}",
                upd.as_ref().map(|u| u.kind_code),
                upd.as_ref()
                    .map(|u| {
                        let mut a: Vec<&str> = u.allowed.iter().map(String::as_str).collect();
                        a.sort_unstable();
                        a.join(",")
                    })
                    .unwrap_or_default(),
                kind == AccountKind::Brokerage,
                upd.is_some(),
                allowed("HKWPD"),
                self.bank_supports("HKWPD")
            ));
            let mut currency = upd
                .as_ref()
                .map(|u| u.currency.clone())
                .filter(|c| !c.is_empty())
                .unwrap_or_else(|| "EUR".into());
            let mut balance: Option<i64> = None;
            let mut balance_date = super::today();

            // --- Balance (HKSAL), only if the account allows it
            if self.bank_supports("HKSAL") && allowed("HKSAL") {
                match self.balance(ctx, acc).await {
                    Ok(Some((b, cur, date))) => {
                        balance = Some(b);
                        currency = cur;
                        balance_date = date;
                        ok_any = true;
                    }
                    Ok(None) => {}
                    Err(e) => errors.push(trf("{0}: Saldo – {1}", &[&label, &e])),
                }
            }

            // --- Depot value (HKWPD / MT535), only for depots that allow it
            // Some banks omit HKWPD from HIUPD although HIWPDS is in the BPD: try it
            // anyway for depots; a 9xxx rejection just lands in the error list.
            if kind == AccountKind::Brokerage && self.bank_supports("HKWPD") {
                match self.holdings(ctx, acc).await {
                    Ok(st) => {
                        crate::depot_debug::log(&format!(
                            "fints holdings parsed: {} positions, value_derivable={}",
                            st.holdings.len(),
                            st.value_cents().is_some()
                        ));
                        // Positions are always kept, even when the total can't be derived.
                        if let Some(c) = st
                            .holdings
                            .iter()
                            .map(|h| h.currency.clone())
                            .find(|c| !c.is_empty())
                        {
                            currency = c;
                        }
                        for h in &st.holdings {
                            result.holdings.push(super::RemoteHolding {
                                classification: Default::default(),
                                direct_crypto: false,

                                account_ref: label.clone(),
                                holding: h.to_model(),
                            });
                        }
                        match st.value_cents() {
                            Some(v) => {
                                balance = Some(v);
                                if let Some(d) = st.latest_date() {
                                    balance_date = d;
                                }
                                ok_any = true;
                            }
                            None => {
                                if !st.holdings.is_empty() {
                                    ok_any = true;
                                }
                                errors.push(trf(
                                    "{0}: Depotbestand (HKWPD) – Wert einzelner Positionen fehlt",
                                    &[&label],
                                ))
                            }
                        }
                    }
                    Err(e) => errors.push(trf("{0}: Depotbestand (HKWPD) – {1}", &[&label, &e])),
                }
            }

            // --- Statements: camt (HKCAZ) first, MT940 (HKKAZ) as fallback. Loans are
            // included only when HIUPD permits it (or lists nothing); every request is
            // its own FinTS message, so a 9050 on one loan is just collected in `errors`.
            let mut txs = Vec::new();
            // Credit cards the bank serves through HKKKU or DKKKU (explicitly allowed
            // in HIUPD). comdirect reports its cards with Kontoart 1 (#807).
            if let Some(card_tx) = self.card_tx(upd.as_ref()) {
                if kind == AccountKind::Checking {
                    kind = AccountKind::CreditCard;
                }
                match self
                    .statements_card(ctx, card_tx, acc, label, &currency)
                    .await
                {
                    Ok((t, bal)) => {
                        txs = t;
                        if let Some(b) = bal {
                            balance = Some(b.cents);
                            if !b.currency.is_empty() {
                                currency = b.currency;
                            }
                            if !b.date.is_empty() {
                                balance_date = b.date;
                            }
                        }
                        ok_any = true;
                    }
                    Err(e) => errors.push(trf(
                        "{0}: Kartenumsätze ({1}) – {2}",
                        &[&label, &card_tx, &e],
                    )),
                }
            } else {
                let can_camt = self.bank_supports("HKCAZ")
                    && allowed("HKCAZ")
                    && !self.st.camt_formats.is_empty();
                let can_mt940 = self.bank_supports("HKKAZ") && allowed("HKKAZ");
                let mut done = false;
                if can_camt {
                    match self.statements_camt(ctx, acc, label).await {
                        Ok((t, closing)) => {
                            holds_listed = true;
                            // Loan sub-accounts ("Darlehen") often answer camt with an empty
                            // list; then MT940 is tried before giving up.
                            let empty_loan = kind == AccountKind::Loan && t.is_empty() && can_mt940;
                            txs = t;
                            if balance.is_none() {
                                balance = closing;
                            }
                            done = !empty_loan;
                            ok_any = true;
                        }
                        Err(e) => {
                            // the stored holds of this login stay as they are
                            holds_complete = false;
                            // MT940 would only send another TAN to the app (#1083)
                            done = tan_demanded(&e);
                            errors.push(trf("{0}: Umsätze (HKCAZ/camt.052) – {1}", &[&label, &e]))
                        }
                    }
                }
                if !done && can_mt940 {
                    match self.statements_mt940(ctx, acc, label, &currency).await {
                        Ok((t, closing)) => {
                            if !t.is_empty() || txs.is_empty() {
                                txs = t;
                            }
                            if balance.is_none() {
                                balance = closing;
                            }
                            ok_any = true;
                            if std::mem::take(&mut self.partial_statements) {
                                errors.push(trf(
                                    "{0}: Die Bank hat nur einen Teil des angeforderten Zeitraums geliefert (Rückmeldung 3997).",
                                    &[&label],
                                ));
                            }
                        }
                        Err(e) => {
                            errors.push(trf("{0}: Umsätze (HKKAZ/MT940) – {1}", &[&label, &e]))
                        }
                    }
                }
            }
            // comdirect names no counterparty for Visa debit card payments (#1135);
            // the ids are set by now, so content ids stay as stored
            if is_comdirect(&self.blz) {
                txs.iter_mut()
                    .for_each(super::comdirect::fill_card_merchant);
            }
            result.transactions.extend(txs);

            // --- Standing orders (HKCDB), read-only. Sent right after the statements,
            // in the same dialog: a bank that wants SCA for it usually accepts the
            // approval just given. A decoupled (push) approval is handled by `request`.
            if kind == AccountKind::Checking && self.bank_supports("HKCDB") && allowed("HKCDB") {
                match self.standing_orders(ctx, acc, label).await {
                    Ok(orders) => {
                        result.standing_orders.extend(orders);
                        result.standing_orders_fetched.push(label.clone());
                    }
                    Err(e) => errors.push(trf("{0}: Daueraufträge – {1}", &[&label, &e])),
                }
            }

            // Opt-in (GitHub #57): PDF statements, at most weekly per account. A failure
            // never fails the sync; the explicit fetch reports errors instead.
            if auto_statements.contains(label) {
                let tx = self.statement_tx();
                let upd_ok = tx.is_some_and(allowed);
                self.log_statement_support(Some(upd_ok));
                if upd_ok {
                    match self.statements_pdf(ctx, acc, label).await {
                        Ok(docs) => {
                            crate::depot_debug::log(&format!(
                                "statements: auto fetch ok, {} PDF(s)",
                                docs.len()
                            ));
                            result.documents.extend(docs);
                            result.statements_fetched.push(label.clone());
                        }
                        Err(e) => crate::depot_debug::log(&format!(
                            "statements: auto fetch failed: {}",
                            e.to_string().chars().take(160).collect::<String>()
                        )),
                    }
                }
            }
            // Not read this time (#1083): the stored balance stays instead of
            // becoming 0. Cards without one get theirs derived when storing.
            if balance.is_none() && kind != AccountKind::CreditCard {
                result.balances_unknown.push(label.clone());
            }
            let name = upd
                .as_ref()
                .map(|u| u.product.clone())
                .filter(|p| !p.is_empty())
                .unwrap_or_else(|| match kind {
                    AccountKind::Loan => "Darlehen".into(),
                    AccountKind::Savings => "Sparkonto".into(),
                    AccountKind::CreditCard => "Kreditkarte".into(),
                    AccountKind::Brokerage => "Depot".into(),
                    _ => "Girokonto".into(),
                });
            result.accounts.push(RemoteAccount {
                external_ref: label.clone(),
                name,
                owner: upd.as_ref().map(|u| u.owner.clone()).unwrap_or_default(),
                iban: acc.iban.clone(),
                account_number: acc.account_number.clone(),
                currency,
                kind,
                balance_cents: balance.unwrap_or(0),
                balance_date,
            });
        }
        if !ok_any && !errors.is_empty() {
            return Err(CoreError::Bank {
                code: "FinTS".into(),
                message: errors.join(" / "),
            });
        }
        // Something worked: the sync is done, and says what did not (#1083).
        result.warnings = errors;
        // Claim the hold list as complete only when every camt request worked.
        if holds_listed && holds_complete {
            result.hold_prefix = Some(camt::HOLD_PREFIX.into());
        }
        result.state = Some(SecretJson::encode(&self.st)?);
        Ok(result)
    }

    async fn finish(&mut self, ctx: &ConnectContext) {
        let sec = self.sec();
        self.end_dialog(ctx, &sec).await;
    }

    async fn send_transfer(
        &mut self,
        ctx: &ConnectContext,
        job: &TransferJob,
    ) -> CoreResult<Phase> {
        if self.rah.is_some() {
            return Err(CoreError::invalid(tr(
                "RAH-10 unterstützt zunächst nur Kontoabrufe.",
            )));
        }
        self.begin_order();
        let r = self.start_transfer(ctx, job).await;
        if r.is_err() {
            self.transferring = false;
        }
        r
    }

    async fn send_batch_transfer(
        &mut self,
        ctx: &ConnectContext,
        job: &BatchTransferJob,
    ) -> CoreResult<Phase> {
        self.begin_order();
        let r = self.start_batch(ctx, job).await;
        if r.is_err() {
            self.transferring = false;
        }
        r
    }

    async fn send_standing_order(
        &mut self,
        ctx: &ConnectContext,
        job: &StandingOrderJob,
    ) -> CoreResult<Phase> {
        self.begin_order();
        let r = self.start_standing_order(ctx, job).await;
        if r.is_err() {
            self.transferring = false;
        }
        r
    }

    async fn send_direct_debit(
        &mut self,
        ctx: &ConnectContext,
        job: &DebitJob,
    ) -> CoreResult<Phase> {
        self.begin_order();
        let r = self.start_direct_debit(ctx, job).await;
        if r.is_err() {
            self.transferring = false;
        }
        r
    }

    fn transfer_result(&self) -> TransferResult {
        self.result.clone()
    }

    fn order_submitted(&self) -> bool {
        self.order_submitted
    }
}

#[cfg(test)]
mod invariant_tests;
#[cfg(test)]
mod tests;
