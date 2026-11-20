//! Time-series graphs with gradient fills, smooth scrolling and hover readouts.

use crate::sampler::HIST;
use crate::theme::*;
use egui::{Color32, CornerRadius, Mesh, Pos2, Rect, Shape, Stroke, Ui, pos2, vec2};
use std::collections::VecDeque;

pub struct Series<'a> {
    pub data: &'a VecDeque<f32>,
    pub color: Color32,
    pub label: &'a str,
}

pub struct Opts<'a> {
    pub max: Option<f32>,
    pub fmt: &'a dyn Fn(f32) -> String,
    pub offset: f32,
    pub grid: bool,
    pub rounded: bool,
    pub interval: f32,
}

/// Rounds up to 1/2/5 × 10^n for axis maxima.
pub fn nice_max(v: f32) -> f32 {
    if v <= 0.0 {
        return 1.0;
    }
    let exp = v.log10().floor();
    let base = 10f32.powf(exp);
    let m = v / base;
    let n = if m <= 1.0 { 1.0 } else if m <= 2.0 { 2.0 } else if m <= 5.0 { 5.0 } else { 10.0 };
    n * base
}

fn points(rect: Rect, data: &VecDeque<f32>, max: f32, offset: f32) -> Vec<Pos2> {
    points_w(rect, data, max, offset, HIST)
}

fn points_w(rect: Rect, data: &VecDeque<f32>, max: f32, offset: f32, window: usize) -> Vec<Pos2> {
    let step = rect.width() / (window as f32 - 2.0);
    let skip = data.len().saturating_sub(window);
    let n = data.len() - skip;
    data.iter()
        .skip(skip)
        .enumerate()
        .map(|(i, v)| {
            let x = rect.right() - ((n - 1 - i) as f32 + offset) * step;
            let y = rect.bottom() - (v / max).clamp(0.0, 1.0) * (rect.height() - 2.0) - 1.0;
            pos2(x, y)
        })
        .collect()
}

fn fill_mesh(rect: Rect, pts: &[Pos2], color: Color32) -> Mesh {
    let mut mesh = Mesh::default();
    let top = color.gamma_multiply(0.32);
    let bottom = color.gamma_multiply(0.0);
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let i = mesh.vertices.len() as u32;
        let ta = lerp_color(bottom, top, ((rect.bottom() - a.y) / rect.height()).clamp(0.15, 1.0));
        let tb = lerp_color(bottom, top, ((rect.bottom() - b.y) / rect.height()).clamp(0.15, 1.0));
        mesh.colored_vertex(a, ta);
        mesh.colored_vertex(b, tb);
        mesh.colored_vertex(pos2(b.x, rect.bottom()), bottom);
        mesh.colored_vertex(pos2(a.x, rect.bottom()), bottom);
        mesh.add_triangle(i, i + 1, i + 2);
        mesh.add_triangle(i, i + 2, i + 3);
    }
    mesh
}

/// Extra behaviour for [`graph_ex`]: a real time axis, a crosshair shared between graphs, and
/// drag-to-zoom / click / double-click handling.
#[derive(Default, Clone, Copy)]
pub struct Ex<'a> {
    /// Unix time of the newest sample. Turns on clock-time ticks and labels.
    pub end_unix: Option<f64>,
    /// Graphs with the same link show the same moment when any one of them is hovered.
    pub link: Option<&'a str>,
    /// Drag to select a time range, click a moment, double-click to reset.
    pub select: bool,
}

pub struct Out {
    pub resp: egui::Response,
    /// Drag-selected range (unix start, end).
    pub selected: Option<(f64, f64)>,
    /// Clicked moment (unix) and where on screen.
    pub clicked: Option<(f64, Pos2)>,
    pub reset: bool,
    /// Mouse wheel over the graph (positive = zoom in), with the time under the pointer.
    pub wheel: Option<(f32, f64)>,
}

