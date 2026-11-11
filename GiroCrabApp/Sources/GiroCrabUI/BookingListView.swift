import SwiftUI
import GiroCrabCore

/// Column 2: calm booking stream. Readable max width (~720 pt), bookings
/// grouped into white day cards with the daily sum in the card header.
struct BookingStreamView: View {
    @Environment(AppModel.self) private var model
    @FocusState private var focused: Bool
    @AppStorage("showAccountAnalytics") private var showAnalytics: Bool = false
    #if os(iOS)
    @Environment(\.horizontalSizeClass) private var hSize
    @State private var showCashflowIOS = false
    #endif

    /// Depots show positions; bookings (buys, sells) live on the clearing account.
    private var isDepot: Bool {
        if case .account(let id)? = model.sidebarSelection, let a = model.account(for: id) {
            return a.kind == .brokerage || a.name.localizedCaseInsensitiveContains("depot")
        }
        return false
    }

    var body: some View {
        @Bindable var model = model
        let isOverview: Bool = { if case .allAccounts? = model.sidebarSelection { return true }; return false }()
        let emptyVault = !model.demoActive && model.allAccounts.isEmpty
        ScrollViewReader { proxy in
            VStack(spacing: 0) {
                #if !os(iOS)
                header(isOverview: isOverview, emptyVault: emptyVault, scope: $model.scope)
                #endif
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 14) {
                        #if os(iOS)
                        // iPhone: header scrolls away so bookings get the screen.
                        header(isOverview: isOverview, emptyVault: emptyVault, scope: $model.scope)
                        #endif
                        Color.clear.frame(height: 0).id(Self.topID)
                        if emptyVault {
                            EmptyVaultCard()
                        }
                        if model.scope == .investments {
                            InvestmentDashboardView()
                        } else if !model.allAccounts.isEmpty, isOverview || showAnalytics {
                            // Overview: always; single account: only when toggled on.
                            #if os(iOS)
                            DisclosureGroup("Cashflow-Analyse", isExpanded: $showCashflowIOS) {
                                VStack(spacing: 14) {
                                    CashflowCard()
                                    BudgetProgressCard()
                                }
                                .padding(.top, 8)
                            }
                            .font(.subheadline.weight(.medium))
                            .padding(.horizontal, 14).padding(.vertical, 10)
                            .frame(maxWidth: .infinity)
                            .background(RoundedRectangle(cornerRadius: 12, style: .continuous).fill(Theme.card))
                            #else
                            CashflowCard().transition(.opacity.combined(with: .move(edge: .top)))
                            BudgetProgressCard().transition(.opacity.combined(with: .move(edge: .top)))
                            #endif
                        }
                        if model.dayGroups.isEmpty && isDepot && model.searchText.isEmpty {
                            ContentUnavailableView(
                                "Keine Buchungen im Depot",
                                systemImage: "chart.line.uptrend.xyaxis",
                                description: Text("Ein Depot zeigt seine Positionen oben. Käufe, Verkäufe und Sparpläne stehen als Buchungen im Verrechnungskonto.")
                            )
                            .frame(maxWidth: .infinity)
                            // fill the visible scroll area so the placeholder sits in its middle
                            .containerRelativeFrame(.vertical, alignment: .center)
                        } else if model.dayGroups.isEmpty && !(model.allAccounts.isEmpty && !model.demoActive) {
                            ContentUnavailableView(
                                model.searchText.isEmpty ? "Keine Buchungen" : "Keine Treffer",
                                systemImage: model.searchText.isEmpty ? "tray" : "magnifyingglass",
                                description: Text(model.searchText.isEmpty ? "Konto abrufen oder anderen Zeitraum wählen." : "Nichts passt zu „\(model.searchText)“.")
                            )
                            .padding(.top, 40)
                        }
                        ForEach(model.dayGroups) { day in
                            DayCard(day: day)
                                .id(day.id)
                        }
                        if model.hasMoreBookings {
                            LoadMoreBookingsFooter()
                        }
                    }
                    .padding(.horizontal, 20)
                    .padding(.top, 4)
                    .padding(.bottom, 18)
                    .frame(maxWidth: 720)
                    .frame(maxWidth: .infinity)
                    .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { contentWidth = $0 }
                }
                .frame(minHeight: 0, maxHeight: .infinity)
                #if os(iOS)
                .safeAreaInset(edge: .bottom, spacing: 0) {
                    // keeps the last booking clear of the floating search bar
                    Color.clear.frame(height: 64)
                }
                #endif
            }
            .background(Theme.canvas)
            .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { columnWidth = $0 }
            #if os(iOS)
            // iPhone: the detail column is not reachable by tapping, so show it as a sheet.
            .sheet(isPresented: Binding(
                get: { hSize == .compact && model.showBookingSheet && model.selectedBookingID != nil },
                set: { if !$0 { model.showBookingSheet = false } }
            )) {
                NavigationStack { InspectorView() }
                    .presentationDetents([.medium, .large])
            }
            #endif
            // Drop a receipt anywhere on the stream: matched by amount/date/merchant.
            .dropDestination(for: URL.self) { urls, _ in
                model.dropAnywhere(urls)
                return !urls.isEmpty
            }
            .onChange(of: model.selectedBookingID) { _, id in
                guard let id else { return }
                // Rows live inside lazily built day cards: an off-screen day must be
                // scrolled to first, otherwise the row's id is unknown to the proxy.
                if let day = model.dayGroups.first(where: { $0.bookings.contains { $0.id == id } }) {
                    proxy.scrollTo(day.id)
                }
                DispatchQueue.main.async {
                    withAnimation(.easeOut(duration: 0.15)) { proxy.scrollTo(id) }
                }
            }
            // another account, group, period or view: back to the newest day
            .onChange(of: model.sidebarSelection) { _, _ in scrollToTop(proxy) }
            .onChange(of: model.period) { _, _ in scrollToTop(proxy) }
            .onChange(of: model.scope) { _, _ in scrollToTop(proxy) }
            .onChange(of: model.scrollToDay) { _, jump in
                guard let day = jump?.day else { return }
                withAnimation(.easeOut(duration: 0.25)) { proxy.scrollTo(day, anchor: .top) }
                // select the day's first booking so the detail pane follows
                if let first = model.dayGroups.first(where: { $0.date == day })?.bookings.first {
                    model.selectedBookingID = first.id
                }
            }
        }
        .focusable()
        .focusEffectDisabled()
        .focused($focused)
        .onAppear { focused = model.options.screenshotPath == nil }
        .onKeyPress(.downArrow) { model.moveSelection(1); return .handled }
        .onKeyPress(.upArrow) { model.moveSelection(-1); return .handled }
        .onKeyPress(.return) {
            guard model.options.screenshotPath == nil, let b = model.selectedBooking else { return .ignored }
            model.editNote(for: b)
            return .handled
        }
        .onChange(of: model.focusListToken) { _, _ in focused = true }
        .onKeyPress(.space) {
            guard model.options.screenshotPath == nil, let b = model.selectedBooking else { return .ignored }
            // No receipt attached: Space toggles "checked" instead of QuickLook.
            guard let d = model.documents(for: b).first else { model.setChecked(!b.checked, for: b); return .handled }
            if model.quickLookURL != nil { model.quickLookURL = nil } else { model.quickLook(d) }
            return .handled
        }
        #if os(macOS)
        // ⌘C copies the selected booking (Edit ▸ Copy)
        .onCopyCommand {
            guard let b = model.selectedBooking else { return [] }
            return [NSItemProvider(object: b.clipboardLine as NSString)]
        }
        #endif
        .onKeyPress(characters: CharacterSet(charactersIn: "cCrR")) { press in
            // leave ⌘C to the copy command above
            guard !press.modifiers.contains(.command) else { return .ignored }
            guard model.options.screenshotPath == nil, let b = model.selectedBooking else { return .ignored }
            if press.characters.lowercased() == "c" {
                model.showCategorizer = true
            } else {
                model.openRuleEditor(from: b)
            }
            return .handled
        }
        .searchable(text: $model.searchText, placement: .toolbar, prompt: "Händler, Zweck, IBAN, Betrag")
        .toolbar {
            #if os(iOS)
            ToolbarItemGroup(placement: .primaryAction) {
                Button { model.requestSyncAll() } label: { Label("Abrufen", systemImage: "arrow.triangle.2.circlepath") }
                Menu {
                    Button { model.openTransfer() } label: { Label("Überweisen", systemImage: "paperplane") }
                        .disabled(!model.canTransfer)
                    Button { model.exportFilteredBookingsCSV() } label: { Label("CSV exportieren…", systemImage: "square.and.arrow.up") }
                    Button { model.openRuleEditor() } label: { Label("Regeln", systemImage: "wand.and.stars") }
                    Button { model.showSettings = true } label: { Label("Einstellungen", systemImage: "gearshape") }
                    if model.demoActive && model.hasOwnVault {
                        Button { model.showOwnVault() } label: { Label("Demo beenden", systemImage: "theatermasks") }
                    } else if !model.demoActive && model.hasOwnVault && model.allAccounts.isEmpty {
                        Button { model.showDemo() } label: { Label("Demo-Daten laden", systemImage: "theatermasks") }
                    }
                } label: { Label("Mehr", systemImage: "ellipsis.circle") }
            }
            #else
            ToolbarItem(placement: .principal) {
                BookingScopePicker()
            }
            ToolbarItemGroup(placement: .primaryAction) {
                Button { model.openTransfer() } label: { Label("Überweisen", systemImage: "paperplane") }
                    .help("Neue Überweisung (⌘N)")
                    .disabled(!model.canTransfer)
                Button { model.requestSyncAll() } label: { Label("Abrufen", systemImage: "arrow.triangle.2.circlepath") }
                    .help("Alle Konten abrufen (⇧⌘R)")
                Button { model.showCommandPalette = true } label: { Label("Befehle", systemImage: "command") }
                    .help("Befehlspalette (⌘K)")
                Button { model.exportFilteredBookingsCSV() } label: { Label("CSV exportieren…", systemImage: "square.and.arrow.up") }
                    .help("Gefilterte Buchungen als CSV exportieren")
                Button { model.openRuleEditor() } label: { Label("Regeln", systemImage: "wand.and.stars") }
                    .help("Kategorisierungsregeln")
                SettingsLink { Label("Einstellungen", systemImage: "gearshape") }
            }
            #endif
        }
        .navigationTitle("")
        .fileExporter(
            isPresented: $model.showCSVExporter,
            document: model.csvExport,
            contentType: .commaSeparatedText,
            defaultFilename: "GiroCrab-Buchungen"
        ) { result in
            if case .failure(let e) = result { model.errorMessage = e.localizedDescription }
        }
    }


    @ViewBuilder
    private func header(isOverview: Bool, emptyVault: Bool, scope: Binding<BookingScope>) -> some View {
                VStack(alignment: .leading, spacing: 14) {
                    if model.demoActive {
                        DemoBanner()
                    }
                    #if os(iOS)
                    Picker("Ansicht", selection: scope) {
                        ForEach(BookingScope.ordered, id: \.self) { Text($0.shortTitle).tag($0) }
                    }
                    .pickerStyle(.segmented)
                    #endif
                    if !emptyVault {
                        StreamHeaderCard()
                        if !isOverview && !isDepot && model.scope != .investments && !model.allAccounts.isEmpty {
                            HStack {
                                Button {
                                    withAnimation(.easeInOut(duration: 0.25)) { showAnalytics.toggle() }
                                } label: {
                                    Label("Analysen", systemImage: "chart.bar.xaxis")
                                        .font(.caption.weight(.medium))
                                        .padding(.horizontal, 10)
                                        .padding(.vertical, 4)
                                        .background(showAnalytics ? Color.accentColor.opacity(0.18) : Color.secondary.opacity(0.12), in: Capsule())
                                }
                                .buttonStyle(.plain)
                                .help("Cashflow und Budgets für dieses Konto ein-/ausblenden")
                                Spacer()
                            }
                        }
                    }
                }
                #if os(iOS)
                // the scroll content already carries the 20pt side padding
                .padding(.top, 4)
                .padding(.bottom, 6)
                #else
                .padding(.horizontal, 20)
                .padding(.top, 18)
                .padding(.bottom, 10)
                #endif
                .frame(maxWidth: 720)
                .frame(maxWidth: .infinity)
                .padding(.trailing, scrollerInset)
    }

    /// macOS: width a scroller takes from the scroll area; the pinned header
    /// (outside the ScrollView) gets the same trailing inset to stay aligned.
    @State private var columnWidth: CGFloat = 0
    @State private var contentWidth: CGFloat = 0
    private var scrollerInset: CGFloat {
        #if os(macOS)
        (columnWidth > 0 && contentWidth > 0) ? max(0, columnWidth - contentWidth) : 0
        #else
        0
        #endif
    }

    private static let topID = "stream-top"


    private func scrollToTop(_ proxy: ScrollViewProxy) {
        proxy.scrollTo(Self.topID, anchor: .top)
    }
}

