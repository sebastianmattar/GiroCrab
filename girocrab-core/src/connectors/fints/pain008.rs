//! SEPA direct debit (`pain.008`) generator for HKDSE / HKDME (CORE, COR1) and
//! HKBSE / HKBME (B2B), plus Creditor Identifier validation (GitHub #143).
//!
//! Like the credit transfer builder it emits one `PmtInf` per message: one
//! scheme (`LclInstrm`), one sequence type, one collection date. A caller that
//! mixes sequence types or schemes builds one message per group.

use super::sepa::{
    format_amount, is_sepa_identifier, sanitize_sepa_text, validate_bic, xml_escape,
};
use super::standing::{tag_start, tag_text};
use crate::error::{CoreError, CoreResult};
use crate::i18n::{tr, trf};
use crate::iban::{normalize_iban, validate_iban};

pub const PAIN008_02_DESCRIPTOR: &str = "urn:iso:std:iso:20022:tech:xsd:pain.008.003.02";
pub const PAIN008_08_DESCRIPTOR: &str = "urn:iso:std:iso:20022:tech:xsd:pain.008.001.08";

/// pain.008 schema version a bank accepts (BPD lists the descriptors).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebitVersion {
    /// `pain.008.003.02`, the long-standing DK format.
    V02,
    /// `pain.008.001.08`.
    V08,
}

impl DebitVersion {
    pub fn descriptor(self) -> &'static str {
        match self {
            DebitVersion::V02 => PAIN008_02_DESCRIPTOR,
            DebitVersion::V08 => PAIN008_08_DESCRIPTOR,
        }
    }

    /// Picks from the descriptors a bank advertises; prefers `.003.02` (the
    /// one every bank knows), an empty list (unknown) also means `.003.02`.
    pub fn pick(advertised: &[String]) -> Option<Self> {
        let has = |d: &str| advertised.iter().any(|a| a == d);
        if advertised.is_empty() || has(PAIN008_02_DESCRIPTOR) {
            Some(DebitVersion::V02)
        } else if has(PAIN008_08_DESCRIPTOR) {
            Some(DebitVersion::V08)
        } else {
            None
        }
    }
}

/// Direct debit scheme (`LclInstrm`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebitScheme {
    Core,
    Cor1,
    B2b,
}

impl DebitScheme {
    pub fn code(self) -> &'static str {
        match self {
            DebitScheme::Core => "CORE",
            DebitScheme::Cor1 => "COR1",
            DebitScheme::B2b => "B2B",
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "CORE" => Some(DebitScheme::Core),
            "COR1" => Some(DebitScheme::Cor1),
            "B2B" => Some(DebitScheme::B2b),
            _ => None,
        }
    }
}

/// Sequence type (`SeqTp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SequenceType {
    First,
    Recurring,
    OneOff,
    Final,
}

impl SequenceType {
    pub fn code(self) -> &'static str {
        match self {
            SequenceType::First => "FRST",
            SequenceType::Recurring => "RCUR",
            SequenceType::OneOff => "OOFF",
            SequenceType::Final => "FNAL",
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "FRST" => Some(SequenceType::First),
            "RCUR" => Some(SequenceType::Recurring),
            "OOFF" => Some(SequenceType::OneOff),
            "FNAL" => Some(SequenceType::Final),
            _ => None,
        }
    }
}

/// One collection from a debtor under a mandate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collection {
    pub debtor_name: String,
    pub debtor_iban: String,
    pub debtor_bic: Option<String>,
    pub amount_cents: i64,
    pub purpose: String,
    /// EndToEndId, unique per collection (`NOTPROVIDED` when empty).
    pub end_to_end_id: String,
    pub mandate_id: String,
    /// Date the mandate was signed, `YYYY-MM-DD`.
    pub mandate_date: String,
}

pub struct Pain008<'a> {
    pub message_id: &'a str,
    /// ISO date-time, e.g. 2026-11-09T10:15:00
    pub created: &'a str,
    /// Due date of the collection, `YYYY-MM-DD`.
    pub collection_date: &'a str,
    pub creditor_name: &'a str,
    pub creditor_iban: &'a str,
    pub creditor_bic: &'a str,
    pub creditor_id: &'a str,
    pub scheme: DebitScheme,
    pub sequence: SequenceType,
    pub collections: &'a [Collection],
    pub version: DebitVersion,
    /// `true` = one booking for the whole message (`BtchBookg` true).
    pub batch_booking: bool,
}

