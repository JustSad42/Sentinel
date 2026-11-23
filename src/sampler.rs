//! Background sampler: one fast pass per tick over processes, CPU, memory, disks, GPU and network.
//! Also applies automation rules and records process history, even while the window is hidden.

use crate::config::{Priority, Rule, RuleAction, glob_match};
use crate::net::{self, Adapter, Conn, Proto};
use crate::pdh::{DiskSample, GpuSample, Pdh};
use crate::win::{self, NtQuerySystemInformation};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::c_void;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

pub type Key = (u32, i64);
pub const HIST: usize = 150;
const PROC_HIST: usize = 90;

#[derive(Clone)]
pub struct WinInfo {
    pub hwnd: isize,
    pub title: String,
    pub hung: bool,
}

#[derive(Clone, Default)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
    pub create_time: i64,
    pub threads: u32,
    pub handles: u32,
    pub session: u32,
    pub base_priority: i32,
    pub cpu: f32,
    pub cpu_time: u64,
    pub mem: u64,
    pub private_bytes: u64,
    pub working_set: u64,
    pub peak_working_set: u64,
    pub io_rate: f64,
    pub io_total: u64,
    pub gpu: f32,
    pub gpu_mem: u64,
    pub net_send: f64,
    pub net_recv: f64,
    pub net_total: u64,
    pub conns: u32,
    pub suspended: bool,
    pub window: Option<WinInfo>,
    pub path: Option<Arc<str>>,
}

impl Proc {
    pub fn key(&self) -> Key {
        (self.pid, self.create_time)
    }
    pub fn net_rate(&self) -> f64 {
        self.net_send + self.net_recv
    }
}

#[derive(Clone, Default)]
pub struct Snapshot {
    pub seq: u64,
    pub at: Option<Instant>,
    pub interval: f32,
    pub procs: Vec<Proc>,
    pub cpu_total: f32,
    pub cpu_kernel: f32,
    pub cores: Vec<f32>,
    pub mem_total: u64,
    pub mem_avail: u64,
    pub commit_total: u64,
    pub commit_limit: u64,
    pub cache: u64,
    pub paged_pool: u64,
    pub nonpaged_pool: u64,
    pub proc_count: u32,
    pub thread_count: u32,
    pub handle_count: u32,
    pub disks: Vec<DiskSample>,
    pub gpu: GpuSample,
    pub adapters: Vec<Adapter>,
    pub net_send: f64,
    pub net_recv: f64,
    pub conns: Vec<Conn>,
    pub per_app_net: bool,
    pub services_by_pid: Arc<HashMap<u32, Vec<(String, String)>>>,
    pub extra: crate::hw::Extra,
}

pub type Ring = VecDeque<f32>;

fn push(r: &mut Ring, v: f32) {
    if r.len() >= HIST {
        r.pop_front();
    }
    r.push_back(v);
}

#[derive(Default)]
pub struct SysHistory {
    pub cpu: Ring,
    pub cpu_kernel: Ring,
    pub cores: Vec<Ring>,
    pub mem: Ring,
    pub commit: Ring,
    pub gpu: Ring,
    pub gpu_mem: Ring,
    pub disk_active: HashMap<String, Ring>,
    pub disk_read: HashMap<String, Ring>,
    pub disk_write: HashMap<String, Ring>,
    pub net_send: Ring,
    pub net_recv: Ring,
    pub adapter_send: HashMap<String, Ring>,
    pub adapter_recv: HashMap<String, Ring>,
}

