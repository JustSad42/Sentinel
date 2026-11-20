//! Per-app 24-hour history (one-minute averages of CPU and memory, by executable name) and the
//! memory-leak check built on it. Recorded by the sampler, so it keeps going while Sentinel is hidden.

use crate::sampler::Proc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Minutes kept (24 h).
const KEEP_MIN: u32 = 1440;
/// Only minutes where an app used at least this much are stored (keeps the file small).
const MIN_MEM: u64 = 30 << 20;
const MIN_CPU: f32 = 0.5;

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Series {
    /// Unix minute of each point.
    pub m: Vec<u32>,
    /// CPU % × 100.
    pub c: Vec<u16>,
    /// Memory in MB.
    pub r: Vec<u32>,
}

impl Series {
    fn push(&mut self, minute: u32, cpu: f32, mem: u64) {
        self.m.push(minute);
        self.c.push((cpu * 100.0).round().clamp(0.0, 65535.0) as u16);
        self.r.push((mem >> 20) as u32);
    }
    fn prune(&mut self, oldest: u32) {
        let cut = self.m.iter().position(|m| *m >= oldest).unwrap_or(self.m.len());
        if cut > 0 {
            self.m.drain(..cut);
            self.c.drain(..cut);
            self.r.drain(..cut);
        }
    }
    pub fn last_minute(&self) -> Option<u32> {
        self.m.last().copied()
    }
}

/// An app whose memory has only gone up for a long time.
#[derive(Clone)]
pub struct Leak {
    pub name: String,
    pub from_mb: u32,
    pub to_mb: u32,
    pub minutes: u32,
}

impl Leak {
    pub fn text(&self) -> String {
        format!("Memory has kept growing for {}: {} → {}", crate::fmt::duration(self.minutes as u64 * 60), crate::fmt::bytes((self.from_mb as u64) << 20), crate::fmt::bytes((self.to_mb as u64) << 20))
    }
}

#[derive(Default)]
pub struct AppHist {
    pub apps: HashMap<String, Series>,
    acc: HashMap<String, (f32, u64)>,
    acc_n: u32,
    minute: u32,
}

impl AppHist {
    pub fn sample(&mut self, procs: &[Proc], now_unix: i64) {
        let minute = (now_unix / 60) as u32;
        if self.minute == 0 {
            self.minute = minute;
        }
        if minute != self.minute {
            self.flush();
            self.minute = minute;
        }
        // Sum per app for this tick, then add to the minute's running total.
        let mut tick: HashMap<&str, (f32, u64)> = HashMap::new();
        for p in procs {
            if p.pid <= 4 {
                continue;
            }
            let e = tick.entry(p.name.as_str()).or_default();
            e.0 += p.cpu;
            e.1 += p.mem;
        }
        for (name, (c, m)) in tick {
            let e = self.acc.entry(name.to_lowercase()).or_default();
            e.0 += c;
            e.1 += m;
        }
        self.acc_n += 1;
    }

    fn flush(&mut self) {
        let n = self.acc_n.max(1);
        for (name, (c, m)) in self.acc.drain() {
            let (cpu, mem) = (c / n as f32, m / n as u64);
            if mem >= MIN_MEM || cpu >= MIN_CPU {
                self.apps.entry(name).or_default().push(self.minute, cpu, mem);
            }
        }
        self.acc_n = 0;
        let oldest = self.minute.saturating_sub(KEEP_MIN);
        self.apps.retain(|_, s| {
            s.prune(oldest);
            !s.m.is_empty()
        });
    }

    /// `name` is the exe name (any case).
    pub fn series(&self, name: &str) -> Option<&Series> {
        self.apps.get(&name.to_lowercase())
    }

