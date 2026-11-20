//! Wires the sidebar navigation and the views behind it (depot / Sachwerte, Vermögen, Geplant,
//! Lastschriften, Belege & PostBox, rules dialog) to the window. Every vault call runs on a
//! worker thread; results are applied on the UI thread and dropped when a newer load started.
//! The logic itself lives in `nav`, `portfolio` and `lists` (pure, unit-tested).

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use girocrab_core::asset_classification::AssetClass;
use girocrab_core::documents::{DocumentFilter, DocumentInfo};
use girocrab_core::mandates::Mandate;
use girocrab_core::recurring::RecurringSeries;
use girocrab_core::{Account, AccountKind, BookingFilter, Category, Rule, UserGroup, Vault};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::daily::mask;
use crate::lists::{self, RuleForm};
use crate::manage::DeleteGate;
use crate::nav::{self, GroupMode, SidebarInput, Target};
use crate::portfolio::{
    self, Cells, DepotData, DepotTab, Period, WealthData, WealthPosition, WealthTab,
};
use crate::{
    KpiTile, MainWindow, RuleItem, ShareBar, SideRow, TableRow, german_date, hex_color, money,
    privacy_on, with_state,
};

type VaultSlot = Arc<Mutex<Option<Arc<Vault>>>>;

/// View modes of `MainWindow.view-mode`.
const MODE_BOOKINGS: i32 = 0;
const MODE_DEPOT: i32 = 1;
const MODE_WEALTH: i32 = 2;
const MODE_PLANNED: i32 = 3;
const MODE_MANDATES: i32 = 4;
const MODE_DOCUMENTS: i32 = 5;

/// What a worker loaded for a view.
enum Loaded {
    Depot(Box<DepotData>),
    Wealth(WealthData),
    Planned(Vec<RecurringSeries>),
    Mandates(Vec<Mandate>),
    Documents(Vec<DocumentInfo>),
}

#[derive(Default)]
struct Ui {
    accounts: Vec<Account>,
    user_groups: Vec<UserGroup>,
    categories: Vec<Category>,
    tangible_ids: Vec<i64>,
    /// Cached "Geplant" count; `None` = recompute on the next sidebar refresh.
    planned_count: Option<usize>,
    mandate_count: u32,
    document_count: u32,
    folded: HashSet<String>,
    side_rows: Vec<nav::SideRow>,
    side_model: Option<Rc<VecModel<SideRow>>>,
    /// Drops the result of a superseded load.
    generation: u64,
    depot: Option<DepotData>,
    wealth: Option<WealthData>,
    series: Vec<RecurringSeries>,
    mandates: Vec<Mandate>,
    docs: Vec<DocumentInfo>,
    /// Document ids by table row of the Belege view.
    doc_rows: Vec<i64>,
    selected_doc: Option<i64>,
    link_ids: Vec<i64>,
    rules: Vec<Rule>,
    rule_categories: Vec<Category>,
    rule_gate: DeleteGate,
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

fn eur_text(cents: i64, currency: &str, privacy: bool) -> String {
    mask(&money(cents, currency), privacy)
}

/// Updates `model` in place so unchanged rows keep their delegate (a text field in the list keeps
/// the keyboard focus while the rows around it change).
fn sync_model<T: Clone + PartialEq + 'static>(model: &VecModel<T>, new: Vec<T>) {
    let old = model.row_count();
    for (i, row) in new.iter().enumerate() {
        if i < old {
            if model.row_data(i).as_ref() != Some(row) {
                model.set_row_data(i, row.clone());
            }
        } else {
            model.push(row.clone());
        }
    }
    while model.row_count() > new.len() {
        model.remove(model.row_count() - 1);
    }
}

// ------------------------------------------------------------------------------ sidebar

fn slint_row(r: &nav::SideRow) -> SideRow {
    let grey = slint::Color::from_rgb_u8(0x8e, 0x8e, 0x93);
    let dot = match r.kind {
        nav::kind::ACCOUNT => hex_color(&r.color_hex).unwrap_or_else(|| crate::dot_color(&r.label)),
        _ => hex_color(&r.color_hex).unwrap_or(grey),
    };
    SideRow {
        kind: r.kind,
        key: r.key.clone().into(),
        label: r.label.clone().into(),
        detail: r.detail.clone().into(),
        badge: r.badge.clone().into(),
        amount: r.amount.clone().into(),
        negative: r.negative,
        progress: r.progress,
        dot,
        has_dot: matches!(r.kind, nav::kind::ACCOUNT | nav::kind::CATEGORY),
        icon: r.icon.clone().into(),
        depth: r.depth,
        fold: r.fold,
    }
}

/// Rebuilds the sidebar rows from the loaded data (no vault access).
pub fn render_sidebar(win: &MainWindow) {
    let privacy = privacy_on(win);
    let mode = GroupMode::from_index(win.get_group_mode());
    let query = win.get_side_query().to_string();
    let (rows, model) = with_ui(|u| {
        let rows = nav::build(
            &SidebarInput {
                accounts: &u.accounts,
                user_groups: &u.user_groups,
                categories: &u.categories,
                planned_count: u.planned_count.unwrap_or(0),
                mandate_count: u.mandate_count,
                document_count: u.document_count,
                mode,
                query: &query,
                folded: &u.folded,
                privacy,
            },
            &|c, cur| money(c, cur),
        );
        u.side_rows = rows.clone();
        (rows, u.side_model.clone())
    });
    if let Some(model) = model {
        sync_model(&model, rows.iter().map(slint_row).collect());
    }
}

/// Marks the cached "Geplant" count stale (unlock, sync, import, transfer, recurring edit).
pub fn invalidate_planned_count() {
    with_ui(|u| u.planned_count = None);
}

