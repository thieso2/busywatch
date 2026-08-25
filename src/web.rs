//! A dependency-free HTTP server for the history UI.
//!
//! One thread per connection, everything read-only, bound to loopback unless
//! told otherwise.  The page itself is a single embedded HTML file that talks
//! to `/api/*`; there are no external assets, so it works offline.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use rusqlite::Connection;

use crate::db;
use crate::sample::{self, read_meminfo, read_pressures};
use crate::util::{json_num, json_opt_num, json_str, log, unix_now};

const UI: &str = include_str!("ui.html");
const MAX_CONNS: usize = 16;

pub struct Server {
    db_path: PathBuf,
}

/// Serve until killed.  Returns an exit code only on a fatal bind error.
pub fn serve(addr: &str, db_path: &Path) -> i32 {
    let listener = match TcpListener::bind(addr) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("busywatch web: cannot bind {addr}: {e}");
            return 1;
        }
    };
    let shown = listener.local_addr().map(|a| a.to_string()).unwrap_or_else(|_| addr.to_string());
    log(&format!("web ui on http://{shown}/  (history: {})", db_path.display()));
    let srv = Arc::new(Server { db_path: db_path.to_path_buf() });
    let live = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        if live.load(Ordering::Relaxed) >= MAX_CONNS {
            let _ = respond(&stream, 503, "text/plain", b"busy");
            continue;
        }
        live.fetch_add(1, Ordering::Relaxed);
        let (srv, live) = (srv.clone(), live.clone());
        std::thread::spawn(move || {
            srv.handle(stream);
            live.fetch_sub(1, Ordering::Relaxed);
        });
    }
    0
}

/// Start the UI in a background thread (used by the watch daemon).
pub fn spawn(addr: String, db_path: PathBuf) {
    std::thread::spawn(move || {
        serve(&addr, &db_path);
    });
}

