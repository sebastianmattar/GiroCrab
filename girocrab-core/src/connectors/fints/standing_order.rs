//! Standing orders: create (HKCDE), change (HKCDN) and delete (HKCDL), #496.
//!
//! Layout after the public FinTS specification (mirrored in hbci4java):
//! - HKCDE: header, KTI, SEPA descriptor, pain.001 (binary), schedule
//! - HKCDN / HKCDL: header, KTI, descriptor, pain.001, date (unused), order id, schedule
//!
//! The schedule is `first date : M|W : interval : execution day : [time (HKCDL v2) :] last date`.
//! The pain.001 carries the payment; its execution date is a placeholder
//! (`1999-01-01`), the real dates live in the schedule. The parameter segments
//! HICDES / HICDNS / HICDLS list the allowed intervals and days as one string of
//! two-digit numbers (`00` = all).

use serde::{Deserialize, Serialize};

use super::syntax::{Segment, SegmentBuilder};
use super::{FinTsConnector, PreparedOrder, SepaAccount, Vop, sepa};
use crate::connectors::{ConnectContext, Phase, StandingOrderJob};
use crate::error::{CoreError, CoreResult};
use crate::i18n::{tr, trf};
use crate::models::StandingOrderAction;

/// Execution date in the pain.001 of a standing order: the schedule carries the real ones.
const PAIN_PLACEHOLDER_DATE: &str = "1999-01-01";

/// What the bank allows when creating or changing an order (BPD HICDES / HICDNS).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct StandingRules {
    /// Days the first execution must lie ahead at least / at most (0 = no limit).
    min_pretime: u32,
    max_pretime: u32,
    /// Allowed intervals (months): two-digit numbers in a row, empty or `00` = all.
    turnus_months: String,
    /// Allowed days of the month, same notation.
    days_per_month: String,
    turnus_weeks: String,
    /// Allowed weekdays: single digits 1–7 in a row, empty or `0` = all.
    days_per_week: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct StandingParams {
    pub(super) create: Option<StandingRules>,
    pub(super) change: Option<StandingRules>,
}

/// HICDES: header, max. orders, signatures, class, then the parameter DEG:
/// max. usage lines, min./max. lead time, the allowed intervals and days.
pub(super) fn parse_create_rules(seg: &Segment) -> StandingRules {
    let n = |c: usize| seg.get(4, c).parse().unwrap_or(0);
    StandingRules {
        min_pretime: n(1),
        max_pretime: n(2),
        turnus_months: seg.get(4, 3).to_string(),
        days_per_month: seg.get(4, 4).to_string(),
        turnus_weeks: seg.get(4, 5).to_string(),
        days_per_week: seg.get(4, 6).to_string(),
    }
}

/// HICDNS: like HICDES, but with the number of date changes first and nine
/// "field is editable" flags before the lists.
pub(super) fn parse_change_rules(seg: &Segment) -> StandingRules {
    let n = |c: usize| seg.get(4, c).parse().unwrap_or(0);
    StandingRules {
        min_pretime: n(1),
        max_pretime: n(2),
        turnus_months: seg.get(4, 12).to_string(),
        days_per_month: seg.get(4, 13).to_string(),
        turnus_weeks: seg.get(4, 14).to_string(),
        days_per_week: seg.get(4, 15).to_string(),
    }
}

/// `value` in a string of `width`-digit numbers; empty or all-zero = no restriction.
fn allowed_in(list: &str, width: usize, value: u32) -> bool {
    let list = list.trim();
    if list.is_empty() || list.bytes().all(|b| b == b'0') {
        return true;
    }
    let want = format!("{value:0width$}");
    list.as_bytes().chunks(width).any(|c| c == want.as_bytes())
}

/// The numbers in `list` (`width` digits each); empty for "no restriction".
fn numbers_in(list: &str, width: usize) -> Vec<u32> {
    let list = list.trim();
    if list.is_empty() || list.bytes().all(|b| b == b'0') {
        return Vec::new();
    }
    list.as_bytes()
        .chunks(width)
        .filter_map(|c| std::str::from_utf8(c).ok()?.parse().ok())
        .collect()
}

