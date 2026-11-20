//! Startup, Services, History, Rules and Settings pages, plus modal dialogs.

use crate::admin::{self, AdminOp, ServiceAction};
use crate::app::{App, Modal, Msg, ToastKind, header};
use crate::config::{Priority, Rule, RuleAction, glob_match};
use crate::fmt;
use crate::icons as I;
use crate::sampler::{HistEntry, LogKind};
use crate::theme::*;
use crate::{integration, updater, win};
use egui::{Align, Color32, CornerRadius, Layout, RichText, Sense, Stroke, Ui, vec2};
use egui_extras::{Column, TableBuilder};
use std::sync::atomic::Ordering;

fn table_frame() -> egui::Frame {
    egui::Frame::new().fill(CARD()).stroke(Stroke::new(1.0, BORDER_SOFT())).corner_radius(CornerRadius::same(12)).inner_margin(egui::Margin::same(6))
}

fn th(ui: &mut Ui, t: &str) {
    ui.label(RichText::new(t).font(bold(11.5)).color(MUTED()));
}

impl App {
    // ------------------------------------------------------------ Startup

    pub fn startup_page(&mut self, ui: &mut Ui) {
        if self.startup.is_none() && !self.startup_loading {
            self.load_startup();
        }
        let ctx = ui.ctx().clone();
        let all = self.startup.clone().unwrap_or_default();
        let enabled = all.iter().filter(|i| i.enabled).count();
        header(ui, "Startup apps", &format!("{} of {} start with Windows", enabled, all.len()), |ui| {
            if button(ui, Btn::Ghost, I::ARROW_CLOCKWISE, "Refresh", !self.startup_loading).clicked() {
                self.load_startup();
            }
        });
        if self.startup.is_none() {
            spinner(ui, 18.0, ACCENT_HI());
            return;
        }
        ui.horizontal(|ui| {
            field(ui, egui::Id::unique("startup-search"), I::MAGNIFYING_GLASS, &mut self.startup_search, "Search startup apps…", 300.0);
            ui.add_space(8.0);
            ui.label(RichText::new("Sort").color(FAINT()));
            let (mut by, mut desc) = self.cfg.startup_sort;
            let mut changed = segmented(ui, &mut by, &["Name", "Enabled first", "Publisher", "Location", "Impact"]);
            let dir_tip = if desc { "Descending (click for ascending)" } else { "Ascending (click for descending)" };
            if icon_button(ui, if desc { I::ARROW_DOWN } else { I::ARROW_UP }, dir_tip, false).clicked() {
                desc = !desc;
                changed = true;
            }
            if changed {
                self.cfg.startup_sort = (by, desc);
                self.cfg.save();
            }
        });
        ui.add_space(8.0);
        if self.boot_rec.0.is_none_or(|t| t.elapsed().as_secs() >= 10) {
            self.boot_rec = (Some(std::time::Instant::now()), std::sync::Arc::new(crate::boot::load()));
        }
        let boot = self.boot_rec.1.clone();
        self.boot_timeline(ui, &boot, &all);
        ui.add_space(10.0);
        let q = self.startup_search.trim().to_lowercase();
        let display = |it: &crate::startup::StartupItem| it.description.clone().filter(|d| !d.is_empty()).unwrap_or_else(|| it.name.clone()).to_lowercase();
        let mut list: Vec<crate::startup::StartupItem> = all
            .into_iter()
            .filter(|it| q.is_empty() || display(it).contains(&q) || it.name.to_lowercase().contains(&q) || it.command.to_lowercase().contains(&q) || it.publisher.as_deref().unwrap_or("").to_lowercase().contains(&q))
            .collect();
        let (by, desc) = self.cfg.startup_sort;
        list.sort_by_cached_key(|it| {
            let name = display(it);
            let pre = match by {
                1 => (!it.enabled as u8).to_string(),
                2 => it.publisher.clone().unwrap_or_else(|| "~".into()).to_lowercase(),
                3 => it.source.label().to_lowercase(),
                // Impact: High first, then by CPU time; unmeasured last.
                4 => match boot.impact_for(&it.target) {
                    Some(a) => format!("{}{:012}", 3 - a.level() as u8, u64::MAX / 2 - a.cpu_ms.min(u64::MAX / 4)),
                    None => "9".into(),
                },
                _ => String::new(),
            };
            (pre, name)
        });
        if desc {
            list.reverse();
        }
        let mut toggle: Option<(crate::startup::StartupItem, bool)> = None;
        let mut remove: Option<crate::startup::StartupItem> = None;
        let mut copy: Option<String> = None;
        table_frame().show(ui, |ui| {
            TableBuilder::new(ui)
                .id_salt("startup")
                .striped(false)
                .sense(Sense::click())
                .cell_layout(Layout::left_to_right(Align::Center))
                .auto_shrink([false, false])
                .column(Column::exact(50.0))
                .column(Column::initial(280.0).at_least(160.0).clip(true))
                .column(Column::initial(150.0).at_least(110.0).clip(true))
                .column(Column::remainder().at_least(200.0).clip(true))
                .column(Column::initial(170.0).clip(true))
                .column(Column::exact(44.0))
                .header(28.0, |mut h| {
                    for t in ["", "App", "Startup impact", "Command", "Location", ""] {
                        h.col(|ui| th(ui, t));
                    }
                })
                .body(|body| {
                    body.rows(48.0, list.len(), |mut row| {
                        let it = &list[row.index()];
                        row.col(|ui| {
                            let mut on = it.enabled;
                            if toggle_widget(ui, &mut on).changed() {
                                toggle = Some((it.clone(), on));
                            }
                        });
                        row.col(|ui| {
                            let tex = self.tex(&ctx, &it.target);
                            let (r, _) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::hover());
                            paint_icon(ui, r, tex.as_ref(), I::ROCKET_LAUNCH);
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 1.0;
                                let name = it.description.clone().filter(|d| !d.is_empty()).unwrap_or_else(|| it.name.clone());
                                ui.horizontal(|ui| {
                                    ui.add(egui::Label::new(RichText::new(name).font(bold(13.0)).color(if it.enabled { TEXT() } else { MUTED() })).truncate());
                                    if it.missing {
                                        pill(ui, "File missing", WARN());
                                    }
                                    if it.name == "Sentinel" {
                                        pill(ui, "This app", ACCENT_HI());
                                    }
                                });
                                ui.add(egui::Label::new(RichText::new(it.publisher.clone().unwrap_or_else(|| it.name.clone())).size(11.5).color(FAINT())).truncate());
                            });
                        });
                        row.col(|ui| match boot.impact_for(&it.target) {
                            Some(a) => {
                                let c = match a.level() {
                                    crate::boot::Impact::High => BAD(),
                                    crate::boot::Impact::Medium => WARN(),
                                    crate::boot::Impact::Low => GOOD(),
                                };
                                let r = pill(ui, a.label(), c);
                                ui.label(RichText::new(format!("{:.1} s", a.cpu_ms as f64 / 1000.0)).size(11.5).color(FAINT()));
                                r.on_hover_text(format!(
                                    "In its first 2 minutes after the last {}:\nCPU time {:.1} s  ·  disk {}\nStarted {:.0} s after {}{}",
                                    if boot.from_sign_in { "sign-in" } else { "boot" },
                                    a.cpu_ms as f64 / 1000.0,
                                    fmt::bytes(a.io_bytes),
                                    a.start_secs,
                                    if boot.from_sign_in { "sign-in" } else { "power-on" },
                                    if a.exact { "" } else { "\n(Sentinel started late, so this may read high)" }
                                ));
                            }
                            None => {
                                ui.label(RichText::new(if it.enabled { "Not measured" } else { "Disabled" }).size(12.0).color(FAINT())).on_hover_text("Measured during the first minutes after you sign in. Didn't run last time.");
                            }
                        });
                        row.col(|ui| {
                            ui.add(egui::Label::new(RichText::new(&it.command).size(11.5).color(MUTED()).monospace()).truncate()).on_hover_text(&it.command);
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(it.source.label()).size(12.0).color(MUTED()));
                        });
                        row.col(|ui| {
                            let r = icon_button(ui, I::DOTS_THREE_VERTICAL, "", false);
                            egui::Popup::menu(&r).show(|ui| {
                                ui.set_min_width(200.0);
                                if !it.missing && ui.button(format!("{}  Open file location", I::FOLDER_OPEN)).clicked() {
                                    win::reveal_in_explorer(&it.target);
                                    ui.close();
                                }
                                if ui.button(format!("{}  Copy command", I::COPY)).clicked() {
                                    copy = Some(it.command.clone());
                                    ui.close();
                                }
                                if ui.button(RichText::new(format!("{}  Delete entry", I::TRASH)).color(BAD())).clicked() {
                                    remove = Some(it.clone());
                                    ui.close();
                                }
                            });
                        });
                    });
                });
        });
        if let Some(t) = copy {
            self.copy(&ctx, t);
        }
        if let Some((it, on)) = toggle {
            // Optimistic: flip the switch now, the refresh confirms it.
            if let Some(l) = &mut self.startup {
                if let Some(x) = l.iter_mut().find(|x| x.name == it.name && x.source == it.source) {
                    x.enabled = on;
                }
            }
            let tx = self.tx.clone();
            std::thread::spawn(move || {
                let r = crate::startup::set_enabled(&it, on);
                let _ = tx.send(match r {
                    Ok(()) => Msg::Toast(ToastKind::Good, format!("{} {} at startup", it.name, if on { "will run" } else { "won't run" }), None),
                    Err(e) => Msg::Toast(ToastKind::Bad, e, None),
                });
                let _ = tx.send(Msg::RefreshStartup);
            });
        }
        if let Some(it) = remove {
            let tx = self.tx.clone();
            std::thread::spawn(move || {
                let r = crate::startup::remove(&it);
                let _ = tx.send(match r {
                    Ok(()) => Msg::Toast(ToastKind::Good, format!("Removed {} from startup", it.name), None),
                    Err(e) => Msg::Toast(ToastKind::Bad, e, None),
                });
                let _ = tx.send(Msg::RefreshStartup);
            });
        }
    }

    // ------------------------------------------------------------ Services

    pub fn services_page(&mut self, ui: &mut Ui) {
        if self.services.is_none() && !self.services_loading {
            self.load_services();
        }
        let list = self.services.clone().unwrap_or_default();
        let running = list.iter().filter(|s| s.running()).count();
        header(ui, "Services", &format!("{} running of {}", running, list.len()), |ui| {
            if button(ui, Btn::Ghost, I::ARROW_CLOCKWISE, "Refresh", !self.services_loading).clicked() {
                self.load_services();
            }
            if button(ui, Btn::Subtle, I::ARROW_SQUARE_OUT, "services.msc", true).clicked() {
                let _ = win::shell_open("services.msc", "", "");
            }
        });
        if self.services.is_none() {
            spinner(ui, 18.0, ACCENT_HI());
            return;
        }
        ui.horizontal(|ui| {
            field(ui, egui::Id::unique("svc-search"), I::MAGNIFYING_GLASS, &mut self.svc_search, "Search services…", 320.0);
            let mut f = self.svc_filter;
            if segmented(ui, &mut f, &["All", "Running", "Stopped", "Automatic", "Disabled"]) {
                self.svc_filter = f;
            }
        });
        ui.add_space(8.0);
        let q = self.svc_search.trim().to_lowercase();
        let rows: Vec<&crate::services::Service> = list
            .iter()
            .filter(|s| q.is_empty() || s.name.to_lowercase().contains(&q) || s.display.to_lowercase().contains(&q) || s.pid.to_string() == q)
            .filter(|s| match self.svc_filter {
                1 => s.running(),
                2 => !s.running(),
                3 => s.start_type == windows_sys::Win32::System::Services::SERVICE_AUTO_START,
                4 => s.start_type == windows_sys::Win32::System::Services::SERVICE_DISABLED,
                _ => true,
            })
            .collect();
        // Description strip for the selected service.
        if let Some(sel) = self.svc_selected.clone() {
            if let Some(s) = list.iter().find(|s| s.name == sel) {
                if !self.svc_desc.contains_key(&sel) {
                    self.svc_desc.insert(sel.clone(), String::new());
                    let tx = self.tx.clone();
                    let n = sel.clone();
                    std::thread::spawn(move || {
                        let d = crate::services::description(&n);
                        let _ = tx.send(Msg::ServiceDesc(n, d));
                    });
                }
                card().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&s.display).font(bold(14.0)));
                        ui.label(RichText::new(&s.name).color(FAINT()));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.add(egui::Label::new(RichText::new(I::X).color(FAINT())).sense(Sense::click())).clicked() {
                                self.svc_selected = None;
                            }
                        });
                    });
                    let d = self.svc_desc.get(&sel).cloned().unwrap_or_default();
                    if !d.is_empty() {
                        ui.label(RichText::new(d).size(12.5).color(MUTED()));
                    }
                    if !s.binary.is_empty() {
                        ui.add(egui::Label::new(RichText::new(&s.binary).monospace().size(11.5).color(FAINT())).wrap());
                    }
                    if !s.account.is_empty() {
                        ui.label(RichText::new(format!("Runs as {}", s.account)).size(12.0).color(FAINT()));
                    }
                });
                ui.add_space(8.0);
            }
        }
        let mut action: Option<(String, String, ServiceAction)> = None;
        let mut goto_pid: Option<u32> = None;
        table_frame().show(ui, |ui| {
            TableBuilder::new(ui)
                .id_salt("services")
                .striped(false)
                .resizable(true)
                .sense(Sense::click())
                .cell_layout(Layout::left_to_right(Align::Center))
                .auto_shrink([false, false])
                .column(Column::remainder().at_least(240.0).clip(true))
                .column(Column::initial(170.0).clip(true))
                .column(Column::initial(96.0))
                .column(Column::initial(150.0))
                .column(Column::initial(64.0))
                .column(Column::initial(110.0))
                .header(28.0, |mut h| {
                    for t in ["Service", "Name", "Status", "Startup type", "PID", ""] {
                        h.col(|ui| th(ui, t));
                    }
                })
                .body(|body| {
                    body.rows(32.0, rows.len(), |mut row| {
                        let s = rows[row.index()];
                        row.set_selected(self.svc_selected.as_deref() == Some(s.name.as_str()));
                        row.col(|ui| {
                            ui.label(RichText::new("●").size(10.0).color(if s.running() { GOOD() } else { FAINT() }));
                            ui.add(egui::Label::new(&s.display).truncate());
                        });
                        row.col(|ui| {
                            ui.add(egui::Label::new(RichText::new(&s.name).size(12.0).color(MUTED())).truncate());
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(s.state_label()).size(12.0).color(if s.running() { GOOD() } else { MUTED() }));
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(crate::services::start_type_label(s.start_type, s.delayed)).size(12.0).color(MUTED()));
                        });
                        row.col(|ui| {
                            if s.pid != 0 && ui.link(RichText::new(s.pid.to_string()).size(12.0)).clicked() {
                                goto_pid = Some(s.pid);
                            }
                        });
                        row.col(|ui| {
                            if s.running() {
                                if icon_button(ui, I::STOP, "Stop", false).clicked() {
                                    action = Some((s.name.clone(), s.display.clone(), ServiceAction::Stop));
                                }
                                if icon_button(ui, I::ARROWS_CLOCKWISE, "Restart", false).clicked() {
                                    action = Some((s.name.clone(), s.display.clone(), ServiceAction::Restart));
                                }
                            } else if s.start_type != windows_sys::Win32::System::Services::SERVICE_DISABLED && icon_button(ui, I::PLAY, "Start", false).clicked() {
                                action = Some((s.name.clone(), s.display.clone(), ServiceAction::Start));
                            }
                        });
                        let resp = row.response();
                        if resp.clicked() {
                            self.svc_selected = Some(s.name.clone());
                        }
                        resp.context_menu(|ui| {
                            ui.set_min_width(200.0);
                            if !s.running() && ui.button(format!("{}  Start", I::PLAY)).clicked() {
                                action = Some((s.name.clone(), s.display.clone(), ServiceAction::Start));
                                ui.close();
                            }
                            if s.running() && ui.button(format!("{}  Stop", I::STOP)).clicked() {
                                action = Some((s.name.clone(), s.display.clone(), ServiceAction::Stop));
                                ui.close();
                            }
                            if s.running() && ui.button(format!("{}  Restart", I::ARROWS_CLOCKWISE)).clicked() {
                                action = Some((s.name.clone(), s.display.clone(), ServiceAction::Restart));
                                ui.close();
                            }
                            ui.menu_button(format!("{}  Startup type", I::ROCKET_LAUNCH), |ui| {
                                use windows_sys::Win32::System::Services::*;
                                for (label, t) in [("Automatic", SERVICE_AUTO_START), ("Automatic (delayed)", 0x100), ("Manual", SERVICE_DEMAND_START), ("Disabled", SERVICE_DISABLED)] {
                                    if ui.button(label).clicked() {
                                        action = Some((s.name.clone(), s.display.clone(), ServiceAction::StartType(t)));
                                        ui.close();
                                    }
                                }
                            });
                        });
                    });
                });
        });
        if let Some(pid) = goto_pid {
            if let Some(p) = self.snap.procs.iter().find(|p| p.pid == pid) {
                self.selected = Some(p.key());
                self.sel_group = None;
                self.page = crate::app::Page::Processes;
                self.details_open = true;
                self.scroll_to_sel = true;
                self.cfg.view = crate::config::ViewMode::Flat;
            }
        }
        if let Some((name, display, act)) = action {
            let tx = self.tx.clone();
            let elevated = self.elevated;
            self.toast(ToastKind::Info, format!("{display}: working…"));
            std::thread::spawn(move || {
                let verb = match &act {
                    ServiceAction::Start => "Started",
                    ServiceAction::Stop => "Stopped",
                    ServiceAction::Restart => "Restarted",
                    ServiceAction::StartType(_) => "Changed startup type of",
                };
                let mut r = crate::services::control(&name, &act);
                if let Err(e) = &r {
                    if crate::app::admin_needed(e) && !elevated {
                        r = admin::run(AdminOp::Service { name: name.clone(), action: act.clone() });
                    }
                }
                let _ = tx.send(match r {
                    Ok(()) => Msg::Toast(ToastKind::Good, format!("{verb} {display}"), None),
                    Err(e) => Msg::Toast(ToastKind::Bad, format!("{display}: {e}"), None),
                });
                let _ = tx.send(Msg::RefreshServices);
            });
        }
    }

    // ------------------------------------------------------------ History

    pub fn history_page(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        header(ui, "History", "What ran, when, and how hard it worked, even while Sentinel was hidden", |ui| {
            let mut t = self.hist_tab;
            if segmented(ui, &mut t, &["Processes", "Activity log"]) {
                self.hist_tab = t;
            }
        });
        if self.hist_tab == 1 {
            let log: Vec<crate::sampler::LogEvent> = self.shared.lock().unwrap().log.iter().rev().take(1000).cloned().collect();
            if log.is_empty() {
                empty_state(ui, I::CLOCK_COUNTER_CLOCKWISE, "Nothing yet", "Ended tasks, rule actions and errors show up here.");
                return;
            }
            table_frame().show(ui, |ui| {
                TableBuilder::new(ui)
                    .id_salt("activity")
                    .striped(false)
                    .cell_layout(Layout::left_to_right(Align::Center))
                    .auto_shrink([false, false])
                    .column(Column::exact(140.0))
                    .column(Column::exact(90.0))
                    .column(Column::remainder().clip(true))
                    .body(|body| {
                        body.rows(30.0, log.len(), |mut row| {
                            let e = &log[row.index()];
                            row.col(|ui| {
                                ui.label(RichText::new(fmt::when(e.time)).size(12.0).color(MUTED()));
                            });
                            row.col(|ui| {
                                let (t, c) = match e.kind {
                                    LogKind::Rule => ("Rule", ACCENT_HI()),
                                    LogKind::Action => ("You", GOOD()),
                                    LogKind::Error => ("Error", BAD()),
                                    LogKind::Info => ("Info", MUTED()),
                                };
                                pill(ui, t, c);
                            });
                            row.col(|ui| {
                                ui.add(egui::Label::new(&e.text).truncate());
                            });
                        });
                    });
            });
            return;
        }
        ui.horizontal(|ui| {
            field(ui, egui::Id::unique("hist-search"), I::MAGNIFYING_GLASS, &mut self.hist_search, "Search…", 280.0);
            let mut r = self.hist_range;
            if segmented(ui, &mut r, &["Last hour", "Today", "7 days", "All"]) {
                self.hist_range = r;
            }
            chip(ui, &mut self.hist_spikes, &format!("{} Spikes only", I::FIRE), None).on_hover_text("Peak CPU ≥ 50% or peak memory ≥ 1 GB");
        });
        ui.add_space(8.0);
        let now = win::now_unix();
        let since = match self.hist_range {
            0 => now - 3600,
            1 => now - 86400,
            2 => now - 7 * 86400,
            _ => 0,
        };
        let q = self.hist_search.trim().to_lowercase();
        let mut rows: Vec<HistEntry> = {
            let sh = self.shared.lock().unwrap();
            sh.history
                .iter()
                .filter(|e| e.ended.unwrap_or(now) >= since)
                .filter(|e| q.is_empty() || e.name.to_lowercase().contains(&q) || e.pid.to_string() == q || e.path.as_deref().is_some_and(|p| p.to_lowercase().contains(&q)))
                .filter(|e| !self.hist_spikes || e.peak_cpu >= 50.0 || e.peak_mem >= 1 << 30)
                .cloned()
                .collect()
        };
        let (col, desc) = self.hist_sort;
        rows.sort_by(|a, b| {
            let o = match col {
                1 => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                2 => a.peak_cpu.total_cmp(&b.peak_cpu),
                3 => a.peak_mem.cmp(&b.peak_mem),
                4 => a.cpu_time.cmp(&b.cpu_time),
                5 => a.io_total.cmp(&b.io_total),
                6 => a.net_total.cmp(&b.net_total),
                7 => (a.ended.unwrap_or(now) - a.started).cmp(&(b.ended.unwrap_or(now) - b.started)),
                _ => a.started.cmp(&b.started),
            };
            if desc { o.reverse() } else { o }
        });
        ui.label(RichText::new(format!("{} process runs", rows.len())).size(12.0).color(FAINT()));
        let mut sort_click = None;
        table_frame().show(ui, |ui| {
            TableBuilder::new(ui)
                .id_salt("history")
                .striped(false)
                .resizable(true)
                .cell_layout(Layout::left_to_right(Align::Center))
                .auto_shrink([false, false])
                .column(Column::remainder().at_least(200.0).clip(true))
                .column(Column::initial(130.0))
                .column(Column::initial(110.0))
                .column(Column::initial(90.0))
                .column(Column::initial(90.0))
                .column(Column::initial(84.0))
                .column(Column::initial(90.0))
                .column(Column::initial(90.0))
                .header(28.0, |mut h| {
                    for (i, t) in [(1, "Process"), (0, "Started"), (7, "Ran for"), (2, "Peak CPU"), (3, "Peak memory"), (4, "CPU time"), (5, "I/O"), (6, "Network")] {
                        h.col(|ui| {
                            let active = col == i;
                            let label = format!("{t}{}", if active { if desc { I::CARET_DOWN } else { I::CARET_UP } } else { "" });
                            if ui.add(egui::Label::new(RichText::new(label).font(bold(11.5)).color(if active { ACCENT_HI() } else { MUTED() })).sense(Sense::click()).selectable(false)).clicked() {
                                sort_click = Some(i);
                            }
                        });
                    }
                })
                .body(|body| {
                    body.rows(32.0, rows.len(), |mut row| {
                        let e = &rows[row.index()];
                        row.col(|ui| {
                            let tex = e.path.as_deref().and_then(|p| self.tex(&ctx, p));
                            let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                            paint_icon(ui, r, tex.as_ref(), I::APP_WINDOW);
                            ui.add(egui::Label::new(&e.name).truncate());
                            ui.label(RichText::new(e.pid.to_string()).size(11.0).color(FAINT()));
                            if e.ended.is_none() {
                                pill(ui, "Running", GOOD());
                            }
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(fmt::when(e.started)).size(12.0).color(MUTED()));
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(fmt::duration((e.ended.unwrap_or(now) - e.started).max(0) as u64)).size(12.0).color(MUTED()));
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(fmt::pct(e.peak_cpu)).size(12.0).color(if e.peak_cpu >= 50.0 { WARN() } else { TEXT() }));
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(fmt::bytes(e.peak_mem)).size(12.0).color(if e.peak_mem >= 1 << 30 { WARN() } else { TEXT() }));
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(fmt::cpu_time(e.cpu_time)).size(12.0).color(MUTED()));
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(fmt::bytes(e.io_total)).size(12.0).color(MUTED()));
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(if e.net_total > 0 { fmt::bytes(e.net_total) } else { "—".into() }).size(12.0).color(MUTED()));
                        });
                    });
                });
        });
        if let Some(i) = sort_click {
            self.hist_sort = if self.hist_sort.0 == i { (i, !self.hist_sort.1) } else { (i, i != 1) };
        }
    }

    // ------------------------------------------------------------ Rules

    pub fn rules_page(&mut self, ui: &mut Ui) {
        header(ui, "Rules", "Automatic actions applied to matching processes, even while Sentinel is hidden", |ui| {
            if button(ui, Btn::Primary, I::PLUS, "New rule", true).clicked() {
                let id = self.cfg.next_rule_id;
                self.cfg.next_rule_id += 1;
                self.cfg.rules.push(Rule { id, enabled: true, pattern: String::new(), action: RuleAction::SetPriority(Priority::BelowNormal) });
                self.sync_rules();
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Templates").color(FAINT()));
            let templates: [(&str, &str, RuleAction); 4] = [
                ("Game launchers at low priority", "*launcher*.exe", RuleAction::SetPriority(Priority::BelowNormal)),
                ("End runaway memory hogs (> 8 GB)", "", RuleAction::KillAboveMemoryMb(8192)),
                ("Throttle anything stuck at 90% CPU", "", RuleAction::LowerPriorityAboveCpu { percent: 90.0, seconds: 60 }),
                ("Block an app from running", "", RuleAction::Block),
            ];
            for (label, pat, act) in templates {
                let mut off = false;
                if chip(ui, &mut off, label, None).clicked() {
                    let id = self.cfg.next_rule_id;
                    self.cfg.next_rule_id += 1;
                    self.cfg.rules.push(Rule { id, enabled: true, pattern: pat.to_string(), action: act });
                    self.sync_rules();
                }
            }
        });
        ui.add_space(10.0);
        if self.cfg.rules.is_empty() {
            empty_state(ui, I::LIGHTNING, "No rules yet", "Right-click a process › Add rule, or start from a template above.");
            return;
        }
        let snap = self.snap.clone();
        let mut changed = false;
        let mut delete = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let cores = snap.cores.len().max(1);
            for (ri, r) in self.cfg.rules.iter_mut().enumerate() {
                card().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        changed |= toggle_widget(ui, &mut r.enabled).changed();
                        ui.label(RichText::new("When").color(MUTED()));
                        let resp = field(ui, egui::Id::unique(("rule-pat", r.id)), I::MAGNIFYING_GLASS, &mut r.pattern, "process name, e.g. chrome.exe or *launcher*", 280.0);
                        changed |= resp.changed();
                        ui.label(RichText::new("runs").color(MUTED()));
                        let kinds = ["Set priority", "Limit to CPU cores", "Block from running", "End if memory exceeds", "End if CPU stays above", "Throttle if CPU stays above"];
                        let cur = kinds.iter().position(|k| *k == r.action.label()).unwrap_or(0);
                        let mut sel = cur;
                        egui::ComboBox::from_id_salt(("rule-kind", r.id)).selected_text(kinds[cur]).width(200.0).show_ui(ui, |ui| {
                            for (i, k) in kinds.iter().enumerate() {
                                ui.selectable_value(&mut sel, i, *k);
                            }
                        });
                        if sel != cur {
                            r.action = match sel {
                                0 => RuleAction::SetPriority(Priority::BelowNormal),
                                1 => RuleAction::SetAffinity((1u64 << (cores / 2).max(1)) - 1),
                                2 => RuleAction::Block,
                                3 => RuleAction::KillAboveMemoryMb(4096),
                                4 => RuleAction::KillAboveCpu { percent: 90.0, seconds: 60 },
                                _ => RuleAction::LowerPriorityAboveCpu { percent: 90.0, seconds: 30 },
                            };
                            changed = true;
                        }
                        match &mut r.action {
                            RuleAction::SetPriority(p) => {
                                egui::ComboBox::from_id_salt(("rule-pr", r.id)).selected_text(p.label()).width(130.0).show_ui(ui, |ui| {
                                    for x in Priority::ALL {
                                        if ui.selectable_value(p, x, x.label()).changed() {
                                            changed = true;
                                        }
                                    }
                                });
                            }
                            RuleAction::SetAffinity(mask) => {
                                let mut n = mask.count_ones() as usize;
                                if ui.add(egui::Slider::new(&mut n, 1..=cores).suffix(" cores")).changed() {
                                    *mask = if n >= 64 { u64::MAX } else { (1u64 << n) - 1 };
                                    changed = true;
                                }
                            }
                            RuleAction::KillAboveMemoryMb(mb) => {
                                changed |= ui.add(egui::DragValue::new(mb).range(64..=1_048_576).speed(64).suffix(" MB")).changed();
                            }
                            RuleAction::KillAboveCpu { percent, seconds } | RuleAction::LowerPriorityAboveCpu { percent, seconds } => {
                                changed |= ui.add(egui::DragValue::new(percent).range(1.0..=100.0).suffix("%")).changed();
                                ui.label(RichText::new("for").color(MUTED()));
                                changed |= ui.add(egui::DragValue::new(seconds).range(1..=3600).suffix(" s")).changed();
                            }
                            RuleAction::Block => {}
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if icon_button(ui, I::TRASH, "Delete rule", false).clicked() {
                                delete = Some(ri);
                            }
                        });
                    });
                    let n = if r.pattern.trim().is_empty() { 0 } else { snap.procs.iter().filter(|p| glob_match(r.pattern.trim(), &p.name)).count() };
                    let protected = r.pattern.trim().is_empty() || crate::sampler::critical(r.pattern.trim());
                    ui.label(RichText::new(if r.pattern.trim().is_empty() { "Type a process name to activate this rule.".to_string() } else if protected { "Critical Windows processes are never touched by rules.".to_string() } else { format!("Matches {n} running process{}", if n == 1 { "" } else { "es" }) }).size(12.0).color(FAINT()));
                });
                ui.add_space(8.0);
            }
        });
        if let Some(i) = delete {
            self.cfg.rules.remove(i);
            changed = true;
        }
        if changed {
            self.sync_rules();
        }
    }

    // ------------------------------------------------------------ Settings

    pub fn settings_page(&mut self, ui: &mut Ui) {
        header(ui, "Settings", "", |_| {});
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.set_max_width(860.0);
            section(ui, I::KEYBOARD, "Open with Ctrl+Shift+Esc");
            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                let mut hook = self.cfg.hook_ctrl_shift_esc;
                setting_row(ui, "Ctrl+Shift+Esc opens Sentinel", "Catches the shortcut while Sentinel runs in the background. No admin rights needed.", |ui| {
                    if toggle_widget(ui, &mut hook).changed() {
                        self.cfg.hook_ctrl_shift_esc = hook;
                        integration::HOOK_ENABLED.store(hook, Ordering::Relaxed);
                        self.cfg.save();
                    }
                });
                ui.separator();
                let ifeo = self.ifeo.unwrap_or(false);
                let dev = integration::is_dev_build();
                setting_row(ui, "Replace Windows Task Manager", "Everything that would open Task Manager opens Sentinel instead: Ctrl+Alt+Del › Task Manager, taskbar right-click, and Ctrl+Shift+Esc even over admin windows. Asks for admin once.", |ui| {
                    let mut on = ifeo;
                    let r = ui.add_enabled_ui(!dev && self.ifeo.is_some(), |ui| toggle_widget(ui, &mut on)).inner;
                    if dev {
                        r.on_hover_text("Only available in the installed copy");
                    } else if r.changed() {
                        self.ifeo = None;
                        let tx = self.tx.clone();
                        let exe = integration::exe_path();
                        std::thread::spawn(move || {
                            let res = admin::run(AdminOp::Ifeo { enable: on, exe });
                            let _ = tx.send(match res {
                                Ok(()) => Msg::Toast(ToastKind::Good, if on { "Sentinel now replaces Task Manager".into() } else { "Windows Task Manager restored".into() }, None),
                                Err(e) => Msg::Toast(ToastKind::Bad, e, None),
                            });
                            let _ = tx.send(Msg::IfeoState(admin::ifeo_enabled(), admin::logon_task_enabled()));
                        });
                    }
                });
            });
            ui.add_space(14.0);
            section(ui, I::ROCKET_LAUNCH, "Startup");
            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                let dev = integration::is_dev_build();
                let mut on = self.logon_task.unwrap_or(self.cfg.start_with_windows);
                setting_row(ui, "Start hidden when I sign in", "Sentinel starts in the background with admin rights, no window and no tray icon, so it's instant when you press the shortcut. No UAC prompt.", |ui| {
                    let r = ui.add_enabled_ui(!dev && self.logon_task.is_some(), |ui| toggle_widget(ui, &mut on)).inner;
                    if dev {
                        r.on_hover_text("Only available in the installed copy");
                    } else if r.changed() {
                        self.cfg.start_with_windows = on;
                        self.cfg.save();
                        self.logon_task = None;
                        let tx = self.tx.clone();
                        let exe = integration::exe_path();
                        std::thread::spawn(move || {
                            let res = admin::run(AdminOp::LogonTask { enable: on, exe });
                            if let Err(e) = res {
                                let _ = tx.send(Msg::Toast(ToastKind::Bad, e, None));
                            }
                            let _ = tx.send(Msg::IfeoState(admin::ifeo_enabled(), admin::logon_task_enabled()));
                        });
                    }
                });
                ui.separator();
                let mut v = self.cfg.always_on_top;
                setting_row(ui, "Always on top", "Keep Sentinel above other windows. Also in the sidebar.", |ui| {
                    if toggle_widget(ui, &mut v).changed() {
                        let ctx = ui.ctx().clone();
                        self.set_on_top(&ctx, v);
                    }
                });
            });
            ui.add_space(14.0);
            section(ui, I::PALETTE, "Appearance");
            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                let ctx = ui.ctx().clone();
                setting_row(ui, "Theme", "Dark or light. Graph and status colours adjust to stay readable.", |ui| {
                    let mut i = self.cfg.light_theme as usize;
                    if segmented(ui, &mut i, &["Dark", "Light"]) {
                        self.cfg.light_theme = i == 1;
                        self.cfg.save();
                        crate::theme::apply_theme(&ctx, &self.cfg);
                    }
                });
                ui.separator();
                setting_row(ui, "Accent colour", "Used for selection, buttons and highlights.", |ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        for (i, (name, c)) in crate::theme::ACCENTS.iter().enumerate().rev() {
                            let (r, resp) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
                            let on = self.cfg.accent == i;
                            ui.painter().circle_filled(r.center(), if on { 11.0 } else { 9.0 }, *c);
                            if on {
                                ui.painter().circle_stroke(r.center(), 12.0, Stroke::new(2.0, TEXT()));
                            }
                            if resp.on_hover_text(*name).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                                self.cfg.accent = i;
                                self.cfg.save();
                                crate::theme::apply_theme(&ctx, &self.cfg);
                            }
                        }
                    });
                });
                ui.separator();
                setting_row(ui, "Text size", "Scales the whole window.", |ui| {
                    let opts = [0.9f32, 1.0, 1.1, 1.2, 1.3];
                    let mut i = opts.iter().position(|v| (*v - self.cfg.text_scale).abs() < 0.01).unwrap_or(1);
                    if segmented(ui, &mut i, &["90%", "100%", "110%", "120%", "130%"]) {
                        self.cfg.text_scale = opts[i];
                        self.cfg.save();
                        crate::theme::apply_theme(&ctx, &self.cfg);
                    }
                });
                ui.separator();
                let mut v = self.cfg.compact_rows;
                setting_row(ui, "Compact rows", "Fit more processes on screen.", |ui| {
                    if toggle_widget(ui, &mut v).changed() {
                        self.cfg.compact_rows = v;
                        self.cfg.save();
                    }
                });
                ui.separator();
                let mut v = self.cfg.overlay;
                setting_row(ui, "Mini overlay", "A small always-on-top readout of CPU, memory, GPU and network in a screen corner, even while Sentinel is hidden. Click it to open Sentinel.", |ui| {
                    if toggle_widget(ui, &mut v).changed() {
                        self.cfg.overlay = v;
                        self.cfg.save();
                    }
                });
                if self.cfg.overlay {
                    ui.separator();
                    setting_row(ui, "Overlay position", "Which corner of your main screen.", |ui| {
                        let mut i = self.cfg.overlay_corner.min(3);
                        if segmented(ui, &mut i, &["Top right", "Top left", "Bottom right", "Bottom left"]) {
                            self.cfg.overlay_corner = i;
                            self.cfg.save();
                        }
                    });
                }
            });
            ui.add_space(14.0);
            section(ui, I::FILE_TEXT, "System report");
            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                setting_row(ui, "Export system report", "Saves a Markdown file with your hardware, Windows build, top apps, startup apps (with boot impact), services and installed software. Handy for troubleshooting.", |ui| {
                    if button(ui, Btn::Ghost, I::EXPORT, "Export…", true).clicked() {
                        self.export_report();
                    }
                });
            });
            ui.add_space(14.0);
            section(ui, I::SLIDERS_HORIZONTAL, "Behavior");
            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                setting_row(ui, "Update speed", "How often numbers refresh. Faster uses a little more CPU.", |ui| {
                    let opts = [500u64, 1000, 2000, 5000];
                    let mut i = opts.iter().position(|v| *v == self.cfg.refresh_ms).unwrap_or(1);
                    if segmented(ui, &mut i, &["0.5 s", "1 s", "2 s", "5 s"]) {
                        self.cfg.refresh_ms = opts[i];
                        self.ctl.interval_ms.store(opts[i], Ordering::Relaxed);
                        self.cfg.save();
                    }
                });
                ui.separator();
                let mut v = self.cfg.hide_on_focus_loss;
                setting_row(ui, "Hide when I click away", "Like a quick-look panel: Sentinel hides itself as soon as another window gets focus.", |ui| {
                    if toggle_widget(ui, &mut v).changed() {
                        self.cfg.hide_on_focus_loss = v;
                        self.cfg.save();
                    }
                });
                ui.separator();
                let mut v = self.cfg.confirm_end_task;
                setting_row(ui, "Confirm before ending a task", "Critical system processes and multi-process kills always ask.", |ui| {
                    if toggle_widget(ui, &mut v).changed() {
                        self.cfg.confirm_end_task = v;
                        self.cfg.save();
                    }
                });
                ui.separator();
                let mut v = self.cfg.show_windows_group;
                setting_row(ui, "Separate Windows processes", "In grouped view, keep Windows' own processes in their own section.", |ui| {
                    if toggle_widget(ui, &mut v).changed() {
                        self.cfg.show_windows_group = v;
                        self.cfg.save();
                    }
                });
                ui.separator();
                let mut v = self.cfg.smooth_graphs;
                setting_row(ui, "Smooth graph scrolling", "Graphs glide instead of stepping once per update.", |ui| {
                    if toggle_widget(ui, &mut v).changed() {
                        self.cfg.smooth_graphs = v;
                        self.cfg.save();
                    }
                });
                ui.separator();
                setting_row(ui, "Keep history for", "Process history and the activity log.", |ui| {
                    let opts = [1u32, 7, 30];
                    let mut i = opts.iter().position(|v| *v == self.cfg.history_days).unwrap_or(1);
                    if segmented(ui, &mut i, &["1 day", "7 days", "30 days"]) {
                        self.cfg.history_days = opts[i];
                        self.cfg.save();
                    }
                });
            });
            ui.add_space(14.0);
            section(ui, I::DOWNLOAD_SIMPLE, "Updates");
            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                let mut au = self.cfg.auto_update;
                setting_row(ui, "Update automatically", "Checks GitHub every few hours and installs new versions silently while the window is hidden.", |ui| {
                    if toggle_widget(ui, &mut au).changed() {
                        self.cfg.auto_update = au;
                        self.auto_update.store(au && !integration::is_dev_build(), Ordering::Relaxed);
                        self.cfg.save();
                    }
                });
                ui.separator();
                let st = self.update.lock().unwrap().clone();
                let status = match &st {
                    updater::State::Idle => format!("Version {}", updater::VERSION),
                    updater::State::Checking => "Checking…".into(),
                    updater::State::UpToDate => format!("Version {} is the latest", updater::VERSION),
                    updater::State::Downloading(v) => format!("Downloading {v}…"),
                    updater::State::Ready { version, .. } => format!("Version {version} is ready to install"),
                    updater::State::Error(e) => format!("Update check failed: {e}"),
                };
                setting_row(ui, &status, &format!("github.com/{}", updater::REPO), |ui| {
                    match st {
                        updater::State::Ready { path, .. } => {
                            if button(ui, Btn::Primary, I::ARROWS_CLOCKWISE, "Restart and update", true).clicked() {
                                self.apply_update(ui.ctx(), &path, true);
                            }
                        }
                        updater::State::Checking | updater::State::Downloading(_) => spinner(ui, 18.0, ACCENT_HI()),
                        _ => {
                            if button(ui, Btn::Ghost, I::ARROW_CLOCKWISE, "Check now", true).clicked() {
                                let s = self.update.clone();
                                std::thread::spawn(move || updater::check(&s));
                            }
                        }
                    }
                });
            });
            ui.add_space(14.0);
            section(ui, I::KEYBOARD, "Keyboard shortcuts");
            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                shortcuts_grid(ui);
            });
            ui.add_space(14.0);
            section(ui, I::INFO, "About");
            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(format!("Sentinel {}  ·  {}", updater::VERSION, if self.elevated { "running as administrator" } else { "running as a standard user" })).font(bold(13.5)));
                ui.label(RichText::new(format!("Opened in {:.0} ms. Settings and history live in {}", self.start_ms, crate::config::data_dir().display())).size(12.0).color(MUTED()));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if button(ui, Btn::Ghost, I::FOLDER_OPEN, "Open data folder", true).clicked() {
                        let _ = win::shell_open(&crate::config::data_dir().to_string_lossy(), "", "");
                    }
                    if button(ui, Btn::Ghost, I::ARROW_SQUARE_OUT, "GitHub", true).clicked() {
                        let _ = win::shell_open(&format!("https://github.com/{}", updater::REPO), "", "");
                    }
                    if button(ui, Btn::Danger, I::POWER, "Quit Sentinel", true).on_hover_text("Ctrl+Q. Ctrl+Shift+Esc won't work until it starts again.").clicked() {
                        self.quit(ui.ctx());
                    }
                });
            });
            ui.add_space(20.0);
        });
    }

    // ------------------------------------------------------------ Modals

    pub fn modals(&mut self, ctx: &egui::Context) {
        let Some(modal) = self.modal.take() else { return };
        let mut keep = true;
        let mut next: Option<Modal> = None;
        let mut modal = modal;
        let frame = egui::Frame::new().fill(CARD()).stroke(Stroke::new(1.0, BORDER())).corner_radius(CornerRadius::same(14)).inner_margin(egui::Margin::same(20)).shadow(egui::Shadow { offset: [0, 16], blur: 48, spread: 0, color: Color32::from_black_alpha(170) });
        let resp = egui::Modal::new(egui::Id::unique("modal")).frame(frame).backdrop_color(Color32::from_black_alpha(140)).show(ctx, |ui| {
            ui.set_width(460.0);
            match &mut modal {
                Modal::ConfirmKill { title, targets, names, critical } => {
                    ui.label(RichText::new(title.as_str()).font(bold(17.0)));
                    ui.add_space(4.0);
                    if *critical {
                        ui.label(RichText::new(format!("{} This includes a critical Windows process. Ending it will crash or restart Windows and you may lose unsaved work.", I::WARNING)).color(BAD()));
                    } else {
                        ui.label(RichText::new("Unsaved work in these processes will be lost.").color(MUTED()));
                    }
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if button(ui, Btn::Danger, I::X_CIRCLE, if targets.len() > 1 { "End all" } else { "End task" }, true).clicked() {
                                let (t, n) = (targets.clone(), names.clone());
                                self.do_kill(t, n);
                                keep = false;
                            }
                            if button(ui, Btn::Ghost, "", "Cancel", true).clicked() {
                                keep = false;
                            }
                        });
                    });
                    if ui.input(|i| i.key_pressed(egui::Key::Enter)) && !*critical {
                        let (t, n) = (targets.clone(), names.clone());
                        self.do_kill(t, n);
                        keep = false;
                    }
                }
                Modal::Affinity { pid, name, mask, sys } => {
                    ui.label(RichText::new(format!("CPU cores for {name}")).font(bold(17.0)));
                    ui.label(RichText::new("Pick which logical processors this process may run on. Resets when it restarts; use a rule to make it stick.").size(12.5).color(MUTED()));
                    ui.add_space(10.0);
                    let n = (64 - sys.leading_zeros()) as usize;
                    egui::Grid::new("aff").num_columns(8).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
                        for i in 0..n {
                            let mut on = *mask & (1 << i) != 0;
                            if ui.checkbox(&mut on, format!("{i}")).changed() {
                                if on {
                                    *mask |= 1 << i;
                                } else {
                                    *mask &= !(1 << i);
                                }
                            }
                            if i % 8 == 7 {
                                ui.end_row();
                            }
                        }
                    });
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if button(ui, Btn::Subtle, "", "All", true).clicked() {
                            *mask = *sys;
                        }
                        if button(ui, Btn::Subtle, "", "Half", true).clicked() {
                            *mask = (1u64 << (n / 2).max(1)) - 1;
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if button(ui, Btn::Primary, I::CHECK, "Apply", *mask != 0).clicked() {
                                match crate::actions::set_affinity(*pid, *mask) {
                                    Ok(()) => self.toast(ToastKind::Good, format!("{name} limited to {} cores", mask.count_ones())),
                                    Err(e) => self.toast(ToastKind::Bad, e),
                                }
                                keep = false;
                            }
                            if button(ui, Btn::Ghost, "", "Cancel", true).clicked() {
                                keep = false;
                            }
                        });
                    });
                }
                Modal::RunTask { cmd, admin } => {
                    ui.label(RichText::new("Run new task").font(bold(17.0)));
                    ui.label(RichText::new("Program, folder, document or website.").size(12.5).color(MUTED()));
                    ui.add_space(8.0);
                    let r = field(ui, egui::Id::unique("runtask"), I::TERMINAL_WINDOW, cmd, "e.g. notepad, cmd, regedit, https://…", 420.0);
                    if !r.has_focus() && cmd.is_empty() {
                        r.request_focus();
                    }
                    ui.horizontal(|ui| {
                        toggle_widget(ui, admin);
                        ui.label("Run as administrator");
                    });
                    ui.add_space(10.0);
                    let go = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.horizontal(|ui| {
                        if button(ui, Btn::Ghost, I::FOLDER_OPEN, "Browse…", true).clicked() {
                            if let Some(p) = rfd::FileDialog::new().pick_file() {
                                *cmd = format!("\"{}\"", p.display());
                            }
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if (button(ui, Btn::Primary, I::PLAY, "Run", !cmd.trim().is_empty()).clicked() || go) && !cmd.trim().is_empty() {
                                let (exe, args) = crate::actions::split_args(cmd.trim());
                                match win::shell_open(&exe, &args, if *admin { "runas" } else { "" }) {
                                    Ok(()) => {
                                        self.toast(ToastKind::Good, format!("Started {exe}"));
                                        keep = false;
                                    }
                                    Err(e) => self.toast(ToastKind::Bad, e),
                                }
                            }
                            if button(ui, Btn::Ghost, "", "Cancel", true).clicked() {
                                keep = false;
                            }
                        });
                    });
                }
                Modal::Lockers { path, result } => {
                    ui.label(RichText::new("What's using this file?").font(bold(17.0)));
                    ui.add(egui::Label::new(RichText::new(path.as_str()).size(12.0).color(MUTED())).wrap());
                    ui.add_space(10.0);
                    match result {
                        None => {
                            ui.horizontal(|ui| {
                                spinner(ui, 18.0, ACCENT_HI());
                                ui.label("Checking…");
                            });
                        }
                        Some(Err(e)) => {
                            ui.label(RichText::new(e.as_str()).color(BAD()));
                        }
                        Some(Ok(list)) if list.is_empty() => {
                            ui.label(RichText::new(format!("{} Nothing has it open. You can delete or move it.", I::CHECK_CIRCLE)).color(GOOD()));
                        }
                        Some(Ok(list)) => {
                            let mut kill_all = false;
                            for l in list.iter() {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(I::LOCK_KEY).color(WARN()));
                                    ui.label(RichText::new(&l.name).font(bold(13.0)));
                                    ui.label(RichText::new(format!("PID {}{}", l.pid, if l.service.is_empty() { String::new() } else { format!(" · service {}", l.service) })).size(12.0).color(FAINT()));
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        if button_sized(ui, Btn::Danger, "", "End", true, 26.0).clicked() {
                                            let (pid, ct, n) = (l.pid, l.create_time, l.name.clone());
                                            next = Some(Modal::ConfirmKill { title: format!("End {n}?"), targets: vec![(pid, ct)], names: vec![n], critical: false });
                                        }
                                    });
                                });
                            }
                            ui.add_space(8.0);
                            if list.len() > 1 && button(ui, Btn::Danger, I::X_CIRCLE, &format!("End all {}", list.len()), true).clicked() {
                                kill_all = true;
                            }
                            if kill_all {
                                let t: Vec<(u32, i64)> = list.iter().map(|l| (l.pid, l.create_time)).collect();
                                let n: Vec<String> = list.iter().map(|l| l.name.clone()).collect();
                                next = Some(Modal::ConfirmKill { title: format!("End {} processes?", t.len()), targets: t, names: n, critical: false });
                            }
                        }
                    }
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Tip: drop any file on the window to check it.").size(12.0).color(FAINT()));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if button(ui, Btn::Ghost, "", "Close", true).clicked() {
                                keep = false;
                            }
                        });
                    });
                }
                Modal::RemoveApps(list) => {
                    ui.label(RichText::new(if list.len() == 1 { format!("Remove \"{}\" from the list?", list[0].name) } else { format!("Remove {} entries?", list.len()) }).font(bold(17.0)));
                    ui.label(RichText::new("This deletes only the leftover registry entry so the app stops showing up as installed. Files are not touched. A .reg backup is saved first so it can be restored by double-clicking it.").size(12.5).color(MUTED()));
                    ui.add_space(6.0);
                    egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                        for a in list.iter() {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("•").color(FAINT()));
                                ui.label(&a.name);
                                if a.health == crate::apps::Health::Ok {
                                    pill(ui, "Still installed", WARN());
                                }
                            });
                        }
                    });
                    if list.iter().any(|a| a.health == crate::apps::Health::Ok) {
                        ui.label(RichText::new(format!("{} Some of these still look installed. Uninstall them instead unless the uninstaller is broken.", I::WARNING)).size(12.5).color(WARN()));
                    }
                    if list.iter().any(|a| a.scope != crate::apps::Scope::User) && !self.elevated {
                        ui.label(RichText::new(format!("{} Windows will ask for admin permission.", I::SHIELD_WARNING)).size(12.5).color(FAINT()));
                    }
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if button(ui, Btn::Danger, I::TRASH, "Remove", true).clicked() {
                                let l = list.clone();
                                self.remove_entries(l);
                                keep = false;
                            }
                            if button(ui, Btn::Ghost, "", "Cancel", true).clicked() {
                                keep = false;
                            }
                        });
                    });
                }
                Modal::Leftovers { app, items } => {
                    ui.label(RichText::new(format!("Leftovers from {app}")).font(bold(17.0)));
                    ui.label(RichText::new("These folders still exist. Untick anything you want to keep. Selected folders go to the Recycle Bin, so you can restore them.").size(12.5).color(MUTED()));
                    ui.add_space(8.0);
                    for (p, size, on) in items.iter_mut() {
                        ui.horizontal(|ui| {
                            ui.checkbox(on, "");
                            ui.add(egui::Label::new(RichText::new(p.as_str()).size(12.5)).truncate());
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.label(RichText::new(fmt::bytes(*size)).size(12.0).color(FAINT()));
                            });
                        });
                    }
                    ui.add_space(12.0);
                    let sel: Vec<String> = items.iter().filter(|i| i.2).map(|i| i.0.clone()).collect();
                    ui.horizontal(|ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if button(ui, Btn::Danger, I::RECYCLE, &format!("Recycle {}", sel.len()), !sel.is_empty()).clicked() {
                                let tx = self.tx.clone();
                                let elevated = self.elevated;
                                std::thread::spawn(move || {
                                    let mut r = win::recycle(&sel);
                                    if r.is_err() && !elevated {
                                        r = admin::run(AdminOp::Recycle(sel.clone()));
                                    }
                                    let _ = tx.send(match r {
                                        Ok(()) => Msg::Toast(ToastKind::Good, format!("Moved {} folder{} to the Recycle Bin", sel.len(), if sel.len() == 1 { "" } else { "s" }), None),
                                        Err(e) => Msg::Toast(ToastKind::Bad, e, None),
                                    });
                                });
                                keep = false;
                            }
                            if button(ui, Btn::Ghost, "", "Keep all", true).clicked() {
                                keep = false;
                            }
                        });
                    });
                }
                Modal::Shortcuts => {
                    ui.label(RichText::new("Keyboard shortcuts").font(bold(17.0)));
                    ui.add_space(8.0);
                    shortcuts_grid(ui);
                    ui.add_space(10.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if button(ui, Btn::Ghost, "", "Close", true).clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::UninstallRunning { name } => {
                    ui.horizontal(|ui| {
                        spinner(ui, 22.0, ACCENT_HI());
                        ui.label(RichText::new(format!("Uninstalling {name}…")).font(bold(16.0)));
                    });
                    ui.label(RichText::new("Finish the steps in the uninstaller window. Sentinel checks for leftovers when it closes.").size(12.5).color(MUTED()));
                    ui.add_space(10.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if button(ui, Btn::Ghost, "", "Hide", true).clicked() {
                            keep = false;
                        }
                    });
                }
            }
        });
        let closable = !matches!(modal, Modal::UninstallRunning { .. });
        if resp.should_close() && closable {
            keep = false;
        }
        if let Some(n) = next {
            self.modal = Some(n);
        } else if keep && self.modal.is_none() {
            self.modal = Some(modal);
        }
    }
}

