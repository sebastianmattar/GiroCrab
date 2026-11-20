use super::*;

fn d(s: &str) -> Date {
    jiff::civil::Date::strptime("%Y-%m-%d", s).unwrap()
}

fn row(unit: &str, interval: u32, exec_day: u32, first: &str, last: Option<&str>) -> Row {
    Row {
        id: 1,
        account_id: 1,
        account_name: "A".into(),
        name: "N".into(),
        iban: String::new(),
        cents: 100,
        purpose: String::new(),
        first: d(first),
        last: last.map(d),
        unit: unit.into(),
        interval,
        exec_day,
    }
}

fn dates(o: &Row, from: &str, to: &str) -> Vec<String> {
    occurrences(o, d(from), d(to))
        .iter()
        .map(|x| x.to_string())
        .collect()
}

#[test]
fn monthly_on_a_fixed_day() {
    let o = row("M", 1, 5, "2026-01-05", None);
    assert_eq!(
        dates(&o, "2026-11-15", "2027-02-28"),
        ["2026-12-05", "2027-01-05", "2027-02-05"]
    );
}

#[test]
fn day_31_is_the_last_day_and_short_months_clamp() {
    let o = row("M", 1, 31, "2026-01-31", None);
    assert_eq!(
        dates(&o, "2027-01-01", "2027-04-30"),
        ["2027-01-31", "2027-02-28", "2027-03-31", "2027-04-30"]
    );
    let o = row("M", 1, 30, "2026-01-30", None);
    assert_eq!(dates(&o, "2028-02-01", "2028-02-29"), ["2028-02-29"]);
}

#[test]
fn quarterly_and_the_last_date() {
    let o = row("M", 3, 1, "2026-01-01", Some("2026-12-31"));
    assert_eq!(
        dates(&o, "2026-02-01", "2030-01-01"),
        ["2026-04-01", "2026-07-01", "2026-10-01"]
    );
}

#[test]
fn nothing_before_the_first_date() {
    // Execution day 1 but first date 15th: the first run is the next month.
    let o = row("M", 1, 1, "2026-01-15", None);
    assert_eq!(
        dates(&o, "2026-01-01", "2026-03-31"),
        ["2026-02-01", "2026-03-01"]
    );
}

#[test]
fn weekly_and_every_second_week() {
    // 2026-11-02 is a Monday; weekday 3 = Wednesday.
    let o = row("W", 1, 3, "2026-11-02", None);
    assert_eq!(
        dates(&o, "2026-11-15", "2026-11-30"),
        ["2026-11-18", "2026-11-25"]
    );
    let o = row("W", 2, 3, "2026-11-02", None);
    assert_eq!(
        dates(&o, "2026-11-01", "2026-12-10"),
        ["2026-11-04", "2026-11-18", "2026-12-02"]
    );
}

#[test]
fn stored_orders_are_replaced_per_account_and_listed() {
    let v = crate::vault::Vault::open(
        crate::test_support::thread_temp_path("standing")
            .to_string_lossy()
            .into(),
        vec![9u8; 32],
    )
    .unwrap();
    let db = v.db().unwrap();
    db.execute(
        "INSERT INTO account_groups(name, sf_symbol, sort_order) VALUES ('G','tray',1)",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO accounts(group_id, external_ref, name, kind)
         VALUES ((SELECT min(id) FROM account_groups), 'a', 'Giro', 'checking')",
        [],
    )
    .unwrap();
    let acc = db.last_insert_rowid();
    let order = |id: &str, cents: i64| RemoteStandingOrder {
        account_ref: "a".into(),
        order_id: id.into(),
        recipient_name: "Vermieter".into(),
        recipient_iban: "DE89370400440532013000".into(),
        amount_cents: cents,
        purpose: "Miete".into(),
        first_date: "2020-01-01".into(),
        last_date: None,
        time_unit: "M".into(),
        interval: 1,
        exec_day: 1,
        can_change: false,
        can_delete: false,
    };
    replace_for_account(&db, acc, &[&order("A", 85_000), &order("B", 1_000)]).unwrap();
    replace_for_account(&db, acc, &[&order("A", 90_000)]).unwrap();
    let n: i64 = db
        .query_row("SELECT count(*) FROM standing_orders", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
    let rows = planned_executions(&db, d("2026-11-15"), Some(d("2027-01-31")), &[]).unwrap();
    assert_eq!(rows.len(), 2); // 1 Dec and 1 Jan; the 1 Nov run is already past
    assert!(rows.iter().all(|p| p.amount_cents == -90_000));
    assert_eq!(rows[0].date, "2026-12-01");
    assert_eq!(rows[0].rhythm_unit, "M");
    assert!(
        planned_executions(&db, d("2026-11-15"), None, &[999])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn an_open_period_shows_at_most_twelve_executions_per_order() {
    let o = row("W", 1, 3, "2026-01-07", None);
    let v = occurrences(&o, d("2026-11-15"), d("2027-11-15"));
    assert!(v.len() > 12); // the order itself runs weekly; only the list is capped
}

#[test]
fn very_old_orders_still_reach_today() {
    // weekly since 2005: more than 1000 steps before today
    let w = row("W", 1, 3, "2005-01-05", None);
    assert_eq!(
        dates(&w, "2026-11-10", "2026-11-25"),
        ["2026-11-11", "2026-11-18", "2026-11-25"]
    );
    // every second week keeps its rhythm from the first date
    let w2 = row("W", 2, 3, "2005-01-05", None);
    let got = dates(&w2, "2026-11-01", "2026-12-31");
    assert!(!got.is_empty());
    for pair in got.windows(2) {
        assert_eq!(crate::dates::days_between(d(&pair[0]), d(&pair[1])), 14);
    }
    // monthly since 1990 on the 31st
    let m = row("M", 1, 31, "1990-01-31", None);
    assert_eq!(
        dates(&m, "2026-11-01", "2027-01-31"),
        ["2026-11-30", "2026-12-31", "2027-01-31"]
    );
}
