// What the battery counters mean, once somebody is looking at them.
//
// The daemon records what the kernel handed over and nothing more: microwatts
// across the terminals, microwatt-hours in the pack, the Type-C mode the port
// negotiated. None of that is what a person wants to read. "Full in 1h50m" is
// a division that only makes sense at the moment of asking, so it lives here
// rather than in the database — the same reason Format.js converts units at
// the last moment.
//
// Every function takes nulls for granted. A desktop has no battery, a VM has
// no power supplies, and a database written before these columns existed has
// the columns but no values in them; all three must come out as "nothing to
// say" rather than as a zero.
.pragma library

.import "Format.js" as Fmt

// How fast the pack is really filling or emptying, in microwatt-hours per
// second, measured across the tail of the recorded history instead of read off
// the instantaneous watts.
//
// The instantaneous figure swings with whatever the machine is doing this
// second — one compile starting is enough to turn "empty in 4h" into "empty in
// 40m". A slope across the last stretch of buckets is the estimate that
// survives that.
//
// Null unless the tail is recent, unbroken and long enough to divide by: a
// laptop that just woke has a last bucket from before it slept, and the energy
// difference across that gap is not a rate anybody wants shown.
function slope(rows, overview) {
  if (!overview || !rows || !rows.length) return null
  var bucket = overview.bucket || 60
  var want = Math.max(600, bucket * 2)
  var pts = []
  for (var i = rows.length - 1; i >= 0; i--) {
    var e = rows[i].batE
    if (e === null || e === undefined) break
    pts.push(rows[i])
    if (pts[0].t - rows[i].t >= want) break
  }
  if (pts.length < 2) return null
  var last = pts[0], first = pts[pts.length - 1]
  if (overview.now - last.t > bucket * 2 + 300) return null
  var dt = last.t - first.t
  if (dt < Math.max(120, bucket)) return null
  return { rate: (last.batE - first.batE) / dt, span: dt }
}

function _num(v) { return (v === null || v === undefined) ? null : v }

// The whole charging story as one object, or null on a machine with no
// battery. `live` is the daemon's live block; `rows` and `overview` are the
// history the estimate is measured across, and may be empty.
function state(live, rows, overview) {
  if (!live || _num(live.batPct) === null) return null
  var st = String(live.batStatus || "").toLowerCase()
  var uw = _num(live.batPowerUw) || 0
  var now = _num(live.batEnergyUwh), full = _num(live.batEnergyFullUwh)
  var design = _num(live.batEnergyDesignUwh)
  // A battery sitting at its charge limit reports zero watts and "Charging",
  // so the status breaks the tie the sign cannot.
  var charging = uw > 0 || (uw === 0 && st === "charging")
  var togo = charging ? (full === null || now === null ? null : full - now) : now

  // Prefer the measured slope, but only while it agrees with which way the
  // battery is going: a range whose tail straddles the moment the charger went
  // in has a slope that means nothing here.
  var eta = null, etaFrom = null
  var h = slope(rows, overview)
  if (h && togo !== null && togo > 0 && ((charging && h.rate > 0) || (!charging && h.rate < 0))) {
    eta = togo / Math.abs(h.rate)
    etaFrom = "measured over the last " + Fmt.briefDur(h.span)
  } else if (uw !== 0 && togo !== null && togo > 0) {
    eta = togo / Math.abs(uw) * 3600
    etaFrom = "at the rate right now"
  }

  return {
    charging: charging,
    idle: uw === 0,
    status: st,
    acOnline: live.acOnline === 1,
    pct: live.batPct,
    uw: uw,
    uv: _num(live.batVoltageUv),
    now: now,
    full: full,
    design: design,
    cycles: _num(live.batCycles),
    health: (full !== null && design) ? full / design * 100 : null,
    etaSecs: eta,
    etaFrom: etaFrom,
    pdMode: live.pdMode || null,
    chargerUw: _num(live.chargerMaxUw)
  }
}

// What the negotiated Type-C mode means, in words. The mode is the difference
// between a laptop charging and a laptop losing ground on a phone charger, and
// "3.0A" on its own does not say that to anybody.
function pdModeText(mode) {
  if (mode === "usb_power_delivery") return "USB-C, PD contract negotiated"
  if (mode === "1.5A") return "USB-C at 1.5A, no PD contract"
  if (mode === "3.0A") return "USB-C at 3.0A, no PD contract"
  if (mode === "default") return "USB-C at the default 5V, nothing negotiated"
  return "no USB-C port reporting"
}

// The figures worth a tile. Each is dropped rather than drawn blank when the
// machine does not report what it needs — except the charger, which is shown
// empty-handed on purpose: "we cannot tell" is the answer people come looking
// for, and a missing tile reads as an oversight instead.
function tiles(c) {
  if (!c) return []
  var t = []

  t.push({ label: c.idle ? "battery power" : (c.charging ? "charging at" : "drawing"),
           value: c.idle ? (c.status === "full" ? "full" : "idle") : Fmt.watts(c.uw),
           sub: (c.uv !== null && c.uw)
             ? Fmt.amps(c.uw, c.uv) + " at " + Fmt.volts(c.uv) : "" })

  if (c.etaSecs !== null)
    t.push({ label: c.charging ? "full in" : "empty in",
             value: Fmt.dur(Math.round(c.etaSecs)),
             sub: c.etaFrom })

  if (c.now !== null && c.full !== null)
    t.push({ label: "in the pack", value: Fmt.wattHours(c.now),
             sub: "of " + Fmt.wattHours(c.full) + " full" })

  if (c.health !== null) {
    var hs = Fmt.wattHoursPair(c.full, c.design) + " new"
    if (c.cycles !== null) hs += " · " + c.cycles + " cycles"
    t.push({ label: "battery health", value: c.health.toFixed(0) + "%", sub: hs })
  }

  t.push({ label: "charger",
           value: c.chargerUw !== null ? Fmt.watts(c.chargerUw) : "not reported",
           sub: pdModeText(c.pdMode) })
  return t
}

// The heading over the tiles, which is the one-word answer to "what is the
// battery doing".
function title(c) {
  if (!c) return "Battery"
  if (c.idle) return c.status === "full" ? "Battery full" : "Battery"
  return c.charging ? "Charging" : "On battery"
}
