import SwiftUI
import GiroCrabCore
#if os(macOS)
import AppKit
#endif

/// Einstellungen: Bankzugänge, eigene FinTS-Produkt-ID und API-Schlüssel
/// (im verschlüsselten SQLCipher-Tresor), Händler-Logos, Sicherheit.
struct SettingsView: View {
    @Environment(AppModel.self) private var model
    @State private var showKeySheet = false

    var body: some View {
        @Bindable var model = model
        TabView(selection: $model.settingsTab) {
            GeneralSettings()
                .settingsPane(height: 600)   // "Sprache" … "Händler-Logos" + "Demo": scrolls
                .tabItem { Label("Allgemein", systemImage: "gearshape") }
                .tag(SettingsTab.general)
            BankConnectionsSettings()
                .settingsPane(height: 600)
                .disabled(!model.isUnlocked)
                .tabItem { Label("Banken", systemImage: "building.columns") }
                .tag(SettingsTab.banks)
            #if os(macOS)
            DocumentFolderSettings()
                .settingsPane(height: 420)
                .disabled(!model.isUnlocked)
                .tabItem { Label("Dokumente", systemImage: "doc.on.doc") }
                .tag(SettingsTab.documents)
            #endif
            SecuritySettings(showKeySheet: $showKeySheet)
                .settingsPane(height: 760)   // incl. "Sicherheitssperre für Bankverbindungen"
                .disabled(!model.isUnlocked)
                .tabItem { Label("Sicherheit", systemImage: "lock.shield") }
                .tag(SettingsTab.security)
            #if os(macOS)
            ExternalAccessSettings()
                .settingsPane(height: 640)
                .disabled(!model.isUnlocked)
                .tabItem { Label("Analyse", systemImage: "chart.bar.doc.horizontal") }
                .tag(SettingsTab.externalAccess)
            // Own pane like macOS "Softwareupdate"; works without an open vault.
            // Only in the Developer-ID build (Sparkle registered).
            if AppUpdates.shared.isAvailable {
                UpdateSettings()
                    .settingsPane(height: 360)
                    .tabItem { Label("Updates", systemImage: "arrow.down.circle") }
                    .tag(SettingsTab.updates)
            }
            #endif
        }
        #if os(iOS)
        .navigationTitle("Einstellungen")
        .navigationBarTitleDisplayMode(.inline)
        #endif
    }
}

/// Allgemein: app language (#93). Usable without an open vault.
struct GeneralSettings: View {
    @Environment(AppModel.self) private var model
    @State private var showHelp = false
    @AppStorage(ViewLayout.statisticsKey) private var showStatistics = true
    @AppStorage(ViewLayout.budgetsKey) private var showBudgets = true
    @AppStorage(ViewLayout.runningBalanceKey) private var showRunningBalance = false
    @AppStorage(ViewLayout.lastFetchKey) private var showLastFetch = false
    @AppStorage(NegativeAmountStyle.defaultsKey) private var negativeAmountStyle: NegativeAmountStyle = .neutral
    @AppStorage(AppTextSize.defaultsKey) private var textSize: AppTextSize = .standard
    @AppStorage(PrivacyMode.enabledKey) private var privacyMode = false
    @AppStorage(NaturalSearch.defaultsKey) private var naturalSearch = false
    @AppStorage(PrivacyMode.scopeKey) private var privacyScope = PrivacyMode.defaultScope
    @AppStorage(AppAppearance.defaultsKey) private var appearance: AppAppearance = .system
    @AppStorage(BookingDensity.defaultsKey) private var bookingDensity: BookingDensity = .standard

    /// Core setting `sync.days_back`: how far the first fetch of a new account reaches back.
    private var firstFetchDays: Binding<Int> {
        Binding(
            get: { Int(model.settingEntry("sync.days_back")?.value ?? "") ?? 90 },
            set: { model.saveSetting("sync.days_back", String($0)) }
        )
    }

