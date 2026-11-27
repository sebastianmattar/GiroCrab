#if os(macOS)
import SwiftUI

/// Ansicht › Symbolleiste anpassen … (#2049): one list that mirrors the toolbar. A checkbox shows or hides an entry,
/// dragging or the context menu (also VoiceOver actions) reorders; the „Flexibler Zwischenraum“ row splits the buttons into the group after the view filter and the
/// group next to the search. Edits the stored `ToolbarLayout`, the toolbar follows live.
/// Design page: `docs/SYMBOLLEISTE_UI.md`.
struct ToolbarCustomizeSheet: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(\.textScale) private var textScale
    @Bindable private var store = ToolbarLayoutStore.shared
    /// The list order, hidden buttons included: kept here so that unchecking a button does not move its row.
    @State private var rows = ToolbarLayoutStore.shared.layout.listRows(oneGroup: !Self.twoGroups)

    /// Before macOS 26 the toolbar has no flexible spacer: one group, no „Flexibler Zwischenraum“ row.
    private static var twoGroups: Bool {
        if #available(macOS 26, *) { return true }
        return false
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("Symbolleiste anpassen")
                .appFont(.headline)
                .padding(.horizontal, 20)
                .padding(.top, 16)
                .padding(.bottom, 6)
            // fixed in the middle of the bar: not a row of the list
            Toggle("Ansichtsfilter in der Mitte anzeigen", isOn: $store.layout.showsScope)
                .appFont(.body)
                .padding(.horizontal, 20)
                .padding(.vertical, 6)
            List {
                ForEach(rows, id: \.self) { row in
                    Group {
                        switch row {
                        case .item(let item):
                            let isShown = store.layout.shown.contains(item)
                            Toggle(isOn: shown(item)) {
                                // what is not in the bar reads as secondary (Theme.secondary keeps 4.5:1)
                                ToolbarItemLabel(item: item).foregroundStyle(isShown ? Theme.text : Theme.secondary)
                            }
                        case .flexibleSpace: FlexibleSpaceRow().listRowSeparator(.hidden)
                        }
                    }
                    .contextMenu { moveCommands(row, onlyAvailable: false) }
                    .accessibilityActions { moveCommands(row, onlyAvailable: true) }
                }
                .onMove { from, to in
                    rows.move(fromOffsets: from, toOffset: to)
                    apply(shown: store.layout.shown)
                }
            }
            .listStyle(.inset)
            .toggleStyle(.checkbox)
            Text(Self.twoGroups
                ? "Ziehen oder Kontextmenü zum Sortieren. Was unter „Flexibler Zwischenraum“ steht, erscheint neben der Suche."
                : "Ziehen oder Kontextmenü zum Sortieren.")
                .appFont(.caption)
                .foregroundStyle(Theme.secondary)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.horizontal, 20)
                .padding(.vertical, 10)
            Divider()
            HStack {
                Button("Standard wiederherstellen") {
                    store.layout = .standard
                    rows = ToolbarLayout.standard.listRows(oneGroup: !Self.twoGroups)
                }
                .disabled(store.layout == .standard)
                Spacer()
                Button("Fertig") { dismiss() }
                    .keyboardShortcut(.defaultAction)
            }
            .padding(16)
        }
        // room for every row at once (13 at most) without scrolling
        .frame(width: 400 * textScale, height: CGFloat(rows.count + 1) * 25 * textScale + 180)
    }

    /// „Nach oben“, „Nach unten“ and the side switch: in the context menu (disabled where they do nothing) and as
    /// VoiceOver actions (only those that apply).
    @ViewBuilder
    private func moveCommands(_ row: ToolbarListRow, onlyAvailable: Bool) -> some View {
        let up = rows.canMove(row, by: -1), down = rows.canMove(row, by: 1)
        if up || !onlyAvailable {
            Button("Nach oben") { step(row, by: -1) }.disabled(!up)
        }
        if down || !onlyAvailable {
            Button("Nach unten") { step(row, by: 1) }.disabled(!down)
        }
        if Self.twoGroups, case .item(let item) = row {
            Button(rows.isNextToSearch(item) ? "Nach links" : "Neben die Suche") {
                rows.switchSide(item)
                apply(shown: store.layout.shown)
            }
        }
    }

    private func step(_ row: ToolbarListRow, by step: Int) {
        rows.move(row, by: step)
        apply(shown: store.layout.shown)
    }

    private func shown(_ item: ToolbarLayout.Item) -> Binding<Bool> {
        Binding(get: { store.layout.shown.contains(item) }, set: { on in
            var shown = store.layout.shown
            if on { shown.insert(item) } else { shown.remove(item) }
            apply(shown: shown)
        })
    }

    private func apply(shown: Set<ToolbarLayout.Item>) {
        store.layout = .from(rows: rows, shown: shown, showsScope: store.layout.showsScope)
    }
}

