//! Read-only employee plan snapshots from the authenticated embedded browser.
//! No credentials, cookies or anti-CSRF tokens cross the Swift/Core bridge.
use super::{FetchResult, RemoteAccount, RemoteHolding, SyncSummary};
use crate::i18n::tr;
use crate::{
    error::{CoreError, CoreResult},
    models::{AccountKind, ConnectorKind, Holding},
    vault::Vault,
};
use rusqlite::params;
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashSet},
    sync::Arc,
};

fn invalid() -> CoreError {
    CoreError::invalid(tr(
        "EquatePlus: Unvollständige oder nicht unterstützte Bestandsdaten. Es wurden keine Daten übernommen.",
    ))
}
fn label(v: &Value) -> Option<&str> {
    v.as_str()
        .or_else(|| v["displayValue"].as_str())
        .filter(|s| !s.trim().is_empty())
}
fn identifier(v: &Value) -> CoreResult<String> {
    label(v)
        .map(str::to_owned)
        .or_else(|| v.as_u64().map(|n| n.to_string()))
        .ok_or_else(invalid)
}
fn entries(v: &Value) -> CoreResult<&Vec<Value>> {
    v["entries"].as_array().ok_or_else(invalid)
}
fn amount(v: &Value) -> CoreResult<f64> {
    v["amount"]
        .as_f64()
        .filter(|n| n.is_finite() && *n >= 0.0 && *n < 1e10)
        .ok_or_else(invalid)
}
fn money(v: &Value, currency: &str) -> CoreResult<f64> {
    // Without FX data, never treat a foreign quote as reporting currency.
    if let Some(unit) = v["unit"]["code"]
        .as_str()
        .or(v["currency"]["code"].as_str())
        .or(v["currency"].as_str())
        && unit != currency
    {
        return Err(invalid());
    }
    amount(v)
}
fn cents(n: f64) -> CoreResult<i64> {
    let c = (n * 100.0).round();
    if !c.is_finite() || !(0.0..1e15).contains(&c) {
        return Err(invalid());
    }
    Ok(c as i64)
}

/// Explicit availability fields are disjoint. Generic total fields must not
/// be added to their components; ambiguous combinations fail before storage.
fn quantities(v: &Value, can_trade: Option<bool>) -> CoreResult<Vec<(f64, &'static str)>> {
    let first = |keys: &[&str]| -> CoreResult<Option<f64>> {
        let values: Vec<f64> = keys
            .iter()
            .filter(|k| !v[**k]["amount"].is_null())
            .map(|k| {
                v[*k]["amount"]
                    .as_f64()
                    .filter(|n| n.is_finite() && n.abs() < 1e10)
                    .ok_or_else(invalid)
            })
            .collect::<CoreResult<_>>()?;
        if values.windows(2).any(|w| (w[0] - w[1]).abs() > 1e-8) {
            return Err(invalid());
        }
        Ok(values.first().copied())
    };
    let available = first(&["QUANTITY", "AVAIL_QTY", "AVAILABLE_UNITS"])?;
    let locked = first(&["LOCKED_QTY"])?;
    let performance = first(&["LOCKED_PERF_QTY"])?;
    let total = first(&["TOTAL_UNITS", "UNITS", "NET_UNITS"])?;
    // A tradable instrument does not prove that every unit of a generic
    // total is available. Likewise, don't assume performance units are
    // disjoint from locked units without a reconciling total.
    if locked.is_some_and(|n| n > 0.0) && performance.is_some_and(|n| n > 0.0) && total.is_none() {
        return Err(invalid());
    }
    let known = available.unwrap_or(0.0) + locked.unwrap_or(0.0) + performance.unwrap_or(0.0);
    if available.is_none() && locked.is_none() && performance.is_none() {
        let q = total.ok_or_else(invalid)?;
        return Ok(vec![(
            q,
            match can_trade {
                Some(false) => "Gesperrt",
                _ => "Verfügbarkeit unbekannt",
            },
        )]);
    }
    if total.is_some_and(|n| (n - known).abs() > 1e-8) {
        return Err(invalid());
    }
    let mut result = Vec::new();
    if let Some(q) = available {
        result.push((
            q,
            match can_trade {
                Some(false) => "Gesperrt",
                Some(true) => "Verfügbar",
                None if locked.is_some()
                    || performance.is_some()
                    || !v["AVAIL_QTY"].is_null()
                    || !v["AVAILABLE_UNITS"].is_null() =>
                {
                    "Verfügbar"
                }
                None => "Verfügbarkeit unbekannt",
            },
        ));
    }
    if let Some(q) = locked {
        result.push((q, "Gesperrt"));
    }
    if let Some(q) = performance {
        result.push((q, "Gesperrt (Performance)"));
    }
    Ok(result)
}

/// Detail rows include historical credits/debits, not just current lots.
/// Net them by underlying instrument and availability before creating holdings.
struct Lot {
    name: String,
    state: &'static str,
    quantity: f64,
    price: f64,
    purchase: Option<f64>,
}

