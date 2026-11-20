//! Deeper hardware and kernel metrics for the Performance page.
//!
//! * `statics()`: things that never change while running (CPU topology and caches, RAM modules from
//!   SMBIOS, motherboard/BIOS, OS build, GPU driver). Gathered once on a background thread.
//! * `Dyn`: per-tick counters the sampler adds to each snapshot (context switches, interrupts, DPC
//!   time, page faults, memory lists, page files, GPU temperature/fan/power, TCP health, CPU clock).

use crate::win::{self, NtQuerySystemInformation, wide};
use std::ffi::c_void;
use std::sync::OnceLock;

// ---------------------------------------------------------------- raw imports

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetLogicalProcessorInformationEx(rel: i32, buf: *mut u8, len: *mut u32) -> i32;
    fn GetSystemFirmwareTable(sig: u32, id: u32, buf: *mut u8, size: u32) -> u32;
    fn GetPhysicallyInstalledSystemMemory(kb: *mut u64) -> i32;
    fn IsProcessorFeaturePresent(f: u32) -> i32;
    fn GetSystemPowerStatus(s: *mut PowerStatus) -> i32;
    fn GetCurrentProcess() -> *mut c_void;
}
#[link(name = "powrprof")]
unsafe extern "system" {
    fn CallNtPowerInformation(level: i32, inb: *const c_void, inl: u32, outb: *mut c_void, outl: u32) -> i32;
}
#[link(name = "gdi32")]
unsafe extern "system" {
    fn D3DKMTEnumAdapters2(p: *mut EnumAdapters2) -> i32;
    fn D3DKMTQueryAdapterInfo(p: *mut QueryAdapterInfo) -> i32;
}
#[link(name = "iphlpapi")]
unsafe extern "system" {
    fn GetTcpStatisticsEx(stats: *mut [u32; 15], family: u32) -> u32;
}
#[link(name = "advapi32")]
unsafe extern "system" {
    fn OpenProcessToken(p: *mut c_void, access: u32, tok: *mut *mut c_void) -> i32;
    fn LookupPrivilegeValueW(sys: *const u16, name: *const u16, luid: *mut [u32; 2]) -> i32;
    fn AdjustTokenPrivileges(tok: *mut c_void, disable: i32, new: *const TokenPriv, len: u32, prev: *mut c_void, ret: *mut u32) -> i32;
}

#[repr(C)]
struct TokenPriv {
    count: u32,
    luid: [u32; 2],
    attrs: u32,
}

