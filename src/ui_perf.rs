//! Performance page: resource list with live sparklines and a detailed view per resource.

use crate::app::{App, Page, header};
use crate::fmt;
use crate::graph::{self, Opts, Series};
use crate::icons as I;
use crate::sampler::{Proc, Ring};
use crate::theme::*;
use egui::{Color32, CornerRadius, RichText, Sense, Stroke, Ui, vec2};
use std::collections::VecDeque;
use std::sync::OnceLock;

fn cpu_name() -> &'static str {
    static N: OnceLock<String> = OnceLock::new();
    N.get_or_init(|| {
        winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE)
            .open_subkey(r"HARDWARE\DESCRIPTION\System\CentralProcessor\0")
            .and_then(|k| k.get_value::<String, _>("ProcessorNameString"))
            .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
            .unwrap_or_else(|_| "Processor".into())
    })
}

fn cpu_mhz() -> u32 {
    static N: OnceLock<u32> = OnceLock::new();
    *N.get_or_init(|| winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE).open_subkey(r"HARDWARE\DESCRIPTION\System\CentralProcessor\0").and_then(|k| k.get_value::<u32, _>("~MHz")).unwrap_or(0))
}

/// (name, dedicated VRAM bytes) of the GPU with the most memory.
pub fn gpu_info() -> &'static (String, u64) {
    static N: OnceLock<(String, u64)> = OnceLock::new();
    N.get_or_init(|| {
        let mut best = ("GPU".to_string(), 0u64);
        if let Ok(class) = winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE).open_subkey(r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}") {
            for k in class.enum_keys().flatten().filter(|k| k.chars().all(|c| c.is_ascii_digit())) {
                if let Ok(sub) = class.open_subkey(&k) {
                    let mem = sub.get_value::<u64, _>("HardwareInformation.qwMemorySize").ok().or_else(|| sub.get_value::<u32, _>("HardwareInformation.MemorySize").ok().map(|v| v as u64)).unwrap_or(0);
                    let name: String = sub.get_value("DriverDesc").unwrap_or_default();
                    if mem > best.1 || (best.1 == 0 && !name.is_empty()) {
                        best = (name, mem);
                    }
                }
            }
        }
        best
    })
}

#[derive(Clone, PartialEq)]
enum Res {
    Cpu,
    Mem,
    Disk(String),
    Net(String),
    Gpu,
    System,
}

impl App {
    pub fn performance_page(&mut self, ui: &mut Ui) {
        let snap = self.snap.clone();
        header(ui, "Performance", &format!("{}  ·  up {}", cpu_name(), fmt::duration(crate::win::uptime_secs())), |ui| {
            let mut r = self.perf_range;
            if segmented(ui, &mut r, &["Live", "1 hour", "24 hours", "7 days"]) {
                self.perf_range = r;
                self.perf_zoom = None;
                self.perf_pick = None;
            }
            let mut ex = self.perf_expanded;
            if chip(ui, &mut ex, &format!("{} Expand", I::FRAME_CORNERS), None).on_hover_text("Make the graphs fill the window (Esc to go back)").clicked() {
                self.perf_expanded = ex;
            }
            if let Some((a, b)) = self.perf_zoom {
                let mut on = true;
                let lbl = format!("{} {} – {}", I::X, graph::time_label(a, if b - a > 86400.0 { 86400.0 } else { 60.0 }), graph::time_label(b, if b - a > 86400.0 { 86400.0 } else { 60.0 }));
                if chip(ui, &mut on, &lbl, None).on_hover_text("Zoomed in. Click (or double-click a graph) to reset").clicked() {
                    self.perf_zoom = None;
                }
            }
            if self.perf_range == 0 {
                let mut on = self.cfg.smooth_graphs;
                if chip(ui, &mut on, "Smooth scrolling", None).clicked() {
                    self.cfg.smooth_graphs = on;
                    self.cfg.save();
                }
            }
        });
        let mut list: Vec<Res> = vec![Res::Cpu, Res::Mem];
        for d in &snap.disks {
            list.push(Res::Disk(d.name.clone()));
        }
        for a in &snap.adapters {
            list.push(Res::Net(a.alias.clone()));
        }
        list.push(Res::Gpu);
        list.push(Res::System);
        // perf_sel: 0..4 = kinds (from the processes page tiles), 100+i = exact list index.
        let sel = if self.perf_sel >= 100 {
            (self.perf_sel - 100).min(list.len() - 1)
        } else {
            match self.perf_sel {
                0 => 0,
                1 => 1,
                2 => list.iter().position(|r| matches!(r, Res::Disk(_))).unwrap_or(0),
                3 => list.iter().position(|r| matches!(r, Res::Net(_))).unwrap_or(0),
                _ => list.iter().position(|r| matches!(r, Res::Gpu)).unwrap_or(0),
            }
        };
        let long_range = [0i64, 3600, 86400, 7 * 86400][self.perf_range.min(3)];
        let offset = if long_range > 0 { 0.0 } else { self.smooth_offset() };
        if self.perf_expanded && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.perf_expanded = false;
        }
        let now = crate::win::now_unix();
        let (ws, we) = match self.perf_zoom {
            Some(z) if long_range > 0 => z,
            _ => ((now - long_range) as f64, now as f64),
        };
        let long = if long_range > 0 {
            let key = (ws as i64 / 10, we as i64 / 10, self.perf_range);
            match &self.perf_cache {
                Some((k, c)) if *k == key => c.clone(),
                _ => {
                    let c = std::rc::Rc::new(LongSeries::build(&self.shared.lock().unwrap(), ws, we));
                    self.perf_cache = Some((key, c.clone()));
                    c
                }
            }
        } else {
            std::rc::Rc::new(LongSeries::default())
        };
        let sh = self.shared.lock().unwrap();
        let sys = &sh.sys;
        // Copy what we need so the lock is short.
        let cpu = sys.cpu.clone();
        let cpu_k = sys.cpu_kernel.clone();
        let cores = sys.cores.clone();
        let mem = sys.mem.clone();
        let commit = sys.commit.clone();
        let gpu = sys.gpu.clone();
        let gpu_mem = sys.gpu_mem.clone();
        let disk_a = sys.disk_active.clone();
        let disk_r = sys.disk_read.clone();
        let disk_w = sys.disk_write.clone();
        let ad_s = sys.adapter_send.clone();
        let ad_r = sys.adapter_recv.clone();
        drop(sh);

