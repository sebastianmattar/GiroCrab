import SwiftUI
import GiroCrabCore
#if os(macOS)
import AppKit
#endif

/// Column 2: calm booking stream. Fills the space between sidebar and
/// inspector, bookings grouped into flat day sections separated by
/// full-width dividers instead of cards.
struct BookingStreamView: View {
    @Environment(\.bookingDensity) private var density
    @Environment(\.textScale) private var textScale
    /// The empty state repeats the search text, which is drawn as a parameter
    /// `Text` and cannot carry `.sensitive()`.
    @Environment(\.reportRedaction) private var redaction

    @Environment(AppModel.self) private var model
    @FocusState private var focused: Bool

    var body: some View {
        @Bindable var model = model
        let isOverview = model.showsOverview
        // „Neue Umsätze“: account cards instead of day cards, no charts
        let isNewBookings = model.showsNewBookings
        let emptyVault = !model.demoActive && model.allAccounts.isEmpty
        ScrollViewReader { proxy in
            VStack(spacing: 0) {
                // Security lock: first, above everything else (stays visible).
                // Outside the ScrollView like the header: they end where the cards below end, not
                // under the scroll bar (#1535).
                VStack(spacing: 0) {
                    BankingPolicyBanner()
                    AnnouncementBanner()
                }
                #if os(macOS)
                .padding(.trailing, scrollerInset)
                #endif
                if let focus = model.merchantFocus {
                    MerchantFocusBar(focus: focus)
                }
                #if !os(iOS)
                header(isOverview: isOverview, emptyVault: emptyVault, scope: $model.scope)
                #endif
                ScrollView {
                    // Spacing 0: the stream below is one flat run of day headers and
                    // rows; dividers provide the structure, everything above is one block.
                    LazyVStack(alignment: .leading, spacing: 0) {
                        #if !os(iOS)
                        // The scroll-to-top target is the very first thing in the scroll
                        // content and carries the gap above the first card itself. As a
                        // zero-height view below a top padding, `scrollTo(…, anchor: .top)`
                        // scrolled that padding away: after a scope or period change the
                        // cards sat a few points higher than after launch (GitHub #423).
                        Color.clear.frame(height: Self.topGap).id(Self.topID)
                        #endif
                        VStack(alignment: .leading, spacing: ViewLayout.cardGap) {
                        #if os(iOS)
                        // iPhone: header scrolls away so bookings get the screen.
                        header(isOverview: isOverview, emptyVault: emptyVault, scope: $model.scope)
                        // Below the header: "to the top" on iPhone means the newest day.
                        Color.clear.frame(height: 0).id(Self.topID)
                        #endif
                        if emptyVault {
                            EmptyVaultCard()
                        }
                        if !emptyVault && model.scope != .investments {
                            MerchantLogoTipView()
                        }
                        StreamAnalysisCards(isOverview: isOverview, isNewBookings: isNewBookings, hasAccounts: !model.allAccounts.isEmpty)
                        if model.isLoadingSelection {
                            ProgressView().controlSize(.small)
                                .frame(maxWidth: .infinity)
                                .padding(.top, 40)
                        } else if isNewBookings {
                            // the cards below bring their own empty state
                        } else if model.dayGroups.isEmpty && !(model.allAccounts.isEmpty && !model.demoActive) {
                            ContentUnavailableView(
                                model.searchText.isEmpty ? "Keine Buchungen" : "Keine Treffer",
                                systemImage: model.searchText.isEmpty ? "tray" : "magnifyingglass",
                                description: Text(model.searchText.isEmpty ? "Konto abrufen oder anderen Zeitraum wählen." : "Nichts passt zu „\(redaction.masking(model.effectiveSearchText.isEmpty ? model.searchText : model.effectiveSearchText))“.")
                            )
                            .padding(.top, 40)
                        }
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        // One lazy element per header and per booking (not per day):
                        // rows are nearly equally high, so the scroll bar stays steady.
                        if isNewBookings {
                            NewBookingsCards()
                        } else {
                            ForEach(model.streamItems.filter { item in
                                if density == .compact, case .header = item { return false }
                                return true
                            }) { item in
                                // No `.id(day/booking)` here: identity is the position (`StreamItem`),
                                // an explicit id would rebuild every row of a new list.
                                switch item {
                                case .header(let day, let n):
                                    DayHeader(day: day, balance: model.listBalances.days[day.date]).padding(.top, n == 0 ? ViewLayout.cardGap : 14)
                                        .trackScrollDay(day.date)
                                case .row(let b, let isLast, _):
                                    DayCardRow(booking: b, isLast: isLast, balance: model.listBalances.bookings[b.id])
                                        .trackScrollDay(b.bookingDate)
                                }
                            }
                        }
                        if model.hasMoreBookings {
                            LoadMoreBookingsFooter()
                                .padding(.top, 14)
                        }
                    }
                    .padding(.horizontal, 16)
                    #if os(iOS)
                    .padding(.top, 4)
                    #endif
                    .padding(.bottom, 14)
                    .frame(maxWidth: .infinity)
                    #if os(macOS)
                    // One value, so the view re-runs only when a scroller comes or goes, not at every
                    // step of a resize or sidebar animation.
                    .onGeometryChange(for: CGFloat.self) { proxy in
                        proxy.bounds(of: .named(StreamDayTracker.space)).map { max(0, $0.width - proxy.size.width) } ?? 0
                    } action: { scrollerInset = $0 }
                    #endif
                }
                .coordinateSpace(name: StreamDayTracker.space)
                .frame(minHeight: 0, maxHeight: .infinity)
                .scrollBounceBehavior(.basedOnSize)
                .safeAreaInset(edge: .bottom, spacing: 0) {
                    VStack(spacing: 0) {
                        SelectionSummaryBar()
                        #if os(iOS)
                        // keeps the last booking clear of the floating search bar
                        Color.clear.frame(height: 64)
                        #endif
                    }
                }
            }
            .background(Theme.systemBackground)
            // Drop a receipt anywhere on the stream: matched by amount/date/merchant.
            .dropDestination(for: URL.self) { urls, _ in
                model.dropAnywhere(urls)
            }
            .background(SelectionScroller(proxy: proxy))
            // another account, group, period or view: back to the newest day; for a sidebar
            // selection or a period once its data is in (`AppModel.requestStreamTop`)
            .onChange(of: model.streamTopRequest) { _, _ in proxy.scrollTo(Self.topID, anchor: .top) }
            .onChange(of: model.scope) { _, _ in scrollToTop(proxy) }
            .onChange(of: model.scrollToDay) { _, jump in
                guard let day = jump?.day else { return }
                let first = model.dayGroups.first(where: { $0.date == day })?.bookings.first
                let slots = model.streamSlots
                withAnimation(Motion.animation(.easeOut(duration: 0.25))) {
                    if density == .compact, let first, let slot = slots.booking[first.id] {
                        proxy.scrollTo(slot, anchor: .top)
                    } else if let slot = slots.day[day] {
                        proxy.scrollTo(slot, anchor: .top)
                    }
                }
                // select the day's first booking so the detail pane follows
                if let first {
                    // only when the selection really changes: otherwise nothing consumes the flag
                    if model.selectedBookingID != first.id { model.jumpSelectedBookingID = first.id }
                    model.selectedBookingID = first.id
                    // Fallback once the day scroll has run: the booking must be on screen, also
                    // when the day cannot reach the top edge (a short list, the lazy stack
                    // had not built the day yet). A no-op when it is visible already.
                    let token = jump?.token
                    Task { @MainActor in
                        try? await Task.sleep(for: .milliseconds(350))
                        guard model.scrollToDay?.token == token, let slot = model.streamSlots.booking[first.id] else { return }
                        withAnimation(Motion.animation(.easeOut(duration: 0.15))) {
                            proxy.scrollTo(slot, anchor: density == .standard ? nil : .top)
                        }
                    }
                }
            }
        }
        // Pull-to-refresh (iOS): iCloud merge + bank sync of the current selection
        .refreshable { await model.refreshCurrentView() }
        .focusable()
        .focusEffectDisabled()
        .focused($focused)
        .onAppear {
            // also runs when a sidebar selection brings the list back, e.g. after a depot
            if model.listMayTakeFocusOnAppear { focused = true }
            #if os(macOS)
            ClickModifiers.install()
            #endif
        }
        .onKeyPress(.downArrow) { model.moveSelection(1); return .handled }
        .onKeyPress(.upArrow) { model.moveSelection(-1); return .handled }
        // Bild↓/Bild↑ page through the list, Pos1/Ende jump to its ends (GitHub #1561);
        // `SelectionScroller` keeps the selected row in view, also when the target row
        // is already selected (`keyboardScrollToken`).
        .onKeyPress(.pageDown) { model.moveSelection(AppModel.keyboardPageStep); return .handled }
        .onKeyPress(.pageUp) { model.moveSelection(-AppModel.keyboardPageStep); return .handled }
        .onKeyPress(.home) { model.moveSelection(-Int.max); return .handled }
        .onKeyPress(.end) { model.moveSelection(Int.max); return .handled }
        .onKeyPress(.return) {
            // The balance chart sits inside the list: with keyboard focus there,
            // Return belongs to the chart (jump to the chosen day).
            guard !model.chartHasKeyboardFocus else { return .ignored }
            guard model.options.screenshotPath == nil, let b = model.selectedBooking else { return .ignored }
            model.editNote(for: b)
            return .handled
        }
        .onChange(of: model.focusListToken) { _, _ in focused = true }
        .merchantLogoTipEligibility()
        .onKeyPress(.space) {
            guard model.options.screenshotPath == nil, let b = model.selectedBooking else { return .ignored }
            // Space is Quick Look only (as in the Finder); "Geprüft" is ⇧⌘U.
            guard let d = model.documents(for: b).first else { return .ignored }
            if model.quickLookURL != nil { model.quickLookURL = nil } else { model.quickLook(d) }
            return .handled
        }
        #if os(macOS)
        // Bearbeiten › Alles auswählen (⌘A) selects all listed bookings
        .onCommand(#selector(NSResponder.selectAll(_:))) { model.selectAllBookings() }
        // ⌘C copies the selected booking (Edit ▸ Copy)
        .onCopyCommand {
            guard redaction.allowsWholeBookingCopy, let b = model.selectedBooking else { return [] }
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
        .searchable(text: $model.searchText, placement: .toolbar, prompt: textScale > 1 ? "Händler, Zweck, IBAN …" : "Händler, Zweck, IBAN / Kontonummer, Betrag")
        #if os(iOS)
        .modifier(MainToolbar())
        #endif
        // A real window title (Window menu, Mission Control, VoiceOver); the
        // toolbar keeps its room for the scope picker.
        #if os(iOS)
        .modifier(WindowTitle())
        #endif
        .bookingOutputSheets()
    }


    @ViewBuilder
    private func header(isOverview: Bool, emptyVault: Bool, scope: Binding<BookingScope>) -> some View {
                VStack(alignment: .leading, spacing: ViewLayout.cardGap) {
                    // Marketing screenshots (`--screenshot`) look like a real vault: no demo banner.
                    if model.demoActive && LaunchOptions.current.screenshotPath == nil {
                        DemoBanner()
                    }
                    #if os(iOS)
                    // writes through `selectScope` like the macOS toolbar picker: a chosen category ends
                    Picker("Ansicht", selection: Binding(get: { scope.wrappedValue }, set: { model.selectScope($0) })) {
                        ForEach(BookingScope.ordered, id: \.self) { Text($0.shortTitle).tag($0) }
                    }
                    .pickerStyle(.segmented)
                    #endif
                    if !emptyVault {
                        StreamHeaderCard()
                    }
                }
                #if os(iOS)
                // the scroll content already carries the 20pt side padding
                .padding(.top, 4)
                .padding(.bottom, 6)
                #else
                .padding(.horizontal, 16)
                .padding(.top, 10)
                .padding(.bottom, ViewLayout.sectionSpacing)
                #endif
                .frame(maxWidth: .infinity)
                // Full-width line between the pinned header and the scroll content.
                #if os(macOS)
                .overlay(alignment: .bottom) { Divider() }
                #endif
                .padding(.trailing, scrollerInset)
    }

    /// macOS: width a scroller takes from the scroll area; the pinned header
    /// (outside the ScrollView) gets the same trailing inset to stay aligned.
    @State private var scrollerInset: CGFloat = 0

    private static let topID = "stream-top"
    #if !os(iOS)
    /// Gap between the fixed header and the first card of the scroll content.
    static let topGap: CGFloat = ViewLayout.cardGap
    #endif


    private func scrollToTop(_ proxy: ScrollViewProxy) {
        model.scrollFocusDay = nil
        proxy.scrollTo(Self.topID, anchor: .top)
    }
}


/// Investment dashboard or the cashflow and budget cards above the booking stream. A view of
/// its own: its `@AppStorage` settings never compare equal, so in `BookingStreamView` they made
/// the whole stream re-run whenever its parent was re-created (on every sidebar selection).
private struct StreamAnalysisCards: View {
    @Environment(AppModel.self) private var model
    let isOverview: Bool
    let isNewBookings: Bool
    let hasAccounts: Bool
    @AppStorage("showAccountAnalytics") private var showAnalytics: Bool = false
    /// View › "Auswertungen anzeigen" (GitHub #233): hides the charts and analysis cards.
    @AppStorage(ViewLayout.statisticsKey) private var showStatistics: Bool = true
    @AppStorage(ViewLayout.budgetsKey) private var showBudgets: Bool = true
    #if os(iOS)
    @State private var showCashflowIOS = false
    #endif

    var body: some View {
        let cards = ViewLayout.cards(statistics: showStatistics, budgets: showBudgets)
        if model.scope == .investments {
            InvestmentDashboardView()
        } else if cards.any, !isNewBookings, hasAccounts, isOverview || showAnalytics {
            // Overview: always; single account: only when toggled on.
            #if os(iOS)
            if cards.cashflow {
                DisclosureGroup("Cashflow-Analyse", isExpanded: $showCashflowIOS) {
                    VStack(spacing: 0) {
                        CashflowCard()
                        if cards.budgets {
                            Divider()
                            BudgetProgressCard()
                        }
                    }
                }
                .appFont(.subheadline, weight: .medium)
                .flatSection(horizontal: 14, vertical: 6)
            } else {
                BudgetProgressCard()
            }
            #else
            VStack(spacing: 0) {
                if cards.cashflow {
                    CashflowCard().transition(Motion.slide(.top))
                }
                if cards.cashflow && cards.budgets {
                    Divider()
                }
                if cards.budgets {
                    BudgetProgressCard().transition(Motion.slide(.top))
                }
            }
            #endif
        }
    }
}

/// Print and export sheets plus the file exporters behind the menu commands; shared by the
/// booking list and the depot view, so a command works in both.
private struct BookingOutputSheets: ViewModifier {
    @Environment(AppModel.self) private var model

    func body(content: Content) -> some View {
        @Bindable var model = model
        content
        .capturableSheet(isPresented: $model.showPrintSheet) { PrintSheet() }
        .capturableSheet(isPresented: $model.showExportSheet) { ExportSheet() }
        .fileExporter(
            isPresented: $model.showExportExporter,
            document: model.exportFile,
            contentType: model.exportContentType,
            defaultFilename: model.exportFilename
        ) { result in
            if case .failure(let e) = result { model.showError(String(localized: "Export fehlgeschlagen"), e.localizedDescription) }
        }
    }
}

extension View {
    func bookingOutputSheets() -> some View { modifier(BookingOutputSheets()) }
}

#if os(macOS)
/// Keep the toolbar control independent of list, chart and inspector updates.
/// Its only observable dependency is the selected scope. The system segmented
/// control brings arrow-key navigation, focus ring and VoiceOver with it (an
/// older selection-animation glitch no longer occurs on current macOS).
private struct BookingScopePicker: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        Picker("Ansicht", selection: Binding(get: { model.scope }, set: { model.selectScope($0) })) {
            ForEach(BookingScope.ordered, id: \.self) { Text($0.title).tag($0) }
        }
        .pickerStyle(.segmented)
        .labelsHidden()
        .fixedSize()
        .help("Ansicht wählen (⌘1, ⌘2, ⌘3)")
    }
}
#endif

/// One horizontal slice of a white day card. The stream draws a card from
/// stacked slices (header, rows) so each slice can be a lazy element of its own:
/// rounded corners only at the card's top and bottom, the border only on the
/// sides in between.
/// Shown above a merchant-centred list (#495): names the merchant and leads back
/// to the view the list was opened from.
struct MerchantFocusBar: View {
    @Environment(AppModel.self) private var model
    let focus: MerchantFocus

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "storefront").foregroundStyle(Theme.secondary)
            Text("Alle Buchungen von").foregroundStyle(Theme.secondary)
            Text(focus.name).fontWeight(.semibold).lineLimit(1).sensitive(.personal)
            Text(verbatim: "· \(model.shownPeriod.title)").foregroundStyle(Theme.secondary).lineLimit(1)
            Spacer(minLength: 8)
            Button("Zurück", systemImage: "xmark.circle.fill") { model.leaveMerchantFocus() }
                .buttonStyle(.borderless)
                .fixedSize()
                .help("Händler-Filter aufheben und zur vorherigen Ansicht zurückkehren")
        }
        .appFont(.callout)
        .padding(.horizontal, 16).padding(.vertical, 6)
        .frame(maxWidth: .infinity, alignment: .leading)
        .overlay(alignment: .bottom) { Divider() }
    }
}

