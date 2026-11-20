//! Operations that need administrator rights. When Sentinel isn't elevated it re-launches itself
//! elevated for just that one operation (one UAC prompt), instead of restarting the whole app.

use crate::win;
use serde::{Deserialize, Serialize};
use std::os::windows::process::CommandExt;
use winreg::RegKey;
use winreg::enums::*;

#[derive(Clone, Serialize, Deserialize, Debug)]
pub enum ServiceAction {
    Start,
    Stop,
    Restart,
    StartType(u32),
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub enum AdminOp {
    KillMany(Vec<(u32, i64)>),
    SetPriority(u32, crate::config::Priority),
    Ifeo { enable: bool, exe: String },
    LogonTask { enable: bool, exe: String },
    /// HKLM key path, deleted with all subkeys (64-bit view unless `wow32`).
    RegDeleteTree { path: String, wow32: bool },
    RegSetBinary { path: String, name: String, data: Vec<u8> },
    RegDeleteValue { path: String, name: String, wow32: bool },
    Service { name: String, action: ServiceAction },
    Recycle(Vec<String>),
    Firewall { block: bool, path: String },
    CloseTcp { local: String, remote: String },
}

pub fn run(op: AdminOp) -> Result<(), String> {
    if win::is_elevated() {
        return execute(&op);
    }
    let json = serde_json::to_vec(&op).map_err(|e| e.to_string())?;
    let hex: String = json.iter().map(|b| format!("{b:02x}")).collect();
    let result_file = std::env::temp_dir().join(format!("sentinel-admin-{}-{}.txt", std::process::id(), win::now_unix()));
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let h = win::shell_exec_wait_handle(&exe.to_string_lossy(), &format!("--admin-op {hex} \"{}\"", result_file.display()), "runas", 0)?;
    unsafe { windows_sys::Win32::System::Threading::WaitForSingleObject(h.0, 120_000) };
    let res = std::fs::read_to_string(&result_file).unwrap_or_else(|_| "Elevated helper did not report back".into());
    let _ = std::fs::remove_file(&result_file);
    if res.trim() == "ok" { Ok(()) } else { Err(res.trim().to_string()) }
}

/// Entry point for `sentinel.exe --admin-op <hex> <result-file>`.
pub fn helper_main(hex: &str, result_file: &str) {
    let bytes: Vec<u8> = (0..hex.len() / 2).filter_map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()).collect();
    let res = match serde_json::from_slice::<AdminOp>(&bytes) {
        Ok(op) => execute(&op),
        Err(e) => Err(e.to_string()),
    };
    let _ = std::fs::write(result_file, match res {
        Ok(()) => "ok".to_string(),
        Err(e) => e,
    });
}

fn hklm(wow32: bool, access: u32) -> Result<RegKey, String> {
    let _ = access;
    Ok(RegKey::predef(HKEY_LOCAL_MACHINE)).map(|k| {
        let _ = wow32;
        k
    })
}

fn view(wow32: bool) -> u32 {
    if wow32 { KEY_WOW64_32KEY } else { KEY_WOW64_64KEY }
}

pub const IFEO_KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\taskmgr.exe";

fn execute(op: &AdminOp) -> Result<(), String> {
    match op {
        AdminOp::KillMany(list) => {
            let (_, errs) = crate::actions::kill_many(list);
            if errs.is_empty() { Ok(()) } else { Err(errs.join("; ")) }
        }
        AdminOp::SetPriority(pid, p) => crate::actions::set_priority(*pid, *p),
        AdminOp::Ifeo { enable, exe } => {
            let root = hklm(false, 0)?;
            let (k, _) = root.create_subkey_with_flags(IFEO_KEY, KEY_ALL_ACCESS | KEY_WOW64_64KEY).map_err(|e| e.to_string())?;
            if *enable {
                k.set_value("Debugger", &format!("\"{exe}\" --ifeo")).map_err(|e| e.to_string())
            } else {
                match k.delete_value("Debugger") {
                    Ok(()) => Ok(()),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(e) => Err(e.to_string()),
                }
            }
        }
        AdminOp::LogonTask { enable, exe } => ensure_task(exe, *enable),
        AdminOp::RegDeleteTree { path, wow32 } => {
            let (parent, leaf) = path.rsplit_once('\\').ok_or("bad key")?;
            let p = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey_with_flags(parent, KEY_ALL_ACCESS | view(*wow32)).map_err(|e| e.to_string())?;
            p.delete_subkey_all(leaf).map_err(|e| e.to_string())
        }
        AdminOp::RegSetBinary { path, name, data } => {
            let (k, _) = RegKey::predef(HKEY_LOCAL_MACHINE).create_subkey_with_flags(path, KEY_ALL_ACCESS | KEY_WOW64_64KEY).map_err(|e| e.to_string())?;
            k.set_raw_value(name, &winreg::RegValue { bytes: data.clone().into(), vtype: REG_BINARY }).map_err(|e| e.to_string())
        }
        AdminOp::RegDeleteValue { path, name, wow32 } => {
            let k = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey_with_flags(path, KEY_ALL_ACCESS | view(*wow32)).map_err(|e| e.to_string())?;
            k.delete_value(name).map_err(|e| e.to_string())
        }
        AdminOp::Service { name, action } => crate::services::control(name, action),
        AdminOp::Recycle(paths) => win::recycle(paths),
        AdminOp::Firewall { block, path } => firewall(*block, path),
        AdminOp::CloseTcp { local, remote } => crate::net::close_tcp(local.parse().map_err(|_| "bad address")?, remote.parse().map_err(|_| "bad address")?),
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

pub fn firewall_rule_name(path: &str) -> String {
    let file = std::path::Path::new(path).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
    format!("Sentinel block - {file}")
}

fn firewall(block: bool, path: &str) -> Result<(), String> {
    let name = firewall_rule_name(path);
    let run = |args: String| -> Result<(), String> {
        let out = std::process::Command::new("netsh.exe").creation_flags(0x0800_0000).raw_arg(args).output().map_err(|e| e.to_string())?;
        if out.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&out.stdout).trim().to_string()) }
    };
    if block {
        run(format!("advfirewall firewall add rule name=\"{name}\" dir=out action=block program=\"{path}\" enable=yes"))?;
        run(format!("advfirewall firewall add rule name=\"{name}\" dir=in action=block program=\"{path}\" enable=yes"))
    } else {
        run(format!("advfirewall firewall delete rule name=\"{name}\" program=\"{path}\""))
    }
}

