import QtQuick
import QtQuick.Layouts
import qs.Commons
import "Charge.js" as Charge

// The charging card: what the battery is doing right now, and when it will be
// done doing it.
//
// A component of its own rather than another hundred lines of App.qml, for the
// same reason Chart and DataTable are: it can then be stood up on its own and
// checked, and the charge maths behind it lives in Charge.js where it can be
// checked without any of this.
//
// Deliberately live rather than historical. The charts above already carry the
// shape over time; what a person wants beside them is the figure now.
Section {
  id: root

  property var charge: null

  readonly property var tiles: Charge.tiles(charge)

  // Fill the row rather than fix a column count. Five tiles across a wide
  // window read as one row of figures; the same five on a narrow one wrap to
  // whole rows instead of leaving the last tile stranded on a line of its own.
  readonly property int cols: {
    var per = Style.space(170) + Style.space(10)
    var fit = Math.max(1, Math.floor((tileFlow.width + Style.space(10)) / per))
    return Math.min(Math.max(1, tiles.length), fit)
  }

  visible: !!charge
  spacing: Style.space(6)

  RowLayout {
    Layout.fillWidth: true
    spacing: Style.space(10)

    Text {
      text: Charge.title(root.charge)
      font.family: Style.font.family
      font.pixelSize: Style.font.subtitle
      font.weight: Font.DemiBold
      color: root.colors ? root.colors.ink : "black"
    }
    Text {
      Layout.fillWidth: true
      Layout.minimumWidth: 0
      elide: Text.ElideRight
      text: root.charge ? root.charge.pct.toFixed(0) + "% · "
        + (root.charge.acOnline ? "adapter online" : "no adapter") : ""
      font.family: Style.font.family
      font.pixelSize: Style.font.bodySmall
      color: root.colors ? root.colors.dim : "grey"
    }
  }

  Flow {
    id: tileFlow
    objectName: "tiles"
    Layout.fillWidth: true
    Layout.topMargin: Style.space(2)
    spacing: Style.space(10)

    Repeater {
      model: root.tiles
      Tile {
        colors: root.colors
        label: modelData.label
        value: modelData.value
        sub: modelData.sub
        width: (tileFlow.width - (root.cols - 1) * Style.space(10)) / root.cols
      }
    }
  }

  // Two things the numbers cannot say for themselves, and both of them are
  // what somebody would otherwise misread: watts at the terminals are not
  // watts from the wall, and an unknown charger rating is the firmware's
  // silence rather than an oversight here.
  Text {
    objectName: "note"
    Layout.fillWidth: true
    Layout.topMargin: Style.space(4)
    text: {
      if (!root.charge) return ""
      var lines = ["Watts are measured at the battery terminals. On the adapter "
        + "the charger is also feeding the running system, and nothing on this "
        + "machine reports that half."]
      if (root.charge.chargerUw === null)
        lines.push("The charger's rating is what it advertises in its PD source "
          + "capabilities. This machine's firmware does not pass those to the "
          + "kernel, so no program here can know it.")
      return lines.join(" ")
    }
    wrapMode: Text.WordWrap
    font.family: Style.font.family
    font.pixelSize: Style.font.bodySmall
    color: root.colors ? root.colors.dim : "grey"
  }
}
