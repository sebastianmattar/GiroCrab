import SwiftUI
import QuickLook
import GiroCrabCore
#if os(macOS)
import AppKit
#endif

/// The app's scenes, shared by the macOS executable and the Xcode
/// multiplatform target.
public struct GiroCrabScenes: Scene {
    @State private var model: AppModel
    @AppStorage(AppAppearance.defaultsKey) private var appearance: AppAppearance = .system
    #if os(macOS)
    @Environment(\.openWindow) private var openWindow
    @AppStorage(AppTextSize.defaultsKey) private var textSize: AppTextSize = .standard
    #endif

    public init(options: LaunchOptions = .current) {
        _model = State(initialValue: Self.prepare(options: options))
    }

    /// Creates the app's one model. Call it from the `App`'s `init`, not from its
    /// `body`: SwiftUI re-creates this struct on every evaluation of the app body,
    /// and a model built there (a) is built again each time – `AppModel.init`
    /// opens the vault and loads everything on the main thread – and (b) makes
    /// every property the init reads a dependency of the app body, so the
    /// model's own async loads re-evaluate the body and build the next model.
    /// A trace showed 10–13 such throw-away models per second.
    @discardableResult
    public static func prepare(options: LaunchOptions = .current) -> AppModel {
        if let model = sharedModel { return model }
        let model = AppModel(options: options)
        sharedModel = model
        GiroCrabTips.configure(options)
        return model
    }

    /// The one model of the app process; see `prepare`.
    @MainActor private static var sharedModel: AppModel?

    public var body: some Scene {
        WindowGroup("GiroCrab", id: "main") {
            // `GiroCrabRootView` sets the preferred colour scheme itself (dark on the
            // brand start/lock screen, otherwise the chosen appearance).
            GiroCrabRootView()
                .environment(model)
                .modifier(AutoLockOnBackground(model: model))
                .privacyModeEnvironment()
                .listSettingsEnvironment()
                // `girocrab://transfer?…` from another app or a web page (PaymentLink); the
                // open main window takes it instead of a new one being made for it.
                .onOpenURL { model.openLink($0) }
                .handlesExternalEvents(preferring: [PaymentLink.scheme], allowing: ["*"])
                #if os(macOS)
                .appTextSize(textSize.effective)
                .modifier(MenuBarBridge(model: model))
                // Keep the minimum independent of the split columns' ideal heights.
                .frame(minHeight: 480, maxHeight: .infinity)
                #endif
        }
        .handlesExternalEvents(matching: [PaymentLink.scheme])
        #if os(macOS)
        // Fits a 13" MacBook Air (1470 × 956 pt) with room for the Dock.
        .defaultSize(width: 1280, height: 820)
        .windowResizability(.contentMinSize)
        .commands {
            FileCommands(model: model)
            BookingCommands(model: model)
            AppCommands(model: model, openWindow: openWindow)
            SidebarCommands()
            MainToolbarCommands(model: model)
            ViewCommands(model: model)
        }
        #endif

        #if os(macOS)
        Window("Regeln", id: RuleEditorWindow.id) {
            RuleEditorWindow()
                .environment(model)
                .appTextSize(textSize.effective)
                .preferredColorScheme(appearance.colorScheme)
        }
        .defaultSize(width: 920, height: 660)
        .windowResizability(.contentMinSize)
        .commandsRemoved()

        Window("Mitteilungen", id: "announcements") {
            AnnouncementsListView()
                .appTextSize(textSize.effective)
                .preferredColorScheme(appearance.colorScheme)
        }
        .defaultSize(width: 460, height: 360)
        Window("Abruf-Protokoll", id: "sync-log") {
            SyncLogView()
                .toastOverlay()
                .environment(model)
                .privacyModeEnvironment()
                .appTextSize(textSize.effective)
                .preferredColorScheme(appearance.colorScheme)
        }
        .defaultSize(width: 900, height: 520)

        Window("Problem melden", id: "bug-report") {
            BugReportView()
                .environment(model)
                .preferredColorScheme(appearance.colorScheme)
        }
        .defaultSize(width: 760, height: 860)

        Window("GiroCrab-Hilfe", id: "help") {
            NavigationStack { HelpView() }
                .appTextSize(textSize.effective)
                .preferredColorScheme(appearance.colorScheme)
        }
        .defaultSize(width: 640, height: 720)
        Settings {
            // HIG: the Settings window fits each tab (SettingsView sizes them).
            // `reportCapturable`: "Problem melden" draws the Settings window
            // (a window of its own, like a sheet) over the main window, so its
            // views get the same redaction and mark personal data.
            SettingsView()
                // Notices raised here appear here, not behind it in the main window (#1145).
                .toastOverlay()
                .environment(model)
                .privacyModeEnvironment()
                .appTextSize(textSize.effective)
                .preferredColorScheme(appearance.colorScheme)
                .reportCapturable()
        }
        #endif
    }
}

