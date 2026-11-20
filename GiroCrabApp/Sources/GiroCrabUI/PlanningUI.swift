import SwiftUI
import Charts
import GiroCrabCore

// MARK: - Model glue

extension AppModel {
    /// `nil` = locked, cancelled or the vault changed meanwhile (keep what is shown).
    func budgetProgress(year: Int, month: Int) async -> [CategoryBudgetStatus]? {
        try? await vaultOffMain { try $0.listCategoryBudgetProgress(year: Int32(year), month: UInt32(month)) }
    }

    func setBudget(_ cents: Int64?, for categoryID: Int64) {
        guard let vault else { return }
        do { try vault.setCategoryBudget(categoryId: categoryID, budgetCents: cents) } catch { errorMessage = "\(error)" }
        reloadCategories()
    }

    /// `nil` = could not be read (error, locked, vault swapped) – not the same as "not split".
    func splits(for booking: Booking) async -> [TransactionSplit]? {
        let id = booking.id
        return try? await vaultOffMain { try $0.listTransactionSplits(transactionId: id) }
    }

    func saveSplits(_ parts: [TransactionSplitInput], for booking: Booking) -> String? {
        guard let vault else { return nil }
        do { try vault.setTransactionSplits(transactionId: booking.id, splits: parts) } catch { return "\(error)" }
        reloadBookings(); reloadCategories(); reloadAccounts()
        return nil
    }

    /// `nil` = locked, cancelled or the vault changed meanwhile (keep what is shown).
    /// `accountIds` empty = all accounts. `respectInTotal`: the card as an
    /// overall money figure (#337) — only the effective total set contributes;
    /// the default `false` keeps the proof semantics of a directly requested
    /// selection (single account).
    func cashflow(_ g: PeriodGranularity, count: Int, accountIds: [Int64] = [], respectInTotal: Bool = false) async -> [PeriodCashflow]? {
        try? await vaultOffMain {
            try $0.cashflowByPeriod(granularity: g, count: UInt32(count), accountIds: accountIds, respectInTotal: respectInTotal)
        }
    }

    func transferTemplates() async -> [TransferTemplate] {
        (try? await vaultOffMain { try $0.listTransferTemplates() }) ?? []
    }

    func saveTransferTemplate(named name: String) {
        guard let vault else { return }
        let d = transferDraft
        let t = TransferTemplate(
            id: 0, name: name, recipientName: d.name, recipientIban: d.iban, recipientBic: d.bic,
            defaultAmountCents: (try? parseAmountToCents(text: d.amountText)).flatMap { $0 > 0 ? $0 : nil },
            defaultPurpose: d.purpose)
        do { _ = try vault.saveTransferTemplate(template: t) } catch { transferError = "\(error)" }
    }

    func applyTransferTemplate(_ t: TransferTemplate) {
        fillTransferDraft(ParsedPayment(name: t.recipientName, iban: t.recipientIban, bic: t.recipientBic,
                                        amountText: t.defaultAmountCents.map { Money.plain($0) } ?? "",
                                        purpose: t.defaultPurpose))
    }

    func deleteTransferTemplate(_ id: Int64) {
        try? vault?.deleteTransferTemplate(id: id)
    }
}

// MARK: - Collapse chevron (GitHub #603)

