//! Daemon mode: a background process holds the audio engines and app state;
//! a thin UI client attaches over a Unix socket and ships keys / receives
//! rendered terminal buffers. Audio survives terminal close.
//!
//! See `/Users/fabhiantomaoludyo/.claude/plans/saya-ingin-buat-daemon-dreamy-sunset.md`.

pub mod client;
pub mod server;
pub mod wire;

use std::path::PathBuf;

/// Wire-protocol version. Bump on any incompatible change to `wire::ClientMsg`
/// or `wire::DaemonMsg`. A client whose `Hello.proto` doesn't match the
/// daemon's `Welcome.proto` aborts with a clear message.
pub const PROTO_VERSION: u32 = 1;

/// Directory holding the socket + lock file. Prefers XDG `$XDG_RUNTIME_DIR`
/// when available (Linux); falls back to the OS cache dir (macOS has no
/// runtime dir); final fallback `$TMPDIR`.
pub fn runtime_dir() -> PathBuf {
    let dirs = directories::ProjectDirs::from("dev", "local", "quran-tui");
    dirs.as_ref()
        .and_then(|d| d.runtime_dir().map(PathBuf::from))
        .or_else(|| dirs.map(|d| d.cache_dir().to_path_buf()))
        .unwrap_or_else(std::env::temp_dir)
}

/// Unix-domain-socket path the daemon binds and the client connects to.
pub fn socket_path() -> PathBuf {
    runtime_dir().join("quran-tui.sock")
}

/// Advisory single-instance lock file (held by the live daemon process via `flock`).
pub fn lock_path() -> PathBuf {
    runtime_dir().join("quran-tui.lock")
}
