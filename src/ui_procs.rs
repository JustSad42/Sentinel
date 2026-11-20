//! Processes page: grouped / tree / flat list with heat-mapped columns, details panel, context menu.

use crate::admin::AdminOp;
use crate::app::{App, Modal, Page, ToastKind, header};
use crate::config::{Col, Priority, ViewMode};
use crate::fmt;
use crate::icons as I;
use crate::sampler::{Key, Proc, Snapshot};
use crate::theme::*;
use crate::win;
use egui::{Align, Color32, CornerRadius, Layout, RichText, Sense, Stroke, Ui, vec2};
use egui_extras::{Column, TableBuilder};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Section {
    Pinned,
    Apps,
    Background,
    Windows,
}

impl Section {
    fn label(self) -> &'static str {
        match self {
            Section::Pinned => "Pinned",
            Section::Apps => "Apps",
            Section::Background => "Background processes",
            Section::Windows => "Windows processes",
        }
    }
    fn id(self) -> &'static str {
        match self {
            Section::Pinned => "pinned",
            Section::Apps => "apps",
            Section::Background => "bg",
            Section::Windows => "win",
        }
    }
}

#[derive(Default, Clone, Copy)]
struct Agg {
    cpu: f32,
    mem: u64,
    io: f64,
    net: f64,
    gpu: f32,
    gpu_mem: u64,
    threads: u32,
    handles: u32,
    conns: u32,
    hung: bool,
    suspended: bool,
}

/// Keeps row order steady: values are only re-ranked every few seconds (or right away when the
/// sort, search or layout changes), with name/PID tie-breaks so equal rows never swap places.
#[derive(Default)]
pub struct SortCache {
    procs: HashMap<Key, u32>,
    groups: HashMap<String, u32>,
    next_procs: HashMap<Key, u32>,
    next_groups: HashMap<String, u32>,
    at: Option<std::time::Instant>,
    sig: String,
    fresh: bool,
}

const RESORT_EVERY: std::time::Duration = std::time::Duration::from_millis(3000);

/// Sort value with small wobble removed (0.3% vs 0.4% CPU shouldn't swap rows).
fn calm(v: f64, c: Col) -> f64 {
    match c {
        Col::Cpu | Col::Gpu => (v * 2.0).round() / 2.0,
        Col::Memory | Col::GpuMem => (v / (1 << 20) as f64).round(),
        Col::Disk | Col::Network => if v < 1024.0 { 0.0 } else { (v.log2() * 4.0).round() },
        _ => v,
    }
}

enum Row {
    Section { s: Section, count: usize, collapsed: bool },
    Group { key: String, members: Vec<usize>, agg: Agg, expanded: bool },
    Proc { idx: usize, depth: usize, expander: Option<bool> },
}

enum Act {
    Select(Key),
    SelectGroup(String),
    ToggleSection(&'static str),
    ToggleGroup(String),
    ToggleTree(Key),
    Kill(Vec<(u32, i64)>, Vec<String>, String),
    Suspend(u32, bool, String),
    Priority(Proc, Priority),
    Affinity(u32, String),
    SwitchTo(isize),
    CloseWindow(isize),
    Restart(Proc),
    Pin(String),
    Rule(String),
    Open(String),
    Props(String),
    Copy(String),
    Search(String),
    Block(String),
    Sort(Col),
    SortName,
    ShowConns(u32),
}

/// Fixed-width arrow column. Returns the x up to which a click toggles expansion.
fn caret_slot(ui: &mut Ui, indent: f32, caret: Option<bool>, color: Color32) -> f32 {
    if indent > 0.0 {
        ui.add_space(indent);
    }
    let (cr, _) = ui.allocate_exact_size(vec2(14.0, 20.0), Sense::hover());
    if let Some(open) = caret {
        ui.painter().text(cr.center(), egui::Align2::CENTER_CENTER, if open { I::CARET_DOWN } else { I::CARET_RIGHT }, egui::FontId::proportional(12.0), color);
    }
    cr.right() + 4.0
}

fn is_windows_proc(p: &Proc) -> bool {
    if p.pid <= 4 || crate::sampler::critical(&p.name) {
        return true;
    }
    match &p.path {
        Some(path) => {
            let l = path.to_ascii_lowercase();
            l.starts_with("c:\\windows\\") && !l.contains("\\explorer.exe") || (l.contains("\\windowsapps\\microsoftwindows.") )
        }
        None => p.session == 0,
    }
}

fn col_width(c: Col) -> f32 {
    match c {
        Col::Pid => 64.0,
        Col::Status => 104.0,
        Col::User => 100.0,
        Col::Cpu => 86.0,
        Col::Memory => 104.0,
        Col::Disk => 104.0,
        Col::Network => 108.0,
        Col::Gpu => 84.0,
        Col::GpuMem => 90.0,
        Col::Threads | Col::Handles => 70.0,
        Col::Priority => 100.0,
        Col::Started => 116.0,
        Col::Conns => 92.0,
        Col::Path | Col::CommandLine => 260.0,
    }
}

fn proc_value(p: &Proc, c: Col) -> f64 {
    match c {
        Col::Pid => p.pid as f64,
        Col::Cpu => p.cpu as f64,
        Col::Memory => p.mem as f64,
        Col::Disk => p.io_rate,
        Col::Network => p.net_rate(),
        Col::Gpu => p.gpu as f64,
        Col::GpuMem => p.gpu_mem as f64,
        Col::Threads => p.threads as f64,
        Col::Handles => p.handles as f64,
        Col::Conns => p.conns as f64,
        Col::Started => p.create_time as f64,
        Col::Priority => p.base_priority as f64,
        _ => 0.0,
    }
}

fn agg_value(a: &Agg, c: Col) -> f64 {
    match c {
        Col::Cpu => a.cpu as f64,
        Col::Memory => a.mem as f64,
        Col::Disk => a.io,
        Col::Network => a.net,
        Col::Gpu => a.gpu as f64,
        Col::GpuMem => a.gpu_mem as f64,
        Col::Threads => a.threads as f64,
        Col::Handles => a.handles as f64,
        Col::Conns => a.conns as f64,
        _ => 0.0,
    }
}

impl App {
    fn display_name(&self, p: &Proc) -> (String, Option<String>) {
        let meta = self.info.meta(p.key(), p.path.as_ref());
        let desc = meta.as_ref().and_then(|m| m.description.clone()).filter(|d| !d.is_empty());
        let name = if p.name.is_empty() { format!("PID {}", p.pid) } else { p.name.clone() };
        let stem = name.strip_suffix(".exe").or_else(|| name.strip_suffix(".EXE")).unwrap_or(&name).to_string();
        match desc {
            // Same as the file name: show the friendly form only.
            Some(d) if d.eq_ignore_ascii_case(&stem) || d.eq_ignore_ascii_case(&name) => (d, None),
            Some(d) => (d, Some(name)),
            None => (name, None),
        }
    }

    fn matches_search(&self, p: &Proc, q: &str) -> bool {
        if q.is_empty() {
            return true;
        }
        if let Some(port) = port_query(q) {
            return self.snap.conns.iter().any(|c| c.pid == p.pid && (c.local.port() == port || c.remote.is_some_and(|r| r.port() == port)));
        }
        if p.name.to_lowercase().contains(q) || p.pid.to_string() == q {
            return true;
        }
        if let Some(w) = &p.window {
            if w.title.to_lowercase().contains(q) {
                return true;
            }
        }
        if let Some(m) = self.info.meta(p.key(), p.path.as_ref()) {
            if m.description.as_ref().is_some_and(|d| d.to_lowercase().contains(q)) || m.company.as_ref().is_some_and(|d| d.to_lowercase().contains(q)) || m.cmdline.as_ref().is_some_and(|d| d.to_lowercase().contains(q)) || m.user.as_ref().is_some_and(|d| d.to_lowercase() == q) {
                return true;
            }
        }
        if let Some(svcs) = self.snap.services_by_pid.get(&p.pid) {
            if svcs.iter().any(|(n, d)| n.to_lowercase().contains(q) || d.to_lowercase().contains(q)) {
                return true;
            }
        }
        false
    }

    fn sort_indices(&self, idx: &mut [usize], procs: &[Proc]) {
        let mut sc = self.sort_cache.borrow_mut();
        if !sc.fresh {
            // Between re-ranks keep the last order; newcomers go after known rows, by name.
            idx.sort_by_cached_key(|&i| {
                let p = &procs[i];
                (sc.procs.get(&p.key()).copied().unwrap_or(u32::MAX), self.display_name(p).0.to_lowercase(), p.pid)
            });
            return;
        }
        let desc = self.cfg.sort_desc;
        let names: HashMap<usize, String> = idx.iter().map(|&i| (i, self.display_name(&procs[i]).0.to_lowercase())).collect();
        let tie = |a: usize, b: usize| names[&a].cmp(&names[&b]).then(procs[a].pid.cmp(&procs[b].pid));
        match self.cfg.sort_col {
            None => idx.sort_by(|&a, &b| if desc { tie(b, a) } else { tie(a, b) }),
            Some(c) if !c.numeric() && c != Col::Started && c != Col::Priority => {
                idx.sort_by_cached_key(|&i| {
                    let p = &procs[i];
                    let k = match c {
                        Col::Path => p.path.as_deref().unwrap_or("").to_lowercase(),
                        Col::Status => format!("{}{}", p.window.as_ref().is_some_and(|w| w.hung) as u8, p.suspended as u8),
                        _ => names[&i].clone(),
                    };
                    (k, names[&i].clone(), p.pid)
                });
                if desc {
                    idx.reverse();
                }
            }
            Some(c) => {
                idx.sort_by(|&a, &b| {
                    let (va, vb) = (calm(proc_value(&procs[a], c), c), calm(proc_value(&procs[b], c), c));
                    let o = va.total_cmp(&vb);
                    (if desc { o.reverse() } else { o }).then_with(|| tie(a, b))
                });
            }
        }
        for (n, &i) in idx.iter().enumerate() {
            sc.next_procs.insert(procs[i].key(), n as u32);
        }
    }

