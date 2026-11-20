//! Slint wiring for account / category management, About and the keyboard shortcuts.
//! The decisions live in `manage.rs` (unit-tested); this file only moves values between the
//! window and the vault. The core calls are single SQL statements, so they run on the UI thread.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use girocrab_core::{Category, Vault};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::manage::{self, Command, Context, DeleteGate, Modal, Modals};
use crate::{CategoryRow, MainWindow, VERSION, hex_color, show};

type VaultSlot = Arc<Mutex<Option<Arc<Vault>>>>;

#[derive(Default)]
struct Ui {
    palette: Vec<String>,
    categories: Vec<Category>,
    account_gate: DeleteGate,
    category_gate: DeleteGate,
}

thread_local! {
    static UI: RefCell<Ui> = RefCell::new(Ui::default());
}

fn with_ui<R>(f: impl FnOnce(&mut Ui) -> R) -> R {
    UI.with(|u| f(&mut u.borrow_mut()))
}

/// Gives the shortcut scope the keyboard focus again (a closed dialog or the unlock button took
/// it with them). Deferred one tick: the element that held the focus is destroyed after the call.
pub fn refocus_later(win: &MainWindow) {
    let weak = win.as_weak();
    slint::Timer::single_shot(Duration::from_millis(50), move || {
        if let Some(win) = weak.upgrade() {
            win.invoke_refocus();
        }
    });
}

fn vault_of(slot: &VaultSlot) -> Option<Arc<Vault>> {
    slot.lock().unwrap().clone()
}

fn selected_account(win: &MainWindow) -> Option<i64> {
    Some(win.get_selected() as i64).filter(|s| *s > 0)
}

fn reload_all(win: &MainWindow, v: &Arc<Vault>) {
    let sel = Some(win.get_selected() as i64).filter(|s| *s >= 0);
    show(win, v, sel);
}

fn account_by_id(v: &Vault, id: i64) -> Option<girocrab_core::Account> {
    v.list_accounts().ok()?.into_iter().find(|a| a.id == id)
}

fn modals(win: &MainWindow) -> Modals {
    Modals {
        tan: win.get_tan_open(),
        transfer: win.get_transfer_open(),
        add_bank: win.get_add_open(),
        account: win.get_acct_open(),
        categories: win.get_cat_open(),
        rules: win.get_rules_open(),
        link: win.get_link_open(),
        about: win.get_about_open(),
    }
}

fn close_modal(win: &MainWindow, m: Modal) {
    match m {
        Modal::Tan => {
            win.set_tan_text(SharedString::new());
            win.invoke_tan_cancel();
        }
        Modal::Transfer => win.set_transfer_open(false),
        Modal::AddBank => win.set_add_open(false),
        Modal::Account => {
            with_ui(|u| u.account_gate.reset());
            win.set_acct_open(false);
        }
        Modal::Categories => {
            with_ui(|u| u.category_gate.reset());
            win.set_cat_open(false);
        }
        Modal::Rules => crate::views_ui::close_rules(win),
        Modal::Link => win.set_link_open(false),
        Modal::About => win.set_about_open(false),
    }
}

fn run_command(win: &MainWindow, cmd: Command) {
    match cmd {
        Command::FocusSearch => {
            win.set_search_focus_tick(win.get_search_focus_tick().wrapping_add(1))
        }
        Command::Lock => win.invoke_lock(),
        Command::Refresh => win.invoke_sync(),
        Command::Import => win.invoke_import_file(),
        Command::NewTransfer => win.invoke_transfer_start(),
        Command::CloseModal(m) => {
            close_modal(win, m);
            win.invoke_refocus();
        }
    }
}

fn set_account_dialog(win: &MainWindow, archived: bool) {
    win.set_acct_archived(archived);
    win.set_acct_armed(false);
}

