//! Slint wiring of the "Wiederkehrend & Geplant" dialog (`ui/recurring_dialog.slint`): mark the
//! selected booking as recurring / salary, the recurring items of the month, planned payments
//! and standing orders. The pure texts live in `recurring_view.rs`. Every vault call runs on a
//! worker thread; results come back through `upgrade_in_event_loop`.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use girocrab_core::planned::{PlannedPayment, PlannedTotals};
use girocrab_core::recurring::{RecurringSeries, RecurringSnapshot, series_matches};
use girocrab_core::salary::SalaryCycle;
use girocrab_core::standing_orders::StandingOrderInfo;
use girocrab_core::{BookingFilter, StandingOrderAction, Vault};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::recurring_view::{self as view, Line};
use crate::{MainWindow, RecLine, german_date, money, privacy_on, reload, with_state};

type VaultSlot = Arc<Mutex<Option<Arc<Vault>>>>;

/// The selected booking as far as the dialog needs it.
#[derive(Debug, Clone, Default)]
struct Selected {
    id: i64,
    title: String,
    date: String,
    series_key: String,
    inflow: bool,
    salary: bool,
    salary_flag: Option<bool>,
    salary_signal: bool,
}

/// Everything one load of the dialog gathers.
struct Loaded {
    snapshot: RecurringSnapshot,
    planned: Vec<PlannedPayment>,
    totals: PlannedTotals,
    badge: u32,
    standing: Vec<StandingOrderInfo>,
    cycle: Option<SalaryCycle>,
    /// The series the selected booking belongs to.
    series: Option<RecurringSeries>,
}

#[derive(Default)]
struct Ui {
    snapshot: Option<RecurringSnapshot>,
    standing: Vec<StandingOrderInfo>,
}

thread_local! {
    static UI: RefCell<Ui> = RefCell::new(Ui::default());
}

fn with_ui<R>(f: impl FnOnce(&mut Ui) -> R) -> R {
    UI.with(|u| f(&mut u.borrow_mut()))
}

fn vault_of(slot: &VaultSlot) -> Option<Arc<Vault>> {
    slot.lock().unwrap().clone()
}

fn model(lines: Vec<Line>) -> ModelRc<RecLine> {
    ModelRc::new(VecModel::from(
        lines
            .into_iter()
            .map(|l| RecLine {
                id: l.id as i32,
                title: l.title.into(),
                detail: l.detail.into(),
                amount: l.amount.into(),
                negative: l.negative,
            })
            .collect::<Vec<_>>(),
    ))
}

fn selected() -> Option<Selected> {
    with_state(|st| {
        let id = st.selected_booking?;
        let r = st.rows.iter().find(|r| r.id == id)?;
        let title = match (r.counterparty.trim(), r.purpose.trim()) {
            ("", p) => p.to_string(),
            (c, "") => c.to_string(),
            (c, p) => format!("{c} – {p}"),
        };
        Some(Selected {
            id,
            title,
            date: r.booking_date.clone(),
            series_key: r.series_key.clone(),
            inflow: r.amount_cents > 0,
            salary: r.salary,
            salary_flag: r.salary_flag,
            salary_signal: r.salary_signal,
        })
    })
}

/// Closes the dialog and forgets its state when the vault locks.
pub fn on_lock(win: &MainWindow) {
    win.set_rc_open(false);
    with_ui(|u| *u = Ui::default());
}

fn eur(c: i64) -> String {
    money(c, "EUR")
}

/// Shows what a load gathered (UI thread).
fn render(win: &MainWindow, l: Loaded, sel: &Option<Selected>) {
    let o = &l.snapshot.month;
    win.set_rc_month_summary(view::month_summary(o, &eur).into());
    win.set_rc_month_rows(model(view::month_lines(o, &eur, &german_date)));
    win.set_rc_ignored_rows(model(view::ignored_lines(
        &l.snapshot.ignored,
        &eur,
        &german_date,
    )));
    win.set_rc_planned_summary(view::planned_summary(&l.totals, l.badge, &eur).into());
    win.set_rc_planned_rows(model(view::planned_lines_grouped(
        &l.planned,
        &eur,
        &german_date,
    )));
    win.set_rc_standing_rows(model(view::standing_lines(&l.standing, &eur, &german_date)));
    win.set_rc_salary_cycle(view::salary_cycle_text(l.cycle.as_ref(), &eur, &german_date).into());
    win.set_rc_has_series(l.series.is_some());
    match sel {
        Some(s) => {
            win.set_rc_has_booking(true);
            win.set_rc_booking_title(s.title.clone().into());
            win.set_rc_series_text(
                view::series_text(l.series.as_ref(), !s.series_key.is_empty(), &german_date).into(),
            );
            win.set_rc_salary_state(
                view::salary_state_text(s.salary, s.salary_flag, s.salary_signal, s.inflow).into(),
            );
            win.set_rc_can_salary(s.inflow);
            if let Some(series) = &l.series {
                win.set_rc_interval(view::index_of_interval(series.interval));
                win.set_rc_custom_dates(
                    series
                        .custom_dates
                        .iter()
                        .map(|d| view::custom_date_label(d))
                        .collect::<Vec<_>>()
                        .join(", ")
                        .into(),
                );
            }
        }
        None => {
            win.set_rc_has_booking(false);
            win.set_rc_booking_title(SharedString::new());
            win.set_rc_series_text(SharedString::new());
            win.set_rc_salary_state(SharedString::new());
            win.set_rc_can_salary(false);
        }
    }
    with_ui(|u| {
        u.snapshot = Some(l.snapshot);
        u.standing = l.standing;
    });
}

