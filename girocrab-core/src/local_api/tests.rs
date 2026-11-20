use super::*;
use crate::test_support::TempDir;
use std::collections::HashMap;
use std::sync::Arc;

fn demo() -> (TempDir, Arc<Vault>) {
    let dir = TempDir::new("local-api");
    let v = Vault::open(
        dir.join("v.db").to_string_lossy().to_string(),
        vec![5u8; 32],
    )
    .unwrap();
    v.seed_demo_data().unwrap();
    (dir, v)
}

fn account_id(v: &Vault, name: &str) -> i64 {
    v.list_accounts()
        .unwrap()
        .into_iter()
        .find(|a| a.name == name)
        .unwrap()
        .id
}

fn share(v: &Vault, accounts: &[i64], edit: impl FnOnce(&mut ShareScope)) {
    let mut scope = ShareScope {
        enabled: true,
        account_ids: accounts.to_vec(),
        ..Default::default()
    };
    edit(&mut scope);
    v.set_local_api_scope(scope).unwrap();
}

fn call(v: &Vault, method: &str, params: Value) -> Value {
    let req = json!({ "v": 1, "method": method, "params": params }).to_string();
    let resp: Value = serde_json::from_str(&v.local_api_request(&req)).unwrap();
    assert_eq!(resp["v"], 1);
    resp
}

fn ok(v: &Vault, method: &str, params: Value) -> Value {
    let resp = call(v, method, params);
    assert!(resp.get("error").is_none(), "{method}: {resp}");
    resp["result"].clone()
}

fn err_code(v: &Vault, method: &str, params: Value) -> String {
    call(v, method, params)["error"]["code"]
        .as_str()
        .unwrap()
        .to_string()
}

/// The longest summary range, ending a few days from now.
fn summary_range() -> (String, String) {
    let to = crate::dates::today() + crate::dates::days(5);
    let from = to - crate::dates::days(MAX_SUMMARY_DAYS);
    (from.to_string(), to.to_string())
}

/// Every booking of `params` across all pages of `limit`.
fn all_pages(v: &Vault, mut params: Value, limit: u32) -> Vec<Value> {
    params["limit"] = json!(limit);
    let mut out = Vec::new();
    loop {
        let page = ok(v, "transactions", params.clone());
        out.extend(page["transactions"].as_array().unwrap().iter().cloned());
        match page["next_cursor"].as_str() {
            Some(c) => params["cursor"] = json!(c),
            None => return out,
        }
    }
}

#[test]
fn off_until_enabled_and_revocable() {
    let (_d, v) = demo();
    assert_eq!(err_code(&v, "info", Value::Null), "disabled");
    let giro = account_id(&v, "Musterbank Girokonto");
    share(&v, &[giro], |_| {});
    assert_eq!(ok(&v, "info", Value::Null)["account_count"], 1);
    share(&v, &[giro], |s| s.enabled = false);
    assert_eq!(err_code(&v, "accounts", Value::Null), "disabled");
}

#[test]
fn only_read_methods_and_this_version() {
    let (_d, v) = demo();
    share(&v, &[account_id(&v, "Musterbank Girokonto")], |_| {});
    for method in [
        "set_transaction_category",
        "update_transaction_note",
        "list_bank_connections",
    ] {
        assert_eq!(err_code(&v, method, Value::Null), "unknown_method");
    }
    let resp: Value =
        serde_json::from_str(&v.local_api_request(r#"{"v":2,"method":"info"}"#)).unwrap();
    assert_eq!(resp["error"]["code"], "unsupported_version");
    let resp: Value = serde_json::from_str(&v.local_api_request("not json")).unwrap();
    assert_eq!(resp["error"]["code"], "invalid_params");
    assert_eq!(
        err_code(&v, "transactions", json!({ "accout_ids": [1] })),
        "invalid_params",
        "unknown parameters are rejected, not ignored"
    );
}

#[test]
fn accounts_are_an_allow_list_without_numbers_by_default() {
    let (_d, v) = demo();
    let giro = account_id(&v, "Musterbank Girokonto");
    let savings = account_id(&v, "Tagesgeld Plus");
    share(&v, &[giro], |_| {});
    let accounts = ok(&v, "accounts", Value::Null);
    let accounts = accounts.as_array().unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0]["id"], giro);
    assert!(accounts[0].get("iban").is_none());
    assert_eq!(
        err_code(&v, "transactions", json!({ "account_ids": [savings] })),
        "not_shared"
    );
    assert_eq!(
        err_code(&v, "transactions", json!({ "account_ids": [999_999] })),
        "not_shared",
        "an unknown id reads like an unshared one"
    );
    share(&v, &[giro], |s| s.share_account_numbers = true);
    let accounts = ok(&v, "accounts", Value::Null);
    assert_eq!(accounts[0]["iban"], "DE89370400440532013000");
}

