//! Central application state and the input/update logic. The main thread is the
//! single owner of `App` (§5.1 of the plan).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::widgets::ListState;

use crate::audio::device::{self, AudioDevice};
use crate::audio::engine::{EngineCommand, EngineEvent, EngineState, PlaybackEngine};
use crate::config::{self, AppConfig};
use crate::content::downloader;
use crate::content::resolver::{self, Resolution};
use crate::domain::catalog::{Catalog, Reciter, Surah};
use crate::domain::segment::{playback_segments, PlaybackSegment};
use crate::domain::verses::{SurahVerses, Verse};
use crate::event::{AppMessage, DownloadUpdate, MediaAction};
use crate::media_keys::{NowPlayingSnapshot, NowPlayingState};
use crate::model::output::{DownloadProgress, OutputChannel, OutputId};
use crate::model::playback_config::PlaybackConfig;
use crate::model::playlist::{Playlist, PlaylistItem, PlaylistStore};

/// Volume step for the `+` / `-` keys.
const VOLUME_STEP: f32 = 0.05;

/// How long a toast stays on screen.
const TOAST_TTL: Duration = Duration::from_secs(5);

/// The four primary tabs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    NowPlaying,
    Browse,
    Outputs,
    Playlists,
}

impl Tab {
    /// Tabs in display order; index 0 is the landing screen.
    pub const ALL: [Tab; 4] = [Tab::NowPlaying, Tab::Browse, Tab::Outputs, Tab::Playlists];

    /// Human-readable tab label shown in the tab strip.
    pub fn title(self) -> &'static str {
        match self {
            Tab::NowPlaying => "Now Playing",
            Tab::Browse => "Browse",
            Tab::Outputs => "Outputs",
            Tab::Playlists => "Playlists",
        }
    }

    /// Position of this tab in [`Tab::ALL`].
    pub fn index(self) -> usize {
        Tab::ALL.iter().position(|&t| t == self).unwrap_or(0)
    }
}

/// Severity of a transient status message.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToastKind {
    Info,
    Warn,
    Error,
}

/// A transient status message shown above the transport bar.
pub struct Toast {
    pub text: String,
    pub kind: ToastKind,
    pub expires: Instant,
}

/// What the Now Playing verse panel should display for the current track.
pub enum VerseView<'a> {
    /// Verse text and translation are ready to show.
    Ready {
        surah: u16,
        ayah: u16,
        verse: &'a Verse,
    },
    /// A fetch for this surah is in flight.
    Loading,
    /// The verse could not be loaded — offline, or the fetch failed.
    Unavailable,
    /// No verse applies — a bismillah clip or a whole-surah fallback file.
    Hidden,
}

/// Which Browse control currently has focus.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BrowseField {
    SurahList,
    FromSurah,
    FromAyah,
    ToSurah,
    ToAyah,
}

impl BrowseField {
    /// Tab-cycle order.
    const ORDER: [BrowseField; 5] = [
        BrowseField::SurahList,
        BrowseField::FromSurah,
        BrowseField::FromAyah,
        BrowseField::ToSurah,
        BrowseField::ToAyah,
    ];

    /// True for the four editable numeric range fields.
    pub fn is_numeric(self) -> bool {
        !matches!(self, BrowseField::SurahList)
    }
}

/// UI-only state for the Browse tab. The committed values live in the focused
/// output's [`PlaybackConfig`].
pub struct BrowseState {
    pub field: BrowseField,
    pub list_state: ListState,
    /// `Some` while the incremental surah search is active.
    pub search: Option<String>,
    /// Digits typed into the focused numeric field before they are committed.
    pub edit_buffer: String,
}

impl BrowseState {
    fn new(initial_cursor: usize) -> Self {
        let mut list_state = ListState::default();
        list_state.select(Some(initial_cursor));
        Self {
            field: BrowseField::SurahList,
            list_state,
            search: None,
            edit_buffer: String::new(),
        }
    }
}

/// Which list the Outputs tab is currently navigating.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OutputsFocus {
    Channels,
    Devices,
}

/// Which pane the Playlists tab is currently navigating.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlaylistPane {
    /// The list of playlists, on the left.
    Playlists,
    /// The tracks within the selected playlist, on the right.
    Items,
}

/// What a confirmed text-input modal should do.
#[derive(Debug, Clone)]
pub enum TextAction {
    CreatePlaylist,
    RenamePlaylist(String),
}

/// What a confirmed yes/no modal should do.
#[derive(Debug, Clone)]
pub enum ConfirmAction {
    DeletePlaylist(String),
}

/// A modal dialog that captures all input until dismissed (§7.7).
pub enum Modal {
    Text {
        title: String,
        input: String,
        action: TextAction,
    },
    Confirm {
        title: String,
        message: String,
        action: ConfirmAction,
    },
}

/// All mutable application state.
pub struct App {
    pub should_quit: bool,
    pub active_tab: Tab,
    pub show_help: bool,

    pub catalog: Catalog,
    pub config: AppConfig,

    /// Always non-empty; `outputs[0]` is the system default.
    pub outputs: Vec<OutputChannel>,
    pub focused_output: usize,

    /// Output devices from the last enumeration.
    pub devices: Vec<AudioDevice>,
    /// Outputs-tab navigation state.
    pub outputs_focus: OutputsFocus,
    pub device_cursor: usize,
    /// Next id to assign to a new output channel.
    next_output_id: OutputId,

    pub browse: BrowseState,
    pub toast: Option<Toast>,

    /// Saved playlists and the Playlists-tab navigation state.
    pub playlists: PlaylistStore,
    /// Cursor into the playlist list (left pane).
    pub playlist_cursor: usize,
    /// Cursor into the selected playlist's tracks (right pane).
    pub playlist_item_cursor: usize,
    /// Which Playlists-tab pane currently has focus.
    pub playlist_pane: PlaylistPane,
    /// The active modal dialog, if any.
    pub modal: Option<Modal>,

    /// Verse text + translation per surah, loaded lazily; key = surah number.
    pub verses: HashMap<u16, SurahVerses>,
    /// Surahs with an in-flight verse fetch — guards against duplicate workers.
    verses_pending: HashSet<u16>,
    /// Directory where fetched verse JSON is cached.
    verses_dir: PathBuf,

    /// Receiver drained by the main loop; engine threads hold cloned senders.
    pub msg_rx: Receiver<AppMessage>,
    /// Kept so background threads (engines, download workers) can be handed clones.
    msg_tx: Sender<AppMessage>,
}

impl App {
    /// Build the app, load config, and spawn the default output's engine thread.
    pub fn new(audio_dir: Option<PathBuf>) -> Self {
        let catalog = Catalog::load();
        let config = AppConfig::load(audio_dir);
        let (msg_tx, msg_rx) = crossbeam_channel::unbounded();

        // Default output channel + its engine, configured from saved state.
        let (cmd_tx, join) = PlaybackEngine::spawn(None, msg_tx.clone(), 0);
        let playback_config = restore_config(&catalog, &config);
        let cursor = catalog
            .surahs
            .iter()
            .position(|s| s.number == playback_config.from_surah)
            .unwrap_or(0);
        let default_output = OutputChannel::new(
            0,
            "System Default".to_string(),
            None,
            playback_config,
            cmd_tx,
            join,
        );

        // Enumerate output devices in the background.
        device::spawn_refresh(msg_tx.clone());

        Self {
            should_quit: false,
            active_tab: Tab::NowPlaying,
            show_help: false,
            catalog,
            config,
            outputs: vec![default_output],
            focused_output: 0,
            devices: Vec::new(),
            outputs_focus: OutputsFocus::Channels,
            device_cursor: 0,
            next_output_id: 1,
            browse: BrowseState::new(cursor),
            toast: None,
            playlists: PlaylistStore::load(),
            playlist_cursor: 0,
            playlist_item_cursor: 0,
            playlist_pane: PlaylistPane::Playlists,
            modal: None,
            verses: HashMap::new(),
            verses_pending: HashSet::new(),
            verses_dir: config::verses_dir(),
            msg_rx,
            msg_tx,
        }
    }

