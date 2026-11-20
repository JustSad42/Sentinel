//! Persistent settings, stored as JSON in %APPDATA%\Sentinel.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum Priority {
    Idle,
    BelowNormal,
    Normal,
    AboveNormal,
    High,
    Realtime,
}

impl Priority {
    pub const ALL: [Priority; 6] = [Priority::Idle, Priority::BelowNormal, Priority::Normal, Priority::AboveNormal, Priority::High, Priority::Realtime];
    pub fn label(self) -> &'static str {
        match self {
            Priority::Idle => "Low",
            Priority::BelowNormal => "Below normal",
            Priority::Normal => "Normal",
            Priority::AboveNormal => "Above normal",
            Priority::High => "High",
            Priority::Realtime => "Realtime",
        }
    }
    pub fn class(self) -> u32 {
        use windows_sys::Win32::System::Threading::*;
        match self {
            Priority::Idle => IDLE_PRIORITY_CLASS,
            Priority::BelowNormal => BELOW_NORMAL_PRIORITY_CLASS,
            Priority::Normal => NORMAL_PRIORITY_CLASS,
            Priority::AboveNormal => ABOVE_NORMAL_PRIORITY_CLASS,
            Priority::High => HIGH_PRIORITY_CLASS,
            Priority::Realtime => REALTIME_PRIORITY_CLASS,
        }
    }
    /// From the base priority reported by the kernel.
    pub fn from_base(b: i32) -> Priority {
        match b {
            ..=4 => Priority::Idle,
            5..=7 => Priority::BelowNormal,
            8 => Priority::Normal,
            9..=10 => Priority::AboveNormal,
            11..=15 => Priority::High,
            _ => Priority::Realtime,
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
pub enum RuleAction {
    SetPriority(Priority),
    SetAffinity(u64),
    Block,
    KillAboveMemoryMb(u64),
    KillAboveCpu { percent: f32, seconds: u32 },
    LowerPriorityAboveCpu { percent: f32, seconds: u32 },
}

impl RuleAction {
    pub fn label(&self) -> &'static str {
        match self {
            RuleAction::SetPriority(_) => "Set priority",
            RuleAction::SetAffinity(_) => "Limit to CPU cores",
            RuleAction::Block => "Block from running",
            RuleAction::KillAboveMemoryMb(_) => "End if memory exceeds",
            RuleAction::KillAboveCpu { .. } => "End if CPU stays above",
            RuleAction::LowerPriorityAboveCpu { .. } => "Throttle if CPU stays above",
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Rule {
    pub id: u64,
    pub enabled: bool,
    /// Executable name pattern, `*` and `?` wildcards, case-insensitive (e.g. `chrome.exe`, `*launcher*`).
    pub pattern: String,
    pub action: RuleAction,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum ViewMode {
    Grouped,
    Tree,
    Flat,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug, Hash)]
pub enum Col {
    Pid,
    Status,
    User,
    Cpu,
    Memory,
    Disk,
    Network,
    Gpu,
    GpuMem,
    Threads,
    Handles,
    Priority,
    Started,
    Conns,
    Path,
    CommandLine,
}

impl Col {
    /// Usage columns first, then the optional extras (Columns menu shows them in two groups).
    pub const ALL: [Col; 15] = [Col::Cpu, Col::Memory, Col::Disk, Col::Network, Col::Gpu, Col::Pid, Col::Status, Col::GpuMem, Col::Threads, Col::Handles, Col::Priority, Col::Started, Col::Conns, Col::Path, Col::CommandLine];
    pub fn label(self) -> &'static str {
        match self {
            Col::Pid => "PID",
            Col::Status => "Status",
            Col::User => "User",
            Col::Cpu => "CPU",
            Col::Memory => "Memory",
            Col::Disk => "Disk",
            Col::Network => "Network",
            Col::Gpu => "GPU",
            Col::GpuMem => "GPU memory",
            Col::Threads => "Threads",
            Col::Handles => "Handles",
            Col::Priority => "Priority",
            Col::Started => "Started",
            Col::Conns => "Connections",
            Col::Path => "Path",
            Col::CommandLine => "Command line",
        }
    }
    pub fn numeric(self) -> bool {
        matches!(self, Col::Pid | Col::Cpu | Col::Memory | Col::Disk | Col::Network | Col::Gpu | Col::GpuMem | Col::Threads | Col::Handles | Col::Conns)
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub refresh_ms: u64,
    pub view: ViewMode,
    pub columns: Vec<Col>,
    pub sort_col: Option<Col>,
    pub sort_desc: bool,
    pub pinned: Vec<String>,
    pub rules: Vec<Rule>,
    pub next_rule_id: u64,
    pub start_with_windows: bool,
    pub hook_ctrl_shift_esc: bool,
    pub hide_on_focus_loss: bool,
    pub confirm_end_task: bool,
    pub show_windows_group: bool,
    pub auto_update: bool,
    pub smooth_graphs: bool,
    pub sidebar_collapsed: bool,
    pub window_size: Option<[f32; 2]>,
    pub last_tab: usize,
    pub blocked_apps: Vec<String>,
    pub latency_targets: Vec<(String, String)>,
    pub history_days: u32,
    pub first_run_done: bool,
    pub always_on_top: bool,
    pub light_theme: bool,
    pub accent: usize,
    /// Whole-UI zoom (text size slider).
    pub text_scale: f32,
    pub compact_rows: bool,
    /// Tiny always-on-top metrics overlay.
    pub overlay: bool,
    /// 0 = top-right, 1 = top-left, 2 = bottom-right, 3 = bottom-left.
    pub overlay_corner: usize,
    /// Exe name of the last selected process, restored on the next start.
    pub last_selected: Option<String>,
    /// Bumped when the default layout changes; older configs are migrated once on load.
    #[serde(default)]
    pub layout_version: u32,
    pub startup_sort: (usize, bool),
}

impl Default for Config {
    fn default() -> Self {
        Self {
            refresh_ms: 1000,
            view: ViewMode::Grouped,
            columns: vec![Col::Cpu, Col::Memory, Col::Disk, Col::Network, Col::Gpu],
            sort_col: Some(Col::Cpu),
            sort_desc: true,
            pinned: Vec::new(),
            rules: Vec::new(),
            next_rule_id: 1,
            start_with_windows: true,
            hook_ctrl_shift_esc: true,
            hide_on_focus_loss: false,
            confirm_end_task: false,
            show_windows_group: true,
            auto_update: true,
            smooth_graphs: true,
            sidebar_collapsed: false,
            window_size: None,
            last_tab: 0,
            blocked_apps: Vec::new(),
            latency_targets: vec![("Internet (Cloudflare)".into(), "1.1.1.1".into()), ("Google DNS".into(), "8.8.8.8".into())],
            history_days: 7,
            first_run_done: false,
            always_on_top: false,
            light_theme: false,
            accent: 0,
            text_scale: 1.0,
            compact_rows: false,
            overlay: false,
            overlay_corner: 0,
            last_selected: None,
            layout_version: 2,
            startup_sort: (0, false),
        }
    }
}

pub fn test_mode() -> bool {
    std::env::var_os("SENTINEL_TEST").is_some()
}

pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    let d = base.join(if test_mode() { "Sentinel-test" } else { "Sentinel" });
    let _ = std::fs::create_dir_all(&d);
    d
}

impl Config {
    pub fn load() -> Self {
        let mut c: Self = std::fs::read(data_dir().join("config.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        // Single-user PC: the User column is gone (kept in the enum so old configs still parse).
        c.columns.retain(|x| *x != Col::User);
        if c.layout_version < 2 {
            // v2: Status and PID became extras (status shows inline, PID on hover / in details).
            c.columns.retain(|x| *x != Col::Status && *x != Col::Pid);
            c.layout_version = 2;
        }
        let order: Vec<Col> = Col::ALL.iter().copied().filter(|x| c.columns.contains(x)).collect();
        c.columns = order;
        if c.sort_col == Some(Col::User) {
            c.sort_col = Some(Col::Cpu);
        }
        c
    }
    pub fn save(&self) {
        if let Ok(b) = serde_json::to_vec_pretty(self) {
            let p = data_dir().join("config.json");
            let tmp = p.with_extension("json.tmp");
            if std::fs::write(&tmp, b).is_ok() {
                let _ = std::fs::rename(&tmp, &p);
            }
        }
    }
}

/// Case-insensitive glob with `*` and `?`.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let (mut pi, mut ti, mut star, mut mark) = (0usize, 0usize, None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::glob_match;
    #[test]
    fn globs() {
        assert!(glob_match("chrome.exe", "Chrome.EXE"));
        assert!(glob_match("*launcher*", "EpicGamesLauncher.exe"));
        assert!(glob_match("a?c.exe", "abc.exe"));
        assert!(!glob_match("chrome.exe", "chromedriver.exe"));
        assert!(glob_match("*", "x"));
    }
}