/// True when Ctrl+Shift+Esc / taskmgr.exe launches Sentinel.
pub fn ifeo_enabled() -> bool {
    RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(IFEO_KEY, KEY_READ | KEY_WOW64_64KEY)
        .and_then(|k| k.get_value::<String, _>("Debugger"))
        .map(|v| v.to_ascii_lowercase().contains("sentinel"))
        .unwrap_or(false)
}

fn task_xml() -> Option<String> {
    let out = std::process::Command::new("schtasks.exe").creation_flags(0x0800_0000).args(["/Query", "/TN", "Sentinel", "/XML"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    // schtasks prints the XML as UTF-16 or the console code page depending on version.
    let b = &out.stdout;
    if b.len() > 2 && b[1] == 0 {
        let w: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        Some(String::from_utf16_lossy(&w))
    } else {
        Some(String::from_utf8_lossy(b).to_string())
    }
}

/// True when the logon task starts Sentinel at sign-in.
pub fn logon_task_enabled() -> bool {
    task_xml().is_some_and(|x| x.contains("<LogonTrigger>"))
}

/// The elevated launch task: exists, points at this exe, and has a logon trigger iff `at_logon`.
/// Needs admin. Running it later (`run_task`) needs no UAC prompt.
pub fn ensure_task(exe: &str, at_logon: bool) -> Result<(), String> {
    if let Some(x) = task_xml() {
        if x.contains(&xml_escape(exe)) && x.contains("<LogonTrigger>") == at_logon {
            return Ok(());
        }
    }
    let user = std::env::var("USERDOMAIN").map(|d| format!("{d}\\")).unwrap_or_default() + &std::env::var("USERNAME").unwrap_or_default();
    let trigger = if at_logon { format!("<Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{}</UserId></LogonTrigger></Triggers>", xml_escape(&user)) } else { "<Triggers/>".to_string() };
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>Starts Sentinel with admin rights without a UAC prompt.</Description></RegistrationInfo>
  {trigger}
  <Principals><Principal id="Author"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>HighestAvailable</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>false</StartWhenAvailable>
    <IdleSettings><StopOnIdleEnd>false</StopOnIdleEnd><RestartOnIdle>false</RestartOnIdle></IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>4</Priority>
  </Settings>
  <Actions Context="Author"><Exec><Command>{exe}</Command><Arguments>--background</Arguments></Exec></Actions>
</Task>"#,
        user = xml_escape(&user),
        exe = xml_escape(exe)
    );
    let path = std::env::temp_dir().join("sentinel-task.xml");
    let utf16: Vec<u8> = std::iter::once(0xFEFFu16).chain(xml.encode_utf16()).flat_map(|c| c.to_le_bytes()).collect();
    std::fs::write(&path, utf16).map_err(|e| e.to_string())?;
    let out = std::process::Command::new("schtasks.exe").creation_flags(0x0800_0000).args(["/Create", "/TN", "Sentinel", "/XML"]).arg(&path).arg("/F").output().map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&path);
    if out.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&out.stderr).trim().to_string()) }
}

/// Starts the elevated task from a non-elevated process (no UAC prompt). False if it doesn't exist.
pub fn run_task() -> bool {
    std::process::Command::new("schtasks.exe").creation_flags(0x0800_0000).args(["/Run", "/TN", "Sentinel"]).output().map(|o| o.status.success()).unwrap_or(false)
}

pub fn delete_task() {
    let _ = std::process::Command::new("schtasks.exe").creation_flags(0x0800_0000).args(["/Delete", "/TN", "Sentinel", "/F"]).output();
}
