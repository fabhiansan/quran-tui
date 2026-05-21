//! Presets tab — save, load, rename, and delete output presets (§7.5).

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use super::{theme, widgets};
use crate::app::App;

/// Draw the Presets content region.
pub fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let rows = Layout::vertical([
        Constraint::Length(1), // header
        Constraint::Min(3),    // preset list
        Constraint::Length(4), // missing-device banner
        Constraint::Length(1), // footer
    ])
    .split(area);

    draw_header(frame, rows[0]);
    draw_list(frame, rows[1], app);
    draw_banner(frame, rows[2], app);
    draw_footer(frame, rows[3]);
}

fn draw_header(frame: &mut Frame, area: Rect) {
    let title = Paragraph::new(Span::styled(
        "  Presets",
        Style::default().fg(theme::DIM).add_modifier(Modifier::BOLD),
    ));
    frame.render_widget(title, area);
    let hint = Paragraph::new(Span::styled(
        "[s] save current ",
        Style::default().fg(theme::DIM),
    ))
    .alignment(Alignment::Right);
    frame.render_widget(hint, area);
}

fn draw_list(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::ACCENT));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.presets.presets.is_empty() {
        let hint = Paragraph::new(Span::styled(
            "No presets yet — press  s  to save the current setup.",
            Style::default().fg(theme::DIM),
        ))
        .alignment(Alignment::Center);
        frame.render_widget(hint, widgets::vertically_centered(inner, 1));
        return;
    }

    let lines: Vec<Line> = app
        .presets
        .presets
        .iter()
        .enumerate()
        .map(|(i, preset)| {
            let selected = i == app.preset_cursor;
            let marker = if selected { "▸ " } else { "  " };
            let outputs = preset.entries.len();
            let mut line = Line::from(vec![
                Span::raw(format!(
                    "{marker}{:<28}",
                    widgets::truncate(&preset.name, 28)
                )),
                Span::styled(
                    format!(
                        "{} output{} · {}",
                        outputs,
                        if outputs == 1 { "" } else { "s" },
                        preset.created_display()
                    ),
                    Style::default().fg(theme::DIM),
                ),
            ]);
            if selected {
                line = line.style(Style::default().add_modifier(Modifier::REVERSED));
            }
            line
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_banner(frame: &mut Frame, area: Rect, app: &App) {
    if app.missing_devices.is_empty() {
        return;
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::WARNING))
        .title(" Missing devices ");
    let names = app.missing_devices.join(", ");
    let text = Paragraph::new(vec![
        Line::from(Span::styled(
            format!(
                "⚠ {} device(s) not present — config kept, not applied:",
                app.missing_devices.len()
            ),
            Style::default().fg(theme::WARNING),
        )),
        Line::from(Span::raw(format!("  {names}"))),
    ])
    .block(block)
    .wrap(Wrap { trim: true });
    frame.render_widget(text, area);
}

fn draw_footer(frame: &mut Frame, area: Rect) {
    let line = Line::from(Span::styled(
        "  [Enter] load   [d] delete   [r] rename   [s] save current   ↑/↓ select",
        Style::default().fg(theme::DIM),
    ));
    frame.render_widget(Paragraph::new(line), area);
}
