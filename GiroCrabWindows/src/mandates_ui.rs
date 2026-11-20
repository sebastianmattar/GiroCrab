//! Slint wiring of the "Lastschriften & Mandate" dialog (`ui/mandates_dialog.slint`): creditor ID,
//! mandates (form, CSV and pain.008 import, CSV export), the collection run (pain.008 files,
//! "collected" marker), returns and the batch-transfer CSV preview. The pure texts live in
//! `mandates_view.rs`. Every vault call, file dialog and file access runs on a worker thread;
//! results come back through `upgrade_in_event_loop`.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use girocrab_core::Vault;
use girocrab_core::batch_csv::parse_batch_csv;
use girocrab_core::mandates::{
    DebitFile, Mandate, MandateBooking, MandateCsvRow, parse_mandate_csv,
};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::mandates_view::{self as view, Form, Line, Pick};
use crate::{MainWindow, MandatesState, MdRow, german_date, money, privacy_on};

type VaultSlot = Arc<Mutex<Option<Arc<Vault>>>>;

#[derive(Default)]
struct Ui {
    /// Creditor accounts in the order of the account menu.
    accounts: Vec<i64>,
    mandates: Vec<Mandate>,
    picks: Vec<Pick>,
    csv: Vec<MandateCsvRow>,
    returns: Vec<MandateBooking>,
    /// Files of the last run and its due date, waiting for "als eingezogen markieren".
    files: Vec<DebitFile>,
    due: String,
    armed_delete: Option<i64>,
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

fn model(lines: Vec<Line>) -> ModelRc<MdRow> {
    ModelRc::new(VecModel::from(
        lines
            .into_iter()
            .map(|l| MdRow {
                title: l.title.into(),
                detail: l.detail.into(),
                amount: l.amount.into(),
                checked: l.checked,
                negative: l.negative,
            })
            .collect::<Vec<_>>(),
    ))
}

fn names(items: Vec<String>) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        items
            .into_iter()
            .map(SharedString::from)
            .collect::<Vec<_>>(),
    ))
}

/// Closes the dialog and forgets its state when the vault locks.
pub fn on_lock(win: &MainWindow) {
    win.global::<MandatesState>().set_open(false);
    with_ui(|u| *u = Ui::default());
}

fn account_id(win: &MainWindow) -> Option<i64> {
    let i = usize::try_from(win.global::<MandatesState>().get_acc()).ok()?;
    with_ui(|u| u.accounts.get(i).copied())
}

fn set_status(win: &MainWindow, text: impl Into<SharedString>) {
    win.global::<MandatesState>().set_status(text.into());
}