/// Full graph in `rect`.
pub fn graph(ui: &mut Ui, rect: Rect, series: &[Series], o: &Opts) -> egui::Response {
    graph_ex(ui, rect, series, o, &Ex::default()).resp
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// Clock label for a tick or tooltip; detail depends on how far apart ticks are.
pub fn time_label(t: f64, step: f64) -> String {
    let (_, mo, d, h, mi, s) = crate::fmt::civil(t.round() as i64);
    if step < 60.0 {
        format!("{h:02}:{mi:02}:{s:02}")
    } else if step < 86400.0 {
        format!("{h:02}:{mi:02}")
    } else {
        format!("{d} {}", MONTHS[(mo as usize).saturating_sub(1).min(11)])
    }
}

pub fn graph_ex(ui: &mut Ui, rect: Rect, series: &[Series], o: &Opts, ex: &Ex) -> Out {
    let resp = ui.allocate_rect(rect, if ex.select { egui::Sense::click_and_drag() } else { egui::Sense::hover() });
    let painter = ui.painter_at(rect);
    let max = o.max.unwrap_or_else(|| nice_max(series.iter().flat_map(|s| s.data.iter().copied()).fold(0.0f32, f32::max) * 1.1));
    let step = rect.width() / (HIST as f32 - 2.0);
    let interval = o.interval.max(1e-3) as f64;
    let end = ex.end_unix.unwrap_or(0.0);
    let x_of = |t: f64| rect.right() - (((end - t) / interval) as f32 + o.offset) * step;
    let back_at = |x: f32| (((rect.right() - x) / step) - o.offset).round().max(0.0) as usize;
    if o.rounded {
        painter.rect_filled(rect, CornerRadius::same(8), FIELD());
    }
    if o.grid {
        for i in 1..4 {
            let y = rect.top() + rect.height() * i as f32 / 4.0;
            painter.line_segment([pos2(rect.left(), y), pos2(rect.right(), y)], Stroke::new(1.0, BORDER_SOFT()));
        }
        if ex.end_unix.is_some() {
            // Ticks on round clock times (local), at most ~6 across.
            let span = (rect.width() / step) as f64 * interval;
            let steps = [10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 900.0, 1800.0, 3600.0, 7200.0, 10800.0, 21600.0, 43200.0, 86400.0];
            let ts = steps.iter().copied().find(|s| span / s <= 6.5).unwrap_or(86400.0);
            let lo = crate::fmt::local_offset_secs() as f64;
            let mut t = ((end - span + lo) / ts).ceil() * ts - lo;
            while t <= end {
                let x = x_of(t);
                if x > rect.left() + 2.0 && x < rect.right() - 2.0 {
                    painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1.0, BORDER_SOFT().gamma_multiply(0.6)));
                    painter.text(pos2(x + 4.0, rect.bottom() - 3.0), egui::Align2::LEFT_BOTTOM, time_label(t, ts), egui::FontId::proportional(10.0), FAINT());
                }
                t += ts;
            }
        } else {
            let cols = 6;
            for i in 1..cols {
                // Vertical lines scroll with the data.
                let x = rect.right() - (i as f32 * (HIST as f32 / cols as f32) + o.offset) * step;
                painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1.0, BORDER_SOFT().gamma_multiply(0.6)));
            }
        }
    }
    for s in series {
        if s.data.len() < 2 {
            continue;
        }
        // NaN samples are gaps (time nothing was recorded): draw each run separately.
        let pts = points(rect, s.data, max, o.offset);
        for run in pts.split(|p| p.y.is_nan()).filter(|r| r.len() >= 2) {
            painter.add(Shape::mesh(fill_mesh(rect, run, s.color)));
            painter.add(Shape::line(run.to_vec(), Stroke::new(1.6, s.color)));
        }
    }
    // Axis max label.
    painter.text(rect.left_top() + vec2(6.0, 4.0), egui::Align2::LEFT_TOP, (o.fmt)(max), egui::FontId::proportional(10.5), FAINT());

    // Which sample is hovered: this graph's pointer, or the same moment from a linked graph.
    let ctx = ui.ctx().clone();
    let frame = ctx.cumulative_frame_nr();
    let link_id = ex.link.filter(|_| ex.end_unix.is_some()).map(|l| egui::Id::unique(("graph-link", l)));
    let own = resp.hover_pos().filter(|p| rect.contains(*p));
    let back = match own {
        Some(p) => Some(back_at(p.x)),
        None => link_id.and_then(|id| ctx.data(|d| d.get_temp::<(u64, f64)>(id))).filter(|(f, _)| *f + 1 >= frame).map(|(_, t)| ((end - t) / interval).round().max(0.0) as usize),
    };
    if let (Some(p), Some(id)) = (own, link_id) {
        let t = end - back_at(p.x) as f64 * interval;
        ctx.data_mut(|d| d.insert_temp(id, (frame, t)));
    }
    if let Some(back) = back {
        let x = rect.right() - (back as f32 + o.offset) * step;
        painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1.0, MUTED().gamma_multiply(if own.is_some() { 0.5 } else { 0.35 })));
        let mut lines = Vec::new();
        for s in series {
            if back < s.data.len() && !s.data[s.data.len() - 1 - back].is_nan() {
                let v = s.data[s.data.len() - 1 - back];
                let y = rect.bottom() - (v / max).clamp(0.0, 1.0) * (rect.height() - 2.0) - 1.0;
                painter.circle_filled(pos2(x, y), 3.5, s.color);
                painter.circle_stroke(pos2(x, y), 3.5, Stroke::new(1.5, BG()));
                lines.push((s.color, if s.label.is_empty() { (o.fmt)(v) } else { format!("{}  {}", s.label, (o.fmt)(v)) }));
            }
        }
        if own.is_none() {
            // Linked: show the first value as a small tag instead of a tooltip.
            if let Some((c, t)) = lines.first() {
                let g = painter.layout_no_wrap(t.clone(), bold(11.0), *c);
                let tag = Rect::from_min_size(pos2((x + 6.0).min(rect.right() - g.size().x - 10.0), rect.top() + 4.0), g.size() + vec2(8.0, 4.0));
                painter.rect_filled(tag, CornerRadius::same(4), CARD_HI());
                painter.galley(tag.min + vec2(4.0, 2.0), g, *c);
            }
        } else if !lines.is_empty() {
            let secs = (back as f64 * interval).round() as u64;
            egui::Tooltip::always_open(ctx.clone(), ui.layer_id(), resp.id.with("tip"), egui::PopupAnchor::Pointer).gap(12.0).show(|ui| {
                let ago = if secs == 0 {
                    "now".to_string()
                } else if secs < 120 {
                    format!("{secs}s ago")
                } else if secs < 7200 {
                    format!("{} min ago", secs / 60)
                } else if secs < 172800 {
                    format!("{}h {:02}m ago", secs / 3600, (secs % 3600) / 60)
                } else {
                    format!("{} days ago", secs / 86400)
                };
                let when = if ex.end_unix.is_some() { format!("{}  ·  {ago}", time_label(end - back as f64 * interval, if interval >= 600.0 { 86400.0 } else if interval >= 1.0 && secs >= 60 { 60.0 } else { 1.0 })) } else { ago };
                ui.label(egui::RichText::new(when).size(11.0).color(FAINT()));
                for (c, t) in lines {
                    ui.label(egui::RichText::new(t).color(c).font(bold(12.5)));
                }
                if ex.select {
                    ui.label(egui::RichText::new("Click: what was running  ·  Drag: zoom  ·  Double-click: reset").size(10.5).color(FAINT()));
                }
            });
        }
    }

    let mut out = Out { resp: resp.clone(), selected: None, clicked: None, reset: false, wheel: None };
    if ex.select && ex.end_unix.is_some() {
        let sel_id = resp.id.with("drag-from");
        if resp.drag_started() {
            if let Some(p) = resp.interact_pointer_pos() {
                ctx.data_mut(|d| d.insert_temp(sel_id, p.x));
            }
        }
        let from: Option<f32> = ctx.data(|d| d.get_temp(sel_id));
        if let (Some(fx), Some(p)) = (from, resp.interact_pointer_pos()) {
            if resp.dragged() {
                let (a, b) = (fx.min(p.x).max(rect.left()), fx.max(p.x).min(rect.right()));
                painter.rect_filled(Rect::from_x_y_ranges(a..=b, rect.y_range()), CornerRadius::ZERO, ACCENT().gamma_multiply(0.18));
                painter.line_segment([pos2(a, rect.top()), pos2(a, rect.bottom())], Stroke::new(1.0, ACCENT()));
                painter.line_segment([pos2(b, rect.top()), pos2(b, rect.bottom())], Stroke::new(1.0, ACCENT()));
            }
        }
        if resp.drag_stopped() {
            if let (Some(fx), Some(p)) = (from, resp.interact_pointer_pos()) {
                let (a, b) = (fx.min(p.x).max(rect.left()), fx.max(p.x).min(rect.right()));
                if b - a > 8.0 {
                    let t = |x: f32| end - ((((rect.right() - x) / step) - o.offset).max(0.0) as f64) * interval;
                    out.selected = Some((t(a), t(b)));
                }
            }
            ctx.data_mut(|d| d.remove::<f32>(sel_id));
        }
        if resp.double_clicked() {
            out.reset = true;
        } else if resp.clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                out.clicked = Some((end - back_at(p.x) as f64 * interval, p));
            }
        }
        if let Some(p) = own {
            // Ctrl + wheel (egui turns it into a zoom delta).
            let z = ui.input(|i| i.zoom_delta());
            if (z - 1.0).abs() > 0.001 {
                out.wheel = Some((if z > 1.0 { 1.0 } else { -1.0 }, end - back_at(p.x) as f64 * interval));
            }
        }
    }
    out
}