    /// The output channel that transport keys and Browse act on.
    pub fn focused(&self) -> &OutputChannel {
        &self.outputs[self.focused_output]
    }

    /// Clone of the shared message sender — so the main loop can hand it to
    /// the OS media-key bridge, which forwards `MPRemoteCommandCenter` events
    /// back into `handle_message`.
    pub fn msg_tx(&self) -> Sender<AppMessage> {
        self.msg_tx.clone()
    }

    /// Snapshot of what the OS Now Playing widget should show right now.
    /// Built from the focused output so multi-output mode also publishes the
    /// "currently watched" track. Falls back to "Quran TUI" when nothing's
    /// loaded yet so the system's Now Playing UI still has a name to show.
    pub fn now_playing_snapshot(&self) -> NowPlayingSnapshot {
        let output = self.focused();
        let cfg = output.display_config.as_ref().unwrap_or(&output.config);

        // Title: prefer the per-track label (e.g. "18:5") backed by the surah
        // name; fall back to the playing surah's name when only a whole-surah
        // file is loaded.
        let title = match output.current_track_label() {
            Some(label) => format_track_title(&self.catalog, label, output.is_fallback, cfg),
            None => format_default_title(&self.catalog, cfg),
        };
        let artist = self
            .catalog
            .reciter(&cfg.reciter_id)
            .map(|r| r.display_name.clone())
            .unwrap_or_else(|| "Unknown reciter".to_string());

        let state = match output.state {
            EngineState::Playing => NowPlayingState::Playing,
            EngineState::Paused => NowPlayingState::Paused,
            _ => NowPlayingState::Stopped,
        };

        NowPlayingSnapshot {
            title,
            artist,
            album: "Quran".to_string(),
            duration: output.track_len,
            elapsed: output.elapsed,
            state,
        }
    }

    /// Per-frame housekeeping: drop expired toasts.
    pub fn tick(&mut self) {
        if let Some(toast) = &self.toast {
            if Instant::now() >= toast.expires {
                self.toast = None;
            }
        }
    }

    /// Save the current reciter/surah selection to `config.json`.
    pub fn persist_config(&mut self) {
        let cfg = &self.outputs[self.focused_output].config;
        self.config.last_reciter_id = cfg.reciter_id.clone();
        self.config.last_surah = cfg.from_surah;
        self.config.save();
    }

    // --- Input ------------------------------------------------------------

