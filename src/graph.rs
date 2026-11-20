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

/// Full graph in `rect`. Returns the hovered sample value of the first series.
pub fn graph(ui: &mut Ui, rect: Rect, series: &[Series], o: &Opts) -> egui::Response {
    let resp = ui.allocate_rect(rect, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let max = o.max.unwrap_or_else(|| nice_max(series.iter().flat_map(|s| s.data.iter().copied()).fold(0.0f32, f32::max) * 1.1));
    if o.rounded {
        painter.rect_filled(rect, CornerRadius::same(8), FIELD);
    }
    if o.grid {
        for i in 1..4 {
            let y = rect.top() + rect.height() * i as f32 / 4.0;
            painter.line_segment([pos2(rect.left(), y), pos2(rect.right(), y)], Stroke::new(1.0, BORDER_SOFT));
        }
        let step = rect.width() / (HIST as f32 - 2.0);
        let cols = 6;
        for i in 1..cols {
            // Vertical lines scroll with the data.
            let x = rect.right() - (i as f32 * (HIST as f32 / cols as f32) + o.offset) * step;
            painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1.0, BORDER_SOFT.gamma_multiply(0.6)));
        }
    }
    for s in series {
        if s.data.len() < 2 {
            continue;
        }
        let pts = points(rect, s.data, max, o.offset);
        painter.add(Shape::mesh(fill_mesh(rect, &pts, s.color)));
        painter.add(Shape::line(pts, Stroke::new(1.6, s.color)));
    }
    // Axis max label.
    painter.text(rect.left_top() + vec2(6.0, 4.0), egui::Align2::LEFT_TOP, (o.fmt)(max), egui::FontId::proportional(10.5), FAINT);
    if let Some(p) = resp.hover_pos() {
        let step = rect.width() / (HIST as f32 - 2.0);
        let back = (((rect.right() - p.x) / step) - o.offset).round().max(0.0) as usize;
        let x = rect.right() - (back as f32 + o.offset) * step;
        painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1.0, MUTED.gamma_multiply(0.5)));
        let mut lines = Vec::new();
        for s in series {
            if back < s.data.len() {
                let v = s.data[s.data.len() - 1 - back];
                let y = rect.bottom() - (v / max).clamp(0.0, 1.0) * (rect.height() - 2.0) - 1.0;
                painter.circle_filled(pos2(x, y), 3.5, s.color);
                painter.circle_stroke(pos2(x, y), 3.5, Stroke::new(1.5, BG));
                lines.push((s.color, if s.label.is_empty() { (o.fmt)(v) } else { format!("{}  {}", s.label, (o.fmt)(v)) }));
            }
        }
        if !lines.is_empty() {
            let secs = (back as f32 * o.interval).round() as u64;
            egui::Tooltip::always_open(ui.ctx().clone(), ui.layer_id(), resp.id.with("tip"), egui::PopupAnchor::Pointer).gap(12.0).show(|ui| {
                ui.label(egui::RichText::new(if secs == 0 { "now".to_string() } else { format!("{secs}s ago") }).size(11.0).color(FAINT));
                for (c, t) in lines {
                    ui.label(egui::RichText::new(t).color(c).font(bold(12.5)));
                }
            });
        }
    }
    resp
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
