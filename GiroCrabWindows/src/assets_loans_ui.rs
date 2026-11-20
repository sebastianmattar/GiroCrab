//! Slint wiring for Sachwerte, the classification of positions and manual loans. The dialog lives
//! in `ui/assets_loans_dialog.slint`, the pure logic in `assets_loans.rs`. Every vault call runs
//! on a worker thread; results come back through `upgrade_in_event_loop`.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use girocrab_core::asset_classification::ClassifiedAsset;
use girocrab_core::asset_classification::{
    AssetClassification, ClassificationSource, ClassificationValues, classify_asset,
    normalized_isin,
};
use girocrab_core::assets::PRICE_SYMBOLS;
use girocrab_core::assets::{AssetItem, AssetWeightPreset, MANUAL_SOURCE, asset_weight_presets};
use girocrab_core::loans::{LoanLink, ManualLoan};
use girocrab_core::{Account, AccountKind, Vault};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::assets_loans::{self as al, ItemForm, LoanForm};
use crate::planning::{euro, format_plain};
use crate::portfolio::class_label;
use crate::{AlPoint, AlRow, AssetsState, MainWindow, privacy_on, with_state};

type VaultSlot = Arc<Mutex<Option<Arc<Vault>>>>;

/// Dialog state on the UI thread.
#[derive(Default)]
struct Ui {
    accounts: Vec<Account>,
    /// Tangible accounts in the order of the account menu.
    tangible: Vec<i64>,
    items: Vec<AssetItem>,
    all_items: usize,
    all_value_cents: i64,
    loans: Vec<ManualLoan>,
    links: Vec<LoanLink>,
    presets: Vec<AssetWeightPreset>,
    /// Accounts of the classification menu (brokerage + tangible).
    class_accounts: Vec<i64>,
    classified: Vec<ClassifiedAsset>,
    armed_item: Option<i64>,
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

fn model<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(items))
}

fn names(items: impl IntoIterator<Item = String>) -> ModelRc<SharedString> {
    model(items.into_iter().map(SharedString::from).collect())
}

/// Closes the dialog and forgets its state when the vault locks.
pub fn on_lock(win: &MainWindow) {
    win.global::<AssetsState>().set_open(false);
    with_ui(|u| *u = Ui::default());
}

/// Price sources of the item form: manual first, then the automatic symbols.
fn sources() -> Vec<String> {
    std::iter::once(MANUAL_SOURCE.to_string())
        .chain(PRICE_SYMBOLS.iter().map(|s| (*s).to_string()))
        .collect()
}

fn source_label(source: &str) -> String {
    match source {
        MANUAL_SOURCE => "Manuell".into(),
        "XAU" => "Gold (XAU)".into(),
        other => other.to_string(),
    }
}

/// What a job leaves selected afterwards.
#[derive(Default, Clone)]
struct Sel {
    account: Option<i64>,
    item: Option<i64>,
    loan: Option<i64>,
}

fn selected_account_id(win: &MainWindow) -> Option<i64> {
    let i = usize::try_from(win.global::<AssetsState>().get_acc()).ok()?;
    with_ui(|u| u.tangible.get(i).copied())
}

fn selected_item(win: &MainWindow) -> Option<AssetItem> {
    let i = usize::try_from(win.global::<AssetsState>().get_sel_item()).ok()?;
    with_ui(|u| u.items.get(i).cloned())
}

fn selected_loan(win: &MainWindow) -> Option<ManualLoan> {
    let i = usize::try_from(win.global::<AssetsState>().get_sel_loan()).ok()?;
    with_ui(|u| u.loans.get(i).cloned())
}

fn selected_class(win: &MainWindow) -> Option<ClassifiedAsset> {
    let i = usize::try_from(win.global::<AssetsState>().get_sel_class()).ok()?;
    with_ui(|u| u.classified.get(i).cloned())
}

fn current_sel(win: &MainWindow) -> Sel {
    Sel {
        account: selected_account_id(win),
        item: selected_item(win).map(|i| i.id),
        loan: selected_loan(win).map(|l| l.account_id),
    }
}

fn fail(win: &MainWindow, text: impl Into<SharedString>) {
    win.global::<AssetsState>().set_status(text.into());
}

// -------------------------------------------------------------------------------- loading

struct Data {
    accounts: Vec<Account>,
    tangible: Vec<i64>,
    account: Option<i64>,
    items: Vec<AssetItem>,
    all_items: usize,
    all_value_cents: i64,
    loans: Vec<ManualLoan>,
    links: Vec<LoanLink>,
}

fn fetch(v: &Vault, account: Option<i64>) -> Result<Data, String> {
    let e = |e: girocrab_core::CoreError| e.to_string();
    let accounts = v.list_accounts().map_err(e)?;
    let tangible = v.list_tangible_account_ids().map_err(e)?;
    let account = account
        .filter(|a| tangible.contains(a))
        .or_else(|| tangible.first().copied());
    let items = match account {
        Some(a) => v.list_asset_items(a).map_err(e)?,
        None => vec![],
    };
    let all = v.list_all_asset_items().map_err(e)?;
    Ok(Data {
        all_items: all.iter().filter(|i| !i.sold).count(),
        all_value_cents: all.iter().map(|i| i.holding.value_cents).sum(),
        accounts,
        tangible,
        account,
        items,
        loans: v.list_manual_loans().map_err(e)?,
        links: v.list_loan_links().map_err(e)?,
    })
}

