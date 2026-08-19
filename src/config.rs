//! App configuration: platform directories and persisted state.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const GAME_URL: &str = "https://play.dragonfable.com/game/DFLoader.swf";
pub const BASE_DOMAIN: &str = "play.dragonfable.com";
pub const CACHE_MAX_BYTES: u64 = 512 * 1024 * 1024;

fn join(app_dir: Option<PathBuf>, name: &str) -> PathBuf {
    app_dir.expect("could not determine platform directory").join(name)
}

/// `$XDG_CACHE_HOME/itmr-dragonfable-launcher` (Linux) / `%LOCALAPPDATA%\itmr-dragonfable-launcher` (Windows).
pub fn cache_dir() -> PathBuf {
    join(dirs::cache_dir(), "itmr-dragonfable-launcher")
}

/// `$XDG_DATA_HOME/itmr-dragonfable-launcher/SharedObjects` (Linux) / `%LOCALAPPDATA%\itmr-dragonfable-launcher\SharedObjects` (Windows).
pub fn save_dir() -> PathBuf {
    join(dirs::data_local_dir(), "itmr-dragonfable-launcher/SharedObjects")
}

/// `$XDG_CONFIG_HOME/itmr-dragonfable-launcher` (Linux) / `%LOCALAPPDATA%\itmr-dragonfable-launcher` (Windows).
pub fn config_dir() -> PathBuf {
    join(dirs::config_local_dir(), "itmr-dragonfable-launcher")
}

/// `$XDG_DATA_HOME/itmr-dragonfable-launcher/log` (Linux) / `%LOCALAPPDATA%\itmr-dragonfable-launcher\log` (Windows).
pub fn log_dir() -> PathBuf {
    join(dirs::data_local_dir(), "itmr-dragonfable-launcher/log")
}

/// First-boot state persisted as `state.toml` in [`config_dir`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct State {
    pub disclaimer_accepted: bool,
    pub migration: Option<MigrationChoice>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MigrationChoice {
    /// Which source data was copied from; `None` means the user chose not to copy.
    pub source: Option<String>,
    pub copied_at_unix: i64,
}

impl State {
    pub fn load(config_dir: &Path) -> Self {
        match std::fs::read(config_dir.join("state.toml")) {
            Ok(bytes) => toml::from_str(&String::from_utf8_lossy(&bytes)).unwrap_or_default(),
            Err(_) => State::default(),
        }
    }

    pub fn save(&self, config_dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(config_dir)?;
        let encoded = toml::to_string(self).map_err(io::Error::other)?;
        std::fs::write(config_dir.join("state.toml"), encoded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_var<T: AsRef<std::ffi::OsStr>>(key: &str, value: Option<T>) {
        match value {
            // set_var/remove_var are unsafe in edition 2024 (process-global state).
            Some(value) => unsafe { std::env::set_var(key, value.as_ref()) },
            None => unsafe { std::env::remove_var(key) },
        }
    }

    // Env vars are process-global, so all dir assertions live in one test to
    // avoid races with other tests.
    #[test]
    fn dirs_follow_xdg_environment() {
        let original_cache = std::env::var_os("XDG_CACHE_HOME");
        let original_data = std::env::var_os("XDG_DATA_HOME");
        let original_config = std::env::var_os("XDG_CONFIG_HOME");

        set_var("XDG_CACHE_HOME", Some("/tmp/df-test-cache"));
        set_var("XDG_DATA_HOME", Some("/tmp/df-test-data"));
        set_var("XDG_CONFIG_HOME", Some("/tmp/df-test-config"));
        assert_eq!(cache_dir(), PathBuf::from("/tmp/df-test-cache/itmr-dragonfable-launcher"));
        assert_eq!(
            save_dir(),
            PathBuf::from("/tmp/df-test-data/itmr-dragonfable-launcher/SharedObjects")
        );
        assert_eq!(
            config_dir(),
            PathBuf::from("/tmp/df-test-config/itmr-dragonfable-launcher")
        );
        assert_eq!(log_dir(), PathBuf::from("/tmp/df-test-data/itmr-dragonfable-launcher/log"));

        set_var("XDG_CACHE_HOME", original_cache.as_deref());
        set_var("XDG_DATA_HOME", original_data.as_deref());
        set_var("XDG_CONFIG_HOME", original_config.as_deref());
    }

    #[test]
    fn state_roundtrips_through_toml() {
        let dir = tempfile::tempdir().unwrap();
        let state = State {
            disclaimer_accepted: true,
            migration: Some(MigrationChoice {
                source: Some("flash-player".into()),
                copied_at_unix: 1234,
            }),
        };
        state.save(dir.path()).unwrap();
        let loaded = State::load(dir.path());
        assert_eq!(loaded, state);
        assert!(dir.path().join("state.toml").exists());
    }

    #[test]
    fn missing_state_file_loads_as_default() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(State::load(dir.path()), State::default());
    }
}
