use super::*;

fn setup(anchor: &str, balance: i64, ppm: Option<i64>, expr: &str) -> LoanSetup {
    LoanSetup {
        name: "Autokredit".into(),
        original_cents: Some(15_000_00),
        anchor_date: anchor.into(),
        anchor_balance_cents: balance,
        interest_rate_ppm: ppm,
        match_expr: expr.into(),
    }
}

fn temp_vault() -> (Arc<Vault>, std::path::PathBuf) {
    let p = std::env::temp_dir().join(format!("girocrab-loans-{}.db", uuid::Uuid::new_v4()));
    let v = Vault::open(p.to_string_lossy().to_string(), vec![7u8; 32]).unwrap();
    (v, p)
}

use std::sync::Arc;

/// Giro account with three car-loan rates, a supermarket booking and a
/// salary; returns (giro id, [rate ids], rewe id).
fn seed(v: &Vault) -> (i64, Vec<i64>, i64) {
    let db = v.db().unwrap();
    db.execute("INSERT INTO account_groups(name) VALUES ('G')", [])
        .unwrap();
    let g = db.last_insert_rowid();
    db.execute(
        "INSERT INTO accounts(group_id, external_ref, name, kind, iban) VALUES (?1, 'giro', 'Giro', 'checking', 'DE02120300000000202051')",
        [g],
    )
    .unwrap();
    let giro = db.last_insert_rowid();
    let mut rates = vec![];
    for (i, d) in ["2026-08-01", "2026-09-01", "2026-10-01"]
        .iter()
        .enumerate()
    {
        db.execute(
            "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, counterparty, purpose)
             VALUES (?1, ?2, ?3, -30000, 'Autobank AG', 'Rate Kreditvertrag 4711')",
            params![giro, format!("r{i}"), d],
        )
        .unwrap();
        rates.push(db.last_insert_rowid());
    }
    db.execute(
        "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, counterparty, purpose)
         VALUES (?1, 'rewe', '2026-09-15', -5000, 'REWE', 'Einkauf')",
        [giro],
    )
    .unwrap();
    let rewe = db.last_insert_rowid();
    (giro, rates, rewe)
}

fn balance(v: &Vault, id: i64) -> (i64, String, Option<i64>) {
    let a = v
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|a| a.id == id)
        .unwrap();
    (a.balance_cents, a.balance_date, a.initial_balance_cents)
}

#[test]
fn status_without_interest_counts_payments_after_the_anchor() {
    let s = setup("2026-08-01", 8_400_00, None, "");
    let st = compute_status(
        &s,
        &[
            ("2026-08-01".into(), -300_00),
            ("2026-09-01".into(), -300_00),
            ("2026-10-01".into(), -300_00),
        ],
    );
    assert_eq!(st.remaining_cents, 7_800_00);
    assert_eq!(st.payment_count, 2);
    assert_eq!(st.before_anchor_count, 1);
    assert_eq!(st.paid_cents, 600_00);
    assert_eq!(st.principal_cents, 600_00);
    assert_eq!(st.interest_cents, 0);
    assert_eq!(st.as_of_date, "2026-10-01");
    assert_eq!(st.last_payment_cents, Some(300_00));
    assert_eq!(st.months_left, Some(26));
}

#[test]
fn status_with_interest_only_counts_principal() {
    // 6 % p.a. on 10,000.00 for 30 days = 49.32 interest
    let s = setup("2026-01-01", 10_000_00, Some(60_000), "");
    let st = compute_status(&s, &[("2026-01-31".into(), -500_00)]);
    assert_eq!(st.interest_cents, 4_932);
    assert_eq!(st.principal_cents, 500_00 - 4_932);
    assert_eq!(st.remaining_cents, 10_000_00 + 4_932 - 500_00);
    // a payment below the monthly interest never pays the loan off
    let st = compute_status(&s, &[("2026-01-31".into(), -40_00)]);
    assert_eq!(st.months_left, None);
    // no payment yet: balance is the Stand
    let st = compute_status(&s, &[]);
    assert_eq!(st.remaining_cents, 10_000_00);
    assert_eq!(st.as_of_date, "2026-01-01");
    assert_eq!(st.months_left, None);
}

