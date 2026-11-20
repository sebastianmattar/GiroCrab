//! Slint wiring for tags, split bookings, category budgets and transfer templates. The dialogs
//! live in `ui/tags_dialog.slint`, the pure logic (validation, tag arithmetic, texts) in
//! `planning.rs`. Every vault call runs on a worker thread; results come back through
//! `upgrade_in_event_loop`.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use girocrab_core::tags::{BookingTag, Tag};
use girocrab_core::{Category, CategoryBudgetStatus, TransferTemplate, Vault};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::planning::{self, SplitDraft, TemplateForm};
use crate::{
    BudgetRow, MainWindow, SplitRow, TagRow, TemplateRow, hex_color, privacy_on, refresh_dashboard,
    reload, with_state,
};

type VaultSlot = Arc<Mutex<Option<Arc<Vault>>>>;

/// Tag colours offered in the dialog (`#RRGGBB`).
const PALETTE: [&str; 10] = [
    "#E5484D", "#F76B15", "#F5B800", "#30A46C", "#12A594", "#0090FF", "#3E63DD", "#8E4EC6",
    "#E93D82", "#8B8D98",
];

/// Dialog state on the UI thread.
#[derive(Default)]
struct Ui {
    tags: Vec<Tag>,
    /// Tag the delete button is armed for (second click deletes).
    armed_tag: Option<i64>,
    split_booking: Option<(i64, i64)>,
    split_categories: Vec<Category>,
    drafts: Vec<SplitDraft>,
    budget_categories: Vec<Category>,
    budget_status: Vec<CategoryBudgetStatus>,
    templates: Vec<TransferTemplate>,
    armed_template: Option<i64>,
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

fn palette() -> Vec<String> {
    PALETTE.iter().map(|c| (*c).to_string()).collect()
}

fn color_of(hex: &str) -> slint::Color {
    hex_color(hex).unwrap_or_else(|| slint::Color::from_rgb_u8(0x8b, 0x8d, 0x98))
}

fn model<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(items))
}

/// The selected booking: (id, "counterparty – purpose", amount in cents, its tags).
fn selected_booking() -> Option<(i64, String, i64, Vec<BookingTag>)> {
    with_state(|st| {
        let id = st.selected_booking?;
        let r = st.rows.iter().find(|r| r.id == id)?;
        let title = match (r.counterparty.trim(), r.purpose.trim()) {
            ("", p) => p.to_string(),
            (c, "") => c.to_string(),
            (c, p) => format!("{c} – {p}"),
        };
        Some((id, title, r.amount_cents, r.tags.clone()))
    })
}

/// Sets the tags of the selected booking in the loaded rows (before the reload confirms them).
fn set_row_tags(booking: i64, tags: Vec<BookingTag>) {
    with_state(|st| {
        if let Some(r) = st.rows.iter_mut().find(|r| r.id == booking) {
            r.tags = tags;
        }
    });
}

/// Inspector line "Tag, Tag" for the selected booking.
pub fn refresh_inspector_tags(win: &MainWindow) {
    let text = selected_booking()
        .map(|(_, _, _, tags)| planning::tag_summary(&tags))
        .unwrap_or_default();
    win.set_inspector_tags(text.into());
}

/// Closes the dialogs and forgets their state when the vault locks.
pub fn on_lock(win: &MainWindow) {
    win.set_tags_open(false);
    win.set_splits_open(false);
    win.set_budgets_open(false);
    win.set_templates_open(false);
    win.set_inspector_tags(SharedString::new());
    with_ui(|u| *u = Ui::default());
}

/// Edits are off in privacy mode (they would show amounts and names).
fn blocked(win: &MainWindow) -> bool {
    privacy_on(win)
}

// ------------------------------------------------------------------ tags

fn booking_has(tags: &[BookingTag], id: i64) -> bool {
    tags.iter().any(|t| t.id == id)
}

