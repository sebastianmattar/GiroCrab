import SwiftUI
import QuickLook
import GiroCrabCore

/// The app's scenes, shared by the macOS executable and the Xcode
/// multiplatform target.
public struct GiroCrabScenes: Scene {
    @State private var model: AppModel

    public init(options: LaunchOptions = .current) {
        _model = State(initialValue: AppModel(options: options))
    }

    public var body: some Scene {
        WindowGroup("GiroCrab", id: "main") {
            GiroCrabRootView()
                .environment(model)
                .modifier(AutoLockOnBackground(model: model))
                #if os(macOS)
                // Keep the minimum independent of the split columns' ideal heights.
                .frame(minHeight: 480, maxHeight: .infinity)
                #endif
        }
        #if os(macOS)
        .defaultSize(width: 1500, height: 900)
        .windowResizability(.contentMinSize)
        .commands {
            CommandGroup(replacing: .newItem) {
                Button("Neue Überweisung …") { model.openTransfer() }
                    .keyboardShortcut("n", modifiers: .command)
                    .disabled(!model.canTransfer)
                Button("Befehlspalette …") { model.showCommandPalette = true }
                    .keyboardShortcut("k", modifiers: .command)
                    .disabled(model.vault == nil)
                Button("Neue Kontogruppe …") { model.groupDialog = .create() }
                    .keyboardShortcut("n", modifiers: [.command, .shift])
                    .disabled(model.vault == nil)
                Button("Aus MoneyMoney importieren…") { model.openMoneyMoneyImport() }
                    .disabled(model.vault == nil)
                Button("Alle Konten abrufen") { model.requestSyncAll() }
                    .keyboardShortcut("r", modifiers: .command)
                    .disabled(model.vault == nil)
                Button("Regeln …") { model.openRuleEditor() }
                    .keyboardShortcut("k", modifiers: [.command, .option])
                    .disabled(model.vault == nil)
            }
            CommandMenu("Buchung") {
                Button("Kategorisieren …") { if model.selectedBooking != nil { model.showCategorizer = true } }
                    .disabled(model.selectedBooking == nil)
                Button("Regel für diesen Händler erstellen …") { model.openRuleEditor(from: model.selectedBooking) }
                    .disabled(model.selectedBooking == nil)
                Divider()
                RecurringMenu(model: model, booking: model.selectedBooking)
                    .disabled(model.selectedBooking == nil)
                Toggle("Als Gehalt / Rente zählen", isOn: Binding(
                    get: { model.selectedBooking?.isSalary ?? false },
                    set: { on in if let b = model.selectedBooking { model.setSalaryFlag(b, on) } }
                ))
                .disabled((model.selectedBooking?.amountCents ?? 0) <= 0)
                Button("Wiederkehrende Buchungen …") { model.showRecurringSheet = true }
                    .keyboardShortcut("f", modifiers: [.command, .shift])
                    .disabled(model.vault == nil)
                Divider()
                Button("Nächste Buchung") { model.moveSelection(1) }
                    .keyboardShortcut(.downArrow, modifiers: .command)
                Button("Vorherige Buchung") { model.moveSelection(-1) }
                    .keyboardShortcut(.upArrow, modifiers: .command)
                Divider()
                ForEach(Array(BookingScope.ordered.enumerated()), id: \.offset) { i, s in
                    Button(s.title) { model.scope = s }
                        .keyboardShortcut(KeyEquivalent(Character(String(i + 1))), modifiers: .command)
                }
            }
            CommandGroup(after: .appInfo) {
                Button("Tresor sperren") { model.lock() }
                    .keyboardShortcut("l", modifiers: [.command, .control])
                    .disabled(!model.hasOwnVault)
            }
            CommandGroup(after: .sidebar) {
                Picker("Konten anzeigen", selection: Binding(get: { model.accountView }, set: { model.accountView = $0 })) {
                    Text("Nach Gruppe").tag(AccountSidebarMode.byGroup)
                    Text("Nach Kontoart").tag(AccountSidebarMode.byKind)
                }
                .disabled(model.vault == nil)
                Button("Kontogruppen bearbeiten …") { model.openGroupSettings() }
                    .disabled(model.vault == nil)
                Divider()
                Button(model.demoActive ? "Eigenen Tresor anzeigen" : "Demo-Daten anzeigen") {
                    if model.demoActive { model.showOwnVault() } else { model.showDemo() }
                }
                .keyboardShortcut("d", modifiers: [.command, .shift])
                .disabled(model.vault == nil)
            }
        }
        #endif

        #if os(macOS)
        Settings {
            SettingsView()
                .environment(model)
                .frame(width: 760, height: 600)
        }
        #endif
    }
}

