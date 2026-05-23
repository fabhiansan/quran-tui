//! Messages the main loop consumes from background threads (§5.5).

use crate::audio::device::AudioDevice;
use crate::audio::engine::EngineEvent;
use crate::model::output::OutputId;

/// Everything the main loop drains from the shared channel each frame.
#[derive(Debug)]
pub enum AppMessage {
    /// An engine thread reported a playback event for the given output.
    Engine(OutputId, EngineEvent),
    /// A download worker reported progress for the given output.
    Download(OutputId, DownloadUpdate),
    /// A device-refresh worker finished enumerating output devices.
    DevicesRefreshed(Vec<AudioDevice>),
    /// A verse-fetch worker finished loading one surah's verses.
    Verses {
        surah: u16,
        result: Result<crate::domain::verses::SurahVerses, String>,
    },
    /// A non-fatal error to surface to the user.
    Error(String),
    /// An OS-level media control event (Mac media keys, Bluetooth headset, etc.).
    MediaControl(MediaAction),
}

/// What the OS asked us to do via the media controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaAction {
    Toggle,
    Play,
    Pause,
    Next,
    Prev,
    Stop,
}

/// Progress reported by a download worker thread.
#[derive(Debug, Clone)]
pub enum DownloadUpdate {
    /// `done` of `total` files fetched; `label` names the most recent file.
    Progress {
        done: usize,
        total: usize,
        label: String,
    },
    /// Every file downloaded — the caller should re-resolve and play.
    Completed,
    /// The download failed; the string describes which file and why.
    Failed(String),
}
