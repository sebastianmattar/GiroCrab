#if os(macOS)
import SwiftUI
import AppKit
import GiroCrabCore

/// The GiroCrab icon at menu bar size (the app icon, scaled; not a template, so
/// the crab keeps its colours). Nil only if the system has no icon to give.
enum MenuBarIcon {
    @MainActor static func image(size: CGFloat = 18) -> NSImage? {
        guard let source = NSImage(named: "AppIcon") ?? NSApp?.applicationIconImage else { return nil }
        let image = NSImage(size: NSSize(width: size, height: size), flipped: false) { rect in
            source.draw(in: rect)
            return true
        }
        image.isTemplate = false
        image.accessibilityDescription = "GiroCrab"
        return image
    }
}

/// Windows, actions and the right-click menu of the menu bar item (GitHub #546).
@MainActor
public enum MenuBarActions {
    /// Set by the label and the popover, which have SwiftUI's `openWindow`.
    static var openWindowByID: ((String) -> Void)?
    /// Set by the label; the right-click menu reads state and runs actions on it.
    static weak var model: AppModel?
    /// `NSMenuItem.target` is weak: the targets of the menu on screen live here.
    private static var retained: [MenuAction] = []

    /// SwiftUI's `openSettings`, set by the label and the popover.
    static var openSettingsAction: (() -> Void)?

    /// A window that is already open, so it is brought forward instead of a
    /// second one being created (`openWindow` always opens a new window of a
    /// `WindowGroup`). "main" is every titled window that is not settings, help
    /// or the bug report.
    static func existingWindow(_ id: String) -> NSWindow? {
        let others = ["settings", "help", "bug-report"]
        return NSApp.windows.first { w in
            guard w.styleMask.contains(.titled), w.isVisible || w.isMiniaturized else { return false }
            let identifier = w.identifier?.rawValue.lowercased() ?? ""
            return id == "main" ? !others.contains { identifier.contains($0) } : identifier.contains(id)
        }
    }

    /// Brings a window back; without any window the app has no Dock icon
    /// (accessory), so it becomes a regular app first.
    static func show(window id: String) {
        NSApp.setActivationPolicy(.regular)
        if let window = existingWindow(id) {
            if window.isMiniaturized { window.deminiaturize(nil) }
            window.makeKeyAndOrderFront(nil)
        } else {
            openWindowByID?(id)
        }
        NSApp.activate()
    }

    public static func showMain() { show(window: "main") }

    static func showSettings() {
        NSApp.setActivationPolicy(.regular)
        NSApp.activate()
        if let openSettingsAction {
            openSettingsAction()
        } else {
            NSApp.sendAction(Selector(("showSettingsWindow:")), to: nil, from: nil)
        }
    }

    /// "Aktualisieren": fetches all logins. Stays in the background, unless a
    /// PIN, TAN or set-up question needs the main window.
    static func refresh() {
        guard let model, model.isUnlocked else { return }
        model.requestSyncAll()
        Task { @MainActor in
            try? await Task.sleep(for: .seconds(1.5))
            if model.pendingLogin != nil || model.activeChallenge != nil
                || model.setupRequest != nil || model.webLoginConnection != nil {
                showMain()
            }
        }
    }

    // MARK: right-click menu