fn collect(
    node: &Value,
    price: Option<f64>,
    can_trade: Option<bool>,
    currency: &str,
    fallback_name: &str,
    security: Option<String>,
    out: &mut Vec<Lot>,
    depth: usize,
) -> CoreResult<()> {
    if depth > 12 {
        return Err(invalid());
    }
    let price = if node["marketPrice"].is_null() {
        price
    } else {
        Some(money(&node["marketPrice"], currency)?)
    };
    let can_trade = if can_trade == Some(false) {
        Some(false)
    } else {
        node["canTrade"].as_bool().or(can_trade)
    };
    let security = label(&node["vehicle"])
        .map(|name| {
            if label(&node["productType"]) == Some("RSU") {
                format!("{name} RSU")
            } else {
                name.to_owned()
            }
        })
        .or(security);
    if !node["entries"].is_null() && !entries(node)?.is_empty() {
        for child in entries(node)? {
            collect(
                child,
                price,
                can_trade,
                currency,
                fallback_name,
                security.clone(),
                out,
                depth + 1,
            )?;
        }
        return Ok(());
    }
    let has_quantity = [
        "QUANTITY",
        "AVAIL_QTY",
        "AVAILABLE_UNITS",
        "LOCKED_QTY",
        "LOCKED_PERF_QTY",
        "TOTAL_UNITS",
        "UNITS",
        "NET_UNITS",
    ]
    .iter()
    .any(|k| !node[*k]["amount"].is_null());
    if !has_quantity {
        // Empty security descriptions and contribution/purchase-history rows
        // are present even in empty programs. Program totals below prove that
        // skipping them did not omit a current holding.
        if node["entries"].as_array().is_some_and(Vec::is_empty)
            || [
                "VEHICLE",
                "VEHICLE_DESCRIPTION",
                "SECURITY",
                "VEHICLE_NAME",
                "PURCHASE_PRICE",
                "SELL_PURCHASE_PRICE",
            ]
            .iter()
            .any(|k| !node[*k].is_null())
        {
            return Ok(());
        }
        return Err(invalid());
    }
    let quantities = quantities(node, can_trade)?;
    if quantities.iter().all(|(q, _)| *q == 0.0) {
        return Ok(());
    }
    let name = security.unwrap_or_else(|| {
        let name = ["VEHICLE", "VEHICLE_DESCRIPTION", "SECURITY", "VEHICLE_NAME"]
            .iter()
            .find_map(|k| label(&node[*k]))
            .unwrap_or(fallback_name);
        if name == "Own SAP Dividend" {
            "Own SAP".into()
        } else {
            name.into()
        }
    });
    let purchase = ["SELL_PURCHASE_PRICE", "COST_BASIS", "PURCHASE_PRICE"]
        .iter()
        .find_map(|k| (!node[*k]["amount"].is_null()).then_some(&node[*k]))
        .map(|v| money(v, currency))
        .transpose()?;
    for (quantity, state) in quantities {
        if quantity != 0.0 {
            out.push(Lot {
                name: name.clone(),
                state,
                quantity,
                price: price.ok_or_else(invalid)?,
                purchase,
            });
        }
    }
    Ok(())
}

