//! Display-driver errors, as the kernel logs them.
//!
//! A GPU driver that loses step with the panel — a PSR or Panel Replay exit
//! that times out, a FIFO underrun, a display-state-buffer poll that never
//! completes — shows up on screen as tearing, smears and stale patches, and
//! in the kernel log as `[drm] *ERROR*` lines, often hundreds a minute.
//! Nothing else records it: the pressures stay calm and the GPU reads idle.
//!
//! The kernel log is not readable by an ordinary user (`dmesg_restrict`), but
//! the journal is, for members of `wheel`, `adm` or `systemd-journal`.  So
//! this follows `journalctl -k -f` in a thread, filtered in journalctl itself
//! so busywatch only wakes for the lines that matter, and hands them to the
//! watch loop folded by message: a storm of one error is one row with a
//! count, not a thousand rows.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::util::log;

/// What journalctl is asked to match: driver errors, kernel WARN splats from
/// inside the DRM tree, and GPU hangs.  Warnings about a slow modeset and the
/// like are left out; they are noise beside these.
const PATTERN: &str = r"\[drm\] \*ERROR\*|at drivers/gpu/drm/|GPU HANG";

/// One message, folded over however many times it repeated.
#[derive(Clone, Debug, PartialEq)]
pub struct DrmEvent {
    pub first: i64,
    pub last: i64,
    pub msg: String,
    pub count: u64,
}

#[derive(Clone, Default)]
pub struct DrmWatch {
    pending: Arc<Mutex<Vec<DrmEvent>>>,
}

impl DrmWatch {
    /// Everything seen since the last drain, oldest first.
    pub fn drain(&self) -> Vec<DrmEvent> {
        std::mem::take(&mut *self.pending.lock().unwrap_or_else(|e| e.into_inner()))
    }

    fn push(&self, ts: i64, msg: String) {
        let mut v = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        match v.iter_mut().find(|e| e.msg == msg) {
            Some(e) => {
                e.count += 1;
                e.last = e.last.max(ts);
            }
            None => v.push(DrmEvent { first: ts, last: ts, msg, count: 1 }),
        }
    }
}

/// Follow the kernel journal until busywatch exits.  A journalctl that dies
/// (the journal rotated under it, or it was never installed) is restarted
/// after a pause rather than taking the watcher down with it.
pub fn spawn() -> DrmWatch {
    let w = DrmWatch::default();
    let out = w.clone();
    std::thread::spawn(move || loop {
        follow(&w);
        std::thread::sleep(Duration::from_secs(30));
    });
    out
}

fn follow(w: &DrmWatch) {
    let mut cmd = Command::new("journalctl");
    cmd.args(["-k", "-f", "-n", "0", "-o", "short-unix", "--no-pager", "--grep", PATTERN])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // The child must not outlive the watcher: systemd would reap it, but a
    // busywatch run from a terminal would leave journalctl behind.
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            log(&format!("cannot follow the kernel journal for display errors: {e}"));
            return;
        }
    };
    log("following the kernel journal for display-driver errors");
    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Some((ts, msg)) = parse(&line) {
                w.push(ts, msg);
            }
        }
    }
    let _ = child.kill();
    let status = child.wait();
    log(&format!("kernel journal follower exited ({status:?}); retrying in 30s"));
}

/// `1790335048.150607 host kernel: xe 0000:00:02.0: [drm] *ERROR* Timed out…`
/// → (1790335048, "xe: Timed out…").  The PCI address is dropped — there is
/// one GPU on nearly every machine this runs on — and so is the `[drm]
/// *ERROR*` tag every one of these carries, leaving the words that differ.
fn parse(line: &str) -> Option<(i64, String)> {
    let (ts, rest) = line.split_once(' ')?;
    let ts = ts.split('.').next()?.parse::<i64>().ok()?;
    // "host ident: message" — the ident is "kernel" for the kernel's own
    // lines and "unknown" for ones written through /dev/kmsg; neither matters.
    let msg = rest.split_once(' ')?.1.split_once(": ")?.1.trim();
    Some((ts, normalize(msg)))
}

fn normalize(msg: &str) -> String {
    // A WARN splat names CPU and PID, which differ every time and would stop
    // repeats folding together; the source location is what identifies it.
    if let Some(i) = msg.find(" at drivers/gpu/drm/") {
        if msg.starts_with("WARNING") {
            let loc = &msg[i + 4..];
            let loc = loc.split_whitespace().next().unwrap_or(loc);
            return format!("WARNING at {loc}");
        }
    }
    let (driver, text) = match msg.split_once(": [drm] ") {
        Some((head, text)) => (head.split_whitespace().next().unwrap_or(""), text),
        None => ("", msg),
    };
    let text = text.strip_prefix("*ERROR* ").unwrap_or(text).trim();
    if driver.is_empty() {
        text.to_string()
    } else {
        format!("{driver}: {text}")
    }
}

/// The toast body: the loudest messages first, with how often each fired.
pub fn summarize(events: &[DrmEvent], n: usize) -> String {
    let mut by_msg: HashMap<&str, u64> = HashMap::new();
    for e in events {
        *by_msg.entry(e.msg.as_str()).or_default() += e.count;
    }
    let mut v: Vec<_> = by_msg.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let more = v.len().saturating_sub(n);
    let mut lines: Vec<String> = v.iter().take(n).map(|(m, c)| format!("{c}× {m}")).collect();
    if more > 0 {
        lines.push(format!("…and {more} other message{}", if more > 1 { "s" } else { "" }));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_driver_error() {
        let l = "1790335048.150607 xp26 kernel: xe 0000:00:02.0: [drm] *ERROR* Timed out waiting for PSR Idle for re-enable";
        assert_eq!(
            parse(l),
            Some((1790335048, "xe: Timed out waiting for PSR Idle for re-enable".into()))
        );
        let l = "1790334527.1 xp26 kernel: xe 0000:00:02.0: [drm] *ERROR* [CRTC:151:pipe A] DSB 0 poll error";
        assert_eq!(parse(l).unwrap().1, "xe: [CRTC:151:pipe A] DSB 0 poll error");
        let l = "1790336501.5 xp26 unknown: xe 0000:00:02.0: [drm] *ERROR* test";
        assert_eq!(parse(l).unwrap().1, "xe: test");
    }

    /// Two splats from the same line of the driver must fold into one row.
    #[test]
    fn warn_splats_fold_by_location() {
        let a = normalize("WARNING: CPU: 3 PID: 812 at drivers/gpu/drm/xe/display/intel_psr.c:1234 intel_psr_x+0x1/0x2 [xe]");
        let b = normalize("WARNING: CPU: 5 PID: 17 at drivers/gpu/drm/xe/display/intel_psr.c:1234 intel_psr_x+0x1/0x2 [xe]");
        assert_eq!(a, b);
        assert_eq!(a, "WARNING at drivers/gpu/drm/xe/display/intel_psr.c:1234");
    }

    #[test]
    fn repeats_fold_and_summaries_rank_by_count() {
        let w = DrmWatch::default();
        for t in 0..5 {
            w.push(100 + t, "xe: A".into());
        }
        w.push(101, "xe: B".into());
        let ev = w.drain();
        assert_eq!(ev.len(), 2);
        assert_eq!((ev[0].first, ev[0].last, ev[0].count), (100, 104, 5));
        assert!(w.drain().is_empty());
        assert_eq!(summarize(&ev, 1), "5× xe: A\n…and 1 other message");
    }
}
