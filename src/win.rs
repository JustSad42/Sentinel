//! Thin, safe-ish wrappers over the Win32 / NT APIs the app needs.

use std::ffi::c_void;
use std::path::Path;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::Security::*;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// # Safety
/// `p` must be null or point to a NUL-terminated UTF-16 string.
pub unsafe fn from_pwstr(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut len = 0;
    unsafe {
        while *p.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
    }
}

/// Owned kernel handle, closed on drop.
pub struct Handle(pub HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe { CloseHandle(self.0) };
        }
    }
}
unsafe impl Send for Handle {}

pub fn open_process(pid: u32, access: u32) -> Result<Handle, u32> {
    let h = unsafe { OpenProcess(access, 0, pid) };
    if h.is_null() { Err(unsafe { GetLastError() }) } else { Ok(Handle(h)) }
}

pub fn last_error() -> u32 {
    unsafe { GetLastError() }
}

pub fn error_text(code: u32) -> String {
    match code {
        ERROR_ACCESS_DENIED => "Access denied".into(),
        ERROR_INVALID_PARAMETER => "The process no longer exists".into(),
        _ => {
            let mut buf = [0u16; 512];
            let n = unsafe {
                windows_sys::Win32::System::Diagnostics::Debug::FormatMessageW(
                    windows_sys::Win32::System::Diagnostics::Debug::FORMAT_MESSAGE_FROM_SYSTEM
                        | windows_sys::Win32::System::Diagnostics::Debug::FORMAT_MESSAGE_IGNORE_INSERTS,
                    std::ptr::null(),
                    code,
                    0,
                    buf.as_mut_ptr(),
                    buf.len() as u32,
                    std::ptr::null(),
                )
            };
            if n == 0 { format!("Error {code}") } else { from_wide(&buf[..n as usize]).trim().to_string() }
        }
    }
}

pub fn process_path(pid: u32) -> Option<String> {
    let h = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION).ok()?;
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    let ok = unsafe { QueryFullProcessImageNameW(h.0, 0, buf.as_mut_ptr(), &mut len) };
    if ok == 0 { None } else { Some(String::from_utf16_lossy(&buf[..len as usize])) }
}

/// Creation time (FILETIME as i64) of a live process, used to confirm a PID still refers to the same process.
pub fn process_create_time(h: HANDLE) -> Option<i64> {
    let mut c = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let mut e = c;
    let mut k = c;
    let mut u = c;
    let ok = unsafe { GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u) };
    if ok == 0 { None } else { Some(((c.dwHighDateTime as i64) << 32) | c.dwLowDateTime as i64) }
}

pub fn is_elevated() -> bool {
    unsafe {
        let mut tok: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok) == 0 {
            return false;
        }
        let tok = Handle(tok);
        let mut el = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut ret = 0u32;
        let ok = GetTokenInformation(tok.0, TokenElevation, &mut el as *mut _ as *mut c_void, std::mem::size_of::<TOKEN_ELEVATION>() as u32, &mut ret);
        ok != 0 && el.TokenIsElevated != 0
    }
}

pub fn process_is_elevated(pid: u32) -> Option<bool> {
    unsafe {
        let h = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION).ok()?;
        let mut tok: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(h.0, TOKEN_QUERY, &mut tok) == 0 {
            return None;
        }
        let tok = Handle(tok);
        let mut el = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut ret = 0u32;
        let ok = GetTokenInformation(tok.0, TokenElevation, &mut el as *mut _ as *mut c_void, std::mem::size_of::<TOKEN_ELEVATION>() as u32, &mut ret);
        if ok == 0 { None } else { Some(el.TokenIsElevated != 0) }
    }
}

/// "DOMAIN\\user" → just "user" for display.
pub fn process_user(pid: u32) -> Option<String> {
    unsafe {
        let h = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION).ok()?;
        let mut tok: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(h.0, TOKEN_QUERY, &mut tok) == 0 {
            return None;
        }
        let tok = Handle(tok);
        let mut buf = vec![0u8; 256];
        let mut ret = 0u32;
        if GetTokenInformation(tok.0, TokenUser, buf.as_mut_ptr() as *mut c_void, buf.len() as u32, &mut ret) == 0 {
            return None;
        }
        let tu = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut name = [0u16; 256];
        let mut dom = [0u16; 256];
        let mut nl = name.len() as u32;
        let mut dl = dom.len() as u32;
        let mut use_ = 0;
        if LookupAccountSidW(std::ptr::null(), tu.User.Sid, name.as_mut_ptr(), &mut nl, dom.as_mut_ptr(), &mut dl, &mut use_) == 0 {
            return None;
        }
        Some(from_wide(&name))
    }
}