#[derive(Clone, Copy, Default)]
pub struct ProcPoint {
    pub cpu: f32,
    pub mem: u64,
    pub io: f64,
    pub net: f64,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct HistEntry {
    pub name: String,
    pub pid: u32,
    pub path: Option<String>,
    pub started: i64,
    pub ended: Option<i64>,
    pub peak_cpu: f32,
    pub peak_mem: u64,
    pub cpu_time: u64,
    pub io_total: u64,
    #[serde(default)]
    pub net_total: u64,
    #[serde(default)]
    pub create_time: i64,
}

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum LogKind {
    Rule,
    Action,
    Error,
    Info,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct LogEvent {
    pub time: i64,
    pub kind: LogKind,
    pub text: String,
}

/// One 10-second average for the 24-hour graphs.
#[derive(Clone, Copy, Default, Serialize, Deserialize)]
pub struct LongPoint {
    pub t: i64,
    pub cpu: f32,
    pub mem: f32,
    /// NaN when no GPU sample was taken in this bucket.
    pub gpu: f32,
    pub gpu_temp: f32,
    pub disk_r: f32,
    pub disk_w: f32,
    pub disk_active: f32,
    pub down: f32,
    pub up: f32,
}

pub const LONG_SECS: i64 = 10;
pub const LONG_KEEP: usize = 8640 + 360;
pub const WEEK_SECS: i64 = 300;
pub const WEEK_KEEP: usize = 2016 + 48;

#[derive(Default)]
struct LongAcc {
    n: u32,
    gpu_n: u32,
    temp_n: u32,
    sum: LongPoint,
    started: i64,
}

pub struct Shared {
    pub snap: Arc<Snapshot>,
    pub sys: SysHistory,
    pub long: VecDeque<LongPoint>,
    /// 5-minute averages for the 7-day graphs (built from `long`).
    pub week: VecDeque<LongPoint>,
    pub proc_hist: HashMap<Key, VecDeque<ProcPoint>>,
    /// Per-app one-minute averages for the last 24 h.
    pub apphist: crate::apphist::AppHist,
    pub history: Vec<HistEntry>,
    pub running_idx: HashMap<Key, usize>,
    pub log: VecDeque<LogEvent>,
    pub rules: Vec<Rule>,
    pub schedules: Vec<crate::config::Schedule>,
    /// Last run (unix) per schedule id; persisted in schedules-state.json.
    pub sched_last: HashMap<u64, i64>,
    /// "Run now" clicks waiting for the sampler.
    pub sched_run_now: Vec<u64>,
}

impl Control {
    /// Ask for a fresh sample now (wakes the sampler instead of it polling for this).
    pub fn refresh(&self) {
        self.refresh_now.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.get() {
            t.unpark();
        }
    }
}

impl Shared {
    pub fn log(&mut self, kind: LogKind, text: impl Into<String>) {
        if self.log.len() > 2000 {
            self.log.pop_front();
        }
        self.log.push_back(LogEvent { time: win::now_unix(), kind, text: text.into() });
    }
}

pub struct Control {
    /// The sampler thread, so a refresh request can wake it straight away.
    pub thread: std::sync::OnceLock<std::thread::Thread>,
    pub interval_ms: AtomicU64,
    pub visible: AtomicBool,
    pub refresh_now: AtomicBool,
    pub want_conns: AtomicBool,
    pub repaint: Mutex<Option<egui::Context>>,
    /// Set by the UI: the sampler warms names and icons for new processes even while hidden,
    /// so opening the window shows a finished list instead of rows filling in.
    pub info: Mutex<Option<Arc<crate::info::Info>>>,
    /// The mini overlay is showing (needs GPU numbers even while the window is hidden).
    pub overlay: AtomicBool,
}

// ---------------------------------------------------------------- NT structures

#[repr(C)]
struct SpiUnicode {
    length: u16,
    max_length: u16,
    buffer: *const u16,
}

#[repr(C)]
struct SystemProcessInformation {
    next_entry_offset: u32,
    number_of_threads: u32,
    working_set_private_size: i64,
    hard_fault_count: u32,
    number_of_threads_high_watermark: u32,
    cycle_time: u64,
    create_time: i64,
    user_time: i64,
    kernel_time: i64,
    image_name: SpiUnicode,
    base_priority: i32,
    unique_process_id: usize,
    inherited_from_unique_process_id: usize,
    handle_count: u32,
    session_id: u32,
    unique_process_key: usize,
    peak_virtual_size: usize,
    virtual_size: usize,
    page_fault_count: u32,
    peak_working_set_size: usize,
    working_set_size: usize,
    quota_peak_paged_pool_usage: usize,
    quota_paged_pool_usage: usize,
    quota_peak_non_paged_pool_usage: usize,
    quota_non_paged_pool_usage: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
    private_page_count: usize,
    read_operation_count: i64,
    write_operation_count: i64,
    other_operation_count: i64,
    read_transfer_count: i64,
    write_transfer_count: i64,
    other_transfer_count: i64,
}
const _: () = assert!(std::mem::size_of::<SystemProcessInformation>() == 256);

#[repr(C)]
struct SystemThreadInformation {
    kernel_time: i64,
    user_time: i64,
    create_time: i64,
    wait_time: u32,
    start_address: usize,
    client_pid: usize,
    client_tid: usize,
    priority: i32,
    base_priority: i32,
    context_switches: u32,
    thread_state: u32,
    wait_reason: u32,
}
const _: () = assert!(std::mem::size_of::<SystemThreadInformation>() == 80);

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ProcessorPerf {
    idle: i64,
    kernel: i64,
    user: i64,
    dpc: i64,
    interrupt: i64,
    interrupt_count: u32,
}

fn query_system(class: u32, buf: &mut Vec<u8>) -> bool {
    loop {
        let mut ret = 0u32;
        let st = unsafe { NtQuerySystemInformation(class, buf.as_mut_ptr() as *mut c_void, buf.len() as u32, &mut ret) };
        if st == 0 {
            return true;
        }
        if st as u32 == 0xC0000004 && buf.len() < 64 << 20 {
            let n = (ret as usize).max(buf.len()) + 128 * 1024;
            buf.resize(n, 0);
            continue;
        }
        return false;
    }
}

// ---------------------------------------------------------------- window enumeration

struct EnumCtx {
    out: HashMap<u32, WinInfo>,
}

unsafe extern "system" fn enum_cb(hwnd: HWND, lp: LPARAM) -> windows_sys::core::BOOL {
    unsafe {
        let ctx = &mut *(lp as *mut EnumCtx);
        if IsWindowVisible(hwnd) == 0 || !GetWindow(hwnd, GW_OWNER).is_null() {
            return 1;
        }
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        if ex & WS_EX_TOOLWINDOW != 0 {
            return 1;
        }
        let mut cloaked = 0u32;
        windows_sys::Win32::Graphics::Dwm::DwmGetWindowAttribute(hwnd, windows_sys::Win32::Graphics::Dwm::DWMWA_CLOAKED as u32, &mut cloaked as *mut _ as *mut c_void, 4);
        if cloaked != 0 {
            return 1;
        }
        let len = GetWindowTextLengthW(hwnd);
        if len == 0 {
            return 1;
        }
        let mut buf = vec![0u16; len as usize + 1];
        GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        let hung = IsHungAppWindow(hwnd) != 0;
        let title = win::from_wide(&buf);
        let e = ctx.out.entry(pid).or_insert(WinInfo { hwnd: hwnd as isize, title: title.clone(), hung });
        e.hung |= hung;
        1
    }
}

pub fn top_windows() -> HashMap<u32, WinInfo> {
    let mut ctx = EnumCtx { out: HashMap::new() };
    unsafe { EnumWindows(Some(enum_cb), &mut ctx as *mut _ as LPARAM) };
    ctx.out
}

// ---------------------------------------------------------------- sampler

struct Prev {
    cpu_time: u64,
    io_total: u64,
}

pub struct Sampler {
    pub shared: Arc<Mutex<Shared>>,
    pub ctl: Arc<Control>,
    buf: Vec<u8>,
    prev: HashMap<Key, Prev>,
    prev_cores: Vec<ProcessorPerf>,
    prev_at: Option<Instant>,
    prev_adapters: HashMap<u32, (u64, u64, u64, u64)>,
    hw: crate::hw::Dyn,
    long_acc: LongAcc,
    week_bucket: i64,
    on_battery: bool,
    boot: crate::boot::Tracker,
    pdh: Option<Pdh>,
    disk_io: Option<crate::disk::Disks>,
    paths: HashMap<Key, Option<Arc<str>>>,
    tick: u64,
    ncpu: usize,
    applied: HashSet<(Key, u64)>,
    over: HashMap<(Key, u64), f32>,
    net_totals: HashMap<Key, u64>,
    services: Arc<HashMap<u32, Vec<(String, String)>>>,
    addr_cache: Vec<Adapter>,
    self_pid: u32,
    etw_tried: bool,
}

pub fn critical(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "system" | "registry" | "smss.exe" | "csrss.exe" | "wininit.exe" | "services.exe" | "lsass.exe" | "winlogon.exe" | "memory compression" | "secure system" | "lsaiso.exe" | "system idle process"
    )
}

impl Sampler {
    pub fn spawn(shared: Arc<Mutex<Shared>>, ctl: Arc<Control>) {
        let mut s = Sampler {
            shared,
            ctl,
            buf: vec![0u8; 2 << 20],
            prev: HashMap::new(),
            prev_cores: Vec::new(),
            prev_at: None,
            prev_adapters: HashMap::new(),
            hw: Default::default(),
            long_acc: LongAcc::default(),
            week_bucket: 0,
            on_battery: false,
            boot: Default::default(),
            pdh: None,
            disk_io: None,
            paths: HashMap::new(),
            tick: 0,
            ncpu: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
            applied: HashSet::new(),
            over: HashMap::new(),
            net_totals: HashMap::new(),
            services: Arc::new(HashMap::new()),
            addr_cache: Vec::new(),
            self_pid: std::process::id(),
            etw_tried: false,
        };
        std::thread::Builder::new()
            .name("sampler".into())
            .spawn(move || {
                s.pdh = Pdh::new();
                s.disk_io = Some(crate::disk::Disks::new());
                let _ = s.ctl.thread.set(std::thread::current());
                let started = Instant::now();
                let mut hidden_since: Option<Instant> = None;
                let mut last_trim: Option<Instant> = None;
                loop {
                    let t0 = Instant::now();
                    s.sample();
                    // Return memory: once after startup settles, then 5 s after hiding and every
                    // 10 minutes while hidden.
                    let hidden = !s.ctl.visible.load(Ordering::Relaxed);
                    if hidden {
                        hidden_since.get_or_insert(t0);
                    } else {
                        hidden_since = None;
                    }
                    let due = match (last_trim, hidden_since) {
                        (None, _) => started.elapsed() >= Duration::from_secs(20),
                        (Some(l), Some(h)) => (h.elapsed() >= Duration::from_secs(5) && l < h) || l.elapsed() >= Duration::from_secs(600),
                        _ => false,
                    };
                    if due {
                        win::trim_working_set();
                        last_trim = Some(Instant::now());
                    }
                    // Hidden (and no overlay): every 2 s is plenty for rules, history and schedules.
                    let mut ms = s.ctl.interval_ms.load(Ordering::Relaxed).clamp(250, 10_000);
                    if !s.ctl.visible.load(Ordering::Relaxed) && !s.ctl.overlay.load(Ordering::Relaxed) {
                        ms = ms.max(2000);
                    }
                    let interval = Duration::from_millis(ms);
                    // Sleep until the next tick, or until the UI asks for a refresh (unpark).
                    loop {
                        if s.ctl.refresh_now.swap(false, Ordering::Relaxed) {
                            break;
                        }
                        let left = interval.saturating_sub(t0.elapsed());
                        if left.is_zero() {
                            break;
                        }
                        std::thread::park_timeout(left);
                    }
                }
            })
            .expect("sampler thread");
    }