    /// Route a key event. Ignores non-press events (Windows emits release too).
    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        // A modal captures all input until dismissed.
        if self.modal.is_some() {
            self.handle_modal_key(key);
            return;
        }
        if self.show_help {
            self.show_help = false;
            return;
        }
        // Active tabs own most keys while focused.
        if self.active_tab == Tab::Browse && self.handle_browse_key(key) {
            return;
        }
        if self.active_tab == Tab::Outputs && self.handle_outputs_key(key) {
            return;
        }
        if self.active_tab == Tab::Playlists && self.handle_playlists_key(key) {
            return;
        }
        if self.active_tab == Tab::NowPlaying && self.handle_now_playing_key(key) {
            return;
        }
        self.handle_global_key(key);
    }

    /// Modal key handling — captures every key until the modal is dismissed.
    fn handle_modal_key(&mut self, key: KeyEvent) {
        let is_text = matches!(self.modal, Some(Modal::Text { .. }));
        let is_confirm = matches!(self.modal, Some(Modal::Confirm { .. }));
        match key.code {
            KeyCode::Esc => self.modal = None,
            KeyCode::Enter => self.confirm_modal(),
            KeyCode::Char('n') if is_confirm => self.modal = None,
            KeyCode::Char('y') if is_confirm => self.confirm_modal(),
            KeyCode::Backspace if is_text => {
                if let Some(Modal::Text { input, .. }) = &mut self.modal {
                    input.pop();
                }
            }
            KeyCode::Char(c) if is_text => {
                if let Some(Modal::Text { input, .. }) = &mut self.modal {
                    input.push(c);
                }
            }
            _ => {}
        }
    }

    /// Apply the active modal's action and dismiss it.
    fn confirm_modal(&mut self) {
        let Some(modal) = self.modal.take() else {
            return;
        };
        match modal {
            Modal::Text { input, action, .. } => {
                let name = input.trim();
                if name.is_empty() {
                    self.set_toast("Name cannot be empty", ToastKind::Warn);
                    return;
                }
                match action {
                    TextAction::CreatePlaylist => {
                        let id = self.playlists.create(name);
                        self.playlist_cursor = self
                            .playlists
                            .playlists
                            .iter()
                            .position(|p| p.id == id)
                            .unwrap_or(0);
                        self.playlist_item_cursor = 0;
                        self.playlist_pane = PlaylistPane::Playlists;
                        self.set_toast(format!("Created playlist \"{name}\""), ToastKind::Info);
                    }
                    TextAction::RenamePlaylist(id) => {
                        self.playlists.rename(&id, name);
                        self.set_toast("Playlist renamed", ToastKind::Info);
                    }
                }
            }
            Modal::Confirm { action, .. } => match action {
                ConfirmAction::DeletePlaylist(id) => {
                    self.playlists.delete(&id);
                    self.playlist_cursor = self
                        .playlist_cursor
                        .min(self.playlists.playlists.len().saturating_sub(1));
                    self.playlist_item_cursor = 0;
                    self.set_toast("Playlist deleted", ToastKind::Info);
                }
            },
        }
    }

    fn handle_global_key(&mut self, key: KeyEvent) {
        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => self.should_quit = true,
            (KeyCode::Char('q'), _) => self.should_quit = true,
            (KeyCode::Char('?'), _) => self.show_help = true,
            (KeyCode::Tab, _) => self.cycle_tab(1),
            (KeyCode::BackTab, _) => self.cycle_tab(-1),
            (KeyCode::Char('1'), _) => self.active_tab = Tab::NowPlaying,
            (KeyCode::Char('2'), _) => self.active_tab = Tab::Browse,
            (KeyCode::Char('3'), _) => self.active_tab = Tab::Outputs,
            (KeyCode::Char('4'), _) => self.active_tab = Tab::Playlists,

            (KeyCode::Char(' '), _) => self.toggle_play(),
            (KeyCode::Char('s'), _) if self.active_tab != Tab::Browse => self.stop_focused(),
            (KeyCode::Char('n'), _) => self.transport(EngineCommand::Next),
            (KeyCode::Char('p'), _) => self.transport(EngineCommand::Prev),
            (KeyCode::Char('l'), _) => self.toggle_loop(),
            (KeyCode::Char('+') | KeyCode::Char('='), _) => self.adjust_volume(VOLUME_STEP),
            (KeyCode::Char('-'), _) => self.adjust_volume(-VOLUME_STEP),

            // Multi-output: play / pause / stop every output.
            (KeyCode::Char('A'), _) => self.play_all(),
            (KeyCode::Char('P'), _) => self.pause_all(),
            (KeyCode::Char('S'), _) => self.stop_all(),
            _ => {}
        }
    }

    /// Browse-tab key handling. Returns `true` if the key was consumed.
    fn handle_browse_key(&mut self, key: KeyEvent) -> bool {
        if self.browse.search.is_some() {
            return self.handle_search_key(key);
        }
        match key.code {
            KeyCode::Tab => self.cycle_browse_field(1),
            KeyCode::BackTab => self.cycle_browse_field(-1),
            KeyCode::Left => self.cycle_reciter(-1),
            KeyCode::Right => self.cycle_reciter(1),
            KeyCode::Up => self.browse_nav(false),
            KeyCode::Down => self.browse_nav(true),
            KeyCode::Char('/') => self.enter_search(),
            KeyCode::Enter => {
                self.commit_browse_field();
                self.start_playback(self.focused_output, true);
            }
            KeyCode::Char('a') => {
                self.commit_browse_field();
                self.start_playback(self.focused_output, false);
            }
            KeyCode::Char('A') => self.add_browse_selection_to_playlist(),
            KeyCode::Backspace if self.browse.field.is_numeric() => {
                self.browse.edit_buffer.pop();
            }
            KeyCode::Char(c) if c.is_ascii_digit() && self.browse.field.is_numeric() => {
                if self.browse.edit_buffer.len() < 3 {
                    self.browse.edit_buffer.push(c);
                }
            }
            _ => return false,
        }
        true
    }

    /// Surah-search key handling. Returns `true` if the key was consumed.
    fn handle_search_key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return false; // let Ctrl+C reach the global handler
        }
        match key.code {
            KeyCode::Esc | KeyCode::Enter => self.exit_search(),
            KeyCode::Backspace => {
                if let Some(query) = self.browse.search.as_mut() {
                    query.pop();
                }
                self.reset_search_cursor();
            }
            KeyCode::Up => self.move_surah_cursor(-1),
            KeyCode::Down => self.move_surah_cursor(1),
            KeyCode::Char(c) => {
                if let Some(query) = self.browse.search.as_mut() {
                    query.push(c);
                }
                self.reset_search_cursor();
            }
            _ => {}
        }
        true
    }

    // --- Browse logic -----------------------------------------------------

    /// The surah list, filtered by the active search query.
    pub fn filtered_surahs(&self) -> Vec<&Surah> {
        match &self.browse.search {
            None => self.catalog.surahs.iter().collect(),
            Some(query) => {
                let needle = query.to_lowercase();
                self.catalog
                    .surahs
                    .iter()
                    .filter(|s| {
                        s.name_transliterated.to_lowercase().contains(&needle)
                            || s.number.to_string().contains(&needle)
                    })
                    .collect()
            }
        }
    }

    fn enter_search(&mut self) {
        self.browse.field = BrowseField::SurahList;
        self.browse.search = Some(String::new());
        self.browse.list_state.select(Some(0));
    }

    fn exit_search(&mut self) {
        self.browse.search = None;
        // Re-anchor the cursor onto the selected surah in the full list.
        let target = self.outputs[self.focused_output].config.from_surah;
        let index = self
            .catalog
            .surahs
            .iter()
            .position(|s| s.number == target)
            .unwrap_or(0);
        self.browse.list_state.select(Some(index));
    }

    fn reset_search_cursor(&mut self) {
        self.browse.list_state.select(Some(0));
        self.apply_surah_selection();
    }

    fn browse_nav(&mut self, down: bool) {
        if self.browse.field == BrowseField::SurahList {
            self.move_surah_cursor(if down { 1 } else { -1 });
        } else {
            self.commit_browse_field();
            self.adjust_numeric_field(if down { -1 } else { 1 });
        }
    }

    fn move_surah_cursor(&mut self, delta: isize) {
        let len = self.filtered_surahs().len();
        if len == 0 {
            return;
        }
        let current = self.browse.list_state.selected().unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, len as isize - 1) as usize;
        self.browse.list_state.select(Some(next));
        self.apply_surah_selection();
    }

    /// Apply the highlighted surah to the focused config (from = to = surah).
    fn apply_surah_selection(&mut self) {
        let selected = self.browse.list_state.selected().unwrap_or(0);
        let picked = self
            .filtered_surahs()
            .get(selected)
            .map(|s| (s.number, s.ayah_count));
        if let Some((number, ayah_count)) = picked {
            let cfg = &mut self.outputs[self.focused_output].config;
            cfg.from_surah = number;
            cfg.to_surah = number;
            cfg.from_ayah = 1;
            cfg.to_ayah = ayah_count;
        }
    }

    fn cycle_browse_field(&mut self, delta: isize) {
        self.commit_browse_field();
        let order = BrowseField::ORDER;
        let current = order
            .iter()
            .position(|&f| f == self.browse.field)
            .unwrap_or(0) as isize;
        let next = (current + delta).rem_euclid(order.len() as isize) as usize;
        self.browse.field = order[next];
    }

    fn cycle_reciter(&mut self, delta: isize) {
        if self.catalog.reciters.is_empty() {
            return;
        }
        let len = self.catalog.reciters.len() as isize;
        let current = {
            let id = &self.outputs[self.focused_output].config.reciter_id;
            self.catalog
                .reciters
                .iter()
                .position(|r| &r.id == id)
                .unwrap_or(0) as isize
        };
        let next = (current + delta).rem_euclid(len) as usize;
        self.outputs[self.focused_output].config.reciter_id =
            self.catalog.reciters[next].id.clone();
    }

    /// Commit the digits typed into the focused numeric field.
    fn commit_browse_field(&mut self) {
        if self.browse.field.is_numeric() && !self.browse.edit_buffer.is_empty() {
            if let Ok(raw) = self.browse.edit_buffer.parse::<u32>() {
                self.set_numeric_field(self.browse.field, raw);
            }
        }
        self.browse.edit_buffer.clear();
    }

    fn adjust_numeric_field(&mut self, delta: i32) {
        let field = self.browse.field;
        if !field.is_numeric() {
            return;
        }
        let current = self.numeric_field_value(field) as i32;
        let next = (current + delta).max(0) as u32;
        self.set_numeric_field(field, next);
    }

    fn numeric_field_value(&self, field: BrowseField) -> u16 {
        let cfg = &self.outputs[self.focused_output].config;
        match field {
            BrowseField::FromSurah => cfg.from_surah,
            BrowseField::FromAyah => cfg.from_ayah,
            BrowseField::ToSurah => cfg.to_surah,
            BrowseField::ToAyah => cfg.to_ayah,
            BrowseField::SurahList => 0,
        }
    }

    fn set_numeric_field(&mut self, field: BrowseField, raw: u32) {
        let value = raw.min(u16::MAX as u32) as u16;
        {
            let cfg = &mut self.outputs[self.focused_output].config;
            match field {
                BrowseField::FromSurah => cfg.from_surah = value,
                BrowseField::FromAyah => cfg.from_ayah = value,
                BrowseField::ToSurah => cfg.to_surah = value,
                BrowseField::ToAyah => cfg.to_ayah = value,
                BrowseField::SurahList => {}
            }
        }
        self.clamp_focused_config();
    }

    /// Clamp the focused output's surah/ayah bounds to valid catalog ranges.
    fn clamp_focused_config(&mut self) {
        let cfg = self.outputs[self.focused_output].config.clone();
        let from_surah = cfg.from_surah.clamp(1, 114);
        let to_surah = cfg.to_surah.clamp(1, 114);
        let from_count = self.ayah_count(from_surah);
        let to_count = self.ayah_count(to_surah);
        let target = &mut self.outputs[self.focused_output].config;
        target.from_surah = from_surah;
        target.to_surah = to_surah;
        target.from_ayah = cfg.from_ayah.clamp(1, from_count);
        target.to_ayah = cfg.to_ayah.clamp(1, to_count);
    }

    fn ayah_count(&self, surah_number: u16) -> u16 {
        self.catalog
            .surah(surah_number)
            .map(|s| s.ayah_count)
            .unwrap_or(1)
    }

    // --- Transport --------------------------------------------------------

    fn cycle_tab(&mut self, delta: isize) {
        let len = Tab::ALL.len() as isize;
        let next = (self.active_tab.index() as isize + delta).rem_euclid(len);
        self.active_tab = Tab::ALL[next as usize];
    }

    fn toggle_play(&mut self) {
        let index = self.focused_output;
        let output = &self.outputs[index];
        match output.state {
            EngineState::Playing => output.send(EngineCommand::Pause),
            _ if output.track_total == 0 => self.start_playback(index, true),
            _ => output.send(EngineCommand::Play),
        }
    }

    fn stop_focused(&mut self) {
        let output = &mut self.outputs[self.focused_output];
        output.send(EngineCommand::Stop);
        output.track_index = 0;
        output.track_total = 0;
        output.track_labels.clear();
        output.elapsed = Duration::ZERO;
        output.display_config = None;
    }

    fn transport(&self, cmd: EngineCommand) {
        self.outputs[self.focused_output].send(cmd);
    }

    fn toggle_loop(&mut self) {
        let output = &mut self.outputs[self.focused_output];
        output.config.loop_enabled = !output.config.loop_enabled;
        output.send(EngineCommand::SetLoop(output.config.loop_enabled));
    }

    fn adjust_volume(&mut self, delta: f32) {
        let output = &mut self.outputs[self.focused_output];
        output.config.volume = (output.config.volume + delta).clamp(0.0, 1.0);
        output.send(EngineCommand::SetVolume(output.config.volume));
    }

    /// Resolve the focused output's config to audio files and load the engine.
    /// Missing files trigger an on-demand download that auto-plays on completion.
    fn start_playback(&mut self, index: usize, autoplay: bool) {
        if self.outputs[index].download.is_some() {
            self.set_toast("A download is already in progress", ToastKind::Warn);
            return;
        }
        let cfg = self.outputs[index].config.clone();
        let Some(reciter) = self.catalog.reciter(&cfg.reciter_id).cloned() else {
            self.set_toast("Unknown reciter", ToastKind::Error);
            return;
        };
        let segments = playback_segments(
            &self.catalog.surahs,
            cfg.from_surah,
            cfg.to_surah,
            cfg.from_ayah,
            cfg.to_ayah,
        );
        if segments.is_empty() {
            self.set_toast("Nothing to play for that range", ToastKind::Warn);
            return;
        }
        self.ensure_verses(&segments);

        match resolver::resolve(&segments, &reciter, &self.config.audio_root) {
            Resolution::Local(tracks) => {
                self.outputs[index].display_config = Some(cfg.clone());
                self.apply_local(index, tracks, &segments, autoplay);
            }
            Resolution::WholeSurahFallback(path) => {
                self.outputs[index].display_config = Some(cfg.clone());
                self.apply_whole_surah(index, path, cfg.from_surah, autoplay);
                self.set_toast(
                    "Per-ayah files absent — playing the whole-surah file",
                    ToastKind::Info,
                );
            }
            Resolution::Missing { .. } => {
                self.outputs[index].display_config = Some(cfg.clone());
                self.begin_download(index, &segments, &reciter, autoplay);
            }
        }
    }

    /// Load a per-ayah playlist into the output's engine.
    fn apply_local(
        &mut self,
        index: usize,
        tracks: Vec<PathBuf>,
        segments: &[PlaybackSegment],
        autoplay: bool,
    ) {
        let labels = resolver::per_ayah_labels(segments);
        let output = &mut self.outputs[index];
        output.is_fallback = false;
        output.track_total = tracks.len();
        output.track_index = 0;
        output.track_labels = labels;
        let (volume, loop_enabled) = (output.config.volume, output.config.loop_enabled);
        output.send(EngineCommand::SetVolume(volume));
        output.send(EngineCommand::Load {
            tracks,
            loop_enabled,
            autoplay,
        });
    }

    /// Load a single whole-surah file into the output's engine.
    fn apply_whole_surah(&mut self, index: usize, path: PathBuf, from_surah: u16, autoplay: bool) {
        let label = self
            .catalog
            .surah(from_surah)
            .map(|s| format!("{} (full surah)", s.name_transliterated))
            .unwrap_or_else(|| "Full surah".to_string());
        let output = &mut self.outputs[index];
        output.is_fallback = true;
        output.track_total = 1;
        output.track_index = 0;
        output.track_labels = vec![label];
        let (volume, loop_enabled) = (output.config.volume, output.config.loop_enabled);
        output.send(EngineCommand::SetVolume(volume));
        output.send(EngineCommand::Load {
            tracks: vec![path],
            loop_enabled,
            autoplay,
        });
    }

    /// Spawn a worker to download the missing files for `index`, and load the
    /// engine optimistically with the full deterministic track list. The
    /// engine waits on tracks whose files aren't on disk yet and picks them
    /// up as the downloader writes them — so playback starts as soon as the
    /// first ayah is ready, without waiting for the whole range to finish.
    fn begin_download(
        &mut self,
        index: usize,
        segments: &[PlaybackSegment],
        reciter: &Reciter,
        autoplay: bool,
    ) {
        let missing = resolver::missing_files(segments, reciter, &self.config.audio_root);
        if missing.is_empty() {
            self.set_toast("Nothing to download", ToastKind::Warn);
            return;
        }
        let total = missing.len();

        let tracks = resolver::per_ayah_paths(segments, reciter, &self.config.audio_root);
        self.apply_local(index, tracks, segments, autoplay);

        self.outputs[index].download = Some(DownloadProgress {
            done: 0,
            total,
            label: "starting".to_string(),
            autoplay,
            playlist_id: None,
        });
        let output_id = self.outputs[index].id;
        downloader::download_missing(missing, self.msg_tx.clone(), output_id);
        self.set_toast(
            format!("Downloading {total} ayah file(s) — playing as they arrive"),
            ToastKind::Info,
        );
    }

    // --- Verses -----------------------------------------------------------

    /// Ensure every surah in `segments` has its verse text loaded or being
    /// fetched. Already-cached and already-pending surahs are skipped, so this
    /// is cheap to call on every `start_playback`.
    fn ensure_verses(&mut self, segments: &[PlaybackSegment]) {
        for segment in segments {
            let surah = segment.surah.number;
            if self.verses.contains_key(&surah) || !self.verses_pending.insert(surah) {
                continue;
            }
            crate::content::verses::fetch_surah(
                surah,
                self.verses_dir.clone(),
                self.msg_tx.clone(),
            );
        }
    }

    /// Verse-panel state for the focused output's current track.
    pub fn current_verse(&self) -> VerseView<'_> {
        let output = self.focused();
        if output.is_fallback {
            return VerseView::Hidden;
        }
        let Some((surah, ayah)) = output.current_track_label().and_then(parse_track_ref) else {
            return VerseView::Hidden;
        };
        if let Some(surah_verses) = self.verses.get(&surah) {
            return match surah_verses.ayah(ayah) {
                Some(verse) => VerseView::Ready { surah, ayah, verse },
                None => VerseView::Unavailable,
            };
        }
        if self.verses_pending.contains(&surah) {
            VerseView::Loading
        } else {
            VerseView::Unavailable
        }
    }

    // --- Multi-output -----------------------------------------------------

    /// Outputs-tab key handling. Returns `true` if the key was consumed.
    fn handle_outputs_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Tab | KeyCode::BackTab => self.toggle_outputs_focus(),
            KeyCode::Up => self.outputs_nav(-1),
            KeyCode::Down => self.outputs_nav(1),
            KeyCode::Char('+') => self.add_output(),
            KeyCode::Char('d') => self.delete_focused_output(),
            KeyCode::Char('r') => self.refresh_devices(),
            KeyCode::Enter => self.bind_focused_to_selected_device(),
            _ => return false,
        }
        true
    }

    /// Now Playing key handling: surfs the output cards in multi-output mode.
    /// Returns `true` if the key was consumed.
    fn handle_now_playing_key(&mut self, key: KeyEvent) -> bool {
        if self.outputs.len() <= 1 {
            return false;
        }
        match key.code {
            KeyCode::Up => self.focus_output(-1),
            KeyCode::Down => self.focus_output(1),
            _ => return false,
        }
        true
    }

    fn toggle_outputs_focus(&mut self) {
        self.outputs_focus = match self.outputs_focus {
            OutputsFocus::Channels => OutputsFocus::Devices,
            OutputsFocus::Devices => OutputsFocus::Channels,
        };
    }

    fn outputs_nav(&mut self, delta: isize) {
        match self.outputs_focus {
            OutputsFocus::Channels => self.focus_output(delta),
            OutputsFocus::Devices => {
                if self.devices.is_empty() {
                    return;
                }
                let last = self.devices.len() as isize - 1;
                let next = (self.device_cursor as isize + delta).clamp(0, last);
                self.device_cursor = next as usize;
            }
        }
    }

    /// Move the focused output by `delta`, clamped to the output list.
    fn focus_output(&mut self, delta: isize) {
        let last = self.outputs.len() as isize - 1;
        let next = (self.focused_output as isize + delta).clamp(0, last);
        self.focused_output = next as usize;
    }

    /// Add a new output bound to the device under the device cursor.
    fn add_output(&mut self) {
        let Some(device_info) = self.devices.get(self.device_cursor).cloned() else {
            self.set_toast("No device selected — press r to refresh", ToastKind::Warn);
            return;
        };
        let cpal_device = device::find_device(&device_info.name);
        if cpal_device.is_none() {
            self.set_toast("That device is no longer available", ToastKind::Warn);
            return;
        }
        let id = self.next_output_id;
        self.next_output_id += 1;
        let (cmd_tx, join) = PlaybackEngine::spawn(cpal_device, self.msg_tx.clone(), id);
        let config = self.outputs[0].config.clone();
        let output = OutputChannel::new(
            id,
            device_info.name.clone(),
            Some(device_info.name.clone()),
            config,
            cmd_tx,
            join,
        );
        self.outputs.push(output);
        self.focused_output = self.outputs.len() - 1;
        self.outputs_focus = OutputsFocus::Channels;
        self.set_toast(
            format!("Added output: {}", device_info.name),
            ToastKind::Info,
        );
    }

    /// Remove the focused output. Output 0 (the default) cannot be removed.
    fn delete_focused_output(&mut self) {
        let index = self.focused_output;
        if self.outputs[index].id == 0 {
            self.set_toast("The default output cannot be removed", ToastKind::Warn);
            return;
        }
        let mut output = self.outputs.remove(index);
        output.send(EngineCommand::Shutdown);
        if let Some(join) = output.join_handle.take() {
            let _ = join.join();
        }
        self.focused_output = self.focused_output.min(self.outputs.len() - 1);
        self.set_toast("Output removed", ToastKind::Info);
    }

    /// Rebind the focused output to the selected device (respawns its engine).
    fn bind_focused_to_selected_device(&mut self) {
        if self.outputs_focus != OutputsFocus::Devices {
            return;
        }
        let Some(device_info) = self.devices.get(self.device_cursor).cloned() else {
            return;
        };
        let cpal_device = device::find_device(&device_info.name);
        let index = self.focused_output;
        let id = self.outputs[index].id;

        // Tear down the old engine.
        self.outputs[index].send(EngineCommand::Shutdown);
        if let Some(join) = self.outputs[index].join_handle.take() {
            let _ = join.join();
        }
        // Spawn a fresh engine on the new device.
        let (cmd_tx, join) = PlaybackEngine::spawn(cpal_device, self.msg_tx.clone(), id);
        let output = &mut self.outputs[index];
        output.cmd_tx = cmd_tx;
        output.join_handle = Some(join);
        output.device_name = Some(device_info.name.clone());
        output.label = device_info.name.clone();
        output.state = EngineState::Idle;
        output.track_index = 0;
        output.track_total = 0;
        output.track_labels.clear();
        output.elapsed = Duration::ZERO;
        output.display_config = None;
        self.set_toast(format!("Bound to {}", device_info.name), ToastKind::Info);
    }

    fn refresh_devices(&mut self) {
        device::spawn_refresh(self.msg_tx.clone());
        self.set_toast("Refreshing devices…", ToastKind::Info);
    }

    /// Start playback on every output.
    fn play_all(&mut self) {
        for index in 0..self.outputs.len() {
            if self.outputs[index].track_total == 0 {
                self.start_playback(index, true);
            } else {
                self.outputs[index].send(EngineCommand::Play);
            }
        }
    }

    fn pause_all(&self) {
        for output in &self.outputs {
            output.send(EngineCommand::Pause);
        }
    }

    fn stop_all(&mut self) {
        for output in &mut self.outputs {
            output.send(EngineCommand::Stop);
            output.track_index = 0;
            output.track_total = 0;
            output.track_labels.clear();
            output.elapsed = Duration::ZERO;
            output.display_config = None;
        }
    }

    // --- Playlists --------------------------------------------------------

    /// Playlists-tab key handling. Returns `true` if the key was consumed.
    fn handle_playlists_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Tab | KeyCode::BackTab => self.toggle_playlist_pane(),
            KeyCode::Up => self.playlist_nav(-1),
            KeyCode::Down => self.playlist_nav(1),
            KeyCode::Char('n') => self.open_create_playlist_modal(),
            KeyCode::Char('r') => self.open_rename_playlist_modal(),
            KeyCode::Char('d') => self.delete_at_playlist_cursor(),
            KeyCode::Char('a') => self.jump_to_browse_for_playlist(),
            KeyCode::Enter => self.play_selected_playlist(),
            _ => return false,
        }
        true
    }

    fn toggle_playlist_pane(&mut self) {
        self.playlist_pane = match self.playlist_pane {
            PlaylistPane::Playlists => PlaylistPane::Items,
            PlaylistPane::Items => PlaylistPane::Playlists,
        };
    }

    /// Move the cursor within whichever Playlists-tab pane has focus.
    fn playlist_nav(&mut self, delta: isize) {
        match self.playlist_pane {
            PlaylistPane::Playlists => {
                if self.playlists.playlists.is_empty() {
                    return;
                }
                let last = self.playlists.playlists.len() as isize - 1;
                self.playlist_cursor =
                    (self.playlist_cursor as isize + delta).clamp(0, last) as usize;
                // A different playlist is selected — re-anchor the item cursor.
                self.playlist_item_cursor = 0;
            }
            PlaylistPane::Items => {
                let len = self.selected_playlist_len();
                if len == 0 {
                    return;
                }
                let last = len as isize - 1;
                self.playlist_item_cursor =
                    (self.playlist_item_cursor as isize + delta).clamp(0, last) as usize;
            }
        }
    }

    /// Number of tracks in the currently selected playlist.
    fn selected_playlist_len(&self) -> usize {
        self.playlists
            .playlists
            .get(self.playlist_cursor)
            .map(|p| p.items.len())
            .unwrap_or(0)
    }

    fn open_create_playlist_modal(&mut self) {
        let default_name = format!("Playlist {}", self.playlists.playlists.len() + 1);
        self.modal = Some(Modal::Text {
            title: "New playlist".to_string(),
            input: default_name,
            action: TextAction::CreatePlaylist,
        });
    }

    fn open_rename_playlist_modal(&mut self) {
        let Some(playlist) = self.playlists.playlists.get(self.playlist_cursor) else {
            self.set_toast("No playlist selected", ToastKind::Warn);
            return;
        };
        self.modal = Some(Modal::Text {
            title: "Rename playlist".to_string(),
            input: playlist.name.clone(),
            action: TextAction::RenamePlaylist(playlist.id.clone()),
        });
    }

    /// `d` — delete the selected playlist (with a confirm), or, on the Items
    /// pane, remove the selected track from the playlist immediately.
    fn delete_at_playlist_cursor(&mut self) {
        match self.playlist_pane {
            PlaylistPane::Playlists => {
                let Some(playlist) = self.playlists.playlists.get(self.playlist_cursor) else {
                    self.set_toast("No playlist selected", ToastKind::Warn);
                    return;
                };
                self.modal = Some(Modal::Confirm {
                    title: "Delete playlist".to_string(),
                    message: format!("Delete \"{}\"?", playlist.name),
                    action: ConfirmAction::DeletePlaylist(playlist.id.clone()),
                });
            }
            PlaylistPane::Items => {
                let Some(playlist) = self.playlists.playlists.get(self.playlist_cursor) else {
                    return;
                };
                if playlist.items.is_empty() {
                    self.set_toast("No track to remove", ToastKind::Warn);
                    return;
                }
                let id = playlist.id.clone();
                self.playlists.remove_item(&id, self.playlist_item_cursor);
                let new_len = self.selected_playlist_len();
                self.playlist_item_cursor =
                    self.playlist_item_cursor.min(new_len.saturating_sub(1));
                self.set_toast("Track removed", ToastKind::Info);
            }
        }
    }

    /// `a` — jump to Browse to compose a track for the selected playlist.
    fn jump_to_browse_for_playlist(&mut self) {
        let Some(playlist) = self.playlists.playlists.get(self.playlist_cursor) else {
            self.set_toast("Create a playlist first — press n", ToastKind::Warn);
            return;
        };
        let name = playlist.name.clone();
        self.active_tab = Tab::Browse;
        self.set_toast(
            format!("Pick a range, then press  A  to add it to \"{name}\""),
            ToastKind::Info,
        );
    }

    /// `A` in Browse — append the current Browse selection (reciter + surah +
    /// ayah range) to the selected playlist as a new track.
    fn add_browse_selection_to_playlist(&mut self) {
        self.commit_browse_field();
        let Some(playlist) = self.playlists.playlists.get(self.playlist_cursor) else {
            self.set_toast(
                "No playlist yet — create one on the Playlists tab (4)",
                ToastKind::Warn,
            );
            return;
        };
        let (id, name) = (playlist.id.clone(), playlist.name.clone());
        let cfg = &self.outputs[self.focused_output].config;
        let item = PlaylistItem {
            reciter_id: cfg.reciter_id.clone(),
            from_surah: cfg.from_surah,
            from_ayah: cfg.from_ayah,
            to_surah: cfg.to_surah,
            to_ayah: cfg.to_ayah,
        };
        self.playlists.add_item(&id, item);
        self.set_toast(format!("Added a track to \"{name}\""), ToastKind::Info);
    }

    /// Enter on the Playlists tab — play the selected playlist on the focused
    /// output.
    fn play_selected_playlist(&mut self) {
        let Some(playlist) = self.playlists.playlists.get(self.playlist_cursor).cloned() else {
            self.set_toast("No playlist selected", ToastKind::Warn);
            return;
        };
        self.play_playlist(self.focused_output, &playlist, true);
    }

    /// Resolve every item of `playlist` into one flat track list and load it
    /// into output `index`'s engine. Missing files trigger a download that
    /// re-resolves and plays the playlist once complete.
    fn play_playlist(&mut self, index: usize, playlist: &Playlist, autoplay: bool) {
        if self.outputs[index].download.is_some() {
            self.set_toast("A download is already in progress", ToastKind::Warn);
            return;
        }
        if playlist.items.is_empty() {
            self.set_toast("This playlist is empty — add tracks first", ToastKind::Warn);
            return;
        }

        let mut tracks = Vec::new();
        let mut labels = Vec::new();
        let mut missing = Vec::new();
        let mut all_segments = Vec::new();
        for item in &playlist.items {
            let Some(reciter) = self.catalog.reciter(&item.reciter_id).cloned() else {
                continue;
            };
            let segments = playback_segments(
                &self.catalog.surahs,
                item.from_surah,
                item.to_surah,
                item.from_ayah,
                item.to_ayah,
            );
            let root = &self.config.audio_root;
            tracks.extend(resolver::per_ayah_paths(&segments, &reciter, root));
            labels.extend(resolver::per_ayah_labels(&segments));
            missing.extend(resolver::missing_files(&segments, &reciter, root));
            all_segments.extend(segments);
        }

        if tracks.is_empty() {
            self.set_toast("Nothing to play in this playlist", ToastKind::Warn);
            return;
        }
        self.ensure_verses(&all_segments);

        // Load the engine optimistically with the full deterministic track
        // list — files that aren't on disk yet are filled in as the downloader
        // writes them, so playback starts as soon as the first track is ready.
        self.apply_playlist(index, playlist, tracks, labels, autoplay);

        if !missing.is_empty() {
            let total = missing.len();
            self.outputs[index].download = Some(DownloadProgress {
                done: 0,
                total,
                label: "starting".to_string(),
                autoplay,
                playlist_id: Some(playlist.id.clone()),
            });
            let output_id = self.outputs[index].id;
            downloader::download_missing(missing, self.msg_tx.clone(), output_id);
            self.set_toast(
                format!(
                    "Downloading {total} ayah file(s) for \"{}\" — playing as they arrive",
                    playlist.name
                ),
                ToastKind::Info,
            );
        }
    }

    /// Load a fully-resolved playlist track list into output `index`'s engine.
    fn apply_playlist(
        &mut self,
        index: usize,
        playlist: &Playlist,
        tracks: Vec<PathBuf>,
        labels: Vec<String>,
        autoplay: bool,
    ) {
        // Now Playing reads `display_config`; show the first track as the
        // header. The per-track label + verse panel still follow live.
        let first = &playlist.items[0];
        let display = PlaybackConfig {
            reciter_id: first.reciter_id.clone(),
            from_surah: first.from_surah,
            from_ayah: first.from_ayah,
            to_surah: first.to_surah,
            to_ayah: first.to_ayah,
            volume: self.outputs[index].config.volume,
            loop_enabled: self.outputs[index].config.loop_enabled,
        };
        let output = &mut self.outputs[index];
        output.is_fallback = false;
        output.track_total = tracks.len();
        output.track_index = 0;
        output.track_labels = labels;
        output.display_config = Some(display);
        let (volume, loop_enabled) = (output.config.volume, output.config.loop_enabled);
        output.send(EngineCommand::SetVolume(volume));
        output.send(EngineCommand::Load {
            tracks,
            loop_enabled,
            autoplay,
        });
    }

    // --- Messages ---------------------------------------------------------

    /// Drain one background-thread message.
    pub fn handle_message(&mut self, msg: AppMessage) {
        match msg {
            AppMessage::Engine(id, event) => self.handle_engine_event(id, event),
            AppMessage::Download(id, update) => self.handle_download_update(id, update),
            AppMessage::DevicesRefreshed(devices) => {
                self.devices = devices;
                self.device_cursor = self.device_cursor.min(self.devices.len().saturating_sub(1));
            }
            AppMessage::Verses { surah, result } => self.handle_verses_loaded(surah, result),
            AppMessage::Error(err) => {
                tracing::error!("background error: {err}");
                self.set_toast(err, ToastKind::Error);
            }
            AppMessage::MediaControl(action) => self.handle_media_action(action),
        }
    }

    /// Apply an OS media-control event (Mac media keys, Bluetooth, etc.) to the
    /// focused output. Matches the in-app keys: space, n, p, s — so behaviour
    /// stays consistent whether the user presses a key in the TUI or F8 in any
    /// other window.
    fn handle_media_action(&mut self, action: MediaAction) {
        match action {
            MediaAction::Toggle => self.toggle_play(),
            MediaAction::Play => {
                let index = self.focused_output;
                if self.outputs[index].track_total == 0 {
                    self.start_playback(index, true);
                } else {
                    self.outputs[index].send(EngineCommand::Play);
                }
            }
            MediaAction::Pause => self.transport(EngineCommand::Pause),
            MediaAction::Next => self.transport(EngineCommand::Next),
            MediaAction::Prev => self.transport(EngineCommand::Prev),
            MediaAction::Stop => self.stop_focused(),
        }
    }

    /// Store a completed verse fetch, or log and ignore a failure. A failure
    /// stays silent in the UI — the verse panel falls back to "unavailable".
    fn handle_verses_loaded(&mut self, surah: u16, result: Result<SurahVerses, String>) {
        self.verses_pending.remove(&surah);
        match result {
            Ok(surah_verses) => {
                self.verses.insert(surah, surah_verses);
            }
            Err(err) => tracing::warn!("verse fetch for surah {surah} failed: {err}"),
        }
    }

    fn handle_download_update(&mut self, id: OutputId, update: DownloadUpdate) {
        let Some(index) = self.outputs.iter().position(|o| o.id == id) else {
            return;
        };
        match update {
            DownloadUpdate::Progress { done, total, label } => {
                if let Some(progress) = &mut self.outputs[index].download {
                    progress.done = done;
                    progress.total = total;
                    progress.label = label;
                }
            }
            DownloadUpdate::Completed => {
                // The engine has been playing as files arrived — nothing left
                // to load. Just clear the download indicator.
                self.outputs[index].download = None;
                self.set_toast("Download complete", ToastKind::Info);
            }
            DownloadUpdate::Failed(err) => {
                let was_playlist = self.outputs[index]
                    .download
                    .as_ref()
                    .map(|p| p.playlist_id.is_some())
                    .unwrap_or(false);
                self.outputs[index].download = None;
                let output = &self.outputs[index];
                let already_played = output.track_index > 0 || !output.elapsed.is_zero();
                // If something already played, or it's a playlist (no whole-
                // surah fallback applies), truncate at the missing file so the
                // engine finishes cleanly on what's already downloaded.
                // Otherwise fall back to the whole-surah file if one exists.
                if already_played || was_playlist {
                    output.send(EngineCommand::DropMissing);
                    self.set_toast(format!("Download failed: {err}"), ToastKind::Error);
                } else {
                    output.send(EngineCommand::Stop);
                    self.fallback_after_failed_download(index, &err);
                }
            }
        }
    }

    /// After a failed download, play the whole-surah file if one exists.
    fn fallback_after_failed_download(&mut self, index: usize, err: &str) {
        let cfg = self.outputs[index].config.clone();
        let segments = playback_segments(
            &self.catalog.surahs,
            cfg.from_surah,
            cfg.to_surah,
            cfg.from_ayah,
            cfg.to_ayah,
        );
        let resolution = self
            .catalog
            .reciter(&cfg.reciter_id)
            .cloned()
            .map(|reciter| resolver::resolve(&segments, &reciter, &self.config.audio_root));
        if let Some(Resolution::WholeSurahFallback(path)) = resolution {
            self.apply_whole_surah(index, path, cfg.from_surah, true);
            self.set_toast(
                format!("Download failed ({err}) — playing whole-surah file"),
                ToastKind::Warn,
            );
        } else {
            self.set_toast(format!("Download failed: {err}"), ToastKind::Error);
        }
    }

    fn handle_engine_event(&mut self, id: OutputId, event: EngineEvent) {
        let Some(output) = self.outputs.iter_mut().find(|o| o.id == id) else {
            return;
        };
        match event {
            EngineEvent::StateChanged(state) => output.state = state,
            EngineEvent::TrackChanged { index, total } => {
                output.track_index = index;
                output.track_total = total;
            }
            EngineEvent::Progress { elapsed, track_len } => {
                output.elapsed = elapsed;
                output.track_len = track_len;
            }
            EngineEvent::Finished => {
                output.state = EngineState::Idle;
                output.elapsed = Duration::ZERO;
                output.track_index = 0;
                output.track_total = 0;
                output.track_labels.clear();
                output.display_config = None;
            }
        }
    }

    fn set_toast(&mut self, text: impl Into<String>, kind: ToastKind) {
        self.toast = Some(Toast {
            text: text.into(),
            kind,
            expires: Instant::now() + TOAST_TTL,
        });
    }
}

