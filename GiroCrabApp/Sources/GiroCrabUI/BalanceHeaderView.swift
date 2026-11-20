import SwiftUI
import Charts
import GiroCrabCore

/// Right-click / long-press menu on the header's account info: copies bank details,
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

    func bankDetails(bic: String?) -> String {
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
/// The account holder shown under the bank/IBAN line; nil when empty (#606).
func accountHolderLine(_ a: Account) -> String? {
    let owner = a.owner.trimmingCharacters(in: .whitespacesAndNewlines)
    return owner.isEmpty ? nil : owner
}

struct StreamHeaderCard: View {
    @Environment(AppModel.self) private var model
    @Environment(\.reportRedaction) private var redaction
    #if os(iOS)
    @Environment(\.horizontalSizeClass) private var sizeClass
    #endif
    @State private var showDocuments = false
    @AppStorage("showAccountAnalytics") private var showAnalytics: Bool = false
    /// Where the tag button sits in the filter row: the tag popover points at it (#228).
    @State private var tagButtonFrame: CGRect = .zero
    @State private var showAmountFilter = false
    @AppStorage(ViewLayout.statisticsKey) private var showStatistics: Bool = true
    @AppStorage(ViewLayout.compactHeaderKey) private var compactHeader: Bool = false
    @AppStorage(ViewLayout.lastFetchKey) private var showLastFetch: Bool = false
    @AppStorage(BalanceChartStyle.defaultsKey) private var chartStyle: BalanceChartStyle = .line
    @AppStorage(ViewLayout.headerChartCollapsedKey) private var chartCollapsed: Bool = false
    @AppStorage(BalanceBarUnit.defaultsKey) private var barUnit: BalanceBarUnit = .automatic

    private var account: Account? {
        if case .account(let id) = model.shownSelection { return model.account(for: id) }
        return nil
    }

    private var compactHeaderTitle: LocalizedStringKey {
        compactHeader ? "Kopfbereich einblenden" : "Kopfbereich minimieren"
    }

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: ViewLayout.cardSpacing) {
            HStack(alignment: compactHeader ? .center : .top) {
                // Minimise the header: more room for the booking list (GitHub #358).
                CardChevron(collapsed: $compactHeader, customTitle: compactHeaderTitle)
                VStack(alignment: .leading, spacing: 3) {
                    Text(model.title).sensitive(account == nil ? .labels : .personal).appFont(compactHeader ? .subheadline : .title2, weight: .semibold).foregroundStyle(Theme.text).lineLimit(1).minimumScaleFactor(0.7)
                    if !compactHeader {
                    Group {
                        if let a = account {
                            Text(([model.isOffline(a.id) ? OfflineAccountOptions.subtitle(a) : ""] + [a.bankName, a.iban.isEmpty ? "" : formatIBAN(a.iban)]).filter { !$0.isEmpty }.joined(separator: " · "))
                                .sensitive()
                            if let holder = accountHolderLine(a) {
                                Text(holder)
                                    .sensitive()
                                    .appFont(.caption)
                                    .foregroundStyle(Theme.secondary)
                                    .lineLimit(1)
                                    .help(redaction.masking(holder))
                            }
                            let note = model.accountNote(for: a.id)
                            if !note.isEmpty {
                                Text(note)
                                    .sensitive()
                                    .appFont(.caption)
                                    .foregroundStyle(Theme.secondary)
                                    .lineLimit(2)
                                    .help(redaction.masking(note))
                            }
                        } else {
                            Text(model.lastSyncText)
                        }
                    }
                    .appFont(.caption)
                    .foregroundStyle(Theme.secondary)
                    }
                }
                .contextMenu {
                    if let a = account { AccountCopyMenu(account: a) }
                }
                // Folded: period and status filters share the title row (GitHub #603).
                if compactHeader {
                    filterRow
                }
                #if os(iOS)
                // long-press is invisible on iPhone/iPad: show an explicit entry too
                if let a = account {
                    Menu { AccountCopyMenu(account: a) } label: {
                        Image(systemName: "doc.on.doc").appFont(.caption).foregroundStyle(Theme.secondary)
                    }
                    .accessibilityLabel("In Zwischenablage kopieren")
                }
                #endif
                Spacer()
                if let hb = model.headerBalance {
                    VStack(alignment: .trailing, spacing: 2) {
                        Text(Money.format(hb.cents, currency: model.balanceCurrency))
                            .sensitive(.amounts)
                            .appFont(compactHeader ? .subheadline : .largeTitle, weight: .semibold, monospacedDigit: true)
                            .foregroundStyle(account?.kind == .loan ? Theme.loan : Theme.text)
                            .lineLimit(1)
                            .minimumScaleFactor(0.6)
                        if !compactHeader {
                            Text(hb.label).appFont(.caption).foregroundStyle(Theme.secondary)
                            if showLastFetch, let a = account, let fetch = model.lastFetchText(for: a) {
                                Text(fetch.text)
                                    .sensitive()
                                    .appFont(.caption)
                                    .foregroundStyle(Theme.secondary)
                                    .lineLimit(1)
                                    .help(fetch.help ?? "")
                            }
                        }
                        if case .category = model.shownSelection, !model.categoryOtherCurrencies.isEmpty {
                            Text("Zusätzlich: \(model.categoryOtherCurrencies.map { Money.format($0.cents, currency: $0.currency) }.joined(separator: " · "))")
                                .sensitive(.amounts)
                                .appFont(.caption, monospacedDigit: true)
                                .foregroundStyle(Theme.secondary)
                                .lineLimit(1)
                                .help("Weitere Währungen werden nicht umgerechnet und nicht in die Summe einbezogen.")
                        }
                        if account == nil, let split = model.headerTotalSplit {
                            Text("Zusätzlich: \(split.others.map { Money.format($0.cents, currency: $0.currency) }.joined(separator: " · "))")
                                .sensitive(.amounts)
                                .appFont(.caption, monospacedDigit: true)
                                .foregroundStyle(Theme.secondary)
                                .lineLimit(1)
                                .help("Weitere Währungen werden nicht umgerechnet und nicht in die Summe einbezogen.")
                        }
                    }
                    .contextMenu {
                        Button("\(hb.label) kopieren", systemImage: "doc.on.doc") { copyToPasteboard(Money.clipboard(hb.cents)) }
                    }
                }
            }

            if !compactHeader, let a = account, a.kind == .loan {
                LoanProgressCard(account: a, compact: true)
            }

            if !compactHeader && showStatistics && model.scope != .investments {
                VStack(alignment: .leading, spacing: 4) {
                    if !chartCollapsed {
                        // iOS: 44 pt = HIG minimum hit region (the chart is tappable);
                        // macOS: room for the min/max/0 axis labels (GitHub #203).
                        #if os(iOS)
                        headerChart.frame(height: 44)
                        #else
                        headerChart.frame(height: 72)
                        #endif
                    }
                    // below the chart, also while it is hidden (#634)
                    HeaderChartControls(collapsed: $chartCollapsed) { chartUnitMenu }
                }
            }

            if !compactHeader {
            #if os(iOS)
            HStack(spacing: 10) {
                Text("Konsum \(kpiValue(-model.consumptionThisMonth))").foregroundStyle(Theme.outflow).sensitive(.amounts)
                Text("Einnahmen \(kpiValue(model.incomeThisMonth))").foregroundStyle(Theme.inflow).sensitive(.amounts)
                Spacer(minLength: 0)
            }
            .appFont(.caption, weight: .medium, monospacedDigit: true)
            .lineLimit(1)
            .minimumScaleFactor(0.7)
            #else
            HStack(spacing: 10) { kpis }
                .frame(maxWidth: .infinity)
                .fixedSize(horizontal: false, vertical: true)
            #endif
            }

            filterRow
        }
        .card(padding: ViewLayout.cardPadding, vertical: ViewLayout.cardVerticalPadding(collapsed: compactHeader))
    }

    private var isCategory: Bool {
        if case .category = model.shownSelection { return true }
        return false
    }

    /// Bar width next to the chart type: category sums (line or bars), balance bars.
    @ViewBuilder
    private var chartUnitMenu: some View {
        if chartStyle != .pie {
            if isCategory {
                BarUnitMenu(selection: Binding(get: { model.effectiveCategoryBarUnit },
                                               set: { model.categoryBarUnit = $0; model.reloadCategoryHistory() }),
                            units: BalanceBarUnit.categoryCases)
            } else if chartStyle == .bars {
                BarUnitMenu(selection: $barUnit, units: BalanceBarUnit.balanceCases,
                            resolved: barUnit == .automatic ? barUnit.resolved(dayCount: model.history.count) : nil)
            }
        }
    }

    /// Chart of the header in the chosen type (#634); a category's sums, else the balance history.
    @ViewBuilder
    private var headerChart: some View {
        if chartStyle == .pie {
            Group {
                if case .category(let id) = model.shownSelection {
                    HeaderPieChart(
                        slices: HeaderPieSlices.fold(HeaderPieSlices.subcategories(of: id, in: model.categories), limit: HeaderPieSlices.limit),
                        emptyText: "Keine Buchungen im Zeitraum"
                    ) { slice in
                        if let sub = Int64(slice) { model.sidebarSelection = .category(sub) }
                    }
                } else {
                    AccountExpensePie()
                }
            }
            .contextMenu { ChartStyleInlinePicker() }
        } else if case .category = model.shownSelection {
            CategorySumChart(sums: model.categoryHistory, style: chartStyle)
        } else {
            BalanceChart(points: model.history) { model.jumpToDay($0) }
        }
    }

    /// The tiles belong to the selection: no stale sums while it loads or after its load failed.
    private func kpiValue(_ cents: Int64) -> String {
        model.selectionFiguresMissing ? "–" : Money.format(cents, currency: model.balanceCurrency)
    }

    /// "Zusätzlich" line of a tile: its value in the currencies other than EUR (#1049); none while loading.
    private func kpiExtra(_ pick: (CurrencyFlow) -> Int64) -> String? {
        model.selectionFiguresMissing ? nil : model.otherCurrencyText(pick)
    }

    @ViewBuilder
    private var kpis: some View {
        KPI(title: "Konsumausgaben", subtitle: LocalizedStringKey(model.periodSubtitle), value: kpiValue(-model.consumptionThisMonth), tint: Theme.outflow,
            extra: kpiExtra { -$0.consumptionCents })
        KPI(title: "Investiert", subtitle: "Sparpläne & Perks", value: kpiValue(model.investedThisMonth), tint: Theme.invest,
            extra: kpiExtra { $0.investedCents })
        KPI(title: "Einnahmen", subtitle: LocalizedStringKey(model.periodSubtitle), value: kpiValue(model.incomeThisMonth), tint: Theme.inflow,
            extra: kpiExtra { $0.incomeCents })
        fixedCostsKPI
    }

    /// Recurring expenses still due this month (GitHub #35); opens the overview.
    @ViewBuilder
    private var fixedCostsKPI: some View {
        let open = model.recurringMonth?.items.filter { !$0.booked && $0.amountCents < 0 }.count ?? 0
        // like the other tiles: none of the previous selection's figures while loading or after a failed load
        let missing = model.selectionFiguresMissing
        Button { model.showRecurringSheet = true } label: {
            KPI(title: "Fixkosten offen", subtitle: missing ? "Diesen Monat" : "Diesen Monat · \(open) offen",
                value: missing ? "–" : Money.format(-(model.recurringMonth?.openExpenseCents ?? 0), currency: model.balanceCurrency), tint: Theme.outflow,
                disclosure: true)
        }
        .buttonStyle(.plain)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .help("Wiederkehrende Buchungen anzeigen")
        .accessibilityHint("Öffnet die Übersicht der wiederkehrenden Buchungen")
    }

    @ViewBuilder
    private var filterRow: some View {
        @Bindable var model = model
        // Never let the fixed-size filter controls widen the header card: they scroll when the
        // row is too narrow, the count stays at its end. Always the scroll view, no `ViewThatFits`:
        // the split view measures the column at width 0 on every frame of a sidebar animation or
        // window resize, and `ViewThatFits` then built a scrolling copy of the controls (menus,
        // pop-up buttons) and dropped it again in the layout right after (#1131).
        HStack(spacing: 12) {
            ScrollView(.horizontal, showsIndicators: false) {
                filterControls.fixedSize(horizontal: true, vertical: false)
            }
            .scrollBounceBehavior(.basedOnSize, axes: .horizontal)
            bookingCount
        }
        .appFont(.caption)
        .foregroundStyle(Theme.secondary)
        .coordinateSpace(.named(TagFilterButton.rowSpace))
        .capturablePopover(isPresented: $model.showCustomPeriod, arrowEdge: .bottom) {
            CustomPeriodForm()
        }
        #if os(macOS)
        // anchored on the tag button, so its arrow points there and not at the row's middle
        .capturablePopover(isPresented: $model.showTagFilter,
                           attachmentAnchor: tagButtonFrame.isEmpty ? .rect(.bounds) : .rect(.rect(tagButtonFrame)),
                           arrowEdge: .bottom) {
            TagFilterPopover()
        }
        #endif
    }

    /// The period menu (also above Sachwerte charts).
    private var periodMenu: some View { PeriodMenuButton() }

    /// Category views: only the bookings of the chosen accounts (GitHub #406).
    private var categoryAccountMenu: some View {
        Menu {
            Toggle("Alle Konten", isOn: Binding(
                get: { model.categoryAccountIDs.isEmpty },
                set: { _ in model.categoryAccountFilter = [] }
            ))
            Divider()
            ForEach(model.accountFilterCandidates, id: \.id) { a in
                Toggle(isOn: Binding(
                    get: { model.categoryAccountIDs.contains(a.id) },
                    set: { _ in model.toggleCategoryAccount(a.id) }
                )) {
                    Text(verbatim: a.bankName.isEmpty || a.name.localizedCaseInsensitiveContains(a.bankName) ? a.name : "\(a.name) · \(a.bankName)")
                }
            }
        } label: {
            Label(model.categoryAccountFilterTitle,
                  systemImage: model.categoryAccountIDs.isEmpty ? "building.columns" : "building.columns.fill")
                .lineLimit(1).fixedSize(horizontal: true, vertical: false)
        }
        .menuStyle(.borderlessButton)
        .fixedSize()
        .help("Nur Buchungen ausgewählter Konten anzeigen")
    }

    /// Amount range (#357): the pill opens the popover and shows the range while one is set;
    /// the cross next to it resets it with one click.
    @ViewBuilder
    private var amountFilterPill: some View {
        let filter = model.amountFilter
        HStack(spacing: 4) {
            // Looks like the account menu next to it: body size, primary colour, chevron.
            Button { showAmountFilter = true } label: {
                HStack(spacing: 4) {
                    if filter.isActive {
                        Label(filter.title, systemImage: "eurosign.circle.fill")
                            .sensitive(.amounts)
                    } else {
                        Label(filter.title, systemImage: "eurosign.circle")
                    }
                    Image(systemName: "chevron.down").font(.system(size: 9, weight: .semibold))
                }
                .appFont(.body)
                .foregroundStyle(Theme.text)
                .lineLimit(1).fixedSize(horizontal: true, vertical: false)
            }
            .buttonStyle(.plain)
            .help("Nach Betrag filtern")
            // On the pill itself, so the arrow points at it (not at the middle of the row).
            .capturablePopover(isPresented: $showAmountFilter, arrowEdge: .bottom) {
                AmountFilterForm(isPresented: $showAmountFilter)
            }
            if filter.isActive {
                Button { model.interactiveAmountFilter = AmountFilter() } label: {
                    Image(systemName: "xmark.circle.fill")
                }
                .buttonStyle(.borderless)
                .help("Betragsfilter zurücksetzen")
                .accessibilityLabel("Betragsfilter zurücksetzen")
            }
        }
        .fixedSize()
    }

    /// The natural-language reading of the typed search phrase (`AppModel.naturalSearch`): what the
    /// list really searches, so the field itself keeps what the user typed. × searches literally.
    @ViewBuilder
    private var naturalSearchChip: some View {
        if let natural = model.naturalSearch {
            HStack(spacing: 4) {
                Image(systemName: "sparkle.magnifyingglass")
                if natural.keyword.isEmpty {
                    Text("Natürliche Suche")
                } else {
                    Text("Suche:")
                    Text(natural.keyword).sensitive()
                }
                Button { model.searchLiterally() } label: {
                    Image(systemName: "xmark.circle.fill")
                }
                .buttonStyle(.borderless)
                .help("Wörtlich nach dem eingegebenen Text suchen")
                .accessibilityLabel("Wörtlich suchen")
            }
            .appFont(.body)
            .foregroundStyle(Theme.text)
            .lineLimit(1)
            .fixedSize()
        }
    }

    @ViewBuilder
    private var filterControls: some View {
        @Bindable var model = model
        let account = self.account
        HStack(spacing: 12) {
                periodMenu
                // One filter menu (status + income/expenses) keeps the row narrow.
                Menu {
                    // Inline pickers show no title in a menu: name each group with a section header.
                    Section("Status") {
                        Picker("Status", selection: $model.interactiveStatusFilter) {
                            ForEach(StatusFilter.allCases) { Text($0.title).tag($0) }
                        }
                        .pickerStyle(.inline)
                        .labelsHidden()
                    }
                    Section("Buchungsart") {
                        Picker("Buchungsart", selection: $model.interactiveDirectionFilter) {
                            ForEach(BookingDirection.ordered) { Text($0.title).tag($0) }
                        }
                        .pickerStyle(.inline)
                        .labelsHidden()
                    }
                } label: {
                    #if os(iOS)
                    if sizeClass == .compact {
                        // icon only: the period menu label takes most of the width
                        Image(systemName: model.filterSymbol)
                            .accessibilityLabel(model.filterTitle)
                    } else {
                        Label(model.filterTitle, systemImage: model.filterSymbol)
                            .lineLimit(1).fixedSize(horizontal: true, vertical: false)
                    }
                    #else
                    Label(model.filterTitle, systemImage: model.filterSymbol)
                        .lineLimit(1).fixedSize(horizontal: true, vertical: false)
                    #endif
                }
                .menuStyle(.borderlessButton)
                .fixedSize()
                .help("Nach Status oder nach Einnahmen und Ausgaben filtern")
                #if os(macOS)
                // tag filter (#228); the iOS layout follows later
                if model.showsTagFilter {
                    TagFilterButton { tagButtonFrame = $0 }
                }
                #endif
                if model.showsAccountFilter && model.accountFilterCandidates.count > 1 {
                    categoryAccountMenu
                }
                amountFilterPill
                naturalSearchChip
                if model.hasActiveListFilters {
                    // all list filters at once (#840); the period is a view choice and stays
                    Button { model.resetListFilters(inBackground: true) } label: {
                        Label("Filter zurücksetzen", systemImage: "xmark.circle")
                            .lineLimit(1).fixedSize(horizontal: true, vertical: false)
                            #if os(iOS)
                            .labelStyle(.iconOnly)
                            #endif
                    }
                    .buttonStyle(.borderless)
                    .help("Status, Buchungsart, Tags, Konten, Betrag und Suche zurücksetzen")
                }
                if let a = account {
                    Button { showDocuments = true } label: {
                        Label("Dokumente", systemImage: "doc.text")
                            .lineLimit(1).fixedSize(horizontal: true, vertical: false)
                            #if os(iOS)
                            .labelStyle(.iconOnly)
                            #endif
                    }
                    .buttonStyle(.borderless)
                    .capturableSheet(isPresented: $showDocuments) { AccountDocumentsSheet(account: a) }
                }
                if showStatistics, !model.showsOverview, model.scope != .investments, !model.allAccounts.isEmpty {
                    Button {
                        withAnimation(Motion.animation(.easeInOut(duration: 0.25))) { showAnalytics.toggle() }
                    } label: {
                        Label("Analysen", systemImage: "chart.bar.xaxis")
                            .lineLimit(1).fixedSize(horizontal: true, vertical: false)
                            #if os(iOS)
                            .labelStyle(.iconOnly)
                            #endif
                    }
                    .buttonStyle(.borderless)
                    .foregroundStyle(showAnalytics ? Color.accentColor : Theme.secondary)
                    .help("Cashflow und Budgets für dieses Konto ein-/ausblenden")
                }
            }
    }

    @ViewBuilder
    private var bookingCount: some View {
        #if os(iOS)
        if sizeClass != .compact {
            Text("\(model.bookingCountLabel) Buchungen")
                .appFont(.caption)
                .foregroundStyle(Theme.secondary)
                .lineLimit(1)
                .fixedSize(horizontal: true, vertical: false)
        }
        #else
        Text("\(model.bookingCountLabel) Buchungen")
            .appFont(.caption)
            .foregroundStyle(Theme.secondary)
            .lineLimit(1)
            .fixedSize(horizontal: true, vertical: false)
        #endif
    }
}

