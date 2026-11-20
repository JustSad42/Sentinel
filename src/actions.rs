//! Things you can do to a process.

use crate::config::Priority;
use crate::sampler::Proc;
use crate::win::{self, Handle, error_text, open_process, wide};
use std::collections::HashMap;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::RestartManager::*;
use windows_sys::Win32::System::Threading::*;

pub fn is_access_denied(e: &str) -> bool {
    e.contains("Access denied") || e.contains("denied")
}

fn open_checked(pid: u32, create_time: i64, access: u32) -> Result<Handle, String> {
    let h = open_process(pid, access | PROCESS_QUERY_LIMITED_INFORMATION).map_err(error_text)?;
    if create_time != 0 {
        if let Some(ct) = win::process_create_time(h.0) {
            if ct != create_time {
                return Err("The process already exited".into());
            }
        }
    }
    Ok(h)
}

pub fn kill(pid: u32, create_time: i64) -> Result<(), String> {
    if pid == std::process::id() {
        return Err("That's Sentinel itself".into());
    }
    let h = open_checked(pid, create_time, PROCESS_TERMINATE)?;
    if unsafe { TerminateProcess(h.0, 1) } == 0 {
        let e = win::last_error();
        // Already exiting counts as success.
        if e == ERROR_ACCESS_DENIED {
            let mut code = 0u32;
            if unsafe { GetExitCodeProcess(h.0, &mut code) } != 0 && code != STILL_ACTIVE as u32 {
                return Ok(());
            }
        }
        return Err(error_text(e));
    }
    Ok(())
}

/// The process and all its descendants, children first.
pub fn tree_of(root: (u32, i64), procs: &[Proc]) -> Vec<(u32, i64)> {
    let mut children: HashMap<u32, Vec<&Proc>> = HashMap::new();
    let ct: HashMap<u32, i64> = procs.iter().map(|p| (p.pid, p.create_time)).collect();
    for p in procs {
        // A parent must be older than its child, otherwise the PID was reused.
        if ct.get(&p.ppid).is_some_and(|&pc| pc <= p.create_time) && p.ppid != p.pid {
            children.entry(p.ppid).or_default().push(p);
        }
    }
    let mut out = Vec::new();
    fn walk(pid: u32, ct: i64, ch: &HashMap<u32, Vec<&Proc>>, out: &mut Vec<(u32, i64)>, depth: usize) {
        if depth > 64 {
            return;
        }
        if let Some(kids) = ch.get(&pid) {
            for k in kids {
                walk(k.pid, k.create_time, ch, out, depth + 1);
            }
        }
        out.push((pid, ct));
    }
    walk(root.0, root.1, &children, &mut out, 0);
    out
}

pub fn kill_many(list: &[(u32, i64)]) -> (usize, Vec<String>) {
    let mut ok = 0;
    let mut errs = Vec::new();
    for &(pid, ct) in list {
        match kill(pid, ct) {
            Ok(()) => ok += 1,
            Err(e) => errs.push(e),
        }
    }
    (ok, errs)
}

pub fn set_priority(pid: u32, p: Priority) -> Result<(), String> {
    let h = open_process(pid, PROCESS_SET_INFORMATION).map_err(error_text)?;
    if unsafe { SetPriorityClass(h.0, p.class()) } == 0 { Err(error_text(win::last_error())) } else { Ok(()) }
}

pub fn get_affinity(pid: u32) -> Option<(u64, u64)> {
    let h = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION).ok()?;
    let (mut pm, mut sm) = (0usize, 0usize);
    if unsafe { GetProcessAffinityMask(h.0, &mut pm, &mut sm) } == 0 { None } else { Some((pm as u64, sm as u64)) }
}

pub fn set_affinity(pid: u32, mask: u64) -> Result<(), String> {
    if mask == 0 {
        return Err("Pick at least one core".into());
    }
    let h = open_process(pid, PROCESS_SET_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION).map_err(error_text)?;
    let sys = get_affinity(pid).map(|x| x.1).unwrap_or(u64::MAX);
    if unsafe { SetProcessAffinityMask(h.0, (mask & sys) as usize) } == 0 { Err(error_text(win::last_error())) } else { Ok(()) }
}

