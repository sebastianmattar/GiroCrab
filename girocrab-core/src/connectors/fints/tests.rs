use super::accounts::*;
use super::bpd::*;
use super::debit::*;
use super::statements::*;
use super::transfer::*;
use super::*;
use crate::secret::SecretString;

#[tokio::test]
async fn rah10_without_keyfile_never_falls_back_to_pin_tan() {
    let mut connector = FinTsConnector::for_key("fints:12345678:rah10");
    let mut ctx = test_ctx();
    // Even a valid-looking PIN/TAN URL must not enable a downgrade.
    ctx.settings.insert(
        "fints.url.12345678".into(),
        SecretString::new("https://example.invalid/fints".into()),
    );
    assert!(matches!(
        connector.begin(&ctx).await,
        Err(CoreError::MissingSetting { .. })
    ));
    assert!(connector.client.is_none());
    assert!(connector.rah_stream.is_none());
    assert_eq!(
        crate::models::ConnectorKind::fints_blz("fints:12345678:rah10"),
        Some("12345678")
    );
    assert_eq!(
        crate::models::ConnectorKind::fints_blz("fints:12345678:unknown"),
        None
    );
}

fn test_ctx() -> ConnectContext {
    ConnectContext {
        connection_id: 1,
        login_name: "user:1".into(),
        secret: SecretString::new("12345".into()),
        state: serde_json::json!({}).into(),
        since: "2025-01-01".into(),
        ..Default::default()
    }
}

/// Mock FinTS endpoint: answers the n-th request with `replies[n]` (last one repeats)
/// and records which segment ids each request contained.
fn mock_bank(
    replies: Vec<&'static str>,
) -> (String, std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://127.0.0.1:{}/fints",
        listener.local_addr().unwrap().port()
    );
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    std::thread::spawn(move || {
        for (n, stream) in listener.incoming().enumerate() {
            let Ok(mut s) = stream else { break };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
            let body = loop {
                let k = s.read(&mut chunk).unwrap_or(0);
                buf.extend_from_slice(&chunk[..k]);
                let text = String::from_utf8_lossy(&buf).to_string();
                if let Some(i) = text.find("\r\n\r\n") {
                    let len = text[..i]
                        .to_ascii_lowercase()
                        .split("content-length:")
                        .nth(1)
                        .and_then(|v| v.lines().next()?.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if buf.len() >= i + 4 + len || k == 0 {
                        break text[i + 4..].to_string();
                    }
                } else if k == 0 {
                    break String::new();
                }
            };
            let raw = base64::engine::general_purpose::STANDARD
                .decode(body.trim())
                .unwrap_or_default();
            let ids = syntax::parse_response(&raw)
                .map(|v| v.iter().map(|x| x.id().to_string()).collect())
                .unwrap_or_default();
            seen2.lock().unwrap().push(ids);
            let reply = replies[n.min(replies.len() - 1)];
            let b64 = base64::engine::general_purpose::STANDARD.encode(reply);
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b64}",
                b64.len()
            );
            let _ = s.write_all(resp.as_bytes());
        }
    });
    (url, seen)
}

const REJECT_9010: &str = "HNHBK:1:3+000000000000+300+0+1'HIRMG:2:2+9800::Der Dialog wurde abgebrochen.'HIRMS:3:2:3+9010::Ungültiger Signaturaufbau: Fehler im Segmentaufbau.'";
const ACCEPT: &str =
    "HNHBK:1:3+000000000000+300+DIALOG1+1'HIRMG:2:2+0010::Nachricht entgegengenommen.'";
const STALE_SYSTEM_ID: &str = "HNHBK:1:3+000000000000+300+0+1'HIRMG:2:2+9800::abgebrochen.'HIRMS:3:2:3+9931::Anmeldename falsch.'HIRMS:4:2:3+9010::Fehler.'";

fn two_step_connector(url: &str, state: serde_json::Value) -> FinTsConnector {
    let mut c = FinTsConnector::new("50010517");
    c.url = url.into();
    c.product_id = "TESTPRODUCT".into();
    c.product_version = "0.1".into();
    c.st = serde_json::from_value(state).unwrap();
    c
}

fn has_hktan(ids: &[String]) -> bool {
    ids.iter().any(|i| i == "HKTAN")
}

/// comdirect's answer when the bank code does not belong to the login.
const WRONG_BLZ: &str = "HNHBK:1:3+000000000000+300+0+1'HIRMG:2:2+9050::Die Nachricht enthaelt Fehler.+9800::Dialog abgebrochen'HIRMS:3:2:3+9210::Bankleitzahl passt nicht zum Benutzer.+9340::Ungueltige Auftragsnachricht: Ungueltige Signatur.'";

fn comdirect_connector(key_blz: &str, url: &str, state: serde_json::Value) -> FinTsConnector {
    let mut c = two_step_connector(url, state);
    c.blz = key_blz.into();
    c
}

/// Synchronised one-step state: the dialog init is the first request.
fn synced() -> serde_json::Value {
    serde_json::json!({"system_id": "123", "sec_func": "999", "tan_method_name": "", "tan_medium": "", "versions": {}, "tan_required": {}})
}

#[test]
fn wrong_bank_code_is_told_apart_from_other_9210() {
    let r = |raw: &str| reply(raw.as_bytes());
    assert!(super::dialog::blz_mismatch(&r(WRONG_BLZ)));
    for other in [
        "HIRMG:2:2+9050::Fehler.'HIRMS:3:2:3+9210::Auftrag abgelehnt.'",
        "HIRMG:2:2+9050::Fehler.'HIRMS:3:2:3+9340::Bankleitzahl ungueltig.'",
    ] {
        assert!(!super::dialog::blz_mismatch(&r(other)), "{other}");
    }
}