/// Loads everything on a worker thread and shows it.
fn load(win: &MainWindow, v: &Arc<Vault>, sel: Sel, status: String) {
    let (weak, v) = (win.as_weak(), v.clone());
    std::thread::spawn(move || {
        let data = fetch(&v, sel.account);
        let presets = asset_weight_presets();
        let _ = weak.upgrade_in_event_loop(move |win| match data {
            Ok(d) => {
                render(&win, d, presets, &sel);
                win.global::<AssetsState>().set_status(status.into());
            }
            Err(e) => fail(&win, e),
        });
    });
}

fn render(win: &MainWindow, d: Data, presets: Vec<AssetWeightPreset>, sel: &Sel) {
    let st = win.global::<AssetsState>();
    let acc_names: Vec<String> = d
        .tangible
        .iter()
        .map(|id| {
            d.accounts
                .iter()
                .find(|a| a.id == *id)
                .map_or_else(|| format!("Konto {id}"), |a| a.name.clone())
        })
        .collect();
    let acc_index = d
        .account
        .and_then(|id| d.tangible.iter().position(|t| *t == id));
    let item_rows: Vec<AlRow> = d
        .items
        .iter()
        .map(|i| AlRow {
            name: i.setup.name.clone().into(),
            detail: al::item_detail(i).into(),
        })
        .collect();
    let loan_rows: Vec<AlRow> = d
        .loans
        .iter()
        .map(|l| AlRow {
            name: l.setup.name.clone().into(),
            detail: al::loan_detail(l).into(),
        })
        .collect();
    let item_index = sel
        .item
        .and_then(|id| d.items.iter().position(|i| i.id == id));
    let loan_index = sel
        .loan
        .and_then(|id| d.loans.iter().position(|l| l.account_id == id));
    let class_accounts: Vec<i64> = d
        .accounts
        .iter()
        .filter(|a| {
            a.archived_unix.is_none()
                && (a.kind == AccountKind::Brokerage || d.tangible.contains(&a.id))
        })
        .map(|a| a.id)
        .collect();
    let class_names: Vec<String> = class_accounts
        .iter()
        .filter_map(|id| d.accounts.iter().find(|a| a.id == *id))
        .map(|a| a.name.clone())
        .collect();
    let summary = if d.all_items == 0 {
        String::new()
    } else {
        format!(
            "{} Sachwerte in allen Konten, Gesamtwert {}",
            d.all_items,
            euro(d.all_value_cents)
        )
    };
    with_ui(|u| {
        u.accounts = d.accounts;
        u.tangible = d.tangible;
        u.items = d.items;
        u.all_items = d.all_items;
        u.all_value_cents = d.all_value_cents;
        u.loans = d.loans;
        u.links = d.links;
        u.presets = presets;
        u.armed_item = None;
        u.class_accounts = class_accounts;
    });
    st.set_acc_names(names(acc_names));
    st.set_acc(acc_index.map_or(0, |i| i as i32));
    st.set_items(model(item_rows));
    st.set_items_summary(summary.into());
    st.set_item_armed(false);
    st.set_i_source_names(names(sources().iter().map(|s| source_label(s))));
    st.set_i_presets(names(std::iter::once("Eigenes Gewicht".to_string()).chain(
        with_ui(|u| {
            u.presets
                .iter()
                .map(|p| p.label.clone())
                .collect::<Vec<_>>()
        }),
    )));
    st.set_loans(model(loan_rows));
    st.set_c_account_names(names(class_names));
    st.set_c_class_names(names(al::class_menu()));
    st.set_c_instr_names(names(al::instrument_menu()));
    match item_index {
        Some(i) => fill_item(win, i),
        None => new_item_form(win),
    }
    match loan_index {
        Some(i) => fill_loan(win, i),
        None => new_loan_form(win),
    }
    refresh_booking(win);
    if st.get_c_account() as usize >= with_ui(|u| u.class_accounts.len()) {
        st.set_c_account(0);
    }
    if st.get_mode() == 2 || with_ui(|u| !u.classified.is_empty()) {
        load_classified(win);
    }
}

// -------------------------------------------------------------------------------- Sachwerte

fn new_item_form(win: &MainWindow) {
    let st = win.global::<AssetsState>();
    st.set_sel_item(-1);
    apply_item_form(win, &ItemForm::default());
    st.set_i_source(0);
    st.set_i_manual(true);
    st.set_i_qty("1".into());
    st.set_item_info(SharedString::new());
    st.set_history_info(SharedString::new());
    st.set_price_info(SharedString::new());
    st.set_quantities(model(vec![]));
    st.set_values(model(vec![]));
    st.set_item_armed(false);
}