impl StandingRules {
    fn limits(&self) -> crate::models::StandingOrderLimits {
        crate::models::StandingOrderLimits {
            min_days_ahead: self.min_pretime,
            max_days_ahead: self.max_pretime,
            months: numbers_in(&self.turnus_months, 2),
            weeks: numbers_in(&self.turnus_weeks, 2),
            days_of_month: numbers_in(&self.days_per_month, 2),
            weekdays: numbers_in(&self.days_per_week, 1),
        }
    }
}

/// The limits stored in a FinTS connection's state for creating or changing
/// (`None` = the bank stated none, or the state is not FinTS).
pub(crate) fn limits_from_state(
    state: &crate::secret::SecretJson,
    action: StandingOrderAction,
) -> Option<crate::models::StandingOrderLimits> {
    let st: super::FinTsState = state.decode().ok()?;
    let rules = match action {
        StandingOrderAction::Create => st.standing_params.create,
        StandingOrderAction::Change => st.standing_params.change,
        StandingOrderAction::Delete => None,
    }?;
    Some(rules.limits())
}

/// Checks the schedule of `job` against the bank's rules before anything is sent.
pub(super) fn check_schedule(
    rules: &StandingRules,
    job: &StandingOrderJob,
    today: jiff::civil::Date,
) -> CoreResult<()> {
    let invalid = |m: String| Err(CoreError::InvalidInput { message: m });
    let first = jiff::civil::Date::strptime("%Y-%m-%d", &job.first_date).map_err(|_| {
        CoreError::InvalidInput {
            message: tr("Der erste Termin ist ungültig (JJJJ-MM-TT).").into(),
        }
    })?;
    let ahead = crate::dates::days_between(today, first);
    if ahead < i64::from(rules.min_pretime) {
        return invalid(trf(
            "Der erste Termin muss mindestens {0} Tage in der Zukunft liegen.",
            &[&rules.min_pretime],
        ));
    }
    if rules.max_pretime > 0 && ahead > i64::from(rules.max_pretime) {
        return invalid(trf(
            "Der erste Termin darf höchstens {0} Tage in der Zukunft liegen.",
            &[&rules.max_pretime],
        ));
    }
    let weekly = job.time_unit == "W";
    let (turnus, day) = if weekly {
        (&rules.turnus_weeks, &rules.days_per_week)
    } else {
        (&rules.turnus_months, &rules.days_per_month)
    };
    if !allowed_in(turnus, 2, job.interval) {
        return invalid(tr("Die Bank erlaubt diesen Rhythmus nicht.").into());
    }
    if !allowed_in(day, if weekly { 1 } else { 2 }, job.exec_day) {
        return invalid(tr("Die Bank erlaubt diesen Ausführungstag nicht.").into());
    }
    Ok(())
}

/// FinTS date `YYYYMMDD` from `YYYY-MM-DD`.
fn fints_date(iso: &str) -> String {
    iso.replace('-', "")
}

/// The schedule element (DauerDetails, v2 of HKCDL with the empty time of day).
pub(super) fn schedule_components(job: &StandingOrderJob, with_time: bool) -> Vec<String> {
    let mut c = vec![
        fints_date(&job.first_date),
        job.time_unit.clone(),
        job.interval.to_string(),
        job.exec_day.to_string(),
    ];
    // HKCDL v2 has the time of day before the last date; trailing empty
    // components are left out.
    if let Some(last) = job.last_date.as_deref().filter(|l| !l.is_empty()) {
        if with_time {
            c.push(String::new());
        }
        c.push(fints_date(last));
    }
    c
}

/// What `order_payload` adds after the pain.001 of a standing order.
#[derive(Clone)]
pub(super) struct StandingTail {
    /// HKCDN / HKCDL carry the order id; HKCDE does not.
    pub(super) order_id: Option<String>,
    pub(super) schedule: Vec<String>,
}