    var body: some View {
        @Bindable var model = model
        Form {
            #if os(iOS)
            // The Mac has these in its menus (Hilfe › GiroCrab-Hilfe, Ansicht › Demo-Daten anzeigen).
            Section {
                Button { showHelp = true } label: {
                    Label("Hilfe & FAQ", systemImage: "questionmark.circle")
                }
                BugReportButton()
            }
            #endif
            LanguageSettingsSection()
            AnnouncementsSettingsSection()
            Section {
                Picker("Erscheinungsbild", selection: $appearance) {
                    ForEach(AppAppearance.allCases) { Text($0.title).tag($0) }
                }
                Text("Hell oder Dunkel gilt nur für GiroCrab; „System“ folgt der Einstellung des Systems.")
                    .appFont(.caption).foregroundStyle(.secondary)
                Picker("Buchungsdichte", selection: $bookingDensity) {
                    ForEach(BookingDensity.allCases) { Text($0.title).appFont(.body).tag($0) }
                }
                Text("Kompakt öffnet die aktive Buchung mit allen Details. Standard zeigt einen kurzen Verwendungszweck. Großzügig zeigt alle Buchungen ausführlich.")
                    .appFont(.caption).foregroundStyle(.secondary)
                #if os(macOS)
                // iOS: the system's Dynamic Type setting (Einstellungen › Anzeige & Helligkeit).
                Picker("Schriftgröße", selection: $textSize) {
                    ForEach(AppTextSize.allCases) { Text($0.title).tag($0) }
                }
                Text("Vergrößert die Schrift in der ganzen App. Auch im Menü Ansicht mit ⌘+ / ⌘− / ⌘0.")
                    .appFont(.caption).foregroundStyle(.secondary)
                #endif
                Toggle("Privatmodus", isOn: $privacyMode)
                Picker("Im Privatmodus ausblenden", selection: $privacyScope) {
                    ForEach(PrivacyMode.Scope.allCases) { Text($0.title).tag($0) }
                }
                Text("Blendet Beträge, Namen, Kontonummern und Verwendungszwecke aus, wenn du die App jemandem zeigst. Auf Wunsch nur die Beträge.")
                    .appFont(.caption).foregroundStyle(.secondary)
                if NaturalSearch.isAvailable {
                    Toggle("Suche in natürlicher Sprache", isOn: $naturalSearch)
                        .help("Versteht Angaben wie „Lebensmittel letzten Monat über 50 €“. Läuft vollständig auf diesem Gerät; an das Modell gehen nur Suchtext, heutiges Datum und Kategorienamen.")
                }
                Toggle("Auswertungen in der Übersicht anzeigen", isOn: $showStatistics)
                Text("Blendet Saldoverlauf, Cashflow und Budgets über der Buchungsliste aus. Auch im Menü Ansicht umschaltbar.")
                    .appFont(.caption).foregroundStyle(.secondary)
                Toggle("Budgets anzeigen", isOn: $showBudgets)
                Text("Wer keine Monatsbudgets nutzt, blendet die Budget-Karte und „Monatsbudget festlegen …“ ganz aus. Auch im Menü Ansicht umschaltbar.")
                    .appFont(.caption).foregroundStyle(.secondary)
                Toggle("Saldo in der Buchungsliste anzeigen", isOn: $showRunningBalance)
                Text("Zeigt den Kontostand nach jeder Buchung und am Ende jedes Tages, zum Abgleich mit dem Kontoauszug. Bei mehreren Konten die Summe der Euro-Konten ohne Depots und Darlehen. Innerhalb eines Tages kann die Reihenfolge von der Bank abweichen, der Tagessaldo stimmt immer. Auch im Menü Ansicht umschaltbar.")
                    .appFont(.caption).foregroundStyle(.secondary)
                Toggle("Letzten Abruf anzeigen", isOn: $showLastFetch)
                Text("Zeigt unter dem Kontostand, wann die Bankverbindung des Kontos zuletzt abgerufen wurde. Der Zeitpunkt gilt je Bankverbindung, Konten derselben Bank zeigen also denselben Wert. Nur in der Ansicht eines einzelnen Kontos und bei ausgeklapptem Kopfbereich. Auch im Menü Ansicht umschaltbar.")
                    .appFont(.caption).foregroundStyle(.secondary)
                Picker("Negative Beträge", selection: $negativeAmountStyle) {
                    ForEach(NegativeAmountStyle.allCases) { Text($0.title).tag($0) }
                }
                Text("Farbe von Abbuchungen in der Buchungsliste: neutral wie der übrige Text, rot wie negative Salden in der Seitenleiste oder kontraststark (orange und blau, auch bei Rot-Grün-Schwäche gut unterscheidbar). Das Vorzeichen bleibt immer sichtbar.")
                    .appFont(.caption).foregroundStyle(.secondary)
            } header: { Text("Ansicht").appSectionHeader() }
            Section {
                Picker("Als geprüft markieren", selection: $model.autoReviewDelay) {
                    ForEach(AutoReviewDelay.allCases) { Text($0.title).tag($0) }
                }
                Text(model.autoReviewDelay.helpText)
                    .appFont(.caption).foregroundStyle(.secondary)
                Picker("Zeitraum beim ersten Abruf", selection: firstFetchDays) {
                    Text("90 Tage").tag(90)
                    Text("1 Jahr").tag(365)
                    Text("2 Jahre").tag(730)
                    // a value set elsewhere (e.g. synced from another device) stays selectable
                    if ![90, 365, 730].contains(firstFetchDays.wrappedValue) {
                        Text("\(firstFetchDays.wrappedValue) Tage").tag(firstFetchDays.wrappedValue)
                    }
                }
                Text("Wie weit GiroCrab beim ersten Abruf eines neuen Kontos zurückgeht. Viele Banken liefern nur 90 Tage oder verlangen für mehr eine TAN. Bereits abgerufene Konten betrifft das nicht.")
                    .appFont(.caption).foregroundStyle(.secondary)
            } header: { Text("Buchungen").appSectionHeader() }
            TransferSettingsSection()
            BalanceChartSettingsSection()
            MerchantLogoSettingsSection()
            #if os(macOS)
            AutoSyncSettingsSection()
            DockAndMenuBarSettingsSection()
            #endif
            #if os(iOS)
            Section {
                Button {
                    model.loadDemoData()
                    model.settingsRequested = false
                } label: {
                    Label("Demo-Tresor / Demo-Daten laden", systemImage: "play.circle")
                }
                Text("Lädt einen Wegwerf-Tresor mit fiktiven Konten bei sieben Banken. „Alle abrufen“, TAN-Dialoge und Überweisungen laufen simuliert, ohne Netzwerk. Ihre eigenen Daten bleiben unberührt.")
                    .appFont(.caption).foregroundStyle(.secondary)
            } header: { Text("Demo").appSectionHeader() }
            #endif
        }
        .formStyle(.grouped)
        #if os(iOS)
        .capturableSheet(isPresented: $showHelp) { HelpSheet() }
        #endif
    }
}

/// Überweisungen: opt-in clipboard check when the form opens (#663), per device.
struct TransferSettingsSection: View {
    @AppStorage(ClipboardPaymentCheck.enabledKey) private var clipboardCheck = ClipboardPaymentCheck.enabledDefault

    var body: some View {
        Section {
            Toggle("Zwischenablage beim Öffnen einer Überweisung prüfen", isOn: $clipboardCheck)
            Text("Nur bei leerem Formular. Der Inhalt wird nur auf diesem Gerät gelesen, nie gespeichert oder protokolliert. Felder werden erst nach Bestätigung im Hinweis ausgefüllt.")
                .appFont(.caption).foregroundStyle(.secondary)
        } header: { Text("Überweisungen").appSectionHeader() }
    }
}

/// Bar width of the balance history in the header (GitHub #203); the chart type itself
/// is chosen below the chart (#634).
struct BalanceChartSettingsSection: View {
    @AppStorage(BalanceBarUnit.defaultsKey) private var barUnit: BalanceBarUnit = .automatic

    var body: some View {
        Section {
            Picker("Balken je", selection: $barUnit) {
                ForEach(BalanceBarUnit.balanceCases) { Text($0.title).tag($0) }
            }
            Text("Diagrammart (Linie, Balken, Kreis) und Balkenbreite wählen Sie auch direkt unter dem Diagramm. Balken zeigen den durchschnittlichen Saldo je Monat (bei kurzen Zeiträumen je Tag, ab sechs Jahren je Jahr) und machen Trends und Jahre vergleichbar. Gestrichelte Linien markieren den Jahreswechsel.")
                .appFont(.caption).foregroundStyle(.secondary)
        } header: { Text("Saldoverlauf").appSectionHeader() }
    }
}

/// Tabs of the settings window (macOS) / sheet (iOS).
enum SettingsTab: Hashable {
    case general, banks, documents, security, externalAccess, updates