// ---------------------------------------------------------------- ntdll

#[repr(C)]
struct UnicodeString {
    length: u16,
    max_length: u16,
    buffer: *mut u16,
}

#[link(name = "ntdll")]
unsafe extern "system" {
    pub fn NtQuerySystemInformation(class: u32, buf: *mut c_void, len: u32, ret: *mut u32) -> i32;
    fn NtQueryInformationProcess(h: HANDLE, class: u32, buf: *mut c_void, len: u32, ret: *mut u32) -> i32;
    fn NtSuspendProcess(h: HANDLE) -> i32;
    fn NtResumeProcess(h: HANDLE) -> i32;
}

pub fn command_line(pid: u32) -> Option<String> {
    let h = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION).ok()?;
    let mut buf = vec![0u8; 4096];
    loop {
        let mut ret = 0u32;
        // ProcessCommandLineInformation = 60 (Windows 8.1+)
        let st = unsafe { NtQueryInformationProcess(h.0, 60, buf.as_mut_ptr() as *mut c_void, buf.len() as u32, &mut ret) };
        if st == 0 {
            let us = unsafe { &*(buf.as_ptr() as *const UnicodeString) };
            if us.buffer.is_null() {
                return None;
            }
            let s = unsafe { std::slice::from_raw_parts(us.buffer, us.length as usize / 2) };
            return Some(String::from_utf16_lossy(s));
        }
        // STATUS_INFO_LENGTH_MISMATCH / BUFFER_OVERFLOW / BUFFER_TOO_SMALL
        if (st as u32 == 0xC0000004 || st as u32 == 0x80000005 || st as u32 == 0xC0000023) && ret as usize > buf.len() && ret < 1 << 20 {
            buf.resize(ret as usize + 16, 0);
            continue;
        }
        return None;
    }
}

pub fn suspend(pid: u32, resume: bool) -> Result<(), String> {
    let h = open_process(pid, PROCESS_SUSPEND_RESUME).map_err(error_text)?;
    let st = unsafe { if resume { NtResumeProcess(h.0) } else { NtSuspendProcess(h.0) } };
    if st == 0 { Ok(()) } else { Err(format!("NTSTATUS 0x{:08X}", st as u32)) }
}

// ---------------------------------------------------------------- files / shell