#[repr(C)]
#[derive(Default)]
struct PowerStatus {
    ac: u8,
    flag: u8,
    percent: u8,
    saver: u8,
    life: u32,
    full: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct AdapterInfo {
    handle: u32,
    luid_lo: u32,
    luid_hi: i32,
    sources: u32,
    precise: i32,
}
#[repr(C)]
struct EnumAdapters2 {
    num: u32,
    adapters: *mut AdapterInfo,
}
#[repr(C)]
struct QueryAdapterInfo {
    handle: u32,
    ty: u32,
    data: *mut c_void,
    size: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct AdapterPerfData {
    phys_index: u32,
    mem_freq: u64,
    max_mem_freq: u64,
    max_mem_freq_oc: u64,
    mem_bandwidth: u64,
    pcie_bandwidth: u64,
    fan_rpm: u32,
    power: u32,
    temperature: u32,
    power_state_override: u8,
}
const _: () = assert!(std::mem::size_of::<AdapterPerfData>() == 64);
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SegmentSize {
    dedicated_video: u64,
    dedicated_system: u64,
    shared_system: u64,
}
const KMTQAITYPE_GETSEGMENTSIZE: u32 = 3;
const KMTQAITYPE_ADAPTERPERFDATA: u32 = 62;

// ---------------------------------------------------------------- static info

#[derive(Clone, Default)]
pub struct CpuStatic {
    pub sockets: u32,
    pub cores: u32,
    pub p_cores: u32,
    pub e_cores: u32,
    pub logical: u32,
    pub l1: u64,
    pub l2: u64,
    pub l3: u64,
    pub virtualization: bool,
    pub max_mhz: u32,
    pub base_mhz: u32,
}

#[derive(Clone, Default)]
pub struct Dimm {
    pub slot: String,
    pub size: u64,
    pub speed: u32,
    pub configured: u32,
    pub kind: String,
    pub form: String,
    pub maker: String,
    pub part: String,
}

#[derive(Clone, Default)]
pub struct MemStatic {
    pub installed: u64,
    pub slots_total: u32,
    pub dimms: Vec<Dimm>,
}

impl MemStatic {
    pub fn speed(&self) -> u32 {
        self.dimms.iter().map(|d| if d.configured > 0 { d.configured } else { d.speed }).max().unwrap_or(0)
    }
    pub fn kind(&self) -> String {
        self.dimms.iter().map(|d| d.kind.clone()).find(|k| !k.is_empty()).unwrap_or_default()
    }
    pub fn form(&self) -> String {
        self.dimms.iter().map(|d| d.form.clone()).find(|k| !k.is_empty()).unwrap_or_default()
    }
}

#[derive(Clone, Default)]
pub struct SysStatic {
    pub os: String,
    pub build: String,
    pub maker: String,
    pub model: String,
    pub board: String,
    pub bios: String,
    pub bios_date: String,
    pub gpu_driver: String,
    pub gpu_driver_date: String,
}

#[derive(Clone, Default)]
pub struct Statics {
    pub cpu: CpuStatic,
    pub mem: MemStatic,
    pub sys: SysStatic,
}

static STATICS: OnceLock<Statics> = OnceLock::new();

/// Starts gathering static info in the background (call once at startup).
pub fn warm() {
    std::thread::Builder::new()
        .name("hw-static".into())
        .spawn(|| {
            let _ = STATICS.get_or_init(gather);
        })
        .ok();
}

pub fn statics() -> Option<&'static Statics> {
    STATICS.get()
}

fn gather() -> Statics {
    let mut s = Statics { cpu: cpu_topology(), ..Default::default() };
    let smb = smbios();
    s.mem = smb.0;
    s.sys = smb.1;
    let mut kb = 0u64;
    if unsafe { GetPhysicallyInstalledSystemMemory(&mut kb) } != 0 {
        s.mem.installed = kb * 1024;
    }
    let (os, build) = os_version();
    s.sys.os = os;
    s.sys.build = build;
    let (ver, date) = gpu_driver();
    s.sys.gpu_driver = ver;
    s.sys.gpu_driver_date = date;
    s
}

fn cpu_topology() -> CpuStatic {
    let mut c = CpuStatic::default();
    let mut len = 0u32;
    unsafe { GetLogicalProcessorInformationEx(0xFFFF, std::ptr::null_mut(), &mut len) };
    if len > 0 {
        let mut buf = vec![0u8; len as usize];
        if unsafe { GetLogicalProcessorInformationEx(0xFFFF, buf.as_mut_ptr(), &mut len) } != 0 {
            let mut classes = Vec::new();
            let mut off = 0usize;
            while off + 8 <= len as usize {
                let rel = u32::from_ne_bytes(buf[off..off + 4].try_into().unwrap());
                let size = u32::from_ne_bytes(buf[off + 4..off + 8].try_into().unwrap()) as usize;
                if size == 0 {
                    break;
                }
                let b = &buf[off..off + size];
                match rel {
                    0 => {
                        c.cores += 1;
                        classes.push(b[9]);
                        // GROUP_AFFINITY array at 30: count logical processors via the mask.
                        let groups = u16::from_ne_bytes([b[30], b[31]]) as usize;
                        for g in 0..groups.max(1) {
                            let at = 32 + g * 16;
                            if at + 8 <= b.len() {
                                c.logical += u64::from_ne_bytes(b[at..at + 8].try_into().unwrap()).count_ones();
                            }
                        }
                    }
                    2 => {
                        let level = b[8];
                        let csize = u32::from_ne_bytes(b[12..16].try_into().unwrap()) as u64;
                        match level {
                            1 => c.l1 += csize,
                            2 => c.l2 += csize,
                            3 => c.l3 += csize,
                            _ => {}
                        }
                    }
                    3 => c.sockets += 1,
                    _ => {}
                }
                off += size;
            }
            // Hybrid CPUs: the highest efficiency class is the performance cores.
            let top = classes.iter().copied().max().unwrap_or(0);
            if top > 0 {
                c.p_cores = classes.iter().filter(|&&x| x == top).count() as u32;
                c.e_cores = c.cores - c.p_cores;
            }
        }
    }
    c.virtualization = unsafe { IsProcessorFeaturePresent(21) } != 0;
    let n = c.logical.max(1) as usize;
    let mut info = vec![[0u32; 6]; n];
    if unsafe { CallNtPowerInformation(11, std::ptr::null(), 0, info.as_mut_ptr() as *mut c_void, (n * 24) as u32) } == 0 {
        c.max_mhz = info.iter().map(|i| i[1]).max().unwrap_or(0);
    }
    c.base_mhz = winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE).open_subkey(r"HARDWARE\DESCRIPTION\System\CentralProcessor\0").and_then(|k| k.get_value::<u32, _>("~MHz")).unwrap_or(0);
    c
}

