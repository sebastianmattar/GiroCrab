//! Invented answers in the shape of the myBHW API; no real data.

use super::*;
use serde_json::json;

#[test]
fn reads_the_login_data() {
    let who = identity("12 3456 7890", " Erika ; 31.12.1970 ; 12345 ").unwrap();
    assert_eq!(who.id_type, "ContractId");
    assert_eq!(who.id, "1234567890");
    assert_eq!(who.first_name, "Erika");
    assert_eq!(who.birth_date, "1970-12-31");
    assert_eq!(who.zip, "12345");

    let who = identity("12345678901", "Erika;1.2.1970;12345").unwrap();
    assert_eq!(who.id_type, "BhwId");
    assert_eq!(who.birth_date, "1970-02-01");
    assert_eq!(
        identity("1234567890", "Erika;1970-02-01;12345")
            .unwrap()
            .birth_date,
        "1970-02-01"
    );
}

#[test]
fn incomplete_login_data_is_refused_before_anything_is_sent() {
    let auth = |r: CoreResult<Identity>| matches!(r, Err(CoreError::Authentication { .. }));
    assert!(auth(identity("123456789", "Erika;31.12.1970;12345")));
    assert!(auth(identity("", "Erika;31.12.1970;12345")));
    assert!(auth(identity("1234567890", "")));
    assert!(auth(identity("1234567890", "Erika;31.12.1970")));
    assert!(auth(identity("1234567890", "Erika;31.12.1970;12345;x")));
    assert!(auth(identity("1234567890", ";31.12.1970;12345")));
    assert!(auth(identity("1234567890", "Erika;31.12.70;12345")));
    assert!(auth(identity("1234567890", "Erika;31.02.1970;12345")));
    assert!(auth(identity("1234567890", "Erika;31.12.1970;")));
    // ten characters, but no number
    assert!(auth(identity("ABCDEFGHIJ", "Erika;31.12.1970;12345")));
    assert!(auth(identity("12345-67890", "Erika;31.12.1970;12345")));
}

#[test]
fn the_code_goes_by_email_unless_blocked() {
    let permits = json!({ "permitV2": { "accepted": true } });
    let login = |email: Value, phone: Value| json!({ "data": "opaque", "permits": permits, "email": email, "phone": phone });
    assert_eq!(
        code_method(&login(json!("e***@example.org"), json!("+49 *** 12"))).unwrap(),
        CodeMethod::Email
    );
    assert_eq!(
        code_method(&login(json!({ "isBlocked": true }), json!("+49 *** 12"))).unwrap(),
        CodeMethod::Phone
    );
    assert_eq!(
        code_method(&login(Value::Null, json!({ "isBlocked": false }))).unwrap(),
        CodeMethod::Phone
    );
    assert!(code_method(&login(Value::Null, Value::Null)).is_err());
    // Terms not accepted in the browser yet.
    let no_permits = json!({ "data": "opaque", "email": "e***@example.org" });
    let err = code_method(&no_permits).unwrap_err().to_string();
    assert!(err.contains("Nutzungsbedingungen"), "{err}");
}

#[test]
fn only_the_bhw_host_is_talked_to() {
    let ok = |u: &str| ensure_host(&url::Url::parse(u).unwrap()).is_ok();
    assert!(ok("https://opra.bhw.de/api/bhw/login"));
    assert!(!ok("http://opra.bhw.de/api/bhw/login"));
    assert!(!ok("https://opra.bhw.de.example.org/api/bhw/login"));
    assert!(!ok("https://example.org/opra.bhw.de"));
    assert_eq!(
        BhwConnector::url("contracts/12%2F3/transactions")
            .unwrap()
            .as_str(),
        "https://opra.bhw.de/api/bhw/contracts/12%2F3/transactions"
    );
    // A path can never leave the API host.
    assert!(BhwConnector::url("https://example.org/x").is_err());
    assert!(BhwConnector::url("//example.org/x").is_err());
}

