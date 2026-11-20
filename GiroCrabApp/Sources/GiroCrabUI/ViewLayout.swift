import SwiftUI

/// Persisted view-layout switches shared by the View menu, the toolbar and
/// the settings (GitHub #233, #234).
extension UserDefaults {
    /// `ViewLayout.runningBalanceKey` for KVO: the model reloads when it changes (#577).
    @objc dynamic var showRunningBalance: Bool { bool(forKey: ViewLayout.runningBalanceKey) }
}

enum ViewLayout {
    /// Charts and analysis cards above the booking list.
    static let statisticsKey = "showStatistics"
    /// Right-hand inspector column.
    static let inspectorKey = "showInspector"
    /// Monthly budgets: the card above the list and „Monatsbudget festlegen …“.
    static let budgetsKey = "showBudgets"
    /// Balance after each booking and at the end of each day in the list (#577).
    static let runningBalanceKey = "showRunningBalance"
    /// Header card reduced to title, balance and the filter row: no balance
    /// chart, KPIs or loan progress, more room for the bookings (GitHub #358).
    static let compactHeaderKey = "compactHeader"
    /// Cashflow / monthly budget cards folded to a one-line header (GitHub #603).
    static let cashflowCollapsedKey = "cashflowCollapsed"
    static let budgetsCollapsedKey = "budgetsCollapsed"
    /// „Abgerufen vor …“ under the balance of a single account; off by default (#815).
    static let lastFetchKey = "showLastFetch"
    /// Header chart folded to its title row; balance, KPIs and filters stay (#634).
    static let headerChartCollapsedKey = "headerChartCollapsed"
    /// Shared card spacing so the three cards above the list line up (GitHub #603).
    static let cardPadding: CGFloat = 12
    static let cardSpacing: CGFloat = 8
    /// Outer gap between the stacked cards above the booking list (demo banner, header, cashflow, budgets, first day).
    static let cardGap: CGFloat = 4
    /// Top/bottom inset of a folded card: one compact row.
    static let collapsedCardVerticalPadding: CGFloat = 4
    static func cardVerticalPadding(collapsed: Bool) -> CGFloat { collapsed ? collapsedCardVerticalPadding : cardPadding }

    /// Budgets show until switched off (Ansicht › Budgets anzeigen).
    static func showsBudgets(_ defaults: UserDefaults = AppDefaults.store) -> Bool {
        defaults.object(forKey: budgetsKey) as? Bool ?? true
    }

    /// Which cards go above the list where analyses show (overview, or an
    /// account with „Auswertungen“ opened): each follows its own switch, so
    /// the budgets stay when the analyses are off.
    static func cards(statistics: Bool, budgets: Bool) -> (cashflow: Bool, budgets: Bool, any: Bool) {
        (statistics, budgets, statistics || budgets)
    }

    /// The header shows in full until minimised (Ansicht › Kopfbereich minimieren).
    static func compactHeader(_ defaults: UserDefaults = AppDefaults.store) -> Bool {
        defaults.bool(forKey: compactHeaderKey)
    }

    /// Accounts that hide the category chip in booking rows (GitHub #336).
    static let hiddenCategoryChipAccountsKey = "hiddenCategoryChipAccounts"

    /// Parses the stored value: account ids, comma-separated.
    static func hiddenCategoryChipAccounts(raw: String) -> Set<Int64> {
        Set(raw.split(separator: ",").compactMap { Int64($0) })
    }
}
