//! Reading the kernel's view of the system: PSI pressure, meminfo, and a
//! /proc sweep turned into per-process CPU / IO / RSS consumers.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
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

/// How the attached USB-C port is being fed.  `Default` is the 5V/900mA a
/// port gives away before anything is negotiated; the two current modes are
/// Type-C resistor advertisement with no PD contract at all; `Pd` means a
/// power-delivery contract was agreed, which is the only mode under which a
/// laptop-sized charger delivers laptop-sized watts.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PdMode {
    Default,
    Type1_5A,
    Type3_0A,
    Pd,
    Unknown,
}

impl PdMode {
    pub fn parse(s: &str) -> PdMode {
        match s.trim() {
            "default" => PdMode::Default,
            "1.5A" => PdMode::Type1_5A,
            "3.0A" => PdMode::Type3_0A,
            "usb_power_delivery" => PdMode::Pd,
            _ => PdMode::Unknown,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            PdMode::Default => "default",
            PdMode::Type1_5A => "1.5A",
            PdMode::Type3_0A => "3.0A",
            PdMode::Pd => "usb_power_delivery",
            PdMode::Unknown => "unknown",
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
    ///
    /// This is what crosses the battery terminals, *not* what the charger
    /// delivers: on mains the adapter is also feeding the running system, and
    /// no counter here reports that half.
    pub bat_power_uw: Option<i64>,
    /// Microwatt-hours in the battery now, and when full.  `energy_full`
    /// shrinks as the cells age, so the pair is both a fuel gauge (with a
    /// better resolution than the rounded whole-percent `capacity`) and, held
    /// against `bat_energy_design_uwh`, a health figure.
    pub bat_energy_uwh: Option<i64>,
    pub bat_energy_full_uwh: Option<i64>,
    /// What the pack held new.  Constant for the life of the battery, so it
    /// is reported live rather than recorded every minute.
    pub bat_energy_design_uwh: Option<i64>,
    /// Terminal voltage in microvolts.  Wanted only to turn watts into the
    /// amps a charger is actually pushing.
    pub bat_voltage_uv: Option<i64>,
    pub bat_cycles: Option<i64>,
    /// The negotiated Type-C power mode of whichever port is attached.
    pub pd_mode: Option<PdMode>,
    /// The most the charger *says* it can supply, in microwatts — the top of
    /// its advertised PD source capabilities.  `None` is the common case, not
    /// an error: plenty of firmware never passes the source PDOs up to the
    /// kernel, and then nothing on the machine knows the brick's rating.
    pub charger_max_uw: Option<i64>,
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
            if let Some((charging, secs)) = self.eta_secs() {
                s.push_str(&format!(
                    ", {} in {}",
                    if charging { "full" } else { "empty" },
                    fmt_eta(secs)
                ));
            }
            parts.push(s);
        }
        if let Some(h) = self.health_pct() {
            let mut s = format!("health {h:.0}%");
            if let (Some(f), Some(d)) = (self.bat_energy_full_uwh, self.bat_energy_design_uwh) {
                s.push_str(&format!(" ({:.1} of {:.1}Wh", f as f64 / 1e6, d as f64 / 1e6));
                match self.bat_cycles {
                    Some(c) => s.push_str(&format!(", {c} cycles)")),
                    None => s.push(')'),
                }
            }
            parts.push(s);
        }
        if let Some(uw) = self.charger_max_uw {
            parts.push(format!("charger rated {:.0}W", uw as f64 / 1e6));
        } else if self.pd_mode == Some(PdMode::Pd) {
            // Worth saying even without a number: it rules out the far more
            // common complaint, a laptop trickling off a phone charger.
            parts.push("charger negotiated PD".to_string());
        }
        (!parts.is_empty()).then(|| parts.join(", "))
    }

