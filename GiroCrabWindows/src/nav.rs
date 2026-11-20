//! Sidebar navigation of the Windows app without Slint: what a click on a sidebar row means
//! (`Target`), how accounts are grouped (`GroupMode`) and the flat row list the sidebar shows.
//! Pure functions over core records, unit-tested on every platform; `main.rs` only draws it.

use std::collections::HashSet;

use girocrab_core::{Account, AccountKind, BookingFilter, Category, UserGroup};

/// How the "Konten" section groups its accounts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GroupMode {
    #[default]
    Groups,
    Banks,
    Kind,
}

impl GroupMode {
    /// Index of the `Gruppen | Banken | Kontoart` switcher.
    pub fn from_index(i: i32) -> Self {
        match i {
            1 => Self::Banks,
            2 => Self::Kind,
            _ => Self::Groups,
        }
    }
}

/// What the main pane shows for the selected sidebar row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Target {
    #[default]
    AllAccounts,
    Account(i64),
    NewBookings,
    Wealth,
    /// Securities overview over all depots.
    Depot,
    Planned,
    /// Contracts & subscriptions (recurring payments).
    Subscriptions,
    Mandates,
    Documents,
    Uncategorized,
    Category(i64),
}

impl Target {
    /// Stable string a sidebar row carries to Slint and back.
    pub fn key(self) -> String {
        match self {
            Self::AllAccounts => "all".into(),
            Self::Account(id) => format!("acct:{id}"),
            Self::NewBookings => "new".into(),
            Self::Wealth => "wealth".into(),
            Self::Depot => "depot".into(),
            Self::Subscriptions => "abos".into(),
            Self::Planned => "planned".into(),
            Self::Mandates => "mandates".into(),
            Self::Documents => "docs".into(),
            Self::Uncategorized => "uncat".into(),
            Self::Category(id) => format!("cat:{id}"),
        }
    }

    pub fn parse(key: &str) -> Option<Self> {
        Some(match key {
            "all" => Self::AllAccounts,
            "new" => Self::NewBookings,
            "wealth" => Self::Wealth,
            "depot" => Self::Depot,
            "abos" => Self::Subscriptions,
            "planned" => Self::Planned,
            "mandates" => Self::Mandates,
            "docs" => Self::Documents,
            "uncat" => Self::Uncategorized,
            _ => {
                if let Some(id) = key.strip_prefix("acct:") {
                    Self::Account(id.parse().ok()?)
                } else {
                    Self::Category(key.strip_prefix("cat:")?.parse().ok()?)
                }
            }
        })
    }
}

/// Core filter of a booking-listing target (limit and offset are the caller's).
pub fn booking_filter(target: Target) -> BookingFilter {
    let mut f = BookingFilter::default();
    match target {
        Target::Account(id) => f.account_ids = vec![id],
        Target::NewBookings => f.checked = Some(false),
        Target::Uncategorized => f.uncategorized_only = true,
        Target::Category(id) => {
            f.category_id = Some(id);
            f.include_subcategories = true;
        }
        _ => {}
    }
    f
}

/// Row kinds as Slint sees them (`SideRow.kind`).
pub mod kind {
    pub const ITEM: i32 = 0;
    pub const SECTION: i32 = 1;
    pub const GROUP: i32 = 2;
    pub const ACCOUNT: i32 = 3;
    pub const CATEGORY: i32 = 4;
    /// Mode switcher and search of the "Konten" section (drawn by Slint, not by the row data).
    pub const CONTROLS: i32 = 5;
}

/// One line of the sidebar. `key` is a `Target` key for selectable rows and `toggle:<id>` for
/// rows that only fold or unfold (sections, groups, categories with sub-categories).
#[derive(Debug, Clone, PartialEq)]
pub struct SideRow {
    pub kind: i32,
    pub key: String,
    pub label: String,
    pub detail: String,
    pub badge: String,
    pub amount: String,
    pub negative: bool,
    /// Repayment progress of a loan 0.0..=1.0; negative = none.
    pub progress: f32,
    pub color_hex: String,
    /// Vector icon name drawn by the sidebar (empty = none).
    pub icon: String,
    pub depth: i32,
    /// `0` = nothing to fold, `1` = open, `2` = folded.
    pub fold: i32,
}

