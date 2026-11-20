//! View-models of the "Wiederkehrend & Geplant" dialog without Slint: marking a booking as
//! recurring, the month overview, planned payments, standing orders and the salary cycle.
//! Pure functions over core records, unit-tested on every platform.

use girocrab_core::StandingOrderLimits;
use girocrab_core::planned::{PlannedKind, PlannedPayment, PlannedTotals};
use girocrab_core::recurring::{
    IgnoredRecurring, RecurrenceInterval, RecurringMonthItem, RecurringMonthOverview,
    RecurringSeries, normalize_custom_dates,
};
use girocrab_core::salary::SalaryCycle;
use girocrab_core::standing_orders::StandingOrderInfo;

/// Intervals offered when marking a booking; index = `RecurrenceInterval` order below.
pub const INTERVAL_CHOICES: [&str; 5] = [
    "Monatlich",
    "Vierteljährlich",
    "Halbjährlich",
    "Jährlich",
    "Feste Tage (z. B. 15.03., 15.08.)",
];

pub fn interval_from_index(i: i32) -> RecurrenceInterval {
    match i {
        1 => RecurrenceInterval::Quarterly,
        2 => RecurrenceInterval::HalfYearly,
        3 => RecurrenceInterval::Yearly,
        4 => RecurrenceInterval::Custom,
        _ => RecurrenceInterval::Monthly,
    }
}

pub fn index_of_interval(i: RecurrenceInterval) -> i32 {
    match i {
        RecurrenceInterval::Monthly => 0,
        RecurrenceInterval::Quarterly => 1,
        RecurrenceInterval::HalfYearly => 2,
        RecurrenceInterval::Yearly => 3,
        RecurrenceInterval::Custom => 4,
    }
}

/// Splits "15.03., 15.08" into the entries `normalize_custom_dates` understands.
pub fn parse_custom_dates(text: &str) -> Vec<String> {
    let raw: Vec<String> = text
        .split([',', ';', ' ', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    normalize_custom_dates(&raw)
}

/// What `mark_booking_recurring` is called with, or the message for the user.
pub fn mark_request(
    interval_index: i32,
    custom_text: &str,
) -> Result<(RecurrenceInterval, Vec<String>), String> {
    let interval = interval_from_index(interval_index);
    if interval != RecurrenceInterval::Custom {
        return Ok((interval, Vec::new()));
    }
    let dates = parse_custom_dates(custom_text);
    if dates.is_empty() {
        return Err("Bitte mindestens ein Datum angeben (z. B. 15.03.).".into());
    }
    Ok((interval, dates))
}

/// "03-15" -> "15.03."
pub fn custom_date_label(mmdd: &str) -> String {
    match mmdd.split_once('-') {
        Some((m, d)) => format!("{d}.{m}."),
        None => mmdd.to_string(),
    }
}

pub fn interval_text(s: &RecurringSeries) -> String {
    if s.interval == RecurrenceInterval::Custom {
        let days: Vec<String> = s
            .custom_dates
            .iter()
            .map(|d| custom_date_label(d))
            .collect();
        return format!("Feste Tage: {}", days.join(", "));
    }
    INTERVAL_CHOICES[index_of_interval(s.interval) as usize].to_string()
}

/// The inspector line of the dialog for the selected booking.
pub fn series_text(
    series: Option<&RecurringSeries>,
    has_key: bool,
    date: &dyn Fn(&str) -> String,
) -> String {
    match series {
        Some(s) => {
            let source = match s.source {
                girocrab_core::recurring::RecurringSource::Detected => "automatisch erkannt",
                girocrab_core::recurring::RecurringSource::Manual => "von Ihnen markiert",
            };
            format!(
                "{} · {source} · nächste Fälligkeit {}",
                interval_text(s),
                date(&s.next_due)
            )
        }
        None if has_key => "Nicht als wiederkehrend erkannt.".into(),
        None => "Diese Buchung lässt sich keiner Serie zuordnen (kein Empfänger).".into(),
    }
}

/// One line of a dialog list.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub id: i64,
    pub title: String,
    pub detail: String,
    pub amount: String,
    pub negative: bool,
}

const MONTHS: [&str; 12] = [
    "Januar",
    "Februar",
    "März",
    "April",
    "Mai",
    "Juni",
    "Juli",
    "August",
    "September",
    "Oktober",
    "November",
    "Dezember",
];

pub fn month_title(year: i32, month: u32) -> String {
    let name = MONTHS
        .get((month as usize).wrapping_sub(1))
        .copied()
        .unwrap_or("");
    format!("{name} {year}")
}

fn month_line(
    i: usize,
    it: &RecurringMonthItem,
    money: &dyn Fn(i64) -> String,
    date: &dyn Fn(&str) -> String,
) -> Line {
    let state = if it.booked { "gebucht" } else { "fällig" };
    Line {
        id: i as i64,
        title: it.series.name.clone(),
        detail: format!(
            "{state} am {} · {}",
            date(&it.due_date),
            interval_text(&it.series)
        ),
        amount: money(it.amount_cents),
        negative: it.amount_cents < 0,
    }
}

/// Items of the month: still due first (by date), then the booked ones.
pub fn month_lines(
    o: &RecurringMonthOverview,
    money: &dyn Fn(i64) -> String,
    date: &dyn Fn(&str) -> String,
) -> Vec<Line> {
    let mut items: Vec<&RecurringMonthItem> = o.items.iter().collect();
    items.sort_by(|a, b| {
        a.booked
            .cmp(&b.booked)
            .then_with(|| a.due_date.cmp(&b.due_date))
            .then_with(|| a.series.name.cmp(&b.series.name))
    });
    items
        .iter()
        .enumerate()
        .map(|(i, it)| month_line(i, it, money, date))
        .collect()
}

pub fn month_summary(o: &RecurringMonthOverview, money: &dyn Fn(i64) -> String) -> String {
    format!(
        "{}: noch offen {} Ausgaben, {} Einnahmen · bereits gebucht {} Ausgaben",
        month_title(o.year, o.month),
        money(o.open_expense_cents),
        money(o.open_income_cents),
        money(o.booked_expense_cents)
    )
}

pub fn ignored_lines(
    items: &[IgnoredRecurring],
    money: &dyn Fn(i64) -> String,
    date: &dyn Fn(&str) -> String,
) -> Vec<Line> {
    items
        .iter()
        .enumerate()
        .map(|(i, g)| Line {
            id: i as i64,
            title: g.name.clone(),
            detail: format!("zuletzt {}", date(&g.last_date)),
            amount: money(g.amount_cents),
            negative: g.amount_cents < 0,
        })
        .collect()
}

pub fn planned_kind_label(k: PlannedKind) -> &'static str {
    match k {
        PlannedKind::ScheduledTransfer => "Terminüberweisung",
        PlannedKind::StandingOrder => "Dauerauftrag",
        PlannedKind::DirectDebit => "Lastschrift (Einzug)",
    }
}

