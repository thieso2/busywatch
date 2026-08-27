import QtQuick
import qs.Commons

// The colours the charts are drawn in.
//
// Two kinds live here and they are governed differently. The *semantic* ones —
// cpu, mem, io and the rest — are fixed, because they are the program's own
// vocabulary: the same red means CPU in the toast, in the tray icon, on the
// website and here, and a theme is not entitled to reassign that. The
// *structural* ones — grid lines, hover fills, the colour a break is cut down
// to — are derived from the active Omarchy theme, because they are the surface
// the series are drawn on and they must belong to it.
//
// Both are picked for a light or a dark ground, which is the one thing the
// theme does get to decide.
QtObject {
  id: root

  // Themes carry no light/dark flag, so the ground itself is asked.
  readonly property bool dark: Color.background.hslLightness < 0.5

  readonly property color cpu:  dark ? "#ff7a5e" : "#e0503a"
  readonly property color mem:  dark ? "#63a8ff" : "#3f7fd6"
  readonly property color io:   dark ? "#e8b552" : "#c88a1d"
  readonly property color load: dark ? "#a48bff" : "#7a5bd0"
  readonly property color swap: dark ? "#d97a5c" : "#b0563f"
  readonly property color freq: dark ? "#5fc79e" : "#2f8f6f"
  readonly property color bat:  dark ? "#c8cf5e" : "#8a8f3a"
  readonly property color draw: dark ? "#ff9457" : "#b5502a"
  readonly property color chg:  dark ? "#4cc38a" : "#2f9e6e"
  readonly property color temp: dark ? "#ff8fab" : "#b83b5e"
  readonly property color fan:  dark ? "#4fc3d9" : "#1f7a8c"

  // The stack chart's series colours, in order. Eight, because that is how many
  // apps the server charts, and they have to stay apart at 14% opacity when one
  // of them is selected and the rest are dimmed.
  //
  // Declared as `color` properties rather than as strings in a list: the chart
  // reads .r/.g/.b off these to build a translucent fill, and a list literal
  // holds the strings uncoerced — every band comes out black.
  readonly property color c0: dark ? "#63a8ff" : "#3f7fd6"
  readonly property color c1: dark ? "#ff7a5e" : "#e0503a"
  readonly property color c2: dark ? "#4cc38a" : "#2f9e6e"
  readonly property color c3: dark ? "#e8b552" : "#c88a1d"
  readonly property color c4: dark ? "#a48bff" : "#7a5bd0"
  readonly property color c5: dark ? "#3fc4c9" : "#159ba0"
  readonly property color c6: dark ? "#ff86ab" : "#d0567f"
  readonly property color c7: dark ? "#93a1b0" : "#7c8a99"
  readonly property var series: [c0, c1, c2, c3, c4, c5, c6, c7]

  function forIndex(i) { return series[i % series.length] }

  // An incident is drawn in the colour of the resource that raised it.
  function forKind(kind) {
    return kind === "mem" ? mem : kind === "io" ? io : cpu
  }

  // --------------------------------------------------------------- structure
  readonly property color ink: Color.foreground
  readonly property color ground: Color.background
  // Secondary text. Mixed toward the ground rather than darkened: on a light
  // theme darkening an almost-black foreground makes labels heavier than the
  // body text they are subordinate to.
  readonly property color dim: Qt.rgba(
    ink.r * 0.58 + ground.r * 0.42,
    ink.g * 0.58 + ground.g * 0.42,
    ink.b * 0.58 + ground.b * 0.42, 1)
  readonly property color line: Qt.rgba(ink.r, ink.g, ink.b, dark ? 0.14 : 0.12)
  readonly property color grid: Qt.rgba(ink.r, ink.g, ink.b, dark ? 0.09 : 0.07)
  readonly property color hover: Qt.rgba(ink.r, ink.g, ink.b, dark ? 0.07 : 0.05)
  // Panels sit slightly off the window ground, the way the sections did on the
  // page — enough to read as cards, not enough to become a second background.
  readonly property color panel: Qt.rgba(
    ground.r + (dark ? 0.028 : -0.012),
    ground.g + (dark ? 0.028 : -0.012),
    ground.b + (dark ? 0.028 : -0.012), 1)
}
