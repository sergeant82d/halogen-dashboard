use crate::{
    rates::{Frame, apply_rates, cache_totals, totals},
    types::{Event, Sample, Status},
};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path};

pub const RAW_MS: i64 = 48 * 60 * 60 * 1000;
pub const RETAIN_MS: i64 = 30 * 24 * 60 * 60 * 1000;
pub const HOUR_MS: i64 = 60 * 60 * 1000;
pub const DAY_MS: i64 = 24 * HOUR_MS;
/// Hourly Rollups are kept for a year; daily Rollups are kept for all time.
pub const HOURLY_MS: i64 = 366 * DAY_MS;
/// The Halogen fields come first; an Outage blanks only those.
const HALOGEN_FIELDS: usize = 25;
/// A Halogen rate added after the hardware fields.
const TTFT: usize = 33;
pub const FIELDS: [&str; 34] = [
    "busy_slots",
    "queued",
    "kv_used",
    "kv_reserved",
    "kv_use",
    "kv_reserved_use",
    "gen_speed",
    "throughput",
    "prefill_speed",
    "prefill_throughput",
    "requests_per_min",
    "mean_service_time",
    "prompt_cache_hit_rate",
    "draft_acceptance",
    "kv_pool",
    "busy_for",
    "kv_waiting",
    "cache_entries",
    "cache_bytes",
    "cache_lookup_hit_rate",
    "cache_evictions_per_min",
    "cache_rewrites_per_min",
    "cache_disk_hits_per_min",
    "cache_disk_cut_backs_per_min",
    "cache_disk_bytes",
    "cpu_use",
    "cpu_temp",
    "gpu_use",
    "gpu_temp",
    "nvme_temp",
    "apu_power",
    "gpu_mem_used",
    "ram_used",
    "ttft",
];

