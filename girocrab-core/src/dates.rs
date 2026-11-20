//! Date and time helpers. The vault stores dates as `YYYY-MM-DD` text and
//! change stamps (`created_ms`, `updated_ms`, `deleted_ms`) as Unix
//! milliseconds.
//!
//! "Today" is the calendar day in the system time zone. jiff falls back to
//! UTC when it cannot find one (it logs a warning); on Android that needs the
//! bundled zone data enabled in `Cargo.toml`.

use std::sync::LazyLock;

use jiff::civil::{Date, DateTime};
use jiff::tz::TimeZone;
use jiff::{Span, Timestamp, ZonedDateTime};
use regex::Regex;

/// The current time as a change stamp: Unix milliseconds, UTC.
pub(crate) fn now_ms() -> i64 {
    Timestamp::now().as_millisecond()
}

/// Today's date in the system time zone.
pub(crate) fn today() -> Date {
    ZonedDateTime::now().date()
}

/// Today's date in the system time zone, as stored: `YYYY-MM-DD`.
pub(crate) fn today_iso() -> String {
    iso(today())
}

/// The current UTC wall-clock time, for APIs that send UTC without an offset.
pub(crate) fn now_utc() -> DateTime {
    TimeZone::UTC.to_datetime(Timestamp::now())
}

/// The date a Unix timestamp falls on in the system time zone.
pub(crate) fn local_date(ts: Timestamp) -> Date {
    TimeZone::system().to_datetime(ts).date()
}

/// The date a Unix timestamp falls on in UTC.
pub(crate) fn utc_date(ts: Timestamp) -> Date {
    TimeZone::UTC.to_datetime(ts).date()
}

/// The day `y`-`m`-`d`, or `None` if there is no such day. Out-of-range
/// numbers are `None` too, never truncated into jiff's narrower fields.
pub(crate) fn ymd(y: i32, m: u32, d: u32) -> Option<Date> {
    Date::new(y.try_into().ok()?, m.try_into().ok()?, d.try_into().ok()?).ok()
}

/// A span of `n` calendar days, for date arithmetic (`today - days(7)`).
/// `Date + Span` panics outside jiff's range (years -9999 to 9999): for a date
/// read from bank, PDF or user data use [`add_days`] instead ("gültig bis
/// 31.12.9999" is a real thing to find in a document).
pub(crate) fn days(n: i64) -> Span {
    Span::new().days(n)
}

/// `d` moved by `n` days, or `None` past jiff's range (years -9999 to 9999).
pub(crate) fn add_days(d: Date, n: i64) -> Option<Date> {
    d.checked_add(Span::new().try_days(n).ok()?).ok()
}

/// Whole days from `from` to `to`, negative when `to` is earlier.
pub(crate) fn days_between(from: Date, to: Date) -> i64 {
    // SignedDuration skips the Span rounding machinery; civil days are 24 h.
    to.duration_since(from).as_hours() / 24
}

/// A running day count (0001-01-01 is day 1) for comparing dates as plain
/// numbers; only differences between two counts mean anything.
pub(crate) fn day_number(d: Date) -> i32 {
    days_between(jiff::civil::date(1, 1, 1), d) as i32 + 1
}

/// `d` as stored: `YYYY-MM-DD` (jiff's `Display`, the year padded to four digits).
pub(crate) fn iso(d: Date) -> String {
    d.to_string()
}

/// The `YYYY-MM-DD` at the start of `s`; a time after it is ignored.
pub(crate) fn parse_iso(s: &str) -> Option<Date> {
    Date::strptime("%Y-%m-%d", s.get(..10).unwrap_or(s)).ok()
}

/// `YYYY-MM-DD` or `DD.MM.YYYY`, blanks around it ignored.
pub(crate) fn parse_de_or_iso(s: &str) -> Option<Date> {
    let s = s.trim();
    Date::strptime("%Y-%m-%d", s)
        .or_else(|_| Date::strptime("%d.%m.%Y", s))
        .ok()
}