fn smbios() -> (MemStatic, SysStatic) {
    let mut mem = MemStatic::default();
    let mut sys = SysStatic::default();
    let sig = u32::from_be_bytes(*b"RSMB");
    let size = unsafe { GetSystemFirmwareTable(sig, 0, std::ptr::null_mut(), 0) };
    if size == 0 {
        return (mem, sys);
    }
    let mut buf = vec![0u8; size as usize];
    if unsafe { GetSystemFirmwareTable(sig, 0, buf.as_mut_ptr(), size) } == 0 || buf.len() < 8 {
        return (mem, sys);
    }
    let len = u32::from_ne_bytes(buf[4..8].try_into().unwrap()) as usize;
    let data = &buf[8..(8 + len).min(buf.len())];
    let mut off = 0usize;
    while off + 4 <= data.len() {
        let ty = data[off];
        let flen = data[off + 1] as usize;
        if flen < 4 || off + flen > data.len() {
            break;
        }
        let f = &data[off..off + flen];
        // String table follows the formatted area, ends with a double NUL.
        let mut strings = Vec::new();
        let mut p = off + flen;
        let mut cur = Vec::new();
        while p < data.len() {
            let b = data[p];
            p += 1;
            if b == 0 {
                if cur.is_empty() {
                    break;
                }
                strings.push(String::from_utf8_lossy(&cur).trim().to_string());
                cur.clear();
            } else {
                cur.push(b);
            }
        }
        if strings.is_empty() && p < data.len() && data[p] == 0 {
            p += 1;
        }
        let st = |i: usize| -> String { f.get(i).and_then(|&n| if n == 0 { None } else { strings.get(n as usize - 1).cloned() }).unwrap_or_default() };
        let w = |i: usize| -> u32 { if i + 2 <= f.len() { u16::from_ne_bytes([f[i], f[i + 1]]) as u32 } else { 0 } };
        match ty {
            0 => {
                sys.bios = tidy_vendor(&format!("{} {}", st(4), st(5)));
                sys.bios_date = st(8);
            }
            1 => {
                sys.maker = tidy_vendor(&st(4));
                sys.model = tidy_vendor(&st(5));
            }
            2 => sys.board = tidy_vendor(&format!("{} {}", st(4), st(5))),
            17 => {
                mem.slots_total += 1;
                let raw = w(0x0C);
                let size = if raw == 0 || raw == 0xFFFF {
                    0
                } else if raw == 0x7FFF && f.len() >= 0x20 {
                    u32::from_ne_bytes(f[0x1C..0x20].try_into().unwrap()) as u64 * 1048576
                } else if raw & 0x8000 != 0 {
                    (raw & 0x7FFF) as u64 * 1024
                } else {
                    raw as u64 * 1048576
                };
                if size > 0 {
                    let kind = match f.get(0x12).copied().unwrap_or(0) {
                        0x18 => "DDR3",
                        0x1A => "DDR4",
                        0x1B => "LPDDR",
                        0x1C => "LPDDR2",
                        0x1D => "LPDDR3",
                        0x1E => "LPDDR4",
                        0x22 => "DDR5",
                        0x23 => "LPDDR5",
                        _ => "",
                    };
                    let form = match f.get(0x0E).copied().unwrap_or(0) {
                        0x09 => "DIMM",
                        0x0D => "SODIMM",
                        0x0F => "FB-DIMM",
                        0x0B => "Row of chips",
                        _ => "",
                    };
                    let slot = [st(0x10), st(0x11)].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" / ");
                    mem.dimms.push(Dimm {
                        slot,
                        size,
                        speed: w(0x15),
                        configured: if f.len() >= 0x22 { w(0x20) } else { 0 },
                        kind: kind.into(),
                        form: form.into(),
                        maker: st(0x17),
                        part: st(0x1A),
                    });
                }
            }
            127 => break,
            _ => {}
        }
        off = p;
    }
    (mem, sys)
}