struct CardChevron: View {
    @Binding var collapsed: Bool
    var customTitle: LocalizedStringKey? = nil
    var body: some View {
        let title: LocalizedStringKey = customTitle ?? (collapsed ? "Einblenden" : "Ausblenden")
        Button {
            withAnimation(Motion.animation(.easeInOut(duration: 0.25))) { collapsed.toggle() }
        } label: {
            Image(systemName: collapsed ? "chevron.down" : "chevron.up")
                .appFont(.caption, weight: .semibold)
                .foregroundStyle(Theme.secondary)
                .padding(4)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help(title)
        .accessibilityLabel(title)
    }
}

// MARK: - Budget bar

enum BudgetTone {
    /// green < 80 %, yellow 80–100 %, red > 100 %.
    static func color(_ percent: Double) -> Color {
        percent > 100 ? Theme.negative : (percent >= 80 ? Theme.warning : Theme.inflow)
    }
}

struct BudgetBar: View {
    let percent: Double
    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                Capsule().fill(Theme.border)
                Capsule().fill(BudgetTone.color(percent))
                    .frame(width: geo.size.width * min(max(percent, 0), 100) / 100)
            }
        }
        .frame(height: 6)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text("Budgetverbrauch"))
        .accessibilityValue(Self.spokenValue(percent))
    }

    /// "85 % des Budgets verbraucht"; over budget (red bar) is never spoken as 100 %.
    static func spokenValue(_ percent: Double) -> Text {
        let rounded = Int(percent.rounded())
        return percent > 100
            ? Text("\(max(rounded, 101)) % des Budgets verbraucht, überschritten")
            : Text("\(min(rounded, 100)) % des Budgets verbraucht")
    }
}

/// Monthly category budgets of the current month with progress bars.
struct BudgetProgressCard: View {
    @Environment(AppModel.self) private var model
    @State private var items: [CategoryBudgetStatus] = []
    @State private var editing: IDBox?
    /// Budget row under the pointer: shows its pencil.
    @State private var hovered: Int64?
    @AppStorage(ViewLayout.budgetsCollapsedKey) private var collapsed = false

    private var totalSpent: Int64 { items.reduce(0) { $0 + $1.spentCents } }
    private var totalBudget: Int64 { items.reduce(0) { $0 + $1.budgetCents } }
    private var totalPercent: Double { totalBudget > 0 ? Double(totalSpent) / Double(totalBudget) * 100 : 0 }

    private var token: String {
        model.categories.map { "\($0.id):\($0.budgetCents ?? 0):\($0.totalCents)" }.joined(separator: ",") + "#\(model.bookings.count)"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: ViewLayout.cardSpacing) {
            HStack {
                CardChevron(collapsed: $collapsed)
                Text("Monatsbudgets").appFont(.subheadline, weight: .semibold)
                Spacer()
                if collapsed {
                    if !items.isEmpty {
                        Text("\(Money.format(totalSpent)) / \(Money.format(totalBudget))")
                            .sensitive(.amounts)
                            .appFont(.subheadline, weight: .semibold, monospacedDigit: true).foregroundStyle(BudgetTone.color(totalPercent))
                            .fixedSize()
                        BudgetBar(percent: totalPercent).frame(width: 60)
                    }
                } else {
                Menu {
                    ForEach(model.categories.filter { $0.budgetCents == nil }, id: \.id) { c in
                        Button(c.path) { editing = IDBox(id: c.id) }
                    }
                } label: { Label("Budget hinzufügen", systemImage: "plus.circle") }
                .menuStyle(.borderlessButton).fixedSize()
                }
            }
            if !collapsed && items.isEmpty {
                Text("Noch keine Budgets. Legen Sie über das Kontextmenü einer Kategorie ein Monatsbudget fest.")
                    .appFont(.caption).foregroundStyle(Theme.secondary)
            }
            if !collapsed { ForEach(items, id: \.categoryId) { s in
                Button { editing = IDBox(id: s.categoryId) } label: {
                    VStack(alignment: .leading, spacing: 4) {
                        HStack {
                            Text(s.name).appFont(.callout).foregroundStyle(Theme.text).sensitive(.labels)
                            Spacer()
                            Text("\(Money.format(s.spentCents)) / \(Money.format(s.budgetCents))")
                                .sensitive(.amounts)
                                .appFont(.caption, monospacedDigit: true).foregroundStyle(BudgetTone.color(s.percent))
                            // shows the row opens the editor (it looked read-only)
                            Image(systemName: "pencil")
                                .appFont(.caption)
                                .foregroundStyle(Theme.secondary)
                                .opacity(hovered == s.categoryId ? 1 : 0)
                                .accessibilityHidden(true)
                        }
                        BudgetBar(percent: s.percent)
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                // the button merges its children; keep the bar's value
                .accessibilityValue(BudgetBar.spokenValue(s.percent))
                .onHover { inside in
                    if inside { hovered = s.categoryId } else if hovered == s.categoryId { hovered = nil }
                }
                .help("Budget bearbeiten …")
                .contextMenu {
                    Button("Budget bearbeiten …", systemImage: "pencil") { editing = IDBox(id: s.categoryId) }
                    Button("Budget entfernen", systemImage: "trash", role: .destructive) {
                        model.setBudget(nil, for: s.categoryId)
                    }
                }
            } }
        }
        .card(padding: ViewLayout.cardPadding, vertical: ViewLayout.cardVerticalPadding(collapsed: collapsed))
        .task(id: token) {
            let now = Calendar(identifier: .gregorian).dateComponents([.year, .month], from: Date())
            if let loaded = await model.budgetProgress(year: now.year ?? 2025, month: now.month ?? 1) { items = loaded }
        }
        .capturableSheet(item: $editing) { box in BudgetEditorSheet(categoryID: box.id).environment(model) }
    }
}

struct IDBox: Identifiable { let id: Int64 }

struct BudgetEditorSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let categoryID: Int64
    @State private var text = ""

