// The timeline, ported from the web UI.
//
// Time is not always drawn to scale. A machine that was asleep, off, or not
// running busywatch leaves an hours-wide hole in every chart, and over a week
// those holes can be most of the width — so a long one collapses to a narrow
// marked break and the recorded time gets the pixels instead. Every chart in
// the window shares one axis object: they sit in different sections but move
// one crosshair between them, so they have to agree on where a moment is.
.pragma library

// Finer in the middle than plain doubling: when a break takes a label away,
// having a 3h step to fall back on beats jumping from 2h to 6h and leaving
// three labels on a day.
var TICK_STEPS = [60, 300, 600, 900, 1800, 3600, 7200, 10800, 14400, 21600,
                  43200, 86400, 172800, 604800, 2419200]

var BRK_W = .009    // one break, as a share of the time actually recorded
var BRK_CAP = .18   // ...but all of them together never take more than this

function pad(n) { return String(n).padStart(2, "0") }

function clockLabel(t) {
  var d = new Date(t * 1000)
  return pad(d.getHours()) + ":" + pad(d.getMinutes())
}

// Typical spacing of a timestamp list; used to tell a real recording gap
// (busywatch was not running) from ordinary spacing.
function medianGap(ts) {
  if (ts.length < 2) return Infinity
  var d = []
  for (var i = 1; i < ts.length; i++) d.push(ts[i] - ts[i - 1])
  d.sort(function (a, b) { return a - b })
  return d[Math.floor(d.length / 2)] || Infinity
}

function niceMax(v, floor) {
  v = Math.max(v, floor)
  var mag = Math.pow(10, Math.floor(Math.log(v) / Math.LN10))
  var ms = [1, 1.5, 2, 2.5, 3, 4, 5, 7.5, 10]
  for (var i = 0; i < ms.length; i++) if (v <= ms[i] * mag) return ms[i] * mag
  return 10 * mag
}

function tickRun(A, step) {
  var out = []
  var tz = new Date().getTimezoneOffset() * 60
  var last = -Infinity
  for (var t = Math.ceil((A.from - tz) / step) * step + tz; t <= A.to; t += step) {
    // A label for a time nothing was recorded at points at a break, not at a
    // place on the chart.
    if (A.inBreak(t)) continue
    // Two ticks either side of a break are a few pixels apart on the page even
    // when they are hours apart in truth: keep the first, drop the one that
    // would sit on top of it.
    var u = A.u(t)
    if (u - last < A.U * .045) continue
    last = u
    // Over several days the same "12:00" on every axis is useless — date the
    // midnight ticks so a day can be told from its neighbour.
    var d = new Date(t * 1000)
    var midnight = d.getHours() === 0 && d.getMinutes() === 0
    out.push([t, step >= 86400 || midnight
      ? (d.getMonth() + 1) + "/" + d.getDate()
      : clockLabel(t)])
  }
  return out
}

// The finest spacing that still fits: counted after the collapsed stretches
// have had their labels taken away, so an axis that skips every night does not
// end up with two lonely dates on it.
function ticks(A) {
  var out = []
  for (var i = 0; i < TICK_STEPS.length; i++) {
    out = tickRun(A, TICK_STEPS[i])
    if (out.length <= 8) return out
  }
  return out
}

// `times` is the record of when busywatch was actually awake; `recordedFrom`
// is the first sample ever, which is a different thing from a hole and gets
// its own label on the chart.
function make(from, to, times, bucket, recordedFrom) {
  var span = Math.max(1, to - from)
  var step = bucket || 60
  // A hole has to be wider than the break that replaces it, or "collapsing" it
  // would only make it bigger.
  var floor = Math.max(step * 2.5, span * BRK_W * 2)
  var holes = []
  var prev = null
  var list = times || []
  for (var i = 0; i < list.length; i++) {
    var t = list[i]
    if (prev !== null) {
      // The sample before a hole still covers its own bucket, so the missing
      // time starts where that bucket ends — and it is the missing time, not
      // the distance between the two samples, that has to clear the floor.
      var start = Math.min(prev + step, t)
      if (t - start > floor) holes.push([start, t])
    }
    prev = t
  }
  // A window that opens on a machine which was already off starts with a hole
  // too — but only back as far as the first sample ever, because everything
  // before that is "no history", which says something else.
  if (list.length) {
    var s0 = Math.max(from, recordedFrom || from)
    if (list[0] - s0 > floor) holes.unshift([s0, list[0]])
  }
  var dead = 0
  for (var h = 0; h < holes.length; h++) dead += holes[h][1] - holes[h][0]
  var live = Math.max(1, span - dead)
  var bw = live * (holes.length ? Math.min(BRK_W, BRK_CAP / holes.length) : BRK_W)
  var breaks = []
  var u = 0, cur = from
  for (var j = 0; j < holes.length; j++) {
    u += holes[j][0] - cur
    breaks.push({ from: holes[j][0], to: holes[j][1], u0: u, u1: u + bw })
    u += bw
    cur = holes[j][1]
  }
  var U = Math.max(1, u + (to - cur))

  var A = { from: from, to: to, U: U, live: live, breaks: breaks, dead: dead }

  // Time in drawn units.
  A.u = function (t) {
    if (t <= from) return 0
    if (t >= to) return U
    var acc = 0, c = from
    for (var k = 0; k < breaks.length; k++) {
      var b = breaks[k]
      if (t < b.from) return acc + (t - c)
      acc += b.from - c
      if (t <= b.to) return acc + (t - b.from) / Math.max(1, b.to - b.from) * bw
      acc += bw
      c = b.to
    }
    return acc + (t - c)
  }

  // The way back, and it says so when the point asked about is one of the
  // collapsed stretches rather than a moment that was recorded.
  A.at = function (uu) {
    uu = Math.min(U, Math.max(0, uu))
    var acc = 0, c = from
    for (var k = 0; k < breaks.length; k++) {
      var b = breaks[k]
      var run = b.from - c
      if (uu <= acc + run) return { t: c + (uu - acc) }
      acc += run
      if (uu <= acc + bw) return { t: b.from, brk: b }
      acc += bw
      c = b.to
    }
    return { t: Math.min(to, c + (uu - acc)) }
  }

  A.inBreak = function (t) {
    for (var k = 0; k < breaks.length; k++)
      if (t > breaks[k].from && t < breaks[k].to) return true
    return false
  }

  return A
}
