//! Payment orders: single and batch transfers (HKCCS/HKCSE/HKIPZ/HKCCM), the
//! name check (Verification of Payee) and how the bank's replies decide
//! between executed, rejected and unknown.

use serde::{Deserialize, Serialize};

use super::bpd::is_segment_code;
use super::syntax::{Segment, SegmentBuilder};
use super::{BankReply, FinTsConnector, PreparedOrder, SepaAccount, Vop, sepa};
use crate::connectors::{
    BatchTransferJob, ConnectContext, PayeeCheck, PayeeMatch, Phase, TransferJob, TransferResult,
};
use crate::error::{CoreError, CoreResult};
use crate::i18n::{tr, trf};

/// What the bank allows for batch transfers (BPD HICCMS).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct BatchParams {
    /// Most payments in one order, 0 = not stated.
    pub(super) max_count: u32,
    /// The order must carry the control sum (Summenfeld).
    pub(super) sum_required: bool,
    /// The bank accepts "one booking per payment".
    pub(super) single_booking_allowed: bool,
}

/// HICCMS: header, max. orders, signatures, security class, then the
/// parameter DEG: max. payments, Summenfeld required, single booking
/// allowed, pain descriptors. Tolerates missing fields (stricter reading
/// is only possible against a real BPD).
pub(super) fn parse_batch_params(seg: &Segment) -> BatchParams {
    let flag = |s: &str| s == "J";
    BatchParams {
        max_count: seg.get(4, 0).parse().unwrap_or(0),
        sum_required: flag(seg.get(4, 1)),
        single_booking_allowed: flag(seg.get(4, 2)),
    }
}

/// Longest wait between two name-check polls, and in total.
const VOP_POLL_WAIT_MAX: std::time::Duration = std::time::Duration::from_secs(3);
const VOP_POLL_TOTAL_MAX: std::time::Duration = std::time::Duration::from_secs(25);

/// What a bank reply means for a transfer order.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum OrderReply {
    /// No errors: accepted (a challenge may follow) or executed.
    Accepted,
    /// Clearly not executed; report the bank's error.
    Rejected,
    /// Instant order clearly not executed; the user may send it as a
    /// regular transfer (new order, new approval).
    InstantDeclined(String),
    /// The order may have been executed.
    Unknown(String),
}

/// PIN/TAN/approval/SCA errors: the order was certainly not executed.
pub(super) fn is_auth_error(code: &str) -> bool {
    code.starts_with("99") || matches!(code, "9075" | "9076" | "9078")
}

/// Message-level codes that say nothing about the order itself.
fn is_generic_error(code: &str) -> bool {
    matches!(code, "9010" | "9050" | "9800")
}

/// Order-specific rejections that unambiguously mean "not executed"
/// (invalid order, limit exceeded, account does not allow it / is blocked).
fn is_clear_rejection(code: &str) -> bool {
    matches!(code, "9210" | "9380" | "9390" | "9391")
}

/// Bank errors in user-facing form ("9380 Der Betrag überschreitet …").
fn reason_text(reply: &BankReply) -> String {
    let errs = reply.errors();
    let specific: Vec<_> = errs.iter().filter(|e| !is_generic_error(&e.0)).collect();
    let shown: Vec<_> = if specific.is_empty() {
        errs.iter().collect()
    } else {
        specific
    };
    shown
        .iter()
        .map(|e| explain_transfer_code(e))
        .collect::<Vec<_>>()
        .join(" / ")
}

/// Reply to the order itself (HKCCS/HKCSE/HKIPZ + HKTAN process 4), i.e.
/// before the user approved anything. Errors here mean the bank refused the
/// order before executing it, unless the reply also confirms the order
/// segment (contradictory: unknown).
pub(super) fn classify_submit_reply(
    reply: &BankReply,
    instant: bool,
    order_seg: u32,
) -> OrderReply {
    let errs = reply.errors();
    if errs.is_empty() {
        return OrderReply::Accepted;
    }
    let order_confirmed = reply
        .codes
        .iter()
        .any(|c| c.0.starts_with('0') && c.1 == Some(order_seg));
    if order_confirmed {
        return OrderReply::Unknown(reason_text(reply));
    }
    if !instant || errs.iter().any(|e| is_auth_error(&e.0)) {
        return OrderReply::Rejected;
    }
    // Dialog aborted: without SCA an instant order may already be executed.
    if reply.has("9800") {
        return OrderReply::Unknown(reason_text(reply));
    }
    OrderReply::InstantDeclined(reason_text(reply))
}