#[test]
fn overpaid_loan_stops_at_zero() {
    let s = setup("2026-01-01", 100_00, None, "");
    let st = compute_status(&s, &[("2026-02-01".into(), -300_00)]);
    assert_eq!(st.remaining_cents, 0);
    assert_eq!(st.months_left, Some(0));
}

#[test]
fn incoming_payout_increases_the_debt() {
    let s = setup("2026-01-01", 1_000_00, None, "");
    let st = compute_status(&s, &[("2026-02-01".into(), 500_00)]);
    assert_eq!(st.remaining_cents, 1_500_00);
}

#[test]
fn validation() {
    let ok = setup("2026-08-01", 1, None, "");
    assert!(validate(&ok).is_ok());
    for bad in [
        LoanSetup {
            name: "  ".into(),
            ..ok.clone()
        },
        LoanSetup {
            anchor_date: "2026-13-01".into(),
            ..ok.clone()
        },
        LoanSetup {
            anchor_date: "2999-01-01".into(),
            ..ok.clone()
        },
        LoanSetup {
            anchor_balance_cents: -1,
            ..ok.clone()
        },
        LoanSetup {
            original_cents: Some(0),
            ..ok.clone()
        },
        LoanSetup {
            interest_rate_ppm: Some(300_001),
            ..ok.clone()
        },
        LoanSetup {
            match_expr: "(Autobank".into(),
            ..ok.clone()
        },
        LoanSetup {
            match_expr: "category:Auto".into(),
            ..ok.clone()
        },
    ] {
        assert!(validate(&bad).is_err(), "{bad:?}");
    }
    // a 0 % rate is stored as "no rate", the name is trimmed
    let v = validate(&LoanSetup {
        name: " Auto ".into(),
        interest_rate_ppm: Some(0),
        ..ok
    })
    .unwrap();
    assert_eq!(v.name, "Auto");
    assert_eq!(v.interest_rate_ppm, None);
}