/// The stream is one flat run of rows: no rounded day cards, no strokes.
struct DayCardSegment: ViewModifier {
    func body(content: Content) -> some View {
        content.frame(maxWidth: .infinity)
    }
}

/// Date groups bookings without repeating a daily total.
struct DayHeader: View {
    @Environment(\.bookingDensity) private var density
    @Environment(AppModel.self) private var model
    let day: DayGroup
    /// End-of-day balance (#577); nil = none shown.
    var balance: RunningBalance?

    var body: some View {
        // this day is the one the balance chart's marker points to (#593)
        let marked = model.streamHighlightDay == day.date
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                if marked {
                    Circle().fill(Theme.accent).frame(width: 7, height: 7)
                }
                Text(DateText.dayHeader(day.date))
                    .appFont(.subheadline, weight: .semibold)
                    .foregroundStyle(marked ? Theme.accent : Theme.text)
                Spacer()
                // end-of-day balance (#577): exact, whatever the order within the day
                if let balance {
                    Text("Saldo \(balance.text)")
                        .sensitive(.amounts)
                        .appFont(.caption, monospacedDigit: true)
                        .foregroundStyle(Theme.secondary)
                        .lineLimit(1).fixedSize()
                        .help("Kontostand am Ende des Tages")
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, density == .compact ? 3 : 6)
            .background(marked ? Theme.accent.opacity(0.12) : Color.clear)
            Divider()
        }
        .modifier(DayCardSegment())
    }
}

