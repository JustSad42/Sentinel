//! Visual design: palette, fonts and custom widgets.

use egui::{Align2, Color32, CornerRadius, FontFamily, FontId, Rect, Response, RichText, Sense, Stroke, StrokeKind, Ui, Vec2, vec2};
use std::sync::Arc;

pub const BG: Color32 = Color32::from_rgb(11, 12, 17);
pub const SIDEBAR: Color32 = Color32::from_rgb(15, 16, 23);
pub const CARD: Color32 = Color32::from_rgb(20, 22, 31);
pub const CARD_HI: Color32 = Color32::from_rgb(28, 30, 42);
pub const ROW_HOVER: Color32 = Color32::from_rgb(25, 27, 38);
pub const FIELD: Color32 = Color32::from_rgb(16, 17, 25);
pub const BORDER: Color32 = Color32::from_rgb(36, 39, 54);
pub const BORDER_SOFT: Color32 = Color32::from_rgb(28, 30, 42);
pub const TEXT: Color32 = Color32::from_rgb(232, 234, 242);
pub const MUTED: Color32 = Color32::from_rgb(148, 152, 172);
pub const FAINT: Color32 = Color32::from_rgb(96, 100, 122);
pub const ACCENT: Color32 = Color32::from_rgb(124, 92, 255);
pub const ACCENT_HI: Color32 = Color32::from_rgb(150, 124, 255);
pub const ACCENT_DIM: Color32 = Color32::from_rgb(38, 32, 74);
pub const GOOD: Color32 = Color32::from_rgb(52, 211, 153);
pub const WARN: Color32 = Color32::from_rgb(251, 191, 36);
pub const BAD: Color32 = Color32::from_rgb(248, 113, 113);
pub const CYAN: Color32 = Color32::from_rgb(56, 189, 248);
pub const PINK: Color32 = Color32::from_rgb(244, 114, 182);
pub const ORANGE: Color32 = Color32::from_rgb(251, 146, 60);
pub const BLUE: Color32 = Color32::from_rgb(96, 165, 250);

/// Series colors used consistently: CPU, memory, disk, network, GPU.
pub const C_CPU: Color32 = Color32::from_rgb(56, 189, 248);
pub const C_MEM: Color32 = Color32::from_rgb(167, 139, 250);
pub const C_DISK: Color32 = Color32::from_rgb(52, 211, 153);
pub const C_NET: Color32 = Color32::from_rgb(251, 146, 60);
pub const C_GPU: Color32 = Color32::from_rgb(244, 114, 182);

