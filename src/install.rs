//! The installed copy. Sentinel runs elevated without a UAC prompt (launch task, Task Manager
//! redirect), so those may only ever point at an exe that normal programs can't replace:
//! `Program Files\Sentinel`. Anything under the user's own folders would hand them admin rights.

use crate::updater;
use std::path::{Path, PathBuf};
use windows_sys::Win32::UI::Shell::{FOLDERID_ProgramFiles, SHGetKnownFolderPath};

/// Asked from Windows, not `%ProgramFiles%`: a user can change their environment variables, and
/// elevated processes inherit them.
pub fn dir() -> Option<PathBuf> {
    unsafe {
        let mut p: *mut u16 = std::ptr::null_mut();
        let ok = SHGetKnownFolderPath(&FOLDERID_ProgramFiles, 0, std::ptr::null_mut(), &mut p) >= 0;
        let path = ok.then(|| {
            let len = (0..).take_while(|i| *p.add(*i) != 0).count();
            PathBuf::from(String::from_utf16_lossy(std::slice::from_raw_parts(p, len))).join("Sentinel")
        });
        windows_sys::Win32::System::Com::CoTaskMemFree(p as _);
        path
    }
}

/// True for an exe directly inside `dir`.
fn inside(exe: &Path, dir: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().to_lowercase();
    exe.parent().is_some_and(|p| norm(p) == norm(dir))
}

pub fn is_protected(exe: &str) -> bool {
    dir().is_some_and(|d| inside(Path::new(exe), &d))
}

/// Remembers the installed version, so an older copy left elsewhere never replaces a newer one.
pub fn record_version(exe: &Path) {
    if let Some(d) = dir().filter(|d| inside(exe, d)) {
        let _ = std::fs::write(d.join("version.txt"), updater::VERSION);
    }
}

/// Called by an elevated copy. `Ok(None)`: this is the installed copy. `Ok(Some(exe))`: start that
/// one instead (installed just now, or already there and at least as new).
pub fn ensure() -> Result<Option<PathBuf>, String> {
    let cur = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = dir().ok_or("Program Files not found")?;
    if inside(&cur, &dir) {
        record_version(&cur);
        return Ok(None);
    }
    let target = dir.join("Sentinel.exe");
    let installed = std::fs::read_to_string(dir.join("version.txt")).unwrap_or_default();
    if !target.exists() || installed.is_empty() || updater::newer(updater::VERSION, &installed) {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let bytes = std::fs::read(&cur).map_err(|e| e.to_string())?;
        updater::replace(&target, &bytes)?;
        record_version(&target);
    }
    Ok(Some(target))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_files_directly_inside_count() {
        let d = Path::new(r"C:\Program Files\Sentinel");
        assert!(inside(Path::new(r"c:\program files\sentinel\Sentinel.exe"), d));
        assert!(!inside(Path::new(r"C:\Users\a\AppData\Local\Programs\Sentinel\Sentinel.exe"), d));
        assert!(!inside(Path::new(r"C:\Program Files\Sentinel\update\Sentinel.exe"), d));
        assert!(!inside(Path::new(r"C:\Program Files\SentinelX\Sentinel.exe"), d));
    }

    #[test]
    fn program_files_is_found() {
        assert!(dir().is_some_and(|d| d.ends_with("Sentinel")));
    }
}