fn net_holdings(lots: Vec<Lot>, account_ref: &str) -> CoreResult<Vec<RemoteHolding>> {
    struct Net {
        quantity: f64,
        cost: Option<f64>,
    }
    let mut nets: BTreeMap<(String, &'static str, u64), Net> = BTreeMap::new();
    for lot in lots {
        let net = nets
            .entry((lot.name, lot.state, lot.price.to_bits()))
            .or_insert(Net {
                quantity: 0.0,
                cost: Some(0.0),
            });
        net.quantity += lot.quantity;
        // A historical disposal price is not the cost of the remaining shares.
        net.cost = if lot.quantity < 0.0 {
            None
        } else {
            net.cost
                .zip(lot.purchase)
                .map(|(cost, price)| cost + price * lot.quantity)
        };
    }
    let mut result = Vec::new();
    for ((name, state, bits), net) in nets {
        if net.quantity.abs() < 1e-8 {
            continue;
        }
        if net.quantity < 0.0 {
            return Err(invalid());
        }
        let price = f64::from_bits(bits);
        result.push(RemoteHolding {
            account_ref: account_ref.into(),
            holding: Holding {
                name: format!("{name} · {state}"),
                quantity: net.quantity,
                current_price_cents: cents(price)?,
                purchase_price_cents: net
                    .cost
                    .map(|c| cents(c / net.quantity))
                    .transpose()?
                    .unwrap_or(0),
                value_cents: cents(net.quantity * price)?,
                custody_type: state.into(),
                ..Default::default()
            },
            ..Default::default()
        });
    }
    Ok(result)
}

/// Reconcile the net ledger against the portal's current program totals. A
/// missing/unknown quantity column must never silently clear a nonempty plan.
fn reconcile(details: &Value, currency: &str, holdings: &[RemoteHolding]) -> CoreResult<i64> {
    let totals = details["totals"].as_array().ok_or_else(invalid)?;
    let mut seen = HashSet::new();
    let mut balance = None;
    for total in totals {
        let kind = label(&total["type"]).ok_or_else(invalid)?;
        if !seen.insert(kind) {
            return Err(invalid());
        }
        let expected_qty = amount(&total["quantity"])?;
        let expected_value = cents(money(&total["value"], currency)?)?;
        let included = |h: &&RemoteHolding| match kind {
            "TOTAL" => true,
            "AVAILABLE" => h.holding.custody_type == "Verfügbar",
            "LOCKED" => h.holding.custody_type.starts_with("Gesperrt"),
            _ => false,
        };
        if !["TOTAL", "AVAILABLE", "LOCKED"].contains(&kind) {
            continue;
        }
        let qty: f64 = holdings
            .iter()
            .filter(included)
            .map(|h| h.holding.quantity)
            .sum();
        let value = holdings.iter().filter(included).try_fold(0i64, |sum, h| {
            sum.checked_add(h.holding.value_cents).ok_or_else(invalid)
        })?;
        let rounding = i64::try_from(holdings.len().max(1)).map_err(|_| invalid())?;
        if (qty - expected_qty).abs() > 1e-7 || (value - expected_value).abs() > rounding {
            return Err(invalid());
        }
        if kind == "TOTAL" {
            balance = Some(expected_value);
        }
    }
    balance.ok_or_else(invalid)
}

fn snapshot(json: &str) -> CoreResult<FetchResult> {
    let v: Value = serde_json::from_str(json).map_err(|_| invalid())?;
    if v["version"].as_u64() != Some(1) {
        return Err(invalid());
    }
    let user = identifier(&v["user"]["participant"]["userId"])?;
    let currency = label(&v["user"]["reportingCurrency"]["code"]).ok_or_else(invalid)?;
    if currency.len() != 3 || !currency.bytes().all(|c| c.is_ascii_uppercase()) {
        return Err(invalid());
    }
    let date = label(&v["date"]).ok_or_else(invalid)?;
    jiff::civil::Date::strptime("%Y-%m-%d", date).map_err(|_| invalid())?;
    let plans = v["plans"].as_array().ok_or_else(invalid)?;
    // An empty overview may also be an expired session; don't clear a depot.
    if plans.is_empty() {
        return Err(invalid());
    }
    let mut seen = HashSet::new();
    let mut result = FetchResult {
        bank_name: Some("EquatePlus".into()),
        ..Default::default()
    };
    for plan in plans {
        let id = identifier(&plan["id"])?;
        if !seen.insert(id.clone()) {
            return Err(invalid());
        }
        let name = label(&plan["name"])
            .or_else(|| label(&plan["details"]["name"]))
            .unwrap_or(&id);
        let account_ref = format!("{}:{}:{}", user.len(), user, id);
        let mut lots = Vec::new();
        // Require the root array even for an explicitly empty plan.
        for node in entries(&plan["details"])? {
            collect(node, None, None, currency, name, None, &mut lots, 0)?;
        }
        let holdings = net_holdings(lots, &account_ref)?;
        let balance = reconcile(&plan["details"], currency, &holdings)?;
        result.holdings.extend(holdings);
        result.accounts.push(RemoteAccount {
            external_ref: account_ref.clone(),
            name: format!("EquatePlus · {name}"),
            owner: String::new(),
            iban: String::new(),
            account_number: format!("{user}/{id}"),
            currency: currency.into(),
            kind: AccountKind::Brokerage,
            balance_cents: balance,
            balance_date: date.into(),
        });
        result.holdings_fetched.push(account_ref);
    }
    Ok(result)
}

#[uniffi::export]
impl Vault {
    pub fn import_equateplus_snapshot(
        self: Arc<Self>,
        connection_id: i64,
        snapshot_json: String,
    ) -> CoreResult<SyncSummary> {
        if self.portal_connection(connection_id)?.0 != ConnectorKind::EquatePlus {
            return Err(invalid());
        }
        crate::banking_gate::check_connector("equateplus")?;
        let summary = super::store_fetch(
            &self,
            ConnectorKind::EquatePlus,
            connection_id,
            snapshot(&snapshot_json)?,
        )?;
        let _ = self.checkpoint();
        Ok(summary)
    }

    /// Only the documents of the portal's library that are not stored yet. The list may be long
    /// (one entry per statement, order confirmation, tax certificate); invalid entries are
    /// skipped, never stored half. Optional and on demand: nothing here is part of a normal fetch.
    pub fn pending_equateplus_documents(
        self: Arc<Self>,
        connection_id: i64,
        documents_json: String,
    ) -> CoreResult<Vec<String>> {
        self.check_equateplus(connection_id)?;
        let docs: Vec<Value> = serde_json::from_str(&documents_json).map_err(|_| invalid())?;
        let db = self.db()?;
        // a new run starts: nothing is matched yet
        self.equateplus_doc_claims
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(c, _), _| *c != connection_id);
        let mut groups: Vec<(String, Vec<LibraryDocument>)> = Vec::new();
        for doc in docs {
            let Some(mut parsed) = document(&doc) else {
                continue;
            };
            let group = document_group(&parsed.identity);
            parsed.key = Some(group.clone());
            match groups.iter_mut().find(|(g, _)| *g == group) {
                Some((_, list)) => list.push(parsed),
                None => groups.push((group, vec![parsed])),
            }
        }
        let mut pending = Vec::new();
        for (group, list) in groups {
            let stored: i64 = db.query_row(
                "SELECT count(*) FROM booking_documents WHERE source_url LIKE ?1",
                [group_slots_pattern(connection_id, &group)],
                |r| r.get(0),
            )?;
            // A group is complete when as many PDFs are stored as the portal lists. Otherwise all
            // of its entries are downloaded again (few, and small); the ones already stored match
            // their slot by content, so the order of the portal's list does not matter.
            if usize::try_from(stored).unwrap_or(0) < list.len() {
                pending.extend(list.iter().map(LibraryDocument::json));
            }
        }
        Ok(pending)
    }

    /// Stores one downloaded PDF. The program named in the document (if it matches exactly one
    /// program of this login) is its account; documents are not linked to bookings or activities.
    pub fn import_equateplus_document(
        self: Arc<Self>,
        connection_id: i64,
        document_json: String,
        bytes: Vec<u8>,
    ) -> CoreResult<bool> {
        self.check_equateplus(connection_id)?;
        if !has_pdf_header(&bytes) {
            return Err(invalid());
        }
        let doc: Value = serde_json::from_str(&document_json).map_err(|_| invalid())?;
        let parsed = document(&doc).ok_or_else(invalid)?;
        let group = doc["key"]
            .as_str()
            .filter(|k| valid_document_group(k))
            .ok_or_else(invalid)?
            .to_owned();
        let db = self.db()?;
        let account_id: Option<i64> = match &parsed.plan {
            Some(plan) => {
                let mut q = db.prepare(
                    "SELECT id FROM accounts WHERE connection_id=?1 AND deleted_ms IS NULL AND name=?2",
                )?;
                let ids: Vec<i64> = q
                    .query_map(
                        params![connection_id, format!("EquatePlus · {plan}")],
                        |r| r.get(0),
                    )?
                    .collect::<Result<_, _>>()?;
                (ids.len() == 1).then(|| ids[0])
            }
            None => None,
        };
        // which slot of the group is this PDF? The one whose stored content it is, if that slot
        // was not matched yet in this run; otherwise a free one.
        use sha2::{Digest, Sha256};
        let content = Sha256::digest(&bytes);
        let mut slots: Vec<(usize, Vec<u8>)> = Vec::new();
        {
            let mut q = db.prepare(
                "SELECT source_url, pdf_bytes FROM booking_documents WHERE source_url LIKE ?1",
            )?;
            let prefix = format!("equateplus:{connection_id}:{group}-");
            let rows = q.query_map([group_slots_pattern(connection_id, &group)], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
            })?;
            for row in rows {
                let (source, stored) = row?;
                if let Some(slot) = source
                    .strip_prefix(&prefix)
                    .and_then(|n| n.parse::<usize>().ok())
                {
                    slots.push((slot, Sha256::digest(&stored).to_vec()));
                }
            }
        }
        let mut claims = self
            .equateplus_doc_claims
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let claimed = claims.entry((connection_id, group.clone())).or_default();
        if let Some((slot, _)) = slots.iter().find(|(slot, hash)| {
            hash.as_slice() == content.as_slice()
                && !claimed.contains(&document_source(connection_id, &group, *slot))
        }) {
            claimed.insert(document_source(connection_id, &group, *slot));
            return Ok(false);
        }
        let used: HashSet<usize> = slots.iter().map(|(slot, _)| *slot).collect();
        let slot = (0..).find(|n| !used.contains(n)).unwrap_or(0);
        let source = document_source(connection_id, &group, slot);
        let stored = crate::documents::insert_doc(
            &db,
            None,
            account_id,
            &parsed.title,
            "postbox",
            "application/pdf",
            &format!("equateplus_{group}-{slot}.pdf"),
            &bytes,
            &source,
            &parsed.date,
        )?
        .is_some();
        claimed.insert(source);
        Ok(stored)
    }
}