#[tokio::test]
async fn comdirect_login_tries_its_other_bank_codes_and_keeps_the_one_that_works() {
    // IBAN code 20041177, the login is known under 20041111 (the first one tried)
    let (url, seen) = mock_bank(vec![WRONG_BLZ, ACCEPT]);
    let mut c = comdirect_connector("20041177", &url, synced());
    assert!(matches!(c.login(&test_ctx()).await.unwrap(), Phase::Ready));
    assert_eq!(seen.lock().unwrap().len(), 2);
    assert_eq!(c.blz, "20041111");
    let persisted = serde_json::to_value(&c.st).unwrap();
    assert_eq!(persisted["login_blz"], "20041111");

    // the next sync logs in under the found code straight away
    let (url2, seen2) = mock_bank(vec![ACCEPT]);
    let mut c2 = FinTsConnector::for_key("fints:20041177");
    let mut ctx = test_ctx();
    for (k, v) in [
        ("fints.url.20041177", url2.as_str()),
        ("fints.product_id", "TESTPRODUCT"),
    ] {
        ctx.settings.insert(k.into(), SecretString::new(v.into()));
    }
    ctx.state = persisted.into();
    assert!(matches!(c2.begin(&ctx).await.unwrap(), Phase::Ready));
    assert_eq!(c2.blz, "20041111");
    assert_eq!(seen2.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn comdirect_login_gives_up_with_the_first_error_when_no_code_fits() {
    let (url, seen) = mock_bank(vec![WRONG_BLZ]);
    let mut c = comdirect_connector("20041177", &url, synced());
    let err = c.login(&test_ctx()).await.unwrap_err();
    assert!(
        err.to_string().contains("Bankleitzahl passt nicht"),
        "{err}"
    );
    // every comdirect code once, then back to the connector's
    assert_eq!(
        seen.lock().unwrap().len(),
        crate::connectors::fints_directory::COMDIRECT_BLZ.len()
    );
    assert_eq!(c.blz, "20041177");
    assert!(c.st.login_blz.is_empty());
}

#[tokio::test]
async fn other_banks_and_other_errors_are_not_retried() {
    // not comdirect: the bank code is the bank's, nothing to try
    let (url, seen) = mock_bank(vec![WRONG_BLZ, ACCEPT]);
    let mut c = comdirect_connector("50010517", &url, synced());
    assert!(c.login(&test_ctx()).await.is_err());
    assert_eq!(seen.lock().unwrap().len(), 1);
    // comdirect, but a wrong PIN: trying other codes would only repeat it
    let (url, seen) = mock_bank(vec![STALE_SYSTEM_ID, ACCEPT]);
    let mut c = comdirect_connector("20041177", &url, synced());
    assert!(c.login(&test_ctx()).await.is_err());
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert_eq!(c.blz, "20041177");
}

#[tokio::test]
async fn dialog_init_9010_falls_back_to_single_step_and_persists() {
    let (url, seen) = mock_bank(vec![REJECT_9010, ACCEPT]);
    let state = serde_json::json!({"system_id": "123", "sec_func": "946", "tan_method_name": "", "tan_medium": "", "versions": {}, "tan_required": {}});
    let mut c = two_step_connector(&url, state);
    let phase = c.open_dialog(&test_ctx()).await.unwrap();
    assert!(matches!(phase, Phase::Ready));
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(has_hktan(&seen[0]), "first try is two-step: {:?}", seen[0]);
        assert!(!has_hktan(&seen[1]), "retry is one-step: {:?}", seen[1]);
    }
    assert!(c.st.single_step);
    let persisted = serde_json::to_value(&c.st).unwrap();
    assert_eq!(persisted["single_step"], true);

    // Next sync starts directly one-step.
    let (url2, seen2) = mock_bank(vec![ACCEPT]);
    let mut c2 = two_step_connector(&url2, persisted);
    assert_eq!(c2.sec(), "999");
    c2.open_dialog(&test_ctx()).await.unwrap();
    let seen2 = seen2.lock().unwrap();
    assert_eq!(seen2.len(), 1);
    assert!(!has_hktan(&seen2[0]), "{:?}", seen2[0]);
}

#[tokio::test]
async fn pending_receipts_are_sent_once_and_rejected_ones_dropped() {
    let state = serde_json::json!({"system_id": "123", "sec_func": "999", "tan_method_name": "", "tan_medium": "", "versions": {"HKQTG": [1]}, "tan_required": {}, "pending_receipts": ["0102", "0304"]});
    const REJECT: &str = "HNHBK:1:3+000000000000+300+DIALOG1+1'HIRMG:2:2+9050::Teilweise fehlerhaft.'HIRMS:3:2:3+9210::Quittung unbekannt.'";
    let (url, seen) = mock_bank(vec![ACCEPT, REJECT]);
    let mut c = two_step_connector(&url, state);
    c.send_pending_receipts(&test_ctx()).await;
    assert!(
        c.st.pending_receipts.is_empty(),
        "{:?}",
        c.st.pending_receipts
    );
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert!(
        seen.iter().all(|ids| ids.iter().any(|i| i == "HKQTG")),
        "{seen:?}"
    );
}

#[tokio::test]
async fn receipts_wait_when_the_bank_offers_no_hkqtg() {
    let state = serde_json::json!({"system_id": "123", "sec_func": "999", "tan_method_name": "", "tan_medium": "", "versions": {}, "tan_required": {}, "pending_receipts": ["0102"]});
    let (url, seen) = mock_bank(vec![ACCEPT]);
    let mut c = two_step_connector(&url, state);
    c.send_pending_receipts(&test_ctx()).await;
    assert_eq!(c.st.pending_receipts, vec!["0102".to_string()]);
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn dialog_init_9010_with_stale_system_id_does_not_fall_back() {
    let (url, seen) = mock_bank(vec![STALE_SYSTEM_ID]);
    let state = serde_json::json!({"system_id": "123", "sec_func": "946", "tan_method_name": "", "tan_medium": "", "versions": {}, "tan_required": {}});
    let mut c = two_step_connector(&url, state);
    assert!(c.open_dialog(&test_ctx()).await.is_err());
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert!(!c.st.single_step);
    assert!(c.st.system_id.is_empty());
}

#[tokio::test]
async fn failed_single_step_retry_is_not_remembered() {
    let (url, seen) = mock_bank(vec![REJECT_9010]);
    let state = serde_json::json!({"system_id": "123", "sec_func": "946", "tan_method_name": "", "tan_medium": "", "versions": {}, "tan_required": {}});
    let mut c = two_step_connector(&url, state);
    assert!(c.open_dialog(&test_ctx()).await.is_err());
    assert_eq!(seen.lock().unwrap().len(), 2);
    assert!(!c.st.single_step);
}

#[test]
fn secure_url_check_rejects_plain_http() {
    assert!(crate::connectors::ensure_secure_fints_url("https://bank.example/fints").is_ok());
    assert!(crate::connectors::ensure_secure_fints_url("http://bank.example/fints").is_err());
    assert!(crate::connectors::ensure_secure_fints_url("ftp://bank.example").is_err());
    assert!(crate::connectors::ensure_secure_fints_url("").is_err());
}

#[tokio::test]
async fn send_rejects_http_url() {
    let mut c = FinTsConnector::new("52090000");
    c.url = "http://bank.example/fints".into();
    let r = c.send(&test_ctx(), vec![], "999", None).await;
    assert!(matches!(r, Err(CoreError::Protocol { .. })));
}

#[tokio::test]
async fn send_does_not_follow_redirects() {
    use std::io::{Read, Write};
    for code in [301, 302, 307, 308] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let h2 = hits.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { break };
                h2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut buf = [0u8; 8192];
                let _ = s.read(&mut buf);
                let resp = format!(
                    "HTTP/1.1 {code} Moved\r\nLocation: http://127.0.0.1:{port}/other\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = s.write_all(resp.as_bytes());
            }
        });
        let mut c = FinTsConnector::new("52090000");
        c.url = format!("http://127.0.0.1:{port}/fints");
        let r = c.send(&test_ctx(), vec![], "999", None).await;
        assert!(
            matches!(r, Err(CoreError::Network { .. })),
            "code {code}: {r:?}"
        );
        assert_eq!(
            hits.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "code {code}"
        );
    }
}

#[test]
fn message_has_correct_length_and_structure() {
    let mut c = FinTsConnector::new("52090000");
    c.product_id = "TESTPRODUCT".into();
    c.product_version = "0.1".into();
    let ctx = ConnectContext {
        connection_id: 1,
        login_name: "user:1".into(),
        secret: SecretString::new("12345".into()),
        state: serde_json::json!({}).into(),
        since: "2025-01-01".into(),
        ..Default::default()
    };
    let payload = vec![c.hkidn(&ctx, 3), c.hkvvb(4), c.hktan_process4(5, "HKIDN")];
    let msg = c.build_message(&ctx, &payload, "946", None);
    let header_len: usize = std::str::from_utf8(&msg[10..22]).unwrap().parse().unwrap();
    assert_eq!(header_len, msg.len());
    let segs = syntax::parse_response(&msg).unwrap();
    let ids: Vec<&str> = segs.iter().map(|s| s.id()).collect();
    assert_eq!(
        ids,
        vec![
            "HNHBK", "HNVSK", "HNSHK", "HKIDN", "HKVVB", "HKTAN", "HNSHA", "HNHBS"
        ]
    );
    assert_eq!(segs[3].get(2, 0), "user:1", "login is escaped and restored");
    assert_eq!(segs[6].get(3, 0), "12345");
    assert_eq!(segs[7].number(), 7);
    // Two-step method: security profile version 2 in HNVSK and HNSHK.
    assert_eq!((segs[1].get(1, 0), segs[1].get(1, 1)), ("PIN", "2"));
    assert_eq!((segs[2].get(1, 0), segs[2].get(1, 1)), ("PIN", "2"));

    // One-step (999, synchronisation dialog): profile version 1. Consorsbank
    // answers PIN:2 here with "9010 Ungültiger Signaturaufbau".
    let sync = vec![c.hkidn(&ctx, 3), c.hkvvb(4)];
    let msg = c.build_message(&ctx, &sync, "999", None);
    let segs = syntax::parse_response(&msg).unwrap();
    assert_eq!((segs[1].id(), segs[1].get(1, 1)), ("HNVSK", "1"));
    assert_eq!((segs[2].id(), segs[2].get(1, 1)), ("HNSHK", "1"));
    assert_eq!(segs[2].get(2, 0), "999");
}

#[test]
fn card_request_starts_within_the_storage_period() {
    // 775 days kept: two years back is fine, three years is cut to the limit
    assert_eq!(card_since("2024-11-15", "2026-11-15", 775), "2024-11-15");
    assert_eq!(card_since("2023-11-15", "2026-11-15", 775), "2024-10-01");
    // unknown storage period: unchanged
    assert_eq!(card_since("2020-01-01", "2026-11-15", 0), "2020-01-01");
    // a hostile HIKKUS value must not panic
    assert_eq!(
        card_since("2020-01-01", "2026-11-15", i64::MAX),
        "2020-01-01"
    );
}

#[test]
fn decoupled_poll_recognises_a_pending_approval() {
    // standard: 3956
    assert!(approval_pending(&reply(
        b"HIRMG:2:2+3060::Bitte beachten Sie die enthaltenen Warnungen.'HIRMS:3:2:3+3956::Starke Kundenauthentifizierung noch ausstehend.'"
    )));
    // 3045: SEPA-Instant handover after approval is no longer pending (#1121)
    assert!(!approval_pending(&reply(
        b"HIRMG:2:2+3060::Bitte beachten Sie die enthaltenen Warnungen.'HIRMS:3:2:3+3045::SEPA-Instant Payment Statusabfrage HKIPS veranlassen.'"
    )));
    // warnings only, no success code (Deutsche Bank)
    assert!(approval_pending(&reply(
        b"HIRMG:2:2+3060::Teilweise liegen Warnungen vor.'HIRMS:3:2:3+3060::Hinweis.'"
    )));
    // approved: success code
    assert!(!approval_pending(&reply(
        b"HIRMG:2:2+0010::Nachricht entgegengenommen.'HIRMS:3:2:3+0020::Auftrag ausgef\xfchrt.'"
    )));
    // rejected: an error ends the wait
    assert!(!approval_pending(&reply(
        b"HIRMG:2:2+9050::Teilweise fehlerhaft.'HIRMS:3:2:3+9210::Keine Daten.'"
    )));
}

#[test]
fn so_prefix_of_a_tan_medium_is_recognised() {
    assert!(has_so_prefix("SO:Mein Gerät") && has_so_prefix("so:x"));
    assert!(!has_so_prefix("MH AIR") && !has_so_prefix("SO") && !has_so_prefix("Ää"));
}

#[test]
fn consorsbank_statements_always_carry_hktan() {
    let mut c = FinTsConnector::new("76030080");
    c.st.tan_required.insert("HKKAZ".into(), false);
    assert!(c.needs_hktan("HKKAZ") && c.needs_hktan("HKSAL"));
    assert!(!c.needs_hktan("HKSPA"));
    // Other banks: HIPINS decides.
    let mut c = FinTsConnector::new("52090000");
    c.st.tan_required.insert("HKKAZ".into(), false);
    c.st.tan_required.insert("HKSAL".into(), true);
    assert!(!c.needs_hktan("HKKAZ") && c.needs_hktan("HKSAL"));
}

