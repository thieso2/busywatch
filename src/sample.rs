//! Reading the kernel's view of the system: PSI pressure, meminfo, and a
//! /proc sweep turned into per-process CPU / IO / RSS consumers.

use std::collections::HashMap;
use std::fs;
use std::time::{Duration, Instant};

use crate::util::fmt_bytes;

pub const PSI_CPU: &str = "/proc/pressure/cpu";
pub const PSI_MEM: &str = "/proc/pressure/memory";
pub const PSI_IO: &str = "/proc/pressure/io";

#[derive(Clone, Copy, Default, Debug)]
pub struct Psi {
    pub avg10: f64,
    pub avg60: f64,
    pub avg300: f64,
}

/// Parse `some avg10=1.50 avg60=1.61 avg300=0.42 total=…` of a pressure file.
pub fn read_psi(path: &str) -> Option<Psi> {
    let text = fs::read_to_string(path).ok()?;
    let line = text.lines().find(|l| l.starts_with("some"))?;
    let mut p = Psi::default();
    for field in line.split_whitespace() {
        if let Some(v) = field.strip_prefix("avg10=") {
            p.avg10 = v.parse().ok()?;
        } else if let Some(v) = field.strip_prefix("avg60=") {
            p.avg60 = v.parse().ok()?;
        } else if let Some(v) = field.strip_prefix("avg300=") {
            p.avg300 = v.parse().ok()?;
        }
    }
    Some(p)
}

/// All three pressures at once; missing files read as zero pressure.
#[derive(Clone, Copy, Default, Debug)]
pub struct Pressures {
    pub cpu: Psi,
    pub mem: Psi,
    pub io: Psi,
}

pub fn read_pressures() -> Pressures {
    Pressures {
        cpu: read_psi(PSI_CPU).unwrap_or_default(),
        mem: read_psi(PSI_MEM).unwrap_or_default(),
        io: read_psi(PSI_IO).unwrap_or_default(),
    }
}

#[derive(Clone, Copy, Default, Debug)]
pub struct MemInfo {
    pub total_kb: u64,
    pub avail_kb: u64,
    pub swap_total_kb: u64,
    pub swap_used_kb: u64,
}

impl MemInfo {
    /// Percentage of RAM that is *not* available — what "memory used" means
    /// for a human (MemAvailable already discounts reclaimable cache).
    pub fn used_pct(&self) -> f64 {
        if self.total_kb == 0 {
            return 0.0;
        }
        (self.total_kb.saturating_sub(self.avail_kb)) as f64 / self.total_kb as f64 * 100.0
    }
    pub fn avail_pct(&self) -> f64 {
        if self.total_kb == 0 {
            return 100.0;
        }
        self.avail_kb as f64 / self.total_kb as f64 * 100.0
    }
}

pub fn read_meminfo() -> MemInfo {
    let mut m = MemInfo::default();
    let mut swap_free = 0u64;
    if let Ok(s) = fs::read_to_string("/proc/meminfo") {
        for l in s.lines() {
            let kb = || -> u64 {
                l.split_whitespace().nth(1).and_then(|v| v.parse().ok()).unwrap_or(0)
            };
            if l.starts_with("MemTotal:") {
                m.total_kb = kb();
            } else if l.starts_with("MemAvailable:") {
                m.avail_kb = kb();
            } else if l.starts_with("SwapTotal:") {
                m.swap_total_kb = kb();
            } else if l.starts_with("SwapFree:") {
                swap_free = kb();
            }
        }
    }
    m.swap_used_kb = m.swap_total_kb.saturating_sub(swap_free);
    m
}

pub fn read_loadavg() -> String {
    fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|s| s.split_whitespace().take(3).map(String::from).reduce(|a, b| a + " " + &b))
        .unwrap_or_else(|| "?".into())
}

pub fn load1() -> f64 {
    fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|s| s.split_whitespace().next().and_then(|f| f.parse().ok()))
        .unwrap_or(0.0)
}

pub fn cores() -> i64 {
    unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) as i64 }
}

// ------------------------------------------------------------------- power

/// What the battery is doing.  An enum rather than the raw sysfs string so a
/// `Reading` stays `Copy`; unrecognised values become `Unknown` rather than
/// being dropped, so a kernel that grows a new state still records something.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BatStatus {
    Charging,
    Discharging,
    Full,
    NotCharging,
    Unknown,
}