impl StandingTail {
    /// The elements after the binary pain.001.
    pub(super) fn append(&self, seg: SegmentBuilder) -> SegmentBuilder {
        let refs: Vec<&str> = self.schedule.iter().map(String::as_str).collect();
        match &self.order_id {
            // date (not used), order id, schedule
            Some(id) => seg.raw("").de(&[id]).de(&refs),
            None => seg.de(&refs),
        }
    }
}

impl FinTsConnector {
    /// Sends HKCDE / HKCDN / HKCDL + HKTAN process 4. No name check: standing
    /// orders are not among the orders the name check covers.
    pub(super) async fn start_standing_order(
        &mut self,
        ctx: &ConnectContext,
        job: &StandingOrderJob,
    ) -> CoreResult<Phase> {
        let seg_id: &'static str = match job.action {
            StandingOrderAction::Create => "HKCDE",
            StandingOrderAction::Change => "HKCDN",
            StandingOrderAction::Delete => "HKCDL",
        };
        let not_allowed = || CoreError::Bank {
            code: seg_id.into(),
            message: trf(
                "Dieses Konto oder diese Bank erlaubt diese Änderung am Dauerauftrag per FinTS ({0}) nicht. Bitte in der Banking-App ändern.",
                &[&seg_id],
            ),
        };
        if !self.bank_supports(seg_id) {
            return Err(not_allowed());
        }
        if job.action != StandingOrderAction::Create && job.order_id.trim().is_empty() {
            return Err(CoreError::InvalidInput {
                message: tr("Die Bank hat für diesen Dauerauftrag keine Auftragsnummer gemeldet. Bitte in der Banking-App ändern.").into(),
            });
        }
        self.require_tan_method(tr("Für Überweisungen ist ein TAN-Verfahren nötig; es wurde keines gefunden. Bitte zuerst eine Synchronisation mit TAN-Verfahren durchführen."))?;
        let Some(acc) = self.source_account(ctx, &job.source_iban, seg_id).await? else {
            return Err(not_allowed());
        };
        // The bank's limits, when it stated them (a delete has none to check).
        let rules = match job.action {
            StandingOrderAction::Create => self.st.standing_params.create.clone(),
            StandingOrderAction::Change => self.st.standing_params.change.clone(),
            StandingOrderAction::Delete => None,
        };
        if let Some(rules) = rules {
            check_schedule(&rules, job, crate::dates::today())?;
        }
        let version = self.pain_version(seg_id).unwrap_or(sepa::PainVersion::V03);
        let with_time = seg_id == "HKCDL" && self.version(seg_id) >= 2;
        self.order = Some(standing_order(job, &acc, seg_id, version, with_time)?);
        let reply = self.send_order(ctx, Vop::None).await?;
        self.finish_submit(&reply)
    }
}

