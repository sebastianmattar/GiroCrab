//! The raw layer of the depot value history (#578): what a source delivered.
//!
//! A row of `depot_observations` is one value a fetch or an import reported: the depot total
//! (`kind = 'total'`) or one position (`kind = 'position'`), at `observed_ms`, dated with the
//! bank's valuation day (`value_date`, the retrieval day when the source named none:
//! `date_assumed`). Rows are never edited, so the vault merge is a union, and `depot_values`
//! is derived from them on each device ([`super::rebuild`]).
//!
//! A fetch writes a row only when something changed against the latest fetch row of that
//! position or when its valuation day is new, so a day without rows means no fetch saw it. An
//! import stores its rows as delivered, duplicates and zeros included; [`day_values`] resolves
//! them, for the derived totals and for the position charts alike.
//!
//! Positions are keyed by ISIN, `cash` for the depot's cash balance or `name:<normalised name>`
//! as the last resort; the key outlives the row in `holdings`, so sold positions keep their
//! history.

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension, params};

use crate::dates::iso;
use crate::error::CoreResult;
use crate::models::Holding;

pub(crate) const KIND_TOTAL: &str = "total";
pub(crate) const KIND_POSITION: &str = "position";
pub(crate) const SOURCE_FETCH: &str = "fetch";
pub(crate) const SOURCE_IMPORT: &str = "import";
const CASH: &str = "cash";

/// The key of a position (the apps get it through `ffi.rs`), `None` without ISIN and name.
pub(crate) fn position_key(isin: &str, name: &str) -> Option<String> {
    // only a valid ISIN: placeholders ("-", "N/A") or a WKN would join different securities
    if let Some(isin) = crate::asset_classification::normalized_isin(isin) {
        return Some(isin);
    }
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    let low = name.to_lowercase();
    match low.as_str() {
        "" => None,
        "liquidität" | "liquiditaet" | "liquidity" | "cash" => Some(CASH.into()),
        _ => Some(format!("name:{low}")),
    }
}

/// The valuation day of a fetch: the bank's date, unless it is missing, malformed or in the
/// future; then the retrieval day (`true` = assumed).
pub(crate) fn valuation_day(balance_date: &str, today: jiff::civil::Date) -> (String, bool) {
    match jiff::civil::Date::strptime("%Y-%m-%d", balance_date.trim()) {
        Ok(d) if d <= today => (iso(d), false),
        _ => (iso(today), true),
    }
}

/// One value of a fetch, before the change check.
struct Value<'a> {
    kind: &'a str,
    position: &'a str,
    name: &'a str,
    value_cents: i64,
    quantity: Option<f64>,
    price_cents: Option<i64>,
}

/// Where a fetch stores its values: the depot, its currency and the valuation day.
struct FetchStamp<'a> {
    account_id: i64,
    currency: &'a str,
    observed_ms: i64,
    value_date: &'a str,
    date_assumed: bool,
    confirmed: bool,
}