fn apply_item_form(win: &MainWindow, f: &ItemForm) {
    let st = win.global::<AssetsState>();
    st.set_i_name(f.name.clone().into());
    st.set_i_weight(f.weight.clone().into());
    st.set_i_purity(f.purity.clone().into());
    st.set_i_linear(f.linear);
    st.set_i_date(f.date.clone().into());
    st.set_i_qty(f.quantity.clone().into());
    st.set_i_price(f.price.clone().into());
    st.set_i_start(f.start_value.clone().into());
    st.set_i_note(f.note.clone().into());
    st.set_i_symbol(f.symbol.clone().into());
    st.set_i_preset(0);
}

fn read_item_form(win: &MainWindow) -> ItemForm {
    let st = win.global::<AssetsState>();
    let source = usize::try_from(st.get_i_source())
        .ok()
        .and_then(|i| sources().get(i).cloned())
        .unwrap_or_else(|| MANUAL_SOURCE.to_string());
    ItemForm {
        name: st.get_i_name().into(),
        source,
        weight: st.get_i_weight().into(),
        purity: st.get_i_purity().into(),
        linear: st.get_i_linear(),
        date: st.get_i_date().into(),
        quantity: st.get_i_qty().into(),
        price: st.get_i_price().into(),
        start_value: st.get_i_start().into(),
        note: st.get_i_note().into(),
        symbol: st.get_i_symbol().into(),
    }
}

fn fill_item(win: &MainWindow, index: usize) {
    let Some(it) = with_ui(|u| u.items.get(index).cloned()) else {
        return;
    };
    let st = win.global::<AssetsState>();
    st.set_sel_item(index as i32);
    apply_item_form(win, &al::item_form(&it.setup));
    let manual = it.setup.price_source == MANUAL_SOURCE;
    st.set_i_manual(manual);
    st.set_i_source(
        sources()
            .iter()
            .position(|s| *s == it.setup.price_source)
            .map_or(0, |i| i as i32),
    );
    st.set_quantities(model(
        it.quantities
            .iter()
            .map(|q| AlPoint {
                date: q.date.clone().into(),
                text: format!("{} Stück", al::format_number(q.quantity)).into(),
            })
            .collect(),
    ));
    st.set_values(model(
        it.values
            .iter()
            .map(|v| AlPoint {
                date: v.date.clone().into(),
                text: euro(v.value_cents).into(),
            })
            .collect(),
    ));
    st.set_item_info(
        format!(
            "{} · {} · Wert {}{}",
            source_label(&it.setup.price_source),
            al::item_detail(&it),
            euro(it.holding.value_cents),
            if it.setup.note.is_empty() {
                String::new()
            } else {
                format!("\n{}", it.setup.note)
            }
        )
        .into(),
    );
    st.set_history_info("Verlauf wird geladen …".into());
    st.set_price_info(SharedString::new());
    st.set_item_armed(false);
    with_ui(|u| u.armed_item = None);
    load_item_extras(win, &it);
}

/// History and today's price of the selected item, off the UI thread.
fn load_item_extras(win: &MainWindow, it: &AssetItem) {
    let Some(v) = vault_now() else { return };
    let (weak, id, source) = (win.as_weak(), it.id, it.setup.price_source.clone());
    std::thread::spawn(move || {
        let history = v
            .asset_item_history(id)
            .map(|h| {
                al::history_summary(
                    &h.into_iter()
                        .map(|d| (d.date, d.balance_cents))
                        .collect::<Vec<_>>(),
                )
            })
            .unwrap_or_else(|e| e.to_string());
        let price = if source == MANUAL_SOURCE {
            String::new()
        } else {
            match v.latest_asset_price(source) {
                Ok(Some(p)) => format!(
                    "Aktueller Kurs {} je Feinunze ({})",
                    euro(p.value_cents),
                    p.date
                ),
                Ok(None) => "Noch kein Kurs gespeichert.".into(),
                Err(e) => e.to_string(),
            }
        };
        let _ = weak.upgrade_in_event_loop(move |win| {
            // The selection may have moved on while the vault answered.
            if selected_item(&win).map(|i| i.id) == Some(id) {
                let st = win.global::<AssetsState>();
                st.set_history_info(history.into());
                st.set_price_info(price.into());
            }
        });
    });
}

// ---------------------------------------------------------------------------------- Darlehen

fn new_loan_form(win: &MainWindow) {
    let st = win.global::<AssetsState>();
    st.set_sel_loan(-1);
    apply_loan_form(win, &LoanForm::default());
    st.set_l_status(SharedString::new());
    st.set_l_preview(SharedString::new());
    st.set_l_matches(names([]));
    st.set_link_info(link_info(None).into());
}

fn apply_loan_form(win: &MainWindow, f: &LoanForm) {
    let st = win.global::<AssetsState>();
    st.set_l_name(f.name.clone().into());
    st.set_l_original(f.original.clone().into());
    st.set_l_anchor_date(f.anchor_date.clone().into());
    st.set_l_anchor_balance(f.anchor_balance.clone().into());
    st.set_l_rate(f.rate.clone().into());
    st.set_l_expr(f.expr.clone().into());
}

