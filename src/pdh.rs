//! Performance counters (PDH) for GPU and physical disks.

use crate::win::{from_pwstr, wide};
use std::collections::HashMap;
use windows_sys::Win32::System::Performance::*;

pub struct Pdh {
    query: PDH_HQUERY,
    gpu_engine: PDH_HCOUNTER,
    gpu_proc_mem: PDH_HCOUNTER,
    gpu_adapter_ded: PDH_HCOUNTER,
    gpu_adapter_shared: PDH_HCOUNTER,
    disk_read: PDH_HCOUNTER,
    disk_write: PDH_HCOUNTER,
    disk_idle: PDH_HCOUNTER,
    primed: bool,
}
unsafe impl Send for Pdh {}

#[derive(Clone, Default)]
pub struct GpuSample {
    pub util: f32,
    pub engines: Vec<(String, f32)>,
    pub per_pid: HashMap<u32, f32>,
    pub mem_per_pid: HashMap<u32, u64>,
    pub dedicated_used: u64,
    pub shared_used: u64,
}

#[derive(Clone, Default)]
pub struct DiskSample {
    pub name: String,
    pub read: f64,
    pub write: f64,
    pub active: f32,
    pub read_iops: f64,
    pub write_iops: f64,
    /// Average time per I/O over the last interval, milliseconds.
    pub response_ms: f64,
    pub queue: u32,
    pub temperature: Option<i16>,
    pub info: Option<std::sync::Arc<crate::disk::DiskInfo>>,
    pub volumes: Vec<crate::disk::Volume>,
}

impl Pdh {
    pub fn new() -> Option<Self> {
        unsafe {
            let mut q: PDH_HQUERY = std::mem::zeroed();
            if PdhOpenQueryW(std::ptr::null(), 0, &mut q) != 0 {
                return None;
            }
            let add = |path: &str| -> PDH_HCOUNTER {
                let mut c: PDH_HCOUNTER = std::mem::zeroed();
                let w = wide(path);
                if PdhAddEnglishCounterW(q, w.as_ptr(), 0, &mut c) != 0 {
                    return std::mem::zeroed();
                }
                c
            };
            Some(Self {
                query: q,
                gpu_engine: add("\\GPU Engine(*)\\Utilization Percentage"),
                gpu_proc_mem: add("\\GPU Process Memory(*)\\Dedicated Usage"),
                gpu_adapter_ded: add("\\GPU Adapter Memory(*)\\Dedicated Usage"),
                gpu_adapter_shared: add("\\GPU Adapter Memory(*)\\Shared Usage"),
                disk_read: std::mem::zeroed(),
                disk_write: std::mem::zeroed(),
                disk_idle: std::mem::zeroed(),
                primed: false,
            })
        }
    }

    pub fn collect(&mut self) -> bool {
        let ok = unsafe { PdhCollectQueryData(self.query) } == 0;
        let was = self.primed;
        self.primed = true;
        ok && was
    }

    fn array(counter: PDH_HCOUNTER, large: bool) -> Vec<(String, f64)> {
        let mut out = Vec::new();
        if counter as usize == 0 {
            return out;
        }
        unsafe {
            let fmt = if large { PDH_FMT_LARGE } else { PDH_FMT_DOUBLE | 0x8000 };
            let mut size = 0u32;
            let mut count = 0u32;
            let r = PdhGetFormattedCounterArrayW(counter, fmt, &mut size, &mut count, std::ptr::null_mut());
            if r != PDH_MORE_DATA as u32 || size == 0 {
                return out;
            }
            let mut buf = vec![0u8; size as usize + 64];
            let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
            if PdhGetFormattedCounterArrayW(counter, fmt, &mut size, &mut count, items) != 0 {
                return out;
            }
            for i in 0..count as usize {
                let it = &*items.add(i);
                if it.FmtValue.CStatus != 0 && it.FmtValue.CStatus != 1 {
                    continue;
                }
                let v = if large { it.FmtValue.Anonymous.largeValue as f64 } else { it.FmtValue.Anonymous.doubleValue };
                out.push((from_pwstr(it.szName), v));
            }
        }
        out
    }

    pub fn gpu(&self) -> GpuSample {
        let mut s = GpuSample::default();
        // instance: pid_1234_luid_0x..._phys_0_eng_3_engtype_3D
        let mut per_pid_type: HashMap<(u32, String), f32> = HashMap::new();
        let mut per_type: HashMap<String, f32> = HashMap::new();
        for (name, v) in Self::array(self.gpu_engine, false) {
            let pid = name.strip_prefix("pid_").and_then(|r| r.split('_').next()).and_then(|p| p.parse::<u32>().ok()).unwrap_or(0);
            let ty = name.rsplit("engtype_").next().unwrap_or("").to_string();
            let v = v as f32;
            *per_pid_type.entry((pid, ty.clone())).or_default() += v;
            *per_type.entry(ty).or_default() += v;
        }
        for ((pid, _), v) in per_pid_type {
            let e = s.per_pid.entry(pid).or_default();
            *e = e.max(v.min(100.0));
        }
        let mut engines: Vec<(String, f32)> = per_type.into_iter().filter(|(k, _)| !k.is_empty()).map(|(k, v)| (k, v.min(100.0))).collect();
        engines.sort_by(|a, b| b.1.total_cmp(&a.1));
        s.util = engines.first().map(|e| e.1).unwrap_or(0.0);
        s.engines = engines;
        for (name, v) in Self::array(self.gpu_proc_mem, true) {
            if let Some(pid) = name.strip_prefix("pid_").and_then(|r| r.split('_').next()).and_then(|p| p.parse::<u32>().ok()) {
                *s.mem_per_pid.entry(pid).or_default() += v as u64;
            }
        }
        s.dedicated_used = Self::array(self.gpu_adapter_ded, true).iter().map(|x| x.1 as u64).max().unwrap_or(0);
        s.shared_used = Self::array(self.gpu_adapter_shared, true).iter().map(|x| x.1 as u64).max().unwrap_or(0);
        s
    }

    pub fn disks(&self) -> Vec<DiskSample> {
        let r = Self::array(self.disk_read, false);
        let w: HashMap<String, f64> = Self::array(self.disk_write, false).into_iter().collect();
        let idle: HashMap<String, f64> = Self::array(self.disk_idle, false).into_iter().collect();
        let mut out: Vec<DiskSample> = r
            .into_iter()
            .filter(|(n, _)| n != "_Total")
            .map(|(n, rv)| DiskSample {
                read: rv,
                write: *w.get(&n).unwrap_or(&0.0),
                active: (100.0 - *idle.get(&n).unwrap_or(&100.0)).clamp(0.0, 100.0) as f32,
                name: n,
                ..Default::default()
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }
}

impl Drop for Pdh {
    fn drop(&mut self) {
        unsafe { PdhCloseQuery(self.query) };
    }
}