fn clip(s: &str, max: usize) -> String {
    sanitize_sepa_text(s)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max)
        .collect()
}

/// Creditor Identifier (Gläubiger-ID, e.g. `DE98ZZZ09999999999`): country,
/// two check digits, three business-code characters, then the national part.
/// The check digits follow ISO 7064 mod 97-10 over the identifier without the
/// business code.
pub fn validate_creditor_id(id: &str) -> bool {
    let id = id.trim().to_ascii_uppercase();
    let b = id.as_bytes();
    if !(8..=35).contains(&b.len())
        || !b[..2].iter().all(u8::is_ascii_uppercase)
        || !b[2..4].iter().all(u8::is_ascii_digit)
        || !b[4..].iter().all(u8::is_ascii_alphanumeric)
    {
        return false;
    }
    // national part + country + check digits, letters A=10 … Z=35
    super::sepa::mod97(id[7..].chars().chain(id[..4].chars())) == 1
}

/// Sum of all collections in cents; `None` on overflow.
pub fn collections_total_cents(collections: &[Collection]) -> Option<i64> {
    collections
        .iter()
        .try_fold(0i64, |acc, c| acc.checked_add(c.amount_cents))
}

/// Checks one collection; the message names the 1-based position.
pub fn validate_collection(position: usize, c: &Collection) -> CoreResult<()> {
    let fail = |msg: &str| CoreError::invalid(trf("Lastschrift {0}: {1}", &[&position, &msg]));
    if c.debtor_name.trim().is_empty() {
        return Err(fail(tr("Der Name des Zahlungspflichtigen fehlt.")));
    }
    if !validate_iban(&c.debtor_iban) {
        return Err(fail(tr("Die IBAN des Zahlungspflichtigen ist ungültig.")));
    }
    if c.amount_cents <= 0 {
        return Err(fail(tr("Der Betrag muss größer als 0 sein.")));
    }
    if c.amount_cents > super::sepa::MAX_AMOUNT_CENTS {
        return Err(fail(tr(
            "Der Betrag ist größer als SEPA erlaubt (999.999.999,99 €).",
        )));
    }
    if c.mandate_id.trim().is_empty() || c.mandate_id.trim().chars().count() > 35 {
        return Err(fail(tr(
            "Die Mandatsreferenz fehlt oder ist länger als 35 Zeichen.",
        )));
    }
    if !is_sepa_identifier(c.mandate_id.trim()) {
        return Err(fail(tr(
            "Die Mandatsreferenz enthält Zeichen, die SEPA nicht erlaubt (nur Buchstaben ohne Umlaute, Ziffern und / - ? : ( ) . , ' + Leerzeichen).",
        )));
    }
    if jiff::civil::Date::strptime("%Y-%m-%d", &c.mandate_date).is_err() {
        return Err(fail(tr("Das Datum der Mandatsunterschrift fehlt.")));
    }
    Ok(())
}