/// The period menu of the stream header and the depot view. A depot has no pay cycle, so it hides
/// "since last salary" there and names the period it really uses.
struct PeriodMenuButton: View {
    @Environment(AppModel.self) private var model
    var forDepot = false

    var body: some View {
        @Bindable var model = model
        Menu {
            // sections as in the usual period menus (#382), each an inline picker on the same selection
            ForEach(Array(Period.menu().enumerated()), id: \.offset) { _, section in
                Picker("Zeitraum", selection: $model.interactivePeriod) {
                    ForEach(section.filter { !(forDepot && $0 == .sinceSalary) }) { Text($0.title).tag($0) }
                }
                .pickerStyle(.inline)
                Divider()
            }
            // A Toggle gives the native menu checkmark while the custom range is active;
            // choosing it always opens the date dialog.
            Toggle(isOn: Binding(get: { model.period == .custom }, set: { _ in model.showCustomPeriod = true })) {
                if model.period == .custom {
                    Text("Benutzerdefiniert: \(model.periodTitle) …")
                } else {
                    Text("Benutzerdefiniert …")
                }
            }
        } label: {
            // Single period control (GitHub #102): the menu is the only selector,
            // so it always shows the active range.
            Label(forDepot ? model.depotPeriodTitle : model.periodTitle, systemImage: "calendar")
                .lineLimit(1)
        }
        .menuStyle(.borderlessButton)
        .fixedSize()
    }
}