    /// Apps still running whose memory has grown steadily for at least 90 minutes.
    pub fn leaks(&self, now_unix: i64) -> Vec<Leak> {
        let now_min = (now_unix / 60) as u32;
        let mut out = Vec::new();
        for (name, s) in &self.apps {
            if s.last_minute().is_none_or(|m| now_min.saturating_sub(m) > 3) || s.m.len() < 90 {
                continue;
            }
            // Walk back over the latest unbroken run: no gap > 10 min and no big drop (a restart).
            let mut start = s.m.len() - 1;
            while start > 0 {
                let gap = s.m[start] - s.m[start - 1];
                if gap > 10 || (s.r[start] as f32) < s.r[start - 1] as f32 * 0.7 {
                    break;
                }
                start -= 1;
            }
            let run = &s.r[start..];
            let minutes = s.m[s.m.len() - 1] - s.m[start];
            if minutes < 90 || run.len() < 60 {
                continue;
            }
            // Six slices: each one's median must stay at or above the one before (one wobble allowed).
            let seg = run.len() / 6;
            let med = |sl: &[u32]| {
                let mut v = sl.to_vec();
                v.sort_unstable();
                v[v.len() / 2]
            };
            let meds: Vec<u32> = (0..6).map(|i| med(&run[i * seg..if i == 5 { run.len() } else { (i + 1) * seg }])).collect();
            let falls = meds.windows(2).filter(|w| (w[1] as f32) < w[0] as f32 * 0.98).count();
            let (from, to) = (meds[0], meds[5]);
            if falls <= 1 && to >= from + 400 && to as f32 >= from as f32 * 1.4 && meds[5] >= meds[3] {
                out.push(Leak { name: name.clone(), from_mb: from, to_mb: to, minutes });
            }
        }
        out.sort_by(|a, b| (b.to_mb - b.from_mb).cmp(&(a.to_mb - a.from_mb)));
        out
    }

    /// Apps averaged over minutes `from..=to`, heaviest first (by memory or CPU): (exe, cpu %, MB).
    pub fn top_between(&self, from: u32, to: u32, by_mem: bool, n: usize) -> Vec<(String, f32, u32)> {
        let mut out: Vec<(String, f32, u32)> = Vec::new();
        let span = (to.saturating_sub(from) + 1) as f32;
        for (name, s) in &self.apps {
            let a = s.m.partition_point(|m| *m < from);
            let b = s.m.partition_point(|m| *m <= to);
            if a >= b {
                continue;
            }
            // Average over the whole window (minutes it didn't run count as zero CPU).
            let cpu = s.c[a..b].iter().map(|c| *c as f32 / 100.0).sum::<f32>() / span.max((b - a) as f32);
            let mem = (s.r[a..b].iter().map(|r| *r as u64).sum::<u64>() / (b - a) as u64) as u32;
            out.push((name.clone(), cpu, mem));
        }
        if by_mem {
            out.sort_by(|x, y| y.2.cmp(&x.2));
        } else {
            out.sort_by(|x, y| y.1.total_cmp(&x.1));
        }
        out.truncate(n);
        out
    }

    pub fn first_minute(&self) -> Option<u32> {
        self.apps.values().filter_map(|s| s.m.first().copied()).min()
    }

    pub fn load() -> Self {
        let apps: HashMap<String, Series> = std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let mut h = AppHist { apps, ..Default::default() };
        let oldest = (crate::win::now_unix() / 60) as u32 - KEEP_MIN;
        h.apps.retain(|_, s| {
            s.prune(oldest);
            !s.m.is_empty()
        });
        h
    }

    pub fn save(&self) {
        if let Ok(b) = serde_json::to_vec(&self.apps) {
            let _ = std::fs::write(path(), b);
        }
    }
}

fn path() -> std::path::PathBuf {
    crate::config::data_dir().join("app-history.json")
}

/// Averages `s` into `n` buckets covering the last `range_secs`, CPU and memory (bytes); NaN = no data.
pub fn buckets(s: Option<&Series>, now_unix: i64, range_secs: i64, n: usize) -> (std::collections::VecDeque<f32>, std::collections::VecDeque<f32>) {
    let mut cpu = vec![(0.0f32, 0u32); n];
    let mut mem = vec![(0.0f32, 0u32); n];
    if let Some(s) = s {
        let start = now_unix - range_secs;
        let bw = range_secs as f32 / n as f32;
        for i in 0..s.m.len() {
            let t = s.m[i] as i64 * 60 + 30;
            if t < start {
                continue;
            }
            let b = (((t - start) as f32) / bw) as usize;
            if b < n {
                cpu[b].0 += s.c[i] as f32 / 100.0;
                cpu[b].1 += 1;
                mem[b].0 += (s.r[i] as f32) * 1048576.0;
                mem[b].1 += 1;
            }
        }
    }
    let avg = |v: Vec<(f32, u32)>| v.into_iter().map(|(s, c)| if c > 0 { s / c as f32 } else { f32::NAN }).collect();
    (avg(cpu), avg(mem))
}