    fn begin_sort(&self, q: &str) {
        let sig = format!("{:?}|{}|{:?}|{}|{}|{}|{}|{}", self.cfg.sort_col, self.cfg.sort_desc, self.cfg.view, q, self.collapsed.len(), self.expanded_groups.len(), self.tree_collapsed.len(), self.cfg.pinned.len());
        let mut sc = self.sort_cache.borrow_mut();
        sc.fresh = sc.sig != sig || sc.at.is_none_or(|t| t.elapsed() >= RESORT_EVERY) || sc.procs.is_empty();
        if sc.fresh {
            sc.sig = sig;
            sc.next_procs.clear();
            sc.next_groups.clear();
        }
    }

    fn end_sort(&self) {
        let mut sc = self.sort_cache.borrow_mut();
        if sc.fresh {
            sc.procs = std::mem::take(&mut sc.next_procs);
            sc.groups = std::mem::take(&mut sc.next_groups);
            sc.at = Some(std::time::Instant::now());
            sc.fresh = false;
        }
    }

    fn build_rows(&self, snap: &Snapshot) -> Vec<Row> {
        self.begin_sort(&self.search.trim().to_lowercase());
        let rows = self.build_rows_inner(snap);
        self.end_sort();
        rows
    }

    fn build_rows_inner(&self, snap: &Snapshot) -> Vec<Row> {
        let procs = &snap.procs;
        let q = self.search.trim().to_lowercase();
        let filtering = !q.is_empty() || self.only_hung || self.only_suspended;
        let mut idx: Vec<usize> = (0..procs.len())
            .filter(|&i| {
                let p = &procs[i];
                self.matches_search(p, &q) && (!self.only_hung || p.window.as_ref().is_some_and(|w| w.hung)) && (!self.only_suspended || p.suspended)
            })
            .collect();
        let mut rows = Vec::new();
        if filtering || self.cfg.view == ViewMode::Flat {
            self.sort_indices(&mut idx, procs);
            let pinned: Vec<usize> = idx.iter().copied().filter(|&i| self.cfg.pinned.iter().any(|n| n.eq_ignore_ascii_case(&procs[i].name))).collect();
            for &i in pinned.iter().chain(idx.iter().filter(|i| !pinned.contains(i))) {
                rows.push(Row::Proc { idx: i, depth: 0, expander: None });
            }
            return rows;
        }
        if self.cfg.view == ViewMode::Tree {
            let ct: HashMap<u32, i64> = procs.iter().map(|p| (p.pid, p.create_time)).collect();
            let mut children: HashMap<u32, Vec<usize>> = HashMap::new();
            let mut roots = Vec::new();
            for &i in &idx {
                let p = &procs[i];
                if p.ppid != p.pid && ct.get(&p.ppid).is_some_and(|&c| c <= p.create_time) && p.ppid != 0 {
                    children.entry(p.ppid).or_default().push(i);
                } else {
                    roots.push(i);
                }
            }
            self.sort_indices(&mut roots, procs);
            for v in children.values_mut() {
                self.sort_indices(v, procs);
            }
            fn walk(app: &App, i: usize, depth: usize, procs: &[Proc], ch: &HashMap<u32, Vec<usize>>, rows: &mut Vec<Row>) {
                let p = &procs[i];
                let kids = ch.get(&p.pid).filter(|k| !k.is_empty());
                let expanded = !app.tree_collapsed.contains(&p.key());
                rows.push(Row::Proc { idx: i, depth, expander: kids.map(|_| expanded) });
                if let Some(k) = kids {
                    if expanded && depth < 40 {
                        for &c in k {
                            walk(app, c, depth + 1, procs, ch, rows);
                        }
                    }
                }
            }
            for r in roots {
                walk(self, r, 0, procs, &children, &mut rows);
            }
            return rows;
        }
        // Grouped: sections, then one group per executable.
        let app_names: HashSet<String> = idx.iter().filter(|&&i| procs[i].window.is_some()).map(|&i| procs[i].name.to_lowercase()).collect();
        let mut by_section: HashMap<Section, std::collections::BTreeMap<String, Vec<usize>>> = HashMap::new();
        for &i in &idx {
            let p = &procs[i];
            let lname = p.name.to_lowercase();
            let s = if self.cfg.pinned.iter().any(|n| n.eq_ignore_ascii_case(&p.name)) {
                Section::Pinned
            } else if app_names.contains(&lname) && !crate::sampler::critical(&p.name) {
                Section::Apps
            } else if is_windows_proc(p) {
                if self.cfg.show_windows_group { Section::Windows } else { Section::Background }
            } else {
                Section::Background
            };
            // Group by path when known (two different "node.exe" builds stay apart).
            let gk = p.path.as_deref().map(|s| s.to_lowercase()).unwrap_or_else(|| lname.clone());
            by_section.entry(s).or_default().entry(gk).or_default().push(i);
        }
        for s in [Section::Pinned, Section::Apps, Section::Background, Section::Windows] {
            let Some(groups) = by_section.get(&s) else { continue };
            let count: usize = groups.values().map(|v| v.len()).sum();
            let collapsed = self.collapsed.contains(s.id());
            rows.push(Row::Section { s, count, collapsed });
            if collapsed {
                continue;
            }
            let mut glist: Vec<(String, Vec<usize>, Agg)> = groups
                .iter()
                .map(|(k, members)| {
                    let mut a = Agg::default();
                    for &i in members {
                        let p = &procs[i];
                        a.cpu += p.cpu;
                        a.mem += p.mem;
                        a.io += p.io_rate;
                        a.net += p.net_rate();
                        a.gpu = (a.gpu + p.gpu).min(100.0);
                        a.gpu_mem += p.gpu_mem;
                        a.threads += p.threads;
                        a.handles += p.handles;
                        a.conns += p.conns;
                        a.hung |= p.window.as_ref().is_some_and(|w| w.hung);
                        a.suspended |= p.suspended;
                    }
                    let mut m = members.clone();
                    self.sort_indices(&mut m, procs);
                    (format!("{}|{}", s.id(), k), m, a)
                })
                .collect();
            let desc = self.cfg.sort_desc;
            let gname: HashMap<String, String> = glist.iter().map(|g| (g.0.clone(), self.display_name(&procs[g.1[0]]).0.to_lowercase())).collect();
            let fresh = self.sort_cache.borrow().fresh;
            if !fresh {
                let sc = self.sort_cache.borrow();
                glist.sort_by(|a, b| sc.groups.get(&a.0).copied().unwrap_or(u32::MAX).cmp(&sc.groups.get(&b.0).copied().unwrap_or(u32::MAX)).then_with(|| gname[&a.0].cmp(&gname[&b.0])));
            } else {
                match self.cfg.sort_col {
                    Some(c) if c.numeric() && c != Col::Pid => glist.sort_by(|a, b| {
                        let o = calm(agg_value(&a.2, c), c).total_cmp(&calm(agg_value(&b.2, c), c));
                        (if desc { o.reverse() } else { o }).then_with(|| gname[&a.0].cmp(&gname[&b.0]))
                    }),
                    _ => {
                        glist.sort_by(|a, b| gname[&a.0].cmp(&gname[&b.0]).then_with(|| a.0.cmp(&b.0)));
                        if desc && self.cfg.sort_col.is_none() {
                            glist.reverse();
                        }
                    }
                }
                let mut sc = self.sort_cache.borrow_mut();
                for (n, g) in glist.iter().enumerate() {
                    sc.next_groups.insert(g.0.clone(), n as u32);
                }
            }
            for (key, members, agg) in glist {
                if members.len() == 1 {
                    rows.push(Row::Proc { idx: members[0], depth: 1, expander: None });
                } else {
                    let expanded = self.expanded_groups.contains(&key);
                    rows.push(Row::Group { key: key.clone(), members: members.clone(), agg, expanded });
                    if expanded {
                        for i in members {
                            rows.push(Row::Proc { idx: i, depth: 2, expander: None });
                        }
                    }
                }
            }
        }
        rows
    }

