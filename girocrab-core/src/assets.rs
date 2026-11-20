//! Tangible assets ("Sachwerte", GiroCrab/GiroCrab#87): physical things with a
//! changing value – gold coins and bars priced automatically, a house or a car
//! valued by hand.
//!
//! A Sachwerte account is a depot without a bank connection
//! (`kind = 'brokerage'`, `external_ref = "tangible-<uuid>"`). Its items live in
//! `asset_items`; quantities and manual values are dated points, so the value of
//! any past day can be reproduced (a partial sale doesn't rewrite history):
//!
//! `value(item, day) = quantity(day) × unit value(day)`
//!
//! - quantity(day): the latest quantity point on or before `day` (0 before the purchase)
//! - unit value, automatic: the latest stored price (EUR per troy ounce) on or
//!   before `day` × the item's fine weight in troy ounces
//! - unit value, manual: the value points, held (step) or interpolated (linear)
//!
//! Prices are stored per day (`asset_prices`) and only missing days are
//! fetched ([`Vault::refresh_asset_prices`]), so the history works offline and
//! travels with the iCloud vault merge. Prices typed in by hand win over the API.
//!
//! To the rest of the app a Sachwerte account looks like a securities depot:
//! `list_holdings` returns its items as [`Holding`]s, `accounts.balance_cents`
//! holds today's value, and `balance_history` uses the daily valuation.

use std::collections::{HashMap, HashSet};

use jiff::civil::Date;
use rusqlite::{Connection, OptionalExtension, params};

use crate::dates::{iso, now_ms, parse_iso, today};
use crate::error::{CoreError, CoreResult};
use crate::i18n::{tr, trf};
use crate::models::*;
use crate::schema::{has_column, has_table};
use crate::vault::Vault;

pub(crate) const ASSET_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS asset_items (
    id                   INTEGER PRIMARY KEY,
    uuid                 TEXT    NOT NULL UNIQUE,     -- identity across devices (vault merge)
    account_id           INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    name                 TEXT    NOT NULL,
    price_source         TEXT    NOT NULL,            -- 'XAU' (API symbol) | 'manual'
    weight_grams         REAL    NOT NULL DEFAULT 0,  -- gross weight of one piece
    purity_permille      REAL    NOT NULL DEFAULT 1000, -- fineness, 999.9 / 916.7 / 585 …
    interpolation        TEXT    NOT NULL DEFAULT 'step', -- manual values: step | linear
    purchase_date        TEXT    NOT NULL,            -- YYYY-MM-DD
    purchase_price_cents INTEGER,                     -- total paid; NULL = market value on purchase_date
    note                 TEXT    NOT NULL DEFAULT '',
    created_ms           INTEGER NOT NULL DEFAULT 0,
    symbol               TEXT    NOT NULL DEFAULT '', -- SF Symbol name; '' = by price source
    changed_ms           INTEGER NOT NULL DEFAULT 0,  -- last edit of the setup (vault merge: newest wins)
    deleted_ms           INTEGER                      -- soft delete, so a merge can't bring it back
);
CREATE INDEX IF NOT EXISTS asset_items_account ON asset_items(account_id);
CREATE TABLE IF NOT EXISTS asset_quantities (
    item_id  INTEGER NOT NULL REFERENCES asset_items(id) ON DELETE CASCADE,
    date     TEXT    NOT NULL,                        -- quantity held from this day on
    quantity REAL    NOT NULL,
    changed_ms INTEGER NOT NULL DEFAULT 0,
    deleted_ms INTEGER,
    PRIMARY KEY (item_id, date)
);
CREATE TABLE IF NOT EXISTS asset_values (
    item_id     INTEGER NOT NULL REFERENCES asset_items(id) ON DELETE CASCADE,
    date        TEXT    NOT NULL,
    value_cents INTEGER NOT NULL,                     -- manual value of ONE piece
    changed_ms  INTEGER NOT NULL DEFAULT 0,
    deleted_ms  INTEGER,
    PRIMARY KEY (item_id, date)
);
CREATE TABLE IF NOT EXISTS asset_prices (
    symbol      TEXT    NOT NULL,                     -- 'XAU'
    date        TEXT    NOT NULL,
    price_cents INTEGER NOT NULL,                     -- EUR cents per troy ounce
    source      TEXT    NOT NULL DEFAULT 'api',       -- api | manual (manual is never overwritten)
    PRIMARY KEY (symbol, date)
);
CREATE TABLE IF NOT EXISTS asset_price_fetch (
    symbol       TEXT PRIMARY KEY,
    from_date    TEXT    NOT NULL,                    -- the API was asked for [from_date, to_date]
    to_date      TEXT    NOT NULL,
    fetched_unix INTEGER NOT NULL
);";

/// Columns added after the first development builds of the feature: (table, column, type).
const LATER_COLUMNS: &[(&str, &str, &str)] = &[
    ("asset_items", "symbol", "TEXT NOT NULL DEFAULT ''"),
    ("asset_items", "changed_ms", "INTEGER NOT NULL DEFAULT 0"),
    ("asset_items", "deleted_ms", "INTEGER"),
    (
        "asset_quantities",
        "changed_ms",
        "INTEGER NOT NULL DEFAULT 0",
    ),
    ("asset_quantities", "deleted_ms", "INTEGER"),
    ("asset_values", "changed_ms", "INTEGER NOT NULL DEFAULT 0"),
    ("asset_values", "deleted_ms", "INTEGER"),
];

/// Schema step of the feature (see `schema.rs`): the tables, plus the columns a
/// vault of a development build may still lack. Idempotent.
pub(crate) fn migrate(conn: &Connection) -> CoreResult<()> {
    conn.execute_batch(ASSET_SCHEMA)?;
    for (table, col, def) in LATER_COLUMNS {
        if !has_column(conn, "main", table, col)? {
            conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {col} {def}"))?;
        }
    }
    Ok(())
}

/// Sets a dated point (`asset_quantities.quantity` / `asset_values.value_cents`),
/// reviving a deleted one.
fn put_point(
    conn: &Connection,
    table: &str,
    col: &str,
    item_id: i64,
    date: &str,
    value: &dyn rusqlite::ToSql,
) -> CoreResult<()> {
    conn.execute(
        &format!(
            "INSERT INTO {table}(item_id, date, {col}, changed_ms, deleted_ms) VALUES (?1, ?2, ?3, ?4, NULL)
             ON CONFLICT(item_id, date) DO UPDATE SET {col} = excluded.{col},
                 changed_ms = excluded.changed_ms, deleted_ms = NULL"
        ),
        params![item_id, date, value, now_ms()],
    )?;
    Ok(())
}

/// Soft-deletes the live points of an item matching `filter` (SQL on `date`,
/// `?1` = item id, `?2` = `arg`), so a vault merge can't bring them back.
fn drop_points(
    conn: &Connection,
    table: &str,
    item_id: i64,
    filter: &str,
    arg: &str,
) -> CoreResult<()> {
    conn.execute(
        &format!(
            "UPDATE {table} SET deleted_ms = ?3, changed_ms = ?3
              WHERE item_id = ?1 AND deleted_ms IS NULL AND ({filter})"
        ),
        params![item_id, arg, now_ms()],
    )?;
    Ok(())
}

