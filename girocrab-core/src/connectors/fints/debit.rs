//! Direct debit submission (HKDME/HKDSE and the COR1/B2B variants) and the
//! bank's lead-time limits for it.

use serde::{Deserialize, Serialize};

use super::sepa::PainHeader;
use super::syntax::Segment;
use super::transfer::is_auth_error;
use super::{FinTsConnector, PreparedOrder, Vop, pain008};
use crate::connectors::{ConnectContext, DebitJob, Phase};
use crate::error::{CoreError, CoreResult};
use crate::i18n::{tr, trf};

/// What the bank allows for batch direct debits (BPD HIDMES/HIDMCS/HIBMES).
/// Lead times in calendar days before the due date.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct DebitParams {
    pub(super) min_days_recurring: u32,
    pub(super) max_days_recurring: u32,
    pub(super) min_days_first: u32,
    pub(super) max_days_first: u32,
    pub(super) max_count: u32,
    /// The order must carry the control sum.
    pub(super) sum_required: bool,
    /// The order states "one booking per collection".
    pub(super) single_booking_allowed: bool,
}

/// HIDMES-like segment: header, max. orders, signatures, security class, then the
/// parameter DEG: lead times (FNAL/RCUR min/max, FRST/OOFF min/max), max.
/// collections, Summenfeld required, single booking allowed. Untested
/// against a real BPD: callers must treat implausible values as "unknown".
pub(super) fn parse_debit_params(seg: &Segment) -> DebitParams {
    let num = |i: usize| seg.get(4, i).parse().unwrap_or(0);
    let flag = |i: usize| seg.get(4, i) == "J";
    DebitParams {
        min_days_recurring: num(0),
        max_days_recurring: num(1),
        min_days_first: num(2),
        max_days_first: num(3),
        max_count: num(4),
        sum_required: flag(5),
        single_booking_allowed: flag(6),
    }
}

/// Checks the due date against the bank's lead times. `Ok` when the
/// parameters are unusable (nothing is invented), otherwise the error names the
/// earliest/latest date. `today` and `due` are `YYYY-MM-DD`.
pub(super) fn check_debit_lead_time(
    p: &DebitParams,
    sequence: pain008::SequenceType,
    today: jiff::civil::Date,
    due: jiff::civil::Date,
) -> CoreResult<()> {
    use pain008::SequenceType::*;
    let (min, max) = match sequence {
        First | OneOff => (p.min_days_first, p.max_days_first),
        Recurring | Final => (p.min_days_recurring, p.max_days_recurring),
    };
    if max == 0 || min > max || max > 400 {
        return Ok(());
    }
    // The unit (calendar vs. TARGET days) is not certain: the minimum is only
    // enforced where it is too early in either unit; the maximum gets the
    // longer reading (TARGET days are at most 7/5 calendar days) plus a margin.
    // The bank has the last word.
    let earliest = today + crate::dates::days(i64::from(min));
    let latest = today + crate::dates::days(i64::from(max) * 7 / 5 + 3);
    if due < earliest {
        return Err(CoreError::InvalidInput {
            message: trf(
                "Die Bank braucht für diese Lastschrift mindestens {0} Tage Vorlauf; frühestens möglich: {1}.",
                &[&min, &earliest.strftime("%d.%m.%Y").to_string()],
            ),
        });
    }
    if due > latest {
        return Err(CoreError::InvalidInput {
            message: trf(
                "Die Bank nimmt diese Lastschrift höchstens {0} Tage im Voraus an; spätestens möglich: {1}.",
                &[
                    &max,
                    &today
                        .checked_add(crate::dates::days(i64::from(max)))
                        .unwrap_or(latest)
                        .strftime("%d.%m.%Y")
                        .to_string(),
                ],
            ),
        });
    }
    Ok(())
}

