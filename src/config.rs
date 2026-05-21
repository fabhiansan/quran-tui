//! Application configuration persisted to `config.json` (§6.5).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Persisted app settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// Where local audio lives and where downloads land.
    pub audio_root: PathBuf,
    pub last_reciter_id: String,
    pub last_surah: u16,
}

impl AppConfig {
    /// Load config, applying the resolution order: `--audio-dir` override →
    /// `config.json` value → `<data_dir>/audio`.
    pub fn load(audio_dir_override: Option<PathBuf>) -> Self {
        let mut config = Self::read_from_disk().unwrap_or_else(Self::fallback);

        if let Some(dir) = audio_dir_override {
            config.audio_root = dir;
        }
        if let Err(err) = std::fs::create_dir_all(&config.audio_root) {
            tracing::warn!("could not create audio_root {:?}: {err}", config.audio_root);
        }
        config
    }

    /// Persist the config atomically (`.part` file, then rename).
    pub fn save(&self) {
        let Some(path) = config_path() else {
            tracing::warn!("no config directory available; not saving config");
            return;
        };
        if let Some(parent) = path.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                tracing::warn!("could not create config dir: {err}");
                return;
            }
        }
        match serde_json::to_string_pretty(self) {
            Ok(json) => write_atomic(&path, json.as_bytes()),
            Err(err) => tracing::warn!("could not serialize config: {err}"),
        }
    }

    fn read_from_disk() -> Option<Self> {
        let path = config_path()?;
        let text = std::fs::read_to_string(&path).ok()?;
        match serde_json::from_str(&text) {
            Ok(config) => Some(config),
            Err(err) => {
                tracing::warn!("config.json is corrupt ({err}); using defaults");
                None
            }
        }
    }

    /// Defaults used when no `config.json` exists.
    fn fallback() -> Self {
        Self {
            audio_root: default_audio_root(),
            last_reciter_id: "alafasy".to_string(),
            last_surah: 1,
        }
    }
}

/// `<config_dir>/config.json`.
fn config_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("dev", "local", "quran-tui")
        .map(|dirs| dirs.config_dir().join("config.json"))
}

/// `<data_dir>/audio`, or a temp dir if no data directory is available.
fn default_audio_root() -> PathBuf {
    directories::ProjectDirs::from("dev", "local", "quran-tui")
        .map(|dirs| dirs.data_dir().join("audio"))
        .unwrap_or_else(|| std::env::temp_dir().join("quran-tui-audio"))
}

/// Write `bytes` to `path` atomically via a `.part` sibling + rename.
fn write_atomic(path: &Path, bytes: &[u8]) {
    let tmp = path.with_extension("part");
    if let Err(err) = std::fs::write(&tmp, bytes) {
        tracing::warn!("could not write {:?}: {err}", tmp);
        return;
    }
    if let Err(err) = std::fs::rename(&tmp, path) {
        tracing::warn!("could not finalize {:?}: {err}", path);
        let _ = std::fs::remove_file(&tmp);
    }
}