    fn sample(&mut self) {
        let now = Instant::now();
        let elapsed = self.prev_at.map(|p| now.duration_since(p).as_secs_f64()).unwrap_or(1.0).max(0.05);
        self.prev_at = Some(now);
        self.tick += 1;
        let visible = self.ctl.visible.load(Ordering::Relaxed);
        let mut prof = Prof::start();
        if visible && !self.etw_tried {
            self.etw_tried = true;
            if win::is_elevated() {
                net::start_etw();
            }
        }

        // --- processes
        let mut procs: Vec<Proc> = Vec::with_capacity(400);
        let mut thread_count = 0u32;
        let mut handle_count = 0u32;
        let got = query_system(5, &mut self.buf);
        prof.mark("ntquery");
        if got {
            let mut off = 0usize;
            loop {
                let p = unsafe { &*(self.buf.as_ptr().add(off) as *const SystemProcessInformation) };
                let pid = p.unique_process_id as u32;
                thread_count += p.number_of_threads;
                handle_count += p.handle_count;
                if pid != 0 {
                    let name = if p.image_name.buffer.is_null() {
                        if pid == 4 { "System".to_string() } else { String::new() }
                    } else {
                        unsafe { String::from_utf16_lossy(std::slice::from_raw_parts(p.image_name.buffer, p.image_name.length as usize / 2)) }
                    };
                    // Suspended: every thread waiting with reason Suspended (5).
                    let mut suspended = p.number_of_threads > 0;
                    for i in 0..p.number_of_threads as usize {
                        let t = unsafe { &*(self.buf.as_ptr().add(off + 256 + i * 80) as *const SystemThreadInformation) };
                        if !(t.thread_state == 5 && t.wait_reason == 5) {
                            suspended = false;
                            break;
                        }
                    }
                    let cpu_time = (p.kernel_time + p.user_time) as u64;
                    let io_total = (p.read_transfer_count + p.write_transfer_count + p.other_transfer_count) as u64;
                    procs.push(Proc {
                        pid,
                        ppid: p.inherited_from_unique_process_id as u32,
                        name,
                        create_time: p.create_time,
                        threads: p.number_of_threads,
                        handles: p.handle_count,
                        session: p.session_id,
                        base_priority: p.base_priority,
                        cpu_time,
                        mem: p.working_set_private_size.max(0) as u64,
                        private_bytes: p.private_page_count as u64,
                        working_set: p.working_set_size as u64,
                        peak_working_set: p.peak_working_set_size as u64,
                        io_total,
                        suspended,
                        ..Default::default()
                    });
                }
                if p.next_entry_offset == 0 {
                    break;
                }
                off += p.next_entry_offset as usize;
            }
        }
        prof.mark("procs");
        let proc_count = procs.len() as u32;

        // --- per-core CPU
        let mut cores_buf = vec![0u8; std::mem::size_of::<ProcessorPerf>() * 256];
        let mut cores = Vec::new();
        let (mut tot_busy, mut tot_all, mut tot_kern) = (0f64, 0f64, 0f64);
        let (mut tot_intr, mut tot_dpc, mut tot_irq) = (0u32, 0f64, 0f64);
        if query_system(8, &mut cores_buf) {
            let n = self.ncpu.min(256);
            let cur: Vec<ProcessorPerf> = (0..n).map(|i| unsafe { *(cores_buf.as_ptr() as *const ProcessorPerf).add(i) }).collect();
            if self.prev_cores.len() == cur.len() {
                for (a, b) in cur.iter().zip(&self.prev_cores) {
                    let idle = (a.idle - b.idle) as f64;
                    let kern = (a.kernel - b.kernel) as f64;
                    let user = (a.user - b.user) as f64;
                    let all = kern + user;
                    let busy = (all - idle).max(0.0);
                    cores.push(if all > 0.0 { (busy / all * 100.0) as f32 } else { 0.0 });
                    tot_busy += busy;
                    tot_all += all;
                    tot_kern += (kern - idle).max(0.0);
                    tot_intr = tot_intr.wrapping_add(a.interrupt_count.wrapping_sub(b.interrupt_count));
                    tot_dpc += (a.dpc - b.dpc).max(0) as f64;
                    tot_irq += (a.interrupt - b.interrupt).max(0) as f64;
                }
            } else {
                cores = vec![0.0; cur.len()];
            }
            self.prev_cores = cur;
        }
        let cpu_total = if tot_all > 0.0 { (tot_busy / tot_all * 100.0) as f32 } else { 0.0 };
        prof.mark("cores");
        let cpu_kernel = if tot_all > 0.0 { (tot_kern / tot_all * 100.0) as f32 } else { 0.0 };

        // --- per-process rates
        let denom = elapsed * 10_000_000.0 * self.ncpu as f64;
        let mut seen: HashSet<Key> = HashSet::with_capacity(procs.len());
        for p in procs.iter_mut() {
            let k = p.key();
            seen.insert(k);
            if let Some(pr) = self.prev.get(&k) {
                p.cpu = ((p.cpu_time.saturating_sub(pr.cpu_time)) as f64 / denom * 100.0).min(100.0) as f32;
                p.io_rate = p.io_total.saturating_sub(pr.io_total) as f64 / elapsed;
            }
            self.prev.insert(k, Prev { cpu_time: p.cpu_time, io_total: p.io_total });
            let path = self.paths.entry(k).or_insert_with(|| win::process_path(k.0).map(Arc::from));
            p.path = path.clone();
        }
        self.prev.retain(|k, _| seen.contains(k));
        self.paths.retain(|k, _| seen.contains(k));
        // Scale so the per-process sum matches the system total (sampling skew).
        let _ = cpu_total;

        prof.mark("rates");
        // --- windows (apps, hung detection)
        let wins = top_windows();
        for p in procs.iter_mut() {
            if let Some(w) = wins.get(&p.pid) {
                p.window = Some(w.clone());
            }
        }

        prof.mark("windows");
        // --- memory
        let (mut mem_total, mut mem_avail, mut commit_total, mut commit_limit, mut cache, mut paged, mut nonpaged) = (0, 0, 0, 0, 0, 0, 0);
        unsafe {
            use windows_sys::Win32::System::ProcessStatus::*;
            use windows_sys::Win32::System::SystemInformation::*;
            let mut ms: MEMORYSTATUSEX = std::mem::zeroed();
            ms.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
            if GlobalMemoryStatusEx(&mut ms) != 0 {
                mem_total = ms.ullTotalPhys;
                mem_avail = ms.ullAvailPhys;
            }
            let mut pi: PERFORMANCE_INFORMATION = std::mem::zeroed();
            pi.cb = std::mem::size_of::<PERFORMANCE_INFORMATION>() as u32;
            if GetPerformanceInfo(&mut pi, pi.cb) != 0 {
                let pg = pi.PageSize as u64;
                commit_total = pi.CommitTotal as u64 * pg;
                commit_limit = pi.CommitLimit as u64 * pg;
                cache = pi.SystemCache as u64 * pg;
                paged = pi.KernelPaged as u64 * pg;
                nonpaged = pi.KernelNonpaged as u64 * pg;
            }
        }

        prof.mark("memory");
        // --- GPU & disks (counters are comparatively expensive: only while visible)
        let (mut gpu, mut disks) = (GpuSample::default(), Vec::new());
        let overlay = self.ctl.overlay.load(Ordering::Relaxed);
        let long_tick = self.tick % 10 == 0;
        if visible || overlay || long_tick {
            if let Some(pdh) = self.pdh.as_mut() {
                if pdh.collect() {
                    gpu = pdh.gpu();
                }
            }
            for p in procs.iter_mut() {
                p.gpu = *gpu.per_pid.get(&p.pid).unwrap_or(&0.0);
                p.gpu_mem = *gpu.mem_per_pid.get(&p.pid).unwrap_or(&0);
            }
        }

        prof.mark("gpu");
        if let Some(d) = self.disk_io.as_mut() {
            disks = d.sample();
        }

        prof.mark("disks");
        // --- network
        let mut adapters = net::adapters();
        if self.tick % 10 == 1 || self.addr_cache.len() != adapters.len() {
            let mut a = adapters.clone();
            net::fill_addresses(&mut a);
            self.addr_cache = a;
        }
        let (mut net_send, mut net_recv) = (0.0, 0.0);
        let mut seen_if = HashMap::new();
        for a in adapters.iter_mut() {
            if let Some(c) = self.addr_cache.iter().find(|c| c.index == a.index) {
                a.ipv4 = c.ipv4.clone();
                a.ipv6 = c.ipv6.clone();
                a.gateways = c.gateways.clone();
                a.dns = c.dns.clone();
            }
            if let Some(&(pi, po, pki, pko)) = self.prev_adapters.get(&a.index) {
                a.recv_rate = a.in_octets.saturating_sub(pi) as f64 / elapsed;
                a.send_rate = a.out_octets.saturating_sub(po) as f64 / elapsed;
                a.in_pps = a.in_pkts.saturating_sub(pki) as f64 / elapsed;
                a.out_pps = a.out_pkts.saturating_sub(pko) as f64 / elapsed;
            }
            net_send += a.send_rate;
            net_recv += a.recv_rate;
            seen_if.insert(a.index, (a.in_octets, a.out_octets, a.in_pkts, a.out_pkts));
        }
        self.prev_adapters = seen_if;

        prof.mark("net");
        let etw = net::etw_drain();
        let per_app_net = net::etw_running();
        // Socket tables are the most expensive read here: only when something on screen shows them.
        let want_conns = visible && self.ctl.want_conns.load(Ordering::Relaxed);
        let mut conns = if visible && (want_conns || per_app_net) { net::connections() } else { Vec::new() };
        if !conns.is_empty() {
            let mut counts: HashMap<u32, u32> = HashMap::new();
            for c in &conns {
                if c.proto == Proto::Tcp && c.state != 2 {
                    *counts.entry(c.pid).or_default() += 1;
                }
            }
            for p in procs.iter_mut() {
                p.conns = *counts.get(&p.pid).unwrap_or(&0);
            }
        }
        if per_app_net {
            let by_pid: HashMap<u32, Key> = procs.iter().map(|p| (p.pid, p.key())).collect();
            for (pid, (s, r)) in &etw.per_pid {
                if let Some(k) = by_pid.get(pid) {
                    *self.net_totals.entry(*k).or_default() += s + r;
                }
            }
            for p in procs.iter_mut() {
                if let Some((s, r)) = etw.per_pid.get(&p.pid) {
                    p.net_send = *s as f64 / elapsed;
                    p.net_recv = *r as f64 / elapsed;
                }
                p.net_total = *self.net_totals.get(&p.key()).unwrap_or(&0);
            }
            self.net_totals.retain(|k, _| seen.contains(k));
            // Attribute per-connection bytes; UDP sockets have no remote, match on local port.
            let mut conn_map: HashMap<(u32, Proto, SocketAddr, Option<SocketAddr>), (u64, u64)> = HashMap::new();
            for ((pid, proto, l, r), v) in etw.per_conn {
                let key = if proto == Proto::Udp { (pid, proto, l, None) } else { (pid, proto, l, Some(r)) };
                let e = conn_map.entry(key).or_default();
                e.0 += v.0;
                e.1 += v.1;
            }
            for c in conns.iter_mut() {
                let k = (c.pid, c.proto, c.local, c.remote);
                let hit = conn_map.get(&k).copied().or_else(|| {
                    // Wildcard-bound sockets: match by port only.
                    conn_map.iter().find(|((pid, pr, l, r), _)| *pid == c.pid && *pr == c.proto && l.port() == c.local.port() && (c.proto == Proto::Udp || *r == c.remote)).map(|(_, v)| *v)
                });
                if let Some((s, r)) = hit {
                    c.send_rate = s as f64 / elapsed;
                    c.recv_rate = r as f64 / elapsed;
                }
            }
        }

        prof.mark("conns");
        // --- services (pid -> names), refreshed every few seconds while visible
        if visible && (self.tick % 8 == 1 || self.services.is_empty()) {
            let mut m: HashMap<u32, Vec<(String, String)>> = HashMap::new();
            for s in crate::services::list_basic() {
                if s.pid != 0 {
                    m.entry(s.pid).or_default().push((s.name, s.display));
                }
            }
            self.services = Arc::new(m);
        }

        prof.mark("services");
        // --- rules
        self.apply_rules(&procs, elapsed as f32);

        prof.mark("rules");
        // --- deeper kernel / hardware counters
        let extra = self.hw.sample(elapsed, visible || overlay || long_tick, self.tick, (tot_intr, tot_dpc, tot_irq, tot_all));

        prof.mark("hw");
        if let Some((_, ac)) = extra.battery {
            self.on_battery = !ac;
        }
        self.apply_schedules(&procs);

        prof.mark("sched");
        // --- boot timeline / startup impact (first minutes of the session only)
        self.boot.sample(&procs, cpu_total);

        prof.mark("boot");
        // --- warm metadata + icons
        let info = self.ctl.info.lock().unwrap().clone();
        if let Some(info) = info {
            let by_pid: HashMap<u32, &Proc> = procs.iter().map(|p| (p.pid, p)).collect();
            for p in &procs {
                info.warm(p.key(), p.path.as_ref(), || {
                    let mut out = Vec::new();
                    let mut cur = p;
                    for _ in 0..6 {
                        match by_pid.get(&cur.ppid) {
                            // Parent must be older than the child, or the PID was reused.
                            Some(par) if par.pid != cur.pid && par.create_time <= cur.create_time => {
                                if let Some(path) = &par.path {
                                    out.push(path.clone());
                                }
                                cur = par;
                            }
                            _ => break,
                        }
                    }
                    out
                });
                if let Some(m) = info.meta(p.key(), p.path.as_ref()) {
                    if let Some(k) = &m.icon_key {
                        let _ = info.icon(k);
                    }
                }
            }
            if self.tick % 300 == 7 {
                let live: HashSet<Key> = procs.iter().map(|p| p.key()).collect();
                info.prune(&live);
            }
        }

        prof.mark("warm");
        // --- publish
        let mut sh = self.shared.lock().unwrap();
        // history
        let now_unix = win::now_unix();
        for p in &procs {
            let k = p.key();
            let idx = match sh.running_idx.get(&k) {
                Some(&i) => i,
                None => {
                    let i = sh.history.len();
                    sh.history.push(HistEntry {
                        name: p.name.clone(),
                        pid: p.pid,
                        path: p.path.as_ref().map(|s| s.to_string()),
                        started: win::filetime_to_unix(p.create_time),
                        ended: None,
                        peak_cpu: 0.0,
                        peak_mem: 0,
                        cpu_time: 0,
                        io_total: 0,
                        net_total: 0,
                        create_time: p.create_time,
                    });
                    sh.running_idx.insert(k, i);
                    i
                }
            };
            let e = &mut sh.history[idx];
            e.peak_cpu = e.peak_cpu.max(p.cpu);
            e.peak_mem = e.peak_mem.max(p.mem);
            e.cpu_time = p.cpu_time;
            e.io_total = p.io_total;
            e.net_total = p.net_total;
        }
        let gone: Vec<Key> = sh.running_idx.keys().filter(|k| !seen.contains(k)).copied().collect();
        for k in gone {
            if let Some(i) = sh.running_idx.remove(&k) {
                sh.history[i].ended = Some(now_unix);
            }
        }
        // per-process sparkline history
        for p in &procs {
            let h = sh.proc_hist.entry(p.key()).or_default();
            if h.len() >= PROC_HIST {
                h.pop_front();
            }
            h.push_back(ProcPoint { cpu: p.cpu, mem: p.mem, io: p.io_rate, net: p.net_rate() });
        }
        sh.proc_hist.retain(|k, _| seen.contains(k));
        sh.apphist.sample(&procs, now_unix);
        // 24-hour history: 10-second averages
        {
            let now_u = now_unix;
            let a = &mut self.long_acc;
            if a.n == 0 {
                a.started = now_u;
            }
            let used = mem_total.saturating_sub(mem_avail);
            a.n += 1;
            a.sum.cpu += cpu_total;
            a.sum.mem += if mem_total > 0 { used as f32 / mem_total as f32 * 100.0 } else { 0.0 };
            if visible || overlay || long_tick {
                a.gpu_n += 1;
                a.sum.gpu += gpu.util;
            }
            if let Some(t) = extra.gpu.temperature {
                a.temp_n += 1;
                a.sum.gpu_temp += t;
            }
            a.sum.disk_r += disks.iter().map(|d| d.read as f32).sum::<f32>();
            a.sum.disk_w += disks.iter().map(|d| d.write as f32).sum::<f32>();
            a.sum.disk_active += disks.iter().map(|d| d.active).fold(0.0, f32::max);
            a.sum.down += net_recv as f32;
            a.sum.up += net_send as f32;
            if now_u - a.started >= LONG_SECS {
                let n = a.n as f32;
                let p = LongPoint {
                    t: now_u,
                    cpu: a.sum.cpu / n,
                    mem: a.sum.mem / n,
                    gpu: if a.gpu_n > 0 { a.sum.gpu / a.gpu_n as f32 } else { f32::NAN },
                    gpu_temp: if a.temp_n > 0 { a.sum.gpu_temp / a.temp_n as f32 } else { f32::NAN },
                    disk_r: a.sum.disk_r / n,
                    disk_w: a.sum.disk_w / n,
                    disk_active: a.sum.disk_active / n,
                    down: a.sum.down / n,
                    up: a.sum.up / n,
                };
                if sh.long.len() >= LONG_KEEP {
                    sh.long.pop_front();
                }
                sh.long.push_back(p);
                *a = LongAcc::default();
                // Every 5 minutes, fold the finished bucket of 10-second points into the 7-day series.
                let b = p.t / WEEK_SECS;
                if self.week_bucket != 0 && b != self.week_bucket {
                    let prev = self.week_bucket;
                    let pts: Vec<LongPoint> = sh.long.iter().rev().skip(1).take_while(|q| q.t / WEEK_SECS >= prev).filter(|q| q.t / WEEK_SECS == prev).copied().collect();
                    if let Some(w) = average(&pts, (prev + 1) * WEEK_SECS) {
                        if sh.week.back().is_none_or(|l| l.t < w.t) {
                            if sh.week.len() >= WEEK_KEEP {
                                sh.week.pop_front();
                            }
                            sh.week.push_back(w);
                        }
                    }
                }
                self.week_bucket = b;
            }
        }
        // system history
        {
            let s = &mut sh.sys;
            push(&mut s.cpu, cpu_total);
            push(&mut s.cpu_kernel, cpu_kernel);
            if s.cores.len() != cores.len() {
                s.cores = vec![Ring::new(); cores.len()];
            }
            for (r, v) in s.cores.iter_mut().zip(&cores) {
                push(r, *v);
            }
            let used = mem_total.saturating_sub(mem_avail);
            push(&mut s.mem, if mem_total > 0 { used as f32 / mem_total as f32 * 100.0 } else { 0.0 });
            push(&mut s.commit, if commit_limit > 0 { commit_total as f32 / commit_limit as f32 * 100.0 } else { 0.0 });
            if visible {
                push(&mut s.gpu, gpu.util);
                push(&mut s.gpu_mem, gpu.dedicated_used as f32);
                for d in &disks {
                    push(s.disk_active.entry(d.name.clone()).or_default(), d.active);
                    push(s.disk_read.entry(d.name.clone()).or_default(), d.read as f32);
                    push(s.disk_write.entry(d.name.clone()).or_default(), d.write as f32);
                }
            }
            push(&mut s.net_send, net_send as f32);
            push(&mut s.net_recv, net_recv as f32);
            for a in &adapters {
                push(s.adapter_send.entry(a.alias.clone()).or_default(), a.send_rate as f32);
                push(s.adapter_recv.entry(a.alias.clone()).or_default(), a.recv_rate as f32);
            }
        }
        let seq = sh.snap.seq + 1;
        sh.snap = Arc::new(Snapshot {
            seq,
            at: Some(now),
            interval: elapsed as f32,
            procs,
            cpu_total,
            cpu_kernel,
            cores,
            mem_total,
            mem_avail,
            commit_total,
            commit_limit,
            cache,
            paged_pool: paged,
            nonpaged_pool: nonpaged,
            proc_count,
            thread_count,
            handle_count,
            disks,
            gpu,
            adapters,
            net_send,
            net_recv,
            conns,
            per_app_net,
            services_by_pid: self.services.clone(),
            extra,
        });
        drop(sh);
        prof.mark("publish");
        prof.finish(visible);
        if visible {
            if let Some(ctx) = self.ctl.repaint.lock().unwrap().as_ref() {
                ctx.request_repaint();
            }
        }
    }

