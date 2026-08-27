pragma Singleton
import QtQuick

// The part of Omarchy's Style singleton these components actually read.
//
// The real one is not loadable outside omarchy-shell — it reaches for
// Quickshell's own types for the config file and the theme — so the tests take
// this instead. It is deliberately the smallest thing that satisfies the
// bindings, at scale 1, so a test measuring a widget gets round numbers.
QtObject {
  function space(px) { return Math.max(1, Math.round(px)) }
  function spaceReal(px) { return px }

  readonly property QtObject font: QtObject {
    readonly property string family: "monospace"
    readonly property int caption: 10
    readonly property int bodySmall: 11
    readonly property int body: 12
    readonly property int subtitle: 13
    readonly property int title: 14
    readonly property int heading: 16
    readonly property int display: 24
  }
}