/// "ASUSTeK COMPUTER INC." -> "ASUS", "Micro-Star International Co., Ltd." -> "MSI", etc.
fn tidy_vendor(s: &str) -> String {
    let mut out = s.trim().to_string();
    for (long, short) in [
        ("ASUSTeK COMPUTER INC.", "ASUS"),
        ("ASUSTeK Computer Inc.", "ASUS"),
        ("Micro-Star International Co., Ltd.", "MSI"),
        ("Micro-Star International Co.,Ltd.", "MSI"),
        ("Gigabyte Technology Co., Ltd.", "Gigabyte"),
        ("American Megatrends International, LLC.", "AMI"),
        ("American Megatrends Inc.", "AMI"),
        ("ASRock", "ASRock"),
        ("Hewlett-Packard", "HP"),
        ("Dell Inc.", "Dell"),
        ("LENOVO", "Lenovo"),
    ] {
        if out.starts_with(long) {
            out = format!("{short}{}", &out[long.len()..]);
        }
    }
    out.replace("To Be Filled By O.E.M.", "").replace("System Product Name", "").replace("Default string", "").trim().to_string()
}

fn os_version() -> (String, String) {
    let k = winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE).open_subkey(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion");
    let Ok(k) = k else { return ("Windows".into(), String::new()) };
    let mut name: String = k.get_value("ProductName").unwrap_or_else(|_| "Windows".into());
    let build: String = k.get_value("CurrentBuild").unwrap_or_default();
    let ubr: u32 = k.get_value("UBR").unwrap_or(0);
    let disp: String = k.get_value("DisplayVersion").unwrap_or_default();
    // Windows 11 still says "Windows 10" in ProductName.
    if build.parse::<u32>().unwrap_or(0) >= 22000 {
        name = name.replace("Windows 10", "Windows 11");
    }
    let os = if disp.is_empty() { name } else { format!("{name} {disp}") };
    (os, if ubr > 0 { format!("{build}.{ubr}") } else { build })
}

fn gpu_driver() -> (String, String) {
    let mut best = (0u64, String::new(), String::new());
    if let Ok(class) = winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE).open_subkey(r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}") {
        for k in class.enum_keys().flatten().filter(|k| k.chars().all(|c| c.is_ascii_digit())) {
            if let Ok(sub) = class.open_subkey(&k) {
                let mem = sub.get_value::<u64, _>("HardwareInformation.qwMemorySize").ok().or_else(|| sub.get_value::<u32, _>("HardwareInformation.MemorySize").ok().map(|v| v as u64)).unwrap_or(0);
                let ver: String = sub.get_value("DriverVersion").unwrap_or_default();
                let date: String = sub.get_value("DriverDate").unwrap_or_default();
                if mem >= best.0 && !ver.is_empty() {
                    best = (mem, ver, date);
                }
            }
        }
    }
    (best.1, best.2)
}

// ---------------------------------------------------------------- dynamic

#[derive(Clone, Default)]
pub struct MemLists {
    pub zero: u64,
    pub free: u64,
    pub modified: u64,
    pub standby: u64,
}

#[derive(Clone, Default)]
pub struct PageFile {
    pub name: String,
    pub total: u64,
    pub used: u64,
    pub peak: u64,
}

#[derive(Clone, Default)]
pub struct Tcp {
    pub established: u32,
    pub connections: u32,
    pub retrans_pct: f32,
    pub segs_in: f64,
    pub segs_out: f64,
    pub failed: f64,
    pub resets: f64,
    pub in_errors: f64,
}

