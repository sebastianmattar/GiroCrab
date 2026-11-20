//! Valuation history of securities depots: one value per depot and day.
//!
//! A depot has no cash bookings to walk back over, and the bank reports only today's valuation.
//! So each fetch leaves a point: the depot value the bank reported, stamped with its valuation
//! date. The history therefore starts with the first fetch (or an imported history) and never
//! claims a past it did not see: before its first point a depot's value is **unknown** (it is not
//! 0 and not the first value). A confirmed value of 0 (a depot that was sold off) is a point like
//! any other. Between two points the last value carries on (no interpolation). Values are the
//! reported depot value, so a change in them may also come from purchases, sales and transfers:
//! it is no return. A sum over several depots says per day how many of them were covered.
//!
//! Two layers (#578): [`observations`] keeps what a fetch or an import delivered
//! (`depot_observations`, totals and positions, merged between devices as a union).
//! `depot_values` is derived from it on each device by [`rebuild`]: one value per depot, day and
//! source, which [`series`] and the charts read.

use std::collections::HashMap;

use jiff::civil::Date;
use rusqlite::{Connection, OptionalExtension, params};

use crate::error::CoreResult;

pub(crate) mod observations;
pub(crate) mod positions;

/// Step 29. `depot_observations` is the raw layer ([`observations`]): one row per value a
/// fetch (`source = 'fetch'`) or an import (`'import'`) delivered, for the depot total or a
/// position, never edited. It is stored in the order its readers walk it (one series, by
/// source, day and time), so a series or all positions of a depot are one range scan. `depot_values` is derived from it ([`rebuild`]) and not merged: per
/// depot, valuation day and source one value with a quality (`observed`, `reconstructed`,
/// `estimated`; `prices` and the last two are reserved for computed values), and the
/// observation it came from (`observed_ms`). Several sources of a day live side by side;
/// [`series`] shows the best one. Both disappear with their account (`ON DELETE CASCADE`).
pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS depot_observations (
    account_id   INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    kind         TEXT    NOT NULL,             -- total | position
    position     TEXT    NOT NULL DEFAULT '',  -- '' (total) | ISIN | 'cash' | 'name:<normalised>'
    currency     TEXT    NOT NULL,
    source       TEXT    NOT NULL,             -- fetch | import
    value_date   TEXT    NOT NULL,             -- YYYY-MM-DD, the bank's valuation day
    observed_ms  INTEGER NOT NULL,             -- retrieval time; import: the source's timestamp
    source_ref   TEXT    NOT NULL DEFAULT '',  -- import: the source's row of the position
    date_assumed INTEGER NOT NULL DEFAULT 0,   -- 1 = no valuation date, the retrieval day
    value_cents  INTEGER NOT NULL,
    confirmed    INTEGER NOT NULL DEFAULT 0,   -- 1 = the inventory was delivered: a 0 is real
    quantity     REAL,                         -- fetch only
    price_cents  INTEGER,                      -- fetch only
    name         TEXT    NOT NULL DEFAULT '',  -- name of the position at that time
    PRIMARY KEY (account_id, kind, position, currency, source, value_date, observed_ms, source_ref)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS depot_values (
    account_id  INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    value_date  TEXT    NOT NULL,                    -- YYYY-MM-DD, the valuation day
    source      TEXT    NOT NULL DEFAULT 'fetch',    -- fetch | import | prices
    quality     TEXT    NOT NULL DEFAULT 'observed', -- observed | reconstructed | estimated
    value_cents INTEGER NOT NULL,
    observed_ms INTEGER NOT NULL DEFAULT 0,          -- the observation it came from
    PRIMARY KEY (account_id, value_date, source)
) WITHOUT ROWID;
";

pub(crate) const QUALITY_OBSERVED: &str = "observed";

/// A securities depot whose values are derived: the one place that decides which accounts have
/// a value history (a Sachwerte account has its own value series).
pub(crate) struct DepotRow {
    pub currency: String,
    pub balance_cents: i64,
    pub balance_date: String,
}

/// The depot `account_id`, `None` for any other (or a deleted) account.
pub(crate) fn depot_row(conn: &Connection, account_id: i64) -> CoreResult<Option<DepotRow>> {
    let row = conn
        .prepare_cached(
            "SELECT currency, balance_cents, balance_date FROM accounts
             WHERE id = ?1 AND kind = 'brokerage' AND deleted_ms IS NULL",
        )?
        .query_row([account_id], |r| {
            Ok(DepotRow {
                currency: r.get(0)?,
                balance_cents: r.get(1)?,
                balance_date: r.get(2)?,
            })
        })
        .optional()?;
    match row {
        Some(_) if crate::assets::is_tangible(conn, account_id)? => Ok(None),
        row => Ok(row),
    }
}

/// Derives `depot_values` of a depot from its total observations in the account currency
/// (values in another currency stay out): all days, or only `fetch_day` of the fetch source
/// (a fetch needs no more: its rule looks at one day only, see [`observations::day_values`]).
pub(crate) fn rebuild(
    conn: &Connection,
    account_id: i64,
    fetch_day: Option<&str>,
) -> CoreResult<()> {
    let Some(DepotRow { currency, .. }) = depot_row(conn, account_id)? else {
        conn.execute(
            "DELETE FROM depot_values WHERE account_id = ?1",
            [account_id],
        )?;
        return Ok(());
    };
    let rows = observations::observed(
        conn,
        account_id,
        observations::KIND_TOTAL,
        "",
        &currency,
        fetch_day,
    )?;
    match fetch_day {
        Some(day) => {
            conn.execute(
                "DELETE FROM depot_values WHERE account_id = ?1 AND value_date = ?2 AND source = 'fetch'",
                params![account_id, day],
            )?;
        }
        None => {
            conn.execute(
                "DELETE FROM depot_values WHERE account_id = ?1",
                [account_id],
            )?;
        }
    }
    let mut ins = conn.prepare_cached(
        "INSERT OR REPLACE INTO depot_values(account_id, value_date, source, quality, value_cents, observed_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for d in observations::day_values(&rows) {
        ins.execute(params![
            account_id,
            observations::day_text(d.day),
            d.source,
            QUALITY_OBSERVED,
            d.value_cents,
            d.observed_ms
        ])?;
    }
    Ok(())
}

/// Records what a fetch reported for a depot and derives that day (see
/// [`observations::record_fetch`]).
pub(crate) fn record_fetch(
    conn: &Connection,
    account_id: i64,
    holdings: Option<&[&crate::models::Holding]>,
    previous: &[String],
    balance_known: bool,
    observed_ms: i64,
) -> CoreResult<()> {
    let day = observations::record_fetch(
        conn,
        account_id,
        holdings,
        previous,
        balance_known,
        observed_ms,
    )?;
    if let Some(day) = day {
        rebuild(conn, account_id, Some(&day))?;
    }
    Ok(())
}

/// The history of `from` goes to `to` (an imported twin folded into the bank's account, a
/// duplicate account merged away): its observations move, both are derived again.
pub(crate) fn move_account(conn: &Connection, from: i64, to: i64) -> CoreResult<()> {
    conn.execute(
        "UPDATE OR IGNORE depot_observations SET account_id = ?1 WHERE account_id = ?2",
        params![to, from],
    )?;
    // left over: rows `to` holds already
    conn.execute(
        "DELETE FROM depot_observations WHERE account_id = ?1",
        [from],
    )?;
    conn.execute("DELETE FROM depot_values WHERE account_id = ?1", [from])?;
    rebuild(conn, to, None)
}

/// Vault merge: adds the other vault's observations of mapped accounts and derives the depots
/// that gained some again.
pub(crate) fn merge_attached(conn: &Connection, acc_map: &HashMap<i64, i64>) -> CoreResult<()> {
    for id in observations::merge_attached(conn, acc_map)? {
        rebuild(conn, id, None)?;
    }
    Ok(())
}

/// Per-day values of the depots among `ids` that have a history, summed, for `start..=end`.
/// `None` when none has a point. A depot counts from its first point on; before that it adds
/// nothing and is not counted as covered.
pub(crate) struct Series {
    /// Sum of the known depot values per day (depots not yet covered add nothing).
    pub values: Vec<i64>,
    /// Per day: how many depots had a value (a confirmed 0 counts).
    pub covered: Vec<u32>,
    /// Securities depots among `ids`, with or without a history. The chart reads it next to
    /// `covered` ("2 of 3 depots") in `Vault::depot_value_history`.
    pub total_depots: u32,
    /// Per depot with at least one point its first one (day, value), possibly before `start`: the
    /// caller leaves these depots out of its own walk back over bookings, and a chart without
    /// coverage (`balance_history`) carries the value back instead of letting the depot jump in.
    pub firsts: Vec<(i64, String, i64)>,
}

pub(crate) fn series(
    conn: &Connection,
    ids: &[i64],
    start: Date,
    end: Date,
) -> CoreResult<Option<Series>> {
    if ids.is_empty() || end < start {
        return Ok(None);
    }
    let days = crate::dates::days_between(start, end) as usize + 1;
    let mut total = vec![0_i64; days];
    let mut covered_days = vec![0_u32; days];
    let mut firsts = Vec::new();
    let mut total_depots = 0_u32;
    let mut q = conn.prepare(
        "SELECT value_date, value_cents FROM depot_values
         WHERE account_id = ?1 AND value_date <= ?2
         ORDER BY value_date,
                  CASE quality WHEN 'observed' THEN 0 WHEN 'reconstructed' THEN 1 ELSE 2 END,
                  CASE source WHEN 'fetch' THEN 0 WHEN 'import' THEN 1 ELSE 2 END",
    )?;
    let last = crate::dates::iso(end);
    // The same day keys for every depot: format them once.
    let keys: Vec<String> = start
        .series(crate::dates::days(1))
        .take(days)
        .map(crate::dates::iso)
        .collect();
    for &id in ids {
        // another account (a former depot keeps stale values until it is derived again) has none
        let Some(depot) = depot_row(conn, id)? else {
            continue;
        };
        total_depots += 1;
        let mut points: Vec<(String, i64)> = q
            .query_map(params![id, last], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        // the best source of a day comes first (see the ORDER BY)
        points.dedup_by(|later, first| later.0 == first.0);
        if points.is_empty() {
            // A depot that was never recorded (a vault from before the history) is known from its
            // reported balance on: one value on its valuation day, unknown before it. Its balance
            // is never carried backwards over the days it was not seen.
            match reported_point(conn, id, &depot, end)? {
                Some(point) => points.push(point),
                None => continue,
            }
        }
        firsts.push((id, points[0].0.clone(), points[0].1));
        let mut next = 0;
        let mut current: Option<i64> = None;
        for (i, day) in keys.iter().enumerate() {
            while next < points.len() && points[next].0 <= *day {
                current = Some(points[next].1);
                next += 1;
            }
            if let Some(v) = current {
                total[i] += v;
                covered_days[i] += 1;
            }
        }
    }
    Ok((!firsts.is_empty()).then_some(Series {
        values: total,
        covered: covered_days,
        total_depots,
        firsts,
    }))
}

/// The balance a depot reports, as one point (valuation day or `end`), for a depot that has no
/// recorded value and no bookings. A balance of 0 is not a reported value.
fn reported_point(
    conn: &Connection,
    id: i64,
    depot: &DepotRow,
    end: Date,
) -> CoreResult<Option<(String, i64)>> {
    if depot.balance_cents == 0 {
        return Ok(None);
    }
    // An account with bookings keeps the walk back over them: that history is real. Only a depot
    // without any would show nothing but today's value carried backwards.
    let booked: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM transactions WHERE account_id = ?1 AND deleted_ms IS NULL)",
        [id],
        |r| r.get(0),
    )?;
    if booked {
        return Ok(None);
    }
    // a missing, malformed or future date (a bank's value date) is the end of the series
    let (day, _) = observations::valuation_day(&depot.balance_date, end);
    Ok(Some((day, depot.balance_cents)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::AccountKind;
    use crate::vault::Vault;

    fn open(name: &str) -> (crate::test_support::TempDir, std::sync::Arc<Vault>) {
        let dir = crate::test_support::TempDir::new(name);
        let v = Vault::open(
            dir.join("v.db").to_string_lossy().to_string(),
            vec![7u8; 32],
        )
        .unwrap();
        (dir, v)
    }

    fn depot(v: &Vault, balance: i64, date: &str) -> i64 {
        let db = v.db().unwrap();
        db.execute(
            "INSERT INTO accounts(group_id, name, kind, balance_cents, balance_date, currency)
             VALUES ((SELECT COALESCE(MIN(id), 1) FROM account_groups), 'D', ?1, ?2, ?3, 'EUR')",
            params![AccountKind::Brokerage.as_str(), balance, date],
        )
        .or_else(|_| {
            db.execute(
                "INSERT INTO account_groups(name, sf_symbol, sort_order) VALUES ('G', 'x', 0)",
                [],
            )?;
            db.execute(
                "INSERT INTO accounts(group_id, name, kind, balance_cents, balance_date, currency)
                 VALUES (last_insert_rowid(), 'D', ?1, ?2, ?3, 'EUR')",
                params![AccountKind::Brokerage.as_str(), balance, date],
            )
        })
        .unwrap();
        db.last_insert_rowid()
    }

    /// One confirmed fetch observation of the depot total on `date`, derived.
    fn observe(db: &Connection, id: i64, date: &str, cents: i64, ms: i64) {
        observe_from(db, id, date, cents, ms, observations::SOURCE_FETCH);
    }

    fn observe_from(db: &Connection, id: i64, date: &str, cents: i64, ms: i64, source: &str) {
        let row = observations::ObservedValue {
            kind: observations::KIND_TOTAL,
            position: String::new(),
            source_ref: String::new(),
            name: String::new(),
            currency: "EUR".into(),
            observed_ms: ms,
            value_date: date.into(),
            value_cents: cents,
        };
        observations::record_as(db, id, source, &[row]).unwrap();
        rebuild(db, id, None).unwrap();
    }

    /// A derived value as such (sources and qualities no observation produces yet).
    fn derived(db: &Connection, id: i64, date: &str, cents: i64, source: &str, quality: &str) {
        db.execute(
            "INSERT OR REPLACE INTO depot_values(account_id, value_date, source, quality, value_cents)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, date, source, quality, cents],
        )
        .unwrap();
    }

    fn day(s: &str) -> Date {
        Date::strptime("%Y-%m-%d", s).unwrap()
    }

    #[test]
    fn the_later_fetch_of_a_day_replaces_the_earlier_one() {
        let (_d, v) = open("depot-values-record");
        let id = depot(&v, 100_000, "2026-03-01");
        let db = v.db().unwrap();
        record_fetch(&db, id, None, &[], true, 1).unwrap();
        db.execute(
            "UPDATE accounts SET balance_cents = 120_000 WHERE id = ?1",
            [id],
        )
        .unwrap();
        record_fetch(&db, id, None, &[], true, 2).unwrap();
        // both fetches stay observed, the later one is the value of the day
        let observed: i64 = db
            .query_row("SELECT count(*) FROM depot_observations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(observed, 2);
        let rows: Vec<(String, i64)> = db
            .prepare("SELECT value_date, value_cents FROM depot_values WHERE account_id = ?1")
            .unwrap()
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(rows, vec![("2026-03-01".to_string(), 120_000)]);
    }

    #[test]
    fn an_unconfirmed_zero_and_other_accounts_leave_no_point() {
        let (_d, v) = open("depot-values-skip");
        let empty = depot(&v, 0, "2026-03-01");
        let db = v.db().unwrap();
        record_fetch(&db, empty, None, &[], true, 1).unwrap();
        db.execute(
            "UPDATE accounts SET kind = 'checking', balance_cents = 5 WHERE id = ?1",
            [empty],
        )
        .unwrap();
        record_fetch(&db, empty, Some(&[]), &[], true, 2).unwrap();
        let n: i64 = db
            .query_row("SELECT count(*) FROM depot_values", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn a_confirmed_zero_is_a_point_and_ends_the_last_positive_value() {
        let (_d, v) = open("depot-values-zero");
        let id = depot(&v, 50_000, "2026-03-02");
        let db = v.db().unwrap();
        record_fetch(&db, id, Some(&[]), &[], true, 1).unwrap();
        db.execute(
            "UPDATE accounts SET balance_cents = 0, balance_date = '2026-03-05' WHERE id = ?1",
            [id],
        )
        .unwrap();
        record_fetch(&db, id, Some(&[]), &[], true, 2).unwrap();
        let s = series(&db, &[id], day("2026-03-02"), day("2026-03-07"))
            .unwrap()
            .unwrap();
        assert_eq!(s.values, vec![50_000, 50_000, 50_000, 0, 0, 0]);
        assert_eq!(s.covered, vec![1; 6], "a confirmed zero counts as covered");
    }

    #[test]
    fn values_carry_forward_and_stay_unknown_before_the_first_point() {
        let (_d, v) = open("depot-values-series");
        let id = depot(&v, 130_000, "2026-03-10");
        let db = v.db().unwrap();
        observe(&db, id, "2026-03-03", 100_000, 1);
        observe(&db, id, "2026-03-07", 110_000, 2);
        observe(&db, id, "2026-03-10", 130_000, 3);
        let s = series(&db, &[id], day("2026-03-01"), day("2026-03-10"))
            .unwrap()
            .unwrap();
        assert_eq!(
            s.values,
            vec![
                0, 0, 100_000, 100_000, 100_000, 100_000, 110_000, 110_000, 110_000, 130_000
            ]
        );
        assert_eq!(s.covered, vec![0, 0, 1, 1, 1, 1, 1, 1, 1, 1]);
        assert_eq!(s.total_depots, 1);
        assert_eq!(s.firsts.iter().map(|f| f.0).collect::<Vec<_>>(), vec![id]);
        assert!(
            series(&db, &[id + 99], day("2026-03-01"), day("2026-03-10"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn a_second_depot_counts_from_its_first_point_and_the_coverage_says_so() {
        let (_d, v) = open("depot-values-coverage");
        let a = depot(&v, 100, "2026-03-04");
        let b = depot(&v, 40, "2026-03-04");
        let none = depot(&v, 0, "2026-03-04");
        let db = v.db().unwrap();
        observe(&db, a, "2026-03-01", 100, 1);
        observe(&db, b, "2026-03-03", 40, 1);
        let s = series(&db, &[a, b, none], day("2026-03-01"), day("2026-03-04"))
            .unwrap()
            .unwrap();
        assert_eq!(s.values, vec![100, 100, 140, 140]);
        assert_eq!(s.covered, vec![1, 1, 2, 2]);
        assert_eq!(
            s.total_depots, 3,
            "a depot without any point is still a depot"
        );
        assert_eq!(s.firsts.iter().map(|f| f.0).collect::<Vec<_>>(), vec![a, b]);
    }

    #[test]
    fn the_history_uses_the_depot_values_even_when_the_balance_differs() {
        let (_d, v) = open("depot-values-history");
        let today = crate::dates::today();
        let f = crate::dates::iso;
        // the balance (100) differs from the last point (120), as after a device merge
        let id = depot(&v, 100_00, &f(today));
        {
            let db = v.db().unwrap();
            observe(&db, id, &f(today - crate::dates::days(1)), 80_00, 1);
            observe(&db, id, &f(today), 120_00, 2);
        }
        let h = v.balance_history(vec![id], 5).unwrap();
        let values: Vec<i64> = h.iter().map(|p| p.balance_cents).collect();
        // starts on the first known day: no invented past, no shifted line
        assert_eq!(values, vec![80_00, 120_00]);
    }

    #[test]
    fn a_depot_without_history_is_known_only_from_its_reported_balance_on() {
        let (_d, v) = open("depot-values-unrecorded");
        let a = depot(&v, 100, "2026-03-04"); // never recorded, balance reported on the 4th
        let b = depot(&v, 40, "2026-03-04");
        let db = v.db().unwrap();
        observe(&db, b, "2026-03-01", 40, 1);
        let s = series(&db, &[a, b], day("2026-03-01"), day("2026-03-05"))
            .unwrap()
            .unwrap();
        // before the 4th only the recorded depot is known; today's balance is not carried back
        assert_eq!(s.values, vec![40, 40, 40, 140, 140]);
        assert_eq!(s.covered, vec![1, 1, 1, 2, 2]);
        assert_eq!(s.total_depots, 2);
    }

    #[test]
    fn a_depot_without_history_does_not_jump_into_the_total() {
        let (_d, v) = open("depot-values-total");
        let today = crate::dates::today();
        let f = |n: i64| crate::dates::iso(today - crate::dates::days(n));
        let a = depot(&v, 100, &f(0)); // no recorded value
        let b = depot(&v, 40, &f(0));
        {
            let db = v.db().unwrap();
            observe(&db, b, &f(3), 40, 1);
        }
        let h = v.balance_history(vec![a, b], 5).unwrap();
        let values: Vec<i64> = h.iter().map(|p| p.balance_cents).collect();
        // the unrecorded depot (100, reported today) stays at its value instead of jumping in
        // today: the total chart cannot mark a partial sum
        assert_eq!(values, vec![140, 140, 140, 140]);
    }

    #[test]
    fn a_depot_walks_back_over_its_bookings_before_its_first_value() {
        let (_d, v) = open("depot-values-bookings");
        let today = crate::dates::today();
        let f = |n: i64| crate::dates::iso(today - crate::dates::days(n));
        let id = depot(&v, 1_000, &f(0));
        {
            let db = v.db().unwrap();
            // a deposit three days ago, then the first fetched value yesterday
            db.execute(
                "INSERT INTO transactions(account_id, external_id, booking_date, value_date,
                        amount_cents, currency, counterparty, purpose)
                 VALUES (?1, 'b1', ?2, ?2, 300, 'EUR', '', '')",
                params![id, f(3)],
            )
            .unwrap();
            observe(&db, id, &f(1), 1_000, 1);
        }
        let h = v.balance_history(vec![id], 5).unwrap();
        let values: Vec<i64> = h.iter().map(|p| p.balance_cents).collect();
        // before the deposit (end of day -4) 700, from it 1 000: the booking history stays real
        assert_eq!(values, vec![700, 1_000, 1_000, 1_000, 1_000]);
    }

    #[test]
    fn a_future_balance_date_counts_as_today() {
        let (_d, v) = open("depot-values-future");
        let today = crate::dates::today();
        let ahead = crate::dates::iso(today + crate::dates::days(2));
        let id = depot(&v, 10_000, &ahead);
        let h = v.depot_value_history(vec![id], 5).unwrap();
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].value_cents, 10_000);
    }

    #[test]
    fn a_former_depot_leaves_the_history_with_its_kind() {
        let (_d, v) = open("depot-values-kind");
        let today = crate::dates::today();
        let f = |n: i64| crate::dates::iso(today - crate::dates::days(n));
        let id = depot(&v, 500, &f(0));
        observe(&v.db().unwrap(), id, &f(2), 400, 1);
        assert_eq!(v.depot_value_history(vec![id], 5).unwrap().len(), 3);
        v.set_account_kind(id, AccountKind::Checking).unwrap();
        assert!(v.depot_value_history(vec![id], 5).unwrap().is_empty());
        let n: i64 = v
            .db()
            .unwrap()
            .query_row("SELECT count(*) FROM depot_values", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "derived again: no stale values");
        // back to a depot: the history counts again
        v.set_account_kind(id, AccountKind::Brokerage).unwrap();
        assert_eq!(v.depot_value_history(vec![id], 5).unwrap().len(), 3);
    }

    #[test]
    fn the_best_source_of_a_day_is_shown_and_the_others_stay() {
        let (_d, v) = open("depot-values-sources");
        let id = depot(&v, 1, "2026-03-03");
        let db = v.db().unwrap();
        observe_from(&db, id, "2026-03-01", 950, 1, "import");
        observe_from(&db, id, "2026-03-03", 990, 3, "import");
        observe(&db, id, "2026-03-03", 1000, 2);
        derived(&db, id, "2026-03-01", 900, "prices", "estimated");
        derived(&db, id, "2026-03-02", 970, "prices", "reconstructed");
        derived(&db, id, "2026-03-02", 960, "prices", "estimated");
        let s = series(&db, &[id], day("2026-03-01"), day("2026-03-03"))
            .unwrap()
            .unwrap();
        // observed beats reconstructed beats estimated; a fetch beats an import of the same quality
        assert_eq!(s.values, vec![950, 960, 1000]);
        let n: i64 = db
            .query_row("SELECT count(*) FROM depot_values", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 5, "nothing is overwritten across sources");
    }

    #[test]
    fn the_depot_history_says_how_many_depots_each_day_covers() {
        let (_d, v) = open("depot-values-api");
        let today = crate::dates::today();
        let f = |n: i64| crate::dates::iso(today - crate::dates::days(n));
        let a = depot(&v, 100, &f(0));
        let b = depot(&v, 40, &f(0));
        {
            let db = v.db().unwrap();
            observe(&db, a, &f(3), 100, 1);
            observe(&db, b, &f(1), 40, 1);
        }
        let h = v.depot_value_history(vec![], 10).unwrap();
        let shape: Vec<(i64, u32, u32)> = h
            .iter()
            .map(|p| (p.value_cents, p.covered_depots, p.total_depots))
            .collect();
        // starts on the first known day; the second depot joins on day -1
        assert_eq!(h.first().unwrap().date, f(3));
        assert_eq!(
            shape,
            vec![(100, 1, 2), (100, 1, 2), (140, 2, 2), (140, 2, 2)]
        );
        let only_b = v.depot_value_history(vec![b], 10).unwrap();
        assert_eq!(only_b.len(), 2);
        assert_eq!(only_b[0].total_depots, 1);
        assert!(v.depot_value_history(vec![a + 99], 10).unwrap().is_empty());
    }

    #[test]
    fn the_latest_observation_of_a_day_wins_in_whatever_order_it_arrives() {
        let (_d, v) = open("depot-values-merge");
        let id = depot(&v, 1, "2026-03-01");
        let db = v.db().unwrap();
        let value = || -> i64 {
            db.query_row(
                "SELECT value_cents FROM depot_values WHERE account_id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap()
        };
        observe(&db, id, "2026-03-01", 100, 50);
        // an older observation (another device's, merged later) changes nothing
        observe(&db, id, "2026-03-01", 90, 40);
        assert_eq!(value(), 100);
        observe(&db, id, "2026-03-01", 120, 60);
        assert_eq!(value(), 120);
    }
}