/// Reply after the user approved the order (TAN / decoupled poll). Only
/// auth errors and clear order rejections count as "not executed"; generic
/// codes, 9800 (dialog aborted) and anything else mean the instant payment
/// may already have been executed.
pub(super) fn classify_approval_reply(reply: &BankReply, instant: bool) -> OrderReply {
    let errs = reply.errors();
    if errs.is_empty() {
        return OrderReply::Accepted;
    }
    if errs.iter().any(|e| is_auth_error(&e.0)) {
        return OrderReply::Rejected;
    }
    let specific: Vec<_> = errs.iter().filter(|e| !is_generic_error(&e.0)).collect();
    let clear = !reply.has("9800")
        && !specific.is_empty()
        && specific.iter().all(|e| is_clear_rejection(&e.0));
    match (clear, instant) {
        (true, true) => OrderReply::InstantDeclined(reason_text(reply)),
        (true, false) => OrderReply::Rejected,
        (false, _) => OrderReply::Unknown(reason_text(reply)),
    }
}

/// HIVPPS: (payment status report format, orders that need a name check).
/// Parameter group: max. entries : structured : V/S : batch : count : format : HKxxx …
pub(super) fn vop_parameters(seg: &Segment) -> (String, Vec<String>) {
    let mut format = String::new();
    let mut orders = Vec::new();
    for el in &seg.elements {
        for v in el {
            let t = v.text();
            if format.is_empty() && t.contains("pain.002") {
                format = t.to_string();
            } else if t.starts_with("HK") && is_segment_code(t) {
                // The name check covers HK orders only, never DK ones.
                orders.push(t.to_string());
            }
        }
    }
    (format, orders)
}

/// Max. length of an account holder name (SEPA) and of bank free texts.
pub(super) const VOP_NAME_MAX: usize = 70;
const VOP_TEXT_MAX: usize = 500;

/// Bank free text for display / the form: control characters dropped,
/// whitespace collapsed, cut to `max` characters.
pub(super) fn clean_bank_text(s: &str, max: usize) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max)
        .collect()
}

/// The worst result among all codes found: a match must never mask a
/// mismatch. Worst first: RVNM, RVMC, RVNA, PDNG, RCVC.
pub(super) fn worst_vop_code(candidates: &[&str]) -> String {
    ["RVNM", "RVMC", "RVNA", "PDNG", "RCVC"]
        .iter()
        .find(|c| candidates.iter().any(|t| t.contains(*c)))
        .map(|c| c.to_string())
        .unwrap_or_default()
}

/// VOP result code from HIVPP: the single-transaction result (EVPE) and the
/// codes in the payment status report; the worst one wins.
pub(super) fn vop_result_code(h: &Segment) -> String {
    let evpe = h.get(6, 4).trim().to_string();
    let report = h
        .binary(5)
        .map(crate::text::decode_bank_bytes)
        .unwrap_or_default();
    worst_vop_code(&[&evpe, &report])
}

impl BankReply {
    /// Like `check`, but with German explanations for typical HKCCS rejections.
    pub(super) fn check_transfer(&self) -> CoreResult<()> {
        let errs = self.errors();
        let Some(first) = errs.first() else {
            return Ok(());
        };
        // Name the rejected segment ("[HKCCS #4]"): it tells a refused order
        // from a refused name-check approval.
        let msg = errs
            .iter()
            .map(|e| {
                let text = explain_transfer_code(e);
                match e.1.and_then(|n| self.sent.get(&n).map(|seg| (n, seg))) {
                    Some((n, seg)) => format!("{text} [{seg} #{n}]"),
                    None => text,
                }
            })
            .collect::<Vec<_>>()
            .join(" / ");
        Err(CoreError::Bank {
            code: first.0.clone(),
            message: msg,
        })
    }
}

