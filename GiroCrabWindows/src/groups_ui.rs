//! Slint wiring for account groups and account settings (group tree, colours, order, merge,
//! account kind / owner / symbol / group, loan amount, opening balance, bank connection). The
//! dialog lives in `ui/groups_dialog.slint`, the pure logic in `groups.rs`. Every vault call runs
//! on a worker thread; results come back through `upgrade_in_event_loop`.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use girocrab_core::user_groups::account_color_palette;
use girocrab_core::{Account, UserGroup, Vault};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::groups;
use crate::manage;
use crate::planning::{self, euro};
use crate::{GroupAccountRow, GroupRow, GroupsState, MainWindow, hex_color, privacy_on};

type VaultSlot = Arc<Mutex<Option<Arc<Vault>>>>;

/// Dialog state on the UI thread.
#[derive(Default)]
struct Ui {
    /// The user's groups (without the "Ohne Gruppe" entry), as a flattened tree.
    groups: Vec<UserGroup>,
    accounts: Vec<Account>,
    palette: Vec<String>,
    /// Group indices offered as parent / merge target of the selected group.
    parent_cands: Vec<usize>,
    merge_cands: Vec<usize>,
    armed_group: Option<String>,
    armed_connection: Option<i64>,
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

fn color_of(hex: &str) -> slint::Color {
    hex_color(hex).unwrap_or_else(|| slint::Color::from_rgb_u8(0x8b, 0x8d, 0x98))
}

fn model<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(items))
}

fn names(items: impl IntoIterator<Item = String>) -> ModelRc<SharedString> {
    model(items.into_iter().map(SharedString::from).collect())
}

/// Closes the dialog and forgets its state when the vault locks.
pub fn on_lock(win: &MainWindow) {
    win.global::<GroupsState>().set_open(false);
    with_ui(|u| *u = Ui::default());
}

/// What a job leaves selected afterwards.
#[derive(Default, Clone)]
struct Sel {
    group: Option<String>,
    account: Option<i64>,
}

fn current_sel(win: &MainWindow) -> Sel {
    let gs = win.global::<GroupsState>();
    let group = usize::try_from(gs.get_sel_group())
        .ok()
        .and_then(|i| with_ui(|u| u.groups.get(i).map(|g| g.uuid.clone())));
    let account = usize::try_from(gs.get_sel_account())
        .ok()
        .and_then(|i| with_ui(|u| u.accounts.get(i).map(|a| a.id)));
    Sel { group, account }
}

fn selected_group(win: &MainWindow) -> Option<(usize, UserGroup)> {
    let i = usize::try_from(win.global::<GroupsState>().get_sel_group()).ok()?;
    with_ui(|u| u.groups.get(i).cloned()).map(|g| (i, g))
}

fn selected_account(win: &MainWindow) -> Option<Account> {
    let i = usize::try_from(win.global::<GroupsState>().get_sel_account()).ok()?;
    with_ui(|u| u.accounts.get(i).cloned())
}

// -------------------------------------------------------------------------------- rendering

