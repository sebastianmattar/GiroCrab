//! Salary-period evaluation (#565): income against living expenses per period
//! that starts on the payday instead of the first of the month.
//!
//! Someone paid on the 15th sees `15.10. – 14.11.` as one period. The
//! evaluation only reads `(date, amount, flag)` triples, so booking categories
//! stay untouched.

use jiff::civil::Date;

use crate::dates::{clamped_ymd, iso, parse_iso};

/// One payday period with its income and living expenses (all in cents).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaydayPeriod {
    /// First day of the period, `YYYY-MM-DD`.
    pub start_ymd: String,
    /// Last day of the period (inclusive), `YYYY-MM-DD`.
    pub end_ymd: String,
    /// Sum of all positive amounts.
    pub income_cents: i64,
    /// Sum of negative amounts flagged as living expense, as a positive number.
    pub expense_cents: i64,
    /// `income_cents - expense_cents`.
    pub surplus_cents: i64,
}

/// The payday in the month of `day`, or the month's last day when it is shorter.
fn payday_in(payday: u32, day: Date) -> Date {
    clamped_ymd(day.year().into(), day.month() as u32, payday).unwrap_or(day)
}

/// Groups bookings into consecutive payday periods, oldest first.
///
/// `bookings` are `(date YYYY-MM-DD, amount_cents, is_living_expense)`:
/// positive amounts count as income; negative amounts count as expense only
/// when the flag is set (so transfers, savings or one-off items can be left
/// out). Entries with an unparsable date are ignored. `payday_day` is clamped
/// to 1..=31; in shorter months the period starts on the month's last day.
/// Periods without bookings between the first and last one are included
/// with zeros, so consecutive periods can be compared.
pub fn evaluate_payday_periods(
    payday_day: u8,
    bookings: &[(String, i64, bool)],
) -> Vec<PaydayPeriod> {
    let payday = u32::from(payday_day.clamp(1, 31));
    // keyed by the period's first day
    let mut sums: std::collections::BTreeMap<Date, (i64, i64)> = Default::default();
    for (date, cents, living) in bookings {
        let Some(day) = parse_iso(date) else {
            continue;
        };
        let mut start = payday_in(payday, day);
        if day < start {
            let Ok(previous) = day.first_of_month().yesterday() else {
                continue;
            };
            start = payday_in(payday, previous);
        }
        let entry = sums.entry(start).or_default();
        if *cents > 0 {
            entry.0 += cents;
        } else if *cents < 0 && *living {
            entry.1 -= cents;
        }
    }
    let (Some(&first), Some(&last)) = (sums.keys().next(), sums.keys().next_back()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut cur = first;
    loop {
        let (income, expense) = sums.get(&cur).copied().unwrap_or((0, 0));
        // jiff stops at 9999-12-31: a period running past it ends there
        let next = cur
            .last_of_month()
            .tomorrow()
            .ok()
            .map(|d| payday_in(payday, d));
        let end = next.and_then(|n| n.yesterday().ok()).unwrap_or(Date::MAX);
        out.push(PaydayPeriod {
            start_ymd: iso(cur),
            end_ymd: iso(end),
            income_cents: income,
            expense_cents: expense,
            surplus_cents: income - expense,
        });
        match next {
            Some(n) if cur != last => cur = n,
            _ => break,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(d: &str, c: i64, l: bool) -> (String, i64, bool) {
        (d.to_string(), c, l)
    }

    #[test]
    fn mid_month_payday_period() {
        let r = evaluate_payday_periods(
            15,
            &[
                b("2026-10-15", 300_000, false),
                b("2026-10-20", -80_000, true),
                b("2026-11-14", -20_000, true),
                b("2026-11-15", 300_000, false),
            ],
        );
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].start_ymd, "2026-10-15");
        assert_eq!(r[0].end_ymd, "2026-11-14");
        assert_eq!(r[0].income_cents, 300_000);
        assert_eq!(r[0].expense_cents, 100_000);
        assert_eq!(r[0].surplus_cents, 200_000);
        assert_eq!(r[1].start_ymd, "2026-11-15");
        assert_eq!(r[1].end_ymd, "2026-12-14");
        assert_eq!(r[1].surplus_cents, 300_000);
    }

    #[test]
    fn payday_one_is_calendar_month() {
        let r = evaluate_payday_periods(1, &[b("2026-02-10", 5, false)]);
        assert_eq!(
            (r[0].start_ymd.as_str(), r[0].end_ymd.as_str()),
            ("2026-02-01", "2026-02-28")
        );
    }

    #[test]
    fn year_boundary() {
        let r = evaluate_payday_periods(15, &[b("2027-01-03", 100, false)]);
        assert_eq!(
            (r[0].start_ymd.as_str(), r[0].end_ymd.as_str()),
            ("2026-12-15", "2027-01-14")
        );
    }

    #[test]
    fn short_month_clamps_payday() {
        let r = evaluate_payday_periods(
            31,
            &[b("2026-02-28", 100, false), b("2026-03-31", 1, false)],
        );
        assert_eq!(r[0].start_ymd, "2026-02-28");
        assert_eq!(r[0].end_ymd, "2026-03-30");
        assert_eq!(r[1].start_ymd, "2026-03-31");
        assert_eq!(r[1].end_ymd, "2026-04-29");
    }

    #[test]
    fn gaps_are_filled_and_flag_excludes_expenses() {
        let r = evaluate_payday_periods(
            10,
            &[
                b("2026-01-10", 100, false),
                b("2026-03-12", -50, false),
                b("2026-03-12", -7, true),
            ],
        );
        assert_eq!(r.len(), 3);
        assert_eq!(r[1].income_cents, 0);
        assert_eq!(r[1].expense_cents, 0);
        assert_eq!(r[2].expense_cents, 7);
        assert_eq!(r[2].surplus_cents, -7);
    }

    #[test]
    fn a_period_at_the_end_of_the_calendar_ends_there() {
        let r = evaluate_payday_periods(15, &[b("9999-12-20", 1, false)]);
        assert_eq!(
            (r[0].start_ymd.as_str(), r[0].end_ymd.as_str()),
            ("9999-12-15", "9999-12-31")
        );
    }

    #[test]
    fn empty_and_invalid_input() {
        assert!(evaluate_payday_periods(15, &[]).is_empty());
        assert!(
            evaluate_payday_periods(15, &[b("garbage", 1, true), b("2026-02-30", 1, true)])
                .is_empty()
        );
    }
}