/// Restart: end the process and start it again with the same command line.
pub fn restart(p: &Proc) -> Result<(), String> {
    let path = p.path.as_deref().ok_or("Path unknown")?.to_string();
    let cmd = win::command_line(p.pid).unwrap_or_default();
    let args = split_args(&cmd).1;
    let is_explorer = p.name.eq_ignore_ascii_case("explorer.exe");
    kill(p.pid, p.create_time)?;
    if let Ok(h) = open_process(p.pid, 0x0010_0000) {
        unsafe { WaitForSingleObject(h.0, 3000) };
    }
    if is_explorer {
        // Windows restarts the shell itself in most cases.
        std::thread::sleep(std::time::Duration::from_millis(1500));
        if is_running("explorer.exe") {
            return Ok(());
        }
    }
    win::launch_as_user(&path, &args, None)
}

fn is_running(name: &str) -> bool {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("tasklist")
        .creation_flags(0x0800_0000).args(["/FI", &format!("IMAGENAME eq {name}"), "/NH"]).output().map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase().contains(&name.to_lowercase())).unwrap_or(false)
}

/// Splits a command line into (program, rest).
pub fn split_args(cmd: &str) -> (String, String) {
    let c = cmd.trim();
    if let Some(rest) = c.strip_prefix('"') {
        if let Some(end) = rest.find('"') {
            return (rest[..end].to_string(), rest[end + 1..].trim().to_string());
        }
    }
    let lower = c.to_ascii_lowercase();
    if let Some(i) = lower.find(".exe") {
        let end = i + 4;
        return (c[..end].to_string(), c[end..].trim().to_string());
    }
    match c.split_once(' ') {
        Some((a, b)) => (a.to_string(), b.trim().to_string()),
        None => (c.to_string(), String::new()),
    }
}

// ---------------------------------------------------------------- file lockers (Restart Manager)

#[derive(Clone)]
pub struct Locker {
    pub pid: u32,
    pub name: String,
    pub service: String,
    pub create_time: i64,
}

/// Which processes hold a file (or any file in a folder) open.
pub fn lockers(path: &str) -> Result<Vec<Locker>, String> {
    let mut files: Vec<String> = Vec::new();
    let p = std::path::Path::new(path);
    if p.is_dir() {
        let mut stack = vec![p.to_path_buf()];
        while let Some(d) = stack.pop() {
            if let Ok(rd) = std::fs::read_dir(&d) {
                for e in rd.flatten() {
                    let ep = e.path();
                    if ep.is_dir() {
                        stack.push(ep);
                    } else {
                        files.push(ep.to_string_lossy().to_string());
                    }
                    if files.len() >= 5000 {
                        break;
                    }
                }
            }
        }
    } else {
        files.push(path.to_string());
    }
    if files.is_empty() {
        return Ok(Vec::new());
    }
    unsafe {
        let mut session = 0u32;
        let mut key = [0u16; CCH_RM_SESSION_KEY as usize + 1];
        let r = RmStartSession(&mut session, 0, key.as_mut_ptr());
        if r != 0 {
            return Err(error_text(r));
        }
        let wides: Vec<Vec<u16>> = files.iter().map(|f| wide(f)).collect();
        let ptrs: Vec<*const u16> = wides.iter().map(|w| w.as_ptr()).collect();
        let mut out = Vec::new();
        let mut err = None;
        for chunk in ptrs.chunks(500) {
            let r = RmRegisterResources(session, chunk.len() as u32, chunk.as_ptr(), 0, std::ptr::null(), 0, std::ptr::null());
            if r != 0 {
                err = Some(error_text(r));
                break;
            }
        }
        if err.is_none() {
            let mut needed = 0u32;
            let mut count;
            let mut reasons = 0u32;
            let mut list: Vec<RM_PROCESS_INFO> = Vec::new();
            loop {
                count = list.len() as u32;
                let r = RmGetList(session, &mut needed, &mut count, if list.is_empty() { std::ptr::null_mut() } else { list.as_mut_ptr() }, &mut reasons);
                if r == ERROR_MORE_DATA {
                    list = vec![std::mem::zeroed(); needed as usize + 4];
                    continue;
                }
                if r != 0 {
                    err = Some(error_text(r));
                }
                break;
            }
            for i in 0..(count as usize).min(list.len()) {
                let pi = &list[i];
                let ft = pi.Process.ProcessStartTime;
                out.push(Locker {
                    pid: pi.Process.dwProcessId,
                    name: win::from_wide(&pi.strAppName),
                    service: win::from_wide(&pi.strServiceShortName),
                    create_time: ((ft.dwHighDateTime as i64) << 32) | ft.dwLowDateTime as i64,
                });
            }
        }
        RmEndSession(session);
        match err {
            Some(e) => Err(e),
            None => Ok(out),
        }
    }
}