impl FinTsConnector {
    /// Sends a direct debit collection (HKDME/HKDSE and the COR1/B2B
    /// variants) + HKTAN process 4. The bank checks mandate and
    /// creditor ID itself; here only the due date and size limits are checked.
    pub(super) async fn start_direct_debit(
        &mut self,
        ctx: &ConnectContext,
        job: &DebitJob,
    ) -> CoreResult<Phase> {
        use pain008::DebitScheme::*;
        let (batch_id, single_id): (&'static str, &'static str) = match job.scheme {
            Core => ("HKDME", "HKDSE"),
            Cor1 => ("HKDMC", "HKDSC"),
            B2b => ("HKBME", "HKBSE"),
        };
        let scheme_name = job.scheme.code();
        let not_allowed = || CoreError::Bank {
            code: batch_id.into(),
            message: trf(
                "Dieses Konto oder diese Bank erlaubt keine Lastschrift-Einreichung ({0}) per FinTS. Bitte die Datei im Online-Banking hochladen.",
                &[&scheme_name],
            ),
        };
        let seg_id = if self.bank_supports(batch_id) {
            batch_id
        } else if job.collections.len() == 1 && self.bank_supports(single_id) {
            single_id
        } else {
            return Err(not_allowed());
        };
        let params = self.st.debit_params.get(batch_id).cloned();
        if seg_id == batch_id
            && let Some(p) = &params
        {
            if p.max_count > 0 && job.collections.len() > p.max_count as usize {
                return Err(CoreError::InvalidInput {
                    message: trf(
                        "Die Bank erlaubt höchstens {0} Lastschriften pro Einreichung.",
                        &[&p.max_count],
                    ),
                });
            }
            if !job.batch_booking && !p.single_booking_allowed {
                return Err(CoreError::InvalidInput {
                    message: tr("Die Bank bucht Lastschriften nur als Sammelbuchung.").into(),
                });
            }
        }
        if let (Some(p), Ok(due)) = (
            &params,
            jiff::civil::Date::strptime("%Y-%m-%d", &job.collection_date),
        ) {
            check_debit_lead_time(p, job.sequence, crate::dates::today(), due)?;
        }
        self.require_tan_method(tr("Für Lastschriften ist ein TAN-Verfahren nötig; es wurde keines gefunden. Bitte zuerst eine Synchronisation mit TAN-Verfahren durchführen."))?;
        let Some(acc) = self.source_account(ctx, &job.source_iban, seg_id).await? else {
            return Err(not_allowed());
        };
        let version = pain008::DebitVersion::pick(
            self.st
                .pain_formats
                .get(seg_id)
                .map(Vec::as_slice)
                .unwrap_or(&[]),
        )
        .ok_or_else(|| CoreError::Bank {
            code: seg_id.into(),
            message: tr("Die Bank akzeptiert keines der unterstützten pain.008-Formate.").into(),
        })?;
        let header = PainHeader::new(&job.creditor_name);
        let xml = pain008::build_pain008(&pain008::Pain008 {
            message_id: &header.message_id,
            created: &header.created,
            collection_date: &job.collection_date,
            creditor_name: header.owner,
            creditor_iban: &acc.iban,
            creditor_bic: &acc.bic,
            creditor_id: &job.creditor_id,
            scheme: job.scheme,
            sequence: job.sequence,
            collections: &job.collections,
            version,
            batch_booking: job.batch_booking,
        })?;
        let total: i64 = job.collections.iter().map(|c| c.amount_cents).sum();
        let batch = seg_id == batch_id;
        let p = params.unwrap_or_default();
        self.order = Some(PreparedOrder {
            seg_id,
            kti: Self::kti(&acc),
            descriptor: version.descriptor(),
            xml,
            instant: false,
            recipient_name: String::new(),
            recipient_iban: String::new(),
            batch: batch.then_some((total, !job.batch_booking)),
            debit_layout: Some((p.sum_required, p.single_booking_allowed)),
            standing: None,
        });
        let reply = self.send_order(ctx, Vop::None).await?;
        // The bank lists HKCCM among the orders that need a name check (VoP): batches are
        // sent without one, so a refusal most likely means exactly that. Say so instead
        // of showing a bare bank code.
        let needs_vop =
            !self.st.vop_format.is_empty() && self.st.vop_orders.iter().any(|o| o == seg_id);
        match self.finish_submit(&reply) {
            Err(CoreError::Bank { code, message }) if needs_vop && !is_auth_error(&code) => {
                Err(CoreError::Bank {
                    code,
                    message: format!(
                        "{message} {}",
                        tr(
                            "Die Bank verlangt für diesen Auftrag eine Empfängerprüfung (Verification of Payee), die GiroCrab für Sammelüberweisungen noch nicht unterstützt. Bitte in der Banking-App überweisen."
                        )
                    ),
                })
            }
            other => other,
        }
    }
}