/// Reloads accounts, groups, categories and counts for the sidebar on a worker thread.
/// Stores the accounts and draws the sidebar at once (before the background refresh returns).
pub fn set_accounts_now(win: &MainWindow, vault: &Vault) {
    let accounts = vault.list_accounts().unwrap_or_default();
    let user_groups = vault.list_user_groups().unwrap_or_default();
    let tangible_ids = vault.list_tangible_account_ids().unwrap_or_default();
    with_ui(|u| {
        u.accounts = accounts;
        u.user_groups = user_groups;
        u.tangible_ids = tangible_ids;
    });
    render_sidebar(win);
}

pub fn refresh_sidebar(win: &MainWindow, vault: &Arc<Vault>) {
    let (weak, v) = (win.as_weak(), vault.clone());
    let cached = with_ui(|u| u.planned_count);
    std::thread::spawn(move || {
        let accounts = v.list_accounts().unwrap_or_default();
        let user_groups = v.list_user_groups().unwrap_or_default();
        let categories = v.list_categories(None, None).unwrap_or_default();
        let tangible_ids = v.list_tangible_account_ids().unwrap_or_default();
        let planned_count = cached.or_else(|| v.list_recurring(Vec::new()).ok().map(|s| s.len()));
        let mandate_count = v.count_active_mandates().unwrap_or(0);
        let document_count = v
            .list_all_documents(DocumentFilter::default())
            .map_or(0, |d| d.len() as u32);
        let v2 = v.clone();
        let _ = weak.upgrade_in_event_loop(move |win| {
            with_ui(|u| {
                u.accounts = accounts;
                u.user_groups = user_groups;
                u.categories = categories;
                u.tangible_ids = tangible_ids;
                u.planned_count = planned_count;
                u.mandate_count = mandate_count;
                u.document_count = document_count;
            });
            render_sidebar(&win);
            // The account kinds are known now: a depot opened before shows its own view.
            let target = with_state(|st| st.target);
            if wants_view(target) != win.get_view_mode() {
                apply_view(&win, &v2);
            } else if wants_view(target) == MODE_BOOKINGS {
                apply_header(&win);
            }
        });
    });
}

fn toggle_fold(win: &MainWindow, id: &str) {
    with_ui(|u| {
        if !u.folded.remove(id) {
            u.folded.insert(id.to_string());
        }
    });
    render_sidebar(win);
}

// ------------------------------------------------------------------------------ targets

fn is_depot_like(u: &Ui, id: i64) -> bool {
    u.tangible_ids.contains(&id)
        || u.accounts
            .iter()
            .any(|a| a.id == id && a.kind == AccountKind::Brokerage)
}

fn wants_view(target: Target) -> i32 {
    match target {
        Target::Wealth | Target::Depot => MODE_WEALTH,
        Target::Planned | Target::Subscriptions => MODE_PLANNED,
        Target::Mandates => MODE_MANDATES,
        Target::Documents => MODE_DOCUMENTS,
        Target::Account(id) if with_ui(|u| is_depot_like(u, id)) => MODE_DEPOT,
        _ => MODE_BOOKINGS,
    }
}

/// Keeps the booking scope in line with the account selection of the header (`show`).
pub fn sync_target(selected: Option<i64>) {
    with_state(|st| {
        if matches!(st.target, Target::Account(_) | Target::AllAccounts) {
            st.target = match selected {
                Some(id) if id > 0 => Target::Account(id),
                _ => Target::AllAccounts,
            };
        }
    });
}

/// A click on a sidebar row or on an account elsewhere.
pub fn select_target(win: &MainWindow, vault: &Arc<Vault>, target: Target) {
    with_state(|st| st.target = target);
    win.set_side_selected(target.key().into());
    win.set_selected(match target {
        Target::Account(id) => id as i32,
        _ => 0,
    });
    win.set_table_selected(-1);
    with_ui(|u| u.selected_doc = None);
    apply_view(win, vault);
}

/// Shows the view of the current target: bookings reload, the other views load their data.
pub fn apply_view(win: &MainWindow, vault: &Arc<Vault>) {
    let target = with_state(|st| st.target);
    let mode = wants_view(target);
    win.set_view_mode(mode);
    win.set_side_selected(target.key().into());
    win.set_view_status(SharedString::new());
    if mode == MODE_BOOKINGS {
        apply_header(win);
        crate::reload(win, vault.clone());
        return;
    }
    clear_table(win, "Wird geladen …");
    let (accounts, tangible) = with_ui(|u| (u.accounts.clone(), u.tangible_ids.clone()));
    let generation = with_ui(|u| {
        u.generation += 1;
        u.generation
    });
    let (weak, v) = (win.as_weak(), vault.clone());
    std::thread::spawn(move || {
        let loaded = load(&v, target, &accounts, &tangible);
        let _ = weak.upgrade_in_event_loop(move |win| {
            if with_ui(|u| u.generation) != generation {
                return;
            }
            match loaded {
                Ok(l) => {
                    with_ui(|u| match l {
                        Loaded::Depot(d) => u.depot = Some(*d),
                        Loaded::Wealth(w) => u.wealth = Some(w),
                        Loaded::Planned(s) => u.series = s,
                        Loaded::Mandates(m) => u.mandates = m,
                        Loaded::Documents(d) => u.docs = d,
                    });
                    render_view(&win);
                }
                Err(e) => {
                    clear_table(&win, "");
                    win.set_view_status(e.into());
                }
            }
        });
    });
}

fn depot_data(vault: &Vault, account: &Account, tangible: bool) -> Result<DepotData, String> {
    let holdings = match vault.classified_assets(account.id) {
        Ok(v) => v
            .into_iter()
            .map(|c| (c.holding, c.classification.asset_class))
            .collect(),
        Err(_) if !tangible => vault
            .list_holdings(account.id)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|h| (h, AssetClass::Unclassified))
            .collect(),
        Err(e) => return Err(e.to_string()),
    };
    // Sachwerte have no securities transactions: an empty list means "no data", not zero.
    let transactions = match vault.list_security_transactions(account.id) {
        Ok(t) => t,
        Err(_) if tangible => Vec::new(),
        Err(e) => return Err(e.to_string()),
    };
    Ok(DepotData {
        account: account.clone(),
        holdings,
        transactions,
    })
}

