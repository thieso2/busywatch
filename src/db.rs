//! The history store: an SQLite file holding one row per sample (system-wide
//! pressure / load / memory) plus the notable per-process consumers of that
//! sample, and one row per busy incident.
//!
//! Samples are taken on a heartbeat while the system is healthy and at the
//! (faster) incident cadence while it is not, so the tables carry a
//! continuous minutes-to-days history that the web UI charts.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::Duration;

use rusqlite::{params, Connection, OpenFlags};

use crate::sample::{io_rate, top_by, Consumer, MemInfo, Pressures};
use crate::util::unix_now;

/// What kind of resource an incident is about.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Cpu,
    Mem,
    Io,
}

pub const KINDS: [Kind; 3] = [Kind::Cpu, Kind::Mem, Kind::Io];

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Cpu => "cpu",
            Kind::Mem => "mem",
            Kind::Io => "io",
        }
    }
    pub fn idx(self) -> usize {
        match self {
            Kind::Cpu => 0,
            Kind::Mem => 1,
            Kind::Io => 2,
        }
    }
}

/// One application's total across the pids sharing its command name.
#[derive(Default)]
struct App {
    rss_kb: u64,
    cpu_pct: f64,
    io_rd_ps: u64,
    io_wr_ps: u64,
    pids: i64,
}

impl App {
    fn rank(&self, key: usize) -> f64 {
        match key {
            0 => self.rss_kb as f64,
            1 => self.cpu_pct,
            _ => (self.io_rd_ps + self.io_wr_ps) as f64,
        }
    }
}

/// A system-wide reading — everything a `sample` row stores except its
/// consumers.
#[derive(Clone, Copy, Debug)]
pub struct Reading {
    pub psi: Pressures,
    pub mem: MemInfo,
    pub load1: f64,
}

fn ensure_columns(conn: &Connection, table: &str, cols: &[(&str, &str)]) -> rusqlite::Result<()> {
    let mut have = Vec::new();
    {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
        for c in rows {
            have.push(c?);
        }
    }
    if have.is_empty() {
        return Ok(()); // table doesn't exist yet; CREATE handled it
    }
    for (name, decl) in cols {
        if !have.iter().any(|h| h == name) {
            conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {name} {decl}"))?;
        }
    }
    Ok(())
}

pub struct Db {
    conn: Connection,
}

impl Db {
    pub fn open(path: &Path) -> rusqlite::Result<Db> {
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        let _ = conn.pragma_update(None, "synchronous", "NORMAL");
        migrate(&conn)?;
        Ok(Db { conn })
    }

