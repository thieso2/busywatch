// Number formatting, ported from the web UI verbatim.
//
// These are the units the history speaks — KiB for memory, kHz for the clock,
// microwatts for the battery, millidegrees for the die — and every one of them
// is what the kernel handed over, so the conversion belongs at the last moment
// before a person reads it rather than in the database.
.pragma library

function pad(n) { return String(n).padStart(2, "0") }

// Input: KiB. Steps at 1000 rather than 1024 because the figure is being read,
// not allocated.
function bytes(kb) {
  var v = kb * 1024, u = ["B", "KB", "MB", "GB", "TB"], i = 0
  while (v >= 1000 && i < u.length - 1) { v /= 1000; i++ }
  return (i ? v.toFixed(v < 10 ? 1 : 0) : v.toFixed(0)) + u[i]
}

function rate(b) { return bytes(b / 1024) + "/s" }

function pct(v) { return (v >= 10 ? v.toFixed(0) : v.toFixed(1)) + "%" }

// kHz in, because that is what the kernel and the history both speak.
function ghz(k) {
  return k >= 1e6 ? (k / 1e6).toFixed(1) + "GHz" : Math.round(k / 1000) + "MHz"
}

function watts(uw) { return (Math.abs(uw) / 1e6).toFixed(1) + "W" }

// Microwatt-hours in, because that is what the battery counters and the
// history both speak. A pack is a two-figure number of watt-hours, so one
// decimal is the whole useful range of the reading.
function wattHours(uwh) { return (Math.abs(uwh) / 1e6).toFixed(1) + "Wh" }

// Two watt-hour figures that share a unit — "44.2 of 52.0Wh". Spelling the
// unit twice costs a tile's width for nothing, and the sub-line under a tile
// is where the room runs out first.
function wattHoursPair(a, b) {
  return (Math.abs(a) / 1e6).toFixed(1) + " of " + wattHours(b)
}

// Watts and volts, both as the kernel's micro-units, out as the current the
// charger is actually pushing. Amps are what a charger is argued about in.
function amps(uw, uv) {
  if (!uv) return null
  return (Math.abs(uw) / Math.abs(uv)).toFixed(2) + "A"
}

// Microvolts in.
function volts(uv) { return (uv / 1e6).toFixed(2) + "V" }

// Millidegrees in, because that is what hwmon and the history both speak.
function degC(mc) { return (mc / 1000).toFixed(0) + "°C" }

// Fan speeds run to four figures; "5.4k" beats "5400" on a cramped axis.
function krpm(v) { return v >= 1000 ? (v / 1000).toFixed(1) + "k" : v.toFixed(0) }

function clock(t) {
  var d = new Date(t * 1000)
  return pad(d.getHours()) + ":" + pad(d.getMinutes())
}

function stamp(t) {
  var d = new Date(t * 1000)
  return d.getFullYear() + "-" + pad(d.getMonth() + 1) + "-" + pad(d.getDate()) +
    " " + pad(d.getHours()) + ":" + pad(d.getMinutes())
}

function dur(s) {
  if (s < 0) s = 0
  if (s >= 86400) return Math.floor(s / 86400) + "d" + pad(Math.floor(s % 86400 / 3600)) + "h"
  if (s >= 3600) return Math.floor(s / 3600) + "h" + pad(Math.floor(s % 3600 / 60)) + "m"
  if (s >= 60) return Math.floor(s / 60) + "m" + pad(s % 60) + "s"
  return s + "s"
}

function ago(t) {
  var d = Math.round(Date.now() / 1000) - t
  return d < 90 ? "just now" : dur(d) + " ago"
}

// "8h", "22m", "2d" — a break is a dozen pixels wide, and the exact figure is
// a hover away.
function briefDur(s) {
  return s >= 86400 ? Math.round(s / 86400) + "d"
    : s >= 3600 ? Math.round(s / 3600) + "h"
    : Math.max(1, Math.round(s / 60)) + "m"
}

// The three metrics the app chart can stack, each in its own unit.
function forMetric(metric) {
  return metric === "cpu" ? pct : metric === "io" ? rate : bytes
}