/// Shows `tags`; selects `select` (or keeps nothing selected).
fn render_tags(win: &MainWindow, tags: Vec<Tag>, select: Option<i64>) {
    let booking = selected_booking();
    let current = booking.as_ref().map(|b| b.3.clone()).unwrap_or_default();
    let rows: Vec<TagRow> = tags
        .iter()
        .map(|t| TagRow {
            id: t.id as i32,
            name: t.name.clone().into(),
            detail: planning::tag_detail(t.booking_count).into(),
            color: color_of(&t.color_hex),
            on: booking_has(&current, t.id),
        })
        .collect();
    let index = select.and_then(|id| tags.iter().position(|t| t.id == id));
    win.set_tg_has_booking(booking.is_some());
    win.set_tg_booking_title(booking.map(|b| b.1).unwrap_or_default().into());
    win.set_tg_rows(model(rows));
    with_ui(|u| {
        u.tags = tags;
        u.armed_tag = None;
    });
    win.set_tg_armed(false);
    match index {
        Some(i) => fill_tag_form(win, i),
        None => {
            win.set_tg_selected(-1);
            win.set_tg_name(SharedString::new());
            win.set_tg_color(-1);
            win.set_tg_merge_names(model(Vec::new()));
        }
    }
}

fn fill_tag_form(win: &MainWindow, index: usize) {
    let Some(tag) = with_ui(|u| u.tags.get(index).cloned()) else {
        return;
    };
    let names: Vec<SharedString> = with_ui(|u| {
        planning::merge_candidates(u.tags.len(), index)
            .into_iter()
            .map(|i| u.tags[i].name.clone().into())
            .collect()
    });
    win.set_tg_selected(index as i32);
    win.set_tg_name(tag.name.into());
    win.set_tg_color(planning::color_index(&palette(), &tag.color_hex));
    win.set_tg_merge_names(model(names));
    win.set_tg_merge(0);
    win.set_tg_armed(false);
    with_ui(|u| u.armed_tag = None);
}

fn selected_tag(win: &MainWindow) -> Option<Tag> {
    let i = usize::try_from(win.get_tg_selected()).ok()?;
    with_ui(|u| u.tags.get(i).cloned())
}

/// Loads the tags on a worker thread and shows them.
fn load_tags(win: &MainWindow, v: &Arc<Vault>, select: Option<i64>, status: String) {
    let (weak, v) = (win.as_weak(), v.clone());
    std::thread::spawn(move || {
        let tags = v.list_tags().map_err(|e| e.to_string());
        let _ = weak.upgrade_in_event_loop(move |win| match tags {
            Ok(tags) => {
                render_tags(&win, tags, select);
                win.set_tg_status(status.into());
            }
            Err(e) => win.set_tg_status(e.into()),
        });
    });
}

/// Runs a tag change on a worker thread, then refreshes the tag list and the booking list.
fn tag_job<F>(win: &MainWindow, v: &Arc<Vault>, select: Option<i64>, done: &str, job: F)
where
    F: FnOnce(&Vault) -> Result<(), String> + Send + 'static,
{
    win.set_tg_status("Wird gespeichert …".into());
    let (weak, v) = (win.as_weak(), v.clone());
    let done = done.to_string();
    std::thread::spawn(move || {
        let result = job(&v);
        let _ = weak.upgrade_in_event_loop(move |win| match result {
            Ok(()) => {
                load_tags(&win, &v, select, done);
                reload(&win, v);
            }
            Err(e) => win.set_tg_status(e.into()),
        });
    });
}

