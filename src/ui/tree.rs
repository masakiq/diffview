use ratatui::{
    layout::Rect,
    style::{Color, Style},
    widgets::{Block, Borders, List, ListItem, ListState},
    Frame,
};

use crate::app::{App, TreePane};
use crate::components::tree_row::render_row;

pub fn render(f: &mut Frame, app: &App, area: Rect, pane: TreePane) {
    let focused = app.is_tree_focused(pane);
    let tree = app.tree(pane);
    let show_cursor = focused;

    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let pane_label = app.tree_title(pane);
    let title = if tree.visible.is_empty() {
        format!(" {} (0) ", pane_label)
    } else {
        format!(" {} ({}) ", pane_label, tree.file_count())
    };

    if tree.is_empty() {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(title);
        f.render_widget(block, area);
        return;
    }

    let is_commit = app.is_commit();
    let search_query = app.tree_search_query(pane);
    let items: Vec<ListItem> = tree
        .visible
        .iter()
        .enumerate()
        .map(|(display_idx, &node_idx)| {
            let node = &tree.all_nodes[node_idx];
            let is_selected = show_cursor && display_idx == tree.cursor;
            render_row(node, pane, is_selected, is_commit, search_query)
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
    if show_cursor && !tree.is_empty() {
        list_state.select(Some(tree.cursor));
    }

    f.render_stateful_widget(list, area, &mut list_state);
}
