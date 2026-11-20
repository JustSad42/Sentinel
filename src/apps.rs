//! Installed programs (the Uninstall registry keys): uninstall, detect broken entries, clean up.

use crate::admin::{self, AdminOp};
use crate::win;
use std::os::windows::process::CommandExt;
use std::path::Path;
use winreg::RegKey;
use winreg::enums::*;

const UNINSTALL: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    Machine,
    Machine32,
    User,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Health {
    Ok,
    /// The install folder is gone but the uninstaller may still work.
    FilesMissing,
    /// The uninstaller itself is gone: the entry can't be removed the normal way.
    Broken,
}

#[derive(Clone)]
pub struct App {
    pub key: String,
    pub scope: Scope,
    pub name: String,
    pub publisher: String,
    pub version: String,
    pub install_date: String,
    pub size_kb: u64,
    pub location: String,
    pub uninstall: String,
    pub quiet_uninstall: String,
    pub icon: String,
    pub msi: bool,
    pub no_remove: bool,
    pub health: Health,
    pub uninstaller_exe: String,
}

impl App {
    pub fn id(&self) -> String {
        format!("{:?}\\{}", self.scope, self.key)
    }
    pub fn reg_path(&self) -> String {
        let root = match self.scope {
            Scope::User => "HKEY_CURRENT_USER",
            _ => "HKEY_LOCAL_MACHINE",
        };
        let base = if self.scope == Scope::Machine32 { r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall" } else { UNINSTALL };
        format!("{root}\\{base}\\{}", self.key)
    }
    pub fn installed_unix(&self) -> i64 {
        // yyyymmdd → rough unix time for sorting
        let s = &self.install_date;
        if s.len() == 8 {
            let y: i64 = s[0..4].parse().unwrap_or(1970);
            let m: i64 = s[4..6].parse().unwrap_or(1);
            let d: i64 = s[6..8].parse().unwrap_or(1);
            ((y - 1970) * 365 + (m - 1) * 30 + d) * 86400
        } else {
            0
        }
    }
}

fn get_s(k: &RegKey, n: &str) -> String {
    k.get_value::<String, _>(n).unwrap_or_default().trim().to_string()
}

pub fn list() -> Vec<App> {
    let mut out: Vec<App> = Vec::new();
    let roots = [
        (RegKey::predef(HKEY_LOCAL_MACHINE), KEY_WOW64_64KEY, Scope::Machine),
        (RegKey::predef(HKEY_LOCAL_MACHINE), KEY_WOW64_32KEY, Scope::Machine32),
        (RegKey::predef(HKEY_CURRENT_USER), KEY_WOW64_64KEY, Scope::User),
    ];
    for (root, view, scope) in roots {
        let Ok(base) = root.open_subkey_with_flags(UNINSTALL, KEY_READ | view) else { continue };
        for key in base.enum_keys().flatten() {
            let Ok(k) = base.open_subkey_with_flags(&key, KEY_READ | view) else { continue };
            let name = get_s(&k, "DisplayName");
            if name.is_empty() {
                continue;
            }
            if k.get_value::<u32, _>("SystemComponent").unwrap_or(0) == 1 {
                continue;
            }
            if !get_s(&k, "ParentKeyName").is_empty() {
                continue;
            }
            let rt = get_s(&k, "ReleaseType").to_lowercase();
            if rt.contains("update") || rt.contains("hotfix") {
                continue;
            }
            let uninstall = get_s(&k, "UninstallString");
            let quiet = get_s(&k, "QuietUninstallString");
            let msi = k.get_value::<u32, _>("WindowsInstaller").unwrap_or(0) == 1 || uninstall.to_ascii_lowercase().contains("msiexec");
            if uninstall.is_empty() && quiet.is_empty() && !msi {
                continue;
            }
            let mut location = win::expand_env(get_s(&k, "InstallLocation").trim_matches('"'));
            while location.ends_with('\\') && location.len() > 3 {
                location.pop();
            }
            let icon = get_s(&k, "DisplayIcon");
            let uninstaller_exe = if msi { String::new() } else { win::expand_env(&crate::actions::split_args(if uninstall.is_empty() { &quiet } else { &uninstall }).0) };
            let health = if !msi && !uninstaller_exe.is_empty() && !Path::new(&uninstaller_exe).exists() && !uninstaller_exe.to_ascii_lowercase().ends_with("rundll32.exe") {
                Health::Broken
            } else if !location.is_empty() && !Path::new(&location).exists() {
                Health::FilesMissing
            } else {
                Health::Ok
            };
            // The same product often appears in both views; keep the first.
            if out.iter().any(|a| a.name == name && a.version == get_s(&k, "DisplayVersion") && a.key == key) {
                continue;
            }
            out.push(App {
                key,
                scope,
                publisher: get_s(&k, "Publisher"),
                version: get_s(&k, "DisplayVersion"),
                install_date: get_s(&k, "InstallDate"),
                size_kb: k.get_value::<u32, _>("EstimatedSize").unwrap_or(0) as u64,
                location,
                uninstall,
                quiet_uninstall: quiet,
                icon,
                msi,
                no_remove: k.get_value::<u32, _>("NoRemove").unwrap_or(0) == 1,
                health,
                uninstaller_exe,
                name,
            });
        }
    }
    out.sort_by_key(|a| a.name.to_lowercase());
    out
}

/// Launches the app's uninstaller and returns its process handle so the UI can wait for it.
pub fn uninstall(app: &App, quiet: bool) -> Result<win::Handle, String> {
    let cmd = if app.msi && app.key.starts_with('{') {
        format!("MsiExec.exe /X{}{}", app.key, if quiet { " /qb" } else { "" })
    } else if quiet && !app.quiet_uninstall.is_empty() {
        app.quiet_uninstall.clone()
    } else {
        app.uninstall.clone()
    };
    if cmd.is_empty() {
        return Err("This entry has no uninstaller".into());
    }
    let (exe, args) = crate::actions::split_args(&win::expand_env(&cmd));
    win::shell_exec_wait_handle(&exe, &args, "", 1)
}

fn backup_dir() -> std::path::PathBuf {
    let d = crate::config::data_dir().join("registry-backups");
    let _ = std::fs::create_dir_all(&d);
    d
}

fn safe_file(s: &str) -> String {
    s.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '.' || c == ' ' { c } else { '_' }).collect::<String>().trim().to_string()
}