    private var category: GiroCrabCore.Category? { model.categories.first { $0.id == categoryID } }
    private var cents: Int64? {
        guard let c = try? parseAmountToCents(text: text), c > 0 else { return nil }
        return c
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Monatsbudget").appFont(.title3, weight: .bold)
            Text(category?.path ?? "").foregroundStyle(Theme.secondary).sensitive(.labels)
            HStack {
                TextField(Money.plain(0), text: $text)
                    .textFieldStyle(.roundedBorder).multilineTextAlignment(.trailing)
                    .appFont(.body, monospacedDigit: true).frame(maxWidth: 160)
                Text("€ pro Monat").foregroundStyle(Theme.secondary)
            }
            Text("Ausgaben der Unterkategorien zählen mit.").appFont(.caption).foregroundStyle(Theme.tertiary)
            HStack {
                if category?.budgetCents != nil {
                    Button("Budget entfernen", role: .destructive) { model.setBudget(nil, for: categoryID); dismiss() }
                }
                Spacer()
                Button("Abbrechen", role: .cancel) { dismiss() }.keyboardShortcut(.cancelAction)
                Button("Speichern") { model.setBudget(cents, for: categoryID); dismiss() }
                    .keyboardShortcut(.defaultAction).buttonStyle(.borderedProminent).disabled(cents == nil)
            }
        }
        .padding(22)
        .macFrame(width: 380)
        .onAppear { text = category?.budgetCents.map { Money.plain($0) } ?? "" }
    }
}

// MARK: - Chart color palette

enum ChartColorPalette: String, CaseIterable, Identifiable {
    case girocrab, profi, emerald
    var id: String { rawValue }

    var title: LocalizedStringKey {
        switch self {
        case .girocrab: "GiroCrab — Cyan & Orange"
        case .profi: "Profi-Banking — Blau & Schiefer"
        case .emerald: "Klassisch — Smaragd & Rose"
        }
    }

    private var hexes: (income: (String, String), expense: (String, String)) {
        switch self {
        case .girocrab: (("#38BDF8", "#0284C7"), ("#F97316", "#EA580C"))
        case .profi: (("#3B82F6", "#2563EB"), ("#64748B", "#475569"))
        case .emerald: (("#10B981", "#059669"), ("#F43F5E", "#E11D48"))
        }
    }

    private func gradient(_ h: (String, String)) -> LinearGradient {
        LinearGradient(colors: [Color(hex: h.0).opacity(0.95), Color(hex: h.1).opacity(0.45)],
                       startPoint: .top, endPoint: .bottom)
    }

