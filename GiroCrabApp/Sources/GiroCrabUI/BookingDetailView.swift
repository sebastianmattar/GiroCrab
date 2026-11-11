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
                if let b = model.selectedBooking {
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
                            .font(.headline)
                        Text("Wählen Sie eine Buchung, um Beleg, Händler-Historie und Kategorie zu sehen.")
                            .font(.callout).foregroundStyle(Theme.secondary)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .card()
                }
                ShortcutHintCard()
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
                } label: { Label("Text bearbeiten", systemImage: "pencil") }
                    .buttonStyle(.borderless).controlSize(.small).font(.caption)
                    .help("Gegenpartei und Verwendungszweck korrigieren")
            }
            .popover(isPresented: $editing) {
                VStack(alignment: .leading, spacing: 8) {
                    Text("Gegenpartei").font(.caption).foregroundStyle(Theme.secondary)
                    TextField("Gegenpartei", text: $draftName).textFieldStyle(.roundedBorder)
                    Text("Verwendungszweck").font(.caption).foregroundStyle(Theme.secondary)
                    TextEditor(text: $draftPurpose)
                        .font(.callout).frame(height: 90)
                        .overlay(RoundedRectangle(cornerRadius: 5).stroke(Theme.secondary.opacity(0.3)))
                    HStack {
                        Spacer()
                        Button("Abbrechen") { editing = false }
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
                        .font(.title3.weight(.semibold))
                        .foregroundStyle(Theme.text)
                        .textSelection(.enabled)
                    Text("\(booking.accountName) · \(booking.paymentMethod)")
                        .font(.caption).foregroundStyle(Theme.secondary)
                    MerchantDomainRow(merchant: booking.merchant)
                }
                Spacer()
            }
            Text(Money.signed(booking.amountCents))
                .font(.system(size: 30, weight: .semibold, design: .rounded).monospacedDigit())
                .foregroundStyle(Money.ink(booking.amountCents))
            HStack(spacing: 6) {
                Image(systemName: "calendar").foregroundStyle(Theme.tertiary)
                Text(DateText.long(booking.bookingDate))
                Spacer()
                FlowBadge(flow: booking.flow)
            }
            .font(.caption)
            .foregroundStyle(Theme.secondary)
            if !booking.cleanPurpose.isEmpty {
                Text(booking.cleanPurpose)
                    .font(.callout)
                    .foregroundStyle(Theme.text)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Toggle("Geprüft", isOn: Binding(get: { booking.checked }, set: { model.setChecked($0, for: booking) }))
                .toggleStyle(.switch)
                .controlSize(.small)
                .font(.caption)
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
            Label("Notiz", systemImage: "note.text").font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
            TextEditor(text: $draft)
                .font(.callout)
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
                    .font(.caption)
                    .frame(maxWidth: 180)
                    .autocorrectionDisabled()
                    .onSubmit(save)
                Button("Logo laden", action: save).controlSize(.small)
                if merchant.domainSource == "override" {
                    Button { model.setMerchantDomain(merchant, nil); editing = false } label: { Image(systemName: "arrow.uturn.backward") }
                        .buttonStyle(.borderless).help("Automatische Erkennung wiederherstellen")
                }
                Button { editing = false } label: { Image(systemName: "xmark") }.buttonStyle(.borderless)
            }
        } else {
            HStack(spacing: 4) {
                Image(systemName: "globe").font(.system(size: 9))
                Text(merchant.domain.isEmpty ? (merchant.isPerson ? String(localized: "Privatperson – kein Logo") : String(localized: "Domain festlegen")) : merchant.domain)
                    .font(.caption.monospaced())
                if !merchant.domain.isEmpty {
                    Text("· \(sourceLabel)").font(.caption2)
                }
                Button {
                    text = merchant.domain
                    editing = true
                } label: { Image(systemName: "pencil").font(.system(size: 10)) }
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
                .font(.subheadline.weight(.semibold))
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
                .font(.caption)
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
            .font(.caption2.weight(.semibold))
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
            Text("Händler-Historie").font(.subheadline.weight(.semibold))
            if let s = model.merchantStats, s.key == booking.merchant.key {
                HStack(spacing: 10) {
                    stat("Buchungen", "\(s.count)×")
                    stat("Summe", Money.format(s.totalCents))
                    stat("Ø Betrag", Money.format(s.averageCents))
                }
                if !s.firstDate.isEmpty {
                    Text("Seit \(DateText.medium(s.firstDate)) · zuletzt \(DateText.medium(s.lastDate))")
                        .font(.caption2).foregroundStyle(Theme.tertiary)
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
            Text(title).font(.caption2).foregroundStyle(Theme.secondary)
            Text(value).font(.callout.weight(.semibold).monospacedDigit()).foregroundStyle(Theme.text).lineLimit(1).minimumScaleFactor(0.7)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func labeled(_ title: LocalizedStringKey, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(title).font(.caption2).foregroundStyle(Theme.secondary)
            Text(value).font(.caption.monospaced()).textSelection(.enabled)
        }
    }
}

/// 1-click category grid: top-level categories, then the sub-categories of
/// the chosen one; plus "Regel für diesen Händler erstellen".
struct CategoryGridCard: View {
    @Environment(AppModel.self) private var model
    let booking: Booking
    @State private var openRoot: Int64?

    private var currentRoot: Int64? {
        guard let cid = booking.categoryId, let c = model.categories.first(where: { $0.id == cid }) else { return nil }
        return c.parentId ?? c.id
    }

    var body: some View {
        let root = openRoot ?? currentRoot
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("Kategorie").font(.subheadline.weight(.semibold))
                Spacer()
                Text(booking.categoryPath.isEmpty ? String(localized: "Ohne Kategorie") : booking.categoryPath)
                    .font(.caption).foregroundStyle(Theme.secondary).lineLimit(1)
            }
            LazyVGrid(columns: [GridItem(.adaptive(minimum: 104), spacing: 6)], spacing: 6) {
                ForEach(model.categoryTree) { node in
                    CategoryChip(category: node.category, selected: node.id == root) {
                        if node.children == nil { model.setCategory(node.id, for: booking) } else { openRoot = node.id }
                    }
                }
            }
            if let root, let node = model.categoryTree.first(where: { $0.id == root }), let kids = node.children {
                Divider()
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 120), spacing: 6)], spacing: 6) {
                    ForEach(kids) { kid in
                        CategoryChip(category: kid.category, selected: kid.id == booking.categoryId, filled: true) {
                            model.setCategory(kid.id, for: booking)
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
                Label("Regel für diesen Händler erstellen", systemImage: "wand.and.stars")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .tint(Theme.accent)
            .controlSize(.large)
            Text("Tastatur: C kategorisieren · R Regel erstellen")
                .font(.caption2).foregroundStyle(Theme.tertiary)
        }
        .card()
        .onChange(of: booking.id) { _, _ in openRoot = nil }
    }
}

