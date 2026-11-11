import SwiftUI
import GiroCrabCore
#if os(macOS)
import AppKit
#endif

/// Einstellungen: Bankzugänge, eigene FinTS-Produkt-ID und API-Schlüssel
/// (im verschlüsselten SQLCipher-Tresor), Händler-Logos, Sicherheit.
struct SettingsView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        @Bindable var model = model
        TabView(selection: $model.settingsTab) {
            BankConnectionsSettings()
                .tabItem { Label("Banken", systemImage: "building.columns") }
                .tag(SettingsTab.banks)
            AccountGroupsSettings()
                .tabItem { Label("Kontogruppen", systemImage: "folder") }
                .tag(SettingsTab.groups)
            CredentialsSettings()
                .tabItem { Label("FinTS & API-Schlüssel", systemImage: "key.horizontal") }
                .tag(SettingsTab.credentials)
            LogoSettings()
                .tabItem { Label("Händler-Logos", systemImage: "photo.on.rectangle") }
                .tag(SettingsTab.logos)
            #if os(macOS)
            DocumentFolderSettings()
                .tabItem { Label("Dokumente", systemImage: "doc.on.doc") }
                .tag(SettingsTab.documents)
            #endif
            SecuritySettings()
                .tabItem { Label("Sicherheit", systemImage: "lock.shield") }
                .tag(SettingsTab.security)
        }
        .disabled(model.vault == nil)
        #if os(iOS)
        .navigationTitle("Einstellungen")
        .navigationBarTitleDisplayMode(.inline)
        #endif
    }
}

/// Tabs of the settings window (macOS) / sheet (iOS).
enum SettingsTab: Hashable {
    case banks, groups, credentials, logos, documents, security
}

struct BankConnectionsSettings: View {
    @Environment(AppModel.self) private var model
    @State private var showAddBank = false
    @State private var editing: BankConnection?

    var body: some View {
        Form {
            if model.demoActive {
                Section {
                    Label(model.hasOwnVault
                          ? "Sie sehen gerade Demo-Daten. Neue Bankzugänge werden in Ihrem eigenen Tresor angelegt – die App wechselt dabei automatisch dorthin."
                          : "Demo-Modus: Bankzugänge werden hier nur simuliert. Für echte Konten zuerst den eigenen Tresor öffnen.",
                          systemImage: "theatermasks")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                    if !model.hasOwnVault {
                        Button("Eigenen Tresor öffnen") { model.showOwnVault() }
                    }
                }
            }
            Section("Verbundene Banken") {
                if model.connections.isEmpty {
                    Text("Noch keine Bank hinzugefügt.").foregroundStyle(.secondary)
                }
                ForEach(model.connections) { c in
                    HStack(spacing: 10) {
                        RoundedRectangle(cornerRadius: 8, style: .continuous)
                            .fill(c.connector.tint.opacity(0.12))
                            .overlay(Text(c.connector.monogram).font(.caption.bold()).foregroundStyle(c.connector.tint))
                            .frame(width: 30, height: 30)
                        VStack(alignment: .leading, spacing: 1) {
                            Text(c.displayName).fontWeight(.medium)
                            Text(protocolName(c.connector) + (c.loginName.isEmpty ? "" : " · \(c.loginName)"))
                                .font(.caption).foregroundStyle(.secondary)
                        }
                        Spacer()
                        if let t = c.lastSyncUnix {
                            Text(Date(timeIntervalSince1970: TimeInterval(t)), style: .relative)
                                .font(.caption).foregroundStyle(.secondary)
                        }
                        Button { editing = c } label: { Image(systemName: "slider.horizontal.3") }
                            .buttonStyle(.borderless)
                            .help("Bearbeiten / Details")
                        Button { model.requestSync(c) } label: { Image(systemName: "arrow.triangle.2.circlepath") }
                            .buttonStyle(.borderless)
                            .help("Jetzt abrufen")
                        Button(role: .destructive) { model.removeConnection(c.id) } label: { Image(systemName: "trash") }
                            .buttonStyle(.borderless)
                            .help("Bank und ihre Konten entfernen")
                    }
                    .contentShape(Rectangle())
                    .onTapGesture { editing = c }
                }
            }
            Section("Bank hinzufügen") {
                Button { showAddBank = true } label: {
                    Label("Bank suchen und hinzufügen …", systemImage: "magnifyingglass")
                }
                Text("Suche nach Bankname, BLZ oder IBAN: FinTS-Banken, Banken mit eigener API (bunq, comdirect, Trade Republic, PayPal) und PSD2-Banken (N26, Revolut, C24 …).")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
        .sheet(isPresented: $showAddBank) {
            AddBankSheet()
                .environment(model)
        }
        .sheet(item: $editing) { c in
            ConnectionEditSheet(connection: c)
                .environment(model)
        }
    }

    private func protocolName(_ k: ConnectorKind) -> String {
        bankCatalog().first { $0.kind == k }?.protocol ?? ""
    }
}

struct CredentialsSettings: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        // Group by the core's stable group id, show its translated title, match
        // on keys (logo settings have their own tab) and keep the core's order.
        let entries = model.settings.filter { !$0.key.hasPrefix("merchants.") }
        let grouped = Dictionary(grouping: entries, by: \.group)
        let order = entries.map(\.group).reduce(into: [String]()) { if !$0.contains($1) { $0.append($1) } }
        Form {
            Section {
                Label("Nichts davon wird mit der App ausgeliefert. Registrieren Sie Ihr eigenes FinTS-Produkt und Ihre eigenen API-Anwendungen; die Werte liegen nur in Ihrem verschlüsselten Tresor.", systemImage: "info.circle")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
            ForEach(order.filter { grouped[$0] != nil }, id: \.self) { g in
                Section(grouped[g]?.first?.groupTitle ?? g) {
                    ForEach(grouped[g] ?? []) { entry in
                        SettingField(entry: entry)
                    }
                }
            }
        }
        .formStyle(.grouped)
    }
}

struct SettingField: View {
    @Environment(AppModel.self) private var model
    let entry: SettingEntry
    @State private var text = ""
    @State private var editing = false

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text(entry.label)
                Spacer()
                if entry.isSecret {
                    if editing {
                        SecureField("", text: $text)
                            .frame(maxWidth: 280)
                            .onSubmit(save)
                        Button("Speichern", action: save)
                    } else {
                        Text(entry.hasValue ? "••••••••" : "nicht gesetzt").foregroundStyle(entry.hasValue ? .primary : .secondary)
                        Button(entry.hasValue ? "Ändern" : "Festlegen") { text = ""; editing = true }
                    }
                } else {
                    TextField("", text: $text, prompt: Text("nicht gesetzt"))
                        .frame(maxWidth: 340)
                        .multilineTextAlignment(.trailing)
                        .autocorrectionDisabled()
                        .onSubmit(save)
                }
            }
            Text(entry.help).font(.caption).foregroundStyle(.secondary)
        }
        .onAppear { if !entry.isSecret { text = entry.value } }
        #if os(macOS)
        .onDisappear { if !entry.isSecret && text != entry.value { save() } }
        #endif
    }

    private func save() {
        model.saveSetting(entry.key, text)
        if entry.isSecret { text = ""; editing = false }
    }
}

