//! `TransferSession::cancel()`: the outcome comes from the bank, the session
//! stays retryable until the cancellation is confirmed.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;

use super::*;
use crate::vault::Vault;

struct Scripted {
    outcomes: Vec<CancelOutcome>,
    calls: Arc<AtomicUsize>,
    open: bool,
}

#[async_trait]
impl BankConnector for Scripted {
    async fn begin(&mut self, _ctx: &ConnectContext) -> CoreResult<Phase> {
        Ok(Phase::Ready)
    }
    async fn respond(&mut self, _: &ConnectContext, _: ChallengeResponse) -> CoreResult<Phase> {
        Ok(Phase::Ready)
    }
    async fn fetch(&mut self, _ctx: &ConnectContext) -> CoreResult<FetchResult> {
        Ok(FetchResult::default())
    }
    async fn cancel_transfer(&mut self, _ctx: &ConnectContext) -> CancelOutcome {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let out = self.outcomes[n.min(self.outcomes.len() - 1)].clone();
        self.open = out != CancelOutcome::Cancelled;
        out
    }
    fn has_open_order(&self) -> bool {
        self.open
    }
}

fn session(outcomes: Vec<CancelOutcome>) -> (TransferSession, Arc<AtomicUsize>) {
    let dir = std::env::temp_dir().join(format!(
        "gc-cancel-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let vault = Vault::open(dir.join("v.db").to_string_lossy().into(), vec![6u8; 32]).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let ctx = ConnectContext {
        connection_id: 1,
        login_name: String::new(),
        secret: crate::secret::SecretString::new(String::new()),
        settings: Default::default(),
        state: serde_json::Value::Null,
        since: "2020-01-01".into(),
        since_by_account: Default::default(),
    };
    let job = TransferJob {
        source_iban: "DE02120300000000202051".into(),
        source_owner: "A".into(),
        recipient_name: "B".into(),
        recipient_iban: "DE89370400440532013000".into(),
        recipient_bic: None,
        amount_cents: 100,
        purpose: "x".into(),
        instant: false,
        execution_date: None,
        end_to_end_id: "GC1".into(),
    };
    let connector = Scripted {
        outcomes,
        calls: calls.clone(),
        open: false,
    };
    (
        TransferSession::new(vault, Box::new(connector), ctx, job, 1),
        calls,
    )
}

fn pending() -> CancelOutcome {
    CancelOutcome::CancellationPending { reason: "r".into() }
}

#[tokio::test]
async fn cancel_retries_until_confirmed_then_stops() {
    let (s, calls) = session(vec![
        pending(),
        CancelOutcome::CancellationUnknown { reason: "r".into() },
        CancelOutcome::Cancelled,
    ]);
    assert_eq!(s.cancel().await, pending());
    // The session is closed for further steps, but cancel stays retryable.
    assert!(matches!(s.start().await, Err(CoreError::SessionClosed)));
    assert!(matches!(
        s.cancel().await,
        CancelOutcome::CancellationUnknown { .. }
    ));
    assert_eq!(s.cancel().await, CancelOutcome::Cancelled);
    // Repeated calls after confirmation do not contact the bank again.
    assert_eq!(s.cancel().await, CancelOutcome::Cancelled);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn confirmed_cancel_is_reported_once_per_bank_call() {
    let (s, calls) = session(vec![CancelOutcome::Cancelled]);
    assert_eq!(s.cancel().await, CancelOutcome::Cancelled);
    assert_eq!(s.cancel().await, CancelOutcome::Cancelled);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
