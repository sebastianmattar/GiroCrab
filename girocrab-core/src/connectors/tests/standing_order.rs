//! Standing order changes (#496): the connector gets the job, and once the
//! bank accepted it the vault mirrors it until the next fetch replaces the list.

use std::sync::Arc;

use async_trait::async_trait;

use super::*;
use crate::models::StandingOrderAction as Action;
use crate::vault::Vault;

struct Accepting;

#[async_trait]
impl BankConnector for Accepting {
    async fn begin(&mut self, _: &ConnectContext) -> CoreResult<Phase> {
        Ok(Phase::Ready)
    }
    async fn respond(&mut self, _: &ConnectContext, _: ChallengeResponse) -> CoreResult<Phase> {
        Ok(Phase::Ready)
    }
    async fn fetch(&mut self, _: &ConnectContext) -> CoreResult<FetchResult> {
        Ok(FetchResult::default())
    }
    async fn send_standing_order(
        &mut self,
        _: &ConnectContext,
        job: &StandingOrderJob,
    ) -> CoreResult<Phase> {
        assert!(!job.recipient_iban.is_empty());
        Ok(Phase::Ready)
    }
}

fn vault_with_order() -> (Arc<Vault>, i64) {
    let vault = Vault::open(
        crate::test_support::thread_temp_path("standing-order")
            .to_string_lossy()
            .into(),
        vec![8u8; 32],
    )
    .unwrap();
    let account_id = {
        let db = vault.db().unwrap();
        db.execute(
            "INSERT INTO account_groups(name, sf_symbol, sort_order) VALUES ('G', 'tray', 1)",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO accounts(group_id, external_ref, name, kind)
             VALUES ((SELECT min(id) FROM account_groups), 'a', 'a', 'checking')",
            [],
        )
        .unwrap();
        db.last_insert_rowid()
    };
    let rent = RemoteStandingOrder {
        order_id: "A1".into(),
        recipient_name: "Vermieter".into(),
        recipient_iban: "DE89370400440532013000".into(),
        amount_cents: 85_000,
        purpose: "Miete".into(),
        first_date: "2026-12-01".into(),
        time_unit: "M".into(),
        interval: 1,
        exec_day: 1,
        can_change: true,
        can_delete: true,
        ..Default::default()
    };
    let gym = RemoteStandingOrder {
        order_id: "B2".into(),
        recipient_name: "Studio".into(),
        amount_cents: 2_500,
        ..rent.clone()
    };
    crate::standing_orders::replace_for_account(&vault.db().unwrap(), account_id, &[&rent, &gym])
        .unwrap();
    (vault, account_id)
}

fn job(action: Action, order_id: &str) -> StandingOrderJob {
    StandingOrderJob {
        action,
        source_iban: "DE02120300000000202051".into(),
        source_owner: "A".into(),
        order_id: order_id.into(),
        recipient_name: "Neuer Vermieter".into(),
        recipient_iban: "DE89370400440532013000".into(),
        recipient_bic: None,
        amount_cents: 90_000,
        purpose: "Miete neu".into(),
        first_date: "2027-01-01".into(),
        last_date: Some("2028-01-01".into()),
        time_unit: "M".into(),
        interval: 3,
        exec_day: 15,
    }
}

async fn run(vault: &Arc<Vault>, account_id: i64, order: StandingOrderJob) {
    let ctx = ConnectContext {
        connection_id: 1,
        since: "2020-01-01".into(),
        ..Default::default()
    };
    let s = TransferSession::new(
        Arc::clone(vault),
        Box::new(Accepting),
        ctx,
        OrderKind::Standing(order),
        account_id,
    );
    assert!(matches!(
        s.start().await.unwrap(),
        TransferStep::Finished { .. }
    ));
}

#[tokio::test]
async fn deleting_removes_only_that_order() {
    let (v, acc) = vault_with_order();
    run(&v, acc, job(Action::Delete, "A1")).await;
    let left = v.list_standing_orders(vec![acc]).unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].order_id, "B2");
}

#[tokio::test]
async fn changing_rewrites_the_order_and_keeps_the_others() {
    let (v, acc) = vault_with_order();
    run(&v, acc, job(Action::Change, "A1")).await;
    let all = v.list_standing_orders(vec![acc]).unwrap();
    let a1 = all.iter().find(|o| o.order_id == "A1").unwrap();
    assert_eq!(a1.amount_cents, 90_000);
    assert_eq!(a1.recipient_name, "Neuer Vermieter");
    assert_eq!((a1.interval, a1.exec_day), (3, 15));
    assert_eq!(a1.last_date.as_deref(), Some("2028-01-01"));
    assert!(a1.can_change, "the bank's flags stay until the next fetch");
    let b2 = all.iter().find(|o| o.order_id == "B2").unwrap();
    assert_eq!(b2.amount_cents, 2_500);
}

#[tokio::test]
async fn a_new_order_appears_but_cannot_be_edited_before_the_next_fetch() {
    let (v, acc) = vault_with_order();
    run(&v, acc, job(Action::Create, "")).await;
    let all = v.list_standing_orders(vec![acc]).unwrap();
    assert_eq!(all.len(), 3);
    let new = all.iter().find(|o| o.order_id.is_empty()).unwrap();
    assert_eq!(new.amount_cents, 90_000);
    assert!(!new.can_change && !new.can_delete);
}

#[test]
fn listing_filters_by_account_and_finds_one_by_row_id() {
    let (v, acc) = vault_with_order();
    assert_eq!(v.list_standing_orders(vec![]).unwrap().len(), 2);
    assert!(v.list_standing_orders(vec![acc + 1]).unwrap().is_empty());
    let first = v.list_standing_orders(vec![acc]).unwrap().remove(0);
    assert_eq!(v.standing_order(first.id).unwrap().unwrap(), first);
    assert!(v.standing_order(9_999).unwrap().is_none());
}

#[test]
fn limits_are_unknown_without_parameters_from_the_bank() {
    let (vault, account_id) = vault_with_order();
    assert!(
        vault
            .standing_order_limits(account_id, Action::Create)
            .unwrap()
            .is_none()
    );
    assert!(
        vault
            .standing_order_limits(9999, Action::Change)
            .unwrap()
            .is_none()
    );
}