public struct GiroCrabRootView: View {
    @Environment(AppModel.self) private var model
    /// Language question at start-up (#93, macOS only; iOS has a per-app
    /// language setting of its own). Skipped for developer/screenshot launches.
    @State private var languageQuestion: LanguageQuestion = {
        #if os(macOS)
        let o = LaunchOptions.current
        let developerLaunch = o.demoSeed || o.freshVault || o.screenshotPath != nil
        guard LanguagePreference.isAvailable, !developerLaunch || o.simulateSystemLanguage != nil else { return .none }
        return LanguagePreference.current.startupQuestion()
        #else
        return .none
        #endif
    }()
    @Environment(\.scenePhase) private var scenePhase
    #if os(macOS)
    @Environment(\.openWindow) private var openWindow
    #endif
    /// iOS cold start: keep the animated crab visible for ~1 s, even when the vault
    /// is ready earlier. Only the view is held back; `bootstrap()` runs immediately.
    #if os(iOS)
    @State private var splashHold = true
    #else
    @State private var splashHold = false
    /// `--demo-sheet report`: the report window was opened (see `MainSplitView`).
    @State private var demoReportShown = false
    #endif

    @AppStorage(AppAppearance.defaultsKey) private var appearance: AppAppearance = .system

    public init() {}

    private var splash: some View { SplashView() }

    /// Colour scheme of the window. The brand start and lock screen (iOS, #505)
    /// are always dark, so the status bar (time, battery) stays white on the
    /// dark backdrop even when the device or the app runs in light mode. Set
    /// here in one place: a second `.preferredColorScheme` further out would
    /// override one on the screens themselves.
    static func preferredScheme(brand: Bool, splashHold: Bool, vaultOpen: Bool, appearance: ColorScheme?) -> ColorScheme? {
        brand && (splashHold || !vaultOpen) ? .dark : appearance
    }

