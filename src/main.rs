//! quran-tui — a terminal UI Quran audio player.
//!
//! Entry point: parse args, set up file logging, install the terminal guard and
//! panic hook, then run the ratatui event loop.

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
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let _log_guard = init_tracing(&cli.log_level)?;
    install_panic_hook();
    tracing::info!(
        "quran-tui starting (audio_dir override: {:?})",
        cli.audio_dir
    );

    // The guard restores the terminal on drop, including on early return / `?`.
    let _terminal_guard = TerminalGuard::new()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(cli.audio_dir);
    let result = run(&mut terminal, &mut app);
    app.persist_config();

    tracing::info!("quran-tui exiting");
    result
}

/// The main render / input loop (§5.2 of the plan).
fn run(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> Result<()> {
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
        // Drain everything the background threads sent since the last frame.
        while let Ok(msg) = app.msg_rx.try_recv() {
            app.handle_message(msg);
        }
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

/// Set up `tracing` to write to a file under the OS cache dir. Never log to
/// stdout — stdout is the TUI.
fn init_tracing(level: &str) -> Result<WorkerGuard> {
    use tracing_subscriber::EnvFilter;

    let cache_dir = directories::ProjectDirs::from("dev", "local", "quran-tui")
        .map(|d| d.cache_dir().to_path_buf())
        .unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&cache_dir)?;

    let file_appender = tracing_appender::rolling::never(&cache_dir, "quran-tui.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let filter = EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_env_filter(filter)
        .init();

    Ok(guard)
}
