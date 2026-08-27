import QtQuick
import qs.Commons
import "Axis.js" as Axis
import "Format.js" as Fmt

// The top applications over time, stacked, on the same shared timeline.
//
// Bottom-up cumulative bands, split wherever the history has a hole, so a
// machine that was asleep does not get a band drawn straight across the hours
// it was away. Selecting an app dims the others rather than hiding them: the
// total is the point of a stack, and removing a band would move every band
// above it.
Item {
  id: root

  property var axis: null
  // [{comm, points: [[t, v], ...]}]
  property var series: []
  property int bucket: 60
  property var fmt: Fmt.bytes
  property var recordedFrom: null
  property string selected: ""
  property var colors: null
  property real hoverU: -1

  // Scene coordinates ride along so the one shared tooltip knows where the
  // pointer is without a second MouseArea over the whole window.
  signal probed(real t, var brk, real sx, real sy)

  implicitHeight: Style.space(190)

  readonly property int padL: Style.space(46)
  readonly property int padR: Style.space(42)
  readonly property int padT: Style.space(8)
  readonly property int padB: Style.space(16)
  readonly property real iw: Math.max(1, width - padL - padR)
  readonly property real ih: Math.max(1, height - padT - padB)

  function xu(u) { return padL + u / (axis ? axis.U : 1) * iw }
  function xt(t) { return xu(axis ? axis.u(t) : 0) }

  // Times, per-series lookup and totals, built once and read by both the paint
  // pass and the tooltip. Rebuilding this inside onPaint would do it again on
  // every resize frame.
  readonly property var model: {
    var out = { times: [], maps: [], totals: [] }
    var list = series || []
    var seen = ({})
    var i, j
    for (i = 0; i < list.length; i++) {
      var m = ({})
      var pts = list[i].points || []
      for (j = 0; j < pts.length; j++) {
        m[pts[j][0]] = pts[j][1]
        seen[pts[j][0]] = true
      }
      out.maps.push(m)
    }
    for (var k in seen) out.times.push(Number(k))
    out.times.sort(function (a, b) { return a - b })
    for (i = 0; i < out.times.length; i++) {
      var tot = 0
      for (j = 0; j < out.maps.length; j++) tot += out.maps[j][out.times[i]] || 0
      out.totals.push(tot)
    }
    return out
  }

  function valueAt(seriesIndex, t) {
    var m = model.maps[seriesIndex]
    return m ? (m[t] || 0) : 0
  }

  // The recorded moment nearest a probed time — which is what the tooltip has
  // to speak about, since the pointer lands between buckets far more often
  // than on one.
  function nearestTime(t) {
    var best = null, bd = Infinity
    for (var i = 0; i < model.times.length; i++) {
      var d = Math.abs(model.times[i] - t)
      if (d < bd) { bd = d; best = model.times[i] }
    }
    return best
  }

  onModelChanged: canvas.requestPaint()
  onAxisChanged: canvas.requestPaint()
  onSelectedChanged: canvas.requestPaint()
  onWidthChanged: canvas.requestPaint()
  onHeightChanged: canvas.requestPaint()
  onColorsChanged: canvas.requestPaint()
  onFmtChanged: canvas.requestPaint()
  onRecordedFromChanged: canvas.requestPaint()
  onBucketChanged: canvas.requestPaint()

  Canvas {
    id: canvas
    anchors.fill: parent
    antialiasing: true

    onPaint: {
      var ctx = getContext("2d")
      ctx.reset()
      if (!root.axis || !root.colors || root.width <= 0) return
      var A = root.axis, P = root.colors
      var M = root.model
      var padL = root.padL, padT = root.padT, iw = root.iw, ih = root.ih
      var caption = Style.font.caption, fam = Style.font.family
      var i, j, t

      var max = Axis.niceMax(Math.max.apply(null, [1].concat(M.totals)), 1)
      var Y = function (v) { return padT + ih - Math.min(1, v / max) * ih }

      ctx.font = caption + "px " + fam
      ctx.textBaseline = "middle"
      for (i = 0; i <= 2; i++) {
        var gy = padT + ih - ih * i / 2
        ctx.strokeStyle = P.grid
        ctx.lineWidth = 1
        ctx.beginPath()
        ctx.moveTo(padL, Math.round(gy) + 0.5)
        ctx.lineTo(root.width - root.padR, Math.round(gy) + 0.5)
        ctx.stroke()
        ctx.fillStyle = P.dim
        ctx.textAlign = "right"
        ctx.fillText(root.fmt(max * i / 2), padL - Style.space(6), gy)
      }

      if (root.recordedFrom && root.recordedFrom > A.from) {
        var xr = Math.min(root.xt(root.recordedFrom), root.width - root.padR)
        ctx.fillStyle = Qt.rgba(P.dim.r, P.dim.g, P.dim.b, 0.06)
        ctx.fillRect(padL, padT, Math.max(0, xr - padL), ih)
      }

      var ticks = Axis.ticks(A)
      ctx.textAlign = "center"
      ctx.textBaseline = "alphabetic"
      ctx.fillStyle = P.dim
      for (i = 0; i < ticks.length; i++)
        ctx.fillText(ticks[i][1], root.xt(ticks[i][0]), root.height - Style.space(4))

      // ------------------------------------------------------- stacked bands
      var gap = Math.max(root.bucket || 60, Axis.medianGap(M.times)) * 2.5
      var base = ({})
      for (i = 0; i < M.times.length; i++) base[M.times[i]] = 0

      for (i = 0; i < (root.series || []).length; i++) {
        var ser = root.series[i]
        var col = P.forIndex(i)
        var dimmed = root.selected && root.selected !== ser.comm
        var seg = [], prev = null

        var flush = function () {
          if (seg.length) {
            ctx.beginPath()
            for (var k = 0; k < seg.length; k++) {
              var x = root.xt(seg[k])
              var y = Y(base[seg[k]] + root.valueAt(i, seg[k]))
              if (k === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y)
            }
            for (k = seg.length - 1; k >= 0; k--)
              ctx.lineTo(root.xt(seg[k]), Y(base[seg[k]]))
            ctx.closePath()
            ctx.fillStyle = Qt.rgba(col.r, col.g, col.b, dimmed ? 0.14 : 0.72)
            ctx.fill()
            ctx.strokeStyle = Qt.rgba(col.r, col.g, col.b, dimmed ? 0.2 : 1)
            ctx.lineWidth = 0.6
            ctx.stroke()
          }
          seg = []
        }

        for (j = 0; j < M.times.length; j++) {
          t = M.times[j]
          if (prev !== null && t - prev > gap) flush()
          seg.push(t); prev = t
        }
        flush()
        for (j = 0; j < M.times.length; j++)
          base[M.times[j]] += root.valueAt(i, M.times[j])
      }

      for (i = 0; i < A.breaks.length; i++) {
        var b = A.breaks[i]
        var bx0 = root.xu(b.u0), bx1 = root.xu(b.u1)
        var bw = Math.max(2, bx1 - bx0)
        var xm = bx0 + bw / 2
        ctx.fillStyle = P.ground
        ctx.fillRect(bx0, padT, bw, ih)
        ctx.strokeStyle = P.line
        ctx.lineWidth = 1
        ctx.strokeRect(Math.round(bx0) + 0.5, Math.round(padT) + 0.5,
                       Math.round(bw), Math.round(ih))
        if (bw >= Style.space(9) && ih >= Style.space(44)) {
          ctx.save()
          ctx.translate(xm, padT + ih / 2)
          ctx.rotate(-Math.PI / 2)
          ctx.fillStyle = P.dim
          ctx.font = Math.max(8, caption - 1) + "px " + fam
          ctx.textAlign = "center"
          ctx.textBaseline = "middle"
          ctx.fillText(Fmt.briefDur(b.to - b.from), 0, 0)
          ctx.restore()
        } else {
          ctx.strokeStyle = Qt.rgba(P.dim.r, P.dim.g, P.dim.b, 0.5)
          ctx.setLineDash([2, 4])
          ctx.beginPath()
          ctx.moveTo(xm, padT)
          ctx.lineTo(xm, padT + ih)
          ctx.stroke()
          ctx.setLineDash([])
        }
      }
    }
  }

  Rectangle {
    visible: root.hoverU >= 0 && root.axis
    x: root.hoverU >= 0 ? root.xu(root.hoverU) : 0
    y: root.padT
    width: 1
    height: root.ih
    color: root.colors ? Qt.rgba(root.colors.dim.r, root.colors.dim.g,
                                  root.colors.dim.b, 0.75) : "transparent"
  }

  MouseArea {
    anchors.fill: parent
    hoverEnabled: true
    acceptedButtons: Qt.NoButton
    onPositionChanged: function (mouse) {
      if (!root.axis) return
      var u = (mouse.x - root.padL) / root.iw * root.axis.U
      var at = root.axis.at(u)
      var scene = mapToItem(null, mouse.x, mouse.y)
      root.probed(at.t, at.brk || null, scene.x, scene.y)
    }
    onExited: root.probed(-1, null, 0, 0)
  }
}