struct CategoryChip: View {
    let category: GiroCrabCore.Category
    var selected: Bool
    var filled = false
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 5) {
                Image(systemName: category.sfSymbol).font(.system(size: 11))
                Text(category.name).font(.caption).lineLimit(1).minimumScaleFactor(0.8)
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 8).padding(.vertical, 6)
            .foregroundStyle(selected ? Theme.accent : Theme.text)
            .background(RoundedRectangle(cornerRadius: 7, style: .continuous).fill(selected ? Theme.selection : (filled ? Theme.canvas : Theme.card)))
            .overlay(RoundedRectangle(cornerRadius: 7, style: .continuous).strokeBorder(selected ? Theme.accent.opacity(0.5) : Theme.border, lineWidth: 1))
        }
        .buttonStyle(.plain)
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
        DisclosureGroup(isExpanded: $expanded) {
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
        } label: {
            Label("SEPA & Bank-Rohdaten", systemImage: "doc.plaintext")
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(Theme.text)
        }
        .card()
    }

    private func row(_ title: LocalizedStringKey, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(title).font(.caption2).foregroundStyle(Theme.secondary)
            Text(value).font(.caption.monospaced()).foregroundStyle(Theme.text).textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
        }
    }
}

struct ShortcutHintCard: View {
    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Tastatur").font(.caption.weight(.semibold)).foregroundStyle(Theme.secondary)
            hint("⌘K", "Befehlspalette")
            hint("↑ ↓", "Buchung wechseln")
            hint("↩", "Notiz bearbeiten (oder Doppelklick)")
            hint("C", "Kategorisieren")
            hint("R", "Regel erstellen")
            hint("⌘1–3", "Alle · Alltag · Sparpläne")
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(12)
        .background(RoundedRectangle(cornerRadius: 10).strokeBorder(Theme.border, style: StrokeStyle(lineWidth: 1, dash: [3, 3])))
    }

    private func hint(_ key: String, _ text: LocalizedStringKey) -> some View {
        HStack {
            Text(key).font(.caption.monospaced().weight(.semibold))
                .padding(.horizontal, 5).padding(.vertical, 1)
                .background(RoundedRectangle(cornerRadius: 4).fill(Theme.card))
                .overlay(RoundedRectangle(cornerRadius: 4).strokeBorder(Theme.border))
            Text(text).font(.caption).foregroundStyle(Theme.secondary)
        }
    }
}
