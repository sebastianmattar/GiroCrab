import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// SidebarView.swift › AccountRow: signet with the unread count on its corner, the name over
// the whole width (owner as tooltip), then bank / loan progress and the balance (#624, #667).
SidebarRow {
    id: row
    required property var modelData
    readonly property var account: modelData.account
    readonly property bool archived: account.archived_unix !== null
    /// SidebarView.swift › accountRow(syncing:): its bank login is being fetched
    readonly property bool syncing: AppModel.syncingConnectionID !== null && AppModel.syncingConnectionID === account.connection_id
    /// right click: SidebarView opens accountMenu(_:)
    signal contextMenuRequested()
    readonly property var repaid: account.kind === "Loan" && account.initial_balance_cents > 0
        ? Math.max(0, Math.min(1, 1 - (-account.balance_cents) / account.initial_balance_cents)) : null
    /// While searching, prefer Kontoinhaber or IBAN so the hit is obvious.
    readonly property string searchAwareSubtitle: {
        if (!AppModel.isSidebarAccountSearching) return account.bank_name
        const q = AppModel.sidebarAccountSearch.trim()
        const compact = q.replace(/\s/g, "").toLowerCase()
        const terms = q.split(/\s+/).filter(t => t)
        if (account.owner && terms.length && terms.every(t => account.owner.toLowerCase().includes(t.toLowerCase())))
            return account.owner
        if (compact.length >= 3 && AppModel.compactAccountIds(account).some(x => x.includes(compact)))
            return account.iban ? Fmt.iban(account.iban) : account.account_number
        return account.bank_name
    }
    readonly property bool ibanSubtitle: AppModel.isSidebarAccountSearching && account.iban !== "" && searchAwareSubtitle === Fmt.iban(account.iban)
    /// SidebarView.swift › dialogAccountID: the row a confirmation dialog is about stays marked (#668).
    readonly property bool dialogTarget: AppModel.sidebarDialog !== null && !!AppModel.sidebarDialog.account
                                         && AppModel.sidebarDialog.account.id === account.id
    selectionKey: "account:" + account.id
    rowHeight: Math.max(34, Math.ceil(content.implicitHeight) + 6)
    opacity: archived ? 0.55 : 1
    TapHandler { acceptedButtons: Qt.RightButton; onTapped: row.contextMenuRequested() }
    Rectangle { anchors.fill: parent; radius: 6; visible: row.dialogTarget; color: Theme.withAlpha(Theme.accent, 0.22) }

    RowLayout {
        id: content
        anchors.left: parent.left; anchors.right: parent.right; anchors.verticalCenter: parent.verticalCenter
        anchors.leftMargin: SidebarModel.rowLeading + row.modelData.indent; anchors.rightMargin: SidebarModel.rowTrailing
        spacing: SidebarModel.gap
        // tinted like the account's marker in the booking list
        AccountSignet {
            account: row.account; size: 20; tintColor: AppModel.accountColor(row.account.id); onSelection: row.selected
            // unread count pinned to the icon's corner, so the subtitle stays untruncated
            Rectangle {
                readonly property string label: row.account.new_count <= 0 ? "" : row.account.new_count > 999 ? "999+" : String(row.account.new_count)
                visible: label !== ""
                x: parent.width - width + 5; y: -4
                width: Math.max(14, newText.implicitWidth + 8); height: 14
                radius: height / 2
                color: Theme.accent
                border.width: 1.5; border.color: Theme.sidebar
                Accessible.name: qsTr("%1 neue Buchungen").arg(row.account.new_count)
                Text { id: newText; anchors.centerIn: parent; text: parent.label; color: "white"
                       font.pixelSize: Theme.caption2; font.weight: Font.Bold; font.features: { "tnum": 1 } }
            }
        }
        ColumnLayout {
            spacing: 1
            Layout.fillWidth: true
            // row 1: the whole width for the name; the owner only as tooltip
            RowLayout {
                spacing: 4
                Layout.fillWidth: true
                Text {
                    text: row.account.name; font.pixelSize: Theme.callout; font.weight: Font.Medium; color: Theme.text
                    elide: Text.ElideRight; Layout.maximumWidth: Math.ceil(implicitWidth); Layout.fillWidth: true
                    HoverHandler { id: nameHover }
                    ToolTip.visible: nameHover.hovered && (row.account.owner || "") !== ""; ToolTip.delay: 600
                    ToolTip.text: AppModel.masking(row.account.owner || "")
                    Sensitive {}
                }
                SFIcon { visible: row.account.access_revoked; symbol: "lock.slash"; size: 10; color: "orange"
                         Accessible.name: qsTr("Zugriff entzogen")
                         HoverHandler { id: revokedHover }
                         ToolTip.visible: revokedHover.hovered; ToolTip.delay: 600
                         ToolTip.text: qsTr("Zugriff entzogen: Die Bank liefert dieses Konto nicht mehr. Buchungen bleiben erhalten.") }
                Item { Layout.fillWidth: true; Layout.preferredWidth: 0 }
                BusyIndicator { visible: row.syncing; running: row.syncing; implicitWidth: 12; implicitHeight: 12; padding: 0
                                Accessible.name: qsTr("Wird synchronisiert") }
            }
            // row 2: bank / loan progress left, balance right
            RowLayout {
                spacing: 6
                Layout.fillWidth: true
                ProgressBar {
                    visible: row.repaid !== null
                    from: 0; to: 1; value: row.repaid || 0
                    Layout.preferredWidth: 44; implicitHeight: 4
                    background: Rectangle { radius: 2; color: Theme.withAlpha(Theme.loan, 0.2) }
                    contentItem: Item { Rectangle { width: parent.width * (row.repaid || 0); height: parent.height; radius: 2; color: Theme.withAlpha(Theme.loan, 0.7) } }
                    Sensitive { amounts: true }   // SidebarView.swift: the bar shows the share too
                }
                Text {
                    visible: row.repaid !== null
                    text: qsTr("%1 getilgt").arg(Fmt.percent(row.repaid || 0)); font.pixelSize: Theme.caption2; color: Theme.secondary
                    elide: Text.ElideRight; Layout.fillWidth: true
                    Sensitive { amounts: true }
                }
                Text {
                    visible: row.repaid === null
                    text: row.archived ? qsTr("Archiviert %1").arg(new Date(row.account.archived_unix * 1000).toLocaleDateString(Fmt.locale, Fmt.pattern("dd.MM.yy"))) : row.searchAwareSubtitle
                    font.pixelSize: Theme.caption2; font.family: row.ibanSubtitle ? "monospace" : Qt.application.font.family
                    color: Theme.secondary; elide: Text.ElideRight; Layout.fillWidth: true
                    HoverHandler { id: archivedHover; enabled: row.archived }
                    ToolTip.visible: row.archived && archivedHover.hovered; ToolTip.delay: 600
                    ToolTip.text: qsTr("Archiviert – wird nicht mehr abgerufen")
                    Sensitive { when: !row.archived && row.searchAwareSubtitle !== row.account.bank_name }   // owner / IBAN hits
                }
                // archived accounts show no balance (#667); the subtitle truncates, never the balance
                Text {
                    visible: !row.archived
                    text: Fmt.money(row.account.balance_cents, row.account.currency)
                    font.pixelSize: Theme.subheadline; font.features: { "tnum": 1 }
                    color: row.selected ? Theme.text : (row.account.kind === "Loan" ? Theme.loan : (row.account.balance_cents < 0 ? Theme.negative : Theme.text))
                    Sensitive { amounts: true }
                }
            }
        }
    }

    // accounts are dragged in the "Nach Gruppe" view only
    DropSlot {
        readonly property var g: row.modelData.group || null
        key: "a:" + row.account.id
        intoKey: g ? "g:" + g.uuid : key
        payload: g ? { type: "account", id: row.account.id, label: row.account.name } : null
        enabled: g !== null
        edgeFor: (half, p) => {
            if (p.type === "group") return AppModel.canDropGroup(p.uuid, g, true) ? "into" : null
            return half
        }
        onPerform: (p, edge) => {
            if (p.type === "account") {
                const from = g.accounts.findIndex(a => a.id === p.id)
                AppModel.moveAccount(p.id, g.uuid, AppModel.insertionIndex(row.modelData.row, edge === "below", from))
            } else {
                AppModel.dropUserGroup(p.uuid, g, edge !== "into")
            }
        }
    }
}