/// Changes the booking's tag set: updates the loaded row at once, then the vault.
fn booking_tag_change(win: &MainWindow, v: &Arc<Vault>, tag: Option<Tag>, add: Option<bool>) {
    let Some((booking, _, _, current)) = selected_booking() else {
        return;
    };
    let ids: Vec<i64> = current.iter().map(|t| t.id).collect();
    let new_ids = match (&tag, add) {
        (Some(t), Some(a)) => planning::with_tag(&ids, t.id, a),
        _ => Vec::new(),
    };
    let all = with_ui(|u| u.tags.clone());
    let new_tags: Vec<BookingTag> = new_ids
        .iter()
        .filter_map(|id| all.iter().find(|t| t.id == *id))
        .map(|t| BookingTag {
            id: t.id,
            name: t.name.clone(),
            color_hex: t.color_hex.clone(),
        })
        .collect();
    let select = tag.as_ref().map(|t| t.id);
    tag_job(win, v, select, "Tags der Buchung gespeichert.", move |v| {
        match (tag, add) {
            (Some(t), Some(true)) => v.add_tag_to_bookings(vec![booking], t.id).map(drop),
            (Some(t), Some(false)) => v.remove_tag_from_bookings(vec![booking], t.id).map(drop),
            _ => v.set_booking_tags(booking, Vec::new()),
        }
        .map_err(|e| e.to_string())
    });
    set_row_tags(booking, new_tags);
    refresh_inspector_tags(win);
}

fn wire_tags(win: &MainWindow, slot: &VaultSlot) {
    let (weak, s) = (win.as_weak(), slot.clone());
    win.on_tags_request(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&s)) else {
            return;
        };
        if blocked(&win) {
            return;
        }
        win.set_tg_palette(model(PALETTE.iter().map(|c| color_of(c)).collect()));
        win.set_tg_status(SharedString::new());
        win.set_tags_open(true);
        load_tags(&win, &v, None, String::new());
    });

    let weak = win.as_weak();
    win.on_tag_select(move |i| {
        if let Some(win) = weak.upgrade() {
            win.set_tg_status(SharedString::new());
            fill_tag_form(&win, i.max(0) as usize);
        }
    });

    let (weak, s) = (win.as_weak(), slot.clone());
    win.on_tag_create(move |name, color| {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&s)) else {
            return;
        };
        let name = match planning::validate_tag_name(&name) {
            Ok(n) => n,
            Err(e) => return win.set_tg_status(e.into()),
        };
        let hex = planning::tag_color(&palette(), color);
        win.set_tg_status("Wird gespeichert …".into());
        let weak = win.as_weak();
        std::thread::spawn(move || {
            let created = v.create_tag(name, hex).map_err(|e| e.to_string());
            let _ = weak.upgrade_in_event_loop(move |win| match created {
                Ok(t) => load_tags(&win, &v, Some(t.id), "Tag angelegt.".into()),
                Err(e) => win.set_tg_status(e.into()),
            });
        });
    });

    let (weak, s) = (win.as_weak(), slot.clone());
    win.on_tag_save(move |name, color| {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&s)) else {
            return;
        };
        let Some(tag) = selected_tag(&win) else {
            return;
        };
        let name = match planning::validate_tag_name(&name) {
            Ok(n) => n,
            Err(e) => return win.set_tg_status(e.into()),
        };
        let hex = planning::tag_color(&palette(), color);
        let id = tag.id;
        tag_job(&win, &v, Some(id), "Tag gespeichert.", move |v| {
            if name != tag.name {
                v.rename_tag(id, name).map_err(|e| e.to_string())?;
            }
            if hex != tag.color_hex {
                v.set_tag_color(id, hex).map_err(|e| e.to_string())?;
            }
            Ok(())
        });
    });

    let (weak, s) = (win.as_weak(), slot.clone());
    win.on_tag_delete(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&s)) else {
            return;
        };
        let Some(tag) = selected_tag(&win) else {
            return;
        };
        if with_ui(|u| u.armed_tag) != Some(tag.id) {
            with_ui(|u| u.armed_tag = Some(tag.id));
            win.set_tg_armed(true);
            win.set_tg_status(
                format!(
                    "„{}“ wird von {} entfernt. Zum Bestätigen erneut klicken.",
                    tag.name,
                    planning::tag_detail(tag.booking_count)
                )
                .into(),
            );
            return;
        }
        tag_job(&win, &v, None, "Tag gelöscht.", move |v| {
            v.delete_tag(tag.id).map_err(|e| e.to_string())
        });
    });

    let (weak, s) = (win.as_weak(), slot.clone());
    win.on_tag_move(move |step| {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&s)) else {
            return;
        };
        let Some(tag) = selected_tag(&win) else {
            return;
        };
        let ids: Vec<i64> = with_ui(|u| u.tags.iter().map(|t| t.id).collect());
        let Some(order) = planning::moved_order(&ids, win.get_tg_selected().max(0) as usize, step)
        else {
            return;
        };
        tag_job(&win, &v, Some(tag.id), "", move |v| {
            v.reorder_tags(order).map_err(|e| e.to_string())
        });
    });

    let (weak, s) = (win.as_weak(), slot.clone());
    win.on_tag_merge(move |pick| {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&s)) else {
            return;
        };
        let Some(source) = selected_tag(&win) else {
            return;
        };
        let target = with_ui(|u| {
            planning::merge_candidates(u.tags.len(), win.get_tg_selected().max(0) as usize)
                .get(pick.max(0) as usize)
                .and_then(|i| u.tags.get(*i).cloned())
        });
        let Some(target) = target else {
            return;
        };
        let msg = format!("„{}“ in „{}“ zusammengeführt.", source.name, target.name);
        tag_job(&win, &v, Some(target.id), &msg, move |v| {
            v.merge_tag_into(source.id, target.id)
                .map_err(|e| e.to_string())
        });
    });

    let (weak, s) = (win.as_weak(), slot.clone());
    win.on_tag_assign(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&s)) else {
            return;
        };
        if let Some(tag) = selected_tag(&win) {
            booking_tag_change(&win, &v, Some(tag), Some(true));
        }
    });
    let (weak, s) = (win.as_weak(), slot.clone());
    win.on_tag_unassign(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&s)) else {
            return;
        };
        if let Some(tag) = selected_tag(&win) {
            booking_tag_change(&win, &v, Some(tag), Some(false));
        }
    });

    let (weak, s) = (win.as_weak(), slot.clone());
    win.on_tag_clear_booking(move || {
        if let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&s)) {
            booking_tag_change(&win, &v, None, None);
        }
    });
}

