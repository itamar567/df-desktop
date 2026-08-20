use egui::{Color32, CornerRadius, FontId, Stroke, TextStyle};

pub const APP_BACKGROUND: Color32 = Color32::from_rgb(0x66, 0x00, 0x00);
pub const ACCENT: Color32 = Color32::from_rgb(0xC0, 0x8A, 0x47);
pub const LOG_BACKGROUND: Color32 = Color32::from_rgb(0x1A, 0x04, 0x04);
pub const LOG_TEXT: Color32 = Color32::from_rgb(0xF4, 0xE8, 0xD0);
pub const LOG_BORDER: Color32 = Color32::from_rgb(0x8D, 0x5E, 0x2F);

pub fn configure(ctx: &egui::Context) {
    let ivory = LOG_TEXT;
    let bronze = ACCENT;
    let mut style = (*ctx.style()).clone();
    style.visuals = egui::Visuals::dark();
    style.visuals.override_text_color = Some(ivory);
    style.visuals.panel_fill = APP_BACKGROUND;
    style.visuals.window_fill = APP_BACKGROUND;
    style.visuals.extreme_bg_color = Color32::from_rgb(0x38, 0x08, 0x08);
    style.visuals.faint_bg_color = Color32::from_rgb(0x5A, 0x00, 0x00);
    style.visuals.selection.bg_fill = bronze;
    style.visuals.selection.stroke = Stroke::new(1.0_f32, ivory);

    style.visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(0x3E, 0x0B, 0x0B);
    style.visuals.widgets.inactive.bg_fill = Color32::from_rgb(0x3E, 0x0B, 0x0B);
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, bronze);
    style.visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, ivory);
    style.visuals.widgets.inactive.corner_radius = CornerRadius::same(5);

    style.visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x86, 0x18, 0x18);
    style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(0x86, 0x18, 0x18);
    style.visuals.widgets.hovered.bg_stroke =
        Stroke::new(1.5_f32, Color32::from_rgb(0xE2, 0xB8, 0x70));
    style.visuals.widgets.hovered.fg_stroke = Stroke::new(1.5_f32, Color32::WHITE);
    style.visuals.widgets.hovered.corner_radius = CornerRadius::same(5);

    style.visuals.widgets.active.weak_bg_fill = Color32::from_rgb(0x2B, 0x06, 0x06);
    style.visuals.widgets.active.bg_fill = Color32::from_rgb(0x2B, 0x06, 0x06);
    style.visuals.widgets.active.bg_stroke = Stroke::new(1.5_f32, bronze);
    style.visuals.widgets.active.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
    style.visuals.widgets.active.corner_radius = CornerRadius::same(5);

    style.spacing.button_padding = egui::vec2(16.0, 9.0);
    style
        .text_styles
        .insert(TextStyle::Heading, FontId::proportional(30.0));
    style
        .text_styles
        .insert(TextStyle::Body, FontId::proportional(16.0));
    style
        .text_styles
        .insert(TextStyle::Button, FontId::proportional(16.0));
    ctx.set_style(style);
}