impl BatStatus {
    pub fn parse(s: &str) -> BatStatus {
        match s.trim() {
            "Charging" => BatStatus::Charging,
            "Discharging" => BatStatus::Discharging,
            "Full" => BatStatus::Full,
            "Not charging" => BatStatus::NotCharging,
            _ => BatStatus::Unknown,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            BatStatus::Charging => "Charging",
            BatStatus::Discharging => "Discharging",
            BatStatus::Full => "Full",
            BatStatus::NotCharging => "Not charging",
            BatStatus::Unknown => "Unknown",
        }
    }
}

/// Mains and battery state.  Every field is optional: a desktop has no
/// battery, a VM has no power supplies at all, and `None` must stay
/// distinguishable from a real zero.
#[derive(Clone, Copy, Default, Debug)]
pub struct Power {
    /// True when a supply of type Mains (or UPS) reports online.  A USB-C
    /// charger the firmware does not accept as an adapter reads *false* here
    /// while still charging — which is exactly the case worth recording.
    pub ac_online: Option<bool>,
    pub bat_pct: Option<f64>,
    pub bat_status: Option<BatStatus>,
    /// Signed microwatts: negative while discharging, positive while
    /// charging.  The sysfs counters are unsigned magnitudes, so the sign
    /// comes from `bat_status`.
    pub bat_power_uw: Option<i64>,
}

impl Power {
    /// One human line, or None when there is nothing to say — a desktop with
    /// no battery and no mains supply exposed reports nothing rather than a
    /// row of blanks.
    pub fn summary(&self) -> Option<String> {
        let mut parts = Vec::new();
        match self.ac_online {
            Some(true) => parts.push("on AC".to_string()),
            Some(false) => parts.push("on battery (AC offline)".to_string()),
            None => {}
        }
        if let Some(pct) = self.bat_pct {
            let mut s = format!("battery {pct:.0}%");
            if let Some(st) = self.bat_status {
                s.push(' ');
                s.push_str(&st.as_str().to_lowercase());
            }
            match self.bat_power_uw {
                Some(uw) if uw != 0 => s.push_str(&format!(" at {:.1}W", uw.abs() as f64 / 1e6)),
                _ => {}
            }
            parts.push(s);
        }
        (!parts.is_empty()).then(|| parts.join(", "))
    }
}

fn fmt_khz(khz: u64) -> String {
    if khz >= 1_000_000 {
        format!("{:.1}GHz", khz as f64 / 1e6)
    } else {
        format!("{}MHz", khz / 1000)
    }
}

impl CpuClock {
    pub fn summary(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(cur) = self.freq_khz {
            parts.push(match self.freq_max_khz {
                Some(max) => format!("{} of {} max", fmt_khz(cur), fmt_khz(max)),
                None => fmt_khz(cur),
            });
        }
        if let Some(n) = self.throttle_count {
            parts.push(format!(
                "throttled {n}x / {:.1}s since boot",
                self.throttle_ms.unwrap_or(0) as f64 / 1000.0
            ));
        }
        (!parts.is_empty()).then(|| parts.join("   "))
    }
}

pub fn read_power() -> Power {
    let mut p = Power::default();
    let Ok(dir) = fs::read_dir("/sys/class/power_supply") else { return p };
    let mut paths: Vec<_> = dir.flatten().map(|e| e.path()).collect();
    // Sorted so a two-battery machine always reports the same one.
    paths.sort();
    for path in paths {
        let text = |f: &str| fs::read_to_string(path.join(f)).ok().map(|s| s.trim().to_string());
        let num = |f: &str| text(f).and_then(|s| s.parse::<i64>().ok());
        match text("type").as_deref() {
            Some("Mains") | Some("UPS") => {
                if num("online") == Some(1) {
                    p.ac_online = Some(true);
                } else {
                    // Never demote a supply already found online.
                    p.ac_online.get_or_insert(false);
                }
            }
            // First present battery wins; `present` is 0 for an empty bay.
            Some("Battery") if p.bat_status.is_none() && num("present") != Some(0) => {
                p.bat_pct = num("capacity").map(|v| v as f64);
                p.bat_status = text("status").map(|s| BatStatus::parse(&s));
                // power_now is microwatts directly; batteries that lack it
                // expose current and voltage instead.
                let uw = num("power_now").or_else(|| {
                    let (i, v) = (num("current_now")?, num("voltage_now")?);
                    Some((i as i128 * v as i128 / 1_000_000) as i64)
                });
                p.bat_power_uw = uw.map(|w| {
                    if p.bat_status == Some(BatStatus::Discharging) { -w.abs() } else { w.abs() }
                });
            }
            _ => {}
        }
    }
    p
}

