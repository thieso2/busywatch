import QtQuick
import Quickshell

// Everything the bar widget and the window both need, kept in one place that
// outlives either of them.
//
// The service is what talks to the running busywatch. It holds the view state
// too — range, metric, which app is drilled into — because closing the window
// and opening it again should come back where you were, and in the browser UI
// that job belonged to the URL hash. There is no URL here, so it belongs to
// the thing that survives the window.
//
// Two polls, because they answer different questions at different costs. The
// bar needs one small reading of what is true now, often. The window needs the
// whole range — series, hogs, stack, incidents — and only while it is on
// screen. Running the second one for a bar dot would pull four hundred rows of
// application history every ten seconds to colour nine pixels.
Item {
  id: root

  visible: false
  width: 0
  height: 0

  // Injected by the shell after the component loads. Not `required`: the host
  // assigns them, and a required property would fail the load before it could.
  property var shell: null
  property var manifest: null
  property var pluginRegistry: null
  property var barWidgetRegistry: null

  readonly property string pluginId: manifest && manifest.id
    ? String(manifest.id) : "busywatch"

  // ------------------------------------------------------------------ config
  // Settings reach a plugin through its bar widget, not its service, so the
  // widget pushes them across. Until it does, these are the defaults.
  property string host: "127.0.0.1"
  property int port: 8787
  property int barIntervalSec: 10
  property int windowIntervalSec: 15
  property real cpuSustained: 20
  property real memSustained: 10
  property real ioSustained: 25

  readonly property string base: "http://" + host + ":" + port

  function applySettings(s) {
    if (!s) return
    if (s.host !== undefined && s.host !== null && String(s.host).length)
      host = String(s.host)
    if (s.port !== undefined && s.port !== null && Number(s.port) > 0)
      port = Number(s.port)
    if (s.refreshIntervalSec !== undefined && s.refreshIntervalSec !== null)
      barIntervalSec = Math.max(2, Number(s.refreshIntervalSec))
  }

  // ------------------------------------------------------------------- state
  property var live: null          // the /api/live snapshot, for the bar
  property var overview: null      // the /api/overview payload, for the window
  property bool online: false
  property string lastError: ""
  property bool windowOpen: false
  property bool paused: false

  // The view, which is the window's but is kept here so it survives it.
  property int span: 3600
  property string metric: "mem"
  property string selected: ""     // drilled-into app, "" for none
  property var detail: null        // its /api/proc payload

  // How wide the charts are, in points. The window sets it from its own width;
  // asking for a thousand buckets to draw across six hundred pixels is work
  // nobody sees.
  property int points: 900

  signal dataArrived()
  signal detailArrived()

  // ----------------------------------------------------------------- verdict
  // What the bar dot means, in the same vocabulary the toasts and the tray
  // icon use: the resource whose sustained average is over its threshold, worst
  // first, or "ok" when nothing is.
  readonly property string verdict: {
    if (!online || !live) return "down"
    var over = []
    if (live.cpu && live.cpu.avg60 >= cpuSustained) over.push(["cpu", live.cpu.avg60 / cpuSustained])
    if (live.mem && live.mem.avg60 >= memSustained) over.push(["mem", live.mem.avg60 / memSustained])
    if (live.io && live.io.avg60 >= ioSustained) over.push(["io", live.io.avg60 / ioSustained])
    if (!over.length) return "ok"
    over.sort(function (a, b) { return b[1] - a[1] })
    return over[0][0]
  }

  readonly property string barTooltip: {
    if (!online) return "busywatch — not responding on " + base
    if (!live) return "busywatch"
    var p = function (v) { return (v >= 10 ? v.toFixed(0) : v.toFixed(1)) + "%" }
    var memUsed = live.memTotal ? (live.memTotal - live.memAvail) / live.memTotal * 100 : 0
    return "cpu stall " + p(live.cpu.avg60)
      + " · mem stall " + p(live.mem.avg60)
      + " · io stall " + p(live.io.avg60)
      + "\nmemory " + p(memUsed) + " used · load " + live.load.toFixed(2)
  }

  // -------------------------------------------------------------------- http
  // One helper, because all three endpoints are GET-JSON-or-fail and the only
  // interesting part is what to do with the answer.
  function get(path, onOk, onErr) {
    var xhr = new XMLHttpRequest()
    xhr.onreadystatechange = function () {
      if (xhr.readyState !== XMLHttpRequest.DONE) return
      if (xhr.status < 200 || xhr.status >= 300) {
        if (onErr) onErr(xhr.status ? "HTTP " + xhr.status : "no connection")
        return
      }
      var parsed = null
      try { parsed = JSON.parse(xhr.responseText) }
      catch (e) { if (onErr) onErr("bad JSON from " + path); return }
      onOk(parsed)
    }
    try { xhr.open("GET", base + path); xhr.send() }
    catch (e) { if (onErr) onErr(String(e)) }
  }

  // A slow earlier request must not overwrite a newer one's result: the range
  // buttons are the fastest way to get two in flight at once.
  property int loadSeq: 0
  property int detailSeq: 0

  // `/api/live` is newer than the rest of the API. A busywatch that predates it
  // answers 404, and the same snapshot is embedded in `/api/overview` — so fall
  // back to the expensive request rather than show a dead dot against a daemon
  // that is running perfectly well. One probe decides it, not every poll.
  property bool liveEndpoint: true

  function loadLive() {
    if (!liveEndpoint) { loadLiveFromOverview(); return }
    get("/api/live",
      function (d) { live = d; online = true; lastError = "" },
      function (e) {
        if (String(e).indexOf("404") !== -1) {
          liveEndpoint = false
          loadLiveFromOverview()
          return
        }
        online = false
        lastError = e
      })
  }

  // The narrowest overview that still carries `live`: one bucket, one app.
  function loadLiveFromOverview() {
    get("/api/overview?span=900&points=20&limit=1",
      function (d) {
        if (d && d.live) live = d.live
        online = true
        lastError = ""
      },
      function (e) { online = false; lastError = e })
  }

  function loadOverview() {
    var seq = ++loadSeq
    var q = "?span=" + span + "&points=" + points + "&metric=" + metric + "&limit=8"
    get("/api/overview" + q,
      function (d) {
        if (seq !== loadSeq) return
        overview = d
        if (d && d.live) live = d.live
        online = true
        lastError = ""
        dataArrived()
      },
      function (e) {
        if (seq !== loadSeq) return
        online = false
        lastError = e
      })
  }

  function loadDetail(comm) {
    if (!comm) { detail = null; return }
    var seq = ++detailSeq
    var q = "?comm=" + encodeURIComponent(comm) + "&span=" + span + "&points=" + points
    get("/api/proc" + q,
      function (d) {
        if (seq !== detailSeq) return
        // The answer to a question nobody is asking any more.
        if (!d || d.comm !== comm || selected !== comm) return
        detail = d
        detailArrived()
      },
      function (e) { /* leaves the previous drilldown standing */ })
  }

  // ------------------------------------------------------------------ intents
  function setSpan(s) {
    if (span === s) return
    span = s
    loadOverview()
    if (selected) loadDetail(selected)
  }

  function setMetric(m) {
    if (metric === m) return
    metric = m
    loadOverview()
  }

  function select(comm) {
    if (!comm) { selected = ""; detail = null; return }
    selected = comm
    loadDetail(comm)
  }

  function refresh() {
    loadLive()
    if (windowOpen) loadOverview()
  }

  // The bar keeps its own slow beat whether or not the window exists. The
  // window's faster one is only wound while it is on screen — and stops while
  // it is paused, which is what the pause button is for.
  Timer {
    interval: Math.max(2, root.barIntervalSec) * 1000
    running: true
    repeat: true
    triggeredOnStart: true
    onTriggered: root.loadLive()
  }

  Timer {
    interval: Math.max(2, root.windowIntervalSec) * 1000
    running: root.windowOpen && !root.paused
    repeat: true
    onTriggered: root.loadOverview()
  }

  // A window that opens onto a dead server must not sit blank: keep trying,
  // slowly, and the moment it answers the charts fill in.
  Timer {
    interval: 3000
    running: root.windowOpen && !root.online
    repeat: true
    onTriggered: root.loadOverview()
  }
}