#[test]
fn dkb_statements_go_through_hkekp_when_hkeka_is_missing() {
    // DKB (#449): the BPD offer HKEKP v1/v2 (PDF statement) but no HKEKA.
    let mut c = FinTsConnector::new("12030000");
    assert_eq!(c.statement_tx(), None);
    c.learn_bpd(&reply(
        b"HIEKPS:30:1:3+1+1+0+J:J:N'HIEKPS:31:2:3+1+1+0+J:J:N'",
    ));
    assert_eq!(c.statement_tx(), Some("HKEKP"));
    assert_eq!(c.version("HKEKP"), 2);
    // A bank offering both keeps the format-based HKEKA.
    c.learn_bpd(&reply(b"HIEKAS:32:5:3+1+1+0+J:J:N:3'"));
    assert_eq!(c.statement_tx(), Some("HKEKA"));
}

#[test]
fn dkkku_is_learned_from_dikkus_and_hipins() {
    // comdirect (#807): card bookings only as the bank-specific DKKKU v2.
    let mut c = FinTsConnector::new("20041155");
    assert!(!c.bank_supports("DKKKU"));
    c.learn_bpd(&reply(
        b"DIKKUS:40:2:3+1+1+0+400:J:J:J'HIPINS:41:1:3+1+1+0+:::::HKSAL:N:DKKKU:J:HKKKU:N'",
    ));
    assert!(c.bank_supports("DKKKU"));
    assert_eq!(c.version("DKKKU"), 2);
    assert!(!c.bank_supports("HKKKU"), "only the DK variant is offered");
    assert!(c.needs_hktan("DKKKU"));
    assert!(!c.needs_hktan("HKKKU"));
    // DIKKUS is read like HIKKUS when the values fit
    assert_eq!(c.st.kku_storage_days, 400);
    assert!(!c.st.kku_no_date_range && !c.st.kku_no_account);
}

#[test]
fn card_tx_prefers_hkkku_and_needs_bank_and_account() {
    let mut c = FinTsConnector::new("20041155");
    let card = upd_acc("5232000011112222", "", "", "Visa", &["DKKKU", "DKKKS"]);
    assert_eq!(c.card_tx(Some(&card)), None, "bank offers nothing yet");
    c.learn_bpd(&reply(b"DIKKUS:40:2:3+1+1+0+400:J:J:J'"));
    assert_eq!(c.card_tx(Some(&card)), Some("DKKKU"));
    let giro = upd_acc("1234567", "", IBAN_A, "Girokonto", &["HKSAL", "HKCAZ"]);
    assert_eq!(c.card_tx(Some(&giro)), None, "not allowed for this account");
    assert_eq!(c.card_tx(None), None);
    c.learn_bpd(&reply(b"HIKKUS:42:1:3+1+1+0+775:N:J:J'"));
    let both = upd_acc("5232000011112222", "", "", "Visa", &["DKKKU", "HKKKU"]);
    assert_eq!(c.card_tx(Some(&both)), Some("HKKKU"));
}

#[test]
fn builds_dkkku_with_ktv_and_hkkku_with_kti() {
    let acc = sepa_acc("", "5232000011112222");
    assert_eq!(
        FinTsConnector::card_request("DKKKU", 2, &acc, true, "20260901", "20261117", None),
        b"DKKKU:3:2+5232000011112222::280:20041155+5232000011112222++20260901+20261117'".to_vec()
    );
    assert_eq!(
        FinTsConnector::card_request("HKKKU", 1, &acc, true, "", "", Some("A:1")),
        b"HKKKU:3:1+::5232000011112222::280:20041155+5232000011112222+++++A?:1'".to_vec()
    );
    assert_eq!(
        FinTsConnector::card_request("DKKKU", 2, &acc, false, "", "", None),
        b"DKKKU:3:2++5232000011112222'".to_vec()
    );
}

#[test]
fn card_account_with_dkkku_only_is_usable() {
    // comdirect: Kontoart 1, no IBAN, only DKKKU/DKKKS/DKKAU besides admin ones.
    let upd = [upd_acc(
        "5232000011112222",
        "",
        "",
        "Visa",
        &["DKKAU", "DKKKS", "DKKKU", "HKSPA", "HKTAN"],
    )];
    let out = merge_upd_with_sepa(&upd, Vec::new(), "20041155");
    assert_eq!(out.len(), 1);
    assert_eq!(account_label(&out[0].0), "5232000011112222");
}

#[test]
fn hiekp_file_name_is_taken_from_the_bank() {
    let r = reply(b"HIEKP:5:2:4+@8@%PDF-1.7+20261031+Kontoauszug_10_2026.pdf+7'");
    let s = r.all("HIEKP").next().unwrap();
    assert_eq!(
        statement_file_name(s).as_deref(),
        Some("Kontoauszug_10_2026.pdf")
    );
    assert_eq!(
        statement_title("Kontoauszug_10_2026_vom_05.10.2026_zu_Konto_1234.pdf"),
        "Kontoauszug 10 2026 vom 05.10.2026 zu Konto 1234"
    );
    // the debug layout shows kinds and lengths only, no content
    assert_eq!(segment_layout(s), "bin(8) date8 pdfname(23) num1");
    let none = reply(b"HIEKP:5:2:4+@8@%PDF-1.7+20261031'");
    assert_eq!(statement_file_name(none.all("HIEKP").next().unwrap()), None);
    let path = reply(b"HIEKP:5:2:4+@8@%PDF-1.7+x/y/Auszug.PDF'");
    assert_eq!(
        statement_file_name(path.all("HIEKP").next().unwrap()).as_deref(),
        Some("Auszug.PDF")
    );
}

#[test]
fn receipt_code_is_the_non_pdf_binary_field() {
    let r = reply(b"HIEKP:5:2:4+@8@%PDF-1.7+20261105:20261105+20261105+2026+11+DE02+BIC++++Auszug.pdf+@4@\x01\xab\x00Z'");
    let s = r.all("HIEKP").next().unwrap();
    let code = receipt_code(s).unwrap();
    assert_eq!(code, "01ab005a");
    assert_eq!(hex_decode(&code).unwrap(), vec![0x01, 0xab, 0x00, 0x5a]);
    // no receipt field: nothing to acknowledge
    let r = reply(b"HIEKP:5:2:4+@8@%PDF-1.7+20261105:20261105'");
    assert_eq!(receipt_code(r.all("HIEKP").next().unwrap()), None);
    assert_eq!(hex_decode("abc"), None);
    // state saved by an older version (no field) still loads
    let st: FinTsState = serde_json::from_value(serde_json::json!({"system_id": "1", "sec_func": "999", "tan_method_name": "", "tan_medium": "", "versions": {}, "tan_required": {}})).unwrap();
    assert!(st.pending_receipts.is_empty());
}

#[test]
fn hiekp_details_follow_the_dkb_layout() {
    // DKB HIEKP v2: PDF, period, creation date, year, number, IBAN, BIC, …, name, receipt
    let r = reply(
        b"HIEKP:5:2:4+@8@%PDF-1.7+20261001:20261031+20261103+2026+10+DE02120300000000202051+BYLADEM1001++++Kontoauszug_10_2026.pdf+@4@abcd'",
    );
    let s = r.all("HIEKP").next().unwrap();
    assert_eq!(
        hiekp_details(s),
        (
            "10/2026 (2026-10-01–2026-10-31)".to_string(),
            "2026-10-01–2026-10-31".to_string(),
            "2026-10-31".to_string()
        )
    );
    // the same date twice (DKB) is shown once
    let r = reply(b"HIEKP:5:2:4+@8@%PDF-1.7+20261105:20261105+20261105+2026+11'");
    assert_eq!(
        hiekp_details(r.all("HIEKP").next().unwrap()).0,
        "11/2026 (2026-11-05)"
    );
    // a leading zero in the number is dropped, the creation date never widens the period
    let r = reply(b"HIEKP:5:2:4+@8@%PDF-1.7+20260101:20260131+20260203+2026+01'");
    assert_eq!(
        hiekp_details(r.all("HIEKP").next().unwrap()).0,
        "1/2026 (2026-01-01–2026-01-31)"
    );
}

#[test]
fn hiekp_period_comes_from_its_dates() {
    let r = reply(b"HIEKP:5:2:4+@8@%PDF-1.7+20261001:20261031+20261102'");
    let s = r.all("HIEKP").next().unwrap();
    assert_eq!(
        statement_period(s),
        (
            "2026-10-01–2026-11-02".to_string(),
            "2026-11-02".to_string()
        )
    );
}

fn reply(raw: &[u8]) -> BankReply {
    let segments = syntax::parse_response(raw).unwrap();
    let mut codes = Vec::new();
    for s in &segments {
        if s.id() == "HIRMG" || s.id() == "HIRMS" {
            for el in s.elements.iter().skip(1) {
                let t = |i: usize| el.get(i).map(|v| v.text().to_string()).unwrap_or_default();
                codes.push((
                    t(0),
                    s.reference(),
                    t(2),
                    el.iter().skip(3).map(|v| v.text().to_string()).collect(),
                ));
            }
        }
    }
    BankReply {
        segments,
        codes,
        sent: HashMap::from([(3, "HKKAZ".to_string())]),
    }
}