pub fn install(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let win = std::path::Path::new("C:\\Windows\\Fonts");
    fonts.font_data.insert("phosphor".into(), Arc::new(egui::FontData::from_static(include_bytes!("../assets/phosphor.ttf"))));
    let mut prop = Vec::new();
    if let Ok(bytes) = std::fs::read(win.join("segoeui.ttf")) {
        fonts.font_data.insert("segoe".into(), Arc::new(egui::FontData::from_owned(bytes)));
        prop.push("segoe".to_string());
    }
    let bold_src = ["seguisb.ttf", "segoeuib.ttf"].iter().find_map(|f| std::fs::read(win.join(f)).ok());
    if let Some(bytes) = bold_src {
        fonts.font_data.insert("segoe-sb".into(), Arc::new(egui::FontData::from_owned(bytes)));
    }
    let mono = ["CascadiaMono.ttf", "consola.ttf"].iter().find_map(|f| std::fs::read(win.join(f)).ok());
    if let Some(bytes) = mono {
        fonts.font_data.insert("mono".into(), Arc::new(egui::FontData::from_owned(bytes)));
        fonts.families.entry(FontFamily::Monospace).or_default().insert(0, "mono".into());
    }
    let p = fonts.families.entry(FontFamily::Proportional).or_default();
    for (i, n) in prop.into_iter().enumerate() {
        p.insert(i, n);
    }
    p.push("phosphor".into());
    let prop_list = fonts.families[&FontFamily::Proportional].clone();
    let mut bold = Vec::new();
    if fonts.font_data.contains_key("segoe-sb") {
        bold.push("segoe-sb".to_string());
    }
    bold.extend(prop_list);
    fonts.families.insert(FontFamily::Name("bold".into()), bold);
    fonts.families.entry(FontFamily::Monospace).or_default().push("phosphor".into());
    ctx.set_fonts(fonts);

    ctx.all_styles_mut(|s| {
        use egui::TextStyle::*;
        s.text_styles = [
            (Heading, FontId::new(22.0, FontFamily::Name("bold".into()))),
            (Body, FontId::new(13.5, FontFamily::Proportional)),
            (Button, FontId::new(13.5, FontFamily::Proportional)),
            (Small, FontId::new(11.5, FontFamily::Proportional)),
            (Monospace, FontId::new(12.5, FontFamily::Monospace)),
        ]
        .into();
        s.spacing.item_spacing = vec2(8.0, 8.0);
        s.spacing.button_padding = vec2(12.0, 6.0);
        s.spacing.interact_size.y = 30.0;
        s.spacing.scroll.floating = true;
        s.spacing.scroll.bar_width = 7.0;
        s.spacing.scroll.floating_allocated_width = 0.0;
        s.spacing.menu_margin = egui::Margin::same(6);
        s.interaction.tooltip_delay = 0.35;
        s.animation_time = 0.12;
        let v = &mut s.visuals;
        *v = egui::Visuals::dark();
        v.panel_fill = BG;
        v.window_fill = CARD;
        v.extreme_bg_color = FIELD;
        v.faint_bg_color = CARD;
        v.override_text_color = Some(TEXT);
        v.hyperlink_color = ACCENT_HI;
        v.selection.bg_fill = ACCENT.gamma_multiply(0.22);
        v.selection.stroke = Stroke::new(1.0, ACCENT_HI);
        v.window_corner_radius = CornerRadius::same(14);
        v.menu_corner_radius = CornerRadius::same(10);
        v.window_stroke = Stroke::new(1.0, BORDER);
        v.popup_shadow = egui::Shadow { offset: [0, 8], blur: 28, spread: 0, color: Color32::from_black_alpha(150) };
        v.window_shadow = egui::Shadow { offset: [0, 18], blur: 50, spread: 0, color: Color32::from_black_alpha(180) };
        let r = CornerRadius::same(8);
        for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
            w.corner_radius = r;
        }
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER_SOFT);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        v.widgets.inactive.weak_bg_fill = CARD_HI;
        v.widgets.inactive.bg_fill = CARD_HI;
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
        v.widgets.hovered.weak_bg_fill = Color32::from_rgb(36, 38, 54);
        v.widgets.hovered.bg_fill = Color32::from_rgb(36, 38, 54);
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT.gamma_multiply(0.6));
        v.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT);
        v.widgets.active.weak_bg_fill = ACCENT_DIM;
        v.widgets.active.bg_fill = ACCENT_DIM;
        v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
        v.widgets.active.fg_stroke = Stroke::new(1.0, TEXT);
        v.widgets.open.weak_bg_fill = CARD_HI;
        v.text_edit_bg_color = Some(FIELD);
        v.striped = false;
    });
}

pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("bold".into()))
}

pub fn card() -> egui::Frame {
    egui::Frame::new().fill(CARD).stroke(Stroke::new(1.0, BORDER_SOFT)).corner_radius(CornerRadius::same(12)).inner_margin(egui::Margin::same(16))
}

pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()), l(a.a(), b.a()))
}

/// Background tint for a "heat" cell: 0 = none, 1 = strong.
pub fn heat(color: Color32, t: f32) -> Color32 {
    if t <= 0.002 {
        return Color32::TRANSPARENT;
    }
    color.gamma_multiply(0.05 + 0.32 * t.clamp(0.0, 1.0).sqrt())
}