#[test]
fn a_refused_request_names_its_step_and_a_clean_code() {
    let body = json!({ "error": "BHW-CSP-ELN-003 Erika Mustermann <script>" });
    assert_eq!(bank_error_code(&body), "BHW-CSP-ELN-003");
    assert_eq!(bank_error_code(&json!({ "error": "<b>X1</b>" })), "bX1b");
    let err = rejected(reqwest::StatusCode::BAD_GATEWAY, "contracts", &json!({}));
    let text = err.to_string();
    assert!(
        text.contains("502") && text.contains("(contracts)"),
        "{text}"
    );
    assert_eq!(
        bank_error_code(&json!({ "error": "x".repeat(100) })).len(),
        40
    );
}

#[test]
fn amounts_and_dates() {
    assert_eq!(cents(&json!(1234.5)), Some(123_450));
    assert_eq!(cents(&json!(-80)), Some(-8_000));
    assert_eq!(cents(&json!("1.234,56")), Some(123_456));
    assert_eq!(cents(&json!("-7,50 €")), Some(-750));
    assert_eq!(cents(&json!("12,00 EUR")), Some(1_200));
    assert_eq!(cents(&json!("\u{2212}3,10")), Some(-310));
    assert_eq!(cents(&json!("abc")), None);
    assert_eq!(cents(&Value::Null), None);
    assert_eq!(iso_date("05.11.2026").as_deref(), Some("2026-11-05"));
    assert_eq!(iso_date("5.1.2026 10:30").as_deref(), Some("2026-01-05"));
    assert_eq!(
        iso_date("2026-11-05T08:15:00Z").as_deref(),
        Some("2026-11-05")
    );
    assert_eq!(iso_date("05.11.26"), None);
    assert_eq!(iso_date(""), None);
}

fn contracts_json() -> Value {
    json!({ "contracts": [
        { "id": "1000000001", "productLongName": "BHW Bausparvertrag Dispo maXX",
          "accountBalance": "12.345,67", "currency": "EUR" },
        { "contractId": 1000000002, "productLongName": "BHW Wohnbau-Darlehen",
          "accountBalance": 50000.0 },
        { "id": "1000000003", "productLongName": "BHW Vorfinanzierung",
          "accountBalance": -1500 },
        { "id": "1000000004", "productLongName": "Bausparvertrag ohne Saldo" },
        { "id": "1000000001", "productLongName": "doppelt", "accountBalance": 1 },
        { "productLongName": "ohne Nummer", "accountBalance": 1 }
    ]})
}

#[test]
fn reads_savings_contracts_and_loans() {
    let c = parse_contracts(&contracts_json()).unwrap();
    assert_eq!(c.len(), 4);
    assert_eq!(c[0].id, "1000000001");
    assert_eq!(c[0].kind, AccountKind::Savings);
    assert_eq!(c[0].balance_cents, Some(1_234_567));
    // A loan is debt, whichever sign the BHW uses.
    assert_eq!(c[1].id, "1000000002");
    assert_eq!(c[1].kind, AccountKind::Loan);
    assert_eq!(c[1].balance_cents, Some(-5_000_000));
    assert_eq!(c[2].kind, AccountKind::Loan);
    assert_eq!(c[2].balance_cents, Some(-150_000));
    assert_eq!(c[3].balance_cents, None);
    assert_eq!(c[3].currency, "EUR");

    let a = remote_account(&c[0], "2026-11-20");
    assert_eq!(a.external_ref, "bhw-1000000001");
    assert_eq!(a.name, "BHW Bausparvertrag Dispo maXX");
    assert_eq!(
        remote_account(&c[3], "2026-11-20").name,
        "BHW Bausparvertrag ohne Saldo"
    );
    assert_eq!(a.account_number, "1000000001");

    // A bare list is read the same way; no contract at all is no answer.
    let bare = json!([{ "id": "1", "productLongName": "Bausparvertrag", "accountBalance": 0 }]);
    assert_eq!(parse_contracts(&bare).unwrap().len(), 1);
    assert!(parse_contracts(&json!({ "contracts": [] })).is_err());
    assert!(parse_contracts(&json!({ "error": "x" })).is_err());
    assert!(
        parse_contracts(&json!({ "contracts": [
            { "id": "1", "accountBalance": "kaputt" }
        ]}))
        .is_err()
    );
}

fn booking(date: &str, amount: Value, text: &str) -> Value {
    json!({ "postingDate": date, "valueDate": date, "text": text, "amount": amount,
            "currency": "EUR" })
}