/// Gathers the dialog data (worker thread).
fn gather(v: &Vault, sel: &Option<Selected>, horizon: i32) -> Result<Loaded, String> {
    let e = |e: girocrab_core::CoreError| e.to_string();
    let (year, month) = crate::cockpit::current_year_month();
    let snapshot = v.recurring_snapshot(Vec::new(), year, month).map_err(e)?;
    let to = view::horizon_days(horizon).map(crate::cockpit::today_plus_days);
    let planned = v
        .list_planned_payments(None, to.clone(), Vec::new())
        .map_err(e)?;
    let totals = v.planned_payment_totals(None, to, Vec::new()).map_err(e)?;
    let badge = v.count_planned_payments().map_err(e)?;
    let standing = v.list_standing_orders(Vec::new()).map_err(e)?;
    let cycle = v.salary_cycle(Vec::new()).map_err(e)?;
    let series = match sel {
        Some(s) if !s.series_key.is_empty() => {
            // One day of bookings is enough to find the booking behind the selected row.
            let day = BookingFilter {
                date_from: Some(s.date.clone()),
                date_to: Some(s.date.clone()),
                ..Default::default()
            };
            let page = v.list_transactions(day).map_err(e)?;
            page.transactions
                .iter()
                .find(|b| b.id == s.id)
                .and_then(|b| {
                    snapshot
                        .series
                        .iter()
                        .find(|x| series_matches(x, b))
                        .cloned()
                })
        }
        _ => None,
    };
    Ok(Loaded {
        snapshot,
        planned,
        totals,
        badge,
        standing,
        cycle,
        series,
    })
}

fn load(win: &MainWindow, v: &Arc<Vault>, status: String) {
    let sel = selected();
    let horizon = win.get_rc_planned_horizon();
    win.set_rc_status("Wird geladen …".into());
    let (weak, v) = (win.as_weak(), v.clone());
    std::thread::spawn(move || {
        let result = gather(&v, &sel, horizon);
        let _ = weak.upgrade_in_event_loop(move |win| match result {
            Ok(l) => {
                render(&win, l, &sel);
                win.set_rc_status(status.into());
            }
            Err(e) => win.set_rc_status(e.into()),
        });
    });
}

/// Runs an edit on a worker thread, then refreshes the dialog, the sidebar and the booking list.
fn job<F>(win: &MainWindow, v: &Arc<Vault>, done: &str, f: F)
where
    F: FnOnce(&Vault) -> Result<(), String> + Send + 'static,
{
    win.set_rc_status("Wird gespeichert …".into());
    let (weak, v) = (win.as_weak(), v.clone());
    let done = done.to_string();
    std::thread::spawn(move || {
        let result = f(&v);
        let _ = weak.upgrade_in_event_loop(move |win| match result {
            Ok(()) => {
                crate::views_ui::invalidate_planned_count();
                crate::views_ui::refresh_sidebar(&win, &v);
                load(&win, &v, done);
                reload(&win, v);
            }
            Err(e) => win.set_rc_status(e.into()),
        });
    });
}

/// Edits are off in privacy mode (they would show amounts and names).
fn blocked(win: &MainWindow) -> bool {
    privacy_on(win)
}

/// The series key an edit of the selected booking refers to.
fn edit_key(sel: &Selected) -> Option<String> {
    (!sel.series_key.is_empty()).then(|| sel.series_key.clone())
}

