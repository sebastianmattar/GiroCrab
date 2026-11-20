//! Slint wiring of the "Import, Export & Einstellungen" dialog (`ui/settings_import_dialog.slint`):
//! statement and MoneyMoney imports, Amazon orders, CSV / CAMT.053 export, encrypted backup,
//! settings and the Abrufprotokoll. The pure texts and checks live in `settings_import.rs`. Every
//! vault call, file dialog and file access runs on a worker thread; results come back through
//! `upgrade_in_event_loop`.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use girocrab_core::export::{
    Camt053Request, CamtVersion, ExportCsvBalanceLine, ExportCsvColumns, export_csv,
};
use girocrab_core::importers::moneymoney_db::{
    MoneyMoneyDbAccountChoice, MoneyMoneyDbImportOptions, MoneyMoneyDbImportProgress,
    MoneyMoneyImportObserver,
};
use girocrab_core::sync_log::{SyncLogEntry, SyncLogFilter};
use girocrab_core::{BookingFilter, ConnectorKind, SettingEntry, Vault};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::announcements::{self, Announcement};
use crate::settings_import::{self as view, Line};
use crate::{MainWindow, SettingsImportState, SiRow, german_date, money, privacy_on};

type VaultSlot = Arc<Mutex<Option<Arc<Vault>>>>;

const CAMT_VERSIONS: [&str; 2] = [
    "camt.053.001.02 (ältere Programme)",
    "camt.053.001.08 (aktuell)",
];

#[derive(Default)]
struct Ui {
    /// Import targets in menu order; `None` = new archive account.
    targets: Vec<Option<i64>>,
    /// Accounts in the export menu: (id, name).
    accounts: Vec<(i64, String)>,
    amazon_ids: Vec<i64>,
    file: Option<PathBuf>,
    db_path: Option<String>,
    amazon_json: Option<String>,
    settings: Vec<SettingEntry>,
    log: Vec<SyncLogEntry>,
    restore_armed: bool,
    clear_armed: bool,
    cancel: Option<Arc<AtomicBool>>,
    /// Announcements shown in the Mitteilungen tab (not dismissed).
    news: Vec<Announcement>,
}

thread_local! {
    static UI: RefCell<Ui> = RefCell::new(Ui::default());
    static SLOT: RefCell<Option<VaultSlot>> = const { RefCell::new(None) };
}

fn with_ui<R>(f: impl FnOnce(&mut Ui) -> R) -> R {
    UI.with(|u| f(&mut u.borrow_mut()))
}

fn vault_now() -> Option<Arc<Vault>> {
    SLOT.with(|s| s.borrow().as_ref().and_then(|s| s.lock().unwrap().clone()))
}

fn eur(c: i64) -> String {
    money(c, "EUR")
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn names(items: Vec<String>) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        items
            .into_iter()
            .map(SharedString::from)
            .collect::<Vec<_>>(),
    ))
}

fn rows(lines: Vec<Line>) -> ModelRc<SiRow> {
    ModelRc::new(VecModel::from(
        lines
            .into_iter()
            .map(|l| SiRow {
                title: l.title.into(),
                detail: l.detail.into(),
            })
            .collect::<Vec<_>>(),
    ))
}

/// Closes the dialog and forgets its state (and every typed password) when the vault locks.
pub fn on_lock(win: &MainWindow) {
    let gs = win.global::<SettingsImportState>();
    gs.set_open(false);
    gs.set_db_password(SharedString::new());
    gs.set_backup_password(SharedString::new());
    gs.set_backup_repeat(SharedString::new());
    gs.set_setting_value(SharedString::new());
    if let Some(c) = with_ui(|u| u.cancel.clone()) {
        c.store(true, Ordering::SeqCst);
    }
    with_ui(|u| *u = Ui::default());
}

fn set_status(win: &MainWindow, text: impl Into<SharedString>) {
    win.global::<SettingsImportState>().set_status(text.into());
}