/// Builds a `pain.008` document (UTF-8) with one `PmtInf` and one
/// `DrctDbtTxInf` per collection; `NbOfTxs` and `CtrlSum` in header and block.
pub fn build_pain008(p: &Pain008) -> CoreResult<String> {
    if p.collections.is_empty() {
        return Err(CoreError::invalid(tr(
            "Der Einzug enthält keine Lastschrift.",
        )));
    }
    if !validate_creditor_id(p.creditor_id) {
        return Err(CoreError::invalid(tr("Die Gläubiger-ID ist ungültig.")));
    }
    if !validate_iban(p.creditor_iban) {
        return Err(CoreError::invalid(tr(
            "Die IBAN des Gläubigers ist ungültig.",
        )));
    }
    for (i, c) in p.collections.iter().enumerate() {
        validate_collection(i + 1, c)?;
    }
    let total = collections_total_cents(p.collections)
        .ok_or_else(|| CoreError::invalid(tr("Die Summe des Einzugs ist zu groß.")))?;
    let sum = format_amount(total);
    let count = p.collections.len();
    let msg_id = xml_escape(&clip(p.message_id, 35));
    let creditor = xml_escape(&clip(p.creditor_name, 70));
    let c_iban = normalize_iban(p.creditor_iban);
    let bic_tag = match p.version {
        DebitVersion::V02 => "BIC",
        DebitVersion::V08 => "BICFI",
    };
    let agent = |bic: &str| {
        if validate_bic(bic) {
            format!("<{bic_tag}>{}</{bic_tag}>", normalize_iban(bic))
        } else {
            "<Othr><Id>NOTPROVIDED</Id></Othr>".to_string()
        }
    };
    let mut txs = String::new();
    for c in p.collections {
        let e2e = if c.end_to_end_id.trim().is_empty() {
            "NOTPROVIDED".to_string()
        } else {
            xml_escape(&clip(&c.end_to_end_id, 35))
        };
        let purpose = xml_escape(&clip(&c.purpose, 140));
        let rmt = if purpose.is_empty() {
            String::new()
        } else {
            format!("<RmtInf><Ustrd>{purpose}</Ustrd></RmtInf>")
        };
        txs.push_str(&format!(
            "<DrctDbtTxInf><PmtId><EndToEndId>{e2e}</EndToEndId></PmtId>\
<InstdAmt Ccy=\"EUR\">{amount}</InstdAmt>\
<DrctDbtTx><MndtRltdInf><MndtId>{mandate}</MndtId><DtOfSgntr>{signed}</DtOfSgntr></MndtRltdInf></DrctDbtTx>\
<DbtrAgt><FinInstnId>{d_agent}</FinInstnId></DbtrAgt>\
<Dbtr><Nm>{name}</Nm></Dbtr><DbtrAcct><Id><IBAN>{iban}</IBAN></Id></DbtrAcct>{rmt}</DrctDbtTxInf>",
            amount = format_amount(c.amount_cents),
            mandate = xml_escape(c.mandate_id.trim()),
            signed = xml_escape(&c.mandate_date),
            d_agent = agent(c.debtor_bic.as_deref().unwrap_or("")),
            name = xml_escape(&clip(&c.debtor_name, 70)),
            iban = normalize_iban(&c.debtor_iban),
        ));
    }
    // 001.08 wraps nothing extra here; the two versions differ in the BIC tag.
    let ns = p.version.descriptor();
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
<Document xmlns=\"{ns}\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\">\
<CstmrDrctDbtInitn>\
<GrpHdr><MsgId>{msg_id}</MsgId><CreDtTm>{created}</CreDtTm><NbOfTxs>{count}</NbOfTxs><CtrlSum>{sum}</CtrlSum>\
<InitgPty><Nm>{creditor}</Nm></InitgPty></GrpHdr>\
<PmtInf><PmtInfId>{msg_id}</PmtInfId><PmtMtd>DD</PmtMtd><BtchBookg>{batch}</BtchBookg><NbOfTxs>{count}</NbOfTxs><CtrlSum>{sum}</CtrlSum>\
<PmtTpInf><SvcLvl><Cd>SEPA</Cd></SvcLvl><LclInstrm><Cd>{scheme}</Cd></LclInstrm><SeqTp>{seq}</SeqTp></PmtTpInf>\
<ReqdColltnDt>{due}</ReqdColltnDt>\
<Cdtr><Nm>{creditor}</Nm></Cdtr><CdtrAcct><Id><IBAN>{c_iban}</IBAN></Id></CdtrAcct>\
<CdtrAgt><FinInstnId>{c_agent}</FinInstnId></CdtrAgt><ChrgBr>SLEV</ChrgBr>\
<CdtrSchmeId><Id><PrvtId><Othr><Id>{cid}</Id><SchmeNm><Prtry>SEPA</Prtry></SchmeNm></Othr></PrvtId></Id></CdtrSchmeId>\
{txs}</PmtInf></CstmrDrctDbtInitn></Document>",
        created = xml_escape(p.created),
        batch = p.batch_booking,
        scheme = p.scheme.code(),
        seq = p.sequence.code(),
        due = xml_escape(p.collection_date),
        c_agent = agent(p.creditor_bic),
        cid = xml_escape(&p.creditor_id.trim().to_ascii_uppercase()),
    ))
}

/// One `PmtInf` of a `pain.008` file as read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedDebitBlock {
    pub creditor_id: String,
    pub creditor_iban: String,
    pub scheme: DebitScheme,
    pub sequence: SequenceType,
    /// Due date `YYYY-MM-DD`, "" if the file has none.
    pub collection_date: String,
    pub collections: Vec<Collection>,
}

/// Offsets just after every `<Tag …>` in `xml` (any prefix).
fn tag_starts(xml: &str, tag: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = tag_start(xml, tag, from) {
        out.push(i);
        from = i;
    }
    out
}