/// Runs `work` on a worker thread; `done` and errors are shown on the UI thread.
fn spawn<T: Send + 'static>(
    win: &MainWindow,
    busy_text: &str,
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
    done: impl FnOnce(&MainWindow, T) + Send + 'static,
) {
    let gs = win.global::<MandatesState>();
    gs.set_busy(true);
    gs.set_status(busy_text.into());
    let weak = win.as_weak();
    std::thread::spawn(move || {
        let result = work();
        let _ = weak.upgrade_in_event_loop(move |win| {
            win.global::<MandatesState>().set_busy(false);
            match result {
                Ok(v) => done(&win, v),
                Err(e) => set_status(&win, e),
            }
        });
    });
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

#[cfg(windows)]
fn pick_dir(title: &str) -> Option<PathBuf> {
    rfd::FileDialog::new().set_title(title).pick_folder()
}

#[cfg(not(windows))]
fn pick_dir(_title: &str) -> Option<PathBuf> {
    std::env::var_os("GIROCRAB_EXPORT_DIR").map(PathBuf::from)
}

fn read_text(path: &PathBuf) -> Result<String, String> {
    let bytes =
        std::fs::read(path).map_err(|e| format!("Die Datei konnte nicht gelesen werden: {e}"))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

// ---- rendering (UI thread) -----------------------------------------------------------------------

fn render_mandates(win: &MainWindow) {
    let gs = win.global::<MandatesState>();
    let (lines, summary) = with_ui(|u| {
        (
            u.mandates
                .iter()
                .map(|m| view::mandate_line(m, &eur, &german_date))
                .collect::<Vec<_>>(),
            view::mandates_summary(&u.mandates),
        )
    });
    gs.set_mandates(model(lines));
    gs.set_mandates_summary(summary.into());
}

fn render_picks(win: &MainWindow) {
    let gs = win.global::<MandatesState>();
    let (lines, summary) = with_ui(|u| {
        (
            u.mandates
                .iter()
                .zip(&u.picks)
                .map(|(m, p)| view::pick_line(m, p, &eur))
                .collect::<Vec<_>>(),
            view::picks_summary(&u.picks, &eur),
        )
    });
    gs.set_coll_rows(model(lines));
    gs.set_coll_summary(summary.into());
}

fn render_returns(win: &MainWindow) {
    let gs = win.global::<MandatesState>();
    let (lines, summary) = with_ui(|u| {
        (
            u.returns
                .iter()
                .map(|b| {
                    let name = u
                        .mandates
                        .iter()
                        .find(|m| m.id == b.mandate_id)
                        .map_or("", |m| m.debtor_name.as_str());
                    view::booking_line(b, name, &eur, &german_date)
                })
                .collect::<Vec<_>>(),
            view::returns_summary(u.returns.len()),
        )
    });
    gs.set_returns(model(lines));
    gs.set_returns_summary(summary.into());
    gs.set_sel_return(-1);
    gs.set_history(model(Vec::new()));
}

fn render_files(win: &MainWindow) {
    let gs = win.global::<MandatesState>();
    let (lines, ready, summary) = with_ui(|u| {
        let total: i64 = u.files.iter().map(|f| f.total_cents).sum();
        let count: u32 = u.files.iter().map(|f| f.count).sum();
        (
            u.files
                .iter()
                .map(|f| view::file_line(f, &eur))
                .collect::<Vec<_>>(),
            !u.files.is_empty(),
            if u.files.is_empty() {
                String::new()
            } else {
                format!(
                    "{} Dateien gespeichert ({count} Einzüge, {}). Nach dem Einreichen bei der Bank als eingezogen markieren.",
                    u.files.len(),
                    eur(total)
                )
            },
        )
    });
    gs.set_files(model(lines));
    gs.set_files_ready(ready);
    gs.set_files_summary(summary.into());
}

fn set_form(win: &MainWindow, f: &Form, next: String) {
    let gs = win.global::<MandatesState>();
    gs.set_f_name(f.name.clone().into());
    gs.set_f_iban(f.iban.clone().into());
    gs.set_f_bic(f.bic.clone().into());
    gs.set_f_ref(f.reference.clone().into());
    gs.set_f_date(f.date.clone().into());
    gs.set_f_scheme(f.scheme);
    gs.set_f_oneoff(f.one_off);
    gs.set_f_amount(f.amount.clone().into());
    gs.set_f_purpose(f.purpose.clone().into());
    gs.set_f_note(f.note.clone().into());
    gs.set_f_active(f.active);
    gs.set_f_next(next.into());
}

fn blank_form() -> Form {
    Form {
        active: true,
        ..Form::default()
    }
}

fn read_form(win: &MainWindow) -> Form {
    let gs = win.global::<MandatesState>();
    Form {
        name: gs.get_f_name().into(),
        iban: gs.get_f_iban().into(),
        bic: gs.get_f_bic().into(),
        reference: gs.get_f_ref().into(),
        date: gs.get_f_date().into(),
        scheme: gs.get_f_scheme(),
        one_off: gs.get_f_oneoff(),
        amount: gs.get_f_amount().into(),
        purpose: gs.get_f_purpose().into(),
        note: gs.get_f_note().into(),
        active: gs.get_f_active(),
    }
}

fn clear_selection(win: &MainWindow) {
    let gs = win.global::<MandatesState>();
    gs.set_sel_mandate(-1);
    gs.set_delete_armed(false);
    with_ui(|u| u.armed_delete = None);
    set_form(win, &blank_form(), view::sequence_text(false, None));
}

// ---- loading -----------------------------------------------------------------------------------

struct Snap {
    mandates: Vec<Mandate>,
    creditor: String,
    returns: Vec<MandateBooking>,
}

fn gather(v: &Vault, account: i64) -> Result<Snap, String> {
    let e = |e: girocrab_core::CoreError| e.to_string();
    Ok(Snap {
        mandates: v.list_mandates(Some(account)).map_err(e)?,
        creditor: v.creditor_id(account).map_err(e)?,
        returns: v.open_mandate_returns(Some(account)).map_err(e)?,
    })
}

fn apply(win: &MainWindow, snap: Snap, status: String) {
    with_ui(|u| {
        u.picks = view::picks_of(&snap.mandates);
        u.mandates = snap.mandates;
        u.returns = snap.returns;
        u.files.clear();
        u.csv.clear();
    });
    let gs = win.global::<MandatesState>();
    gs.set_creditor_id(snap.creditor.into());
    gs.set_csv_rows(model(Vec::new()));
    gs.set_csv_summary(SharedString::new());
    gs.set_csv_ready(false);
    gs.set_sel_coll(-1);
    clear_selection(win);
    render_mandates(win);
    render_picks(win);
    render_returns(win);
    render_files(win);
    gs.set_status(status.into());
}

fn load(win: &MainWindow, v: &Arc<Vault>, status: String) {
    let Some(account) = account_id(win) else {
        set_status(win, "Kein Konto vorhanden.");
        return;
    };
    let v = v.clone();
    spawn(
        win,
        "Wird geladen …",
        move || gather(&v, account),
        move |win, snap| apply(win, snap, status),
    );
}

/// Runs an edit on a worker thread, then reloads the dialog.
fn job(
    win: &MainWindow,
    v: &Arc<Vault>,
    busy: &str,
    f: impl FnOnce(&Vault) -> Result<String, String> + Send + 'static,
) {
    let Some(account) = account_id(win) else {
        return;
    };
    let v = v.clone();
    spawn(
        win,
        busy,
        move || {
            let status = f(&v)?;
            Ok((gather(&v, account)?, status))
        },
        |win, (snap, status)| apply(win, snap, status),
    );
}

fn blocked(win: &MainWindow) -> bool {
    win.global::<MandatesState>().get_busy()
}

// ---- wiring ------------------------------------------------------------------------------------

pub fn wire(win: &MainWindow, slot: VaultSlot) {
    SLOT.with(|s| *s.borrow_mut() = Some(slot));
    win.global::<MandatesState>().set_scheme_names(names(
        view::SCHEME_NAMES.iter().map(|s| s.to_string()).collect(),
    ));
    wire_open(win);
    wire_mandates(win);
    wire_import_export(win);
    wire_collection(win);
    wire_returns(win);
    wire_batch(win);
}

fn wire_open(win: &MainWindow) {
    let gs = win.global::<MandatesState>();

    let weak = win.as_weak();
    gs.on_request(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_now()) else {
            return;
        };
        if privacy_on(&win) {
            return;
        }
        let gs = win.global::<MandatesState>();
        gs.set_status(SharedString::new());
        gs.set_tab(0);
        gs.set_open(true);
        gs.set_busy(true);
        let weak = win.as_weak();
        std::thread::spawn(move || {
            let accounts = v.list_accounts().map_err(|e| e.to_string());
            let _ = weak.upgrade_in_event_loop(move |win| {
                win.global::<MandatesState>().set_busy(false);
                match accounts {
                    Ok(list) if !list.is_empty() => {
                        let labels = list
                            .iter()
                            .map(|a| {
                                if a.iban.is_empty() {
                                    a.name.clone()
                                } else {
                                    format!("{} ({})", a.name, a.iban)
                                }
                            })
                            .collect();
                        with_ui(|u| u.accounts = list.iter().map(|a| a.id).collect());
                        let gs = win.global::<MandatesState>();
                        gs.set_acc_names(names(labels));
                        gs.set_acc(0);
                        if let Some(v) = vault_now() {
                            load(&win, &v, String::new());
                        }
                    }
                    Ok(_) => set_status(&win, "Kein Konto vorhanden."),
                    Err(e) => set_status(&win, e),
                }
            });
        });
    });

    let weak = win.as_weak();
    gs.on_account_changed(move |_| {
        if let (Some(win), Some(v)) = (weak.upgrade(), vault_now()) {
            load(&win, &v, String::new());
        }
    });

    let weak = win.as_weak();
    gs.on_tab_changed(move |_| {
        if let Some(win) = weak.upgrade() {
            set_status(&win, SharedString::new());
        }
    });

    let weak = win.as_weak();
    gs.on_creditor_save(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_now()) else {
            return;
        };
        let Some(account) = account_id(&win) else {
            return;
        };
        if blocked(&win) {
            return;
        }
        let id = win.global::<MandatesState>().get_creditor_id().to_string();
        spawn(
            &win,
            "Wird gespeichert …",
            move || {
                v.set_creditor_id(account, id).map_err(|e| e.to_string())?;
                v.creditor_id(account).map_err(|e| e.to_string())
            },
            |win, stored| {
                let gs = win.global::<MandatesState>();
                gs.set_creditor_id(stored.into());
                gs.set_status("Gläubiger-ID gespeichert.".into());
            },
        );
    });

    let weak = win.as_weak();
    gs.on_creditor_suggest(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_now()) else {
            return;
        };
        if blocked(&win) {
            return;
        }
        spawn(
            &win,
            "Wird ermittelt …",
            move || v.suggested_creditor_id().map_err(|e| e.to_string()),
            |win, id| {
                let gs = win.global::<MandatesState>();
                if id.is_empty() {
                    gs.set_status("Kein Vorschlag vorhanden.".into());
                } else {
                    gs.set_creditor_id(id.into());
                    gs.set_status("Vorschlag eingetragen – mit „Speichern“ übernehmen.".into());
                }
            },
        );
    });
}

