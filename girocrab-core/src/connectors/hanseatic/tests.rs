//! Invented answers in the shape of the Hanseatic Bank API; no real data.

use super::*;
use serde_json::json;

fn jwt(claims: Value) -> String {
    let enc = |v: &Value| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(v).unwrap())
    };
    format!(
        "{}.{}.signature",
        enc(&json!({"alg": "none"})),
        enc(&claims)
    )
}

const SIGN_IN: &str = r#"<html><head></head><body><div id="__nuxt"></div>
<script>window.__NUXT__={};window.__NUXT__.config={public:{NORTHLAYER_API_KEY:"test-api-key",NORTHLAYER_CLIENT_KEY:"test-client-key",NORTHLAYER_CLIENT_SECRET:"test-client-secret",BASIC_AUTH:"Basic dGVzdDp0ZXN0",NORTHLAYER_BASE_URL:"https:\u002F\u002Fconnecthb.hanseaticbank.de",ENV:"prod"},app:{baseURL:"/"}}</script>
</body></html>"#;

#[test]
fn reads_the_public_web_app_config() {
    let c = parse_config(SIGN_IN).unwrap();
    assert_eq!(c.client_key.expose(), "test-client-key");
    assert_eq!(c.client_secret.expose(), "test-client-secret");
    assert_eq!(c.basic_auth.expose(), "Basic dGVzdDp0ZXN0");
    assert_eq!(c.base, "https://connecthb.hanseaticbank.de");
}

