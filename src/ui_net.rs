//! Network page: per-app bandwidth, live connections, adapters, latency monitor, firewall blocks.

use crate::admin::AdminOp;
use crate::app::{App, Page, ToastKind, header};
use crate::fmt;
use crate::graph::{self, Opts, Series};
use crate::icons as I;
use crate::net::{Conn, Proto, tcp_state};
use crate::sampler::Proc;
use crate::theme::*;
use egui::{Align, Color32, CornerRadius, Layout, RichText, Sense, Stroke, Ui, vec2};
use egui_extras::{Column, TableBuilder};
use std::collections::{HashMap, VecDeque};

enum NetAct {
    Kill(u32, i64, String),
    Block(String),
    GoTo(u32, i64),
    Copy(String),
    Close(Conn),
    Lookup(String),
}

impl App {
    pub fn network_page(&mut self, ui: &mut Ui) {
        let snap = self.snap.clone();
        let sub = format!("↓ {}   ↑ {}   ·   {} connections", fmt::bits(snap.net_recv), fmt::bits(snap.net_send), snap.conns.iter().filter(|c| c.proto == Proto::Tcp && c.state == 5).count());
        header(ui, "Network", &sub, |ui| {
            let mut t = self.net_tab;
            if segmented(ui, &mut t, &["Overview", "Connections", "Adapters", "Latency", "Blocked apps"]) {
                self.net_tab = t;
            }
        });
        if !snap.per_app_net && self.net_tab <= 1 {
            egui::Frame::new().fill(WARN().gamma_multiply(0.12)).stroke(Stroke::new(1.0, WARN().gamma_multiply(0.35))).corner_radius(CornerRadius::same(10)).inner_margin(egui::Margin::symmetric(14, 9)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(I::SHIELD_WARNING).color(WARN()).size(16.0));
                    ui.label(RichText::new(if self.elevated { "Starting the network monitor…" } else { "Per-app speeds need admin rights. The UAC prompt was declined; restart Sentinel and accept it." }).color(TEXT()));
                });
            });
            ui.add_space(10.0);
        }
        let mut acts = Vec::new();
        match self.net_tab {
            0 => self.net_overview(ui, &snap, &mut acts),
            1 => self.net_connections(ui, &snap, &mut acts),
            2 => self.net_adapters(ui, &snap),
            3 => self.net_latency(ui, &snap),
            _ => self.net_blocked(ui),
        }
        let ctx = ui.ctx().clone();
        for a in acts {
            match a {
                NetAct::Kill(pid, ct, name) => self.kill_targets(vec![(pid, ct)], vec![name.clone()], format!("End {name}?")),
                NetAct::Block(p) => self.toggle_block(p),
                NetAct::GoTo(pid, ct) => {
                    self.page = Page::Processes;
                    self.selected = Some((pid, ct));
                    self.sel_group = None;
                    self.details_open = true;
                    self.scroll_to_sel = true;
                    self.search.clear();
                    self.cfg.view = crate::config::ViewMode::Flat;
                }
                NetAct::Copy(t) => self.copy(&ctx, t),
                NetAct::Close(c) => {
                    if let Some(r) = c.remote {
                        let res = crate::net::close_tcp(c.local, r);
                        self.try_or_admin(&format!("Closed connection to {r}"), res, AdminOp::CloseTcp { local: c.local.to_string(), remote: r.to_string() });
                    }
                }
                NetAct::Lookup(ip) => {
                    let _ = crate::win::shell_open(&format!("https://ipinfo.io/{ip}"), "", "");
                }
            }
        }
    }

    fn net_overview(&mut self, ui: &mut Ui, snap: &crate::sampler::Snapshot, acts: &mut Vec<NetAct>) {
        let offset = self.smooth_offset();
        let (recv, send) = {
            let sh = self.shared.lock().unwrap();
            (sh.sys.net_recv.clone(), sh.sys.net_send.clone())
        };
        let w = ui.available_width();
        let rect = ui.allocate_exact_size(vec2(w, 170.0), Sense::hover()).0;
        graph::graph(ui, rect, &[Series { data: &recv, color: C_NET(), label: "Download" }, Series { data: &send, color: CYAN(), label: "Upload" }], &Opts { max: None, fmt: &|v| fmt::bits(v as f64), offset, grid: true, rounded: true, interval: snap.interval.max(0.25) });
        ui.add_space(12.0);

        // Per-app table
        let mut apps: Vec<&Proc> = snap.procs.iter().filter(|p| p.net_rate() > 0.0 || p.net_total > 0 || p.conns > 0).collect();
        if snap.per_app_net {
            apps.sort_by(|a, b| b.net_rate().total_cmp(&a.net_rate()).then(b.net_total.cmp(&a.net_total)));
        } else {
            apps.sort_by(|a, b| b.conns.cmp(&a.conns));
        }
        label_caps(ui, if snap.per_app_net { "Apps using the network" } else { "Apps with open connections" });
        egui::Frame::new().fill(CARD()).stroke(Stroke::new(1.0, BORDER_SOFT())).corner_radius(CornerRadius::same(12)).inner_margin(egui::Margin::same(6)).show(ui, |ui| {
            let max_rate = apps.iter().map(|p| p.net_rate()).fold(1.0, f64::max);
            TableBuilder::new(ui)
                .id_salt("netapps")
                .striped(false)
                .sense(Sense::click())
                .cell_layout(Layout::left_to_right(Align::Center))
                .auto_shrink([false, false])
                .column(Column::remainder().at_least(220.0).clip(true))
                .column(Column::initial(110.0))
                .column(Column::initial(110.0))
                .column(Column::initial(160.0))
                .column(Column::initial(100.0))
                .column(Column::initial(90.0))
                .header(28.0, |mut h| {
                    for t in ["App", "Download", "Upload", "Activity", "Session total", "Connections"] {
                        h.col(|ui| {
                            ui.label(RichText::new(t).font(bold(11.5)).color(MUTED()));
                        });
                    }
                })
                .body(|body| {
                    body.rows(34.0, apps.len(), |mut row| {
                        let p = apps[row.index()];
                        row.col(|ui| {
                            let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                            self.paint_proc_icon(ui, r, p);
                            let name = self.info.meta(p.key(), p.path.as_ref()).and_then(|m| m.description.clone()).filter(|d| !d.is_empty()).unwrap_or_else(|| p.name.clone());
                            ui.add(egui::Label::new(name).truncate());
                            ui.label(RichText::new(p.pid.to_string()).size(11.0).color(FAINT()));
                            if self.cfg.blocked_apps.iter().any(|b| Some(b.as_str()) == p.path.as_deref()) {
                                pill(ui, "Blocked", ORANGE());
                            }
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(if snap.per_app_net { fmt::rate(p.net_recv) } else { "—".into() }).color(C_NET()));
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(if snap.per_app_net { fmt::rate(p.net_send) } else { "—".into() }).color(CYAN()));
                        });
                        row.col(|ui| {
                            bar(ui, (p.net_rate() / max_rate) as f32, C_NET(), 140.0, 6.0);
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(if snap.per_app_net { fmt::bytes(p.net_total) } else { "—".into() }).color(MUTED()));
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(p.conns.to_string()).color(MUTED()));
                        });
                        let resp = row.response();
                        if resp.double_clicked() {
                            acts.push(NetAct::GoTo(p.pid, p.create_time));
                        }
                        resp.context_menu(|ui| {
                            if ui.button(format!("{}  Show in Processes", I::LIST_BULLETS)).clicked() {
                                acts.push(NetAct::GoTo(p.pid, p.create_time));
                                ui.close();
                            }
                            if ui.button(format!("{}  Show connections", I::PLUGS_CONNECTED)).clicked() {
                                self.net_tab = 1;
                                self.net_search = p.pid.to_string();
                                ui.close();
                            }
                            if let Some(path) = p.path.as_deref() {
                                let blocked = self.cfg.blocked_apps.iter().any(|b| b.eq_ignore_ascii_case(path));
                                if ui.button(format!("{}  {}", I::PROHIBIT, if blocked { "Allow internet access" } else { "Block internet access" })).clicked() {
                                    acts.push(NetAct::Block(path.to_string()));
                                    ui.close();
                                }
                            }
                            if ui.button(RichText::new(format!("{}  End task", I::X_CIRCLE)).color(BAD())).clicked() {
                                acts.push(NetAct::Kill(p.pid, p.create_time, p.name.clone()));
                                ui.close();
                            }
                        });
                    });
                });
        });
    }

    fn net_connections(&mut self, ui: &mut Ui, snap: &crate::sampler::Snapshot, acts: &mut Vec<NetAct>) {
        ui.horizontal(|ui| {
            field(ui, egui::Id::unique("net-search"), I::MAGNIFYING_GLASS, &mut self.net_search, "Filter by app, PID, IP, host or port (e.g. 443)…", 420.0);
            chip(ui, &mut self.net_hide_listen, "Hide listening", None);
            chip(ui, &mut self.net_only_active, "Only active", None);
        });
        ui.add_space(8.0);
        let by_pid: HashMap<u32, &Proc> = snap.procs.iter().map(|p| (p.pid, p)).collect();
        let q = self.net_search.trim().to_lowercase();
        let mut conns: Vec<&Conn> = snap
            .conns
            .iter()
            .filter(|c| {
                if self.net_hide_listen && (c.state == 2 || (c.proto == Proto::Udp && c.send_rate + c.recv_rate == 0.0)) {
                    return false;
                }
                if self.net_only_active && c.send_rate + c.recv_rate == 0.0 && c.state != 5 {
                    return false;
                }
                if q.is_empty() {
                    return true;
                }
                let name = by_pid.get(&c.pid).map(|p| p.name.to_lowercase()).unwrap_or_default();
                let remote = c.remote.map(|r| r.to_string()).unwrap_or_default();
                let host = c.remote.and_then(|r| self.rdns.get(r.ip())).unwrap_or_default().to_lowercase();
                c.pid.to_string() == q || name.contains(&q) || remote.contains(&q) || host.contains(&q) || c.local.port().to_string() == q || c.remote.is_some_and(|r| r.port().to_string() == q) || c.local.to_string().contains(&q)
            })
            .collect();
        conns.sort_by(|a, b| (b.send_rate + b.recv_rate).total_cmp(&(a.send_rate + a.recv_rate)).then((a.state != 5).cmp(&(b.state != 5))).then(a.pid.cmp(&b.pid)));
        ui.label(RichText::new(format!("{} of {} sockets", conns.len(), snap.conns.len())).size(12.0).color(FAINT()));
        egui::Frame::new().fill(CARD()).stroke(Stroke::new(1.0, BORDER_SOFT())).corner_radius(CornerRadius::same(12)).inner_margin(egui::Margin::same(6)).show(ui, |ui| {
            TableBuilder::new(ui)
                .id_salt("conns")
                .striped(false)
                .resizable(true)
                .sense(Sense::click())
                .cell_layout(Layout::left_to_right(Align::Center))
                .auto_shrink([false, false])
                .column(Column::initial(200.0).at_least(120.0).clip(true))
                .column(Column::initial(56.0))
                .column(Column::initial(170.0).clip(true))
                .column(Column::remainder().at_least(200.0).clip(true))
                .column(Column::initial(96.0))
                .column(Column::initial(90.0))
                .column(Column::initial(90.0))
                .header(28.0, |mut h| {
                    for t in ["App", "Proto", "Local", "Remote", "State", "Download", "Upload"] {
                        h.col(|ui| {
                            ui.label(RichText::new(t).font(bold(11.5)).color(MUTED()));
                        });
                    }
                })
                .body(|body| {
                    body.rows(28.0, conns.len(), |mut row| {
                        let c = conns[row.index()];
                        let p = by_pid.get(&c.pid).copied();
                        row.col(|ui| {
                            let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                            match p {
                                Some(p) => self.paint_proc_icon(ui, r, p),
                                None => paint_icon(ui, r, None, I::GEAR_SIX),
                            }
                            ui.add(egui::Label::new(p.map(|p| p.name.clone()).unwrap_or_else(|| if c.pid == 4 { "System".into() } else { format!("PID {}", c.pid) })).truncate());
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(match (c.proto, c.local.is_ipv6()) {
                                (Proto::Tcp, false) => "TCP",
                                (Proto::Tcp, true) => "TCP6",
                                (Proto::Udp, false) => "UDP",
                                (Proto::Udp, true) => "UDP6",
                            }).size(12.0).color(MUTED()));
                        });
                        row.col(|ui| {
                            ui.add(egui::Label::new(RichText::new(c.local.to_string()).size(12.0).color(MUTED())).truncate());
                        });
                        row.col(|ui| match c.remote {
                            Some(r) if !r.ip().is_unspecified() => {
                                let host = self.rdns.get(r.ip());
                                ui.add(egui::Label::new(RichText::new(format!("{}:{}", r.ip(), r.port())).size(12.0)).truncate());
                                if let Some(h) = host {
                                    ui.add(egui::Label::new(RichText::new(h).size(11.5).color(FAINT())).truncate());
                                }
                            }
                            _ => {
                                ui.label(RichText::new(if c.proto == Proto::Tcp { "—" } else { "any" }).color(FAINT()));
                            }
                        });
                        row.col(|ui| {
                            let st = if c.proto == Proto::Udp { "" } else { tcp_state(c.state) };
                            let col = match c.state {
                                5 => GOOD(),
                                2 => BLUE(),
                                _ => FAINT(),
                            };
                            if !st.is_empty() {
                                ui.label(RichText::new(st).size(12.0).color(col));
                            }
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(if c.recv_rate > 0.0 { fmt::rate(c.recv_rate) } else { String::new() }).size(12.0).color(C_NET()));
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(if c.send_rate > 0.0 { fmt::rate(c.send_rate) } else { String::new() }).size(12.0).color(CYAN()));
                        });
                        let resp = row.response();
                        resp.context_menu(|ui| {
                            if let Some(p) = p {
                                if ui.button(format!("{}  Show in Processes", I::LIST_BULLETS)).clicked() {
                                    acts.push(NetAct::GoTo(p.pid, p.create_time));
                                    ui.close();
                                }
                            }
                            if let Some(r) = c.remote.filter(|r| !r.ip().is_unspecified()) {
                                if ui.button(format!("{}  Copy remote address", I::COPY)).clicked() {
                                    acts.push(NetAct::Copy(r.to_string()));
                                    ui.close();
                                }
                                if ui.button(format!("{}  Look up IP owner", I::GLOBE)).clicked() {
                                    acts.push(NetAct::Lookup(r.ip().to_string()));
                                    ui.close();
                                }
                                if c.proto == Proto::Tcp && c.local.is_ipv4() && ui.button(RichText::new(format!("{}  Close connection", I::LINK_BREAK)).color(BAD())).clicked() {
                                    acts.push(NetAct::Close(c.clone()));
                                    ui.close();
                                }
                            }
                            if let Some(path) = p.and_then(|p| p.path.as_deref()) {
                                if ui.button(format!("{}  Block app's internet access", I::PROHIBIT)).clicked() {
                                    acts.push(NetAct::Block(path.to_string()));
                                    ui.close();
                                }
                            }
                        });
                        if resp.double_clicked() {
                            if let Some(p) = p {
                                acts.push(NetAct::GoTo(p.pid, p.create_time));
                            }
                        }
                    });
                });
        });
    }

    fn net_adapters(&mut self, ui: &mut Ui, snap: &crate::sampler::Snapshot) {
        let ctx = ui.ctx().clone();
        let offset = self.smooth_offset();
        let (s, r) = {
            let sh = self.shared.lock().unwrap();
            (sh.sys.adapter_send.clone(), sh.sys.adapter_recv.clone())
        };
        if snap.adapters.is_empty() {
            empty_state(ui, I::LINK_BREAK, "No connected adapters", "Plug in a cable or connect to Wi-Fi.");
            return;
        }
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for a in &snap.adapters {
                card().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(if a.kind == "Wi-Fi" { I::WIFI_HIGH } else { I::GLOBE }).size(22.0).color(C_NET()));
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            ui.label(RichText::new(&a.alias).font(bold(15.0)));
                            ui.label(RichText::new(format!("{}  ·  {}  ·  {}", a.kind, a.description, fmt::link_speed(a.speed))).size(12.0).color(MUTED()));
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(RichText::new(format!("↑ {}", fmt::bits(a.send_rate))).font(bold(13.0)).color(CYAN()));
                            ui.label(RichText::new(format!("↓ {}", fmt::bits(a.recv_rate))).font(bold(13.0)).color(C_NET()));
                        });
                    });
                    ui.add_space(6.0);
                    let rect = ui.allocate_exact_size(vec2(ui.available_width(), 110.0), Sense::hover()).0;
                    let e = VecDeque::new();
                    graph::graph(ui, rect, &[Series { data: r.get(&a.alias).unwrap_or(&e), color: C_NET(), label: "Download" }, Series { data: s.get(&a.alias).unwrap_or(&e), color: CYAN(), label: "Upload" }], &Opts { max: None, fmt: &|v| fmt::bits(v as f64), offset, grid: true, rounded: true, interval: snap.interval.max(0.25) });
                    ui.add_space(10.0);
                    let link_v6: Vec<&String> = a.ipv6.iter().filter(|x| !x.starts_with("fe80")).collect();
                    let tiles: Vec<(&str, String)> = vec![
                        ("IPv4 address", a.ipv4.join(", ")),
                        ("Gateway", a.gateways.join(", ")),
                        ("DNS servers", a.dns.iter().filter(|d| !d.starts_with("fe80")).cloned().collect::<Vec<_>>().join(", ")),
                        ("IPv6 address", link_v6.first().map(|x| x.to_string()).or_else(|| a.ipv6.first().cloned()).unwrap_or_default()),
                        ("MAC address", a.mac.clone()),
                        ("Link speed", fmt::link_speed(a.speed)),
                        ("Downloaded since boot", fmt::bytes(a.in_octets)),
                        ("Uploaded since boot", fmt::bytes(a.out_octets)),
                        ("Packets in / out per s", format!("{} / {}", fmt::count(a.in_pps), fmt::count(a.out_pps))),
                        ("Errors in / out", format!("{} / {}", a.in_errors, a.out_errors)),
                        ("Discards in / out", format!("{} / {}", a.in_discards, a.out_discards)),
                    ];
                    if let Some(v) = copy_tiles(ui, &a.alias, &tiles) {
                        ctx.copy_text(v);
                        self.toast(ToastKind::Info, "Copied to clipboard");
                    }
                });
                ui.add_space(10.0);
            }
        });
    }

    fn net_latency(&mut self, ui: &mut Ui, snap: &crate::sampler::Snapshot) {
        // Router first (auto-detected), then the user's targets.
        let mut targets: Vec<(String, String)> = Vec::new();
        if let Some(gw) = snap.adapters.iter().flat_map(|a| a.gateways.iter()).find(|g| g.parse::<std::net::Ipv4Addr>().is_ok()) {
            targets.push(("Your router".into(), gw.clone()));
        }
        targets.extend(self.cfg.latency_targets.iter().cloned());
        *self.latency.targets.lock().unwrap() = targets.clone();
        let series = self.latency.series.lock().unwrap().clone();
        ui.label(RichText::new("Pings each target once a second while this page is open. A spike on \"Internet\" but not on \"Your router\" means the problem is outside your home network.").size(12.5).color(MUTED()));
        ui.add_space(8.0);
        let mut remove: Option<String> = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for (label, target) in &targets {
                let s = series.iter().find(|s| &s.target == target);
                card().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    let last = s.and_then(|s| s.last());
                    let loss = s.map(|s| s.loss_pct()).unwrap_or(0.0);
                    let color = match last {
                        None if s.is_some_and(|s| !s.samples.is_empty()) => BAD(),
                        Some(v) if v < 40.0 => GOOD(),
                        Some(v) if v < 100.0 => WARN(),
                        Some(_) => BAD(),
                        None => FAINT(),
                    };
                    ui.horizontal(|ui| {
                        ui.set_min_height(38.0);
                        ui.label(RichText::new("●").color(color));
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            ui.label(RichText::new(label).font(bold(14.5)));
                            ui.label(RichText::new(target).size(12.0).color(FAINT()));
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if label == "Your router" {
                                ui.add_space(14.0); // keeps stats aligned with removable cards
                            } else if ui.add(egui::Label::new(RichText::new(I::X).color(FAINT())).sense(Sense::click())).on_hover_text("Remove").clicked() {
                                remove = Some(target.clone());
                            }
                            ui.add_space(8.0);
                            stat_inline(ui, "Loss", &format!("{loss:.0}%"), if loss > 2.0 { BAD() } else { MUTED() });
                            stat_inline(ui, "Jitter", &s.and_then(|s| s.jitter()).map(|v| format!("{v:.1} ms")).unwrap_or("—".into()), MUTED());
                            stat_inline(ui, "Average", &s.and_then(|s| s.avg()).map(|v| format!("{v:.0} ms")).unwrap_or("—".into()), MUTED());
                            stat_inline(ui, "Now", &last.map(|v| format!("{v:.0} ms")).unwrap_or(if s.is_some_and(|s| !s.samples.is_empty()) { "timeout".into() } else { "…".into() }), color);
                        });
                    });
                    ui.add_space(6.0);
                    let rect = ui.allocate_exact_size(vec2(ui.available_width(), 90.0), Sense::hover()).0;
                    let data: VecDeque<f32> = s.map(|s| s.samples.iter().map(|v| v.unwrap_or(0.0)).collect()).unwrap_or_default();
                    // Mark lost packets in red under the graph.
                    graph::graph(ui, rect, &[Series { data: &data, color: C_NET(), label: "" }], &Opts { max: None, fmt: &|v| format!("{v:.0} ms"), offset: 0.0, grid: true, rounded: true, interval: 1.0 });
                    if let Some(s) = s {
                        let step = rect.width() / (crate::sampler::HIST as f32 - 2.0);
                        let n = s.samples.len();
                        for (i, v) in s.samples.iter().enumerate() {
                            if v.is_none() {
                                let x = rect.right() - (n - 1 - i) as f32 * step;
                                ui.painter().rect_filled(egui::Rect::from_min_max(egui::pos2(x - step / 2.0, rect.bottom() - 4.0), egui::pos2(x + step / 2.0, rect.bottom())), CornerRadius::ZERO, BAD());
                            }
                        }
                    }
                });
                ui.add_space(8.0);
            }
            ui.horizontal(|ui| {
                field(ui, egui::Id::unique("lat-name"), I::TAG, &mut self.new_target.0, "Name (e.g. Game server)", 220.0);
                field(ui, egui::Id::unique("lat-ip"), I::GLOBE, &mut self.new_target.1, "IPv4 address", 200.0);
                let ok = self.new_target.1.trim().parse::<std::net::Ipv4Addr>().is_ok();
                if button(ui, Btn::Primary, I::PLUS, "Add target", ok).clicked() {
                    let name = if self.new_target.0.trim().is_empty() { self.new_target.1.trim().to_string() } else { self.new_target.0.trim().to_string() };
                    self.cfg.latency_targets.push((name, self.new_target.1.trim().to_string()));
                    self.cfg.save();
                    self.new_target = (String::new(), String::new());
                }
            });
        });
        if let Some(t) = remove {
            self.cfg.latency_targets.retain(|(_, x)| *x != t);
            self.cfg.save();
        }
    }

    fn net_blocked(&mut self, ui: &mut Ui) {
        ui.label(RichText::new("Apps blocked from the internet with Windows Firewall rules created by Sentinel. Right-click any app in Processes or Network to block it.").size(12.5).color(MUTED()));
        ui.add_space(8.0);
        if self.cfg.blocked_apps.is_empty() {
            empty_state(ui, I::PROHIBIT, "Nothing blocked", "Blocked apps show up here.");
            return;
        }
        let ctx = ui.ctx().clone();
        let mut unblock = None;
        for path in self.cfg.blocked_apps.clone() {
            card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let tex = self.tex(&ctx, &path);
                    let (r, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
                    paint_icon(ui, r, tex.as_ref(), I::APP_WINDOW);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.label(RichText::new(std::path::Path::new(&path).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default()).font(bold(13.5)));
                        ui.label(RichText::new(&path).size(11.5).color(FAINT()));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if button(ui, Btn::Ghost, I::CHECK, "Allow again", true).clicked() {
                            unblock = Some(path.clone());
                        }
                    });
                });
            });
            ui.add_space(6.0);
        }
        if let Some(p) = unblock {
            self.toggle_block(p);
        }
    }
}