fn render_categories(win: &MainWindow, cats: Vec<Category>, select: Option<i64>) {
    let rows: Vec<CategoryRow> = cats
        .iter()
        .map(|c| CategoryRow {
            id: c.id as i32,
            name: c.name.clone().into(),
            depth: c.depth as i32,
            color: hex_color(&c.color_hex).unwrap_or(slint::Color::from_rgb_u8(142, 142, 147)),
        })
        .collect();
    let index = select.and_then(|id| cats.iter().position(|c| c.id == id));
    win.set_cat_rows(ModelRc::new(VecModel::from(rows)));
    win.set_cat_selected(index.map_or(-1, |i| i as i32));
    let palette = with_ui(|u| u.palette.clone());
    match index.map(|i| &cats[i]) {
        Some(c) => {
            win.set_cat_name(c.name.clone().into());
            win.set_cat_color(
                manage::palette_index(&palette, &c.color_hex).map_or(-1, |i| i as i32),
            );
        }
        None => {
            win.set_cat_name(SharedString::new());
            win.set_cat_color(-1);
        }
    }
    win.set_cat_armed(false);
    with_ui(|u| {
        u.categories = cats;
        u.category_gate.reset();
    });
}

fn refresh_categories(win: &MainWindow, v: &Arc<Vault>, select: Option<i64>) {
    match v.list_categories(None, None) {
        Ok(cats) => render_categories(win, cats, select),
        Err(e) => win.set_cat_status(e.to_string().into()),
    }
}

pub fn wire(win: &MainWindow, vault: VaultSlot) {
    let palette = girocrab_core::user_groups::account_color_palette();
    let colors: Vec<slint::Color> = palette.iter().filter_map(|c| hex_color(c)).collect();
    win.set_palette(ModelRc::new(VecModel::from(colors)));
    win.set_app_version(VERSION.into());
    with_ui(|u| u.palette = palette);

    wire_accounts(win, &vault);
    wire_categories(win, &vault);

    let weak = win.as_weak();
    win.on_shortcut(move |text, ctrl| {
        let Some(win) = weak.upgrade() else {
            return false;
        };
        let ctx = Context {
            unlocked: win.get_unlocked(),
            busy: win.get_busy(),
            can_transfer: win.get_transfer_accounts().row_count() > 0,
            modals: modals(&win),
        };
        match manage::route(&text, ctrl, &ctx) {
            Some(cmd) => {
                run_command(&win, cmd);
                true
            }
            None => false,
        }
    });
}

fn wire_accounts(win: &MainWindow, vault: &VaultSlot) {
    let weak = win.as_weak();
    win.on_account_new(move || {
        let Some(win) = weak.upgrade() else { return };
        with_ui(|u| u.account_gate.reset());
        win.set_acct_edit(false);
        win.set_acct_name(SharedString::new());
        win.set_acct_color(-1);
        win.set_acct_status(SharedString::new());
        set_account_dialog(&win, false);
        win.set_acct_open(true);
    });

    let (weak, slot) = (win.as_weak(), vault.clone());
    win.on_account_edit(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&slot)) else {
            return;
        };
        let Some(account) = selected_account(&win).and_then(|id| account_by_id(&v, id)) else {
            return;
        };
        let palette = with_ui(|u| {
            u.account_gate.reset();
            u.palette.clone()
        });
        win.set_acct_edit(true);
        win.set_acct_name(account.name.clone().into());
        win.set_acct_color(
            manage::palette_index(&palette, &account.color_hex).map_or(-1, |i| i as i32),
        );
        win.set_acct_status(SharedString::new());
        set_account_dialog(&win, account.archived_unix.is_some());
        win.set_acct_open(true);
    });

    let (weak, slot) = (win.as_weak(), vault.clone());
    win.on_account_save(move |name, choice| {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&slot)) else {
            return;
        };
        let palette = with_ui(|u| u.palette.clone());
        let form = match manage::validate_account(&name, &palette, choice) {
            Ok(f) => f,
            Err(e) => {
                win.set_acct_status(e.into());
                return;
            }
        };
        let result = (|| {
            let id = if win.get_acct_edit() {
                let id = selected_account(&win).ok_or("Kein Konto ausgewählt.".to_string())?;
                v.rename_account(id, form.name.clone())
                    .map_err(|e| e.to_string())?;
                id
            } else {
                v.create_tangible_account(form.name.clone())
                    .map_err(|e| e.to_string())?
            };
            if let Some(color) = form.color.clone() {
                v.set_account_color(id, Some(color))
                    .map_err(|e| e.to_string())?;
            }
            Ok::<i64, String>(id)
        })();
        match result {
            Ok(id) => {
                let created = !win.get_acct_edit();
                win.set_acct_open(false);
                show(&win, &v, Some(id));
                win.set_sync_status(
                    if created {
                        format!("Konto „{}“ angelegt.", form.name)
                    } else {
                        format!("Konto „{}“ gespeichert.", form.name)
                    }
                    .into(),
                );
                win.invoke_refocus();
            }
            Err(e) => win.set_acct_status(e.into()),
        }
    });

    let (weak, slot) = (win.as_weak(), vault.clone());
    win.on_account_archive(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&slot)) else {
            return;
        };
        let Some(account) = selected_account(&win).and_then(|id| account_by_id(&v, id)) else {
            return;
        };
        let archive = account.archived_unix.is_none();
        match v.set_account_archived(account.id, archive) {
            Ok(()) => {
                win.set_acct_status(SharedString::new());
                set_account_dialog(&win, archive);
                with_ui(|u| u.account_gate.reset());
                show(&win, &v, Some(account.id));
            }
            Err(e) => win.set_acct_status(e.to_string().into()),
        }
    });

    let (weak, slot) = (win.as_weak(), vault.clone());
    win.on_account_delete(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&slot)) else {
            return;
        };
        let Some(account) = selected_account(&win).and_then(|id| account_by_id(&v, id)) else {
            return;
        };
        if !manage::can_delete_account(account.archived_unix.is_some()) {
            win.set_acct_status("Nur archivierte Konten können gelöscht werden.".into());
            return;
        }
        if !with_ui(|u| u.account_gate.press(account.id)) {
            win.set_acct_armed(true);
            win.set_acct_status(
                format!(
                    "„{}“ mit allen Umsätzen endgültig löschen? Zum Bestätigen noch einmal klicken.",
                    account.name
                )
                .into(),
            );
            return;
        }
        match v.delete_account(account.id) {
            Ok(()) => {
                win.set_acct_open(false);
                show(&win, &v, Some(0));
                win.set_sync_status(format!("Konto „{}“ gelöscht.", account.name).into());
                win.invoke_refocus();
            }
            Err(e) => {
                win.set_acct_armed(false);
                win.set_acct_status(e.to_string().into());
            }
        }
    });
}

