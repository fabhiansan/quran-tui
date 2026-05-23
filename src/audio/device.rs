//! Output device enumeration. On macOS we enumerate via Core Audio / HAL first
//! because `cpal::HostTrait::output_devices()` filters devices by stream-config
//! probing, which can hide connected Bluetooth outputs from the Outputs tab.

use rodio::cpal;
use rodio::cpal::traits::{DeviceTrait, HostTrait};

use crate::event::AppMessage;

/// A detected output device — display info only. Use [`find_device`] to obtain
/// the `cpal` device for binding an engine to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDevice {
    pub name: String,
    pub is_default: bool,
    /// True for Core Audio Bluetooth transports on macOS, otherwise a name hint.
    pub bt_hint: bool,
}

/// Enumerate output devices. Cheap enough to call on a worker thread.
pub fn enumerate() -> Vec<AudioDevice> {
    #[cfg(target_os = "macos")]
    {
        enumerate_macos()
    }

    #[cfg(not(target_os = "macos"))]
    {
        enumerate_cpal_outputs()
    }
}

fn enumerate_cpal_outputs() -> Vec<AudioDevice> {
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

/// Find the `cpal` device with the given name, for binding an engine to it. Use
/// `devices()` instead of `output_devices()` so HAL-discovered outputs that
/// fail CPAL's format-probing filter still have a chance to bind.
pub fn find_device(name: &str) -> Option<cpal::Device> {
    let host = cpal::default_host();
    host.devices()
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

#[cfg(target_os = "macos")]
fn enumerate_macos() -> Vec<AudioDevice> {
    use coreaudio::audio_unit::{macos_helpers, Scope};

    let mut devices = Vec::new();
    let default_id = macos_helpers::get_default_device_id(false);
    let ids = match macos_helpers::get_audio_device_ids_for_scope(Scope::Output) {
        Ok(ids) if !ids.is_empty() => Ok(ids),
        Ok(_) => macos_helpers::get_audio_device_ids(),
        Err(err) => {
            tracing::warn!("could not enumerate HAL output devices by scope: {err:?}");
            macos_helpers::get_audio_device_ids()
        }
    };

    match ids {
        Ok(ids) => {
            for id in ids {
                match macos_helpers::get_audio_device_supports_scope(id, Scope::Output) {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(err) => {
                        tracing::debug!("skipping HAL device {id}: output probe failed: {err:?}");
                        continue;
                    }
                }

                let name = match macos_helpers::get_device_name(id) {
                    Ok(name) if !name.trim().is_empty() => name,
                    Ok(_) => continue,
                    Err(err) => {
                        tracing::debug!("skipping HAL device {id}: name lookup failed: {err:?}");
                        continue;
                    }
                };

                push_unique(
                    &mut devices,
                    AudioDevice {
                        bt_hint: is_bluetooth_transport(id) || looks_like_bluetooth(&name),
                        is_default: default_id == Some(id),
                        name,
                    },
                );
            }
        }
        Err(err) => tracing::warn!("could not enumerate HAL audio devices: {err:?}"),
    }

    for device in enumerate_cpal_outputs() {
        push_unique(&mut devices, device);
    }

    devices
}

#[cfg(target_os = "macos")]
fn push_unique(devices: &mut Vec<AudioDevice>, device: AudioDevice) {
    if let Some(existing) = devices.iter_mut().find(|d| d.name == device.name) {
        existing.is_default |= device.is_default;
        existing.bt_hint |= device.bt_hint;
    } else {
        devices.push(device);
    }
}

#[cfg(target_os = "macos")]
fn is_bluetooth_transport(device_id: coreaudio::sys::AudioDeviceID) -> bool {
    use coreaudio::sys::{
        kAudioDevicePropertyTransportType, kAudioDeviceTransportTypeBluetooth,
        kAudioDeviceTransportTypeBluetoothLE, kAudioHardwareNoError,
        kAudioObjectPropertyElementMaster, kAudioObjectPropertyScopeGlobal,
        AudioObjectGetPropertyData, AudioObjectPropertyAddress,
    };
    use std::mem;
    use std::ptr::null;

    let property_address = AudioObjectPropertyAddress {
        mSelector: kAudioDevicePropertyTransportType,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMaster,
    };
    let mut transport = 0u32;
    let mut data_size = mem::size_of::<u32>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            device_id,
            &property_address as *const _,
            0,
            null(),
            &mut data_size as *mut _,
            &mut transport as *mut _ as *mut _,
        )
    };

    status == kAudioHardwareNoError as i32
        && (transport == kAudioDeviceTransportTypeBluetooth
            || transport == kAudioDeviceTransportTypeBluetoothLE)
}
