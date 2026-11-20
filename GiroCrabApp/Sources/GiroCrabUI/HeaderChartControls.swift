import SwiftUI
import Charts
import GiroCrabCore

/// Chart controls in the row below the header chart (#634): hidden, line, bars or pie in one
/// group, then the bar width. „Ausgeblendet“ folds only the chart (the row, balance, KPIs and
/// filters stay) and keeps the chosen type for when the chart is shown again.
struct HeaderChartControls<Unit: View>: View {
    @Binding var collapsed: Bool
    @ViewBuilder var unitMenu: Unit
    @AppStorage(BalanceChartStyle.defaultsKey) private var style: BalanceChartStyle = .line

    private enum Choice: Hashable {
        case hidden
        case style(BalanceChartStyle)
    }

    private var selection: Binding<Choice> {
        Binding(get: { collapsed ? .hidden : .style(style) }, set: { choice in
            withAnimation(Motion.animation(.easeInOut(duration: 0.25))) {
                switch choice {
                case .hidden: collapsed = true
                case .style(let s): style = s; collapsed = false
                }
            }
        })
    }

    var body: some View {
        HStack(spacing: 8) {
            Picker("Diagramm", selection: selection) {
                Image(systemName: "eye.slash")
                    .accessibilityLabel("Ausgeblendet")
                    .help("Ausgeblendet")
                    .tag(Choice.hidden)
                ForEach(BalanceChartStyle.allCases) { s in
                    Image(systemName: s.systemImage)
                        .accessibilityLabel(s.title)
                        .help(s.title)
                        .tag(Choice.style(s))
                }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .controlSize(.small)
            .fixedSize()
            .help("Diagrammart")
            if !collapsed { unitMenu }
            Spacer(minLength: 0)
        }
        .appFont(.caption)
        .foregroundStyle(Theme.secondary)
    }
}

/// Bar width of the header chart (#634).
struct BarUnitMenu: View {
    @Binding var selection: BalanceBarUnit
    let units: [BalanceBarUnit]
    /// What `.automatic` stands for right now (shown in the label).
    var resolved: BalanceBarUnit? = nil

    private var label: String {
        resolved.map { String(localized: "Auto: \($0.title)") } ?? selection.title
    }

    var body: some View {
        Menu {
            Picker("Balken je", selection: $selection) {
                ForEach(units) { Text($0.title).tag($0) }
            }
            .pickerStyle(.inline)
        } label: {
            Text(label).appFont(.caption2).foregroundStyle(Theme.secondary)
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.visible)
        .fixedSize()
        .help("Balken je")
        .accessibilityLabel("Balken je")
        .accessibilityValue(label)
    }
}

/// The chart types as an inline picker (right click on a chart).
struct ChartStyleInlinePicker: View {
    @AppStorage(BalanceChartStyle.defaultsKey) private var style: BalanceChartStyle = .line

    var body: some View {
        Picker("Darstellung", selection: $style) {
            ForEach(BalanceChartStyle.allCases) { Text($0.title).tag($0) }
        }
        .pickerStyle(.inline)
    }
}

/// Slices of the header pie (#634).
enum HeaderPieSlices {
    struct Part: Equatable {
        /// Category id as text; "own" = bookings directly in the selected category.
        let id: String
        let label: String
        /// Positive share (expenses as magnitude).
        let cents: Int64
    }

    /// Shares of a category's direct subcategories (incl. their own children) plus the bookings
    /// directly in the category. Only parts on the side of the category's total count, so a
    /// refund in an expense category does not show as a share.
    static func subcategories(of id: Int64, in categories: [GiroCrabCore.Category]) -> [Part] {
        guard let parent = categories.first(where: { $0.id == id }) else { return [] }
        let sign: Int64 = parent.totalCents < 0 ? -1 : 1
        var parts = categories.filter { $0.parentId == id }
            .map { Part(id: "\($0.id)", label: $0.name, cents: $0.totalCents * sign) }
        parts.append(Part(id: "own", label: String(localized: "Direkt: \(parent.name)"), cents: parent.ownTotalCents * sign))
        return parts.filter { $0.cents > 0 }
    }

    /// Expenses by top-level category (totals include the subcategories).
    static func expenses(_ categories: [GiroCrabCore.Category]) -> [Part] {
        categories.filter { $0.depth == 0 && $0.totalCents < 0 }
            .map { Part(id: "\($0.id)", label: $0.name, cents: -$0.totalCents) }
    }

