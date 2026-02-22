use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState},
    Frame,
};

use crate::app::{App, Focus};

pub fn render(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Tree;

    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let items: Vec<ListItem> = app
        .visible
        .iter()
        .enumerate()
        .map(|(display_idx, &node_idx)| {
            let node = &app.all_nodes[node_idx];
            let is_selected = display_idx == app.tree_cursor;

            let indent = "  ".repeat(node.depth);

            let prefix = if node.is_dir {
                if node.expanded { "▼ " } else { "▶ " }
            } else {
                "  "
            };

            let status_char = if node.is_dir {
                ' '
            } else {
                node.short_status()
            };

            let status_str = if status_char == ' ' || node.is_dir {
                String::new()
            } else {
                format!(" {}", status_char)
            };

            // Colour coding per status
            let name_style = if node.is_dir {
                Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD)
            } else if node.is_untracked() {
                Style::default().fg(Color::DarkGray)
            } else if node.is_unmerged() {
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
            } else {
                match status_char {
                    'M' => Style::default().fg(Color::Yellow),
                    'A' => Style::default().fg(Color::Green),
                    'D' => Style::default().fg(Color::Red),
                    '?' => Style::default().fg(Color::DarkGray),
                    _ => Style::default(),
                }
            };

            let status_style = match status_char {
                'M' => Style::default().fg(Color::Yellow),
                'A' => Style::default().fg(Color::Green),
                'D' => Style::default().fg(Color::Red),
                '?' => Style::default().fg(Color::DarkGray),
                'U' => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                _ => Style::default(),
            };

            let row_style = if is_selected && focused {
                Style::default().bg(Color::DarkGray)
            } else if is_selected {
                Style::default().bg(Color::DarkGray)
            } else {
                Style::default()
            };

            let spans = vec![
                Span::styled(format!("{}{}", indent, prefix), row_style),
                Span::styled(node.name.clone(), name_style.patch(row_style)),
                Span::styled(status_str, status_style.patch(row_style)),
            ];

            ListItem::new(Line::from(spans))
        })
        .collect();

    let title = if app.visible.is_empty() {
        " Files (no changes) ".to_string()
    } else {
        format!(" Files ({}/{}) ", app.tree_cursor + 1, app.visible.len())
    };

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title(title),
        )
        .highlight_style(Style::default().bg(Color::DarkGray));

    let mut list_state = ListState::default();
    if !app.visible.is_empty() {
        list_state.select(Some(app.tree_cursor));
    }

    f.render_stateful_widget(list, area, &mut list_state);
}
