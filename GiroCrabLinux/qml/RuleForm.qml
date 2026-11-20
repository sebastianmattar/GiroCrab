import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// RuleEditorView.swift › RuleForm: one rule (.formStyle(.grouped)) with live preview.
// Every change emits `edited(rule)` with a new object (the editor owns the draft).
Item {
    id: form
    property var rule: null
    signal edited(var rule)
    signal saveRequested(var rule)
    signal deleteRequested(var rule)

    function set(key, value) {
        const r = Object.assign({}, rule)
        r[key] = value
        edited(r)
    }
    function setMany(values) { edited(Object.assign({}, rule, values)) }

    readonly property bool isExpression: rule !== null && rule.match_kind === "Expression"
    readonly property int direction: rule === null ? 0 : (rule.max_amount_cents === 0 ? 1 : (rule.min_amount_cents === 1 ? 2 : 0))
    readonly property string placeholder: rule === null ? "" : rule.match_kind === "Keyword" ? "REWE | EDEKA | ALDI"
        : rule.match_kind === "Regex" ? "^DB\\s+(Fernverkehr|Vertrieb)" : "name:REWE OR purpose:\"Bahn AG\""

    // model.preview(rule): debounced, so typing a pattern stays fluid on big vaults
    property var preview: ({ matches: [], error: "" })
    onRuleChanged: previewTimer.restart()
    Timer { id: previewTimer; interval: 120; onTriggered: form.preview = form.rule ? AppModel.preview(form.rule) : { matches: [], error: "" } }

    readonly property var fields: [
        { id: "Any", title: qsTr("Name, Verwendungszweck und Buchungstext") }, { id: "Counterparty", title: qsTr("Name") },
        { id: "Purpose", title: qsTr("Verwendungszweck") }, { id: "BookingText", title: qsTr("Buchungstext") },
        { id: "CounterpartyIban", title: qsTr("IBAN") }
    ]
    // a rule that only adds tags (#580, made in the Mac app) keeps showing that choice; setting
    // tags on rules is not in the Linux UI yet, so new rules always need a category
    readonly property var categoryOptions: (rule && rule.category_id === 0 ? [{ id: 0, title: qsTr("Keine (nur Tags)") }] : [])
        .concat(AppModel.categories.map(c => ({ id: c.id, title: "    ".repeat(c.depth) + c.name })))

    ColumnLayout {
        anchors.fill: parent
        spacing: 0
        ScrollView {
            ScrollBar.vertical: ScrollViewBar {}
            id: scroll
            Layout.fillWidth: true
            Layout.fillHeight: true
            contentWidth: availableWidth
            clip: true
            ColumnLayout {
                width: scroll.availableWidth
                spacing: 6
                Item { implicitHeight: 8 }

                FormSection {
                    title: qsTr("Regel")
                    FormRow {
                        label: qsTr("Name")
                        TextField { Layout.fillWidth: true; text: form.rule ? form.rule.name : ""; onTextEdited: form.set("name", text) }
                    }
                    FormRow {
                        label: qsTr("Kategorie")
                        AppComboBox {
                            Layout.fillWidth: true
                            model: form.categoryOptions
                            textRole: "title"
                            currentIndex: form.rule ? form.categoryOptions.findIndex(o => o.id === form.rule.category_id) : -1
                            onActivated: i => form.set("category_id", form.categoryOptions[i].id)
                        }
                    }
                    FormRow {
                        label: qsTr("Aktiv")
                        Item { Layout.fillWidth: true }
                        Switch { checked: form.rule ? form.rule.enabled : false; onToggled: form.set("enabled", checked) }
                    }
                }

                FormSection {
                    title: qsTr("Treffer")
                    // A search expression names its fields itself and ignores case.
                    FormRow {
                        visible: !form.isExpression
                        label: qsTr("Suchen in")
                        AppComboBox {
                            Layout.fillWidth: true
                            model: form.fields
                            textRole: "title"
                            currentIndex: form.rule ? form.fields.findIndex(o => o.id === form.rule.field) : 0
                            onActivated: i => form.set("field", form.fields[i].id)
                        }
                    }
                    FormRow {
                        label: qsTr("Art")
                        Item { Layout.fillWidth: true }
                        Segmented {
                            options: [{ id: "Keyword", title: qsTr("Stichwörter") }, { id: "Regex", title: qsTr("Regulärer Ausdruck") },
                                      { id: "Expression", title: qsTr("Suchausdruck") }]
                            current: form.rule ? form.rule.match_kind : ""
                            onActivated: id => form.set("match_kind", id)
                        }
                    }
                    FormRow {
                        visible: !form.isExpression
                        label: qsTr("Groß-/Kleinschreibung beachten")
                        Item { Layout.fillWidth: true }
                        Switch { checked: form.rule ? form.rule.case_sensitive : false; onToggled: form.set("case_sensitive", checked) }
                    }
                }

                // The pattern gets its own full-width, multi-line field (#179);
                // the example is the footer.
                FormSection {
                    title: qsTr("Muster")
                    // a plain TextEdit (.textFieldStyle(.plain), 3…10 lines)
                    TextEdit {
                        id: patternField
                        Layout.fillWidth: true
                        Layout.minimumHeight: 3 * 18
                        wrapMode: TextEdit.Wrap
                        font.family: "monospace"
                        font.pixelSize: Theme.body
                        color: Theme.text
                        selectByMouse: true
                        selectionColor: Theme.selection
                        selectedTextColor: Theme.text
                        text: form.rule ? form.rule.pattern : ""
                        onTextChanged: if (form.rule && text !== form.rule.pattern) form.set("pattern", text)
                        Text {
                            visible: patternField.text === ""
                            text: form.placeholder
                            font: patternField.font
                            color: Theme.tertiary
                        }
                    }
                }
                ColumnLayout {
                    Layout.fillWidth: true
                    Layout.leftMargin: 32; Layout.rightMargin: 32
                    spacing: 4
                    TextEdit {
                        Layout.fillWidth: true; readOnly: true; selectByMouse: true; wrapMode: TextEdit.Wrap
                        textFormat: TextEdit.RichText
                        text: qsTr("Beispiel: %1").arg("<span style=\"font-family: monospace\">" + form.placeholder.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/"/g, "&quot;") + "</span>")
                        font.pixelSize: Theme.caption; color: Theme.secondary
                    }
                    Text {
                        visible: form.rule !== null && form.rule.match_kind !== "Regex"
                        Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: Theme.caption; color: Theme.secondary
                        text: form.isExpression
                              ? qsTr("Mit AND, OR, NOT, Klammern, „Wortgruppen“, * und ?, /Regex/, Feldern wie name: oder purpose: und Beträgen wie amount<0.")
                              : qsTr("Stichwörter mit | trennen oder eines pro Zeile.")
                    }
                }

                FormSection {
                    title: qsTr("Betrag")
                    FormRow {
                        label: qsTr("Richtung")
                        Item { Layout.fillWidth: true }
                        Segmented {
                            options: [{ id: "0", title: qsTr("Alle") }, { id: "1", title: qsTr("Nur Ausgaben") }, { id: "2", title: qsTr("Nur Einnahmen") }]
                            current: String(form.direction)
                            onActivated: id => form.setMany(id === "1" ? { min_amount_cents: null, max_amount_cents: 0 }
                                                          : id === "2" ? { min_amount_cents: 1, max_amount_cents: null }
                                                          : { min_amount_cents: null, max_amount_cents: null })
                        }
                    }
                    // RuleAccountPicker (#562): several accounts, empty = all
                    RuleAccountPicker {
                        Layout.fillWidth: true
                        accountIds: form.rule && form.rule.account_ids ? form.rule.account_ids : []
                        onPicked: ids => form.set("account_ids", ids)
                    }
                }

                FormSection {
                    title: qsTr("Live-Vorschau")
                    RowLayout {
                        visible: form.preview.error !== ""
                        Layout.fillWidth: true
                        SFIcon { symbol: "exclamationmark.triangle.fill"; size: 14; color: Theme.warning }
                        Text { text: form.preview.error; color: Theme.warning; font.pixelSize: Theme.body; wrapMode: Text.WordWrap; Layout.fillWidth: true }
                    }
                    Text {
                        visible: form.preview.error === ""
                        text: qsTr("%1 passende Buchungen").arg(form.preview.matches.length >= 200 ? "200+" : form.preview.matches.length)
                        font.pixelSize: Theme.body + 1; font.weight: Font.Bold; color: Theme.text
                    }
                    Repeater {
                        model: form.preview.error === "" ? form.preview.matches.slice(0, 40) : []
                        RowLayout {
                            id: pr
                            required property var modelData
                            Layout.fillWidth: true
                            spacing: 8
                            Text { text: Fmt.medium(pr.modelData.booking_date); color: Theme.secondary; font.pixelSize: Theme.callout + 1; font.features: { "tnum": 1 } }
                            Text { text: pr.modelData.counterparty; color: Theme.text; font.pixelSize: Theme.callout + 1; elide: Text.ElideRight; Layout.fillWidth: true }
                            SFIcon {
                                visible: pr.modelData.category_source === "Manual"; symbol: "hand.raised.fill"; size: 12
                                HoverHandler { id: mh }
                                ToolTip.visible: mh.hovered
                                ToolTip.text: qsTr("Manuell zugeordnet – die Regel ändert das nicht")
                            }
                            Text { text: Fmt.money(pr.modelData.amount_cents); font.pixelSize: Theme.callout + 1; font.features: { "tnum": 1 }
                                   color: pr.modelData.amount_cents > 0 ? Theme.inflow : Theme.outflow; Sensitive { amounts: true } }
                        }
                    }
                }
                Item { implicitHeight: 12 }
            }
        }
        Rectangle { Layout.fillWidth: true; height: 1; color: Theme.border }
        RowLayout {
            Layout.fillWidth: true
            Layout.margins: 12
            Button {
                visible: form.rule !== null && form.rule.id !== 0
                text: qsTr("Regel löschen …")
                palette.buttonText: Theme.negative
                onClicked: form.deleteRequested(form.rule)
            }
            Item { Layout.fillWidth: true }
            Button {
                text: qsTr("Speichern && anwenden")
                highlighted: true
                // not while the live preview is still pending (it may turn out invalid)
                enabled: form.rule !== null && form.rule.pattern.trim() !== "" && !previewTimer.running && form.preview.error === ""
                onClicked: form.saveRequested(form.rule)
            }
        }
    }

    // Form { Section(title) { … } } in the grouped macOS look
    component FormSection: ColumnLayout {
        id: fs
        property string title
        default property alias rows: box.data
        Layout.fillWidth: true
        Layout.leftMargin: 20; Layout.rightMargin: 20
        spacing: 6
        Text { text: fs.title; font.pixelSize: Theme.callout + 1; font.weight: Font.DemiBold; color: Theme.secondary; Layout.leftMargin: 12; Layout.topMargin: 6 }
        Rectangle {
            Layout.fillWidth: true
            implicitHeight: box.implicitHeight + 20
            radius: 8
            color: Theme.card
            border.color: Theme.border
            ColumnLayout {
                id: box
                anchors.left: parent.left; anchors.right: parent.right; anchors.top: parent.top
                anchors.margins: 10
                spacing: 8
            }
        }
    }
    component FormRow: RowLayout {
        id: frow
        property string label
        Layout.fillWidth: true
        spacing: 12
        Text { text: frow.label; font.pixelSize: Theme.body; color: Theme.text; Layout.preferredWidth: implicitWidth }
    }
}