#[derive(Clone, Copy, PartialEq)]
pub enum Btn {
    Primary,
    Ghost,
    Danger,
    Subtle,
}

pub fn button(ui: &mut Ui, kind: Btn, icon: &str, label: &str, enabled: bool) -> Response {
    button_sized(ui, kind, icon, label, enabled, 32.0)
}

pub fn button_sized(ui: &mut Ui, kind: Btn, icon: &str, label: &str, enabled: bool, h: f32) -> Response {
    let font = if kind == Btn::Primary { bold(13.5) } else { FontId::proportional(13.5) };
    let text = if label.is_empty() { icon.to_string() } else if icon.is_empty() { label.to_string() } else { format!("{icon}  {label}") };
    let galley = ui.painter().layout_no_wrap(text, font, TEXT);
    let pad = if label.is_empty() { vec2(8.0, 6.0) } else { vec2(13.0, 7.0) };
    let size = vec2((galley.size().x + pad.x * 2.0).max(if label.is_empty() { h } else { 0.0 }), h);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    let hover = ui.ctx().animate_bool_with_time(resp.id, enabled && resp.hovered(), 0.1);
    let press = enabled && resp.is_pointer_button_down_on();
    let (fill, fill_hi, stroke, fg) = match kind {
        Btn::Primary => (ACCENT, ACCENT_HI, Color32::TRANSPARENT, Color32::WHITE),
        Btn::Ghost => (CARD_HI, Color32::from_rgb(40, 42, 60), BORDER, TEXT),
        Btn::Danger => (Color32::from_rgb(58, 24, 32), Color32::from_rgb(84, 30, 40), Color32::from_rgb(104, 40, 52), BAD),
        Btn::Subtle => (Color32::TRANSPARENT, CARD_HI, Color32::TRANSPARENT, MUTED),
    };
    let mut f = lerp_color(fill, fill_hi, hover);
    let mut fg = if kind == Btn::Subtle { lerp_color(fg, TEXT, hover) } else { fg };
    if !enabled {
        f = f.gamma_multiply(0.45);
        fg = fg.gamma_multiply(0.45);
    }
    let r = if press { rect.shrink(1.0) } else { rect };
    if kind == Btn::Primary && enabled && hover > 0.0 {
        ui.painter().rect_filled(r.expand(3.0 * hover), CornerRadius::same(11), ACCENT.gamma_multiply(0.16 * hover));
    }
    ui.painter().rect(r, CornerRadius::same(8), f, Stroke::new(1.0, stroke), StrokeKind::Inside);
    ui.painter().galley_with_override_text_color(r.center() - galley.size() / 2.0, galley, fg);
    if enabled { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp }
}

/// Square icon-only button with tooltip.
pub fn icon_button(ui: &mut Ui, icon: &str, tip: &str, active: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::click());
    let hover = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
    let bg = if active { ACCENT_DIM } else { CARD_HI.gamma_multiply(hover) };
    ui.painter().rect_filled(rect, CornerRadius::same(8), bg);
    let fg = if active { ACCENT_HI } else { lerp_color(MUTED, TEXT, hover) };
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, icon, FontId::proportional(16.0), fg);
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    if tip.is_empty() { resp } else { resp.on_hover_text(tip) }
}

pub fn pill(ui: &mut Ui, text: &str, color: Color32) -> Response {
    let galley = ui.painter().layout_no_wrap(text.to_string(), bold(10.5), color);
    let size = galley.size() + vec2(12.0, 5.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(255), color.gamma_multiply(0.15));
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, color);
    resp
}