    public var body: some View {
        Group {
            if splashHold && model.vaultState != .starting {
                splash.transition(.opacity)
            } else {
            switch model.vaultState {
            case .starting where languageQuestion == .chooseLanguage:
                LanguagePromptView { languageQuestion = .none }
            case .starting:
                // Same background + centred logo as the system launch screen (LaunchInfo.plist),
                // so there is no visual jump between the two.
                splash
                    .onAppear { model.bootstrap() }
            case .open:
                MainSplitView()
                    // the only window the bug report may capture; inside the
                    // redaction environment, so it can report what was built
                    .background(ReportWindowTracker(role: .main))
                    .transformEnvironment(\.reportRedaction) { $0.formUnion(model.reportRedaction.union(LaunchOptions.current.redaction)) }
                    .onAppear { StartupClock.mark("MainSplitView appear") }
                    #if os(macOS)
                    .onChange(of: model.ruleEditorRequest, initial: true) {
                        guard model.showRuleEditor else { return }
                        openWindow(id: RuleEditorWindow.id)
                    }
                    // screenshots: --demo-sheet report[:<sheet>] captures the open
                    // view or demo sheet, as "Problem melden" would; once, also
                    // after a lock/unlock brings this view back
                    .task(id: model.options.demoSheet) {
                        guard model.options.demoSheetOpensReport, !demoReportShown else { return }
                        demoReportShown = true
                        try? await Task.sleep(for: .milliseconds(2400))
                        openWindow(id: "bug-report")
                    }
                    #endif
                    #if os(iOS)
                    .onDisappear {
                        // #384: a pick with no scene left to pick in ends (iPad:
                        // scenes closed). Checked after this update, once the
                        // closed scene's views are gone.
                        Task { @MainActor in
                            if model.reportPick != nil, !BugReport.hasMainWindow { model.reportPick = nil }
                        }
                    }
                    #endif
                    // Asked only once the vault is open, so it never competes with Touch ID.
                    .modifier(SystemLanguageChangedAlert(newLanguage: Binding(
                        get: { if case .systemChanged(let to) = languageQuestion { to } else { nil } },
                        set: { if $0 == nil { languageQuestion = .none } }
                    )))
            case .locked, .unlocking:
                LockView()
            case .failed(let message):
                LockView(error: message)
            }
            }
        }
        .preferredColorScheme(Self.preferredScheme(
            brand: BrandBackdrop.isPlatformDefault, splashHold: splashHold,
            vaultOpen: model.vaultState == .open, appearance: appearance.colorScheme
        ))
        .animation(.easeOut(duration: 0.35), value: splashHold)
        .task {
            guard splashHold else { return }
            try? await Task.sleep(for: .seconds(1.0))
            splashHold = false
        }
        .onChange(of: scenePhase) { _, phase in CloudSyncManager.shared.sceneChanged(phase) }
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

/// Start screen: the system launch screen (LaunchInfo.plist) continued, so there is no jump.
struct SplashView: View {
    /// Dark brand backdrop with the large crab (iOS, #505) instead of the canvas.
    var brand = BrandBackdrop.isPlatformDefault

    var body: some View {
        ZStack {
            if brand {
                BrandBackdrop.color.ignoresSafeArea()
                BrandCrabLogo()
                ProgressView().tint(.white).offset(y: BrandBackdrop.logoSize * 0.5 + 44)
            } else {
                MacLaunchBackground()
                VStack(spacing: 20) {
                    MacHeroIcon(phase: .idle)
                    ProgressView()
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .environment(\.colorScheme, brand ? .dark : scheme)
    }

    @Environment(\.colorScheme) private var scheme
}

struct LockView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.colorScheme) private var scheme
    var error: String?
    /// Dark brand backdrop, large crab and one unlock control (iOS, #505).
    var brand = BrandBackdrop.isPlatformDefault
    /// The unlock has been waiting for a while: most likely a Touch ID or
    /// Keychain prompt the user hasn't noticed (behind the window, other screen).
    @State private var waitingLong = false

    private var waitingHint: LocalizedStringKey {
        #if os(iOS)
        "Bitte bestätigen Sie die Abfrage auf Ihrem Gerät (Face ID oder Code)."
        #else
        "Bitte bestätigen Sie die Abfrage von macOS (Touch ID oder Schlüsselbund). Sie kann hinter diesem Fenster oder auf einem anderen Bildschirm liegen."
        #endif
    }

    private var crabPhase: AnimatedCrabLogoView.Phase {
        if model.unlockCelebrating { return .unlocked }
        if error != nil { return .failed }
        if case .unlocking = model.vaultState { return .unlocking }
        return .idle
    }

    /// One control for both states (#505): the unlock button turns into the
    /// waiting status instead of a greyed-out button with a second line below.
    private var brandUnlockControl: some View {
        let waiting = model.vaultState == .unlocking
        let name = VaultKeyManager.shared.biometryName
        let title: LocalizedStringKey = waiting ? "Warten auf \(name) …" : "Mit \(name) entsperren"
        return Button {
            model.unlock()
        } label: {
            HStack(spacing: 10) {
                if waiting {
                    ProgressView().controlSize(.small).tint(.white)
                } else {
                    Image(systemName: VaultKeyManager.shared.biometrySymbol)
                }
                Text(title)
            }
            .appFont(.headline)
            .foregroundStyle(Color.white.opacity(waiting ? 0.8 : 1))
            .padding(.horizontal, 20)
            .frame(maxWidth: 340, minHeight: 54)
            .background {
                RoundedRectangle(cornerRadius: 16, style: .continuous)
                    .fill(waiting ? AnyShapeStyle(Color.white.opacity(0.1)) : AnyShapeStyle(Theme.accent))
            }
            .overlay {
                RoundedRectangle(cornerRadius: 16, style: .continuous)
                    .strokeBorder(Color.white.opacity(waiting ? 0.22 : 0))
            }
            .contentShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
        }
        .buttonStyle(.plain)
        .disabled(waiting)
        .animation(Motion.animation(.easeInOut(duration: 0.2)), value: waiting)
    }

    private var brandBody: some View {
        VStack(spacing: 0) {
            Spacer(minLength: 0)
            BrandCrabLogo(phase: crabPhase)
            Text("GiroCrab ist gesperrt")
                .appFont(.title, weight: .bold)
                .foregroundStyle(.white)
                .multilineTextAlignment(.center)
            Text("Ihre Buchungen liegen in einem verschlüsselten Tresor.\nDer Schlüssel verlässt nie den Schlüsselbund dieses Geräts.")
                .multilineTextAlignment(.center)
                .foregroundStyle(Color.white.opacity(0.7))
                .padding(.top, 10)
            Spacer(minLength: 24)
            VStack(spacing: 14) {
                if let error {
                    Text(error).appFont(.callout).foregroundStyle(Theme.charge).multilineTextAlignment(.center)
                }
                if waitingLong && model.vaultState == .unlocking {
                    Label(waitingHint, systemImage: "exclamationmark.bubble")
                        .appFont(.callout)
                        .foregroundStyle(Theme.warning)
                        .multilineTextAlignment(.center)
                        .frame(maxWidth: 460)
                }
                brandUnlockControl
                if model.lockPreview && !AppModel.vaultMayHoldRealData && model.vaultState != .unlocking {
                    Button("Zurück zur Demo") { model.endLockPreview() }
                        .buttonStyle(.borderless)
                        .tint(.white)
                }
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 28)
        .padding(.vertical, 24)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(BrandBackdrop.color.ignoresSafeArea())
    }

    var body: some View {
        Group {
            if brand { brandBody } else { standardBody }
        }
        // Theme colours on the dark backdrop resolve to their dark variants.
        .environment(\.colorScheme, brand ? .dark : scheme)
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

    private var standardBody: some View {
        VStack(spacing: 16) {
            MacHeroIcon(phase: crabPhase)
                .padding(.bottom, -22)
            Text("GiroCrab ist gesperrt")
                .appFont(.title, weight: .bold)
            Text("Ihre Buchungen liegen in einem verschlüsselten Tresor.\nDer Schlüssel verlässt nie den Schlüsselbund dieses Geräts.")
                .appFont(.body)
                .multilineTextAlignment(.center)
                .foregroundStyle(Theme.secondary)
                .padding(.bottom, 6)
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
                    #if os(iOS)
                    Text("Warten auf \(VaultKeyManager.shared.biometryName) …").foregroundStyle(Theme.secondary)
                    #else
                    Text("Warte auf macOS …").foregroundStyle(Theme.secondary)
                    #endif
                }
                if waitingLong {
                    Label(waitingHint, systemImage: "exclamationmark.bubble")
                        .appFont(.callout)
                        .foregroundStyle(Theme.warning)
                        .multilineTextAlignment(.center)
                        .frame(maxWidth: 460)
                }
            }
            if let error {
                Text(error).appFont(.callout).foregroundStyle(Theme.negative).multilineTextAlignment(.center)
            }
            if model.lockPreview && !AppModel.vaultMayHoldRealData && model.vaultState != .unlocking {
                Button("Zurück zur Demo") { model.endLockPreview() }
                    .buttonStyle(.borderless)
            }
        }
        .padding(40)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(MacLaunchBackground())
    }
}

struct MainSplitView: View {
    @Environment(AppModel.self) private var model
    @AppStorage(EnableBankingKeySync.noticeSuppressedKey) private var enableBankingNoticeSuppressed = false
    @Environment(\.textScale) private var textScale
    @AppStorage(ViewLayout.inspectorKey) private var showInspector = true
    #if os(macOS)
    @State private var columns: NavigationSplitViewVisibility = .all
    @State private var inspectorWidth: CGFloat = InspectorWidth.standard
    @State private var inspectorDragStart: CGFloat?
    @State private var splitterHovered = false
    @State private var splitterCursorPushed = false
    @Environment(\.openSettings) private var openSettings
    #else
    /// The sidebar opens on request (toolbar button): next to the detail column
    /// the booking column needs the width.
    @State private var columns: NavigationSplitViewVisibility = .detailOnly
    @Environment(\.horizontalSizeClass) private var hSize
    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.dynamicTypeSize) private var dynamicTypeSize
    @State private var span = WindowSpan()
    #endif

    private var sidebar: some View {
        SidebarView()
            .frame(minHeight: 0, maxHeight: .infinity)
            // wider with larger text (#343): balances must not be cut off
            .macColumnWidth(min: 240 * textScale, ideal: 260 * textScale, max: 480 * textScale)
    }

    private var bookingColumn: some View {
        Group {
            switch model.mainColumn {
            case .documents: DocumentsView()
            case .planned: PlannedPaymentsView()
            case .directDebits: DirectDebitsView()
            case .subscriptions: SubscriptionsView()
            case .wealth: WealthView()
            case .bookings: AccountColumn()
            }
        }
        #if os(macOS)
        // The toolbar belongs to the column, not to the view in it: its items
        // stay when Belege replaces the booking list.
        .modifier(MainToolbar())
        // Window title for both views here too, so the toolbar keeps the same
        // compact style (and height) when switching to Belege.
        .modifier(WindowTitle())
        #endif
        .frame(minHeight: 0, maxHeight: .infinity)
    }

    #if os(macOS)
    /// macOS: the right pane sits next to the booking column in the `detail`
    /// column instead of the system `.inspector`. On macOS 27 the inspector
    /// renders its content a few points below where it hit-tests, so clicks in
    /// it land one row off (#492); a plain view does not have that offset.
    private var splitView: some View {
        NavigationSplitView(columnVisibility: $columns) {
            sidebar
        } detail: {
            HStack(spacing: 0) {
                bookingColumn
                // Belege & Postbox has its own preview and Vermögen no booking list (#1808): the booking
                // inspector has nothing to show there.
                if showInspector, model.mainColumn.hasInspector {
                    inspectorSplitter
                    InspectorView()
                        .frame(width: inspectorWidth * textScale)
                        .frame(minHeight: 0, maxHeight: .infinity)
                }
            }
            .frame(minHeight: 0, maxHeight: .infinity)
            // min widths: 240 (sidebar) + 560 + 280 (inspector) keep the minimum window size.
            // Must not depend on `inspectorWidth`: the split view re-lays out whenever these
            // change, which made the column jump while the splitter was dragged (#828).
            .macColumnWidth(min: showInspector && model.mainColumn.hasInspector ? 560 + InspectorWidth.minimum * textScale : 560,
                            ideal: showInspector && model.mainColumn.hasInspector ? 700 + InspectorWidth.standard * textScale : 700)
        }
        .navigationSplitViewStyle(.balanced)
        .sidebarDividerToTop()
    }

    /// Draggable grip between the booking column and the inspector; the system
    /// `.inspector` provided one, so the replacement HStack re-implements it.
    private var inspectorSplitter: some View {
        ZStack {
            Color.clear.frame(width: 10)
            Theme.systemSeparator.frame(width: 1).frame(maxHeight: .infinity)
        }
        .contentShape(Rectangle())
        .frame(width: 10)
        .onHover { inside in
            splitterHovered = inside
            updateSplitterCursor()
        }
        .onDisappear {
            splitterHovered = false
            inspectorDragStart = nil
            updateSplitterCursor()
        }
        .gesture(
            // The splitter moves with the column while it is dragged, so its own (local)
            // coordinates would shift under the pointer and feed back into the width (#828).
            // Window coordinates stay put.
            DragGesture(minimumDistance: 0, coordinateSpace: .global)
                .onChanged { value in
                    if inspectorDragStart == nil { inspectorDragStart = inspectorWidth }
                    inspectorWidth = InspectorWidth.dragged(from: inspectorDragStart ?? inspectorWidth,
                                                            translation: value.translation.width,
                                                            textScale: textScale)
                    updateSplitterCursor()
                }
                .onEnded { _ in
                    inspectorDragStart = nil
                    updateSplitterCursor()
                }
        )
    }

    /// Resize cursor while hovering or dragging; push and pop stay paired so it cannot stick.
    private func updateSplitterCursor() {
        let wanted = splitterHovered || inspectorDragStart != nil
        if wanted, !splitterCursorPushed {
            NSCursor.resizeLeftRight.push()
            splitterCursorPushed = true
        } else if !wanted, splitterCursorPushed {
            NSCursor.pop()
            splitterCursorPushed = false
        }
    }
    #else
    /// iOS/iPadOS: the booking column is the main area at every width, the
    /// booking detail sits next to it when the window is wide enough and opens
    /// as a sheet otherwise. The width decides, not the size class: a
    /// three-column split shows only its (empty) detail column in a regular
    /// but narrow window such as an unfolded iPhone Duo held upright, and the
    /// system `.inspector` shows neither column nor sheet there.
    @Environment(\.reportRedaction) private var reportRedaction

    /// iPhone sheet title: the merchant, else the booking text like the list row, else
    /// "Buchung" (an empty name left the title blank).
    private var bookingSheetTitle: String {
        guard let b = model.selectedBooking else { return String(localized: "Buchung") }
        let name = b.merchant.name.isEmpty ? b.bookingText : b.merchant.name
        return name.isEmpty ? String(localized: "Buchung") : name
    }

    private var splitView: some View {
        HStack(spacing: 0) {
            NavigationSplitView(columnVisibility: $columns) {
                sidebar
            } detail: {
                bookingColumn
                    .toolbar {
                        // Only where the column has room: in a narrower window the
                        // detail is a sheet anyway and the switch would do nothing.
                        if inspectorFits {
                            ToolbarItem(placement: .primaryAction) { inspectorToggle }
                        }
                    }
            }
            .navigationSplitViewStyle(.balanced)
            if inspectorShown {
                Divider().ignoresSafeArea()
                InspectorView()
                    .frame(width: inspectorWidth)
                    .frame(minHeight: 0, maxHeight: .infinity)
                    .background(Theme.systemBackground.ignoresSafeArea())
            }
        }
        .onGeometryChange(for: WindowSpan.self) {
            WindowSpan(width: $0.size.width, leading: $0.safeAreaInsets.leading, trailing: $0.safeAreaInsets.trailing)
        } action: { span = $0 }
        .capturableSheet(isPresented: Binding(
            get: { !inspectorShown && model.showBookingSheet && hasDetailSelection },
            // Only a dismissal by the user ends it: when the window gets wide
            // enough the sheet gives way to the column and returns with the
            // same booking once the window is narrow again.
            set: { if !$0, !inspectorShown { model.showBookingSheet = false } }
        )) {
            // Title + "Fertig" (UI-Audit INS-3); the merchant is personal, so the
            // title is masked in the bug report (a parameter text, no `.sensitive()`).
            NavigationStack {
                InspectorView()
                    .navigationTitle(reportRedaction.masking(bookingSheetTitle))
                    .navigationBarTitleDisplayMode(.inline)
                    .toolbar {
                        ToolbarItem(placement: .confirmationAction) {
                            Button("Fertig") { model.showBookingSheet = false }
                        }
                    }
            }
            // The presentation can retain the dark launch appearance while
            // the booking list has already switched to its effective scheme.
            .preferredColorScheme(colorScheme)
            .presentationDetents(dynamicTypeSize.isAccessibilitySize ? [.large] : [.medium, .large])
            .presentationDragIndicator(.visible)
        }
        // Unfolding or widening the window (compact → regular) would otherwise
        // bring the sidebar back as an overlay on top of the booking column.
        .onChange(of: hSize) { _, new in
            if new == .regular { columns = .detailOnly }
        }
        // Turning an iPad upright keeps a sidebar that was open as a column of its own,
        // which leaves the booking column too little room next to the detail column.
        .onChange(of: span.full < 1000) { _, narrow in
            if narrow { columns = .detailOnly }
        }
        // In such a window the open sidebar takes its width from the main column (the
        // documents view no longer fits beside it), so choosing an entry closes it again,
        // as the push to the detail does on an iPhone.
        .background(SidebarSelectionWatcher {
            if span.full < 1000 { columns = .detailOnly }
        })
    }

    /// Room for the booking column (about a phone's width) next to the detail
    /// column. The documents view has its own preview, and „Vermögen“ has nothing to
    /// select: the column would show the booking picked last in another list.
    private var inspectorFits: Bool {
        model.mainColumn != .documents && model.mainColumn != .wealth && span.full >= 760 * textScale
    }

    /// The detail column is on screen: there is room for it and the user has not
    /// hidden it. Hidden, the detail opens as a sheet like in a narrow window (#1746).
    private var inspectorShown: Bool { showInspector && inspectorFits }

    /// Hides or shows the detail column; the choice is kept like on the Mac
    /// (`ViewLayout.inspectorKey`, the same switch as „Informationen ausblenden“ there).
    private var inspectorToggle: some View {
        Button {
            // A booking tapped while the column was showing asked for no sheet:
            // hiding the column must not open one for it.
            if showInspector { model.showBookingSheet = false }
            showInspector.toggle()
        } label: {
            Label(showInspector ? "Informationen ausblenden" : "Informationen einblenden", systemImage: "sidebar.trailing")
        }
    }

    /// The list in the booking column has a selection the detail sheet can show.
    private var hasDetailSelection: Bool {
        switch model.mainColumn {
        case .bookings: model.selectedBookingID != nil
        case .planned: model.selectedPlannedID != nil
        case .directDebits: model.selectedMandateID != nil
        case .subscriptions: model.selectedSubscriptionID != nil
        case .documents, .wealth: false
        }
    }

    /// Up to about 1000 pt the two columns meet on the centre line of the window,
    /// which is the fold of an open iPhone Duo; wider windows give the list more.
    private var inspectorWidth: CGFloat {
        if span.full < 1000 { return span.width - (span.full / 2 - span.leading) }
        return min(max(span.width * 0.4, 320 * textScale), 440 * textScale)
    }

    /// Reports a new sidebar selection without making the split view read it: a body that
    /// depends on the selection re-lays out every column on each change (`AppModel.mainColumn`).
    private struct SidebarSelectionWatcher: View {
        @Environment(AppModel.self) private var model
        let changed: () -> Void

        var body: some View {
            Color.clear.onChange(of: model.sidebarSelection) { changed() }
        }
    }

    /// Width inside the safe area plus the horizontal insets around it.
    private struct WindowSpan: Equatable {
        var width: CGFloat = 0
        var leading: CGFloat = 0
        var trailing: CGFloat = 0
        var full: CGFloat { width + leading + trailing }
    }
    #endif

    var body: some View {
        @Bindable var model = model
        splitView
        .quickLookPreview($model.quickLookURL)
        .accountGroupDialogs($model.groupDialog)
        .newCategoryDialog()
        .manualLoanSheets(model)
        .openingBalanceSheet(model)
        .offlineAccountSheets(model)
        .tangibleAssetSheets(model)
        .modifier(AdvanziaBackgroundModifier())
        #if os(macOS)
        // AppModel.openSettings(_:), e.g. the merchant-logo tip → Settings › Händler-Logos
        .onChange(of: model.settingsRequested) { _, requested in
            guard requested else { return }
            model.settingsRequested = false
            openSettings()
        }
        #endif
        .capturableSheet(isPresented: $model.showGroupManager) {
            GroupManagerSheet()
                .environment(model)
        }
        #if os(macOS)
        .capturableSheet(isPresented: Bindable(ToolbarLayoutStore.shared).isCustomizing) {
            ToolbarCustomizeSheet()
        }
        // the draft is stored once the sheet is really gone, not when another
        // sheet opens on top of it
        .capturableSheet(isPresented: $model.showTagManager, onDismiss: { model.applyTagDraft() }) {
            TagManagerView()
                .environment(model)
        }
        #endif
        #if os(iOS)
        .capturableSheet(isPresented: $model.showRuleEditor) {
            RuleEditorView()
                .environment(model)
        }
        #endif
        .capturableSheet(isPresented: $model.showCommandPalette, onDismiss: {
            // Follow-up sheet/popover chosen in the palette: opened once the
            // palette is really gone, instead of after a guessed delay.
            let next = model.afterPaletteDismiss
            model.afterPaletteDismiss = nil
            next?()
        }) {
            CommandPaletteView()
                .environment(model)
        }
        .capturableSheet(item: $model.accountSelectionConnection) { c in
            AccountSelectionSheet(connection: c)
                .environment(model)
        }
        .capturableSheet(isPresented: $model.showAddBank, onDismiss: { model.advanziaWorkflow?.start() }) {
            AddBankSheet()
                .environment(model)
        }
        .capturableSheet(isPresented: $model.showMoneyMoneyImport) {
            MoneyMoneyImportSheet()
                .environment(model)
        }
        .capturableSheet(isPresented: $model.showRecurringSheet) {
            RecurringSheet()
                .environment(model)
        }
        .capturableSheet(item: $model.customRecurringBooking) { b in
            CustomDatesSheet(initialDates: model.recurringSeries(for: b).flatMap { $0.interval == .custom ? $0.customDates : nil } ?? []) { dates in
                model.markRecurring(b, .custom, customDates: dates)
            }
            .environment(model)
        }
        .capturableSheet(isPresented: $model.showCategorizer) {
            CategorizerSheet()
                .environment(model)
        }
        .capturableSheet(isPresented: $model.showBatchSheet) {
            BatchTransferSheet()
                .environment(model)
                .interactiveDismissDisabled(model.transferBusy)
        }
        .capturableSheet(isPresented: $model.showStandingSheet) {
            StandingOrderSheet()
                .environment(model)
                .interactiveDismissDisabled(model.transferBusy)
        }
        .alert("Status des Dauerauftrags unklar", isPresented: Binding(get: { model.standingUnknown != nil }, set: { if !$0 { model.standingUnknown = nil } })) {
            Button("Verstanden", role: .cancel) { model.standingUnknown = nil }
        } message: {
            Text("Der Auftrag hat die Bank erreicht, aber die Antwort ist nicht eindeutig. Bitte zuerst im Online-Banking prüfen, ob der Dauerauftrag angelegt, geändert oder gelöscht wurde, bevor Sie es erneut versuchen.\n\nBank: \(model.standingUnknown ?? "")")
        }
        .capturableSheet(isPresented: $model.showTransferSheet) {
            TransferSheetView()
                .environment(model)
                .interactiveDismissDisabled(model.transferBusy)
        }
        .capturableSheet(isPresented: Binding(get: { model.splitBooking != nil }, set: { if !$0 { model.splitBooking = nil } })) {
            if let b = model.splitBooking { SplitSheet(booking: b, presetPercent: model.splitPresetPercent).environment(model) }
        }
        .capturableSheet(isPresented: Binding(get: { model.refundPickerBooking != nil }, set: { if !$0 { model.refundPickerBooking = nil } })) {
            if let booking = model.refundPickerBooking {
                RefundPickerSheet(booking: booking).environment(model).id("\(model.vaultRevision)|\(booking.id)")
            }
        }
        .capturableSheet(item: $model.payeeCheck) { _ in
            PayeeCheckSheet()
                .environment(model)
                .interactiveDismissDisabled()
        }
        .capturableSheet(item: $model.activeChallenge) { _ in
            TwoFactorSheet()
                .environment(model)
                .interactiveDismissDisabled()
        }
        .capturableSheet(item: $model.webLoginConnection) { c in
            Group {
                if c.connector == .tradeRepublic { WebLoginSheet(connection: c) }
                else if c.connector == .amazonOrders { AmazonWebSheet(connection: c) }
                else if c.connector == .fnz { FnzWebSheet(connection: c) }
                else if c.connector == .equatePlus { EquatePlusWebSheet(connection: c) }
                else if c.connector == .quirion { QuirionWebSheet(connection: c) }
                else if c.connector == .sharinbox { SharinboxWebSheet(connection: c) }
                else if c.connector == .axaShareplan { AxaShareplanWebSheet(connection: c) }
                else if c.connector == .unionInvestment { UnionInvestmentWebSheet(connection: c) }
                else if c.connector == .airPlus { AirPlusWebSheet(connection: c) }
                else if c.connector == .berghausCie { BerghausCieWebSheet(connection: c) }
                else if c.connector == .schwaebischHall { SchwaebischHallWebSheet(connection: c) }
                else { PortalWebSheet(connection: c) }
            }
            .environment(model)
        }
        .capturableSheet(item: $model.setupRequest) { req in
            BankSetupSheet(request: req)
                .environment(model)
        }
        .capturableSheet(item: $model.pendingLogin) { login in
            PinSheet(connection: login.connection)
                .environment(model)
        }
        #if os(iOS)
        .capturableSheet(isPresented: $model.showSettings, onDismiss: { model.settingsDismissed() }) {
            NavigationStack { SettingsView() }
                .environment(model)
        }
        #endif
        .alert("Status der Überweisung unklar", isPresented: Binding(get: { model.transferUnknown != nil }, set: { if !$0 { model.transferUnknown = nil } })) {
            Button("Verstanden", role: .cancel) { model.transferUnknown = nil }
        } message: {
            if let u = model.transferUnknown {
                Text(String(format: String(localized: "Der Auftrag hat die Bank erreicht, aber die Antwort ist nicht eindeutig. Die Überweisung wurde möglicherweise bereits ausgeführt.\n\nBitte prüfen Sie zuerst Ihr Konto (Umsätze oder Online-Banking), bevor Sie sie erneut senden. In GiroCrab ist sie als „Status unklar“ vorgemerkt.\n\nReferenz (EndToEnd-ID): %@\nBank: %@"), u.endToEndId, u.reason))
            }
        }
        .alert("Stornierung nicht bestätigt", isPresented: Binding(get: { model.transferCancelNotice != nil }, set: { if !$0 { model.transferCancelNotice = nil } })) {
            Button("Status erneut prüfen") { model.recheckTransferCancel() }
            Button("Schließen", role: .cancel) { model.transferCancelNotice = nil }
        } message: {
            Text(model.transferCancelNotice?.message ?? "")
        }
        .alert(model.errorTitle ?? String(localized: "Etwas ist schiefgelaufen"),
               isPresented: Binding(get: { model.errorMessage != nil }, set: { if !$0 { model.errorMessage = nil } })) {
            if let retry = model.errorRetry {
                Button("Erneut versuchen") { model.errorOnClose = nil; model.errorMessage = nil; retry() }
            }
            ForEach(Array(model.errorAlternatives.enumerated()), id: \.offset) { _, alt in
                Button(alt.title) { model.errorOnClose = nil; model.errorMessage = nil; alt.action() }
            }
            if let close = model.errorCloseTitle {
                Button(close, role: .cancel) { model.errorMessage = nil }
            } else {
                Button("OK", role: .cancel) { model.errorMessage = nil }
            }
        } message: {
            Text(model.errorMessage ?? "")
        }
        // After the upgrade: GiroCrab's own FinTS registration or keep the user's (asked once).
        .alert("Welche FinTS-Registrierung soll GiroCrab verwenden?",
               isPresented: Binding(get: { model.offerProductIDChoice }, set: { _ in })) {
            Button("GiroCrab-Registrierung verwenden (empfohlen)") { model.setFintsProductIDMode("builtin") }
                .keyboardShortcut(.defaultAction)
            Button("Meine eigene behalten") { model.setFintsProductIDMode("own") }
        } message: {
            Text("GiroCrab hat jetzt eine eigene FinTS-Produktregistrierung. Sie müssen dafür nichts mehr tun. Ihre eigene Registrierung bleibt gespeichert, und Sie können jederzeit in den Einstellungen wechseln.")
        }
        // own host view: the "don't show again" toggle must not reach other dialogs
        .background {
            Color.clear
                .alert("Enable-Banking-Anwendung synchronisiert",
                       isPresented: Binding(get: { model.enableBankingAdoptNotice != nil },
                                            set: { if !$0 { model.enableBankingAdoptNotice = nil } }),
                       presenting: model.enableBankingAdoptNotice) { _ in
                    Button("OK") {}
                } message: { notice in
                    Text(notice.message)
                }
                .dialogSuppressionToggle(isSuppressed: $enableBankingNoticeSuppressed)
        }
        .background {
            Color.clear
                .alert("Enable-Banking-Anwendung synchronisiert",
                       // after the default application's notice, never on top of it
                       isPresented: Binding(get: { model.enableBankingLoginNotice != nil && model.enableBankingAdoptNotice == nil },
                                            set: { if !$0 { model.enableBankingLoginNotice = nil } }),
                       presenting: model.enableBankingLoginNotice) { _ in
                    Button("OK") {}
                } message: { notice in
                    Text(notice.message)
                }
                .dialogSuppressionToggle(isSuppressed: $enableBankingNoticeSuppressed)
        }
        .toastOverlay()
    }
}

/// A securities depot and a Sachwerte account each have their own view on the shared holdings frame
/// (`AssetAccountFrame`); everything else shows bookings. `.id` keeps no state of another account.
/// A view of its own so that only it follows the selected account, not the whole split view.
private struct AccountColumn: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        switch model.presentation {
        case .securities:
            if let depot = model.selectedSecuritiesDepot { DepotDetailView(account: depot).id(depot.id) }
        case .tangible:
            if let account = model.selectedTangibleAccount { TangibleAccountDetailView(account: account).id(account.id) }
        case .bookings:
            BookingStreamView()
        }
    }
}