/// Text of `tag` that follows the first `<outer>` in `xml`.
fn nested(xml: &str, outer: &str, tag: &str) -> Option<String> {
    let at = tag_start(xml, outer, 0)?;
    tag_text(xml, tag, at)
}

/// Reads the `PmtInf` blocks of a direct debit file (`pain.008.*`). Tolerant
/// about prefixes and order; refuses a file without a direct debit.
pub fn parse_pain008(xml: &str) -> CoreResult<Vec<ParsedDebitBlock>> {
    let blocks = tag_starts(xml, "PmtInf");
    let mut out = Vec::new();
    for (n, &start) in blocks.iter().enumerate() {
        let end = blocks.get(n + 1).copied().unwrap_or(xml.len());
        let block = &xml[start..end];
        let txs = tag_starts(block, "DrctDbtTxInf");
        let Some(&first_tx) = txs.first() else {
            continue;
        };
        let head = &block[..first_tx];
        let scheme = tag_text(head, "Cd", tag_start(head, "LclInstrm", 0).unwrap_or(0))
            .and_then(|c| DebitScheme::from_code(&c))
            .unwrap_or(DebitScheme::Core);
        let sequence = tag_text(head, "SeqTp", 0)
            .and_then(|c| SequenceType::from_code(&c))
            .unwrap_or(SequenceType::First);
        let mut collections = Vec::new();
        for (i, &t) in txs.iter().enumerate() {
            let tx = &block[t..txs.get(i + 1).copied().unwrap_or(block.len())];
            // an unreadable amount must not turn into 0: the file would be applied
            // with a silently wrong collection
            let cents = tag_text(tx, "InstdAmt", 0)
                .and_then(|a| crate::money::parse_cents(&a).ok())
                .ok_or_else(|| {
                    CoreError::invalid(tr("Die Datei enthält einen unlesbaren Betrag (pain.008)."))
                })?;
            collections.push(Collection {
                debtor_name: nested(tx, "Dbtr", "Nm").unwrap_or_default(),
                debtor_iban: nested(tx, "DbtrAcct", "IBAN").unwrap_or_default(),
                debtor_bic: nested(tx, "DbtrAgt", "BIC")
                    .or_else(|| nested(tx, "DbtrAgt", "BICFI"))
                    .filter(|b| !b.is_empty()),
                amount_cents: cents,
                purpose: tag_text(tx, "Ustrd", 0).unwrap_or_default(),
                end_to_end_id: tag_text(tx, "EndToEndId", 0).unwrap_or_default(),
                mandate_id: tag_text(tx, "MndtId", 0).unwrap_or_default(),
                mandate_date: tag_text(tx, "DtOfSgntr", 0).unwrap_or_default(),
            });
        }
        // The block's own control figures must match what it contains; a mismatch
        // means a damaged or hand-edited file.
        let declared_count =
            tag_text(head, "NbOfTxs", 0).and_then(|c| c.trim().parse::<usize>().ok());
        let declared_sum =
            tag_text(head, "CtrlSum", 0).and_then(|c| crate::money::parse_cents(&c).ok());
        let actual_sum = collections
            .iter()
            .try_fold(0i64, |acc: i64, c: &Collection| {
                acc.checked_add(c.amount_cents)
            });
        if declared_count.is_some_and(|n| n != collections.len())
            || declared_sum.is_some_and(|s| Some(s) != actual_sum)
        {
            return Err(CoreError::invalid(tr(
                "Anzahl oder Kontrollsumme der Datei stimmen nicht mit den Lastschriften überein (pain.008).",
            )));
        }
        out.push(ParsedDebitBlock {
            creditor_id: nested(head, "CdtrSchmeId", "Id")
                .and_then(|_| {
                    let at = tag_start(head, "CdtrSchmeId", 0)?;
                    let othr = tag_start(head, "Othr", at)?;
                    tag_text(head, "Id", othr)
                })
                .unwrap_or_default(),
            creditor_iban: nested(head, "CdtrAcct", "IBAN").unwrap_or_default(),
            scheme,
            sequence,
            collection_date: tag_text(head, "ReqdColltnDt", 0)
                .map(|d| d.trim_start_matches("<Dt>").chars().take(10).collect())
                .unwrap_or_default(),
            collections,
        });
    }
    if out.is_empty() {
        return Err(CoreError::invalid(tr(
            "Die Datei enthält keine SEPA-Lastschrift (pain.008).",
        )));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collection(n: u32) -> Collection {
        Collection {
            debtor_name: format!("Mitglied {n} Müller"),
            debtor_iban: "DE89370400440532013000".into(),
            debtor_bic: None,
            amount_cents: 1250 + i64::from(n),
            purpose: "Mitgliedsbeitrag 2026".into(),
            end_to_end_id: format!("BEITRAG-{n}"),
            mandate_id: format!("M-{n:04}"),
            mandate_date: "2024-03-01".into(),
        }
    }

    fn base<'a>(cs: &'a [Collection]) -> Pain008<'a> {
        Pain008 {
            message_id: "MSG-1",
            created: "2026-11-15T10:00:00",
            collection_date: "2026-11-30",
            creditor_name: "Sportverein Nord e.V.",
            creditor_iban: "DE02120300000000202051",
            creditor_bic: "BYLADEM1001",
            creditor_id: "DE98ZZZ09999999999",
            scheme: DebitScheme::Core,
            sequence: SequenceType::Recurring,
            collections: cs,
            version: DebitVersion::V02,
            batch_booking: true,
        }
    }

    #[test]
    fn creditor_id_check_digits() {
        assert!(validate_creditor_id("DE98ZZZ09999999999")); // the Bundesbank sample
        assert!(validate_creditor_id(" de98zzz09999999999 "));
        assert!(!validate_creditor_id("DE99ZZZ09999999999")); // wrong check digits
        assert!(!validate_creditor_id("DE98ZZZ0999999999")); // one character short
        assert!(!validate_creditor_id("98DEZZZ09999999999"));
        assert!(!validate_creditor_id(""));
    }

    #[test]
    fn builds_header_block_and_every_collection() {
        let cs = [collection(1), collection(2)];
        let xml = build_pain008(&base(&cs)).unwrap();
        assert!(xml.contains(PAIN008_02_DESCRIPTOR));
        assert!(xml.contains("<NbOfTxs>2</NbOfTxs>"));
        assert!(xml.contains("<CtrlSum>25.03</CtrlSum>")); // 12.51 + 12.52
        assert!(xml.contains("<PmtMtd>DD</PmtMtd>"));
        assert!(xml.contains("<LclInstrm><Cd>CORE</Cd></LclInstrm><SeqTp>RCUR</SeqTp>"));
        assert!(xml.contains("<ReqdColltnDt>2026-11-30</ReqdColltnDt>"));
        assert!(xml.contains("<Id>DE98ZZZ09999999999</Id><SchmeNm><Prtry>SEPA</Prtry>"));
        assert_eq!(xml.matches("<DrctDbtTxInf>").count(), 2);
        assert!(xml.contains("<MndtId>M-0001</MndtId><DtOfSgntr>2024-03-01</DtOfSgntr>"));
        assert!(xml.contains("<Nm>Mitglied 1 Mueller</Nm>")); // SEPA character set
        assert!(xml.contains("<BIC>BYLADEM1001</BIC>"));
        // debtor without BIC: IBAN only
        assert!(xml.contains("<DbtrAgt><FinInstnId><Othr><Id>NOTPROVIDED</Id>"));
    }

    #[test]
    fn scheme_sequence_and_version_are_switchable() {
        let cs = [collection(1)];
        let mut p = base(&cs);
        p.scheme = DebitScheme::B2b;
        p.sequence = SequenceType::First;
        p.version = DebitVersion::V08;
        let xml = build_pain008(&p).unwrap();
        assert!(xml.contains("<LclInstrm><Cd>B2B</Cd></LclInstrm><SeqTp>FRST</SeqTp>"));
        assert!(xml.contains(PAIN008_08_DESCRIPTOR));
        assert!(xml.contains("<BICFI>BYLADEM1001</BICFI>"));
    }

    #[test]
    fn rejects_bad_input_with_position() {
        let mut cs = [collection(1), collection(2)];
        cs[1].debtor_iban = "DE00".into();
        let err = build_pain008(&base(&cs)).unwrap_err().to_string();
        assert!(err.contains('2'), "{err}");
        let cs = [collection(1)];
        let mut p = base(&cs);
        p.creditor_id = "DE00ZZZ00000000000";
        assert!(build_pain008(&p).is_err());
        let none: [Collection; 0] = [];
        assert!(build_pain008(&base(&none)).is_err());
        let mut cs = [collection(1)];
        cs[0].mandate_id = String::new();
        assert!(build_pain008(&base(&cs)).is_err());
        cs[0].mandate_id = "Müller_1".into();
        assert!(build_pain008(&base(&cs)).is_err());
        cs[0].mandate_id = "M-1".into();
        cs[0].amount_cents = 0;
        assert!(build_pain008(&base(&cs)).is_err());
    }

    #[test]
    fn picks_the_version_from_the_bpd() {
        let d = |s: &str| s.to_string();
        assert_eq!(DebitVersion::pick(&[]), Some(DebitVersion::V02));
        assert_eq!(
            DebitVersion::pick(&[d(PAIN008_08_DESCRIPTOR)]),
            Some(DebitVersion::V08)
        );
        assert_eq!(
            DebitVersion::pick(&[d(PAIN008_08_DESCRIPTOR), d(PAIN008_02_DESCRIPTOR)]),
            Some(DebitVersion::V02)
        );
        assert_eq!(DebitVersion::pick(&[d("urn:other")]), None);
    }

    #[test]
    fn reads_back_what_it_writes() {
        let cs = [collection(1), collection(2)];
        for version in [DebitVersion::V02, DebitVersion::V08] {
            let mut p = base(&cs);
            p.version = version;
            p.scheme = DebitScheme::B2b;
            p.sequence = SequenceType::First;
            let blocks = parse_pain008(&build_pain008(&p).unwrap()).unwrap();
            assert_eq!(blocks.len(), 1);
            let b = &blocks[0];
            assert_eq!(b.creditor_id, "DE98ZZZ09999999999");
            assert_eq!(b.creditor_iban, "DE02120300000000202051");
            assert_eq!(
                (b.scheme, b.sequence),
                (DebitScheme::B2b, SequenceType::First)
            );
            assert_eq!(b.collection_date, "2026-11-30");
            assert_eq!(b.collections.len(), 2);
            let c = &b.collections[1];
            assert_eq!(c.debtor_name, "Mitglied 2 Mueller");
            assert_eq!(c.debtor_iban, "DE89370400440532013000");
            assert_eq!(c.amount_cents, 1252);
            assert_eq!(c.mandate_id, "M-0002");
            assert_eq!(c.mandate_date, "2024-03-01");
            assert_eq!(c.purpose, "Mitgliedsbeitrag 2026");
        }
        assert!(parse_pain008("<Document/>").is_err());
    }

    #[test]
    fn control_figures_must_match_the_collections() {
        let cs = [collection(1), collection(2)];
        let xml = build_pain008(&base(&cs)).unwrap();
        assert!(parse_pain008(&xml).is_ok());
        // control sum: 12.51 + 12.52 = 25.03 in the block
        let bad_sum = xml.replacen("<CtrlSum>25.03</CtrlSum>", "<CtrlSum>99.99</CtrlSum>", 2);
        assert!(parse_pain008(&bad_sum).is_err());
        let bad_count = xml.replacen("<NbOfTxs>2</NbOfTxs>", "<NbOfTxs>3</NbOfTxs>", 2);
        assert!(parse_pain008(&bad_count).is_err());
        let bad_amount = xml.replacen("12.51", "zwölf", 1);
        assert!(parse_pain008(&bad_amount).is_err());
    }

    /// Developer aid: writes sample files for every scheme/version into the
    /// directory in `GIROCRAB_PAIN008_DUMP`, to validate them against the
    /// published XSDs (`xmllint --noout --schema pain.008.003.02.xsd file.xml`).
    #[test]
    #[ignore]
    fn dump_samples_for_schema_validation() {
        let Ok(dir) = std::env::var("GIROCRAB_PAIN008_DUMP") else {
            return;
        };
        let cs = [collection(1), collection(2)];
        for (version, tag) in [(DebitVersion::V02, "003.02"), (DebitVersion::V08, "001.08")] {
            for scheme in [DebitScheme::Core, DebitScheme::Cor1, DebitScheme::B2b] {
                let mut p = base(&cs);
                p.version = version;
                p.scheme = scheme;
                let xml = build_pain008(&p).unwrap();
                std::fs::write(format!("{dir}/pain008-{tag}-{}.xml", scheme.code()), xml).unwrap();
            }
        }
        // creditor and debtor with a BIC, no purpose
        let mut with_bic = collection(3);
        with_bic.debtor_bic = Some("COBADEFFXXX".into());
        with_bic.purpose = String::new();
        let one = [with_bic];
        let mut p = base(&one);
        p.sequence = SequenceType::OneOff;
        std::fs::write(
            format!("{dir}/pain008-003.02-bic.xml"),
            build_pain008(&p).unwrap(),
        )
        .unwrap();
    }
}
