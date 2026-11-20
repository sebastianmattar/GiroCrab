//! Transfer templates ("Überweisungsvorlagen") from the MoneyMoney database
//! (#579).
//!
//! MoneyMoney keeps templates and sent orders in one table, `payments`; a
//! template is a row of `type` 12. Recipient name, purpose and amount are
//! plain columns, the recipient's IBAN and BIC only live in the `data` blob.
//! That blob is a protobuf-style record: field 4 recipient name, 5 IBAN,
//! 6 BIC, 7 amount (zigzag cents), 8 purpose, 3 a nested record with the
//! sender account. Besides the standard wire types it uses wire type 7 for a
//! fixed 16-byte value (an id). Only the top-level string fields are taken;
//! the reader stops at anything it does not know.
//!
//! Only template rows are read (`WHERE type = 12`): the blobs of sent orders
//! (complete SEPA messages) are never selected.

use std::collections::HashSet;

use rusqlite::{Connection, params};

use super::{get, real_to_cents, select, vint, vreal, vtext};
use crate::connectors::fints::sepa;
use crate::error::CoreResult;
use crate::iban::{normalize_iban, validate_iban};

/// `payments.type` of a transfer template in MoneyMoney.
const MM_TRANSFER_TEMPLATE: i64 = 12;

/// One MoneyMoney transfer template as read from the database.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct SrcTemplate {
    pub recipient_name: String,
    pub iban: String,
    pub bic: String,
    /// Positive cents; `None` = no amount stored.
    pub amount_cents: Option<i64>,
    pub purpose: String,
    pub currency: String,
}

impl SrcTemplate {
    /// GiroCrab cannot take it over: no recipient, invalid IBAN or not euro.
    fn unusable(&self) -> bool {
        self.recipient_name.trim().is_empty()
            || !validate_iban(&self.iban)
            || !(self.currency.is_empty() || self.currency == "EUR")
    }
}

/// Top-level text fields of a `payments.data` blob.
#[derive(Debug, Default, PartialEq)]
pub(super) struct PaymentFields {
    pub name: String,
    pub iban: String,
    pub bic: String,
    pub purpose: String,
}