/// Full-window background of the Mac start and lock screen: soft light surface (#F8FAFC to #EEF2F6)
/// in Light Mode, deep slate navy (#0B1120 to #162033) in Dark Mode.
struct MacLaunchBackground: View {
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        LinearGradient(
            colors: scheme == .dark
                ? [Color(hex: 0x0B1120), Color(hex: 0x162033)]
                : [Color(hex: 0xF8FAFC), Color(hex: 0xEEF2F6)],
            startPoint: .top, endPoint: .bottom
        )
        .ignoresSafeArea()
    }
}

/// Hero icon for start and lock screen in a Mac window: the cut-out crab (no app-icon squircle),
/// 270 pt wide, with a soft contact shadow; in Dark Mode with a subtle ambient glow.
struct MacHeroIcon: View {
    var phase: AnimatedCrabLogoView.Phase
    var size: CGFloat = 270
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        let dark = scheme == .dark
        ZStack {
            if dark {
                Circle()
                    .fill(RadialGradient(
                        colors: [Color(hex: 0x3B4C6B, opacity: 0.45), Color(hex: 0x3B4C6B, opacity: 0)],
                        center: .center, startRadius: size * 0.1, endRadius: size * 0.95))
                    .frame(width: size * 2, height: size * 2)
                    .allowsHitTesting(false)
            }
            AnimatedCrabLogoView(size: size, phase: phase, showBackgroundSquircle: false)
                .shadow(color: .black.opacity(dark ? 0.45 : 0.20), radius: size * 0.074, x: 0, y: size * 0.063)
                .shadow(color: .black.opacity(dark ? 0.30 : 0.10), radius: size * 0.016, x: 0, y: size * 0.011)
        }
        .frame(width: size * 1.3, height: size * 1.3)
    }
}

#if os(macOS)
import AppKit

/// Bearbeiten › Suchen … (⌘F): puts the cursor into the window's toolbar search
/// field (`.searchable(placement: .toolbar)`; `searchFocused` needs macOS 15).
enum ToolbarSearch {
    @MainActor
    static func focus() {
        let window = NSApp.keyWindow ?? NSApp.mainWindow ?? NSApp.windows.first { $0.toolbar != nil }
        // A view without search has no search item: ⌘F does nothing there.
        guard let window, let item = window.toolbar?.items.compactMap({ $0 as? NSSearchToolbarItem }).first else {
            FileHandle.standardError.write("ToolbarSearch: no search field\n".data(using: .utf8)!)
            return
        }
        window.makeFirstResponder(item.searchField)
    }
}
#endif

