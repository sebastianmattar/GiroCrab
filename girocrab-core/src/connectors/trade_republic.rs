//! Trade Republic – web login + WebSocket API.
//!
//! Trade Republic publishes no official API; this connector follows the
//! publicly observable behaviour of the web app (as also used by the
//! open-source `pytr` project):
//!
//!  POST https://api.traderepublic.com/api/v1/auth/web/login       (fallback: /api/v2/…)
//!       {"phoneNumber": "+49…", "pin": "1234"}   -> {"processId", "countdownInSeconds"}
//!  POST …/auth/web/login/{processId}/sms                          (optional: code per SMS)
//!  POST …/auth/web/login/{processId}/{code}  -> HTTP 200, often EMPTY body;
//!       the session arrives as Set-Cookie (tr_session, tr_refresh)
//!  GET  …/api/v1/auth/web/session            -> refreshes a stored session
//!  WSS  wss://api.traderepublic.com  (Cookie + Origin headers)
//!       "connect 31 {…}" -> "connected"
//!       "sub 1 {\"type\":\"cash\"}"                         (fallback "availableCash")
//!       "sub 2 {\"type\":\"timelineTransactions\",\"after\":…}"
//!       frames: "<id> A <json>" (answer), "<id> E <json>" (error), "<id> C" (complete)
//!
//! The session cookies are stored (encrypted) in the connection state so the
//! next sync needs no new 2FA code. Cookies from the in-app browser login
//! (WKWebView) can be handed over with `Vault::set_connection_session`.
//! It may break whenever Trade Republic changes its web app.

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use reqwest::cookie::{CookieStore, Jar};
use reqwest::header::{ACCEPT, HeaderMap, HeaderValue, ORIGIN, REFERER, SET_COOKIE, USER_AGENT};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use super::{
    BROWSER_UA, BankConnector, Challenge, ChallengeKind, ChallengeResponse, ConnectContext,
    FetchResult, Phase, RemoteAccount, RemoteHolding, RemoteTransaction,
};
use crate::asset_classification::{AssetClass, ClassificationValues, InstrumentKind};
use crate::error::{CoreError, CoreResult};
use crate::i18n::{tr, trf};
use crate::models::{AccountKind, Holding};
use crate::money::json_number_to_cents;
use crate::secret::{SecretJson, SecretString};

const API: &str = "https://api.traderepublic.com";
const WS: &str = "wss://api.traderepublic.com";
const APP_ORIGIN: &str = "https://app.traderepublic.com";

/// Strict E.164 for German numbers: "0171 1234567", "0049171…", "49171…",
/// "17012345678" -> "+491711234567" / "+4917012345678".
pub fn normalize_phone_number(input: String) -> String {
    normalize_phone(&input)
}

pub fn normalize_phone(input: &str) -> String {
    let t = input.trim();
    let plus = t.starts_with('+');
    let digits: String = t.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return String::new();
    }
    if plus {
        return format!("+{digits}");
    }
    if let Some(rest) = digits.strip_prefix("00") {
        return format!("+{rest}");
    }
    if let Some(rest) = digits.strip_prefix('0') {
        return format!("+49{rest}");
    }
    if digits.starts_with("49") && digits.len() >= 12 {
        return format!("+{digits}");
    }
    // German mobile numbers without the trunk 0 ("1512…", "171…")
    format!("+49{digits}")
}

