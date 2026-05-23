//! The daemon serve loop. Mirrors `main::run` (foreground) but renders to an
//! in-memory `Buffer` and ships frames to the attached client over a Unix
//! socket. Audio engines spawned by `App::new` survive across client detach.

use std::fs::{File, OpenOptions};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossbeam_channel::{unbounded, Receiver, TryRecvError};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::Terminal;

use crate::app::App;
use crate::daemon::wire::{read_msg, write_msg, ClientMsg, DaemonMsg};
use crate::daemon::{lock_path, socket_path, PROTO_VERSION};
use crate::ui;

const TICK: Duration = Duration::from_millis(120);

pub fn serve(audio_dir: Option<PathBuf>) -> Result<()> {
    let sock = socket_path();
    let lock = lock_path();
    if let Some(parent) = sock.parent() {
        std::fs::create_dir_all(parent).context("create runtime dir")?;
    }

    // 1. Advisory lock. If another daemon already holds it, exit silently —
    //    the client will retry-connect to that other daemon.
    let _lock_guard = match acquire_lock(&lock) {
        Ok(f) => f,
        Err(_) => {
            tracing::info!("another daemon already holds the lock; exiting");
            return Ok(());
        }
    };

    // 2. Clean up a stale socket from a previously crashed daemon.
    cleanup_stale_socket(&sock);

    // 3. Bind.
    let listener = UnixListener::bind(&sock).context("bind socket")?;
    let _serve_guard = ServeGuard { sock: sock.clone() };
    tracing::info!("daemon listening on {}", sock.display());

    // 4. Headless app + in-memory terminal.
    let mut app = App::new(audio_dir);
    let mut terminal = Terminal::new(TestBackend::new(80, 24))?;

    // 5. Accept thread → channel.
    let (conn_tx, conn_rx) = unbounded::<UnixStream>();
    thread::Builder::new()
        .name("daemon-accept".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                if conn_tx.send(stream).is_err() {
                    break;
                }
            }
        })
        .context("spawn accept thread")?;

    // 6. Main loop.
    let mut active: Option<ActiveClient> = None;
    let mut shutdown = false;

    while !shutdown {
        let loop_start = Instant::now();
        app.tick();

        // Pick up new connections (last-attach-wins: replaces any existing client).
        while let Ok(stream) = conn_rx.try_recv() {
            match attach_new_client(stream, &mut terminal) {
                Ok(c) => {
                    if active.is_some() {
                        tracing::info!("replacing previous client");
                    }
                    active = Some(c);
                }
                Err(e) => tracing::warn!("rejected client: {e}"),
            }
        }

        // Drain client messages.
        let mut detach = false;
        if let Some(c) = active.as_mut() {
            loop {
                match c.key_rx.try_recv() {
                    Ok(ClientMsg::Key(k)) => app.handle_key(k),
                    Ok(ClientMsg::Resize { w, h }) => {
                        resize_terminal(&mut terminal, w, h);
                        c.last_sent = None;
                    }
                    Ok(ClientMsg::Shutdown) => {
                        shutdown = true;
                        break;
                    }
                    Ok(ClientMsg::Hello { .. }) => { /* late hello, ignore */ }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        detach = true;
                        break;
                    }
                }
            }
        }
        if detach {
            active = None;
        }
        if shutdown {
            break;
        }

        // `q` / Ctrl+C ⇒ user wants the UI gone, but the daemon stays.
        if app.should_quit {
            app.should_quit = false;
            active = None;
        }

        // Render. `Terminal::draw` swaps its double-buffer after the callback,
        // so we must read the just-rendered frame from the returned
        // `CompletedFrame` — NOT from `current_buffer_mut()`, which after the
        // swap is the reset back-buffer for the *next* frame.
        let rendered: Buffer = terminal
            .draw(|f| ui::draw(f, &mut app))?
            .buffer
            .clone();

        // Ship frame only on change.
        let mut detach_after_send = false;
        if let Some(c) = active.as_mut() {
            if c.last_sent.as_ref() != Some(&rendered) {
                let frame = DaemonMsg::Frame {
                    buffer: rendered.clone(),
                    cursor: None,
                };
                match write_msg(&mut c.write, &frame) {
                    Ok(()) => c.last_sent = Some(rendered),
                    Err(e) => {
                        tracing::debug!("client write failed: {e}");
                        detach_after_send = true;
                    }
                }
            }
        }
        if detach_after_send {
            active = None;
        }

        // Drain engine messages.
        while let Ok(m) = app.msg_rx.try_recv() {
            app.handle_message(m);
        }

        // Pace the loop. No event::poll here — sleep the remainder.
        let elapsed = loop_start.elapsed();
        if elapsed < TICK {
            thread::sleep(TICK - elapsed);
        }
    }

    // Clean shutdown after `--stop`. Best-effort goodbye, then drop App which
    // sends EngineCommand::Shutdown to every engine (stopping audio).
    if let Some(c) = active.as_mut() {
        let _ = write_msg(&mut c.write, &DaemonMsg::Bye);
    }
    app.persist_config();
    tracing::info!("daemon exiting");
    Ok(())
}

