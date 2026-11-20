//! Running balance in the booking list (GiroCrab/GiroCrab#577): the balance
//! after each booking and at the end of each day, so imports and syncs can be
//! checked against the bank statement.
//!
//! Like `balance_history` it walks back from the current balance, so the
//! newest value is the sidebar balance and each step is one booking. It always
//! covers every booking of the chosen accounts, whatever the list filter hides.
//! Within a day the walk follows the list order (`id`), which need not be the
//! bank's; the end-of-day value does not depend on it.
//!
//! Which bookings move the balance (`counts_sql`) follows how it is kept: a
//! balance from the bank excludes pending bookings and is as of `balance_date`
//! (later bookings are added); a computed one (`balance_derived`) counts
//! pending bookings, only the account currency and, with an opening balance
//! (#212), only bookings from its date on. Before that date the balance is
//! unknown: those days get none.

use std::collections::HashMap;

use rusqlite::{Connection, params, params_from_iter};

use super::PENDING_SQL;
use crate::error::CoreError;
use crate::models::{Booking, BookingBalance, DailyBalance};
use crate::opening_balance::DERIVED_SQL;
use crate::vault::accounts::IN_TOTAL_SQL;

/// Tables `counts_sql` reads: a booking `t`, its account `a` and its opening balance `o`.
pub(crate) const COUNTED_FROM: &str = "FROM transactions t JOIN accounts a ON a.id = t.account_id
     LEFT JOIN opening_balances o ON o.account_id = a.id AND o.opening_cents IS NOT NULL";

/// SQL condition over `COUNTED_FROM`: booking `t` moves the balance of its account.
pub(crate) fn counts_sql() -> String {
    format!(
        "(CASE WHEN {DERIVED_SQL}
              THEN (t.currency = '' OR upper(t.currency) = upper(a.currency))
                   AND (o.opening_date IS NULL OR o.opening_date = '' OR t.booking_date >= o.opening_date)
              ELSE NOT ({PENDING_SQL}) END)"
    )
}

/// Sum of the booked (not pending) bookings after a bank balance's date: what
/// moves that balance to today. 0 without a full date.
pub(crate) fn booked_after(
    db: &Connection,
    account_id: i64,
    balance_date: &str,
) -> Result<i64, CoreError> {
    let date = balance_date.trim();
    if date.len() != 10 {
        return Ok(0);
    }
    Ok(db.query_row(
        &format!(
            "SELECT COALESCE(SUM(t.amount_cents), 0) FROM transactions t
              WHERE t.account_id = ?1 AND t.booking_date > ?2 AND t.deleted_ms IS NULL AND NOT ({PENDING_SQL})"
        ),
        params![account_id, date],
        |r| r.get(0),
    )?)
}

