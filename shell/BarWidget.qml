import QtQuick
import qs.Commons
import qs.Ui

// One dot in the bar, in the colour of whatever is stalling.
//
// The same vocabulary the toasts and the tray icon use: quiet while nothing is
// over its threshold, and the colour of the resource when something is. It is
// deliberately the smallest thing that can carry a verdict — the history is one
// click away and that is where the answer lives.
//
// busywatch also registers a StatusNotifierItem, which the bar's tray widget
// picks up on its own. Two indicators for one program is one too many: run the
// daemon with --no-tray when this widget is in the bar, or leave this out of
// the bar and keep the tray icon.
BarWidget {
  id: root

  moduleName: "busywatch"

  readonly property var service: bar && bar.shell
    ? bar.shell.serviceFor("busywatch") : null

  readonly property color foreground: bar ? bar.barForeground : Color.foreground

  // The shell hands plugin settings to the bar widget rather than to the
  // service, so the widget is what pushes them across.
  function pushSettings() {
    if (service && typeof service.applySettings === "function") service.applySettings(settings)
  }
  onSettingsChanged: pushSettings()
  onServiceChanged: pushSettings()
  Component.onCompleted: pushSettings()

  function openWindow() {
    if (!bar || !bar.shell) return
    if (typeof bar.shell.toggle === "function") bar.shell.toggle("busywatch", "{}")
    else if (typeof bar.shell.summon === "function") bar.shell.summon("busywatch", "{}")
  }

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  Scheme { id: pal }

  BarIconButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    tooltipText: root.service ? root.service.barTooltip : "busywatch"

    // Read from inside `iconComponent`, where `root` would be ambiguous: both
    // BarIconButton and the delegate name their own root object.
    readonly property string verdict: root.service ? root.service.verdict : "down"
    readonly property color dotColor: verdict === "cpu" ? pal.cpu
      : verdict === "mem" ? pal.mem
      : verdict === "io" ? pal.io
      : verdict === "ok" ? pal.chg
      : root.foreground
    readonly property bool down: verdict === "down"
    readonly property bool quiet: verdict === "ok"
    readonly property bool windowOpen: !!root.service && root.service.windowOpen

    active: windowOpen

    iconComponent: Component {
      Item {
        Rectangle {
          anchors.centerIn: parent
          width: Style.space(11)
          height: width
          radius: width / 2
          // Quiet is a ring, not a disc: a filled dot every minute of every day
          // stops being a signal. Something stalling fills it in.
          color: button.quiet || button.down ? "transparent" : button.dotColor
          border.width: button.quiet || button.down ? Math.max(1, Style.space(2)) : 0
          border.color: button.dotColor
          opacity: button.down ? 0.4 : 1

          Behavior on color { ColorAnimation { duration: 250 } }
        }

        // Not responding is a different state from nothing wrong, and has to
        // look like one: the ring is struck through rather than simply dimmed.
        Rectangle {
          visible: button.down
          anchors.centerIn: parent
          width: Style.space(15)
          height: Math.max(1, Style.space(1))
          rotation: -45
          color: button.dotColor
          opacity: 0.55
        }
      }
    }

    onPressed: function (buttonCode) {
      if (buttonCode === Qt.RightButton) {
        // A reading on demand, for when the dot says something changed and the
        // next scheduled poll is seconds away.
        if (root.service) root.service.refresh()
        return
      }
      root.openWindow()
    }
  }
}
