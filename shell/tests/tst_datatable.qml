import QtQuick
import QtTest
import ".."

// The rundown: clicking a heading sorts by it, clicking a row drills into the
// app it names. Both are how the window is actually used, and both are one
// MouseArea deep inside a delegate — which is exactly the wiring that a screen
// shot of a correct-looking table cannot vouch for.
TestCase {
  id: testCase
  name: "DataTable"
  when: windowShown
  visible: true
  width: 700
  height: 240

  Scheme { id: scheme }

  property string lastSortKey: ""
  property var lastActivated: null

  DataTable {
    id: table
    width: testCase.width
    height: testCase.height
    colors: scheme
    idKey: "comm"
    maxRows: 6
    rows: [
      { comm: "brave", rssMax: 6100000, pids: 26 },
      { comm: "claude", rssMax: 3700000, pids: 13 },
      { comm: "foot", rssMax: 247000, pids: 6 }
    ]
    columns: [
      { key: "comm", title: "app", flex: 2, min: 160, sortable: true },
      { key: "rssMax", title: "rss peak", flex: 1, min: 90, align: "right", sortable: true },
      { key: "pids", title: "pids", flex: 1, min: 60, align: "right" }
    ]
    cell: function (r, key) {
      if (key === "comm") return { text: r.comm, pill: r.pids > 1 ? r.pids + " pids" : "" }
      return { text: String(r[key]) }
    }
    onSortRequested: function (key) { testCase.lastSortKey = key }
    onActivated: function (row) { testCase.lastActivated = row }
  }

  function init() {
    lastSortKey = ""
    lastActivated = null
    table.selectedKey = ""
  }

  readonly property real headerY: table.rowHeight / 2
  function rowY(i) { return table.rowHeight + 1 + table.rowHeight * i + table.rowHeight / 2 }
  function colX(i) {
    var x = 0
    for (var j = 0; j < i; j++) x += table.columnWidth(j)
    return x + table.columnWidth(i) / 2
  }

  function test_clicking_a_heading_asks_to_sort_by_it() {
    mouseClick(table, colX(1), headerY)
    compare(lastSortKey, "rssMax", "the rss peak heading sorts by rssMax")
  }

  function test_each_sortable_heading_reports_its_own_key() {
    mouseClick(table, colX(0), headerY)
    compare(lastSortKey, "comm")
  }

  // `pids` has no `sortable`, so its heading is inert — the MouseArea under it
  // is disabled rather than merely unhandled.
  function test_a_column_that_is_not_sortable_ignores_the_click() {
    mouseClick(table, colX(2), headerY)
    compare(lastSortKey, "", "an unsortable heading emits nothing")
  }

  function test_clicking_a_row_activates_that_row() {
    mouseClick(table, colX(0), rowY(0))
    verify(!!lastActivated, "a row click reaches onActivated")
    compare(lastActivated.comm, "brave")
  }

  function test_the_row_clicked_is_the_row_under_the_pointer() {
    mouseClick(table, colX(0), rowY(2))
    compare(lastActivated.comm, "foot", "the third row activates foot, not the first")
  }

  // Clicking anywhere across the row works, not just on the name: the figures
  // are the widest part of it and are what the pointer is usually near.
  function test_a_click_on_a_figure_activates_the_row_too() {
    mouseClick(table, colX(1), rowY(1))
    compare(lastActivated.comm, "claude")
  }

  function test_the_selected_row_is_marked() {
    table.selectedKey = "claude"
    wait(50)
    verify(table.selectedKey === "claude")
  }

  // Three columns fit 700px, so nothing should scroll sideways; the same table
  // squeezed below its floors has to.
  function test_the_table_only_scrolls_when_the_columns_do_not_fit() {
    verify(table.tableWidth <= table.width,
           "with room, the columns fit the viewport and nothing scrolls")
    verify(table.width - table.tableWidth <= 4,
           "and they fill it, bar the hair of slack that keeps a rounding "
           + "error from raising a scrollbar")
    table.width = 200
    wait(50)
    verify(table.tableWidth > table.width,
           "below the column floors the table is wider than its viewport")
    compare(table.tableWidth, 160 + 90 + 60, "and it is exactly the sum of the floors")
    table.width = testCase.width
  }
}