public struct GiroCrabRootView: View {
    @Environment(AppModel.self) private var model

    public init() {}

    public var body: some View {
        switch model.vaultState {
        case .open:
            MainSplitView()
        case .locked, .unlocking:
            LockView()
        case .failed(let message):
            LockView(error: message)
        }
    }
}

/// Locks the personal vault again after the app has been in the background
/// for a while, so the Face ID / passcode prompt returns on the next start.
struct AutoLockOnBackground: ViewModifier {
    let model: AppModel
    @Environment(\.scenePhase) private var phase
    @State private var backgroundedAt: Date?
    private let timeout: TimeInterval = 60

    func body(content: Content) -> some View {
        #if os(iOS)
        content.onChange(of: phase) { _, new in
            switch new {
            case .background:
                backgroundedAt = Date()
            case .active:
                if let t = backgroundedAt, Date().timeIntervalSince(t) >= timeout, model.holdsRealData { model.lock() }
                backgroundedAt = nil
            default: break
            }
        }
        #else
        content
        #endif
    }
}

struct LockView: View {
    @Environment(AppModel.self) private var model
    var error: String?
    /// The unlock has been waiting for a while: most likely a Touch ID or
    /// Keychain prompt the user hasn't noticed (behind the window, other screen).
    @State private var waitingLong = false

    var body: some View {
        VStack(spacing: 18) {
            Image(systemName: "lock.shield.fill")
                .font(.system(size: 60))
                .foregroundStyle(Theme.accent)
            Text("GiroCrab ist gesperrt")
                .font(.title.bold())
            Text("Ihre Buchungen liegen in einem verschlüsselten Tresor.\nDer Schlüssel verlässt nie den Schlüsselbund dieses Geräts.")
                .multilineTextAlignment(.center)
                .foregroundStyle(Theme.secondary)
            Button {
                model.unlock()
            } label: {
                Label("Mit \(VaultKeyManager.shared.biometryName) entsperren", systemImage: VaultKeyManager.shared.biometrySymbol)
                    .padding(.horizontal, 8)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
            .disabled(model.vaultState == .unlocking)
            if model.vaultState == .unlocking {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text("Warte auf macOS …").foregroundStyle(Theme.secondary)
                }
                if waitingLong {
                    Label("Bitte bestätigen Sie die Abfrage von macOS (Touch ID oder Schlüsselbund). Sie kann hinter diesem Fenster oder auf einem anderen Bildschirm liegen.", systemImage: "exclamationmark.bubble")
                        .font(.callout)
                        .foregroundStyle(Theme.warning)
                        .multilineTextAlignment(.center)
                        .frame(maxWidth: 460)
                }
            }
            if let error {
                Text(error).font(.callout).foregroundStyle(.red).multilineTextAlignment(.center)
            }
            if model.lockPreview && !AppModel.vaultMayHoldRealData && model.vaultState != .unlocking {
                Button("Zurück zur Demo") { model.endLockPreview() }
                    .buttonStyle(.borderless)
            }
        }
        .padding(40)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.canvas)
        .task {
            if model.vaultState == .locked && error == nil && !model.demoDismissedLock { model.unlock() }
        }
        .task(id: model.vaultState == .unlocking) {
            waitingLong = false
            guard model.vaultState == .unlocking else { return }
            try? await Task.sleep(for: .seconds(5))
            if !Task.isCancelled && model.vaultState == .unlocking { waitingLong = true }
        }
    }
}

struct MainSplitView: View {
    @Environment(AppModel.self) private var model
    #if os(macOS)
    @State private var columns: NavigationSplitViewVisibility = .all
    @Environment(\.openSettings) private var openSettings
    #else
    @State private var columns: NavigationSplitViewVisibility = .automatic
    #endif