/// Reports the day of the stream element that crosses a line just below the top edge of the
/// scroll view, so the balance chart can follow the scroll position (#593). The geometry
/// callback fires only when the Boolean flips, not on every scroll tick.
private struct StreamDayTracker: ViewModifier {
    static let space = "bookingStream"
    /// Distance from the top edge of the scroll view in points.
    private static let line: CGFloat = 24
    @Environment(AppModel.self) private var model
    /// The day this element counted in `scrollCrossing` while it crosses the line; nil = not on
    /// the line. Kept instead of a flag because elements are reused for other days (positional
    /// identity, `StreamItem`): a release must hit the day that was counted, whichever of the
    /// geometry callback and the day change runs first.
    @State private var counted: String?
    let day: String

    func body(content: Content) -> some View {
        content
            .onGeometryChange(for: Bool.self) { proxy in
                let f = proxy.frame(in: .named(Self.space))
                return f.minY <= Self.line && f.maxY > Self.line
            } action: { crosses in
                guard crosses != (counted != nil) else { return }
                if crosses {
                    count(day)
                    if model.scrollFocusDay != day { model.scrollFocusDay = day }
                } else {
                    releaseCounted()
                }
            }
            // the lazy stack drops rows that scrolled far away without a last callback
            .onDisappear { releaseCounted() }
            // Reused for another day while on the line: the count moves to the new day. The
            // chart marker is left alone; a new list brings its own (`requestStreamTop`), and
            // pointing it at whatever day sits at the old offset made it jump.
            .onChange(of: day) {
                guard counted != nil else { return }
                releaseCounted()
                count(day)
            }
    }

    private func count(_ day: String) {
        model.scrollCrossing[day, default: 0] += 1
        counted = day
    }

    /// A day has several elements (header, rows): the focus ends only when none of them is
    /// left on the line. A short delay covers the hand-over to the next day's element.
    private func releaseCounted() {
        guard let day = counted else { return }
        counted = nil
        let left = (model.scrollCrossing[day] ?? 1) - 1
        model.scrollCrossing[day] = left > 0 ? left : nil
        guard left <= 0, model.scrollFocusDay == day else { return }
        Task { @MainActor in
            try? await Task.sleep(for: .milliseconds(120))
            if model.scrollCrossing[day] == nil, model.scrollFocusDay == day { model.scrollFocusDay = nil }
        }
    }
}

private extension View {
    /// Rows carry no `.id(...)` (see `StreamItem`); one placed after this modifier would take the
    /// scroll target away from `ScrollViewProxy.scrollTo`, so every jump would do nothing.
    func trackScrollDay(_ day: String) -> some View { modifier(StreamDayTracker(day: day)) }
}

/// One booking row of a flat day section; the last one ends the day's dividers.
struct DayCardRow: View {
    @Environment(\.textScale) private var textScale
    @Environment(\.bookingDensity) private var density
    @Environment(\.reportRedaction) private var redaction
    @Environment(AppModel.self) private var model
    let booking: Booking
    let isLast: Bool
    /// Balance after this booking (#577); nil = none shown.
    var balance: RunningBalance?
    @State private var compactDetailsCollapsed = false

    var body: some View {
        let b = booking
        VStack(spacing: 0) {
                BookingRow(booking: b, compactDetailsCollapsed: compactDetailsCollapsed, balance: balance) {
                    selectRow(togglesDetails: false)
                }
                    .dropDestination(for: URL.self) { urls, _ in
                        guard model.attach(urls, to: b) else { return false }
                        model.selectedBookingID = b.id
                        return true
                    }
                    #if os(macOS)
                    .modifier(BookingRowClicks(selectRow: { flags in
                        // Finder-style: ⌘-click adds/removes, ⇧-click selects a range.
                        selectRow(toggle: flags.contains(.command), extend: flags.contains(.shift))
                    }, openNote: { model.editNote(for: b) }))
                    #else
                    .contentShape(Rectangle())
                    .onTapGesture { selectRow() }
                    .simultaneousGesture(TapGesture(count: 2).onEnded { model.editNote(for: b) })
                    #endif
                    .modifier(RowSelectionTraits(id: b.id))
                    .contextMenu { rowMenu(b) }
                    #if os(macOS)
                    // The payload is read when a row drag starts outside the selectable text.
                    .onDrag { model.dragItem(for: b).itemProvider() }
                    #endif
            if !isLast || density == .compact {
                Divider()
                    .padding(.leading, density == .compact ? 12 : 48)
            }
        }
        .modifier(DayCardSegment())
        .onChange(of: model.selectedBookingID) { _, id in
            if id != booking.id { compactDetailsCollapsed = false }
        }
        // the row is reused for another booking (positional identity, `StreamItem`)
        .onChange(of: booking.id) { compactDetailsCollapsed = false }
    }

    private func selectRow(toggle: Bool = false, extend: Bool = false, togglesDetails: Bool = true) {
        if density == .compact, model.selectedBookingID == booking.id, !toggle, !extend {
            if togglesDetails { compactDetailsCollapsed.toggle() }
        } else {
            compactDetailsCollapsed = false
        }
        model.selectBooking(booking.id, toggle: toggle, extend: extend, fromListClick: true)
        model.showBookingSheet = true
        #if os(macOS)
        model.focusListToken &+= 1
        #endif
    }

    @ViewBuilder
    private func rowMenu(_ b: Booking) -> some View {
        CategoryAssignMenu(booking: b)
        Button("Regel für diesen Händler erstellen …", systemImage: "wand.and.stars") { model.openRuleEditor(from: b) }
        Button("Notiz bearbeiten …", systemImage: "note.text") { model.editNote(for: b) }
        TagsMenu(model: model, booking: b)
        // #1663: a common share opens the sheet with that share already in the second part
        Menu("Umsatz aufteilen (Splitbuchung)", systemImage: "arrow.triangle.branch") {
            ForEach(SplitMath.shares, id: \.self) { percent in
                Button { model.openSplit(b, percent: percent) } label: { Text(verbatim: SplitMath.label(percent)) }
            }
            Divider()
            Button("Andere Aufteilung …") { model.openSplit(b) }
        }
        RefundLinkMenu(model: model, booking: b)
        LoanAssignMenu(model: model, booking: b, hidesWhenUnavailable: true)
        OfflineBookingMenu(model: model, booking: b)
        Divider()
        Button("Nach „\(b.counterparty.isEmpty ? b.merchantName : b.counterparty)“ suchen", systemImage: "magnifyingglass") {
            model.searchText = b.counterparty.isEmpty ? b.merchantName : b.counterparty
        }
        .disabled(b.counterparty.isEmpty && b.merchantName.isEmpty)
        Button("Neue Überweisung aus Buchung …", systemImage: "paperplane") { model.openTransfer(from: b) }
            .disabled(!model.canTransfer)
        Button("Als Dauerauftrag anlegen …", systemImage: "repeat") { model.newStandingOrder(from: b) }
            .disabled(!model.canMakeStandingOrder(from: b))
        Button("Umsatz kopieren", systemImage: "doc.on.doc") {
            guard redaction.allowsWholeBookingCopy else { return }
            copyToPasteboard(b.clipboardLine)
        }
        .disabled(!redaction.allowsWholeBookingCopy)
        exportMenuButton(for: b)
        Divider()
        Button(b.checked ? "Als neu markieren" : "Als geprüft markieren", systemImage: "checkmark.circle") { model.setChecked(!b.checked, for: b) }
    }

