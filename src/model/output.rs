//! Output channels (§6.3). `App.outputs` always holds at least one element —
//! `outputs[0]`, the system default, which can never be removed.

use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::Sender;

use crate::audio::engine::{EngineCommand, EngineState};
use crate::model::playback_config::PlaybackConfig;

/// Identifies one output channel. Monotonically increasing; output `0` is always
/// the system default and can never be removed.
pub type OutputId = u32;

/// In-flight download state for one output.
#[derive(Debug, Clone)]
pub struct DownloadProgress {
    pub done: usize,
    pub total: usize,
    /// Label of the most recently fetched file.
    pub label: String,
    /// Whether to start playing once the download completes.
    pub autoplay: bool,
    /// `Some` when this download was triggered to play a playlist — its id, so
    /// the completion handler re-resolves and plays that playlist.
    pub playlist_id: Option<String>,
}

/// One output channel: its config, mirrored playback state, and the command
/// channel to its dedicated engine thread.
pub struct OutputChannel {
    pub id: OutputId,
    /// "System Default" or the bound device name.
    pub label: String,
    /// `None` follows the system default output.
    pub device_name: Option<String>,
    pub config: PlaybackConfig,

    /// Snapshot of `config` when playback last started. Now Playing reads this
    /// instead of `config` so that editing the range in Browse doesn't disturb
    /// what's shown on the Now Playing tab.
    pub display_config: Option<PlaybackConfig>,

    // Mirrored from the engine thread.
    pub state: EngineState,
    pub track_index: usize,
    pub track_total: usize,
    pub elapsed: Duration,
    pub track_len: Option<Duration>,
    /// Human-readable label per track, e.g. `18:5` — parallel to the playlist.
    pub track_labels: Vec<String>,
    /// True when playing a whole-surah file because per-ayah files were absent.
    pub is_fallback: bool,
    /// `Some` while an on-demand download is running for this output.
    pub download: Option<DownloadProgress>,

    /// Command channel to this output's engine thread.
    pub cmd_tx: Sender<EngineCommand>,
    /// Join handle for the engine thread, so a removed output can be joined.
    pub join_handle: Option<JoinHandle<()>>,
}

impl OutputChannel {
    pub fn new(
        id: OutputId,
        label: String,
        device_name: Option<String>,
        config: PlaybackConfig,
        cmd_tx: Sender<EngineCommand>,
        join_handle: JoinHandle<()>,
    ) -> Self {
        Self {
            id,
            label,
            device_name,
            config,
            display_config: None,
            state: EngineState::Idle,
            track_index: 0,
            track_total: 0,
            elapsed: Duration::ZERO,
            track_len: None,
            track_labels: Vec::new(),
            is_fallback: false,
            download: None,
            cmd_tx,
            join_handle: Some(join_handle),
        }
    }

    /// True when this output is actively playing audio.
    pub fn is_playing(&self) -> bool {
        self.state == EngineState::Playing
    }

    /// Label of the currently playing track, if any.
    pub fn current_track_label(&self) -> Option<&str> {
        self.track_labels.get(self.track_index).map(String::as_str)
    }

    /// Send a command to this output's engine thread.
    pub fn send(&self, cmd: EngineCommand) {
        if self.cmd_tx.send(cmd).is_err() {
            tracing::error!("output {}: engine channel disconnected", self.id);
        }
    }
}
