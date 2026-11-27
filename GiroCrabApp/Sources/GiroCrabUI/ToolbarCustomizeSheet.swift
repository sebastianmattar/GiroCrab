#if os(macOS)
import SwiftUI

/// Ansicht › Symbolleiste anpassen … (#2049): edits the stored `ToolbarLayout`, the toolbar follows live.
/// Design page: `docs/SYMBOLLEISTE_UI.md`.
struct ToolbarCustomizeSheet: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(\.textScale) private var textScale
    @Bindable private var store = ToolbarLayoutStore.shared

    /// Before macOS 26 the toolbar has no flexible spacer, so left and right look the same: one list.
    private var twoGroups: Bool {
        if #available(macOS 26, *) { return true }
        return false
    }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    Toggle("Ansichtsfilter", isOn: $store.layout.showsScope)
                } footer: {
                    Text("Der Filter der gewählten Ansicht in der Mitte der Symbolleiste.").leadingFooter()
                }
                if twoGroups {
                    group(.leading, title: "Links")
                    group(.trailing, title: "Rechts, neben der Suche")
                } else {
                    Section {
                        ForEach(store.layout.leading + store.layout.trailing, id: \.self) { row($0, in: nil) }
                            .onMove { store.layout.moveAsOneGroup(from: $0, to: $1) }
                    } header: { Text("Symbole").appSectionHeader() }
                }
                Section {
                    ForEach(store.layout.hidden, id: \.self) { item in
                        HStack {
                            ToolbarItemLabel(item: item)
                            Spacer()
                            if twoGroups {
                                Menu {
                                    Button("Links hinzufügen") { store.layout.add(item, to: .leading) }
                                    Button("Rechts hinzufügen") { store.layout.add(item, to: .trailing) }
                                } label: { Label("Hinzufügen", systemImage: "plus.circle").labelStyle(.iconOnly) }
                                    .menuStyle(.borderlessButton)
                                    .menuIndicator(.hidden)
                                    .fixedSize()
                                    .help("Hinzufügen")
                            } else {
                                rowButton("Hinzufügen", systemImage: "plus.circle") { store.layout.add(item, to: .trailing) }
                            }
                        }
                    }
                } header: { Text("Weitere Symbole").appSectionHeader() }
            }
            .formStyle(.grouped)
            .navigationTitle("Symbolleiste anpassen")
            .toolbar {
                ToolbarItem(placement: .destructiveAction) {
                    Button("Standard wiederherstellen") { store.layout = .standard }
                        .disabled(store.layout == .standard)
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Fertig") { dismiss() }
                        .keyboardShortcut(.defaultAction)
                }
            }
        }
        .desktopFrame(minWidth: 420 * textScale, idealWidth: 460 * textScale, minHeight: 520, idealHeight: 720)
    }

    private func group(_ group: ToolbarLayout.Group, title: LocalizedStringKey) -> some View {
        Section {
            ForEach(store.layout[group], id: \.self) { row($0, in: group) }
                .onMove { store.layout.move(in: group, from: $0, to: $1) }
            if store.layout[group].isEmpty {
                Text("Keine Symbole").appFont(.callout).foregroundStyle(Theme.secondary)
            }
        } header: { Text(title).appSectionHeader() }
    }

    /// A shown button: drag to reorder, move to the other side (two groups only), remove.
    private func row(_ item: ToolbarLayout.Item, in group: ToolbarLayout.Group?) -> some View {
        HStack {
            ToolbarItemLabel(item: item)
            Spacer()
            if let group {
                let toRight = group == .leading
                rowButton(toRight ? "Nach rechts" : "Nach links", systemImage: toRight ? "arrow.right.circle" : "arrow.left.circle") {
                    store.layout.add(item, to: toRight ? .trailing : .leading)
                }
            }
            rowButton("Entfernen", systemImage: "minus.circle") { store.layout.remove(item) }
        }
    }

    private func rowButton(_ title: LocalizedStringKey, systemImage: String, action: @escaping () -> Void) -> some View {
        Button(action: action) { Label(title, systemImage: systemImage).labelStyle(.iconOnly) }
            .buttonStyle(.borderless)
            .help(Text(title))
    }
}

/// Icon and name of a toolbar button, as in the toolbar itself (`MainToolbarButtons`).
private struct ToolbarItemLabel: View {
    let item: ToolbarLayout.Item
    @Environment(\.textScale) private var textScale

    var body: some View {
        Label { Text(title) } icon: { Image(systemName: systemImage).frame(width: 22 * textScale) }
            .appFont(.body)
    }

    private var title: LocalizedStringKey {
        switch item {
        case .transfer: "Überweisen"
        case .newBooking: "Neue Buchung"
        case .sync: "Abrufen"
        case .inspector: "Informationen"
        case .privacy: "Privatmodus"
        case .palette: "Befehle"
        case .export: "Umsätze exportieren"
        case .print: "Drucken"
        case .rules: "Regeln"
        case .lock: "Tresor sperren"
        case .settings: "Einstellungen"
        }
    }

    private var systemImage: String {
        switch item {
        case .transfer: "paperplane"
        case .newBooking: "plus"
        case .sync: "arrow.triangle.2.circlepath"
        case .inspector: "sidebar.trailing"
        case .privacy: "eye"
        case .palette: "command"
        case .export: "square.and.arrow.up"
        case .print: "printer"
        case .rules: "wand.and.stars"
        case .lock: "lock"
        case .settings: "gearshape"
        }
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