#[test]
fn negotiates_versions_from_bpd() {
    let mut c = FinTsConnector::new("52090000");
    let r = reply(b"HISALS:5:5:3+1+1+0'HISALS:6:7:3+1+1+0'HISALS:7:8:3+1+1+0'HICAZS:8:1:3+1+1+0+450:N:N:urn?:iso?:std?:iso?:20022?:tech?:xsd?:camt.052.001.02:urn?:iso?:std?:iso?:20022?:tech?:xsd?:camt.052.001.08'HITANS:9:6:3+1+1+0+J:N:0:946:2:SECUREGO:Decoupled::SecureGo plus:6:1:x'HITANS:10:7:3+1+1+0+J:N:0'HIKAZS:11:7:3+1+1+0+90:N:N'HIRMS:12:2:3+3920::Zugelassene Zwei-Schritt-Verfahren:942:946'");
    c.learn_bpd(&r);
    assert_eq!(c.version("HKSAL"), 8);
    assert_eq!(c.version("HKCAZ"), 1);
    assert_eq!(c.version("HKTAN"), 7);
    assert_eq!(c.version("HKKAZ"), 7);
    assert!(!c.bank_supports("HKSPA"));
    assert_eq!(
        c.st.camt_formats[0],
        "urn:iso:std:iso:20022:tech:xsd:camt.052.001.08"
    );
    assert_eq!(
        c.st.sec_func, "946",
        "decoupled SecureGo plus preferred over 942"
    );
    assert_eq!(c.st.tan_method_name, "SecureGo plus");
}

/// UPD account for the merge tests (invented numbers, public example IBANs).
fn upd_acc(number: &str, sub: &str, iban: &str, product: &str, allowed: &[&str]) -> UpdAccount {
    UpdAccount {
        iban: iban.into(),
        account_number: number.into(),
        sub_account: sub.into(),
        blz: "20041155".into(),
        kind_code: 1,
        currency: "EUR".into(),
        owner: "Max Mustermann".into(),
        product: product.into(),
        allowed: allowed.iter().map(|s| s.to_string()).collect(),
    }
}

fn sepa_acc(iban: &str, number: &str) -> SepaAccount {
    SepaAccount {
        iban: iban.into(),
        bic: "COBADEHDXXX".into(),
        account_number: number.into(),
        sub_account: String::new(),
        blz: "20041155".into(),
    }
}

const IBAN_A: &str = "DE89370400440532013000";
const IBAN_B: &str = "DE02120300000000202051";

#[test]
fn depot_without_iban_keeps_own_key_next_to_girokonto_with_same_number() {
    // comdirect (#805): every account has Kontoart 1, the depot has no IBAN and
    // the Girokonto's account number; HISPA lists only the two cash accounts.
    let upd = [
        upd_acc("1234567", "", IBAN_A, "Girokonto", &["HKSAL", "HKCAZ"]),
        upd_acc(
            "7654321",
            "",
            IBAN_B,
            "Verrechnungskonto",
            &["HKSAL", "HKCAZ"],
        ),
        upd_acc("1234567", "", "", "Depot", &["HKWPD"]),
    ];
    let sepa = vec![sepa_acc(IBAN_A, "1234567"), sepa_acc(IBAN_B, "7654321")];
    let out = merge_upd_with_sepa(&upd, sepa, "20041155");
    let labels: Vec<String> = out.iter().map(|(a, _)| account_label(a)).collect();
    assert_eq!(labels, [IBAN_A, IBAN_B, "1234567"]);
    let depot = &out[2];
    assert!(depot.0.iban.is_empty() && depot.0.bic.is_empty());
    assert_eq!(depot.1.as_ref().map(|u| u.product.as_str()), Some("Depot"));
}

#[test]
fn account_without_iban_still_takes_unclaimed_iban_by_account_number() {
    // Unchanged behaviour: the IBAN of a HISPA entry no UPD account carries
    // goes to the UPD account with that number, and is not listed twice.
    let upd = [upd_acc("1234567", "", "", "Girokonto", &["HKSAL"])];
    let out = merge_upd_with_sepa(&upd, vec![sepa_acc(IBAN_A, "1234567")], "20041155");
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].0.iban, IBAN_A);
    assert_eq!(out[0].0.bic, "COBADEHDXXX");
}

#[test]
fn unclaimed_iban_goes_to_one_account_only() {
    // Two UPD accounts without IBAN share a number: only the first one may take
    // the HISPA IBAN, the second keeps a key of its own.
    let upd = [
        upd_acc("1234567", "", "", "Girokonto", &["HKSAL"]),
        upd_acc("1234567", "01", "", "Darlehen", &["HKSAL"]),
    ];
    let out = merge_upd_with_sepa(&upd, vec![sepa_acc(IBAN_A, "1234567")], "20041155");
    let labels: Vec<String> = out.iter().map(|(a, _)| account_label(a)).collect();
    assert_eq!(labels, [IBAN_A, "1234567-01"]);
}

#[test]
fn own_iban_wins_over_account_number_match() {
    // HISPA lists another account with the same number first: the UPD account
    // keeps its own IBAN instead of taking the first number match.
    let upd = [upd_acc("1234567", "", IBAN_B, "Girokonto", &["HKSAL"])];
    let sepa = vec![sepa_acc(IBAN_A, "1234567"), sepa_acc(IBAN_B, "1234567")];
    let out = merge_upd_with_sepa(&upd, sepa, "20041155");
    assert_eq!(out[0].0.iban, IBAN_B);
    // the other HISPA account is listed on its own
    assert_eq!(out.len(), 2);
    assert_eq!(out[1].0.iban, IBAN_A);
    assert!(out[1].1.is_none());
}

#[test]
fn parses_upd_with_allowed_transactions_and_loans() {
    let r = reply(b"HIUPD:5:6:3+0012345601::280:52090000+DE44520900000012345601+KUNDE+1+EUR+Muster+Alex+Girokonto++HKSAL:1+HKCAZ:1+HKKAZ:1'HIUPD:6:6:3+0098765400::280:52090000++KUNDE+40+EUR+Muster+Alex+Baudarlehen++HKSAL:1'");
    let upds: Vec<UpdAccount> = r.all("HIUPD").map(parse_upd).collect();
    assert_eq!(upds.len(), 2);
    assert_eq!(upds[0].iban, "DE44520900000012345601");
    assert!(upds[0].allowed.contains("HKCAZ"));
    assert_eq!(kind_from_code(upds[1].kind_code), AccountKind::Loan);
    assert!(
        upds[1].allowed.contains("HKSAL") && !upds[1].allowed.contains("HKCAZ"),
        "loan: balance only"
    );
    let kti = FinTsConnector::kti(&SepaAccount {
        iban: String::new(),
        bic: String::new(),
        account_number: "0098765400".into(),
        sub_account: String::new(),
        blz: "52090000".into(),
    });
    assert_eq!(kti, "::0098765400::280:52090000");
    // IBAN known: IBAN:BIC – except Consorsbank, which wants the full form.
    let acc = |blz: &str| SepaAccount {
        iban: "DE00".into(),
        bic: "BICXDEXX".into(),
        account_number: "0123456789".into(),
        sub_account: String::new(),
        blz: blz.into(),
    };
    assert_eq!(FinTsConnector::kti(&acc("52090000")), "DE00:BICXDEXX");
    assert_eq!(
        FinTsConnector::kti(&acc("76030080")),
        "DE00:BICXDEXX:0123456789::280:76030080"
    );
}

#[test]
fn error_names_offending_segment() {
    let r = reply(b"HIRMG:2:2+9050::Die Nachricht enth\xe4lt Fehler.'HIRMS:3:2:3+9010::Auftragsart wird nicht unterst\xfctzt.'");
    let err = r.check().unwrap_err().to_string();
    assert!(err.contains("9050 [Nachricht]"), "{err}");
    assert!(err.contains("9010 [HKKAZ #3]"), "{err}");
}

#[test]
fn submit_reply_declines_instant_only_when_order_was_refused() {
    // Order refused before any approval -> instant declined, with the
    // specific code rather than the generic 9050.
    let r = reply(b"HIRMG:2:2+9050::Die Nachricht enth\xe4lt Fehler.'HIRMS:3:2:3+9210::Empf\xe4ngerbank nimmt nicht an SEPA Instant teil.'");
    let OrderReply::InstantDeclined(why) = classify_submit_reply(&r, true, 3) else {
        panic!()
    };
    assert!(why.starts_with("9210 "), "{why}");
    assert!(!why.contains("9050"), "{why}");
    // Same reply for a regular order -> plain error.
    assert_eq!(classify_submit_reply(&r, false, 3), OrderReply::Rejected);
    // Accepted, TAN required.
    let r = reply(b"HIRMG:2:2+0010::Nachricht entgegengenommen.'HIRMS:3:2:3+0030::Auftrag empfangen - TAN erforderlich.'");
    assert_eq!(classify_submit_reply(&r, true, 3), OrderReply::Accepted);
    // Wrong PIN -> not executed, no instant decline either.
    let r = reply(b"HIRMG:2:2+9942::PIN falsch.'");
    assert_eq!(classify_submit_reply(&r, true, 3), OrderReply::Rejected);
    // Dialog abort on an instant order: may have been executed.
    let r = reply(b"HIRMG:2:2+9800::Dialog abgebrochen.'");
    assert!(matches!(
        classify_submit_reply(&r, true, 3),
        OrderReply::Unknown(_)
    ));
    // Contradictory: order segment confirmed but errors present -> unknown.
    let r = reply(b"HIRMG:2:2+9800::Dialog abgebrochen.'HIRMS:3:2:3+0020::Auftrag ausgef\xfchrt.'");
    assert!(matches!(
        classify_submit_reply(&r, true, 3),
        OrderReply::Unknown(_)
    ));
}

