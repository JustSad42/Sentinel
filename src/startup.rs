//! Startup apps: Run keys and Startup folders, with the same enable/disable switches Windows uses.

use crate::admin::{self, AdminOp};
use crate::win;
use winreg::RegKey;
use winreg::enums::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    UserRun,
    MachineRun,
    MachineRun32,
    UserFolder,
    CommonFolder,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::UserRun => "Registry (you)",
            Source::MachineRun => "Registry (all users)",
            Source::MachineRun32 => "Registry (all users, 32-bit)",
            Source::UserFolder => "Startup folder (you)",
            Source::CommonFolder => "Startup folder (all users)",
        }
    }
    fn machine(self) -> bool {
        matches!(self, Source::MachineRun | Source::MachineRun32 | Source::CommonFolder)
    }
    fn approved_key(self) -> &'static str {
        match self {
            Source::UserRun | Source::MachineRun => r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run",
            Source::MachineRun32 => r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run32",
            Source::UserFolder | Source::CommonFolder => r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\StartupFolder",
        }
    }
}

#[derive(Clone)]
pub struct StartupItem {
    pub name: String,
    pub command: String,
    pub target: String,
    pub source: Source,
    pub enabled: bool,
    pub missing: bool,
    pub description: Option<String>,
    pub publisher: Option<String>,
    /// For folder items: the shortcut/file path.
    pub file: Option<String>,
}

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

fn approved(source: Source, name: &str) -> bool {
    let root = if source.machine() { RegKey::predef(HKEY_LOCAL_MACHINE) } else { RegKey::predef(HKEY_CURRENT_USER) };
    match root.open_subkey_with_flags(source.approved_key(), KEY_READ | KEY_WOW64_64KEY).and_then(|k| k.get_raw_value(name)) {
        Ok(v) => v.bytes.first().map(|b| b & 1 == 0).unwrap_or(true),
        Err(_) => true,
    }
}

pub fn list() -> Vec<StartupItem> {
    let mut out = Vec::new();
    let run_keys = [
        (RegKey::predef(HKEY_CURRENT_USER), KEY_WOW64_64KEY, Source::UserRun),
        (RegKey::predef(HKEY_LOCAL_MACHINE), KEY_WOW64_64KEY, Source::MachineRun),
        (RegKey::predef(HKEY_LOCAL_MACHINE), KEY_WOW64_32KEY, Source::MachineRun32),
    ];
    for (root, view, source) in run_keys {
        if let Ok(k) = root.open_subkey_with_flags(RUN, KEY_READ | view) {
            for (name, _) in k.enum_values().flatten() {
                let command: String = k.get_value(&name).unwrap_or_default();
                let target = win::expand_env(&crate::actions::split_args(&command).0);
                let missing = !target.is_empty() && !std::path::Path::new(&target).exists();
                out.push(StartupItem { enabled: approved(source, &name), name, command, target, source, missing, description: None, publisher: None, file: None });
            }
        }
    }
    let folders = [
        (std::env::var("APPDATA").unwrap_or_default() + r"\Microsoft\Windows\Start Menu\Programs\Startup", Source::UserFolder),
        (std::env::var("ProgramData").unwrap_or_default() + r"\Microsoft\Windows\Start Menu\Programs\StartUp", Source::CommonFolder),
    ];
    for (dir, source) in folders {
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let fname = e.file_name().to_string_lossy().to_string();
                if fname.eq_ignore_ascii_case("desktop.ini") {
                    continue;
                }
                let p = e.path().to_string_lossy().to_string();
                let name = fname.trim_end_matches(".lnk").to_string();
                out.push(StartupItem { enabled: approved(source, &fname), name, command: p.clone(), target: p.clone(), source, missing: false, description: None, publisher: None, file: Some(p) });
            }
        }
    }
    for it in out.iter_mut() {
        if it.file.is_none() && !it.missing {
            let (d, c) = win::version_strings(&it.target);
            it.description = d;
            it.publisher = c;
        }
    }
    out.sort_by_key(|i| i.name.to_lowercase());
    out
}

fn value_name(it: &StartupItem) -> String {
    match &it.file {
        Some(f) => std::path::Path::new(f).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(),
        None => it.name.clone(),
    }
}

pub fn set_enabled(it: &StartupItem, enabled: bool) -> Result<(), String> {
    let mut data = vec![0u8; 12];
    if enabled {
        data[0] = 2;
    } else {
        data[0] = 3;
        let ft = (win::now_unix() + 11_644_473_600) as u64 * 10_000_000;
        data[4..12].copy_from_slice(&ft.to_le_bytes());
    }
    let name = value_name(it);
    if it.source.machine() {
        admin::run(AdminOp::RegSetBinary { path: it.source.approved_key().to_string(), name, data })
    } else {
        let (k, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(it.source.approved_key()).map_err(|e| e.to_string())?;
        k.set_raw_value(&name, &winreg::RegValue { bytes: data.into(), vtype: REG_BINARY }).map_err(|e| e.to_string())
    }
}

pub fn remove(it: &StartupItem) -> Result<(), String> {
    match it.source {
        Source::UserRun => RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RUN, KEY_ALL_ACCESS).and_then(|k| k.delete_value(&it.name)).map_err(|e| e.to_string()),
        Source::MachineRun | Source::MachineRun32 => admin::run(AdminOp::RegDeleteValue { path: RUN.to_string(), name: it.name.clone(), wow32: it.source == Source::MachineRun32 }),
        Source::UserFolder => win::recycle(&[it.file.clone().unwrap_or_default()]),
        Source::CommonFolder => admin::run(AdminOp::Recycle(vec![it.file.clone().unwrap_or_default()])),
    }
}