fn reg_export(path: &str, file: &Path, wow32: bool) -> bool {
    let mut c = std::process::Command::new("reg.exe");
    c.creation_flags(0x0800_0000).arg("export").arg(path).arg(file).arg("/y");
    c.arg(if wow32 { "/reg:32" } else { "/reg:64" });
    c.output().map(|o| o.status.success()).unwrap_or(false)
}

/// MSI "packed GUID" used under Installer\Products.
pub fn packed_guid(guid: &str) -> Option<String> {
    let g: String = guid.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if g.len() != 32 {
        return None;
    }
    let rev = |s: &str| s.chars().rev().collect::<String>();
    let mut out = String::new();
    out += &rev(&g[0..8]);
    out += &rev(&g[8..12]);
    out += &rev(&g[12..16]);
    for i in (16..32).step_by(2) {
        out += &rev(&g[i..i + 2]);
    }
    Some(out.to_uppercase())
}

/// Removes the leftover registry entry (after exporting a .reg backup). Returns the backup path.
pub fn remove_entry(app: &App) -> Result<String, String> {
    let ts = win::now_unix();
    let backup = backup_dir().join(format!("{}-{ts}.reg", safe_file(&app.name)));
    if !reg_export(&app.reg_path(), &backup, app.scope == Scope::Machine32) {
        return Err("Could not back up the registry entry, so nothing was removed".into());
    }
    match app.scope {
        Scope::User => {
            let base = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(UNINSTALL, KEY_ALL_ACCESS).map_err(|e| e.to_string())?;
            base.delete_subkey_all(&app.key).map_err(|e| e.to_string())?;
        }
        Scope::Machine | Scope::Machine32 => {
            admin::run(AdminOp::RegDeleteTree { path: format!("{UNINSTALL}\\{}", app.key), wow32: app.scope == Scope::Machine32 })?;
        }
    }
    // Orphaned MSI registration (otherwise Windows Installer still thinks it's installed).
    if app.msi {
        if let Some(packed) = packed_guid(&app.key) {
            let prod = format!(r"SOFTWARE\Classes\Installer\Products\{packed}");
            if RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey_with_flags(&prod, KEY_READ | KEY_WOW64_64KEY).is_ok() {
                let b2 = backup_dir().join(format!("{}-{ts}-msi.reg", safe_file(&app.name)));
                if reg_export(&format!("HKEY_LOCAL_MACHINE\\{prod}"), &b2, false) {
                    let _ = admin::run(AdminOp::RegDeleteTree { path: prod, wow32: false });
                }
            }
        }
    }
    Ok(backup.to_string_lossy().to_string())
}