impl Vault {
    fn check_equateplus(&self, connection_id: i64) -> CoreResult<()> {
        if self.portal_connection(connection_id)?.0 != ConnectorKind::EquatePlus {
            return Err(invalid());
        }
        crate::banking_gate::check_connector("equateplus")
    }
}

/// A PDF starts with `%PDF-`, but the format allows a few bytes in front (within the first 1024).
fn has_pdf_header(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(1024)]
        .windows(5)
        .any(|w| w == b"%PDF-")
}

fn document_source(connection_id: i64, group: &str, slot: usize) -> String {
    format!("equateplus:{connection_id}:{group}-{slot}")
}

/// A library entry reduced to what is stored. `url` must be one of the portal's two download
/// services with only a document id, so nothing else can be requested through it.
struct LibraryDocument {
    id: String,
    date: String,
    title: String,
    document_type: String,
    plan: Option<String>,
    url: String,
    /// What identifies the document across sign-ins: the portal's own ids are not stable, so the
    /// key is made of type, dates, description, program and language (plus a running number for
    /// identical entries, e.g. three order confirmations of one day). Set by the list step.
    key: Option<String>,
    /// The entry's identifying fields before hashing.
    identity: String,
}

impl LibraryDocument {
    fn json(&self) -> String {
        serde_json::json!({
            "id": self.id, "date": self.date, "description": self.title, "key": self.key,
            "documentType": self.document_type, "plan": self.plan, "url": self.url,
        })
        .to_string()
    }
}

/// The group of a document: a hash of its identity. Entries with the same identity (e.g. three
/// order confirmations of one day) share a group; which slot of the group a PDF takes is decided
/// by its content when it is stored, never by its position in the portal's list.
fn document_group(identity: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(&Sha256::digest(identity.as_bytes())[..16])
}