#[test]
fn submit_reply_reads_the_order_segment_behind_the_name_check() {
    // With HKVPP in front, the order is segment 4: its confirmation plus
    // an error elsewhere is contradictory (unknown), not a plain decline.
    let r = reply(b"HIRMG:2:2+9800::Dialog abgebrochen.'HIRMS:4:2:4+0020::Auftrag ausgef\xfchrt.'");
    assert!(matches!(
        classify_submit_reply(&r, true, 4),
        OrderReply::Unknown(_)
    ));
    // A success code for the name-check segment (3) says nothing about the order.
    let r = reply(b"HIRMG:2:2+9050::Fehler.'HIRMS:3:2:3+0020::ok.'HIRMS:4:2:4+9210::Abgelehnt.'");
    assert!(matches!(
        classify_submit_reply(&r, true, 4),
        OrderReply::InstantDeclined(_)
    ));
}

#[test]
fn approval_reply_never_treats_ambiguous_codes_as_not_executed() {
    // Executed.
    let r = reply(
        b"HIRMG:2:2+0010::Nachricht entgegengenommen.'HIRMS:3:2:3+0020::Auftrag ausgef\xfchrt.'",
    );
    assert_eq!(classify_approval_reply(&r, true), OrderReply::Accepted);
    // Generic codes, dialog abort, unknown order codes, transient errors:
    // the instant payment may have been executed -> unknown, both for
    // instant and regular orders.
    for raw in [
        &b"HIRMG:2:2+9050::Die Nachricht enth\xe4lt Fehler.'"[..],
        &b"HIRMG:2:2+9010::Verarbeitung nicht m\xf6glich.'"[..],
        &b"HIRMG:2:2+9800::Dialog abgebrochen.'"[..],
        &b"HIRMG:2:2+9800::Dialog abgebrochen.'HIRMS:3:2:3+9210::Auftrag abgelehnt.'"[..],
        &b"HIRMG:2:2+9050::Fehler.'HIRMS:3:2:3+9110::Verarbeitung abgebrochen.'"[..],
        &b"HIRMG:2:2+9050::Fehler.'HIRMS:3:2:3+9210::Abgelehnt.'HIRMS:4:2:4+9160::Unbekannt.'"[..],
    ] {
        for instant in [true, false] {
            let got = classify_approval_reply(&reply(raw), instant);
            assert!(
                matches!(got, OrderReply::Unknown(_)),
                "{:?} -> {got:?}",
                String::from_utf8_lossy(raw)
            );
        }
    }
    // Clear order-specific rejection (9210, limit 9380) without 9800:
    // instant -> declined (user may resend as regular), regular -> error.
    for raw in [
        &b"HIRMG:2:2+9050::Fehler.'HIRMS:3:2:3+9210::Empf\xe4ngerbank nicht erreichbar.'"[..],
        &b"HIRMG:2:2+3060::Warnungen.'HIRMS:3:2:3+9380::Betrag \xfcbersteigt Limit.'"[..],
    ] {
        let r = reply(raw);
        assert!(matches!(
            classify_approval_reply(&r, true),
            OrderReply::InstantDeclined(_)
        ));
        assert_eq!(classify_approval_reply(&r, false), OrderReply::Rejected);
    }
    // Declined in the app, wrong TAN -> not executed.
    for raw in [
        &b"HIRMG:2:2+9050::Fehler.'HIRMS:3:2:3+9955::Auftrag abgelehnt.'"[..],
        &b"HIRMG:2:2+9050::Fehler.'HIRMS:3:2:3+9941::TAN ung\xfcltig.'"[..],
    ] {
        assert_eq!(
            classify_approval_reply(&reply(raw), true),
            OrderReply::Rejected
        );
    }
}

#[test]
fn reads_instant_formats_and_negotiates_hkipz() {
    // HIIPZS as sent by Consorsbank (purpose codes empty) and comdirect.
    let raw = b"HIIPZS:11:1:3+1+1+0+:urn?:iso?:std?:iso?:20022?:tech?:xsd?:pain.001.001.09:urn?:iso?:std?:iso?:20022?:tech?:xsd?:pain.001.001.03'";
    let r = reply(raw);
    assert_eq!(
        pain_formats(&r, "HIIPZS"),
        vec![
            sepa::PAIN_09_DESCRIPTOR.to_string(),
            sepa::PAIN_DESCRIPTOR.to_string()
        ]
    );
    let raw = b"HIIPZS:20:1:3+999+1+0+BENE,BONU:urn?:iso?:std?:iso?:20022?:tech?:xsd?:pain.001.001.09:urn?:iso?:std?:iso?:20022?:tech?:xsd?:pain.001.001.09.xsd'";
    assert_eq!(
        pain_formats(&reply(raw), "HIIPZS"),
        vec![sepa::PAIN_09_DESCRIPTOR.to_string()]
    );

    let mut c = FinTsConnector::new("76030080");
    c.learn_bpd(&reply(raw));
    assert!(c.bank_supports("HKIPZ"));
    assert_eq!(c.version("HKIPZ"), 1);
    assert_eq!(
        c.st.pain_formats["HKIPZ"],
        vec![sepa::PAIN_09_DESCRIPTOR.to_string()]
    );
    // A second BPD message adds formats instead of replacing them.
    c.learn_bpd(&reply(
        b"HIIPZS:20:1:3+999+1+0+:urn?:iso?:std?:iso?:20022?:tech?:xsd?:pain.001.001.03'",
    ));
    assert_eq!(
        c.st.pain_formats["HKIPZ"],
        vec![
            sepa::PAIN_09_DESCRIPTOR.to_string(),
            sepa::PAIN_DESCRIPTOR.to_string()
        ]
    );
}

fn order_for(c: &mut FinTsConnector) {
    c.order = Some(PreparedOrder {
        seg_id: "HKCCS",
        kti: String::new(),
        descriptor: sepa::PAIN_DESCRIPTOR,
        xml: String::new(),
        instant: false,
        recipient_name: "Erika Musterfrau".into(),
        recipient_iban: "DE89370400440532013000".into(),
        batch: None,
        debit_layout: None,
        standing: None,
    });
}

#[test]
fn reads_vop_parameters_from_bpd() {
    // HIVPPS as sent by comdirect, plus a DK code: the name check covers HK orders only.
    let raw =
        b"HIVPPS:7:1:3+1+1+0+20:J:S:N:N:sepade.pain.002.001.10.xsd:HKIPZ:HKCUI:HKCCS:HKCDE:DKKKU'";
    let mut c = FinTsConnector::new("20041111");
    c.learn_bpd(&reply(raw));
    assert_eq!(c.st.vop_format, "sepade.pain.002.001.10.xsd");
    assert_eq!(c.st.vop_orders, vec!["HKIPZ", "HKCUI", "HKCCS", "HKCDE"]);
    // Consorsbank sends no HIVPPS: no name check.
    let c = FinTsConnector::new("76030080");
    assert!(c.st.vop_format.is_empty());
}

#[test]
fn reads_batch_parameters_and_builds_hkccm() {
    let raw = b"HICCMS:12:1:3+1+1+0+500:J:J:urn?:iso?:std?:iso?:20022?:tech?:xsd?:pain.001.001.09'";
    let mut c = FinTsConnector::new("20041111");
    c.learn_bpd(&reply(raw));
    assert_eq!(
        c.st.batch_params,
        Some(BatchParams {
            max_count: 500,
            sum_required: true,
            single_booking_allowed: true
        })
    );
    assert_eq!(
        c.st.pain_formats["HKCCM"],
        vec![sepa::PAIN_09_DESCRIPTOR.to_string()]
    );
    order_for(&mut c);
    if let Some(o) = c.order.as_mut() {
        o.seg_id = "HKCCM";
        o.batch = Some((123_450, true));
    }
    let text = String::from_utf8_lossy(&c.order_payload(Vop::None).unwrap().concat()).to_string();
    assert!(text.contains("HKCCM:3:"), "{text}");
    assert!(text.contains("+1234,50:EUR+J+"), "{text}");
    // Without the BPD flags neither optional field is sent.
    c.st.batch_params = Some(BatchParams::default());
    let text = String::from_utf8_lossy(&c.order_payload(Vop::None).unwrap().concat()).to_string();
    assert!(!text.contains("EUR+"), "{text}");
}

