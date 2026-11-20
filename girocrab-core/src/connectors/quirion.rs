//! quirion's authenticated portal API. Requests and authorization stay inside
//! WKWebView; only the selected account/portfolio data crosses the bridge.
use std::{collections::HashSet, sync::Arc};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rusqlite::{OptionalExtension, params};
use serde_json::Value;

use super::{FetchResult, RemoteAccount, RemoteHolding, RemoteTransaction, SyncSummary};
use crate::{
    error::{CoreError, CoreResult},
    i18n::tr,
    models::{AccountKind, ConnectorKind, Holding},
    vault::Vault,
};

fn invalid() -> CoreError {
    CoreError::InvalidInput { message: tr("quirion: Unvollständige oder ungültige Portalantwort. Es wurden keine Daten übernommen.").into() }
}

fn text<'a>(v: &'a Value, key: &str) -> CoreResult<&'a str> {
    v[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(invalid)
}

fn date(v: &Value, key: &str) -> CoreResult<String> {
    let s = text(v, key)?.get(..10).ok_or_else(invalid)?;
    jiff::civil::Date::strptime("%Y-%m-%d", s).map_err(|_| invalid())?;
    Ok(s.into())
}

fn number(v: &Value, key: &str) -> CoreResult<f64> {
    v[key]
        .as_f64()
        .filter(|n| n.is_finite() && n.abs() < 1e12)
        .ok_or_else(invalid)
}

// The portal returns binary-float artefacts (e.g. 8398.279999999999).
// Round monetary values once at the boundary, never quantities.
fn cents(v: &Value, key: &str) -> CoreResult<i64> {
    Ok((number(v, key)? * 100.0).round() as i64)
}

fn id(v: &Value, key: &str) -> CoreResult<String> {
    v[key]
        .as_u64()
        .filter(|n| *n > 0)
        .map(|n| n.to_string())
        .ok_or_else(invalid)
}

fn rows<'a>(v: &'a Value, key: &str) -> CoreResult<&'a Vec<Value>> {
    v[key].as_array().ok_or_else(invalid)
}

fn cash(
    v: &Value,
    account_ref: String,
    name: String,
    valuation_date: &str,
) -> CoreResult<RemoteAccount> {
    let iban = text(v, "iban")?;
    if text(v, "currency")? != "EUR" || number(v, "exchangeRate")? != 1.0 {
        return Err(invalid());
    }
    Ok(RemoteAccount {
        external_ref: account_ref,
        name,
        owner: text(v, "accountHolder")?.into(),
        iban: iban.into(),
        account_number: text(v, "sortKey")?
            .rsplit('#')
            .next()
            .ok_or_else(invalid)?
            .into(),
        currency: "EUR".into(),
        kind: AccountKind::Checking,
        balance_cents: cents(v, "balance")?,
        // liquid.balanceDate is the creation date even when its balance changes.
        balance_date: valuation_date.into(),
    })
}

fn transactions(
    product: &Value,
    key: &str,
    account: &RemoteAccount,
) -> CoreResult<Vec<RemoteTransaction>> {
    rows(product, key)?
        .iter()
        .map(|t| {
            if text(t, "status")? != "CLOSED" || text(t, "currency")? != account.currency {
                return Err(invalid());
            }
            let amount = cents(t, "amount")?;
            let (party, iban) = if amount >= 0 {
                ("debtorAccountHolder", "debtorIban")
            } else {
                ("creditorAccountHolder", "creditorIban")
            };
            Ok(RemoteTransaction {
                account_ref: account.external_ref.clone(),
                booking_date: date(t, "bookingDate")?,
                value_date: date(t, "valutaDate")?,
                amount_cents: amount,
                currency: account.currency.clone(),
                counterparty: t[party].as_str().unwrap_or_default().into(),
                counterparty_iban: t[iban].as_str().unwrap_or_default().into(),
                purpose: t["verwendungszweck"]
                    .as_str()
                    .or_else(|| t["Verwendungszweck"].as_str())
                    .unwrap_or_default()
                    .into(),
                booking_text: text(t, "type")?.into(),
                ..Default::default()
            })
        })
        .collect()
}

