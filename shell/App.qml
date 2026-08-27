import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import Quickshell
import qs.Commons
import qs.Ui
import "Axis.js" as Axis
import "Format.js" as Fmt

// The history window: the browser page, drawn by the shell.
//
// The shell loads this entry point when the plugin is summoned and calls
// open()/close() on it; the FloatingWindow follows `opened`. Everything it
// draws comes from the service, which keeps polling whether or not this is on
// screen — so the range you were on, the app you had drilled into, and the
// data behind them all survive a close.
//
// The page kept its view in the URL hash. There is no URL here, so open() takes
// the same three values as a payload: a toast click can summon the window
// straight onto the resource that raised it.
Item {
  id: root

  property var shell: null
  property var manifest: null
  property var service: null
  property bool opened: false
  property bool closingFromHost: false

  readonly property string pluginId: manifest && manifest.id
    ? String(manifest.id) : "busywatch"

  Scheme { id: pal }

  // ------------------------------------------------------------- host contract
  function open(payloadJson) {
    if (payloadJson && String(payloadJson).length) {
      try {
        var p = JSON.parse(payloadJson)
        if (p && service) {
          if (p.span) service.span = Number(p.span)
          if (p.metric) service.metric = String(p.metric)
          if (p.proc !== undefined) service.select(String(p.proc || ""))
        }
      } catch (e) { /* an unparseable payload still opens the window */ }
    }
    opened = true
    if (service) service.loadOverview()
  }

  function close() {
    closingFromHost = true
    opened = false
    closingFromHost = false
  }

  // Closing from the window's own decoration has to go back through the host,
  // or the shell keeps believing the panel is open and the next toggle closes
  // nothing.
  function requestClose() {
    if (shell && typeof shell.hide === "function") shell.hide(pluginId)
    else opened = false
  }

  onOpenedChanged: if (service) service.windowOpen = opened

  // ----------------------------------------------------------- derived data
  readonly property var overview: service ? service.overview : null
  readonly property var live: service ? service.live : null
  readonly property bool online: service ? service.online : false

  readonly property var ranges: [["15m", 900], ["1h", 3600], ["6h", 21600],
                                 ["24h", 86400], ["3d", 259200], ["7d", 604800],
                                 ["30d", 2592000]]
  readonly property var metrics: [["mem", "Memory"], ["cpu", "CPU"], ["io", "IO"]]

  // Built once, from the system series — the record of when busywatch was
  // actually awake — and handed to every chart in the window, so they all skip
  // the same stretches and the crosshair still means one moment.
  readonly property var axis: {
    if (!overview) return null
    var times = []
    var s = overview.series || []
    for (var i = 0; i < s.length; i++) times.push(s[i].t)
    return Axis.make(overview.from, overview.to, times, overview.bucket,
                     overview.span ? overview.span.first : null)
  }

  // The series rows with the three derived columns the charts need. Watts are
  // recorded signed — negative while the battery drains, positive while it
  // fills — and splitting the sign into two positive series lets one watt axis
  // carry both directions without a charging stretch drawing a discharge line
  // flat along zero.
  readonly property var rows: {
    if (!overview || !overview.series) return []
    var out = []
    for (var i = 0; i < overview.series.length; i++) {
      var r = overview.series[i]
      var c = {}
      for (var k in r) c[k] = r[k]
      c.wOut = r.batW < 0 ? -r.batW / 1e6 : null
      c.wIn = r.batW > 0 ? r.batW / 1e6 : null
      c.tempC = r.temp === null || r.temp === undefined ? null : r.temp / 1000
      c.tempMaxC = r.tempMax === null || r.tempMax === undefined ? null : r.tempMax / 1000
      out.push(c)
    }
    return out
  }

  // Older rows carry no power, clock or heat at all; a chart appears only when
  // this range actually has some, so an upgraded install does not get an empty
  // panel for every range that predates the columns.
  function seen(test) {
    for (var i = 0; i < rows.length; i++) if (test(rows[i])) return true
    return false
  }
  readonly property bool swapSeen: seen(function (r) { return r.swap > 0 })
  readonly property bool clockSeen: seen(function (r) { return r.freq !== null || r.bat !== null })
  readonly property bool wattsSeen: seen(function (r) { return r.wOut !== null || r.wIn !== null })
  readonly property bool heatSeen: seen(function (r) { return r.temp !== null || r.fan !== null })

  // Throttle counters are cumulative, so a bucket's `thr` is how many events
  // fell inside it: shade those buckets rather than plotting a count nobody can
  // read off an axis.
  readonly property var throttleBands: {
    var out = []
    if (!overview) return out
    for (var i = 0; i < rows.length; i++)
      if (rows[i].thr > 0) out.push({ from: rows[i].t, to: rows[i].t + overview.bucket })
    return out
  }

  readonly property var maxFreq: {
    var m = 0
    for (var i = 0; i < rows.length; i++) m = Math.max(m, rows[i].freqMax || 0)
    return m > 0 ? m : null
  }

  readonly property var stackSeries: {
    if (!overview || !overview.stack) return []
    var out = []
    for (var i = 0; i < overview.stack.length; i++)
      if (overview.stack[i].points && overview.stack[i].points.length) out.push(overview.stack[i])
    return out
  }

  readonly property var incidents: overview && overview.incidents ? overview.incidents : []
  readonly property var recordedFrom: overview && overview.span ? overview.span.first : null

  // ------------------------------------------------------------------- rundown
  property string filterText: ""
  property string sortKey: ""
  property int sortDir: -1

  readonly property string effectiveSortKey: sortKey.length ? sortKey
    : (service && service.metric === "cpu" ? "cpuSecs"
       : service && service.metric === "io" ? "ioRd" : "rssMax")

  readonly property var hogRows: {
    if (!overview || !overview.hogs) return []
    var q = filterText.trim().toLowerCase()
    var out = []
    for (var i = 0; i < overview.hogs.length; i++) {
      var h = overview.hogs[i]
      if (!q || h.comm.toLowerCase().indexOf(q) !== -1) out.push(h)
    }
    var key = effectiveSortKey, dir = sortDir
    out.sort(function (a, b) {
      var va = a[key], vb = b[key]
      return (typeof va === "string" ? va.localeCompare(vb) : va - vb) * dir
    })
    return out
  }

  function requestSort(key) {
    if (effectiveSortKey === key) sortDir = -sortDir
    else { sortKey = key; sortDir = -1 }
  }

  // --------------------------------------------------------------------- hover
  // One hovered moment for the whole window. Every chart reports into it and
  // every chart draws its crosshair from it, which is what makes eight charts
  // read as one timeline.
  property real hoverT: -1
  property var hoverBrk: null
  property var tipContent: null
  property real tipX: 0
  property real tipY: 0

  readonly property real hoverU: {
    if (!axis) return -1
    if (hoverBrk) return (hoverBrk.u0 + hoverBrk.u1) / 2
    return hoverT >= 0 ? axis.u(hoverT) : -1
  }

  function probe(t, brk, sx, sy, builder) {
    if (t < 0) { hoverT = -1; hoverBrk = null; tipContent = null; return }
    tipX = sx; tipY = sy
    if (brk) {
      hoverT = brk.from
      hoverBrk = brk
      // What a collapsed stretch hid. Hovering one has to answer that, rather
      // than snap to whichever sample happens to sit on its edge.
      tipContent = {
        title: "time skipped · " + Fmt.dur(brk.to - brk.from),
        rows: [{ k: "from", v: Fmt.stamp(brk.from) },
               { k: "to", v: Fmt.stamp(brk.to) }],
        note: "no samples — asleep, off, or busywatch not running"
      }
      return
    }
    hoverBrk = null
    var hit = builder(t)
    if (!hit) { hoverT = -1; tipContent = null; return }
    hoverT = hit.t
    tipContent = hit.content
  }

  function nearest(list, t) {
    var best = null, bd = Infinity
    for (var i = 0; i < list.length; i++) {
      var d = Math.abs(list[i].t - t)
      if (d < bd) { bd = d; best = list[i] }
    }
    return best
  }

  // The system charts all speak about the same row, so they share one builder:
  // clock next to load and stall at the same instant is what tells you whether
  // a low reading means an idle machine or a held-back one.
  function systemTip(t) {
    var r = nearest(rows, t)
    if (!r) return null
    var out = [
      { k: "cpu stall", v: Fmt.pct(r.cpu) + "  peak " + Fmt.pct(r.cpuMax) },
      { k: "mem stall", v: Fmt.pct(r.mem) + "  peak " + Fmt.pct(r.memMax) },
      { k: "io stall", v: Fmt.pct(r.io) + "  peak " + Fmt.pct(r.ioMax) },
      { k: "memory used", v: Fmt.pct(r.memUsed) + " of " + Fmt.bytes(r.memTotal) },
      { k: "swap", v: Fmt.bytes(r.swap) },
      { k: "load", v: r.load.toFixed(2) + "  peak " + r.loadMax.toFixed(2) }
    ]
    if (r.freq !== null && r.freq !== undefined)
      out.push({ k: "cpu clock", v: Fmt.ghz(r.freq) + (r.freqMax ? " of " + Fmt.ghz(r.freqMax) : "") })
    if (r.thr)
      out.push({ k: "throttled", v: r.thr + "×" + (r.thrMs ? " · " + (r.thrMs / 1000).toFixed(1) + "s" : "") })
    if (r.temp !== null && r.temp !== undefined)
      out.push({ k: "cpu temp", v: Fmt.degC(r.temp) + (r.tempMax > r.temp ? "  peak " + Fmt.degC(r.tempMax) : "") })
    if (r.fan !== null && r.fan !== undefined)
      out.push({ k: "fan", v: (r.fan ? Math.round(r.fan) + " rpm" : "off")
        + (r.fanMax > r.fan ? "  peak " + Math.round(r.fanMax) + " rpm" : "") })
    if (r.bat !== null && r.bat !== undefined)
      out.push({ k: "battery", v: r.bat.toFixed(0) + "%"
        + (r.batW ? "  " + (r.batW < 0 ? "↓" : "↑") + Fmt.watts(r.batW) : "") })
    if (r.ac !== null && r.ac !== undefined)
      out.push({ k: "power", v: (r.ac >= 0.5 ? "on AC" : "on battery")
        + (r.ac > 0 && r.ac < 1 ? " (changed in this bucket)" : "") })
    return { t: r.t, content: { title: Fmt.stamp(r.t), rows: out } }
  }

  function stackTip(t) {
    var bt = stack.nearestTime(t)
    if (bt === null) return null
    var fmt = Fmt.forMetric(service ? service.metric : "mem")
    var out = []
    var total = 0
    for (var i = 0; i < stackSeries.length; i++) {
      var v = stack.valueAt(i, bt)
      total += v
      if (v) out.push({ k: stackSeries[i].comm, v: fmt(v), swatch: pal.forIndex(i) })
    }
    out.push({ k: "total shown", v: fmt(total), muted: true })
    return { t: bt, content: { title: Fmt.stamp(bt), rows: out } }
  }

  // Where the drilldown sits in the scrolled page, brought into view without
  // yanking: `flick` is null until the ScrollView has built its contents.
  function revealDrilldown() {
    var flick = page.contentItem
    if (!flick || !drilldown.visible) return
    var want = Math.max(0, Math.min(drilldown.y - Style.space(8),
                                    flick.contentHeight - flick.height))
    scrollTo.to = want
    scrollTo.restart()
  }

  // Clicking an app in the rundown opens a section below the fold, and a click
  // that appears to do nothing is worse than no click. The browser page called
  // scrollIntoView; this is the same intent asked of the ScrollView's flickable.
  //
  // It waits for the drilldown to exist rather than for the click: selecting an
  // app only starts a fetch, and the section is not there — so has no position
  // to scroll to — until that comes back.
  // One frame is not enough: the section has only just become visible, so the
  // column has not been laid out again and the flickable still reports the
  // height it had without it. Scrolling against that clamps the target to zero.
  onDetailOpenChanged: if (detailOpen) revealTimer.restart()

  Timer {
    id: revealTimer
    interval: 60
    onTriggered: root.revealDrilldown()
  }

  // --------------------------------------------------------------- drilldown
  readonly property var detail: service ? service.detail : null
  readonly property bool detailOpen: !!detail && !!service && service.selected.length > 0

  function detailRows(points) {
    var out = []
    var p = points || []
    for (var i = 0; i < p.length; i++) out.push({ t: p[i][0], v: p[i][1] })
    return out
  }
  readonly property var dMemRows: detail ? detailRows(detail.mem) : []
  readonly property var dCpuRows: detail ? detailRows(detail.cpu) : []
  readonly property var dIoRows: detail ? detailRows(detail.io) : []

  function detailTip(list, label, fmt) {
    return function (t) {
      var r = root.nearest(list, t)
      if (!r) return null
      return { t: r.t, content: { title: Fmt.stamp(r.t),
        rows: [{ k: (root.service ? root.service.selected : "") + " " + label, v: fmt(r.v) }] } }
    }
  }

  // ------------------------------------------------------------------ gauges
  readonly property var gauges: {
    var l = live
    if (!l) return []
    var memUsed = l.memTotal ? (l.memTotal - l.memAvail) / l.memTotal * 100 : 0
    var g = [
      { label: "cpu stall", value: Fmt.pct(l.cpu.avg60), frac: l.cpu.avg60, color: pal.cpu },
      { label: "memory", value: Fmt.pct(memUsed), frac: memUsed, color: pal.mem },
      { label: "mem stall", value: Fmt.pct(l.mem.avg60), frac: l.mem.avg60, color: pal.mem },
      { label: "io stall", value: Fmt.pct(l.io.avg60), frac: l.io.avg60, color: pal.io },
      { label: "load", value: l.load.toFixed(2),
        frac: l.load / Math.max(1, l.cores) * 100, color: pal.load },
      { label: "swap", value: Fmt.bytes(l.swapUsed),
        frac: l.swapTotal ? l.swapUsed / l.swapTotal * 100 : 0, color: pal.swap }
    ]
    // Power, clock and heat only exist on some machines, and only in databases
    // written by a version that records them. A desktop must not grow an empty
    // battery gauge, so each is added only when there is a reading behind it.
    if (l.cpuFreqKhz !== null && l.cpuFreqKhz !== undefined) {
      var mx = l.cpuFreqMaxKhz || l.cpuFreqKhz
      g.push({ label: "cpu clock", value: Fmt.ghz(l.cpuFreqKhz),
               frac: l.cpuFreqKhz / Math.max(1, mx) * 100, color: pal.freq })
    }
    if (l.cpuTempMc !== null && l.cpuTempMc !== undefined)
      g.push({ label: "cpu temp", value: Fmt.degC(l.cpuTempMc),
               frac: l.cpuTempMc / 1000, color: pal.temp })
    if (l.fanRpm !== null && l.fanRpm !== undefined)
      g.push({ label: "fan", value: l.fanRpm ? Math.round(l.fanRpm) + " rpm" : "off",
               frac: l.fanMaxRpm ? l.fanRpm / l.fanMaxRpm * 100 : 0, color: pal.fan })
    if (l.batPct !== null && l.batPct !== undefined) {
      // The label carries what the number cannot: charging on mains reads very
      // differently from the same percentage draining on an adapter that cannot
      // keep up.
      var on = l.acOnline === 1 ? " · AC" : ""
      var st = String(l.batStatus || "").toLowerCase()
      var arrow = st === "charging" ? "↑" : st === "discharging" ? "↓" : ""
      var w = (l.batPowerUw !== null && l.batPowerUw !== undefined && l.batPowerUw !== 0)
        ? " " + arrow + Math.abs(l.batPowerUw / 1e6).toFixed(1) + "W" : ""
      g.push({ label: "battery" + on, value: l.batPct.toFixed(0) + "%" + w,
               frac: l.batPct, color: pal.bat })
    }
    return g
  }

  // How far back the history actually goes, and which range buttons can show
  // anything at all — a 30d button over 8h of history looks like a dead button.
  readonly property real haveSecs: {
    if (!overview || !overview.span || !overview.span.first) return 0
    return overview.now - overview.span.first
  }

  // ===========================================================================
  NumberAnimation {
    id: scrollTo
    target: page.contentItem
    property: "contentY"
    duration: 220
    easing.type: Easing.OutCubic
  }

  FloatingWindow {
    id: window
    visible: root.opened
    title: "busywatch"
    color: pal.ground
    implicitWidth: Style.space(1180)
    implicitHeight: Style.space(820)
    minimumSize: Qt.size(Style.space(720), Style.space(480))

    onVisibleChanged: {
      if (!visible && root.opened && !root.closingFromHost) root.requestClose()
    }

    // The charts are drawn to the width they have. Asking for a thousand
    // buckets to paint across six hundred pixels is work nobody sees, so the
    // request is sized to the window — debounced, because a drag-resize would
    // otherwise refetch on every frame.
    onWidthChanged: pointsDebounce.restart()
    Timer {
      id: pointsDebounce
      interval: 200
      onTriggered: {
        if (!root.service) return
        var want = Math.min(1200, Math.max(60, Math.round(window.width * 0.9)))
        if (Math.abs(want - root.service.points) < 40) return
        root.service.points = want
        root.service.loadOverview()
      }
    }

    ColumnLayout {
      anchors.fill: parent
      spacing: 0

      // ---------------------------------------------------------- offline bar
      // The old failure mode was a grey line in a corner: the charts kept
      // showing the previous range, so a click looked like it had done nothing.
      Rectangle {
        Layout.fillWidth: true
        visible: !root.online
        color: pal.cpu
        implicitHeight: offlineText.implicitHeight + Style.space(12)

        Text {
          id: offlineText
          anchors.verticalCenter: parent.verticalCenter
          anchors.left: parent.left
          anchors.leftMargin: Style.space(16)
          text: "busywatch is not responding on " + (root.service ? root.service.base : "")
            + (root.service && root.service.lastError.length ? " — " + root.service.lastError : "")
            + " · retrying…"
          font.family: Style.font.family
          font.pixelSize: Style.font.bodySmall
          color: "#ffffff"
        }
      }

      // --------------------------------------------------------------- header
      Rectangle {
        Layout.fillWidth: true
        color: pal.panel
        implicitHeight: headerCol.implicitHeight + Style.space(20)

        Rectangle {
          anchors.bottom: parent.bottom
          width: parent.width
          height: 1
          color: pal.line
        }

        ColumnLayout {
          id: headerCol
          anchors.left: parent.left
          anchors.right: parent.right
          anchors.top: parent.top
          anchors.margins: Style.space(10)
          anchors.leftMargin: Style.space(16)
          anchors.rightMargin: Style.space(16)
          spacing: Style.space(10)

          // A Flow, not a Row: seven ranges plus a pause button and a title do
          // not fit a narrow window, and pills that run off the right edge are
          // pills you cannot press. They wrap to a second line instead.
          Flow {
            Layout.fillWidth: true
            spacing: Style.space(10)

            Row {
              spacing: Style.space(10)
              Text {
                anchors.verticalCenter: parent.verticalCenter
                text: "busywatch"
                font.family: Style.font.family
                font.pixelSize: Style.font.title
                font.weight: Font.DemiBold
                color: pal.ink
              }
              Text {
                anchors.verticalCenter: parent.verticalCenter
                text: root.live ? "· " + root.live.cores + " cores · "
                                  + Fmt.bytes(root.live.memTotal) + " ram" : ""
                font.family: Style.font.family
                font.pixelSize: Style.font.bodySmall
                color: pal.dim
              }
            }

            Row {
              spacing: Style.space(4)

              Repeater {
                model: root.ranges
                Pill {
                  colors: pal
                  text: modelData[0]
                  pressedState: !!root.service && root.service.span === modelData[1]
                  empty: root.haveSecs > 0 && modelData[1] > root.haveSecs * 1.5
                  onClicked: if (root.service) root.service.setSpan(modelData[1])
                }
              }

              Pill {
                colors: pal
                text: root.service && root.service.paused ? "paused" : "pause"
                pressedState: !!root.service && root.service.paused
                onClicked: {
                  if (!root.service) return
                  root.service.paused = !root.service.paused
                  if (!root.service.paused) root.service.loadOverview()
                }
              }
            }
          }

          Flow {
            Layout.fillWidth: true
            spacing: Style.space(16)

            Repeater {
              model: root.gauges
              Gauge {
                colors: pal
                label: modelData.label
                value: modelData.value
                fraction: modelData.frac
                accent: modelData.color
              }
            }
          }
        }
      }

      // ----------------------------------------------------------------- page
      ScrollView {
        id: page
        Layout.fillWidth: true
        Layout.fillHeight: true
        contentWidth: availableWidth
        clip: true

        ColumnLayout {
          // ScrollView reparents its content into a Flickable, so `parent` is
          // not the ScrollView and reaching for its width through the chain is
          // a guess about that. Ask the ScrollView itself.
          width: page.availableWidth
          spacing: Style.space(14)

          Item { Layout.preferredHeight: Style.space(2) }

          // ================================================ system pressure
          Section {
            colors: pal
            Layout.leftMargin: Style.space(16)
            Layout.rightMargin: Style.space(16)

            RowLayout {
              Layout.fillWidth: true
              spacing: Style.space(10)

              Text {
                text: "System pressure"
                font.family: Style.font.family
                font.pixelSize: Style.font.subtitle
                font.weight: Font.DemiBold
                color: pal.ink
              }
              Text {
                Layout.fillWidth: true
                Layout.minimumWidth: 0
                elide: Text.ElideRight
                text: {
                  if (!root.overview || !root.rows.length) return ""
                  var n = root.incidents.length
                  return root.rows.length + " points · " + Fmt.dur(root.overview.bucket) + " each · "
                    + Fmt.stamp(root.overview.from) + " → " + Fmt.stamp(root.overview.to)
                    + (n ? " · " + n + " incident" + (n > 1 ? "s" : "") : "")
                }
                font.family: Style.font.family
                font.pixelSize: Style.font.bodySmall
                color: pal.dim
              }
              Text {
                Layout.fillWidth: true
                Layout.minimumWidth: 0
                horizontalAlignment: Text.AlignRight
                elide: Text.ElideRight
                text: root.overview && root.overview.span && root.overview.span.first
                  ? "history: " + Fmt.dur(root.haveSecs) + " back to "
                    + Fmt.stamp(root.overview.span.first)
                  : "no history recorded yet"
                font.family: Style.font.family
                font.pixelSize: Style.font.bodySmall
                color: pal.dim
              }
            }

            Text {
              Layout.fillWidth: true
              visible: !root.rows.length
              text: "No samples in this range yet. busywatch records a sample every "
                + "minute once the watcher runs."
              wrapMode: Text.WordWrap
              font.family: Style.font.family
              font.pixelSize: Style.font.body
              color: pal.dim
            }

            Chart {
              id: c1
              Layout.fillWidth: true
              visible: root.rows.length > 0
              implicitHeight: Style.space(104)
              axis: root.axis; rows: root.rows; bucket: root.overview ? root.overview.bucket : 60
              incidents: root.incidents; recordedFrom: root.recordedFrom
              colors: pal; hoverU: root.hoverU
              title: "cpu stall % · load"
              fmtL: Fmt.pct
              fmtR: function (v) { return v.toFixed(v < 10 ? 1 : 0) }
              defs: [{ key: "cpuMax", color: pal.cpu, kind: "area", opacity: 0.13,
                       width: 0.8, dash: [2, 2] },
                     { key: "cpu", color: pal.cpu, kind: "area" },
                     { key: "load", color: pal.load, axis: "r", width: 1.2 }]
              onProbed: function (t, brk, sx, sy) { root.probe(t, brk, sx, sy, root.systemTip) }
            }

            Chart {
              Layout.fillWidth: true
              visible: root.rows.length > 0
              implicitHeight: Style.space(104)
              axis: root.axis; rows: root.rows; bucket: root.overview ? root.overview.bucket : 60
              incidents: root.incidents; recordedFrom: root.recordedFrom
              colors: pal; hoverU: root.hoverU
              title: "memory used % · mem stall % (right)"
              maxL: 100; floorR: 5
              fmtL: Fmt.pct; fmtR: Fmt.pct
              defs: [{ key: "memUsed", color: pal.mem, kind: "area" },
                     { key: "memUsedMax", color: pal.mem, width: 0.8, dash: [2, 2] },
                     { key: "memMax", color: pal.cpu, axis: "r", width: 1.2 }]
              onProbed: function (t, brk, sx, sy) { root.probe(t, brk, sx, sy, root.systemTip) }
            }

            Chart {
              Layout.fillWidth: true
              visible: root.rows.length > 0
              implicitHeight: Style.space(92)
              axis: root.axis; rows: root.rows; bucket: root.overview ? root.overview.bucket : 60
              incidents: root.incidents; recordedFrom: root.recordedFrom
              colors: pal; hoverU: root.hoverU
              title: "io stall %"
              fmtL: Fmt.pct
              defs: [{ key: "ioMax", color: pal.io, kind: "area", opacity: 0.12,
                       width: 0.8, dash: [2, 2] },
                     { key: "io", color: pal.io, kind: "area" }]
              onProbed: function (t, brk, sx, sy) { root.probe(t, brk, sx, sy, root.systemTip) }
            }

            Chart {
              Layout.fillWidth: true
              visible: root.rows.length > 0 && root.swapSeen
              implicitHeight: Style.space(84)
              axis: root.axis; rows: root.rows; bucket: root.overview ? root.overview.bucket : 60
              incidents: root.incidents; recordedFrom: root.recordedFrom
              colors: pal; hoverU: root.hoverU
              title: "swap used"
              fmtL: Fmt.bytes; floorL: 1024
              defs: [{ key: "swap", color: pal.swap, kind: "area" }]
              onProbed: function (t, brk, sx, sy) { root.probe(t, brk, sx, sy, root.systemTip) }
            }

            Chart {
              Layout.fillWidth: true
              visible: root.rows.length > 0 && root.clockSeen
              implicitHeight: Style.space(92)
              axis: root.axis; rows: root.rows; bucket: root.overview ? root.overview.bucket : 60
              incidents: root.incidents; recordedFrom: root.recordedFrom
              bands: root.throttleBands
              colors: pal; hoverU: root.hoverU
              title: "cpu clock · battery % (right)"
                + (root.throttleBands.length ? " · shaded = throttled" : "")
              maxL: root.maxFreq; maxR: 100
              fmtL: Fmt.ghz
              fmtR: function (v) { return v.toFixed(0) + "%" }
              defs: [{ key: "freq", color: pal.freq, kind: "area" },
                     { key: "bat", color: pal.bat, axis: "r", width: 1.2 }]
              onProbed: function (t, brk, sx, sy) { root.probe(t, brk, sx, sy, root.systemTip) }
            }

            // Heat sits directly under the clock rather than at the end of the
            // column: the shaded throttled stretches above are what this line
            // is the explanation for.
            Chart {
              Layout.fillWidth: true
              visible: root.rows.length > 0 && root.heatSeen
              implicitHeight: Style.space(92)
              axis: root.axis; rows: root.rows; bucket: root.overview ? root.overview.bucket : 60
              incidents: root.incidents; recordedFrom: root.recordedFrom
              colors: pal; hoverU: root.hoverU
              title: "cpu temp · fan rpm (right)"
              // Fixed to 100°C rather than scaled to whatever happened: a CPU
              // is at its limit around there, so the height of the line means
              // something on its own, and means the same thing every range.
              maxL: 100; floorR: 1000
              fmtL: function (v) { return v.toFixed(0) + "°C" }
              fmtR: Fmt.krpm
              defs: [{ key: "tempMaxC", color: pal.temp, kind: "area", opacity: 0.12,
                       width: 0.8, dash: [2, 2] },
                     { key: "tempC", color: pal.temp, kind: "area" },
                     { key: "fan", color: pal.fan, axis: "r", width: 1.2 }]
              onProbed: function (t, brk, sx, sy) { root.probe(t, brk, sx, sy, root.systemTip) }
            }

            Chart {
              Layout.fillWidth: true
              visible: root.rows.length > 0 && root.wattsSeen
              implicitHeight: Style.space(92)
              axis: root.axis; rows: root.rows; bucket: root.overview ? root.overview.bucket : 60
              incidents: root.incidents; recordedFrom: root.recordedFrom
              colors: pal; hoverU: root.hoverU
              // The title doubles as the legend: two bands, coloured in place,
              // so nobody has to guess which way round they are.
              titleRuns: [{ text: "battery power · ", color: pal.dim },
                          { text: "↓ draw", color: pal.draw },
                          { text: " · ", color: pal.dim },
                          { text: "↑ charge", color: pal.chg }]
              fmtL: function (w) { return w.toFixed(w < 10 ? 1 : 0) + "W" }
              floorL: 5
              defs: [{ key: "wOut", color: pal.draw, kind: "area" },
                     { key: "wIn", color: pal.chg, kind: "area" }]
              onProbed: function (t, brk, sx, sy) { root.probe(t, brk, sx, sy, root.systemTip) }
            }

            Flow {
              Layout.fillWidth: true
              Layout.topMargin: Style.space(4)
              spacing: Style.space(12)
              visible: root.rows.length > 0

              Repeater {
                model: [{ t: "CPU stall", c: pal.cpu }, { t: "memory used", c: pal.mem },
                        { t: "IO stall", c: pal.io }, { t: "load", c: pal.load }]
                Row {
                  spacing: Style.space(5)
                  Rectangle {
                    anchors.verticalCenter: parent.verticalCenter
                    width: Style.space(9); height: Style.space(9); radius: Style.space(2)
                    color: modelData.c
                  }
                  Text {
                    text: modelData.t
                    font.family: Style.font.family
                    font.pixelSize: Style.font.bodySmall
                    color: pal.dim
                  }
                }
              }

              Text {
                text: "shaded band = recorded incident"
                font.family: Style.font.family
                font.pixelSize: Style.font.bodySmall
                color: pal.dim
              }

              // The collapsed stretches need saying out loud, not just drawing:
              // an axis that is not to scale is worse than a gap if nobody
              // notices it is there.
              Text {
                visible: !!root.axis && root.axis.breaks.length > 0
                text: {
                  if (!root.axis || !root.axis.breaks.length) return ""
                  var n = root.axis.breaks.length
                  return "⁄⁄ " + n + " gap" + (n > 1 ? "s" : "") + " collapsed · "
                    + Fmt.dur(root.axis.dead) + " asleep, off or not recording"
                }
                font.family: Style.font.family
                font.pixelSize: Style.font.bodySmall
                color: pal.dim
              }
            }
          }

          // ========================================================== apps
          Section {
            colors: pal
            Layout.leftMargin: Style.space(16)
            Layout.rightMargin: Style.space(16)

            RowLayout {
              Layout.fillWidth: true
              spacing: Style.space(10)

              Text {
                text: root.service
                  ? ({ mem: "Apps by memory", cpu: "Apps by CPU", io: "Apps by IO" })[root.service.metric]
                  : "Apps"
                font.family: Style.font.family
                font.pixelSize: Style.font.subtitle
                font.weight: Font.DemiBold
                color: pal.ink
              }
              Text {
                Layout.fillWidth: true
                Layout.minimumWidth: 0
                text: "summed across processes sharing a name; stacked"
                elide: Text.ElideRight
                font.family: Style.font.family
                font.pixelSize: Style.font.bodySmall
                color: pal.dim
              }

              Repeater {
                model: root.metrics
                Pill {
                  colors: pal
                  text: modelData[1]
                  pressedState: !!root.service && root.service.metric === modelData[0]
                  onClicked: {
                    if (!root.service) return
                    root.sortKey = ""
                    root.service.setMetric(modelData[0])
                  }
                }
              }
            }

            StackChart {
              id: stack
              Layout.fillWidth: true
              visible: root.stackSeries.length > 0
              implicitHeight: Style.space(190)
              axis: root.axis
              series: root.stackSeries
              bucket: root.overview ? root.overview.bucket : 60
              fmt: Fmt.forMetric(root.service ? root.service.metric : "mem")
              recordedFrom: root.recordedFrom
              selected: root.service ? root.service.selected : ""
              colors: pal
              hoverU: root.hoverU
              onProbed: function (t, brk, sx, sy) { root.probe(t, brk, sx, sy, root.stackTip) }
            }

            Text {
              Layout.fillWidth: true
              visible: root.stackSeries.length === 0
              text: "No per-process history in this range."
              font.family: Style.font.family
              font.pixelSize: Style.font.body
              color: pal.dim
            }

            Flow {
              Layout.fillWidth: true
              spacing: Style.space(12)

              Repeater {
                model: root.stackSeries

                // The click target has to fill the whole entry, and a Row will
                // not have an anchored child — so the Row is the content and
                // this Item is the thing that can be clicked.
                Item {
                  implicitWidth: entry.implicitWidth
                  implicitHeight: entry.implicitHeight

                  Row {
                    id: entry
                    spacing: Style.space(5)
                    Rectangle {
                      anchors.verticalCenter: parent.verticalCenter
                      width: Style.space(9); height: Style.space(9); radius: Style.space(2)
                      color: pal.forIndex(index)
                    }
                    Text {
                      text: modelData.comm
                      font.family: Style.font.family
                      font.pixelSize: Style.font.bodySmall
                      // The selected app is named in full strength; the rest
                      // stay subordinate, the way their bands do.
                      color: root.service && root.service.selected === modelData.comm
                        ? pal.ink : pal.dim
                    }
                  }

                  MouseArea {
                    anchors.fill: parent
                    cursorShape: Qt.PointingHandCursor
                    onClicked: if (root.service) root.service.select(modelData.comm)
                  }
                }
              }
            }

            RowLayout {
              Layout.fillWidth: true
              Layout.topMargin: Style.space(6)
              spacing: Style.space(12)

              TextField {
                Layout.preferredWidth: Style.space(200)
                placeholderText: "filter apps…"
                font.family: Style.font.family
                font.pixelSize: Style.font.body
                onTextChanged: root.filterText = text
              }
              Text {
                text: root.filterText.trim().length
                  ? root.hogRows.length + " of " + (root.overview && root.overview.hogs
                      ? root.overview.hogs.length : 0) + " apps"
                  : (root.overview && root.overview.hogs ? root.overview.hogs.length : 0) + " apps recorded"
                font.family: Style.font.family
                font.pixelSize: Style.font.bodySmall
                color: pal.dim
              }
              Text {
                Layout.fillWidth: true
                text: "click an app for its full rundown"
                font.family: Style.font.family
                font.pixelSize: Style.font.bodySmall
                color: pal.dim
              }
            }

            DataTable {
              Layout.fillWidth: true
              colors: pal
              maxRows: 14
              idKey: "comm"
              selectedKey: root.service ? root.service.selected : ""
              rows: root.hogRows
              sortKey: root.effectiveSortKey
              sortDir: root.sortDir
              emptyText: "nothing recorded yet"
              columns: [
                { key: "comm", title: "app", flex: 2.4, min: 168, sortable: true },
                { key: "rssLast", title: "rss now", flex: 1, min: 84, align: "right", sortable: true },
                { key: "rssMax", title: "rss peak", flex: 1, min: 86, align: "right", sortable: true },
                { key: "rssAvg", title: "rss avg", flex: 1, min: 84, align: "right", sortable: true },
                { key: "cpuSecs", title: "cpu time", flex: 1, min: 86, align: "right", sortable: true },
                { key: "cpuMax", title: "cpu peak", flex: 0.9, min: 86, align: "right", sortable: true },
                { key: "cpuAvg", title: "cpu avg", flex: 0.9, min: 82, align: "right", sortable: true },
                { key: "ioRd", title: "io read", flex: 1, min: 80, align: "right", sortable: true },
                { key: "ioWr", title: "io write", flex: 1, min: 84, align: "right", sortable: true },
                { key: "pids", title: "pids", flex: 0.6, min: 56, align: "right", sortable: true },
                { key: "last", title: "last seen", flex: 1.2, min: 104, align: "right", sortable: true }
              ]
              cell: function (h, key) {
                switch (key) {
                case "comm": return { text: h.comm, pill: h.pids > 1 ? h.pids + " pids" : "" }
                case "rssLast": return h.rssLast ? { text: Fmt.bytes(h.rssLast) }
                                                 : { text: "—", muted: true }
                case "rssMax": return { text: Fmt.bytes(h.rssMax), bold: true }
                case "rssAvg": return { text: Fmt.bytes(h.rssAvg) }
                case "cpuSecs": return h.cpuSecs >= 1
                  ? { text: Fmt.dur(Math.round(h.cpuSecs)) } : { text: "—", muted: true }
                case "cpuMax": return { text: Fmt.pct(h.cpuMax) }
                case "cpuAvg": return { text: Fmt.pct(h.cpuAvg) }
                case "ioRd": return h.ioRd ? { text: Fmt.bytes(h.ioRd / 1024) }
                                           : { text: "—", muted: true }
                case "ioWr": return h.ioWr ? { text: Fmt.bytes(h.ioWr / 1024) }
                                           : { text: "—", muted: true }
                case "pids": return { text: String(h.pids) }
                case "last": return { text: Fmt.ago(h.last), muted: true }
                }
                return { text: "" }
              }
              onSortRequested: function (key) { root.requestSort(key) }
              onActivated: function (h) { if (root.service) root.service.select(h.comm) }
            }
          }

          // ===================================================== drilldown
          Section {
            id: drilldown
            colors: pal
            visible: root.detailOpen
            Layout.leftMargin: Style.space(16)
            Layout.rightMargin: Style.space(16)

            RowLayout {
              Layout.fillWidth: true
              spacing: Style.space(10)

              Text {
                text: root.service ? root.service.selected : ""
                font.family: Style.font.family
                font.pixelSize: Style.font.subtitle
                font.weight: Font.DemiBold
                color: pal.ink
              }
              Text {
                Layout.fillWidth: true
                Layout.minimumWidth: 0
                elide: Text.ElideRight
                text: {
                  if (!root.detail || !root.detail.summary) return ""
                  var a = root.detail.summary
                  if (!a.samples) return "nothing recorded for this app in this range"
                  return a.pidsSeen + " pid" + (a.pidsSeen === 1 ? "" : "s")
                    + " seen · present in " + a.samples + " samples · "
                    + Fmt.stamp(a.first) + " → " + Fmt.stamp(a.last)
                }
                font.family: Style.font.family
                font.pixelSize: Style.font.bodySmall
                color: pal.dim
              }
              Pill {
                colors: pal
                text: "close"
                onClicked: if (root.service) root.service.select("")
              }
            }

            GridLayout {
              Layout.fillWidth: true
              Layout.topMargin: Style.space(4)
              columns: Math.max(2, Math.floor(width / Style.space(150)))
              columnSpacing: Style.space(10)
              rowSpacing: Style.space(10)

              Repeater {
                model: {
                  if (!root.detail || !root.detail.summary) return []
                  var a = root.detail.summary
                  var inc = root.detail.incidents || []
                  var win = Math.max(1, root.detail.to - root.detail.from)
                  var kinds = []
                  for (var i = 0; i < inc.length; i++)
                    if (kinds.indexOf(inc[i].kind) === -1) kinds.push(inc[i].kind)
                  return [
                    { l: "cpu time", v: a.cpuSecs >= 1 ? Fmt.dur(Math.round(a.cpuSecs)) : "—",
                      s: (a.cpuSecs / win * 100).toFixed(1) + "% of one core, "
                         + Fmt.dur(win) + " range" },
                    { l: "cpu peak", v: Fmt.pct(a.cpuMax),
                      s: "avg " + Fmt.pct(a.cpuAvg) + " while present" },
                    { l: "rss peak", v: Fmt.bytes(a.rssMax),
                      s: root.detail.memTotal
                         ? (a.rssMax / root.detail.memTotal * 100).toFixed(0) + "% of ram" : "" },
                    { l: "rss now", v: a.rssLast ? Fmt.bytes(a.rssLast) : "—",
                      s: a.rssLast && a.rssAvg ? "avg " + Fmt.bytes(a.rssAvg)
                                               : "not in last sample" },
                    { l: "io read", v: a.ioRd ? Fmt.bytes(a.ioRd / 1024) : "—",
                      s: a.ioMax ? "peak " + Fmt.rate(a.ioMax) : "" },
                    { l: "io written", v: a.ioWr ? Fmt.bytes(a.ioWr / 1024) : "—", s: "" },
                    { l: "pids", v: String(a.pidsSeen),
                      s: a.pidsMax > 1 ? "up to " + a.pidsMax + " at once" : "single process" },
                    { l: "incidents caused", v: String(inc.length),
                      s: kinds.length ? kinds.join(", ") : "none" }
                  ]
                }
                Tile {
                  Layout.fillWidth: true
                  colors: pal
                  label: modelData.l
                  value: modelData.v
                  sub: modelData.s
                }
              }
            }

            // These three sit on the window's shared axis, so they are rebuilt
            // whenever the range changes or a new gap is collapsed — not only
            // when the drilldown is opened.
            Chart {
              Layout.fillWidth: true
              Layout.topMargin: Style.space(6)
              implicitHeight: Style.space(112)
              axis: root.axis; rows: root.dMemRows
              bucket: root.detail ? root.detail.bucket : 60
              incidents: root.detail && root.detail.incidents ? root.detail.incidents : []
              colors: pal; hoverU: root.hoverU
              title: "rss"
              fmtL: Fmt.bytes; floorL: 1024
              defs: [{ key: "v", color: pal.mem, kind: "area" }]
              onProbed: function (t, brk, sx, sy) {
                root.probe(t, brk, sx, sy, root.detailTip(root.dMemRows, "rss", Fmt.bytes))
              }
            }

            Chart {
              Layout.fillWidth: true
              implicitHeight: Style.space(112)
              axis: root.axis; rows: root.dCpuRows
              bucket: root.detail ? root.detail.bucket : 60
              incidents: root.detail && root.detail.incidents ? root.detail.incidents : []
              colors: pal; hoverU: root.hoverU
              title: "cpu %"
              fmtL: Fmt.pct
              defs: [{ key: "v", color: pal.cpu, kind: "area" }]
              onProbed: function (t, brk, sx, sy) {
                root.probe(t, brk, sx, sy, root.detailTip(root.dCpuRows, "cpu", Fmt.pct))
              }
            }

            Chart {
              Layout.fillWidth: true
              implicitHeight: Style.space(96)
              axis: root.axis; rows: root.dIoRows
              bucket: root.detail ? root.detail.bucket : 60
              incidents: root.detail && root.detail.incidents ? root.detail.incidents : []
              colors: pal; hoverU: root.hoverU
              title: "io read+write per second"
              fmtL: Fmt.rate; floorL: 65536
              defs: [{ key: "v", color: pal.io, kind: "area" }]
              onProbed: function (t, brk, sx, sy) {
                root.probe(t, brk, sx, sy, root.detailTip(root.dIoRows, "io", Fmt.rate))
              }
            }

            Text {
              Layout.topMargin: Style.space(6)
              text: "PIDS"
              font.family: Style.font.family
              font.pixelSize: Style.font.caption
              font.letterSpacing: 0.4
              color: pal.dim
            }

            DataTable {
              Layout.fillWidth: true
              colors: pal
              maxRows: 8
              rows: root.detail && root.detail.pids ? root.detail.pids : []
              emptyText: "no pids in range"
              columns: [
                { key: "pid", title: "pid", flex: 1, min: 70 },
                { key: "rssMax", title: "rss peak", flex: 1, min: 86, align: "right" },
                { key: "cpuMax", title: "cpu peak", flex: 1, min: 86, align: "right" },
                { key: "ioMax", title: "io peak", flex: 1, min: 84, align: "right" },
                { key: "first", title: "first seen", flex: 1.4, min: 118, align: "right" },
                { key: "last", title: "last seen", flex: 1.4, min: 118, align: "right" }
              ]
              cell: function (p, key) {
                switch (key) {
                case "pid": return { text: String(p.pid) }
                case "rssMax": return { text: Fmt.bytes(p.rssMax) }
                case "cpuMax": return { text: Fmt.pct(p.cpuMax) }
                case "ioMax": return p.ioMax ? { text: Fmt.rate(p.ioMax) }
                                             : { text: "—", muted: true }
                case "first": return { text: Fmt.stamp(p.first) }
                case "last": return { text: Fmt.stamp(p.last) }
                }
                return { text: "" }
              }
            }

            Text {
              Layout.topMargin: Style.space(6)
              text: "INCIDENTS IT WAS BLAMED FOR"
              font.family: Style.font.family
              font.pixelSize: Style.font.caption
              font.letterSpacing: 0.4
              color: pal.dim
            }

            DataTable {
              Layout.fillWidth: true
              colors: pal
              maxRows: 6
              rows: root.detail && root.detail.incidents ? root.detail.incidents : []
              emptyText: "none — it never triggered a warning"
              columns: [
                { key: "started", title: "when", flex: 1.4, min: 118 },
                { key: "kind", title: "kind", flex: 1, min: 78 },
                { key: "dur", title: "duration", flex: 1, min: 86, align: "right" },
                { key: "peak", title: "peak stall", flex: 1, min: 90, align: "right" }
              ]
              cell: function (i, key) {
                switch (key) {
                case "started": return { text: Fmt.stamp(i.started) }
                case "kind": return { text: i.kind, swatch: pal.forKind(i.kind) }
                case "dur": return { text: Fmt.dur((i.ended === null || i.ended === undefined
                  ? Math.round(Date.now() / 1000) : i.ended) - i.started) }
                case "peak": return i.peak === null || i.peak === undefined
                  ? { text: "—", muted: true } : { text: Fmt.pct(i.peak) }
                }
                return { text: "" }
              }
            }
          }

          // ===================================================== incidents
          Section {
            colors: pal
            Layout.leftMargin: Style.space(16)
            Layout.rightMargin: Style.space(16)
            Layout.bottomMargin: Style.space(16)

            RowLayout {
              Layout.fillWidth: true
              spacing: Style.space(10)
              Text {
                text: "Incidents"
                font.family: Style.font.family
                font.pixelSize: Style.font.subtitle
                font.weight: Font.DemiBold
                color: pal.ink
              }
              Text {
                Layout.fillWidth: true
                text: root.incidents.length ? root.incidents.length + " in range" : "none in range"
                font.family: Style.font.family
                font.pixelSize: Style.font.bodySmall
                color: pal.dim
              }
            }

            DataTable {
              Layout.fillWidth: true
              colors: pal
              maxRows: 10
              rows: root.incidents
              emptyText: "no incidents recorded"
              columns: [
                { key: "started", title: "when", flex: 1.4, min: 118 },
                { key: "kind", title: "kind", flex: 1.1, min: 104 },
                { key: "dur", title: "duration", flex: 1, min: 86, align: "right" },
                { key: "peak", title: "peak stall", flex: 1, min: 90, align: "right" },
                { key: "minMemAvail", title: "min free mem", flex: 1.1, min: 112, align: "right" },
                { key: "top", title: "culprit", flex: 1.4, min: 100, align: "right" }
              ]
              cell: function (i, key) {
                switch (key) {
                case "started": return { text: Fmt.stamp(i.started) }
                case "kind": return { text: i.kind, swatch: pal.forKind(i.kind),
                  pill: (i.ended === null || i.ended === undefined) ? "ongoing" : "" }
                case "dur": return { text: Fmt.dur((i.ended === null || i.ended === undefined
                  ? (root.overview ? root.overview.now : 0) : i.ended) - i.started) }
                case "peak": return i.peak === null || i.peak === undefined
                  ? { text: "—", muted: true } : { text: Fmt.pct(i.peak) }
                case "minMemAvail": return i.minMemAvail === null || i.minMemAvail === undefined
                  ? { text: "—", muted: true } : { text: Fmt.bytes(i.minMemAvail) }
                case "top": return i.top ? { text: i.top } : { text: "—", muted: true }
                }
                return { text: "" }
              }
              onActivated: function (i) {
                if (i.top && root.service) root.service.select(i.top)
              }
            }
          }
        }
      }
    }

    Tip {
      colors: pal
      content: root.tipContent
      anchorX: root.tipX
      anchorY: root.tipY
    }
  }
}