#[test]
fn standing_order_limits_come_from_the_stored_state() {
    use crate::models::StandingOrderAction as A;
    let raw = b"HICDES:12:1:3+1+1+0+1:2:90:010203:00:00:0'\
                HICDNS:13:1:3+1+1+0+1:2:90:J:J:J:J:J:J:J:J:J:010612:010515:00:135'";
    let mut c = FinTsConnector::new("20041111");
    c.learn_bpd(&reply(raw));
    let state = crate::secret::SecretJson::encode(&c.st).unwrap();
    let create = standing_limits_from_state(&state, A::Create).unwrap();
    assert_eq!((create.min_days_ahead, create.max_days_ahead), (2, 90));
    assert_eq!(create.months, [1, 2, 3]);
    assert!(create.days_of_month.is_empty() && create.weeks.is_empty());
    assert!(create.weekdays.is_empty(), "0 = every weekday");
    let change = standing_limits_from_state(&state, A::Change).unwrap();
    assert_eq!(change.months, [1, 6, 12]);
    assert_eq!(change.days_of_month, [1, 5, 15]);
    assert_eq!(change.weekdays, [1, 3, 5]);
    assert!(standing_limits_from_state(&state, A::Delete).is_none());
    // Not a FinTS state, or none of the parameters seen yet.
    assert!(standing_limits_from_state(&serde_json::json!({}).into(), A::Create).is_none());
}

#[test]
fn reads_standing_order_parameters_and_builds_the_three_orders() {
    let raw = b"HICDES:12:1:3+1+1+0+1:2:90:010203:00:00:0'\
                HICDNS:13:1:3+1+1+0+1:2:90:J:J:J:J:J:J:J:J:J:010203:00:00:0'\
                HICDLS:14:2:3+1+1+0+1:90:J:J'";
    let mut c = FinTsConnector::new("20041111");
    c.learn_bpd(&reply(raw));
    assert!(c.bank_supports("HKCDE") && c.bank_supports("HKCDN") && c.bank_supports("HKCDL"));
    assert_eq!(c.version("HKCDL"), 2);
    let create = c.st.standing_params.create.clone().unwrap();
    assert_eq!(
        create,
        standing_order::parse_create_rules(
            &syntax::parse_response(b"HICDES:12:1:3+1+1+0+1:2:90:010203:00:00:0'").unwrap()[0]
        )
    );
    let change = c.st.standing_params.change.clone().unwrap();
    assert!(format!("{change:?}").contains("010203"));

    let job = |action, order_id: &str, last: Option<&str>| crate::connectors::StandingOrderJob {
        action,
        source_iban: "DE02120300000000202051".into(),
        source_owner: "A".into(),
        order_id: order_id.into(),
        recipient_name: "Vermieter".into(),
        recipient_iban: "DE89370400440532013000".into(),
        recipient_bic: None,
        amount_cents: 85_000,
        purpose: "Miete".into(),
        first_date: "2026-12-01".into(),
        last_date: last.map(str::to_string),
        time_unit: "M".into(),
        interval: 1,
        exec_day: 1,
    };
    let payload = |c: &mut FinTsConnector,
                   seg: &'static str,
                   j: &crate::connectors::StandingOrderJob,
                   v: u32| {
        order_for(c);
        let with_time = seg == "HKCDL" && v >= 2;
        let o = c.order.as_mut().unwrap();
        o.seg_id = seg;
        o.xml = "<x/>".into();
        o.standing = Some(standing_order::StandingTail {
            order_id: (j.action != crate::models::StandingOrderAction::Create)
                .then(|| j.order_id.clone()),
            schedule: standing_order::schedule_components(j, with_time),
        });
        String::from_utf8_lossy(&c.order_payload(Vop::None).unwrap().concat()).to_string()
    };
    use crate::models::StandingOrderAction as A;
    let text = payload(&mut c, "HKCDE", &job(A::Create, "", None), 1);
    assert!(text.contains("+@4@<x/>+20261201:M:1:1'"), "{text}");
    let text = payload(
        &mut c,
        "HKCDN",
        &job(A::Change, "A1", Some("2027-11-01")),
        1,
    );
    assert!(
        text.contains("+@4@<x/>++A1+20261201:M:1:1:20271101'"),
        "{text}"
    );
    let text = payload(&mut c, "HKCDL", &job(A::Delete, "A1", None), 2);
    assert!(text.contains("+@4@<x/>++A1+20261201:M:1:1'"), "{text}");
    let text = payload(
        &mut c,
        "HKCDL",
        &job(A::Delete, "A1", Some("2027-11-01")),
        2,
    );
    assert!(text.contains("++A1+20261201:M:1:1::20271101'"), "{text}");
}

#[test]
fn reads_direct_debit_parameters_and_formats() {
    let raw = b"HIDMES:12:1:3+1+1+0+2:30:5:30:360:J:N:urn?:iso?:std?:iso?:20022?:tech?:xsd?:pain.008.003.02'";
    let mut c = FinTsConnector::new("20041111");
    c.learn_bpd(&reply(raw));
    assert_eq!(
        c.st.debit_params["HKDME"],
        DebitParams {
            min_days_recurring: 2,
            max_days_recurring: 30,
            min_days_first: 5,
            max_days_first: 30,
            max_count: 360,
            sum_required: true,
            single_booking_allowed: false,
        }
    );
    assert_eq!(
        c.st.pain_formats["HKDME"],
        vec![pain008::PAIN008_02_DESCRIPTOR.to_string()]
    );
}

#[test]
fn debit_lead_time_is_checked_per_sequence() {
    use pain008::SequenceType::*;
    let p = DebitParams {
        min_days_recurring: 2,
        max_days_recurring: 30,
        min_days_first: 5,
        max_days_first: 30,
        ..Default::default()
    };
    let d = |s: &str| jiff::civil::Date::strptime("%Y-%m-%d", s).unwrap();
    let today = d("2026-11-02");
    assert!(check_debit_lead_time(&p, Recurring, today, d("2026-11-04")).is_ok());
    assert!(check_debit_lead_time(&p, First, today, d("2026-11-04")).is_err());
    let e = check_debit_lead_time(&p, First, today, d("2026-11-04")).unwrap_err();
    assert!(e.to_string().contains("07.11.2026"), "{e}");
    assert!(check_debit_lead_time(&p, OneOff, today, d("2027-03-01")).is_err());
    // 30 days may be TARGET days (about 42 calendar days): not rejected
    assert!(check_debit_lead_time(&p, OneOff, today, d("2026-12-10")).is_ok());
    // unusable parameters never block
    let junk = DebitParams::default();
    assert!(check_debit_lead_time(&junk, First, today, d("2026-11-02")).is_ok());
}

#[test]
fn builds_hkdme_with_the_banks_layout() {
    let mut c = FinTsConnector::new("20041111");
    order_for(&mut c);
    if let Some(o) = c.order.as_mut() {
        o.seg_id = "HKDME";
        o.batch = Some((5_000, false));
        o.debit_layout = Some((true, false));
    }
    let text = String::from_utf8_lossy(&c.order_payload(Vop::None).unwrap().concat()).to_string();
    assert!(text.contains("HKDME:3:"), "{text}");
    assert!(text.contains("+50,00:EUR+"), "{text}");
    assert!(
        !text.contains(":EUR+J") && !text.contains(":EUR+N"),
        "{text}"
    );
}

#[test]
fn polling_sends_only_hkvpp_without_order_or_hktan() {
    let mut c = FinTsConnector::new("20041111");
    order_for(&mut c);
    c.st.vop_format = "urn:iso:std:iso:20022:tech:xsd:pain.002.001.14".into();
    let text = |p: Vec<Vec<u8>>| String::from_utf8_lossy(&p.concat()).to_string();
    let first = text(c.order_payload(Vop::Check).unwrap());
    assert!(first.contains("HKVPP") && first.contains("HKCCS") && first.contains("HKTAN"));
    let poll = text(
        c.order_payload(Vop::Poll {
            polling_id: b"POLL-1".to_vec(),
            touchdown: Some("TD1".into()),
        })
        .unwrap(),
    );
    assert!(poll.contains("HKVPP") && poll.contains("@6@POLL-1") && poll.contains("TD1"));
    for seg in ["HKCCS", "HKIPZ", "HKCSE", "HKTAN"] {
        assert!(!poll.contains(seg), "{seg} in {poll}");
    }
}

#[test]
fn worst_vop_result_wins_and_bank_texts_are_cleaned() {
    assert_eq!(worst_vop_code(&["RCVC", "... RVNM ..."]), "RVNM");
    assert_eq!(worst_vop_code(&["", "RCVC RVMC"]), "RVMC");
    assert_eq!(worst_vop_code(&["RCVC", "PDNG"]), "PDNG");
    assert_eq!(worst_vop_code(&["RCVC"]), "RCVC");
    assert_eq!(worst_vop_code(&["", ""]), "");
    let dirty = format!("Max\u{0}\n  Muster\u{7}{}", "x".repeat(100));
    let clean = clean_bank_text(&dirty, VOP_NAME_MAX);
    assert_eq!(clean.chars().count(), VOP_NAME_MAX);
    assert!(clean.starts_with("Max Muster") && !clean.chars().any(|c| c.is_control()));
}

