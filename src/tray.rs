//! A StatusNotifierItem tray icon.
//!
//! The icon is the watcher's state at a glance: a calm dot while nothing is
//! wrong, and the colour of whichever resource is stalling while an incident
//! runs. Clicking it opens the history overlay, the same destination a toast
//! click reaches.
//!
//! Hosts are told about changes through `NewIcon`/`NewStatus`/`NewToolTip`
//! signals, which carry no payload — the host re-reads the properties itself.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::db::Kind;
use crate::dbus::{Conn, Dec, Enc, Msg, Tx, ERROR, METHOD_CALL, SIGNAL};
use crate::util::log;

const ITEM_PATH: &str = "/StatusNotifierItem";
const ITEM_IFACE: &str = "org.kde.StatusNotifierItem";
const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";

/// What the icon is currently saying.
#[derive(Clone, Default)]
pub struct State {
    /// The worst incident running, or None when all is quiet.
    pub alarm: Option<Kind>,
    /// One line for the tooltip body — current pressures and the culprit.
    pub detail: String,
}

struct Shared {
    state: State,
    /// The URL a click should open, if the web UI is running.
    url: Option<String>,
}

/// Handle held by the watch loop. Dropping it does not stop the tray thread;
/// the process exiting does.
#[derive(Clone)]
pub struct Tray {
    shared: Arc<Mutex<Shared>>,
    tx: Arc<Mutex<Tx>>,
    /// False once the bus connection dies, so the watch loop stops paying for
    /// signal sends nobody receives.
    alive: Arc<AtomicBool>,
}

impl Tray {
    /// Publish new state and tell the host what changed. Cheap and silent
    /// when nothing actually moved — the watch loop calls this every tick.
    pub fn update(&self, state: State) {
        if !self.alive.load(Ordering::Relaxed) {
            return;
        }
        let (icon_changed, tip_changed) = {
            let Ok(mut sh) = self.shared.lock() else { return };
            let changed = (sh.state.alarm != state.alarm, sh.state.detail != state.detail);
            sh.state = state;
            changed
        };
        let mut sigs: Vec<Msg> = Vec::new();
        if icon_changed {
            sigs.push(Msg::signal(ITEM_PATH, ITEM_IFACE, "NewIcon"));
            sigs.push(Msg::signal(ITEM_PATH, ITEM_IFACE, "NewStatus").with_body(
                "s",
                {
                    let mut e = Enc::new();
                    e.string(status_str(self.shared.lock().ok().and_then(|s| s.state.alarm)));
                    e.buf
                },
            ));
        }
        if tip_changed {
            sigs.push(Msg::signal(ITEM_PATH, ITEM_IFACE, "NewToolTip"));
        }
        if sigs.is_empty() {
            return;
        }
        let Ok(mut tx) = self.tx.lock() else { return };
        for s in sigs {
            if tx.send(s).is_err() {
                self.alive.store(false, Ordering::Relaxed);
                return;
            }
        }
    }

    pub fn set_url(&self, url: Option<String>) {
        if let Ok(mut sh) = self.shared.lock() {
            sh.url = url;
        }
    }
}

fn status_str(alarm: Option<Kind>) -> &'static str {
    if alarm.is_some() {
        "NeedsAttention"
    } else {
        "Active"
    }
}

