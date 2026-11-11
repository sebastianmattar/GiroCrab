//! Gehaltserkennung (GitHub #37): detects salary / pension inflows so the
//! income view can follow the user's pay cycle instead of the calendar month.
//!
//! A booking counts as salary when it is an inflow and
//! * the user marked it (`transactions.salary_flag = 1`), or the user marked
//!   another inflow from the same counterparty IBAN, or
//! * its SEPA purpose code is `SALA`, `PENS`, `BONU`, `PAYR` or `SPSP`, or
//! * its Geschäftsvorfallcode is `153` (Lohn-, Gehalts-, Rentengutschrift), or
//! * the purpose / booking text contains a typical keyword (Lohn, Gehalt, Rente, …).
//!
//! `salary_flag = 0` always wins ("kein Gehalt").

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use chrono::{Duration, Local, Months, NaiveDate};
use regex::Regex;
use rusqlite::{params, Connection};

use crate::error::{CoreError, CoreResult};
use crate::i18n::tr;
use crate::models::{Booking, BookingFilter};
use crate::vault::Vault;

/// ISO 20022 / SEPA purpose codes for salary-like payments.
pub const SALARY_PURPOSE_CODES: [&str; 5] = ["SALA", "PENS", "BONU", "PAYR", "SPSP"];
/// DK Geschäftsvorfallcodes for salary / pension credits.
pub const SALARY_GVCS: [&str; 1] = ["153"];

/// How far back `salary_cycle` looks for salary inflows.
const LOOKBACK_DAYS: i64 = 200;
/// A salary older than this no longer defines the current cycle.
const STALE_DAYS: i64 = 45;

static KEYWORDS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(gehalt|lohn|löhne|arbeitslohn|bez(ü|ue)ge|dienstbez|versorgungsbez|besoldung|rente\b|renten(zahlung|gutschrift)|altersrente|pension|ruhegehalt|salary|payroll|wages)",
    )
    .expect("salary keyword regex")
});

/// Pure signal check without user context (flag / marked IBANs).
pub fn salary_signal(b: &Booking) -> bool {
    if b.amount_cents <= 0 {
        return false;
    }
    let code = b.purpose_code.trim().to_ascii_uppercase();
    if SALARY_PURPOSE_CODES.contains(&code.as_str()) {
        return true;
    }
    if SALARY_GVCS.contains(&b.gvc.trim()) {
        return true;
    }
    KEYWORDS.is_match(&b.purpose) || KEYWORDS.is_match(&b.booking_text)
}

/// Full decision incl. the user's flag and IBANs the user marked as salary source.
pub fn is_salary(b: &Booking, marked_ibans: &HashSet<String>) -> bool {
    match b.salary_flag {
        Some(f) => f && b.amount_cents > 0,
        None => {
            salary_signal(b)
                || (b.amount_cents > 0
                    && !b.counterparty_iban.is_empty()
                    && marked_ibans.contains(&normalize_iban(&b.counterparty_iban)))
        }
    }
}

fn normalize_iban(iban: &str) -> String {
    iban.chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_uppercase()
}

/// Counterparty IBANs of inflows the user explicitly marked as salary.
pub(crate) fn marked_salary_ibans(conn: &Connection) -> CoreResult<HashSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT counterparty_iban FROM transactions
         WHERE salary_flag = 1 AND counterparty_iban != '' AND amount_cents > 0",
    )?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut out = HashSet::new();
    for r in rows {
        out.insert(normalize_iban(&r?));
    }
    Ok(out)
}

/// The user's current pay cycle, derived from the main salary inflow.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct SalaryCycle {
    /// Booking date of the latest main salary (start of the current cycle, YYYY-MM-DD).
    pub last_date: String,
    /// Expected date of the next salary (YYYY-MM-DD).
    pub next_expected: String,
    pub amount_cents: i64,
    pub counterparty: String,
    pub booking_id: i64,
}

