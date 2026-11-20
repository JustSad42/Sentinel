//! Boot timeline and startup impact.
//!
//! Sentinel starts at sign-in, so it watches the first minutes of each session: when the desktop
//! appeared, when each app started, how much CPU time and disk I/O each app used in its first two
//! minutes, and when the system settled down. The last record is kept in boot.json.

use crate::sampler::Proc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Track apps that start within this long after the reference point.
const WINDOW_SECS: f64 = 600.0;
/// Each app's cost is measured over its first this-many seconds.
const MEASURE_SECS: f64 = 120.0;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Impact {
    Low,
    Medium,
    High,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct AppImpact {
    pub path: String,
    pub name: String,
    /// Seconds after the reference point (power-on, or sign-in after a Fast Startup boot).
    pub start_secs: f32,
    pub cpu_ms: u64,
    pub io_bytes: u64,
    pub processes: u32,
    /// False when Sentinel first saw it after its two-minute measuring window (numbers run high).
    pub exact: bool,
}

impl AppImpact {
    /// Same thresholds as Windows uses for startup impact.
    pub fn level(&self) -> Impact {
        if self.cpu_ms > 1000 || self.io_bytes > 3 << 20 {
            Impact::High
        } else if self.cpu_ms > 300 || self.io_bytes > 300 << 10 {
            Impact::Medium
        } else {
            Impact::Low
        }
    }
    pub fn label(&self) -> &'static str {
        match self.level() {
            Impact::High => "High",
            Impact::Medium => "Medium",
            Impact::Low => "Low",
        }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct BootRecord {
    /// Unix time the timeline starts from.
    pub boot_unix: i64,
    /// True when the timeline starts at sign-in (Fast Startup resumes keep the old boot time).
    pub from_sign_in: bool,
    pub desktop_secs: Option<f32>,
    pub sentinel_secs: Option<f32>,
    /// When CPU use first stayed low for 10 s after the desktop appeared.
    pub settled_secs: Option<f32>,
    pub apps: Vec<AppImpact>,
}

impl BootRecord {
    pub fn impact_for(&self, target: &str) -> Option<&AppImpact> {
        let t = target.trim().trim_matches('"').to_lowercase();
        if t.is_empty() {
            return None;
        }
        if let Some(a) = self.apps.iter().find(|a| a.path.to_lowercase() == t) {
            return Some(a);
        }
        let file = std::path::Path::new(&t).file_name().map(|f| f.to_string_lossy().to_string())?;
        self.apps.iter().find(|a| std::path::Path::new(&a.path.to_lowercase()).file_name().is_some_and(|f| f.to_string_lossy() == file))
    }
}

fn path() -> std::path::PathBuf {
    crate::config::data_dir().join("boot.json")
}

pub fn load() -> BootRecord {
    std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save(r: &BootRecord) {
    if let Ok(b) = serde_json::to_vec_pretty(r) {
        let p = path();
        let tmp = p.with_extension("json.tmp");
        if std::fs::write(&tmp, b).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}

fn ft_secs(ft: i64) -> f64 {
    (ft as f64 - 116_444_736_000_000_000.0) / 10_000_000.0
}

fn is_system_path(p: &str) -> bool {
    let l = p.to_ascii_lowercase();
    l.starts_with("c:\\windows\\") || l.contains("\\windowsapps\\microsoftwindows.")
}

#[derive(Default)]
pub struct Tracker {
    rec: Option<BootRecord>,
    reference: f64,
    done: bool,
    started: bool,
    calm_since: Option<f64>,
    per_pid: HashMap<(u32, i64), (String, String, f64, u64, u64, bool)>,
    ticks: u64,
}

impl Tracker {
    pub fn sample(&mut self, procs: &[Proc], cpu_total: f32) {
        if self.done {
            return;
        }
        let now = crate::win::now_unix() as f64;
        if !self.started {
            self.started = true;
            let boot = now - crate::win::uptime_secs() as f64;
            // Sign-in = the user's first explorer.exe.
            let explorer = procs.iter().filter(|p| p.session > 0 && p.name.eq_ignore_ascii_case("explorer.exe")).map(|p| ft_secs(p.create_time)).fold(f64::INFINITY, f64::min);
            let explorer = if explorer.is_finite() { Some(explorer) } else { None };
            // Cold boot: the desktop came up within 10 minutes of power-on. Otherwise (Fast Startup
            // resume) measure from sign-in.
            let (reference, from_sign_in) = match explorer {
                Some(e) if e - boot < WINDOW_SECS => (boot, false),
                Some(e) => (e - 1.0, true),
                None => (boot, false),
            };
            if now - reference > WINDOW_SECS + MEASURE_SECS {
                // Sentinel started too late in this session to measure it; keep the last record.
                self.done = true;
                return;
            }
            let old = load();
            if (old.boot_unix - reference as i64).abs() < 30 {
                // Same session (Sentinel restarted, e.g. after an update): keep adding to it.
                self.rec = Some(old);
            } else {
                self.rec = Some(BootRecord { boot_unix: reference as i64, from_sign_in, desktop_secs: explorer.map(|e| (e - reference) as f32), sentinel_secs: Some((now - reference) as f32), ..Default::default() });
            }
            self.reference = reference;
        }
        let reference = self.reference;
        let Some(rec) = self.rec.as_mut() else { return };
        let elapsed = now - reference;

        for p in procs {
            if p.session == 0 {
                continue;
            }
            let Some(path) = p.path.as_deref() else { continue };
            if is_system_path(path) {
                continue;
            }
            let start = ft_secs(p.create_time) - reference;
            if !(0.0..=WINDOW_SECS).contains(&start) {
                continue;
            }
            let age = now - ft_secs(p.create_time);
            let e = self.per_pid.entry(p.key()).or_insert_with(|| (path.to_string(), p.name.clone(), start, 0, 0, age <= MEASURE_SECS + 5.0));
            // Lifetime counters: keep updating until the app is two minutes old.
            if age <= MEASURE_SECS || (e.3 == 0 && e.4 == 0) {
                e.3 = p.cpu_time / 10_000;
                e.4 = p.io_total;
            }
        }

        // Settled: CPU under 15% for 10 s once the desktop is up.
        if rec.settled_secs.is_none() && rec.desktop_secs.is_some_and(|d| elapsed > d as f64) {
            if cpu_total < 15.0 {
                let since = *self.calm_since.get_or_insert(elapsed);
                if elapsed - since >= 10.0 {
                    rec.settled_secs = Some(since as f32);
                }
            } else {
                self.calm_since = None;
            }
        }

        self.ticks += 1;
        let finished = elapsed > WINDOW_SECS + MEASURE_SECS;
        if self.ticks % 15 == 0 || finished {
            // Fold per-process numbers into per-app rows.
            let mut apps: HashMap<String, AppImpact> = rec.apps.iter().map(|a| (a.path.to_lowercase(), a.clone())).collect();
            let mut fresh: HashMap<String, AppImpact> = HashMap::new();
            for (path, name, start, cpu, io, exact) in self.per_pid.values() {
                let a = fresh.entry(path.to_lowercase()).or_insert_with(|| AppImpact { path: path.clone(), name: name.clone(), start_secs: *start as f32, exact: true, ..Default::default() });
                a.start_secs = a.start_secs.min(*start as f32);
                a.cpu_ms += cpu;
                a.io_bytes += io;
                a.processes += 1;
                a.exact &= *exact;
            }
            for (k, v) in fresh {
                apps.insert(k, v);
            }
            let mut list: Vec<AppImpact> = apps.into_values().collect();
            list.sort_by(|a, b| a.start_secs.total_cmp(&b.start_secs));
            rec.apps = list;
            save(rec);
        }
        if finished {
            self.done = true;
        }
    }
}
