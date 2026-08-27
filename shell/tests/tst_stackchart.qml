import QtQuick
import QtTest
import ".."
import "../Axis.js" as Axis

// The stacked applications. Its model is what both the bands and the readout
// are built from, so a wrong total here is a wrong tooltip and a wrong chart at
// the same time — and they would agree with each other while doing it.
TestCase {
  id: testCase
  name: "StackChart"
  when: windowShown
  visible: true
  width: 600
  height: 300

  Scheme { id: scheme }

  property real probedT: -1
  property int probes: 0

  readonly property var axisObj: Axis.make(0, 600, [0, 120, 240, 360, 480, 600], 120, 0)

  StackChart {
    id: stack
    width: testCase.width
    height: 190
    colors: scheme
    axis: testCase.axisObj
    bucket: 120
    series: [
      { comm: "brave", points: [[0, 100], [120, 200], [240, 300]] },
      { comm: "claude", points: [[0, 50], [240, 25]] }
    ]
    onProbed: function (t, brk, sx, sy) { testCase.probedT = t; testCase.probes++ }
  }

  function test_the_model_unions_every_series_timestamp() {
    compare(stack.model.times.length, 3, "three distinct moments across both series")
    compare(stack.model.times[0], 0)
    compare(stack.model.times[2], 240)
  }

  // A series that has no point at a moment contributes nothing there — it must
  // not carry its previous value forward into the total.
  function test_a_missing_point_counts_as_nothing_not_as_before() {
    compare(stack.valueAt(1, 120), 0, "claude has no point at 120")
    compare(stack.model.totals[1], 200, "so the total there is brave alone")
    compare(stack.model.totals[0], 150, "and both where both are present")
    compare(stack.model.totals[2], 325)
  }

  function test_the_nearest_recorded_moment_is_what_the_readout_speaks_about() {
    compare(stack.nearestTime(0), 0)
    compare(stack.nearestTime(130), 120, "a pointer between buckets snaps to the nearer one")
    compare(stack.nearestTime(1000), 240, "past the end it is the last one recorded")
  }

  function test_hovering_reports_a_moment() {
    mouseMove(stack, stack.width / 2, stack.height / 2)
    tryVerify(function () { return testCase.probes > 0 }, 1000,
              "the stacked chart probes like every other chart")
    verify(probedT >= 0 && probedT <= 600)
  }

  // Selecting an app dims the others rather than hiding them: the total is the
  // point of a stack, and removing a band would move every band above it.
  function test_selecting_an_app_keeps_every_band() {
    stack.selected = "brave"
    wait(50)
    compare(stack.model.times.length, 3, "selection changes nothing about the model")
    compare(stack.model.totals[2], 325, "and nothing about the totals")
    stack.selected = ""
  }
}
