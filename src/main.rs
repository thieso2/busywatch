//! busywatch — warn when the system is too busy, at near-zero cost, and keep
//! a history of what made it busy.
//!
//! Instead of polling, it registers PSI (pressure stall information) triggers
//! with the kernel — one each for CPU, memory and IO — and sleeps in poll()
//! until the kernel itself reports that tasks were stalled beyond the trigger
//! threshold inside the window.
//!
//! A trigger event alone is not a warning: transient spikes (a compile, a page
//! load) fire triggers constantly.  On each wakeup the sustained average
//! (avg60) is checked, and only when it exceeds the threshold for that
//! resource is a warning issued — with the top consumers sampled at that
//! moment, so the warning names the culprit.  Memory additionally warns when
//! MemAvailable falls below --mem-free-pct, which catches a hog eating RAM
//! before the kernel starts stalling on it.
//!
//! A warning opens a busy *incident* of that kind; while one lasts, sampling
//! runs at --poll-secs so the history is dense where it matters, and recovery
//! (avg60 below half the threshold) sends an all-clear toast.  Independently,
//! a heartbeat sample every --sample-secs records the whole system — pressure,
//! load, memory, and the top CPU/IO/RSS processes — so `busywatch web` can
//! chart minutes, hours or days of history rather than incidents alone.

mod dbus;
mod tray;
mod db;
mod sample;
mod util;
mod web;

use std::fs::OpenOptions;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use db::{Db, Kind, Reading, KINDS};
use sample::{
    fmt_top_cpu, fmt_top_io, fmt_top_mem, io_rate, load1, psi_summary, read_loadavg, read_meminfo,
    read_pressures, top_by, Consumer, Tracker, PSI_CPU, PSI_IO, PSI_MEM,
};
use util::{fmt_bytes, fmt_dur, fmt_ts, json_str, log, parse_span, unix_now};

/// Incident ends when the pressure drops below this fraction of its threshold
/// (hysteresis, so recover/busy doesn't flap toasts).
const CLEAR_FRACTION: f64 = 0.5;
const DEFAULT_ADDR: &str = "127.0.0.1:8787";

struct Config {
    stall_ms: u64,          // trigger: stall time within window
    window_ms: u64,         // trigger: window size
    sustained: [f64; 3],    // warn when avg60 "some" >= this %, per Kind
    mem_free_pct: f64,      // warn when MemAvailable < this % of MemTotal
    cooldown: u64,          // seconds between repeat warnings
    top: usize,             // processes to name in the warning
    poll_secs: u64,         // fallback sampling / busy re-check interval
    sample_secs: u64,       // heartbeat history sampling (0 = incidents only)
    retain_days: u64,       // prune history older than this (0 = keep all)
    retain_pid_days: u64,   // keep the bulky per-pid rows for this long
    notify: bool,           // send a desktop notification (else log only)
    db: Option<PathBuf>,    // history database (None = disabled)
    web: Option<String>,    // serve the UI on this address while watching
    tray: bool,             // show a StatusNotifierItem tray icon
}

impl Default for Config {
    fn default() -> Self {
        Config {
            stall_ms: 500,
            window_ms: 1000,
            sustained: [20.0, 10.0, 25.0],
            mem_free_pct: 10.0,
            cooldown: 300,
            top: 3,
            poll_secs: 10,
            sample_secs: 60,
            retain_days: 30,
            retain_pid_days: 3,
            notify: true,
            db: Some(default_db_path()),
            web: None,
            tray: true,
        }
    }
}

fn default_db_path() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state")
        })
        .join("busywatch/history.db")
}

fn usage() -> ! {
    eprintln!(
        "usage: busywatch [--stall-ms N] [--window-ms N] [--sustained PCT]\n\
         \x20                [--mem-sustained PCT] [--io-sustained PCT] [--mem-free-pct PCT]\n\
         \x20                [--cooldown SECS] [--top N] [--poll-secs N] [--sample-secs N]\n\
         \x20                [--retain-days N] [--retain-pid-days N] [--web [ADDR:]PORT]\n\
         \x20                [--no-notify] [--no-tray]\n\
         \x20                [--db PATH] [--no-db]\n\
         \x20      busywatch history [N] [--db PATH]      recent busy incidents\n\
         \x20      busywatch hogs [--by mem|cpu|io] [--since 24h] [--top N]\n\
         \x20      busywatch app NAME [--since 24h]       full rundown of one app\n\
         \x20      busywatch web [--bind [ADDR:]PORT]     history UI in a browser\n\
         \x20      busywatch detail [--db PATH]           live system detail report"
    );
    std::process::exit(2);
}

fn parse_args(args: Vec<String>) -> Config {
    let mut cfg = Config::default();
    let mut args = args.into_iter().peekable();
    while let Some(a) = args.next() {
        let val = |args: &mut dyn Iterator<Item = String>| -> String {
            args.next().unwrap_or_else(|| usage())
        };
        match a.as_str() {
            "--stall-ms" => cfg.stall_ms = val(&mut args).parse().unwrap_or_else(|_| usage()),
            "--window-ms" => cfg.window_ms = val(&mut args).parse().unwrap_or_else(|_| usage()),
            "--sustained" => {
                cfg.sustained[0] = val(&mut args).parse().unwrap_or_else(|_| usage())
            }
            "--mem-sustained" => {
                cfg.sustained[1] = val(&mut args).parse().unwrap_or_else(|_| usage())
            }
            "--io-sustained" => {
                cfg.sustained[2] = val(&mut args).parse().unwrap_or_else(|_| usage())
            }
            "--mem-free-pct" => cfg.mem_free_pct = val(&mut args).parse().unwrap_or_else(|_| usage()),
            "--cooldown" => cfg.cooldown = val(&mut args).parse().unwrap_or_else(|_| usage()),
            "--top" => cfg.top = val(&mut args).parse().unwrap_or_else(|_| usage()),
            "--poll-secs" => cfg.poll_secs = val(&mut args).parse().unwrap_or_else(|_| usage()),
            "--sample-secs" => cfg.sample_secs = val(&mut args).parse().unwrap_or_else(|_| usage()),
            "--retain-days" => cfg.retain_days = val(&mut args).parse().unwrap_or_else(|_| usage()),
            "--retain-pid-days" => {
                cfg.retain_pid_days = val(&mut args).parse().unwrap_or_else(|_| usage())
            }
            "--web" => {
                // `--web` alone means the default address.
                let next = args.peek().filter(|s| !s.starts_with("--")).cloned();
                cfg.web = Some(match next {
                    Some(v) => {
                        args.next();
                        web::normalize_addr(&v)
                    }
                    None => DEFAULT_ADDR.to_string(),
                });
            }
            "--no-notify" => cfg.notify = false,
            "--no-tray" => cfg.tray = false,
            "--db" => cfg.db = Some(PathBuf::from(val(&mut args))),
            "--no-db" => cfg.db = None,
            "-h" | "--help" => usage(),
            _ => usage(),
        }
    }
    cfg
}