/// "alle 3 Monate" / "jede Woche" for standing orders, else "".
pub fn rhythm_text(unit: &str, interval: u32) -> String {
    match (unit, interval) {
        ("M", 1) => "monatlich".into(),
        ("M", n) if n > 1 => format!("alle {n} Monate"),
        ("W", 1) => "wöchentlich".into(),
        ("W", n) if n > 1 => format!("alle {n} Wochen"),
        _ => String::new(),
    }
}

pub fn planned_lines(
    items: &[PlannedPayment],
    money: &dyn Fn(i64) -> String,
    date: &dyn Fn(&str) -> String,
) -> Vec<Line> {
    items
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let mut detail = vec![date(&p.date), planned_kind_label(p.kind).to_string()];
            let rhythm = rhythm_text(&p.rhythm_unit, p.rhythm_interval);
            if !rhythm.is_empty() {
                detail.push(rhythm);
            }
            if !p.account_name.is_empty() {
                detail.push(p.account_name.clone());
            }
            Line {
                id: i as i64,
                title: if p.counterparty.is_empty() {
                    p.purpose.clone()
                } else {
                    p.counterparty.clone()
                },
                detail: detail.join(" · "),
                amount: money(p.amount_cents),
                negative: p.amount_cents < 0,
            }
        })
        .collect()
}

/// Entries of the horizon picker of "Geplant" (like macOS `PlannedHorizon`).
#[allow(dead_code)]
pub const HORIZON_LABELS: [&str; 4] = ["30 Tage", "90 Tage", "1 Jahr", "Alle"];

/// Days ahead for a horizon index; `None` = open end.
pub fn horizon_days(index: i32) -> Option<i64> {
    match index {
        0 => Some(30),
        1 => Some(90),
        2 => Some(365),
        _ => None,
    }
}

/// "November 2026" for an ISO date or `YYYY-MM` key.
pub fn month_heading(iso: &str) -> String {
    let (y, m) = (iso.get(..4).unwrap_or(""), iso.get(5..7).unwrap_or(""));
    match m.parse::<usize>() {
        Ok(n) if (1..=12).contains(&n) => format!("{} {y}", MONTHS[n - 1]),
        _ => iso.to_string(),
    }
}