    var incomeGradient: LinearGradient { gradient(hexes.income) }
    var expenseGradient: LinearGradient { gradient(hexes.expense) }
    var incomeDot: Color { Color(hex: hexes.income.0) }
    var expenseDot: Color { Color(hex: hexes.expense.0) }
}

// MARK: - Cashflow comparison

struct CashflowCard: View {
    @Environment(AppModel.self) private var model
    @AppStorage("chartColorPalette") private var paletteRaw = ChartColorPalette.girocrab.rawValue
    @State private var granularity: PeriodGranularity = .month
    @State private var rows: [PeriodCashflow] = []
    @AppStorage(ViewLayout.cashflowCollapsedKey) private var collapsed = false

    private struct Bar: Identifiable {
        let id = UUID()
        let period: String
        let kind: String
        let euros: Double
    }

    private func label(_ key: String) -> String {
        granularity == .month ? DateText.monthLabel(key) : key
    }

    private func legendDot(_ color: Color, _ title: String) -> some View {
        HStack(spacing: 5) { Circle().fill(color).frame(width: 8, height: 8); Text(title) }
    }

    var body: some View {
        let palette = ChartColorPalette(rawValue: paletteRaw) ?? .girocrab
        let income = String(localized: "Einnahmen")
        let expense = String(localized: "Ausgaben")
        let bars = rows.flatMap { r in
            [Bar(period: label(r.key), kind: income, euros: Double(r.incomeCents) / 100),
             Bar(period: label(r.key), kind: expense, euros: Double(r.expenseCents) / 100)]
        }
        VStack(alignment: .leading, spacing: ViewLayout.cardSpacing) {
            HStack {
                CardChevron(collapsed: $collapsed)
                Text("Cashflow").appFont(.subheadline, weight: .semibold)
                Spacer()
                if collapsed {
                    if let last = rows.last {
                        let net = last.incomeCents - last.expenseCents
                        Text("\(label(last.key)): \(Money.format(net))")
                            .sensitive(.amounts)
                            .appFont(.subheadline, weight: .semibold, monospacedDigit: true)
                            .foregroundStyle(net >= 0 ? Theme.inflow : Theme.outflow)
                            .fixedSize()
                    }
                } else {
                Menu {
                    Picker("Farbschema", selection: $paletteRaw) {
                        ForEach(ChartColorPalette.allCases) { Text($0.title).tag($0.rawValue) }
                    }
                    .pickerStyle(.inline)
                } label: {
                    Image(systemName: "paintpalette")
                }
                .menuStyle(.borderlessButton).fixedSize()
                .help(Text("Farbschema"))
                .accessibilityLabel(Text("Farbschema"))
                Picker("Zeitraum", selection: $granularity) {
                    Text("Monat").tag(PeriodGranularity.month)
                    Text("Quartal").tag(PeriodGranularity.quarter)
                    Text("Jahr").tag(PeriodGranularity.year)
                }
                .pickerStyle(.segmented).labelsHidden().macFrame(width: 220)
                }
            }
            if !collapsed {
            Chart(bars) { b in
                BarMark(x: .value("Zeitraum", b.period), y: .value("€", b.euros))
                    .foregroundStyle(by: .value("Art", b.kind))
                    .position(by: .value("Art", b.kind))
                    .clipShape(RoundedRectangle(cornerRadius: 5))
            }
            .chartForegroundStyleScale([income: palette.incomeGradient, expense: palette.expenseGradient])
            .chartLegend(.hidden)
            .chartYAxis {
                AxisMarks(position: .leading) { v in
                    AxisGridLine(stroke: StrokeStyle(lineWidth: 0.5, dash: [3, 3]))
                        .foregroundStyle(Theme.border)
                    AxisValueLabel {
                        if let d = v.as(Double.self) { Text(d, format: .number.precision(.fractionLength(0))).appFont(.caption2).fixedSize().sensitive(.amounts) }
                    }
                }
            }
            .chartXAxis {
                AxisMarks { v in
                    AxisGridLine(stroke: StrokeStyle(lineWidth: 0.5, dash: [3, 3]))
                        .foregroundStyle(Theme.border)
                    AxisValueLabel { Text(verbatim: v.as(String.self) ?? "").appFont(.caption2).fixedSize() }
                }
            }
            .frame(height: 160)
            .accessibilityLabel("Cashflow")
            .accessibilityChartDescriptor(BarChartDescriptor(
                title: String(localized: "Cashflow"),
                summary: String(localized: "Einnahmen und Ausgaben je Zeitraum"),
                categoryTitle: String(localized: "Zeitraum"),
                categories: rows.map { label($0.key) },
                valueTitle: String(localized: "Betrag"),
                series: [
                    .init(name: income, values: rows.map { Double($0.incomeCents) / 100 }),
                    .init(name: expense, values: rows.map { Double($0.expenseCents) / 100 }),
                ]
            ))
            HStack(spacing: 14) {
                legendDot(palette.incomeDot, income)
                legendDot(palette.expenseDot, expense)
            }
            .appFont(.caption).foregroundStyle(Theme.secondary)
            .accessibilityHidden(true)   // the chart descriptor names both series
            }
        }
        .card(padding: ViewLayout.cardPadding, vertical: ViewLayout.cardVerticalPadding(collapsed: collapsed))
        // Follows the sidebar: overview = all accounts, account/group = its accounts (#731).
        // The calculation context is part of the key: switching between a single
        // account and a group of only that account keeps ids and booking count,
        // but changes proof (#337) vs. totals sums. `totalsVersion` catches the
        // account/group switches that change the effective set without moving
        // the (proof) booking count (invalidateTotals in afterGroupChange/reloadAll).
        .task(id: "\(granularity)#\(model.bookings.count)#\(accountIds)#\(model.sidebarSelection?.usesTotalsContext ?? true)#\(model.totalsVersion)") {
            // calculation context like `currentFilter` (#337): overview, group,
            // kind / bank = overall money figure; a directly requested single
            // account is a proof view and keeps its sums
            if let loaded = await model.cashflow(granularity, count: granularity == .year ? 5 : 6, accountIds: accountIds,
                                                 respectInTotal: model.sidebarSelection?.usesTotalsContext ?? true) {
                rows = loaded
            }
        }
    }

