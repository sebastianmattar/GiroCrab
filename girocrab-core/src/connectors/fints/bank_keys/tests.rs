use super::*;

#[test]
fn anonymous_request_matches_independently_generated_hbci4java_message() {
    // Generated offline with HBCI4Java 4.1.17 MsgGen/FirstKeyReq and invented data.
    // Published artifact: com.github.hbci4j:hbci4j-core:4.1.17 (Maven Central).
    // JAR SHA-256: bb3bc54587a3ff525d68d1763fae811ba8b2a6d8fc1684e157f3beb9a8c8feec
    let expected = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/rah10-first-key-request.bin"
    ));
    assert_eq!(request("12345678", &"A".repeat(25), "0.1"), expected);
}

#[test]
fn full_registration_is_only_sent_in_hkvvb_product_field() {
    let product = "A".repeat(25); // Invented registration; never use a real ID in tests.
    let raw = request("12345678", &product, "0.8.5");
    let parsed = syntax::parse(&raw).unwrap();
    let hkvvb = parsed.iter().find(|s| s.id() == "HKVVB").unwrap();
    assert_eq!(hkvvb.get(4, 0), product);
    assert_eq!(hkvvb.get(4, 0).len(), 25);
    assert_eq!(hkvvb.get(5, 0), "0.8.5");
    assert_eq!(
        raw.windows(product.len())
            .filter(|part| *part == product.as_bytes())
            .count(),
        1
    );
}

#[test]
fn rejection_exposes_only_numeric_untrusted_codes() {
    let feedback = SegmentBuilder::new("HIRMG", 2, 2)
        .de(&["9050", "", "PRIVATE-RESPONSE-TEXT"])
        .de(&["9078", "", "PRIVATE-REGISTRATION"])
        .de(&["9050", "", "duplicate"])
        .de(&["9UNTRUSTED", "", "do not display"])
        .build();
    let error = parse_candidates(&message(&[feedback], "dialog", 1), "12345678")
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("unbestätigte FinTS-Rückmeldecodes: 9050, 9078"));
    assert!(error.contains("noch nicht authentifiziert"));
    assert!(!error.contains("PRIVATE"));
    assert!(!error.contains("9UNTRUSTED"));
}

#[test]
fn malformed_error_code_still_rejects_without_forwarding_it() {
    let feedback = SegmentBuilder::new("HIRMS", 2, 2)
        .de(&["9UNTRUSTED", "", "PRIVATE-RESPONSE-TEXT"])
        .build();
    let error = parse_candidates(&message(&[feedback], "dialog", 1), "12345678")
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("Bankschlüsselabruf wurde abgelehnt"));
    assert!(!error.contains("PRIVATE"));
    assert!(!error.contains("9UNTRUSTED"));
}

#[test]
fn rejection_uses_request_segment_reference_and_each_codes_field() {
    let feedback = SegmentBuilder::with_ref("HIRMS", 8, 2, 3)
        .de(&["9110", "5", "PRIVATE-TEXT", "PRIVATE-PARAMETER"])
        .de(&["9130", "2,1", "PRIVATE-TEXT"])
        .build();
    let error = parse_candidates(&message(&[feedback], "dialog", 1), "12345678")
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("9110: Segment 3 (HKVVB), Feld 5"));
    assert!(error.contains("9130: Segment 3 (HKVVB), Feld 2,1"));
    assert!(!error.contains("Segment 8"));
    assert!(!error.contains("PRIVATE"));
}

#[test]
fn rejection_distinguishes_both_key_requests() {
    let feedback: Vec<_> = [4, 5]
        .into_iter()
        .map(|reference| {
            SegmentBuilder::with_ref("HIRMS", 8, 2, reference)
                .de(&["9110", "4,2", "PRIVATE"])
                .build()
        })
        .collect();
    let error = parse_candidates(&message(&feedback, "dialog", 1), "12345678")
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("9110: Segment 4 (HKISA/V), Feld 4,2"));
    assert!(error.contains("9110: Segment 5 (HKISA/S), Feld 4,2"));
}

