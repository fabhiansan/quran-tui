//! The UI client: connect to (or spawn) the daemon, forward keys, blit
//! incoming frames to the real terminal.

use std::io::{self, Stdout};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossbeam_channel::{unbounded, TryRecvError};
use crossterm::event::{self, Event};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::buffer::{Buffer, Cell};

use crate::daemon::wire::{read_msg, write_msg, ClientMsg, DaemonMsg};
use crate::daemon::{socket_path, PROTO_VERSION};

const SPAWN_BUDGET: Duration = Duration::from_secs(5);
const SPAWN_POLL: Duration = Duration::from_millis(100);

/// `quran-tui --daemon`: connect to an existing daemon, or spawn a detached
/// one and then connect.
pub fn connect_or_spawn(audio_dir: Option<PathBuf>, log_level: &str) -> Result<()> {
    let sock = socket_path();
    if let Ok(stream) = UnixStream::connect(&sock) {
        return run_client(stream);
    }

    spawn_detached_daemon(audio_dir.as_deref(), log_level).context("spawn detached daemon")?;
    tracing::info!("spawned daemon; awaiting socket");

    let deadline = Instant::now() + SPAWN_BUDGET;
    loop {
        match UnixStream::connect(&sock) {
            Ok(stream) => return run_client(stream),
            Err(_) if Instant::now() < deadline => thread::sleep(SPAWN_POLL),
            Err(e) => {
                anyhow::bail!("spawned daemon did not become reachable: {e}");
            }
        }
    }
}

/// `quran-tui --stop`: tell the daemon to shut down. Idempotent: no daemon → exit 0.
pub fn stop() -> Result<()> {
    let sock = socket_path();
    let mut stream = match UnixStream::connect(&sock) {
        Ok(s) => s,
        Err(_) => {
            eprintln!("no daemon running");
            return Ok(());
        }
    };
    let (w, h) = crossterm::terminal::size().unwrap_or((80, 24));
    write_msg(
        &mut stream,
        &ClientMsg::Hello {
            proto: PROTO_VERSION,
            w,
            h,
        },
    )?;
    // Consume Welcome (best effort).
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
    let _ = read_msg::<_, DaemonMsg>(&mut stream);

    write_msg(&mut stream, &ClientMsg::Shutdown)?;

    // Wait briefly for Bye / EOF as confirmation.
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let _ = read_msg::<_, DaemonMsg>(&mut stream);
    eprintln!("daemon stopped");
    Ok(())
}

/// Drive an attached UI session over the given socket. Restores the terminal
/// on any exit path (clean detach, daemon Bye, daemon crash, panic).
fn run_client(mut stream: UnixStream) -> Result<()> {
    // Handshake before entering raw mode so handshake errors print plainly.
    let (w, h) = crossterm::terminal::size().context("get terminal size")?;
    write_msg(
        &mut stream,
        &ClientMsg::Hello {
            proto: PROTO_VERSION,
            w,
            h,
        },
    )
    .context("send Hello")?;

    match read_msg::<_, DaemonMsg>(&mut stream).context("read Welcome")? {
        DaemonMsg::Welcome { proto } if proto == PROTO_VERSION => {}
        DaemonMsg::Welcome { proto } => {
            anyhow::bail!(
                "daemon protocol {proto} ≠ client protocol {PROTO_VERSION}; \
                 run `quran-tui --stop` and retry"
            );
        }
        _ => anyhow::bail!("daemon did not send Welcome"),
    }

    let _guard = ClientTerminalGuard::new()?;
    install_panic_hook();

    let write_half = stream.try_clone().context("clone stream")?;
    let mut read_half = stream;
    let (frame_tx, frame_rx) = unbounded::<DaemonMsg>();
    thread::Builder::new()
        .name("client-reader".into())
        .spawn(move || {
            while let Ok(msg) = read_msg::<_, DaemonMsg>(&mut read_half) {
                if frame_tx.send(msg).is_err() {
                    break;
                }
            }
        })?;

    let mut tx = write_half;
    let mut backend = CrosstermBackend::new(io::stdout());
    backend.hide_cursor().ok();
    let mut prev: Option<Buffer> = None;

    loop {
        // Forward local input. A failed write ⇒ daemon gone ⇒ clean exit.
        if event::poll(Duration::from_millis(50))? {
            let forwarded = match event::read()? {
                Event::Key(k) => write_msg(&mut tx, &ClientMsg::Key(k)),
                Event::Resize(w, h) => write_msg(&mut tx, &ClientMsg::Resize { w, h }),
                _ => Ok(()),
            };
            if forwarded.is_err() {
                return Ok(());
            }
        }

        // Drain frames non-blocking. Apply only the latest in a burst.
        let mut latest: Option<Buffer> = None;
        let mut done = false;
        loop {
            match frame_rx.try_recv() {
                Ok(DaemonMsg::Frame { buffer, .. }) => latest = Some(buffer),
                Ok(DaemonMsg::Bye) => {
                    done = true;
                    break;
                }
                Ok(DaemonMsg::Welcome { .. }) => {}
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    done = true;
                    break;
                }
            }
        }
        if let Some(buf) = latest {
            blit(&mut backend, prev.as_ref(), &buf)?;
            prev = Some(buf);
        }
        if done {
            return Ok(());
        }
    }
}

/// Write only the cells that changed between `prev` and `next` to the real
/// terminal — the same trick `ratatui::Terminal::flush` plays between its
/// double buffers.
fn blit(
    backend: &mut CrosstermBackend<Stdout>,
    prev: Option<&Buffer>,
    next: &Buffer,
) -> Result<()> {
    let same_area = prev.is_some_and(|p| p.area == next.area);
    if !same_area {
        backend.clear()?;
    }

    let area = next.area;
    let updates: Vec<(u16, u16, &Cell)> = next
        .content
        .iter()
        .enumerate()
        .filter_map(|(i, cell)| {
            let changed = match (same_area, prev) {
                (true, Some(p)) => &p.content[i] != cell,
                _ => true,
            };
            if !changed {
                return None;
            }
            let x = area.x + (i as u16) % area.width;
            let y = area.y + (i as u16) / area.width;
            Some((x, y, cell))
        })
        .collect();

    backend.draw(updates.into_iter())?;
    Backend::flush(backend)?;
    Ok(())
}

fn spawn_detached_daemon(audio_dir: Option<&std::path::Path>, log_level: &str) -> Result<()> {
    let exe = std::env::current_exe().context("locate own executable")?;
    let mut cmd = Command::new(exe);
    cmd.arg("--__serve").arg("--log-level").arg(log_level);
    if let Some(dir) = audio_dir {
        cmd.arg("--audio-dir").arg(dir);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    // SAFETY: `pre_exec` runs after fork in the child, before exec. setsid is
    // async-signal-safe and we touch no other heap/global state here.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    // Spawn but DO NOT wait — the daemon outlives this client.
    let _child = cmd.spawn().context("spawn daemon process")?;
    Ok(())
}

/// Local copy of the foreground `TerminalGuard` so client.rs is self-contained.
/// Kept verbatim from `main.rs`.
struct ClientTerminalGuard;

impl ClientTerminalGuard {
    fn new() -> Result<Self> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;
        Ok(Self)
    }
}

impl Drop for ClientTerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        default_hook(info);
    }));
}