    /// Right-click (or ctrl-click) on the menu bar icon opens this menu; a left
    /// click opens the popover.
    static func buildMenu() -> NSMenu {
        retained = []
        let unlocked = model?.isUnlocked ?? false
        let canLock = unlocked && (model?.holdsRealData ?? false)
        let privateMode = AppDefaults.store.bool(forKey: MenuBarSummary.privateKey)
        let menu = NSMenu()
        menu.autoenablesItems = false
        menu.addItem(item(String(localized: "GiroCrab öffnen")) { showMain() })
        menu.addItem(item(String(localized: "Aktualisieren"), key: "r", enabled: unlocked) { refresh() })
        menu.addItem(item(String(localized: "Neue Überweisung …"), key: "n", enabled: unlocked && (model?.canTransfer ?? false)) {
            model?.openTransfer()
            showMain()
        })
        menu.addItem(.separator())
        menu.addItem(item(String(localized: "Beträge in der Menüleiste verdecken"), checked: privateMode) {
            AppDefaults.store.set(!privateMode, forKey: MenuBarSummary.privateKey)
        })
        if unlocked {
            menu.addItem(item(String(localized: "Tresor sperren"), key: "l", enabled: canLock) { model?.lock() })
        } else {
            menu.addItem(item(String(localized: "Tresor entsperren"), key: "l") { model?.unlock() })
        }
        menu.addItem(.separator())
        menu.addItem(submenu(String(localized: "Einstellungen"), settingsItems()))
        menu.addItem(submenu(String(localized: "Support"), [
            item(String(localized: "GiroCrab-Hilfe")) { show(window: "help") },
            item(String(localized: "Problem melden …")) { show(window: "bug-report") },
        ]))
        menu.addItem(.separator())
        menu.addItem(item(String(localized: "GiroCrab beenden"), key: "q") { NSApp.terminate(nil) })
        return menu
    }

    private static func settingsItems() -> [NSMenuItem] {
        let defaults = AppDefaults.store
        let display = MenuBarSummary.BarDisplay(rawValue: defaults.string(forKey: MenuBarSummary.barDisplayKey) ?? "") ?? .amount
        let hours = (defaults.object(forKey: AutoSync.hoursKey) as? Int) ?? model?.autoSyncHours ?? 0
        let intervals = AutoSync.hourOptions.map { h in
            item(AutoSync.label(h), checked: h == hours) { defaults.set(h, forKey: AutoSync.hoursKey) }
        }
        return [
            item(String(localized: "Betrag in der Menüleiste"), checked: display == .amount) {
                defaults.set((display == .amount ? MenuBarSummary.BarDisplay.iconOnly : .amount).rawValue,
                             forKey: MenuBarSummary.barDisplayKey)
            },
            submenu(String(localized: "Automatischer Abruf"), intervals),
            .separator(),
            item(String(localized: "Alle Einstellungen …"), key: ",") { showSettings() },
        ]
    }

    private static func item(_ title: String, key: String = "", checked: Bool = false, enabled: Bool = true,
                             _ run: @escaping @MainActor () -> Void) -> NSMenuItem {
        let action = MenuAction(run)
        retained.append(action)
        let item = NSMenuItem(title: title, action: #selector(MenuAction.fire), keyEquivalent: key)
        item.keyEquivalentModifierMask = .command
        item.target = action
        item.state = checked ? .on : .off
        item.isEnabled = enabled
        return item
    }

    private static func submenu(_ title: String, _ items: [NSMenuItem]) -> NSMenuItem {
        let parent = NSMenuItem(title: title, action: nil, keyEquivalent: "")
        let menu = NSMenu(title: title)
        menu.autoenablesItems = false
        items.forEach(menu.addItem)
        parent.submenu = menu
        return parent
    }
}

@MainActor
final class MenuAction: NSObject {
    let run: @MainActor () -> Void
    init(_ run: @escaping @MainActor () -> Void) { self.run = run }
    @objc func fire() { run() }
}

/// Title of the optional menu bar item: the crab icon plus, if chosen, the
/// balance of the focused account or the sum. Locked vault: the crab alone,
/// dimmed; private mode: "•••". Never an amount or a name in those two cases.
public struct MenuBarLabel: View {
    @Environment(AppModel.self) private var model
    @AppStorage(MenuBarSummary.privateKey) private var privateMode = false
    @AppStorage(MenuBarSummary.accountsKey) private var accountsRaw = ""
    @AppStorage(MenuBarSummary.focusKey) private var focus = MenuBarSummary.totalFocus
    @AppStorage(MenuBarSummary.barDisplayKey) private var display = MenuBarSummary.BarDisplay.amount

    public init() {}

