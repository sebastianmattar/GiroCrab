//! `SyncSession`: the two-phase login/fetch state machine exposed via FFI.

use std::sync::Arc;

use tokio::sync::Mutex as AsyncMutex;

use crate::error::{CoreError, CoreResult};
use crate::models::ConnectorKind;
use crate::secret::SecretString;
use crate::vault::Vault;

use super::*;

/// The open bank dialog.
struct Active {
    connector: Box<dyn BankConnector>,
    ctx: ConnectContext,
}

/// One sync run for one bank connection. Created by `Vault::begin_sync`.
#[derive(uniffi::Object)]
pub struct SyncSession {
    vault: Arc<Vault>,
    kind: ConnectorKind,
    /// `None` once the sync finished, failed or was cancelled, so every
    /// later step gets `SessionClosed`.
    state: AsyncMutex<Option<Active>>,
}

impl SyncSession {
    pub(super) fn new(
        vault: Arc<Vault>,
        kind: ConnectorKind,
        connector: Box<dyn BankConnector>,
        ctx: ConnectContext,
    ) -> Self {
        Self {
            vault,
            kind,
            state: AsyncMutex::new(Some(Active { connector, ctx })),
        }
    }

    /// Copy of the prepared login context (settings, PIN, connector state),
    /// e.g. to reuse the sync set-up for a transfer.
    pub(super) fn clone_context(&self) -> CoreResult<ConnectContext> {
        let st = self
            .state
            .try_lock()
            .map_err(|_| CoreError::SessionClosed)?;
        Ok(st.as_ref().ok_or(CoreError::SessionClosed)?.ctx.clone())
    }

    /// Runs one step on the open dialog and closes it when the sync ends.
    async fn step(
        &self,
        call: impl AsyncFnOnce(&mut Active) -> CoreResult<Phase>,
    ) -> CoreResult<SyncStep> {
        let mut st = self.state.lock().await;
        let active = st.as_mut().ok_or(CoreError::SessionClosed)?;
        let phase = call(active).await;
        let step = self.advance(phase, active).await;
        if !matches!(
            step,
            Ok(SyncStep::NeedsChallenge { .. } | SyncStep::StillWaiting { .. })
        ) {
            *st = None;
        }
        step
    }

    async fn advance(&self, phase: CoreResult<Phase>, st: &mut Active) -> CoreResult<SyncStep> {
        match phase {
            Err(e) => {
                st.connector.finish(&st.ctx).await;
                Err(e)
            }
            Ok(Phase::Challenge(c)) => Ok(SyncStep::NeedsChallenge { challenge: c }),
            Ok(Phase::Pending(c)) => Ok(SyncStep::StillWaiting { challenge: c }),
            Ok(Phase::ConfirmPayee(_)) => {
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
                let mut data = fetched?;
                if let Some(only) = &st.ctx.only_account {
                    data.keep_only(only);
                }
                let summary = store_fetch(&self.vault, self.kind, st.ctx.connection_id, data)?;
                Ok(SyncStep::Finished { summary })
            }
        }
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl SyncSession {
    /// Phase 1: log in. Returns a challenge or finishes the sync.
    pub async fn start(&self) -> Result<SyncStep, CoreError> {
        self.step(async |s| s.connector.begin(&s.ctx).await).await
    }

    /// Phase 2a: submit the code / redirect URL the user typed.
    pub async fn submit_code(&self, code: String) -> Result<SyncStep, CoreError> {
        let code = ChallengeResponse::Code(SecretString::new(code));
        self.step(async |s| s.connector.respond(&s.ctx, code).await)
            .await
    }

    /// Phase 2b: ask whether the decoupled push approval happened.
    pub async fn poll(&self) -> Result<SyncStep, CoreError> {
        self.step(async |s| s.connector.respond(&s.ctx, ChallengeResponse::Poll).await)
            .await
    }

    /// Cancels the sync and closes the bank dialog.
    pub async fn cancel(&self) {
        if let Some(mut a) = self.state.lock().await.take() {
            a.connector.finish(&a.ctx).await;
        }
    }

    pub fn bank_name(&self) -> String {
        descriptor(self.kind).name
    }
}