    /// `--demo-settings <name>` (screenshots).
    init?(demoName: String) {
        switch demoName {
        case "general": self = .general
        case "banks": self = .banks
        case "credentials": self = .banks   // FinTS registration now lives under "Banken"
        case "logos": self = .general   // merchant logos are a section of "Allgemein"
        case "documents": self = .documents
        case "security": self = .security
        case "external": self = .externalAccess
        case "updates": self = .updates
        default: return nil
        }
    }
}

struct BankConnectionsSettings: View {
    @Environment(\.reportRedaction) private var redaction
    @Environment(AppModel.self) private var model
    @State private var showAddBank = false
    @State private var editing: BankConnection?
    @State private var confirmRemove: BankConnection?

    var body: some View {
        Form {
            if model.demoActive {
                Section {
                    Label(model.hasOwnVault
                          ? "Sie sehen gerade Demo-Daten. Neue Bankzugänge werden in Ihrem eigenen Tresor angelegt – die App wechselt dabei automatisch dorthin."
                          : "Demo-Modus: Bankzugänge werden hier nur simuliert. Für echte Konten zuerst den eigenen Tresor öffnen.",
                          systemImage: "theatermasks")
                        .appFont(.callout)
                        .foregroundStyle(.secondary)
                    if !model.hasOwnVault {
                        Button("Eigenen Tresor öffnen") { model.showOwnVault() }
                    }
                }
            }
            Section {
                if model.connections.isEmpty {
                    Text("Noch keine Bank hinzugefügt.").foregroundStyle(.secondary)
                }
                ForEach(model.connections) { c in
                    HStack(spacing: 10) {
                        RoundedRectangle(cornerRadius: 8, style: .continuous)
                            .fill(c.tint.opacity(0.12))
                            .overlay(Text(c.monogram).appFont(.caption, weight: .bold).foregroundStyle(c.tint).sensitive())
                            .frame(width: 30, height: 30)
                        VStack(alignment: .leading, spacing: 1) {
                            Text(c.displayName).fontWeight(.medium).sensitive()
                            Text(protocolName(c.connector) + (c.loginName.isEmpty ? "" : " · \(redaction.masking(c.loginName))"))
                                .appFont(.caption).foregroundStyle(.secondary)
                        }
                        Spacer()
                        if let t = c.lastSyncUnix {
                            Text(Date(timeIntervalSince1970: TimeInterval(t)), style: .relative)
                                .appFont(.caption).foregroundStyle(.secondary)
                        }
                        Button { editing = c } label: { Image(systemName: "slider.horizontal.3") }
                            .buttonStyle(.borderless)
                            .help("Bearbeiten / Details")
                            .accessibilityLabel("Bearbeiten / Details")
                        Button { model.requestFullSync(c) } label: { Image(systemName: "arrow.triangle.2.circlepath") }
                            .buttonStyle(.borderless)
                            .help(model.bankingGate(for: c)?.message ?? String(localized: "Jetzt abrufen"))
                            .accessibilityLabel("Jetzt abrufen")
                            .accessibilityHint(Text(verbatim: model.bankingGate(for: c)?.message ?? ""))
                            .disabled(model.bankingGate(for: c) != nil)
                        Button(role: .destructive) { confirmRemove = c } label: { Label("Bankzugang entfernen …", systemImage: "trash") }
                            .labelStyle(.iconOnly)
                            .buttonStyle(.borderless)
                            .help("Bankzugang entfernen …")
                    }
                    .contentShape(Rectangle())
                    .onTapGesture { editing = c }
                }
            } header: { Text("Verbundene Banken").appSectionHeader() }
            Section {
                Button { showAddBank = true } label: {
                    Label("Bank suchen und hinzufügen …", systemImage: "magnifyingglass")
                }
                Text("Suche nach Bankname, BLZ oder IBAN: FinTS-Banken, Banken mit eigener API (bunq, comdirect, Trade Republic, PayPal) und PSD2-Banken (N26, Revolut, C24 …).")
                    .appFont(.caption).foregroundStyle(.secondary)
            } header: { Text("Bank hinzufügen").appSectionHeader() }
            fintsRegistrationSection
        }
        .formStyle(.grouped)
        .capturableSheet(isPresented: $showAddBank) {
            AddBankSheet()
                .environment(model)
        }
        .capturableSheet(item: $editing) { c in
            ConnectionEditSheet(connection: c)
                .environment(model)
        }
        .confirmRemoveConnection($confirmRemove)
        .task {
            // --demo-settings banks:edit / banks:add (screenshots)
            switch model.demoSettingsSheet {
            case "edit": editing = model.connections.first
            case "select": editing = model.connections.last
            case "add": showAddBank = true
            case "remove": confirmRemove = model.connections.first
            default: break
            }
            model.demoSettingsSheet = nil
        }
    }

    /// Which FinTS product registration is used for all FinTS banks. The server
    /// address and the keys of the other connectors are edited per bank
    /// (sheet "Bearbeiten / Details").
    @ViewBuilder private var fintsRegistrationSection: some View {
        Section {
            if model.fintsBuiltinAvailable {
                Label("Offizielle FinTS-Produktregistrierung von GiroCrab aktiv", systemImage: "checkmark.seal.fill")
                    .appFont(.callout)
                    .foregroundStyle(.secondary)
            } else {
                if AppUpdateChannel.current != .stable {
                    Label("Dieser Beta-Build enthält die FinTS-Registrierung von GiroCrab nicht. Bitte melden Sie das den Entwicklern.", systemImage: "exclamationmark.triangle")
                        .appFont(.callout)
                        .foregroundStyle(Theme.warning)
                }
                // Own product ID is only needed in self-built / developer binaries without embedded ID (#1092).
                OwnProductIDField()
            }
        } header: {
            Text("FinTS-Registrierung").appSectionHeader()
        } footer: {
            Text(model.fintsBuiltinAvailable
                 ? "Das FinTS-Produkt von GiroCrab ist eingebaut. Eigene API-Anwendungen (zum Beispiel für comdirect oder PayPal) registrieren Sie selbst; die Werte liegen nur in Ihrem verschlüsselten Tresor."
                 : "Nichts davon wird mit dieser Version ausgeliefert. Registrieren Sie Ihr eigenes FinTS-Produkt und Ihre eigenen API-Anwendungen; die Werte liegen nur in Ihrem verschlüsselten Tresor.")
                .appFont(.footnote)
                .foregroundStyle(.secondary)
                .leadingFooter()
        }
    }

    private func protocolName(_ k: ConnectorKind) -> String {
        bankCatalog().first { $0.kind == k }?.protocol ?? ""
    }
}