    /// Seconds until full while charging, or until empty while draining, with
    /// a flag saying which — `None` when the battery is idle, the rate is not
    /// reported, or the energy counters are missing.
    ///
    /// This is the naive projection at the current rate, and it is honest only
    /// about the next few minutes: charging tapers hard near the top, and a
    /// discharge estimate follows whatever the machine happens to be doing
    /// this second.  It is a reading, not a forecast.
    pub fn eta_secs(&self) -> Option<(bool, i64)> {
        let uw = self.bat_power_uw?;
        let now = self.bat_energy_uwh?;
        if uw > 0 {
            let full = self.bat_energy_full_uwh?;
            let togo = full.checked_sub(now).filter(|v| *v > 0)?;
            Some((true, (togo as i128 * 3600 / uw as i128) as i64))
        } else if uw < 0 && now > 0 {
            Some((false, (now as i128 * 3600 / -uw as i128) as i64))
        } else {
            None
        }
    }

    /// What is left of the pack, as a percentage of what it held new.  `None`
    /// on a battery that does not report a design capacity.
    pub fn health_pct(&self) -> Option<f64> {
        let (full, design) = (self.bat_energy_full_uwh?, self.bat_energy_design_uwh?);
        (design > 0).then(|| full as f64 / design as f64 * 100.0)
    }
}

/// "1h53m", "22m" — coarse on purpose, because the estimate behind it does not
/// support a finer figure.
fn fmt_eta(secs: i64) -> String {
    if secs >= 3600 {
        format!("{}h{:02}m", secs / 3600, secs % 3600 / 60)
    } else {
        format!("{}m", (secs / 60).max(1))
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
                p.bat_voltage_uv = num("voltage_now").filter(|v| *v > 0);
                // power_now is microwatts directly; batteries that lack it
                // expose current and voltage instead.
                let uw = num("power_now").or_else(|| {
                    let (i, v) = (num("current_now")?, num("voltage_now")?);
                    Some((i as i128 * v as i128 / 1_000_000) as i64)
                });
                p.bat_power_uw = uw.map(|w| {
                    if p.bat_status == Some(BatStatus::Discharging) { -w.abs() } else { w.abs() }
                });
                // Energy-reporting batteries give microwatt-hours outright;
                // charge-reporting ones give microamp-hours, which only become
                // comparable once multiplied by the terminal voltage.  Design
                // voltage is the wrong multiplier for `now` but the right one
                // for the two capacities, so each uses what it should.
                let uah_to_uwh = |uah: i64, uv: Option<i64>| -> Option<i64> {
                    Some((uah as i128 * uv? as i128 / 1_000_000) as i64)
                };
                let design_uv = num("voltage_min_design").or(p.bat_voltage_uv);
                p.bat_energy_uwh = num("energy_now")
                    .or_else(|| uah_to_uwh(num("charge_now")?, p.bat_voltage_uv));
                p.bat_energy_full_uwh = num("energy_full")
                    .or_else(|| uah_to_uwh(num("charge_full")?, design_uv));
                p.bat_energy_design_uwh = num("energy_full_design")
                    .or_else(|| uah_to_uwh(num("charge_full_design")?, design_uv));
                p.bat_cycles = num("cycle_count").filter(|v| *v > 0);
            }
            // The USB-C source side of a laptop being charged over Type-C.
            // `online` picks the port that is actually feeding us out of the
            // several a machine exposes.
            Some("USB") if num("online") == Some(1) => {
                if let (Some(uv), Some(ua)) = (num("voltage_max"), num("current_max")) {
                    let uw = (uv as i128 * ua as i128 / 1_000_000) as i64;
                    if uw > 0 {
                        p.charger_max_uw = Some(p.charger_max_uw.unwrap_or(0).max(uw));
                    }
                }
            }
            _ => {}
        }
    }
    let (mode, pdo_max) = read_typec();
    p.pd_mode = mode;
    // The advertised PDOs beat the power-supply pair when both exist: the
    // supply reports the contract in force, which on a half-charged laptop is
    // often well under what the brick can do.
    p.charger_max_uw = pdo_max.or(p.charger_max_uw);
    p
}