#[test]
fn absent_references_are_reported_without_inventing_positions() {
    for (kind, expected) in [
        ("HIRMG", "Nachricht, kein Segmentbezug"),
        ("HIRMS", "Segmentbezug fehlt"),
    ] {
        let feedback = SegmentBuilder::new(kind, 2, 2)
            .de(&["9110", "", "PRIVATE"])
            .build();
        let error = parse_candidates(&message(&[feedback], "dialog", 1), "12345678")
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains(&format!("9110: {expected}, kein Feldbezug")));
        assert!(!error.contains("Segment 2 (HKIDN)"));
    }
}

#[test]
fn invalid_references_never_expose_their_contents() {
    for field in [
        "PRIVATE-REF",
        "1234567890123456789012345",
        "1,2,3",
        "0",
        "1,0",
        "１",
        "1,<x>",
    ] {
        let feedback = SegmentBuilder::with_ref("HIRMS", 8, 2, 1000)
            .de(&["9110", field, "PRIVATE-TEXT", "PRIVATE-PARAMETER"])
            .build();
        let error = parse_candidates(&message(&[feedback], "dialog", 1), "12345678")
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("Segmentbezug ungültig, Feldbezug ungültig"));
        assert!(!error.contains("1000"));
        assert!(!error.contains("PRIVATE"));
        assert!(!error.contains("1234567890123456789012345"));
        assert!(!error.contains("<x>"));
    }
    assert_eq!(field_reference("999,999").as_deref(), Some("999,999"));
    assert_eq!(field_reference("005,002").as_deref(), Some("5,2"));
}

#[test]
fn rejection_limits_reference_details() {
    let feedback: Vec<_> = (1..=20)
        .map(|position| {
            SegmentBuilder::with_ref("HIRMS", position + 1, 2, 4)
                .de(&["9110", &position.to_string(), "PRIVATE"])
                .build()
        })
        .collect();
    let error = parse_candidates(&message(&feedback, "dialog", 1), "12345678")
        .err()
        .unwrap()
        .to_string();
    assert_eq!(error.matches("9110: Segment").count(), 16);
    assert!(error.contains("weitere Bezüge ausgeblendet"));
    assert!(!error.contains("PRIVATE"));
}

fn candidate(kind: &str) -> Rah10BankKeyCandidate {
    let key = rsa::PrivateKey::new(&mut getrandom::SysRng, 2048)
        .unwrap()
        .to_public_key();
    let mut c = Rah10BankKeyCandidate {
        blz: "12345678".into(),
        user_id: "bank".into(),
        key_type: kind.into(),
        number: 1,
        version: 1,
        public_key_der: key.to_public_key_der().unwrap().as_bytes().to_vec(),
        fingerprint: String::new(),
        bits: 2048,
    };
    c.fingerprint = fingerprint(&public_key(&c).unwrap()).unwrap();
    c
}

fn key_segment(c: &Rah10BankKeyCandidate, number: u32) -> Vec<u8> {
    let key = public_key(c).unwrap().key;
    let n = key.n_bytes();
    let e = key.e_bytes();
    let mut segment = SegmentBuilder::new("HIISA", number, 3)
        .raw("1")
        .raw("dialog")
        .raw("1")
        .raw("224")
        .raw(&format!(
            "280:{}:{}:{}:{}:{}",
            c.blz, c.user_id, c.key_type, c.number, c.version
        ))
        .build();
    segment.pop();
    segment.extend_from_slice(format!("+6:19:10:@{}@", n.len()).as_bytes());
    segment.extend(n);
    segment.extend_from_slice(format!(":12:@{}@", e.len()).as_bytes());
    segment.extend(e);
    segment.extend_from_slice(b":13'");
    segment
}