/// Picks the main salary series and derives the cycle. `today` is injected for tests.
pub(crate) fn derive_cycle(salaries: &[Booking], today: NaiveDate) -> Option<SalaryCycle> {
    // Group by payer (IBAN, else name); the main series has the largest total.
    let mut groups: HashMap<String, Vec<&Booking>> = HashMap::new();
    for b in salaries {
        let key = if b.counterparty_iban.is_empty() {
            b.counterparty.trim().to_lowercase()
        } else {
            normalize_iban(&b.counterparty_iban)
        };
        groups.entry(key).or_default().push(b);
    }
    let (_, mut main) = groups
        .into_iter()
        .max_by_key(|(_, v)| v.iter().map(|b| b.amount_cents).sum::<i64>())?;
    main.sort_by(|a, b| a.booking_date.cmp(&b.booking_date));
    let last = *main.last()?;
    let last_date = NaiveDate::parse_from_str(&last.booking_date, "%Y-%m-%d").ok()?;
    if (today - last_date).num_days() > STALE_DAYS {
        return None;
    }
    // Typical interval: median gap between consecutive salaries, else one month.
    let mut gaps: Vec<i64> = main
        .windows(2)
        .filter_map(|w| {
            let a = NaiveDate::parse_from_str(&w[0].booking_date, "%Y-%m-%d").ok()?;
            let b = NaiveDate::parse_from_str(&w[1].booking_date, "%Y-%m-%d").ok()?;
            Some((b - a).num_days())
        })
        .filter(|d| *d >= 7) // same-month extras (bonus) don't define the cycle
        .collect();
    gaps.sort_unstable();
    let next = match gaps.get(gaps.len() / 2) {
        Some(&g) if !(25..=35).contains(&g) => last_date + Duration::days(g),
        _ => last_date
            .checked_add_months(Months::new(1))
            .unwrap_or(last_date + Duration::days(30)),
    };
    Some(SalaryCycle {
        last_date: last.booking_date.clone(),
        next_expected: next.format("%Y-%m-%d").to_string(),
        amount_cents: last.amount_cents,
        counterparty: if last.merchant_name.is_empty() {
            last.counterparty.clone()
        } else {
            last.merchant_name.clone()
        },
        booking_id: last.id,
    })
}

#[uniffi::export]
impl Vault {
    /// Marks a booking as salary (`Some(true)`), as not salary (`Some(false)`)
    /// or resets it to automatic detection (`None`). Marking an inflow also
    /// makes future inflows from the same IBAN count as salary.
    pub fn set_booking_salary_flag(
        &self,
        booking_id: i64,
        flag: Option<bool>,
    ) -> Result<(), CoreError> {
        let n = self.db()?.execute(
            "UPDATE transactions SET salary_flag = ?1 WHERE id = ?2",
            params![flag.map(i64::from), booking_id],
        )?;
        if n == 0 {
            return Err(CoreError::NotFound {
                what: tr("Buchung").into(),
            });
        }
        Ok(())
    }

    /// Current pay cycle of the given accounts (empty = all), or `None` when no
    /// salary was detected within the last ~6 weeks.
    pub fn salary_cycle(&self, account_ids: Vec<i64>) -> Result<Option<SalaryCycle>, CoreError> {
        let today = Local::now().date_naive();
        let from = (today - Duration::days(LOOKBACK_DAYS))
            .format("%Y-%m-%d")
            .to_string();
        let page = self.list_transactions(BookingFilter {
            account_ids,
            date_from: Some(from),
            ..Default::default()
        })?;
        let salaries: Vec<Booking> = page
            .transactions
            .into_iter()
            .filter(|b| b.is_salary)
            .collect();
        Ok(derive_cycle(&salaries, today))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merchants::{FlowKind, MerchantResolver};
    use crate::models::{AccountKind, CategorySource};

    fn b(id: i64, date: &str, cents: i64, iban: &str, purpose: &str) -> Booking {
        Booking {
            id,
            account_id: 1,
            account_name: String::new(),
            booking_date: date.into(),
            value_date: date.into(),
            amount_cents: cents,
            currency: "EUR".into(),
            counterparty: "ACME GmbH".into(),
            counterparty_iban: iban.into(),
            purpose: purpose.into(),
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
            flow: FlowKind::Income,
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

    #[test]
    fn signals() {
        let none = HashSet::new();
        let mut x = b(1, "2026-10-28", 300_000, "", "Überweisung");
        assert!(!is_salary(&x, &none));
        x.purpose_code = "SALA".into();
        assert!(is_salary(&x, &none));
        x.purpose_code = "pens".into();
        assert!(is_salary(&x, &none));
        x.purpose_code.clear();
        x.gvc = "153".into();
        assert!(is_salary(&x, &none));
        x.gvc.clear();
        x.purpose = "GEHALT 10/2026 PERS.NR 4711".into();
        assert!(is_salary(&x, &none));
        x.purpose = "Altersrente November".into();
        assert!(is_salary(&x, &none));
        // outflows never count, the user's "no" always wins
        x.amount_cents = -300_000;
        assert!(!is_salary(&x, &none));
        x.amount_cents = 300_000;
        x.salary_flag = Some(false);
        assert!(!is_salary(&x, &none));
        // no false positive on unrelated words
        let y = b(2, "2026-10-28", 5_000, "", "Belohnung Prämie Gewinnspiel");
        assert!(!is_salary(&y, &none));
        // marked IBAN carries over to other inflows of the same payer
        let z = b(
            3,
            "2026-10-28",
            250_000,
            "DE02 1203 0000 0000 2020 51",
            "Zahlung",
        );
        let marked: HashSet<String> = ["DE02120300000000202051".to_string()].into();
        assert!(is_salary(&z, &marked));
    }

    #[test]
    fn cycle_follows_month_end_salary() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 11).unwrap();
        let s = vec![
            b(1, "2026-08-28", 300_000, "DE1", "Gehalt"),
            b(2, "2026-09-29", 300_000, "DE1", "Gehalt"),
            b(3, "2026-10-28", 300_000, "DE1", "Gehalt"),
            b(4, "2026-11-02", 20_000, "DE2", "Rente"), // smaller second income
        ];
        let c = derive_cycle(&s, today).unwrap();
        assert_eq!(c.last_date, "2026-10-28");
        assert_eq!(c.next_expected, "2026-11-28");
        assert_eq!(c.booking_id, 3);
    }

    #[test]
    fn stale_salary_gives_no_cycle() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 11).unwrap();
        let s = vec![b(1, "2026-08-28", 300_000, "DE1", "Gehalt")];
        assert_eq!(derive_cycle(&s, today), None);
        assert_eq!(derive_cycle(&[], today), None);
    }

    #[test]
    fn quarterly_payer_uses_its_interval() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 11).unwrap();
        let s = vec![
            b(1, "2026-04-01", 100_000, "DE1", "Pension"),
            b(2, "2026-07-01", 100_000, "DE1", "Pension"),
            b(3, "2026-10-01", 100_000, "DE1", "Pension"),
        ];
        let c = derive_cycle(&s, today).unwrap();
        assert_eq!(c.next_expected, "2027-01-01");
    }
}