// ---------------------------------------------------------------- splits

fn category_names(cats: &[Category]) -> Vec<SharedString> {
    std::iter::once("(keine Kategorie)".into())
        .chain(cats.iter().map(|c| c.path.clone().into()))
        .collect()
}

fn render_splits(win: &MainWindow) {
    let (rows, remaining) = with_ui(|u| {
        let cents = u.split_booking.map_or(0, |b| b.1);
        let rows: Vec<SplitRow> = u
            .drafts
            .iter()
            .map(|d| SplitRow {
                category: d.category_index,
                amount: d.amount.clone().into(),
                note: d.note.clone().into(),
            })
            .collect();
        (rows, planning::split_remaining(cents, &u.drafts))
    });
    win.set_sp_rows(model(rows));
    win.set_sp_remaining(planning::euro(remaining).into());
}

fn update_remaining(win: &MainWindow) {
    let remaining =
        with_ui(|u| planning::split_remaining(u.split_booking.map_or(0, |b| b.1), &u.drafts));
    win.set_sp_remaining(planning::euro(remaining).into());
}

fn edit_draft(win: &MainWindow, i: i32, f: impl FnOnce(&mut SplitDraft)) {
    let changed = with_ui(
        |u| match usize::try_from(i).ok().and_then(|i| u.drafts.get_mut(i)) {
            Some(d) => {
                f(d);
                true
            }
            None => false,
        },
    );
    if changed {
        update_remaining(win);
    }
}

