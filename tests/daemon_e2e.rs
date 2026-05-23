//! End-to-end smoke test of daemon mode. Spawns the binary as a daemon, drives
//! it with a synthetic client over the real socket, verifies it produces
//! non-trivial UI frames, then sends Shutdown. Ignored by default — opens the
//! real audio device.

#![cfg(unix)]

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use quran_tui::daemon::wire::{read_msg, write_msg, ClientMsg, DaemonMsg};
use quran_tui::daemon::{socket_path, PROTO_VERSION};

#[test]
#[ignore = "opens real audio device; run with `cargo test --test daemon_e2e -- --ignored --nocapture`"]
fn daemon_produces_frames() {
    // Stop anything running and clear stale state.
    let _ = Command::new(env!("CARGO_BIN_EXE_quran-tui")).arg("--stop").status();
    let _ = std::fs::remove_file(socket_path());
    thread::sleep(Duration::from_millis(200));

    // Start a daemon via the real CLI (so we exercise the spawn path indirectly).
    let mut daemon = Command::new(env!("CARGO_BIN_EXE_quran-tui"))
        .arg("--__serve")
        .arg("--log-level")
        .arg("debug")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn daemon");

    // Wait for socket.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut stream = loop {
        match UnixStream::connect(socket_path()) {
            Ok(s) => break s,
            Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(100)),
            Err(e) => panic!("daemon never became reachable: {e}"),
        }
    };
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    eprintln!("connected to daemon");

    // Handshake.
    write_msg(
        &mut stream,
        &ClientMsg::Hello {
            proto: PROTO_VERSION,
            w: 100,
            h: 30,
        },
    )
    .unwrap();
    eprintln!("sent Hello");

    let welcome: DaemonMsg = read_msg(&mut stream).expect("read Welcome");
    match welcome {
        DaemonMsg::Welcome { proto } => assert_eq!(proto, PROTO_VERSION),
        other => panic!("expected Welcome, got {other:?}"),
    }
    eprintln!("got Welcome");

    // Read a few frames; the first one should have a 100x30 buffer with > 0 cells
    // and at least one non-default cell (the UI actually rendered something).
    let mut got_real_frame = false;
    let frame_deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < frame_deadline {
        match read_msg::<_, DaemonMsg>(&mut stream) {
            Ok(DaemonMsg::Frame { buffer, .. }) => {
                eprintln!(
                    "got frame: {}x{}, {} cells",
                    buffer.area.width,
                    buffer.area.height,
                    buffer.content.len()
                );
                assert_eq!(buffer.area.width, 100, "buffer width should match Hello");
                assert_eq!(buffer.area.height, 30, "buffer height should match Hello");
                let non_default = buffer
                    .content
                    .iter()
                    .filter(|c| c.symbol() != " " || c.fg != ratatui::style::Color::Reset)
                    .count();
                eprintln!("  non-default cells: {non_default}");
                if non_default > 0 {
                    got_real_frame = true;
                    break;
                }
            }
            Ok(other) => eprintln!("ignoring {other:?}"),
            Err(e) => panic!("frame read failed: {e}"),
        }
    }
    assert!(got_real_frame, "daemon never sent a frame with visible content");

    // Cleanup.
    write_msg(&mut stream, &ClientMsg::Shutdown).unwrap();
    // Best-effort read of Bye / EOF.
    let mut sink = [0u8; 256];
    let _ = stream.read(&mut sink);
    let _ = stream.flush();
    eprintln!("sent Shutdown");
    // Reap the daemon so we don't leave a zombie / trigger clippy.
    let _ = daemon.wait();
}
