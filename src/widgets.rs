//! Hand-painted controls for the launcher.

use crate::theme::*;
use eframe::egui::{self, pos2, vec2, Align2, Color32, CornerRadius, Rect, Response, Sense, Stroke, StrokeKind};

fn focus_ring(ui: &egui::Ui, r: &Response, radius: f32) {
    if r.has_focus() {
        ui.painter().rect_stroke(r.rect.expand(3.0), radius + 3.0, Stroke::new(2.0, LAMP), StrokeKind::Outside);
    }
}

/// Full-window dusk gradient with a low streetlamp glow along the bottom edge.
pub fn backdrop(p: &egui::Painter, r: Rect) {
    let mut m = egui::Mesh::default();
    let stops = [(0.0, SKY_TOP), (0.72, SKY_LOW), (1.0, Color32::from_rgb(0x3A, 0x2A, 0x3E))];
    for (i, (t, c)) in stops.iter().enumerate() {
        let y = r.top() + r.height() * t;
        m.colored_vertex(pos2(r.left(), y), *c);
        m.colored_vertex(pos2(r.right(), y), *c);
        if i > 0 {
            let b = (i as u32) * 2;
            m.add_triangle(b - 2, b - 1, b);
            m.add_triangle(b - 1, b, b + 1);
        }
    }
    p.add(egui::Shape::mesh(m));
}

pub fn source_row(ui: &mut egui::Ui, selected: bool, title: &str, detail: &str, available: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 64.0), Sense::click());
    let t = ui.ctx().animate_bool_with_time(resp.id, selected, 0.15);
    let p = ui.painter();
    let fill = if resp.hovered() && !selected { PANEL_HI.gamma_multiply(0.7) } else { PANEL_HI.gamma_multiply(t) };
    p.rect_filled(rect, 12.0, fill);
    if t > 0.0 {
        let bar = Rect::from_min_size(rect.min + vec2(0.0, 14.0), vec2(4.0, (rect.height() - 28.0) * t));
        p.rect_filled(bar, 2.0, LAMP);
    }
    let color = match (selected, available) {
        (true, _) => LAMP,
        (false, true) => PAPER,
        (false, false) => HAZE,
    };
    p.text(rect.min + vec2(20.0, 9.0), Align2::LEFT_TOP, title, display(27.0), color);
    p.text(rect.min + vec2(20.0, 41.0), Align2::LEFT_TOP, detail, egui::FontId::proportional(14.0), HAZE);
    focus_ring(ui, &resp, 12.0);
    resp
}

/// Two or more options with an amber pill that slides to the chosen one.
pub fn segmented<T: PartialEq + Copy>(ui: &mut egui::Ui, current: &mut T, options: &[(T, &str)]) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::hover());
    let seg = rect.width() / options.len() as f32;
    let idx = options.iter().position(|(v, _)| v == current).unwrap_or(0) as f32;
    let x = ui.ctx().animate_value_with_time(ui.scope_id().with(rect.min.y as i32), idx, 0.15);
    ui.painter().rect_filled(rect, 12.0, DEEP);
    let pill = Rect::from_min_size(rect.min + vec2(x * seg + 4.0, 4.0), vec2(seg - 8.0, rect.height() - 8.0));
    ui.painter().rect_filled(pill, 9.0, LAMP);
    for (i, (value, label)) in options.iter().enumerate() {
        let r = Rect::from_min_size(rect.min + vec2(i as f32 * seg, 0.0), vec2(seg, rect.height()));
        let resp = ui.interact(r, ui.scope_id().with(("seg", label)), Sense::click());
        if resp.clicked() {
            *current = *value;
        }
        let color = if *value == *current { DUSK } else if resp.hovered() { PAPER } else { HAZE };
        ui.painter().text(r.center(), Align2::CENTER_CENTER, *label, egui::FontId::proportional(17.0), color);
        focus_ring(ui, &resp, 9.0);
    }
}

pub fn toggle(ui: &mut egui::Ui, on: &mut bool, label: &str) {
    let (rect, mut resp) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool_with_time(resp.id, *on, 0.12);
    let p = ui.painter();
    p.text(pos2(rect.left(), rect.center().y), Align2::LEFT_CENTER, label, egui::FontId::proportional(16.0), PAPER);
    let track = Rect::from_center_size(pos2(rect.right() - 22.0, rect.center().y), vec2(44.0, 24.0));
    p.rect_filled(track, 12.0, lerp_color(LINE, LAMP, t));
    let knob_x = egui::lerp(track.left() + 12.0..=track.right() - 12.0, t);
    p.circle_filled(pos2(knob_x, track.center().y), 9.0, if *on { DUSK } else { PAPER });
    focus_ring(ui, &resp, 12.0);
}

pub fn start_button(ui: &mut egui::Ui, enabled: bool, label: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 60.0), Sense::click());
    let p = ui.painter();
    let fill = match (enabled, resp.hovered()) {
        (false, _) => LINE,
        (true, true) => LAMP_HI,
        (true, false) => LAMP,
    };
    p.rect_filled(rect, CornerRadius::same(14), fill);
    let ink = if enabled { DUSK } else { HAZE };
    p.text(pos2(rect.left() + 22.0, rect.center().y), Align2::LEFT_CENTER, label, display(26.0), ink);
    if enabled {
        let key = Rect::from_center_size(pos2(rect.right() - 44.0, rect.center().y), vec2(60.0, 28.0));
        p.rect_stroke(key, 7.0, Stroke::new(1.5, ink.gamma_multiply(0.5)), StrokeKind::Inside);
        p.text(key.center(), Align2::CENTER_CENTER, "Enter", egui::FontId::proportional(14.0), ink);
    }
    focus_ring(ui, &resp, 14.0);
    if enabled { resp } else { resp.on_hover_text("Pick a working source first") }
}

pub fn status_chip(ui: &egui::Ui, right_center: egui::Pos2, text: &str, ok: bool) {
    let font = egui::FontId::proportional(15.0);
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, PAPER);
    let w = galley.size().x + 44.0;
    let r = Rect::from_center_size(pos2(right_center.x - w / 2.0, right_center.y), vec2(w, 34.0));
    ui.painter().rect_filled(r, 17.0, PANEL);
    ui.painter().circle_filled(pos2(r.left() + 18.0, r.center().y), 4.5, if ok { SIGNAL } else { FLARE });
    ui.painter().galley(pos2(r.left() + 30.0, r.center().y - galley.size().y / 2.0), galley, PAPER);
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}
