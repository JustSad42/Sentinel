//! Self-update from GitHub Releases: download Sentinel.exe + .sha256, verify, swap in place, restart.

use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const REPO: &str = "JustSad42/Sentinel";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, PartialEq)]
pub enum State {
    Idle,
    Checking,
    UpToDate,
    Downloading(String),
    Ready { version: String, notes: String, path: std::path::PathBuf },
    Error(String),
}

pub type Shared = Arc<Mutex<State>>;

fn parse(v: &str) -> (u32, u32, u32) {
    let mut it = v.trim_start_matches('v').split('.').map(|x| x.parse::<u32>().unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0), it.next().unwrap_or(0))
}

pub fn newer(remote: &str, local: &str) -> bool {
    parse(remote) > parse(local)
}

#[derive(serde::Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    body: String,
    assets: Vec<Asset>,
}
#[derive(serde::Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(60))).build().into()
}

fn get_bytes(url: &str, limit: u64) -> Result<Vec<u8>, String> {
    let mut r = agent().get(url).header("User-Agent", "Sentinel-updater").call().map_err(|e| e.to_string())?;
    r.body_mut().with_config().limit(limit).read_to_vec().map_err(|e| e.to_string())
}

/// Checks for a newer release and downloads it. Leaves the result in `state`.
pub fn check(state: &Shared) {
    *state.lock().unwrap() = State::Checking;
    let res = (|| -> Result<State, String> {
        let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
        let mut resp = agent().get(&url).header("User-Agent", "Sentinel-updater").header("Accept", "application/vnd.github+json").call().map_err(|e| {
            if e.to_string().contains("404") { "No releases published yet".to_string() } else { e.to_string() }
        })?;
        let rel: Release = resp.body_mut().read_json().map_err(|e| e.to_string())?;
        if !newer(&rel.tag_name, VERSION) {
            return Ok(State::UpToDate);
        }
        let version = rel.tag_name.trim_start_matches('v').to_string();
        let exe = rel.assets.iter().find(|a| a.name.eq_ignore_ascii_case("Sentinel.exe")).ok_or("Release has no Sentinel.exe")?;
        let sum = rel.assets.iter().find(|a| a.name.eq_ignore_ascii_case("Sentinel.exe.sha256")).ok_or("Release has no checksum")?;
        *state.lock().unwrap() = State::Downloading(version.clone());
        let expected = String::from_utf8_lossy(&get_bytes(&sum.browser_download_url, 4096)?).split_whitespace().next().unwrap_or("").to_lowercase();
        let bytes = get_bytes(&exe.browser_download_url, 200 << 20)?;
        let actual = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect::<String>();
        if actual != expected {
            return Err("Downloaded update failed the checksum, discarded".into());
        }
        let dir = crate::config::data_dir().join("update");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("Sentinel-{version}.exe"));
        std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
        Ok(State::Ready { version, notes: rel.body, path })
    })();
    *state.lock().unwrap() = match res {
        Ok(s) => s,
        Err(e) => State::Error(e),
    };
}

/// Replaces the running exe with the downloaded one and starts it. The caller then exits.
pub fn apply(path: &std::path::Path, show: bool) -> Result<(), String> {
    let cur = std::env::current_exe().map_err(|e| e.to_string())?;
    let old = cur.with_extension("old.exe");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&cur, &old).map_err(|e| format!("Could not replace the app: {e}"))?;
    if let Err(e) = std::fs::copy(path, &cur) {
        let _ = std::fs::rename(&old, &cur);
        return Err(format!("Could not install the update: {e}"));
    }
    let _ = std::fs::remove_file(path);
    let mut cmd = std::process::Command::new(&cur);
    cmd.arg("--takeover").arg("--updated");
    if !show {
        cmd.arg("--background");
    }
    cmd.spawn().map_err(|e| e.to_string())?;
    Ok(())
}

/// Removes the previous exe left behind by an update.
pub fn cleanup() {
    if let Ok(cur) = std::env::current_exe() {
        let _ = std::fs::remove_file(cur.with_extension("old.exe"));
    }
}

pub fn spawn_auto(state: Shared, enabled: Arc<std::sync::atomic::AtomicBool>) {
    std::thread::Builder::new()
        .name("updater".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_secs(45));
            loop {
                if enabled.load(std::sync::atomic::Ordering::Relaxed) && !matches!(*state.lock().unwrap(), State::Ready { .. }) {
                    check(&state);
                }
                std::thread::sleep(Duration::from_secs(6 * 3600));
            }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    #[test]
    fn versions() {
        assert!(super::newer("v1.2.0", "1.1.9"));
        assert!(super::newer("1.10.0", "1.9.0"));
        assert!(!super::newer("v1.0.0", "1.0.0"));
    }
}
