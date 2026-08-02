use ratatui::{
    style::{Color, Modifier, Style},
    text::Span,
    widgets::ListItem,
};

use crate::app::{TreeNode, TreePane};
use crate::components::highlight::highlight_line;

/// Renders one tree row (file or directory), shared by the Unstaged/Staged/Files
/// sections — the only differences between them are the node's own status chars and
/// whether the tree is under the Commit target (which suppresses unmerged styling).
pub fn render_row<'a>(
    node: &TreeNode,
    pane: TreePane,
    is_selected: bool,
    is_commit: bool,
    search_query: Option<&str>,
) -> ListItem<'a> {
    let prefix = node.display_prefix();

    let status_char = if node.is_dir {
        ' '
    } else {
        node.status_for(pane)
    };

    let status_str = node.display_status_suffix(pane);

    let name_style = if node.is_dir {
        Style::default()
            .fg(Color::Blue)
            .add_modifier(Modifier::BOLD)
    } else if node.is_untracked() {
        Style::default().fg(Color::DarkGray)
    } else if !is_commit && node.is_unmerged() {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else {
        match status_char {
            'M' => Style::default().fg(Color::Yellow),
            'A' => Style::default().fg(Color::Green),
            'D' => Style::default().fg(Color::Red),
            'R' | 'C' => Style::default().fg(Color::Cyan),
            '?' => Style::default().fg(Color::DarkGray),
            _ => Style::default(),
        }
    };

    let status_style = match status_char {
        'M' => Style::default().fg(Color::Yellow),
        'A' => Style::default().fg(Color::Green),
        'D' => Style::default().fg(Color::Red),
        'R' | 'C' => Style::default().fg(Color::Cyan),
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
        Span::styled(prefix, row_style),
        Span::styled(node.name.clone(), name_style.patch(row_style)),
        Span::styled(status_str, status_style.patch(row_style)),
    ];

    ListItem::new(highlight_line(spans, search_query))
}
