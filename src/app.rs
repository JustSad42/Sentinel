//! Application shell: sidebar navigation, pages, toasts, modals, keyboard shortcuts, show/hide.

use crate::actions::{self, Locker};
use crate::admin::{self, AdminOp};
use crate::apps::App as InstalledApp;
use crate::config::{Config, Priority, Rule};
use crate::icons as I;
use crate::info::Info;
use crate::net::{Latency, Rdns};
use crate::sampler::{Control, Key, LogKind, Proc, Shared, Snapshot};
use crate::services::Service;
use crate::startup::StartupItem;
use crate::theme::*;
use crate::{integration, updater, win};
use egui::{Color32, CornerRadius, RichText, Sense, Stroke, Ui, vec2};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    Processes,
    Performance,
    Network,
    Apps,
    Startup,
    Services,
    History,
    Rules,
    Settings,
}

pub const PAGES: [(Page, &str, &str); 9] = [
    (Page::Processes, I::LIST_BULLETS, "Processes"),
    (Page::Performance, I::PULSE, "Performance"),
    (Page::Network, I::GLOBE, "Network"),
    (Page::Apps, I::PACKAGE, "Installed apps"),
    (Page::Startup, I::ROCKET_LAUNCH, "Startup"),
    (Page::Services, I::GEAR_SIX, "Services"),
    (Page::History, I::CLOCK_COUNTER_CLOCKWISE, "History"),
    (Page::Rules, I::LIGHTNING, "Rules"),
    (Page::Settings, I::SLIDERS_HORIZONTAL, "Settings"),
];

#[derive(Clone, Copy, PartialEq)]
pub enum ToastKind {
    Good,
    Bad,
    Info,
}

pub struct Toast {
    pub text: String,
    pub kind: ToastKind,
    pub born: f64,
    pub retry: Option<AdminOp>,
    /// "Undo" after ending apps: (program, arguments) to start again.
    pub undo: Option<Vec<(String, String)>>,
}

pub enum Modal {
    ConfirmKill { title: String, targets: Vec<(u32, i64)>, names: Vec<String>, critical: bool },
    Affinity { pid: u32, name: String, mask: u64, sys: u64 },
    RunTask { cmd: String, admin: bool },
    Lockers { path: String, result: Option<Result<Vec<Locker>, String>> },
    RemoveApps(Vec<InstalledApp>),
    Leftovers { app: String, items: Vec<(String, u64, bool)> },
    Shortcuts,
    UninstallRunning { name: String },
}

/// Results from background work.
pub enum Msg {
    Toast(ToastKind, String, Option<AdminOp>),
    Apps(Vec<InstalledApp>),
    Startup(Vec<StartupItem>),
    Services(Vec<Service>),
    Lockers(String, Result<Vec<Locker>, String>),
    Uninstalled(InstalledApp, bool, Vec<(String, u64)>),
    IfeoState(bool, bool),
    RefreshServices,
    RefreshStartup,
    RefreshApps,
    ServiceDesc(String, String),
}

pub struct App {
    pub cfg: Config,
    pub shared: Arc<Mutex<Shared>>,
    pub ctl: Arc<Control>,
    pub snap: Arc<Snapshot>,
    pub paused: bool,
    pub info: Arc<Info>,
    pub rdns: Arc<Rdns>,
    pub latency: Arc<Latency>,
    pub textures: HashMap<String, egui::TextureHandle>,
    pub page: Page,
    pub tx: Sender<Msg>,
    rx: Receiver<Msg>,
    pub toasts: Vec<Toast>,
    pub modal: Option<Modal>,
    pub hwnd: isize,
    pub show_flag: Arc<AtomicBool>,
    pub visible: bool,
    pub elevated: bool,
    pub update: updater::Shared,
    pub auto_update: Arc<AtomicBool>,
    quit: bool,
    last_save: Instant,
    pub instance: Option<Arc<integration::Instance>>,

    // processes
    pub search: String,
    pub selected: Option<Key>,
    pub sel_group: Option<String>,
    pub details_open: bool,
    pub collapsed: HashSet<String>,
    pub expanded_groups: HashSet<String>,
    pub tree_collapsed: HashSet<Key>,
    pub sort_cache: std::cell::RefCell<crate::ui_procs::SortCache>,
    pub palette: crate::palette::Palette,
    pub overlay_styled: bool,
    /// When the window was last shown / hidden, and whether it has had focus since showing.
    shown_at: Instant,
    hidden_at: Instant,
    had_focus: bool,
    /// Started hidden: the window is cloaked until eframe's first paint (which force-shows it) is undone.
    start_cloaked: bool,
    /// (opacity, click-through) last applied to the overlay window.
    pub overlay_style: (f32, bool),
    /// Exe name to re-select once its process shows up (selection survives restarts).
    pub pending_select: Option<String>,
    /// File/folder from Explorer's "What's using this?" waiting to be checked.
    pub pending_locks: Option<String>,
    pub only_hung: bool,
    pub only_suspended: bool,
    pub scroll_to_sel: bool,
    pub focus_search: bool,
    pub visible_rows: Vec<Key>,

    // performance
    pub perf_sel: usize,
    pub per_core: bool,
    /// Performance graphs: 0 = live, 1 = last hour, 2 = last 24 hours.
    pub perf_range: usize,
    /// Zoomed time window on the Performance graphs (unix start, end).
    pub perf_zoom: Option<(f64, f64)>,
    pub perf_expanded: bool,
    /// Clicked moment on a Performance graph: (unix, bucket seconds, by memory?, screen pos, frame).
    pub perf_pick: Option<(f64, f64, bool, egui::Pos2, u64)>,
    /// Long-range series, rebuilt only when the window moves (see ui_perf::LongSeries).
    pub perf_cache: Option<((i64, i64, usize), std::rc::Rc<crate::ui_perf::LongSeries>)>,
    /// Details panel history range: 0 = 6 hours, 1 = 24 hours.
    pub app_range: usize,
    /// Rules page: 0 = when an app runs, 1 = scheduled.
    pub rules_tab: usize,
    /// Apps whose memory keeps growing (refreshed every minute) and the ones already announced.
    pub leaks: (Option<Instant>, Vec<crate::apphist::Leak>),
    pub leak_told: std::collections::HashSet<String>,

