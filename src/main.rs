//! busywatch — warn when the system is too busy, at near-zero cost.
//!
//! Instead of polling, it registers a PSI (pressure stall information)
//! trigger with the kernel and sleeps in poll() until the kernel itself
//! reports that tasks were stalled on CPU for more than the trigger
//! threshold inside the window.  While the system is healthy this process
//! uses no CPU at all.
//!
//! A trigger event alone is not a warning: transient spikes (a compile, a
//! page load) fire triggers constantly.  On each wakeup the sustained
//! pressure average (avg60) is checked, and only when it exceeds the
//! --sustained threshold is a warning issued — with the top CPU consumers
//! sampled at that moment, so the warning names the culprit.
//!
//! If registering the trigger is not permitted (some kernels restrict
//! unprivileged PSI triggers), it falls back to reading avg10/avg60 every
//! --poll-secs seconds, which is one small file read — still negligible.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const PSI_CPU: &str = "/proc/pressure/cpu";

struct Config {
    stall_ms: u64,      // trigger: stall time within window
    window_ms: u64,     // trigger: window size
    sustained: f64,     // warn when avg60 "some" >= this percentage
    cooldown: u64,      // seconds between warnings
    top: usize,         // processes to name in the warning
    poll_secs: u64,     // fallback polling interval
    notify: bool,       // send a desktop notification (else log only)
}

impl Default for Config {
    fn default() -> Self {
        Config {
            stall_ms: 500,
            window_ms: 1000,
            sustained: 20.0,
            cooldown: 300,
            top: 3,
            poll_secs: 10,
            notify: true,
        }
    }
}

fn usage() -> ! {
    eprintln!(
        "usage: busywatch [--stall-ms N] [--window-ms N] [--sustained PCT]\n\
         \x20                [--cooldown SECS] [--top N] [--poll-secs N] [--no-notify]"
    );
    std::process::exit(2);
}

fn parse_args() -> Config {
    let mut cfg = Config::default();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let val = |args: &mut dyn Iterator<Item = String>| -> String {
            args.next().unwrap_or_else(|| usage())
        };
        match a.as_str() {
            "--stall-ms" => cfg.stall_ms = val(&mut args).parse().unwrap_or_else(|_| usage()),
            "--window-ms" => cfg.window_ms = val(&mut args).parse().unwrap_or_else(|_| usage()),
            "--sustained" => cfg.sustained = val(&mut args).parse().unwrap_or_else(|_| usage()),
            "--cooldown" => cfg.cooldown = val(&mut args).parse().unwrap_or_else(|_| usage()),
            "--top" => cfg.top = val(&mut args).parse().unwrap_or_else(|_| usage()),
            "--poll-secs" => cfg.poll_secs = val(&mut args).parse().unwrap_or_else(|_| usage()),
            "--no-notify" => cfg.notify = false,
            "-h" | "--help" => usage(),
            _ => usage(),
        }
    }
    cfg
}

fn log(msg: &str) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // journald adds its own timestamp; this one helps plain-file logs.
    println!("[{secs}] {msg}");
}

/// Parse `some avg10=1.50 avg60=1.61 ...` -> (avg10, avg60)
fn read_psi_some() -> Option<(f64, f64)> {
    let text = fs::read_to_string(PSI_CPU).ok()?;
    let line = text.lines().find(|l| l.starts_with("some"))?;
    let mut avg10 = None;
    let mut avg60 = None;
    for field in line.split_whitespace() {
        if let Some(v) = field.strip_prefix("avg10=") {
            avg10 = v.parse().ok();
        } else if let Some(v) = field.strip_prefix("avg60=") {
            avg60 = v.parse().ok();
        }
    }
    Some((avg10?, avg60?))
}

fn read_loadavg() -> String {
    fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|s| s.split_whitespace().take(3).map(String::from).reduce(|a, b| a + " " + &b))
        .unwrap_or_else(|| "?".into())
}

/// (utime+stime ticks, comm) per pid.  comm sits in parens and may contain
/// spaces or parens itself, so split on the LAST ')'.
fn proc_cpu_ticks() -> Vec<(u32, u64, String)> {
    let mut out = Vec::new();
    let Ok(dir) = fs::read_dir("/proc") else { return out };
    for entry in dir.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else { continue };
        let Some(open) = stat.find('(') else { continue };
        let Some(close) = stat.rfind(')') else { continue };
        let comm = stat[open + 1..close].to_string();
        let rest: Vec<&str> = stat[close + 1..].split_whitespace().collect();
        // rest[0] is state; utime and stime are fields 14 and 15 of the full
        // stat line, i.e. rest[11] and rest[12].
        let (Some(ut), Some(st)) = (rest.get(11), rest.get(12)) else { continue };
        let (Ok(ut), Ok(st)) = (ut.parse::<u64>(), st.parse::<u64>()) else { continue };
        out.push((pid, ut + st, comm));
    }
    out
}