#[test]
fn evaluates_vop_results() {
    // Close match: bank reports the holder's name and a notice.
    let r = reply(b"HIRMG:2:2+3060::Hinweise.'HIRMS:3:2:4+3945::Freigabe ohne VOP-Best\xe4tigung nicht m\xf6glich.'HIVPP:5:1:3+@8@VOPID123+++++DE89370400440532013000::Erika Mustermann::RVMC+Trotzdem freigeben??'");
    let mut c = FinTsConnector::new("20041111");
    order_for(&mut c);
    let check = c.evaluate_payee(&r).unwrap();
    assert_eq!(check.result, PayeeMatch::CloseMatch);
    assert_eq!(check.bank_name.as_deref(), Some("Erika Mustermann"));
    assert_eq!(check.entered_name, "Erika Musterfrau");
    assert_eq!(check.notice.as_deref(), Some("Trotzdem freigeben?"));
    assert_eq!(c.vop_id.as_deref(), Some(&b"VOPID123"[..]));
    assert!(!c.vop_with_tan);
    assert_eq!(c.hkvpa(3).unwrap(), b"HKVPA:3:1+@8@VOPID123'".to_vec());

    // Match without 3945: go on to the approval, HKVPA with the TAN.
    let r = reply(b"HIRMG:2:2+0010::OK.'HIVPP:5:1:3+@4@ABCD+++++DE89370400440532013000::::RCVC'");
    let mut c = FinTsConnector::new("20041111");
    order_for(&mut c);
    assert!(c.evaluate_payee(&r).is_none());
    assert!(c.vop_with_tan);

    // Pending (polling ID, no result) is reported as "not possible".
    let r = reply(b"HIVPP:6:1:3+++@36@6b1a5ec1-9af5-4398-b0da-6526cbf210dd+++++2'");
    let h = r.first("HIVPP").unwrap();
    assert_eq!(vop_result_code(h), "");
    assert_eq!(h.binary(3).map(|b| b.len()), Some(36));
    assert_eq!(h.get(8, 0), "2");
    let mut c = FinTsConnector::new("20041111");
    order_for(&mut c);
    assert_eq!(
        c.evaluate_payee(&r).unwrap().result,
        PayeeMatch::NotPossible
    );

    // No HIVPP at all: continue without a check.
    let mut c = FinTsConnector::new("20041111");
    order_for(&mut c);
    assert!(c.evaluate_payee(&reply(b"HIRMG:2:2+0010::OK.'")).is_none());
}

#[test]
fn match_with_3091_needs_no_hkvpa() {
    // 3091 "VOP-Ausführungsauftrag nicht benötigt": the challenge is simply answered.
    let r = reply(
        b"HIRMG:2:2+0010::OK.'HIRMS:3:2:3+3091::VOP-Ausfuehrungsauftrag nicht benoetigt.'HIVPP:5:1:3+@4@ABCD+++++DE89370400440532013000::::RCVC'",
    );
    let mut c = FinTsConnector::new("20041111");
    order_for(&mut c);
    assert!(c.evaluate_payee(&r).is_none());
    assert!(!c.vop_with_tan);
}

#[tokio::test]
async fn confirming_without_a_name_check_result_after_3091_resends_nothing() {
    // Berliner Sparkasse: HKVPP + HKCCS + HKTAN is answered with 3091 (no HKVPA
    // needed) and 3955 (approval in the app); HIVPP carries no result. The
    // order is already with the bank: "send anyway" must continue that
    // challenge. HKVPA + HKCCS + HKTAN again was rejected with 9010 on the order.
    let r = reply(
        b"HIRMG:2:2+3060::Hinweise.'HIRMS:3:2:3+3091::VOP-Ausfuehrungsauftrag nicht benoetigt.'HIRMS:4:2:5+3955::Auftrag empfangen - Bitte Auftrag in Ihrer App freigeben.'HIVPP:5:1:3+@8@VOPID123'",
    );
    let mut c = FinTsConnector::new("20041111");
    order_for(&mut c);
    let check = c.evaluate_payee(&r).unwrap();
    assert_eq!(check.result, PayeeMatch::NotPossible);
    c.keep_challenge_on_3091(&r, false).unwrap();
    assert!(!c.vop_with_tan);
    assert!(c.vop_resume.is_some());
    c.vop_pending = Some(check.result);

    // The connector has no URL: any message sent here would fail the call.
    let phase = c.confirm_payee(&test_ctx()).await.unwrap();
    assert!(matches!(phase, Phase::Challenge(_)), "{phase:?}");
    assert!(c.vop_resume.is_none());
    // A second confirmation is refused, as before.
    assert!(c.confirm_payee(&test_ctx()).await.is_err());
}

#[test]
fn the_3091_challenge_is_only_kept_when_the_reply_carries_it() {
    let with_3091 = reply(
        b"HIRMG:2:2+3060::Hinweise.'HIRMS:3:2:3+3091::nicht benoetigt.'HIRMS:4:2:5+3955::Bitte in der App freigeben.'",
    );
    // After polling the last reply answers a bare HKVPP: nothing to keep.
    let mut c = FinTsConnector::new("20041111");
    c.keep_challenge_on_3091(&with_3091, true).unwrap();
    assert!(c.vop_resume.is_none());
    // 3945 (confirmation needed): HKVPA + order are sent again, nothing to keep.
    let confirm = reply(
        b"HIRMG:2:2+3060::Hinweise.'HIRMS:3:2:3+3945::Freigabe ohne VOP-Bestaetigung nicht moeglich.'HIRMS:4:2:5+3955::Bitte in der App freigeben.'",
    );
    c.keep_challenge_on_3091(&confirm, false).unwrap();
    assert!(c.vop_resume.is_none());
    // No 3091: unchanged.
    let plain = reply(b"HIRMG:2:2+3060::Hinweise.'HIRMS:4:2:5+3955::Bitte in der App freigeben.'");
    c.keep_challenge_on_3091(&plain, false).unwrap();
    assert!(c.vop_resume.is_none());
    // A new order forgets what the previous one kept.
    c.keep_challenge_on_3091(&with_3091, false).unwrap();
    assert!(c.vop_resume.is_some());
    c.begin_order();
    assert!(c.vop_resume.is_none());
}