// ------------------------------------------------------------ notification

fn exe_path() -> String {
    std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "busywatch".into())
}

/// Fresh full-system snapshot, shown as a toast when no terminal can be
/// opened (last-resort click fallback).
fn detail_snapshot() -> String {
    let cons = sample::sample_consumers(Duration::from_millis(1000));
    format!(
        "cpu:  {}\nmem:  {}\nio:   {}\nload: {}\ntop cpu: {}\ntop io: {}\ntop mem: {}",
        psi_summary(PSI_CPU),
        psi_summary(PSI_MEM),
        psi_summary(PSI_IO),
        read_loadavg(),
        fmt_top_cpu(&cons, 3),
        fmt_top_io(&cons, 3),
        fmt_top_mem(&cons, 3),
    )
}

/// The loopback alias the overlay is opened under.
///
/// `--class` is X11-only: on Wayland a Chromium-family app window derives its
/// app_id from the URL's *host*, so an overlay opened on `127.0.0.1` lands in
/// the same class as every other local web app and cannot be told apart by a
/// compositor rule. Any name under `.localhost` resolves to loopback without
/// touching /etc/hosts, and the server does not care which Host it is asked
/// for — so this buys a window class of its own for free.
const OVERLAY_HOST: &str = "busywatch.localhost";

/// Rewrite a loopback URL to the alias. Anything else is left alone: a UI
/// deliberately bound to a real address must stay reachable at that address.
fn overlay_url(url: &str) -> String {
    for host in ["127.0.0.1", "localhost", "[::1]"] {
        let from = format!("://{host}");
        if url.starts_with(&format!("http{from}")) || url.contains(&format!("{from}:")) {
            return url.replacen(&from, &format!("://{OVERLAY_HOST}"), 1);
        }
    }
    url.to_string()
}

/// Open the history UI as a chromeless overlay rather than a browser tab.
///
/// `--app=URL` gives a window with no tabs, address bar or menu — the page
/// and nothing else — and `--class` labels it so Hyprland can float it. The
/// ladder is: Omarchy's own web-app launcher, then any Chromium-family
/// browser directly, then `xdg-open` (an ordinary tab, but it opens), then
/// the terminal report. Each rung is a real fallback, not a retry.
fn open_url(url: &str) {
    let url = &overlay_url(url);
    // Omarchy already launches every other web app this way, so its wrapper
    // gets first refusal: it knows which browser is default and starts it
    // detached under the session's own scope.
    if let Ok(st) = Command::new("omarchy-launch-webapp").arg(url).status()
    {
        if st.success() {
            return;
        }
    }
    for browser in ["brave", "chromium", "google-chrome-stable", "microsoft-edge", "vivaldi"] {
        let ok = Command::new(browser).arg(format!("--app={url}")).spawn().is_ok();
        if ok {
            return;
        }
    }
    if let Ok(st) = Command::new("xdg-open").arg(url).status() {
        if st.success() {
            log("no chromeless-capable browser found — opened an ordinary tab");
            return;
        }
    }
    log(&format!("cannot open {url} — showing the terminal report instead"));
    open_detail_window();
}

/// Open the detail report in a terminal window: Omarchy's floating
/// presentation terminal first, plain xdg-terminal-exec next, and a
/// notification with a condensed snapshot as the last resort.  Runs from
/// the click-handler thread, so blocking until the window closes is fine.
fn open_detail_window() {
    let exe = exe_path();
    if let Ok(st) = Command::new("omarchy-launch-floating-terminal-with-presentation")
        .arg(format!("{exe} detail"))
        .status()
    {
        if st.success() {
            return;
        }
    }
    let script = format!("{exe} detail; echo; read -rn1 -s -p 'press any key to close'");
    if Command::new("xdg-terminal-exec").args(["-e", "bash", "-c", &script]).status().is_ok() {
        return;
    }
    let detail = detail_snapshot();
    let _ = Command::new("notify-send")
        .args([
            "-u",
            "normal",
            "-a",
            "busywatch",
            "-t",
            "30000",
            "-h",
            "string:x-canonical-private-synchronous:busywatch",
            "System detail",
            &detail,
        ])
        .status();
}

/// Toast ids per Kind, so each resource replaces its own previous toast
/// instead of stacking (or erasing another resource's warning).
static TOAST_ID: [std::sync::atomic::AtomicU32; 3] = [
    std::sync::atomic::AtomicU32::new(0),
    std::sync::atomic::AtomicU32::new(0),
    std::sync::atomic::AtomicU32::new(0),
];

