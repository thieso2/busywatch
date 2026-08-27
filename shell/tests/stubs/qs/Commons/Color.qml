pragma Singleton
import QtQuick

// The part of Omarchy's Color singleton these components actually read. A dark
// ground, because Scheme picks its palette from the background's lightness and
// the dark branch is the one the screenshots were checked against.
QtObject {
  readonly property color foreground: "#e6e8ec"
  readonly property color background: "#181b21"
  readonly property color accent: "#63a8ff"
  readonly property color urgent: "#ff7a5e"
  readonly property color muted: "#98a0ad"

  readonly property QtObject tooltip: QtObject {
    readonly property color background: "#181b21"
    readonly property color text: "#e6e8ec"
    readonly property color border: "#3a4049"
  }
}