pub fn wire(win: &MainWindow, slot: VaultSlot) {
    win.set_rc_interval_names(ModelRc::new(VecModel::from(
        view::INTERVAL_CHOICES
            .iter()
            .map(|s| SharedString::from(*s))
            .collect::<Vec<_>>(),
    )));

    let (weak, s) = (win.as_weak(), slot.clone());
    win.on_rc_request(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&s)) else {
            return;
        };
        if blocked(&win) {
            return;
        }
        win.set_rc_tab(if selected().is_some() { 0 } else { 1 });
        win.set_rc_open(true);
        load(&win, &v, String::new());
    });

    let (weak, s) = (win.as_weak(), slot.clone());
    win.on_rc_refresh(move || {
        if let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&s)) {
            load(&win, &v, String::new());
        }
    });
    let weak = win.as_weak();
    win.on_rc_tab_changed(move |_| {
        if let Some(win) = weak.upgrade() {
            win.set_rc_status(SharedString::new());
        }
    });

    let (weak, s) = (win.as_weak(), slot.clone());
    win.on_rc_mark(move |interval, dates| {
        let (Some(win), Some(v), Some(sel)) = (weak.upgrade(), vault_of(&s), selected()) else {
            return;
        };
        match view::mark_request(interval, dates.as_str()) {
            Ok((interval, dates)) => job(&win, &v, "Als wiederkehrend markiert.", move |v| {
                v.mark_booking_recurring(sel.id, interval, dates)
                    .map_err(|e| e.to_string())
            }),
            Err(msg) => win.set_rc_status(msg.into()),
        }
    });

    let (weak, s) = (win.as_weak(), slot.clone());
    win.on_rc_ignore(move || {
        let (Some(win), Some(v), Some(sel)) = (weak.upgrade(), vault_of(&s), selected()) else {
            return;
        };
        let Some(key) = edit_key(&sel) else { return };
        job(&win, &v, "Serie ausgeblendet.", move |v| {
            v.ignore_recurring(key).map_err(|e| e.to_string())
        });
    });

    let (weak, s) = (win.as_weak(), slot.clone());
    win.on_rc_reset(move || {
        let (Some(win), Some(v), Some(sel)) = (weak.upgrade(), vault_of(&s), selected()) else {
            return;
        };
        let Some(key) = edit_key(&sel) else { return };
        job(
            &win,
            &v,
            "Auf automatische Erkennung zurückgesetzt.",
            move |v| v.reset_recurring(key).map_err(|e| e.to_string()),
        );
    });

    let (weak, s) = (win.as_weak(), slot.clone());
    win.on_rc_salary(move |choice| {
        let (Some(win), Some(v), Some(sel)) = (weak.upgrade(), vault_of(&s), selected()) else {
            return;
        };
        let flag = view::salary_flag_for(choice);
        // The loaded row keeps showing the old state until the reload confirms the new one.
        with_state(|st| {
            if let Some(r) = st.rows.iter_mut().find(|r| r.id == sel.id) {
                r.salary_flag = flag;
                r.salary = flag.unwrap_or(r.salary_signal) && r.amount_cents > 0;
            }
        });
        job(&win, &v, "Gehalts-Einstellung gespeichert.", move |v| {
            v.set_booking_salary_flag(sel.id, flag)
                .map_err(|e| e.to_string())
        });
    });

    let (weak, s) = (win.as_weak(), slot.clone());
    win.on_rc_restore(move |i| {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&s)) else {
            return;
        };
        let key = with_ui(|u| {
            u.snapshot
                .as_ref()
                .and_then(|s| s.ignored.get(i.max(0) as usize))
                .map(|g| g.series_key.clone())
        });
        let Some(key) = key else { return };
        job(&win, &v, "Serie wird wieder angezeigt.", move |v| {
            v.reset_recurring(key).map_err(|e| e.to_string())
        });
    });

    let (weak, s) = (win.as_weak(), slot);
    win.on_rc_standing_select(move |i| {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&s)) else {
            return;
        };
        let Some(account) = with_ui(|u| u.standing.get(i.max(0) as usize).map(|o| o.account_id))
        else {
            return;
        };
        let weak = win.as_weak();
        std::thread::spawn(move || {
            let limits = v
                .standing_order_limits(account, StandingOrderAction::Create)
                .map_err(|e| e.to_string());
            let _ = weak.upgrade_in_event_loop(move |win| match limits {
                Ok(l) => win.set_rc_limits(view::limits_text(l.as_ref()).into()),
                Err(e) => win.set_rc_status(e.into()),
            });
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_need_a_groupable_booking() {
        let mut s = Selected::default();
        assert_eq!(edit_key(&s), None);
        s.series_key = "-iban:DE1".into();
        assert_eq!(edit_key(&s).as_deref(), Some("-iban:DE1"));
    }

    #[test]
    fn lines_become_model_rows() {
        let m = model(vec![Line {
            id: 3,
            title: "a".into(),
            detail: "b".into(),
            amount: "1,00 €".into(),
            negative: true,
        }]);
        use slint::Model;
        assert_eq!(m.row_count(), 1);
        assert_eq!(m.row_data(0).unwrap().id, 3);
    }
}