impl SideRow {
    fn new(kind: i32, key: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            kind,
            key: key.into(),
            label: label.into(),
            detail: String::new(),
            badge: String::new(),
            amount: String::new(),
            negative: false,
            progress: -1.0,
            color_hex: String::new(),
            icon: String::new(),
            depth: 0,
            fold: 0,
        }
    }
}

/// Everything the sidebar is built from.
pub struct SidebarInput<'a> {
    pub accounts: &'a [Account],
    pub user_groups: &'a [UserGroup],
    pub categories: &'a [Category],
    /// Recurring bookings found in the history.
    pub planned_count: usize,
    pub mandate_count: u32,
    /// Documents in the vault (badge of the documents row).
    pub document_count: u32,
    pub mode: GroupMode,
    pub query: &'a str,
    /// Keys (`toggle:…` suffixes) of folded sections, groups and categories.
    pub folded: &'a HashSet<String>,
    pub privacy: bool,
}

/// Share of a loan paid back: `initial` is the original amount, `balance` the open debt.
/// `None` without a known original amount.
pub fn loan_progress(initial: Option<i64>, balance_cents: i64) -> Option<f32> {
    let initial = initial.filter(|i| *i > 0)?;
    let open = balance_cents.abs().min(initial);
    Some(((initial - open) as f32 / initial as f32).clamp(0.0, 1.0))
}

/// "37 % getilgt".
pub fn progress_label(progress: f32) -> String {
    format!("{} % getilgt", (progress * 100.0).round() as i32)
}

pub fn kind_label(k: AccountKind) -> &'static str {
    match k {
        AccountKind::Checking => "Girokonten",
        AccountKind::Savings => "Sparkonten",
        AccountKind::CreditCard => "Kreditkarten",
        AccountKind::PaymentService => "Zahlungsdienste",
        AccountKind::Brokerage => "Depots",
        AccountKind::Cash => "Bargeld",
        AccountKind::Loan => "Darlehen",
    }
}

/// Order of the account kinds in "Kontoart" mode.
const KIND_ORDER: [AccountKind; 7] = [
    AccountKind::Checking,
    AccountKind::Savings,
    AccountKind::CreditCard,
    AccountKind::PaymentService,
    AccountKind::Brokerage,
    AccountKind::Cash,
    AccountKind::Loan,
];

/// Accounts that count as "new" in the badge: archived accounts are skipped by syncs.
pub fn unchecked_total(accounts: &[Account]) -> u32 {
    accounts
        .iter()
        .filter(|a| a.archived_unix.is_none())
        .map(|a| a.new_count)
        .sum()
}

fn account_matches(a: &Account, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    q.is_empty()
        || [&a.name, &a.bank_name, &a.iban, &a.account_number]
            .iter()
            .any(|f| f.to_lowercase().contains(&q))
}

/// Account groups in the order of the chosen mode: (group label, accounts).
fn grouped<'a>(
    accounts: &[&'a Account],
    groups: &[UserGroup],
    mode: GroupMode,
) -> Vec<(String, Vec<&'a Account>)> {
    let mut out: Vec<(String, Vec<&Account>)> = Vec::new();
    match mode {
        GroupMode::Groups => {
            for g in groups {
                let members: Vec<_> = accounts
                    .iter()
                    .copied()
                    .filter(|a| match &a.user_group_uuid {
                        Some(u) => *u == g.uuid && !g.is_ungrouped,
                        None => g.is_ungrouped,
                    })
                    .collect();
                if !members.is_empty() {
                    let pad = "  ".repeat(g.depth as usize);
                    out.push((format!("{pad}{}", g.name), members));
                }
            }
            // Accounts of a group the list does not know still show up.
            let known: HashSet<&str> = out
                .iter()
                .flat_map(|(_, m)| m.iter().map(|a| a.name.as_str()))
                .collect();
            let rest: Vec<_> = accounts
                .iter()
                .copied()
                .filter(|a| !known.contains(a.name.as_str()))
                .collect();
            if !rest.is_empty() {
                out.push(("Ohne Gruppe".into(), rest));
            }
        }
        GroupMode::Banks => {
            let mut banks: Vec<String> = accounts
                .iter()
                .map(|a| {
                    if a.bank_name.trim().is_empty() {
                        "Sonstige".to_string()
                    } else {
                        a.bank_name.clone()
                    }
                })
                .collect();
            banks.sort_by_key(|b| b.to_lowercase());
            banks.dedup();
            for b in banks {
                let members = accounts
                    .iter()
                    .copied()
                    .filter(|a| {
                        let name = if a.bank_name.trim().is_empty() {
                            "Sonstige"
                        } else {
                            a.bank_name.as_str()
                        };
                        name == b
                    })
                    .collect();
                out.push((b, members));
            }
        }
        GroupMode::Kind => {
            for k in KIND_ORDER {
                let members: Vec<_> = accounts.iter().copied().filter(|a| a.kind == k).collect();
                if !members.is_empty() {
                    out.push((kind_label(k).to_string(), members));
                }
            }
        }
    }
    out
}