fn snapshot(json: &str) -> CoreResult<FetchResult> {
    let root: Value = serde_json::from_str(json).map_err(|_| invalid())?;
    if root["version"].as_u64() != Some(1) {
        return Err(invalid());
    }
    let mut result = FetchResult {
        bank_name: Some("quirion".into()),
        ..Default::default()
    };
    let mut seen = HashSet::new();
    for partner in rows(&root, "partners")? {
        let bp = id(partner, "businessPartnerId")?;
        if !seen.insert(format!("bp:{bp}")) {
            return Err(invalid());
        }
        let owner = text(partner, "name")?;
        let partner_date = date(partner, "balanceDate")?;
        if !partner["clearing"].is_null() {
            let account = cash(
                &partner["clearing"],
                format!("clearing:{bp}"),
                "Verrechnungskonto PLUS".into(),
                &partner_date,
            )?;
            result
                .transactions
                .extend(transactions(partner, "clearingTransactions", &account)?);
            result.accounts.push(account);
        }
        for product in rows(partner, "products")? {
            let summary = &product["summary"];
            let ips = id(summary, "ipsId")?;
            if id(summary, "businessPartnerId")? != bp || text(summary, "status")? != "OPEN" {
                return Err(invalid());
            }
            let depot_ref = format!("depot:{bp}:{ips}");
            if !seen.insert(depot_ref.clone()) {
                return Err(invalid());
            }
            let name = text(summary, "name")?;
            let balance_date = date(summary, "balanceDate")?;
            let detail = &product["detail"];
            let liquid_ref = format!("liquid:{bp}:{ips}");
            let liquid = cash(
                &detail["liquid"],
                liquid_ref.clone(),
                format!("{name} – Liquidität"),
                &balance_date,
            )?;
            let depot_value = cents(summary, "depotBalance")?;
            if depot_value + liquid.balance_cents != cents(summary, "balance")? {
                return Err(invalid());
            }
            result.accounts.push(RemoteAccount {
                external_ref: depot_ref.clone(),
                account_number: ips,
                name: format!("{name} – Depot"),
                owner: owner.into(),
                iban: String::new(),
                currency: liquid.currency.clone(),
                kind: AccountKind::Brokerage,
                balance_cents: depot_value,
                balance_date,
            });
            let mut total = 0i64;
            let mut isins = HashSet::new();
            for item in rows(detail, "depotItems")? {
                let isin = text(item, "isin")?;
                if !isins.insert(isin)
                    || isin.len() != 12
                    || !isin.bytes().all(|b| b.is_ascii_alphanumeric())
                    || number(item, "quantity")? < 0.0
                    || number(item, "exchangeRate")? != 1.0
                {
                    return Err(invalid());
                }
                let value = cents(item, "value")?;
                total += value;
                result.holdings.push(RemoteHolding {
                    classification: crate::asset_classification::from_labels(
                        item["assetClass1"]
                            .as_str()
                            .or(item["class"].as_str())
                            .unwrap_or_default(),
                        "",
                    ),
                    direct_crypto: false,

                    account_ref: depot_ref.clone(),
                    holding: Holding {
                        isin: isin.into(),
                        name: item["shortName"]
                            .as_str()
                            .filter(|s| !s.is_empty())
                            .unwrap_or(text(item, "name")?)
                            .into(),
                        quantity: number(item, "quantity")?,
                        current_price_cents: cents(item, "aktuellerKurs")?,
                        purchase_price_cents: cents(item, "einstandskurs")?,
                        value_cents: value,
                        ..Default::default()
                    },
                });
            }
            if total != depot_value {
                return Err(invalid());
            }
            result.holdings_fetched.push(depot_ref);
            result
                .transactions
                .extend(transactions(product, "transactions", &liquid)?);
            result.accounts.push(liquid);
        }
    }
    if result.accounts.is_empty() {
        return Err(invalid());
    }
    Ok(result)
}

