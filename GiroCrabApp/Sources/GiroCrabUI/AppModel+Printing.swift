import Foundation
import SwiftUI
import GiroCrabCore

extension AppModel {
    // MARK: - Printing (GitHub #53)

    /// Everything the list shows (account, period, search, scope, status) without the list limit.
    func printSource() -> PrintReport? {
        guard let vault else { return nil }
        var filter = currentFilter
        filter.limit = 0
        do {
            let rows = try vault.listTransactions(filter: filter).transactions.map(PrintRow.init)
            let accounts = allAccounts.map {
                PrintAccount(id: $0.id, name: $0.name, iban: $0.iban, balanceCents: $0.balanceCents,
                             balanceDate: $0.balanceDate, currency: $0.currency)
            }
            var report = PrintReport(title: title, periodLabel: periodTitle, rows: rows, accounts: accounts)
            // The list's own income/expense filter is named in the printout header.
            switch directionFilter {
            case .all: break
            case .income: report.filterNotes = [PrintDirection.income.title]
            case .expense: report.filterNotes = [PrintDirection.expenses.title]
            }
            return report
        } catch {
            showError(String(localized: "Drucken nicht möglich"), error.localizedDescription)
            return nil
        }
    }

    /// End-of-day balances since `from` for each account (key = id) and combined (key 0).
    func printDailyBalances(accountIds: [Int64], from: String) -> [Int64: [String: Int64]] {
        guard let vault, !accountIds.isEmpty else { return [:] }
        let today = Calendar.current.startOfDay(for: Date())
        let start = DateText.date(from) ?? today
        let days = UInt32(max(1, (Calendar.current.dateComponents([.day], from: start, to: today).day ?? 0) + 1))
        func map(_ ids: [Int64]) -> [String: Int64] {
            let points = (try? vault.balanceHistory(accountIds: ids, days: days)) ?? []
            return Dictionary(points.map { ($0.date, $0.balanceCents) }, uniquingKeysWith: { _, b in b })
        }
        var out: [Int64: [String: Int64]] = [0: map(accountIds)]
        for id in accountIds { out[id] = map([id]) }
        return out
    }
}
