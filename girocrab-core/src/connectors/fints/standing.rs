//! Standing orders (Dauerauftrag Bestand): fetches them with HKCDB and reads
//! HICDB (v1/v2) and the pain.001 it carries. Field layout after the public DK
//! specification (as mirrored in hbci4java): header, KTI, SEPA descriptor,
//! pain.001 (binary), order id, schedule (first date, M/W, interval, execution
//! day, [v2: time,] last date), suspension, "may change", "may skip", "may
//! delete".

use super::syntax::{Segment, SegmentBuilder};
use super::{FinTsConnector, SepaAccount, sepa};
use crate::connectors::{ConnectContext, RemoteStandingOrder};
use crate::error::{CoreError, CoreResult};
use crate::i18n::tr;

/// One HICDB segment; `None` when it lacks account, schedule or payment.
fn parse_hicdb(seg: &Segment, account_ref: &str) -> Option<RemoteStandingOrder> {
    let first = super::fints_date(seg.get(5, 0))?;
    let time_unit = seg.get(5, 1).to_string();
    if time_unit != "M" && time_unit != "W" {
        return None;
    }
    let interval: u32 = seg.get(5, 2).parse().ok().filter(|n| *n > 0)?;
    let exec_day: u32 = seg.get(5, 3).parse().ok()?;
    // DauerDetails: v1 ends with the last date (component 4); v2 puts the time of
    // day (HHMMSS) in between, so the last date is component 5. An entered but
    // unreadable last date would make the order look endless: skip it then.
    let last_cell = seg.get(5, if seg.version() >= 2 { 5 } else { 4 });
    let last = if last_cell.is_empty() {
        None
    } else {
        Some(super::fints_date(last_cell)?)
    };
    let xml = crate::text::decode_xml_bytes(seg.binary(3)?);
    let pay = pain_payment(&xml)?;
    let flag = |e: usize| seg.get(e, 0) == "J";
    Some(RemoteStandingOrder {
        account_ref: account_ref.to_string(),
        order_id: seg.get(4, 0).to_string(),
        recipient_name: pay.name,
        recipient_iban: pay.iban,
        amount_cents: pay.cents,
        purpose: pay.purpose,
        first_date: first,
        last_date: last,
        time_unit,
        interval,
        exec_day,
        can_change: flag(7),
        can_delete: flag(9),
    })
}

#[derive(Debug, PartialEq, Eq)]
struct PainPayment {
    pub name: String,
    pub iban: String,
    pub cents: i64,
    pub purpose: String,
}

/// First payment of a pain.001: creditor name, IBAN, amount, purpose. A
/// small tag scanner (namespace prefixes ignored): the file is only read.
fn pain_payment(xml: &str) -> Option<PainPayment> {
    let tx = tag_start(xml, "CdtTrfTxInf", 0)?;
    let amount = tag_text(xml, "InstdAmt", tx)?;
    let cents = crate::money::parse_cents(&amount.replace('.', ",")).ok()?;
    let cdtr = tag_start(xml, "Cdtr", tx).unwrap_or(tx);
    let name = tag_text(xml, "Nm", cdtr).unwrap_or_default();
    let acct = tag_start(xml, "CdtrAcct", tx).unwrap_or(tx);
    let iban = tag_text(xml, "IBAN", acct)?;
    let purpose = tag_text(xml, "Ustrd", tx).unwrap_or_default();
    Some(PainPayment {
        name,
        iban: iban.replace(' ', ""),
        cents,
        purpose,
    })
}

/// Offset just after the first `<Tag …>` at or after `from` (any prefix).
pub(super) fn tag_start(xml: &str, tag: &str, from: usize) -> Option<usize> {
    let mut pos = from;
    while let Some(i) = xml[pos..].find('<') {
        let open = pos + i + 1;
        let end = xml[open..].find('>')? + open;
        let inner = xml[open..end].trim_end_matches('/');
        let name = inner.split_whitespace().next().unwrap_or("");
        let local = name.rsplit(':').next().unwrap_or(name);
        if local == tag && !name.starts_with('/') {
            return Some(end + 1);
        }
        pos = end + 1;
    }
    None
}