/// Merchant logos from the internet (GitHub #33); needs an open vault.
struct MerchantLogoSettingsSection: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        Section {
            Picker("Logos aus dem Internet laden", selection: Binding(
                get: { model.logoProvider },
                // `logoProvider` follows once the core has stored it: the core gates
                // cached logos on the same setting, so an earlier switch would load nothing (#422).
                set: { model.saveSettingInBackground("merchants.online_logos", $0) }
            )) {
                Text("Aus").tag("off")
                Text("DuckDuckGo").tag("duckduckgo")
                Text("Google").tag("google")
            }
            Button("Logo-Cache leeren") { model.clearLogoCache() }
        } header: {
            Text("Händler-Logos").appSectionHeader()
        } footer: {
            Text("Ohne Anbieter zeigt GiroCrab mitgelieferte Markenzeichen, sonst Monogramme und Kategorie-Symbole. Mit Anbieter werden Favicons geladen und verschlüsselt im Tresor zwischengespeichert. Übertragen wird nur die Händler-Domain (z. B. lidl.de) – keine Beträge, keine Kontodaten.")
                .appFont(.footnote)
                .foregroundStyle(.secondary)
                .leadingFooter()
        }
        .disabled(!model.isUnlocked)
    }
}

#if os(macOS)
/// Automatic copy of all documents (statements, receipts, PostBox) into a
/// folder, e.g. in iCloud Drive (GitHub #57).
struct DocumentFolderSettings: View {
    @Environment(AppModel.self) private var model
    @State private var choosing = false

    var body: some View {
        Form {
            Section {
                Toggle("Dokumente automatisch in einen Ordner sichern", isOn: Binding(
                    get: { model.documentMirrorPath != nil },
                    set: { on in
                        if !on { model.setDocumentMirror(nil) }
                        else if let url = AppModel.suggestedMirrorURL { model.setDocumentMirror(url) }
                        else { choosing = true }
                    }
                ))
                if let path = model.documentMirrorPath {
                    LabeledContent("Ordner") {
                        HStack {
                            Text(displayPath(path))
                                .lineLimit(1)
                                .truncationMode(.middle)
                                .sensitive() // the path names the user
                                .help(path)
                            Button("Ändern …") { choosing = true }
                            Button("Im Finder zeigen") {
                                NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: path)])
                            }
                        }
                    }
                    LabeledContent("Status") {
                        HStack {
                            Text(model.documentMirrorStatus ?? "–").foregroundStyle(.secondary)
                            Button("Jetzt sichern") { model.mirrorDocumentsIfEnabled() }
                        }
                    }
                }
                Text("Kontoauszüge, Belege und PostBox-Dokumente werden nach jedem Sync als Kopie abgelegt: <Ordner>/<Konto>/<Jahr>/. Vorhandene Dateien werden nie überschrieben. Achtung: Im Ordner liegen die PDFs unverschlüsselt – in iCloud Drive also auch in Ihrer iCloud.")
                    .appFont(.caption).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            } header: { Text("Dokumentenordner").appSectionHeader() }
            Section {
                Text("Ob GiroCrab die PDF-Kontoauszüge eines FinTS-Kontos bei jedem Sync mit abruft, stellen Sie pro Konto unter „Dokumente / Kontoauszüge“ ein.")
                    .appFont(.caption).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            } header: { Text("Automatischer Abruf").appSectionHeader() }
        }
        .formStyle(.grouped)
        .fileImporter(isPresented: $choosing, allowedContentTypes: [.folder]) { result in
            if case .success(let url) = result { model.setDocumentMirror(url) }
        }
    }

    /// "~/Library/Mobile Documents/com~apple~CloudDocs/GiroCrab" -> "iCloud Drive/GiroCrab".
    private func displayPath(_ path: String) -> String {
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        let cloud = home + "/Library/Mobile Documents/com~apple~CloudDocs"
        if path.hasPrefix(cloud) { return "iCloud Drive" + path.dropFirst(cloud.count) }
        if path.hasPrefix(home) { return "~" + path.dropFirst(home.count) }
        return path
    }
}
#endif

struct SecuritySettings: View {
    @Environment(AppModel.self) private var model
    @State private var requireBiometrics = VaultKeyManager.shared.requireBiometrics
    /// Read once per vault: the core call takes the vault mutex, so not in `body`.
    @State private var vaultPath = "–"
    @State private var confirmReset = false
    @Binding var showKeySheet: Bool

    var body: some View {
        Form {
            Section {
                LabeledContent("Verschlüsselung", value: "SQLCipher 4 (AES-256)")
                LabeledContent("Hauptschlüssel", value: model.isDemo ? String(localized: "Demo-Schlüssel (Wegwerf)") : String(localized: "Schlüsselbund, geschützt durch \(VaultKeyManager.shared.biometryName)"))
                #if os(macOS)
                if !model.isDemo {
                    Toggle("Beim Entsperren \(VaultKeyManager.shared.biometryName) verlangen", isOn: $requireBiometrics)
                        .onChange(of: requireBiometrics) { _, on in
                            guard on != VaultKeyManager.shared.requireBiometrics else { return }
                            Task {
                                do { try await VaultKeyManager.shared.setRequireBiometrics(on) }
                                catch {
                                    requireBiometrics = VaultKeyManager.shared.requireBiometrics
                                    model.toast = error.localizedDescription
                                }
                            }
                        }
                }
                #endif
                LabeledContent("Datei") { Text(vaultPath).sensitive() } // the path names the user
                    .textSelection(.enabled)
                LabeledContent("Version", value: AppVersionDisplay.current(core: coreVersion()))
                    .textSelection(.enabled)
            } header: { Text("Tresor").appSectionHeader() }
            if model.hasOwnVault { VaultBackupSection() }
            ICloudSyncSection(showKeySheet: $showKeySheet)
            BankingPolicySection()
            Section {
                Text("GiroCrab verbindet sich direkt von diesem Gerät mit Ihren Banken. Es gibt keinen GiroCrab-Server und kein Tracking. Nur wenn Sie den iCloud-Sync oben einschalten, liegt der verschlüsselte Tresor in Ihrem iCloud Drive. PINs und TANs liegen nur für einen Abruf im Speicher und werden danach gelöscht.")
                    .appFont(.callout).foregroundStyle(.secondary)
            } header: { Text("Datenschutz").appSectionHeader() }
            if model.hasOwnVault && !model.isDemo {
                Section {
                    Button("Jetzt sperren", systemImage: "lock.fill") { model.lock() }
                }
            }
            Section {
                Button("App zurücksetzen …", systemImage: "trash", role: .destructive) { confirmReset = true }
                    .disabled(model.isResettingApp)
                Text("Löscht Tresor, Schlüssel, Einstellungen und Zwischenspeicher auf diesem Gerät und beginnt neu. Das lässt sich nicht rückgängig machen.")
                    .appFont(.callout).foregroundStyle(.secondary)
            } header: { Text("Zurücksetzen").appSectionHeader() }
        }
        .formStyle(.grouped)
        // Present from the stable Form, not a Section that iOS distributes
        // across its rows. Otherwise opening this nested sheet can tear down
        // the Settings sheet as well (#768).
        .capturableSheet(isPresented: $showKeySheet) {
            KeyPasteSheet().environment(model)
        }
        .task(id: model.isDemo) { vaultPath = model.vaultFilePath ?? "–" }
        .confirmationDialog("GiroCrab zurücksetzen?", isPresented: $confirmReset, titleVisibility: .visible) {
            Button("Alles löschen", role: .destructive) { Task { await model.resetApp() } }
            Button("Abbrechen", role: .cancel) {}
        } message: {
            Text("Der Tresor mit allen Konten und Buchungen, der Tresor-Schlüssel (auch im iCloud-Schlüsselbund und damit auf Ihren anderen Geräten, z. B. am Mac: dort angelegte Tresore und iCloud-Drive-Sicherungen sind dann nicht mehr lesbar), Ihr Enable-Banking-Zugang und alle Einstellungen werden gelöscht. Ohne eigenes Backup sind die Daten verloren. Zugriffe bei Ihren Banken und Dokumente in Ihrem Dokumentenordner bleiben bestehen. Zur Bestätigung werden Sie nach \(VaultKeyManager.shared.biometryName) oder Ihrem Gerätecode gefragt.")
        }
    }
}