fn render(win: &MainWindow, groups: Vec<UserGroup>, accounts: Vec<Account>, sel: &Sel) {
    let gs = win.global::<GroupsState>();
    let groups: Vec<UserGroup> = groups.into_iter().filter(|g| !g.is_ungrouped).collect();
    let g_rows: Vec<GroupRow> = groups
        .iter()
        .map(|g| GroupRow {
            name: groups::group_label(g).into(),
            detail: format!("{} · {}", g.accounts.len(), euro(g.balance_cents)).into(),
            color: color_of(&g.color_hex),
        })
        .collect();
    let a_rows: Vec<GroupAccountRow> = accounts
        .iter()
        .map(|a| GroupAccountRow {
            name: a.name.clone().into(),
            detail: if a.archived_unix.is_some() {
                "archiviert".into()
            } else {
                a.bank_name.clone().into()
            },
            color: color_of(&a.color_hex),
        })
        .collect();
    let group_names = std::iter::once("Ohne Gruppe".to_string())
        .chain(groups.iter().map(groups::group_label))
        .collect::<Vec<_>>();
    let g_index = sel
        .group
        .as_ref()
        .and_then(|u| groups.iter().position(|g| g.uuid == *u));
    let a_index = sel
        .account
        .and_then(|id| accounts.iter().position(|a| a.id == id));
    with_ui(|u| {
        u.groups = groups;
        u.accounts = accounts;
        u.palette = account_color_palette();
        u.armed_group = None;
        u.armed_connection = None;
    });
    gs.set_palette(model(
        with_ui(|u| u.palette.clone())
            .iter()
            .map(|c| color_of(c))
            .collect(),
    ));
    gs.set_groups(model(g_rows));
    gs.set_accounts(model(a_rows));
    gs.set_a_kind_names(names(groups::kind_labels().into_iter().map(String::from)));
    gs.set_a_group_names(names(group_names));
    gs.set_group_armed(false);
    gs.set_connection_armed(false);
    match g_index {
        Some(i) => fill_group_form(win, i),
        None => {
            gs.set_sel_group(-1);
            gs.set_g_name(SharedString::new());
            gs.set_g_symbol(SharedString::new());
            gs.set_g_color(-1);
            gs.set_g_total(true);
            gs.set_g_parent_names(names([]));
            gs.set_g_merge_names(names([]));
        }
    }
    match a_index {
        Some(i) => fill_account_form(win, i),
        None => gs.set_sel_account(-1),
    }
}

fn fill_group_form(win: &MainWindow, index: usize) {
    let gs = win.global::<GroupsState>();
    let Some(g) = with_ui(|u| u.groups.get(index).cloned()) else {
        return;
    };
    let (parent_names, parent_choice, merge_names) = with_ui(|u| {
        u.parent_cands = groups::parent_candidates(&u.groups, index);
        u.merge_cands = groups::merge_candidates(u.groups.len(), index);
        u.armed_group = None;
        let parent_names = std::iter::once("Oberste Ebene".to_string())
            .chain(u.parent_cands.iter().map(|i| u.groups[*i].name.clone()))
            .collect::<Vec<_>>();
        let choice = groups::parent_choice(&u.groups, index, &u.parent_cands);
        let merge = u
            .merge_cands
            .iter()
            .map(|i| u.groups[*i].name.clone())
            .collect::<Vec<_>>();
        (parent_names, choice, merge)
    });
    let color = with_ui(|u| manage::palette_index(&u.palette, &g.color_hex));
    gs.set_sel_group(index as i32);
    gs.set_g_name(g.name.into());
    gs.set_g_symbol(g.sf_symbol.into());
    gs.set_g_color(color.map_or(-1, |i| i as i32));
    gs.set_g_total(g.include_in_total);
    gs.set_g_parent_names(names(parent_names));
    gs.set_g_parent(parent_choice);
    gs.set_g_merge_names(names(merge_names));
    gs.set_g_merge(0);
    gs.set_group_armed(false);
}

fn fill_account_form(win: &MainWindow, index: usize) {
    let gs = win.global::<GroupsState>();
    let Some(a) = with_ui(|u| u.accounts.get(index).cloned()) else {
        return;
    };
    let group_choice = with_ui(|u| {
        u.armed_connection = None;
        a.user_group_uuid
            .as_ref()
            .and_then(|uuid| u.groups.iter().position(|g| g.uuid == *uuid))
            .map_or(0, |i| i as i32 + 1)
    });
    gs.set_sel_account(index as i32);
    gs.set_a_owner(a.owner.clone().into());
    gs.set_a_symbol(a.sf_symbol.clone().into());
    gs.set_a_kind(groups::kind_index(&a.kind));
    gs.set_a_group(group_choice);
    gs.set_a_initial(
        a.initial_balance_cents
            .map(planning::format_plain)
            .unwrap_or_default()
            .into(),
    );
    gs.set_a_open_date(a.opening_balance_date.clone().into());
    gs.set_a_open_amount(
        a.opening_balance_cents
            .map(planning::format_plain)
            .unwrap_or_default()
            .into(),
    );
    gs.set_a_target(SharedString::new());
    gs.set_a_preview(SharedString::new());
    gs.set_a_derived(a.balance_derived);
    gs.set_a_has_connection(a.connection_id.is_some());
    gs.set_connection_armed(false);
    gs.set_a_info(SharedString::new());
    load_account_info(win, a);
}