// --------------------------------------------------------- clock & throttle

/// CPU frequency and the kernel's cumulative throttle counters.
///
/// `freq_khz` is worth recording but NOT worth trusting on its own: on some
/// platforms `scaling_cur_freq` is stuck at the minimum permanently, under
/// load included.  `freq_max_khz` is the reliable half — when something caps
/// the CPU in software it shows up there — and the throttle counters are
/// cumulative since boot, so it is the *delta* between two samples that says
/// whether the window throttled.
#[derive(Clone, Copy, Default, Debug)]
pub struct CpuClock {
    pub freq_khz: Option<u64>,
    pub freq_max_khz: Option<u64>,
    pub throttle_count: Option<u64>,
    pub throttle_ms: Option<u64>,
}

pub fn read_cpu_clock() -> CpuClock {
    let mut c = CpuClock::default();
    let (mut cur, mut ncur, mut max, mut nmax) = (0u64, 0u64, 0u64, 0u64);
    for i in 0..cores() {
        let base = format!("/sys/devices/system/cpu/cpu{i}/cpufreq");
        let rd = |f: &str| {
            fs::read_to_string(format!("{base}/{f}")).ok().and_then(|s| s.trim().parse::<u64>().ok())
        };
        if let Some(v) = rd("scaling_cur_freq") {
            cur += v;
            ncur += 1;
        }
        if let Some(v) = rd("scaling_max_freq") {
            max += v;
            nmax += 1;
        }
    }
    if ncur > 0 {
        c.freq_khz = Some(cur / ncur);
    }
    if nmax > 0 {
        c.freq_max_khz = Some(max / nmax);
    }
    // These are package-wide counters; cpu0 carries them for the single-socket
    // machines this runs on.  Absent entirely on AMD and in VMs.
    let rd = |f: &str| {
        fs::read_to_string(format!("/sys/devices/system/cpu/cpu0/thermal_throttle/{f}"))
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
    };
    c.throttle_count = rd("package_throttle_count");
    c.throttle_ms = rd("package_throttle_total_time_ms");
    c
}

// ---------------------------------------------------------------- sweeping

pub struct ProcSnap {
    pub pid: u32,
    pub comm: String,
    pub ticks: u64, // utime+stime
    pub io_rd: u64, // cumulative read_bytes (0 if /proc/pid/io unreadable)
    pub io_wr: u64,
    pub rss_kb: u64,
}

/// One /proc sweep: CPU ticks, IO byte counters, RSS per pid.  comm sits in
/// parens and may contain spaces or parens itself, so split on the LAST ')'.
pub fn proc_snapshot() -> Vec<ProcSnap> {
    let page_kb = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64 / 1024;
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
        // /proc/pid/io needs same-uid (or privileges) — 0 for other users' procs.
        let (mut io_rd, mut io_wr) = (0u64, 0u64);
        if let Ok(io) = fs::read_to_string(format!("/proc/{pid}/io")) {
            for l in io.lines() {
                if let Some(v) = l.strip_prefix("read_bytes: ") {
                    io_rd = v.trim().parse().unwrap_or(0);
                } else if let Some(v) = l.strip_prefix("write_bytes: ") {
                    io_wr = v.trim().parse().unwrap_or(0);
                }
            }
        }
        let rss_kb = fs::read_to_string(format!("/proc/{pid}/statm"))
            .ok()
            .and_then(|s| s.split_whitespace().nth(1).and_then(|v| v.parse::<u64>().ok()))
            .map(|pages| pages * page_kb)
            .unwrap_or(0);
        out.push(ProcSnap { pid, comm, ticks: ut + st, io_rd, io_wr, rss_kb });
    }
    out
}

#[derive(Clone, Debug)]
pub struct Consumer {
    pub pid: u32,
    pub comm: String,
    pub cpu_pct: f64,
    pub io_rd_ps: u64, // bytes/sec over the sample window
    pub io_wr_ps: u64,
    pub rss_kb: u64,
}