/// Per booking day the sum of the bookings of `ids` after `after` (up to `until`) that move their
/// balances: the same ones as in the list's running balance (#577), so a computed balance counts
/// pending ones and starts at its opening date.
pub(crate) fn booked_per_day(
    db: &Connection,
    ids: &[i64],
    after: &str,
    until: Option<&str>,
) -> Result<HashMap<String, i64>, CoreError> {
    if ids.is_empty() {
        return Ok(Default::default());
    }
    let list = (0..ids.len())
        .map(|n| format!("?{}", n + 3))
        .collect::<Vec<_>>()
        .join(",");
    let mut stmt = db.prepare(&format!(
        "SELECT t.booking_date, SUM(t.amount_cents) {COUNTED_FROM}
          WHERE t.account_id IN ({list}) AND t.booking_date > ?1 AND (?2 IS NULL OR t.booking_date <= ?2)
            AND t.deleted_ms IS NULL AND {}
          GROUP BY t.booking_date",
        counts_sql()
    ))?;
    let args: Vec<rusqlite::types::Value> =
        [after.to_string().into(), until.map(str::to_string).into()]
            .into_iter()
            .chain(ids.iter().map(|&i| i.into()))
            .collect();
    let rows = stmt.query_map(rusqlite::params_from_iter(args), |r| {
        Ok((r.get(0)?, r.get(1)?))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// What the walk needs of one chosen account.
struct Chosen {
    id: i64,
    /// Current balance, moved to today for a bank balance of an older date.
    balance: i64,
    /// Opening date of a computed balance; `""` = none or before the first booking.
    opening: String,
}

/// Accounts the balance sums, as in `balance_history`: one account in any
/// currency, several (or all counted ones) only in EUR. Depots and Sachwerte
/// (market value) and loans (anchored outstanding amount) are left out: their
/// balance does not follow their bookings. Reads only the account rows, not
/// the counts `list_accounts` adds.
fn chosen(db: &Connection, ids: &[i64]) -> Result<Vec<Chosen>, CoreError> {
    let excluded = crate::user_groups::excluded_from_total(db)?;
    let mut stmt = db.prepare_cached(&format!(
        "SELECT a.id, a.kind = 'credit_card', a.currency = 'EUR', a.balance_cents, a.balance_date,
                {DERIVED_SQL}, o.opening_cents IS NOT NULL,
                CASE WHEN {DERIVED_SQL} THEN IFNULL(o.opening_date, '') ELSE '' END,
                {IN_TOTAL_SQL}, a.user_group_uuid
           FROM accounts a
           LEFT JOIN opening_balances o ON o.account_id = a.id AND o.opening_cents IS NOT NULL
           LEFT JOIN user_groups g ON g.uuid = a.user_group_uuid AND g.deleted_ms IS NULL
          WHERE a.deleted_ms IS NULL AND a.kind NOT IN ('brokerage', 'loan')"
    ))?;
    let single = ids.len() == 1;
    let mut out = Vec::new();
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        let id: i64 = r.get(0)?;
        let wanted = if ids.is_empty() {
            r.get::<_, bool>(8)?
                && !r
                    .get::<_, Option<String>>(9)?
                    .is_some_and(|g| excluded.contains(&g))
        } else {
            ids.contains(&id)
        };
        if !wanted || !(single || r.get::<_, bool>(2)?) {
            continue;
        }
        let (derived, has_opening): (bool, bool) = (r.get(5)?, r.get(6)?);
        let mut balance: i64 = r.get(3)?;
        // as `list_accounts`: a card stored with balance 0 shows its open amount
        if r.get::<_, bool>(1)? && balance == 0 && !(derived && has_opening) {
            balance = crate::connectors::card_open_balance(db, id)?.unwrap_or(0);
        }
        if !derived {
            balance += booked_after(db, id, &r.get::<_, String>(4)?)?;
        }
        out.push(Chosen {
            id,
            balance,
            opening: r.get(7)?,
        });
    }
    Ok(out)
}

/// Balances for one page of `list_transactions` (sorted newest first), read
/// under the same lock as the page. Only the page's days are walked: what is
/// newer comes from one SQL sum, so deep pages stay as cheap as the first.
pub(crate) fn page_balances(
    db: &Connection,
    account_ids: &[i64],
    page: &[Booking],
) -> Result<(Vec<BookingBalance>, Vec<DailyBalance>), CoreError> {
    let (Some(newest), Some(oldest)) = (page.first(), page.last()) else {
        return Ok((Vec::new(), Vec::new()));
    };
    let (newest, oldest) = (newest.booking_date.as_str(), oldest.booking_date.as_str());
    let chosen = chosen(db, account_ids)?;
    if chosen.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    // before the latest opening date the sum misses that account's balance
    let known_from = chosen
        .iter()
        .map(|c| c.opening.as_str())
        .max()
        .unwrap_or("");

    // `?{first}`, `?{first+1}`, … for the chosen account ids
    let ids = |first: usize| {
        (0..chosen.len())
            .map(|n| format!("?{}", n + first))
            .collect::<Vec<_>>()
            .join(",")
    };
    let id_args = || chosen.iter().map(|c| rusqlite::types::Value::from(c.id));
    let counts = counts_sql();
    // balance at the end of the page's newest day
    let newer: i64 = db.query_row(
        &format!(
            "SELECT COALESCE(SUM(t.amount_cents), 0) {COUNTED_FROM}
              WHERE t.account_id IN ({}) AND t.booking_date > ?1 AND t.deleted_ms IS NULL AND {counts}",
            ids(2)
        ),
        params_from_iter(std::iter::once(newest.to_string().into()).chain(id_args())),
        |r| r.get(0),
    )?;
    let mut running = chosen.iter().map(|c| c.balance).sum::<i64>() - newer;

    let mut stmt = db.prepare(&format!(
        "SELECT t.id, t.booking_date, t.amount_cents, {PENDING_SQL}, {counts}
           {COUNTED_FROM}
          WHERE t.account_id IN ({}) AND t.booking_date >= ?1 AND t.booking_date <= ?2
            AND t.deleted_ms IS NULL
          ORDER BY t.booking_date DESC, t.id DESC",
        ids(3)
    ))?;
    let args: Vec<rusqlite::types::Value> = [oldest, newest]
        .into_iter()
        .map(|d| d.to_string().into())
        .chain(id_args())
        .collect();
    let mut rows = stmt.query(params_from_iter(args))?;

    let mut days: Vec<&str> = page.iter().map(|b| b.booking_date.as_str()).collect();
    days.dedup();
    let known = |date: &str| date >= known_from;
    let mut day_balances = Vec::new();
    let mut end_of = |day: &str, cents: i64| {
        if known(day) {
            day_balances.push(DailyBalance {
                date: day.to_string(),
                balance_cents: cents,
            });
        }
    };
    let mut after: HashMap<i64, i64> = HashMap::new();
    let mut next_day = 0;
    while let Some(r) = rows.next()? {
        let date: String = r.get(1)?;
        // a day's end: before its own bookings are walked back
        while next_day < days.len() && days[next_day] >= date.as_str() {
            end_of(days[next_day], running);
            next_day += 1;
        }
        if !r.get::<_, bool>(3)? {
            after.insert(r.get(0)?, running);
        }
        if r.get::<_, bool>(4)? {
            running -= r.get::<_, i64>(2)?;
        }
    }
    for day in &days[next_day..] {
        end_of(day, running);
    }

    let balances = page
        .iter()
        .filter(|b| known(&b.booking_date))
        .filter_map(|b| {
            after.get(&b.id).map(|&cents| BookingBalance {
                booking_id: b.id,
                balance_cents: cents,
            })
        })
        .collect();
    Ok((balances, day_balances))
}

#[cfg(test)]
mod tests;
