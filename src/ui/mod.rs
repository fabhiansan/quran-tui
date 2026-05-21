//! TUI rendering. `draw` lays out the global frame (header, content, transport)
//! and dispatches the content region to the active tab.

mod browse;
mod help;
mod now_playing;
mod outputs;
mod presets;
mod theme;
mod widgets;

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Tabs};
use ratatui::Frame;

use crate::app::{App, Modal, Tab, ToastKind};

/// Minimum supported terminal size (§7.1).
const MIN_WIDTH: u16 = 80;
const MIN_HEIGHT: u16 = 24;

/// Render one frame.
pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        draw_size_guard(frame, area);
        return;
    }

    // header (1) · tab strip (1) · content (flex) · transport (2)
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(2),
    ])
    .split(area);

    draw_header(frame, rows[0], app);
    draw_tabs(frame, rows[1], app);
    draw_content(frame, rows[2], app);
    draw_transport(frame, rows[3], app);

    if app.toast.is_some() {
        draw_toast(frame, rows[2], app);
    }
    if app.show_help {
        help::draw(frame, area);
    }
    if let Some(modal) = &app.modal {
        draw_modal(frame, area, modal);
    }
}

/// Centered message shown when the terminal is below the minimum size.
fn draw_size_guard(frame: &mut Frame, area: Rect) {
    let msg = Paragraph::new(format!(
        "Terminal too small (need {MIN_WIDTH}x{MIN_HEIGHT})"
    ))
    .style(Style::default().fg(theme::WARNING))
    .alignment(Alignment::Center);
    frame.render_widget(msg, widgets::vertically_centered(area, 1));
}

/// Header bar: title on the left, an output summary chip on the right.
fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let title = Paragraph::new(Line::from(Span::styled(
        " Quran Player",
        Style::default()
            .fg(theme::ACCENT)
            .add_modifier(Modifier::BOLD),
    )));
    frame.render_widget(title, area);

    let playing = app.outputs.iter().filter(|o| o.is_playing()).count();
    let chip = Paragraph::new(Span::styled(
        format!("♪ {} output(s) · {playing} playing ", app.outputs.len()),
        Style::default().fg(theme::DIM),
    ))
    .alignment(Alignment::Right);
    frame.render_widget(chip, area);
}

/// Tab strip: four tabs, the active one highlighted.
fn draw_tabs(frame: &mut Frame, area: Rect, app: &App) {
    let titles: Vec<Line> = Tab::ALL
        .iter()
        .enumerate()
        .map(|(i, t)| Line::from(format!(" {} {} ", i + 1, t.title())))
        .collect();

    let tabs = Tabs::new(titles)
        .select(app.active_tab.index())
        .style(Style::default().fg(theme::DIM))
        .highlight_style(
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        )
        .divider(" ");
    frame.render_widget(tabs, area);
}

/// Content region for the active tab.
fn draw_content(frame: &mut Frame, area: Rect, app: &mut App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::DIM))
        .title(format!(" {} ", app.active_tab.title()));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    match app.active_tab {
        Tab::NowPlaying => now_playing::draw(frame, inner, app),
        Tab::Browse => browse::draw(frame, inner, app),
        Tab::Outputs => outputs::draw(frame, inner, app),
        Tab::Presets => presets::draw(frame, inner, app),
    }
}

/// A modal dialog drawn centered over the current tab (§7.7).
fn draw_modal(frame: &mut Frame, area: Rect, modal: &Modal) {
    let popup = widgets::centered_rect(area, 56, 30);
    frame.render_widget(Clear, popup);
    let dim = Style::default().fg(theme::DIM);

    let (title, body): (&str, Vec<Line>) = match modal {
        Modal::Text { title, input, .. } => (
            title,
            vec![
                Line::from(""),
                Line::from(vec![
                    Span::raw("  > "),
                    Span::styled(input.clone(), Style::default().add_modifier(Modifier::BOLD)),
                    Span::styled("▏", Style::default().fg(theme::ACCENT)),
                ]),
                Line::from(""),
                Line::from(Span::styled("  [Enter] confirm    [Esc] cancel", dim)),
            ],
        ),
        Modal::Confirm { title, message, .. } => (
            title,
            vec![
                Line::from(""),
                Line::from(format!("  {message}")),
                Line::from(""),
                Line::from(Span::styled("  [y] confirm    [n] cancel", dim)),
            ],
        ),
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::ACCENT))
        .title(format!(" {title} "));
    frame.render_widget(Paragraph::new(body).block(block), popup);
}

