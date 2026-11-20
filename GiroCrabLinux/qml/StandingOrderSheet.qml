import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// StandingOrderSheet.swift: Dauerauftrag anlegen, ändern oder löschen (GitHub #496) – a
// form, then a read-only review, then the bank's TAN request (TwoFactorSheet). A deletion
// starts at the review. State and schedule logic: Payments.qml (StandingDraft); open it
// with Payments.openStandingOrder(mode, …). Also holds RootView's alert "Status des
// Dauerauftrags unklar".
Sheet {
    id: sheet
    readonly property var draft: Payments.standingDraft
    readonly property var limits: Payments.standingLimits
    readonly property bool isDelete: draft.action === "Delete"
    property bool reviewing: false
    property bool demoReviewShown: false

    readonly property var account: AppModel.allAccounts.find(a => a.id === draft.account_id) || null
    readonly property bool pinStored: {
        if (!account || account.connection_id === null) return false
        if (AppModel.isDemo) return true
        const st = AppModel.connectionStatus(account.connection_id)
        return st ? st.has_pin : false
    }
    readonly property bool ibanValid: AppModel.validateIban(draft.iban)
    readonly property var cents: Payments.standingCents(draft)
    readonly property var intervals: Payments.allowedIntervals(draft, limits)
    readonly property var days: Payments.allowedDays(draft, limits)
    readonly property var firstRange: Payments.firstDateRange(limits)
    /// Choices of the execution day: [{ day, title }].
    readonly property var dayChoices: {
        if (draft.unit === "W") return (days.length ? days : [1, 2, 3, 4, 5, 6, 7]).map(d => ({ day: d, title: Payments.weekdayName(d) }))
        const list = (days.length ? days.filter(d => d < 31) : [...Array(30).keys()].map(i => i + 1)).map(d => ({ day: d, title: d + "." }))
        if (!days.length || days.indexOf(31) >= 0) list.push({ day: 31, title: qsTr("Letzter Tag des Monats") })
        return list
    }
    readonly property string heading: draft.action === "Create" ? (reviewing ? qsTr("Dauerauftrag prüfen") : qsTr("Neuer Dauerauftrag"))
                                    : draft.action === "Change" ? (reviewing ? qsTr("Änderung prüfen") : qsTr("Dauerauftrag ändern"))
                                    : qsTr("Dauerauftrag löschen")

    visible: Payments.showStandingSheet
    sheetWidth: 520
    padding: 0
    closePolicy: AppModel.transferBusy ? Popup.NoAutoClose : Popup.CloseOnEscape
    onAboutToShow: {
        reviewing = Payments.standingDraft.action === "Delete"
            || (AppModel.demoSheet === "standing-order-confirm" && !demoReviewShown) // screenshots
        demoReviewShown = true
    }
    onClosed: if (Payments.showStandingSheet) Payments.cancelStandingSheet()

    function intervalText(n) {
        if (draft.unit === "W") return n === 1 ? qsTr("1 Woche") : qsTr("%1 Wochen").arg(n)
        return n === 1 ? qsTr("1 Monat") : qsTr("%1 Monate").arg(n)
    }
    function start() { if (!AppModel.transferBusy) Payments.startStandingOrder() }

    ColumnLayout {
        width: sheet.availableWidth
        spacing: 0

        Text {
            id: head
            Layout.fillWidth: true
            Layout.leftMargin: 20; Layout.rightMargin: 20; Layout.topMargin: 14; Layout.bottomMargin: 14
            text: sheet.heading
            font.pixelSize: Theme.title2; font.weight: Font.DemiBold; color: Theme.text
            Accessible.role: Accessible.Heading
        }
        Rectangle { Layout.fillWidth: true; height: 1; color: Theme.border }

        ScrollView {
            id: scroll
            ScrollBar.vertical: ScrollViewBar {}
            Layout.fillWidth: true
            readonly property real chrome: head.implicitHeight + 28 + messages.implicitHeight + buttonRow.implicitHeight + 32
            readonly property real body: (sheet.reviewing ? review.implicitHeight : form.implicitHeight) + 40
            Layout.preferredHeight: Math.min(sheet.reviewing ? body : Math.max(body, 420), sheet.maxBodyHeight - chrome)
            contentWidth: availableWidth
            // several children (form / review): the ScrollView cannot guess the height itself
            contentHeight: body
            clip: true

            // MARK: editor
            ColumnLayout {
                id: form
                visible: !sheet.reviewing
                x: 20; y: 20; width: scroll.availableWidth - 40
                spacing: 18

                FormSection {
                    FormRow {
                        label: qsTr("Von Konto")
                        enabled: sheet.draft.action !== "Change"
                        AppComboBox {
                            Layout.preferredWidth: 280
                            model: Payments.standingOrderAccounts.map(a => a.name + " · " + Fmt.iban(a.iban))
                            currentIndex: Payments.standingOrderAccounts.findIndex(a => a.id === sheet.draft.account_id)
                            displayText: currentIndex < 0 && sheet.account ? sheet.account.name + " · " + Fmt.iban(sheet.account.iban) : currentText
                            onActivated: i => Payments.setStanding({ account_id: Payments.standingOrderAccounts[i].id })
                            Sensitive {}
                        }
                    }
                }
                FormSection {
                    FormRow {
                        label: qsTr("Empfänger")
                        TextField {
                            Layout.preferredWidth: 280; horizontalAlignment: TextInput.AlignRight
                            placeholderText: qsTr("Name")
                            text: sheet.draft.name
                            onTextEdited: Payments.setStanding({ name: text })
                            Sensitive {}
                        }
                    }
                    FormRow {
                        label: qsTr("IBAN")
                        TextField {
                            Layout.preferredWidth: 280; horizontalAlignment: TextInput.AlignRight
                            placeholderText: qsTr("DE00 …")
                            font.family: "monospace"
                            inputMethodHints: Qt.ImhNoPredictiveText | Qt.ImhPreferUppercase
                            text: sheet.draft.iban
                            onTextEdited: Payments.setStanding({ iban: AppModel.groupedIBAN(text) })
                            Sensitive {}
                        }
                        SFIcon {
                            opacity: sheet.draft.iban !== "" && !sheet.ibanValid ? 1 : 0
                            symbol: "xmark.circle.fill"; size: 16; color: Theme.negative
                            HoverHandler { id: ibanHover }
                            ToolTip.visible: ibanHover.hovered && opacity > 0
                            ToolTip.text: qsTr("IBAN ungültig (Prüfsumme/Länge)")
                            Accessible.name: qsTr("IBAN ungültig")
                        }
                    }
                    FormRow {
                        label: qsTr("Betrag")
                        TextField {
                            Layout.preferredWidth: 160; horizontalAlignment: TextInput.AlignRight
                            placeholderText: "0,00"
                            font.features: { "tnum": 1 }
                            text: sheet.draft.amountText
                            onTextEdited: Payments.setStanding({ amountText: text })
                            Sensitive { amounts: true }
                        }
                    }
                    FormRow {
                        label: qsTr("Verwendungszweck")
                        TextField {
                            Layout.preferredWidth: 280; horizontalAlignment: TextInput.AlignRight
                            placeholderText: qsTr("optional")
                            maximumLength: 140
                            text: sheet.draft.purpose
                            onTextEdited: Payments.setStanding({ purpose: text })
                            Sensitive {}
                        }
                    }
                }
                FormSection {
                    FormRow {
                        label: qsTr("Rhythmus")
                        AppComboBox {
                            Layout.preferredWidth: 200
                            model: [qsTr("Monate"), qsTr("Wochen")]
                            currentIndex: sheet.draft.unit === "W" ? 1 : 0
                            onActivated: i => Payments.setStanding({ unit: i === 1 ? "W" : "M" }, "unit")
                        }
                    }
                    FormRow {
                        label: qsTr("Alle")
                        // Stepper(in: 1...99) when the bank names no intervals, else a Picker of them
                        // one stepper per unit: a SpinBox keeps its text when only the unit changes
                        Repeater {
                            model: ["M", "W"]
                            SpinBox {
                                required property string modelData
                                visible: sheet.intervals.length === 0 && sheet.draft.unit === modelData
                                Layout.preferredWidth: 200
                                from: 1; to: 99
                                value: sheet.draft.interval
                                textFromValue: v => modelData === "W" ? (v === 1 ? qsTr("1 Woche") : qsTr("%1 Wochen").arg(v))
                                                                      : (v === 1 ? qsTr("1 Monat") : qsTr("%1 Monate").arg(v))
                                valueFromText: t => parseInt(t) || 1
                                onValueModified: Payments.setStanding({ interval: value })
                            }
                        }
                        AppComboBox {
                            visible: sheet.intervals.length > 0
                            Layout.preferredWidth: 200
                            model: sheet.intervals.map(n => sheet.intervalText(n))
                            currentIndex: sheet.intervals.indexOf(sheet.draft.interval)
                            onActivated: i => Payments.setStanding({ interval: sheet.intervals[i] })
                        }
                    }
                    FormRow {
                        label: sheet.draft.unit === "W" ? qsTr("Wochentag") : qsTr("Ausführungstag")
                        AppComboBox {
                            Layout.preferredWidth: 200
                            model: sheet.dayChoices.map(c => c.title)
                            currentIndex: sheet.dayChoices.findIndex(c => c.day === sheet.draft.execDay)
                            // The first execution and the execution day describe the same schedule: the one changed last wins.
                            onActivated: i => Payments.setStanding({ execDay: sheet.dayChoices[i].day }, "date")
                        }
                    }
                    FormRow {
                        label: qsTr("Erste Ausführung")
                        DateField {
                            date: sheet.draft.firstDate
                            minimum: sheet.firstRange.from
                            maximum: sheet.firstRange.to
                            onPicked: d => Payments.setStanding({ firstDate: d }, "day")
                        }
                    }
                    FormRow {
                        label: qsTr("Mit Enddatum")
                        Switch { checked: sheet.draft.hasEnd; onToggled: Payments.setStanding({ hasEnd: checked }) }
                    }
                    FormRow {
                        visible: sheet.draft.hasEnd
                        label: qsTr("Letzte Ausführung")
                        DateField {
                            date: sheet.draft.lastDate
                            minimum: sheet.draft.firstDate
                            onPicked: d => Payments.setStanding({ lastDate: d })
                        }
                    }
                }
            }

            // MARK: review
            ColumnLayout {
                id: review
                visible: sheet.reviewing
                x: 20; y: 20; width: scroll.availableWidth - 40
                spacing: 12
                Note {
                    visible: sheet.isDelete; symbol: "exclamationmark.triangle"
                    text: qsTr("Der Dauerauftrag wird bei der Bank gelöscht. Danach werden keine weiteren Zahlungen mehr ausgeführt.")
                }
                Card {
                    Layout.fillWidth: true
                    ColumnLayout {
                        width: parent.width
                        spacing: 10
                        Text { text: Fmt.money(sheet.cents === null ? 0 : sheet.cents); font.pixelSize: Theme.title; font.weight: Font.DemiBold
                               font.features: { "tnum": 1 }; color: Theme.text; Sensitive { amounts: true } }
                        Text { text: Payments.rhythmText(sheet.draft) + " · " + Payments.execDayText(sheet.draft)
                               font.pixelSize: Theme.callout; color: Theme.secondary }
                        Line { label: qsTr("Von Konto"); value: sheet.account ? sheet.account.name + " · " + Fmt.iban(sheet.account.iban) : "" }
                        Line { label: qsTr("Empfänger"); value: sheet.draft.name }
                        Line { label: qsTr("IBAN"); value: sheet.draft.iban; mono: true }
                        Line { visible: sheet.draft.purpose !== ""; label: qsTr("Verwendungszweck"); value: sheet.draft.purpose }
                        Line { label: qsTr("Erste Ausführung"); value: Fmt.medium(Fmt.iso(sheet.draft.firstDate)); personal: false }
                        Line { label: qsTr("Letzte Ausführung"); personal: false
                               value: sheet.draft.hasEnd ? Fmt.medium(Fmt.iso(sheet.draft.lastDate)) : qsTr("bis auf Widerruf") }
                    }
                }
                TextField {
                    visible: !sheet.pinStored
                    Layout.preferredWidth: 360 * Theme.textScale
                    echoMode: TextInput.Password
                    placeholderText: qsTr("Banking-PIN") + " (" + qsTr("wird nicht gespeichert") + ")"
                    text: sheet.draft.pin
                    onTextEdited: Payments.setStanding({ pin: text })
                }
                Note {
                    visible: !sheet.isDelete; symbol: "exclamationmark.shield"
                    text: qsTr("Bitte Empfänger und IBAN genau prüfen. Die Bank bestätigt den Auftrag mit einer TAN.")
                }
            }
        }

        RowLayout {
            id: messages
            visible: AppModel.transferError !== ""
            Layout.fillWidth: true
            Layout.leftMargin: 20; Layout.rightMargin: 20; Layout.topMargin: 8; Layout.bottomMargin: 8
            spacing: 6
            SFIcon { symbol: "exclamationmark.triangle.fill"; size: 14; color: Theme.negative; Layout.alignment: Qt.AlignTop }
            TextEdit { Layout.fillWidth: true; readOnly: true; selectByMouse: true; wrapMode: Text.WordWrap
                       text: AppModel.transferError; color: Theme.negative; font.pixelSize: Theme.callout }
        }
        Rectangle { Layout.fillWidth: true; height: 1; color: Theme.border }

        // MARK: buttons
        RowLayout {
            id: buttonRow
            Layout.fillWidth: true
            Layout.margins: 14
            spacing: 8
            Button { text: qsTr("Abbrechen"); onClicked: Payments.cancelStandingSheet() }
            Item { Layout.fillWidth: true }
            Button { visible: sheet.reviewing && !sheet.isDelete; text: qsTr("Zurück"); onClicked: { sheet.reviewing = false; AppModel.transferError = "" } }
            // HIG: a destructive action is never the default button (Return does nothing here)
            Button {
                visible: sheet.isDelete
                text: qsTr("Dauerauftrag löschen")
                palette.buttonText: Theme.negative
                enabled: !AppModel.transferBusy
                onClicked: Payments.startStandingOrder()
            }
            Button {
                visible: sheet.reviewing && !sheet.isDelete
                text: qsTr("Jetzt mit TAN freigeben"); highlighted: true
                enabled: !AppModel.transferBusy
                onClicked: sheet.start()
            }
            Button {
                id: nextButton
                visible: !sheet.reviewing
                text: qsTr("Weiter"); highlighted: true
                enabled: Payments.standingIsValid(sheet.draft)
                onClicked: { AppModel.transferError = ""; sheet.reviewing = true }
            }
        }
    }

    // .keyboardShortcut(.defaultAction) – not for a deletion
    Shortcut {
        sequences: ["Return", "Enter"]
        enabled: sheet.visible && !sheet.isDelete
        onActivated: { if (!sheet.reviewing) { if (nextButton.enabled) nextButton.clicked() } else sheet.start() }
    }

    // RootView: .alert("Status des Dauerauftrags unklar")
    Sheet {
        id: unknownDialog
        sheetWidth: 440
        visible: Payments.standingUnknown !== null
        onClosed: Payments.standingUnknown = null
        ColumnLayout {
            width: unknownDialog.availableWidth
            spacing: 12
            Text { text: qsTr("Status des Dauerauftrags unklar"); font.pixelSize: Theme.body + 2; font.weight: Font.Bold; color: Theme.text
                   wrapMode: Text.WordWrap; Layout.fillWidth: true }
            TextEdit {
                Layout.fillWidth: true; readOnly: true; selectByMouse: true; wrapMode: Text.WordWrap; color: Theme.text; font.pixelSize: Theme.body
                text: qsTr("Der Auftrag hat die Bank erreicht, aber die Antwort ist nicht eindeutig. Bitte zuerst im Online-Banking prüfen, ob der Dauerauftrag angelegt, geändert oder gelöscht wurde, bevor Sie es erneut versuchen.\n\nBank: %1")
                      .arg(Payments.standingUnknown || "")
            }
            RowLayout {
                Item { Layout.fillWidth: true }
                Button { text: qsTr("Verstanden"); highlighted: true; onClicked: unknownDialog.close() }
            }
        }
    }

    component Note: RowLayout {
        property alias text: noteText.text
        property alias symbol: noteIcon.symbol
        Layout.fillWidth: true
        spacing: 6
        SFIcon { id: noteIcon; size: 14; color: Theme.secondary; Layout.alignment: Qt.AlignTop }
        Text { id: noteText; Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: Theme.callout; color: Theme.secondary }
    }
    /// line(_:_:): label above, the value never truncated (recipient and IBAN are what the person checks).
    component Line: ColumnLayout {
        property string label
        property string value
        property bool mono: false
        property bool personal: true
        Layout.fillWidth: true
        spacing: 2
        Text { text: parent.label; font.pixelSize: Theme.caption; color: Theme.secondary }
        TextEdit {
            Layout.fillWidth: true; readOnly: true; selectByMouse: true; wrapMode: Text.Wrap
            text: parent.value; font.pixelSize: Theme.callout; color: Theme.text
            font.family: parent.mono ? "monospace" : Qt.application.font.family
            Sensitive { when: parent.parent.personal }
        }
    }
}
