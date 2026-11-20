//! Mini overlay: a small always-on-top readout (CPU, memory, GPU, network) in a screen corner.
//! It keeps updating while Sentinel itself is hidden; clicking it opens Sentinel.

use crate::app::App;
use crate::fmt;
use crate::theme::*;
use egui::{Color32, CornerRadius, Sense, Stroke, ViewportBuilder, ViewportId, vec2};
use std::sync::atomic::Ordering;

const W: f32 = 318.0;
const H: f32 = 58.0;
const TITLE: &str = "Sentinel overlay";

fn work_area() -> (f32, f32, f32, f32) {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::UI::WindowsAndMessaging::{SPI_GETWORKAREA, SystemParametersInfoW};
    let mut r = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };
    unsafe { SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut r as *mut _ as *mut _, 0) };
    (r.left as f32, r.top as f32, r.right as f32, r.bottom as f32)
}

/// Rounded corners and no shadow-less square edges on Windows 11.
fn round_corners() {
    unsafe {
        let t = crate::win::wide(TITLE);
        let h = windows_sys::Win32::UI::WindowsAndMessaging::FindWindowW(std::ptr::null(), t.as_ptr());
        if !h.is_null() {
            let pref: u32 = 2; // DWMWCP_ROUND
            windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute(h, 33, &pref as *const _ as *const _, 4);
        }
    }
}

impl App {
    pub fn overlay(&mut self, ctx: &egui::Context) {
        self.ctl.overlay.store(self.cfg.overlay, Ordering::Relaxed);
        if !self.cfg.overlay {
            self.overlay_styled = false;
            return;
        }
        let snap = self.snap.clone();
        let ppp = ctx.input(|i| i.viewport().native_pixels_per_point).unwrap_or(1.0).max(0.5);
        let (l, t, r, b) = work_area();
        let (l, t, r, b) = (l / ppp, t / ppp, r / ppp, b / ppp);
        let m = 14.0;
        let pos = match self.cfg.overlay_corner {
            1 => egui::pos2(l + m, t + m),
            2 => egui::pos2(r - W - m, b - H - m),
            3 => egui::pos2(l + m, b - H - m),
            _ => egui::pos2(r - W - m, t + m),
        };
        let builder = ViewportBuilder::default()
            .with_title(TITLE)
            .with_decorations(false)
            .with_window_level(egui::WindowLevel::AlwaysOnTop)
            .with_taskbar(false)
            .with_resizable(false)
            .with_active(false)
            .with_inner_size([W, H])
            .with_position(pos);
        let used = snap.mem_total.saturating_sub(snap.mem_avail);
        let mem_pct = used as f32 / snap.mem_total.max(1) as f32 * 100.0;
        let gpu_t = snap.extra.gpu.temperature;
        let items: [(&str, String, Color32); 4] = [
            ("CPU", fmt::pct(snap.cpu_total), if snap.cpu_total >= 90.0 { BAD() } else { C_CPU() }),
            ("MEMORY", format!("{mem_pct:.0}%"), if mem_pct >= 90.0 { BAD() } else { C_MEM() }),
            ("GPU", format!("{}{}", fmt::pct(snap.gpu.util), gpu_t.map(|t| format!("  {t:.0}°")).unwrap_or_default()), if gpu_t.is_some_and(|t| t >= 83.0) { BAD() } else { C_GPU() }),
            ("NETWORK", format!("↓{}", fmt::bits(snap.net_recv)), C_NET()),
        ];
        let clicked = ctx.show_viewport_immediate(ViewportId::from_hash_of("sentinel-overlay"), builder, |ui, _| {
            let rect = ui.max_rect();
            let p = ui.painter();
            p.rect(rect, CornerRadius::same(8), SIDEBAR(), Stroke::new(1.0, BORDER()), egui::StrokeKind::Inside);
            let cw = (rect.width() - 16.0) / 4.0;
            for (i, (k, v, c)) in items.iter().enumerate() {
                let x = rect.left() + 12.0 + i as f32 * cw;
                p.text(egui::pos2(x, rect.top() + 10.0), egui::Align2::LEFT_TOP, *k, bold(9.5), FAINT());
                p.text(egui::pos2(x, rect.top() + 25.0), egui::Align2::LEFT_TOP, v, bold(15.0), *c);
            }
            let resp = ui.interact(rect, ui.scope_id().with("ov"), Sense::click());
            resp.on_hover_text("Open Sentinel").on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
        });
        if !self.overlay_styled {
            self.overlay_styled = true;
            round_corners();
        }
        if clicked {
            ctx.send_viewport_cmd_to(ViewportId::ROOT, egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd_to(ViewportId::ROOT, egui::ViewportCommand::Focus);
            self.show_flag.store(true, Ordering::SeqCst);
        }
        let _ = vec2;
    }
}