    fn apply_schedules(&mut self, procs: &[Proc]) {
        use crate::config::{SchedAction, When};
        let (scheds, run_now, last) = {
            let mut sh = self.shared.lock().unwrap();
            if sh.schedules.is_empty() {
                return;
            }
            (sh.schedules.clone(), std::mem::take(&mut sh.sched_run_now), sh.sched_last.clone())
        };
        let now = win::now_unix();
        let (_, _, _, h, mi, s) = crate::fmt::civil(now);
        let midnight = now - (h as i64 * 3600 + mi as i64 * 60 + s as i64);
        let mut logs: Vec<(LogKind, String)> = Vec::new();
        let mut ran: Vec<(u64, i64)> = Vec::new();
        for sc in &scheds {
            let forced = run_now.contains(&sc.id);
            if !sc.enabled && !forced {
                continue;
            }
            let prev = last.get(&sc.id).copied();
            let due = forced
                || match sc.when {
                    When::Daily { hour, minute } => {
                        let t = midnight + hour as i64 * 3600 + minute as i64 * 60;
                        now >= t && now < t + 600 && prev.is_none_or(|l| l < t)
                    }
                    When::Every { hours } => match prev {
                        // First time we see it: start the clock rather than firing right away.
                        None => {
                            ran.push((sc.id, now));
                            false
                        }
                        Some(l) => now - l >= hours.max(1) as i64 * 3600,
                    },
                    When::OnBattery => self.on_battery && prev.is_none_or(|l| now - l >= 30),
                };
            if !due {
                continue;
            }
            ran.push((sc.id, now));
            let quiet_if_idle = sc.when == When::OnBattery && !forced;
            let pat = sc.target.trim();
            let matching: Vec<&Proc> = if sc.action == SchedAction::RestartExplorer {
                procs.iter().filter(|p| p.name.eq_ignore_ascii_case("explorer.exe")).collect()
            } else if pat.is_empty() {
                Vec::new()
            } else {
                procs.iter().filter(|p| p.pid > 4 && !critical(&p.name) && glob_match(pat, &p.name)).collect()
            };
            let pids: HashSet<u32> = matching.iter().map(|p| p.pid).collect();
            let roots: Vec<&Proc> = matching.iter().copied().filter(|p| !pids.contains(&p.ppid)).collect();
            let what = format!("Scheduled ({})", sc.action.label().to_lowercase());
            match sc.action {
                SchedAction::End => {
                    if matching.is_empty() {
                        if !quiet_if_idle {
                            logs.push((LogKind::Rule, format!("{what}: {pat} wasn't running")));
                        }
                        continue;
                    }
                    let targets: Vec<(u32, i64)> = matching.iter().map(|p| p.key()).collect();
                    let (ok, errs) = crate::actions::kill_many(&targets);
                    logs.push((if errs.is_empty() { LogKind::Rule } else { LogKind::Error }, format!("{what}: ended {ok} {} process{}{}", matching[0].name, if ok == 1 { "" } else { "es" }, errs.first().map(|e| format!(" ({e})")).unwrap_or_default())));
                }
                SchedAction::Restart | SchedAction::RestartExplorer => {
                    if roots.is_empty() {
                        logs.push((LogKind::Rule, format!("{what}: {} wasn't running", if pat.is_empty() { "explorer.exe" } else { pat })));
                        continue;
                    }
                    for p in roots.iter().take(4) {
                        match crate::actions::restart(p) {
                            Ok(()) => logs.push((LogKind::Rule, format!("{what}: restarted {}", p.name))),
                            Err(e) => logs.push((LogKind::Error, format!("{what}: couldn't restart {}: {e}", p.name))),
                        }
                    }
                }
                SchedAction::Start => {
                    let file = std::path::Path::new(pat.trim_matches('"')).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
                    if !file.is_empty() && procs.iter().any(|p| p.name.eq_ignore_ascii_case(&file)) {
                        if forced {
                            logs.push((LogKind::Rule, format!("{what}: {file} is already running")));
                        }
                        continue;
                    }
                    let (exe, args) = crate::actions::split_args(pat);
                    match win::launch_as_user(&exe, &args, None) {
                        Ok(()) => logs.push((LogKind::Rule, format!("{what}: started {file}"))),
                        Err(e) => logs.push((LogKind::Error, format!("{what}: couldn't start {file}: {e}"))),
                    }
                }
                SchedAction::ClearStandby => match crate::hw::purge_standby() {
                    Ok(freed) => logs.push((LogKind::Rule, format!("{what}: freed {}", crate::fmt::bytes(freed)))),
                    Err(e) => logs.push((LogKind::Error, format!("{what}: {e}"))),
                },
            }
        }
        if ran.is_empty() && logs.is_empty() {
            return;
        }
        let mut sh = self.shared.lock().unwrap();
        for (id, t) in ran {
            sh.sched_last.insert(id, t);
        }
        for (k, t) in logs {
            sh.log(k, t);
        }
        save_sched_last(&sh.sched_last);
    }

