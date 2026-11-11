//! Wiederkehrende Buchungen (GitHub #35): detects recurring bookings (rent,
//! subscriptions, insurance, salary, …) from the booking history, lets the
//! user mark a booking as recurring with an interval (incl. custom dates such
//! as "every 15.03. and 15.08.") and shows which fixed costs of a month are
//! already booked and which are still due.
//!
//! Bookings are grouped by a stable *series key* (direction + counterparty
//! IBAN, else merchant). Detection looks at the gaps between the bookings of
//! a group; user decisions live in `recurring_series`.

use std::collections::HashMap;

use chrono::{Datelike, Duration, Local, Months, NaiveDate};
use rusqlite::{params, Connection, OptionalExtension};

use crate::error::{CoreError, CoreResult};
use crate::i18n::tr;
use crate::merchants::FlowKind;
use crate::models::{Booking, BookingFilter};
use crate::vault::Vault;

pub(crate) const RECURRING_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS recurring_series (
    id            INTEGER PRIMARY KEY,
    series_key    TEXT    NOT NULL UNIQUE,
    status        TEXT    NOT NULL DEFAULT 'manual',   -- manual | ignored
    interval      TEXT    NOT NULL DEFAULT 'monthly',  -- monthly | quarterly | halfyearly | yearly | custom
    custom_dates  TEXT    NOT NULL DEFAULT '',         -- custom: comma-separated MM-DD
    created_at    INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);
";

/// Detection window (two years, so yearly payments are seen twice).
const LOOKBACK_DAYS: i64 = 800;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum RecurrenceInterval {
    Monthly,
    Quarterly,
    HalfYearly,
    Yearly,
    /// Fixed calendar days (`custom_dates`, MM-DD), e.g. 15.03. and 15.08.
    Custom,
}

