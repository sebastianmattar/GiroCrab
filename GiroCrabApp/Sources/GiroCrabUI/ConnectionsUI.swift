import SwiftUI
import WebKit
import UniformTypeIdentifiers
import GiroCrabCore

// MARK: - PIN / login sheet before a sync

/// Asks for the PIN right before a sync. Trade Republic: the phone number is
/// editable here (normalised to +49…), and the in-app browser login is offered.
struct PinSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.reportRedaction) private var redaction
    @Environment(\.dismiss) private var dismiss
    let connection: BankConnection
    @State private var login = ""
    @State private var secret = ""
    @State private var savePin = false

    private var isRah10: Bool { model.usesRah10(connection) }
    private var secretLabel: String { isRah10 ? "Passwort der Schlüsseldatei" : (bankCatalog().first { $0.kind == connection.connector }?.secretLabel ?? String(localized: "PIN")) }

    private var isTR: Bool { connection.connector == .tradeRepublic }
    /// BHW: the secret is first name, date of birth and postcode, not a PIN.
    private var isBhw: Bool { connection.connector == .bhw }

    var body: some View {
        let d = bankCatalog().first { $0.kind == connection.connector }
        VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 10) {
                SymbolTile(symbol: connection.sfSymbol, tint: connection.tint, size: 34)
                VStack(alignment: .leading) {
                    Text("Anmeldung bei \(redaction.masking(connection.displayName))").appFont(.headline)
                    Text(d?.protocol ?? "").appFont(.caption).foregroundStyle(Theme.secondary)
                }
                Spacer()
                SyncBatchProgressLabel()
            }
            if model.demoActive || connection.bankCode == "99999999" { DemoHintBanner(style: .credentials) { login = "demo"; secret = "12345" } }
            VStack(alignment: .leading, spacing: 4) {
                Text(d?.loginLabel ?? String(localized: "Anmeldung")).appFont(.caption).foregroundStyle(Theme.secondary)
                TextField(d?.loginLabel ?? String(localized: "Anmeldung"), text: $login)
                    .disabled(isRah10)
                    .textFieldStyle(.roundedBorder)
                    .autocorrectionDisabled()
                    #if os(iOS)
                    .keyboardType(isTR ? .phonePad : .default)
                    #endif
                if isTR && !login.isEmpty {
                    let n = normalizePhoneNumber(input: login)
                    Text(n == login ? "Format korrekt (E.164)" : "Wird gesendet als \(redaction.masking(n))")
                        .appFont(.caption2).foregroundStyle(Theme.secondary)
                }
            }
            VStack(alignment: .leading, spacing: 4) {
                Text(secretLabel).appFont(.caption).foregroundStyle(Theme.secondary)
                SecureField(secretLabel, text: $secret)
                    .textFieldStyle(.roundedBorder)
                    .onSubmit(start)
                Toggle(isRah10 ? "Passwort im Tresor speichern" : isBhw ? "Anmeldedaten im Tresor speichern" : "PIN im Tresor speichern", isOn: $savePin)
                    .appFont(.caption)
                    .help("Verschlüsselt in der SQLCipher-Datenbank; beim nächsten Abruf wird nicht mehr gefragt.")
            }
            Text(isRah10
                 ? (savePin ? "Das Passwort wird verschlüsselt im Tresor gespeichert." : "Das Passwort wird nur für diesen Abruf verwendet.")
                 : isBhw
                 ? (savePin ? "Die Anmeldedaten werden verschlüsselt im Tresor gespeichert." : "Die Anmeldedaten werden nur für diesen Abruf verwendet und nie gespeichert.")
                 : (savePin ? "Die PIN wird verschlüsselt im Tresor gespeichert." : "Die PIN wird nur für diesen Abruf verwendet und nie gespeichert."))
                .appFont(.caption).foregroundStyle(Theme.secondary)
            if isTR {
                Divider()
                VStack(alignment: .leading, spacing: 6) {
                    Text("Anmeldung blockiert (HTTP 403/426)?").appFont(.caption, weight: .semibold)
                    Text("Melden Sie sich im eingebauten Browser-Fenster an. GiroCrab übernimmt danach die Sitzung (tr_session) für den Abruf.")
                        .appFont(.caption).foregroundStyle(Theme.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                    Button {
                        saveLoginIfChanged()
                        dismiss()
                        model.pendingLogin = nil
                        model.webLoginConnection = currentConnection
                    } label: {
                        Label("Im Browser-Fenster anmelden …", systemImage: "safari")
                    }
                }
            }
            SyncPromptButtons(cancel: model.cancelPendingLogin) {
                Button(action: start) {
                    Text("Abrufen").frame(maxWidth: .infinity)
                }
                .keyboardShortcut(.defaultAction)
                .buttonStyle(.borderedProminent)
                .disabled(secret.isEmpty || login.trimmingCharacters(in: .whitespaces).isEmpty)
            }
        }
        .padding(24)
        .macFrame(minWidth: 420)
        .onAppear {
            login = connection.loginName
            savePin = model.connectionStatus(connection.id)?.hasPin ?? false
        }
    }

    private var currentConnection: BankConnection {
        model.connections.first { $0.id == connection.id } ?? connection
    }

    private func saveLoginIfChanged() {
        let l = isTR ? normalizePhoneNumber(input: login) : login.trimmingCharacters(in: .whitespaces)
        if l != connection.loginName {
            model.updateConnection(connection.id, name: connection.displayName, login: l)
        }
    }

    private func start() {
        guard !secret.isEmpty else { return }
        saveLoginIfChanged()
        let s = secret
        secret = ""
        model.setConnectionPin(connection.id, savePin ? s : nil)
        model.startSync(currentConnection, secret: s)
    }
}

// MARK: - Edit sheet for a connected bank