/// Start the tray on its own thread. Returns None when there is no session
/// bus or no host willing to take the item — both are ordinary on a headless
/// box, and neither is worth failing the process over.
pub fn start(url: Option<String>, on_click: impl Fn(Option<String>) + Send + 'static) -> Option<Tray> {
    let mut conn = match Conn::session() {
        Some(c) => c,
        None => {
            log("tray: no session bus — running without a tray icon");
            return None;
        }
    };

    // The well-known name a host looks for. The pid keeps it unique when two
    // busywatch instances run, which happens while testing a new build.
    let name = format!("org.kde.StatusNotifierItem-{}-1", std::process::id());
    if let Err(e) = conn.request_name(&name) {
        log(&format!("tray: cannot own {name}: {e}"));
        return None;
    }

    // Ask to hear when the watcher comes and goes, so a bar restart — which
    // on a tiling desktop happens constantly — gets the icon back.
    let mut e = Enc::new();
    e.string(&format!(
        "type='signal',interface='org.freedesktop.DBus',member='NameOwnerChanged',arg0='{WATCHER_NAME}'"
    ));
    let _ = conn.call_sync(
        Msg::call(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "AddMatch",
        )
        .with_body("s", e.buf),
    );

    let shared = Arc::new(Mutex::new(Shared { state: State::default(), url }));
    let tray = Tray {
        shared: Arc::clone(&shared),
        tx: conn.sender(),
        alive: Arc::new(AtomicBool::new(true)),
    };
    let alive = Arc::clone(&tray.alive);
    let unique = conn.unique.clone();
    let unique_for_thread = unique.clone();

    // The reader starts BEFORE registering, and registration is *sent* rather
    // than called-and-awaited: a host answers RegisterStatusNotifierItem by
    // immediately reading our properties, and a synchronous wait here would
    // be parked inside `call_sync`, which drops every message that is not the
    // reply it wants. The host would get silence, time out, and discard the
    // item — which is exactly what happened the first time this ran.
    std::thread::spawn(move || {
        loop {
            let m = match conn.read() {
                Ok(m) => m,
                Err(e) => {
                    log(&format!("tray: bus connection closed ({e})"));
                    alive.store(false, Ordering::Relaxed);
                    return;
                }
            };
            match m.kind {
                METHOD_CALL => {
                    if let Some(reply) = dispatch(&m, &shared, &on_click) {
                        if conn.send(reply).is_err() {
                            alive.store(false, Ordering::Relaxed);
                            return;
                        }
                    }
                }
                ERROR => log(&format!(
                    "tray: {} — {}",
                    m.err_name.as_deref().unwrap_or("bus error"),
                    Dec::new(&m.body).string().unwrap_or_default()
                )),
                SIGNAL if m.member.as_deref() == Some("NameOwnerChanged") => {
                    // args: name, old owner, new owner. A non-empty new owner
                    // means a watcher just appeared and has never heard of us.
                    let mut d = Dec::new(&m.body);
                    let (_name, _old, new) = (d.string(), d.string(), d.string());
                    if new.is_some_and(|n| !n.is_empty()) {
                        log("tray: StatusNotifierWatcher reappeared — re-registering");
                        let _ = conn.send(register_msg(&unique_for_thread));
                    }
                }
                _ => {}
            }
        }
    });

    match tray.tx.lock() {
        Ok(mut tx) => {
            if let Err(e) = tx.send(register_msg(&unique)) {
                log(&format!("tray: cannot reach a StatusNotifierWatcher ({e})"));
                return None;
            }
        }
        Err(_) => return None,
    }

    log("tray: registered a StatusNotifierItem");
    Some(tray)
}

/// The watcher accepts either a bus name or an object path; the unique name
/// is what hosts store, paired with our well-known object path.
fn register_msg(unique: &str) -> Msg {
    let mut e = Enc::new();
    e.string(unique);
    Msg::call(WATCHER_NAME, WATCHER_PATH, WATCHER_NAME, "RegisterStatusNotifierItem")
        .with_body("s", e.buf)
}

/// Answer one method call. None means "no reply needed".
fn dispatch(
    m: &Msg,
    shared: &Arc<Mutex<Shared>>,
    on_click: &impl Fn(Option<String>),
) -> Option<Msg> {
    let iface = m.iface.as_deref().unwrap_or("");
    let member = m.member.as_deref().unwrap_or("");
    match (iface, member) {
        ("org.freedesktop.DBus.Introspectable", "Introspect") => {
            let mut e = Enc::new();
            e.string(INTROSPECT_XML);
            Some(Msg::reply_to(m).with_body("s", e.buf))
        }
        ("org.freedesktop.DBus.Properties", "Get") => {
            let mut d = Dec::new(&m.body);
            let (_iface, prop) = (d.string(), d.string()?);
            let mut e = Enc::new();
            if !write_prop(&mut e, &prop, shared) {
                return Some(Msg::error_to(
                    m,
                    "org.freedesktop.DBus.Error.UnknownProperty",
                    &prop,
                ));
            }
            Some(Msg::reply_to(m).with_body("v", e.buf))
        }
        ("org.freedesktop.DBus.Properties", "GetAll") => {
            let mut e = Enc::new();
            e.array(8, |e| {
                for p in PROPS {
                    e.align(8);
                    e.string(p);
                    // write_prop emits the variant, which is the dict value.
                    write_prop(e, p, shared);
                }
            });
            Some(Msg::reply_to(m).with_body("a{sv}", e.buf))
        }
        // A host may Set nothing on us; answering politely beats an error
        // that some hosts log loudly on every startup.
        ("org.freedesktop.DBus.Properties", "Set") => Some(Msg::reply_to(m)),
        (ITEM_IFACE, "Activate" | "SecondaryActivate" | "ContextMenu") => {
            let url = shared.lock().ok().and_then(|s| s.url.clone());
            on_click(url);
            Some(Msg::reply_to(m))
        }
        (ITEM_IFACE, "Scroll") => Some(Msg::reply_to(m)),
        _ if m.kind == METHOD_CALL => Some(Msg::error_to(
            m,
            "org.freedesktop.DBus.Error.UnknownMethod",
            &format!("{iface}.{member}"),
        )),
        _ => None,
    }
}

