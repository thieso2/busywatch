import QtQuick
import QtTest
import ".."
import "../Charge.js" as Charge

// The charging card, stood up for real. Charge.js is checked on its own; this
// is here because a binding that throws leaves a card that is simply blank,
// and a blank card looks exactly like a machine with no battery.
TestCase {
  name: "ChargePanel"
  when: windowShown
  visible: true
  width: 1200
  height: 300

  Scheme { id: scheme }

  Component {
    id: panelC
    ChargePanel {
      colors: scheme
      // The width the history window actually opens at, less its margins:
      // the column count is a function of it, so a made-up width would test
      // a layout nobody sees.
      width: 1128
    }
  }

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

  function tilesOf(panel) {
    var flow = findChild(panel, "tiles")
    verify(flow !== null)
    var out = []
    for (var i = 0; i < flow.children.length; i++) {
      var c = flow.children[i]
      if (c.label !== undefined) out.push(c)
    }
    return out
  }

  function test_a_machine_with_no_battery_gets_no_card_at_all() {
    var p = createTemporaryObject(panelC, this, { charge: null })
    verify(!p.visible)
    compare(tilesOf(p).length, 0)
  }

  function test_the_charging_case_draws_every_tile_with_its_figure() {
    var p = createTemporaryObject(panelC, this,
      { charge: Charge.state(live({}), [], null) })
    verify(p.visible)
    var t = tilesOf(p)
    compare(t.length, 5)
    compare(t[0].label, "charging at")
    compare(t[0].value, "18.0W")
    compare(t[0].sub, "1.52A at 11.82V")
    compare(t[1].label, "full in")
    compare(t[2].value, "19.3Wh")
    compare(t[3].value, "100%")
    compare(t[4].value, "not reported")
    // All five across the real card width: the row is divided by however many
    // tiles fit, so none is left stranded on a line of its own.
    compare(p.cols, 5)
    verify(Math.abs(t[0].width - t[4].width) < 1)
    verify(t[0].width * 5 < 1128)
  }

  function test_a_narrow_card_wraps_to_whole_rows_rather_than_squeezing() {
    var p = createTemporaryObject(panelC, this,
      { charge: Charge.state(live({}), [], null), width: 380 })
    // 380px takes two 170px tiles, not five slivers.
    compare(p.cols, 2)
    verify(tilesOf(p)[0].width >= 170)
  }

  function test_the_note_owns_up_to_the_unknown_charger_and_drops_it_when_known() {
    var p = createTemporaryObject(panelC, this,
      { charge: Charge.state(live({}), [], null) })
    var note = findChild(p, "note")
    verify(note.text.indexOf("battery terminals") >= 0)
    verify(note.text.indexOf("does not pass those to the kernel") >= 0)

    p.charge = Charge.state(live({ chargerMaxUw: 65000000 }), [], null)
    verify(note.text.indexOf("battery terminals") >= 0)
    compare(note.text.indexOf("does not pass those to the kernel"), -1)
    compare(tilesOf(p)[4].value, "65.0W")
  }

  function test_a_battery_draining_says_so_in_the_heading() {
    var p = createTemporaryObject(panelC, this, { charge: Charge.state(
      live({ batPowerUw: -12000000, batStatus: "Discharging", acOnline: 0 }), [], null) })
    verify(p.visible)
    compare(tilesOf(p)[0].label, "drawing")
    compare(tilesOf(p)[1].label, "empty in")
  }
}
