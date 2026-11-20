import Foundation
import GiroCrabCore

/// Logic behind the optional menu bar item (GiroCrab/GiroCrab#546): which
/// accounts it shows, which one is in the title, and when nothing personal may
/// be drawn at all. Pure functions, so the privacy rule is unit-tested.
enum MenuBarSummary {
    static let enabledKey = "menuBar.enabled"
    static let privateKey = "menuBar.private"
    static let accountsKey = "menuBar.accounts"
    static let focusKey = "menuBar.focus"
    /// `focusKey` value for the sum of the chosen accounts (the default).
    static let totalFocus = "total"
    static let barDisplayKey = "menuBar.barDisplay"

    /// What the menu bar item itself shows next to the crab icon. The popover
    /// always shows the amount (unless locked or private).
    enum BarDisplay: String, CaseIterable, Identifiable {
        /// Crab and the balance (default).
        case amount
        /// Crab only: the amount is in the popover.
        case iconOnly
        var id: String { rawValue }
    }

    /// What the menu bar title and the popover may show.
    enum Visibility: Equatable {
        /// Vault locked: only "Tresor gesperrt".
        case locked
        /// Private mode: no amounts and no account names.
        case covered
        case visible
    }

    static func visibility(isUnlocked: Bool, privateMode: Bool) -> Visibility {
        if !isUnlocked { return .locked }
        return privateMode ? .covered : .visible
    }

    /// Account IDs are stored comma separated in one `@AppStorage` string.
    static func decode(_ raw: String) -> [Int64] {
        raw.split(separator: ",").compactMap { Int64($0) }
    }

    static func encode(_ ids: some Sequence<Int64>) -> String {
        ids.sorted().map(String.init).joined(separator: ",")
    }

    /// The chosen accounts in the app's account order; archived ones and IDs
    /// that no longer exist drop out.
    static func selectedAccounts(_ accounts: [Account], raw: String) -> [Account] {
        let ids = Set(decode(raw))
        return accounts.filter { ids.contains($0.id) && !$0.isArchived }
    }

    /// The account named by `focus`, nil for the sum (or an account no longer chosen).
    static func focusedAccount(in selected: [Account], focus: String) -> Account? {
        guard let id = Int64(focus) else { return nil }
        return selected.first { $0.id == id }
    }

    static func currencyCode(_ account: Account) -> String {
        account.currency.isEmpty ? "EUR" : account.currency.uppercased()
    }

    /// Sum of the given accounts, one amount per currency (other currencies
    /// first, euro last, like `AllAccountsTotal`).
    static func totals(of accounts: [Account]) -> [(currency: String, cents: Int64)] {
        var sums: [String: Int64] = [:]
        for a in accounts { sums[currencyCode(a), default: 0] += a.balanceCents }
        return sums
            .map { (currency: $0.key, cents: $0.value) }
            .sorted { ($0.currency == "EUR" ? 1 : 0, $0.currency) < ($1.currency == "EUR" ? 1 : 0, $1.currency) }
    }

    /// Red amounts: the balance is below zero. A sum over several currencies has no sign.
    static func isNegative(_ account: Account) -> Bool { account.balanceCents < 0 }

    static func totalIsNegative(of accounts: [Account]) -> Bool {
        let sums = totals(of: accounts)
        return sums.count == 1 && sums[0].cents < 0
    }

    static func totalText(of accounts: [Account]) -> String {
        totals(of: accounts).map { Money.format($0.cents, currency: $0.currency) }.joined(separator: " · ")
    }

    static func balanceText(_ account: Account) -> String {
        Money.format(account.balanceCents, currency: currencyCode(account))
    }

    /// The focus after `step` places in the ring „Gesamtsumme, Konto 1, Konto 2 …“
    /// (wraps around). A focus on an account that is no longer chosen counts as the sum.
    static func cycle(focus: String, selected: [Account], step: Int) -> String {
        let ring = [totalFocus] + selected.map { String($0.id) }
        let current = focusedAccount(in: selected, focus: focus).map { String($0.id) } ?? totalFocus
        let i = ring.firstIndex(of: current) ?? 0
        return ring[((i + step) % ring.count + ring.count) % ring.count]
    }

    /// Latest fetch of the logins behind `accounts`, nil if none ever ran.
    static func lastUpdate(of accounts: [Account], connections: [BankConnection]) -> Date? {
        let ids = Set(accounts.compactMap(\.connectionId))
        return connections.filter { ids.contains($0.id) }
            .compactMap(\.lastSyncUnix).max()
            .map { Date(timeIntervalSince1970: TimeInterval($0)) }
    }

    /// „22:15“ for today, „15.11.26, 22:15“ for an earlier day (the user's locale).
    static func updatedText(_ date: Date, now: Date = Date(), calendar: Calendar = .current,
                            locale: Locale = .current) -> String {
        let f = DateFormatter()
        f.locale = locale
        f.timeZone = calendar.timeZone
        f.timeStyle = .short
        f.dateStyle = calendar.isDate(date, inSameDayAs: now) ? .none : .short
        return f.string(from: date)
    }

    /// Text next to the menu bar icon. Locked or private: never an amount or a
    /// name; locked shows nothing at all. „Nur Symbol“: nothing either.
    static func title(_ visibility: Visibility, selected: [Account], focus: String,
                      display: BarDisplay = .amount) -> String {
        if display == .iconOnly { return "" }
        switch visibility {
        // locked: the crab alone (dimmed); the popover says why
        case .locked: return ""
        case .covered: return "•••"
        case .visible:
            guard !selected.isEmpty else { return "" }
            if let account = focusedAccount(in: selected, focus: focus) { return balanceText(account) }
            return totalText(of: selected)
        }
    }
}

/// Read by the macOS executable (`AppDelegate`): with the menu bar item on, the
/// app keeps running after the last window is closed, so the item stays.
public enum MenuBarKeepAlive {
    public static var isEnabled: Bool { AppDefaults.store.bool(forKey: MenuBarSummary.enabledKey) }
}

public enum DockKeepAlive {
    static let enabledKey = "keepAppOpenInDock"

    public static var isEnabled: Bool { UserDefaults.standard.bool(forKey: enabledKey) }
}
