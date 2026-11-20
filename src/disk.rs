//! Physical disk throughput, IOPS, latency, queue and temperature straight from the disk driver
//! (IOCTL_DISK_PERFORMANCE / IOCTL_STORAGE_QUERY_PROPERTY). Works even when Windows performance
//! counters are disabled or corrupted.

use crate::pdh::DiskSample;
use crate::win::{Handle, from_wide, wide};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Arc;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::IO::DeviceIoControl;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct DiskPerformance {
    bytes_read: i64,
    bytes_written: i64,
    read_time: i64,
    write_time: i64,
    idle_time: i64,
    read_count: u32,
    write_count: u32,
    queue_depth: u32,
    split_count: u32,
    query_time: i64,
    storage_device_number: u32,
    storage_manager_name: [u16; 8],
}

const IOCTL_DISK_PERFORMANCE: u32 = 0x0007_0020;
const IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS: u32 = 0x0056_0000;
const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;
const IOCTL_DISK_GET_DRIVE_GEOMETRY_EX: u32 = 0x0007_00A0;

/// Static facts about a physical disk.
#[derive(Clone, Default)]
pub struct DiskInfo {
    pub model: String,
    pub bus: String,
    pub ssd: Option<bool>,
    pub size: u64,
    pub removable: bool,
}

#[derive(Clone, Default)]
pub struct Volume {
    pub letter: char,
    pub label: String,
    pub fs: String,
    pub total: u64,
    pub free: u64,
}

struct Drive {
    num: u32,
    handle: Handle,
    prev: Option<DiskPerformance>,
    info: Arc<DiskInfo>,
    temp: Option<i16>,
    temp_ok: bool,
}

pub struct Disks {
    drives: Vec<Drive>,
    letters: HashMap<u32, Vec<char>>,
    volumes: HashMap<char, Volume>,
    tick: u64,
}

fn open(path: &str) -> Option<Handle> {
    let w = wide(path);
    let h = unsafe { CreateFileW(w.as_ptr(), 0, FILE_SHARE_READ | FILE_SHARE_WRITE, std::ptr::null(), OPEN_EXISTING, 0, std::ptr::null_mut()) };
    if h.is_null() || h == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE { None } else { Some(Handle(h)) }
}

fn ioctl(h: &Handle, code: u32, input: &[u8], out: &mut [u8]) -> Option<usize> {
    let mut ret = 0u32;
    let ok = unsafe { DeviceIoControl(h.0, code, if input.is_empty() { std::ptr::null() } else { input.as_ptr() as *const c_void }, input.len() as u32, out.as_mut_ptr() as *mut c_void, out.len() as u32, &mut ret, std::ptr::null_mut()) };
    if ok == 0 { None } else { Some(ret as usize) }
}

fn query(h: &Handle) -> Option<DiskPerformance> {
    let mut p = DiskPerformance::default();
    let mut ret = 0u32;
    let ok = unsafe { DeviceIoControl(h.0, IOCTL_DISK_PERFORMANCE, std::ptr::null(), 0, &mut p as *mut _ as *mut c_void, std::mem::size_of::<DiskPerformance>() as u32, &mut ret, std::ptr::null_mut()) };
    if ok == 0 { None } else { Some(p) }
}

/// STORAGE_PROPERTY_QUERY { PropertyId, QueryType = standard, AdditionalParameters }.
fn property(h: &Handle, id: u32, out: &mut [u8]) -> Option<usize> {
    let mut q = [0u8; 12];
    q[0..4].copy_from_slice(&id.to_ne_bytes());
    ioctl(h, IOCTL_STORAGE_QUERY_PROPERTY, &q, out)
}

fn cstr_at(buf: &[u8], off: usize) -> String {
    if off == 0 || off >= buf.len() {
        return String::new();
    }
    let end = buf[off..].iter().position(|&b| b == 0).map(|e| off + e).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[off..end]).trim().to_string()
}

fn bus_name(t: u32) -> &'static str {
    match t {
        1 => "SCSI",
        2 => "ATAPI",
        3 => "ATA",
        4 => "1394",
        7 => "USB",
        8 => "RAID",
        9 => "iSCSI",
        10 => "SAS",
        11 => "SATA",
        12 => "SD",
        13 => "MMC",
        14 | 15 => "Virtual",
        16 => "Storage Spaces",
        17 => "NVMe",
        18 => "SCM",
        19 => "UFS",
        _ => "",
    }
}

fn disk_info(h: &Handle) -> DiskInfo {
    let mut d = DiskInfo::default();
    // STORAGE_DEVICE_DESCRIPTOR: RemovableMedia @10, VendorIdOffset @12, ProductIdOffset @16, BusType @28.
    let mut buf = vec![0u8; 1024];
    if let Some(n) = property(h, 0, &mut buf) {
        let b = &buf[..n.max(32).min(buf.len())];
        let rd = |o: usize| u32::from_ne_bytes(b[o..o + 4].try_into().unwrap()) as usize;
        let vendor = cstr_at(b, rd(12));
        let product = cstr_at(b, rd(16));
        d.model = if vendor.is_empty() || product.to_lowercase().starts_with(&vendor.to_lowercase()) { product } else { format!("{vendor} {product}") };
        d.bus = bus_name(rd(28) as u32).to_string();
        d.removable = b[10] != 0;
    }
    // DEVICE_SEEK_PENALTY_DESCRIPTOR: IncursSeekPenalty @8.
    let mut sp = [0u8; 12];
    if property(h, 7, &mut sp).is_some_and(|n| n >= 9) {
        d.ssd = Some(sp[8] == 0);
    }
    // DISK_GEOMETRY_EX: DiskSize @24.
    let mut g = [0u8; 256];
    if ioctl(h, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, &[], &mut g).is_some_and(|n| n >= 32) {
        d.size = u64::from_ne_bytes(g[24..32].try_into().unwrap());
    }
    d
}

