use super::*;
use serde_json::json;

fn fixture() -> Value {
    let account = |number: &str, internal: &str, cash: bool| {
        json!({
            "depotOrCashAccountId":{"depotId":{"id":internal},"subDepotId":if cash {json!({"depotId":{"id":internal},"subDepotNumber":"1"})}else{Value::Null}},
            "externalDepotNumber":{"externalDepotNumber":number},"externalDepotName":if cash {"Konto flex"}else{"Investmentdepot"},
            "closedFlag":false,"cashAccountContractTypeAndSubtype":if cash {json!("CURRENT_ACCOUNT")}else{Value::Null}
        })
    };
    let money = |value: &str, currency: &str| json!({"value":value,"currencyId":{"id":currency},"estimatedValueFlag":false});
    let mut groups = Vec::new();
    let mut details = Vec::new();
    for (owner, depot, cash, internal) in [
        ("Test Adult", "D1", "C1", "I1"),
        ("Test Child", "D2", "C2", "I2"),
    ] {
        let dep = account(depot, internal, false);
        let ca = account(cash, internal, true);
        groups.push(json!({"sortedDepotOwners":[{"displayName":owner}],
            "investmentDepots":[{"depotOrCashAccount":dep,"depotInventorySumAccountCurrency":money("1500.12","EUR")}],
            "managedDepots":[],"stockDepots":[],"preciousMetalDepots":[],
            "cashAccounts":[{"depotOrCashAccount":ca.clone(),"depotInventorySumAccountCurrency":money("12.34","EUR")}]}));
        let trade = |number: &str, amount: Value| {
            json!({"depotId":{"id":internal},
            "orderTransactionId":{"orderId":{"orderDate":"2025-01-01","orderNumber":number},"orderTransactionKindId":"1"},
            "bookingDate":"2025-01-02","fundPriceDate":"2025-01-03","transactionTypeName":"Kauf",
            "fund":{"isin":"DE0001234567","fundName":"Test Fund"},"paymentAmount":amount,"numberSign":"1"})
        };
        let mut neutral = trade("info", Value::Null);
        neutral["numberSign"] = json!("0");
        neutral["transactionTypeName"] = json!("Vorabpauschale");
        let mut cancel = trade("cancel", money("-10.00", "EUR"));
        cancel["isCancelationTransaction"] = json!(true);
        details.push(json!({"accountRef":depot,"kind":"investment","inventory":{
            "depot":dep,"investmentDepotInventories":[{"fund":{"isin":"DE0001234567","wkn":"123456","fundName":"Test Fund"},
                "pieces":{"value":"50.004","estimatedValueFlag":false},"inventory":money("1500.12","EUR"),
                "currentFundPrice":{"value":"60.00","currencyId":{"id":"USD"},"fundPriceDate":"2025-01-03","estimatedValueFlag":false},
                "currentExchangeRate":{"value":"2.00","baseCurrencyId":"EUR","targetCurrencyId":"USD"}}],
            "totalInventoryValueAccountCurrency":money("1500.12","EUR")},
            "transactions":[trade("one",money("123.45","EUR")),trade("two",money("1.23","USD")),neutral,cancel],
            "historicalTransactions":[{"migratedAabTransactionId":{"id":"old-1"},"depotId":{"id":internal},
                "bookingDate":"2018-01-01","priceDate":"2018-01-03","transactionTypeName":"Kauf",
                "shareName":"Test Fund","paymentAmount":money("5.00","EUR")}]}));
        details.push(json!({"accountRef":cash,"kind":"cash","cashAccountId":ca["depotOrCashAccountId"]["subDepotId"],
            "iban":{"iban":format!("DE00{cash}")},"transactions":[{
                "cashAccountId":ca["depotOrCashAccountId"]["subDepotId"],"orderOrTransactionId":{"orderId":null,
                    "transactionId":{"belegNummer":"B1","belegLfdNummer":"1","einzelgegenbuchung":""}},
                "bookingDate":"2025-01-02","valutaDate":"2025-01-03","amount":money("-23.40","EUR"),
                "transactionType":"Überweisung","reasonForPayment":"Test transfer",
                "countersideParticipant":{"accountHolder":"Test Recipient","iban":{"iban":"DE00TEST"}},
                "mandate":{"creditorIdentifier":"TESTCREDITOR","mandateReference":"TESTMANDATE"},"endToEndId":"TESTE2E"}]}));
    }
    json!({"version":1,"overview":{"ownerGroups":groups},"details":details,"totalsOnly":0})
}

#[test]
fn imports_both_owners_depots_and_cash_with_stable_ids_and_correct_currencies() {
    let r = snapshot(&fixture().to_string()).unwrap();
    assert_eq!(r.accounts.len(), 4);
    assert_eq!(r.transactions.len(), 12);
    assert_eq!(r.holdings.len(), 2);
    assert_eq!(r.holdings_fetched, vec!["D1", "D2"]);
    assert_eq!(r.accounts[2].owner, "Test Child");
    assert_eq!(r.accounts[1].iban, "DE00C1");
    assert_eq!(r.holdings[0].holding.quantity, 50.004);
    assert_eq!(r.holdings[0].holding.current_price_cents, 3000); // USD quote normalized using EUR/USD
    assert_eq!(r.holdings[0].holding.purchase_price_cents, 0); // unknown, not guessed from lifetime gains
    assert_eq!(r.transactions[1].currency, "USD");
    assert_eq!(r.transactions[2].amount_cents, 0);
    assert_eq!(r.transactions[3].amount_cents, -1000);
    assert!(r.transactions[3].booking_text.contains("Storno"));
    assert_eq!(r.transactions[0].value_date, ""); // pricing date is not a value date
    let cash = r
        .transactions
        .iter()
        .find(|t| t.account_ref == "C2")
        .unwrap();
    assert_eq!(cash.amount_cents, -2340);
    assert_eq!(cash.value_date, "2025-01-03");
    assert_eq!(cash.end_to_end_id, "TESTE2E");
    assert_eq!(cash.mandate_id, "TESTMANDATE");
    assert_eq!(cash.creditor_id, "TESTCREDITOR");
}

