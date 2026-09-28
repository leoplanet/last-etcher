//! Dependency-free vector glyphs drawn with egui shapes.

use egui::{Color32, Painter, Pos2, Rect, Vec2};

/// Map unit-square points into a rect.
fn map(pts: &[(f32, f32)], rect: Rect) -> Vec<Pos2> {
    pts.iter()
        .map(|(x, y)| rect.min + Vec2::new(*x * rect.width(), *y * rect.height()))
        .collect()
}

/// Disk drive: rounded slab with an activity dot.
pub fn disk(p: &Painter, rect: Rect, color: Color32) {
    let r = egui::Rounding::same(2.5);
    p.rect_stroke(rect, r, egui::Stroke::new(1.6_f32, color));
    let dot = rect.center() + Vec2::new(rect.width() * 0.28, rect.height() * 0.28);
    p.circle_filled(dot, 1.6, color);
}

/// File/page with a folded corner.
pub fn file(p: &Painter, rect: Rect, color: Color32) {
    let w = rect.width();
    let h = rect.height();
    p.rect_stroke(rect, 2.0, egui::Stroke::new(1.6_f32, color));
    // folded corner
    p.line_segment(
        [
            rect.min + Vec2::new(w * 0.65, 0.0),
            rect.min + Vec2::new(w * 0.65, h * 0.22),
        ],
        egui::Stroke::new(1.4_f32, color),
    );
    p.line_segment(
        [
            rect.min + Vec2::new(w * 0.65, h * 0.22),
            rect.min + Vec2::new(w * 0.85, h * 0.22),
        ],
        egui::Stroke::new(1.4_f32, color),
    );
}

/// Lightning bolt (two triangles), filled.
pub fn bolt(p: &Painter, rect: Rect, color: Color32) {
    let top = map(&[(0.55, 0.0), (0.12, 0.55), (0.47, 0.55)], rect);
    let bottom = map(&[(0.47, 0.45), (0.38, 1.0), (0.88, 0.45)], rect);
    p.add(egui::Shape::convex_polygon(top, color, egui::epaint::PathStroke::NONE));
    p.add(egui::Shape::convex_polygon(bottom, color, egui::epaint::PathStroke::NONE));
}

/// Checkmark. `t` in 0..1 animates the draw-on.
pub fn check(p: &Painter, rect: Rect, color: Color32, t: f32) {
    let a = rect.min + Vec2::new(rect.width() * 0.18, rect.height() * 0.55);
    let m = rect.min + Vec2::new(rect.width() * 0.42, rect.height() * 0.75);
    let b = rect.min + Vec2::new(rect.width() * 0.85, rect.height() * 0.25);
    let seg1 = (m - a).length().max(0.001);
    let seg2 = (b - m).length().max(0.001);
    let total = seg1 + seg2;
    let dist = t.clamp(0.0, 1.0) * total;
    if dist <= seg1 {
        p.line_segment([a, a + (m - a) * (dist / seg1)], egui::Stroke::new(2.4_f32, color));
    } else {
        p.line_segment([a, m], egui::Stroke::new(2.4_f32, color));
        p.line_segment([m, m + (b - m) * ((dist - seg1) / seg2)], egui::Stroke::new(2.4_f32, color));
    }
}

/// X mark. `t` animates the draw-on.
pub fn x_mark(p: &Painter, rect: Rect, color: Color32, t: f32) {
    let t = t.clamp(0.0, 1.0);
    let a = rect.min + Vec2::new(rect.width() * 0.2, rect.height() * 0.2);
    let b = rect.min + Vec2::new(rect.width() * 0.8, rect.height() * 0.8);
    let c = rect.min + Vec2::new(rect.width() * 0.8, rect.height() * 0.2);
    let d = rect.min + Vec2::new(rect.width() * 0.2, rect.height() * 0.8);
    p.line_segment([a, a + (b - a) * t], egui::Stroke::new(2.4_f32, color));
    p.line_segment([c, c + (d - c) * t], egui::Stroke::new(2.4_f32, color));
}

/// Warning triangle with an exclamation mark.
pub fn warn(p: &Painter, rect: Rect, color: Color32) {
    let pts = map(&[(0.5, 0.05), (0.95, 0.95), (0.05, 0.95)], rect);
    p.add(egui::Shape::convex_polygon(pts, color, egui::epaint::PathStroke::NONE));
    // exclamation (cut out with the card color is not possible here; draw dark)
    let dark = Color32::from_rgb(14, 16, 21);
    let bar = Rect::from_center_size(
        rect.center() + Vec2::new(0.0, rect.height() * 0.05),
        Vec2::new(rect.width() * 0.09, rect.height() * 0.38),
    );
    p.rect(bar, 2.0, dark, egui::Stroke::NONE);
    p.circle_filled(
        rect.center() + Vec2::new(0.0, rect.height() * 0.34),
        rect.width() * 0.06,
        dark,
    );
}

/// Small filled circle (status dot).
pub fn dot(p: &Painter, center: Pos2, radius: f32, color: Color32) {
    p.circle_filled(center, radius, color);
}

/// A small soft "glisten" sweeping across a progress bar's filled portion.
/// `x` is the highlight center (pixels from the bar's left edge).
pub fn glisten(p: &Painter, bar: Rect, x: f32) {
    let cx = bar.min.x + x;
    let cy = bar.center().y;
    let h = bar.height();
    // Soft gaussian-ish profile: a few narrow rects, brightest at the center.
    let layers: [(f32, f32, u8); 5] = [
        (14.0, 10.0, 10),
        (10.0, 8.0, 16),
        (6.0, 6.0, 26),
        (3.0, 5.0, 34),
        (1.5, 4.0, 44),
    ];
    for (w, _h, a) in layers {
        let r = Rect::from_center_size(Pos2::new(cx, cy), Vec2::new(w, h));
        if r.intersects(bar) {
            p.rect(r, h / 2.0, Color32::from_rgba_premultiplied(255, 255, 255, a), egui::Stroke::NONE);
        }
    }
}

/// Rotating arc spinner. `phase` in 0..1 drives the rotation.
pub fn spinner(p: &Painter, center: Pos2, radius: f32, color: Color32, phase: f32) {
    use std::f32::consts::PI;
    let start = phase * 2.0 * PI;
    let end = start + 1.5 * PI; // 270° arc
    let segs = 24;
    let mut prev = center + Vec2::new(start.cos() * radius, start.sin() * radius);
    for i in 1..=segs {
        let a = start + (end - start) * (i as f32 / segs as f32);
        let pt = center + Vec2::new(a.cos() * radius, a.sin() * radius);
        p.line_segment([prev, pt], egui::Stroke::new(2.0_f32, color));
        prev = pt;
    }
}
