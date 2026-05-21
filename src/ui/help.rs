//! Help overlay — a centered modal listing every keybinding (§7.6).
//!
//! This is the single source of truth for the key map; keep it in sync with
//! `App::handle_key`.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use super::{theme, widgets};

/// Draw the help overlay over the dimmed current tab.
pub fn draw(frame: &mut Frame, area: Rect) {
    let popup = widgets::centered_rect(area, 82, 82);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::ACCENT))
        .title(" Help — press any key to close ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let columns =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).split(inner);
    frame.render_widget(Paragraph::new(left_column()), columns[0]);
    frame.render_widget(Paragraph::new(right_column()), columns[1]);
}

fn heading(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!("  {text}"),
        Style::default()
            .fg(theme::ACCENT)
            .add_modifier(Modifier::BOLD),
    ))
}

fn binding(keys: &str, action: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("   {keys:<12}"), Style::default().fg(theme::OK)),
        Span::raw(action.to_string()),
    ])
}

fn left_column() -> Vec<Line<'static>> {
    vec![
        heading("Global"),
        binding("1-4", "Jump to tab"),
        binding("Tab S-Tab", "Cycle tabs / fields"),
        binding("?", "Toggle this help"),
        binding("q  Ctrl+C", "Quit"),
        Line::from(""),
        heading("Transport"),
        binding("Space", "Play / pause"),
        binding("s", "Stop"),
        binding("n  p", "Next / prev track"),
        binding("l", "Loop on / off"),
        binding("+  -", "Volume ±5%"),
        Line::from(""),
        heading("Now Playing"),
        binding("↑  ↓", "Focus output"),
        binding("A  P  S", "Play/pause/stop all"),
    ]
}

fn right_column() -> Vec<Line<'static>> {
    vec![
        heading("Browse"),
        binding("↑  ↓", "Move surah list"),
        binding("←  →", "Change reciter"),
        binding("/", "Search surahs"),
        binding("Tab", "Cycle range fields"),
        binding("0-9", "Edit focused field"),
        binding("Enter", "Play on focused output"),
        binding("a", "Queue without playing"),
        Line::from(""),
        heading("Outputs"),
        binding("Tab", "Switch channel/device"),
        binding("+  d", "Add / delete output"),
        binding("Enter", "Bind device"),
        binding("r", "Refresh devices"),
        Line::from(""),
        heading("Presets"),
        binding("Enter", "Load preset"),
        binding("s  d  r", "Save / delete / rename"),
    ]
}
