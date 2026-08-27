import QtQuick
import qs.Commons

// A drilldown figure: the absolute total first, and what it was a share of
// underneath. "1h12m of CPU" is the number that settles an argument; a
// percentage of a moment is not.
Rectangle {
  id: root

  property var colors: null
  property string label: ""
  property string value: ""
  property string sub: ""

  implicitHeight: col.implicitHeight + Style.space(16)
  radius: Style.space(8)
  color: "transparent"
  border.width: 1
  border.color: colors ? colors.line : "transparent"

  Column {
    id: col
    anchors.left: parent.left
    anchors.right: parent.right
    anchors.verticalCenter: parent.verticalCenter
    anchors.leftMargin: Style.space(10)
    anchors.rightMargin: Style.space(10)
    spacing: Style.space(2)

    Text {
      text: root.label
      font.family: Style.font.family
      font.pixelSize: Style.font.caption
      font.capitalization: Font.AllUppercase
      font.letterSpacing: 0.5
      color: root.colors ? root.colors.dim : "grey"
    }
    Text {
      text: root.value
      font.family: Style.font.family
      font.pixelSize: Style.font.heading
      font.weight: Font.DemiBold
      color: root.colors ? root.colors.ink : "black"
    }
    Text {
      width: parent.width
      text: root.sub
      visible: root.sub.length > 0
      elide: Text.ElideRight
      font.family: Style.font.family
      font.pixelSize: Style.font.bodySmall
      color: root.colors ? root.colors.dim : "grey"
    }
  }
}