/// Planned payments with a header line (id -1, month and its signed sum) before each month.
pub fn planned_lines_grouped(
    items: &[PlannedPayment],
    money: &dyn Fn(i64) -> String,
    date: &dyn Fn(&str) -> String,
) -> Vec<Line> {
    let lines = planned_lines(items, money, date);
    let mut out = Vec::with_capacity(lines.len() + 4);
    let mut i = 0;
    while i < items.len() {
        let key = items[i].date.get(..7).unwrap_or("").to_string();
        let mut j = i;
        let mut sum = 0;
        while j < items.len() && items[j].date.get(..7).unwrap_or("") == key {
            sum += items[j].amount_cents;
            j += 1;
        }
        out.push(Line {
            id: -1,
            title: month_heading(&key),
            detail: format!("{} Zahlungen", j - i),
            amount: money(sum),
            negative: sum < 0,
        });
        out.extend(lines[i..j].iter().cloned());
        i = j;
    }
    out
}

pub fn planned_summary(t: &PlannedTotals, badge: u32, money: &dyn Fn(i64) -> String) -> String {
    format!(
        "{} geplante Zahlungen: {} ausgehend, {} eingehend · {badge} Termine/Einzüge bei der Bank",
        t.count,
        money(t.outgoing_cents),
        money(t.incoming_cents)
    )
}

fn unit_text(unit: &str) -> &'static str {
    if unit == "W" { "Wochen" } else { "Monate" }
}

pub fn standing_lines(
    items: &[StandingOrderInfo],
    money: &dyn Fn(i64) -> String,
    date: &dyn Fn(&str) -> String,
) -> Vec<Line> {
    items
        .iter()
        .enumerate()
        .map(|(i, o)| {
            let mut detail = vec![
                format!(
                    "{} · seit {}",
                    rhythm_text(&o.time_unit, o.interval),
                    date(&o.first_date)
                ),
                o.account_name.clone(),
            ];
            if let Some(last) = &o.last_date {
                detail.push(format!("bis {}", date(last)));
            }
            if !o.can_change {
                detail.push("nicht änderbar".into());
            }
            Line {
                id: i as i64,
                title: if o.recipient_name.is_empty() {
                    o.purpose.clone()
                } else {
                    o.recipient_name.clone()
                },
                detail: detail.join(" · "),
                amount: money(-o.amount_cents),
                negative: true,
            }
        })
        .collect()
}

fn list_text(v: &[u32]) -> String {
    v.iter().map(u32::to_string).collect::<Vec<_>>().join(", ")
}

/// What the bank allows for a standing order of the account, or a note that nothing is known.
pub fn limits_text(l: Option<&StandingOrderLimits>) -> String {
    let Some(l) = l else {
        return "Die Bank hat keine Einschränkungen gemeldet.".into();
    };
    let mut parts = Vec::new();
    if l.min_days_ahead > 0 || l.max_days_ahead > 0 {
        parts.push(format!(
            "Erste Ausführung {}–{} Tage im Voraus",
            l.min_days_ahead, l.max_days_ahead
        ));
    }
    if !l.months.is_empty() {
        parts.push(format!("{}: {}", unit_text("M"), list_text(&l.months)));
    }
    if !l.weeks.is_empty() {
        parts.push(format!("{}: {}", unit_text("W"), list_text(&l.weeks)));
    }
    if !l.days_of_month.is_empty() {
        parts.push(format!("Monatstage: {}", list_text(&l.days_of_month)));
    }
    if !l.weekdays.is_empty() {
        parts.push(format!("Wochentage: {}", list_text(&l.weekdays)));
    }
    if parts.is_empty() {
        "Die Bank hat keine Einschränkungen gemeldet.".into()
    } else {
        parts.join(" · ")
    }
}

pub fn salary_cycle_text(
    c: Option<&SalaryCycle>,
    money: &dyn Fn(i64) -> String,
    date: &dyn Fn(&str) -> String,
) -> String {
    match c {
        Some(c) => format!(
            "Gehaltszyklus: {} am {} von {} · nächstes Gehalt erwartet {}",
            money(c.amount_cents),
            date(&c.last_date),
            c.counterparty,
            date(&c.next_expected)
        ),
        None => "Kein Gehalt in den letzten Wochen erkannt.".into(),
    }
}