#if os(macOS)
/// Einstellungen › Updates (nur im Developer-ID-Build mit Sparkle; im App-Store-
/// oder Xcode-Build ist kein Updater registriert und der Tab fehlt). Bedienbar
/// auch ohne geöffneten Tresor.
///
/// Die Kanäle Beta und Nightly sind bewusst versteckt: Die Auswahl erscheint
/// erst, wenn bei geöffnetem Tab die Wahltaste (⌥) gedrückt wird, und bleibt
/// dann sichtbar, bis die Einstellungen geschlossen werden – oder immer, wenn
/// schon ein Vorabkanal aktiv ist, damit man ihn jederzeit verlassen kann. Der
/// Wechsel in einen weniger stabilen Kanal muss bestätigt werden; „Abbrechen“
/// ist dabei die Standardtaste (HIG: riskante Aktion nie als Default).
struct UpdateSettings: View {
    @Bindable private var updates = AppUpdates.shared
    @State private var channelRevealed = NSEvent.modifierFlags.contains(.option)
    @State private var flagsMonitor: Any?
    @State private var pendingChannel: AppUpdateChannel?

    private var showsChannelPicker: Bool {
        channelRevealed || updates.channel != .stable
    }

    var body: some View {
        Form {
            Section {
                Toggle("Automatisch nach Updates suchen", isOn: $updates.automaticallyChecksForUpdates)
                Toggle("Updates automatisch laden und installieren", isOn: $updates.automaticallyDownloadsUpdates)
                    .disabled(!updates.automaticallyChecksForUpdates || !updates.allowsAutomaticUpdates)
            }
            Section {
                if showsChannelPicker {
                    Picker(selection: Binding(
                        get: { updates.channel },
                        set: { new in
                            if new.risk > updates.channel.risk { pendingChannel = new } else { updates.setChannel(new) }
                        }
                    )) {
                        Text("Stabil").tag(AppUpdateChannel.stable)
                        Text("Beta").tag(AppUpdateChannel.beta)
                        Text("Nightly").tag(AppUpdateChannel.nightly)
                    } label: {
                        if updates.channel == .stable {
                            Text("Update-Kanal")
                        } else {
                            Label("Update-Kanal", systemImage: "exclamationmark.triangle.fill")
                                .foregroundStyle(Theme.warning)
                        }
                    }
                }
                LabeledContent("Zuletzt gesucht") {
                    if let date = updates.lastUpdateCheckDate {
                        Text(date, format: .relative(presentation: .named))
                    } else {
                        Text("Noch nie")
                    }
                }
                Button("Nach Updates suchen …") { updates.checkForUpdates() }
                    .disabled(!updates.canCheckForUpdates)
            } footer: {
                Group { switch updates.channel {
                case .stable:
                    Text("Updates werden nur installiert, wenn sie mit dem GiroCrab-Schlüssel signiert und von Apple notarisiert sind.")
                case .beta:
                    Text("Beta-Versionen sind Vorabversionen zum Testen. Neuere stabile Versionen erhalten Sie weiterhin. Ein Wechsel zurück installiert keine ältere Version.")
                case .nightly:
                    Text("Nightly-Builds sind ungetestete Entwicklungsstände. Neuere Beta- und stabile Versionen erhalten Sie weiterhin. Ein Wechsel zurück installiert keine ältere Version.")
                } }
                .leadingFooter()
            }
        }
        .formStyle(.grouped)
        .alert(
            pendingChannel == .nightly ? "Nightly-Builds erhalten?" : "Beta-Versionen erhalten?",
            isPresented: Binding(get: { pendingChannel != nil }, set: { if !$0 { pendingChannel = nil } }),
            presenting: pendingChannel
        ) { channel in
            Button(channel == .nightly ? "Nightly-Builds aktivieren" : "Beta-Versionen aktivieren", role: .destructive) {
                updates.setChannel(channel)
            }
            Button("Abbrechen", role: .cancel) {}
                .keyboardShortcut(.defaultAction)
        } message: { channel in
            if channel == .nightly {
                Text("Nightly-Builds entstehen aus dem aktuellen Entwicklungsstand und sind nicht getestet. Sie können Fehler enthalten oder Ihre Daten so verändern, dass ältere Versionen sie nicht mehr lesen können. Legen Sie vorher eine Sicherung Ihres Tresors an.")
            } else {
                Text("Beta-Versionen sind Vorabversionen zum Testen. Sie können Fehler enthalten oder Ihre Daten so verändern, dass ältere Versionen sie nicht mehr lesen können. Legen Sie vorher eine Sicherung Ihres Tresors an.")
            }
        }
        .onAppear {
            flagsMonitor = NSEvent.addLocalMonitorForEvents(matching: .flagsChanged) { event in
                if event.modifierFlags.contains(.option) { channelRevealed = true }
                return event
            }
        }
        .onDisappear {
            if let flagsMonitor { NSEvent.removeMonitor(flagsMonitor) }
            flagsMonitor = nil
            channelRevealed = false
        }
    }
}
#endif

