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
use crate::event::{AppMessage, DownloadUpdate};
use crate::model::output::{DownloadProgress, OutputChannel, OutputId};
use crate::model::playback_config::PlaybackConfig;
use crate::model::preset::PresetStore;

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
    Presets,
}

impl Tab {
    /// Tabs in display order; index 0 is the landing screen.
    pub const ALL: [Tab; 4] = [Tab::NowPlaying, Tab::Browse, Tab::Outputs, Tab::Presets];

    /// Human-readable tab label shown in the tab strip.
    pub fn title(self) -> &'static str {
        match self {
            Tab::NowPlaying => "Now Playing",
            Tab::Browse => "Browse",
            Tab::Outputs => "Outputs",
            Tab::Presets => "Presets",
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

/// What a confirmed text-input modal should do.
#[derive(Debug, Clone)]
pub enum TextAction {
    SavePreset,
    RenamePreset(String),
}

/// What a confirmed yes/no modal should do.
#[derive(Debug, Clone)]
pub enum ConfirmAction {
    DeletePreset(String),
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

    /// Saved presets and the list cursor on the Presets tab.
    pub presets: PresetStore,
    pub preset_cursor: usize,
    /// Device labels named in a preset whose devices were absent on load.
    pub missing_devices: Vec<String>,
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
            presets: PresetStore::load(),
            preset_cursor: 0,
            missing_devices: Vec::new(),
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
        if self.active_tab == Tab::Presets && self.handle_presets_key(key) {
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
                    TextAction::SavePreset => {
                        self.presets.save_current(name, &self.outputs);
                        self.preset_cursor = 0;
                        self.set_toast(format!("Saved preset \"{name}\""), ToastKind::Info);
                    }
                    TextAction::RenamePreset(id) => {
                        self.presets.rename(&id, name);
                        self.set_toast("Preset renamed", ToastKind::Info);
                    }
                }
            }
            Modal::Confirm { action, .. } => match action {
                ConfirmAction::DeletePreset(id) => {
                    self.presets.delete(&id);
                    self.preset_cursor = self
                        .preset_cursor
                        .min(self.presets.presets.len().saturating_sub(1));
                    self.set_toast("Preset deleted", ToastKind::Info);
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
            (KeyCode::Char('4'), _) => self.active_tab = Tab::Presets,

            (KeyCode::Char(' '), _) => self.toggle_play(),
            (KeyCode::Char('s'), _) if !matches!(self.active_tab, Tab::Browse | Tab::Presets) => {
                self.stop_focused()
            }
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
            Resolution::Missing { .. } => self.begin_download(index, &segments, &reciter, autoplay),
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

    /// Spawn a worker to download the missing files for `index`.
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
        self.outputs[index].download = Some(DownloadProgress {
            done: 0,
            total,
            label: "starting".to_string(),
            autoplay,
        });
        let output_id = self.outputs[index].id;
        downloader::download_missing(missing, self.msg_tx.clone(), output_id);
        self.set_toast(
            format!("Downloading {total} ayah file(s)…"),
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

    // --- Presets ----------------------------------------------------------

    /// Presets-tab key handling. Returns `true` if the key was consumed.
    fn handle_presets_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Up => self.move_preset_cursor(-1),
            KeyCode::Down => self.move_preset_cursor(1),
            KeyCode::Char('s') => self.open_save_modal(),
            KeyCode::Char('d') => self.open_delete_modal(),
            KeyCode::Char('r') => self.open_rename_modal(),
            KeyCode::Enter => self.load_selected_preset(),
            _ => return false,
        }
        true
    }

    fn move_preset_cursor(&mut self, delta: isize) {
        if self.presets.presets.is_empty() {
            return;
        }
        let last = self.presets.presets.len() as isize - 1;
        let next = (self.preset_cursor as isize + delta).clamp(0, last);
        self.preset_cursor = next as usize;
    }

    fn open_save_modal(&mut self) {
        let default_name = format!("Preset {}", self.presets.presets.len() + 1);
        self.modal = Some(Modal::Text {
            title: "Save current setup as preset".to_string(),
            input: default_name,
            action: TextAction::SavePreset,
        });
    }

    fn open_delete_modal(&mut self) {
        let Some(preset) = self.presets.presets.get(self.preset_cursor) else {
            self.set_toast("No preset selected", ToastKind::Warn);
            return;
        };
        self.modal = Some(Modal::Confirm {
            title: "Delete preset".to_string(),
            message: format!("Delete \"{}\"?", preset.name),
            action: ConfirmAction::DeletePreset(preset.id.clone()),
        });
    }

    fn open_rename_modal(&mut self) {
        let Some(preset) = self.presets.presets.get(self.preset_cursor) else {
            self.set_toast("No preset selected", ToastKind::Warn);
            return;
        };
        self.modal = Some(Modal::Text {
            title: "Rename preset".to_string(),
            input: preset.name.clone(),
            action: TextAction::RenamePreset(preset.id.clone()),
        });
    }

    fn load_selected_preset(&mut self) {
        let Some(preset) = self.presets.presets.get(self.preset_cursor).cloned() else {
            self.set_toast("No preset selected", ToastKind::Warn);
            return;
        };
        // Apply each entry to the matching output; loading never auto-plays.
        let mut missing = Vec::new();
        for entry in &preset.entries {
            match self
                .outputs
                .iter_mut()
                .find(|o| o.device_name == entry.device_name)
            {
                Some(output) => {
                    output.config = entry.config.clone();
                    output.send(EngineCommand::SetVolume(entry.config.volume));
                    output.send(EngineCommand::SetLoop(entry.config.loop_enabled));
                }
                None => missing.push(entry.device_label.clone()),
            }
        }
        self.missing_devices = missing;
        if self.missing_devices.is_empty() {
            self.set_toast(format!("Loaded \"{}\"", preset.name), ToastKind::Info);
        } else {
            self.set_toast(
                format!(
                    "Loaded \"{}\" — {} device(s) missing",
                    preset.name,
                    self.missing_devices.len()
                ),
                ToastKind::Warn,
            );
        }
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
                let autoplay = self.outputs[index]
                    .download
                    .take()
                    .map(|p| p.autoplay)
                    .unwrap_or(true);
                self.set_toast("Download complete", ToastKind::Info);
                self.start_playback(index, autoplay);
            }
            DownloadUpdate::Failed(err) => {
                self.outputs[index].download = None;
                self.fallback_after_failed_download(index, &err);
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
fn parse_track_ref(label: &str) -> Option<(u16, u16)> {
    let (surah, ayah) = label.split_once(':')?;
    Some((surah.parse().ok()?, ayah.parse().ok()?))
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
    fn save_then_delete_preset_via_modals() {
        use crate::model::preset::PresetStore;

        let mut app = test_app();
        app.presets = PresetStore::in_memory();
        app.handle_key(key('4'));
        assert_eq!(app.active_tab, Tab::Presets);
        assert_eq!(app.presets.presets.len(), 0);

        // s opens the save modal; Enter confirms the default name.
        app.handle_key(key('s'));
        assert!(app.modal.is_some());
        app.handle_key(special(KeyCode::Enter));
        assert!(app.modal.is_none());
        assert_eq!(app.presets.presets.len(), 1);

        // d opens a confirm modal; y deletes.
        app.handle_key(key('d'));
        assert!(app.modal.is_some());
        app.handle_key(key('y'));
        assert_eq!(app.presets.presets.len(), 0);
    }

    #[test]
    fn loading_a_preset_restores_config_and_does_not_play() {
        use crate::model::preset::PresetStore;

        let mut app = test_app();
        app.presets = PresetStore::in_memory();

        app.outputs[0].config.from_surah = 36;
        app.outputs[0].config.to_surah = 36;
        app.presets.save_current("Ya-Sin", &app.outputs);

        // Change the config, then load the preset back.
        app.outputs[0].config.from_surah = 1;
        app.handle_key(key('4'));
        app.handle_key(special(KeyCode::Enter));

        assert_eq!(app.outputs[0].config.from_surah, 36);
        assert!(!app.outputs[0].is_playing());
        assert!(app.missing_devices.is_empty());
    }

    #[test]
    fn renders_every_tab_and_overlay_without_panicking() {
        use crate::model::preset::PresetStore;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut app = test_app();
        app.presets = PresetStore::in_memory();
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();

        for tab in ['1', '2', '3', '4'] {
            app.handle_key(key(tab));
            terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        }

        // Help overlay, then close it.
        app.handle_key(key('?'));
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        app.handle_key(key('?'));

        // A text-input modal over the Presets tab.
        app.handle_key(key('4'));
        app.handle_key(key('s'));
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();

        // Below the minimum terminal size → the size guard renders.
        let mut tiny = Terminal::new(TestBackend::new(40, 10)).unwrap();
        tiny.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
    }
}