#[test]
fn rejects_incomplete_wrong_account_and_duplicate_transactions() {
    for (pointer,value) in [
        ("/details/0/transactions",Value::Null),
        ("/details/0/accountRef",json!("UNKNOWN")),
        ("/details/0/transactions/0/depotId/id",json!("OTHER")),
        ("/details/1/cashAccountId/subDepotNumber",json!("OTHER")),
        ("/details/1/transactions/0/orderOrTransactionId/transactionId/belegNummer",Value::Null),
        ("/details/0/inventory/investmentDepotInventories/0/currentExchangeRate/value",json!("0")),
        ("/details/0/inventory/investmentDepotInventories/0/currentExchangeRate/targetCurrencyId",json!("GBP")),
        ("/details/0/inventory/investmentDepotInventories/0/inventory/value",json!("999")),
        ("/details/0/transactions/0/paymentAmount",Value::Null),
        ("/details/0/historicalTransactions/0/paymentAmount",Value::Null),
    ] {
        let mut v=fixture();*v.pointer_mut(pointer).unwrap()=value;
        assert!(snapshot(&v.to_string()).is_err(),"{pointer}");
    }
    let mut v = fixture();
    let row = v["details"][0]["transactions"][0].clone();
    v["details"][0]["transactions"]
        .as_array_mut()
        .unwrap()
        .push(row);
    assert!(snapshot(&v.to_string()).is_err());
    let mut v = fixture();
    v["details"].as_array_mut().unwrap().pop();
    assert!(snapshot(&v.to_string()).is_err());
    // Missing account IDs on every side must not pass as null == null.
    let mut v = fixture();
    v["overview"]["ownerGroups"][0]["cashAccounts"][0]["depotOrCashAccount"]
        ["depotOrCashAccountId"]["subDepotId"] = Value::Null;
    v["details"][1]["cashAccountId"] = Value::Null;
    v["details"][1]["transactions"][0]["cashAccountId"] = Value::Null;
    assert!(snapshot(&v.to_string()).is_err());
}

#[test]
fn booked_order_replaces_its_pending_cash_row() {
    let dir = crate::test_support::TempDir::new("fnz-order");
    let vault = Vault::open(dir.join("vault.db").to_string_lossy().into(), vec![7; 32]).unwrap();
    let connection = vault
        .add_bank_connection(
            crate::models::ConnectorKind::Fnz,
            "Test FNZ".into(),
            "test".into(),
        )
        .unwrap();
    let live = || {
        vault
            .db()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM transactions t JOIN accounts a ON a.id = t.account_id
                  WHERE a.external_ref = 'C1' AND t.deleted_ms IS NULL",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
    };
    let mut ordered = fixture();
    ordered["details"][1]["transactions"][0]["orderOrTransactionId"] = json!({
        "orderId":{"orderDate":"2025-01-02","orderNumber":"O1"},"transactionId":null});
    vault
        .clone()
        .import_fnz_snapshot(connection, ordered.to_string())
        .unwrap();
    assert_eq!(live(), 1);
    let mut booked = ordered.clone();
    booked["details"][1]["transactions"][0]["orderOrTransactionId"]["transactionId"] =
        json!({"belegNummer":"B1","belegLfdNummer":"1","einzelgegenbuchung":""});
    vault
        .clone()
        .import_fnz_snapshot(connection, booked.to_string())
        .unwrap();
    assert_eq!(live(), 1);
}

#[test]
fn repeated_sync_is_idempotent_and_empty_inventory_clears_positions() {
    let dir = crate::test_support::TempDir::new("fnz-snapshot");
    let vault = Vault::open(dir.join("vault.db").to_string_lossy().into(), vec![7; 32]).unwrap();
    let connection = vault
        .add_bank_connection(
            crate::models::ConnectorKind::Fnz,
            "Test FNZ".into(),
            "test".into(),
        )
        .unwrap();
    let first = vault
        .clone()
        .import_fnz_snapshot(connection, fixture().to_string())
        .unwrap();
    assert_eq!(first.accounts_updated, 4);
    assert_eq!(first.transactions_imported, 12);
    assert_eq!(
        vault
            .clone()
            .import_fnz_snapshot(connection, fixture().to_string())
            .unwrap()
            .transactions_imported,
        0
    );
    let depot = vault
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|a| a.account_number == "D1")
        .unwrap();
    assert_eq!(vault.list_holdings(depot.id).unwrap().len(), 1);
    let mut broken = fixture();
    broken["details"][0]["transactions"] = Value::Null;
    assert!(vault
        .clone()
        .import_fnz_snapshot(connection, broken.to_string())
        .is_err());
    assert_eq!(vault.list_holdings(depot.id).unwrap().len(), 1);
    let mut empty = fixture();
    empty["overview"]["ownerGroups"][0]["investmentDepots"][0]
        ["depotInventorySumAccountCurrency"]["value"] = json!("0.00");
    empty["details"][0]["inventory"]["totalInventoryValueAccountCurrency"]["value"] = json!("0.00");
    empty["details"][0]["inventory"]["investmentDepotInventories"] = json!([]);
    vault
        .clone()
        .import_fnz_snapshot(connection, empty.to_string())
        .unwrap();
    assert!(vault.list_holdings(depot.id).unwrap().is_empty());
}