/// Kompakter iCloud-Drive-Sync (nur die verschlüsselte Tresordatei).
struct ICloudSyncSection: View {
    @Environment(AppModel.self) private var model
    @State private var sync = CloudSyncManager.shared
    @State private var picking = false
    @State private var pendingFolder: URL?
    @State private var wrongFolderHint: String?
    @State private var keychainSync = VaultKeyManager.shared.syncViaICloudKeychain
    @Binding var showKeySheet: Bool
    /// When the copied key leaves the clipboard; the button counts down to it (#909).
    @State private var keyCopiedUntil: Date?
    @State private var confirmKeyCopy = false
    /// Shown in the section: a toast would land in the main window, behind Settings on macOS.
    @State private var keyCopyError: String?

    var body: some View {
        Section {
            Toggle("Tresor über iCloud synchronisieren", isOn: Bindable(sync).isEnabled)
                .onChange(of: sync.isEnabled) { _, on in
                    guard on else { return }
                    Task {
                        // Share the vault key via iCloud Keychain (not as a file in iCloud Drive).
                        if !keychainSync {
                            if (try? await VaultKeyManager.shared.setICloudKeychainSync(true)) != nil { keychainSync = true }
                        }
                        model.syncNow()
                    }
                }
            if sync.isEnabled {
                LabeledContent("Ordner") { Text(sync.folderName ?? "iCloud Drive / GiroCrab").sensitive() }
                if sync.folderNotice {
                    Text("Der Ordner „GiroCrab“ in iCloud Drive enthält bereits andere Dateien. Ihre bisherigen Daten bleiben unverändert im bisherigen Ordner und werden dort weiter synchronisiert.")
                        .appFont(.caption).foregroundStyle(Theme.warning)
                }
                if sync.legacyFolderWritten {
                    Text("Ein anderes Gerät nutzt noch eine ältere GiroCrab-Version und schreibt in den alten Ordner. Bitte alle Geräte aktualisieren.")
                        .appFont(.caption).foregroundStyle(Theme.warning)
                }
                if sync.folderBothVaults {
                    Text("Der bisherige Ordner und der Ordner „GiroCrab“ in iCloud Drive enthalten beide einen Tresor. GiroCrab wechselt nicht automatisch und nutzt weiter den bisherigen Ordner. Bitte prüfen Sie, welcher Tresor aktuell ist, bevor Sie Daten zusammenführen.")
                        .appFont(.caption).foregroundStyle(Theme.warning)
                }
                if sync.repickFolderHint {
                    Label("Die Daten wurden in den Ordner „GiroCrab“ in iCloud Drive verschoben. Bitte wechseln Sie über „Ordner wählen …“ zu diesem Ordner, damit alle Geräte denselben Ordner nutzen.", systemImage: "exclamationmark.triangle.fill")
                        .appFont(.callout).foregroundStyle(Theme.warning)
                }
                if sync.location == .folder {
                    HStack {
                        Button("Ordner wählen …", systemImage: "folder.badge.gearshape") { picking = true }
                        #if os(macOS)
                        Button("Im Finder zeigen", systemImage: "folder") { revealFolder() }
                        #endif
                    }
                    if sync.folderChosen && sync.containerAvailable {
                        Button("Wieder über iCloud synchronisieren", systemImage: "icloud") {
                            sync.useContainerAgain(); model.syncNow()
                        }
                    }
                } else {
                    // The app's own container is invisible in iCloud Drive: a device
                    // without one (e.g. a Mac) syncs via the folder "GiroCrab" instead (#484).
                    Button("Ordner wählen …", systemImage: "folder.badge.gearshape") { picking = true }
                    Text("Nutzt ein anderes Gerät (etwa die Mac-App) den Ordner „GiroCrab“ in iCloud Drive, wählen Sie diesen Ordner, damit beide denselben Tresor sehen.")
                        .appFont(.caption).foregroundStyle(.secondary)
                }
                Button(sync.isSyncing ? LocalizedStringKey("Synchronisiere …") : LocalizedStringKey("Jetzt synchronisieren"), systemImage: "arrow.triangle.2.circlepath") {
                    model.syncNow()
                }
                .disabled(sync.isSyncing)
                if sync.isSyncing { ProgressView().controlSize(.small) }
                if let d = sync.lastSync {
                    LabeledContent("Letzter Sync", value: d.formatted(date: .abbreviated, time: .shortened))
                }
                if let s = sync.lastStats { Text(s).appFont(.caption).foregroundStyle(.secondary) }
                if let e = sync.lastError { Text(e).appFont(.caption).foregroundStyle(Theme.negative) }
                #if os(macOS)
                // „Ordner wählen …“ above grants access as well, without the system settings.
                if sync.folderAccessDenied {
                    Button("Systemeinstellungen öffnen", systemImage: "gearshape") { openFilesAndFoldersSettings() }
                }
                #endif
                Toggle("Tresor-Schlüssel über iCloud-Schlüsselbund teilen", isOn: $keychainSync)
                    .onChange(of: keychainSync) { _, on in
                        guard on != VaultKeyManager.shared.syncViaICloudKeychain else { return }
                        Task {
                            do { try await VaultKeyManager.shared.setICloudKeychainSync(on) }
                            catch { keychainSync = VaultKeyManager.shared.syncViaICloudKeychain; model.toast = error.localizedDescription }
                        }
                    }
                    // without the entitlement it could only fail (#909); switching off always works
                    .disabled(!VaultKeyManager.canShareViaICloudKeychain && !keychainSync)
                if let note = KeychainSyncNote.text(available: VaultKeyManager.canShareViaICloudKeychain) {
                    Text(note).appFont(.caption).foregroundStyle(.secondary)
                }
                HStack {
                    Button {
                        confirmKeyCopy = true
                    } label: {
                        // ticks every second while the key is on the clipboard, then offers copying again
                        TimelineView(.periodic(from: .now, by: 1)) { tick in
                            Label(KeyCopyCountdown.title(secondsLeft: KeyCopyCountdown.secondsLeft(until: keyCopiedUntil, now: tick.date)),
                                  systemImage: "key")
                                .monospacedDigit()
                        }
                    }
                    .confirmationDialog("Tresor-Schlüssel kopieren?", isPresented: $confirmKeyCopy, titleVisibility: .visible) {
                        Button("Schlüssel kopieren") {
                            Task {
                                // keychain failures show in the section; a cancelled unlock stays silent
                                keyCopyError = nil
                                do {
                                    VaultKeyManager.copyKeyToPasteboard(try await VaultKeyManager.shared.exportKeyBase64())
                                    keyCopiedUntil = Date().addingTimeInterval(VaultKeyManager.keyClipboardLifetime)
                                } catch VaultKeyManager.KeyError.cancelled {
                                } catch {
                                    keyCopyError = describe(error)
                                }
                            }
                        }
                        Button("Abbrechen", role: .cancel) {}
                    } message: {
                        Text("Der Schlüssel entschlüsselt Ihren gesamten Tresor. Er liegt 60 Sekunden in der Zwischenablage, nicht als Text. Über die geteilte Zwischenablage (Handoff, gleicher Apple Account) übernehmen Sie ihn auf dem anderen Gerät mit „Schlüssel einfügen …“ › „Aus Zwischenablage einfügen“; danach wird er dort aus der Zwischenablage gelöscht.")
                    }
                    Button("Schlüssel einfügen …", systemImage: "square.and.pencil") { showKeySheet = true }
                }
                #if os(iOS)
                // Two plain buttons in one Form row: iOS fires both on every tap, so
                // "Tresor-Schlüssel kopieren" opened the paste sheet too (#909).
                .buttonStyle(.borderless)
                #endif
                if let keyCopyError {
                    Label(keyCopyError, systemImage: "exclamationmark.triangle.fill")
                        .appFont(.caption).foregroundStyle(Theme.negative)
                }
                if sync.keyMismatch {
                    Label("Der iCloud-Tresor nutzt einen anderen Schlüssel. Fügen Sie den Schlüssel von Ihrem anderen Gerät ein.", systemImage: "exclamationmark.triangle.fill")
                        .appFont(.caption).foregroundStyle(Theme.warning)
                }
            }
            Text(sync.location == .container
                 ? LocalizedStringKey("Synchronisiert Ihren verschlüsselten Tresor über iCloud. Der Schlüssel liegt nie in iCloud Drive: Er wird über den iCloud-Schlüsselbund geteilt oder einmalig auf dem zweiten Gerät eingefügt.")
                 : LocalizedStringKey("Synchronisiert Ihren verschlüsselten Tresor über iCloud Drive (Ordner „GiroCrab“). Der Schlüssel liegt nie im Ordner: Er wird über den iCloud-Schlüsselbund geteilt oder einmalig auf dem zweiten Gerät eingefügt."))
                .appFont(.caption).foregroundStyle(.secondary)
        } header: { Text("iCloud Sync").appSectionHeader() }
        .onChange(of: sync.keyMismatch) { _, bad in if bad { showKeySheet = true } }
        .fileImporter(isPresented: $picking, allowedContentTypes: [.folder]) { r in
            if case .success(let url) = r {
                Task {
                    let scoped = url.startAccessingSecurityScopedResource()
                    // iCloud may still be listing the folder: wait briefly before calling it empty.
                    let hasVault = await ICloudFolderMigration.waitForVault(in: url, wait: 6)
                    let sub = hasVault ? nil : ICloudFolderMigration.vaultSubfolder(in: url)
                    if scoped { url.stopAccessingSecurityScopedResource() }
                    if let sub {
                        wrongFolderHint = sub
                    } else if !hasVault {
                        pendingFolder = url
                    } else {
                        do { try sync.setFolder(url, chosen: true); model.syncNow() } catch { }
                    }
                }
            }
        }
        .alert("Tresor liegt in einem Unterordner", isPresented: Binding(get: { wrongFolderHint != nil }, set: { if !$0 { wrongFolderHint = nil } })) {
            Button("OK", role: .cancel) { wrongFolderHint = nil }
        } message: {
            Text("Der gewählte Ordner enthält einen Unterordner „\(wrongFolderHint ?? "GiroCrab")“ mit Ihrem Tresor. Wählen Sie bitte genau diesen Unterordner aus, damit kein neuer Tresor angelegt wird.")
        }
        .alert("Neuen Tresor in diesem Ordner anlegen?", isPresented: Binding(get: { pendingFolder != nil }, set: { if !$0 { pendingFolder = nil } })) {
            Button("Neuen Tresor anlegen") {
                if let url = pendingFolder { do { try sync.setFolder(url, chosen: true); model.syncNow() } catch { } }
                pendingFolder = nil
            }
            Button("Abbrechen", role: .cancel) { pendingFolder = nil }
        } message: {
            Text("Der gewählte Ordner enthält noch keinen GiroCrab-Tresor. Dort wird ein neuer Tresor angelegt. Erwarten Sie Ihre Daten in einem anderen Ordner, brechen Sie ab und wählen Sie diesen.")
        }
        #if os(macOS)
        .fileDialogDefaultDirectory(Self.syncFolderOrCloudDocs)
        #endif
    }

