//! Wire protocol between the daemon and the UI client.
//!
//! Framing: a 4-byte little-endian length prefix followed by that many bytes
//! of JSON. We reuse `serde_json` (already a dependency) and rely on ratatui
//! 0.29's `serde` feature for `Buffer`/`Cell` serialization, and crossterm
//! 0.28's `serde` feature for `KeyEvent` serialization. No mirror types needed.

use std::io::{self, Read, Write};

use crossterm::event::KeyEvent;
use ratatui::buffer::Buffer;
use serde::{Deserialize, Serialize};

/// Anything above this is rejected as garbage. 8 MiB is far above a maxed-out
/// terminal's serialized `Buffer` (worst case ~200x60 = 12k cells, ≲ 2 MiB).
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// Client → Daemon.
#[derive(Serialize, Deserialize, Debug)]
pub enum ClientMsg {
    /// First message after connect. Carries proto version + terminal size so
    /// the daemon can sanity-check + render at the right dimensions.
    Hello { proto: u32, w: u16, h: u16 },
    /// Forwarded key from the client's local crossterm event loop.
    Key(KeyEvent),
    /// Client's terminal was resized.
    Resize { w: u16, h: u16 },
    /// Ask the daemon to stop the audio and exit. Sent by `quran-tui --stop`.
    Shutdown,
}

/// Daemon → Client.
#[derive(Serialize, Deserialize, Debug)]
pub enum DaemonMsg {
    /// Reply to `Hello`. Mismatched `proto` ⇒ client aborts with a clear msg.
    Welcome { proto: u32 },
    /// A rendered frame. The client diffs against its previous buffer and
    /// writes only changed cells to its real terminal.
    Frame {
        buffer: Buffer,
        /// Currently always `None` (no `set_cursor_position` calls in `ui/`).
        /// Reserved for future text-input modals that show a real cursor.
        cursor: Option<(u16, u16)>,
    },
    /// Daemon is exiting cleanly (after `Shutdown`). Client breaks its loop.
    Bye,
}

/// Write one length-prefixed JSON frame.
pub fn write_msg<W: Write, T: Serialize>(w: &mut W, msg: &T) -> io::Result<()> {
    let body =
        serde_json::to_vec(msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if body.len() > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    w.write_all(&(body.len() as u32).to_le_bytes())?;
    w.write_all(&body)?;
    w.flush()
}

/// Read one length-prefixed JSON frame. Returns `UnexpectedEof` on peer close.
pub fn read_msg<R: Read, T: serde::de::DeserializeOwned>(r: &mut R) -> io::Result<T> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    serde_json::from_slice(&body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}