/// Shows and edits everything stored for one bank login.
struct ConnectionEditSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let connection: BankConnection

    @State private var name = ""
    @State private var login = ""
    @State private var pin = ""
    @State private var advanziaSettings = AdvanziaConnectionSettingsState()
    @State private var storePin = false
    @State private var tanMedium = ""
    private var isFinTs: Bool { [.volksbankKasselGoettingen, .direkt1822, .finTsGeneric].contains(connection.connector) }
    private var tanMediumKey: String { "fints.tan_medium.\(connection.id)" }
    /// Application-ID / key: shown and saved for this login only (#824).
    private static let ebIDKey = "enablebanking.application_id"
    private static let ebPemKey = "enablebanking.private_key_pem"
    private func isEBAppKey(_ key: String) -> Bool { key == Self.ebIDKey || key == Self.ebPemKey }
    @State private var values: [String: String] = [:]
    /// Enable Banking application of this login as loaded (#824): saved per login, only when changed.
    @State private var loadedEBApp: (id: String, pem: String) = ("", "")
    @State private var revealed: Set<String> = []
    @State private var importingKey = false
    @State private var status: ConnectionStatus?
    @State private var confirmRemove: BankConnection?
    @State private var selection = AccountSelectionDraft()
    /// Document retrieval of this connection (only for banks that deliver documents).
    @State private var documentsSupported = false
    @State private var documentsFetch = true
    /// "Dokumente abrufen" is off: nothing that fetches documents runs (PostBox included).
    private var documentsBlocked: Bool { documentsSupported && !documentsFetch }
    @State private var initialDocumentsFetch = true
    /// The values as loaded: "Speichern" is only available once something differs.
    @State private var initialValues: [String: String] = [:]
    @State private var initialTanMedium = ""
    @State private var confirmClear: SessionClear?

    /// Session data whose deletion is being confirmed.
    private enum SessionClear: Identifiable {
        case portal, tradeRepublic
        var id: Self { self }
    }

    private var isRah10: Bool { model.usesRah10(connection) }

    private var descriptor: BankDescriptor? { bankCatalog().first { $0.kind == connection.connector } }

    /// Global setting keys relevant for this connector.
    private var keys: [String] {
        let all = allKeys
        // With GiroCrab's own registration built in, it is chosen in Settings, not per bank.
        return model.fintsBuiltinAvailable ? all.filter { $0 != "fints.product_id" } : all
    }

    private var allKeys: [String] {
        switch connection.connector {
        case .volksbankKasselGoettingen: return ["fints.url.52090000", "fints.product_id", "fints.product_version"]
        case .direkt1822: return ["fints.url.50050201", "fints.product_id", "fints.product_version"]
        case .finTsGeneric: return isRah10 ? ["fints.product_id", "fints.product_version"] : ["fints.url.\(connection.bankCode)", "fints.product_id", "fints.product_version"]
        case .comdirect: return ["comdirect.client_id", "comdirect.client_secret"]
        case .kontist: return ["kontist.client_id", "kontist.client_secret"]
        case .payPal: return ["paypal.client_id", "paypal.client_secret", "paypal.environment"]
        case .tradeRepublic: return []
        case .americanExpress, .amazonVisa, .advanzia: return []
        case .bunq: return ["bunq.environment"]
        case .enableBanking: return ["enablebanking.application_id", "enablebanking.private_key_pem", "enablebanking.redirect_url"]
        case .scalableCapital, .amazonOrders, .fnz, .ibkr, .quirion, .easybank, .alteLeipziger, .volkswagenBank, .uberspace, .schwaebischHall, .equatePlus, .ffb, .fordMoney, .opelDirektbank, .hanseaticBank, .bhw, .finpension: return []
        }
    }

    /// comdirect via FinTS: the API credentials for the PostBox (#838), shown in its section.
    private var postboxKeys: [String] {
        isComdirect && connection.connector == .finTsGeneric ? ["comdirect.client_id", "comdirect.client_secret"] : []
    }

    /// Optional power-user PSD2 credentials (collapsed "Erweitert" section).
    private var advancedKeys: [String] {
        switch connection.connector {
        case .americanExpress: return ["enablebanking.use.amex", "enablebanking.application_id", "enablebanking.private_key_pem", "enablebanking.redirect_url", "enablebanking.aspsp.amex", "enablebanking.country.amex"]
        // Prototype #156: classic NVP API instead of the REST app.
        case .payPal: return AppModel.payPalSignatureKeys
        case .amazonVisa: return ["enablebanking.use.amazonvisa", "enablebanking.application_id", "enablebanking.private_key_pem", "enablebanking.redirect_url", "enablebanking.aspsp.amazonvisa", "enablebanking.country.amazonvisa"]
        default: return []
        }
    }
    /// PSD2 consent: end date, warning before expiry, accounts without
    /// access, and "Neu verbinden …" (new consent at the bank).
    @ViewBuilder private func consentRows(_ st: ConnectionStatus) -> some View {
        let end = PSD2Consent.parse(st.consentValidUntil)
        let stage = PSD2Consent.stage(until: end)
        LabeledContent("Zustimmung") {
            if let end {
                HStack(spacing: 6) {
                    Text("gültig bis \(end.formatted(date: .abbreviated, time: .omitted))")
                    switch stage {
                    case .expired:
                        Label("abgelaufen", systemImage: "exclamationmark.triangle.fill")
                            .foregroundStyle(Theme.negative)
                    case .expiringSoon:
                        Label("noch \(max(0, PSD2Consent.daysLeft(until: end))) Tage", systemImage: "clock.badge.exclamationmark")
                            .foregroundStyle(Theme.warning)
                    default:
                        EmptyView()
                    }
                }
                .appFont(.callout)
            } else {
                Text("keine Zustimmung gespeichert").foregroundStyle(Theme.secondary)
            }
        }
        if st.revokedAccountCount > 0 {
            LabeledContent("Ohne Zugriff") {
                Label("\(st.revokedAccountCount) Konten", systemImage: "lock.slash")
                    .foregroundStyle(Theme.warning)
                    .help("Diese Konten sind in der Zustimmung nicht mehr enthalten. Buchungen bleiben erhalten; Konto bei Bedarf selbst löschen.")
            }
        }
        Button {
            save()
            dismiss()
            model.reconnect(connection)
        } label: { Label("Neu verbinden …", systemImage: "arrow.triangle.2.circlepath") }
            .help("Neue Zustimmung bei der Bank erteilen (z. B. vor Ablauf oder um Konten hinzuzufügen oder zu entfernen).")
        Text("Konten in Enable Banking verknüpft oder entfernt? Das wirkt erst nach „Neu verbinden“.")
            .appFont(.caption).foregroundStyle(Theme.secondary)
    }

    private var isPortal: Bool { connection.connector == .americanExpress || connection.connector == .amazonVisa }
    @State private var importingStatement = false
    @State private var importingFinpension = false
    @State private var confirmSwitch = false
    /// comdirect via FinTS: PostBox through the bank's API as well (#838).
    @State private var postbox = false
    /// comdirect logins can switch between FinTS and comdirect's own API.
    /// Consorsbank via FinTS: documents from the Online-Archiv as well (#1024).
    private var isConsorsbankFinTS: Bool {
        connection.connector == .finTsGeneric && connection.bankCode == "76030080"
    }
    private var isComdirect: Bool {
        connection.connector == .comdirect
            || (connection.connector == .finTsGeneric && ["20041111", "20041133", "20041144", "20041155"].contains(connection.bankCode))
    }
    @State private var showAdvanced = false

    private var usesPin: Bool { [.volksbankKasselGoettingen, .direkt1822, .finTsGeneric, .comdirect, .tradeRepublic, .bunq, .ibkr, .easybank, .alteLeipziger, .volkswagenBank, .uberspace, .ffb, .fordMoney, .opelDirektbank, .hanseaticBank, .bhw, .kontist].contains(connection.connector) }
    private var isTR: Bool { connection.connector == .tradeRepublic }
    /// Amex: user name and password for the portal login, kept in the vault and
    /// filled into the login form of the portal window.
    private var savesPortalLogin: Bool { connection.connector == .americanExpress }
    @State private var storePortalLogin = false
    @State private var portalUser = ""
    @State private var initialPortalUser = ""
    @State private var initialStorePortalLogin = false

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 10) {
                SymbolTile(symbol: connection.sfSymbol, tint: connection.tint, size: 34)
                VStack(alignment: .leading, spacing: 2) {
                    Text(connection.displayName).appFont(.headline).sensitive()
                    Text(descriptor?.protocol ?? "").appFont(.caption).foregroundStyle(Theme.secondary)
                }
                Spacer()
            }
            .padding(20)
            Divider()
            Form {
                Section {
                    TextField("Anzeigename", text: $name)
                    TextField(descriptor?.loginLabel ?? String(localized: "Benutzerkennung"), text: $login)
                        .disabled(isRah10)
                        .autocorrectionDisabled()
                    if isTR && !login.isEmpty {
                        LabeledContent("Gespeichert als") { Text(normalizePhoneNumber(input: login)).sensitive() }
                    }
                    let blz = connection.bankCode.isEmpty ? (descriptor?.bankCode ?? "") : connection.bankCode
                    if !blz.isEmpty, isFinTs {
                        LabeledContent("BLZ", value: blz)
                    }
                    if usesPin {
                        Toggle(isRah10 ? "Passwort im Tresor speichern" : connection.connector == .bunq ? "API-Schlüssel im Tresor speichern" : connection.connector == .ibkr ? "Flex-Query-ID im Tresor speichern" : connection.connector == .bhw ? "Anmeldedaten im Tresor speichern" : [.easybank, .alteLeipziger, .uberspace, .fordMoney, .opelDirektbank, .hanseaticBank, .kontist].contains(connection.connector) ? "Passwort im Tresor speichern" : "PIN im Tresor speichern", isOn: $storePin)
                        if storePin {
                            SecureField(isRah10 ? "Passwort der Schlüsseldatei (leer = unverändert)" : status?.hasPin == true ? "Gespeicherte PIN (leer = unverändert)" : (connection.connector == .ibkr ? "Flex-Query-ID" : connection.connector == .bhw ? "Vorname;TT.MM.JJJJ;PLZ" : [.easybank, .alteLeipziger, .uberspace, .fordMoney, .opelDirektbank, .hanseaticBank, .kontist].contains(connection.connector) ? "Passwort" : "PIN"), text: $pin)
                        }
                    }
                    if isFinTs {
                        // Name of the TAN device some banks require in every TAN request
                        // (Deutsche Bank / Postbank BestSign, error 9210).
                        TextField("TAN-Medium", text: $tanMedium, prompt: Text("Optional"))
                            .autocorrectionDisabled()
                    }
                    if documentsSupported {
                        Toggle("Dokumente abrufen", isOn: $documentsFetch)
                            .help("Belege, Kontoauszüge und PostBox-Dokumente werden bei jedem Abruf geladen. Aus: keine neuen Dokumente, bereits gespeicherte bleiben. Beim Einschalten lädt der nächste Abruf fehlende Dokumente nach.")
                    }
                } header: { Text("Zugang").appSectionHeader() }
                if connection.connector == .advanzia {
                    AdvanziaConnectionFields(state: advanziaSettings)
                }
                if !keys.isEmpty {
                    Section {
                        ForEach(keys, id: \.self) { key in field(key) }
                    } header: { Text(sectionTitle).appSectionHeader() }
                }
                // Prototype #156: classic NVP API (API signature) instead of the REST app.
                if connection.connector == .payPal {
                    Section {
                        DisclosureGroup("Erweitert: API-Signatur (klassische NVP-API)", isExpanded: $showAdvanced) {
                            Text("Ist die API-Signatur gesetzt, ruft GiroCrab per API-Benutzername, API-Passwort und Signatur ab; Client-ID und Secret werden dann nicht gebraucht.")
                                .appFont(.caption).foregroundStyle(Theme.secondary)
                                .fixedSize(horizontal: false, vertical: true)
                            ForEach(advancedKeys, id: \.self) { key in field(key) }
                        }
                    }
                }
                if connection.connector == .finpension {
                    Section {
                        Text("Finpension hat keine Schnittstelle: Laden Sie in Ihrem Finpension-Konto den Export (CSV oder JSON) mit Positionen und Cash-Buchungen herunter und lesen Sie ihn hier ein. Säule 3a, Freizügigkeit und 1e werden gleich behandelt. Es gibt keine Anmeldung und keinen Netzwerkzugriff.")
                            .appFont(.caption).foregroundStyle(Theme.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                        Button { importingFinpension = true } label: { Label("Finpension-Export importieren …", systemImage: "doc.badge.plus") }
                            .buttonStyle(.borderedProminent)
                            .fileImporter(isPresented: $importingFinpension, allowedContentTypes: [.commaSeparatedText, .json, .plainText, .data]) { result in
                                if case .success(let url) = result {
                                    save()
                                    model.importFinpension(connection, url: url)
                                    dismiss()
                                }
                            }
                    }
                }
                if isPortal {
                    Section {
                        Text("Melden Sie sich im Browser-Fenster mit Ihrem normalen Benutzernamen, Passwort und 2FA an. GiroCrab liest die Umsätze direkt aus dem Portal – es sind keine API-Schlüssel nötig. Die Sitzung bleibt für spätere Abrufe erhalten.")
                            .appFont(.caption).foregroundStyle(Theme.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                        Button {
                            save()
                            dismiss()
                            model.webLoginConnection = model.connections.first { $0.id == connection.id } ?? connection
                        } label: { Label("Anmelden & Abrufen", systemImage: "safari") }
                            .buttonStyle(.borderedProminent)
                        Button { importingStatement = true } label: { Label("CSV- oder PDF-Kontoauszug importieren …", systemImage: "doc.badge.plus") }
                            .fileImporter(isPresented: $importingStatement, allowedContentTypes: [.commaSeparatedText, .plainText, .pdf, .data]) { result in
                                if case .success(let url) = result {
                                    save()
                                    model.importStatement(connection, url: url)
                                    dismiss()
                                }
                            }
                        if savesPortalLogin {
                            Toggle("Anmeldedaten im Tresor speichern", isOn: $storePortalLogin)
                            if storePortalLogin {
                                TextField("Benutzername", text: $portalUser)
                                    .autocorrectionDisabled()
                                    .textContentType(.username)
                                SecureField(status?.hasPin == true ? "Gespeichertes Passwort (leer = unverändert)" : "Passwort", text: $pin)
                                    .textContentType(.password)
                                Text("GiroCrab trägt beide verschlüsselt gespeichert ins Anmeldeformular ein. „Anmelden“ und den Bestätigungscode bestätigen Sie selbst.")
                                    .appFont(.caption).foregroundStyle(Theme.secondary)
                                    .fixedSize(horizontal: false, vertical: true)
                            }
                        }
                        Button("Web-Sitzung (Cookies) löschen …", role: .destructive) { confirmClear = .portal }
                    } header: { Text("Direkt-Login (Web-Session)").appSectionHeader() }
                    Section {
                        DisclosureGroup("Erweitert: Eigene PSD2/Enable-Banking API (Optional)", isExpanded: $showAdvanced) {
                            Text("Nur für Power-User: Mit eigener Application-ID und PEM-Schlüssel nutzt GiroCrab statt des Web-Logins die PSD2-Schnittstelle von Enable Banking.")
                                .appFont(.caption).foregroundStyle(Theme.secondary)
                            ForEach(advancedKeys, id: \.self) { key in field(key) }
                        }
                    }
                }
                // Browser-only portals keep their session cookies in the app's web
                // store: offer to remove them like for the card portals above.
                if connection.connector == .schwaebischHall {
                    Section {
                        Button {
                            save()
                            dismiss()
                            model.webLoginConnection = model.connections.first { $0.id == connection.id } ?? connection
                        } label: { Label("Anmelden & Abrufen", systemImage: "safari") }
                            .buttonStyle(.borderedProminent)
                        Button("Web-Sitzung (Cookies) löschen …", role: .destructive) { confirmClear = .portal }
                    } header: { Text("Direkt-Login (Web-Session)").appSectionHeader() }
                }
                if isComdirect {
                    Section {
                        LabeledContent("Abrufweg", value: connection.connector == .comdirect
                            ? String(localized: "Schnittstelle der Bank") : String(localized: "Online-Banking (FinTS)"))
                        Button(connection.connector == .comdirect ? "Auf FinTS umstellen …" : "Auf die Schnittstelle der Bank umstellen …") {
                            confirmSwitch = true
                        }
                    } header: {
                        Text("Abrufweg").appSectionHeader()
                    } footer: {
                        Text("Konten, Kategorien und Notizen bleiben erhalten. Beim nächsten Abruf ordnet GiroCrab Konten und Buchungen dem neuen Weg zu.")
                            .appFont(.caption).foregroundStyle(Theme.secondary)
                            .leadingFooter()
                    }
                    if connection.connector == .finTsGeneric {
                        Section {
                            ForEach(postboxKeys, id: \.self) { key in field(key) }
                            Toggle("PostBox über die Schnittstelle der Bank abrufen", isOn: Binding(
                                get: { postbox },
                                set: { on in
                                    // the switch needs the credentials stored
                                    if on { saveSettings(postboxKeys) }
                                    model.setComdirectPostbox(connection.id, on)
                                    postbox = model.comdirectPostboxEnabled(connection.id)
                                }))
                                .disabled(documentsBlocked)
                            if documentsBlocked {
                                Label("Wird nicht abgerufen, solange „Dokumente abrufen“ oben ausgeschaltet ist.", systemImage: "info.circle")
                                    .appFont(.caption).foregroundStyle(Theme.secondary)
                            }
                            if postbox {
                                Button {
                                    save()
                                    dismiss()
                                    model.fetchComdirectPostbox(model.connections.first { $0.id == connection.id } ?? connection)
                                } label: { Label("PostBox jetzt abrufen", systemImage: "tray.and.arrow.down") }
                                    .disabled(model.syncingConnectionID != nil || documentsBlocked)
                            }
                        } header: {
                            Text("PostBox").appSectionHeader()
                        } footer: {
                            Text("Kontoauszüge, Finanzreport und Wertpapierabrechnungen aus der comdirect-PostBox, höchstens einmal pro Woche beim Abruf. Dafür sind Client-ID und Client-Secret der comdirect-API nötig und eine eigene Freigabe in der photoTAN-App. Wertpapierdokumente kommen ans Depot, die übrigen auf das erste Girokonto. Über das Kontextmenü in „Belege & PostBox“ lässt sich ein Dokument einem anderen Konto zuordnen.")
                                .appFont(.caption).foregroundStyle(Theme.secondary)
                                .leadingFooter()
                        }
                    }
                }
                if isConsorsbankFinTS {
                    Section {
                        Toggle("Online-Archiv abrufen", isOn: Binding(
                            get: { postbox },
                            set: { on in
                                model.setComdirectPostbox(connection.id, on)
                                postbox = model.comdirectPostboxEnabled(connection.id)
                            }))
                            .disabled(documentsBlocked)
                        if documentsBlocked {
                            Label("Wird nicht abgerufen, solange „Dokumente abrufen“ oben ausgeschaltet ist.", systemImage: "info.circle")
                                .appFont(.caption).foregroundStyle(Theme.secondary)
                        }
                        if postbox {
                            Button {
                                save()
                                dismiss()
                                model.fetchComdirectPostbox(model.connections.first { $0.id == connection.id } ?? connection)
                            } label: { Label("Online-Archiv jetzt abrufen", systemImage: "tray.and.arrow.down") }
                                .disabled(model.syncingConnectionID != nil || documentsBlocked)
                        }
                    } header: {
                        Text("Online-Archiv").appSectionHeader()
                    } footer: {
                        Text("Kontoauszüge, Abrechnungen, Depotauszüge und Steuerdokumente aus dem Online-Archiv der Consorsbank, höchstens einmal pro Woche beim Abruf. GiroCrab meldet sich dafür mit Kontonummer und PIN dieses Zugangs im Online-Banking an, die Anmeldung geben Sie in der Consorsbank App frei. Die Schnittstelle ist inoffiziell. Gespeicherte Dokumente werden nicht erneut geladen, denn jeder Abruf markiert ein Dokument bei der Bank als gelesen.")
                            .appFont(.caption).foregroundStyle(Theme.secondary)
                            .leadingFooter()
                    }
                }
                Section {
                    // saved by the sheet's own Speichern, so there is a single place to confirm
                    AccountSelectionList(connection: connection, showsSaveButton: false, draft: selection)
                } header: { Text("Kontenauswahl").appSectionHeader() }
                Section {
                    if let st = status {
                        LabeledContent("Konten", value: "\(st.accountCount)")
                        LabeledContent("Buchungen", value: "\(st.bookingCount)")
                        if connection.connector == .enableBanking {
                            consentRows(st)
                        }
                        if isTR {
                            LabeledContent("Sitzung", value: st.hasSession ? sessionText(st) : "keine gespeichert")
                            if st.hasSession {
                                Button("Sitzung löschen …", role: .destructive) { confirmClear = .tradeRepublic }
                            }
                        }
                    }
                    if let t = connection.lastSyncUnix {
                        LabeledContent("Letzter Abruf") { Text(Date(timeIntervalSince1970: TimeInterval(t)), style: .relative) }
                    }
                    if isTR {
                        Button {
                            save()
                            dismiss()
                            model.webLoginConnection = model.connections.first { $0.id == connection.id } ?? connection
                        } label: { Label("Im Browser-Fenster anmelden …", systemImage: "safari") }
                    }
                } header: { Text("Status").appSectionHeader() }
            }
            .formStyle(.grouped)
            Divider()
            ViewThatFits(in: .horizontal) {
                // HIG: the default button (Return) is the rightmost one.
                HStack { removeBtn; Spacer(); cancelBtn; fetchBtn; saveBtn }
                VStack(spacing: 8) {
                    HStack { cancelBtn; Spacer(); fetchBtn; saveBtn }
                    removeBtn
                }
            }
            .padding(14)
        }
        .macFrame(minWidth: 560, minHeight: 640)
        .onAppear(perform: load)
        .task {
            if connection.connector == .advanzia {
                await advanziaSettings.load(connectionID: connection.id, model: model)
            }
        }
        .confirmRemoveConnection($confirmRemove) { dismiss() }
        .confirmationDialog("Abrufweg umstellen?", isPresented: $confirmSwitch, titleVisibility: .visible) {
            Button("Umstellen") {
                model.switchComdirectAccess(connection.id)
                dismiss()
            }
            Button("Abbrechen", role: .cancel) {}
        } message: {
            Text(connection.connector == .comdirect
                ? "GiroCrab ruft comdirect danach per FinTS ab. Die Bankleitzahl kommt aus der IBAN Ihrer Konten. Beim ersten Abruf werden PIN und Freigabe in der App erneut abgefragt."
                : "GiroCrab ruft comdirect danach über die Schnittstelle der Bank ab. Dafür sind Client-ID und Client-Secret nötig.")
        }
        .confirmationDialog("Sitzung löschen?", isPresented: Binding(get: { confirmClear != nil }, set: { if !$0 { confirmClear = nil } }),
                            titleVisibility: .visible, presenting: confirmClear) { kind in
            Button("Sitzung löschen", role: .destructive) {
                switch kind {
                case .portal: Task { await clearPortalSession(connection.connector) }
                case .tradeRepublic:
                    model.clearSession(connection.id)
                    Task { await WebLoginSheet.clearAllTradeRepublicWebData() }
                    status = model.connectionStatus(connection.id)
                }
            }
            Button("Abbrechen", role: .cancel) {}
        } message: { _ in
            Text("Beim nächsten Abruf müssen Sie sich erneut anmelden und die Anmeldung in der App Ihrer Bank bestätigen.")
        }
        .fileImporter(isPresented: $importingKey, allowedContentTypes: [.data, .text, .plainText]) { result in
            if case .success(let url) = result {
                // Key and application ID belong together: the downloaded
                // file is named after the ID.
                if let key = try? EnableBankingKeyFile.read(url) {
                    values["enablebanking.private_key_pem"] = key.pem
                    revealed.insert("enablebanking.private_key_pem")
                    if let id = key.applicationID { values["enablebanking.application_id"] = id }
                }
            }
        }
        .fileDialogDefaultDirectory(EnableBankingKeyFile.downloadsFolder)
    }

    private var sectionTitle: String {
        switch connection.connector {
        case .volksbankKasselGoettingen, .direkt1822, .finTsGeneric: return "FinTS 3.0"
        case .comdirect: return "comdirect REST-API"
        case .payPal: return "PayPal REST-App"
        case .kontist: return "Kontist OAuth2-Client"
        case .bunq: return "bunq API"
        case .ibkr: return "Interactive Brokers Flex Web Service"
        case .alteLeipziger: return "fin4u"
        default: return "Enable Banking (PSD2)"
        }
    }

    private func sessionText(_ st: ConnectionStatus) -> String {
        guard let t = st.sessionSavedUnix else { return String(localized: "gespeichert") }
        let f = RelativeDateTimeFormatter()
        f.locale = .autoupdatingCurrent
        let relative = f.localizedString(for: Date(timeIntervalSince1970: TimeInterval(t)), relativeTo: Date())
        return String(localized: "gespeichert \(relative)")
    }

    @ViewBuilder
    private func field(_ key: String) -> some View {
        let entry = model.settingEntry(key)
        let label = entry?.label ?? settingLabels[key] ?? (key.hasPrefix("fints.url.") ? String(localized: "FinTS-URL") : key)
        let secret = AppModel.isSecretSetting(key, entry)
        VStack(alignment: .leading, spacing: 4) {
            if key.contains("private_key") {
                HStack {
                    Text(label)
                    Spacer()
                    Button("PEM-Datei wählen …") { importingKey = true }
                    revealButton(key)
                }
                if revealed.contains(key) {
                    TextEditor(text: binding(key))
                        .appFont(.caption, design: .monospaced)
                        .frame(height: 90)
                } else {
                    let stored = isEBAppKey(key) ? !(values[key] ?? "").isEmpty : entry?.hasValue == true
                    Text(stored ? "•••••• (gespeichert)" : "nicht gesetzt").foregroundStyle(Theme.secondary)
                }
            } else if secret {
                HStack {
                    Text(label)
                    Spacer()
                    if revealed.contains(key) {
                        TextField(label, text: binding(key)).labelsHidden().frame(maxWidth: 300).multilineTextAlignment(.trailing)
                    } else {
                        SecureField(label, text: binding(key)).labelsHidden().frame(maxWidth: 300).multilineTextAlignment(.trailing)
                    }
                    revealButton(key)
                }
            } else {
                TextField(label, text: binding(key))
                    .autocorrectionDisabled()
            }
            if let help = entry?.help {
                Text(help).appFont(.caption2).foregroundStyle(Theme.secondary)
            }
            if key == Self.ebPemKey {
                Text("Gilt nur für diesen Bankzugang. Eine andere Anwendung braucht beim nächsten Abruf eine neue Zustimmung bei der Bank.")
                    .appFont(.caption2).foregroundStyle(Theme.secondary)
            }
        }
    }

    private func revealButton(_ key: String) -> some View {
        Button {
            if revealed.contains(key) {
                revealed.remove(key)
            } else {
                if (values[key] ?? "").isEmpty {
                    values[key] = key == Self.ebPemKey ? model.connectionEnableBankingKey(connection.id) : model.revealSetting(key)
                }
                revealed.insert(key)
            }
        } label: {
            Image(systemName: revealed.contains(key) ? "eye.slash" : "eye")
        }
        .buttonStyle(.borderless)
        .help(revealed.contains(key) ? "Verbergen" : "Anzeigen")
        .accessibilityLabel(revealed.contains(key) ? Text("Verbergen") : Text("Anzeigen"))
    }

    private func binding(_ key: String) -> Binding<String> {
        Binding(get: { values[key] ?? "" }, set: { values[key] = $0 })
    }

    private var cancelBtn: some View {
        Button(hasChanges ? "Abbrechen" : "Schließen", role: .cancel) { dismiss() }.keyboardShortcut(.cancelAction)
    }
    private var saveBtn: some View {
        Button("Speichern") { saveAll() }
            .disabled(connection.connector == .advanzia && (advanziaSettings.busy || !advanziaSettings.loaded))
            .keyboardShortcut(.defaultAction).buttonStyle(.borderedProminent)
            .disabled(!hasChanges)
    }
    /// Something in the sheet differs from what was loaded (name, login, PIN, settings, account selection).
    private var hasChanges: Bool {
        let l = isTR ? normalizePhoneNumber(input: login) : login.trimmingCharacters(in: .whitespaces)
        if name != connection.displayName || l != connection.loginName { return true }
        if usesPin, storePin != (status?.hasPin ?? false) || !pin.isEmpty { return true }
        if values != initialValues { return true }
        if isFinTs, tanMedium.trimmingCharacters(in: .whitespaces) != initialTanMedium { return true }
        if documentsSupported, documentsFetch != initialDocumentsFetch { return true }
        // Amex portal login (user name, password, save switch) and Advanzia (password, release method)
        // are not in `values`.
        if savesPortalLogin,
           storePortalLogin != initialStorePortalLogin || !pin.isEmpty
            || portalUser.trimmingCharacters(in: .whitespaces) != initialPortalUser { return true }
        if connection.connector == .advanzia, advanziaSettings.hasChanges { return true }
        return selection.hasChanges
    }
    /// Mac: the trash in the list of connections removes it, so the sheet has no button for that.
    @ViewBuilder private var removeBtn: some View {
        #if !os(macOS)
        Button("Bankzugang entfernen …", role: .destructive) { confirmRemove = connection }
        #endif
    }
    /// Icon only: "Speichern & abrufen".
    private var fetchBtn: some View {
        Button { saveAll(thenSync: true) } label: { Image(systemName: "arrow.triangle.2.circlepath") }
            .help("Speichern & abrufen")
            .accessibilityLabel(Text("Speichern & abrufen"))
            .disabled(connection.connector == .advanzia && (advanziaSettings.busy || !advanziaSettings.loaded))
    }

    /// The login's settings and the account selection, saved together; a fetch only starts after
    /// the selection is stored (a switched-off account must not be fetched).
    private func saveAll(thenSync: Bool = false) {
        if connection.connector == .advanzia {
            Task { @MainActor in
                guard let updated = await advanziaSettings.save(connection: connection, name: name, login: login, model: model) else { return }
                dismiss()
                if thenSync { model.requestFullSync(updated) }
            }
            return
        }
        save()
        let id = connection.id
        let draft = selection
        Task { @MainActor in
            if await draft.saveIfChanged(model: model, connectionId: id) {
                if thenSync, let c = model.connections.first(where: { $0.id == id }) { model.requestFullSync(c) }
            } else if let message = draft.error {
                model.showError(String(localized: "Kontenauswahl nicht gespeichert"), message)
            }
        }
        dismiss()
    }

    private func load() {
        name = connection.displayName
        login = connection.loginName
        status = model.connectionStatus(connection.id)
        storePin = status?.hasPin ?? false
        if isComdirect || isConsorsbankFinTS, connection.connector == .finTsGeneric { postbox = model.comdirectPostboxEnabled(connection.id) }
        documentsSupported = model.documentsFetchSupported(connection.id)
        documentsFetch = model.documentsFetchEnabled(connection.id)
        initialDocumentsFetch = documentsFetch
        if isFinTs { tanMedium = model.revealSetting(tanMediumKey) }
        if savesPortalLogin {
            portalUser = model.revealSetting(AppModel.portalLoginKey(connection.id))
            storePortalLogin = status?.hasPin == true || !portalUser.isEmpty
            initialPortalUser = portalUser
            initialStorePortalLogin = storePortalLogin
        }
        if (keys + advancedKeys).contains(where: isEBAppKey) {
            let id = model.connectionEnableBankingApp(connection.id)?.applicationId ?? ""
            let pem = model.connectionEnableBankingKey(connection.id)
            loadedEBApp = (id, pem)
            values[Self.ebIDKey] = id
            values[Self.ebPemKey] = pem
        }
        for key in keys + advancedKeys + postboxKeys where !isEBAppKey(key) {
            let e = model.settingEntry(key)
            if e?.isSecret == true {
                values[key] = model.revealSetting(key)
            } else {
                values[key] = e?.value ?? model.revealSetting(key)
            }
        }
        initialValues = values
        initialTanMedium = tanMedium.trimmingCharacters(in: .whitespaces)
    }

    private func saveSettings(_ keys: [String]) {
        for key in keys where !isEBAppKey(key) {
            let v = values[key] ?? ""
            let current = (model.settingEntry(key)?.isSecret ?? false) ? model.revealSetting(key) : (model.settingEntry(key)?.value ?? "")
            if v != current {
                model.saveSetting(key, v)
            }
        }
    }

    private func save() {
        if documentsSupported, documentsFetch != initialDocumentsFetch {
            model.setDocumentsFetch(connection.id, documentsFetch)
            initialDocumentsFetch = documentsFetch
        }
        let l = isTR ? normalizePhoneNumber(input: login) : login.trimmingCharacters(in: .whitespaces)
        model.updateConnection(connection.id, name: name, login: l)
        if usesPin {
            if !storePin {
                model.setConnectionPin(connection.id, nil)
            } else if !pin.isEmpty {
                model.setConnectionPin(connection.id, pin)
            }
        }
        if savesPortalLogin {
            let key = AppModel.portalLoginKey(connection.id)
            let user = portalUser.trimmingCharacters(in: .whitespaces)
            if !storePortalLogin {
                model.setConnectionPin(connection.id, nil)
                model.saveSetting(key, "")
            } else {
                if user != model.revealSetting(key) { model.saveSetting(key, user) }
                if !pin.isEmpty { model.setConnectionPin(connection.id, pin) }
            }
        }
        saveSettings(keys + advancedKeys + postboxKeys)
        if (keys + advancedKeys).contains(where: isEBAppKey) {
            let id = (values[Self.ebIDKey] ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            let pem = (values[Self.ebPemKey] ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            if id != loadedEBApp.id.trimmingCharacters(in: .whitespacesAndNewlines)
                || pem != loadedEBApp.pem.trimmingCharacters(in: .whitespacesAndNewlines) {
                model.setConnectionEnableBankingApp(connection.id, applicationID: id, pem: pem)
            }
        }
        if isFinTs {
            let medium = tanMedium.trimmingCharacters(in: .whitespaces)
            if medium != model.revealSetting(tanMediumKey) { model.saveSetting(tanMediumKey, medium) }
        }
        pin = ""
        // the sheet can stay open (Speichern): what was saved is the new "unchanged"
        initialValues = values
        initialTanMedium = tanMedium.trimmingCharacters(in: .whitespaces)
        initialPortalUser = portalUser.trimmingCharacters(in: .whitespaces)
        initialStorePortalLogin = storePortalLogin
    }
}

// MARK: - Trade Republic in-app browser login (WKWebView)

/// Opens app.traderepublic.com/login in an embedded WKWebView (solves the AWS
/// WAF natively). As soon as the `tr_session` cookie appears in
/// WKWebsiteDataStore.default(), the cookies are handed to the Rust core,
/// which then opens the WebSocket (connect 31 → cash → timelineTransactions).
struct WebLoginSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let connection: BankConnection
    @State private var found: String?
    @State private var info = String(localized: "Melden Sie sich wie gewohnt an (Telefonnummer, PIN, Code aus der App).")
    /// The old `tr_session` cookie is gone: only now the login page loads, so
    /// any session found afterwards was set in this window.
    @State private var storeCleared = false
    /// Last automatic hand-over per login: if the core rejects a fresh session
    /// right away, the sheet comes back – then the user decides (no loop).
    @MainActor private static var lastAutoApply: [Int64: Date] = [:]

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 10) {
                SymbolTile(symbol: "safari", tint: connection.connector.tint, size: 28)
                VStack(alignment: .leading, spacing: 1) {
                    Text("Trade Republic – Anmeldung im Browser-Fenster").appFont(.headline)
                    Text(info).appFont(.caption).foregroundStyle(Theme.secondary)
                }
                Spacer()
                SyncBatchProgressLabel()
                if found != nil {
                    Label("Sitzung gefunden", systemImage: "checkmark.seal.fill").foregroundStyle(Theme.inflow).appFont(.caption)
                } else {
                    ProgressView().controlSize(.small)
                }
            }
            .padding(14)
            Divider()
            Group {
                if storeCleared {
                    TRWebView(url: URL(string: "https://app.traderepublic.com/login")!, phone: connection.loginName)
                } else {
                    ProgressView()
                }
            }
            .macFrame(minWidth: 720, minHeight: 620)
            Divider()
            // "Alle abrufen": cancelling skips this login (see SyncPromptButtons)
            // no dismiss(): see the setup sheet – the next login's sheet may be open already
            SyncPromptButtons(cancel: { model.cancelWebLogin() }) {
                Button {
                    if let c = found { model.applyBrowserSession(connection, cookies: c) }
                    dismiss()
                } label: {
                    Text("Sitzung übernehmen").frame(maxWidth: .infinity)
                }
                .keyboardShortcut(.defaultAction)
                .buttonStyle(.borderedProminent)
                .disabled(found == nil)
            }
            .padding(12)
        }
        .task {
            // A session cookie left from an earlier (expired) login would be taken
            // over at once, rejected by Trade Republic and bring this sheet back
            // in a loop: drop it before the login page loads.
            await Self.removeSessionCookies()
            storeCleared = true
            // Poll the shared cookie store until a new tr_session shows up.
            while !Task.isCancelled {
                if let c = await Self.trCookies(), c.contains("tr_session=") {
                    if found == nil {
                        found = c
                        let recent = Self.lastAutoApply[connection.id].map { Date().timeIntervalSince($0) < 120 } ?? false
                        guard !recent else {
                            info = String(localized: "Sitzung gefunden – bitte „Sitzung übernehmen“ wählen.")
                            return
                        }
                        Self.lastAutoApply[connection.id] = Date()
                        info = String(localized: "Angemeldet – Sitzung wird übernommen …")
                        try? await Task.sleep(nanoseconds: 800_000_000)
                        guard !Task.isCancelled else { return }
                        model.applyBrowserSession(connection, cookies: c)
                        dismiss()
                    }
                    return
                }
                try? await Task.sleep(nanoseconds: 1_000_000_000)
            }
        }
    }

    /// Cookie or page host of Trade Republic itself: `traderepublic.com` or a
    /// subdomain (not `nottraderepublic.com`, not `traderepublic.com.evil.io`).
    static func isTradeRepublicHost(_ host: String?) -> Bool {
        guard var h = host?.lowercased() else { return false }
        if h.hasPrefix(".") { h.removeFirst() }
        if h.hasSuffix(".") { h.removeLast() }
        return h == "traderepublic.com" || h.hasSuffix(".traderepublic.com")
    }

    @MainActor
    static func removeSessionCookies() async {
        let store = WKWebsiteDataStore.default().httpCookieStore
        for c in await store.allCookies() where isTradeRepublicHost(c.domain) && c.name == "tr_session" {
            await store.deleteCookie(c)
        }
    }

    /// Removes everything the embedded browser keeps for traderepublic.com:
    /// all cookies (tr_session, tr_refresh, …) and all website data records.
    @MainActor
    static func clearAllTradeRepublicWebData() async {
        let data = WKWebsiteDataStore.default()
        let cookieStore = data.httpCookieStore
        for c in await cookieStore.allCookies() where isTradeRepublicHost(c.domain) {
            await cookieStore.deleteCookie(c)
        }
        let types = WKWebsiteDataStore.allWebsiteDataTypes()
        let records = await data.dataRecords(ofTypes: types)
        await data.removeData(ofTypes: types, for: records.filter { isTradeRepublicHost($0.displayName) || $0.displayName.lowercased().contains("traderepublic.com") })
    }

    @MainActor
    static func trCookies() async -> String? {
        let cookies = await WKWebsiteDataStore.default().httpCookieStore.allCookies()
        let tr = cookies.filter { isTradeRepublicHost($0.domain) }
        guard !tr.isEmpty else { return nil }
        return tr.map { "\($0.name)=\($0.value)" }.joined(separator: "; ")
    }
}

