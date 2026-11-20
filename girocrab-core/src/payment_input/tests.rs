use super::*;

const IBAN: &str = "DE02120300000000202051";

fn text(s: &str) -> PaymentDetails {
    parse_payment_text(s.to_string())
}

fn payload(name: &str, iban: &str, bic: &str, cents: i64, purpose: &str) -> Option<String> {
    girocode_payload(name.into(), iban.into(), bic.into(), cents, purpose.into())
}

// MARK: free text

#[test]
fn iban_with_and_without_spaces() {
    assert_eq!(
        text("IBAN: DE02 1203 0000 0000 2020 51").iban.as_deref(),
        Some(IBAN)
    );
    assert_eq!(
        text("an de02120300000000202051 bitte").iban.as_deref(),
        Some(IBAN)
    );
}

#[test]
fn iban_with_bad_checksum_is_skipped() {
    let p = text("DE00 1203 0000 0000 2020 51\nDE89 3704 0044 0532 0130 00");
    assert_eq!(p.iban.as_deref(), Some("DE89370400440532013000"));
    assert_eq!(text("DE00120300000000202051").iban, None);
}

#[test]
fn bic_only_when_labelled() {
    assert_eq!(text("BIC: byladem1001").bic.as_deref(), Some("BYLADEM1001"));
    assert_eq!(text("SWIFT = COBADEFF").bic.as_deref(), Some("COBADEFF"));
    assert_eq!(text("BYLADEM1001").bic, None);
    assert_eq!(text("BIC: 12345678").bic, None, "no BIC shape");
}

#[test]
fn amounts() {
    assert_eq!(text("Summe 1.234,56 €").amount_cents, Some(123_456));
    assert_eq!(text("EUR 12.50").amount_cents, Some(12_50));
    assert_eq!(text("Total 1,234.56").amount_cents, Some(123_456));
    assert_eq!(text("Betrag: 12,50").amount_cents, Some(12_50));
    // unlabelled fallback, skipping the IBAN line
    assert_eq!(
        text("DE02 1203 0000 0000 2020 51\nNr 7\n99,90").amount_cents,
        Some(99_90)
    );
    // the labelled line wins over an earlier unlabelled number
    assert_eq!(
        text("Datum 01.02\n3,00 Stück\nzu zahlen 8,40").amount_cents,
        Some(8_40)
    );
    // numbers glued to digits, dots or commas are no amount ("12.345", "1.2.34")
    assert_eq!(text("Kunde 12.345\nRef 1.2.34").amount_cents, None);
    assert_eq!(text("kein Betrag").amount_cents, None);
}

#[test]
fn loose_amounts() {
    let a = |s: &str| parse_amount_loose(s.into());
    assert_eq!(a("1.234,56"), Some(123_456));
    assert_eq!(a("1,234.56"), Some(123_456));
    assert_eq!(a("1234.56"), Some(123_456));
    assert_eq!(a(" 12,5 "), Some(12_50));
    assert_eq!(a("199.99"), Some(199_99));
    assert_eq!(a("0.00"), None);
    assert_eq!(a("-5.00"), None);
    assert_eq!(a("abc"), None);
    assert_eq!(a(""), None);
}

#[test]
fn name_label_and_first_line_fallback() {
    let p = text("Rechnung\nEmpfänger: Stadtwerke Kassel\nIBAN DE02120300000000202051");
    assert_eq!(p.name.as_deref(), Some("Stadtwerke Kassel"));
    let p = text("DE02 1203 0000 0000 2020 51\n12,50\nDatum: heute\nMüller GmbH\nweiter");
    assert_eq!(p.name.as_deref(), Some("Müller GmbH"));
    assert_eq!(text(&"x".repeat(71)).name, None, "longer than 70");
}

#[test]
fn purpose_label_and_invoice_fallback() {
    let p = text("Verwendungszweck: Kundennr. 4711\nRechnungsnummer: 99");
    assert_eq!(p.purpose.as_deref(), Some("Kundennr. 4711"));
    assert_eq!(
        text("Bitte zahlen Sie Rechnung RE-123 bis Freitag")
            .purpose
            .as_deref(),
        Some("Rechnung RE-123")
    );
    assert_eq!(
        text("Invoice No. 2026/117").purpose.as_deref(),
        Some("Rechnung 2026/117")
    );
    assert_eq!(text("Hallo").purpose, None);
}