/// One troy ounce in grams (the unit of every metal price).
pub const TROY_OUNCE_GRAMS: f64 = 31.103_476_8;
/// Value source of items priced by hand.
pub const MANUAL_SOURCE: &str = "manual";
/// Price symbols the app can fetch (Frankfurter API). Silver, platinum and
/// palladium (`XAG`, `XPT`, `XPD`) work the same way and only need to be added here.
pub const PRICE_SYMBOLS: &[&str] = &["XAU"];
const EXTERNAL_REF_PREFIX: &str = "tangible-";
const MAX_NAME_CHARS: usize = 100;
const MAX_NOTE_CHARS: usize = 2000;
const MAX_SYMBOL_CHARS: usize = 64;
const FRANKFURTER_URL: &str = "https://api.frankfurter.dev/v2/rates";

/// Between two manual value points: keep the older value (step) or draw a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum AssetInterpolation {
    Step,
    Linear,
}

impl AssetInterpolation {
    fn as_str(self) -> &'static str {
        match self {
            AssetInterpolation::Step => "step",
            AssetInterpolation::Linear => "linear",
        }
    }
    fn parse(s: &str) -> Self {
        if s == "linear" {
            AssetInterpolation::Linear
        } else {
            AssetInterpolation::Step
        }
    }
}

/// What the user enters for an item.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct AssetItemSetup {
    pub name: String,
    /// Price symbol (`"XAU"`) or `"manual"`.
    pub price_source: String,
    /// Gross weight of one piece in grams (automatic prices only).
    pub weight_grams: f64,
    /// Fineness in per mille (999.9, 916.7, 585 …); 1000 = pure.
    pub purity_permille: f64,
    pub interpolation: AssetInterpolation,
    /// `YYYY-MM-DD`.
    pub purchase_date: String,
    /// Pieces bought on the purchase date (> 0).
    pub quantity: f64,
    /// Total price paid; `None` = the market value on the purchase date.
    pub purchase_price_cents: Option<i64>,
    /// Manual items: value of one piece on the purchase date.
    pub start_value_cents: Option<i64>,
    pub note: String,
    /// Icon (SF Symbol name) the user picked; empty = the default for the price source.
    pub symbol: String,
}

/// Quantity held from `date` on.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct AssetQuantityPoint {
    pub date: String,
    pub quantity: f64,
}

/// Manual value of one piece on `date`.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct AssetValuePoint {
    pub date: String,
    pub value_cents: i64,
}

/// An item with its setup, history and today's valuation.
#[derive(Debug, Clone, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct AssetItem {
    pub id: i64,
    pub account_id: i64,
    pub setup: AssetItemSetup,
    /// Today's position, in the same shape as a securities position
    /// (`current_price_cents` / `purchase_price_cents` per piece).
    pub holding: Holding,
    pub quantities: Vec<AssetQuantityPoint>,
    pub values: Vec<AssetValuePoint>,
    /// Date of the price or value point today's value is based on ("" = none yet).
    pub value_date: String,
    /// The API has no price for this date (e.g. a purchase before 2000):
    /// the user should enter one ([`Vault::set_asset_price`]).
    pub missing_price_date: Option<String>,
    /// Sold completely (quantity today is 0).
    pub sold: bool,
}

/// A denomination ("Stückelung") the item editor offers.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct AssetWeightPreset {
    pub label: String,
    pub grams: f64,
}

/// Result of [`Vault::refresh_asset_prices`].
#[derive(Debug, Clone, Default, PartialEq, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct AssetPriceRefresh {
    /// Whether the API was contacted at all (only with automatic items).
    pub requested: bool,
    /// New or changed daily prices.
    pub prices_stored: u32,
    /// Network/API problem; the last known prices stay in use.
    pub error: Option<String>,
}

fn is_manual(source: &str) -> bool {
    source == MANUAL_SOURCE
}

fn valid_date(s: &str) -> CoreResult<String> {
    let d = parse_iso(s).ok_or_else(|| CoreError::invalid(trf("Ungültiges Datum: {0}", &[&s])))?;
    if d > today() {
        return Err(CoreError::invalid(tr(
            "Das Datum darf nicht in der Zukunft liegen.",
        )));
    }
    Ok(iso(d))
}

fn validate(s: &AssetItemSetup) -> CoreResult<AssetItemSetup> {
    let mut s = s.clone();
    s.name = s.name.trim().to_string();
    if s.name.is_empty() {
        return Err(CoreError::invalid(tr("Bitte einen Namen angeben.")));
    }
    if s.name.chars().count() > MAX_NAME_CHARS {
        return Err(CoreError::invalid(trf(
            "Der Name ist zu lang (höchstens {0} Zeichen).",
            &[&MAX_NAME_CHARS],
        )));
    }
    s.note = s.note.trim().chars().take(MAX_NOTE_CHARS).collect();
    s.symbol = s.symbol.trim().chars().take(MAX_SYMBOL_CHARS).collect();
    s.purchase_date = valid_date(&s.purchase_date)?;
    if !(s.quantity.is_finite() && s.quantity > 0.0) {
        return Err(CoreError::invalid(tr("Die Anzahl muss größer als 0 sein.")));
    }
    if s.purchase_price_cents.is_some_and(|c| c < 0) {
        return Err(CoreError::invalid(tr(
            "Der Kaufpreis darf nicht negativ sein.",
        )));
    }
    if is_manual(&s.price_source) {
        match s.start_value_cents {
            Some(v) if v >= 0 => {}
            _ => {
                return Err(CoreError::invalid(tr(
                    "Bitte den Wert zum Kaufdatum angeben.",
                )));
            }
        }
        s.weight_grams = 0.0;
        s.purity_permille = 1000.0;
    } else {
        if !PRICE_SYMBOLS.contains(&s.price_source.as_str()) {
            return Err(CoreError::invalid(trf(
                "Unbekannte Kursquelle: {0}",
                &[&s.price_source],
            )));
        }
        if !(s.weight_grams.is_finite() && s.weight_grams > 0.0) {
            return Err(CoreError::invalid(tr(
                "Das Gewicht muss größer als 0 sein.",
            )));
        }
        if !(s.purity_permille.is_finite()
            && s.purity_permille > 0.0
            && s.purity_permille <= 1000.0)
        {
            return Err(CoreError::invalid(tr(
                "Der Feingehalt muss zwischen 0 und 1000 ‰ liegen.",
            )));
        }
        s.start_value_cents = None;
    }
    Ok(s)
}

// ------------------------------------------------------------------ valuation

/// An item row with its points, everything the valuation needs.
#[derive(Debug, Clone)]
struct Loaded {
    id: i64,
    account_id: i64,
    name: String,
    source: String,
    weight_grams: f64,
    purity_permille: f64,
    interpolation: AssetInterpolation,
    purchase_date: String,
    purchase_price_cents: Option<i64>,
    note: String,
    symbol: String,
    /// Ascending by date.
    quantities: Vec<(String, f64)>,
    /// Ascending by date (manual items).
    values: Vec<(String, i64)>,
}

impl Loaded {
    fn fine_ounces(&self) -> f64 {
        self.weight_grams * self.purity_permille / 1000.0 / TROY_OUNCE_GRAMS
    }

    fn quantity_at(&self, day: &str) -> f64 {
        let i = self.quantities.partition_point(|(d, _)| d.as_str() <= day);
        if i == 0 {
            0.0
        } else {
            self.quantities[i - 1].1
        }
    }

    /// Value of one piece on `day` and the date it is based on.
    fn unit_value_at(&self, day: &str, prices: &Prices) -> Option<(f64, String)> {
        if is_manual(&self.source) {
            manual_value_at(&self.values, self.interpolation, day)
        } else {
            let (date, cents) = prices.at(&self.source, day)?;
            Some((cents as f64 * self.fine_ounces(), date.to_string()))
        }
    }