/// Omarchy's shell runs a toast's click command only from the
/// `omarchy-exec-argv` hint, which its own sender builds from `--exec` — it
/// carries the click as data, so a toast restored after a shell restart stays
/// clickable, where a libnotify action cannot.  Returns false when that sender
/// is not installed, leaving the libnotify path to handle it.
fn omarchy_notify(kind: Kind, urgency: &str, summary: &str, body: &str, click: &[String]) -> bool {
    let prev = TOAST_ID[kind.idx()].load(Ordering::Relaxed).to_string();
    let mut cmd = Command::new("omarchy-notification-send");
    cmd.args(["--app-name", "busywatch", "-u", urgency, "-r", &prev, "-p"]);
    cmd.arg(summary).arg(body).arg("--exec").args(click);
    let Ok(out) = cmd.output() else { return false };
    if !out.status.success() {
        return false;
    }
    if let Ok(id) = String::from_utf8_lossy(&out.stdout).trim().parse::<u32>() {
        TOAST_ID[kind.idx()].store(id, Ordering::Relaxed);
    }
    true
}

/// Send a toast whose click opens the history UI in a browser, on the metric
/// and time span of the incident that raised it — or, when no web UI is
/// running, the detail report in a floating terminal.
///
/// Omarchy's sender is preferred (see above).  Everything else goes through
/// notify-send: the synchronous hint makes daemons replace the previous
/// busywatch toast *of the same kind*, a libnotify "default" action covers
/// mako/dunst/swaync, and the argv hint rides along for other Omarchy
/// versions.  The waiting notify-send lives in a background thread so the
/// watch loop is never blocked.
fn send_notification(
    kind: Kind,
    urgency: &'static str,
    summary: &str,
    body: &str,
    url: Option<String>,
) {
    let click: Vec<String> = match &url {
        Some(u) => vec!["xdg-open".into(), u.clone()],
        None => vec![
            "omarchy-launch-floating-terminal-with-presentation".into(),
            format!("{} detail", exe_path()),
        ],
    };
    if omarchy_notify(kind, urgency, summary, body, &click) {
        return;
    }
    let argv_hint = format!(
        "string:omarchy-exec-argv:[{}]",
        click.iter().map(|a| json_str(a)).collect::<Vec<_>>().join(",")
    );
    let mut cmd = Command::new("notify-send");
    cmd.args([
        "-u",
        urgency,
        "-a",
        "busywatch",
        "-h",
        &format!("string:x-canonical-private-synchronous:busywatch-{}", kind.as_str()),
        "-h",
        &argv_hint,
        "-A",
        if url.is_some() { "default=Open history" } else { "default=Details" },
        summary,
        body,
    ]);
    std::thread::spawn(move || {
        // -A implies --wait: returns when the toast is clicked, dismissed,
        // expired, or replaced by the next busywatch toast.
        let Ok(out) = cmd.output() else { return };
        if String::from_utf8_lossy(&out.stdout).trim() == "default" {
            match url {
                Some(u) => open_url(&u),
                None => open_detail_window(),
            }
        }
    });
}

// ------------------------------------------------------------- watch loop

struct Incident {
    since: Instant,
    peak: f64,
    min_avail_kb: u64,
    top_comm: Option<String>,
    db_id: Option<i64>,
    last_warn: Option<Instant>,
}

struct Watcher {
    cfg: Config,
    db: Option<Db>,
    tracker: Tracker,
    incidents: [Option<Incident>; 3],
    last_sample: Option<Instant>,
    last_prune: Instant,
    tray: Option<tray::Tray>,
}

/// The UI's range buttons.  Snapping to one of these means the range the
/// click opens is also the one shown as selected.
const UI_SPANS: [u64; 7] = [900, 3600, 21600, 86400, 259200, 604800, 2592000];

fn snap_span(secs: u64) -> u64 {
    *UI_SPANS.iter().find(|s| **s >= secs).unwrap_or(UI_SPANS.last().unwrap())
}

impl Watcher {
    fn threshold(&self, k: Kind) -> f64 {
        self.cfg.sustained[k.idx()]
    }

    /// Is this resource over its warning threshold right now?
    fn over(&self, k: Kind, r: &Reading) -> bool {
        match k {
            Kind::Cpu => r.psi.cpu.avg60 >= self.threshold(k),
            Kind::Io => r.psi.io.avg60 >= self.threshold(k),
            Kind::Mem => {
                r.psi.mem.avg60 >= self.threshold(k)
                    || (r.mem.total_kb > 0 && r.mem.avail_pct() < self.cfg.mem_free_pct)
            }
        }
    }

    /// Has it recovered enough to close the incident?  Deliberately lower than
    /// `over` so a resource hovering at the threshold doesn't flap toasts.
    fn recovered(&self, k: Kind, r: &Reading) -> bool {
        let clear = self.threshold(k) * CLEAR_FRACTION;
        match k {
            Kind::Cpu => r.psi.cpu.avg60 < clear,
            Kind::Io => r.psi.io.avg60 < clear,
            Kind::Mem => {
                r.psi.mem.avg60 < clear
                    && (r.mem.total_kb == 0 || r.mem.avail_pct() > self.cfg.mem_free_pct * 1.5)
            }
        }
    }

    fn level(k: Kind, r: &Reading) -> f64 {
        match k {
            Kind::Cpu => r.psi.cpu.avg60,
            Kind::Mem => r.psi.mem.avg60,
            Kind::Io => r.psi.io.avg60,
        }
    }