/// Runs `work` on a worker thread; `done` and errors are shown on the UI thread.
fn spawn<T: Send + 'static>(
    win: &MainWindow,
    busy_text: &str,
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
    done: impl FnOnce(&MainWindow, T) + Send + 'static,
) {
    let gs = win.global::<SettingsImportState>();
    gs.set_busy(true);
    gs.set_status(busy_text.into());
    let weak = win.as_weak();
    std::thread::spawn(move || {
        let result = work();
        let _ = weak.upgrade_in_event_loop(move |win| {
            let gs = win.global::<SettingsImportState>();
            gs.set_busy(false);
            gs.set_show_progress(false);
            match result {
                Ok(v) => done(&win, v),
                Err(e) => set_status(&win, e),
            }
        });
    });
}

/// Reloads the booking list after a change of the vault's contents.
fn refresh_main(win: &MainWindow, v: &Arc<Vault>) {
    let sel = Some(win.get_selected() as i64).filter(|s| *s >= 0);
    crate::views_ui::invalidate_planned_count();
    crate::show(win, v, sel);
}

// ---- files -------------------------------------------------------------------------------------

#[cfg(windows)]
fn pick_file(title: &str, filter: &str, exts: &[&str]) -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title(title)
        .add_filter(filter, exts)
        .pick_file()
}

#[cfg(not(windows))]
fn pick_file(_title: &str, _filter: &str, _exts: &[&str]) -> Option<PathBuf> {
    std::env::var_os("GIROCRAB_IMPORT_FILE").map(PathBuf::from)
}

#[cfg(windows)]
fn pick_dir(title: &str) -> Option<PathBuf> {
    rfd::FileDialog::new().set_title(title).pick_folder()
}

#[cfg(not(windows))]
fn pick_dir(_title: &str) -> Option<PathBuf> {
    std::env::var_os("GIROCRAB_IMPORT_FILE").map(PathBuf::from)
}

#[cfg(windows)]
fn save_file(title: &str, name: &str) -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title(title)
        .set_file_name(name)
        .save_file()
}

#[cfg(not(windows))]
fn save_file(_title: &str, name: &str) -> Option<PathBuf> {
    std::env::var_os("GIROCRAB_EXPORT_DIR").map(|d| PathBuf::from(d).join(name))
}

fn read_bytes(path: &PathBuf) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("Die Datei konnte nicht gelesen werden: {e}"))
}

fn write_bytes(path: &PathBuf, data: &[u8]) -> Result<(), String> {
    std::fs::write(path, data)
        .map_err(|e| format!("Die Datei konnte nicht geschrieben werden: {e}"))
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

// ---- progress ------------------------------------------------------------------------------------

struct Observer {
    weak: slint::Weak<MainWindow>,
    cancel: Arc<AtomicBool>,
}

impl MoneyMoneyImportObserver for Observer {
    fn progress(&self, p: MoneyMoneyDbImportProgress) -> bool {
        let text = view::progress_text(&p);
        let fraction = p.fraction.clamp(0.0, 1.0) as f32;
        let _ = self.weak.upgrade_in_event_loop(move |win| {
            let gs = win.global::<SettingsImportState>();
            gs.set_progress(fraction);
            gs.set_status(text.into());
        });
        !self.cancel.load(Ordering::SeqCst)
    }
}

// ---- wiring --------------------------------------------------------------------------------------

pub fn wire(win: &MainWindow, slot: VaultSlot) {
    SLOT.with(|s| *s.borrow_mut() = Some(slot));
    let gs = win.global::<SettingsImportState>();
    gs.set_camt_versions(names(CAMT_VERSIONS.iter().map(|s| s.to_string()).collect()));
    gs.set_mode_names(names(view::product_mode_names()));
    wire_open(win);
    wire_import(win);
    wire_export(win);
    wire_backup(win);
    wire_settings(win);
    wire_log(win);
    wire_news(win);
}

fn wire_open(win: &MainWindow) {
    let gs = win.global::<SettingsImportState>();

    let weak = win.as_weak();
    gs.on_request(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_now()) else {
            return;
        };
        if privacy_on(&win) {
            return;
        }
        let gs = win.global::<SettingsImportState>();
        gs.set_tab(0);
        gs.set_open(true);
        with_ui(|u| {
            u.restore_armed = false;
            u.clear_armed = false;
        });
        spawn(
            &win,
            "Wird geladen …",
            move || {
                let accounts = v.list_accounts().map_err(|e| e.to_string())?;
                let connections = v.list_bank_connections().map_err(|e| e.to_string())?;
                Ok((accounts, connections))
            },
            |win, (accounts, connections)| {
                let mut targets = vec![None];
                let mut labels = vec!["Neues Archivkonto".to_string()];
                for a in &accounts {
                    targets.push(Some(a.id));
                    labels.push(a.name.clone());
                }
                let amazon: Vec<_> = connections
                    .iter()
                    .filter(|c| c.connector == ConnectorKind::AmazonOrders)
                    .collect();
                let gs = win.global::<SettingsImportState>();
                gs.set_target_names(names(labels));
                gs.set_target(0);
                gs.set_acc_names(names(accounts.iter().map(|a| a.name.clone()).collect()));
                gs.set_acc(0);
                gs.set_amazon_names(names(
                    amazon.iter().map(|c| c.display_name.clone()).collect(),
                ));
                gs.set_amazon(0);
                gs.set_status(SharedString::new());
                with_ui(|u| {
                    u.targets = targets;
                    u.accounts = accounts.iter().map(|a| (a.id, a.name.clone())).collect();
                    u.amazon_ids = amazon.iter().map(|c| c.id).collect();
                });
            },
        );
    });

    let weak = win.as_weak();
    gs.on_tab_changed(move |tab| {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_now()) else {
            return;
        };
        set_status(&win, "");
        with_ui(|u| {
            u.restore_armed = false;
            u.clear_armed = false;
        });
        let gs = win.global::<SettingsImportState>();
        gs.set_restore_armed(false);
        gs.set_clear_armed(false);
        match tab {
            3 => load_settings(&win, &v),
            4 => load_log(&win, &v),
            5 => refresh_news(&win, &v),
            _ => {}
        }
    });
}