#[test]
fn bookings_stay_in_shared_accounts_and_hide_ibans_and_notes() {
    let (_d, v) = demo();
    let giro = account_id(&v, "Musterbank Girokonto");
    let booking = v
        .list_transactions(BookingFilter {
            account_ids: vec![giro],
            limit: 1,
            ..Default::default()
        })
        .unwrap()
        .transactions[0]
        .id;
    v.update_transaction_note(booking, "Geheime Notiz".into())
        .unwrap();
    {
        let db = v.db().unwrap();
        db.execute(
            "UPDATE transactions SET counterparty_iban = 'DE02120300000000202051',
                    purpose = 'Miete an DE02 1203 0000 0000 2020 51' WHERE id = ?1",
            [booking],
        )
        .unwrap();
    }
    share(&v, &[giro], |_| {});
    let rows = all_pages(&v, json!({}), 50);
    assert!(!rows.is_empty());
    assert!(rows.iter().all(|t| t["account_id"] == giro));
    let text = serde_json::to_string(&rows).unwrap();
    assert!(!text.contains("DE02"), "IBAN leaked: {text}");
    assert!(!text.contains("Geheime Notiz"));
    let row = rows.iter().find(|t| t["id"] == booking).unwrap();
    assert_eq!(row["purpose"], "Miete an [IBAN]");

    share(&v, &[giro], |s| {
        s.share_account_numbers = true;
        s.share_notes = true;
    });
    let rows = all_pages(&v, json!({}), 50);
    let row = rows.iter().find(|t| t["id"] == booking).unwrap();
    assert_eq!(row["counterparty_iban"], "DE02120300000000202051");
    assert_eq!(row["note"], "Geheime Notiz");

    share(&v, &[giro], |s| {
        s.share_counterparty = false;
        s.share_purpose = false;
    });
    let rows = all_pages(&v, json!({}), 50);
    assert!(rows.iter().all(|t| t.get("counterparty").is_none()
        && t.get("purpose").is_none()
        && t.get("booking_text").is_none()
        && t["merchant"] == ""));
}

