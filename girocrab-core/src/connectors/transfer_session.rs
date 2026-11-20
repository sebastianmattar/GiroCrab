//! `TransferSession`: the two-phase SEPA transfer state machine exposed via FFI.

use std::sync::Arc;

use tokio::sync::Mutex as AsyncMutex;

use crate::error::{CoreError, CoreResult};
use crate::i18n::tr;
use crate::secret::SecretString;
use crate::vault::Vault;

use super::*;

/// A direct debit collection handed to the bank by this session.
pub(crate) struct DebitSubmission {
    pub job: DebitJob,
    pub mandate_ids: Vec<i64>,
}

/// What the session sends to the bank.
pub(crate) enum OrderKind {
    Single(TransferJob),
    /// `summary` is the whole batch as one transfer (sum, shared date and
    /// reference), recorded when the bank books it as one line.
    Batch {
        batch: BatchTransferJob,
        summary: TransferJob,
    },
    Debit(DebitSubmission),
    Standing(StandingOrderJob),
}

impl OrderKind {
    /// The EndToEndId the outcome reports. Collections and standing orders
    /// have no single one for the whole order, so they get a fresh id that is
    /// never sent to the bank.
    fn end_to_end_id(&self) -> String {
        match self {
            OrderKind::Single(job) | OrderKind::Batch { summary: job, .. } => {
                job.end_to_end_id.clone()
            }
            OrderKind::Debit(_) | OrderKind::Standing(_) => new_end_to_end_id(),
        }
    }
}

/// Where the session stands. Every stage after `Submitted` closes the dialog
/// for further steps; that says nothing about whether the bank still holds an
/// open order (see `cancel` and `BankConnector::has_open_order`).
#[derive(PartialEq, Eq)]
enum Stage {
    LoggingIn,
    /// The order was sent; waiting for its TAN.
    Submitted,
    /// The bank executed the order: it can no longer be cancelled.
    Executed,
    /// The order reached the bank, but whether it was executed is unclear.
    OutcomeUnknown,
    /// The bank confirmed the order is not payable any more.
    Cancelled,
    /// Failed, declined or cancellation not (yet) confirmed.
    Closed,
}

struct TransferState {
    connector: Box<dyn BankConnector>,
    ctx: ConnectContext,
    order: OrderKind,
    account_id: i64,
    stage: Stage,
}

/// After the order was handed to the bank, only errors that prove it was
/// refused (wrong credentials, invalid input) mean "not executed". Anything
/// else (network, garbled reply, ...) leaves the result unknown.
pub(super) fn error_leaves_outcome_unknown(e: &CoreError) -> bool {
    !matches!(
        e,
        CoreError::Authentication { .. } | CoreError::InvalidInput { .. }
    )
}

/// One SEPA transfer. Created by `Vault::begin_transfer`; same two-phase
/// challenge handling as `SyncSession` (login SCA, then transfer SCA).
#[derive(uniffi::Object)]
pub struct TransferSession {
    vault: Arc<Vault>,
    state: AsyncMutex<TransferState>,
}

impl TransferSession {
    pub(crate) fn new(
        vault: Arc<Vault>,
        connector: Box<dyn BankConnector>,
        ctx: ConnectContext,
        order: OrderKind,
        account_id: i64,
    ) -> Self {
        Self {
            vault,
            state: AsyncMutex::new(TransferState {
                connector,
                ctx,
                order,
                account_id,
                stage: Stage::LoggingIn,
            }),
        }
    }

    /// Runs one dialog step; `SessionClosed` once the dialog is over.
    async fn step(
        &self,
        call: impl AsyncFnOnce(&mut TransferState) -> CoreResult<Phase>,
    ) -> CoreResult<TransferStep> {
        let mut st = self.state.lock().await;
        if !matches!(st.stage, Stage::LoggingIn | Stage::Submitted) {
            return Err(CoreError::SessionClosed);
        }
        let phase = call(&mut st).await;
        self.advance(phase, &mut st).await
    }