impl Drop for App {
    fn drop(&mut self) {
        // Graceful shutdown: ask every engine to stop its sink before exit.
        for output in &self.outputs {
            let _ = output.cmd_tx.send(EngineCommand::Shutdown);
        }
    }
}

/// Build the default output's [`PlaybackConfig`] from saved app config,
/// validating the reciter and surah against the catalog.
fn restore_config(catalog: &Catalog, config: &AppConfig) -> PlaybackConfig {
    let reciter_id = if catalog.reciter(&config.last_reciter_id).is_some() {
        config.last_reciter_id.clone()
    } else {
        catalog
            .reciters
            .first()
            .map(|r| r.id.clone())
            .unwrap_or_default()
    };
    let surah = catalog
        .surah(config.last_surah.clamp(1, 114))
        .or_else(|| catalog.surahs.first());
    let (from_surah, ayah_count) = surah.map(|s| (s.number, s.ayah_count)).unwrap_or((1, 7));

    PlaybackConfig {
        reciter_id,
        from_surah,
        to_surah: from_surah,
        from_ayah: 1,
        to_ayah: ayah_count,
        ..PlaybackConfig::default()
    }
}

/// Parse a track label such as `"18:5"` into `(surah, ayah)`. Non-ayah labels
/// like `"Bismillah"` yield `None`.
pub fn parse_track_ref(label: &str) -> Option<(u16, u16)> {
    let (surah, ayah) = label.split_once(':')?;
    Some((surah.parse().ok()?, ayah.parse().ok()?))
}

