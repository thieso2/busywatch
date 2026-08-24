//! Small shared helpers: time, formatting, logging.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn log(msg: &str) {
    // journald adds its own timestamp; this one helps plain-file logs.
    println!("[{}] {msg}", unix_now());
}

pub fn fmt_dur(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 86400 {
        format!("{}d{:02}h", s / 86400, (s % 86400) / 3600)
    } else if s >= 3600 {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

pub fn fmt_bytes(b: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1000.0 && i < UNITS.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 { format!("{b}B") } else { format!("{v:.1}{}", UNITS[i]) }
}

pub fn fmt_ts(secs: i64) -> String {
    strftime(secs, b"%Y-%m-%d %H:%M\0")
}

pub fn strftime(secs: i64, fmt: &[u8]) -> String {
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        let t = secs as libc::time_t;
        libc::localtime_r(&t, &mut tm);
        let mut buf = [0u8; 64];
        let n = libc::strftime(
            buf.as_mut_ptr() as *mut _,
            buf.len(),
            fmt.as_ptr() as *const _,
            &tm,
        );
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }
}

/// Parse a duration like `90`, `45m`, `6h`, `7d`, `2w` into seconds.
pub fn parse_span(s: &str) -> Option<u64> {
    let s = s.trim();
    let (num, mult) = match s.chars().last()? {
        's' => (&s[..s.len() - 1], 1),
        'm' => (&s[..s.len() - 1], 60),
        'h' => (&s[..s.len() - 1], 3600),
        'd' => (&s[..s.len() - 1], 86400),
        'w' => (&s[..s.len() - 1], 604800),
        _ => (s, 1),
    };
    num.trim().parse::<u64>().ok().map(|n| n * mult)
}

/// Escape a string into a JSON string literal (quotes included).
pub fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// JSON number that never emits NaN/Infinity (invalid JSON) — those become null.
pub fn json_num(v: f64) -> String {
    if v.is_finite() { format!("{:.3}", v).trim_end_matches('0').trim_end_matches('.').to_string() } else { "null".into() }
}

pub fn json_opt_num(v: Option<f64>) -> String {
    match v {
        Some(v) => json_num(v),
        None => "null".into(),
    }
}
