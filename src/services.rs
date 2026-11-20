//! Windows services: list, start/stop/restart, startup type.

use crate::admin::ServiceAction;
use crate::win::{error_text, from_pwstr, last_error, wide};
use windows_sys::Win32::System::Services::*;

#[derive(Clone, Default)]
pub struct Service {
    pub name: String,
    pub display: String,
    pub state: u32,
    pub pid: u32,
    pub start_type: u32,
    pub delayed: bool,
    pub description: String,
    pub binary: String,
    pub account: String,
}

impl Service {
    pub fn state_label(&self) -> &'static str {
        match self.state {
            SERVICE_RUNNING => "Running",
            SERVICE_STOPPED => "Stopped",
            SERVICE_START_PENDING => "Starting",
            SERVICE_STOP_PENDING => "Stopping",
            SERVICE_PAUSED => "Paused",
            SERVICE_PAUSE_PENDING => "Pausing",
            SERVICE_CONTINUE_PENDING => "Resuming",
            _ => "Unknown",
        }
    }
    pub fn running(&self) -> bool {
        self.state == SERVICE_RUNNING
    }
}

pub fn start_type_label(t: u32, delayed: bool) -> &'static str {
    match t {
        SERVICE_AUTO_START if delayed => "Automatic (delayed)",
        SERVICE_AUTO_START => "Automatic",
        SERVICE_DEMAND_START => "Manual",
        SERVICE_DISABLED => "Disabled",
        SERVICE_BOOT_START => "Boot",
        SERVICE_SYSTEM_START => "System",
        _ => "",
    }
}

struct Scm(SC_HANDLE);
impl Drop for Scm {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CloseServiceHandle(self.0) };
        }
    }
}

fn manager(access: u32) -> Result<Scm, String> {
    let h = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), access) };
    if h.is_null() { Err(error_text(last_error())) } else { Ok(Scm(h)) }
}

/// Fast list: names, state and PID only.
pub fn list_basic() -> Vec<Service> {
    let mut out = Vec::new();
    let Ok(scm) = manager(SC_MANAGER_ENUMERATE_SERVICE) else { return out };
    unsafe {
        let mut needed = 0u32;
        let mut count = 0u32;
        let mut resume = 0u32;
        let mut buf: Vec<u8> = vec![0u8; 256 * 1024];
        loop {
            let ok = EnumServicesStatusExW(scm.0, SC_ENUM_PROCESS_INFO, SERVICE_WIN32, SERVICE_STATE_ALL, buf.as_mut_ptr(), buf.len() as u32, &mut needed, &mut count, &mut resume, std::ptr::null());
            let items = buf.as_ptr() as *const ENUM_SERVICE_STATUS_PROCESSW;
            for i in 0..count as usize {
                let it = &*items.add(i);
                out.push(Service {
                    name: from_pwstr(it.lpServiceName),
                    display: from_pwstr(it.lpDisplayName),
                    state: it.ServiceStatusProcess.dwCurrentState,
                    pid: it.ServiceStatusProcess.dwProcessId,
                    ..Default::default()
                });
            }
            if ok != 0 {
                break;
            }
            if last_error() == 234 {
                // ERROR_MORE_DATA: continue from `resume`
                if needed as usize > buf.len() {
                    buf.resize(needed as usize, 0);
                }
                continue;
            }
            break;
        }
    }
    out
}

/// Full list with start type, binary and account (slower; run off the UI thread).
pub fn list_full() -> Vec<Service> {
    let mut list = list_basic();
    let Ok(scm) = manager(SC_MANAGER_ENUMERATE_SERVICE | SC_MANAGER_CONNECT) else { return list };
    unsafe {
        for s in list.iter_mut() {
            let n = wide(&s.name);
            let h = OpenServiceW(scm.0, n.as_ptr(), SERVICE_QUERY_CONFIG);
            if h.is_null() {
                continue;
            }
            let h = Scm(h);
            let mut needed = 0u32;
            QueryServiceConfigW(h.0, std::ptr::null_mut(), 0, &mut needed);
            if needed > 0 {
                let mut buf = vec![0u8; needed as usize];
                if QueryServiceConfigW(h.0, buf.as_mut_ptr() as *mut QUERY_SERVICE_CONFIGW, needed, &mut needed) != 0 {
                    let c = &*(buf.as_ptr() as *const QUERY_SERVICE_CONFIGW);
                    s.start_type = c.dwStartType;
                    s.binary = from_pwstr(c.lpBinaryPathName);
                    s.account = from_pwstr(c.lpServiceStartName);
                }
            }
            if s.start_type == SERVICE_AUTO_START {
                let mut d = SERVICE_DELAYED_AUTO_START_INFO { fDelayedAutostart: 0 };
                let mut n2 = 0u32;
                if QueryServiceConfig2W(h.0, SERVICE_CONFIG_DELAYED_AUTO_START_INFO, &mut d as *mut _ as *mut u8, std::mem::size_of::<SERVICE_DELAYED_AUTO_START_INFO>() as u32, &mut n2) != 0 {
                    s.delayed = d.fDelayedAutostart != 0;
                }
            }
        }
    }
    list
}

