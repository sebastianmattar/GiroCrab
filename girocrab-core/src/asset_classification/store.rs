//! User choices outlive replacement of holdings. Resets are timestamped NULLs.
use super::*;
use crate::{
    error::{CoreError, CoreResult},
    vault::Vault,
};
use rusqlite::{Connection, params};
use std::collections::HashMap;

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS security_classifications (
 security_key TEXT PRIMARY KEY NOT NULL,
 asset_class_override TEXT, asset_class_ms INTEGER NOT NULL DEFAULT 0,
 instrument_override TEXT, instrument_ms INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS holding_classifications (
 holding_id INTEGER PRIMARY KEY REFERENCES holdings(id) ON DELETE CASCADE,
 asset_class TEXT, instrument TEXT, direct_crypto INTEGER NOT NULL DEFAULT 0
);";

#[derive(Debug, Clone, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ClassifiedAsset {
    pub holding: Holding,
    pub classification: AssetClassification,
    pub asset_item_id: Option<i64>,
}
fn class_of(key: Option<String>) -> Option<AssetClass> {
    key.map(|s| AssetClass::from_db(&s))
}
fn instrument_of(key: Option<String>) -> Option<InstrumentKind> {
    key.map(|s| InstrumentKind::from_db(&s))
}

/// All user choices of the vault, keyed by security key: one query per call, not one per position.
fn user_overrides(db: &Connection) -> CoreResult<HashMap<String, ClassificationValues>> {
    let mut q = db.prepare(
        "SELECT security_key, asset_class_override, instrument_override
         FROM security_classifications",
    )?;
    let rows = q.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            ClassificationValues {
                asset_class: class_of(r.get(1)?),
                instrument: instrument_of(r.get(2)?),
            },
        ))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn tangible_assets(vault: &Vault, account_id: i64) -> CoreResult<Vec<ClassifiedAsset>> {
    let items = vault.list_asset_items(account_id)?;
    let db = vault.db()?;
    let overrides = user_overrides(&db)?;
    let uuids: HashMap<i64, String> = db
        .prepare("SELECT id, uuid FROM asset_items WHERE account_id = ?1")?
        .query_map([account_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let mut out = Vec::new();
    for item in items.into_iter().filter(|i| !i.sold) {
        let Some(uuid) = uuids.get(&item.id) else {
            continue;
        };
        let key = format!("asset:{uuid}");
        let user = overrides.get(&key).cloned().unwrap_or_default();
        // Automatically priced items are precious metals; everything else needs a choice.
        let metal = item.setup.price_source != "manual";
        let (asset_class, asset_class_source) = match user.asset_class {
            Some(v) => (v, ClassificationSource::User),
            None if metal => (AssetClass::Commodity, ClassificationSource::Rule),
            None => (AssetClass::Unclassified, ClassificationSource::Unknown),
        };
        let (instrument, instrument_source) = match user.instrument {
            Some(v) => (v, ClassificationSource::User),
            None => (InstrumentKind::DirectHolding, ClassificationSource::Rule),
        };
        out.push(ClassifiedAsset {
            holding: item.holding,
            classification: AssetClassification {
                security_key: Some(key),
                asset_class,
                asset_class_source,
                instrument,
                instrument_source,
            },
            asset_item_id: Some(item.id),
        });
    }
    Ok(out)
}

/// Positions and their import metadata come from one statement, so a row can never be
/// paired with the metadata of another one.
fn security_assets(db: &Connection, account_id: i64) -> CoreResult<Vec<ClassifiedAsset>> {
    use crate::vault::accounts::{HOLDING_COLUMNS, HOLDING_FILTER, holding_from_row};
    let overrides = user_overrides(db)?;
    let mut q = db.prepare(&format!(
        "SELECT {HOLDING_COLUMNS}, c.asset_class, c.instrument, COALESCE(c.direct_crypto, 0)
         FROM holdings h LEFT JOIN holding_classifications c ON c.holding_id = h.id
         WHERE {HOLDING_FILTER}"
    ))?;
    let rows = q.query_map([account_id], |r| {
        Ok((
            holding_from_row(r)?,
            ClassificationValues {
                asset_class: class_of(r.get(18)?),
                instrument: instrument_of(r.get(19)?),
            },
            r.get::<_, bool>(20)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (holding, imported, direct) = row?;
        let user = security_key(&holding, direct)
            .and_then(|key| overrides.get(&key).cloned())
            .unwrap_or_default();
        out.push(ClassifiedAsset {
            classification: classify_asset(holding.clone(), imported, user, direct),
            holding,
            asset_item_id: None,
        });
    }
    Ok(out)
}

fn invalid() -> CoreError {
    CoreError::invalid(crate::i18n::tr("Ungültiger Klassifizierungsschlüssel."))
}
fn validate_key(db: &Connection, key: &str) -> CoreResult<()> {
    if let Some(isin) = key.strip_prefix("isin:")
        && normalized_isin(isin).as_deref() == Some(isin)
    {
        return Ok(());
    }
    if let Some(t) = key.strip_prefix("crypto:")
        && crypto_ticker(
            &Holding {
                wkn: t.into(),
                ..Default::default()
            },
            true,
        )
        .as_deref()
            == Some(t)
    {
        return Ok(());
    }
    if let Some(uuid) = key.strip_prefix("asset:") {
        let exists: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM asset_items WHERE uuid=?1 AND deleted_ms IS NULL)",
            [uuid],
            |r| r.get(0),
        )?;
        if exists {
            return Ok(());
        }
    }
    Err(invalid())
}
impl Vault {
    fn set_classification_field(
        &self,
        key: &str,
        value: Option<&str>,
        instrument: bool,
    ) -> CoreResult<()> {
        let db = self.db()?;
        validate_key(&db, key)?;
        let tx = db.unchecked_transaction()?;
        let (column, stamp) = if instrument {
            ("instrument_override", "instrument_ms")
        } else {
            ("asset_class_override", "asset_class_ms")
        };
        tx.execute(
            "INSERT OR IGNORE INTO security_classifications(security_key) VALUES (?1)",
            [key],
        )?;
        tx.execute(
            &format!(
                "UPDATE security_classifications
                 SET {column} = ?2, {stamp} = MAX(?3, {stamp} + 1)
                 WHERE security_key = ?1"
            ),
            params![key, value, crate::dates::now_ms()],
        )?;
        tx.commit()?;
        Ok(())
    }
}
#[uniffi::export]
impl Vault {
    pub fn set_asset_class_override(
        &self,
        security_key: String,
        value: Option<AssetClass>,
    ) -> CoreResult<()> {
        self.set_classification_field(&security_key, value.map(AssetClass::as_str), false)
    }
    pub fn set_instrument_override(
        &self,
        security_key: String,
        value: Option<InstrumentKind>,
    ) -> CoreResult<()> {
        self.set_classification_field(&security_key, value.map(InstrumentKind::as_str), true)
    }
    pub fn classified_assets(&self, account_id: i64) -> CoreResult<Vec<ClassifiedAsset>> {
        if self.list_tangible_account_ids()?.contains(&account_id) {
            return tangible_assets(self, account_id);
        }
        security_assets(&*self.db()?, account_id)
    }
}

pub(crate) fn merge_attached(db: &Connection) -> CoreResult<()> {
    // Fieldwise LWW. Deterministic equal-stamp tie break; a reset wins an equal tie.
    db.execute_batch(
        "INSERT INTO security_classifications
         SELECT * FROM other.security_classifications WHERE true
         ON CONFLICT(security_key) DO UPDATE SET
           asset_class_override = CASE
             WHEN excluded.asset_class_ms > asset_class_ms
               OR (excluded.asset_class_ms = asset_class_ms
                   AND COALESCE(excluded.asset_class_override, '~')
                     > COALESCE(asset_class_override, '~'))
             THEN excluded.asset_class_override ELSE asset_class_override END,
           asset_class_ms = MAX(asset_class_ms, excluded.asset_class_ms),
           instrument_override = CASE
             WHEN excluded.instrument_ms > instrument_ms
               OR (excluded.instrument_ms = instrument_ms
                   AND COALESCE(excluded.instrument_override, '~')
                     > COALESCE(instrument_override, '~'))
             THEN excluded.instrument_override ELSE instrument_override END,
           instrument_ms = MAX(instrument_ms, excluded.instrument_ms);",
    )?;
    Ok(())
}