// ---- import ---------------------------------------------------------------------------------------

fn target_id(win: &MainWindow) -> Option<i64> {
    let i = usize::try_from(win.global::<SettingsImportState>().get_target()).ok()?;
    with_ui(|u| u.targets.get(i).copied().flatten())
}

fn wire_import(win: &MainWindow) {
    let gs = win.global::<SettingsImportState>();

    // statement file: pick, check, then the core's dry run
    let weak = win.as_weak();
    gs.on_pick_file(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_now()) else {
            return;
        };
        let target = target_id(&win);
        win.global::<SettingsImportState>().set_file_ready(false);
        spawn(
            &win,
            "Datei wird geprüft …",
            move || {
                let path = pick_file(
                    "Kontoauszug wählen",
                    "CSV, XML, CAMT, MT940",
                    &["csv", "xml", "sta", "mt940", "txt"],
                )
                .ok_or("Import abgebrochen.".to_string())?;
                let data = read_bytes(&path)?;
                let name = file_name(&path);
                let (format, parsed) = view::parse_statement(&name, &data)?;
                let check = view::check_rows(format, &parsed);
                let preview = v
                    .preview_moneymoney_import(data, name, target)
                    .map_err(|e| e.to_string())?;
                let text = format!(
                    "{} {}",
                    view::file_check_text(&check),
                    view::preview_text(&preview, &german_date)
                );
                Ok((path, text))
            },
            |win, (path, text)| {
                with_ui(|u| u.file = Some(path));
                let gs = win.global::<SettingsImportState>();
                gs.set_file_info(text.into());
                gs.set_file_ready(true);
            },
        );
    });

    let weak = win.as_weak();
    gs.on_run_import(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_now()) else {
            return;
        };
        let Some(path) = with_ui(|u| u.file.clone()) else {
            return;
        };
        let target = target_id(&win);
        let vault = v.clone();
        spawn(
            &win,
            "Wird importiert …",
            move || {
                let data = read_bytes(&path)?;
                let r = v
                    .import_moneymoney_data(data, file_name(&path), target, true, true)
                    .map_err(|e| e.to_string())?;
                Ok(view::import_result_text(&r))
            },
            move |win, text| {
                refresh_main(win, &vault);
                let gs = win.global::<SettingsImportState>();
                gs.set_file_ready(false);
                gs.set_file_info(text.into());
            },
        );
    });

    // MoneyMoney database
    let weak = win.as_weak();
    gs.on_pick_db(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_now()) else {
            return;
        };
        let password = win
            .global::<SettingsImportState>()
            .get_db_password()
            .to_string();
        if password.is_empty() {
            set_status(&win, "Bitte zuerst das Passwort der Datenbank eintragen.");
            return;
        }
        spawn(
            &win,
            "Datenbank wird gelesen …",
            move || {
                let path = pick_file(
                    "Datenbank wählen",
                    "Datenbank",
                    &["moneymoney", "sqlite", "db"],
                )
                .or_else(|| pick_dir("Ordner mit Datenbank wählen"))
                .ok_or("Import abgebrochen.".to_string())?;
                let location = path.display().to_string();
                let p = v
                    .preview_moneymoney_database(location.clone(), password, None)
                    .map_err(|e| e.to_string())?;
                let text = format!("{} Konten gefunden.", p.accounts.len());
                Ok((location, text))
            },
            |win, (location, text)| {
                with_ui(|u| u.db_path = Some(location));
                let gs = win.global::<SettingsImportState>();
                gs.set_db_info(text.into());
                gs.set_db_ready(true);
            },
        );
    });

    let weak = win.as_weak();
    gs.on_run_db(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_now()) else {
            return;
        };
        let Some(location) = with_ui(|u| u.db_path.clone()) else {
            return;
        };
        let password = win
            .global::<SettingsImportState>()
            .get_db_password()
            .to_string();
        let cancel = Arc::new(AtomicBool::new(false));
        with_ui(|u| u.cancel = Some(cancel.clone()));
        let observer = Arc::new(Observer {
            weak: win.as_weak(),
            cancel,
        });
        let vault = v.clone();
        spawn(
            &win,
            "Datenbank wird gelesen …",
            move || {
                let preview = v
                    .preview_moneymoney_database(location.clone(), password.clone(), None)
                    .map_err(|e| e.to_string())?;
                let options = MoneyMoneyDbImportOptions {
                    accounts: preview
                        .accounts
                        .iter()
                        .map(|a| MoneyMoneyDbAccountChoice {
                            mm_id: a.mm_id,
                            import: true,
                            kind: a.suggested_kind,
                            target_account_id: a.existing_account_id,
                        })
                        .collect(),
                    import_categories: true,
                    import_notes: true,
                    create_connections: false,
                    import_groups: false,
                    import_statements: false,
                    statements_dir: None,
                    bank_choices: vec![],
                    import_transfer_templates: true,
                };
                let r = v
                    .import_moneymoney_database_with_progress(location, password, options, observer)
                    .map_err(|e| e.to_string())?;
                Ok(view::db_result_text(&r))
            },
            move |win, text| {
                refresh_main(win, &vault);
                let gs = win.global::<SettingsImportState>();
                gs.set_db_ready(false);
                gs.set_db_password(SharedString::new());
                gs.set_db_info(text.clone().into());
                gs.set_status(text.into());
                with_ui(|u| u.cancel = None);
            },
        );
        win.global::<SettingsImportState>().set_progress(0.0);
        win.global::<SettingsImportState>().set_show_progress(true);
    });

    let weak = win.as_weak();
    gs.on_cancel_db(move || {
        if let Some(c) = with_ui(|u| u.cancel.clone()) {
            c.store(true, Ordering::SeqCst);
        }
        if let Some(win) = weak.upgrade() {
            win.global::<SettingsImportState>().set_show_progress(false);
        }
    });

    // Amazon orders
    let weak = win.as_weak();
    gs.on_pick_amazon(move || {
        let Some(win) = weak.upgrade() else { return };
        win.global::<SettingsImportState>().set_amazon_ready(false);
        spawn(
            &win,
            "Datei wird geprüft …",
            move || {
                let path = pick_file("Bestell-Export wählen", "JSON", &["json", "txt"])
                    .ok_or("Import abgebrochen.".to_string())?;
                let text = String::from_utf8(read_bytes(&path)?)
                    .map_err(|_| "Die Datei ist kein Text (UTF-8).".to_string())?;
                let check = view::amazon_check(&text)?;
                Ok((text, view::amazon_text(&check, &eur)))
            },
            |win, (json, info)| {
                with_ui(|u| u.amazon_json = Some(json));
                let gs = win.global::<SettingsImportState>();
                gs.set_amazon_info(info.into());
                gs.set_amazon_ready(true);
            },
        );
    });

    let weak = win.as_weak();
    gs.on_run_amazon(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_now()) else {
            return;
        };
        let idx = usize::try_from(win.global::<SettingsImportState>().get_amazon()).ok();
        let (Some(json), Some(id)) = (
            with_ui(|u| u.amazon_json.clone()),
            idx.and_then(|i| with_ui(|u| u.amazon_ids.get(i).copied())),
        ) else {
            return;
        };
        let vault = v.clone();
        spawn(
            &win,
            "Bestellungen werden importiert …",
            move || {
                let s = v
                    .import_amazon_orders(id, json)
                    .map_err(|e| e.to_string())?;
                Ok(format!(
                    "Amazon-Import: {} neue Umsätze.",
                    s.transactions_imported
                ))
            },
            move |win, text| {
                refresh_main(win, &vault);
                let gs = win.global::<SettingsImportState>();
                gs.set_amazon_ready(false);
                gs.set_amazon_info(text.into());
            },
        );
    });
}