#[test]
fn rule_links_rates_and_drives_the_account_balance() {
    let (v, p) = temp_vault();
    let (giro, rates, rewe) = seed(&v);
    let s = setup("2026-08-01", 8_400_00, None, "Autobank");
    let preview = v.preview_manual_loan(None, s.clone(), 10).unwrap();
    assert_eq!(preview.match_count, 3);
    assert_eq!(preview.status.remaining_cents, 7_800_00);
    // The limit cuts the listed bookings (newest first), never count or status.
    let first = v.preview_manual_loan(None, s.clone(), 1).unwrap();
    assert_eq!(first.match_count, 3);
    assert_eq!(first.status, preview.status);
    let listed: Vec<i64> = first.matches.iter().map(|b| b.id).collect();
    assert_eq!(listed, [rates[2]]);
    assert_eq!(first.matches[0].counterparty, "Autobank AG");

    let loan = v.create_manual_loan(s).unwrap();
    assert_eq!(
        balance(&v, loan),
        (-7_800_00, "2026-10-01".into(), Some(15_000_00))
    );
    let acc = v
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|a| a.id == loan)
        .unwrap();
    assert_eq!(acc.kind, AccountKind::Loan);
    assert_eq!(acc.connection_id, None);
    let loans = v.list_manual_loans().unwrap();
    assert_eq!(loans.len(), 1);
    assert_eq!(loans[0].status, preview.status);
    let links = v.list_loan_links().unwrap();
    assert_eq!(links.len(), 3);
    assert!(links.iter().all(|l| l.by_rule && l.loan_account_id == loan));

    // loan view lists the Giro rates, classified as loan flow
    let page = v
        .list_transactions(BookingFilter {
            account_ids: vec![loan],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(page.transactions.len(), 3);
    assert!(
        page.transactions
            .iter()
            .all(|b| b.flow == crate::merchants::FlowKind::Loan)
    );
    assert_eq!(page.consumption_cents, 0);
    // the Giro view is unchanged
    let page = v
        .list_transactions(BookingFilter {
            account_ids: vec![giro],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(page.transactions.len(), 4);

    // unlink a rate by hand: it stays unlinked when rules run again
    v.set_booking_loan(rates[2], None).unwrap();
    v.apply_rules(false).unwrap();
    assert_eq!(balance(&v, loan).0, -8_100_00);
    assert_eq!(v.list_loan_links().unwrap().len(), 2);
    // link a non-matching booking by hand (e.g. a Sondertilgung)
    v.set_booking_loan(rewe, Some(loan)).unwrap();
    assert_eq!(balance(&v, loan).0, -8_050_00);
    // undo both
    v.set_booking_loan(rewe, None).unwrap();
    v.set_booking_loan(rates[2], Some(loan)).unwrap();
    assert_eq!(balance(&v, loan).0, -7_800_00);

    // a new import is linked automatically
    {
        let db = v.db().unwrap();
        db.execute(
            "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, counterparty, purpose)
             VALUES (?1, 'r3', '2026-11-01', -30000, 'Autobank AG', 'Rate')",
            [giro],
        )
        .unwrap();
        Vault::apply_rules_inner(&db, true).unwrap();
    }
    assert_eq!(balance(&v, loan).0, -7_500_00);

    // new Stand from the annual statement
    v.set_loan_anchor(loan, "2026-10-31".into(), 7_700_00)
        .unwrap();
    assert_eq!(
        balance(&v, loan),
        (-7_400_00, "2026-11-01".into(), Some(15_000_00))
    );
    let st = &v.list_manual_loans().unwrap()[0].status;
    assert_eq!(st.payment_count, 1);
    assert_eq!(st.before_anchor_count, 3);

    // changing the expression drops rule links, keeps hand-made ones
    let mut s2 = v.list_manual_loans().unwrap()[0].setup.clone();
    s2.match_expr = "Kreditvertrag".into();
    v.update_manual_loan(loan, s2.clone()).unwrap();
    let links = v.list_loan_links().unwrap();
    assert_eq!(links.len(), 3); // r3 has no "Kreditvertrag", rates[2] is manual now
    assert!(
        links
            .iter()
            .any(|l| l.transaction_id == rates[2] && !l.by_rule)
    );
    s2.match_expr = String::new();
    v.update_manual_loan(loan, s2).unwrap();
    assert_eq!(v.list_loan_links().unwrap().len(), 1);

    drop(v);
    let _ = std::fs::remove_file(p);
}

#[test]
fn bookings_of_loan_accounts_cannot_be_linked() {
    let (v, p) = temp_vault();
    let (_, rates, _) = seed(&v);
    let loan = v
        .create_manual_loan(setup("2026-08-01", 1_000_00, None, ""))
        .unwrap();
    let other = v
        .create_manual_loan(setup("2026-08-01", 1_000_00, None, ""))
        .unwrap();
    let loan_tx = {
        let db = v.db().unwrap();
        db.execute(
            "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents) VALUES (?1, 'x', '2026-09-01', 100)",
            [loan],
        )
        .unwrap();
        db.last_insert_rowid()
    };
    assert!(v.set_booking_loan(loan_tx, Some(other)).is_err());
    assert!(v.set_booking_loan(rates[0], Some(9999)).is_err());
    assert!(v.set_booking_loan(9999, Some(loan)).is_err());
    // moving a booking between loans refreshes both
    v.set_booking_loan(rates[1], Some(loan)).unwrap();
    assert_eq!(balance(&v, loan).0, -700_00);
    v.set_booking_loan(rates[1], Some(other)).unwrap();
    assert_eq!(balance(&v, loan).0, -1_000_00);
    assert_eq!(balance(&v, other).0, -700_00);
    drop(v);
    let _ = std::fs::remove_file(p);
}

#[test]
fn deleting_the_paying_account_refreshes_the_loan() {
    let (v, p) = temp_vault();
    let (giro, _, _) = seed(&v);
    let loan = v
        .create_manual_loan(setup("2026-07-31", 8_400_00, None, "Autobank"))
        .unwrap();
    assert_eq!(balance(&v, loan).0, -7_500_00);
    // archiving keeps the rates (GiroCrab/GiroCrab#65)
    v.set_account_archived(giro, true).unwrap();
    assert_eq!(balance(&v, loan).0, -7_500_00);
    v.delete_account(giro).unwrap();
    assert_eq!(balance(&v, loan).0, -8_400_00);
    assert!(v.list_loan_links().unwrap().is_empty());
    // deleting the loan removes its terms
    v.set_account_archived(loan, true).unwrap();
    v.delete_account(loan).unwrap();
    assert!(v.list_manual_loans().unwrap().is_empty());
    drop(v);
    let _ = std::fs::remove_file(p);
}

#[test]
fn merge_takes_over_manual_loans_and_hand_made_links() {
    let (a, pa) = temp_vault();
    let (_, rates, rewe) = seed(&a);
    let loan = a
        .create_manual_loan(setup("2026-07-31", 8_400_00, Some(49_900), "Autobank"))
        .unwrap();
    a.set_booking_loan(rewe, Some(loan)).unwrap();
    a.set_booking_loan(rates[0], None).unwrap();
    let expected = a.list_manual_loans().unwrap()[0].status.clone();
    drop(a);

    let (b, pb) = temp_vault();
    b.merge_other_vault(pa.to_string_lossy().to_string(), vec![7u8; 32])
        .unwrap();
    let loans = b.list_manual_loans().unwrap();
    assert_eq!(loans.len(), 1);
    assert_eq!(loans[0].setup.interest_rate_ppm, Some(49_900));
    assert_eq!(loans[0].status, expected);
    let acc = b
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|a| a.id == loans[0].account_id)
        .unwrap();
    assert_eq!(acc.balance_cents, -expected.remaining_cents);
    // merging again changes nothing
    b.merge_other_vault(pa.to_string_lossy().to_string(), vec![7u8; 32])
        .unwrap();
    assert_eq!(b.list_manual_loans().unwrap()[0].status, expected);
    assert_eq!(b.list_manual_loans().unwrap().len(), 1);
    drop(b);
    let _ = std::fs::remove_file(pa);
    let _ = std::fs::remove_file(pb);
}

/// A manual loan turned into a cash account is valued by its opening balance
/// and bookings from then on; turned back, it is a loan again.
#[test]
fn loan_changed_to_cash_is_an_ordinary_account_until_changed_back() {
    let (v, p) = temp_vault();
    seed(&v);
    let loan = v
        .create_manual_loan(setup("2026-08-01", 8_400_00, None, "Autobank"))
        .unwrap();
    assert_eq!(balance(&v, loan).0, -7_800_00);

    v.set_account_kind(loan, AccountKind::Cash).unwrap();
    assert_eq!(balance(&v, loan).0, 0);
    assert!(v.list_manual_loans().unwrap().is_empty());
    v.set_account_opening_balance(loan, Some(100_00), String::new())
        .unwrap();
    apply_loan_rules(&v.db().unwrap()).unwrap();
    assert_eq!(balance(&v, loan).0, 100_00);

    v.set_account_kind(loan, AccountKind::Loan).unwrap();
    assert_eq!(balance(&v, loan).0, -7_800_00);
    assert_eq!(v.list_manual_loans().unwrap().len(), 1);
    drop(v);
    let _ = std::fs::remove_file(p);
}

#[test]
fn vault_without_loan_tables_is_migrated() {
    let (v, p) = temp_vault();
    {
        let db = v.db().unwrap();
        // a schema-8 vault from before the loan feature (schema 9 repairs it)
        db.execute_batch("DROP TABLE loan_links; DROP TABLE loan_terms; DROP TABLE sync_log; PRAGMA user_version = 8;")
            .unwrap();
        // rules still run on a vault that lacks the tables
        Vault::apply_rules_inner(&db, true).unwrap();
    }
    drop(v);
    let v = Vault::open(p.to_string_lossy().to_string(), vec![7u8; 32]).unwrap();
    assert!(v.list_manual_loans().unwrap().is_empty());
    assert!(v.list_loan_links().unwrap().is_empty());
    drop(v);
    let _ = std::fs::remove_file(p);
}