#[derive(Clone, Default)]
pub struct GpuSensors {
    pub temperature: Option<f32>,
    pub fan_rpm: Option<u32>,
    pub power_pct: Option<f32>,
    pub mem_clock_mhz: Option<u32>,
}

#[derive(Clone, Default)]
pub struct Extra {
    pub ctx_switches: f64,
    pub syscalls: f64,
    pub interrupts: f64,
    pub dpc_pct: f32,
    pub irq_pct: f32,
    pub page_faults: f64,
    pub hard_faults: f64,
    pub cpu_mhz: Option<u32>,
    pub mem_lists: Option<MemLists>,
    pub pagefiles: Vec<PageFile>,
    pub gpu: GpuSensors,
    pub tcp: Option<Tcp>,
    pub battery: Option<(u8, bool)>,
}

#[derive(Default)]
pub struct Dyn {
    perf_buf: Vec<u8>,
    prev_perf: Option<(u32, u32, u32, u32)>,
    prev_tcp: Option<[u32; 15]>,
    priv_enabled: bool,
    gpu_adapter: Option<u32>,
    gpu_tried: bool,
    freq_pdh: Option<FreqPdh>,
    freq_tried: bool,
}

/// `cpu`: per-interval totals (interrupts, dpc time, irq time, all time) from the per-core counters.
impl Dyn {
    pub fn sample(&mut self, elapsed: f64, visible: bool, _tick: u64, cpu: (u32, f64, f64, f64)) -> Extra {
        let mut e = Extra::default();
        let (intr, dpc, irq, all) = cpu;
        e.interrupts = intr as f64 / elapsed;
        if all > 0.0 {
            e.dpc_pct = (dpc / all * 100.0) as f32;
            e.irq_pct = (irq / all * 100.0) as f32;
        }
        // SYSTEM_PERFORMANCE_INFORMATION: PageFaultCount @60, PageReadIoCount @84,
        // ContextSwitches @296, SystemCalls @308.
        if self.perf_buf.is_empty() {
            self.perf_buf = vec![0u8; 1024];
        }
        if query(2, &mut self.perf_buf) && self.perf_buf.len() >= 312 {
            let r = |o: usize| u32::from_ne_bytes(self.perf_buf[o..o + 4].try_into().unwrap());
            let cur = (r(60), r(84), r(296), r(308));
            if let Some(p) = self.prev_perf {
                e.page_faults = cur.0.wrapping_sub(p.0) as f64 / elapsed;
                e.hard_faults = cur.1.wrapping_sub(p.1) as f64 / elapsed;
                e.ctx_switches = cur.2.wrapping_sub(p.2) as f64 / elapsed;
                e.syscalls = cur.3.wrapping_sub(p.3) as f64 / elapsed;
            }
            self.prev_perf = Some(cur);
        }
        if !visible {
            return e;
        }
        // Memory lists (needs SeProfileSingleProcessPrivilege, which admins can enable).
        if !self.priv_enabled {
            self.priv_enabled = true;
            enable_privilege("SeProfileSingleProcessPrivilege");
        }
        let mut ml = vec![0u8; 256];
        if query(80, &mut ml) {
            let r = |o: usize| u64::from_ne_bytes(ml[o..o + 8].try_into().unwrap()) * 4096;
            let standby: u64 = (0..8).map(|i| r(40 + i * 8)).sum();
            e.mem_lists = Some(MemLists { zero: r(0), free: r(8), modified: r(16), standby });
        }
        // Page files.
        let mut pf = vec![0u8; 4096];
        if query(18, &mut pf) {
            let mut off = 0usize;
            loop {
                if off + 32 > pf.len() {
                    break;
                }
                let next = u32::from_ne_bytes(pf[off..off + 4].try_into().unwrap()) as usize;
                let total = u32::from_ne_bytes(pf[off + 4..off + 8].try_into().unwrap()) as u64 * 4096;
                let used = u32::from_ne_bytes(pf[off + 8..off + 12].try_into().unwrap()) as u64 * 4096;
                let peak = u32::from_ne_bytes(pf[off + 12..off + 16].try_into().unwrap()) as u64 * 4096;
                let nlen = u16::from_ne_bytes([pf[off + 16], pf[off + 17]]) as usize / 2;
                let ptr = usize::from_ne_bytes(pf[off + 24..off + 32].try_into().unwrap()) as *const u16;
                let name = if ptr.is_null() || nlen == 0 { String::new() } else { String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(ptr, nlen) }) };
                if total > 0 {
                    e.pagefiles.push(PageFile { name: name.trim_start_matches(r"\??\").to_string(), total, used, peak });
                }
                if next == 0 {
                    break;
                }
                off += next;
            }
        }
        // GPU sensors through the display kernel (what Task Manager uses for temperature).
        if !self.gpu_tried {
            self.gpu_tried = true;
            self.gpu_adapter = pick_gpu();
        }
        if let Some(h) = self.gpu_adapter {
            let mut pd = AdapterPerfData::default();
            if kmt_query(h, KMTQAITYPE_ADAPTERPERFDATA, &mut pd) {
                e.gpu = GpuSensors {
                    temperature: (pd.temperature > 0).then(|| pd.temperature as f32 / 10.0),
                    fan_rpm: (pd.fan_rpm > 0).then_some(pd.fan_rpm),
                    power_pct: (pd.power > 0).then(|| pd.power as f32 / 10.0),
                    mem_clock_mhz: (pd.mem_freq > 0).then(|| (pd.mem_freq / 1_000_000) as u32),
                };
            }
        }
        // TCP health (IPv4 + IPv6).
        let mut sum = [0u32; 15];
        let mut ok = false;
        for fam in [2u32, 23] {
            let mut s = [0u32; 15];
            if unsafe { GetTcpStatisticsEx(&mut s, fam) } == 0 {
                ok = true;
                for (a, b) in sum.iter_mut().zip(s) {
                    *a = a.wrapping_add(b);
                }
            }
        }
        if ok {
            // MIB_TCPSTATS: 4 ActiveOpens, 5 PassiveOpens, 6 AttemptFails, 7 EstabResets, 8 CurrEstab,
            // 9 InSegs, 10 OutSegs, 11 RetransSegs, 12 InErrs, 13 OutRsts, 14 NumConns.
            let mut t = Tcp { established: sum[8], connections: sum[14], ..Default::default() };
            if let Some(p) = self.prev_tcp {
                let d = |i: usize| sum[i].wrapping_sub(p[i]) as f64;
                t.segs_in = d(9) / elapsed;
                t.segs_out = d(10) / elapsed;
                t.failed = d(6) / elapsed;
                t.resets = d(7) / elapsed;
                t.in_errors = d(12) / elapsed;
                t.retrans_pct = if d(10) > 0.0 { (d(11) / d(10) * 100.0) as f32 } else { 0.0 };
            }
            self.prev_tcp = Some(sum);
            e.tcp = Some(t);
        }
        // Current CPU clock: "% Processor Performance" × base speed (same source as Task Manager).
        if !self.freq_tried {
            self.freq_tried = true;
            self.freq_pdh = FreqPdh::new();
        }
        if let Some(f) = self.freq_pdh.as_mut() {
            if let Some(pct) = f.read() {
                let base = statics().map(|s| s.cpu.base_mhz).unwrap_or(0);
                if base > 0 && pct > 0.0 {
                    e.cpu_mhz = Some((base as f64 * pct / 100.0) as u32);
                }
            }
        }
        {
            let mut ps = PowerStatus::default();
            if unsafe { GetSystemPowerStatus(&mut ps) } != 0 && ps.flag != 128 && ps.flag != 255 && ps.percent <= 100 {
                e.battery = Some((ps.percent, ps.ac == 1));
            }
        }
        e
    }
}

fn query(class: u32, buf: &mut Vec<u8>) -> bool {
    for _ in 0..4 {
        let mut ret = 0u32;
        let st = unsafe { NtQuerySystemInformation(class, buf.as_mut_ptr() as *mut c_void, buf.len() as u32, &mut ret) };
        if st == 0 {
            return true;
        }
        if (st as u32 == 0xC0000004 || st as u32 == 0x80000005) && buf.len() < 1 << 20 {
            buf.resize((ret as usize).max(buf.len() * 2), 0);
            continue;
        }
        return false;
    }
    false
}

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtSetSystemInformation(class: u32, info: *const c_void, len: u32) -> i32;
}