/// Localised text for a bank return code (HIRMG/HIRMS) during a transfer.
fn explain_transfer_code(c: &(String, Option<u32>, String, Vec<String>)) -> String {
    let hint = match c.0.as_str() {
        "9050" => tr("Die Nachricht enthält Fehler."),
        "9010" => tr("Die Bank hat die Nachricht nicht verarbeitet."),
        "9120" | "9130" | "9140" | "9160" => {
            tr("Ein Pflichtfeld der Überweisung fehlt oder ist ungültig.")
        }
        "9210" => tr("Der Auftrag wurde abgelehnt (Daten ungültig oder nicht erlaubt)."),
        "9340" => tr("Der Auftrag ist ungültig; bitte IBAN, Betrag und Name prüfen."),
        "9380" => tr("Der Betrag überschreitet das Limit des Kontos."),
        "9390" => tr("Das Konto lässt diese Überweisung nicht zu."),
        "9391" => tr("Das Konto ist für Überweisungen gesperrt."),
        "9931" | "9942" => tr("PIN oder Zugangsdaten wurden abgelehnt."),
        "9941" => tr("Die TAN ist ungültig oder abgelaufen."),
        "9955" | "9956" => tr("Die Freigabe wurde abgelehnt oder ist abgelaufen."),
        "9075" => {
            tr("Starke Kundenauthentifizierung ist erforderlich, wurde aber nicht abgeschlossen.")
        }
        _ => "",
    };
    let params = if c.3.is_empty() {
        String::new()
    } else {
        format!(" ({})", c.3.join(", "))
    };
    if hint.is_empty() {
        trf(
            "{0} Die Bank hat die Überweisung abgelehnt: {1}{2}",
            &[&c.0, &c.2, &params],
        )
    } else {
        trf("{0} {1} Bank: {2}{3}", &[&c.0, &hint, &c.2, &params])
    }
}

/// The single transfer (HKCCS, HKCSE or HKIPZ) as it is sent: the pain.001 from
/// the job and the source account. Pure, so `invariant_tests` can follow the
/// values from the job to the segment bytes.
pub(super) fn transfer_order(
    job: &TransferJob,
    acc: &SepaAccount,
    seg_id: &'static str,
    version: sepa::PainVersion,
) -> CoreResult<PreparedOrder> {
    let header = sepa::PainHeader::new(&job.source_owner);
    let exec = job
        .execution_date
        .as_deref()
        .unwrap_or(sepa::IMMEDIATE_EXECUTION_DATE);
    let xml = sepa::build_pain001(&sepa::Pain001 {
        message_id: &header.message_id,
        created: &header.created,
        execution_date: exec,
        debtor_name: header.owner,
        debtor_iban: &acc.iban,
        debtor_bic: &acc.bic,
        creditor_name: &job.recipient_name,
        creditor_iban: &job.recipient_iban,
        creditor_bic: job.recipient_bic.as_deref(),
        amount_cents: job.amount_cents,
        purpose: &job.purpose,
        version,
        end_to_end_id: &job.end_to_end_id,
        instant: job.instant,
    })?;
    Ok(PreparedOrder {
        seg_id,
        kti: FinTsConnector::kti(acc),
        descriptor: version.descriptor(),
        xml,
        instant: job.instant,
        recipient_name: job.recipient_name.clone(),
        recipient_iban: job.recipient_iban.clone(),
        batch: None,
        debit_layout: None,
        standing: None,
    })
}

/// The batch transfer (HKCCM) as it is sent, see [`transfer_order`].
pub(super) fn batch_order(
    job: &BatchTransferJob,
    acc: &SepaAccount,
    version: sepa::PainVersion,
) -> CoreResult<PreparedOrder> {
    let header = sepa::PainHeader::new(&job.source_owner);
    let exec = job
        .execution_date
        .as_deref()
        .unwrap_or(sepa::IMMEDIATE_EXECUTION_DATE);
    let xml = sepa::build_pain001_batch(&sepa::Pain001Batch {
        message_id: &header.message_id,
        created: &header.created,
        execution_date: exec,
        debtor_name: header.owner,
        debtor_iban: &acc.iban,
        debtor_bic: &acc.bic,
        payments: &job.payments,
        version,
        batch_booking: job.batch_booking,
    })?;
    let total = sepa::batch_total_cents(&job.payments).ok_or_else(|| CoreError::InvalidInput {
        message: tr("Die Summe der Sammelüberweisung ist zu groß.").into(),
    })?;
    Ok(PreparedOrder {
        seg_id: "HKCCM",
        kti: FinTsConnector::kti(acc),
        descriptor: version.descriptor(),
        xml,
        instant: false,
        recipient_name: String::new(),
        recipient_iban: String::new(),
        batch: Some((total, !job.batch_booking)),
        debit_layout: None,
        standing: None,
    })
}

