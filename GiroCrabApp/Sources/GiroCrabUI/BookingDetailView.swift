import SwiftUI
import GiroCrabCore

/// Column 3: permanent context & receipt inspector.
struct InspectorView: View {
    @Environment(AppModel.self) private var model
    /// Order and hidden cards (#448), see `InspectorLayout`.
    @AppStorage(InspectorLayout.defaultsKey) private var layoutValue = ""
    @State private var editingLayout = LaunchOptions.current.inspectorEdit
    @State private var revealedNoteBookingID: Booking.ID?

    private var layout: InspectorLayout { InspectorLayout(storageValue: layoutValue) }

    private func updateLayout(_ change: (inout InspectorLayout) -> Void) {
        var next = layout
        change(&next)
        withAnimation(.snappy(duration: 0.2)) { layoutValue = next.persistedValue }
    }

    var body: some View {
        // The bar sits above the scroll view instead of in a `safeAreaInset` or as its first row:
        // there it got no clicks, and clicks below it landed a row too low (macOS 27, #448, #492).
        VStack(spacing: 0) {
            if showsBookingCards {
                InspectorLayoutBar(editing: $editingLayout, canReset: !layout.isStandard) {
                    updateLayout { $0 = .standard }
                }
            }
            ScrollViewReader { proxy in
                ScrollView {
                    VStack(alignment: .leading, spacing: 14) {
                        if model.mainColumn == .planned {
                            PlannedInspector()
                        } else if model.mainColumn == .directDebits {
                            MandateInspector()
                        } else {
                            SelectedLoanCard()
                            if model.isMultiSelection {
                                MultiSelectionCard()
                            } else if let booking = model.selectedBooking {
                                if editingLayout {
                                    InspectorLayoutEditor(booking: booking, layout: layout, update: updateLayout)
                                } else {
                                    ForEach(visibleSections(for: booking)) { section in
                                        InspectorSectionCard(section: section, booking: booking) {
                                            proxy.scrollTo(InspectorSection.note.id)
                                        }
                                        .id(section.id)
                                    }
                                }
                            } else {
                                VStack(alignment: .leading, spacing: 10) {
                                    Label("Keine Buchung ausgewählt", systemImage: "doc.text.magnifyingglass")
                                        .appFont(.headline)
                                    Text("Wählen Sie eine Buchung, um Beleg, Händler-Historie und Kategorie zu sehen.")
                                        .appFont(.callout).foregroundStyle(Theme.secondary)
                                }
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .card()
                            }
                        }
                    }
                    .padding(16)
                }
                .onChange(of: model.pendingNoteFocus, initial: true) { _, requestedID in
                    guard let requestedID, requestedID == model.selectedBookingID else { return }
                    editingLayout = false
                    if layout.hidden.contains(.note) { revealedNoteBookingID = requestedID }
                }
                .onChange(of: model.selectedBookingID) { _, selectedID in
                    if revealedNoteBookingID != selectedID { revealedNoteBookingID = nil }
                }
                .onChange(of: editingLayout) { _, editing in
                    if editing { revealedNoteBookingID = nil }
                }
            }
        }
        .background(Theme.canvas)
    }

    private func visibleSections(for booking: Booking) -> [InspectorSection] {
        if revealedNoteBookingID == booking.id, layout.hidden.contains(.note) {
            return layout.visible + [.note]
        }
        return layout.visible
    }

    /// The single-booking cards are shown, so their layout can be edited.
    private var showsBookingCards: Bool {
        model.mainColumn != .planned && model.mainColumn != .directDebits
            && !model.isMultiSelection && model.selectedBooking != nil
    }
}

/// The loan's progress above the booking cards while a loan account is selected. A view of its
/// own so that only it follows the sidebar selection, not the whole inspector.
private struct SelectedLoanCard: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        if case .account(let id) = model.shownSelection, let a = model.account(for: id), a.kind == .loan {
            LoanProgressCard(account: a)
        }
    }
}