/// "Low · Average · Peak (at 14:32)" for what a graph currently shows.
pub fn range_stats(ui: &mut Ui, data: &VecDeque<f32>, f: &dyn Fn(f32) -> String, end_unix: f64, interval: f32, color: Color32) {
    let vals: Vec<(usize, f32)> = data.iter().copied().enumerate().filter(|(_, v)| v.is_finite()).collect();
    if vals.len() < 2 {
        return;
    }
    let lo = vals.iter().map(|v| v.1).fold(f32::INFINITY, f32::min);
    let avg = vals.iter().map(|v| v.1).sum::<f32>() / vals.len() as f32;
    let (pi, pk) = vals.iter().copied().fold((0, f32::NEG_INFINITY), |a, b| if b.1 > a.1 { b } else { a });
    let when = end_unix - (data.len() - 1 - pi) as f64 * interval as f64;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let item = |ui: &mut Ui, k: &str, v: String, c: Color32| {
            ui.label(egui::RichText::new(k).size(11.5).color(FAINT()));
            ui.label(egui::RichText::new(v).size(12.0).font(bold(12.0)).color(c));
            ui.add_space(8.0);
        };
        item(ui, "Low", f(lo), MUTED());
        item(ui, "Average", f(avg), TEXT());
        item(ui, "Peak", f(pk), color);
        ui.label(egui::RichText::new(format!("at {}", time_label(when, if interval >= 600.0 { 86400.0 } else if interval >= 1.0 { 60.0 } else { 1.0 }))).size(11.5).color(FAINT()));
    });
}