/// Pre-fills the saved +49… phone number into the Trade Republic login form
/// (React-controlled input: native value setter + input event), retrying while
/// the single-page app renders.
final class TRWebCoordinator: NSObject, WKNavigationDelegate {
    let phone: String
    init(phone: String) { self.phone = phone }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        // The phone number goes into Trade Republic's own form only (https).
        guard !phone.isEmpty, webView.url?.scheme == "https",
              WebLoginSheet.isTradeRepublicHost(webView.url?.host) else { return }
        let digits = phone.replacingOccurrences(of: "\"", with: "")
        let js = """
        (function fill(tries) {
          var el = document.querySelector('input[type="tel"], input[autocomplete="tel"], input[name*="phone" i], input[id*="phone" i]');
          if (!el) { if (tries > 0) setTimeout(function(){ fill(tries - 1); }, 500); return; }
          var v = "\(digits)";
          if (el.maxLength > 0 && el.maxLength < v.length && v.startsWith("+49")) v = v.substring(3);
          var setter = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, 'value').set;
          setter.call(el, v);
          el.dispatchEvent(new Event('input', { bubbles: true }));
          el.dispatchEvent(new Event('change', { bubbles: true }));
        })(20);
        """
        webView.evaluateJavaScript(js, completionHandler: nil)
    }
}