    fn value_at(&self, day: &str, prices: &Prices) -> i64 {
        let q = self.quantity_at(day);
        if q == 0.0 {
            return 0;
        }
        self.unit_value_at(day, prices)
            .map(|(u, _)| (u * q).round() as i64)
            .unwrap_or(0)
    }
}

/// Manual value of one piece on `day` (`None` before the first point).
fn manual_value_at(
    points: &[(String, i64)],
    interp: AssetInterpolation,
    day: &str,
) -> Option<(f64, String)> {
    let i = points.partition_point(|(d, _)| d.as_str() <= day);
    if i == 0 {
        return None;
    }
    let (d0, v0) = &points[i - 1];
    if interp == AssetInterpolation::Linear
        && let Some((d1, v1)) = points.get(i)
        && let (Some(a), Some(b), Some(x)) = (parse_iso(d0), parse_iso(d1), parse_iso(day))
    {
        let span = crate::dates::days_between(a, b) as f64;
        if span > 0.0 {
            let t = crate::dates::days_between(a, x) as f64 / span;
            return Some((*v0 as f64 + (*v1 - *v0) as f64 * t, day.to_string()));
        }
    }
    Some((*v0 as f64, d0.clone()))
}

/// Stored daily prices per symbol, ascending by date.
#[derive(Debug, Default)]
struct Prices(HashMap<String, Vec<(String, i64)>>);

impl Prices {
    fn load(conn: &Connection) -> CoreResult<Self> {
        let mut map: HashMap<String, Vec<(String, i64)>> = HashMap::new();
        let mut st = conn
            .prepare("SELECT symbol, date, price_cents FROM asset_prices ORDER BY symbol, date")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows {
            let (s, d, p) = row?;
            map.entry(s).or_default().push((d, p));
        }
        Ok(Prices(map))
    }

    /// The latest price on or before `day` (weekends and gaps keep the last price).
    fn at(&self, symbol: &str, day: &str) -> Option<(&str, i64)> {
        let list = self.0.get(symbol)?;
        let i = list.partition_point(|(d, _)| d.as_str() <= day);
        (i > 0).then(|| (list[i - 1].0.as_str(), list[i - 1].1))
    }
}

/// Live items (of one account / one item, or all) with their live points.
fn load_items(
    conn: &Connection,
    account_id: Option<i64>,
    item_id: Option<i64>,
) -> CoreResult<Vec<Loaded>> {
    let mut items: Vec<Loaded> = {
        let mut st = conn.prepare(
            "SELECT id, account_id, name, price_source, weight_grams, purity_permille, interpolation,
                    purchase_date, purchase_price_cents, note, symbol
               FROM asset_items
              WHERE (?1 IS NULL OR account_id = ?1) AND (?2 IS NULL OR id = ?2) AND deleted_ms IS NULL
                -- a deleted account keeps its items (it may come back), hidden
                AND account_id IN (SELECT id FROM accounts WHERE deleted_ms IS NULL)
              ORDER BY account_id, purchase_date, id",
        )?;
        let rows = st.query_map(params![account_id, item_id], |r| {
            Ok(Loaded {
                id: r.get(0)?,
                account_id: r.get(1)?,
                name: r.get(2)?,
                source: r.get(3)?,
                weight_grams: r.get(4)?,
                purity_permille: r.get(5)?,
                interpolation: AssetInterpolation::parse(&r.get::<_, String>(6)?),
                purchase_date: r.get(7)?,
                purchase_price_cents: r.get(8)?,
                note: r.get(9)?,
                symbol: r.get(10)?,
                quantities: vec![],
                values: vec![],
            })
        })?;
        rows.collect::<Result<_, _>>()?
    };
    let index: HashMap<i64, usize> = items.iter().enumerate().map(|(i, it)| (it.id, i)).collect();
    {
        let mut st = conn.prepare(
            "SELECT item_id, date, quantity FROM asset_quantities
              WHERE deleted_ms IS NULL AND (?1 IS NULL OR item_id = ?1) ORDER BY item_id, date",
        )?;
        let rows = st.query_map([item_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, f64>(2)?,
            ))
        })?;
        for row in rows {
            let (id, d, q) = row?;
            if let Some(&i) = index.get(&id) {
                items[i].quantities.push((d, q));
            }
        }
    }
    {
        let mut st = conn.prepare(
            "SELECT item_id, date, value_cents FROM asset_values
              WHERE deleted_ms IS NULL AND (?1 IS NULL OR item_id = ?1) ORDER BY item_id, date",
        )?;
        let rows = st.query_map([item_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows {
            let (id, d, v) = row?;
            if let Some(&i) = index.get(&id) {
                items[i].values.push((d, v));
            }
        }
    }
    Ok(items)
}

/// Dates the API was already asked for, per symbol.
fn fetched_ranges(conn: &Connection) -> CoreResult<HashMap<String, (String, String, i64)>> {
    let mut st =
        conn.prepare("SELECT symbol, from_date, to_date, fetched_unix FROM asset_price_fetch")?;
    let rows = st.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?, r.get(3)?)))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn to_item(
    it: &Loaded,
    prices: &Prices,
    fetched: &HashMap<String, (String, String, i64)>,
    day: &str,
) -> AssetItem {
    let quantity = it.quantity_at(day);
    let unit = it.unit_value_at(day, prices);
    let initial = it.quantities.first().map(|q| q.1).unwrap_or(0.0);
    // purchase price per piece: what was paid, else the value on the purchase date
    let bought_unit = match it.purchase_price_cents {
        Some(total) if initial > 0.0 => Some(total as f64 / initial),
        _ => it.unit_value_at(&it.purchase_date, prices).map(|(u, _)| u),
    };
    let missing_price_date = (!is_manual(&it.source)
        && prices.at(&it.source, &it.purchase_date).is_none()
        && fetched
            .get(&it.source)
            .is_some_and(|(from, _, _)| from.as_str() <= it.purchase_date.as_str()))
    .then(|| it.purchase_date.clone());
    AssetItem {
        id: it.id,
        account_id: it.account_id,
        setup: AssetItemSetup {
            name: it.name.clone(),
            price_source: it.source.clone(),
            weight_grams: it.weight_grams,
            purity_permille: it.purity_permille,
            interpolation: it.interpolation,
            purchase_date: it.purchase_date.clone(),
            quantity: initial,
            purchase_price_cents: it.purchase_price_cents,
            start_value_cents: it.values.first().map(|v| v.1),
            note: it.note.clone(),
            symbol: it.symbol.clone(),
        },
        holding: Holding {
            isin: String::new(),
            wkn: String::new(),
            name: it.name.clone(),
            quantity,
            current_price_cents: unit.as_ref().map(|(u, _)| u.round() as i64).unwrap_or(0),
            purchase_price_cents: bought_unit.map(|u| u.round() as i64).unwrap_or(0),
            value_cents: it.value_at(day, prices),
            ..Default::default()
        },
        quantities: it
            .quantities
            .iter()
            .map(|(date, quantity)| AssetQuantityPoint {
                date: date.clone(),
                quantity: *quantity,
            })
            .collect(),
        values: it
            .values
            .iter()
            .map(|(date, value_cents)| AssetValuePoint {
                date: date.clone(),
                value_cents: *value_cents,
            })
            .collect(),
        value_date: unit.map(|(_, d)| d).unwrap_or_default(),
        missing_price_date,
        sold: quantity == 0.0 && !it.quantities.is_empty(),
    }
}