/// Icon of an account badge by account type.
fn account_icon(kind: AccountKind) -> &'static str {
    match kind {
        AccountKind::Checking => "bank",
        AccountKind::Savings | AccountKind::Cash => "banknote",
        AccountKind::CreditCard | AccountKind::PaymentService => "card",
        AccountKind::Brokerage => "chart",
        AccountKind::Loan => "house",
    }
}

/// Icon of a group header: by well-known group names, else by account-type group.
fn group_icon(label: &str) -> &'static str {
    let l = label.to_lowercase();
    if l.contains("alltag") {
        "person"
    } else if l.contains("sparen") || l.contains("vorsorge") {
        "clock"
    } else if l.contains("haus") || l.contains("darlehen") {
        "house"
    } else if l.contains("girokonten") {
        "bank"
    } else if l.contains("sparkonten") || l.contains("bargeld") {
        "banknote"
    } else if l.contains("kreditkarten") || l.contains("zahlungsdienste") {
        "card"
    } else if l.contains("depot") {
        "chart"
    } else {
        ""
    }
}

/// Builds the whole sidebar. `money` formats cents with a currency, as everywhere in the app.
pub fn build(input: &SidebarInput, money: &dyn Fn(i64, &str) -> String) -> Vec<SideRow> {
    let hide = |text: String| {
        if input.privacy {
            crate::daily::MASK.to_string()
        } else {
            text
        }
    };
    let folded = |id: &str| input.folded.contains(id);
    let mut rows = Vec::new();

    let new_count = unchecked_total(input.accounts);
    let mut new_row = SideRow::new(kind::ITEM, Target::NewBookings.key(), "Neue Umsätze");
    new_row.icon = "inbox".into();
    if new_count > 0 {
        new_row.badge = new_count.to_string();
    }
    rows.push(new_row);

    let mut wealth = SideRow::new(kind::ITEM, Target::Wealth.key(), "Vermögen");
    let total: i64 = input
        .accounts
        .iter()
        .filter(|a| a.in_total && a.currency == "EUR" && a.archived_unix.is_none())
        .map(|a| a.balance_cents)
        .sum();
    wealth.amount = hide(money(total, "EUR"));
    wealth.icon = "pie".into();
    rows.push(wealth);

    let mut depot = SideRow::new(kind::ITEM, Target::Depot.key(), "Depot / Wertpapiere");
    depot.icon = "chart".into();
    let brokerage = input
        .accounts
        .iter()
        .filter(|a| a.kind == AccountKind::Brokerage && a.archived_unix.is_none())
        .count();
    if brokerage > 0 {
        depot.badge = brokerage.to_string();
    }
    rows.push(depot);

    // ---- Konten
    let section = |id: &str, label: &str| {
        let mut r = SideRow::new(kind::SECTION, format!("toggle:{id}"), label);
        r.fold = if folded(id) { 2 } else { 1 };
        r
    };
    rows.push(section("accounts", "Konten"));
    if !folded("accounts") {
        let mut controls = SideRow::new(kind::CONTROLS, "controls", "");
        controls.depth = 1;
        rows.push(controls);
        let mut all = SideRow::new(kind::ITEM, Target::AllAccounts.key(), "Alle Konten");
        all.depth = 1;
        all.amount = hide(money(total, "EUR"));
        all.icon = "grid".into();
        rows.push(all);
        let visible: Vec<&Account> = input
            .accounts
            .iter()
            .filter(|a| account_matches(a, input.query))
            .collect();
        for (label, members) in grouped(&visible, input.user_groups, input.mode) {
            let gid = format!("g:{}", label.trim());
            let mut head = SideRow::new(kind::GROUP, format!("toggle:{gid}"), label.trim());
            head.depth = 1 + (label.len() - label.trim_start().len()) as i32 / 2;
            // A search shows every match, folded or not.
            head.fold = if input.query.trim().is_empty() && folded(&gid) {
                2
            } else {
                1
            };
            let is_folded = head.fold == 2;
            // group figures are overall money sums (#337): only effectively
            // included accounts count; every account row stays visible
            let eur: Vec<i64> = members
                .iter()
                .filter(|a| a.currency == "EUR" && a.in_total)
                .map(|a| a.balance_cents)
                .collect();
            if !eur.is_empty() {
                let sum: i64 = eur.iter().sum();
                head.amount = hide(money(sum, "EUR"));
                head.negative = sum < 0 && !input.privacy;
            }
            head.icon = group_icon(label.trim()).into();
            rows.push(head);
            if is_folded {
                continue;
            }
            for a in members {
                let mut r = SideRow::new(kind::ACCOUNT, Target::Account(a.id).key(), &a.name);
                r.depth = 2;
                r.detail = a.bank_name.clone();
                r.amount = hide(money(a.balance_cents, &a.currency));
                r.negative = a.balance_cents < 0 && !input.privacy;
                r.color_hex = a.color_hex.clone();
                r.icon = account_icon(a.kind).into();
                if a.new_count > 0 {
                    r.badge = a.new_count.to_string();
                }
                if a.kind == AccountKind::Loan
                    && let Some(p) = loan_progress(a.initial_balance_cents, a.balance_cents)
                {
                    r.progress = p;
                    r.detail = progress_label(p);
                }
                rows.push(r);
            }
        }
    }

    // ---- Zahlungen
    rows.push(section("payments", "Zahlungen"));
    if !folded("payments") {
        let mut planned = SideRow::new(kind::ITEM, Target::Planned.key(), "Geplant");
        planned.depth = 1;
        planned.badge = input.planned_count.to_string();
        planned.icon = "calendar".into();
        rows.push(planned);
        let mut subs = SideRow::new(kind::ITEM, Target::Subscriptions.key(), "Verträge & Abos");
        subs.depth = 1;
        subs.badge = input.planned_count.to_string();
        subs.icon = "calendar".into();
        rows.push(subs);
        let mut debits = SideRow::new(kind::ITEM, Target::Mandates.key(), "Lastschriften");
        debits.depth = 1;
        debits.badge = input.mandate_count.to_string();
        debits.icon = "arrow-in".into();
        rows.push(debits);
    }

    // ---- Dokumente
    rows.push(section("documents", "Dokumente"));
    if !folded("documents") {
        let mut docs = SideRow::new(kind::ITEM, Target::Documents.key(), "Belege & PostBox");
        docs.depth = 1;
        docs.icon = "document".into();
        if input.document_count > 0 {
            docs.badge = input.document_count.to_string();
        }
        rows.push(docs);
    }

    // ---- Kategorien
    rows.push(section("categories", "Kategorien"));
    if !folded("categories") {
        let mut none = SideRow::new(kind::ITEM, Target::Uncategorized.key(), "Ohne Kategorie");
        none.depth = 1;
        let open: u32 = input
            .accounts
            .iter()
            .filter(|a| a.archived_unix.is_none())
            .map(|a| a.uncategorized_count)
            .sum();
        if open > 0 {
            none.badge = open.to_string();
        }
        none.icon = "folder".into();
        rows.push(none);
        let mut skip_below: Option<u32> = None;
        for (i, c) in input.categories.iter().enumerate() {
            if let Some(d) = skip_below {
                if c.depth > d {
                    continue;
                }
                skip_below = None;
            }
            let has_children = input
                .categories
                .get(i + 1)
                .is_some_and(|n| n.depth > c.depth);
            let cid = format!("c:{}", c.id);
            let mut r = SideRow::new(kind::CATEGORY, Target::Category(c.id).key(), &c.name);
            r.depth = 1 + c.depth as i32;
            r.color_hex = c.color_hex.clone();
            r.amount = hide(money(c.total_cents, "EUR"));
            r.negative = c.total_cents < 0 && !input.privacy;
            if c.transaction_count > 0 {
                r.badge = c.transaction_count.to_string();
            }
            if has_children {
                r.fold = if folded(&cid) { 2 } else { 1 };
                if r.fold == 2 {
                    skip_below = Some(c.depth);
                }
            }
            rows.push(r);
        }
    }
    rows
}