        egui::Panel::left("perf-list").exact_size(250.0).resizable(false).show_separator_line(false).frame(egui::Frame::new().inner_margin(egui::Margin { left: 0, right: 14, top: 0, bottom: 0 })).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                for (i, r) in list.iter().enumerate() {
                    let (icon, title, value, data, color, max): (&str, String, String, VecDeque<f32>, Color32, Option<f32>) = match r {
                        Res::Cpu => (I::CPU, "CPU".into(), format!("{}  ·  {:.2} GHz", fmt::pct(snap.cpu_total), snap.extra.cpu_mhz.unwrap_or(cpu_mhz()) as f32 / 1000.0), cpu.clone(), C_CPU(), Some(100.0)),
                        Res::Mem => {
                            let used = snap.mem_total.saturating_sub(snap.mem_avail);
                            (I::MEMORY, "Memory".into(), format!("{} / {} ({:.0}%)", fmt::bytes(used), fmt::bytes(snap.mem_total), used as f64 / snap.mem_total.max(1) as f64 * 100.0), mem.clone(), C_MEM(), Some(100.0))
                        }
                        Res::Disk(n) => {
                            let d = snap.disks.iter().find(|d| &d.name == n);
                            let kind = d.and_then(|d| d.info.as_ref()).map(|i| match i.ssd { Some(true) => "SSD", Some(false) => "HDD", None => "" }).unwrap_or("");
                            let temp = d.and_then(|d| d.temperature).map(|t| format!("  ·  {t}°C")).unwrap_or_default();
                            (I::HARD_DRIVES, format!("Disk {n}  {kind}"), d.map(|d| format!("{:.0}%  ·  {}{temp}", d.active, fmt::rate(d.read + d.write))).unwrap_or_default(), disk_a.get(n).cloned().unwrap_or_default(), C_DISK(), Some(100.0))
                        }
                        Res::Net(n) => {
                            let a = snap.adapters.iter().find(|a| &a.alias == n);
                            let s = ad_s.get(n).cloned().unwrap_or_default();
                            let rr = ad_r.get(n).cloned().unwrap_or_default();
                            let sum: VecDeque<f32> = s.iter().zip(rr.iter()).map(|(a, b)| a + b).collect();
                            (if a.is_some_and(|a| a.kind == "Wi-Fi") { I::WIFI_HIGH } else { I::GLOBE }, a.map(|a| a.kind.to_string()).unwrap_or_default() + " · " + n, a.map(|a| format!("↓ {}  ↑ {}", fmt::bits(a.recv_rate), fmt::bits(a.send_rate))).unwrap_or_default(), sum, C_NET(), None)
                        }
                        Res::Gpu => {
                            let temp = snap.extra.gpu.temperature.map(|t| format!("  ·  {t:.0}°C")).unwrap_or_default();
                            (I::GRAPHICS_CARD, "GPU".into(), format!("{}{temp}  ·  {}", fmt::pct(snap.gpu.util), gpu_info().0), gpu.clone(), C_GPU(), Some(100.0))
                        }
                        Res::System => {
                            let os = crate::hw::statics().map(|s| s.sys.os.clone()).unwrap_or_default();
                            (I::MONITOR, "System".into(), format!("{os}  ·  up {}", fmt::duration(crate::win::uptime_secs())), VecDeque::new(), ACCENT_HI(), None)
                        }
                    };
                    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 66.0), Sense::click());
                    let hover = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
                    let active = i == sel;
                    ui.painter().rect(rect, CornerRadius::same(10), if active { CARD_HI() } else { lerp_color(BG(), CARD(), hover) }, Stroke::new(1.0, if active { color.gamma_multiply(0.55) } else { Color32::TRANSPARENT }), egui::StrokeKind::Inside);
                    let spark = egui::Rect::from_min_size(rect.left_top() + vec2(10.0, 12.0), vec2(64.0, 42.0));
                    ui.painter().rect_filled(spark, CornerRadius::same(6), FIELD());
                    if matches!(r, Res::System) {
                        ui.painter().text(spark.center(), egui::Align2::CENTER_CENTER, I::INFO, egui::FontId::proportional(22.0), ACCENT_HI());
                    } else {
                        graph::sparkline(ui, spark, &data, color, max, offset);
                    }
                    ui.painter().text(rect.left_top() + vec2(86.0, 14.0), egui::Align2::LEFT_TOP, format!("{icon}  {title}"), bold(13.0), TEXT());
                    let galley = ui.painter().layout(value, egui::FontId::proportional(11.5), MUTED(), rect.width() - 96.0);
                    ui.painter().galley(rect.left_top() + vec2(86.0, 34.0), galley, MUTED());
                    if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                        self.perf_sel = 100 + i;
                    }
                    ui.add_space(2.0);
                }
            });
        });

        // Long ranges: HIST buckets of averages, so the same graphs can draw them.
        let (cpu, mem, gpu, disk_a, disk_r, disk_w, ad_s, ad_r, gpu_temp, interval_long) = if long_range > 0 {
            let all = |m: &VecDeque<f32>| -> std::collections::HashMap<String, VecDeque<f32>> { list.iter().filter_map(|r| match r { Res::Disk(n) | Res::Net(n) => Some((n.clone(), m.clone())), _ => None }).collect() };
            (long.cpu.clone(), long.mem.clone(), long.gpu.clone(), all(&long.disk_a), all(&long.disk_r), all(&long.disk_w), all(&long.up), all(&long.down), long.gpu_temp.clone(), long.bw)
        } else {
            (cpu, mem, gpu, disk_a, disk_r, disk_w, ad_s, ad_r, VecDeque::new(), 0.0)
        };
        let empty_q: VecDeque<f32> = VecDeque::new();
        let long_note = if long_range > 0 {
            let have = now - long.first_t;
            let zoomed = self.perf_zoom.is_some();
            Some(if long.first_t == 0 {
                "Nothing recorded yet. Sentinel records in the background.".to_string()
            } else if have + 120 < long_range && !zoomed {
                format!("Averages · {} recorded so far (Sentinel records in the background)", fmt::duration(have.max(0) as u64))
            } else {
                format!("Averages of {} each  ·  drag across a graph to zoom, click a moment to see what was running", fmt::duration(long.bw.round() as u64))
            })
        } else {
            None
        };
        let expanded = self.perf_expanded && !matches!(list[sel], Res::System);
        let view_h = ui.available_height();
        let gx = Gx { end: if long_range > 0 { we } else { now_f() - (offset * snap.interval.max(0.25)) as f64 }, long: long_range > 0 };
        egui::ScrollArea::vertical().id_salt("perf-detail").auto_shrink([false, false]).show(ui, |ui| {
            let w = ui.available_width();
            let interval = if long_range > 0 { interval_long } else { snap.interval.max(0.25) };
            // Expanded: graphs share the window height; otherwise their usual sizes.
            let tall = |base: f32, n: f32| if expanded { ((view_h - 150.0) / n - 34.0).max(base) } else { base };
            if let Some(n) = &long_note {
                ui.label(RichText::new(format!("{}  {n}", I::CLOCK)).size(12.0).color(FAINT()));
                ui.add_space(4.0);
            }
            match &list[sel] {
                Res::Cpu => {
                    big_title(ui, "CPU", cpu_name(), &fmt::pct(snap.cpu_total), C_CPU());
                    let rect = alloc(ui, w, tall(240.0, 1.0));
                    let pf = |v: f32| format!("{v:.0}%");
                    self.pgraph(ui, rect, &[Series { data: &cpu, color: C_CPU(), label: "Total" }, Series { data: if long_range > 0 { &empty_q } else { &cpu_k }, color: BLUE().gamma_multiply(0.7), label: "Kernel" }], &Opts { max: Some(100.0), fmt: &pf, offset, grid: true, rounded: true, interval }, &gx, false);
                    graph::range_stats(ui, &cpu, &pf, gx.end, interval, C_CPU());
                    if expanded {
                        return;
                    }
                    ui.add_space(10.0);
                    let x = &snap.extra;
                    let st = crate::hw::statics().map(|s| s.cpu.clone()).unwrap_or_default();
                    let mut items: Vec<(&str, String)> = vec![
                        ("Utilization", fmt::pct(snap.cpu_total)),
                        ("Speed", x.cpu_mhz.map(fmt::mhz).unwrap_or_else(|| "—".into())),
                        ("Base speed", fmt::mhz(if st.base_mhz > 0 { st.base_mhz } else { cpu_mhz() })),
                        ("Kernel time", fmt::pct(snap.cpu_kernel)),
                    ];
                    if st.cores > 0 {
                        items.push(("Sockets", st.sockets.max(1).to_string()));
                        items.push(("Cores", if st.e_cores > 0 { format!("{} ({}P + {}E)", st.cores, st.p_cores, st.e_cores) } else { st.cores.to_string() }));
                    }
                    items.push(("Logical processors", snap.cores.len().to_string()));
                    items.push(("Virtualization", if st.virtualization { "Enabled".into() } else { "Disabled".into() }));
                    if st.l1 > 0 {
                        items.push(("L1 cache", fmt::bytes(st.l1)));
                        items.push(("L2 cache", fmt::bytes(st.l2)));
                        items.push(("L3 cache", fmt::bytes(st.l3)));
                    }
                    items.extend([
                        ("Processes", snap.proc_count.to_string()),
                        ("Threads", snap.thread_count.to_string()),
                        ("Handles", snap.handle_count.to_string()),
                        ("Up time", fmt::duration(crate::win::uptime_secs())),
                    ]);
                    stats(ui, &items);
                    ui.add_space(8.0);
                    label_caps(ui, "Kernel activity");
                    stats(ui, &[
                        ("Context switches / s", fmt::count(x.ctx_switches)),
                        ("System calls / s", fmt::count(x.syscalls)),
                        ("Interrupts / s", fmt::count(x.interrupts)),
                        ("DPC time", format!("{:.1}%", x.dpc_pct)),
                        ("Interrupt time", format!("{:.1}%", x.irq_pct)),
                    ]);
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        label_caps(ui, "Logical processors");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            chip(ui, &mut self.per_core, "Show graphs", None);
                        });
                    });
                    if self.per_core {
                        let n = cores.len().max(1);
                        let cols = ((w / 150.0).floor() as usize).clamp(2, 16);
                        let cw = (w - (cols - 1) as f32 * 8.0) / cols as f32;
                        for chunk in (0..n).collect::<Vec<_>>().chunks(cols) {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 8.0;
                                for &i in chunk {
                                    let r = alloc(ui, cw, 64.0);
                                    ui.painter().rect_filled(r, CornerRadius::same(8), FIELD());
                                    if let Some(d) = cores.get(i) {
                                        graph::sparkline(ui, r.shrink(1.0), d, C_CPU(), Some(100.0), offset);
                                    }
                                    ui.painter().text(r.left_top() + vec2(6.0, 4.0), egui::Align2::LEFT_TOP, format!("{i}  {}", fmt::pct(*snap.cores.get(i).unwrap_or(&0.0))), egui::FontId::proportional(10.5), MUTED());
                                }
                            });
                            ui.add_space(4.0);
                        }
                    } else {
                        // Compact heat bars per core.
                        let n = snap.cores.len().max(1);
                        let bw = ((w - (n - 1) as f32 * 3.0) / n as f32).clamp(4.0, 40.0);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 3.0;
                            for (i, v) in snap.cores.iter().enumerate() {
                                let r = alloc(ui, bw, 46.0);
                                ui.painter().rect_filled(r, CornerRadius::same(4), FIELD());
                                let h = r.height() * (v / 100.0).clamp(0.0, 1.0);
                                ui.painter().rect_filled(egui::Rect::from_min_max(egui::pos2(r.left(), r.bottom() - h), r.right_bottom()), CornerRadius::same(4), lerp_color(C_CPU().gamma_multiply(0.6), BAD(), (v / 100.0 - 0.6).max(0.0) * 2.5));
                                ui.interact(r, ui.scope_id().with(("core", i)), Sense::hover()).on_hover_text(format!("CPU {i}: {}", fmt::pct(*v)));
                            }
                        });
                    }
                    ui.add_space(14.0);
                    self.top_list(ui, "Top CPU", &snap.procs, |p| p.cpu as f64, |v| fmt::pct(v as f32), C_CPU(), 100.0);
                }
                Res::Mem => {
                    let used = snap.mem_total.saturating_sub(snap.mem_avail);
                    big_title(ui, "Memory", &format!("{} installed", fmt::bytes(snap.mem_total)), &fmt::bytes(used), C_MEM());
                    let rect = alloc(ui, w, tall(220.0, 1.0));
                    let pf = |v: f32| format!("{v:.0}%");
                    self.pgraph(ui, rect, &[Series { data: &mem, color: C_MEM(), label: "In use" }, Series { data: if long_range > 0 { &empty_q } else { &commit }, color: PINK().gamma_multiply(0.6), label: "Committed" }], &Opts { max: Some(100.0), fmt: &pf, offset, grid: true, rounded: true, interval }, &gx, true);
                    graph::range_stats(ui, &mem, &pf, gx.end, interval, C_MEM());
                    if expanded {
                        return;
                    }
                    ui.add_space(12.0);
                    label_caps(ui, "Composition");
                    let r = alloc(ui, w, 18.0);
                    let total = snap.mem_total.max(1) as f32;
                    let cache = snap.cache.min(snap.mem_avail);
                    let segs: Vec<(f32, Color32, &str)> = if let Some(l) = &snap.extra.mem_lists {
                        let free = l.free + l.zero;
                        let in_use = snap.mem_total.saturating_sub(free + l.standby + l.modified);
                        vec![(in_use as f32, C_MEM(), "In use"), (l.modified as f32, ORANGE().gamma_multiply(0.75), "Modified"), (l.standby as f32, BLUE().gamma_multiply(0.6), "Standby"), (free as f32, BORDER(), "Free")]
                    } else {
                        vec![(used as f32, C_MEM(), "In use"), (cache as f32, BLUE().gamma_multiply(0.6), "Cached"), ((snap.mem_avail - cache) as f32, BORDER(), "Free")]
                    };
                    let mut x = r.left();
                    for &(v, c, _) in &segs {
                        let sw = r.width() * v / total;
                        ui.painter().rect_filled(egui::Rect::from_min_size(egui::pos2(x, r.top()), vec2(sw.max(0.0), r.height())), CornerRadius::same(4), c);
                        x += sw;
                    }
                    ui.horizontal(|ui| {
                        for &(v, c, l) in &segs {
                            ui.label(RichText::new("●").color(c));
                            ui.label(RichText::new(format!("{l} {}", fmt::bytes(v as u64))).size(12.0).color(MUTED()));
                            ui.add_space(8.0);
                        }
                    });
                    ui.add_space(8.0);
                    let x = &snap.extra;
                    let ms = crate::hw::statics().map(|s| s.mem.clone()).unwrap_or_default();
                    let mut items: Vec<(String, String)> = vec![
                        ("In use".into(), fmt::bytes(used)),
                        ("Available".into(), fmt::bytes(snap.mem_avail)),
                        ("Committed".into(), format!("{} / {}", fmt::bytes(snap.commit_total), fmt::bytes(snap.commit_limit))),
                        ("Cached".into(), fmt::bytes(snap.cache)),
                    ];
                    if let Some(l) = &x.mem_lists {
                        items.push(("Standby".into(), fmt::bytes(l.standby)));
                        items.push(("Modified".into(), fmt::bytes(l.modified)));
                        items.push(("Free".into(), fmt::bytes(l.free + l.zero)));
                    }
                    if let Some(c) = snap.procs.iter().find(|p| p.name.eq_ignore_ascii_case("Memory Compression")) {
                        items.push(("Compressed".into(), fmt::bytes(c.mem)));
                    }
                    items.push(("Paged pool".into(), fmt::bytes(snap.paged_pool)));
                    items.push(("Non-paged pool".into(), fmt::bytes(snap.nonpaged_pool)));
                    if ms.speed() > 0 {
                        items.push(("Speed".into(), format!("{} MT/s", ms.speed())));
                    }
                    let kind = format!("{} {}", ms.kind(), ms.form()).trim().to_string();
                    if !kind.is_empty() {
                        items.push(("Type".into(), kind));
                    }
                    if ms.slots_total > 0 {
                        items.push(("Slots used".into(), format!("{} of {}", ms.dimms.len(), ms.slots_total)));
                    }
                    if ms.installed > snap.mem_total {
                        items.push(("Hardware reserved".into(), fmt::bytes(ms.installed - snap.mem_total)));
                    }
                    for p in &x.pagefiles {
                        items.push((format!("Page file {}", p.name.get(..2).unwrap_or("")), format!("{} / {}", fmt::bytes(p.used), fmt::bytes(p.total))));
                    }
                    items.push(("Page faults / s".into(), fmt::count(x.page_faults)));
                    items.push(("Hard faults / s".into(), fmt::count(x.hard_faults)));
                    stats(ui, &items);
                    if !ms.dimms.is_empty() {
                        ui.add_space(8.0);
                        card().show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            label_caps(ui, "Memory modules");
                            for d in &ms.dimms {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(I::MEMORY).color(C_MEM()));
                                    ui.label(RichText::new(fmt::bytes(d.size)).font(bold(13.0)));
                                    let spd = if d.configured > 0 && d.configured != d.speed { format!("{} MT/s (rated {})", d.configured, d.speed) } else { format!("{} MT/s", d.speed) };
                                    ui.label(RichText::new(format!("{} {}  ·  {spd}", d.kind, d.form)).color(MUTED()));
                                    ui.label(RichText::new(format!("{} {}", d.maker, d.part)).color(FAINT()));
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        ui.label(RichText::new(&d.slot).size(11.5).color(FAINT()));
                                    });
                                });
                            }
                        });
                    }
                    ui.add_space(14.0);
                    let tm = snap.mem_total as f64;
                    self.top_list(ui, "Top memory", &snap.procs, |p| p.mem as f64, |v| fmt::bytes(v as u64), C_MEM(), tm * 0.25);
                }
                Res::Disk(n) => {
                    let d = snap.disks.iter().find(|d| &d.name == n).cloned().unwrap_or_default();
                    let info = d.info.as_deref().cloned().unwrap_or_default();
                    let kind = match info.ssd { Some(true) => "SSD", Some(false) => "HDD", None => "Disk" };
                    let sub = [info.model.clone(), format!("{} {kind}", info.bus).trim().to_string(), if info.size > 0 { fmt::bytes(info.size) } else { String::new() }].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("  ·  ");
                    big_title(ui, &format!("Disk {n}"), &sub, &format!("{:.0}% active", d.active), C_DISK());
                    let empty = Ring::new();
                    let rect = alloc(ui, w, tall(170.0, 2.0));
                    let pf = |v: f32| format!("{v:.0}%");
                    let act = disk_a.get(n).unwrap_or(&empty);
                    self.pgraph(ui, rect, &[Series { data: act, color: C_DISK(), label: if long_range > 0 { "Busiest disk" } else { "Active time" } }], &Opts { max: Some(100.0), fmt: &pf, offset, grid: true, rounded: true, interval }, &gx, false);
                    graph::range_stats(ui, act, &pf, gx.end, interval, C_DISK());
                    ui.add_space(6.0);
                    let rect = alloc(ui, w, tall(150.0, 2.0));
                    self.pgraph(ui, rect, &[Series { data: disk_r.get(n).unwrap_or(&empty), color: C_DISK(), label: "Read" }, Series { data: disk_w.get(n).unwrap_or(&empty), color: CYAN(), label: "Write" }], &Opts { max: None, fmt: &|v| fmt::rate(v as f64), offset, grid: true, rounded: true, interval }, &gx, false);
                    if expanded {
                        return;
                    }
                    ui.add_space(10.0);
                    let mut items: Vec<(&str, String)> = vec![
                        ("Active time", format!("{:.0}%", d.active)),
                        ("Read speed", fmt::rate(d.read)),
                        ("Write speed", fmt::rate(d.write)),
                        ("Average response", if d.response_ms > 0.0 && d.response_ms < 0.1 { "< 0.1 ms".into() } else { format!("{:.1} ms", d.response_ms) }),
                        ("Read IOPS", fmt::count(d.read_iops)),
                        ("Write IOPS", fmt::count(d.write_iops)),
                        ("Queue length", d.queue.to_string()),
                    ];
                    if let Some(t) = d.temperature {
                        items.push(("Temperature", format!("{t} °C")));
                    }
                    if info.size > 0 {
                        items.push(("Capacity", fmt::bytes(info.size)));
                    }
                    if !info.bus.is_empty() {
                        items.push(("Type", format!("{} {kind}", info.bus)));
                    }
                    stats(ui, &items);
                    if !d.volumes.is_empty() {
                        ui.add_space(8.0);
                        card().show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            label_caps(ui, "Volumes");
                            for v in &d.volumes {
                                ui.horizontal(|ui| {
                                    let used = v.total.saturating_sub(v.free);
                                    let frac = used as f32 / v.total.max(1) as f32;
                                    ui.label(RichText::new(format!("{}:", v.letter)).font(bold(14.0)));
                                    ui.allocate_ui_with_layout(vec2(170.0, 20.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                        ui.set_width(170.0);
                                        let l = if v.label.is_empty() { format!("Local disk  ·  {}", v.fs) } else { format!("{}  ·  {}", v.label, v.fs) };
                                        ui.add(egui::Label::new(RichText::new(l).color(MUTED())).truncate());
                                    });
                                    let bw = (ui.available_width() - 200.0).max(60.0);
                                    bar(ui, frac, if frac > 0.9 { BAD() } else if frac > 0.8 { WARN() } else { C_DISK() }, bw, 8.0);
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        ui.label(RichText::new(format!("{} free of {}", fmt::bytes(v.free), fmt::bytes(v.total))).size(12.5));
                                    });
                                });
                            }
                        });
                    }
                    ui.add_space(14.0);
                    self.top_list(ui, "Top I/O (all devices)", &snap.procs, |p| p.io_rate, fmt::rate, C_DISK(), 50.0 * 1048576.0);
                }
                Res::Net(n) => {
                    let a = snap.adapters.iter().find(|a| &a.alias == n).cloned().unwrap_or_default();
                    big_title(ui, &a.alias, &a.description, &format!("↓ {}   ↑ {}", fmt::bits(a.recv_rate), fmt::bits(a.send_rate)), C_NET());
                    let empty = Ring::new();
                    let rect = alloc(ui, w, tall(220.0, 1.0));
                    let pf = |v: f32| fmt::bits(v as f64);
                    let rx = ad_r.get(n).unwrap_or(&empty);
                    self.pgraph(ui, rect, &[Series { data: rx, color: C_NET(), label: "Receive" }, Series { data: ad_s.get(n).unwrap_or(&empty), color: CYAN(), label: "Send" }], &Opts { max: None, fmt: &pf, offset, grid: true, rounded: true, interval }, &gx, false);
                    graph::range_stats(ui, rx, &pf, gx.end, interval, C_NET());
                    if expanded {
                        return;
                    }
                    ui.add_space(10.0);
                    stats(ui, &[
                        ("Receive", fmt::bits(a.recv_rate)),
                        ("Send", fmt::bits(a.send_rate)),
                        ("Link speed", fmt::link_speed(a.speed)),
                        ("Type", a.kind.to_string()),
                        ("Received since boot", fmt::bytes(a.in_octets)),
                        ("Sent since boot", fmt::bytes(a.out_octets)),
                        ("Packets in / s", fmt::count(a.in_pps)),
                        ("Packets out / s", fmt::count(a.out_pps)),
                        ("Errors (in / out)", format!("{} / {}", a.in_errors, a.out_errors)),
                        ("Discards (in / out)", format!("{} / {}", a.in_discards, a.out_discards)),
                        ("IPv4", a.ipv4.join(", ")),
                        ("IPv6", a.ipv6.first().cloned().unwrap_or_else(|| "—".into())),
                        ("Gateway", a.gateways.join(", ")),
                        ("DNS", a.dns.join(", ")),
                        ("MAC", a.mac.clone()),
                    ]);
                    if let Some(t) = &snap.extra.tcp {
                        ui.add_space(8.0);
                        label_caps(ui, "TCP health (all adapters)");
                        stats(ui, &[
                            ("Established", t.established.to_string()),
                            ("All TCP connections", t.connections.to_string()),
                            ("Retransmitted", format!("{:.2}%", t.retrans_pct)),
                            ("Segments in / s", fmt::count(t.segs_in)),
                            ("Segments out / s", fmt::count(t.segs_out)),
                            ("Failed connects / s", format!("{:.1}", t.failed)),
                            ("Resets / s", format!("{:.1}", t.resets)),
                            ("Receive errors / s", format!("{:.1}", t.in_errors)),
                        ]);
                    }
                    ui.add_space(10.0);
                    if button(ui, Btn::Ghost, I::GLOBE, "Open Network monitor", true).clicked() {
                        self.page = Page::Network;
                    }
                }
                Res::System => {
                    let st = crate::hw::statics().cloned().unwrap_or_default();
                    let model = format!("{} {}", st.sys.maker, st.sys.model).trim().to_string();
                    let title = if model.is_empty() || model.eq_ignore_ascii_case(&st.sys.maker) { st.sys.board.clone() } else { model };
                    big_title(ui, if title.is_empty() { "This PC" } else { &title }, &st.sys.os, &format!("up {}", fmt::duration(crate::win::uptime_secs())), ACCENT_HI());
                    let boot = crate::win::now_unix() - crate::win::uptime_secs() as i64;
                    let mem_kind = [st.mem.kind(), if st.mem.speed() > 0 { format!("{} MT/s", st.mem.speed()) } else { String::new() }].join(" ");
                    let mut items: Vec<(&str, String)> = vec![
                        ("Windows", st.sys.os.clone()),
                        ("Build", st.sys.build.clone()),
                        ("Booted", fmt::when(boot)),
                        ("Up time", fmt::duration(crate::win::uptime_secs())),
                        ("Processor", cpu_name().to_string()),
                        ("Memory", format!("{} {}", fmt::bytes(if st.mem.installed > 0 { st.mem.installed } else { snap.mem_total }), mem_kind.trim())),
                        ("Graphics", gpu_info().0.clone()),
                        ("Motherboard", st.sys.board.clone()),
                        ("BIOS", st.sys.bios.clone()),
                        ("BIOS date", st.sys.bios_date.clone()),
                    ];
                    if let Some((pct, ac)) = snap.extra.battery {
                        items.push(("Battery", format!("{pct}%{}", if ac { "  ·  plugged in" } else { "" })));
                    }
                    items.retain(|(_, v)| !v.trim().is_empty());
                    stats_w(ui, &items, 380.0);
                    ui.add_space(10.0);
                    let disks: Vec<String> = snap.disks.iter().filter_map(|d| d.info.as_ref().map(|i| format!("{}  ·  {} {}", i.model, fmt::bytes(i.size), match i.ssd { Some(true) => "SSD", Some(false) => "HDD", None => "" }))).collect();
                    if !disks.is_empty() {
                        card().show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            label_caps(ui, "Storage");
                            for d in disks {
                                ui.label(RichText::new(format!("{}  {d}", I::HARD_DRIVES)).color(TEXT()));
                            }
                        });
                    }
                }
                Res::Gpu => {
                    let (name, total) = gpu_info();
                    big_title(ui, "GPU", name, &fmt::pct(snap.gpu.util), C_GPU());
                    let has_second = (long_range > 0 && gpu_temp.iter().any(|v| *v > 0.0)) || (*total > 0 && long_range == 0);
                    let n_g = if has_second { 2.0 } else { 1.0 };
                    let rect = alloc(ui, w, tall(200.0, n_g));
                    let pf = |v: f32| format!("{v:.0}%");
                    self.pgraph(ui, rect, &[Series { data: &gpu, color: C_GPU(), label: "Utilization" }], &Opts { max: Some(100.0), fmt: &pf, offset, grid: true, rounded: true, interval }, &gx, false);
                    graph::range_stats(ui, &gpu, &pf, gx.end, interval, C_GPU());
                    ui.add_space(6.0);
                    if long_range > 0 && gpu_temp.iter().any(|v| *v > 0.0) {
                        let rect = alloc(ui, w, tall(110.0, n_g));
                        let tf = |v: f32| format!("{v:.0} °C");
                        self.pgraph(ui, rect, &[Series { data: &gpu_temp, color: ORANGE(), label: "Temperature" }], &Opts { max: None, fmt: &tf, offset, grid: true, rounded: true, interval }, &gx, false);
                        graph::range_stats(ui, &gpu_temp, &tf, gx.end, interval, ORANGE());
                        ui.add_space(8.0);
                    } else if *total > 0 && long_range == 0 {
                        let rect = alloc(ui, w, tall(110.0, n_g));
                        let t = *total as f32;
                        self.pgraph(ui, rect, &[Series { data: &gpu_mem, color: PINK(), label: "Dedicated memory" }], &Opts { max: Some(t), fmt: &|v| fmt::bytes(v as u64), offset, grid: true, rounded: true, interval }, &gx, false);
                        ui.add_space(8.0);
                    }
                    if expanded {
                        return;
                    }
                    let mut s: Vec<(String, String)> = vec![
                        ("Utilization".into(), fmt::pct(snap.gpu.util)),
                        ("Dedicated memory".into(), if *total > 0 { format!("{} / {}", fmt::bytes(snap.gpu.dedicated_used), fmt::bytes(*total)) } else { fmt::bytes(snap.gpu.dedicated_used) }),
                        ("Shared memory".into(), fmt::bytes(snap.gpu.shared_used)),
                    ];
                    let gs = &snap.extra.gpu;
                    if let Some(t) = gs.temperature {
                        s.push(("Temperature".into(), format!("{t:.0} °C")));
                    }
                    if let Some(f) = gs.fan_rpm {
                        s.push(("Fan".into(), format!("{f} RPM")));
                    }
                    if let Some(p) = gs.power_pct {
                        s.push(("Power draw".into(), format!("{p:.0}% of limit")));
                    }
                    if let Some(m) = gs.mem_clock_mhz {
                        s.push(("Memory clock".into(), fmt::mhz(m)));
                    }
                    if *total > 0 {
                        s.push(("Total VRAM".into(), fmt::bytes(*total)));
                    }
                    if let Some(st) = crate::hw::statics().filter(|s| !s.sys.gpu_driver.is_empty()) {
                        s.push(("Driver".into(), st.sys.gpu_driver.clone()));
                        s.push(("Driver date".into(), st.sys.gpu_driver_date.clone()));
                    }
                    for (e, v) in snap.gpu.engines.iter().take(5) {
                        s.push((format!("Engine: {e}"), fmt::pct(*v)));
                    }
                    stats(ui, &s);
                    ui.add_space(14.0);
                    self.top_list(ui, "Top GPU", &snap.procs, |p| p.gpu as f64, |v| fmt::pct(v as f32), C_GPU(), 100.0);
                }
            }
        });
    }

    fn top_list(&mut self, ui: &mut Ui, title: &str, procs: &[Proc], val: impl Fn(&Proc) -> f64, fmt_v: impl Fn(f64) -> String, color: Color32, scale: f64) {
        let mut v: Vec<&Proc> = procs.iter().filter(|p| val(p) > 0.0).collect();
        v.sort_by(|a, b| val(b).total_cmp(&val(a)));
        if v.is_empty() {
            return;
        }
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            label_caps(ui, title);
            for p in v.into_iter().take(6) {
                let r = ui.horizontal(|ui| {
                    let (ir, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                    self.paint_proc_icon(ui, ir, p);
                    let meta = self.info.meta(p.key(), p.path.as_ref());
                    let name = meta.and_then(|m| m.description.clone()).filter(|d| !d.is_empty()).unwrap_or_else(|| p.name.clone());
                    ui.allocate_ui_with_layout(vec2(240.0, 20.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.set_width(240.0);
                        ui.add(egui::Label::new(RichText::new(name)).truncate());
                    });
                    let bw = (ui.available_width() - 90.0).max(40.0);
                    bar(ui, (val(p) / scale) as f32, color, bw, 6.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(fmt_v(val(p))).font(bold(12.5)));
                    });
                });
                if ui.interact(r.response.rect, ui.scope_id().with(("top", p.pid)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    self.page = Page::Processes;
                    self.selected = Some(p.key());
                    self.sel_group = None;
                    self.details_open = true;
                    self.scroll_to_sel = true;
                    self.cfg.view = crate::config::ViewMode::Flat;
                }
            }
        });
    }
}

