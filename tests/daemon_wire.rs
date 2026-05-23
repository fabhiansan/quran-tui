//! Wire-protocol round-trip tests. Verifies that `Buffer` and `KeyEvent` go
//! through `write_msg` → `read_msg` unchanged, that two frames in one stream
//! decode in order, and that the oversize-frame guard fires.

#![cfg(unix)]

use std::io::Cursor;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use quran_tui::daemon::wire::{read_msg, write_msg, ClientMsg, DaemonMsg};

fn sample_buffer() -> Buffer {
    let mut buf = Buffer::empty(Rect::new(0, 0, 6, 2));
    buf.set_string(
        0,
        0,
        "hello!",
        Style::default()
            .fg(Color::Red)
            .bg(Color::Blue)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
    );
    buf.set_string(0, 1, "world.", Style::default().fg(Color::Green));
    buf
}

#[test]
fn buffer_round_trips() {
    let original = DaemonMsg::Frame {
        buffer: sample_buffer(),
        cursor: Some((3, 1)),
    };
    let mut wire: Vec<u8> = Vec::new();
    write_msg(&mut wire, &original).unwrap();

    let mut r = Cursor::new(wire);
    let decoded: DaemonMsg = read_msg(&mut r).unwrap();
    match (original, decoded) {
        (
            DaemonMsg::Frame { buffer: a, cursor: ca },
            DaemonMsg::Frame { buffer: b, cursor: cb },
        ) => {
            assert_eq!(a, b, "Buffer must round-trip losslessly");
            assert_eq!(ca, cb);
        }
        _ => panic!("variant mismatch"),
    }
}

#[test]
fn key_event_round_trips() {
    let original = ClientMsg::Key(KeyEvent::new_with_kind_and_state(
        KeyCode::Char('q'),
        KeyModifiers::CONTROL,
        KeyEventKind::Press,
        KeyEventState::NONE,
    ));
    let mut wire: Vec<u8> = Vec::new();
    write_msg(&mut wire, &original).unwrap();

    let mut r = Cursor::new(wire);
    let decoded: ClientMsg = read_msg(&mut r).unwrap();
    match (original, decoded) {
        (ClientMsg::Key(a), ClientMsg::Key(b)) => assert_eq!(a, b),
        _ => panic!("variant mismatch"),
    }
}

#[test]
fn two_messages_decode_in_order() {
    let mut wire: Vec<u8> = Vec::new();
    write_msg(
        &mut wire,
        &ClientMsg::Hello {
            proto: 1,
            w: 80,
            h: 24,
        },
    )
    .unwrap();
    write_msg(&mut wire, &ClientMsg::Resize { w: 100, h: 30 }).unwrap();

    let mut r = Cursor::new(wire);
    let first: ClientMsg = read_msg(&mut r).unwrap();
    let second: ClientMsg = read_msg(&mut r).unwrap();
    assert!(matches!(
        first,
        ClientMsg::Hello {
            proto: 1,
            w: 80,
            h: 24
        }
    ));
    assert!(matches!(
        second,
        ClientMsg::Resize { w: 100, h: 30 }
    ));
}

#[test]
fn oversize_length_prefix_is_rejected() {
    // u32 length = 9 MiB > MAX_FRAME_BYTES (8 MiB). No body bytes follow; the
    // guard must fire before any read_exact for the body.
    let mut wire: Vec<u8> = Vec::new();
    wire.extend_from_slice(&(9u32 * 1024 * 1024).to_le_bytes());
    let mut r = Cursor::new(wire);
    let err = read_msg::<_, ClientMsg>(&mut r).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}