fn toggle_widget(ui: &mut Ui, on: &mut bool) -> egui::Response {
    toggle(ui, on)
}

fn section(ui: &mut Ui, icon: &str, title: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(icon).color(ACCENT_HI()).size(15.0));
        ui.label(RichText::new(title).font(bold(14.5)));
    });
    ui.add_space(4.0);
}

impl App {
    /// "Last boot" card: key milestones and when each startup app launched, coloured by impact.
    fn boot_timeline(&mut self, ui: &mut Ui, boot: &crate::boot::BootRecord, items: &[crate::startup::StartupItem]) {
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            if boot.boot_unix == 0 {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(I::TIMER).size(20.0).color(ACCENT_HI()));
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.label(RichText::new("Boot timeline").font(bold(14.5)));
                        ui.label(RichText::new("Measured the next time you sign in: Sentinel watches the first minutes to see how long startup takes and which apps slow it down.").size(12.0).color(MUTED()));
                    });
                });
                return;
            }
            // Startup apps that ran last time, in launch order.
            let mut marks: Vec<(&crate::boot::AppImpact, String)> = items
                .iter()
                .filter_map(|it| boot.impact_for(&it.target).map(|a| (a, it.description.clone().filter(|d| !d.is_empty()).unwrap_or_else(|| it.name.clone()))))
                .collect();
            marks.sort_by(|a, b| a.0.start_secs.total_cmp(&b.0.start_secs));
            marks.dedup_by(|a, b| a.0.path == b.0.path);
            let total = marks.iter().map(|m| m.0.start_secs).fold(boot.settled_secs.unwrap_or(0.0).max(boot.desktop_secs.unwrap_or(0.0)), f32::max).max(10.0) * 1.08;
            ui.horizontal(|ui| {
                ui.label(RichText::new(I::TIMER).size(20.0).color(ACCENT_HI()));
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.label(RichText::new("Last boot").font(bold(14.5)));
                    ui.label(RichText::new(format!("{}  ·  measured from {}", fmt::when(boot.boot_unix), if boot.from_sign_in { "sign-in (Fast Startup)" } else { "power-on" })).size(12.0).color(FAINT()));
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let high = marks.iter().filter(|m| m.0.level() == crate::boot::Impact::High).count();
                    boot_stat(ui, "High impact", &high.to_string(), if high > 0 { BAD() } else { GOOD() });
                    boot_stat(ui, "Startup apps", &marks.len().to_string(), TEXT());
                    boot_stat(ui, "Settled", &boot.settled_secs.map(|s| format!("{s:.0} s")).unwrap_or("…".into()), C_CPU());
                    if !boot.from_sign_in {
                        boot_stat(ui, "Desktop ready", &boot.desktop_secs.map(|s| format!("{s:.0} s")).unwrap_or("?".into()), ACCENT_HI());
                    }
                });
            });
            ui.add_space(8.0);
            // Timeline
            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::hover());
            let x_of = |s: f32| rect.left() + 8.0 + (rect.width() - 16.0) * (s / total).clamp(0.0, 1.0);
            let mid = rect.top() + 18.0;
            let p = ui.painter();
            p.line_segment([egui::pos2(rect.left() + 8.0, mid), egui::pos2(rect.right() - 8.0, mid)], Stroke::new(2.0, BORDER()));
            let step = [5.0f32, 10.0, 15.0, 30.0, 60.0, 120.0].into_iter().find(|s| total / s <= 8.0).unwrap_or(120.0);
            let mut t = 0.0;
            while t <= total {
                let x = x_of(t);
                p.line_segment([egui::pos2(x, mid - 3.0), egui::pos2(x, mid + 3.0)], Stroke::new(1.0, BORDER()));
                p.text(egui::pos2(x, rect.bottom() - 2.0), egui::Align2::CENTER_BOTTOM, format!("{t:.0}s"), egui::FontId::proportional(10.5), FAINT());
                t += step;
            }
            for (label, at, c) in [("Desktop", boot.desktop_secs.filter(|_| !boot.from_sign_in), ACCENT_HI()), ("Settled", boot.settled_secs, C_CPU())] {
                if let Some(s) = at {
                    let x = x_of(s);
                    p.line_segment([egui::pos2(x, mid - 12.0), egui::pos2(x, mid + 12.0)], Stroke::new(2.0, c));
                    p.text(egui::pos2(x + 4.0, mid - 12.0), egui::Align2::LEFT_TOP, label, egui::FontId::proportional(10.5), c);
                }
            }
            for (i, (a, name)) in marks.iter().enumerate() {
                let c = match a.level() {
                    crate::boot::Impact::High => BAD(),
                    crate::boot::Impact::Medium => WARN(),
                    crate::boot::Impact::Low => GOOD(),
                };
                let center = egui::pos2(x_of(a.start_secs), mid);
                let r = if a.level() == crate::boot::Impact::High { 6.0 } else { 4.5 };
                ui.painter().circle(center, r, c, Stroke::new(1.5, CARD()));
                let hit = egui::Rect::from_center_size(center, vec2(14.0, 14.0));
                ui.interact(hit, ui.scope_id().with(("bootmark", i)), Sense::hover()).on_hover_text(format!("{name}\nStarted at {:.0} s  ·  {} impact\nCPU {:.1} s  ·  disk {}", a.start_secs, a.label(), a.cpu_ms as f64 / 1000.0, fmt::bytes(a.io_bytes)));
            }
        });
    }
}

