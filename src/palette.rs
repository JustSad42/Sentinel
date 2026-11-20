//! Command palette (Ctrl+K): jump to pages, run commands, and act on any app by typing its name.
//! Pick an app, then pick what to do with it (end, restart, open location, block…).

use crate::app::{App, Modal, PAGES, Page, ToastKind};
use crate::icons as I;
use crate::sampler::Proc;
use crate::theme::*;
use egui::{Color32, CornerRadius, RichText, Sense, Stroke, vec2};

#[derive(Default)]
pub struct Palette {
    pub open: bool,
    query: String,
    sel: usize,
    /// Exe name (lowercase) chosen in step one; step two lists actions for it.
    target: Option<String>,
    focus: bool,
}

impl Palette {
    /// Test copies only: `SENTINEL_PALETTE=<query>` opens the palette on launch.
    pub fn test_open() -> Self {
        match std::env::var("SENTINEL_PALETTE") {
            Ok(q) if crate::config::test_mode() => Palette { open: true, focus: true, query: q, ..Default::default() },
            _ => Default::default(),
        }
    }

    pub fn toggle(&mut self) {
        if self.open {
            self.open = false;
        } else {
            *self = Palette { open: true, focus: true, ..Default::default() };
        }
    }
}

#[derive(Clone, Copy)]
enum Cmd {
    RunTask,
    OnTop,
    Pause,
    Compact,
    Theme,
    Overlay,
    Report,
    Shortcuts,
    Hide,
    Quit,
}

#[derive(Clone, Copy)]
enum PAct {
    Show,
    End,
    Restart,
    Open,
    Block,
    Pin,
    CopyPath,
}

#[derive(Clone)]
enum Item {
    Page(usize),
    Cmd(Cmd),
    App(String),
    Act(PAct),
}

struct Row {
    item: Item,
    icon: &'static str,
    title: String,
    hint: String,
    color: Color32,
    proc: Option<Proc>,
}

fn score(hay: &str, q: &str) -> Option<u32> {
    if q.is_empty() {
        return Some(1);
    }
    let h = hay.to_lowercase();
    if h.starts_with(q) {
        Some(3)
    } else if h.split(|c: char| !c.is_alphanumeric()).any(|w| w.starts_with(q)) {
        Some(2)
    } else if h.contains(q) {
        Some(1)
    } else {
        None
    }
}

impl App {
    fn palette_name(&self, p: &Proc) -> String {
        self.info.meta(p.key(), p.path.as_ref()).and_then(|m| m.description.clone()).filter(|d| !d.is_empty()).unwrap_or_else(|| p.name.clone())
    }

