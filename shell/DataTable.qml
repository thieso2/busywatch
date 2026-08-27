import QtQuick
import QtQuick.Controls
import qs.Commons

// The tables: the application rundown, the incident list, and the pids behind
// one app.
//
// The page put these in the document flow and let the whole page scroll. A
// window is not a page — four hundred applications below a chart means the
// chart is gone by the time you reach the list you were sorting. So the body
// scrolls in place under a header that stays, and the table is given a height
// rather than taking one.
Item {
  id: root

  property var colors: null
  // [{key, title, flex, align, sortable, mono}]
  property var columns: []
  property var rows: []
  // (row, key) -> {text, bold, muted, pill}
  property var cell: function (row, key) { return { text: String(row[key]) } }
  property string sortKey: ""
  property int sortDir: -1
  property string selectedKey: ""      // value of `idKey` for the selected row
  property string idKey: ""
  property int maxRows: 12
  property string emptyText: "nothing recorded yet"

  signal sortRequested(string key)
  signal activated(var row)

  readonly property int rowHeight: Math.max(Style.space(22),
                                            Style.font.body + Style.space(10))
  readonly property real totalFlex: {
    var f = 0
    for (var i = 0; i < columns.length; i++) f += (columns[i].flex || 1)
    return Math.max(1, f)
  }

  implicitHeight: header.height + Math.max(rowHeight,
    Math.min(Math.max(1, rows.length), maxRows) * rowHeight) + Style.space(2)

  // Eleven columns do not fit a narrow window, and squeezing them until every
  // figure is an ellipsis is not fitting them — it is losing them. So each
  // column has a width below which it will not go, and when the sum of those
  // exceeds the window the table scrolls sideways instead, which is what the
  // browser page did with `overflow-x: auto`. Where there is room, the slack is
  // handed out by flex and nothing scrolls.
  readonly property var widths: {
    var w = [], need = 0, i
    for (i = 0; i < columns.length; i++) {
      w.push(Style.space(columns[i].min || 78))
      need += w[i]
    }
    if (width > need) {
      var slack = width - need - Style.space(2)
      for (i = 0; i < columns.length; i++)
        w[i] += slack * (columns[i].flex || 1) / totalFlex
    }
    return w
  }

  readonly property real tableWidth: {
    var t = 0
    for (var i = 0; i < widths.length; i++) t += widths[i]
    return t
  }

  function columnWidth(i) { return widths[i] }

  Flickable {
    id: pan
    anchors.fill: parent
    contentWidth: root.tableWidth
    contentHeight: height
    flickableDirection: Flickable.HorizontalFlick
    boundsBehavior: Flickable.StopAtBounds
    clip: true

    ScrollBar.horizontal: ScrollBar {
      policy: pan.contentWidth > pan.width ? ScrollBar.AsNeeded : ScrollBar.AlwaysOff
    }

  // ------------------------------------------------------------------ header
  Row {
    id: header
    width: root.tableWidth
    height: root.rowHeight

    Repeater {
      model: root.columns

      Item {
        width: root.columnWidth(index)
        height: header.height

        // Anchored on both sides and elided, not sized to its own text: a
        // narrow window would otherwise let one heading run straight over the
        // next one rather than shortening.
        Text {
          anchors.fill: parent
          anchors.leftMargin: Style.space(6)
          anchors.rightMargin: Style.space(6)
          verticalAlignment: Text.AlignVCenter
          horizontalAlignment: modelData.align === "right" ? Text.AlignRight : Text.AlignLeft
          elide: Text.ElideRight
          // The sorted column says so, and which way round it is.
          text: modelData.title
            + (root.sortKey === modelData.key ? (root.sortDir < 0 ? " ↓" : " ↑") : "")
          font.family: Style.font.family
          font.pixelSize: Style.font.caption
          font.capitalization: Font.AllUppercase
          font.letterSpacing: 0.4
          color: root.sortKey === modelData.key
            ? (root.colors ? root.colors.ink : "black")
            : (root.colors ? root.colors.dim : "grey")
        }

        MouseArea {
          anchors.fill: parent
          enabled: modelData.sortable === true
          cursorShape: Qt.PointingHandCursor
          onClicked: root.sortRequested(modelData.key)
        }
      }
    }
  }

  Rectangle {
    anchors.top: header.bottom
    width: root.tableWidth
    height: 1
    color: root.colors ? root.colors.line : "transparent"
  }

  // -------------------------------------------------------------------- body
  ListView {
    id: view
    anchors.top: header.bottom
    anchors.topMargin: 1
    height: pan.height - header.height - 1
    width: root.tableWidth
    clip: true
    boundsBehavior: Flickable.StopAtBounds
    model: root.rows

    ScrollBar.vertical: ScrollBar {
      policy: view.contentHeight > view.height ? ScrollBar.AsNeeded : ScrollBar.AlwaysOff
    }

    delegate: Item {
      width: view.width
      height: root.rowHeight

      readonly property var rowData: modelData
      readonly property bool isSelected: root.idKey.length > 0
        && root.selectedKey.length > 0
        && String(modelData[root.idKey]) === root.selectedKey

      Rectangle {
        anchors.fill: parent
        color: isSelected || rowArea.containsMouse
          ? (root.colors ? root.colors.hover : "transparent")
          : "transparent"
      }

      // The selected app keeps a marker down its left edge, so scrolling the
      // rundown never loses which one the drilldown below is about.
      Rectangle {
        visible: isSelected
        width: Style.space(2)
        height: parent.height
        color: root.colors ? root.colors.ink : "transparent"
      }

      Row {
        anchors.fill: parent

        Repeater {
          model: root.columns

          Item {
            width: root.columnWidth(index)
            height: root.rowHeight

            readonly property var spec: root.cell(rowData, modelData.key)
            clip: true

            // The swatch and the pill are anchored to the edges and the text
            // fills what is left, so a long command name shortens instead of
            // running into the column beside it.
            Rectangle {
              id: swatch
              anchors.verticalCenter: parent.verticalCenter
              anchors.left: parent.left
              anchors.leftMargin: Style.space(6)
              visible: !!spec.swatch
              width: visible ? Style.space(9) : 0
              height: Style.space(9)
              radius: Style.space(2)
              color: spec.swatch ? spec.swatch : "transparent"
            }

            // "20 pids", "ongoing" — a count that qualifies the name beside it
            // rather than a column of its own.
            Rectangle {
              id: pill
              anchors.verticalCenter: parent.verticalCenter
              anchors.right: parent.right
              anchors.rightMargin: Style.space(6)
              visible: !!spec.pill
              radius: Style.space(99)
              border.width: 1
              border.color: root.colors ? root.colors.line : "transparent"
              color: "transparent"
              implicitWidth: visible ? pillText.implicitWidth + Style.space(12) : 0
              implicitHeight: pillText.implicitHeight + Style.space(2)
              Text {
                id: pillText
                anchors.centerIn: parent
                text: spec.pill ? spec.pill : ""
                font.family: Style.font.family
                font.pixelSize: Style.font.caption
                color: root.colors ? root.colors.dim : "grey"
              }
            }

            Text {
              anchors.fill: parent
              anchors.leftMargin: Style.space(6)
                + (swatch.visible ? swatch.width + Style.space(5) : 0)
              anchors.rightMargin: Style.space(6)
                + (pill.visible ? pill.implicitWidth + Style.space(5) : 0)
              verticalAlignment: Text.AlignVCenter
              horizontalAlignment: modelData.align === "right" ? Text.AlignRight : Text.AlignLeft
              elide: Text.ElideRight
              text: spec.text === undefined ? "" : spec.text
              font.family: Style.font.family
              font.pixelSize: Style.font.body
              font.weight: spec.bold ? Font.DemiBold : Font.Normal
              color: spec.muted ? (root.colors ? root.colors.dim : "grey")
                                : (root.colors ? root.colors.ink : "black")
            }
          }
        }
      }

      MouseArea {
        id: rowArea
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onClicked: root.activated(rowData)
      }
    }
  }

  }

  Text {
    anchors.top: parent.top
    anchors.topMargin: root.rowHeight + Style.space(8)
    anchors.left: parent.left
    anchors.leftMargin: Style.space(6)
    visible: !root.rows || root.rows.length === 0
    text: root.emptyText
    font.family: Style.font.family
    font.pixelSize: Style.font.body
    color: root.colors ? root.colors.dim : "grey"
  }
}