/// Empties the standby list (cached file data Windows keeps "just in case"). Returns bytes freed.
pub fn purge_standby() -> Result<u64, String> {
    enable_privilege("SeProfileSingleProcessPrivilege");
    let before = avail_bytes();
    let cmd: u32 = 4; // MemoryPurgeStandbyList
    let st = unsafe { NtSetSystemInformation(80, &cmd as *const u32 as *const c_void, 4) };
    if st != 0 {
        return Err(format!("Windows refused (0x{:08X}). Needs administrator rights.", st as u32));
    }
    Ok(avail_bytes().saturating_sub(before).max(0))
}

fn avail_bytes() -> u64 {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut m: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    m.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    unsafe { GlobalMemoryStatusEx(&mut m) };
    // Standby pages count as available, so measure "free" (zero + free lists) instead.
    let mut info = [0u64; 64];
    let mut ret = 0u32;
    let st = unsafe { NtQuerySystemInformation(80, info.as_mut_ptr() as *mut c_void, std::mem::size_of_val(&info) as u32, &mut ret) };
    if st == 0 {
        // SYSTEM_MEMORY_LIST_INFORMATION: ZeroPageCount, FreePageCount are the first two fields.
        (info[0] + info[1]) * 4096
    } else {
        m.ullAvailPhys
    }
}