    async fn advance(
        &self,
        mut phase: CoreResult<Phase>,
        st: &mut TransferState,
    ) -> CoreResult<TransferStep> {
        loop {
            match phase {
                Err(e) => {
                    // A dropped connection after the order reached the bank:
                    // it may have been executed, so never report "failed".
                    let unknown = st.stage == Stage::Submitted
                        && st.connector.order_submitted()
                        && error_leaves_outcome_unknown(&e);
                    st.connector.finish(&st.ctx).await;
                    if unknown {
                        return Ok(self.outcome_unknown(st, e.to_string()));
                    }
                    st.stage = Stage::Closed;
                    return Err(e);
                }
                Ok(Phase::Challenge(c)) => {
                    return Ok(TransferStep::NeedsChallenge { challenge: c });
                }
                Ok(Phase::Pending(c)) => return Ok(TransferStep::StillWaiting { challenge: c }),
                Ok(Phase::ConfirmPayee(check)) => {
                    return Ok(TransferStep::NeedsPayeeConfirmation { check });
                }
                Ok(Phase::Ready) if st.stage == Stage::LoggingIn => {
                    st.stage = Stage::Submitted;
                    phase = match &st.order {
                        OrderKind::Single(job) => st.connector.send_transfer(&st.ctx, job).await,
                        OrderKind::Batch { batch, .. } => {
                            st.connector.send_batch_transfer(&st.ctx, batch).await
                        }
                        OrderKind::Debit(d) => {
                            st.connector.send_direct_debit(&st.ctx, &d.job).await
                        }
                        OrderKind::Standing(o) => {
                            st.connector.send_standing_order(&st.ctx, o).await
                        }
                    };
                }
                Ok(Phase::Ready) => {
                    st.connector.finish(&st.ctx).await;
                    return match st.connector.transfer_result() {
                        TransferResult::Executed { instant } => {
                            st.stage = Stage::Executed;
                            // The money is gone either way: a failed write is
                            // reported with the outcome, never as an error the
                            // app would answer by offering to send it again.
                            let record_error = self
                                .record_executed(st, instant)
                                .err()
                                .map(|e| e.to_string());
                            Ok(TransferStep::Finished {
                                outcome: TransferOutcome {
                                    instant,
                                    end_to_end_id: st.order.end_to_end_id(),
                                    record_error,
                                },
                            })
                        }
                        TransferResult::InstantDeclined { reason } => {
                            st.stage = Stage::Closed;
                            Ok(TransferStep::InstantDeclined { reason })
                        }
                        TransferResult::Unknown { reason } => Ok(self.outcome_unknown(st, reason)),
                    };
                }
            }
        }
    }

    /// Stores the optimistic booking (or standing order, collection) for an
    /// executed order.
    fn record_executed(&self, st: &TransferState, instant: bool) -> CoreResult<()> {
        match &st.order {
            OrderKind::Standing(o) => self.vault.record_standing_order(st.account_id, o),
            OrderKind::Debit(d) => self.vault.record_direct_debit(st.account_id, d),
            OrderKind::Batch { batch, summary } => {
                self.vault
                    .record_batch_transfer(st.account_id, batch, summary)
            }
            // scheduled: shown under "Geplant" until the bank books it, not
            // as a booking dated today
            OrderKind::Single(job) if job.execution_date.is_some() => {
                self.vault.record_planned_transfer(st.account_id, job)
            }
            OrderKind::Single(job) => self.vault.record_pending_transfer(
                st.account_id,
                job,
                if instant {
                    "Echtzeitüberweisung (vorgemerkt)"
                } else {
                    "Überweisung (vorgemerkt)"
                },
            ),
        }
    }