    /// Accounts of the sidebar selection; a category view keeps its account filter.
    private var accountIds: [Int64] {
        model.currentFilter.accountIds
    }
}

// MARK: - Custom period (GitHub #53)

/// Start and end date of the user-defined period.
struct CustomPeriodForm: View {
    @Environment(AppModel.self) private var model
    @State private var from = Date()
    @State private var to = Date()

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Eigener Zeitraum").appFont(.headline)
            Grid(alignment: .leading, horizontalSpacing: 12, verticalSpacing: 8) {
                GridRow {
                    Text("Von")
                    // HIG: prevent invalid input instead of correcting it afterwards.
                    DatePicker("Von", selection: $from, in: ...to, displayedComponents: .date).labelsHidden()
                }
                GridRow {
                    Text("Bis")
                    DatePicker("Bis", selection: $to, in: from..., displayedComponents: .date).labelsHidden()
                }
            }
            HStack {
                Spacer()
                Button("Abbrechen", role: .cancel) { model.showCustomPeriod = false }
                    .keyboardShortcut(.cancelAction)
                Button("Übernehmen") {
                    model.applyCustomPeriod(from: from, to: to)
                    model.showCustomPeriod = false
                }
                .keyboardShortcut(.defaultAction)
            }
        }
        .padding(16)
        .frame(minWidth: 280)
        #if os(iOS)
        .presentationDetents([.medium])
        #endif
        .onAppear {
            let a = DateText.date(model.customPeriodFrom) ?? Date()
            let b = DateText.date(model.customPeriodTo) ?? Date()
            (from, to) = a <= b ? (a, b) : (b, a)
        }
    }
}

