import SwiftUI
import Charts
import GiroCrabCore

/// Securities positions of a depot account (from the last sync).
struct HoldingsCard: View {
    @Environment(AppModel.self) private var model
    let account: Account
    @State private var holdings: [Holding] = []

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if !holdings.isEmpty {
                Text("Positionen").font(.caption.weight(.semibold)).foregroundStyle(Theme.secondary)
            }
            ForEach(holdings.indices, id: \.self) { i in
                let h = holdings[i]
                HStack(alignment: .firstTextBaseline) {
                    VStack(alignment: .leading, spacing: 1) {
                        Text(h.name.isEmpty ? (h.isin.isEmpty ? h.wkn : h.isin) : h.name).lineLimit(1)
                        Text("\(h.quantity.formatted(.number.precision(.fractionLength(0...4)))) × \(Money.format(h.currentPriceCents)) · \(h.isin.isEmpty ? h.wkn : h.isin)")
                            .font(.caption2).foregroundStyle(Theme.secondary).lineLimit(1)
                    }
                    Spacer()
                    Text(Money.format(h.valueCents)).monospacedDigit()
                }
                .font(.callout)
            }
        }
        .task(id: "\(account.id)-\(account.balanceCents)-\(account.balanceDate)") {
            holdings = model.holdings(for: account.id)
        }
    }
}

/// Right-click menu on the header's account info: copies bank details,
/// IBAN or balance as plain text.
struct AccountCopyMenu: View {
    let account: Account

    var body: some View {
        // From the Bundesbank BLZ list (German IBANs only); looked up once per menu.
        let bic = bicForIban(iban: account.iban)
        Section("In Zwischenablage kopieren") {
            if !account.iban.isEmpty {
                Button("Bankverbindung") { copyToPasteboard(bankDetails(bic: bic)) }
                Button("IBAN") { copyToPasteboard(account.iban) }
            }
            if let bic {
                Button("BIC") { copyToPasteboard(bic) }
            }
            Button("Kontostand") { copyToPasteboard(Money.clipboard(account.balanceCents)) }
        }
    }

    private func bankDetails(bic: String?) -> String {
        let bank: String
        switch (bic, account.bankName.isEmpty) {
        case (let b?, false): bank = "BIC: \(b) (\(account.bankName))"
        case (let b?, true): bank = "BIC: \(b)"
        case (nil, false): bank = "Bank: \(account.bankName)"
        case (nil, true): bank = ""
        }
        return [
            account.owner.isEmpty ? "" : "Name: \(account.owner)",
            "IBAN: \(formatIBAN(account.iban))",
            bank,
        ].filter { !$0.isEmpty }.joined(separator: "\n")
    }
}

/// Header card of the stream: title, balance, calm balance chart and the
/// monthly KPIs (consumption is always shown without ETF plans / perks).
struct StreamHeaderCard: View {
    @Environment(AppModel.self) private var model
    #if os(iOS)
    @Environment(\.horizontalSizeClass) private var sizeClass
    #endif
    @State private var showDocuments = false

    private var account: Account? {
        if case .account(let id) = model.sidebarSelection { return model.account(for: id) }
        return nil
    }

