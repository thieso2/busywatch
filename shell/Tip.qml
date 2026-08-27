import QtQuick
import qs.Commons

// The hover readout. One of these for the whole window, moved to wherever the
// pointer is, because two charts can never be hovered at once and a tooltip per
// chart would be eight things to keep in step.
//
// Content is a structure rather than markup: {title, rows: [{k, v, muted,
// swatch}], note}. The page built HTML strings, which is the one thing that
// does not port — there is nothing here to parse them.
Rectangle {
  id: root

  property var colors: null
  property var content: null       // null hides it
  property real anchorX: 0
  property real anchorY: 0

  visible: !!content
  opacity: visible ? 1 : 0
  Behavior on opacity { NumberAnimation { duration: 90 } }

  z: 100
  radius: Style.space(8)
  color: Color.tooltip.background
  border.width: 1
  border.color: Color.tooltip.border
  implicitWidth: Math.min(Style.space(360), col.implicitWidth + Style.space(18))
  implicitHeight: col.implicitHeight + Style.space(14)

  // Follows the pointer, and flips to the other side rather than being clipped
  // by the window edge.
  x: {
    var want = anchorX + Style.space(14)
    if (parent && want + width > parent.width - Style.space(8))
      want = anchorX - width - Style.space(14)
    return Math.max(Style.space(4), want)
  }
  y: {
    var want = anchorY + Style.space(14)
    if (parent && want + height > parent.height - Style.space(8))
      want = anchorY - height - Style.space(14)
    return Math.max(Style.space(4), want)
  }

  Column {
    id: col
    anchors.left: parent.left
    anchors.top: parent.top
    anchors.leftMargin: Style.space(9)
    anchors.topMargin: Style.space(7)
    spacing: Style.space(3)

    Text {
      text: root.content ? String(root.content.title || "") : ""
      visible: text.length > 0
      font.family: Style.font.family
      font.pixelSize: Style.font.bodySmall
      font.weight: Font.DemiBold
      color: Color.tooltip.text
    }

    Repeater {
      model: root.content && root.content.rows ? root.content.rows : []

      Row {
        spacing: Style.space(10)

        Row {
          spacing: Style.space(5)
          Rectangle {
            anchors.verticalCenter: parent.verticalCenter
            visible: !!modelData.swatch
            width: Style.space(9); height: Style.space(9); radius: Style.space(2)
            color: modelData.swatch ? modelData.swatch : "transparent"
          }
          Text {
            text: String(modelData.k || "")
            font.family: Style.font.family
            font.pixelSize: Style.font.bodySmall
            color: modelData.muted && root.colors ? root.colors.dim : Color.tooltip.text
          }
        }

        Text {
          text: String(modelData.v || "")
          font.family: Style.font.family
          font.pixelSize: Style.font.bodySmall
          color: modelData.muted && root.colors ? root.colors.dim : Color.tooltip.text
        }
      }
    }

    Text {
      width: Style.space(300)
      text: root.content ? String(root.content.note || "") : ""
      visible: text.length > 0
      wrapMode: Text.WordWrap
      font.family: Style.font.family
      font.pixelSize: Style.font.caption
      color: root.colors ? root.colors.dim : "grey"
    }
  }
}