// ------------------------------------------------------------------ accounts

/// A Sachwerte account whose kind the user changed (e.g. to cash) is an
/// ordinary account afterwards: its balance comes from its bookings and
/// opening balance, not from asset items.
pub(crate) fn tangible_account_ids(conn: &Connection) -> CoreResult<Vec<i64>> {
    let mut st = conn.prepare(
        "SELECT id FROM accounts WHERE connection_id IS NULL AND external_ref LIKE 'tangible-%' AND kind = 'brokerage' AND deleted_ms IS NULL ORDER BY id",
    )?;
    let rows = st.query_map([], |r| r.get(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub(crate) fn is_tangible(conn: &Connection, account_id: i64) -> CoreResult<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM accounts WHERE id = ?1 AND connection_id IS NULL AND external_ref LIKE 'tangible-%' AND kind = 'brokerage' AND deleted_ms IS NULL",
            [account_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn require_tangible(conn: &Connection, account_id: i64) -> CoreResult<()> {
    if is_tangible(conn, account_id)? {
        Ok(())
    } else {
        Err(CoreError::NotFound {
            what: trf("Sachwerte-Konto {0}", &[&account_id]),
        })
    }
}

/// Item id → (account id, price source, purchase date).
fn require_item(conn: &Connection, item_id: i64) -> CoreResult<(i64, String, String)> {
    conn.query_row(
        "SELECT account_id, price_source, purchase_date FROM asset_items WHERE id = ?1 AND deleted_ms IS NULL",
        [item_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .optional()?
    .ok_or_else(|| CoreError::NotFound {
        what: trf("Sachwert {0}", &[&item_id]),
    })
}

/// Writes today's value into `accounts.balance_cents` of every Sachwerte
/// account (like `loans::refresh`), so totals, groups and sidebar just work.
pub(crate) fn refresh_balances(conn: &Connection) -> CoreResult<()> {
    let ids = tangible_account_ids(conn)?;
    if ids.is_empty() {
        return Ok(());
    }
    let day = iso(today());
    let prices = Prices::load(conn)?;
    let items = load_items(conn, None, None)?;
    for id in ids {
        let value: i64 = items
            .iter()
            .filter(|i| i.account_id == id)
            .map(|i| i.value_at(&day, &prices))
            .sum();
        conn.execute(
            "UPDATE accounts SET balance_cents = ?1, balance_date = ?2 WHERE id = ?3",
            params![value, day, id],
        )?;
    }
    Ok(())
}

/// [`refresh_balances`] when a stored value is from an earlier day: values
/// move with the date (linear values, newer prices), not only with edits.
pub(crate) fn refresh_balances_if_stale(conn: &Connection) -> CoreResult<()> {
    let stale: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM accounts WHERE connection_id IS NULL AND external_ref LIKE 'tangible-%' AND kind = 'brokerage'
                         AND deleted_ms IS NULL AND IFNULL(balance_date, '') != ?1)",
        [iso(today())],
        |r| r.get(0),
    )?;
    if stale {
        refresh_balances(conn)?;
    }
    Ok(())
}

/// Today's items of a Sachwerte account as securities positions (still held only).
pub(crate) fn holdings(conn: &Connection, account_id: i64) -> CoreResult<Vec<Holding>> {
    let day = iso(today());
    let prices = Prices::load(conn)?;
    let fetched = fetched_ranges(conn)?;
    let mut out: Vec<Holding> = load_items(conn, Some(account_id), None)?
        .iter()
        .map(|it| to_item(it, &prices, &fetched, &day).holding)
        .filter(|h| h.quantity > 0.0)
        .collect();
    out.sort_by(|a, b| {
        b.value_cents
            .cmp(&a.value_cents)
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(out)
}

/// Daily value of the Sachwerte accounts among `account_ids` for the days
/// `start..=end` (ascending), or `None` when none of them is one. Used by
/// `balance_history`; a valuation for securities depots can plug in here.
pub(crate) fn value_series(
    conn: &Connection,
    account_ids: &[i64],
    start: Date,
    end: Date,
) -> CoreResult<Option<Vec<i64>>> {
    let tangible: HashSet<i64> = tangible_account_ids(conn)?.into_iter().collect();
    let wanted: HashSet<i64> = account_ids
        .iter()
        .copied()
        .filter(|id| tangible.contains(id))
        .collect();
    if wanted.is_empty() {
        return Ok(None);
    }
    let prices = Prices::load(conn)?;
    let items: Vec<Loaded> = load_items(conn, None, None)?
        .into_iter()
        .filter(|i| wanted.contains(&i.account_id))
        .collect();
    let mut out = Vec::new();
    let mut day = start;
    while day <= end {
        let key = iso(day);
        out.push(items.iter().map(|i| i.value_at(&key, &prices)).sum());
        let Ok(next) = day.tomorrow() else { break };
        day = next;
    }
    Ok(Some(out))
}

/// [`Vault::create_tangible_account`] inside an open transaction.
pub(crate) fn create_account_in(conn: &Connection, name: &str) -> CoreResult<i64> {
    let name = name.trim();
    let name = if name.is_empty() {
        tr("Sachwerte")
    } else {
        name
    };
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(CoreError::invalid(trf(
            "Der Name ist zu lang (höchstens {0} Zeichen).",
            &[&MAX_NAME_CHARS],
        )));
    }
    let (group, sym, order) = crate::connectors::group_for(AccountKind::Brokerage);
    let gid = Vault::ensure_group(conn, group, sym, order)?;
    conn.execute(
        "INSERT INTO accounts(connection_id, group_id, external_ref, name, kind, sf_symbol, balance_date, sort_order)
         VALUES (NULL, ?1, ?2, ?3, 'brokerage', 'diamond', ?4, (SELECT IFNULL(MAX(sort_order),0)+1 FROM accounts))",
        params![gid, format!("{EXTERNAL_REF_PREFIX}{}", uuid::Uuid::new_v4()), name, iso(today())],
    )?;
    let id = conn.last_insert_rowid();
    crate::user_groups::place_new_account(conn, id)?;
    Ok(id)
}

/// Stores a validated setup; returns the item id.
fn store_item(
    conn: &Connection,
    account_id: i64,
    item_id: Option<i64>,
    s: &AssetItemSetup,
) -> CoreResult<i64> {
    let id = match item_id {
        Some(id) => {
            // later quantity changes / value points must stay after the purchase date,
            // and switching a valued item to a market price would drop its values
            let later_before: i64 = conn.query_row(
                "SELECT (SELECT count(*) FROM asset_quantities WHERE item_id = ?1 AND deleted_ms IS NULL AND date < ?2
                           AND date > (SELECT MIN(date) FROM asset_quantities WHERE item_id = ?1 AND deleted_ms IS NULL))
                      + (SELECT count(*) FROM asset_values WHERE item_id = ?1 AND deleted_ms IS NULL AND date < ?2
                           AND date > (SELECT MIN(date) FROM asset_values WHERE item_id = ?1 AND deleted_ms IS NULL))",
                params![id, s.purchase_date],
                |r| r.get(0),
            )?;
            if later_before > 0 {
                return Err(CoreError::invalid(tr(
                    "Vor dem neuen Kaufdatum liegen noch Bestands- oder Wertänderungen. Bitte diese zuerst entfernen.",
                )));
            }
            if !is_manual(&s.price_source) {
                let values: i64 = conn.query_row(
                    "SELECT count(*) FROM asset_values WHERE item_id = ?1 AND deleted_ms IS NULL",
                    [id],
                    |r| r.get(0),
                )?;
                if values > 1 {
                    return Err(CoreError::invalid(tr(
                        "Der Sachwert hat eingetragene Werte. Bitte diese zuerst entfernen, bevor er über den Kurs bewertet wird.",
                    )));
                }
            }
            conn.execute(
                "UPDATE asset_items SET name = ?1, price_source = ?2, weight_grams = ?3, purity_permille = ?4,
                        interpolation = ?5, purchase_date = ?6, purchase_price_cents = ?7, note = ?8,
                        symbol = ?10, changed_ms = ?11
                  WHERE id = ?9",
                params![
                    s.name,
                    s.price_source,
                    s.weight_grams,
                    s.purity_permille,
                    s.interpolation.as_str(),
                    s.purchase_date,
                    s.purchase_price_cents,
                    s.note,
                    id,
                    s.symbol,
                    now_ms()
                ],
            )?;
            // the purchase point moves with the purchase date
            for table in ["asset_quantities", "asset_values"] {
                drop_points(
                    conn,
                    table,
                    id,
                    &format!(
                        "date < ?2 OR date = (SELECT MIN(date) FROM {table} WHERE item_id = ?1 AND deleted_ms IS NULL)"
                    ),
                    &s.purchase_date,
                )?;
            }
            id
        }
        None => {
            conn.execute(
                "INSERT INTO asset_items(uuid, account_id, name, price_source, weight_grams, purity_permille,
                        interpolation, purchase_date, purchase_price_cents, note, created_ms, symbol, changed_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?11)",
                params![
                    uuid::Uuid::new_v4().to_string(),
                    account_id,
                    s.name,
                    s.price_source,
                    s.weight_grams,
                    s.purity_permille,
                    s.interpolation.as_str(),
                    s.purchase_date,
                    s.purchase_price_cents,
                    s.note,
                    now_ms(),
                    s.symbol
                ],
            )?;
            conn.last_insert_rowid()
        }
    };
    put_point(
        conn,
        "asset_quantities",
        "quantity",
        id,
        &s.purchase_date,
        &s.quantity,
    )?;
    if is_manual(&s.price_source) {
        put_point(
            conn,
            "asset_values",
            "value_cents",
            id,
            &s.purchase_date,
            &s.start_value_cents.unwrap_or(0),
        )?;
    } else {
        drop_points(conn, "asset_values", id, "1", "")?;
    }
    Ok(id)
}

/// Takes over Sachwerte items, their points and hand-typed prices from an
/// attached vault (`other`). Items are matched by uuid, points by item and
/// date; for each the newer edit wins (`changed_ms`), and soft deletes travel
/// along, so a deleted item or point does not come back. Vaults of older
/// development builds lack the stamps: their rows count as oldest.
pub(crate) fn merge_attached(conn: &Connection, acc_map: &HashMap<i64, i64>) -> CoreResult<()> {
    for table in [
        "asset_items",
        "asset_quantities",
        "asset_values",
        "asset_prices",
    ] {
        if !has_table(conn, "other", table)? {
            return Ok(());
        }
    }
    // a column of `other`, or a stand-in when its vault predates it
    let col = |table: &str, name: &str, fallback: &'static str| -> CoreResult<String> {
        Ok(if has_column(conn, "other", table, name)? {
            name.to_string()
        } else {
            fallback.to_string()
        })
    };
    let rows: Vec<(i64, String, i64)> = conn
        .prepare("SELECT id, uuid, account_id FROM other.asset_items")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    let (symbol, changed, deleted) = (
        col("asset_items", "symbol", "''")?,
        col("asset_items", "changed_ms", "0")?,
        col("asset_items", "deleted_ms", "NULL")?,
    );
    let mut item_map: HashMap<i64, i64> = HashMap::new();
    for (oid, uuid, oacc) in rows {
        let Some(acc) = acc_map.get(&oacc) else {
            continue;
        };
        // the account stays this vault's; everything else follows the newer edit
        conn.execute(
            &format!(
                "INSERT INTO asset_items(uuid, account_id, name, price_source, weight_grams, purity_permille,
                        interpolation, purchase_date, purchase_price_cents, note, created_ms, symbol,
                        changed_ms, deleted_ms)
                 SELECT uuid, ?2, name, price_source, weight_grams, purity_permille, interpolation, purchase_date,
                        purchase_price_cents, note, created_ms, {symbol}, {changed}, {deleted}
                   FROM other.asset_items WHERE id = ?1
                 ON CONFLICT(uuid) DO UPDATE SET name = excluded.name, price_source = excluded.price_source,
                        weight_grams = excluded.weight_grams, purity_permille = excluded.purity_permille,
                        interpolation = excluded.interpolation, purchase_date = excluded.purchase_date,
                        purchase_price_cents = excluded.purchase_price_cents, note = excluded.note,
                        symbol = excluded.symbol, changed_ms = excluded.changed_ms, deleted_ms = excluded.deleted_ms
                  WHERE excluded.changed_ms > asset_items.changed_ms"
            ),
            params![oid, acc],
        )?;
        let id: i64 =
            conn.query_row("SELECT id FROM asset_items WHERE uuid = ?1", [&uuid], |r| {
                r.get(0)
            })?;
        item_map.insert(oid, id);
    }
    for (table, value) in [
        ("asset_quantities", "quantity"),
        ("asset_values", "value_cents"),
    ] {
        let (changed, deleted) = (
            col(table, "changed_ms", "0")?,
            col(table, "deleted_ms", "NULL")?,
        );
        let sql = format!(
            "INSERT INTO {table}(item_id, date, {value}, changed_ms, deleted_ms)
             SELECT ?2, date, {value}, {changed}, {deleted} FROM other.{table} WHERE item_id = ?1
             ON CONFLICT(item_id, date) DO UPDATE SET {value} = excluded.{value},
                    changed_ms = excluded.changed_ms, deleted_ms = excluded.deleted_ms
              WHERE excluded.changed_ms > {table}.changed_ms"
        );
        for (oid, id) in &item_map {
            conn.execute(&sql, params![oid, id])?;
        }
    }
    // API prices come along (no new download), hand-typed ones win over API ones
    conn.execute(
        "INSERT OR IGNORE INTO asset_prices(symbol, date, price_cents, source)
         SELECT symbol, date, price_cents, source FROM other.asset_prices WHERE source = 'api'",
        [],
    )?;
    conn.execute(
        "INSERT INTO asset_prices(symbol, date, price_cents, source)
         SELECT symbol, date, price_cents, source FROM other.asset_prices WHERE source = 'manual'
         ON CONFLICT(symbol, date) DO UPDATE SET price_cents = excluded.price_cents, source = 'manual'
          WHERE asset_prices.source = 'api'",
        [],
    )?;
    refresh_balances(conn)?;
    Ok(())
}

/// The latest stored price of `symbol` on or before `day` (primary key lookup).
fn price_on(conn: &Connection, symbol: &str, day: &str) -> CoreResult<Option<(String, i64)>> {
    Ok(conn
        .query_row(
            "SELECT date, price_cents FROM asset_prices WHERE symbol = ?1 AND date <= ?2
              ORDER BY date DESC LIMIT 1",
            params![symbol, day],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}

/// The denominations ("Stückelung") the item editor offers, in menu order:
/// gram weights, then ounce fractions, then the kilo bar – each lightest first.
pub fn asset_weight_presets() -> Vec<AssetWeightPreset> {
    const OZ: f64 = TROY_OUNCE_GRAMS;
    [
        ("1 g", 1.0),
        ("2 g", 2.0),
        ("2,5 g", 2.5),
        ("5 g", 5.0),
        ("10 g", 10.0),
        ("20 g", 20.0),
        ("50 g", 50.0),
        ("100 g", 100.0),
        ("250 g", 250.0),
        ("500 g", 500.0),
        ("1/20 oz", OZ / 20.0),
        ("1/10 oz", OZ / 10.0),
        ("1/4 oz", OZ / 4.0),
        ("1/2 oz", OZ / 2.0),
        ("1 oz", OZ),
        ("2 oz", OZ * 2.0),
        ("5 oz", OZ * 5.0),
        ("10 oz", OZ * 10.0),
        ("1 kg", 1000.0),
    ]
    .into_iter()
    .map(|(label, grams)| AssetWeightPreset {
        label: label.to_string(),
        grams,
    })
    .collect()
}

/// One row of the Frankfurter `/v2/rates` answer.
#[derive(Debug, serde::Deserialize)]
struct RateRow {
    date: String,
    rate: f64,
}

/// Parses a Frankfurter answer into (date, cents per unit).
fn parse_rates(body: &str) -> CoreResult<Vec<(String, i64)>> {
    let rows: Vec<RateRow> = serde_json::from_str(body)
        .map_err(|e| CoreError::protocol(trf("Unerwartete Antwort der Kurs-API: {0}", &[&e])))?;
    Ok(rows
        .into_iter()
        .filter(|r| r.rate.is_finite() && r.rate > 0.0 && parse_iso(&r.date).is_some())
        .map(|r| (r.date, (r.rate * 100.0).round() as i64))
        .collect())
}

/// Stores API prices; hand-typed prices stay. Returns the number of new or changed rows.
fn store_api_prices(conn: &Connection, symbol: &str, rows: &[(String, i64)]) -> CoreResult<u32> {
    let mut n = 0u32;
    for (date, cents) in rows {
        n += conn.execute(
            "INSERT INTO asset_prices(symbol, date, price_cents, source) VALUES (?1, ?2, ?3, 'api')
             ON CONFLICT(symbol, date) DO UPDATE SET price_cents = excluded.price_cents
              WHERE asset_prices.source = 'api' AND asset_prices.price_cents != excluded.price_cents",
            params![symbol, date, cents],
        )? as u32;
    }
    Ok(n)
}

/// What [`Vault::refresh_asset_prices`] has to ask the API for: per symbol in
/// use, the date range `[from, to]`.
fn fetch_plan(conn: &Connection, force: bool) -> CoreResult<Vec<(String, String, String)>> {
    let mut st = conn.prepare(
        "SELECT i.price_source, MIN(i.purchase_date) FROM asset_items i JOIN accounts a ON a.id = i.account_id
          WHERE i.price_source != 'manual' AND i.deleted_ms IS NULL AND a.archived_unix IS NULL AND a.deleted_ms IS NULL GROUP BY i.price_source",
    )?;
    let needed: Vec<(String, String)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let fetched = fetched_ranges(conn)?;
    let to = iso(today());
    let mut out = vec![];
    for (symbol, oldest) in needed {
        // a week before the oldest purchase: a weekend or holiday has no rate of its own
        let min_date = parse_iso(&oldest)
            .map(|d| iso(d - crate::dates::days(7)))
            .unwrap_or(oldest);
        if !PRICE_SYMBOLS.contains(&symbol.as_str()) {
            continue;
        }
        let from = match fetched.get(&symbol) {
            // older purchase than ever asked for: the whole range again
            Some((f, _, _)) if min_date < *f => min_date,
            // up to date (prices are published once a day)
            Some((_, t, _)) if *t == to && !force => continue,
            // only the days since the last answer (the last day again: it may have been missing)
            Some((_, t, _)) => t.clone(),
            None => min_date,
        };
        out.push((symbol, from, to.clone()));
    }
    Ok(out)
}

#[uniffi::export]
impl Vault {
    /// Creates a Sachwerte account (a depot without a bank connection).
    /// An empty name becomes "Sachwerte". Returns the account id.
    pub fn create_tangible_account(&self, name: String) -> Result<i64, CoreError> {
        let db = self.db()?;
        let tx = db.unchecked_transaction()?;
        let id = create_account_in(&tx, &name)?;
        tx.commit()?;
        Ok(id)
    }

    /// Ids of all Sachwerte accounts (archived ones included).
    pub fn list_tangible_account_ids(&self) -> Result<Vec<i64>, CoreError> {
        tangible_account_ids(&*self.db()?)
    }

    /// All items of all Sachwerte accounts (sold ones included) with today's
    /// value, in one pass – what the app shows.
    pub fn list_all_asset_items(&self) -> Result<Vec<AssetItem>, CoreError> {
        let db = self.db()?;
        let tangible: HashSet<i64> = tangible_account_ids(&db)?.into_iter().collect();
        let day = iso(today());
        let prices = Prices::load(&db)?;
        let fetched = fetched_ranges(&db)?;
        Ok(load_items(&db, None, None)?
            .iter()
            .filter(|it| tangible.contains(&it.account_id))
            .map(|it| to_item(it, &prices, &fetched, &day))
            .collect())
    }

    /// All items of a Sachwerte account (sold ones included) with today's value.
    pub fn list_asset_items(&self, account_id: i64) -> Result<Vec<AssetItem>, CoreError> {
        let db = self.db()?;
        require_tangible(&db, account_id)?;
        let day = iso(today());
        let prices = Prices::load(&db)?;
        let fetched = fetched_ranges(&db)?;
        Ok(load_items(&db, Some(account_id), None)?
            .iter()
            .map(|it| to_item(it, &prices, &fetched, &day))
            .collect())
    }

    /// Creates (`item_id` = `None`) or changes an item. Returns its id.
    pub fn save_asset_item(
        &self,
        account_id: i64,
        item_id: Option<i64>,
        setup: AssetItemSetup,
    ) -> Result<i64, CoreError> {
        let s = validate(&setup)?;
        let db = self.db()?;
        let tx = db.unchecked_transaction()?;
        require_tangible(&tx, account_id)?;
        if let Some(id) = item_id {
            let (acc, _, _) = require_item(&tx, id)?;
            if acc != account_id {
                return Err(CoreError::NotFound {
                    what: trf("Sachwert {0}", &[&id]),
                });
            }
        }
        let id = store_item(&tx, account_id, item_id, &s)?;
        refresh_balances(&tx)?;
        tx.commit()?;
        Ok(id)
    }

    pub fn delete_asset_item(&self, item_id: i64) -> Result<(), CoreError> {
        let db = self.db()?;
        let tx = db.unchecked_transaction()?;
        require_item(&tx, item_id)?;
        // soft delete: a vault merge must not bring it back; its points stay with it
        let now = now_ms();
        tx.execute(
            "UPDATE asset_items SET deleted_ms = ?2, changed_ms = ?2 WHERE id = ?1",
            params![item_id, now],
        )?;
        refresh_balances(&tx)?;
        tx.commit()?;
        Ok(())
    }

    /// Sets the quantity held from `date` on (a partial sale, a purchase of
    /// more pieces, 0 = sold). On the purchase date it changes the initial quantity.
    pub fn set_asset_quantity(
        &self,
        item_id: i64,
        date: String,
        quantity: f64,
    ) -> Result<(), CoreError> {
        let date = valid_date(&date)?;
        if !(quantity.is_finite() && quantity >= 0.0) {
            return Err(CoreError::invalid(tr(
                "Die Anzahl darf nicht negativ sein.",
            )));
        }
        let db = self.db()?;
        let tx = db.unchecked_transaction()?;
        let (_, _, purchase) = require_item(&tx, item_id)?;
        if date < purchase {
            return Err(CoreError::invalid(tr("Das Datum liegt vor dem Kaufdatum.")));
        }
        if date == purchase && quantity == 0.0 {
            return Err(CoreError::invalid(tr("Die Anzahl muss größer als 0 sein.")));
        }
        put_point(
            &tx,
            "asset_quantities",
            "quantity",
            item_id,
            &date,
            &quantity,
        )?;
        refresh_balances(&tx)?;
        tx.commit()?;
        Ok(())
    }

    /// Removes a quantity change (not the purchase).
    pub fn delete_asset_quantity(&self, item_id: i64, date: String) -> Result<(), CoreError> {
        let db = self.db()?;
        let tx = db.unchecked_transaction()?;
        let (_, _, purchase) = require_item(&tx, item_id)?;
        if date == purchase {
            return Err(CoreError::invalid(tr(
                "Der Kauf selbst kann nicht entfernt werden.",
            )));
        }
        drop_points(&tx, "asset_quantities", item_id, "date = ?2", &date)?;
        refresh_balances(&tx)?;
        tx.commit()?;
        Ok(())
    }

    /// Sets the manual value of one piece on `date` (manual items only).
    pub fn set_asset_value(
        &self,
        item_id: i64,
        date: String,
        value_cents: i64,
    ) -> Result<(), CoreError> {
        let date = valid_date(&date)?;
        if value_cents < 0 {
            return Err(CoreError::invalid(tr("Der Wert darf nicht negativ sein.")));
        }
        let db = self.db()?;
        let tx = db.unchecked_transaction()?;
        let (_, source, purchase) = require_item(&tx, item_id)?;
        if !is_manual(&source) {
            return Err(CoreError::invalid(tr(
                "Dieser Sachwert wird über den Kurs bewertet.",
            )));
        }
        if date < purchase {
            return Err(CoreError::invalid(tr("Das Datum liegt vor dem Kaufdatum.")));
        }
        put_point(
            &tx,
            "asset_values",
            "value_cents",
            item_id,
            &date,
            &value_cents,
        )?;
        refresh_balances(&tx)?;
        tx.commit()?;
        Ok(())
    }

    /// Removes a manual value point (not the one on the purchase date).
    pub fn delete_asset_value(&self, item_id: i64, date: String) -> Result<(), CoreError> {
        let db = self.db()?;
        let tx = db.unchecked_transaction()?;
        let (_, _, purchase) = require_item(&tx, item_id)?;
        if date == purchase {
            return Err(CoreError::invalid(tr(
                "Der Wert zum Kaufdatum kann nicht entfernt werden.",
            )));
        }
        drop_points(&tx, "asset_values", item_id, "date = ?2", &date)?;
        refresh_balances(&tx)?;
        tx.commit()?;
        Ok(())
    }

    /// Sells `count` pieces on `date` (after the purchase date) in one step: the
    /// quantity drops from that day on, and for manual items an optional sale
    /// price per piece becomes the value point of that day.
    pub fn sell_asset(
        &self,
        item_id: i64,
        date: String,
        count: f64,
        price_cents: Option<i64>,
    ) -> Result<(), CoreError> {
        let date = valid_date(&date)?;
        if !(count.is_finite() && count > 0.0) {
            return Err(CoreError::invalid(tr("Die Anzahl muss größer als 0 sein.")));
        }
        if price_cents.is_some_and(|p| p < 0) {
            return Err(CoreError::invalid(tr("Der Wert darf nicht negativ sein.")));
        }
        let db = self.db()?;
        let tx = db.unchecked_transaction()?;
        let (_, source, purchase) = require_item(&tx, item_id)?;
        if date <= purchase {
            return Err(CoreError::invalid(tr(
                "Ein Verkauf muss nach dem Kaufdatum liegen.",
            )));
        }
        if price_cents.is_some() && !is_manual(&source) {
            return Err(CoreError::invalid(tr(
                "Dieser Sachwert wird über den Kurs bewertet.",
            )));
        }
        let held = load_items(&tx, None, Some(item_id))?
            .first()
            .map(|it| it.quantity_at(&date))
            .unwrap_or(0.0);
        if count > held + 1e-9 {
            return Err(CoreError::invalid(tr(
                "Die Anzahl ist größer als der Bestand an diesem Tag.",
            )));
        }
        if let Some(p) = price_cents {
            put_point(&tx, "asset_values", "value_cents", item_id, &date, &p)?;
        }
        let left = (held - count).max(0.0);
        put_point(&tx, "asset_quantities", "quantity", item_id, &date, &left)?;
        refresh_balances(&tx)?;
        tx.commit()?;
        Ok(())
    }

    /// Stores a price typed in by hand (EUR cents per troy ounce), e.g. for a
    /// purchase before the API's history starts. The API never overwrites it.
    pub fn set_asset_price(
        &self,
        symbol: String,
        date: String,
        price_cents: i64,
    ) -> Result<(), CoreError> {
        let date = valid_date(&date)?;
        if !PRICE_SYMBOLS.contains(&symbol.as_str()) {
            return Err(CoreError::invalid(trf(
                "Unbekannte Kursquelle: {0}",
                &[&symbol],
            )));
        }
        if price_cents <= 0 {
            return Err(CoreError::invalid(tr("Der Kurs muss größer als 0 sein.")));
        }
        let db = self.db()?;
        let tx = db.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO asset_prices(symbol, date, price_cents, source) VALUES (?1, ?2, ?3, 'manual')
             ON CONFLICT(symbol, date) DO UPDATE SET price_cents = excluded.price_cents, source = 'manual'",
            params![symbol, date, price_cents],
        )?;
        refresh_balances(&tx)?;
        tx.commit()?;
        Ok(())
    }

    /// Today's price (EUR cents per troy ounce) of `symbol` and its date, if any is stored.
    pub fn latest_asset_price(&self, symbol: String) -> Result<Option<AssetValuePoint>, CoreError> {
        Ok(price_on(&*self.db()?, &symbol, &iso(today()))?
            .map(|(date, value_cents)| AssetValuePoint { date, value_cents }))
    }

    /// Price (EUR cents per troy ounce) of `symbol` on `date` – the latest
    /// stored price on or before it – and that price's date.
    pub fn asset_price_on(
        &self,
        symbol: String,
        date: String,
    ) -> Result<Option<AssetValuePoint>, CoreError> {
        let date = valid_date(&date)?;
        Ok(price_on(&*self.db()?, &symbol, &date)?
            .map(|(date, value_cents)| AssetValuePoint { date, value_cents }))
    }

    /// Value of one piece of an item on `date` (gold: price × fine weight,
    /// manual: the value points with the item's interpolation).
    pub fn asset_unit_value_on(
        &self,
        item_id: i64,
        date: String,
    ) -> Result<Option<i64>, CoreError> {
        let date = valid_date(&date)?;
        let db = self.db()?;
        require_item(&db, item_id)?;
        let Some(item) = load_items(&db, None, Some(item_id))?.pop() else {
            return Ok(None);
        };
        // one item, one price: no full load of the price table
        let unit = if is_manual(&item.source) {
            manual_value_at(&item.values, item.interpolation, &date).map(|(u, _)| u)
        } else {
            price_on(&db, &item.source, &date)?.map(|(_, c)| c as f64 * item.fine_ounces())
        };
        Ok(unit.map(|u| u.round() as i64))
    }

    /// Daily value of one item from its purchase date until today (the
    /// chart when an item is selected). Sold days are 0.
    pub fn asset_item_history(&self, item_id: i64) -> Result<Vec<DailyBalance>, CoreError> {
        let db = self.db()?;
        let (_, _, purchase) = require_item(&db, item_id)?;
        let prices = Prices::load(&db)?;
        let Some(item) = load_items(&db, None, Some(item_id))?.pop() else {
            return Ok(vec![]);
        };
        let end = today();
        let mut day = parse_iso(&purchase).unwrap_or(end).min(end);
        let mut out = Vec::new();
        while day <= end {
            let key = iso(day);
            out.push(DailyBalance {
                balance_cents: item.value_at(&key, &prices),
                date: key,
            });
            let Ok(next) = day.tomorrow() else { break };
            day = next;
        }
        Ok(out)
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl Vault {
    /// Fetches the missing daily prices (Frankfurter API) for the symbols that
    /// items actually use – no request at all without automatic items. The
    /// request names only the metal and a date range. Once a day is enough;
    /// `force` asks again anyway. Network problems don't fail: the last known
    /// prices stay in use and the gap is filled next time.
    pub async fn refresh_asset_prices(&self, force: bool) -> Result<AssetPriceRefresh, CoreError> {
        let plan = fetch_plan(&*self.db()?, force)?;
        let mut out = AssetPriceRefresh::default();
        if plan.is_empty() {
            return Ok(out);
        }
        out.requested = true;
        let client =
            crate::connectors::client_builder(std::time::Duration::from_secs(15)).build()?;
        for (symbol, from, to) in plan {
            let resp = client
                .get(FRANKFURTER_URL)
                .query(&[
                    ("base", symbol.as_str()),
                    ("quotes", "EUR"),
                    ("from", &from),
                    ("to", &to),
                ])
                .send()
                .await;
            let body = match resp {
                Ok(r) if r.status().is_success() => crate::connectors::read_text_limited(r).await,
                Ok(r) => Err(CoreError::Network {
                    message: trf("Kurs-API antwortet mit HTTP {0}", &[&r.status().as_u16()]),
                }),
                Err(e) => Err(CoreError::from(e)),
            };
            let rows = match body.and_then(|b| parse_rates(&b)) {
                Ok(rows) => rows,
                Err(e) => {
                    out.error = Some(e.to_string());
                    continue;
                }
            };
            let db = self.db()?;
            let tx = db.unchecked_transaction()?;
            out.prices_stored += store_api_prices(&tx, &symbol, &rows)?;
            tx.execute(
                "INSERT INTO asset_price_fetch(symbol, from_date, to_date, fetched_unix) VALUES (?1, ?2, ?3, strftime('%s','now'))
                 ON CONFLICT(symbol) DO UPDATE SET from_date = MIN(from_date, excluded.from_date),
                     to_date = excluded.to_date, fetched_unix = excluded.fetched_unix",
                params![symbol, from, to],
            )?;
            refresh_balances(&tx)?;
            tx.commit()?;
        }
        Ok(out)
    }
}

/// Demo data (fictional): a Sachwerte account with a house, a car, a gold
/// coin and a gold necklace, bought over the last five years. Gold is valued
/// with the real prices from the API like in a real vault (offline the gold
/// items show a missing price); house and car carry their own values,
/// roughly following the market. Returns the account id.
pub(crate) fn seed_demo(conn: &Connection, today: Date) -> CoreResult<i64> {
    let day = |ago: i64| iso(today - crate::dates::days(ago));
    let acc = create_account_in(conn, "Sachwerte")?;
    let values = |item: i64, points: &[(i64, i64)]| -> CoreResult<()> {
        for &(ago, cents) in points {
            conn.execute(
                "INSERT OR REPLACE INTO asset_values(item_id, date, value_cents) VALUES (?1, ?2, ?3)",
                params![item, day(ago), cents],
            )?;
        }
        Ok(())
    };
    let manual = |name: &str,
                  symbol: &str,
                  interpolation: AssetInterpolation,
                  ago: i64,
                  paid: i64,
                  note: &str| AssetItemSetup {
        name: name.into(),
        price_source: MANUAL_SOURCE.into(),
        weight_grams: 0.0,
        purity_permille: 1000.0,
        interpolation,
        purchase_date: day(ago),
        quantity: 1.0,
        purchase_price_cents: Some(paid),
        // the purchase price is the first value
        start_value_cents: Some(paid),
        note: note.into(),
        symbol: symbol.into(),
    };
    let gold = |name: &str,
                symbol: &str,
                grams: f64,
                purity: f64,
                ago: i64,
                paid: Option<i64>,
                note: &str| AssetItemSetup {
        name: name.into(),
        price_source: "XAU".into(),
        weight_grams: grams,
        purity_permille: purity,
        interpolation: AssetInterpolation::Step,
        purchase_date: day(ago),
        quantity: 1.0,
        purchase_price_cents: paid,
        start_value_cents: None,
        note: note.into(),
        symbol: symbol.into(),
    };

    // house: bought at the 2021/22 peak, dip in 2023, recovering since;
    // valued now and then (appraisal, market estimate): the value holds until the next
    let house = store_item(
        conn,
        acc,
        None,
        &manual(
            "Einfamilienhaus",
            "house.fill",
            AssetInterpolation::Step,
            1_790,
            485_000_00,
            "Kaufvertrag und Gutachten im Ordner „Haus“",
        ),
    )?;
    // Explicit example choice for this synthetic item, not a name-based rule for real data.
    conn.execute(
        "INSERT INTO security_classifications(security_key,asset_class_override,asset_class_ms)
         SELECT 'asset:' || uuid,'real_estate',1 FROM asset_items WHERE id=?1",
        [house],
    )?;
    values(
        house,
        &[
            (1_400, 510_000_00),
            (900, 468_000_00),
            (500, 476_000_00),
            (90, 495_000_00),
        ],
    )?;

    // car: new five years ago, losing value steadily
    let car = store_item(
        conn,
        acc,
        None,
        &manual(
            "Auto (Kombi)",
            "car.side.fill",
            AssetInterpolation::Linear,
            1_810,
            38_900_00,
            "",
        ),
    )?;
    values(
        car,
        &[
            (1_450, 32_500_00),
            (1_000, 27_000_00),
            (500, 22_800_00),
            (30, 19_900_00),
        ],
    )?;

    // gold coins: two pieces, no purchase price given, so they start at the
    // gold value of their day
    let coins = AssetItemSetup {
        quantity: 2.0,
        ..gold(
            "Krügerrand 1 oz",
            "seal.fill",
            33.931,
            916.7,
            1_700,
            None,
            "Bankschließfach",
        )
    };
    store_item(conn, acc, None, &coins)?;

    // necklace: 14 carat (585 ‰); paid well above its gold value
    store_item(
        conn,
        acc,
        None,
        &gold(
            "Goldkette 585",
            "girocrab.necklace",
            12.0,
            585.0,
            1_100,
            Some(1_150_00),
            "Geschenk zum Jahrestag",
        ),
    )?;

    refresh_balances(conn)?;
    Ok(acc)
}

#[cfg(test)]
mod tests;