struct ReceiptCard: View {
    @Environment(AppModel.self) private var model
    let booking: Booking
    @State private var editing = false
    @State private var draftName = ""
    @State private var draftPurpose = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Spacer()
                Button {
                    draftName = booking.counterparty
                    draftPurpose = booking.purpose
                    editing = true
                } label: { Label("Text bearbeiten …", systemImage: "pencil") }
                    .buttonStyle(.borderless).controlSize(.small).appFont(.caption)
                    .help("Gegenpartei und Verwendungszweck korrigieren")
            }
            .capturablePopover(isPresented: $editing) {
                VStack(alignment: .leading, spacing: 8) {
                    Text("Gegenpartei").appFont(.caption).foregroundStyle(Theme.secondary)
                    TextField("Gegenpartei", text: $draftName).textFieldStyle(.roundedBorder)
                    Text("Verwendungszweck").appFont(.caption).foregroundStyle(Theme.secondary)
                    TextEditor(text: $draftPurpose)
                        .appFont(.callout).frame(height: 90)
                        .overlay(RoundedRectangle(cornerRadius: 5).stroke(Theme.secondary.opacity(0.3)))
                    HStack {
                        Spacer()
                        Button("Abbrechen") { editing = false }
                            .keyboardShortcut(.cancelAction)
                        Button("Speichern") {
                            model.updateText(counterparty: draftName, purpose: draftPurpose, for: booking)
                            editing = false
                        }.keyboardShortcut(.defaultAction)
                    }
                }
                .padding(14).frame(width: 340)
            }
            HStack(spacing: 12) {
                MerchantSignet(merchant: booking.merchant, categorySymbol: booking.categorySymbol, size: 48)
                VStack(alignment: .leading, spacing: 2) {
                    Text(booking.merchant.name)
                        .sensitive()
                        .appFont(.title3, weight: .semibold)
                        .foregroundStyle(Theme.text)
                        .textSelection(.enabled)
                    Text("\(booking.accountName) · \(booking.paymentMethod)")
                        .sensitive()
                        .appFont(.caption).foregroundStyle(Theme.secondary)
                    if let counter = model.counterAccount(for: booking) {
                        Text("⇄ \(counter.name) (\(formatIBAN(booking.counterpartyIban)))")
                            .sensitive()
                            .appFont(.caption).foregroundStyle(Theme.secondary)
                            .textSelection(.enabled)
                    } else if !booking.counterpartyIban.isEmpty {
                        Text(formatIBAN(booking.counterpartyIban))
                            .sensitive()
                            .appFont(.caption, design: .monospaced).foregroundStyle(Theme.secondary)
                            .textSelection(.enabled)
                    }
                    MerchantDomainRow(merchant: booking.merchant)
                }
                Spacer()
            }
            Text(Money.signed(booking.amountCents, currency: booking.currency))
                .sensitive(.amounts)
                .font(.system(.largeTitle, design: .rounded, weight: .semibold).monospacedDigit())
                .foregroundStyle(Money.ink(booking.amountCents))
            HStack(spacing: 6) {
                Image(systemName: "calendar").foregroundStyle(Theme.tertiary)
                Text(DateText.long(booking.bookingDate))
                Spacer()
                FlowBadge(flow: booking.flow)
            }
            .appFont(.caption)
            .foregroundStyle(Theme.secondary)
            if booking.flow == .transfer {
                // #574: why this booking counts as an internal transfer
                let ownIban = !booking.counterpartyIban.isEmpty && model.accountsByID.values.contains {
                    $0.iban.replacingOccurrences(of: " ", with: "").caseInsensitiveCompare(booking.counterpartyIban.replacingOccurrences(of: " ", with: "")) == .orderedSame
                }
                let reason: LocalizedStringKey = if ownIban {
                    "Umbuchung erkannt: Die Gegenkonto-IBAN gehört zu einem eigenen Konto."
                } else if booking.categoryName == "Umbuchungen" || booking.categoryPath.hasPrefix("Umbuchungen") {
                    "Umbuchung erkannt: Die Buchung hat die Kategorie „Umbuchungen“."
                } else {
                    "Umbuchung erkannt: Der Buchungstext deutet auf eine Übertragung zwischen eigenen Konten oder eine Abrechnung hin."
                }
                Label(reason, systemImage: "arrow.left.arrow.right")
                    .appFont(.caption)
                    .foregroundStyle(Theme.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let ref = AmazonOrderRef.parse(purpose: booking.purpose, merchantDomain: booking.merchantDomain, bookingText: booking.bookingText, itemRefs: booking.itemRefs), !ref.itemList.isEmpty {
                // Amazon order: one item per line instead of the ";" purpose run;
                // an item with a stored ASIN opens its product page (#354)
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(Array(ref.itemList.enumerated()), id: \.offset) { index, item in
                        if let url = ref.productURL(at: index) {
                            Link(destination: url) {
                                Text(item)
                                    .appFont(.callout)
                                    .foregroundStyle(Theme.accent)
                                    .multilineTextAlignment(.leading)
                                    .fixedSize(horizontal: false, vertical: true)
                            }
                            .sensitive()
                            .help("Produkt auf \(ref.domain) öffnen")
                        } else {
                            Text(item)
                                .sensitive()
                                .appFont(.callout)
                                .foregroundStyle(Theme.text)
                                .textSelection(.enabled)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                    }
                }
            } else if !booking.cleanPurpose.isEmpty {
                Text(booking.cleanPurpose)
                    .sensitive()
                    .appFont(.callout)
                    .foregroundStyle(Theme.text)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let source = model.amazonRefundSource(booking) {
                // refund: jump to the order it belongs to (same order code)
                Button {
                    model.selectedBookingID = source.id
                } label: {
                    Label("Erstattung zu: \(source.items)", systemImage: "arrow.uturn.backward")
                        .sensitive()
                        .appFont(.caption)
                }
                .buttonStyle(.borderless)
                .foregroundStyle(Theme.accent)
                .help("Zur erstatteten Bestellung springen")
            }
            Toggle("Geprüft", isOn: Binding(get: { booking.checked }, set: { model.setChecked($0, for: booking) }))
                .toggleStyle(.switch)
                .controlSize(.small)
                .appFont(.caption)
        }
        .card()
    }
}