    pub fn begin_incident(&self, kind: Kind, ts: u64) -> rusqlite::Result<i64> {
        self.conn.execute(
            "INSERT INTO incident (kind, started) VALUES (?1, ?2)",
            params![kind.as_str(), ts as i64],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn end_incident(
        &self,
        id: i64,
        ts: u64,
        peak: f64,
        min_mem_avail_kb: Option<u64>,
        top_comm: Option<&str>,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE incident
                SET ended = ?1, peak_avg60 = ?2, min_mem_avail_kb = ?3, top_comm = ?4
              WHERE id = ?5",
            params![ts as i64, peak, min_mem_avail_kb.map(|v| v as i64), top_comm, id],
        )?;
        Ok(())
    }

    /// Close any incident left open by a crash or a kill: it can never be
    /// resolved now, and an "ongoing" row would poison every later query.
    pub fn close_stale_incidents(&self) -> rusqlite::Result<usize> {
        self.conn.execute(
            "UPDATE incident
                SET ended = COALESCE(
                        (SELECT MAX(ts) FROM sample WHERE sample.incident_id = incident.id),
                        started)
              WHERE ended IS NULL",
            [],
        )
    }

    /// Store one sample: the system row, a per-application row for each
    /// notable app, and the individual processes behind the notable ones.
    ///
    /// The per-app rows are aggregated over *every* process first and only
    /// then ranked, so a browser's total is its real total.  Ranking pids
    /// first and summing the survivors — which is all the per-pid rows can
    /// support — undercounts exactly the many-process apps that matter.
    pub fn record_sample(
        &mut self,
        incident_id: Option<i64>,
        r: &Reading,
        cons: &[Consumer],
        top_n: usize,
    ) -> rusqlite::Result<i64> {
        let ts = unix_now() as i64;
        let mut chosen: HashMap<u32, &Consumer> = HashMap::new();
        for c in top_by(cons, top_n * 2, |c| c.cpu_pct, 1.0) {
            chosen.insert(c.pid, c);
        }
        for c in top_by(cons, top_n, io_rate, 65536.0) {
            chosen.insert(c.pid, c);
        }
        for c in top_by(cons, top_n, |c| c.rss_kb as f64, 8192.0) {
            chosen.insert(c.pid, c);
        }
        // Aggregate every process by command name, then keep the notable apps.
        let mut apps: HashMap<&str, App> = HashMap::new();
        for c in cons {
            let a = apps.entry(c.comm.as_str()).or_default();
            a.rss_kb += c.rss_kb;
            a.cpu_pct += c.cpu_pct;
            a.io_rd_ps += c.io_rd_ps;
            a.io_wr_ps += c.io_wr_ps;
            a.pids += 1;
        }
        let mut app_rows: Vec<(&str, &App)> = apps.iter().map(|(k, v)| (*k, v)).collect();
        let mut keep: HashMap<&str, &App> = HashMap::new();
        // Floors keep the rundown a list of applications: without them an idle
        // machine fills every sample with kworker threads twitching at 2%.
        for (key, min) in [
            (0usize, 8192.0),   // rss_kb: 8 MB
            (1, 3.0),           // cpu_pct
            (2, 65536.0),       // io bytes/s
        ] {
            app_rows.sort_by(|a, b| {
                b.1.rank(key).partial_cmp(&a.1.rank(key)).unwrap_or(std::cmp::Ordering::Equal)
            });
            for (name, a) in app_rows.iter().take(top_n * 2) {
                if a.rank(key) >= min {
                    keep.insert(name, a);
                }
            }
        }

        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO sample (incident_id, ts, avg10, avg60, load1,
                                 mem_avg10, mem_avg60, io_avg10, io_avg60,
                                 mem_total_kb, mem_avail_kb, swap_total_kb, swap_used_kb)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![
                incident_id,
                ts,
                r.psi.cpu.avg10,
                r.psi.cpu.avg60,
                r.load1,
                r.psi.mem.avg10,
                r.psi.mem.avg60,
                r.psi.io.avg10,
                r.psi.io.avg60,
                r.mem.total_kb as i64,
                r.mem.avail_kb as i64,
                r.mem.swap_total_kb as i64,
                r.mem.swap_used_kb as i64,
            ],
        )?;
        let sid = tx.last_insert_rowid();
        {
            let mut ins = tx.prepare_cached(
                "INSERT INTO app_sample (ts, comm, rss_kb, cpu_pct, io_rd_ps, io_wr_ps, pids)
                 VALUES (?1,?2,?3,?4,?5,?6,?7)",
            )?;
            for (name, a) in &keep {
                ins.execute(params![
                    ts,
                    name,
                    a.rss_kb as i64,
                    a.cpu_pct,
                    a.io_rd_ps as i64,
                    a.io_wr_ps as i64,
                    a.pids
                ])?;
            }
        }
        {
            let mut ins = tx.prepare_cached(
                "INSERT INTO consumer (sample_id, ts, pid, comm, cpu_pct, io_rd_ps, io_wr_ps, rss_kb)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            )?;
            for c in chosen.values() {
                ins.execute(params![
                    sid,
                    ts,
                    c.pid,
                    c.comm,
                    c.cpu_pct,
                    c.io_rd_ps as i64,
                    c.io_wr_ps as i64,
                    c.rss_kb as i64
                ])?;
            }
        }
        tx.commit()?;
        Ok(sid)
    }

    /// Drop history older than `days`.  Per-pid rows go after `pid_days`
    /// instead: they are bulky and only useful as recent forensics, while the
    /// per-app rows carry the long record.  Returns rows removed.
    pub fn prune(&self, days: u64, pid_days: u64) -> rusqlite::Result<usize> {
        if days == 0 {
            return Ok(0);
        }
        let cutoff = unix_now().saturating_sub(days * 86400) as i64;
        let pid_cutoff = unix_now().saturating_sub(pid_days.min(days) * 86400) as i64;
        let mut n = self.conn.execute("DELETE FROM consumer WHERE ts < ?1", [pid_cutoff])?;
        n += self.conn.execute("DELETE FROM app_sample WHERE ts < ?1", [cutoff])?;
        n += self.conn.execute("DELETE FROM sample WHERE ts < ?1", [cutoff])?;
        n += self.conn.execute(
            "DELETE FROM incident WHERE started < ?1 AND ended IS NOT NULL",
            [cutoff],
        )?;
        Ok(n)
    }
}

pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS incident (
             id INTEGER PRIMARY KEY,
             kind TEXT NOT NULL DEFAULT 'cpu',
             started INTEGER NOT NULL,
             ended INTEGER,
             peak_avg60 REAL,
             min_mem_avail_kb INTEGER,
             top_comm TEXT
         );
         CREATE TABLE IF NOT EXISTS sample (
             id INTEGER PRIMARY KEY,
             incident_id INTEGER,
             ts INTEGER NOT NULL,
             avg10 REAL NOT NULL,
             avg60 REAL NOT NULL,
             load1 REAL,
             mem_avg10 REAL, mem_avg60 REAL,
             io_avg10 REAL, io_avg60 REAL,
             mem_total_kb INTEGER, mem_avail_kb INTEGER,
             swap_total_kb INTEGER, swap_used_kb INTEGER
         );
         CREATE TABLE IF NOT EXISTS app_sample (
             ts INTEGER NOT NULL,
             comm TEXT NOT NULL,
             rss_kb INTEGER NOT NULL,
             cpu_pct REAL NOT NULL,
             io_rd_ps INTEGER NOT NULL,
             io_wr_ps INTEGER NOT NULL,
             pids INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS consumer (
             sample_id INTEGER NOT NULL,
             ts INTEGER NOT NULL DEFAULT 0,
             pid INTEGER NOT NULL,
             comm TEXT NOT NULL,
             cpu_pct REAL NOT NULL,
             io_rd_ps INTEGER NOT NULL,
             io_wr_ps INTEGER NOT NULL,
             rss_kb INTEGER NOT NULL
         );",
    )?;
    // Databases written by busywatch 0.1 lack the memory/IO columns.
    ensure_columns(
        conn,
        "incident",
        &[
            ("kind", "TEXT NOT NULL DEFAULT 'cpu'"),
            ("min_mem_avail_kb", "INTEGER"),
            ("top_comm", "TEXT"),
        ],
    )?;
    ensure_columns(
        conn,
        "sample",
        &[
            ("mem_avg10", "REAL"),
            ("mem_avg60", "REAL"),
            ("io_avg10", "REAL"),
            ("io_avg60", "REAL"),
            ("mem_total_kb", "INTEGER"),
            ("mem_avail_kb", "INTEGER"),
            ("swap_total_kb", "INTEGER"),
            ("swap_used_kb", "INTEGER"),
        ],
    )?;
    ensure_columns(conn, "consumer", &[("ts", "INTEGER NOT NULL DEFAULT 0")])?;
    // consumer.ts is denormalised so every history query is a single indexed
    // scan; backfill it for rows migrated from the old schema.
    conn.execute_batch(
        "UPDATE consumer SET ts = COALESCE(
             (SELECT ts FROM sample WHERE sample.id = consumer.sample_id), 0)
          WHERE ts = 0;
         CREATE INDEX IF NOT EXISTS sample_ts ON sample(ts);
         CREATE INDEX IF NOT EXISTS sample_incident ON sample(incident_id);
         CREATE INDEX IF NOT EXISTS consumer_ts ON consumer(ts);
         CREATE INDEX IF NOT EXISTS consumer_sample ON consumer(sample_id);
         CREATE INDEX IF NOT EXISTS consumer_comm ON consumer(comm, ts);
         CREATE INDEX IF NOT EXISTS app_sample_ts ON app_sample(ts);
         CREATE INDEX IF NOT EXISTS app_sample_comm ON app_sample(comm, ts);
         CREATE INDEX IF NOT EXISTS incident_started ON incident(started);",
    )?;
    Ok(())
}

