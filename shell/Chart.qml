import QtQuick
import qs.Commons
import "Axis.js" as Axis
import "Format.js" as Fmt

// One line/area chart on the shared timeline.
//
// The plot is a Canvas and the crosshair is not. In the browser both were SVG
// and a pointer move rewrote an attribute; here a repaint would mean rasterising
// every series again for a line that moves with the mouse. So the Canvas draws
// what changes when the data changes — once per poll — and the crosshair is a
// one-pixel Rectangle over the top, which is also why it can follow the pointer
// on all eight charts at once without costing anything.
Item {
  id: root

  // The axis every chart in the window shares. Without it they would each
  // collapse their own gaps and stop agreeing on where a moment is.
  property var axis: null
  property var rows: []
  // {key, color, kind: "area"|"line", axis: "l"|"r", width, dash, opacity}
  property var defs: []
  property int bucket: 60

  // null means "fit the data"; a number pins the axis, which is how the
  // temperature chart keeps meaning the same thing on every range.
  property var maxL: null
  property var maxR: null
  property real floorL: 5
  property real floorR: 1
  property var fmtL: Fmt.pct
  property var fmtR: null

  property var incidents: []
  // Shaded like an incident, but saying something else: a throttled stretch is
  // the firmware holding the CPU back, not a busywatch incident.
  property var bands: []
  property var recordedFrom: null

  // Either a plain string, or [{text, color}] when the title doubles as the
  // legend — which is how the watts chart says which band is which.
  property string title: ""
  property var titleRuns: null

  property var colors: null
  property real hoverU: -1

  // Scene coordinates ride along so the one shared tooltip knows where the
  // pointer is without a second MouseArea over the whole window.
  signal probed(real t, var brk, real sx, real sy)

  implicitHeight: Style.space(96)

  readonly property int padL: Style.space(46)
  readonly property int padR: Style.space(42)
  readonly property int padT: Style.space(8)
  readonly property int padB: Style.space(16)
  readonly property real iw: Math.max(1, width - padL - padR)
  readonly property real ih: Math.max(1, height - padT - padB)

  function xu(u) { return padL + u / (axis ? axis.U : 1) * iw }
  function xt(t) { return xu(axis ? axis.u(t) : 0) }

  // Everything the paint pass reads has to be able to trigger it. Leaving one
  // out gives a chart that is right until exactly the moment only that thing
  // changes — an incident ending, or an axis being pinned — and then quietly
  // is not.
  onRowsChanged: canvas.requestPaint()
  onAxisChanged: canvas.requestPaint()
  onDefsChanged: canvas.requestPaint()
  onWidthChanged: canvas.requestPaint()
  onHeightChanged: canvas.requestPaint()
  onColorsChanged: canvas.requestPaint()
  onIncidentsChanged: canvas.requestPaint()
  onBandsChanged: canvas.requestPaint()
  onMaxLChanged: canvas.requestPaint()
  onMaxRChanged: canvas.requestPaint()
  onFmtLChanged: canvas.requestPaint()
  onFmtRChanged: canvas.requestPaint()
  onRecordedFromChanged: canvas.requestPaint()
  onBucketChanged: canvas.requestPaint()
  onTitleChanged: canvas.requestPaint()
  onTitleRunsChanged: canvas.requestPaint()

  Canvas {
    id: canvas
    anchors.fill: parent
    antialiasing: true

    onPaint: {
      var ctx = getContext("2d")
      ctx.reset()
      if (!root.axis || root.width <= 0 || root.height <= 0) return
      var A = root.axis
      var P = root.colors
      if (!P) return
      var rows = root.rows || []
      var defs = root.defs || []
      var padL = root.padL, padT = root.padT
      var iw = root.iw, ih = root.ih
      var caption = Style.font.caption
      var fam = Style.font.family

      // ------------------------------------------------------------- scales
      var ml = root.maxL, mr = root.maxR
      var i, j, d, r
      if (ml === null || ml === undefined) {
        ml = 0
        for (i = 0; i < defs.length; i++) {
          d = defs[i]
          if (d.axis === "r") continue
          for (j = 0; j < rows.length; j++) ml = Math.max(ml, rows[j][d.key] || 0)
        }
        ml = Axis.niceMax(ml, root.floorL)
      }
      if (mr === null || mr === undefined) {
        mr = 0
        for (i = 0; i < defs.length; i++) {
          d = defs[i]
          if (d.axis !== "r") continue
          for (j = 0; j < rows.length; j++) mr = Math.max(mr, rows[j][d.key] || 0)
        }
        mr = Axis.niceMax(mr, root.floorR)
      }
      var Y = function (v, ax) {
        return padT + ih - Math.min(1, (v || 0) / (ax === "r" ? mr : ml)) * ih
      }

      // --------------------------------------------------------- background
      // An empty left half is otherwise indistinguishable from a broken chart:
      // say plainly that busywatch was not recording yet.
      if (root.recordedFrom && root.recordedFrom > A.from) {
        var xr = Math.min(root.xt(root.recordedFrom), root.width - root.padR)
        ctx.fillStyle = Qt.rgba(P.dim.r, P.dim.g, P.dim.b, 0.06)
        ctx.fillRect(padL, padT, Math.max(0, xr - padL), ih)
        if (xr - padL > Style.space(90)) {
          ctx.fillStyle = P.dim
          ctx.font = caption + "px " + fam
          ctx.textAlign = "center"
          ctx.textBaseline = "middle"
          ctx.fillText("no history before " + Fmt.stamp(root.recordedFrom),
                       (padL + xr) / 2, padT + ih / 2)
        }
      }

      var shade = function (from, to, color) {
        var a = Math.max(A.from, from), b = Math.min(A.to, to)
        if (b < a) return
        var x0 = root.xt(a), x1 = root.xt(b)
        ctx.fillStyle = Qt.rgba(color.r, color.g, color.b, 0.13)
        ctx.fillRect(x0, padT, Math.max(1.5, x1 - x0), ih)
      }
      var bands = root.bands || []
      for (i = 0; i < bands.length; i++) shade(bands[i].from, bands[i].to, P.cpu)
      var incs = root.incidents || []
      for (i = 0; i < incs.length; i++) {
        shade(incs[i].started,
              incs[i].ended === null || incs[i].ended === undefined ? A.to : incs[i].ended,
              P.forKind(incs[i].kind))
      }

      // ---------------------------------------------------------------- axes
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
        ctx.fillText(root.fmtL(ml * i / 2), padL - Style.space(6), gy)
        if (root.fmtR) {
          ctx.textAlign = "left"
          ctx.fillText(root.fmtR(mr * i / 2), root.width - root.padR + Style.space(6), gy)
        }
      }
      var ticks = Axis.ticks(A)
      ctx.textAlign = "center"
      ctx.textBaseline = "alphabetic"
      ctx.fillStyle = P.dim
      for (i = 0; i < ticks.length; i++)
        ctx.fillText(ticks[i][1], root.xt(ticks[i][0]), root.height - Style.space(4))

      // -------------------------------------------------------------- series
      // A missing value is not zero. Power and clock only exist in rows written
      // since busywatch recorded them, so on an upgraded database they start
      // partway through the range — drawn as zeros they made a flat line along
      // the floor that claimed the battery had been empty all morning.
      var ts = []
      for (i = 0; i < rows.length; i++) ts.push(rows[i].t)
      var gap = Math.max(root.bucket || 60, Axis.medianGap(ts)) * 2.5

      for (i = 0; i < defs.length; i++) {
        d = defs[i]
        var segs = [], cur = [], prev = null
        for (j = 0; j < rows.length; j++) {
          r = rows[j]
          if (r[d.key] === null || r[d.key] === undefined) {
            if (cur.length) segs.push(cur)
            cur = []; prev = null
            continue
          }
          if (prev !== null && r.t - prev > gap) { segs.push(cur); cur = [] }
          cur.push(r); prev = r.t
        }
        if (cur.length) segs.push(cur)

        for (var s = 0; s < segs.length; s++) {
          var seg = segs[s]
          if (!seg.length) continue
          var pts = []
          for (j = 0; j < seg.length; j++)
            pts.push([root.xt(seg[j].t), Y(seg[j][d.key], d.axis)])
          // A single sample is a point, not a line: give it a pixel of width so
          // it is drawn at all.
          if (pts.length === 1) pts.push([pts[0][0] + 1, pts[0][1]])

          if (d.kind === "area") {
            var y0 = padT + ih
            ctx.beginPath()
            ctx.moveTo(pts[0][0], y0)
            for (j = 0; j < pts.length; j++) ctx.lineTo(pts[j][0], pts[j][1])
            ctx.lineTo(pts[pts.length - 1][0], y0)
            ctx.closePath()
            ctx.fillStyle = Qt.rgba(d.color.r, d.color.g, d.color.b,
                                    d.opacity === undefined ? 0.18 : d.opacity)
            ctx.fill()
          }
          ctx.beginPath()
          ctx.moveTo(pts[0][0], pts[0][1])
          for (j = 1; j < pts.length; j++) ctx.lineTo(pts[j][0], pts[j][1])
          ctx.strokeStyle = d.color
          ctx.lineWidth = d.width || 1.4
          ctx.lineJoin = "round"
          ctx.lineCap = "round"
          if (d.dash) ctx.setLineDash(d.dash)
          ctx.stroke()
          if (d.dash) ctx.setLineDash([])
        }
      }

      // -------------------------------------------------------------- breaks
      // Drawn last, over everything: a notch in the window colour, so an
      // incident that was still open when the machine went away cannot appear
      // to run straight through the missing hours.
      for (i = 0; i < A.breaks.length; i++) {
        var b = A.breaks[i]
        var bx0 = root.xu(b.u0), bx1 = root.xu(b.u1)
        var bw = Math.max(2, bx1 - bx0)
        var xm = bx0 + bw / 2, ym = padT + ih / 2
        ctx.fillStyle = P.ground
        ctx.fillRect(bx0, padT, bw, ih)
        ctx.strokeStyle = P.line
        ctx.lineWidth = 1
        ctx.strokeRect(Math.round(bx0) + 0.5, Math.round(padT) + 0.5,
                       Math.round(bw), Math.round(ih))
        if (bw >= Style.space(9) && ih >= Style.space(44)) {
          // Down the seam. Along the bottom axis this figure would read as one
          // more clock time.
          ctx.save()
          ctx.translate(xm, ym)
          ctx.rotate(-Math.PI / 2)
          ctx.fillStyle = P.dim
          ctx.font = Math.max(8, caption - 1) + "px " + fam
          ctx.textAlign = "center"
          ctx.textBaseline = "middle"
          ctx.fillText(Fmt.briefDur(b.to - b.from), 0, 0)
          ctx.restore()
        } else {
          // Too narrow to letter — a dotted rule still says the axis is cut.
          ctx.strokeStyle = Qt.rgba(P.dim.r, P.dim.g, P.dim.b, 0.5)
          ctx.setLineDash([2, 4])
          ctx.beginPath()
          ctx.moveTo(xm, padT)
          ctx.lineTo(xm, padT + ih)
          ctx.stroke()
          ctx.setLineDash([])
        }
      }

      // --------------------------------------------------------------- title
      var runs = root.titleRuns
      if (!runs && root.title) runs = [{ text: root.title, color: P.dim }]
      if (runs) {
        ctx.font = caption + "px " + fam
        ctx.textAlign = "left"
        ctx.textBaseline = "alphabetic"
        var tx = padL
        for (i = 0; i < runs.length; i++) {
          ctx.fillStyle = runs[i].color || P.dim
          ctx.fillText(runs[i].text, tx, padT + caption)
          tx += ctx.measureText(runs[i].text).width
        }
      }
    }
  }

  // The crosshair. Shared time in, so every chart in the window shows the same
  // moment; hidden when the pointer is off all of them.
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