fn load(
    vault: &Vault,
    target: Target,
    accounts: &[Account],
    tangible: &[i64],
) -> Result<Loaded, String> {
    let e = |e: girocrab_core::CoreError| e.to_string();
    match target {
        Target::Account(id) => {
            let account = accounts
                .iter()
                .find(|a| a.id == id)
                .ok_or("Konto nicht gefunden.")?;
            depot_data(vault, account, tangible.contains(&id)).map(|d| Loaded::Depot(Box::new(d)))
        }
        Target::Wealth | Target::Depot => {
            let mut positions = Vec::new();
            for a in accounts {
                let is_tangible = tangible.contains(&a.id);
                if a.kind != AccountKind::Brokerage && !is_tangible {
                    continue;
                }
                for c in vault.classified_assets(a.id).map_err(e)? {
                    positions.push((
                        a.id,
                        WealthPosition {
                            holding: c.holding,
                            tangible: is_tangible,
                        },
                    ));
                }
            }
            Ok(Loaded::Wealth(WealthData {
                accounts: accounts.to_vec(),
                tangible_account_ids: tangible.to_vec(),
                positions,
            }))
        }
        Target::Planned | Target::Subscriptions => vault
            .list_recurring(Vec::new())
            .map(Loaded::Planned)
            .map_err(e),
        Target::Mandates => vault.list_mandates(None).map(Loaded::Mandates).map_err(e),
        Target::Documents => vault
            .list_all_documents(DocumentFilter::default())
            .map(Loaded::Documents)
            .map_err(e),
        _ => Err("Keine Ansicht für diese Auswahl.".into()),
    }
}

// ------------------------------------------------------------------------------ rendering

fn to_table(rows: Vec<Cells>) -> Vec<TableRow> {
    rows.into_iter()
        .map(|r| TableRow {
            id: r.id as i32,
            c0: r.cells[0].clone().into(),
            c1: r.cells[1].clone().into(),
            c2: r.cells[2].clone().into(),
            c3: r.cells[3].clone().into(),
            c4: r.cells[4].clone().into(),
            negative: r.negative,
        })
        .collect()
}

fn set_columns(win: &MainWindow, columns: &[&str]) {
    let cols: Vec<SharedString> = columns.iter().map(|c| SharedString::from(*c)).collect();
    win.set_table_columns(ModelRc::new(VecModel::from(cols)));
}

fn set_rows(win: &MainWindow, rows: Vec<Cells>, empty: &str) {
    win.set_table_rows(ModelRc::new(VecModel::from(to_table(rows))));
    win.set_table_empty(empty.into());
}

fn clear_table(win: &MainWindow, empty: &str) {
    set_rows(win, Vec::new(), empty);
    win.set_kpis(ModelRc::new(VecModel::from(Vec::<KpiTile>::new())));
    win.set_shares(ModelRc::new(VecModel::from(Vec::<ShareBar>::new())));
    win.set_view_note(SharedString::new());
}

fn set_shares(win: &MainWindow, shares: &[portfolio::Share]) {
    let bars: Vec<ShareBar> = shares
        .iter()
        .map(|s| ShareBar {
            label: s.label.clone().into(),
            value: s.value.clone().into(),
            share: s.share,
            color: hex_color(&s.color_hex).unwrap_or(slint::Color::from_rgb_u8(0x8e, 0x8e, 0x93)),
        })
        .collect();
    win.set_shares(ModelRc::new(VecModel::from(bars)));
}

