import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import QtQuick.Dialogs
import QtCore

// ConnectionsUI.swift › ConnectionEditSheet: name, login, stored PIN, the settings
// this connector needs, status (consent, session) and removal.
Sheet {
    id: sheet
    property var connection: null
    sheetWidth: 560
    property string name: ""
    property string login: ""
    property string pin: ""
    property bool storePin: false
    property var values: ({})
    /// Enable Banking application of this login as loaded (#824): saved per login, only when changed.
    property var loadedEBApp: ({ id: "", pem: "" })
    readonly property var ebAppKeys: ["enablebanking.application_id", "enablebanking.private_key_pem"]
    function isEBAppKey(key) { return ebAppKeys.indexOf(key) >= 0 }
    property var revealed: ({})
    property var status: null
    property bool showAdvanced: false
    /// comdirect via FinTS: PostBox through the bank's API as well (#838).
    property bool postbox: false

    readonly property string kind: connection ? connection.connector : ""
    readonly property var descriptor: connection ? AppModel.descriptor(kind) : null
    readonly property bool isTR: kind === "TradeRepublic"
    readonly property bool isPortal: kind === "AmericanExpress" || kind === "AmazonVisa"
    /// comdirect logins can switch between FinTS and comdirect's own API.
    readonly property bool isComdirect: kind === "Comdirect"
        || (kind === "FinTsGeneric" && ["20041111", "20041133", "20041144", "20041155"].indexOf(connection.bank_code) >= 0)
    readonly property bool usesPin: ["VolksbankKasselGoettingen", "Direkt1822", "FinTsGeneric", "Comdirect", "TradeRepublic", "Bunq", "Ibkr", "Easybank", "AlteLeipziger", "VolkswagenBank", "Uberspace", "Ffb", "FordMoney", "OpelDirektbank", "Bhw", "Kontist"].indexOf(kind) >= 0
    /// ConnectionsUI.swift › ConnectionEditSheet: these portals take a password, not a PIN
    readonly property bool usesPassword: ["Easybank", "AlteLeipziger", "Uberspace", "FordMoney", "OpelDirektbank", "Bhw", "Kontist"].indexOf(kind) >= 0
    /// AppModel+Rah.swift › usesRah10 (#460): the secret is the key file's password, the login is fixed.
    readonly property bool isRah10: !!connection && kind === "FinTsGeneric"
        && (AppModel.run(() => AppModel.core("connection_key", { connection_id: connection.id })) || "").endsWith(":rah10")
    /// Global setting keys relevant for this connector.
    readonly property var allKeys: {
        switch (kind) {
        case "VolksbankKasselGoettingen": return ["fints.url.52090000", "fints.product_id", "fints.product_version"]
        case "Direkt1822": return ["fints.url.50050201", "fints.product_id", "fints.product_version"]
        case "FinTsGeneric": return ["fints.url." + connection.bank_code, "fints.product_id", "fints.product_version"]
        case "Comdirect": return ["comdirect.client_id", "comdirect.client_secret"]
        case "PayPal": return ["paypal.client_id", "paypal.client_secret", "paypal.environment"]
        case "Kontist": return ["kontist.client_id", "kontist.client_secret"]
        case "Bunq": return ["bunq.environment"]
        case "EnableBanking": return ["enablebanking.application_id", "enablebanking.private_key_pem", "enablebanking.redirect_url"]
        default: return []
        }
    }
    // With GiroCrab's own registration built in, it is chosen in Settings, not per bank.
    readonly property var keys: AppModel.fintsBuiltinAvailable ? allKeys.filter(k => k !== "fints.product_id") : allKeys
    /// comdirect via FinTS: the API credentials for the PostBox (#838), shown in its section.
    readonly property var postboxKeys: isComdirect && kind === "FinTsGeneric" ? ["comdirect.client_id", "comdirect.client_secret"] : []
    /// Optional power-user PSD2 credentials (collapsed "Erweitert" section).
    readonly property var advancedKeys: kind === "AmericanExpress"
        ? ["enablebanking.use.amex", "enablebanking.application_id", "enablebanking.private_key_pem", "enablebanking.redirect_url", "enablebanking.aspsp.amex", "enablebanking.country.amex"]
        : kind === "AmazonVisa"
        ? ["enablebanking.use.amazonvisa", "enablebanking.application_id", "enablebanking.private_key_pem", "enablebanking.redirect_url", "enablebanking.aspsp.amazonvisa", "enablebanking.country.amazonvisa"]
        : []
    readonly property string sectionTitle: {
        switch (kind) {
        case "VolksbankKasselGoettingen": case "Direkt1822": case "FinTsGeneric": return "FinTS 3.0"
        case "Comdirect": return "comdirect REST-API"
        case "PayPal": return "PayPal REST-App"
        case "Kontist": return "Kontist OAuth2-Client"
        case "Bunq": return "bunq API"
        case "Ibkr": return "Interactive Brokers Flex Web Service"
        default: return "Enable Banking (PSD2)"
        }
    }

    function edit(c) { connection = c; open() }
    onAboutToShow: load()
    onClosed: { pin = ""; values = ({}); revealed = ({}) }

    function setValue(key, v) { const x = Object.assign({}, values); x[key] = v; values = x }
    function toggleReveal(key) {
        const r = Object.assign({}, revealed)
        if (r[key]) delete r[key]
        else {
            if (!(values[key] || ""))
                setValue(key, key === "enablebanking.private_key_pem" ? AppModel.connectionEnableBankingKey(connection.id) : AppModel.revealSetting(key))
            r[key] = true
        }
        revealed = r
    }
    function load() {
        name = connection.display_name
        login = connection.login_name
        pin = ""
        status = AppModel.connectionStatus(connection.id)
        storePin = status ? status.has_pin : false
        postbox = isComdirect && kind === "FinTsGeneric" ? AppModel.comdirectPostbox(connection.id) : false
        revealed = ({})
        showAdvanced = false
        const v = {}
        if (keys.concat(advancedKeys).some(isEBAppKey)) {
            const app = AppModel.connectionEnableBankingApp(connection.id)
            const pem = AppModel.connectionEnableBankingKey(connection.id)
            loadedEBApp = { id: app ? app.application_id : "", pem: pem }
            v["enablebanking.application_id"] = loadedEBApp.id
            v["enablebanking.private_key_pem"] = pem
        }
        for (const key of keys.concat(advancedKeys, postboxKeys)) {
            if (isEBAppKey(key)) continue
            const e = AppModel.settingEntry(key)
            v[key] = e && e.is_secret ? AppModel.revealSetting(key) : (e ? e.value : AppModel.revealSetting(key))
        }
        values = v
    }
    function save() {
        const l = isTR ? AppModel.core("normalize_phone_number", { input: login }) : login.trim()
        AppModel.updateConnection(connection.id, name, l)
        if (usesPin) {
            if (!storePin) AppModel.setConnectionPin(connection.id, null)
            else if (pin !== "") AppModel.setConnectionPin(connection.id, pin)
        }
        saveSettings(keys.concat(advancedKeys, postboxKeys))
        if (keys.concat(advancedKeys).some(isEBAppKey)) {
            const id = (values["enablebanking.application_id"] || "").trim()
            const pem = (values["enablebanking.private_key_pem"] || "").trim()
            if (id !== loadedEBApp.id.trim() || pem !== loadedEBApp.pem.trim())
                AppModel.setConnectionEnableBankingApp(connection.id, id, pem)
        }
        pin = ""
    }
    function saveSettings(list) {
        for (const key of list) {
            if (isEBAppKey(key)) continue
            const e = AppModel.settingEntry(key)
            const v = values[key] || ""
            const current = e && e.is_secret ? AppModel.revealSetting(key) : (e ? e.value : "")
            if (v !== current) AppModel.saveSetting(key, v)
        }
    }
    /// ConnectionsUI.swift › saveAll(thenSync:): the login's settings and the account selection,
    /// saved together; a fetch only starts after the selection is stored (a switched-off
    /// account must not be fetched).
    function saveAll(thenSync) {
        save()
        const id = connection.id
        selection.saveIfChanged((ok, message) => {
            if (ok) {
                const c = AppModel.connections.find(x => x.id === id)
                if (thenSync && c) AppModel.requestSync(c)
            } else {
                AppModel.errorTitle = qsTr("Kontenauswahl nicht gespeichert")
                AppModel.errorMessage = message
            }
        })
        close()
    }
    function current() { return AppModel.connections.find(c => c.id === connection.id) || connection }
    /// The other save-and-go actions (log in, PostBox, reconnect, import): like `saveAll`, the
    /// action starts only once the account selection is stored, never with the old one.
    function saveThen(action) {
        save()
        const id = connection.id
        const fallback = current()
        selection.saveIfChanged((ok, message) => {
            // after the sheet has closed, as before (the action may open another sheet)
            if (ok) Qt.callLater(() => action(AppModel.connections.find(x => x.id === id) || fallback))
            else {
                AppModel.errorTitle = qsTr("Kontenauswahl nicht gespeichert")
                AppModel.errorMessage = message
            }
        })
        close()
    }

    /// PSD2 consent (PSD2Consent.swift): end date and stage.
    readonly property var consentEnd: status && status.consent_valid_until ? new Date(status.consent_valid_until) : null
    readonly property int consentDaysLeft: consentEnd ? Math.floor((consentEnd - new Date()) / 86400000) : 0
    readonly property string consentStage: !consentEnd || isNaN(consentEnd) ? "none" : consentEnd <= new Date() ? "expired" : consentDaysLeft < 14 ? "expiringSoon" : "valid"

    FileDialog {
        id: pemDialog
        title: qsTr("PEM-Datei wählen …")
        currentFolder: StandardPaths.writableLocation(StandardPaths.DownloadLocation)
        nameFilters: [qsTr("Privater Schlüssel (*.pem *.key *.txt)"), qsTr("Alle Dateien (*)")]
        onAccepted: {
            // Key and application ID belong together: the downloaded file is named after the ID.
            try {
                const k = AppModel.readEnableBankingKeyFile(selectedFile)
                sheet.setValue("enablebanking.private_key_pem", k.pem)
                const r = Object.assign({}, sheet.revealed); r["enablebanking.private_key_pem"] = true; sheet.revealed = r
                if (k.applicationID) sheet.setValue("enablebanking.application_id", k.applicationID)
            } catch (e) { AppModel.toast = e.message }
        }
    }
    ConfirmDialog {
        id: confirmRemove
        heading: sheet.connection ? qsTr("„%1“ entfernen?").arg(sheet.connection.display_name) : ""
        message: qsTr("Alle Konten dieses Bankzugangs werden samt Buchungen, Belegen und gespeicherten Zugangsdaten aus dem Tresor gelöscht. Das lässt sich nicht widerrufen.")
        actionTitle: qsTr("Entfernen")
        onConfirmed: { const id = sheet.connection.id; sheet.close(); AppModel.removeConnection(id) }
    }
    ConfirmDialog {
        id: confirmSwitch
        destructive: false
        heading: qsTr("Abrufweg umstellen?")
        message: sheet.kind === "Comdirect"
            ? qsTr("GiroCrab ruft comdirect danach per FinTS ab. Die Bankleitzahl kommt aus der IBAN Ihrer Konten. Beim ersten Abruf werden PIN und Freigabe in der App erneut abgefragt.")
            : qsTr("GiroCrab ruft comdirect danach über die Schnittstelle der Bank ab. Dafür sind Client-ID und Client-Secret nötig.")
        actionTitle: qsTr("Umstellen")
        onConfirmed: { const id = sheet.connection.id; sheet.close(); AppModel.switchComdirectAccess(id) }
    }
    ConfirmDialog {
        id: confirmClear
        /// "portal" (web-portal cookies) or "tradeRepublic" (stored session + browser cookies).
        property string kind: "tradeRepublic"
        heading: qsTr("Sitzung löschen?")
        message: qsTr("Beim nächsten Abruf müssen Sie sich erneut anmelden und die Anmeldung in der App Ihrer Bank bestätigen.")
        actionTitle: qsTr("Sitzung löschen")
        onConfirmed: {
            if (kind === "portal") { AppModel.clearPortalSession(sheet.kind, sheet.connection.id); return }
            AppModel.clearSession(sheet.connection.id)
            AppModel.clearTradeRepublicWebData(false, sheet.connection.id)
            sheet.status = AppModel.connectionStatus(sheet.connection.id)
        }
    }
    FileDialog {
        id: statementDialog
        title: qsTr("CSV- oder PDF-Kontoauszug importieren …")
        nameFilters: [qsTr("Kontoauszüge (*.csv *.txt *.pdf)"), qsTr("Alle Dateien (*)")]
        currentFolder: StandardPaths.writableLocation(StandardPaths.DownloadLocation)
        onAccepted: { const file = selectedFile; sheet.saveThen(c => AppModel.importStatement(c, file)) }
    }

    ColumnLayout {
        width: sheet.availableWidth
        spacing: 12
        RowLayout {
            spacing: 10
            SymbolTile { symbol: sheet.connection ? sheet.connection.sf_symbol : ""; size: 34; tint: Theme.accent }
            ColumnLayout { spacing: 2; Layout.fillWidth: true
                Text { text: sheet.connection ? sheet.connection.display_name : ""; font.pixelSize: Theme.body + 1; font.weight: Font.DemiBold; color: Theme.text }
                Text { text: sheet.descriptor ? sheet.descriptor.protocol : ""; font.pixelSize: Theme.caption; color: Theme.secondary } }
        }
        Rectangle { Layout.fillWidth: true; height: 1; color: Theme.border }
        ScrollView {
            ScrollBar.vertical: ScrollViewBar {}
            id: scroll
            Layout.fillWidth: true
            Layout.preferredHeight: Math.min(form.implicitHeight, 520, sheet.maxBodyHeight - 150)
            contentWidth: availableWidth
            clip: true
            ColumnLayout {
                id: form
                width: scroll.availableWidth
                spacing: 16
                SettingsFormSection {
                    title: qsTr("Zugang")
                    SettingsFormRow { label: qsTr("Anzeigename")
                        TextField { text: sheet.name; onTextEdited: sheet.name = text; Layout.fillWidth: true; Layout.preferredWidth: 300 } }
                    SettingsFormRow { label: sheet.descriptor ? sheet.descriptor.login_label : qsTr("Benutzerkennung")
                        TextField { text: sheet.login; onTextEdited: sheet.login = text; enabled: !sheet.isRah10; Layout.fillWidth: true; Layout.preferredWidth: 300 } }
                    SettingsFormRow { visible: sheet.isTR && sheet.login !== ""; label: qsTr("Gespeichert als")
                              value: sheet.isTR && sheet.login !== "" ? AppModel.core("normalize_phone_number", { input: sheet.login }) : "" }
                    SettingsFormRow {
                        readonly property string blz: !sheet.connection ? "" : sheet.connection.bank_code !== "" ? sheet.connection.bank_code : (sheet.descriptor ? sheet.descriptor.bank_code : "")
                        visible: blz !== ""; label: qsTr("BLZ"); value: blz
                    }
                    CheckBox {
                        visible: sheet.usesPin
                        text: sheet.isRah10 ? qsTr("Passwort im Tresor speichern") : sheet.kind === "Bunq" ? qsTr("API-Schlüssel im Tresor speichern") : sheet.kind === "Ibkr" ? qsTr("Flex-Query-ID im Tresor speichern") : sheet.kind === "Bhw" ? qsTr("Anmeldedaten im Tresor speichern") : sheet.usesPassword ? qsTr("Passwort im Tresor speichern") : qsTr("PIN im Tresor speichern")
                        checked: sheet.storePin
                        onToggled: sheet.storePin = checked
                    }
                    TextField {
                        visible: sheet.usesPin && sheet.storePin
                        Layout.fillWidth: true
                        echoMode: TextInput.Password
                        text: sheet.pin; onTextEdited: sheet.pin = text
                        placeholderText: sheet.isRah10 ? qsTr("Passwort der Schlüsseldatei (leer = unverändert)") : sheet.kind === "Bhw" ? (sheet.status && sheet.status.has_pin ? qsTr("Gespeicherte Anmeldedaten (leer = unverändert)") : qsTr("Vorname;TT.MM.JJJJ;PLZ")) : sheet.status && sheet.status.has_pin ? qsTr("Gespeicherte PIN (leer = unverändert)") : (sheet.kind === "Ibkr" ? qsTr("Flex-Query-ID") : sheet.usesPassword ? qsTr("Passwort") : qsTr("PIN"))
                    }
                }
                SettingsFormSection {
                    visible: sheet.keys.length > 0
                    title: sheet.sectionTitle
                    Repeater { model: sheet.keys; KeyField { required property string modelData; key: modelData } }
                }
                SettingsFormSection {
                    visible: sheet.isPortal
                    title: qsTr("Direkt-Login (Web-Session)")
                    Text { Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: Theme.caption; color: Theme.secondary
                           text: qsTr("Melden Sie sich im Browser-Fenster mit Ihrem normalen Benutzernamen, Passwort und 2FA an. GiroCrab liest die Umsätze direkt aus dem Portal – es sind keine API-Schlüssel nötig. Die Sitzung bleibt für spätere Abrufe erhalten.") }
                    Button { text: qsTr("Anmelden && Abrufen"); highlighted: true
                             onClicked: sheet.saveThen(c => { AppModel.webLoginConnection = c }) }
                    IconButton { symbol: "doc.badge.plus"; text: qsTr("CSV- oder PDF-Kontoauszug importieren …"); tint: Theme.accent
                                 onClicked: statementDialog.open() }
                    Button { text: qsTr("Web-Sitzung (Cookies) löschen …"); palette.buttonText: Theme.negative
                             onClicked: { confirmClear.kind = "portal"; confirmClear.open() } }
                }
                // Browser-only portal: sign in, and remove its session cookies like for the cards above.
                SettingsFormSection {
                    visible: sheet.kind === "SchwaebischHall"
                    title: qsTr("Direkt-Login (Web-Session)")
                    Button { text: qsTr("Anmelden && Abrufen"); highlighted: true
                             onClicked: sheet.saveThen(c => { AppModel.webLoginConnection = c }) }
                    Button { text: qsTr("Web-Sitzung (Cookies) löschen …"); palette.buttonText: Theme.negative
                             onClicked: { confirmClear.kind = "portal"; confirmClear.open() } }
                }
                SettingsFormSection {
                    visible: sheet.isPortal
                    RowLayout {
                        Layout.fillWidth: true
                        SFIcon { symbol: sheet.showAdvanced ? "chevron.down" : "chevron.right"; size: 10; color: Theme.secondary }
                        Text { text: qsTr("Erweitert: Eigene PSD2/Enable-Banking API (Optional)"); font.pixelSize: Theme.body; color: Theme.text; Layout.fillWidth: true
                               MouseArea { anchors.fill: parent; cursorShape: Qt.PointingHandCursor; onClicked: sheet.showAdvanced = !sheet.showAdvanced } }
                    }
                    Text { visible: sheet.showAdvanced; Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: Theme.caption; color: Theme.secondary
                           text: qsTr("Nur für Power-User: Mit eigener Application-ID und PEM-Schlüssel nutzt GiroCrab statt des Web-Logins die PSD2-Schnittstelle von Enable Banking.") }
                    Repeater { model: sheet.showAdvanced ? sheet.advancedKeys : []; KeyField { required property string modelData; key: modelData } }
                }
                SettingsFormSection {
                    visible: sheet.isComdirect
                    title: qsTr("Abrufweg")
                    footer: qsTr("Konten, Kategorien und Notizen bleiben erhalten. Beim nächsten Abruf ordnet GiroCrab Konten und Buchungen dem neuen Weg zu.")
                    SettingsFormRow { label: qsTr("Abrufweg")
                                      value: sheet.kind === "Comdirect" ? qsTr("Schnittstelle der Bank") : qsTr("Online-Banking (FinTS)") }
                    Button { text: sheet.kind === "Comdirect" ? qsTr("Auf FinTS umstellen …") : qsTr("Auf die Schnittstelle der Bank umstellen …")
                             onClicked: confirmSwitch.open() }
                }
                SettingsFormSection {
                    visible: sheet.isComdirect && sheet.kind === "FinTsGeneric"
                    title: qsTr("PostBox")
                    footer: qsTr("Kontoauszüge, Finanzreport und Wertpapierabrechnungen aus der comdirect-PostBox, höchstens einmal pro Woche beim Abruf. Dafür sind Client-ID und Client-Secret der comdirect-API nötig und eine eigene Freigabe in der photoTAN-App. Wertpapierdokumente kommen ans Depot, die übrigen auf das erste Girokonto. Über das Kontextmenü in „Belege & PostBox“ lässt sich ein Dokument einem anderen Konto zuordnen.")
                    Repeater { model: sheet.postboxKeys; KeyField { required property string modelData; key: modelData } }
                    CheckBox {
                        text: qsTr("PostBox über die Schnittstelle der Bank abrufen")
                        checked: sheet.postbox
                        onToggled: {
                            // the switch needs the credentials stored
                            if (checked) sheet.saveSettings(sheet.postboxKeys)
                            AppModel.setComdirectPostbox(sheet.connection.id, checked)
                            sheet.postbox = AppModel.comdirectPostbox(sheet.connection.id)
                            checked = Qt.binding(() => sheet.postbox)
                        }
                    }
                    Button {
                        visible: sheet.postbox
                        text: qsTr("PostBox jetzt abrufen")
                        enabled: AppModel.syncingConnectionID === null
                        onClicked: sheet.saveThen(c => AppModel.fetchComdirectPostbox(c))
                    }
                }
                SettingsFormSection {
                    title: qsTr("Kontenauswahl")
                    // saved by the sheet's own Speichern, so there is a single place to confirm (#720)
                    AccountSelectionList { id: selection; connection: sheet.visible ? sheet.connection : null; showsSaveButton: false }
                }
                SettingsFormSection {
                    title: qsTr("Status")
                    SettingsFormRow { visible: sheet.status !== null; label: qsTr("Konten"); value: sheet.status ? String(sheet.status.account_count) : "" }
                    SettingsFormRow { visible: sheet.status !== null; label: qsTr("Buchungen"); value: sheet.status ? String(sheet.status.booking_count) : "" }
                    // PSD2 consent: end date, warning before expiry, accounts without access, "Neu verbinden …".
                    SettingsFormRow {
                        visible: sheet.kind === "EnableBanking" && sheet.status !== null
                        label: qsTr("Zustimmung")
                        Text {
                            text: sheet.consentStage === "none" ? qsTr("keine Zustimmung gespeichert")
                                : qsTr("gültig bis %1").arg(sheet.consentEnd.toLocaleDateString(Fmt.locale, Fmt.pattern("d. MMM yyyy")))
                            font.pixelSize: Theme.callout + 1; color: sheet.consentStage === "none" ? Theme.secondary : Theme.text
                        }
                        SFIcon { visible: sheet.consentStage === "expired" || sheet.consentStage === "expiringSoon"; symbol: "exclamationmark.triangle"; size: 12
                                 color: sheet.consentStage === "expired" ? Theme.negative : Theme.warning }
                        Text {
                            visible: sheet.consentStage === "expired" || sheet.consentStage === "expiringSoon"
                            text: sheet.consentStage === "expired" ? qsTr("abgelaufen") : qsTr("noch %1 Tage").arg(Math.max(0, sheet.consentDaysLeft))
                            font.pixelSize: Theme.callout + 1; color: sheet.consentStage === "expired" ? Theme.negative : Theme.warning
                        }
                    }
                    SettingsFormRow {
                        visible: sheet.kind === "EnableBanking" && sheet.status !== null && sheet.status.revoked_account_count > 0
                        label: qsTr("Ohne Zugriff")
                        SFIcon { symbol: "lock.slash"; size: 12; color: Theme.warning }
                        Text { text: qsTr("%1 Konten").arg(sheet.status ? sheet.status.revoked_account_count : 0); font.pixelSize: Theme.body; color: Theme.warning
                               ToolTip.visible: revokedHover.hovered; ToolTip.text: qsTr("Diese Konten sind in der Zustimmung nicht mehr enthalten. Buchungen bleiben erhalten; Konto bei Bedarf selbst löschen.")
                               HoverHandler { id: revokedHover } }
                    }
                    IconButton {
                        visible: sheet.kind === "EnableBanking" && sheet.status !== null
                        symbol: "arrow.triangle.2.circlepath"; text: qsTr("Neu verbinden …"); tint: Theme.accent
                        ToolTip.text: qsTr("Neue Zustimmung bei der Bank erteilen (z. B. vor Ablauf oder um Konten hinzuzufügen oder zu entfernen).")
                        onClicked: sheet.saveThen(c => AppModel.reconnect(c))
                    }
                    Text { visible: sheet.kind === "EnableBanking" && sheet.status !== null; Layout.fillWidth: true; wrapMode: Text.WordWrap
                           font.pixelSize: Theme.caption; color: Theme.secondary
                           text: qsTr("Konten in Enable Banking verknüpft oder entfernt? Das wirkt erst nach „Neu verbinden“.") }
                    SettingsFormRow {
                        visible: sheet.isTR && sheet.status !== null
                        label: qsTr("Sitzung")
                        value: !sheet.status ? "" : !sheet.status.has_session ? qsTr("keine gespeichert")
                            : sheet.status.session_saved_unix === null ? qsTr("gespeichert") : qsTr("gespeichert %1").arg(Fmt.relative(sheet.status.session_saved_unix))
                    }
                    Button {
                        visible: sheet.isTR && sheet.status !== null && sheet.status.has_session
                        text: qsTr("Sitzung löschen …"); palette.buttonText: Theme.negative
                        onClicked: { confirmClear.kind = "tradeRepublic"; confirmClear.open() }
                    }
                    SettingsFormRow { visible: sheet.connection !== null && sheet.connection.last_sync_unix !== null; label: qsTr("Letzter Abruf")
                              value: sheet.connection && sheet.connection.last_sync_unix !== null ? Fmt.relative(sheet.connection.last_sync_unix) : "" }
                    IconButton {
                        visible: sheet.isTR
                        symbol: "safari"; text: qsTr("Im Browser-Fenster anmelden …"); tint: Theme.accent
                        onClicked: sheet.saveThen(c => { AppModel.webLoginConnection = c })
                    }
                }
            }
        }
        Rectangle { Layout.fillWidth: true; height: 1; color: Theme.border }
        // HIG: the default button (Return) is the rightmost one.
        RowLayout {
            Layout.fillWidth: true
            Button { text: qsTr("Bankzugang entfernen …"); palette.buttonText: Theme.negative; onClicked: confirmRemove.open() }
            Item { Layout.fillWidth: true }
            Button { text: qsTr("Abbrechen"); onClicked: sheet.close() }
            Button { text: qsTr("Speichern && abrufen"); onClicked: sheet.saveAll(true) }
            Button { text: qsTr("Speichern"); highlighted: true; onClicked: sheet.saveAll(false) }
        }
    }

    // One setting of this connector (`field(_:)`): PEM key, secret with reveal, or plain text.
    component KeyField: ColumnLayout {
        id: kf
        property string key
        readonly property var entry: AppModel.settingEntry(key)
        readonly property string label: AppModel.settingLabel(key)
        readonly property bool secret: entry ? entry.is_secret : false
        readonly property bool isRevealed: !!sheet.revealed[key]
        readonly property bool isPem: key.indexOf("private_key") >= 0
        Layout.fillWidth: true
        spacing: 4
        RowLayout {
            Layout.fillWidth: true
            spacing: 8
            Text { text: kf.label; font.pixelSize: Theme.body; color: Theme.text; wrapMode: Text.WordWrap
                   Layout.fillWidth: true; Layout.preferredWidth: 200 }
            Button { visible: kf.isPem; text: qsTr("PEM-Datei wählen …"); onClicked: pemDialog.open() }
            TextField {
                visible: !kf.isPem
                Layout.fillWidth: true; Layout.preferredWidth: 260
                horizontalAlignment: kf.secret ? TextInput.AlignRight : TextInput.AlignLeft
                echoMode: kf.secret && !kf.isRevealed ? TextInput.Password : TextInput.Normal
                text: sheet.values[kf.key] || ""
                placeholderText: kf.label
                onTextEdited: sheet.setValue(kf.key, text)
            }
            IconButton {
                visible: kf.isPem || kf.secret
                symbol: kf.isRevealed ? "eye.slash" : "eye"
                ToolTip.text: kf.isRevealed ? qsTr("Verbergen") : qsTr("Anzeigen")
                onClicked: sheet.toggleReveal(kf.key)
            }
        }
        TextArea {
            visible: kf.isPem && kf.isRevealed
            Layout.fillWidth: true; Layout.preferredHeight: 90
            font.family: "monospace"; font.pixelSize: Theme.caption
            text: sheet.values[kf.key] || ""
            onTextChanged: if (visible && text !== (sheet.values[kf.key] || "")) sheet.setValue(kf.key, text)
        }
        Text {
            visible: kf.isPem && !kf.isRevealed
            text: (sheet.isEBAppKey(kf.key) ? !!(sheet.values[kf.key] || "") : !!(kf.entry && kf.entry.has_value))
                  ? qsTr("•••••• (gespeichert)") : qsTr("nicht gesetzt")
            font.pixelSize: Theme.body; color: Theme.secondary
        }
        Text { visible: kf.entry !== null && kf.entry.help !== ""; text: kf.entry ? kf.entry.help : ""; Layout.fillWidth: true; wrapMode: Text.WordWrap
               font.pixelSize: Theme.caption2; color: Theme.secondary }
        Text { visible: kf.key === "enablebanking.private_key_pem"; Layout.fillWidth: true; wrapMode: Text.WordWrap
               text: qsTr("Gilt nur für diesen Bankzugang. Eine andere Anwendung braucht beim nächsten Abruf eine neue Zustimmung bei der Bank.")
               font.pixelSize: Theme.caption2; color: Theme.secondary }
    }
}
