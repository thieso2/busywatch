import QtQuick
import QtQuick.Layouts
import qs.Commons

// One card on the page. The browser UI drew these as bordered panels a shade
// off the background, and they carry the same job here: they are what makes
// "system pressure" and "apps" two things rather than one long scroll.
Rectangle {
  id: root

  property var colors: null
  default property alias content: body.data
  property alias spacing: body.spacing

  color: colors ? colors.panel : "transparent"
  border.color: colors ? colors.line : "transparent"
  border.width: 1
  radius: Style.space(10)

  Layout.fillWidth: true
  implicitHeight: body.implicitHeight + Style.space(24)

  ColumnLayout {
    id: body
    anchors.left: parent.left
    anchors.right: parent.right
    anchors.top: parent.top
    anchors.leftMargin: Style.space(14)
    anchors.rightMargin: Style.space(14)
    anchors.topMargin: Style.space(12)
    spacing: Style.space(6)
  }
}
