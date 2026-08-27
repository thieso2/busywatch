import QtQuick
import QtTest
import ".."
import "../Axis.js" as Axis

// Hovering a chart. The crosshair and the readout both hang off one signal, and
// the pointer maths behind it has to survive the axis not being to scale.
TestCase {
  id: testCase
  name: "Chart"
  when: windowShown
  visible: true
  width: 600
  height: 300

  Scheme { id: scheme }

  readonly property int hour: 3600
  property real probedT: -1
  property var probedBrk: null
  property int probes: 0

  function minutely(from, to) {
    var t = []
    for (var x = from; x <= to; x += 60) t.push(x)
    return t
  }
  function rowsFor(times) {
    var r = []
    for (var i = 0; i < times.length; i++) r.push({ t: times[i], v: i % 10 })
    return r
  }

  readonly property var times: minutely(0, 2 * hour).concat(minutely(10 * hour, 12 * hour))
  readonly property var axisObj: Axis.make(0, 12 * hour, times, 60, 0)

  Chart {
    id: chart
    width: testCase.width
    height: 160
    colors: scheme
    axis: testCase.axisObj
    rows: testCase.rowsFor(testCase.times)
    bucket: 60
    defs: [{ key: "v", color: scheme.cpu, kind: "area" }]
    onProbed: function (t, brk, sx, sy) {
      testCase.probedT = t
      testCase.probedBrk = brk
      testCase.probes++
    }
  }

  function init() { probedT = -1; probedBrk = null; probes = 0 }

  function test_hovering_the_plot_reports_a_moment() {
    mouseMove(chart, chart.width / 2, chart.height / 2)
    tryVerify(function () { return testCase.probes > 0 }, 1000,
              "moving the pointer over a chart emits probed()")
    verify(probedT >= 0 && probedT <= 12 * testCase.hour,
           "and the moment it names is inside the range")
  }

  // The left edge of the plot is `from`, the right edge is `to`; anything else
  // means the padding is being counted twice or not at all.
  function test_the_ends_of_the_plot_are_the_ends_of_the_range() {
    mouseMove(chart, chart.padL + 1, chart.height / 2)
    tryVerify(function () { return testCase.probes > 0 }, 1000)
    verify(probedT < testCase.hour, "just inside the left edge is near the start")

    probes = 0
    mouseMove(chart, chart.width - chart.padR - 1, chart.height / 2)
    tryVerify(function () { return testCase.probes > 0 }, 1000)
    verify(probedT > 11 * testCase.hour, "just inside the right edge is near the end")
  }

  // The point of sharing one axis: a position is a moment, and the collapsed
  // stretches have to be reported as themselves rather than snapped to an edge.
  function test_hovering_a_seam_reports_the_break() {
    var b = testCase.axisObj.breaks[0]
    var xu = chart.xu((b.u0 + b.u1) / 2)
    mouseMove(chart, xu, chart.height / 2)
    tryVerify(function () { return testCase.probes > 0 }, 1000)
    verify(!!probedBrk, "the pointer over a seam reports the break it stands for")
    compare(probedBrk.from, b.from)
  }

  function test_leaving_the_chart_clears_the_hover() {
    // Somewhere the pointer is not already sitting: a move to the position it
    // already holds produces no event, and the test would be reading the
    // previous one's state.
    mouseMove(chart, chart.width / 3, chart.height / 3)
    tryVerify(function () { return testCase.probes > 0 }, 1000,
              "the pointer is over the plot to begin with")
    probes = 0
    // Into the window but off the chart, which is what a real pointer does.
    mouseMove(testCase, testCase.width / 2, chart.height + 60)
    tryVerify(function () { return testCase.probedT === -1 }, 1000,
              "the crosshair is taken down when the pointer leaves")
  }

  // The crosshair is a Rectangle over the Canvas, not part of it — this is what
  // lets one hovered moment show on eight charts without eight repaints.
  function test_the_crosshair_follows_the_shared_hover_position() {
    chart.hoverU = testCase.axisObj.u(testCase.hour)
    wait(50)
    var expected = chart.xu(chart.hoverU)
    var line = null
    for (var i = 0; i < chart.children.length; i++)
      if (chart.children[i].width === 1) line = chart.children[i]
    verify(!!line, "the chart carries a one-pixel crosshair")
    verify(line.visible, "which shows once a moment is hovered")
    fuzzyCompare(line.x, expected, 1)
  }
}