/// Every `D.M.YYYY` in `s` that is a real day ("31.02.2025" is skipped).
/// Digits glued to it ("112.03.2025") don't count as one.
pub(crate) fn de_dates(s: &str) -> impl Iterator<Item = Date> + '_ {
    static DE_DATE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\b(\d{1,2})\.(\d{1,2})\.(\d{4})\b").unwrap());
    DE_DATE
        .captures_iter(s)
        .filter_map(|c| ymd(c[3].parse().ok()?, c[2].parse().ok()?, c[1].parse().ok()?))
}

/// The first of [`de_dates`].
pub(crate) fn find_de_date(s: &str) -> Option<Date> {
    de_dates(s).next()
}

/// The day `day.month.` that falls on or before `latest`, for dates printed
/// without a year: "28.12." before 05.01.2026 is 2025-12-28.
pub(crate) fn day_month_on_or_before(day: u32, month: u32, latest: Date) -> Option<Date> {
    let later = (month, day) > (latest.month() as u32, latest.day() as u32);
    ymd(i32::from(latest.year()) - i32::from(later), month, day)
}

/// The day a leading `YYYYMMDD` names (FinTS and MT535), `None` unless the
/// eight characters are digits and a real day.
pub(crate) fn parse_yyyymmdd(s: &str) -> Option<Date> {
    let d = s
        .get(..8)
        .filter(|d| d.bytes().all(|b| b.is_ascii_digit()))?;
    ymd(
        d[..4].parse().ok()?,
        d[4..6].parse().ok()?,
        d[6..].parse().ok()?,
    )
}

/// A year written with four or two digits. Two digits pivot at 80: 81–99 is
/// 1981–1999, 00–80 is 2000–2080 ("31.12.99" on a receipt is 1999, not 2099).
pub(crate) fn year(s: &str) -> Option<i32> {
    let n: i32 = s.parse().ok()?;
    Some(match s.len() {
        2 if n > 80 => 1900 + n,
        2 => 2000 + n,
        _ => n,
    })
}

/// Day `d` of month `m`, a day past the month's end moved to its last day
/// (31 in February is the 28th or 29th). `None` for a day outside 1–31.
pub(crate) fn clamped_ymd(y: i32, m: u32, d: u32) -> Option<Date> {
    if !(1..=31).contains(&d) {
        return None;
    }
    let last = ymd(y, m, 1)?.days_in_month() as u32;
    ymd(y, m, d.min(last))
}

/// Month names and abbreviations (German, English), lower case.
const MONTHS: [&[&str]; 12] = [
    &["januar", "jänner", "january", "jän", "jan"],
    &["februar", "february", "febr", "feb"],
    &["märz", "maerz", "marz", "march", "mär", "mrz", "mar"],
    &["april", "apr"],
    &["mai", "may"],
    &["juni", "june", "jun"],
    &["juli", "july", "jul"],
    &["august", "aug"],
    &["september", "sept", "sep"],
    &["oktober", "october", "okt", "oct"],
    &["november", "nov"],
    &["dezember", "december", "dez", "dec"],
];

