//! Playlists tab — build and play ordered queues of Quran selections (§7.5).
//!
//! Two panes: the playlist list on the left, the selected playlist's tracks on
//! the right. `Tab` switches focus; the focused pane gets the accent border.

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use super::{theme, widgets};
use crate::app::{App, PlaylistPane};
use crate::model::playlist::PlaylistItem;

/// Draw the Playlists content region.
pub fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let rows = Layout::vertical([
        Constraint::Min(3),    // two-pane body
        Constraint::Length(1), // footer
    ])
    .split(area);

    let panes =
        Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)]).split(rows[0]);
    draw_playlists_pane(frame, panes[0], app);
    draw_items_pane(frame, panes[1], app);
    draw_footer(frame, rows[1], app);
}

/// Left pane: every saved playlist with its track count. The outer tab frame
/// already labels this as "Playlists", so the pane itself is borderless-title.
fn draw_playlists_pane(frame: &mut Frame, area: Rect, app: &App) {
    let focused = app.playlist_pane == PlaylistPane::Playlists;
    let border = if focused { theme::ACCENT } else { theme::DIM };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.playlists.playlists.is_empty() {
        let hint = Paragraph::new(Span::styled(
            "No playlists yet — press  n  to create one.",
            Style::default().fg(theme::DIM),
        ))
        .alignment(Alignment::Center);
        frame.render_widget(hint, widgets::vertically_centered(inner, 1));
        return;
    }

    let lines: Vec<Line> = app
        .playlists
        .playlists
        .iter()
        .enumerate()
        .map(|(i, playlist)| {
            let selected = i == app.playlist_cursor;
            let marker = if selected { "▸ " } else { "  " };
            let count = playlist.items.len();
            let mut line = Line::from(vec![
                Span::raw(format!(
                    "{marker}{:<20}",
                    widgets::truncate(&playlist.name, 20)
                )),
                Span::styled(
                    format!("{} track{}", count, if count == 1 { "" } else { "s" }),
                    Style::default().fg(theme::DIM),
                ),
            ]);
            if selected && focused {
                line = line.style(Style::default().add_modifier(Modifier::REVERSED));
            }
            line
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Right pane: the tracks inside the selected playlist.
fn draw_items_pane(frame: &mut Frame, area: Rect, app: &App) {
    let focused = app.playlist_pane == PlaylistPane::Items;
    let border = if focused { theme::ACCENT } else { theme::DIM };
    let playlist = app.playlists.playlists.get(app.playlist_cursor);
    let title = match playlist {
        Some(p) => format!(" {} ", widgets::truncate(&p.name, 30)),
        None => " Tracks ".to_string(),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(playlist) = playlist else {
        return;
    };
    if playlist.items.is_empty() {
        let hint = Paragraph::new(Span::styled(
            "Empty — press  a  to add tracks from Browse.",
            Style::default().fg(theme::DIM),
        ))
        .alignment(Alignment::Center);
        frame.render_widget(hint, widgets::vertically_centered(inner, 1));
        return;
    }

    let lines: Vec<Line> = playlist
        .items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let selected = i == app.playlist_item_cursor;
            let marker = if selected { "▸ " } else { "  " };
            let mut line = Line::from(vec![
                Span::styled(
                    format!("{marker}{:>2}. ", i + 1),
                    Style::default().fg(theme::DIM),
                ),
                Span::raw(item_label(app, item)),
            ]);
            if selected && focused {
                line = line.style(Style::default().add_modifier(Modifier::REVERSED));
            }
            line
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Human-readable label for one playlist track, e.g. `Al-Mulk`,
/// `Al-Baqarah · ayah 1–20`, or `An-Naba → An-Nas`.
fn item_label(app: &App, item: &PlaylistItem) -> String {
    let name = |n: u16| {
        app.catalog
            .surah(n)
            .map(|s| s.name_transliterated.clone())
            .unwrap_or_else(|| format!("Surah {n}"))
    };
    if item.from_surah == item.to_surah {
        let full = app
            .catalog
            .surah(item.from_surah)
            .map(|s| item.from_ayah <= 1 && item.to_ayah >= s.ayah_count)
            .unwrap_or(false);
        if full {
            name(item.from_surah)
        } else {
            format!(
                "{} · ayah {}–{}",
                name(item.from_surah),
                item.from_ayah,
                item.to_ayah
            )
        }
    } else {
        format!("{} → {}", name(item.from_surah), name(item.to_surah))
    }
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let text = match app.playlist_pane {
        PlaylistPane::Playlists => {
            "  [Enter] play   [n] new   [r] rename   [d] delete   [a] add tracks   [Tab] tracks"
        }
        PlaylistPane::Items => {
            "  [Enter] play   [d] remove track   [a] add more   [Tab] playlists   ↑/↓ select"
        }
    };
    let line = Line::from(Span::styled(text, Style::default().fg(theme::DIM)));
    frame.render_widget(Paragraph::new(line), area);
}