    // network
    pub net_tab: usize,
    pub net_search: String,
    pub net_hide_listen: bool,
    pub net_only_active: bool,
    pub net_group_proc: bool,
    pub new_target: (String, String),

    // installed apps
    pub apps: Option<Vec<InstalledApp>>,
    pub apps_loading: bool,
    pub apps_search: String,
    pub apps_filter: usize,
    pub apps_sort: usize,
    pub apps_checked: HashSet<String>,

    // startup / services
    pub startup: Option<Vec<StartupItem>>,
    /// Last boot timeline / startup impact (boot.json), reloaded every few seconds on the Startup page.
    pub boot_rec: (Option<Instant>, Arc<crate::boot::BootRecord>),
    pub startup_loading: bool,
    pub services: Option<Vec<Service>>,
    pub services_loading: bool,
    pub svc_search: String,
    pub startup_search: String,
    pub svc_filter: usize,
    pub svc_selected: Option<String>,
    pub svc_desc: HashMap<String, String>,

    // history
    pub hist_tab: usize,
    pub hist_search: String,
    pub hist_range: usize,
    pub hist_spikes: bool,
    pub hist_sort: (usize, bool),

    // settings
    pub ifeo: Option<bool>,
    pub logon_task: Option<bool>,
    pub start_ms: f64,
}

