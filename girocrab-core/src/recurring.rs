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
use std::ops::RangeInclusive;

use jiff::civil::Date;
use rusqlite::{Connection, OptionalExtension, params};

use crate::dates::{iso, parse_iso};
use crate::error::{CoreError, CoreResult};
use crate::i18n::tr;
use crate::iban::normalize_iban;
use crate::merchants::FlowKind;
use crate::models::{Booking, BookingFilter};
use crate::vault::Vault;

pub(crate) const RECURRING_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS recurring_series (
    id            INTEGER PRIMARY KEY,
    series_key    TEXT    NOT NULL UNIQUE,
    status        TEXT    NOT NULL DEFAULT 'manual',   -- manual | ignored | auto (#615)
    interval      TEXT    NOT NULL DEFAULT 'monthly',  -- monthly | quarterly | halfyearly | yearly | custom
    custom_dates  TEXT    NOT NULL DEFAULT '',         -- custom: comma-separated MM-DD
    created_at    INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);
";

/// Detection window (two years, so yearly payments are seen twice).
const LOOKBACK_DAYS: i64 = 800;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, uniffi::Enum)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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
}

/// The intervals detection looks for, each with its accepted gap between two
/// bookings (days) and the minimum number of bookings.
const DETECTABLE: [(RecurrenceInterval, RangeInclusive<i64>, usize); 4] = [
    (RecurrenceInterval::Monthly, 25..=36, 2),
    (RecurrenceInterval::Quarterly, 82..=100, 3),
    (RecurrenceInterval::HalfYearly, 170..=195, 2),
    (RecurrenceInterval::Yearly, 350..=380, 2),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum RecurringSource {
    /// Found automatically in the booking history.
    Detected,
    /// Marked by the user.
    Manual,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct IgnoredRecurring {
    pub series_key: String,
    pub name: String,
    /// Latest amount (negative = expense).
    pub amount_cents: i64,
    pub last_date: String,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RecurringMonthItem {
    pub series: RecurringSeries,
    /// Due date in the month (expected, or actual booking date when booked).
    pub due_date: String,
    pub booked: bool,
    pub booking_id: Option<i64>,
    /// Actual amount when booked, else the expected amount.
    pub amount_cents: i64,
}

/// Everything the recurring UI shows for one selection, from a single pass
/// over the bookings.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RecurringSnapshot {
    pub series: Vec<RecurringSeries>,
    pub ignored: Vec<IgnoredRecurring>,
    pub month: RecurringMonthOverview,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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

/// Separates a known merchant from the IBAN in a series key (#615).
const MERCHANT_SEP: &str = "|m:";

/// Stable grouping key of a booking: direction + payer/payee. "" = not groupable.
/// An IBAN shared by several known merchants (a payment service, a card clearing
/// account) keeps them apart: `-iban:X|m:apple` (#615). Merchants guessed from the
/// counterparty name (`x:…`) are left out, so a payee's spelling can't split a series.
pub fn series_key(b: &Booking) -> String {
    let dir = if b.amount_cents < 0 { '-' } else { '+' };
    if !b.counterparty_iban.trim().is_empty() {
        let iban = normalize_iban(&b.counterparty_iban);
        let m = &b.merchant.key;
        if !m.is_empty() && !m.starts_with("x:") {
            return format!("{dir}iban:{iban}{MERCHANT_SEP}{m}");
        }
        return format!("{dir}iban:{iban}");
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

/// The key a decision was stored under before #615 (IBAN without merchant),
/// for a key that names a merchant; an amount suffix (`#cents`) is kept.
fn legacy_key(key: &str) -> Option<String> {
    let (head, tail) = key.split_once(MERCHANT_SEP)?;
    let suffix = tail.find('#').map_or("", |i| &tail[i..]);
    Some(format!("{head}{suffix}"))
}

/// The user's decision for a series: its own entry, else one stored for the whole
/// IBAN before #615. `auto` ("Automatisch erkennen" over such an old entry) means
/// none, so the old entry no longer applies.
fn decision<'a>(stored: &'a HashMap<String, Stored>, key: &str) -> Option<&'a Stored> {
    let s = stored
        .get(key)
        .or_else(|| legacy_key(key).and_then(|k| stored.get(&k)))?;
    (!s.auto).then_some(s)
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
            crate::dates::ymd(2024, m, d)?;
            Some(format!("{m:02}-{d:02}"))
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The custom date (MM-DD) in a given year; Feb 29 falls back to Feb 28.
fn custom_in_year(mmdd: &str, year: i32) -> Option<Date> {
    let (m, d) = mmdd.split_once('-')?;
    let (m, d) = (m.parse().ok()?, d.parse().ok()?);
    crate::dates::ymd(year, m, d).or_else(|| crate::dates::ymd(year, m, d - 1))
}

/// First due date strictly after `after`.
fn next_after(after: Date, interval: RecurrenceInterval, custom: &[String]) -> Option<Date> {
    match interval.months() {
        Some(m) => after.checked_add(jiff::Span::new().months(m)).ok(),
        None => (i32::from(after.year())..=i32::from(after.year()) + 1)
            .flat_map(|y| custom.iter().filter_map(move |c| custom_in_year(c, y)))
            .filter(|d| *d > after)
            .min(),
    }
}

/// Classifies the booking dates of one group; `None` = not regular.
fn detect_interval(dates: &[Date]) -> Option<RecurrenceInterval> {
    if dates.len() < 2 {
        return None;
    }
    let gaps: Vec<i64> = dates
        .windows(2)
        .map(|w| crate::dates::days_between(w[0], w[1]))
        .collect();
    let med = median(gaps.clone());
    DETECTABLE
        .into_iter()
        .find(|(_, gap, min_n)| {
            let fitting = gaps.iter().filter(|g| gap.contains(g)).count();
            gap.contains(&med) && dates.len() >= *min_n && fitting * 4 >= gaps.len() * 3
        })
        .map(|(interval, ..)| interval)
}

/// Stored user decision for a series key.
#[derive(Debug, Clone)]
struct Stored {
    ignored: bool,
    /// Back to automatic detection although an entry for the whole IBAN exists (#615).
    auto: bool,
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
                auto: status == "auto",
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

/// `|a - b| * factor <= |b|`, exact in i128 so extreme peer amounts neither
/// panic (debug) nor wrap (release).
fn amount_close(a: i64, b: i64, factor: i64) -> bool {
    (i128::from(a) - i128::from(b)).abs() * i128::from(factor) <= i128::from(b).abs()
}

/// `|a - b| * 100 <= |b| * percent`, exact in i128.
fn amount_close_pct(a: i64, b: i64, percent: i64) -> bool {
    (i128::from(a) - i128::from(b)).abs() * 100 <= i128::from(b).abs() * i128::from(percent)
}

/// Interval of a group of bookings (date-sorted) when it looks recurring.
fn detect_group(list: &[&Booking]) -> Option<RecurrenceInterval> {
    // one booking per day (split payments / corrections)
    let mut dates: Vec<Date> = list
        .iter()
        .filter_map(|b| parse_iso(&b.booking_date))
        .collect();
    dates.dedup();
    let i = detect_interval(&dates)?;
    // amounts must be similar (±25 % of the median)
    let med = median(list.iter().map(|b| b.amount_cents).collect());
    let similar = list
        .iter()
        .filter(|b| amount_close(b.amount_cents, med, 4))
        .count();
    if similar * 4 < list.len() * 3 {
        return None;
    }
    // Only two monthly bookings: amounts must (almost) match (±5 %).
    if dates.len() < 3
        && i == RecurrenceInterval::Monthly
        && list.iter().any(|b| !amount_close(b.amount_cents, med, 20))
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
            Some(c) if amount_close(b.amount_cents, c[0].amount_cents, 10) => c.push(b),
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
        && (!s.amount_specific || amount_close_pct(b.amount_cents, s.amount_cents, 15))
}

fn make_series(
    key: String,
    base_key: &str,
    list: &[&Booking],
    interval: RecurrenceInterval,
    custom: Vec<String>,
    source: RecurringSource,
    today: Date,
) -> Option<RecurringSeries> {
    let last = *list.last()?;
    let last_date = parse_iso(&last.booking_date)?;
    let next = next_after(last_date, interval, &custom)?;
    // A detected series that missed two cycles has ended (contract cancelled).
    if source == RecurringSource::Detected {
        let cycle = crate::dates::days_between(last_date, next);
        if crate::dates::days_between(last_date, today) > cycle * 2 + 7 {
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
        next_due: iso(next),
        occurrences: list.len() as u32,
        source,
    })
}

/// Builds all series from bookings (any order) and the stored decisions.
fn build_series(
    bookings: &[Booking],
    stored: &HashMap<String, Stored>,
    today: Date,
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
        match decision(stored, &key) {
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
                    if decision(stored, &sub).is_some_and(|s| s.ignored) {
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
            // The base key may hold free text with '#' ("-n:shop #12"): a whole key
            // that is itself a series key wins, otherwise only a numeric suffix
            // after the *last* '#' is the amount of a sub-series ("{key}#{cents}").
            let whole = bookings.iter().any(|b| {
                series_key(b) == *key || legacy_key(&series_key(b)).as_deref() == Some(key)
            });
            let (base, amount) = match key.rsplit_once('#') {
                Some((b, a)) if !whole => match a.parse::<i64>() {
                    Ok(a) => (b, Some(a)),
                    Err(_) => (key.as_str(), None),
                },
                _ => (key.as_str(), None),
            };
            // An entry for the whole IBAN (before #615) covers the merchants behind it
            // that have no decision of their own.
            let in_base = |b: &&Booking| {
                let k = series_key(b);
                k == base
                    || (legacy_key(&k).as_deref() == Some(base)
                        && !stored.contains_key(&k)
                        && amount.is_none_or(|a| !stored.contains_key(&format!("{k}#{a}"))))
            };
            let last = bookings
                .iter()
                .filter(in_base)
                .filter(|b| {
                    amount.is_none_or(|a| amount_close_pct(b.amount_cents.saturating_abs(), a, 15))
                })
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
    let Some(start) = crate::dates::ymd(year, month, 1) else {
        return ov;
    };
    let end = start.last_of_month();
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
                    ov.booked_expense_cents = ov
                        .booked_expense_cents
                        .saturating_add(b.amount_cents.saturating_neg());
                }
            }
            continue;
        }
        let Some(mut due) = parse_iso(&s.last_date) else {
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
                    due_date: iso(due),
                    booked: false,
                    booking_id: None,
                    amount_cents: s.amount_cents,
                });
                if s.amount_cents < 0 {
                    ov.open_expense_cents = ov
                        .open_expense_cents
                        .saturating_add(s.amount_cents.saturating_neg());
                } else {
                    ov.open_income_cents = ov.open_income_cents.saturating_add(s.amount_cents);
                }
            }
        }
    }
    ov.items
        .sort_by(|a, b| (&a.due_date, &a.series.name).cmp(&(&b.due_date, &b.series.name)));
    ov
}