    @ViewBuilder
    private func exportMenuButton(for b: Booking) -> some View {
        let count = model.isMultiSelection && model.selectedBookingIDs.contains(b.id)
            ? model.selectedBookingIDs.count : 1
        Button(count > 1
               ? String(localized: "\(count) Umsätze exportieren …")
               : String(localized: "Umsatz exportieren …"),
               systemImage: "square.and.arrow.up") {
            if !model.selectedBookingIDs.contains(b.id) { model.selectedBookingID = b.id }
            model.openBookingExport(selection: true)
        }
    }
}

/// The purpose shown behind the name in a booking row (#497).
enum ListPurpose {
    static func text(for b: Booking) -> String? {
        text(purpose: b.purpose, cleanPurpose: b.cleanPurpose,
             shownName: b.merchant.name.isEmpty ? b.bookingText : b.merchant.name,
             counterparty: b.counterparty, paymentMethod: b.paymentMethod, bookingText: b.bookingText)
    }

    /// One line of purpose, or nil when there is none or it only repeats what the
    /// row already shows (merchant name, counterparty, payment method), as card
    /// payments often do.
    static func text(purpose: String, cleanPurpose: String, shownName: String, counterparty: String,
                     paymentMethod: String, bookingText: String) -> String? {
        let raw = cleanPurpose.isEmpty ? purpose : cleanPurpose
        let line = raw.split(whereSeparator: \.isWhitespace).joined(separator: " ")
        let key = normalized(line)
        guard !key.isEmpty else { return nil }
        for shown in [shownName, counterparty, paymentMethod, bookingText] {
            let other = normalized(shown)
            if !other.isEmpty, other.contains(key) { return nil }
        }
        return line
    }

    /// Letters and digits only, case-insensitive: "REWE Markt GmbH" ≙ "rewe-markt gmbh".
    private static func normalized(_ s: String) -> String {
        String(s.lowercased().unicodeScalars.filter(CharacterSet.alphanumerics.contains))
    }
}

private struct BookingNoteIndicator: View {
    @Environment(AppModel.self) private var model
    @Environment(\.reportRedaction) private var redaction
    @Environment(\.textScale) private var textScale
    let booking: Booking
    #if os(macOS)
    @State private var isHovered = false
    @State private var didClickDuringHover = false
    @State private var showsPreview = false

    private var canShowPreview: Bool {
        isHovered && !didClickDuringHover && !redaction.contains(.personal)
    }
    #endif

    var body: some View {
        let note = booking.note.trimmingCharacters(in: .whitespacesAndNewlines)
        ZStack {
            if !note.isEmpty {
                Button {
                    #if os(macOS)
                    didClickDuringHover = isHovered
                    showsPreview = false
                    #endif
                    model.editNote(for: booking)
                } label: {
                    SpiralNotebook()
                        .stroke(style: StrokeStyle(lineWidth: 1.15, lineCap: .round))
                        .frame(width: 13 * textScale, height: 14 * textScale)
                        .foregroundStyle(Theme.secondary)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.borderless)
                .bookingRowControl()
                .accessibilityLabel("Notiz bearbeiten")
                #if os(macOS)
                .onHover { inside in
                    isHovered = inside
                    if !inside { didClickDuringHover = false }
                }
                .task(id: canShowPreview) {
                    guard canShowPreview else {
                        showsPreview = false
                        return
                    }
                    try? await Task.sleep(for: .milliseconds(500))
                    if !Task.isCancelled { showsPreview = true }
                }
                .onDisappear {
                    isHovered = false
                    didClickDuringHover = false
                    showsPreview = false
                }
                .capturablePopover(isPresented: $showsPreview, arrowEdge: .bottom) {
                    Text(verbatim: preview(of: note))
                        .appFont(.callout)
                        .lineLimit(3)
                        .frame(maxWidth: 280 * textScale, alignment: .leading)
                        .fixedSize(horizontal: false, vertical: true)
                        .sensitive()
                        .padding(10)
                }
                #endif
            }
        }
        .frame(width: 18 * textScale, height: 16 * textScale)
    }

    private func preview(of note: String) -> String {
        let limited = note.prefix(240)
        let lines = limited.split(separator: "\n", maxSplits: 3, omittingEmptySubsequences: false)
        let preview = lines.prefix(3).joined(separator: "\n").trimmingCharacters(in: .whitespacesAndNewlines)
        return limited.count < note.count || lines.count > 3 ? preview + " …" : preview
    }
}

private struct SpiralNotebook: Shape {
    func path(in rect: CGRect) -> Path {
        var path = Path(roundedRect: CGRect(x: 1.5, y: 2.75, width: 10, height: 10.5), cornerRadius: 1.5)
        for x in [4.0, 6.5, 9.0] {
            path.move(to: CGPoint(x: x, y: 1))
            path.addLine(to: CGPoint(x: x, y: 4.25))
        }
        path.move(to: CGPoint(x: 4, y: 7))
        path.addLine(to: CGPoint(x: 9, y: 7))
        path.move(to: CGPoint(x: 4, y: 9.5))
        path.addLine(to: CGPoint(x: 8, y: 9.5))
        return path.applying(CGAffineTransform(scaleX: rect.width / 13, y: rect.height / 14))
    }
}

/// Exactly three elements: signet · name + subtitle · amount.
struct BookingRow: View {
    @Environment(\.reportRedaction) private var redaction
    @Environment(\.bookingDensity) private var density
    @Environment(\.textScale) private var textScale
    @Environment(AppModel.self) private var model
    let booking: Booking
    var compactDetailsCollapsed = false
    var balance: RunningBalance?
    var onTextSingleClick: (() -> Void)?
    /// Amazon orders: drill-down of all items (and the refunded order's items)
    /// below the row; collapsed rows keep the uniform list height.
    @State private var amazonExpanded = false
    @Environment(\.hiddenCategoryChipAccounts) private var hiddenCategoryChipAccounts

    private var showsCategoryChip: Bool { !hiddenCategoryChipAccounts.contains(booking.accountId) }

    private var showsDetails: Bool {
        density.showsDetails(isActive: density == .compact && model.selectedBookingID == booking.id && !compactDetailsCollapsed)
    }

    private var effectiveDensity: BookingDensity { showsDetails ? .spacious : density }
    private var detailInset: CGFloat { 36 * textScale + 12 + (density == .compact ? 80 * textScale + 4 : 0) }
    private var isSingleLine: Bool { density == .compact && !showsDetails }

    private var visiblePurpose: String {
        showsDetails ? BookingInlineDetails.fullPurpose(for: booking) : density.purpose(for: booking)
    }

    private var amazonRef: AmazonOrderRef? {
        AmazonOrderRef.parse(purpose: booking.purpose, merchantDomain: booking.merchantDomain,
                             bookingText: booking.bookingText)
    }

