//! Complete read snapshot for FNZ investment depots and cash accounts.
//! Keep the existing account IDs and document association from the first cut.
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde_json::Value;

use super::{field, invalid, is_currency_code, open_rows, overview};
use crate::{
    connectors::{store_fetch, FetchResult, RemoteHolding, RemoteTransaction, SyncSummary},
    error::{CoreError, CoreResult},
    models::Holding,
    vault::Vault,
};

#[cfg(test)]
mod tests;

fn incomplete() -> CoreError {
    invalid("FNZ: Unvollständige Konten-, Bestands- oder Umsatzdaten. Es wurden keine Daten übernommen.")
}

fn text<'a>(v: &'a Value, pointer: &str) -> CoreResult<&'a str> {
    field(v, pointer).map_err(|_| incomplete())
}

fn array<'a>(v: &'a Value, key: &str) -> CoreResult<&'a [Value]> {
    v[key].as_array().map(Vec::as_slice).ok_or_else(incomplete)
}

fn date(v: &Value, key: &str) -> CoreResult<String> {
    let s = text(v, key)?;
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|_| incomplete())?;
    Ok(s.into())
}

fn currency(v: &Value) -> CoreResult<&str> {
    let s = text(v, "/currencyId/id")?;
    if !is_currency_code(s) {
        return Err(incomplete());
    }
    Ok(s)
}

fn amount(v: &Value) -> CoreResult<i64> {
    if v["estimatedValueFlag"].as_bool() == Some(true) {
        return Err(incomplete());
    }
    crate::money::json_number_to_cents(&v["value"]).map_err(|_| incomplete())
}

fn decimal(v: &Value) -> CoreResult<f64> {
    let n = v
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .or_else(|| v.as_f64())
        .ok_or_else(incomplete)?;
    (n.is_finite() && n.abs() < 1e12)
        .then_some(n)
        .ok_or_else(incomplete)
}

/// Cash bookings that are still only an order. Their ID changes once the bank
/// books them, so store_fetch keeps such rows only while they are reported.
const ORDER_PREFIX: &str = "fnz:order:";

fn has_value(v: &Value) -> bool {
    match v {
        Value::String(s) => !s.trim().is_empty(),
        Value::Object(o) => o.values().any(has_value),
        _ => false,
    }
}

/// Internal account ID from the overview; a missing one would make the
/// per-booking account check compare null with null.
fn account_id(v: &Value) -> CoreResult<&Value> {
    has_value(v).then_some(v).ok_or_else(incomplete)
}

fn identifier(v: &Value, prefix: &str) -> CoreResult<String> {
    if !has_value(v) {
        return Err(incomplete());
    }
    // serde_json's sorted object keys make this independent of JSON key order.
    Ok(format!(
        "fnz:{prefix}:{}",
        URL_SAFE_NO_PAD.encode(v.to_string())
    ))
}

fn optional_text(v: &Value, pointer: &str) -> String {
    v.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .into()
}