fn read_loan_form(win: &MainWindow) -> LoanForm {
    let st = win.global::<AssetsState>();
    LoanForm {
        name: st.get_l_name().into(),
        original: st.get_l_original().into(),
        anchor_date: st.get_l_anchor_date().into(),
        anchor_balance: st.get_l_anchor_balance().into(),
        rate: st.get_l_rate().into(),
        expr: st.get_l_expr().into(),
    }
}

fn fill_loan(win: &MainWindow, index: usize) {
    let Some(l) = with_ui(|u| u.loans.get(index).cloned()) else {
        return;
    };
    let st = win.global::<AssetsState>();
    st.set_sel_loan(index as i32);
    apply_loan_form(win, &al::loan_form(&l.setup));
    st.set_l_status(al::status_text(&l.status).into());
    st.set_l_preview(SharedString::new());
    st.set_l_matches(names([]));
    st.set_link_info(link_info(Some(l.account_id)).into());
    refresh_booking(win);
}

fn link_info(loan: Option<i64>) -> String {
    with_ui(|u| al::link_summary(&u.links, loan))
}

/// The booking selected in the main window and whether it is linked to a loan.
fn refresh_booking(win: &MainWindow) {
    let st = win.global::<AssetsState>();
    let booking = with_state(|s| {
        let id = s.selected_booking?;
        let r = s.rows.iter().find(|r| r.id == id)?;
        let who = if r.counterparty.trim().is_empty() {
            r.purpose.trim().to_string()
        } else {
            r.counterparty.trim().to_string()
        };
        Some((id, format!("{who} · {}", format_plain(r.amount_cents))))
    });
    let linked = booking
        .as_ref()
        .is_some_and(|(id, _)| with_ui(|u| u.links.iter().any(|l| l.transaction_id == *id)));
    st.set_has_booking(booking.is_some());
    st.set_booking_linked(linked);
    st.set_booking_info(booking.map(|b| b.1).unwrap_or_default().into());
}

// ----------------------------------------------------------------------------- Klassifizierung

fn selected_class_account(win: &MainWindow) -> Option<i64> {
    let i = usize::try_from(win.global::<AssetsState>().get_c_account()).ok()?;
    with_ui(|u| u.class_accounts.get(i).copied())
}

fn load_classified(win: &MainWindow) {
    let (Some(v), Some(account)) = (vault_now(), selected_class_account(win)) else {
        win.global::<AssetsState>().set_c_rows(model(vec![]));
        return;
    };
    let weak = win.as_weak();
    std::thread::spawn(move || {
        let data = v.classified_assets(account).map_err(|e| e.to_string());
        let _ = weak.upgrade_in_event_loop(move |win| {
            if selected_class_account(&win) != Some(account) {
                return;
            }
            match data {
                Ok(rows) => render_classified(&win, rows),
                Err(e) => fail(&win, e),
            }
        });
    });
}

fn render_classified(win: &MainWindow, rows: Vec<ClassifiedAsset>) {
    let st = win.global::<AssetsState>();
    let list: Vec<AlRow> = rows
        .iter()
        .map(|r| AlRow {
            name: r.holding.name.clone().into(),
            detail: class_line(&r.classification).into(),
        })
        .collect();
    with_ui(|u| u.classified = rows);
    st.set_c_rows(model(list));
    st.set_sel_class(-1);
    st.set_c_preview(SharedString::new());
}

fn class_line(c: &AssetClassification) -> String {
    format!(
        "{} · {}",
        class_label(c.asset_class),
        al::instrument_label(c.instrument)
    )
}

fn source_text(s: ClassificationSource) -> &'static str {
    match s {
        ClassificationSource::User => "von dir festgelegt",
        ClassificationSource::Import => "aus dem Import",
        ClassificationSource::Rule => "automatisch",
        ClassificationSource::Unknown => "unbekannt",
    }
}

fn fill_class(win: &MainWindow, index: usize) {
    let Some(r) = with_ui(|u| u.classified.get(index).cloned()) else {
        return;
    };
    let st = win.global::<AssetsState>();
    st.set_sel_class(index as i32);
    let isin = match normalized_isin(&r.holding.isin) {
        Some(i) => format!("ISIN {i}"),
        None if r.holding.isin.trim().is_empty() => "keine ISIN".to_string(),
        None => format!("ISIN {} ist ungültig", r.holding.isin.trim()),
    };
    let c = &r.classification;
    st.set_c_info(
        format!(
            "{} · {}\nAnlageklasse: {} ({})\nInstrument: {} ({}){}",
            r.holding.name,
            isin,
            class_label(c.asset_class),
            source_text(c.asset_class_source),
            al::instrument_label(c.instrument),
            source_text(c.instrument_source),
            if c.security_key.is_none() {
                "\nOhne stabilen Schlüssel – eine eigene Zuordnung lässt sich nicht speichern."
            } else {
                ""
            }
        )
        .into(),
    );
    // A user choice shows as selected; everything else stays on "Automatisch".
    let class_idx = if c.asset_class_source == ClassificationSource::User {
        al::CLASS_KEYS
            .iter()
            .position(|k| *k == c.asset_class.as_str())
            .unwrap_or(0)
    } else {
        0
    };
    let instr_idx = if c.instrument_source == ClassificationSource::User {
        al::INSTRUMENT_KEYS
            .iter()
            .position(|k| *k == c.instrument.as_str())
            .unwrap_or(0)
    } else {
        0
    };
    st.set_c_class(class_idx as i32);
    st.set_c_instr(instr_idx as i32);
    st.set_c_preview(SharedString::new());
}