/// Draws the loaded data of the current view; no vault access (period, tab, interval and
/// privacy changes come through here).
pub fn render_view(win: &MainWindow) {
    let privacy = privacy_on(win);
    let mode = win.get_view_mode();
    let eur = move |c: i64| eur_text(c, "EUR", privacy);
    let date = |d: &str| german_date(d);
    win.set_period_labels(ModelRc::new(VecModel::from(
        Period::LABELS
            .iter()
            .map(|l| SharedString::from(*l))
            .collect::<Vec<_>>(),
    )));
    match mode {
        MODE_DEPOT => {
            let Some(data) = with_ui(|u| u.depot.take()) else {
                return;
            };
            let cur = data.account.currency.clone();
            let cur_money = move |c: i64| eur_text(c, &cur, privacy);
            let period = Period::from_index(win.get_period_index());
            let view =
                portfolio::depot_view(&data, period, &portfolio::today_iso(), &cur_money, &date);
            let tab = DepotTab::from_index(win.get_depot_tab());
            win.set_view_title(mask(&view.title, privacy).into());
            win.set_view_total(view.total.clone().into());
            win.set_view_note(view.note.clone().into());
            let kpis: Vec<KpiTile> = view
                .kpis
                .iter()
                .map(|k| KpiTile {
                    label: k.label.clone().into(),
                    value: k.value.clone().into(),
                    negative: k.negative && !privacy,
                })
                .collect();
            win.set_kpis(ModelRc::new(VecModel::from(kpis)));
            set_shares(win, &view.composition);
            match tab {
                DepotTab::Positions => {
                    set_columns(win, &portfolio::POSITION_COLUMNS);
                    set_rows(win, view.positions, "Keine Positionen");
                }
                DepotTab::Activities => {
                    set_columns(win, &portfolio::ACTIVITY_COLUMNS);
                    set_rows(win, view.activities, "Keine Aktivitäten im Zeitraum");
                }
            }
            with_ui(|u| u.depot = Some(data));
        }
        MODE_WEALTH => {
            let Some(data) = with_ui(|u| u.wealth.take()) else {
                return;
            };
            let tab = WealthTab::from_index(win.get_wealth_tab());
            let view = portfolio::wealth_view(&data, tab, &eur);
            win.set_view_title("Vermögen".into());
            win.set_view_total(view.net_worth.clone().into());
            win.set_view_note(view.note.clone().into());
            win.set_kpis(ModelRc::new(VecModel::from(Vec::<KpiTile>::new())));
            set_shares(win, &view.composition);
            let cols: Vec<&str> = view.columns.iter().map(String::as_str).collect();
            set_columns(win, &cols);
            let mut rows = view.rows;
            for r in &mut rows {
                r.mask(&[0], privacy);
            }
            set_rows(win, rows, "Keine Einträge");
            with_ui(|u| u.wealth = Some(data));
        }
        MODE_PLANNED => {
            win.set_view_title("Geplant".into());
            win.set_interval_labels(ModelRc::new(VecModel::from(
                lists::INTERVAL_LABELS
                    .iter()
                    .map(|l| SharedString::from(*l))
                    .collect::<Vec<_>>(),
            )));
            let mut rows =
                with_ui(|u| lists::planned_rows(&u.series, win.get_interval_index(), &eur, &date));
            for r in &mut rows {
                r.mask(&[0], privacy);
            }
            set_columns(win, &lists::PLANNED_COLUMNS);
            win.set_view_note("Wiederkehrende Umsätze, die aus dem Verlauf erkannt wurden.".into());
            set_rows(win, rows, "Keine wiederkehrenden Umsätze erkannt");
        }
        MODE_MANDATES => {
            win.set_view_title("Lastschriften".into());
            let mut rows = with_ui(|u| lists::mandate_rows(&u.mandates, &eur, &date));
            for r in &mut rows {
                r.mask(&[0, 1], privacy);
            }
            set_columns(win, &lists::MANDATE_COLUMNS);
            win.set_view_note(
                "Mandate, unter denen du Lastschriften einziehst. Der nächste Einzug nennt die Folgeart; ein festes Datum gibt es pro Mandat nicht.".into(),
            );
            set_rows(win, rows, "Keine Lastschrift-Mandate");
        }
        MODE_DOCUMENTS => {
            win.set_view_title("Belege & PostBox".into());
            let (mut rows, accounts) = with_ui(|u| {
                (
                    lists::document_rows(&u.docs, &date),
                    u.accounts
                        .iter()
                        .filter(|a| a.archived_unix.is_none())
                        .map(|a| (a.id, a.name.clone()))
                        .collect::<Vec<_>>(),
                )
            });
            for r in &mut rows {
                r.mask(&[0, 3], privacy);
            }
            let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
            let names: Vec<SharedString> = accounts
                .iter()
                .map(|a| SharedString::from(mask(&a.1, privacy)))
                .collect();
            if win.get_doc_account_index() as usize >= names.len() {
                win.set_doc_account_index(0);
            }
            win.set_doc_accounts(ModelRc::new(VecModel::from(names)));
            let selected = with_ui(|u| {
                u.doc_rows = ids.clone();
                u.selected_doc.filter(|d| ids.contains(d))
            });
            with_ui(|u| u.selected_doc = selected);
            win.set_table_selected(
                selected
                    .and_then(|d| ids.iter().position(|i| *i == d))
                    .map_or(-1, |p| p as i32),
            );
            set_doc_flags(win);
            win.set_view_note(
                "Neue Dateien werden dem im Auswahlfeld gewählten Konto zugeordnet und lassen sich mit einem Umsatz verknüpfen.".into(),
            );
            set_columns(win, &lists::DOCUMENT_COLUMNS);
            set_rows(win, rows, "Keine Belege");
        }
        _ => {}
    }
}

fn set_doc_flags(win: &MainWindow) {
    let (selected, linked) = with_ui(|u| {
        let doc = u
            .selected_doc
            .and_then(|id| u.docs.iter().find(|d| d.id == id));
        (doc.is_some(), doc.is_some_and(|d| d.booking_id.is_some()))
    });
    win.set_doc_selected(selected);
    win.set_doc_linked(linked);
}

/// Header card of a booking list: account balance, "Alle Konten" or the scope's label.
pub fn apply_header(win: &MainWindow) {
    let privacy = privacy_on(win);
    let target = with_state(|st| st.target);
    let (name, balance) = match target {
        Target::Account(id) => with_ui(|u| {
            u.accounts.iter().find(|a| a.id == id).map(|a| {
                (
                    a.name.clone(),
                    eur_text(a.balance_cents, &a.currency, privacy),
                )
            })
        })
        .unwrap_or_else(|| {
            (
                win.get_selected_name().to_string(),
                win.get_selected_balance().to_string(),
            )
        }),
        Target::AllAccounts => ("Alle Konten".into(), win.get_total().to_string()),
        other => with_ui(|u| {
            u.side_rows.iter().find(|r| r.key == other.key()).map(|r| {
                let note = if matches!(other, Target::Category(_)) {
                    r.amount.clone()
                } else if r.badge.is_empty() {
                    String::new()
                } else {
                    format!("{} offen", r.badge)
                };
                (r.label.clone(), note)
            })
        })
        .unwrap_or_default(),
    };
    let subtitle = match target {
        Target::Account(id) => with_ui(|u| {
            u.accounts
                .iter()
                .find(|a| a.id == id)
                .map(|a| crate::daily::account_subtitle(&a.bank_name, &a.iban, privacy))
        })
        .unwrap_or_default(),
        Target::AllAccounts => {
            let n = with_ui(|u| u.accounts.len());
            format!("{n} Konten")
        }
        Target::NewBookings => "Ungeprüfte Umsätze aller Konten".into(),
        Target::Uncategorized => "Umsätze ohne Kategorie".into(),
        Target::Category(_) => "Kategorie inklusive Unterkategorien".into(),
        _ => String::new(),
    };
    win.set_selected_subtitle(subtitle.into());
    win.set_selected_name(name.into());
    win.set_selected_balance(balance.into());
}

/// Privacy toggled: redraw sidebar and view from the cached data.
pub fn rerender(win: &MainWindow) {
    render_sidebar(win);
    if win.get_view_mode() == MODE_BOOKINGS {
        apply_header(win);
    } else {
        render_view(win);
    }
}