    /// Depots show positions; bookings (buys, sells) live on the clearing account.
    private var isDepot: Bool {
        guard let a = account else { return false }
        return a.kind == .brokerage || a.name.localizedCaseInsensitiveContains("depot")
    }

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 9) {
            HStack(alignment: .top) {
                VStack(alignment: .leading, spacing: 3) {
                    Text(model.title).font(.title2.weight(.semibold)).foregroundStyle(Theme.text).lineLimit(1).minimumScaleFactor(0.7)
                    Group {
                        if let a = account {
                            Text([a.bankName, a.iban.isEmpty ? "" : formatIBAN(a.iban)].filter { !$0.isEmpty }.joined(separator: " · "))
                        } else {
                            Text(model.lastSyncText)
                        }
                    }
                    .font(.caption)
                    .foregroundStyle(Theme.secondary)
                }
                .contextMenu {
                    if let a = account { AccountCopyMenu(account: a) }
                }
                Spacer()
                if let hb = model.headerBalance {
                    VStack(alignment: .trailing, spacing: 2) {
                        Text(Money.format(hb.cents))
                            .font(.system(size: 26, weight: .semibold).monospacedDigit())
                            .foregroundStyle(account?.kind == .loan ? Theme.loan : Theme.text)
                            .lineLimit(1)
                            .minimumScaleFactor(0.6)
                        Text(hb.label).font(.caption).foregroundStyle(Theme.secondary)
                    }
                    .contextMenu {
                        Button("\(hb.label) kopieren", systemImage: "doc.on.doc") { copyToPasteboard(Money.clipboard(hb.cents)) }
                    }
                }
            }

            if let a = account, a.kind == .loan {
                LoanProgressCard(account: a, compact: true)
            }

            if let a = account, isDepot {
                HoldingsCard(account: a)
            }

            if isDepot {
                depotRow
            } else {
                if model.scope != .investments {
                    #if os(iOS)
                    BalanceChart(points: model.history) { model.jumpToDay($0) }
                        .frame(height: 28)
                    #else
                    BalanceChart(points: model.history) { model.jumpToDay($0) }
                        .frame(height: 56)
                    #endif
                }

                #if os(iOS)
                HStack(spacing: 10) {
                    Text("Konsum \(Money.format(-model.consumptionThisMonth))").foregroundStyle(Theme.outflow)
                    Text("Einnahmen \(Money.format(model.incomeThisMonth))").foregroundStyle(Theme.inflow)
                    Spacer(minLength: 0)
                }
                .font(.caption.weight(.medium).monospacedDigit())
                .lineLimit(1)
                .minimumScaleFactor(0.7)
                #else
                HStack(spacing: 10) { kpis }
                    .frame(maxWidth: .infinity)
                    .fixedSize(horizontal: false, vertical: true)
                #endif

                filterRow
            }
        }
        .card()
    }

    /// Depot: no cash-flow KPIs, chart or period filters – only documents and a hint.
    @ViewBuilder
    private var depotRow: some View {
        HStack(spacing: 12) {
            if let a = account {
                Button { showDocuments = true } label: {
                    Label("Dokumente", systemImage: "doc.text")
                }
                .buttonStyle(.borderless)
                .sheet(isPresented: $showDocuments) { AccountDocumentsSheet(account: a) }
            }
            Spacer()
            Text("Depot: Positionen statt Buchungen")
                .lineLimit(1)
        }
        .font(.caption)
        .foregroundStyle(Theme.secondary)
    }

    @ViewBuilder
    private var kpis: some View {
        KPI(title: "Konsumausgaben", subtitle: LocalizedStringKey(model.periodSubtitle), value: Money.format(-model.consumptionThisMonth), tint: Theme.outflow)
        KPI(title: "Investiert", subtitle: "Sparpläne & Perks", value: Money.format(model.investedThisMonth), tint: Theme.invest)
        KPI(title: "Einnahmen", subtitle: LocalizedStringKey(model.periodSubtitle), value: Money.format(model.incomeThisMonth), tint: Theme.inflow)
        fixedCostsKPI
    }

    /// Recurring expenses still due this month (GitHub #35); opens the overview.
    @ViewBuilder
    private var fixedCostsKPI: some View {
        let open = model.recurringMonth?.items.filter { !$0.booked && $0.amountCents < 0 }.count ?? 0
        Button { model.showRecurringSheet = true } label: {
            KPI(title: "Fixkosten offen", subtitle: "Diesen Monat · \(open) offen",
                value: Money.format(-(model.recurringMonth?.openExpenseCents ?? 0)), tint: Theme.outflow,
                disclosure: true)
        }
        .buttonStyle(.plain)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .help("Wiederkehrende Buchungen anzeigen (⇧⌘F)")
        .accessibilityHint("Öffnet die Übersicht der wiederkehrenden Buchungen")
    }

    @ViewBuilder
    private var filterRow: some View {
        @Bindable var model = model
        // Never let the fixed-size filter controls widen the header card:
        // fits -> normal row (count right-aligned); too narrow -> scrolls.
        ViewThatFits(in: .horizontal) {
            filterContent
            ScrollView(.horizontal, showsIndicators: false) {
                filterContent.fixedSize(horizontal: true, vertical: false)
            }
        }
    }

    @ViewBuilder
    private var filterContent: some View {
        @Bindable var model = model
        let account = self.account
        HStack(spacing: 12) {
                PeriodQuickPicker()
                Menu {
                    Picker("Zeitraum", selection: $model.period) {
                        ForEach(Period.allCases) { Text($0.title).tag($0) }
                    }
                    .pickerStyle(.inline)
                } label: {
                    #if os(iOS)
                    if sizeClass == .compact {
                        Image(systemName: "calendar").accessibilityLabel(model.period.title)
                    } else {
                        Label(model.period.title, systemImage: "calendar")
                    }
                    #else
                    Label(model.period.title, systemImage: "calendar")
                    #endif
                }
                .menuStyle(.borderlessButton)
                .fixedSize()
                Menu {
                    Picker("Status", selection: $model.statusFilter) {
                        ForEach(StatusFilter.allCases) { Text($0.title).tag($0) }
                    }
                    .pickerStyle(.inline)
                } label: {
                    #if os(iOS)
                    if sizeClass == .compact {
                        // icon only: the segmented period picker already takes most of the width
                        Image(systemName: "line.3.horizontal.decrease")
                            .accessibilityLabel("Status: \(model.statusFilter.title)")
                    } else {
                        Label("Status: \(model.statusFilter.title)", systemImage: "line.3.horizontal.decrease")
                            .lineLimit(1).fixedSize(horizontal: true, vertical: false)
                    }
                    #else
                    Label("Status: \(model.statusFilter.title)", systemImage: "line.3.horizontal.decrease")
                        .lineLimit(1).fixedSize(horizontal: true, vertical: false)
                    #endif
                }
                .menuStyle(.borderlessButton)
                .fixedSize()
                if let a = account {
                    Button { showDocuments = true } label: {
                        Label("Dokumente", systemImage: "doc.text")
                            .lineLimit(1).fixedSize(horizontal: true, vertical: false)
                            #if os(iOS)
                            .labelStyle(.iconOnly)
                            #endif
                    }
                    .buttonStyle(.borderless)
                    .sheet(isPresented: $showDocuments) { AccountDocumentsSheet(account: a) }
                }
                Spacer()
                #if os(iOS)
                if sizeClass != .compact {
                    Text("\(model.bookingCountLabel) Buchungen")
                        .font(.caption)
                        .foregroundStyle(Theme.secondary)
                        .lineLimit(1)
                        .fixedSize(horizontal: true, vertical: false)
                }
                #else
                Text("\(model.bookingCountLabel) Buchungen")
                    .font(.caption)
                    .foregroundStyle(Theme.secondary)
                    .lineLimit(1)
                    .fixedSize(horizontal: true, vertical: false)
                #endif
            }
        .font(.caption)
        .foregroundStyle(Theme.secondary)
    }
}

