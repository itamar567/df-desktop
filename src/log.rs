//! Log callback implementation and log display using LocalConnection.

use std::sync::{Arc, Mutex};

use ruffle_core::local_connection::{LocalConnectionListener, LocalConnectionMessage};

/// Represents the mode for displaying logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogMode {
    #[default]
    Disabled,
    SideBySide,
    PopOut,
}

/// Represents which log tab is currently active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogTab {
    #[default]
    Game,
    Battle,
}

#[derive(Debug, Clone, Default)]
pub struct LogState {
    pub mode: LogMode,
    pub visible: bool,
    pub tab: LogTab,
    pub game_log: String,
    pub battle_log: String,
}

/// Thread-safe wrapper for log state.
pub type SharedLogState = Arc<Mutex<LogState>>;

/// Creates a new shared log state.
pub fn new_shared_log_state() -> SharedLogState {
    Arc::new(Mutex::new(LogState::default()))
}

/// Listener that captures LocalConnection messages for the log.
pub struct LogListener {
    state: SharedLogState,
}

impl LogListener {
    pub fn new(state: SharedLogState) -> Self {
        Self { state }
    }
}

impl LocalConnectionListener for LogListener {
    fn on_message(&self, message: &LocalConnectionMessage) {
        tracing::debug!("LocalConnection: channel={}, method={}, arg_count={}", message.channel, message.method, message.arguments.len());
        
        // Only handle messages to the "df_log" channel
        if !message.channel.ends_with(":df_log") && message.channel != "df_log" {
            return;
        }

        match message.method.as_str() {
            "swapGameLog" => {
                if let Some(content) = message.arguments.first() {
                    if let Ok(mut state) = self.state.lock() {
                        state.game_log = content.clone();
                    }
                }
            }
            "swapBattleLog" => {
                if let Some(content) = message.arguments.first() {
                    if let Ok(mut state) = self.state.lock() {
                        state.battle_log = content.clone();
                    }
                }
            }
            "resetLogs" => {
                tracing::debug!("Ignoring resetLogs request");
            }
            "logSwap" => {
                if let Ok(mut state) = self.state.lock() {
                    let new_tab = if state.tab == LogTab::Game {
                        LogTab::Battle
                    } else {
                        LogTab::Game
                    };
                    state.tab = new_tab;
                }
            }
            _ => {
                tracing::warn!("Unknown df_log method: {}", message.method);
            }
        }
    }
}

/// Handles `javascript:` log URLs from the SWF.
pub fn handle_javascript_url(url: &str, log_state: &SharedLogState) -> bool {
    let Some(command) = parse_javascript_url(url) else {
        return false;
    };

    tracing::debug!("Log command: {command:?}");

    if let Ok(mut state) = log_state.lock() {
        match command {
            LogCommand::ToggleSbS => {
                state.mode = LogMode::SideBySide;
                state.visible = !state.visible;
            }
            LogCommand::ToggleEx => {
                state.mode = LogMode::PopOut;
                state.visible = !state.visible;
            }
            LogCommand::HideLog => {
                state.visible = false;
            }
            LogCommand::ToggleView => {
                if matches!(state.mode, LogMode::PopOut) {
                    state.mode = LogMode::SideBySide;
                } else if matches!(state.mode, LogMode::SideBySide) {
                    state.mode = LogMode::PopOut;
                }
            }
        }
    }

    true
}

/// Represents a log-related JavaScript command.
#[derive(Debug)]
enum LogCommand {
    ToggleSbS,
    ToggleEx,
    HideLog,
    ToggleView,
}

/// Parses a `javascript:` URL and returns a LogCommand if it's log-related.
fn parse_javascript_url(url: &str) -> Option<LogCommand> {
    let path = url.strip_prefix("javascript:")?.trim();
    match path {
        "toggleSbS();" | "toggleSbS()" => Some(LogCommand::ToggleSbS),
        "toggleEx();" | "toggleEx()" => Some(LogCommand::ToggleEx),
        "hideLog();" | "hideLog()" => Some(LogCommand::HideLog),
        "toggleView();" | "toggleView()" => Some(LogCommand::ToggleView),
        _ => None,
    }
}

/// Check if a JavaScript URL path is a log command.
pub fn is_log_javascript_url(url: &str) -> bool {
    parse_javascript_url(url).is_some()
}

/// Represents a parsed HTML fragment from the log with color information.
#[derive(Debug, Clone)]
pub struct LogFragment {
    pub text: String,
    pub color: Option<[u8; 3]>,
}