impl FinTsConnector {
    /// Sends HKCCS (regular), HKCSE (scheduled) or HKIPZ (instant) + HKTAN
    /// process 4. An instant order the bank does not offer ends as
    /// `TransferResult::InstantDeclined` without sending anything. There is
    /// never an automatic second order.
    pub(super) async fn start_transfer(
        &mut self,
        ctx: &ConnectContext,
        job: &TransferJob,
    ) -> CoreResult<Phase> {
        let instant = job.instant;
        let seg_id: &'static str = if instant {
            "HKIPZ"
        } else if job.execution_date.is_some() {
            "HKCSE"
        } else {
            "HKCCS"
        };
        let not_allowed = || {
            CoreError::Bank {
            code: seg_id.into(),
            message: match seg_id {
                "HKIPZ" => tr("Dieses Konto oder diese Bank erlaubt keine Echtzeitüberweisungen per FinTS (HKIPZ). Bitte in der Banking-App überweisen."),
                "HKCSE" => tr("Dieses Konto oder diese Bank erlaubt keine Terminüberweisungen per FinTS (HKCSE). Bitte in der Banking-App überweisen."),
                _ => tr("Dieses Konto oder diese Bank erlaubt keine SEPA-Überweisungen per FinTS (HKCCS). Bitte in der Banking-App überweisen."),
            }
            .into(),
        }
        };
        if !self.bank_supports(seg_id) {
            if instant {
                return Ok(self.decline_instant(tr(
                    "Die Bank bietet per FinTS keine Echtzeitüberweisung (HKIPZ) an.",
                )));
            }
            return Err(not_allowed());
        }
        self.require_tan_method(tr("Für Überweisungen ist ein TAN-Verfahren nötig; es wurde keines gefunden. Bitte zuerst eine Synchronisation mit TAN-Verfahren durchführen."))?;
        let Some(acc) = self.source_account(ctx, &job.source_iban, seg_id).await? else {
            if instant {
                return Ok(self.decline_instant(tr(
                    "Für dieses Konto ist die Echtzeitüberweisung per FinTS nicht freigeschaltet.",
                )));
            }
            return Err(not_allowed());
        };
        let version = match self.pain_version(seg_id) {
            Some(v) => v,
            None => {
                return Ok(self.decline_instant(tr(
                    "Die Bank erwartet für Echtzeitüberweisungen ein Datenformat, das GiroCrab nicht unterstützt.",
                )))
            }
        };
        self.order = Some(transfer_order(job, &acc, seg_id, version)?);
        let needs_vop =
            !self.st.vop_format.is_empty() && self.st.vop_orders.iter().any(|o| o == seg_id);
        if !needs_vop {
            let reply = self.send_order(ctx, Vop::None).await?;
            return self.finish_submit(&reply);
        }
        let (reply, polled) = self.check_payee(ctx).await?;
        if let Some(early) = self.reject_or_stop(&reply)? {
            return Ok(early);
        }
        let Some(check) = self.evaluate_payee(&reply) else {
            // Match (or no result reported): on to the approval.
            if polled && self.vop_id.is_some() {
                // The last reply answered a bare HKVPP poll and carries no
                // challenge: authorise with HKVPA + order + HKTAN now.
                self.vop_with_tan = false;
                let reply = self.send_order(ctx, Vop::Execute).await?;
                return self.finish_submit(&reply);
            }
            return self.finish_submit(&reply);
        };
        self.keep_challenge_on_3091(&reply, polled)?;
        // Anything but a clean match: nothing more is sent until the user
        // confirms (`confirm_payee`); cancelling ends the dialog instead.
        self.vop_pending = Some(check.result);
        Ok(Phase::ConfirmPayee(check))
    }

    /// Sends HKCCM + HKTAN process 4. No name check for batches: the bank
    /// answers with what it requires.
    pub(super) async fn start_batch(
        &mut self,
        ctx: &ConnectContext,
        job: &BatchTransferJob,
    ) -> CoreResult<Phase> {
        let seg_id: &'static str = "HKCCM";
        let not_allowed = || {
            CoreError::Bank {
            code: seg_id.into(),
            message: tr("Dieses Konto oder diese Bank erlaubt keine Sammelüberweisungen per FinTS (HKCCM). Bitte in der Banking-App überweisen.").into(),
        }
        };
        let Some(params) = self
            .st
            .batch_params
            .clone()
            .filter(|_| self.bank_supports(seg_id))
        else {
            return Err(not_allowed());
        };
        if params.max_count > 0 && job.payments.len() > params.max_count as usize {
            return Err(CoreError::InvalidInput {
                message: trf(
                    "Die Bank erlaubt höchstens {0} Zahlungen pro Sammelüberweisung.",
                    &[&params.max_count],
                ),
            });
        }
        if !job.batch_booking && !params.single_booking_allowed {
            return Err(CoreError::InvalidInput {
                message: tr("Die Bank bucht Sammelüberweisungen nur als Sammelbuchung.").into(),
            });
        }
        self.require_tan_method(tr("Für Überweisungen ist ein TAN-Verfahren nötig; es wurde keines gefunden. Bitte zuerst eine Synchronisation mit TAN-Verfahren durchführen."))?;
        let Some(acc) = self.source_account(ctx, &job.source_iban, seg_id).await? else {
            return Err(not_allowed());
        };
        let version = self.pain_version(seg_id).unwrap_or(sepa::PainVersion::V03);
        self.order = Some(batch_order(job, &acc, version)?);
        let reply = self.send_order(ctx, Vop::None).await?;
        self.finish_submit(&reply)
    }

    /// Starts a new order: nothing of the previous one, its name check included, carries over.
    pub(super) fn begin_order(&mut self) {
        self.transferring = true;
        self.result = TransferResult::default();
        self.order_submitted = false;
        self.order = None;
        self.vop_id = None;
        self.vop_with_tan = false;
        self.vop_pending = None;
        self.vop_resume = None;
    }

    /// Orders need a two-step TAN method; `sec()` is 999 (one-step) while none is known.
    pub(super) fn require_tan_method(&self, refusal: &'static str) -> CoreResult<()> {
        if self.sec() == "999" {
            return Err(CoreError::Bank {
                code: "HKTAN".into(),
                message: refusal.into(),
            });
        }
        Ok(())
    }

    /// Instant not possible before anything was sent: nothing to approve.
    fn decline_instant(&mut self, reason: &str) -> Phase {
        self.result = TransferResult::InstantDeclined {
            reason: reason.to_string(),
        };
        Phase::Ready
    }

    /// Segments of one order message: [HKVPP|HKVPA] + order + HKTAN
    /// process 4, or HKVPP alone for a polling request.
    pub(super) fn order_payload(&mut self, vop: Vop) -> CoreResult<Vec<Vec<u8>>> {
        let order = self.order.clone().ok_or(CoreError::SessionClosed)?;
        let mut n = 3;
        let mut payload = Vec::new();
        match vop {
            Vop::None => {}
            Vop::Check => {
                payload.push(
                    SegmentBuilder::new("HKVPP", n, self.version("HKVPP"))
                        .de(&[&self.st.vop_format])
                        .build(),
                );
                n += 1;
            }
            Vop::Poll {
                polling_id,
                touchdown,
            } => {
                // Polling: HKVPP only (format, polling ID, max. entries,
                // continuation point), no order and no HKTAN.
                let mut b = SegmentBuilder::new("HKVPP", n, self.version("HKVPP"))
                    .de(&[&self.st.vop_format])
                    .binary(&polling_id);
                if let Some(t) = touchdown {
                    b = b.raw("").de(&[&t]);
                }
                return Ok(vec![b.build()]);
            }
            Vop::Execute => {
                payload.push(self.hkvpa(n)?);
                n += 1;
            }
        }
        self.order_seg_no = n;
        let mut seg =
            SegmentBuilder::new(order.seg_id, n, self.version(order.seg_id)).raw(&order.kti);
        if let Some((total, single)) = order.batch {
            // HKCCM: Summenfeld, Einzelbuchung gewünscht (only what the BPD asks for).
            let (sum_required, single_allowed) = order.debit_layout.unwrap_or_else(|| {
                let p = self.st.batch_params.clone().unwrap_or_default();
                (p.sum_required, p.single_booking_allowed)
            });
            seg = if sum_required {
                seg.de(&[&sepa::format_amount(total).replace('.', ","), "EUR"])
            } else {
                seg.raw("")
            };
            seg = if single_allowed {
                seg.de(&[if single { "J" } else { "N" }])
            } else {
                seg.raw("")
            };
        }
        seg = seg.de(&[order.descriptor]).binary(order.xml.as_bytes());
        if let Some(tail) = &order.standing {
            seg = tail.append(seg);
        }
        payload.push(seg.build());
        payload.push(self.hktan_process4(n + 1, order.seg_id));
        Ok(payload)
    }

    /// Sends one order message. Marks the order as submitted first: once the
    /// bytes may have left, the result can be "unknown".
    pub(super) async fn send_order(
        &mut self,
        ctx: &ConnectContext,
        vop: Vop,
    ) -> CoreResult<BankReply> {
        let payload = self.order_payload(vop)?;
        let sec = self.sec();
        self.order_submitted = true;
        self.send(ctx, payload, &sec, None).await
    }

    pub(super) fn hkvpa(&self, n: u32) -> CoreResult<Vec<u8>> {
        // No VOP-ID: nothing can be approved (and nothing was sent).
        let id = self
            .vop_id
            .as_deref()
            .ok_or_else(|| CoreError::InvalidInput {
                message: "name check: no VOP-ID from the bank".into(),
            })?;
        Ok(SegmentBuilder::new("HKVPA", n, 1).binary(id).build())
    }

    /// HKVPP + order; asks again (HKVPP only, with the polling ID) while the
    /// result is pending. Waits at most `VOP_POLL_WAIT_MAX` per round and
    /// `VOP_POLL_TOTAL_MAX` overall, so the session lock is never held long.
    /// Returns the last reply and whether polling happened.
    async fn check_payee(&mut self, ctx: &ConnectContext) -> CoreResult<(BankReply, bool)> {
        let mut reply = self.send_order(ctx, Vop::Check).await?;
        let mut polled = false;
        let mut waited = std::time::Duration::ZERO;
        for _ in 0..10 {
            let Some(h) = reply.first("HIVPP") else { break };
            let code = vop_result_code(h);
            let polling = h.binary(3).map(|b| b.to_vec());
            if !reply.errors().is_empty()
                || !(code.is_empty() || code == "PDNG")
                || polling.is_none()
            {
                break;
            }
            let wait = std::time::Duration::from_secs(
                h.get(8, 0)
                    .parse::<u64>()
                    .unwrap_or(2)
                    .clamp(1, VOP_POLL_WAIT_MAX.as_secs()),
            );
            if waited + wait > VOP_POLL_TOTAL_MAX {
                break;
            }
            waited += wait;
            tokio::time::sleep(wait).await;
            let touchdown = reply.code("3040").and_then(|c| c.3.first().cloned());
            polled = true;
            reply = self
                .send_order(
                    ctx,
                    Vop::Poll {
                        polling_id: polling.unwrap_or_default(),
                        touchdown,
                    },
                )
                .await?;
        }
        Ok((reply, polled))
    }

    /// Reads HIVPP. `None` = go on to the approval (match, or the bank did
    /// not report a result); `Some` = the user has to confirm.
    pub(super) fn evaluate_payee(&mut self, reply: &BankReply) -> Option<PayeeCheck> {
        let h = reply.first("HIVPP")?;
        self.vop_id = h.binary(1).map(|b| b.to_vec()).filter(|b| !b.is_empty());
        let code = vop_result_code(h);
        let order = self.order.as_ref()?;
        let opt = |s: &str, max: usize| Some(clean_bank_text(s, max)).filter(|s| !s.is_empty());
        let result = match code.as_str() {
            "RCVC" => PayeeMatch::Match,
            "RVMC" => PayeeMatch::CloseMatch,
            "RVNM" => PayeeMatch::NoMatch,
            _ => PayeeMatch::NotPossible,
        };
        if result == PayeeMatch::Match && !reply.has("3945") {
            // 3091: the bank needs no HKVPA; the challenge is simply answered.
            self.vop_with_tan = self.vop_id.is_some() && !reply.has("3091");
            return None;
        }
        Some(PayeeCheck {
            result,
            entered_name: order.recipient_name.clone(),
            iban: order.recipient_iban.clone(),
            bank_name: opt(h.get(6, 2), VOP_NAME_MAX),
            reason: opt(h.get(6, 5), VOP_TEXT_MAX).or_else(|| {
                (code.is_empty() || code == "PDNG")
                    .then(|| tr("Die Empfängerbank hat kein Ergebnis geliefert.").to_string())
            }),
            notice: opt(h.get(7, 0), VOP_TEXT_MAX),
        })
    }

    /// 3091 ("VOP-Ausführungsauftrag nicht benötigt"): the bank needs no HKVPA
    /// and has accepted the order already; its challenge (e.g. the app
    /// approval, 3955) is in this reply. Keeps it for the user's confirmation:
    /// HKVPA + the order sent again would be a second order in the same dialog,
    /// which banks reject (9010 on the order). Not after polling, where the
    /// last reply answers a bare HKVPP and carries no challenge.
    pub(super) fn keep_challenge_on_3091(
        &mut self,
        reply: &BankReply,
        polled: bool,
    ) -> CoreResult<()> {
        if polled || !reply.has("3091") || reply.has("3945") {
            return Ok(());
        }
        self.vop_with_tan = false;
        self.vop_resume = match self.challenge_from(reply)? {
            Some(Phase::Challenge(c)) => Some((c, false)),
            Some(Phase::Pending(c)) => Some((c, true)),
            _ => None,
        };
        Ok(())
    }

    /// A clear rejection: the bank did not execute the order, so the session
    /// reports the bank's error instead of "status unclear".
    fn rejected(&mut self, reply: &BankReply) -> CoreResult<()> {
        self.order_submitted = false;
        reply.check_transfer()
    }

    /// Classifies the reply to the order message. `Some(phase)` ends the
    /// transfer (declined / unknown); `None` = accepted, go on. Rejections
    /// are returned as errors.
    pub(super) fn reject_or_stop(&mut self, reply: &BankReply) -> CoreResult<Option<Phase>> {
        let instant = self.order.as_ref().is_some_and(|o| o.instant);
        match classify_submit_reply(reply, instant, self.order_seg_no) {
            OrderReply::Accepted => Ok(None),
            OrderReply::Rejected => {
                self.rejected(reply)?;
                unreachable!("Rejected implies errors")
            }
            OrderReply::InstantDeclined(reason) => {
                self.result = TransferResult::InstantDeclined { reason };
                Ok(Some(Phase::Ready))
            }
            OrderReply::Unknown(reason) => {
                self.result = TransferResult::Unknown { reason };
                Ok(Some(Phase::Ready))
            }
        }
    }

    /// Reply to an order message that needs no further name check: classify
    /// it, then wait for the approval or finish.
    pub(super) fn finish_submit(&mut self, reply: &BankReply) -> CoreResult<Phase> {
        if let Some(early) = self.reject_or_stop(reply)? {
            return Ok(early);
        }
        if let Some(p) = self.challenge_from(reply)? {
            return Ok(p);
        }
        let instant = self.order.as_ref().is_some_and(|o| o.instant);
        self.result = TransferResult::Executed { instant };
        Ok(Phase::Ready)
    }

    /// The user confirmed despite the name-check result: HKVPA + order + HKTAN.
    /// Only ever reached from the user's explicit confirmation.
    pub(super) async fn confirm_payee(&mut self, ctx: &ConnectContext) -> CoreResult<Phase> {
        // A stray/duplicate confirmation must not look like an unknown outcome.
        if self.vop_pending.take().is_none() {
            return Err(CoreError::InvalidInput {
                message: "no name check awaiting confirmation".into(),
            });
        }
        // The bank asked for no HKVPA (3091) and already issued the challenge
        // for the order it holds: carry on with that, send nothing.
        if let Some((challenge, pending)) = self.vop_resume.take() {
            return Ok(if pending {
                Phase::Pending(challenge)
            } else {
                Phase::Challenge(challenge)
            });
        }
        let reply = self.send_order(ctx, Vop::Execute).await?;
        self.finish_submit(&reply)
    }

    /// Reply to an approved TAN / decoupled poll. For a login this checks
    /// the reply; for a transfer it decides executed / rejected / unknown.
    pub(super) async fn after_approval(
        &mut self,
        _ctx: &ConnectContext,
        reply: BankReply,
    ) -> CoreResult<Phase> {
        if !self.order_submitted {
            self.check_reply(&reply)?;
            return Ok(Phase::Ready);
        }
        let instant = self.order.as_ref().is_some_and(|o| o.instant);
        match classify_approval_reply(&reply, instant) {
            OrderReply::Accepted => {
                self.result = TransferResult::Executed { instant };
            }
            OrderReply::Rejected => {
                self.rejected(&reply)?;
                unreachable!("Rejected implies errors")
            }
            OrderReply::InstantDeclined(reason) => {
                self.result = TransferResult::InstantDeclined { reason };
            }
            OrderReply::Unknown(reason) => {
                self.result = TransferResult::Unknown { reason };
            }
        }
        Ok(Phase::Ready)
    }
}