pub fn description(name: &str) -> String {
    let Ok(scm) = manager(SC_MANAGER_CONNECT) else { return String::new() };
    unsafe {
        let n = wide(name);
        let h = OpenServiceW(scm.0, n.as_ptr(), SERVICE_QUERY_CONFIG);
        if h.is_null() {
            return String::new();
        }
        let h = Scm(h);
        let mut needed = 0u32;
        QueryServiceConfig2W(h.0, SERVICE_CONFIG_DESCRIPTION, std::ptr::null_mut(), 0, &mut needed);
        if needed == 0 {
            return String::new();
        }
        let mut buf = vec![0u8; needed as usize];
        if QueryServiceConfig2W(h.0, SERVICE_CONFIG_DESCRIPTION, buf.as_mut_ptr(), needed, &mut needed) == 0 {
            return String::new();
        }
        let d = &*(buf.as_ptr() as *const SERVICE_DESCRIPTIONW);
        let s = from_pwstr(d.lpDescription);
        if s.starts_with('@') { String::new() } else { s }
    }
}

fn wait_state(h: SC_HANDLE, want: u32, ms: u64) -> bool {
    let t0 = std::time::Instant::now();
    unsafe {
        loop {
            let mut st: SERVICE_STATUS = std::mem::zeroed();
            if QueryServiceStatus(h, &mut st) == 0 {
                return false;
            }
            if st.dwCurrentState == want {
                return true;
            }
            if t0.elapsed().as_millis() as u64 > ms {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
    }
}

pub fn control(name: &str, action: &ServiceAction) -> Result<(), String> {
    let scm = manager(SC_MANAGER_CONNECT)?;
    unsafe {
        let n = wide(name);
        let access = SERVICE_START | SERVICE_STOP | SERVICE_QUERY_STATUS | if matches!(action, ServiceAction::StartType(_)) { SERVICE_CHANGE_CONFIG } else { 0 };
        let h = OpenServiceW(scm.0, n.as_ptr(), access);
        if h.is_null() {
            return Err(error_text(last_error()));
        }
        let h = Scm(h);
        let stop = |h: &Scm| -> Result<(), String> {
            let mut st: SERVICE_STATUS = std::mem::zeroed();
            if ControlService(h.0, SERVICE_CONTROL_STOP, &mut st) == 0 {
                let e = last_error();
                if e != 1062 {
                    // ERROR_SERVICE_NOT_ACTIVE
                    return Err(error_text(e));
                }
            }
            if !wait_state(h.0, SERVICE_STOPPED, 20_000) {
                return Err("The service did not stop in time".into());
            }
            Ok(())
        };
        let start = |h: &Scm| -> Result<(), String> {
            if StartServiceW(h.0, 0, std::ptr::null()) == 0 {
                let e = last_error();
                if e != 1056 {
                    // ERROR_SERVICE_ALREADY_RUNNING
                    return Err(error_text(e));
                }
            }
            wait_state(h.0, SERVICE_RUNNING, 20_000);
            Ok(())
        };
        match action {
            ServiceAction::Start => start(&h),
            ServiceAction::Stop => stop(&h),
            ServiceAction::Restart => {
                stop(&h)?;
                start(&h)
            }
            ServiceAction::StartType(t) => {
                let (ty, delayed) = if *t == 0x100 { (SERVICE_AUTO_START, true) } else { (*t, false) };
                if ChangeServiceConfigW(h.0, SERVICE_NO_CHANGE, ty, SERVICE_NO_CHANGE, std::ptr::null(), std::ptr::null(), std::ptr::null_mut(), std::ptr::null(), std::ptr::null(), std::ptr::null(), std::ptr::null()) == 0 {
                    return Err(error_text(last_error()));
                }
                if ty == SERVICE_AUTO_START {
                    let mut d = SERVICE_DELAYED_AUTO_START_INFO { fDelayedAutostart: delayed as _ };
                    ChangeServiceConfig2W(h.0, SERVICE_CONFIG_DELAYED_AUTO_START_INFO, &mut d as *mut _ as *mut core::ffi::c_void);
                }
                Ok(())
            }
        }
    }
}