/// Free-text user note (imported from MoneyMoney or typed here).
struct NoteCard: View {
    @Environment(AppModel.self) private var model
    let booking: Booking
    var scrollToNote: (() -> Void)?
    @State private var draft = ""
    /// The booking `draft` was started from. The inspector reuses the card for the next selected
    /// booking (no `.id`: rebuilding the text editor on every selection was a large part of the
    /// inspector's update), so a save that runs after the switch (focus loss from the click in
    /// the list) must still go to this booking, never to the new one.
    @State private var draftBooking: Booking?
    /// The text last stored for `draftBooking`: a second save of the same text (focus loss, then
    /// the switch) writes nothing.
    @State private var savedText: String?
    /// Identity of the text editor. It stays across bookings, except after typing: the editor's
    /// undo history then holds the previous booking's edits, and ⌘Z in the next booking's note
    /// would bring that text back (and the next save store it there). A new editor drops it.
    @State private var editorID = 0
    @State private var typed = false
    @FocusState private var focused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Label("Notiz", systemImage: "note.text").appFont(.subheadline, weight: .semibold).foregroundStyle(Theme.text)
            TextEditor(text: $draft)
                .id(editorID)
                .appFont(.callout)
                .frame(minHeight: 54, maxHeight: 120)
                .sensitive()
                .scrollContentBackground(.hidden)
                .focused($focused)
                .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(Theme.border))
                .onChange(of: focused) { _, f in if !f { save() } }
                // Esc: leave the note (saves via focus loss) and go back to the list
                .onKeyPress(.escape) {
                    focused = false
                    model.focusListToken &+= 1
                    return .handled
                }
        }
        .card()
        .onAppear { start(booking); claimFocus() }
        .onDisappear { save() }
        .onChange(of: draft) { if focused { typed = true } }
        .onChange(of: booking.id) {
            // what was typed for the previous booking is stored first; then the editor is left,
            // as with a new card, unless the new booking asked for it
            save()
            start(booking)
            focused = false
            if typed {
                typed = false
                editorID &+= 1
            }
            claimFocus()
        }
        .onChange(of: booking.note) {
            if !focused, draftBooking?.id == booking.id { start(booking) }
        }
        .onChange(of: model.pendingNoteFocus) { _, _ in claimFocus() }
    }

    private func start(_ b: Booking) {
        draft = b.note
        draftBooking = b
        savedText = nil
    }

    /// Takes the focus requested by a double-click / Return in the list for this booking.
    private func claimFocus() {
        // The layout editor shows a preview of the card and must leave the request for
        // the editable card. Give a newly revealed inspector one layout pass before scrolling.
        guard let scrollToNote, model.pendingNoteFocus == booking.id else { return }
        DispatchQueue.main.async {
            guard model.pendingNoteFocus == booking.id else { return }
            guard model.selectedBookingID == booking.id else {
                model.pendingNoteFocus = nil
                return
            }
            scrollToNote()
            model.pendingNoteFocus = nil
            focused = true
        }
    }

    private func save() {
        guard let b = draftBooking else { return }
        let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard text != (savedText ?? b.note) else { return }
        model.updateNote(draft, for: b)
        savedText = text
    }
}