// MARK: - Split booking

/// Split arithmetic in the booking's direction (cents, positive = same sign as the booking).
enum SplitMath {
    static func remaining(total: Int64, parts: [Int64]) -> Int64 { total - parts.reduce(0, +) }
    /// At least two parts, none 0, parts add up to `total`. Negative parts are allowed.
    static func isValid(total: Int64, parts: [Int64]) -> Bool {
        parts.count >= 2 && !parts.contains(0) && remaining(total: total, parts: parts) == 0
    }
}

struct SplitSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.reportRedaction) private var redaction
    @Environment(\.dismiss) private var dismiss
    let booking: Booking

    private struct Part: Identifiable {
        let id = UUID()
        var categoryID: Int64?
        var amount = ""
        var note = ""
    }
    @State private var parts: [Part] = []
    @State private var error: String?
    /// The booking is split already (loaded with the parts).
    @State private var hasSplits = false

    private var total: Int64 { abs(booking.amountCents) }
    private var sign: Int64 { booking.amountCents < 0 ? -1 : 1 }
    private func cents(_ p: Part) -> Int64 { (try? parseAmountToCents(text: p.amount)) ?? 0 }
    // Amounts are entered in the booking's direction: a negative part counter-books the others
    // (+59 = +200 and -141, #427). Only the sum must match; no part may be 0.
    private var remaining: Int64 { SplitMath.remaining(total: total, parts: parts.map(cents)) }
    private var valid: Bool { SplitMath.isValid(total: total, parts: parts.map(cents)) }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Umsatz aufteilen").appFont(.title3, weight: .bold)
            Text("\(booking.merchantName) · \(Money.format(booking.amountCents))").foregroundStyle(Theme.secondary).sensitive([.personal, .amounts])
            ForEach($parts) { $p in
                HStack(spacing: 8) {
                    Picker("Kategorie", selection: $p.categoryID) {
                        Text(model.uncategorizedLabel).tag(Int64?.none)
                        ForEach(model.categories, id: \.id) { c in Text(c.path).tag(Int64?.some(c.id)) }
                    }
                    .labelsHidden().frame(maxWidth: 190)
                    .sensitive(.labels)
                    TextField(Money.plain(0), text: $p.amount)
                        .textFieldStyle(.roundedBorder).multilineTextAlignment(.trailing)
                        .appFont(.body, monospacedDigit: true).frame(width: 90)
                    TextField("Notiz", text: $p.note).textFieldStyle(.roundedBorder)
                    if parts.count > 2 {
                        Button { parts.removeAll { $0.id == p.id } } label: { Label("Aufteilung entfernen", systemImage: "minus.circle") }
                            .labelStyle(.iconOnly)
                            .buttonStyle(.borderless)
                            .help("Aufteilung entfernen")
                    }
                }
            }
            HStack {
                Button { parts.append(Part(amount: remaining != 0 ? Money.plain(remaining) : "")) } label: {
                    Label("Teil hinzufügen", systemImage: "plus")
                }
                .buttonStyle(.borderless)
                Spacer()
                Text("Restbetrag: \(Money.format(remaining))")
                    .appFont(.callout, weight: .semibold, monospacedDigit: true)
                    .foregroundStyle(remaining == 0 ? Theme.inflow : Theme.warning)
                    .sensitive(.amounts)
                if remaining != 0, let last = parts.indices.last {
                    // one click: the rest goes into the last part (#427)
                    Button("Rest zuweisen") {
                        let others = parts.indices.filter { $0 != last }.reduce(0) { $0 + cents(parts[$1]) }
                        parts[last].amount = Money.plain(total - others)
                    }
                    .help("Trägt den Restbetrag im letzten Teil ein")
                }
            }
            // Why "Speichern" is greyed out (#383): say so instead of leaving it silent.
            if !valid, !parts.isEmpty, error == nil {
                Text(remaining != 0
                    ? "Die Teilbeträge müssen zusammen \(redaction.masking(Money.format(total), .amounts)) ergeben (Restbetrag: \(redaction.masking(Money.format(remaining), .amounts))), erst dann lässt sich speichern."
                    : "Jeder Teil braucht einen Betrag ungleich 0.")
                    .appFont(.callout).foregroundStyle(Theme.secondary)
            }
            if let error { Text(error).appFont(.callout).foregroundStyle(Theme.negative) }
            HStack {
                if hasSplits {
                    Button("Aufteilung aufheben", role: .destructive) {
                        error = model.saveSplits([], for: booking)
                        if error == nil { dismiss() }
                    }
                }
                Spacer()
                Button("Abbrechen", role: .cancel) { dismiss() }.keyboardShortcut(.cancelAction)
                Button("Speichern") {
                    let inputs = parts.map {
                        TransactionSplitInput(categoryId: $0.categoryID, amountCents: sign * cents($0), note: $0.note)
                    }
                    error = model.saveSplits(inputs, for: booking)
                    if error == nil { dismiss() }
                }
                .keyboardShortcut(.defaultAction).buttonStyle(.borderedProminent).disabled(!valid)
            }
        }
        .padding(22)
        .macFrame(width: 560)
        .task {
            guard let existing = await model.splits(for: booking) else {
                // never offer a fresh split over one that could not be read
                error = String(localized: "Die bestehende Aufteilung konnte nicht geladen werden.")
                return
            }
            hasSplits = !existing.isEmpty
            if existing.isEmpty {
                parts = [Part(categoryID: booking.categoryId, amount: Money.plain(total)), Part()]
            } else {
                parts = existing.map { Part(categoryID: $0.categoryId, amount: Money.plain(sign * $0.amountCents), note: $0.note) }
            }
        }
    }
}