/// Writes `v` unless the latest fetch row of its key says the same for the same day.
fn write_if_changed(conn: &Connection, s: &FetchStamp, v: &Value) -> CoreResult<bool> {
    type Latest = (String, i64, Option<f64>, Option<i64>, bool);
    let prev: Option<Latest> = conn
        .prepare_cached(
            "SELECT value_date, value_cents, quantity, price_cents, confirmed FROM depot_observations
             WHERE account_id = ?1 AND kind = ?2 AND position = ?3 AND currency = ?4 AND source = 'fetch'
             ORDER BY value_date DESC, observed_ms DESC LIMIT 1",
        )?
        .query_row(params![s.account_id, v.kind, v.position, s.currency], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .optional()?;
    if prev.is_some_and(|p| {
        p == (
            s.value_date.to_string(),
            v.value_cents,
            v.quantity,
            v.price_cents,
            s.confirmed,
        )
    }) {
        return Ok(false);
    }
    conn.prepare_cached(
        "INSERT OR IGNORE INTO depot_observations(account_id, kind, position, source, observed_ms,
                value_date, date_assumed, currency, value_cents, confirmed, quantity, price_cents, name)
         VALUES (?1, ?2, ?3, 'fetch', ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
    )?
    .execute(params![
        s.account_id,
        v.kind,
        v.position,
        s.observed_ms,
        s.value_date,
        s.date_assumed,
        s.currency,
        v.value_cents,
        s.confirmed,
        v.quantity,
        v.price_cents,
        v.name
    ])?;
    Ok(true)
}

/// Records what a fetch reported for a securities depot: its value (the account balance just
/// stored) and, when the fetch delivered the inventory (`holdings`, maybe empty), each position.
/// A delivered inventory confirms a value of 0 and closes with a 0 each position of `previous`
/// (the keys held before this fetch) that is missing from it, when its latest value came from a
/// fetch: a position only an import knows is not closed on its behalf. Without an inventory a 0
/// stays unconfirmed (the bank may just not have reported it). A balance of 0 next to positions
/// is taken as their sum. With `balance_known` false the balance is a placeholder: only the
/// positions are recorded, dated today. Returns the valuation day when a total was written.
pub(crate) fn record_fetch(
    conn: &Connection,
    account_id: i64,
    holdings: Option<&[&Holding]>,
    previous: &[String],
    balance_known: bool,
    observed_ms: i64,
) -> CoreResult<Option<String>> {
    let Some(super::DepotRow {
        currency,
        balance_cents: balance,
        balance_date,
    }) = super::depot_row(conn, account_id)?
    else {
        return Ok(None);
    };
    let today = crate::dates::today();
    let (value_date, date_assumed) = if balance_known {
        valuation_day(&balance_date, today)
    } else {
        (iso(today), true)
    };
    let stamp = FetchStamp {
        account_id,
        currency: &currency,
        observed_ms,
        value_date: &value_date,
        date_assumed,
        confirmed: holdings.is_some(),
    };
    // two custody rows of one ISIN are one position
    let mut agg: Vec<(String, Holding)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for h in holdings.unwrap_or_default() {
        let Some(key) = position_key(&h.isin, &h.name) else {
            continue;
        };
        match index.get(&key) {
            Some(&i) => {
                let a = &mut agg[i].1;
                a.quantity += h.quantity;
                a.value_cents += h.value_cents;
                // one price for the sum, so quantity × price keeps matching the value
                if a.quantity != 0.0 {
                    a.current_price_cents = (a.value_cents as f64 / a.quantity).round() as i64;
                }
            }
            None => {
                index.insert(key.clone(), agg.len());
                agg.push((key, (*h).clone()));
            }
        }
    }
    let positions_sum: i64 = agg.iter().map(|(_, h)| h.value_cents).sum();
    let total = if balance == 0 && positions_sum != 0 {
        positions_sum
    } else {
        balance
    };
    let wrote_total = balance_known
        && write_if_changed(
            conn,
            &stamp,
            &Value {
                kind: KIND_TOTAL,
                position: "",
                name: "",
                value_cents: total,
                quantity: None,
                price_cents: None,
            },
        )?;
    for (key, h) in &agg {
        write_if_changed(
            conn,
            &stamp,
            &Value {
                kind: KIND_POSITION,
                position: key,
                name: &h.name,
                value_cents: h.value_cents,
                quantity: Some(h.quantity),
                price_cents: Some(h.current_price_cents),
            },
        )?;
    }
    if holdings.is_some() {
        // a position held before and missing now was sold, when a fetch reported it last
        let mut latest = conn.prepare_cached(
            "SELECT value_cents, source, name FROM depot_observations
             WHERE account_id = ?1 AND kind = 'position' AND position = ?2 AND currency = ?3
             ORDER BY observed_ms DESC LIMIT 1",
        )?;
        for key in previous {
            if index.contains_key(key) {
                continue;
            }
            let last: Option<(i64, String, String)> = latest
                .query_row(params![account_id, key, currency], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })
                .optional()?;
            let Some((value, source, name)) = last else {
                continue;
            };
            if value == 0 || source != SOURCE_FETCH {
                continue;
            }
            write_if_changed(
                conn,
                &stamp,
                &Value {
                    kind: KIND_POSITION,
                    position: key,
                    name: &name,
                    value_cents: 0,
                    quantity: Some(0.0),
                    price_cents: None,
                },
            )?;
        }
    }
    Ok(wrote_total.then_some(value_date))
}

/// One value as a source delivered it (an imported history, demo data).
pub(crate) struct ObservedValue {
    pub kind: &'static str,
    /// position key, "" for the total
    pub position: String,
    /// the source's own row of the position, keeps two rows of one ISIN apart
    pub source_ref: String,
    pub name: String,
    pub currency: String,
    /// the source's timestamp (ms)
    pub observed_ms: i64,
    /// `YYYY-MM-DD`
    pub value_date: String,
    pub value_cents: i64,
}