/// Resolved logo domain with source and an edit button (manual override).
struct MerchantDomainRow: View {
    @Environment(AppModel.self) private var model
    let merchant: MerchantInfo
    @State private var editing = false
    @State private var text = ""

    private var sourceLabel: String {
        switch merchant.domainSource {
        case "dictionary": return String(localized: "Wörterbuch")
        case "company": return String(localized: "Firmenname")
        case "person": return String(localized: "Privatperson")
        case "override": return String(localized: "manuell")
        default: return String(localized: "keine Domain")
        }
    }

    var body: some View {
        if editing {
            HStack(spacing: 4) {
                TextField("z. B. enbw.com", text: $text)
                    .textFieldStyle(.roundedBorder)
                    .sensitive()
                    .appFont(.caption)
                    .frame(maxWidth: 180)
                    .autocorrectionDisabled()
                    .onSubmit(save)
                Button("Logo laden", action: save).controlSize(.small)
                if merchant.domainSource == "override" {
                    Button { model.setMerchantDomain(merchant, nil); editing = false } label: { Label("Automatische Erkennung wiederherstellen", systemImage: "arrow.uturn.backward") }
                        .labelStyle(.iconOnly)
                        .buttonStyle(.borderless).help("Automatische Erkennung wiederherstellen")
                }
                Button { editing = false } label: { Label("Abbrechen", systemImage: "xmark") }
                    .labelStyle(.iconOnly).buttonStyle(.borderless)
                    .help("Abbrechen").keyboardShortcut(.cancelAction)
            }
        } else {
            HStack(spacing: 4) {
                Image(systemName: "globe").appFont(.caption2)
                Text(merchant.domain.isEmpty ? (merchant.isPerson ? String(localized: "Privatperson – kein Logo") : String(localized: "Domain festlegen")) : merchant.domain)
                    .appFont(.caption, design: .monospaced)
                    // a guessed domain spells out the (redacted) counterparty; dictionary brands show their logo anyway
                    .sensitive(merchant.domain.isEmpty || merchant.domainSource == "dictionary" ? [] : .personal)
                if !merchant.domain.isEmpty {
                    Text("· \(sourceLabel)").appFont(.caption2)
                }
                Button {
                    text = merchant.domain
                    editing = true
                } label: { Label("Domain bearbeiten", systemImage: "pencil").labelStyle(.iconOnly).appFont(.caption2) }
                    .buttonStyle(.borderless)
                    .help("Eigene Domain für das Logo eintragen")
            }
            .foregroundStyle(Theme.tertiary)
        }
    }

    private func save() {
        model.setMerchantDomain(merchant, text)
        editing = false
    }
}

/// Salary / pension marking of an inflow (GitHub #37). Drives the
/// "Seit letztem Gehalt" period of the income tiles.
struct SalaryCard: View {
    @Environment(AppModel.self) private var model
    let booking: Booking

    private enum Choice: Hashable { case auto, yes, no }