/// Value of one cookie in a "a=1; b=2" header string.
fn cookie_value(cookies: &str, name: &str) -> Option<String> {
    cookies
        .split(';')
        .filter_map(|p| p.trim().split_once('='))
        .find(|(k, _)| k.trim() == name)
        .map(|(_, v)| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// "a=1; b=2" + Set-Cookie values -> merged "name=value; …" (later wins).
fn merge_cookies(existing: &str, set_cookie_values: &[String]) -> String {
    let mut pairs: Vec<(String, String)> = existing
        .split(';')
        .filter_map(|p| {
            p.trim()
                .split_once('=')
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        })
        .filter(|(k, _)| !k.is_empty())
        .collect();
    for sc in set_cookie_values {
        let first = sc.split(';').next().unwrap_or("");
        if let Some((k, v)) = first.split_once('=') {
            let (k, v) = (k.trim().to_string(), v.trim().to_string());
            pairs.retain(|(pk, _)| *pk != k);
            if !v.is_empty() {
                pairs.push((k, v));
            }
        }
    }
    pairs
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ")
}

fn snippet(body: &str) -> String {
    let b = body.trim();
    if b.is_empty() {
        "(leere Antwort)".into()
    } else if b.chars().count() > 300 {
        format!("{}…", b.chars().take(300).collect::<String>())
    } else {
        b.to_string()
    }
}

/// Amounts arrive as JSON numbers that may carry float noise (-4.040000000000001).
fn amount_cents(v: &Value) -> i64 {
    json_number_to_cents(v)
        .unwrap_or_else(|_| v.as_f64().map(|f| (f * 100.0).round() as i64).unwrap_or(0))
}

/// A number in a detail text such as "1.234,56 €", "0,5" or "12.5": German
/// notation first. A lone `.` followed by exactly three digits after at most
/// three is a thousands separator ("1.234"), otherwise a decimal point.
fn parse_decimal_text(text: &str) -> Option<f64> {
    let t: String = text
        .chars()
        .filter(|c| c.is_ascii_digit() || matches!(c, ',' | '.' | '-' | '−'))
        .map(|c| if c == '−' { '-' } else { c })
        .collect();
    if t.is_empty() || !t.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    let norm = match (t.rfind(','), t.rfind('.')) {
        (Some(c), Some(d)) if c > d => t.replace('.', "").replace(',', "."),
        (Some(_), Some(_)) => t.replace(',', ""),
        (Some(_), None) => t.replace(',', "."),
        (None, Some(d)) => {
            let (head, tail) = t.split_at(d);
            let thousands = tail.len() == 4
                && head.trim_start_matches('-').len() <= 3
                && !head.trim_start_matches('-').contains('.');
            if thousands { t.replace('.', "") } else { t }
        }
        (None, None) => t,
    };
    norm.parse().ok()
}

/// What a trade's detail screen (`timelineDetailV2`) tells besides the
/// timeline row: ISIN, units, price per unit, fees and taxes.
#[derive(Debug, Default, PartialEq)]
struct TradeDetail {
    isin: Option<String>,
    quantity: Option<f64>,
    price_cents: Option<i64>,
    fees_cents: Option<i64>,
    taxes_cents: Option<i64>,
}

/// `logos/US0378331005/v2` or a bare ISIN → the ISIN.
fn isin_of(icon: &str) -> Option<String> {
    let rest = icon.find('/').map_or(icon, |i| &icon[i + 1..]);
    let isin = rest.split('/').next().unwrap_or_default();
    is_isin(isin).then(|| isin.to_string())
}

/// Shaped like an ISIN: two letters, then ten letters or digits.
fn is_isin(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 12
        && b[..2].iter().all(u8::is_ascii_uppercase)
        && b[2..]
            .iter()
            .all(|c| c.is_ascii_digit() || c.is_ascii_uppercase())
}

fn walk_rows(v: &Value, f: &mut dyn FnMut(&str, &str)) {
    match v {
        Value::Object(m) => {
            if let Some(title) = m.get("title").and_then(Value::as_str) {
                let text = m
                    .get("detail")
                    .and_then(|d| d["text"].as_str().or_else(|| d.as_str()))
                    .unwrap_or_default();
                f(title, text);
            }
            m.values().for_each(|x| walk_rows(x, f));
        }
        Value::Array(a) => a.iter().for_each(|x| walk_rows(x, f)),
        _ => {}
    }
}

fn parse_trade_detail(item_icon: &str, detail: &Value) -> TradeDetail {
    let mut d = TradeDetail {
        isin: isin_of(item_icon),
        ..Default::default()
    };
    if d.isin.is_none() {
        // header section carries the instrument icon, an action its ISIN
        if let Some(sections) = detail["sections"].as_array() {
            d.isin = sections.iter().find_map(|s| {
                let icon = s["data"]["icon"]
                    .as_str()
                    .or_else(|| s["data"]["icon"]["asset"].as_str());
                icon.and_then(isin_of).or_else(|| {
                    (s["action"]["type"] == "instrumentDetail")
                        .then(|| s["action"]["payload"].as_str().and_then(isin_of))
                        .flatten()
                })
            });
        }
    }
    let cents = |t: &str| parse_decimal_text(t).map(|f| (f * 100.0).round() as i64);
    walk_rows(detail, &mut |title, text| {
        match title {
            "Aktien" | "Anteile" | "Shares" | "Aktien hinzugefügt" | "Aktien entfernt"
                if d.quantity.is_none() =>
            {
                d.quantity = parse_decimal_text(text).map(f64::abs);
            }
            "Preis" | "Kurs" | "Aktienkurs" | "Quotation" | "Price" if d.price_cents.is_none() => {
                d.price_cents = cents(text).map(i64::abs);
            }
            "Gebühr" | "Gebühren" | "Fee" if d.fees_cents.is_none() => {
                d.fees_cents = cents(text).map(i64::abs);
            }
            "Steuer" | "Steuern" | "Tax" if d.taxes_cents.is_none() => {
                d.taxes_cents = cents(text).map(i64::abs);
            }
            _ => {}
        };
    });
    d
}

/// A timeline row of a trade, dividend or savings plan that has a detail screen.
struct TradeRow {
    id: String,
    date: String,
    title: String,
    kind_text: String,
    amount_cents: i64,
    currency: String,
    icon: String,
}

fn is_trade_kind(kind_text: &str) -> bool {
    [
        "kauf",
        "sparplan",
        "savings_plan",
        "trade",
        "dividende",
        "dividend",
        "ausschüttung",
    ]
    .iter()
    .any(|k| kind_text.contains(k))
}

/// The securities side of a trade row (#314). Direction from the row's text
/// (sell before buy: "Verkauf" contains "kauf"), ISIN, units, price, fees and
/// taxes from the detail screen. Without ISIN or direction there is no
/// securities transaction. Value: units × price, else the cash amount
/// without fees and taxes. Best effort: the screen is not a documented API.
fn trade_security(row: &TradeRow, detail: &Value) -> Option<super::RemoteSecurityTransaction> {
    use crate::models::{SecurityDirection, SecurityTransaction};
    let k = &row.kind_text;
    let direction = if k.contains("dividend") || k.contains("ausschüttung") {
        SecurityDirection::Dividend
    } else if k.contains("verkauf") || k.contains("sell") {
        SecurityDirection::Sell
    } else if k.contains("kauf")
        || k.contains("sparplan")
        || k.contains("savings_plan")
        || k.contains("buy")
    {
        SecurityDirection::Buy
    } else {
        return None;
    };
    let d = parse_trade_detail(&row.icon, detail);
    let isin = d.isin?;
    let fees = d.fees_cents.unwrap_or(0);
    let taxes = d.taxes_cents.unwrap_or(0);
    let cash = row.amount_cents.abs();
    let quantity = d.quantity.unwrap_or(0.0);
    let by_price = d
        .price_cents
        .filter(|_| quantity > 0.0)
        .map(|p| (p as f64 * quantity).round() as i64);
    let value = by_price.unwrap_or(match direction {
        SecurityDirection::Buy => (cash - fees - taxes).max(0),
        SecurityDirection::Sell => cash + fees + taxes,
        _ => cash + taxes,
    });
    let price_cents = d.price_cents.unwrap_or(if quantity > 0.0 {
        (value as f64 / quantity).round() as i64
    } else {
        0
    });
    Some(super::RemoteSecurityTransaction {
        account_ref: "tr:depot".into(),
        external_id: row.id.clone(),
        cash_external_id: row.id.clone(),
        transaction: SecurityTransaction {
            booking_date: row.date.clone(),
            isin,
            name: row.title.clone(),
            direction,
            quantity,
            price_cents,
            currency: if row.currency.is_empty() {
                "EUR".into()
            } else {
                row.currency.clone()
            },
            value_cents: value,
            fees_cents: fees,
            taxes_cents: taxes,
            booking_text: row.kind_text.trim().to_string(),
            ..Default::default()
        },
    })
}

// --- Depot positions (compactPortfolioByType + instrument + ticker)

/// One position of the portfolio answer.
#[derive(Debug, Default, Clone, PartialEq)]
struct RawPosition {
    /// Instrument id as Trade Republic reports it: an ISIN, for some assets another key.
    isin: String,
    name: Option<String>,
    category: String,
    quantity: f64,
    /// Average purchase price per piece (bonds: see [`holding_of`]).
    average_buy_in: Option<f64>,
}

/// A JSON number, or a number sent as text ("40.745", "1.234,56").
fn json_as_f64(v: &Value) -> Option<f64> {
    if let Some(f) = v.as_f64() {
        return Some(f);
    }
    let text = v.as_str()?.trim();
    // plain decimals first: "40.745" is a price, not forty thousand
    text.parse::<f64>()
        .ok()
        .or_else(|| parse_decimal_text(text))
        .filter(|f| f.is_finite())
}

fn parse_position_item(item: &Value, default_category: &str) -> Option<RawPosition> {
    let isin = item["instrumentId"]
        .as_str()
        .or_else(|| item["isin"].as_str())
        .or_else(|| item["id"].as_str())
        .map(|s| s.trim().to_string())
        .or_else(|| item["icon"].as_str().and_then(isin_of))
        .filter(|s| !s.is_empty())?;
    let quantity = ["netSize", "shares", "size", "quantity", "units"]
        .iter()
        .find_map(|k| json_as_f64(&item[*k]))
        .unwrap_or(0.0);
    if quantity <= 0.0 {
        return None;
    }
    let average_buy_in = ["averageBuyIn", "buyIn", "averagePrice", "purchasePrice"]
        .iter()
        .find_map(|k| json_as_f64(&item[*k]));
    let name = ["name", "title", "shortName"]
        .iter()
        .find_map(|k| item[*k].as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let category = ["category", "type", "instrumentType"]
        .iter()
        .find_map(|k| item[*k].as_str())
        .unwrap_or(default_category)
        .to_string();
    Some(RawPosition {
        isin,
        name,
        category,
        quantity,
        average_buy_in,
    })
}

const DEFAULT_CATEGORY: &str = "stocksAndETFs";

/// The positions of a portfolio answer. `None` = the answer has none of the
/// known layouts: it says nothing about the depot (and must not empty it).
fn portfolio_positions(val: &Value) -> Option<Vec<RawPosition>> {
    fn category(v: &Value) -> &str {
        v["type"]
            .as_str()
            .or_else(|| v["categoryType"].as_str())
            .unwrap_or(DEFAULT_CATEGORY)
    }
    let mut out = Vec::new();
    if let Some(categories) = val["categories"].as_array() {
        for cat in categories {
            for pos in cat["positions"].as_array().into_iter().flatten() {
                out.extend(parse_position_item(pos, category(cat)));
            }
        }
    } else if let Some(positions) = val["positions"].as_array() {
        for pos in positions {
            out.extend(parse_position_item(pos, DEFAULT_CATEGORY));
        }
    } else {
        for item in val.as_array()? {
            if let Some(positions) = item["positions"].as_array() {
                for pos in positions {
                    out.extend(parse_position_item(pos, category(item)));
                }
            } else {
                out.extend(parse_position_item(item, DEFAULT_CATEGORY));
            }
        }
    }
    Some(out)
}

/// Securities account number of the session (`accountPairs`), the default product first.
fn securities_account_number(pairs: &Value) -> Option<String> {
    let mut accounts: Vec<&Value> = pairs["accounts"]
        .as_array()?
        .iter()
        .filter(|a| {
            a["securitiesAccountNumber"]
                .as_str()
                .is_some_and(|s| !s.is_empty())
        })
        .collect();
    accounts.sort_by_key(|a| a["productType"].as_str() != Some("DEFAULT"));
    accounts
        .first()
        .and_then(|a| a["securitiesAccountNumber"].as_str())
        .map(str::to_string)
}

/// What the `instrument` answer tells about a position.
#[derive(Debug, Default, Clone, PartialEq)]
struct InstrumentInfo {
    name: String,
    short_name: String,
    wkn: String,
    /// First exchange of `exchangeIds`: the ticker id is `<ISIN>.<exchange>`.
    exchange: Option<String>,
    /// `stock`, `fund`, `etf`, `crypto`, `bond`, `derivative` …
    type_id: String,
    /// `fundInfo.category` (`etf`).
    fund_category: String,
    /// `fundInfo.underlyingClass`: what the fund invests in ("Aktien", "Festverzinslich" …).
    underlying_class: String,
    /// `priceFactor`; only traced, to learn how bonds are quoted.
    price_factor: Option<f64>,
}

fn instrument_info(v: &Value) -> InstrumentInfo {
    let text = |v: &Value| v.as_str().unwrap_or_default().trim().to_string();
    InstrumentInfo {
        name: text(&v["name"]),
        short_name: text(&v["shortName"]),
        wkn: text(&v["wkn"]),
        exchange: v["exchangeIds"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        type_id: ["typeId", "type", "instrumentType"]
            .iter()
            .find_map(|k| v[*k].as_str())
            .unwrap_or_default()
            .trim()
            .to_lowercase(),
        fund_category: text(&v["fundInfo"]["category"]).to_lowercase(),
        underlying_class: text(&v["fundInfo"]["underlyingClass"]),
        price_factor: v["priceFactor"].as_f64(),
    }
}

/// `word` as a whole word of `text` ("ETC", not the "etc" in "Fletcher").
fn has_word(text: &str, word: &str) -> bool {
    text.split(|c: char| !c.is_alphanumeric())
        .any(|t| t.eq_ignore_ascii_case(word))
}

fn is_bond(category: &str, info: &InstrumentInfo) -> bool {
    let cat = category.to_lowercase();
    info.type_id == "bond" || cat.contains("bond") || cat.contains("anleihe")
}

fn is_crypto(category: &str, info: &InstrumentInfo) -> bool {
    let cat = category.to_lowercase();
    info.type_id == "crypto" || cat.contains("crypto") || cat.contains("krypto")
}

/// Asset class a fund invests in; an unknown label stays open for the name rules.
fn underlying_asset_class(label: &str) -> Option<AssetClass> {
    let l = label.trim().to_lowercase();
    if l.is_empty() {
        return None;
    }
    if let Some(known) = crate::asset_classification::from_labels(&l, "").asset_class {
        return Some(known);
    }
    let has = |parts: &[&str]| parts.iter().any(|p| l.contains(p));
    if has(&["multi", "misch"]) {
        Some(AssetClass::MultiAsset)
    } else if has(&["money", "geldmarkt"]) {
        Some(AssetClass::Cash)
    } else if has(&["bond", "anleihe", "renten", "fixed", "festverzins"]) {
        Some(AssetClass::Bond)
    } else if has(&["commodit", "rohstoff", "metal"]) {
        Some(AssetClass::Commodity)
    } else if has(&["immobil", "realestate", "real estate", "property"]) {
        Some(AssetClass::RealEstate)
    } else if has(&["crypto", "krypto"]) {
        Some(AssetClass::Crypto)
    } else if has(&["equit", "aktie", "stock"]) {
        Some(AssetClass::Equity)
    } else {
        None
    }
}

/// Classification from what Trade Republic says about the instrument, and
/// whether it is a directly held crypto asset. Only stated facts are imported:
/// the instrument type decides, never a substring of the name ("Netflix" is
/// no ETF), and a fund's asset class comes from its underlying class. What
/// stays `None` is left to the core's ISIN and name rules.
fn classify_tr_instrument(
    category: &str,
    info: &InstrumentInfo,
    name: &str,
) -> (ClassificationValues, bool) {
    let values = |asset_class, instrument| ClassificationValues {
        asset_class,
        instrument,
    };
    if is_crypto(category, info) {
        return (
            values(
                Some(AssetClass::Crypto),
                Some(InstrumentKind::DirectHolding),
            ),
            true,
        );
    }
    if is_bond(category, info) {
        return (
            values(Some(AssetClass::Bond), Some(InstrumentKind::SingleSecurity)),
            false,
        );
    }
    let cat = category.to_lowercase();
    if matches!(
        info.type_id.as_str(),
        "derivative" | "derivate" | "warrant" | "knockout" | "factor"
    ) || cat.contains("derivat")
    {
        return (values(None, Some(InstrumentKind::Derivative)), false);
    }
    let underlying = underlying_asset_class(&info.underlying_class);
    let named = |word: &str| has_word(name, word) || has_word(&info.name, word);
    if named("ETC") {
        return (
            values(
                underlying.or(Some(AssetClass::Commodity)),
                Some(InstrumentKind::EtcEtn),
            ),
            false,
        );
    }
    if named("ETN") {
        return (values(underlying, Some(InstrumentKind::EtcEtn)), false);
    }
    let classification = match info.type_id.as_str() {
        "etf" => values(underlying, Some(InstrumentKind::Etf)),
        "fund" if info.fund_category == "etf" => values(underlying, Some(InstrumentKind::Etf)),
        "fund" => values(underlying, Some(InstrumentKind::Fund)),
        "stock" => values(
            Some(AssetClass::Equity),
            Some(InstrumentKind::SingleSecurity),
        ),
        _ => values(None, None),
    };
    (classification, false)
}

/// Euro amount to cents, half away from zero.
fn to_cents(p: f64) -> i64 {
    if !p.is_finite() {
        return 0;
    }
    // the epsilon keeps .5 cases that binary floats store just below (40.745 * 100 = 4074.4999…)
    let eps = if p >= 0.0 { 1e-7 } else { -1e-7 };
    (p * 100.0 + eps).round() as i64
}

/// Prices of a `ticker` answer.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct Quote {
    last: Option<f64>,
    /// Previous close.
    pre: Option<f64>,
}

fn quote_of(tick: &Value) -> Quote {
    let price = |v: &Value| {
        json_as_f64(&v["price"])
            .or_else(|| json_as_f64(v))
            .filter(|p| *p > 0.0)
    };
    Quote {
        last: price(&tick["last"]).or_else(|| price(&tick["bid"])),
        pre: price(&tick["pre"]),
    }
}

/// Bonds are quoted in percent of the nominal (the quantity): 99.5 means 0.995 per unit.
const BOND_PRICE_FACTOR: f64 = 0.01;

/// The stored position. Values come from the unrounded price (quantity ×
/// price, then cents), so the depot total matches the bank's.
///
/// Without a quote the position is valued at cost (price = purchase price) and
/// reports no gain, instead of a value of zero.
fn holding_of(
    raw: &RawPosition,
    info: &InstrumentInfo,
    trade_name: Option<&str>,
    quote: Quote,
) -> RemoteHolding {
    let bond = is_bond(&raw.category, info);
    let scale = if bond { BOND_PRICE_FACTOR } else { 1.0 };
    let last = quote.last.map(|p| p * scale);
    let pre = quote.pre.map(|p| p * scale);
    let buy_in = raw.average_buy_in.filter(|p| *p > 0.0).map(|p| {
        // A bond's purchase price is in percent like its quote. Should it come per
        // unit already, it is close to the scaled quote and stays as it is.
        let per_unit = bond && last.is_some_and(|l| p < l * 10.0);
        if bond && !per_unit {
            p * BOND_PRICE_FACTOR
        } else {
            p
        }
    });

    let purchase_value_cents = buy_in.map(|p| to_cents(raw.quantity * p));
    let value_cents = last
        .map(|p| to_cents(raw.quantity * p))
        .or(purchase_value_cents)
        .unwrap_or(0);
    let prev_day_value_cents = pre.map(|p| to_cents(raw.quantity * p));
    // gains only against a real quote
    let gain = |base: Option<i64>| {
        let base = base.filter(|b| *b > 0)?;
        last?;
        Some((
            value_cents - base,
            (value_cents - base) as f64 / base as f64 * 100.0,
        ))
    };
    let purchase_gain = gain(purchase_value_cents);
    let day_gain = gain(prev_day_value_cents);

    let (classification, direct_crypto) =
        classify_tr_instrument(&raw.category, info, raw.name.as_deref().unwrap_or_default());
    // The core knows a directly held coin by its ticker in `wkn` with no ISIN.
    let (isin, wkn) = if direct_crypto && !is_isin(&raw.isin) {
        (String::new(), raw.isin.to_ascii_uppercase())
    } else {
        (raw.isin.clone(), info.wkn.clone())
    };
    let name = [
        raw.name.as_deref().unwrap_or_default(),
        &info.short_name,
        &info.name,
        trade_name.unwrap_or_default(),
        &raw.isin,
    ]
    .into_iter()
    .find(|s| !s.is_empty())
    .unwrap_or_default()
    .to_string();

    RemoteHolding {
        classification,
        direct_crypto,
        account_ref: DEPOT_REF.into(),
        holding: Holding {
            isin,
            wkn,
            name,
            quantity: raw.quantity,
            current_price_cents: last.or(buy_in).map(to_cents).unwrap_or(0),
            purchase_price_cents: buy_in.map(to_cents).unwrap_or(0),
            value_cents,
            prev_day_price_cents: pre.map(to_cents),
            prev_day_value_cents,
            profit_loss_purchase_cents: purchase_gain.map(|g| g.0),
            profit_loss_purchase_rel: purchase_gain.map(|g| g.1),
            profit_loss_prev_day_cents: day_gain.map(|g| g.0),
            profit_loss_prev_day_rel: day_gain.map(|g| g.1),
            // Trade Republic does not report how a position is held
            custody_type: String::new(),
            ..Default::default()
        },
    }
}

const DEPOT_REF: &str = "tr:depot";

/// Layout of an answer for the depot debug log: keys and value types only,
/// never a value. Keys that are identifiers themselves are masked.
fn shape(v: &Value, depth: u8) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(_) => "bool".into(),
        Value::Number(_) => "num".into(),
        Value::String(_) => "str".into(),
        Value::Array(a) if depth == 0 => format!("[{}]", a.len()),
        Value::Array(a) => format!(
            "[{}x {}]",
            a.len(),
            a.first().map(|x| shape(x, depth - 1)).unwrap_or_default()
        ),
        Value::Object(m) if depth == 0 => format!("{{{}}}", m.len()),
        Value::Object(m) => {
            let fields: Vec<String> = m
                .iter()
                .map(|(k, x)| {
                    let plain = k.len() <= 32
                        && !is_isin(k)
                        && k.chars().all(|c| c.is_ascii_alphabetic() || c == '_');
                    let key = if plain { k.as_str() } else { "<id>" };
                    format!("{key}:{}", shape(x, depth - 1))
                })
                .collect();
            format!("{{{}}}", fields.join(","))
        }
    }
}

/// A failed subscription request. `transport` = the connection itself failed
/// (send error, closed socket); otherwise the server refused or stayed silent
/// and the connection is still usable.
struct RequestError {
    transport: bool,
    error: CoreError,
}

const REQUEST_WAIT: std::time::Duration = std::time::Duration::from_secs(20);
const ACCOUNT_WAIT: std::time::Duration = std::time::Duration::from_secs(5);
const INSTRUMENT_WAIT: std::time::Duration = std::time::Duration::from_secs(10);
/// Instruments without a quote never answer: do not hold up the sync for them.
const TICKER_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Subscribes, waits up to `wait` for the first answer ("<id> A <json>") and unsubscribes.
async fn request_within<S>(
    ws: &mut S,
    id: u32,
    mut payload: Value,
    token: &str,
    wait: std::time::Duration,
) -> Result<Value, RequestError>
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
        + futures_util::Sink<Message>
        + Unpin,
    <S as futures_util::Sink<Message>>::Error: std::fmt::Display,
{
    let transport = |error| RequestError {
        transport: true,
        error,
    };
    let refused = |error| RequestError {
        transport: false,
        error,
    };
    if !token.is_empty() {
        payload["token"] = json!(token);
    }
    let kind = payload["type"].as_str().unwrap_or("?").to_string();
    ws.send(Message::text(format!("sub {id} {payload}")))
        .await
        .map_err(|e| transport(TradeRepublicConnector::ws_err(e)))?;
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let msg = match tokio::time::timeout_at(deadline, ws.next()).await {
            Err(_) => {
                let _ = ws.send(Message::text(format!("unsub {id}"))).await;
                return Err(refused(CoreError::Network {
                    message: trf("Trade Republic hat auf „{0}“ nicht geantwortet.", &[&kind]),
                }));
            }
            Ok(None) => {
                return Err(transport(CoreError::Network {
                    message: trf("WebSocket während „{0}“ geschlossen.", &[&kind]),
                }));
            }
            Ok(Some(Err(e))) => return Err(transport(TradeRepublicConnector::ws_err(e))),
            Ok(Some(Ok(msg))) => msg,
        };
        let Message::Text(text) = msg else { continue };
        let text = text.to_string();
        let mut parts = text.splitn(3, ' ');
        let (Some(rid), Some(code)) = (parts.next(), parts.next()) else {
            continue;
        };
        if rid != id.to_string() {
            continue;
        }
        let body = parts.next().unwrap_or("null");
        let _ = ws.send(Message::text(format!("unsub {id}"))).await;
        return match code {
            "A" => serde_json::from_str(body).map_err(|e| refused(e.into())),
            "E" => Err(refused(CoreError::Bank {
                code: format!("WS {kind}"),
                message: trf("Fehler-Frame: {0}", &[&snippet(body)]),
            })),
            _ => Ok(Value::Null),
        };
    }
}

