//! The playback engine (§5.4). One thread per output owns a `rodio`
//! `OutputStream` + `Sink` (both `!Send`) for its whole life and processes
//! commands from a channel.
//!
//! FD-safety: tracks are read fully into memory and decoded from a `Cursor`,
//! and appended in a rolling window of [`WINDOW`] so the sink never holds
//! hundreds of open files and `Stop`/`Load` stay responsive.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink, Source};

use crate::event::AppMessage;
use crate::model::output::OutputId;

/// Housekeeping cadence; also the command-receive timeout.
const TICK: Duration = Duration::from_millis(120);

/// How many tracks are kept queued in the sink at once.
const WINDOW: usize = 3;

/// A decoder over an in-memory track plus its duration (if known).
type DecodedTrack = (Decoder<Cursor<Vec<u8>>>, Option<Duration>);

/// Lifecycle state of one engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineState {
    Idle,
    Loading,
    Playing,
    Paused,
    Error(String),
}

/// Commands sent to an engine thread.
#[derive(Debug)]
pub enum EngineCommand {
    /// Replace the playlist and optionally start playing.
    Load {
        tracks: Vec<PathBuf>,
        loop_enabled: bool,
        autoplay: bool,
    },
    Play,
    Pause,
    Stop,
    Next,
    Prev,
    SetVolume(f32),
    SetLoop(bool),
    Shutdown,
}

/// Events emitted by an engine thread.
#[derive(Debug, Clone)]
pub enum EngineEvent {
    StateChanged(EngineState),
    TrackChanged {
        index: usize,
        total: usize,
    },
    Progress {
        elapsed: Duration,
        track_len: Option<Duration>,
    },
    Finished,
}

/// Handle for spawning engine threads.
pub struct PlaybackEngine;

impl PlaybackEngine {
    /// Spawn an engine thread for one output. `device == None` follows the
    /// system default output. Returns the command channel and the thread's
    /// join handle (so a removed output can be joined cleanly).
    pub fn spawn(
        device: Option<rodio::cpal::Device>,
        tx: Sender<AppMessage>,
        id: OutputId,
    ) -> (Sender<EngineCommand>, thread::JoinHandle<()>) {
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let join = thread::Builder::new()
            .name(format!("engine-{id}"))
            .spawn(move || run(device, cmd_rx, tx, id))
            .expect("failed to spawn engine thread");
        (cmd_tx, join)
    }
}

