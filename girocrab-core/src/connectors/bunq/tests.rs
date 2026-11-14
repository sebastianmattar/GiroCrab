use base64::Engine as _;
use rsa::pkcs8::{EncodePrivateKey, EncodePublicKey, LineEnding};
use rsa::signature::{SignatureEncoding, Signer};
use rsa::PrivateKey;

use super::*;
use crate::models::AccountKind;
use crate::money::json_number_to_cents;

#[test]
fn parses_accounts_and_payments() {
    let acc = json!({"MonetaryAccountSavings": {
        "id": 42, "description": "Rücklagen", "currency": "EUR", "status": "ACTIVE",
        "balance": {"value": "1234.56", "currency": "EUR"},
        "alias": [{"type": "EMAIL", "value": "x@example.com"},
                  {"type": "IBAN", "value": "NL00BUNQ0000000000", "name": "Erika Mustermann"}]
    }});
    let (id, a) = parse_account(&acc, "2026-11-10").unwrap();
    assert_eq!(id, 42);
    assert_eq!(a.external_ref, "bunq:42");
    assert_eq!(a.kind, AccountKind::Savings);
    assert_eq!(a.balance_cents, 123_456);
    assert_eq!(a.iban, "NL00BUNQ0000000000");
    assert_eq!(a.owner, "Erika Mustermann");

    let cancelled = json!({"MonetaryAccountBank": {"id": 1, "status": "CANCELLED"}});
    assert!(parse_account(&cancelled, "2026-11-10").is_none());
    let closing =
        json!({"MonetaryAccountBank": {"id": 1, "status": "CANCELLED_PENDING_SETTLEMENT"}});
    assert!(parse_account(&closing, "2026-11-10").is_none());
    let ext = json!({"MonetaryAccountExternalSavings": {"id": 3, "status": "ACTIVE"}});
    assert_eq!(
        parse_account(&ext, "2026-11-10").unwrap().1.kind,
        AccountKind::Savings
    );

    let pay = json!({
        "id": 7, "created": "2026-11-09 12:00:00.000000",
        "amount": {"value": "-12.50", "currency": "EUR"},
        "description": " Lunch ", "type": "MASTERCARD", "sub_type": "PAYMENT",
        "counterparty_alias": {"display_name": "Café", "iban": null}
    });
    let t = parse_payment(&pay, "bunq:42").unwrap();
    assert_eq!(t.external_id, "bunq-7");
    assert_eq!(t.amount_cents, -1_250);
    assert_eq!(t.counterparty, "Café");
    assert_eq!(t.purpose, "Lunch");
    assert_eq!(t.booking_date, "2026-11-09");
    assert_eq!(t.booking_text, tr("Kartenzahlung"));
}

#[test]
fn signature_roundtrip_and_find() {
    let key = PrivateKey::new(&mut getrandom::SysRng, 1024).unwrap();
    let mut c = BunqConnector::new();
    c.state.private_key_pem = key.to_pkcs8_pem(LineEnding::LF).unwrap().to_string();
    c.state.server_public_key = key
        .to_public_key()
        .to_public_key_pem(LineEnding::LF)
        .unwrap();
    let body = br#"{"Response":[{"Id":{"id":1}},{"Token":{"token":"abc"}}]}"#;
    let sig = base64::engine::general_purpose::STANDARD
        .encode(c.signing_key().unwrap().sign(body).to_bytes());
    assert!(c.verify_response(body, Some(&sig)).is_ok());
    assert!(c.verify_response(b"tampered", Some(&sig)).is_err());
    assert!(c.verify_response(body, None).is_err());
    let v: Value = serde_json::from_slice(body).unwrap();
    assert_eq!(find(&v, "Token")["token"], "abc");
    assert_eq!(key_fingerprint("k").len(), 16);
}

#[test]
fn card_action_refusals() {
    assert!(refuses_card_actions(404));
    assert!(refuses_card_actions(403));
    for status in [0, 401, 429, 500, 503] {
        assert!(!refuses_card_actions(status), "{status}");
    }
    assert!(
        HOLD_PREFIX.starts_with("pending-"),
        "Booking.is_pending relies on it"
    );
}

#[test]
fn card_holds_and_foreign_amounts() {
    let now =
        chrono::NaiveDateTime::parse_from_str("2026-11-10 12:00:00.0", "%Y-%m-%d %H:%M:%S%.f")
            .unwrap();
    let mut a = json!({
        "id": 99, "created": "2026-11-08 10:00:00.000000",
        "amount_local": {"value": "20.00", "currency": "USD"},
        "amount_billing": {"value": "18.70", "currency": "EUR"},
        "decision": "ALLOWED", "clearing_status": "PENDING",
        "authorisation_type": "NORMAL_AUTHORISATION",
        "reservation_expiry_time": "2026-11-20 10:00:00.000000",
        "description": "NYC Deli", "counterparty_alias": {"display_name": "NYC Deli"}
    });
    assert!(is_open_hold(&a, now));
    let t = parse_hold(&a, "bunq:1").unwrap();
    assert_eq!(t.external_id, "pending-hold-bunq-99");
    assert_eq!(t.amount_cents, -1_870);
    assert!(t.purpose.contains("USD"), "{}", t.purpose);
    assert!(t.booking_text.contains(tr("vorgemerkt")));
    assert_eq!(
        foreign_amount_note(&a, "EUR"),
        Some(trf(
            "Originalbetrag {0}",
            &[&crate::money::format_cents(2_000, "USD".into())]
        ))
    );
    assert_eq!(foreign_amount_note(&a, "USD"), None);

    a["amount_billing"]["value"] = json!("-18.70");
    assert_eq!(card_amount_cents(&a), Some(-1_870));
    a["authorisation_type"] = json!("REFUND");
    assert_eq!(card_amount_cents(&a), Some(1_870));

    a["clearing_status"] = json!("FIRST_PRESENTMENT_COMPLETE");
    assert!(!is_open_hold(&a, now), "cleared = booked");
    a["clearing_status"] = json!("PENDING");
    a["decision"] = json!("INSUFFICIENT_BALANCE");
    assert!(!is_open_hold(&a, now), "declined");
    a["decision"] = json!("ALLOWED");
    a["reservation_expiry_time"] = json!("2026-11-01 00:00:00.000000");
    assert!(!is_open_hold(&a, now), "expired");
}

#[test]
fn request_payments_match_their_response() {
    let pay = json!({"id": 1, "sub_type": "REQUEST", "created": "2026-11-02 14:01:03.477301",
        "amount": {"value": "-19.57", "currency": "EUR"}});
    let resp = |id: i64, status: &str, value: &str, t: &str| {
        json!({"id": id, "status": status,
        "amount_responded": {"value": value, "currency": "EUR"}, "time_responded": t})
    };
    assert!(is_request_payment(&pay));
    assert!(!is_request_payment(
        &json!({"sub_type": "PAYMENT", "amount": {"value": "-1.00"}})
    ));
    assert!(!is_request_payment(
        &json!({"sub_type": "REQUEST", "amount": {"value": "5.00"}})
    ));
    let responses = vec![
        resp(10, "ACCEPTED", "19.57", "2026-10-02 14:01:03.500000"), // last month
        resp(11, "REJECTED", "19.57", "2026-11-02 14:01:03.500000"),
        resp(12, "ACCEPTED", "19.58", "2026-11-02 14:01:03.500000"),
        resp(13, "ACCEPTED", "19.57", "2026-11-02 14:01:03.584702"),
    ];
    assert_eq!(match_request_response(&pay, &responses), Some(13));
    assert_eq!(match_request_response(&pay, &responses[..3]), None);
}

#[test]
fn only_plain_api_paths_are_sent() {
    assert!(is_api_path("/v1/installation"));
    assert!(is_api_path(
        "/v1/user/1/monetary-account/2/payment?count=200&older_id=123"
    ));
    for bad in [
        "@evil.com/v1/x",
        "/v1/x@evil.com",
        "//evil.com/v1/x",
        "/v1//evil.com",
        "https://evil.com/v1/x",
        "/v2/x",
        "v1/x",
        "/v1/x\\evil",
        "/v1/x y",
        "/v1/x\n",
        "",
    ] {
        assert!(!is_api_path(bad), "{bad:?}");
    }
}