/// The attached port's negotiated power mode, and the biggest of the source
/// capabilities its partner advertises, in microwatts.
///
/// Both come out `None` on most machines.  The advertised capabilities are a
/// PD message the port controller has to hand up to the kernel, and firmware
/// behind an ACPI UCSI interface frequently does not: `usb_power_delivery`
/// then holds a `revision` and nothing else.  There is no other place on the
/// system to learn the charger's rating, so `None` here means the question is
/// unanswerable rather than merely unread.
fn read_typec() -> (Option<PdMode>, Option<i64>) {
    let (mut mode, mut max_uw) = (None, None);
    let Ok(dir) = fs::read_dir("/sys/class/typec") else { return (mode, max_uw) };
    let mut paths: Vec<_> = dir.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        if name.ends_with("-partner") {
            max_uw = max_uw.max(read_source_caps(&path.join("usb_power_delivery")));
            continue;
        }
        // A port's own mode only says something while a partner is attached;
        // an empty port sits at "default" and would otherwise outvote the one
        // holding a PD contract.
        if !path.join(format!("{name}-partner")).exists() {
            continue;
        }
        if let Some(m) = fs::read_to_string(path.join("power_operation_mode"))
            .ok()
            .map(|s| PdMode::parse(&s))
        {
            // Prefer the port that actually negotiated PD over one that did not.
            if mode != Some(PdMode::Pd) {
                mode = Some(m);
            }
        }
    }
    (mode, max_uw)
}

