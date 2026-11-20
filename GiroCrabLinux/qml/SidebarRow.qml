import QtQuick

// A selectable sidebar row (List(selection:) with .listStyle(.sidebar)):
// rounded selection fill, click selects `selectionKey`. Content goes into
// the row area below `topGap`.
Item {
    id: row
    default property alias content: area.data
    property string selectionKey
    property int topGap: 0
    property int rowHeight: 30
    readonly property bool selected: AppModel.sidebarSelection === selectionKey
    /// Takes the keyboard on a click when the row is not in a ListView (the sidebar's header rows).
    property Item focusTarget: null
    width: ListView.view ? ListView.view.width : 200
    height: rowHeight + topGap

    Rectangle {
        x: 8; y: row.topGap
        width: row.width - 16; height: row.rowHeight
        radius: 6
        color: row.selected ? Theme.withAlpha(Theme.secondary, Theme.dark ? 0.28 : 0.16)
             : hover.hovered ? Theme.withAlpha(Theme.secondary, 0.07) : "transparent"
    }
    HoverHandler { id: hover }
    TapHandler {
        onTapped: {
            // a click on the row already selected („Alle Konten“ under the merchant list) ends
            // that list like the Mac's didSet, which fires on every assignment
            if (AppModel.sidebarSelection === row.selectionKey && MerchantFocus.focus !== null) {
                MerchantFocus.sidebarChanged()
                AppModel.reloadSelection(false)
            }
            AppModel.selectFromSidebar(row.selectionKey)
            // the sidebar takes the keyboard (↑/↓) like a clicked List
            if (row.ListView.view && row.ListView.view.takeKeyFocus) row.ListView.view.takeKeyFocus()
            else if (row.focusTarget) row.focusTarget.forceActiveFocus(Qt.MouseFocusReason)
        }
    }
    Item {
        id: area
        x: 8; y: row.topGap
        width: row.width - 16; height: row.rowHeight
    }
}