fn alloc(ui: &mut Ui, w: f32, h: f32) -> egui::Rect {
    ui.allocate_exact_size(vec2(w, h), Sense::hover()).0
}

fn big_title(ui: &mut Ui, title: &str, sub: &str, value: &str, color: Color32) {
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.label(RichText::new(title).font(bold(20.0)));
            ui.label(RichText::new(sub).color(MUTED()).size(12.5));
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(value).font(bold(24.0)).color(color));
        });
    });
    ui.add_space(8.0);
}

fn stats<K: AsRef<str>>(ui: &mut Ui, items: &[(K, String)]) {
    stats_w(ui, items, 190.0);
}

/// Stat cards with a minimum card width (wider cards for long values like model names).
fn stats_w<K: AsRef<str>>(ui: &mut Ui, items: &[(K, String)], min_w: f32) {
    let w = ui.available_width();
    let cols = ((w / min_w).floor() as usize).clamp(2, 4);
    let cw = (w - (cols - 1) as f32 * 8.0) / cols as f32;
    for chunk in items.chunks(cols) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            for (k, v) in chunk {
                let (r, resp) = ui.allocate_exact_size(vec2(cw, 54.0), Sense::hover());
                ui.painter().rect(r, CornerRadius::same(10), CARD(), Stroke::new(1.0, BORDER_SOFT()), egui::StrokeKind::Inside);
                ui.painter().text(r.left_top() + vec2(12.0, 9.0), egui::Align2::LEFT_TOP, k.as_ref(), egui::FontId::proportional(11.5), FAINT());
                let galley = ui.painter().layout_no_wrap(v.clone(), bold(14.0), TEXT());
                let clipped = galley.size().x > r.width() - 24.0;
                ui.painter().with_clip_rect(r.shrink(8.0)).galley(r.left_top() + vec2(12.0, 27.0), galley, TEXT());
                if clipped {
                    resp.on_hover_text(v);
                }
            }
        });
        ui.add_space(4.0);
    }
}