fn holdings(
    data: &Value,
    account_ref: &str,
    account_currency: &str,
    balance: i64,
) -> CoreResult<Vec<RemoteHolding>> {
    if data["resourceIsUnavailable"].as_bool() == Some(true)
        || text(data, "/depot/externalDepotNumber/externalDepotNumber")? != account_ref
        || currency(&data["totalInventoryValueAccountCurrency"])? != account_currency
        || amount(&data["totalInventoryValueAccountCurrency"])? != balance
    {
        return Err(incomplete());
    }
    let mut result = Vec::new();
    let mut total = 0i64;
    for item in array(data, "investmentDepotInventories")? {
        let quantity = decimal(&item["pieces"]["value"])?;
        if quantity < 0.0
            || item["pieces"]["estimatedValueFlag"].as_bool() == Some(true)
            || currency(&item["inventory"])? != account_currency
        {
            return Err(incomplete());
        }
        let value = amount(&item["inventory"])?;
        total = total.checked_add(value).ok_or_else(incomplete)?;
        if quantity == 0.0 && value == 0 {
            continue;
        }
        let quote = &item["currentFundPrice"];
        if quote["estimatedValueFlag"].as_bool() == Some(true) {
            return Err(incomplete());
        }
        let mut price = decimal(&quote["value"])?;
        if currency(quote)? != account_currency {
            // The portal provides the current FX rate in an explicit direction.
            // Normalize a foreign-currency unit quote to the account currency.
            let fx = &item["currentExchangeRate"];
            let rate = decimal(&fx["value"])?;
            if rate <= 0.0 {
                return Err(incomplete());
            }
            let base = text(fx, "/baseCurrencyId")?;
            let target = text(fx, "/targetCurrencyId")?;
            if base == account_currency && target == currency(quote)? {
                price /= rate;
            } else if base == currency(quote)? && target == account_currency {
                price *= rate;
            } else {
                return Err(incomplete());
            }
        }
        if !price.is_finite() || !(0.0..1e12).contains(&price) {
            return Err(incomplete());
        }
        date(quote, "/fundPriceDate")?;
        let isin = text(item, "/fund/isin")?;
        if isin.len() != 12 || !isin.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(incomplete());
        }
        result.push(RemoteHolding {
            account_ref: account_ref.into(),
            holding: Holding {
                isin: isin.into(),
                wkn: optional_text(item, "/fund/wkn"),
                name: text(item, "/fund/fundName")?.into(),
                quantity,
                current_price_cents: (price * 100.0).round() as i64,
                // The observed fund inventory has no purchase-price field. Keep it
                // unknown rather than treating lifetime (possibly realized) gains
                // as unrealized gains or reconstructing an incomplete cost basis.
                purchase_price_cents: 0,
                value_cents: value,
            },
        });
    }
    if total != balance {
        return Err(incomplete());
    }
    Ok(result)
}

fn cash_transactions(
    detail: &Value,
    account_ref: &str,
    expected_id: &Value,
) -> CoreResult<Vec<RemoteTransaction>> {
    if &detail["cashAccountId"] != expected_id {
        return Err(incomplete());
    }
    array(detail, "transactions")?
        .iter()
        .map(|t| {
            if &t["cashAccountId"] != expected_id {
                return Err(incomplete());
            }
            let id = &t["orderOrTransactionId"];
            let transaction_id = &id["transactionId"];
            let order = transaction_id.is_null();
            if order {
                text(id, "/orderId/orderDate")?;
                text(id, "/orderId/orderNumber")?;
            } else {
                text(transaction_id, "/belegNummer")?;
                text(transaction_id, "/belegLfdNummer")?;
                // einzelgegenbuchung can be an empty discriminator; keep it in
                // the ID without requiring a nonempty value.
                if !transaction_id["einzelgegenbuchung"].is_string() {
                    return Err(incomplete());
                }
            }
            Ok(RemoteTransaction {
                account_ref: account_ref.into(),
                external_id: identifier(id, if order { "order" } else { "cash" })?,
                booking_date: date(t, "/bookingDate")?,
                value_date: date(t, "/valutaDate")?,
                amount_cents: amount(&t["amount"])?,
                currency: currency(&t["amount"])?.into(),
                counterparty: optional_text(t, "/countersideParticipant/accountHolder"),
                counterparty_iban: optional_text(t, "/countersideParticipant/iban/iban"),
                purpose: optional_text(t, "/reasonForPayment"),
                booking_text: text(t, "/transactionType")?.into(),
                end_to_end_id: optional_text(t, "/endToEndId"),
                mandate_id: optional_text(t, "/mandate/mandateReference"),
                creditor_id: optional_text(t, "/mandate/creditorIdentifier"),
                ..Default::default()
            })
        })
        .collect()
}