/// Vault locked: drop everything cached and the opened document copies.
pub fn on_lock(win: &MainWindow) {
    clear_opened_documents();
    with_ui(|u| {
        let model = u.side_model.take();
        *u = Ui::default();
        u.side_model = model;
        if let Some(m) = &u.side_model {
            sync_model(m, Vec::new());
        }
    });
    win.set_view_mode(MODE_BOOKINGS);
    win.set_side_selected("all".into());
    win.set_side_query(SharedString::new());
    win.set_link_open(false);
    win.set_rules_open(false);
    crate::tags_ui::on_lock(win);
    crate::groups_ui::on_lock(win);
    crate::assets_loans_ui::on_lock(win);
    crate::recurring_ui::on_lock(win);
    crate::mandates_ui::on_lock(win);
    crate::settings_import_ui::on_lock(win);
    clear_table(win, "");
}

// ------------------------------------------------------------------------------ documents

fn docs_dir() -> PathBuf {
    std::env::temp_dir().join("girocrab-belege")
}

/// Removes the decrypted copies written for "Öffnen".
pub fn clear_opened_documents() {
    let _ = std::fs::remove_dir_all(docs_dir());
}

/// Startup cleanup: a hard kill can leave decrypted receipt copies behind.
pub fn purge_stale_documents() {
    clear_opened_documents();
}

fn open_external(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    let mut cmd = std::process::Command::new("explorer");
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let mut cmd = std::process::Command::new("xdg-open");
    cmd.arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Die Datei konnte nicht geöffnet werden: {e}"))
}

/// Native file dialog for a receipt (Windows); elsewhere the `GIROCRAB_IMPORT_FILE` fallback.
#[cfg(windows)]
fn pick_document() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Beleg hinzufügen")
        .add_filter("Belege", &["pdf", "png", "jpg", "jpeg", "txt"])
        .pick_file()
}

#[cfg(not(windows))]
fn pick_document() -> Option<PathBuf> {
    std::env::var_os("GIROCRAB_IMPORT_FILE").map(PathBuf::from)
}

/// Largest file taken in as a receipt.
const MAX_DOCUMENT_BYTES: u64 = 50 * 1024 * 1024;

fn reload_docs(win: &MainWindow, vault: &Arc<Vault>, status: String) {
    apply_view(win, vault);
    win.set_view_status(status.into());
}

fn run_doc_job(
    win: &MainWindow,
    vault: &Arc<Vault>,
    job: impl FnOnce(&Vault) -> Result<String, String> + Send + 'static,
) {
    win.set_busy(true);
    let (weak, v) = (win.as_weak(), vault.clone());
    std::thread::spawn(move || {
        let result = job(&v);
        let _ = weak.upgrade_in_event_loop(move |win| {
            win.set_busy(false);
            match result {
                Ok(msg) => reload_docs(&win, &v, msg),
                Err(e) => win.set_view_status(e.into()),
            }
        });
    });
}

fn wire_documents(win: &MainWindow, slot: &VaultSlot) {
    let (weak, slot2) = (win.as_weak(), slot.clone());
    win.on_table_select(move |i| {
        let Some(win) = weak.upgrade() else { return };
        let _ = &slot2;
        let doc = with_ui(|u| u.doc_rows.get(i as usize).copied());
        with_ui(|u| u.selected_doc = doc);
        win.set_table_selected(if doc.is_some() { i } else { -1 });
        set_doc_flags(&win);
    });

    let (weak, slot2) = (win.as_weak(), slot.clone());
    win.on_doc_attach(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&slot2)) else {
            return;
        };
        if privacy_on(&win) {
            win.set_view_status("Im Privatmodus nicht möglich.".into());
            return;
        }
        let Some(account) = with_ui(|u| {
            u.accounts
                .iter()
                .filter(|a| a.archived_unix.is_none())
                .nth(win.get_doc_account_index().max(0) as usize)
                .map(|a| a.id)
        }) else {
            return;
        };
        let Some(path) = pick_document() else { return };
        run_doc_job(&win, &v, move |vault| {
            let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
            if meta.len() > MAX_DOCUMENT_BYTES {
                return Err("Die Datei ist größer als 50 MB.".into());
            }
            let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
            let file_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("beleg")
                .to_string();
            vault
                .attach_document_to_account(
                    account,
                    lists::document_title(&path),
                    file_name,
                    bytes,
                    "Beleg".into(),
                )
                .map(|_| "Beleg hinzugefügt.".to_string())
                .map_err(|e| e.to_string())
        });
    });

    let (weak, slot2) = (win.as_weak(), slot.clone());
    win.on_doc_open(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&slot2)) else {
            return;
        };
        let Some(doc) = with_ui(|u| {
            u.selected_doc
                .and_then(|id| u.docs.iter().find(|d| d.id == id).cloned())
        }) else {
            return;
        };
        win.set_busy(true);
        let weak = win.as_weak();
        std::thread::spawn(move || {
            let result = (|| -> Result<(), String> {
                let bytes = v.get_document_bytes(doc.id).map_err(|e| e.to_string())?;
                clear_opened_documents();
                let dir = docs_dir();
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                let name = lists::safe_file_name(&doc.file_name, "beleg.bin");
                let path = dir.join(format!("{}-{name}", doc.id));
                std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
                open_external(&path)
            })();
            let _ = weak.upgrade_in_event_loop(move |win| {
                win.set_busy(false);
                win.set_view_status(match result {
                    Ok(()) => "Beleg geöffnet. Die Kopie wird beim Sperren gelöscht.".into(),
                    Err(e) => e.into(),
                });
            });
        });
    });

    let (weak, slot2) = (win.as_weak(), slot.clone());
    win.on_doc_unlink(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&slot2)) else {
            return;
        };
        if privacy_on(&win) {
            win.set_view_status("Im Privatmodus nicht möglich.".into());
            return;
        }
        let Some(id) = with_ui(|u| u.selected_doc) else {
            return;
        };
        run_doc_job(&win, &v, move |vault| {
            vault
                .link_document(id, None)
                .map(|_| "Verknüpfung gelöst.".to_string())
                .map_err(|e| e.to_string())
        });
    });

    let (weak, slot2) = (win.as_weak(), slot.clone());
    win.on_doc_link_open(move || {
        let Some(win) = weak.upgrade() else { return };
        if privacy_on(&win) {
            win.set_view_status("Im Privatmodus nicht möglich.".into());
            return;
        }
        win.set_link_query(SharedString::new());
        win.set_link_status(SharedString::new());
        win.set_link_open(true);
        if let Some(v) = vault_of(&slot2) {
            search_bookings(&win, &v, String::new());
        }
    });

    let (weak, slot2) = (win.as_weak(), slot.clone());
    win.on_link_search(move |q| {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&slot2)) else {
            return;
        };
        search_bookings(&win, &v, q.to_string());
    });

    let (weak, slot2) = (win.as_weak(), slot.clone());
    win.on_link_pick(move |i| {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&slot2)) else {
            return;
        };
        let (Some(booking), Some(doc)) = (
            with_ui(|u| u.link_ids.get(i as usize).copied()),
            with_ui(|u| u.selected_doc),
        ) else {
            return;
        };
        win.set_link_open(false);
        win.invoke_refocus();
        run_doc_job(&win, &v, move |vault| {
            vault
                .link_document(doc, Some(booking))
                .map(|_| "Beleg verknüpft.".to_string())
                .map_err(|e| e.to_string())
        });
    });
}