struct KPI: View {
    let title: LocalizedStringKey
    let subtitle: LocalizedStringKey
    let value: String
    let tint: Color
    /// Tile opens a detail view: shows a chevron next to the title.
    var disclosure = false
    /// Amounts in currencies other than EUR, which `value` leaves out (#1049).
    var extra: String?

    var body: some View {
        // Title and amount sit at a fixed distance and the title keeps to one line, so the
        // amounts of all tiles line up (HIG: align elements, consistent spacing).
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 3) {
                Text(title).appFont(.caption, weight: .medium).foregroundStyle(Theme.secondary)
                    .lineLimit(1).minimumScaleFactor(0.85)
                if disclosure {
                    Image(systemName: "chevron.right")
                        .appFont(.caption2, weight: .bold).imageScale(.small)
                        .foregroundStyle(Theme.tertiary)
                        .accessibilityHidden(true)
                }
            }
            Text(value).appFont(.title3, weight: .semibold, monospacedDigit: true).foregroundStyle(tint)
                .lineLimit(1).minimumScaleFactor(0.75)
                .sensitive(.amounts)
            if let extra {
                Text("Zusätzlich: \(extra)")
                    .appFont(.caption2, monospacedDigit: true).foregroundStyle(Theme.secondary)
                    .lineLimit(1)
                    .sensitive(.amounts)
                    .help("Weitere Währungen werden nicht umgerechnet und nicht in die Summe einbezogen.")
            }
            Spacer(minLength: 2)
            Text(subtitle).appFont(.caption2).foregroundStyle(Theme.tertiary)
                .lineLimit(2)
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
    @Environment(AppModel.self) private var model
    let points: [DailyBalance]
    /// Click on a day (jump to its bookings).
    var onSelect: ((Date) -> Void)? = nil
    /// Day or bar under the mouse pointer. Only the overlay and the accessibility value read it,
    /// each in a view of its own: a hover step redraws the marker without re-running `body`,
    /// where Charts re-evaluates every mark (up to ~1800 on long histories, every frame).
    @State private var hover = Hover()
    @AppStorage(BalanceChartStyle.defaultsKey) private var style: BalanceChartStyle = .line
    @AppStorage(BalanceBarUnit.defaultsKey) private var barUnit: BalanceBarUnit = .automatic

