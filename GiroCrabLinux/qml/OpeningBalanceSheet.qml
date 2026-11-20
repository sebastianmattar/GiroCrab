import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// OpeningBalanceUI.swift › OpeningBalanceSheet: Startsaldo of an account (GiroCrab/GiroCrab#212),
// as in MoneyMoney: for accounts whose balance GiroCrab computes from the bookings, the
// balance is the Startsaldo plus every booking from its date on. "Kalibrieren" fills in
// the Startsaldo that yields a given balance, e.g. the one on the last statement.
// Nothing is stored before "Speichern". Driven by `AppModel.openingBalanceRequest`.
Sheet {
    id: sheet
    readonly property var request: AppModel.openingBalanceRequest
    readonly property var account: request ? (AppModel.accountsByID[request.accountId] || null) : null
    readonly property string currency: account ? account.currency : "EUR"
    visible: request !== null
    onAboutToShow: load()
    onClosed: AppModel.openingBalanceRequest = null
    sheetWidth: 540
    padding: 0

    property string amountText: ""
    property bool hasDate: false
    property date date: new Date()
    property string targetText: ""
    property var preview: null
    property string error: ""
    property int previewGeneration: 0

    readonly property var amount: Fmt.parseCents(amountText)
    readonly property var target: Fmt.parseCents(targetText)
    readonly property string dateString: hasDate ? Fmt.iso(date) : ""
    readonly property bool canCalibrate: target !== null && preview !== null
    onDateStringChanged: if (visible) refreshPreview()

    function load() {
        preview = null; error = ""
        amountText = ""; hasDate = false; date = new Date(); targetText = ""
        // read the request directly: aboutToShow comes before the bindings above are updated
        const req = AppModel.openingBalanceRequest
        const a = req ? AppModel.accountsByID[req.accountId] : null
        if (!a) return
        amountText = a.opening_balance_cents !== null ? Fmt.plain(a.opening_balance_cents) : ""
        const d = Fmt.date(a.opening_balance_date)
        if (d) { hasDate = true; date = d }
        targetText = Fmt.plain(a.balance_cents)
        refreshPreview()
        Qt.callLater(() => amountField.forceActiveFocus())
    }

    function refreshPreview() {
        const req = AppModel.openingBalanceRequest
        if (!req) return
        const gen = ++previewGeneration
        AppModel.previewOpeningBalance(req.accountId, hasDate ? Fmt.iso(date) : "", p => {
            if (gen !== previewGeneration) return
            preview = p
            error = ""
        }, err => {
            if (gen !== previewGeneration) return
            preview = null
            error = AppModel.parseCoreError(err).message
        })
    }

    /// Fills in the Startsaldo for the entered balance; stores nothing.
    function calibrate() {
        if (target === null || preview === null) return
        amountText = Fmt.plain(target - preview.sum_cents)
        amountField.forceActiveFocus()
    }

    function save() {
        if (amount === null) return
        try {
            AppModel.setOpeningBalance(request.accountId, amount, dateString)
            close()
        } catch (e) {
            error = AppModel.parseCoreError(e.message).message
        }
    }

    function remove() {
        try {
            AppModel.setOpeningBalance(request.accountId, null, "")
            close()
        } catch (e) {
            error = AppModel.parseCoreError(e.message).message
        }
    }

    ColumnLayout {
        width: sheet.availableWidth
        spacing: 0

        ColumnLayout {
            id: head
            Layout.fillWidth: true
            Layout.leftMargin: 20; Layout.rightMargin: 20; Layout.topMargin: 20
            spacing: 4
            Text { text: qsTr("Startsaldo"); font.pixelSize: Theme.body; font.weight: Font.Bold; color: Theme.text; Accessible.role: Accessible.Heading }
            Text {
                visible: sheet.account !== null
                text: sheet.account ? sheet.account.name : ""
                Layout.fillWidth: true; elide: Text.ElideRight
                font.pixelSize: Theme.callout; color: Theme.secondary
            }
        }

        ScrollView {

            ScrollBar.vertical: ScrollViewBar {}
            id: scroll
            Layout.fillWidth: true
            readonly property real chrome: head.implicitHeight + 20 + footer.implicitHeight + 33
            Layout.preferredHeight: Math.min(Math.max(form.implicitHeight + 40, 520 - chrome), sheet.maxBodyHeight - chrome)
            contentWidth: availableWidth
            clip: true
            ColumnLayout {
                id: form
                x: 20; y: 20; width: scroll.availableWidth - 40
                spacing: 18

                FormSection {
                    title: qsTr("Startsaldo")
                    footers: [qsTr("Der Kontostand zu Beginn des Stichtags, z. B. aus dem ältesten vorhandenen Kontoauszug. Umsätze ab dem Stichtag werden addiert, ältere nicht. Ohne Stichtag zählen alle Umsätze.")]
                    FormRow {
                        label: qsTr("Startsaldo")
                        TextField { id: amountField; Layout.preferredWidth: 220; placeholderText: qsTr("z. B. %1").arg(Fmt.plain(125000))
                                    text: sheet.amountText; onTextEdited: sheet.amountText = text; Sensitive { amounts: true; when: parent.text !== "" } }
                    }
                    FormRow {
                        label: qsTr("Ab Stichtag")
                        Switch { checked: sheet.hasDate; onToggled: sheet.hasDate = checked }
                    }
                    FormRow {
                        visible: sheet.hasDate
                        label: qsTr("Stichtag")
                        DateField { date: sheet.date; maximum: new Date(); onPicked: d => sheet.date = d }
                    }
                }
                FormSection {
                    title: qsTr("Kalibrieren")
                    footers: [qsTr("Trägt den Startsaldo ein, mit dem der Kontostand diesem Betrag entspricht, z. B. dem Saldo auf dem letzten Kontoauszug. Gespeichert wird erst mit „Speichern“.")]
                    FormRow {
                        label: qsTr("Kontostand")
                        TextField {
                            id: targetField
                            Layout.preferredWidth: 220; placeholderText: qsTr("z. B. %1").arg(Fmt.plain(206483))
                            text: sheet.targetText; onTextEdited: sheet.targetText = text
                            // Return in the Kontostand field calibrates instead of saving
                            onAccepted: sheet.calibrate()
                            Sensitive { amounts: true; when: parent.text !== "" }
                        }
                    }
                    RowLayout {
                        Button { text: qsTr("Kalibrieren"); icon.name: SF.icon("scope"); enabled: sheet.canCalibrate; onClicked: sheet.calibrate() }
                    }
                }
                FormSection {
                    title: qsTr("Vorschau")
                    FormRow { visible: sheet.account !== null; label: qsTr("Kontostand bisher"); amounts: true
                              value: sheet.account ? Fmt.money(sheet.account.balance_cents, sheet.currency) : "" }
                    RowLayout {
                        visible: sheet.preview !== null && sheet.amount !== null
                        Layout.fillWidth: true
                        Text { text: qsTr("Kontostand danach"); font.pixelSize: Theme.body; font.weight: Font.Bold; color: Theme.text; Layout.fillWidth: true }
                        Text { text: sheet.preview && sheet.amount !== null ? Fmt.money(sheet.amount + sheet.preview.sum_cents, sheet.currency) : ""
                               font.pixelSize: Theme.body; font.weight: Font.Bold; font.features: { "tnum": 1 }; color: Theme.text; Sensitive { amounts: true } }
                    }
                    FormRow { visible: sheet.preview !== null && sheet.amount !== null; label: qsTr("Umsätze ab Stichtag"); amounts: true
                              value: sheet.preview ? sheet.preview.counted + " · " + Fmt.money(sheet.preview.sum_cents, sheet.currency) : "" }
                    FormRow { visible: sheet.preview !== null && sheet.amount !== null && sheet.preview.older > 0; label: qsTr("Ältere Umsätze (im Startsaldo enthalten)")
                              value: sheet.preview ? String(sheet.preview.older) : "" }
                    Text {
                        visible: sheet.preview === null || sheet.amount === null
                        text: qsTr("Vorschau erscheint, sobald ein Startsaldo eingetragen ist.")
                        Layout.fillWidth: true; wrapMode: Text.WordWrap
                        font.pixelSize: Theme.body; color: Theme.secondary
                    }
                }
            }
        }

        Rectangle { Layout.fillWidth: true; height: 1; color: Theme.border }
        RowLayout {
            id: footer
            Layout.fillWidth: true
            Layout.margins: 16
            spacing: 8
            // destructive action on the leading edge, away from Cancel / Save
            Button {
                visible: sheet.account !== null && sheet.account.opening_balance_cents !== null
                text: qsTr("Entfernen")
                palette.buttonText: Theme.negative
                onClicked: sheet.remove()
            }
            RowLayout {
                visible: sheet.error !== ""
                Layout.fillWidth: true
                spacing: 6
                SFIcon { symbol: "exclamationmark.triangle.fill"; size: 13; color: Theme.warning; Layout.alignment: Qt.AlignTop }
                Text { text: sheet.error; font.pixelSize: Theme.callout; color: Theme.warning; wrapMode: Text.WordWrap; Layout.fillWidth: true }
            }
            Item { visible: sheet.error === ""; Layout.fillWidth: true }
            Button { text: qsTr("Abbrechen"); onClicked: sheet.close() }
            Button {
                text: qsTr("Speichern")
                highlighted: true
                enabled: sheet.amount !== null
                onClicked: sheet.save()
                Shortcut { sequences: ["Return", "Enter"]; enabled: sheet.visible && sheet.amount !== null && !targetField.activeFocus; onActivated: sheet.save() }
            }
        }
    }
}