    public var body: some View {
        let visibility = MenuBarSummary.visibility(isUnlocked: model.isUnlocked, privateMode: privateMode)
        let selected = MenuBarSummary.selectedAccounts(model.allAccounts, raw: accountsRaw)
        let title = MenuBarSummary.title(visibility, selected: selected, focus: focus, display: display)
        let focused = MenuBarSummary.focusedAccount(in: selected, focus: focus)
        // red only where an amount is shown at all
        let negative = visibility == .visible && !title.isEmpty
            && (focused.map(MenuBarSummary.isNegative) ?? MenuBarSummary.totalIsNegative(of: selected))
        HStack(spacing: 4) {
            Group {
                if let icon = MenuBarIcon.image() {
                    Image(nsImage: icon)
                } else {
                    Image(systemName: "building.columns")
                }
            }
            .opacity(visibility == .locked ? 0.5 : 1)
            if !title.isEmpty {
                Text(title).appFont(.body, monospacedDigit: true)
                    .foregroundStyle(negative ? Theme.negative : Color.primary)
                    .sensitive()
            }
        }
        .accessibilityLabel("GiroCrab")
    }
}

/// Popover of the menu bar item: one account (or the sum) at a time, large,
/// with the time of its last fetch and a quick switch (arrows, or the menu that
/// lists every account with its balance). Locked or private: nothing personal.
public struct MenuBarPopover: View {
    @Environment(AppModel.self) private var model
    @Environment(\.textScale) private var textScale
    @AppStorage(MenuBarSummary.privateKey) private var privateMode = false
    @AppStorage(MenuBarSummary.accountsKey) private var accountsRaw = ""
    @AppStorage(MenuBarSummary.focusKey) private var focus = MenuBarSummary.totalFocus
    /// -1 = never set (the former hidden setting may still apply); read here so the popover redraws on a change.
    @AppStorage(AutoSync.hoursKey) private var storedHours = -1
    /// The list of all chosen accounts with their balances is unfolded.
    @State private var showList = false

    public init() {}

    public var body: some View {
        let visibility = MenuBarSummary.visibility(isUnlocked: model.isUnlocked, privateMode: privateMode)
        let selected = MenuBarSummary.selectedAccounts(model.allAccounts, raw: accountsRaw)
        let notices = model.autoSyncStatus.notice(visibility, intervalOn: storedHours >= 0 ? storedHours > 0 : model.autoSyncHours > 0)
        let listing = showList && visibility == .visible && !selected.isEmpty
        // Either the overview or the list; the panel follows the height and
        // keeps its top edge under the menu bar.
        Group {
            if listing {
                VStack(alignment: .leading, spacing: 10) { accountList(selected) }
            } else {
                overview(visibility, selected, notices)
            }
        }
        .padding(14)
        .frame(width: 300 * textScale, alignment: .leading)
    }