fn respond(mut stream: &TcpStream, code: u16, ctype: &str, body: &[u8]) -> std::io::Result<()> {
    let reason = match code {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

impl Server {
    fn handle(&self, stream: TcpStream) {
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(10)));
        let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(30)));
        let mut reader = BufReader::new(match stream.try_clone() {
            Ok(s) => s,
            Err(_) => return,
        });
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line.is_empty() {
            return;
        }
        // Drain headers; we never read a body.
        let mut hdr = String::new();
        loop {
            hdr.clear();
            match reader.read_line(&mut hdr) {
                Ok(0) => break,
                Ok(_) if hdr.trim().is_empty() => break,
                Ok(_) if hdr.len() > 8192 => break,
                Ok(_) => {}
                Err(_) => break,
            }
        }
        let mut parts = line.split_whitespace();
        let method = parts.next().unwrap_or("");
        let target = parts.next().unwrap_or("/");
        if method != "GET" && method != "HEAD" {
            let _ = respond(&stream, 400, "text/plain", b"only GET");
            return;
        }
        let (path, query) = match target.split_once('?') {
            Some((p, q)) => (p, parse_query(q)),
            None => (target, HashMap::new()),
        };
        let result = self.route(path, &query);
        match result {
            Some((ctype, body)) => {
                let _ = respond(&stream, 200, ctype, body.as_bytes());
            }
            None => {
                let _ = respond(&stream, 404, "text/plain", b"not found");
            }
        }
    }

    fn route(&self, path: &str, q: &HashMap<String, String>) -> Option<(&'static str, String)> {
        match path {
            "/" | "/index.html" => Some(("text/html; charset=utf-8", UI.to_string())),
            "/api/overview" => Some(("application/json", self.overview(q))),
            "/api/hogs" => Some(("application/json", self.hogs_json(q))),
            "/api/proc" => Some(("application/json", self.proc_json(q))),
            _ => None,
        }
    }

    fn open(&self) -> Option<Connection> {
        match db::open_read(&self.db_path) {
            Ok(c) => Some(c),
            Err(e) => {
                log(&format!("web: cannot open history {}: {e}", self.db_path.display()));
                None
            }
        }
    }

    /// Everything one screen refresh needs, in a single request.
    fn overview(&self, q: &HashMap<String, String>) -> String {
        let now = unix_now() as i64;
        let span = num(q, "span", 3600).max(60);
        let to = num(q, "to", now).min(now);
        let from = to - span;
        let points = num(q, "points", 300).clamp(20, 2000);
        let metric = q.get("metric").map(String::as_str).unwrap_or("mem");
        let limit = num(q, "limit", 8).clamp(1, 30) as usize;

        let mut out = String::with_capacity(64 * 1024);
        out.push('{');
        let Some(conn) = self.open() else {
            out.push_str(&format!(
                "\"from\":{from},\"to\":{to},\"bucket\":60,\"now\":{now},\"metric\":{},\"live\":{},\
                 \"error\":\"no history database\",\"series\":[],\"incidents\":[],\"hogs\":[],\"stack\":[]}}",
                json_str(metric),
                live_json()
            ));
            return out;
        };
        // Bucketing finer than the recording interval would leave most
        // buckets empty and draw a comb of one-point spikes.
        let bucket = (span / points).max(natural_bucket(&conn, from, to));
        out.push_str(&format!(
            "\"from\":{from},\"to\":{to},\"bucket\":{bucket},\"now\":{now},\"metric\":{},",
            json_str(metric)
        ));
        out.push_str(&format!("\"live\":{},", live_json()));
        let (first, last) = db::span(&conn);
        out.push_str(&format!("\"span\":{{\"first\":{first},\"last\":{last}}},"));

        out.push_str("\"series\":[");
        if let Ok(rows) = db::series(&conn, from, to, bucket) {
            for (i, b) in rows.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&format!(
                    "{{\"t\":{},\"cpu\":{},\"cpuMax\":{},\"mem\":{},\"memMax\":{},\
                      \"io\":{},\"ioMax\":{},\"load\":{},\"loadMax\":{},\
                      \"memUsed\":{},\"memUsedMax\":{},\"memTotal\":{},\"swap\":{},\"n\":{},\
                      \"ac\":{},\"bat\":{},\"batW\":{},\"freq\":{},\"freqMax\":{},\
                      \"thr\":{},\"thrMs\":{}}}",
                    b.t,
                    json_num(b.cpu_avg),
                    json_num(b.cpu_max),
                    json_num(b.mem_avg),
                    json_num(b.mem_max),
                    json_num(b.io_avg),
                    json_num(b.io_max),
                    json_num(b.load),
                    json_num(b.load_max),
                    json_num(b.mem_used_pct),
                    json_num(b.mem_used_max_pct),
                    b.mem_total_kb,
                    b.swap_used_kb,
                    b.n,
                    json_opt_num(b.ac_online),
                    json_opt_num(b.bat_pct),
                    json_opt_num(b.bat_power_uw),
                    json_opt_num(b.freq_khz),
                    json_opt_num(b.freq_max_khz),
                    opt_i64(b.throttled),
                    opt_i64(b.throttled_ms)
                ));
            }
        }
        out.push_str("],");

        out.push_str("\"incidents\":[");
        if let Ok(rows) = db::incidents(&conn, from, to, 200) {
            for (i, r) in rows.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&incident_json(r));
            }
        }
        out.push_str("],");

        // One pass over the range gives every app seen, ordered by the chosen
        // metric: the head of it feeds the stacked chart, all of it feeds the
        // rundown table.
        let hogs = db::hogs(&conn, from, to, metric, 400).unwrap_or_default();
        out.push_str("\"hogs\":[");
        for (i, h) in hogs.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&hog_json(h));
        }
        out.push_str("],");

        // Per-process series for the charted processes, so the stacked chart
        // and the head of the table always agree.
        let comms: Vec<String> = hogs.iter().take(limit).map(|h| h.comm.clone()).collect();
        let stack = db::hog_series(&conn, from, to, bucket, metric, &comms).unwrap_or_default();
        out.push_str("\"stack\":[");
        for (i, comm) in comms.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!("{{\"comm\":{},\"points\":[", json_str(comm)));
            if let Some(pts) = stack.get(comm) {
                for (j, (t, v)) in pts.iter().enumerate() {
                    if j > 0 {
                        out.push(',');
                    }
                    out.push_str(&format!("[{t},{}]", json_num(*v)));
                }
            }
            out.push_str("]}");
        }
        out.push_str("]}");
        out
    }

    fn hogs_json(&self, q: &HashMap<String, String>) -> String {
        let now = unix_now() as i64;
        let to = num(q, "to", now).min(now);
        let from = to - num(q, "span", 3600).max(60);
        let metric = q.get("metric").map(String::as_str).unwrap_or("mem");
        let limit = num(q, "limit", 20).clamp(1, 200) as usize;
        let Some(conn) = self.open() else { return "{\"hogs\":[]}".into() };
        let hogs = db::hogs(&conn, from, to, metric, limit).unwrap_or_default();
        let body =
            hogs.iter().map(hog_json).collect::<Vec<_>>().join(",");
        format!("{{\"from\":{from},\"to\":{to},\"hogs\":[{body}]}}")
    }

    /// Full detail for one command name: all three metrics over the range,
    /// the totals it added up to, its pids, and the incidents it caused.
    fn proc_json(&self, q: &HashMap<String, String>) -> String {
        let now = unix_now() as i64;
        let to = num(q, "to", now).min(now);
        let span = num(q, "span", 3600).max(60);
        let from = to - span;
        let points = num(q, "points", 300).clamp(20, 2000);
        let comm = q.get("comm").cloned().unwrap_or_default();
        let Some(conn) = self.open() else { return "{}".into() };
        let bucket = (span / points).max(natural_bucket(&conn, from, to));
        let comms = vec![comm.clone()];
        let mut out = format!(
            "{{\"comm\":{},\"from\":{from},\"to\":{to},\"bucket\":{bucket},\"cores\":{}",
            json_str(&comm),
            sample::cores()
        );
        let a = db::app_summary(&conn, &comm, from, to).unwrap_or_default();
        out.push_str(&format!(
            ",\"summary\":{{\"samples\":{},\"first\":{},\"last\":{},\"covered\":{},\
              \"rssMax\":{},\"rssAvg\":{},\"rssLast\":{},\"cpuMax\":{},\"cpuAvg\":{},\
              \"cpuSecs\":{},\"ioMax\":{},\"ioRd\":{},\"ioWr\":{},\"pidsMax\":{},\"pidsSeen\":{}}}",
            a.samples,
            a.first_ts,
            a.last_ts,
            a.covered_secs,
            a.rss_max_kb,
            a.rss_avg_kb,
            a.rss_last_kb,
            json_num(a.cpu_max),
            json_num(a.cpu_avg),
            json_num(a.cpu_secs),
            a.io_max_ps,
            a.io_rd_bytes,
            a.io_wr_bytes,
            a.pids_max,
            a.pids_seen
        ));
        // How much of the machine that was: total RAM for the memory figures.
        let mem_total: i64 = conn
            .query_row(
                "SELECT mem_total_kb FROM sample WHERE ts <= ?1 AND mem_total_kb IS NOT NULL
                  ORDER BY ts DESC LIMIT 1",
                rusqlite::params![to],
                |r| r.get(0),
            )
            .unwrap_or(0);
        out.push_str(&format!(",\"memTotal\":{mem_total}"));

        for m in ["mem", "cpu", "io"] {
            let s = db::hog_series(&conn, from, to, bucket, m, &comms).unwrap_or_default();
            let pts = s.get(&comm).cloned().unwrap_or_default();
            out.push_str(&format!(",\"{m}\":["));
            for (j, (t, v)) in pts.iter().enumerate() {
                if j > 0 {
                    out.push(',');
                }
                out.push_str(&format!("[{t},{}]", json_num(*v)));
            }
            out.push(']');
        }

        out.push_str(",\"pids\":[");
        if let Ok(pids) = db::app_pids(&conn, &comm, from, to, 30) {
            for (i, p) in pids.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&format!(
                    "{{\"pid\":{},\"rssMax\":{},\"cpuMax\":{},\"ioMax\":{},\"first\":{},\"last\":{}}}",
                    p.pid,
                    p.rss_max_kb,
                    json_num(p.cpu_max),
                    p.io_max_ps,
                    p.first_ts,
                    p.last_ts
                ));
            }
        }
        out.push_str("],\"incidents\":[");
        if let Ok(rows) = db::app_incidents(&conn, &comm, from, to) {
            for (i, r) in rows.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&incident_json(r));
            }
        }
        out.push_str("]}");
        out
    }
}

