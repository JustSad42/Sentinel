//! Mini overlay: a small always-on-top readout in a screen corner, with the metrics the user picks.
//! It keeps updating while Sentinel itself is hidden; clicking it opens Sentinel (unless it's set to
//! click-through, for games). Opacity and click-through are applied as Win32 layered-window styles.

use crate::app::App;
use crate::config::OverlayItem;
use crate::fmt;
use crate::theme::*;
use egui::{Color32, CornerRadius, Sense, Stroke, ViewportBuilder, ViewportId};
use std::sync::atomic::Ordering;

const H: f32 = 58.0;
const TITLE: &str = "Sentinel overlay";

fn work_area() -> (f32, f32, f32, f32) {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::UI::WindowsAndMessaging::{SPI_GETWORKAREA, SystemParametersInfoW};
    let mut r = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };
    unsafe { SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut r as *mut _ as *mut _, 0) };
    (r.left as f32, r.top as f32, r.right as f32, r.bottom as f32)
}

/// Rounded corners (Windows 11), opacity and click-through. Returns false if the window isn't up yet.
fn style_window(alpha: f32, click_through: bool) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    unsafe {
        let t = crate::win::wide(TITLE);
        let h = FindWindowW(std::ptr::null(), t.as_ptr());
        if h.is_null() {
            return false;
        }
        let pref: u32 = 2; // DWMWCP_ROUND
        windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute(h, 33, &pref as *const _ as *const _, 4);
        let mut ex = GetWindowLongPtrW(h, GWL_EXSTYLE) as u32;
        ex |= WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW;
        if click_through {
            ex |= WS_EX_TRANSPARENT;
        } else {
            ex &= !WS_EX_TRANSPARENT;
        }
        SetWindowLongPtrW(h, GWL_EXSTYLE, ex as isize);
        SetLayeredWindowAttributes(h, 0, (alpha.clamp(0.3, 1.0) * 255.0).round() as u8, LWA_ALPHA);
        true
    }
}

fn width_of(it: OverlayItem) -> f32 {
    match it {
        OverlayItem::Down | OverlayItem::Up => 98.0,
        OverlayItem::Memory | OverlayItem::Vram | OverlayItem::CpuClock => 82.0,
        OverlayItem::GpuTemp => 78.0,
        _ => 66.0,
    }
}

impl App {
    fn overlay_value(&self, it: OverlayItem) -> (&'static str, String, Color32) {
        let s = &self.snap;
        let x = &s.extra;
        match it {
            OverlayItem::Cpu => ("CPU", fmt::pct(s.cpu_total), if s.cpu_total >= 90.0 { BAD() } else { C_CPU() }),
            OverlayItem::CpuClock => ("CLOCK", x.cpu_mhz.map(fmt::mhz).unwrap_or("—".into()), C_CPU()),
            OverlayItem::Memory => {
                let used = s.mem_total.saturating_sub(s.mem_avail);
                let pct = used as f32 / s.mem_total.max(1) as f32 * 100.0;
                ("MEMORY", format!("{pct:.0}%"), if pct >= 90.0 { BAD() } else { C_MEM() })
            }
            OverlayItem::Gpu => ("GPU", fmt::pct(s.gpu.util), C_GPU()),
            OverlayItem::GpuTemp => match x.gpu.temperature {
                Some(t) => ("GPU TEMP", format!("{t:.0} °C"), if t >= 83.0 { BAD() } else if t >= 75.0 { WARN() } else { C_GPU() }),
                None => ("GPU TEMP", "—".into(), FAINT()),
            },
            OverlayItem::Vram => ("VRAM", fmt::bytes(s.gpu.dedicated_used), C_GPU()),
            OverlayItem::Disk => {
                let a = s.disks.iter().map(|d| d.active).fold(0.0, f32::max);
                ("DISK", format!("{a:.0}%"), if a >= 90.0 { BAD() } else { C_DISK() })
            }
            OverlayItem::Down => ("DOWN", fmt::bits(s.net_recv), C_NET()),
            OverlayItem::Up => ("UP", fmt::bits(s.net_send), C_NET()),
            OverlayItem::Ping => {
                let v = self.overlay_ping();
                match v {
                    Some(ms) => ("PING", format!("{ms:.0} ms"), if ms >= 120.0 { BAD() } else if ms >= 60.0 { WARN() } else { GOOD() }),
                    None => ("PING", "—".into(), FAINT()),
                }
            }
        }
    }