fn wire_categories(win: &MainWindow, vault: &VaultSlot) {
    let (weak, slot) = (win.as_weak(), vault.clone());
    win.on_categories_open(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&slot)) else {
            return;
        };
        win.set_cat_status(SharedString::new());
        win.set_cat_sub(false);
        refresh_categories(&win, &v, None);
        win.set_cat_open(true);
    });

    let weak = win.as_weak();
    win.on_category_select(move |index| {
        let Some(win) = weak.upgrade() else { return };
        let (cat, palette) = with_ui(|u| {
            u.category_gate.reset();
            (
                u.categories.get(index.max(0) as usize).cloned(),
                u.palette.clone(),
            )
        });
        let Some(cat) = cat else { return };
        win.set_cat_selected(index);
        win.set_cat_name(cat.name.clone().into());
        win.set_cat_color(manage::palette_index(&palette, &cat.color_hex).map_or(-1, |i| i as i32));
        win.set_cat_armed(false);
        win.set_cat_status(SharedString::new());
    });

    let (weak, slot) = (win.as_weak(), vault.clone());
    win.on_category_add(move |name, choice, sub| {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&slot)) else {
            return;
        };
        let (palette, parent) = with_ui(|u| {
            let parent = usize::try_from(win.get_cat_selected())
                .ok()
                .and_then(|i| u.categories.get(i))
                .filter(|_| sub)
                .map(|c| c.id);
            (u.palette.clone(), parent)
        });
        let form = match manage::validate_category(&name, &palette, choice) {
            Ok(f) => f,
            Err(e) => {
                win.set_cat_status(e.into());
                return;
            }
        };
        let color = manage::new_category_color(&form, &palette);
        match v.create_category(form.name.clone(), parent, String::new(), color) {
            Ok(id) => {
                win.set_cat_status(SharedString::new());
                refresh_categories(&win, &v, Some(id));
                reload_all(&win, &v);
            }
            Err(e) => win.set_cat_status(e.to_string().into()),
        }
    });

    let (weak, slot) = (win.as_weak(), vault.clone());
    win.on_category_save(move |name, choice| {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&slot)) else {
            return;
        };
        let (palette, current) = with_ui(|u| {
            (
                u.palette.clone(),
                usize::try_from(win.get_cat_selected())
                    .ok()
                    .and_then(|i| u.categories.get(i).cloned()),
            )
        });
        let Some(current) = current else { return };
        let form = match manage::validate_category(&name, &palette, choice) {
            Ok(f) => f,
            Err(e) => {
                win.set_cat_status(e.into());
                return;
            }
        };
        let color = form.color.unwrap_or_else(|| current.color_hex.clone());
        match v.update_category(
            current.id,
            form.name,
            current.parent_id,
            current.sf_symbol.clone(),
            color,
        ) {
            Ok(()) => {
                win.set_cat_status(SharedString::new());
                refresh_categories(&win, &v, Some(current.id));
                reload_all(&win, &v);
            }
            Err(e) => win.set_cat_status(e.to_string().into()),
        }
    });

    let (weak, slot) = (win.as_weak(), vault.clone());
    win.on_category_delete(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&slot)) else {
            return;
        };
        let current = with_ui(|u| {
            usize::try_from(win.get_cat_selected())
                .ok()
                .and_then(|i| u.categories.get(i).cloned())
        });
        let Some(current) = current else { return };
        if !with_ui(|u| u.category_gate.press(current.id)) {
            win.set_cat_armed(true);
            win.set_cat_status(
                format!(
                    "„{}“ samt Unterkategorien löschen? Zum Bestätigen noch einmal klicken.",
                    current.name
                )
                .into(),
            );
            return;
        }
        match v.delete_category(current.id) {
            Ok(()) => {
                win.set_cat_status(SharedString::new());
                refresh_categories(&win, &v, None);
                reload_all(&win, &v);
            }
            Err(e) => {
                win.set_cat_armed(false);
                win.set_cat_status(e.to_string().into());
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demo_vault(tag: &str) -> (Arc<Vault>, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "girocrab-win-manage-{tag}-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let vault = Vault::open(path.display().to_string(), vec![9; 32]).unwrap();
        (vault, path)
    }

    #[test]
    fn account_lifecycle_create_edit_archive_delete() {
        let (v, path) = demo_vault("acct");
        let palette = girocrab_core::user_groups::account_color_palette();
        let form = manage::validate_account("Bargeld", &palette, 2).unwrap();
        let id = v.create_tangible_account(form.name.clone()).unwrap();
        v.set_account_color(id, form.color.clone()).unwrap();
        v.rename_account(id, "Kasse".into()).unwrap();
        let a = account_by_id(&v, id).unwrap();
        assert_eq!(a.name, "Kasse");
        assert!(a.color_hex.eq_ignore_ascii_case(&palette[2]));
        // Delete is gated on archiving: the form logic and the core agree.
        assert!(!manage::can_delete_account(a.archived_unix.is_some()));
        assert!(v.delete_account(id).is_err());
        v.set_account_archived(id, true).unwrap();
        let a = account_by_id(&v, id).unwrap();
        assert!(manage::can_delete_account(a.archived_unix.is_some()));
        v.delete_account(id).unwrap();
        assert!(account_by_id(&v, id).is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn category_lifecycle_create_sub_rename_delete() {
        let (v, path) = demo_vault("cat");
        let palette = girocrab_core::user_groups::account_color_palette();
        let form = manage::validate_category("Reisen", &palette, -1).unwrap();
        let color = manage::new_category_color(&form, &palette);
        let parent = v
            .create_category(form.name, None, String::new(), color)
            .unwrap();
        let sub = v
            .create_category(
                "Bahn".into(),
                Some(parent),
                String::new(),
                palette[1].clone(),
            )
            .unwrap();
        let cats = v.list_categories(None, None).unwrap();
        let sub_cat = cats.iter().find(|c| c.id == sub).unwrap();
        assert_eq!(sub_cat.depth, 1);
        v.update_category(
            sub,
            "Fernverkehr".into(),
            sub_cat.parent_id,
            sub_cat.sf_symbol.clone(),
            palette[3].clone(),
        )
        .unwrap();
        let cats = v.list_categories(None, None).unwrap();
        let renamed = cats.iter().find(|c| c.id == sub).unwrap();
        assert_eq!(renamed.name, "Fernverkehr");
        assert!(renamed.color_hex.eq_ignore_ascii_case(&palette[3]));
        // Deleting the parent takes the sub-category with it.
        v.delete_category(parent).unwrap();
        let cats = v.list_categories(None, None).unwrap();
        assert!(cats.iter().all(|c| c.id != parent && c.id != sub));
        let _ = std::fs::remove_file(&path);
    }
}