/// STORAGE_TEMPERATURE_DATA_DESCRIPTOR: InfoCount @12, first STORAGE_TEMPERATURE_INFO @24 with
/// Temperature (°C, i16) @+2.
fn temperature(h: &Handle) -> Option<i16> {
    let mut buf = [0u8; 256];
    let n = property(h, 51, &mut buf)?;
    if n < 28 || u16::from_ne_bytes([buf[12], buf[13]]) == 0 {
        return None;
    }
    let t = i16::from_ne_bytes([buf[26], buf[27]]);
    (t > -40 && t < 150 && t != 0).then_some(t)
}

/// Which physical disk each drive letter lives on.
fn map_letters() -> HashMap<u32, Vec<char>> {
    let mut out: HashMap<u32, Vec<char>> = HashMap::new();
    let mask = unsafe { GetLogicalDrives() };
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let root = wide(&format!("{letter}:\\"));
        let ty = unsafe { GetDriveTypeW(root.as_ptr()) };
        if ty != 3 && ty != 2 {
            continue; // fixed and removable disks only
        }
        let Some(h) = open(&format!("\\\\.\\{letter}:")) else { continue };
        let mut buf = [0u8; 256];
        if ioctl(&h, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, &[], &mut buf).is_some_and(|n| n >= 12) {
            // VOLUME_DISK_EXTENTS { NumberOfDiskExtents: u32, pad, DISK_EXTENT { DiskNumber: u32, ... } }
            let n = u32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]);
            if n >= 1 {
                let disk = u32::from_ne_bytes([buf[8], buf[9], buf[10], buf[11]]);
                out.entry(disk).or_default().push(letter);
            }
        }
    }
    out
}

fn volume(letter: char) -> Option<Volume> {
    let root = wide(&format!("{letter}:\\"));
    let (mut free, mut total, mut _tf) = (0u64, 0u64, 0u64);
    if unsafe { GetDiskFreeSpaceExW(root.as_ptr(), &mut free, &mut total, &mut _tf) } == 0 {
        return None;
    }
    let mut label = [0u16; 261];
    let mut fs = [0u16; 64];
    unsafe { GetVolumeInformationW(root.as_ptr(), label.as_mut_ptr(), label.len() as u32, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), fs.as_mut_ptr(), fs.len() as u32) };
    Some(Volume { letter, label: from_wide(&label), fs: from_wide(&fs), total, free })
}

impl Disks {
    pub fn new() -> Self {
        let mut drives = Vec::new();
        for num in 0..32u32 {
            if let Some(h) = open(&format!("\\\\.\\PhysicalDrive{num}")) {
                if query(&h).is_some() {
                    let info = Arc::new(disk_info(&h));
                    let temp = temperature(&h);
                    drives.push(Drive { num, handle: h, prev: None, info, temp_ok: temp.is_some(), temp });
                }
            }
        }
        Self { drives, letters: map_letters(), volumes: HashMap::new(), tick: 0 }
    }

    pub fn sample(&mut self) -> Vec<DiskSample> {
        self.tick += 1;
        if self.tick % 30 == 0 {
            self.letters = map_letters();
        }
        if self.tick % 10 == 1 {
            self.volumes = self.letters.values().flatten().filter_map(|&l| volume(l).map(|v| (l, v))).collect();
        }
        let refresh_temp = self.tick % 5 == 0;
        let mut out = Vec::new();
        for d in self.drives.iter_mut() {
            let Some(cur) = query(&d.handle) else { continue };
            if refresh_temp && d.temp_ok {
                d.temp = temperature(&d.handle);
            }
            if let Some(p) = d.prev {
                let dt = (cur.query_time - p.query_time) as f64; // 100ns units
                if dt > 0.0 {
                    let secs = dt / 10_000_000.0;
                    let idle = (cur.idle_time - p.idle_time) as f64;
                    let active = ((1.0 - idle / dt) * 100.0).clamp(0.0, 100.0) as f32;
                    let letters = self.letters.get(&d.num).cloned().unwrap_or_default();
                    let ltxt = letters.iter().map(|c| format!("{c}:")).collect::<Vec<_>>().join(" ");
                    let reads = cur.read_count.wrapping_sub(p.read_count) as f64;
                    let writes = cur.write_count.wrapping_sub(p.write_count) as f64;
                    let busy = ((cur.read_time - p.read_time) + (cur.write_time - p.write_time)).max(0) as f64;
                    let ios = reads + writes;
                    out.push(DiskSample {
                        name: if ltxt.is_empty() { format!("{}", d.num) } else { format!("{} ({ltxt})", d.num) },
                        read: (cur.bytes_read - p.bytes_read).max(0) as f64 / secs,
                        write: (cur.bytes_written - p.bytes_written).max(0) as f64 / secs,
                        active,
                        read_iops: reads / secs,
                        write_iops: writes / secs,
                        response_ms: if ios > 0.0 { busy / ios / 10_000.0 } else { 0.0 },
                        queue: cur.queue_depth,
                        temperature: d.temp,
                        info: Some(d.info.clone()),
                        volumes: letters.iter().filter_map(|l| self.volumes.get(l).cloned()).collect(),
                    });
                }
            }
            d.prev = Some(cur);
        }
        out
    }
}