fn wire_mandates(win: &MainWindow) {
    let gs = win.global::<MandatesState>();

    let weak = win.as_weak();
    gs.on_mandate_select(move |i| {
        let Some(win) = weak.upgrade() else { return };
        let Some(m) = usize::try_from(i)
            .ok()
            .and_then(|i| with_ui(|u| u.mandates.get(i).cloned()))
        else {
            return;
        };
        let gs = win.global::<MandatesState>();
        gs.set_sel_mandate(i);
        gs.set_delete_armed(false);
        with_ui(|u| u.armed_delete = None);
        set_form(
            &win,
            &view::form_of(&m, &german_date),
            view::sequence_text(m.one_off, m.last_collected.as_deref()),
        );
    });

    let weak = win.as_weak();
    gs.on_mandate_new(move || {
        if let Some(win) = weak.upgrade() {
            clear_selection(&win);
        }
    });

    let weak = win.as_weak();
    gs.on_form_changed(move || {
        let Some(win) = weak.upgrade() else { return };
        let gs = win.global::<MandatesState>();
        let last = usize::try_from(gs.get_sel_mandate())
            .ok()
            .and_then(|i| with_ui(|u| u.mandates.get(i).and_then(|m| m.last_collected.clone())));
        gs.set_f_next(view::sequence_text(gs.get_f_oneoff(), last.as_deref()).into());
    });

    let weak = win.as_weak();
    gs.on_mandate_save(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_now()) else {
            return;
        };
        let Some(account) = account_id(&win) else {
            return;
        };
        if blocked(&win) {
            return;
        }
        let input = match view::form_input(account, &read_form(&win)) {
            Ok(i) => i,
            Err(e) => return set_status(&win, e),
        };
        let id = usize::try_from(win.global::<MandatesState>().get_sel_mandate())
            .ok()
            .and_then(|i| with_ui(|u| u.mandates.get(i).map(|m| m.id)));
        job(&win, &v, "Wird gespeichert …", move |v| {
            let m = v.save_mandate(id, input).map_err(|e| e.to_string())?;
            Ok(format!("Mandat „{}“ gespeichert.", m.reference))
        });
    });

    let weak = win.as_weak();
    gs.on_mandate_delete(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_now()) else {
            return;
        };
        if blocked(&win) {
            return;
        }
        let Some(id) = usize::try_from(win.global::<MandatesState>().get_sel_mandate())
            .ok()
            .and_then(|i| with_ui(|u| u.mandates.get(i).map(|m| m.id)))
        else {
            return;
        };
        if with_ui(|u| u.armed_delete) != Some(id) {
            with_ui(|u| u.armed_delete = Some(id));
            win.global::<MandatesState>().set_delete_armed(true);
            set_status(
                &win,
                "Zum Löschen erneut klicken. Bereits eingereichte Einzüge bleiben in den Umsätzen.",
            );
            return;
        }
        job(&win, &v, "Wird gelöscht …", move |v| {
            v.delete_mandate(id).map_err(|e| e.to_string())?;
            Ok("Mandat gelöscht.".to_string())
        });
    });
}