/// Filter chip: toggles on click.
pub fn chip(ui: &mut Ui, on: &mut bool, label: &str, count: Option<usize>) -> Response {
    let text = match count {
        Some(n) => format!("{label}  {n}"),
        None => label.to_string(),
    };
    let galley = ui.painter().layout_no_wrap(text, bold(12.0), TEXT);
    let (rect, resp) = ui.allocate_exact_size(galley.size() + vec2(20.0, 12.0), Sense::click());
    let hover = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
    let t = ui.ctx().animate_bool_with_time(resp.id.with("on"), *on, 0.12);
    let fill = lerp_color(lerp_color(FIELD, CARD_HI, hover), ACCENT_DIM, t);
    let stroke = lerp_color(BORDER, ACCENT.gamma_multiply(0.8), t);
    ui.painter().rect(rect, CornerRadius::same(255), fill, Stroke::new(1.0, stroke), StrokeKind::Inside);
    ui.painter().galley_with_override_text_color(rect.center() - galley.size() / 2.0, galley, lerp_color(MUTED, TEXT, t.max(hover * 0.6)));
    if resp.clicked() {
        *on = !*on;
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Segmented control. Returns true if changed.
pub fn segmented(ui: &mut Ui, selected: &mut usize, labels: &[&str]) -> bool {
    // Measure first and allocate one exact rect, so it looks right in any layout direction.
    let mut changed = false;
    let widths: Vec<f32> = labels.iter().map(|l| ui.painter().layout_no_wrap(l.to_string(), bold(12.5), TEXT).size().x + 22.0).collect();
    let gap = 2.0;
    let total = vec2(widths.iter().sum::<f32>() + gap * (labels.len() as f32 - 1.0) + 6.0, 32.0);
    let (outer, _) = ui.allocate_exact_size(total, Sense::hover());
    ui.painter().rect(outer, CornerRadius::same(9), FIELD, Stroke::new(1.0, BORDER), StrokeKind::Inside);
    let mut x = outer.left() + 3.0;
    let mut rects = Vec::with_capacity(labels.len());
    for (i, w) in widths.iter().enumerate() {
        let rect = Rect::from_min_size(egui::pos2(x, outer.top() + 3.0), vec2(*w, 26.0));
        x += w + gap;
        let resp = ui.interact(rect, ui.scope_id().with(("seg", labels[0], i)), Sense::click());
        let hover = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.1);
        if i != *selected && hover > 0.0 {
            ui.painter().rect_filled(rect, CornerRadius::same(7), CARD_HI.gamma_multiply(hover));
        }
        if resp.clicked() && *selected != i {
            *selected = i;
            changed = true;
        }
        resp.on_hover_cursor(egui::CursorIcon::PointingHand);
        rects.push(rect);
    }
    let id = ui.scope_id().with(("seg-hl", labels[0]));
    let target = rects[(*selected).min(rects.len() - 1)];
    let x0 = ui.ctx().animate_value_with_time(id.with(0), target.left(), 0.15);
    let x1 = ui.ctx().animate_value_with_time(id.with(1), target.right(), 0.15);
    ui.painter().rect_filled(Rect::from_x_y_ranges(x0..=x1, target.y_range()), CornerRadius::same(7), ACCENT);
    for (i, rect) in rects.iter().enumerate() {
        let color = if i == *selected { Color32::WHITE } else { MUTED };
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, labels[i], bold(12.5), color);
    }
    changed
}

/// iOS-style switch. Returns true if toggled.
pub fn toggle(ui: &mut Ui, on: &mut bool) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(vec2(38.0, 22.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool_with_time(resp.id, *on, 0.14);
    let bg = lerp_color(Color32::from_rgb(52, 55, 74), ACCENT, t);
    ui.painter().rect_filled(rect, CornerRadius::same(255), bg);
    let x = egui::lerp(rect.left() + 11.0..=rect.right() - 11.0, t);
    ui.painter().circle_filled(egui::pos2(x, rect.center().y), 8.0, Color32::WHITE);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Settings row: title + description on the left, control on the right.
pub fn setting_row(ui: &mut Ui, title: &str, desc: &str, add: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.set_min_height(44.0);
        let w = ui.available_width();
        ui.vertical(|ui| {
            ui.set_width((w - 200.0).max(200.0));
            ui.label(RichText::new(title).font(bold(13.5)));
            if !desc.is_empty() {
                ui.label(RichText::new(desc).size(12.0).color(MUTED));
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), add);
    });
}

pub fn label_caps(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text.to_uppercase()).font(bold(10.5)).color(FAINT).extra_letter_spacing(0.9));
}

