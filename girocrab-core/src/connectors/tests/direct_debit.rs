//! Direct debit submission: the connector gets one group, and a confirmed
//! submission shows up under "Geplant" and advances the mandates.

use std::sync::Arc;

use async_trait::async_trait;

use super::*;
use crate::mandates::{CollectionItem, MandateInput, MandateScheme};
use crate::test_support::TempDir;
use crate::vault::Vault;

struct Accepting {
    unknown: bool,
}

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
    async fn send_direct_debit(&mut self, _: &ConnectContext, job: &DebitJob) -> CoreResult<Phase> {
        assert_eq!(job.collections.len(), 2);
        Ok(Phase::Ready)
    }
    fn transfer_result(&self) -> TransferResult {
        if self.unknown {
            TransferResult::Unknown { reason: "x".into() }
        } else {
            TransferResult::default()
        }
    }
}

fn mandate(account_id: i64, reference: &str) -> MandateInput {
    MandateInput {
        account_id,
        debtor_name: "Mia Beispiel".into(),
        debtor_iban: "DE89370400440532013000".into(),
        debtor_bic: String::new(),
        reference: reference.into(),
        signed_date: "2025-01-15".into(),
        scheme: MandateScheme::Core,
        one_off: false,
        default_amount_cents: 1500,
        default_purpose: "Beitrag".into(),
        note: String::new(),
        active: true,
    }
}

/// (planned rows, planned sum, active mandates, mandates with last_collected)
async fn run(batch_booking: bool, last: bool, unknown: bool) -> (i64, i64, i64, i64) {
    let dir = TempDir::new("girocrab-debit");
    let vault = Vault::open(
        dir.join("v.db").to_string_lossy().to_string(),
        vec![6u8; 32],
    )
    .unwrap();
    vault.seed_demo_data().unwrap();
    for m in vault.list_mandates(None).unwrap() {
        vault.delete_mandate(m.id).unwrap();
    }
    let account_id = vault.list_accounts().unwrap()[0].id;
    vault
        .set_creditor_id(account_id, "DE98ZZZ09999999999".into())
        .unwrap();
    let a = vault
        .save_mandate(None, mandate(account_id, "M-1"))
        .unwrap();
    let b = vault
        .save_mandate(None, mandate(account_id, "M-2"))
        .unwrap();
    if last {
        // earlier collection: the next one can be the final one
        vault
            .mark_collected(vec![a.id, b.id], "2026-01-01".into())
            .unwrap();
    }
    let items: Vec<CollectionItem> = [a.id, b.id]
        .into_iter()
        .map(|id| CollectionItem {
            mandate_id: id,
            amount_cents: 1500,
            purpose: String::new(),
            last_collection: last,
        })
        .collect();
    let date = (crate::dates::today() + crate::dates::days(10)).to_string();
    let plan = vault.plan_collection(account_id, &items, &date).unwrap();
    let group = plan.groups.into_iter().next().unwrap();
    let (ids, collections): (Vec<i64>, Vec<_>) = group.rows.into_iter().unzip();
    let job = DebitJob {
        source_iban: "DE02120300000000202051".into(),
        creditor_name: "A".into(),
        creditor_id: plan.creditor_id,
        scheme: group.scheme.debit(),
        sequence: group.sequence,
        collection_date: date,
        collections,
        batch_booking,
    };
    let ctx = ConnectContext {
        connection_id: 1,
        since: "2020-01-01".into(),
        ..Default::default()
    };
    let s = TransferSession::new(
        Arc::clone(&vault),
        Box::new(Accepting { unknown }),
        ctx,
        OrderKind::Debit(DebitSubmission {
            job,
            mandate_ids: ids,
        }),
        account_id,
    );
    let step = s.start().await.unwrap();
    assert_eq!(unknown, matches!(step, TransferStep::OutcomeUnknown { .. }));
    let db = vault.db().unwrap();
    let one = |sql: &str| db.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
    (
        one(
            "SELECT count(*) FROM planned_transfers WHERE kind = 'debit' AND end_to_end_id NOT LIKE 'DEMO%'",
        ),
        one(
            "SELECT IFNULL(sum(amount_cents),0) FROM planned_transfers WHERE kind = 'debit' AND end_to_end_id NOT LIKE 'DEMO%'",
        ),
        one("SELECT count(*) FROM mandates WHERE active = 1"),
        one("SELECT count(*) FROM mandates WHERE last_collected IS NOT NULL"),
    )
}

#[tokio::test]
async fn sum_booking_shows_one_planned_line_and_marks_mandates() {
    assert_eq!(run(true, false, false).await, (1, 3_000, 2, 2));
}

#[tokio::test]
async fn single_bookings_show_one_line_per_collection() {
    assert_eq!(run(false, false, false).await, (2, 3_000, 2, 2));
}

#[tokio::test]
async fn final_collection_switches_the_mandates_off() {
    assert_eq!(run(true, true, false).await, (1, 3_000, 0, 2));
}

#[tokio::test]
async fn unknown_outcome_records_nothing() {
    assert_eq!(run(true, false, true).await, (0, 0, 2, 0));
}