fn wire_import_export(win: &MainWindow) {
    let gs = win.global::<MandatesState>();

    let weak = win.as_weak();
    gs.on_csv_import(move || {
        let Some(win) = weak.upgrade() else { return };
        if blocked(&win) {
            return;
        }
        spawn(
            &win,
            "Datei wird gelesen …",
            || {
                let path = pick_file("Mandate importieren", "CSV", &["csv", "txt"])
                    .ok_or("Kein Import gewählt.")?;
                Ok(parse_mandate_csv(read_text(&path)?))
            },
            |win, imp| {
                let summary = view::csv_summary(&imp.rows, &imp.problems);
                let lines: Vec<Line> = imp.rows.iter().map(|r| view::csv_line(r, &eur)).collect();
                let ready = imp.rows.iter().any(|r| r.errors.is_empty());
                with_ui(|u| u.csv = imp.rows);
                let gs = win.global::<MandatesState>();
                gs.set_csv_rows(model(lines));
                gs.set_csv_summary(summary.into());
                gs.set_csv_ready(ready);
                gs.set_status(SharedString::new());
            },
        );
    });

    let weak = win.as_weak();
    gs.on_csv_apply(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_now()) else {
            return;
        };
        let Some(account) = account_id(&win) else {
            return;
        };
        if blocked(&win) {
            return;
        }
        let rows = with_ui(|u| u.csv.clone());
        job(&win, &v, "Mandate werden importiert …", move |v| {
            let r = v
                .import_mandate_rows(account, rows)
                .map_err(|e| e.to_string())?;
            Ok(view::import_summary(&r))
        });
    });

    let weak = win.as_weak();
    gs.on_csv_edit(move |i| {
        let Some(win) = weak.upgrade() else { return };
        let Some(row) = usize::try_from(i)
            .ok()
            .and_then(|i| with_ui(|u| u.csv.get(i).cloned()))
        else {
            return;
        };
        let gs = win.global::<MandatesState>();
        gs.set_sel_mandate(-1);
        set_form(
            &win,
            &view::form_of_csv(&row, &german_date),
            view::sequence_text(row.one_off, None),
        );
        gs.set_status("Zeile in das Formular geladen – korrigieren und speichern.".into());
    });

    let weak = win.as_weak();
    gs.on_csv_export(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_now()) else {
            return;
        };
        let Some(account) = account_id(&win) else {
            return;
        };
        if blocked(&win) {
            return;
        }
        spawn(
            &win,
            "Wird exportiert …",
            move || {
                let text = v.export_mandates_csv(account).map_err(|e| e.to_string())?;
                let path = save_file("Mandate exportieren", "mandate.csv")
                    .ok_or("Kein Export gewählt.")?;
                std::fs::write(&path, text)
                    .map_err(|e| format!("Die Datei konnte nicht geschrieben werden: {e}"))?;
                Ok(path)
            },
            |win, path| set_status(win, format!("Mandate exportiert: {}", path.display())),
        );
    });

    let weak = win.as_weak();
    gs.on_pain_import(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_now()) else {
            return;
        };
        let Some(account) = account_id(&win) else {
            return;
        };
        if blocked(&win) {
            return;
        }
        job(&win, &v, "Datei wird importiert …", move |v| {
            let path = pick_file("pain.008 importieren", "pain.008", &["xml"])
                .ok_or("Kein Import gewählt.")?;
            let r = v
                .import_pain008(account, read_text(&path)?)
                .map_err(|e| e.to_string())?;
            Ok(view::import_summary(&r))
        });
    });
}