/// 1–12 for a German or English month name or abbreviation, in any case and
/// with or without a trailing dot. Only whole names count, so "Maintal" or
/// "Junior" is no month.
pub(crate) fn month_from_name(s: &str) -> Option<u32> {
    let s = s.trim().trim_end_matches('.').to_lowercase();
    MONTHS
        .iter()
        .position(|names| names.contains(&s.as_str()))
        .map(|i| i as u32 + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date as day;

    #[test]
    fn iso_round_trip() {
        assert_eq!(iso(day(2025, 3, 2)), "2025-03-02");
        assert_eq!(iso(day(999, 1, 1)), "0999-01-01");
        assert_eq!(parse_iso("2025-03-02"), Some(day(2025, 3, 2)));
        assert_eq!(parse_iso("2025-03-02T10:00:00Z"), Some(day(2025, 3, 2)));
        assert_eq!(parse_iso("2025-02-30"), None);
        assert_eq!(parse_iso("02.03.2025"), None);
        assert_eq!(parse_iso(""), None);
    }

    #[test]
    fn german_or_iso() {
        assert_eq!(parse_de_or_iso(" 2025-03-02 "), Some(day(2025, 3, 2)));
        assert_eq!(parse_de_or_iso("02.03.2025"), Some(day(2025, 3, 2)));
        assert_eq!(parse_de_or_iso("1.3.2025"), Some(day(2025, 3, 1)));
        assert_eq!(parse_de_or_iso("31.02.2025"), None);
        assert_eq!(parse_de_or_iso("März 2025"), None);
    }

    #[test]
    fn day_month_without_year() {
        let latest = day(2026, 1, 5);
        assert_eq!(
            day_month_on_or_before(28, 12, latest),
            Some(day(2025, 12, 28))
        );
        assert_eq!(day_month_on_or_before(5, 1, latest), Some(day(2026, 1, 5)));
        assert_eq!(day_month_on_or_before(6, 1, latest), Some(day(2025, 1, 6)));
        assert_eq!(day_month_on_or_before(29, 2, day(2025, 3, 1)), None);
        assert_eq!(day_month_on_or_before(31, 4, latest), None);
    }

    #[test]
    fn every_german_date_in_text() {
        let dates: Vec<_> = de_dates("05.01.2026 bis 31.02.2026, 01.12.2025").collect();
        assert_eq!(dates, [day(2026, 1, 5), day(2025, 12, 1)]);
    }

    #[test]
    fn german_date_in_text() {
        assert_eq!(find_de_date("am 1.2.2026."), Some(day(2026, 2, 1)));
        assert_eq!(find_de_date("31.02.2026"), None);
        assert_eq!(
            find_de_date("Formular 10.20.2024, Stand 01.03.2025"),
            Some(day(2025, 3, 1))
        );
        assert_eq!(find_de_date("Bestellung 112.03.2025"), None);
        assert_eq!(find_de_date("2025-03-02"), None);
    }

    #[test]
    fn compact_dates() {
        assert_eq!(parse_yyyymmdd("20261107"), Some(day(2026, 11, 7)));
        assert_eq!(parse_yyyymmdd("20261107120000"), Some(day(2026, 11, 7)));
        assert_eq!(parse_yyyymmdd("20260230"), None);
        assert_eq!(parse_yyyymmdd("2026+107"), None);
        assert_eq!(parse_yyyymmdd("202611"), None);
    }

    #[test]
    fn years() {
        assert_eq!(year("2025"), Some(2025));
        assert_eq!(year("00"), Some(2000));
        assert_eq!(year("80"), Some(2080));
        assert_eq!(year("81"), Some(1981));
        assert_eq!(year("99"), Some(1999));
        assert_eq!(year("x"), None);
    }

    #[test]
    fn days_past_the_month_end_are_clamped() {
        assert_eq!(clamped_ymd(2024, 2, 30), Some(day(2024, 2, 29)));
        assert_eq!(clamped_ymd(2025, 2, 31), Some(day(2025, 2, 28)));
        assert_eq!(clamped_ymd(2025, 4, 15), Some(day(2025, 4, 15)));
        assert_eq!(clamped_ymd(2025, 4, 0), None);
        assert_eq!(clamped_ymd(2025, 4, 32), None);
        assert_eq!(clamped_ymd(2025, 13, 1), None);
    }

    #[test]
    fn whole_days() {
        assert_eq!(ymd(2024, 2, 29), Some(day(2024, 2, 29)));
        assert_eq!(ymd(2025, 2, 29), None);
        // 257 would wrap to 1 under an `as i8` cast.
        assert_eq!(ymd(2025, 257, 1), None);
        assert_eq!(ymd(40_000, 1, 1), None);
        assert_eq!(add_days(day(2025, 12, 31), 1), Some(day(2026, 1, 1)));
        assert_eq!(add_days(day(9999, 12, 31), 1), None);
        assert_eq!(add_days(day(2025, 1, 1), i64::MAX), None);
        assert_eq!(days_between(day(2025, 3, 1), day(2025, 2, 1)), -28);
    }

    #[test]
    fn month_names() {
        assert_eq!(month_from_name("März"), Some(3));
        assert_eq!(month_from_name("Mär."), Some(3));
        assert_eq!(month_from_name("SEPT"), Some(9));
        assert_eq!(month_from_name("Febr."), Some(2));
        assert_eq!(month_from_name("Jän."), Some(1));
        assert_eq!(month_from_name("Marz"), Some(3));
        assert_eq!(month_from_name("may"), Some(5));
        for word in ["marketing", "Maintal", "Augen", "Junior", "decimal", ""] {
            assert_eq!(month_from_name(word), None, "{word}");
        }
    }
}