pub fn admin_needed(e: &str) -> bool {
    actions::is_access_denied(e)
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, instance: Option<integration::Instance>, start_hidden: bool, start_ms: f64) -> Self {
        let cfg = Config::load();
        crate::theme::set_palette(cfg.light_theme, cfg.accent);
        install(&cc.egui_ctx);
        crate::theme::apply_theme(&cc.egui_ctx, &cfg);
        let (history, log) = crate::sampler::load_history();
        let long = crate::sampler::load_long();
        let shared = Arc::new(Mutex::new(Shared {
            snap: Arc::new(Snapshot::default()),
            sys: Default::default(),
            long: long.clone(),
            week: crate::sampler::load_week(&long),
            proc_hist: HashMap::new(),
            apphist: crate::apphist::AppHist::load(),
            history,
            running_idx: HashMap::new(),
            log,
            rules: cfg.rules.clone(),
            schedules: cfg.schedules.clone(),
            sched_last: crate::sampler::load_sched_last(),
            sched_run_now: Vec::new(),
        }));
        let ctl = Arc::new(Control {
            thread: Default::default(),
            interval_ms: cfg.refresh_ms.into(),
            visible: (!start_hidden).into(),
            refresh_now: false.into(),
            want_conns: false.into(),
            overlay: false.into(),
            repaint: Mutex::new(Some(cc.egui_ctx.clone())),
            info: Mutex::new(None),
        });
        let info = Info::new();
        info.set_ctx(cc.egui_ctx.clone());
        *ctl.info.lock().unwrap() = Some(info.clone());
        crate::sampler::Sampler::spawn(shared.clone(), ctl.clone());

        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let hwnd = match cc.window_handle().map(|h| h.as_raw()) {
            Ok(RawWindowHandle::Win32(h)) => h.hwnd.get(),
            _ => 0,
        };
        let show_flag = Arc::new(AtomicBool::new(false));
        let instance = instance.map(Arc::new);
        if let Some(inst) = &instance {
            integration::start_show_listener(inst, hwnd, cc.egui_ctx.clone(), show_flag.clone());
        }
        integration::HOOK_ENABLED.store(cfg.hook_ctrl_shift_esc, Ordering::Relaxed);
        if !crate::config::test_mode() || std::env::var_os("SENTINEL_TEST_HOOK").is_some() {
            integration::start_hook();
        }

        let (tx, rx) = channel();
        let update = Arc::new(Mutex::new(updater::State::Idle));
        let auto_update = Arc::new(AtomicBool::new(cfg.auto_update && !integration::is_dev_build()));
        updater::spawn_auto(update.clone(), auto_update.clone());
        updater::cleanup();

        let latency = Latency::new();
        *latency.targets.lock().unwrap() = cfg.latency_targets.clone();

        let mut cfg = cfg;
        if !cfg.first_run_done {
            cfg.first_run_done = true;
            cfg.save();
        }
        // Installed + elevated: keep the launch task in sync with this exe and the "start at sign-in" choice.
        if !integration::is_dev_build() && win::is_elevated() {
            let exe = integration::exe_path();
            let at_logon = cfg.start_with_windows;
            std::thread::spawn(move || {
                let _ = admin::ensure_task(&exe, at_logon);
                // Older builds used the Run key, which can't start elevated apps.
                let _ = integration::set_autostart(false);
            });
        }
        let mut page = PAGES.get(cfg.last_tab).map(|p| p.0).unwrap_or(Page::Processes);
        // Test hook for screenshots: SENTINEL_PAGE=<index>
        if let Some(i) = std::env::var("SENTINEL_PAGE").ok().and_then(|v| v.parse::<usize>().ok()).filter(|_| crate::config::test_mode()) {
            page = PAGES.get(i).map(|p| p.0).unwrap_or(page);
        }

        let me = Self {
            snap: Arc::new(Snapshot::default()),
            paused: false,
            rdns: Rdns::new(),
            latency,
            textures: HashMap::new(),
            page,
            tx,
            rx,
            toasts: Vec::new(),
            modal: None,
            hwnd,
            show_flag,
            visible: !start_hidden,
            elevated: win::is_elevated(),
            update,
            auto_update,
            quit: false,
            last_save: Instant::now(),
            instance,
            search: std::env::var("SENTINEL_SEARCH").ok().filter(|_| crate::config::test_mode()).unwrap_or_default(),
            selected: None,
            sel_group: None,
            details_open: true,
            collapsed: HashSet::new(),
            expanded_groups: HashSet::new(),
            tree_collapsed: HashSet::new(),
            sort_cache: Default::default(),
            palette: crate::palette::Palette::test_open(),
            overlay_styled: false,
            shown_at: Instant::now(),
            hidden_at: Instant::now(),
            had_focus: false,
            start_cloaked: start_hidden && win::set_cloaked(hwnd, true),
            overlay_style: (1.0, false),
            pending_select: cfg.last_selected.clone(),
            pending_locks: integration::take_locks_request(),
            only_hung: false,
            only_suspended: false,
            scroll_to_sel: false,
            focus_search: false,
            visible_rows: Vec::new(),
            perf_sel: std::env::var("SENTINEL_PERF").ok().and_then(|v| v.parse::<usize>().ok()).filter(|_| crate::config::test_mode()).map(|i| 100 + i).unwrap_or(0),
            per_core: false,
            perf_range: std::env::var("SENTINEL_RANGE").ok().and_then(|v| v.parse::<usize>().ok()).filter(|_| crate::config::test_mode()).unwrap_or(0),
            perf_zoom: None,
            perf_expanded: crate::config::test_mode() && std::env::var_os("SENTINEL_EXPAND").is_some(),
            perf_pick: std::env::var("SENTINEL_PICK_AGO").ok().and_then(|v| v.parse::<f64>().ok()).filter(|_| crate::config::test_mode()).map(|a| (crate::win::now_unix() as f64 - a, 576.0, false, egui::pos2(760.0, 330.0), 0)),
            perf_cache: None,
            app_range: 1,
            rules_tab: std::env::var("SENTINEL_RULES_TAB").ok().and_then(|v| v.parse().ok()).filter(|_| crate::config::test_mode()).unwrap_or(0),
            leaks: (None, Vec::new()),
            leak_told: Default::default(),
            net_tab: std::env::var("SENTINEL_NET").ok().and_then(|v| v.parse::<usize>().ok()).filter(|_| crate::config::test_mode()).unwrap_or(0),
            net_search: String::new(),
            net_hide_listen: false,
            net_only_active: false,
            net_group_proc: false,
            new_target: (String::new(), String::new()),
            apps: None,
            apps_loading: false,
            apps_search: String::new(),
            apps_filter: 0,
            apps_sort: 0,
            apps_checked: HashSet::new(),
            startup: None,
            boot_rec: (None, Arc::new(crate::boot::BootRecord::default())),
            startup_loading: false,
            services: None,
            services_loading: false,
            svc_search: String::new(),
            startup_search: String::new(),
            svc_filter: 0,
            svc_selected: None,
            svc_desc: HashMap::new(),
            hist_tab: 0,
            hist_search: String::new(),
            hist_range: 1,
            hist_spikes: false,
            hist_sort: (0, true),
            ifeo: None,
            logon_task: None,
            start_ms,
            shared,
            ctl,
            info,
            cfg,
        };
        me.refresh_integration_state();
        me
    }

    pub fn refresh_integration_state(&self) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Msg::IfeoState(admin::ifeo_enabled(), admin::logon_task_enabled()));
        });
    }

    // ------------------------------------------------------------ helpers

    pub fn toast(&mut self, kind: ToastKind, text: impl Into<String>) {
        self.toasts.push(Toast { text: text.into(), kind, born: 0.0, retry: None, undo: None });
    }

    pub fn toast_retry(&mut self, text: impl Into<String>, op: AdminOp) {
        self.toasts.push(Toast { text: text.into(), kind: ToastKind::Bad, born: 0.0, retry: Some(op), undo: None });
    }

    pub fn log(&self, kind: LogKind, text: impl Into<String>) {
        self.shared.lock().unwrap().log(kind, text);
    }

    pub fn tex(&mut self, ctx: &egui::Context, spec: &str) -> Option<egui::TextureHandle> {
        if spec.is_empty() {
            return None;
        }
        if let Some(t) = self.textures.get(spec) {
            return Some(t.clone());
        }
        let img = self.info.icon(spec)?;
        let (w, h, px) = (&img.0, &img.1, &img.2);
        let ci = egui::ColorImage::from_rgba_unmultiplied([*w as usize, *h as usize], px);
        let t = ctx.load_texture(spec, ci, egui::TextureOptions::LINEAR);
        self.textures.insert(spec.to_string(), t.clone());
        Some(t)
    }

    /// Draws a process icon: its own, a parent app's, or a letter badge (system processes get a glyph).
    pub fn paint_proc_icon(&mut self, ui: &Ui, rect: egui::Rect, p: &Proc) {
        let ctx = ui.ctx().clone();
        let meta = self.info.meta(p.key(), p.path.as_ref());
        let spec = meta.as_ref().and_then(|m| m.icon_key.clone());
        let tex = spec.as_deref().and_then(|s| self.tex(&ctx, s));
        if tex.is_some() || p.path.is_none() {
            crate::theme::paint_icon(ui, rect, tex.as_ref(), if p.window.is_some() { crate::icons::APP_WINDOW } else { crate::icons::GEAR_SIX });
        } else {
            let name = meta.and_then(|m| m.description.clone()).filter(|d| !d.is_empty()).unwrap_or_else(|| p.name.clone());
            crate::theme::paint_avatar(ui, rect, &name);
        }
    }

    /// Runs an action that may need admin; on "access denied" offers a one-click elevated retry.
    pub fn try_or_admin(&mut self, done: &str, res: Result<(), String>, op: AdminOp) {
        match res {
            Ok(()) => {
                self.toast(ToastKind::Good, done);
                self.log(LogKind::Action, done);
            }
            Err(e) if admin_needed(&e) && !self.elevated => self.toast_retry(format!("{e}. This needs admin rights."), op),
            Err(e) => self.toast(ToastKind::Bad, e),
        }
        self.ctl.refresh();
    }

    pub fn run_admin_async(&self, op: AdminOp, done: String, after: Option<Msg>) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            match admin::run(op) {
                Ok(()) => {
                    let _ = tx.send(Msg::Toast(ToastKind::Good, done, None));
                }
                Err(e) => {
                    let _ = tx.send(Msg::Toast(ToastKind::Bad, e, None));
                }
            }
            if let Some(m) = after {
                let _ = tx.send(m);
            }
        });
    }

    pub fn kill_targets(&mut self, targets: Vec<(u32, i64)>, names: Vec<String>, title: String) {
        let critical = self.snap.procs.iter().any(|p| targets.contains(&(p.pid, p.create_time)) && crate::sampler::critical(&p.name));
        if critical || self.cfg.confirm_end_task || targets.len() > 1 {
            self.modal = Some(Modal::ConfirmKill { title, targets, names, critical });
        } else {
            self.do_kill(targets, names);
        }
    }

    /// What to start again if the user hits Undo: only the top process of each ended app (its
    /// helpers are started by it), never services or Windows' own processes.
    fn relaunch_plan(&self, targets: &[(u32, i64)]) -> Vec<(String, String)> {
        let set: std::collections::HashSet<u32> = targets.iter().map(|t| t.0).collect();
        let mut out: Vec<(String, String)> = Vec::new();
        for (pid, ct) in targets {
            let Some(p) = self.snap.procs.iter().find(|p| p.pid == *pid && p.create_time == *ct) else { continue };
            let Some(path) = p.path.as_deref() else { continue };
            let lower = path.to_lowercase();
            if set.contains(&p.ppid) || crate::sampler::critical(&p.name) || self.snap.services_by_pid.contains_key(&p.pid) || lower.contains("\\windows\\system32\\") {
                continue;
            }
            let cmd = self.info.meta(p.key(), p.path.as_ref()).and_then(|m| m.cmdline.clone()).or_else(|| win::command_line(p.pid)).unwrap_or_default();
            let args = actions::split_args(&cmd).1;
            if !out.iter().any(|(e, a)| e.eq_ignore_ascii_case(path) && *a == args) {
                out.push((path.to_string(), args));
            }
        }
        out.truncate(8);
        out
    }

    /// Re-checks for apps with steadily growing memory once a minute; announces each one once.
    pub fn refresh_leaks(&mut self) {
        if self.leaks.0.is_some_and(|t| t.elapsed().as_secs() < 60) {
            return;
        }
        let list = self.shared.lock().unwrap().apphist.leaks(win::now_unix());
        for l in &list {
            if self.leak_told.insert(l.name.clone()) {
                let shown = self.snap.procs.iter().find(|p| p.name.eq_ignore_ascii_case(&l.name)).and_then(|p| self.info.meta(p.key(), p.path.as_ref()).and_then(|m| m.description.clone())).filter(|d| !d.is_empty()).unwrap_or_else(|| l.name.clone());
                let text = format!("{shown} may be leaking memory. {}", l.text());
                self.log(LogKind::Info, text.clone());
                self.toast(ToastKind::Info, text);
            }
        }
        self.leaks = (Some(Instant::now()), list);
    }

    pub fn leak_for(&self, name: &str) -> Option<&crate::apphist::Leak> {
        self.leaks.1.iter().find(|l| l.name.eq_ignore_ascii_case(name))
    }

    pub fn do_kill(&mut self, targets: Vec<(u32, i64)>, names: Vec<String>) {
        let plan = self.relaunch_plan(&targets);
        let (ok, errs) = actions::kill_many(&targets);
        let label = if names.len() == 1 { names[0].clone() } else { format!("{} processes", targets.len()) };
        if errs.is_empty() {
            self.toasts.push(Toast { text: format!("Ended {label}"), kind: ToastKind::Good, born: 0.0, retry: None, undo: if plan.is_empty() { None } else { Some(plan) } });
            self.log(LogKind::Action, format!("Ended {label}"));
        } else if errs.iter().any(|e| admin_needed(e)) && !self.elevated {
            let msg = if ok > 0 { format!("Ended {ok}, {} need admin rights", errs.len()) } else { format!("Couldn't end {label}: access denied") };
            self.toast_retry(msg, AdminOp::KillMany(targets));
        } else {
            self.toast(ToastKind::Bad, format!("Couldn't end {label}: {}", errs[0]));
        }
        self.ctl.refresh();
    }

    pub fn set_priority(&mut self, p: &Proc, pr: Priority) {
        let r = actions::set_priority(p.pid, pr);
        self.try_or_admin(&format!("{} set to {} priority", p.name, pr.label()), r, AdminOp::SetPriority(p.pid, pr));
    }

    pub fn add_rule_for(&mut self, name: &str) {
        let id = self.cfg.next_rule_id;
        self.cfg.next_rule_id += 1;
        self.cfg.rules.push(Rule { id, enabled: true, pattern: name.to_string(), action: crate::config::RuleAction::SetPriority(Priority::BelowNormal) });
        self.sync_rules();
        self.page = Page::Rules;
    }

    pub fn sync_rules(&mut self) {
        {
            let mut sh = self.shared.lock().unwrap();
            sh.rules = self.cfg.rules.clone();
            sh.schedules = self.cfg.schedules.clone();
        }
        self.cfg.save();
    }

    pub fn copy(&mut self, ctx: &egui::Context, text: String) {
        ctx.copy_text(text);
        self.toast(ToastKind::Info, "Copied to clipboard");
    }

    pub fn hide(&mut self, ctx: &egui::Context) {
        if crate::config::test_mode() && std::env::var_os("SENTINEL_TEST_HIDE").is_none() {
            self.quit(ctx);
            return;
        }
        self.visible = false;
        self.hidden_at = Instant::now();
        self.ctl.visible.store(false, Ordering::Relaxed);
        self.latency.active.store(false, Ordering::Relaxed);
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        self.save_all();
        // Free icon textures while hidden; they reload instantly from the cache.
        self.textures.clear();
    }

    pub fn quit(&mut self, ctx: &egui::Context) {
        self.quit = true;
        self.save_all();
        net_stop();
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    pub fn save_all(&mut self) {
        self.cfg.last_tab = PAGES.iter().position(|p| p.0 == self.page).unwrap_or(0);
        if let Some(k) = self.selected {
            if let Some(p) = self.snap.procs.iter().find(|p| p.key() == k) {
                self.cfg.last_selected = Some(p.name.clone());
            }
        }
        self.cfg.save();
        let sh = self.shared.lock().unwrap();
        crate::sampler::save_history(&sh, self.cfg.history_days);
        self.last_save = Instant::now();
    }

    pub fn export_report(&mut self) {
        crate::report::export(self);
    }

    pub fn set_on_top(&mut self, ctx: &egui::Context, on: bool) {
        self.cfg.always_on_top = on;
        self.cfg.save();
        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(if on { egui::WindowLevel::AlwaysOnTop } else { egui::WindowLevel::Normal }));
    }

    fn handle_msgs(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Toast(k, t, retry) => {
                    if k == ToastKind::Good {
                        self.log(LogKind::Action, t.clone());
                    }
                    self.toasts.push(Toast { text: t, kind: k, born: 0.0, retry, undo: None });
                }
                Msg::Apps(a) => {
                    self.apps = Some(a);
                    self.apps_loading = false;
                }
                Msg::Startup(s) => {
                    self.startup = Some(s);
                    self.startup_loading = false;
                }
                Msg::Services(s) => {
                    self.services = Some(s);
                    self.services_loading = false;
                }
                Msg::Lockers(path, r) => {
                    if let Some(Modal::Lockers { path: p, result }) = &mut self.modal {
                        if *p == path {
                            *result = Some(r);
                        }
                    }
                }
                Msg::Uninstalled(app, still, left) => self.after_uninstall(app, still, left),
                Msg::IfeoState(i, t) => {
                    self.ifeo = Some(i);
                    self.logon_task = Some(t);
                }
                Msg::RefreshServices => self.load_services(),
                Msg::RefreshStartup => self.load_startup(),
                Msg::RefreshApps => self.load_apps(),
                Msg::ServiceDesc(n, d) => {
                    self.svc_desc.insert(n, d);
                }
            }
        }
    }

    pub fn load_apps(&mut self) {
        if self.apps_loading {
            return;
        }
        self.apps_loading = true;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Apps(crate::apps::list()));
        });
    }

    pub fn load_startup(&mut self) {
        if self.startup_loading {
            return;
        }
        self.startup_loading = true;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Startup(crate::startup::list()));
        });
    }

    pub fn load_services(&mut self) {
        if self.services_loading {
            return;
        }
        self.services_loading = true;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Services(crate::services::list_full()));
        });
    }

    pub fn find_lockers(&mut self, path: String) {
        self.modal = Some(Modal::Lockers { path: path.clone(), result: None });
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let r = actions::lockers(&path);
            let _ = tx.send(Msg::Lockers(path, r));
        });
    }

    // ------------------------------------------------------------ frame

    fn on_show(&mut self, ctx: &egui::Context) {
        self.visible = true;
        self.shown_at = Instant::now();
        self.had_focus = false;
        if std::mem::take(&mut self.start_cloaked) {
            win::set_cloaked(self.hwnd, false);
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        self.ctl.visible.store(true, Ordering::Relaxed);
        self.ctl.refresh();
        self.elevated = win::is_elevated();
        if self.page == Page::Processes {
            self.focus_search = true;
        }
        if let Some(p) = integration::take_locks_request() {
            self.find_lockers(p);
        }
        ctx.request_repaint();
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::K)) {
            self.palette.toggle();
        }
        if self.palette.open {
            return;
        }
        let typing = ctx.memory(|m| m.focused().is_some());
        let (esc, ctrl_f, f5, ctrl_w, ctrl_q, ctrl_n, f1, ctrl_p, num) = ctx.input(|i| {
            let num = (1..=9).find(|n| i.modifiers.command && i.key_pressed(match n {
                1 => egui::Key::Num1,
                2 => egui::Key::Num2,
                3 => egui::Key::Num3,
                4 => egui::Key::Num4,
                5 => egui::Key::Num5,
                6 => egui::Key::Num6,
                7 => egui::Key::Num7,
                8 => egui::Key::Num8,
                _ => egui::Key::Num9,
            }));
            (
                i.key_pressed(egui::Key::Escape),
                i.modifiers.command && i.key_pressed(egui::Key::F),
                i.key_pressed(egui::Key::F5),
                i.modifiers.command && i.key_pressed(egui::Key::W),
                i.modifiers.command && i.key_pressed(egui::Key::Q),
                i.modifiers.command && i.key_pressed(egui::Key::N),
                i.key_pressed(egui::Key::F1),
                i.modifiers.command && i.key_pressed(egui::Key::P),
                num,
            )
        });
        if let Some(n) = num {
            self.page = PAGES[n - 1].0;
        }
        if esc {
            if self.modal.is_some() {
                self.modal = None;
            } else if typing {
                ctx.memory_mut(|m| {
                    if let Some(id) = m.focused() {
                        m.surrender_focus(id)
                    }
                });
                if !self.search.is_empty() && self.page == Page::Processes {
                    self.search.clear();
                }
            } else if self.page == Page::Processes && !self.search.is_empty() {
                self.search.clear();
            } else {
                self.hide(ctx);
            }
        }
        if ctrl_f {
            self.focus_search = true;
        }
        if f5 {
            self.ctl.refresh();
            match self.page {
                Page::Apps => self.load_apps(),
                Page::Startup => self.load_startup(),
                Page::Services => self.load_services(),
                _ => {}
            }
        }
        if ctrl_p {
            self.paused = !self.paused;
        }
        if ctrl_w {
            self.hide(ctx);
        }
        if ctrl_q {
            self.quit(ctx);
        }
        if ctrl_n {
            self.modal = Some(Modal::RunTask { cmd: String::new(), admin: false });
        }
        if f1 {
            self.modal = Some(Modal::Shortcuts);
        }
        // Drop a file anywhere: what's locking it?
        let dropped: Vec<String> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_string_lossy().to_string()).filter(|s| !s.is_empty()).collect());
        if let Some(p) = dropped.into_iter().next() {
            self.find_lockers(p);
        }
    }

    fn sidebar(&mut self, ui: &mut Ui) {
        let collapsed = self.cfg.sidebar_collapsed;
        let w = ui.ctx().animate_value_with_time(egui::Id::unique("sbw"), if collapsed { 64.0 } else { 212.0 }, 0.16);
        egui::Panel::left("sidebar").exact_size(w).resizable(false).show_separator_line(false).frame(egui::Frame::new().fill(SIDEBAR()).inner_margin(egui::Margin { left: 10, right: 10, top: 14, bottom: 12 })).show(ui, |ui| {
            let full = w > 150.0;
            // Brand
            ui.horizontal(|ui| {
                let (r, resp) = ui.allocate_exact_size(vec2(36.0, 36.0), Sense::click());
                let painter = ui.painter();
                painter.rect_filled(r, CornerRadius::same(10), ACCENT());
                painter.text(r.center(), egui::Align2::CENTER_CENTER, I::PULSE, egui::FontId::proportional(20.0), Color32::WHITE);
                if resp.on_hover_text(if collapsed { "Expand sidebar" } else { "Collapse sidebar" }).clicked() {
                    self.cfg.sidebar_collapsed = !collapsed;
                }
                if full {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.label(RichText::new("Sentinel").font(bold(16.0)));
                        ui.label(RichText::new(format!("v{}", updater::VERSION)).size(11.0).color(FAINT()));
                    });
                }
            });
            ui.add_space(18.0);
            let snap = self.snap.clone();
            let sys_cpu = snap.cpu_total;
            let sel_idx = PAGES.iter().position(|p| p.0 == self.page).unwrap_or(0);
            let mut rects = Vec::new();
            for (i, (page, icon, label)) in PAGES.iter().enumerate() {
                if *page == Page::Settings {
                    continue;
                }
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 38.0), Sense::click());
                rects.push((i, rect));
                let hover = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
                if i != sel_idx && hover > 0.0 {
                    ui.painter().rect_filled(rect, CornerRadius::same(9), CARD_HI().gamma_multiply(hover * 0.8));
                }
                let active = i == sel_idx;
                let fg = if active { TEXT() } else { lerp_color(MUTED(), TEXT(), hover) };
                let ic = if active { ACCENT_HI() } else { fg };
                let x = if full { rect.left() + 14.0 } else { rect.center().x };
                let anchor = if full { egui::Align2::LEFT_CENTER } else { egui::Align2::CENTER_CENTER };
                ui.painter().text(egui::pos2(x, rect.center().y), anchor, *icon, egui::FontId::proportional(17.0), ic);
                if full {
                    ui.painter().text(egui::pos2(rect.left() + 42.0, rect.center().y), egui::Align2::LEFT_CENTER, *label, if active { bold(13.5) } else { egui::FontId::proportional(13.5) }, fg);
                    let badge = match page {
                        Page::Processes => Some(format!("{}", snap.proc_count)),
                        Page::Performance => Some(format!("{:.0}%", sys_cpu)),
                        _ => None,
                    };
                    if let Some(b) = badge {
                        ui.painter().text(egui::pos2(rect.right() - 10.0, rect.center().y), egui::Align2::RIGHT_CENTER, b, egui::FontId::proportional(11.5), FAINT());
                    }
                }
                let resp = if full { resp } else { resp.on_hover_text(*label) };
                if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    self.page = *page;
                }
            }
            // Sliding selection indicator
            if let Some((_, r)) = rects.iter().find(|(i, _)| *i == sel_idx) {
                let y = ui.ctx().animate_value_with_time(egui::Id::unique("sbsel"), r.top(), 0.16);
                let hl = egui::Rect::from_min_size(egui::pos2(r.left(), y), r.size());
                ui.painter().rect_filled(hl, CornerRadius::same(9), ACCENT().gamma_multiply(0.14));
                ui.painter().rect_filled(egui::Rect::from_min_size(egui::pos2(hl.left(), hl.top() + 9.0), vec2(3.0, hl.height() - 18.0)), CornerRadius::same(2), ACCENT_HI());
            }

            ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                // Bottom: settings, admin, mini meters
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 38.0), Sense::click());
                let active = self.page == Page::Settings;
                let hover = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
                ui.painter().rect_filled(rect, CornerRadius::same(9), if active { ACCENT().gamma_multiply(0.14) } else { CARD_HI().gamma_multiply(hover * 0.8) });
                let x = if full { rect.left() + 14.0 } else { rect.center().x };
                let anchor = if full { egui::Align2::LEFT_CENTER } else { egui::Align2::CENTER_CENTER };
                ui.painter().text(egui::pos2(x, rect.center().y), anchor, I::SLIDERS_HORIZONTAL, egui::FontId::proportional(17.0), if active { ACCENT_HI() } else { MUTED() });
                if full {
                    ui.painter().text(egui::pos2(rect.left() + 42.0, rect.center().y), egui::Align2::LEFT_CENTER, "Settings", egui::FontId::proportional(13.5), if active { TEXT() } else { MUTED() });
                }
                if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    self.page = Page::Settings;
                }
                ui.add_space(4.0);
                // Always-on-top pin
                let on_top = self.cfg.always_on_top;
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
                let hover = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
                ui.painter().rect_filled(rect, CornerRadius::same(9), if on_top { ACCENT().gamma_multiply(0.14) } else { CARD_HI().gamma_multiply(hover * 0.8) });
                let x = if full { rect.left() + 14.0 } else { rect.center().x };
                let anchor = if full { egui::Align2::LEFT_CENTER } else { egui::Align2::CENTER_CENTER };
                ui.painter().text(egui::pos2(x, rect.center().y), anchor, I::PUSH_PIN, egui::FontId::proportional(16.0), if on_top { ACCENT_HI() } else { MUTED() });
                if full {
                    ui.painter().text(egui::pos2(rect.left() + 42.0, rect.center().y), egui::Align2::LEFT_CENTER, "Always on top", egui::FontId::proportional(13.0), if on_top { TEXT() } else { MUTED() });
                }
                if resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("Keep Sentinel above other windows").clicked() {
                    self.set_on_top(ui.ctx(), !on_top);
                }
                ui.add_space(4.0);
                if full {
                    ui.horizontal(|ui| {
                        ui.add_space(6.0);
                        if self.elevated {
                            pill(ui, &format!("{}  Administrator", I::SHIELD_CHECK), GOOD());
                        } else {
                            pill(ui, &format!("{}  Limited (UAC declined)", I::SHIELD_WARNING), WARN()).on_hover_text("Restart Sentinel and accept the prompt for full control");
                        }
                    });
                }
                ui.add_space(10.0);
                if full {
                    // Bottom-up layout: draw the last meter first so the list reads top to bottom.
                    let items = self.cfg.sidebar_meters.clone();
                    let r = ui
                        .scope(|ui| {
                            // Compact rows; skip the ones that would run into the menu on short windows.
                            let fit = ((ui.available_height() - 8.0) / 38.0).floor().max(0.0) as usize;
                            let shown: Vec<_> = items.iter().take(fit).collect();
                            for it in shown.into_iter().rev() {
                                let (text, color, frac) = self.meter_value(*it);
                                let w = ui.available_width();
                                let (r, _) = ui.allocate_exact_size(vec2(w, 36.0), Sense::hover());
                                let p = ui.painter();
                                p.text(r.left_top() + vec2(8.0, 2.0), egui::Align2::LEFT_TOP, it.label(), egui::FontId::proportional(11.5), MUTED());
                                p.text(r.right_top() + vec2(-8.0, 1.0), egui::Align2::RIGHT_TOP, &text, bold(12.0), TEXT());
                                let bar = egui::Rect::from_min_size(r.left_top() + vec2(8.0, 22.0), vec2(w - 16.0, 5.0));
                                p.rect_filled(bar, CornerRadius::same(3), BORDER());
                                p.rect_filled(egui::Rect::from_min_size(bar.min, vec2((bar.width() * frac).max(if frac > 0.0 { 3.0 } else { 0.0 }), 5.0)), CornerRadius::same(3), color);
                            }
                            if items.is_empty() {
                                ui.label(RichText::new("Right-click to add meters").size(11.0).color(FAINT()));
                            }
                        })
                        .response;
                    let r = ui.interact(r.rect, egui::Id::unique("sidebar-meters"), Sense::click()).on_hover_text("Right-click to choose which meters show");
                    r.context_menu(|ui| {
                        ui.label(RichText::new("Show in the sidebar").size(11.5).color(FAINT()));
                        for it in crate::config::OverlayItem::ALL {
                            let mut on = self.cfg.sidebar_meters.contains(&it);
                            if ui.checkbox(&mut on, it.label()).changed() {
                                if on {
                                    self.cfg.sidebar_meters.push(it);
                                    let order = crate::config::OverlayItem::ALL;
                                    self.cfg.sidebar_meters.sort_by_key(|x| order.iter().position(|o| *o == *x));
                                } else {
                                    self.cfg.sidebar_meters.retain(|x| *x != it);
                                }
                                self.cfg.save();
                            }
                        }
                    });
                }
            });
        });
    }

    fn update_banner(&mut self, ui: &mut Ui) {
        let st = self.update.lock().unwrap().clone();
        if let updater::State::Ready { version, path, .. } = st {
            egui::Frame::new().fill(ACCENT_DIM()).corner_radius(CornerRadius::same(10)).inner_margin(egui::Margin::symmetric(14, 8)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(I::SPARKLE).color(ACCENT_HI()).size(16.0));
                    ui.label(RichText::new(format!("Sentinel {version} is ready")).font(bold(13.0)));
                    ui.label(RichText::new("It installs automatically next time the window is hidden.").color(MUTED()).size(12.5));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if button_sized(ui, Btn::Primary, I::ARROWS_CLOCKWISE, "Restart now", true, 28.0).clicked() {
                            self.apply_update(ui.ctx(), &path, true);
                        }
                    });
                });
            });
            ui.add_space(8.0);
        }
    }

    pub fn apply_update(&mut self, ctx: &egui::Context, path: &std::path::Path, show: bool) {
        self.save_all();
        match updater::apply(path, show) {
            Ok(()) => {
                let inst = self.instance.take();
                drop(inst);
                self.quit = true;
                net_stop();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Err(e) => {
                *self.update.lock().unwrap() = updater::State::Error(e.clone());
                self.toast(ToastKind::Bad, e);
            }
        }
    }

    fn toasts_ui(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        let mut retry: Option<AdminOp> = None;
        let mut undo: Option<Vec<(String, String)>> = None;
        let mut close: Option<usize> = None;
        for t in self.toasts.iter_mut() {
            if t.born == 0.0 {
                t.born = now;
            }
        }
        let life_of = |t: &Toast| if t.retry.is_some() || t.undo.is_some() { 9.0 } else { 4.0 };
        self.toasts.retain(|t| now - t.born < life_of(t));
        if self.toasts.len() > 4 {
            let n = self.toasts.len() - 4;
            self.toasts.drain(..n);
        }
        let screen = ctx.content_rect();
        let mut y = screen.bottom() - 46.0;
        for (i, t) in self.toasts.iter().enumerate().rev() {
            let age = (now - t.born) as f32;
            let life = life_of(t) as f32;
            let a = (age / 0.18).min(1.0) * ((life - age) / 0.3).clamp(0.0, 1.0);
            let id = egui::Id::unique("toast").with(t.born.to_bits());
            let resp = egui::Area::new(id).order(egui::Order::Foreground).fixed_pos(egui::pos2(screen.right() - 18.0, y + (1.0 - a) * 12.0)).pivot(egui::Align2::RIGHT_BOTTOM).show(ctx, |ui| {
                ui.set_opacity(a);
                let (icon, col) = match t.kind {
                    ToastKind::Good => (I::CHECK_CIRCLE, GOOD()),
                    ToastKind::Bad => (I::WARNING_CIRCLE, BAD()),
                    ToastKind::Info => (I::INFO, ACCENT_HI()),
                };
                egui::Frame::new().fill(CARD_HI()).stroke(Stroke::new(1.0, BORDER())).corner_radius(CornerRadius::same(10)).inner_margin(egui::Margin::symmetric(14, 10)).shadow(egui::Shadow { offset: [0, 8], blur: 24, spread: 0, color: Color32::from_black_alpha(120) }).show(ui, |ui| {
                    ui.set_max_width(440.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(icon).color(col).size(16.0));
                        ui.add(egui::Label::new(RichText::new(&t.text).color(TEXT())).wrap());
                        if let Some(u) = &t.undo {
                            if button_sized(ui, Btn::Ghost, I::CLOCK_COUNTER_CLOCKWISE, "Undo", true, 26.0).on_hover_text("Start it again").clicked() {
                                undo = Some(u.clone());
                                close = Some(i);
                            }
                        }
                        if let Some(op) = &t.retry {
                            if button_sized(ui, Btn::Primary, I::SHIELD_CHECK, "Retry as admin", true, 26.0).clicked() {
                                retry = Some(op.clone());
                                close = Some(i);
                            }
                        }
                        if ui.add(egui::Label::new(RichText::new(I::X).color(FAINT())).sense(Sense::click())).clicked() {
                            close = Some(i);
                        }
                    });
                });
            });
            y -= resp.response.rect.height() + 8.0;
            if a < 1.0 {
                ctx.request_repaint();
            }
        }
        if let Some(i) = close {
            self.toasts.remove(i);
        }
        if let Some(op) = retry {
            self.run_admin_async(op, "Done (as administrator)".into(), None);
        }
        if let Some(list) = undo {
            let mut failed = None;
            for (exe, args) in &list {
                if let Err(e) = win::launch_as_user(exe, args, None) {
                    failed = Some(e);
                }
            }
            match failed {
                None => {
                    let n = list.len();
                    self.toast(ToastKind::Good, if n == 1 { "Started it again".to_string() } else { format!("Started {n} apps again") });
                    self.log(LogKind::Action, format!("Undo: started {} again", list.iter().map(|(e, _)| std::path::Path::new(e).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default()).collect::<Vec<_>>().join(", ")));
                }
                Some(e) => self.toast(ToastKind::Bad, format!("Couldn't start it again: {e}")),
            }
            self.ctl.refresh();
        }
        if !self.toasts.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }
}