    private var choice: Choice {
        switch booking.salaryFlag {
        case .some(true): .yes
        case .some(false): .no
        case .none: .auto
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Gehalt / Rente")
                .appFont(.subheadline, weight: .semibold)
                .foregroundStyle(booking.isSalary ? Theme.inflow : Theme.text)
            Picker("Gehalt / Rente", selection: Binding(
                get: { choice },
                set: { new in
                    model.setSalaryFlag(booking, new == .auto ? nil : new == .yes)
                }
            )) {
                Text("Automatisch").tag(Choice.auto)
                Text("Ja").tag(Choice.yes)
                Text("Nein").tag(Choice.no)
            }
            .labelsHidden()
            .pickerStyle(.segmented)
            Text(hint)
                .appFont(.caption)
                .foregroundStyle(Theme.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .card()
    }

    private var hint: LocalizedStringKey {
        switch choice {
        case .yes: "Als Gehalt markiert. Weitere Eingänge von diesem Konto zählen ebenfalls als Gehalt."
        case .no: "Wird nie als Gehalt gezählt."
        case .auto where booking.isSalary: "Als Gehalt erkannt (Purpose-Code, Geschäftsvorfallcode oder Verwendungszweck)."
        case .auto: "Nicht als Gehalt erkannt. Markieren Sie den Eingang, damit der Zeitraum „Seit letztem Gehalt“ ihn nutzt."
        }
    }
}

struct FlowBadge: View {
    let flow: FlowKind

    var body: some View {
        let color: Color = switch flow {
        case .investment, .perk: Theme.invest
        case .income: Theme.inflow
        case .loan: Theme.loan
        case .transfer: Theme.secondary
        case .consumption: Theme.outflow
        }
        Text(flow.label)
            .appFont(.caption2, weight: .semibold)
            .foregroundStyle(color)
            .padding(.horizontal, 7).padding(.vertical, 2)
            .background(Capsule().fill(color.opacity(0.08)))
    }
}

struct MerchantHistoryCard: View {
    @Environment(AppModel.self) private var model
    let booking: Booking

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            // The heading opens the list of this merchant's bookings (#495).
            Button { model.focusMerchant(of: booking) } label: {
                HStack(spacing: 4) {
                    Text("Händler-Historie").appFont(.subheadline, weight: .semibold)
                    Image(systemName: "chevron.right")
                        .appFont(.caption, weight: .semibold)
                        .foregroundStyle(Theme.secondary)
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .disabled(booking.merchant.key.isEmpty)
            .help("Alle Buchungen dieses Händlers im gewählten Zeitraum anzeigen")
            .accessibilityHint("Alle Buchungen dieses Händlers im gewählten Zeitraum anzeigen")
            if let s = model.merchantStats, s.key == booking.merchant.key {
                // The merchant also occurs on other accounts (#299): show the
                // total and the booking's own account side by side.
                if let own = model.merchantAccountStats, own.accountId == booking.accountId,
                   own.stats.key == s.key, own.stats.count < s.count {
                    statsRow(label: "Alle Konten", s)
                    statsRow(label: "Dieses Konto", accountName: booking.accountName, own.stats)
                } else {
                    statsRow(label: nil, s)
                }
            }
            if !booking.merchant.isin.isEmpty || !booking.merchant.assetClass.isEmpty {
                Divider()
                HStack(spacing: 14) {
                    if !booking.merchant.assetClass.isEmpty { labeled("Anlage", booking.merchant.assetClass) }
                    if !booking.merchant.isin.isEmpty { labeled("ISIN", booking.merchant.isin) }
                    if !booking.merchant.wkn.isEmpty { labeled("WKN", booking.merchant.wkn) }
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .card()
    }

    private func statsRow(label: LocalizedStringKey?, accountName: String? = nil, _ s: MerchantStats) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            if let label {
                HStack(spacing: 4) {
                    Text(label)
                    if let accountName { Text("·"); Text(accountName).sensitive(.personal) }
                }
                .appFont(.caption, weight: .medium).foregroundStyle(Theme.secondary).lineLimit(1)
            }
            HStack(spacing: 10) {
                stat("Buchungen", "\(s.count)×")
                stat("Summe", Money.format(s.totalCents), sensitive: true)
                stat("Ø Betrag", Money.format(s.averageCents), sensitive: true)
            }
            if !s.firstDate.isEmpty {
                Text("Seit \(DateText.medium(s.firstDate)) · zuletzt \(DateText.medium(s.lastDate))")
                    .appFont(.caption2).foregroundStyle(Theme.tertiary)
            }
        }
    }

    private func stat(_ title: LocalizedStringKey, _ value: String, sensitive: Bool = false) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title).appFont(.caption2).foregroundStyle(Theme.secondary)
            Text(value).appFont(.callout, weight: .semibold, monospacedDigit: true).foregroundStyle(Theme.text).lineLimit(1).minimumScaleFactor(0.7)
                .sensitive(sensitive ? .amounts : [])
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func labeled(_ title: LocalizedStringKey, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(title).appFont(.caption2).foregroundStyle(Theme.secondary)
            Text(value).appFont(.caption, design: .monospaced).textSelection(.enabled)
        }
    }
}

/// 1-click category grid: top-level categories, then one grid of
/// sub-categories per opened level (any depth, #101); plus
/// "Regel für diesen Händler erstellen …".
struct CategoryGridCard: View {
    @Environment(AppModel.self) private var model
    let booking: Booking
    /// Opened categories from the top level down; nil = the booking's branch.
    @State private var openPath: [Int64]?

    /// The booking's category's ancestors, plus the category itself when it
    /// has sub-categories.
    private var currentPath: [Int64] {
        guard let cid = booking.categoryId else { return [] }
        var path: [Int64] = []
        var next: Int64? = cid
        while let id = next, path.count < 32, let c = model.categories.first(where: { $0.id == id }) {
            path.insert(id, at: 0)
            next = c.parentId
        }
        if path.last == cid, model.categoryTree.node(cid)?.children == nil { path.removeLast() }
        return path
    }

