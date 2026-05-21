//! Browse tab — pick a reciter, surah, and ayah range (§7.3).

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::Frame;

use super::theme;
use crate::app::{App, BrowseField};
use crate::content::resolver;
use crate::domain::segment::playback_segments;

/// Draw the Browse content region.
pub fn draw(frame: &mut Frame, area: Rect, app: &mut App) {
    let rows = Layout::vertical([
        Constraint::Length(1), // reciter
        Constraint::Length(1), // surah label / search
        Constraint::Min(3),    // surah list
        Constraint::Length(1), // spacer
        Constraint::Length(1), // range fields
        Constraint::Length(1), // range hint
        Constraint::Length(1), // availability
        Constraint::Length(1), // footer
    ])
    .split(area);

    draw_reciter(frame, rows[0], app);
    draw_surah_label(frame, rows[1], app);
    draw_surah_list(frame, rows[2], app);
    draw_range(frame, rows[4], app);
    draw_range_hint(frame, rows[5]);
    draw_availability(frame, rows[6], app);
    draw_footer(frame, rows[7]);
}

fn dim() -> Style {
    Style::default().fg(theme::DIM)
}

fn draw_reciter(frame: &mut Frame, area: Rect, app: &App) {
    let cfg = &app.focused().config;
    let name = app
        .catalog
        .reciter(&cfg.reciter_id)
        .map(|r| r.display_name.as_str())
        .unwrap_or(&cfg.reciter_id);
    let line = Line::from(vec![
        Span::styled("  Reciter   ", dim()),
        Span::styled(
            format!("‹ {name} ›"),
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("   (←/→ to change)", dim()),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn draw_surah_label(frame: &mut Frame, area: Rect, app: &App) {
    let spans = match &app.browse.search {
        Some(query) => vec![
            Span::styled("  Surah     ", dim()),
            Span::styled(
                format!("search: {query}"),
                Style::default().fg(theme::ACCENT),
            ),
            Span::styled("▏", Style::default().fg(theme::ACCENT)),
            Span::styled("   (Esc / Enter to exit search)", dim()),
        ],
        None => vec![
            Span::styled("  Surah     ", dim()),
            Span::styled("(press  /  to search)", dim()),
        ],
    };
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_surah_list(frame: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.browse.field == BrowseField::SurahList || app.browse.search.is_some();
    let border_color = if focused { theme::ACCENT } else { theme::DIM };

    let items: Vec<ListItem> = {
        let surahs = app.filtered_surahs();
        surahs
            .iter()
            .map(|s| {
                ListItem::new(format!(
                    "  {:>3}  {:<26}{:<9}{:>3} ayahs",
                    s.number, s.name_transliterated, s.revelation_place, s.ayah_count
                ))
            })
            .collect()
    };
    let count = items.len();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(format!(" {count} surah(s) "));
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    frame.render_stateful_widget(list, area, &mut app.browse.list_state);
}

fn draw_range(frame: &mut Frame, area: Rect, app: &App) {
    let cfg = app.focused().config.clone();
    let spans = vec![
        Span::styled("  Range     from surah ", dim()),
        field_span(app, BrowseField::FromSurah, cfg.from_surah),
        Span::styled(" ayah ", dim()),
        field_span(app, BrowseField::FromAyah, cfg.from_ayah),
        Span::styled("   to surah ", dim()),
        field_span(app, BrowseField::ToSurah, cfg.to_surah),
        Span::styled(" ayah ", dim()),
        field_span(app, BrowseField::ToAyah, cfg.to_ayah),
    ];
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Render one editable numeric field, highlighted when focused.
fn field_span(app: &App, field: BrowseField, value: u16) -> Span<'static> {
    let is_focused = app.browse.field == field && app.browse.search.is_none();
    let content = if is_focused && !app.browse.edit_buffer.is_empty() {
        format!("[{:>3}]", app.browse.edit_buffer)
    } else {
        format!("[{value:>3}]")
    };
    if is_focused {
        Span::styled(
            content,
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::REVERSED),
        )
    } else {
        Span::styled(content, Style::default().fg(theme::DIM))
    }
}

fn draw_range_hint(frame: &mut Frame, area: Rect) {
    let line = Line::from(Span::styled(
        "            (Tab cycles fields · type digits · ↑/↓ adjust · Enter plays)",
        dim(),
    ));
    frame.render_widget(Paragraph::new(line), area);
}

fn draw_availability(frame: &mut Frame, area: Rect, app: &App) {
    let cfg = &app.focused().config;
    let segments = playback_segments(
        &app.catalog.surahs,
        cfg.from_surah,
        cfg.to_surah,
        cfg.from_ayah,
        cfg.to_ayah,
    );
    let total: usize = segments
        .iter()
        .map(|s| {
            let len = (*s.range.end() - *s.range.start() + 1) as usize;
            len + usize::from(resolver::should_prepend_bismillah(s))
        })
        .sum();

    let to_download = match app.catalog.reciter(&cfg.reciter_id) {
        Some(reciter) => resolver::missing_files(&segments, reciter, &app.config.audio_root).len(),
        None => total,
    };
    let local = total.saturating_sub(to_download);

    let (text, color) = if total == 0 {
        ("(empty range)".to_string(), theme::DIM)
    } else if to_download == 0 {
        (format!("all {local} file(s) local"), theme::OK)
    } else {
        (
            format!("{local} local · {to_download} to download"),
            theme::WARNING,
        )
    };
    let line = Line::from(vec![
        Span::styled("  Availability:  ", dim()),
        Span::styled(text, Style::default().fg(color)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn draw_footer(frame: &mut Frame, area: Rect) {
    let line = Line::from(Span::styled(
        "  [Enter] play on focused output    [a] queue without playing",
        dim(),
    ));
    frame.render_widget(Paragraph::new(line), area);
}