/// Folders that likely belong to the app and still exist (install dir, AppData, ProgramData).
pub fn leftovers(app: &App) -> Vec<String> {
    let mut out = Vec::new();
    if !app.location.is_empty() && Path::new(&app.location).is_dir() {
        let l = app.location.to_ascii_lowercase();
        // Never offer drive roots or system folders.
        let protected = ["c:\\windows", "c:\\program files", "c:\\program files (x86)", "c:\\programdata", "c:\\users"];
        if app.location.len() > 3 && !protected.contains(&l.as_str()) {
            out.push(app.location.clone());
        }
    }
    let mut names = vec![app.name.clone()];
    // "Foo 1.2.3" → "Foo"
    if let Some(stripped) = app.name.rsplit_once(' ').filter(|(_, v)| v.chars().next().is_some_and(|c| c.is_ascii_digit() || c == 'v')).map(|(n, _)| n.to_string()) {
        names.push(stripped);
    }
    if let Some(leaf) = Path::new(&app.location).file_name() {
        names.push(leaf.to_string_lossy().to_string());
    }
    let bases: Vec<String> = ["APPDATA", "LOCALAPPDATA", "ProgramData"].iter().filter_map(|v| std::env::var(v).ok()).collect();
    for b in &bases {
        for n in &names {
            if n.len() < 3 {
                continue;
            }
            for cand in [Path::new(b).join(n), Path::new(b).join(&app.publisher).join(n)] {
                let s = cand.to_string_lossy().to_string();
                if cand.is_dir() && !out.iter().any(|o: &String| o.eq_ignore_ascii_case(&s)) {
                    out.push(s);
                }
            }
        }
    }
    out
}

pub fn dir_size(path: &str) -> u64 {
    let mut total = 0;
    let mut stack = vec![std::path::PathBuf::from(path)];
    let mut n = 0;
    while let Some(d) = stack.pop() {
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                n += 1;
                if n > 50_000 {
                    return total;
                }
                match e.metadata() {
                    Ok(m) if m.is_dir() => stack.push(e.path()),
                    Ok(m) => total += m.len(),
                    _ => {}
                }
            }
        }
    }
    total
}

/// Opens Registry Editor at the app's key.
pub fn show_in_regedit(app: &App) {
    if let Ok((k, _)) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(r"Software\Microsoft\Windows\CurrentVersion\Applets\Regedit") {
        let _ = k.set_value("LastKey", &format!("Computer\\{}", app.reg_path().replace("HKEY_LOCAL_MACHINE\\SOFTWARE\\WOW6432Node", "HKEY_LOCAL_MACHINE\\SOFTWARE\\WOW6432Node")));
    }
    let _ = win::shell_open("regedit.exe", "-m", "");
}

#[cfg(test)]
mod tests {
    #[test]
    fn packed() {
        assert_eq!(super::packed_guid("{12345678-ABCD-EF01-2345-6789ABCDEF01}").unwrap(), "87654321DCBA10FE32547698BADCFE10");
    }
}
