use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::app::{App, DiffTool, DiffViewMode, Focus, FullFileSource};
use crate::ui::highlight::highlight_text;

/// Background tint for full-file view's added/removed line highlight, matching delta's own
/// default `plus-color`/`minus-color` so patch view (under `--tool delta`) and full-file view
/// read as the same diff. Dark and desaturated so it tints bat's syntax-highlighted
/// foreground rather than replacing it.
const FULL_FILE_ADDED_BG: Color = Color::Rgb(0, 40, 0);
const FULL_FILE_REMOVED_BG: Color = Color::Rgb(63, 0, 1);

/// Background for the full-file line-select cursor/range, matching the existing
/// `InlineSelect` cursor's `DarkGray` convention (`build_raw_diff_text`, below).
const FULL_FILE_SELECT_BG: Color = Color::DarkGray;

pub fn render(f: &mut Frame, app: &App, area: Rect) {
    let focused = matches!(
        app.focus,
        Focus::DiffView | Focus::InlineSelect | Focus::FullFileSelect
    );

    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let origin_label = match app.diff_origin {
        Some(pane) => app.diff_origin_label(pane),
        None => String::new(),
    };

    let title = match &app.current_file {
        Some(path) => {
            if matches!(app.diff_view_mode, crate::app::DiffViewMode::FullFile(_)) {
                let mode_label = app
                    .content_annotation
                    .map(|annotation| annotation.title_label())
                    .unwrap_or_else(|| app.diff_view_mode.label());
                format!(" {} [{}] [{}] ", path, origin_label, mode_label)
            } else if app.file_diff.is_binary {
                format!(" {} [{}][binary] ", path, origin_label)
            } else if !app.file_diff.hunks.is_empty() {
                format!(
                    " {} [{}] [{}] (hunk {}/{}) ",
                    path,
                    origin_label,
                    app.diff_view_mode.label(),
                    app.hunk_cursor + 1,
                    app.file_diff.hunks.len()
                )
            } else {
                format!(
                    " {} [{}] [{}] ",
                    path,
                    origin_label,
                    app.diff_view_mode.label()
                )
            }
        }
        None => " Diff ".to_string(),
    };

    let inner = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(title);

    let inner_area = inner.inner(area);
    f.render_widget(inner, area);

    if app.current_file.is_none() {
        let hint = Paragraph::new("Select a file and press 'l' to view its diff.")
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(hint, inner_area);
        return;
    }

    let (content, use_raw_renderer) = if app.focus == Focus::InlineSelect {
        (&app.raw_diff, true)
    } else {
        match app.tool {
            DiffTool::Raw => (&app.display_diff, app.cached_display_text.is_none()),
            _ => (&app.display_diff, false),
        }
    };

    let scroll = app.diff_scroll as u16;

    let text = if use_raw_renderer {
        build_raw_diff_text(app, content)
    } else {
        app.cached_display_text
            .clone()
            .unwrap_or_else(|| build_raw_diff_text(app, content))
    };
    let text = apply_full_file_line_bg(text, app, inner_area.width);
    let text = apply_full_file_select_cursor(text, app, inner_area.width);
    let para = Paragraph::new(highlight_text(text, app.diff_search_query())).scroll((scroll, 0));
    f.render_widget(para, inner_area);
}

fn build_raw_diff_text<'a>(app: &App, content: &'a str) -> Text<'a> {
    let inline_select = app.focus == Focus::InlineSelect;

    let lines: Vec<Line<'a>> = content
        .lines()
        .enumerate()
        .map(|(display_idx, line)| {
            let base_style = diff_line_style(line);

            let style = if inline_select && display_idx == app.diff_cursor {
                base_style.bg(Color::DarkGray).add_modifier(Modifier::BOLD)
            } else {
                base_style
            };

            Line::from(Span::styled(line.to_string(), style))
        })
        .collect();

    Text::from(lines)
}