pub fn expand_env(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let w = wide(s);
    let mut buf = vec![0u16; 2048];
    let n = unsafe { windows_sys::Win32::System::Environment::ExpandEnvironmentStringsW(w.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
    if n == 0 || n as usize > buf.len() { s.to_string() } else { from_wide(&buf) }
}

/// FileDescription and CompanyName from an executable's version resource.
pub fn version_strings(path: &str) -> (Option<String>, Option<String>) {
    unsafe {
        let w = wide(path);
        let mut dummy = 0;
        let size = GetFileVersionInfoSizeW(w.as_ptr(), &mut dummy);
        if size == 0 {
            return (None, None);
        }
        let mut data = vec![0u8; size as usize];
        if GetFileVersionInfoW(w.as_ptr(), 0, size, data.as_mut_ptr() as *mut c_void) == 0 {
            return (None, None);
        }
        let mut ptr: *mut c_void = std::ptr::null_mut();
        let mut len = 0u32;
        let mut langs: Vec<(u16, u16)> = Vec::new();
        if VerQueryValueW(data.as_ptr() as *const c_void, wide("\\VarFileInfo\\Translation").as_ptr(), &mut ptr, &mut len) != 0 && len >= 4 {
            let n = len as usize / 4;
            let s = std::slice::from_raw_parts(ptr as *const u16, n * 2);
            for i in 0..n {
                langs.push((s[i * 2], s[i * 2 + 1]));
            }
        }
        langs.push((0x0409, 0x04B0));
        langs.push((0x0409, 0x04E4));
        let get = |key: &str| -> Option<String> {
            for (l, c) in &langs {
                let q = format!("\\StringFileInfo\\{l:04x}{c:04x}\\{key}");
                let mut p: *mut c_void = std::ptr::null_mut();
                let mut n = 0u32;
                if VerQueryValueW(data.as_ptr() as *const c_void, wide(&q).as_ptr(), &mut p, &mut n) != 0 && n > 1 {
                    let s = from_pwstr(p as *const u16).trim().to_string();
                    if !s.is_empty() {
                        return Some(s);
                    }
                }
            }
            None
        };
        (get("FileDescription"), get("CompanyName"))
    }
}

/// 32x32 RGBA icon for a file (exe, lnk, dll…), via the shell.
pub fn file_icon_rgba(path: &str, size: u32) -> Option<(u32, u32, Vec<u8>)> {
    unsafe {
        let w = wide(path);
        let mut info: SHFILEINFOW = std::mem::zeroed();
        let flags = SHGFI_ICON | if size <= 16 { SHGFI_SMALLICON } else { SHGFI_LARGEICON };
        let r = SHGetFileInfoW(w.as_ptr(), 0, &mut info, std::mem::size_of::<SHFILEINFOW>() as u32, flags);
        if r == 0 || info.hIcon.is_null() {
            return None;
        }
        let out = hicon_to_rgba(info.hIcon);
        DestroyIcon(info.hIcon);
        out
    }
}

/// True when the file has an embedded icon (otherwise the shell would show the generic exe icon).
pub fn has_own_icon(path: &str) -> bool {
    let w = wide(path);
    unsafe { ExtractIconExW(w.as_ptr(), -1, std::ptr::null_mut(), std::ptr::null_mut(), 0) > 0 }
}

/// Icon from a "path,index" resource string (DisplayIcon style).
pub fn resource_icon_rgba(spec: &str) -> Option<(u32, u32, Vec<u8>)> {
    let spec = expand_env(spec.trim().trim_matches('"'));
    let (path, idx) = match spec.rsplit_once(',') {
        Some((p, i)) if i.trim().parse::<i32>().is_ok() => (p.trim().trim_matches('"').to_string(), i.trim().parse::<i32>().unwrap()),
        _ => (spec.clone(), 0),
    };
    if !Path::new(&path).exists() {
        return None;
    }
    if path.to_ascii_lowercase().ends_with(".ico") || idx == 0 {
        return file_icon_rgba(&path, 32);
    }
    unsafe {
        let w = wide(&path);
        let mut large: HICON = std::ptr::null_mut();
        let n = ExtractIconExW(w.as_ptr(), idx, &mut large, std::ptr::null_mut(), 1);
        if n == 0 || large.is_null() {
            return file_icon_rgba(&path, 32);
        }
        let out = hicon_to_rgba(large);
        DestroyIcon(large);
        out
    }
}

unsafe fn hicon_to_rgba(icon: HICON) -> Option<(u32, u32, Vec<u8>)> {
    unsafe {
        let mut ii: ICONINFO = std::mem::zeroed();
        if GetIconInfo(icon, &mut ii) == 0 {
            return None;
        }
        let color = ii.hbmColor;
        let mask = ii.hbmMask;
        let cleanup = || {
            if !color.is_null() {
                DeleteObject(color as HGDIOBJ);
            }
            if !mask.is_null() {
                DeleteObject(mask as HGDIOBJ);
            }
        };
        if color.is_null() {
            cleanup();
            return None;
        }
        let mut bm: BITMAP = std::mem::zeroed();
        GetObjectW(color as HGDIOBJ, std::mem::size_of::<BITMAP>() as i32, &mut bm as *mut _ as *mut c_void);
        let (w, h) = (bm.bmWidth.max(1) as u32, bm.bmHeight.max(1) as u32);
        let dc = CreateCompatibleDC(std::ptr::null_mut());
        let mut bi: BITMAPINFO = std::mem::zeroed();
        bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bi.bmiHeader.biWidth = w as i32;
        bi.bmiHeader.biHeight = -(h as i32);
        bi.bmiHeader.biPlanes = 1;
        bi.bmiHeader.biBitCount = 32;
        bi.bmiHeader.biCompression = BI_RGB;
        let mut px = vec![0u8; (w * h * 4) as usize];
        let got = GetDIBits(dc, color, 0, h, px.as_mut_ptr() as *mut c_void, &mut bi, DIB_RGB_COLORS);
        let has_alpha = px.chunks_exact(4).any(|p| p[3] != 0);
        if got != 0 && !has_alpha && !mask.is_null() {
            // Old-style icon: alpha comes from the AND mask.
            let mut mpx = vec![0u8; (w * h * 4) as usize];
            if GetDIBits(dc, mask, 0, h, mpx.as_mut_ptr() as *mut c_void, &mut bi, DIB_RGB_COLORS) != 0 {
                for (p, m) in px.chunks_exact_mut(4).zip(mpx.chunks_exact(4)) {
                    p[3] = if m[0] == 0 { 255 } else { 0 };
                }
            } else {
                for p in px.chunks_exact_mut(4) {
                    p[3] = 255;
                }
            }
        }
        DeleteDC(dc);
        cleanup();
        if got == 0 {
            return None;
        }
        for p in px.chunks_exact_mut(4) {
            p.swap(0, 2); // BGRA -> RGBA
        }
        Some((w, h, px))
    }
}

pub fn shell_open(file: &str, params: &str, verb: &str) -> Result<(), String> {
    let f = wide(file);
    let p = wide(params);
    let v = wide(verb);
    let r = unsafe {
        ShellExecuteW(std::ptr::null_mut(), if verb.is_empty() { std::ptr::null() } else { v.as_ptr() }, f.as_ptr(), if params.is_empty() { std::ptr::null() } else { p.as_ptr() }, std::ptr::null(), SW_SHOWNORMAL)
    };
    if r as isize > 32 { Ok(()) } else if r as isize == 5 { Err("Access denied".into()) } else { Err(format!("Could not open (code {})", r as isize)) }
}

/// ShellExecuteEx that returns the process handle so the caller can wait on it.
pub fn shell_exec_wait_handle(file: &str, params: &str, verb: &str, show: i32) -> Result<Handle, String> {
    unsafe {
        let f = wide(file);
        let p = wide(params);
        let v = wide(verb);
        let mut sei: SHELLEXECUTEINFOW = std::mem::zeroed();
        sei.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        sei.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI;
        sei.lpVerb = if verb.is_empty() { std::ptr::null() } else { v.as_ptr() };
        sei.lpFile = f.as_ptr();
        sei.lpParameters = if params.is_empty() { std::ptr::null() } else { p.as_ptr() };
        sei.nShow = show;
        if ShellExecuteExW(&mut sei) == 0 {
            let e = GetLastError();
            return Err(if e == ERROR_CANCELLED { "Cancelled".into() } else { error_text(e) });
        }
        Ok(Handle(sei.hProcess))
    }
}

pub fn show_properties(path: &str) {
    unsafe {
        let f = wide(path);
        let v = wide("properties");
        let mut sei: SHELLEXECUTEINFOW = std::mem::zeroed();
        sei.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        sei.fMask = SEE_MASK_INVOKEIDLIST;
        sei.lpVerb = v.as_ptr();
        sei.lpFile = f.as_ptr();
        sei.nShow = SW_SHOW;
        ShellExecuteExW(&mut sei);
    }
}

pub fn reveal_in_explorer(path: &str) {
    let _ = std::process::Command::new("explorer.exe").raw_arg(format!("/select,\"{path}\"")).spawn();
}

use std::os::windows::process::CommandExt;

/// Send files/folders to the Recycle Bin.
pub fn recycle(paths: &[String]) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut buf: Vec<u16> = Vec::new();
    for p in paths {
        buf.extend(p.encode_utf16());
        buf.push(0);
    }
    buf.push(0);
    let mut op: SHFILEOPSTRUCTW = unsafe { std::mem::zeroed() };
    op.wFunc = FO_DELETE;
    op.pFrom = buf.as_ptr();
    op.fFlags = (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI) as u16;
    let r = unsafe { SHFileOperationW(&mut op) };
    if r == 0 && op.fAnyOperationsAborted == 0 { Ok(()) } else if r == 5 || r == 0x78 { Err("Access denied".into()) } else { Err(format!("Could not delete (code {r})")) }
}

// ---------------------------------------------------------------- windows

pub fn force_foreground(hwnd: HWND) {
    unsafe {
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        } else {
            ShowWindow(hwnd, SW_SHOW);
        }
        let fg = GetForegroundWindow();
        let fg_tid = GetWindowThreadProcessId(fg, std::ptr::null_mut());
        let me = GetCurrentThreadId();
        let attached = fg_tid != 0 && fg_tid != me && windows_sys::Win32::System::Threading::AttachThreadInput(me, fg_tid, 1) != 0;
        BringWindowToTop(hwnd);
        SetForegroundWindow(hwnd);
        if attached {
            windows_sys::Win32::System::Threading::AttachThreadInput(me, fg_tid, 0);
        }
        if GetForegroundWindow() != hwnd {
            // Last resort: a synthetic Alt tap grants this process the foreground right.
            use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
            let mut inputs: [INPUT; 2] = std::mem::zeroed();
            inputs[0].r#type = INPUT_KEYBOARD;
            inputs[0].Anonymous.ki.wVk = VK_MENU;
            inputs[1].r#type = INPUT_KEYBOARD;
            inputs[1].Anonymous.ki.wVk = VK_MENU;
            inputs[1].Anonymous.ki.dwFlags = KEYEVENTF_KEYUP;
            SendInput(2, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32);
            SetForegroundWindow(hwnd);
        }
    }
}

