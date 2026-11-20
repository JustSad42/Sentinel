//! Networking: adapters, connection tables, per-process bandwidth (ETW), reverse DNS and latency probes.

use crate::win::{from_wide, wide};
use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use windows_sys::Win32::NetworkManagement::IpHelper::*;

// ---------------------------------------------------------------- adapters

#[derive(Clone, Default)]
pub struct Adapter {
    pub index: u32,
    pub alias: String,
    pub description: String,
    pub kind: &'static str,
    pub mac: String,
    pub speed: u64,
    pub in_octets: u64,
    pub out_octets: u64,
    pub recv_rate: f64,
    pub send_rate: f64,
    pub in_pkts: u64,
    pub out_pkts: u64,
    pub in_pps: f64,
    pub out_pps: f64,
    pub in_errors: u64,
    pub out_errors: u64,
    pub in_discards: u64,
    pub out_discards: u64,
    pub ipv4: Vec<String>,
    pub ipv6: Vec<String>,
    pub gateways: Vec<String>,
    pub dns: Vec<String>,
}

fn if_kind(t: u32) -> &'static str {
    match t {
        6 => "Ethernet",
        71 => "Wi-Fi",
        243 | 244 => "Cellular",
        23 | 53 => "VPN / PPP",
        131 => "Tunnel",
        _ => "Network",
    }
}

/// Physical, connected adapters with their byte counters.
pub fn adapters() -> Vec<Adapter> {
    let mut out = Vec::new();
    unsafe {
        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        if GetIfTable2(&mut table) != 0 || table.is_null() {
            return out;
        }
        let n = (*table).NumEntries as usize;
        let rows = std::slice::from_raw_parts((*table).Table.as_ptr(), n);
        for r in rows {
            let flags = r.InterfaceAndOperStatusFlags._bitfield;
            let hardware = flags & 1 != 0;
            let filter = flags & 2 != 0;
            if !hardware || filter || r.OperStatus != 1 || r.Type == 24 {
                continue;
            }
            let mac = r.PhysicalAddress[..r.PhysicalAddressLength.min(8) as usize].iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join("-");
            out.push(Adapter {
                index: r.InterfaceIndex,
                alias: from_wide(&r.Alias),
                description: from_wide(&r.Description),
                kind: if_kind(r.Type),
                mac,
                speed: r.ReceiveLinkSpeed.max(r.TransmitLinkSpeed),
                in_octets: r.InOctets,
                out_octets: r.OutOctets,
                in_pkts: r.InUcastPkts + r.InNUcastPkts,
                out_pkts: r.OutUcastPkts + r.OutNUcastPkts,
                in_errors: r.InErrors,
                out_errors: r.OutErrors,
                in_discards: r.InDiscards,
                out_discards: r.OutDiscards,
                ..Default::default()
            });
        }
        FreeMibTable(table as *const c_void);
    }
    out
}

unsafe fn sockaddr_ip(p: *const u8) -> Option<IpAddr> {
    if p.is_null() {
        return None;
    }
    unsafe {
        let fam = *(p as *const u16);
        match fam {
            2 => Some(IpAddr::V4(Ipv4Addr::new(*p.add(4), *p.add(5), *p.add(6), *p.add(7)))),
            23 => {
                let mut b = [0u8; 16];
                std::ptr::copy_nonoverlapping(p.add(8), b.as_mut_ptr(), 16);
                Some(IpAddr::V6(Ipv6Addr::from(b)))
            }
            _ => None,
        }
    }
}

