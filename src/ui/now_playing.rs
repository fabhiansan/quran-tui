//! Now Playing tab — single-output layout (§7.2).

use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::{theme, widgets};
use crate::app::App;
use crate::model::output::{DownloadProgress, OutputChannel};

/// Draw the Now Playing content region.
pub fn draw(frame: &mut Frame, area: Rect, app: &App) {
    if app.outputs.len() > 1 {
        draw_multi(frame, area, app);
        return;
    }
    let output = app.focused();
    if let Some(progress) = &output.download {
        draw_downloading(frame, area, progress);
    } else if output.track_total == 0 {
        draw_empty(frame, area);
    } else {
        draw_playing(frame, area, app, output);
    }
}

/// Multi-output mode: one compact card per output.
fn draw_multi(frame: &mut Frame, area: Rect, app: &App) {
    let dim = Style::default().fg(theme::DIM);
    let mut lines: Vec<Line> = vec![Line::from("")];

    for (i, output) in app.outputs.iter().enumerate() {
        let focused = i == app.focused_output;
        let marker = if focused { "▸ " } else { "  " };
        let (icon, label, color) = widgets::engine_state_chip(&output.state);

        let detail = if let Some(progress) = &output.download {
            format!("⇣ downloading ({}/{})", progress.done, progress.total)
        } else if output.track_total > 0 {
            format!(
                "{}  ({}/{})",
                output.current_track_label().unwrap_or("—"),
                output.track_index + 1,
                output.track_total
            )
        } else {
            "— nothing loaded —".to_string()
        };

        let name_style = if focused {
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let mut spans = vec![
            Span::styled(format!("  {marker}"), Style::default().fg(theme::ACCENT)),
            Span::styled(
                format!("{:<20}", widgets::truncate(&output.label, 20)),
                name_style,
            ),
            Span::styled(format!("{icon} {label:<9}"), Style::default().fg(color)),
            Span::raw(format!("  {:<30}", widgets::truncate(&detail, 30))),
            Span::styled(
                format!("vol {}%", (output.config.volume * 100.0).round() as u32),
                dim,
            ),
        ];
        if output.config.loop_enabled {
            spans.push(Span::styled(" L", Style::default().fg(theme::OK)));
        }
        lines.push(Line::from(spans));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  ↑/↓ focus   [Space] focused play/pause   [A] play all   [P] pause all   [S] stop all",
        dim,
    )));
    frame.render_widget(Paragraph::new(lines), area);
}

/// Centered download progress shown while files are being fetched.
fn draw_downloading(frame: &mut Frame, area: Rect, progress: &DownloadProgress) {
    let ratio = if progress.total > 0 {
        progress.done as f32 / progress.total as f32
    } else {
        0.0
    };
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "Downloading missing ayah audio…",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            widgets::bar(ratio, 46),
            Style::default().fg(theme::WARNING),
        )),
        Line::from(Span::styled(
            format!(
                "{} / {}   ·   {}",
                progress.done, progress.total, progress.label
            ),
            Style::default().fg(theme::DIM),
        )),
    ];
    let para = Paragraph::new(lines).alignment(Alignment::Center);
    frame.render_widget(para, widgets::vertically_centered(area, 5));
}

/// Centered hint shown before anything is loaded.
fn draw_empty(frame: &mut Frame, area: Rect) {
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "Nothing loaded.",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "Press  2  to browse the Quran and pick a surah.",
            Style::default().fg(theme::DIM),
        )),
    ];
    let para = Paragraph::new(lines).alignment(Alignment::Center);
    frame.render_widget(para, widgets::vertically_centered(area, 3));
}