/// Connector key and 30-day balance history of the account, read on a worker thread.
fn load_account_info(win: &MainWindow, account: Account) {
    let Some(v) = vault_now() else {
        return;
    };
    let weak = win.as_weak();
    std::thread::spawn(move || {
        let mut lines = Vec::new();
        if let Some(c) = account.connection_id
            && let Ok(key) = v.connection_key(c)
        {
            lines.push(groups::connection_line(&key));
        }
        if let Ok(h) = v.balance_history(vec![account.id], 30) {
            lines.push(groups::history_summary(&h));
        }
        let id = account.id;
        let _ = weak.upgrade_in_event_loop(move |win| {
            // The selection may have moved on while the vault answered.
            if selected_account(&win).map(|a| a.id) == Some(id) {
                win.global::<GroupsState>()
                    .set_a_info(lines.join("\n").into());
            }
        });
    });
}

// -------------------------------------------------------------------------------- jobs

/// Loads groups and accounts on a worker thread and shows them.
fn load(win: &MainWindow, v: &Arc<Vault>, sel: Sel, status: String) {
    let (weak, v) = (win.as_weak(), v.clone());
    std::thread::spawn(move || {
        let data = v
            .list_user_groups()
            .and_then(|g| v.list_accounts().map(|a| (g, a)))
            .and_then(|(g, a)| v.list_account_groups().map(|t| (g, a, t)))
            .map_err(|e| e.to_string());
        let _ = weak.upgrade_in_event_loop(move |win| match data {
            Ok((g, a, t)) => {
                render(&win, g, a, &sel);
                win.global::<GroupsState>()
                    .set_type_summary(groups::type_summary(&t).into());
                win.global::<GroupsState>().set_status(status.into());
            }
            Err(e) => win.global::<GroupsState>().set_status(e.into()),
        });
    });
}

/// Runs a change on a worker thread, then refreshes the dialog and the main window's sidebar.
/// `account_side` changes also redraw the header (balance, kind) of the selected account.
fn job<F>(win: &MainWindow, v: &Arc<Vault>, done: &str, account_side: bool, f: F)
where
    F: FnOnce(&Vault) -> Result<Sel, String> + Send + 'static,
{
    win.global::<GroupsState>()
        .set_status("Wird gespeichert …".into());
    let (weak, v) = (win.as_weak(), v.clone());
    let done = done.to_string();
    let keep = current_sel(win);
    std::thread::spawn(move || {
        let result = f(&v);
        let _ = weak.upgrade_in_event_loop(move |win| match result {
            Ok(sel) => {
                let sel = Sel {
                    group: sel.group.or(keep.group),
                    account: sel.account.or(keep.account),
                };
                load(&win, &v, sel, done);
                if account_side {
                    let target = Some(win.get_selected() as i64).filter(|s| *s >= 0);
                    crate::show(&win, &v, target);
                } else {
                    crate::views_ui::refresh_sidebar(&win, &v);
                }
            }
            Err(e) => win.global::<GroupsState>().set_status(e.into()),
        });
    });
}

fn fail(win: &MainWindow, text: impl Into<SharedString>) {
    win.global::<GroupsState>().set_status(text.into());
}

fn amount_field(text: &str, what: &str) -> Result<Option<i64>, String> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(None);
    }
    planning::parse_cents(t)
        .map(Some)
        .ok_or_else(|| format!("{what}: Bitte einen Betrag wie 1.234,56 eingeben."))
}