/// The average spacing of the samples actually stored in this range — the
/// finest bucket that still yields a continuous line.
fn natural_bucket(conn: &Connection, from: i64, to: i64) -> i64 {
    conn.query_row(
        "SELECT COUNT(*), MIN(ts), MAX(ts) FROM sample WHERE ts BETWEEN ?1 AND ?2",
        rusqlite::params![from, to],
        |r| {
            let n: i64 = r.get(0)?;
            let lo: Option<i64> = r.get(1)?;
            let hi: Option<i64> = r.get(2)?;
            Ok(match (n, lo, hi) {
                (n, Some(lo), Some(hi)) if n > 1 => ((hi - lo) / (n - 1)).max(1),
                _ => 60,
            })
        },
    )
    .unwrap_or(60)
}

fn hog_json(h: &db::HogRow) -> String {
    format!(
        "{{\"comm\":{},\"rssMax\":{},\"rssAvg\":{},\"rssLast\":{},\"cpuMax\":{},\"cpuAvg\":{},\
          \"cpuSecs\":{},\"ioMax\":{},\"ioRd\":{},\"ioWr\":{},\"pids\":{},\"samples\":{},\
          \"first\":{},\"last\":{}}}",
        json_str(&h.comm),
        h.rss_max_kb,
        h.rss_avg_kb,
        h.rss_last_kb,
        json_num(h.cpu_max),
        json_num(h.cpu_avg),
        json_num(h.cpu_secs),
        h.io_max_ps,
        h.io_rd_bytes,
        h.io_wr_bytes,
        h.pids,
        h.samples,
        h.first_ts,
        h.last_ts
    )
}