    /// A category with sub-categories opens the next level, a leaf is assigned.
    private func pick(_ node: CategoryNode, level: Int, in path: [Int64]) {
        if node.children == nil {
            model.setCategory(node.id, for: booking)
        } else {
            openPath = Array(path.prefix(level)) + [node.id]
        }
    }

    var body: some View {
        let path = openPath ?? currentPath
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("Kategorie").appFont(.subheadline, weight: .semibold)
                Spacer()
                Text(booking.categoryPath.isEmpty ? model.uncategorizedLabel : booking.categoryPath)
                    .appFont(.caption).foregroundStyle(Theme.secondary).lineLimit(1)
            }
            if let s = model.categorySuggestion, s.bookingId == booking.id,
               let category = model.categories.first(where: { $0.id == s.categoryId }) {
                CategorySuggestionBox(booking: booking, suggestion: s, category: category)
            }
            LazyVGrid(columns: [GridItem(.adaptive(minimum: 104), spacing: 6)], spacing: 6) {
                ForEach(model.categoryTree) { node in
                    CategoryChip(category: node.category, selected: node.id == path.first) {
                        pick(node, level: 0, in: path)
                    }
                }
            }
            ForEach(Array(path.enumerated()), id: \.element) { level, id in
                if let kids = model.categoryTree.node(id)?.children {
                    let selected = level + 1 < path.count ? path[level + 1] : booking.categoryId
                    Divider()
                    LazyVGrid(columns: [GridItem(.adaptive(minimum: 120), spacing: 6)], spacing: 6) {
                        ForEach(kids) { kid in
                            CategoryChip(category: kid.category, selected: kid.id == selected, filled: true) {
                                pick(kid, level: level + 1, in: path)
                            }
                        }
                    }
                }
            }
            Button { model.splitBooking = booking } label: {
                Label("Umsatz aufteilen (Splitbuchung) …", systemImage: "arrow.triangle.branch").frame(maxWidth: .infinity)
            }
            .buttonStyle(.bordered).controlSize(.large)
            Button {
                model.openRuleEditor(from: booking)
            } label: {
                Label("Regel für diesen Händler erstellen …", systemImage: "wand.and.stars")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .tint(Theme.accent)
            .controlSize(.large)
            Text("In der Liste: C kategorisieren · R Regel erstellen")
                .appFont(.caption2).foregroundStyle(Theme.tertiary)
        }
        .card()
        .onChange(of: booking.id) { _, _ in openPath = nil }
    }
}

/// "Vorschlag (aus Historie)" (#664): the category earlier bookings of this
/// payee got, to accept with one click or to turn into a rule. Dashed, so it
/// never reads as an assigned category.
struct CategorySuggestionBox: View {
    @Environment(AppModel.self) private var model
    let booking: Booking
    let suggestion: CategorySuggestion
    let category: GiroCrabCore.Category

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Label("Vorschlag (aus Historie)", systemImage: "clock.arrow.circlepath")
                .appFont(.caption, weight: .semibold).foregroundStyle(Theme.secondary)
            HStack(spacing: 6) {
                Image(systemName: category.sfSymbol).appFont(.subheadline)
                Text(category.path).appFont(.callout, weight: .semibold).lineLimit(2).sensitive(.labels)
            }
            .foregroundStyle(Theme.text)
            reason.appFont(.caption2).foregroundStyle(Theme.secondary)
            // side by side while both labels fit, else stacked (narrow inspector, larger text)
            ViewThatFits(in: .horizontal) {
                HStack(spacing: 6) { buttons }
                VStack(spacing: 6) { buttons }
            }
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 8, style: .continuous).fill(Theme.canvas))
        .overlay(RoundedRectangle(cornerRadius: 8, style: .continuous)
            .strokeBorder(Theme.accent, style: StrokeStyle(lineWidth: 1, dash: [4, 3])))
    }

    @ViewBuilder private var buttons: some View {
        Button { model.setCategory(category.id, for: booking) } label: {
            Label("Übernehmen", systemImage: "checkmark").fixedSize().frame(maxWidth: .infinity)
        }
        .buttonStyle(.borderedProminent).tint(Theme.accent)
        Button {
            model.openRuleEditor(draft: Self.ruleDraft(model.newRuleDraft(from: booking), booking: booking, suggestion: suggestion))
        } label: {
            Label("Als Regel …", systemImage: "wand.and.stars").fixedSize().frame(maxWidth: .infinity)
        }
        .buttonStyle(.bordered)
    }

    /// "Als Regel …": the usual draft with the suggested category, matching the
    /// IBAN when that is what told this payee apart from a namesake.
    static func ruleDraft(_ base: Rule, booking: Booking, suggestion: CategorySuggestion) -> Rule {
        var rule = base
        rule.categoryId = suggestion.categoryId
        let iban = booking.counterpartyIban.filter { !$0.isWhitespace }
        if suggestion.basis == .iban, !iban.isEmpty {
            rule.field = .counterpartyIban
            rule.pattern = iban
        }
        return rule
    }

    /// At least two bookings back a suggestion, so the plural always fits. The
    /// core weighs the votes, so a split count says why it still decided.
    private var reason: Text {
        let (n, of) = (Int(suggestion.support), Int(suggestion.considered))
        switch (suggestion.basis, n == of) {
        case (.iban, true): return Text("Wie alle \(n) Buchungen an diese IBAN")
        case (.merchant, true): return Text("Wie alle \(n) Buchungen bei diesem Händler")
        case (.iban, false): return Text("Wie \(n) von \(of) Buchungen an diese IBAN; zeitnahe zählen mehr, Regel-Buchungen weniger")
        case (.merchant, false): return Text("Wie \(n) von \(of) Buchungen bei diesem Händler; zeitnahe zählen mehr, Regel-Buchungen weniger")
        case (.similar, true): return Text("Wie alle \(n) Buchungen mit ähnlichem Text")
        case (.similar, false): return Text("Wie \(n) von \(of) Buchungen mit ähnlichem Text; ähnlichere und zeitnahe zählen mehr")
        }
    }
}