pub fn hide_window(hwnd: HWND) {
    unsafe { ShowWindow(hwnd, SW_HIDE) };
}

pub fn is_window_visible(hwnd: HWND) -> bool {
    unsafe { IsWindowVisible(hwnd) != 0 }
}

pub fn switch_to_window(hwnd: HWND) {
    force_foreground(hwnd);
}

pub fn close_window(hwnd: HWND) {
    unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) };
}

pub fn uptime_secs() -> u64 {
    unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() / 1000 }
}

pub fn filetime_to_unix(ft: i64) -> i64 {
    // 100ns ticks since 1601 → seconds since 1970
    (ft / 10_000_000) - 11_644_473_600
}

pub fn now_unix() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Starts a program with the signed-in user's normal (non-admin) rights, even though Sentinel itself
/// runs elevated: the new process is parented to Explorer and inherits its token. Falls back to a
/// plain ShellExecute when that isn't possible (e.g. URLs, or Explorer isn't running).
pub fn launch_as_user(exe: &str, args: &str, cwd: Option<&str>) -> Result<(), String> {
    let looks_like_program = !exe.contains("://") && (exe.to_ascii_lowercase().ends_with(".exe") || !exe.contains('.'));
    if !is_elevated() || !looks_like_program {
        return shell_open(exe, args, "");
    }
    unsafe {
        let shell = GetShellWindow();
        let mut pid = 0u32;
        if shell.is_null() || GetWindowThreadProcessId(shell, &mut pid) == 0 || pid == 0 {
            return shell_open(exe, args, "");
        }
        let hp = OpenProcess(PROCESS_CREATE_PROCESS, 0, pid);
        if hp.is_null() {
            return shell_open(exe, args, "");
        }
        let hp = Handle(hp);
        let mut size = 0usize;
        InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size);
        let mut buf = vec![0u8; size.max(64)];
        let list = buf.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
        if InitializeProcThreadAttributeList(list, 1, 0, &mut size) == 0 {
            return shell_open(exe, args, "");
        }
        let mut parent = hp.0;
        let ok = UpdateProcThreadAttribute(list, 0, PROC_THREAD_ATTRIBUTE_PARENT_PROCESS as usize, &mut parent as *mut HANDLE as *const c_void, std::mem::size_of::<HANDLE>(), std::ptr::null_mut(), std::ptr::null());
        if ok == 0 {
            DeleteProcThreadAttributeList(list);
            return shell_open(exe, args, "");
        }
        let mut si: STARTUPINFOEXW = std::mem::zeroed();
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        si.lpAttributeList = list;
        let quoted = if exe.starts_with('"') { exe.to_string() } else { format!("\"{exe}\"") };
        let mut cmd = wide(&if args.is_empty() { quoted } else { format!("{quoted} {args}") });
        let dir = cwd.map(wide).or_else(|| Path::new(exe).parent().filter(|p| p.is_dir()).map(|p| wide(&p.to_string_lossy())));
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        let ok = CreateProcessW(
            std::ptr::null(),
            cmd.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_NEW_PROCESS_GROUP,
            std::ptr::null(),
            dir.as_ref().map_or(std::ptr::null(), |d| d.as_ptr()),
            &si.StartupInfo,
            &mut pi,
        );
        DeleteProcThreadAttributeList(list);
        if ok == 0 {
            let e = last_error();
            return shell_open(exe, args, "").map_err(|_| format!("Could not start it (error {e})"));
        }
        CloseHandle(pi.hThread);
        CloseHandle(pi.hProcess);
        Ok(())
    }
}

/// Hands memory pages Sentinel isn't actively using back to Windows (they come back on demand).
/// Most of the startup working set (driver and font setup) is never touched again.
pub fn trim_working_set() {
    unsafe {
        SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
}