/// [`request_within`] with the default wait; any failure is an error.
async fn request<S>(ws: &mut S, id: u32, payload: Value, token: &str) -> CoreResult<Value>
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
        + futures_util::Sink<Message>
        + Unpin,
    <S as futures_util::Sink<Message>>::Error: std::fmt::Display,
{
    request_within(ws, id, payload, token, REQUEST_WAIT)
        .await
        .map_err(|e| e.error)
}

/// The complete depot inventory, largest position first. `None` = it could not
/// be read reliably (no usable portfolio answer, or the connection failed on
/// the way): the stored positions and the depot value must stay as they are.
async fn fetch_portfolio<S>(
    ws: &mut S,
    sub_id: &mut u32,
    token: &str,
    trade_names: &std::collections::HashMap<String, String>,
) -> Option<Vec<RemoteHolding>>
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
        + futures_util::Sink<Message>
        + Unpin,
    <S as futures_util::Sink<Message>>::Error: std::fmt::Display,
{
    use crate::depot_debug::log;
    let mut next_id = || {
        *sub_id += 2;
        *sub_id
    };

    // The portfolio is asked per securities account; older sessions answer without it.
    let sec_acc_no = match request_within(
        ws,
        next_id(),
        json!({"type": "accountPairs"}),
        token,
        ACCOUNT_WAIT,
    )
    .await
    {
        Ok(pairs) => securities_account_number(&pairs),
        Err(e) if e.transport => {
            log("trade republic: accountPairs: connection failed, depot unchanged");
            return None;
        }
        Err(_) => None,
    };
    let mut attempts = vec![
        ("byType", json!({"type": "compactPortfolioByType"})),
        ("legacy", json!({"type": "compactPortfolio"})),
    ];
    if let Some(number) = &sec_acc_no {
        attempts.insert(
            0,
            (
                "byType+secAccNo",
                json!({"type": "compactPortfolioByType", "secAccNo": number}),
            ),
        );
    }
    let mut positions = None;
    for (label, payload) in attempts {
        match request_within(ws, next_id(), payload, token, REQUEST_WAIT).await {
            Ok(answer) => match portfolio_positions(&answer) {
                // only the current request may report an empty depot
                Some(found) if !found.is_empty() || label != "legacy" => {
                    log(&format!(
                        "trade republic: portfolio via {label}: positions={} shape={}",
                        found.len(),
                        shape(&answer, 5)
                    ));
                    positions = Some(found);
                    break;
                }
                _ => log(&format!(
                    "trade republic: portfolio via {label}: not usable, shape={}",
                    shape(&answer, 5)
                )),
            },
            Err(e) if e.transport => {
                log("trade republic: portfolio: connection failed, depot unchanged");
                return None;
            }
            Err(_) => log(&format!("trade republic: portfolio via {label}: no answer")),
        }
    }
    let positions = positions?;

    let mut holdings = Vec::with_capacity(positions.len());
    for (i, pos) in positions.iter().enumerate() {
        let info = match request_within(
            ws,
            next_id(),
            json!({"type": "instrument", "id": &pos.isin}),
            token,
            INSTRUMENT_WAIT,
        )
        .await
        {
            Ok(answer) => {
                if i == 0 {
                    log(&format!(
                        "trade republic: instrument shape={}",
                        shape(&answer, 2)
                    ));
                }
                instrument_info(&answer)
            }
            Err(e) if e.transport => {
                log("trade republic: instrument: connection failed, depot unchanged");
                return None;
            }
            Err(_) => InstrumentInfo::default(),
        };
        let exchange = info.exchange.as_deref().unwrap_or("LSX");
        let quote = match request_within(
            ws,
            next_id(),
            json!({"type": "ticker", "id": format!("{}.{}", pos.isin, exchange)}),
            token,
            TICKER_WAIT,
        )
        .await
        {
            Ok(answer) => {
                if i == 0 {
                    log(&format!(
                        "trade republic: ticker shape={}",
                        shape(&answer, 2)
                    ));
                }
                quote_of(&answer)
            }
            Err(e) if e.transport => {
                log("trade republic: ticker: connection failed, depot unchanged");
                return None;
            }
            Err(_) => Quote::default(),
        };
        // codes and flags only: no instrument id, name, quantity or price
        log(&format!(
            "trade republic: position {i}: category={} type={} fund={} underlying={} factor={:?} exchange={} isin_id={} last={} pre={} buy_in={}",
            pos.category,
            info.type_id,
            info.fund_category,
            info.underlying_class,
            info.price_factor,
            exchange,
            is_isin(&pos.isin),
            quote.last.is_some(),
            quote.pre.is_some(),
            pos.average_buy_in.is_some(),
        ));
        holdings.push(holding_of(
            pos,
            &info,
            trade_names.get(&pos.isin).map(String::as_str),
            quote,
        ));
    }
    holdings.sort_by_key(|h| std::cmp::Reverse(h.holding.value_cents));
    Some(holdings)
}

