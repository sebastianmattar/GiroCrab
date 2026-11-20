//! TEMPLATE: how to add a new bank to GiroCrab in under 100 lines.
//!
//! Copy this file (e.g. `ing.rs`), rename `TemplateConnector`, then:
//!   1. implement `begin` / `respond` / `fetch` against the bank's API
//!   2. add a `ConnectorKind` variant in `models.rs` + a `BankDescriptor` in
//!      `connectors/catalog.rs::descriptor()`
//!   3. wire it into `make_connector()` and add a test (see below).
//!
//! Rules: money is always `i64` cents (never floats); never log secrets;
//! `external_id` must be stable so re-syncs de-duplicate (no bank id: leave it
//! empty and the vault derives one from the content); talk only to the bank,
//! never to a third party. Documents (PDF statements, receipts) follow
//! `ctx.documents`: nothing while it is `Off`. This file is not wired in and
//! uses only fake data, so it is safe to run: `cargo test -p girocrab-core template`.

use async_trait::async_trait;

use super::{
    BankConnector, ChallengeResponse, ConnectContext, FetchResult, Phase, RemoteAccount,
    RemoteTransaction,
};
use crate::error::{CoreError, CoreResult};
use crate::models::AccountKind;

#[derive(Default)]
pub struct TemplateConnector {
    logged_in: bool,
}

#[async_trait]
impl BankConnector for TemplateConnector {
    /// Phase 1: log in. Return `Phase::Challenge(..)` if the bank wants a TAN /
    /// push approval / redirect (see `ChallengeKind`), otherwise `Phase::Ready`.
    async fn begin(&mut self, ctx: &ConnectContext) -> CoreResult<Phase> {
        if ctx.login_name.is_empty() || ctx.secret.expose().is_empty() {
            return Err(CoreError::Authentication {
                message: "Login name and PIN required".into(),
            });
        }
        // Real connector: `let http = super::http_client()?;` then POST the login.
        self.logged_in = true;
        Ok(Phase::Ready)
    }

    /// Phase 2: answer (or poll) the challenge returned by `begin`.
    async fn respond(
        &mut self,
        _ctx: &ConnectContext,
        _response: ChallengeResponse,
    ) -> CoreResult<Phase> {
        Ok(Phase::Ready)
    }

    /// Download accounts and their bookings, each account from
    /// `ctx.since_for(account)` (YYYY-MM-DD) on. Read a paged booking list
    /// with `super::paginate`.
    async fn fetch(&mut self, ctx: &ConnectContext) -> CoreResult<FetchResult> {
        let today = super::today();
        let account = "template-1"; // stable id at the bank
        let mut out = FetchResult::default();
        out.accounts.push(RemoteAccount {
            external_ref: account.into(),
            name: "Template Girokonto".into(),
            owner: "Erika Mustermann".into(),
            iban: "DE89370400440532013000".into(), // public example IBAN
            account_number: String::new(),
            currency: "EUR".into(),
            kind: AccountKind::Checking,
            balance_cents: 1_234_56,
            balance_date: today.clone(),
        });
        let booking = RemoteTransaction {
            account_ref: account.into(),
            external_id: "template-0001".into(), // the bank's id
            booking_date: today.clone(),
            value_date: today,
            amount_cents: -9_99, // debit = negative
            currency: "EUR".into(),
            counterparty: "Beispiel Shop GmbH".into(),
            counterparty_iban: String::new(),
            purpose: "Kartenzahlung".into(),
            booking_text: "KARTENZAHLUNG".into(),
            ..Default::default()
        };
        // Real connector: ask the bank for the bookings from this date on.
        if booking.booking_date.as_str() >= ctx.since_for(account) {
            out.transactions.push(booking);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::SecretString;

    #[tokio::test]
    async fn template_flow_returns_one_account_and_booking() {
        let ctx = ConnectContext {
            connection_id: 1,
            login_name: "demo".into(),
            secret: SecretString::new("1234".into()),
            since: "2026-01-01".into(),
            ..Default::default()
        };
        let mut c = TemplateConnector::default();
        assert!(matches!(c.begin(&ctx).await.unwrap(), Phase::Ready));
        let r = c.fetch(&ctx).await.unwrap();
        assert_eq!((r.accounts.len(), r.transactions.len()), (1, 1));
    }
}