    /// One pass: read the kernel, open/close incidents, sample and record.
    /// Returns true while any incident is active, so the caller re-checks at
    /// the faster cadence instead of waiting for the next kernel event.
    fn tick(&mut self) -> bool {
        let r = Reading {
            psi: read_pressures(),
            mem: read_meminfo(),
            load1: load1(),
            power: sample::read_power(),
            clock: sample::read_cpu_clock(),
            thermal: sample::read_thermal(),
        };

        // Closing comes first: an incident that just ended must not hold the
        // loop at the busy cadence for another round.
        for k in KINDS {
            if self.incidents[k.idx()].is_some() && self.recovered(k, &r) {
                let inc = self.incidents[k.idx()].take().unwrap();
                self.all_clear(k, &r, &inc);
            }
        }
        let mut opened: Vec<Kind> = Vec::new();
        for k in KINDS {
            if self.incidents[k.idx()].is_none() && self.over(k, &r) {
                let db_id = self.db.as_ref().and_then(|db| db.begin_incident(k, unix_now()).ok());
                self.incidents[k.idx()] = Some(Incident {
                    since: Instant::now(),
                    peak: Self::level(k, &r),
                    min_avail_kb: r.mem.avail_kb,
                    top_comm: None,
                    db_id,
                    last_warn: None,
                });
                opened.push(k);
            }
        }
        let busy = self.incidents.iter().any(Option::is_some);
        for k in KINDS {
            if let Some(inc) = self.incidents[k.idx()].as_mut() {
                inc.peak = inc.peak.max(Self::level(k, &r));
                inc.min_avail_kb = inc.min_avail_kb.min(r.mem.avail_kb);
            }
        }

        // Which kinds want a (re-)warning this round?
        let cooldown = self.cfg.cooldown;
        let warn: Vec<Kind> = KINDS
            .into_iter()
            .filter(|k| {
                self.incidents[k.idx()].as_ref().is_some_and(|i| {
                    i.last_warn.map_or(true, |t| t.elapsed().as_secs() >= cooldown)
                })
            })
            .collect();

        let interval = if busy { self.cfg.poll_secs } else { self.cfg.sample_secs };
        let sample_due = self.db.is_some()
            && interval > 0
            && self.last_sample.map_or(true, |t| t.elapsed().as_secs() + 1 >= interval);

        if sample_due || !warn.is_empty() {
            // One /proc sweep serves both the record and the warning text.
            let (cons, _span) = self.tracker.sample(Duration::from_millis(400));
            for k in KINDS {
                if let Some(inc) = self.incidents[k.idx()].as_mut() {
                    if let Some(c) = Self::culprit(k, &cons) {
                        inc.top_comm = Some(c);
                    }
                }
            }
            if sample_due {
                self.last_sample = Some(Instant::now());
                let inc_id = KINDS.iter().find_map(|k| {
                    self.incidents[k.idx()].as_ref().and_then(|i| i.db_id)
                });
                let top_n = if busy { 8 } else { 4 };
                if let Some(db) = self.db.as_mut() {
                    if let Err(e) = db.record_sample(inc_id, &r, &cons, top_n) {
                        log(&format!("history db write failed: {e}"));
                    }
                }
            }
            for k in warn {
                if let Some(inc) = self.incidents[k.idx()].as_mut() {
                    inc.last_warn = Some(Instant::now());
                }
                self.warn(k, &r, &cons, opened.contains(&k));
            }
        }

        if self.cfg.retain_days > 0 && self.last_prune.elapsed().as_secs() >= 6 * 3600 {
            self.last_prune = Instant::now();
            if let Some(db) = self.db.as_ref() {
                match db.prune(self.cfg.retain_days, self.cfg.retain_pid_days) {
                    Ok(n) if n > 0 => log(&format!("pruned {n} rows older than {} days", self.cfg.retain_days)),
                    Err(e) => log(&format!("history prune failed: {e}")),
                    _ => {}
                }
            }
        }

        // The tray icon carries the same verdict the toasts do. Memory
        // outranks IO outranks CPU: when two resources stall at once, the
        // colour should name the one that hurts most.
        if let Some(t) = self.tray.as_ref() {
            let alarm = [Kind::Mem, Kind::Io, Kind::Cpu]
                .into_iter()
                .find(|k| self.incidents[k.idx()].is_some());
            t.update(tray::State {
                alarm,
                detail: format!(
                    "cpu {:.0}%  mem {:.0}%  io {:.0}%  load {:.1}",
                    r.psi.cpu.avg60, r.psi.mem.avg60, r.psi.io.avg60, r.load1
                ),
            });
        }
        busy
    }

    /// Where a click on this toast should land: the history UI, opened on the
    /// resource that raised it and wide enough to show the whole incident.
    fn click_url(&self, k: Kind, elapsed: Duration) -> Option<String> {
        let span = snap_span((elapsed.as_secs() * 3).max(900));
        self.cfg.web.as_deref().map(|addr| web::ui_url(addr, k.as_str(), span))
    }

    /// The process most responsible for this kind of pressure, if any stands out.
    fn culprit(k: Kind, cons: &[Consumer]) -> Option<String> {
        let c = match k {
            Kind::Cpu => top_by(cons, 1, |c| c.cpu_pct, 20.0),
            Kind::Mem => top_by(cons, 1, |c| c.rss_kb as f64, 65536.0),
            Kind::Io => top_by(cons, 1, io_rate, 1_000_000.0),
        };
        c.first().map(|c| c.comm.clone())
    }

    fn warn(&self, k: Kind, r: &Reading, cons: &[Consumer], first: bool) {
        let elapsed =
            self.incidents[k.idx()].as_ref().map_or(Duration::ZERO, |i| i.since.elapsed());
        let (summary, body) = match k {
            Kind::Cpu => (
                "System is too busy",
                format!(
                    "CPU pressure avg10={:.0}% avg60={:.0}% load={}\ntop: {}",
                    r.psi.cpu.avg10,
                    r.psi.cpu.avg60,
                    read_loadavg(),
                    fmt_top_cpu(cons, self.cfg.top)
                ),
            ),
            Kind::Mem => (
                "Memory is running low",
                format!(
                    "{} of {} available ({:.0}%), memory stall avg60={:.0}%{}\ntop: {}",
                    fmt_bytes(r.mem.avail_kb * 1024),
                    fmt_bytes(r.mem.total_kb * 1024),
                    r.mem.avail_pct(),
                    r.psi.mem.avg60,
                    if r.mem.swap_used_kb > 0 {
                        format!(", swap {}", fmt_bytes(r.mem.swap_used_kb * 1024))
                    } else {
                        String::new()
                    },
                    fmt_top_mem(cons, self.cfg.top)
                ),
            ),
            Kind::Io => (
                "Disk IO is saturated",
                format!(
                    "IO pressure avg10={:.0}% avg60={:.0}%\ntop: {}",
                    r.psi.io.avg10,
                    r.psi.io.avg60,
                    fmt_top_io(cons, self.cfg.top)
                ),
            ),
        };
        log(&format!(
            "WARNING {} {summary} — {}",
            if first { "(new)" } else { "(ongoing)" },
            body.replace('\n', " — ")
        ));
        if self.cfg.notify {
            send_notification(k, "critical", summary, &body, self.click_url(k, elapsed));
        }
    }