    var body: some View {
        @Bindable var model = model
        NavigationSplitView(columnVisibility: $columns) {
            SidebarView()
                .frame(minHeight: 0, maxHeight: .infinity)
                .macColumnWidth(min: 240, ideal: 260, max: 320)
        } content: {
            Group {
                if model.sidebarSelection == .documents {
                    DocumentsView()
                } else {
                    BookingStreamView()
                }
            }
            .frame(minHeight: 0, maxHeight: .infinity)
            // min widths still sum to 1080 pt (240 + 560 + 280) so the minimum window size stays as before
            .macColumnWidth(min: 560, ideal: 900)
        } detail: {
            InspectorView()
                .frame(minHeight: 0, maxHeight: .infinity)
                .macColumnWidth(min: 280, ideal: 320, max: 380)
        }
        .navigationSplitViewStyle(.balanced)
        .quickLookPreview($model.quickLookURL)
        .accountGroupDialogs($model.groupDialog)
        #if os(macOS)
        // "Gruppen bearbeiten …" → Settings › Kontogruppen
        .onChange(of: model.groupSettingsRequested) { _, requested in
            guard requested else { return }
            model.groupSettingsRequested = false
            openSettings()
        }
        #endif
        .sheet(isPresented: $model.showRuleEditor) {
            RuleEditorView(initialDraft: model.ruleDraft)
                .environment(model)
        }
        .sheet(isPresented: $model.showCommandPalette) {
            CommandPaletteView()
                .environment(model)
        }
        .sheet(isPresented: $model.showMoneyMoneyImport) {
            MoneyMoneyImportSheet()
                .environment(model)
        }
        .sheet(isPresented: $model.showRecurringSheet) {
            RecurringSheet()
                .environment(model)
        }
        .sheet(item: $model.customRecurringBooking) { b in
            CustomDatesSheet(initialDates: model.recurringSeries(for: b).flatMap { $0.interval == .custom ? $0.customDates : nil } ?? []) { dates in
                model.markRecurring(b, .custom, customDates: dates)
            }
            .environment(model)
        }
        .sheet(isPresented: $model.showCategorizer) {
            CategorizerSheet()
                .environment(model)
        }
        .sheet(isPresented: $model.showTransferSheet) {
            TransferSheetView()
                .environment(model)
                .interactiveDismissDisabled(model.transferBusy)
        }
        .sheet(isPresented: Binding(get: { model.splitBooking != nil }, set: { if !$0 { model.splitBooking = nil } })) {
            if let b = model.splitBooking { SplitSheet(booking: b).environment(model) }
        }
        .sheet(item: $model.payeeCheck) { _ in
            PayeeCheckSheet()
                .environment(model)
                .interactiveDismissDisabled()
        }
        .sheet(item: $model.activeChallenge) { _ in
            TwoFactorSheet()
                .environment(model)
                .interactiveDismissDisabled()
        }
        .sheet(item: $model.webLoginConnection) { c in
            Group {
                if c.connector == .tradeRepublic { WebLoginSheet(connection: c) }
                else if c.connector == .amazonOrders { AmazonWebSheet(connection: c) }
                else { PortalWebSheet(connection: c) }
            }
            .environment(model)
        }
        .sheet(item: $model.setupRequest) { req in
            BankSetupSheet(request: req)
                .environment(model)
        }
        .sheet(item: $model.pendingLogin) { login in
            PinSheet(connection: login.connection)
                .environment(model)
        }
        #if os(iOS)
        .sheet(isPresented: $model.showSettings) {
            NavigationStack { SettingsView() }
                .environment(model)
        }
        #endif
        .alert("Status der Überweisung unklar", isPresented: Binding(get: { model.transferUnknown != nil }, set: { if !$0 { model.transferUnknown = nil } })) {
            Button("Verstanden", role: .cancel) { model.transferUnknown = nil }
        } message: {
            if let u = model.transferUnknown {
                Text(String(format: String(localized: "Der Auftrag hat die Bank erreicht, aber die Antwort ist nicht eindeutig. Die Überweisung wurde möglicherweise bereits ausgeführt.\n\nBitte prüfe zuerst dein Konto (Umsätze oder Online-Banking), bevor du sie erneut sendest. In GiroCrab ist sie als „Status unklar“ vorgemerkt.\n\nReferenz (EndToEnd-ID): %@\nBank: %@"), u.endToEndId, u.reason))
            }
        }
        .alert("Etwas ist schiefgelaufen", isPresented: Binding(get: { model.errorMessage != nil }, set: { if !$0 { model.errorMessage = nil } })) {
            Button("OK", role: .cancel) { model.errorMessage = nil }
        } message: {
            Text(model.errorMessage ?? "")
        }
        .overlay(alignment: .bottom) {
            if let toast = model.toast {
                Label(toast, systemImage: "checkmark.circle.fill")
                    .padding(.horizontal, 16).padding(.vertical, 10)
                    .background(.regularMaterial, in: Capsule())
                    .shadow(color: .black.opacity(0.08), radius: 8, y: 2)
                    .padding(.bottom, 22)
                    .transition(.move(edge: .bottom).combined(with: .opacity))
                    .task {
                        try? await Task.sleep(nanoseconds: 3_500_000_000)
                        withAnimation { model.toast = nil }
                    }
            }
        }
        .animation(.spring(duration: 0.3), value: model.toast)
    }
}