// MARK: - Transfer templates

struct TransferTemplatesMenu: View {
    @Environment(AppModel.self) private var model
    @State private var templates: [TransferTemplate] = []
    @State private var naming = false
    @State private var deleting: TransferTemplate?
    @State private var name = ""

    var body: some View {
        Menu {
            ForEach(templates, id: \.id) { t in
                Button("\(t.name) · \(t.recipientName)") { model.applyTransferTemplate(t) }
            }
            if !templates.isEmpty {
                Divider()
                Menu("Vorlage löschen") {
                    ForEach(templates, id: \.id) { t in
                        Button("\(t.name) …", role: .destructive) { deleting = t }
                    }
                }
            }
            Button("Als Vorlage speichern …", systemImage: "square.and.arrow.down") {
                name = model.transferDraft.name
                naming = true
            }
            .disabled(model.transferDraft.name.trimmingCharacters(in: .whitespaces).isEmpty || model.transferDraft.iban.isEmpty)
        } label: { Label("Vorlagen", systemImage: "doc.on.doc") }
        .menuStyle(.borderlessButton).fixedSize()
        .task { templates = await model.transferTemplates() }
        .confirmationDialog(Text("Vorlage „\(deleting?.name ?? "")“ löschen?"),
                            isPresented: Binding(get: { deleting != nil }, set: { if !$0 { deleting = nil } }),
                            titleVisibility: .visible, presenting: deleting) { t in
            Button("Löschen", role: .destructive) { model.deleteTransferTemplate(t.id); Task { templates = await model.transferTemplates() } }
            Button("Abbrechen", role: .cancel) {}
        }
        .alert("Vorlage speichern", isPresented: $naming) {
            TextField("Name der Vorlage", text: $name)
            Button("Speichern") { model.saveTransferTemplate(named: name); Task { templates = await model.transferTemplates() } }
            Button("Abbrechen", role: .cancel) {}
        }
    }
}