/// Recolors every span's background to `bg` and pads with blank, `bg`-styled cells out
/// to `width`, so the tint reaches the right edge of the pane instead of stopping at
/// the end of the line's own text.
fn tint_line_bg<'a>(spans: Vec<Span<'a>>, bg: Color, width: usize) -> Vec<Span<'a>> {
    let mut spans: Vec<Span<'a>> = spans
        .into_iter()
        .map(|span| Span {
            style: span.style.bg(bg),
            content: span.content,
        })
        .collect();

    let pad = width.saturating_sub(spans.iter().map(Span::width).sum());
    if pad > 0 {
        spans.push(Span::styled(" ".repeat(pad), Style::default().bg(bg)));
    }
    spans
}

/// Overlays an added/removed background tint on full-file view rows that the currently
/// loaded diff marks as changed (`app.full_file_highlight_lines`, 1-based file line numbers).
/// A no-op outside full-file view, or once no lines are marked (e.g. patch view, or an
/// unchanged file). `app.full_file_content_offset` accounts for bat's leading decoration
/// rows, so row indices line up with file line numbers the same way scroll targeting does.
fn apply_full_file_line_bg<'a>(text: Text<'a>, app: &App, width: u16) -> Text<'a> {
    let bg = match app.diff_view_mode {
        DiffViewMode::FullFile(FullFileSource::Current) => FULL_FILE_ADDED_BG,
        DiffViewMode::FullFile(FullFileSource::Previous) => FULL_FILE_REMOVED_BG,
        DiffViewMode::Patch => return text,
    };
    if app.full_file_highlight_lines.is_empty() {
        return text;
    }

    let offset = app.full_file_content_offset;
    let width = width as usize;
    let lines = text
        .lines
        .into_iter()
        .enumerate()
        .map(|(row, line)| {
            let Some(file_line) = row.checked_sub(offset).map(|n| n as u32 + 1) else {
                return line;
            };
            if app
                .full_file_highlight_lines
                .binary_search(&file_line)
                .is_err()
            {
                return line;
            }

            Line {
                style: line.style,
                alignment: line.alignment,
                spans: tint_line_bg(line.spans, bg, width),
            }
        })
        .collect();

    Text {
        alignment: text.alignment,
        style: text.style,
        lines,
    }
}

/// Overlays the line-select cursor/range on full-file view rows while
/// `Focus::FullFileSelect` is active: every row within `[anchor, cursor]` (or just the
/// cursor's own row when no range is active) gets `FULL_FILE_SELECT_BG`, with the exact
/// cursor row additionally bolded to mark it within a multi-row range. Wins over the
/// add/removed diff tint on overlapping rows, since it's applied afterward.
fn apply_full_file_select_cursor<'a>(text: Text<'a>, app: &App, width: u16) -> Text<'a> {
    if app.focus != Focus::FullFileSelect {
        return text;
    }

    let offset = app.full_file_content_offset;
    let cursor = app.full_file_select_cursor;
    let (lo, hi) = match app.full_file_select_anchor {
        Some(anchor) => (anchor.min(cursor), anchor.max(cursor)),
        None => (cursor, cursor),
    };
    let width = width as usize;

    let lines = text
        .lines
        .into_iter()
        .enumerate()
        .map(|(row, line)| {
            let Some(file_idx) = row.checked_sub(offset) else {
                return line;
            };
            if file_idx < lo || file_idx > hi {
                return line;
            }

            let mut spans = tint_line_bg(line.spans, FULL_FILE_SELECT_BG, width);
            if file_idx == cursor {
                spans = spans
                    .into_iter()
                    .map(|span| Span {
                        style: span.style.add_modifier(Modifier::BOLD),
                        content: span.content,
                    })
                    .collect();
            }

            Line {
                style: line.style,
                alignment: line.alignment,
                spans,
            }
        })
        .collect();

    Text {
        alignment: text.alignment,
        style: text.style,
        lines,
    }
}

fn diff_line_style(line: &str) -> Style {
    if line.starts_with('+') && !line.starts_with("+++") {
        Style::default().fg(Color::Green)
    } else if line.starts_with('-') && !line.starts_with("---") {
        Style::default().fg(Color::Red)
    } else if line.starts_with("@@") {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else if line.starts_with("diff ")
        || line.starts_with("--- ")
        || line.starts_with("+++ ")
        || line.starts_with("index ")
    {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    }
}
