//! Standing orders (Daueraufträge, #239): what the bank reports is stored per
//! account, replaced on every complete fetch, and expanded into the coming
//! executions for the "Geplant" page. Read-only; nothing is sent from here.

use jiff::civil::Date;
use rusqlite::{Connection, OptionalExtension, params};

use crate::connectors::{RemoteStandingOrder, StandingOrderJob};
use crate::dates::parse_iso;
use crate::error::CoreResult;
use crate::models::StandingOrderAction;
use crate::planned::{PlannedKind, PlannedPayment};
use crate::vault::Vault;

/// Without an end date a period shows at most this many executions per order
/// (a weekly order would otherwise fill the page).
const MAX_OPEN_EXECUTIONS: usize = 12;
/// How far ahead the search for those executions goes.
const MAX_AHEAD_DAYS: i64 = 366;

const STANDING_ORDERS: &str = "
CREATE TABLE IF NOT EXISTS standing_orders (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id      INTEGER NOT NULL REFERENCES accounts(id),
    key             TEXT    NOT NULL,                 -- bank order id, else the content
    order_id        TEXT    NOT NULL DEFAULT '',
    recipient_name  TEXT    NOT NULL DEFAULT '',
    recipient_iban  TEXT    NOT NULL DEFAULT '',
    amount_cents    INTEGER NOT NULL,                 -- positive = outgoing
    purpose         TEXT    NOT NULL DEFAULT '',
    first_date      TEXT    NOT NULL,
    last_date       TEXT,
    time_unit       TEXT    NOT NULL,                 -- M | W
    interval        INTEGER NOT NULL,
    exec_day        INTEGER NOT NULL,
    can_change      INTEGER NOT NULL DEFAULT 0,
    can_delete      INTEGER NOT NULL DEFAULT 0,
    fetched_ms      INTEGER NOT NULL,
    UNIQUE(account_id, key)
);
";

/// Step 18: the table (guarded like step 17: old vaults are rebuilt from the current schema).
pub(crate) fn standing_orders_table(conn: &Connection) -> CoreResult<()> {
    conn.execute_batch(STANDING_ORDERS)?;
    Ok(())
}

fn key_of(o: &RemoteStandingOrder) -> String {
    if o.order_id.is_empty() {
        format!(
            "{}|{}|{}|{}|{}{}|{}",
            o.recipient_iban,
            o.amount_cents,
            o.first_date,
            o.time_unit,
            o.interval,
            o.exec_day,
            o.purpose
        )
    } else {
        o.order_id.clone()
    }
}

/// Replaces the stored standing orders of `account_id` with `orders`.
pub(crate) fn replace_for_account(
    conn: &Connection,
    account_id: i64,
    orders: &[&RemoteStandingOrder],
) -> CoreResult<()> {
    conn.execute(
        "DELETE FROM standing_orders WHERE account_id = ?1",
        [account_id],
    )?;
    for o in orders {
        conn.execute(
            "INSERT OR REPLACE INTO standing_orders(account_id, key, order_id, recipient_name,
                    recipient_iban, amount_cents, purpose, first_date, last_date, time_unit,
                    interval, exec_day, can_change, can_delete, fetched_ms)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
            params![
                account_id,
                key_of(o),
                o.order_id,
                o.recipient_name,
                o.recipient_iban,
                o.amount_cents,
                o.purpose,
                o.first_date,
                o.last_date,
                o.time_unit,
                o.interval,
                o.exec_day,
                o.can_change,
                o.can_delete,
                crate::dates::now_ms()
            ],
        )?;
    }
    Ok(())
}

struct Row {
    id: i64,
    account_id: i64,
    account_name: String,
    name: String,
    iban: String,
    cents: i64,
    purpose: String,
    first: Date,
    last: Option<Date>,
    unit: String,
    interval: u32,
    exec_day: u32,
}