    fn all_clear(&self, k: Kind, r: &Reading, inc: &Incident) {
        if let (Some(db), Some(id)) = (self.db.as_ref(), inc.db_id) {
            let min_avail = (k == Kind::Mem).then_some(inc.min_avail_kb);
            if let Err(e) =
                db.end_incident(id, unix_now(), inc.peak, min_avail, inc.top_comm.as_deref())
            {
                log(&format!("history db write failed: {e}"));
            }
        }
        let summary = match k {
            Kind::Cpu => "System back to normal",
            Kind::Mem => "Memory back to normal",
            Kind::Io => "Disk IO back to normal",
        };
        let extra = match k {
            Kind::Mem => format!(", now {} available", fmt_bytes(r.mem.avail_kb * 1024)),
            _ => String::new(),
        };
        let body = format!(
            "{} pressure avg60={:.0}% — was busy for {}, peak {:.0}%{}{}",
            k.as_str(),
            Self::level(k, r),
            fmt_dur(inc.since.elapsed()),
            inc.peak,
            extra,
            inc.top_comm.as_deref().map(|c| format!(" — {c}")).unwrap_or_default(),
        );
        log(&format!("all clear — {body}"));
        if self.cfg.notify {
            send_notification(k, "normal", summary, &body, self.click_url(k, inc.since.elapsed()));
        }
    }
}

/// Register a PSI trigger on one pressure file; None if not permitted or the
/// kernel has no such file (cgroup-less kernels lack /proc/pressure entirely).
fn psi_trigger(path: &str, cfg: &Config) -> Option<std::fs::File> {
    let mut f = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(f) => f,
        Err(e) => {
            log(&format!("cannot open {path} read-write: {e}"));
            return None;
        }
    };
    // The kernel NUL-terminates the buffer in place, clobbering the last
    // byte — write strlen+1 bytes like the kernel docs' example does.
    let spec = format!("some {} {}\0", cfg.stall_ms * 1000, cfg.window_ms * 1000);
    if let Err(e) = f.write_all(spec.as_bytes()) {
        log(&format!("cannot register PSI trigger on {path} ({spec:?}): {e}"));
        return None;
    }
    Some(f)
}

fn watch(cfg: Config) -> ! {
    let db = cfg.db.as_ref().and_then(|path| match Db::open(path) {
        Ok(db) => {
            log(&format!("history db: {}", path.display()));
            match db.close_stale_incidents() {
                Ok(n) if n > 0 => log(&format!("closed {n} incident(s) left open by a previous run")),
                Err(e) => log(&format!("history db check failed: {e}")),
                _ => {}
            }
            if cfg.retain_days > 0 {
                if let Ok(n) = db.prune(cfg.retain_days, cfg.retain_pid_days) {
                    if n > 0 {
                        log(&format!("pruned {n} rows older than {} days", cfg.retain_days));
                    }
                }
            }
            Some(db)
        }
        Err(e) => {
            log(&format!("cannot open history db {}: {e} — continuing without", path.display()));
            None
        }
    });
    if let (Some(addr), Some(path)) = (cfg.web.clone(), cfg.db.clone()) {
        web::spawn(addr, path);
    } else if cfg.web.is_some() {
        log("--web needs a history database; ignoring");
    }

    // The tray icon opens the same place a toast click does, so it shares the
    // click handler; None when there is no session bus or no tray host.
    let tray_url = cfg.web.as_deref().map(|addr| web::ui_url(addr, "cpu", 21600));
    let tray = if cfg.tray {
        tray::start(tray_url, |url| match url {
            Some(u) => open_url(&u),
            None => open_detail_window(),
        })
    } else {
        None
    };

    let mut w = Watcher {
        cfg,
        db,
        tracker: Tracker::default(),
        incidents: [None, None, None],
        last_sample: None,
        last_prune: Instant::now(),
        tray,
    };

    let triggers: Vec<(Kind, std::fs::File)> = [
        (Kind::Cpu, PSI_CPU),
        (Kind::Mem, PSI_MEM),
        (Kind::Io, PSI_IO),
    ]
    .into_iter()
    .filter_map(|(k, p)| psi_trigger(p, &w.cfg).map(|f| (k, f)))
    .collect();

    if triggers.is_empty() {
        log(&format!(
            "no PSI triggers permitted — falling back to sampling every {}s",
            w.cfg.poll_secs
        ));
        loop {
            let busy = w.tick();
            let secs = if busy || w.cfg.sample_secs == 0 {
                w.cfg.poll_secs
            } else {
                w.cfg.poll_secs.min(w.cfg.sample_secs)
            };
            std::thread::sleep(Duration::from_secs(secs.max(1)));
        }
    }

    log(&format!(
        "watching {} via kernel triggers (some {}ms/{}ms); warn at cpu>={}%, mem>={}% or <{}% free, io>={}%; \
         history every {}s, {}",
        triggers.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join("+"),
        w.cfg.stall_ms,
        w.cfg.window_ms,
        w.cfg.sustained[0],
        w.cfg.sustained[1],
        w.cfg.mem_free_pct,
        w.cfg.sustained[2],
        w.cfg.sample_secs,
        w.cfg.web.as_deref().map(|a| format!("web on {a}")).unwrap_or_else(|| "no web ui".into()),
    ));

    let mut busy = false;
    loop {
        // While an incident is active the kernel may go quiet (load gone) —
        // and the history heartbeat has to fire regardless — so poll with a
        // deadline instead of blocking forever.  With nothing to record and
        // nothing busy there is no deadline at all: block until the kernel
        // says something happened, which is the whole point of the triggers.
        let interval = if busy {
            w.cfg.poll_secs
        } else if w.db.is_some() {
            w.cfg.sample_secs
        } else {
            0
        };
        let timeout: i32 = if interval == 0 {
            -1
        } else {
            let due = w
                .last_sample
                .map(|t| interval.saturating_sub(t.elapsed().as_secs()))
                .unwrap_or(0);
            (due.max(1) * 1000).min(i32::MAX as u64) as i32
        };
        let mut pfds: Vec<libc::pollfd> = triggers
            .iter()
            .map(|(_, f)| libc::pollfd { fd: f.as_raw_fd(), events: libc::POLLPRI, revents: 0 })
            .collect();
        let rc = unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as libc::nfds_t, timeout) };
        if rc < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            log(&format!("poll failed: {err}"));
            std::process::exit(1);
        }
        if pfds.iter().any(|p| p.revents & libc::POLLERR != 0) {
            log("PSI trigger fd went bad (POLLERR)");
            std::process::exit(1);
        }
        // Either a trigger fired or the deadline passed; both mean "look".
        // At most one event per window is generated, so looping straight back
        // into poll() is cheap even under load.
        busy = w.tick();
    }
}