/// Saves `splits` for the open booking on a worker thread, then closes the dialog.
fn split_job(win: &MainWindow, v: &Arc<Vault>, splits: Vec<girocrab_core::TransactionSplitInput>) {
    let Some((booking, _)) = with_ui(|u| u.split_booking) else {
        return;
    };
    win.set_sp_status("Wird gespeichert …".into());
    let (weak, v) = (win.as_weak(), v.clone());
    std::thread::spawn(move || {
        let saved = v
            .set_transaction_splits(booking, splits)
            .map_err(|e| e.to_string());
        let _ = weak.upgrade_in_event_loop(move |win| match saved {
            Ok(()) => {
                win.set_splits_open(false);
                reload(&win, v.clone());
                refresh_dashboard(&win, v);
            }
            Err(e) => win.set_sp_status(e.into()),
        });
    });
}

fn wire_splits(win: &MainWindow, slot: &VaultSlot) {
    let (weak, s) = (win.as_weak(), slot.clone());
    win.on_splits_request(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&s)) else {
            return;
        };
        let Some((booking, title, cents, _)) = selected_booking() else {
            return;
        };
        if blocked(&win) {
            return;
        }
        win.set_sp_status(SharedString::new());
        let weak = win.as_weak();
        std::thread::spawn(move || {
            let splits = v
                .list_transaction_splits(booking)
                .map_err(|e| e.to_string());
            let cats = v.list_categories(None, None).map_err(|e| e.to_string());
            let _ = weak.upgrade_in_event_loop(move |win| match (splits, cats) {
                (Ok(splits), Ok(cats)) => {
                    let mut drafts: Vec<SplitDraft> = splits
                        .iter()
                        .map(|s| SplitDraft {
                            category_index: s
                                .category_id
                                .and_then(|id| cats.iter().position(|c| c.id == id))
                                .map_or(0, |p| p as i32 + 1),
                            amount: planning::format_plain(s.amount_cents),
                            note: s.note.clone(),
                        })
                        .collect();
                    while drafts.len() < 2 {
                        drafts.push(SplitDraft::default());
                    }
                    win.set_sp_title(title.into());
                    win.set_sp_amount(planning::euro(cents).into());
                    win.set_sp_category_names(model(category_names(&cats)));
                    with_ui(|u| {
                        u.split_booking = Some((booking, cents));
                        u.split_categories = cats;
                        u.drafts = drafts;
                    });
                    render_splits(&win);
                    win.set_splits_open(true);
                }
                (Err(e), _) | (_, Err(e)) => win.set_sp_status(e.into()),
            });
        });
    });

    let weak = win.as_weak();
    win.on_split_amount(move |i, t| {
        if let Some(win) = weak.upgrade() {
            edit_draft(&win, i, |d| d.amount = t.to_string());
        }
    });
    let weak = win.as_weak();
    win.on_split_note(move |i, t| {
        if let Some(win) = weak.upgrade() {
            edit_draft(&win, i, |d| d.note = t.to_string());
        }
    });
    let weak = win.as_weak();
    win.on_split_category(move |i, c| {
        if let Some(win) = weak.upgrade() {
            edit_draft(&win, i, |d| d.category_index = c);
        }
    });

    let weak = win.as_weak();
    win.on_split_add(move || {
        if let Some(win) = weak.upgrade() {
            with_ui(|u| u.drafts.push(SplitDraft::default()));
            render_splits(&win);
        }
    });
    let weak = win.as_weak();
    win.on_split_remove_row(move |i| {
        if let Some(win) = weak.upgrade() {
            with_ui(|u| {
                if u.drafts.len() > 2 && (i as usize) < u.drafts.len() {
                    u.drafts.remove(i as usize);
                }
            });
            render_splits(&win);
        }
    });
    let weak = win.as_weak();
    win.on_split_fill(move |i| {
        if let Some(win) = weak.upgrade() {
            with_ui(|u| {
                let cents = u.split_booking.map_or(0, |b| b.1);
                let rest = planning::split_remaining(cents, &u.drafts);
                if let Some(d) = u.drafts.get_mut(i.max(0) as usize) {
                    let own = planning::parse_cents(&d.amount).unwrap_or(0);
                    d.amount = planning::format_plain(own + rest);
                }
            });
            render_splits(&win);
        }
    });

    let (weak, s) = (win.as_weak(), slot.clone());
    win.on_split_save(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&s)) else {
            return;
        };
        let built = with_ui(|u| {
            planning::build_splits(
                &u.drafts,
                &u.split_categories,
                u.split_booking.map_or(0, |b| b.1),
            )
        });
        match built {
            Ok(splits) => split_job(&win, &v, splits),
            Err(e) => win.set_sp_status(e.into()),
        }
    });
    let (weak, s) = (win.as_weak(), slot.clone());
    win.on_split_clear(move || {
        if let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&s)) {
            split_job(&win, &v, Vec::new());
        }
    });
}