fn boot_stat(ui: &mut Ui, k: &str, v: &str, color: Color32) {
    let (r, _) = ui.allocate_exact_size(vec2(104.0, 36.0), Sense::hover());
    let p = ui.painter();
    p.text(r.left_top(), egui::Align2::LEFT_TOP, k, egui::FontId::proportional(11.0), FAINT());
    p.text(r.left_top() + vec2(0.0, 15.0), egui::Align2::LEFT_TOP, v, bold(15.5), color);
}

fn shortcuts_grid(ui: &mut Ui) {
    let items = [
        ("Ctrl+Shift+Esc", "Open Sentinel from anywhere"),
        ("Ctrl+K", "Command palette: apps, pages, commands"),
        ("Esc", "Clear search, then hide the window"),
        ("Type anywhere", "Search processes"),
        ("↑ ↓  PgUp PgDn", "Move the selection"),
        ("Delete", "End the selected task"),
        ("Shift+Delete", "End the process tree"),
        ("Enter", "Show / hide details"),
        ("Double-click", "Switch to the app's window"),
        ("Ctrl+F", "Focus search"),
        ("Ctrl+1 … 9", "Jump to a page"),
        ("Ctrl+N", "Run new task"),
        ("Ctrl+P", "Pause / resume updates"),
        ("F5", "Refresh now"),
        ("Ctrl+W", "Hide"),
        ("Ctrl+Q", "Quit"),
        ("Drop a file", "See which app has it locked"),
        ("F1", "This list"),
    ];
    egui::Grid::new(ui.scope_id().with("sc")).num_columns(2).spacing(vec2(24.0, 6.0)).show(ui, |ui| {
        for (k, v) in items {
            egui::Frame::new().fill(FIELD()).stroke(Stroke::new(1.0, BORDER())).corner_radius(CornerRadius::same(6)).inner_margin(egui::Margin::symmetric(8, 2)).show(ui, |ui| {
                ui.label(RichText::new(k).font(bold(12.0)));
            });
            ui.label(RichText::new(v).color(MUTED()));
            ui.end_row();
        }
    });
}