/// Hidden in plain sight: f64 "now" for smooth time axes.
fn now_f() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// Where the Performance graphs end and whether they are long-range (zoomable) ones.
pub struct Gx {
    pub end: f64,
    pub long: bool,
}

/// Long-range Performance series: `HIST` buckets of averages over a time window.
#[derive(Default)]
pub struct LongSeries {
    pub cpu: VecDeque<f32>,
    pub mem: VecDeque<f32>,
    pub gpu: VecDeque<f32>,
    pub gpu_temp: VecDeque<f32>,
    pub disk_a: VecDeque<f32>,
    pub disk_r: VecDeque<f32>,
    pub disk_w: VecDeque<f32>,
    pub up: VecDeque<f32>,
    pub down: VecDeque<f32>,
    /// Bucket width in seconds.
    pub bw: f32,
    /// First recorded point in the source (0 = none).
    pub first_t: i64,
}

impl LongSeries {
    pub fn build(sh: &crate::sampler::Shared, start: f64, end: f64) -> Self {
        let n = crate::sampler::HIST;
        let bw = ((end - start) / n as f64).max(1.0);
        // The 10-second points cover the last 24 h; older windows use the 5-minute ones, topped up
        // with the 10-second points newer than the last 5-minute one.
        let recent = start >= (crate::win::now_unix() - 86400 - 60) as f64;
        let first_t = if recent { sh.long.front() } else { sh.week.front().or(sh.long.front()) }.map(|p| p.t).unwrap_or(0);
        let mut sums = vec![[(0.0f32, 0u32); 9]; n];
        let week_end = if recent { i64::MIN } else { sh.week.back().map(|p| p.t).unwrap_or(i64::MIN) };
        let week_part = if recent { None } else { Some(sh.week.iter().skip(sh.week.partition_point(|p| (p.t as f64) < start))) };
        let long_part = sh.long.iter().skip(sh.long.partition_point(|p| (p.t as f64) < start)).filter(move |p| p.t > week_end);
        for p in week_part.into_iter().flatten().chain(long_part) {
            let t = p.t as f64;
            if t > end + bw {
                break;
            }
            let i = ((t - start) / bw) as usize;
            if i >= n {
                continue;
            }
            for (k, v) in [p.cpu, p.mem, p.gpu, p.gpu_temp, p.disk_active, p.disk_r, p.disk_w, p.up, p.down].into_iter().enumerate() {
                if v.is_finite() {
                    sums[i][k].0 += v;
                    sums[i][k].1 += 1;
                }
            }
        }
        let col = |k: usize| -> VecDeque<f32> { sums.iter().map(|b| if b[k].1 > 0 { b[k].0 / b[k].1 as f32 } else { f32::NAN }).collect() };
        LongSeries { cpu: col(0), mem: col(1), gpu: col(2), gpu_temp: col(3), disk_a: col(4), disk_r: col(5), disk_w: col(6), up: col(7), down: col(8), bw: bw as f32, first_t }
    }
}

