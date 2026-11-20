import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// AccountSelectionView.swift › AccountSelectionList (#346): which accounts of one login
// are fetched. Accounts that are already reachable through another login are held back
// until the user decides; skipped accounts stay listed so the choice can be changed later.
ColumnLayout {
    id: list
    property var connection: null
    property bool showsSaveButton: true
    property string saveTitle: qsTr("Auswahl speichern")
    /// After a successful save (the sheet closes itself).
    signal saved()

    property var choices: []
    /// account_ref → true ("Abrufen" on)
    property var enabledRefs: ({})
    /// What was on when the list was loaded (AccountSelectionDraft.original).
    property var originalRefs: ({})
    property bool loaded: false
    property string error: ""

    Layout.fillWidth: true
    spacing: 10

    function load() {
        loaded = false
        error = ""
        if (!connection) return
        const id = connection.id
        AppModel.coreAsync("connection_account_choices", { connection_id: id }, rows => {
            if (!list.connection || list.connection.id !== id) return
            list.choices = rows
            // held-back accounts start as "Abrufen" off: the user has to opt in
            const on = {}
            for (const c of rows) if (c.enabled) on[c.account_ref] = true
            list.enabledRefs = on
            list.originalRefs = on
            list.loaded = true
        }, () => { list.loaded = true })
    }
    onConnectionChanged: load()
    Component.onCompleted: load()

    function setEnabled(ref, on) {
        const x = Object.assign({}, enabledRefs)
        if (on) x[ref] = true
        else delete x[ref]
        enabledRefs = x
    }
    function detail(c) { return [c.number, c.owner].filter(s => s !== "").join(" · ") }
    /// AccountSelectionDraft.hasChanges
    readonly property bool hasChanges: {
        if (!loaded) return false
        const a = Object.keys(enabledRefs), b = Object.keys(originalRefs)
        return a.length !== b.length || a.some(r => !originalRefs[r])
    }
    /// AccountSelectionList.switchesAnythingOff: an account that was on is switched off by this
    /// save. It gets archived, and the sidebar then hides archived accounts (#720).
    function switchesAnythingOff() { return choices.some(c => c.enabled && !enabledRefs[c.account_ref]) }

    /// AccountSelectionDraft.save: `done(true, "")` or `done(false, message)`. The callbacks
    /// only use what is captured here, so a sheet may close before the core answers.
    function commit(done) {
        const id = connection.id
        const refs = Object.keys(enabledRefs)
        const switchedOff = switchesAnythingOff()
        AppModel.coreAsync("set_connection_account_choices", { connection_id: id, enabled_refs: refs }, () => {
            if (switchedOff) AppModel.showArchivedAccounts = false
            AppModel.reloadAll()
            done(true, "")
        }, err => done(false, AppModel.parseCoreError(err).message))
    }
    function save() {
        commit((ok, message) => {
            if (!ok) { list.error = message; return }
            list.error = ""
            list.originalRefs = list.enabledRefs
            list.saved()
        })
    }
    /// AccountSelectionDraft.saveIfChanged: for a sheet's "Speichern"; nothing to do (true) unless changed.
    function saveIfChanged(done) {
        if (!hasChanges) { done(true, ""); return }
        commit(done)
    }

    Spinner { visible: !list.loaded; size: 20; Layout.alignment: Qt.AlignHCenter }
    Text {
        visible: list.loaded && list.choices.length === 0
        text: qsTr("Noch keine Konten abgerufen.")
        font.pixelSize: Theme.body; color: Theme.secondary
    }
    Repeater {
        model: list.loaded ? list.choices : []
        delegate: RowLayout {
            id: row
            required property var modelData
            Layout.fillWidth: true
            spacing: 10
            ColumnLayout {
                Layout.fillWidth: true
                spacing: 2
                Text { text: row.modelData.name; font.pixelSize: Theme.body; color: Theme.text; elide: Text.ElideRight; Layout.fillWidth: true }
                Text { visible: text !== ""; text: list.detail(row.modelData); font.pixelSize: Theme.caption; color: Theme.secondary
                       elide: Text.ElideRight; Layout.fillWidth: true }
                RowLayout {
                    visible: row.modelData.pending
                    Layout.fillWidth: true
                    spacing: 4
                    SFIcon { symbol: "exclamationmark.triangle"; size: 11; color: Theme.warning; Layout.alignment: Qt.AlignTop }
                    Text { text: qsTr("Bereits über „%1“ eingebunden").arg(row.modelData.existing_connection)
                           font.pixelSize: Theme.caption; color: Theme.warning; wrapMode: Text.WordWrap; Layout.fillWidth: true }
                }
            }
            Switch {
                checked: !!list.enabledRefs[row.modelData.account_ref]
                onToggled: list.setEnabled(row.modelData.account_ref, checked)
            }
        }
    }
    Text {
        visible: list.loaded && list.choices.length > 0
        Layout.fillWidth: true; wrapMode: Text.WordWrap
        text: qsTr("Abrufen: Konto, Umsätze und Dokumente werden über diesen Zugang importiert. Überspringen: nichts davon; das Konto wird archiviert und ausgeblendet und behält seine gespeicherten Daten. Das Archiv-Symbol in der Seitenleiste zeigt archivierte Konten wieder an.")
        font.pixelSize: Theme.caption; color: Theme.secondary
    }
    Text {
        visible: list.error !== ""
        Layout.fillWidth: true; wrapMode: Text.WordWrap
        text: list.error
        font.pixelSize: Theme.caption; color: Theme.negative
    }
    Button {
        visible: list.loaded && list.choices.length > 0 && list.showsSaveButton
        text: list.saveTitle
        highlighted: true
        onClicked: list.save()
    }
}
