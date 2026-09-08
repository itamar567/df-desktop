//! Auto-clicker for cutscenes: detects a visible "continue" prompt in the
//! display tree and synthesizes a stage click to advance past it.

use ruffle_core::context::UpdateContext;
use ruffle_core::events::{MouseButton, MouseInputSource, PlayerEvent};
use ruffle_core::string::WStr;
use ruffle_core::swf::Point;
use ruffle_core::{DisplayObjectContainer, Player, TDisplayObject, TDisplayObjectContainer};

const CUTSCENE_CONTINUE_TEXT: &str = "click anywhere to continue";

pub fn matches_continue_text(text: &WStr) -> bool {
    text.to_utf8_lossy().to_lowercase().contains(CUTSCENE_CONTINUE_TEXT)
}

/// Runs one per-frame auto-click pass: when enabled and a visible continue
/// prompt is on stage, clicks the stage's center. The prompt's hit area
/// covers the cutscene area, and the center avoids the edge/corner hit-test
/// quirks of an exact (0, 0) click. Ruffle executes the button handlers
/// synchronously inside `handle_event`, so the cutscene advances immediately
/// and the next frame's search finds no prompt.
///
/// The synthetic click moves ruffle's internal mouse position and hover, so
/// `cursor_position` (the real cursor position in viewport pixels, if known)
/// is re-sent as a MouseMove afterwards to restore them; ruffle processes it
/// at the start of the next frame, before any scripts run.
pub fn auto_click_if_needed(player: &mut Player, enabled: bool, cursor_position: Option<(f64, f64)>) {
    if !enabled {
        return;
    }
    let click_point = player.mutate_with_update_context(|context| {
        if !contains_continue_text(context.stage.into(), context) {
            return None;
        }
        let (width, height) = context.stage.stage_size();
        let center = Point::from_pixels(f64::from(width) / 2.0, f64::from(height) / 2.0);
        let point = context.stage.view_matrix() * center;
        Some((point.x.to_pixels(), point.y.to_pixels()))
    });
    let Some((x, y)) = click_point else {
        return;
    };
    for event in [
        PlayerEvent::MouseDown {
            x,
            y,
            button: MouseButton::Left,
            index: None,
            source: MouseInputSource::Mouse,
        },
        PlayerEvent::MouseUp {
            x,
            y,
            button: MouseButton::Left,
            source: MouseInputSource::Mouse,
        },
    ] {
        player.handle_event(event);
    }
    if let Some((x, y)) = cursor_position {
        player.handle_event(PlayerEvent::MouseMove { x, y, source: MouseInputSource::Mouse });
    }
}

/// Depth-first search over the display tree for the continue prompt.
/// Invisible branches are pruned, so "visible" means the object and all of
/// its ancestors are visible.
fn contains_continue_text<'gc>(
    container: DisplayObjectContainer<'gc>,
    context: &mut UpdateContext<'gc>,
) -> bool {
    for child in container.iter_render_list() {
        if !child.visible() {
            continue;
        }
        if let Some(text) = child.as_text()
            && text.text(context).is_some_and(|text| matches_continue_text(&text))
        {
            return true;
        }
        if let Some(edit_text) = child.as_edit_text()
            && matches_continue_text(&edit_text.text())
        {
            return true;
        }
        if let Some(nested) = child.as_container()
            && contains_continue_text(nested, context)
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruffle_core::string::WString;

    fn wstr(text: &str) -> WString {
        text.encode_utf16().collect::<WString>()
    }

    #[test]
    fn matches_the_prompt_exactly() {
        assert!(matches_continue_text(&wstr("Click anywhere to continue")));
    }

    #[test]
    fn matches_the_prompt_case_insensitively() {
        assert!(matches_continue_text(&wstr("CLICK ANYWHERE TO CONTINUE")));
        assert!(matches_continue_text(&wstr("click anywhere to continue")));
    }

    #[test]
    fn matches_the_prompt_inside_other_text() {
        assert!(matches_continue_text(&wstr(
            "\u{2022} Click anywhere to continue \u{25B6}"
        )));
    }

    #[test]
    fn rejects_similar_but_different_text() {
        assert!(!matches_continue_text(&wstr("Click to continue")));
        assert!(!matches_continue_text(&wstr("Click anywhere")));
        assert!(!matches_continue_text(&wstr("")));
    }
}