/// Open the history read-only.  Falls back to read-write because a WAL
/// database whose -shm file is missing (no writer running) cannot be opened
/// read-only.
pub fn open_read(path: &Path) -> rusqlite::Result<Connection> {
    let conn = match Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(c) => c,
        Err(_) => Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?,
    };
    conn.busy_timeout(Duration::from_secs(5))?;
    Ok(conn)
}

// ------------------------------------------------------------------ queries

#[derive(Debug)]
pub struct IncidentRow {
    pub id: i64,
    pub kind: String,
    pub started: i64,
    pub ended: Option<i64>,
    pub peak_avg60: Option<f64>,
    pub min_mem_avail_kb: Option<i64>,
    pub top_comm: Option<String>,
}

/// Incidents overlapping [from, to], newest first.  `to == 0` means "now".
pub fn incidents(
    conn: &Connection,
    from: i64,
    to: i64,
    limit: usize,
) -> rusqlite::Result<Vec<IncidentRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, kind, started, ended, peak_avg60, min_mem_avail_kb, top_comm
           FROM incident
          WHERE started <= ?2 AND COALESCE(ended, ?2) >= ?1
          ORDER BY started DESC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![from, to, limit as i64], |r| {
        Ok(IncidentRow {
            id: r.get(0)?,
            kind: r.get::<_, Option<String>>(1)?.unwrap_or_else(|| "cpu".into()),
            started: r.get(2)?,
            ended: r.get(3)?,
            peak_avg60: r.get(4)?,
            min_mem_avail_kb: r.get(5)?,
            top_comm: r.get(6)?,
        })
    })?;
    rows.collect()
}