fn values(frame: &Frame) -> [Option<f64>; FIELDS.len()] {
    let s = &frame.sample;
    [
        s.busy_slots,
        s.queued,
        s.kv_used,
        s.kv_reserved,
        s.kv_use,
        s.kv_reserved
            .zip(frame.kv_pool)
            .and_then(|(a, b)| crate::rates::ratio(a, b)),
        s.gen_speed,
        s.throughput,
        s.prefill_speed,
        s.prefill_throughput,
        s.requests_per_min,
        s.mean_service_time,
        s.prompt_cache_hit_rate,
        s.draft_acceptance,
        frame.kv_pool,
        s.busy_for,
        s.kv_waiting,
        s.cache_entries,
        s.cache_bytes,
        s.cache_lookup_hit_rate,
        s.cache_evictions_per_min,
        s.cache_rewrites_per_min,
        s.cache_disk_hits_per_min,
        s.cache_disk_cut_backs_per_min,
        s.cache_disk_bytes,
        s.cpu_use,
        s.cpu_temp,
        s.gpu_use,
        s.gpu_temp,
        s.nvme_temp,
        s.apu_power,
        s.gpu_mem_used,
        s.ram_used,
        s.ttft,
    ]
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Stat {
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub sum: f64,
    pub count: u64,
}
impl Stat {
    fn add(&mut self, v: Option<f64>) {
        if let Some(v) = v {
            self.min = Some(self.min.map_or(v, |a| a.min(v)));
            self.max = Some(self.max.map_or(v, |a| a.max(v)));
            self.sum += v;
            self.count += 1;
        }
    }
    fn merge(&mut self, other: &Self) {
        if let Some(v) = other.min {
            self.min = Some(self.min.map_or(v, |a| a.min(v)));
        }
        if let Some(v) = other.max {
            self.max = Some(self.max.map_or(v, |a| a.max(v)));
        }
        self.sum += other.sum;
        self.count += other.count;
    }
    pub fn mean(&self) -> Option<f64> {
        (self.count > 0).then(|| self.sum / self.count as f64)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rollup {
    pub t: i64,
    #[serde(deserialize_with = "read_stats")]
    pub stats: Vec<Stat>,
    pub status: Status,
    pub first: Frame,
    pub last: Frame,
}

fn read_stats<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<Stat>, D::Error> {
    let mut stats = Vec::<Stat>::deserialize(deserializer)?;
    // Existing stored Rollups contain 15 stats. Preserve their positions.
    stats.resize(FIELDS.len(), Stat::default());
    Ok(stats)
}
impl Rollup {
    pub fn new(frame: &Frame) -> Self {
        let mut r = Self {
            t: frame.sample.t / 60_000 * 60_000,
            stats: vec![Stat::default(); FIELDS.len()],
            status: frame.sample.status,
            first: frame.clone(),
            last: frame.clone(),
        };
        r.add(frame);
        r
    }
    pub fn add(&mut self, f: &Frame) {
        for (s, v) in self.stats.iter_mut().zip(values(f)) {
            s.add(v);
        }
        self.status = self.status.max(f.sample.status);
        self.last = f.clone();
    }
    fn merge(&mut self, r: &Self) {
        for (s, v) in self.stats.iter_mut().zip(&r.stats) {
            s.merge(v);
        }
        self.status = self.status.max(r.status);
        self.last = r.last.clone();
    }
}

pub struct Store {
    pub conn: Connection,
    pending: Option<Rollup>,
    /// Local time minus UTC, ms, so that days start at local midnight
    /// (HALOGEN_DASHBOARD_UTC_OFFSET_HOURS).
    pub offset: i64,
}
impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; CREATE TABLE IF NOT EXISTS raw(t INTEGER PRIMARY KEY, data TEXT NOT NULL); CREATE TABLE IF NOT EXISTS rollups(t INTEGER PRIMARY KEY,data TEXT NOT NULL); CREATE TABLE IF NOT EXISTS events(id INTEGER PRIMARY KEY,t INTEGER NOT NULL,data TEXT NOT NULL); CREATE INDEX IF NOT EXISTS events_time ON events(t); CREATE TABLE IF NOT EXISTS hourly(t INTEGER PRIMARY KEY,data TEXT NOT NULL); CREATE TABLE IF NOT EXISTS daily(t INTEGER PRIMARY KEY,data TEXT NOT NULL);")?;
        let pending = conn
            .query_row(
                "SELECT data FROM rollups ORDER BY t DESC LIMIT 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .ok()
            .map(|s| serde_json::from_str(&s))
            .transpose()?;
        let offset = std::env::var("HALOGEN_DASHBOARD_UTC_OFFSET_HOURS")
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
            .map_or(0, |h| (h * HOUR_MS as f64) as i64);
        let store = Self {
            conn,
            pending,
            offset,
        };
        store.backfill()?;
        Ok(store)
    }
    /// Fills empty hourly and daily tables from the minute Rollups that already exist.
    fn backfill(&self) -> Result<()> {
        let hours: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM hourly", [], |r| r.get(0))?;
        if hours > 0 {
            return Ok(());
        }
        let pending = self.pending.as_ref().map_or(i64::MAX, |r| r.t);
        let rollups: Vec<Rollup> = {
            let mut stmt = self
                .conn
                .prepare("SELECT data FROM rollups WHERE t<?1 ORDER BY t")?;
            stmt.query_map([pending], |r| r.get::<_, String>(0))?
                .map(|s| Ok(serde_json::from_str(&s?)?))
                .collect::<Result<_>>()?
        };
        self.conn.execute_batch("BEGIN")?;
        for r in &rollups {
            self.fold(r)?;
        }
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }
    /// Start of the bucket of `size` ms that holds `t`, in local time.
    pub fn bucket(&self, t: i64, size: i64) -> i64 {
        (t + self.offset).div_euclid(size) * size - self.offset
    }
    /// Adds a finished minute Rollup to its hourly and daily Rollups.
    fn fold(&self, r: &Rollup) -> Result<()> {
        for (table, size) in [("hourly", HOUR_MS), ("daily", DAY_MS)] {
            let t = self.bucket(r.t, size);
            let old = self
                .conn
                .query_row(
                    &format!("SELECT data FROM {table} WHERE t=?1"),
                    [t],
                    |row| row.get::<_, String>(0),
                )
                .ok()
                .map(|s| serde_json::from_str::<Rollup>(&s))
                .transpose()?;
            let merged = match old {
                Some(mut b) => {
                    b.merge(r);
                    b
                }
                None => Rollup { t, ..r.clone() },
            };
            self.conn.execute(
                &format!("INSERT OR REPLACE INTO {table}(t,data) VALUES(?1,?2)"),
                params![t, serde_json::to_string(&merged)?],
            )?;
        }
        Ok(())
    }
    pub fn insert(&mut self, frame: &Frame, raw: bool) -> Result<()> {
        if raw {
            self.conn.execute(
                "INSERT OR REPLACE INTO raw(t,data) VALUES(?1,?2)",
                params![frame.sample.t, serde_json::to_string(frame)?],
            )?;
        }
        let minute = frame.sample.t / 60_000 * 60_000;
        if self.pending.as_ref().is_some_and(|r| r.t != minute) {
            self.flush()?;
            if let Some(done) = self.pending.take() {
                self.fold(&done)?;
            }
        }
        match &mut self.pending {
            Some(r) => r.add(frame),
            None => self.pending = Some(Rollup::new(frame)),
        }
        Ok(())
    }
    pub fn flush(&self) -> Result<()> {
        if let Some(r) = &self.pending {
            self.conn.execute(
                "INSERT OR REPLACE INTO rollups(t,data) VALUES(?1,?2)",
                params![r.t, serde_json::to_string(r)?],
            )?;
        }
        Ok(())
    }
    pub fn event(&self, e: &Event) -> Result<()> {
        self.conn.execute(
            "INSERT INTO events(t,data) VALUES(?1,?2)",
            params![e.t, serde_json::to_string(e)?],
        )?;
        Ok(())
    }
    pub fn events(&self, from: i64, to: i64, newest: bool) -> Result<Vec<Event>> {
        let sql = if newest {
            "SELECT data FROM events WHERE t>=?1 AND t<=?2 ORDER BY t DESC,id DESC LIMIT 100"
        } else {
            "SELECT data FROM events WHERE t>=?1 AND t<=?2 ORDER BY t,id"
        };
        let mut stmt = self.conn.prepare(sql)?;
        stmt.query_map(params![from, to], |r| r.get::<_, String>(0))?
            .map(|s| Ok(serde_json::from_str(&s?)?))
            .collect()
    }
    pub fn prune(&self, now: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM raw WHERE t<?1", [now - RAW_MS])?;
        self.conn
            .execute("DELETE FROM rollups WHERE t<?1", [now - RETAIN_MS])?;
        self.conn
            .execute("DELETE FROM hourly WHERE t<?1", [now - HOURLY_MS])?;
        self.conn
            .execute("DELETE FROM events WHERE t<?1", [now - RETAIN_MS])?;
        Ok(())
    }
    pub fn series(&self, from: i64, to: i64, step: Option<i64>, now: i64) -> Result<Value> {
        let (step, source) = selection(from, to, step, now)?;
        self.flush()?;
        let window = step.max(30_000);
        let start = self.bucket(from, step);
        let mut buckets: BTreeMap<i64, Rollup> = BTreeMap::new();
        let mut frames: Vec<Frame> = Vec::new();
        if source == "raw" {
            let mut stmt = self
                .conn
                .prepare("SELECT data FROM raw WHERE t>=?1 AND t<=?2 ORDER BY t")?;
            let rows = stmt.query_map(params![start - window - 2000, to], |r| {
                r.get::<_, String>(0)
            })?;
            for row in rows {
                let f: Frame = serde_json::from_str(&row?)?;
                if f.sample.t >= from {
                    let t = self.bucket(f.sample.t, step);
                    match buckets.get_mut(&t) {
                        Some(r) => r.add(&f),
                        None => {
                            buckets.insert(t, Rollup::new(&f));
                        }
                    }
                }
                frames.push(f);
            }
        } else {
            let (table, size) = match source {
                "hourly" => ("hourly", HOUR_MS),
                "daily" => ("daily", DAY_MS),
                _ => ("rollups", 60_000),
            };
            let mut stmt = self.conn.prepare(&format!(
                "SELECT data FROM {table} WHERE t>=?1 AND t<=?2 ORDER BY t"
            ))?;
            let rows = stmt.query_map(params![start - window - 2 * size, to], |r| {
                r.get::<_, String>(0)
            })?;
            for row in rows {
                let r: Rollup = serde_json::from_str(&row?)?;
                if r.t + size > from && r.t <= to {
                    let t = self.bucket(r.t, step);
                    match buckets.get_mut(&t) {
                        Some(b) => b.merge(&r),
                        None => {
                            buckets.insert(t, r.clone());
                        }
                    }
                }
                frames.push(r.first);
                frames.push(r.last);
            }
        }
        let mut result =
            json!({"from":from,"to":to,"step":step,"source":source,"t":[],"status":[]});
        for name in FIELDS {
            result[name] = json!([]);
        }
        for name in [
            "busy_slots_max",
            "queued_max",
            "kv_use_max",
            "busy_for_max",
            "kv_waiting_max",
            "cpu_temp_max",
            "gpu_temp_max",
            "apu_power_max",
        ] {
            result[name] = json!([]);
        }
        // Hourly and daily buckets hold many Outages and Restarts: they show the
        // mean of the Sample rates, and Down only when no Sample was Up.
        let long = matches!(source, "hourly" | "daily");
        let mut summary = vec![Stat::default(); FIELDS.len()];
        for t in (start..=to).step_by(step as usize) {
            result["t"].as_array_mut().unwrap().push(json!(t));
            let r = buckets.get(&t);
            if let Some(r) = r {
                for (s, v) in summary.iter_mut().zip(&r.stats) {
                    s.merge(v);
                }
            }
            result["status"]
                .as_array_mut()
                .unwrap()
                .push(json!(r.map(|r| {
                    if long && r.status == Status::Down && r.stats[0].count > 0 {
                        Status::Degraded
                    } else {
                        r.status
                    }
                })));
            let mut vs: [Option<f64>; FIELDS.len()] =
                std::array::from_fn(|i| r.and_then(|r| r.stats[i].mean()));
            if let Some(r) = r.filter(|_| !long) {
                if r.status == Status::Down {
                    vs[..HALOGEN_FIELDS].fill(None);
                    vs[TTFT] = None;
                } else {
                    let last = &r.last;
                    let boundary = last.sample.t - window;
                    let idx = frames.partition_point(|f| f.sample.t <= boundary);
                    let first = frames
                        .get(idx.saturating_sub(1))
                        .filter(|f| f.sample.t < last.sample.t);
                    let mut s = Sample::default();
                    if let Some(first) = first {
                        apply_rates(&mut s, first, last, window);
                    }
                    vs[6..14].copy_from_slice(&[
                        s.gen_speed,
                        s.throughput,
                        s.prefill_speed,
                        s.prefill_throughput,
                        s.requests_per_min,
                        s.mean_service_time,
                        s.prompt_cache_hit_rate,
                        s.draft_acceptance,
                    ]);
                    vs[19..24].copy_from_slice(&[
                        s.cache_lookup_hit_rate,
                        s.cache_evictions_per_min,
                        s.cache_rewrites_per_min,
                        s.cache_disk_hits_per_min,
                        s.cache_disk_cut_backs_per_min,
                    ]);
                    vs[TTFT] = s.ttft;
                }
            }
            for (name, v) in FIELDS.iter().zip(vs) {
                result[*name].as_array_mut().unwrap().push(json!(v));
            }
            for (name, index) in [
                ("busy_slots_max", 0),
                ("queued_max", 1),
                ("kv_use_max", 4),
                ("busy_for_max", 15),
                ("kv_waiting_max", 16),
            ] {
                result[name].as_array_mut().unwrap().push(json!(
                    r.filter(|r| long || r.status != Status::Down)
                        .and_then(|r| r.stats[index].max)
                ));
            }
            for (name, index) in [
                ("cpu_temp_max", 26),
                ("gpu_temp_max", 28),
                ("apu_power_max", 30),
            ] {
                result[name]
                    .as_array_mut()
                    .unwrap()
                    .push(json!(r.and_then(|r| r.stats[index].max)));
            }
        }
        let first = frames
            .iter()
            .rev()
            .find(|f| f.sample.t <= from && f.sample.totals.is_some())
            .or_else(|| {
                frames
                    .iter()
                    .find(|f| f.sample.t >= from && f.sample.totals.is_some())
            });
        let last = frames
            .iter()
            .rev()
            .find(|f| f.sample.t <= to && f.sample.totals.is_some());
        let mut delta = [0.0; 11];
        let mut cache_delta = [0.0; 6];
        if let (Some(a), Some(b)) = (first, last) {
            for (i, v) in delta.iter_mut().enumerate() {
                *v = (b.counters[i] - a.counters[i]).max(0.0);
            }
            let a = a.sample.totals.unwrap_or_default().cache_counters();
            let b = b.sample.totals.unwrap_or_default().cache_counters();
            for (i, v) in cache_delta.iter_mut().enumerate() {
                *v = (b[i] - a[i]).max(0.0);
            }
        }
        let mut totals = totals(&delta);
        cache_totals(&mut totals, &cache_delta);
        result["totals_in_range"] = serde_json::to_value(totals)?;
        // Mean, lowest and highest Sample value of each field over the whole range.
        result["summary"] = json!({});
        for (name, s) in FIELDS.iter().zip(&summary) {
            result["summary"][*name] = json!({"mean":s.mean(),"min":s.min,"max":s.max});
        }
        Ok(result)
    }
}

pub fn selection(from: i64, to: i64, step: Option<i64>, now: i64) -> Result<(i64, &'static str)> {
    if to < from || from < 0 {
        bail!("from must be nonnegative and not after to");
    }
    let span = to.checked_sub(from).context("range is too large")?;
    let step = step.unwrap_or((span / 1000).max(1000)).max(1000);
    if step > HOURLY_MS {
        bail!("step must not exceed 366 days");
    }
    let (source, size) = if from >= now - RAW_MS && step < 60_000 {
        ("raw", 1)
    } else if from >= now - RETAIN_MS && step < HOUR_MS {
        ("rollup", 60_000)
    } else if from >= now - HOURLY_MS && step < DAY_MS {
        ("hourly", HOUR_MS)
    } else {
        ("daily", DAY_MS)
    };
    let step = (step + size - 1) / size * size;
    if span / step > 100_000 {
        bail!("series exceeds 100000 points; increase step");
    }
    Ok((step, source))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cache_series_recomputes_rates_and_keeps_gauge_means_and_maxima() {
        let mut tracker = crate::rates::CounterTracker::default();
        let mut store = Store::open(":memory:").unwrap();
        for sec in 0..=90 {
            let text = format!(
                "halogen:cache_hits_total {}\nhalogen:cache_misses_total {}\nhalogen:cache_evicted_total {}\nhalogen:cache_last_message_rewrites_total {}\nhalogen:cache_disk_hits_total {}\nhalogen:cache_disk_cut_back_total {}\nhalogen:busy_for_seconds {}\nhalogen:kv_pool_waiting_for_room {}\nhalogen:cache_entries 4\nhalogen:cache_bytes 400\nhalogen:cache_disk_bytes 0",
                sec * 3,
                sec,
                sec * 2,
                sec,
                sec * 4,
                sec * 5,
                sec,
                sec
            );
            let (mut f, _) = tracker.sample(sec * 1000, Some(&text), Status::Up);
            f.sample.cache_lookup_hit_rate = Some(999.0);
            store.insert(&f, true).unwrap();
        }
        for step in [30_000, 60_000] {
            let series = store.series(0, 90_000, Some(step), 90_000).unwrap();
            assert_eq!(series["cache_lookup_hit_rate"][0], 0.75);
            assert_eq!(series["cache_evictions_per_min"][0], 120.0);
            assert_eq!(series["cache_rewrites_per_min"][0], 60.0);
            assert_eq!(series["cache_disk_hits_per_min"][0], 240.0);
            assert_eq!(series["cache_disk_cut_backs_per_min"][0], 300.0);
            assert_eq!(series["busy_for"][0], (step / 1000 - 1) as f64 / 2.0);
            assert_eq!(series["busy_for_max"][0], (step / 1000 - 1) as f64);
            assert_eq!(series["kv_waiting_max"][0], (step / 1000 - 1) as f64);
            assert_eq!(series["totals_in_range"]["cache_hits"], 270.0);
        }
    }
    #[test]
    fn rollup_keeps_min_mean_max_and_worst_status() {
        let mut f = Frame::default();
        f.sample.busy_slots = Some(1.0);
        let mut r = Rollup::new(&f);
        f.sample.busy_slots = Some(4.0);
        f.sample.status = Status::Degraded;
        r.add(&f);
        f.sample.busy_slots = None;
        f.sample.status = Status::Down;
        r.add(&f);
        assert_eq!(r.stats[0].min, Some(1.0));
        assert_eq!(r.stats[0].max, Some(4.0));
        assert_eq!(r.stats[0].mean(), Some(2.5));
        assert_eq!(r.status, Status::Down);
    }
    #[test]
    fn selection_uses_raw_only_for_recent_subminute_ranges() {
        let now = RETAIN_MS;
        assert_eq!(
            selection(now - 100_000, now, None, now).unwrap(),
            (1000, "raw")
        );
        assert_eq!(
            selection(now - 100_000, now, Some(60_000), now).unwrap(),
            (60_000, "rollup")
        );
        assert_eq!(
            selection(now - RAW_MS - 1, now, Some(1000), now).unwrap(),
            (60_000, "rollup")
        );
        assert!(selection(2, 1, None, now).is_err());
        assert!(selection(0, 1000, Some(i64::MAX), now).is_err());
        let now = 2 * HOURLY_MS;
        assert_eq!(
            selection(now - 365 * DAY_MS, now, None, now).unwrap(),
            (9 * HOUR_MS, "hourly")
        );
        assert_eq!(
            selection(now - HOURLY_MS - DAY_MS, now, None, now).unwrap(),
            (DAY_MS, "daily")
        );
    }

    #[test]
    fn minute_rollups_fold_into_local_hours_and_days_with_a_range_summary() {
        let mut store = Store::open(":memory:").unwrap();
        store.offset = -6 * HOUR_MS;
        // Two days of one Sample a minute; GPU temperature 40 + minute of the day / 100.
        let start = 400 * DAY_MS;
        for m in 0..2 * 24 * 60 + 1 {
            let mut f = Frame::default();
            f.sample.t = start + m * 60_000;
            f.sample.gpu_temp = Some(40.0 + (m % 1440) as f64 / 100.0);
            f.sample.busy_slots = Some(1.0);
            if m == 30 {
                f.sample.status = Status::Down;
                f.sample.busy_slots = None;
            }
            store.insert(&f, false).unwrap();
        }
        let days: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM daily", [], |r| r.get(0))
            .unwrap();
        // Local days start at 06:00 UTC: 00:00-06:00, a full day, and 06:00-24:00.
        assert_eq!(days, 3);
        let hours: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM hourly", [], |r| r.get(0))
            .unwrap();
        assert_eq!(hours, 48);
        let now = start + 2 * DAY_MS;
        let s = store
            .series(now - HOURLY_MS - DAY_MS, now, None, now)
            .unwrap();
        assert_eq!(s["source"], "daily");
        let s = store.series(start, now, Some(HOUR_MS), now).unwrap();
        assert_eq!(s["source"], "hourly");
        // The first hour held an Outage minute but was mostly Up.
        assert_eq!(s["status"][0], "degraded");
        assert_eq!(s["busy_slots"][0], 1.0);
        assert_eq!(s["summary"]["gpu_temp"]["min"], 40.0);
        let max = s["summary"]["gpu_temp"]["max"].as_f64().unwrap();
        assert!((max - 54.39).abs() < 1e-9);
        assert_eq!(s["summary"]["busy_slots"]["mean"], 1.0);
    }

    #[test]
    fn series_recomputes_rates_from_counters_and_returns_null_outage_buckets() {
        let mut store = Store::open(":memory:").unwrap();
        for sec in 0..=90 {
            let mut f = Frame::default();
            f.sample.t = sec * 1000;
            f.counters[0] = sec as f64;
            f.counters[2] = sec as f64 * 40.0;
            f.counters[8] = sec as f64;
            f.sample.totals = Some(totals(&f.counters));
            f.sample.gen_speed = Some(999.0);
            f.sample.busy_slots = Some(2.0);
            f.sample.kv_reserved = Some(100.0);
            f.kv_pool = Some(200.0);
            if sec == 80 {
                f.sample.status = Status::Down;
                f.sample.totals = None;
                f.sample.gen_speed = None;
            }
            store.insert(&f, true).unwrap();
        }
        let raw = store.series(30_000, 90_000, Some(1000), 90_000).unwrap();
        assert_eq!(raw["source"], "raw");
        assert_eq!(raw["gen_speed"][0], 40.0);
        assert_eq!(raw["kv_reserved_use"][0], 0.5);
        assert_eq!(raw["status"][50], "down");
        assert!(raw["gen_speed"][50].is_null());
        assert_eq!(raw["totals_in_range"]["predicted_tokens"], 2400.0);
        let rollup = store.series(0, 90_000, Some(60_000), 90_000).unwrap();
        assert_eq!(rollup["source"], "rollup");
        assert_eq!(rollup["gen_speed"][0], 40.0);
        assert_eq!(rollup["status"][1], "down");
        assert!(rollup["gen_speed"][1].is_null());
    }
}