/// Fills IPs, gateways and DNS servers into the adapters (by interface index).
pub fn fill_addresses(list: &mut [Adapter]) {
    unsafe {
        let mut size = 32 * 1024u32;
        let mut buf: Vec<u8>;
        loop {
            buf = vec![0u8; size as usize];
            let r = GetAdaptersAddresses(0, GAA_FLAG_INCLUDE_GATEWAYS | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_ANYCAST, std::ptr::null(), buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH, &mut size);
            if r == 111 {
                continue; // ERROR_BUFFER_OVERFLOW
            }
            if r != 0 {
                return;
            }
            break;
        }
        let mut cur = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
        while !cur.is_null() {
            let a = &*cur;
            let idx = a.Anonymous1.Anonymous.IfIndex;
            let idx6 = a.Ipv6IfIndex;
            if let Some(ad) = list.iter_mut().find(|x| x.index == idx || x.index == idx6) {
                let mut u = a.FirstUnicastAddress;
                while !u.is_null() {
                    if let Some(ip) = sockaddr_ip((*u).Address.lpSockaddr as *const u8) {
                        match ip {
                            IpAddr::V4(v) => ad.ipv4.push(v.to_string()),
                            IpAddr::V6(v) if !v.is_unicast_link_local() => ad.ipv6.push(v.to_string()),
                            _ => {}
                        }
                    }
                    u = (*u).Next;
                }
                let mut g = a.FirstGatewayAddress;
                while !g.is_null() {
                    if let Some(ip) = sockaddr_ip((*g).Address.lpSockaddr as *const u8) {
                        ad.gateways.push(ip.to_string());
                    }
                    g = (*g).Next;
                }
                let mut d = a.FirstDnsServerAddress;
                while !d.is_null() {
                    if let Some(ip) = sockaddr_ip((*d).Address.lpSockaddr as *const u8) {
                        ad.dns.push(ip.to_string());
                    }
                    d = (*d).Next;
                }
            }
            cur = a.Next;
        }
    }
}

// ---------------------------------------------------------------- connections

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Proto {
    Tcp,
    Udp,
}

#[derive(Clone)]
pub struct Conn {
    pub proto: Proto,
    pub local: SocketAddr,
    pub remote: Option<SocketAddr>,
    pub state: u32,
    pub pid: u32,
    pub send_rate: f64,
    pub recv_rate: f64,
    pub total: u64,
}

pub fn tcp_state(s: u32) -> &'static str {
    match s {
        1 => "Closed",
        2 => "Listening",
        3 => "SYN sent",
        4 => "SYN received",
        5 => "Established",
        6 => "FIN wait 1",
        7 => "FIN wait 2",
        8 => "Close wait",
        9 => "Closing",
        10 => "Last ACK",
        11 => "Time wait",
        12 => "Delete TCB",
        _ => "",
    }
}

#[link(name = "iphlpapi")]
unsafe extern "system" {
    fn GetExtendedTcpTable(table: *mut c_void, size: *mut u32, order: i32, af: u32, class: i32, reserved: u32) -> u32;
    fn GetExtendedUdpTable(table: *mut c_void, size: *mut u32, order: i32, af: u32, class: i32, reserved: u32) -> u32;
    fn SetTcpEntry(row: *const TcpRow) -> u32;
}

#[repr(C)]
#[derive(Clone, Copy)]
struct TcpRow {
    state: u32,
    local_addr: u32,
    local_port: u32,
    remote_addr: u32,
    remote_port: u32,
}