/// Walk one `usb_power_delivery` device's advertised source PDOs and return
/// the largest, in microwatts.
///
/// The three PDO shapes each spell their limit differently, and the units are
/// the ones the kernel's ABI fixes: millivolts, milliamps, milliwatts.
fn read_source_caps(pd: &Path) -> Option<i64> {
    let mut best: Option<i64> = None;
    for entry in fs::read_dir(pd.join("source-capabilities")).ok()?.flatten() {
        let dir = entry.path();
        let mv = |f: &str| -> Option<i64> {
            fs::read_to_string(dir.join(f)).ok()?.trim().parse::<i64>().ok()
        };
        // A fixed supply names its one voltage; the ranged shapes are worth
        // only their ceiling, which is what a charger is rated at.
        let uw = match (mv("voltage"), mv("maximum_voltage"), mv("maximum_current"), mv("maximum_power")) {
            (_, _, _, Some(mw)) => mw.checked_mul(1000),
            (Some(v), _, Some(i), _) => v.checked_mul(i),
            (None, Some(v), Some(i), _) => v.checked_mul(i),
            _ => None,
        };
        if let Some(uw) = uw.filter(|w| *w > 0) {
            best = Some(best.unwrap_or(0).max(uw));
        }
    }
    best
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

// ------------------------------------------------------------------ thermal

/// What the machine is doing about heat: how hot the CPU is, and how hard the
/// fan is working to keep it there.
///
/// Both are optional and often absent: a desktop may expose no fan tachometer
/// at all, a VM exposes neither, and which hwmon driver carries the CPU
/// temperature differs between Intel, AMD and everything else.
#[derive(Clone, Copy, Default, Debug)]
pub struct Thermal {
    /// Millidegrees C, the way hwmon reports it, so nothing is lost to
    /// rounding before it reaches the database.
    pub cpu_temp_mc: Option<i64>,
    /// The fastest fan on the machine. A laptop with two fans has one story —
    /// how hard is it working — and the loudest fan tells it.
    pub fan_rpm: Option<u64>,
    /// What that fan tops out at, where the driver says so. Only useful for
    /// showing the current speed as a share of it, so it is not recorded.
    pub fan_max_rpm: Option<u64>,
}

impl Thermal {
    pub fn summary(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(t) = self.cpu_temp_mc {
            parts.push(format!("{:.0}°C", t as f64 / 1000.0));
        }
        match (self.fan_rpm, self.fan_max_rpm) {
            (Some(0), _) => parts.push("fan idle".to_string()),
            (Some(r), Some(m)) if m > 0 => {
                parts.push(format!("fan {r} rpm ({:.0}% of {m})", r as f64 / m as f64 * 100.0))
            }
            (Some(r), _) => parts.push(format!("fan {r} rpm")),
            (None, _) => {}
        }
        (!parts.is_empty()).then(|| parts.join(", "))
    }
}

/// How much a hwmon driver deserves to be believed as *the* CPU temperature.
/// coretemp and k10temp read the on-die sensor; acpitz is a firmware guess at
/// something near it and is the last resort rather than the answer.
fn cpu_temp_rank(name: &str) -> Option<u8> {
    match name {
        "coretemp" | "k10temp" | "zenpower" => Some(3),
        // Apple silicon and some ARM boards; a real die sensor under a
        // different name.
        "cpu_thermal" | "soc_thermal" => Some(2),
        "acpitz" => Some(1),
        _ => None,
    }
}

/// The sensor within a driver that means "the whole package", not one core.
fn pkg_label_rank(label: &str) -> u8 {
    let l = label.to_ascii_lowercase();
    if l.starts_with("package") || l == "tdie" || l == "tctl" {
        2
    } else if l.starts_with("core") {
        0 // a single core runs hotter or cooler than the part; prefer neither
    } else {
        1
    }
}

/// One pass over /sys/class/hwmon.  Around thirty small reads a minute, which
/// is nothing next to the /proc sweep that follows it.
pub fn read_thermal() -> Thermal {
    let mut t = Thermal::default();
    let Ok(dir) = fs::read_dir("/sys/class/hwmon") else { return t };
    let mut entries: Vec<_> = dir.flatten().map(|e| e.path()).collect();
    entries.sort();
    // Best (driver, sensor) pair seen so far.
    let mut best: Option<(u8, u8, i64)> = None;
    for base in entries {
        let name = fs::read_to_string(base.join("name"))
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let num = |f: &Path| fs::read_to_string(f).ok().and_then(|s| s.trim().parse::<i64>().ok());
        // Sensor numbering is not dense and not bounded; ten is well past
        // what any of these drivers expose.
        for i in 1..=10 {
            if let Some(rank) = cpu_temp_rank(&name) {
                if let Some(mc) = num(&base.join(format!("temp{i}_input"))) {
                    // A sensor that has been unplugged reads 0; a CPU at 0°C
                    // is a broken sensor, not a cold one.
                    let label = fs::read_to_string(base.join(format!("temp{i}_label")))
                        .unwrap_or_default();
                    let lr = pkg_label_rank(label.trim());
                    if mc > 0 && best.map_or(true, |(r, l, _)| (rank, lr) > (r, l)) {
                        best = Some((rank, lr, mc));
                    }
                }
            }
            if let Some(rpm) = num(&base.join(format!("fan{i}_input"))) {
                if rpm >= 0 {
                    let rpm = rpm as u64;
                    if t.fan_rpm.map_or(true, |cur| rpm > cur) {
                        t.fan_rpm = Some(rpm);
                        t.fan_max_rpm = num(&base.join(format!("fan{i}_max")))
                            .filter(|m| *m > 0)
                            .map(|m| m as u64);
                    }
                }
            }
        }
    }
    t.cpu_temp_mc = best.map(|(_, _, mc)| mc);
    t
}

// --------------------------------------------------------------------- gpu

/// What the graphics side of the machine is doing: how much power it draws,
/// how often its engines are awake, how fast they clock, and how bright the
/// panel is lit.
///
/// Watts and busy shares are rates, so they come from a [`GpuMeter`] that
/// remembers the previous reading; a single read has nothing to divide by.
/// Everything is optional: the RAPL energy counters are root-only unless a
/// udev rule opens them up, AMD has no "uncore" domain at all, and a desktop
/// has no backlight.
#[derive(Clone, Copy, Default, Debug)]
pub struct Gpu {
    /// Mean graphics power since the previous reading, in milliwatts, off the
    /// RAPL "uncore" domain — which on Intel client parts is the GPU.
    pub gpu_mw: Option<i64>,
    /// Mean package power over the same stretch, so the GPU's share of the
    /// whole chip can be read off the same chart.
    pub pkg_mw: Option<i64>,
    /// Share of the stretch the render engine was awake (out of RC6), 0–100.
    pub render_busy_pct: Option<f64>,
    /// The same for the media engine, which does video decode and encode.
    pub media_busy_pct: Option<f64>,
    /// The render engine's actual clock at the moment of reading, MHz; 0
    /// while it sleeps.
    pub freq_mhz: Option<u64>,
    /// Panel backlight as a share of its maximum; 0 with the panel dark.
    pub backlight_pct: Option<f64>,
}

impl Gpu {
    pub fn summary(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(mw) = self.gpu_mw {
            let pkg = self.pkg_mw.map(|p| format!(" of {:.1}W package", p as f64 / 1000.0));
            parts.push(format!("{:.2}W{}", mw as f64 / 1000.0, pkg.unwrap_or_default()));
        }
        if let Some(b) = self.render_busy_pct {
            parts.push(format!("render busy {b:.0}%"));
        }
        if let Some(b) = self.media_busy_pct {
            parts.push(format!("media busy {b:.0}%"));
        }
        if let Some(f) = self.freq_mhz {
            parts.push(if f == 0 { "clock idle".into() } else { format!("{f} MHz") });
        }
        if let Some(b) = self.backlight_pct {
            parts.push(format!("backlight {b:.0}%"));
        }
        (!parts.is_empty()).then(|| parts.join(", "))
    }
}

/// Where the counters live on this machine, found once: the paths do not
/// move while the machine is up, and walking sysfs every minute to find them
/// again would cost more than the reads.
#[derive(Default, Debug)]
struct GpuPaths {
    /// (energy_uj, max_energy_range_uj) for the uncore and package domains.
    gpu_energy: Option<(std::path::PathBuf, u64)>,
    pkg_energy: Option<(std::path::PathBuf, u64)>,
    /// Cumulative milliseconds asleep, per engine.
    render_idle: Option<std::path::PathBuf>,
    media_idle: Option<std::path::PathBuf>,
    freq: Option<std::path::PathBuf>,
    backlight: Option<(std::path::PathBuf, u64)>,
}

fn read_u64(p: &Path) -> Option<u64> {
    fs::read_to_string(p).ok()?.trim().parse().ok()
}

fn trimmed(p: &Path) -> String {
    fs::read_to_string(p).map(|s| s.trim().to_string()).unwrap_or_default()
}

fn sorted_dir(dir: &str) -> Vec<std::path::PathBuf> {
    let mut v: Vec<_> = fs::read_dir(dir)
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

impl GpuPaths {
    fn discover() -> GpuPaths {
        let mut g = GpuPaths::default();
        // RAPL: the MSR-backed tree, not intel-rapl-mmio, which carries only
        // the package again.  A counter this user cannot read is left out
        // here, so the missing figure reads as "not permitted", not as 0 W.
        for zone in sorted_dir("/sys/class/powercap") {
            let fname = zone.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !fname.starts_with("intel-rapl:") {
                continue;
            }
            let energy = zone.join("energy_uj");
            if fs::read_to_string(&energy).is_err() {
                continue;
            }
            let range = read_u64(&zone.join("max_energy_range_uj")).unwrap_or(0);
            match trimmed(&zone.join("name")).as_str() {
                "uncore" if g.gpu_energy.is_none() => g.gpu_energy = Some((energy, range)),
                "package-0" if g.pkg_energy.is_none() => g.pkg_energy = Some((energy, range)),
                _ => {}
            }
        }
        // xe: tile*/gt*/gtidle, named "gt0-rc" for render and "gt1-mc" for
        // media.  i915 keeps one rc6 counter per gt under card*/gt instead.
        for card in sorted_dir("/sys/class/drm") {
            let cname = card.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !cname.starts_with("card") || cname.contains('-') {
                continue;
            }
            let dev = card.join("device");
            for tile in sorted_dir(&dev.to_string_lossy()) {
                if !tile.file_name().is_some_and(|n| n.to_string_lossy().starts_with("tile")) {
                    continue;
                }
                for gt in sorted_dir(&tile.to_string_lossy()) {
                    let name = trimmed(&gt.join("gtidle/name"));
                    let idle = gt.join("gtidle/idle_residency_ms");
                    if name.ends_with("-rc") && g.render_idle.is_none() {
                        g.render_idle = Some(idle);
                        g.freq = Some(gt.join("freq0/act_freq"));
                    } else if name.ends_with("-mc") && g.media_idle.is_none() {
                        g.media_idle = Some(idle);
                    }
                }
            }
            let i915 = card.join("gt/gt0/rc6_residency_ms");
            if g.render_idle.is_none() && i915.exists() {
                g.render_idle = Some(i915);
                g.freq = Some(card.join("gt/gt0/rps_act_freq_mhz"));
                let media = card.join("gt/gt1/rc6_residency_ms");
                g.media_idle = media.exists().then_some(media);
            }
        }
        // The first backlight is the panel's; an external monitor's DDC
        // backlight, where one shows up, comes after it.
        if let Some(bl) = sorted_dir("/sys/class/backlight").into_iter().next() {
            if let Some(max) = read_u64(&bl.join("max_brightness")).filter(|m| *m > 0) {
                g.backlight = Some((bl.join("actual_brightness"), max));
            }
        }
        g
    }
}

/// The cumulative counters behind one [`Gpu`] reading.
#[derive(Clone, Copy, Debug)]
struct GpuCounters {
    at: Instant,
    gpu_uj: Option<u64>,
    pkg_uj: Option<u64>,
    render_idle_ms: Option<u64>,
    media_idle_ms: Option<u64>,
}

/// Turns the cumulative counters into rates between successive reads.
///
/// Each consumer keeps its own meter — the watcher for the history, the web
/// server for the live figures — so neither shortens the other's window.
#[derive(Default)]
pub struct GpuMeter {
    paths: Option<GpuPaths>,
    prev: Option<GpuCounters>,
}

/// The increase of a counter that wraps at `range`, or None when it went
/// backwards by more than a wrap explains (a resume that reset it).
fn counter_delta(prev: u64, now: u64, range: u64) -> Option<u64> {
    if now >= prev {
        Some(now - prev)
    } else if range > prev {
        Some(range - prev + now)
    } else {
        None
    }
}

/// Share of `wall_ms` an engine was *not* asleep, clamped: the idle counter
/// and the wall clock are read a few microseconds apart, which can put the
/// raw figure a hair outside 0–100.
fn busy_pct(idle_prev: u64, idle_now: u64, wall_ms: f64) -> Option<f64> {
    if idle_now < idle_prev || wall_ms <= 0.0 {
        return None;
    }
    Some((100.0 - (idle_now - idle_prev) as f64 / wall_ms * 100.0).clamp(0.0, 100.0))
}

fn gpu_counters(p: &GpuPaths) -> GpuCounters {
    GpuCounters {
        at: Instant::now(),
        gpu_uj: p.gpu_energy.as_ref().and_then(|(f, _)| read_u64(f)),
        pkg_uj: p.pkg_energy.as_ref().and_then(|(f, _)| read_u64(f)),
        render_idle_ms: p.render_idle.as_deref().and_then(read_u64),
        media_idle_ms: p.media_idle.as_deref().and_then(read_u64),
    }
}

impl GpuMeter {
    /// Rates since the previous call; the first call only primes the meter
    /// and reports the instantaneous figures (clock, backlight).
    pub fn read(&mut self) -> Gpu {
        let p = self.paths.get_or_insert_with(GpuPaths::discover);
        let now = gpu_counters(p);
        let mut g = Gpu {
            freq_mhz: p.freq.as_deref().and_then(read_u64),
            backlight_pct: p
                .backlight
                .as_ref()
                .and_then(|(f, max)| read_u64(f).map(|b| b as f64 / *max as f64 * 100.0)),
            ..Gpu::default()
        };
        if let Some(prev) = self.prev {
            let secs = now.at.duration_since(prev.at).as_secs_f64();
            if secs > 0.05 {
                let mw = |a: Option<u64>, b: Option<u64>, range: u64| {
                    counter_delta(a?, b?, range).map(|uj| (uj as f64 / secs / 1000.0).round() as i64)
                };
                let range = |e: &Option<(std::path::PathBuf, u64)>| e.as_ref().map_or(0, |(_, r)| *r);
                g.gpu_mw = mw(prev.gpu_uj, now.gpu_uj, range(&p.gpu_energy));
                g.pkg_mw = mw(prev.pkg_uj, now.pkg_uj, range(&p.pkg_energy));
                let wall_ms = secs * 1000.0;
                g.render_busy_pct = prev
                    .render_idle_ms
                    .zip(now.render_idle_ms)
                    .and_then(|(a, b)| busy_pct(a, b, wall_ms));
                g.media_busy_pct = prev
                    .media_idle_ms
                    .zip(now.media_idle_ms)
                    .and_then(|(a, b)| busy_pct(a, b, wall_ms));
            }
        }
        self.prev = Some(now);
        g
    }

    /// A reading that covers at least `min` — for callers that read rarely
    /// or for the first time, and would otherwise get no rates at all or
    /// ones averaged over a stale hour.
    pub fn read_fresh(&mut self, min: Duration, max_age: Duration) -> Gpu {
        let stale = self.prev.map_or(true, |p| p.at.elapsed() > max_age);
        if stale {
            self.read();
            std::thread::sleep(min);
        } else if let Some(left) = self.prev.map(|p| min.saturating_sub(p.at.elapsed())) {
            std::thread::sleep(left);
        }
        self.read()
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Which sensor is read decides whether the chart shows the part or one
    /// hot core, and the two differ by several degrees under load.
    #[test]
    fn picks_the_package_sensor_over_a_single_core() {
        assert!(pkg_label_rank("Package id 0") > pkg_label_rank("Core 1"));
        assert!(pkg_label_rank("Tdie") > pkg_label_rank("Core 0"));
        assert!(pkg_label_rank("") > pkg_label_rank("Core 3"));
    }

    /// acpitz is a firmware guess at something near the CPU; where a real die
    /// sensor exists it has to win.
    #[test]
    fn prefers_a_die_sensor_over_the_firmware_guess() {
        assert!(cpu_temp_rank("coretemp") > cpu_temp_rank("acpitz"));
        assert!(cpu_temp_rank("k10temp") > cpu_temp_rank("acpitz"));
        assert_eq!(cpu_temp_rank("nvme"), None);
        assert_eq!(cpu_temp_rank("BAT0"), None);
    }

    /// The three PDO shapes each spell their limit differently and each in a
    /// different unit, and a wrong multiplier here turns a 65W brick into a
    /// 65mW one without ever looking implausible enough to notice.
    #[test]
    fn source_capabilities_come_out_in_microwatts_whatever_shape_they_are_in() {
        let dir = std::env::temp_dir().join(format!("bw-pdo-{}", std::process::id()));
        let caps = dir.join("source-capabilities");
        let write = |sub: &str, files: &[(&str, &str)]| {
            let d = caps.join(sub);
            fs::create_dir_all(&d).unwrap();
            for (name, val) in files {
                fs::write(d.join(name), val).unwrap();
            }
        };
        // The 5V/3A every charger offers first, then a 20V/3.25A fixed PDO —
        // the 65W the brick is sold as — and a battery PDO in milliwatts.
        write("1:fixed_supply", &[("voltage", "5000"), ("maximum_current", "3000")]);
        write("2:fixed_supply", &[("voltage", "20000"), ("maximum_current", "3250")]);
        write("3:battery", &[("maximum_voltage", "20000"), ("minimum_voltage", "5000"),
                             ("maximum_power", "27000")]);
        assert_eq!(read_source_caps(&dir), Some(65_000_000));

        // A range PDO has no single `voltage`, so its ceiling is the pair to
        // multiply; 20V at 5A is the 100W a bigger charger advertises.
        write("4:programmable_supply", &[("maximum_voltage", "20000"),
                                         ("minimum_voltage", "3300"),
                                         ("maximum_current", "5000")]);
        assert_eq!(read_source_caps(&dir), Some(100_000_000));

        // The common case: a device that exposes the directory and nothing in
        // it must read as unknown, not as a zero-watt charger.
        let empty = dir.join("empty");
        fs::create_dir_all(empty.join("source-capabilities")).unwrap();
        assert_eq!(read_source_caps(&empty), None);
        // And one with no such directory at all — most machines.
        assert_eq!(read_source_caps(&dir.join("absent")), None);
        let _ = fs::remove_dir_all(&dir);
    }

    /// Every one of these is a division somebody reads as a promise, and each
    /// guard is there because the alternative is a confident wrong number.
    #[test]
    fn the_estimates_refuse_to_divide_by_what_they_do_not_have() {
        let full = Power {
            bat_status: Some(BatStatus::Charging),
            bat_power_uw: Some(18_000_000),
            bat_energy_uwh: Some(19_290_000),
            bat_energy_full_uwh: Some(52_000_000),
            bat_energy_design_uwh: Some(52_000_000),
            ..Power::default()
        };
        // 32.71Wh of headroom at 18W is a little under two hours.
        let (charging, secs) = full.eta_secs().unwrap();
        assert!(charging);
        assert!((6400..6700).contains(&secs), "{secs}s");
        assert_eq!(full.health_pct().map(|h| h.round()), Some(100.0));

        // Draining counts the whole pack down, not the headroom above it.
        let draining = Power {
            bat_status: Some(BatStatus::Discharging),
            bat_power_uw: Some(-12_000_000),
            ..full
        };
        let (charging, secs) = draining.eta_secs().unwrap();
        assert!(!charging);
        assert!((5700..5900).contains(&secs), "{secs}s");

        // A battery sitting at a charge threshold reports zero watts; there is
        // no rate to divide by and so no estimate to make.
        assert_eq!(Power { bat_power_uw: Some(0), ..full }.eta_secs(), None);
        // A full battery has no headroom left, which is not an infinite wait.
        assert_eq!(Power { bat_energy_uwh: Some(52_000_000), ..full }.eta_secs(), None);
        // A battery that reports no energy counters at all says nothing.
        assert_eq!(Power { bat_energy_uwh: None, ..full }.eta_secs(), None);
        assert_eq!(Power { bat_energy_design_uwh: None, ..full }.health_pct(), None);

        // A worn pack is the whole reason the figure is worth showing.
        let worn = Power { bat_energy_full_uwh: Some(44_200_000), ..full };
        assert_eq!(worn.health_pct().map(|h| h.round()), Some(85.0));
    }

    /// Whatever the machine this runs on happens to expose, it must come back
    /// either absent or plausible — never a 0 K CPU or a 200 000 rpm fan,
    /// which is what a misread sysfs file looks like.
    #[test]
    fn reads_this_machine_or_says_nothing() {
        let t = read_thermal();
        if let Some(mc) = t.cpu_temp_mc {
            assert!((5_000..=125_000).contains(&mc), "implausible temperature {mc}");
        }
        if let Some(r) = t.fan_rpm {
            assert!(r <= 30_000, "implausible fan speed {r}");
        }
        // A machine with neither says nothing at all rather than "0°C, fan off".
        if t.cpu_temp_mc.is_none() && t.fan_rpm.is_none() {
            assert_eq!(t.summary(), None);
        }
    }

    /// RAPL energy counters wrap at max_energy_range_uj; a wrap is energy
    /// used, a counter reset (resume) is not a negative watt figure.
    #[test]
    fn energy_counters_survive_a_wrap() {
        assert_eq!(counter_delta(100, 250, 1000), Some(150));
        assert_eq!(counter_delta(900, 50, 1000), Some(150));
        assert_eq!(counter_delta(900, 50, 0), None);
    }

    #[test]
    fn busy_share_is_the_complement_of_idle() {
        assert_eq!(busy_pct(1000, 1750, 1000.0), Some(25.0));
        // Counter and clock read microseconds apart can overshoot a hair.
        assert_eq!(busy_pct(1000, 2003, 1000.0), Some(0.0));
        assert_eq!(busy_pct(2000, 1000, 1000.0), None);
    }

    /// Whatever the GPU exposes must come back absent or plausible.
    #[test]
    fn reads_the_gpu_or_says_nothing() {
        let mut m = GpuMeter::default();
        let g = m.read_fresh(Duration::from_millis(100), Duration::ZERO);
        for b in [g.render_busy_pct, g.media_busy_pct, g.backlight_pct].into_iter().flatten() {
            assert!((0.0..=100.0).contains(&b), "implausible share {b}");
        }
        if let Some(mw) = g.gpu_mw {
            assert!((0..200_000).contains(&mw), "implausible gpu power {mw} mW");
        }
    }
}