/// Peak per-process figures across one incident, as pseudo-Consumers.
/// Selected by time window rather than by `sample.incident_id`: a sample can
/// only name one incident, and CPU and memory incidents often overlap.
pub fn incident_consumers(conn: &Connection, r: &IncidentRow) -> rusqlite::Result<Vec<Consumer>> {
    let to = r.ended.unwrap_or_else(|| unix_now() as i64);
    let mut stmt = conn.prepare(
        "SELECT pid, comm, MAX(cpu_pct), MAX(io_rd_ps), MAX(io_wr_ps), MAX(rss_kb)
           FROM consumer
          WHERE ts BETWEEN ?1 AND ?2
          GROUP BY pid, comm",
    )?;
    let rows = stmt.query_map(params![r.started, to], |r| {
        Ok(Consumer {
            pid: r.get::<_, i64>(0)? as u32,
            comm: r.get(1)?,
            cpu_pct: r.get(2)?,
            io_rd_ps: r.get::<_, i64>(3)? as u64,
            io_wr_ps: r.get::<_, i64>(4)? as u64,
            rss_kb: r.get::<_, i64>(5)? as u64,
        })
    })?;
    rows.collect()
}

/// One time bucket of the system-wide series.
#[derive(Debug, Default)]
pub struct Bucket {
    pub t: i64,
    pub cpu_avg: f64,
    pub cpu_max: f64,
    pub mem_avg: f64,
    pub mem_max: f64,
    pub io_avg: f64,
    pub io_max: f64,
    pub load: f64,
    pub load_max: f64,
    pub mem_used_pct: f64,
    pub mem_used_max_pct: f64,
    pub mem_total_kb: i64,
    pub swap_used_kb: i64,
    pub n: i64,
}

