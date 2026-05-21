//! Outputs tab — manage output channels and bind audio devices (§7.4).

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use super::{theme, widgets};
use crate::app::{App, OutputsFocus};

/// Draw the Outputs content region.
pub fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let rows = Layout::vertical([
        Constraint::Length(1), // channels header
        Constraint::Min(3),    // channels list
        Constraint::Length(1), // devices header
        Constraint::Min(3),    // devices list
        Constraint::Length(1), // footer
    ])
    .split(area);

    draw_channels(frame, rows[0], rows[1], app);
    draw_devices(frame, rows[2], rows[3], app);
    draw_footer(frame, rows[4]);
}

fn header(text: &str) -> Paragraph<'static> {
    Paragraph::new(Span::styled(
        format!("  {text}"),
        Style::default().fg(theme::DIM).add_modifier(Modifier::BOLD),
    ))
}

fn draw_channels(frame: &mut Frame, header_area: Rect, list_area: Rect, app: &App) {
    frame.render_widget(header("Output channels"), header_area);

    let list_focused = app.outputs_focus == OutputsFocus::Channels;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if list_focused {
            theme::ACCENT
        } else {
            theme::DIM
        }));
    let inner = block.inner(list_area);
    frame.render_widget(block, list_area);

    let lines: Vec<Line> = app
        .outputs
        .iter()
        .enumerate()
        .map(|(i, output)| {
            let selected = i == app.focused_output;
            let marker = if selected { "▸ " } else { "  " };
            let (icon, label, color) = widgets::engine_state_chip(&output.state);
            let mut line = Line::from(vec![
                Span::raw(format!("{marker}{}  ", output.id)),
                Span::raw(format!("{:<24}", widgets::truncate(&output.label, 24))),
                Span::styled(format!("{icon} {label}"), Style::default().fg(color)),
            ]);
            if selected && list_focused {
                line = line.style(Style::default().add_modifier(Modifier::REVERSED));
            }
            line
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_devices(frame: &mut Frame, header_area: Rect, list_area: Rect, app: &App) {
    frame.render_widget(header("Detected output devices"), header_area);

    let list_focused = app.outputs_focus == OutputsFocus::Devices;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if list_focused {
            theme::ACCENT
        } else {
            theme::DIM
        }));
    let inner = block.inner(list_area);
    frame.render_widget(block, list_area);

    if app.devices.is_empty() {
        let hint = Paragraph::new(Span::styled(
            "  (no devices — press r to refresh)",
            Style::default().fg(theme::DIM),
        ));
        frame.render_widget(hint, inner);
        return;
    }

    let lines: Vec<Line> = app
        .devices
        .iter()
        .enumerate()
        .map(|(i, device)| {
            let selected = i == app.device_cursor;
            let marker = if selected { "▸ " } else { "  " };
            let mut spans = vec![Span::raw(format!(
                "{marker}{:<26}",
                widgets::truncate(&device.name, 26)
            ))];
            if device.is_default {
                spans.push(Span::styled(" [default]", Style::default().fg(theme::DIM)));
            }
            if device.bt_hint {
                spans.push(Span::styled(" BT?", Style::default().fg(theme::ACCENT)));
            }
            if let Some(id) = output_using(app, &device.name) {
                spans.push(Span::styled(
                    format!("  ● in use by output {id}"),
                    Style::default().fg(theme::OK),
                ));
            }
            let mut line = Line::from(spans);
            if selected && list_focused {
                line = line.style(Style::default().add_modifier(Modifier::REVERSED));
            }
            line
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_footer(frame: &mut Frame, area: Rect) {
    let line = Line::from(Span::styled(
        "  [+] add output   [Enter] bind to focused   [d] delete output   [r] refresh   [Tab] switch list",
        Style::default().fg(theme::DIM),
    ));
    frame.render_widget(Paragraph::new(line), area);
}

/// The id of an output channel bound to `device_name`, if any.
fn output_using(app: &App, device_name: &str) -> Option<u32> {
    app.outputs
        .iter()
        .find(|o| o.device_name.as_deref() == Some(device_name))
        .map(|o| o.id)
}
