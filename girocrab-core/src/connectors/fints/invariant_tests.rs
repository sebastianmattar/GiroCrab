//! End-to-end invariants of the payment path (#947): what the user entered
//! (recipient IBAN, name, amount, purpose, end-to-end id) is exactly what
//! leaves in the order segment, and no field ends up in another one.
//!
//! Every case goes job -> `PreparedOrder` (the functions `start_transfer`,
//! `start_batch` and `start_standing_order` use) -> `order_payload` bytes ->
//! FinTS segment parser -> binary element -> independent XML parser
//! (`roxmltree`). The expected values are written out here (transliteration,
//! clipping, IBAN form, amounts), never computed with the builder's helpers.

use super::standing_order::standing_order;
use super::transfer::{BatchParams, batch_order, transfer_order};
use super::{FinTsConnector, PreparedOrder, SepaAccount, Vop, sepa, syntax};
use crate::connectors::{BatchTransferJob, StandingOrderJob, TransferJob};
use crate::models::StandingOrderAction;
use sepa::{BatchPayment, PainVersion};

const SOURCE_IBAN: &str = "DE02120300000000202051";
const SOURCE_BIC: &str = "BYLADEM1001";

fn source() -> SepaAccount {
    SepaAccount {
        iban: SOURCE_IBAN.into(),
        bic: SOURCE_BIC.into(),
        account_number: "202051".into(),
        sub_account: String::new(),
        blz: "12030000".into(),
    }
}

/// (as entered, as it must arrive: upper case, no spaces)
const IBANS: &[(&str, &str)] = &[
    ("DE89370400440532013000", "DE89370400440532013000"),
    ("de89 3704 0044 0532 0130 00", "DE89370400440532013000"),
    ("AT611904300234573201", "AT611904300234573201"),
    (
        "FR14 2004 1010 0505 0001 3M02 606",
        "FR1420041010050500013M02606",
    ),
    ("NL91ABNA0417164300", "NL91ABNA0417164300"),
    ("CH9300762011623852957", "CH9300762011623852957"),
    ("GB29NWBK60161331926819", "GB29NWBK60161331926819"),
    ("IT60X0542811101000000123456", "IT60X0542811101000000123456"),
    ("ES9121000418450200051332", "ES9121000418450200051332"),
];

/// (as entered, as it must arrive in the DK character set, at most 70 chars)
const NAMES: &[(&str, &str)] = &[
    ("Erika Mustermann", "Erika Mustermann"),
    ("Jürgen Weiß-Öztürk", "Juergen Weiss-Oeztuerk"),
    ("O'Brien & Söhne GmbH", "O'Brien Soehne GmbH"),
    ("Café <Zürich> \"Ltd\"", "Cafe Zuerich Ltd"),
    // FinTS delimiters must survive inside the binary element.
    ("A+B?C:D'E (Teil 2)", "A+B?C:D'E (Teil 2)"),
    (
        "Allgemeine Ortskrankenkasse Musterstadt Abteilung Beitragseinzug und Sonstiges",
        "Allgemeine Ortskrankenkasse Musterstadt Abteilung Beitragseinzug und S",
    ),
];

/// (as entered, as it must arrive; `None` = no `RmtInf`)
const PURPOSES: &[(&str, Option<&str>)] = &[
    ("Rechnung 2026-0042", Some("Rechnung 2026-0042")),
    ("", None),
    (
        "Miete Nov. (Whg. 3) + NK: 120,00",
        Some("Miete Nov. (Whg. 3) + NK: 120,00"),
    ),
    ("Übertrag 50 €", Some("Uebertrag 50")),
    ("Zeile1\nZeile2\tEnde", Some("Zeile1 Zeile2 Ende")),
    (
        "Kd.Nr. 4711 Rg.Nr. 0815 Leistungszeitraum 01.10.2026 bis 31.10.2026 Vertrag 12345678 \
         Abschlag Strom und Gas inklusive Grundgebuehr und Messstellenbetrieb Danke",
        Some(
            "Kd.Nr. 4711 Rg.Nr. 0815 Leistungszeitraum 01.10.2026 bis 31.10.2026 Vertrag 12345678 \
             Abschlag Strom und Gas inklusive Grundgebuehr und Messs",
        ),
    ),
];