impl App {
    /// A Performance graph: linked crosshair, and on long ranges drag-to-zoom, Ctrl+wheel zoom,
    /// double-click reset and click-for-top-apps.
    fn pgraph(&mut self, ui: &mut Ui, rect: egui::Rect, series: &[Series], o: &Opts, gx: &Gx, by_mem: bool) {
        let out = graph::graph_ex(ui, rect, series, o, &graph::Ex { end_unix: Some(gx.end), link: Some("perf"), select: gx.long });
        if !gx.long {
            return;
        }
        let range = [0.0, 3600.0, 86400.0, 7.0 * 86400.0][self.perf_range.min(3)];
        let now = crate::win::now_unix() as f64;
        if let Some((a, b)) = out.selected {
            if b - a >= 120.0 {
                self.perf_zoom = Some((a, b));
                self.perf_pick = None;
            }
        }
        if out.reset {
            self.perf_zoom = None;
            self.perf_pick = None;
        } else if let Some((t, pos)) = out.clicked {
            self.perf_pick = Some((t, o.interval as f64, by_mem, pos, ui.ctx().cumulative_frame_nr()));
        }
        if let Some((dir, t)) = out.wheel {
            let (a, b) = self.perf_zoom.unwrap_or((now - range, now));
            let span = b - a;
            let new = (span * if dir > 0.0 { 0.7 } else { 1.0 / 0.7 }).clamp(600.0, range);
            if new >= range - 1.0 {
                self.perf_zoom = None;
            } else {
                let f = ((t - a) / span).clamp(0.0, 1.0);
                let mut na = t - new * f;
                let mut nb = na + new;
                if nb > now {
                    nb = now;
                    na = now - new;
                }
                if na < now - range {
                    na = now - range;
                    nb = na + new;
                }
                self.perf_zoom = Some((na, nb));
            }
        }
    }

