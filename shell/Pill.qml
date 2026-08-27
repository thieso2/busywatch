import QtQuick
import qs.Commons

// A range or metric button. Pressed is a filled inversion rather than a tint,
// which is what the page did: at seven of them in a row a tint is a guess and
// a fill is an answer.
Rectangle {
  id: root

  property var colors: null
  property string text: ""
  property bool pressedState: false
  // Ranges wider than the recorded history are still clickable — they simply
  // have nothing to show, and saying so beats a button that looks broken.
  property bool empty: false
  property string tooltip: ""

  signal clicked()

  implicitWidth: label.implicitWidth + Style.space(18)
  implicitHeight: Math.max(Style.space(22), label.implicitHeight + Style.space(6))
  radius: Style.space(6)
  color: pressedState ? (colors ? colors.ink : "transparent")
       : area.containsMouse ? (colors ? colors.hover : "transparent")
       : "transparent"
  border.width: 1
  border.color: pressedState ? (colors ? colors.ink : "transparent")
                             : (colors ? colors.line : "transparent")
  opacity: empty && !pressedState ? 0.45 : 1

  Text {
    id: label
    anchors.centerIn: parent
    text: root.text
    font.family: Style.font.family
    font.pixelSize: Style.font.bodySmall
    color: root.pressedState ? (root.colors ? root.colors.ground : "white")
                             : (root.colors ? root.colors.ink : "black")
  }

  MouseArea {
    id: area
    anchors.fill: parent
    hoverEnabled: true
    cursorShape: Qt.PointingHandCursor
    onClicked: root.clicked()
  }
}