/// Id a `toggle:` key folds (`accounts`, `g:Bank`, `c:5`).
pub fn toggle_id(key: &str) -> Option<&str> {
    key.strip_prefix("toggle:")
}

/// Fold id of a category row, so a click on its chevron folds the sub-tree.
pub fn category_fold_id(key: &str) -> Option<String> {
    match Target::parse(key)? {
        Target::Category(id) => Some(format!("c:{id}")),
        _ => None,
    }
}

/// Moves `target_id` one place up (before its predecessor) or down (after its
/// successor) in `ids`. Returns `false` when nothing changed (id missing or
/// already at the edge).
#[allow(dead_code)] // not wired into the UI yet; covered by tests
pub fn move_category_in_order(ids: &mut [i64], target_id: i64, move_up: bool) -> bool {
    let Some(pos) = ids.iter().position(|&id| id == target_id) else {
        return false;
    };
    let other = if move_up {
        match pos.checked_sub(1) {
            Some(p) => p,
            None => return false,
        }
    } else {
        pos + 1
    };
    if other >= ids.len() {
        return false;
    }
    ids.swap(pos, other);
    true
}

/// Sorts `items` by the user-defined order `ordered_ids`. Items whose id is not
/// listed keep their relative order and follow the listed ones (stable sort).
#[allow(dead_code)] // not wired into the UI yet; covered by tests
pub fn apply_custom_category_order<T, F: Fn(&T) -> i64>(
    items: &mut [T],
    ordered_ids: &[i64],
    id_fn: F,
) {
    items.sort_by_key(|item| {
        let id = id_fn(item);
        ordered_ids
            .iter()
            .position(|&o| o == id)
            .unwrap_or(usize::MAX)
    });
}