struct CategoryChip: View {
    let category: GiroCrabCore.Category
    var selected: Bool
    var filled = false
    /// Label override (e.g. the path below the top-level category); default: the name.
    var title: String?
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 5) {
                Image(systemName: category.sfSymbol).appFont(.subheadline)
                // two lines instead of shrinking: long names stay at the text size (#343)
                Text(title ?? category.name).appFont(.caption).lineLimit(2).sensitive(.labels)
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 8).padding(.vertical, 6)
            .frame(maxHeight: .infinity)
            // Selected: accent fill and border, text in the normal colour – accent-
            // coloured text on an accent fill is unreadable with light accents (yellow).
            .foregroundStyle(Theme.text)
            .fontWeight(selected ? .semibold : nil)
            .background(RoundedRectangle(cornerRadius: 7, style: .continuous).fill(selected ? Theme.selection : (filled ? Theme.canvas : Theme.card)))
            .overlay(RoundedRectangle(cornerRadius: 7, style: .continuous).strokeBorder(selected ? Theme.accent : Theme.border, lineWidth: selected ? 1.5 : 1))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }
}

/// SEPA & raw bank data, collapsed by default.
struct RawDataCard: View {
    let booking: Booking
    @State private var expanded = false

    private func sepa(_ key: String) -> String? {
        guard let r = booking.purpose.range(of: key + "+") else { return nil }
        let rest = booking.purpose[r.upperBound...]
        return rest.split(separator: " ").first.map(String.init)
    }