const PROPS: &[&str] = &[
    "Category",
    "Id",
    "Title",
    "Status",
    "IconName",
    "IconPixmap",
    "AttentionIconName",
    "OverlayIconName",
    "ToolTip",
    "ItemIsMenu",
    "WindowId",
];

/// Writes `prop` as a VARIANT into `e`. False when the name is unknown.
fn write_prop(e: &mut Enc, prop: &str, shared: &Arc<Mutex<Shared>>) -> bool {
    let state = shared.lock().ok().map(|s| s.state.clone()).unwrap_or_default();
    match prop {
        "Category" => e.variant_str("s", "SystemServices"),
        "Id" => e.variant_str("s", "busywatch"),
        "Title" => e.variant_str("s", "busywatch"),
        "Status" => e.variant_str("s", status_str(state.alarm)),
        // The icon is drawn here rather than named, so it works on a system
        // with no busywatch icon installed in any theme — which is all of them.
        "IconName" | "AttentionIconName" | "OverlayIconName" => e.variant_str("s", ""),
        "IconPixmap" => e.variant("a(iiay)", |e| write_pixmap(e, state.alarm)),
        "ToolTip" => e.variant("(sa(iiay)ss)", |e| {
            e.strukt(|e| {
                e.string(""); // icon name: the item's own icon is used
                e.array(8, |_| {}); // no tooltip pixmaps
                e.string("busywatch");
                e.string(&tooltip_body(&state));
            })
        }),
        "ItemIsMenu" => e.variant("b", |e| e.boolean(false)),
        "WindowId" => e.variant("i", |e| e.i32(0)),
        _ => return false,
    }
    true
}

fn tooltip_body(state: &State) -> String {
    match (&state.alarm, state.detail.is_empty()) {
        (Some(k), false) => format!("{} pressure — {}", k.as_str(), state.detail),
        (Some(k), true) => format!("{} pressure", k.as_str()),
        (None, false) => state.detail.clone(),
        (None, true) => "idle".into(),
    }
}

// ---------------------------------------------------------------- the icon

/// Colour per state, as (r, g, b).
fn colour(alarm: Option<Kind>) -> (u8, u8, u8) {
    match alarm {
        None => (0x6b, 0x8f, 0x71),        // quiet green-grey
        Some(Kind::Cpu) => (0xe6, 0x9a, 0x2e), // amber
        Some(Kind::Mem) => (0xd9, 0x4f, 0x43), // red
        Some(Kind::Io) => (0x3d, 0x8b, 0xd4),  // blue
    }
}

const ICON_PX: i32 = 22;