// -------------------------------------------------------------------------------------- jobs

/// Runs a change on a worker thread, then reloads the dialog and the main window.
fn job<F>(win: &MainWindow, v: &Arc<Vault>, done: &str, f: F)
where
    F: FnOnce(&Vault) -> Result<Sel, String> + Send + 'static,
{
    let st = win.global::<AssetsState>();
    st.set_status("Wird gespeichert …".into());
    st.set_busy(true);
    let (weak, v, done) = (win.as_weak(), v.clone(), done.to_string());
    let keep = current_sel(win);
    std::thread::spawn(move || {
        let result = f(&v);
        let _ = weak.upgrade_in_event_loop(move |win| {
            win.global::<AssetsState>().set_busy(false);
            match result {
                Ok(sel) => {
                    let sel = Sel {
                        account: sel.account.or(keep.account),
                        item: sel.item.or(keep.item),
                        loan: sel.loan.or(keep.loan),
                    };
                    load(&win, &v, sel, done);
                    let target = Some(win.get_selected() as i64).filter(|s| *s >= 0);
                    crate::show(&win, &v, target);
                }
                Err(e) => fail(&win, e),
            }
        });
    });
}

/// Common start of an edit: not in privacy mode, vault present.
fn guard(weak: &slint::Weak<MainWindow>) -> Option<(MainWindow, Arc<Vault>)> {
    let win = weak.upgrade()?;
    let v = vault_now()?;
    if privacy_on(&win) {
        return None;
    }
    Some((win, v))
}

fn core_err(e: girocrab_core::CoreError) -> String {
    e.to_string()
}

// ------------------------------------------------------------------------------------ wiring

pub fn wire(win: &MainWindow, slot: VaultSlot) {
    SLOT.with(|s| *s.borrow_mut() = Some(slot));
    wire_open_and_items(win);
    wire_item_points(win);
    wire_loans(win);
    wire_classification(win);
}

fn wire_open_and_items(win: &MainWindow) {
    let st = win.global::<AssetsState>();

    let weak = win.as_weak();
    st.on_request(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_now()) else {
            return;
        };
        if privacy_on(&win) {
            return;
        }
        let st = win.global::<AssetsState>();
        st.set_status(SharedString::new());
        st.set_open(true);
        load(&win, &v, Sel::default(), String::new());
    });

    let weak = win.as_weak();
    st.on_account_changed(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_now()) else {
            return;
        };
        let sel = Sel {
            account: selected_account_id(&win),
            ..Sel::default()
        };
        load(&win, &v, sel, String::new());
    });

    let weak = win.as_weak();
    st.on_account_create(move || {
        let Some((win, v)) = guard(&weak) else { return };
        let st = win.global::<AssetsState>();
        let name = st.get_new_account().trim().to_string();
        if name.is_empty() {
            return fail(&win, "Bitte einen Namen für das Konto eingeben.");
        }
        st.set_new_account(SharedString::new());
        job(&win, &v, "Konto angelegt.", move |v| {
            let id = v.create_tangible_account(name).map_err(core_err)?;
            Ok(Sel {
                account: Some(id),
                ..Sel::default()
            })
        });
    });

    let weak = win.as_weak();
    st.on_item_select(move |i| {
        if let (Some(win), Ok(i)) = (weak.upgrade(), usize::try_from(i)) {
            fill_item(&win, i);
        }
    });

    let weak = win.as_weak();
    st.on_item_new(move || {
        if let Some(win) = weak.upgrade() {
            new_item_form(&win);
        }
    });

    let weak = win.as_weak();
    st.on_source_changed(move || {
        let Some(win) = weak.upgrade() else { return };
        let manual = read_item_form(&win).source == MANUAL_SOURCE;
        win.global::<AssetsState>().set_i_manual(manual);
    });

    let weak = win.as_weak();
    st.on_preset_chosen(move || {
        let Some(win) = weak.upgrade() else { return };
        let st = win.global::<AssetsState>();
        // Index 0 is "Eigenes Gewicht"; a denomination fills the weight field.
        let grams = usize::try_from(st.get_i_preset())
            .ok()
            .and_then(|i| i.checked_sub(1))
            .and_then(|i| with_ui(|u| u.presets.get(i).map(|p| p.grams)));
        if let Some(g) = grams {
            st.set_i_weight(al::format_number(g).into());
        }
    });

    let weak = win.as_weak();
    st.on_item_save(move || {
        let Some((win, v)) = guard(&weak) else { return };
        let Some(account) = selected_account_id(&win) else {
            return fail(&win, "Bitte zuerst ein Sachwert-Konto anlegen.");
        };
        let setup = match al::build_item(&read_item_form(&win)) {
            Ok(s) => s,
            Err(e) => return fail(&win, e),
        };
        let item_id = selected_item(&win).map(|i| i.id);
        job(&win, &v, "Sachwert gespeichert.", move |v| {
            let id = v
                .save_asset_item(account, item_id, setup)
                .map_err(core_err)?;
            Ok(Sel {
                account: Some(account),
                item: Some(id),
                ..Sel::default()
            })
        });
    });

    let weak = win.as_weak();
    st.on_item_delete(move || {
        let Some((win, v)) = guard(&weak) else { return };
        let Some(item) = selected_item(&win) else {
            return;
        };
        // Second click deletes.
        if with_ui(|u| u.armed_item.replace(item.id)) != Some(item.id) {
            win.global::<AssetsState>().set_item_armed(true);
            return;
        }
        job(&win, &v, "Sachwert gelöscht.", move |v| {
            v.delete_asset_item(item.id).map_err(core_err)?;
            Ok(Sel {
                account: Some(item.account_id),
                ..Sel::default()
            })
        });
    });
}

