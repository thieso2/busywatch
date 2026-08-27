import QtQuick
import QtTest
import "../Axis.js" as Axis

// The timeline. A machine that was asleep leaves an hours-wide hole in every
// chart, and over a week those holes can be most of the width — so a long one
// collapses to a narrow marked break and the recorded time gets the pixels.
//
// This is the part of the port with the least margin for error: it decides
// where every point on every chart is drawn, and being subtly wrong looks
// exactly like being right.
TestCase {
  name: "Axis"

  readonly property int hour: 3600

  // A continuous hour of one-minute samples.
  function minutely(from, to) {
    var t = []
    for (var x = from; x <= to; x += 60) t.push(x)
    return t
  }

  function test_a_continuous_range_has_no_breaks() {
    var A = Axis.make(0, hour, minutely(0, hour), 60, 0)
    compare(A.breaks.length, 0, "nothing was missing, so nothing is collapsed")
    compare(A.dead, 0)
    // With no breaks, drawn units are just seconds and time is to scale.
    compare(A.u(0), 0)
    compare(A.u(hour), A.U)
    fuzzyCompare(A.u(hour / 2), A.U / 2, 1)
  }

  function test_a_night_asleep_collapses_to_a_seam() {
    // Two hours recorded, eight hours away, two hours recorded.
    var t = minutely(0, 2 * hour).concat(minutely(10 * hour, 12 * hour))
    var A = Axis.make(0, 12 * hour, t, 60, 0)
    compare(A.breaks.length, 1, "the one hole becomes one break")
    var b = A.breaks[0]
    compare(b.from, 2 * hour + 60, "the break starts where the last sample's bucket ends")
    compare(b.to, 10 * hour)
    // Eight hours of absence must not take eight hours of width.
    var seam = b.u1 - b.u0
    verify(seam < A.U * 0.05, "the seam is a sliver, not most of the chart")
    verify(A.dead > 7.9 * hour, "and the axis reports how much it swallowed")
  }

  function test_recorded_time_keeps_the_width_the_gap_gave_up() {
    var t = minutely(0, 2 * hour).concat(minutely(10 * hour, 12 * hour))
    var A = Axis.make(0, 12 * hour, t, 60, 0)
    // The first recorded stretch is 2h of 4h recorded, so it should occupy
    // roughly half the drawn width rather than a sixth of it.
    var share = A.u(2 * hour) / A.U
    verify(share > 0.4 && share < 0.55,
           "two recorded hours out of four take about half the width, not 1/6")
  }

  // The rule that stops the cure being worse than the disease.
  function test_a_gap_narrower_than_its_own_seam_is_left_alone() {
    // A three-minute suspend inside an hour: collapsing it would make it wider.
    var t = minutely(0, 20 * 60).concat(minutely(23 * 60, hour))
    var A = Axis.make(0, hour, t, 60, 0)
    compare(A.breaks.length, 0, "a short suspend stays an ordinary gap in the line")
  }

  function test_several_nights_share_a_capped_budget() {
    var t = []
    for (var d = 0; d < 4; d++) {
      var base = d * 24 * hour
      t = t.concat(minutely(base, base + 6 * hour))
    }
    var A = Axis.make(0, 4 * 24 * hour, t, 60, 0)
    compare(A.breaks.length, 3, "three nights between four days")
    var total = 0
    for (var i = 0; i < A.breaks.length; i++)
      total += A.breaks[i].u1 - A.breaks[i].u0
    verify(total / A.U < 0.2,
           "all the seams together never take more than a fifth of the chart")
  }

  // u() and at() are the two directions of the same map; the crosshair depends
  // on them agreeing, because it converts a pixel back into a moment.
  function test_position_and_time_round_trip() {
    var t = minutely(0, 2 * hour).concat(minutely(10 * hour, 12 * hour))
    var A = Axis.make(0, 12 * hour, t, 60, 0)
    var probes = [0, 600, hour, 2 * hour, 10 * hour + 60, 11 * hour, 12 * hour]
    for (var i = 0; i < probes.length; i++) {
      var back = A.at(A.u(probes[i]))
      fuzzyCompare(back.t, probes[i], 2,
                   "a time converted to a position and back is the same time")
    }
  }

  function test_a_point_inside_a_collapsed_stretch_says_so() {
    var t = minutely(0, 2 * hour).concat(minutely(10 * hour, 12 * hour))
    var A = Axis.make(0, 12 * hour, t, 60, 0)
    var b = A.breaks[0]
    var mid = A.at((b.u0 + b.u1) / 2)
    verify(!!mid.brk, "landing on a seam reports the break, not a nearby sample")
    compare(mid.brk.from, b.from)
    verify(A.inBreak(5 * hour), "a moment nothing was recorded at is inside the break")
    verify(!A.inBreak(hour), "a moment that was recorded is not")
  }

  // A window opening on a machine that was already off starts with a hole too,
  // but only back as far as the first sample ever.
  function test_history_that_starts_late_is_not_a_gap_before_it_began() {
    var t = minutely(10 * hour, 12 * hour)
    var A = Axis.make(0, 12 * hour, t, 60, 8 * hour)
    compare(A.breaks.length, 1)
    compare(A.breaks[0].from, 8 * hour,
            "the break begins at the first sample ever, not at the window edge")
  }

  function test_ticks_avoid_the_collapsed_stretches() {
    var t = minutely(0, 2 * hour).concat(minutely(10 * hour, 12 * hour))
    var A = Axis.make(0, 12 * hour, t, 60, 0)
    var ticks = Axis.ticks(A)
    verify(ticks.length > 0, "an axis gets labels")
    verify(ticks.length <= 8, "and never more than fit")
    for (var i = 0; i < ticks.length; i++)
      verify(!A.inBreak(ticks[i][0]),
             "no label points into a stretch that was never recorded")
  }

  function test_median_gap_and_nice_max() {
    compare(Axis.medianGap([0, 60, 120, 180]), 60)
    compare(Axis.medianGap([0]), Infinity, "one sample has no spacing")
    compare(Axis.niceMax(0, 5), 5, "the floor holds an empty chart open")
    compare(Axis.niceMax(87, 5), 100)
    compare(Axis.niceMax(2.2, 1), 2.5)
  }
}