struct LogoSettings: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        Form {
            Section("Händler-Logos") {
                LabeledContent("Stufe 1 – offline", value: String(localized: "Mitgelieferte Händlertabelle (Monogramm + Markenfarbe)"))
                LabeledContent("Stufe 1b – simple-icons", value: String(localized: "Vektor-Markenzeichen (CC0-Datensatz) für bekannte Marken"))
                Picker("Händler-Logos laden", selection: Binding(
                    get: { model.logoProvider },
                    set: { model.saveSetting("merchants.online_logos", $0) }
                )) {
                    Text("Aus").tag("off")
                    Text("DuckDuckGo").tag("duckduckgo")
                    Text("Google").tag("google")
                }
                Text("Stufe 2: Lädt Favicons vom gewählten Anbieter (icons.duckduckgo.com bzw. google.com/s2/favicons) und speichert sie verschlüsselt in der Tabelle merchant_logo_cache. Übertragen wird nur die Händler-Domain (z. B. lidl.de) – keine Beträge, keine Kontodaten. Standard: aus.")
                    .font(.caption).foregroundStyle(.secondary)
                LabeledContent("Stufe 3 – Rückfall", value: String(localized: "SF Symbol der Kategorie"))
                Button("Logo-Cache leeren", role: .destructive) { model.clearLogoCache() }
            }
        }
        .formStyle(.grouped)
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
            Section("Dokumentenordner") {
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
                    .font(.caption).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Section("Automatischer Abruf") {
                Text("Ob GiroCrab die PDF-Kontoauszüge eines FinTS-Kontos bei jedem Sync mit abruft, stellen Sie pro Konto unter „Dokumente / Kontoauszüge“ ein.")
                    .font(.caption).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
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

    var body: some View {
        Form {
            Section("Tresor") {
                LabeledContent("Verschlüsselung", value: "SQLCipher 4 (AES-256)")
                LabeledContent("Hauptschlüssel", value: model.isDemo ? String(localized: "Demo-Schlüssel (Wegwerf)") : String(localized: "Schlüsselbund, geschützt durch \(VaultKeyManager.shared.biometryName)"))
                LabeledContent("Datei", value: model.vault?.filePath() ?? "–")
                    .textSelection(.enabled)
                LabeledContent("Core-Version", value: coreVersion())
            }
            ICloudSyncSection()
            Section("Datenschutz") {
                Text("GiroCrab verbindet sich direkt von diesem Gerät mit Ihren Banken. Es gibt keinen GiroCrab-Server, kein Tracking und keine Cloud-Synchronisation. PINs und TANs liegen nur für einen Abruf im Speicher und werden danach gelöscht.")
                    .font(.callout).foregroundStyle(.secondary)
            }
            if model.hasOwnVault && !model.isDemo {
                Section {
                    Button("Jetzt sperren", systemImage: "lock.fill") { model.lock() }
                }
            }
        }
        .formStyle(.grouped)
    }
}

/// Kompakter iCloud-Drive-Sync (nur die verschlüsselte Tresordatei).
struct ICloudSyncSection: View {
    @Environment(AppModel.self) private var model
    @State private var sync = CloudSyncManager.shared
    @State private var picking = false
    @State private var keychainSync = VaultKeyManager.shared.syncViaICloudKeychain
    @State private var showKeySheet = false
    @State private var keyCopied = false

    var body: some View {
        Section("iCloud Sync") {
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
                LabeledContent("Ordner", value: sync.folderName ?? "iCloud Drive / GiroCrab")
                #if os(macOS)
                Button("Im Finder zeigen", systemImage: "folder") { revealFolder() }
                #else
                Button("Ordner wählen …", systemImage: "folder") { picking = true }
                #endif
                Button(sync.isSyncing ? "Synchronisiere …" : "Jetzt synchronisieren", systemImage: "arrow.triangle.2.circlepath") {
                    model.syncNow()
                }
                .disabled(sync.isSyncing)
                if sync.isSyncing { ProgressView().controlSize(.small) }
                if let d = sync.lastSync {
                    LabeledContent("Letzter Sync", value: d.formatted(date: .abbreviated, time: .shortened))
                }
                if let s = sync.lastStats { Text(s).font(.caption).foregroundStyle(.secondary) }
                if let e = sync.lastError { Text(e).font(.caption).foregroundStyle(.red) }
                Toggle("Tresor-Schlüssel über iCloud-Schlüsselbund teilen", isOn: $keychainSync)
                    .onChange(of: keychainSync) { _, on in
                        guard on != VaultKeyManager.shared.syncViaICloudKeychain else { return }
                        Task {
                            do { try await VaultKeyManager.shared.setICloudKeychainSync(on) }
                            catch { keychainSync = VaultKeyManager.shared.syncViaICloudKeychain; model.toast = error.localizedDescription }
                        }
                    }
                HStack {
                    Button(keyCopied ? "Schlüssel kopiert (60 s)" : "Tresor-Schlüssel kopieren", systemImage: "key") {
                        Task {
                            if let k = try? await VaultKeyManager.shared.exportKeyBase64() {
                                VaultKeyManager.copyToPasteboard(k)
                                keyCopied = true
                            }
                        }
                    }
                    Button("Schlüssel einfügen …", systemImage: "square.and.pencil") { showKeySheet = true }
                }
                if sync.keyMismatch {
                    Label("Der iCloud-Tresor nutzt einen anderen Schlüssel. Fügen Sie den Schlüssel von Ihrem anderen Gerät ein.", systemImage: "exclamationmark.triangle.fill")
                        .font(.caption).foregroundStyle(.orange)
                }
            }
            Text("Synchronisiert Ihren verschlüsselten Tresor über iCloud Drive (Ordner „GiroCrab“). Der Schlüssel liegt nie im Ordner: Er wird über den iCloud-Schlüsselbund geteilt oder einmalig auf dem zweiten Gerät eingefügt.")
                .font(.caption).foregroundStyle(.secondary)
        }
        .onChange(of: sync.keyMismatch) { _, bad in if bad { showKeySheet = true } }
        .sheet(isPresented: $showKeySheet) {
            KeyPasteSheet().environment(model)
        }
        #if os(iOS)
        .fileImporter(isPresented: $picking, allowedContentTypes: [.folder]) { r in
            if case .success(let url) = r {
                do { try sync.setFolder(url); model.syncNow() } catch { }
            }
        }
        #endif
    }

    #if os(macOS)
    private func revealFolder() {
        let cloudDocs = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Mobile Documents/com~apple~CloudDocs", isDirectory: true)
        let mm = cloudDocs.appendingPathComponent("moneymoney", isDirectory: true)
        let gc = cloudDocs.appendingPathComponent("GiroCrab", isDirectory: true)
        let url = FileManager.default.fileExists(atPath: mm.appendingPathComponent("tresor.girocrab").path) ? mm : gc
        if FileManager.default.fileExists(atPath: url.path) {
            NSWorkspace.shared.activateFileViewerSelecting([url])
        } else {
            NSWorkspace.shared.open(cloudDocs)
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

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Tresor-Schlüssel einfügen").font(.headline)
            Text("Kopieren Sie den Schlüssel auf Ihrem anderen Gerät (Einstellungen › iCloud Sync › „Tresor-Schlüssel kopieren“) und fügen Sie ihn hier ein. Er wird im Schlüsselbund gespeichert, nie in iCloud Drive.")
                .font(.callout).foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            SecureField("Schlüssel (Base64 oder Hex)", text: $text)
                .textFieldStyle(.roundedBorder)
                .autocorrectionDisabled()
            HStack {
                Button("Abbrechen", role: .cancel) { dismiss() }.keyboardShortcut(.cancelAction)
                Spacer()
                Button("Übernehmen") {
                    model.adoptSyncKey(text)
                    text = ""
                    dismiss()
                }
                .keyboardShortcut(.defaultAction)
                .buttonStyle(.borderedProminent)
                .disabled(!valid)
            }
        }
        .padding(24)
        .macFrame(minWidth: 420)
    }
}