struct KPI: View {
    let title: LocalizedStringKey
    let subtitle: LocalizedStringKey
    let value: String
    let tint: Color
    /// Tile opens a detail view: shows a chevron next to the title.
    var disclosure = false

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 3) {
                Text(title).font(.caption.weight(.medium)).foregroundStyle(Theme.secondary)
                    .lineLimit(2, reservesSpace: true)
                if disclosure {
                    Image(systemName: "chevron.right")
                        .font(.system(size: 8, weight: .bold))
                        .foregroundStyle(Theme.tertiary)
                        .accessibilityHidden(true)
                }
            }
            Spacer(minLength: 4)
            Text(value).font(.system(size: 16, weight: .semibold).monospacedDigit()).foregroundStyle(tint)
                .lineLimit(1).minimumScaleFactor(0.75)
            Spacer(minLength: 4)
            Text(subtitle).font(.caption2).foregroundStyle(Theme.tertiary)
                .lineLimit(2, reservesSpace: true)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .padding(.horizontal, 10)
        .padding(.vertical, 6)
        .background(RoundedRectangle(cornerRadius: 9, style: .continuous).fill(Theme.canvas))
        .overlay(RoundedRectangle(cornerRadius: 9, style: .continuous).strokeBorder(Theme.border, lineWidth: 1))
        .contentShape(RoundedRectangle(cornerRadius: 9, style: .continuous))
        .accessibilityElement(children: .combine)
    }
}

struct BalanceChart: View {
    let points: [DailyBalance]
    /// Click on a day (jump to its bookings).
    var onSelect: ((Date) -> Void)? = nil
    /// Day under the mouse pointer (hover readout: date + balance).
    @State private var hovered: Point?

    #if os(iOS)
    @Environment(\.horizontalSizeClass) private var sizeClass
    private var tickDays: Int { sizeClass == .compact ? 14 : 7 }
    private var tickMonths: Int { sizeClass == .compact ? 2 : 1 }
    #else
    private var tickDays: Int { 7 }
    private var tickMonths: Int { 1 }
    #endif

    private struct Point: Identifiable, Equatable {
        let id: Int
        let date: Date
        let cents: Int64
        var euros: Double { Double(cents) / 100 } // display only; data stays in cents
    }

