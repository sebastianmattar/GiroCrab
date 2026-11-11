import SwiftUI
import GiroCrabCore

/// ⌘K command palette: views, actions, accounts, categories, merchants.
struct CommandPaletteView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    #if os(macOS)
    @Environment(\.openSettings) private var openSettings
    #endif
    @State private var query = ""
    @State private var index = 0
    @FocusState private var focused: Bool

    struct Item: Identifiable {
        let id = UUID()
        let title: String
        let subtitle: String
        let symbol: String
        let action: () -> Void
    }

    private var items: [Item] {
        var out: [Item] = []
        let action = String(localized: "Aktion")
        for s in BookingScope.ordered {
            out.append(Item(title: s.title, subtitle: String(localized: "Ansicht"), symbol: "rectangle.split.3x1") { model.scope = s })
        }
        if model.canTransfer {
            out.append(Item(title: String(localized: "Überweisung senden"), subtitle: "\(action) · ⌘N", symbol: "paperplane") { model.openTransfer() })
        }
        out.append(Item(title: String(localized: "Alle Konten abrufen"), subtitle: "\(action) · ⇧⌘R", symbol: "arrow.triangle.2.circlepath") { model.requestSyncAll() })
        out.append(Item(title: String(localized: "Regeln bearbeiten"), subtitle: action, symbol: "wand.and.stars") { model.openRuleEditor() })
        out.append(Item(title: String(localized: "Wiederkehrende Buchungen"), subtitle: "\(action) · ⇧⌘F", symbol: "repeat") { model.showRecurringSheet = true })
        out.append(Item(title: String(localized: "Zeitraum: Seit letztem Gehalt"), subtitle: String(localized: "Ansicht"), symbol: "banknote") { model.period = .sinceSalary })
        if let b = model.selectedBooking {
            out.append(Item(title: String(localized: "„\(b.merchant.name)“ kategorisieren"), subtitle: "\(action) · C", symbol: "tag") { model.showCategorizer = true })
            out.append(Item(title: String(localized: "Regel für „\(b.merchant.name)“ erstellen"), subtitle: "\(action) · R", symbol: "wand.and.stars") { model.openRuleEditor(from: b) })
        }
        out.append(Item(title: String(localized: "MoneyMoney Historie & Kategorien importieren"), subtitle: action, symbol: "square.and.arrow.down.on.square") { model.openMoneyMoneyImport() })
        out.append(Item(title: String(localized: "Regeln jetzt anwenden"), subtitle: action, symbol: "play") { model.applyAllRules() })
        #if os(macOS)
        out.append(Item(title: String(localized: "Einstellungen"), subtitle: "\(action) · ⌘,", symbol: "gearshape") { openSettings() })
        #else
        out.append(Item(title: String(localized: "Einstellungen"), subtitle: action, symbol: "gearshape") { model.showSettings = true })
        #endif
        out.append(Item(title: String(localized: "Übersicht"), subtitle: String(localized: "Konten"), symbol: "square.grid.2x2") { model.sidebarSelection = .allAccounts })
        out.append(Item(title: String(localized: "Neue Kontogruppe"), subtitle: "\(action) · ⇧⌘N", symbol: "folder.badge.plus") { model.groupDialog = .create() })
        out.append(Item(title: String(localized: "Kontogruppen bearbeiten"), subtitle: action, symbol: "slider.horizontal.3") { model.openGroupSettings() })
        for m in AccountSidebarMode.allCases where m != model.accountView {
            out.append(Item(title: m == .byGroup ? String(localized: "Konten nach Gruppe anzeigen") : String(localized: "Konten nach Kontoart anzeigen"), subtitle: String(localized: "Ansicht"), symbol: m.symbol) { model.accountView = m })
        }
        for g in model.namedUserGroups {
            out.append(Item(title: g.name, subtitle: String(localized: "Gruppe · \(accountCountText(g.accounts.count))"), symbol: g.sfSymbol) {
                model.accountView = .byGroup
                model.sidebarSelection = .userGroup(g.uuid)
            })
        }
        if case .account(let id) = model.sidebarSelection, let a = model.account(for: id) {
            for g in model.namedUserGroups where g.uuid != a.userGroupUuid {
                out.append(Item(title: String(localized: "„\(a.name)“ nach „\(g.name)“ verschieben"), subtitle: action, symbol: "folder") { model.moveAccount(a.id, to: g.uuid) })
            }
        }
        for a in model.allAccounts {
            out.append(Item(title: a.name, subtitle: String(localized: "Konto · \(a.bankName)"), symbol: a.sfSymbol) { model.sidebarSelection = .account(a.id) })
        }
        for c in model.categories {
            out.append(Item(title: c.name, subtitle: String(localized: "Kategorie · \(c.path)"), symbol: c.sfSymbol) { model.sidebarSelection = .category(c.id) })
        }
        var seen = Set<String>()
        for b in model.bookings where seen.insert(b.merchant.key).inserted && seen.count < 300 {
            let name = b.merchant.name
            out.append(Item(title: name, subtitle: String(localized: "Händler suchen"), symbol: "magnifyingglass") { model.searchText = name })
        }
        let q = query.trimmingCharacters(in: .whitespaces).lowercased()
        guard !q.isEmpty else { return Array(out.prefix(14)) }
        return out.filter { $0.title.lowercased().contains(q) || $0.subtitle.lowercased().contains(q) }.prefix(14).map { $0 }
    }

    var body: some View {
        let list = items
        VStack(spacing: 0) {
            HStack(spacing: 10) {
                Image(systemName: "command").foregroundStyle(Theme.secondary)
                TextField("Befehl, Konto, Kategorie oder Händler …", text: $query)
                    .textFieldStyle(.plain)
                    .font(.title3)
                    .focused($focused)
                    .onSubmit { run(list) }
                    .onKeyPress(.downArrow) { index = min(index + 1, max(list.count - 1, 0)); return .handled }
                    .onKeyPress(.upArrow) { index = max(index - 1, 0); return .handled }
                    .onKeyPress(.escape) { dismiss(); return .handled }
            }
            .padding(14)
            Divider()
            ScrollView {
                VStack(spacing: 2) {
                    ForEach(Array(list.enumerated()), id: \.element.id) { i, item in
                        HStack(spacing: 10) {
                            Image(systemName: item.symbol).frame(width: 20).foregroundStyle(i == index ? Theme.accent : Theme.secondary)
                            Text(item.title).foregroundStyle(Theme.text)
                            Spacer()
                            Text(item.subtitle).font(.caption).foregroundStyle(Theme.secondary)
                        }
                        .padding(.horizontal, 12).padding(.vertical, 7)
                        .background(RoundedRectangle(cornerRadius: 7).fill(i == index ? Theme.selection : Color.clear))
                        .contentShape(Rectangle())
                        .onTapGesture { index = i; run(list) }
                    }
                }
                .padding(8)
            }
            .frame(height: 380)
        }
        .macFrame(width: 560)
        .background(Theme.card)
        .onAppear { focused = true }
        .onChange(of: query) { _, _ in index = 0 }
    }

    private func run(_ list: [Item]) {
        guard list.indices.contains(index) else { return }
        let action = list[index].action
        dismiss()
        DispatchQueue.main.async { action() }
    }
}