/// The standing order (HKCDE/HKCDN/HKCDL) as it is sent: the pain.001 from the
/// job and the source account, then order id and schedule. Pure, so
/// `invariant_tests` can follow the values from the job to the segment bytes.
pub(super) fn standing_order(
    job: &StandingOrderJob,
    acc: &SepaAccount,
    seg_id: &'static str,
    version: sepa::PainVersion,
    with_time: bool,
) -> CoreResult<PreparedOrder> {
    let header = sepa::PainHeader::new(&job.source_owner);
    let xml = sepa::build_pain001(&sepa::Pain001 {
        message_id: &header.message_id,
        created: &header.created,
        execution_date: PAIN_PLACEHOLDER_DATE,
        debtor_name: header.owner,
        debtor_iban: &acc.iban,
        debtor_bic: &acc.bic,
        creditor_name: &job.recipient_name,
        creditor_iban: &job.recipient_iban,
        creditor_bic: job.recipient_bic.as_deref(),
        amount_cents: job.amount_cents,
        purpose: &job.purpose,
        version,
        end_to_end_id: "",
        instant: false,
    })?;
    Ok(PreparedOrder {
        seg_id,
        kti: FinTsConnector::kti(acc),
        descriptor: version.descriptor(),
        xml,
        instant: false,
        recipient_name: job.recipient_name.clone(),
        recipient_iban: job.recipient_iban.clone(),
        batch: None,
        debit_layout: None,
        standing: Some(StandingTail {
            order_id: (job.action != StandingOrderAction::Create)
                .then(|| job.order_id.trim().to_string()),
            schedule: schedule_components(job, with_time),
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::StandingOrderAction as A;

    fn job() -> StandingOrderJob {
        StandingOrderJob {
            action: A::Create,
            source_iban: "DE02120300000000202051".into(),
            source_owner: "A".into(),
            order_id: String::new(),
            recipient_name: "Vermieter".into(),
            recipient_iban: "DE89370400440532013000".into(),
            recipient_bic: None,
            amount_cents: 85_000,
            purpose: "Miete".into(),
            first_date: "2026-12-01".into(),
            last_date: None,
            time_unit: "M".into(),
            interval: 1,
            exec_day: 1,
        }
    }

    fn day(s: &str) -> jiff::civil::Date {
        jiff::civil::Date::strptime("%Y-%m-%d", s).unwrap()
    }

    #[test]
    fn lists_of_numbers_are_read_two_digits_at_a_time() {
        assert!(allowed_in("010203", 2, 3));
        assert!(!allowed_in("010203", 2, 12));
        // "01" "10" must not match 11 by reading across the boundary
        assert!(!allowed_in("0110", 2, 11));
        assert!(allowed_in("00", 2, 7));
        assert!(allowed_in("", 2, 7));
        assert!(allowed_in("1357", 1, 5));
        assert!(!allowed_in("1357", 1, 4));
        assert!(allowed_in("0", 1, 4));
    }

    #[test]
    fn schedule_is_checked_against_the_banks_rules() {
        let rules = StandingRules {
            min_pretime: 2,
            max_pretime: 90,
            turnus_months: "010306".into(),
            days_per_month: "00".into(),
            turnus_weeks: "00".into(),
            days_per_week: "0".into(),
        };
        let today = day("2026-11-25");
        assert!(check_schedule(&rules, &job(), today).is_ok());
        let mut j = job();
        j.first_date = "2026-11-26".into();
        assert!(check_schedule(&rules, &j, today).is_err(), "too soon");
        j.first_date = "2027-06-01".into();
        assert!(check_schedule(&rules, &j, today).is_err(), "too far");
        let mut j = job();
        j.interval = 2;
        assert!(check_schedule(&rules, &j, today).is_err(), "interval");
        let mut j = job();
        j.first_date = "kaputt".into();
        assert!(check_schedule(&rules, &j, today).is_err());
    }

    #[test]
    fn schedule_element_follows_the_dk_layout() {
        let mut j = job();
        assert_eq!(schedule_components(&j, false), ["20261201", "M", "1", "1"]);
        j.last_date = Some("2027-11-01".into());
        assert_eq!(
            schedule_components(&j, false),
            ["20261201", "M", "1", "1", "20271101"]
        );
        // HKCDL v2 has the time of day (empty) before the last date
        assert_eq!(
            schedule_components(&j, true),
            ["20261201", "M", "1", "1", "", "20271101"]
        );
    }

    #[test]
    fn tail_puts_the_order_id_only_on_change_and_delete() {
        let base = || SegmentBuilder::new("HKCDE", 3, 1).raw("K");
        let create = StandingTail {
            order_id: None,
            schedule: schedule_components(&job(), false),
        };
        let text = String::from_utf8(create.append(base()).build()).unwrap();
        assert_eq!(text, "HKCDE:3:1+K+20261201:M:1:1'");
        let change = StandingTail {
            order_id: Some("ID?+1".into()),
            schedule: schedule_components(&job(), false),
        };
        let text = String::from_utf8(change.append(base()).build()).unwrap();
        // "?" and "+" in the id are escaped
        assert_eq!(text, "HKCDE:3:1+K++ID???+1+20261201:M:1:1'");
    }
}