fn table(tcp: bool, af: u32) -> Vec<u8> {
    let class = if tcp { 5 } else { 1 }; // TCP_TABLE_OWNER_PID_ALL / UDP_TABLE_OWNER_PID
    let mut size = 0u32;
    let mut buf = Vec::new();
    for _ in 0..5 {
        let r = unsafe {
            if tcp {
                GetExtendedTcpTable(if buf.is_empty() { std::ptr::null_mut() } else { buf.as_mut_ptr() as *mut c_void }, &mut size, 0, af, class, 0)
            } else {
                GetExtendedUdpTable(if buf.is_empty() { std::ptr::null_mut() } else { buf.as_mut_ptr() as *mut c_void }, &mut size, 0, af, class, 0)
            }
        };
        if r == 0 && !buf.is_empty() {
            return buf;
        }
        if r == 122 || buf.is_empty() {
            buf = vec![0u8; size as usize + 1024];
            continue;
        }
        break;
    }
    Vec::new()
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_ne_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn port(v: u32) -> u16 {
    u16::from_be(v as u16)
}
fn v4(v: u32) -> Ipv4Addr {
    Ipv4Addr::from(v.to_ne_bytes())
}
fn v6(b: &[u8], o: usize) -> Ipv6Addr {
    let mut a = [0u8; 16];
    a.copy_from_slice(&b[o..o + 16]);
    Ipv6Addr::from(a)
}

pub fn connections() -> Vec<Conn> {
    let mut out = Vec::new();
    let mk = |proto, local, remote, state, pid| Conn { proto, local, remote, state, pid, send_rate: 0.0, recv_rate: 0.0, total: 0 };
    let b = table(true, 2);
    if b.len() >= 4 {
        let n = u32_at(&b, 0) as usize;
        for i in 0..n {
            let o = 4 + i * 24;
            if o + 24 > b.len() {
                break;
            }
            let st = u32_at(&b, o);
            let l = SocketAddr::new(IpAddr::V4(v4(u32_at(&b, o + 4))), port(u32_at(&b, o + 8)));
            let r = SocketAddr::new(IpAddr::V4(v4(u32_at(&b, o + 12))), port(u32_at(&b, o + 16)));
            out.push(mk(Proto::Tcp, l, if st == 2 { None } else { Some(r) }, st, u32_at(&b, o + 20)));
        }
    }
    let b = table(true, 23);
    if b.len() >= 4 {
        let n = u32_at(&b, 0) as usize;
        for i in 0..n {
            let o = 4 + i * 56;
            if o + 56 > b.len() {
                break;
            }
            let l = SocketAddr::new(IpAddr::V6(v6(&b, o)), port(u32_at(&b, o + 20)));
            let r = SocketAddr::new(IpAddr::V6(v6(&b, o + 24)), port(u32_at(&b, o + 44)));
            let st = u32_at(&b, o + 48);
            out.push(mk(Proto::Tcp, l, if st == 2 { None } else { Some(r) }, st, u32_at(&b, o + 52)));
        }
    }
    let b = table(false, 2);
    if b.len() >= 4 {
        let n = u32_at(&b, 0) as usize;
        for i in 0..n {
            let o = 4 + i * 12;
            if o + 12 > b.len() {
                break;
            }
            let l = SocketAddr::new(IpAddr::V4(v4(u32_at(&b, o))), port(u32_at(&b, o + 4)));
            out.push(mk(Proto::Udp, l, None, 0, u32_at(&b, o + 8)));
        }
    }
    let b = table(false, 23);
    if b.len() >= 4 {
        let n = u32_at(&b, 0) as usize;
        for i in 0..n {
            let o = 4 + i * 28;
            if o + 28 > b.len() {
                break;
            }
            let l = SocketAddr::new(IpAddr::V6(v6(&b, o)), port(u32_at(&b, o + 20)));
            out.push(mk(Proto::Udp, l, None, 0, u32_at(&b, o + 24)));
        }
    }
    out
}

/// Forcibly closes an IPv4 TCP connection (needs admin).
pub fn close_tcp(local: SocketAddr, remote: SocketAddr) -> Result<(), String> {
    let (IpAddr::V4(l), IpAddr::V4(r)) = (local.ip(), remote.ip()) else {
        return Err("Windows can only close IPv4 connections".into());
    };
    let row = TcpRow {
        state: 12,
        local_addr: u32::from_ne_bytes(l.octets()),
        local_port: local.port().to_be() as u32,
        remote_addr: u32::from_ne_bytes(r.octets()),
        remote_port: remote.port().to_be() as u32,
    };
    match unsafe { SetTcpEntry(&row) } {
        0 => Ok(()),
        5 | 317 => Err("Needs admin".into()),
        e => Err(crate::win::error_text(e)),
    }
}

// ---------------------------------------------------------------- per-process bandwidth (ETW)

#[derive(Default)]
pub struct EtwTotals {
    /// pid -> (sent, received) bytes since last drain
    pub per_pid: HashMap<u32, (u64, u64)>,
    /// (pid, proto, local, remote) -> (sent, received) since last drain
    pub per_conn: HashMap<(u32, Proto, SocketAddr, SocketAddr), (u64, u64)>,
}

static ETW: OnceLock<Mutex<EtwTotals>> = OnceLock::new();
static ETW_RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn etw_running() -> bool {
    ETW_RUNNING.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn etw_drain() -> EtwTotals {
    match ETW.get() {
        Some(m) => std::mem::take(&mut *m.lock().unwrap()),
        None => EtwTotals::default(),
    }
}

// Microsoft-Windows-Kernel-Network {7DD42A49-5329-4832-8DFD-43D979153A88}
const KERNEL_NETWORK: windows_sys::core::GUID = windows_sys::core::GUID { data1: 0x7DD42A49, data2: 0x5329, data3: 0x4832, data4: [0x8D, 0xFD, 0x43, 0xD9, 0x79, 0x15, 0x3A, 0x88] };

unsafe extern "system" fn etw_callback(rec: *mut windows_sys::Win32::System::Diagnostics::Etw::EVENT_RECORD) {
    unsafe {
        let r = &*rec;
        let id = r.EventHeader.EventDescriptor.Id;
        // 10/11 TCP v4 send/recv, 26/27 TCP v6, 42/43 UDP v4, 58/59 UDP v6
        let (send, is_v6, proto) = match id {
            10 => (true, false, Proto::Tcp),
            11 => (false, false, Proto::Tcp),
            26 => (true, true, Proto::Tcp),
            27 => (false, true, Proto::Tcp),
            42 => (true, false, Proto::Udp),
            43 => (false, false, Proto::Udp),
            58 => (true, true, Proto::Udp),
            59 => (false, true, Proto::Udp),
            _ => return,
        };
        let need = if is_v6 { 44 } else { 20 };
        if (r.UserDataLength as usize) < need || r.UserData.is_null() {
            return;
        }
        let d = std::slice::from_raw_parts(r.UserData as *const u8, r.UserDataLength as usize);
        let pid = u32_at(d, 0);
        let size = u32_at(d, 4) as u64;
        let (da, sa, dp, sp) = if is_v6 {
            (IpAddr::V6(v6(d, 8)), IpAddr::V6(v6(d, 24)), u16::from_be_bytes([d[40], d[41]]), u16::from_be_bytes([d[42], d[43]]))
        } else {
            (IpAddr::V4(Ipv4Addr::new(d[8], d[9], d[10], d[11])), IpAddr::V4(Ipv4Addr::new(d[12], d[13], d[14], d[15])), u16::from_be_bytes([d[16], d[17]]), u16::from_be_bytes([d[18], d[19]]))
        };
        let (local, remote) = (SocketAddr::new(sa, sp), SocketAddr::new(da, dp));
        let Some(m) = ETW.get() else { return };
        let mut t = m.lock().unwrap();
        let e = t.per_pid.entry(pid).or_default();
        if send { e.0 += size } else { e.1 += size }
        let c = t.per_conn.entry((pid, proto, local, remote)).or_default();
        if send { c.0 += size } else { c.1 += size }
    }
}

/// Starts a real-time ETW session for kernel network events. Needs admin; returns false otherwise.
pub fn start_etw() -> bool {
    use windows_sys::Win32::System::Diagnostics::Etw::*;
    if etw_running() {
        return true;
    }
    ETW.get_or_init(|| Mutex::new(EtwTotals::default()));
    let name = wide("Sentinel Network Monitor");
    unsafe {
        let size = std::mem::size_of::<EVENT_TRACE_PROPERTIES>() + name.len() * 2 + 16;
        let mut buf = vec![0u8; size];
        let props = buf.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES;
        let init = |props: *mut EVENT_TRACE_PROPERTIES| {
            std::ptr::write_bytes(props as *mut u8, 0, size);
            (*props).Wnode.BufferSize = size as u32;
            (*props).Wnode.Flags = WNODE_FLAG_TRACED_GUID;
            (*props).Wnode.ClientContext = 1;
            (*props).LogFileMode = EVENT_TRACE_REAL_TIME_MODE;
            (*props).BufferSize = 64;
            (*props).MinimumBuffers = 4;
            (*props).MaximumBuffers = 16;
            (*props).FlushTimer = 1;
            (*props).LoggerNameOffset = std::mem::size_of::<EVENT_TRACE_PROPERTIES>() as u32;
        };
        // A session left over from a crash would block StartTrace.
        init(props);
        ControlTraceW(CONTROLTRACE_HANDLE { Value: 0 }, name.as_ptr(), props, EVENT_TRACE_CONTROL_STOP);
        init(props);
        let mut h = CONTROLTRACE_HANDLE { Value: 0 };
        if StartTraceW(&mut h, name.as_ptr(), props) != 0 {
            return false;
        }
        // IPv4 | IPv6 keywords
        if EnableTraceEx2(h, &KERNEL_NETWORK, EVENT_CONTROL_CODE_ENABLE_PROVIDER, 4, 0x30, 0, 0, std::ptr::null()) != 0 {
            ControlTraceW(h, std::ptr::null(), props, EVENT_TRACE_CONTROL_STOP);
            return false;
        }
        ETW_RUNNING.store(true, std::sync::atomic::Ordering::Relaxed);
        let name2 = name.clone();
        std::thread::Builder::new()
            .name("etw-net".into())
            .spawn(move || {
                let mut nm = name2;
                let mut lf: EVENT_TRACE_LOGFILEW = std::mem::zeroed();
                lf.LoggerName = nm.as_mut_ptr();
                lf.Anonymous1.ProcessTraceMode = PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD;
                lf.Anonymous2.EventRecordCallback = Some(etw_callback);
                let th = OpenTraceW(&mut lf);
                if th.Value != u64::MAX {
                    ProcessTrace(&th, 1, std::ptr::null(), std::ptr::null());
                    CloseTrace(th);
                }
                ETW_RUNNING.store(false, std::sync::atomic::Ordering::Relaxed);
            })
            .ok();
    }
    true
}

pub fn stop_etw() {
    use windows_sys::Win32::System::Diagnostics::Etw::*;
    let name = wide("Sentinel Network Monitor");
    unsafe {
        let size = std::mem::size_of::<EVENT_TRACE_PROPERTIES>() + name.len() * 2 + 16;
        let mut buf = vec![0u8; size];
        let props = buf.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES;
        (*props).Wnode.BufferSize = size as u32;
        (*props).LoggerNameOffset = std::mem::size_of::<EVENT_TRACE_PROPERTIES>() as u32;
        ControlTraceW(CONTROLTRACE_HANDLE { Value: 0 }, name.as_ptr(), props, EVENT_TRACE_CONTROL_STOP);
    }
}

// ---------------------------------------------------------------- reverse DNS

pub struct Rdns {
    cache: Mutex<HashMap<IpAddr, Option<String>>>,
    queue: Mutex<VecDeque<IpAddr>>,
    cv: std::sync::Condvar,
}

impl Rdns {
    pub fn new() -> Arc<Self> {
        let me = Arc::new(Self { cache: Mutex::new(HashMap::new()), queue: Mutex::new(VecDeque::new()), cv: std::sync::Condvar::new() });
        unsafe {
            let mut wsa: windows_sys::Win32::Networking::WinSock::WSADATA = std::mem::zeroed();
            windows_sys::Win32::Networking::WinSock::WSAStartup(0x0202, &mut wsa);
        }
        for i in 0..3 {
            let m = me.clone();
            std::thread::Builder::new()
                .name(format!("rdns-{i}"))
                .spawn(move || loop {
                    let ip = {
                        let mut q = m.queue.lock().unwrap();
                        while q.is_empty() {
                            q = m.cv.wait(q).unwrap();
                        }
                        q.pop_front().unwrap()
                    };
                    let name = reverse_lookup(ip);
                    m.cache.lock().unwrap().insert(ip, name);
                })
                .ok();
        }
        me
    }

    /// Cached hostname; schedules a lookup on first sight.
    pub fn get(&self, ip: IpAddr) -> Option<String> {
        if ip.is_loopback() || ip.is_unspecified() {
            return Some("localhost".into());
        }
        let mut c = self.cache.lock().unwrap();
        match c.get(&ip) {
            Some(v) => v.clone(),
            None => {
                c.insert(ip, None);
                drop(c);
                self.queue.lock().unwrap().push_back(ip);
                self.cv.notify_one();
                None
            }
        }
    }
}

fn reverse_lookup(ip: IpAddr) -> Option<String> {
    use windows_sys::Win32::Networking::WinSock::*;
    unsafe {
        let mut host = [0u16; 1025];
        let r = match ip {
            IpAddr::V4(a) => {
                let mut sa: SOCKADDR_IN = std::mem::zeroed();
                sa.sin_family = AF_INET;
                sa.sin_addr.S_un.S_addr = u32::from_ne_bytes(a.octets());
                GetNameInfoW(&sa as *const _ as *const SOCKADDR, std::mem::size_of::<SOCKADDR_IN>() as i32, host.as_mut_ptr(), host.len() as u32, std::ptr::null_mut(), 0, NI_NAMEREQD as i32)
            }
            IpAddr::V6(a) => {
                let mut sa: SOCKADDR_IN6 = std::mem::zeroed();
                sa.sin6_family = AF_INET6;
                sa.sin6_addr.u.Byte = a.octets();
                GetNameInfoW(&sa as *const _ as *const SOCKADDR, std::mem::size_of::<SOCKADDR_IN6>() as i32, host.as_mut_ptr(), host.len() as u32, std::ptr::null_mut(), 0, NI_NAMEREQD as i32)
            }
        };
        if r == 0 { Some(from_wide(&host)) } else { None }
    }
}

// ---------------------------------------------------------------- latency

/// Round-trip time in ms to an IPv4 address, or None on timeout/loss.
pub fn ping(ip: Ipv4Addr, timeout_ms: u32) -> Option<f32> {
    unsafe {
        let h = IcmpCreateFile();
        if h.is_null() || h == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
            return None;
        }
        let data = [0x50u8; 32];
        let mut reply = vec![0u8; std::mem::size_of::<ICMP_ECHO_REPLY>() + data.len() + 64];
        let t0 = std::time::Instant::now();
        let n = IcmpSendEcho(h, u32::from_ne_bytes(ip.octets()), data.as_ptr() as *const c_void, data.len() as u16, std::ptr::null(), reply.as_mut_ptr() as *mut c_void, reply.len() as u32, timeout_ms);
        let elapsed = t0.elapsed();
        IcmpCloseHandle(h);
        if n == 0 {
            return None;
        }
        let rep = &*(reply.as_ptr() as *const ICMP_ECHO_REPLY);
        if rep.Status != 0 {
            return None;
        }
        // RoundTripTime is whole ms; use the measured time for sub-ms precision on LAN.
        let rtt = rep.RoundTripTime as f32;
        Some(if rtt < 2.0 { (elapsed.as_secs_f32() * 1000.0).min(rtt + 1.0) } else { rtt })
    }
}

#[derive(Clone, Default)]
pub struct LatencySeries {
    pub label: String,
    pub target: String,
    pub samples: VecDeque<Option<f32>>,
}

impl LatencySeries {
    pub fn push(&mut self, v: Option<f32>) {
        self.samples.push_back(v);
        while self.samples.len() > 120 {
            self.samples.pop_front();
        }
    }
    pub fn last(&self) -> Option<f32> {
        self.samples.back().copied().flatten()
    }
    pub fn loss_pct(&self) -> f32 {
        let n = self.samples.len().min(60);
        if n == 0 {
            return 0.0;
        }
        let lost = self.samples.iter().rev().take(n).filter(|s| s.is_none()).count();
        lost as f32 * 100.0 / n as f32
    }
    pub fn avg(&self) -> Option<f32> {
        let v: Vec<f32> = self.samples.iter().rev().take(60).flatten().copied().collect();
        if v.is_empty() { None } else { Some(v.iter().sum::<f32>() / v.len() as f32) }
    }
    pub fn jitter(&self) -> Option<f32> {
        let v: Vec<f32> = self.samples.iter().rev().take(60).flatten().copied().collect();
        if v.len() < 2 {
            return None;
        }
        Some(v.windows(2).map(|w| (w[0] - w[1]).abs()).sum::<f32>() / (v.len() - 1) as f32)
    }
}

pub struct Latency {
    pub series: Mutex<Vec<LatencySeries>>,
    pub active: std::sync::atomic::AtomicBool,
    pub targets: Mutex<Vec<(String, String)>>,
}

impl Latency {
    pub fn new() -> Arc<Self> {
        let me = Arc::new(Self { series: Mutex::new(Vec::new()), active: false.into(), targets: Mutex::new(Vec::new()) });
        let m = me.clone();
        std::thread::Builder::new()
            .name("latency".into())
            .spawn(move || loop {
                if !m.active.load(std::sync::atomic::Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(400));
                    continue;
                }
                let t0 = std::time::Instant::now();
                let targets = m.targets.lock().unwrap().clone();
                let results: Vec<(String, String, Option<f32>)> = std::thread::scope(|s| {
                    let hs: Vec<_> = targets
                        .iter()
                        .map(|(label, target)| {
                            s.spawn(move || {
                                let v = target.parse::<Ipv4Addr>().ok().and_then(|ip| ping(ip, 900));
                                (label.clone(), target.clone(), v)
                            })
                        })
                        .collect();
                    hs.into_iter().filter_map(|h| h.join().ok()).collect()
                });
                {
                    let mut ser = m.series.lock().unwrap();
                    ser.retain(|s| targets.iter().any(|(_, t)| *t == s.target));
                    for (label, target, v) in results {
                        match ser.iter_mut().find(|s| s.target == target) {
                            Some(s) => {
                                s.label = label;
                                s.push(v)
                            }
                            None => {
                                let mut s = LatencySeries { label, target, samples: VecDeque::new() };
                                s.push(v);
                                ser.push(s);
                            }
                        }
                    }
                }
                let el = t0.elapsed();
                if el < Duration::from_secs(1) {
                    std::thread::sleep(Duration::from_secs(1) - el);
                }
            })
            .ok();
        me
    }
}