/// Transport bar: focused-output status line + a context-sensitive hint line.
fn draw_transport(frame: &mut Frame, area: Rect, app: &App) {
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(area);
    let output = app.focused();

    let status = if let Some(dl) = &output.download {
        Line::from(vec![
            Span::styled(" ⇣ Downloading ", Style::default().fg(theme::WARNING)),
            Span::raw(dl.label.clone()),
            Span::styled(
                format!("  ({}/{})", dl.done, dl.total),
                Style::default().fg(theme::DIM),
            ),
        ])
    } else {
        let (icon, label, color) = widgets::engine_state_chip(&output.state);
        let mut spans = vec![Span::styled(
            format!(" {icon}  {label}"),
            Style::default().fg(color),
        )];
        if output.track_total > 0 {
            let current = output.current_track_label().unwrap_or("—");
            spans.push(Span::raw(format!(
                "   {current}  ({}/{})",
                output.track_index + 1,
                output.track_total
            )));
            let total = output
                .track_len
                .map(widgets::fmt_mmss)
                .unwrap_or_else(|| "--:--".to_string());
            spans.push(Span::styled(
                format!("   {} / {}", widgets::fmt_mmss(output.elapsed), total),
                Style::default().fg(theme::DIM),
            ));
        }
        spans.push(Span::styled(
            format!("   vol {}%", (output.config.volume * 100.0).round() as u32),
            Style::default().fg(theme::DIM),
        ));
        if output.config.loop_enabled {
            spans.push(Span::styled("   loop", Style::default().fg(theme::OK)));
        }
        Line::from(spans)
    };
    frame.render_widget(Paragraph::new(status), rows[0]);

    let hint = Paragraph::new(Span::styled(
        transport_hint(app.active_tab),
        Style::default().fg(theme::DIM),
    ));
    frame.render_widget(hint, rows[1]);
}

/// Context-sensitive keybinding hint for the transport bar's second row.
fn transport_hint(tab: Tab) -> &'static str {
    match tab {
        Tab::Browse => {
            " Tab field   ←/→ reciter   / search   Enter play   1 Now Playing   ? help   q quit"
        }
        Tab::Outputs => {
            " Tab switch list   ↑/↓ select   + add   Enter bind   d delete   r refresh   q quit"
        }
        Tab::Presets => {
            " ↑/↓ select   Enter load   s save   d delete   r rename   1 Now Playing   q quit"
        }
        _ => {
            " Space play/pause   s stop   n/p track   l loop   +/- vol   Tab tabs   ? help   q quit"
        }
    }
}

/// A transient status message, drawn on the bottom row of the content region.
fn draw_toast(frame: &mut Frame, content_area: Rect, app: &App) {
    let Some(toast) = &app.toast else {
        return;
    };
    if content_area.height < 3 {
        return;
    }
    let color = match toast.kind {
        ToastKind::Info => theme::ACCENT,
        ToastKind::Warn => theme::WARNING,
        ToastKind::Error => theme::ERROR,
    };
    let row = Rect {
        x: content_area.x + 1,
        y: content_area.y + content_area.height - 2,
        width: content_area.width.saturating_sub(2),
        height: 1,
    };
    frame.render_widget(Clear, row);
    let para = Paragraph::new(Span::styled(
        format!(" {} ", toast.text),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    ))
    .alignment(Alignment::Center);
    frame.render_widget(para, row);
}
