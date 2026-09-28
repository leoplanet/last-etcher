//! Design tokens + global egui style. One place for the whole look.

/// Dracula palette (https://draculatheme.com) — a famous, cohesive dark theme.
pub mod colors {
    use egui::Color32;

    pub const WINDOW: Color32 = Color32::from_rgb(40, 42, 54);     // #282a36
    pub const SURFACE: Color32 = Color32::from_rgb(44, 47, 61);    // #2c2f3d
    pub const RAISED: Color32 = Color32::from_rgb(50, 53, 69);     // #323545
    pub const RAISED_HOVER: Color32 = Color32::from_rgb(58, 61, 79); // #3a3d4f
    pub const BORDER: Color32 = Color32::from_rgb(68, 71, 90);     // #44475a
    pub const ACCENT: Color32 = Color32::from_rgb(189, 147, 249);  // #bd93f9 purple
    pub const ACCENT_HOVER: Color32 = Color32::from_rgb(209, 169, 251); // #d1a9fb
    pub const ACCENT_DIM: Color32 = Color32::from_rgb(96, 72, 140); // #60488c
    pub const SUCCESS: Color32 = Color32::from_rgb(80, 250, 123);  // #50fa7b green
    pub const DANGER: Color32 = Color32::from_rgb(255, 85, 85);    // #ff5555 red
    pub const DANGER_DEEP: Color32 = Color32::from_rgb(200, 60, 60); // #c83c3c
    pub const DANGER_TINT: Color32 = Color32::from_rgb(70, 40, 44);
    pub const SUCCESS_TINT: Color32 = Color32::from_rgb(38, 66, 50);
    pub const WARN: Color32 = Color32::from_rgb(241, 250, 140);    // #f1fa8c yellow
    pub const TEXT: Color32 = Color32::from_rgb(248, 248, 242);    // #f8f8f2
    pub const TEXT_2: Color32 = Color32::from_rgb(156, 160, 176);  // #9ca0b0
    pub const TEXT_3: Color32 = Color32::from_rgb(98, 114, 164);   // #6272a4 comment
}

pub const RADIUS_CARD: f32 = 12.0;
pub const RADIUS_INPUT: f32 = 8.0;
pub const RADIUS_CHIP: f32 = 6.0;

/// Type scale — the ONLY font sizes used in the UI, so hierarchy is consistent.
pub mod type_scale {
    pub const TITLE: f32 = 18.0;   // app name
    pub const HEADING: f32 = 16.0; // card title
    pub const BODY: f32 = 14.0;    // primary text / values
    pub const LABEL: f32 = 12.5;   // secondary text / sub-labels
    pub const CAPTION: f32 = 11.5; // small text / hints
    pub const BADGE: f32 = 10.5;   // pill badges
}

#[inline(always)]
pub fn font(size: f32) -> egui::FontId {
    egui::FontId::proportional(size)
}
#[inline(always)]
pub fn mono(size: f32) -> egui::FontId {
    egui::FontId::monospace(size)
}

/// Ease-out cubic.
pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// Same RGB, with an explicit alpha (0..=255).
pub fn tint(c: egui::Color32, a: u8) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
}

pub fn apply(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "Inter".to_owned(),
        egui::FontData::from_static(include_bytes!("../assets/Inter-Regular.ttf")),
    );
    fonts
        .families
        .insert(egui::FontFamily::Proportional, vec!["Inter".to_owned()]);
    ctx.set_fonts(fonts);

    let mut style = (*ctx.style()).clone();
    style.visuals = visuals();
    style.spacing.item_spacing = egui::vec2(10.0, 10.0);
    style.spacing.button_padding = egui::vec2(12.0, 7.0);
    style.text_styles = std::collections::BTreeMap::from([
        (egui::TextStyle::Heading, egui::FontId::proportional(20.0)),
        (egui::TextStyle::Body, egui::FontId::proportional(14.0)),
        (egui::TextStyle::Button, egui::FontId::proportional(13.5)),
        (egui::TextStyle::Small, egui::FontId::proportional(12.0)),
        (egui::TextStyle::Monospace, egui::FontId::monospace(13.0)),
    ]);
    ctx.set_style(style);
}

fn visuals() -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    v.dark_mode = true;
    v.override_text_color = Some(colors::TEXT);
    v.window_fill = colors::WINDOW;
    v.panel_fill = colors::WINDOW;
    v.extreme_bg_color = colors::SURFACE;
    v.faint_bg_color = colors::SURFACE;

    v.selection.bg_fill = colors::ACCENT;
    v.selection.stroke = egui::Stroke::NONE;
    v.hyperlink_color = colors::ACCENT;

    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
    ] {
        w.rounding = egui::Rounding::same(RADIUS_INPUT);
    }
    v.widgets.noninteractive.bg_fill = colors::RAISED;
    v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, colors::BORDER);
    v.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0_f32, colors::TEXT_2);
    v.widgets.inactive.bg_fill = colors::RAISED;
    v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, colors::BORDER);
    v.widgets.inactive.fg_stroke = egui::Stroke::new(1.0_f32, colors::TEXT_2);
    v.widgets.inactive.weak_bg_fill = colors::SURFACE;
    v.widgets.hovered.bg_fill = colors::RAISED_HOVER;
    v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, colors::ACCENT);
    v.widgets.hovered.fg_stroke = egui::Stroke::new(1.0_f32, colors::TEXT);
    v.widgets.hovered.weak_bg_fill = colors::RAISED;
    v.widgets.active.bg_fill = colors::RAISED_HOVER;
    v.widgets.active.bg_stroke = egui::Stroke::new(1.0_f32, colors::ACCENT);
    v.widgets.active.fg_stroke = egui::Stroke::new(1.0_f32, colors::TEXT);
    v
}