/// Fills the booking picker (latest 40 hits) on a worker thread.
fn search_bookings(win: &MainWindow, vault: &Arc<Vault>, query: String) {
    let (weak, v) = (win.as_weak(), vault.clone());
    std::thread::spawn(move || {
        let page = v.list_transactions(BookingFilter {
            search: Some(query).filter(|q| !q.trim().is_empty()),
            limit: 40,
            ..Default::default()
        });
        let _ = weak.upgrade_in_event_loop(move |win| match page {
            Ok(page) => {
                let rows: Vec<Cells> = page
                    .transactions
                    .iter()
                    .map(|b| Cells {
                        id: b.id,
                        cells: [
                            german_date(&b.booking_date),
                            b.counterparty.clone(),
                            money(b.amount_cents, &b.currency),
                            String::new(),
                            String::new(),
                            String::new(),
                        ],
                        negative: b.amount_cents < 0,
                    })
                    .collect();
                with_ui(|u| u.link_ids = rows.iter().map(|r| r.id).collect());
                // The list is indexed by position, not by booking id.
                let table: Vec<TableRow> = to_table(rows)
                    .into_iter()
                    .enumerate()
                    .map(|(i, mut r)| {
                        r.id = i as i32;
                        r
                    })
                    .collect();
                win.set_link_results(ModelRc::new(VecModel::from(table)));
                win.set_link_status(SharedString::new());
            }
            Err(e) => win.set_link_status(e.to_string().into()),
        });
    });
}

// ------------------------------------------------------------------------------ rules

pub fn close_rules(win: &MainWindow) {
    with_ui(|u| u.rule_gate.reset());
    win.set_rule_armed(false);
    win.set_rules_open(false);
}

fn clear_rule_form(win: &MainWindow) {
    win.set_rule_selected(-1);
    win.set_rule_name(SharedString::new());
    win.set_rule_pattern(SharedString::new());
    win.set_rule_category(0);
    win.set_rule_field(0);
    win.set_rule_kind(0);
    win.set_rule_enabled(true);
    win.set_rule_case(false);
    win.set_rule_min(SharedString::new());
    win.set_rule_max(SharedString::new());
    win.set_rule_preview(SharedString::new());
    win.set_rule_armed(false);
    with_ui(|u| u.rule_gate.reset());
}

fn fill_rule_form(win: &MainWindow, index: usize) {
    let Some((rule, cat)) = with_ui(|u| {
        u.rules.get(index).map(|r| {
            (
                r.clone(),
                u.rule_categories.iter().position(|c| c.id == r.category_id),
            )
        })
    }) else {
        return;
    };
    win.set_rule_selected(index as i32);
    win.set_rule_name(rule.name.into());
    win.set_rule_pattern(rule.pattern.into());
    win.set_rule_category(cat.map_or(0, |c| c as i32));
    win.set_rule_field(lists::rule_field_index(rule.field));
    win.set_rule_kind(lists::rule_kind_index(rule.match_kind));
    win.set_rule_enabled(rule.enabled);
    win.set_rule_case(rule.case_sensitive);
    win.set_rule_min(lists::bound_text(rule.min_amount_cents).into());
    win.set_rule_max(lists::bound_text(rule.max_amount_cents).into());
    win.set_rule_preview(SharedString::new());
    win.set_rule_armed(false);
    with_ui(|u| u.rule_gate.reset());
}

fn render_rules(win: &MainWindow, rules: Vec<Rule>, cats: Vec<Category>, select: Option<i64>) {
    let items: Vec<RuleItem> = rules
        .iter()
        .map(|r| {
            let (title, detail) = lists::rule_line(r, &cats);
            RuleItem {
                title: title.into(),
                detail: detail.into(),
            }
        })
        .collect();
    let names: Vec<SharedString> = cats
        .iter()
        .map(|c| SharedString::from(c.path.as_str()))
        .collect();
    let index = select.and_then(|id| rules.iter().position(|r| r.id == id));
    with_ui(|u| {
        u.rules = rules;
        u.rule_categories = cats;
        u.rule_gate.reset();
    });
    win.set_rule_items(ModelRc::new(VecModel::from(items)));
    win.set_rule_category_names(ModelRc::new(VecModel::from(names)));
    match index {
        Some(i) => fill_rule_form(win, i),
        None => clear_rule_form(win),
    }
}