fn chosen_color(index: i32) -> Option<String> {
    usize::try_from(index)
        .ok()
        .and_then(|i| with_ui(|u| u.palette.get(i).cloned()))
}

// -------------------------------------------------------------------------------- wiring

fn wire_groups(win: &MainWindow, slot: &VaultSlot) {
    let gs = win.global::<GroupsState>();

    let (weak, s) = (win.as_weak(), slot.clone());
    gs.on_request(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&s)) else {
            return;
        };
        if privacy_on(&win) {
            return;
        }
        let gs = win.global::<GroupsState>();
        gs.set_status(SharedString::new());
        gs.set_open(true);
        load(&win, &v, Sel::default(), String::new());
    });

    let weak = win.as_weak();
    gs.on_group_select(move |i| {
        if let (Some(win), Ok(i)) = (weak.upgrade(), usize::try_from(i)) {
            fill_group_form(&win, i);
        }
    });

    let weak = win.as_weak();
    gs.on_group_new(move || {
        let Some(win) = weak.upgrade() else { return };
        let gs = win.global::<GroupsState>();
        let name = match manage::validate_name("die Gruppe", &gs.get_g_name()) {
            Ok(n) => n,
            Err(e) => return fail(&win, e),
        };
        let symbol = match groups::clean_symbol(&gs.get_g_symbol()) {
            Ok(s) => s,
            Err(e) => return fail(&win, e),
        };
        let color = chosen_color(gs.get_g_color());
        let Some(v) = vault_now() else {
            return;
        };
        job(&win, &v, "Gruppe angelegt.", false, move |v| {
            let uuid = v
                .create_user_group(name, symbol)
                .map_err(|e| e.to_string())?;
            if color.is_some() {
                v.set_user_group_color(uuid.clone(), color)
                    .map_err(|e| e.to_string())?;
            }
            Ok(Sel {
                group: Some(uuid),
                account: None,
            })
        });
    });

    let weak = win.as_weak();
    gs.on_group_save(move || {
        let Some(win) = weak.upgrade() else { return };
        let Some((_, group)) = selected_group(&win) else {
            // Nothing selected: the form creates a group.
            return win.global::<GroupsState>().invoke_group_new();
        };
        let gs = win.global::<GroupsState>();
        let name = match manage::validate_name("die Gruppe", &gs.get_g_name()) {
            Ok(n) => n,
            Err(e) => return fail(&win, e),
        };
        let symbol = match groups::clean_symbol(&gs.get_g_symbol()) {
            Ok(s) => s,
            Err(e) => return fail(&win, e),
        };
        let total = gs.get_g_total();
        let color = chosen_color(gs.get_g_color());
        let parent_choice = gs.get_g_parent();
        let new_parent: Option<String> = usize::try_from(parent_choice - 1).ok().and_then(|p| {
            with_ui(|u| {
                u.parent_cands
                    .get(p)
                    .and_then(|i| u.groups.get(*i))
                    .map(|g| g.uuid.clone())
            })
        });
        let Some(v) = vault_now() else {
            return;
        };
        job(&win, &v, "Gruppe gespeichert.", false, move |v| {
            let uuid = group.uuid.clone();
            v.update_user_group(uuid.clone(), name, symbol, total)
                .map_err(|e| e.to_string())?;
            if color.is_some() && color.as_deref() != Some(group.color_hex.as_str()) {
                v.set_user_group_color(uuid.clone(), color)
                    .map_err(|e| e.to_string())?;
            }
            if new_parent != group.parent_uuid {
                v.set_user_group_parent(uuid.clone(), new_parent)
                    .map_err(|e| e.to_string())?;
            }
            Ok(Sel {
                group: Some(uuid),
                account: None,
            })
        });
    });

    let weak = win.as_weak();
    gs.on_group_delete(move || {
        let Some(win) = weak.upgrade() else { return };
        let Some((_, group)) = selected_group(&win) else {
            return;
        };
        let gs = win.global::<GroupsState>();
        let confirmed = with_ui(|u| {
            let confirmed = u.armed_group.as_deref() == Some(group.uuid.as_str());
            u.armed_group = (!confirmed).then(|| group.uuid.clone());
            confirmed
        });
        if !confirmed {
            gs.set_group_armed(true);
            return fail(
                &win,
                format!(
                    "Gruppe „{}“ löschen? Ihre Konten landen unter „Ohne Gruppe“. Zum Bestätigen noch einmal klicken.",
                    group.name
                ),
            );
        }
        let Some(v) = vault_now() else {
            return;
        };
        gs.set_sel_group(-1);
        job(&win, &v, "Gruppe gelöscht.", false, move |v| {
            v.delete_user_group(group.uuid)
                .map_err(|e| e.to_string())?;
            Ok(Sel::default())
        });
    });

    let weak = win.as_weak();
    gs.on_group_move(move |delta| {
        let Some(win) = weak.upgrade() else { return };
        let Some((i, group)) = selected_group(&win) else {
            return;
        };
        let Some(order) = with_ui(|u| groups::move_group(&u.groups, i, delta)) else {
            return;
        };
        let Some(v) = vault_now() else {
            return;
        };
        job(&win, &v, "Reihenfolge gespeichert.", false, move |v| {
            v.reorder_user_groups(order).map_err(|e| e.to_string())?;
            Ok(Sel {
                group: Some(group.uuid),
                account: None,
            })
        });
    });

    let weak = win.as_weak();
    gs.on_group_merge(move || {
        let Some(win) = weak.upgrade() else { return };
        let Some((_, from)) = selected_group(&win) else {
            return;
        };
        let target = usize::try_from(win.global::<GroupsState>().get_g_merge())
            .ok()
            .and_then(|m| {
                with_ui(|u| {
                    u.merge_cands
                        .get(m)
                        .and_then(|i| u.groups.get(*i))
                        .map(|g| g.uuid.clone())
                })
            });
        let Some(into) = target else { return };
        let Some(v) = vault_now() else {
            return;
        };
        job(&win, &v, "Gruppen zusammengeführt.", true, move |v| {
            v.merge_user_groups(from.uuid, into.clone())
                .map_err(|e| e.to_string())?;
            Ok(Sel {
                group: Some(into),
                account: None,
            })
        });
    });
}