const AMOUNTS: &[i64] = &[1, 99, 100, 1_234, 100_000, sepa::MAX_AMOUNT_CENTS];

/// One `CdtTrfTxInf` as read back.
#[derive(Debug, PartialEq)]
struct Tx {
    end_to_end_id: String,
    amount_cents: i64,
    creditor_name: String,
    creditor_iban: String,
    creditor_bic: Option<String>,
    purpose: Option<String>,
}

/// A pain.001 document as read back by `roxmltree`.
#[derive(Debug)]
struct Parsed {
    namespace: String,
    nb_of_txs: Vec<String>,
    ctrl_sums: Vec<i64>,
    initiator: String,
    debtor_name: String,
    debtor_iban: String,
    debtor_bic: Option<String>,
    execution_date: String,
    instant: bool,
    batch_booking: String,
    txs: Vec<Tx>,
}

/// Cents from an XML amount: digits, a dot and exactly two decimals.
fn cents(text: &str) -> i64 {
    let (euros, decimals) = text.split_once('.').expect("amount with decimals");
    assert!(
        !euros.is_empty()
            && euros.bytes().all(|b| b.is_ascii_digit())
            && decimals.len() == 2
            && decimals.bytes().all(|b| b.is_ascii_digit()),
        "malformed amount {text:?}"
    );
    euros.parse::<i64>().unwrap() * 100 + decimals.parse::<i64>().unwrap()
}

fn elements<'a, 'i>(
    node: roxmltree::Node<'a, 'i>,
    name: &'static str,
) -> impl Iterator<Item = roxmltree::Node<'a, 'i>> {
    node.children()
        .filter(move |c| c.is_element() && c.tag_name().name() == name)
}

/// The only child element `name`, if any (more than one is a failure).
fn opt<'a, 'i>(
    node: roxmltree::Node<'a, 'i>,
    name: &'static str,
) -> Option<roxmltree::Node<'a, 'i>> {
    let mut found = elements(node, name);
    let first = found.next();
    assert!(found.next().is_none(), "more than one <{name}>");
    first
}

