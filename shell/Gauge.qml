import QtQuick
import qs.Commons

// One reading in the window header: what it is, what it says now, and how far
// along its own scale that is. The bar underneath is the whole reason these
// beat a row of numbers — 6.2 means nothing until you can see it is most of
// the way across.
Item {
  id: root

  property var colors: null
  property string label: ""
  property string value: ""
  property real fraction: 0        // 0..100
  property color accent: "grey"

  implicitWidth: Math.max(Style.space(88),
                          Math.max(labelText.implicitWidth, valueText.implicitWidth))
  implicitHeight: labelText.implicitHeight + valueText.implicitHeight + Style.space(9)

  Text {
    id: labelText
    text: root.label
    font.family: Style.font.family
    font.pixelSize: Style.font.caption
    font.capitalization: Font.AllUppercase
    font.letterSpacing: 0.5
    color: root.colors ? root.colors.dim : "grey"
  }

  Text {
    id: valueText
    anchors.top: labelText.bottom
    text: root.value
    font.family: Style.font.family
    font.pixelSize: Style.font.heading
    font.weight: Font.DemiBold
    color: root.colors ? root.colors.ink : "black"
  }

  Rectangle {
    anchors.top: valueText.bottom
    anchors.topMargin: Style.space(3)
    width: parent.width
    height: Style.space(3)
    radius: height / 2
    color: root.colors ? root.colors.grid : "transparent"

    Rectangle {
      width: parent.width * Math.max(0, Math.min(100, root.fraction)) / 100
      height: parent.height
      radius: parent.radius
      color: root.accent
      Behavior on width { NumberAnimation { duration: 300; easing.type: Easing.OutCubic } }
    }
  }
}