    pub fn processes_page(&mut self, ui: &mut Ui) {
        self.refresh_leaks();
        let ctx = ui.ctx().clone();
        let snap = self.snap.clone();
        let sub = format!(
            "{} processes  ·  {} threads  ·  {} handles  ·  up {}",
            snap.proc_count,
            snap.thread_count,
            snap.handle_count,
            fmt::duration(win::uptime_secs())
        );
        header(ui, "Processes", &sub, |ui| {
            if icon_button(ui, if self.paused { I::PLAY } else { I::PAUSE }, if self.paused { "Resume updates (Ctrl+P)" } else { "Pause updates (Ctrl+P)" }, self.paused).clicked() {
                self.paused = !self.paused;
            }
            if icon_button(ui, I::LOCK_KEY, "What's locking a file? (or drop a file on the window)", false).clicked() {
                if let Some(p) = rfd::FileDialog::new().set_title("Which file is locked?").pick_file() {
                    self.find_lockers(p.to_string_lossy().to_string());
                }
            }
            if icon_button(ui, I::TERMINAL_WINDOW, "Run new task (Ctrl+N)", false).clicked() {
                self.modal = Some(Modal::RunTask { cmd: String::new(), admin: false });
            }
            ui.menu_button(RichText::new(format!("{}  Columns", I::SQUARES_FOUR)).color(MUTED()), |ui| {
                ui.set_min_width(200.0);
                for (i, c) in Col::ALL.into_iter().enumerate() {
                    if i == 0 {
                        label_caps(ui, "Usage");
                    }
                    if c == Col::Pid {
                        ui.separator();
                        label_caps(ui, "Extra");
                    }
                    let mut on = self.cfg.columns.contains(&c);
                    if ui.checkbox(&mut on, c.label()).changed() {
                        if on {
                            self.cfg.columns.push(c);
                            let order: Vec<Col> = Col::ALL.iter().copied().filter(|x| self.cfg.columns.contains(x)).collect();
                            self.cfg.columns = order;
                        } else {
                            self.cfg.columns.retain(|x| *x != c);
                        }
                        self.cfg.save();
                    }
                }
            });
        });
        self.summary_strip(ui, &snap);
        ui.add_space(10.0);

        // Toolbar
        let hung = snap.procs.iter().filter(|p| p.window.as_ref().is_some_and(|w| w.hung)).count();
        let susp = snap.procs.iter().filter(|p| p.suspended).count();
        let search_id = egui::Id::unique("proc-search");
        ui.horizontal(|ui| {
            let r = field(ui, search_id, I::MAGNIFYING_GLASS, &mut self.search, "Search name, PID, title, command line, publisher, service…", 420.0);
            if self.focus_search {
                r.request_focus();
                self.focus_search = false;
            }
            ui.add_space(6.0);
            let mut v = match self.cfg.view {
                ViewMode::Grouped => 0,
                ViewMode::Tree => 1,
                ViewMode::Flat => 2,
            };
            if segmented(ui, &mut v, &["Grouped", "Tree", "List"]) {
                self.cfg.view = [ViewMode::Grouped, ViewMode::Tree, ViewMode::Flat][v];
                self.cfg.save();
            }
            if hung > 0 || self.only_hung {
                chip(ui, &mut self.only_hung, &format!("{} Not responding", I::WARNING), Some(hung));
            }
            if susp > 0 || self.only_suspended {
                chip(ui, &mut self.only_suspended, &format!("{} Suspended", I::SNOWFLAKE), Some(susp));
            }
            // Truncated, so a narrow window never gets pushed wider by this hint.
            let hint = if port_query(&self.search).is_some() {
                "Apps using this port"
            } else if !self.search.is_empty() {
                "Showing matches as a list"
            } else {
                "Tip: type :3000 to find what's using a port"
            };
            ui.add(egui::Label::new(RichText::new(hint).size(12.0).color(FAINT())).truncate());
        });
        let port_act = match port_query(&self.search) {
            Some(port) => self.port_banner(ui, port, &snap),
            None => None,
        };
        // Type anywhere to search.
        if !ctx.memory(|m| m.focused().is_some()) && self.modal.is_none() && !self.palette.open {
            let typed: String = ctx.input(|i| i.events.iter().filter_map(|e| if let egui::Event::Text(t) = e { Some(t.clone()) } else { None }).collect());
            if !typed.is_empty() && !ctx.input(|i| i.modifiers.command) && typed.chars().all(|c| !c.is_control()) {
                self.search.push_str(&typed);
                ctx.memory_mut(|m| m.request_focus(search_id));
            }
        }
        ui.add_space(8.0);

        // Restore the selection from the last session once that app is running.
        if self.selected.is_none() && !snap.procs.is_empty() {
            if let Some(want) = self.pending_select.take() {
                if let Some(p) = snap.procs.iter().filter(|p| p.name.eq_ignore_ascii_case(&want)).max_by_key(|p| p.mem) {
                    self.selected = Some(p.key());
                    self.scroll_to_sel = true;
                }
            }
        }
        // Test hook for screenshots: SENTINEL_SELECT=<exe name>
        if self.selected.is_none() && crate::config::test_mode() {
            if let Ok(want) = std::env::var("SENTINEL_SELECT") {
                if let Some(p) = snap.procs.iter().filter(|p| p.name.eq_ignore_ascii_case(&want)).max_by(|a, b| a.mem.cmp(&b.mem)) {
                    self.selected = Some(p.key());
                }
            }
        }
        let rows = self.build_rows(&snap);
        self.visible_rows = rows.iter().filter_map(|r| if let Row::Proc { idx, .. } = r { Some(snap.procs[*idx].key()) } else { None }).collect();
        // Keep selection valid.
        if let Some(k) = self.selected {
            if !snap.procs.iter().any(|p| p.key() == k) && !snap.procs.is_empty() {
                self.selected = None;
            }
        }
        self.keyboard_nav(&ctx, &snap, search_id);

        let sel_proc = self.selected.and_then(|k| snap.procs.iter().find(|p| p.key() == k).cloned());
        let show_details = self.details_open && (sel_proc.is_some() || self.sel_group.is_some());
        let mut acts: Vec<Act> = Vec::new();
        acts.extend(port_act);
        // Slide-in details panel.
        let target_w = if show_details { 360.0 } else { 0.0 };
        let w = ctx.animate_value_with_time(egui::Id::unique("details-w"), target_w, 0.16);
        let full = ui.available_rect_before_wrap();
        if w > 4.0 {
            egui::Panel::right("proc-details").exact_size(w).resizable(false).show_separator_line(false).frame(egui::Frame::new().inner_margin(egui::Margin { left: 14, right: 0, top: 0, bottom: 0 })).show(ui, |ui| {
                ui.set_min_width(346.0);
                if let Some(p) = &sel_proc {
                    self.details(ui, p, &snap, &mut acts);
                } else if let Some(g) = self.sel_group.clone() {
                    self.group_details(ui, &g, &snap, &rows, &mut acts);
                }
            });
        }

        // Table
        let mut cols = self.cfg.columns.clone();
        let total_mem = snap.mem_total.max(1) as f64;
        let scroll_idx = if self.scroll_to_sel {
            self.scroll_to_sel = false;
            rows.iter().position(|r| matches!(r, Row::Proc { idx, .. } if Some(snap.procs[*idx].key()) == self.selected))
        } else {
            None
        };
        // A right-side panel only moves the cursor, so give the table an explicit rect beside it.
        let mut table_rect = full;
        if w > 4.0 {
            table_rect.max.x -= w;
        }
        // With the details panel open, drop the least useful columns until the table fits beside it
        // (otherwise the table's minimum widths would push it underneath the panel).
        let compact = w > 4.0;
        if compact {
            let need = |cols: &[Col]| 230.0 + cols.iter().map(|c| col_width(*c) + 10.0).sum::<f32>() + 60.0;
            for drop in [Col::CommandLine, Col::Path, Col::Started, Col::Priority, Col::Handles, Col::Threads, Col::Conns, Col::GpuMem, Col::Pid, Col::Status, Col::Gpu, Col::Network] {
                if need(&cols) <= table_rect.width() {
                    break;
                }
                cols.retain(|c| *c != drop);
            }
        }
        let mut table_ui = ui.new_child(egui::UiBuilder::new().max_rect(table_rect).layout(Layout::top_down(Align::Min)));
        table_ui.set_clip_rect(table_rect.intersect(ui.clip_rect()));
        let ui = &mut table_ui;
        egui::Frame::new().fill(CARD()).stroke(Stroke::new(1.0, BORDER_SOFT())).corner_radius(CornerRadius::same(12)).inner_margin(egui::Margin { left: 6, right: 6, top: 4, bottom: 4 }).show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            ui.style_mut().spacing.item_spacing.x = 10.0;
            let mut tb = TableBuilder::new(ui)
                .id_salt(if compact { "procs-compact" } else { "procs" })
                .striped(false)
                .resizable(true)
                .sense(Sense::click())
                .cell_layout(Layout::left_to_right(Align::Center))
                .auto_shrink([false, false])
                .column(Column::remainder().at_least(230.0).clip(true));
            for c in &cols {
                tb = tb.column(Column::initial(col_width(*c)).at_least(44.0).clip(true));
            }
            if let Some(i) = scroll_idx {
                tb = tb.scroll_to_row(i, Some(Align::Center));
            }
            let sort_col = self.cfg.sort_col;
            let sort_desc = self.cfg.sort_desc;
            let sums = (snap.cpu_total, snap.mem_total - snap.mem_avail, snap.procs.iter().map(|p| p.io_rate).sum::<f64>(), snap.net_send + snap.net_recv, snap.gpu.util);
            tb.header(46.0, |mut h| {
                h.col(|ui| {
                    if header_cell(ui, "Name", None, TEXT(), sort_col.is_none(), sort_desc, false) {
                        acts.push(Act::SortName);
                    }
                });
                for c in &cols {
                    h.col(|ui| {
                        // System totals above each usage column, in the column's colour (amber/red when high).
                        let sev = |v: f32, warn: f32, bad: f32, base: Color32| if v >= bad { BAD() } else if v >= warn { WARN() } else { base };
                        let mem_pct = (sums.1 as f64 / total_mem * 100.0) as f32;
                        let (top, color) = match c {
                            Col::Cpu => (Some(fmt::pct(sums.0)), sev(sums.0, 70.0, 90.0, C_CPU())),
                            Col::Memory => (Some(format!("{mem_pct:.0}%")), sev(mem_pct, 80.0, 90.0, C_MEM())),
                            Col::Disk => (Some(fmt::rate(sums.2)), C_DISK()),
                            Col::Network if snap.per_app_net => (Some(fmt::rate(sums.3)), C_NET()),
                            Col::Gpu => (Some(fmt::pct(sums.4)), sev(sums.4, 80.0, 95.0, C_GPU())),
                            _ => (None, TEXT()),
                        };
                        if header_cell(ui, c.label(), top.as_deref(), color, sort_col == Some(*c), sort_desc, c.numeric()) {
                            acts.push(Act::Sort(*c));
                        }
                    });
                }
            })
            .body(|body| {
                let tree = self.cfg.view == ViewMode::Tree;
                let flat = rows.iter().all(|r| matches!(r, Row::Proc { depth: 0, expander: None, .. }));
                let row_h = if self.cfg.compact_rows { 24.0 } else { 30.0 };
                body.rows(row_h, rows.len(), |mut row| {
                    let ri = row.index();
                    match &rows[ri] {
                        Row::Section { s, count, collapsed } => {
                            row.col(|ui| {
                                caret_slot(ui, 0.0, Some(!*collapsed), FAINT());
                                ui.label(RichText::new(s.label().to_uppercase()).font(bold(11.0)).color(MUTED()).extra_letter_spacing(0.8));
                                ui.label(RichText::new(count.to_string()).size(11.0).color(FAINT()));
                            });
                            for _ in &cols {
                                row.col(|_| {});
                            }
                            if row.response().clicked() {
                                acts.push(Act::ToggleSection(s.id()));
                            }
                        }
                        Row::Group { key, members, agg, expanded } => {
                            let first = &snap.procs[members[0]];
                            row.set_selected(self.sel_group.as_deref() == Some(key.as_str()));
                            let mut caret_hit = 0.0;
                            row.col(|ui| {
                                caret_hit = caret_slot(ui, 0.0, Some(*expanded), MUTED());
                                self.name_cell(ui, first, true);
                                ui.label(RichText::new(format!("({})", members.len())).color(FAINT()));
                                if agg.hung {
                                    pill(ui, "Not responding", BAD());
                                }
                            });
                            for c in &cols {
                                row.col(|ui| self.agg_cell(ui, *c, agg, total_mem, snap.per_app_net));
                            }
                            let resp = row.response();
                            if resp.clicked() {
                                if resp.interact_pointer_pos().is_some_and(|p| p.x <= caret_hit) {
                                    acts.push(Act::ToggleGroup(key.clone()));
                                } else {
                                    acts.push(Act::SelectGroup(key.clone()));
                                }
                            }
                            if resp.double_clicked() {
                                acts.push(Act::ToggleGroup(key.clone()));
                            }
                            let targets: Vec<(u32, i64)> = members.iter().map(|&i| snap.procs[i].key()).collect();
                            let fname = first.name.clone();
                            let fpath = first.path.as_deref().map(|s| s.to_string());
                            resp.context_menu(|ui| {
                                ui.set_min_width(200.0);
                                if ui.button(format!("{}  End all {} processes", I::X_CIRCLE, targets.len())).clicked() {
                                    acts.push(Act::Kill(targets.clone(), vec![fname.clone()], format!("End all {} {} processes?", targets.len(), fname)));
                                    ui.close();
                                }
                                if ui.button(format!("{}  {}", I::PUSH_PIN, if self.cfg.pinned.iter().any(|n| n.eq_ignore_ascii_case(&fname)) { "Unpin" } else { "Pin to top" })).clicked() {
                                    acts.push(Act::Pin(fname.clone()));
                                    ui.close();
                                }
                                if ui.button(format!("{}  Add rule…", I::LIGHTNING)).clicked() {
                                    acts.push(Act::Rule(fname.clone()));
                                    ui.close();
                                }
                                if let Some(p) = &fpath {
                                    if ui.button(format!("{}  Open file location", I::FOLDER_OPEN)).clicked() {
                                        acts.push(Act::Open(p.clone()));
                                        ui.close();
                                    }
                                }
                            });
                        }
                        Row::Proc { idx, depth, expander } => {
                            let p = &snap.procs[*idx];
                            row.set_selected(self.selected == Some(p.key()) && self.sel_group.is_none());
                            let mut caret_hit = 0.0;
                            row.col(|ui| {
                                // Every row keeps the same arrow slot so names line up; children shift only slightly.
                                if !flat {
                                    let indent = if tree { *depth as f32 * 10.0 } else { depth.saturating_sub(1) as f32 * 12.0 };
                                    caret_hit = caret_slot(ui, indent, *expander, MUTED());
                                }
                                self.name_cell(ui, p, false);
                            });
                            for c in &cols {
                                row.col(|ui| self.proc_cell(ui, *c, p, total_mem, snap.per_app_net));
                            }
                            let resp = row.response();
                            if resp.clicked() {
                                if expander.is_some() && resp.interact_pointer_pos().is_some_and(|pp| pp.x <= caret_hit) {
                                    acts.push(Act::ToggleTree(p.key()));
                                } else {
                                    acts.push(Act::Select(p.key()));
                                }
                            }
                            if resp.double_clicked() {
                                if let Some(w) = &p.window {
                                    acts.push(Act::SwitchTo(w.hwnd));
                                }
                            }
                            resp.context_menu(|ui| self.proc_menu(ui, p, &snap, &mut acts));
                        }
                    }
                });
            });
        });

