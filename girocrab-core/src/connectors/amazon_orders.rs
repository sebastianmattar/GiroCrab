//! Amazon customer account ("Meine Bestellungen"): every order becomes one
//! booking on a synthetic account, refunds become credit bookings.
//!
//! Amazon has no API and guards its login with WAF / captcha / 2FA, so the app
//! signs in inside an embedded WKWebView (same keyless approach as the card
//! portals). A script running in that logged-in page walks the order history,
//! reads every order-details page and hands compact JSON ([`Capture`]) to
//! [`Vault::import_amazon_orders`]. All interpretation (dates, amounts, refund
//! detection) happens here so it is unit-testable without a browser.
//!
//! Ported from the MoneyMoney extension `amazon-bestellungen.lua`; Business /
//! analytics-report handling is intentionally not included.

use std::sync::Arc;

use regex::Regex;
use serde::Deserialize;

use super::portal_import::parse_amount;
use super::{store_fetch, FetchResult, RemoteAccount, RemoteTransaction, SyncSummary};
use crate::error::CoreError;
use crate::i18n::tr;
use crate::models::{AccountKind, ConnectorKind};
use crate::vault::Vault;

/// JSON produced by the in-page scraper.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Capture {
    #[serde(default)]
    pub orders: Vec<Order>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Order {
    /// e.g. `028-1234567-1234567` (or `D01-…` for digital orders)
    pub code: String,
    /// Order date as displayed, e.g. `12. März 2025`.
    #[serde(default)]
    pub date: String,
    #[serde(default)]
    pub items: Vec<Item>,
    /// Order summary rows (label / value text), e.g. "Gesamtsumme" / "45,98 €".
    #[serde(default)]
    pub rows: Vec<Row>,
    /// Details page said the order was cancelled before it was billed.
    #[serde(default)]
    pub cancelled: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Item {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub qty: Option<u32>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Row {
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub bold: bool,
}

/// One normalised booking derived from an order.
#[derive(Debug, Clone, PartialEq)]
pub struct Booking {
    pub id: String,
    pub date: String,
    pub cents: i64,
    pub purpose: String,
    pub text: &'static str,
}

const MONTHS: [(&str, u32); 21] = [
    ("januar", 1),
    ("january", 1),
    ("februar", 2),
    ("february", 2),
    ("märz", 3),
    ("maerz", 3),
    ("march", 3),
    ("april", 4),
    ("mai", 5),
    ("may", 5),
    ("juni", 6),
    ("june", 6),
    ("juli", 7),
    ("july", 7),
    ("august", 8),
    ("september", 9),
    ("oktober", 10),
    ("october", 10),
    ("november", 11),
    ("dezember", 12),
    ("december", 12),
];

/// "12. März 2025", "March 12, 2025", "12.03.2025" -> "2025-03-12".
pub fn parse_date(s: &str) -> Option<String> {
    let s = s.trim();
    if let Some(c) = Regex::new(r"(\d{1,2})\.(\d{1,2})\.(\d{4})")
        .ok()?
        .captures(s)
    {
        return iso(c[3].parse().ok()?, c[2].parse().ok()?, c[1].parse().ok()?);
    }
    let lower = s.to_lowercase();
    let month = MONTHS
        .iter()
        .filter(|(name, _)| lower.contains(name))
        .max_by_key(|(name, _)| name.len())
        .map(|(_, m)| *m)?;
    let nums: Vec<u32> = Regex::new(r"\d+")
        .ok()?
        .find_iter(&lower)
        .filter_map(|m| m.as_str().parse().ok())
        .collect();
    let year = *nums.iter().find(|n| **n >= 1990)?;
    let day = *nums.iter().find(|n| (1..=31).contains(*n))?;
    iso(year, month, day)
}

fn iso(y: u32, m: u32, d: u32) -> Option<String> {
    ((1..=12).contains(&m) && (1..=31).contains(&d)).then(|| format!("{y:04}-{m:02}-{d:02}"))
}

/// First price found in free text ("EUR 12,99", "12,99 €", "Gesamt: 1.234,56 €").
pub fn price_cents(s: &str) -> Option<i64> {
    let re = Regex::new(r"-?\s*\d{1,3}(?:\.\d{3})*,\d{2}|-?\s*\d+,\d{2}").ok()?;
    let m = re.find(s)?;
    parse_amount(m.as_str())
}

fn has(label: &str, needles: &[&str]) -> bool {
    let l = label.to_lowercase();
    needles.iter().any(|n| l.contains(n))
}

/// Order grand total in cents: the "Gesamtsumme"/"Grand Total" row, else the
/// last bold row (refund rows are never the total).
fn total_cents(o: &Order) -> Option<i64> {
    let rows = || {
        o.rows
            .iter()
            .filter(|r| !has(&r.label, &["erstattung", "refund"]))
    };
    rows()
        .filter(|r| {
            has(
                &r.label,
                &[
                    "gesamtsumme",
                    "gesamtbetrag für",
                    "grand total",
                    "order total",
                ],
            )
        })
        .filter(|r| {
            !has(
                &r.label,
                &["zwischensumme", "subtotal", "ohne mwst", "vor "],
            )
        })
        .filter_map(|r| price_cents(&r.value))
        .next_back()
        .or_else(|| {
            rows()
                .filter(|r| r.bold)
                .filter_map(|r| price_cents(&r.value))
                .next_back()
        })
}

fn purpose(o: &Order) -> String {
    let items: Vec<String> = o
        .items
        .iter()
        .filter(|i| !i.title.trim().is_empty())
        .map(|i| {
            let t = i.title.split_whitespace().collect::<Vec<_>>().join(" ");
            match i.qty {
                Some(q) if q > 1 => format!("{q}× {t}"),
                _ => t,
            }
        })
        .collect();
    if items.is_empty() {
        format!("Bestellung {}", o.code)
    } else {
        format!("{} – Bestellung {}", items.join("; "), o.code)
    }
}

/// Orders -> bookings. Cancelled / unbilled / undated orders are skipped.
pub fn bookings(orders: &[Order]) -> Vec<Booking> {
    let mut out = Vec::new();
    for o in orders {
        let code = o.code.trim();
        let Some(date) = parse_date(&o.date) else {
            continue;
        };
        if code.is_empty() || o.cancelled {
            continue;
        }
        let purpose = purpose(o);
        // 0,00 € orders (Audible credits, fully promo-covered) stay visible as 0 € bookings
        if let Some(total) = total_cents(o).filter(|t| *t >= 0) {
            out.push(Booking {
                id: format!("amazon:{code}"),
                date: date.clone(),
                cents: -total,
                purpose: purpose.clone(),
                text: "Amazon-Bestellung",
            });
        }
        for (i, r) in o
            .rows
            .iter()
            .filter(|r| has(&r.label, &["erstattung", "refund"]))
            .enumerate()
        {
            if let Some(c) = price_cents(&r.value).filter(|c| *c > 0) {
                out.push(Booking {
                    id: format!("amazon-refund:{code}:{i}:{c}"),
                    date: date.clone(),
                    cents: c,
                    purpose: format!("Erstattung für Bestellung {code}"),
                    text: "Amazon-Erstattung",
                });
            }
        }
    }
    out
}

fn to_fetch(connection_id: i64, name: &str, bookings: Vec<Booking>) -> FetchResult {
    let account_ref = format!("amazon-orders-{connection_id}");
    let transactions = bookings
        .into_iter()
        .map(|b| RemoteTransaction {
            account_ref: account_ref.clone(),
            external_id: b.id,
            booking_date: b.date.clone(),
            value_date: b.date,
            amount_cents: b.cents,
            currency: "EUR".into(),
            counterparty: "Amazon".into(),
            counterparty_iban: String::new(),
            purpose: b.purpose,
            booking_text: b.text.into(),
            ..Default::default()
        })
        .collect();
    FetchResult {
        accounts: vec![RemoteAccount {
            external_ref: account_ref,
            name: name.to_string(),
            owner: String::new(),
            iban: String::new(),
            account_number: String::new(),
            currency: "EUR".into(),
            kind: AccountKind::PaymentService,
            balance_cents: 0,
            balance_date: super::today(),
        }],
        transactions,
        state: None,
        // orders are not a real balance: accumulate the imported bookings
        adjust_balances: true,
        ..Default::default()
    }
}

/// Amazon has no API: a sync request always asks the app for the in-app
/// browser login, which then calls [`Vault::import_amazon_orders`].
pub struct WebOnlyConnector;

#[async_trait::async_trait]
impl super::BankConnector for WebOnlyConnector {
    async fn begin(
        &mut self,
        _ctx: &super::ConnectContext,
    ) -> crate::error::CoreResult<super::Phase> {
        Err(CoreError::BrowserLoginRequired {
            message: tr("Bitte im Browser-Fenster bei Amazon anmelden.").into(),
        })
    }
    async fn respond(
        &mut self,
        _ctx: &super::ConnectContext,
        _response: super::ChallengeResponse,
    ) -> crate::error::CoreResult<super::Phase> {
        Err(CoreError::BrowserLoginRequired {
            message: tr("Bitte im Browser-Fenster bei Amazon anmelden.").into(),
        })
    }
    async fn fetch(
        &mut self,
        _ctx: &super::ConnectContext,
    ) -> crate::error::CoreResult<FetchResult> {
        Err(CoreError::BrowserLoginRequired {
            message: tr("Bitte im Browser-Fenster bei Amazon anmelden.").into(),
        })
    }
}

fn parse_capture(json: &str) -> Vec<Booking> {
    serde_json::from_str::<Capture>(json)
        .map(|c| bookings(&c.orders))
        .unwrap_or_default()
}

#[uniffi::export]
impl Vault {
    /// Imports the JSON captured from the Amazon order history (see module docs).
    pub fn import_amazon_orders(
        self: Arc<Self>,
        connection_id: i64,
        orders_json: String,
    ) -> Result<SyncSummary, CoreError> {
        let (kind, name) = self.portal_connection(connection_id)?;
        if kind != ConnectorKind::AmazonOrders {
            return Err(CoreError::InvalidInput {
                message: "not an Amazon connection".into(),
            });
        }
        let parsed = parse_capture(&orders_json);
        if parsed.is_empty() {
            return Err(CoreError::InvalidInput {
                message: tr("Keine Bestellungen erkannt.").into(),
            });
        }
        store_fetch(
            &self,
            kind,
            connection_id,
            to_fetch(connection_id, &name, parsed),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(json: &str) -> Order {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn dates() {
        assert_eq!(parse_date("12. März 2025").as_deref(), Some("2025-03-12"));
        assert_eq!(parse_date("3. Januar 2024").as_deref(), Some("2024-01-03"));
        assert_eq!(parse_date("March 12, 2025").as_deref(), Some("2025-03-12"));
        assert_eq!(parse_date("05.11.2025").as_deref(), Some("2025-11-05"));
        assert_eq!(parse_date("bald"), None);
    }

    #[test]
    fn prices() {
        assert_eq!(price_cents("169,98€"), Some(16998));
        assert_eq!(price_cents("EUR 1.234,56"), Some(123456));
        assert_eq!(price_cents("-5,00 €"), Some(-500));
        assert_eq!(price_cents("gratis"), None);
    }

    #[test]
    fn order_becomes_debit_and_refund_credit() {
        let o = order(
            r#"{"code":"028-1234567-1234567","date":"12. März 2025",
                "items":[{"title":"USB-Kabel","qty":2},{"title":"Buch"}],
                "rows":[{"label":"Zwischensumme:","value":"40,00 €"},
                        {"label":"Gesamtsumme:","value":"45,98 €","bold":true},
                        {"label":"Erstattung gesamt","value":"10,00 €"}]}"#,
        );
        let b = bookings(&[o]);
        assert_eq!(b.len(), 2);
        assert_eq!((b[0].cents, b[0].date.as_str()), (-4598, "2025-03-12"));
        assert_eq!(b[0].id, "amazon:028-1234567-1234567");
        assert!(b[0].purpose.contains("2× USB-Kabel; Buch"));
        assert_eq!(b[1].cents, 1000);
    }

    #[test]
    fn cancelled_or_undated_orders_are_skipped() {
        let c = order(
            r#"{"code":"1","date":"1. Mai 2025","cancelled":true,"rows":[{"label":"Gesamtsumme","value":"9,99 €"}]}"#,
        );
        let u =
            order(r#"{"code":"2","date":"","rows":[{"label":"Gesamtsumme","value":"9,99 €"}]}"#);
        assert!(bookings(&[c, u]).is_empty());
    }

    #[test]
    fn digital_order_total_ignores_pre_tax_row() {
        let o = order(
            r#"{"code":"D01-1","date":"9. November 2026","rows":[{"label":"Artikel-Zwischensumme:","value":"9,30 €"},{"label":"Gesamtbetrag vor Steuern:","value":"8,40 €"},{"label":"MwSt:","value":"1,59 €"},{"label":"Gesamtbetrag für diese Bestellung:","value":"9,99 €"}]}"#,
        );
        assert_eq!(bookings(&[o])[0].cents, -999);
    }

    #[test]
    fn zero_euro_order_is_kept_as_zero_booking() {
        let o = order(
            r#"{"code":"D01-2","date":"3. November 2026","items":[{"title":"Hörbuch"}],"rows":[{"label":"Artikel-Zwischensumme:","value":"0,00 €"},{"label":"Gesamtbetrag für diese Bestellung:","value":"0,00 €"}]}"#,
        );
        let b = bookings(&[o]);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].cents, 0);
        assert!(b[0].purpose.contains("Hörbuch"));
    }

    #[test]
    fn falls_back_to_last_bold_row() {
        let o = order(
            r#"{"code":"3","date":"1. Mai 2025","rows":[{"label":"Summe","value":"7,00 €"},{"label":"Endbetrag","value":"8,50 €","bold":true}]}"#,
        );
        assert_eq!(bookings(&[o])[0].cents, -850);
    }
}