pub fn series(conn: &Connection, from: i64, to: i64, bucket: i64) -> rusqlite::Result<Vec<Bucket>> {
    let b = bucket.max(1);
    let mut stmt = conn.prepare(
        "SELECT (ts/?3)*?3 AS b,
                AVG(avg60), MAX(avg60),
                AVG(COALESCE(mem_avg60,0)), MAX(COALESCE(mem_avg60,0)),
                AVG(COALESCE(io_avg60,0)),  MAX(COALESCE(io_avg60,0)),
                AVG(COALESCE(load1,0)), MAX(COALESCE(load1,0)),
                AVG(mem_total_kb), AVG(mem_avail_kb), MIN(mem_avail_kb),
                MAX(COALESCE(swap_used_kb,0)), COUNT(*)
           FROM sample
          WHERE ts BETWEEN ?1 AND ?2
          GROUP BY b ORDER BY b",
    )?;
    let rows = stmt.query_map(params![from, to, b], |r| {
        let total: Option<f64> = r.get(9)?;
        let avail: Option<f64> = r.get(10)?;
        let avail_min: Option<f64> = r.get(11)?;
        let used = |a: Option<f64>| match (total, a) {
            (Some(t), Some(a)) if t > 0.0 => (t - a) / t * 100.0,
            _ => 0.0,
        };
        Ok(Bucket {
            t: r.get(0)?,
            cpu_avg: r.get(1)?,
            cpu_max: r.get(2)?,
            mem_avg: r.get(3)?,
            mem_max: r.get(4)?,
            io_avg: r.get(5)?,
            io_max: r.get(6)?,
            load: r.get(7)?,
            load_max: r.get(8)?,
            mem_used_pct: used(avail),
            mem_used_max_pct: used(avail_min),
            mem_total_kb: total.unwrap_or(0.0) as i64,
            swap_used_kb: r.get::<_, i64>(12)?,
            n: r.get(13)?,
        })
    })?;
    rows.collect()
}

/// A process (grouped by command name, summed over its pids) over a range.
#[derive(Debug)]
pub struct HogRow {
    pub comm: String,
    pub rss_max_kb: i64,
    pub rss_avg_kb: i64,
    pub rss_last_kb: i64,
    pub cpu_max: f64,
    pub cpu_avg: f64,
    pub cpu_secs: f64,
    pub io_max_ps: i64,
    pub io_rd_bytes: i64,
    pub io_wr_bytes: i64,
    pub pids: i64,
    pub samples: i64,
    pub first_ts: i64,
    pub last_ts: i64,
}

/// A sample's `cpu_pct` is an average over the gap since the previous one, so
/// CPU-seconds and IO totals are that rate times that gap.  The gap is capped:
/// while busywatch was not running there is no evidence about what ran, and an
/// uncapped gap would invent hours of CPU out of one stale sample.
const GAP_CAP: i64 = 300;

/// Per-timestamp totals per command, with the weight (seconds) each one
/// stands for.  The building block of every "how much did it use" query.
/// Per-app rows are authoritative; timestamps predating them (history from
/// busywatch 0.1, or from before this version) are reconstructed from the
/// per-pid rows, which undercount many-process apps but are all that exists.
const INST_CTE: &str = "
    WITH inst AS (
         SELECT ts, comm, rss_kb rss, cpu_pct cpu, io_rd_ps rd, io_wr_ps wr, pids
           FROM app_sample WHERE ts BETWEEN ?1 AND ?2 AND (?4 = '' OR comm = ?4)
         UNION ALL
         SELECT ts, comm, SUM(rss_kb), SUM(cpu_pct),
                SUM(io_rd_ps), SUM(io_wr_ps), COUNT(DISTINCT pid)
           FROM consumer c WHERE ts BETWEEN ?1 AND ?2 AND (?4 = '' OR comm = ?4)
            AND NOT EXISTS (SELECT 1 FROM app_sample a WHERE a.ts = c.ts)
          GROUP BY ts, comm),
         w AS (
         SELECT *, MIN(COALESCE(ts - LAG(ts) OVER (PARTITION BY comm ORDER BY ts), 60), ?5) dt
           FROM inst)";