/// Left indent in pixels for a sidebar row at hierarchy `depth`, so
/// subcategory icons align under the parent category label text start.
#[allow(dead_code)] // not wired into the UI yet; covered by tests
pub fn sidebar_hierarchy_indent_px(depth: u32, compact_mode: bool) -> i32 {
    depth as i32 * if compact_mode { 16 } else { 22 }
}

/// Fixed sidebar width in pixels (#600): the structure no longer resizes with its content.
#[allow(dead_code)] // not wired into the UI yet; covered by tests
pub const SIDEBAR_FIXED_WIDTH_PX: i32 = 256;

/// Whether `section` (the id behind a `toggle:<id>` section key) is one of the sidebar's
/// fixed top-level sections, which keep their order and cannot be moved or removed (#600).
#[allow(dead_code)] // not wired into the UI yet; covered by tests
pub fn is_sidebar_section_pinned(section: &str) -> bool {
    matches!(
        section,
        "accounts" | "payments" | "documents" | "categories"
    )
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    pub fn account(id: i64, name: &str, bank: &str, kind: AccountKind, balance: i64) -> Account {
        Account {
            id,
            connection_id: None,
            connector: None,
            group_id: 0,
            name: name.into(),
            bank_name: bank.into(),
            owner: String::new(),
            iban: format!("DE00{id}"),
            account_number: String::new(),
            currency: "EUR".into(),
            kind,
            balance_cents: balance,
            balance_date: String::new(),
            sf_symbol: String::new(),
            sort_order: 0,
            transaction_count: 0,
            uncategorized_count: 0,
            new_count: 0,
            initial_balance_cents: None,
            user_group_uuid: None,
            include_in_total: true,
            in_total: true,
            color_hex: String::new(),
            archived_unix: None,
            access_revoked: false,
            opening_balance_cents: None,
            opening_balance_date: String::new(),
            balance_derived: false,
            note: String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn money(c: i64, cur: &str) -> String {
        format!("{c} {cur}")
    }

    use super::testing::account;

    #[test]
    fn sidebar_hierarchy_indent_scales_with_depth() {
        assert_eq!(sidebar_hierarchy_indent_px(0, false), 0);
        assert_eq!(sidebar_hierarchy_indent_px(0, true), 0);
        assert_eq!(sidebar_hierarchy_indent_px(1, false), 22);
        assert_eq!(sidebar_hierarchy_indent_px(1, true), 16);
        assert_eq!(sidebar_hierarchy_indent_px(3, false), 66);
        assert_eq!(sidebar_hierarchy_indent_px(3, true), 48);
    }

    #[test]
    fn custom_category_order_sorts_listed_first_and_keeps_unlisted_stable() {
        let mut items = vec![(1, "a"), (2, "b"), (3, "c"), (4, "d"), (5, "e")];
        apply_custom_category_order(&mut items, &[4, 2, 99], |i| i.0);
        let ids: Vec<i64> = items.iter().map(|i| i.0).collect();
        assert_eq!(ids, vec![4, 2, 1, 3, 5]);
    }

    fn category(id: i64, parent: Option<i64>, name: &str, depth: u32, total: i64) -> Category {
        Category {
            id,
            parent_id: parent,
            name: name.into(),
            sf_symbol: String::new(),
            color_hex: String::new(),
            sort_order: 0,
            depth,
            path: name.into(),
            own_total_cents: total,
            total_cents: total,
            transaction_count: 1,
            budget_cents: None,
        }
    }

    fn input<'a>(
        accounts: &'a [Account],
        categories: &'a [Category],
        folded: &'a HashSet<String>,
        mode: GroupMode,
        query: &'a str,
    ) -> SidebarInput<'a> {
        SidebarInput {
            accounts,
            user_groups: &[],
            categories,
            planned_count: 4,
            mandate_count: 2,
            document_count: 3,
            mode,
            query,
            folded,
            privacy: false,
        }
    }

    fn keys(rows: &[SideRow]) -> Vec<&str> {
        rows.iter().map(|r| r.key.as_str()).collect()
    }

    #[test]
    fn target_keys_round_trip() {
        for t in [
            Target::AllAccounts,
            Target::Account(7),
            Target::NewBookings,
            Target::Wealth,
            Target::Depot,
            Target::Subscriptions,
            Target::Planned,
            Target::Mandates,
            Target::Documents,
            Target::Uncategorized,
            Target::Category(12),
        ] {
            assert_eq!(Target::parse(&t.key()), Some(t));
        }
        assert_eq!(Target::parse("acct:x"), None);
        assert_eq!(Target::parse("toggle:accounts"), None);
    }

    #[test]
    fn booking_filters_follow_the_target() {
        assert!(booking_filter(Target::AllAccounts).account_ids.is_empty());
        assert_eq!(booking_filter(Target::Account(4)).account_ids, vec![4]);
        assert_eq!(booking_filter(Target::NewBookings).checked, Some(false));
        assert!(booking_filter(Target::Uncategorized).uncategorized_only);
        let c = booking_filter(Target::Category(9));
        assert_eq!(c.category_id, Some(9));
        assert!(c.include_subcategories);
        assert_eq!(booking_filter(Target::Category(9)).checked, None);
    }

    #[test]
    fn group_mode_follows_switcher_index() {
        assert_eq!(GroupMode::from_index(0), GroupMode::Groups);
        assert_eq!(GroupMode::from_index(1), GroupMode::Banks);
        assert_eq!(GroupMode::from_index(2), GroupMode::Kind);
        assert_eq!(GroupMode::from_index(9), GroupMode::Groups);
    }

    #[test]
    fn loan_progress_is_share_repaid() {
        assert_eq!(loan_progress(Some(100_000), -75_000), Some(0.25));
        assert_eq!(loan_progress(Some(100_000), 0), Some(1.0));
        // More debt than the original amount never gives a negative progress.
        assert_eq!(loan_progress(Some(100_000), -120_000), Some(0.0));
        assert_eq!(loan_progress(None, -5), None);
        assert_eq!(loan_progress(Some(0), -5), None);
        assert_eq!(progress_label(0.374), "37 % getilgt");
    }

    #[test]
    fn top_items_carry_the_unchecked_badge() {
        let mut a = account(1, "Giro", "Bank A", AccountKind::Checking, 100);
        a.new_count = 3;
        let mut b = account(2, "Alt", "Bank A", AccountKind::Checking, 0);
        b.new_count = 9;
        b.archived_unix = Some(1);
        let accounts = [a, b];
        let folded = HashSet::new();
        let rows = build(
            &input(&accounts, &[], &folded, GroupMode::Banks, ""),
            &money,
        );
        assert_eq!(rows[0].key, "new");
        assert_eq!(rows[0].badge, "3");
        assert_eq!(rows[1].key, "wealth");
        // No unchecked bookings: no badge at all (never "0").
        let none = [account(1, "Giro", "Bank A", AccountKind::Checking, 100)];
        let rows = build(&input(&none, &[], &folded, GroupMode::Banks, ""), &money);
        assert_eq!(rows[0].badge, "");
    }

    #[test]
    fn accounts_group_by_bank_and_kind() {
        let accounts = [
            account(1, "Giro", "Zeta Bank", AccountKind::Checking, 1),
            account(2, "Tagesgeld", "alpha Bank", AccountKind::Savings, 2),
            account(3, "Depot", "Zeta Bank", AccountKind::Brokerage, 3),
        ];
        let folded = HashSet::new();
        let banks = build(
            &input(&accounts, &[], &folded, GroupMode::Banks, ""),
            &money,
        );
        let heads: Vec<&str> = banks
            .iter()
            .filter(|r| r.kind == kind::GROUP)
            .map(|r| r.label.as_str())
            .collect();
        assert_eq!(heads, ["alpha Bank", "Zeta Bank"]);
        let kinds = build(&input(&accounts, &[], &folded, GroupMode::Kind, ""), &money);
        let heads: Vec<&str> = kinds
            .iter()
            .filter(|r| r.kind == kind::GROUP)
            .map(|r| r.label.as_str())
            .collect();
        assert_eq!(heads, ["Girokonten", "Sparkonten", "Depots"]);
    }

    #[test]
    fn user_groups_hold_their_accounts() {
        let mut a = account(1, "Giro", "A", AccountKind::Checking, 1);
        a.user_group_uuid = Some("g1".into());
        let b = account(2, "Bar", "A", AccountKind::Cash, 1);
        let group = |uuid: &str, name: &str, ungrouped: bool| UserGroup {
            uuid: uuid.into(),
            name: name.into(),
            sf_symbol: String::new(),
            color_hex: String::new(),
            sort_order: 0,
            include_in_total: true,
            in_total: true,
            is_ungrouped: ungrouped,
            parent_uuid: None,
            depth: 0,
            balance_cents: 0,
            accounts: vec![],
        };
        let groups = [group("g1", "Privat", false), group("", "Ohne Gruppe", true)];
        let accounts = [a, b];
        let folded = HashSet::new();
        let mut inp = input(&accounts, &[], &folded, GroupMode::Groups, "");
        inp.user_groups = &groups;
        let rows = build(&inp, &money);
        let labels: Vec<&str> = rows
            .iter()
            .filter(|r| matches!(r.kind, kind::GROUP | kind::ACCOUNT))
            .map(|r| r.label.as_str())
            .collect();
        assert_eq!(labels, ["Privat", "Giro", "Ohne Gruppe", "Bar"]);
    }

    #[test]
    fn search_filters_accounts_and_ignores_folding() {
        let accounts = [
            account(1, "Giro", "Bank A", AccountKind::Checking, 1),
            account(2, "Tagesgeld", "Bank B", AccountKind::Savings, 2),
        ];
        let mut folded = HashSet::new();
        folded.insert("g:Bank B".to_string());
        let rows = build(
            &input(&accounts, &[], &folded, GroupMode::Banks, "tages"),
            &money,
        );
        let accts: Vec<&str> = rows
            .iter()
            .filter(|r| r.kind == kind::ACCOUNT)
            .map(|r| r.label.as_str())
            .collect();
        assert_eq!(accts, ["Tagesgeld"]);
    }

    #[test]
    fn loan_rows_show_repayment_progress() {
        let mut loan = account(1, "Baukredit", "Bank", AccountKind::Loan, -60_000);
        loan.initial_balance_cents = Some(100_000);
        let accounts = [loan];
        let folded = HashSet::new();
        let rows = build(&input(&accounts, &[], &folded, GroupMode::Kind, ""), &money);
        let r = rows.iter().find(|r| r.kind == kind::ACCOUNT).unwrap();
        assert!((r.progress - 0.4).abs() < 1e-6);
        assert_eq!(r.detail, "40 % getilgt");
    }

    #[test]
    fn payment_and_document_sections_fold() {
        let accounts = [account(1, "Giro", "A", AccountKind::Checking, 1)];
        let mut folded = HashSet::new();
        let open = build(
            &input(&accounts, &[], &folded, GroupMode::Banks, ""),
            &money,
        );
        let planned = open.iter().find(|r| r.key == "planned").unwrap();
        assert_eq!(planned.badge, "4");
        assert_eq!(
            open.iter().find(|r| r.key == "mandates").unwrap().badge,
            "2"
        );
        assert!(keys(&open).contains(&"docs"));
        folded.insert("payments".into());
        folded.insert("documents".into());
        let closed = build(
            &input(&accounts, &[], &folded, GroupMode::Banks, ""),
            &money,
        );
        assert!(!keys(&closed).contains(&"planned"));
        assert!(!keys(&closed).contains(&"docs"));
        assert!(keys(&closed).contains(&"toggle:payments"));
    }

    #[test]
    fn category_tree_shows_totals_and_folds_subtrees() {
        let cats = [
            category(1, None, "Haushalt", 0, -5000),
            category(2, Some(1), "Strom", 1, -3000),
            category(3, Some(1), "Miete", 1, -2000),
            category(4, None, "Gehalt", 0, 9000),
        ];
        let accounts = [account(1, "Giro", "A", AccountKind::Checking, 1)];
        let mut folded = HashSet::new();
        let rows = build(
            &input(&accounts, &cats, &folded, GroupMode::Banks, ""),
            &money,
        );
        let tree: Vec<&SideRow> = rows.iter().filter(|r| r.kind == kind::CATEGORY).collect();
        assert_eq!(tree.len(), 4);
        assert_eq!(tree[0].amount, "-5000 EUR");
        assert!(tree[0].negative);
        assert_eq!(tree[0].fold, 1);
        assert_eq!(tree[1].fold, 0);
        assert_eq!(tree[1].depth, tree[0].depth + 1);
        assert_eq!(tree[3].amount, "9000 EUR");
        folded.insert(category_fold_id("cat:1").unwrap());
        let rows = build(
            &input(&accounts, &cats, &folded, GroupMode::Banks, ""),
            &money,
        );
        let tree: Vec<&str> = rows
            .iter()
            .filter(|r| r.kind == kind::CATEGORY)
            .map(|r| r.label.as_str())
            .collect();
        assert_eq!(tree, ["Haushalt", "Gehalt"]);
    }

    #[test]
    fn privacy_hides_amounts_but_not_labels() {
        let accounts = [account(1, "Giro", "A", AccountKind::Checking, 1234)];
        let cats = [category(1, None, "Haushalt", 0, -5000)];
        let folded = HashSet::new();
        let mut inp = input(&accounts, &cats, &folded, GroupMode::Banks, "");
        inp.privacy = true;
        let rows = build(&inp, &money);
        for r in rows.iter().filter(|r| !r.amount.is_empty()) {
            assert_eq!(r.amount, crate::daily::MASK);
            assert!(!r.negative);
        }
        assert!(rows.iter().any(|r| r.label == "Haushalt"));
    }

    #[test]
    fn toggle_keys_are_recognised() {
        assert_eq!(toggle_id("toggle:payments"), Some("payments"));
        assert_eq!(toggle_id("acct:1"), None);
        assert_eq!(category_fold_id("acct:1"), None);
    }

    #[test]
    fn test_move_category_in_order() {
        let mut ids = vec![1, 2, 3];
        assert!(move_category_in_order(&mut ids, 2, true));
        assert_eq!(ids, [2, 1, 3]);
        assert!(move_category_in_order(&mut ids, 2, false));
        assert_eq!(ids, [1, 2, 3]);
        // Edges and unknown ids leave the order untouched.
        assert!(!move_category_in_order(&mut ids, 1, true));
        assert!(!move_category_in_order(&mut ids, 3, false));
        assert!(!move_category_in_order(&mut ids, 9, true));
        assert_eq!(ids, [1, 2, 3]);
        let mut empty: Vec<i64> = Vec::new();
        assert!(!move_category_in_order(&mut empty, 1, false));
    }

    #[test]
    fn test_sidebar_fixed_width_and_pinned_structure() {
        assert_eq!(SIDEBAR_FIXED_WIDTH_PX, 256);
        for id in ["accounts", "payments", "documents", "categories"] {
            assert!(is_sidebar_section_pinned(id), "{id}");
        }
        assert!(!is_sidebar_section_pinned("g:Bank"));
        assert!(!is_sidebar_section_pinned("c:5"));
        assert!(!is_sidebar_section_pinned(""));
        // Every section the builder emits is pinned.
        let accounts = [testing::account(1, "Giro", "A", AccountKind::Checking, 0)];
        let folded = HashSet::new();
        let rows = build(
            &input(&accounts, &[], &folded, GroupMode::Banks, ""),
            &money,
        );
        for r in rows.iter().filter(|r| r.kind == kind::SECTION) {
            let id = toggle_id(&r.key).unwrap();
            assert!(is_sidebar_section_pinned(id), "{}", r.key);
        }
    }
}
