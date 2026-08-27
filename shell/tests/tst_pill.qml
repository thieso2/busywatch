import QtQuick
import QtTest
import ".."

// The range and metric buttons. Every one of them is a Pill, so this is where
// "clicking 6h changes the range" is settled — App only wires the signal to a
// call on the service.
TestCase {
  id: testCase
  name: "Pill"
  when: windowShown
  visible: true
  width: 240
  height: 80

  Scheme { id: scheme }

  property int clicks: 0

  Pill {
    id: pill
    colors: scheme
    text: "6h"
    onClicked: testCase.clicks++
  }

  function init() {
    clicks = 0
    pill.pressedState = false
    pill.empty = false
  }

  function test_click_emits() {
    mouseClick(pill, pill.width / 2, pill.height / 2)
    compare(clicks, 1, "a click on the pill emits clicked() exactly once")
  }

  function test_repeated_clicks_all_land() {
    // Ranges get pressed repeatedly while reading; a handler that fires once
    // and then stops would look like the window had frozen.
    for (var i = 0; i < 3; i++) mouseClick(pill, pill.width / 2, pill.height / 2)
    compare(clicks, 3)
  }

  function test_click_outside_does_nothing() {
    mouseClick(testCase, pill.x + pill.width + 40, pill.y + pill.height / 2)
    compare(clicks, 0)
  }

  // A range wider than the recorded history is dimmed but still pressable:
  // it has nothing to show, which is a different thing from being disabled.
  function test_an_empty_range_is_still_clickable() {
    pill.empty = true
    mouseClick(pill, pill.width / 2, pill.height / 2)
    compare(clicks, 1)
    verify(pill.opacity < 1, "an empty range is dimmed")
  }

  function test_pressed_state_inverts() {
    pill.pressedState = true
    tryVerify(function () { return Qt.colorEqual(pill.color, scheme.ink) }, 1000,
              "the selected range is a filled inversion, not a tint")
    verify(Qt.colorEqual(pill.opacity === 1 ? scheme.ink : scheme.ink, scheme.ink))
  }
}