#if os(macOS)
/// Keep the toolbar control independent of list, chart and inspector updates.
/// Its only observable dependency is the selected scope.
private struct BookingScopePicker: View {
    @Environment(AppModel.self) private var model
    @Namespace private var selectionAnimation

    var body: some View {
        // The native toolbar Picker can animate its selection back through an
        // intermediate segment while the stream changes. Animate one stable
        // highlight locally instead; hover and selection remain independent.
        HStack(spacing: 0) {
            ForEach(BookingScope.ordered, id: \.self) { scope in
                Button {
                    model.scope = scope
                } label: {
                    Text(scope.title)
                        .font(.callout)
                        .lineLimit(1)
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 6)
                        .contentShape(Capsule())
                }
                .buttonStyle(.plain)
                .background {
                    if model.scope == scope {
                        Capsule()
                            .fill(Color.primary.opacity(0.09))
                            .matchedGeometryEffect(id: "scope-selection", in: selectionAnimation)
                    }
                }
                .accessibilityAddTraits(model.scope == scope ? .isSelected : [])
            }
        }
        .padding(3)
        .background(Theme.canvas, in: Capsule())
        .overlay(Capsule().strokeBorder(Theme.border, lineWidth: 1).allowsHitTesting(false))
        .frame(width: 430)
        .animation(.easeOut(duration: 0.16), value: model.scope)
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Ansicht")
        .help("⌘1 / ⌘2 / ⌘3")
    }
}
#endif

