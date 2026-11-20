//! Installed apps: search, uninstall, and clean up entries whose files are already gone.

use crate::app::{App, Modal, Msg, ToastKind, header};
use crate::apps::{self, App as InstalledApp, Health};
use crate::fmt;
use crate::icons as I;
use crate::theme::*;
use egui::{Align, CornerRadius, Layout, RichText, Sense, Stroke, Ui, vec2};
use egui_extras::{Column, TableBuilder};

enum AppAct {
    Uninstall(InstalledApp, bool),
    Remove(Vec<InstalledApp>),
    OpenFolder(String),
    Regedit(InstalledApp),
    Copy(String),
    Leftovers(InstalledApp),
}

impl App {
    pub fn apps_page(&mut self, ui: &mut Ui) {
        if self.apps.is_none() && !self.apps_loading {
            self.load_apps();
        }
        let ctx = ui.ctx().clone();
        let list = self.apps.clone().unwrap_or_default();
        let broken = list.iter().filter(|a| a.health == Health::Broken).count();
        let total_kb: u64 = list.iter().map(|a| a.size_kb).sum();
        let sub = if self.apps.is_some() { format!("{} apps  ·  {} reported size{}", list.len(), fmt::bytes(total_kb * 1024), if broken > 0 { format!("  ·  {broken} broken") } else { String::new() }) } else { "Loading…".into() };
        header(ui, "Installed apps", &sub, |ui| {
            if button(ui, Btn::Ghost, I::ARROW_CLOCKWISE, "Refresh", !self.apps_loading).clicked() {
                self.load_apps();
            }
            if button(ui, Btn::Subtle, I::ARROW_SQUARE_OUT, "Windows settings", true).on_hover_text("Open Settings › Apps (for Microsoft Store apps)").clicked() {
                let _ = crate::win::shell_open("ms-settings:appsfeatures", "", "");
            }
        });
        if self.apps.is_none() {
            ui.horizontal(|ui| {
                spinner(ui, 18.0, ACCENT_HI());
                ui.label(RichText::new("Reading installed programs…").color(MUTED()));
            });
            return;
        }
        if broken > 0 && self.apps_filter != 1 {
            egui::Frame::new().fill(BAD().gamma_multiply(0.12)).stroke(Stroke::new(1.0, BAD().gamma_multiply(0.35))).corner_radius(CornerRadius::same(10)).inner_margin(egui::Margin::symmetric(14, 9)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(I::GHOST).color(BAD()).size(17.0));
                    ui.label(RichText::new(format!("{broken} app{} can't be uninstalled because {} files were already deleted.", if broken == 1 { "" } else { "s" }, if broken == 1 { "its" } else { "their" })).color(TEXT()));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if button_sized(ui, Btn::Primary, I::BROOM, "Review and clean up", true, 28.0).clicked() {
                            self.apps_filter = 1;
                        }
                    });
                });
            });
            ui.add_space(10.0);
        }
        ui.horizontal(|ui| {
            field(ui, egui::Id::unique("apps-search"), I::MAGNIFYING_GLASS, &mut self.apps_search, "Search apps or publishers…", 340.0);
            let mut f = self.apps_filter;
            if segmented(ui, &mut f, &["All", "Broken", "Large", "Recent"]) {
                self.apps_filter = f;
                self.apps_checked.clear();
            }
            ui.label(RichText::new("Sort").color(FAINT()));
            let sorts = ["Name", "Size", "Install date", "Publisher"];
            egui::ComboBox::from_id_salt("apps-sort").selected_text(sorts[self.apps_sort]).width(120.0).show_ui(ui, |ui| {
                for (i, s) in sorts.iter().enumerate() {
                    ui.selectable_value(&mut self.apps_sort, i, *s);
                }
            });
        });
        ui.add_space(8.0);

        let q = self.apps_search.trim().to_lowercase();
        let mut rows: Vec<&InstalledApp> = list
            .iter()
            .filter(|a| q.is_empty() || a.name.to_lowercase().contains(&q) || a.publisher.to_lowercase().contains(&q))
            .filter(|a| match self.apps_filter {
                1 => a.health != Health::Ok,
                2 => a.size_kb >= 500 * 1024,
                3 => {
                    let now = crate::win::now_unix();
                    a.installed_unix() > 0 && a.install_date.len() == 8 && {
                        let (y, m, d): (i64, i64, i64) = (a.install_date[0..4].parse().unwrap_or(0), a.install_date[4..6].parse().unwrap_or(0), a.install_date[6..8].parse().unwrap_or(0));
                        let approx = ((y - 1970) * 365 + (y - 1969) / 4 + [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334][(m.clamp(1, 12) - 1) as usize] + d - 1) * 86400;
                        now - approx < 30 * 86400
                    }
                }
                _ => true,
            })
            .collect();
        match self.apps_sort {
            1 => rows.sort_by(|a, b| b.size_kb.cmp(&a.size_kb)),
            2 => rows.sort_by(|a, b| b.install_date.cmp(&a.install_date)),
            3 => rows.sort_by_key(|a| (a.publisher.to_lowercase(), a.name.to_lowercase())),
            _ => {}
        }
        let mut acts: Vec<AppAct> = Vec::new();
        let bulk = self.apps_filter == 1;
        if bulk {
            ui.horizontal(|ui| {
                let n = rows.iter().filter(|a| self.apps_checked.contains(&a.id())).count();
                let mut all = n == rows.len() && n > 0;
                if ui.checkbox(&mut all, "Select all").changed() {
                    if all {
                        self.apps_checked = rows.iter().map(|a| a.id()).collect();
                    } else {
                        self.apps_checked.clear();
                    }
                }
                ui.label(RichText::new("Broken = the uninstaller is gone. Files missing = the install folder is gone, the uninstaller might still work.").size(12.0).color(FAINT()));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if button(ui, Btn::Danger, I::TRASH, &format!("Remove {n} entr{}", if n == 1 { "y" } else { "ies" }), n > 0).clicked() {
                        acts.push(AppAct::Remove(rows.iter().filter(|a| self.apps_checked.contains(&a.id())).map(|a| (*a).clone()).collect()));
                    }
                });
            });
            ui.add_space(6.0);
        }
        if rows.is_empty() {
            empty_state(ui, if bulk { I::CHECK_CIRCLE } else { I::PACKAGE }, if bulk { "No broken entries" } else { "No apps match" }, if bulk { "Every installed app still has its uninstaller." } else { "Try a different search or filter." });
            return;
        }
        egui::Frame::new().fill(CARD()).stroke(Stroke::new(1.0, BORDER_SOFT())).corner_radius(CornerRadius::same(12)).inner_margin(egui::Margin::same(6)).show(ui, |ui| {
            let mut tb = TableBuilder::new(ui).id_salt("apps").striped(false).sense(Sense::click()).cell_layout(Layout::left_to_right(Align::Center)).auto_shrink([false, false]);
            if bulk {
                tb = tb.column(Column::exact(26.0));
            }
            tb.column(Column::remainder().at_least(260.0).clip(true))
                .column(Column::initial(110.0).clip(true))
                .column(Column::initial(84.0))
                .column(Column::initial(110.0))
                .column(Column::initial(120.0))
                .column(Column::initial(200.0))
                .header(28.0, |mut h| {
                    if bulk {
                        h.col(|_| {});
                    }
                    for t in ["App", "Version", "Size", "Installed", "Status", ""] {
                        h.col(|ui| {
                            ui.label(RichText::new(t).font(bold(11.5)).color(MUTED()));
                        });
                    }
                })
                .body(|body| {
                    body.rows(48.0, rows.len(), |mut row| {
                        let a = rows[row.index()];
                        let id = a.id();
                        if bulk {
                            row.col(|ui| {
                                let mut on = self.apps_checked.contains(&id);
                                if ui.checkbox(&mut on, "").changed() {
                                    if on {
                                        self.apps_checked.insert(id.clone());
                                    } else {
                                        self.apps_checked.remove(&id);
                                    }
                                }
                            });
                        }
                        row.col(|ui| {
                            let spec = if !a.icon.is_empty() { a.icon.clone() } else { a.uninstaller_exe.clone() };
                            let tex = self.tex(&ctx, &spec);
                            let (r, _) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::hover());
                            paint_icon(ui, r, tex.as_ref(), I::PACKAGE);
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 1.0;
                                ui.add(egui::Label::new(RichText::new(&a.name).font(bold(13.0))).truncate());
                                let sub = match a.scope {
                                    apps::Scope::User => format!("{}  ·  just you", a.publisher),
                                    _ => a.publisher.clone(),
                                };
                                ui.add(egui::Label::new(RichText::new(sub).size(11.5).color(FAINT())).truncate());
                            });
                        });
                        row.col(|ui| {
                            ui.add(egui::Label::new(RichText::new(&a.version).size(12.0).color(MUTED())).truncate());
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(if a.size_kb > 0 { fmt::bytes(a.size_kb * 1024) } else { String::new() }).size(12.0).color(MUTED()));
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(fmt::install_date(&a.install_date)).size(12.0).color(MUTED()));
                        });
                        row.col(|ui| match a.health {
                            Health::Broken => {
                                pill(ui, "Broken", BAD()).on_hover_text(format!("Uninstaller missing:\n{}", a.uninstaller_exe));
                            }
                            Health::FilesMissing => {
                                pill(ui, "Files missing", WARN()).on_hover_text(format!("Install folder is gone:\n{}", a.location));
                            }
                            Health::Ok => {}
                        });
                        row.col(|ui| {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let more = icon_button(ui, I::DOTS_THREE_VERTICAL, "More", false);
                                egui::Popup::menu(&more).show(|ui| self.app_menu(ui, a, &mut acts));
                                if a.health == Health::Broken {
                                    if button_sized(ui, Btn::Danger, I::TRASH, "Remove entry", true, 28.0).clicked() {
                                        acts.push(AppAct::Remove(vec![a.clone()]));
                                    }
                                } else if !a.no_remove && button_sized(ui, Btn::Ghost, I::TRASH, "Uninstall", true, 28.0).clicked() {
                                    acts.push(AppAct::Uninstall(a.clone(), false));
                                }
                            });
                        });
                        row.response().context_menu(|ui| self.app_menu(ui, a, &mut acts));
                    });
                });
        });
        for a in acts {
            match a {
                AppAct::Uninstall(app, quiet) => self.start_uninstall(app, quiet),
                AppAct::Remove(list) => self.modal = Some(Modal::RemoveApps(list)),
                AppAct::OpenFolder(p) => {
                    let _ = crate::win::shell_open(&p, "", "");
                }
                AppAct::Regedit(app) => apps::show_in_regedit(&app),
                AppAct::Copy(t) => self.copy(&ctx, t),
                AppAct::Leftovers(app) => {
                    let name = app.name.clone();
                    let tx = self.tx.clone();
                    std::thread::spawn(move || {
                        let left: Vec<(String, u64)> = apps::leftovers(&app).into_iter().map(|p| {
                            let s = apps::dir_size(&p);
                            (p, s)
                        }).collect();
                        let _ = tx.send(Msg::Uninstalled(app, true, left));
                    });
                    self.toast(ToastKind::Info, format!("Looking for {name} leftovers…"));
                }
            }
        }
    }

    fn app_menu(&mut self, ui: &mut Ui, a: &InstalledApp, acts: &mut Vec<AppAct>) {
        ui.set_min_width(220.0);
        if a.health != Health::Broken && !a.no_remove {
            if ui.button(format!("{}  Uninstall", I::TRASH)).clicked() {
                acts.push(AppAct::Uninstall(a.clone(), false));
                ui.close();
            }
            if (!a.quiet_uninstall.is_empty() || a.msi) && ui.button(format!("{}  Uninstall silently", I::LIGHTNING)).clicked() {
                acts.push(AppAct::Uninstall(a.clone(), true));
                ui.close();
            }
        }
        if ui.button(RichText::new(format!("{}  Remove entry from list", I::FILE_X)).color(BAD())).on_hover_text("Deletes only the registry entry (a .reg backup is saved first)").clicked() {
            acts.push(AppAct::Remove(vec![a.clone()]));
            ui.close();
        }
        if ui.button(format!("{}  Find leftover folders", I::BROOM)).clicked() {
            acts.push(AppAct::Leftovers(a.clone()));
            ui.close();
        }
        ui.separator();
        if !a.location.is_empty() && std::path::Path::new(&a.location).exists() && ui.button(format!("{}  Open install folder", I::FOLDER_OPEN)).clicked() {
            acts.push(AppAct::OpenFolder(a.location.clone()));
            ui.close();
        }
        if ui.button(format!("{}  Show in Registry Editor", I::WRENCH)).clicked() {
            acts.push(AppAct::Regedit(a.clone()));
            ui.close();
        }
        if !a.uninstall.is_empty() && ui.button(format!("{}  Copy uninstall command", I::COPY)).clicked() {
            acts.push(AppAct::Copy(a.uninstall.clone()));
            ui.close();
        }
    }

    fn start_uninstall(&mut self, app: InstalledApp, quiet: bool) {
        match apps::uninstall(&app, quiet) {
            Ok(h) => {
                self.modal = Some(Modal::UninstallRunning { name: app.name.clone() });
                let tx = self.tx.clone();
                std::thread::spawn(move || {
                    unsafe { windows_sys::Win32::System::Threading::WaitForSingleObject(h.0, u32::MAX) };
                    drop(h);
                    // Some uninstallers hand off to a child process and exit immediately; give them a moment.
                    std::thread::sleep(std::time::Duration::from_millis(1500));
                    let still = apps::list().iter().any(|a| a.id() == app.id());
                    let left: Vec<(String, u64)> = if still { Vec::new() } else { apps::leftovers(&app).into_iter().map(|p| { let s = apps::dir_size(&p); (p, s) }).collect() };
                    let _ = tx.send(Msg::Uninstalled(app, still, left));
                });
            }
            Err(e) => self.toast(ToastKind::Bad, format!("Couldn't start the uninstaller: {e}")),
        }
    }

    pub fn after_uninstall(&mut self, app: InstalledApp, still: bool, left: Vec<(String, u64)>) {
        if matches!(self.modal, Some(Modal::UninstallRunning { .. })) {
            self.modal = None;
        }
        self.load_apps();
        if !left.is_empty() {
            self.modal = Some(Modal::Leftovers { app: app.name.clone(), items: left.into_iter().map(|(p, s)| (p, s, true)).collect() });
        } else if still {
            self.toast(ToastKind::Info, format!("{} is still installed (the uninstaller was closed or is still finishing)", app.name));
        } else {
            self.toast(ToastKind::Good, format!("Uninstalled {}", app.name));
        }
    }

    pub fn remove_entries(&mut self, list: Vec<InstalledApp>) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let mut ok = 0;
            let mut last_err = None;
            let mut backup = None;
            for a in &list {
                match apps::remove_entry(a) {
                    Ok(b) => {
                        ok += 1;
                        backup = Some(b);
                    }
                    Err(e) => last_err = Some(format!("{}: {e}", a.name)),
                }
            }
            let msg = if ok > 0 {
                let dir = backup.as_deref().and_then(|b| std::path::Path::new(b).parent()).map(|p| p.display().to_string()).unwrap_or_default();
                Msg::Toast(ToastKind::Good, format!("Removed {ok} entr{}. Backups saved in {dir}", if ok == 1 { "y" } else { "ies" }), None)
            } else {
                Msg::Toast(ToastKind::Bad, last_err.clone().unwrap_or_default(), None)
            };
            let _ = tx.send(msg);
            if ok > 0 {
                if let Some(e) = last_err {
                    let _ = tx.send(Msg::Toast(ToastKind::Bad, e, None));
                }
            }
            let _ = tx.send(Msg::RefreshApps);
        });
        self.apps_checked.clear();
    }
}