// --------------------------------------------------------------- budgets

fn render_budgets(
    win: &MainWindow,
    cats: Vec<Category>,
    status: Vec<CategoryBudgetStatus>,
    year: i32,
    month: u32,
) {
    let rows: Vec<BudgetRow> = status
        .iter()
        .map(|s| BudgetRow {
            index: cats
                .iter()
                .position(|c| c.id == s.category_id)
                .map_or(-1, |p| p as i32),
            name: s.path.clone().into(),
            detail: planning::budget_detail(s.spent_cents, s.budget_cents).into(),
            progress: planning::budget_progress(s.spent_cents, s.budget_cents),
            over: s.remaining_cents < 0,
            color: color_of(&s.color_hex),
        })
        .collect();
    win.set_bg_month(planning::month_title(year, month).into());
    win.set_bg_category_names(model(
        cats.iter()
            .map(|c| SharedString::from(c.path.as_str()))
            .collect(),
    ));
    win.set_bg_rows(model(rows));
    with_ui(|u| {
        u.budget_categories = cats;
        u.budget_status = status;
    });
}

fn load_budgets(win: &MainWindow, v: &Arc<Vault>, status: String) {
    let (year, month) = crate::cockpit::current_year_month();
    let (weak, v) = (win.as_weak(), v.clone());
    std::thread::spawn(move || {
        let cats = v.list_categories(None, None).map_err(|e| e.to_string());
        let progress = v
            .list_category_budget_progress(year, month)
            .map_err(|e| e.to_string());
        let _ = weak.upgrade_in_event_loop(move |win| match (cats, progress) {
            (Ok(cats), Ok(progress)) => {
                render_budgets(&win, cats, progress, year, month);
                win.set_bg_status(status.into());
            }
            (Err(e), _) | (_, Err(e)) => win.set_bg_status(e.into()),
        });
    });
}

fn budget_job(win: &MainWindow, v: &Arc<Vault>, category: i32, cents: Option<i64>, done: &str) {
    let Some(id) = with_ui(|u| {
        usize::try_from(category)
            .ok()
            .and_then(|i| u.budget_categories.get(i).map(|c| c.id))
    }) else {
        win.set_bg_status("Bitte eine Kategorie wählen.".into());
        return;
    };
    win.set_bg_status("Wird gespeichert …".into());
    let (weak, v, done) = (win.as_weak(), v.clone(), done.to_string());
    std::thread::spawn(move || {
        let saved = v.set_category_budget(id, cents).map_err(|e| e.to_string());
        let _ = weak.upgrade_in_event_loop(move |win| match saved {
            Ok(()) => {
                load_budgets(&win, &v, done);
                refresh_dashboard(&win, v);
            }
            Err(e) => win.set_bg_status(e.into()),
        });
    });
}

