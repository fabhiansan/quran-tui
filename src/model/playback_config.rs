//! Per-output playback configuration — ported from `DevicePlaybackConfig.swift`.

use serde::{Deserialize, Serialize};

/// What one output channel should play and how.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaybackConfig {
    pub reciter_id: String,
    pub from_surah: u16,
    pub to_surah: u16,
    pub from_ayah: u16,
    pub to_ayah: u16,
    /// 0.0 ..= 1.0
    pub volume: f32,
    pub loop_enabled: bool,
}

impl Default for PlaybackConfig {
    fn default() -> Self {
        Self {
            reciter_id: "alafasy".to_string(),
            from_surah: 1,
            to_surah: 1,
            from_ayah: 1,
            to_ayah: 7,
            volume: 1.0,
            loop_enabled: false,
        }
    }
}