#[cfg(test)]
mod vault_tests {
    use super::*;

    #[test]
    fn marking_an_inflow_carries_over_to_its_payer() {
        let dir = std::env::temp_dir().join(format!("gc-salary-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let v = Vault::open(dir.join("v.db").to_string_lossy().into(), vec![5u8; 32]).unwrap();
        let today = Local::now().date_naive();
        let d = |n: i64| (today - Duration::days(n)).format("%Y-%m-%d").to_string();
        let ids: Vec<i64> = {
            let c = v.db().unwrap();
            c.execute("INSERT INTO account_groups(name) VALUES ('G')", [])
                .unwrap();
            c.execute(
                "INSERT INTO accounts(group_id, external_ref, name) VALUES ((SELECT id FROM account_groups), 'A', 'K')",
                [],
            )
            .unwrap();
            let mut ids = Vec::new();
            for (ext, days, iban) in [("a", 40, "DE99"), ("b", 10, "DE99"), ("c", 5, "DE77")] {
                c.execute(
                    "INSERT INTO transactions(account_id, external_id, booking_date, amount_cents, counterparty, counterparty_iban, purpose)
                     VALUES ((SELECT id FROM accounts LIMIT 1), ?1, ?2, 250000, 'Firma', ?3, 'Zahlung')",
                    params![ext, d(days), iban],
                )
                .unwrap();
                ids.push(c.last_insert_rowid());
            }
            ids
        };
        assert_eq!(v.salary_cycle(vec![]).unwrap(), None, "no signal yet");
        v.set_booking_salary_flag(ids[0], Some(true)).unwrap();
        let c = v.salary_cycle(vec![]).unwrap().expect("cycle");
        assert_eq!(c.last_date, d(10), "same IBAN counts as salary");
        assert_eq!(c.booking_id, ids[1]);
        v.set_booking_salary_flag(ids[1], Some(false)).unwrap();
        assert_eq!(v.salary_cycle(vec![]).unwrap().unwrap().last_date, d(40));
        v.set_booking_salary_flag(ids[1], None).unwrap();
        assert_eq!(v.salary_cycle(vec![]).unwrap().unwrap().last_date, d(10));
        assert!(v.set_booking_salary_flag(9_999, Some(true)).is_err());
        drop(v);
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod demo_tests {
    use super::*;

    #[test]
    fn demo_vault_has_a_pay_cycle() {
        let dir = std::env::temp_dir().join(format!("gc-salary-demo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let v = Vault::open(dir.join("v.db").to_string_lossy().into(), vec![6u8; 32]).unwrap();
        v.seed_demo_data().unwrap();
        let c = v
            .salary_cycle(vec![])
            .unwrap()
            .expect("demo salary detected");
        let expect = (Local::now().date_naive() - Duration::days(22))
            .format("%Y-%m-%d")
            .to_string();
        assert_eq!(c.last_date, expect);
        assert_eq!(c.amount_cents, 365_000);
        drop(v);
        let _ = std::fs::remove_dir_all(dir);
    }
}