#[test]
fn bootstrap_is_anonymous_and_returns_only_untrusted_candidates() {
    let request = request("12345678", "test-product", "0.1");
    let parsed = syntax::parse(&request).unwrap();
    assert_eq!(parsed.iter().filter(|s| s.id() == "HKISA").count(), 2);
    assert_eq!(parsed[1].get(2, 0), "9999999999");
    assert!(
        !parsed
            .iter()
            .any(|s| matches!(s.id(), "HNSHK" | "HNSHA" | "HNVSD"))
    );
    let a = candidate("S");
    let b = candidate("V");
    let raw = message(&[key_segment(&a, 2), key_segment(&b, 3)], "dialog", 1);
    let (_, result) = parse_candidates(&raw, "12345678").unwrap();
    assert_eq!(result[0].fingerprint, a.fingerprint);
    assert_eq!(result[1].public_key_der, b.public_key_der);
    assert!(parse_candidates(&raw, "87654321").is_err());
    for end in 0..raw.len() {
        assert!(parse_candidates(&raw[..end], "12345678").is_err());
    }
    assert!(parse_candidates(&message(&[key_segment(&a, 2)], "dialog", 1), "12345678").is_err());
    assert!(
        parse_candidates(
            &message(&[key_segment(&a, 2), key_segment(&a, 3)], "dialog", 1),
            "12345678"
        )
        .is_err()
    );
}

#[test]
fn errors_follow_the_ui_language() {
    use crate::i18n::{Lang, with_language};
    let keys = vec![candidate("S"), candidate("V")];
    let result = with_language(Lang::En, || confirmed_keys("12345678", &keys, &[]));
    let Err(CoreError::InvalidInput { message }) = result else {
        panic!("expected InvalidInput");
    };
    assert_eq!(
        message,
        "The signature and encryption keys must be confirmed separately."
    );
}

#[test]
fn only_independently_confirmed_fingerprints_can_be_pinned() {
    let keys = vec![candidate("S"), candidate("V")];
    let expected: Vec<_> = keys.iter().map(|k| k.fingerprint.clone()).collect();
    assert!(confirmed_keys("12345678", &keys, &expected).is_ok());
    assert!(confirmed_keys("12345678", &keys, &[]).is_err());
    assert!(confirmed_keys("12345678", &keys, &["00".repeat(32), expected[1].clone()]).is_err());
    let mut tampered = keys.clone();
    tampered[0].public_key_der = candidate("S").public_key_der;
    assert!(confirmed_keys("12345678", &tampered, &expected).is_err());
    let mut tampered = keys.clone();
    tampered[1].key_type = "S".into();
    assert!(confirmed_keys("12345678", &tampered, &expected).is_err());
    assert!(confirmed_keys("87654321", &keys, &expected).is_err());
    let spaced: Vec<_> = expected
        .iter()
        .map(|s| {
            s.as_bytes()
                .chunks(2)
                .map(|p| std::str::from_utf8(p).unwrap().to_lowercase())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    assert!(confirmed_keys("12345678", &keys, &spaced).is_ok());
    // Revalidate after persistence; an edited or corrupted pin must fail closed.
    let json = serde_json::to_string(&SavedBankKeys {
        candidates: keys,
        expected,
    })
    .unwrap();
    let mut restored: SavedBankKeys = serde_json::from_str(&json).unwrap();
    assert!(confirmed_keys("12345678", &restored.candidates, &restored.expected).is_ok());
    restored.expected[0] = "00".repeat(32);
    assert!(confirmed_keys("12345678", &restored.candidates, &restored.expected).is_err());
}

#[test]
fn ini_fingerprint_matches_fixed_padded_big_endian_vector() {
    // Public, invented numbers; reference SHA-256 independently calculated with hashlib.
    let rsa = PublicKey::new(
        BoxedUint::from_be_slice_vartime(&[0x81; 256]),
        BoxedUint::from(65537u64),
    )
    .unwrap();
    let key = BankKey {
        blz: "12345678".into(),
        user_id: "bank".into(),
        kind: 0,
        number: 1,
        version: 1,
        key: rsa,
    };
    assert_eq!(
        fingerprint(&key).unwrap(),
        "F26C6259E5CDFC2717EBFF4ADB6D60BB6A607531E0B0BE41F7A8FF014FA6754B"
    );
}