// ---- export ---------------------------------------------------------------------------------------

type Period = (Option<String>, Option<String>);

fn export_account(win: &MainWindow) -> Result<(i64, Period), String> {
    let gs = win.global::<SettingsImportState>();
    let (id, _) = usize::try_from(gs.get_acc())
        .ok()
        .and_then(|i| with_ui(|u| u.accounts.get(i).cloned()))
        .ok_or("Kein Konto vorhanden.".to_string())?;
    let from = view::parse_day(&gs.get_date_from())?;
    let to = view::parse_day(&gs.get_date_to())?;
    Ok((id, (from, to)))
}

fn wire_export(win: &MainWindow) {
    let gs = win.global::<SettingsImportState>();

    let weak = win.as_weak();
    gs.on_export_csv(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_now()) else {
            return;
        };
        let (id, (from, to)) = match export_account(&win) {
            Ok(x) => x,
            Err(e) => return set_status(&win, e),
        };
        let gs = win.global::<SettingsImportState>();
        let columns = ExportCsvColumns {
            category: gs.get_col_category(),
            note: gs.get_col_note(),
            daily_balance: false,
            running_balance: false,
            iban: false,
            tags: false,
        };
        let with_balance = gs.get_col_balance();
        spawn(
            &win,
            "Wird exportiert …",
            move || {
                let page = v
                    .list_transactions(BookingFilter {
                        account_ids: vec![id],
                        date_from: from,
                        date_to: to,
                        limit: 0,
                        ..Default::default()
                    })
                    .map_err(|e| e.to_string())?;
                let mut list = page.transactions;
                list.sort_by(|a, b| a.booking_date.cmp(&b.booking_date).then(a.id.cmp(&b.id)));
                let balance_lines = if with_balance {
                    v.list_accounts()
                        .map_err(|e| e.to_string())?
                        .into_iter()
                        .filter(|a| a.id == id)
                        .map(|a| ExportCsvBalanceLine {
                            name: a.name,
                            balance_cents: a.balance_cents,
                            currency: a.currency,
                            date: a.balance_date,
                        })
                        .collect()
                } else {
                    vec![]
                };
                let csv = export_csv(view::export_rows(&list), columns, balance_lines);
                let name = view::export_file_name("csv", &view::today(now_secs()));
                let path = save_file("Umsätze exportieren", &name)
                    .ok_or("Export abgebrochen.".to_string())?;
                write_bytes(&path, csv.as_bytes())?;
                Ok(format!("{} Umsätze exportiert.", list.len()))
            },
            set_status,
        );
    });

    let weak = win.as_weak();
    gs.on_export_camt(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_now()) else {
            return;
        };
        let (id, (from, to)) = match export_account(&win) {
            Ok(x) => x,
            Err(e) => return set_status(&win, e),
        };
        let version = if win.global::<SettingsImportState>().get_camt_version() == 0 {
            CamtVersion::V02
        } else {
            CamtVersion::V08
        };
        spawn(
            &win,
            "Wird exportiert …",
            move || {
                let now = now_secs();
                let xml = v
                    .camt053_export(Camt053Request {
                        account_id: id,
                        date_from: from,
                        date_to: to,
                        booking_ids: None,
                        version,
                        created_at: view::iso_utc(now),
                    })
                    .map_err(|e| e.to_string())?;
                let name = view::export_file_name("camt", &view::today(now));
                let path = save_file("CAMT.053 speichern", &name)
                    .ok_or("Export abgebrochen.".to_string())?;
                write_bytes(&path, xml.as_bytes())?;
                Ok("CAMT.053 gespeichert.".to_string())
            },
            set_status,
        );
    });
}

