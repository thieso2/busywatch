import QtQuick
import QtTest
import "../Charge.js" as Charge

// The charging readout. Everything here is a division somebody will read as a
// promise — "full in 1h50m" — so the guards against dividing by a gap, by a
// stale bucket or by a rate pointing the wrong way are the point of the file.
TestCase {
  name: "Charge"

  // A 52Wh pack, half full, taking 18W on the adapter.
  function live(over) {
    var l = {
      batPct: 37, batStatus: "Charging", batPowerUw: 18000000,
      batEnergyUwh: 19290000, batEnergyFullUwh: 52000000,
      batEnergyDesignUwh: 52000000, batVoltageUv: 11821000,
      batCycles: 2, acOnline: 1, pdMode: "usb_power_delivery", chargerMaxUw: null
    }
    for (var k in over) l[k] = over[k]
    return l
  }

  // Buckets a minute apart, energy climbing by `perMin` microwatt-hours.
  function rows(n, perMin, from) {
    var out = []
    for (var i = 0; i < n; i++)
      out.push({ t: from + i * 60, batE: 19000000 + i * perMin })
    return out
  }

  function test_no_battery_means_no_readout() {
    compare(Charge.state(null, [], null), null)
    compare(Charge.state({ batPct: null }, [], null), null)
    compare(Charge.tiles(null).length, 0)
  }

  function test_the_slope_needs_two_points_a_span_and_a_fresh_tail() {
    var ov = { bucket: 60, now: 1000 + 20 * 60 }
    // Twenty minutes of climbing energy: a real slope.
    var h = Charge.slope(rows(21, 100000, 1000), ov)
    verify(h !== null)
    // 100000 uWh per 60s.
    fuzzyCompare(h.rate, 100000 / 60, 1)
    // One point is not a slope.
    compare(Charge.slope(rows(1, 100000, 1000), ov), null)
    // A tail that stopped an hour ago is stale — the laptop slept through it.
    compare(Charge.slope(rows(21, 100000, 1000), { bucket: 60, now: 1000 + 20 * 60 + 4000 }), null)
  }

  function test_a_gap_in_the_energy_column_stops_the_slope_rather_than_spanning_it() {
    // Rows written before the column existed read null, and the difference
    // across such a gap is not a rate.
    var r = rows(21, 100000, 1000)
    r[15].batE = null
    // Only five usable points remain at the tail: 5 * 60s = 300s, which is
    // under the ten minutes asked for but over the two-minute floor.
    var h = Charge.slope(r, { bucket: 60, now: 1000 + 20 * 60 })
    verify(h !== null)
    fuzzyCompare(h.rate, 100000 / 60, 1)
    // Cut it to one point past the gap and there is nothing left to divide.
    r = rows(21, 100000, 1000)
    for (var i = 0; i < 19; i++) r[i].batE = null
    compare(Charge.slope(r, { bucket: 60, now: 1000 + 20 * 60 }), null)
  }

  function test_the_measured_slope_beats_the_instantaneous_watts() {
    var ov = { bucket: 60, now: 1000 + 20 * 60 }
    // 100000 uWh/min is 6W, not the 18W showing on the terminals right now.
    var c = Charge.state(live({}), rows(21, 100000, 1000), ov)
    verify(c.charging)
    // The window stops at ten minutes: `want` is max(600, two buckets), and
    // the walk back ends the moment it has that much.
    compare(c.etaFrom, "measured over the last 10m")
    // 52.0 - 19.29 = 32.71Wh to go at 6W is a little under five and a half hours.
    fuzzyCompare(c.etaSecs / 3600, 32710000 / 6000000, 0.02)
  }

  function test_a_slope_pointing_the_wrong_way_is_refused_not_averaged() {
    // The charger went in partway through this range, so the tail's slope is
    // a discharge while the battery is now filling.
    var ov = { bucket: 60, now: 1000 + 20 * 60 }
    var c = Charge.state(live({}), rows(21, -100000, 1000), ov)
    compare(c.etaFrom, "at the rate right now")
    // Falls back to 18W: 32.71Wh at 18W is about 1h49m.
    fuzzyCompare(c.etaSecs / 3600, 32710000 / 18000000, 0.02)
  }

  function test_with_no_history_at_all_the_instantaneous_rate_still_answers() {
    var c = Charge.state(live({}), [], null)
    compare(c.etaFrom, "at the rate right now")
    verify(c.etaSecs > 0)
  }

  function test_discharging_counts_down_to_empty_not_to_full() {
    var c = Charge.state(live({ batStatus: "Discharging", batPowerUw: -12000000 }), [], null)
    verify(!c.charging)
    // The whole 19.29Wh in the pack at 12W, not the 32.71Wh of headroom.
    fuzzyCompare(c.etaSecs / 3600, 19290000 / 12000000, 0.02)
    compare(Charge.tiles(c)[0].label, "drawing")
    compare(Charge.tiles(c)[1].label, "empty in")
  }

  function test_a_battery_held_at_its_limit_reads_as_charging_but_idle() {
    // Zero watts with a "Charging" status is what a charge threshold looks
    // like; calling that "drawing" would be a lie in the other direction.
    var c = Charge.state(live({ batPowerUw: 0 }), [], null)
    verify(c.idle)
    verify(c.charging)
    compare(c.etaSecs, null)
    compare(Charge.tiles(c)[0].value, "idle")
    compare(Charge.tiles(c)[0].label, "battery power")
  }

  function test_health_is_what_is_left_of_what_the_pack_held_new() {
    var c = Charge.state(live({ batEnergyFullUwh: 44200000 }), [], null)
    fuzzyCompare(c.health, 85, 0.1)
    var health = Charge.tiles(c)[3]
    compare(health.label, "battery health")
    compare(health.value, "85%")
    compare(health.sub, "44.2 of 52.0Wh new · 2 cycles")
    // A battery with no design figure gets no health tile rather than a 100%.
    c = Charge.state(live({ batEnergyDesignUwh: null }), [], null)
    compare(c.health, null)
    for (var i = 0; i < Charge.tiles(c).length; i++)
      verify(Charge.tiles(c)[i].label !== "battery health")
  }

  function test_the_charger_tile_says_so_when_nothing_reports_a_rating() {
    // The common case: firmware that never passes the PD source capabilities
    // up to the kernel. Saying "not reported" is the whole point of the tile.
    var t = Charge.tiles(Charge.state(live({}), [], null))
    var charger = t[t.length - 1]
    compare(charger.label, "charger")
    compare(charger.value, "not reported")
    compare(charger.sub, "USB-C, PD contract negotiated")

    t = Charge.tiles(Charge.state(live({ chargerMaxUw: 65000000 }), [], null))
    compare(t[t.length - 1].value, "65.0W")
  }

  function test_the_mode_is_spelled_out_because_the_bare_string_says_nothing() {
    compare(Charge.pdModeText("3.0A"), "USB-C at 3.0A, no PD contract")
    compare(Charge.pdModeText("default"), "USB-C at the default 5V, nothing negotiated")
    compare(Charge.pdModeText(null), "no USB-C port reporting")
  }

  function test_the_title_names_what_the_battery_is_doing() {
    compare(Charge.title(null), "Battery")
    compare(Charge.title(Charge.state(live({}), [], null)), "Charging")
    compare(Charge.title(Charge.state(live({ batPowerUw: -9000000,
      batStatus: "Discharging" }), [], null)), "On battery")
    compare(Charge.title(Charge.state(live({ batPowerUw: 0,
      batStatus: "Full" }), [], null)), "Battery full")
  }
}