        for a in acts {
            self.apply(a, &ctx, &snap);
        }
    }

    fn keyboard_nav(&mut self, ctx: &egui::Context, snap: &Snapshot, search_id: egui::Id) {
        if self.modal.is_some() || self.palette.open {
            return;
        }
        let search_focused = ctx.memory(|m| m.has_focus(search_id));
        let other_focused = ctx.memory(|m| m.focused().is_some_and(|f| f != search_id));
        if other_focused {
            return;
        }
        let (up, down, pgup, pgdn, home, end, del, enter) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::PageUp),
                i.key_pressed(egui::Key::PageDown),
                !search_focused && i.key_pressed(egui::Key::Home),
                !search_focused && i.key_pressed(egui::Key::End),
                (!search_focused || self.search.is_empty()) && i.key_pressed(egui::Key::Delete),
                i.key_pressed(egui::Key::Enter),
            )
        });
        let shift = ctx.input(|i| i.modifiers.shift);
        let n = self.visible_rows.len();
        if n > 0 && (up || down || pgup || pgdn || home || end) {
            let cur = self.selected.and_then(|k| self.visible_rows.iter().position(|x| *x == k));
            let next = match cur {
                None => 0,
                Some(c) => {
                    if up {
                        c.saturating_sub(1)
                    } else if down {
                        (c + 1).min(n - 1)
                    } else if pgup {
                        c.saturating_sub(15)
                    } else if pgdn {
                        (c + 15).min(n - 1)
                    } else if home {
                        0
                    } else {
                        n - 1
                    }
                }
            };
            self.selected = Some(self.visible_rows[next]);
            self.sel_group = None;
            self.scroll_to_sel = true;
        }
        if enter && self.selected.is_some() {
            if search_focused {
                ctx.memory_mut(|m| m.surrender_focus(search_id));
            }
            self.details_open = !self.details_open || search_focused;
        }
        if del {
            if let Some(k) = self.selected {
                if let Some(p) = snap.procs.iter().find(|p| p.key() == k) {
                    let name = p.name.clone();
                    if shift {
                        let t = crate::actions::tree_of(k, &snap.procs);
                        let n = t.len();
                        self.kill_targets(t, vec![name.clone()], format!("End {name} and its {} child processes?", n - 1));
                    } else {
                        self.kill_targets(vec![k], vec![name.clone()], format!("End {name}?"));
                    }
                }
            } else if let Some(g) = self.sel_group.clone() {
                let members: Vec<&Proc> = snap.procs.iter().filter(|p| group_key_matches(&g, p)).collect();
                if !members.is_empty() {
                    let name = members[0].name.clone();
                    self.kill_targets(members.iter().map(|p| p.key()).collect(), vec![name.clone()], format!("End all {} {name} processes?", members.len()));
                }
            }
        }
    }

    fn name_cell(&mut self, ui: &mut Ui, p: &Proc, group: bool) {
        let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
        self.paint_proc_icon(ui, r, p);
        let (name, exe) = self.display_name(p);
        let pinned = self.cfg.pinned.iter().any(|n| n.eq_ignore_ascii_case(&p.name));
        if pinned {
            ui.label(RichText::new(I::PUSH_PIN).size(11.0).color(ACCENT_HI()));
        }
        let hung = p.window.as_ref().is_some_and(|w| w.hung);
        ui.add(egui::Label::new(RichText::new(&name).color(TEXT())).truncate().selectable(false)).on_hover_text(format!("{}  ·  PID {}", p.name, p.pid));
        if !group && !self.cfg.columns.contains(&Col::Status) {
            if hung {
                pill(ui, "Not responding", BAD());
            } else if p.suspended {
                pill(ui, "Suspended", BLUE());
            }
        }
        if let Some(l) = self.leak_for(&p.name) {
            let t = l.text();
            pill(ui, "Memory growing", ORANGE()).on_hover_text(format!("{t}\nIf it keeps climbing, restarting the app frees the memory."));
        }
        if !group {
            let role = self.info.meta(p.key(), p.path.as_ref()).and_then(|m| m.cmdline.clone()).and_then(|c| crate::roles::role(&c));
            if let Some(r) = role {
                ui.add(egui::Label::new(RichText::new(r).size(11.5).color(FAINT())).truncate().selectable(false));
            } else if let Some(e) = exe {
                ui.add(egui::Label::new(RichText::new(e).size(11.5).color(FAINT())).truncate().selectable(false));
            }
            if let Some(svcs) = self.snap.services_by_pid.get(&p.pid) {
                if p.name.eq_ignore_ascii_case("svchost.exe") {
                    let t = if svcs.len() == 1 { svcs[0].1.clone() } else { format!("{} services", svcs.len()) };
                    ui.add(egui::Label::new(RichText::new(t).size(11.5).color(FAINT())).truncate().selectable(false));
                }
            }
        }
    }

    fn agg_cell(&mut self, ui: &mut Ui, c: Col, a: &Agg, total_mem: f64, per_app_net: bool) {
        match c {
            Col::Cpu | Col::Memory | Col::Disk | Col::Network | Col::Gpu => {
                let v = Usage { cpu: a.cpu, mem: a.mem, io: a.io, net: a.net, gpu: a.gpu };
                v.cell(ui, c, total_mem, per_app_net);
            }
            Col::GpuMem => num_cell(ui, &if a.gpu_mem > 0 { fmt::bytes(a.gpu_mem) } else { "—".into() }, Color32::TRANSPARENT),
            Col::Threads => num_cell(ui, &a.threads.to_string(), Color32::TRANSPARENT),
            Col::Handles => num_cell(ui, &a.handles.to_string(), Color32::TRANSPARENT),
            Col::Conns => num_cell(ui, &a.conns.to_string(), Color32::TRANSPARENT),
            Col::Status => {
                if a.hung {
                    pill(ui, "Not responding", BAD());
                } else if a.suspended {
                    pill(ui, "Suspended", BLUE());
                }
            }
            _ => {}
        }
    }

    fn proc_cell(&mut self, ui: &mut Ui, c: Col, p: &Proc, total_mem: f64, per_app_net: bool) {
        match c {
            Col::Pid => num_cell(ui, &p.pid.to_string(), Color32::TRANSPARENT),
            Col::Cpu | Col::Memory | Col::Disk | Col::Network | Col::Gpu => {
                let v = Usage { cpu: p.cpu, mem: p.mem, io: p.io_rate, net: p.net_rate(), gpu: p.gpu };
                v.cell(ui, c, total_mem, per_app_net);
            }
            Col::GpuMem => num_cell(ui, &if p.gpu_mem > 0 { fmt::bytes(p.gpu_mem) } else { "—".into() }, Color32::TRANSPARENT),
            Col::Threads => num_cell(ui, &p.threads.to_string(), Color32::TRANSPARENT),
            Col::Handles => num_cell(ui, &p.handles.to_string(), Color32::TRANSPARENT),
            Col::Conns => num_cell(ui, &if p.conns > 0 { p.conns.to_string() } else { String::new() }, Color32::TRANSPARENT),
            Col::Status => {
                if p.window.as_ref().is_some_and(|w| w.hung) {
                    pill(ui, "Not responding", BAD());
                } else if p.suspended {
                    pill(ui, "Suspended", BLUE());
                } else if self.info.meta(p.key(), p.path.as_ref()).and_then(|m| m.elevated).unwrap_or(false) {
                    ui.label(RichText::new(format!("{} Admin", I::SHIELD_CHECK)).size(11.5).color(FAINT()));
                }
            }
            Col::User => {
                let u = self.info.meta(p.key(), p.path.as_ref()).and_then(|m| m.user.clone()).unwrap_or_default();
                ui.add(egui::Label::new(RichText::new(u).color(MUTED())).truncate());
            }
            Col::Priority => {
                ui.label(RichText::new(Priority::from_base(p.base_priority).label()).color(MUTED()));
            }
            Col::Started => {
                ui.label(RichText::new(fmt::when(win::filetime_to_unix(p.create_time))).color(MUTED()));
            }
            Col::Path => {
                ui.add(egui::Label::new(RichText::new(p.path.as_deref().unwrap_or("")).color(MUTED())).truncate());
            }
            Col::CommandLine => {
                let c = self.info.meta(p.key(), p.path.as_ref()).and_then(|m| m.cmdline.clone()).unwrap_or_default();
                ui.add(egui::Label::new(RichText::new(c).color(MUTED()).monospace().size(11.5)).truncate());
            }
        }
    }

    fn proc_menu(&mut self, ui: &mut Ui, p: &Proc, snap: &Snapshot, acts: &mut Vec<Act>) {
        ui.set_min_width(220.0);
        let name = p.name.clone();
        if let Some(w) = &p.window {
            if ui.button(format!("{}  Switch to", I::ARROW_SQUARE_OUT)).clicked() {
                acts.push(Act::SwitchTo(w.hwnd));
                ui.close();
            }
        }
        if ui.button(RichText::new(format!("{}  End task", I::X_CIRCLE)).color(BAD())).clicked() {
            acts.push(Act::Kill(vec![p.key()], vec![name.clone()], format!("End {name}?")));
            ui.close();
        }
        let tree = crate::actions::tree_of(p.key(), &snap.procs);
        if tree.len() > 1 && ui.button(format!("{}  End process tree ({})", I::TREE_STRUCTURE, tree.len())).clicked() {
            acts.push(Act::Kill(tree.clone(), vec![name.clone()], format!("End {name} and its {} child processes?", tree.len() - 1)));
            ui.close();
        }
        let same: Vec<(u32, i64)> = snap.procs.iter().filter(|x| x.name.eq_ignore_ascii_case(&name)).map(|x| x.key()).collect();
        if same.len() > 1 && ui.button(format!("{}  End all \"{}\" ({})", I::SKULL, name, same.len())).clicked() {
            acts.push(Act::Kill(same.clone(), vec![name.clone()], format!("End all {} {name} processes?", same.len())));
            ui.close();
        }
        if let Some(w) = &p.window {
            if ui.button(format!("{}  Close window (ask nicely)", I::APP_WINDOW)).clicked() {
                acts.push(Act::CloseWindow(w.hwnd));
                ui.close();
            }
        }
        if p.path.is_some() && ui.button(format!("{}  Restart", I::ARROWS_CLOCKWISE)).clicked() {
            acts.push(Act::Restart(p.clone()));
            ui.close();
        }
        if ui.button(format!("{}  {}", if p.suspended { I::PLAY } else { I::PAUSE }, if p.suspended { "Resume" } else { "Suspend" })).clicked() {
            acts.push(Act::Suspend(p.pid, p.suspended, name.clone()));
            ui.close();
        }
        ui.separator();
        ui.menu_button(format!("{}  Priority", I::GAUGE), |ui| {
            let cur = Priority::from_base(p.base_priority);
            for pr in Priority::ALL {
                if ui.radio(cur == pr, pr.label()).clicked() {
                    acts.push(Act::Priority(p.clone(), pr));
                    ui.close();
                }
            }
        });
        if ui.button(format!("{}  CPU cores…", I::CPU)).clicked() {
            acts.push(Act::Affinity(p.pid, name.clone()));
            ui.close();
        }
        let pinned = self.cfg.pinned.iter().any(|n| n.eq_ignore_ascii_case(&name));
        if ui.button(format!("{}  {}", if pinned { I::PUSH_PIN_SLASH } else { I::PUSH_PIN }, if pinned { "Unpin" } else { "Pin to top" })).clicked() {
            acts.push(Act::Pin(name.clone()));
            ui.close();
        }
        if ui.button(format!("{}  Add rule…", I::LIGHTNING)).clicked() {
            acts.push(Act::Rule(name.clone()));
            ui.close();
        }
        if p.conns > 0 && ui.button(format!("{}  Show connections ({})", I::PLUGS_CONNECTED, p.conns)).clicked() {
            acts.push(Act::ShowConns(p.pid));
            ui.close();
        }
        if let Some(path) = p.path.as_deref() {
            let blocked = self.cfg.blocked_apps.iter().any(|b| b.eq_ignore_ascii_case(path));
            if ui.button(format!("{}  {}", I::PROHIBIT, if blocked { "Allow internet access" } else { "Block internet access" })).clicked() {
                acts.push(Act::Block(path.to_string()));
                ui.close();
            }
        }
        ui.separator();
        if let Some(path) = p.path.as_deref() {
            if ui.button(format!("{}  Open file location", I::FOLDER_OPEN)).clicked() {
                acts.push(Act::Open(path.to_string()));
                ui.close();
            }
            if ui.button(format!("{}  Properties", I::INFO)).clicked() {
                acts.push(Act::Props(path.to_string()));
                ui.close();
            }
        }
        ui.menu_button(format!("{}  Copy", I::COPY), |ui| {
            if ui.button("Name").clicked() {
                acts.push(Act::Copy(name.clone()));
                ui.close();
            }
            if ui.button("PID").clicked() {
                acts.push(Act::Copy(p.pid.to_string()));
                ui.close();
            }
            if let Some(path) = p.path.as_deref() {
                if ui.button("Path").clicked() {
                    acts.push(Act::Copy(path.to_string()));
                    ui.close();
                }
            }
            if let Some(c) = self.info.meta(p.key(), p.path.as_ref()).and_then(|m| m.cmdline.clone()) {
                if ui.button("Command line").clicked() {
                    acts.push(Act::Copy(c));
                    ui.close();
                }
            }
        });
        if ui.button(format!("{}  Search online", I::GLOBE)).clicked() {
            acts.push(Act::Search(name.clone()));
            ui.close();
        }
    }

    fn apply(&mut self, a: Act, ctx: &egui::Context, snap: &Snapshot) {
        match a {
            Act::Select(k) => {
                self.selected = Some(k);
                self.sel_group = None;
                self.details_open = true;
            }
            Act::SelectGroup(g) => {
                self.sel_group = Some(g);
                self.selected = None;
                self.details_open = true;
            }
            Act::ToggleSection(id) => {
                if !self.collapsed.remove(id) {
                    self.collapsed.insert(id.to_string());
                }
            }
            Act::ToggleGroup(g) => {
                if !self.expanded_groups.remove(&g) {
                    self.expanded_groups.insert(g);
                }
            }
            Act::ToggleTree(k) => {
                if !self.tree_collapsed.remove(&k) {
                    self.tree_collapsed.insert(k);
                }
            }
            Act::Kill(t, n, title) => self.kill_targets(t, n, title),
            Act::Suspend(pid, resume, name) => {
                let r = win::suspend(pid, resume);
                let msg = format!("{} {name}", if resume { "Resumed" } else { "Suspended" });
                match r {
                    Ok(()) => self.toast(ToastKind::Good, msg),
                    Err(e) => self.toast(ToastKind::Bad, format!("Couldn't change {name}: {e}")),
                }
                self.ctl.refresh();
            }
            Act::Priority(p, pr) => self.set_priority(&p, pr),
            Act::Affinity(pid, name) => {
                if let Some((mask, sys)) = crate::actions::get_affinity(pid) {
                    self.modal = Some(Modal::Affinity { pid, name, mask, sys });
                } else {
                    self.toast(ToastKind::Bad, "Couldn't read this process's CPU cores (access denied)");
                }
            }
            Act::SwitchTo(h) => {
                win::switch_to_window(h as _);
            }
            Act::CloseWindow(h) => win::close_window(h as _),
            Act::Restart(p) => match crate::actions::restart(&p) {
                Ok(()) => self.toast(ToastKind::Good, format!("Restarted {}", p.name)),
                Err(e) => self.toast(ToastKind::Bad, format!("Couldn't restart {}: {e}", p.name)),
            },
            Act::Pin(name) => {
                if let Some(i) = self.cfg.pinned.iter().position(|n| n.eq_ignore_ascii_case(&name)) {
                    self.cfg.pinned.remove(i);
                } else {
                    self.cfg.pinned.push(name);
                }
                self.cfg.save();
            }
            Act::Rule(name) => self.add_rule_for(&name),
            Act::Open(p) => win::reveal_in_explorer(&p),
            Act::Props(p) => win::show_properties(&p),
            Act::Copy(t) => self.copy(ctx, t),
            Act::Search(n) => {
                let q: String = n.chars().map(|c| if c.is_alphanumeric() || c == '.' || c == '-' { c } else { '+' }).collect();
                let _ = win::shell_open(&format!("https://duckduckgo.com/?q={q}+process"), "", "");
            }
            Act::Block(path) => self.toggle_block(path),
            Act::Sort(c) => self.toggle_sort(Some(c)),
            Act::SortName => self.toggle_sort(None),
            Act::ShowConns(pid) => {
                self.page = Page::Network;
                self.net_tab = 1;
                self.net_search = pid.to_string();
            }
        }
        let _ = snap;
    }

    pub fn toggle_sort(&mut self, c: Option<Col>) {
        if self.cfg.sort_col == c {
            self.cfg.sort_desc = !self.cfg.sort_desc;
        } else {
            self.cfg.sort_col = c;
            self.cfg.sort_desc = c.is_some_and(|c| c.numeric());
        }
        self.cfg.save();
    }

    pub fn toggle_block(&mut self, path: String) {
        let blocked = self.cfg.blocked_apps.iter().any(|b| b.eq_ignore_ascii_case(&path));
        let file = std::path::Path::new(&path).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
        if blocked {
            self.cfg.blocked_apps.retain(|b| !b.eq_ignore_ascii_case(&path));
        } else {
            self.cfg.blocked_apps.push(path.clone());
        }
        self.cfg.save();
        self.run_admin_async(AdminOp::Firewall { block: !blocked, path }, if blocked { format!("{file} can use the internet again") } else { format!("Blocked internet access for {file}") }, None);
    }

    fn summary_strip(&mut self, ui: &mut Ui, snap: &Snapshot) {
        let sys = {
            let sh = self.shared.lock().unwrap();
            (sh.sys.cpu.clone(), sh.sys.mem.clone(), sh.sys.net_recv.iter().zip(&sh.sys.net_send).map(|(a, b)| a + b).collect::<std::collections::VecDeque<f32>>(), sh.sys.gpu.clone(), {
                let mut d: std::collections::VecDeque<f32> = std::collections::VecDeque::new();
                for (k, r) in &sh.sys.disk_active {
                    let _ = k;
                    if d.is_empty() {
                        d = r.clone();
                    } else {
                        for (a, b) in d.iter_mut().zip(r) {
                            *a = a.max(*b);
                        }
                    }
                }
                d
            })
        };
        let used = snap.mem_total.saturating_sub(snap.mem_avail);
        let disk_active = snap.disks.iter().map(|d| d.active).fold(0.0, f32::max);
        let disk_rate: f64 = snap.disks.iter().map(|d| d.read + d.write).sum();
        let tiles: [(&str, &str, String, String, &std::collections::VecDeque<f32>, Color32, Option<f32>, usize); 5] = [
            (I::CPU, "CPU", fmt::pct(snap.cpu_total), format!("{} cores", snap.cores.len()), &sys.0, C_CPU(), Some(100.0), 0),
            (I::MEMORY, "Memory", fmt::bytes(used), format!("of {}", fmt::bytes(snap.mem_total)), &sys.1, C_MEM(), Some(100.0), 1),
            (I::HARD_DRIVES, "Disk", format!("{:.0}%", disk_active), fmt::rate(disk_rate), &sys.4, C_DISK(), Some(100.0), 2),
            (I::WIFI_HIGH, "Network", fmt::bits(snap.net_recv + snap.net_send), format!("↓ {}  ↑ {}", fmt::rate(snap.net_recv), fmt::rate(snap.net_send)), &sys.2, C_NET(), None, 3),
            (I::GRAPHICS_CARD, "GPU", fmt::pct(snap.gpu.util), if snap.gpu.dedicated_used > 0 { format!("{} VRAM", fmt::bytes(snap.gpu.dedicated_used)) } else { String::new() }, &sys.3, C_GPU(), Some(100.0), 4),
        ];
        let offset = self.smooth_offset();
        let w = ((ui.available_width() - 4.0 * 10.0) / 5.0).max(120.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            for (icon, label, value, sub, data, color, max, perf_idx) in tiles {
                let (rect, resp) = ui.allocate_exact_size(vec2(w, 74.0), Sense::click());
                let hover = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
                ui.painter().rect(rect, CornerRadius::same(12), lerp_color(CARD(), CARD_HI(), hover * 0.6), Stroke::new(1.0, lerp_color(BORDER_SOFT(), color.gamma_multiply(0.5), hover)), egui::StrokeKind::Inside);
                let spark = egui::Rect::from_min_max(egui::pos2(rect.left() + 1.0, rect.top() + 30.0), egui::pos2(rect.right() - 1.0, rect.bottom() - 1.0));
                crate::graph::sparkline(ui, spark.shrink2(vec2(1.0, 0.0)), data, color.gamma_multiply(0.85), max, offset);
                let p = ui.painter();
                p.text(rect.left_top() + vec2(12.0, 12.0), egui::Align2::LEFT_TOP, format!("{icon}  {label}"), egui::FontId::proportional(12.0), MUTED());
                p.text(rect.right_top() + vec2(-12.0, 9.0), egui::Align2::RIGHT_TOP, value, bold(17.0), TEXT());
                p.text(rect.right_top() + vec2(-12.0, 32.0), egui::Align2::RIGHT_TOP, sub, egui::FontId::proportional(11.0), FAINT());
                if resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("Open in Performance").clicked() {
                    self.page = Page::Performance;
                    self.perf_sel = perf_idx;
                }
            }
        });
    }

    pub fn smooth_offset(&self) -> f32 {
        if !self.cfg.smooth_graphs || self.paused {
            return 0.0;
        }
        match self.snap.at {
            Some(t) => (t.elapsed().as_secs_f32() / self.snap.interval.max(0.25)).clamp(0.0, 1.0),
            None => 0.0,
        }
    }

    fn details(&mut self, ui: &mut Ui, p: &Proc, snap: &Snapshot, acts: &mut Vec<Act>) {
        let meta = self.info.meta(p.key(), p.path.as_ref());
        egui::ScrollArea::vertical().id_salt("details-scroll").auto_shrink([false, false]).show(ui, |ui| {
            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::hover());
                    self.paint_proc_icon(ui, r, p);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        let (name, exe) = self.display_name(p);
                        ui.add(egui::Label::new(RichText::new(name).font(bold(15.5))).truncate());
                        let mut sub = exe.unwrap_or_default();
                        if let Some(c) = meta.as_ref().and_then(|m| m.company.clone()) {
                            if !sub.is_empty() {
                                sub.push_str("  ·  ");
                            }
                            sub.push_str(&c);
                        }
                        ui.add(egui::Label::new(RichText::new(sub).size(12.0).color(MUTED())).truncate());
                    });
                    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                        if ui.add(egui::Label::new(RichText::new(I::X).color(FAINT())).sense(Sense::click())).on_hover_text("Close details (Enter)").clicked() {
                            self.details_open = false;
                        }
                    });
                });
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    if p.window.as_ref().is_some_and(|w| w.hung) {
                        pill(ui, "Not responding", BAD());
                    }
                    if p.suspended {
                        pill(ui, "Suspended", BLUE());
                    }
                    if meta.as_ref().and_then(|m| m.elevated).unwrap_or(false) {
                        pill(ui, "Administrator", WARN());
                    }
                    if crate::sampler::critical(&p.name) {
                        pill(ui, "Critical system process", BAD());
                    }
                    if self.cfg.blocked_apps.iter().any(|b| Some(b.as_str()) == p.path.as_deref()) {
                        pill(ui, "Internet blocked", ORANGE());
                    }
                });
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    if button(ui, Btn::Danger, I::X_CIRCLE, "End task", true).on_hover_text("Delete").clicked() {
                        acts.push(Act::Kill(vec![p.key()], vec![p.name.clone()], format!("End {}?", p.name)));
                    }
                    let tree = crate::actions::tree_of(p.key(), &snap.procs);
                    if tree.len() > 1 && button(ui, Btn::Ghost, I::TREE_STRUCTURE, &format!("End tree ({})", tree.len()), true).on_hover_text("Shift+Delete").clicked() {
                        acts.push(Act::Kill(tree.clone(), vec![p.name.clone()], format!("End {} and its {} child processes?", p.name, tree.len() - 1)));
                    }
                    if let Some(w) = &p.window {
                        if icon_button(ui, I::ARROW_SQUARE_OUT, "Switch to window", false).clicked() {
                            acts.push(Act::SwitchTo(w.hwnd));
                        }
                    }
                    if icon_button(ui, if p.suspended { I::PLAY } else { I::PAUSE }, if p.suspended { "Resume" } else { "Suspend" }, p.suspended).clicked() {
                        acts.push(Act::Suspend(p.pid, p.suspended, p.name.clone()));
                    }
                    let r = icon_button(ui, I::DOTS_THREE_VERTICAL, "More", false);
                    egui::Popup::menu(&r).show(|ui| self.proc_menu(ui, p, snap, acts));
                });
            });
            ui.add_space(10.0);

            // Live metrics with sparklines
            let hist: Vec<crate::sampler::ProcPoint> = self.shared.lock().unwrap().proc_hist.get(&p.key()).map(|h| h.iter().copied().collect()).unwrap_or_default();
            let cpu: Vec<f32> = hist.iter().map(|h| h.cpu).collect();
            let mem: Vec<f32> = hist.iter().map(|h| h.mem as f32).collect();
            let io: Vec<f32> = hist.iter().map(|h| h.io as f32).collect();
            let net: Vec<f32> = hist.iter().map(|h| h.net as f32).collect();
            let cpu_max = crate::graph::nice_max(cpu.iter().copied().fold(5.0, f32::max));
            let mem_max = mem.iter().copied().fold(1.0, f32::max) * 1.15;
            let io_max = crate::graph::nice_max(io.iter().copied().fold(1024.0, f32::max));
            let net_max = crate::graph::nice_max(net.iter().copied().fold(1024.0, f32::max));
            let w = (ui.available_width() - 10.0) / 2.0;
            let metric = |ui: &mut Ui, label: &str, value: String, data: &[f32], color: Color32, max: f32| {
                let (rect, _) = ui.allocate_exact_size(vec2(w, 70.0), Sense::hover());
                ui.painter().rect(rect, CornerRadius::same(10), CARD(), Stroke::new(1.0, BORDER_SOFT()), egui::StrokeKind::Inside);
                crate::graph::mini(ui, egui::Rect::from_min_max(rect.left_top() + vec2(1.0, 30.0), rect.right_bottom() - vec2(1.0, 1.0)), data, color, max);
                ui.painter().text(rect.left_top() + vec2(10.0, 9.0), egui::Align2::LEFT_TOP, label, egui::FontId::proportional(11.5), FAINT());
                ui.painter().text(rect.right_top() + vec2(-10.0, 8.0), egui::Align2::RIGHT_TOP, value, bold(14.0), color);
            };
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                metric(ui, "CPU", fmt::pct(p.cpu), &cpu, C_CPU(), cpu_max);
                metric(ui, "Memory", fmt::bytes(p.mem), &mem, C_MEM(), mem_max);
            });
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                metric(ui, "Disk / I/O", fmt::rate(p.io_rate), &io, C_DISK(), io_max);
                if snap.per_app_net {
                    metric(ui, "Network", fmt::rate(p.net_rate()), &net, C_NET(), net_max);
                } else {
                    metric(ui, "GPU", fmt::pct(p.gpu), &[], C_GPU(), 100.0);
                }
            });
            ui.add_space(10.0);
            self.app_history_card(ui, &p.name);

            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                label_caps(ui, "Details");
                let parent = snap.procs.iter().find(|x| x.pid == p.ppid && x.create_time <= p.create_time);
                kv(ui, "PID", &p.pid.to_string());
                if let Some(r) = meta.as_ref().and_then(|m| m.cmdline.as_deref()).and_then(crate::roles::role) {
                    kv(ui, "Role", &r);
                }
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Parent").color(FAINT()));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| match parent {
                        Some(pp) => {
                            if ui.link(format!("{} ({})", pp.name, pp.pid)).clicked() {
                                acts.push(Act::Select(pp.key()));
                                self.scroll_to_sel = true;
                            }
                        }
                        None => {
                            ui.label(RichText::new(format!("{} (exited)", p.ppid)).color(MUTED()));
                        }
                    });
                });
                if let Some(u) = meta.as_ref().and_then(|m| m.user.clone()) {
                    kv(ui, "User", &u);
                }
                kv(ui, "Started", &format!("{}  ({})", fmt::when(win::filetime_to_unix(p.create_time)), fmt::ago(win::filetime_to_unix(p.create_time))));
                kv(ui, "CPU time", &fmt::cpu_time(p.cpu_time));
                kv(ui, "Priority", Priority::from_base(p.base_priority).label());
                kv(ui, "Private bytes", &fmt::bytes(p.private_bytes));
                kv(ui, "Working set", &format!("{}  (peak {})", fmt::bytes(p.working_set), fmt::bytes(p.peak_working_set)));
                kv(ui, "Threads / handles", &format!("{} / {}", p.threads, p.handles));
                if p.gpu_mem > 0 {
                    kv(ui, "GPU memory", &fmt::bytes(p.gpu_mem));
                }
                if snap.per_app_net {
                    kv(ui, "Network (session)", &fmt::bytes(p.net_total));
                }
                if let Some(w) = &p.window {
                    kv(ui, "Window", &w.title);
                }
            });
            if let Some(svcs) = snap.services_by_pid.get(&p.pid) {
                ui.add_space(10.0);
                card().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    label_caps(ui, &format!("Hosts {} service{}", svcs.len(), if svcs.len() == 1 { "" } else { "s" }));
                    for (n, d) in svcs.iter().take(30) {
                        ui.horizontal(|ui| {
                            ui.add(egui::Label::new(RichText::new(d).color(TEXT())).truncate());
                            ui.label(RichText::new(n).size(11.0).color(FAINT()));
                        });
                    }
                });
            }
            if let Some(path) = p.path.as_deref() {
                ui.add_space(10.0);
                card().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        label_caps(ui, "Location");
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if icon_button(ui, I::COPY, "Copy path", false).clicked() {
                                acts.push(Act::Copy(path.to_string()));
                            }
                            if icon_button(ui, I::FOLDER_OPEN, "Open file location", false).clicked() {
                                acts.push(Act::Open(path.to_string()));
                            }
                        });
                    });
                    ui.add(egui::Label::new(RichText::new(path).size(12.0).color(MUTED())).wrap());
                    if let Some(c) = meta.as_ref().and_then(|m| m.cmdline.clone()) {
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            label_caps(ui, "Command line");
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if icon_button(ui, I::COPY, "Copy command line", false).clicked() {
                                    acts.push(Act::Copy(c.clone()));
                                }
                            });
                        });
                        egui::Frame::new().fill(FIELD()).corner_radius(CornerRadius::same(8)).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.add(egui::Label::new(RichText::new(c).monospace().size(11.5).color(MUTED())).wrap());
                        });
                    }
                });
            }
            let conns: Vec<&crate::net::Conn> = snap.conns.iter().filter(|c| c.pid == p.pid && c.remote.is_some_and(|r| !r.ip().is_unspecified())).collect();
            if !conns.is_empty() {
                ui.add_space(10.0);
                card().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        label_caps(ui, &format!("Connections ({})", conns.len()));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.link("View all").clicked() {
                                acts.push(Act::ShowConns(p.pid));
                            }
                        });
                    });
                    for c in conns.iter().take(8) {
                        let r = c.remote.unwrap();
                        let host = self.rdns.get(r.ip()).unwrap_or_else(|| r.ip().to_string());
                        ui.horizontal(|ui| {
                            ui.add(egui::Label::new(RichText::new(format!("{host}:{}", r.port())).size(12.0)).truncate());
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.label(RichText::new(crate::net::tcp_state(c.state)).size(11.0).color(FAINT()));
                            });
                        });
                    }
                });
            }
        });
    }

    /// Last 6 / 24 hours of this app's CPU and memory (all its processes together).
    fn app_history_card(&mut self, ui: &mut Ui, exe: &str) {
        let now = win::now_unix();
        let range = if self.app_range == 0 { 6 * 3600 } else { 86400 };
        let (cpu, mem, have) = {
            let sh = self.shared.lock().unwrap();
            let s = sh.apphist.series(exe);
            let have = s.and_then(|s| s.m.first().copied()).map(|m| now - m as i64 * 60).unwrap_or(0);
            let (c, m) = crate::apphist::buckets(s, now, range, crate::sampler::HIST);
            (c, m, have)
        };
        let leak = self.leak_for(exe).cloned();
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                label_caps(ui, "History");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let mut r = self.app_range;
                    if segmented(ui, &mut r, &["6 h", "24 h"]) {
                        self.app_range = r;
                    }
                });
            });
            if let Some(l) = &leak {
                egui::Frame::new().fill(ORANGE().gamma_multiply(0.12)).stroke(Stroke::new(1.0, ORANGE().gamma_multiply(0.35))).corner_radius(CornerRadius::same(8)).inner_margin(egui::Margin::symmetric(10, 7)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(format!("{}  Possible memory leak. {}", I::WARNING, l.text())).size(12.0).color(TEXT()));
                });
                ui.add_space(6.0);
            }
            if cpu.iter().all(|v| v.is_nan()) {
                ui.label(RichText::new("Nothing recorded yet. Sentinel records each app once a minute while it uses more than 30 MB or 0.5% CPU.").size(12.0).color(FAINT()));
                return;
            }
            let w = ui.available_width();
            let interval = range as f32 / crate::sampler::HIST as f32;
            let (rect, _) = ui.allocate_exact_size(vec2(w, 74.0), Sense::hover());
            let ex = crate::graph::Ex { end_unix: Some(now as f64), link: Some("app"), select: false };
            crate::graph::graph_ex(ui, rect, &[crate::graph::Series { data: &mem, color: C_MEM(), label: "Memory" }], &crate::graph::Opts { max: None, fmt: &|v| fmt::bytes(v as u64), offset: 0.0, grid: true, rounded: true, interval }, &ex);
            ui.add_space(4.0);
            let (rect, _) = ui.allocate_exact_size(vec2(w, 58.0), Sense::hover());
            crate::graph::graph_ex(ui, rect, &[crate::graph::Series { data: &cpu, color: C_CPU(), label: "CPU" }], &crate::graph::Opts { max: None, fmt: &|v| format!("{v:.0}%"), offset: 0.0, grid: true, rounded: true, interval }, &ex);
            crate::graph::range_stats(ui, &mem, &|v| fmt::bytes(v as u64), now as f64, interval, C_MEM());
            if have < range - 300 {
                ui.label(RichText::new(format!("Recorded for {} so far", fmt::duration(have.max(0) as u64))).size(11.0).color(FAINT()));
            }
        });
        ui.add_space(10.0);
    }

    /// Search ":3000": who listens on / talks over the port, with a one-click fix.
    fn port_banner(&mut self, ui: &mut Ui, port: u16, snap: &Snapshot) -> Option<Act> {
        let mut listeners: Vec<(u32, &str)> = Vec::new();
        let mut users = 0usize;
        for c in &snap.conns {
            if c.local.port() == port && (c.state == 2 || c.proto == crate::net::Proto::Udp) {
                if !listeners.iter().any(|(p, _)| *p == c.pid) {
                    listeners.push((c.pid, if c.proto == crate::net::Proto::Udp { "UDP" } else { "TCP" }));
                }
            } else if c.local.port() == port || c.remote.is_some_and(|r| r.port() == port) {
                users += 1;
            }
        }
        ui.add_space(8.0);
        let col = if listeners.is_empty() { GOOD() } else { WARN() };
        let mut kill: Option<Act> = None;
        egui::Frame::new().fill(col.gamma_multiply(0.10)).stroke(Stroke::new(1.0, col.gamma_multiply(0.35))).corner_radius(CornerRadius::same(10)).inner_margin(egui::Margin::symmetric(14, 9)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            if listeners.is_empty() {
                ui.label(RichText::new(format!("{}  Port {port} is free: nothing is listening on it.{}", I::CHECK_CIRCLE, if users > 0 { format!(" {users} connection(s) use it as their remote port.") } else { String::new() })).color(TEXT()));
                return;
            }
            for (pid, proto) in &listeners {
                let Some(p) = snap.procs.iter().find(|p| p.pid == *pid) else { continue };
                ui.horizontal(|ui| {
                    ui.label(RichText::new(I::PLUGS_CONNECTED).color(WARN()));
                    let (name, _) = self.display_name(p);
                    ui.label(RichText::new(format!("{name} is listening on {proto} port {port}")).color(TEXT()));
                    ui.label(RichText::new(format!("{} · PID {}", p.name, p.pid)).size(12.0).color(FAINT()));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if button_sized(ui, Btn::Danger, I::X_CIRCLE, "Free this port", !crate::sampler::critical(&p.name) && p.pid > 4, 26.0).on_hover_text("End the process that holds it").clicked() {
                            kill = Some(Act::Kill(vec![p.key()], vec![p.name.clone()], format!("End {} to free port {port}?", p.name)));
                        }
                    });
                });
            }
        });
        kill
    }

    fn group_details(&mut self, ui: &mut Ui, g: &str, snap: &Snapshot, _rows: &[Row], acts: &mut Vec<Act>) {
        let members: Vec<&Proc> = snap.procs.iter().filter(|p| group_key_matches(g, p)).collect();
        if members.is_empty() {
            self.sel_group = None;
            return;
        }
        let first = members[0];
        let (name, _) = self.display_name(first);
        let cpu: f32 = members.iter().map(|p| p.cpu).sum();
        let mem: u64 = members.iter().map(|p| p.mem).sum();
        egui::ScrollArea::vertical().id_salt("gdetails").auto_shrink([false, false]).show(ui, |ui| {
            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::hover());
                    self.paint_proc_icon(ui, r, first);
                    ui.vertical(|ui| {
                        ui.add(egui::Label::new(RichText::new(&name).font(bold(15.5))).truncate());
                        ui.label(RichText::new(format!("{} processes · {} · {}", members.len(), fmt::pct(cpu), fmt::bytes(mem))).size(12.0).color(MUTED()));
                    });
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if button(ui, Btn::Danger, I::X_CIRCLE, &format!("End all {}", members.len()), true).clicked() {
                        acts.push(Act::Kill(members.iter().map(|p| p.key()).collect(), vec![first.name.clone()], format!("End all {} {} processes?", members.len(), first.name)));
                    }
                    if let Some(w) = members.iter().find_map(|p| p.window.as_ref()) {
                        if icon_button(ui, I::ARROW_SQUARE_OUT, "Switch to window", false).clicked() {
                            acts.push(Act::SwitchTo(w.hwnd));
                        }
                    }
                });
            });
            ui.add_space(10.0);
            self.app_history_card(ui, &first.name);
            // What the helper processes are for (browsers and Electron apps).
            let mut roles: Vec<(String, usize, f32, u64)> = Vec::new();
            for p in &members {
                let cmd = self.info.meta(p.key(), p.path.as_ref()).and_then(|m| m.cmdline.clone()).unwrap_or_default();
                let r = crate::roles::role(&cmd).unwrap_or_else(|| if members.len() > 1 && crate::roles::is_main(&cmd) { "Main process".into() } else { String::new() });
                if r.is_empty() {
                    continue;
                }
                match roles.iter_mut().find(|x| x.0 == r) {
                    Some(x) => {
                        x.1 += 1;
                        x.2 += p.cpu;
                        x.3 += p.mem;
                    }
                    None => roles.push((r, 1, p.cpu, p.mem)),
                }
            }
            if roles.len() > 1 {
                roles.sort_by(|a, b| b.3.cmp(&a.3));
                card().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    label_caps(ui, "What the processes do");
                    let total = mem.max(1) as f32;
                    for (r, n, c, m) in &roles {
                        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::hover());
                        let p = ui.painter();
                        p.text(rect.left_top() + vec2(0.0, 2.0), egui::Align2::LEFT_TOP, if *n > 1 { format!("{r}  ×{n}") } else { r.clone() }, egui::FontId::proportional(13.0), TEXT());
                        p.text(rect.right_top() + vec2(0.0, 2.0), egui::Align2::RIGHT_TOP, format!("{}  ·  {}", fmt::pct(*c), fmt::bytes(*m)), egui::FontId::proportional(12.0), MUTED());
                        let bar = egui::Rect::from_min_size(rect.left_top() + vec2(0.0, 22.0), vec2(rect.width(), 4.0));
                        p.rect_filled(bar, CornerRadius::same(2), FIELD());
                        p.rect_filled(egui::Rect::from_min_size(bar.min, vec2(bar.width() * (*m as f32 / total).clamp(0.0, 1.0), 4.0)), CornerRadius::same(2), C_MEM());
                    }
                    ui.label(RichText::new("Tab titles aren't visible from outside the browser; its own task manager (Shift+Esc in Chrome and Edge) can show them per tab.").size(11.5).color(FAINT()));
                });
                ui.add_space(10.0);
            }
            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                label_caps(ui, "Processes");
                let mut sorted = members.clone();
                sorted.sort_by(|a, b| b.mem.cmp(&a.mem));
                for p in sorted {
                    let title = p.window.as_ref().map(|w| w.title.clone()).unwrap_or_else(|| format!("PID {}", p.pid));
                    let r = ui.horizontal(|ui| {
                        ui.add(egui::Label::new(RichText::new(title).size(12.5)).truncate().sense(Sense::click()))
                    });
                    let role = self.info.meta(p.key(), p.path.as_ref()).and_then(|m| m.cmdline.clone()).and_then(|c| crate::roles::role(&c));
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{}{} · {} · PID {}", role.map(|r| format!("{r} · ")).unwrap_or_default(), fmt::pct(p.cpu), fmt::bytes(p.mem), p.pid)).size(11.0).color(FAINT()));
                    });
                    if r.inner.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                        acts.push(Act::Select(p.key()));
                    }
                }
            });
        });
    }
}

