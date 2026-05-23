//! OS-level media key + Now Playing integration.
//!
//! On macOS, wraps `souvlaki` (which talks to `MPRemoteCommandCenter` +
//! `MPNowPlayingInfoCenter`) and pumps the main thread's CFRunLoop each tick
//! so the callback blocks the system attaches actually fire. F7/F8/F9, the
//! Now Playing widget in Control Center, and Bluetooth headset controls all
//! ride on the same APIs — registering with souvlaki gets us all three.
//!
//! On every other platform this module is a no-op so the TUI loop can call
//! [`MediaKeys::update`] and [`pump`] unconditionally.

use std::time::Duration;

use crossbeam_channel::Sender;

use crate::event::AppMessage;

/// What we publish to the OS each tick: title, artist, position, and whether
/// audio is playing/paused/stopped. Built from `App`'s current focused output.
#[derive(Clone, Debug, Default)]
pub struct NowPlayingSnapshot {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration: Option<Duration>,
    pub elapsed: Duration,
    pub state: NowPlayingState,
}

/// Playback state, mapped from `EngineState`. Stopped also covers "idle".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NowPlayingState {
    #[default]
    Stopped,
    Paused,
    Playing,
}

/// Handle to the OS media controls. Holds the platform impl (or nothing on
/// unsupported platforms / when init fails).
pub struct MediaKeys {
    #[cfg(target_os = "macos")]
    inner: Option<macos::Inner>,
}

impl MediaKeys {
    /// Initialise the OS media-control bridge. Must be called from the main
    /// thread on macOS — the underlying `MPRemoteCommandCenter` is main-thread
    /// only.
    pub fn init(_msg_tx: Sender<AppMessage>) -> Self {
        #[cfg(target_os = "macos")]
        {
            Self {
                inner: macos::Inner::init(_msg_tx),
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            Self {}
        }
    }

    /// Push the current Now Playing info to the OS. Cheap enough to call once
    /// per TUI tick; the underlying APIs replace the whole dictionary each time.
    pub fn update(&mut self, _snapshot: &NowPlayingSnapshot) {
        #[cfg(target_os = "macos")]
        if let Some(inner) = self.inner.as_mut() {
            inner.update(_snapshot);
        }
    }
}

/// Pump the main thread's run loop so the OS can deliver queued media-control
/// callbacks. Call once per TUI tick. No-op off macOS.
pub fn pump() {
    #[cfg(target_os = "macos")]
    macos::pump();
}

#[cfg(target_os = "macos")]
mod macos {
    use std::sync::atomic::{AtomicBool, Ordering};

    use core_foundation::base::TCFType;
    use core_foundation::date::CFTimeInterval;
    use core_foundation::runloop::{kCFRunLoopDefaultMode, CFRunLoopRunInMode};
    use core_foundation::string::CFString;
    use crossbeam_channel::Sender;
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    use souvlaki::{
        MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition,
        PlatformConfig,
    };

    use crate::event::{AppMessage, MediaAction};

    use super::{NowPlayingSnapshot, NowPlayingState};

    /// Avoid initialising `[NSApplication sharedApplication]` more than once
    /// per process — `MediaPlayer.framework` only needs it the first time.
    static NS_APP_INIT: AtomicBool = AtomicBool::new(false);

    pub(super) struct Inner {
        controls: MediaControls,
        last: Option<NowPlayingSnapshot>,
    }

    impl Inner {
        pub(super) fn init(msg_tx: Sender<AppMessage>) -> Option<Self> {
            // MediaPlayer.framework wants an NSApplication instance to exist
            // before MPRemoteCommandCenter dispatches anything. We don't run
            // the NSApp event loop — CFRunLoopRunInMode pumps it for us — but
            // the singleton must be constructed at least once on the main
            // thread.
            ensure_nsapp_initialised();

            let config = PlatformConfig {
                dbus_name: "quran-tui",
                display_name: "Quran TUI",
                hwnd: None,
            };
            let mut controls = match MediaControls::new(config) {
                Ok(c) => c,
                Err(err) => {
                    tracing::warn!("media controls init failed: {err:?}");
                    return None;
                }
            };

            let attached = controls.attach(move |event| {
                if let Some(action) = map_event(event) {
                    let _ = msg_tx.send(AppMessage::MediaControl(action));
                }
            });
            if let Err(err) = attached {
                tracing::warn!("media controls attach failed: {err:?}");
                return None;
            }
            tracing::info!("media controls attached (MPRemoteCommandCenter)");
            Some(Self {
                controls,
                last: None,
            })
        }

        pub(super) fn update(&mut self, snapshot: &NowPlayingSnapshot) {
            // Metadata is the expensive part (allocates an NSDictionary on
            // every call). Skip it when nothing the user cares about changed.
            let metadata_changed = self.last.as_ref().map_or(true, |prev| {
                prev.title != snapshot.title
                    || prev.artist != snapshot.artist
                    || prev.album != snapshot.album
                    || prev.duration != snapshot.duration
            });
            if metadata_changed {
                let _ = self.controls.set_metadata(MediaMetadata {
                    title: Some(&snapshot.title),
                    artist: Some(&snapshot.artist),
                    album: Some(&snapshot.album),
                    cover_url: None,
                    duration: snapshot.duration,
                });
            }

            let playback = match snapshot.state {
                NowPlayingState::Playing => MediaPlayback::Playing {
                    progress: Some(MediaPosition(snapshot.elapsed)),
                },
                NowPlayingState::Paused => MediaPlayback::Paused {
                    progress: Some(MediaPosition(snapshot.elapsed)),
                },
                NowPlayingState::Stopped => MediaPlayback::Stopped,
            };
            let _ = self.controls.set_playback(playback);
            self.last = Some(snapshot.clone());
        }
    }

    /// Drain any callbacks the OS has queued on the main run loop without
    /// blocking. 0.0 means "process whatever's ready, then return immediately".
    pub(super) fn pump() {
        unsafe {
            // The third arg (`return_after_source_handled`) being true makes
            // us return as soon as a single event is handled, but with timeout
            // 0 it doesn't matter — we return immediately either way.
            let mode = CFString::wrap_under_get_rule(kCFRunLoopDefaultMode);
            CFRunLoopRunInMode(mode.as_concrete_TypeRef(), 0.0 as CFTimeInterval, 0);
        }
    }

    fn ensure_nsapp_initialised() {
        if NS_APP_INIT.swap(true, Ordering::SeqCst) {
            return;
        }
        unsafe {
            let _: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        }
    }

    fn map_event(event: MediaControlEvent) -> Option<MediaAction> {
        Some(match event {
            MediaControlEvent::Play => MediaAction::Play,
            MediaControlEvent::Pause => MediaAction::Pause,
            MediaControlEvent::Toggle => MediaAction::Toggle,
            MediaControlEvent::Next => MediaAction::Next,
            MediaControlEvent::Previous => MediaAction::Prev,
            MediaControlEvent::Stop => MediaAction::Stop,
            _ => return None,
        })
    }
}