// ------------------------------------------------------------ subcommands

fn cmd_history(db_path: &Path, n: usize) -> i32 {
    let conn = match db::open_read(db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cannot open {}: {e}", db_path.display());
            return 1;
        }
    };
    let rows = match db::incidents(&conn, 0, i64::MAX, n) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("history query failed: {e}");
            return 1;
        }
    };
    if rows.is_empty() {
        println!("no busy incidents recorded");
        return 0;
    }
    for r in &rows {
        let dur = match r.ended {
            Some(e) => fmt_dur(Duration::from_secs((e - r.started).max(0) as u64)),
            None => "ongoing".into(),
        };
        let peak = r.peak_avg60.map(|p| format!("{p:.0}%")).unwrap_or_else(|| "?".into());
        let free = r
            .min_mem_avail_kb
            .map(|kb| format!("  min free {}", fmt_bytes(kb as u64 * 1024)))
            .unwrap_or_default();
        println!(
            "incident {} [{}]  {}  {dur}  peak {peak}{free}",
            r.id,
            r.kind,
            fmt_ts(r.started)
        );
        let cons = db::incident_consumers(&conn, r).unwrap_or_default();
        println!("  cpu: {}", fmt_top_cpu(&cons, 5));
        println!("  io:  {}", fmt_top_io(&cons, 3));
        println!("  mem: {}", fmt_top_mem(&cons, 3));
    }
    0
}

/// `busywatch hogs` — what has been eating the machine over a period,
/// whether or not it ever crossed a warning threshold.
fn cmd_hogs(db_path: &Path, metric: &str, since: u64, top: usize) -> i32 {
    let conn = match db::open_read(db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cannot open {}: {e}", db_path.display());
            return 1;
        }
    };
    let now = unix_now() as i64;
    let from = now - since as i64;
    let rows = match db::hogs(&conn, from, now, metric, top) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("hogs query failed: {e}");
            return 1;
        }
    };
    if rows.is_empty() {
        println!("no history recorded in the last {}", fmt_dur(Duration::from_secs(since)));
        return 0;
    }
    println!(
        "top {} by {metric} over the last {} (since {})\n",
        rows.len(),
        fmt_dur(Duration::from_secs(since)),
        fmt_ts(from)
    );
    println!(
        "{:<20} {:>10} {:>10} {:>10} {:>9} {:>7} {:>9} {:>9}  {}",
        "process", "rss peak", "rss avg", "rss now", "cpu time", "cpu pk", "io read", "io write",
        "last seen"
    );
    for h in &rows {
        println!(
            "{:<20} {:>10} {:>10} {:>10} {:>9} {:>6.0}% {:>9} {:>9}  {}",
            truncate(&h.comm, 20),
            fmt_bytes(h.rss_max_kb as u64 * 1024),
            fmt_bytes(h.rss_avg_kb as u64 * 1024),
            // "0B now" means "not in the last sample", which is not the same
            // as holding no memory — say nothing rather than something wrong.
            if h.rss_last_kb > 0 { fmt_bytes(h.rss_last_kb as u64 * 1024) } else { "—".into() },
            fmt_dur(Duration::from_secs(h.cpu_secs as u64)),
            h.cpu_max,
            fmt_bytes(h.io_rd_bytes as u64),
            fmt_bytes(h.io_wr_bytes as u64),
            fmt_ts(h.last_ts)
        );
    }
    println!("\nbusywatch app <name> for the full rundown of one app");
    0
}