fn incident_json(r: &db::IncidentRow) -> String {
    format!(
        "{{\"id\":{},\"kind\":{},\"started\":{},\"ended\":{},\"peak\":{},\
          \"minMemAvail\":{},\"top\":{}}}",
        r.id,
        json_str(&r.kind),
        r.started,
        r.ended.map(|e| e.to_string()).unwrap_or_else(|| "null".into()),
        json_opt_num(r.peak_avg60),
        r.min_mem_avail_kb.map(|v| v.to_string()).unwrap_or_else(|| "null".into()),
        r.top_comm.as_deref().map(json_str).unwrap_or_else(|| "null".into()),
    )
}

/// Live kernel figures — cheap enough to read on every refresh (no /proc
/// sweep, so no visible cost).
fn live_json() -> String {
    let p = read_pressures();
    let m = read_meminfo();
    let pw = sample::read_power();
    let ck = sample::read_cpu_clock();
    format!(
        "{{\"cpu\":{{\"avg10\":{},\"avg60\":{},\"avg300\":{}}},\
          \"mem\":{{\"avg10\":{},\"avg60\":{},\"avg300\":{}}},\
          \"io\":{{\"avg10\":{},\"avg60\":{},\"avg300\":{}}},\
          \"load\":{},\"cores\":{},\"memTotal\":{},\"memAvail\":{},\
          \"swapTotal\":{},\"swapUsed\":{},\
          \"acOnline\":{},\"batPct\":{},\"batStatus\":{},\"batPowerUw\":{},\
          \"cpuFreqKhz\":{},\"cpuFreqMaxKhz\":{},\"throttleCount\":{},\"throttleMs\":{}}}",
        json_num(p.cpu.avg10),
        json_num(p.cpu.avg60),
        json_num(p.cpu.avg300),
        json_num(p.mem.avg10),
        json_num(p.mem.avg60),
        json_num(p.mem.avg300),
        json_num(p.io.avg10),
        json_num(p.io.avg60),
        json_num(p.io.avg300),
        json_num(sample::load1()),
        sample::cores(),
        m.total_kb,
        m.avail_kb,
        m.swap_total_kb,
        m.swap_used_kb,
        match pw.ac_online {
            Some(b) => (b as i32).to_string(),
            None => "null".into(),
        },
        json_opt_num(pw.bat_pct),
        match pw.bat_status {
            Some(s) => json_str(s.as_str()),
            None => "null".into(),
        },
        opt_i64(pw.bat_power_uw),
        opt_i64(ck.freq_khz.map(|v| v as i64)),
        opt_i64(ck.freq_max_khz.map(|v| v as i64)),
        opt_i64(ck.throttle_count.map(|v| v as i64)),
        opt_i64(ck.throttle_ms.map(|v| v as i64)),
    )
}

