//! Overlay screens (disclaimer / migration setup / game / error) and the
//! minimal `UiBackend` implementation.

use std::rc::Rc;
use std::sync::{Arc, Mutex};

use fontdb::Database;
use ruffle_core::backend::ui::{
    FileFilter, FontDefinition, LanguageIdentifier, MouseCursor, UiBackend,
};
use ruffle_core::font::{FontFileData, FontQuery};
use url::Url;
use winit::window::{CursorIcon, Window};

use crate::config::State;

/// Which overlay screen the app is showing. The `ui` module owns the
/// transition logic; the app renders the current screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen {
    Disclaimer,
    Setup { sources: Vec<MigratedSource> },
    Playing,
    Error { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigratedSource {
    pub id: String,
    pub name: String,
}

impl From<&crate::migration::MigrationSource> for MigratedSource {
    fn from(source: &crate::migration::MigrationSource) -> Self {
        Self { id: source.id.into(), name: source.name.into() }
    }
}

/// Decides which screen to show after loading persisted state and scanning
/// migration sources. (Actions like "Continue" or "copy from X" are applied
/// by the app, which persists the choice and switches to `Playing`.)
pub fn initial_screen(state: &State, detected: &[MigratedSource]) -> Screen {
    if !state.disclaimer_accepted {
        Screen::Disclaimer
    } else if state.migration.is_none() && !detected.is_empty() {
        Screen::Setup { sources: detected.to_vec() }
    } else {
        Screen::Playing
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::State;

    fn source(id: &str) -> MigratedSource {
        MigratedSource { id: id.into(), name: id.into() }
    }

    #[test]
    fn disclaimer_shows_first_even_with_data_available() {
        let state = State::default();
        let detected = vec![source("flash-player")];
        assert_eq!(initial_screen(&state, &detected), Screen::Disclaimer);
    }

    #[test]
    fn setup_shows_only_when_data_detected() {
        let state = State { disclaimer_accepted: true, ..State::default() };
        let detected = vec![source("flash-player"), source("evolved-dragonfable-launcher")];
        assert_eq!(
            initial_screen(&state, &detected),
            Screen::Setup { sources: detected.clone() }
        );
    }

    #[test]
    fn setup_skipped_when_no_data() {
        let state = State { disclaimer_accepted: true, ..State::default() };
        assert_eq!(initial_screen(&state, &[]), Screen::Playing);
    }

    #[test]
    fn setup_skipped_after_migration_choice() {
        let state = State {
            disclaimer_accepted: true,
            migration: Some(crate::config::MigrationChoice {
                source: None,
                copied_at_unix: 0,
            }),
            ..State::default()
        };
        assert_eq!(initial_screen(&state, &[source("flash-player")]), Screen::Playing);
    }
}

/// Minimal `UiBackend`: mouse cursor + clipboard + fullscreen via winit/arboard,
/// device fonts via fontdb, everything else a no-op or `None`.
pub struct MinimalUiBackend {
    window: Arc<Window>,
    font_database: Rc<Database>,
    clipboard: Option<arboard::Clipboard>,
    cursor_visible: bool,
    /// Set by the core when the root movie fails to download; the app reads it
    /// each frame to switch to the error screen.
    pub root_error: Arc<Mutex<Option<String>>>,
}

impl MinimalUiBackend {
    pub fn new(
        window: Arc<Window>,
        font_database: Rc<Database>,
        root_error: Arc<Mutex<Option<String>>>,
    ) -> Self {
        let clipboard = arboard::Clipboard::new().ok();
        window.set_cursor_visible(true);
        Self { window, font_database, clipboard, cursor_visible: true, root_error }
    }
}

impl UiBackend for MinimalUiBackend {
    fn mouse_visible(&self) -> bool {
        self.cursor_visible
    }

    fn set_mouse_visible(&mut self, visible: bool) {
        self.cursor_visible = visible;
        self.window.set_cursor_visible(visible);
    }

    fn set_mouse_cursor(&mut self, cursor: MouseCursor) {
        let icon = match cursor {
            MouseCursor::Arrow => CursorIcon::Default,
            MouseCursor::Hand => CursorIcon::Pointer,
            MouseCursor::IBeam => CursorIcon::Text,
            MouseCursor::Grab => CursorIcon::Grab,
        };
        self.window.set_cursor(icon);
    }

    fn clipboard_content(&mut self) -> String {
        self.clipboard.as_mut().and_then(|clipboard| clipboard.get_text().ok()).unwrap_or_default()
    }

    fn set_clipboard_content(&mut self, content: String) {
        if let Some(clipboard) = self.clipboard.as_mut() {
            let _ = clipboard.set_text(content);
        }
    }

    fn set_fullscreen(
        &mut self,
        is_full: bool,
    ) -> Result<(), ruffle_core::backend::ui::FullscreenError> {
        use winit::window::Fullscreen;
        self.window
            .set_fullscreen(if is_full { Some(Fullscreen::Borderless(None)) } else { None });
        Ok(())
    }

    fn display_root_movie_download_failed_message(&self, _invalid_swf: bool, fetched_error: String) {
        *self.root_error.lock().unwrap() = Some(fetched_error);
    }

    fn message(&self, message: &str) {
        tracing::warn!("Flash message: {message}");
    }

    fn open_virtual_keyboard(&self) {}

    fn close_virtual_keyboard(&self) {}

    fn language(&self) -> LanguageIdentifier {
        sys_locale::get_locale()
            .and_then(|locale| locale.parse().ok())
            .unwrap_or_else(|| "en-US".parse().expect("en-US is a valid locale"))
    }

    fn display_unsupported_video(&self, url: Url) {
        if url.scheme() == "javascript" {
            tracing::warn!("SWF tried to run a script, but javascript calls are not allowed");
            return;
        }
        if let Err(error) = webbrowser::open(url.as_str()) {
            tracing::error!("Could not open URL {}: {error}", url);
        }
    }

    fn load_device_font(&self, query: &FontQuery, register: &mut dyn FnMut(FontDefinition)) {
        use fontdb::{Family, Query, Style, Weight};

        let name = query.name.clone();
        let query = Query {
            families: &[Family::Name(&name)],
            weight: if query.is_bold { Weight::BOLD } else { Weight::NORMAL },
            style: if query.is_italic { Style::Italic } else { Style::Normal },
            ..Default::default()
        };
        if let Some(id) = self.font_database.query(&query)
            && let Some(face) = self.font_database.face(id)
        {
            let is_bold = face.weight > Weight::NORMAL;
            let is_italic = face.style != Style::Normal;
            match &face.source {
                fontdb::Source::File(path) => {
                    if let Ok(data) = std::fs::read(path) {
                        register(FontDefinition::FontFile {
                            name: name.clone(),
                            is_bold,
                            is_italic,
                            data: FontFileData::new(data),
                            index: face.index,
                        });
                    }
                }
                fontdb::Source::Binary(bin) | fontdb::Source::SharedFile(_, bin) => {
                    register(FontDefinition::FontFile {
                        name: name.clone(),
                        is_bold,
                        is_italic,
                        data: FontFileData::new_shared(bin.clone()),
                        index: face.index,
                    });
                }
            }
        }
    }

    fn sort_device_fonts(
        &self,
        _query: &FontQuery,
        _register: &mut dyn FnMut(FontDefinition),
    ) -> Vec<FontQuery> {
        // No fontconfig integration yet; system font fallback covers the game.
        Vec::new()
    }

    fn display_file_open_dialog(
        &mut self,
        _filters: Vec<FileFilter>,
    ) -> Option<ruffle_core::backend::ui::DialogResultFuture> {
        None
    }

    fn display_file_open_dialog_multiple(
        &mut self,
        _filters: Vec<FileFilter>,
    ) -> Option<ruffle_core::backend::ui::MultiDialogResultFuture> {
        None
    }

    fn close_file_dialog(&mut self) {}

    fn display_file_save_dialog(
        &mut self,
        _file_name: String,
        _domain: String,
    ) -> Option<ruffle_core::backend::ui::DialogResultFuture> {
        None
    }
}
