import QtQuick
import QtQuick.Controls
import QtQuick.Dialogs
import QtQuick.Layouts

// TagManagerView.swift › TagManagerView (#228, part 2b): Konten › Tags verwalten … – every tag with
// its booking count; rename, colour, order, merge, delete. A sheet of the main window with a fixed
// size (names never exceed 64 characters). Names, colours and order are a draft (TagManager.draft)
// stored when the sheet closes; creating, merging and deleting act at once. Tag names stay marked
// `Sensitive {}` like everywhere else.
Sheet {
    id: sheet
    visible: TagManager.shown
    // the draft is stored once the sheet is really gone, not when another sheet opens on top of it
    onClosed: { TagManager.applyDraft(); TagManager.shown = false }
    sheetWidth: 560
    padding: 0
    // Esc: leaves the search field (clearing it), then clears the selection, then closes the sheet
    closePolicy: Popup.NoAutoClose
    onOpened: tagList.forceActiveFocus()

    /// The selected tag ids as a set ({ id: true }).
    property var selection: ({})
    /// Shift-click extends from here (index in `shown`).
    property int anchorIndex: -1
    property string search: ""

    readonly property var shown: {
        const q = search.trim()
        if (q === "") return TagManager.ordered
        const lower = q.toLowerCase(), loose = TagsModel.lookAlikeKey(q)
        return TagManager.ordered.filter(t => t.name.toLowerCase().indexOf(lower) >= 0
                                          || (loose !== "" && TagsModel.lookAlikeKey(t.name).indexOf(loose) >= 0))
    }
    readonly property var selectedIDs: Object.keys(selection).map(Number)
    readonly property var selected: TagManager.current.filter(t => selection[t.id])
    /// Merging needs at least two selected tags; the sheet then asks which one stays.
    readonly property bool canMerge: selectedIDs.length > 1
    /// Drag to reorder; not while searching (the list is only a part then).
    readonly property bool movable: search === ""

    function select(ids) { const s = {}; for (const id of ids) s[id] = true; selection = s }
    /// A click: alone, Strg toggles, Umschalt extends (List(selection:) on macOS, ⌘ → Strg).
    function click(index, modifiers) {
        const id = shown[index].id
        if (modifiers & Qt.ControlModifier) {
            const s = Object.assign({}, selection)
            if (s[id]) delete s[id]; else s[id] = true
            selection = s
            anchorIndex = index
        } else if ((modifiers & Qt.ShiftModifier) && anchorIndex >= 0 && anchorIndex < shown.length) {
            const lo = Math.min(anchorIndex, index), hi = Math.max(anchorIndex, index)
            select(shown.slice(lo, hi + 1).map(t => t.id))
        } else {
            select([id])
            anchorIndex = index
        }
        tagList.forceActiveFocus()
    }
    /// ↑ / ↓ in the list.
    function step(delta) {
        if (shown.length === 0) return
        const cur = shown.findIndex(t => selection[t.id])
        const i = cur < 0 ? (delta > 0 ? 0 : shown.length - 1) : Math.max(0, Math.min(shown.length - 1, cur + delta))
        select([shown[i].id])
        anchorIndex = i
        tagList.positionViewAtIndex(i, ListView.Contain)
    }
    function handleEscape() {
        if (searchField.activeFocus) {
            search = ""
            tagList.forceActiveFocus()
        } else if (selectedIDs.length > 0) {
            selection = ({})
        } else {
            sheet.close()
        }
    }
    function moveTag(from, to) {
        const ids = TagManager.ordered.map(t => t.id)
        const [id] = ids.splice(from, 1)
        ids.splice(to, 0, id)
        TagManager.setOrder(ids)
    }
    function openEditor(mode, id) { nameEditor.mode = mode; nameEditor.tagID = id; nameEditor.open() }
    function rename() { if (selectedIDs.length === 1) openEditor("rename", selectedIDs[0]) }
    function confirmDeleteSelection() { if (selectedIDs.length > 0) deleteDialog.open() }

    // a tag gone (merged, deleted, synced away) leaves the selection
    Connections {
        target: TagsModel
        function onTagsChanged() {
            const live = {}
            for (const t of TagsModel.tags) live[t.id] = true
            if (sheet.selectedIDs.some(id => !live[id])) sheet.select(sheet.selectedIDs.filter(id => live[id]))
        }
    }

    // ⌘N (Strg+N) while no dialog lies on top: a modal sheet above blocks this shortcut, and the
    // sheet blocks the menu's „Neue Überweisung …“ (same keys) while it is up
    Shortcut { sequences: ["Ctrl+N"]; onActivated: sheet.openEditor("create", null) }
    Shortcut { sequences: ["Esc"]; onActivated: sheet.handleEscape() }

    ColumnLayout {
        width: sheet.availableWidth
        height: Math.min(520 * Theme.textScale, sheet.maxBodyHeight)
        spacing: 0

        // titleBar: „Tags verwalten“ and the search field
        RowLayout {
            Layout.fillWidth: true
            Layout.leftMargin: 16; Layout.rightMargin: 16; Layout.topMargin: 12; Layout.bottomMargin: 12
            spacing: 12
            Text { text: qsTr("Tags verwalten"); font.pixelSize: Theme.body; font.weight: Font.DemiBold; color: Theme.text }
            Item { Layout.fillWidth: true; Layout.minimumWidth: 12 }
            Rectangle {
                Layout.preferredWidth: 200 * Theme.textScale
                implicitHeight: searchRow.implicitHeight + 6
                radius: 6
                color: Theme.withAlpha(Theme.tertiary, 0.12)
                RowLayout {
                    id: searchRow
                    anchors.fill: parent; anchors.leftMargin: 6; anchors.rightMargin: 6
                    spacing: 4
                    SFIcon { symbol: "magnifyingglass"; size: Theme.caption; color: Theme.secondary }
                    TextField {
                        id: searchField
                        Layout.fillWidth: true
                        padding: 0; leftPadding: 0; rightPadding: 0
                        background: null
                        placeholderText: qsTr("Tags durchsuchen")
                        font.pixelSize: Theme.body
                        color: Theme.text
                        text: sheet.search
                        onTextEdited: sheet.search = text
                        Keys.onDownPressed: { tagList.forceActiveFocus(); sheet.step(1) }
                        Sensitive {}
                    }
                }
            }
        }
        Rectangle { Layout.fillWidth: true; implicitHeight: 1; color: Theme.border }

        // ContentUnavailableView „Noch keine Tags“, centred in the free space
        Item {
            visible: TagsModel.tags.length === 0
            Layout.fillWidth: true; Layout.fillHeight: true
            ColumnLayout {
                anchors.centerIn: parent
                width: Math.min(parent.width - 32, 380 * Theme.textScale)
                spacing: 8
                SFIcon { Layout.alignment: Qt.AlignHCenter; symbol: "number"; size: 32; color: Theme.tertiary }
                Text { Layout.alignment: Qt.AlignHCenter; text: qsTr("Noch keine Tags"); font.pixelSize: Theme.body + 3; font.weight: Font.Bold; color: Theme.text }
                Text {
                    Layout.fillWidth: true
                    horizontalAlignment: Text.AlignHCenter; wrapMode: Text.WordWrap
                    text: qsTr("Tags vergeben Sie in der rechten Seitenleiste einer Buchung oder per Rechtsklick in der Buchungsliste.")
                    font.pixelSize: Theme.body; color: Theme.secondary
                }
                Button { Layout.alignment: Qt.AlignHCenter; text: qsTr("Neuer Tag …"); onClicked: sheet.openEditor("create", null) }
            }
        }

        // header: „Tag“ … „Buchungen“
        RowLayout {
            visible: TagsModel.tags.length > 0
            Layout.fillWidth: true
            Layout.leftMargin: 20; Layout.rightMargin: 20; Layout.topMargin: 10; Layout.bottomMargin: 2
            Text { text: qsTr("Tag"); font.pixelSize: Theme.caption; font.weight: Font.DemiBold; color: Theme.secondary }
            Item { Layout.fillWidth: true }
            Text { text: qsTr("Buchungen"); font.pixelSize: Theme.caption; font.weight: Font.DemiBold; color: Theme.secondary }
        }
        ListView {
            id: tagList
            visible: TagsModel.tags.length > 0
            Layout.fillWidth: true; Layout.fillHeight: true
            Layout.leftMargin: 8; Layout.rightMargin: 8
            clip: true
            focus: true
            boundsBehavior: Flickable.StopAtBounds
            // rows are dragged to reorder; the wheel still scrolls
            interactive: false
            model: sheet.shown
            ScrollBar.vertical: AppScrollBar {}
            WheelHandler {
                onWheel: event => {
                    const max = Math.max(0, tagList.contentHeight - tagList.height)
                    tagList.contentY = Math.max(0, Math.min(max, tagList.contentY - event.angleDelta.y / 2))
                }
            }
            // Return: rename the one selected tag (as in Finder); ⌫ / Entf: delete
            Keys.onReturnPressed: sheet.rename()
            Keys.onEnterPressed: sheet.rename()
            Keys.onDeletePressed: sheet.confirmDeleteSelection()
            Keys.onPressed: event => {
                if (event.key === Qt.Key_Backspace) { sheet.confirmDeleteSelection(); event.accepted = true }
            }
            Keys.onUpPressed: sheet.step(-1)
            Keys.onDownPressed: sheet.step(1)
            delegate: TagManagerRow {}
            Text {
                anchors.centerIn: parent
                visible: sheet.search.trim() !== "" && tagList.count === 0
                text: qsTr("Keine Treffer")
                font.pixelSize: Theme.caption; color: Theme.secondary
            }
        }
        Rectangle { Layout.fillWidth: true; implicitHeight: 1; color: Theme.border }

        // bottomBar: +/− square buttons beneath the list (as in the account groups), then the actions
        RowLayout {
            Layout.fillWidth: true
            Layout.leftMargin: 12; Layout.rightMargin: 12; Layout.topMargin: 8; Layout.bottomMargin: 8
            spacing: 12
            RowLayout {
                spacing: 0
                IconButton { symbol: "plus"; symbolSize: 12; implicitWidth: 26; implicitHeight: 22
                             ToolTip.text: qsTr("Neuer Tag … (Strg+N)"); Accessible.name: qsTr("Neuer Tag")
                             onClicked: sheet.openEditor("create", null) }
                Rectangle { implicitWidth: 1; implicitHeight: 16; color: Theme.border }
                IconButton { symbol: "minus"; symbolSize: 12; implicitWidth: 26; implicitHeight: 22
                             enabled: sheet.selectedIDs.length > 0
                             ToolTip.text: sheet.selectedIDs.length > 1 ? qsTr("Ausgewählte Tags löschen …") : qsTr("Tag löschen …")
                             Accessible.name: qsTr("Tag löschen")
                             onClicked: sheet.confirmDeleteSelection() }
            }
            Button {
                id: mergeButton
                text: qsTr("Zusammenführen …")
                enabled: sheet.canMerge
                onClicked: mergeSheet.open()
                // .help also on the disabled button
                HoverHandler { id: mergeHover }
                ToolTip.visible: mergeHover.hovered
                ToolTip.delay: 600
                ToolTip.text: sheet.canMerge ? qsTr("Ausgewählte Tags zu einem vereinen") : qsTr("Mindestens zwei Tags auswählen (Strg-Klick)")
            }
            Item { Layout.fillWidth: true; Layout.minimumWidth: 8 }
            Button {
                id: sortButton
                text: qsTr("Sortieren") + "  ▾"
                enabled: TagsModel.tags.length > 1
                onClicked: sortMenu.popup(sortButton, 0, sortButton.height)
                AppMenu {
                    id: sortMenu
                    Action { text: qsTr("Nach Name"); onTriggered: TagManager.setOrder(TagManager.idsByName(TagManager.current)) }
                    Action { text: qsTr("Meiste Buchungen zuerst"); onTriggered: TagManager.setOrder(TagManager.idsByBookings(TagsModel.tags, true)) }
                    Action { text: qsTr("Wenigste Buchungen zuerst"); onTriggered: TagManager.setOrder(TagManager.idsByBookings(TagsModel.tags, false)) }
                    AppMenuSeparator {}
                    Action { text: qsTr("Oder Tags in der Liste ziehen."); enabled: false }
                }
            }
            Button { text: qsTr("Ungenutzte entfernen …"); enabled: TagManager.unusedTags.length > 0; onClicked: unusedSheet.open() }
        }
        Rectangle { Layout.fillWidth: true; implicitHeight: 1; color: Theme.border }

        // „Schließen“ in its own row, with the pending hint and the keys this sheet answers to
        RowLayout {
            Layout.fillWidth: true
            Layout.margins: 12
            ColumnLayout {
                Layout.fillWidth: true
                spacing: 2
                Text {
                    visible: !TagManager.draftEmpty
                    Layout.fillWidth: true; wrapMode: Text.WordWrap
                    text: qsTr("Neue Namen, Farben und die Reihenfolge gelten, sobald Sie schließen.")
                    font.pixelSize: Theme.caption; color: Theme.secondary
                }
                Text {
                    Layout.fillWidth: true; wrapMode: Text.WordWrap
                    text: qsTr("Strg+N Neuer Tag · ↩ Umbenennen · Entf Löschen · Esc Auswahl aufheben · Zahl: je Konto")
                    font.pixelSize: Theme.caption2; color: Theme.tertiary
                }
            }
            // no Return shortcut: Return renames the selected tag
            Button { text: qsTr("Schließen"); onClicked: sheet.close() }
        }
    }

    // MARK: - rows
    /// TagManagerRow: grip, colour (click: palette), name, booking count.
    component TagManagerRow: Rectangle {
        id: row
        required property var modelData
        required property int index
        readonly property bool isSelected: !!sheet.selection[modelData.id]
        property real homeY: 0
        width: ListView.view.width
        implicitHeight: Math.max(26, nameText.implicitHeight + 8)
        radius: 5
        z: drag.active ? 10 : 0
        color: isSelected ? (tagList.activeFocus ? Theme.withAlpha(Theme.accent, 0.22) : Theme.withAlpha(Theme.tertiary, 0.18))
                          : drag.active ? Theme.card : "transparent"
        RowLayout {
            anchors.fill: parent; anchors.leftMargin: 6; anchors.rightMargin: 8
            spacing: 8
            // the grip (line.3.horizontal); hidden while searching
            Column {
                opacity: sheet.movable ? 1 : 0
                spacing: 2
                Repeater { model: 3; Rectangle { width: 10; height: 1.5; radius: 0.75; color: Theme.tertiary } }
                HoverHandler { id: gripHover }
                ToolTip.visible: gripHover.hovered && sheet.movable
                ToolTip.delay: 600
                ToolTip.text: qsTr("Zum Verschieben ziehen")
            }
            Item {
                id: dot
                implicitWidth: 18; implicitHeight: 18
                Rectangle { anchors.centerIn: parent; width: 12; height: 12; radius: 6
                            color: row.modelData.color_hex === "" ? Theme.secondary : Theme.readable(row.modelData.color_hex) }
                MouseArea { id: dotMouse; anchors.fill: parent; hoverEnabled: true; cursorShape: Qt.PointingHandCursor
                            onClicked: colorPopover.openFor(row.modelData, dot) }
                ToolTip.visible: dotMouse.containsMouse
                ToolTip.delay: 600
                ToolTip.text: qsTr("Farbe ändern")
            }
            Text {
                id: nameText
                Layout.fillWidth: true
                text: row.modelData.name
                font.pixelSize: Theme.body; color: Theme.text
                wrapMode: Text.Wrap; maximumLineCount: 2; elide: Text.ElideRight
                Sensitive {}
            }
            // TagBookingCount (#228, part 3): hovering (or clicking) it shows the bookings per account
            Item {
                id: countItem
                readonly property bool active: row.modelData.booking_count > 0
                implicitWidth: countText.implicitWidth + 8; implicitHeight: countText.implicitHeight + 2
                Text {
                    id: countText
                    anchors.centerIn: parent
                    text: Fmt.count(row.modelData.booking_count)
                    font.pixelSize: Theme.callout; font.features: { "tnum": 1 }
                    color: row.modelData.booking_count === 0 ? Theme.tertiary : Theme.secondary
                }
                MouseArea {
                    anchors.fill: parent
                    enabled: countItem.active
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onContainsMouseChanged: sheet.countHovered(containsMouse, row.modelData, countItem)
                    onClicked: accountsPopover.toggleFor(row.modelData, countItem)
                }
                ToolTip.visible: countItem.active && countItemHover.hovered && !accountsPopover.visible
                ToolTip.delay: 600
                ToolTip.text: qsTr("Buchungen nach Konten")
                HoverHandler { id: countItemHover }
                // screenshots: --demo-sheet tags-accounts (once the sheet stands)
                Timer {
                    running: AppModel.demoSheet === "tags-accounts" && row.index === 0 && countItem.active
                    interval: 800
                    onTriggered: accountsPopover.openFor(row.modelData, countItem)
                }
            }
        }
        TapHandler {
            id: leftTap
            acceptedButtons: Qt.LeftButton
            onTapped: sheet.click(row.index, leftTap.point.modifiers)
            // double-click: rename
            onDoubleTapped: { sheet.select([row.modelData.id]); sheet.openEditor("rename", row.modelData.id) }
        }
        TapHandler {
            acceptedButtons: Qt.RightButton
            onTapped: {
                // the menu acts on the selection when the row is part of it, else on the row
                if (!row.isSelected) { sheet.select([row.modelData.id]); sheet.anchorIndex = row.index }
                rowMenu.ids = sheet.selectedIDs
                rowMenu.popup()
            }
        }
        DragHandler {
            id: drag
            enabled: sheet.movable
            xAxis.enabled: false
            dragThreshold: 6
            onActiveChanged: {
                if (active) { row.homeY = row.y; return }
                const centre = row.y + row.height / 2
                let to = 0
                for (let i = 0; i < tagList.count; ++i) {
                    if (i === row.index) continue
                    const it = tagList.itemAtIndex(i)
                    if (it && it.y + it.height / 2 < centre) to++
                }
                row.y = row.homeY
                if (to !== row.index) sheet.moveTag(row.index, to)
            }
        }
    }

    // MARK: - bookings per account (TagManagerView.swift › TagBookingCount / TagAccountsPopover, #228 part 3)
    property bool overCount: false
    property bool overPopover: false
    /// The count the pointer rests on: { tag, anchor }.
    property var hoverTarget: null
    /// Opens after a short rest on the count; closes once the pointer has left both the count
    /// and the popover (the gap between them takes a moment to cross).
    function countHovered(inside, tag, anchor) {
        overCount = inside
        if (inside) hoverTarget = { tag: tag, anchor: anchor }
        hoverChanged()
    }
    function hoverChanged() {
        hoverTimer.stop()
        const open = overCount || overPopover
        const showing = accountsPopover.visible && (!overCount || (hoverTarget && accountsPopover.tag && accountsPopover.tag.id === hoverTarget.tag.id))
        if (open === showing) return
        hoverTimer.opening = open
        hoverTimer.interval = open ? 350 : 300
        hoverTimer.start()
    }
    Timer {
        id: hoverTimer
        property bool opening: false
        onTriggered: {
            if ((sheet.overCount || sheet.overPopover) !== opening) return
            if (opening && sheet.overCount && sheet.hoverTarget) accountsPopover.openFor(sheet.hoverTarget.tag, sheet.hoverTarget.anchor)
            else if (!opening) accountsPopover.close()
        }
    }
    Popup {
        id: accountsPopover
        property var tag: null
        /// [TagAccountCount] or null while loading.
        property var rows: null
        readonly property var own: rows ? rows.filter(r => !r.linked) : []
        readonly property var linked: rows ? rows.filter(r => r.linked) : []
        function openFor(t, anchor) {
            tag = t
            rows = null
            parent = anchor
            // to the left, over the list: the count sits at the sheet's right edge
            x = -width - 6
            place()
            open()
            const id = t.id
            TagFilter.loadAccountCounts(id, r => { if (accountsPopover.tag && accountsPopover.tag.id === id) accountsPopover.rows = r || [] })
        }
        /// Keeps the popover inside the sheet, also once the rows have arrived.
        function place() {
            if (!parent || !parent.mapToItem) return
            const top = parent.mapToItem(sheet.contentItem, 0, 0).y
            y = Math.max(-top + 12, Math.min(0, sheet.height - top - implicitHeight - 24))
        }
        onImplicitHeightChanged: if (visible) place()
        function toggleFor(t, anchor) {
            hoverTimer.stop()
            if (visible && tag && tag.id === t.id) close(); else openFor(t, anchor)
        }
        width: 280 * Theme.textScale
        padding: 12
        closePolicy: Popup.CloseOnEscape | Popup.CloseOnPressOutsideParent
        background: Rectangle { radius: 9; color: Theme.card; border.color: Theme.border }
        onClosed: sheet.overPopover = false
        contentItem: ColumnLayout {
            spacing: 6
            HoverHandler { onHoveredChanged: { sheet.overPopover = hovered; sheet.hoverChanged() } }
            Text { text: qsTr("Buchungen nach Konten"); font.pixelSize: Theme.body; font.weight: Font.DemiBold; color: Theme.text }
            Spinner { visible: accountsPopover.rows === null; size: 16 }
            // many accounts: the rows scroll, the popover stays inside the sheet
            Flickable {
                id: accountFlick
                Layout.fillWidth: true
                Layout.preferredHeight: Math.min(accountRows.implicitHeight, Math.max(80, sheet.height - 160))
                contentHeight: accountRows.implicitHeight
                clip: true
                boundsBehavior: Flickable.StopAtBounds
                ScrollBar.vertical: ScrollBar { policy: accountRows.implicitHeight > accountFlick.height ? ScrollBar.AsNeeded : ScrollBar.AlwaysOff }
                ColumnLayout {
                    id: accountRows
                    width: accountFlick.width
                    spacing: 6
                    // each booking once, at its own account (adds up to the tag's count) …
                    Repeater { model: accountsPopover.own; AccountCountRow { required property var modelData; Layout.fillWidth: true; entry: modelData } }
                    // … then loans whose view shows some of them too (rates linked from the Girokonto)
                    Rectangle { visible: accountsPopover.linked.length > 0; Layout.fillWidth: true; implicitHeight: 1; color: Theme.border }
                    Repeater { model: accountsPopover.linked; AccountCountRow { required property var modelData; Layout.fillWidth: true; entry: modelData } }
                }
            }
            Text {
                visible: accountsPopover.linked.length > 0
                Layout.fillWidth: true; wrapMode: Text.WordWrap
                text: qsTr("Raten von anderen Konten, die mit dem Kredit verknüpft sind.")
                font.pixelSize: Theme.caption; color: Theme.secondary
            }
            Text {
                visible: accountsPopover.rows !== null
                Layout.fillWidth: true; wrapMode: Text.WordWrap
                text: qsTr("Klick auf ein Konto zeigt seine Buchungen mit diesem Tag.")
                font.pixelSize: Theme.caption; color: Theme.secondary
            }
        }
    }
    /// TagAccountsPopover.accountRow: a click closes the sheet and lists the bookings.
    component AccountCountRow: Item {
        id: acRow
        property var entry
        readonly property var account: entry ? AppModel.account(entry.account_id) : null
        implicitHeight: acLayout.implicitHeight + 4
        opacity: account ? 1 : 0.5
        RowLayout {
            id: acLayout
            anchors.fill: parent
            spacing: 4
            Text {
                // the name truncates, the count never
                Layout.maximumWidth: Math.max(40, acRow.width - linkedText.width - acCount.implicitWidth - 24)
                text: acRow.account ? acRow.account.name : qsTr("Unbekanntes Konto")
                font.pixelSize: Theme.body; color: Theme.text; elide: Text.ElideRight
                Sensitive {}
            }
            Text { id: linkedText; visible: acRow.entry && acRow.entry.linked; text: qsTr("(verknüpft)"); font.pixelSize: Theme.body; color: Theme.secondary }
            Item { Layout.fillWidth: true; Layout.minimumWidth: 12 }
            Text {
                id: acCount
                text: acRow.entry ? Fmt.count(acRow.entry.booking_count) : ""
                font.pixelSize: Theme.callout; font.features: { "tnum": 1 }; color: Theme.secondary
            }
        }
        MouseArea {
            id: acMouse
            anchors.fill: parent
            enabled: acRow.account !== null
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: { const t = accountsPopover.tag; accountsPopover.close(); TagFilter.showBookings(t.id, acRow.entry.account_id) }
        }
        ToolTip.visible: acMouse.containsMouse; ToolTip.delay: 600; ToolTip.text: qsTr("Diese Buchungen in der Liste zeigen")
    }

    // .contextMenu(forSelectionType:)
    AppMenu {
        id: rowMenu
        property var ids: []
        AppMenuItem { text: qsTr("Umbenennen …"); visible: rowMenu.ids.length === 1; height: visible ? implicitHeight : 0
                      onTriggered: sheet.openEditor("rename", rowMenu.ids[0]) }
        AppMenuItem { text: qsTr("Zusammenführen …"); visible: rowMenu.ids.length > 1; height: visible ? implicitHeight : 0
                      onTriggered: { sheet.select(rowMenu.ids); mergeSheet.open() } }
        AppMenuSeparator {}
        AppMenuItem { text: rowMenu.ids.length > 1 ? qsTr("%1 Tags löschen …").arg(rowMenu.ids.length) : qsTr("Löschen …")
                      onTriggered: { sheet.select(rowMenu.ids); deleteDialog.open() } }
    }

    // MARK: - colour (TagColorPalette in a popover)
    Popup {
        id: colorPopover
        property var tag: null
        function openFor(t, anchor) {
            tag = t
            parent = anchor
            x = -12; y = anchor.height + 4
            open()
        }
        readonly property string current: tag ? tag.color_hex.toUpperCase() : ""
        /// TagColorPalette.names
        readonly property var names: ({ "#2E86DE": qsTr("Blau"), "#27AE60": qsTr("Grün"), "#E67E22": qsTr("Orange"), "#8E44AD": qsTr("Violett"),
                                        "#C0392B": qsTr("Rot"), "#16A085": qsTr("Türkis"), "#D4AC0D": qsTr("Gelb"), "#7F8C8D": qsTr("Grau") })
        padding: 12
        closePolicy: Popup.CloseOnEscape | Popup.CloseOnPressOutside
        background: Rectangle { radius: 9; color: Theme.card; border.color: Theme.border }
        // stored when the sheet closes, like names and order
        function pick(hex) { TagManager.setColor(tag.id, hex); close() }
        /// The colour dialog is created on first use (it is slow to build on some platforms).
        function chooseCustom() {
            colorDialogLoader.active = true
            const d = colorDialogLoader.item
            d.selectedColor = current !== "" ? current : Theme.accent
            d.open()
        }
        contentItem: ColumnLayout {
            spacing: 8
            Row {
                spacing: 8
                Repeater {
                    model: TagsModel.palette
                    Item {
                        id: swatch
                        required property string modelData
                        width: 26; height: 26
                        Rectangle { anchors.centerIn: parent; width: 26; height: 26; radius: 13; color: "transparent"
                                    border.width: 2; border.color: colorPopover.current === swatch.modelData ? Theme.text : "transparent" }
                        Rectangle { anchors.centerIn: parent; width: 20; height: 20; radius: 10; color: swatch.modelData }
                        HoverHandler { id: swHover; cursorShape: Qt.PointingHandCursor }
                        ToolTip.visible: swHover.hovered
                        ToolTip.delay: 600
                        ToolTip.text: colorPopover.names[swatch.modelData] || swatch.modelData
                        TapHandler { onTapped: colorPopover.pick(swatch.modelData) }
                    }
                }
            }
            RowLayout {
                spacing: 12
                Text {
                    text: qsTr("Keine Farbe")
                    font.pixelSize: Theme.caption; font.underline: noneMouse.containsMouse && noneMouse.enabled
                    color: colorPopover.current === "" ? Theme.tertiary : Theme.accent
                    Accessible.role: Accessible.Button
                    Accessible.name: text
                    MouseArea { id: noneMouse; anchors.fill: parent; hoverEnabled: true; enabled: colorPopover.current !== ""
                                cursorShape: Qt.PointingHandCursor; onClicked: colorPopover.pick("") }
                }
                Item { Layout.fillWidth: true; Layout.minimumWidth: 8 }
                Text { text: qsTr("Eigene Farbe"); font.pixelSize: Theme.caption; color: Theme.secondary }
                // ColorPicker: the system colour dialog
                Rectangle {
                    implicitWidth: 26; implicitHeight: 20; radius: 4
                    border.color: Theme.border
                    color: colorPopover.current === "" ? Theme.accent : colorPopover.current
                    HoverHandler { id: customHover; cursorShape: Qt.PointingHandCursor }
                    ToolTip.visible: customHover.hovered
                    ToolTip.delay: 600
                    ToolTip.text: qsTr("Weitere Farben aus dem Farbfenster")
                    TapHandler { onTapped: colorPopover.chooseCustom() }
                }
            }
        }
    }
    Loader {
        id: colorDialogLoader
        active: false
        sourceComponent: Component {
            ColorDialog {
                title: qsTr("Eigene Farbe")
                onAccepted: {
                    const c = selectedColor
                    const h = x => ("0" + Math.round(x * 255).toString(16)).slice(-2)
                    // the popover stays open (the Mac's colour panel sends every change)
                    if (colorPopover.tag) TagManager.setColor(colorPopover.tag.id, ("#" + h(c.r) + h(c.g) + h(c.b)).toUpperCase())
                }
            }
        }
    }

    // MARK: - delete
    ConfirmDialog {
        id: deleteDialog
        readonly property var tags: sheet.selected
        readonly property var one: tags.length === 1 ? tags[0] : null
        heading: one ? qsTr("„%1“ löschen?").arg(one.name) : qsTr("%1 Tags löschen?").arg(tags.length)
        message: !one ? qsTr("Die Tags werden bei allen Buchungen entfernt. Die Buchungen bleiben.")
                 : one.booking_count === 0 ? qsTr("Der Tag ist keiner Buchung zugeordnet.")
                 : one.booking_count === 1 ? qsTr("Der Tag ist einer Buchung zugeordnet und wird dort entfernt. Die Buchung bleibt.")
                 : qsTr("Der Tag ist %1 Buchungen zugeordnet und wird bei allen entfernt. Die Buchungen bleiben.").arg(one.booking_count)
        actionTitle: qsTr("Löschen")
        onConfirmed: {
            TagManager.deleteTags(sheet.selectedIDs)
            sheet.selection = ({})
        }
        onClosed: tagList.forceActiveFocus()
    }

    // MARK: - „Neuer Tag …“ / „Umbenennen …“ (TagNameEditor)
    Sheet {
        id: nameEditor
        property string mode: "create"
        property var tagID: null
        /// Set on save: the sheet keeps its last look while it closes instead of reporting the
        /// name it just created as taken.
        property bool saved: false
        sheetWidth: 380
        padding: 20
        onAboutToShow: {
            saved = false
            const t = mode === "rename" ? TagManager.current.find(x => x.id === tagID) : null
            nameField.text = t ? t.name : ""
        }
        onOpened: { nameField.forceActiveFocus(); nameField.selectAll() }
        onClosed: tagList.forceActiveFocus()
        /// The checks run against the names the tag sheet shows: { canSave, hint, edge }.
        readonly property var check: {
            if (saved) return { canSave: true, hint: "", edge: null }
            const tags = TagManager.current, name = nameField.text
            const tooLong = { canSave: false, hint: qsTr("Höchstens %1 Zeichen").arg(TagsModel.maxTagNameLength), edge: Theme.negative }
            if (mode === "create") {
                const i = TagManager.entryIntent(name, tags)
                switch (i.kind) {
                case "create": return { canSave: true, hint: "", edge: null }
                case "add": return { canSave: false, hint: qsTr("„%1“ gibt es schon.").arg(i.existing), edge: Theme.warning }
                case "similar": return { canSave: true, hint: qsTr("Ähnlich wie „%1“.").arg(i.existing), edge: Theme.warning }
                case "tooLong": return tooLong
                }
                return { canSave: false, hint: "", edge: null }
            }
            const r = TagManager.renameIntent(name, tags, tagID)
            switch (r.kind) {
            case "rename": return { canSave: true, hint: "", edge: null }
            case "taken": return { canSave: false, hint: qsTr("„%1“ gibt es schon. Um beide zu vereinen: „Zusammenführen …“.").arg(r.existing), edge: Theme.warning }
            case "similar": return { canSave: true, hint: qsTr("Ähnlich wie „%1“.").arg(r.existing), edge: Theme.warning }
            case "tooLong": return tooLong
            }
            return { canSave: false, hint: "", edge: null }
        }
        function save() {
            if (saved || !check.canSave) return
            saved = true
            if (mode === "create") TagManager.createFromSheet(nameField.text)
            else TagManager.setRename(tagID, TagsModel.cleanTagName(nameField.text))
            close()
        }
        ColumnLayout {
            width: nameEditor.availableWidth
            spacing: 10
            Text { text: nameEditor.mode === "create" ? qsTr("Neuer Tag") : qsTr("Tag umbenennen")
                   font.pixelSize: Theme.body; font.weight: Font.DemiBold; color: Theme.text }
            TextField {
                id: nameField
                Layout.fillWidth: true
                placeholderText: qsTr("Name")
                font.pixelSize: Theme.body
                onAccepted: nameEditor.save()
                Rectangle {
                    anchors.fill: parent
                    visible: nameEditor.check.edge !== null
                    color: "transparent"; radius: 6
                    border.width: 1.5; border.color: nameEditor.check.edge || "transparent"
                }
                Sensitive {}
            }
            Text {
                visible: nameEditor.check.hint !== ""
                Layout.fillWidth: true
                text: nameEditor.check.hint
                font.pixelSize: Theme.caption
                color: nameEditor.check.edge || Theme.secondary
                wrapMode: Text.WordWrap; maximumLineCount: 3; elide: Text.ElideRight
                Sensitive {}
            }
            RowLayout {
                Item { Layout.fillWidth: true }
                Button { text: qsTr("Abbrechen"); onClicked: nameEditor.close() }
                Button { text: nameEditor.mode === "create" ? qsTr("Anlegen") : qsTr("Umbenennen"); highlighted: true
                         enabled: nameEditor.check.canSave; onClicked: nameEditor.save() }
            }
        }
    }

    // MARK: - „Zusammenführen …“ (TagMergeSheet)
    Sheet {
        id: mergeSheet
        /// Taken on open, so the sheet keeps its look while it closes.
        property var choices: []
        property var sources: []
        property var target: null
        readonly property var staying: choices.find(t => t.id === target) || null
        readonly property var leaving: choices.filter(t => t.id !== target)
        sheetWidth: 440
        padding: 20
        onAboutToShow: {
            sources = sheet.selectedIDs
            choices = TagManager.current.filter(t => sheet.selection[t.id])
            // preset: the one with the most bookings
            let best = null
            for (const t of choices) if (best === null || t.booking_count > best.booking_count) best = t
            target = best ? best.id : null
        }
        onClosed: tagList.forceActiveFocus()
        ColumnLayout {
            width: mergeSheet.availableWidth
            spacing: 14
            ColumnLayout {
                spacing: 4
                Text { text: qsTr("Tags zusammenführen"); font.pixelSize: Theme.body; font.weight: Font.DemiBold; color: Theme.text }
                Text { Layout.fillWidth: true; wrapMode: Text.WordWrap
                       text: qsTr("Ein Tag bleibt und übernimmt die Buchungen der anderen. Um einen anderen zu behalten, klicken Sie ihn an.")
                       font.pixelSize: Theme.callout; color: Theme.secondary }
            }
            ColumnLayout {
                visible: mergeSheet.staying !== null
                spacing: 6
                Text { text: qsTr("Bleibt"); font.pixelSize: Theme.caption; font.weight: Font.DemiBold; color: Theme.secondary }
                TagChipFlow { Layout.fillWidth: true; tags: mergeSheet.staying ? [mergeSheet.staying] : []; highlighted: true }
            }
            ColumnLayout {
                visible: mergeSheet.staying !== null && mergeSheet.leaving.length > 0
                spacing: 6
                Text {
                    Layout.fillWidth: true; wrapMode: Text.WordWrap
                    text: !mergeSheet.staying ? ""
                          : mergeSheet.leaving.length === 1 ? qsTr("Geht in „%1“ auf").arg(mergeSheet.staying.name)
                          : qsTr("Gehen in „%1“ auf (%2)").arg(mergeSheet.staying.name).arg(mergeSheet.leaving.length)
                    font.pixelSize: Theme.caption; font.weight: Font.DemiBold; color: Theme.secondary
                    Sensitive {}
                }
                TagChipFlow { Layout.fillWidth: true; tags: mergeSheet.leaving; maxHeight: 140
                              tapHelp: qsTr("Stattdessen diesen Tag behalten"); onTapped: t => mergeSheet.target = t.id }
            }
            Text {
                visible: mergeSheet.staying !== null && mergeSheet.leaving.length > 0
                Layout.fillWidth: true; wrapMode: Text.WordWrap
                font.pixelSize: Theme.callout; color: Theme.text
                text: {
                    const t = mergeSheet.staying, l = mergeSheet.leaving
                    if (!t || l.length === 0) return ""
                    if (l.length === 1) return qsTr("„%1“ bleibt und übernimmt die Buchungen von „%2“. „%3“ wird danach gelöscht. Das lässt sich nicht widerrufen.").arg(t.name).arg(l[0].name).arg(l[0].name)
                    return qsTr("„%1“ bleibt und übernimmt die Buchungen der anderen %2 Tags. Diese werden danach gelöscht. Das lässt sich nicht widerrufen.").arg(t.name).arg(l.length)
                }
                Sensitive {}
            }
            RowLayout {
                Item { Layout.fillWidth: true }
                Button { text: qsTr("Abbrechen"); onClicked: mergeSheet.close() }
                Button {
                    text: qsTr("Zusammenführen"); highlighted: true
                    enabled: mergeSheet.staying !== null && mergeSheet.leaving.length > 0
                    onClicked: {
                        const target = mergeSheet.target
                        TagManager.mergeTags(mergeSheet.sources, target)
                        // the window selects the tag that stays
                        sheet.select([target])
                        mergeSheet.close()
                    }
                }
            }
        }
    }

    // MARK: - „Ungenutzte entfernen …“ (TagUnusedSheet)
    Sheet {
        id: unusedSheet
        /// Taken on open, so the chips don't vanish while the sheet closes.
        property var unused: []
        sheetWidth: 440
        padding: 20
        onAboutToShow: unused = TagManager.current.filter(t => t.booking_count === 0)
        onClosed: tagList.forceActiveFocus()
        ColumnLayout {
            width: unusedSheet.availableWidth
            spacing: 12
            Text { text: unusedSheet.unused.length === 1 ? qsTr("1 ungenutzten Tag entfernen?") : qsTr("%1 ungenutzte Tags entfernen?").arg(unusedSheet.unused.length)
                   font.pixelSize: Theme.body; font.weight: Font.DemiBold; color: Theme.text }
            Text { text: qsTr("Diese Tags sind keiner Buchung zugeordnet."); font.pixelSize: Theme.callout; color: Theme.secondary
                   Layout.fillWidth: true; wrapMode: Text.WordWrap }
            TagChipFlow { Layout.fillWidth: true; tags: unusedSheet.unused }
            RowLayout {
                Item { Layout.fillWidth: true }
                Button { text: qsTr("Abbrechen"); onClicked: unusedSheet.close() }
                Button { text: qsTr("Entfernen"); highlighted: true; palette.buttonText: Theme.negative
                         enabled: unusedSheet.unused.length > 0
                         onClicked: { TagManager.removeUnusedTags(); unusedSheet.close() } }
            }
        }
    }

    /// TagChipFlow: tags as coloured chips that wrap; scrolls only beyond `maxHeight` (scaled).
    /// With `tapHelp` the chips are buttons (`tapped(tag)`).
    component TagChipFlow: Flickable {
        id: cf
        property var tags: []
        property real maxHeight: 200
        property bool highlighted: false
        property string tapHelp: ""
        signal tapped(var tag)
        implicitHeight: Math.min(Math.max(flow.implicitHeight, 20), maxHeight * Theme.textScale)
        contentHeight: flow.implicitHeight
        clip: true
        boundsBehavior: Flickable.StopAtBounds
        interactive: contentHeight > height
        ScrollBar.vertical: AppScrollBar {}
        Flow {
            id: flow
            width: cf.width
            spacing: 6
            Repeater {
                model: cf.tags
                TagChip {
                    required property var modelData
                    name: modelData.name
                    colorHex: modelData.color_hex
                    maxWidth: flow.width
                    highlighted: cf.highlighted
                    tappable: cf.tapHelp !== ""
                    toolTip: cf.tapHelp
                    onTapped: cf.tapped(modelData)
                }
            }
        }
    }
}