/// Full playback layout.
fn draw_playing(frame: &mut Frame, area: Rect, app: &App, output: &OutputChannel) {
    let cfg = &output.config;
    let surah = app.catalog.surah(cfg.from_surah);
    let reciter_name = app
        .catalog
        .reciter(&cfg.reciter_id)
        .map(|r| r.display_name.as_str())
        .unwrap_or(&cfg.reciter_id);

    let dim = Style::default().fg(theme::DIM);
    let bold = Style::default().add_modifier(Modifier::BOLD);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("  Output: ", dim),
        Span::raw(output.label.clone()),
    ]));
    lines.push(Line::from(""));

    // Surah heading.
    let (heading, sub) = match surah {
        Some(s) => (
            format!("  ♪  {}    {}", s.name_transliterated, s.name_arabic),
            format!(
                "     Surah {} · {} · {} ayahs",
                s.number, s.revelation_place, s.ayah_count
            ),
        ),
        None => (format!("  ♪  Surah {}", cfg.from_surah), String::new()),
    };
    lines.push(Line::from(Span::styled(heading, bold.fg(theme::ACCENT))));
    if !sub.is_empty() {
        lines.push(Line::from(Span::styled(sub, dim)));
    }
    lines.push(Line::from(vec![
        Span::styled("     Reciter: ", dim),
        Span::raw(reciter_name.to_string()),
    ]));
    lines.push(Line::from(vec![
        Span::styled("     Range: ", dim),
        Span::raw(range_label(cfg)),
        Span::styled("   ·   now playing: ", dim),
        Span::styled(
            output.current_track_label().unwrap_or("—").to_string(),
            Style::default().fg(theme::ACCENT),
        ),
    ]));
    lines.push(Line::from(""));

    // Progress.
    let ratio = progress_ratio(output);
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(widgets::bar(ratio, 46), Style::default().fg(theme::ACCENT)),
        Span::raw(format!("  {:>3} %", (ratio * 100.0).round() as u32)),
    ]));
    let total = output
        .track_len
        .map(widgets::fmt_mmss)
        .unwrap_or_else(|| "--:--".to_string());
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("{} / {}", widgets::fmt_mmss(output.elapsed), total),
            dim,
        ),
        Span::styled(
            format!(
                "    track {} of {}",
                output.track_index + 1,
                output.track_total
            ),
            dim,
        ),
    ]));
    lines.push(Line::from(""));

    // Volume + loop.
    let loop_span = if cfg.loop_enabled {
        Span::styled("● ON", Style::default().fg(theme::OK))
    } else {
        Span::styled("○ off", dim)
    };
    lines.push(Line::from(vec![
        Span::styled("  Volume  ", dim),
        Span::styled(widgets::bar(cfg.volume, 10), Style::default().fg(theme::OK)),
        Span::raw(format!(" {:>3} %", (cfg.volume * 100.0).round() as u32)),
        Span::styled("       Loop  ", dim),
        loop_span,
    ]));

    let (icon, label, color) = widgets::engine_state_chip(&output.state);
    lines.push(Line::from(vec![
        Span::styled("  State   ", dim),
        Span::styled(format!("{icon}  {label}"), Style::default().fg(color)),
    ]));
    if output.is_fallback {
        lines.push(Line::from(Span::styled(
            "  (whole-surah file — per-ayah range not applied)",
            Style::default().fg(theme::WARNING),
        )));
    }

    frame.render_widget(
        Paragraph::new(lines),
        widgets::vertically_centered(area, 14),
    );
}

/// Range label: `ayah 1–110` for one surah, `18:1 – 20:30` for a span.
fn range_label(cfg: &crate::model::playback_config::PlaybackConfig) -> String {
    if cfg.from_surah == cfg.to_surah {
        format!("ayah {}–{}", cfg.from_ayah, cfg.to_ayah)
    } else {
        format!(
            "{}:{} – {}:{}",
            cfg.from_surah, cfg.from_ayah, cfg.to_surah, cfg.to_ayah
        )
    }
}

/// Playback progress as a 0.0–1.0 ratio: within-track if a duration is known,
/// otherwise by track index.
fn progress_ratio(output: &OutputChannel) -> f32 {
    match output.track_len {
        Some(len) if len.as_secs_f32() > 0.0 => output.elapsed.as_secs_f32() / len.as_secs_f32(),
        _ if output.track_total > 0 => output.track_index as f32 / output.track_total as f32,
        _ => 0.0,
    }
}