#[test]
fn loan_view_does_not_pull_in_other_accounts() {
    let (_d, v) = demo();
    let giro = account_id(&v, "Musterbank Girokonto");
    let loan = account_id(&v, "Immobilien-Darlehen");
    let linked = v
        .list_transactions(BookingFilter {
            account_ids: vec![giro],
            limit: 1,
            ..Default::default()
        })
        .unwrap()
        .transactions[0]
        .id;
    v.db()
        .unwrap()
        .execute(
            "INSERT OR REPLACE INTO loan_links(transaction_id, loan_account_id, source) VALUES (?1, ?2, 'manual')",
            [linked, loan],
        )
        .unwrap();
    // The app's loan view shows the Girokonto booking …
    let app = v
        .list_transactions(BookingFilter {
            account_ids: vec![loan],
            ..Default::default()
        })
        .unwrap();
    assert!(app.transactions.iter().any(|t| t.id == linked));
    // … the interface only the bookings on the loan account, also with both shared.
    share(&v, &[giro, loan], |_| {});
    let rows = all_pages(&v, json!({ "account_ids": [loan] }), 50);
    assert!(rows.iter().all(|t| t["account_id"] == loan), "{rows:?}");
    share(&v, &[loan], |_| {});
    let (from, to) = summary_range();
    let rows = all_pages(&v, json!({ "date_from": from, "date_to": to }), 50);
    assert!(rows.iter().all(|t| t["account_id"] == loan), "{rows:?}");
    let summary = ok(&v, "summary", json!({ "date_from": from, "date_to": to }));
    let count: u64 = summary["totals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["booking_count"].as_u64().unwrap())
        .sum();
    assert_eq!(count as usize, rows.len());
}

#[test]
fn keyset_pages_match_the_booking_list() {
    let (_d, v) = demo();
    let giro = account_id(&v, "Musterbank Girokonto");
    let card = account_id(&v, "Visa Gold Kreditkarte");
    share(&v, &[giro, card], |_| {});
    let expected: Vec<i64> = v
        .list_transactions(BookingFilter {
            account_ids: vec![giro, card],
            direction: BookingDirection::Expense,
            ..Default::default()
        })
        .unwrap()
        .transactions
        .iter()
        .map(|t| t.id)
        .collect();
    assert!(expected.len() > 20);
    let got: Vec<i64> = all_pages(&v, json!({ "direction": "expense" }), 7)
        .iter()
        .map(|t| t["id"].as_i64().unwrap())
        .collect();
    assert_eq!(got, expected);
}

#[test]
fn filters_and_cursor_are_validated() {
    let (_d, v) = demo();
    share(&v, &[account_id(&v, "Musterbank Girokonto")], |_| {});
    assert_eq!(
        err_code(&v, "transactions", json!({ "date_from": "01.02.2026" })),
        "invalid_params"
    );
    assert_eq!(
        err_code(&v, "transactions", json!({ "cursor": "x" })),
        "invalid_params"
    );
    assert_eq!(
        err_code(&v, "transactions", json!({ "direction": "sideways" })),
        "invalid_params"
    );
    assert_eq!(err_code(&v, "summary", json!({})), "invalid_params");
    let page = ok(&v, "transactions", json!({ "limit": 100_000 }));
    assert!(page["transactions"].as_array().unwrap().len() <= MAX_PAGE as usize);
}

#[test]
fn search_cannot_probe_hidden_fields() {
    let (_d, v) = demo();
    let giro = account_id(&v, "Musterbank Girokonto");
    // "INGB" / "DE89" would match hidden IBANs, "Miete" hidden purposes.
    for hidden in [
        |s: &mut ShareScope| s.share_account_numbers = false,
        |s: &mut ShareScope| s.share_purpose = false,
        |s: &mut ShareScope| s.share_counterparty = false,
    ] {
        share(&v, &[giro], |s| {
            s.share_account_numbers = true;
            hidden(s);
        });
        for q in ["Miete", "INGB", "DE89"] {
            assert_eq!(
                err_code(&v, "transactions", json!({ "search": q })),
                "not_shared"
            );
        }
    }
    share(&v, &[giro], |s| s.share_account_numbers = true);
    ok(&v, "transactions", json!({ "search": "DE89" }));
    ok(&v, "transactions", json!({ "search": "Miete" }));
}

#[test]
fn summary_matches_the_app_tiles() {
    let (_d, v) = demo();
    let giro = account_id(&v, "Musterbank Girokonto");
    // Everything visible, as in the app (the demo pays its loan from the Girokonto).
    let all: Vec<i64> = v.list_accounts().unwrap().iter().map(|a| a.id).collect();
    share(&v, &all, |_| {});
    let (from, to) = summary_range();
    let page = v
        .list_transactions(BookingFilter {
            account_ids: vec![giro],
            date_from: Some(from.clone()),
            date_to: Some(to.clone()),
            ..Default::default()
        })
        .unwrap();
    let s = ok(
        &v,
        "summary",
        json!({ "account_ids": [giro], "date_from": from, "date_to": to }),
    );
    let totals = s["totals"].as_array().unwrap();
    assert_eq!(totals.len(), 1, "demo Girokonto is EUR only");
    assert_eq!(totals[0]["currency"], "EUR");
    assert_eq!(totals[0]["income_cents"], page.income_cents);
    assert_eq!(totals[0]["expense_cents"], -page.expense_cents);
    assert_eq!(totals[0]["booking_count"], page.total_count);
    let net: i64 = s["by_category"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["net_cents"].as_i64().unwrap())
        .sum();
    let all: i64 = page.transactions.iter().map(|t| t.amount_cents).sum();
    assert_eq!(net, all, "split parts add up to the bookings");
}

#[test]
fn categories_are_the_tree() {
    let (_d, v) = demo();
    share(&v, &[account_id(&v, "Musterbank Girokonto")], |_| {});
    let cats = ok(&v, "categories", Value::Null);
    let cats = cats.as_array().unwrap();
    assert!(!cats.is_empty());
    let ids: HashSet<i64> = cats.iter().map(|c| c["id"].as_i64().unwrap()).collect();
    for c in cats {
        if let Some(p) = c["parent_id"].as_i64() {
            assert!(ids.contains(&p));
            assert!(c["depth"].as_u64().unwrap() >= 1);
        }
    }
}

#[test]
fn long_numbers_and_payment_method_follow_the_scope() {
    let (_d, v) = demo();
    let giro = account_id(&v, "Musterbank Girokonto");
    let booking = v
        .list_transactions(BookingFilter {
            account_ids: vec![giro],
            limit: 1,
            ..Default::default()
        })
        .unwrap()
        .transactions[0]
        .id;
    v.db()
        .unwrap()
        .execute(
            "UPDATE transactions SET purpose = 'Kto 0532013000 BLZ 3704 0044 Karte 4111 1111 Rate 12,50 am 15.11.' WHERE id = ?1",
            [booking],
        )
        .unwrap();
    share(&v, &[giro], |_| {});
    let rows = all_pages(&v, json!({}), 100);
    let row = rows.iter().find(|t| t["id"] == booking).unwrap();
    assert_eq!(
        row["purpose"],
        "Kto [number] BLZ [number] Karte [number] Rate 12,50 am 15.11."
    );
    assert!(rows.iter().any(|t| t["payment_method"] != ""));
    share(&v, &[giro], |s| s.share_purpose = false);
    let rows = all_pages(&v, json!({}), 100);
    assert!(rows.iter().all(|t| t["payment_method"] == ""));
}

#[test]
fn balances_match_the_sidebar() {
    let (_d, v) = demo();
    let all: Vec<_> = v.list_accounts().unwrap();
    share(&v, &all.iter().map(|a| a.id).collect::<Vec<_>>(), |_| {});
    let api = ok(&v, "accounts", Value::Null);
    for a in &all {
        let row = api
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == a.id)
            .unwrap();
        assert_eq!(row["balance_cents"], a.balance_cents, "{}", a.name);
    }
}

