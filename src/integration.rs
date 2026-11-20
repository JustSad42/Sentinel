//! OS integration: single instance, "show" signalling, the Ctrl+Shift+Esc keyboard hook, autostart.

use crate::win::{self, Handle, wide};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Security::Authorization::*;
use windows_sys::Win32::Security::*;
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

fn name(base: &str) -> String {
    format!("Local\\Sentinel.{base}{}", if crate::config::test_mode() { ".test" } else { "" })
}

/// Security attributes that let both elevated and non-elevated copies open our objects.
struct OpenSa {
    sa: SECURITY_ATTRIBUTES,
}
impl OpenSa {
    fn new() -> Option<Self> {
        let sddl = wide("D:(A;;GA;;;WD)(A;;GA;;;AU)S:(ML;;NW;;;LW)");
        let mut psd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        let ok = unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), SDDL_REVISION_1, &mut psd, std::ptr::null_mut()) };
        if ok == 0 {
            return None;
        }
        Some(Self { sa: SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: psd, bInheritHandle: 0 } })
    }
    fn ptr(this: &Option<Self>) -> *const SECURITY_ATTRIBUTES {
        this.as_ref().map(|s| &s.sa as *const _).unwrap_or(std::ptr::null())
    }
}

pub struct Instance {
    _mutex: Handle,
    pub show_event: Handle,
}
unsafe impl Send for Instance {}
unsafe impl Sync for Instance {}

/// Becomes the single running instance. With `wait`, keeps trying for a few seconds (used after
/// "restart as admin" and self-update, where the old copy is still shutting down).
pub fn acquire(wait: bool) -> Option<Instance> {
    let sa = OpenSa::new();
    let mname = wide(&name("Instance"));
    let ename = wide(&name("Show"));
    let tries = if wait { 100 } else { 1 };
    for i in 0..tries {
        let m = unsafe { CreateMutexW(OpenSa::ptr(&sa), 0, mname.as_ptr()) };
        let err = win::last_error();
        if !m.is_null() && err != ERROR_ALREADY_EXISTS {
            let e = unsafe { CreateEventW(OpenSa::ptr(&sa), 0, 0, ename.as_ptr()) };
            return Some(Instance { _mutex: Handle(m), show_event: Handle(e) });
        }
        if !m.is_null() {
            unsafe { CloseHandle(m) };
        }
        if i + 1 < tries {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    None
}

/// Asks the running instance to show itself. Returns false if none is listening.
pub fn signal_existing() -> bool {
    unsafe {
        AllowSetForegroundWindow(ASFW_ANY);
        let ename = wide(&name("Show"));
        let e = OpenEventW(EVENT_MODIFY_STATE, 0, ename.as_ptr());
        if e.is_null() {
            return false;
        }
        let e = Handle(e);
        SetEvent(e.0) != 0
    }
}

static SHOW_EVENT: OnceLock<usize> = OnceLock::new();

/// Waits for show requests (second launch, IFEO, hook) and brings the window up.
pub fn start_show_listener(inst: &Instance, hwnd: isize, ctx: egui::Context, flag: std::sync::Arc<AtomicBool>) {
    let ev = inst.show_event.0 as usize;
    SHOW_EVENT.get_or_init(|| ev);
    std::thread::Builder::new()
        .name("show-listener".into())
        .spawn(move || loop {
            let r = unsafe { WaitForSingleObject(ev as HANDLE, INFINITE) };
            if r != WAIT_OBJECT_0 {
                std::thread::sleep(std::time::Duration::from_millis(500));
                continue;
            }
            flag.store(true, Ordering::SeqCst);
            win::force_foreground(hwnd as HWND);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            ctx.request_repaint();
        })
        .ok();
}

// ---------------------------------------------------------------- keyboard hook

pub static HOOK_ENABLED: AtomicBool = AtomicBool::new(true);
static SWALLOW_ESC_UP: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn ll_keyboard(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        if code == HC_ACTION as i32 && HOOK_ENABLED.load(Ordering::Relaxed) {
            let kb = &*(lparam as *const KBDLLHOOKSTRUCT);
            if kb.vkCode == VK_ESCAPE as u32 {
                let down = wparam == WM_KEYDOWN as usize || wparam == WM_SYSKEYDOWN as usize;
                if down {
                    let held = |vk: u16| GetAsyncKeyState(vk as i32) as u16 & 0x8000 != 0;
                    if held(VK_CONTROL) && held(VK_SHIFT) && !held(VK_MENU) && !held(VK_LWIN) && !held(VK_RWIN) {
                        SWALLOW_ESC_UP.store(true, Ordering::Relaxed);
                        if let Some(&ev) = SHOW_EVENT.get() {
                            SetEvent(ev as HANDLE);
                        }
                        return 1;
                    }
                } else if SWALLOW_ESC_UP.swap(false, Ordering::Relaxed) {
                    return 1;
                }
            }
        }
        CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
    }
}

/// Installs a low-level keyboard hook on its own thread so a busy UI never delays system input.
pub fn start_hook() {
    std::thread::Builder::new()
        .name("kb-hook".into())
        .spawn(|| unsafe {
            let hmod = windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(std::ptr::null());
            let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(ll_keyboard), hmod, 0);
            if hook.is_null() {
                return;
            }
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            UnhookWindowsHookEx(hook);
        })
        .ok();
}

// ---------------------------------------------------------------- autostart

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

pub fn exe_path() -> String {
    std::env::current_exe().map(|p| p.to_string_lossy().to_string()).unwrap_or_default()
}

/// A dev build (target\debug|release) must never register itself.
pub fn is_dev_build() -> bool {
    let p = exe_path().to_ascii_lowercase();
    p.contains("\\target\\") || crate::config::test_mode()
}