    #if os(macOS)
    private static var cloudDocs: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Mobile Documents/com~apple~CloudDocs", isDirectory: true)
    }

    /// The sync folder in iCloud Drive ("GiroCrab"), or nil before it exists.
    private static var syncFolder: URL? {
        let url = ICloudFolderMigration.resolve(cloudDocs: cloudDocs, create: false, migratedByThisApp: CloudSyncManager.migratedByThisApp).url
        return FileManager.default.fileExists(atPath: url.path) ? url : nil
    }

    /// „Ordner wählen …“ opens inside the sync folder: „Öffnen“ alone picks it.
    private static var syncFolderOrCloudDocs: URL { syncFolder ?? cloudDocs }

    private func revealFolder() {
        if let url = Self.syncFolder {
            NSWorkspace.shared.activateFileViewerSelecting([url])
        } else {
            NSWorkspace.shared.open(Self.cloudDocs)
        }
    }

    /// Datenschutz & Sicherheit › Dateien und Ordner.
    private func openFilesAndFoldersSettings() {
        if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_FilesAndFolders") {
            NSWorkspace.shared.open(url)
        }
    }
    #endif
}

/// Fallback when the iCloud snapshot uses another key: paste the vault key
/// copied from the first device ("Tresor-Schlüssel kopieren").
struct KeyPasteSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var text = ""

    private var valid: Bool { (try? VaultKeyManager.parseKey(text)) != nil }
    private var working: Bool { model.keyAdoption == .working }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Tresor-Schlüssel einfügen").appFont(.headline)
            Text("Kopieren Sie den Schlüssel auf Ihrem anderen Gerät (Einstellungen › iCloud Sync › „Tresor-Schlüssel kopieren“) und fügen Sie ihn hier ein. Er wird im Schlüsselbund gespeichert, nie in iCloud Drive.")
                .appFont(.callout).foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            SecureField("Schlüssel (Base64 oder Hex)", text: $text)
                .textFieldStyle(.roundedBorder)
                .autocorrectionDisabled()
                .sensitive()
                .disabled(working)
            Button("Aus Zwischenablage einfügen", systemImage: "doc.on.clipboard") {
                if let k = VaultKeyManager.keyFromPasteboard() { text = k }
            }
            .disabled(working)
            if case .failed(let message) = model.keyAdoption {
                Label(message, systemImage: "exclamationmark.triangle.fill")
                    .appFont(.caption).foregroundStyle(Theme.negative)
                    .fixedSize(horizontal: false, vertical: true)
            }
            HStack {
                Button("Abbrechen", role: .cancel) { dismiss() }.keyboardShortcut(.cancelAction)
                Spacer()
                if working { ProgressView().controlSize(.small) }
                Button("Übernehmen") { model.adoptSyncKey(text) }
                .keyboardShortcut(.defaultAction)
                .buttonStyle(.borderedProminent)
                .disabled(!valid || working)
            }
        }
        .padding(24)
        .macFrame(minWidth: 420)
        .interactiveDismissDisabled(working)
        .onAppear { model.keyAdoption = .idle }
        .onChange(of: model.keyAdoption) { _, state in
            if state == .done { text = ""; dismiss() }
        }
    }
}