fn wire_budgets(win: &MainWindow, slot: &VaultSlot) {
    let (weak, s) = (win.as_weak(), slot.clone());
    win.on_budgets_request(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&s)) else {
            return;
        };
        if blocked(&win) {
            return;
        }
        win.set_bg_status(SharedString::new());
        win.set_budgets_open(true);
        load_budgets(&win, &v, String::new());
    });

    let weak = win.as_weak();
    win.on_budget_pick(move |i| {
        let Some(win) = weak.upgrade() else { return };
        let picked = with_ui(|u| {
            let s = u.budget_status.get(i.max(0) as usize)?;
            let cat = u
                .budget_categories
                .iter()
                .position(|c| c.id == s.category_id)?;
            Some((cat as i32, planning::format_plain(s.budget_cents)))
        });
        if let Some((cat, amount)) = picked {
            win.set_bg_category(cat);
            win.set_bg_amount(amount.into());
            win.set_bg_status(SharedString::new());
        }
    });

    let (weak, s) = (win.as_weak(), slot.clone());
    win.on_budget_set(move |category, amount| {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&s)) else {
            return;
        };
        match planning::parse_budget(&amount) {
            Ok(Some(cents)) => budget_job(&win, &v, category, Some(cents), "Budget gespeichert."),
            Ok(None) => win.set_bg_status(
                "Bitte einen Betrag eingeben (zum Entfernen „Budget entfernen“ wählen).".into(),
            ),
            Err(e) => win.set_bg_status(e.into()),
        }
    });
    let (weak, s) = (win.as_weak(), slot.clone());
    win.on_budget_clear(move |category| {
        if let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&s)) {
            budget_job(&win, &v, category, None, "Budget entfernt.");
        }
    });
}

// ------------------------------------------------------------- templates

fn render_templates(win: &MainWindow, templates: Vec<TransferTemplate>, select: Option<i64>) {
    let rows: Vec<TemplateRow> = templates
        .iter()
        .map(|t| TemplateRow {
            title: t.name.clone().into(),
            detail: planning::template_detail(t).into(),
        })
        .collect();
    let index = select.and_then(|id| templates.iter().position(|t| t.id == id));
    win.set_tp_rows(model(rows));
    with_ui(|u| {
        u.templates = templates;
        u.armed_template = None;
    });
    win.set_tp_armed(false);
    match index {
        Some(i) => fill_template_form(win, i),
        None => clear_template_form(win),
    }
}

fn clear_template_form(win: &MainWindow) {
    win.set_tp_selected(-1);
    for set in [
        MainWindow::set_tp_name,
        MainWindow::set_tp_recipient,
        MainWindow::set_tp_iban,
        MainWindow::set_tp_bic,
        MainWindow::set_tp_amount,
        MainWindow::set_tp_purpose,
    ] {
        set(win, SharedString::new());
    }
    win.set_tp_armed(false);
    with_ui(|u| u.armed_template = None);
}

fn fill_template_form(win: &MainWindow, index: usize) {
    let Some(t) = with_ui(|u| u.templates.get(index).cloned()) else {
        return;
    };
    win.set_tp_selected(index as i32);
    win.set_tp_name(t.name.into());
    win.set_tp_recipient(t.recipient_name.into());
    win.set_tp_iban(t.recipient_iban.into());
    win.set_tp_bic(t.recipient_bic.into());
    win.set_tp_amount(
        t.default_amount_cents
            .map(planning::format_plain)
            .unwrap_or_default()
            .into(),
    );
    win.set_tp_purpose(t.default_purpose.into());
    win.set_tp_armed(false);
    with_ui(|u| u.armed_template = None);
}

fn load_templates(win: &MainWindow, v: &Arc<Vault>, select: Option<i64>, status: String) {
    let (weak, v) = (win.as_weak(), v.clone());
    std::thread::spawn(move || {
        let list = v.list_transfer_templates().map_err(|e| e.to_string());
        let _ = weak.upgrade_in_event_loop(move |win| match list {
            Ok(list) => {
                render_templates(&win, list, select);
                win.set_tp_status(status.into());
            }
            Err(e) => win.set_tp_status(e.into()),
        });
    });
}