    /// Width of the chart: the x-axis spaces its labels by it (no overlapping years). A reference,
    /// so a new width is no state change: a sidebar animation or window resize changes it every
    /// frame, and each body run makes Charts rebuild the chart.
    @State private var chartWidth = ChartWidth()
    /// Label spacing at the last width: the only state the width changes, so the body runs
    /// again only when the labels move.
    @State private var xStride = 0
    /// Height of the chart: the y-axis keeps its labels apart by it (large text in a small chart).
    @State private var chartHeight: CGFloat = 0

    private final class ChartWidth {
        var value: CGFloat = 600
    }

    @Observable fileprivate final class Hover {
        /// Day under the mouse pointer (hover readout: date + balance).
        var point: Point?
        /// Bar under the mouse pointer (bar style: month/day + average balance).
        var bar: BalanceBucket?
    }

    /// A point of `AreaPlot`/`LinePlot`. `floor` is the area's lower edge, the same in every
    /// point: the plots take only key paths.
    private struct PlotPoint {
        let date: Date
        let euros: Double
        let floor: Double
    }

    fileprivate struct Point: Identifiable, Equatable {
        let id: Int
        let date: Date
        let cents: Int64
        var euros: Double { Double(cents) / 100 } // display only; data stays in cents
    }

    /// The chart's first render compiles Metal shaders (gradient + mask, ~1 s on a cold start),
    /// so it is mounted one run-loop tick + a short delay after the list and cards are shown.
    /// Only the first time: a chart rebuilt later (e.g. depot -> giro) shows at once.
    @State private var mounted = BalanceChart.shadersWarm
    @MainActor private static var shadersWarm = false

    var body: some View {
        Group {
            if mounted { chartBody } else { Color.clear }
        }
        .task {
            guard !mounted else { return }
            try? await Task.sleep(nanoseconds: 250_000_000)
            StartupClock.mark("BalanceChart mounting")
            mounted = true
            Self.shadersWarm = true
        }
    }

    /// Prepared once per data change, not on every redraw: a scroll or hover step only moves the
    /// marker, and parsing thousands of days each time made it stutter. Filled while the body
    /// is evaluated (a reference type, so no state change), keyed on the data it was built
    /// from: a frame never pairs new `points` with arrays of the old ones.
    private final class Prepared {
        var key = 0
        var unit: BalanceBarUnit?
        var pts: [Point] = []
        /// `pts` thinned to what the chart width can show (extremes of each stretch kept).
        var drawn: [Point] = []
        /// `drawn` at `dayMiddle`.
        var plotted: [PlotPoint] = []
        /// Lowest and highest balance (axis labels) and the padded y-scale around them.
        var low = 0.0
        var high = 1.0
        var yDomain: ClosedRange<Double> {
            let pad = max((high - low) * 0.15, 1)
            return (low - pad)...(high + pad)
        }
        /// Whole days from the first point to the last: the x-scale `unit: .day` marks get.
        var xDomain: ClosedRange<Date> = Date.distantPast...Date.distantPast
        var buckets: [BalanceBucket] = []
    }
    @State private var prepared = Prepared()

    /// A year of days is drawn in full: 367 (both ends and a leap day), rounded up to whole
    /// stretches of `thinned`. Longer histories are thinned, where the hover marker may sit off
    /// the line. Charts lays out every point again at each new width (sidebar animation, window
    /// resize), so the count sets the cost of each such frame.
    private static let maxDrawnPoints = 368

    /// Where `unit: .day` marks put a day, and so where the plots and the marker do.
    private static func dayMiddle(_ date: Date) -> Date {
        Calendar.current.dateInterval(of: .day, for: date).map { $0.start + $0.duration / 2 } ?? date
    }

    /// The day under `x` (days are drawn across their whole span), else the nearest one.
    private static func day(atX x: CGFloat, proxy: ChartProxy, in all: [Point]) -> Point? {
        guard let date: Date = proxy.value(atX: x) else { return nil }
        let start = Calendar.current.startOfDay(for: date)
        return all.min { abs($0.date.timeIntervalSince(start)) < abs($1.date.timeIntervalSince(start)) }
    }

    /// "Mo. 9. Nov. 2026": the marker readout and the day bar's title.
    private static let dayFormat = Date.FormatStyle.dateTime.weekday(.abbreviated).day().month(.abbreviated).year()

    /// Cheap fingerprint of `points`: length, ends and the sum of all balances.
    private var dataKey: Int {
        var h = Hasher()
        h.combine(points.count)
        h.combine(points.first?.date)
        h.combine(points.last?.date)
        h.combine(points.reduce(Int64(0)) { $0 &+ $1.balanceCents })
        return h.finalize()
    }

    private var current: Prepared {
        let key = dataKey
        if prepared.key != key || prepared.unit == nil {
            let all: [Point] = points.enumerated().compactMap { i, p in
                DateText.date(p.date).map { Point(id: i, date: $0, cents: p.balanceCents) }
            }
            prepared.pts = all
            prepared.drawn = Self.thinned(all, to: Self.maxDrawnPoints)
            prepared.low = all.map(\.euros).min() ?? 0
            prepared.high = all.map(\.euros).max() ?? 1
            let floor = prepared.yDomain.lowerBound
            prepared.plotted = prepared.drawn.map { PlotPoint(date: Self.dayMiddle($0.date), euros: $0.euros, floor: floor) }
            let calendar = Calendar.current
            if let first = all.first.flatMap({ calendar.dateInterval(of: .day, for: $0.date) }),
               let last = all.last.flatMap({ calendar.dateInterval(of: .day, for: $0.date) }) {
                prepared.xDomain = first.start...last.end
            } else {
                let today = calendar.startOfDay(for: .now)
                prepared.xDomain = today...today
            }
            prepared.key = key
            prepared.unit = nil
        }
        if prepared.unit != barUnit {
            prepared.buckets = BalanceBuckets.make(points, unit: barUnit)
            prepared.unit = barUnit
        }
        return prepared
    }

    private var pts: [Point] { current.pts }
    private var buckets: [BalanceBucket] { current.buckets }

    /// Keeps first, lowest, highest and last point of each stretch, so peaks and dips survive.
    private static func thinned(_ all: [Point], to limit: Int) -> [Point] {
        guard all.count > limit else { return all }
        let stretches = limit / 4
        let size = Double(all.count) / Double(stretches)
        var out: [Point] = []
        out.reserveCapacity(limit + 8)
        for k in 0..<stretches {
            let s = Int(Double(k) * size), e = min(all.count, Int(Double(k + 1) * size))
            guard s < e else { continue }
            let slice = all[s..<e]
            guard let lo = slice.min(by: { $0.cents < $1.cents }), let hi = slice.max(by: { $0.cents < $1.cents }) else { continue }
            for p in [slice[s], lo, hi, slice[e - 1]].sorted(by: { $0.id < $1.id }) where out.last?.id != p.id { out.append(p) }
        }
        return out
    }