#[test]
fn empty_text() {
    assert_eq!(text(""), PaymentDetails::default());
}

// MARK: EPC parse

#[test]
fn parse_epc_v002() {
    let p = parse_epc_payload(
        "BCD\n002\n1\nSCT\nBYLADEM1001\nStadtwerke Kassel\nDE02 1203 0000 0000 2020 51\nEUR123.45\n\n\nAbschlag November".into(),
    )
    .unwrap();
    assert_eq!(
        p,
        PaymentDetails {
            name: Some("Stadtwerke Kassel".into()),
            iban: Some(IBAN.into()),
            bic: Some("BYLADEM1001".into()),
            amount_cents: Some(123_45),
            purpose: Some("Abschlag November".into()),
        }
    );
}

#[test]
fn parse_epc_v001_crlf_and_reference_fallback() {
    let p = parse_epc_payload(
        "BCD\r\n001\r\n1\r\nSCT\r\nBYLADEM1001\r\nA\r\nDE02120300000000202051\r\nEUR5\r\nGDDS\r\nRF18539007547034".into(),
    )
    .unwrap();
    assert_eq!(p.bic.as_deref(), Some("BYLADEM1001"));
    assert_eq!(p.amount_cents, Some(5_00));
    assert_eq!(p.purpose.as_deref(), Some("RF18539007547034"));
}

#[test]
fn parse_epc_rejects_other_payloads() {
    let ok = "BCD\n002\n1\nSCT\n\nA\nDE02120300000000202051";
    assert!(parse_epc_payload(ok.into()).is_some());
    assert_eq!(parse_epc_payload(ok.into()).unwrap().amount_cents, None);
    // as Swift / QML: version and identification are not checked
    let other = ok
        .replacen("\n002\n", "\n003\n", 1)
        .replacen("SCT", "INST", 1);
    assert_eq!(
        parse_epc_payload(other).unwrap().iban.as_deref(),
        Some(IBAN)
    );
    for bad in [
        ok.replace("BCD", "XYZ"),
        "BCD\n002\n1\nSCT\n\nA".to_string(),
        "https://example.com".to_string(),
    ] {
        assert_eq!(parse_epc_payload(bad.clone()), None, "{bad}");
    }
}

// MARK: GiroCode payload (GiroCodeTests.swift)

#[test]
fn payload_follows_epc069() {
    let p = payload(
        "Stadtwerke Kassel",
        "de02 1203 0000 0000 2020 51",
        "byladem1001",
        12_345,
        "Abschlag\nNovember",
    );
    assert_eq!(
        p.as_deref(),
        Some(
            "BCD\n002\n1\nSCT\nBYLADEM1001\nStadtwerke Kassel\nDE02120300000000202051\nEUR123.45\n\n\nAbschlag November"
        )
    );
}

#[test]
fn limits_and_invalid_input() {
    let long = "x".repeat(200);
    let p = payload(&long, IBAN, "", 5, &long).unwrap();
    let lines: Vec<&str> = p.split('\n').collect();
    assert_eq!(lines[5].chars().count(), GIROCODE_MAX_NAME);
    assert_eq!(lines[7], "EUR0.05");
    assert_eq!(lines[10].chars().count(), GIROCODE_MAX_REMITTANCE);
    assert_eq!(lines[4], "", "BIC is optional");
    assert!(p.len() <= GIROCODE_MAX_BYTES);
    assert_eq!(payload("A", "DE02", "", 100, ""), None);
    assert_eq!(payload(" ", IBAN, "", 100, ""), None);
    assert_eq!(payload("A", IBAN, "", 0, ""), None);
    assert_eq!(payload("A", IBAN, "", GIROCODE_MAX_CENTS + 1, ""), None);
    assert!(payload("A", IBAN, "", GIROCODE_MAX_CENTS, "").is_some());
}