    private func overview(_ visibility: MenuBarSummary.Visibility, _ selected: [Account], _ notices: [String]) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            switch visibility {
            case .locked:
                Label("Tresor gesperrt", systemImage: "lock.fill").appFont(.headline)
                Button("Tresor entsperren") { model.unlock() }
                    .appFont(.callout)
                    .disabled(model.vaultState == .unlocking)
            case .covered:
                Label("Beträge in der Menüleiste verdeckt", systemImage: "eye.slash").appFont(.headline)
                Text("Beträge und Kontonamen sind verdeckt.")
                    .appFont(.caption).foregroundStyle(.secondary)
            case .visible:
                if selected.isEmpty {
                    Text("Keine Konten ausgewählt").appFont(.headline)
                    Text("Die Konten für die Menüleiste wählst du unter Einstellungen › Allgemein.")
                        .appFont(.caption).foregroundStyle(.secondary)
                } else {
                    current(selected)
                }
            }
            // Hints and errors of the automatic fetch: marked like the amounts, and
            // without detail while locked or private (see `AutoSyncStatus.notice`).
            ForEach(notices, id: \.self) { line in
                Text(line).appFont(.caption).foregroundStyle(.secondary).sensitive()
            }
            if visibility != .locked {
                Divider()
                Toggle("Beträge in der Menüleiste verdecken", isOn: $privateMode).appFont(.callout)
            }
        }
    }

    /// Double click on the amount: GiroCrab opens on that account (the sum: all accounts).
    private func openInGiroCrab(_ account: Account?) {
        model.interactiveSidebarSelection = account.map { .account($0.id) } ?? .allAccounts
        MenuBarController.shared.hidePopover()
        MenuBarActions.showMain()
    }

    /// One row of the unfolded list: name left, amount right-aligned, red below zero.
    private func row(_ title: String, _ amount: String, negative: Bool, focused: Bool, sensitiveTitle: Bool,
                     action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack(spacing: 8) {
                Image(systemName: "checkmark")
                    .appFont(.caption, weight: .semibold)
                    .opacity(focused ? 1 : 0)
                    .accessibilityHidden(true)
                if sensitiveTitle {
                    Text(title).appFont(.callout).lineLimit(1).truncationMode(.tail).sensitive()
                } else {
                    Text(title).appFont(.callout)
                }
                Spacer(minLength: 8)
                Text(amount)
                    .appFont(.callout, monospacedDigit: true)
                    .foregroundStyle(negative ? Theme.negative : Color.primary)
                    .fixedSize()
                    .sensitive()
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(focused ? .isSelected : [])
    }

    /// Replaces the whole popover while open: the sum and every chosen account
    /// with its balance, one under the other. A click picks one and goes back.
    @ViewBuilder
    private func accountList(_ selected: [Account]) -> some View {
        let focused = MenuBarSummary.focusedAccount(in: selected, focus: focus)
        HStack {
            Text("Alle Konten").appFont(.headline)
            Spacer()
            Button { showList = false } label: {
                Image(systemName: "xmark.circle.fill").appFont(.callout).foregroundStyle(.secondary)
            }
            .buttonStyle(.borderless)
            .help("Schließen")
            .accessibilityLabel("Schließen")
        }
        Divider()
        let rows = VStack(alignment: .leading, spacing: 10) {
            row(String(localized: "Gesamtsumme"), MenuBarSummary.totalText(of: selected),
                negative: MenuBarSummary.totalIsNegative(of: selected), focused: focused == nil, sensitiveTitle: false) {
                focus = MenuBarSummary.totalFocus
                showList = false
            }
            Divider()
            ForEach(selected) { a in
                row(a.name, MenuBarSummary.balanceText(a), negative: MenuBarSummary.isNegative(a),
                    focused: focused?.id == a.id, sensitiveTitle: true) {
                    focus = String(a.id)
                    showList = false
                }
            }
        }
        // many accounts scroll instead of growing past the screen
        ScrollView { rows }.frame(maxHeight: 360 * textScale).fixedSize(horizontal: false, vertical: true)
    }

    /// The one account (or the sum) shown now, with the switch.
    @ViewBuilder
    private func current(_ selected: [Account]) -> some View {
        let account = MenuBarSummary.focusedAccount(in: selected, focus: focus)
        // the fetch time belongs to one account; the sum shows none
        let updated = account.flatMap { MenuBarSummary.lastUpdate(of: [$0], connections: model.connections) }
        HStack(spacing: 6) {
            Button { focus = MenuBarSummary.cycle(focus: focus, selected: selected, step: -1) } label: {
                Image(systemName: "chevron.left").appFont(.callout, weight: .semibold)
            }
            .help("Vorheriges Konto")
            .accessibilityLabel("Vorheriges Konto")
            Button { showList = true } label: {
                HStack(spacing: 4) {
                    if let account {
                        Text(account.name).appFont(.callout, weight: .semibold).lineLimit(1).truncationMode(.tail).sensitive()
                    } else {
                        Text("Gesamtsumme").appFont(.callout, weight: .semibold)
                    }
                    Image(systemName: "chevron.down")
                        .appFont(.caption2, weight: .semibold)
                        .accessibilityHidden(true)
                }
                .frame(maxWidth: .infinity)
                .contentShape(Rectangle())
            }
            .help("Alle gewählten Konten anzeigen")
            Button { focus = MenuBarSummary.cycle(focus: focus, selected: selected, step: 1) } label: {
                Image(systemName: "chevron.right").appFont(.callout, weight: .semibold)
            }
            .help("Nächstes Konto")
            .accessibilityLabel("Nächstes Konto")
        }
        .buttonStyle(.borderless)
        Text(account.map(MenuBarSummary.balanceText) ?? MenuBarSummary.totalText(of: selected))
            .font(.system(size: 38 * textScale, weight: .bold, design: .default).monospacedDigit())
            .foregroundStyle((account.map(MenuBarSummary.isNegative) ?? MenuBarSummary.totalIsNegative(of: selected)) ? Theme.negative : Color.primary)
            .lineLimit(1).minimumScaleFactor(0.5)
            .sensitive()
            .contentShape(Rectangle())
            .onTapGesture(count: 2) { openInGiroCrab(account) }
            .help("Doppelklick öffnet dieses Konto in GiroCrab")
        // One caption slot: the sum names itself, an account shows its fetch time.
        // Always one line high, so the popover keeps its size while switching.
        let caption = account == nil ? String(localized: "Summe der gewählten Konten")
            : updated.map { String(localized: "Aktualisiert \(MenuBarSummary.updatedText($0))") } ?? " "
        Text(caption).appFont(.caption).foregroundStyle(.secondary)
            .accessibilityHidden(caption == " ")
    }
}