#[uniffi::export]
impl Vault {
    pub fn import_quirion_snapshot(
        self: Arc<Self>,
        connection_id: i64,
        snapshot_json: String,
    ) -> CoreResult<SyncSummary> {
        if self.portal_connection(connection_id)?.0 != ConnectorKind::Quirion {
            return Err(invalid());
        }
        crate::banking_gate::check_connector("quirion")?;
        let summary = super::store_fetch(
            &self,
            ConnectorKind::Quirion,
            connection_id,
            snapshot(&snapshot_json)?,
        )?;
        let _ = self.checkpoint();
        Ok(summary)
    }

    /// Only return unknown PDF metadata; downloaded content is stored once per
    /// connection/document id. No session state is part of this identifier.
    pub fn pending_quirion_documents(
        self: Arc<Self>,
        connection_id: i64,
        documents_json: String,
    ) -> CoreResult<Vec<String>> {
        self.check_quirion(connection_id)?;
        // document retrieval switched off for this connection: nothing to download
        if !super::documents_fetch_on(&*self.db()?, connection_id)? {
            return Ok(Vec::new());
        }
        let docs: Vec<Value> = serde_json::from_str(&documents_json).map_err(|_| invalid())?;
        let db = self.db()?;
        let mut seen = HashSet::new();
        let mut pending = Vec::new();
        for doc in docs {
            let key = text(&doc, "id")?;
            id(&doc, "businessPartnerId")?;
            date(&doc, "createdAt")?;
            if seen.insert(key.to_string())
                && !db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM booking_documents WHERE source_url=?1)",
                    [document_source(connection_id, key)],
                    |r| r.get::<_, bool>(0),
                )?
            {
                pending.push(doc.to_string());
            }
        }
        Ok(pending)
    }

    pub fn import_quirion_document(
        self: Arc<Self>,
        connection_id: i64,
        document_json: String,
        bytes: Vec<u8>,
    ) -> CoreResult<bool> {
        self.check_quirion(connection_id)?;
        if !bytes.starts_with(b"%PDF-") {
            return Err(invalid());
        }
        let doc: Value = serde_json::from_str(&document_json).map_err(|_| invalid())?;
        let key = text(&doc, "id")?;
        let bp = id(&doc, "businessPartnerId")?;
        let created = date(&doc, "createdAt")?;
        let title = text(&doc, "displayName")?;
        let db = self.db()?;
        // Metadata identifies a business partner, not a particular portfolio.
        // Attach to its clearing account, falling back to one of its depots.
        let account_id: i64 = db
            .query_row(
                "SELECT id FROM accounts WHERE connection_id=?1 AND deleted_ms IS NULL
             AND (external_ref=?2 OR external_ref LIKE ?3)
             ORDER BY (external_ref=?2) DESC, id LIMIT 1",
                params![
                    connection_id,
                    format!("clearing:{bp}"),
                    format!("depot:{bp}:%")
                ],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(invalid)?;
        Ok(crate::documents::insert_doc(
            &db,
            None,
            Some(account_id),
            title,
            "postbox",
            "application/pdf",
            &format!("quirion_{}.pdf", URL_SAFE_NO_PAD.encode(key)),
            &bytes,
            &document_source(connection_id, key),
            &created,
        )?
        .is_some())
    }
}

impl Vault {
    fn check_quirion(&self, connection_id: i64) -> CoreResult<()> {
        if self.portal_connection(connection_id)?.0 != ConnectorKind::Quirion {
            return Err(invalid());
        }
        crate::banking_gate::check_connector("quirion")
    }
}

fn document_source(connection_id: i64, id: &str) -> String {
    format!("quirion:{connection_id}:{}", URL_SAFE_NO_PAD.encode(id))
}

pub struct WebOnlyConnector;