/// EPC069-12: no trailing empty fields; the payload still parses.
#[test]
fn no_trailing_newline_without_purpose() {
    let p = payload("A", IBAN, "", 100, "  ").unwrap();
    assert_eq!(p, "BCD\n002\n1\nSCT\n\nA\nDE02120300000000202051\nEUR1.00");
    let parsed = parse_epc_payload(p).unwrap();
    assert_eq!(parsed.amount_cents, Some(1_00));
    assert_eq!(parsed.purpose, None);
}

/// EPC069-12: 331 bytes in all; umlauts take two bytes each, so the remittance
/// and then the name shrink on character boundaries.
#[test]
fn payload_fits_331_bytes_with_umlauts() {
    let name = "Ü".repeat(70);
    let purpose = "ä".repeat(140);
    let p = payload(&name, IBAN, "BYLADEM1001", GIROCODE_MAX_CENTS, &purpose).unwrap();
    assert!(p.len() <= GIROCODE_MAX_BYTES, "{} bytes", p.len());
    let lines: Vec<&str> = p.split('\n').collect();
    assert_eq!(
        lines[5], name,
        "name kept whole while the remittance shrinks"
    );
    assert!(lines[10].chars().all(|c| c == 'ä') && !lines[10].is_empty());
    // one more byte would not have fitted
    assert!(p.len() + 'ä'.len_utf8() > GIROCODE_MAX_BYTES);

    // 4-byte characters: the remittance goes entirely, then the name shrinks
    let name = "😀".repeat(70);
    let p = payload(&name, IBAN, "BYLADEM1001", GIROCODE_MAX_CENTS, &purpose).unwrap();
    assert!(p.len() <= GIROCODE_MAX_BYTES, "{} bytes", p.len());
    let lines: Vec<&str> = p.split('\n').collect();
    assert_eq!(lines.len(), 8, "trailing empty fields dropped: {p:?}");
    assert!(lines[5].chars().count() < 70 && lines[5].chars().all(|c| c == '😀'));
    assert!(p.len() + '😀'.len_utf8() > GIROCODE_MAX_BYTES);
    let parsed = parse_epc_payload(p).unwrap();
    assert_eq!(parsed.amount_cents, Some(GIROCODE_MAX_CENTS));
    assert_eq!(parsed.purpose, None);
}

/// EPC069-12: an ISO 11649 reference goes into the structured field.
#[test]
fn rf_reference_goes_to_structured_field() {
    let p = payload("A", IBAN, "", 100, "rf18 5390 0754 7034").unwrap();
    let lines: Vec<&str> = p.split('\n').collect();
    assert_eq!(lines.len(), 10, "remittance empty and dropped: {p:?}");
    assert_eq!(lines[9], "RF18539007547034");
    assert_eq!(
        parse_epc_payload(p).unwrap().purpose.as_deref(),
        Some("RF18539007547034")
    );
    // wrong check digits or no RF at all: free text
    for free in ["RF19 5390 0754 7034", "Rechnung 4711", "RF18"] {
        let p = payload("A", IBAN, "", 100, free).unwrap();
        let lines: Vec<&str> = p.split('\n').collect();
        assert_eq!((lines[9], lines[10]), ("", free));
    }
    assert_eq!(
        rf_reference("RF71 1234 5678 9012 3456 7890 12"),
        None,
        "too long"
    );
    assert!(rf_reference("RF45G72UUR").is_some());
}

#[test]
fn round_trip() {
    let p = payload(
        "Müller & Söhne GmbH",
        IBAN,
        "",
        1_999_99,
        "Rechnung 2026-117 – Danke",
    )
    .unwrap();
    let parsed = parse_epc_payload(p).unwrap();
    assert_eq!(
        parsed,
        PaymentDetails {
            name: Some("Müller & Söhne GmbH".into()),
            iban: Some(IBAN.into()),
            bic: None,
            amount_cents: Some(1_999_99),
            purpose: Some("Rechnung 2026-117 – Danke".into()),
        }
    );
}

// MARK: QR matrix

