//! Parsers for bunq API objects: accounts, payments, card holds, receipts
//! and statements.

use serde_json::Value;

use super::HOLD_PREFIX;
use crate::connectors::{RemoteAccount, RemoteTransaction};
use crate::i18n::{tr, trf};
use crate::models::AccountKind;
use crate::money::json_number_to_cents;

/// Cents -> bunq decimal string ("-12.5" is never sent; always "12.50").
pub(super) fn cents_to_decimal(cents: i64) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    let c = cents.unsigned_abs();
    format!("{sign}{}.{:02}", c / 100, c % 100)
}

/// Finds the object wrapped under `key` in a bunq `Response` array.
pub(super) fn find<'a>(v: &'a Value, key: &str) -> &'a Value {
    v["Response"]
        .as_array()
        .and_then(|a| a.iter().find(|o| o.get(key).is_some()))
        .map(|o| &o[key])
        .unwrap_or(&Value::Null)
}

/// `{"MonetaryAccountBank": {...}}` -> (bunq id, account).
pub(super) fn parse_account(item: &Value, today: &str) -> Option<(i64, RemoteAccount)> {
    let (wrapper, a) = item.as_object()?.iter().next()?;
    let id = a["id"].as_i64()?;
    if matches!(
        a["status"].as_str(),
        Some("CANCELLED" | "CANCELLED_PENDING_SETTLEMENT")
    ) {
        return None;
    }
    let iban_alias = a["alias"]
        .as_array()
        .and_then(|al| al.iter().find(|x| x["type"] == "IBAN"));
    let kind = if matches!(
        wrapper.as_str(),
        "MonetaryAccountSavings" | "MonetaryAccountExternalSavings"
    ) {
        AccountKind::Savings
    } else {
        AccountKind::Checking
    };
    Some((
        id,
        RemoteAccount {
            external_ref: format!("bunq:{id}"),
            name: a["description"].as_str().unwrap_or("bunq").to_string(),
            owner: iban_alias
                .and_then(|x| x["name"].as_str())
                .or(a["display_name"].as_str())
                .unwrap_or_default()
                .to_string(),
            iban: iban_alias
                .and_then(|x| x["value"].as_str())
                .unwrap_or_default()
                .to_string(),
            account_number: String::new(),
            currency: a["currency"].as_str().unwrap_or("EUR").to_string(),
            kind,
            balance_cents: json_number_to_cents(&a["balance"]["value"]).unwrap_or(0),
            balance_date: today.to_string(),
        },
    ))
}

/// Signed amount in the account currency: card purchases are debits.
pub(super) fn card_amount_cents(a: &Value) -> Option<i64> {
    let cents = json_number_to_cents(&a["amount_billing"]["value"]).ok()?;
    let kind = a["authorisation_type"]
        .as_str()
        .unwrap_or_default()
        .to_uppercase();
    let credit = ["REFUND", "CREDIT", "RETURN"]
        .iter()
        .any(|k| kind.contains(k));
    Some(if credit { cents.abs() } else { -cents.abs() })
}

/// Allowed authorisation that has not been cleared (booked) or expired yet.
pub(super) fn is_open_hold(a: &Value, now: jiff::civil::DateTime) -> bool {
    if a["decision"].as_str() != Some("ALLOWED") {
        return false;
    }
    if !matches!(a["clearing_status"].as_str(), Some("PENDING") | None) {
        return false;
    }
    if a["payment_status"]
        .as_str()
        .is_some_and(|s| s.contains("FAIL"))
    {
        return false;
    }
    let expired = |k: &str| {
        a[k].as_str()
            .and_then(|t| jiff::civil::DateTime::strptime("%Y-%m-%d %H:%M:%S%.f", t).ok())
            .is_some_and(|t| t < now)
    };
    !(expired("reservation_expiry_time") || expired("clearing_expiry_time"))
}