fn wire_item_points(win: &MainWindow) {
    let st = win.global::<AssetsState>();

    let weak = win.as_weak();
    st.on_quantity_set(move || {
        let Some((win, v)) = guard(&weak) else { return };
        let Some(item) = selected_item(&win) else {
            return;
        };
        let st = win.global::<AssetsState>();
        let date = match al::require_date(&st.get_q_date(), "Datum") {
            Ok(d) => d,
            Err(e) => return fail(&win, e),
        };
        let Some(qty) = al::parse_number(&st.get_q_amount()) else {
            return fail(&win, "Anzahl: Bitte eine Zahl (0 oder größer) eingeben.");
        };
        job(&win, &v, "Bestand gespeichert.", move |v| {
            v.set_asset_quantity(item.id, date, qty).map_err(core_err)?;
            Ok(Sel::default())
        });
    });

    let weak = win.as_weak();
    st.on_quantity_remove(move |i| {
        let Some((win, v)) = guard(&weak) else { return };
        let Some(item) = selected_item(&win) else {
            return;
        };
        let Some(p) = usize::try_from(i).ok().and_then(|i| item.quantities.get(i)) else {
            return;
        };
        let date = p.date.clone();
        job(&win, &v, "Bestandsänderung entfernt.", move |v| {
            v.delete_asset_quantity(item.id, date).map_err(core_err)?;
            Ok(Sel::default())
        });
    });

    let weak = win.as_weak();
    st.on_value_set(move || {
        let Some((win, v)) = guard(&weak) else { return };
        let Some(item) = selected_item(&win) else {
            return;
        };
        let st = win.global::<AssetsState>();
        let date = match al::require_date(&st.get_v_date(), "Datum") {
            Ok(d) => d,
            Err(e) => return fail(&win, e),
        };
        let cents = match al::require_cents(&st.get_v_amount(), "Wert") {
            Ok(c) => c,
            Err(e) => return fail(&win, e),
        };
        job(&win, &v, "Wert gespeichert.", move |v| {
            v.set_asset_value(item.id, date, cents).map_err(core_err)?;
            Ok(Sel::default())
        });
    });

    let weak = win.as_weak();
    st.on_value_remove(move |i| {
        let Some((win, v)) = guard(&weak) else { return };
        let Some(item) = selected_item(&win) else {
            return;
        };
        let Some(p) = usize::try_from(i).ok().and_then(|i| item.values.get(i)) else {
            return;
        };
        let date = p.date.clone();
        job(&win, &v, "Wert entfernt.", move |v| {
            v.delete_asset_value(item.id, date).map_err(core_err)?;
            Ok(Sel::default())
        });
    });

    let weak = win.as_weak();
    st.on_sell(move || {
        let Some((win, v)) = guard(&weak) else { return };
        let Some(item) = selected_item(&win) else {
            return;
        };
        let st = win.global::<AssetsState>();
        let date = match al::require_date(&st.get_s_date(), "Verkaufsdatum") {
            Ok(d) => d,
            Err(e) => return fail(&win, e),
        };
        let Some(count) = al::parse_number(&st.get_s_count()).filter(|c| *c > 0.0) else {
            return fail(&win, "Anzahl: Bitte eine Zahl größer als 0 eingeben.");
        };
        let price = if item.setup.price_source == MANUAL_SOURCE {
            match al::optional_cents(&st.get_s_price(), "Verkaufspreis") {
                Ok(p) => p,
                Err(e) => return fail(&win, e),
            }
        } else {
            None
        };
        job(&win, &v, "Verkauf gespeichert.", move |v| {
            v.sell_asset(item.id, date, count, price)
                .map_err(core_err)?;
            Ok(Sel::default())
        });
    });

    let weak = win.as_weak();
    st.on_price_set(move || {
        let Some((win, v)) = guard(&weak) else { return };
        let Some(item) = selected_item(&win) else {
            return;
        };
        let st = win.global::<AssetsState>();
        let date = match al::require_date(&st.get_p_date(), "Datum") {
            Ok(d) => d,
            Err(e) => return fail(&win, e),
        };
        let cents = match al::require_cents(&st.get_p_amount(), "Kurs") {
            Ok(c) => c,
            Err(e) => return fail(&win, e),
        };
        let symbol = item.setup.price_source.clone();
        job(&win, &v, "Kurs eingetragen.", move |v| {
            v.set_asset_price(symbol, date, cents).map_err(core_err)?;
            Ok(Sel::default())
        });
    });

    let weak = win.as_weak();
    st.on_price_lookup(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_now()) else {
            return;
        };
        let Some(item) = selected_item(&win) else {
            return;
        };
        let raw = win.global::<AssetsState>().get_p_date();
        let date = if raw.trim().is_empty() {
            None
        } else {
            match al::require_date(&raw, "Datum") {
                Ok(d) => Some(d),
                Err(e) => return fail(&win, e),
            }
        };
        let symbol = item.setup.price_source.clone();
        let weak = win.as_weak();
        std::thread::spawn(move || {
            let found = match date {
                Some(d) => v.asset_price_on(symbol, d),
                None => v.latest_asset_price(symbol),
            };
            let text = match found {
                Ok(Some(p)) => format!("Kurs {} je Feinunze (vom {})", euro(p.value_cents), p.date),
                Ok(None) => "Kein Kurs gespeichert.".into(),
                Err(e) => e.to_string(),
            };
            let _ = weak.upgrade_in_event_loop(move |win| {
                win.global::<AssetsState>().set_price_info(text.into());
            });
        });
    });

    let weak = win.as_weak();
    st.on_unit_value(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_now()) else {
            return;
        };
        let Some(item) = selected_item(&win) else {
            return;
        };
        let date = match al::require_date(&win.global::<AssetsState>().get_s_date(), "Datum") {
            Ok(d) => d,
            Err(e) => return fail(&win, e),
        };
        let weak = win.as_weak();
        std::thread::spawn(move || {
            let text = match v.asset_unit_value_on(item.id, date.clone()) {
                Ok(Some(c)) => format!("Wert je Stück am {date}: {}", euro(c)),
                Ok(None) => format!("Für den {date} liegt kein Wert vor."),
                Err(e) => e.to_string(),
            };
            let _ = weak.upgrade_in_event_loop(move |win| {
                win.global::<AssetsState>().set_price_info(text.into());
            });
        });
    });
}