fn wire_templates(win: &MainWindow, slot: &VaultSlot) {
    let (weak, s) = (win.as_weak(), slot.clone());
    win.on_templates_request(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&s)) else {
            return;
        };
        if blocked(&win) {
            return;
        }
        win.set_tp_status(SharedString::new());
        win.set_templates_open(true);
        load_templates(&win, &v, None, String::new());
    });

    let weak = win.as_weak();
    win.on_template_select(move |i| {
        if let Some(win) = weak.upgrade() {
            win.set_tp_status(SharedString::new());
            fill_template_form(&win, i.max(0) as usize);
        }
    });
    let weak = win.as_weak();
    win.on_template_new(move || {
        if let Some(win) = weak.upgrade() {
            win.set_tp_status(SharedString::new());
            clear_template_form(&win);
        }
    });

    let (weak, s) = (win.as_weak(), slot.clone());
    win.on_template_save(move |name, recipient, iban, bic, amount, purpose| {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&s)) else {
            return;
        };
        let id = with_ui(|u| {
            usize::try_from(win.get_tp_selected())
                .ok()
                .and_then(|i| u.templates.get(i).map(|t| t.id))
                .unwrap_or(0)
        });
        let form = TemplateForm {
            name: name.to_string(),
            recipient: recipient.to_string(),
            iban: iban.to_string(),
            bic: bic.to_string(),
            amount: amount.to_string(),
            purpose: purpose.to_string(),
        };
        let template = match planning::build_template(&form, id) {
            Ok(t) => t,
            Err(e) => return win.set_tp_status(e.into()),
        };
        win.set_tp_status("Wird gespeichert …".into());
        let weak = win.as_weak();
        std::thread::spawn(move || {
            let saved = v
                .save_transfer_template(template)
                .map_err(|e| e.to_string());
            let _ = weak.upgrade_in_event_loop(move |win| match saved {
                Ok(id) => load_templates(&win, &v, Some(id), "Vorlage gespeichert.".into()),
                Err(e) => win.set_tp_status(e.into()),
            });
        });
    });

    let (weak, s) = (win.as_weak(), slot.clone());
    win.on_template_delete(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&s)) else {
            return;
        };
        let Some(t) = with_ui(|u| {
            usize::try_from(win.get_tp_selected())
                .ok()
                .and_then(|i| u.templates.get(i).cloned())
        }) else {
            return;
        };
        if with_ui(|u| u.armed_template) != Some(t.id) {
            with_ui(|u| u.armed_template = Some(t.id));
            win.set_tp_armed(true);
            win.set_tp_status(
                format!("„{}“ löschen? Zum Bestätigen erneut klicken.", t.name).into(),
            );
            return;
        }
        win.set_tp_status("Wird gelöscht …".into());
        let weak = win.as_weak();
        std::thread::spawn(move || {
            let deleted = v.delete_transfer_template(t.id).map_err(|e| e.to_string());
            let _ = weak.upgrade_in_event_loop(move |win| match deleted {
                Ok(()) => load_templates(&win, &v, None, "Vorlage gelöscht.".into()),
                Err(e) => win.set_tp_status(e.into()),
            });
        });
    });
}

/// Connects the four dialogs to the vault.
pub fn wire(win: &MainWindow, slot: VaultSlot) {
    wire_tags(win, &slot);
    wire_splits(win, &slot);
    wire_budgets(win, &slot);
    wire_templates(win, &slot);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_colours_are_valid_hex() {
        for c in PALETTE {
            assert!(hex_color(c).is_some(), "{c}");
        }
        assert_eq!(palette().len(), PALETTE.len());
    }

    #[test]
    fn unknown_colour_falls_back_to_grey() {
        assert_eq!(color_of(""), slint::Color::from_rgb_u8(0x8b, 0x8d, 0x98));
    }

    #[test]
    fn booking_membership() {
        let t = |id| BookingTag {
            id,
            name: String::new(),
            color_hex: String::new(),
        };
        assert!(booking_has(&[t(1), t(2)], 2));
        assert!(!booking_has(&[t(1)], 5));
    }
}