#[test]
fn transfer_errors_name_the_rejected_segment() {
    // Berliner Sparkasse, second message: HKVPA (#3) is fine, the order (#4) is not.
    let r = BankReply {
        segments: Vec::new(),
        codes: vec![
            (
                "9050".into(),
                None,
                "Die Nachricht enthält Fehler.".into(),
                vec![],
            ),
            ("0020".into(), Some(3), "In Ordnung.".into(), vec![]),
            ("9010".into(), Some(4), "Systemfehler".into(), vec![]),
        ],
        sent: HashMap::from([(3, "HKVPA".to_string()), (4, "HKCCS".to_string())]),
    };
    match r.check_transfer() {
        Err(CoreError::Bank { code, message }) => {
            assert_eq!(code, "9050");
            assert!(message.contains("9010"), "{message}");
            assert!(message.contains("[HKCCS #4]"), "{message}");
            assert!(!message.contains("HKVPA"), "{message}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn builds_hkcaz_with_escaped_camt_urn() {
    let mut c = FinTsConnector::new("52090000");
    c.st.versions.insert("HKCAZ".into(), vec![1]);
    let acc = SepaAccount {
        iban: "DE44520900000012345601".into(),
        bic: "GENODE51KS1".into(),
        account_number: String::new(),
        sub_account: String::new(),
        blz: "52090000".into(),
    };
    let seg = c.hkcaz(
        &acc,
        "urn:iso:std:iso:20022:tech:xsd:camt.052.001.08",
        "20260801",
        "20261109",
        None,
    );
    let text = String::from_utf8(seg.clone()).unwrap();
    assert!(text.starts_with("HKCAZ:3:1+DE44520900000012345601:GENODE51KS1+urn?:iso?:std?:iso?:20022?:tech?:xsd?:camt.052.001.08+N+20260801+20261109'"), "{text}");
    let parsed = syntax::parse(&seg).unwrap();
    assert_eq!(
        parsed[0].get(2, 0),
        "urn:iso:std:iso:20022:tech:xsd:camt.052.001.08"
    );
}

#[test]
fn builds_hkwpd_for_depot_with_touchdown() {
    let acc = SepaAccount {
        iban: String::new(),
        bic: String::new(),
        account_number: "1234567".into(),
        sub_account: String::new(),
        blz: "12030000".into(),
    };
    assert_eq!(
        FinTsConnector::hkwpd(6, &acc, None),
        b"HKWPD:3:6+1234567::280:12030000'".to_vec()
    );
    assert_eq!(
        FinTsConnector::hkwpd(5, &acc, Some("P:2")),
        b"HKWPD:3:5+1234567::280:12030000++++P?:2'".to_vec()
    );
}

#[test]
fn hktan_uses_negotiated_version() {
    let mut c = FinTsConnector::new("52090000");
    c.st.versions.insert("HKTAN".into(), vec![6]);
    let seg = String::from_utf8(c.hktan_process4(5, "HKIDN")).unwrap();
    assert!(seg.starts_with("HKTAN:5:6+4+HKIDN"), "{seg}");
}

#[test]
fn loan_sub_accounts_get_distinct_labels() {
    let mk = |sub: &str| SepaAccount {
        iban: String::new(),
        bic: String::new(),
        account_number: "0098765400".into(),
        sub_account: sub.into(),
        blz: "52090000".into(),
    };
    assert_ne!(account_label(&mk("01")), account_label(&mk("02")));
    assert_eq!(account_label(&mk("")), "0098765400");
}

#[test]
fn transfers_use_pain_09_whenever_the_bank_offers_it() {
    // comdirect offers both and rejects .03 ("Sepa-Format nicht mehr gültig").
    let mut c = FinTsConnector::new("20041111");
    let both = vec![
        sepa::PAIN_DESCRIPTOR.to_string(),
        sepa::PAIN_09_DESCRIPTOR.to_string(),
    ];
    c.st.pain_formats.insert("HKCCS".into(), both);
    assert_eq!(c.pain_version("HKCCS"), Some(sepa::PainVersion::V09));
    c.st.pain_formats
        .insert("HKCCS".into(), vec![sepa::PAIN_DESCRIPTOR.to_string()]);
    assert_eq!(c.pain_version("HKCCS"), Some(sepa::PainVersion::V03));
    c.st.pain_formats.remove("HKCCS");
    assert_eq!(c.pain_version("HKCCS"), Some(sepa::PainVersion::V03));
}

#[test]
fn a_clearly_rejected_order_is_not_left_as_submitted() {
    // comdirect's reply to HKVPP + HKCCS + HKTAN with an outdated format:
    // the order was refused before any approval, so the session must
    // report the error, not "status unclear".
    let mut c = FinTsConnector::new("20041111");
    c.order_submitted = true;
    let r = reply(
        b"HIRMG:2:2+9050::Die Nachricht enthaelt Fehler.'HIRMS:3:2:3+9210::VOP-Auftrag ungueltig.'HIRMS:4:2:4+9210::Sepa-Format nicht mehr g\xfcltig.'",
    );
    let err = c.reject_or_stop(&r).unwrap_err();
    assert!(matches!(err, CoreError::Bank { .. }));
    assert!(!c.order_submitted());
}

#[test]
fn fints_date_needs_eight_ascii_digits() {
    assert_eq!(fints_date("20261107").as_deref(), Some("2026-11-07"));
    assert_eq!(fints_date("202ä1107"), None); // 8 bytes, not 8 digits
    assert_eq!(fints_date("2026110"), None);
    assert_eq!(fints_date("20260230"), None);
    assert_eq!(fints_date(""), None);
}

#[test]
fn pain_header_is_well_formed_and_always_names_the_owner() {
    let h = sepa::PainHeader::new("  Max Mustermann ");
    assert_eq!(h.owner, "Max Mustermann");
    assert_eq!(sepa::PainHeader::new(" ").owner, "Kontoinhaber");
    assert!(
        jiff::civil::DateTime::strptime("%Y-%m-%dT%H:%M:%S", &h.created).is_ok(),
        "{}",
        h.created
    );
    // GC + timestamp + 8 hex digits, within the 35 characters pain allows.
    assert!(h.message_id.starts_with("GC") && h.message_id.len() == 24);
}

#[test]
fn orders_need_a_two_step_tan_method() {
    let mut c = FinTsConnector::new("20041111");
    assert!(matches!(
        c.require_tan_method("refused"),
        Err(CoreError::Bank { code, message }) if code == "HKTAN" && message == "refused"
    ));
    c.st.sec_func = "946".into();
    assert!(c.require_tan_method("refused").is_ok());
}

fn order_connector(url: &str) -> FinTsConnector {
    let state = serde_json::json!({"system_id": "123", "sec_func": "946", "tan_method_name": "", "tan_medium": "", "versions": {"HKSPA": [1]}, "tan_required": {}});
    two_step_connector(url, state)
}

#[tokio::test]
async fn order_source_account_honours_the_upd_and_says_why_it_is_missing() {
    // The bank offers HKSPA but rejects it: the UPD alone still lists the account.
    let (url, _) = mock_bank(vec![REJECT_9010]);
    let mut c = order_connector(&url);
    c.upd = vec![upd_acc(
        "0532013000",
        "",
        IBAN_A,
        "Girokonto",
        &["HKSAL", "HKCCS"],
    )];
    let ctx = test_ctx();

    let acc = c
        .source_account(&ctx, "DE89 3704 0044 0532 0130 00", "HKCCS")
        .await
        .unwrap();
    assert_eq!(acc.map(|a| a.iban).as_deref(), Some(IBAN_A));
    assert!(
        c.source_account(&ctx, IBAN_A, "HKIPZ")
            .await
            .unwrap()
            .is_none()
    );
    // The HKSPA error explains the missing account.
    match c.source_account(&ctx, IBAN_B, "HKCCS").await {
        Err(CoreError::NotFound { what }) => assert!(what.contains("HKSPA"), "{what}"),
        other => panic!("{:?}", other.map(|a| a.map(|a| a.iban))),
    }
}

#[tokio::test]
async fn order_source_account_without_upd_restrictions_allows_every_order() {
    // IBAN_B is known only from HKSPA (no UPD entry); IBAN_A's UPD lists nothing.
    let (url, _) = mock_bank(vec![
        "HNHBK:1:3+000000000000+300+DIALOG1+1'HIRMG:2:2+0010::Nachricht entgegengenommen.'HISPA:3:1:3+J:DE02120300000000202051:BYLADEM1001:202051::280:12030000'",
    ]);
    let mut c = order_connector(&url);
    c.upd = vec![upd_acc("0532013000", "", IBAN_A, "Girokonto", &[])];
    let ctx = test_ctx();

    for iban in [IBAN_A, IBAN_B] {
        let acc = c.source_account(&ctx, iban, "HKCCS").await.unwrap();
        assert_eq!(acc.map(|a| a.iban).as_deref(), Some(iban));
    }
}

/// One savings account that allows only HKSAL, against a bank answering `hksal`.
async fn fetch_savings(hksal: &'static str) -> FetchResult {
    let (url, _) = mock_bank(vec![hksal]);
    let state = serde_json::json!({"system_id": "123", "sec_func": "946", "tan_method_name": "", "tan_medium": "", "versions": {"HKSAL": [7]}, "tan_required": {}});
    let mut c = two_step_connector(&url, state);
    let mut savings = upd_acc("0532013000", "", IBAN_A, "Tagesgeld", &["HKSAL"]);
    savings.kind_code = 10;
    c.upd = vec![savings];
    c.fetch(&test_ctx()).await.unwrap()
}

#[tokio::test]
async fn a_balance_the_bank_did_not_send_is_unknown_not_zero() {
    // #998: no HISAL and no statement to take a closing balance from: the stored
    // balance must stay, the 0 in `balance_cents` is only a placeholder.
    let r = fetch_savings(ACCEPT).await;
    assert_eq!(r.accounts.len(), 1);
    assert_eq!(r.accounts[0].kind, AccountKind::Savings);
    assert_eq!(r.balances_unknown, vec![r.accounts[0].external_ref.clone()]);

    let r = fetch_savings(
        "HNHBK:1:3+000000000000+300+DIALOG1+1'HIRMG:2:2+0010::Nachricht entgegengenommen.'HISAL:3:7:3+DE89370400440532013000+Tagesgeld+EUR+C:1234,56:EUR:20261118'",
    )
    .await;
    assert_eq!(r.accounts[0].balance_cents, 123_456);
    assert!(r.balances_unknown.is_empty());
}

#[tokio::test]
async fn bookings_that_need_a_tan_are_reported_and_not_requested_twice() {
    // #1083 (flatex): the camt request gets 0030, a TAN the running fetch cannot
    // ask for. MT940 would only send the next TAN to the app; the other
    // account's balance is stored and the failure reported with the sync.
    const BALANCE: &str = "HNHBK:1:3+000000000000+300+DIALOG1+1'HIRMG:2:2+0010::Nachricht entgegengenommen.'HISAL:3:7:3+X+Girokonto+EUR+C:1234,56:EUR:20250102'";
    const TAN: &str = "HNHBK:1:3+000000000000+300+DIALOG1+1'HIRMG:2:2+0010::Nachricht entgegengenommen.'HIRMS:3:2:4+0030::Auftrag empfangen - TAN erforderlich.'HITAN:4:6:4+4++TASK1+TAN eingeben'";
    let (url, seen) = mock_bank(vec![BALANCE, TAN]);
    let state = serde_json::json!({"system_id": "123", "sec_func": "946", "tan_method_name": "", "tan_medium": "", "versions": {"HKSAL": [7], "HKCAZ": [1], "HKKAZ": [7]}, "tan_required": {"HKCAZ": true, "HKKAZ": true}, "camt_formats": ["urn:iso:std:iso:20022:tech:xsd:camt.052.001.08"]});
    let mut c = two_step_connector(&url, state);
    c.upd = vec![
        upd_acc("0532013000", "", IBAN_A, "Girokonto", &["HKSAL"]),
        upd_acc(
            "0000202051",
            "",
            IBAN_B,
            "Verrechnungskonto",
            &["HKCAZ", "HKKAZ"],
        ),
    ];
    let r = c.fetch(&test_ctx()).await.unwrap();
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "{seen:?}");
        assert!(seen[1].iter().any(|i| i == "HKCAZ"), "{seen:?}");
        assert!(!seen.iter().flatten().any(|i| i == "HKKAZ"), "{seen:?}");
    }
    assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
    assert!(r.warnings[0].starts_with(IBAN_B), "{:?}", r.warnings);
    assert!(r.warnings[0].contains("HKCAZ"), "{:?}", r.warnings);
    // B's balance was not read: the stored one stays; A's was
    assert_eq!(r.balances_unknown, vec![IBAN_B.to_string()]);
    let a = r
        .accounts
        .iter()
        .find(|a| a.external_ref == IBAN_A)
        .unwrap();
    assert_eq!(a.balance_cents, 123_456);
}

#[test]
fn hispas_advertises_pain_001_09_for_hkccs() {
    let mut c = FinTsConnector::new("20041133");
    c.learn_bpd(&reply(
        b"HICCSS:10:1:3+1+1+0'HISPAS:11:1:3+1+1+0+J:N:N:sepade.pain.001.001.09.xsd:urn?:iso?:std?:iso?:20022?:tech?:xsd?:pain.008.001.08'",
    ));
    assert_eq!(c.pain_version("HKCCS"), Some(super::sepa::PainVersion::V09));
}