/// Connection state saved between syncs (inside the encrypted vault).
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct StoredSession {
    /// "tr_session=…; tr_refresh=…"
    cookies: SecretString,
    session_token: SecretString,
    phone: String,
    saved_unix: i64,
    /// Timeline item ids whose documents were already downloaded, oldest first.
    doc_items: Vec<String>,
}

/// Document ids kept in the state; the oldest are dropped first.
const MAX_DOC_ITEMS: usize = 2000;

/// Body of the PIN login; borrowed so the PIN is not copied before sending.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LoginRequest<'a> {
    phone_number: &'a str,
    pin: &'a str,
}

pub struct TradeRepublicConnector {
    jar: Arc<Jar>,
    client: Option<reqwest::Client>,
    process_id: String,
    session: StoredSession,
    challenge: Option<Challenge>,
}

impl TradeRepublicConnector {
    pub fn new() -> Self {
        TradeRepublicConnector {
            jar: Arc::new(Jar::default()),
            client: None,
            process_id: String::new(),
            session: StoredSession::default(),
            challenge: None,
        }
    }

    fn ws_err(e: impl std::fmt::Display) -> CoreError {
        CoreError::Network {
            message: format!("WebSocket: {e}"),
        }
    }

    fn client(&mut self) -> CoreResult<reqwest::Client> {
        if let Some(c) = &self.client {
            return Ok(c.clone());
        }
        let c = self.build_client(super::same_host_redirects())?;
        self.client = Some(c.clone());
        Ok(c)
    }

    /// Client for document downloads (GET without credentials in the body). Presigned
    /// document URLs may redirect to a storage host, so redirects to other hosts are
    /// followed here; reqwest drops the manual Cookie header on a cross-host redirect.
    fn document_client(&self) -> CoreResult<reqwest::Client> {
        self.build_client(reqwest::redirect::Policy::default())
    }

    fn build_client(&self, redirect: reqwest::redirect::Policy) -> CoreResult<reqwest::Client> {
        let mut h = HeaderMap::new();
        h.insert(USER_AGENT, HeaderValue::from_static(BROWSER_UA));
        h.insert(ORIGIN, HeaderValue::from_static(APP_ORIGIN));
        h.insert(
            REFERER,
            HeaderValue::from_static("https://app.traderepublic.com/"),
        );
        h.insert(ACCEPT, HeaderValue::from_static("application/json"));
        h.insert(
            "Accept-Language",
            HeaderValue::from_static("de-DE,de;q=0.9"),
        );
        let c = reqwest::Client::builder()
            .cookie_provider(self.jar.clone())
            .default_headers(h)
            .timeout(std::time::Duration::from_secs(30))
            .redirect(redirect)
            .referer(false)
            .build()?;
        Ok(c)
    }

    /// Reads status, Set-Cookie headers and body of a response.
    async fn read(resp: reqwest::Response) -> (u16, Vec<String>, String) {
        let status = resp.status().as_u16();
        let cookies = resp
            .headers()
            .get_all(SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok().map(|s| s.to_string()))
            .collect();
        let body = super::read_text_limited(resp).await.unwrap_or_default();
        (status, cookies, body)
    }

    fn http_error(step: &str, status: u16, body: &str) -> CoreError {
        let hint = match status {
            403 | 405 => tr(
                " – AWS-WAF-Schutz von Trade Republic. Bitte „Im Browser-Fenster anmelden“ verwenden.",
            ),
            426 => tr(
                " – CLIENT_VERSION_OUTDATED: Trade Republic verlangt einen neueren Client. Bitte „Im Browser-Fenster anmelden“ verwenden.",
            ),
            401 => tr(" – PIN, Code oder Sitzung ungültig."),
            429 => tr(" – zu viele Versuche, bitte etwas warten."),
            _ => "",
        };
        CoreError::Bank {
            code: format!("HTTP {status}"),
            message: trf(
                "Trade Republic {0}{1} Antwort: {2}",
                &[&step, &hint, &snippet(body)],
            ),
        }
    }

    fn code_challenge(&mut self, message: String, countdown: Option<u32>) -> Phase {
        let c = Challenge {
            kind: ChallengeKind::Code,
            title: "Trade Republic 2FA".into(),
            message,
            method_name: tr("App-Code (oder SMS)").into(),
            redirect_url: None,
            image: None,
            expires_in_seconds: countdown,
            poll_interval_seconds: 0,
        };
        self.challenge = Some(c.clone());
        Phase::Challenge(c)
    }

    fn state(&mut self) -> Option<SecretJson> {
        let session = &mut self.session;
        session.saved_unix = jiff::Timestamp::now().as_second();
        let old = session.doc_items.len().saturating_sub(MAX_DOC_ITEMS);
        session.doc_items.drain(..old);
        SecretJson::encode(session).ok()
    }

    /// Tries to refresh a stored session. Ok(true) = usable.
    async fn refresh_session(&mut self) -> CoreResult<bool> {
        let client = self.client()?;
        let resp = client
            .get(format!("{API}/api/v1/auth/web/session"))
            .header("Cookie", self.session.cookies.expose())
            .send()
            .await?;
        let (status, set, _body) = Self::read(resp).await;
        if (200..300).contains(&status) {
            self.session.cookies = merge_cookies(self.session.cookies.expose(), &set).into();
            return Ok(true);
        }
        // 401/403: expired. Anything else (endpoint moved …): try the WebSocket anyway.
        Ok(!(status == 401 || status == 403))
    }
}

/// Browser login needed (no WAF token). `expired`: a stored session was just
/// refused by Trade Republic.
fn browser_login_message(expired: bool) -> &'static str {
    if expired {
        tr(
            "Die Trade-Republic-Sitzung ist abgelaufen. Bitte im Browser-Fenster neu anmelden – die neue Sitzung wird danach gespeichert.",
        )
    } else {
        tr(
            "Trade Republic schützt die Anmeldung mit AWS WAF. Bitte im Browser-Fenster anmelden – die Sitzung wird danach gespeichert und wiederverwendet.",
        )
    }
}

/// PIN needed for a direct login. `expired`: as above.
fn pin_prompt_message(expired: bool) -> &'static str {
    if expired {
        tr(
            "Die Trade-Republic-Sitzung ist abgelaufen. Bitte die 4-stellige PIN eingeben (oder „Im Browser-Fenster anmelden“).",
        )
    } else {
        tr("Bitte die 4-stellige Trade-Republic-PIN eingeben (oder „Im Browser-Fenster anmelden“).")
    }
}

impl Default for TradeRepublicConnector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl BankConnector for TradeRepublicConnector {
    async fn begin(&mut self, ctx: &ConnectContext) -> CoreResult<Phase> {
        self.session = ctx.state.decode().unwrap_or_default();
        self.session.phone = normalize_phone(&ctx.login_name);
        // 1) Stored browser/API session (tr_session) -> no PIN, no 2FA.
        let had_session = self.session.cookies.expose().contains("tr_session=");
        if had_session && self.refresh_session().await? {
            return Ok(Phase::Ready);
        }
        // A stored session that Trade Republic no longer accepts: say so, so
        // the PIN / browser prompt below doesn't look like a new problem.
        let expired = had_session;
        // 2) Direct login only with a WAF token harvested by the browser
        //    (aws-waf-token cookie); a bare request gets HTTP 405 from AWS WAF.
        let waf = cookie_value(self.session.cookies.expose(), "aws-waf-token");
        if waf.is_none() {
            return Err(CoreError::BrowserLoginRequired {
                message: browser_login_message(expired).into(),
            });
        }
        if self.session.phone.len() < 8 {
            return Err(CoreError::InvalidInput {
                message: trf(
                    "Telefonnummer fehlt oder ist ungültig („{0}“). Bitte z. B. +4915112345678 eintragen.",
                    &[&ctx.login_name],
                ),
            });
        }
        if ctx.secret.is_empty() {
            return Err(CoreError::Authentication {
                message: pin_prompt_message(expired).into(),
            });
        }
        let client = self.client()?;
        let mut last = (0u16, String::new());
        for version in ["v1", "v2"] {
            let resp = client
                .post(format!("{API}/api/{version}/auth/web/login"))
                .header("x-aws-waf-token", waf.clone().unwrap_or_default())
                .header("Cookie", self.session.cookies.expose())
                .json(&LoginRequest {
                    phone_number: &self.session.phone,
                    pin: ctx.secret.expose(),
                })
                .send()
                .await?;
            let (status, set, text) = Self::read(resp).await;
            self.session.cookies = merge_cookies(self.session.cookies.expose(), &set).into();
            if (200..300).contains(&status) {
                let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                self.process_id = v["processId"].as_str().unwrap_or_default().to_string();
                if self.process_id.is_empty() {
                    return Err(Self::http_error(
                        tr("Login (keine processId)"),
                        status,
                        &text,
                    ));
                }
                let countdown = v["countdownInSeconds"].as_u64().map(|s| s as u32);
                return Ok(self.code_challenge(
                    tr("Geben Sie den 4-stelligen Code ein, der in der Trade-Republic-App angezeigt wird. Keine Push-Nachricht erhalten? „Code per SMS“ wählen.").into(),
                    countdown,
                ));
            }
            last = (status, text);
            if !(status == 404 || status == 426) {
                break; // v2 only as fallback for "not found" / "outdated client"
            }
        }
        if matches!(last.0, 403 | 405) {
            return Err(CoreError::BrowserLoginRequired {
                message: trf(
                    "AWS WAF hat den Login abgelehnt (HTTP {0}). Bitte im Browser-Fenster anmelden.",
                    &[&last.0],
                ),
            });
        }
        Err(Self::http_error("Login", last.0, &last.1))
    }

    async fn respond(
        &mut self,
        _ctx: &ConnectContext,
        response: ChallengeResponse,
    ) -> CoreResult<Phase> {
        let ChallengeResponse::Code(code) = response else {
            return Err(CoreError::InvalidInput {
                message: tr("Trade Republic benötigt den 4-stelligen Code").into(),
            });
        };
        let client = self.client()?;
        let code = code.expose().trim().to_string();
        if code.eq_ignore_ascii_case("sms") {
            let resp = client
                .post(format!(
                    "{API}/api/v1/auth/web/login/{}/sms",
                    self.process_id
                ))
                .send()
                .await?;
            let (status, _, text) = Self::read(resp).await;
            if !(200..300).contains(&status) {
                return Err(Self::http_error(tr("SMS-Anforderung"), status, &text));
            }
            return Ok(self.code_challenge(
                tr("Der Code wurde per SMS gesendet. Bitte die 4 Ziffern eingeben.").into(),
                None,
            ));
        }
        if !(code.len() == 4 && code.chars().all(|c| c.is_ascii_digit())) {
            return Err(CoreError::InvalidInput {
                message: tr("Der Code muss aus 4 Ziffern bestehen.").into(),
            });
        }
        let resp = client
            .post(format!(
                "{API}/api/v1/auth/web/login/{}/{}",
                self.process_id, code
            ))
            .send()
            .await?;
        let (status, set, text) = Self::read(resp).await;
        if !(200..300).contains(&status) {
            return Err(Self::http_error(tr("2FA-Bestätigung"), status, &text));
        }
        // HTTP 200 with an EMPTY body (or {}) is normal: the session comes as Set-Cookie.
        self.session.cookies = merge_cookies(self.session.cookies.expose(), &set).into();
        if let Ok(v) = serde_json::from_str::<Value>(&text)
            && let Some(t) = v["sessionToken"].as_str()
        {
            self.session.session_token = t.into();
        }
        if self.session.cookies.is_empty() {
            let url: url::Url = API.parse().map_err(|e| CoreError::Protocol {
                message: format!("{e}"),
            })?;
            if let Some(h) = self.jar.cookies(&url) {
                self.session.cookies = h.to_str().unwrap_or_default().into();
            }
        }
        if !self.session.cookies.expose().contains("tr_session")
            && self.session.session_token.is_empty()
        {
            return Err(CoreError::Authentication {
                message: trf(
                    "Trade Republic hat keine Sitzung (tr_session) gesetzt. HTTP {0}, Antwort: {1}",
                    &[&status, &snippet(&text)],
                ),
            });
        }
        Ok(Phase::Ready)
    }