/// Top processes in a range by `metric` ("mem" | "cpu" | "io").  Values are
/// summed across the pids sharing a command name at each instant, then
/// aggregated over time — so a browser's many processes count as one hog.
pub fn hogs(
    conn: &Connection,
    from: i64,
    to: i64,
    metric: &str,
    limit: usize,
) -> rusqlite::Result<Vec<HogRow>> {
    let order = match metric {
        "cpu" => "cpu_secs DESC",
        "io" => "(io_rd + io_wr) DESC",
        _ => "rss_max DESC",
    };
    let sql = format!(
        "{INST_CTE}
         SELECT comm,
                MAX(rss) rss_max, AVG(rss) rss_avg,
                MAX(cpu) cpu_max, AVG(cpu) cpu_avg, SUM(cpu/100.0*dt) cpu_secs,
                MAX(rd+wr) io_max, SUM(rd*dt) io_rd, SUM(wr*dt) io_wr,
                MAX(pids), COUNT(*), MIN(ts), MAX(ts)
           FROM w GROUP BY comm ORDER BY {order} LIMIT ?3"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![from, to, limit as i64, "", GAP_CAP], |r| {
        Ok(HogRow {
            comm: r.get(0)?,
            rss_max_kb: r.get::<_, f64>(1)? as i64,
            rss_avg_kb: r.get::<_, f64>(2)? as i64,
            rss_last_kb: 0,
            cpu_max: r.get(3)?,
            cpu_avg: r.get(4)?,
            cpu_secs: r.get(5)?,
            io_max_ps: r.get::<_, f64>(6)? as i64,
            io_rd_bytes: r.get::<_, f64>(7)? as i64,
            io_wr_bytes: r.get::<_, f64>(8)? as i64,
            pids: r.get(9)?,
            samples: r.get(10)?,
            first_ts: r.get(11)?,
            last_ts: r.get(12)?,
        })
    })?;
    let mut rows: Vec<HogRow> = rows.collect::<Result<_, _>>()?;

    // Current size matters for a memory hog (is it still holding it?).  One
    // query for the whole last sample, not one per row.
    let mut last: HashMap<String, i64> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT comm, rss_kb FROM app_sample
              WHERE ts = (SELECT MAX(ts) FROM app_sample WHERE ts <= ?1)
             UNION ALL
             SELECT comm, SUM(rss_kb) FROM consumer
              WHERE ts = (SELECT MAX(ts) FROM consumer WHERE ts <= ?1)
                AND NOT EXISTS (SELECT 1 FROM app_sample a WHERE a.ts <= ?1)
              GROUP BY comm",
        )?;
        let it = stmt.query_map([to], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)? as i64)))?;
        for row in it {
            let (c, v) = row?;
            last.insert(c, v);
        }
    }
    for h in rows.iter_mut() {
        h.rss_last_kb = last.get(&h.comm).copied().unwrap_or(0);
    }
    Ok(rows)
}

/// Everything the per-app drilldown reports about one command name.
#[derive(Debug, Default)]
pub struct AppSummary {
    pub comm: String,
    pub samples: i64,
    pub first_ts: i64,
    pub last_ts: i64,
    pub covered_secs: i64,
    pub rss_max_kb: i64,
    pub rss_avg_kb: i64,
    pub rss_last_kb: i64,
    pub cpu_max: f64,
    pub cpu_avg: f64,
    pub cpu_secs: f64,
    pub io_max_ps: i64,
    pub io_rd_bytes: i64,
    pub io_wr_bytes: i64,
    pub pids_max: i64,
    pub pids_seen: i64,
}

