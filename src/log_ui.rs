use egui::{Color32, Frame, Margin, ScrollArea, Stroke};

use crate::log::{LogDisplay, LogSnapshot, LogTab, SharedLogState, parse_log_content};
use crate::theme::{ACCENT, LOG_BORDER, LOG_TEXT};

pub const DEFAULT_PANEL_WIDTH: f32 = 320.0;
const MIN_PANEL_WIDTH: f32 = 300.0;
const MAX_PANEL_WIDTH: f32 = 800.0;

#[derive(Debug, Default)]
pub struct LogUiResponse {
    selected_tab: Option<LogTab>,
    close_requested: bool,
}

impl LogUiResponse {
    pub fn selected_tab(&self) -> Option<LogTab> {
        self.selected_tab
    }

    pub fn apply(self, logs: &SharedLogState) {
        if let Some(tab) = self.selected_tab {
            logs.set_tab(tab);
        }
        if self.close_requested {
            logs.hide();
        }
    }
}

pub struct SidePanelOutput {
    pub width: f32,
    pub response: LogUiResponse,
}

pub fn side_panel(
    ctx: &egui::Context,
    logs: &SharedLogState,
    width: f32,
) -> Option<SidePanelOutput> {
    if logs.display() != LogDisplay::SideBySide {
        return None;
    }

    let snapshot = logs.snapshot();
    let panel = egui::SidePanel::right("log_panel")
        .resizable(true)
        .default_width(width)
        .width_range(MIN_PANEL_WIDTH..=MAX_PANEL_WIDTH)
        .frame(
            Frame::new()
                .fill(Color32::from_rgba_unmultiplied(0x1A, 0x04, 0x04, 245))
                .stroke(Stroke::new(1.0_f32, LOG_BORDER))
                .inner_margin(Margin::symmetric(8, 8)),
        )
        .show(ctx, |ui| content(ui, &snapshot, true));

    Some(SidePanelOutput {
        width: panel.response.rect.width(),
        response: panel.inner,
    })
}

pub fn content(
    ui: &mut egui::Ui,
    snapshot: &LogSnapshot,
    show_close_button: bool,
) -> LogUiResponse {
    let mut response = LogUiResponse::default();

    if show_close_button {
        tabs_with_close_button(ui, snapshot.tab, &mut response);
    } else {
        centered_tab_buttons(ui, snapshot.tab, &mut response);
    }
    ui.separator();

    ScrollArea::vertical()
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            for fragment in parse_log_content(&snapshot.content) {
                let color = fragment
                    .color
                    .map(|[r, g, b]| Color32::from_rgb(r, g, b))
                    .unwrap_or(LOG_TEXT);
                ui.label(
                    egui::RichText::new(fragment.text)
                        .color(color)
                        .monospace()
                        .size(12.0),
                );
            }
        });

    response
}

fn centered_tab_buttons(ui: &mut egui::Ui, selected_tab: LogTab, response: &mut LogUiResponse) {
    let leading_space = ((ui.available_width() - tab_buttons_width(ui)) / 2.0_f32).max(0.0_f32);
    ui.horizontal(|ui| {
        ui.add_space(leading_space);
        tab_buttons(ui, selected_tab, response);
    });
}

fn tabs_with_close_button(ui: &mut egui::Ui, selected_tab: LogTab, response: &mut LogUiResponse) {
    let available_width = ui.available_width();
    let tabs_width = tab_buttons_width(ui);
    let close_width = close_button_size(ui).x;
    let item_spacing = ui.spacing().item_spacing.x;
    let centered_space = (available_width - tabs_width) / 2.0_f32;
    let reserved_space = available_width - tabs_width - close_width - item_spacing;
    let leading_space = centered_space.min(reserved_space).max(0.0_f32);

    ui.horizontal(|ui| {
        ui.add_space(leading_space);
        tab_buttons(ui, selected_tab, response);
        ui.add_space((ui.available_width() - close_width).max(0.0_f32));
        response.close_requested = close_button(ui).on_hover_text("Close log").clicked();
    });
}