/// Settings › Allgemein › Dock & Menüleiste.
struct DockAndMenuBarSettingsSection: View {
    @Environment(AppModel.self) private var model
    @AppStorage(DockKeepAlive.enabledKey) private var keepAppOpenInDock = false
    @AppStorage(MenuBarSummary.enabledKey) private var enabled = false
    @AppStorage(MenuBarSummary.privateKey) private var privateMode = false
    @AppStorage(MenuBarSummary.accountsKey) private var accountsRaw = ""
    @AppStorage(MenuBarSummary.barDisplayKey) private var display = MenuBarSummary.BarDisplay.amount

    var body: some View {
        Section {
            Toggle("GiroCrab nach Fensterschließen geöffnet lassen", isOn: $keepAppOpenInDock)
            HStack(alignment: .firstTextBaseline, spacing: 4) {
                Image(systemName: "xmark.circle.fill")
                    .foregroundStyle(.red)
                    .accessibilityLabel(Text("Schließen"))
                Text("schließt das Fenster. GiroCrab läuft weiter.")
            }
            .appFont(.caption).foregroundStyle(.secondary)
            .accessibilityElement(children: .combine)

            Toggle("Konten in der Menüleiste anzeigen", isOn: $enabled)
            if enabled {
                Text("Zeigt das GiroCrab-Symbol in der Menüleiste. Ein Klick öffnet ein Popover mit dem Kontostand eines gewählten Kontos oder der Gesamtsumme, ein Rechtsklick ein Menü. Bei gesperrtem Tresor oder bei aktivem „Beträge in der Menüleiste verdecken“ erscheinen weder Beträge noch Kontonamen.")
                    .appFont(.caption).foregroundStyle(.secondary)
                Picker("In der Menüleiste", selection: $display) {
                    Text("Symbol und Betrag").tag(MenuBarSummary.BarDisplay.amount)
                    Text("Nur Symbol").tag(MenuBarSummary.BarDisplay.iconOnly)
                }
                Text("Bei „Nur Symbol“ steht der Betrag nur im Popover.")
                    .appFont(.caption).foregroundStyle(.secondary)
                Toggle("Beträge in der Menüleiste verdecken", isOn: $privateMode)
                Text("Verdeckt Beträge und Kontonamen in der Menüleiste und im Popover.")
                    .appFont(.caption).foregroundStyle(.secondary)
                if model.isUnlocked {
                    let accounts = model.allAccounts.filter { !$0.isArchived }
                    ForEach(accounts) { account in
                        Toggle(isOn: binding(for: account.id)) {
                            Text(account.name).appFont(.body).sensitive()
                        }
                    }
                }
            } else {
                Text("Zeigt das GiroCrab-Symbol oben in der Menüleiste.")
                    .appFont(.caption).foregroundStyle(.secondary)
            }
        } header: { Text("Dock & Menüleiste").appSectionHeader() }
    }

    private func binding(for id: Int64) -> Binding<Bool> {
        Binding(
            get: { MenuBarSummary.decode(accountsRaw).contains(id) },
            set: { on in
                var ids = Set(MenuBarSummary.decode(accountsRaw))
                if on { ids.insert(id) } else { ids.remove(id) }
                accountsRaw = MenuBarSummary.encode(ids)
            }
        )
    }
}
#endif