#[test]
fn matrix_shape() {
    let p = payload("Müller & Söhne GmbH", IBAN, "", 1_999_99, "Rechnung").unwrap();
    let m = girocode_matrix(p).unwrap();
    let (w, q) = (m.width as usize, GIROCODE_QUIET_ZONE);
    assert!(w >= 21 + 2 * q && w % 2 == 1);
    assert_eq!(m.modules.len(), w * w);
    // quiet zone: the first and last q rows and columns are light
    for i in 0..w {
        for j in (0..q).chain(w - q..w) {
            assert!(!m.modules[j * w + i] && !m.modules[i * w + j], "({i}, {j})");
        }
    }
    // finder pattern: top-left corner module dark, the one inside its light ring light
    assert!(m.modules[q * w + q]);
    assert!(!m.modules[(q + 1) * w + q + 1]);
    // the largest payload still fits
    let big = payload(&"Ü".repeat(70), IBAN, "BYLADEM1001", 1, &"ä".repeat(140)).unwrap();
    assert!(girocode_matrix(big).is_some());
    assert_eq!(girocode_matrix("x".repeat(5000)), None);
}

// MARK: review regressions

#[test]
fn invoice_number_needs_word_boundary_digit_and_same_line() {
    let purpose = |s: &str| text(s).purpose;
    assert_eq!(purpose("Rechnung\nEmpfänger: Stadtwerke Kassel"), None);
    assert_eq!(purpose("Rechnungsbetrag 120,00 €"), None);
    assert_eq!(purpose("Ihre Rechnung vom 12.03."), None);
    assert_eq!(
        purpose("Ihre Rechnung vom 12.03.\nRechnungs-Nr. 2026-117").as_deref(),
        Some("Rechnung 2026-117")
    );
    assert_eq!(
        purpose("Rechnungsnummer #A123").as_deref(),
        Some("Rechnung A123")
    );
}

#[test]
fn rf_reference_only_as_whole_token_or_groups_of_four() {
    assert_eq!(
        rf_reference("rf18539007547034").as_deref(),
        Some("RF18539007547034")
    );
    assert_eq!(
        rf_reference("RF18 5390 0754 7034").as_deref(),
        Some("RF18539007547034")
    );
    assert_eq!(rf_reference("RF45 G72U UR").as_deref(), Some("RF45G72UUR"));
    // free text whose letters happen to form a valid reference stays free text
    assert_eq!(rf_reference("RF45 G72 UUR"), None);
    assert_eq!(rf_reference("RF45G72UUR danke"), None);
    let p = payload("A", IBAN, "", 100, "rf12 abc def").unwrap();
    assert!(p.ends_with("\n\nrf12 abc def"), "{p:?}");
}

#[test]
fn currency_word_and_same_line() {
    assert_eq!(
        text("Friseur 2024\nBetrag: 35,00").amount_cents,
        Some(35_00)
    );
    assert_eq!(text("Betrag in EUR\n12.03.2026").amount_cents, None);
    assert_eq!(
        text("Betrag in EUR\n12.03.2026\n7,50").amount_cents,
        Some(7_50)
    );
    assert_eq!(text("Preis 9,99EUR").amount_cents, Some(9_99));
}

#[test]
fn amount_neighbours_are_decimal_digits_only() {
    assert_eq!(text("Fläche 12,50m²").amount_cents, Some(12_50));
    assert_eq!(text("½12,50").amount_cents, Some(12_50));
    assert_eq!(text("Ende 12,50.").amount_cents, Some(12_50));
    // Arabic-Indic digits are \d: glued to the number, no amount
    assert_eq!(text("٣12,50").amount_cents, None);
}

#[test]
fn bic_uses_the_sepa_check() {
    assert_eq!(text("BIC: coba de ff").bic.as_deref(), Some("COBADEFF"));
    assert_eq!(text("BIC: COBADE").bic, None);
}

#[test]
fn byte_limit_cut_is_exact_in_one_pass() {
    // mixed widths: the cut lands on a char boundary and uses the budget fully
    let purpose = "aä€😀".repeat(40);
    let p = payload(&"Ö".repeat(70), IBAN, "BYLADEM1001", 1, &purpose).unwrap();
    assert!(p.len() <= GIROCODE_MAX_BYTES);
    let rem = p.rsplit('\n').next().unwrap();
    let next = purpose[rem.len()..].chars().next().unwrap();
    assert!(purpose.starts_with(rem));
    assert!(p.len() + next.len_utf8() > GIROCODE_MAX_BYTES);
}