#[async_trait::async_trait]
impl super::BankConnector for WebOnlyConnector {
    async fn begin(&mut self, _ctx: &super::ConnectContext) -> CoreResult<super::Phase> {
        Err(CoreError::BrowserLoginRequired {
            message: tr("Bitte bei quirion im Browserfenster anmelden und die Daten abrufen.")
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
            message: tr("Bitte bei quirion im Browserfenster abrufen.").into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> Value {
        let cash = |kind: &str, amount: f64| {
            json!({"accountHolder":"Test Owner", "iban":format!("DE00{kind}"),
            "sortKey":format!("BA#{kind}#1000"),"currency":"EUR","exchangeRate":1,"balance":amount})
        };
        let tx = json!({"amount":-1.23,"currency":"EUR","status":"CLOSED","bookingDate":"2026-11-12",
            "valutaDate":"2026-11-16","type":"Gebühreneinzug","Verwendungszweck":"Test fee"});
        json!({"version":1,"partners":[{"businessPartnerId":1,"name":"Test Owner","balanceDate":"2026-11-13T06:00:00Z",
        "clearing":cash("CLEARING",0.01),"clearingTransactions":[tx.clone()],"products":[{
            "summary":{"ipsId":1001,"businessPartnerId":1,"name":"Globales Portfolio","status":"OPEN",
                "depotBalance":serde_json::from_str::<Value>("100.26999999999999").unwrap(),"balance":110.28,"balanceDate":"2026-11-13T06:00:00Z"},
            "detail":{"liquid":cash("LIQUID",10.01),"depotItems":[{"isin":"DE0001234567","name":"Test ETF",
                "quantity":1.0001,"value":100.27,"aktuellerKurs":100.2599,"einstandskurs":90.12,"exchangeRate":1,
                "notRealGUV":10.14,"notRealGUVPercentage":11.2501,"referenceDate":"2026-11-13"}]},
            "transactions":[tx.clone(),tx]
        }]}]})
    }

    #[test]
    fn maps_separate_accounts_cost_prices_and_float_artifacts() {
        let r = snapshot(&fixture().to_string()).unwrap();
        assert_eq!(r.accounts.len(), 3);
        assert_eq!(
            r.accounts.iter().map(|a| a.balance_cents).sum::<i64>(),
            11029
        );
        assert_eq!(r.accounts[1].balance_cents, 10027);
        assert_eq!(r.accounts[2].balance_date, "2026-11-13");
        assert_eq!(r.transactions.len(), 3);
        assert_eq!(r.holdings[0].holding.quantity, 1.0001);
        assert_eq!(r.holdings[0].holding.purchase_price_cents, 9012);
        assert_eq!(r.holdings[0].holding.current_price_cents, 10026);
        assert_eq!(r.holdings[0].holding.value_cents, 10027);
    }

    #[test]
    fn multiple_business_partners_keep_their_own_accounts_and_positions() {
        let mut root = fixture();
        let mut second = root["partners"][0].clone();
        second["businessPartnerId"] = json!(2);
        second["name"] = json!("Test Child");
        second["products"][0]["summary"]["businessPartnerId"] = json!(2);
        second["products"][0]["summary"]["ipsId"] = json!(2001);
        root["partners"].as_array_mut().unwrap().push(second);
        let parsed = snapshot(&root.to_string()).unwrap();
        assert_eq!(parsed.accounts.len(), 6);
        assert_eq!(parsed.accounts[4].owner, "Test Child");
        assert_eq!(parsed.holdings[1].account_ref, "depot:2:2001");
        assert_eq!(parsed.transactions[3].account_ref, "clearing:2");
        assert_eq!(parsed.transactions[4].account_ref, "liquid:2:2001");
    }

    #[test]
    fn rejects_partial_ambiguous_or_inconsistent_data_before_writing() {
        for pointer in [
            "/partners/0/clearing/balance",
            "/partners/0/products/0/detail/depotItems",
            "/partners/0/products/0/transactions",
            "/partners/0/products/0/detail/liquid/currency",
        ] {
            let mut v = fixture();
            *v.pointer_mut(pointer).unwrap() = Value::Null;
            assert!(snapshot(&v.to_string()).is_err(), "{pointer}");
        }
        for (pointer, value) in [
            ("/partners/0/products/0/summary/balance", json!(999)),
            ("/partners/0/products/0/summary/depotBalance", json!(999)),
            ("/partners/0/products/0/summary/businessPartnerId", json!(2)),
            (
                "/partners/0/products/0/detail/depotItems/0/exchangeRate",
                json!(0.9),
            ),
            (
                "/partners/0/products/0/detail/depotItems/0/quantity",
                json!(-1),
            ),
            (
                "/partners/0/products/0/transactions/0/status",
                json!("OPEN"),
            ),
            (
                "/partners/0/products/0/transactions/0/bookingDate",
                json!("invalid"),
            ),
        ] {
            let mut v = fixture();
            *v.pointer_mut(pointer).unwrap() = value;
            assert!(snapshot(&v.to_string()).is_err(), "{pointer}");
        }
        let mut duplicate = fixture();
        let p = duplicate["partners"][0]["products"][0].clone();
        duplicate["partners"][0]["products"]
            .as_array_mut()
            .unwrap()
            .push(p);
        assert!(snapshot(&duplicate.to_string()).is_err());
    }

    #[test]
    fn sync_is_idempotent_keeps_identical_bookings_and_clears_empty_depots() {
        let dir = crate::test_support::TempDir::new("quirion");
        let vault =
            Vault::open(dir.join("vault.db").to_string_lossy().into(), vec![7; 32]).unwrap();
        let connection = vault
            .add_bank_connection(ConnectorKind::Quirion, "Test quirion".into(), "test".into())
            .unwrap();
        let first = vault
            .clone()
            .import_quirion_snapshot(connection, fixture().to_string())
            .unwrap();
        assert_eq!(first.transactions_imported, 3);
        let second = vault
            .clone()
            .import_quirion_snapshot(connection, fixture().to_string())
            .unwrap();
        assert_eq!(second.transactions_imported, 0);
        assert_eq!(vault.list_accounts().unwrap().len(), 3);
        let depot = vault
            .list_accounts()
            .unwrap()
            .into_iter()
            .find(|a| a.kind == AccountKind::Brokerage)
            .unwrap();
        let holdings = vault.list_holdings(depot.id).unwrap();
        assert_eq!(holdings[0].purchase_price_cents, 9012);
        let mut empty = fixture();
        empty["partners"][0]["products"][0]["detail"]["depotItems"] = json!([]);
        empty["partners"][0]["products"][0]["summary"]["depotBalance"] = json!(0);
        empty["partners"][0]["products"][0]["summary"]["balance"] = json!(10.01);
        vault
            .clone()
            .import_quirion_snapshot(connection, empty.to_string())
            .unwrap();
        assert!(vault.list_holdings(depot.id).unwrap().is_empty());

        let metadata = json!({"id":"test-doc","businessPartnerId":1,"createdAt":"2026-11-01","displayName":"Test statement"}).to_string();
        let list = format!("[{metadata},{metadata}]");
        assert_eq!(
            vault
                .clone()
                .pending_quirion_documents(connection, list.clone())
                .unwrap()
                .len(),
            1
        );
        assert!(
            vault
                .clone()
                .import_quirion_document(connection, metadata.clone(), b"not a PDF".to_vec())
                .is_err()
        );
        assert!(
            vault
                .clone()
                .import_quirion_document(
                    connection,
                    metadata.clone(),
                    b"%PDF-1.7 synthetic".to_vec()
                )
                .unwrap()
        );
        assert!(
            !vault
                .clone()
                .import_quirion_document(connection, metadata, b"%PDF-1.7 synthetic".to_vec())
                .unwrap()
        );
        assert!(
            vault
                .clone()
                .pending_quirion_documents(connection, list)
                .unwrap()
                .is_empty()
        );
        let wrong = vault
            .add_bank_connection(ConnectorKind::PayPal, "Other".into(), "test".into())
            .unwrap();
        assert!(
            vault
                .clone()
                .import_quirion_snapshot(wrong, fixture().to_string())
                .is_err()
        );
    }
}