#[test]
fn reads_bookings_from_the_start_date_on() {
    let contract = parse_contracts(&contracts_json()).unwrap().remove(0);
    let raw = json!({ "transactions": [
        booking("05.11.2026", json!("100,00"), "Sparrate"),
        booking("30.09.2026", json!(100), "Sparrate"),
        { "postingDate": "31.12.2025", "text": "Zinsen", "amount": "12,34" },
        booking("01.11.2026", json!("-5,00"), "Kontoführungsentgelt"),
    ]});
    let t = parse_transactions(&raw, &contract, "2025-12-31").unwrap();
    assert_eq!(t.len(), 4);
    assert_eq!(t[0].account_ref, "bhw-1000000001");
    assert_eq!(t[0].booking_date, "2026-11-05");
    assert_eq!(t[0].amount_cents, 10_000);
    assert_eq!(t[0].purpose, "Sparrate");
    assert!(t[0].external_id.is_empty());
    // no value date: the booking date
    assert_eq!(t[2].value_date, "2025-12-31");
    assert_eq!(t[2].currency, "EUR");
    assert_eq!(t[3].amount_cents, -500);

    let newer = parse_transactions(&raw, &contract, "2026-10-01").unwrap();
    assert_eq!(newer.len(), 2);
    // A booking without a date or amount is a broken answer.
    assert!(
        parse_transactions(
            &json!({ "transactions": [{ "text": "x", "amount": 1 }] }),
            &contract,
            "2026-01-01"
        )
        .is_err()
    );
    assert!(
        parse_transactions(
            &json!({ "transactions": [{ "postingDate": "01.01.2026" }] }),
            &contract,
            "2026-01-01"
        )
        .is_err()
    );
}

#[test]
fn reads_mailbox_messages() {
    let m = parse_messages(&json!({ "messages": [
        { "id": 42, "name": "Jahreskontoauszug 2025.pdf", "contractId": "1000000001",
          "creationDate": "2026-01-15T09:00:00" },
        { "id": "a/b", "name": "", "contractId": 1000000002, "creationDate": "16.01.2026" },
        { "name": "ohne id" }
    ]}))
    .unwrap();
    assert_eq!(m.len(), 2);
    assert_eq!(m[0].id, "42");
    assert_eq!(m[0].contract, "1000000001");
    assert_eq!(m[0].date, "2026-01-15");
    assert_eq!(message_title(&m[0]), "Jahreskontoauszug 2025");
    assert_eq!(document_type(&m[0]), "kontoauszug");
    assert_eq!(
        document_file_name(&m[0]),
        "2026-01-15_Jahreskontoauszug_2025_42.pdf"
    );
    assert_eq!(m[1].contract, "1000000002");
    assert_eq!(message_title(&m[1]), "myBHW Postfach");
    assert_eq!(document_type(&m[1]), "postbox");
    // no path separators in the file name
    assert!(!document_file_name(&m[1]).contains('/'));
    assert!(parse_messages(&json!({ "error": "x" })).is_err());
}

#[test]
fn sent_login_data_is_wiped() {
    let mut body = json!({ "id": "1234567890", "nested": { "zip": "12345" }, "n": 1,
                           "list": ["Erika"] });
    body.zeroize_strings();
    assert_eq!(
        body,
        json!({ "id": "", "nested": { "zip": "" }, "n": 1, "list": [""] })
    );
}

