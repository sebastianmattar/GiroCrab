//! Value history of the positions of a depot (#578), read from the raw observations with the
//! same day rule as the derived depot totals ([`super::observations::day_values`]).

use std::collections::{BTreeMap, HashMap, HashSet};

use jiff::civil::Date;
use rusqlite::{Connection, params};

use super::observations::{
    self, KIND_POSITION, Observed, day_key, day_text, day_values, position_key, source_of,
    source_rank,
};
use crate::error::{CoreError, CoreResult};
use crate::models::DepotValuePoint;
use crate::vault::Vault;

/// A position of a depot with a value history, sold ones included.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ValuePosition {
    /// ISIN, `cash` or `name:…` (the key [`Vault::position_value_history`] takes).
    pub position: String,
    /// Latest known name.
    pub name: String,
    /// Currency of the series: the account's, else the one reported latest.
    pub currency: String,
    pub first_date: String,
    pub last_date: String,
    /// Value on `last_date` (0 = sold).
    pub last_value_cents: i64,
    /// Part of the depot's current holdings; otherwise sold or no longer reported.
    pub held: bool,
}

/// One value per day ([`day_key`]): the best source of each day ([`source_rank`]).
fn best_per_day(rows: &[Observed]) -> Vec<(u32, i64)> {
    let mut days: BTreeMap<u32, (u8, i64)> = BTreeMap::new();
    for d in day_values(rows) {
        let rank = source_rank(d.source);
        match days.get(&d.day) {
            Some((r, _)) if *r <= rank => {}
            _ => {
                days.insert(d.day, (rank, d.value_cents));
            }
        }
    }
    days.into_iter().map(|(d, (_, v))| (d, v)).collect()
}

fn account_currency(conn: &Connection, account_id: i64) -> CoreResult<String> {
    conn.query_row(
        "SELECT currency FROM accounts WHERE id = ?1",
        [account_id],
        |r| r.get(0),
    )
    .map_err(|_| CoreError::NotFound {
        what: crate::i18n::trf("Konto {0}", &[&account_id]),
    })
}

/// The currency a position's series is shown in: the account's when the position has rows in
/// it, else the one reported latest (a position valued in another currency).
fn position_currency(
    conn: &Connection,
    account_id: i64,
    position: &str,
    account_currency: &str,
) -> CoreResult<Option<String>> {
    let mut q = conn.prepare_cached(
        "SELECT currency FROM depot_observations
         WHERE account_id = ?1 AND kind = 'position' AND position = ?2
         ORDER BY currency = ?3 DESC, observed_ms DESC LIMIT 1",
    )?;
    let mut rows = q.query(params![account_id, position, account_currency])?;
    Ok(match rows.next()? {
        Some(r) => Some(r.get(0)?),
        None => None,
    })
}

/// Days from `first` to `end` with the last known value carried on.
fn carried(values: &[(u32, i64)], end: Date) -> Vec<DepotValuePoint> {
    let Some(first) = values
        .first()
        .and_then(|(d, _)| Date::strptime("%Y-%m-%d", day_text(*d)).ok())
    else {
        return vec![];
    };
    let mut out = Vec::new();
    let mut next = 0;
    let mut current = 0;
    let mut day = first;
    while day <= end {
        let date = crate::dates::iso(day);
        let key = day_key(&date);
        while next < values.len() && values[next].0 <= key {
            current = values[next].1;
            next += 1;
        }
        out.push(DepotValuePoint {
            date,
            value_cents: current,
            covered_depots: 1,
            total_depots: 1,
        });
        let Ok(next) = day.tomorrow() else { break };
        day = next;
    }
    out
}

/// The rows of one (position, currency) of a depot, read in one pass over all of them.
struct Group {
    position: String,
    currency: String,
    rows: Vec<Observed>,
    /// latest observation and the name it gave
    latest_ms: i64,
    name: String,
}

