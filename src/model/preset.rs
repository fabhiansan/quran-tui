//! Named presets — save and restore the multi-output configuration.
//! Ported from `PresetModel.swift` / `PresetStore.swift`.
//!
//! Persisted as JSON to `<data_dir>/presets.json` with atomic writes. A corrupt
//! or missing file yields an empty list plus a logged warning.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::model::output::OutputChannel;
use crate::model::playback_config::PlaybackConfig;

/// One output's saved configuration within a preset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresetEntry {
    /// `None` = the default output; otherwise matched by device name on load.
    pub device_name: Option<String>,
    /// Shown in the missing-device banner if the device is absent on load.
    pub device_label: String,
    pub config: PlaybackConfig,
}

/// A named, dated set of per-output configurations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preset {
    pub id: String,
    pub name: String,
    /// RFC3339 timestamp.
    pub created_at: String,
    pub entries: Vec<PresetEntry>,
}

impl Preset {
    /// The timestamp as a compact `YYYY-MM-DD HH:MM` string.
    pub fn created_display(&self) -> String {
        self.created_at.replace('T', " ").chars().take(16).collect()
    }
}

/// The on-disk preset collection.
pub struct PresetStore {
    pub presets: Vec<Preset>,
    path: Option<PathBuf>,
}

impl PresetStore {
    /// Load presets from disk; a missing or corrupt file yields an empty store.
    pub fn load() -> Self {
        let path = preset_path();
        let presets = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|text| match serde_json::from_str::<Vec<Preset>>(&text) {
                Ok(presets) => Some(presets),
                Err(err) => {
                    tracing::warn!("presets.json is corrupt ({err}); starting empty");
                    None
                }
            })
            .unwrap_or_default();
        let mut store = Self { presets, path };
        store.sort_newest_first();
        store
    }

    /// Save the current output configuration as a new preset.
    pub fn save_current(&mut self, name: &str, outputs: &[OutputChannel]) {
        let entries = outputs
            .iter()
            .map(|output| PresetEntry {
                device_name: output.device_name.clone(),
                device_label: output.label.clone(),
                config: output.config.clone(),
            })
            .collect();
        self.presets.push(Preset {
            id: new_id(),
            name: name.to_string(),
            created_at: OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .unwrap_or_default(),
            entries,
        });
        self.sort_newest_first();
        self.persist();
    }

    pub fn delete(&mut self, id: &str) {
        self.presets.retain(|p| p.id != id);
        self.persist();
    }

    pub fn rename(&mut self, id: &str, name: &str) {
        if let Some(preset) = self.presets.iter_mut().find(|p| p.id == id) {
            preset.name = name.to_string();
        }
        self.persist();
    }

    fn sort_newest_first(&mut self) {
        self.presets.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    }

    fn persist(&self) {
        let Some(path) = &self.path else {
            tracing::warn!("no data directory available; presets not saved");
            return;
        };
        if let Some(parent) = path.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                tracing::warn!("could not create preset dir: {err}");
                return;
            }
        }
        match serde_json::to_string_pretty(&self.presets) {
            Ok(json) => write_atomic(path, json.as_bytes()),
            Err(err) => tracing::warn!("could not serialize presets: {err}"),
        }
    }
}

/// `<data_dir>/presets.json`.
fn preset_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("dev", "local", "quran-tui")
        .map(|dirs| dirs.data_dir().join("presets.json"))
}

/// A short, unique-enough hex id derived from the current time.
fn new_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}")
}

#[cfg(test)]
impl PresetStore {
    /// An ephemeral store that never touches disk — for tests.
    pub fn in_memory() -> Self {
        Self {
            presets: Vec::new(),
            path: None,
        }
    }
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