struct DayCard: View {
    @Environment(AppModel.self) private var model
    let day: DayGroup

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text(DateText.dayHeader(day.date))
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(Theme.text)
                Spacer()
                Text(Money.signed(day.sum))
                    .font(.subheadline.monospacedDigit())
                    .foregroundStyle(day.sum > 0 ? Theme.inflow : Theme.secondary)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 10)
            Rectangle().fill(Theme.border).frame(height: 1)
            ForEach(Array(day.bookings.enumerated()), id: \.element.id) { i, b in
                BookingRow(booking: b, selected: model.selectedBookingID == b.id)
                    .id(b.id)
                    .dropDestination(for: URL.self) { urls, _ in
                        model.selectedBookingID = b.id
                        model.attach(urls, to: b)
                        return !urls.isEmpty
                    }
                    .contentShape(Rectangle())
                    .onTapGesture { model.selectedBookingID = b.id; model.showBookingSheet = true }
                    // simultaneous, so single clicks keep selecting without the double-click delay
                    .simultaneousGesture(TapGesture(count: 2).onEnded { model.editNote(for: b) })
                    .contextMenu { rowMenu(b) }
                if i < day.bookings.count - 1 {
                    Rectangle().fill(Theme.border.opacity(0.7)).frame(height: 1).padding(.leading, 64)
                }
            }
        }
        .frame(maxWidth: .infinity)
        .background(RoundedRectangle(cornerRadius: 12, style: .continuous).fill(Theme.card))
        .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).strokeBorder(Theme.border, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
    }

    @ViewBuilder
    private func rowMenu(_ b: Booking) -> some View {
        Button("Kategorisieren …", systemImage: "tag") { model.selectedBookingID = b.id; model.showCategorizer = true }
        Button("Regel für diesen Händler erstellen …", systemImage: "wand.and.stars") { model.openRuleEditor(from: b) }
        Button("Notiz bearbeiten …", systemImage: "note.text") { model.editNote(for: b) }
        Button("Umsatz aufteilen (Splitbuchung) …", systemImage: "arrow.triangle.branch") { model.splitBooking = b }
        Divider()
        Button("Nach „\(b.counterparty.isEmpty ? b.merchantName : b.counterparty)“ suchen", systemImage: "magnifyingglass") {
            model.searchText = b.counterparty.isEmpty ? b.merchantName : b.counterparty
        }
        .disabled(b.counterparty.isEmpty && b.merchantName.isEmpty)
        Button("Neue Überweisung aus Buchung …", systemImage: "paperplane") { model.openTransfer(from: b) }
            .disabled(!model.canTransfer)
        Button("Umsatz kopieren", systemImage: "doc.on.doc") { copyToPasteboard(b.clipboardLine) }
        Divider()
        Button(b.checked ? "Als neu markieren" : "Als geprüft markieren", systemImage: "checkmark.circle") { model.setChecked(!b.checked, for: b) }
    }
}

