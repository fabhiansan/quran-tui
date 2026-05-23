//! quran-tui — a terminal UI Quran audio player.
//!
//! Entry point: parse args, set up file logging, install the terminal guard and
//! panic hook, then run the ratatui event loop. With `--daemon`/`--stop` it
//! instead drives the daemon mode protocol (see `crate::daemon`).

use std::io::{self, Stdout};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use crossterm::event::{self, Event};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tracing_appender::non_blocking::WorkerGuard;

use quran_tui::app::App;
use quran_tui::media_keys::{self, MediaKeys};
use quran_tui::ui;

/// Terminal UI Quran audio player.
#[derive(Parser, Debug)]
#[command(name = "quran-tui", version, about)]
struct Cli {
    /// Directory holding local audio and where downloads land.
    #[arg(long, value_name = "DIR")]
    audio_dir: Option<PathBuf>,

    /// Log verbosity: error, warn, info, debug, trace.
    #[arg(long, default_value = "info")]
    log_level: String,

    /// Run with a background daemon: spawn one if needed, then attach a UI client.
    /// Audio keeps playing after the terminal closes.
    #[arg(long, conflicts_with_all = ["stop", "serve"])]
    daemon: bool,

    /// Shut down a running quran-tui daemon.
    #[arg(long, conflicts_with_all = ["daemon", "serve"])]
    stop: bool,

    /// Internal: run as the detached daemon process. Not for direct use.
    #[arg(long = "__serve", hide = true)]
    serve: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    #[cfg(unix)]
    {
        if cli.serve {
            let _log_guard = init_tracing_for("quran-tui-daemon.log", &cli.log_level)?;
            tracing::info!("quran-tui daemon starting");
            return quran_tui::daemon::server::serve(cli.audio_dir);
        }
        if cli.stop {
            // No raw-mode terminal here; log to a separate file so the running
            // daemon's log isn't fought over.
            let _log_guard = init_tracing_for("quran-tui-client.log", &cli.log_level)?;
            return quran_tui::daemon::client::stop();
        }
        if cli.daemon {
            let _log_guard = init_tracing_for("quran-tui-client.log", &cli.log_level)?;
            install_panic_hook();
            tracing::info!(
                "quran-tui client starting (audio_dir override: {:?})",
                cli.audio_dir
            );
            return quran_tui::daemon::client::connect_or_spawn(cli.audio_dir, &cli.log_level);
        }
    }
    #[cfg(not(unix))]
    if cli.daemon || cli.stop || cli.serve {
        anyhow::bail!("daemon mode is only supported on Unix platforms");
    }

    let _log_guard = init_tracing_for("quran-tui.log", &cli.log_level)?;
    install_panic_hook();
    tracing::info!(
        "quran-tui starting (audio_dir override: {:?})",
        cli.audio_dir
    );
    run_foreground(cli.audio_dir)
}

/// The original single-process flow: own the terminal, run the App loop.
fn run_foreground(audio_dir: Option<PathBuf>) -> Result<()> {
    // The guard restores the terminal on drop, including on early return / `?`.
    let _terminal_guard = TerminalGuard::new()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(audio_dir);
    let mut media = MediaKeys::init(app.msg_tx());
    let result = run(&mut terminal, &mut app, &mut media);
    app.persist_config();

    tracing::info!("quran-tui exiting");
    result
}

/// The main render / input loop (§5.2 of the plan).
fn run(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
    media: &mut MediaKeys,
) -> Result<()> {
    // `poll(tick)` doubles as the redraw clock: ~8 frames/s even with no input.
    let tick = Duration::from_millis(120);

    while !app.should_quit {
        app.tick();
        terminal.draw(|frame| ui::draw(frame, app))?;

        // Input: blocks at most `tick`.
        if event::poll(tick)? {
            match event::read()? {
                Event::Key(key) => app.handle_key(key),
                Event::Resize(_, _) => { /* ratatui reflows on the next draw */ }
                _ => {}
            }
        }
        // Pump the macOS run loop so MPRemoteCommandCenter blocks can deliver
        // queued F7/F8/F9 events into our channel (no-op on other platforms).
        media_keys::pump();
        // Drain everything the background threads sent since the last frame.
        while let Ok(msg) = app.msg_rx.try_recv() {
            app.handle_message(msg);
        }
        media.update(&app.now_playing_snapshot());
    }
    Ok(())
}

/// RAII guard: enters raw mode + the alternate screen on construction, and
/// reverses both on drop so the user's terminal is always restored.
struct TerminalGuard;

impl TerminalGuard {
    fn new() -> Result<Self> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

/// Restore the terminal before the default panic hook prints, so a panic never
/// leaves the user staring at a wrecked raw-mode terminal.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        default_hook(info);
    }));
}

/// Set up `tracing` to write to the given file name under the OS cache dir.
/// Never log to stdout — stdout is the TUI (or in daemon mode, /dev/null).
fn init_tracing_for(filename: &str, level: &str) -> Result<WorkerGuard> {
    use tracing_subscriber::EnvFilter;

    let cache_dir = directories::ProjectDirs::from("dev", "local", "quran-tui")
        .map(|d| d.cache_dir().to_path_buf())
        .unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&cache_dir)?;

    let file_appender = tracing_appender::rolling::never(&cache_dir, filename);
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let filter = EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_env_filter(filter)
        .init();

    Ok(guard)
}