/// Loads rules and categories on a worker thread and shows them.
fn load_rules(win: &MainWindow, vault: &Arc<Vault>, select: Option<i64>, status: String) {
    let (weak, v) = (win.as_weak(), vault.clone());
    std::thread::spawn(move || {
        let rules = v.list_rules().map_err(|e| e.to_string());
        let cats = v.list_categories(None, None).map_err(|e| e.to_string());
        let _ = weak.upgrade_in_event_loop(move |win| match (rules, cats) {
            (Ok(rules), Ok(cats)) => {
                render_rules(&win, rules, cats, select);
                win.set_rule_status(status.into());
            }
            (Err(e), _) | (_, Err(e)) => win.set_rule_status(e.into()),
        });
    });
}

fn wire_rules(win: &MainWindow, slot: &VaultSlot) {
    let (weak, slot2) = (win.as_weak(), slot.clone());
    win.on_rules_request(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&slot2)) else {
            return;
        };
        win.set_rule_status(SharedString::new());
        win.set_rules_open(true);
        load_rules(&win, &v, None, String::new());
    });

    let weak = win.as_weak();
    win.on_rule_select(move |i| {
        if let Some(win) = weak.upgrade() {
            win.set_rule_status(SharedString::new());
            fill_rule_form(&win, i.max(0) as usize);
        }
    });

    let weak = win.as_weak();
    win.on_rule_new(move || {
        if let Some(win) = weak.upgrade() {
            win.set_rule_status(SharedString::new());
            clear_rule_form(&win);
        }
    });

    let (weak, slot2) = (win.as_weak(), slot.clone());
    win.on_rule_save(move |name, pattern, category, field, kind, enabled| {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&slot2)) else {
            return;
        };
        let form = RuleForm {
            name: name.to_string(),
            pattern: pattern.to_string(),
            category_index: category,
            field_index: field,
            kind_index: kind,
            enabled,
            case_sensitive: win.get_rule_case(),
            min_amount: win.get_rule_min().to_string(),
            max_amount: win.get_rule_max().to_string(),
        };
        let built = with_ui(|u| {
            let existing = usize::try_from(win.get_rule_selected())
                .ok()
                .and_then(|i| u.rules.get(i));
            lists::validate_rule(
                &form,
                existing,
                &u.rule_categories,
                lists::next_priority(&u.rules),
            )
        });
        let rule = match built {
            Ok(r) => r,
            Err(e) => {
                win.set_rule_status(e.into());
                return;
            }
        };
        win.set_rule_status("Wird gespeichert …".into());
        let weak = win.as_weak();
        std::thread::spawn(move || {
            let saved = v.save_rule(rule).map_err(|e| e.to_string());
            let _ = weak.upgrade_in_event_loop(move |win| match saved {
                Ok(id) => load_rules(&win, &v, Some(id), "Regel gespeichert.".into()),
                Err(e) => win.set_rule_status(e.into()),
            });
        });
    });

    // Preview (apply = false) and scoped apply (#759): only the bookings the form's rule matches,
    // and only uncategorised ones, get its category. Other bookings stay untouched.
    for apply in [false, true] {
        let (weak, slot2) = (win.as_weak(), slot.clone());
        let handler = move || {
            let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&slot2)) else {
                return;
            };
            let form = RuleForm {
                name: win.get_rule_name().to_string(),
                pattern: win.get_rule_pattern().to_string(),
                category_index: win.get_rule_category(),
                field_index: win.get_rule_field(),
                kind_index: win.get_rule_kind(),
                enabled: true,
                case_sensitive: win.get_rule_case(),
                min_amount: win.get_rule_min().to_string(),
                max_amount: win.get_rule_max().to_string(),
            };
            let built = with_ui(|u| {
                lists::validate_rule(
                    &form,
                    None,
                    &u.rule_categories,
                    lists::next_priority(&u.rules),
                )
            });
            let rule = match built {
                Ok(r) => r,
                Err(e) => {
                    win.set_rule_status(e.into());
                    return;
                }
            };
            win.set_rule_status("Wird geprüft …".into());
            let weak = win.as_weak();
            std::thread::spawn(move || {
                let category = rule.category_id;
                let result = v.preview_rule(rule, 100_000).map_err(|e| e.to_string());
                let result = result.and_then(|hits| {
                    let open: Vec<i64> = hits
                        .iter()
                        .filter(|b| b.category_id.is_none())
                        .map(|b| b.id)
                        .collect();
                    if apply {
                        for id in &open {
                            v.set_transaction_category(*id, Some(category))
                                .map_err(|e| e.to_string())?;
                        }
                    }
                    Ok((hits.len(), open.len()))
                });
                let _ = weak.upgrade_in_event_loop(move |win| match result {
                    Ok((all, open)) => {
                        win.set_rule_status(SharedString::new());
                        win.set_rule_preview(
                            if apply {
                                format!("{open} unkategorisierte Buchung(en) zugeordnet ({all} Treffer).")
                            } else {
                                format!("{all} Treffer, davon {open} ohne Kategorie.")
                            }
                            .into(),
                        );
                    }
                    Err(e) => win.set_rule_status(e.into()),
                });
            });
        };
        if apply {
            win.on_rule_apply_scoped(handler);
        } else {
            win.on_rule_preview_request(handler);
        }
    }

    let (weak, slot2) = (win.as_weak(), slot.clone());
    win.on_rule_delete(move || {
        let (Some(win), Some(v)) = (weak.upgrade(), vault_of(&slot2)) else {
            return;
        };
        let Some(id) = with_ui(|u| {
            usize::try_from(win.get_rule_selected())
                .ok()
                .and_then(|i| u.rules.get(i))
                .map(|r| r.id)
        }) else {
            return;
        };
        let confirmed = with_ui(|u| u.rule_gate.press(id));
        win.set_rule_armed(!confirmed);
        if !confirmed {
            return;
        }
        let weak = win.as_weak();
        std::thread::spawn(move || {
            let deleted = v.delete_rule(id).map_err(|e| e.to_string());
            let _ = weak.upgrade_in_event_loop(move |win| match deleted {
                Ok(()) => load_rules(&win, &v, None, "Regel gelöscht.".into()),
                Err(e) => win.set_rule_status(e.into()),
            });
        });
    });
}

// ------------------------------------------------------------------------------ wiring