/// Exactly three elements: signet · name + subtitle · amount.
struct BookingRow: View {
    @Environment(AppModel.self) private var model
    let booking: Booking
    var selected = false

    #if os(iOS)
    @Environment(\.horizontalSizeClass) private var sizeClass
    private var compactChip: Bool { sizeClass == .compact }
    #else
    private let compactChip = false
    #endif

    var body: some View {
        HStack(spacing: 12) {
            MerchantSignet(merchant: booking.merchant, categorySymbol: booking.categorySymbol, showNewDot: !booking.checked)
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 5) {
                    Text(booking.merchant.name.isEmpty ? booking.bookingText : booking.merchant.name)
                        .font(.system(size: 14, weight: .semibold))
                        .foregroundStyle(Theme.text)
                        .lineLimit(1)
                    if booking.documentCount > 0 {
                        Image(systemName: "doc.text.fill")
                            .font(.system(size: 10))
                            .foregroundStyle(Theme.accent.opacity(0.8))
                            .help("\(booking.documentCount) Beleg(e) – Leertaste für QuickLook")
                    }
                    if model.recurringSeries(for: booking) != nil {
                        Image(systemName: "repeat")
                            .font(.system(size: 10, weight: .semibold))
                            .foregroundStyle(Theme.secondary)
                            .help("Wiederkehrende Buchung")
                            .accessibilityLabel("Wiederkehrend")
                    }
                }
                HStack(spacing: 6) {
                    #if os(iOS)
                    let showChip = false
                    #else
                    let showChip = model.showsAccountMarkers
                    #endif
                    if showChip {
                        AccountChip(label: model.accountShortLabel(booking.accountId), color: model.accountColor(booking.accountId), compact: compactChip)
                    }
                    // category with the sidebar's icon and colour, then the payment method
                    BookingCategoryLabel(booking: booking)
                        .layoutPriority(1)
                    if !booking.paymentMethod.isEmpty {
                        Text("· \(booking.paymentMethod)")
                            .font(.system(size: 12))
                            .foregroundStyle(Theme.secondary)
                            .lineLimit(1)
                            .layoutPriority(-1)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .clipped()
            }
            .layoutPriority(0)
            Spacer(minLength: 8)
            if booking.checked {
                Image(systemName: "checkmark")
                    .font(.system(size: 10, weight: .semibold))
                    .foregroundStyle(Theme.tertiary)
                    .help("Geprüft")
            }
            AmountLabel(cents: booking.amountCents)
                .lineLimit(1)
                .fixedSize()
        }
        .frame(minHeight: 44)
        .padding(.horizontal, 14)
        .padding(.vertical, 5)
        .background(selected ? Theme.selection : Color.clear)
    }
}