private extension View {
    /// macOS: each Settings tab has its own size and the window follows it
    /// (HIG: no large empty panes); iOS: the sheet decides.
    func settingsPane(height: CGFloat) -> some View {
        // Own row under the pane: a grouped Form scrolls beneath a safe-area inset.
        let pane = VStack(spacing: 0) {
            self
            AppVersionFooter()
        }
        #if os(macOS)
        // wider with larger text (#343); the panes scroll, so the height stays
        return pane.modifier(SettingsPaneFrame(height: height))
        #else
        return pane
        #endif
    }
}


#if os(macOS)
private struct SettingsPaneFrame: ViewModifier {
    @Environment(\.textScale) private var scale
    let height: CGFloat

    func body(content: Content) -> some View {
        content.frame(width: 680 * scale, height: height)
    }
}
#endif

/// Einstellungen › Mitteilungen: one switch, on by default (quiet banner only).
struct AnnouncementsSettingsSection: View {
    @State private var center = AnnouncementCenter.shared

    var body: some View {
        Section {
            Toggle("Mitteilungen des GiroCrab-Projekts anzeigen", isOn: $center.isEnabled)
            Text("Gelegentliche Neuigkeiten, unabhängig von Updates. GiroCrab lädt dazu eine signierte Textdatei von GitHub, ohne Kennung und ohne Tracking. Mitteilungen erscheinen nie als Fenster und lassen sich jederzeit ausblenden. Sicherheitswarnungen sind davon unabhängig.")
                .appFont(.callout).foregroundStyle(.secondary)
        } header: { Text("Mitteilungen").appSectionHeader() }
    }
}

/// The user's own FinTS product registration. Applies like every other setting: on Return
/// and when the field is left, no extra button. The value stays hidden (it is not shared).
struct OwnProductIDField: View {
    @Environment(AppModel.self) private var model
    @State private var text = ""
    @FocusState private var focused: Bool

    private var hasValue: Bool { model.settingEntry("fints.product_id")?.hasValue == true }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text("Eigene Produktregistrierung")
                Spacer()
                SecureField("Eigene Produktregistrierung", text: $text,
                            prompt: Text(hasValue ? "Gespeichert – zum Ersetzen neu eingeben" : "Registrierungsnummer eintragen"))
                    .labelsHidden()
                    .frame(maxWidth: 340)
                    .multilineTextAlignment(.trailing)
                    .focused($focused)
                    .onSubmit(commit)
            }
            Text("Nur nötig, wenn Sie Ihre eigene Registrierung statt der von GiroCrab verwenden. Sie bleibt gespeichert, auch wenn Sie die von GiroCrab nutzen.")
                .appFont(.caption).foregroundStyle(.secondary)
            Link("Produktregistrierung bei der Deutschen Kreditwirtschaft beantragen",
                 destination: URL(string: "https://www.fints.org/de/hersteller/produktregistrierung")!)
                .appFont(.caption)
        }
        .onAppear(perform: focusIfAsked)
        .onChange(of: model.focusOwnProductID) { _, _ in focusIfAsked() }
        .onChange(of: focused) { _, isFocused in if !isFocused { commit() } }
        #if os(macOS)
        .onDisappear(perform: commit)
        #endif
    }

    /// An empty field changes nothing (the stored value is never shown or cleared by accident).
    private func commit() {
        let value = text.trimmingCharacters(in: .whitespaces)
        text = ""
        guard !value.isEmpty else { return }
        model.saveSettingInBackground("fints.product_id", value)
        model.setFintsProductIDMode("own")
    }

    /// After a rejected registration the alert sends the user here.
    private func focusIfAsked() {
        guard model.focusOwnProductID else { return }
        model.focusOwnProductID = false
        focused = true
    }
}

/// Why the iCloud Keychain switch is off in this build (#909), or nil when it works.
enum KeychainSyncNote {
    static func text(available: Bool) -> String? {
        available ? nil : String(localized: "In dieser Version nicht verfügbar. Übertragen Sie den Schlüssel mit „Tresor-Schlüssel kopieren“ und fügen Sie ihn auf dem anderen Gerät mit „Schlüssel einfügen …“ ein.")
    }
}

/// Label of "Tresor-Schlüssel kopieren": while the key is on the clipboard, how
/// long it stays there (#909; a fixed "(60 s)" stayed after the key was gone).
enum KeyCopyCountdown {
    /// Whole seconds until `until`, rounded up; nil once it has passed.
    static func secondsLeft(until: Date?, now: Date) -> Int? {
        guard let until, until > now else { return nil }
        return Int(until.timeIntervalSince(now).rounded(.up))
    }

    static func title(secondsLeft: Int?) -> String {
        guard let secondsLeft else { return String(localized: "Tresor-Schlüssel kopieren") }
        return String(localized: "Schlüssel noch \(secondsLeft) s in der Zwischenablage")
    }
}