// ---- backup ---------------------------------------------------------------------------------------

fn wire_backup(win: &MainWindow) {
    let gs = win.global::<SettingsImportState>();

    let weak = win.as_weak();
    gs.on_write_backup(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_now()) else {
            return;
        };
        let gs = win.global::<SettingsImportState>();
        let (pw, repeat) = (
            gs.get_backup_password().to_string(),
            gs.get_backup_repeat().to_string(),
        );
        if let Err(e) = view::validate_backup_password(&pw, &repeat, true) {
            return set_status(&win, e);
        }
        spawn(
            &win,
            "Sicherung wird geschrieben …",
            move || {
                let name = format!("girocrab-sicherung-{}.girocrab", view::today(now_secs()));
                let path = save_file("Sicherung speichern", &name)
                    .ok_or("Sicherung abgebrochen.".to_string())?;
                v.write_backup(path.display().to_string(), pw)
                    .map_err(|e| e.to_string())?;
                Ok("Sicherung geschrieben.".to_string())
            },
            |win, text| {
                let gs = win.global::<SettingsImportState>();
                gs.set_backup_password(SharedString::new());
                gs.set_backup_repeat(SharedString::new());
                set_status(win, text);
            },
        );
    });

    let weak = win.as_weak();
    gs.on_restore_backup(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_now()) else {
            return;
        };
        let gs = win.global::<SettingsImportState>();
        let pw = gs.get_backup_password().to_string();
        if let Err(e) = view::validate_backup_password(&pw, "", false) {
            return set_status(&win, e);
        }
        // Replacing the vault needs a second click.
        if !with_ui(|u| std::mem::replace(&mut u.restore_armed, true)) {
            gs.set_restore_armed(true);
            return set_status(
                &win,
                "Die aktuellen Daten werden ersetzt. Zum Bestätigen noch einmal klicken.",
            );
        }
        with_ui(|u| u.restore_armed = false);
        gs.set_restore_armed(false);
        let vault = v.clone();
        spawn(
            &win,
            "Sicherung wird eingespielt …",
            move || {
                let path = pick_file(
                    "Sicherung wählen",
                    "GiroCrab-Sicherung",
                    &["girocrab", "db", "sqlite"],
                )
                .ok_or("Wiederherstellung abgebrochen.".to_string())?;
                v.restore_backup(path.display().to_string(), pw)
                    .map_err(|e| e.to_string())?;
                Ok("Sicherung wiederhergestellt.".to_string())
            },
            move |win, text| {
                win.global::<SettingsImportState>()
                    .set_backup_password(SharedString::new());
                refresh_main(win, &vault);
                set_status(win, text);
            },
        );
    });
}

