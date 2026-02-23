use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState},
    Frame,
};

use crate::app::{App, TreePane};

pub fn render(f: &mut Frame, app: &App, area: Rect, pane: TreePane) {
    let focused = app.is_tree_focused(pane);
    let tree = app.tree(pane);

    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let title = if tree.visible.is_empty() {
        format!(" {} (0) ", pane.label())
    } else {
        format!(
            " {} ({}) ",
            pane.label(),
            tree.file_count()
        )
    };

    if tree.is_empty() {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(title);
        f.render_widget(block, area);
        return;
    }

    let items: Vec<ListItem> = tree
        .visible
        .iter()
        .enumerate()
        .map(|(display_idx, &node_idx)| {
            let node = &tree.all_nodes[node_idx];
            let is_selected = display_idx == tree.cursor;

            let indent = "  ".repeat(node.depth);

            let prefix = if node.is_dir {
                if node.expanded { "▼ " } else { "▶ " }
            } else {
                "  "
            };

            let status_char = if node.is_dir {
                ' '
            } else {
                node.status_for(pane)
            };

            let status_str = if status_char == ' ' || node.is_dir {
                String::new()
            } else {
                format!(" {}", status_char)
            };

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

            let row_style = if is_selected {
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

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title(title),
        )
        .highlight_style(Style::default().bg(Color::DarkGray));

    let mut list_state = ListState::default();
    if !tree.is_empty() {
        list_state.select(Some(tree.cursor));
    }

    f.render_stateful_widget(list, area, &mut list_state);
}