/// "C": pick a category for the selected booking (type to filter).
struct CategorizerSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var query = ""
    @FocusState private var focused: Bool

    var body: some View {
        let leaves = model.categories.filter { c in c.depth > 0 || !model.categories.contains(where: { $0.parentId == c.id }) }
        let q = query.lowercased()
        let shown = q.isEmpty ? leaves : leaves.filter { $0.path.lowercased().contains(q) }
        VStack(alignment: .leading, spacing: 12) {
            if let b = model.selectedBooking {
                HStack(spacing: 10) {
                    MerchantSignet(merchant: b.merchant, categorySymbol: b.categorySymbol)
                    VStack(alignment: .leading) {
                        Text(b.merchant.name).font(.headline)
                        Text(Money.format(b.amountCents)).font(.caption.monospacedDigit()).foregroundStyle(Theme.secondary)
                    }
                    Spacer()
                }
            }
            TextField("Kategorie suchen …", text: $query)
                .textFieldStyle(.roundedBorder)
                .focused($focused)
                .onSubmit { if let first = shown.first { pick(first.id) } }
            ScrollView {
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 150), spacing: 6)], spacing: 6) {
                    ForEach(shown) { c in
                        CategoryChip(category: c, selected: c.id == model.selectedBooking?.categoryId) { pick(c.id) }
                            .help(c.path)
                    }
                }
            }
            .frame(height: 300)
            HStack {
                Button("Kategorie entfernen") { pick(nil) }
                Spacer()
                Button("Abbrechen", role: .cancel) { dismiss() }.keyboardShortcut(.cancelAction)
            }
        }
        .padding(20)
        .macFrame(width: 560)
        .onAppear { focused = true }
    }

    private func pick(_ id: Int64?) {
        if let b = model.selectedBooking { model.setCategory(id, for: b) }
        dismiss()
    }
}
