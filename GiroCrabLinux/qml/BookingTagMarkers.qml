import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// TagsUI.swift › BookingTagMarkers (#228): up to two tag markers for a booking row, then „+n“.
// Names are cut with „…“ so a long one can't push the row apart; the full name is the tooltip
// (left out while a bug report picture is taken: a tooltip can't be covered).
RowLayout {
    id: markers
    property var tags: []
    spacing: 4
    visible: tags.length > 0

    Repeater {
        model: markers.tags.slice(0, 2)
        Rectangle {
            id: pill
            required property var modelData
            implicitWidth: pillRow.implicitWidth + 10
            implicitHeight: pillRow.implicitHeight + 2
            radius: height / 2
            color: Theme.withAlpha(Theme.secondary, 0.12)
            RowLayout {
                id: pillRow
                anchors.centerIn: parent
                spacing: 3
                Rectangle { implicitWidth: 5; implicitHeight: 5; radius: 2.5; color: pill.modelData.color_hex || Theme.secondary }
                Text {
                    text: pill.modelData.name
                    font.pixelSize: Theme.caption2; color: Theme.secondary
                    elide: Text.ElideRight
                    Layout.maximumWidth: 110 * Theme.textScale
                    Sensitive {}
                }
            }
            HoverHandler { id: pillHover }
            ToolTip.visible: pillHover.hovered && (AppModel.effectiveRedaction & AppModel.redactPersonal) === 0
            ToolTip.delay: 600
            ToolTip.text: pill.modelData.name
        }
    }
    // TagOverflowBadge: a short hover shows every tag of the booking as coloured chips in a
    // popover (a tooltip can only show plain text). Popovers are never part of a bug report picture.
    Text {
        id: badge
        visible: markers.tags.length > 2
        text: "+" + (markers.tags.length - 2)
        font.pixelSize: Theme.caption2; color: Theme.secondary
        leftPadding: 2; rightPadding: 2
        HoverHandler {
            id: badgeHover
            onHoveredChanged: if (hovered) hoverTimer.restart(); else { hoverTimer.stop(); allTags.close() }
        }
        // long enough that brushing over the list shows nothing, short enough to be found
        Timer { id: hoverTimer; interval: 500; onTriggered: allTags.open() }
        TagPopover {
            id: allTags
            tags: markers.tags
            y: badge.height + 4
            x: (badge.width - width) / 2
        }
    }
}