/// Stores an imported history as delivered (its zeros count as confirmed). Importing the same
/// data again adds nothing. Returns the rows added.
pub(crate) fn record_import(
    conn: &Connection,
    account_id: i64,
    rows: &[ObservedValue],
) -> CoreResult<u32> {
    record_as(conn, account_id, SOURCE_IMPORT, rows)
}

/// Stores values of `source` as delivered, confirmed and with their own date.
pub(crate) fn record_as(
    conn: &Connection,
    account_id: i64,
    source: &str,
    rows: &[ObservedValue],
) -> CoreResult<u32> {
    let mut ins = conn.prepare_cached(
        "INSERT OR IGNORE INTO depot_observations(account_id, kind, position, source, source_ref,
                observed_ms, value_date, date_assumed, currency, value_cents, confirmed, name)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8, ?9, 1, ?10)",
    )?;
    let mut n = 0;
    for r in rows {
        n += ins.execute(params![
            account_id,
            r.kind,
            r.position,
            source,
            r.source_ref,
            r.observed_ms,
            r.value_date,
            r.currency,
            r.value_cents,
            r.name
        ])? as u32;
    }
    Ok(n)
}

/// Vault merge: adds the other vault's observations of mapped accounts (rows are never edited,
/// so a clash is the same row). Returns the own accounts that gained rows.
pub(crate) fn merge_attached(
    conn: &Connection,
    acc_map: &HashMap<i64, i64>,
) -> CoreResult<HashSet<i64>> {
    let mut touched = HashSet::new();
    let has: i64 = conn.query_row(
        "SELECT count(*) FROM other.sqlite_master WHERE type = 'table' AND name = 'depot_observations'",
        [],
        |r| r.get(0),
    )?;
    if has == 0 {
        return Ok(touched);
    }
    let mut q = conn.prepare(
        "SELECT account_id, kind, position, source, source_ref, observed_ms, value_date,
                date_assumed, currency, value_cents, confirmed, quantity, price_cents, name
         FROM other.depot_observations",
    )?;
    let mut ins = conn.prepare(
        "INSERT OR IGNORE INTO depot_observations(account_id, kind, position, source, source_ref,
                observed_ms, value_date, date_assumed, currency, value_cents, confirmed, quantity,
                price_cents, name)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
    )?;
    let mut rows = q.query([])?;
    while let Some(r) = rows.next()? {
        let Some(&acc) = acc_map.get(&r.get::<_, i64>(0)?) else {
            continue;
        };
        let added = ins.execute(params![
            acc,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, i64>(5)?,
            r.get::<_, String>(6)?,
            r.get::<_, bool>(7)?,
            r.get::<_, String>(8)?,
            r.get::<_, i64>(9)?,
            r.get::<_, bool>(10)?,
            r.get::<_, Option<f64>>(11)?,
            r.get::<_, Option<i64>>(12)?,
            r.get::<_, String>(13)?,
        ])?;
        if added > 0 {
            touched.insert(acc);
        }
    }
    Ok(touched)
}

/// A day as a number (`YYYYMMDD`), so a long series is read without a string per row; 0 when `s`
/// is no `YYYY-MM-DD`. Orders like the text.
pub(crate) fn day_key(s: &str) -> u32 {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return 0;
    }
    let mut n = 0u32;
    for &c in b.iter().filter(|c| **c != b'-') {
        if !c.is_ascii_digit() {
            return 0;
        }
        n = n * 10 + u32::from(c - b'0');
    }
    n
}

/// `YYYY-MM-DD` of a [`day_key`].
pub(crate) fn day_text(day: u32) -> String {
    format!(
        "{:04}-{:02}-{:02}",
        day / 10_000,
        day / 100 % 100,
        day % 100
    )
}

/// The source of a row as one of the known ones (no string per row).
pub(crate) fn source_of(s: &str) -> &'static str {
    match s {
        SOURCE_FETCH => SOURCE_FETCH,
        SOURCE_IMPORT => SOURCE_IMPORT,
        _ => "other",
    }
}

/// One observation as [`day_values`] reads it.
pub(crate) struct Observed {
    /// [`day_key`] of the valuation day
    pub day: u32,
    pub source: &'static str,
    pub observed_ms: i64,
    pub value_cents: i64,
    pub confirmed: bool,
}

