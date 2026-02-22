pub mod diff;
pub mod statusbar;
pub mod tree;

use ratatui::{
    layout::{Constraint, Direction, Layout},
    Frame,
};

use crate::app::App;

pub fn render(f: &mut Frame, app: &App) {
    let size = f.area();

    // Split vertically: main area + status bar (1 line)
    let vert = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(size);

    let main_area = vert[0];
    let status_area = vert[1];

    // Split main area horizontally: tree (1/4) + diff (3/4)
    let horiz = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Ratio(1, 4), Constraint::Ratio(3, 4)])
        .split(main_area);

    let tree_area = horiz[0];
    let diff_area = horiz[1];

    tree::render(f, app, tree_area);
    diff::render(f, app, diff_area);
    statusbar::render(f, app, status_area);
}