/// Writes the editor fields (amount, purpose, last) of the selected mandate into
/// its collection pick and redraws the list. Nothing selected: nothing to commit.
fn commit_coll_edit(win: &MainWindow) -> Result<(), SharedString> {
    let gs = win.global::<MandatesState>();
    let Ok(i) = usize::try_from(gs.get_sel_coll()) else {
        return Ok(());
    };
    let cents = view::parse_amount(gs.get_c_amount().as_str())?;
    let purpose = gs.get_c_purpose().to_string();
    let last = gs.get_c_last();
    with_ui(|u| {
        if let Some(p) = u.picks.get_mut(i) {
            p.amount_cents = cents;
            p.purpose = purpose;
            p.last = last;
        }
    });
    render_picks(win);
    Ok(())
}

fn wire_collection(win: &MainWindow) {
    let gs = win.global::<MandatesState>();

    let weak = win.as_weak();
    gs.on_coll_toggle(move |i| {
        let Some(win) = weak.upgrade() else { return };
        if let Ok(i) = usize::try_from(i) {
            with_ui(|u| {
                if let Some(p) = u.picks.get_mut(i) {
                    p.checked = !p.checked;
                }
            });
        }
        render_picks(&win);
    });

    let weak = win.as_weak();
    gs.on_coll_select(move |i| {
        let Some(win) = weak.upgrade() else { return };
        let Some(p) = usize::try_from(i)
            .ok()
            .and_then(|i| with_ui(|u| u.picks.get(i).cloned()))
        else {
            return;
        };
        let gs = win.global::<MandatesState>();
        gs.set_sel_coll(i);
        gs.set_c_amount(if p.amount_cents > 0 {
            format!("{:.2}", p.amount_cents as f64 / 100.0)
                .replace('.', ",")
                .into()
        } else {
            SharedString::new()
        });
        gs.set_c_purpose(p.purpose.into());
        gs.set_c_last(p.last);
    });

    let weak = win.as_weak();
    gs.on_coll_edit_apply(move || {
        let Some(win) = weak.upgrade() else { return };
        match commit_coll_edit(&win) {
            Ok(()) => set_status(&win, SharedString::new()),
            Err(e) => set_status(&win, e),
        }
    });

    let weak = win.as_weak();
    gs.on_coll_all(move |on| {
        let Some(win) = weak.upgrade() else { return };
        let mandates: Vec<bool> = with_ui(|u| u.mandates.iter().map(|m| m.active).collect());
        with_ui(|u| {
            for (p, active) in u.picks.iter_mut().zip(mandates) {
                p.checked = on && active;
            }
        });
        render_picks(&win);
    });

    let weak = win.as_weak();
    gs.on_coll_build(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_now()) else {
            return;
        };
        let Some(account) = account_id(&win) else {
            return;
        };
        if blocked(&win) {
            return;
        }
        // The amount, purpose and "last" box of the selected mandate may still hold
        // typed values that "Übernehmen" has not committed: the file must contain
        // what the editor shows, not the previous amount.
        if let Err(e) = commit_coll_edit(&win) {
            return set_status(&win, e);
        }
        let due = match view::parse_date(win.global::<MandatesState>().get_c_date().as_str()) {
            Ok(d) => d,
            Err(e) => return set_status(&win, e),
        };
        let items = match view::collection_items(&with_ui(|u| u.picks.clone())) {
            Ok(i) => i,
            Err(e) => return set_status(&win, e),
        };
        let due_saved = due.clone();
        spawn(
            &win,
            "Dateien werden erzeugt …",
            move || {
                let files = v
                    .build_collection_files(account, items, due)
                    .map_err(|e| e.to_string())?;
                let dir =
                    pick_dir("Ordner für die Lastschrift-Dateien").ok_or("Kein Ordner gewählt.")?;
                for f in &files {
                    let path = dir.join(view::safe_file_name(&f.file_name));
                    std::fs::write(&path, &f.xml)
                        .map_err(|e| format!("Die Datei konnte nicht geschrieben werden: {e}"))?;
                }
                Ok(files)
            },
            move |win, files| {
                with_ui(|u| {
                    u.files = files;
                    u.due = due_saved;
                });
                render_files(win);
                set_status(win, SharedString::new());
            },
        );
    });

    let weak = win.as_weak();
    gs.on_coll_mark(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_now()) else {
            return;
        };
        if blocked(&win) {
            return;
        }
        let (ids, due) = with_ui(|u| {
            (
                u.files
                    .iter()
                    .flat_map(|f| f.mandate_ids.clone())
                    .collect::<Vec<_>>(),
                u.due.clone(),
            )
        });
        if ids.is_empty() {
            return;
        }
        let n = ids.len();
        job(&win, &v, "Wird gespeichert …", move |v| {
            v.mark_collected(ids, due).map_err(|e| e.to_string())?;
            Ok(format!("{n} Mandate als eingezogen markiert."))
        });
    });
}