    /// Stores a booking marked "Status unklar" so the possibly executed order
    /// stays visible (until the bank's booking arrives or 14 days pass).
    fn outcome_unknown(&self, st: &mut TransferState, reason: String) -> TransferStep {
        st.stage = Stage::OutcomeUnknown;
        let recorded = match &st.order {
            // A standing order is no booking: nothing to show until the next
            // fetch, where it appears (or does not). A collection is incoming
            // money: no optimistic booking and no mandate update. Either way
            // the user checks with the bank first.
            OrderKind::Standing(_) | OrderKind::Debit(_) => Ok(()),
            OrderKind::Batch { batch, summary } => {
                self.vault
                    .record_batch_pending(st.account_id, batch, summary, "(Status unklar)")
            }
            OrderKind::Single(job) => self.vault.record_pending_transfer(
                st.account_id,
                job,
                if job.instant {
                    "Echtzeitüberweisung (Status unklar)"
                } else {
                    "Überweisung (Status unklar)"
                },
            ),
        };
        // Only the marker booking is lost (nothing else is recorded for an
        // unclear order): the user checks the account either way.
        if let Err(e) = recorded {
            eprintln!("transfer: outcome unknown, not recorded: {e}");
        }
        TransferStep::OutcomeUnknown {
            reason,
            end_to_end_id: st.order.end_to_end_id(),
        }
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl TransferSession {
    pub async fn start(&self) -> Result<TransferStep, CoreError> {
        self.step(async |s| s.connector.begin(&s.ctx).await).await
    }

    pub async fn submit_code(&self, code: String) -> Result<TransferStep, CoreError> {
        let code = ChallengeResponse::Code(SecretString::new(code));
        self.step(async |s| s.connector.respond(&s.ctx, code).await)
            .await
    }

    pub async fn poll(&self) -> Result<TransferStep, CoreError> {
        self.step(async |s| s.connector.respond(&s.ctx, ChallengeResponse::Poll).await)
            .await
    }

    /// Send the transfer although the name check did not confirm the
    /// recipient (after `NeedsPayeeConfirmation`).
    pub async fn confirm_payee(&self) -> Result<TransferStep, CoreError> {
        self.step(async |s| {
            s.connector
                .respond(&s.ctx, ChallengeResponse::ConfirmPayee)
                .await
        })
        .await
    }

    /// Cancels the transfer. The result says whether the bank confirmed it:
    /// only `Cancelled` is final. For `CancellationPending`, `-Unknown` and
    /// `-Failed` the order stays tracked and `cancel()` can simply be called
    /// again; it re-checks the bank's status and never resends the order.
    /// Calling it again after `Cancelled` is a no-op. After `OutcomeUnknown`
    /// with no order left to withdraw, it always answers `CancellationUnknown`:
    /// only the account shows whether the order went through.
    pub async fn cancel(&self) -> CancelOutcome {
        let mut st = self.state.lock().await;
        match st.stage {
            Stage::Cancelled => return CancelOutcome::Cancelled,
            Stage::Executed => {
                return CancelOutcome::CancellationFailed {
                    reason: tr("Die Überweisung wurde bereits ausgeführt.").into(),
                };
            }
            Stage::OutcomeUnknown if !st.connector.has_open_order() => {
                return CancelOutcome::CancellationUnknown {
                    reason: tr("Ob die Überweisung ausgeführt wurde, ist unklar.").into(),
                };
            }
            // A closed session only needs another round if the bank may still
            // hold an open order from an earlier, unconfirmed cancellation.
            Stage::Closed if !st.connector.has_open_order() => return CancelOutcome::Cancelled,
            _ => {}
        }
        let TransferState { connector, ctx, .. } = &mut *st;
        let outcome = connector.cancel_transfer(ctx).await;
        if outcome == CancelOutcome::Cancelled {
            st.stage = Stage::Cancelled;
        } else if matches!(st.stage, Stage::LoggingIn | Stage::Submitted) {
            st.stage = Stage::Closed;
        }
        outcome
    }
}

/// "GC" + 30 hex digits: unique, within the 35-char SEPA limit and the
/// restricted character set.
pub(super) fn new_end_to_end_id() -> String {
    format!(
        "GC{}",
        &uuid::Uuid::new_v4().simple().to_string().to_uppercase()[..30]
    )
}

/// ISO 13616 Mod-97 IBAN check (whitespace and case ignored).
pub fn validate_iban(iban: String) -> bool {
    crate::iban::validate_iban(&iban)
}