#[test]
fn draft_payments_are_found_by_their_payment() {
    let drafts = vec![
        json!({"id": 7, "status": "PENDING", "entries": [{"amount": {"value": "1.00"}}]}),
        json!({"id": 8, "status": "ACCEPTED",
            "object": {"Payment": {"id": 2996727905_i64, "monetary_account_id": 13340587}}}),
    ];
    assert_eq!(draft_of_payment(2996727905, &drafts), Some(8));
    // other field names / string form
    let other = vec![json!({"id": 9, "result": [{"payment_id": "2996727906"}]})];
    assert_eq!(draft_of_payment(2996727906, &other), Some(9));
    assert_eq!(draft_of_payment(7, &drafts), None); // the draft's own id is no link
    assert_eq!(draft_of_payment(1, &drafts), None);
}

#[test]
fn draft_statuses_never_report_failed_when_unclear() {
    assert_eq!(classify_draft_status("ACCEPTED"), DraftState::Accepted);
    assert_eq!(
        classify_draft_status("DEPRECATED_ACCEPTED_PENDING_PAYMENT"),
        DraftState::Accepted
    );
    assert_eq!(classify_draft_status("PENDING"), DraftState::Pending);
    assert_eq!(classify_draft_status(""), DraftState::Pending);
    for s in ["REJECTED", "CANCELLED", "EXPIRED"] {
        assert_eq!(classify_draft_status(s), DraftState::Refused, "{s}");
    }
    assert_eq!(classify_draft_status("PROCESSING"), DraftState::Unclear);
}

#[tokio::test]
async fn failing_draft_polls_end_as_unknown() {
    // No HTTP client: every status poll fails.
    let mut c = BunqConnector::new();
    assert!(!c.order_submitted());
    let challenge = Challenge {
        kind: ChallengeKind::DecoupledPush,
        title: "t".into(),
        message: "m".into(),
        method_name: "bunq".into(),
        redirect_url: None,
        image: None,
        expires_in_seconds: None,
        poll_interval_seconds: 3,
    };
    c.draft = Some(PendingDraft {
        account_id: 1,
        draft_id: 2,
        challenge,
        poll_failures: 0,
    });
    let ctx = ConnectContext {
        connection_id: 1,
        login_name: String::new(),
        secret: crate::secret::SecretString::new(String::new()),
        settings: Default::default(),
        state: Value::Null,
        since: "2020-01-01".into(),
        since_by_account: Default::default(),
    };
    for _ in 1..MAX_DRAFT_POLL_FAILURES {
        let p = c.respond(&ctx, ChallengeResponse::Poll).await.unwrap();
        assert!(matches!(p, Phase::Pending(_)), "{p:?}");
    }
    let p = c.respond(&ctx, ChallengeResponse::Poll).await.unwrap();
    assert!(matches!(p, Phase::Ready), "{p:?}");
    assert!(matches!(
        c.transfer_result(),
        TransferResult::Unknown { .. }
    ));
    // The draft is kept so `finish` can still try to withdraw it.
    assert!(c.draft.is_some());
}

#[test]
fn older_receipt_checks_rotate() {
    let ids = vec![10, 50, 30, 20, 40];
    // first round starts with the newest
    assert_eq!(rotate_older(ids.clone(), None, 2), (vec![50, 40], Some(40)));
    // continues below the cursor and wraps around
    assert_eq!(
        rotate_older(ids.clone(), Some(40), 2),
        (vec![30, 20], Some(20))
    );
    assert_eq!(
        rotate_older(ids.clone(), Some(20), 2),
        (vec![10, 50], Some(50))
    );
    // everything fits; no budget keeps the cursor; nothing older clears it
    assert_eq!(rotate_older(vec![3, 1, 2], Some(2), 9).0, vec![1, 3, 2]);
    assert_eq!(rotate_older(ids, Some(30), 0), (vec![], Some(30)));
    assert_eq!(rotate_older(vec![], Some(30), 5), (vec![], None));
}

#[test]
fn receipts_and_statement_documents() {
    let d = |s: &str| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
    let months = completed_months(d("2026-03-15"), 3);
    assert_eq!(
        months,
        vec![
            (d("2026-02-01"), d("2026-02-28")),
            (d("2026-01-01"), d("2026-01-31")),
            (d("2025-12-01"), d("2025-12-31")),
        ]
    );
    let doc = receipt_document(
        "bunq:1",
        "bunq-7",
        "2026-11-09",
        "Café Ü/1",
        "",
        55,
        b"%PDF-1.4 x".to_vec(),
        "application/octet-stream",
        "bunq:attachment:55".into(),
    );
    assert_eq!(doc.mime_type, "application/pdf");
    assert_eq!(doc.file_name, "2026-11-09_Café_Ü_1_55.pdf");
    assert_eq!(doc.booking_external_id.as_deref(), Some("bunq-7"));
    assert_eq!(doc.doc_type, "beleg");
    let png = receipt_document(
        "bunq:1",
        "bunq-7",
        "2026-11-09",
        "x",
        "Kassenbon",
        56,
        vec![1, 2],
        "image/png; charset=binary",
        "bunq:attachment:56".into(),
    );
    assert_eq!(
        (png.mime_type.as_str(), png.title.as_str()),
        ("image/png", "Kassenbon")
    );

    let acc = RemoteAccount {
        external_ref: "bunq:1".into(),
        name: "Main account".into(),
        owner: String::new(),
        iban: String::new(),
        account_number: String::new(),
        currency: "EUR".into(),
        kind: AccountKind::Checking,
        balance_cents: 0,
        balance_date: String::new(),
    };
    let st = statement_document(&acc, 1, d("2026-10-01"), d("2026-10-31"), b"%PDF".to_vec());
    assert_eq!(st.source_url, "bunq:statement:1:2026-10");
    assert_eq!(st.file_name, "2026-10_bunq_Main_account.pdf");
    assert_eq!(
        (st.doc_type.as_str(), st.date.as_str()),
        ("kontoauszug", "2026-10-31")
    );
    // Old stored state without the new fields still loads.
    let old: BunqState = serde_json::from_value(json!({"env": "sandbox", "user_id": 5})).unwrap();
    assert_eq!(old.user_id, 5);
    assert!(old.statements_done.is_empty());
}

#[test]
fn decimal_amounts() {
    assert_eq!(cents_to_decimal(1_234), "12.34");
    assert_eq!(cents_to_decimal(5), "0.05");
    assert_eq!(cents_to_decimal(-100), "-1.00");
}

