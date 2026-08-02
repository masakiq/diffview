use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{App, DiffTool, Focus};

pub fn render(f: &mut Frame, app: &App, area: Rect) {
    let spans = if let Some(prompt) = app.search_prompt() {
        vec![
            Span::styled(
                " [SEARCH] ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(" {}", prompt)),
        ]
    } else if let Some(ref err) = app.error_message {
        vec![Span::styled(
            format!(" ⚠ {}", err),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )]
    } else if let Some(ref msg) = app.status_message {
        vec![Span::styled(
            format!(" {}", msg),
            Style::default().fg(Color::Yellow),
        )]
    } else if app.focus == Focus::InlineSelect {
        vec![
            Span::styled(
                " [SELECT] ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(" {}", app.inline_select_help_text())),
        ]
    } else {
        build_normal_statusbar(app)
    };

    let line = Line::from(spans);
    let widget = Paragraph::new(line).style(Style::default().bg(Color::DarkGray).fg(Color::White));
    f.render_widget(widget, area);
}

fn build_normal_statusbar(app: &App) -> Vec<Span<'static>> {
    let tool_label = match &app.tool {
        DiffTool::Raw => " tool:raw ",
        DiffTool::Delta => " tool:delta ",
        DiffTool::Difftastic => " tool:difftastic ",
    };

    let ops = if app.is_commit() {
        match app.focus {
            Focus::Unstaged | Focus::Staged => app.tree_help_text(),
            Focus::DiffView => app.diff_help_text(),
            Focus::InlineSelect => app.inline_select_help_text(),
        }
    } else {
        match app.focus {
            Focus::Unstaged | Focus::Staged => app.tree_help_text(),
            Focus::DiffView => app.diff_help_text(),
            Focus::InlineSelect => app.inline_select_help_text(),
        }
    };

    let status_legend = if app.is_commit() {
        "  M=modified A=added D=deleted R=renamed C=copied"
    } else {
        "  M=modified A=added D=deleted ?=untracked"
    };

    vec![
        Span::styled(
            tool_label.to_string(),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(" {}", ops)),
        Span::styled(status_legend, Style::default().fg(Color::DarkGray)),
    ]
}