    /// Largest first; beyond `limit` slices the rest is folded into „Sonstige“.
    static func fold(_ parts: [Part], limit: Int) -> [WealthSlice] {
        let sorted = parts.sorted { $0.cents != $1.cents ? $0.cents > $1.cents : $0.label < $1.label }
        let head = sorted.count > limit ? Array(sorted.prefix(max(limit - 1, 0))) : sorted
        var out = head.enumerated().map { i, p in
            WealthSlice(id: p.id, label: p.label, cents: p.cents, color: WealthPalette.color(i))
        }
        let rest = sorted.dropFirst(head.count).reduce(Int64(0)) { $0 + $1.cents }
        if rest > 0 { out.append(WealthSlice(id: "rest", label: String(localized: "Sonstige"), cents: rest, color: WealthPalette.rest)) }
        return out
    }

    #if os(iOS)
    static let limit = 4
    #else
    static let limit = 9
    #endif
}

/// Donut with a legend next to it, sized for the header's chart row.
struct HeaderPieChart: View {
    let slices: [WealthSlice]
    let emptyText: LocalizedStringKey
    /// Click on a legend entry (category slices only).
    var onSelect: ((String) -> Void)? = nil
    @Environment(\.textScale) private var textScale

    #if os(iOS)
    private let rows = 2
    #else
    private let rows = 3
    #endif

    var body: some View {
        let total = slices.reduce(Int64(0)) { $0 + $1.cents }
        if total <= 0 {
            Text(emptyText)
                .appFont(.caption)
                .foregroundStyle(Theme.secondary)
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
        } else {
            HStack(alignment: .center, spacing: 14) {
                Chart(slices) { p in
                    SectorMark(angle: .value("Betrag", p.cents), innerRadius: .ratio(0.55), angularInset: 1)
                        .cornerRadius(2)
                        .foregroundStyle(p.color)
                }
                .chartLegend(.hidden)
                .aspectRatio(1, contentMode: .fit)
                .accessibilityHidden(true)
                legend(total: total)
                Spacer(minLength: 0)
            }
            .accessibilityElement(children: .contain)
            .accessibilityLabel("Anteile")
        }
    }

    private func legend(total: Int64) -> some View {
        let columns = stride(from: 0, to: slices.count, by: rows).map { Array(slices[$0..<min($0 + rows, slices.count)]) }
        return HStack(alignment: .top, spacing: 16) {
            ForEach(columns.indices, id: \.self) { c in
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(columns[c]) { entry($0, total: total) }
                }
                .frame(maxWidth: 200 * textScale, alignment: .leading)
            }
        }
    }

    @ViewBuilder
    private func entry(_ s: WealthSlice, total: Int64) -> some View {
        let row = HStack(spacing: 5) {
            Circle().fill(s.color).frame(width: 7, height: 7)
            Text(s.label)
                .sensitive(.labels)
                .lineLimit(1)
                .foregroundStyle(Theme.text)
            Text(Double(s.cents) / Double(total), format: .percent.precision(.fractionLength(0)))
                .sensitive()
                .monospacedDigit()
                .foregroundStyle(Theme.secondary)
                .fixedSize()
        }
        .appFont(.caption2)
        if let onSelect, Int64(s.id) != nil {
            Button { onSelect(s.id) } label: { row.contentShape(Rectangle()) }
                .buttonStyle(.plain)
        } else {
            row
        }
    }
}

/// Pie of an account (group, all accounts): expenses by category in the selected period.
/// One SQL aggregate, loaded off the main thread.
struct AccountExpensePie: View {
    @Environment(AppModel.self) private var model
    /// The slices and the key they were loaded for.
    @State private var loaded: (key: Key, parts: [HeaderPieSlices.Part])?

    private struct Key: Hashable {
        let ids: [Int64]
        let from: String?
        let to: String?
        /// Changes whenever the category sums are reloaded (edits, sync, import).
        let version: [Int64]

        /// Same accounts and period: slices of an older `version` may stand in until the new ones
        /// arrive, those of another period or selection never (#1413).
        func covers(_ other: Key) -> Bool { ids == other.ids && from == other.from && to == other.to }
    }

    var body: some View {
        // the period the header names (#1413), not one still loading
        let (from, to) = model.shownPeriod.range(salaryCycle: model.shownSalaryCycle)
        let key = Key(ids: model.headerChartAccountIDs, from: from, to: to, version: model.categories.map(\.totalCents))
        Group {
            if let loaded, loaded.key.covers(key) {
                HeaderPieChart(slices: HeaderPieSlices.fold(loaded.parts, limit: HeaderPieSlices.limit),
                               emptyText: "Keine kategorisierten Ausgaben im Zeitraum")
            } else {
                Color.clear
            }
        }
        .task(id: key) {
            let parts = try? await model.vaultOffMain { vault in
                HeaderPieSlices.expenses(try vault.listCategoriesFiltered(
                    dateFrom: key.from, dateTo: key.to, accountIds: key.ids, checked: nil, direction: .expense))
            }
            guard !Task.isCancelled, let parts else { return }
            loaded = (key, parts)
        }
    }
}
