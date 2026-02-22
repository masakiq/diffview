use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{App, AppMode, DiffTool};

pub fn render(f: &mut Frame, app: &App, area: Rect) {
    let spans = if let Some(ref err) = app.error_message {
        vec![Span::styled(
            format!(" ⚠ {}", err),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )]
    } else if let Some(ref msg) = app.status_message {
        vec![Span::styled(
            format!(" {}", msg),
            Style::default().fg(Color::Yellow),
        )]
    } else if app.mode == AppMode::SelectLines {
        vec![
            Span::styled(
                " [SELECT] ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" j/k:move  Space:toggle  a:stage  r:revert  Esc:exit"),
        ]
    } else {
        build_normal_statusbar(app)
    };

    let line = Line::from(spans);
    let widget = Paragraph::new(line)
        .style(Style::default().bg(Color::DarkGray).fg(Color::White));
    f.render_widget(widget, area);
}

fn build_normal_statusbar(app: &App) -> Vec<Span<'static>> {
    let tool_label = match &app.tool {
        DiffTool::Raw => " tool:raw ",
        DiffTool::Delta => " tool:delta ",
        DiffTool::Difftastic => " tool:difftastic ",
    };

    let staged_hint = if app.staged_only { " [staged] " } else { "" };

    let ops = if app.tool.supports_line_ops() {
        " [a]add [r]revert [A]add-all [R]revert-all [v]line-select [n/p]hunk [?]help [q]quit"
    } else {
        " [a]add [r]revert [A]add-all [R]revert-all (no line-ops) [?]help [q]quit"
    };

    vec![
        Span::styled(
            tool_label.to_string(),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            staged_hint.to_string(),
            Style::default().fg(Color::Green),
        ),
        Span::raw(ops.to_string()),
        Span::styled(
            "  M=modified A=added D=deleted ?=untracked U=unmerged",
            Style::default().fg(Color::DarkGray),
        ),
    ]
}