/// Category of a booking as in the sidebar: its icon and colour
/// ("Ohne Kategorie" like the sidebar entry, in the warning colour).
struct BookingCategoryLabel: View {
    let booking: Booking

    var body: some View {
        let none = booking.categoryName.isEmpty
        let color = none ? Theme.warning : Color.readable(hex: booking.categoryColorHex)
        HStack(spacing: 3) {
            Image(systemName: none ? "questionmark.folder" : (booking.categorySymbol.isEmpty ? "tag.fill" : booking.categorySymbol))
                .font(.system(size: 10))
            Text(none ? String(localized: "Ohne Kategorie") : booking.categoryName)
                .font(.system(size: 12, weight: .medium))
                .lineLimit(1)
        }
        .foregroundStyle(color)
    }
}

/// Account of a booking in mixed lists: coloured dot + short name.
struct AccountChip: View {
    let label: String
    let color: Color
    /// Narrow iPhone rows: just the dot (the category keeps the room).
    var compact = false

    var body: some View {
        HStack(spacing: 4) {
            Circle().fill(color).frame(width: 6, height: 6)
            if !compact {
                Text(label)
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(color)
                    .lineLimit(1)
            }
        }
        .padding(.horizontal, compact ? 0 : 6)
        .padding(.vertical, 1.5)
        .background(Capsule().fill(color.opacity(compact ? 0 : 0.12)))
        .fixedSize()
        .help(label)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text("Konto: \(label)"))
    }
}

/// Outflows dark slate, inflows an emerald badge.
struct AmountLabel: View {
    let cents: Int64
    var size: CGFloat = 14

    var body: some View {
        if cents > 0 {
            Text(Money.signed(cents))
                .font(.system(size: size, weight: .semibold).monospacedDigit())
                .foregroundStyle(Theme.inflow)
                .padding(.horizontal, 8).padding(.vertical, 3)
                .background(Capsule().fill(Theme.inflowBadge))
        } else {
            Text(Money.format(cents))
                .font(.system(size: size, weight: .medium).monospacedDigit())
                .foregroundStyle(Theme.outflow)
        }
    }
}

/// End of the list: the default limit cut off older bookings – offer to load them.
struct LoadMoreBookingsFooter: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(spacing: 8) {
            Text("\(model.bookings.count) von \(model.bookings.count + model.remainingBookingCount) Buchungen geladen – ältere Buchungen sind noch nicht sichtbar.")
                .font(.caption).foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
            HStack(spacing: 10) {
                Button("Weitere Buchungen laden (+3.000)") { model.loadMoreBookings() }
                Button("Alle Buchungen laden") { model.loadAllBookings() }
            }
            .buttonStyle(.bordered)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 16)
    }
}