fn wire_loans(win: &MainWindow) {
    let st = win.global::<AssetsState>();

    let weak = win.as_weak();
    st.on_loan_select(move |i| {
        if let (Some(win), Ok(i)) = (weak.upgrade(), usize::try_from(i)) {
            fill_loan(&win, i);
        }
    });

    let weak = win.as_weak();
    st.on_loan_new(move || {
        if let Some(win) = weak.upgrade() {
            new_loan_form(&win);
        }
    });

    let weak = win.as_weak();
    st.on_loan_save(move || {
        let Some((win, v)) = guard(&weak) else { return };
        let setup = match al::build_loan(&read_loan_form(&win)) {
            Ok(s) => s,
            Err(e) => return fail(&win, e),
        };
        match selected_loan(&win).map(|l| l.account_id) {
            Some(id) => job(&win, &v, "Darlehen gespeichert.", move |v| {
                v.update_manual_loan(id, setup).map_err(core_err)?;
                Ok(Sel {
                    loan: Some(id),
                    ..Sel::default()
                })
            }),
            None => job(&win, &v, "Darlehen angelegt.", move |v| {
                let id = v.create_manual_loan(setup).map_err(core_err)?;
                Ok(Sel {
                    loan: Some(id),
                    ..Sel::default()
                })
            }),
        }
    });

    let weak = win.as_weak();
    st.on_loan_anchor(move || {
        let Some((win, v)) = guard(&weak) else { return };
        let Some(loan) = selected_loan(&win) else {
            return;
        };
        let st = win.global::<AssetsState>();
        let date = match al::require_date(&st.get_l_anchor_date(), "Stand-Datum") {
            Ok(d) => d,
            Err(e) => return fail(&win, e),
        };
        let balance = match al::require_cents(&st.get_l_anchor_balance(), "Restschuld") {
            Ok(b) if b >= 0 => b,
            Ok(_) => {
                return fail(
                    &win,
                    "Restschuld: Bitte den offenen Betrag positiv eingeben.",
                );
            }
            Err(e) => return fail(&win, e),
        };
        job(&win, &v, "Neuer Stand gesetzt.", move |v| {
            v.set_loan_anchor(loan.account_id, date, balance)
                .map_err(core_err)?;
            Ok(Sel::default())
        });
    });

    let weak = win.as_weak();
    st.on_loan_preview(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_now()) else {
            return;
        };
        let setup = match al::build_loan(&read_loan_form(&win)) {
            Ok(s) => s,
            Err(e) => return fail(&win, e),
        };
        let id = selected_loan(&win).map(|l| l.account_id);
        let weak = win.as_weak();
        std::thread::spawn(move || {
            let result = v
                .preview_manual_loan(id, setup, 5)
                .map_err(|e| e.to_string());
            let _ = weak.upgrade_in_event_loop(move |win| {
                let st = win.global::<AssetsState>();
                match result {
                    Ok(p) => {
                        st.set_status(SharedString::new());
                        st.set_l_preview(
                            format!(
                                "Vorschau: {} Buchungen verknüpft\n{}",
                                p.match_count,
                                al::status_text(&p.status)
                            )
                            .into(),
                        );
                        st.set_l_matches(names(p.matches.iter().map(|b| {
                            format!(
                                "{} · {} · {}",
                                b.booking_date,
                                if b.counterparty.is_empty() {
                                    &b.purpose
                                } else {
                                    &b.counterparty
                                },
                                euro(b.amount_cents)
                            )
                        })));
                    }
                    Err(e) => fail(&win, e),
                }
            });
        });
    });

    let weak = win.as_weak();
    st.on_booking_link(move || {
        let Some((win, v)) = guard(&weak) else { return };
        let (Some(loan), Some(booking)) = (selected_loan(&win), with_state(|s| s.selected_booking))
        else {
            return;
        };
        job(&win, &v, "Buchung zugeordnet.", move |v| {
            v.set_booking_loan(booking, Some(loan.account_id))
                .map_err(core_err)?;
            Ok(Sel::default())
        });
    });

    let weak = win.as_weak();
    st.on_booking_unlink(move || {
        let Some((win, v)) = guard(&weak) else { return };
        let Some(booking) = with_state(|s| s.selected_booking) else {
            return;
        };
        job(&win, &v, "Zuordnung gelöst.", move |v| {
            v.set_booking_loan(booking, None).map_err(core_err)?;
            Ok(Sel::default())
        });
    });
}

