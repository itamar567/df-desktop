//! Log callback implementation and log display using LocalConnection.

use std::sync::{Arc, Mutex, MutexGuard};
use ruffle_core::local_connection::{LocalConnectionListener, LocalConnectionMessage};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum LogMode {
    #[default]
    Disabled,
    SideBySide,
    PopOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogDisplay {
    #[default]
    Hidden,
    SideBySide,
    PopOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum LogTab {
    #[default]
    Game,
    Battle,
}

#[derive(Debug, Default)]
struct LogState {
    mode: LogMode,
    visible: bool,
    tab: LogTab,
    game_log: String,
    battle_log: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogSnapshot {
    pub tab: LogTab,
    pub content: String,
}

#[derive(Clone)]
pub struct SharedLogState {
    state: Arc<Mutex<LogState>>,
    on_change: Arc<dyn Fn() + Send + Sync>,
}

impl SharedLogState {
    fn new(on_change: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            state: Arc::new(Mutex::new(LogState::default())),
            on_change: Arc::new(on_change),
        }
    }

    fn state(&self) -> MutexGuard<'_, LogState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn update(&self, update: impl FnOnce(&mut LogState) -> bool) {
        let changed = {
            let mut state = self.state();
            update(&mut state)
        };
        if changed {
            (self.on_change)();
        }
    }

    pub fn display(&self) -> LogDisplay {
        let state = self.state();
        if !state.visible {
            LogDisplay::Hidden
        } else {
            match state.mode {
                LogMode::SideBySide => LogDisplay::SideBySide,
                LogMode::PopOut => LogDisplay::PopOut,
                LogMode::Disabled => LogDisplay::Hidden,
            }
        }
    }

    pub fn snapshot(&self) -> LogSnapshot {
        let state = self.state();
        let content = match state.tab {
            LogTab::Game => state.game_log.clone(),
            LogTab::Battle => state.battle_log.clone(),
        };
        LogSnapshot {
            tab: state.tab,
            content,
        }
    }

    pub fn replace(&self, snapshot: LogSnapshot) {
        self.update(|state| {
            let tab_changed = state.tab != snapshot.tab;
            state.tab = snapshot.tab;
            let content_changed = match snapshot.tab {
                LogTab::Game => replace_if_changed(&mut state.game_log, snapshot.content),
                LogTab::Battle => replace_if_changed(&mut state.battle_log, snapshot.content),
            };
            tab_changed || content_changed
        });
    }

    pub fn set_game_log(&self, content: String) {
        self.update(|state| replace_if_changed(&mut state.game_log, content));
    }

    pub fn set_battle_log(&self, content: String) {
        self.update(|state| replace_if_changed(&mut state.battle_log, content));
    }

    pub fn set_tab(&self, tab: LogTab) {
        self.update(|state| {
            let changed = state.tab != tab;
            state.tab = tab;
            changed
        });
    }

    pub fn toggle_tab(&self) {
        self.update(|state| {
            state.tab = match state.tab {
                LogTab::Game => LogTab::Battle,
                LogTab::Battle => LogTab::Game,
            };
            true
        });
    }

    pub fn hide(&self) {
        self.update(|state| {
            let changed = state.visible;
            state.visible = false;
            changed
        });
    }

    fn apply(&self, command: LogCommand) {
        self.update(|state| {
            let previous = (state.mode, state.visible);
            match command {
                LogCommand::ToggleSbS => {
                    state.mode = LogMode::SideBySide;
                    state.visible = !state.visible;
                }
                LogCommand::ToggleEx => {
                    state.mode = LogMode::PopOut;
                    state.visible = !state.visible;
                }
                LogCommand::HideLog => state.visible = false,
                LogCommand::ToggleView => match state.mode {
                    LogMode::SideBySide => state.mode = LogMode::PopOut,
                    LogMode::PopOut => state.mode = LogMode::SideBySide,
                    LogMode::Disabled => {}
                },
            }
            previous != (state.mode, state.visible)
        });
    }
}

const LOG_HISTORY_MAX_BYTES: usize = 256 * 1024;

/// Byte offset where the most recent `max_bytes` of `log` start, cut forward
/// to the next newline so the window never begins mid-line; the whole log
/// when no newline exists past the cut point.
fn recent_bytes_start(log: &str, max_bytes: usize) -> usize {
    let overflow = log.len().saturating_sub(max_bytes);
    if overflow == 0 {
        return 0;
    }
    log.bytes()
        .enumerate()
        .skip_while(|(index, byte)| *index < overflow || *byte != b'\n')
        .map(|(index, _)| index + 1)
        .next()
        .unwrap_or(log.len())
}

fn trim_to_recent_bytes(log: &mut String, max_bytes: usize) {
    let start = recent_bytes_start(log, max_bytes);
    log.drain(..start);
}

/// Mirrors one of the game's log buffers while accumulating a longer history.
///
/// Reverse-engineered from the game's DFLog code: every entry is appended to
/// the buffer and the whole buffer (minus its 8-char head) is re-sent with
/// each entry. Once the buffer passes 30000 chars the oldest entry is dropped
/// and the send for that tick is an empty string. So a message is the
/// buffer's current state: it equals the previously seen state minus a
/// trimmed head plus newly appended entries. New entries are recovered by
/// aligning the message against the previously seen state instead of trusting
/// the message head, which the game corrupts after a trim (its fixed 8-char
/// strip cuts into the first surviving entry).
#[derive(Default)]
pub struct LogMirror {
    assembled: String,
    buffer: String,
}

impl LogMirror {
    /// Feeds one message in, returning whether the assembled log changed.
    pub fn push(&mut self, message: &str) -> bool {
        if message.is_empty() {
            // The game sends an empty string on the tick that front-trims its
            // buffer; it carries no state.
            return false;
        }
        let seam = self.seam(message);
        let changed = seam < message.len();
        if changed {
            self.assembled.push_str(&message[seam..]);
            trim_to_recent_bytes(&mut self.assembled, LOG_HISTORY_MAX_BYTES);
        }
        self.buffer.clear();
        self.buffer.push_str(message);
        changed
    }

    /// The assembled history.
    #[cfg(test)]
    pub fn assembled(&self) -> &str {
        &self.assembled
    }

    /// The assembled history capped to its most recent `max_bytes`, cut at a
    /// line boundary.
    pub fn recent(&self, max_bytes: usize) -> String {
        let start = recent_bytes_start(&self.assembled, max_bytes);
        self.assembled[start..].to_owned()
    }

    /// Index in `message` where content beyond the last seen buffer begins.
    ///
    /// The message's old part is a suffix of the last seen buffer (trims and
    /// appends both happen at entry boundaries, which end with `</font>`), so
    /// the seam is the largest prefix of the message that the buffer ends
    /// with. A message sharing nothing with the buffer (game-side reset)
    /// yields a seam of 0 and is appended in full.
    fn seam(&self, message: &str) -> usize {
        let max = message.len().min(self.buffer.len());
        let mut seam = 0;
        let mut search = 0;
        while let Some(end) = message[search..].find("</font>") {
            let candidate = search + end + "</font>".len();
            search = candidate;
            if candidate > max {
                break;
            }
            if self.buffer.ends_with(&message[..candidate]) {
                seam = candidate;
            }
        }
        seam
    }
}

fn replace_if_changed(target: &mut String, replacement: String) -> bool {
    if *target == replacement {
        false
    } else {
        *target = replacement;
        true
    }
}

pub fn new_shared_log_state(
    on_change: impl Fn() + Send + Sync + 'static,
) -> SharedLogState {
    SharedLogState::new(on_change)
}

pub struct LogListener {
    state: SharedLogState,
    game: Mutex<LogMirror>,
    battle: Mutex<LogMirror>,
}

impl LogListener {
    pub fn new(state: SharedLogState) -> Self {
        Self {
            state,
            game: Mutex::new(LogMirror::default()),
            battle: Mutex::new(LogMirror::default()),
        }
    }
}

fn lock(mirror: &Mutex<LogMirror>) -> MutexGuard<'_, LogMirror> {
    mirror.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl LocalConnectionListener for LogListener {
    fn on_message(&self, message: &LocalConnectionMessage) {
        tracing::debug!(
            "LocalConnection: channel={}, method={}, arg_count={}",
            message.channel,
            message.method,
            message.arguments.len()
        );

        if !message.channel.ends_with(":df_log") && message.channel != "df_log" {
            return;
        }

        match message.method.as_str() {
            "swapGameLog" => {
                if let Some(content) = message.arguments.first() {
                    let mut mirror = lock(&self.game);
                    if mirror.push(content) {
                        self.state.set_game_log(mirror.recent(LOG_HISTORY_MAX_BYTES));
                    }
                }
            }
            "swapBattleLog" => {
                if let Some(content) = message.arguments.first() {
                    let mut mirror = lock(&self.battle);
                    if mirror.push(content) {
                        self.state.set_battle_log(mirror.recent(LOG_HISTORY_MAX_BYTES));
                    }
                }
            }
            "resetLogs" => tracing::debug!("Ignoring resetLogs request"),
            "logSwap" => self.state.toggle_tab(),
            _ => tracing::warn!("Unknown df_log method: {}", message.method),
        }
    }
}

pub fn handle_javascript_url(url: &str, log_state: &SharedLogState) -> bool {
    let Some(command) = parse_javascript_url(url) else {
        return false;
    };

    tracing::debug!("Log command: {command:?}");
    log_state.apply(command);
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn logs() -> (SharedLogState, Arc<AtomicUsize>) {
        let changes = Arc::new(AtomicUsize::new(0));
        let observed_changes = changes.clone();
        let logs = new_shared_log_state(move || {
            observed_changes.fetch_add(1, Ordering::Relaxed);
        });
        (logs, changes)
    }

    #[test]
    fn side_by_side_command_opens_and_closes_the_side_panel() {
        let (logs, _) = logs();

        assert!(handle_javascript_url("javascript:toggleSbS();", &logs));
        assert_eq!(logs.display(), LogDisplay::SideBySide);

        assert!(handle_javascript_url("javascript:toggleSbS();", &logs));
        assert_eq!(logs.display(), LogDisplay::Hidden);
    }

    #[test]
    fn pop_out_command_opens_and_closes_the_pop_out() {
        let (logs, _) = logs();

        assert!(handle_javascript_url("javascript:toggleEx();", &logs));
        assert_eq!(logs.display(), LogDisplay::PopOut);

        assert!(handle_javascript_url("javascript:toggleEx();", &logs));
        assert_eq!(logs.display(), LogDisplay::Hidden);
    }

    #[test]
    fn hiding_the_pop_out_preserves_its_mode() {
        let (logs, _) = logs();
        handle_javascript_url("javascript:toggleEx();", &logs);

        logs.hide();
        assert_eq!(logs.display(), LogDisplay::Hidden);

        handle_javascript_url("javascript:toggleEx();", &logs);
        assert_eq!(logs.display(), LogDisplay::PopOut);
    }

    #[test]
    fn toggle_view_moves_a_visible_log_between_windows() {
        let (logs, _) = logs();
        handle_javascript_url("javascript:toggleSbS();", &logs);

        handle_javascript_url("javascript:toggleView();", &logs);
        assert_eq!(logs.display(), LogDisplay::PopOut);

        handle_javascript_url("javascript:toggleView();", &logs);
        assert_eq!(logs.display(), LogDisplay::SideBySide);
    }

    #[test]
    fn toggle_view_does_nothing_before_a_view_is_selected() {
        let (logs, changes) = logs();

        assert!(handle_javascript_url("javascript:toggleView();", &logs));

        assert_eq!(logs.display(), LogDisplay::Hidden);
        assert_eq!(changes.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn hide_command_closes_the_visible_log() {
        let (logs, _) = logs();
        handle_javascript_url("javascript:toggleSbS();", &logs);

        assert!(handle_javascript_url("javascript:hideLog();", &logs));

        assert_eq!(logs.display(), LogDisplay::Hidden);
    }

    #[test]
    fn unrelated_javascript_url_is_not_handled() {
        let (logs, changes) = logs();

        assert!(!handle_javascript_url("javascript:other();", &logs));
        assert_eq!(changes.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn mutations_notify_only_when_state_changes() {
        let (logs, changes) = logs();

        logs.set_game_log("first".to_string());
        logs.set_game_log("first".to_string());
        logs.set_tab(LogTab::Game);
        logs.set_tab(LogTab::Battle);
        logs.hide();

        assert_eq!(changes.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn replacing_log_data_updates_the_selected_log_and_tab() {
        let (logs, changes) = logs();

        logs.replace(LogSnapshot {
            tab: LogTab::Battle,
            content: "battle".to_string(),
        });

        assert_eq!(
            logs.snapshot(),
            LogSnapshot {
                tab: LogTab::Battle,
                content: "battle".to_string(),
            }
        );
        assert_eq!(changes.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn snapshot_contains_only_the_selected_log() {
        let (logs, _) = logs();
        logs.set_game_log("game".to_string());
        logs.set_battle_log("battle".to_string());
        logs.set_tab(LogTab::Battle);

        let snapshot = logs.snapshot();

        assert_eq!(snapshot.tab, LogTab::Battle);
        assert_eq!(snapshot.content, "battle");
    }

    const HEADER: &str = "Battle Log----\n";

    fn entry(text: &str) -> String {
        format!("<font color='#333333'>{text}\\n</font>")
    }

    fn battle_logs() -> (SharedLogState, Arc<AtomicUsize>) {
        let (logs, changes) = logs();
        logs.set_tab(LogTab::Battle);
        (logs, changes)
    }

    #[test]
    fn log_mirror_appends_entries_from_whole_state_messages() {
        let mut mirror = LogMirror::default();
        let e1 = entry("Piotr takes 0 damage to MP.");
        let e2 = entry("Lost Spirit's HP was adjusted.");

        assert!(mirror.push(&format!("{HEADER}{e1}")));
        assert!(mirror.push(&format!("{HEADER}{e1}{e2}")));

        assert_eq!(mirror.assembled(), format!("{HEADER}{e1}{e2}"));
    }

    #[test]
    fn log_mirror_ignores_identical_and_empty_messages() {
        let mut mirror = LogMirror::default();
        let state = format!("{HEADER}{}", entry("a"));

        assert!(mirror.push(&state));
        assert!(!mirror.push(&state));
        assert!(!mirror.push(""));

        assert_eq!(mirror.assembled(), state);
    }

    #[test]
    fn log_mirror_recovers_entries_across_a_front_trim() {
        let mut mirror = LogMirror::default();
        let e1 = entry("[1]: Piotr takes 0 damage to MP.");
        let e2 = entry("[2]: Lost Spirit's HP was adjusted.");
        let e3 = entry("[3]: You miss.");

        mirror.push(&format!("{HEADER}{e1}{e2}"));
        // The tick that front-trims the buffer sends an empty string.
        assert!(!mirror.push(""));

        // The next full state: the game's fixed 8-char strip steals the head
        // of the first surviving entry.
        assert!(mirror.push(&format!("{}{e3}", &e2[8..])));

        assert_eq!(mirror.assembled(), format!("{HEADER}{e1}{e2}{e3}"));
    }

    #[test]
    fn log_mirror_pure_trim_state_adds_nothing() {
        let mut mirror = LogMirror::default();
        let e1 = entry("a");
        let e2 = entry("b");

        mirror.push(&format!("{HEADER}{e1}{e2}"));
        assert!(!mirror.push(&e2[8..]));

        assert_eq!(mirror.assembled(), format!("{HEADER}{e1}{e2}"));
    }

    #[test]
    fn log_mirror_appends_unrelated_state_after_a_game_reset() {
        let mut mirror = LogMirror::default();
        let old = format!("{HEADER}{}", entry("old battle"));
        let fresh = format!("{HEADER}{}", entry("new battle"));

        mirror.push(&old);
        assert!(mirror.push(&fresh));

        assert_eq!(mirror.assembled(), format!("{old}{fresh}"));
    }

    #[test]
    fn log_mirror_continues_after_a_reset_with_more_entries() {
        let mut mirror = LogMirror::default();
        let fresh = format!("{HEADER}{}", entry("new battle"));

        mirror.push(&format!("{HEADER}{}", entry("old battle")));
        mirror.push(&fresh);
        assert!(mirror.push(&format!("{fresh}{}", entry("more"))));

        assert_eq!(
            mirror.assembled(),
            format!("{HEADER}{}{fresh}{}", entry("old battle"), entry("more"))
        );
    }

    #[test]
    fn log_mirror_history_is_capped_to_recent_lines() {
        let mut mirror = LogMirror::default();
        let line = "0123456789\n";
        let message = format!(
            "{HEADER}<font color='#333333'>{}\n</font>",
            line.repeat(LOG_HISTORY_MAX_BYTES / line.len() + 2)
        );

        assert!(mirror.push(&message));

        assert!(mirror.assembled().len() <= LOG_HISTORY_MAX_BYTES);
        assert!(mirror.assembled().ends_with("\n</font>"));
    }

    #[test]
    fn log_mirror_drives_the_display_log() {
        let (logs, changes) = battle_logs();
        let mut mirror = LogMirror::default();
        let state = format!("{HEADER}{}", entry("a"));

        assert!(mirror.push(&state));
        logs.set_battle_log(mirror.recent(LOG_HISTORY_MAX_BYTES));
        assert!(!mirror.push(""));
        logs.set_battle_log(mirror.recent(LOG_HISTORY_MAX_BYTES));

        assert_eq!(logs.snapshot().content, state);
        assert_eq!(changes.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn recent_bytes_start_cuts_at_the_next_newline() {
        assert_eq!(recent_bytes_start("aaa\nbbb\nccc\n", 7), 8);
    }

    #[test]
    fn recent_bytes_start_clears_when_no_newline_exists() {
        assert_eq!(recent_bytes_start("abcdef", 2), 6);
    }

    #[test]
    fn recent_bytes_start_is_the_start_within_the_cap() {
        assert_eq!(recent_bytes_start("aaa\nbbb", 100), 0);
    }
}