/// ":3000" or "port 3000" in the search box.
pub fn port_query(q: &str) -> Option<u16> {
    let q = q.trim();
    let n = q.strip_prefix(':').or_else(|| q.strip_prefix("port ")).or_else(|| q.strip_prefix("port:"))?;
    n.trim().parse::<u16>().ok().filter(|p| *p > 0)
}

fn group_key_matches(g: &str, p: &Proc) -> bool {
    let k = g.split_once('|').map(|x| x.1).unwrap_or("");
    p.path.as_deref().map(|s| s.to_lowercase()).unwrap_or_else(|| p.name.to_lowercase()) == k
}

/// One row's usage numbers, drawn so the busy ones jump out: idle values fade, heavier values get
/// bolder and turn amber/red, and a thin bar under each number shows its size at a glance.
struct Usage {
    cpu: f32,
    mem: u64,
    io: f64,
    net: f64,
    gpu: f32,
}

impl Usage {
    fn cell(&self, ui: &mut Ui, c: Col, total_mem: f64, per_app_net: bool) {
        const MB: f64 = 1048576.0;
        // (text, load 0..1 for the bar, severity 0..1 for color, idle?)
        let (text, load, sev, idle, color) = match c {
            Col::Cpu => (fmt::pct(self.cpu), self.cpu / 100.0, self.cpu / 60.0, self.cpu < 0.05, C_CPU()),
            Col::Gpu => (fmt::pct(self.gpu), self.gpu / 100.0, self.gpu / 70.0, self.gpu < 0.05, C_GPU()),
            Col::Memory => {
                let share = (self.mem as f64 / total_mem) as f32;
                // Relative to installed RAM: amber from ~7.5%, red from ~15% of memory.
                (fmt::bytes(self.mem), share * 4.0, share / 0.15, self.mem < 8 * 1048576, C_MEM())
            }
            Col::Disk => (if self.io < 1.0 { "0 MB/s".into() } else { fmt::rate(self.io) }, (self.io / (50.0 * MB)) as f32, (self.io / (80.0 * MB)) as f32, self.io < 1024.0, C_DISK()),
            Col::Network if !per_app_net => ("—".into(), 0.0, 0.0, true, C_NET()),
            Col::Network => (if self.net < 1.0 { "0 MB/s".into() } else { fmt::rate(self.net) }, (self.net / (5.0 * MB)) as f32, (self.net / (12.0 * MB)) as f32, self.net < 1024.0, C_NET()),
            _ => return,
        };
        let r = ui.max_rect();
        let load = load.clamp(0.0, 1.0);
        if !idle && load > 0.002 {
            // Crisp 2px bar along the bottom; a faint tint only once the value is notable.
            if load > 0.06 {
                ui.painter().rect_filled(r.expand2(vec2(5.0, 1.0)), CornerRadius::ZERO, color.gamma_multiply(0.04 + 0.16 * load));
            }
            let w = (r.width() * load.sqrt()).max(2.0);
            let bar = egui::Rect::from_min_size(egui::pos2(r.right() - w, r.bottom() - 3.0), vec2(w, 2.0));
            ui.painter().rect_filled(bar, CornerRadius::same(1), color.gamma_multiply(0.85));
        }
        let rt = if idle {
            RichText::new(text).color(FAINT())
        } else if sev >= 1.0 {
            RichText::new(text).font(bold(13.5)).color(BAD())
        } else if sev >= 0.5 {
            RichText::new(text).font(bold(13.5)).color(WARN())
        } else if load >= 0.08 || sev >= 0.15 {
            RichText::new(text).font(bold(13.5)).color(TEXT())
        } else {
            RichText::new(text).color(MUTED())
        };
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(rt);
        });
    }
}