/// Keys of the positions a depot holds now (`holdings`).
pub(crate) fn held_keys(conn: &Connection, account_id: i64) -> CoreResult<HashSet<String>> {
    let mut q = conn.prepare_cached("SELECT isin, name FROM holdings WHERE account_id = ?1")?;
    let rows = q.query_map([account_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut keys = HashSet::new();
    for row in rows {
        let (isin, name) = row?;
        keys.extend(position_key(&isin, &name));
    }
    Ok(keys)
}

pub(crate) fn positions(conn: &Connection, account_id: i64) -> CoreResult<Vec<ValuePosition>> {
    let own = account_currency(conn, account_id)?;
    let held = held_keys(conn, account_id)?;
    // per position the account's currency, else the one reported latest
    let mut best: HashMap<String, ((bool, i64), ValuePosition)> = HashMap::new();
    let mut finish = |g: Group| {
        let days = best_per_day(&g.rows);
        let (Some(first), Some(last)) = (days.first(), days.last()) else {
            return;
        };
        let rank = (g.currency == own, g.latest_ms);
        if best.get(&g.position).is_some_and(|(r, _)| *r >= rank) {
            return;
        }
        let p = ValuePosition {
            held: held.contains(&g.position),
            position: g.position.clone(),
            name: g.name,
            currency: g.currency,
            first_date: day_text(first.0),
            last_date: day_text(last.0),
            last_value_cents: last.1,
        };
        best.insert(g.position, (rank, p));
    };
    // all position rows in one pass (the table's own order), a string only where a group starts
    // or a name changes
    let mut q = conn.prepare(
        "SELECT position, currency, value_date, source, observed_ms, value_cents, confirmed, name
         FROM depot_observations WHERE account_id = ?1 AND kind = 'position'
         ORDER BY position, currency, source, value_date, observed_ms",
    )?;
    let mut rows = q.query([account_id])?;
    let mut group: Option<Group> = None;
    while let Some(r) = rows.next()? {
        let text = |i| r.get_ref(i)?.as_str().map_err(rusqlite::Error::from);
        let (position, currency) = (text(0)?, text(1)?);
        if group
            .as_ref()
            .is_none_or(|g| g.position != position || g.currency != currency)
        {
            if let Some(done) = group.take() {
                finish(done);
            }
            group = Some(Group {
                position: position.to_string(),
                currency: currency.to_string(),
                rows: Vec::new(),
                latest_ms: i64::MIN,
                name: String::new(),
            });
        }
        let g = group.as_mut().expect("set above");
        let ms: i64 = r.get(4)?;
        if ms >= g.latest_ms {
            g.latest_ms = ms;
            let name = text(7)?;
            if !name.is_empty() && name != g.name {
                g.name = name.to_string();
            }
        }
        g.rows.push(Observed {
            day: day_key(text(2)?),
            source: source_of(text(3)?),
            observed_ms: ms,
            value_cents: r.get(5)?,
            confirmed: r.get(6)?,
        });
    }
    if let Some(done) = group {
        finish(done);
    }
    let mut out: Vec<ValuePosition> = best.into_values().map(|(_, p)| p).collect();
    out.sort_by_key(|p| p.name.to_lowercase());
    Ok(out)
}

pub(crate) fn history(
    conn: &Connection,
    account_id: i64,
    position: &str,
) -> CoreResult<Vec<DepotValuePoint>> {
    let own = account_currency(conn, account_id)?;
    let Some(currency) = position_currency(conn, account_id, position, &own)? else {
        return Ok(vec![]);
    };
    let rows = observations::observed(conn, account_id, KIND_POSITION, position, &currency, None)?;
    let days = best_per_day(&rows);
    let today = crate::dates::today();
    // A held position carries on to today. A sold one ends with its sale (a 0), one no longer held
    // with the last day a source reported it (its value after that is unknown).
    let end = match days.last() {
        Some((d, v)) if *v == 0 || !held_keys(conn, account_id)?.contains(position) => {
            Date::strptime("%Y-%m-%d", day_text(*d)).unwrap_or(today)
        }
        _ => today,
    };
    Ok(carried(&days, end))
}

#[uniffi::export]
impl Vault {
    /// Positions of a depot that have a value history, sold ones included (`held` = false),
    /// sorted by name.
    pub fn depot_value_positions(&self, account_id: i64) -> Result<Vec<ValuePosition>, CoreError> {
        positions(&*self.db()?, account_id)
    }

    /// Daily value of one position of a depot ([`ValuePosition::position`]) from its first value
    /// on, the last value carried on (to today, or to the sale of a sold position). Empty
    /// without a history.
    pub fn position_value_history(
        &self,
        account_id: i64,
        position: String,
    ) -> Result<Vec<DepotValuePoint>, CoreError> {
        history(&*self.db()?, account_id, &position)
    }
}