    #if os(iOS)
    @Environment(\.horizontalSizeClass) private var sizeClass
    private var compactChip: Bool { sizeClass == .compact }
    #else
    private var compactChip: Bool { density == .compact }
    #endif

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: isSingleLine ? .center : .top, spacing: density == .compact ? 8 : 12) {
            if density == .compact {
                Text(DateText.medium(booking.bookingDate))
                    .appFont(.callout, monospacedDigit: true)
                    .foregroundStyle(Theme.secondary)
                    .lineLimit(1)
                    .fixedSize()
                    .bookingNoteOccupied()
                    .frame(width: 80 * textScale, alignment: .leading)
            }
            MerchantSignet(merchant: booking.merchant, categorySymbol: booking.categorySymbol, size: isSingleLine ? 26 : 36, showNewDot: !booking.checked)
                .opacity(booking.isPending ? 0.7 : 1)
                .overlay {
                    // not booked yet: dashed outline instead of a solid tile
                    if booking.isPending {
                        RoundedRectangle(cornerRadius: (isSingleLine ? 26 : 36) * 0.28, style: .continuous)
                            .strokeBorder(Theme.secondary, style: StrokeStyle(lineWidth: 1.5, dash: [3, 3]))
                    }
                }
                .bookingNoteOccupied()
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 5) {
                    BookingRowText(booking.merchant.name.isEmpty ? booking.bookingText : booking.merchant.name,
                                   prominent: true, wraps: showsDetails, onSingleClick: onTextSingleClick)
                        .fixedSize(horizontal: false, vertical: true)
                        .layoutPriority(1)
                    if booking.documentCount > 0 {
                        Image(systemName: "doc.text.fill")
                            .appFont(.caption)
                            .foregroundStyle(Theme.accent.opacity(0.8))
                            .help("\(booking.documentCount) Beleg(e) – Leertaste für Vorschau")
                    }
                    if model.recurringSeries(for: booking) != nil {
                        Image(systemName: "repeat")
                            .appFont(.caption, weight: .semibold)
                            .foregroundStyle(Theme.secondary)
                            .help("Wiederkehrende Buchung")
                            .accessibilityLabel("Wiederkehrend")
                    }
                    RefundLinkMarker(booking: booking)
                    if isSingleLine && showsCategoryChip {
                        // Keep the name and amount first. Show the category only
                        // when its complete label fits; the purpose yields first.
                        ViewThatFits(in: .horizontal) {
                            BookingCategoryLabel(booking: booking).fixedSize()
                            Color.clear.frame(width: 0, height: 0)
                        }
                        .layoutPriority(-1)
                    }
                    if isSingleLine, !booking.tags.isEmpty {
                        // like the category: only when the markers fit, the purpose yields first
                        ViewThatFits(in: .horizontal) {
                            BookingTagMarkers(tags: booking.tags)
                            Color.clear.frame(width: 0, height: 0)
                        }
                        .layoutPriority(-1)
                    }
                    if isSingleLine, !visiblePurpose.isEmpty {
                        BookingRowText(visiblePurpose, onSingleClick: onTextSingleClick)
                            .layoutPriority(-2)
                    }
                }
                .bookingNoteOccupied()
                if !showsDetails && !isSingleLine {
                    HStack(spacing: 6) {
                        #if !os(iOS)
                        if model.showsAccountMarkers {
                            AccountChip(label: model.accountShortLabel(booking.accountId), color: model.accountColor(booking.accountId),
                                        symbol: model.accountSymbol(booking.accountId), compact: compactChip)
                        }
                        #endif
                        // category with the sidebar's icon and colour, then the payment method
                        if showsCategoryChip {
                            BookingCategoryLabel(booking: booking)
                                .layoutPriority(1)
                        }
                        BookingTagMarkers(tags: booking.tags)
                        if let ref = amazonRef {
                            // Amazon order: what was ordered beats the "Bestellung" kind label.
                            // Refunds show the refunded order's items (joined via the order code).
                            let refundSource = ref.isRefund ? model.amazonRefundSource(booking) : nil
                            // Refund rows start flush like every other row ("· item…");
                            // the ↩ marker goes at the end of the item text.
                            let itemsText = refundSource.map { "· \($0.items)  ↩" }
                                ?? (ref.items.isEmpty ? nil : "· \(ref.items)")
                            if let itemsText {
                                BookingRowText(itemsText, onSingleClick: onTextSingleClick)
                                    .layoutPriority(-1)
                                    .help(ref.isRefund
                                          ? "Erstattung für Bestellung #\(ref.code)"
                                          : ref.itemList.joined(separator: "\n"))
                            }
                            if ref.moreCount > 0 {
                                // items beyond the two shown; the drill-down lists everything
                                Text("+\(ref.moreCount) weitere")
                                    .appFont(.caption2, weight: .medium)
                                    .foregroundStyle(Theme.tertiary)
                                    .lineLimit(1)
                                    .help(ref.itemList.dropFirst(2).joined(separator: "\n"))
                            }
                            if ref.isSubscription {
                                // "D01-…" charges are the recurring Spar-Abo, plain orders are one-off
                                Label("Abo", systemImage: "arrow.triangle.2.circlepath")
                                    .labelStyle(.titleAndIcon)
                                    .appFont(.caption2, weight: .medium)
                                    .foregroundStyle(Theme.tertiary)
                                    .help("Abonnement (Spar-Abo) – wiederkehrende Abbuchung")
                            }
                            Text("· #\(ref.code)")
                                .sensitive()
                                .appFont(.callout, monospacedDigit: true)
                                .foregroundStyle(Theme.tertiary)
                                .lineLimit(1)
                                .fixedSize()
                                .layoutPriority(-1)
                            // drill-down when there is more than the head can show
                            if ref.itemList.count > 1 || refundSource != nil {
                                Button { amazonExpanded.toggle() } label: {
                                    Image(systemName: amazonExpanded ? "chevron.down" : "chevron.right")
                                        .appFont(.caption2, weight: .semibold)
                                        .foregroundStyle(Theme.tertiary)
                                }
                                .buttonStyle(.borderless)
                                .bookingRowControl()
                                .help(amazonExpanded ? "Artikel einklappen" : "Alle Artikel anzeigen")
                                .accessibilityLabel(amazonExpanded ? Text("Artikel einklappen") : Text("Alle Artikel anzeigen"))
                            }
                        } else if density == .compact, !density.purpose(for: booking).isEmpty {
                            BookingRowText(density.purpose(for: booking), onSingleClick: onTextSingleClick)
                                .layoutPriority(-1)
                        } else if let counter = model.counterAccount(for: booking) {
                            Text("· ⇄ \(counter.name)")
                                .sensitive()
                                .appFont(.callout)
                                .foregroundStyle(Theme.secondary)
                                .lineLimit(1)
                                .layoutPriority(-1)
                        } else if !booking.paymentMethod.isEmpty {
                            Text("· \(booking.paymentMethod)")
                                .appFont(.callout)
                                .foregroundStyle(Theme.secondary)
                                .lineLimit(1)
                                .layoutPriority(-1)
                        }
                    }
                    .bookingNoteOccupied()
                    .frame(minWidth: 0, maxWidth: .infinity, alignment: .leading)
                    .clipped()
                }
            }
            .frame(minWidth: 0, maxWidth: .infinity, alignment: .leading)
            .layoutPriority(0)
            Spacer(minLength: isSingleLine ? 0 : 8)
            HStack(spacing: 4) {
                BookingNoteIndicator(booking: booking)
                    .bookingNoteOccupied()
                // Reserve both slots so the note stays beside the checkmark even on unchecked rows.
                ZStack {
                    if booking.checked, model.lingeringBookingID != booking.id {
                        Image(systemName: "checkmark")
                            .appFont(.caption, weight: .semibold)
                            .foregroundStyle(Theme.tertiary)
                            .help("Geprüft")
                    }
                }
                .frame(width: 14 * textScale, height: 16 * textScale)
            }
            if booking.isPending {
                Image(systemName: "clock")
                    .appFont(.caption, weight: .semibold)
                    .foregroundStyle(Theme.tertiary)
                    .help("Vorgemerkt – noch nicht gebucht")
                    .accessibilityLabel("Vorgemerkt")
            }
            // compact rows stay one line: the balance goes next to the amount
            let amountLayout = isSingleLine ? AnyLayout(HStackLayout(spacing: 8)) : AnyLayout(VStackLayout(alignment: .trailing, spacing: 1))
            amountLayout {
                if let ref = amazonRef {
                    // Order account: the direction is never colour alone — charges get a
                    // minus, refunds a plus (and a spoken label); both are badged
                    // symmetrically (same padding, same alignment).
                    Text(ref.isRefund ? Money.signed(booking.amountCents, currency: booking.currency) : Money.format(-abs(booking.amountCents), currency: booking.currency))
                        .sensitive(.amounts)
                        .appFont(.body, weight: .semibold, monospacedDigit: true)
                        .foregroundStyle(ref.isRefund ? Theme.inflow : Theme.charge)
                        .padding(.horizontal, 8).padding(.vertical, 3)
                        .background(Capsule().fill(ref.isRefund ? Theme.inflowBadge : Theme.outflowBadge))
                        .lineLimit(1).fixedSize()
                        .help(ref.isRefund ? "Gutschrift (Erstattung)" : "Bestellung")
                        .accessibilityLabel(Text(verbatim: "\(ref.isRefund ? String(localized: "Erstattung") : String(localized: "Belastung")): \(redaction.masking(Money.format(abs(booking.amountCents), currency: booking.currency), .amounts))"))
                } else {
                    AmountLabel(cents: booking.amountCents, currency: booking.currency)
                        .lineLimit(1)
                        .fixedSize()
                        .padding(.horizontal, booking.isPending ? 6 : 0)
                        .padding(.vertical, booking.isPending ? 2 : 0)
                        .overlay {
                            if booking.isPending {
                                Capsule().strokeBorder(Theme.secondary, style: StrokeStyle(lineWidth: 1, dash: [3, 3]))
                            }
                        }
                }
                // balance after this booking (#577); pending ones have none
                if let balance {
                    Text(balance.text)
                        .sensitive(.amounts)
                        .appFont(.caption, monospacedDigit: true)
                        .foregroundStyle(Theme.tertiary)
                        .lineLimit(1).fixedSize()
                        .help("Saldo nach dieser Buchung")
                        .accessibilityLabel(Text(verbatim: "\(String(localized: "Saldo")): \(redaction.masking(balance.text, .amounts))"))
                }
            }
            .bookingNoteOccupied()
        }
        if (showsDetails || density == .standard), amazonRef == nil, !visiblePurpose.isEmpty {
            BookingRowText(visiblePurpose, wraps: showsDetails, onSingleClick: onTextSingleClick)
                .fixedSize(horizontal: false, vertical: true)
                .bookingNoteOccupied()
                .padding(.leading, detailInset)
        }
        if showsDetails {
            BookingInlineDetails(booking: booking, showsCategoryDropdown: showsCategoryChip, onTextSingleClick: onTextSingleClick)
                .padding(.top, 4)
                .padding(.leading, detailInset)
        }
        if (amazonExpanded && density != .compact) || showsDetails, let ref = amazonRef {
            // drill-down: every item on its own line, indented under the signet
            VStack(alignment: .leading, spacing: 2) {
                if let source = model.amazonRefundSource(booking) {
                    Text("Erstattung zu:").appFont(.caption2).foregroundStyle(Theme.tertiary)
                    ForEach(source.list, id: \.self) { item in
                        orderItemText(item)
                    }
                } else {
                    ForEach(ref.itemList, id: \.self) { item in
                        orderItemText(item)
                    }
                }
            }
            .bookingNoteOccupied()
            .padding(.leading, detailInset)
            .padding(.bottom, 4)
        }
        }
        #if os(macOS)
        .frame(minHeight: (effectiveDensity == .compact ? 26 : 36) * textScale, alignment: .top)
        #else
        .frame(minHeight: 40, alignment: .top)
        #endif
        .padding(.horizontal, 12)
        .padding(.vertical, effectiveDensity.verticalPadding * textScale)
        .background {
            ZStack {
                // not booked yet: light diagonal hatching under the whole row
                if booking.isPending { PendingHatch() }
                RowSelectionBackground(id: booking.id)
            }
        }
        // the row is reused for another booking (positional identity, `StreamItem`)
        .onChange(of: booking.id) { amazonExpanded = false }
    }

    @ViewBuilder
    private func orderItemText(_ item: String) -> some View {
        let text = Text(item).sensitive().appFont(.callout).foregroundStyle(Theme.secondary)
        if redaction.allowsBookingTextCopy {
            text.textSelection(.enabled)
        } else {
            text
        }
    }
}

