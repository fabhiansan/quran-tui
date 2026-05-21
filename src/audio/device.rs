//! Output device enumeration via `cpal` (§2 honest limitation: `cpal` cannot
//! report transport type, so Bluetooth detection is a cosmetic name heuristic).

use rodio::cpal;
use rodio::cpal::traits::{DeviceTrait, HostTrait};

use crate::event::AppMessage;

/// A detected output device — display info only. Use [`find_device`] to obtain
/// the `cpal` device for binding an engine to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDevice {
    pub name: String,
    pub is_default: bool,
    /// Best-effort "looks like Bluetooth" hint from the device name (cosmetic).
    pub bt_hint: bool,
}

/// Enumerate output devices. Cheap enough to call on a worker thread.
pub fn enumerate() -> Vec<AudioDevice> {
    let host = cpal::default_host();
    let default_name = host.default_output_device().and_then(|d| d.name().ok());

    let devices = match host.output_devices() {
        Ok(devices) => devices,
        Err(err) => {
            tracing::warn!("could not enumerate output devices: {err}");
            return Vec::new();
        }
    };

    devices
        .filter_map(|device| {
            let name = device.name().ok()?;
            Some(AudioDevice {
                is_default: default_name.as_deref() == Some(name.as_str()),
                bt_hint: looks_like_bluetooth(&name),
                name,
            })
        })
        .collect()
}

/// Find the `cpal` device with the given name, for binding an engine to it.
pub fn find_device(name: &str) -> Option<cpal::Device> {
    let host = cpal::default_host();
    host.output_devices()
        .ok()?
        .find(|d| d.name().map(|n| n == name).unwrap_or(false))
}

/// Spawn a worker that enumerates devices and reports the result.
pub fn spawn_refresh(tx: crossbeam_channel::Sender<AppMessage>) {
    std::thread::Builder::new()
        .name("device-refresh".to_string())
        .spawn(move || {
            let _ = tx.send(AppMessage::DevicesRefreshed(enumerate()));
        })
        .expect("failed to spawn device-refresh thread");
}

/// Best-effort Bluetooth name heuristic (§2 — cosmetic only).
fn looks_like_bluetooth(name: &str) -> bool {
    let lower = name.to_lowercase();
    const HINTS: [&str; 10] = [
        "airpods",
        "bluetooth",
        "wh-",
        "wf-",
        "buds",
        "jbl",
        "bose",
        "beats",
        "galaxy",
        "soundcore",
    ];
    HINTS.iter().any(|hint| lower.contains(hint))
}