impl Vault {
    fn recurring_input(&self, account_ids: Vec<i64>) -> CoreResult<(Vec<Booking>, Date)> {
        let today = crate::dates::today();
        let filter = BookingFilter {
            account_ids,
            date_from: Some(iso(today - crate::dates::days(LOOKBACK_DAYS))),
            ..Default::default()
        };
        Ok((self.collect_bookings(&*self.db()?, &filter)?, today))
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

    /// `list_recurring`, `list_ignored_recurring` and `recurring_month` in one
    /// call: the bookings are loaded and the series detected only once.
    pub fn recurring_snapshot(
        &self,
        account_ids: Vec<i64>,
        year: i32,
        month: u32,
    ) -> Result<RecurringSnapshot, CoreError> {
        let (bookings, today) = self.recurring_input(account_ids)?;
        let stored = load_stored(&*self.db()?)?;
        let series = build_series(&bookings, &stored, today);
        Ok(RecurringSnapshot {
            month: month_items(&series, &bookings, year, month),
            ignored: ignored_series(&bookings, &stored),
            series,
        })
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

    /// Forgets the user's decision; automatic detection applies again. A merchant
    /// behind an IBAN that still has a decision from before #615 is marked `auto`
    /// instead, so that old decision doesn't take over again (and the other
    /// merchants keep it).
    pub fn reset_recurring(&self, series_key: String) -> Result<(), CoreError> {
        let db = self.db()?;
        let legacy_exists = match legacy_key(&series_key) {
            Some(k) => db
                .query_row(
                    "SELECT 1 FROM recurring_series WHERE series_key = ?1",
                    [k],
                    |_| Ok(()),
                )
                .optional()?
                .is_some(),
            None => false,
        };
        if legacy_exists {
            db.execute(
                "INSERT INTO recurring_series(series_key, status) VALUES (?1, 'auto')
                 ON CONFLICT(series_key) DO UPDATE SET status = 'auto'",
                [series_key],
            )?;
        } else {
            db.execute(
                "DELETE FROM recurring_series WHERE series_key = ?1",
                [series_key],
            )?;
        }
        Ok(())
    }
}

impl Vault {
    fn booking_series_key(&self, booking_id: i64) -> CoreResult<String> {
        let key = self
            .bookings_by_ids(&*self.db()?, &[booking_id])?
            .pop()
            .ok_or_else(|| CoreError::NotFound {
                what: tr("Buchung").into(),
            })?
            .recurrence_key;
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

/// Monthly accrual share (cents) of a payment made every `interval_months` months
/// (annual `12` → 1/12, semi-annual `6` → 1/6, quarterly `3` → 1/4 … i.e. 1/`interval_months`).
/// For analytics/budgets only; real account balances are never touched. Rounds half away
/// from zero. Other intervals (0, 1, 2, …) are returned unchanged (no accrual).
pub fn monthly_accrual_share_cents(amount_cents: i64, interval_months: u8) -> i64 {
    if !matches!(interval_months, 3 | 6 | 12) {
        return amount_cents;
    }
    let n = i128::from(interval_months);
    let a = i128::from(amount_cents);
    let half = n / 2;
    let q = if a >= 0 {
        (a + half) / n
    } else {
        (a - half) / n
    };
    q as i64
}

/// Spreads category splits `(category_id, amount_cents)` of one payment into their monthly
/// shares. The shares add up exactly to `monthly_accrual_share_cents` of the split total:
/// the rounding remainder goes to the split with the largest absolute amount (first on ties).
pub fn pro_rate_splits_monthly(splits: &[(i64, i64)], interval_months: u8) -> Vec<(i64, i64)> {
    let mut out: Vec<(i64, i64)> = splits
        .iter()
        .map(|&(cat, cents)| (cat, monthly_accrual_share_cents(cents, interval_months)))
        .collect();
    if !matches!(interval_months, 3 | 6 | 12) || out.is_empty() {
        return out;
    }
    let total: i64 = splits.iter().map(|s| s.1).sum();
    let diff =
        monthly_accrual_share_cents(total, interval_months) - out.iter().map(|s| s.1).sum::<i64>();
    if diff != 0 {
        let mut idx = 0;
        for (i, s) in splits.iter().enumerate() {
            if s.1.abs() > splits[idx].1.abs() {
                idx = i;
            }
        }
        out[idx].1 += diff;
    }
    out
}

/// A detected price hike of a recurring debit (or a duplicate debit).
///
/// Amounts are positive magnitudes of the debit in cents. For a duplicate debit
/// (same amount twice within [`DUPLICATE_WINDOW_DAYS`]) `previous_cents` is the single
/// charge, `new_cents` the doubled sum and `increase_cents` the extra charge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceIncreaseAlert {
    pub counterparty: String,
    pub previous_cents: i64,
    pub new_cents: i64,
    pub increase_cents: i64,
    pub booking_date: String,
}

/// Debits of the same amount at most this many days apart count as a duplicate.
const DUPLICATE_WINDOW_DAYS: i64 = 3;

/// Detects price increases in recurring debits (electricity, insurance, streaming, mobile …)
/// and duplicate debits. `series` holds `(counterparty, booking_date ISO, amount_cents)`;
/// only debits (negative amounts) are considered, in any order.
///
/// Price increase: per counterparty, the latest debit is higher than the previous one,
/// by at most 100 %, while the two debits before it were equal (a stable price), so at
/// least three debits are needed. Duplicate: two equal debits within
/// [`DUPLICATE_WINDOW_DAYS`]; the second one is not used for the price comparison.
/// Result is sorted by booking date, then counterparty.
pub fn detect_price_increases(series: &[(String, String, i64)]) -> Vec<PriceIncreaseAlert> {
    let mut groups: HashMap<String, (String, Vec<(Date, i64)>)> = HashMap::new();
    for (name, date, cents) in series {
        let key = name.trim().to_lowercase();
        let Some(d) = parse_iso(date) else { continue };
        if *cents >= 0 || key.is_empty() {
            continue;
        }
        groups
            .entry(key)
            .or_insert_with(|| (name.trim().to_string(), Vec::new()))
            .1
            .push((d, cents.abs()));
    }

    let mut alerts = Vec::new();
    for (name, mut list) in groups.into_values() {
        list.sort();
        let mut unique: Vec<(Date, i64)> = Vec::new();
        for (d, amount) in list {
            if let Some(&(pd, pa)) = unique.last()
                && pa == amount
                && crate::dates::days_between(pd, d) <= DUPLICATE_WINDOW_DAYS
            {
                alerts.push(PriceIncreaseAlert {
                    counterparty: name.clone(),
                    previous_cents: amount,
                    new_cents: amount * 2,
                    increase_cents: amount,
                    booking_date: iso(d),
                });
                continue;
            }
            unique.push((d, amount));
        }
        if let [.., a, b, c] = unique.as_slice()
            && a.1 == b.1
            && c.1 > b.1
            && c.1 <= b.1 * 2
        {
            alerts.push(PriceIncreaseAlert {
                counterparty: name.clone(),
                previous_cents: b.1,
                new_cents: c.1,
                increase_cents: c.1 - b.1,
                booking_date: iso(c.0),
            });
        }
    }
    alerts.sort_by(|a, b| {
        (&a.booking_date, &a.counterparty).cmp(&(&b.booking_date, &b.counterparty))
    });
    alerts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extreme_amounts_do_not_overflow_the_similarity_checks() {
        assert!(!amount_close(i64::MIN, i64::MAX, 20));
        assert!(!amount_close(i64::MAX, i64::MIN, 4));
        assert!(!amount_close_pct(i64::MIN, 1, 15));
        assert!(amount_close(1000, 1000, 4));
        assert!(amount_close(1100, 1000, 4));
        assert!(!amount_close(2000, 1000, 4));
        assert!(amount_close_pct(1100, 1000, 15));
        assert!(!amount_close_pct(1200, 1000, 15));
    }

    use crate::merchants::MerchantResolver;

    fn row(name: &str, d: &str, cents: i64) -> (String, String, i64) {
        (name.to_string(), d.to_string(), cents)
    }

    #[test]
    fn price_increase_detected() {
        let s = [
            row("Netflix", "2026-01-05", -1299),
            row("Netflix", "2026-02-05", -1299),
            row("Netflix", "2026-03-05", -1499),
        ];
        let a = detect_price_increases(&s);
        assert_eq!(
            a,
            vec![PriceIncreaseAlert {
                counterparty: "Netflix".into(),
                previous_cents: 1299,
                new_cents: 1499,
                increase_cents: 200,
                booking_date: "2026-03-05".into(),
            }]
        );
    }

    #[test]
    fn stable_price_unordered_input_and_credits_ignored() {
        let s = [
            row("Stadtwerke", "2026-03-01", -5000),
            row("Stadtwerke", "2026-01-01", -5000),
            row("Stadtwerke", "2026-02-01", -5000),
            row("Stadtwerke", "2026-02-15", 9000),
        ];
        assert!(detect_price_increases(&s).is_empty());
    }

    #[test]
    fn needs_three_debits_and_ignores_price_drops() {
        let two = [
            row("Mobil", "2026-01-01", -999),
            row("Mobil", "2026-02-01", -1999),
        ];
        assert!(detect_price_increases(&two).is_empty());
        let drop = [
            row("Mobil", "2026-01-01", -1999),
            row("Mobil", "2026-02-01", -1999),
            row("Mobil", "2026-03-01", -999),
        ];
        assert!(detect_price_increases(&drop).is_empty());
    }

    #[test]
    fn huge_jump_is_not_a_price_increase() {
        let s = [
            row("Versicherung", "2026-01-01", -1000),
            row("Versicherung", "2026-02-01", -1000),
            row("Versicherung", "2026-03-01", -9000),
        ];
        assert!(detect_price_increases(&s).is_empty());
    }

    #[test]
    fn duplicate_debit_detected_and_case_insensitive() {
        let s = [
            row("Netflix", "2026-01-05", -1299),
            row("netflix ", "2026-01-06", -1299),
            row("Netflix", "2026-02-05", -1299),
        ];
        let a = detect_price_increases(&s);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].booking_date, "2026-01-06");
        assert_eq!(a[0].increase_cents, 1299);
        assert_eq!(a[0].new_cents, 2598);
    }

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
            merchant: MerchantResolver::placeholder(),
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
            is_pending: false,
            item_refs: String::new(),
            end_to_end_id: String::new(),
            mandate_id: String::new(),
            creditor_id: String::new(),
            external_id: String::new(),
            tags: Vec::new(),
        }
    }

    fn today() -> Date {
        jiff::civil::date(2026, 11, 11)
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
        assert!(
            s.iter()
                .all(|x| x.amount_specific && x.base_key == s[0].base_key)
        );
        assert!(s[0].series_key.starts_with(&s[0].base_key) && s[0].series_key.contains('#'));
        let ov = month_items(&s, &v, 2026, 11);
        assert_eq!(ov.booked_expense_cents, 299);
        assert_eq!(ov.open_expense_cents, 1_299);
        let mut stored = HashMap::new();
        stored.insert(
            s[0].series_key.clone(),
            Stored {
                ignored: true,
                auto: false,
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

    /// #615: Lidl shopping and an Apple subscription paid through one IBAN
    /// (payment service / card clearing account).
    fn lidl_and_apple_via_one_iban() -> Vec<Booking> {
        let mut v = vec![];
        let with = |mut b: Booking, cp: &str| {
            b.merchant = MerchantResolver::resolve(cp, "");
            b
        };
        for (i, d) in ["2026-08-11", "2026-09-11", "2026-10-11", "2026-11-10"]
            .iter()
            .enumerate()
        {
            v.push(with(
                bk(i as i64, d, -299, "DE99 PAY", "Apple Services"),
                "Apple Services",
            ));
        }
        // monthly shopping (weekly is never detected): its own series next to Apple
        for (i, d) in ["2026-08-04", "2026-09-04", "2026-10-05", "2026-11-04"]
            .iter()
            .enumerate()
        {
            v.push(with(
                bk(10 + i as i64, d, -4_000 - 37 * i as i64, "DE99PAY", "Lidl"),
                "Lidl",
            ));
        }
        v
    }

    fn stored_entry(ignored: bool, auto: bool) -> Stored {
        Stored {
            ignored,
            auto,
            interval: RecurrenceInterval::Monthly,
            custom_dates: vec![],
        }
    }

    #[test]
    fn known_merchants_behind_one_iban_are_separate_series() {
        let v = lidl_and_apple_via_one_iban();
        assert_eq!(series_key(&v[0]), "-iban:DE99PAY|m:apple");
        assert_eq!(series_key(&v[5]), "-iban:DE99PAY|m:lidl");
        let s = build_series(&v, &HashMap::new(), today());
        assert_eq!(s.len(), 2, "{s:?}");
        let apple = s.iter().find(|x| x.name == "Apple Services").unwrap();
        let lidl = s.iter().find(|x| x.name == "Lidl").unwrap();
        assert_eq!(apple.interval, RecurrenceInterval::Monthly);
        assert!(!apple.amount_specific && !lidl.amount_specific);

        // "Nicht wiederkehrend" on Lidl keeps the subscription
        let mut stored = HashMap::new();
        stored.insert(lidl.series_key.clone(), stored_entry(true, false));
        let s = build_series(&v, &stored, today());
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].name, "Apple Services");
        // marking Apple monthly leaves Lidl hidden (and Lidl's bookings out of it)
        stored.insert(apple.series_key.clone(), stored_entry(false, false));
        let s = build_series(&v, &stored, today());
        assert_eq!(s.len(), 1);
        assert_eq!(
            (s[0].name.as_str(), s[0].source),
            ("Apple Services", RecurringSource::Manual)
        );
        assert_eq!(s[0].occurrences, 4);
        let hidden = ignored_series(&v, &stored);
        assert_eq!(hidden.len(), 1);
        assert_eq!(hidden[0].name, "Lidl");
    }

    #[test]
    fn guessed_merchants_keep_the_iban_key() {
        let mut b = bk(1, "2026-11-01", -95_000, "DE11", "Hausverwaltung Müller");
        b.merchant = MerchantResolver::resolve("Hausverwaltung Müller", "");
        assert!(b.merchant.key.starts_with("x:"));
        assert_eq!(series_key(&b), "-iban:DE11");
        assert_eq!(legacy_key("-iban:DE11"), None);
        assert_eq!(
            legacy_key("-iban:DE11|m:apple#299").as_deref(),
            Some("-iban:DE11#299")
        );
    }

    #[test]
    fn decisions_for_the_whole_iban_from_before_still_apply() {
        let v = lidl_and_apple_via_one_iban();
        // hidden before #615 under the IBAN alone: both stay hidden
        let mut stored = HashMap::new();
        stored.insert("-iban:DE99PAY".to_string(), stored_entry(true, false));
        assert!(build_series(&v, &stored, today()).is_empty());
        let hidden = ignored_series(&v, &stored);
        assert_eq!(hidden.len(), 1);
        assert_eq!(hidden[0].series_key, "-iban:DE99PAY");

        // a merchant's own decision wins and takes it out of the old entry
        stored.insert(
            "-iban:DE99PAY|m:apple".to_string(),
            stored_entry(false, false),
        );
        let s = build_series(&v, &stored, today());
        assert_eq!(s.len(), 1);
        assert_eq!(
            (s[0].name.as_str(), s[0].source),
            ("Apple Services", RecurringSource::Manual)
        );
        assert_eq!(ignored_series(&v, &stored)[0].name, "Lidl");

        // "Automatisch erkennen" over the old entry: detection again, for Apple only
        stored.insert(
            "-iban:DE99PAY|m:apple".to_string(),
            stored_entry(false, true),
        );
        let s = build_series(&v, &stored, today());
        assert_eq!(s.len(), 1);
        assert_eq!(
            (s[0].name.as_str(), s[0].source),
            ("Apple Services", RecurringSource::Detected)
        );
    }

    #[test]
    fn manual_custom_dates_and_ignore() {
        let v = vec![bk(1, "2026-08-15", -12_000, "DE55", "Grundsteuer")];
        let mut stored = HashMap::new();
        stored.insert(
            "-iban:DE55".to_string(),
            Stored {
                ignored: false,
                auto: false,
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
            crate::dates::ymd(2027, 2, 28)
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

    #[test]
    fn accrual_share_by_interval() {
        assert_eq!(monthly_accrual_share_cents(-120_000, 12), -10_000);
        assert_eq!(monthly_accrual_share_cents(-60_000, 6), -10_000);
        assert_eq!(monthly_accrual_share_cents(-30_000, 3), -10_000);
        assert_eq!(monthly_accrual_share_cents(10_000, 12), 833); // 833.33
        assert_eq!(monthly_accrual_share_cents(-10_000, 12), -833);
        assert_eq!(monthly_accrual_share_cents(600, 12), 50);
        assert_eq!(monthly_accrual_share_cents(900, 12), 75);
        assert_eq!(monthly_accrual_share_cents(7, 12), 1); // 0.58 rounds up
        assert_eq!(monthly_accrual_share_cents(-7, 12), -1);
        // not an accrual interval: unchanged
        assert_eq!(monthly_accrual_share_cents(-1_999, 1), -1_999);
        assert_eq!(monthly_accrual_share_cents(-1_999, 0), -1_999);
        assert_eq!(monthly_accrual_share_cents(-1_999, 2), -1_999);
    }

    #[test]
    fn pro_rate_splits_sum_matches_total_share() {
        let splits = [(1, -5_000), (2, -3_000), (3, -2_000)];
        let out = pro_rate_splits_monthly(&splits, 12);
        assert_eq!(out.iter().map(|s| s.0).collect::<Vec<_>>(), vec![1, 2, 3]);
        assert_eq!(
            out.iter().map(|s| s.1).sum::<i64>(),
            monthly_accrual_share_cents(-10_000, 12)
        );
        // rounding remainder lands on the largest split
        let splits = [(1, 100), (2, 100), (3, 100)];
        let out = pro_rate_splits_monthly(&splits, 12); // each 8.33 -> 8, total 25
        assert_eq!(out.iter().map(|s| s.1).sum::<i64>(), 25);
        assert_eq!(out, vec![(1, 9), (2, 8), (3, 8)]);
        // exact quarterly
        assert_eq!(
            pro_rate_splits_monthly(&[(1, -3_000), (2, -600)], 3),
            vec![(1, -1_000), (2, -200)]
        );
        // non-accrual interval and empty input
        assert_eq!(pro_rate_splits_monthly(&splits, 1), splits.to_vec());
        assert!(pro_rate_splits_monthly(&[], 12).is_empty());
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
        let d = crate::dates::iso(crate::dates::today() - crate::dates::days(10));
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
        assert!(
            v.mark_booking_recurring(id, RecurrenceInterval::Custom, vec![])
                .is_err()
        );
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

    /// #615: an entry for the whole IBAN from before stays for the other
    /// merchants when one of them goes back to automatic detection.
    #[test]
    fn reset_over_an_old_iban_entry_marks_auto() {
        let dir = std::env::temp_dir().join(format!("gc-recurring-615-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let v = Vault::open(dir.join("v.db").to_string_lossy().into(), vec![7u8; 32]).unwrap();
        let d = crate::dates::iso(crate::dates::today() - crate::dates::days(10));
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
                 VALUES ((SELECT id FROM accounts LIMIT 1), 'x', ?1, -299, 'Apple Services', 'DE99PAY')",
                [&d],
            )
            .unwrap();
            c.execute(
                "INSERT INTO recurring_series(series_key, status) VALUES ('-iban:DE99PAY', 'ignored')",
                [],
            )
            .unwrap();
            c.query_row("SELECT id FROM transactions", [], |r| r.get::<_, i64>(0))
                .unwrap()
        };
        assert!(
            v.list_recurring(vec![]).unwrap().is_empty(),
            "old entry hides it"
        );
        v.mark_booking_recurring(id, RecurrenceInterval::Monthly, vec![])
            .unwrap();
        let s = v.list_recurring(vec![]).unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].series_key, "-iban:DE99PAY|m:apple");
        assert!(v.list_ignored_recurring(vec![]).unwrap().is_empty());

        v.reset_recurring(s[0].series_key.clone()).unwrap();
        let status = |k: &str| {
            v.db()
                .unwrap()
                .query_row(
                    "SELECT status FROM recurring_series WHERE series_key = ?1",
                    [k],
                    |r| r.get::<_, String>(0),
                )
                .optional()
                .unwrap()
        };
        assert_eq!(status("-iban:DE99PAY|m:apple").as_deref(), Some("auto"));
        assert_eq!(status("-iban:DE99PAY").as_deref(), Some("ignored"));
        // one booking: nothing detected, and the old entry no longer hides it
        assert!(v.list_recurring(vec![]).unwrap().is_empty());
        assert!(v.list_ignored_recurring(vec![]).unwrap().is_empty());
        drop(v);
        let _ = std::fs::remove_dir_all(dir);
    }
}
