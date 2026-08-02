pub mod diff;
pub mod statusbar;

use ratatui::{
    layout::{Constraint, Direction, Layout},
    Frame,
};

use crate::app::{ActiveView, App, TreePane};
use crate::views;

pub fn render(f: &mut Frame, app: &App) {
    let size = f.area();

    // Split vertically: main area + status bar (1 line)
    let vert = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(size);

    let main_area = vert[0];
    let status_area = vert[1];

    if app.active_view() == ActiveView::Diff {
        diff::render(f, app, main_area);
        statusbar::render(f, app, status_area);
        return;
    }

    // Split main area horizontally: tree pane (1/4) + diff pane (3/4)
    let horiz = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(app.tree_pane_percentage()),
            Constraint::Percentage(app.diff_pane_percentage()),
        ])
        .split(main_area);

    let tree_area = horiz[0];
    let diff_area = horiz[1];

    if app.is_commit() {
        views::tree::render(f, app, tree_area, TreePane::Unstaged);
    } else {
        // Split tree area vertically into unstaged (top) and staged (bottom)
        let unstaged_items = app.tree.unstaged.visible.len() as u32 + 2; // +2 for border
        let staged_items = app.tree.staged.visible.len() as u32 + 2;
        let total = unstaged_items + staged_items;

        let tree_split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Ratio(unstaged_items.max(3), total.max(6)),
                Constraint::Ratio(staged_items.max(3), total.max(6)),
            ])
            .split(tree_area);

        let unstaged_area = tree_split[0];
        let staged_area = tree_split[1];

        views::tree::render(f, app, unstaged_area, TreePane::Unstaged);
        views::tree::render(f, app, staged_area, TreePane::Staged);
    }
    diff::render(f, app, diff_area);
    statusbar::render(f, app, status_area);
}