fn depot_transactions(
    detail: &Value,
    account_ref: &str,
    expected_id: &Value,
    account_currency: &str,
) -> CoreResult<Vec<RemoteTransaction>> {
    let mut result = Vec::new();
    for (key, historical) in [("transactions", false), ("historicalTransactions", true)] {
        for t in array(detail, key)? {
            if &t["depotId"] != expected_id {
                return Err(incomplete());
            }
            let id_key = if historical {
                "migratedAabTransactionId"
            } else {
                "orderTransactionId"
            };
            if !historical {
                date(&t[id_key], "/orderId/orderDate")?;
                text(&t[id_key], "/orderId/orderNumber")?;
                text(&t[id_key], "/orderTransactionKindId")?;
            }
            let (value, cur) = if t["paymentAmount"].is_null() {
                // Only explicitly neutral events may lack an amount; a missing
                // amount on a purchase/sale must not become a 0.00 booking.
                if text(t, "/numberSign")? != "0" {
                    return Err(incomplete());
                }
                // Neutral tax/dividend/fee information has no cash amount in
                // this overview. Retain it as a zero-value journal event.
                (0, account_currency)
            } else {
                (amount(&t["paymentAmount"])?, currency(&t["paymentAmount"])?)
            };
            let name = if historical {
                optional_text(t, "/shareName")
            } else {
                optional_text(t, "/fund/fundName")
            };
            let isin = if historical {
                let wrapped = optional_text(t, "/isin/isin");
                if wrapped.is_empty() {
                    optional_text(t, "/isin")
                } else {
                    wrapped
                }
            } else {
                optional_text(t, "/fund/isin")
            };
            let mut description = text(t, "/transactionTypeName")?.to_string();
            if t["isCancelationTransaction"].as_bool() == Some(true) {
                description.push_str(" (Storno)");
            } else if t["wasCanceled"].as_bool() == Some(true) {
                description.push_str(" (storniert)");
            }
            let purpose = [description.as_str(), name.as_str(), isin.as_str()]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" · ");
            result.push(RemoteTransaction {
                account_ref: account_ref.into(),
                external_id: identifier(
                    &t[id_key],
                    if historical { "historical" } else { "depot" },
                )?,
                booking_date: date(t, "/bookingDate")?,
                // fundPriceDate/priceDate is a pricing date, not a value date.
                value_date: String::new(),
                amount_cents: value,
                currency: cur.into(),
                counterparty: name,
                purpose,
                booking_text: description,
                ..Default::default()
            });
        }
    }
    Ok(result)
}

fn snapshot(json: &str) -> CoreResult<FetchResult> {
    let root: Value = serde_json::from_str(json).map_err(|_| incomplete())?;
    if root["version"].as_u64() != Some(1) {
        return Err(incomplete());
    }
    let mut result = overview(&root["overview"])?;
    let mut expected = HashMap::new();
    for (_, kind, row) in open_rows(&root["overview"])? {
        let account = &row["depotOrCashAccount"];
        expected.insert(
            text(account, "/externalDepotNumber/externalDepotNumber")?,
            (kind, account),
        );
    }
    let mut details = HashMap::new();
    for d in array(&root, "details")? {
        let key = text(d, "/accountRef")?;
        if !expected.contains_key(key) || details.insert(key, d).is_some() {
            return Err(incomplete());
        }
    }
    if details.len() != expected.len() {
        return Err(incomplete());
    }
    for account in &mut result.accounts {
        let (kind, remote) = expected
            .get(account.external_ref.as_str())
            .ok_or_else(incomplete)?;
        let detail = details[account.external_ref.as_str()];
        match (*kind, text(detail, "/kind")?) {
            ("cashAccounts", "cash") => {
                result.transactions.extend(cash_transactions(
                    detail,
                    &account.external_ref,
                    account_id(&remote["depotOrCashAccountId"]["subDepotId"])?,
                )?);
                account.iban = optional_text(detail, "/iban/iban");
            }
            ("investmentDepots", "investment") => {
                result.holdings.extend(holdings(
                    &detail["inventory"],
                    &account.external_ref,
                    &account.currency,
                    account.balance_cents,
                )?);
                result.holdings_fetched.push(account.external_ref.clone());
                result.transactions.extend(depot_transactions(
                    detail,
                    &account.external_ref,
                    account_id(&remote["depotOrCashAccountId"]["depotId"])?,
                    &account.currency,
                )?);
            }
            ("managedDepots" | "stockDepots" | "preciousMetalDepots", "totalsOnly") => {}
            _ => return Err(incomplete()),
        }
    }
    let mut seen = HashSet::new();
    for t in &result.transactions {
        if !seen.insert((&t.account_ref, &t.external_id)) {
            return Err(incomplete());
        }
    }
    result.hold_prefix = Some(ORDER_PREFIX.into());
    Ok(result)
}

#[uniffi::export]
impl Vault {
    /// Validate every requested account/position/booking before updating the vault.
    pub fn import_fnz_snapshot(
        self: Arc<Self>,
        connection_id: i64,
        snapshot_json: String,
    ) -> CoreResult<SyncSummary> {
        self.check_fnz(connection_id)?;
        let summary = store_fetch(
            &self,
            crate::models::ConnectorKind::Fnz,
            connection_id,
            snapshot(&snapshot_json)?,
        )?;
        let _ = self.checkpoint();
        Ok(summary)
    }
}