struct ActiveClient {
    write: UnixStream,
    key_rx: Receiver<ClientMsg>,
    last_sent: Option<Buffer>,
}

impl Drop for ActiveClient {
    fn drop(&mut self) {
        // shutdown(Both) on this fd closes the underlying socket; the reader
        // thread (holding a cloned fd) then sees EOF and exits on its own.
        let _ = self.write.shutdown(std::net::Shutdown::Both);
    }
}

fn attach_new_client(
    mut stream: UnixStream,
    terminal: &mut Terminal<TestBackend>,
) -> Result<ActiveClient> {
    // Bound the handshake so a stuck peer can't hang the daemon.
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let hello: ClientMsg = read_msg(&mut stream).context("read Hello")?;
    let (w, h) = match hello {
        ClientMsg::Hello { proto, w, h } => {
            if proto != PROTO_VERSION {
                let _ = write_msg(&mut stream, &DaemonMsg::Bye);
                anyhow::bail!("client proto {proto} ≠ daemon proto {PROTO_VERSION}");
            }
            (w, h)
        }
        _ => anyhow::bail!("first message was not Hello"),
    };
    stream.set_read_timeout(None)?;

    resize_terminal(terminal, w, h);
    write_msg(&mut stream, &DaemonMsg::Welcome { proto: PROTO_VERSION })?;

    let write_half = stream.try_clone().context("clone stream")?;
    let mut read_half = stream;
    let (key_tx, key_rx) = unbounded::<ClientMsg>();
    thread::Builder::new()
        .name("daemon-reader".into())
        .spawn(move || {
            // EOF / io error ⇒ client gone.
            while let Ok(msg) = read_msg::<_, ClientMsg>(&mut read_half) {
                if key_tx.send(msg).is_err() {
                    break;
                }
            }
        })?;

    Ok(ActiveClient {
        write: write_half,
        key_rx,
        last_sent: None,
    })
}

fn resize_terminal(terminal: &mut Terminal<TestBackend>, w: u16, h: u16) {
    let w = w.max(1);
    let h = h.max(1);
    terminal.backend_mut().resize(w, h);
    let _ = terminal.resize(Rect::new(0, 0, w, h));
}

fn acquire_lock(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .context("open lock file")?;
    // SAFETY: flock takes a raw fd; we hold `file` for the whole process so
    // the fd remains valid until exit, when the kernel releases the lock.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        anyhow::bail!("another quran-tui daemon is already running");
    }
    Ok(file)
}

fn cleanup_stale_socket(sock: &Path) {
    if !sock.exists() {
        return;
    }
    if UnixStream::connect(sock).is_ok() {
        return; // live daemon owns it
    }
    let _ = std::fs::remove_file(sock);
}

struct ServeGuard {
    sock: PathBuf,
}

impl Drop for ServeGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.sock);
        // The lock file is left on disk; flock releases automatically when the
        // process exits. Future daemons reuse the same file.
    }
}
