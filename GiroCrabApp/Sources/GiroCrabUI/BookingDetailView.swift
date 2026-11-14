import SwiftUI
import GiroCrabCore

/// Column 3: permanent context & receipt inspector.
struct InspectorView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                if case .account(let id) = model.sidebarSelection, let a = model.account(for: id), a.kind == .loan {
                    LoanProgressCard(account: a)
                }
                if model.isMultiSelection {
                    MultiSelectionCard()
                } else if let b = model.selectedBooking {
                    ReceiptCard(booking: b)
                    NoteCard(booking: b).id(b.id)
                    if b.amountCents > 0 { SalaryCard(booking: b) }
                    RecurringCard(booking: b)
                    MerchantHistoryCard(booking: b)
                    CategoryGridCard(booking: b)
                    DocumentsCard(booking: b)
                    RawDataCard(booking: b)
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
            .padding(16)
        }
        .background(Theme.canvas)
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
            .popover(isPresented: $editing) {
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
                        .appFont(.title3, weight: .semibold)
                        .foregroundStyle(Theme.text)
                        .textSelection(.enabled)
                    Text("\(booking.accountName) · \(booking.paymentMethod)")
                        .appFont(.caption).foregroundStyle(Theme.secondary)
                    MerchantDomainRow(merchant: booking.merchant)
                }
                Spacer()
            }
            Text(Money.signed(booking.amountCents))
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
            if !booking.cleanPurpose.isEmpty {
                Text(booking.cleanPurpose)
                    .appFont(.callout)
                    .foregroundStyle(Theme.text)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
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
    @State private var draft = ""
    @FocusState private var focused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Label("Notiz", systemImage: "note.text").appFont(.subheadline, weight: .semibold).foregroundStyle(Theme.text)
            TextEditor(text: $draft)
                .appFont(.callout)
                .frame(minHeight: 54, maxHeight: 120)
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
        .onAppear { draft = booking.note; claimFocus() }
        .onDisappear { save() }
        .onChange(of: booking.note) { _, n in if !focused { draft = n } }
        .onChange(of: model.pendingNoteFocus) { _, _ in claimFocus() }
    }

    /// Takes the focus requested by a double-click / Return in the list for this booking.
    private func claimFocus() {
        guard model.pendingNoteFocus == booking.id else { return }
        model.pendingNoteFocus = nil
        // deferred: focus on a just-appeared TextEditor is otherwise dropped
        DispatchQueue.main.async { focused = true }
    }

    private func save() {
        if draft.trimmingCharacters(in: .whitespacesAndNewlines) != booking.note { model.updateNote(draft, for: booking) }
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
            Text("Händler-Historie").appFont(.subheadline, weight: .semibold)
            if let s = model.merchantStats, s.key == booking.merchant.key {
                HStack(spacing: 10) {
                    stat("Buchungen", "\(s.count)×")
                    stat("Summe", Money.format(s.totalCents))
                    stat("Ø Betrag", Money.format(s.averageCents))
                }
                if !s.firstDate.isEmpty {
                    Text("Seit \(DateText.medium(s.firstDate)) · zuletzt \(DateText.medium(s.lastDate))")
                        .appFont(.caption2).foregroundStyle(Theme.tertiary)
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

    private func stat(_ title: LocalizedStringKey, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title).appFont(.caption2).foregroundStyle(Theme.secondary)
            Text(value).appFont(.callout, weight: .semibold, monospacedDigit: true).foregroundStyle(Theme.text).lineLimit(1).minimumScaleFactor(0.7)
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
                Text(booking.categoryPath.isEmpty ? String(localized: "Ohne Kategorie") : booking.categoryPath)
                    .appFont(.caption).foregroundStyle(Theme.secondary).lineLimit(1)
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
                Text(title ?? category.name).appFont(.caption).lineLimit(2)
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
                    if let e = sepa("EREF") { row("EREF (Ende-zu-Ende-Referenz)", e) }
                    if let m = sepa("MREF") { row("MREF (Mandatsreferenz)", m) }
                    if let c = sepa("CRED") { row("Gläubiger-ID", c) }
                    if !booking.purposeCode.isEmpty { row("Purpose-Code", booking.purposeCode) }
                    if !booking.gvc.isEmpty { row("Geschäftsvorfallcode", booking.gvc) }
                    if !booking.counterpartyIban.isEmpty { row("IBAN Gegenkonto", formatIBAN(booking.counterpartyIban)) }
                    row("Gegenpartei (roh)", booking.counterparty)
                    if !booking.valueDate.isEmpty { row("Wertstellung", DateText.medium(booking.valueDate)) }
                    row("Konto", booking.accountName)
                }
                .padding(.top, 8)
            }
        }
        .card()
    }

    private func row(_ title: LocalizedStringKey, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(title).appFont(.caption2).foregroundStyle(Theme.secondary)
            Text(value).appFont(.caption, design: .monospaced).foregroundStyle(Theme.text).textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
        }
    }
}

/// Inspector for several selected bookings: count, sums and the bulk actions.
struct MultiSelectionCard: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let many = model.selectedBookings
        let income = many.filter { $0.amountCents > 0 }.reduce(Int64(0)) { $0 + $1.amountCents }
        let expense = many.filter { $0.amountCents < 0 }.reduce(Int64(0)) { $0 + $1.amountCents }
        VStack(alignment: .leading, spacing: 12) {
            Label("\(many.count) Buchungen ausgewählt", systemImage: "checkmark.circle")
                .appFont(.headline)
            VStack(alignment: .leading, spacing: 4) {
                LabeledContent("Summe", value: Money.format(income + expense))
                if income != 0 { LabeledContent("Einnahmen", value: Money.format(income)) }
                if expense != 0 { LabeledContent("Ausgaben", value: Money.format(expense)) }
            }
            .appFont(.callout, monospacedDigit: true)
            Button {
                model.showCategorizer = true
            } label: {
                Label("Kategorisieren …", systemImage: "tag").frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
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