fn varint(b: &[u8], i: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for shift in 0..10 {
        let c = *b.get(*i)?;
        *i += 1;
        v |= u64::from(c & 0x7f) << (7 * shift);
        if c & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

/// Reads the recipient fields of a `payments.data` blob. Malformed or
/// unknown data ends the walk; what was read up to there is kept.
pub(super) fn parse_payment_blob(b: &[u8]) -> PaymentFields {
    let mut out = PaymentFields::default();
    let mut i = 0;
    while i < b.len() {
        let Some(key) = varint(b, &mut i) else { break };
        let (field, wire) = (key >> 3, key & 7);
        let skip = match wire {
            0 => {
                if varint(b, &mut i).is_none() {
                    break;
                }
                0
            }
            1 => 8,
            5 => 4,
            // MoneyMoney's own: fixed 16 bytes
            7 => 16,
            2 => {
                let Some(len) = varint(b, &mut i).and_then(|l| usize::try_from(l).ok()) else {
                    break;
                };
                let Some(bytes) = i.checked_add(len).and_then(|end| b.get(i..end)) else {
                    break;
                };
                if let Ok(s) = std::str::from_utf8(bytes) {
                    let slot = match field {
                        4 => Some(&mut out.name),
                        5 => Some(&mut out.iban),
                        6 => Some(&mut out.bic),
                        8 => Some(&mut out.purpose),
                        _ => None,
                    };
                    if let Some(slot) = slot {
                        *slot = s.trim().to_string();
                    }
                }
                len
            }
            _ => break,
        };
        match i.checked_add(skip) {
            Some(end) if end <= b.len() => i = end,
            _ => break,
        }
    }
    out
}

/// The templates of a MoneyMoney database; empty for databases without
/// `payments` (older versions). Deleted rows (`tombstone`) are left out.
pub(super) fn read_templates(
    conn: &Connection,
    have: &HashSet<String>,
) -> CoreResult<Vec<SrcTemplate>> {
    if !have.contains("payments") {
        return Ok(Vec::new());
    }
    let cols: HashSet<String> = conn
        .prepare("SELECT lower(name) FROM pragma_table_info('payments')")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;
    if !cols.contains("type") || !cols.contains("data") {
        return Ok(Vec::new());
    }
    let sql = select(
        conn,
        "payments",
        &["name", "purpose", "amount", "currency", "tombstone", "data"],
        &format!("WHERE \"type\" = {MM_TRANSFER_TEMPLATE} ORDER BY rowid"),
    )?;
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map([], |r| {
        let blob = match get(r, 5)? {
            rusqlite::types::Value::Blob(b) => parse_payment_blob(&b),
            _ => PaymentFields::default(),
        };
        let name = vtext(get(r, 0)?);
        let purpose = vtext(get(r, 1)?);
        let bic = blob.bic.to_uppercase();
        Ok((
            vint(get(r, 4)?).unwrap_or(0) != 0,
            SrcTemplate {
                recipient_name: if name.is_empty() { blob.name } else { name },
                iban: normalize_iban(&blob.iban),
                bic: if sepa::validate_bic(&bic) {
                    bic
                } else {
                    String::new()
                },
                amount_cents: vreal(get(r, 2)?)
                    .and_then(|a| real_to_cents(a.abs()))
                    .filter(|c| *c > 0),
                purpose: if purpose.is_empty() {
                    blob.purpose
                } else {
                    purpose
                },
                currency: vtext(get(r, 3)?).to_uppercase(),
            },
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (deleted, t) = row?;
        if !deleted {
            out.push(t);
        }
    }
    Ok(out)
}

/// What [`import_templates`] did: (new, already there, not usable).
pub(super) fn import_templates(
    conn: &Connection,
    templates: &[SrcTemplate],
) -> CoreResult<(u32, u32, u32)> {
    let (mut created, mut existing, mut skipped) = (0, 0, 0);
    for t in templates {
        if t.unusable() {
            skipped += 1;
            continue;
        }
        let name = t.recipient_name.trim();
        let there: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM transfer_templates
                 WHERE recipient_iban = ?1 AND recipient_name = ?2 AND default_purpose = ?3
                   AND default_amount_cents IS ?4)",
            params![t.iban, name, t.purpose, t.amount_cents],
            |r| r.get(0),
        )?;
        if there {
            existing += 1;
            continue;
        }
        conn.execute(
            "INSERT INTO transfer_templates(name, recipient_name, recipient_iban, recipient_bic,
                 default_amount_cents, default_purpose)
             VALUES (?1, ?1, ?2, ?3, ?4, ?5)",
            params![name, t.iban, t.bic, t.amount_cents, t.purpose],
        )?;
        created += 1;
    }
    Ok((created, existing, skipped))
}

/// Number of templates the import would take over.
pub(super) fn usable_count(templates: &[SrcTemplate]) -> u32 {
    templates.iter().filter(|t| !t.unusable()).count() as u32
}

/// Encodes a template blob like MoneyMoney does (tests only).
#[cfg(test)]
pub(super) fn encode_template_blob(
    name: &str,
    iban: &str,
    bic: &str,
    cents: i64,
    purpose: &str,
) -> Vec<u8> {
    fn varint(out: &mut Vec<u8>, mut v: u64) {
        while v >= 0x80 {
            out.push((v as u8) | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }
    fn text(out: &mut Vec<u8>, field: u64, s: &str) {
        varint(out, field << 3 | 2);
        varint(out, s.len() as u64);
        out.extend_from_slice(s.as_bytes());
    }
    let mut b = Vec::new();
    varint(&mut b, 1 << 3);
    varint(&mut b, 1);
    // wire type 7: 16-byte id
    varint(&mut b, 16 << 3 | 7);
    b.extend_from_slice(&[0xab; 16]);
    // nested sender account { 5: IBAN }
    let mut sender = Vec::new();
    text(&mut sender, 5, "DE02120300000000202051");
    varint(&mut b, 3 << 3 | 2);
    varint(&mut b, sender.len() as u64);
    b.extend_from_slice(&sender);
    text(&mut b, 4, name);
    text(&mut b, 5, iban);
    if !bic.is_empty() {
        text(&mut b, 6, bic);
    }
    varint(&mut b, 7 << 3);
    varint(&mut b, ((cents << 1) ^ (cents >> 63)) as u64);
    text(&mut b, 8, purpose);
    // a timestamp as double
    varint(&mut b, 9 << 3 | 1);
    b.extend_from_slice(&1_772_712_000f64.to_le_bytes());
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    const IBAN: &str = "DE89370400440532013000";

    #[test]
    fn blob_fields_including_moneymoney_wire_type_7() {
        let b = encode_template_blob("Erika Mustermann", IBAN, "COBADEFFXXX", 72_000, "Miete Mai");
        assert_eq!(
            parse_payment_blob(&b),
            PaymentFields {
                name: "Erika Mustermann".into(),
                iban: IBAN.into(),
                bic: "COBADEFFXXX".into(),
                purpose: "Miete Mai".into(),
            }
        );
    }

    #[test]
    fn nested_sender_iban_is_not_taken_for_the_recipient() {
        let b = encode_template_blob("Erika Mustermann", "", "", 0, "");
        assert_eq!(parse_payment_blob(&b).iban, "");
    }

    #[test]
    fn broken_blobs_keep_what_was_read_and_never_panic() {
        let b = encode_template_blob("Erika Mustermann", IBAN, "", 100, "Zweck");
        for cut in 0..b.len() {
            let _ = parse_payment_blob(&b[..cut]);
        }
        // the IBAN ends before the cut: it is kept
        let iban_end = b
            .windows(IBAN.len())
            .position(|w| w == IBAN.as_bytes())
            .unwrap()
            + IBAN.len();
        assert_eq!(parse_payment_blob(&b[..iban_end]).iban, IBAN);
        // unknown wire type 6 stops the walk
        let mut odd = vec![0x08, 0x01, (40 << 3 | 6) as u8, 0x01];
        odd.extend_from_slice(&b);
        assert_eq!(parse_payment_blob(&odd), PaymentFields::default());
        // overlong length, endless varint, garbage
        assert_eq!(
            parse_payment_blob(&[0x2a, 0xff, 0xff, 0xff, 0x0f, b'D']),
            PaymentFields::default()
        );
        assert_eq!(parse_payment_blob(&[0x80; 32]), PaymentFields::default());
        assert_eq!(
            parse_payment_blob(&[0xff, 0x00, 0x13]),
            PaymentFields::default()
        );
    }

    #[test]
    fn unusable_templates() {
        let ok = SrcTemplate {
            recipient_name: "Erika Mustermann".into(),
            iban: IBAN.into(),
            currency: "EUR".into(),
            ..Default::default()
        };
        assert!(!ok.unusable());
        assert!(
            !SrcTemplate {
                currency: String::new(),
                ..ok.clone()
            }
            .unusable()
        );
        assert!(
            SrcTemplate {
                currency: "USD".into(),
                ..ok.clone()
            }
            .unusable()
        );
        assert!(
            SrcTemplate {
                recipient_name: " ".into(),
                ..ok.clone()
            }
            .unusable()
        );
        assert!(
            SrcTemplate {
                iban: "DE89370400440532013001".into(),
                ..ok.clone()
            }
            .unusable()
        );
        assert!(
            SrcTemplate {
                iban: String::new(),
                ..ok
            }
            .unusable()
        );
    }
}