/// Light diagonal hatching that marks a not yet booked ("vorgemerkt") row.
/// Drawn with the theme's secondary colour, so it reads in light and dark.
struct PendingHatch: View {
    var spacing: CGFloat = 7
    var body: some View {
        Canvas { ctx, size in
            var path = Path()
            var x = -size.height
            while x < size.width {
                path.move(to: CGPoint(x: x, y: size.height))
                path.addLine(to: CGPoint(x: x + size.height, y: 0))
                x += spacing
            }
            ctx.stroke(path, with: .color(Theme.secondary.opacity(0.14)), lineWidth: 1)
        }
        .allowsHitTesting(false)
        .accessibilityHidden(true)
    }
}

/// Bar below the list while several bookings are selected (#878): count and sum over all
/// accounts, one sum per currency. Observes the selection itself, so the list is not invalidated.
private struct SelectionSummaryBar: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        if model.isSelectingAllBookings || model.selectAllAfterReload {
            // ⌘A loads the rest of the list first (#1696): no partial count or sum meanwhile
            HStack(spacing: 8) {
                ProgressView().controlSize(.small)
                Text("Alle \(model.bookingCountLabel) Buchungen werden ausgewählt …")
                    .appFont(.callout, weight: .medium, monospacedDigit: true)
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 20).padding(.vertical, 10)
            .frame(maxWidth: .infinity)
            .background(.bar)
            .overlay(alignment: .top) { Divider() }
        } else if model.isMultiSelection {
            let totals = model.selectionTotals
            HStack(alignment: .firstTextBaseline, spacing: 12) {
                Text("\(model.selectedBookingIDs.count) Buchungen ausgewählt")
                    .appFont(.callout, weight: .medium, monospacedDigit: true)
                Spacer(minLength: 12)
                VStack(alignment: .trailing, spacing: 2) {
                    ForEach(totals, id: \.currency) { total in
                        LabeledContent("Summe") {
                            Text(Money.format(total.cents, currency: total.currency))
                                .sensitive(.amounts)
                                .appFont(.callout, weight: .semibold, monospacedDigit: true)
                                .foregroundStyle(Theme.outflow)
                        }
                        .labeledContentStyle(.inlineSum)
                    }
                }
                .fixedSize()
            }
            .padding(.horizontal, 20).padding(.vertical, 10)
            .frame(maxWidth: .infinity)
            .background(.bar)
            .overlay(alignment: .top) { Divider() }
            .transition(.move(edge: .bottom).combined(with: .opacity))
        }
    }
}

/// "Summe  1.234,56 €" on one line (the default macOS style puts label and value apart).
private struct InlineSumStyle: LabeledContentStyle {
    func makeBody(configuration: Configuration) -> some View {
        HStack(spacing: 8) {
            configuration.label.appFont(.callout).foregroundStyle(Theme.secondary)
            configuration.content
        }
    }
}
private extension LabeledContentStyle where Self == InlineSumStyle {
    static var inlineSum: InlineSumStyle { InlineSumStyle() }
}

/// Selection highlight reads selection locally instead of invalidating day cards.
/// Compact booking rows also observe the active selection to expand their details.
private struct RowSelectionBackground: View {
    @Environment(AppModel.self) private var model
    let id: Booking.ID

    var body: some View {
        model.selectedBookingIDs.contains(id) ? Theme.selection : Color.clear
    }
}

/// "Selected" for assistive technologies; like `RowSelectionBackground`, it keeps
/// the selection out of the day card's dependencies.
private struct RowSelectionTraits: ViewModifier {
    @Environment(AppModel.self) private var model
    let id: Booking.ID

    func body(content: Content) -> some View {
        content.accessibilityAddTraits(model.selectedBookingIDs.contains(id) ? [.isButton, .isSelected] : .isButton)
    }
}

/// Scrolls the stream to the selected booking. A view of its own, so the
/// selection is not a dependency of the whole list.
private struct SelectionScroller: View {
    @Environment(\.bookingDensity) private var density
    @Environment(AppModel.self) private var model
    let proxy: ScrollViewProxy

    var body: some View {
        Color.clear
            .onChange(of: model.selectedBookingID) { _, id in
                guard let id else { return }
                // the day jump already scrolled to the day: do not override it
                if model.jumpSelectedBookingID == id { model.jumpSelectedBookingID = nil; return }
                scroll(to: id)
            }
            // A keyboard move onto the row that is already selected (Pos1 on the first row,
            // Ende/Bild↓ on the last) changes no selection: scroll to it anyway.
            .onChange(of: model.keyboardScrollToken) { _, _ in
                guard let id = model.selectedBookingID else { return }
                scroll(to: id)
            }
    }

    private func scroll(to id: Booking.ID) {
        let byKeyboard = model.keyboardSelectedBookingID == id
        model.keyboardSelectedBookingID = nil
        // Every row is an element of the lazy stream, so the proxy finds it
        // even while it is off-screen; its target is its position (`StreamItem`).
        // „Neue Umsätze“ shows its cards instead of the stream.
        guard let slot = model.showsNewBookings ? .newBooking(id) : model.streamSlots.booking[id] else { return }
        // Without animation for the keyboard: while an arrow key is held down each step restarted
        // it, and the list fell behind the selection until the key was released.
        withAnimation(byKeyboard ? nil : Motion.animation(.easeOut(duration: 0.15))) {
            proxy.scrollTo(slot, anchor: SelectionScrollAnchor.anchor(density: density, byKeyboard: byKeyboard))
        }
    }
}

/// Where the stream scrolls a newly selected booking to (#993).
enum SelectionScrollAnchor {
    /// Keyboard moves in a dense list pin the row to the top: an expanded row can exceed
    /// the viewport, and its title must stay visible. A click hits a row that is already
    /// visible: scroll only as far as needed (usually not at all), so the row expands in
    /// place instead of the whole list jumping.
    static func anchor(density: BookingDensity, byKeyboard: Bool) -> UnitPoint? {
        density != .standard && byKeyboard ? .top : nil
    }
}

/// Category of a booking as in the sidebar: its icon and colour
/// ("Ohne Kategorie" like the sidebar entry, in the warning colour).
struct BookingCategoryLabel: View {
    let booking: Booking
    var showsPath = false
    @Environment(AppModel.self) private var model
    #if os(macOS)
    @State private var showsPicker = false
    #endif

