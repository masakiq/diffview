use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{App, DiffTool, Focus};

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
    } else if app.focus == Focus::InlineSelect {
        vec![
            Span::styled(
                " [SELECT] ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" j/k:move  Enter:apply  n/p:hunk  v:back  h:tree"),
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

    let ops = match app.focus {
        Focus::Unstaged | Focus::Staged => {
            " [l]open [h]back [Enter]stage/unstage [j/k]move [?]help [q]quit"
        }
        Focus::DiffView => {
            if app.tool.supports_line_ops() {
                " [j/k]scroll [h]back [v]select [n/p]hunk [q]quit"
            } else {
                " [j/k]scroll [h]back [n/p]hunk [q]quit"
            }
        }
        Focus::InlineSelect => {
            " [j/k]move [Enter]apply [n/p]hunk [v]back [h]tree"
        }
    };

    vec![
        Span::styled(
            tool_label.to_string(),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(ops.to_string()),
        Span::styled(
            "  M=modified A=added D=deleted ?=untracked",
            Style::default().fg(Color::DarkGray),
        ),
    ]
}