pub(super) fn parse_hold(a: &Value, account_ref: &str) -> Option<RemoteTransaction> {
    let id = a["id"].as_i64()?;
    let date = local_date(a["created"].as_str()?)?;
    let amount = card_amount_cents(a)?;
    if amount == 0 {
        return None;
    }
    let counterparty = a["counterparty_alias"]["display_name"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or(a["description"].as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    let mut purpose = a["description"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string();
    if let Some(note) =
        foreign_amount_note(a, a["amount_billing"]["currency"].as_str().unwrap_or("EUR"))
    {
        purpose = if purpose.is_empty() {
            note
        } else {
            format!("{purpose} · {note}")
        };
    }
    Some(RemoteTransaction {
        account_ref: account_ref.to_string(),
        external_id: format!("{HOLD_PREFIX}{id}"),
        booking_date: date.clone(),
        value_date: date,
        amount_cents: amount,
        currency: a["amount_billing"]["currency"]
            .as_str()
            .unwrap_or("EUR")
            .to_string(),
        counterparty,
        counterparty_iban: String::new(),
        purpose,
        booking_text: format!("{} ({})", tr("Kartenzahlung"), tr("vorgemerkt")),
        ..Default::default()
    })
}

/// "Originalbetrag 12,00 USD" when the merchant charged a foreign currency.
pub(super) fn foreign_amount_note(a: &Value, account_currency: &str) -> Option<String> {
    let local = &a["amount_local"];
    let ccy = local["currency"].as_str()?;
    if ccy.is_empty() || ccy.eq_ignore_ascii_case(account_currency) {
        return None;
    }
    let cents = json_number_to_cents(&local["value"]).ok()?.abs();
    Some(trf(
        "Originalbetrag {0}",
        &[&crate::money::format_cents(cents, ccy.to_string())],
    ))
}

/// bunq returns attachments as an array or as a single object.
pub(super) fn attachment_items(v: &Value) -> Vec<&Value> {
    match v {
        Value::Array(items) => items.iter().collect(),
        Value::Object(_) => vec![v],
        _ => Vec::new(),
    }
}

/// Outgoing payment created by accepting a request (direct debit, bunq
/// request): its notes live on the RequestResponse, not on the payment.
pub(super) fn is_request_payment(p: &Value) -> bool {
    p["sub_type"].as_str() == Some("REQUEST")
        && json_number_to_cents(&p["amount"]["value"]).is_ok_and(|c| c < 0)
}

/// The accepted RequestResponse a request payment belongs to. bunq gives no
/// reference, so match the amount and the answer time (the payment is
/// created in the same instant; observed: 0.1 s apart).
pub(super) fn match_request_response(p: &Value, responses: &[Value]) -> Option<i64> {
    let ts = |v: &Value| jiff::civil::DateTime::strptime("%Y-%m-%d %H:%M:%S%.f", v.as_str()?).ok();
    let paid = ts(&p["created"])?;
    let cents = json_number_to_cents(&p["amount"]["value"]).ok()?.abs();
    responses
        .iter()
        .filter(|r| r["status"].as_str() == Some("ACCEPTED"))
        .filter(|r| {
            json_number_to_cents(&r["amount_responded"]["value"])
                .or_else(|_| json_number_to_cents(&r["amount_inquired"]["value"]))
                .is_ok_and(|c| c.abs() == cents)
        })
        .filter_map(|r| {
            let gap = ts(&r["time_responded"])?
                .duration_since(paid)
                .as_millis()
                .abs();
            (gap <= 120_000).then_some((gap, r["id"].as_i64()?))
        })
        .min()
        .map(|(_, id)| id)
}

/// The DraftPayment whose accepted result is payment `pid`. Observed: the
/// draft contains its payment's id (exact field undocumented), so look for
/// that value anywhere inside it. Payment ids are long and unique, so a
/// whole-value match does not collide with other ids.
pub(super) fn draft_of_payment(pid: i64, drafts: &[Value]) -> Option<i64> {
    fn has_id(v: &Value, pid: i64) -> bool {
        match v {
            Value::Object(o) => o.values().any(|c| has_id(c, pid)),
            Value::Array(a) => a.iter().any(|c| has_id(c, pid)),
            Value::Number(n) => n.as_i64() == Some(pid),
            Value::String(s) => s.parse::<i64>().ok() == Some(pid),
            _ => false,
        }
    }
    drafts
        .iter()
        .filter(|d| d["id"].as_i64() != Some(pid))
        .find(|d| {
            d.as_object()
                .is_some_and(|o| o.iter().any(|(k, c)| k != "id" && has_id(c, pid)))
        })
        .and_then(|d| d["id"].as_i64())
}

/// Picks up to `budget` already-checked older payment ids, newest first,
/// continuing below `cursor` and wrapping around. Returns the picked ids and
/// the cursor for the next sync.
pub(super) fn rotate_older(
    mut ids: Vec<i64>,
    cursor: Option<i64>,
    budget: usize,
) -> (Vec<i64>, Option<i64>) {
    if ids.is_empty() {
        return (Vec::new(), None);
    }
    if budget == 0 {
        return (Vec::new(), cursor);
    }
    ids.sort_unstable_by(|a, b| b.cmp(a));
    ids.dedup();
    let start = cursor
        .and_then(|c| ids.iter().position(|&id| id < c))
        .unwrap_or(0);
    let picked: Vec<i64> = ids
        .iter()
        .cycle()
        .skip(start)
        .take(budget.min(ids.len()))
        .copied()
        .collect();
    let next = picked.last().copied();
    (picked, next)
}

/// Short label for a payment's receipt: counterparty or description.
pub(super) fn receipt_label(p: &Value) -> String {
    let cp = &p["counterparty_alias"];
    cp["display_name"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .or(cp["label_user"]["display_name"].as_str())
        .or(p["description"].as_str())
        .unwrap_or("bunq")
        .trim()
        .to_string()
}

pub(super) fn extension_for(mime: &str) -> &'static str {
    match mime {
        "application/pdf" => "pdf",
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/heic" => "heic",
        _ => "bin",
    }
}

/// File-name safe: letters, digits, '-' and '_' only.
pub(super) fn slug(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    out.trim_matches('_').chars().take(40).collect()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn receipt_document(
    account_ref: &str,
    booking_ext: &str,
    date: &str,
    label: &str,
    description: &str,
    attachment_id: i64,
    bytes: Vec<u8>,
    content_type: &str,
    source_url: String,
) -> crate::documents::RemoteDocument {
    let ct = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    let mime = if ct.is_empty() || ct == "application/octet-stream" || ct == "application/json" {
        crate::documents::sniff_mime(&bytes, "")
    } else {
        ct
    };
    let title = if description.trim().is_empty() {
        trf("Beleg {0}", &[&label])
    } else {
        description.trim().to_string()
    };
    crate::documents::RemoteDocument {
        account_ref: account_ref.to_string(),
        booking_external_id: Some(booking_ext.to_string()),
        title,
        doc_type: "beleg".into(),
        file_name: format!(
            "{date}_{}_{attachment_id}.{}",
            slug(label),
            extension_for(&mime)
        ),
        mime_type: mime,
        bytes,
        source_url,
        date: date.to_string(),
    }
}

pub(super) fn statement_document(
    account: &RemoteAccount,
    account_id: i64,
    first: jiff::civil::Date,
    last: jiff::civil::Date,
    bytes: Vec<u8>,
) -> crate::documents::RemoteDocument {
    let month = first.strftime("%Y-%m").to_string();
    crate::documents::RemoteDocument {
        account_ref: account.external_ref.clone(),
        booking_external_id: None,
        title: trf("Kontoauszug {0} – {1}", &[&month, &account.name]),
        doc_type: "kontoauszug".into(),
        mime_type: "application/pdf".into(),
        file_name: format!("{month}_bunq_{}.pdf", slug(&account.name)),
        bytes,
        source_url: format!("bunq:statement:{account_id}:{month}"),
        date: crate::dates::iso(last),
    }
}

/// The `n` most recent completed calendar months as (first day, last day), newest first.
pub(super) fn completed_months(
    today: jiff::civil::Date,
    n: u32,
) -> Vec<(jiff::civil::Date, jiff::civil::Date)> {
    let mut out = Vec::new();
    let mut first_of_this = today.first_of_month();
    for _ in 0..n {
        let last = first_of_this.yesterday().unwrap_or(first_of_this);
        let first = last.first_of_month();
        out.push((first, last));
        first_of_this = first;
    }
    out
}

/// bunq timestamps are UTC ("2026-11-10 12:34:56.123456"); bookings use the local date.
pub(super) fn local_date(ts: &str) -> Option<String> {
    let naive = jiff::civil::DateTime::strptime("%Y-%m-%d %H:%M:%S%.f", ts).ok()?;
    let utc = jiff::tz::TimeZone::UTC.to_timestamp(naive).ok()?;
    Some(crate::dates::iso(crate::dates::local_date(utc)))
}

pub(super) fn booking_text(kind: &str, sub_type: &str) -> String {
    let base = match kind {
        "MASTERCARD" | "MAESTRO" | "FIS" => tr("Kartenzahlung"),
        "IDEAL" => "iDEAL",
        "EBA_SCT" | "SWIFT" => tr("Überweisung"),
        "EBA_SDD" | "DIRECT_DEBIT" => tr("Lastschrift"),
        "INTEREST" => tr("Zinsen"),
        "SAVINGS" => tr("Sparen"),
        "BUNQ" => "bunq",
        other => return other.to_string(),
    };
    match sub_type {
        "REVERSAL" => format!("{base} ({})", tr("Rückbuchung")),
        "WITHDRAWAL" => format!("{base} ({})", tr("Abhebung")),
        _ => base.to_string(),
    }
}

pub(super) fn parse_payment(p: &Value, account_ref: &str) -> Option<RemoteTransaction> {
    let id = p["id"].as_i64()?;
    let date = local_date(p["created"].as_str()?)?;
    let cp = &p["counterparty_alias"];
    let counterparty = cp["display_name"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or(cp["label_user"]["display_name"].as_str())
        .unwrap_or_default();
    Some(RemoteTransaction {
        account_ref: account_ref.to_string(),
        external_id: format!("bunq-{id}"),
        booking_date: date.clone(),
        value_date: date,
        amount_cents: json_number_to_cents(&p["amount"]["value"]).ok()?,
        currency: p["amount"]["currency"]
            .as_str()
            .unwrap_or("EUR")
            .to_string(),
        counterparty: counterparty.trim().to_string(),
        counterparty_iban: cp["iban"].as_str().unwrap_or_default().to_string(),
        purpose: p["description"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_string(),
        booking_text: booking_text(
            p["type"].as_str().unwrap_or_default(),
            p["sub_type"].as_str().unwrap_or_default(),
        ),
        ..Default::default()
    })
}