    var body: some View {
        let pts: [Point] = points.enumerated().compactMap { i, p in
            DateText.date(p.date).map { Point(id: i, date: $0, cents: p.balanceCents) }
        }
        let minV = pts.map(\.euros).min() ?? 0
        let maxV = pts.map(\.euros).max() ?? 1
        let pad = max((maxV - minV) * 0.15, 1)
        Chart {
            ForEach(pts) { p in
                AreaMark(x: .value("Tag", p.date, unit: .day), yStart: .value("Min", minV - pad), yEnd: .value("Saldo", p.euros))
                    .foregroundStyle(LinearGradient(colors: [Theme.accent.opacity(0.14), Theme.accent.opacity(0.01)], startPoint: .top, endPoint: .bottom))
                    .interpolationMethod(.monotone)
                LineMark(x: .value("Tag", p.date, unit: .day), y: .value("Saldo", p.euros))
                    .foregroundStyle(Theme.accent.opacity(0.8))
                    .lineStyle(StrokeStyle(lineWidth: 1.5))
                    .interpolationMethod(.monotone)
            }
            if let h = hovered {
                PointMark(x: .value("Tag", h.date, unit: .day), y: .value("Saldo", h.euros))
                    .foregroundStyle(Theme.accent)
                    .symbolSize(36)
                RuleMark(x: .value("Tag", h.date, unit: .day))
                    .foregroundStyle(Theme.secondary.opacity(0.45))
                    .lineStyle(StrokeStyle(lineWidth: 1))
                    .annotation(position: h.id * 2 > pts.count ? .topLeading : .topTrailing, spacing: 4, overflowResolution: .init(x: .fit(to: .chart), y: .fit(to: .chart))) {
                        VStack(spacing: 1) {
                            Text(h.date, format: .dateTime.weekday(.abbreviated).day().month(.abbreviated).year())
                                .font(.caption2)
                                .foregroundStyle(Theme.secondary)
                            Text(Money.format(h.cents))
                                .font(.caption.weight(.semibold).monospacedDigit())
                                .foregroundStyle(h.cents < 0 ? Theme.outflow : Theme.text)
                        }
                        .padding(.horizontal, 7)
                        .padding(.vertical, 4)
                        .background(Theme.card, in: RoundedRectangle(cornerRadius: 6, style: .continuous))
                        .overlay(RoundedRectangle(cornerRadius: 6, style: .continuous).strokeBorder(Theme.border, lineWidth: 1))
                    }
            }
        }
        .chartYScale(domain: (minV - pad)...(maxV + pad))
        .chartYAxis(.hidden)
        .chartXAxis {
            // long periods: one label per year, otherwise per month
            if let first = pts.first?.date, let last = pts.last?.date,
               last.timeIntervalSince(first) > 400 * 86_400 {
                AxisMarks(values: .stride(by: .year)) { _ in
                    AxisValueLabel(format: .dateTime.year())
                        .foregroundStyle(Theme.tertiary)
                }
            } else if let first = pts.first?.date, let last = pts.last?.date,
                      last.timeIntervalSince(first) < 62 * 86_400 {
                // short periods (this/last month): weekly day labels
                AxisMarks(values: .stride(by: .day, count: tickDays)) { _ in
                    AxisValueLabel(format: .dateTime.day().month(.abbreviated))
                        .foregroundStyle(Theme.tertiary)
                }
            } else {
                AxisMarks(values: .stride(by: .month, count: tickMonths)) { _ in
                    AxisValueLabel(format: .dateTime.month(.abbreviated))
                        .foregroundStyle(Theme.tertiary)
                }
            }
        }
        .chartOverlay { proxy in
            GeometryReader { geo in
                Rectangle().fill(.clear).contentShape(Rectangle())
                    .onContinuousHover { phase in
                        switch phase {
                        case .active(let location):
                            guard let plot = proxy.plotFrame else { return }
                            let x = location.x - geo[plot].origin.x
                            guard let date: Date = proxy.value(atX: x) else { return }
                            let nearest = pts.min { abs($0.date.timeIntervalSince(date)) < abs($1.date.timeIntervalSince(date)) }
                            if nearest != hovered { hovered = nearest }
                        case .ended:
                            hovered = nil
                        }
                    }
                    .onTapGesture { location in
                        guard let onSelect, let plot = proxy.plotFrame,
                              let date: Date = proxy.value(atX: location.x - geo[plot].origin.x),
                              let nearest = pts.min(by: { abs($0.date.timeIntervalSince(date)) < abs($1.date.timeIntervalSince(date)) })
                        else { return }
                        onSelect(nearest.date)
                    }
            }
        }
        .onChange(of: points.first?.date) { _, _ in hovered = nil }
        .onChange(of: points.count) { _, _ in hovered = nil }
    }
}
