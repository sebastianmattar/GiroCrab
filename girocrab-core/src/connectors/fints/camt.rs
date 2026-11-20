//! ISO 20022 camt.052 (Bank-to-Customer Account Report) parser for HICAZ.
//!
//! Supports camt.052.001.02 … .08 (party names under `Dbtr/Nm` as well as
//! `Dbtr/Pty/Nm`). Uses a tiny built-in XML tokenizer: camt is plain,
//! well-formed XML, so no external XML dependency is needed.

use crate::connectors::RemoteTransaction;

/// `external_id` prefix of not yet booked FinTS entries ("vorgemerkt").
/// Persist keeps exactly the ones the latest fetch reports (`hold_prefix`);
/// `Booking.is_pending` relies on the `pending-` start.
pub const HOLD_PREFIX: &str = "pending-hold-fints-";
use crate::money::parse_cents;

#[derive(Debug, PartialEq)]
enum Tok {
    Start(String, Vec<(String, String)>),
    End(String),
    Text(String),
}

fn local(name: &str) -> String {
    name.rsplit(':').next().unwrap_or(name).to_string()
}

pub fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find('&') {
        out.push_str(&rest[..p]);
        let tail = &rest[p..];
        let end = tail.find(';').unwrap_or(0);
        let ent = &tail[1..end.max(1)];
        let rep = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            e if e.starts_with("#x") => u32::from_str_radix(&e[2..], 16)
                .ok()
                .and_then(char::from_u32),
            e if e.starts_with('#') => e[1..].parse::<u32>().ok().and_then(char::from_u32),
            _ => None,
        };
        match (rep, end) {
            (Some(c), e) if e > 0 => {
                out.push(c);
                rest = &tail[end + 1..];
            }
            _ => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn tokenize(xml: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut i = 0;
    let b = xml.as_bytes();
    while i < b.len() {
        if b[i] == b'<' {
            let rest = &xml[i..];
            if rest.starts_with("<?") {
                i += rest.find("?>").map(|p| p + 2).unwrap_or(rest.len());
            } else if rest.starts_with("<!--") {
                i += rest.find("-->").map(|p| p + 3).unwrap_or(rest.len());
            } else if rest.starts_with("<![CDATA[") {
                let end = rest.find("]]>").unwrap_or(rest.len());
                out.push(Tok::Text(rest[9..end].to_string()));
                i += (end + 3).min(rest.len());
            } else if rest.starts_with("<!") {
                i += rest.find('>').map(|p| p + 1).unwrap_or(rest.len());
            } else if let Some(stripped) = rest.strip_prefix("</") {
                let end = stripped.find('>').unwrap_or(stripped.len());
                out.push(Tok::End(local(stripped[..end].trim())));
                i += end + 3;
            } else {
                let end = rest.find('>').unwrap_or(rest.len());
                let inner = &rest[1..end];
                let self_closing = inner.ends_with('/');
                let inner = inner.trim_end_matches('/');
                let name_end = inner
                    .find(|c: char| c.is_whitespace())
                    .unwrap_or(inner.len());
                let name = local(&inner[..name_end]);
                let mut attrs = Vec::new();
                let mut a = &inner[name_end..];
                while let Some(eq) = a.find('=') {
                    let key = local(a[..eq].trim());
                    let after = a[eq + 1..].trim_start();
                    // `x=` at the end of the tag: no value left.
                    let Some(q) = after.chars().next() else {
                        attrs.push((key, String::new()));
                        break;
                    };
                    // char-based: the opening character may be multi-byte
                    let body = &after[q.len_utf8()..];
                    let val_end = body.find(q).unwrap_or(body.len());
                    attrs.push((key, unescape(&body[..val_end])));
                    a = body.get(val_end + q.len_utf8()..).unwrap_or("");
                }
                out.push(Tok::Start(name.clone(), attrs));
                if self_closing {
                    out.push(Tok::End(name));
                }
                i += end + 1;
            }
        } else {
            let end = xml[i..].find('<').map(|p| i + p).unwrap_or(b.len());
            let t = xml[i..end].trim();
            if !t.is_empty() {
                out.push(Tok::Text(unescape(t)));
            }
            i = end;
        }
    }
    out
}

#[derive(Default)]
struct Entry {
    amount: String,
    currency: String,
    indicator: String,
    status: String,
    booking_date: String,
    value_date: String,
    reference: String,
    booking_text: String,
    end_to_end: String,
    mandate: String,
    /// `CdtrSchmeId/Id/…/Id` (SEPA creditor identifier).
    creditor_id: String,
    remittance: Vec<String>,
    debtor: String,
    creditor: String,
    debtor_iban: String,
    creditor_iban: String,
    /// `TxDtls/Purp/Cd` (SEPA purpose code, e.g. SALA).
    purpose_code: String,
    /// `BkTxCd/Domn/Fmly/SubFmlyCd` (ISO bank transaction sub-family, e.g. SALA).
    sub_family: String,
    /// `BkTxCd/Prtry/Cd` (DK format `NTRF+153+…`).
    proprietary: String,
}

#[derive(Debug, Default, Clone)]
pub struct CamtReport {
    pub closing_balance_cents: Option<i64>,
    pub closing_date: Option<String>,
    pub currency: Option<String>,
    /// Not yet booked entries of this document (`Sts` PDNG/INFO …), with
    /// [`HOLD_PREFIX`] ids.
    pub pending: Vec<RemoteTransaction>,
}

/// Geschäftsvorfallcode from a DK proprietary bank transaction code
/// (`NTRF+153+931` → `153`).
fn dk_gvc(proprietary: &str) -> String {
    proprietary
        .split('+')
        .nth(1)
        .filter(|c| c.len() == 3 && c.bytes().all(|b| b.is_ascii_digit()))
        .unwrap_or_default()
        .to_string()
}

fn ends_with(stack: &[String], tail: &[&str]) -> bool {
    stack.len() >= tail.len()
        && stack[stack.len() - tail.len()..]
            .iter()
            .zip(tail)
            .all(|(a, b)| a == b)
}

fn has(stack: &[String], name: &str) -> bool {
    stack.iter().any(|s| s == name)
}

/// Date of a card payment timestamp in a booking text (`2026-11-14T18:56:47`),
/// also when the bank wrapped the text inside it (`20 26-11-14T18:56:47`).
fn card_timestamp_date(text: &str) -> Option<String> {
    let s: Vec<char> = text.chars().filter(|c| !c.is_whitespace()).collect();
    let digit = |i: usize| s.get(i).is_some_and(|c| c.is_ascii_digit());
    for i in 10..s.len() {
        if s[i] != 'T' || !(digit(i + 1) && digit(i + 2)) || s.get(i + 3) != Some(&':') {
            continue;
        }
        let d: String = s[i - 10..i].iter().collect();
        if jiff::civil::Date::strptime("%Y-%m-%d", &d).is_ok() {
            return Some(d);
        }
    }
    None
}

/// Parses one camt.052 document into booked transactions for `account_ref`;
/// not yet booked entries go to [`CamtReport::pending`].
pub fn parse(xml: &str, account_ref: &str) -> (Vec<RemoteTransaction>, CamtReport) {
    parse_doc(xml, account_ref, false)
}

/// Parses a camt.052 document of not yet booked entries (`HICAZ` element 4):
/// every entry is pending, whatever its `Sts` says.
pub fn parse_notbooked(xml: &str, account_ref: &str) -> Vec<RemoteTransaction> {
    parse_doc(xml, account_ref, true).1.pending
}

/// Largest camt document and deepest element nesting that is parsed. A real
/// camt.052 has a nesting depth below 20; the bounds keep a hostile or broken
/// server answer from costing quadratic time (every text node rescans the
/// element stack) or unbounded memory. Over a bound the document counts as empty.
const MAX_CAMT_BYTES: usize = 32 * 1024 * 1024;
const MAX_CAMT_DEPTH: usize = 64;

fn parse_doc(
    xml: &str,
    account_ref: &str,
    all_pending: bool,
) -> (Vec<RemoteTransaction>, CamtReport) {
    if xml.len() > MAX_CAMT_BYTES {
        return (Vec::new(), CamtReport::default());
    }
    let mut stack: Vec<String> = Vec::new();
    let mut entries: Vec<Entry> = Vec::new();
    let mut cur: Option<Entry> = None;
    let mut report = CamtReport::default();
    let (mut bal_type, mut bal_amt, mut bal_ind, mut bal_date, mut bal_ccy) = (
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
    );
    let mut pending_ccy = String::new();
    for tok in tokenize(xml) {
        match tok {
            Tok::Start(name, attrs) => {
                if name == "Ntry" {
                    cur = Some(Entry::default());
                }
                if name == "Bal" {
                    bal_type.clear();
                    bal_amt.clear();
                    bal_ind.clear();
                    bal_date.clear();
                    bal_ccy.clear();
                }
                pending_ccy = attrs
                    .iter()
                    .find(|(k, _)| k == "Ccy")
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default();
                if stack.len() >= MAX_CAMT_DEPTH {
                    return (Vec::new(), CamtReport::default());
                }
                stack.push(name);
            }
            Tok::End(name) => {
                if name == "Ntry"
                    && let Some(e) = cur.take()
                {
                    entries.push(e);
                }
                if name == "Bal"
                    && matches!(bal_type.as_str(), "CLBD" | "ITBD" | "CLAV" | "ITAV")
                    && report.closing_balance_cents.is_none()
                    && let Ok(c) = parse_cents(&bal_amt)
                {
                    report.closing_balance_cents = Some(if bal_ind == "DBIT" { -c } else { c });
                    report.closing_date = Some(bal_date.chars().take(10).collect());
                    report.currency = Some(bal_ccy.clone()).filter(|c| !c.is_empty());
                }
                // tolerate sloppy nesting
                if let Some(pos) = stack.iter().rposition(|s| *s == name) {
                    stack.truncate(pos);
                }
            }
            Tok::Text(text) => {
                if has(&stack, "Bal") && !has(&stack, "Ntry") {
                    if ends_with(&stack, &["Tp", "CdOrPrtry", "Cd"]) {
                        bal_type = text;
                    } else if ends_with(&stack, &["Bal", "Amt"]) {
                        bal_amt = text;
                        bal_ccy = pending_ccy.clone();
                    } else if ends_with(&stack, &["Bal", "CdtDbtInd"]) {
                        bal_ind = text;
                    } else if ends_with(&stack, &["Bal", "Dt", "Dt"])
                        || ends_with(&stack, &["Bal", "Dt", "DtTm"])
                    {
                        bal_date = text;
                    }
                    continue;
                }
                let Some(e) = cur.as_mut() else { continue };
                if ends_with(&stack, &["Ntry", "Amt"]) {
                    e.amount = text;
                    e.currency = pending_ccy.clone();
                } else if ends_with(&stack, &["Ntry", "CdtDbtInd"]) {
                    e.indicator = text;
                } else if ends_with(&stack, &["Ntry", "Sts"])
                    || ends_with(&stack, &["Ntry", "Sts", "Cd"])
                {
                    e.status = text;
                } else if ends_with(&stack, &["BookgDt", "Dt"])
                    || ends_with(&stack, &["BookgDt", "DtTm"])
                {
                    e.booking_date = text.chars().take(10).collect();
                } else if ends_with(&stack, &["ValDt", "Dt"])
                    || ends_with(&stack, &["ValDt", "DtTm"])
                {
                    e.value_date = text.chars().take(10).collect();
                } else if ends_with(&stack, &["Ntry", "AcctSvcrRef"]) {
                    e.reference = text;
                } else if ends_with(&stack, &["Ntry", "AddtlNtryInf"]) {
                    e.booking_text = text;
                } else if ends_with(&stack, &["AddtlTxInf"]) && e.booking_text.is_empty() {
                    e.booking_text = text;
                } else if ends_with(&stack, &["Refs", "EndToEndId"]) {
                    e.end_to_end = text;
                } else if ends_with(&stack, &["Refs", "MndtId"]) {
                    e.mandate = text;
                } else if (has(&stack, "CdtrSchmeId")
                    || (has(&stack, "Cdtr") && has(&stack, "PrvtId") && has(&stack, "Othr")))
                    && stack.last().map(|s| s == "Id").unwrap_or(false)
                {
                    if e.creditor_id.is_empty() {
                        e.creditor_id = text.trim().to_string();
                    }
                } else if ends_with(&stack, &["Purp", "Cd"]) {
                    if e.purpose_code.is_empty() {
                        e.purpose_code = text.trim().to_ascii_uppercase();
                    }
                } else if ends_with(&stack, &["BkTxCd", "Domn", "Fmly", "SubFmlyCd"]) {
                    if e.sub_family.is_empty() {
                        e.sub_family = text.trim().to_ascii_uppercase();
                    }
                } else if ends_with(&stack, &["BkTxCd", "Prtry", "Cd"]) {
                    if e.proprietary.is_empty() {
                        e.proprietary = text.trim().to_string();
                    }
                } else if ends_with(&stack, &["RmtInf", "Ustrd"]) {
                    e.remittance.push(text);
                } else if has(&stack, "RltdPties")
                    && stack.last().map(|s| s == "Nm").unwrap_or(false)
                {
                    if has(&stack, "Dbtr") && e.debtor.is_empty() {
                        e.debtor = text;
                    } else if has(&stack, "Cdtr") && e.creditor.is_empty() {
                        e.creditor = text;
                    }
                } else if ends_with(&stack, &["IBAN"]) {
                    if has(&stack, "DbtrAcct") {
                        e.debtor_iban = text;
                    } else if has(&stack, "CdtrAcct") {
                        e.creditor_iban = text;
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    let mut seen: std::collections::HashMap<String, u32> = Default::default();
    // Some banks (ING) reuse one `AcctSvcrRef` for several entries: it is an
    // id only on its first occurrence in the document (#445).
    let mut seen_refs: std::collections::HashSet<String> = Default::default();
    let mut seen_pending: std::collections::HashMap<String, u32> = Default::default();
    for e in entries {
        let status = e.status.trim();
        let pending = all_pending || !(status.is_empty() || status.eq_ignore_ascii_case("BOOK"));
        let Ok(abs) = parse_cents(&e.amount) else {
            continue;
        };
        let debit = e.indicator == "DBIT";
        let amount = if debit { -abs.abs() } else { abs.abs() };
        let (cp, iban) = if debit {
            (e.creditor.clone(), e.creditor_iban.clone())
        } else {
            (e.debtor.clone(), e.debtor_iban.clone())
        };
        let ustrd = e.remittance.join(" ");
        let mut purpose = String::new();
        if !e.end_to_end.is_empty() && e.end_to_end != "NOTPROVIDED" {
            purpose.push_str(&format!("EREF+{} ", e.end_to_end));
        }
        if !e.mandate.is_empty() {
            purpose.push_str(&format!("MREF+{} ", e.mandate));
        }
        if purpose.is_empty() {
            purpose = ustrd;
        } else {
            purpose.push_str(&format!("SVWZ+{ustrd}"));
        }
        let mut t = RemoteTransaction {
            account_ref: account_ref.to_string(),
            external_id: String::new(),
            booking_date: if e.booking_date.is_empty() {
                e.value_date.clone()
            } else {
                e.booking_date.clone()
            },
            value_date: e.value_date.clone(),
            amount_cents: amount,
            currency: if e.currency.is_empty() {
                "EUR".into()
            } else {
                e.currency.clone()
            },
            counterparty: cp,
            counterparty_iban: iban,
            purpose: purpose.trim().to_string(),
            booking_text: e.booking_text.clone(),
            purpose_code: if e.purpose_code.is_empty() && e.sub_family == "SALA" {
                "SALA".into()
            } else {
                e.purpose_code.clone()
            },
            gvc: dk_gvc(&e.proprietary),
            end_to_end_id: if e.end_to_end.trim().eq_ignore_ascii_case("NOTPROVIDED") {
                String::new()
            } else {
                e.end_to_end.trim().to_string()
            },
            mandate_id: e.mandate.trim().to_string(),
            creditor_id: e.creditor_id.clone(),
            ..Default::default()
        };
        if pending {
            // Not booked yet: banks give holds no stable reference, and often no
            // date (comdirect). The id is content-based and leaves a missing date
            // out, so it stays the same from day to day; the date comes from the
            // card timestamp in the text, else today.
            if t.booking_text.is_empty() {
                t.booking_text = crate::i18n::tr("Vorgemerkt").into();
            }
            let key = format!(
                "{}{}{}{}",
                t.booking_date, t.amount_cents, t.counterparty, t.purpose
            );
            let n = seen_pending.entry(key).or_insert(0);
            t.external_id = format!("{HOLD_PREFIX}{}", t.content_id(*n));
            *n += 1;
            if t.booking_date.is_empty() {
                t.booking_date =
                    card_timestamp_date(&t.purpose).unwrap_or_else(crate::connectors::today);
            }
            report.pending.push(t);
            continue;
        }
        let key = format!(
            "{}{}{}{}",
            t.booking_date, t.amount_cents, t.counterparty, t.purpose
        );
        let n = seen.entry(key).or_insert(0);
        let reference = e.reference.trim();
        t.external_id = if !reference.is_empty()
            && reference != "NONREF"
            && seen_refs.insert(reference.to_string())
        {
            format!("camt:{reference}")
        } else {
            t.content_id(*n)
        };
        *n += 1;
        out.push(t);
    }
    (out, report)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const SAMPLE_08: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Document xmlns="urn:iso:std:iso:20022:tech:xsd:camt.052.001.08">
 <BkToCstmrAcctRpt><Rpt>
  <Acct><Id><IBAN>DE44520900000012345601</IBAN></Id></Acct>
  <Bal><Tp><CdOrPrtry><Cd>CLBD</Cd></CdOrPrtry></Tp><Amt Ccy="EUR">1234.56</Amt><CdtDbtInd>CRDT</CdtDbtInd><Dt><Dt>2026-11-09</Dt></Dt></Bal>
  <Ntry>
   <Amt Ccy="EUR">31.96</Amt><CdtDbtInd>DBIT</CdtDbtInd><Sts><Cd>BOOK</Cd></Sts>
   <BookgDt><Dt>2026-11-07</Dt></BookgDt><ValDt><Dt>2026-11-07</Dt></ValDt>
   <AcctSvcrRef>2026110712345</AcctSvcrRef>
   <BkTxCd><Domn><Cd>PMNT</Cd><Fmly><Cd>CCRD</Cd><SubFmlyCd>POSD</SubFmlyCd></Fmly></Domn><Prtry><Cd>NMSC+106+931</Cd><Issr>DK</Issr></Prtry></BkTxCd>
   <AddtlNtryInf>Kartenzahlung</AddtlNtryInf>
   <NtryDtls><TxDtls>
     <Refs><EndToEndId>NOTPROVIDED</EndToEndId></Refs>
     <RltdPties><Cdtr><Pty><Nm>Lidl Dienstleistung &amp; Co. KG</Nm></Pty></Cdtr><CdtrAcct><Id><IBAN>DE12300700100123456700</IBAN></Id></CdtrAcct></RltdPties>
     <RmtInf><Ustrd>Lidl sagt Danke</Ustrd><Ustrd>Kassel</Ustrd></RmtInf>
   </TxDtls></NtryDtls>
  </Ntry>
  <Ntry>
   <Amt Ccy="EUR">4180.00</Amt><CdtDbtInd>CRDT</CdtDbtInd><Sts>BOOK</Sts>
   <BookgDt><DtTm>2026-10-27T08:00:00</DtTm></BookgDt>
   <NtryDtls><TxDtls>
     <Refs><EndToEndId>NT-2026-10</EndToEndId><MndtId>M-1</MndtId></Refs>
     <RltdPties><Dbtr><Nm>Nordhessen Technik GmbH</Nm></Dbtr></RltdPties>
     <RmtInf><Ustrd>Gehalt Oktober</Ustrd></RmtInf>
     <Purp><Cd>SALA</Cd></Purp>
   </TxDtls></NtryDtls>
  </Ntry>
  <Ntry><Amt Ccy="EUR">9.99</Amt><CdtDbtInd>DBIT</CdtDbtInd><Sts><Cd>PDNG</Cd></Sts><BookgDt><Dt>2026-11-09</Dt></BookgDt></Ntry>
 </Rpt></BkToCstmrAcctRpt>
</Document>"#;

    #[test]
    fn repeated_acct_svcr_ref_falls_back_to_content_id() {
        let entry = |amt: &str, purpose: &str, sts: &str| {
            format!(
                "<Ntry><Amt Ccy=\"EUR\">{amt}</Amt><CdtDbtInd>DBIT</CdtDbtInd><Sts>{sts}</Sts>\
                 <BookgDt><Dt>2026-11-07</Dt></BookgDt><AcctSvcrRef>REF1</AcctSvcrRef>\
                 <NtryDtls><TxDtls><RmtInf><Ustrd>{purpose}</Ustrd></RmtInf></TxDtls></NtryDtls></Ntry>"
            )
        };
        let xml = format!(
            "<Document><BkToCstmrAcctRpt><Rpt>{}{}{}</Rpt></BkToCstmrAcctRpt></Document>",
            entry("1.00", "a", " book "),
            entry("2.00", "b", "BOOK"),
            entry("3.00", "c", "BOOK")
        );
        let (txs, _) = parse(&xml, "acc");
        assert_eq!(txs.len(), 3, "Sts is trimmed and case-insensitive");
        assert_eq!(txs[0].external_id, "camt:REF1");
        assert!(!txs[1].external_id.starts_with("camt:"));
        assert!(!txs[2].external_id.starts_with("camt:"));
        let ids: std::collections::HashSet<_> = txs.iter().map(|t| &t.external_id).collect();
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn hold_without_date_takes_the_card_timestamp_and_keeps_its_id() {
        assert_eq!(
            card_timestamp_date("Lidl sagt Danke Koeln DEU 20 26-11-14T18:56:47").as_deref(),
            Some("2026-11-14")
        );
        assert_eq!(
            card_timestamp_date("REWE 2026-11-12T15:04:37").as_deref(),
            Some("2026-11-12")
        );
        assert_eq!(card_timestamp_date("Miete November"), None);
        assert_eq!(card_timestamp_date("2026-13-40T10:00:00"), None);
        let xml = r#"<Document><BkToCstmrAcctRpt><Rpt>
          <Ntry><Amt Ccy="EUR">54.50</Amt><CdtDbtInd>DBIT</CdtDbtInd>
            <NtryDtls><TxDtls><RltdPties><Cdtr><Nm>Lidl</Nm></Cdtr></RltdPties>
            <RmtInf><Ustrd>Lidl sagt Danke Koeln DEU 20</Ustrd><Ustrd>26-11-14T18:56:47</Ustrd></RmtInf></TxDtls></NtryDtls></Ntry>
          <Ntry><Amt Ccy="EUR">9.00</Amt><CdtDbtInd>DBIT</CdtDbtInd>
            <NtryDtls><TxDtls><RmtInf><Ustrd>ohne Zeitstempel</Ustrd></RmtInf></TxDtls></NtryDtls></Ntry>
        </Rpt></BkToCstmrAcctRpt></Document>"#;
        let p = parse_notbooked(xml, "acc");
        assert_eq!(p[0].booking_date, "2026-11-14");
        assert_eq!(p[1].booking_date, crate::connectors::today());
        // the id leaves the missing date out: the same hold tomorrow keeps it
        let mut again = p[1].clone();
        again.booking_date = String::new();
        again.external_id = String::new();
        assert_eq!(
            p[1].external_id,
            format!("{HOLD_PREFIX}{}", again.content_id(0))
        );
    }

    #[test]
    fn notbooked_document_is_all_pending() {
        let xml = r#"<Document><BkToCstmrAcctRpt><Rpt>
          <Ntry><Amt Ccy="EUR">12.50</Amt><CdtDbtInd>DBIT</CdtDbtInd><Sts><Cd>BOOK</Cd></Sts>
            <ValDt><Dt>2026-11-14</Dt></ValDt><AcctSvcrRef>X1</AcctSvcrRef>
            <NtryDtls><TxDtls><RltdPties><Cdtr><Nm>Bäckerei Schmitz</Nm></Cdtr></RltdPties>
            <RmtInf><Ustrd>Kartenzahlung</Ustrd></RmtInf></TxDtls></NtryDtls></Ntry>
          <Ntry><Amt Ccy="EUR">12.50</Amt><CdtDbtInd>DBIT</CdtDbtInd>
            <ValDt><Dt>2026-11-14</Dt></ValDt>
            <NtryDtls><TxDtls><RltdPties><Cdtr><Nm>Bäckerei Schmitz</Nm></Cdtr></RltdPties>
            <RmtInf><Ustrd>Kartenzahlung</Ustrd></RmtInf></TxDtls></NtryDtls></Ntry>
        </Rpt></BkToCstmrAcctRpt></Document>"#;
        let p = parse_notbooked(xml, "acc");
        assert_eq!(p.len(), 2);
        assert!(p.iter().all(|t| t.external_id.starts_with(HOLD_PREFIX)));
        assert_ne!(
            p[0].external_id, p[1].external_id,
            "two equal holds stay two"
        );
        assert_eq!(
            p[0].booking_date, "2026-11-14",
            "value date when not booked"
        );
        assert_eq!(p[0].counterparty, "Bäckerei Schmitz");
    }

    #[test]
    fn parses_camt_052() {
        let (txs, rep) = parse(SAMPLE_08, "acc");
        assert_eq!(txs.len(), 2, "pending entries are not booked ones");
        assert_eq!(rep.pending.len(), 1, "PDNG entry is reported as pending");
        let p = &rep.pending[0];
        assert!(p.external_id.starts_with(HOLD_PREFIX), "{}", p.external_id);
        assert!(p.external_id.starts_with("pending-"), "Booking.is_pending");
        assert_eq!(
            (p.amount_cents, p.booking_date.as_str()),
            (-9_99, "2026-11-09")
        );
        assert_eq!(p.booking_text, "Vorgemerkt");
        // stable across fetches, so persist can keep or drop it
        assert_eq!(
            parse(SAMPLE_08, "acc").1.pending[0].external_id,
            p.external_id
        );
        assert_eq!(txs[0].amount_cents, -31_96);
        assert_eq!(txs[0].counterparty, "Lidl Dienstleistung & Co. KG");
        assert_eq!(txs[0].counterparty_iban, "DE12300700100123456700");
        assert_eq!(txs[0].purpose, "Lidl sagt Danke Kassel");
        assert_eq!(txs[0].booking_text, "Kartenzahlung");
        assert_eq!(txs[0].external_id, "camt:2026110712345");
        assert_eq!(txs[1].amount_cents, 4_180_00);
        assert_eq!(txs[1].booking_date, "2026-10-27");
        assert_eq!(txs[1].counterparty, "Nordhessen Technik GmbH");
        assert_eq!(
            txs[1].purpose,
            "EREF+NT-2026-10 MREF+M-1 SVWZ+Gehalt Oktober"
        );
        assert_eq!(rep.closing_balance_cents, Some(1_234_56));
        // bank codes for salary detection (#37)
        assert_eq!(txs[0].gvc, "106");
        assert_eq!(txs[0].purpose_code, "");
        assert_eq!(txs[1].purpose_code, "SALA");
    }

    #[test]
    fn sala_sub_family_without_purpose() {
        let xml = SAMPLE_08
            .replace("<Purp><Cd>SALA</Cd></Purp>", "")
            .replace(
                "<Sts>BOOK</Sts>",
                "<Sts>BOOK</Sts><BkTxCd><Domn><Cd>PMNT</Cd><Fmly><Cd>RCDT</Cd><SubFmlyCd>SALA</SubFmlyCd></Fmly></Domn><Prtry><Cd>NTRF+153+000</Cd></Prtry></BkTxCd>",
            );
        let (txs, _) = parse(&xml, "acc");
        assert_eq!(txs[1].purpose_code, "SALA");
        assert_eq!(txs[1].gvc, "153");
    }
}

#[cfg(test)]
mod tokenizer_robustness_tests {
    #[test]
    fn absurd_nesting_and_size_are_refused_quickly() {
        let deep = format!("{}x{}", "<a>".repeat(100_000), "</a>".repeat(100_000));
        let (txs, report) = super::parse(&deep, "acc");
        assert!(txs.is_empty() && report.pending.is_empty());
        let big = "<a>".to_string() + &" ".repeat(super::MAX_CAMT_BYTES) + "</a>";
        assert!(super::parse(&big, "acc").0.is_empty());
    }

    #[test]
    fn broken_attributes_do_not_panic() {
        for xml in [
            "<Amt Ccy=>1</Amt>",
            "<Amt Ccy=",
            "<Amt Ccy=ä1ä>2</Amt>",
            "<Amt Ccy=\"EUR>3",
            "<Amt a=€x€ b='y'>4</Amt>",
        ] {
            let _ = super::tokenize(xml);
        }
        let toks = super::tokenize("<Amt Ccy=ä1ä>2</Amt>");
        assert!(matches!(&toks[0], super::Tok::Start(n, a) if n == "Amt" && a[0].1 == "1"));
    }
}