    /// For a sidebar meter: (value text, colour, how full the bar is 0..1).
    pub fn meter_value(&self, it: OverlayItem) -> (String, Color32, f32) {
        let (_, text, color) = self.overlay_value(it);
        let s = &self.snap;
        let link = s.adapters.iter().map(|a| a.speed).max().unwrap_or(0) as f32;
        let frac = match it {
            OverlayItem::Cpu => s.cpu_total / 100.0,
            OverlayItem::CpuClock => {
                let base = crate::hw::statics().map(|x| x.cpu.base_mhz).unwrap_or(0).max(1000) as f32;
                s.extra.cpu_mhz.unwrap_or(0) as f32 / (base * 1.4)
            }
            OverlayItem::Memory => s.mem_total.saturating_sub(s.mem_avail) as f32 / s.mem_total.max(1) as f32,
            OverlayItem::Gpu => s.gpu.util / 100.0,
            OverlayItem::GpuTemp => s.extra.gpu.temperature.unwrap_or(0.0) / 100.0,
            OverlayItem::Vram => s.gpu.dedicated_used as f32 / crate::ui_perf::gpu_info().1.max(1) as f32,
            OverlayItem::Disk => s.disks.iter().map(|d| d.active).fold(0.0, f32::max) / 100.0,
            OverlayItem::Down => if link > 0.0 { s.net_recv as f32 * 8.0 / link } else { 0.0 },
            OverlayItem::Up => if link > 0.0 { s.net_send as f32 * 8.0 / link } else { 0.0 },
            OverlayItem::Ping => self.overlay_ping().unwrap_or(0.0) / 150.0,
        };
        (text, color, frac.clamp(0.0, 1.0))
    }

    /// Latest ping to the first internet (non-private) latency target.
    fn overlay_ping(&self) -> Option<f32> {
        let series = self.latency.series.lock().unwrap();
        let public = |t: &str| t.parse::<std::net::Ipv4Addr>().is_ok_and(|ip| !ip.is_private() && !ip.is_loopback() && !ip.is_link_local());
        series.iter().find(|s| public(&s.target)).or_else(|| series.first()).and_then(|s| s.last())
    }

    pub fn overlay(&mut self, ctx: &egui::Context) {
        self.ctl.overlay.store(self.cfg.overlay, Ordering::Relaxed);
        if !self.cfg.overlay {
            self.overlay_styled = false;
            return;
        }
        let items: Vec<OverlayItem> = if self.cfg.overlay_items.is_empty() { vec![OverlayItem::Cpu, OverlayItem::Memory] } else { self.cfg.overlay_items.clone() };
        let w = 16.0 + items.iter().map(|i| width_of(*i)).sum::<f32>();
        let ppp = ctx.input(|i| i.viewport().native_pixels_per_point).unwrap_or(1.0).max(0.5);
        let (l, t, r, b) = work_area();
        let (l, t, r, b) = (l / ppp, t / ppp, r / ppp, b / ppp);
        let m = 14.0;
        let pos = match self.cfg.overlay_corner {
            1 => egui::pos2(l + m, t + m),
            2 => egui::pos2(r - w - m, b - H - m),
            3 => egui::pos2(l + m, b - H - m),
            _ => egui::pos2(r - w - m, t + m),
        };
        let builder = ViewportBuilder::default()
            .with_title(TITLE)
            .with_decorations(false)
            .with_window_level(egui::WindowLevel::AlwaysOnTop)
            .with_taskbar(false)
            .with_resizable(false)
            .with_active(false)
            .with_inner_size([w, H])
            .with_position(pos);
        let cells: Vec<(f32, &str, String, Color32)> = items.iter().map(|i| {
            let (k, v, c) = self.overlay_value(*i);
            (width_of(*i), k, v, c)
        }).collect();
        let click_through = self.cfg.overlay_click_through;
        let clicked = ctx.show_viewport_immediate(ViewportId::from_hash_of("sentinel-overlay"), builder, |ui, _| {
            let rect = ui.max_rect();
            let p = ui.painter();
            p.rect(rect, CornerRadius::same(8), SIDEBAR(), Stroke::new(1.0, BORDER()), egui::StrokeKind::Inside);
            let mut x = rect.left() + 12.0;
            for (cw, k, v, c) in &cells {
                p.text(egui::pos2(x, rect.top() + 10.0), egui::Align2::LEFT_TOP, *k, bold(9.5), FAINT());
                p.text(egui::pos2(x, rect.top() + 25.0), egui::Align2::LEFT_TOP, v, bold(15.0), *c);
                x += cw;
            }
            if click_through {
                return false;
            }
            let resp = ui.interact(rect, ui.scope_id().with("ov"), Sense::click());
            resp.on_hover_text("Open Sentinel").on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
        });
        let style = (self.cfg.overlay_opacity, click_through);
        if !self.overlay_styled || self.overlay_style != style {
            if style_window(style.0, style.1) {
                self.overlay_styled = true;
                self.overlay_style = style;
            }
        }
        if clicked {
            ctx.send_viewport_cmd_to(ViewportId::ROOT, egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd_to(ViewportId::ROOT, egui::ViewportCommand::Focus);
            self.show_flag.store(true, Ordering::SeqCst);
        }
    }
}