/// Acceptance "no duplicate bookings": a first sync and repeated syncs leave
/// every booking exactly once, also two identical real bookings.
#[test]
fn repeated_syncs_store_every_booking_once() {
    use crate::models::ConnectorKind;
    use crate::vault::Vault;

    let dir = crate::test_support::TempDir::new("girocrab-bhw");
    let path = dir.join("v.db");
    let v = Vault::open(path.to_string_lossy().to_string(), vec![7u8; 32]).unwrap();
    let conn = v
        .add_bank_connection(ConnectorKind::Bhw, String::new(), "1000000001".into())
        .unwrap();
    let contract = parse_contracts(&contracts_json()).unwrap().remove(0);
    let raw = json!({ "transactions": [
        booking("05.11.2026", json!("100,00"), "Sparrate"),
        booking("05.10.2026", json!("100,00"), "Sparrate"),
        booking("05.10.2026", json!("100,00"), "Sparrate"),
        booking("01.10.2026", json!("-5,00"), "Entgelt"),
    ]});
    let fetch = |since: &str| FetchResult {
        accounts: vec![remote_account(&contract, "2026-11-20")],
        transactions: parse_transactions(&raw, &contract, since).unwrap(),
        balances_reported: vec![account_ref(&contract)],
        accounts_complete: true,
        ..Default::default()
    };
    let rows = || -> Vec<(String, i64)> {
        let db = v.db().unwrap();
        let mut q = db
            .prepare(
                "SELECT booking_date, amount_cents FROM transactions
                  WHERE deleted_ms IS NULL ORDER BY booking_date, amount_cents",
            )
            .unwrap();
        q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    super::super::store_fetch(&v, ConnectorKind::Bhw, conn, fetch("2026-01-01")).unwrap();
    let first = rows();
    assert_eq!(
        first,
        vec![
            ("2026-10-01".to_string(), -500),
            ("2026-10-05".to_string(), 10_000),
            ("2026-10-05".to_string(), 10_000),
            ("2026-11-05".to_string(), 10_000),
        ]
    );
    for since in ["2026-01-01", "2026-10-01", "2026-10-05"] {
        super::super::store_fetch(&v, ConnectorKind::Bhw, conn, fetch(since)).unwrap();
        assert_eq!(rows(), first, "since {since}");
    }
    let balance: i64 = v
        .db()
        .unwrap()
        .query_row(
            "SELECT balance_cents FROM accounts WHERE deleted_ms IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(balance, 1_234_567);
    drop(v);
    std::fs::remove_file(path).ok();
}

#[test]
fn the_bank_search_finds_the_bhw() {
    use crate::connectors::bank_directory::search;
    use crate::models::ConnectorKind;
    for q in ["BHW", "mybhw", "bhw bauspar", "25410200"] {
        let r = search(q, vec![], 10);
        assert!(
            r.iter().any(|b| b.connector == Some(ConnectorKind::Bhw)),
            "{q}"
        );
    }
    // A search for savings banks does not list the building society.
    assert!(
        search("sparkasse", vec![], 10)
            .iter()
            .all(|b| b.connector != Some(ConnectorKind::Bhw))
    );
}

#[test]
fn mailbox_messages_find_their_contract() {
    let contracts = parse_contracts(&json!({ "contracts": [
        { "id": "a1b2", "contractId": "0012345678", "productLongName": "Bausparvertrag",
          "accountBalance": 1 },
        { "id": "2000000002", "productLongName": "Wohnbau-Darlehen", "accountBalance": -1 }
    ]}))
    .unwrap();
    let msg = |contract: &str| Message {
        id: "1".into(),
        contract: contract.into(),
        name: "Kontoauszug".into(),
        date: "2026-01-15".into(),
    };
    // by the contract number in any spelling, or by the API's own id
    assert_eq!(
        message_account(&msg("12 345 678"), &contracts, &[]).as_deref(),
        Some("bhw-a1b2")
    );
    assert_eq!(
        message_account(&msg("0012345678"), &contracts, &[]).as_deref(),
        Some("bhw-a1b2")
    );
    assert_eq!(
        message_account(&msg("A1B2"), &contracts, &[]).as_deref(),
        Some("bhw-a1b2")
    );
    assert_eq!(
        message_account(&msg("2000000002"), &contracts, &[]).as_deref(),
        Some("bhw-2000000002")
    );
    // unknown or missing contract: only clear with a single contract
    assert_eq!(message_account(&msg(""), &contracts, &[]), None);
    assert_eq!(message_account(&msg("999"), &contracts, &[]), None);
    assert_eq!(
        message_account(&msg(""), &contracts[..1], &[]).as_deref(),
        Some("bhw-a1b2")
    );
    assert_eq!(contract_key("0000"), "0");
}

#[test]
fn the_debug_shape_carries_no_values() {
    let v = json!({ "messages": [{ "id": 42, "name": "Erika Mustermann", "contractId": "1234567890",
                                   "isRead": false, "x": null }] });
    let s = shape(&v);
    assert!(s.starts_with("{messages:[1x {"), "{s}");
    for part in [
        "id:num",
        "name:str",
        "contractId:str",
        "isRead:bool",
        "x:null",
    ] {
        assert!(s.contains(part), "{s}");
    }
    assert!(!s.contains("Erika") && !s.contains("1234567890") && !s.contains("42"));
    assert_eq!(shape(&json!([])), "[]");
}

#[test]
fn the_key_relation_log_carries_no_numbers() {
    let contracts = parse_contracts(&json!({ "contracts": [
        { "id": "1234567890", "accountBalance": 1 }
    ]}))
    .unwrap();
    let m = Message {
        id: "1".into(),
        contract: "4567890".into(),
        name: String::new(),
        date: "2026-01-01".into(),
    };
    let r = key_relation(&m, &contracts);
    assert_eq!(
        r,
        "len=7 keys=[10] same_stem=false key_starts=false key_ends=true in_key=true key_in=false"
    );
    assert!(!r.contains("4567890") && !r.contains("1234567890"));
}

#[test]
fn a_letter_names_its_contract_in_its_text() {
    let contracts = parse_contracts(&json!({ "contracts": [
        { "id": "1000000001", "productLongName": "Bausparvertrag", "accountBalance": 1 },
        { "id": "2000000002", "productLongName": "Darlehen", "accountBalance": -1 }
    ]}))
    .unwrap();
    let find = |t: &str| contract_in_text(t, &contracts, &[]);
    // the number after the label wins, also when both numbers occur
    assert_eq!(
        find("Jahreskontoauszug\nVertrags-Nr.: 10 0000 0001\nVerrechnet mit 2000000002"),
        (Some("bhw-1000000001".into()), TextMatch::Label)
    );
    assert_eq!(find("Kundennummer: 9100000001"), (None, TextMatch::None));
    assert_eq!(
        find("Ihre Vertragsnummer 2000000002 / Kundennr. 555"),
        (Some("bhw-2000000002".into()), TextMatch::Label)
    );
    // no label: one number alone is clear, two are not
    assert_eq!(
        find("Konto 1000000001 Saldo"),
        (Some("bhw-1000000001".into()), TextMatch::Unique)
    );
    assert_eq!(
        find("1000000001 und 2000000002"),
        (None, TextMatch::Ambiguous)
    );
    assert_eq!(find("nichts davon 3000000003"), (None, TextMatch::None));
    assert_eq!(find(""), (None, TextMatch::None));
    // short ids never match by text (too likely by chance)
    let short = parse_contracts(&json!([{ "id": "42", "accountBalance": 1 }])).unwrap();
    assert_eq!(
        contract_in_text("Vertrag 42", &short, &[]).1,
        TextMatch::None
    );
}

#[test]
fn contract_details_add_their_numbers_as_keys() {
    let mut keys = vec!["1000000001".to_string()];
    numbers_in(
        &json!({ "savingData": { "contractNumber": "30 0000 0003", "status": "aktiv",
                 "zip": "12345", "balance": 12761.1, "sum": 50000,
                 "date": "2026-01-01", "old": [4000000004u64] },
                 "iban": "DE89370400440532013000" }),
        0,
        &mut keys,
    );
    keys.sort();
    assert_eq!(keys, vec!["1000000001", "3000000003", "4000000004"]);
    assert_eq!(digit_runs("a 1234567890 b 123 c 12345678901", 10), 1);
}

/// A mailbox PDF stored without an account moves to the contract its text
/// names (the app passes the PDFKit text).
#[test]
fn unplaced_mailbox_pdfs_move_to_the_contract_their_text_names() {
    use crate::models::ConnectorKind;
    use crate::vault::Vault;

    let dir = crate::test_support::TempDir::new("girocrab-bhw-place");
    let path = dir.join("v.db");
    let v = Vault::open(path.to_string_lossy().to_string(), vec![7u8; 32]).unwrap();
    let conn = v
        .add_bank_connection(ConnectorKind::Bhw, String::new(), "1000000001".into())
        .unwrap();
    let contracts = parse_contracts(&json!({ "contracts": [
        { "id": "1000000001", "productLongName": "Bausparvertrag", "accountBalance": 1 },
        { "id": "2000000002", "productLongName": "Darlehen", "accountBalance": -1 }
    ]}))
    .unwrap();
    let pdf = |id: &str| RemoteDocument {
        account_ref: String::new(),
        booking_external_id: None,
        title: "Jahreskontoauszug".into(),
        doc_type: "kontoauszug".into(),
        mime_type: "application/pdf".into(),
        file_name: format!("{id}.pdf"),
        bytes: format!("%PDF-1.4 {id}").into_bytes(),
        source_url: format!("bhw:message:{id}"),
        date: "2026-01-15".into(),
    };
    super::super::store_fetch(
        &v,
        ConnectorKind::Bhw,
        conn,
        FetchResult {
            accounts: contracts
                .iter()
                .map(|c| remote_account(c, "2026-11-20"))
                .collect(),
            documents: vec![pdf("1"), pdf("2")],
            ..Default::default()
        },
    )
    .unwrap();
    let unplaced = v.bhw_unplaced_documents().unwrap();
    assert_eq!(unplaced.len(), 2);
    let loan: i64 = v
        .db()
        .unwrap()
        .query_row(
            "SELECT id FROM accounts WHERE external_ref = 'bhw-2000000002'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        v.place_bhw_document(unplaced[0], "Vertrags-Nr. 20 0000 0002".into())
            .unwrap(),
        Some(loan)
    );
    // text without a contract number: stays where it is
    assert_eq!(
        v.place_bhw_document(unplaced[1], "Kundeninformation".into())
            .unwrap(),
        None
    );
    assert_eq!(v.bhw_unplaced_documents().unwrap(), vec![unplaced[1]]);
    // an account the user gave it first stays
    let savings: i64 = v
        .db()
        .unwrap()
        .query_row(
            "SELECT id FROM accounts WHERE external_ref = 'bhw-1000000001'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    v.set_document_account(unplaced[1], Some(savings)).unwrap();
    assert_eq!(
        v.place_bhw_document(unplaced[1], "Vertrags-Nr. 20 0000 0002".into())
            .unwrap(),
        None
    );
    // a placed document is never moved again
    assert_eq!(
        v.place_bhw_document(unplaced[0], "Vertrags-Nr. 1000000001".into())
            .unwrap(),
        None
    );
    // an archived account gets no document placed
    super::super::store_fetch(
        &v,
        ConnectorKind::Bhw,
        conn,
        FetchResult {
            accounts: contracts
                .iter()
                .map(|c| remote_account(c, "2026-11-20"))
                .collect(),
            documents: vec![pdf("3")],
            ..Default::default()
        },
    )
    .unwrap();
    let third = *v.bhw_unplaced_documents().unwrap().last().unwrap();
    v.set_account_archived(loan, true).unwrap();
    assert_eq!(
        v.place_bhw_document(third, "Vertrags-Nr. 20 0000 0002".into())
            .unwrap(),
        None
    );
    assert!(v.bhw_unplaced_documents().unwrap().contains(&third));
    // the label names the archived loan: no other contract gets it
    assert_eq!(
        v.place_bhw_document(third, "Vertragsnummer 2000000002, Bezug 1000000001".into())
            .unwrap(),
        None
    );
    assert!(v.bhw_unplaced_documents().unwrap().contains(&third));
    drop(v);
    std::fs::remove_file(path).ok();
}

/// BHW numbers: an eight-digit contract stem and two digits for the part. The
/// letters name the contract as a whole ("…7 00"), the accounts its parts.
#[test]
fn letters_naming_the_whole_contract_reach_the_bausparvertrag() {
    use crate::models::ConnectorKind;
    use crate::vault::Vault;

    let dir = crate::test_support::TempDir::new("girocrab-bhw-whole");
    let path = dir.join("v.db");
    let v = Vault::open(path.to_string_lossy().to_string(), vec![7u8; 32]).unwrap();
    // the login is the number of the contract as a whole
    let conn = v
        .add_bank_connection(ConnectorKind::Bhw, String::new(), "1234567800".into())
        .unwrap();
    let contracts = parse_contracts(&json!({ "contracts": [
        { "id": "1234567801", "productLongName": "WohnBausparen", "accountBalance": 1 },
        { "id": "1234567851", "productLongName": "BauDarlehen", "accountBalance": -1 }
    ]}))
    .unwrap();
    let pdf = |id: &str, account_ref: &str, bytes: &str| RemoteDocument {
        account_ref: account_ref.into(),
        booking_external_id: None,
        title: "Brief".into(),
        doc_type: "kontoauszug".into(),
        mime_type: "application/pdf".into(),
        file_name: format!("{id}.pdf"),
        bytes: bytes.as_bytes().to_vec(),
        source_url: format!("bhw:message:{id}"),
        date: "2026-01-15".into(),
    };
    let store = |documents| {
        super::super::store_fetch(
            &v,
            ConnectorKind::Bhw,
            conn,
            FetchResult {
                accounts: contracts
                    .iter()
                    .map(|c| remote_account(c, "2026-11-20"))
                    .collect(),
                documents,
                ..Default::default()
            },
        )
        .unwrap()
    };
    store(vec![pdf("1", "", "%PDF-1.4 1"), pdf("2", "", "%PDF-1.4 2")]);
    let account = |r: &str| -> i64 {
        v.db()
            .unwrap()
            .query_row(
                "SELECT id FROM accounts WHERE external_ref = ?1",
                [r],
                |row| row.get(0),
            )
            .unwrap()
    };
    let unplaced = v.bhw_unplaced_documents().unwrap();
    assert_eq!(unplaced.len(), 2);
    // the letter's text: the stem has a loan and a Bausparvertrag
    assert_eq!(
        v.place_bhw_document(
            unplaced[0],
            "Vertragsnummer: 1 234 567 8 00 Anlage KAP".into()
        )
        .unwrap(),
        Some(account("bhw-1234567801"))
    );
    // a later fetch that matches the mailbox field sends the known
    // message without bytes: the stored copy gets its account
    store(vec![pdf("2", "bhw-1234567851", "")]);
    assert!(v.bhw_unplaced_documents().unwrap().is_empty());
    let placed: i64 = v
        .db()
        .unwrap()
        .query_row(
            "SELECT account_id FROM booking_documents WHERE source_url = 'bhw:message:2'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(placed, account("bhw-1234567851"));
    drop(v);
    std::fs::remove_file(path).ok();
}

#[test]
fn a_contract_number_reaches_its_part_by_the_stem() {
    let contracts = parse_contracts(&json!({ "contracts": [
        { "id": "1234567801", "productLongName": "WohnBausparen", "accountBalance": 1 },
        { "id": "1234567851", "productLongName": "BauDarlehen", "accountBalance": -1 },
        { "id": "9999999901", "productLongName": "Bausparvertrag", "accountBalance": 1 }
    ]}))
    .unwrap();
    let by = |n: &str, logins: &[String]| {
        contract_by_number(n, &contracts, logins).map(|c| c.id.clone())
    };
    let login = ["12 3456 7801".to_string()];
    // the exact number first
    assert_eq!(by("1234567851", &login).as_deref(), Some("1234567851"));
    // the whole contract: the stem has two parts, the login decides
    assert_eq!(by("1 234 567 8 00", &login).as_deref(), Some("1234567801"));
    let darlehen_login = ["1234567851".to_string()];
    assert_eq!(
        by("1234567800", &darlehen_login).as_deref(),
        Some("1234567851")
    );
    // a login naming no part (the whole contract): the Bausparvertrag
    let whole = ["1234567800".to_string()];
    assert_eq!(by("1234567800", &whole).as_deref(), Some("1234567801"));
    assert_eq!(by("1234567800", &[]).as_deref(), Some("1234567801"));
    // two parts that are no loan: no guess
    let two_savings = parse_contracts(&json!({ "contracts": [
        { "id": "1234567801", "productLongName": "WohnBausparen", "accountBalance": 1 },
        { "id": "1234567802", "productLongName": "Bausparvertrag", "accountBalance": 1 }
    ]}))
    .unwrap();
    assert_eq!(contract_by_number("1234567800", &two_savings, &whole), None);
    // a stem with one part needs no login
    assert_eq!(by("9999999900", &[]).as_deref(), Some("9999999901"));
    // another part's number is no number of the whole contract
    assert_eq!(by("1234567852", &login), None);
    assert_eq!(by("1234567852", &[]), None);
    // other stems and other lengths never match by the stem
    assert_eq!(by("5555555500", &login), None);
    assert_eq!(by("12345678", &login), None);

    // the same in a letter and in the mailbox field
    assert_eq!(
        contract_in_text(
            "Bitte angeben. Vertrags-/Kontonummer Diktatzeichen\n1 234 567 8 00 D83-51 DMO",
            &contracts,
            &login
        ),
        (Some("bhw-1234567801".into()), TextMatch::Label)
    );
    let m = Message {
        id: "1".into(),
        contract: "1234567800".into(),
        name: String::new(),
        date: "2026-01-01".into(),
    };
    assert_eq!(
        message_account(&m, &contracts, &login).as_deref(),
        Some("bhw-1234567801")
    );
    assert_eq!(
        message_account(&m, &contracts, &whole).as_deref(),
        Some("bhw-1234567801")
    );
}

#[test]
fn a_number_matches_only_as_a_number_of_its_own() {
    let contracts = parse_contracts(&json!({ "contracts": [
        { "id": "1234567801", "productLongName": "WohnBausparen", "accountBalance": 1 },
        { "id": "1234567851", "productLongName": "BauDarlehen", "accountBalance": -1 }
    ]}))
    .unwrap();
    let place = |t: &str| contract_in_text(t, &contracts, &[]);
    // part of a longer number, also a grouped one: no match
    assert_eq!(place("Kundennummer: 91234567801"), (None, TextMatch::None));
    assert_eq!(
        place("Kundennummer: 9 12 3456 7801"),
        (None, TextMatch::None)
    );
    assert_eq!(place("Referenz 12 3456 7801 5"), (None, TextMatch::None));
    // a label in a text whose lower case is longer than the text itself
    assert_eq!(
        place(&format!(
            "{} Vertragsnummer 1234567851, Bezug 1234567801",
            "İ".repeat(40)
        )),
        (Some("bhw-1234567851".into()), TextMatch::Label)
    );
    assert_eq!(place("Referenz 12345678011"), (None, TextMatch::None));
    // followed by a date or written in groups: a match
    assert_eq!(
        place("Vertragsnummer 1234567801 19.01.2024"),
        (Some("bhw-1234567801".into()), TextMatch::Label)
    );
    assert_eq!(
        place("Ihr Darlehen 12 3456 7851."),
        (Some("bhw-1234567851".into()), TextMatch::Unique)
    );
    assert_eq!(
        place("Vertrags-/Kontonummer 1 234 567 8 00 19.01.2024"),
        (Some("bhw-1234567801".into()), TextMatch::Label)
    );
}

#[test]
fn a_number_several_contracts_carry_is_no_match() {
    let mut contracts = parse_contracts(&json!({ "contracts": [
        { "id": "1000000001", "productLongName": "Bausparvertrag", "accountBalance": 1 },
        { "id": "2000000002", "productLongName": "Bausparvertrag", "accountBalance": 1 }
    ]}))
    .unwrap();
    // a detail value of the first contract equals the second's number
    contracts[0].keys.push(contract_key("2000000002"));
    assert_eq!(contract_by_number("2000000002", &contracts, &[]), None);
    assert_eq!(
        contract_by_number("1000000001", &contracts, &[]).map(|c| c.id.as_str()),
        Some("1000000001")
    );
}

#[test]
fn a_contract_row_without_id_keeps_the_list_incomplete() {
    let full = json!({ "contracts": [
        { "id": "1000000001", "productLongName": "Bausparvertrag", "accountBalance": 1 },
        { "contractId": "2000000002", "productLongName": "Darlehen", "accountBalance": -1 }
    ]});
    assert!(contracts_complete(&full));
    let partial = json!({ "contracts": [
        { "id": "1000000001", "productLongName": "Bausparvertrag", "accountBalance": 1 },
        { "productLongName": "Darlehen", "accountBalance": -1 }
    ]});
    assert!(!contracts_complete(&partial));
    assert_eq!(parse_contracts(&partial).unwrap().len(), 1);
}