fn wire_accounts(win: &MainWindow) {
    let gs = win.global::<GroupsState>();

    let weak = win.as_weak();
    gs.on_account_select(move |i| {
        if let (Some(win), Ok(i)) = (weak.upgrade(), usize::try_from(i)) {
            fill_account_form(&win, i);
        }
    });

    let weak = win.as_weak();
    gs.on_account_save(move || {
        let Some(win) = weak.upgrade() else { return };
        let Some(account) = selected_account(&win) else {
            return;
        };
        let gs = win.global::<GroupsState>();
        let owner = gs.get_a_owner().trim().to_string();
        let kind = groups::kind_at(gs.get_a_kind());
        let symbol = gs.get_a_symbol().trim().to_string();
        let group_uuid: Option<String> = usize::try_from(gs.get_a_group() - 1)
            .ok()
            .and_then(|i| with_ui(|u| u.groups.get(i).map(|g| g.uuid.clone())));
        let initial = match amount_field(&gs.get_a_initial(), "Darlehensbetrag") {
            Ok(v) => v,
            Err(e) => return fail(&win, e),
        };
        let Some(v) = vault_now() else {
            return;
        };
        job(&win, &v, "Konto gespeichert.", true, move |v| {
            let id = account.id;
            let e = |e: girocrab_core::CoreError| e.to_string();
            if owner != account.owner {
                v.set_account_owner(id, owner).map_err(e)?;
            }
            if let Some(k) = kind.filter(|k| *k != account.kind) {
                v.set_account_kind(id, k).map_err(e)?;
            }
            if !symbol.is_empty() && symbol != account.sf_symbol {
                v.set_account_symbol(id, symbol).map_err(e)?;
            }
            if group_uuid != account.user_group_uuid {
                v.move_account_to_group(id, group_uuid, None).map_err(e)?;
            }
            if initial != account.initial_balance_cents {
                v.set_account_initial_balance(id, initial).map_err(e)?;
            }
            Ok(Sel {
                group: None,
                account: Some(id),
            })
        });
    });

    let weak = win.as_weak();
    gs.on_account_move(move |delta| {
        let Some(win) = weak.upgrade() else { return };
        let Some(account) = selected_account(&win) else {
            return;
        };
        let ids: Vec<i64> = with_ui(|u| {
            u.accounts
                .iter()
                .filter(|a| a.user_group_uuid == account.user_group_uuid)
                .map(|a| a.id)
                .collect()
        });
        let Some(pos) = ids.iter().position(|i| *i == account.id) else {
            return;
        };
        let Some(order) = groups::move_id(&ids, pos, delta) else {
            return;
        };
        let Some(v) = vault_now() else {
            return;
        };
        job(&win, &v, "Reihenfolge gespeichert.", false, move |v| {
            v.reorder_accounts_in_group(account.user_group_uuid.clone(), order)
                .map_err(|e| e.to_string())?;
            Ok(Sel {
                group: None,
                account: Some(account.id),
            })
        });
    });

    let weak = win.as_weak();
    gs.on_account_reset_symbol(move || {
        let Some(win) = weak.upgrade() else { return };
        let Some(account) = selected_account(&win) else {
            return;
        };
        let Some(v) = vault_now() else {
            return;
        };
        job(&win, &v, "Symbol zurückgesetzt.", false, move |v| {
            v.reset_account_symbol(account.id)
                .map_err(|e| e.to_string())?;
            Ok(Sel {
                group: None,
                account: Some(account.id),
            })
        });
    });

    wire_opening_balance(win);

    let weak = win.as_weak();
    gs.on_connection_delete(move || {
        let Some(win) = weak.upgrade() else { return };
        let Some(account) = selected_account(&win) else {
            return;
        };
        let Some(connection) = account.connection_id else {
            return;
        };
        let confirmed = with_ui(|u| {
            let confirmed = u.armed_connection == Some(connection);
            u.armed_connection = (!confirmed).then_some(connection);
            confirmed
        });
        let gs = win.global::<GroupsState>();
        if !confirmed {
            gs.set_connection_armed(true);
            return fail(
                &win,
                "Den Bankzugang mit allen seinen Konten, Umsätzen und Belegen endgültig löschen? Zum Bestätigen noch einmal klicken.",
            );
        }
        let Some(v) = vault_now() else {
            return;
        };
        gs.set_sel_account(-1);
        // The selected account may be gone: the main window falls back to all accounts.
        win.set_selected(0);
        job(&win, &v, "Bankzugang gelöscht.", true, move |v| {
            v.delete_bank_connection(connection)
                .map_err(|e| e.to_string())?;
            Ok(Sel::default())
        });
    });
}