/// Parses log content containing HTML font tags into fragments with colors.
/// Input format: `<font color='#RRGGBB'>text</font>` or plain text.
pub fn parse_log_content(content: &str) -> Vec<LogFragment> {
    let mut fragments = Vec::new();
    let mut remaining = content;

    const FONT_TAG: &str = "<font";
    while let Some(font_start) = remaining.find(FONT_TAG) {
        if font_start > 0 {
            fragments.push(LogFragment {
                text: remaining[..font_start].to_string(),
                color: None,
            });
        }
        remaining = &remaining[font_start..];
        let Some(color_attr_start) = remaining.find("color=") else {
            fragments.push(LogFragment { text: remaining.to_string(), color: None });
            break;
        };
        let after_eq = &remaining[color_attr_start + 6..];
        let quote = after_eq.chars().next().unwrap_or('\'');
        if quote != '\'' && quote != '"' {
            fragments.push(LogFragment { text: remaining.to_string(), color: None });
            break;
        }
        let after_color_attr = &after_eq[1..];
        if let Some(quote_end) = after_color_attr.find(quote) {
            let color_str = &after_color_attr[..quote_end];
            let color = parse_hex_color(color_str).map(remap_game_color);

            if let Some(tag_end) = remaining.find('>') {
                remaining = &remaining[tag_end + 1..];
                if let Some(font_end) = remaining.find("</font>") {
                    let text = remaining[..font_end].to_string();
                    let text = text.trim_end_matches('\n').to_string();
                    if !text.is_empty() {
                        fragments.push(LogFragment {
                            text,
                            color,
                        });
                    }
                    remaining = &remaining[font_end + 7..];
                } else {
                    fragments.push(LogFragment {
                        text: remaining.to_string(),
                        color,
                    });
                    remaining = "";
                }
            } else {
                remaining = &remaining[1..];
            }
        } else {
            remaining = &remaining[1..];
        }
    }

    if !remaining.is_empty() {
        fragments.push(LogFragment {
            text: remaining.to_string(),
            color: None,
        });
    }

    fragments
}

/// Parses a hex color string (with or without #) into RGB bytes.
fn parse_hex_color(hex: &str) -> Option<[u8; 3]> {
    let hex = hex.trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some([r, g, b])
}

pub fn remap_game_color(color: [u8; 3]) -> [u8; 3] {
    let base = remap_for_dark_bg(color, 0.62, 0.92, 1.24);
    ensure_contrast(base, [0x1A, 0x04, 0x04])
}

fn srgb_to_linear(c: u8) -> f32 {
    let s = c as f32 / 255.0;
    if s <= 0.04045 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
}

fn relative_luminance(rgb: [u8; 3]) -> f32 {
    0.2126 * srgb_to_linear(rgb[0])
        + 0.7152 * srgb_to_linear(rgb[1])
        + 0.0722 * srgb_to_linear(rgb[2])
}

fn contrast_ratio(a: [u8; 3], b: [u8; 3]) -> f32 {
    let la = relative_luminance(a);
    let lb = relative_luminance(b);
    let (lighter, darker) = if la > lb { (la, lb) } else { (lb, la) };
    (lighter + 0.05) / (darker + 0.05)
}

fn ensure_contrast(fg: [u8; 3], bg: [u8; 3]) -> [u8; 3] {
    if contrast_ratio(fg, bg) >= 4.5 {
        return fg;
    }
    for i in 1..=6 {
        let t = i as f32 * 0.16;
        let candidate = [
            (fg[0] as f32 * (1.0 - t) + 255.0 * t).round().clamp(0.0, 255.0) as u8,
            (fg[1] as f32 * (1.0 - t) + 255.0 * t).round().clamp(0.0, 255.0) as u8,
            (fg[2] as f32 * (1.0 - t) + 255.0 * t).round().clamp(0.0, 255.0) as u8,
        ];
        if contrast_ratio(candidate, bg) >= 4.5 {
            return candidate;
        }
    }
    [
        (fg[0] as f32 * 0.2 + 255.0 * 0.8).round().clamp(0.0, 255.0) as u8,
        (fg[1] as f32 * 0.2 + 255.0 * 0.8).round().clamp(0.0, 255.0) as u8,
        (fg[2] as f32 * 0.2 + 255.0 * 0.8).round().clamp(0.0, 255.0) as u8,
    ]
}

fn remap_for_dark_bg(color: [u8; 3], min_lightness: f32, lightness_gamma: f32, saturation_scale: f32) -> [u8; 3] {
    let r = color[0] as f32 / 255.0;
    let g = color[1] as f32 / 255.0;
    let b = color[2] as f32 / 255.0;

    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;

    if (max - min).abs() < 1e-6 {
        let new_l = l.max(min_lightness).max(l.powf(lightness_gamma));
        let c = (new_l * 255.0).round().clamp(0.0, 255.0) as u8;
        return [c, c, c];
    }

    let d = max - min;
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };

    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    let h = h / 6.0;

    let new_l = l.max(min_lightness).max(l.powf(lightness_gamma));
    let new_s = (s * saturation_scale).min(1.0);

    let new_l = new_l.clamp(0.0, 1.0);
    let new_s = new_s.clamp(0.0, 1.0);

    let hue_to_rgb = |p: f32, q: f32, t: f32| -> f32 {
        let t = if t < 0.0 { t + 1.0 } else if t > 1.0 { t - 1.0 } else { t };
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 1.0 / 2.0 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };

    let q = if new_l < 0.5 {
        new_l * (1.0 + new_s)
    } else {
        new_l + new_s - new_l * new_s
    };
    let p = 2.0 * new_l - q;

    let r = hue_to_rgb(p, q, h + 1.0 / 3.0);
    let g = hue_to_rgb(p, q, h);
    let b = hue_to_rgb(p, q, h - 1.0 / 3.0);

    [
        (r * 255.0).round().clamp(0.0, 255.0) as u8,
        (g * 255.0).round().clamp(0.0, 255.0) as u8,
        (b * 255.0).round().clamp(0.0, 255.0) as u8,
    ]
}