/// How the selected booking is treated as salary.
pub fn salary_state_text(salary: bool, flag: Option<bool>, signal: bool, inflow: bool) -> String {
    if !inflow {
        return "Nur Eingänge können Gehalt sein.".into();
    }
    match (flag, salary) {
        (Some(true), _) => "Von Ihnen als Gehalt markiert.".into(),
        (Some(false), _) => "Von Ihnen als kein Gehalt markiert.".into(),
        (None, true) if signal => "Automatisch als Gehalt erkannt.".into(),
        (None, true) => "Als Gehalt gezählt (Absender von Ihnen markiert).".into(),
        (None, false) => "Kein Gehalt (automatisch).".into(),
    }
}

/// Dialog choice (0 automatic, 1 salary, 2 not salary) -> `set_booking_salary_flag` argument.
pub fn salary_flag_for(choice: i32) -> Option<bool> {
    match choice {
        1 => Some(true),
        2 => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use girocrab_core::recurring::RecurringSource;

    fn eur(c: i64) -> String {
        format!("{:.2}", c as f64 / 100.0)
    }
    fn plain(s: &str) -> String {
        s.to_string()
    }

    fn series(name: &str, interval: RecurrenceInterval) -> RecurringSeries {
        RecurringSeries {
            series_key: format!("-n:{name}"),
            base_key: String::new(),
            amount_specific: false,
            name: name.into(),
            category_name: String::new(),
            category_symbol: String::new(),
            merchant_domain: None,
            account_id: 1,
            interval,
            custom_dates: vec!["03-15".into(), "08-15".into()],
            amount_cents: -999,
            last_date: "2026-10-01".into(),
            last_booking_id: 1,
            next_due: "2026-11-01".into(),
            occurrences: 5,
            source: RecurringSource::Detected,
        }
    }

    fn item(name: &str, due: &str, booked: bool) -> RecurringMonthItem {
        RecurringMonthItem {
            series: series(name, RecurrenceInterval::Monthly),
            due_date: due.into(),
            booked,
            booking_id: None,
            amount_cents: -500,
        }
    }

    #[test]
    fn custom_dates_accept_german_and_iso() {
        assert_eq!(
            parse_custom_dates("15.08., 15.3 ; 03-15"),
            vec!["03-15", "08-15"]
        );
        assert!(parse_custom_dates("  ").is_empty());
    }

    #[test]
    fn mark_request_needs_dates_only_for_custom() {
        assert_eq!(
            mark_request(1, "").unwrap(),
            (RecurrenceInterval::Quarterly, Vec::new())
        );
        assert!(mark_request(4, "").is_err());
        let (i, d) = mark_request(4, "15.03.").unwrap();
        assert_eq!(
            (i, d),
            (RecurrenceInterval::Custom, vec!["03-15".to_string()])
        );
    }

    #[test]
    fn interval_index_round_trips() {
        for i in 0..5 {
            assert_eq!(index_of_interval(interval_from_index(i)), i);
        }
        assert_eq!(interval_from_index(99), RecurrenceInterval::Monthly);
    }

    #[test]
    fn interval_text_lists_custom_days() {
        assert_eq!(
            interval_text(&series("a", RecurrenceInterval::Custom)),
            "Feste Tage: 15.03., 15.08."
        );
        assert_eq!(
            interval_text(&series("a", RecurrenceInterval::Yearly)),
            "Jährlich"
        );
    }

    #[test]
    fn series_text_covers_the_three_cases() {
        let s = series("a", RecurrenceInterval::Monthly);
        assert!(series_text(Some(&s), true, &plain).contains("automatisch erkannt"));
        assert!(series_text(None, true, &plain).contains("Nicht als wiederkehrend"));
        assert!(series_text(None, false, &plain).contains("keiner Serie"));
    }

    #[test]
    fn month_lines_put_open_items_first() {
        let o = RecurringMonthOverview {
            year: 2026,
            month: 11,
            items: vec![
                item("B gebucht", "2026-11-02", true),
                item("C offen", "2026-11-20", false),
                item("A offen", "2026-11-10", false),
            ],
            open_expense_cents: 1000,
            open_income_cents: 0,
            booked_expense_cents: 500,
        };
        let lines = month_lines(&o, &eur, &plain);
        let names: Vec<&str> = lines.iter().map(|l| l.title.as_str()).collect();
        assert_eq!(names, ["A offen", "C offen", "B gebucht"]);
        assert!(lines[0].detail.starts_with("fällig am"));
        assert!(lines[2].detail.starts_with("gebucht am"));
        assert!(month_summary(&o, &eur).starts_with("November 2026"));
    }

    #[test]
    fn month_title_tolerates_bad_month() {
        assert_eq!(month_title(2026, 3), "März 2026");
        assert_eq!(month_title(2026, 0), " 2026");
    }

    #[test]
    fn ignored_lines_show_last_date() {
        let g = IgnoredRecurring {
            series_key: "k".into(),
            name: "Studio".into(),
            amount_cents: -2000,
            last_date: "2026-09-30".into(),
        };
        let l = ignored_lines(&[g], &eur, &plain);
        assert_eq!(l[0].detail, "zuletzt 2026-09-30");
        assert!(l[0].negative);
    }

    #[test]
    fn rhythm_texts() {
        assert_eq!(rhythm_text("M", 1), "monatlich");
        assert_eq!(rhythm_text("M", 3), "alle 3 Monate");
        assert_eq!(rhythm_text("W", 2), "alle 2 Wochen");
        assert_eq!(rhythm_text("", 0), "");
    }

    #[test]
    fn planned_lines_describe_kind_and_rhythm() {
        let p = PlannedPayment {
            id: "so-1".into(),
            kind: PlannedKind::StandingOrder,
            account_id: 1,
            account_name: "Giro".into(),
            date: "2026-12-01".into(),
            counterparty: String::new(),
            counterparty_iban: String::new(),
            amount_cents: -70000,
            purpose: "Miete".into(),
            rhythm_unit: "M".into(),
            rhythm_interval: 1,
        };
        let l = planned_lines(&[p], &eur, &plain);
        assert_eq!(l[0].title, "Miete");
        assert_eq!(l[0].detail, "2026-12-01 · Dauerauftrag · monatlich · Giro");
        assert!(l[0].negative);
    }

    #[test]
    fn planned_summary_mentions_all_numbers() {
        let t = PlannedTotals {
            count: 3,
            outgoing_cents: -1000,
            incoming_cents: 500,
        };
        let s = planned_summary(&t, 2, &eur);
        assert!(
            s.contains("3 geplante")
                && s.contains("-10.00")
                && s.contains("5.00")
                && s.contains("2 Termine")
        );
    }

    #[test]
    fn standing_lines_show_outgoing_amount() {
        let o = StandingOrderInfo {
            id: 1,
            account_id: 1,
            account_name: "Giro".into(),
            order_id: String::new(),
            recipient_name: "Vermieter".into(),
            recipient_iban: String::new(),
            amount_cents: 70000,
            purpose: String::new(),
            first_date: "2025-01-01".into(),
            last_date: Some("2027-01-01".into()),
            time_unit: "M".into(),
            interval: 1,
            exec_day: 1,
            can_change: false,
            can_delete: true,
        };
        let l = standing_lines(&[o], &eur, &plain);
        assert_eq!(l[0].amount, "-700.00");
        assert!(l[0].detail.contains("bis 2027-01-01") && l[0].detail.contains("nicht änderbar"));
    }

    #[test]
    fn limits_text_lists_what_is_known() {
        assert!(limits_text(None).contains("keine Einschränkungen"));
        assert!(
            limits_text(Some(&StandingOrderLimits::default())).contains("keine Einschränkungen")
        );
        let l = StandingOrderLimits {
            min_days_ahead: 2,
            max_days_ahead: 90,
            months: vec![1, 3],
            weeks: vec![],
            days_of_month: vec![1, 31],
            weekdays: vec![1],
        };
        let t = limits_text(Some(&l));
        assert!(
            t.contains("2–90 Tage")
                && t.contains("Monate: 1, 3")
                && t.contains("Monatstage: 1, 31")
                && t.contains("Wochentage: 1")
        );
    }

    #[test]
    fn salary_texts_and_flags() {
        assert_eq!(salary_flag_for(0), None);
        assert_eq!(salary_flag_for(1), Some(true));
        assert_eq!(salary_flag_for(2), Some(false));
        assert!(salary_state_text(false, None, false, false).contains("Nur Eingänge"));
        assert!(salary_state_text(true, Some(true), true, true).contains("markiert"));
        assert!(salary_state_text(true, None, true, true).contains("Automatisch"));
        assert!(salary_state_text(true, None, false, true).contains("Absender"));
        assert!(salary_state_text(false, None, false, true).contains("Kein Gehalt"));
        let c = SalaryCycle {
            last_date: "2026-10-28".into(),
            next_expected: "2026-11-28".into(),
            amount_cents: 300000,
            counterparty: "Arbeitgeber".into(),
            booking_id: 1,
        };
        assert!(salary_cycle_text(Some(&c), &eur, &plain).contains("Arbeitgeber"));
        assert!(salary_cycle_text(None, &eur, &plain).contains("Kein Gehalt"));
    }
}