fn wire_opening_balance(win: &MainWindow) {
    let gs = win.global::<GroupsState>();

    let weak = win.as_weak();
    gs.on_opening_preview(move || {
        let Some(win) = weak.upgrade() else { return };
        let Some(account) = selected_account(&win) else {
            return;
        };
        let gs = win.global::<GroupsState>();
        let date = match groups::validate_date(&gs.get_a_open_date()) {
            Ok(d) => d,
            Err(e) => return fail(&win, e),
        };
        let opening = match amount_field(&gs.get_a_open_amount(), "Startsaldo") {
            Ok(v) => v.unwrap_or(0),
            Err(e) => return fail(&win, e),
        };
        let Some(v) = vault_now() else {
            return;
        };
        gs.set_status(SharedString::new());
        let weak = win.as_weak();
        std::thread::spawn(move || {
            let result = v
                .preview_account_opening_balance(account.id, date)
                .map(|p| groups::preview_text(&p, opening))
                .map_err(|e| e.to_string());
            let _ = weak.upgrade_in_event_loop(move |win| {
                let gs = win.global::<GroupsState>();
                match result {
                    Ok(text) => gs.set_a_preview(text.into()),
                    Err(e) => gs.set_status(e.into()),
                }
            });
        });
    });

    let weak = win.as_weak();
    gs.on_opening_save(move || {
        let Some(win) = weak.upgrade() else { return };
        let Some(account) = selected_account(&win) else {
            return;
        };
        let gs = win.global::<GroupsState>();
        let date = match groups::validate_date(&gs.get_a_open_date()) {
            Ok(d) => d,
            Err(e) => return fail(&win, e),
        };
        let cents = match amount_field(&gs.get_a_open_amount(), "Startsaldo") {
            Ok(Some(c)) => c,
            Ok(None) => return fail(&win, "Bitte den Startsaldo als Betrag eingeben."),
            Err(e) => return fail(&win, e),
        };
        let Some(v) = vault_now() else {
            return;
        };
        job(&win, &v, "Startsaldo gesetzt.", true, move |v| {
            v.set_account_opening_balance(account.id, Some(cents), date)
                .map_err(|e| e.to_string())?;
            Ok(Sel {
                group: None,
                account: Some(account.id),
            })
        });
    });

    let weak = win.as_weak();
    gs.on_opening_clear(move || {
        let Some(win) = weak.upgrade() else { return };
        let Some(account) = selected_account(&win) else {
            return;
        };
        let Some(v) = vault_now() else {
            return;
        };
        job(&win, &v, "Startsaldo entfernt.", true, move |v| {
            v.set_account_opening_balance(account.id, None, String::new())
                .map_err(|e| e.to_string())?;
            Ok(Sel {
                group: None,
                account: Some(account.id),
            })
        });
    });

    let weak = win.as_weak();
    gs.on_opening_calibrate(move || {
        let Some(win) = weak.upgrade() else { return };
        let Some(account) = selected_account(&win) else {
            return;
        };
        let target = match amount_field(&win.global::<GroupsState>().get_a_target(), "Zielsaldo") {
            Ok(Some(c)) => c,
            Ok(None) => return fail(&win, "Bitte den Zielsaldo als Betrag eingeben."),
            Err(e) => return fail(&win, e),
        };
        let Some(v) = vault_now() else {
            return;
        };
        job(&win, &v, "Startsaldo kalibriert.", true, move |v| {
            v.calibrate_account_opening_balance(account.id, target)
                .map_err(|e| e.to_string())?;
            Ok(Sel {
                group: None,
                account: Some(account.id),
            })
        });
    });
}