/// A filled circle, drawn by hand into ARGB32. SNI pixmaps are big-endian
/// ARGB regardless of the machine, so the bytes go out A, R, G, B.
///
/// Sampling four points per pixel is enough anti-aliasing at 22px to stop the
/// edge looking like a staircase, and costs nothing at the rate this is
/// redrawn (only when the state actually changes).
fn write_pixmap(e: &mut Enc, alarm: Option<Kind>) {
    let (r, g, b) = colour(alarm);
    e.array(8, |e| {
        e.strukt(|e| {
            e.i32(ICON_PX);
            e.i32(ICON_PX);
            e.array(1, |e| {
                let c = (ICON_PX as f64 - 1.0) / 2.0;
                let radius = ICON_PX as f64 / 2.0 - 1.5;
                for y in 0..ICON_PX {
                    for x in 0..ICON_PX {
                        let mut hits = 0;
                        for (dx, dy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                            let px = x as f64 + dx - 0.5;
                            let py = y as f64 + dy - 0.5;
                            if ((px - c).powi(2) + (py - c).powi(2)).sqrt() <= radius {
                                hits += 1;
                            }
                        }
                        let a = (hits * 255 / 4) as u8;
                        e.byte(a);
                        // Premultiplication is not specified by SNI and hosts
                        // differ; straight colour with the alpha channel is
                        // what every host we care about renders correctly.
                        e.byte(r);
                        e.byte(g);
                        e.byte(b);
                    }
                }
            });
        });
    });
}

const INTROSPECT_XML: &str = r#"<!DOCTYPE node PUBLIC "-//freedesktop//DTD D-BUS Object Introspection 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd">
<node>
 <interface name="org.freedesktop.DBus.Introspectable">
  <method name="Introspect"><arg name="xml" type="s" direction="out"/></method>
 </interface>
 <interface name="org.freedesktop.DBus.Properties">
  <method name="Get">
   <arg name="interface" type="s" direction="in"/>
   <arg name="property" type="s" direction="in"/>
   <arg name="value" type="v" direction="out"/>
  </method>
  <method name="GetAll">
   <arg name="interface" type="s" direction="in"/>
   <arg name="properties" type="a{sv}" direction="out"/>
  </method>
 </interface>
 <interface name="org.kde.StatusNotifierItem">
  <property name="Category" type="s" access="read"/>
  <property name="Id" type="s" access="read"/>
  <property name="Title" type="s" access="read"/>
  <property name="Status" type="s" access="read"/>
  <property name="IconName" type="s" access="read"/>
  <property name="IconPixmap" type="a(iiay)" access="read"/>
  <property name="ToolTip" type="(sa(iiay)ss)" access="read"/>
  <property name="ItemIsMenu" type="b" access="read"/>
  <property name="WindowId" type="i" access="read"/>
  <method name="Activate">
   <arg name="x" type="i" direction="in"/>
   <arg name="y" type="i" direction="in"/>
  </method>
  <method name="SecondaryActivate">
   <arg name="x" type="i" direction="in"/>
   <arg name="y" type="i" direction="in"/>
  </method>
  <method name="ContextMenu">
   <arg name="x" type="i" direction="in"/>
   <arg name="y" type="i" direction="in"/>
  </method>
  <method name="Scroll">
   <arg name="delta" type="i" direction="in"/>
   <arg name="orientation" type="s" direction="in"/>
  </method>
  <signal name="NewIcon"/>
  <signal name="NewStatus"><arg name="status" type="s"/></signal>
  <signal name="NewToolTip"/>
 </interface>
</node>"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// The pixmap is the one place a wrong length silently produces a blank
    /// or corrupt icon rather than an error, so pin the arithmetic.
    #[test]
    fn pixmap_is_argb32_of_the_declared_size() {
        let mut e = Enc::new();
        write_pixmap(&mut e, None);
        // outer array length, then struct: width, height, byte array.
        let mut d = Dec::new(&e.buf);
        let outer = d.u32().unwrap() as usize;
        // The count covers the elements only: after the u32 length the first
        // 8-aligned struct starts at offset 8, so four bytes of padding sit
        // outside it. Getting this backwards is how a pixmap silently
        // arrives four bytes short.
        assert_eq!(outer, e.buf.len() - 8);
        assert_eq!(outer, 4 + 4 + 4 + (ICON_PX * ICON_PX * 4) as usize);
        d.pos = 8; // struct is 8-aligned after the array length
        assert_eq!(d.i32(), Some(ICON_PX));
        assert_eq!(d.i32(), Some(ICON_PX));
        assert_eq!(d.u32(), Some((ICON_PX * ICON_PX * 4) as u32));
    }

    #[test]
    fn status_follows_the_alarm() {
        assert_eq!(status_str(None), "Active");
        assert_eq!(status_str(Some(Kind::Mem)), "NeedsAttention");
    }
}