    /// Point of the day at the top of the scrolled booking list (#593): that day's closing
    /// balance, or the latest earlier one when the day has no point of its own.
    private func followedPoint(in all: [Point]) -> Point? {
        guard let day = model.scrollFocusDay, let date = DateText.date(day) else { return nil }
        return all.last { $0.date <= date } ?? all.first
    }

    private var resolvedBarUnit: BalanceBarUnit { barUnit.resolved(dayCount: points.count) }

    /// What this chart draws: the header shows the pie and hides the chart itself (#634),
    /// so everything but bars is the line here (also for depots, which have no pie).
    private var drawStyle: BalanceChartStyle { style == .bars ? .bars : .line }

    /// Keyboard: ←/→ walk the days (bars: the months), the hover readout follows; Return jumps
    /// to the bookings, Esc ends. VoiceOver: summary, adjustable by day / bar.
    @FocusState private var chartFocused: Bool

    private func step(_ delta: Int) {
        if drawStyle == .bars {
            let all = buckets
            guard !all.isEmpty else { return }
            let current = hover.bar.map(\.id) ?? (delta > 0 ? -1 : all.count)
            hover.bar = all[min(max(current + delta, 0), all.count - 1)]
            return
        }
        let all = pts
        guard !all.isEmpty else { return }
        let current = hover.point.flatMap { h in all.firstIndex { $0.id == h.id } } ?? (delta > 0 ? -1 : all.count)
        hover.point = all[min(max(current + delta, 0), all.count - 1)]
    }

    private var summary: String {
        let all = pts
        guard let first = all.first, let last = all.last,
              let low = all.min(by: { $0.cents < $1.cents }), let high = all.max(by: { $0.cents < $1.cents }) else {
            return String(localized: "Keine Daten")
        }
        let d = Date.FormatStyle.dateTime.day().month(.wide)
        let cur = model.balanceCurrency
        return String(localized: "Von \(Money.format(first.cents, currency: cur)) am \(first.date.formatted(d)) auf \(Money.format(last.cents, currency: cur)) am \(last.date.formatted(d)). Tiefststand \(Money.format(low.cents, currency: cur)) am \(low.date.formatted(d)), Höchststand \(Money.format(high.cents, currency: cur)) am \(high.date.formatted(d)).")
    }

    private var currentValue: String {
        if drawStyle == .bars, let b = hover.bar {
            return String(localized: "\(barTitle(b)): Ø \(Money.format(b.averageCents, currency: model.balanceCurrency)), Ende \(Money.format(b.closingCents, currency: model.balanceCurrency))")
        }
        guard let h = hover.point else { return summary }
        return "\(h.date.formatted(.dateTime.weekday(.wide).day().month(.wide))): \(Money.format(h.cents, currency: model.balanceCurrency))"
    }

    private func barTitle(_ b: BalanceBucket) -> String {
        switch resolvedBarUnit {
        case .year: b.start.formatted(.dateTime.year())
        case .quarter: b.start.formatted(.dateTime.quarter().year())
        case .month: b.start.formatted(.dateTime.month(.wide).year())
        case .day, .automatic: b.start.formatted(Self.dayFormat)
        }
    }

    /// Long histories label years, otherwise months; the year separators mark each January 1st.
    private func spanDays(_ first: Date?, _ last: Date?) -> Double {
        guard let first, let last else { return 0 }
        return last.timeIntervalSince(first) / 86_400
    }

    /// Days the x-axis covers in the drawn style.
    private var xSpan: Double {
        drawStyle == .bars ? spanDays(buckets.first?.start, buckets.last?.last) : spanDays(pts.first?.date, pts.last?.date)
    }

    /// Keeps `xStride` at the spacing the axis draws. A new span or text size changes the drawn
    /// spacing without a width change; a stale value could then swallow the next real change.
    private func syncXStride() {
        let count = Self.xAxisStride(span: xSpan, width: chartWidth.value, textScale: textScale)
        if count != xStride { xStride = count }
    }

    private func clearHover() { hover.point = nil; hover.bar = nil }

    /// Jumps to the bookings of the hovered day / bar; false if nothing is hovered.
    @discardableResult private func selectHovered() -> Bool {
        guard let onSelect else { return false }
        switch drawStyle {
        case .bars:
            guard let b = hover.bar else { return false }
            onSelect(b.last)
        default:
            guard let h = hover.point else { return false }
            onSelect(h.date)
        }
        return true
    }

    @ViewBuilder private var chartBody: some View {
        interactiveChart
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("Saldoverlauf")
            .modifier(HoverObservers(hover: hover, value: { currentValue }))
            .accessibilityHint(onSelect == nil ? "" : String(localized: "Pfeiltasten wählen einen Tag, Eingabe zeigt seine Buchungen."))
            .accessibilityAdjustableAction { direction in
                switch direction {
                case .increment: step(1)
                case .decrement: step(-1)
                @unknown default: break
                }
            }
            .accessibilityAction(named: "Buchungen dieses Tages zeigen") { selectHovered() }
            .accessibilityChartDescriptor(BalanceChartDescriptor(points: pts.map { ($0.date, $0.euros) }, currency: model.balanceCurrency))
    }

    @ViewBuilder private var interactiveChart: some View {
        // reads `xStride`, so a new spacing runs the body again
        let _ = xStride
        Group {
            if drawStyle == .bars { barChart } else { lineChart }
        }
        .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { width in
            chartWidth.value = width
            syncXStride()
        }
        .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { chartHeight = $0 }
        .onChange(of: points.first?.date) { _, _ in clearHover() }
        .onChange(of: points.count) { _, _ in clearHover() }
        .onChange(of: xSpan) { _, _ in syncXStride() }
        .onChange(of: textScale) { _, _ in syncXStride() }
        .onChange(of: style) { _, _ in clearHover() }
        .onChange(of: barUnit) { _, _ in hover.bar = nil }
        .onDisappear { model.chartHoverDay = nil }
        .contextMenu { styleMenu }
        #if os(macOS)
        .focusable(interactions: .edit)
        .focused($chartFocused)
        .onKeyPress(.leftArrow) { step(-1); return .handled }
        .onKeyPress(.rightArrow) { step(1); return .handled }
        .onKeyPress(.return) { selectHovered() ? .handled : .ignored }
        .onKeyPress(.escape) {
            guard hover.point != nil || hover.bar != nil else { return .ignored }
            clearHover()
            return .handled
        }
        .onChange(of: chartFocused) { _, focused in
            if !focused { clearHover() }
            model.chartHasKeyboardFocus = focused
        }
        #endif
    }

    /// Right click (long press on iOS): switch between line and bars right at the chart.
    @ViewBuilder private var styleMenu: some View {
        ChartStyleInlinePicker()
        if drawStyle == .bars {
            Picker("Balken je", selection: $barUnit) {
                ForEach(BalanceBarUnit.balanceCases) { Text($0.title).tag($0) }
            }
            .pickerStyle(.inline)
        }
    }

    // MARK: line