/// Executions of the stored standing orders within `[from, to]`
/// (`to` = `None`: the next [`MAX_OPEN_EXECUTIONS`] of each order), for the given
/// accounts (empty = all).
pub(crate) fn planned_executions(
    conn: &Connection,
    from: Date,
    to: Option<Date>,
    account_ids: &[i64],
) -> CoreResult<Vec<PlannedPayment>> {
    let open_ended = to.is_none();
    let to =
        to.unwrap_or_else(|| crate::dates::add_days(from, MAX_AHEAD_DAYS).unwrap_or(Date::MAX));
    let mut q = conn.prepare(
        "SELECT s.id, s.account_id, a.name, s.recipient_name, s.recipient_iban, s.amount_cents,
                s.purpose, s.first_date, s.last_date, s.time_unit, s.interval, s.exec_day
         FROM standing_orders s JOIN accounts a ON a.id = s.account_id
         WHERE a.deleted_ms IS NULL",
    )?;
    let rows = q.query_map([], |r| {
        let date = |s: String| parse_iso(&s);
        Ok((
            Row {
                id: r.get(0)?,
                account_id: r.get(1)?,
                account_name: r.get(2)?,
                name: r.get(3)?,
                iban: r.get(4)?,
                cents: r.get(5)?,
                purpose: r.get(6)?,
                first: Date::MIN,
                last: None,
                unit: r.get(9)?,
                interval: r.get(10)?,
                exec_day: r.get(11)?,
            },
            date(r.get(7)?),
            r.get::<_, Option<String>>(8)?.and_then(date),
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (mut o, first, last) = row?;
        let Some(first) = first else { continue };
        o.first = first;
        o.last = last;
        if !account_ids.is_empty() && !account_ids.contains(&o.account_id) {
            continue;
        }
        let mut dates = occurrences(&o, from, to);
        if open_ended {
            dates.truncate(MAX_OPEN_EXECUTIONS);
        }
        for d in dates {
            out.push(PlannedPayment {
                id: format!("standing-{}-{d}", o.id),
                kind: PlannedKind::StandingOrder,
                account_id: o.account_id,
                account_name: o.account_name.clone(),
                date: d.to_string(),
                counterparty: o.name.clone(),
                counterparty_iban: o.iban.clone(),
                amount_cents: -o.cents,
                purpose: o.purpose.clone(),
                rhythm_unit: o.unit.clone(),
                rhythm_interval: o.interval,
            });
        }
    }
    Ok(out)
}

/// Execution dates of one order within `[from, to]`. Month orders run on the
/// execution day (31 = last day; days a month lacks fall on its last day),
/// week orders on that weekday (1 = Monday); never before the first date or
/// after the last one.
fn occurrences(o: &Row, from: Date, to: Date) -> Vec<Date> {
    let end = o.last.map_or(to, |l| l.min(to));
    let step = o.interval.max(1) as i64;
    let mut out = Vec::new();
    if o.unit == "W" {
        // First date and interval come from the bank: checked steps end the
        // list at the end of jiff's range (9999-12-31) instead of panicking.
        let back = i64::from(o.first.weekday().to_monday_zero_offset());
        let Some(monday) = crate::dates::add_days(o.first, -back) else {
            return out;
        };
        let Some(exec) = crate::dates::add_days(monday, o.exec_day.clamp(1, 7) as i64 - 1) else {
            return out;
        };
        // start just before `from`: an old order must not run out of the 1000-step cap
        let k0 = (crate::dates::days_between(monday, from) / (7 * step) - 1).max(0);
        for k in k0..k0 + 1000 {
            let Some(d) = crate::dates::add_days(exec, 7 * step * k) else {
                break;
            };
            if d > end {
                break;
            }
            if d >= o.first && d >= from {
                out.push(d);
            }
        }
    } else {
        let first_months = i64::from(o.first.year()) * 12 + i64::from(o.first.month()) - 1;
        let from_months = i64::from(from.year()) * 12 + i64::from(from.month()) - 1;
        let k0 = ((from_months - first_months) / step - 1).max(0);
        for k in k0..k0 + 1000 {
            let months = first_months + step * k;
            let (y, m) = ((months / 12) as i32, (months % 12) as u32 + 1);
            let Some(d) = crate::dates::clamped_ymd(y, m, o.exec_day.clamp(1, 31)) else {
                continue;
            };
            if d > end {
                break;
            }
            if d >= o.first && d >= from {
                out.push(d);
            }
        }
    }
    out
}

/// A stored standing order as the bank reported it (#496): what the editor shows
/// and what a change or deletion refers to.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct StandingOrderInfo {
    /// Row id, the handle for `StandingOrderRequest::order_row_id`.
    pub id: i64,
    pub account_id: i64,
    pub account_name: String,
    /// The bank's order id; empty if it did not report one.
    pub order_id: String,
    pub recipient_name: String,
    pub recipient_iban: String,
    /// Positive = outgoing.
    pub amount_cents: i64,
    pub purpose: String,
    pub first_date: String,
    pub last_date: Option<String>,
    pub time_unit: String,
    pub interval: u32,
    pub exec_day: u32,
    pub can_change: bool,
    pub can_delete: bool,
}

const INFO_COLUMNS: &str = "s.id, s.account_id, a.name, s.order_id, s.recipient_name,
        s.recipient_iban, s.amount_cents, s.purpose, s.first_date, s.last_date, s.time_unit,
        s.interval, s.exec_day, s.can_change, s.can_delete";

fn info_from(r: &rusqlite::Row) -> rusqlite::Result<StandingOrderInfo> {
    Ok(StandingOrderInfo {
        id: r.get(0)?,
        account_id: r.get(1)?,
        account_name: r.get(2)?,
        order_id: r.get(3)?,
        recipient_name: r.get(4)?,
        recipient_iban: r.get(5)?,
        amount_cents: r.get(6)?,
        purpose: r.get(7)?,
        first_date: r.get(8)?,
        last_date: r.get(9)?,
        time_unit: r.get(10)?,
        interval: r.get(11)?,
        exec_day: r.get(12)?,
        can_change: r.get::<_, i64>(13)? != 0,
        can_delete: r.get::<_, i64>(14)? != 0,
    })
}

/// Whether `first` is a day the schedule runs on: the execution day of the month (31 = last
/// day; a day beyond the month's end means its last day) or the weekday (1 = Monday).
pub(crate) fn first_date_fits(first: Date, unit: &str, exec_day: u32) -> bool {
    if unit == "W" {
        return first.weekday().to_monday_one_offset() as u32 == exec_day;
    }
    let month_end = first.days_in_month() as u32;
    let day = first.day() as u32;
    if exec_day >= 31 {
        return day == month_end;
    }
    day == exec_day || (exec_day > month_end && day == month_end)
}

#[uniffi::export]
impl Vault {
    /// The stored standing orders of the given accounts (empty = all), oldest
    /// first. Only what the last bank fetch reported, plus optimistic changes
    /// from [`Vault::begin_standing_order`] until the next fetch.
    pub fn list_standing_orders(
        &self,
        account_ids: Vec<i64>,
    ) -> Result<Vec<StandingOrderInfo>, crate::error::CoreError> {
        let db = self.db()?;
        let mut q = db.prepare(&format!(
            "SELECT {INFO_COLUMNS} FROM standing_orders s JOIN accounts a ON a.id = s.account_id
             WHERE a.deleted_ms IS NULL ORDER BY s.first_date, s.id"
        ))?;
        let rows = q.query_map([], info_from)?;
        let mut out = Vec::new();
        for o in rows {
            let o = o?;
            if account_ids.is_empty() || account_ids.contains(&o.account_id) {
                out.push(o);
            }
        }
        Ok(out)
    }

    /// What the account's bank allows for a new or changed standing order, from the
    /// parameters it sent at the last synchronisation. `None` when it stated none
    /// (not a FinTS account, or not synchronised since this was added): the form
    /// then offers everything and the check happens when sending.
    pub fn standing_order_limits(
        &self,
        account_id: i64,
        action: crate::models::StandingOrderAction,
    ) -> Result<Option<crate::models::StandingOrderLimits>, crate::error::CoreError> {
        let db = self.db()?;
        let state: Option<crate::secret::SecretJson> = db
            .query_row(
                "SELECT c.state_json FROM accounts a JOIN bank_connections c ON c.id = a.connection_id
                 WHERE a.id = ?1 AND a.deleted_ms IS NULL AND c.deleted_ms IS NULL",
                [account_id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(state) = state else {
            return Ok(None);
        };
        Ok(crate::connectors::fints::standing_limits_from_state(
            &state, action,
        ))
    }
}

impl Vault {
    /// One stored order by its row id.
    pub(crate) fn standing_order(&self, row_id: i64) -> CoreResult<Option<StandingOrderInfo>> {
        let db = self.db()?;
        let mut q = db.prepare(&format!(
            "SELECT {INFO_COLUMNS} FROM standing_orders s JOIN accounts a ON a.id = s.account_id
             WHERE s.id = ?1 AND a.deleted_ms IS NULL"
        ))?;
        let mut rows = q.query_map([row_id], info_from)?;
        Ok(rows.next().transpose()?)
    }

    /// After the bank accepted the order: mirror it locally until the next fetch
    /// replaces the account's orders. A new order is not editable before that
    /// (the bank's order id is not known yet).
    pub(crate) fn record_standing_order(
        &self,
        account_id: i64,
        job: &StandingOrderJob,
    ) -> CoreResult<()> {
        let db = self.db()?;
        match job.action {
            StandingOrderAction::Delete => {
                db.execute(
                    "DELETE FROM standing_orders WHERE account_id = ?1 AND order_id = ?2",
                    params![account_id, job.order_id],
                )?;
            }
            StandingOrderAction::Change => {
                db.execute(
                    "UPDATE standing_orders SET recipient_name = ?3, recipient_iban = ?4,
                            amount_cents = ?5, purpose = ?6, first_date = ?7, last_date = ?8,
                            time_unit = ?9, interval = ?10, exec_day = ?11
                     WHERE account_id = ?1 AND order_id = ?2",
                    params![
                        account_id,
                        job.order_id,
                        job.recipient_name,
                        job.recipient_iban,
                        job.amount_cents,
                        job.purpose,
                        job.first_date,
                        job.last_date,
                        job.time_unit,
                        job.interval,
                        job.exec_day
                    ],
                )?;
            }
            StandingOrderAction::Create => {
                let o = RemoteStandingOrder {
                    recipient_name: job.recipient_name.clone(),
                    recipient_iban: job.recipient_iban.clone(),
                    amount_cents: job.amount_cents,
                    purpose: job.purpose.clone(),
                    first_date: job.first_date.clone(),
                    last_date: job.last_date.clone(),
                    time_unit: job.time_unit.clone(),
                    interval: job.interval,
                    exec_day: job.exec_day,
                    ..Default::default()
                };
                db.execute(
                    "INSERT OR REPLACE INTO standing_orders(account_id, key, order_id, recipient_name,
                            recipient_iban, amount_cents, purpose, first_date, last_date, time_unit,
                            interval, exec_day, can_change, can_delete, fetched_ms)
                     VALUES (?1,?2,'',?3,?4,?5,?6,?7,?8,?9,?10,?11,0,0,?12)",
                    params![
                        account_id,
                        key_of(&o),
                        o.recipient_name,
                        o.recipient_iban,
                        o.amount_cents,
                        o.purpose,
                        o.first_date,
                        o.last_date,
                        o.time_unit,
                        o.interval,
                        o.exec_day,
                        crate::dates::now_ms()
                    ],
                )?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod day_tests {
    use super::*;

    #[test]
    fn first_date_must_match_the_execution_day() {
        let d = |y, m, day| crate::dates::ymd(y, m, day).unwrap();
        assert!(first_date_fits(d(2026, 12, 3), "M", 3));
        assert!(!first_date_fits(d(2026, 11, 16), "M", 3));
        assert!(first_date_fits(d(2027, 2, 28), "M", 31), "31 = last day");
        assert!(!first_date_fits(d(2027, 2, 27), "M", 31));
        assert!(
            first_date_fits(d(2027, 2, 28), "M", 30),
            "30 in a short month"
        );
        assert!(first_date_fits(d(2026, 11, 30), "M", 31));
        assert!(first_date_fits(d(2026, 11, 16), "W", 1), "a Monday");
        assert!(!first_date_fits(d(2026, 11, 17), "W", 1));
        assert!(first_date_fits(d(2026, 11, 22), "W", 7), "a Sunday");
    }
}