    async fn fetch(&mut self, ctx: &ConnectContext) -> CoreResult<FetchResult> {
        let mut req = WS.into_client_request().map_err(Self::ws_err)?;
        {
            let h = req.headers_mut();
            if !self.session.cookies.is_empty() {
                h.insert(
                    "Cookie",
                    HeaderValue::from_str(self.session.cookies.expose()).map_err(Self::ws_err)?,
                );
            }
            h.insert("Origin", HeaderValue::from_static(APP_ORIGIN));
            h.insert("User-Agent", HeaderValue::from_static(BROWSER_UA));
        }
        let (mut ws, _) = tokio_tungstenite::connect_async(req).await.map_err(|e| {
            let detail = match &e {
                tokio_tungstenite::tungstenite::Error::Http(resp) => format!(
                    "HTTP {} {}",
                    resp.status(),
                    resp.body().as_ref().map(|b| snippet(&crate::text::decode_bank_bytes(b))).unwrap_or_default()
                ),
                other => other.to_string(),
            };
            if detail.starts_with("HTTP 401") || detail.starts_with("HTTP 403") {
                return CoreError::BrowserLoginRequired { message: trf("Trade-Republic-Sitzung abgelaufen ({0}). Bitte im Browser-Fenster neu anmelden.", &[&detail]) };
            }
            CoreError::Network { message: trf("WebSocket-Verbindung abgelehnt: {0}", &[&detail]) }
        })?;
        let hello = json!({"locale": "de", "platformId": "webtrading", "platformVersion": "safari - 18.0.0", "clientId": "app.traderepublic.com", "clientVersion": "1.2.0"});
        ws.send(Message::text(format!("connect 31 {hello}")))
            .await
            .map_err(Self::ws_err)?;
        // wait for "connected"
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            let msg = tokio::time::timeout_at(deadline, ws.next())
                .await
                .map_err(|_| CoreError::Network {
                    message: tr(
                        "Trade Republic hat „connect 31“ nicht bestätigt (Zeitüberschreitung).",
                    )
                    .into(),
                })?
                .ok_or_else(|| CoreError::Network {
                    message: tr(
                        "WebSocket wurde vor „connected“ geschlossen – Sitzung abgelaufen?",
                    )
                    .into(),
                })?
                .map_err(Self::ws_err)?;
            if let Message::Text(t) = msg {
                let t = t.to_string();
                if t.trim() == "connected" {
                    break;
                }
                return Err(CoreError::Bank {
                    code: "WS".into(),
                    message: trf("Unerwartete Antwort auf connect: {0}", &[&snippet(&t)]),
                });
            }
        }

        let session_token = self.session.session_token.clone();
        let token = session_token.expose();
        let mut out = FetchResult::default();
        let cash = match request(&mut ws, 1, json!({"type": "cash"}), token).await {
            Ok(v) => v,
            Err(_) => request(&mut ws, 3, json!({"type": "availableCash"}), token)
                .await
                .unwrap_or(Value::Null),
        };
        let items: Vec<Value> = match &cash {
            Value::Array(a) => a.clone(),
            Value::Object(_) => vec![cash.clone()],
            _ => vec![],
        };
        let eur = items
            .iter()
            .find(|c| c["currencyId"].as_str().unwrap_or("EUR") == "EUR")
            .cloned()
            .unwrap_or(Value::Null);
        let balance = if eur["amount"].is_null() {
            amount_cents(&eur["value"])
        } else {
            amount_cents(&eur["amount"])
        };
        let iban = ["iban", "accountNumber"]
            .iter()
            .find_map(|k| {
                eur[*k]
                    .as_str()
                    .filter(|s| s.len() > 10)
                    .map(|s| s.to_string())
            })
            .unwrap_or_default();
        out.accounts.push(RemoteAccount {
            external_ref: "tr:cash".into(),
            name: "Verrechnungskonto".into(),
            owner: String::new(),
            iban,
            account_number: self.session.phone.clone(),
            currency: "EUR".into(),
            kind: AccountKind::Checking,
            balance_cents: balance,
            balance_date: super::today(),
        });

        let mut cursor: Option<String> = None;
        let mut sub_id = 2u32;
        let mut doc_candidates: Vec<(String, String, String)> = Vec::new();
        let mut trade_rows: Vec<TradeRow> = Vec::new();
        // After document retrieval was switched on again: reach back to the day it was switched off.
        let cutoff = ctx
            .documents
            .catchup_since()
            .map_or(ctx.since.as_str(), |d| d.min(&ctx.since))
            .to_string();
        'pages: for _ in 0..200 {
            let mut payload = json!({"type": "timelineTransactions"});
            if let Some(c) = &cursor {
                payload["after"] = json!(c);
            }
            let page = request(&mut ws, sub_id, payload, token).await?;
            sub_id += 2;
            for item in page["items"].as_array().cloned().unwrap_or_default() {
                let date: String = item["timestamp"]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(10)
                    .collect();
                if !date.is_empty() && date.as_str() < cutoff.as_str() {
                    break 'pages;
                }
                if item["amount"].is_null() || item["status"].as_str() == Some("CANCELED") {
                    continue;
                }
                let subtitle = item["subtitle"].as_str().unwrap_or_default().to_string();
                let item_id = item["id"].as_str().unwrap_or_default().to_string();
                let kind_text = format!(
                    "{} {} {}",
                    subtitle,
                    item["eventType"].as_str().unwrap_or(""),
                    item["title"].as_str().unwrap_or("")
                )
                .to_lowercase();
                if !item_id.is_empty()
                    && !self.session.doc_items.contains(&item_id)
                    && [
                        "sparplan",
                        "savings_plan",
                        "kauf",
                        "verkauf",
                        "order",
                        "trade",
                        "zinsen",
                        "interest",
                        "dividende",
                        "dividend",
                        "steuer",
                        "tax",
                        "saveback",
                        "round up",
                        "spare_change",
                        "ausschüttung",
                        "corporate_action",
                    ]
                    .iter()
                    .any(|k| kind_text.contains(k))
                {
                    doc_candidates.push((
                        item_id.clone(),
                        date.clone(),
                        item["title"].as_str().unwrap_or_default().to_string(),
                    ));
                }
                if !item_id.is_empty() && is_trade_kind(&kind_text) {
                    trade_rows.push(TradeRow {
                        id: item_id.clone(),
                        date: date.clone(),
                        title: item["title"].as_str().unwrap_or_default().to_string(),
                        kind_text: kind_text.clone(),
                        amount_cents: amount_cents(&item["amount"]["value"]),
                        currency: item["amount"]["currency"]
                            .as_str()
                            .unwrap_or("EUR")
                            .to_string(),
                        icon: item["icon"].as_str().unwrap_or_default().to_string(),
                    });
                }
                out.transactions.push(RemoteTransaction {
                    account_ref: "tr:cash".into(),
                    external_id: item["id"].as_str().unwrap_or_default().to_string(),
                    booking_date: date.clone(),
                    value_date: date,
                    amount_cents: amount_cents(&item["amount"]["value"]),
                    currency: item["amount"]["currency"]
                        .as_str()
                        .unwrap_or("EUR")
                        .to_string(),
                    counterparty: item["title"].as_str().unwrap_or_default().to_string(),
                    counterparty_iban: String::new(),
                    purpose: subtitle.clone(),
                    booking_text: if subtitle.is_empty() {
                        item["eventType"].as_str().unwrap_or_default().to_string()
                    } else {
                        subtitle
                    },
                    ..Default::default()
                });
            }
            cursor = page["cursors"]["after"].as_str().map(|s| s.to_string());
            if cursor.is_none() {
                break;
            }
        }
        // Document retrieval switched off: no detail screens for documents, nothing remembered
        // (the securities page below fetches the trade screens it needs itself).
        if !ctx.documents.enabled() {
            doc_candidates.clear();
        }
        // --- Beleg-Abruf: timelineDetailV2 -> "Dokumente" section -> PDF download
        let client = self.document_client()?;
        let mut details: std::collections::HashMap<String, Value> = Default::default();
        // Documents not looked at (more than one pass takes, a failed request): a catch-up goes on.
        let mut docs_incomplete = doc_candidates.len() > 40;
        for (item_id, date, title) in doc_candidates.into_iter().take(40) {
            sub_id += 2;
            let Ok(detail) = request(
                &mut ws,
                sub_id,
                json!({"type": "timelineDetailV2", "id": item_id}),
                token,
            )
            .await
            else {
                docs_incomplete = true;
                continue;
            };
            details.insert(item_id.clone(), detail.clone());
            for section in detail["sections"].as_array().cloned().unwrap_or_default() {
                let is_docs = section["type"].as_str() == Some("documents")
                    || section["title"]
                        .as_str()
                        .map(|t| t.contains("Dokument"))
                        .unwrap_or(false);
                if !is_docs {
                    continue;
                }
                for d in section["data"].as_array().cloned().unwrap_or_default() {
                    let url = d["action"]["payload"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    let Ok(parsed) = reqwest::Url::parse(&url) else {
                        continue;
                    };
                    if parsed.scheme() != "https" {
                        continue;
                    }
                    // Only send the session cookie to Trade Republic hosts; presigned
                    // CDN/S3 URLs get a cookie-free request. (reqwest also strips the
                    // Cookie header on cross-host redirects.)
                    let mut req = client.get(parsed.clone());
                    if is_trade_republic_host(&parsed) {
                        req = req.header("Cookie", self.session.cookies.expose());
                    }
                    let Ok(resp) = req.send().await else {
                        docs_incomplete = true;
                        continue;
                    };
                    if !resp.status().is_success() {
                        docs_incomplete = true;
                        continue;
                    }
                    let Ok(bytes) = super::read_body_limited(resp, super::MAX_DOC_BYTES).await
                    else {
                        docs_incomplete = true;
                        continue;
                    };
                    if !bytes.starts_with(b"%PDF") {
                        continue;
                    }
                    let doc_title = d["title"].as_str().unwrap_or("Dokument").to_string();
                    let lower = doc_title.to_lowercase();
                    let doc_type = if lower.contains("steuer") {
                        "steuer"
                    } else if lower.contains("abrechnung") || lower.contains("bestätigung") {
                        "abrechnung"
                    } else {
                        "beleg"
                    };
                    let doc_date = d["detail"]
                        .as_str()
                        .and_then(|s| crate::documents::dates_in(s).into_iter().next())
                        .unwrap_or_else(|| date.clone());
                    // strip the presigned query part for a stable de-dup key
                    let stable = url.split('?').next().unwrap_or(&url).to_string();
                    out.documents.push(crate::documents::RemoteDocument {
                        account_ref: "tr:cash".into(),
                        booking_external_id: Some(item_id.clone()),
                        title: format!("{doc_title} – {title}"),
                        doc_type: doc_type.into(),
                        mime_type: "application/pdf".into(),
                        file_name: format!("{}_{}.pdf", doc_date, doc_title.replace(' ', "_")),
                        bytes: bytes.to_vec(),
                        source_url: stable,
                        date: doc_date,
                    });
                }
            }
            self.session.doc_items.push(item_id);
        }
        out.documents_incomplete = docs_incomplete;
        // --- Wertpapierseite (#314): units, price, fees and taxes of the newest
        // trades from their detail screens (cached from the document pass).
        // A trade whose screen fails is skipped; the cash booking stays.
        for row in trade_rows.iter().take(100) {
            let detail = match details.remove(&row.id) {
                Some(d) => d,
                None => {
                    sub_id += 2;
                    match request(
                        &mut ws,
                        sub_id,
                        json!({"type": "timelineDetailV2", "id": row.id}),
                        token,
                    )
                    .await
                    {
                        Ok(d) => d,
                        Err(_) => continue,
                    }
                }
            };
            if let Some(t) = trade_security(row, &detail) {
                out.security_transactions.push(t);
            }
        }

        // --- Depotpositionen: portfolio, then instrument data and a quote per position.
        // Names of the trade rows fill in where the portfolio and the instrument have none.
        let mut trade_names: std::collections::HashMap<String, String> = Default::default();
        for row in &trade_rows {
            if let Some(isin) = isin_of(&row.icon).filter(|_| !row.title.is_empty()) {
                trade_names.entry(isin).or_insert_with(|| row.title.clone());
            }
        }
        let portfolio = fetch_portfolio(&mut ws, &mut sub_id, token, &trade_names).await;
        let has_depot = portfolio.as_ref().is_some_and(|h| !h.is_empty())
            || !out.security_transactions.is_empty()
            || ctx.since_by_account.contains_key(DEPOT_REF);
        if has_depot {
            let balance_cents = match portfolio {
                Some(holdings) => {
                    let total = holdings.iter().map(|h| h.holding.value_cents).sum();
                    out.holdings = holdings;
                    out.holdings_fetched.push(DEPOT_REF.into());
                    total
                }
                None => {
                    // not read: the stored positions and the depot value stay
                    out.balances_unknown.push(DEPOT_REF.into());
                    0
                }
            };
            out.accounts.push(RemoteAccount {
                external_ref: DEPOT_REF.into(),
                name: "Depot".into(),
                owner: String::new(),
                iban: String::new(),
                // no phone number here: it would show up as an account number
                account_number: String::new(),
                currency: "EUR".into(),
                kind: AccountKind::Brokerage,
                balance_cents,
                balance_date: super::today(),
            });
        }
        let _ = ws.close(None).await;
        out.state = self.state();
        Ok(out)
    }
}