/// Engine thread body: open the output stream, then loop on commands +
/// housekeeping until `Shutdown` or the command channel disconnects.
fn run(
    device: Option<rodio::cpal::Device>,
    cmd_rx: Receiver<EngineCommand>,
    tx: Sender<AppMessage>,
    id: OutputId,
) {
    // `_stream` must stay alive for the whole thread — dropping it closes the
    // audio device.
    let (_stream, handle) = match open_stream(device.as_ref()) {
        Ok(pair) => pair,
        Err(err) => {
            let msg = format!("audio output unavailable: {err}");
            tracing::error!("engine {id}: {msg}");
            let _ = tx.send(AppMessage::Engine(
                id,
                EngineEvent::StateChanged(EngineState::Error(msg)),
            ));
            return;
        }
    };
    let sink = match Sink::try_new(&handle) {
        Ok(sink) => sink,
        Err(err) => {
            let msg = format!("could not create audio sink: {err}");
            tracing::error!("engine {id}: {msg}");
            let _ = tx.send(AppMessage::Engine(
                id,
                EngineEvent::StateChanged(EngineState::Error(msg)),
            ));
            return;
        }
    };

    let mut engine = EngineRuntime::new(handle, sink, tx, id);
    loop {
        match cmd_rx.recv_timeout(TICK) {
            Ok(cmd) => {
                if engine.handle_command(cmd) {
                    break; // Shutdown
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        engine.housekeeping();
    }
    tracing::debug!("engine {id} thread exiting");
}

/// Open an output stream for `device`, falling back to the system default.
fn open_stream(
    device: Option<&rodio::cpal::Device>,
) -> Result<(OutputStream, OutputStreamHandle), rodio::StreamError> {
    match device {
        Some(dev) => OutputStream::try_from_device(dev).or_else(|_| OutputStream::try_default()),
        None => OutputStream::try_default(),
    }
}

/// All mutable engine state — lives entirely on the engine thread.
struct EngineRuntime {
    handle: OutputStreamHandle,
    sink: Sink,
    tx: Sender<AppMessage>,
    id: OutputId,
    tracks: Vec<PathBuf>,
    /// Per-track durations, filled lazily as tracks are appended.
    durations: Vec<Option<Duration>>,
    /// Count of tracks pushed into the sink so far.
    appended: usize,
    loop_enabled: bool,
    playing: bool,
    volume: f32,
    last_state: EngineState,
    last_index: Option<usize>,
}

impl EngineRuntime {
    fn new(handle: OutputStreamHandle, sink: Sink, tx: Sender<AppMessage>, id: OutputId) -> Self {
        Self {
            handle,
            sink,
            tx,
            id,
            tracks: Vec::new(),
            durations: Vec::new(),
            appended: 0,
            loop_enabled: false,
            playing: false,
            volume: 1.0,
            last_state: EngineState::Idle,
            last_index: None,
        }
    }

    fn emit(&self, event: EngineEvent) {
        let _ = self.tx.send(AppMessage::Engine(self.id, event));
    }

    /// Emit `StateChanged` only when the state actually changes.
    fn set_state(&mut self, state: EngineState) {
        if self.last_state != state {
            self.last_state = state.clone();
            self.emit(EngineEvent::StateChanged(state));
        }
    }

    /// Emit `TrackChanged` only when the index actually changes.
    fn emit_track_changed(&mut self, index: usize) {
        if self.last_index != Some(index) {
            self.last_index = Some(index);
            self.emit(EngineEvent::TrackChanged {
                index,
                total: self.tracks.len(),
            });
        }
    }

    /// Process one command. Returns `true` if the engine should shut down.
    fn handle_command(&mut self, cmd: EngineCommand) -> bool {
        match cmd {
            EngineCommand::Load {
                tracks,
                loop_enabled,
                autoplay,
            } => self.load(tracks, loop_enabled, autoplay),
            EngineCommand::Play => {
                if !self.tracks.is_empty() {
                    self.sink.play();
                    self.playing = true;
                    self.set_state(EngineState::Playing);
                }
            }
            EngineCommand::Pause => {
                self.sink.pause();
                self.playing = false;
                if !self.tracks.is_empty() {
                    self.set_state(EngineState::Paused);
                }
            }
            EngineCommand::Stop => {
                self.reset_sink();
                self.tracks.clear();
                self.durations.clear();
                self.appended = 0;
                self.playing = false;
                self.last_index = None;
                self.set_state(EngineState::Idle);
            }
            EngineCommand::SetVolume(v) => {
                self.volume = v.clamp(0.0, 1.0);
                self.sink.set_volume(self.volume);
            }
            EngineCommand::SetLoop(enabled) => self.loop_enabled = enabled,
            EngineCommand::Next => self.skip_to(self.current_index().saturating_add(1)),
            EngineCommand::Prev => self.skip_to(self.current_index().saturating_sub(1)),
            EngineCommand::Shutdown => {
                self.sink.stop();
                return true;
            }
        }
        false
    }

    /// Replace the playlist and reset the sink.
    fn load(&mut self, tracks: Vec<PathBuf>, loop_enabled: bool, autoplay: bool) {
        self.set_state(EngineState::Loading);
        self.reset_sink();
        self.durations = vec![None; tracks.len()];
        self.tracks = tracks;
        self.loop_enabled = loop_enabled;
        self.appended = 0;
        self.last_index = None;
        self.fill_window();

        if self.tracks.is_empty() {
            self.playing = false;
            self.set_state(EngineState::Idle);
            return;
        }
        if autoplay {
            self.sink.play();
            self.playing = true;
            self.set_state(EngineState::Playing);
        } else {
            self.sink.pause();
            self.playing = false;
            self.set_state(EngineState::Paused);
        }
        self.emit_track_changed(0);
    }

    /// Replace the sink with a fresh one (a clean reset), preserving volume.
    fn reset_sink(&mut self) {
        self.sink.stop();
        match Sink::try_new(&self.handle) {
            Ok(sink) => {
                sink.set_volume(self.volume);
                self.sink = sink;
            }
            Err(err) => tracing::error!("engine {}: sink rebuild failed: {err}", self.id),
        }
    }

    /// Append tracks (decoded from memory) until the sink holds [`WINDOW`]
    /// items or every track is queued.
    fn fill_window(&mut self) {
        while self.sink.len() < WINDOW && self.appended < self.tracks.len() {
            let idx = self.appended;
            match decode_track(&self.tracks[idx]) {
                Ok((source, duration)) => {
                    self.durations[idx] = duration;
                    self.sink.append(source);
                }
                Err(err) => {
                    tracing::warn!(
                        "engine {}: skipping unreadable track {:?}: {err}",
                        self.id,
                        self.tracks[idx]
                    );
                }
            }
            self.appended += 1;
        }
    }

    /// Index of the track currently at the head of the sink.
    fn current_index(&self) -> usize {
        self.appended.saturating_sub(self.sink.len())
    }

    /// Jump to `index` (rebuilds the sink). `index >= tracks.len()` means the
    /// caller skipped past the end.
    fn skip_to(&mut self, index: usize) {
        if self.tracks.is_empty() {
            return;
        }
        if index >= self.tracks.len() {
            if self.loop_enabled {
                self.restart_from_zero();
            } else {
                self.finish();
            }
            return;
        }
        self.reset_sink();
        self.appended = index;
        self.fill_window();
        if self.playing {
            self.sink.play();
        } else {
            self.sink.pause();
        }
        self.emit_track_changed(index);
    }

    /// Wrap back to the first track and keep playing (loop).
    fn restart_from_zero(&mut self) {
        self.reset_sink();
        self.appended = 0;
        self.last_index = None;
        self.fill_window();
        self.sink.play();
        self.playing = true;
        self.emit_track_changed(0);
        self.set_state(EngineState::Playing);
    }

    /// End the playlist: clear state and emit `Finished`.
    fn finish(&mut self) {
        self.playing = false;
        self.emit(EngineEvent::Finished);
        self.set_state(EngineState::Idle);
        self.reset_sink();
        self.tracks.clear();
        self.durations.clear();
        self.appended = 0;
        self.last_index = None;
    }

    /// Per-tick housekeeping: top up the window, detect end-of-playlist, and
    /// emit progress.
    fn housekeeping(&mut self) {
        if self.tracks.is_empty() {
            return;
        }
        self.fill_window();
        let remaining = self.sink.len();

        if remaining == 0 {
            // The sink drained and nothing more could be appended.
            if !self.playing {
                return;
            }
            if self.loop_enabled {
                self.restart_from_zero();
            } else {
                self.finish();
            }
            return;
        }

        let index = self
            .appended
            .saturating_sub(remaining)
            .min(self.tracks.len() - 1);
        self.emit_track_changed(index);
        self.emit(EngineEvent::Progress {
            elapsed: self.sink.get_pos(),
            track_len: self.durations.get(index).copied().flatten(),
        });
    }
}

/// Read a track fully into memory and build a decoder over it.
fn decode_track(path: &Path) -> anyhow::Result<DecodedTrack> {
    let bytes = std::fs::read(path)?;
    let decoder = Decoder::new(Cursor::new(bytes))?;
    let duration = decoder.total_duration();
    Ok((decoder, duration))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use crate::audio::engine::{EngineCommand, EngineEvent, EngineState, PlaybackEngine};
    use crate::event::AppMessage;

    fn test_asset(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../TestAssets")
            .join(name)
    }

    /// Spawns a real engine, plays briefly at low volume, and asserts the
    /// playback position actually advances. Ignored by default because it
    /// opens the system audio device — run with `cargo test -- --ignored`.
    #[test]
    #[ignore = "opens the real audio device and plays audio"]
    fn plays_and_reports_progress() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let (cmd, _join) = PlaybackEngine::spawn(None, tx, 0);

        let track = test_asset("114-an-nas-mishary-alafasy.mp3");
        assert!(track.exists(), "missing test asset {track:?}");

        cmd.send(EngineCommand::SetVolume(0.12)).unwrap();
        cmd.send(EngineCommand::Load {
            tracks: vec![track],
            loop_enabled: false,
            autoplay: true,
        })
        .unwrap();

        let mut playing = false;
        let mut max_elapsed = Duration::ZERO;
        let deadline = Instant::now() + Duration::from_secs(6);
        while Instant::now() < deadline {
            if let Ok(AppMessage::Engine(_, event)) = rx.recv_timeout(Duration::from_millis(250)) {
                match event {
                    EngineEvent::StateChanged(EngineState::Playing) => playing = true,
                    EngineEvent::StateChanged(EngineState::Error(err)) => {
                        panic!("engine reported an error: {err}")
                    }
                    EngineEvent::Progress { elapsed, .. } => max_elapsed = max_elapsed.max(elapsed),
                    _ => {}
                }
            }
        }
        cmd.send(EngineCommand::Shutdown).ok();

        assert!(playing, "engine never reported Playing");
        assert!(
            max_elapsed > Duration::from_millis(500),
            "playback position did not advance (max elapsed {max_elapsed:?})"
        );
    }
}