// ---- settings -------------------------------------------------------------------------------------

fn load_settings(win: &MainWindow, v: &Arc<Vault>) {
    let v = v.clone();
    spawn(
        win,
        "Wird geladen …",
        move || {
            let list = v.list_settings().map_err(|e| e.to_string())?;
            let mode = v.fints_product_id_mode().map_err(|e| e.to_string())?;
            let source = v.fints_product_id_source().map_err(|e| e.to_string())?;
            Ok((list, mode, source))
        },
        |win, (list, mode, source)| {
            let gs = win.global::<SettingsImportState>();
            gs.set_settings(rows(view::setting_lines(&list)));
            gs.set_sel_setting(-1);
            gs.set_mode(view::product_mode_index(&mode));
            gs.set_mode_source(view::product_source_text(&source).into());
            gs.set_status(SharedString::new());
            with_ui(|u| u.settings = list);
        },
    );
}

fn wire_settings(win: &MainWindow) {
    let gs = win.global::<SettingsImportState>();

    let weak = win.as_weak();
    gs.on_setting_select(move |i| {
        let Some(win) = weak.upgrade() else { return };
        let Some(entry) = usize::try_from(i)
            .ok()
            .and_then(|i| with_ui(|u| u.settings.get(i).cloned()))
        else {
            return;
        };
        let gs = win.global::<SettingsImportState>();
        gs.set_sel_setting(i);
        gs.set_setting_label(entry.label.into());
        gs.set_setting_help(entry.help.into());
        gs.set_setting_secret(entry.is_secret);
        // the core leaves a secret's value empty: it is never shown
        gs.set_setting_value(entry.value.into());
    });

    let weak = win.as_weak();
    gs.on_setting_save(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_now()) else {
            return;
        };
        let gs = win.global::<SettingsImportState>();
        let Some(key) = usize::try_from(gs.get_sel_setting())
            .ok()
            .and_then(|i| with_ui(|u| u.settings.get(i).map(|s| s.key.clone())))
        else {
            return;
        };
        let value = gs.get_setting_value().to_string();
        gs.set_setting_value(SharedString::new());
        let vault = v.clone();
        spawn(
            &win,
            "Wird gespeichert …",
            move || {
                v.set_setting(key.clone(), value)
                    .map_err(|e| e.to_string())?;
                // read back through the core: proves the value is stored
                v.get_setting(key).map_err(|e| e.to_string())?;
                Ok(())
            },
            move |win, ()| {
                load_settings(win, &vault);
                set_status(win, "Gespeichert.");
            },
        );
    });

    let weak = win.as_weak();
    gs.on_mode_changed(move |index| {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_now()) else {
            return;
        };
        let mode = view::product_mode_value(index).to_string();
        spawn(
            &win,
            "Wird gespeichert …",
            move || {
                v.set_fints_product_id_mode(mode)
                    .map_err(|e| e.to_string())?;
                v.fints_product_id_source().map_err(|e| e.to_string())
            },
            |win, source| {
                win.global::<SettingsImportState>()
                    .set_mode_source(view::product_source_text(&source).into());
                set_status(win, "");
            },
        );
    });
}