/// Search / text field with icon and focus ring.
pub fn field(ui: &mut Ui, id: egui::Id, icon: &str, value: &mut String, hint: &str, width: f32) -> Response {
    let mut out = None;
    let focused = ui.memory(|m| m.has_focus(id));
    let f = ui.ctx().animate_bool_with_time(id.with("focus"), focused, 0.12);
    egui::Frame::new().fill(FIELD).stroke(Stroke::new(1.0, lerp_color(BORDER, ACCENT, f))).corner_radius(CornerRadius::same(9)).inner_margin(egui::Margin::symmetric(10, 2)).show(ui, |ui| {
        ui.set_width(width - 22.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if !icon.is_empty() {
                ui.label(RichText::new(icon).size(15.0).color(lerp_color(FAINT, ACCENT_HI, f)));
            }
            let clear_w = if value.is_empty() { 0.0 } else { 22.0 };
            let te = egui::TextEdit::singleline(value).id(id).hint_text(RichText::new(hint).color(FAINT)).frame(egui::Frame::NONE).margin(Vec2::new(0.0, 6.0)).desired_width(ui.available_width() - clear_w);
            out = Some(ui.add(te));
            if !value.is_empty() {
                let r = ui.add(egui::Label::new(RichText::new(crate::icons::X_CIRCLE).size(15.0).color(FAINT)).sense(Sense::click())).on_hover_cursor(egui::CursorIcon::PointingHand);
                if r.clicked() {
                    value.clear();
                }
            }
        });
    });
    out.unwrap()
}

/// Thin progress/usage bar.
pub fn bar(ui: &mut Ui, frac: f32, color: Color32, width: f32, height: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    paint_bar(ui, rect, frac, color);
}

pub fn paint_bar(ui: &Ui, rect: Rect, frac: f32, color: Color32) {
    let r = CornerRadius::same(255);
    ui.painter().rect_filled(rect, r, Color32::from_rgb(34, 36, 50));
    let f = frac.clamp(0.0, 1.0);
    if f > 0.0 {
        let fill = Rect::from_min_size(rect.min, vec2((rect.width() * f).max(rect.height()), rect.height()));
        ui.painter().rect_filled(fill, r, color);
    }
}

/// Indeterminate spinner.
pub fn spinner(ui: &mut Ui, size: f32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    let t = ui.input(|i| i.time) as f32;
    let c = rect.center();
    let r = size * 0.4;
    let n = 24;
    let start = t * 5.0;
    let pts: Vec<egui::Pos2> = (0..=n).map(|i| {
        let a = start + i as f32 / n as f32 * 4.2;
        c + vec2(a.cos(), a.sin()) * r
    }).collect();
    ui.painter().add(egui::Shape::line(pts, Stroke::new(2.0, color)));
    ui.ctx().request_repaint();
}

pub fn empty_state(ui: &mut Ui, icon: &str, title: &str, desc: &str) {
    ui.vertical_centered(|ui| {
        ui.add_space(50.0);
        ui.label(RichText::new(icon).size(40.0).color(FAINT));
        ui.add_space(6.0);
        ui.label(RichText::new(title).font(bold(16.0)));
        ui.label(RichText::new(desc).color(MUTED));
    });
}

/// Paint a texture icon or a fallback glyph inside `rect`.
pub fn paint_icon(ui: &Ui, rect: Rect, tex: Option<&egui::TextureHandle>, fallback: &str) {
    match tex {
        Some(t) => {
            ui.painter().image(t.id(), rect, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
        }
        None => {
            ui.painter().rect_filled(rect, CornerRadius::same(5), CARD_HI);
            ui.painter().text(rect.center(), Align2::CENTER_CENTER, fallback, FontId::proportional(rect.height() * 0.68), FAINT);
        }
    }
}