impl RecurrenceInterval {
    fn as_str(self) -> &'static str {
        match self {
            Self::Monthly => "monthly",
            Self::Quarterly => "quarterly",
            Self::HalfYearly => "halfyearly",
            Self::Yearly => "yearly",
            Self::Custom => "custom",
        }
    }
    fn from_str(s: &str) -> Self {
        match s {
            "quarterly" => Self::Quarterly,
            "halfyearly" => Self::HalfYearly,
            "yearly" => Self::Yearly,
            "custom" => Self::Custom,
            _ => Self::Monthly,
        }
    }
    fn months(self) -> Option<u32> {
        match self {
            Self::Monthly => Some(1),
            Self::Quarterly => Some(3),
            Self::HalfYearly => Some(6),
            Self::Yearly => Some(12),
            Self::Custom => None,
        }
    }
    /// Accepted gap between two bookings (days) and the minimum occurrences.
    fn gap_bounds(self) -> Option<(i64, i64, usize)> {
        match self {
            Self::Monthly => Some((25, 36, 2)),
            Self::Quarterly => Some((82, 100, 3)),
            Self::HalfYearly => Some((170, 195, 2)),
            Self::Yearly => Some((350, 380, 2)),
            Self::Custom => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum RecurringSource {
    /// Found automatically in the booking history.
    Detected,
    /// Marked by the user.
    Manual,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct RecurringSeries {
    /// Identity of the series (for ignore / reset).
    pub series_key: String,
    /// `Booking.recurrence_key` of its bookings.
    pub base_key: String,
    /// Only bookings with a similar amount belong to the series (several
    /// contracts with one payee, e.g. two subscriptions).
    pub amount_specific: bool,
    pub name: String,
    pub category_name: String,
    pub category_symbol: String,
    pub merchant_domain: Option<String>,
    pub account_id: i64,
    pub interval: RecurrenceInterval,
    /// Only for `Custom`: MM-DD, e.g. `["03-15", "08-15"]`.
    pub custom_dates: Vec<String>,
    /// Typical amount (median of the latest bookings; negative = expense).
    pub amount_cents: i64,
    pub last_date: String,
    pub last_booking_id: i64,
    pub next_due: String,
    pub occurrences: u32,
    pub source: RecurringSource,
}

/// A series the user hid with "Nicht wiederkehrend" (can be shown again).
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct IgnoredRecurring {
    pub series_key: String,
    pub name: String,
    /// Latest amount (negative = expense).
    pub amount_cents: i64,
    pub last_date: String,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct RecurringMonthItem {
    pub series: RecurringSeries,
    /// Due date in the month (expected, or actual booking date when booked).
    pub due_date: String,
    pub booked: bool,
    pub booking_id: Option<i64>,
    /// Actual amount when booked, else the expected amount.
    pub amount_cents: i64,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct RecurringMonthOverview {
    pub year: i32,
    pub month: u32,
    pub items: Vec<RecurringMonthItem>,
    /// Expenses still to come this month (positive cents).
    pub open_expense_cents: i64,
    /// Income still to come this month.
    pub open_income_cents: i64,
    /// Recurring expenses already booked this month (positive cents).
    pub booked_expense_cents: i64,
}

fn normalize_iban(iban: &str) -> String {
    iban.chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_uppercase()
}

/// Stable grouping key of a booking: direction + payer/payee. "" = not groupable.
pub fn series_key(b: &Booking) -> String {
    let dir = if b.amount_cents < 0 { '-' } else { '+' };
    if !b.counterparty_iban.trim().is_empty() {
        return format!("{dir}iban:{}", normalize_iban(&b.counterparty_iban));
    }
    if !b.merchant.key.is_empty() {
        return format!("{dir}m:{}", b.merchant.key);
    }
    let name = b.counterparty.trim().to_lowercase();
    if name.is_empty() {
        String::new()
    } else {
        format!("{dir}n:{name}")
    }
}

fn date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}

fn fmt(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

fn median(mut v: Vec<i64>) -> i64 {
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    v[v.len() / 2]
}

/// Parses custom dates ("03-15", "15.03.", "15.3") into sorted, unique MM-DD.
pub fn normalize_custom_dates(input: &[String]) -> Vec<String> {
    let mut out: Vec<String> = input
        .iter()
        .filter_map(|raw| {
            let s = raw.trim().trim_end_matches('.');
            let (m, d) = if let Some((a, b)) = s.split_once('-') {
                (a.parse::<u32>().ok()?, b.parse::<u32>().ok()?)
            } else {
                let (a, b) = s.split_once('.')?;
                (b.parse::<u32>().ok()?, a.parse::<u32>().ok()?)
            };
            // validate against a leap year so 29.02. is accepted
            NaiveDate::from_ymd_opt(2024, m, d)?;
            Some(format!("{m:02}-{d:02}"))
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The custom date (MM-DD) in a given year; Feb 29 falls back to Feb 28.
fn custom_in_year(mmdd: &str, year: i32) -> Option<NaiveDate> {
    let (m, d) = mmdd.split_once('-')?;
    let (m, d) = (m.parse().ok()?, d.parse().ok()?);
    NaiveDate::from_ymd_opt(year, m, d).or_else(|| NaiveDate::from_ymd_opt(year, m, d - 1))
}

/// First due date strictly after `after`.
fn next_after(
    after: NaiveDate,
    interval: RecurrenceInterval,
    custom: &[String],
) -> Option<NaiveDate> {
    match interval.months() {
        Some(m) => after.checked_add_months(Months::new(m)),
        None => (after.year()..=after.year() + 1)
            .flat_map(|y| custom.iter().filter_map(move |c| custom_in_year(c, y)))
            .filter(|d| *d > after)
            .min(),
    }
}

/// Classifies the booking dates of one group; `None` = not regular.
fn detect_interval(dates: &[NaiveDate]) -> Option<RecurrenceInterval> {
    if dates.len() < 2 {
        return None;
    }
    let gaps: Vec<i64> = dates.windows(2).map(|w| (w[1] - w[0]).num_days()).collect();
    let med = median(gaps.clone());
    [
        RecurrenceInterval::Monthly,
        RecurrenceInterval::Quarterly,
        RecurrenceInterval::HalfYearly,
        RecurrenceInterval::Yearly,
    ]
    .into_iter()
    .find(|i| {
        let (lo, hi, min_n) = i.gap_bounds().expect("fixed interval");
        let fitting = gaps.iter().filter(|g| (lo..=hi).contains(*g)).count();
        (lo..=hi).contains(&med) && dates.len() >= min_n && fitting * 4 >= gaps.len() * 3
    })
}

/// Stored user decision for a series key.
#[derive(Debug, Clone)]
struct Stored {
    ignored: bool,
    interval: RecurrenceInterval,
    custom_dates: Vec<String>,
}

fn load_stored(conn: &Connection) -> CoreResult<HashMap<String, Stored>> {
    let mut stmt =
        conn.prepare("SELECT series_key, status, interval, custom_dates FROM recurring_series")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    let mut out = HashMap::new();
    for row in rows {
        let (key, status, interval, custom) = row?;
        out.insert(
            key,
            Stored {
                ignored: status == "ignored",
                interval: RecurrenceInterval::from_str(&interval),
                custom_dates: custom
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect(),
            },
        );
    }
    Ok(out)
}

/// Interval of a group of bookings (date-sorted) when it looks recurring.
fn detect_group(list: &[&Booking]) -> Option<RecurrenceInterval> {
    // one booking per day (split payments / corrections)
    let mut dates: Vec<NaiveDate> = list.iter().filter_map(|b| date(&b.booking_date)).collect();
    dates.dedup();
    let i = detect_interval(&dates)?;
    // amounts must be similar (±25 % of the median)
    let med = median(list.iter().map(|b| b.amount_cents).collect());
    let similar = list
        .iter()
        .filter(|b| (b.amount_cents - med).abs() * 4 <= med.abs())
        .count();
    if similar * 4 < list.len() * 3 {
        return None;
    }
    // Only two monthly bookings: amounts must (almost) match (±5 %).
    if dates.len() < 3
        && i == RecurrenceInterval::Monthly
        && list
            .iter()
            .any(|b| (b.amount_cents - med).abs() * 20 > med.abs())
    {
        return None;
    }
    Some(i)
}

/// Splits one payee's bookings into amount clusters (±10 %), e.g. two
/// subscriptions with the same provider. Each cluster stays date-sorted.
fn amount_clusters<'a>(list: &[&'a Booking]) -> Vec<Vec<&'a Booking>> {
    let mut by_amount: Vec<&Booking> = list.to_vec();
    by_amount.sort_by_key(|b| b.amount_cents.abs());
    let mut clusters: Vec<Vec<&Booking>> = Vec::new();
    for b in by_amount {
        match clusters.last_mut() {
            Some(c)
                if (b.amount_cents - c[0].amount_cents).abs() * 10 <= c[0].amount_cents.abs() =>
            {
                c.push(b)
            }
            _ => clusters.push(vec![b]),
        }
    }
    clusters.retain(|c| c.len() >= 2);
    for c in &mut clusters {
        c.sort_by(|a, b| (&a.booking_date, a.id).cmp(&(&b.booking_date, b.id)));
    }
    clusters
}

/// Does `b` belong to series `s`?
pub fn series_matches(s: &RecurringSeries, b: &Booking) -> bool {
    series_key(b) == s.base_key
        && (!s.amount_specific
            || (b.amount_cents - s.amount_cents).abs() * 100 <= s.amount_cents.abs() * 15)
}

fn make_series(
    key: String,
    base_key: &str,
    list: &[&Booking],
    interval: RecurrenceInterval,
    custom: Vec<String>,
    source: RecurringSource,
    today: NaiveDate,
) -> Option<RecurringSeries> {
    let last = *list.last()?;
    let last_date = date(&last.booking_date)?;
    let next = next_after(last_date, interval, &custom)?;
    // A detected series that missed two cycles has ended (contract cancelled).
    if source == RecurringSource::Detected {
        let cycle = (next - last_date).num_days();
        if (today - last_date).num_days() > cycle * 2 + 7 {
            return None;
        }
    }
    let recent: Vec<i64> = list.iter().rev().take(3).map(|b| b.amount_cents).collect();
    Some(RecurringSeries {
        amount_specific: key != base_key,
        series_key: key,
        base_key: base_key.to_string(),
        name: if last.merchant_name.is_empty() {
            last.counterparty.clone()
        } else {
            last.merchant_name.clone()
        },
        category_name: last.category_name.clone(),
        category_symbol: last.category_symbol.clone(),
        merchant_domain: last.merchant_domain.clone(),
        account_id: last.account_id,
        interval,
        custom_dates: custom,
        amount_cents: median(recent),
        last_date: last.booking_date.clone(),
        last_booking_id: last.id,
        next_due: fmt(next),
        occurrences: list.len() as u32,
        source,
    })
}

/// Builds all series from bookings (any order) and the stored decisions.
fn build_series(
    bookings: &[Booking],
    stored: &HashMap<String, Stored>,
    today: NaiveDate,
) -> Vec<RecurringSeries> {
    let mut groups: HashMap<String, Vec<&Booking>> = HashMap::new();
    for b in bookings {
        if matches!(b.flow, FlowKind::Transfer | FlowKind::Perk) || b.amount_cents == 0 {
            continue;
        }
        let key = series_key(b);
        if !key.is_empty() {
            groups.entry(key).or_default().push(b);
        }
    }
    let mut out = Vec::new();
    for (key, mut list) in groups {
        list.sort_by(|a, b| (&a.booking_date, a.id).cmp(&(&b.booking_date, b.id)));
        match stored.get(&key) {
            Some(s) if s.ignored => {}
            Some(s) => out.extend(make_series(
                key.clone(),
                &key,
                &list,
                s.interval,
                s.custom_dates.clone(),
                RecurringSource::Manual,
                today,
            )),
            None => {
                if let Some(i) = detect_group(&list) {
                    out.extend(make_series(
                        key.clone(),
                        &key,
                        &list,
                        i,
                        Vec::new(),
                        RecurringSource::Detected,
                        today,
                    ));
                    continue;
                }
                for cluster in amount_clusters(&list) {
                    let med = median(cluster.iter().map(|b| b.amount_cents.abs()).collect());
                    let sub = format!("{key}#{med}");
                    if stored.get(&sub).is_some_and(|s| s.ignored) {
                        continue;
                    }
                    if let Some(i) = detect_group(&cluster) {
                        out.extend(make_series(
                            sub,
                            &key,
                            &cluster,
                            i,
                            Vec::new(),
                            RecurringSource::Detected,
                            today,
                        ));
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| (&a.next_due, &a.name).cmp(&(&b.next_due, &b.name)));
    out
}

/// Ignored series that still have bookings in `bookings` (newest first).
fn ignored_series(bookings: &[Booking], stored: &HashMap<String, Stored>) -> Vec<IgnoredRecurring> {
    let mut out: Vec<IgnoredRecurring> = stored
        .iter()
        .filter(|(_, s)| s.ignored)
        .filter_map(|(key, _)| {
            let (base, amount) = match key.split_once('#') {
                Some((b, a)) => (b, a.parse::<i64>().ok()),
                None => (key.as_str(), None),
            };
            let last = bookings
                .iter()
                .filter(|b| series_key(b) == base)
                .filter(|b| amount.is_none_or(|a| (b.amount_cents.abs() - a).abs() * 100 <= a * 15))
                .max_by(|a, b| (&a.booking_date, a.id).cmp(&(&b.booking_date, b.id)))?;
            Some(IgnoredRecurring {
                series_key: key.clone(),
                name: if last.merchant_name.is_empty() {
                    last.counterparty.clone()
                } else {
                    last.merchant_name.clone()
                },
                amount_cents: last.amount_cents,
                last_date: last.booking_date.clone(),
            })
        })
        .collect();
    out.sort_by(|a, b| b.last_date.cmp(&a.last_date).then(a.name.cmp(&b.name)));
    out
}

/// What of `series` falls into the given month: booked bookings or open due dates.
fn month_items(
    series: &[RecurringSeries],
    bookings: &[Booking],
    year: i32,
    month: u32,
) -> RecurringMonthOverview {
    let mut ov = RecurringMonthOverview {
        year,
        month,
        items: Vec::new(),
        open_expense_cents: 0,
        open_income_cents: 0,
        booked_expense_cents: 0,
    };
    let Some(start) = NaiveDate::from_ymd_opt(year, month, 1) else {
        return ov;
    };
    let end = start
        .checked_add_months(Months::new(1))
        .map(|d| d - Duration::days(1))
        .unwrap_or(start);
    let prefix = format!("{year:04}-{month:02}");
    let in_month: Vec<&Booking> = bookings
        .iter()
        .filter(|b| b.booking_date.starts_with(&prefix))
        .collect();
    for s in series {
        let booked: Vec<&&Booking> = in_month.iter().filter(|b| series_matches(s, b)).collect();
        if !booked.is_empty() {
            for b in booked {
                ov.items.push(RecurringMonthItem {
                    series: s.clone(),
                    due_date: b.booking_date.clone(),
                    booked: true,
                    booking_id: Some(b.id),
                    amount_cents: b.amount_cents,
                });
                if b.amount_cents < 0 {
                    ov.booked_expense_cents += -b.amount_cents;
                }
            }
            continue;
        }
        let Some(mut due) = date(&s.last_date) else {
            continue;
        };
        // step forward from the last booking into the month (bounded loop)
        for _ in 0..400 {
            let Some(n) = next_after(due, s.interval, &s.custom_dates) else {
                break;
            };
            due = n;
            if due > end {
                break;
            }
            if due >= start {
                ov.items.push(RecurringMonthItem {
                    series: s.clone(),
                    due_date: fmt(due),
                    booked: false,
                    booking_id: None,
                    amount_cents: s.amount_cents,
                });
                if s.amount_cents < 0 {
                    ov.open_expense_cents += -s.amount_cents;
                } else {
                    ov.open_income_cents += s.amount_cents;
                }
            }
        }
    }
    ov.items
        .sort_by(|a, b| (&a.due_date, &a.series.name).cmp(&(&b.due_date, &b.series.name)));
    ov
}

impl Vault {
    fn recurring_input(&self, account_ids: Vec<i64>) -> CoreResult<(Vec<Booking>, NaiveDate)> {
        let today = Local::now().date_naive();
        let page = self.list_transactions(BookingFilter {
            account_ids,
            date_from: Some(fmt(today - Duration::days(LOOKBACK_DAYS))),
            ..Default::default()
        })?;
        Ok((page.transactions, today))
    }
}

#[uniffi::export]
impl Vault {
    /// All recurring series of the given accounts (empty = all), detected and
    /// marked ones, sorted by next due date. Ignored series are left out.
    pub fn list_recurring(&self, account_ids: Vec<i64>) -> Result<Vec<RecurringSeries>, CoreError> {
        let (bookings, today) = self.recurring_input(account_ids)?;
        let stored = load_stored(&*self.db()?)?;
        Ok(build_series(&bookings, &stored, today))
    }

    /// Recurring bookings of one month: already booked vs. still due.
    pub fn recurring_month(
        &self,
        account_ids: Vec<i64>,
        year: i32,
        month: u32,
    ) -> Result<RecurringMonthOverview, CoreError> {
        let (bookings, today) = self.recurring_input(account_ids)?;
        let stored = load_stored(&*self.db()?)?;
        let series = build_series(&bookings, &stored, today);
        Ok(month_items(&series, &bookings, year, month))
    }

    /// Series hidden with "Nicht wiederkehrend" that still have bookings in
    /// the detection window, so the user can show them again.
    pub fn list_ignored_recurring(
        &self,
        account_ids: Vec<i64>,
    ) -> Result<Vec<IgnoredRecurring>, CoreError> {
        let (bookings, _) = self.recurring_input(account_ids)?;
        let stored = load_stored(&*self.db()?)?;
        Ok(ignored_series(&bookings, &stored))
    }

    /// Marks the series of a booking as recurring with the given interval.
    /// `custom_dates` (only for `Custom`) accepts "15.03." or "03-15".
    pub fn mark_booking_recurring(
        &self,
        booking_id: i64,
        interval: RecurrenceInterval,
        custom_dates: Vec<String>,
    ) -> Result<(), CoreError> {
        let key = self.booking_series_key(booking_id)?;
        let custom = if interval == RecurrenceInterval::Custom {
            let c = normalize_custom_dates(&custom_dates);
            if c.is_empty() {
                return Err(CoreError::InvalidInput {
                    message: tr("Bitte mindestens ein Datum angeben (z. B. 15.03.).").into(),
                });
            }
            c
        } else {
            Vec::new()
        };
        self.db()?.execute(
            "INSERT INTO recurring_series(series_key, status, interval, custom_dates)
             VALUES (?1, 'manual', ?2, ?3)
             ON CONFLICT(series_key) DO UPDATE SET status = 'manual',
                interval = excluded.interval, custom_dates = excluded.custom_dates",
            params![key, interval.as_str(), custom.join(",")],
        )?;
        Ok(())
    }

    /// "Nicht wiederkehrend": hides a detected or marked series.
    pub fn ignore_recurring(&self, series_key: String) -> Result<(), CoreError> {
        self.db()?.execute(
            "INSERT INTO recurring_series(series_key, status) VALUES (?1, 'ignored')
             ON CONFLICT(series_key) DO UPDATE SET status = 'ignored'",
            [series_key],
        )?;
        Ok(())
    }

    /// Forgets the user's decision; automatic detection applies again.
    pub fn reset_recurring(&self, series_key: String) -> Result<(), CoreError> {
        self.db()?.execute(
            "DELETE FROM recurring_series WHERE series_key = ?1",
            [series_key],
        )?;
        Ok(())
    }
}

impl Vault {
    fn booking_series_key(&self, booking_id: i64) -> CoreResult<String> {
        let row: Option<(String, i64)> = self
            .db()?
            .query_row(
                "SELECT booking_date, account_id FROM transactions WHERE id = ?1",
                [booking_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (day, account) = row.ok_or_else(|| CoreError::NotFound {
            what: tr("Buchung").into(),
        })?;
        let page = self.list_transactions(BookingFilter {
            account_ids: vec![account],
            date_from: Some(day.clone()),
            date_to: Some(day),
            ..Default::default()
        })?;
        let b = page
            .transactions
            .iter()
            .find(|b| b.id == booking_id)
            .ok_or_else(|| CoreError::NotFound {
                what: tr("Buchung").into(),
            })?;
        let key = series_key(b);
        if key.is_empty() {
            return Err(CoreError::InvalidInput {
                message: tr(
                    "Buchung ohne Gegenpartei kann nicht als wiederkehrend markiert werden.",
                )
                .into(),
            });
        }
        Ok(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merchants::MerchantResolver;
    use crate::models::{AccountKind, CategorySource};

    fn bk(id: i64, d: &str, cents: i64, iban: &str, name: &str) -> Booking {
        Booking {
            id,
            account_id: 1,
            account_name: String::new(),
            booking_date: d.into(),
            value_date: d.into(),
            amount_cents: cents,
            currency: "EUR".into(),
            counterparty: name.into(),
            counterparty_iban: iban.into(),
            purpose: String::new(),
            booking_text: String::new(),
            category_id: None,
            category_name: String::new(),
            category_path: String::new(),
            category_color_hex: String::new(),
            category_symbol: String::new(),
            category_source: CategorySource::None,
            rule_id: None,
            checked: false,
            merchant: MerchantResolver::resolve("", ""),
            merchant_name: String::new(),
            merchant_domain: None,
            merchant_svg_path: None,
            merchant_hex_color: String::new(),
            payment_method: String::new(),
            flow: FlowKind::Consumption,
            clean_purpose: String::new(),
            account_kind: AccountKind::Checking,
            document_count: 0,
            note: String::new(),
            purpose_code: String::new(),
            gvc: String::new(),
            salary_flag: None,
            is_salary: false,
            recurrence_key: String::new(),
        }
    }

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 11).unwrap()
    }

    #[test]
    fn detects_monthly_rent_and_yearly_insurance() {
        let mut v = vec![];
        for (i, d) in [
            "2026-07-01",
            "2026-08-03",
            "2026-09-01",
            "2026-10-01",
            "2026-11-02",
        ]
        .iter()
        .enumerate()
        {
            v.push(bk(i as i64, d, -95_000, "DE11", "Vermieter"));
        }
        v.push(bk(10, "2025-03-15", -32_000, "DE22", "Versicherung"));
        v.push(bk(11, "2026-03-16", -33_000, "DE22", "Versicherung"));
        // irregular card payments of one shop: not recurring
        for (i, d) in ["2026-09-02", "2026-09-05", "2026-10-20", "2026-11-01"]
            .iter()
            .enumerate()
        {
            v.push(bk(20 + i as i64, d, -1_000 * (i as i64 + 1), "", "Bäcker"));
        }
        let s = build_series(&v, &HashMap::new(), today());
        assert_eq!(s.len(), 2, "{s:?}");
        let rent = s.iter().find(|x| x.name == "Vermieter").unwrap();
        assert_eq!(rent.interval, RecurrenceInterval::Monthly);
        assert_eq!(rent.next_due, "2026-12-02");
        assert_eq!(rent.amount_cents, -95_000);
        let ins = s.iter().find(|x| x.name == "Versicherung").unwrap();
        assert_eq!(ins.interval, RecurrenceInterval::Yearly);
        assert_eq!(ins.next_due, "2027-03-16");
    }

    #[test]
    fn ended_contract_and_amount_jumps_are_dropped() {
        let v = vec![
            bk(1, "2026-01-05", -999, "DE33", "Streaming"),
            bk(2, "2026-02-05", -999, "DE33", "Streaming"),
            bk(3, "2026-03-05", -999, "DE33", "Streaming"),
        ];
        assert!(build_series(&v, &HashMap::new(), today()).is_empty());
        let w = vec![
            bk(1, "2026-08-05", -1_000, "DE44", "Shop"),
            bk(2, "2026-09-05", -9_000, "DE44", "Shop"),
            bk(3, "2026-10-05", -50_000, "DE44", "Shop"),
        ];
        assert!(build_series(&w, &HashMap::new(), today()).is_empty());
    }

    #[test]
    fn two_monthly_bookings_need_matching_amounts() {
        let v = vec![
            bk(1, "2026-09-25", -4_900, "DE88", "Ticket"),
            bk(2, "2026-10-25", -4_900, "DE88", "Ticket"),
            bk(3, "2026-09-12", -2_000, "DE99", "Laden"),
            bk(4, "2026-10-12", -2_400, "DE99", "Laden"),
        ];
        let s = build_series(&v, &HashMap::new(), today());
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].name, "Ticket");
    }

    #[test]
    fn two_subscriptions_with_one_provider() {
        let mut v = vec![];
        for (i, d) in ["2026-08-10", "2026-09-10", "2026-10-10", "2026-11-10"]
            .iter()
            .enumerate()
        {
            v.push(bk(i as i64, d, -299, "", "Apple"));
        }
        for (i, d) in ["2026-08-27", "2026-09-27", "2026-10-27"]
            .iter()
            .enumerate()
        {
            v.push(bk(10 + i as i64, d, -1_299, "", "Apple"));
        }
        v.push(bk(20, "2026-10-02", -129_900, "", "Apple")); // one-off purchase
        let s = build_series(&v, &HashMap::new(), today());
        assert_eq!(s.len(), 2, "{s:?}");
        assert!(s
            .iter()
            .all(|x| x.amount_specific && x.base_key == s[0].base_key));
        assert!(s[0].series_key.starts_with(&s[0].base_key) && s[0].series_key.contains('#'));
        let ov = month_items(&s, &v, 2026, 11);
        assert_eq!(ov.booked_expense_cents, 299);
        assert_eq!(ov.open_expense_cents, 1_299);
        let mut stored = HashMap::new();
        stored.insert(
            s[0].series_key.clone(),
            Stored {
                ignored: true,
                interval: RecurrenceInterval::Monthly,
                custom_dates: vec![],
            },
        );
        assert_eq!(build_series(&v, &stored, today()).len(), 1);
        let hidden = ignored_series(&v, &stored);
        assert_eq!(hidden.len(), 1);
        assert_eq!(hidden[0].series_key, s[0].series_key);
        assert_eq!(hidden[0].amount_cents.abs(), s[0].amount_cents.abs());
    }

    #[test]
    fn manual_custom_dates_and_ignore() {
        let v = vec![bk(1, "2026-08-15", -12_000, "DE55", "Grundsteuer")];
        let mut stored = HashMap::new();
        stored.insert(
            "-iban:DE55".to_string(),
            Stored {
                ignored: false,
                interval: RecurrenceInterval::Custom,
                custom_dates: normalize_custom_dates(&["15.03.".into(), "08-15".into()]),
            },
        );
        let s = build_series(&v, &stored, today());
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].source, RecurringSource::Manual);
        assert_eq!(s[0].next_due, "2027-03-15");
        stored.get_mut("-iban:DE55").unwrap().ignored = true;
        assert!(build_series(&v, &stored, today()).is_empty());
    }

    #[test]
    fn custom_date_parsing() {
        let c = normalize_custom_dates(&[
            "15.8.".into(),
            "03-15".into(),
            "15.03".into(),
            "31.02.".into(),
            "29.02.".into(),
            "x".into(),
        ]);
        assert_eq!(c, vec!["02-29", "03-15", "08-15"]);
        assert_eq!(
            custom_in_year("02-29", 2027),
            NaiveDate::from_ymd_opt(2027, 2, 28)
        );
    }

    #[test]
    fn month_overview_splits_booked_and_open() {
        let mut v = vec![];
        for (i, d) in ["2026-08-01", "2026-09-01", "2026-10-01", "2026-11-02"]
            .iter()
            .enumerate()
        {
            v.push(bk(i as i64, d, -95_000, "DE11", "Miete"));
        }
        for (i, d) in ["2026-08-20", "2026-09-20", "2026-10-20"]
            .iter()
            .enumerate()
        {
            v.push(bk(10 + i as i64, d, -1_299, "DE66", "Handy"));
        }
        for (i, d) in ["2026-08-28", "2026-09-28", "2026-10-28"]
            .iter()
            .enumerate()
        {
            v.push(bk(20 + i as i64, d, 300_000, "DE77", "Arbeitgeber"));
        }
        let s = build_series(&v, &HashMap::new(), today());
        let ov = month_items(&s, &v, 2026, 11);
        assert_eq!(ov.items.len(), 3);
        assert_eq!(ov.booked_expense_cents, 95_000);
        assert_eq!(ov.open_expense_cents, 1_299);
        assert_eq!(ov.open_income_cents, 300_000);
        assert!(ov.items[0].booked);
        assert_eq!(ov.items[1].due_date, "2026-11-20");
        // next month: everything open
        let dec = month_items(&s, &v, 2026, 12);
        assert_eq!(dec.open_expense_cents, 96_299);
        assert!(dec.items.iter().all(|i| !i.booked));
    }
}

#[cfg(test)]
mod vault_tests {
    use super::*;

    #[test]
    fn mark_ignore_reset_roundtrip() {
        let dir = std::env::temp_dir().join(format!("gc-recurring-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let v = Vault::open(dir.join("v.db").to_string_lossy().into(), vec![7u8; 32]).unwrap();
        let d = (Local::now().date_naive() - Duration::days(10))
            .format("%Y-%m-%d")
            .to_string();
        let id = {
            let c = v.db().unwrap();
            c.execute("INSERT INTO account_groups(name) VALUES ('G')", [])
                .unwrap();
            c.execute(
                "INSERT INTO accounts(group_id, external_ref, name) VALUES ((SELECT id FROM account_groups), 'A', 'K')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, counterparty, counterparty_iban)
                 VALUES ((SELECT id FROM accounts LIMIT 1), 'x', ?1, -12000, 'Stadtkasse', 'DE55')",
                [&d],
            )
            .unwrap();
            c.last_insert_rowid()
        };
        assert!(v.list_recurring(vec![]).unwrap().is_empty());
        assert!(v
            .mark_booking_recurring(id, RecurrenceInterval::Custom, vec![])
            .is_err());
        v.mark_booking_recurring(id, RecurrenceInterval::Quarterly, vec![])
            .unwrap();
        let s = v.list_recurring(vec![]).unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].interval, RecurrenceInterval::Quarterly);
        let key = s[0].series_key.clone();
        v.ignore_recurring(key.clone()).unwrap();
        assert!(v.list_recurring(vec![]).unwrap().is_empty());
        let hidden = v.list_ignored_recurring(vec![]).unwrap();
        assert_eq!(hidden.len(), 1);
        assert_eq!(hidden[0].name, "Stadtkasse");
        v.reset_recurring(key).unwrap();
        assert!(v.list_recurring(vec![]).unwrap().is_empty());
        assert!(v.list_ignored_recurring(vec![]).unwrap().is_empty());
        drop(v);
        let _ = std::fs::remove_dir_all(dir);
    }
}