fn wire_classification(win: &MainWindow) {
    let st = win.global::<AssetsState>();

    let weak = win.as_weak();
    st.on_class_account_changed(move || {
        if let Some(win) = weak.upgrade() {
            load_classified(&win);
        }
    });

    let weak = win.as_weak();
    st.on_class_select(move |i| {
        if let (Some(win), Ok(i)) = (weak.upgrade(), usize::try_from(i)) {
            fill_class(&win, i);
        }
    });

    let weak = win.as_weak();
    st.on_class_preview(move || {
        let Some(win) = weak.upgrade() else { return };
        let Some(r) = selected_class(&win) else {
            return;
        };
        let st = win.global::<AssetsState>();
        let user = ClassificationValues {
            asset_class: al::class_at(st.get_c_class()),
            instrument: al::instrument_at(st.get_c_instr()),
        };
        let c = classify_asset(r.holding, ClassificationValues::default(), user, false);
        st.set_c_preview(format!("Vorschau (ohne Importvorgabe): {}", class_line(&c)).into());
    });

    let weak = win.as_weak();
    st.on_class_save(move || {
        let Some((win, v)) = guard(&weak) else { return };
        let Some(r) = selected_class(&win) else {
            return;
        };
        let Some(key) = r.classification.security_key.clone() else {
            return fail(
                &win,
                "Für diese Position gibt es keinen stabilen Schlüssel (ISIN fehlt oder ist ungültig).",
            );
        };
        let st = win.global::<AssetsState>();
        let (class, instr) = (
            al::class_at(st.get_c_class()),
            al::instrument_at(st.get_c_instr()),
        );
        let (weak, v2) = (win.as_weak(), v.clone());
        st.set_busy(true);
        std::thread::spawn(move || {
            let result = v2
                .set_asset_class_override(key.clone(), class)
                .and_then(|()| v2.set_instrument_override(key, instr))
                .map_err(|e| e.to_string());
            let _ = weak.upgrade_in_event_loop(move |win| {
                win.global::<AssetsState>().set_busy(false);
                match result {
                    Ok(()) => {
                        win.global::<AssetsState>()
                            .set_status("Klassifizierung gespeichert.".into());
                        load_classified(&win);
                    }
                    Err(e) => fail(&win, e),
                }
            });
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_start_with_manual_and_label_gold() {
        let s = sources();
        assert_eq!(s[0], MANUAL_SOURCE);
        assert!(s.contains(&"XAU".to_string()));
        assert_eq!(source_label("manual"), "Manuell");
        assert_eq!(source_label("XAU"), "Gold (XAU)");
        assert_eq!(source_label("XYZ"), "XYZ");
    }

    #[test]
    fn source_texts_are_distinct() {
        use ClassificationSource::*;
        let all = [User, Import, Rule, Unknown].map(source_text);
        let mut dedup = all.to_vec();
        dedup.sort_unstable();
        dedup.dedup();
        assert_eq!(dedup.len(), all.len());
    }
}