    var body: some View {
        let none = booking.categoryName.isEmpty
        let color = none ? Theme.warning : Color.readable(hex: booking.categoryColorHex)
        HStack(spacing: 3) {
            #if os(macOS)
            Button { showsPicker = true } label: {
                HStack(spacing: 3) {
                    label(none: none)
                    Image(systemName: "chevron.down").appFont(.caption2).accessibilityHidden(true)
                }
                .foregroundStyle(color)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .bookingRowControl()
            .capturablePopover(isPresented: $showsPicker, arrowEdge: .bottom) {
                BookingCategoryPicker(booking: booking) { showsPicker = false }
                    .environment(model)
            }
            .onChange(of: booking.id) { showsPicker = false }
            #else
            label(none: none).foregroundStyle(color)
            #endif
            if none { BookingSuggestionHint(booking: booking) }
        }
    }

    private func label(none: Bool) -> some View {
        HStack(spacing: 3) {
            Image(systemName: none
                ? "questionmark.folder"
                : (booking.categorySymbol.isEmpty ? SymbolCatalog.defaultCategorySymbol : booking.categorySymbol))
                .appFont(.caption)
            let name = showsPath && !booking.categoryPath.isEmpty ? booking.categoryPath : booking.categoryName
            Text(none ? model.uncategorizedLabel : name)
                .sensitive(.labels)
                .appFont(.callout, weight: .medium)
                .lineLimit(1)
        }
    }
}

/// The history's suggestion (#664) behind "Ohne Kategorie": the category the
/// booking would get, in the secondary colour so it never reads as assigned.
/// A view of its own, so only uncategorised rows depend on the suggestions.
private struct BookingSuggestionHint: View {
    let booking: Booking
    @Environment(AppModel.self) private var model

    var body: some View {
        if let s = model.listSuggestions[booking.id],
           let category = model.categories.first(where: { $0.id == s.categoryId }) {
            HStack(spacing: 3) {
                Image(systemName: "clock.arrow.circlepath").appFont(.caption)
                Text(category.name).sensitive(.labels).appFont(.callout).lineLimit(1)
            }
            .foregroundStyle(Theme.secondary)
            .padding(.leading, 4)
            .accessibilityElement(children: .combine)
            .accessibilityLabel(Text("Vorschlag: \(category.name)"))
        }
    }
}

/// Account of a booking in mixed lists: the account's symbol in its colour
/// + short name (#170 – a symbol is easier to spot than a small dot).
struct AccountChip: View {
    let label: String
    let color: Color
    var symbol = "building.columns"
    /// Narrow rows: just the symbol (the category keeps the room). Otherwise the label shows
    /// whenever the whole chip fits the room the row offers.
    var compact = false

    var body: some View {
        AccountChipLayout {
            Capsule().fill(color.opacity(0.12))
            Image(systemName: symbol)
                .appFont(.caption, weight: .semibold)
                .foregroundStyle(color)
            if !compact {
                Text(label)
                    .sensitive()
                    .appFont(.subheadline, weight: .medium)
                    .foregroundStyle(color)
                    .lineLimit(1)
            }
        }
        // a label that does not fit is placed outside the chip
        .clipped()
        .help(label)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text("Konto: \(label)"))
    }
}

/// Symbol and label in a tinted capsule, or only the symbol when the full chip does not fit
/// the offered width (or there is no label). One layout instead of `ViewThatFits` over a full
/// and a compact chip, which built and measured both for every booking row on screen.
/// Subviews: capsule, symbol, optional label.
private struct AccountChipLayout: Layout {
    private static let padding: CGFloat = 6
    private static let spacing: CGFloat = 4
    private static let vertical: CGFloat = 1.5

    /// Ideal sizes, measured once per subview change: the stack around the chip asks for its
    /// size several times per layout pass.
    struct Sizes {
        var icon: CGSize
        var label: CGSize?
        var full: CGSize?
        var compact: CGSize
    }

    func makeCache(subviews: Subviews) -> Sizes {
        let icon = subviews[1].sizeThatFits(.unspecified)
        let label = subviews.count > 2 ? subviews[2].sizeThatFits(.unspecified) : nil
        let full = label.map {
            CGSize(width: Self.padding * 2 + icon.width + Self.spacing + $0.width,
                   height: max(icon.height, $0.height) + Self.vertical * 2)
        }
        return Sizes(icon: icon, label: label, full: full,
                     compact: CGSize(width: icon.width, height: icon.height + Self.vertical * 2))
    }

    func updateCache(_ cache: inout Sizes, subviews: Subviews) {
        cache = makeCache(subviews: subviews)
    }

    /// The full chip when it fits `width` (nil: ideal size, no limit).
    private func full(_ cache: Sizes, within width: CGFloat?) -> CGSize? {
        cache.full.flatMap { (width ?? .infinity) >= $0.width ? $0 : nil }
    }

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout Sizes) -> CGSize {
        full(cache, within: proposal.width) ?? cache.compact
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout Sizes) {
        let showsLabel = full(cache, within: bounds.width + 0.5) != nil
        let iconX = bounds.minX + (showsLabel ? Self.padding : 0)
        subviews[0].place(at: bounds.origin, proposal: showsLabel ? ProposedViewSize(bounds.size) : .zero)
        subviews[1].place(at: CGPoint(x: iconX, y: bounds.midY), anchor: .leading, proposal: ProposedViewSize(cache.icon))
        guard subviews.count > 2, let label = cache.label else { return }
        // outside the clipped bounds when it does not fit
        let labelX = showsLabel ? iconX + cache.icon.width + Self.spacing : bounds.maxX + 1000
        subviews[2].place(at: CGPoint(x: labelX, y: bounds.midY), anchor: .leading, proposal: ProposedViewSize(label))
    }
}

/// Outflows dark slate (or red / orange, Einstellungen › Allgemein, #218), inflows an
/// emerald (or blue) badge; the sign always carries the direction too.
struct AmountLabel: View {
    let cents: Int64
    /// The booking's own currency; an empty one is EUR (as in the core).
    var currency: String = "EUR"
    @Environment(\.negativeAmountStyle) private var negativeStyle

    private var code: String { currency.isEmpty ? "EUR" : currency.uppercased() }

    var body: some View {
        if cents > 0 {
            Text(Money.signed(cents, currency: code))
                .sensitive(.amounts)
                .appFont(.body, weight: .semibold, monospacedDigit: true)
                .foregroundStyle(negativeStyle.positiveColor)
                .padding(.horizontal, 8).padding(.vertical, 3)
                .background(Capsule().fill(negativeStyle.positiveBadge))
        } else {
            Text(Money.format(cents, currency: code))
                .sensitive(.amounts)
                .appFont(.body, weight: .medium, monospacedDigit: true)
                .foregroundStyle(cents < 0 ? negativeStyle.color : Theme.outflow)
        }
    }
}

/// End of the list: the default limit cut off older bookings – offer to load them.
struct LoadMoreBookingsFooter: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(spacing: 8) {
            Text("\(model.loadedMatchingCount) von \(model.loadedMatchingCount + model.remainingBookingCount) Buchungen geladen – ältere Buchungen sind noch nicht sichtbar.")
                .appFont(.caption).foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
            // always laid out, only faded: the footer keeps its height while a page loads
            ProgressView().controlSize(.small)
                .opacity(model.isLoadingMoreBookings ? 1 : 0)
                .accessibilityHidden(!model.isLoadingMoreBookings)
            HStack(spacing: 10) {
                Button("Weitere Buchungen laden") { model.loadMoreBookings() }
                    .disabled(model.isLoadingMoreBookings)
                Button("Alle Buchungen laden") { model.loadAllBookings() }
            }
            .buttonStyle(.bordered)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 16)
        // The footer scrolled into view: fetch the next page. Runs again after
        // each page for as long as the footer stays visible.
        .task(id: model.bookings.count) { model.loadMoreBookings() }
    }
}

/// Window title from `model.title`, read in the modifier's own body so a title change re-runs
/// only this modifier, not the view that applies it. macOS: the window carries the view's
/// title, the toolbar does not show it (macOS 15+). iOS keeps the empty navigation title.
struct WindowTitle: ViewModifier {
    @Environment(AppModel.self) private var model
    /// The title bar is drawn by AppKit, so `.sensitive()` cannot cover it:
    /// any bug report capture shows the app name (the title may be an account).
    @Environment(\.reportRedaction) private var redaction

    func body(content: Content) -> some View {
        #if os(macOS)
        let title = redaction.isDisjoint(with: [.personal, .labels]) ? model.title : "GiroCrab"
        if #available(macOS 15.0, *) {
            content.navigationTitle(title).toolbar(removing: .title)
        } else {
            content.navigationTitle(title)
        }
        #else
        content.navigationTitle("")
        #endif
    }
}