/// The value of one day from one source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DayValue {
    /// [`day_key`]
    pub day: u32,
    pub source: &'static str,
    pub value_cents: i64,
    /// the observation it came from
    pub observed_ms: i64,
}

/// The value of each (day, source) of one series (a depot total or one position).
/// `rows` sorted by source, day and `observed_ms`.
/// - `fetch`: the latest observation of the day; a 0 only when it was confirmed.
/// - `import`: the largest non-zero value of the day (a source may list one ISIN in several rows,
///   e.g. a position sold and bought again); a 0 only after a positive value, and only the first
///   of a run of zeros (a closed depot keeps reporting 0). Not the sum: in a real database the
///   largest value matched the source's own depot total on 62 % of the days with such rows, the
///   sum on 31 %; most extra rows are leftovers of an earlier holding, not a second lot.
pub(crate) fn day_values(rows: &[Observed]) -> Vec<DayValue> {
    let mut out: Vec<DayValue> = Vec::new();
    let mut i = 0;
    // per source: a value was seen, and the last value kept
    let mut series_source = "";
    let mut import_seen_value = false;
    let mut import_last: Option<i64> = None;
    while i < rows.len() {
        let mut j = i;
        while j < rows.len() && rows[j].source == rows[i].source && rows[j].day == rows[i].day {
            j += 1;
        }
        let day = &rows[i..j];
        let source = rows[i].source;
        if source != series_source {
            series_source = source;
            import_seen_value = false;
            import_last = None;
        }
        let picked = if source == SOURCE_FETCH {
            day.iter().rev().find(|o| o.value_cents != 0 || o.confirmed)
        } else {
            let best = day
                .iter()
                .filter(|o| o.value_cents != 0)
                .max_by_key(|o| (o.value_cents, o.observed_ms))
                .or_else(|| day.iter().rev().find(|o| o.confirmed));
            match best {
                Some(o) if o.value_cents != 0 => {
                    import_seen_value = true;
                    Some(o)
                }
                Some(o) if import_seen_value && import_last != Some(0) => Some(o),
                _ => None,
            }
        };
        if let Some(o) = picked.filter(|o| o.day != 0) {
            if source != SOURCE_FETCH {
                import_last = Some(o.value_cents);
            }
            out.push(DayValue {
                day: o.day,
                source,
                value_cents: o.value_cents,
                observed_ms: o.observed_ms,
            });
        }
        i = j;
    }
    out
}

/// Rank of a source when two give a value for the same day: the own fetch first.
pub(crate) fn source_rank(source: &str) -> u8 {
    match source {
        SOURCE_FETCH => 0,
        SOURCE_IMPORT => 1,
        _ => 2,
    }
}

/// Reads the observations of one series of a depot in `currency`, sorted for [`day_values`]:
/// all of them, or only the fetches of `fetch_day` (what a fetch changed).
pub(crate) fn observed(
    conn: &Connection,
    account_id: i64,
    kind: &str,
    position: &str,
    currency: &str,
    fetch_day: Option<&str>,
) -> CoreResult<Vec<Observed>> {
    let mut q;
    let mut rows = match fetch_day {
        None => {
            q = conn.prepare_cached(
                "SELECT value_date, source, observed_ms, value_cents, confirmed FROM depot_observations
                 WHERE account_id = ?1 AND kind = ?2 AND position = ?3 AND currency = ?4
                 ORDER BY source, value_date, observed_ms",
            )?;
            q.query(params![account_id, kind, position, currency])?
        }
        Some(day) => {
            q = conn.prepare_cached(
                "SELECT value_date, source, observed_ms, value_cents, confirmed FROM depot_observations
                 WHERE account_id = ?1 AND kind = ?2 AND position = ?3 AND currency = ?4
                   AND source = 'fetch' AND value_date = ?5
                 ORDER BY observed_ms",
            )?;
            q.query(params![account_id, kind, position, currency, day])?
        }
    };
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        let text = |i| r.get_ref(i)?.as_str().map_err(rusqlite::Error::from);
        out.push(Observed {
            day: day_key(text(0)?),
            source: source_of(text(1)?),
            observed_ms: r.get(2)?,
            value_cents: r.get(3)?,
            confirmed: r.get(4)?,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
