//! Shared UI helpers — bars, formatting, status chips, layout rects.

use std::time::Duration;

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Color;

use super::theme;
use crate::audio::engine::EngineState;

/// Render a fractional bar `width` cells wide, e.g. `████████░░`.
pub fn bar(ratio: f32, width: usize) -> String {
    let filled = (ratio.clamp(0.0, 1.0) * width as f32).round() as usize;
    let filled = filled.min(width);
    let mut s = String::with_capacity(width);
    for i in 0..width {
        s.push(if i < filled { '█' } else { '░' });
    }
    s
}

/// Format a duration as `MM:SS`.
pub fn fmt_mmss(d: Duration) -> String {
    let secs = d.as_secs();
    format!("{:02}:{:02}", secs / 60, secs % 60)
}

/// Truncate `text` to `max` characters, appending an ellipsis if shortened.
pub fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let kept: String = text.chars().take(max.saturating_sub(1)).collect();
        format!("{kept}…")
    }
}

/// Icon, label, and color for an engine state.
pub fn engine_state_chip(state: &EngineState) -> (&'static str, String, Color) {
    match state {
        EngineState::Idle => ("○", "Idle".to_string(), theme::DIM),
        EngineState::Loading => ("…", "Loading".to_string(), theme::WARNING),
        EngineState::Playing => ("▶", "Playing".to_string(), theme::OK),
        EngineState::Paused => ("❚❚", "Paused".to_string(), theme::WARNING),
        EngineState::Error(err) => ("✖", format!("Error: {err}"), theme::ERROR),
    }
}

/// A `Rect` `percent_x` × `percent_y` of `area`, centered.
pub fn centered_rect(area: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let vertical = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(area);
    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(vertical[1])[1]
}

/// A `height`-row `Rect` vertically centered within `area`.
pub fn vertically_centered(area: Rect, height: u16) -> Rect {
    if height >= area.height {
        return area;
    }
    let pad = (area.height - height) / 2;
    Rect {
        x: area.x,
        y: area.y + pad,
        width: area.width,
        height,
    }
}