    fn apply_rules(&mut self, procs: &[Proc], dt: f32) {
        let rules: Vec<Rule> = self.shared.lock().unwrap().rules.iter().filter(|r| r.enabled && !r.pattern.trim().is_empty()).cloned().collect();
        if rules.is_empty() {
            return;
        }
        let mut logs: Vec<(LogKind, String)> = Vec::new();
        for p in procs {
            if p.pid == self.self_pid || p.pid <= 4 || critical(&p.name) {
                continue;
            }
            for r in &rules {
                if !glob_match(r.pattern.trim(), &p.name) {
                    continue;
                }
                let k = (p.key(), r.id);
                match &r.action {
                    RuleAction::SetPriority(pr) => {
                        if self.applied.insert(k) {
                            match crate::actions::set_priority(p.pid, *pr) {
                                Ok(()) => logs.push((LogKind::Rule, format!("Set {} (PID {}) to {} priority", p.name, p.pid, pr.label()))),
                                Err(e) => logs.push((LogKind::Error, format!("Rule could not set priority of {}: {e}", p.name))),
                            }
                        }
                    }
                    RuleAction::SetAffinity(mask) => {
                        if self.applied.insert(k) {
                            match crate::actions::set_affinity(p.pid, *mask) {
                                Ok(()) => logs.push((LogKind::Rule, format!("Limited {} (PID {}) to {} cores", p.name, p.pid, mask.count_ones()))),
                                Err(e) => logs.push((LogKind::Error, format!("Rule could not set cores of {}: {e}", p.name))),
                            }
                        }
                    }
                    RuleAction::Block => {
                        if self.applied.insert(k) {
                            match crate::actions::kill(p.pid, p.create_time) {
                                Ok(()) => logs.push((LogKind::Rule, format!("Blocked {} (PID {})", p.name, p.pid))),
                                Err(e) => logs.push((LogKind::Error, format!("Rule could not block {}: {e}", p.name))),
                            }
                        }
                    }
                    RuleAction::KillAboveMemoryMb(mb) => {
                        if p.mem > mb * 1024 * 1024 && self.applied.insert(k) {
                            match crate::actions::kill(p.pid, p.create_time) {
                                Ok(()) => logs.push((LogKind::Rule, format!("Ended {} (PID {}): memory {} over {} MB", p.name, p.pid, crate::fmt::bytes(p.mem), mb))),
                                Err(e) => logs.push((LogKind::Error, format!("Rule could not end {}: {e}", p.name))),
                            }
                        }
                    }
                    RuleAction::KillAboveCpu { percent, seconds } | RuleAction::LowerPriorityAboveCpu { percent, seconds } => {
                        let t = self.over.entry(k).or_default();
                        if p.cpu >= *percent { *t += dt } else { *t = 0.0 }
                        if *t >= *seconds as f32 && self.applied.insert(k) {
                            if matches!(r.action, RuleAction::KillAboveCpu { .. }) {
                                match crate::actions::kill(p.pid, p.create_time) {
                                    Ok(()) => logs.push((LogKind::Rule, format!("Ended {} (PID {}): CPU above {percent:.0}% for {seconds}s", p.name, p.pid))),
                                    Err(e) => logs.push((LogKind::Error, format!("Rule could not end {}: {e}", p.name))),
                                }
                            } else {
                                match crate::actions::set_priority(p.pid, Priority::Idle) {
                                    Ok(()) => logs.push((LogKind::Rule, format!("Throttled {} (PID {}) to low priority: CPU above {percent:.0}% for {seconds}s", p.name, p.pid))),
                                    Err(e) => logs.push((LogKind::Error, format!("Rule could not throttle {}: {e}", p.name))),
                                }
                            }
                        }
                    }
                }
            }
        }
        let live: HashSet<Key> = procs.iter().map(|p| p.key()).collect();
        self.applied.retain(|(k, _)| live.contains(k));
        self.over.retain(|(k, _), _| live.contains(k));
        if !logs.is_empty() {
            let mut sh = self.shared.lock().unwrap();
            for (kind, t) in logs {
                sh.log(kind, t);
            }
        }
    }
}

// ---------------------------------------------------------------- history persistence

pub fn load_history() -> (Vec<HistEntry>, VecDeque<LogEvent>) {
    let dir = crate::config::data_dir();
    let mut h: Vec<HistEntry> = std::fs::read(dir.join("history.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let now = win::now_unix();
    for e in h.iter_mut() {
        if e.ended.is_none() {
            e.ended = Some(now);
        }
    }
    let log: VecDeque<LogEvent> = std::fs::read(dir.join("activity.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    (h, log)
}

pub fn load_sched_last() -> HashMap<u64, i64> {
    std::fs::read(crate::config::data_dir().join("schedules-state.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_sched_last(m: &HashMap<u64, i64>) {
    if let Ok(b) = serde_json::to_vec(m) {
        let _ = std::fs::write(crate::config::data_dir().join("schedules-state.json"), b);
    }
}

/// Mean of a set of points (GPU fields ignore missing samples); None if empty.
pub fn average(pts: &[LongPoint], t: i64) -> Option<LongPoint> {
    if pts.is_empty() {
        return None;
    }
    let n = pts.len() as f32;
    let nan_mean = |f: &dyn Fn(&LongPoint) -> f32| {
        let v: Vec<f32> = pts.iter().map(f).filter(|v| v.is_finite()).collect();
        if v.is_empty() { f32::NAN } else { v.iter().sum::<f32>() / v.len() as f32 }
    };
    Some(LongPoint {
        t,
        cpu: pts.iter().map(|p| p.cpu).sum::<f32>() / n,
        mem: pts.iter().map(|p| p.mem).sum::<f32>() / n,
        gpu: nan_mean(&|p| p.gpu),
        gpu_temp: nan_mean(&|p| p.gpu_temp),
        disk_r: pts.iter().map(|p| p.disk_r).sum::<f32>() / n,
        disk_w: pts.iter().map(|p| p.disk_w).sum::<f32>() / n,
        disk_active: pts.iter().map(|p| p.disk_active).sum::<f32>() / n,
        down: pts.iter().map(|p| p.down).sum::<f32>() / n,
        up: pts.iter().map(|p| p.up).sum::<f32>() / n,
    })
}

fn load_points(file: &str, keep_secs: i64) -> VecDeque<LongPoint> {
    let cutoff = win::now_unix() - keep_secs - 3600;
    let v: Vec<LongPoint> = std::fs::read(crate::config::data_dir().join(file)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    v.into_iter().filter(|p| p.t >= cutoff).map(|p| LongPoint { gpu: if p.gpu < 0.0 { f32::NAN } else { p.gpu }, gpu_temp: if p.gpu_temp < 0.0 { f32::NAN } else { p.gpu_temp }, ..p }).collect()
}

fn save_points(file: &str, pts: &VecDeque<LongPoint>) {
    // NaN isn't valid JSON: store missing GPU samples as -1.
    let v: Vec<LongPoint> = pts.iter().map(|p| LongPoint { gpu: if p.gpu.is_nan() { -1.0 } else { p.gpu }, gpu_temp: if p.gpu_temp.is_nan() { -1.0 } else { p.gpu_temp }, ..*p }).collect();
    if let Ok(b) = serde_json::to_vec(&v) {
        let _ = std::fs::write(crate::config::data_dir().join(file), b);
    }
}

pub fn load_long() -> VecDeque<LongPoint> {
    load_points("long-history.json", 86400)
}

/// 7-day series; on first run after updating, seeded from the 24-hour points so it isn't empty.
pub fn load_week(long: &VecDeque<LongPoint>) -> VecDeque<LongPoint> {
    let mut w = load_points("week-history.json", 7 * 86400);
    if w.is_empty() {
        let mut bucket: Vec<LongPoint> = Vec::new();
        let mut cur = 0;
        for p in long {
            let b = p.t / WEEK_SECS;
            if b != cur && !bucket.is_empty() {
                w.extend(average(&bucket, (cur + 1) * WEEK_SECS));
                bucket.clear();
            }
            cur = b;
            bucket.push(*p);
        }
    }
    w
}

pub fn save_history(sh: &Shared, keep_days: u32) {
    sh.apphist.save();
    save_points("long-history.json", &sh.long);
    save_points("week-history.json", &sh.week);
    let cutoff = win::now_unix() - keep_days.max(1) as i64 * 86400;
    let mut list: Vec<&HistEntry> = sh.history.iter().filter(|e| e.ended.unwrap_or(i64::MAX) >= cutoff).collect();
    if list.len() > 30_000 {
        let n = list.len() - 30_000;
        list.drain(..n);
    }
    let dir = crate::config::data_dir();
    if let Ok(b) = serde_json::to_vec(&list) {
        let _ = std::fs::write(dir.join("history.json"), b);
    }
    let log: Vec<&LogEvent> = sh.log.iter().filter(|e| e.time >= cutoff).collect();
    if let Ok(b) = serde_json::to_vec(&log) {
        let _ = std::fs::write(dir.join("activity.json"), b);
    }
}

/// Per-stage sampler timings, written to profile.log every 30 ticks when SENTINEL_PROFILE is set.
struct Prof {
    on: bool,
    last: Instant,
}

static PROF_ACC: Mutex<(u32, Vec<(&'static str, f64, f64)>)> = Mutex::new((0, Vec::new()));

impl Prof {
    fn start() -> Self {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        Prof { on: *ON.get_or_init(|| std::env::var_os("SENTINEL_PROFILE").is_some()), last: Instant::now() }
    }
    fn mark(&mut self, name: &'static str) {
        if !self.on {
            return;
        }
        let ms = self.last.elapsed().as_secs_f64() * 1000.0;
        self.last = Instant::now();
        let mut a = PROF_ACC.lock().unwrap();
        match a.1.iter_mut().find(|(n, _, _)| *n == name) {
            Some(e) => {
                e.1 += ms;
                e.2 = e.2.max(ms);
            }
            None => a.1.push((name, ms, ms)),
        }
    }
    fn finish(&mut self, visible: bool) {
        if !self.on {
            return;
        }
        let mut a = PROF_ACC.lock().unwrap();
        a.0 += 1;
        if a.0 < 30 {
            return;
        }
        let n = a.0 as f64;
        let total: f64 = a.1.iter().map(|e| e.1).sum::<f64>() / n;
        let mut line = format!("{} visible={visible} total={total:.2}ms", crate::win::now_unix());
        for (k, v, m) in &a.1 {
            line += &format!(" {k}={:.2}/{m:.1}", v / n);
        }
        line.push(10 as char);
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(crate::config::data_dir().join("profile.log")) {
            let _ = f.write_all(line.as_bytes());
        }
        *a = (0, Vec::new());
    }
}