fn truncate(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// `busywatch app NAME` — everything recorded about one application over a
/// period: what it added up to, which pids carried it, what it set off.
fn cmd_app(db_path: &Path, comm: &str, since: u64, top_pids: usize) -> i32 {
    let conn = match db::open_read(db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cannot open {}: {e}", db_path.display());
            return 1;
        }
    };
    let now = unix_now() as i64;
    let from = now - since as i64;
    let window = since.max(1);
    let a = match db::app_summary(&conn, comm, from, now) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("app query failed: {e}");
            return 1;
        }
    };
    if a.samples == 0 {
        println!(
            "nothing recorded for {comm:?} in the last {}",
            fmt_dur(Duration::from_secs(since))
        );
        return 0;
    }
    let mem_total: i64 = conn
        .query_row(
            "SELECT mem_total_kb FROM sample WHERE mem_total_kb IS NOT NULL ORDER BY ts DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    println!("{comm} — last {}\n", fmt_dur(Duration::from_secs(since)));
    println!(
        "  cpu time   {:<10} {:.1}% of one core   peak {:.0}%   avg {:.1}% while present",
        fmt_dur(Duration::from_secs(a.cpu_secs as u64)),
        a.cpu_secs / window as f64 * 100.0,
        a.cpu_max,
        a.cpu_avg
    );
    println!(
        "  rss        peak {}   avg {}   now {}{}",
        fmt_bytes(a.rss_max_kb as u64 * 1024),
        fmt_bytes(a.rss_avg_kb as u64 * 1024),
        if a.rss_last_kb > 0 { fmt_bytes(a.rss_last_kb as u64 * 1024) } else { "—".into() },
        if mem_total > 0 {
            format!("   ({:.0}% of ram at peak)", a.rss_max_kb as f64 / mem_total as f64 * 100.0)
        } else {
            String::new()
        }
    );
    println!(
        "  io         read {}   written {}   peak {}/s",
        fmt_bytes(a.io_rd_bytes as u64),
        fmt_bytes(a.io_wr_bytes as u64),
        fmt_bytes(a.io_max_ps as u64)
    );
    println!(
        "  presence   {} samples   {} pid(s){}   {} → {}",
        a.samples,
        a.pids_seen,
        if a.pids_max > 1 { format!(", up to {} at once", a.pids_max) } else { String::new() },
        fmt_ts(a.first_ts),
        fmt_ts(a.last_ts)
    );

    let incs = db::app_incidents(&conn, comm, from, now).unwrap_or_default();
    if incs.is_empty() {
        println!("  incidents  none — it never triggered a warning");
    } else {
        println!("  incidents  {} it was blamed for:", incs.len());
        for i in &incs {
            let d = i
                .ended
                .map(|e| fmt_dur(Duration::from_secs((e - i.started).max(0) as u64)))
                .unwrap_or_else(|| "ongoing".into());
            println!(
                "               {} [{}] {}  peak {}",
                fmt_ts(i.started),
                i.kind,
                d,
                i.peak_avg60.map(|p| format!("{p:.0}%")).unwrap_or_else(|| "?".into())
            );
        }
    }

    let pids = db::app_pids(&conn, comm, from, now, top_pids).unwrap_or_default();
    if pids.len() > 1 || a.pids_seen > 1 {
        println!("\n  {:<8} {:>10} {:>8} {:>11}  {}", "pid", "rss peak", "cpu pk", "io peak", "seen");
        for p in &pids {
            println!(
                "  {:<8} {:>10} {:>7.0}% {:>9}/s  {} → {}",
                p.pid,
                fmt_bytes(p.rss_max_kb as u64 * 1024),
                p.cpu_max,
                fmt_bytes(p.io_max_ps as u64),
                fmt_ts(p.first_ts),
                fmt_ts(p.last_ts)
            );
        }
    }
    0
}

/// [avg10, avg60, avg300] of a pressure file's "some" line, as "N.NN%".
fn psi_avgs(path: &str) -> [String; 3] {
    match sample::read_psi(path) {
        Some(p) => [
            format!("{:.2}%", p.avg10),
            format!("{:.2}%", p.avg60),
            format!("{:.2}%", p.avg300),
        ],
        None => ["?".into(), "?".into(), "?".into()],
    }
}

/// Live full-system report, shown in the terminal window a toast click opens
/// (also usable directly: `busywatch detail`).
fn cmd_detail(db_path: &Path) -> i32 {
    println!("busywatch — system detail — {}\n", fmt_ts(unix_now() as i64));

    println!("pressure      avg10     avg60    avg300");
    for (name, path) in [("cpu", PSI_CPU), ("memory", PSI_MEM), ("io", PSI_IO)] {
        let [a10, a60, a300] = psi_avgs(path);
        println!("  {name:<9} {a10:>8} {a60:>9} {a300:>9}");
    }

    println!("\nload {}  ({} cores)", read_loadavg(), sample::cores());
    let m = read_meminfo();
    println!(
        "mem  {} total, {} available ({:.0}% used){}",
        fmt_bytes(m.total_kb * 1024),
        fmt_bytes(m.avail_kb * 1024),
        m.used_pct(),
        if m.swap_total_kb > 0 {
            format!(
                "\nswap {} used of {}",
                fmt_bytes(m.swap_used_kb * 1024),
                fmt_bytes(m.swap_total_kb * 1024)
            )
        } else {
            String::new()
        }
    );
    if let Some(l) = sample::read_power().summary() {
        println!("power {l}");
    }
    if let Some(l) = sample::read_cpu_clock().summary() {
        println!("cpu   {l}");
    }
    if let Some(l) = sample::read_thermal().summary() {
        println!("heat  {l}");
    }

    print!("\nsampling processes for 1s…");
    let _ = std::io::stdout().flush();
    let cons = sample::sample_consumers(Duration::from_millis(1000));
    println!("\r                           \r");

    println!("top CPU:");
    let top_cpu = top_by(&cons, 8, |c| c.cpu_pct, 1.0);
    if top_cpu.is_empty() {
        println!("  (idle)");
    }
    for c in top_cpu {
        println!("  {:>5.1}%  {} ({})", c.cpu_pct, c.comm, c.pid);
    }
    println!("top IO:");
    let top_io = top_by(&cons, 5, io_rate, 1.0);
    if top_io.is_empty() {
        println!("  (none)");
    }
    for c in top_io {
        println!(
            "  rd {:>7}/s  wr {:>7}/s  {} ({})",
            fmt_bytes(c.io_rd_ps),
            fmt_bytes(c.io_wr_ps),
            c.comm,
            c.pid
        );
    }
    println!("top MEM:");
    for c in top_by(&cons, 5, |c| c.rss_kb as f64, 1.0) {
        println!("  {:>8}  {} ({})", fmt_bytes(c.rss_kb * 1024), c.comm, c.pid);
    }

    if db_path.exists() {
        println!("\nmemory hogs, last 24h:");
        cmd_hogs(db_path, "mem", 86400, 5);
        println!("\nrecent incidents:");
        cmd_history(db_path, 3);
    }
    0
}