fn wire_returns(win: &MainWindow) {
    let gs = win.global::<MandatesState>();

    let weak = win.as_weak();
    gs.on_return_select(move |i| {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_now()) else {
            return;
        };
        let Some(id) = usize::try_from(i)
            .ok()
            .and_then(|i| with_ui(|u| u.returns.get(i).map(|b| b.mandate_id)))
        else {
            return;
        };
        win.global::<MandatesState>().set_sel_return(i);
        let name = with_ui(|u| {
            u.mandates
                .iter()
                .find(|m| m.id == id)
                .map(|m| m.debtor_name.clone())
                .unwrap_or_default()
        });
        spawn(
            &win,
            "Wird geladen …",
            move || v.mandate_bookings(id).map_err(|e| e.to_string()),
            move |win, list| {
                let lines = list
                    .iter()
                    .map(|b| view::booking_line(b, &name, &eur, &german_date))
                    .collect();
                let gs = win.global::<MandatesState>();
                gs.set_history(model(lines));
                gs.set_status(SharedString::new());
            },
        );
    });

    let weak = win.as_weak();
    gs.on_return_ack(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_now()) else {
            return;
        };
        if blocked(&win) {
            return;
        }
        let Some(id) = usize::try_from(win.global::<MandatesState>().get_sel_return())
            .ok()
            .and_then(|i| with_ui(|u| u.returns.get(i).map(|b| b.mandate_id)))
        else {
            return;
        };
        job(&win, &v, "Wird gespeichert …", move |v| {
            v.acknowledge_mandate_returns(id)
                .map_err(|e| e.to_string())?;
            Ok("Rücklastschriften als gesehen markiert.".to_string())
        });
    });

    let weak = win.as_weak();
    gs.on_returns_refresh(move || {
        if let (Some(win), Some(v)) = (weak.upgrade(), vault_now())
            && !blocked(&win)
        {
            load(&win, &v, String::new());
        }
    });
}

fn wire_batch(win: &MainWindow) {
    let weak = win.as_weak();
    win.global::<MandatesState>().on_batch_import(move || {
        let Some(win) = weak.upgrade() else { return };
        if blocked(&win) {
            return;
        }
        spawn(
            &win,
            "Datei wird gelesen …",
            || {
                let path = pick_file("Sammelüberweisung (CSV)", "CSV", &["csv", "txt"])
                    .ok_or("Keine Datei gewählt.")?;
                Ok(parse_batch_csv(read_text(&path)?))
            },
            |win, imp| {
                let gs = win.global::<MandatesState>();
                gs.set_batch_summary(view::batch_summary(&imp, &eur).into());
                gs.set_batch_rows(model(
                    imp.rows.iter().map(|r| view::batch_line(r, &eur)).collect(),
                ));
                gs.set_status(SharedString::new());
            },
        );
    });
}