pub fn app_summary(
    conn: &Connection,
    comm: &str,
    from: i64,
    to: i64,
) -> rusqlite::Result<AppSummary> {
    let sql = format!(
        "{INST_CTE}
         SELECT COUNT(*), MIN(ts), MAX(ts), SUM(dt),
                MAX(rss), AVG(rss),
                MAX(cpu), AVG(cpu), SUM(cpu/100.0*dt),
                MAX(rd+wr), SUM(rd*dt), SUM(wr*dt), MAX(pids)
           FROM w"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut a = stmt.query_row(params![from, to, 0i64, comm, GAP_CAP], |r| {
        Ok(AppSummary {
            comm: comm.to_string(),
            samples: r.get(0)?,
            first_ts: r.get::<_, Option<i64>>(1)?.unwrap_or(0),
            last_ts: r.get::<_, Option<i64>>(2)?.unwrap_or(0),
            covered_secs: r.get::<_, Option<f64>>(3)?.unwrap_or(0.0) as i64,
            rss_max_kb: r.get::<_, Option<f64>>(4)?.unwrap_or(0.0) as i64,
            rss_avg_kb: r.get::<_, Option<f64>>(5)?.unwrap_or(0.0) as i64,
            rss_last_kb: 0,
            cpu_max: r.get::<_, Option<f64>>(6)?.unwrap_or(0.0),
            cpu_avg: r.get::<_, Option<f64>>(7)?.unwrap_or(0.0),
            cpu_secs: r.get::<_, Option<f64>>(8)?.unwrap_or(0.0),
            io_max_ps: r.get::<_, Option<f64>>(9)?.unwrap_or(0.0) as i64,
            io_rd_bytes: r.get::<_, Option<f64>>(10)?.unwrap_or(0.0) as i64,
            io_wr_bytes: r.get::<_, Option<f64>>(11)?.unwrap_or(0.0) as i64,
            pids_max: r.get::<_, Option<i64>>(12)?.unwrap_or(0),
            pids_seen: 0,
        })
    })?;
    a.rss_last_kb = conn
        .query_row(
            "SELECT COALESCE(
                 (SELECT rss_kb FROM app_sample
                   WHERE comm = ?1 AND ts <= ?2 ORDER BY ts DESC LIMIT 1),
                 (SELECT SUM(rss_kb) FROM consumer
                   WHERE comm = ?1
                     AND ts = (SELECT MAX(ts) FROM consumer WHERE comm = ?1 AND ts <= ?2)))",
            params![comm, to],
            |r| r.get::<_, Option<f64>>(0),
        )
        .ok()
        .flatten()
        .unwrap_or(0.0) as i64;
    a.pids_seen = conn
        .query_row(
            "SELECT COUNT(DISTINCT pid) FROM consumer WHERE comm = ?1 AND ts BETWEEN ?2 AND ?3",
            params![comm, from, to],
            |r| r.get(0),
        )
        .unwrap_or(0);
    Ok(a)
}

/// The pids that carried one command name, biggest first.
pub struct PidRow {
    pub pid: i64,
    pub rss_max_kb: i64,
    pub cpu_max: f64,
    pub io_max_ps: i64,
    pub first_ts: i64,
    pub last_ts: i64,
}

pub fn app_pids(
    conn: &Connection,
    comm: &str,
    from: i64,
    to: i64,
    limit: usize,
) -> rusqlite::Result<Vec<PidRow>> {
    let mut stmt = conn.prepare(
        "SELECT pid, MAX(rss_kb), MAX(cpu_pct), MAX(io_rd_ps+io_wr_ps), MIN(ts), MAX(ts)
           FROM consumer WHERE comm = ?1 AND ts BETWEEN ?2 AND ?3
          GROUP BY pid ORDER BY MAX(rss_kb) DESC LIMIT ?4",
    )?;
    let rows = stmt.query_map(params![comm, from, to, limit as i64], |r| {
        Ok(PidRow {
            pid: r.get(0)?,
            rss_max_kb: r.get(1)?,
            cpu_max: r.get(2)?,
            io_max_ps: r.get(3)?,
            first_ts: r.get(4)?,
            last_ts: r.get(5)?,
        })
    })?;
    rows.collect()
}