/// Sample process CPU over `dur` and return the top consumers as
/// "name pid%" strings.  Only runs when a warning is about to be issued.
fn top_consumers(n: usize, dur: Duration) -> Vec<String> {
    let t0 = Instant::now();
    let before = proc_cpu_ticks();
    std::thread::sleep(dur);
    let after = proc_cpu_ticks();
    let tick_hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64;
    // Under the very load being reported, sleep() oversleeps and the /proc
    // sweeps take real time — divide by measured elapsed, not intended.
    let interval = t0.elapsed().as_secs_f64();

    let mut prev = std::collections::HashMap::new();
    for (pid, ticks, _) in &before {
        prev.insert(*pid, *ticks);
    }
    let mut deltas: Vec<(f64, String, u32)> = after
        .into_iter()
        .filter_map(|(pid, ticks, comm)| {
            let d = ticks.saturating_sub(*prev.get(&pid)?) as f64;
            let pct = d / tick_hz / interval * 100.0;
            (pct >= 1.0).then_some((pct, comm, pid))
        })
        .collect();
    deltas.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    deltas
        .into_iter()
        .take(n)
        .map(|(pct, comm, pid)| format!("{comm}({pid}) {pct:.0}%"))
        .collect()
}

fn send_notification(summary: &str, body: &str) {
    // The synchronous hint makes notification daemons replace the previous
    // busywatch toast instead of stacking them.
    let _ = Command::new("notify-send")
        .args([
            "-u", "critical",
            "-a", "busywatch",
            "-h", "string:x-canonical-private-synchronous:busywatch",
            summary, body,
        ])
        .status();
}

fn warn(cfg: &Config, avg10: f64, avg60: f64) {
    let culprits = top_consumers(cfg.top, Duration::from_millis(500));
    let who = if culprits.is_empty() { "(no single hot process)".into() } else { culprits.join(", ") };
    let body = format!(
        "CPU pressure avg10={avg10:.0}% avg60={avg60:.0}% load={}\ntop: {who}",
        read_loadavg()
    );
    log(&format!("WARNING system busy — {}", body.replace('\n', " — ")));
    if cfg.notify {
        send_notification("System is too busy", &body);
    }
}

/// Register a PSI trigger; returns the fd to poll, or None if not permitted.
fn psi_trigger(cfg: &Config) -> Option<std::fs::File> {
    let mut f = match OpenOptions::new().read(true).write(true).open(PSI_CPU) {
        Ok(f) => f,
        Err(e) => {
            log(&format!("cannot open {PSI_CPU} read-write: {e}"));
            return None;
        }
    };
    // The kernel NUL-terminates the buffer in place, clobbering the last
    // byte — write strlen+1 bytes like the kernel docs' example does.
    let spec = format!("some {} {}\0", cfg.stall_ms * 1000, cfg.window_ms * 1000);
    if let Err(e) = f.write_all(spec.as_bytes()) {
        log(&format!("cannot register PSI trigger ({spec:?}): {e}"));
        return None;
    }
    Some(f)
}

fn main() {
    let cfg = parse_args();
    let mut last_warn: Option<Instant> = None;

    let mut maybe_warn = |avg10: f64, avg60: f64| {
        if avg60 < cfg.sustained {
            return;
        }
        if let Some(t) = last_warn {
            if t.elapsed().as_secs() < cfg.cooldown {
                return;
            }
        }
        last_warn = Some(Instant::now());
        warn(&cfg, avg10, avg60);
    };

    match psi_trigger(&cfg) {
        Some(trigger) => {
            log(&format!(
                "watching {PSI_CPU} via kernel trigger (some {}ms/{}ms), warn at avg60>={}%, cooldown {}s",
                cfg.stall_ms, cfg.window_ms, cfg.sustained, cfg.cooldown
            ));
            let fd = trigger.as_raw_fd();
            loop {
                let mut pfd = libc::pollfd { fd, events: libc::POLLPRI, revents: 0 };
                let rc = unsafe { libc::poll(&mut pfd, 1, -1) };
                if rc < 0 {
                    let err = std::io::Error::last_os_error();
                    if err.kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    log(&format!("poll failed: {err}"));
                    std::process::exit(1);
                }
                if pfd.revents & libc::POLLERR != 0 {
                    log("PSI trigger fd went bad (POLLERR)");
                    std::process::exit(1);
                }
                if pfd.revents & libc::POLLPRI != 0 {
                    if let Some((avg10, avg60)) = read_psi_some() {
                        maybe_warn(avg10, avg60);
                    }
                    // At most one event per window is generated, so looping
                    // straight back into poll() is cheap even under load.
                }
            }
        }
        None => {
            log(&format!(
                "PSI trigger not permitted — falling back to sampling every {}s",
                cfg.poll_secs
            ));
            loop {
                if let Some((avg10, avg60)) = read_psi_some() {
                    maybe_warn(avg10, avg60);
                }
                std::thread::sleep(Duration::from_secs(cfg.poll_secs));
            }
        }
    }
}
