//! Color palette (§7.9). Minimal — works on light and dark terminals. No
//! hard-coded backgrounds; the selected list row uses `Modifier::REVERSED`.

// Some colors are only referenced from later-phase screens.
#![allow(dead_code)]

use ratatui::style::Color;

/// Active tab, focus borders.
pub const ACCENT: Color = Color::Cyan;
/// "playing" / "local available".
pub const OK: Color = Color::Green;
/// "downloading" / "to download" / warnings.
pub const WARNING: Color = Color::Yellow;
/// Errors.
pub const ERROR: Color = Color::Red;
/// Secondary text.
pub const DIM: Color = Color::DarkGray;