/// Small sparkline without axes, showing the last minute.
pub fn sparkline(ui: &Ui, rect: Rect, data: &VecDeque<f32>, color: Color32, max: Option<f32>, offset: f32) {
    const WINDOW: usize = 60;
    if data.len() < 2 {
        return;
    }
    let max = max.unwrap_or_else(|| nice_max(data.iter().rev().take(WINDOW).copied().fold(0.0, f32::max) * 1.1));
    let painter = ui.painter_at(rect);
    let pts = points_w(rect, data, max, offset, WINDOW);
    painter.add(Shape::mesh(fill_mesh(rect, &pts, color)));
    painter.add(Shape::line(pts, Stroke::new(1.3, color)));
}

/// Sparkline from a fixed-size slice (per-process history) scaled to its own width.
pub fn mini(ui: &Ui, rect: Rect, data: &[f32], color: Color32, max: f32) {
    if data.len() < 2 {
        return;
    }
    let painter = ui.painter_at(rect);
    let step = rect.width() / (data.len() - 1).max(1) as f32;
    let pts: Vec<Pos2> = data.iter().enumerate().map(|(i, v)| pos2(rect.left() + i as f32 * step, rect.bottom() - (v / max.max(1e-6)).clamp(0.0, 1.0) * (rect.height() - 2.0) - 1.0)).collect();
    painter.add(Shape::mesh(fill_mesh(rect, &pts, color)));
    painter.add(Shape::line(pts, Stroke::new(1.4, color)));
}