// ---- Abrufprotokoll ----------------------------------------------------------------------------------

fn load_log(win: &MainWindow, v: &Arc<Vault>) {
    let gs = win.global::<SettingsImportState>();
    let search = gs.get_log_search().trim().to_string();
    let filter = SyncLogFilter {
        connection_id: None,
        only_errors: gs.get_log_errors(),
        search: Some(search).filter(|s| !s.is_empty()),
        limit: 500,
    };
    let v = v.clone();
    spawn(
        win,
        "Wird geladen …",
        move || v.list_sync_log(filter).map_err(|e| e.to_string()),
        |win, list| {
            let gs = win.global::<SettingsImportState>();
            gs.set_log(rows(view::log_lines(&list)));
            gs.set_log_summary(format!("{} Einträge (Zeiten in UTC)", list.len()).into());
            gs.set_status(SharedString::new());
            with_ui(|u| u.log = list);
        },
    );
}

fn wire_log(win: &MainWindow) {
    let gs = win.global::<SettingsImportState>();

    let weak = win.as_weak();
    gs.on_log_reload(move || {
        if let (Some(win), Some(v)) = (weak.upgrade(), vault_now()) {
            load_log(&win, &v);
        }
    });

    let weak = win.as_weak();
    gs.on_log_clear(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_now()) else {
            return;
        };
        if !with_ui(|u| std::mem::replace(&mut u.clear_armed, true)) {
            win.global::<SettingsImportState>().set_clear_armed(true);
            return;
        }
        with_ui(|u| u.clear_armed = false);
        win.global::<SettingsImportState>().set_clear_armed(false);
        let vault = v.clone();
        spawn(
            &win,
            "Wird geleert …",
            move || v.clear_sync_log().map_err(|e| e.to_string()),
            move |win, ()| load_log(win, &vault),
        );
    });

    let weak = win.as_weak();
    gs.on_log_export(move || {
        let Some(win) = weak.upgrade() else { return };
        let list = with_ui(|u| u.log.clone());
        spawn(
            &win,
            "Wird gespeichert …",
            move || {
                let name = format!("girocrab-abrufprotokoll-{}.xml", view::today(now_secs()));
                let path = save_file("Abrufprotokoll speichern", &name)
                    .ok_or("Export abgebrochen.".to_string())?;
                write_bytes(&path, view::log_xml(&list).as_bytes())?;
                Ok(format!("{} Einträge gespeichert.", list.len()))
            },
            set_status,
        );
    });
}