/// Now Playing title for the currently playing track. Handles three cases:
/// per-ayah ("Al-Kahf 5"), whole-surah fallback ("Al-Kahf (full)"), and bare
/// labels we don't recognise (`"Bismillah"`), which pass through verbatim.
fn format_track_title(
    catalog: &Catalog,
    label: &str,
    is_fallback: bool,
    cfg: &PlaybackConfig,
) -> String {
    if is_fallback {
        return format_default_title(catalog, cfg);
    }
    match parse_track_ref(label) {
        Some((surah, ayah)) => match catalog.surah(surah) {
            Some(s) => format!("{} {ayah}", s.name_transliterated),
            None => label.to_string(),
        },
        None => label.to_string(),
    }
}

/// Now Playing title when nothing is loaded yet, or when only a whole-surah
/// file is playing. Pulls the surah name from the catalog and tacks on the
/// ayah range if the user picked something narrower than the whole surah.
fn format_default_title(catalog: &Catalog, cfg: &PlaybackConfig) -> String {
    let Some(surah) = catalog.surah(cfg.from_surah) else {
        return "Quran TUI".to_string();
    };
    if cfg.from_surah == cfg.to_surah && cfg.from_ayah == 1 && cfg.to_ayah == surah.ayah_count {
        surah.name_transliterated.clone()
    } else if cfg.from_surah == cfg.to_surah {
        format!(
            "{} {}–{}",
            surah.name_transliterated, cfg.from_ayah, cfg.to_ayah
        )
    } else {
        format!("{} …", surah.name_transliterated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::path::PathBuf;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn special(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn test_app() -> App {
        let assets = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../TestAssets");
        App::new(Some(assets))
    }

    #[test]
    fn track_ref_parsing() {
        assert_eq!(parse_track_ref("18:5"), Some((18, 5)));
        assert_eq!(parse_track_ref("112:1"), Some((112, 1)));
        assert_eq!(parse_track_ref("Bismillah"), None);
        assert_eq!(parse_track_ref("18:"), None);
        assert_eq!(parse_track_ref("18:x"), None);
    }

    #[test]
    fn browse_search_selects_a_surah() {
        let mut app = test_app();
        app.handle_key(key('2'));
        assert_eq!(app.active_tab, Tab::Browse);

        app.handle_key(key('/'));
        assert!(app.browse.search.is_some());
        for c in "kahf".chars() {
            app.handle_key(key(c));
        }
        app.handle_key(special(KeyCode::Esc));

        let cfg = &app.focused().config;
        assert_eq!(cfg.from_surah, 18);
        assert_eq!(cfg.to_surah, 18);
        assert_eq!(cfg.to_ayah, 110);
    }

    #[test]
    fn numeric_field_edits_are_clamped() {
        let mut app = test_app();
        app.handle_key(key('2'));
        app.handle_key(key('/'));
        for c in "fatihah".chars() {
            app.handle_key(key(c));
        }
        app.handle_key(special(KeyCode::Esc));
        assert_eq!(app.focused().config.from_surah, 1);

        // SurahList -> FromSurah -> FromAyah
        app.handle_key(special(KeyCode::Tab));
        app.handle_key(special(KeyCode::Tab));
        assert_eq!(app.browse.field, BrowseField::FromAyah);

        app.handle_key(key('9'));
        app.handle_key(key('9'));
        app.handle_key(special(KeyCode::Tab)); // commit

        // Al-Fatihah has 7 ayahs, so 99 clamps to 7.
        assert_eq!(app.focused().config.from_ayah, 7);
    }

    #[test]
    fn reciter_cycles_with_arrow_keys() {
        let mut app = test_app();
        app.handle_key(key('2'));
        let first = app.focused().config.reciter_id.clone();
        app.handle_key(special(KeyCode::Right));
        assert_ne!(first, app.focused().config.reciter_id);
    }

    #[test]
    fn tabs_switch_with_number_keys() {
        let mut app = test_app();
        app.handle_key(key('3'));
        assert_eq!(app.active_tab, Tab::Outputs);
        app.handle_key(key('1'));
        assert_eq!(app.active_tab, Tab::NowPlaying);
    }

    /// Full standalone-player path: browse → search → select → play. Ignored by
    /// default because it opens the real audio device — run with `--ignored`.
    #[test]
    #[ignore = "opens the real audio device and plays audio"]
    fn browse_to_playback_plays_audio() {
        use crate::audio::engine::EngineState;

        let mut app = test_app();
        app.outputs[0].config.volume = 0.12;

        app.handle_key(key('2'));
        app.handle_key(key('/'));
        for c in "ikhlas".chars() {
            app.handle_key(key(c));
        }
        app.handle_key(special(KeyCode::Esc));
        assert_eq!(app.focused().config.from_surah, 112);

        app.handle_key(special(KeyCode::Enter));

        let mut saw_playing = false;
        let deadline = Instant::now() + Duration::from_secs(6);
        while Instant::now() < deadline {
            while let Ok(msg) = app.msg_rx.try_recv() {
                app.handle_message(msg);
            }
            if app.focused().state == EngineState::Playing {
                saw_playing = true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(saw_playing, "playback never reached the Playing state");
        assert!(app.focused().track_total > 0, "no tracks were loaded");
    }

    #[test]
    fn outputs_tab_focus_toggles_and_default_is_undeletable() {
        let mut app = test_app();
        app.handle_key(key('3'));
        assert_eq!(app.active_tab, Tab::Outputs);
        assert_eq!(app.outputs_focus, OutputsFocus::Channels);

        app.handle_key(special(KeyCode::Tab));
        assert_eq!(app.outputs_focus, OutputsFocus::Devices);
        app.handle_key(special(KeyCode::Tab));
        assert_eq!(app.outputs_focus, OutputsFocus::Channels);

        // Output 0 (the default) cannot be deleted.
        app.handle_key(key('d'));
        assert_eq!(app.outputs.len(), 1);
    }

    /// Adds and removes a real output bound to a real device. Ignored by
    /// default because it spawns an engine on the system audio device.
    #[test]
    #[ignore = "spawns engines on real audio devices"]
    fn add_then_delete_output() {
        let mut app = test_app();

        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && app.devices.is_empty() {
            while let Ok(msg) = app.msg_rx.try_recv() {
                app.handle_message(msg);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if app.devices.is_empty() {
            return; // no output devices on this machine — nothing to exercise
        }

        app.handle_key(key('3'));
        app.handle_key(special(KeyCode::Tab)); // focus the device list
        app.handle_key(key('+'));
        assert_eq!(app.outputs.len(), 2);
        assert_eq!(app.focused_output, 1);

        app.handle_key(key('d'));
        assert_eq!(app.outputs.len(), 1);
        assert_eq!(app.focused_output, 0);
    }

    #[test]
    fn create_then_delete_playlist_via_modals() {
        use crate::model::playlist::PlaylistStore;

        let mut app = test_app();
        app.playlists = PlaylistStore::in_memory();
        app.handle_key(key('4'));
        assert_eq!(app.active_tab, Tab::Playlists);
        assert_eq!(app.playlists.playlists.len(), 0);

        // n opens the new-playlist modal; Enter confirms the default name.
        app.handle_key(key('n'));
        assert!(app.modal.is_some());
        app.handle_key(special(KeyCode::Enter));
        assert!(app.modal.is_none());
        assert_eq!(app.playlists.playlists.len(), 1);

        // d opens a confirm modal; y deletes.
        app.handle_key(key('d'));
        assert!(app.modal.is_some());
        app.handle_key(key('y'));
        assert_eq!(app.playlists.playlists.len(), 0);
    }

    #[test]
    fn add_browse_selection_appends_a_playlist_track() {
        use crate::model::playlist::PlaylistStore;

        let mut app = test_app();
        app.playlists = PlaylistStore::in_memory();

        // Create a playlist on the Playlists tab.
        app.handle_key(key('4'));
        app.handle_key(key('n'));
        app.handle_key(special(KeyCode::Enter));
        assert_eq!(app.playlists.playlists.len(), 1);
        assert!(app.playlists.playlists[0].items.is_empty());

        // Pick Al-Mulk in Browse, then A appends it as a track.
        app.handle_key(key('2'));
        app.handle_key(key('/'));
        for c in "mulk".chars() {
            app.handle_key(key(c));
        }
        app.handle_key(special(KeyCode::Esc));
        assert_eq!(app.focused().config.from_surah, 67);

        app.handle_key(key('A'));
        let items = &app.playlists.playlists[0].items;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].from_surah, 67);
        assert_eq!(items[0].to_surah, 67);
    }

    #[test]
    fn renders_every_tab_and_overlay_without_panicking() {
        use crate::model::playlist::PlaylistStore;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = test_app();
        app.playlists = PlaylistStore::in_memory();
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();

        for tab in ['1', '2', '3', '4'] {
            app.handle_key(key(tab));
            terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        }

        // Help overlay, then close it.
        app.handle_key(key('?'));
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        app.handle_key(key('?'));

        // A text-input modal over the Playlists tab.
        app.handle_key(key('4'));
        app.handle_key(key('n'));
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();

        // Below the minimum terminal size → the size guard renders.
        let mut tiny = Terminal::new(TestBackend::new(40, 10)).unwrap();
        tiny.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
    }
}