pub fn wire(win: &MainWindow, vault: VaultSlot) {
    let model = Rc::new(VecModel::<SideRow>::default());
    win.set_side_rows(ModelRc::from(model.clone()));
    with_ui(|u| u.side_model = Some(model));
    win.set_rule_field_labels(ModelRc::new(VecModel::from(
        lists::RULE_FIELD_LABELS
            .iter()
            .map(|l| SharedString::from(*l))
            .collect::<Vec<_>>(),
    )));
    win.set_rule_kind_labels(ModelRc::new(VecModel::from(
        lists::RULE_KIND_LABELS
            .iter()
            .map(|l| SharedString::from(*l))
            .collect::<Vec<_>>(),
    )));

    let (weak, slot) = (win.as_weak(), vault.clone());
    win.on_side_click(move |key| {
        let Some(win) = weak.upgrade() else { return };
        let key = key.to_string();
        if let Some(id) = nav::toggle_id(&key) {
            toggle_fold(&win, id);
        } else if let (Some(target), Some(v)) = (Target::parse(&key), vault_of(&slot)) {
            select_target(&win, &v, target);
        }
    });

    let weak = win.as_weak();
    win.on_side_fold(move |key| {
        let Some(win) = weak.upgrade() else { return };
        if let Some(id) = nav::category_fold_id(&key) {
            toggle_fold(&win, &id);
        }
    });

    let weak = win.as_weak();
    win.on_side_controls_changed(move || {
        if let Some(win) = weak.upgrade() {
            render_sidebar(&win);
        }
    });

    let weak = win.as_weak();
    win.on_view_changed(move || {
        if let Some(win) = weak.upgrade() {
            render_view(&win);
        }
    });

    wire_documents(win, &vault);
    wire_rules(win, &vault);
    crate::tags_ui::wire(win, vault.clone());
    crate::groups_ui::wire(win, vault.clone());
    crate::assets_loans_ui::wire(win, vault.clone());
    crate::recurring_ui::wire(win, vault.clone());
    crate::mandates_ui::wire(win, vault.clone());
    crate::settings_import_ui::wire(win, vault.clone());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demo_vault() -> (Arc<Vault>, PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "girocrab-win-views-{}-{:?}.sqlite",
            std::process::id(),
            std::thread::current().id()
        ));
        let vault = Vault::open(path.display().to_string(), vec![7; 32]).unwrap();
        vault.seed_demo_data().unwrap();
        (vault, path)
    }

    #[test]
    fn every_view_loads_from_the_demo_vault() {
        let (vault, path) = demo_vault();
        let accounts = vault.list_accounts().unwrap();
        let tangible = vault.list_tangible_account_ids().unwrap();
        let eur = |c: i64| money(c, "EUR");
        let date = |d: &str| german_date(d);

        for target in [
            Target::Planned,
            Target::Subscriptions,
            Target::Mandates,
            Target::Documents,
        ] {
            assert!(load(&vault, target, &accounts, &tangible).is_ok());
        }
        match load(&vault, Target::Wealth, &accounts, &tangible).unwrap() {
            Loaded::Wealth(w) => {
                let v = portfolio::wealth_view(&w, WealthTab::All, &eur);
                assert!(!v.net_worth.is_empty());
                assert!(!v.rows.is_empty());
            }
            _ => panic!("wealth expected"),
        }
        let depots: Vec<&Account> = accounts
            .iter()
            .filter(|a| a.kind == AccountKind::Brokerage || tangible.contains(&a.id))
            .collect();
        for a in depots {
            match load(&vault, Target::Account(a.id), &accounts, &tangible).unwrap() {
                Loaded::Depot(d) => {
                    let v = portfolio::depot_view(&d, Period::All, "2026-11-16", &eur, &date);
                    assert_eq!(v.kpis.len(), 4);
                }
                _ => panic!("depot expected"),
            }
        }
        assert!(load(&vault, Target::Account(-5), &accounts, &tangible).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn sidebar_builds_from_the_demo_vault() {
        let (vault, path) = demo_vault();
        let accounts = vault.list_accounts().unwrap();
        let categories = vault.list_categories(None, None).unwrap();
        let groups = vault.list_user_groups().unwrap();
        let folded = HashSet::new();
        for mode in [GroupMode::Groups, GroupMode::Banks, GroupMode::Kind] {
            let rows = nav::build(
                &SidebarInput {
                    accounts: &accounts,
                    user_groups: &groups,
                    categories: &categories,
                    planned_count: 0,
                    mandate_count: 0,
                    document_count: 0,
                    mode,
                    query: "",
                    folded: &folded,
                    privacy: false,
                },
                &|c, cur| money(c, cur),
            );
            // Every account shows up exactly once, whatever the grouping.
            let shown = rows.iter().filter(|r| r.kind == nav::kind::ACCOUNT).count();
            assert_eq!(shown, accounts.len(), "{mode:?}");
            assert_eq!(
                rows.iter()
                    .filter(|r| r.kind == nav::kind::CATEGORY)
                    .count(),
                categories.len()
            );
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rules_round_trip_through_the_vault() {
        let (vault, path) = demo_vault();
        let cats = vault.list_categories(None, None).unwrap();
        let form = RuleForm {
            name: "Test".into(),
            pattern: "TESTHAENDLER".into(),
            category_index: 0,
            field_index: 1,
            kind_index: 0,
            enabled: true,
            case_sensitive: false,
            min_amount: String::new(),
            max_amount: String::new(),
        };
        let rules = vault.list_rules().unwrap();
        let rule = lists::validate_rule(&form, None, &cats, lists::next_priority(&rules)).unwrap();
        let id = vault.save_rule(rule).unwrap();
        let saved = vault.list_rules().unwrap();
        let mine = saved.iter().find(|r| r.id == id).unwrap();
        assert_eq!(mine.pattern, "TESTHAENDLER");
        vault.delete_rule(id).unwrap();
        assert!(vault.list_rules().unwrap().iter().all(|r| r.id != id));
        let _ = std::fs::remove_file(&path);
    }
}