thread_local! {
    static SLOT: RefCell<Option<VaultSlot>> = const { RefCell::new(None) };
}

/// The current vault, `None` while locked.
fn vault_now() -> Option<Arc<Vault>> {
    SLOT.with(|s| s.borrow().as_ref().and_then(vault_of))
}

/// Connects the dialog to the vault.
pub fn wire(win: &MainWindow, slot: VaultSlot) {
    SLOT.with(|s| *s.borrow_mut() = Some(slot.clone()));
    wire_groups(win, &slot);
    wire_accounts(win);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_are_optional_but_must_parse() {
        assert_eq!(amount_field("", "x"), Ok(None));
        assert_eq!(amount_field(" 1.234,56 ", "x"), Ok(Some(123_456)));
        assert!(
            amount_field("abc", "Startsaldo")
                .unwrap_err()
                .starts_with("Startsaldo:")
        );
    }

    #[test]
    fn colour_fallback_is_grey() {
        assert_eq!(
            color_of("nope"),
            slint::Color::from_rgb_u8(0x8b, 0x8d, 0x98)
        );
        assert_eq!(color_of("#FF0000"), slint::Color::from_rgb_u8(255, 0, 0));
    }

    #[test]
    fn unknown_colour_choice_is_none() {
        assert_eq!(chosen_color(-1), None);
        assert_eq!(chosen_color(99), None);
    }
}