fn num_cell(ui: &mut Ui, text: &str, bg: Color32) {
    let r = ui.max_rect().expand2(vec2(5.0, 1.0));
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(r, CornerRadius::ZERO, bg);
    }
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.label(RichText::new(text).color(TEXT()));
    });
}

fn kv(ui: &mut Ui, k: &str, v: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(k).color(FAINT()));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add(egui::Label::new(RichText::new(v).color(TEXT())).truncate());
        });
    });
}

/// Sortable header cell with an optional total on top. Returns true when clicked.
/// Sortable header cell: the system total big and coloured on top, the column name below.
/// Returns true when clicked.
fn header_cell(ui: &mut Ui, label: &str, total: Option<&str>, color: Color32, active: bool, desc: bool, right: bool) -> bool {
    let rect = ui.max_rect();
    let resp = ui.interact(rect, ui.scope_id().with(("hdr", label)), Sense::click());
    let arrow = if active { if desc { I::CARET_DOWN } else { I::CARET_UP } } else { "" };
    let name = if right { format!("{arrow}{}", label.to_uppercase()) } else { format!("{}{arrow}", label.to_uppercase()) };
    let name_col = if active { ACCENT_HI() } else if resp.hovered() { TEXT() } else { MUTED() };
    let (align_x, x) = if right { (egui::Align2::RIGHT_BOTTOM, rect.right()) } else { (egui::Align2::LEFT_BOTTOM, rect.left()) };
    let p = ui.painter();
    let name_font = bold(10.5);
    if let Some(t) = total {
        p.text(egui::pos2(x, rect.top() + 23.0), align_x, t, bold(15.5), color);
        p.text(egui::pos2(x, rect.bottom() - 4.0), align_x, name, name_font, name_col);
    } else {
        p.text(egui::pos2(x, rect.bottom() - 4.0), align_x, name, name_font, name_col);
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}