#[test]
fn summary_counts_deleted_categories_as_none_and_is_bounded() {
    let (_d, v) = demo();
    let giro = account_id(&v, "Musterbank Girokonto");
    share(&v, &[giro], |_| {});
    let (from, to) = summary_range();
    let s = ok(&v, "summary", json!({ "date_from": from, "date_to": to }));
    let used = s["by_category"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|c| c["category_id"].as_i64())
        .unwrap();
    v.db()
        .unwrap()
        .execute("UPDATE categories SET deleted_ms = 1 WHERE id = ?1", [used])
        .unwrap();
    let s = ok(&v, "summary", json!({ "date_from": from, "date_to": to }));
    let cats = s["by_category"].as_array().unwrap();
    assert!(cats.iter().all(|c| c["category_id"] != used));
    let listed: HashSet<i64> = ok(&v, "categories", Value::Null)
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_i64().unwrap())
        .collect();
    assert!(
        cats.iter()
            .filter_map(|c| c["category_id"].as_i64())
            .all(|id| listed.contains(&id))
    );
    assert_eq!(
        err_code(
            &v,
            "summary",
            json!({ "date_from": "2020-01-01", "date_to": "2026-01-01" })
        ),
        "invalid_params"
    );
}

#[test]
fn unshared_loan_payments_read_as_transfers() {
    let (_d, v) = demo();
    let giro = account_id(&v, "Musterbank Girokonto");
    let loan = account_id(&v, "Immobilien-Darlehen");
    let linked = v
        .list_transactions(BookingFilter {
            account_ids: vec![giro],
            direction: BookingDirection::Expense,
            limit: 1,
            ..Default::default()
        })
        .unwrap()
        .transactions[0]
        .id;
    v.db()
        .unwrap()
        .execute(
            "INSERT OR REPLACE INTO loan_links(transaction_id, loan_account_id, source) VALUES (?1, ?2, 'manual')",
            [linked, loan],
        )
        .unwrap();
    let flow = |accounts: &[i64]| {
        share(&v, accounts, |_| {});
        all_pages(&v, json!({ "account_ids": [giro] }), 100)
            .into_iter()
            .find(|t| t["id"] == linked)
            .unwrap()["flow"]
            .clone()
    };
    assert_eq!(flow(&[giro]), "transfer");
    assert_eq!(flow(&[giro, loan]), "loan");
    // `summary` follows the same rule: the unshared loan's payment is neutral.
    let (from, to) = summary_range();
    let expense = |accounts: &[i64]| {
        share(&v, accounts, |_| {});
        ok(
            &v,
            "summary",
            json!({ "account_ids": [giro], "date_from": from, "date_to": to }),
        )["totals"][0]["expense_cents"]
            .as_i64()
            .unwrap()
    };
    let amount = v
        .list_transactions(BookingFilter {
            account_ids: vec![giro],
            ..Default::default()
        })
        .unwrap()
        .transactions
        .into_iter()
        .find(|t| t.id == linked)
        .unwrap()
        .amount_cents;
    assert_eq!(expense(&[giro, loan]) - expense(&[giro]), -amount);
}