// ---- announcements ----------------------------------------------------------------------------------

const DISMISSED_KEY: &str = "announcements.dismissed";

/// Loads the applicable, not yet dismissed announcements into the banner and the Mitteilungen tab.
pub fn refresh_news(win: &MainWindow, v: &Arc<Vault>) {
    let setting = v
        .get_setting(DISMISSED_KEY.into())
        .ok()
        .flatten()
        .unwrap_or_default();
    let dismissed = announcements::dismissed_ids(&setting);
    let list: Vec<Announcement> = announcements::current(
        env!("CARGO_PKG_VERSION"),
        &crate::daily::date_days_ago(0),
        "de",
    )
    .into_iter()
    .filter(|a| !dismissed.contains(&a.id))
    .collect();
    let gs = win.global::<SettingsImportState>();
    let first = list.first();
    gs.set_banner_title(first.map(|a| a.title.clone()).unwrap_or_default().into());
    gs.set_banner_important(first.is_some_and(|a| a.important));
    gs.set_news(rows(
        list.iter()
            .map(|a| Line {
                title: a.title.clone(),
                detail: a.summary.clone(),
            })
            .collect(),
    ));
    gs.set_sel_news(-1);
    gs.set_news_body(SharedString::new());
    gs.set_news_has_link(false);
    with_ui(|u| u.news = list);
}

fn dismiss_news(win: &MainWindow, v: &Arc<Vault>, id: &str) {
    let setting = v
        .get_setting(DISMISSED_KEY.into())
        .ok()
        .flatten()
        .unwrap_or_default();
    match v.set_setting(
        DISMISSED_KEY.into(),
        announcements::with_dismissed(&setting, id),
    ) {
        Ok(()) => refresh_news(win, v),
        Err(e) => set_status(win, e.to_string()),
    }
}

fn wire_news(win: &MainWindow) {
    let gs = win.global::<SettingsImportState>();

    let weak = win.as_weak();
    gs.on_news_select(move |i| {
        let Some(win) = weak.upgrade() else { return };
        let gs = win.global::<SettingsImportState>();
        let item = usize::try_from(i)
            .ok()
            .and_then(|i| with_ui(|u| u.news.get(i).cloned()));
        gs.set_sel_news(if item.is_some() { i } else { -1 });
        gs.set_news_body(
            item.as_ref()
                .map(|a| announcements::plain(&a.body))
                .unwrap_or_default()
                .into(),
        );
        gs.set_news_has_link(item.is_some_and(|a| !a.link.is_empty()));
    });

    let weak = win.as_weak();
    gs.on_news_dismiss(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_now()) else {
            return;
        };
        let sel = win.global::<SettingsImportState>().get_sel_news();
        let id = usize::try_from(sel)
            .ok()
            .and_then(|i| with_ui(|u| u.news.get(i).map(|a| a.id.clone())));
        if let Some(id) = id {
            dismiss_news(&win, &v, &id);
        }
    });

    let weak = win.as_weak();
    gs.on_news_open_link(move || {
        let Some(win) = weak.upgrade() else { return };
        let sel = win.global::<SettingsImportState>().get_sel_news();
        let link = usize::try_from(sel)
            .ok()
            .and_then(|i| with_ui(|u| u.news.get(i).map(|a| a.link.clone())));
        if let Some(link) = link
            && let Err(e) =
                crate::sync_flow::safe_https_url(&link).and_then(crate::sync_flow::open_in_browser)
        {
            set_status(&win, e);
        }
    });

    let weak = win.as_weak();
    gs.on_banner_dismiss(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_now()) else {
            return;
        };
        if let Some(id) = with_ui(|u| u.news.first().map(|a| a.id.clone())) {
            dismiss_news(&win, &v, &id);
        }
    });

    let weak = win.as_weak();
    gs.on_banner_show(move || {
        let Some(win) = weak.upgrade() else { return };
        let gs = win.global::<SettingsImportState>();
        gs.set_tab(5);
        gs.set_open(true);
        gs.invoke_tab_changed(5);
        gs.invoke_news_select(0);
    });
}