/// Small "label over value" block with a fixed size, safe in any layout direction.
fn stat_inline(ui: &mut Ui, k: &str, v: &str, color: Color32) {
    let (r, _) = ui.allocate_exact_size(vec2(96.0, 36.0), Sense::hover());
    let p = ui.painter();
    p.text(r.left_top(), egui::Align2::LEFT_TOP, k, egui::FontId::proportional(11.0), FAINT());
    p.text(r.left_top() + vec2(0.0, 15.0), egui::Align2::LEFT_TOP, v, bold(15.0), color);
}

/// Grid of info tiles (label + value). Clicking a tile returns its value for copying.
fn copy_tiles(ui: &mut Ui, salt: &str, items: &[(&str, String)]) -> Option<String> {
    let mut out = None;
    let items: Vec<&(&str, String)> = items.iter().filter(|(_, v)| !v.trim().is_empty()).collect();
    let w = ui.available_width();
    let cols = ((w / 230.0).floor() as usize).clamp(1, 4);
    let cw = (w - (cols - 1) as f32 * 8.0) / cols as f32;
    for (ri, chunk) in items.chunks(cols).enumerate() {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            for (ci, (k, v)) in chunk.iter().enumerate() {
                let (r, _) = ui.allocate_exact_size(vec2(cw, 54.0), Sense::hover());
                let resp = ui.interact(r, ui.scope_id().with((salt, ri, ci)), Sense::click());
                let hover = resp.hovered();
                ui.painter().rect(r, CornerRadius::same(10), if hover { CARD_HI() } else { FIELD() }, Stroke::new(1.0, BORDER_SOFT()), egui::StrokeKind::Inside);
                ui.painter().text(r.left_top() + vec2(12.0, 9.0), egui::Align2::LEFT_TOP, *k, egui::FontId::proportional(11.5), FAINT());
                if hover {
                    ui.painter().text(r.right_top() + vec2(-12.0, 9.0), egui::Align2::RIGHT_TOP, I::COPY, egui::FontId::proportional(12.0), MUTED());
                }
                let galley = ui.painter().layout_no_wrap(v.clone(), bold(14.0), TEXT());
                let clipped = galley.size().x > r.width() - 24.0;
                ui.painter().with_clip_rect(r.shrink(8.0)).galley(r.left_top() + vec2(12.0, 27.0), galley, TEXT());
                let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
                let resp = if clipped { resp.on_hover_text(v.as_str()) } else { resp.on_hover_text("Click to copy") };
                if resp.clicked() {
                    out = Some(v.clone());
                }
            }
        });
        ui.add_space(4.0);
    }
    out
}