fn diff(before: &[ProcSnap], after: &[ProcSnap], interval: f64) -> Vec<Consumer> {
    let tick_hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64;
    let interval = interval.max(0.001);
    // Our own pid is dropped: busywatch is awake only during the sample
    // window, so its CPU there is the cost of the measurement itself and
    // would otherwise top the list of "hot" processes it just reported.
    let own = std::process::id();
    let mut prev = HashMap::with_capacity(before.len());
    for s in before {
        prev.insert(s.pid, (s.ticks, s.io_rd, s.io_wr));
    }
    after
        .iter()
        .filter_map(|s| {
            if s.pid == own {
                return None;
            }
            // A pid absent from the previous sweep is new: no delta to take.
            let &(pt, prd, pwr) = prev.get(&s.pid)?;
            Some(Consumer {
                cpu_pct: s.ticks.saturating_sub(pt) as f64 / tick_hz / interval * 100.0,
                io_rd_ps: (s.io_rd.saturating_sub(prd) as f64 / interval) as u64,
                io_wr_ps: (s.io_wr.saturating_sub(pwr) as f64 / interval) as u64,
                pid: s.pid,
                comm: s.comm.clone(),
                rss_kb: s.rss_kb,
            })
        })
        .collect()
}

/// Keeps the previous /proc sweep so a sample only costs *one* sweep and the
/// rates cover the whole gap since the last one — which is what the history
/// wants (an average over the minute, not over an arbitrary 500 ms window).
pub struct Tracker {
    prev: Vec<ProcSnap>,
    at: Instant,
}

impl Default for Tracker {
    fn default() -> Self {
        Tracker { prev: proc_snapshot(), at: Instant::now() }
    }
}

impl Tracker {
    /// Consumers since the previous sweep, and how long that spanned.  If the
    /// previous sweep is too recent to give meaningful rates, waits `min` for
    /// a usable window instead of reporting noise.
    pub fn sample(&mut self, min: Duration) -> (Vec<Consumer>, Duration) {
        let waited = self.at.elapsed();
        if waited < min {
            std::thread::sleep(min - waited);
        }
        let span = self.at.elapsed();
        let after = proc_snapshot();
        let cons = diff(&self.prev, &after, span.as_secs_f64());
        // The sweep just taken becomes the base for the next one, so a
        // periodic sample costs exactly one /proc sweep.
        self.prev = after;
        self.at = Instant::now();
        (cons, span)
    }
}

/// One-shot sample over `dur` (two sweeps), for callers with no Tracker.
pub fn sample_consumers(dur: Duration) -> Vec<Consumer> {
    let t0 = Instant::now();
    let before = proc_snapshot();
    std::thread::sleep(dur);
    // Under the very load being reported, sleep() oversleeps and the sweeps
    // take real time — divide by measured elapsed, not intended.
    let after = proc_snapshot();
    diff(&before, &after, t0.elapsed().as_secs_f64())
}

// ------------------------------------------------------------- formatting

pub fn top_by<'a>(
    cons: &'a [Consumer],
    n: usize,
    key: impl Fn(&Consumer) -> f64,
    min: f64,
) -> Vec<&'a Consumer> {
    let mut v: Vec<&Consumer> = cons.iter().filter(|c| key(c) >= min).collect();
    v.sort_by(|a, b| key(b).partial_cmp(&key(a)).unwrap_or(std::cmp::Ordering::Equal));
    v.truncate(n);
    v
}

pub fn io_rate(c: &Consumer) -> f64 {
    (c.io_rd_ps + c.io_wr_ps) as f64
}

pub fn fmt_top_cpu(cons: &[Consumer], n: usize) -> String {
    let top = top_by(cons, n, |c| c.cpu_pct, 1.0);
    if top.is_empty() {
        "(no single hot process)".into()
    } else {
        top.iter()
            .map(|c| format!("{}({}) {:.0}%", c.comm, c.pid, c.cpu_pct))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

pub fn fmt_top_io(cons: &[Consumer], n: usize) -> String {
    let top = top_by(cons, n, io_rate, 1.0);
    if top.is_empty() {
        "(none)".into()
    } else {
        top.iter()
            .map(|c| {
                format!(
                    "{}({}) rd {}/s wr {}/s",
                    c.comm,
                    c.pid,
                    fmt_bytes(c.io_rd_ps),
                    fmt_bytes(c.io_wr_ps)
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

pub fn fmt_top_mem(cons: &[Consumer], n: usize) -> String {
    let top = top_by(cons, n, |c| c.rss_kb as f64, 1.0);
    if top.is_empty() {
        "(none)".into()
    } else {
        top.iter()
            .map(|c| format!("{}({}) {}", c.comm, c.pid, fmt_bytes(c.rss_kb * 1024)))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// The avg fields of a pressure file's "some" line, e.g. "avg10=1.5 avg60=1.6".
pub fn psi_summary(path: &str) -> String {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| {
            s.lines().find(|l| l.starts_with("some")).map(|l| {
                l.split_whitespace()
                    .filter(|f| f.starts_with("avg"))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
        })
        .unwrap_or_else(|| "?".into())
}