fn net_stop() {
    if crate::net::etw_running() {
        crate::net::stop_etw();
    }
}

/// Drawn inside a bottom-up layout, so the bar is added first to end up below its label.
fn meter(ui: &mut Ui, label: &str, frac: f32, value: &str, color: Color32) {
    ui.add_space(4.0);
    let w = ui.available_width() - 8.0;
    ui.horizontal(|ui| {
        ui.add_space(4.0);
        bar(ui, frac, color, w, 4.0);
    });
    ui.horizontal(|ui| {
        ui.add_space(4.0);
        ui.label(RichText::new(label).size(11.5).color(FAINT()));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(4.0);
            ui.label(RichText::new(value).font(bold(11.5)).color(MUTED()));
        });
    });
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_msgs();
        // Keep the mini overlay ticking while the main window is hidden.
        if self.cfg.overlay && !self.visible {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden);
            ctx.request_repaint_after(Duration::from_millis(self.cfg.refresh_ms.max(500)));
        }
        if self.show_flag.swap(false, Ordering::SeqCst) {
            self.on_show(ctx);
        }
        if let Some(p) = self.pending_locks.take() {
            self.find_lockers(p);
        }
        // Close button hides instead of quitting.
        if ctx.input(|i| i.viewport().close_requested()) && !self.quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.hide(ctx);
        }
        // Safety net: the window is on screen (shown by Windows directly, e.g. by the show listener)
        // but we think it's hidden, which would leave it blank. Treat it as shown.
        // eframe shows the window after its first paint even when it was created hidden. Undo that
        // while it's still cloaked (so nothing flashes), then uncloak for later shows.
        if self.start_cloaked && !self.visible && ctx.cumulative_frame_nr() >= 1 {
            if win::is_window_visible(self.hwnd as _) {
                win::hide_window(self.hwnd as _);
            }
            win::set_cloaked(self.hwnd, false);
            self.start_cloaked = false;
            self.hidden_at = Instant::now();
        } else if self.start_cloaked && !self.visible {
            ctx.request_repaint();
        }
        if !self.start_cloaked && !self.visible && !self.quit && self.hidden_at.elapsed() > Duration::from_millis(800) && win::is_window_visible(self.hwnd as _) {
            self.on_show(ctx);
        }
        if self.visible {
            let focused = ctx.input(|i| i.viewport().focused).unwrap_or(false);
            if focused {
                self.had_focus = true;
            }
            // Only hide on focus loss after the window really had focus since it was shown; the focus
            // state right after showing is stale (it still says "not focused" from while hidden).
            if self.cfg.hide_on_focus_loss && self.modal.is_none() && self.had_focus && !focused && self.shown_at.elapsed() > Duration::from_millis(1200) {
                self.hide(ctx);
            }
        }
        if !self.paused {
            self.snap = self.shared.lock().unwrap().snap.clone();
        }
        if self.last_save.elapsed() > Duration::from_secs(300) {
            self.save_all();
            // Drop cached info for processes that exited (keeps memory flat when running for days).
            let live: HashSet<Key> = self.snap.procs.iter().map(|p| p.key()).collect();
            self.info.prune(&live);
        }
        // Silent update while nobody is looking.
        if !self.visible && self.cfg.auto_update {
            let st = self.update.lock().unwrap().clone();
            if let updater::State::Ready { path, .. } = st {
                self.apply_update(ctx, &path, false);
            }
        }
        if let Some(size) = ctx.input(|i| i.viewport().inner_rect.map(|r| r.size())) {
            if self.visible && size.x > 200.0 {
                self.cfg.window_size = Some([size.x, size.y]);
            }
        }
        self.ctl.want_conns.store(self.visible && (self.page == Page::Network || (self.page == Page::Processes && (self.selected.is_some() || self.cfg.columns.contains(&crate::config::Col::Conns) || crate::ui_procs::port_query(&self.search).is_some()))), Ordering::Relaxed);
        let overlay_ping = (self.cfg.overlay && self.cfg.overlay_items.contains(&crate::config::OverlayItem::Ping)) || (self.visible && self.cfg.sidebar_meters.contains(&crate::config::OverlayItem::Ping));
        self.latency.active.store((self.visible && self.page == Page::Network) || overlay_ping, Ordering::Relaxed);
        if self.visible {
            self.shortcuts(ctx);
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.overlay(&ctx);
        if !self.visible {
            return; // painting only for the overlay
        }
        self.sidebar(ui);
        egui::CentralPanel::default().frame(egui::Frame::new().fill(BG()).inner_margin(egui::Margin { left: 22, right: 22, top: 16, bottom: 10 })).show(ui, |ui| {
            self.update_banner(ui);
            match self.page {
                Page::Processes => self.processes_page(ui),
                Page::Performance => self.performance_page(ui),
                Page::Network => self.network_page(ui),
                Page::Apps => self.apps_page(ui),
                Page::Startup => self.startup_page(ui),
                Page::Services => self.services_page(ui),
                Page::History => self.history_page(ui),
                Page::Rules => self.rules_page(ui),
                Page::Settings => self.settings_page(ui),
            }
        });
        self.modals(&ctx);
        self.palette_ui(&ctx);
        self.perf_pick_ui(&ctx);
        self.toasts_ui(&ctx);
        if self.visible {
            let ms = if self.page == Page::Performance && self.cfg.smooth_graphs && self.perf_range == 0 { 33 } else { self.cfg.refresh_ms.max(250) };
            ctx.request_repaint_after(Duration::from_millis(ms));
        }
    }

    fn clear_color(&self, _v: &egui::Visuals) -> [f32; 4] {
        [BG().r() as f32 / 255.0, BG().g() as f32 / 255.0, BG().b() as f32 / 255.0, 1.0]
    }
}

/// Page header: title, subtitle and right-aligned actions.
pub fn header(ui: &mut Ui, title: &str, subtitle: &str, right: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(RichText::new(title).font(bold(22.0)));
            if !subtitle.is_empty() {
                ui.label(RichText::new(subtitle).color(MUTED()).size(12.5));
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), right);
    });
    ui.add_space(10.0);
}