pub(super) fn tag_text(xml: &str, tag: &str, from: usize) -> Option<String> {
    let start = tag_start(xml, tag, from)?;
    let end = xml[start..].find('<')? + start;
    Some(unescape(xml[start..end].trim()))
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

impl FinTsConnector {
    /// HKCDB: the standing orders of an account (all pages). Orders the client
    /// cannot read (no payment in the pain.001) are left out.
    pub(super) async fn standing_orders(
        &mut self,
        ctx: &ConnectContext,
        acc: &SepaAccount,
        label: &str,
    ) -> CoreResult<Vec<RemoteStandingOrder>> {
        let v = self.version("HKCDB");
        let descriptor = self
            .pain_version("HKCDB")
            .unwrap_or(sepa::PainVersion::V03)
            .descriptor();
        let mut out = Vec::new();
        let mut touchdown: Option<String> = None;
        for _ in 0..20 {
            let mut seg = SegmentBuilder::new("HKCDB", 3, v)
                .raw(&Self::kti(acc))
                .de(&[descriptor]);
            if let Some(t) = &touchdown {
                // order id and max. entries empty, then the touchdown
                seg = seg.raw("").raw("").de(&[t]);
            }
            let r = self.request(ctx, "HKCDB", seg.build()).await?;
            for s in r.all("HICDB") {
                if let Some(o) = parse_hicdb(s, label) {
                    out.push(o);
                }
            }
            touchdown = r.code("3040").and_then(|c| c.3.first().cloned());
            if touchdown.is_none() {
                return Ok(out);
            }
        }
        // 20 pages and the bank still has more: not complete, so the caller keeps the stored list
        Err(CoreError::Bank {
            code: "HKCDB".into(),
            message: tr("Die Bank lieferte die Daueraufträge nicht vollständig (zu viele Seiten).")
                .into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::fints::syntax::parse_response;

    const PAIN: &str = r#"<?xml version="1.0" encoding="UTF-8"?><Document xmlns="urn:iso:std:iso:20022:tech:xsd:pain.001.001.03"><CstmrCdtTrfInitn><GrpHdr><MsgId>X</MsgId></GrpHdr><PmtInf><Dbtr><Nm>Ich</Nm></Dbtr><DbtrAcct><Id><IBAN>DE02120300000000202051</IBAN></Id></DbtrAcct><CdtTrfTxInf><Amt><InstdAmt Ccy="EUR">850.00</InstdAmt></Amt><Cdtr><Nm>Hausverwaltung &amp; Co</Nm></Cdtr><CdtrAcct><Id><IBAN>DE89370400440532013000</IBAN></Id></CdtrAcct><RmtInf><Ustrd>Miete Dezember</Ustrd></RmtInf></CdtTrfTxInf></PmtInf></CstmrCdtTrfInitn></Document>"#;

    #[test]
    fn reads_the_payment_of_a_pain001() {
        let p = pain_payment(PAIN).unwrap();
        assert_eq!(p.name, "Hausverwaltung & Co");
        assert_eq!(p.iban, "DE89370400440532013000");
        assert_eq!(p.cents, 85_000);
        assert_eq!(p.purpose, "Miete Dezember");
        // The debtor's data is not mistaken for the creditor's.
        assert!(pain_payment("<Document/>").is_none());
    }

    fn hicdb_v(version: u32, details: &str) -> Segment {
        let mut raw = format!(
            "HICDB:5:{version}:3+DE02120300000000202051:BYLADEM1001:::280:12030000+urn?:iso?:std?:iso?:20022?:tech?:xsd?:pain.001.001.03+@{}@",
            PAIN.len()
        )
        .into_bytes();
        raw.extend_from_slice(PAIN.as_bytes());
        raw.extend_from_slice(format!("+AB12CD+{details}++J+N+J'").as_bytes());
        parse_response(&raw).unwrap().remove(0)
    }

    fn hicdb(details: &str) -> Segment {
        hicdb_v(1, details)
    }

    #[test]
    fn reads_a_monthly_order_v1() {
        let o = parse_hicdb(&hicdb("20260101:M:1:1:20261201"), "acc").unwrap();
        assert_eq!(o.order_id, "AB12CD");
        assert_eq!(
            (o.first_date.as_str(), o.last_date.as_deref()),
            ("2026-01-01", Some("2026-12-01"))
        );
        assert_eq!((o.time_unit.as_str(), o.interval, o.exec_day), ("M", 1, 1));
        assert_eq!(
            (o.amount_cents, o.recipient_iban.as_str()),
            (85_000, "DE89370400440532013000")
        );
        assert!(o.can_change && o.can_delete);
    }

    #[test]
    fn open_ended_and_v2_with_time() {
        let o = parse_hicdb(&hicdb("20260105:W:2:3"), "acc").unwrap();
        assert_eq!(
            (o.time_unit.as_str(), o.interval, o.exec_day, o.last_date),
            ("W", 2, 3, None)
        );
        let o = parse_hicdb(&hicdb_v(2, "20260105:M:3:31:120000:20270101"), "acc").unwrap();
        assert_eq!(o.last_date.as_deref(), Some("2027-01-01"));
        // v2 without a last date but with a time: open-ended, the time is no end date.
        let o = parse_hicdb(&hicdb_v(2, "20260105:M:3:31:120000"), "acc").unwrap();
        assert_eq!(o.last_date, None);
    }

    #[test]
    fn unreadable_last_date_is_skipped_not_endless() {
        assert!(parse_hicdb(&hicdb("20260101:M:1:1:2026-12"), "acc").is_none());
    }

    #[test]
    fn broken_schedule_is_skipped() {
        assert!(parse_hicdb(&hicdb("20260105:X:1:1"), "acc").is_none());
        assert!(parse_hicdb(&hicdb("20260105:M:0:1"), "acc").is_none());
    }
}