    /// Popup after clicking a moment: the heaviest apps around then, from the per-app history.
    pub fn perf_pick_ui(&mut self, ctx: &egui::Context) {
        let Some((t, span, by_mem, pos, opened)) = self.perf_pick else { return };
        let from = ((t - span / 2.0) / 60.0).floor().max(0.0) as u32;
        let to = ((t + span / 2.0) / 60.0).floor() as u32;
        let (rows, first) = {
            let sh = self.shared.lock().unwrap();
            (sh.apphist.top_between(from, to.max(from), by_mem, 6), sh.apphist.first_minute())
        };
        let names: Vec<(String, String)> = rows
            .iter()
            .map(|(exe, _, _)| {
                let shown = self.snap.procs.iter().find(|p| p.name.eq_ignore_ascii_case(exe)).and_then(|p| self.info.meta(p.key(), p.path.as_ref()).and_then(|m| m.description.clone())).filter(|d| !d.is_empty()).unwrap_or_else(|| exe.clone());
                (shown, exe.clone())
            })
            .collect();
        let screen = ctx.content_rect();
        let at = egui::pos2((pos.x + 14.0).min(screen.right() - 330.0), (pos.y + 14.0).min(screen.bottom() - 300.0));
        let mut close = false;
        let r = egui::Area::new(egui::Id::unique("perf-pick")).order(egui::Order::Foreground).fixed_pos(at).show(ctx, |ui| {
            egui::Frame::new().fill(CARD_HI()).stroke(Stroke::new(1.0, BORDER())).corner_radius(CornerRadius::same(10)).inner_margin(egui::Margin::same(12)).shadow(egui::Shadow { offset: [0, 8], blur: 24, spread: 0, color: Color32::from_black_alpha(120) }).show(ui, |ui| {
                ui.set_width(300.0);
                ui.horizontal(|ui| {
                    let label = if span >= 120.0 { format!("{} – {}", graph::time_label(t - span / 2.0, 60.0), graph::time_label(t + span / 2.0, 60.0)) } else { graph::time_label(t, 60.0) };
                    ui.label(RichText::new(label).font(bold(13.5)));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(egui::Label::new(RichText::new(I::X).color(FAINT())).sense(Sense::click())).clicked() {
                            close = true;
                        }
                    });
                });
                ui.label(RichText::new(if by_mem { "Top apps by memory" } else { "Top apps by CPU" }).size(11.5).color(FAINT()));
                ui.add_space(4.0);
                if rows.is_empty() {
                    let msg = match first {
                        Some(m) if (m as f64) * 60.0 > t => format!("Per-app history starts at {}.", graph::time_label(m as f64 * 60.0, 60.0)),
                        _ => "Nothing was using much then.".to_string(),
                    };
                    ui.label(RichText::new(msg).size(12.0).color(MUTED()));
                }
                let maxv = rows.first().map(|r| if by_mem { r.2 as f32 } else { r.1 }).unwrap_or(1.0).max(0.01);
                for ((_, cpu, mb), (shown, exe)) in rows.iter().zip(names.iter()) {
                    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
                    let p = ui.painter();
                    p.text(rect.left_top() + vec2(0.0, 2.0), egui::Align2::LEFT_TOP, shown, egui::FontId::proportional(12.5), TEXT());
                    let v = if by_mem { fmt::bytes((*mb as u64) << 20) } else { fmt::pct(*cpu) };
                    p.text(rect.right_top() + vec2(0.0, 2.0), egui::Align2::RIGHT_TOP, v, bold(12.5), if by_mem { C_MEM() } else { C_CPU() });
                    let frac = (if by_mem { *mb as f32 } else { *cpu }) / maxv;
                    let bar = egui::Rect::from_min_size(rect.left_top() + vec2(0.0, 20.0), vec2(rect.width(), 3.0));
                    p.rect_filled(bar, CornerRadius::same(2), FIELD());
                    p.rect_filled(egui::Rect::from_min_size(bar.min, vec2(bar.width() * frac.clamp(0.0, 1.0), 3.0)), CornerRadius::same(2), if by_mem { C_MEM() } else { C_CPU() });
                    resp.on_hover_text(format!("{exe}  ·  {}  ·  {}", fmt::pct(*cpu), fmt::bytes((*mb as u64) << 20)));
                }
                ui.add_space(2.0);
                ui.label(RichText::new("Per-app history is kept for 24 hours.").size(10.5).color(FAINT()));
            });
        });
        let frame = ctx.cumulative_frame_nr();
        let clicked_outside = frame > opened + 1 && ctx.input(|i| i.pointer.any_pressed()) && ctx.pointer_interact_pos().is_some_and(|p| !r.response.rect.contains(p));
        if close || clicked_outside || ctx.input(|i| i.key_pressed(egui::Key::Escape)) || self.page != crate::app::Page::Performance {
            self.perf_pick = None;
        }
    }
}