/// Incidents this command was named the culprit of.
pub fn app_incidents(
    conn: &Connection,
    comm: &str,
    from: i64,
    to: i64,
) -> rusqlite::Result<Vec<IncidentRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, kind, started, ended, peak_avg60, min_mem_avail_kb, top_comm
           FROM incident
          WHERE top_comm = ?1 AND started <= ?3 AND COALESCE(ended, ?3) >= ?2
          ORDER BY started DESC LIMIT 20",
    )?;
    let rows = stmt.query_map(params![comm, from, to], |r| {
        Ok(IncidentRow {
            id: r.get(0)?,
            kind: r.get::<_, Option<String>>(1)?.unwrap_or_else(|| "cpu".into()),
            started: r.get(2)?,
            ended: r.get(3)?,
            peak_avg60: r.get(4)?,
            min_mem_avail_kb: r.get(5)?,
            top_comm: r.get(6)?,
        })
    })?;
    rows.collect()
}

/// Per-bucket series for a set of command names: (comm, [(bucket, max, avg)]).
pub fn hog_series(
    conn: &Connection,
    from: i64,
    to: i64,
    bucket: i64,
    metric: &str,
    comms: &[String],
) -> rusqlite::Result<HashMap<String, Vec<(i64, f64)>>> {
    let mut out: HashMap<String, Vec<(i64, f64)>> = HashMap::new();
    if comms.is_empty() {
        return Ok(out);
    }
    let (app_expr, pid_expr) = match metric {
        "cpu" => ("cpu_pct", "SUM(cpu_pct)"),
        "io" => ("io_rd_ps+io_wr_ps", "SUM(io_rd_ps+io_wr_ps)"),
        _ => ("rss_kb", "SUM(rss_kb)"),
    };
    let holes = comms.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "WITH inst AS (
             SELECT ts, comm, {app_expr} v FROM app_sample
              WHERE ts BETWEEN ?1 AND ?2 AND comm IN ({holes})
             UNION ALL
             SELECT ts, comm, {pid_expr} v FROM consumer c
              WHERE ts BETWEEN ?1 AND ?2 AND comm IN ({holes})
                AND NOT EXISTS (SELECT 1 FROM app_sample a WHERE a.ts = c.ts)
              GROUP BY ts, comm)
         SELECT (ts/?)*? b, comm, MAX(v) FROM inst GROUP BY b, comm ORDER BY b"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(from), Box::new(to)];
    // ?1/?2 are named, so the two comm lists that follow are positional from
    // ?3 onward — each branch of the UNION needs its own copy.
    for c in comms.iter().chain(comms.iter()) {
        args.push(Box::new(c.clone()));
    }
    let b = bucket.max(1);
    args.push(Box::new(b));
    args.push(Box::new(b));
    let refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|a| a.as_ref()).collect();
    let rows = stmt.query_map(refs.as_slice(), |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, f64>(2)?))
    })?;
    for row in rows {
        let (t, comm, v) = row?;
        out.entry(comm).or_default().push((t, v));
    }
    Ok(out)
}

/// Oldest and newest sample timestamps, for the UI's range limits.
pub fn span(conn: &Connection) -> (i64, i64) {
    conn.query_row("SELECT MIN(ts), MAX(ts) FROM sample", [], |r| {
        Ok((r.get::<_, Option<i64>>(0)?.unwrap_or(0), r.get::<_, Option<i64>>(1)?.unwrap_or(0)))
    })
    .unwrap_or((0, 0))
}