/// True if the URL's host is `traderepublic.com` or a subdomain of it.
fn is_trade_republic_host(url: &reqwest::Url) -> bool {
    url.host_str()
        .map(|h| {
            let h = h.to_ascii_lowercase();
            h == "traderepublic.com" || h.ends_with(".traderepublic.com")
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_session_keeps_its_json_shape() {
        let stored = json!({
            "cookies": "tr_session=abc", "session_token": "st", "phone": "+491701234567",
            "saved_unix": 7, "doc_items": ["a", "b"]
        });
        let session: StoredSession = SecretJson::from(stored.clone()).decode().unwrap();
        assert_eq!(session.cookies.expose(), "tr_session=abc");
        assert_eq!(serde_json::to_value(&session).unwrap(), stored);
    }

    #[test]
    fn login_request_uses_the_api_field_names() {
        let body = LoginRequest {
            phone_number: "+491701234567",
            pin: "1234",
        };
        assert_eq!(
            serde_json::to_value(&body).unwrap(),
            json!({"phoneNumber": "+491701234567", "pin": "1234"})
        );
    }

    #[test]
    fn saved_state_keeps_the_newest_doc_items_in_order() {
        let mut c = TradeRepublicConnector::new();
        c.session.doc_items = (0..=MAX_DOC_ITEMS).map(|i| i.to_string()).collect();
        let saved: StoredSession = c.state().unwrap().decode().unwrap();
        assert_eq!(saved.doc_items.len(), MAX_DOC_ITEMS);
        assert_eq!(saved.doc_items.first().unwrap(), "1");
        assert_eq!(saved.doc_items.last().unwrap(), &MAX_DOC_ITEMS.to_string());
    }

    #[tokio::test]
    async fn login_client_does_not_follow_redirects_to_another_origin() {
        let client = TradeRepublicConnector::new().client().unwrap();
        crate::connectors::tests::redirects::assert_no_cross_origin_replay(&client).await;
    }

    #[tokio::test]
    async fn document_client_follows_redirects_to_another_origin() {
        let client = TradeRepublicConnector::new().document_client().unwrap();
        crate::connectors::tests::redirects::assert_follows_cross_origin_get(&client).await;
    }

    fn detail(extra_rows: serde_json::Value) -> Value {
        json!({"sections": [
            {"type": "header", "data": {"icon": "logos/US0378331005/v2"}},
            {"title": "Übersicht", "type": "table", "data": [
                {"title": "Gebühr", "detail": {"text": "1,00 €"}},
                {"title": "Steuer", "detail": {"text": "0,50 €"}},
                {"title": "Transaktion", "detail": {"action": {"payload": {"sections": [
                    {"type": "table", "data": extra_rows}
                ]}}}}
            ]}
        ]})
    }

    fn row(kind: &str, amount: i64) -> TradeRow {
        TradeRow {
            id: "evt-1".into(),
            date: "2026-11-05".into(),
            title: "Apple".into(),
            kind_text: kind.into(),
            amount_cents: amount,
            currency: "EUR".into(),
            icon: String::new(),
        }
    }

    #[test]
    fn parses_decimal_texts() {
        assert_eq!(parse_decimal_text("1.234,56 €"), Some(1234.56));
        assert_eq!(parse_decimal_text("0,5"), Some(0.5));
        assert_eq!(parse_decimal_text("12.5"), Some(12.5));
        assert_eq!(parse_decimal_text("1.234"), Some(1234.0));
        assert_eq!(parse_decimal_text("1,234.50"), Some(1234.5));
        assert_eq!(parse_decimal_text("−3,00 €"), Some(-3.0));
        assert_eq!(parse_decimal_text("– €"), None);
        assert_eq!(parse_decimal_text(""), None);
    }

    #[test]
    fn reads_trade_details_and_builds_the_securities_side() {
        use crate::models::SecurityDirection;
        let d = detail(json!([
            {"title": "Aktien", "detail": {"text": "2,5"}},
            {"title": "Preis", "detail": {"text": "160,00 €"}}
        ]));
        let t = trade_security(&row("kauf apple", -40_150), &d).unwrap();
        assert_eq!(
            (
                t.account_ref.as_str(),
                t.external_id.as_str(),
                t.cash_external_id.as_str()
            ),
            ("tr:depot", "evt-1", "evt-1")
        );
        let x = &t.transaction;
        assert_eq!(x.isin, "US0378331005");
        assert_eq!(x.direction, SecurityDirection::Buy);
        assert_eq!(
            (x.quantity, x.price_cents, x.value_cents),
            (2.5, 16_000, 40_000)
        );
        assert_eq!((x.fees_cents, x.taxes_cents), (100, 50));
        // without a price the value is the cash amount minus fees and taxes
        let d2 = detail(json!([{"title": "Aktien", "detail": {"text": "2,5"}}]));
        let b = trade_security(&row("kauf apple", -40_150), &d2)
            .unwrap()
            .transaction;
        assert_eq!((b.value_cents, b.price_cents), (40_000, 16_000));
        // "Verkauf" is a sell although it contains "kauf"; sells add fees back
        let s = trade_security(&row("verkaufsorder", 39_850), &d2)
            .unwrap()
            .transaction;
        assert_eq!(s.direction, SecurityDirection::Sell);
        assert_eq!(s.value_cents, 40_000);
        let div = trade_security(&row("dividende", 1_000), &detail(json!([])))
            .unwrap()
            .transaction;
        assert_eq!(div.direction, SecurityDirection::Dividend);
        // no ISIN anywhere: not a securities transaction
        assert!(trade_security(&row("kauf", -100), &json!({"sections": []})).is_none());
        assert!(trade_security(&row("zinsen", 100), &d2).is_none());
    }

    #[test]
    fn isin_comes_from_the_icon_or_the_header() {
        assert_eq!(
            isin_of("logos/IE00B4L5Y983/v2").as_deref(),
            Some("IE00B4L5Y983")
        );
        assert_eq!(isin_of("DE0007164600").as_deref(), Some("DE0007164600"));
        assert_eq!(isin_of("logos/company/v2"), None);
        assert_eq!(
            parse_trade_detail("logos/DE0007164600/v2", &json!({}))
                .isin
                .as_deref(),
            Some("DE0007164600")
        );
        let d = json!({"sections": [{"action": {"type": "instrumentDetail", "payload": "IE00B4L5Y983"}}]});
        assert_eq!(
            parse_trade_detail("", &d).isin.as_deref(),
            Some("IE00B4L5Y983")
        );
    }

    #[test]
    fn expired_session_is_named_in_the_login_prompts() {
        assert!(pin_prompt_message(true).contains("abgelaufen"));
        assert!(browser_login_message(true).contains("abgelaufen"));
        // no stored session: the usual prompts, without "expired"
        assert!(!pin_prompt_message(false).contains("abgelaufen"));
        assert!(browser_login_message(false).contains("AWS WAF"));
    }

    #[test]
    fn cookie_host_check() {
        let p = |s: &str| reqwest::Url::parse(s).unwrap();
        assert!(is_trade_republic_host(&p(
            "https://api.traderepublic.com/x"
        )));
        assert!(is_trade_republic_host(&p(
            "https://a.b.traderepublic.com/x"
        )));
        assert!(!is_trade_republic_host(&p("https://evil.com/x")));
        assert!(!is_trade_republic_host(&p(
            "https://traderepublic.com.evil.com/x"
        )));
        assert!(!is_trade_republic_host(&p(
            "https://eviltraderepublic.com/x"
        )));
        assert!(!is_trade_republic_host(&p(
            "https://bucket.s3.amazonaws.com/x?traderepublic.com"
        )));
    }

    #[test]
    fn normalises_phone_numbers() {
        assert_eq!(normalize_phone("0171 1234567"), "+491711234567");
        assert_eq!(normalize_phone("0049171/1234567"), "+491711234567");
        assert_eq!(normalize_phone("17012345678"), "+4917012345678");
        assert_eq!(normalize_phone("017012345678"), "+4917012345678");
        assert_eq!(normalize_phone("+49 170 1234 5678"), "+4917012345678");
        assert_eq!(normalize_phone("4917012345678"), "+4917012345678");
        assert_eq!(normalize_phone(""), "");
    }

    #[test]
    fn merges_set_cookie_headers() {
        let c = merge_cookies(
            "tr_refresh=old; other=1",
            &[
                "tr_session=abc; Path=/; HttpOnly; Secure".into(),
                "tr_refresh=new; Path=/".into(),
            ],
        );
        assert_eq!(c, "other=1; tr_session=abc; tr_refresh=new");
    }

    #[test]
    fn float_amounts_round_to_cents() {
        assert_eq!(amount_cents(&json!(-4.040000000000001)), -4_04);
        assert_eq!(amount_cents(&json!(-5.78)), -5_78);
        assert_eq!(amount_cents(&json!(47.32)), 4732);
    }

    // --- Depot positions

    fn position(id: &str, category: &str, quantity: f64, buy_in: Option<f64>) -> RawPosition {
        RawPosition {
            isin: id.into(),
            name: None,
            category: category.into(),
            quantity,
            average_buy_in: buy_in,
        }
    }

    fn info(type_id: &str) -> InstrumentInfo {
        InstrumentInfo {
            type_id: type_id.into(),
            ..Default::default()
        }
    }

    fn quote(last: f64, pre: Option<f64>) -> Quote {
        Quote {
            last: Some(last),
            pre,
        }
    }

    #[test]
    fn parses_compact_portfolio_by_type() {
        let v = json!({
            "categories": [
                {"type": "stocksAndETFs", "positions": [
                    {"instrumentId": "US0378331005", "name": "Apple Inc.", "netSize": "10.5", "averageBuyIn": "175.50"},
                    {"isin": "IE00B4L5Y983", "netSize": 40.0, "averageBuyIn": 82.20},
                    {"isin": "DE0007164600", "netSize": "0"}
                ]},
                {"type": "crypto", "positions": [
                    {"instrumentId": "XF000BTC0017", "name": "Bitcoin", "netSize": "0.15", "averageBuyIn": "50000.00"}
                ]},
                {"type": "bonds", "positions": []}
            ]
        });
        let positions = portfolio_positions(&v).unwrap();
        assert_eq!(positions.len(), 3); // the sold-out position is no holding
        assert_eq!(positions[0].isin, "US0378331005");
        assert_eq!(positions[0].name.as_deref(), Some("Apple Inc."));
        assert_eq!(positions[0].category, "stocksAndETFs");
        assert_eq!(positions[0].quantity, 10.5);
        assert_eq!(positions[0].average_buy_in, Some(175.50));
        assert_eq!(positions[1].isin, "IE00B4L5Y983");
        assert_eq!(positions[1].quantity, 40.0);
        assert_eq!(positions[2].category, "crypto");
    }

    #[test]
    fn portfolio_layouts_and_what_is_no_portfolio() {
        let flat = json!({"positions": [
            {"instrumentId": "DE0007164600", "netSize": "5", "averageBuyIn": "140,00"}
        ]});
        let positions = portfolio_positions(&flat).unwrap();
        assert_eq!(positions[0].quantity, 5.0);
        assert_eq!(positions[0].average_buy_in, Some(140.0));
        // an empty depot is an answer …
        assert_eq!(
            portfolio_positions(&json!({"categories": []})),
            Some(vec![])
        );
        // … an unknown layout is not: it must not empty the stored depot
        assert_eq!(portfolio_positions(&json!({"status": "ok"})), None);
        assert_eq!(portfolio_positions(&Value::Null), None);
    }

    #[test]
    fn numbers_as_text_keep_three_decimals() {
        assert_eq!(json_as_f64(&json!("40.745")), Some(40.745));
        assert_eq!(json_as_f64(&json!("18.000")), Some(18.0));
        assert_eq!(json_as_f64(&json!("73.256536")), Some(73.256536));
        assert_eq!(json_as_f64(&json!(40.745)), Some(40.745));
        assert_eq!(json_as_f64(&json!("1.234,56")), Some(1234.56));
        assert_eq!(json_as_f64(&json!("")), None);
    }

    #[test]
    fn securities_account_prefers_the_default_product() {
        let pairs = json!({"accounts": [
            {"productType": "CHILD", "securitiesAccountNumber": "222"},
            {"productType": "DEFAULT", "securitiesAccountNumber": "111"},
            {"productType": "DEFAULT"}
        ]});
        assert_eq!(securities_account_number(&pairs).as_deref(), Some("111"));
        assert_eq!(securities_account_number(&json!({"accounts": []})), None);
        assert_eq!(securities_account_number(&Value::Null), None);
    }

    #[test]
    fn holding_with_price_and_performance() {
        let mut raw = position("US0378331005", "stocksAndETFs", 10.0, Some(150.0));
        raw.name = Some("Apple".into());
        let mut inst = info("stock");
        inst.wkn = "865985".into();
        let rh = holding_of(&raw, &inst, None, quote(180.0, Some(175.0)));
        assert_eq!(rh.account_ref, "tr:depot");
        assert!(!rh.direct_crypto);
        let h = rh.holding;
        assert_eq!(h.isin, "US0378331005");
        assert_eq!(h.wkn, "865985");
        assert_eq!(h.name, "Apple");
        assert_eq!(h.purchase_price_cents, 15_000);
        assert_eq!(h.current_price_cents, 18_000);
        assert_eq!(h.value_cents, 180_000);
        assert_eq!(h.prev_day_price_cents, Some(17_500));
        assert_eq!(h.prev_day_value_cents, Some(175_000));
        assert_eq!(h.profit_loss_purchase_cents, Some(30_000));
        assert_eq!(h.profit_loss_purchase_rel, Some(20.0));
        assert_eq!(h.profit_loss_prev_day_cents, Some(5_000));
        assert_eq!(h.custody_type, "");
        assert_eq!(rh.classification.asset_class, Some(AssetClass::Equity));
        assert_eq!(
            rh.classification.instrument,
            Some(InstrumentKind::SingleSecurity)
        );
    }

    #[test]
    fn value_comes_from_the_unrounded_price() {
        // 73,256536 × 40,745 = 2.984,84 (rounding the price to 40,75 first gave 2.985,20)
        let raw = position("DE000A0F5UH1", "stocksAndETFs", 73.256536, Some(27.57));
        let h = holding_of(&raw, &info("fund"), None, quote(40.745, None)).holding;
        assert_eq!(h.current_price_cents, 4075);
        assert_eq!(h.value_cents, 298_484);
        assert_eq!(h.profit_loss_purchase_cents, Some(298_484 - 201_968));
        assert_eq!(h.prev_day_value_cents, None);
        assert_eq!(h.profit_loss_prev_day_cents, None);
    }

    #[test]
    fn bonds_are_quoted_in_percent_of_the_nominal() {
        // 1.000 nominal at 99,50 % is worth 995,00, not 99.500,00
        let raw = position("DE0001102580", "bonds", 1000.0, Some(98.5));
        let h = holding_of(&raw, &info("bond"), None, quote(99.5, Some(99.0))).holding;
        assert_eq!(h.value_cents, 99_500);
        assert_eq!(h.purchase_price_cents, 99); // 0,985 per unit, shown in cents
        assert_eq!(h.profit_loss_purchase_cents, Some(1_000));
        assert_eq!(h.prev_day_value_cents, Some(99_000));
        // a purchase price that already is per unit stays as it is
        let per_unit = position("DE0001102580", "bonds", 1000.0, Some(0.985));
        let h = holding_of(&per_unit, &info("bond"), None, quote(99.5, None)).holding;
        assert_eq!(h.profit_loss_purchase_cents, Some(1_000));
        // the category alone marks a bond when the instrument answer is missing
        let h = holding_of(&raw, &InstrumentInfo::default(), None, quote(99.5, None)).holding;
        assert_eq!(h.value_cents, 99_500);
    }

    #[test]
    fn without_a_quote_the_position_is_valued_at_cost_and_reports_no_gain() {
        let raw = position("US0378331005", "stocksAndETFs", 2.0, Some(150.0));
        let h = holding_of(&raw, &info("stock"), None, Quote::default()).holding;
        assert_eq!(h.value_cents, 30_000);
        assert_eq!(h.current_price_cents, 15_000);
        assert_eq!(h.profit_loss_purchase_cents, None);
        assert_eq!(h.profit_loss_purchase_rel, None);
        // neither quote nor purchase price: nothing to value
        let bare = position("US0378331005", "stocksAndETFs", 2.0, None);
        let h = holding_of(&bare, &info("stock"), None, Quote::default()).holding;
        assert_eq!((h.value_cents, h.purchase_price_cents), (0, 0));
    }

    #[test]
    fn name_falls_back_from_portfolio_to_instrument_to_trade_row() {
        let raw = position("US0378331005", "stocksAndETFs", 1.0, None);
        let mut inst = info("stock");
        let q = Quote::default();
        assert_eq!(
            holding_of(&raw, &inst, None, q).holding.name,
            "US0378331005"
        );
        assert_eq!(
            holding_of(&raw, &inst, Some("Apple (Kauf)"), q)
                .holding
                .name,
            "Apple (Kauf)"
        );
        inst.name = "Apple Inc.".into();
        assert_eq!(
            holding_of(&raw, &inst, Some("x"), q).holding.name,
            "Apple Inc."
        );
        inst.short_name = "Apple".into();
        assert_eq!(holding_of(&raw, &inst, Some("x"), q).holding.name, "Apple");
    }

    #[test]
    fn classification_follows_the_instrument_type_not_the_name() {
        let class = |category: &str, inst: &InstrumentInfo, name: &str| {
            let (c, crypto) = classify_tr_instrument(category, inst, name);
            (c.asset_class, c.instrument, crypto)
        };
        // "Netflix" contains "etf"
        assert_eq!(
            class("stocksAndETFs", &info("stock"), "Netflix"),
            (
                Some(AssetClass::Equity),
                Some(InstrumentKind::SingleSecurity),
                false
            )
        );
        // an ETF's asset class is its underlying class; unknown stays open for the name rules
        let mut etf = info("fund");
        etf.fund_category = "etf".into();
        assert_eq!(
            class("stocksAndETFs", &etf, "Core Euro Govt Bond"),
            (None, Some(InstrumentKind::Etf), false)
        );
        etf.underlying_class = "Bonds".into();
        assert_eq!(
            class("stocksAndETFs", &etf, "Core Euro Govt Bond"),
            (Some(AssetClass::Bond), Some(InstrumentKind::Etf), false)
        );
        // the labels Trade Republic sends
        etf.underlying_class = "Festverzinslich".into();
        assert_eq!(class("stocksAndETFs", &etf, "x").0, Some(AssetClass::Bond));
        etf.underlying_class = "Aktien".into();
        assert_eq!(
            class("stocksAndETFs", &etf, "x").0,
            Some(AssetClass::Equity)
        );
        etf.underlying_class = "equity".into();
        assert_eq!(
            class("stocksAndETFs", &etf, "MSCI World").0,
            Some(AssetClass::Equity)
        );
        assert_eq!(
            class("stocksAndETFs", &info("fund"), "Mischfonds").1,
            Some(InstrumentKind::Fund)
        );
        assert_eq!(
            class("stocksAndETFs", &info("fund"), "Physical Gold ETC"),
            (
                Some(AssetClass::Commodity),
                Some(InstrumentKind::EtcEtn),
                false
            )
        );
        assert_eq!(
            class("derivatives", &info("derivative"), "Call"),
            (None, Some(InstrumentKind::Derivative), false)
        );
        assert_eq!(
            class("bonds", &InstrumentInfo::default(), "Bund"),
            (
                Some(AssetClass::Bond),
                Some(InstrumentKind::SingleSecurity),
                false
            )
        );
        // nothing stated: nothing imported
        assert_eq!(
            class("stocksAndETFs", &InstrumentInfo::default(), "Netflix"),
            (None, None, false)
        );
    }

    #[test]
    fn direct_crypto_keeps_an_isin_and_moves_a_ticker_to_the_wkn() {
        let coin = position("BTC", "crypto", 0.5, Some(60_000.0));
        let rh = holding_of(&coin, &info("crypto"), None, quote(65_000.0, None));
        assert!(rh.direct_crypto);
        assert_eq!(rh.classification.asset_class, Some(AssetClass::Crypto));
        assert_eq!(
            rh.classification.instrument,
            Some(InstrumentKind::DirectHolding)
        );
        assert_eq!(
            (rh.holding.isin.as_str(), rh.holding.wkn.as_str()),
            ("", "BTC")
        );
        // the core recognises it as a direct holding with a stable key
        let class = crate::asset_classification::classify_asset(
            rh.holding.clone(),
            rh.classification.clone(),
            Default::default(),
            true,
        );
        assert_eq!(class.security_key.as_deref(), Some("crypto:BTC"));

        let with_isin = position("XF000BTC0017", "crypto", 0.5, None);
        let rh = holding_of(&with_isin, &info("crypto"), None, quote(65_000.0, None));
        assert_eq!(rh.holding.isin, "XF000BTC0017");
    }

    #[test]
    fn shape_shows_keys_and_types_but_no_values() {
        let v = json!({
            "categories": [{"type": "crypto", "positions": [
                {"isin": "US0378331005", "netSize": "10.5", "averageBuyIn": 175.5}
            ]}],
            "US0378331005": {"name": "Apple Inc."}
        });
        let s = shape(&v, 5);
        assert!(s.contains(
            "categories:[1x {positions:[1x {averageBuyIn:num,isin:str,netSize:str}],type:str}]"
        ));
        assert!(s.contains("<id>:{name:str}"));
        for secret in ["US0378331005", "Apple", "10.5", "175", "crypto"] {
            assert!(!s.contains(secret), "{secret} leaked into {s}");
        }
    }

    // --- Depot fetch against a scripted WebSocket

    use std::collections::VecDeque;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    enum Reply {
        Answer(Value),
        Refuse,
        /// No answer at all (the request runs into its timeout).
        Silent,
        /// The connection ends.
        Close,
    }

    struct FakeWs<F: FnMut(&Value) -> Reply + Unpin> {
        script: F,
        inbox: VecDeque<Message>,
        closed: bool,
        /// Request types in the order they were sent.
        sent: Vec<String>,
    }

    impl<F: FnMut(&Value) -> Reply + Unpin> FakeWs<F> {
        fn new(script: F) -> Self {
            Self {
                script,
                inbox: VecDeque::new(),
                closed: false,
                sent: Vec::new(),
            }
        }
    }

    impl<F: FnMut(&Value) -> Reply + Unpin> futures_util::Stream for FakeWs<F> {
        type Item = Result<Message, tokio_tungstenite::tungstenite::Error>;
        fn poll_next(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            match self.inbox.pop_front() {
                Some(msg) => Poll::Ready(Some(Ok(msg))),
                None if self.closed => Poll::Ready(None),
                // nothing will come: the request's timeout wakes the task
                None => Poll::Pending,
            }
        }
    }

    impl<F: FnMut(&Value) -> Reply + Unpin> futures_util::Sink<Message> for FakeWs<F> {
        type Error = std::io::Error;
        fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
        fn start_send(mut self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
            let text = item.to_text().unwrap_or_default().to_string();
            let mut parts = text.splitn(3, ' ');
            if parts.next() != Some("sub") {
                return Ok(());
            }
            let id = parts.next().unwrap_or_default().to_string();
            let payload: Value = serde_json::from_str(parts.next().unwrap_or("null")).unwrap();
            self.sent
                .push(payload["type"].as_str().unwrap_or_default().to_string());
            match (self.script)(&payload) {
                Reply::Answer(v) => self.inbox.push_back(Message::text(format!("{id} A {v}"))),
                Reply::Refuse => self.inbox.push_back(Message::text(format!("{id} E {{}}"))),
                Reply::Silent => {}
                Reply::Close => self.closed = true,
            }
            Ok(())
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
        fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    /// Two positions: a share with a quote and a fund.
    fn portfolio_answer() -> Value {
        json!({"categories": [{"type": "stocksAndETFs", "positions": [
            {"isin": "US0378331005", "netSize": "2", "averageBuyIn": "150.00"},
            {"isin": "IE00B4L5Y983", "netSize": "10", "averageBuyIn": "80.00"}
        ]}]})
    }

    /// A server that knows the account, the portfolio and every instrument and quote.
    fn healthy(payload: &Value) -> Reply {
        let id = payload["id"].as_str().unwrap_or_default();
        match payload["type"].as_str().unwrap_or_default() {
            "accountPairs" => Reply::Answer(json!({"accounts": [
                {"productType": "DEFAULT", "securitiesAccountNumber": "111"}
            ]})),
            "compactPortfolioByType" if payload["secAccNo"] == "111" => {
                Reply::Answer(portfolio_answer())
            }
            "instrument" if id == "US0378331005" => Reply::Answer(json!({
                "typeId": "stock", "shortName": "Apple", "wkn": "865985", "exchangeIds": ["LSX"]
            })),
            "instrument" => Reply::Answer(json!({
                "typeId": "fund", "shortName": "Core MSCI World", "exchangeIds": ["TDG"],
                "fundInfo": {"category": "etf", "underlyingClass": "equity"}
            })),
            "ticker" if id == "US0378331005.LSX" => {
                Reply::Answer(json!({"last": {"price": "180.00"}, "pre": {"price": "175.00"}}))
            }
            "ticker" if id == "IE00B4L5Y983.TDG" => {
                Reply::Answer(json!({"last": {"price": "90.555"}}))
            }
            _ => Reply::Refuse,
        }
    }

    async fn run<F: FnMut(&Value) -> Reply + Unpin>(
        script: F,
    ) -> (Option<Vec<RemoteHolding>>, Vec<String>) {
        let mut ws = FakeWs::new(script);
        let mut sub_id = 100;
        let names = std::collections::HashMap::new();
        let holdings = fetch_portfolio(&mut ws, &mut sub_id, "", &names).await;
        (holdings, ws.sent)
    }

    #[tokio::test(start_paused = true)]
    async fn depot_fetch_reads_positions_with_instrument_and_quote() {
        let (holdings, sent) = run(healthy).await;
        let holdings = holdings.unwrap();
        assert_eq!(
            sent,
            [
                "accountPairs",
                "compactPortfolioByType",
                "instrument",
                "ticker",
                "instrument",
                "ticker"
            ]
        );
        // largest value first: 10 × 90,555 = 905,55 before 2 × 180,00 = 360,00
        let fund = &holdings[0];
        assert_eq!(fund.holding.name, "Core MSCI World");
        assert_eq!(fund.holding.value_cents, 90_555);
        assert_eq!(fund.classification.instrument, Some(InstrumentKind::Etf));
        assert_eq!(fund.classification.asset_class, Some(AssetClass::Equity));
        let share = &holdings[1];
        assert_eq!(share.holding.wkn, "865985");
        assert_eq!(share.holding.value_cents, 36_000);
        assert_eq!(share.holding.profit_loss_prev_day_cents, Some(1_000));
    }

    #[tokio::test(start_paused = true)]
    async fn depot_fetch_falls_back_when_the_account_number_is_unknown_or_refused() {
        // no account pairs: the portfolio is asked without the number
        let (holdings, sent) = run(|p: &Value| match p["type"].as_str().unwrap() {
            "accountPairs" => Reply::Refuse,
            "compactPortfolioByType" if p["secAccNo"].is_null() => {
                Reply::Answer(portfolio_answer())
            }
            _ => healthy(p),
        })
        .await;
        assert_eq!(holdings.unwrap().len(), 2);
        assert_eq!(sent[..2], ["accountPairs", "compactPortfolioByType"]);

        // the current request refused twice: the old one still lists the positions
        let (holdings, sent) = run(|p: &Value| match p["type"].as_str().unwrap() {
            "compactPortfolioByType" => Reply::Refuse,
            "compactPortfolio" => Reply::Answer(json!({"positions": [
                {"instrumentId": "US0378331005", "netSize": "2", "averageBuyIn": "150.00"}
            ]})),
            _ => healthy(p),
        })
        .await;
        assert_eq!(holdings.unwrap().len(), 1);
        assert_eq!(
            sent[..4],
            [
                "accountPairs",
                "compactPortfolioByType",
                "compactPortfolioByType",
                "compactPortfolio"
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn depot_fetch_reports_an_empty_depot_only_from_the_current_request() {
        let (holdings, _) = run(|p: &Value| match p["type"].as_str().unwrap() {
            "compactPortfolioByType" => Reply::Answer(json!({"categories": []})),
            _ => healthy(p),
        })
        .await;
        assert_eq!(holdings.map(|h| h.len()), Some(0));

        // an unknown layout, then an empty answer of the old request: not a statement
        let (holdings, _) = run(|p: &Value| match p["type"].as_str().unwrap() {
            "compactPortfolioByType" => Reply::Answer(json!({"status": "ok"})),
            "compactPortfolio" => Reply::Answer(json!({"positions": []})),
            _ => healthy(p),
        })
        .await;
        assert!(holdings.is_none());

        // nothing answers at all
        let (holdings, _) = run(|p: &Value| match p["type"].as_str().unwrap() {
            "compactPortfolioByType" | "compactPortfolio" => Reply::Silent,
            _ => healthy(p),
        })
        .await;
        assert!(holdings.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_ticker_costs_its_timeout_and_values_the_position_at_cost() {
        let started = tokio::time::Instant::now();
        let (holdings, _) = run(|p: &Value| match p["type"].as_str().unwrap() {
            "ticker" if p["id"] == "US0378331005.LSX" => Reply::Silent,
            "instrument" if p["id"] == "IE00B4L5Y983" => Reply::Refuse,
            _ => healthy(p),
        })
        .await;
        assert_eq!(started.elapsed(), TICKER_WAIT);
        let holdings = holdings.unwrap();
        let share = holdings
            .iter()
            .find(|h| h.holding.isin == "US0378331005")
            .unwrap();
        assert_eq!(share.holding.value_cents, 30_000);
        assert_eq!(share.holding.profit_loss_purchase_cents, None);
        // the refused instrument: default exchange, no quote there, still listed
        let fund = holdings
            .iter()
            .find(|h| h.holding.isin == "IE00B4L5Y983")
            .unwrap();
        assert_eq!(fund.holding.value_cents, 80_000);
        assert_eq!(fund.classification, ClassificationValues::default());
    }

    #[tokio::test(start_paused = true)]
    async fn a_lost_connection_leaves_the_depot_untouched() {
        // at any step: no partial inventory with made-up prices
        for lost in [
            "accountPairs",
            "compactPortfolioByType",
            "instrument",
            "ticker",
        ] {
            let mut seen = 0;
            let (holdings, _) = run(|p: &Value| {
                if p["type"] == lost {
                    seen += 1;
                    // the second position's requests, where there are two
                    if seen == 2 || matches!(lost, "accountPairs" | "compactPortfolioByType") {
                        return Reply::Close;
                    }
                }
                healthy(p)
            })
            .await;
            assert!(holdings.is_none(), "connection lost during {lost}");
        }
    }
}