    /// The main process of an app: the one whose parent isn't the same exe, else the biggest.
    fn palette_root<'a>(&self, members: &[&'a Proc]) -> Option<&'a Proc> {
        let names: std::collections::HashSet<u32> = members.iter().map(|p| p.pid).collect();
        members.iter().copied().filter(|p| !names.contains(&p.ppid)).max_by_key(|p| p.mem).or_else(|| members.iter().copied().max_by_key(|p| p.mem))
    }

    fn palette_rows(&self) -> Vec<Row> {
        let q = self.palette.query.trim().to_lowercase();
        let snap = self.snap.clone();
        let mut rows = Vec::new();
        if let Some(t) = &self.palette.target {
            let members: Vec<&Proc> = snap.procs.iter().filter(|p| p.name.to_lowercase() == *t).collect();
            let Some(root) = self.palette_root(&members) else { return rows };
            let name = self.palette_name(root);
            let n = members.len();
            let blocked = root.path.as_deref().is_some_and(|p| self.cfg.blocked_apps.iter().any(|b| b.eq_ignore_ascii_case(p)));
            let pinned = self.cfg.pinned.iter().any(|x| x.eq_ignore_ascii_case(&root.name));
            let mut acts = vec![
                (PAct::Show, I::LIST_BULLETS, format!("Show {name} in Processes"), String::new(), TEXT()),
                (PAct::End, I::X_CIRCLE, if n > 1 { format!("End {name} ({n} processes)") } else { format!("End {name}") }, "Del".into(), BAD()),
            ];
            if root.path.is_some() {
                acts.push((PAct::Restart, I::ARROWS_CLOCKWISE, format!("Restart {name}"), String::new(), TEXT()));
                acts.push((PAct::Open, I::FOLDER_OPEN, "Open file location".into(), String::new(), TEXT()));
                acts.push((PAct::Block, I::PROHIBIT, if blocked { "Allow internet access".into() } else { "Block internet access".into() }, String::new(), if blocked { GOOD() } else { WARN() }));
                acts.push((PAct::CopyPath, I::COPY, "Copy path".into(), String::new(), TEXT()));
            }
            acts.push((PAct::Pin, I::PUSH_PIN, if pinned { "Unpin from top".into() } else { "Pin to top".into() }, String::new(), TEXT()));
            for (a, icon, title, hint, color) in acts {
                if score(&title, &q).is_some() {
                    rows.push(Row { item: Item::Act(a), icon, title, hint, color, proc: None });
                }
            }
            return rows;
        }
        // Apps (grouped by exe), best matches first.
        let mut groups: Vec<(u32, String, Vec<&Proc>)> = Vec::new();
        let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for p in snap.procs.iter().filter(|p| p.pid > 4 && !p.name.is_empty()) {
            let k = p.name.to_lowercase();
            match seen.get(&k) {
                Some(&i) => groups[i].2.push(p),
                None => {
                    seen.insert(k.clone(), groups.len());
                    groups.push((0, k, vec![p]));
                }
            }
        }
        let mut apps: Vec<(u32, f32, Row)> = Vec::new();
        if !q.is_empty() {
            for (_, k, members) in &groups {
                let Some(root) = self.palette_root(members) else { continue };
                let name = self.palette_name(root);
                let s = score(&name, &q).max(score(k, &q));
                if let Some(s) = s {
                    let cpu: f32 = members.iter().map(|p| p.cpu).sum();
                    let mem: u64 = members.iter().map(|p| p.mem).sum();
                    let hint = format!("{}{}  ·  {}", if members.len() > 1 { format!("{} processes  ·  ", members.len()) } else { String::new() }, crate::fmt::pct(cpu), crate::fmt::bytes(mem));
                    apps.push((s, mem as f32, Row { item: Item::App(k.clone()), icon: I::APP_WINDOW, title: name, hint, color: TEXT(), proc: Some(root.clone()) }));
                }
            }
            apps.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.total_cmp(&a.1)));
        }
        rows.extend(apps.into_iter().take(7).map(|a| a.2));
        // Pages
        for (i, (_, icon, label)) in PAGES.iter().enumerate() {
            if score(label, &q).is_some() || (!q.is_empty() && score("go to", &q).is_some()) {
                rows.push(Row { item: Item::Page(i), icon, title: format!("Go to {label}"), hint: format!("Ctrl+{}", i + 1), color: TEXT(), proc: None });
            }
        }
        // Commands
        let cmds = [
            (Cmd::RunTask, I::TERMINAL_WINDOW, "Run new task".to_string(), "Ctrl+N"),
            (Cmd::OnTop, I::PUSH_PIN, if self.cfg.always_on_top { "Turn off always on top".into() } else { "Keep Sentinel always on top".into() }, ""),
            (Cmd::Pause, if self.paused { I::PLAY } else { I::PAUSE }, if self.paused { "Resume updates".into() } else { "Pause updates".into() }, "Ctrl+P"),
            (Cmd::Compact, I::ROWS, if self.cfg.compact_rows { "Use roomy rows".into() } else { "Use compact rows".into() }, ""),
            (Cmd::Theme, I::SUN, if self.cfg.light_theme { "Switch to dark theme".into() } else { "Switch to light theme".into() }, ""),
            (Cmd::Overlay, I::PICTURE_IN_PICTURE, if self.cfg.overlay { "Hide the mini overlay".into() } else { "Show the mini overlay".into() }, ""),
            (Cmd::Report, I::FILE_TEXT, "Export system report…".into(), ""),
            (Cmd::Shortcuts, I::KEYBOARD, "Keyboard shortcuts".into(), "F1"),
            (Cmd::Hide, I::EYE_SLASH, "Hide Sentinel".into(), "Esc"),
            (Cmd::Quit, I::POWER, "Quit Sentinel".into(), "Ctrl+Q"),
        ];
        for (c, icon, title, hint) in cmds {
            if score(&title, &q).is_some() {
                rows.push(Row { item: Item::Cmd(c), icon, title, hint: hint.into(), color: TEXT(), proc: None });
            }
        }
        rows
    }

    pub fn palette_ui(&mut self, ctx: &egui::Context) {
        if !self.palette.open {
            return;
        }
        let rows = self.palette_rows();
        if self.palette.sel >= rows.len() {
            self.palette.sel = rows.len().saturating_sub(1);
        }
        let (up, down, enter, esc, back) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
                i.key_pressed(egui::Key::Backspace),
            )
        });
        if up {
            self.palette.sel = self.palette.sel.saturating_sub(1);
        }
        if down && !rows.is_empty() {
            self.palette.sel = (self.palette.sel + 1).min(rows.len() - 1);
        }
        let mut run: Option<Item> = None;
        if enter {
            run = rows.get(self.palette.sel).map(|r| r.item.clone());
        }
        if esc {
            if self.palette.target.is_some() {
                self.palette.target = None;
                self.palette.query.clear();
                self.palette.sel = 0;
            } else {
                self.palette.open = false;
            }
            return;
        }
        let empty_query = self.palette.query.is_empty();
        let frame = egui::Frame::new().fill(CARD()).stroke(Stroke::new(1.0, BORDER())).corner_radius(CornerRadius::same(14)).inner_margin(egui::Margin::same(10)).shadow(egui::Shadow { offset: [0, 18], blur: 50, spread: 0, color: Color32::from_black_alpha(170) });
        let id = egui::Id::unique("palette-field");
        // Pinned near the top so it doesn't jump around as results change.
        let top = (ctx.content_rect().height() * 0.16).clamp(60.0, 180.0);
        let resp = egui::Modal::new(egui::Id::unique("palette")).area(egui::Modal::default_area(egui::Id::unique("palette")).anchor(egui::Align2::CENTER_TOP, [0.0, top])).frame(frame).backdrop_color(Color32::from_black_alpha(110)).show(ctx, |ui| {
            ui.set_width(600.0);
            ui.horizontal(|ui| {
                if let Some(t) = &self.palette.target {
                    let label = self.snap.procs.iter().find(|p| p.name.to_lowercase() == *t).map(|p| self.palette_name(p)).unwrap_or_default();
                    pill(ui, &label, ACCENT_HI());
                }
                let hint = if self.palette.target.is_some() { "What should happen?" } else { "Type an app, page or command…" };
                let r = ui.add(egui::TextEdit::singleline(&mut self.palette.query).id(id).hint_text(hint).frame(egui::Frame::NONE).font(egui::FontId::proportional(17.0)).desired_width(f32::INFINITY));
                if self.palette.focus {
                    r.request_focus();
                    self.palette.focus = false;
                }
                if r.changed() {
                    self.palette.sel = 0;
                }
            });
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(2.0);
            if rows.is_empty() {
                ui.add_space(14.0);
                ui.vertical_centered(|ui| ui.label(RichText::new("No matches").color(FAINT())));
                ui.add_space(14.0);
                return;
            }
            egui::ScrollArea::vertical().max_height(420.0).auto_shrink([false, true]).show(ui, |ui| {
                for (i, row) in rows.iter().enumerate() {
                    let active = i == self.palette.sel;
                    let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
                    if active {
                        ui.painter().rect_filled(rect, CornerRadius::same(8), ACCENT().gamma_multiply(0.18));
                        if up || down {
                            ui.scroll_to_rect(rect, None);
                        }
                    } else if r.hovered() {
                        ui.painter().rect_filled(rect, CornerRadius::same(8), CARD_HI());
                    }
                    let ir = egui::Rect::from_center_size(egui::pos2(rect.left() + 22.0, rect.center().y), vec2(22.0, 22.0));
                    match &row.proc {
                        Some(p) => self.paint_proc_icon(ui, ir, p),
                        None => {
                            ui.painter().text(ir.center(), egui::Align2::CENTER_CENTER, row.icon, egui::FontId::proportional(17.0), if active { ACCENT_HI() } else { MUTED() });
                        }
                    }
                    ui.painter().text(egui::pos2(rect.left() + 44.0, rect.center().y), egui::Align2::LEFT_CENTER, &row.title, egui::FontId::proportional(14.5), row.color);
                    let hint = if active && matches!(row.item, Item::App(_)) { "Enter for actions".to_string() } else { row.hint.clone() };
                    if !hint.is_empty() {
                        ui.painter().text(egui::pos2(rect.right() - 12.0, rect.center().y), egui::Align2::RIGHT_CENTER, hint, egui::FontId::proportional(12.0), FAINT());
                    }
                    if r.clicked() {
                        run = Some(row.item.clone());
                    }
                    if r.hovered() && ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO) {
                        self.palette.sel = i;
                    }
                }
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("↑↓ move   Enter run   Esc back").size(11.0).color(FAINT()));
            });
        });
        if resp.should_close() {
            self.palette.open = false;
        }
        if back && empty_query && self.palette.target.is_some() {
            self.palette.target = None;
            self.palette.sel = 0;
        }
        if let Some(item) = run {
            self.palette_run(ctx, item);
        }
    }

    fn palette_run(&mut self, ctx: &egui::Context, item: Item) {
        match item {
            Item::App(k) => {
                self.palette.target = Some(k);
                self.palette.query.clear();
                self.palette.sel = 0;
                self.palette.focus = true;
                return;
            }
            Item::Page(i) => self.page = PAGES[i].0,
            Item::Cmd(c) => match c {
                Cmd::RunTask => self.modal = Some(Modal::RunTask { cmd: String::new(), admin: false }),
                Cmd::OnTop => {
                    let on = !self.cfg.always_on_top;
                    self.set_on_top(ctx, on);
                }
                Cmd::Pause => self.paused = !self.paused,
                Cmd::Compact => {
                    self.cfg.compact_rows = !self.cfg.compact_rows;
                    self.cfg.save();
                }
                Cmd::Theme => {
                    self.cfg.light_theme = !self.cfg.light_theme;
                    self.cfg.save();
                    crate::theme::apply_theme(ctx, &self.cfg);
                }
                Cmd::Overlay => {
                    self.cfg.overlay = !self.cfg.overlay;
                    self.cfg.save();
                }
                Cmd::Report => self.export_report(),
                Cmd::Shortcuts => self.modal = Some(Modal::Shortcuts),
                Cmd::Hide => self.hide(ctx),
                Cmd::Quit => self.quit(ctx),
            },
            Item::Act(a) => {
                let Some(t) = self.palette.target.clone() else { return };
                let snap = self.snap.clone();
                let members: Vec<&Proc> = snap.procs.iter().filter(|p| p.name.to_lowercase() == t).collect();
                let Some(root) = self.palette_root(&members).cloned() else { return };
                let name = self.palette_name(&root);
                match a {
                    PAct::Show => {
                        self.page = Page::Processes;
                        self.search.clear();
                        self.selected = Some(root.key());
                        self.sel_group = None;
                        self.details_open = true;
                        self.scroll_to_sel = true;
                    }
                    PAct::End => {
                        let keys: Vec<(u32, i64)> = members.iter().map(|p| p.key()).collect();
                        let title = if keys.len() > 1 { format!("End all {} {name} processes?", keys.len()) } else { format!("End {name}?") };
                        self.kill_targets(keys, vec![root.name.clone()], title);
                    }
                    PAct::Restart => match crate::actions::restart(&root) {
                        Ok(()) => self.toast(ToastKind::Good, format!("Restarted {name}")),
                        Err(e) => self.toast(ToastKind::Bad, format!("Couldn't restart {name}: {e}")),
                    },
                    PAct::Open => {
                        if let Some(p) = root.path.as_deref() {
                            crate::win::reveal_in_explorer(p);
                        }
                    }
                    PAct::Block => {
                        if let Some(p) = root.path.as_deref() {
                            self.toggle_block(p.to_string());
                        }
                    }
                    PAct::Pin => {
                        if let Some(i) = self.cfg.pinned.iter().position(|n| n.eq_ignore_ascii_case(&root.name)) {
                            self.cfg.pinned.remove(i);
                        } else {
                            self.cfg.pinned.push(root.name.clone());
                        }
                        self.cfg.save();
                    }
                    PAct::CopyPath => {
                        if let Some(p) = root.path.as_deref() {
                            ctx.copy_text(p.to_string());
                            self.toast(ToastKind::Info, "Copied to clipboard");
                        }
                    }
                }
                self.ctl.refresh();
            }
        }
        self.palette.open = false;
    }
}
