//! Named playlists — ordered queues of Quran selections, like a music playlist.
//!
//! Each playlist holds an ordered list of [`PlaylistItem`]s; one item is a full
//! Browse selection (reciter + surah range + ayah range). Persisted as JSON to
//! `<data_dir>/playlists.json` with atomic writes. A corrupt or missing file
//! yields an empty list plus a logged warning.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// One entry in a playlist — a self-contained playback selection, equivalent to
/// a Browse range (reciter + surah span + ayah span).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaylistItem {
    pub reciter_id: String,
    pub from_surah: u16,
    pub from_ayah: u16,
    pub to_surah: u16,
    pub to_ayah: u16,
}

/// A named, dated, ordered list of playback selections.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    /// RFC3339 timestamp.
    pub created_at: String,
    pub items: Vec<PlaylistItem>,
}

impl Playlist {
    /// The timestamp as a compact `YYYY-MM-DD HH:MM` string.
    pub fn created_display(&self) -> String {
        self.created_at.replace('T', " ").chars().take(16).collect()
    }
}

/// The on-disk playlist collection.
pub struct PlaylistStore {
    pub playlists: Vec<Playlist>,
    path: Option<PathBuf>,
}

impl PlaylistStore {
    /// Load playlists from disk; a missing or corrupt file yields an empty store.
    pub fn load() -> Self {
        let path = playlist_path();
        let playlists = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|text| match serde_json::from_str::<Vec<Playlist>>(&text) {
                Ok(playlists) => Some(playlists),
                Err(err) => {
                    tracing::warn!("playlists.json is corrupt ({err}); starting empty");
                    None
                }
            })
            .unwrap_or_default();
        Self { playlists, path }
    }

    /// Create a new, empty playlist and return its id.
    pub fn create(&mut self, name: &str) -> String {
        let id = new_id();
        self.playlists.push(Playlist {
            id: id.clone(),
            name: name.to_string(),
            created_at: OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .unwrap_or_default(),
            items: Vec::new(),
        });
        self.persist();
        id
    }

    pub fn delete(&mut self, id: &str) {
        self.playlists.retain(|p| p.id != id);
        self.persist();
    }

    pub fn rename(&mut self, id: &str, name: &str) {
        if let Some(playlist) = self.playlists.iter_mut().find(|p| p.id == id) {
            playlist.name = name.to_string();
        }
        self.persist();
    }

    /// Append an item to the playlist with the given id.
    pub fn add_item(&mut self, id: &str, item: PlaylistItem) {
        if let Some(playlist) = self.playlists.iter_mut().find(|p| p.id == id) {
            playlist.items.push(item);
        }
        self.persist();
    }

    /// Remove the item at `index` from the playlist with the given id.
    pub fn remove_item(&mut self, id: &str, index: usize) {
        if let Some(playlist) = self.playlists.iter_mut().find(|p| p.id == id) {
            if index < playlist.items.len() {
                playlist.items.remove(index);
            }
        }
        self.persist();
    }

    fn persist(&self) {
        let Some(path) = &self.path else {
            tracing::warn!("no data directory available; playlists not saved");
            return;
        };
        if let Some(parent) = path.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                tracing::warn!("could not create playlist dir: {err}");
                return;
            }
        }
        match serde_json::to_string_pretty(&self.playlists) {
            Ok(json) => write_atomic(path, json.as_bytes()),
            Err(err) => tracing::warn!("could not serialize playlists: {err}"),
        }
    }
}

/// `<data_dir>/playlists.json`.
fn playlist_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("dev", "local", "quran-tui")
        .map(|dirs| dirs.data_dir().join("playlists.json"))
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
impl PlaylistStore {
    /// An ephemeral store that never touches disk — for tests.
    pub fn in_memory() -> Self {
        Self {
            playlists: Vec::new(),
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