fn enable_privilege(name: &str) {
    unsafe {
        let mut tok = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), 0x0020 | 0x0008, &mut tok) == 0 {
            return;
        }
        let tok = win::Handle(tok);
        let mut luid = [0u32; 2];
        let n = wide(name);
        if LookupPrivilegeValueW(std::ptr::null(), n.as_ptr(), &mut luid) == 0 {
            return;
        }
        let tp = TokenPriv { count: 1, luid, attrs: 2 };
        AdjustTokenPrivileges(tok.0, 0, &tp, 0, std::ptr::null_mut(), std::ptr::null_mut());
    }
}

fn kmt_query<T>(handle: u32, ty: u32, out: &mut T) -> bool {
    let mut q = QueryAdapterInfo { handle, ty, data: out as *mut T as *mut c_void, size: std::mem::size_of::<T>() as u32 };
    unsafe { D3DKMTQueryAdapterInfo(&mut q) == 0 }
}

/// The adapter with the most dedicated VRAM that reports performance data.
fn pick_gpu() -> Option<u32> {
    let mut e = EnumAdapters2 { num: 0, adapters: std::ptr::null_mut() };
    if unsafe { D3DKMTEnumAdapters2(&mut e) } != 0 || e.num == 0 {
        return None;
    }
    let mut list = vec![AdapterInfo::default(); e.num as usize];
    e.adapters = list.as_mut_ptr();
    if unsafe { D3DKMTEnumAdapters2(&mut e) } != 0 {
        return None;
    }
    let mut best: Option<(u64, u32)> = None;
    for a in list.iter().take(e.num as usize) {
        let mut pd = AdapterPerfData::default();
        if !kmt_query(a.handle, KMTQAITYPE_ADAPTERPERFDATA, &mut pd) {
            continue;
        }
        let mut seg = SegmentSize::default();
        let vram = if kmt_query(a.handle, KMTQAITYPE_GETSEGMENTSIZE, &mut seg) { seg.dedicated_video } else { 0 };
        if best.is_none_or(|b| vram > b.0) {
            best = Some((vram, a.handle));
        }
    }
    best.map(|b| b.1)
}

// PDH counter for the effective CPU clock.
struct FreqPdh {
    query: isize,
    counter: isize,
    primed: bool,
}
unsafe impl Send for FreqPdh {}