#[test]
fn cursor_carries_the_stored_date() {
    assert_eq!(
        parse_cursor("2024-01-01T00:00:00:123").unwrap(),
        ("2024-01-01T00:00:00".to_string(), 123)
    );
    assert!(parse_cursor(":5").is_err());
    assert!(parse_cursor("2024-01-01:x").is_err());
}

#[test]
fn dates_are_zero_padded_for_the_text_compare() {
    assert_eq!(
        check_date(Some("2025-1-5".into()), "d").unwrap().as_deref(),
        Some("2025-01-05")
    );
    assert!(check_date(Some("25-01-01".into()), "d").is_err());
    assert!(check_date(Some("2025-02-30".into()), "d").is_err());
    assert_eq!(check_date(Some(String::new()), "d").unwrap(), None);
}

#[test]
fn grouped_numbers_are_masked_but_dates_stay() {
    let scope = ShareScope::default();
    let masked = |t: &str| shared_text(&scope, true, t).unwrap();
    assert_eq!(
        masked("Karte 4111-1111-1111-1111 Vertrag 1234/5678/90 BLZ 370.400.44"),
        "Karte [number] Vertrag [number] BLZ [number]"
    );
    assert_eq!(masked("Kd-Nr 1234\u{00A0}5678"), "Kd-Nr [number]");
    assert_eq!(
        masked("Rechnung vom 15.11.2026, faellig 2026-12-01, 1.234,56 EUR"),
        "Rechnung vom 15.11.2026, faellig 2026-12-01, 1.234,56 EUR"
    );
    let open = ShareScope {
        share_account_numbers: true,
        ..Default::default()
    };
    assert_eq!(
        shared_text(&open, true, "4111-1111-1111-1111").unwrap(),
        "4111-1111-1111-1111"
    );
}

#[test]
fn depth_follows_the_parent_chain() {
    let (_d, v) = demo();
    share(&v, &[account_id(&v, "Musterbank Girokonto")], |_| {});
    v.db()
        .unwrap()
        .execute(
            "UPDATE categories SET name = 'Haus › Garten' WHERE parent_id IS NULL AND id = (SELECT MIN(id) FROM categories WHERE parent_id IS NULL AND deleted_ms IS NULL)",
            [],
        )
        .unwrap();
    let cats = ok(&v, "categories", Value::Null);
    let cats = cats.as_array().unwrap();
    let top = cats.iter().find(|c| c["name"] == "Haus › Garten").unwrap();
    assert_eq!(top["depth"], 0);
    let by_id: HashMap<i64, &Value> = cats
        .iter()
        .map(|c| (c["id"].as_i64().unwrap(), c))
        .collect();
    for c in cats {
        let expected = match c["parent_id"].as_i64() {
            None => 0,
            Some(p) => by_id[&p]["depth"].as_u64().unwrap() + 1,
        };
        assert_eq!(c["depth"].as_u64().unwrap(), expected);
    }
}