    /// Marker for the hovered day, else the day at the top of the scrolled list. Drawn in the
    /// overlay, outside the chart's marks, so moving it does not rebuild the line.
    @ViewBuilder private func marker(in all: [Point], proxy: ChartProxy, geo: GeometryProxy) -> some View {
        if let m = hover.point ?? followedPoint(in: all), let plot = proxy.plotFrame,
           let x = proxy.position(forX: Self.dayMiddle(m.date)), let y = proxy.position(forY: m.euros) {
            let frame = geo[plot]
            let left = m.id * 2 > all.count
            ZStack(alignment: .topLeading) {
                Rectangle().fill(Theme.secondary.opacity(0.45))
                    .frame(width: 1, height: frame.height)
                    .offset(x: frame.minX + x, y: frame.minY)
                Circle().fill(Theme.accent).frame(width: 6, height: 6)
                    .offset(x: frame.minX + x - 3, y: frame.minY + y - 3)
                PlotReadoutLayout(plot: frame, before: x, after: x, left: left) {
                    readout(m.date.formatted(Self.dayFormat), cents: m.cents)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            .allowsHitTesting(false)
        }
    }

    private static let lineColor = Theme.accent.opacity(0.8)
    private static let lineStroke = StrokeStyle(lineWidth: 1.5)

    /// Area and line of the balance. On macOS 15 / iOS 18 one plot each instead of two marks per
    /// point: Charts builds every mark again at each new width.
    ///
    /// Deliberately not a `@ChartContentBuilder` with `if #available` inside: `ChartContentBuilder`'s
    /// `buildPartialBlock` is obsoleted from macOS 15 / iOS 18 on, and the Xcode on the CI runner
    /// (macos-26 image) still picks it for a block inside such a check, then rejects it
    /// ("'buildPartialBlock(first:)' is unavailable"). Newer compilers accept that code, so a local
    /// build passing proves nothing. Here the plots are joined with an explicit call to the
    /// macOS 15 `buildBlock`, and the type erasure matches what `buildLimitedAvailability` does.
    private static func balanceSeries(_ cur: Prepared) -> AnyChartContent {
        if #available(macOS 15, iOS 18, *) {
            let area = AreaPlot(cur.plotted, x: .value("Tag", \.date), yStart: .value("Min", \.floor), yEnd: .value("Saldo", \.euros))
                .foregroundStyle(Theme.chartAreaFill)
                .interpolationMethod(.monotone)
            let line = LinePlot(cur.plotted, x: .value("Tag", \.date), y: .value("Saldo", \.euros))
                .foregroundStyle(lineColor)
                .lineStyle(lineStroke)
                .interpolationMethod(.monotone)
            return AnyChartContent(erasing: ChartContentBuilder.buildBlock(area, line))
        }
        let floor = cur.yDomain.lowerBound
        return AnyChartContent(erasing: ForEach(cur.drawn) { p in
            AreaMark(x: .value("Tag", p.date, unit: .day), yStart: .value("Min", floor), yEnd: .value("Saldo", p.euros))
                .foregroundStyle(Theme.chartAreaFill)
                .interpolationMethod(.monotone)
            LineMark(x: .value("Tag", p.date, unit: .day), y: .value("Saldo", p.euros))
                .foregroundStyle(lineColor)
                .lineStyle(lineStroke)
                .interpolationMethod(.monotone)
        })
    }

    @ViewBuilder private var lineChart: some View {
        let cur = current
        let pts = cur.pts
        let yDomain = cur.yDomain
        let years = pts.first.flatMap { f in pts.last.map { BalanceBuckets.yearStarts(from: f.date, to: $0.date) } } ?? []
        Chart {
            ForEach(years, id: \.self) { y in
                RuleMark(x: .value("Jahr", y))
                    .foregroundStyle(Theme.tertiary.opacity(0.5))
                    .lineStyle(StrokeStyle(lineWidth: 1, dash: [2, 3]))
            }
            Self.balanceSeries(cur)
        }
        .chartXScale(domain: cur.xDomain)
        .chartYScale(domain: yDomain)
        .chartYAxis { yAxis(min: cur.low, max: cur.high, domain: yDomain) }
        .chartXAxis { xAxis(span: xSpan) }
        .chartOverlay { proxy in
            GeometryReader { geo in
                ObservationScope { marker(in: pts, proxy: proxy, geo: geo) }
                Rectangle().fill(.clear).contentShape(Rectangle())
                    .onContinuousHover { phase in
                        switch phase {
                        case .active(let location):
                            guard let plot = proxy.plotFrame else { return }
                            let nearest = Self.day(atX: location.x - geo[plot].origin.x, proxy: proxy, in: pts)
                            if nearest != hover.point { hover.point = nearest }
                        case .ended:
                            hover.point = nil
                        }
                    }
                    .onTapGesture { location in
                        guard let onSelect, let plot = proxy.plotFrame,
                              let nearest = Self.day(atX: location.x - geo[plot].origin.x, proxy: proxy, in: pts)
                        else { return }
                        onSelect(nearest.date)
                    }
            }
        }
    }

    // MARK: bars

    @ViewBuilder private var barChart: some View {
        let bars = current.buckets
        let barUnit = resolvedBarUnit
        let unit = barUnit.calendarComponent
        let xLabel: LocalizedStringKey = switch barUnit {
        case .year: "Jahr"
        case .quarter: "Quartal"
        case .month: "Monat"
        case .day, .automatic: "Tag"
        }
        let lo = min(0, bars.map(\.euros).min() ?? 0)
        let hi = max(0, bars.map(\.euros).max() ?? 1)
        let pad = max((hi - lo) * 0.08, 1)
        // yearly bars: every gap is already a turn of the year
        let years = barUnit == .year ? []
            : bars.first.flatMap { f in bars.last.map { BalanceBuckets.yearStarts(from: f.start, to: $0.last) } } ?? []
        Chart {
            ForEach(bars) { b in
                BarMark(x: .value(xLabel, b.start, unit: unit), y: .value("Ø Saldo", b.euros))
                    .foregroundStyle(b.averageCents < 0 ? Theme.outflow : Theme.accent)
                    .opacity(0.85)
                    .cornerRadius(1.5)
            }
            // Plain date (no unit): the rule sits on the boundary between December and January.
            ForEach(years, id: \.self) { y in
                RuleMark(x: .value("Jahr", y))
                    .foregroundStyle(Theme.tertiary.opacity(0.6))
                    .lineStyle(StrokeStyle(lineWidth: 1, dash: [2, 3]))
            }
        }
        .chartYScale(domain: (lo - (lo < 0 ? pad : 0))...(hi + pad))
        .chartYAxis {
            yAxis(min: bars.map(\.euros).min() ?? 0, max: bars.map(\.euros).max() ?? 0,
                  domain: (lo - (lo < 0 ? pad : 0))...(hi + pad))
        }
        .chartXAxis { xAxis(span: xSpan, centered: barUnit != .day) }
        .chartOverlay { proxy in
            GeometryReader { geo in
                ObservationScope { barHighlight(in: bars, unit: barUnit, proxy: proxy, geo: geo) }
                Rectangle().fill(.clear).contentShape(Rectangle())
                    .onContinuousHover { phase in
                        switch phase {
                        case .active(let location):
                            guard let plot = proxy.plotFrame,
                                  let date: Date = proxy.value(atX: location.x - geo[plot].origin.x) else { return }
                            let hit = Self.bar(at: date, in: bars, unit: barUnit)
                            if hit != hover.bar { hover.bar = hit }
                        case .ended:
                            hover.bar = nil
                        }
                    }
                    .onTapGesture { location in
                        guard let onSelect, let plot = proxy.plotFrame,
                              let date: Date = proxy.value(atX: location.x - geo[plot].origin.x),
                              let hit = Self.bar(at: date, in: bars, unit: barUnit)
                        else { return }
                        onSelect(hit.last)
                    }
            }
        }
    }

    /// The hovered bar, else the bar of the day at the top of the scrolled booking list (#593):
    /// a veil dims the plot beside it and the readout names it. Drawn in the overlay like the
    /// line chart's `marker`, so the marks never depend on it: Charts re-evaluates every mark
    /// whenever `body` runs, which is slow on long histories, and the scroll position changes
    /// many times a second.
    @ViewBuilder private func barHighlight(in bars: [BalanceBucket], unit: BalanceBarUnit,
                                           proxy: ChartProxy, geo: GeometryProxy) -> some View {
        if let b = hover.bar ?? followedBar(in: bars, unit: unit), let plot = proxy.plotFrame,
           let x0 = proxy.position(forX: b.start),
           let x1 = unit.end(after: b.start).flatMap({ proxy.position(forX: $0) }) {
            let frame = geo[plot]
            // The chart sits on a `.card()`: a bar at 0.85 under this shows at about 0.35.
            let veil = Theme.card.opacity(0.59)
            let gapStart = min(max(0, x0), frame.width)
            ZStack(alignment: .topLeading) {
                HStack(spacing: 0) {
                    veil.frame(width: gapStart)
                    Color.clear.frame(width: max(0, min(x1, frame.width) - gapStart))
                    veil
                }
                .frame(width: frame.width, height: frame.height)
                .offset(x: frame.minX, y: frame.minY)
                PlotReadoutLayout(plot: frame, before: x0, after: x1, left: b.id * 2 > bars.count) {
                    readout(barTitle(b), prefix: "Ø ", cents: b.averageCents)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            .allowsHitTesting(false)
        }
    }

    private func followedBar(in bars: [BalanceBucket], unit: BalanceBarUnit) -> BalanceBucket? {
        guard let day = model.scrollFocusDay, let date = DateText.date(day) else { return nil }
        return Self.bar(at: date, in: bars, unit: unit)
    }

    /// The bar containing `date`, else the one with the nearest start. Binary search over the
    /// date-ordered bars: the scroll position asks many times a second.
    static func bar(at date: Date, in bars: [BalanceBucket], unit: BalanceBarUnit) -> BalanceBucket? {
        var lo = 0, hi = bars.count
        while lo < hi {
            let mid = (lo + hi) / 2
            if bars[mid].start <= date { lo = mid + 1 } else { hi = mid }
        }
        // bars[lo - 1] is the last bar starting on or before `date`
        if lo > 0, let end = unit.end(after: bars[lo - 1].start), date < end { return bars[lo - 1] }
        return bars[max(0, lo - 1)..<min(bars.count, lo + 1)]
            .min { abs($0.start.timeIntervalSince(date)) < abs($1.start.timeIntervalSince(date)) }
    }

    // MARK: shared

    /// Minimum, maximum and zero on a slim trailing axis (zero only if it lies in the shown
    /// range). Labels closer than a fifth of the height to one already placed are left out,
    /// so the 56 pt chart never stacks two labels on top of each other.
    @AxisContentBuilder private func yAxis(min lo: Double, max hi: Double, domain: ClosedRange<Double>) -> some AxisContent {
        // two labels need at least their own height (plus air) between them; the x axis takes its share of the height
        let plot = chartHeight - 20 * textScale
        let labelGap = plot > 0 ? 18 * textScale / plot : 0.2
        let values = Self.axisValues(min: lo, max: hi, domain: domain, minGap: Swift.max(0.2, labelGap))
        AxisMarks(position: .trailing, values: values) { v in
            let isZero = v.as(Double.self) == 0
            AxisGridLine(stroke: StrokeStyle(lineWidth: isZero ? 0.75 : 0.5, dash: isZero ? [] : [2, 3]))
                .foregroundStyle(Theme.tertiary.opacity(isZero ? 0.7 : 0.45))
            AxisValueLabel {
                if let d = v.as(Double.self) {
                    // fixed minimum width: changing values never shift the plot sideways
                    Text(verbatim: Self.compactEuros(d, currency: model.balanceCurrency))
                        .sensitive(.amounts)
                        .appFont(.caption2, monospacedDigit: true)
                        .foregroundStyle(Theme.tertiary)
                        .fixedSize()
                        .frame(minWidth: axisLabelWidth * textScale, alignment: .leading)
                }
            }
        }
    }

    /// Fits "-88.800 €" in Caption 2; longer labels (millions) widen it only then.
    /// ScaledMetric follows Dynamic Type (iOS), textScale the app setting (macOS).
    @ScaledMetric(relativeTo: .caption2) private var axisLabelWidth: CGFloat = 50
    @Environment(\.textScale) private var textScale

    /// Zero first (the reference), then max and min, each only if it keeps its distance.
    static func axisValues(min lo: Double, max hi: Double, domain: ClosedRange<Double>, minGap: Double = 0.2) -> [Double] {
        let gap = (domain.upperBound - domain.lowerBound) * minGap
        var out: [Double] = []
        for v in [0, hi, lo] where domain.contains(v) && out.allSatisfy({ abs($0 - v) >= gap }) {
            out.append(v)
        }
        return out
    }

    /// "4.820 €", "-850 €", "0 €", "1,2 Mio. €": three significant digits, short enough for the
    /// slim axis. German compact notation only abbreviates from millions on (and then drops
    /// the thousands separator below), so smaller amounts use the grouped number style.
    static func compactEuros(_ euros: Double, currency: String = "EUR") -> String {
        let digits = FloatingPointFormatStyle<Double>.number.precision(.significantDigits(1...3))
        // compact currency notation needs macOS 15; the compact number style is older
        let text = abs(euros) < 1_000_000 ? euros.formatted(digits) : euros.formatted(digits.notation(.compactName))
        return text + " " + Money.symbol(currency)
    }

    /// Every how many units (years, months, days) the x-axis gets a label: the smallest
    /// of `steps` for which all labels fit into `width` (each needs `labelWidth`).
    static func axisStride(units: Double, width: CGFloat, labelWidth: CGFloat, steps: [Int]) -> Int {
        let fit = max(Double(width / labelWidth), 1)
        return steps.first { units / Double($0) <= fit } ?? steps.last ?? 1
    }

    /// Label spacing of the x-axis over `span` days: years for long periods (every 1/2/5/10 so
    /// the labels never crowd), weekly days for short ones (this/last month), else months.
    static func xAxisStride(span: Double, width: CGFloat, textScale: CGFloat) -> Int {
        if span > 400 {
            axisStride(units: span / 365.25, width: width, labelWidth: 64 * textScale, steps: [1, 2, 5, 10, 20])
        } else if span < 62 {
            axisStride(units: span, width: width, labelWidth: 64 * textScale, steps: [7, 14, 28])
        } else {
            axisStride(units: span / 30.44, width: width, labelWidth: 44 * textScale, steps: [1, 2, 3, 6])
        }
    }

    /// `centered`: bar mode puts a year/month label under the middle of its bar, not its start.
    @AxisContentBuilder private func xAxis(span: Double, centered: Bool = false) -> some AxisContent {
        let count = Self.xAxisStride(span: span, width: chartWidth.value, textScale: textScale)
        if span > 400 {
            AxisMarks(values: .stride(by: .year, count: count)) { v in
                // centred = midway to the next label: only right if that is the next year
                AxisValueLabel(centered: centered && count == 1) { Self.axisDate(v, .dateTime.year()) }
                    .foregroundStyle(Theme.tertiary)
            }
        } else if span < 62 {
            AxisMarks(values: .stride(by: .day, count: count)) { v in
                AxisValueLabel { Self.axisDate(v, .dateTime.day().month(.abbreviated)) }
                    .foregroundStyle(Theme.tertiary)
            }
        } else {
            AxisMarks(values: .stride(by: .month, count: count)) { v in
                AxisValueLabel(centered: centered && count == 1) { Self.axisDate(v, .dateTime.month(.abbreviated)) }
                    .foregroundStyle(Theme.tertiary)
            }
        }
    }

    /// `AxisValueLabel(format:)` keeps the system size; this follows the text size setting.
    private static func axisDate(_ value: AxisValue, _ format: Date.FormatStyle) -> some View {
        Text(verbatim: value.as(Date.self)?.formatted(format) ?? "").appFont(.caption2).fixedSize()
    }

    private func readout(_ title: String, prefix: String = "", cents: Int64) -> some View {
        VStack(spacing: 1) {
            Text(title)
                .appFont(.caption2)
                .foregroundStyle(Theme.secondary)
            Text(verbatim: prefix + Money.format(cents, currency: model.balanceCurrency))
                .sensitive(.amounts)
                .appFont(.caption, weight: .semibold, monospacedDigit: true)
                .foregroundStyle(cents < 0 ? Theme.outflow : Theme.text)
        }
        .padding(.horizontal, 7)
        .padding(.vertical, 4)
        .background(Theme.card, in: RoundedRectangle(cornerRadius: 6, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 6, style: .continuous).strokeBorder(Theme.border, lineWidth: 1))
    }
}

/// Places a chart readout at the top of `plot`, 4 pt before (`left`) or after the marked x range
/// (plot coordinates), and keeps it inside the plot like an annotation's `.fit(to: .chart)`.
private struct PlotReadoutLayout: Layout {
    let plot: CGRect
    let before: CGFloat
    let after: CGFloat
    let left: Bool

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        proposal.replacingUnspecifiedDimensions()
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        guard let readout = subviews.first else { return }
        let width = readout.sizeThatFits(.unspecified).width
        let x = min(max(left ? before - 4 - width : after + 4, 0), max(0, plot.width - width))
        readout.place(at: CGPoint(x: bounds.minX + plot.minX + x, y: bounds.minY + plot.minY), proposal: .unspecified)
    }
}

/// The parts of `BalanceChart` that follow the hover: the accessibility value and the day the
/// booking list highlights (#593). A modifier of its own, so only it re-runs on a hover step.
private struct HoverObservers: ViewModifier {
    @Environment(AppModel.self) private var model
    let hover: BalanceChart.Hover
    let value: () -> String

    func body(content: Content) -> some View {
        content
            .accessibilityValue(value())
            .onChange(of: hover.point) { _, h in model.chartHoverDay = h.map { DateText.iso($0.date) } }
            .onChange(of: hover.bar) { _, b in model.chartHoverDay = b.map { DateText.iso($0.last) } }
    }
}

/// Builds `content` in a body of its own: observable state it reads invalidates this view,
/// not the one that made the closure.
private struct ObservationScope<Content: View>: View {
    @ViewBuilder let content: () -> Content
    var body: some View { content() }
}

/// Audio Graph / VoiceOver chart description of the balance history.
private struct BalanceChartDescriptor: AXChartDescriptorRepresentable {
    let points: [(Date, Double)]
    var currency = "EUR"

    func makeChartDescriptor() -> AXChartDescriptor {
        let dates = points.map(\.0)
        let values = points.map(\.1)
        let lo = dates.first?.timeIntervalSince1970 ?? 0
        let hi = dates.last?.timeIntervalSince1970 ?? 1
        let x = AXNumericDataAxisDescriptor(
            title: String(localized: "Datum"), range: lo...max(hi, lo + 1), gridlinePositions: []
        ) { Date(timeIntervalSince1970: $0).formatted(date: .abbreviated, time: .omitted) }
        let y = AXNumericDataAxisDescriptor(
            title: String(localized: "Saldo"), range: (values.min() ?? 0)...max(values.max() ?? 1, (values.min() ?? 0) + 1), gridlinePositions: []
        ) { Money.format(Int64(($0 * 100).rounded()), currency: currency) }
        let series = AXDataSeriesDescriptor(
            name: String(localized: "Saldo"), isContinuous: true,
            dataPoints: points.map { AXDataPoint(x: $0.0.timeIntervalSince1970, y: $0.1) }
        )
        return AXChartDescriptor(title: String(localized: "Saldoverlauf"), summary: nil, xAxis: x, yAxis: y, additionalAxes: [], series: [series])
    }
}

/// Value series of a securities depot (#465): the daily walk-back carries today's value
/// flat across the whole period, so the line is cut to the real value dates: nothing
/// before the first change and nothing after the last snapshot (`lastDate`).
///
/// Interim: the cut follows the first and last *change* of the value. That is a heuristic of the
/// booking walk back, not an interface: several real valuations may share one value. A real
/// valuation history (one point per valuation day) should hand over its first and last day instead,
/// and this cut goes away.
enum DepotChartSeries {
    static func points(_ all: [DailyBalance], lastDate: String) -> [DailyBalance] {
        var pts = lastDate.isEmpty ? all : all.filter { $0.date <= lastDate }
        guard let firstChange = pts.indices.dropFirst().first(where: { pts[$0].balanceCents != pts[$0 - 1].balanceCents }) else {
            return []   // never changed: a single value, not a line
        }
        pts = Array(pts[(firstChange - 1)...])
        // drop a flat tail after the last change (not carried forward to today)
        if let lastChange = pts.indices.dropFirst().last(where: { pts[$0].balanceCents != pts[$0 - 1].balanceCents }) {
            pts = Array(pts[...lastChange])
        }
        return pts
    }
}

/// Popover of the amount filter (#357): from / to as absolute amounts, empty = open.
struct AmountFilterForm: View {
    @Environment(AppModel.self) private var model
    @Environment(\.colorScheme) private var colorScheme
    @Binding var isPresented: Bool
    @State private var minText = ""
    @State private var maxText = ""

    /// Empty = no limit; nil = not a valid, non-negative amount.
    private static func cents(_ text: String) -> Int64?? {
        let t = text.trimmingCharacters(in: .whitespaces)
        if t.isEmpty { return .some(nil) }
        guard let c = Money.parseCents(t), c >= 0 else { return nil }
        return .some(c)
    }

    private var parsed: AmountFilter? {
        guard let lo = Self.cents(minText), let hi = Self.cents(maxText) else { return nil }
        return AmountFilter(minCents: lo, maxCents: hi).normalized
    }

    private func apply() {
        guard let filter = parsed else { return }
        model.interactiveAmountFilter = filter
        isPresented = false
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Grid(alignment: .leading, horizontalSpacing: 12, verticalSpacing: 8) {
                GridRow {
                    Text("Von")
                    field("Von", prompt: "Kein Minimum", text: $minText)
                }
                GridRow {
                    Text("Bis")
                    field("Bis", prompt: "Kein Maximum", text: $maxText)
                }
            }
            Text("Betrag ohne Vorzeichen.")
                .appFont(.caption)
                .foregroundStyle(Theme.secondary)
                .fixedSize(horizontal: false, vertical: true)
            HStack {
                Button("Zurücksetzen") {
                    minText = ""; maxText = ""
                    model.interactiveAmountFilter = AmountFilter()
                    isPresented = false
                }
                .disabled(!model.amountFilter.isActive && minText.isEmpty && maxText.isEmpty)
                Spacer()
                Button("Abbrechen", role: .cancel) { isPresented = false }
                    .keyboardShortcut(.cancelAction)
                Button("Übernehmen") { apply() }
                    .keyboardShortcut(.defaultAction)
                    .disabled(parsed == nil)
            }
        }
        // Like the rows of the account menu next to it: body size in the primary text colour.
        .appFont(.body)
        .foregroundStyle(Theme.text)
        .padding(16)
        .frame(minWidth: 300)
        // The popover's own fill is darker than the account menu's panel: lift it to that translucent
        // grey-blue in dark mode (a thin material under a light tint).
        .background {
            if colorScheme == .dark {
                ZStack {
                    Rectangle().fill(.ultraThinMaterial)
                    Color(red: 0.62, green: 0.70, blue: 0.92).opacity(0.16) // the menu's blue-grey
                }
            }
        }
        #if os(iOS)
        .presentationDetents([.medium])
        #endif
        .onAppear {
            let f = model.amountFilter
            minText = f.minCents.map(Money.plain) ?? ""
            maxText = f.maxCents.map(Money.plain) ?? ""
        }
    }

    private func field(_ title: LocalizedStringKey, prompt: LocalizedStringKey, text: Binding<String>) -> some View {
        HStack(spacing: 4) {
            TextField(title, text: text, prompt: Text(prompt))
                .labelsHidden()
                .appFont(.body, monospacedDigit: true)
                .multilineTextAlignment(.trailing)
                .frame(width: 125)
                .onSubmit { apply() }
            Text(verbatim: Money.symbol(model.balanceCurrency)).fixedSize()
        }
        .fixedSize()
    }
}