/// Toolbar of the booking list. macOS: built from the stored `ToolbarLayout` (#2049), a plain toolbar
/// without SwiftUI's customization; every command is also in the menu bar. iOS: unchanged.
///
/// macOS: the modifier's body reads no model state and holds no `@AppStorage` (only the observable
/// `ToolbarLayoutStore.shared.layout`, which changes only when the layout does); each item's
/// content is a view of its own that reads what it shows. Rebuilding the toolbar content
/// re-bridges every item to AppKit (`ToolbarBridge`), so it must not happen on each sidebar
/// or booking selection: a read here (or an `@AppStorage`, which never compares equal when
/// the parent re-creates this modifier) did exactly that.
struct MainToolbar: ViewModifier {
    @Environment(AppModel.self) private var model
    #if os(iOS)
    @AppStorage(PrivacyMode.enabledKey) private var privacyMode = false
    #endif

    func body(content: Content) -> some View {
        #if os(iOS)
        content.toolbar {
            ToolbarItemGroup(placement: .primaryAction) {
                Button { model.requestSyncAll() } label: { Label("Abrufen", systemImage: "arrow.triangle.2.circlepath") }
                    .disabled(model.bankingBlocksAll)
                Menu {
                    Button { model.openTransfer() } label: { Label("Überweisen", systemImage: "paperplane") }
                        .disabled(!model.canTransfer)
                    Button { model.openManualBooking() } label: { Label("Neue Buchung …", systemImage: "plus") }
                        .disabled(!model.canAddManualBooking)
                    Button { model.openBookingExport() } label: { Label("Umsätze exportieren …", systemImage: "square.and.arrow.up") }
                    Button { model.showPrintSheet = true } label: { Label("Drucken …", systemImage: "printer") }
                    Button { model.openRuleEditor() } label: { Label("Regeln", systemImage: "wand.and.stars") }
                    Toggle(isOn: $privacyMode) { Label("Privatmodus", systemImage: "eye.slash") }
                    Button { model.showSettings = true } label: { Label("Einstellungen", systemImage: "gearshape") }
                    if model.hasOwnVault {
                        Button { model.lock() } label: { Label("Tresor sperren", systemImage: "lock") }
                    }
                    if model.demoActive && (model.hasOwnVault || AppModel.vaultMayHoldRealData) {
                        Button { model.showOwnVault() } label: { Label(AppModel.vaultMayHoldRealData ? "Zurück zum eigenen Tresor" : "Eigenes Konto anlegen", systemImage: "lock.open") }
                    } else if !model.demoActive {
                        Button { model.showDemo() } label: { Label("Demo-Daten-Vorschau starten", systemImage: "theatermasks") }
                    }
                } label: { Label("Mehr", systemImage: "ellipsis.circle") }
            }
        }
        #else
        let layout = ToolbarLayoutStore.shared.layout
        content.toolbar {
            if layout.showsScope {
                ToolbarItem(placement: .principal) { ScopeToolbarControl() }
            }
            if #available(macOS 26, *) {
                ToolbarItemGroup(placement: .primaryAction) { MainToolbarButtons(items: layout.leading) }
                ToolbarSpacer(.flexible, placement: .primaryAction)
                ToolbarItemGroup(placement: .primaryAction) { MainToolbarButtons(items: layout.trailing) }
            } else {
                // no ToolbarSpacer before macOS 26: one group
                ToolbarItemGroup(placement: .primaryAction) { MainToolbarButtons(items: layout.leading + layout.trailing) }
            }
        }
        #endif
    }
}

#if os(macOS)
/// The scope toolbar item: the filter of the view in the booking column.
private struct ScopeToolbarControl: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        // One toolbar item, with the filter for the current view.
        if model.mainColumn == .wealth {
            WealthScopePicker()
        } else if model.mainColumn == .documents {
            DocumentsScopePicker()
        } else {
            switch model.presentation {
            case .securities: DepotTabPicker()
            // Sachwerte have neither booking scopes nor depot tabs
            case .tangible: Color.clear.frame(width: 1, height: 1).accessibilityHidden(true)
            case .bookings: BookingScopePicker().disabled(model.sidebarRowSelection(for: .newBookings).isSelected)
            }
        }
    }
}

/// The buttons of one toolbar group, in the stored order (`ToolbarLayout`).
private struct MainToolbarButtons: View {
    let items: [ToolbarLayout.Item]
    @Environment(AppModel.self) private var model

    var body: some View {
        ForEach(items, id: \.self) { item in
            switch item {
            case .transfer: TransferToolbarButton()
            case .newBooking: NewBookingToolbarButton()
            case .sync: SyncToolbarButton()
            case .inspector: InspectorToolbarButton()
            case .privacy: PrivacyToolbarToggle()
            case .palette:
                Button { model.showCommandPalette = true } label: { Label("Befehle", systemImage: "command") }
                    .help("Befehlspalette (⌘K)")
            case .export:
                Button { model.openBookingExport() } label: { Label("Umsätze exportieren", systemImage: "square.and.arrow.up") }
                    .help("Umsätze als PDF, CSV oder CAMT.053 exportieren … (#53 / #372)")
            case .print:
                Button { model.showPrintSheet = true } label: { Label("Drucken", systemImage: "printer") }
                    .help("Gefilterte Buchungen drucken … (⌘P)")
            case .rules:
                Button { model.openRuleEditor() } label: { Label("Regeln", systemImage: "wand.and.stars") }
                    .help("Kategorisierungsregeln … (⌥⌘K)")
            case .lock: LockToolbarButton()
            case .settings:
                SettingsLink { Label("Einstellungen", systemImage: "gearshape") }
                    .help("Einstellungen … (⌘,)")
            }
        }
    }
}

private struct TransferToolbarButton: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        Button { model.openTransfer() } label: { Label("Überweisen", systemImage: "paperplane") }
            .help("Neue Überweisung (⌘N)")
            .disabled(!model.canTransfer)
    }
}

private struct NewBookingToolbarButton: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        Button { model.openManualBooking() } label: { Label("Neue Buchung", systemImage: "plus") }
            .help("Neue Buchung (⌥⌘N)")
            .disabled(!model.canAddManualBooking)
    }
}

private struct SyncToolbarButton: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        Button { model.requestSyncAll() } label: { Label("Abrufen", systemImage: "arrow.triangle.2.circlepath") }
            .help(model.bankingBlocksAll ? (BankingPolicy.shared.gate?.message ?? "") : String(localized: "Alle Konten abrufen (⌘R)"))
            .disabled(model.bankingBlocksAll)
    }
}

private struct InspectorToolbarButton: View {
    @AppStorage(ViewLayout.inspectorKey) private var showInspector = true

    var body: some View {
        Button { showInspector.toggle() } label: { Label("Informationen", systemImage: "sidebar.trailing") }
            .help(showInspector ? String(localized: "Informationen ausblenden (⌥⌘I)") : String(localized: "Informationen einblenden (⌥⌘I)"))
    }
}

private struct PrivacyToolbarToggle: View {
    @AppStorage(PrivacyMode.enabledKey) private var privacyMode = false
    @AppStorage(PrivacyMode.scopeKey) private var privacyScope = PrivacyMode.defaultScope

    var body: some View {
        Toggle(isOn: $privacyMode) { Label("Privatmodus", systemImage: privacyMode ? "eye.slash" : "eye") }
            .help(privacyMode ? String(localized: "Privatmodus beenden (⇧⌘H)")
                : privacyScope == .personal ? String(localized: "Privatmodus: Beträge und persönliche Angaben ausblenden (⇧⌘H)")
                : String(localized: "Privatmodus: Beträge ausblenden (⇧⌘H)"))
    }
}

private struct LockToolbarButton: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        Button { model.lock() } label: { Label("Tresor sperren", systemImage: "lock") }
            .help("Tresor sperren (⌃⌘L)")
            .disabled(!model.hasOwnVault)
    }
}

/// Modifier keys held at the last mouse-down in the app. SwiftUI runs a tap
/// gesture after the event has passed, so `NSApp.currentEvent` no longer
/// carries them; a local monitor records them as the click arrives.
@MainActor
enum ClickModifiers {
    private(set) static var atLastMouseDown: NSEvent.ModifierFlags = []
    private(set) static var bookingTextHandledMouseDown = false
    private(set) static var bookingTextInClickSequence = false
    private static var monitor: Any?

    static func handledBookingTextMouseDown() {
        bookingTextHandledMouseDown = true
        bookingTextInClickSequence = true
    }

    static func install() {
        guard monitor == nil else { return }
        monitor = NSEvent.addLocalMonitorForEvents(matching: [.leftMouseDown]) { event in
            bookingTextHandledMouseDown = false
            if event.clickCount <= 1 { bookingTextInClickSequence = false }
            atLastMouseDown = event.modifierFlags.intersection([.command, .shift, .option, .control])
            return event
        }
    }
}
#endif