fn main() {
    // Rust ignores SIGPIPE, so `busywatch hogs | head` panics on the write
    // that follows the closed pipe.  Restore the default: die quietly.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };

    let args: Vec<String> = std::env::args().skip(1).collect();
    let sub = args.first().map(String::as_str).unwrap_or("");
    let rest = if args.is_empty() { &[][..] } else { &args[1..] };

    match sub {
        "history" => {
            let (mut n, mut db) = (10usize, default_db_path());
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--db" => db = it.next().map(PathBuf::from).unwrap_or_else(|| usage()),
                    s => n = s.parse().unwrap_or_else(|_| usage()),
                }
            }
            std::process::exit(cmd_history(&db, n));
        }
        "hogs" => {
            let (mut metric, mut since, mut top, mut db) =
                ("mem".to_string(), 86400u64, 15usize, default_db_path());
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                let mut next = || it.next().cloned().unwrap_or_else(|| usage());
                match a.as_str() {
                    "--by" => metric = next(),
                    "--since" => since = parse_span(&next()).unwrap_or_else(|| usage()),
                    "--top" => top = next().parse().unwrap_or_else(|_| usage()),
                    "--db" => db = PathBuf::from(next()),
                    _ => usage(),
                }
            }
            if !["mem", "cpu", "io"].contains(&metric.as_str()) {
                usage();
            }
            std::process::exit(cmd_hogs(&db, &metric, since, top));
        }
        "app" => {
            let (mut comm, mut since, mut db) = (String::new(), 86400u64, default_db_path());
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                let mut next = || it.next().cloned().unwrap_or_else(|| usage());
                match a.as_str() {
                    "--since" => since = parse_span(&next()).unwrap_or_else(|| usage()),
                    "--db" => db = PathBuf::from(next()),
                    s if s.starts_with("--") => usage(),
                    s => comm = s.to_string(),
                }
            }
            if comm.is_empty() {
                usage();
            }
            std::process::exit(cmd_app(&db, &comm, since, 30));
        }
        "web" => {
            let (mut addr, mut db) = (DEFAULT_ADDR.to_string(), default_db_path());
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                let mut next = || it.next().cloned().unwrap_or_else(|| usage());
                match a.as_str() {
                    "--bind" | "--port" => addr = web::normalize_addr(&next()),
                    "--db" => db = PathBuf::from(next()),
                    _ => usage(),
                }
            }
            std::process::exit(web::serve(&addr, &db));
        }
        "detail" => {
            let mut db = default_db_path();
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--db" => db = it.next().map(PathBuf::from).unwrap_or_else(|| usage()),
                    _ => usage(),
                }
            }
            std::process::exit(cmd_detail(&db));
        }
        _ => watch(parse_args(args)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_snap_to_a_ui_range() {
        assert_eq!(snap_span(60), 900); // a short incident still gets 15m
        assert_eq!(snap_span(1000), 3600);
        assert_eq!(snap_span(90_000), 259_200);
        assert_eq!(snap_span(u64::MAX), 2_592_000); // never past the last button
    }

    #[test]
    fn spans_parse() {
        assert_eq!(util::parse_span("45m"), Some(2700));
        assert_eq!(util::parse_span("7d"), Some(604_800));
        assert_eq!(util::parse_span("90"), Some(90));
        assert_eq!(util::parse_span("soon"), None);
    }
}

#[cfg(test)]
mod dbus_tests {
    use crate::dbus::{Dec, Enc, Msg, METHOD_CALL};

    /// A round-trip through the header encoder is the cheapest guard against
    /// the alignment mistakes that make a bus drop the connection with no
    /// diagnostic at all.
    #[test]
    fn header_round_trips() {
        let mut b = Enc::new();
        b.string("hello");
        let m = Msg::call("org.kde.X", "/org/kde/X", "org.kde.I", "Do").with_body("s", b.buf);
        let wire = super::dbus::encode_for_test(&m, 7);
        let back = super::dbus::decode_for_test(&wire).expect("decodes");
        assert_eq!(back.kind, METHOD_CALL);
        assert_eq!(back.serial, 7);
        assert_eq!(back.path.as_deref(), Some("/org/kde/X"));
        assert_eq!(back.iface.as_deref(), Some("org.kde.I"));
        assert_eq!(back.member.as_deref(), Some("Do"));
        assert_eq!(back.sig.as_deref(), Some("s"));
        assert_eq!(Dec::new(&back.body).string().as_deref(), Some("hello"));
    }

    /// Arrays carry a byte count, not an element count, and the count
    /// excludes the padding between it and the first element.
    #[test]
    fn array_length_counts_bytes_after_padding() {
        let mut e = Enc::new();
        e.byte(0); // push the array off an 8-boundary
        e.array(8, |e| {
            e.strukt(|e| e.i32(1));
            e.strukt(|e| e.i32(2));
        });
        // len u32 at offset 4 (aligned), first struct at 8, second at 16.
        let len = u32::from_le_bytes(e.buf[4..8].try_into().unwrap()) as usize;
        assert_eq!(len, 12, "two 8-aligned structs of one i32");
        assert_eq!(e.buf.len(), 8 + len);
    }
}

#[cfg(test)]
mod overlay_tests {
    use super::overlay_url;

    #[test]
    fn loopback_urls_get_the_alias() {
        assert_eq!(
            overlay_url("http://127.0.0.1:8787/#span=900"),
            "http://busywatch.localhost:8787/#span=900"
        );
        assert_eq!(overlay_url("http://localhost:9000/"), "http://busywatch.localhost:9000/");
    }

    /// A UI bound to a real address must keep that address: rewriting it to a
    /// loopback alias would open a window on the wrong machine's data.
    #[test]
    fn other_hosts_are_left_alone() {
        assert_eq!(overlay_url("http://192.168.1.9:8787/"), "http://192.168.1.9:8787/");
        assert_eq!(overlay_url("http://box.lan:8787/"), "http://box.lan:8787/");
    }
}
