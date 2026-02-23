use ansi_to_tui::IntoText;
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::app::{App, DiffTool, Focus};

pub fn render(f: &mut Frame, app: &App, area: Rect) {
    let focused = matches!(app.focus, Focus::DiffView | Focus::InlineSelect);

    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let origin_label = match app.diff_origin {
        Some(pane) => pane.label().to_lowercase(),
        None => String::new(),
    };

    let title = match &app.current_file {
        Some(path) => {
            if app.file_diff.is_binary {
                format!(" {} [{}][binary] ", path, origin_label)
            } else if !app.file_diff.hunks.is_empty() {
                format!(
                    " {} [{}] (hunk {}/{}) ",
                    path,
                    origin_label,
                    app.hunk_cursor + 1,
                    app.file_diff.hunks.len()
                )
            } else {
                format!(" {} [{}] ", path, origin_label)
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
            DiffTool::Raw => (&app.display_diff, true),
            _ => (&app.display_diff, false),
        }
    };

    let scroll = app.diff_scroll as u16;

    if use_raw_renderer {
        let text = build_raw_diff_text(app, content);
        let para = Paragraph::new(text).scroll((scroll, 0));
        f.render_widget(para, inner_area);
    } else {
        let text = content
            .as_bytes()
            .into_text()
            .unwrap_or_else(|_| build_raw_diff_text(app, content));
        let para = Paragraph::new(text).scroll((scroll, 0));
        f.render_widget(para, inner_area);
    }
}

fn build_raw_diff_text<'a>(app: &App, content: &'a str) -> Text<'a> {
    let inline_select = app.focus == Focus::InlineSelect;

    let lines: Vec<Line<'a>> = content
        .lines()
        .enumerate()
        .map(|(display_idx, line)| {
            let base_style = diff_line_style(line);

            if inline_select {
                let is_cursor = display_idx == app.diff_cursor;
                let is_selectable = app
                    .line_infos
                    .get(display_idx)
                    .map(|info| info.is_selectable)
                    .unwrap_or(false);

                let style = if is_cursor {
                    base_style.bg(Color::DarkGray).add_modifier(Modifier::BOLD)
                } else if is_selectable {
                    base_style
                } else {
                    base_style
                };

                Line::from(Span::styled(line.to_string(), style))
            } else {
                Line::from(Span::styled(line.to_string(), base_style))
            }
        })
        .collect();

    Text::from(lines)
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