#[test]
fn the_password_never_goes_to_another_host() {
    for base in [
        "https:\\u002F\\u002Fconnecthb.example.org",
        "http:\\u002F\\u002Fconnecthb.hanseaticbank.de",
        "https:\\u002F\\u002Fconnecthb.hanseaticbank.de.example.org",
    ] {
        let html = SIGN_IN.replace("https:\\u002F\\u002Fconnecthb.hanseaticbank.de", base);
        assert!(parse_config(&html).is_err(), "{base}");
    }
    assert!(parse_config("<html>no config</html>").is_err());
    let no_basic = SIGN_IN.replace("Basic dGVzdDp0ZXN0", "");
    assert!(parse_config(&no_basic).is_err());

    for ok in [
        "https://meine.hanseaticbank.de/de/register/sign-in",
        "https://connecthb.hanseaticbank.de/token",
    ] {
        assert!(ensure_host(&url::Url::parse(ok).unwrap()).is_ok(), "{ok}");
    }
    for bad in [
        "http://connecthb.hanseaticbank.de/token",
        "https://hanseaticbank.de.example.org/token",
        "https://example.org/token",
    ] {
        assert!(
            ensure_host(&url::Url::parse(bad).unwrap()).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn routes_the_login_by_the_token_answer() {
    assert_eq!(
        login_of(&json!({"access_token": "invented-access", "expires_in": 3600})).unwrap(),
        Login::Token("invented-access".into())
    );
    let app = json!({"id_token": jwt(json!({"sub": "1000000001", "sca_id": "sca-test-1", "sca_type": "APP"}))});
    assert_eq!(
        login_of(&app).unwrap(),
        Login::Sca(Sca {
            id: "sca-test-1".into(),
            kind: ScaKind::App
        })
    );
    let sms = json!({"id_token": jwt(json!({"sca_id": "sca-test-2", "sca_type": "SMS"}))});
    assert_eq!(
        login_of(&sms).unwrap(),
        Login::Sca(Sca {
            id: "sca-test-2".into(),
            kind: ScaKind::Sms
        })
    );
    for bad in [
        json!({}),
        json!({"id_token": "not-a-jwt"}),
        json!({"id_token": jwt(json!({"sca_id": "x", "sca_type": "LETTER"}))}),
        json!({"id_token": jwt(json!({"sca_type": "APP"}))}),
    ] {
        assert!(login_of(&bad).is_err(), "{bad}");
    }
}

#[test]
fn token_errors_name_the_cause() {
    let bad = reqwest::StatusCode::BAD_REQUEST;
    let msg = |e: CoreError| e.to_string();
    assert_eq!(
        msg(token_error(
            bad,
            &json!({"error_code": "HBAUTH401"}),
            "login"
        )),
        msg(wrong_credentials())
    );
    assert_eq!(
        msg(token_error(
            bad,
            &json!({"error_code": "HBAUTH423"}),
            "login"
        )),
        msg(locked())
    );
    assert_eq!(
        msg(token_error(
            bad,
            &json!({"error_code": "HBSCA422"}),
            "login"
        )),
        msg(wrong_tan())
    );
    assert!(matches!(
        token_error(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            &Value::Null,
            "login"
        ),
        CoreError::Bank { .. }
    ));
}

#[test]
fn a_refused_request_names_its_step_and_the_banks_code() {
    let bad = reqwest::StatusCode::BAD_REQUEST;
    let e = token_error(
        bad,
        &json!({"error": "invalid_grant", "error_description": "user 1000000001 x"}),
        "login",
    );
    let text = e.to_string();
    assert!(text.contains("(login, invalid_grant)"), "{text}");
    assert!(!text.contains("1000000001"), "{text}");
    let e = rejected(bad, "customerportal", &json!({"errorCode": "<b>E 42</b>"}));
    assert!(e.to_string().contains("(customerportal, bE42b)"), "{e}");
    assert!(
        rejected(bad, "app-token", &Value::Null)
            .to_string()
            .contains("(app-token)")
    );
}

#[test]
fn approval_states() {
    for s in ["open", "accepted", "pending"] {
        assert_eq!(
            approval_of(&json!({"status": s})).unwrap(),
            Approval::Pending
        );
    }
    assert_eq!(
        approval_of(&json!({"status": "complete", "resultCode": 200, "resultData": {"DEVICETOKEN": "invented-device"}}))
            .unwrap(),
        Approval::Approved("invented-device".into())
    );
    for s in ["canceled", "failed", "timeout"] {
        assert_eq!(
            approval_of(&json!({"status": s})).unwrap(),
            Approval::Rejected
        );
    }
    assert_eq!(
        approval_of(&json!({"status": "locked"})).unwrap(),
        Approval::Locked
    );
    assert!(approval_of(&json!({"status": "complete", "resultData": {}})).is_err());
    assert!(approval_of(&json!({"status": "something"})).is_err());
}

#[test]
fn amounts_and_dates() {
    assert_eq!(cents(&json!(12.5)), Some(1250));
    assert_eq!(cents(&json!(-1234.56)), Some(-123456));
    assert_eq!(cents(&json!(0)), Some(0));
    assert_eq!(cents(&json!(19.999)), Some(2000));
    assert_eq!(cents(&json!("1.234,56")), Some(123456));
    assert_eq!(cents(&json!("-7,5")), Some(-750));
    assert_eq!(cents(&json!("−3,10 €")), Some(-310));
    assert_eq!(cents(&json!("abc")), None);
    assert_eq!(cents(&Value::Null), None);

    assert_eq!(iso_date("05.11.2026").as_deref(), Some("2026-11-05"));
    assert_eq!(
        iso_date("2026-11-05T10:15:00Z").as_deref(),
        Some("2026-11-05")
    );
    assert_eq!(iso_date("2026-11-05").as_deref(), Some("2026-11-05"));
    assert_eq!(iso_date("31.02.2026"), None);
    assert_eq!(iso_date(""), None);
}

fn accounts_json() -> Value {
    json!([
        {
            "accountNumber": "1000000001",
            "iban": "DE00 0000 0000 1000 0000 01",
            "bic": "TESTDEXXXXX",
            "productLabel": "Test Card",
            "productType": "Kreditkarte",
            "productClass": "Kreditkarte",
            "saldo": 150.25,
            "limit": 2000,
            "availableAmount": 1849.75,
            "status": "Active",
            "nameOnCard": "ERIKA MUSTER",
            "tpan": "****1234",
            "refIban": "DE00********0002"
        },
        {
            "accountNumber": 1000000002,
            "iban": "DE00000000001000000002",
            "productLabel": "",
            "productType": "Spar",
            "productClass": "Sparbrief",
            "saldo": "5.000,00"
        },
        {
            "accountNumber": "1000000003",
            "productType": "Versicherung",
            "productLabel": "Test Insurance"
        }
    ])
}

#[test]
fn reads_cards_and_savings_and_skips_insurances() {
    let a = parse_accounts(&accounts_json()).unwrap();
    assert_eq!(a.len(), 2);
    assert_eq!(
        a[0],
        Account {
            number: "1000000001".into(),
            iban: "DE00000000001000000001".into(),
            label: "Test Card".into(),
            owner: "ERIKA MUSTER".into(),
            kind: AccountKind::CreditCard,
            balance_cents: -15025,
        }
    );
    assert_eq!(a[1].number, "1000000002");
    assert_eq!(a[1].label, "Sparbrief");
    assert_eq!(a[1].kind, AccountKind::Savings);
    assert_eq!(a[1].balance_cents, 500000);

    // A card balance already signed (available = limit + saldo) stays as it is.
    let signed = json!([{"accountNumber": "1", "productType": "Kreditkarte", "saldo": -40, "limit": 100, "availableAmount": 60}]);
    assert_eq!(parse_accounts(&signed).unwrap()[0].balance_cents, -4000);
    // A credit balance on the card (available above the limit) stays positive.
    let credit = json!([{"accountNumber": "1", "productType": "Kreditkarte", "saldo": 40, "limit": 100, "availableAmount": 140}]);
    assert_eq!(parse_accounts(&credit).unwrap()[0].balance_cents, 4000);

    assert!(parse_accounts(&json!({"accounts": []})).is_err());
    assert!(parse_accounts(&json!([{"productType": "Spar", "saldo": 1}])).is_err());
    assert!(parse_accounts(&json!([{"accountNumber": "1", "productType": "Spar"}])).is_err());
}

fn booking(date: &str, amount: Value, booked: bool, id: &str) -> Value {
    json!({
        "transactionDate": date,
        "date": date,
        "amount": amount,
        "creditDebitKey": "D",
        "booked": booked,
        "description": "TEST SHOP KARTENZAHLUNG",
        "transactionId": id,
        "currencyCode": 978,
        "merchantName": "TEST SHOP",
        "city": "MUSTERSTADT",
        "recipientIban": "",
        "recipientName": "",
        "creditorID": "",
        "mandateReference": "",
        "foreignAmount": null,
        "transactionDateTime": "2026-11-10T12:00:00"
    })
}

#[test]
fn reads_bookings_reservations_and_ids() {
    let page = json!({
        "transactions": [
            booking("12.11.2026", json!(-20.98), false, ""),
            booking("10.11.2026", json!(-9.5), true, "tx-test-1"),
            booking("09.11.2026", json!(-4), true, ""),
            booking("09.11.2026", json!(-4), true, ""),
            {
                "transactionDate": "08.11.2026", "date": "08.11.2026", "amount": 100, "booked": true,
                "description": "Überweisung Test", "transactionId": "",
                "recipientName": "Max Mustermann", "recipientIban": "DE00 0000 0000 0000 0000 99",
                "creditorID": "DE00ZZZ00000000001", "mandateReference": "MANDATE-TEST-1"
            },
            booking("01.09.2026", json!(-1), true, "tx-test-old"),
            booking("01.09.2026", json!(-2), false, "")
        ],
        "more": true,
        "moreWithSCA": false
    });
    let (raw, more) = page_of(&page).unwrap();
    assert!(more);
    assert_eq!(oldest_booked(&raw).as_deref(), Some("2026-09-01"));

    let t = parse_bookings(&raw, "hanseatic:1000000001", "2026-11-01").unwrap();
    // The old booking is before `since`; reservations are always kept.
    assert_eq!(t.len(), 6);
    let hold = &t[0];
    assert!(hold.external_id.starts_with(HOLD_PREFIX));
    assert!(hold.external_id.starts_with("pending-"));
    assert_eq!(hold.amount_cents, -2098);
    assert_eq!(hold.booking_text, "Vorgemerkt");
    assert!(t[5].external_id.starts_with(HOLD_PREFIX));
    assert_ne!(t[0].external_id, t[5].external_id);

    let card = &t[1];
    assert_eq!(card.external_id, "hanseatic-tx-test-1");
    assert_eq!(card.booking_date, "2026-11-10");
    assert_eq!(card.counterparty, "TEST SHOP");
    assert_eq!(card.purpose, "TEST SHOP KARTENZAHLUNG MUSTERSTADT");
    assert_eq!(card.currency, "EUR");

    // Two identical bookings without an id stay two rows with stable ids.
    assert_ne!(t[2].external_id, t[3].external_id);
    let again = parse_bookings(&raw, "hanseatic:1000000001", "2026-11-01").unwrap();
    assert_eq!(
        t.iter().map(|x| &x.external_id).collect::<Vec<_>>(),
        again.iter().map(|x| &x.external_id).collect::<Vec<_>>()
    );

    let transfer = &t[4];
    assert_eq!(transfer.amount_cents, 10000);
    assert_eq!(transfer.counterparty, "Max Mustermann");
    assert_eq!(transfer.counterparty_iban, "DE00000000000000000099");
    assert_eq!(transfer.mandate_id, "MANDATE-TEST-1");
    assert_eq!(transfer.creditor_id, "DE00ZZZ00000000001");

    assert!(page_of(&json!({"more": false})).is_err());
    let no_amount = [json!({"date": "01.11.2026", "booked": true})];
    assert!(parse_bookings(&no_amount, "r", "2026-01-01").is_err());
}

#[test]
fn reads_postbox_messages_and_pdfs() {
    let list = json!([
        {"id": "msg-test-1", "accountId": "1000000001", "type": "Kreditkartenkontoauszug", "created": "2026-11-01T08:00:00", "read": false},
        {"id": 42, "accountId": "1000000002", "type": "VertragSparbrief", "created": "15.10.2026", "read": true},
        {"id": "", "accountId": "1000000002", "type": "Sparbriefkontoauszug"},
        {"id": "msg-test-3", "type": "Information", "created": "2026-10-01"}
    ]);
    let m = parse_messages(&list).unwrap();
    assert_eq!(m.len(), 3);
    assert_eq!(
        m[0],
        Message {
            id: "msg-test-1".into(),
            account: "1000000001".into(),
            kind: "Kreditkartenkontoauszug".into(),
            date: "2026-11-01".into(),
        }
    );
    assert_eq!(m[1].id, "42");
    assert_eq!(m[2].account, "");

    assert_eq!(message_title("VertragSparbrief"), "Vertrag Sparbrief");
    assert_eq!(
        message_title("Kreditkartenkontoauszug"),
        "Kreditkartenkontoauszug"
    );
    assert_eq!(message_title(""), "Postbox");
    assert_eq!(document_type("Sparbriefkontoauszug"), "kontoauszug");
    assert_eq!(document_type("Jahressteuerbescheinigung"), "steuer");
    assert_eq!(document_type("VertragSparbrief"), "postbox");
    assert_eq!(
        document_file_name(&m[1]),
        "2026-10-15_Vertrag_Sparbrief_42.pdf"
    );
    let odd = Message {
        id: "../../etc/x".into(),
        account: String::new(),
        kind: "a/b".into(),
        date: "2026-01-01".into(),
    };
    assert!(!document_file_name(&odd).contains('/'));

    let pdf = b"%PDF-1.4 invented";
    let b64 = base64::engine::general_purpose::STANDARD.encode(pdf);
    let wrapped = format!("{}\n{}", &b64[..8], &b64[8..]);
    assert_eq!(
        message_pdf(&json!({"body": {"mimeType": "application/pdf", "content": wrapped}})),
        Some(pdf.to_vec())
    );
    let html = base64::engine::general_purpose::STANDARD.encode(b"<html>");
    assert_eq!(message_pdf(&json!({"body": {"content": html}})), None);
    assert_eq!(message_pdf(&json!({"body": {"content": "%%%"}})), None);
    assert_eq!(message_pdf(&json!({})), None);
    assert!(parse_messages(&json!({})).is_err());
}

#[test]
fn the_state_keeps_documents_and_the_device_token() {
    let mut known = KnownDocuments::default();
    known.insert("msg-test-1".into());
    let token: SecretString = "invented-device".into();
    let state = state_with(&known, Some(&token)).unwrap();

    let back = KnownDocuments::from_state(&state);
    assert!(back.contains("msg-test-1"));
    let saved: SavedLogin = state.decode().unwrap();
    assert_eq!(saved.device_token.unwrap().expose(), "invented-device");

    let plain = state_with(&known, None).unwrap();
    let saved: SavedLogin = plain.decode().unwrap();
    assert!(saved.device_token.is_none());
    let empty: SavedLogin = SecretJson::default().decode().unwrap_or_default();
    assert!(empty.device_token.is_none());
}

#[test]
fn js_strings_are_unescaped() {
    assert_eq!(js_unescape(r"https:\u002F\u002Fa.b"), "https://a.b");
    assert_eq!(js_unescape(r#"a\"b\/c"#), "a\"b/c");
}

#[test]
fn the_app_approval_is_waited_for_only_until_its_deadline() {
    let now = Instant::now();
    assert!(matches!(
        still_waiting(Some(now + APPROVAL_TIMEOUT), now),
        Ok(Phase::Pending(_))
    ));
    assert!(matches!(still_waiting(None, now), Ok(Phase::Pending(_))));
    let expired = still_waiting(Some(now), now + Duration::from_secs(1)).unwrap_err();
    assert_eq!(expired.to_string(), approval_expired().to_string());
}

/// Acceptance "no duplicate bookings": a first sync, repeated syncs and a
/// reservation that gets booked leave every booking exactly once.
#[test]
fn repeated_syncs_store_every_booking_once() {
    use crate::models::ConnectorKind;
    use crate::vault::Vault;

    let dir = crate::test_support::TempDir::new("girocrab-hanseatic");
    let path = dir.join("v.db");
    let v = Vault::open(path.to_string_lossy().to_string(), vec![7u8; 32]).unwrap();
    let conn = v
        .add_bank_connection(
            ConnectorKind::HanseaticBank,
            String::new(),
            "1000000099".into(),
        )
        .unwrap();
    let account = parse_accounts(&accounts_json()).unwrap().remove(0);
    let account_ref = format!("{REF_PREFIX}{}", account.number);
    let fetch = |raw: Vec<Value>| {
        let transactions = parse_bookings(&raw, &account_ref, "2026-11-01").unwrap();
        FetchResult {
            accounts: vec![RemoteAccount {
                external_ref: account_ref.clone(),
                name: format!("Hanseatic Bank {}", account.label),
                owner: account.owner.clone(),
                iban: account.iban.clone(),
                account_number: account.number.clone(),
                currency: "EUR".into(),
                kind: account.kind,
                balance_cents: account.balance_cents,
                balance_date: "2026-11-12".into(),
            }],
            transactions,
            balances_reported: vec![account_ref.clone()],
            hold_prefix: Some(HOLD_PREFIX.into()),
            ..Default::default()
        }
    };
    let rows = || -> Vec<(String, i64)> {
        let db = v.db().unwrap();
        let mut q = db
            .prepare(
                "SELECT external_id, amount_cents FROM transactions
                  WHERE deleted_ms IS NULL ORDER BY booking_date, external_id",
            )
            .unwrap();
        q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    let first = vec![
        booking("12.11.2026", json!(-20.98), false, ""),
        booking("10.11.2026", json!(-9.5), true, "tx-test-1"),
        booking("09.11.2026", json!(-4), true, ""),
        booking("09.11.2026", json!(-4), true, ""),
    ];
    super::super::store_fetch(&v, ConnectorKind::HanseaticBank, conn, fetch(first.clone()))
        .unwrap();
    let after_first = rows();
    assert_eq!(after_first.len(), 4);
    for _ in 0..2 {
        super::super::store_fetch(&v, ConnectorKind::HanseaticBank, conn, fetch(first.clone()))
            .unwrap();
        assert_eq!(rows(), after_first);
    }

    // The reservation is booked: it now carries an id and a later date.
    let mut booked = first[1..].to_vec();
    booked.insert(0, booking("13.11.2026", json!(-20.98), true, "tx-test-2"));
    super::super::store_fetch(&v, ConnectorKind::HanseaticBank, conn, fetch(booked)).unwrap();
    let after_booking = rows();
    assert_eq!(after_booking.len(), 4);
    assert!(
        after_booking
            .iter()
            .all(|(id, _)| !id.starts_with(HOLD_PREFIX))
    );
    assert_eq!(
        after_booking
            .iter()
            .filter(|(_, cents)| *cents == -2098)
            .count(),
        1
    );
    // Balance in cents as the bank reports it.
    let balance: i64 = v
        .db()
        .unwrap()
        .query_row(
            "SELECT balance_cents FROM accounts WHERE deleted_ms IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(balance, -15025);
    drop(v);
    std::fs::remove_file(path).ok();
}