/// The row that stands for the toolbar's flexible space: drawn as a divider, dragged like the buttons.
private struct FlexibleSpaceRow: View {
    var body: some View {
        HStack(spacing: 8) {
            line
            Label("Flexibler Zwischenraum", systemImage: "arrow.left.and.right")
                .appFont(.caption, weight: .medium)
                .foregroundStyle(Theme.secondary)
                .fixedSize()
            line
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .combine)
    }

    private var line: some View {
        Rectangle().fill(Theme.border).frame(height: 1).frame(maxWidth: .infinity)
    }
}

/// Icon and name of a toolbar button, as in the toolbar itself (`MainToolbarButtons`).
private struct ToolbarItemLabel: View {
    let item: ToolbarLayout.Item

    var body: some View {
        switch item {
        case .transfer: ToolbarRowLabel(title: "Überweisen", systemImage: "paperplane")
        case .newBooking: ToolbarRowLabel(title: "Neue Buchung", systemImage: "plus")
        case .sync: ToolbarRowLabel(title: "Abrufen", systemImage: "arrow.triangle.2.circlepath")
        case .inspector: ToolbarRowLabel(title: "Informationen", systemImage: "sidebar.trailing")
        case .privacy: ToolbarRowLabel(title: "Privatmodus", systemImage: "eye")
        case .palette: ToolbarRowLabel(title: "Befehle", systemImage: "command")
        case .export: ToolbarRowLabel(title: "Umsätze exportieren", systemImage: "square.and.arrow.up")
        case .print: ToolbarRowLabel(title: "Drucken", systemImage: "printer")
        case .rules: ToolbarRowLabel(title: "Regeln", systemImage: "wand.and.stars")
        case .lock: ToolbarRowLabel(title: "Tresor sperren", systemImage: "lock")
        case .settings: ToolbarRowLabel(title: "Einstellungen", systemImage: "gearshape")
        }
    }
}

private struct ToolbarRowLabel: View {
    let title: LocalizedStringKey
    let systemImage: String
    @Environment(\.textScale) private var textScale

    var body: some View {
        Label { Text(title) } icon: { Image(systemName: systemImage).frame(width: 22 * textScale) }
            .appFont(.body)
    }
}

/// Ansicht › Symbolleiste: replaces the system's `ToolbarCommands`, whose „Symbolleiste anpassen …“ is the
/// customization SwiftUI gets wrong (#2049).
struct MainToolbarCommands: Commands {
    let model: AppModel

    var body: some Commands {
        CommandGroup(replacing: .toolbar) {
            let store = ToolbarLayoutStore.shared
            Button(store.isToolbarShown ? "Symbolleiste ausblenden" : "Symbolleiste einblenden") {
                guard let window = NSApp.mainWindow, let toolbar = window.toolbar else { return }
                window.toggleToolbarShown(nil)
                store.isToolbarShown = toolbar.isVisible
            }
            .keyboardShortcut("t", modifiers: [.command, .option])
            Button("Symbolleiste anpassen …") { store.isCustomizing = true }
                .disabled(!model.isUnlocked)
        }
    }
}
#endif