fn at<'a, 'i>(node: roxmltree::Node<'a, 'i>, path: &[&'static str]) -> roxmltree::Node<'a, 'i> {
    path.iter().fold(node, |n, name| {
        opt(n, name).unwrap_or_else(|| panic!("missing <{name}> in {path:?}"))
    })
}

fn text(node: roxmltree::Node, path: &[&'static str]) -> String {
    at(node, path).text().unwrap_or("").to_string()
}

fn opt_text(node: roxmltree::Node, path: &[&'static str]) -> Option<String> {
    let (last, parents) = path.split_last().unwrap();
    let mut n = node;
    for name in parents {
        n = opt(n, name)?;
    }
    opt(n, last).map(|n| n.text().unwrap_or("").to_string())
}

/// The layout each pain.001 version prescribes, written out here rather than
/// taken from the builder: namespace, the tag of a bank's BIC, and whether the
/// execution date is wrapped in `<Dt>`.
struct Layout {
    namespace: &'static str,
    bic: &'static str,
    other_bic: &'static str,
    date_in_dt: bool,
}

fn layout(version: PainVersion) -> Layout {
    match version {
        PainVersion::V03 => Layout {
            namespace: "urn:iso:std:iso:20022:tech:xsd:pain.001.001.03",
            bic: "BIC",
            other_bic: "BICFI",
            date_in_dt: false,
        },
        PainVersion::V09 => Layout {
            namespace: "urn:iso:std:iso:20022:tech:xsd:pain.001.001.09",
            bic: "BICFI",
            other_bic: "BIC",
            date_in_dt: true,
        },
    }
}

/// The BIC under `<FinInstnId>`, in the tag of this version only.
fn bic(fin: roxmltree::Node, l: &Layout) -> Option<String> {
    assert!(
        opt(fin, l.other_bic).is_none(),
        "<{}> in a {} document",
        l.other_bic,
        l.namespace
    );
    opt_text(fin, &[l.bic])
}

/// Parses a pain.001 and checks it has the layout of `version`.
fn parse_pain001(xml: &str, version: PainVersion) -> Parsed {
    let l = layout(version);
    let doc = roxmltree::Document::parse(xml).expect("well-formed pain.001");
    let root = doc.root_element();
    assert_eq!(root.tag_name().name(), "Document");
    let init = at(root, &["CstmrCdtTrfInitn"]);
    let hdr = at(init, &["GrpHdr"]);
    let pmt = at(init, &["PmtInf"]);
    let agent = at(pmt, &["DbtrAgt", "FinInstnId"]);
    let exec = at(pmt, &["ReqdExctnDt"]);
    let execution_date = if l.date_in_dt {
        assert!(
            exec.children()
                .all(|c| c.is_element() || c.text().unwrap_or("").trim().is_empty()),
            "pain.001.001.09: the execution date belongs in <Dt>"
        );
        text(exec, &["Dt"])
    } else {
        assert!(
            exec.children().all(|c| !c.is_element()),
            "pain.001.001.03: <ReqdExctnDt> holds the date itself"
        );
        exec.text().unwrap_or("").to_string()
    };
    let txs = elements(pmt, "CdtTrfTxInf")
        .map(|tx| {
            let amount = at(tx, &["Amt", "InstdAmt"]);
            assert_eq!(amount.attribute("Ccy"), Some("EUR"));
            Tx {
                end_to_end_id: text(tx, &["PmtId", "EndToEndId"]),
                amount_cents: cents(amount.text().unwrap_or("")),
                creditor_name: text(tx, &["Cdtr", "Nm"]),
                creditor_iban: text(tx, &["CdtrAcct", "Id", "IBAN"]),
                creditor_bic: opt(tx, "CdtrAgt").and_then(|a| bic(at(a, &["FinInstnId"]), &l)),
                purpose: opt_text(tx, &["RmtInf", "Ustrd"]),
            }
        })
        .collect();
    let namespace = root.tag_name().namespace().unwrap_or("").to_string();
    assert_eq!(namespace, l.namespace);
    Parsed {
        namespace,
        nb_of_txs: vec![text(hdr, &["NbOfTxs"]), text(pmt, &["NbOfTxs"])],
        ctrl_sums: vec![
            cents(&text(hdr, &["CtrlSum"])),
            cents(&text(pmt, &["CtrlSum"])),
        ],
        initiator: text(hdr, &["InitgPty", "Nm"]),
        debtor_name: text(pmt, &["Dbtr", "Nm"]),
        debtor_iban: text(pmt, &["DbtrAcct", "Id", "IBAN"]),
        debtor_bic: bic(agent, &l),
        execution_date,
        instant: opt_text(pmt, &["PmtTpInf", "LclInstrm", "Cd"]).as_deref() == Some("INST"),
        batch_booking: text(pmt, &["BtchBookg"]),
        txs,
    }
}

/// What the order segment carried, read back from the wire bytes.
struct Sent {
    segment: syntax::Segment,
    /// Element index of the pain.001.
    binary_at: usize,
    pain: Parsed,
}

/// Puts `order` into a connector, builds the order message and reads it back
/// with the segment parser: the pain.001 is taken from the binary element,
/// not from `order.xml`, and must have the layout of `version`.
fn send(order: PreparedOrder, version: PainVersion, batch_params: Option<BatchParams>) -> Sent {
    let mut c = FinTsConnector::new("12030000");
    c.st.batch_params = batch_params;
    let seg_id = order.seg_id;
    let descriptor = order.descriptor;
    let xml = order.xml.clone();
    c.order = Some(order);
    let bytes = c.order_payload(Vop::None).unwrap().concat();
    let segments = syntax::parse(&bytes).unwrap();
    let ids: Vec<&str> = segments.iter().map(|s| s.id()).collect();
    assert_eq!(ids, [seg_id, "HKTAN"], "one order segment, then HKTAN");
    let segment = segments[0].clone();
    // KTI of the source account: the money leaves from there.
    assert_eq!(segment.get(1, 0), SOURCE_IBAN);
    assert_eq!(segment.get(1, 1), SOURCE_BIC);
    let binary_at = (0..segment.elements.len())
        .find(|&e| segment.binary(e).is_some())
        .expect("binary pain.001 element");
    assert_eq!(segment.get(binary_at - 1, 0), descriptor);
    let wire = segment.binary(binary_at).unwrap();
    assert_eq!(
        wire,
        xml.as_bytes(),
        "the binary element carries the pain.001 unchanged"
    );
    let pain = parse_pain001(std::str::from_utf8(wire).unwrap(), version);
    assert_eq!(pain.namespace, descriptor);
    Sent {
        segment,
        binary_at,
        pain,
    }
}

/// Source side of every order: never swapped with the recipient.
fn assert_debtor(p: &Parsed, owner: &str) {
    assert_eq!(p.initiator, owner);
    assert_eq!(p.debtor_name, owner);
    assert_eq!(p.debtor_iban, SOURCE_IBAN);
    assert_eq!(p.debtor_bic.as_deref(), Some(SOURCE_BIC));
}

struct Variant {
    seg_id: &'static str,
    version: PainVersion,
    instant: bool,
    date: Option<&'static str>,
}

const VARIANTS: &[Variant] = &[
    Variant {
        seg_id: "HKCCS",
        version: PainVersion::V03,
        instant: false,
        date: None,
    },
    Variant {
        seg_id: "HKCCS",
        version: PainVersion::V09,
        instant: false,
        date: None,
    },
    Variant {
        seg_id: "HKCSE",
        version: PainVersion::V03,
        instant: false,
        date: Some("2026-12-24"),
    },
    Variant {
        seg_id: "HKIPZ",
        version: PainVersion::V09,
        instant: true,
        date: None,
    },
];

#[test]
fn single_transfers_carry_exactly_what_was_entered() {
    let mut n = 0usize;
    for v in VARIANTS {
        for &(name, want_name) in NAMES {
            for &(iban, want_iban) in IBANS {
                for &amount in AMOUNTS {
                    let (purpose, want_purpose) = PURPOSES[n % PURPOSES.len()];
                    let bic = ["COBADEFFXXX", ""][n % 2];
                    let e2e = format!("GC{n:06}");
                    n += 1;
                    let job = TransferJob {
                        source_iban: SOURCE_IBAN.into(),
                        source_owner: "Max Mustermann-Groß".into(),
                        recipient_name: name.into(),
                        recipient_iban: iban.into(),
                        recipient_bic: (!bic.is_empty()).then(|| bic.into()),
                        amount_cents: amount,
                        purpose: purpose.into(),
                        instant: v.instant,
                        execution_date: v.date.map(Into::into),
                        end_to_end_id: e2e.clone(),
                    };
                    let order = transfer_order(&job, &source(), v.seg_id, v.version).unwrap();
                    let sent = send(order, v.version, None);
                    let p = &sent.pain;
                    let case = format!("{} {name:?} {iban:?} {amount}", v.seg_id);
                    assert_eq!(
                        p.txs,
                        [Tx {
                            end_to_end_id: e2e,
                            amount_cents: amount,
                            creditor_name: want_name.into(),
                            creditor_iban: want_iban.into(),
                            creditor_bic: (!bic.is_empty()).then(|| bic.into()),
                            purpose: want_purpose.map(Into::into),
                        }],
                        "{case}"
                    );
                    assert_eq!(p.nb_of_txs, ["1", "1"], "{case}");
                    assert_eq!(p.ctrl_sums, [amount, amount], "{case}");
                    assert_debtor(p, "Max Mustermann-Gross");
                    assert_eq!(p.instant, v.instant, "{case}");
                    assert_eq!(p.execution_date, v.date.unwrap_or("1999-01-01"), "{case}");
                    assert_eq!(p.batch_booking, "false", "{case}");
                    // Nothing follows the pain.001 in a single transfer.
                    assert_eq!(sent.binary_at, sent.segment.elements.len() - 1, "{case}");
                }
            }
        }
    }
    assert_eq!(
        n,
        VARIANTS.len() * NAMES.len() * IBANS.len() * AMOUNTS.len()
    );
}

#[test]
fn an_empty_owner_and_end_to_end_id_get_the_placeholders() {
    let job = TransferJob {
        source_iban: SOURCE_IBAN.into(),
        source_owner: "  ".into(),
        recipient_name: "Erika Mustermann".into(),
        recipient_iban: "DE89370400440532013000".into(),
        recipient_bic: None,
        amount_cents: 500,
        purpose: String::new(),
        instant: false,
        execution_date: None,
        end_to_end_id: String::new(),
    };
    let order = transfer_order(&job, &source(), "HKCCS", PainVersion::V03).unwrap();
    let p = send(order, PainVersion::V03, None).pain;
    assert_debtor(&p, "Kontoinhaber");
    assert_eq!(p.txs[0].end_to_end_id, "NOTPROVIDED");
}

#[test]
fn a_mistyped_iban_or_a_bad_amount_never_becomes_an_order() {
    let job = |iban: &str, name: &str, cents: i64| TransferJob {
        source_iban: SOURCE_IBAN.into(),
        source_owner: "Max".into(),
        recipient_name: name.into(),
        recipient_iban: iban.into(),
        recipient_bic: None,
        amount_cents: cents,
        purpose: "Test".into(),
        instant: false,
        execution_date: None,
        end_to_end_id: "E2E".into(),
    };
    let rejected =
        |j: TransferJob| transfer_order(&j, &source(), "HKCCS", PainVersion::V03).is_err();
    // One digit off, swapped digits, wrong check digits, too short, empty.
    for iban in [
        "DE89370400440532013001",
        "DE89370400440532010300",
        "DE88370400440532013000",
        "DE8937040044053201300",
        "",
    ] {
        assert!(rejected(job(iban, "Erika", 100)), "{iban:?}");
    }
    for cents in [0, -1, -100] {
        assert!(
            rejected(job("DE89370400440532013000", "Erika", cents)),
            "{cents}"
        );
    }
    for name in ["", "   "] {
        assert!(
            rejected(job("DE89370400440532013000", name, 100)),
            "{name:?}"
        );
    }
    let batch = |p: BatchPayment| BatchTransferJob {
        source_iban: SOURCE_IBAN.into(),
        source_owner: "Max".into(),
        payments: vec![payment(0), p],
        batch_booking: true,
        execution_date: None,
    };
    let bad = payment(1);
    for p in [
        BatchPayment {
            iban: "DE88370400440532013000".into(),
            ..bad.clone()
        },
        BatchPayment {
            amount_cents: 0,
            ..bad.clone()
        },
        BatchPayment {
            amount_cents: sepa::MAX_AMOUNT_CENTS + 1,
            ..bad.clone()
        },
        BatchPayment {
            name: " ".into(),
            ..bad
        },
    ] {
        assert!(
            batch_order(&batch(p.clone()), &source(), PainVersion::V03).is_err(),
            "{p:?}"
        );
    }
}

/// Payment `i` of a batch, each one different in every field.
fn payment(i: usize) -> BatchPayment {
    let (name, _) = NAMES[i % NAMES.len()];
    let (iban, _) = IBANS[i % IBANS.len()];
    let (purpose, _) = PURPOSES[i % PURPOSES.len()];
    BatchPayment {
        name: name.into(),
        iban: iban.into(),
        bic: i.is_multiple_of(3).then(|| "INGDDEFFXXX".into()),
        amount_cents: AMOUNTS[i % (AMOUNTS.len() - 1)] + i as i64,
        purpose: purpose.into(),
        end_to_end_id: if i % 4 == 3 {
            String::new()
        } else {
            format!("B{i:04}")
        },
    }
}

#[test]
fn batch_transfers_keep_every_payment_in_order_and_the_sum_in_the_segment() {
    for version in [PainVersion::V03, PainVersion::V09] {
        for batch_booking in [true, false] {
            for count in [1usize, 2, 7, 25] {
                let payments: Vec<BatchPayment> = (0..count).map(payment).collect();
                let total: i64 = payments.iter().map(|p| p.amount_cents).sum();
                let job = BatchTransferJob {
                    source_iban: SOURCE_IBAN.into(),
                    source_owner: "Verein e.V.".into(),
                    payments: payments.clone(),
                    batch_booking,
                    execution_date: Some("2026-12-01".into()),
                };
                let order = batch_order(&job, &source(), version).unwrap();
                let sent = send(
                    order,
                    version,
                    Some(BatchParams {
                        max_count: 500,
                        sum_required: true,
                        single_booking_allowed: true,
                    }),
                );
                let p = &sent.pain;
                let case = format!("{version:?} booking={batch_booking} n={count}");
                let want: Vec<Tx> = (0..count)
                    .map(|i| Tx {
                        end_to_end_id: if i % 4 == 3 {
                            "NOTPROVIDED".into()
                        } else {
                            format!("B{i:04}")
                        },
                        amount_cents: payments[i].amount_cents,
                        creditor_name: NAMES[i % NAMES.len()].1.into(),
                        creditor_iban: IBANS[i % IBANS.len()].1.into(),
                        creditor_bic: i.is_multiple_of(3).then(|| "INGDDEFFXXX".into()),
                        purpose: PURPOSES[i % PURPOSES.len()].1.map(Into::into),
                    })
                    .collect();
                assert_eq!(p.txs, want, "{case}");
                assert_eq!(
                    p.nb_of_txs,
                    [count.to_string(), count.to_string()],
                    "{case}"
                );
                assert_eq!(p.ctrl_sums, [total, total], "{case}");
                assert_debtor(p, "Verein e.V.");
                assert_eq!(p.batch_booking, batch_booking.to_string(), "{case}");
                assert_eq!(p.execution_date, "2026-12-01", "{case}");
                // Summenfeld (amount with a decimal comma, EUR) and "one booking per payment".
                let s = &sent.segment;
                assert_eq!(
                    s.get(2, 0),
                    format!("{},{:02}", total / 100, total % 100),
                    "{case}"
                );
                assert_eq!(s.get(2, 1), "EUR", "{case}");
                assert_eq!(s.get(3, 0), if batch_booking { "N" } else { "J" }, "{case}");
                assert_eq!(sent.binary_at, 5, "{case}");
            }
        }
    }
}

#[test]
fn standing_orders_carry_the_recipient_and_the_schedule_after_the_pain001() {
    for (i, &(name, want_name)) in NAMES.iter().enumerate() {
        let (iban, want_iban) = IBANS[i % IBANS.len()];
        let (purpose, want_purpose) = PURPOSES[i % PURPOSES.len()];
        let amount = AMOUNTS[i % AMOUNTS.len()];
        let last = (i % 2 == 0).then(|| "2027-11-01".to_string());
        let job = StandingOrderJob {
            action: StandingOrderAction::Create,
            source_iban: SOURCE_IBAN.into(),
            source_owner: "Max Mustermann".into(),
            order_id: String::new(),
            recipient_name: name.into(),
            recipient_iban: iban.into(),
            recipient_bic: None,
            amount_cents: amount,
            purpose: purpose.into(),
            first_date: "2026-12-01".into(),
            last_date: last.clone(),
            time_unit: "M".into(),
            interval: 1,
            exec_day: 1,
        };
        let order = standing_order(&job, &source(), "HKCDE", PainVersion::V03, false).unwrap();
        let sent = send(order, PainVersion::V03, None);
        let p = &sent.pain;
        assert_eq!(
            p.txs,
            [Tx {
                end_to_end_id: "NOTPROVIDED".into(),
                amount_cents: amount,
                creditor_name: want_name.into(),
                creditor_iban: want_iban.into(),
                creditor_bic: None,
                purpose: want_purpose.map(Into::into),
            }],
            "{name:?}"
        );
        assert_eq!(p.ctrl_sums, [amount, amount]);
        assert_debtor(p, "Max Mustermann");
        let schedule: Vec<&str> = sent
            .segment
            .element(sent.binary_at + 1)
            .iter()
            .map(|v| v.text())
            .collect();
        let mut want = vec!["20261201", "M", "1", "1"];
        if last.is_some() {
            want.push("20271101");
        }
        assert_eq!(schedule, want, "{name:?}");
    }
}