#if os(macOS)
struct TRWebView: NSViewRepresentable {
    let url: URL
    var phone: String = ""
    func makeCoordinator() -> TRWebCoordinator { TRWebCoordinator(phone: phone) }
    func makeNSView(context: Context) -> WKWebView {
        let cfg = WKWebViewConfiguration()
        cfg.websiteDataStore = .default()
        let v = WKWebView(frame: .zero, configuration: cfg)
        v.navigationDelegate = context.coordinator
        v.load(URLRequest(url: url))
        return v
    }
    func updateNSView(_ nsView: WKWebView, context: Context) {}
}
#else
struct TRWebView: UIViewRepresentable {
    let url: URL
    var phone: String = ""
    func makeCoordinator() -> TRWebCoordinator { TRWebCoordinator(phone: phone) }
    func makeUIView(context: Context) -> WKWebView {
        let cfg = WKWebViewConfiguration()
        cfg.websiteDataStore = .default()
        let v = WKWebView(frame: .zero, configuration: cfg)
        v.navigationDelegate = context.coordinator
        v.load(URLRequest(url: url))
        return v
    }
    func updateUIView(_ uiView: WKWebView, context: Context) {}
}
#endif

/// HIG: removing a bank login deletes data for good, so it is always confirmed
/// (destructive button + Cancel + what gets lost).
struct RemoveConnectionConfirmation: ViewModifier {
    @Environment(AppModel.self) private var model
    @Binding var connection: BankConnection?
    var onRemoved: () -> Void = {}

    func body(content: Content) -> some View {
        content.confirmationDialog(
            Text("„\(connection?.displayName ?? "")“ entfernen?"),
            isPresented: Binding(get: { connection != nil }, set: { if !$0 { connection = nil } }),
            titleVisibility: .visible,
            presenting: connection
        ) { c in
            Button("Entfernen", role: .destructive) { model.removeConnection(c.id); onRemoved() }
            Button("Abbrechen", role: .cancel) {}
        } message: { _ in
            Text("Alle Konten dieses Bankzugangs werden samt Buchungen, Belegen und gespeicherten Zugangsdaten aus dem Tresor gelöscht. Das lässt sich nicht widerrufen.")
        }
    }
}

extension View {
    func confirmRemoveConnection(_ connection: Binding<BankConnection?>, onRemoved: @escaping () -> Void = {}) -> some View {
        modifier(RemoveConnectionConfirmation(connection: connection, onRemoved: onRemoved))
    }
}