impl FreqPdh {
    fn new() -> Option<Self> {
        use windows_sys::Win32::System::Performance::*;
        unsafe {
            let mut q: PDH_HQUERY = std::mem::zeroed();
            if PdhOpenQueryW(std::ptr::null(), 0, &mut q) != 0 {
                return None;
            }
            let mut c: PDH_HCOUNTER = std::mem::zeroed();
            let path = wide("\\Processor Information(_Total)\\% Processor Performance");
            if PdhAddEnglishCounterW(q, path.as_ptr(), 0, &mut c) != 0 {
                PdhCloseQuery(q);
                return None;
            }
            Some(Self { query: q as isize, counter: c as isize, primed: false })
        }
    }
    fn read(&mut self) -> Option<f64> {
        use windows_sys::Win32::System::Performance::*;
        unsafe {
            if PdhCollectQueryData(self.query as PDH_HQUERY) != 0 {
                return None;
            }
            if !self.primed {
                self.primed = true;
                return None;
            }
            let mut v: PDH_FMT_COUNTERVALUE = std::mem::zeroed();
            if PdhGetFormattedCounterValue(self.counter as PDH_HCOUNTER, PDH_FMT_DOUBLE | 0x8000, std::ptr::null_mut(), &mut v) != 0 {
                return None;
            }
            Some(v.Anonymous.doubleValue)
        }
    }
}

impl Drop for FreqPdh {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::System::Performance::PdhCloseQuery(self.query as _) };
    }
}

/// `sentinel --hw-dump`: prints what the Performance page would show, for checking on new hardware.
pub fn dump() {
    let s = STATICS.get_or_init(gather).clone();
    let c = &s.cpu;
    println!("CPU: sockets {} cores {} (P {} / E {}) logical {} L1 {} L2 {} L3 {} virt {} base {} MHz max {} MHz", c.sockets, c.cores, c.p_cores, c.e_cores, c.logical, c.l1, c.l2, c.l3, c.virtualization, c.base_mhz, c.max_mhz);
    println!("RAM: installed {} slots {} speed {} kind {} form {}", s.mem.installed, s.mem.slots_total, s.mem.speed(), s.mem.kind(), s.mem.form());
    for d in &s.mem.dimms {
        println!("  {} {} MB {} MT/s (cfg {}) {} {} {} {}", d.slot, d.size >> 20, d.speed, d.configured, d.kind, d.form, d.maker, d.part);
    }
    let y = &s.sys;
    println!("SYS: {} build {} | {} {} | board {} | BIOS {} ({}) | GPU driver {} ({})", y.os, y.build, y.maker, y.model, y.board, y.bios, y.bios_date, y.gpu_driver, y.gpu_driver_date);
    let mut d = Dyn::default();
    let mut disks = crate::disk::Disks::new();
    for i in 0..3 {
        let e = d.sample(1.0, true, i, (0, 0.0, 0.0, 1.0));
        let ds = disks.sample();
        if i == 2 {
            println!("DYN: ctx {:.0}/s syscalls {:.0}/s faults {:.0}/s hard {:.0}/s mhz {:?}", e.ctx_switches, e.syscalls, e.page_faults, e.hard_faults, e.cpu_mhz);
            if let Some(m) = &e.mem_lists {
                println!("  lists: zero {} free {} modified {} standby {} (MB)", m.zero >> 20, m.free >> 20, m.modified >> 20, m.standby >> 20);
            } else {
                println!("  lists: unavailable (needs admin)");
            }
            for p in &e.pagefiles {
                println!("  pagefile {} total {} used {} peak {} MB", p.name, p.total >> 20, p.used >> 20, p.peak >> 20);
            }
            println!("  gpu temp {:?} fan {:?} power {:?} memclk {:?}", e.gpu.temperature, e.gpu.fan_rpm, e.gpu.power_pct, e.gpu.mem_clock_mhz);
            if let Some(t) = &e.tcp {
                println!("  tcp estab {} conns {} retrans {:.2}% in {:.0}/s out {:.0}/s", t.established, t.connections, t.retrans_pct, t.segs_in, t.segs_out);
            }
            println!("  battery {:?}", e.battery);
            for x in &ds {
                let i = x.info.as_ref().unwrap();
                println!("  disk {} model {} bus {} ssd {:?} size {} GB temp {:?} riops {:.0} wiops {:.0} resp {:.2} ms q {} vols {:?}", x.name, i.model, i.bus, i.ssd, i.size >> 30, x.temperature, x.read_iops, x.write_iops, x.response_ms, x.queue, x.volumes.iter().map(|v| format!("{} {} {} {}/{} GB", v.letter, v.label, v.fs, v.free >> 30, v.total >> 30)).collect::<Vec<_>>());
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(1000));
    }
}