/// Seconds since this user's desktop (first explorer.exe) started, if it's running.
pub fn secs_since_sign_in() -> Option<f64> {
    use windows_sys::Win32::System::Diagnostics::ToolHelp::*;
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return None;
        }
        let snap = Handle(snap);
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut earliest: Option<i64> = None;
        let mut ok = Process32FirstW(snap.0, &mut e);
        while ok != 0 {
            let len = e.szExeFile.iter().position(|c| *c == 0).unwrap_or(e.szExeFile.len());
            if String::from_utf16_lossy(&e.szExeFile[..len]).eq_ignore_ascii_case("explorer.exe") {
                let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, e.th32ProcessID);
                if !h.is_null() {
                    let h = Handle(h);
                    if let Some(t) = win::process_create_time(h.0) {
                        earliest = Some(earliest.map_or(t, |x: i64| x.min(t)));
                    }
                }
            }
            ok = Process32NextW(snap.0, &mut e);
        }
        let mut now = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        windows_sys::Win32::System::SystemInformation::GetSystemTimeAsFileTime(&mut now);
        let now = ((now.dwHighDateTime as i64) << 32) | now.dwLowDateTime as i64;
        earliest.map(|t| (now - t) as f64 / 1e7)
    }
}

/// Milliseconds since the last keyboard or mouse input in this session.
pub fn idle_ms() -> u64 {
    unsafe {
        let mut li = LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
        if GetLastInputInfo(&mut li) == 0 {
            return u64::MAX;
        }
        windows_sys::Win32::System::SystemInformation::GetTickCount().wrapping_sub(li.dwTime) as u64
    }
}

/// True for a launch nobody asked for: in the first two minutes after sign-in, with no key press or
/// click just before it. Windows' app restore (or Task Manager being reopened through the
/// "Replace Task Manager" redirect) does this; Sentinel then stays hidden instead of popping up.
pub fn sign_in_launch() -> bool {
    secs_since_sign_in().is_none_or(|s| s < 120.0) && idle_ms() > 1500
}

/// Appends one line to launch.log (kept short) so unexpected launches can be traced.
pub fn log_launch(line: &str) {
    let p = crate::config::data_dir().join("launch.log");
    let mut lines: Vec<String> = std::fs::read_to_string(&p).map(|s| s.lines().map(String::from).collect()).unwrap_or_default();
    let (y, mo, d, h, mi, s) = crate::fmt::civil(win::now_unix());
    lines.push(format!("{y}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}  {line}"));
    let skip = lines.len().saturating_sub(200);
    let _ = std::fs::write(&p, lines[skip..].join("\n") + "\n");
}

// ---------------------------------------------------------------- "What's using this?" in Explorer

const MENU_KEYS: [&str; 2] = [r"Software\Classes\*\shell\SentinelLocks", r"Software\Classes\Directory\shell\SentinelLocks"];

/// Adds (or removes) "What's using this? (Sentinel)" to the right-click menu of files and folders.
/// Per-user (HKCU), so no admin rights. On Windows 11 it sits under "Show more options".
pub fn set_explorer_menu(enable: bool) -> Result<(), String> {
    let hk = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    for k in MENU_KEYS {
        if enable {
            let (key, _) = hk.create_subkey(k).map_err(|e| e.to_string())?;
            key.set_value("MUIVerb", &"What's using this? (Sentinel)").map_err(|e| e.to_string())?;
            key.set_value("Icon", &format!("\"{}\",0", exe_path())).map_err(|e| e.to_string())?;
            let (cmd, _) = key.create_subkey("command").map_err(|e| e.to_string())?;
            cmd.set_value("", &format!("\"{}\" --locks \"%1\"", exe_path())).map_err(|e| e.to_string())?;
        } else {
            let _ = hk.delete_subkey_all(k);
        }
    }
    Ok(())
}

pub fn explorer_menu_enabled() -> bool {
    winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER).open_subkey(format!("{}\\command", MENU_KEYS[0])).is_ok()
}

fn locks_request_path() -> std::path::PathBuf {
    crate::config::data_dir().join("locks-request")
}

/// `--locks <path>`: leave the path for the running (elevated) copy to pick up when it shows.
pub fn request_locks(path: &str) {
    let _ = std::fs::write(locks_request_path(), path.as_bytes());
}

pub fn take_locks_request() -> Option<String> {
    let p = locks_request_path();
    let fresh = std::fs::metadata(&p).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|d| d.as_secs() < 30);
    let s = std::fs::read_to_string(&p).ok();
    let _ = std::fs::remove_file(&p);
    s.filter(|s| fresh && !s.trim().is_empty())
}

fn show_flag_path() -> std::path::PathBuf {
    crate::config::data_dir().join("show-on-start")
}

/// Tells the elevated copy (started through the task, always with --background) to open its window.
pub fn request_show_on_start() {
    let _ = std::fs::write(show_flag_path(), b"1");
}

pub fn take_show_on_start() -> bool {
    let p = show_flag_path();
    let fresh = std::fs::metadata(&p).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|d| d.as_secs() < 30);
    let _ = std::fs::remove_file(&p);
    fresh
}

pub fn autostart_enabled() -> bool {
    winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER).open_subkey(RUN).and_then(|k| k.get_value::<String, _>("Sentinel")).is_ok()
}

pub fn set_autostart(enable: bool) -> Result<(), String> {
    let (k, _) = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER).create_subkey(RUN).map_err(|e| e.to_string())?;
    if enable {
        k.set_value("Sentinel", &format!("\"{}\" --background", exe_path())).map_err(|e| e.to_string())
    } else {
        match k.delete_value("Sentinel") {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}