fn opt_i64(v: Option<i64>) -> String {
    v.map(|v| v.to_string()).unwrap_or_else(|| "null".into())
}

fn num(q: &HashMap<String, String>, key: &str, default: i64) -> i64 {
    q.get(key).and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn parse_query(q: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for pair in q.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        out.insert(urldecode(k), urldecode(v));
    }
    out
}

fn urldecode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(v) => {
                        out.push(v);
                        i += 3;
                    }
                    None => {
                        out.push(b[i]);
                        i += 1;
                    }
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Browser URL for the UI served at `addr`, opened on one metric and span.
/// A wildcard bind address is not reachable as a URL — the click comes from
/// this machine, so point it at loopback.
pub fn ui_url(addr: &str, metric: &str, span: u64) -> String {
    let hostport = match addr.rsplit_once(':') {
        Some((host, port)) => {
            let host = match host {
                "0.0.0.0" | "::" | "[::]" | "" => "127.0.0.1",
                h => h,
            };
            format!("{host}:{port}")
        }
        None => addr.to_string(),
    };
    format!("http://{hostport}/#span={span}&metric={metric}")
}

/// `8080`, `:8080` and `127.0.0.1:8080` all mean the same thing.
pub fn normalize_addr(s: &str) -> String {
    if s.contains(':') {
        if let Some(rest) = s.strip_prefix(':') {
            format!("127.0.0.1:{rest}")
        } else {
            s.to_string()
        }
    } else {
        format!("127.0.0.1:{s}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_normalize() {
        assert_eq!(normalize_addr("8787"), "127.0.0.1:8787");
        assert_eq!(normalize_addr(":9000"), "127.0.0.1:9000");
        assert_eq!(normalize_addr("0.0.0.0:80"), "0.0.0.0:80");
    }

    #[test]
    fn click_urls_are_reachable_from_this_machine() {
        assert_eq!(
            ui_url("127.0.0.1:8787", "mem", 3600),
            "http://127.0.0.1:8787/#span=3600&metric=mem"
        );
        // A wildcard bind is not a URL host.
        assert_eq!(
            ui_url("0.0.0.0:8787", "cpu", 900),
            "http://127.0.0.1:8787/#span=900&metric=cpu"
        );
        assert_eq!(
            ui_url("[::]:8787", "io", 900),
            "http://127.0.0.1:8787/#span=900&metric=io"
        );
    }

    #[test]
    fn queries_decode() {
        let q = parse_query("comm=npm%20exec%20t3&span=3600&flag");
        assert_eq!(q.get("comm").unwrap(), "npm exec t3");
        assert_eq!(num(&q, "span", 0), 3600);
        assert_eq!(q.get("flag").unwrap(), "");
        assert_eq!(urldecode("a+b%2Fc"), "a b/c");
    }
}