    var body: some View {
        // A plain button header instead of `DisclosureGroup`: inside the macOS
        // `.inspector` the group's chevron-only hit target did not expand (#320).
        VStack(alignment: .leading, spacing: 0) {
            Button {
                expanded.toggle()
            } label: {
                HStack(spacing: 6) {
                    Label("SEPA & Bank-Rohdaten", systemImage: "doc.plaintext")
                        .appFont(.subheadline, weight: .semibold)
                        .foregroundStyle(Theme.text)
                    Spacer(minLength: 0)
                    Image(systemName: "chevron.right")
                        .appFont(.caption, weight: .semibold)
                        .foregroundStyle(Theme.secondary)
                        .rotationEffect(.degrees(expanded ? 90 : 0))
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityAddTraits(.isButton)
            if expanded {
                VStack(alignment: .leading, spacing: 8) {
                    row("Verwendungszweck (roh)", booking.purpose.isEmpty ? "–" : booking.purpose)
                    if !booking.bookingText.isEmpty { row("Buchungstext", booking.bookingText) }
                    if let ref = AmazonOrderRef.parse(purpose: booking.purpose, merchantDomain: booking.merchantDomain, bookingText: booking.bookingText) {
                        VStack(alignment: .leading, spacing: 1) {
                            Text("Bestellnummer").appFont(.caption2).foregroundStyle(Theme.secondary)
                            if let url = ref.orderURL {
                                Link(ref.code, destination: url)
                                    .appFont(.caption, design: .monospaced)
                                    .foregroundStyle(Theme.accent)
                                    .help("Bestellung auf \(ref.domain) öffnen")
                            }
                        }
                        .sensitive() // order numbers identify the purchase (#301)
                        if ref.isSubscription {
                            row("Bestelltyp", "Abonnement (Spar-Abo) – wiederkehrende Abbuchung", hides: [])
                        }
                    }
                    if let e = sepa("EREF") { row("EREF (Ende-zu-Ende-Referenz)", e) }
                    if let m = sepa("MREF") { row("MREF (Mandatsreferenz)", m) }
                    if let c = sepa("CRED") { row("Gläubiger-ID", c) }
                    if !booking.purposeCode.isEmpty { row("Purpose-Code", booking.purposeCode, hides: []) }
                    if !booking.gvc.isEmpty { row("Geschäftsvorfallcode", booking.gvc, hides: []) }
                    if !booking.counterpartyIban.isEmpty { row("IBAN Gegenkonto", formatIBAN(booking.counterpartyIban)) }
                    row("Gegenpartei (roh)", booking.counterparty)
                    if !booking.valueDate.isEmpty { row("Wertstellung", DateText.medium(booking.valueDate), hides: []) }
                    row("Konto", booking.accountName)
                }
                .padding(.top, 8)
            }
        }
        .card()
    }

    /// `hides`: what the bug report screenshot covers (#301); raw data is personal.
    private func row(_ title: LocalizedStringKey, _ value: String, hides: ReportRedaction = .personal) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(title).appFont(.caption2).foregroundStyle(Theme.secondary)
            Text(value).appFont(.caption, design: .monospaced).foregroundStyle(Theme.text).textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
                .sensitive(hides)
        }
    }
}

/// Inspector for several selected bookings: count, sums and the bulk actions.
struct MultiSelectionCard: View {
    @Environment(AppModel.self) private var model

    private static func code(_ b: Booking) -> String { b.currency.isEmpty ? "EUR" : b.currency.uppercased() }

    var body: some View {
        let many = model.selectedBookings
        let income = many.filter { $0.amountCents > 0 }.reduce(Int64(0)) { $0 + $1.amountCents }
        let expense = many.filter { $0.amountCents < 0 }.reduce(Int64(0)) { $0 + $1.amountCents }
        let currencies = Array(Set(many.map(Self.code))).sorted()
        let currency = currencies.first ?? "EUR"
        VStack(alignment: .leading, spacing: 12) {
            Label("\(many.count) Buchungen ausgewählt", systemImage: "checkmark.circle")
                .appFont(.headline)
            VStack(alignment: .leading, spacing: 4) {
                if currencies.count > 1 {
                    // Never one number over several currencies (#569): one sum per currency.
                    ForEach(currencies, id: \.self) { code in
                        LabeledContent("Summe \(code)") {
                            Text(Money.format(many.filter { Self.code($0) == code }.reduce(Int64(0)) { $0 + $1.amountCents }, currency: code))
                                .sensitive(.amounts)
                        }
                    }
                } else {
                    LabeledContent("Summe") { Text(Money.format(income + expense, currency: currency)).sensitive(.amounts) }
                    if income != 0 { LabeledContent("Einnahmen") { Text(Money.format(income, currency: currency)).sensitive(.amounts) } }
                    if expense != 0 { LabeledContent("Ausgaben") { Text(Money.format(expense, currency: currency)).sensitive(.amounts) } }
                }
            }
            .appFont(.callout, monospacedDigit: true)
            MultiSelectionTags(bookings: many)
            Button {
                model.showCategorizer = true
            } label: {
                Label("Kategorisieren …", systemImage: "tag").frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
            // #725: like the single booking's "Geprüft" switch, for the whole selection
            let allChecked = !many.isEmpty && many.allSatisfy(\.checked)
            Button {
                if let first = many.first { model.setChecked(!allChecked, for: first) }
            } label: {
                Label(allChecked ? "Als neu markieren" : "Als geprüft markieren", systemImage: "checkmark.circle")
                    .frame(maxWidth: .infinity)
            }
            .controlSize(.large)
            Button("Auswahl aufheben") {
                if let id = model.selectedBookingID { model.selectBooking(id) }
            }
            Text("⌘-Klick fügt eine Buchung hinzu oder entfernt sie, ⇧-Klick wählt einen Bereich, ⌘A alle.")
                .appFont(.caption).foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .card()
    }
}