#[test]
fn six_digit_numbers_and_date_shaped_numbers_are_masked() {
    let scope = ShareScope::default();
    let masked = |t: &str| shared_text(&scope, true, t).unwrap();
    assert_eq!(
        masked("Kto 1234567 Kundennr. 654321 Ref 1234-56-78 Nr 12.34.5678"),
        "Kto [number] Kundennr. [number] Ref [number] Nr [number]"
    );
    assert_eq!(
        masked("vom 15.11.26 bis 31.12.2026, Rate 12345,67"),
        "vom 15.11.26 bis 31.12.2026, Rate 12345,67"
    );
}

#[test]
fn account_names_hide_numbers_and_info_stays_light() {
    let (_d, v) = demo();
    let giro = account_id(&v, "Musterbank Girokonto");
    v.rename_account(giro, "Giro 0532013000".into()).unwrap();
    share(&v, &[giro], |_| {});
    assert_eq!(ok(&v, "accounts", Value::Null)[0]["name"], "Giro [number]");
    let info = ok(&v, "info", Value::Null);
    assert_eq!(info["account_count"], 1);
    share(&v, &[giro], |s| s.share_account_numbers = true);
    assert_eq!(
        ok(&v, "accounts", Value::Null)[0]["name"],
        "Giro 0532013000"
    );
}

#[test]
fn children_of_deleted_categories_read_as_top_level() {
    let (_d, v) = demo();
    share(&v, &[account_id(&v, "Musterbank Girokonto")], |_| {});
    let parent: i64 = v
        .db()
        .unwrap()
        .query_row(
            "SELECT parent_id FROM categories WHERE parent_id IS NOT NULL AND deleted_ms IS NULL LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    v.db()
        .unwrap()
        .execute(
            "UPDATE categories SET deleted_ms = 1 WHERE id = ?1",
            [parent],
        )
        .unwrap();
    let cats = ok(&v, "categories", Value::Null);
    let cats = cats.as_array().unwrap();
    let ids: HashSet<i64> = cats.iter().map(|c| c["id"].as_i64().unwrap()).collect();
    assert!(!ids.contains(&parent));
    for c in cats {
        match c["parent_id"].as_i64() {
            Some(p) => assert!(ids.contains(&p), "dangling parent {p}"),
            None => assert_eq!(c["depth"], 0),
        }
    }
}

#[test]
fn a_deleted_account_drops_out_of_the_scope() {
    let (_d, v) = demo();
    let ids: Vec<i64> = v.list_accounts().unwrap().iter().map(|a| a.id).collect();
    share(&v, &ids, |_| {});
    v.set_account_archived(ids[0], true).unwrap();
    v.delete_account(ids[0]).unwrap();
    assert_eq!(v.local_api_scope().unwrap().account_ids, ids[1..]);
    // A save from an older copy cannot bring it back either.
    share(&v, &ids, |_| {});
    assert_eq!(v.local_api_scope().unwrap().account_ids, ids[1..]);
}

#[test]
fn merchant_names_are_masked_like_the_texts_they_come_from() {
    let (_d, v) = demo();
    let ids: Vec<i64> = v.list_accounts().unwrap().iter().map(|a| a.id).collect();
    share(&v, &ids, |_| {});
    let long = regex::Regex::new(r"\d{6,}").unwrap();
    let merchants = |v: &Vault| -> Vec<String> {
        let mut out = Vec::new();
        let mut cursor = Value::Null;
        loop {
            let r = call(v, "transactions", json!({ "limit": 500, "cursor": cursor }));
            for t in r["result"]["transactions"].as_array().unwrap() {
                out.push(t["merchant"].as_str().unwrap_or_default().to_string());
            }
            cursor = r["result"]["next_cursor"].clone();
            if cursor.is_null() {
                return out;
            }
        }
    };
    let masked = merchants(&v);
    assert!(
        masked.iter().any(|m| m.contains("[number]")),
        "the demo data has a merchant with an account number"
    );
    assert!(masked.iter().all(|m| !long.is_match(m)), "{masked:?}");
    share(&v, &ids, |s| s.share_account_numbers = true);
    assert!(
        merchants(&v).iter().any(|m| long.is_match(m)),
        "shown when shared"
    );
}