/// Minimal valid RGB PNG (bunq rejects tiny or malformed images).
fn test_png(w: u32, h: u32) -> Vec<u8> {
    fn crc32(data: &[u8]) -> u32 {
        let mut c = 0xffff_ffffu32;
        for &b in data {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xedb8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
        }
        !c
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
        out.extend((data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend(data);
        out.extend(&body);
        out.extend(crc32(&body).to_be_bytes());
    }
    let mut raw = Vec::new();
    for y in 0..h {
        raw.push(0); // filter: none
        for x in 0..w {
            raw.extend([(x * 2) as u8, (y * 2) as u8, 0x80]);
        }
    }
    let mut ihdr = Vec::new();
    ihdr.extend(w.to_be_bytes());
    ihdr.extend(h.to_be_bytes());
    ihdr.extend([8, 2, 0, 0, 0]);
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(
        &mut out,
        b"IDAT",
        &miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6),
    );
    chunk(&mut out, b"IEND", &[]);
    out
}

/// Live check against the bunq sandbox:
/// `BUNQ_SANDBOX_KEY=sandbox_... cargo test -p girocrab-core bunq_sandbox -- --ignored --nocapture`
#[tokio::test]
#[ignore]
async fn bunq_sandbox_live() {
    use crate::secret::SecretString;
    let key = std::env::var("BUNQ_SANDBOX_KEY").expect("BUNQ_SANDBOX_KEY");
    let mut settings = std::collections::HashMap::new();
    settings.insert(
        "bunq.environment".to_string(),
        SecretString::new("sandbox".into()),
    );
    let mut ctx = ConnectContext {
        connection_id: 1,
        login_name: String::new(),
        secret: SecretString::new(key),
        settings,
        state: Value::Null,
        since: "2020-01-01".into(),
        since_by_account: Default::default(),
    };
    let mut c = BunqConnector::new();
    assert!(matches!(c.begin(&ctx).await.unwrap(), Phase::Ready));
    let r = c.fetch(&ctx).await.unwrap();
    println!("accounts: {:?}", r.accounts);
    let (acc_id, _) = c.accounts().await.unwrap()[0].clone();
    // Ask the sandbox "Sugar Daddy" for money so there is a payment to read.
    let token = c.state.session_token.clone();
    let path = format!(
        "/v1/user/{}/monetary-account/{acc_id}/request-inquiry",
        c.state.user_id
    );
    let body = json!({
        "amount_inquired": {"value": "12.34", "currency": "EUR"},
        "counterparty_alias": {"type": "EMAIL", "value": "sugardaddy@bunq.com", "name": "Sugar Daddy"},
        "description": "GiroCrab test",
        "allow_bunqme": false
    });
    c.call(reqwest::Method::POST, &path, Some(&body), Some(&token))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;
    let r = c.fetch(&ctx).await.unwrap();
    println!("transactions: {:#?}", r.transactions);
    assert!(r.transactions.iter().any(|t| t.amount_cents == 1_234));
    // Receipt: upload a PNG, attach it to the Sugar Daddy payment as a note,
    // and expect the next sync to deliver it as a booking document.
    let png = test_png(100, 100);
    let png_ref: &[u8] = &png;
    // Notes can't be added to the incoming request payment: pay 0.10 out first.
    let pay_path = format!(
        "/v1/user/{}/monetary-account/{acc_id}/payment",
        c.state.user_id
    );
    let v = c
        .call(
            reqwest::Method::POST,
            &pay_path,
            Some(&json!({
                "amount": {"value": "0.10", "currency": "EUR"},
                "counterparty_alias": {"type": "EMAIL", "value": "sugardaddy@bunq.com", "name": "Sugar Daddy"},
                "description": "GiroCrab receipt test"
            })),
            Some(&token),
        )
        .await
        .unwrap();
    let pid = find(&v, "Id")["id"].as_i64().unwrap();
    let pay_ext = format!("bunq-{pid}");
    let up_path = format!(
        "/v1/user/{}/monetary-account/{acc_id}/attachment",
        c.state.user_id
    );
    let (up, _) = c
        .send(
            reqwest::Method::POST,
            &up_path,
            Some((png.clone(), "image/png")),
            &[("X-Bunq-Attachment-Description", "Kassenbon")],
            Some(&token),
        )
        .await
        .unwrap();
    let up: Value = serde_json::from_slice(&up).unwrap();
    let att_id = find(&up, "Id")["id"].as_i64().unwrap();
    let note_path = format!(
        "/v1/user/{}/monetary-account/{acc_id}/payment/{pid}/note-attachment",
        c.state.user_id
    );
    c.call(
        reqwest::Method::POST,
        &note_path,
        Some(&json!({"attachment_id": att_id, "description": "Kassenbon"})),
        Some(&token),
    )
    .await
    .unwrap();
    let r = c.fetch(&ctx).await.unwrap();
    for d in &r.documents {
        println!(
            "document: {} {} {} {} bytes",
            d.doc_type,
            d.title,
            d.file_name,
            d.bytes.len()
        );
    }
    let receipt = r
        .documents
        .iter()
        .find(|d| d.doc_type == "beleg")
        .expect("receipt");
    assert_eq!(
        receipt.booking_external_id.as_deref(),
        Some(pay_ext.as_str())
    );
    assert!(receipt.bytes.starts_with(&png_ref[..8]), "PNG expected");
    assert_eq!(receipt.mime_type, "image/png");
    // Statement for the current month (the account is too new for a completed one).
    let today = chrono::Local::now().date_naive();
    use chrono::Datelike;
    let pdf = c
        .export_statement(acc_id, today.with_day(1).unwrap(), today)
        .await
        .unwrap_or_else(|e| panic!("statement: {} {}", e.status, e.message));
    assert!(pdf.starts_with(b"%PDF"));
    println!("statement pdf: {} bytes", pdf.len());

    // Second sync reuses the stored session.
    ctx.state = r.state.unwrap();
    let token_before = ctx.state["session_token"].clone();
    let mut c2 = BunqConnector::new();
    assert!(matches!(c2.begin(&ctx).await.unwrap(), Phase::Ready));
    assert_eq!(Value::String(c2.state.session_token.clone()), token_before);

    // Transfer: draft payment stays pending (no app in the sandbox), then
    // cancelling in GiroCrab withdraws it at bunq.
    let iban = r.accounts[0].iban.clone();
    let job = TransferJob {
        source_iban: iban,
        source_owner: String::new(),
        recipient_name: "Sugar Daddy".into(),
        recipient_iban: "NL32BUNQ2025313705".into(),
        recipient_bic: None,
        amount_cents: 1_00,
        purpose: "GiroCrab draft test".into(),
        instant: false,
        execution_date: None,
        end_to_end_id: "GCTEST".into(),
    };
    let phase = c2.send_transfer(&ctx, &job).await.unwrap();
    assert!(matches!(phase, Phase::Challenge(ref ch) if ch.kind == ChallengeKind::DecoupledPush));
    let d = c2.draft.clone().unwrap();
    let polled = c2.respond(&ctx, ChallengeResponse::Poll).await.unwrap();
    assert!(matches!(polled, Phase::Pending(_)));
    c2.finish(&ctx).await;
    assert!(c2.draft.is_none());
    let status = c2.draft_status(&d).await.unwrap();
    println!("draft status after cancel: {status}");
    assert_eq!(status, "REJECTED");
    assert!(c2.order_submitted());

    // bunq refuses to create the draft (invalid IBAN): nothing exists
    // that could be approved, so it is a plain error, not "Status unklar".
    let mut bad = job.clone();
    bad.recipient_iban = "XX00INVALID".into();
    let refused = c2.send_transfer(&ctx, &bad).await;
    println!("draft with invalid IBAN: {refused:?}");
    assert!(refused.is_err());
    assert!(!c2.order_submitted(), "refused POST must not be unclear");

    // Rejected outside GiroCrab (as if in the app): the next poll is a
    // clear error, nothing executed, not "Status unklar".
    c2.send_transfer(&ctx, &job).await.unwrap();
    let d = c2.draft.clone().unwrap();
    let token = c2.state.session_token.clone();
    let path = c2.draft_path(d.account_id, Some(d.draft_id));
    c2.call(
        reqwest::Method::PUT,
        &path,
        Some(&json!({"status": "REJECTED"})),
        Some(&token),
    )
    .await
    .map_err(ApiError::into_core)
    .unwrap();
    let polled = c2.respond(&ctx, ChallengeResponse::Poll).await;
    println!("poll after reject: {polled:?}");
    assert!(
        matches!(polled, Err(CoreError::Bank { ref code, .. }) if code == "draft"),
        "{polled:?}"
    );
    assert!(c2.draft.is_none());
    assert!(!c2.order_submitted(), "rejected draft must not be unclear");

    // Status checks keep failing (broken session token): pending twice,
    // then "unknown", never "failed". The draft is kept, and once the
    // token works again `finish` still withdraws it.
    c2.send_transfer(&ctx, &job).await.unwrap();
    let d = c2.draft.clone().unwrap();
    c2.state.session_token = "invalid".into();
    for _ in 1..MAX_DRAFT_POLL_FAILURES {
        let p = c2.respond(&ctx, ChallengeResponse::Poll).await.unwrap();
        assert!(matches!(p, Phase::Pending(_)), "{p:?}");
    }
    let p = c2.respond(&ctx, ChallengeResponse::Poll).await.unwrap();
    assert!(matches!(p, Phase::Ready), "{p:?}");
    let result = c2.transfer_result();
    println!("result after failing polls: {result:?}");
    assert!(matches!(result, TransferResult::Unknown { .. }));
    assert!(c2.draft.is_some());
    c2.state.session_token = token;
    c2.finish(&ctx).await;
    let status = c2.draft_status(&d).await.unwrap();
    println!("draft status after unclear + finish: {status}");
    assert_eq!(status, "REJECTED");
}

/// Prints only the card-action status values and amount signs of a real
/// account (no names, amounts or descriptions) to check `is_open_hold`:
/// `BUNQ_API_KEY=... cargo test -p girocrab-core bunq_card_survey -- --ignored --nocapture`
#[tokio::test]
#[ignore]
async fn bunq_card_survey() {
    use crate::secret::SecretString;
    let key = std::env::var("BUNQ_API_KEY").expect("BUNQ_API_KEY");
    let ctx = ConnectContext {
        connection_id: 1,
        login_name: String::new(),
        secret: SecretString::new(key),
        settings: Default::default(),
        state: Value::Null,
        since: "2020-01-01".into(),
        since_by_account: Default::default(),
    };
    let mut c = BunqConnector::new();
    c.begin(&ctx).await.unwrap();
    let window = (chrono::Local::now().date_naive() - chrono::Duration::days(HOLD_WINDOW_DAYS))
        .format("%Y-%m-%d")
        .to_string();
    let now = chrono::Utc::now().naive_utc();
    let mut seen = std::collections::BTreeMap::new();
    for (id, acc) in c.accounts().await.unwrap() {
        let actions = match c.card_actions(id, &window).await {
            Ok(a) => a,
            Err(e) => {
                println!(
                    "account {}: card actions unavailable ({} {})",
                    acc.kind.as_str(),
                    e.status,
                    e.message
                );
                continue;
            }
        };
        for a in &actions {
            let f = |k: &str| a[k].as_str().unwrap_or("-").to_string();
            let sign = match json_number_to_cents(&a["amount_billing"]["value"]) {
                Ok(c) if c < 0 => "neg",
                Ok(_) => "pos",
                Err(_) => "?",
            };
            let foreign = foreign_amount_note(a, &acc.currency).is_some();
            // Days relative to now (negative = past), rounded; "-" if absent.
            let rel = |k: &str| match a[k].as_str() {
                None => "none".to_string(),
                Some(t) => match chrono::NaiveDateTime::parse_from_str(t, "%Y-%m-%d %H:%M:%S%.f") {
                    Ok(t) => format!("{:+}d", (t - now).num_days()),
                    Err(_) => format!("unparsed({})", t.len()),
                },
            };
            let linked = match a["id"].as_i64() {
                Some(aid) => c
                    .payments_of_action(id, aid)
                    .await
                    .map(|p| p.len().to_string())
                    .unwrap_or_else(|_| "err".into()),
                None => "?".into(),
            };
            let row = format!(
                "decision={} auth_status={} auth_type={} payment_status={} clearing={} settlement={} billing_sign={sign} foreign={foreign} created={} reservation_expiry={} clearing_expiry={} linked_payments={linked} => open_hold={}",
                f("decision"), f("authorisation_status"), f("authorisation_type"),
                f("payment_status"), f("clearing_status"), f("settlement_status"),
                rel("created"), rel("reservation_expiry_time"), rel("clearing_expiry_time"),
                is_open_hold(a, now)
            );
            *seen.entry(row).or_insert(0) += 1;
        }
    }
    for (row, n) in seen {
        println!("{n:>4}x {row}");
    }
}

/// Read-only look for direct debits that bunq already knows before they are
/// booked, plus scheduled payments. Prints only field names, status values,
/// amount signs and dates relative to now (no names, amounts or texts):
/// `BUNQ_API_KEY=... cargo test -q -p girocrab-core bunq_direct_debit_survey -- --ignored --nocapture`
#[tokio::test]
#[ignore]
async fn bunq_direct_debit_survey() {
    use crate::secret::SecretString;
    let key = std::env::var("BUNQ_API_KEY").expect("BUNQ_API_KEY");
    let ctx = ConnectContext {
        connection_id: 1,
        login_name: String::new(),
        secret: SecretString::new(key),
        settings: Default::default(),
        state: Value::Null,
        since: "2020-01-01".into(),
        since_by_account: Default::default(),
    };
    let mut c = BunqConnector::new();
    c.begin(&ctx).await.unwrap();
    let now = chrono::Utc::now().naive_utc();
    let uid = c.state.user_id;
    // (path below the account, object key, label)
    let lists = [
        (
            "request-response?count=200",
            "RequestResponse",
            "request-response",
        ),
        (
            "schedule-payment?count=200",
            "ScheduledPayment",
            "schedule-payment",
        ),
        ("schedule?count=200", "Schedule", "schedule"),
    ];
    for (id, acc) in c.accounts().await.unwrap() {
        println!("== account {} ({})", acc.kind.as_str(), acc.currency);
        for (path, obj_key, label) in lists {
            let full = format!("/v1/user/{uid}/monetary-account/{id}/{path}");
            match c.get(&full).await {
                Err(e) => println!("  {label}: ERROR {} {}", e.status, e.message),
                Ok(v) => {
                    let items: Vec<&Value> = v["Response"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|i| &i[obj_key])
                        .filter(|o| o.is_object())
                        .collect();
                    println!("  {label}: {} item(s) on the first page", items.len());
                    let mut seen = BTreeMap::new();
                    for o in items {
                        *seen.entry(survey_shape(o, now, "")).or_insert(0) += 1;
                    }
                    for (row, n) in seen {
                        println!("  {n:>4}x {row}");
                    }
                }
            }
        }
        // Anything still awaiting a reply, whatever its type.
        let full = format!(
            "/v1/user/{uid}/event?monetary_account_id={id}&count=200&display_user_event=false"
        );
        match c.get(&full).await {
            Err(e) => println!("  event: ERROR {} {}", e.status, e.message),
            Ok(v) => {
                let mut seen = BTreeMap::new();
                for i in v["Response"].as_array().into_iter().flatten() {
                    let ev = &i["Event"];
                    let ty = ev["object"]
                        .as_object()
                        .and_then(|m| m.keys().next().cloned())
                        .unwrap_or_else(|| "-".into());
                    let inner = ev["object"][&ty].clone();
                    let row = format!(
                        "type={ty} event_status={} action={} | {}",
                        ev["status"].as_str().unwrap_or("-"),
                        ev["action"].as_str().unwrap_or("-"),
                        survey_shape(&inner, now, "")
                    );
                    *seen.entry(row).or_insert(0) += 1;
                }
                println!("  event (first page, by shape):");
                for (row, n) in seen {
                    println!("  {n:>4}x {row}");
                }
            }
        }
    }
}

/// Read-only check of one payment's receipts on the real account:
/// `BUNQ_API_KEY=… BUNQ_AMOUNT=19.57 cargo test -q -p girocrab-core bunq_receipt_survey -- --ignored --nocapture`
#[tokio::test]
#[ignore]
async fn bunq_receipt_survey() {
    use crate::secret::SecretString;
    let key = std::env::var("BUNQ_API_KEY").expect("BUNQ_API_KEY");
    let amount = std::env::var("BUNQ_AMOUNT").unwrap_or_else(|_| "19.57".into());
    let want = json_number_to_cents(&Value::String(amount.clone()))
        .unwrap()
        .abs();
    let ctx = ConnectContext {
        connection_id: 1,
        login_name: String::new(),
        secret: SecretString::new(key),
        settings: Default::default(),
        state: Value::Null,
        since: "2020-01-01".into(),
        since_by_account: Default::default(),
    };
    let mut c = BunqConnector::new();
    c.begin(&ctx).await.unwrap();
    let since = (chrono::Local::now().date_naive() - chrono::Duration::days(180))
        .format("%Y-%m-%d")
        .to_string();
    let today = chrono::Local::now().date_naive();
    let shape = |v: &Value| match v {
        Value::Null => "null".to_string(),
        Value::Array(a) => format!("array[{}]", a.len()),
        Value::Object(o) => format!(
            "object{{{}}}",
            o.keys().cloned().collect::<Vec<_>>().join(",")
        ),
        other => format!("{other}").chars().take(20).collect(),
    };
    let mut hits = 0;
    for (id, acc) in c.accounts().await.unwrap() {
        let uid = c.state.user_id;
        let payments = c.raw_payments(id, &since).await.unwrap();
        println!(
            "account {} ({}): {} payments in 180 days",
            acc.kind.as_str(),
            id,
            payments.len()
        );
        for p in &payments {
            let cents = json_number_to_cents(&p["amount"]["value"]).unwrap_or(0);
            if cents.abs() != want {
                continue;
            }
            hits += 1;
            let pid = p["id"].as_i64().unwrap_or_default();
            let date = p["created"]
                .as_str()
                .and_then(local_date)
                .unwrap_or_default();
            let age = chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d")
                .map(|d| (today - d).num_days())
                .unwrap_or(-1);
            println!(
                "\nPAYMENT id={pid} date={date} age={age}d amount={cents} type={} sub_type={} updated={} payment.attachment={}",
                p["type"].as_str().unwrap_or("-"),
                p["sub_type"].as_str().unwrap_or("-"),
                p["updated"].as_str().unwrap_or("-"),
                shape(&p["attachment"]),
            );
            let refs: Vec<String> = p["request_reference_split_the_bill"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|r| shape(r) + &format!(" {}", r))
                .map(|t| t.chars().take(160).collect())
                .collect();
            println!("  request_reference_split_the_bill: {refs:?}");
            // Payments of a request: the notes live on the RequestResponse.
            let mut objects = vec![format!("payment/{pid}")];
            let path = format!("/v1/user/{uid}/monetary-account/{id}/request-response?count=200");
            match c.get(&path).await {
                Err(e) => println!("  request-response list: ERROR {e:?}"),
                Ok(v) => {
                    for item in v["Response"].as_array().into_iter().flatten() {
                        let r = &item["RequestResponse"];
                        let rc = json_number_to_cents(&r["amount_responded"]["value"])
                            .or_else(|_| json_number_to_cents(&r["amount_inquired"]["value"]))
                            .unwrap_or(0);
                        if rc.abs() != want {
                            continue;
                        }
                        println!(
                            "  REQUEST-RESPONSE id={} status={} type={} sub_type={} event_id={} time_responded={} created={} attachment={}",
                            r["id"], r["status"], r["type"], r["sub_type"], r["event_id"],
                            r["time_responded"], r["created"], shape(&r["attachment"]),
                        );
                        for a in attachment_items(&r["attachment"]) {
                            println!("    own attachment: {}", shape(a));
                            if let Some(aid) = a["id"].as_i64() {
                                match c.attachment(aid).await {
                                    Ok((b, ct)) => println!(
                                        "      download id={aid}: {} bytes, content-type={ct}, starts={:?}",
                                        b.len(),
                                        String::from_utf8_lossy(&b[..b.len().min(5)])
                                    ),
                                    Err(e) => println!("      download id={aid}: ERROR {e:?}"),
                                }
                            }
                        }
                        if let Some(rid) = r["id"].as_i64() {
                            objects.push(format!("request-response/{rid}"));
                        }
                    }
                }
            }
            for (obj, kind) in objects
                .iter()
                .flat_map(|o| ["note-attachment", "note-text"].map(|k| (o.clone(), k)))
            {
                let kind = format!("{obj} {kind}");
                let path = format!(
                    "/v1/user/{uid}/monetary-account/{id}/{}",
                    kind.replace(' ', "/")
                );
                match c.get(&path).await {
                    Err(e) => println!("  {kind}: ERROR {e:?}"),
                    Ok(v) => {
                        let items = v["Response"].as_array().cloned().unwrap_or_default();
                        println!("  {kind}: {} item(s)", items.len());
                        for item in &items {
                            for (wrapper, note) in item.as_object().into_iter().flatten() {
                                println!(
                                    "    {wrapper}: id={} desc_len={} attachment={}",
                                    note["id"],
                                    note["description"].as_str().map_or(0, str::len),
                                    shape(&note["attachment"]),
                                );
                                for a in attachment_items(&note["attachment"]) {
                                    println!("      item: {}", shape(a));
                                    if let Some(aid) = a["id"].as_i64() {
                                        match c.attachment(aid).await {
                                            Ok((b, ct)) => println!(
                                                "      download id={aid}: {} bytes, content-type={ct}, starts={:?}",
                                                b.len(),
                                                String::from_utf8_lossy(&b[..b.len().min(5)])
                                            ),
                                            Err(e) => println!("      download id={aid}: ERROR {e:?}"),
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            let docs = c
                .receipts(id, &acc.external_ref, std::slice::from_ref(p))
                .await;
            println!("  GiroCrab receipts(): {} document(s)", docs.len());
            for d in &docs {
                println!(
                    "    {} | {} | booking={:?} | {} bytes",
                    d.mime_type,
                    d.file_name,
                    d.booking_external_id,
                    d.bytes.len()
                );
            }
        }
    }
    println!("\n{hits} payment(s) with amount {amount}");
}

/// Read-only: where do note files live for each kind of payment?
/// `BUNQ_API_KEY=… cargo test -q -p girocrab-core bunq_notes_survey -- --ignored --nocapture`
#[tokio::test]
#[ignore]
async fn bunq_notes_survey() {
    use crate::secret::SecretString;
    use std::collections::BTreeMap;
    let key = std::env::var("BUNQ_API_KEY").expect("BUNQ_API_KEY");
    let ctx = ConnectContext {
        connection_id: 1,
        login_name: String::new(),
        secret: SecretString::new(key),
        settings: Default::default(),
        state: Value::Null,
        since: "2020-01-01".into(),
        since_by_account: Default::default(),
    };
    let mut c = BunqConnector::new();
    c.begin(&ctx).await.unwrap();
    let since = (chrono::Local::now().date_naive() - chrono::Duration::days(RECEIPT_WINDOW_DAYS))
        .format("%Y-%m-%d")
        .to_string();
    let uid = c.state.user_id;
    let ts =
        |v: &Value| chrono::NaiveDateTime::parse_from_str(v.as_str()?, "%Y-%m-%d %H:%M:%S%.f").ok();
    let cents_of = |v: &Value| json_number_to_cents(v).map(i64::abs).unwrap_or(-1);
    // "ok:N" (N files) or "404" etc. per probed object
    async fn probe(c: &mut BunqConnector, path: String) -> (String, usize) {
        match c.get(&path).await {
            Ok(v) => {
                let n = v["Response"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|i| i.as_object().and_then(|o| o.values().next()))
                    .map(|note| attachment_items(&note["attachment"]).len())
                    .sum();
                (format!("ok:{n}"), n)
            }
            Err(e) => (format!("{}", e.status), 0),
        }
    }
    // kind -> (payments, outcome counts)
    let mut summary: BTreeMap<String, (usize, BTreeMap<String, usize>)> = BTreeMap::new();
    let mut with_files: Vec<(i64, String, Value)> = Vec::new();
    for (id, acc) in c.accounts().await.unwrap() {
        let payments = c.raw_payments(id, &since).await.unwrap();
        if payments.is_empty() {
            continue;
        }
        println!(
            "account {} ({id}): {} payments in {RECEIPT_WINDOW_DAYS} days",
            acc.kind.as_str(),
            payments.len()
        );
        let mut lists: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
        for (obj, wrapper) in [
            ("request-response", "RequestResponse"),
            ("request-inquiry", "RequestInquiry"),
        ] {
            let path = format!("/v1/user/{uid}/monetary-account/{id}/{obj}?count=200");
            let items = match c.get(&path).await {
                Ok(v) => v["Response"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|i| i[wrapper].clone())
                    .collect(),
                Err(e) => {
                    println!("  {obj} list: {}", e.status);
                    Vec::new()
                }
            };
            lists.insert(obj, items);
        }
        let actions = c.card_actions(id, &since).await.unwrap_or_default();
        lists.insert("mastercard-action", actions);
        for p in &payments {
            let pid = p["id"].as_i64().unwrap_or_default();
            let cents = json_number_to_cents(&p["amount"]["value"]).unwrap_or(0);
            let kind = format!(
                "{}/{}/{}",
                p["type"].as_str().unwrap_or("-"),
                p["sub_type"].as_str().unwrap_or("-"),
                if cents < 0 { "out" } else { "in" }
            );
            let mut outcome = Vec::new();
            let mut files = 0;
            let (st, n) = probe(
                &mut c,
                format!("/v1/user/{uid}/monetary-account/{id}/payment/{pid}/note-attachment"),
            )
            .await;
            outcome.push(format!("payment={st}"));
            files += n;
            if !p["attachment"].as_array().is_none_or(Vec::is_empty) {
                outcome.push("payment.attachment=yes".into());
                files += 1;
            }
            let paid = ts(&p["created"]);
            // related objects that may hold the app's notes
            let mut related: Vec<(String, i64)> = Vec::new();
            if p["sub_type"].as_str() == Some("REQUEST") {
                let obj = if cents < 0 {
                    "request-response"
                } else {
                    "request-inquiry"
                };
                let best = lists[obj]
                    .iter()
                    .filter(|r| {
                        cents_of(&r["amount_responded"]["value"]) == cents.abs()
                            || cents_of(&r["amount_inquired"]["value"]) == cents.abs()
                    })
                    .filter_map(|r| {
                        let gap = (ts(&r["time_responded"])? - paid?).num_seconds().abs();
                        (gap <= 120).then_some((gap, r["id"].as_i64()?))
                    })
                    .min();
                match best {
                    Some((_, rid)) => related.push((obj.into(), rid)),
                    None => outcome.push(format!("{obj}=nomatch")),
                }
            }
            if matches!(p["type"].as_str(), Some("MASTERCARD" | "MAESTRO" | "FIS")) {
                // same amount, action up to 10 days before the booking
                let cands: Vec<i64> = lists["mastercard-action"]
                    .iter()
                    .filter(|a| {
                        cents_of(&a["amount_billing"]["value"]) == cents.abs()
                            || cents_of(&a["amount_local"]["value"]) == cents.abs()
                    })
                    .filter(|a| match (ts(&a["created"]), paid) {
                        (Some(a), Some(p)) => (p - a).num_days().abs() <= 10,
                        _ => false,
                    })
                    .filter_map(|a| a["id"].as_i64())
                    .collect();
                if cands.is_empty() {
                    outcome.push("mastercard-action=nomatch".into());
                }
                if cands.len() > 1 {
                    outcome.push(format!("mastercard-action=ambiguous({})", cands.len()));
                }
                related.extend(
                    cands
                        .into_iter()
                        .take(2)
                        .map(|a| ("mastercard-action".to_string(), a)),
                );
            }
            if p["scheduled_id"].as_i64().is_some() {
                outcome.push("scheduled_id=yes".into());
            }
            if p["batch_id"].as_i64().is_some() {
                outcome.push("batch_id=yes".into());
            }
            for (obj, rid) in related {
                let (st, n) = probe(
                    &mut c,
                    format!("/v1/user/{uid}/monetary-account/{id}/{obj}/{rid}/note-attachment"),
                )
                .await;
                outcome.push(format!("{obj}={st}"));
                files += n;
            }
            let line = outcome.join(" ");
            let e = summary.entry(kind.clone()).or_default();
            e.0 += 1;
            *e.1.entry(line.clone()).or_default() += 1;
            if files > 0 {
                let date = p["created"]
                    .as_str()
                    .and_then(local_date)
                    .unwrap_or_default();
                println!(
                    "  FILES {kind} {date} {:.2} id={pid}: {line}",
                    cents as f64 / 100.0
                );
                with_files.push((id, acc.external_ref.clone(), p.clone()));
            }
        }
    }
    println!("\n== by payment kind (type/sub_type/direction) ==");
    for (kind, (n, outcomes)) in &summary {
        println!("{kind}: {n} payment(s)");
        for (o, k) in outcomes {
            println!("   {k:>3}x {o}");
        }
    }
    println!(
        "\n== GiroCrab receipts() on the {} payment(s) with files ==",
        with_files.len()
    );
    for (id, account_ref, p) in &with_files {
        let docs = c.receipts(*id, account_ref, std::slice::from_ref(p)).await;
        println!(
            "  {}/{} id={}: {} document(s) {:?}",
            p["type"].as_str().unwrap_or("-"),
            p["sub_type"].as_str().unwrap_or("-"),
            p["id"],
            docs.len(),
            docs.iter()
                .map(|d| (d.mime_type.clone(), d.bytes.len()))
                .collect::<Vec<_>>()
        );
    }
}

/// Read-only: for payments that refuse notes, which app event (object)
/// do they belong to, and does that object hold the notes?
/// `BUNQ_API_KEY=… cargo test -q -p girocrab-core bunq_refused_notes_survey -- --ignored --nocapture`
#[tokio::test]
#[ignore]
async fn bunq_refused_notes_survey() {
    use crate::secret::SecretString;
    let key = std::env::var("BUNQ_API_KEY").expect("BUNQ_API_KEY");
    let ctx = ConnectContext {
        connection_id: 1,
        login_name: String::new(),
        secret: SecretString::new(key),
        settings: Default::default(),
        state: Value::Null,
        since: "2020-01-01".into(),
        since_by_account: Default::default(),
    };
    let mut c = BunqConnector::new();
    c.begin(&ctx).await.unwrap();
    let since = (chrono::Local::now().date_naive() - chrono::Duration::days(RECEIPT_WINDOW_DAYS))
        .format("%Y-%m-%d")
        .to_string();
    let uid = c.state.user_id;
    let ts =
        |v: &Value| chrono::NaiveDateTime::parse_from_str(v.as_str()?, "%Y-%m-%d %H:%M:%S%.f").ok();
    // object type -> endpoint segment that takes notes
    let segment = |t: &str| match t {
        "Payment" => Some("payment"),
        "DraftPayment" => Some("draft-payment"),
        "PaymentBatch" => Some("payment-batch"),
        "RequestResponse" => Some("request-response"),
        "RequestInquiry" => Some("request-inquiry"),
        "MasterCardAction" => Some("mastercard-action"),
        "ScheduledPayment" => Some("schedule-payment"),
        "ScheduledPaymentBatch" => Some("schedule-payment-batch"),
        "IdealMerchantTransaction" => Some("ideal-merchant-transaction"),
        "SofortMerchantTransaction" => Some("sofort-merchant-transaction"),
        _ => None,
    };
    let keys = |v: &Value| {
        v.as_object()
            .map(|o| {
                o.keys()
                    .filter(|k| k.contains("id") || k.contains("payment") || k.contains("status"))
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default()
    };
    for (id, acc) in c.accounts().await.unwrap() {
        let payments = c.raw_payments(id, &since).await.unwrap();
        let mut refused = Vec::new();
        for p in &payments {
            if is_request_payment(p) {
                continue;
            }
            let pid = p["id"].as_i64().unwrap_or_default();
            let path =
                format!("/v1/user/{uid}/monetary-account/{id}/payment/{pid}/note-attachment");
            if let Err(e) = c.get(&path).await {
                refused.push((p.clone(), e.status));
            }
        }
        if refused.is_empty() {
            continue;
        }
        println!(
            "\naccount {} ({id}): {} payment(s) refuse notes",
            acc.kind.as_str(),
            refused.len()
        );
        let path = format!(
            "/v1/user/{uid}/event?monetary_account_id={id}&count=200&display_user_event=false"
        );
        let events: Vec<Value> = match c.get(&path).await {
            Ok(v) => v["Response"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|i| i["Event"].clone())
                .collect(),
            Err(e) => {
                println!("  event list: {}", e.status);
                Vec::new()
            }
        };
        for (p, status) in refused {
            let pid = p["id"].as_i64().unwrap_or_default();
            let cents = json_number_to_cents(&p["amount"]["value"]).unwrap_or(0);
            let paid = ts(&p["created"]);
            println!(
                "PAYMENT id={pid} {} {:.2} type={} sub_type={} status={status} payment_keys=[{}] scheduled_id={} batch_id={}",
                p["created"].as_str().and_then(local_date).unwrap_or_default(),
                cents as f64 / 100.0,
                p["type"].as_str().unwrap_or("-"),
                p["sub_type"].as_str().unwrap_or("-"),
                keys(&p),
                p["scheduled_id"], p["batch_id"],
            );
            for e in &events {
                let Some((t, obj)) = e["object"].as_object().and_then(|o| o.iter().next()) else {
                    continue;
                };
                let near = match (ts(&e["created"]), paid) {
                    (Some(a), Some(b)) => (a - b).num_minutes().abs() <= 3 * 24 * 60,
                    _ => false,
                };
                let same_id = obj["id"].as_i64() == Some(pid);
                let text = obj.to_string();
                let amount = format!("{:.2}", (cents.abs()) as f64 / 100.0);
                let mentions = text.contains(&pid.to_string())
                    || text.contains(&format!("\"{amount}\""))
                    || text.contains(&format!("\"-{amount}\""));
                if !(same_id || (near && mentions)) {
                    continue;
                }
                println!(
                    "  EVENT {} action={} type={t} obj_id={} same_id={same_id} mentions_pid={} keys=[{}]",
                    e["id"], e["action"], obj["id"], text.contains(&pid.to_string()), keys(obj),
                );
                if let (Some(seg), Some(oid)) = (segment(t), obj["id"].as_i64()) {
                    let path =
                        format!("/v1/user/{uid}/monetary-account/{id}/{seg}/{oid}/note-attachment");
                    match c.get(&path).await {
                        Ok(v) => {
                            let n: usize = v["Response"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(|i| i.as_object().and_then(|o| o.values().next()))
                                .map(|note| attachment_items(&note["attachment"]).len())
                                .sum();
                            println!("    {seg}/{oid} note-attachment: ok, {n} file(s)");
                        }
                        Err(e) => println!("    {seg}/{oid} note-attachment: {}", e.status),
                    }
                }
            }
        }
    }
}

/// Real account, real money (0.01 EUR): creates one draft payment to
/// `BUNQ_TO_IBAN` and waits up to 3 minutes for you to approve or reject
/// it in the bunq app, then checks how GiroCrab classifies the outcome.
/// Approve -> executed; reject -> plain error (not "Status unklar");
/// no decision -> the draft is withdrawn again. Never sends a second one.
/// `BUNQ_API_KEY=… BUNQ_TO_IBAN=… BUNQ_TO_NAME=… [BUNQ_FROM_IBAN=…] cargo test -q -p girocrab-core bunq_live_transfer -- --ignored --nocapture`
#[tokio::test]
#[ignore]
async fn bunq_live_transfer() {
    use crate::secret::SecretString;
    let key = std::env::var("BUNQ_API_KEY").expect("BUNQ_API_KEY");
    let to_iban = std::env::var("BUNQ_TO_IBAN").expect("BUNQ_TO_IBAN");
    let to_name = std::env::var("BUNQ_TO_NAME").expect("BUNQ_TO_NAME");
    let ctx = ConnectContext {
        connection_id: 1,
        login_name: String::new(),
        secret: SecretString::new(key),
        settings: Default::default(),
        state: Value::Null,
        since: "2020-01-01".into(),
        since_by_account: Default::default(),
    };
    let mut c = BunqConnector::new();
    assert!(matches!(c.begin(&ctx).await.unwrap(), Phase::Ready));
    let accounts = c.accounts().await.unwrap();
    let from = match std::env::var("BUNQ_FROM_IBAN") {
        Ok(iban) => iban.replace(' ', "").to_uppercase(),
        Err(_) => accounts[0].1.iban.clone(),
    };
    let from_acc = accounts
        .iter()
        .find(|(_, a)| a.iban.replace(' ', "").to_uppercase() == from)
        .map(|(_, a)| a.name.clone())
        .expect("BUNQ_FROM_IBAN is not one of your bunq accounts");
    let job = TransferJob {
        source_iban: from.clone(),
        source_owner: String::new(),
        recipient_name: to_name,
        recipient_iban: to_iban.replace(' ', "").to_uppercase(),
        recipient_bic: None,
        amount_cents: 1,
        purpose: "GiroCrab live test".into(),
        instant: false,
        execution_date: None,
        end_to_end_id: "GCLIVETEST".into(),
    };
    println!(
        "from {from_acc} ({from}) to {} ({}): 0.01 EUR",
        job.recipient_name, job.recipient_iban
    );
    let phase = c.send_transfer(&ctx, &job).await.unwrap();
    assert!(matches!(phase, Phase::Challenge(_)), "{phase:?}");
    assert!(c.order_submitted());
    println!(">>> Approve or reject the draft in the bunq app now (3 minutes) …");

    let mut outcome = None;
    for _ in 0..60 {
        tokio::time::sleep(Duration::from_secs(3)).await;
        match c.respond(&ctx, ChallengeResponse::Poll).await {
            Ok(Phase::Pending(_)) => continue,
            other => {
                outcome = Some(other);
                break;
            }
        }
    }
    match outcome {
        Some(Ok(Phase::Ready)) => {
            let r = c.transfer_result();
            println!("RESULT: {r:?}");
            assert_eq!(r, TransferResult::Executed { instant: false });
            assert!(c.draft.is_none());
        }
        Some(Err(e)) => {
            println!(
                "RESULT: error {e:?}, order_submitted={}",
                c.order_submitted()
            );
            assert!(
                matches!(e, CoreError::Bank { ref code, .. } if code == "draft"),
                "{e:?}"
            );
            assert!(!c.order_submitted(), "rejected must not be unclear");
        }
        Some(other) => panic!("unexpected: {other:?}"),
        None => {
            let d = c.draft.clone().unwrap();
            c.finish(&ctx).await;
            let status = c.draft_status(&d).await.unwrap();
            println!("RESULT: no decision, draft withdrawn, status {status}");
        }
    }
    c.finish(&ctx).await;
}

/// Lists PENDING drafts; with `BUNQ_DRAFT_ID` set, withdraws exactly that
/// draft (PUT status=REJECTED) and prints bunq's raw answer.
/// `BUNQ_API_KEY=… [BUNQ_DRAFT_ID=…] cargo test -q -p girocrab-core bunq_pending_drafts -- --ignored --nocapture`
#[tokio::test]
#[ignore]
async fn bunq_pending_drafts() {
    use crate::secret::SecretString;
    let key = std::env::var("BUNQ_API_KEY").expect("BUNQ_API_KEY");
    let target: Option<i64> = std::env::var("BUNQ_DRAFT_ID")
        .ok()
        .and_then(|v| v.trim().parse().ok());
    let ctx = ConnectContext {
        connection_id: 1,
        login_name: String::new(),
        secret: SecretString::new(key),
        settings: Default::default(),
        state: Value::Null,
        since: "2020-01-01".into(),
        since_by_account: Default::default(),
    };
    let mut c = BunqConnector::new();
    c.begin(&ctx).await.unwrap();
    let mut found = 0;
    for (id, acc) in c.accounts().await.unwrap() {
        let path = format!("{}?count=200", c.draft_path(id, None));
        let drafts: Vec<Value> = match c.get(&path).await {
            Ok(v) => v["Response"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|i| i["DraftPayment"].clone())
                .collect(),
            Err(e) => {
                println!(
                    "account {id}: draft list {} {} -> reading drafts from the event feed",
                    e.status, e.message
                );
                let path = format!(
                    "/v1/user/{}/event?monetary_account_id={id}&count=100&display_user_event=false",
                    c.state.user_id
                );
                let ids: BTreeSet<i64> = match c.get(&path).await {
                    Ok(v) => v["Response"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|i| i["Event"]["object"]["DraftPayment"]["id"].as_i64())
                        .collect(),
                    Err(e) => {
                        println!("  event list: {} {}", e.status, e.message);
                        BTreeSet::new()
                    }
                };
                let mut out = Vec::new();
                for did in ids.iter().rev().take(10) {
                    match c.get(&c.draft_path(id, Some(*did))).await {
                        Ok(v) => {
                            let d = find(&v, "DraftPayment").clone();
                            println!(
                                "  draft id={did} created={} status={}",
                                d["created"].as_str().unwrap_or("-"),
                                d["status"]
                            );
                            out.push(d);
                        }
                        Err(e) => println!("  draft id={did}: {} {}", e.status, e.message),
                    }
                }
                out
            }
        };
        for d in drafts
            .iter()
            .filter(|d| d["status"].as_str() == Some("PENDING"))
        {
            found += 1;
            let did = d["id"].as_i64().unwrap_or_default();
            let amount = d["entries"]
                .as_array()
                .and_then(|e| e.first())
                .map(|e| e["amount"]["value"].as_str().unwrap_or("?").to_string())
                .unwrap_or_else(|| "?".into());
            println!(
                "PENDING draft id={did} account={} ({id}) created={} amount={amount} keys=[{}]",
                acc.kind.as_str(),
                d["created"].as_str().unwrap_or("-"),
                d.as_object()
                    .map(|o| o.keys().cloned().collect::<Vec<_>>().join(","))
                    .unwrap_or_default(),
            );
            if target == Some(did) {
                let token = c.state.session_token.clone();
                let path = c.draft_path(id, Some(did));
                match c
                    .call(
                        reqwest::Method::PUT,
                        &path,
                        Some(&json!({"status": "REJECTED"})),
                        Some(&token),
                    )
                    .await
                {
                    Ok(v) => println!(
                        "  PUT REJECTED -> ok: {}",
                        v.to_string().chars().take(300).collect::<String>()
                    ),
                    Err(e) => println!("  PUT REJECTED -> ERROR {} {}", e.status, e.message),
                }
                let status = c
                    .get(&path)
                    .await
                    .map(|v| find(&v, "DraftPayment")["status"].to_string());
                println!("  status now: {:?}", status.map_err(|e| e.status));
            }
        }
    }
    println!(
        "\n{found} pending draft(s){}",
        if target.is_some() {
            ""
        } else {
            " (set BUNQ_DRAFT_ID=<id> to withdraw one)"
        }
    );
}

/// Privacy-safe outline of a bunq object for the live surveys: field names,
/// a few enumerated values, amount signs and dates relative to `now`.
fn survey_shape(o: &Value, now: chrono::NaiveDateTime, prefix: &str) -> String {
    const ENUMS: [&str; 9] = [
        "status",
        "sub_status",
        "type",
        "sub_type",
        "recurrence_unit",
        "recurrence_size",
        "action",
        "request_reference_split_the_bill",
        "eligible_whitelist_id",
    ];
    let Some(map) = o.as_object() else {
        return String::new();
    };
    let mut parts = Vec::new();
    for (k, v) in map {
        let name = format!("{prefix}{k}");
        let part = match v {
            Value::String(t) => {
                match chrono::NaiveDateTime::parse_from_str(t, "%Y-%m-%d %H:%M:%S%.f") {
                    Ok(t) => format!("{name}={:+}d", (t - now).num_days()),
                    Err(_) if ENUMS.contains(&k.as_str()) => format!("{name}={t}"),
                    Err(_) if t.is_empty() => format!("{name}=\"\""),
                    Err(_) => name,
                }
            }
            Value::Number(n) if ENUMS.contains(&k.as_str()) => format!("{name}={n}"),
            Value::Null => format!("{name}=null"),
            Value::Object(m) if m.contains_key("value") && m.contains_key("currency") => {
                match json_number_to_cents(&v["value"]) {
                    Ok(c) if c < 0 => format!("{name}=neg"),
                    Ok(0) => format!("{name}=zero"),
                    Ok(_) => format!("{name}=pos"),
                    Err(_) => format!("{name}=?"),
                }
            }
            // One level deeper for the nested payment/schedule of a ScheduledPayment.
            Value::Object(_)
                if prefix.is_empty() && matches!(k.as_str(), "payment" | "schedule") =>
            {
                format!("{name}{{{}}}", survey_shape(v, now, ""))
            }
            _ => name,
        };
        parts.push(part);
    }
    parts.join(" ")
}

// ---- Draft withdrawal (cancel) against a scripted local server ----

/// Serves one scripted reply per connection (`None` = drop the connection
/// without answering). Returns the base URL and the request lines seen.
async fn scripted_server(
    replies: Vec<Option<(u16, &'static str)>>,
) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        for reply in replies {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let mut buf = vec![0u8; 8192];
            let n = sock.read(&mut buf).await.unwrap_or(0);
            let head = String::from_utf8_lossy(&buf[..n]);
            log.lock()
                .unwrap()
                .push(head.lines().next().unwrap_or_default().to_string());
            if let Some((code, body)) = reply {
                let resp = format!(
                    "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            }
            // Dropping the socket closes it (also the "disconnect" case).
        }
    });
    (base, seen)
}

const REJECTED: &str = r#"{"Response":[{"DraftPayment":{"status":"REJECTED"}}]}"#;
const PENDING: &str = r#"{"Response":[{"DraftPayment":{"status":"PENDING"}}]}"#;
const ACCEPTED: &str = r#"{"Response":[{"DraftPayment":{"status":"ACCEPTED"}}]}"#;
const HTTP_ERR: &str = r#"{"Error":[{"error_description":"nope"}]}"#;

fn connector_with_draft(base: String) -> BunqConnector {
    let mut c = BunqConnector::new();
    c.client = Some(http_client().unwrap());
    c.base = base;
    c.state.user_id = 1;
    c.order_submitted = true;
    c.draft = Some(PendingDraft {
        account_id: 5,
        draft_id: 9,
        challenge: Challenge {
            kind: ChallengeKind::DecoupledPush,
            title: "t".into(),
            message: "m".into(),
            method_name: "bunq".into(),
            redirect_url: None,
            image: None,
            expires_in_seconds: None,
            poll_interval_seconds: 3,
        },
        poll_failures: 0,
    });
    c
}

#[tokio::test]
async fn confirmed_draft_rejection_is_cancelled() {
    let (base, seen) = scripted_server(vec![Some((200, "{}")), Some((200, REJECTED))]).await;
    let mut c = connector_with_draft(base);
    assert_eq!(c.withdraw_draft().await, CancelOutcome::Cancelled);
    assert!(c.draft.is_none() && !c.has_open_order() && !c.order_submitted());
    {
        let seen = seen.lock().unwrap();
        assert!(seen[0].starts_with("PUT ") && seen[0].contains("/draft-payment/9"));
        assert!(seen[1].starts_with("GET "));
    }
    // Nothing left to withdraw: no further request.
    assert_eq!(c.withdraw_draft().await, CancelOutcome::Cancelled);
}

#[tokio::test]
async fn http_error_on_rejection_keeps_the_draft() {
    let (base, _) = scripted_server(vec![Some((400, HTTP_ERR)), Some((200, PENDING))]).await;
    let mut c = connector_with_draft(base);
    let out = c.withdraw_draft().await;
    assert!(
        matches!(out, CancelOutcome::CancellationFailed { .. }),
        "{out:?}"
    );
    assert!(c.draft.is_some() && c.has_open_order());
    assert!(c.order_submitted(), "still unclear, never plain failed");
}

#[tokio::test]
async fn disconnect_on_rejection_is_unknown_and_retryable() {
    let (base, _) =
        scripted_server(vec![None, None, Some((200, "{}")), Some((200, REJECTED))]).await;
    let mut c = connector_with_draft(base);
    let out = c.withdraw_draft().await;
    assert!(
        matches!(out, CancelOutcome::CancellationUnknown { .. }),
        "{out:?}"
    );
    assert!(c.draft.is_some());
    // Retry re-sends the rejection and re-checks the status.
    assert_eq!(c.withdraw_draft().await, CancelOutcome::Cancelled);
    assert!(c.draft.is_none());
}

#[tokio::test]
async fn status_still_open_after_rejection_is_pending_then_confirmed() {
    let (base, _) = scripted_server(vec![
        Some((200, "{}")),
        Some((200, PENDING)),
        Some((200, "{}")),
        Some((200, REJECTED)),
    ])
    .await;
    let mut c = connector_with_draft(base);
    let out = c.withdraw_draft().await;
    assert!(
        matches!(out, CancelOutcome::CancellationPending { .. }),
        "{out:?}"
    );
    assert!(c.draft.is_some());
    assert_eq!(c.withdraw_draft().await, CancelOutcome::Cancelled);
}

#[tokio::test]
async fn already_rejected_draft_is_cancelled_despite_http_error() {
    let (base, _) = scripted_server(vec![Some((400, HTTP_ERR)), Some((200, REJECTED))]).await;
    let mut c = connector_with_draft(base);
    assert_eq!(c.withdraw_draft().await, CancelOutcome::Cancelled);
    assert!(c.draft.is_none());
}

#[tokio::test]
async fn already_approved_draft_cannot_be_cancelled() {
    let (base, _) = scripted_server(vec![Some((400, HTTP_ERR)), Some((200, ACCEPTED))]).await;
    let mut c = connector_with_draft(base);
    let out = c.withdraw_draft().await;
    assert!(
        matches!(out, CancelOutcome::CancellationFailed { .. }),
        "{out:?}"
    );
    assert!(c.draft.is_none());
    assert_eq!(
        c.transfer_result(),
        TransferResult::Executed { instant: false }
    );
}