fn close_button(ui: &mut egui::Ui) -> egui::Response {
    use egui::{Align2, FontId, Sense, StrokeKind};

    let (rect, response) = ui.allocate_exact_size(close_button_size(ui), Sense::click());
    let visuals = ui.style().interact(&response);
    ui.painter().rect(
        rect,
        visuals.corner_radius,
        visuals.weak_bg_fill,
        visuals.bg_stroke,
        StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        "×",
        FontId::proportional(22.0_f32),
        ACCENT,
    );
    response
}

fn close_button_size(ui: &egui::Ui) -> egui::Vec2 {
    let font = egui::TextStyle::Button.resolve(ui.style());
    egui::vec2(
        text_button_width(ui, "×", font.clone()),
        (font.size + 2.0_f32 * ui.spacing().button_padding.y).max(ui.spacing().interact_size.y),
    )
}

fn tab_buttons_width(ui: &egui::Ui) -> f32 {
    let font = egui::TextStyle::Button.resolve(ui.style());
    text_button_width(ui, "Game Log", font.clone())
        + ui.spacing().item_spacing.x
        + text_button_width(ui, "Battle Log", font)
}

fn text_button_width(ui: &egui::Ui, text: &str, font: egui::FontId) -> f32 {
    let text_width = ui
        .painter()
        .layout_no_wrap(text.to_string(), font, Color32::WHITE)
        .size()
        .x;
    (text_width + 2.0_f32 * ui.spacing().button_padding.x).max(ui.spacing().interact_size.x)
}

fn tab_buttons(ui: &mut egui::Ui, selected_tab: LogTab, response: &mut LogUiResponse) {
    tab_button(ui, "Game Log", LogTab::Game, selected_tab, response);
    tab_button(ui, "Battle Log", LogTab::Battle, selected_tab, response);
}

fn tab_button(
    ui: &mut egui::Ui,
    label: &str,
    tab: LogTab,
    selected_tab: LogTab,
    response: &mut LogUiResponse,
) {
    let selected = tab == selected_tab;
    let text = if selected {
        egui::RichText::new(label)
            .strong()
            .color(Color32::from_rgb(0xFF, 0xF0, 0xD0))
    } else {
        egui::RichText::new(label)
    };
    if ui.add(egui::Button::new(text).selected(selected)).clicked() {
        response.selected_tab = Some(tab);
    }
}

pub fn available_game_width(window_width: f64, display: LogDisplay, panel_width: f32) -> f64 {
    if display == LogDisplay::SideBySide {
        (window_width - f64::from(panel_width)).max(100.0)
    } else {
        window_width
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::{handle_javascript_url, new_shared_log_state};

    #[test]
    fn selecting_a_tab_updates_the_log_model() {
        let logs = new_shared_log_state(|| {});

        LogUiResponse {
            selected_tab: Some(LogTab::Battle),
            close_requested: false,
        }
        .apply(&logs);

        assert_eq!(logs.snapshot().tab, LogTab::Battle);
    }

    #[test]
    fn closing_the_panel_hides_the_log() {
        let logs = new_shared_log_state(|| {});
        handle_javascript_url("javascript:toggleSbS();", &logs);

        LogUiResponse {
            selected_tab: None,
            close_requested: true,
        }
        .apply(&logs);

        assert_eq!(logs.display(), LogDisplay::Hidden);
    }

    #[test]
    fn side_panel_reduces_game_width() {
        assert_eq!(
            available_game_width(1280.0, LogDisplay::SideBySide, 320.0),
            960.0
        );
    }

    #[test]
    fn side_panel_keeps_a_minimum_game_width() {
        assert_eq!(
            available_game_width(300.0, LogDisplay::SideBySide, 320.0),
            100.0
        );
    }

    #[test]
    fn hidden_and_pop_out_logs_do_not_reduce_game_width() {
        assert_eq!(
            available_game_width(1280.0, LogDisplay::Hidden, 320.0),
            1280.0
        );
        assert_eq!(
            available_game_width(1280.0, LogDisplay::PopOut, 320.0),
            1280.0
        );
    }
}