fn valid_document_group(key: &str) -> bool {
    key.len() == 32 && key.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `LIKE` pattern of all slots of a group (hex only, so nothing to escape).
fn group_slots_pattern(connection_id: i64, group: &str) -> String {
    format!("equateplus:{connection_id}:{group}-%")
}

fn document(v: &Value) -> Option<LibraryDocument> {
    let text = |k: &str| label(&v[k]).filter(|s| s.len() <= 300).map(str::to_owned);
    let id = text("id")?;
    let url = text("url")?;
    let service = [
        "/services/documents/download?",
        "/services/statements/download?",
    ]
    .iter()
    .find_map(|p| url.strip_prefix(p))?;
    let mut query = service.split('&');
    if !query.next()?.starts_with("documentId=") || query.next().is_some() {
        return None;
    }
    let date = text("date")?.chars().take(10).collect::<String>();
    jiff::civil::Date::strptime("%Y-%m-%d", &date).ok()?;
    let document_type = text("documentType").unwrap_or_default();
    let title = text("description")
        .filter(|d| !d.is_empty())
        .or_else(|| Some(document_type.clone()).filter(|t| !t.is_empty()))
        .unwrap_or_else(|| "EquatePlus".into());
    let plan = text("plan").filter(|p| !p.is_empty());
    let identity = [
        document_type.as_str(),
        text("date").unwrap_or_default().as_str(),
        text("addedDate").unwrap_or_default().as_str(),
        title.as_str(),
        plan.as_deref().unwrap_or(""),
        text("language").unwrap_or_default().as_str(),
    ]
    .join("\u{1f}");
    Some(LibraryDocument {
        id,
        date,
        title,
        document_type,
        plan,
        url,
        key: None,
        identity,
    })
}

pub struct WebOnlyConnector;
#[async_trait::async_trait]
impl super::BankConnector for WebOnlyConnector {
    async fn begin(&mut self, _ctx: &super::ConnectContext) -> CoreResult<super::Phase> {
        Err(CoreError::BrowserLoginRequired {
            message: tr(
                "Bitte bei EquatePlus im Browserfenster anmelden und die Bestände abrufen.",
            )
            .into(),
        })
    }
    async fn respond(
        &mut self,
        ctx: &super::ConnectContext,
        _response: super::ChallengeResponse,
    ) -> CoreResult<super::Phase> {
        self.begin(ctx).await
    }
    async fn fetch(&mut self, _ctx: &super::ConnectContext) -> CoreResult<FetchResult> {
        Err(CoreError::BrowserLoginRequired {
            message: tr(
                "Bitte bei EquatePlus im Browserfenster anmelden und die Bestände abrufen.",
            )
            .into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture() -> Value {
        json!({"version":1,"date":"2026-11-15","user":{"participant":{"userId":"test-user"},"reportingCurrency":{"code":"EUR"}},
          "plans":[{"id":"plan-a","name":"Test Programm","details":{"totals":[{"type":"TOTAL","quantity":{"amount":6.875},"value":{"amount":848.72}}],"entries":[{"entries":[{
            "marketPrice":{"amount":123.45,"unit":{"code":"EUR"}},"canTrade":true,
            "entries":[{"VEHICLE":"Test Aktien","QUANTITY":{"amount":2.5},"LOCKED_QTY":{"amount":3.25},"SELL_PURCHASE_PRICE":{"amount":100}},
                       {"VEHICLE":"Test RSU","LOCKED_PERF_QTY":{"amount":1.125}}]}]}]}}]})
    }
    /// Two programs of one login: plan A as in `fixture()`, plan B with `qty_b` shares of a
    /// second instrument at 50 EUR.
    fn two_plans(qty_b: f64) -> Value {
        let mut v = fixture();
        let value_b = qty_b * 50.0;
        v["plans"].as_array_mut().unwrap().push(json!({
            "id": "plan-b", "name": "Zweites Programm",
            "details": {"totals": [{"type":"TOTAL","quantity":{"amount":qty_b},"value":{"amount":value_b}}],
              "entries":[{"entries":[{"marketPrice":{"amount":50.0,"unit":{"code":"EUR"}},"canTrade":true,
                "entries":[{"VEHICLE":"Test Aktien B","QUANTITY":{"amount":qty_b}}]}]}]}}));
        v
    }

    fn open_vault(name: &str) -> (crate::test_support::TempDir, std::sync::Arc<Vault>, i64) {
        let dir = crate::test_support::TempDir::new(name);
        let vault =
            Vault::open(dir.join("vault.db").to_string_lossy().into(), vec![7; 32]).unwrap();
        let connection = vault
            .add_bank_connection(
                ConnectorKind::EquatePlus,
                "Test EquatePlus".into(),
                "test".into(),
            )
            .unwrap();
        (dir, vault, connection)
    }

    fn depot_quantity(vault: &Vault, name_part: &str) -> f64 {
        let depot = vault
            .list_accounts()
            .unwrap()
            .into_iter()
            .find(|a| a.name.contains(name_part))
            .unwrap();
        vault
            .list_holdings(depot.id)
            .unwrap()
            .iter()
            .map(|h| h.quantity)
            .sum()
    }

    #[test]
    fn every_program_becomes_its_own_depot_and_all_are_fetched_by_default() {
        let (_dir, vault, connection) = open_vault("equateplus-select-all");
        vault
            .clone()
            .import_equateplus_snapshot(connection, two_plans(4.0).to_string())
            .unwrap();
        let accounts = vault.list_accounts().unwrap();
        assert_eq!(accounts.len(), 2);
        assert!(accounts.iter().all(|a| a.kind == AccountKind::Brokerage));
        let choices = vault.connection_account_choices(connection).unwrap();
        assert_eq!(choices.len(), 2);
        assert!(choices.iter().all(|c| c.enabled && !c.pending));
        assert_eq!(depot_quantity(&vault, "Test Programm"), 6.875);
        assert_eq!(depot_quantity(&vault, "Zweites Programm"), 4.0);
    }

    #[test]
    fn a_program_can_be_switched_off_and_on_again() {
        let (_dir, vault, connection) = open_vault("equateplus-select-some");
        vault
            .clone()
            .import_equateplus_snapshot(connection, two_plans(4.0).to_string())
            .unwrap();
        let choices = vault.connection_account_choices(connection).unwrap();
        let ref_a = choices
            .iter()
            .find(|c| c.name.contains("Test Programm"))
            .unwrap()
            .account_ref
            .clone();
        let ref_b = choices
            .iter()
            .find(|c| c.name.contains("Zweites Programm"))
            .unwrap()
            .account_ref
            .clone();

        // switch plan B off: the next fetch must not touch it, plan A still updates
        vault
            .clone()
            .set_connection_account_choices(connection, vec![ref_a.clone()])
            .unwrap();
        vault
            .clone()
            .import_equateplus_snapshot(connection, two_plans(5.0).to_string())
            .unwrap();
        assert_eq!(depot_quantity(&vault, "Zweites Programm"), 4.0);
        assert_eq!(vault.list_accounts().unwrap().len(), 2);
        let after = vault.connection_account_choices(connection).unwrap();
        assert!(
            after
                .iter()
                .find(|c| c.account_ref == ref_a)
                .unwrap()
                .enabled
        );
        assert!(
            !after
                .iter()
                .find(|c| c.account_ref == ref_b)
                .unwrap()
                .enabled
        );

        // switch it on again: the next fetch updates it
        vault
            .clone()
            .set_connection_account_choices(connection, vec![ref_a, ref_b])
            .unwrap();
        vault
            .clone()
            .import_equateplus_snapshot(connection, two_plans(5.0).to_string())
            .unwrap();
        assert_eq!(depot_quantity(&vault, "Zweites Programm"), 5.0);
    }

    #[test]
    fn a_program_that_appears_later_is_fetched_by_default() {
        let (_dir, vault, connection) = open_vault("equateplus-select-new");
        vault
            .clone()
            .import_equateplus_snapshot(connection, fixture().to_string())
            .unwrap();
        assert_eq!(vault.list_accounts().unwrap().len(), 1);
        vault
            .clone()
            .import_equateplus_snapshot(connection, two_plans(4.0).to_string())
            .unwrap();
        assert_eq!(vault.list_accounts().unwrap().len(), 2);
        assert!(
            vault
                .connection_account_choices(connection)
                .unwrap()
                .iter()
                .all(|c| c.enabled)
        );
    }

    fn library_doc(id: &str, plan: Option<&str>, service: &str) -> Value {
        json!({"id": id, "date": "2011-05-26T00:00:00", "addedDate": "2011-05-27T00:00:00", "description": "Test Bestätigung",
          "documentType": "Dividendenbestätigung", "plan": plan,
          "url": format!("/services/{service}/download?documentId={id}")})
    }

    #[test]
    fn document_list_keeps_only_valid_entries_and_groups_identical_ones() {
        let (_dir, vault, connection) = open_vault("equateplus-doc-list");
        let list = json!([
            library_doc("d1", None, "statements"),
            library_doc("d1b", None, "statements"), // same identity: a second document, not a duplicate
            library_doc("d2", Some("Test Programm"), "documents"),
            {"id": "bad-url", "date": "2011-05-26", "url": "https://example.com/x.pdf"},
            {"id": "extra-query", "date": "2011-05-26", "url": "/services/documents/download?documentId=x&other=1"},
            {"id": "bad-date", "date": "gestern", "url": "/services/documents/download?documentId=y"},
            {"date": "2011-05-26", "url": "/services/documents/download?documentId=z"},
        ]);
        let pending = vault
            .clone()
            .pending_equateplus_documents(connection, list.to_string())
            .unwrap();
        assert_eq!(pending.len(), 3);
        let keys: Vec<String> = pending
            .iter()
            .map(|p| {
                let v: Value = serde_json::from_str(p).unwrap();
                assert_eq!(v["date"], "2011-05-26");
                v["key"].as_str().unwrap().to_owned()
            })
            .collect();
        assert_eq!(keys[0], keys[1], "identical entries share a group");
        assert_ne!(keys[0], keys[2]);
        assert!(keys.iter().all(|k| valid_document_group(k)));
        assert!(
            vault
                .clone()
                .pending_equateplus_documents(connection, "kein json".into())
                .is_err()
        );
    }

    fn pending_docs(vault: &std::sync::Arc<Vault>, connection: i64, list: &Value) -> Vec<String> {
        vault
            .clone()
            .pending_equateplus_documents(connection, list.to_string())
            .unwrap()
    }

    fn import_doc(
        vault: &std::sync::Arc<Vault>,
        connection: i64,
        doc: &str,
        content: &str,
    ) -> bool {
        vault
            .clone()
            .import_equateplus_document(
                connection,
                doc.to_owned(),
                format!("%PDF-1.4 {content}").into_bytes(),
            )
            .unwrap()
    }

    fn stored_contents(vault: &Vault) -> Vec<String> {
        let db = vault.db().unwrap();
        let mut q = db
            .prepare("SELECT pdf_bytes FROM booking_documents ORDER BY id")
            .unwrap();
        q.query_map([], |r| r.get::<_, Vec<u8>>(0))
            .unwrap()
            .map(|b| {
                String::from_utf8(b.unwrap())
                    .unwrap()
                    .replace("%PDF-1.4 ", "")
            })
            .collect()
    }

    #[test]
    fn a_document_is_recognised_in_a_later_sign_in_although_the_portal_ids_change() {
        let (_dir, vault, connection) = open_vault("equateplus-doc-stable");
        let first = json!([
            library_doc("session1-a", None, "statements"),
            library_doc("session1-b", None, "statements")
        ]);
        let pending = pending_docs(&vault, connection, &first);
        assert!(import_doc(&vault, connection, &pending[0], "x"));
        assert!(import_doc(&vault, connection, &pending[1], "y"));
        // next sign-in: other ids and urls, same documents
        let second = json!([
            library_doc("session2-x", None, "statements"),
            library_doc("session2-y", None, "statements")
        ]);
        assert!(pending_docs(&vault, connection, &second).is_empty());
        // a third document with the same identity is new: the group is incomplete, all three are
        // listed, the two stored ones match by content and only the new one is stored
        let third = json!([
            library_doc("s3-a", None, "statements"),
            library_doc("s3-b", None, "statements"),
            library_doc("s3-c", None, "statements")
        ]);
        let pending = pending_docs(&vault, connection, &third);
        assert_eq!(pending.len(), 3);
        assert!(!import_doc(&vault, connection, &pending[0], "x"));
        assert!(!import_doc(&vault, connection, &pending[1], "y"));
        assert!(import_doc(&vault, connection, &pending[2], "z"));
        assert!(pending_docs(&vault, connection, &third).is_empty());
        assert_eq!(stored_contents(&vault), ["x", "y", "z"]);
    }

    #[test]
    fn a_partly_failed_group_is_completed_even_if_the_portal_changes_the_order() {
        let (_dir, vault, connection) = open_vault("equateplus-doc-resume");
        let list = json!([
            library_doc("a", None, "statements"),
            library_doc("b", None, "statements")
        ]);
        // PDF A is stored, PDF B fails
        let pending = pending_docs(&vault, connection, &list);
        assert!(import_doc(&vault, connection, &pending[0], "A"));
        // next fetch: the portal lists B before A (other ids as well)
        let swapped = json!([
            library_doc("b2", None, "statements"),
            library_doc("a2", None, "statements")
        ]);
        let pending = pending_docs(&vault, connection, &swapped);
        assert_eq!(pending.len(), 2, "an incomplete group is listed in full");
        assert!(
            import_doc(&vault, connection, &pending[0], "B"),
            "B is new, not mistaken for A"
        );
        assert!(
            !import_doc(&vault, connection, &pending[1], "A"),
            "A is already stored"
        );
        assert!(pending_docs(&vault, connection, &swapped).is_empty());
        assert_eq!(stored_contents(&vault), ["A", "B"]);
    }

    #[test]
    fn identical_pdfs_of_one_group_are_both_kept() {
        let (_dir, vault, connection) = open_vault("equateplus-doc-twins");
        let list = json!([
            library_doc("a", None, "statements"),
            library_doc("b", None, "statements")
        ]);
        let pending = pending_docs(&vault, connection, &list);
        assert!(import_doc(&vault, connection, &pending[0], "same"));
        assert!(
            import_doc(&vault, connection, &pending[1], "same"),
            "the second entry takes the second slot"
        );
        assert!(pending_docs(&vault, connection, &list).is_empty());
        assert_eq!(stored_contents(&vault), ["same", "same"]);
    }

    #[test]
    fn the_description_of_the_document_is_what_gets_stored() {
        let (_dir, vault, connection) = open_vault("equateplus-doc-title");
        let mut doc = library_doc("d1", None, "statements");
        doc["description"] = json!("Order confirmation 123");
        let pending = pending_docs(&vault, connection, &json!([doc]));
        assert!(import_doc(&vault, connection, &pending[0], "t"));
        let title: String = vault
            .db()
            .unwrap()
            .query_row("SELECT title FROM booking_documents", [], |r| r.get(0))
            .unwrap();
        assert_eq!(title, "Order confirmation 123");
    }

    #[test]
    fn a_downloaded_pdf_is_stored_once_and_not_requested_again() {
        let (_dir, vault, connection) = open_vault("equateplus-doc-import");
        vault
            .clone()
            .import_equateplus_snapshot(connection, two_plans(4.0).to_string())
            .unwrap();
        let docs = json!([
            library_doc("d1", Some("Test Programm"), "statements"),
            library_doc("d2", Some("Unbekanntes Programm"), "documents"),
            library_doc("d3", None, "statements"),
        ]);
        // (the three differ in plan or have none, so they are three documents)
        let pending = vault
            .clone()
            .pending_equateplus_documents(connection, docs.to_string())
            .unwrap();
        assert_eq!(pending.len(), 3);
        // anything that is not a PDF is rejected
        assert!(
            vault
                .clone()
                .import_equateplus_document(connection, pending[0].clone(), b"<html>".to_vec())
                .is_err()
        );
        for doc in &pending {
            assert!(
                vault
                    .clone()
                    .import_equateplus_document(connection, doc.clone(), b"%PDF-1.4 test".to_vec())
                    .unwrap()
            );
        }
        // a few bytes in front of the header are allowed, an HTML page is not
        assert!(has_pdf_header(b"\xEF\xBB\xBF\n%PDF-1.4 test"));
        assert!(!has_pdf_header(b"<html><body>Fehler</body></html>"));
        assert!(!has_pdf_header(
            &[b'x'; 2000]
                .iter()
                .copied()
                .chain(*b"%PDF-")
                .collect::<Vec<u8>>()
        ));
        // a later run: the group is complete, nothing is listed, and storing one again changes nothing
        assert!(
            vault
                .clone()
                .pending_equateplus_documents(connection, docs.to_string())
                .unwrap()
                .is_empty()
        );
        assert!(
            !vault
                .clone()
                .import_equateplus_document(
                    connection,
                    pending[0].clone(),
                    b"%PDF-1.4 test".to_vec()
                )
                .unwrap()
        );
        // the program named in the document is its account; otherwise none, never a guess
        let plan_a = vault
            .list_accounts()
            .unwrap()
            .into_iter()
            .find(|a| a.name.contains("Test Programm"))
            .unwrap()
            .id;
        let db = vault.db().unwrap();
        let key_of = |i: usize| -> String {
            let v: Value = serde_json::from_str(&pending[i]).unwrap();
            v["key"].as_str().unwrap().to_owned()
        };
        let account_of = |i: usize| -> Option<i64> {
            db.query_row(
                "SELECT account_id FROM booking_documents WHERE source_url=?1",
                [document_source(connection, &key_of(i), 0)],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(account_of(0), Some(plan_a));
        assert_eq!(account_of(1), None);
        assert_eq!(account_of(2), None);
    }

    #[test]
    fn documents_only_work_for_an_equateplus_login() {
        let dir = crate::test_support::TempDir::new("equateplus-doc-other");
        let vault =
            Vault::open(dir.join("vault.db").to_string_lossy().into(), vec![7; 32]).unwrap();
        let other = vault
            .add_bank_connection(ConnectorKind::Quirion, "Andere".into(), "x".into())
            .unwrap();
        assert!(
            vault
                .clone()
                .pending_equateplus_documents(other, "[]".into())
                .is_err()
        );
        assert!(
            vault
                .clone()
                .import_equateplus_document(other, "{}".into(), b"%PDF-".to_vec())
                .is_err()
        );
    }

    #[test]
    fn separates_available_locked_and_performance_without_cost_basis() {
        let r = snapshot(&fixture().to_string()).unwrap();
        assert_eq!(r.accounts.len(), 1);
        assert_eq!(r.holdings.len(), 3);
        let available = r
            .holdings
            .iter()
            .find(|h| h.holding.custody_type == "Verfügbar")
            .unwrap();
        assert_eq!(available.holding.quantity, 2.5);
        let performance = r
            .holdings
            .iter()
            .find(|h| h.holding.custody_type == "Gesperrt (Performance)")
            .unwrap();
        assert_eq!(performance.holding.purchase_price_cents, 0);
        assert_eq!(r.accounts[0].balance_cents, 84872);
        assert!(r.transactions.is_empty());
    }
    #[test]
    fn live_schema_nets_disposals_and_skips_empty_and_contribution_rows() {
        let mut data = fixture();
        data["plans"][0]["details"] = json!({
            "totals":[{"type":"TOTAL","quantity":{"amount":0},"value":{"amount":0}}],
            "entries":[{"entries":[{
                "vehicle":"Test Company","productType":"SHARE","canTrade":true,
                "marketPrice":{"amount":100,"unit":{"code":"EUR"}},
                "entries":[
                    {"QUANTITY":{"amount":-10.5},"VEHICLE":null,"SELL_PURCHASE_PRICE":{"amount":100}},
                    {"QUANTITY":{"amount":5.25},"VEHICLE":"Test Shares","SELL_PURCHASE_PRICE":{"amount":90}},
                    {"QUANTITY":{"amount":5.25},"VEHICLE":"Test Dividend","SELL_PURCHASE_PRICE":{"amount":80}}
                ]
            },{"marketPrice":null,"entries":[{"PURCHASE_PRICE":{"amount":100}}]}]}]
        });
        let parsed = snapshot(&data.to_string()).unwrap();
        assert!(parsed.holdings.is_empty());
        assert_eq!(parsed.accounts[0].balance_cents, 0);

        data["plans"][0]["details"] = json!({
            "totals":[
                {"type":"TOTAL","quantity":{"amount":4},"value":{"amount":400}},
                {"type":"LOCKED","quantity":{"amount":4},"value":{"amount":400}},
                {"type":"AVAILABLE","quantity":{"amount":0},"value":{"amount":0}}
            ],
            "entries":[{"entries":[{
                "vehicle":"Test Company","productType":"RSU","canTrade":false,
                "marketPrice":{"amount":100,"unit":{"code":"EUR"}},
                "entries":[{"VEHICLE_DESCRIPTION":"Test Award"},
                    {"VEHICLE_DESCRIPTION":"Test Award","LOCKED_QTY":{"amount":1.5}},
                    {"VEHICLE_DESCRIPTION":"Other Award","LOCKED_QTY":{"amount":2.5}}]
            }]}]
        });
        let parsed = snapshot(&data.to_string()).unwrap();
        assert_eq!(parsed.holdings.len(), 1);
        assert_eq!(parsed.holdings[0].holding.quantity, 4.0);
        assert_eq!(
            parsed.holdings[0].holding.name,
            "Test Company RSU · Gesperrt"
        );
        assert_eq!(parsed.accounts[0].balance_cents, 40000);
        data["plans"][0]["details"]["totals"][0]["value"]["amount"] = json!(401);
        assert!(snapshot(&data.to_string()).is_err());
    }

    #[test]
    fn remaining_shares_after_disposal_have_no_invented_cost_basis() {
        let lots = vec![
            Lot {
                name: "Test".into(),
                state: "Verfügbar",
                quantity: 10.0,
                price: 100.0,
                purchase: Some(80.0),
            },
            Lot {
                name: "Test".into(),
                state: "Verfügbar",
                quantity: -4.0,
                price: 100.0,
                purchase: Some(90.0),
            },
        ];
        let holdings = net_holdings(lots, "test").unwrap();
        assert_eq!(holdings[0].holding.quantity, 6.0);
        assert_eq!(holdings[0].holding.value_cents, 60000);
        assert_eq!(holdings[0].holding.purchase_price_cents, 0);
    }

    #[test]
    fn ignores_blank_columns_and_parent_subtotals() {
        let mut data = fixture();
        let market = &mut data["plans"][0]["details"]["entries"][0]["entries"][0];
        market["QUANTITY"] = json!({"amount":6.875});
        let rsu = &mut market["entries"][1];
        rsu["QUANTITY"] = json!({"amount":null,"displayValue":"–"});
        rsu["LOCKED_QTY"] = json!({"displayValue":"–"});
        rsu["SELL_PURCHASE_PRICE"] = json!({"amount":null});
        let parsed = snapshot(&data.to_string()).unwrap();
        assert_eq!(parsed.holdings.len(), 3);
        assert_eq!(parsed.accounts[0].balance_cents, 84872);
        assert_eq!(parsed.holdings[2].holding.purchase_price_cents, 0);
    }

    #[test]
    fn rejects_partial_duplicate_foreign_and_ambiguous_data() {
        let v = fixture();
        let mut bad = v.clone();
        bad["plans"][0]["details"]["entries"] = Value::Null;
        assert!(snapshot(&bad.to_string()).is_err());
        let mut bad = v.clone();
        bad["plans"]
            .as_array_mut()
            .unwrap()
            .push(v["plans"][0].clone());
        assert!(snapshot(&bad.to_string()).is_err());
        let mut bad = v.clone();
        bad["plans"][0]["details"]["entries"][0]["entries"][0]["marketPrice"]["unit"]["code"] =
            json!("USD");
        assert!(snapshot(&bad.to_string()).is_err());
        assert!(
            quantities(
                &json!({"QUANTITY":{"amount":2},"TOTAL_UNITS":{"amount":3}}),
                Some(true)
            )
            .is_err()
        );
        assert_eq!(
            quantities(&json!({"UNITS":{"amount":2}}), None).unwrap()[0].1,
            "Verfügbarkeit unbekannt"
        );
        assert_eq!(
            quantities(&json!({"QUANTITY":{"amount":2}}), Some(false)).unwrap()[0].1,
            "Gesperrt"
        );
    }
    #[test]
    fn repeated_import_and_explicit_empty_plan_replace_holdings() {
        let dir = crate::test_support::TempDir::new("equateplus");
        let vault = Vault::open(
            dir.path().join("vault.db").to_string_lossy().into(),
            vec![7; 32],
        )
        .unwrap();
        let id = vault
            .clone()
            .add_bank_connection(
                ConnectorKind::EquatePlus,
                "Test EquatePlus".into(),
                "test".into(),
            )
            .unwrap();
        let data = fixture().to_string();
        vault
            .clone()
            .import_equateplus_snapshot(id, data.clone())
            .unwrap();
        vault.clone().import_equateplus_snapshot(id, data).unwrap();
        let db = vault.db().unwrap();
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM holdings", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            3
        );
        drop(db);
        let mut broken = fixture();
        broken["plans"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":"broken-plan","details":{}}));
        assert!(
            vault
                .clone()
                .import_equateplus_snapshot(id, broken.to_string())
                .is_err()
        );
        assert_eq!(
            vault
                .db()
                .unwrap()
                .query_row("SELECT SUM(value_cents) FROM holdings", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            84872
        );
        let mut empty = fixture();
        empty["plans"][0]["details"]["entries"] = json!([]);
        empty["plans"][0]["details"]["totals"] =
            json!([{"type":"TOTAL","quantity":{"amount":0},"value":{"amount":0}}]);
        vault
            .clone()
            .import_equateplus_snapshot(id, empty.to_string())
            .unwrap();
        assert_eq!(
            vault
                .db()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM holdings", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}
