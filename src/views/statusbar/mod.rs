use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{App, DiffContent, DiffTool, Focus};
use crate::domain::content::FullFileSource;

impl App {
    pub fn tree_help_text(&self) -> String {
        if self.is_commit() {
            "[l/Enter]open [h]back [c]copy [/]search [n/N]match [Ctrl-U/D]5-lines [j/k]move [r]refresh [?]help [q]quit".to_string()
        } else {
            format!(
                "[l]open [h]back [u]stage/unstage [c]copy [/]search [n/N]match [Ctrl-U/D]5-lines [j/k]move [r]refresh [{}]commit [?]help [q]quit",
                self.commit_key_label()
            )
        }
    }

    pub fn diff_help_text(&self) -> String {
        let mut ops =
            "[j/k]move [Ctrl-U/D]jump [h]back [c]copy-path [/]search [n/N]match [r]refresh [q]quit"
                .to_string();

        if self.diff.diff_content == DiffContent::Patch {
            ops.push_str(" [[]/[]]hunk");
        }
        if self.diff.diff_content.is_full_file() {
            ops.push_str(" [P]copy-file [v]select [y]copy");
        }

        if !self.is_commit() {
            ops.push_str(&format!(" [{}]commit", self.commit_key_label()));
        }
        if self.diff.diff_content == DiffContent::Patch
            && !self.is_commit()
            && self.tool.supports_line_ops()
        {
            ops.push_str(" [v]select");
        }
        // An untracked/unstaged file has no patch view to toggle to (see
        // `current_file_is_untracked_unstaged`), so `f` is disabled there — drop its
        // hint rather than advertise a key that silently does nothing.
        ops.push_str(match self.diff.diff_content {
            DiffContent::Patch => " [f]file [F]prev-file",
            DiffContent::FullFile(FullFileSource::Current)
                if self.current_file_is_untracked_unstaged() =>
            {
                " [F]prev-file"
            }
            DiffContent::FullFile(FullFileSource::Current) => " [f]diff [F]prev-file",
            // A second `F` here would land back on `Patch` (`toggle_full_file` matches
            // `current == source`) — blocked by the same untracked/unstaged guard
            // `toggle_full_file_view` uses, so drop `[F]diff` from the hint too. `f`
            // still works (goes to `FullFile(Current)`, not `Patch`).
            DiffContent::FullFile(FullFileSource::Previous)
                if self.current_file_is_untracked_unstaged() =>
            {
                " [f]file"
            }
            DiffContent::FullFile(FullFileSource::Previous) => " [f]file [F]diff",
        });

        ops
    }

    pub fn inline_select_help_text(&self) -> String {
        "[j/k]move [Ctrl-U/D]jump [u]apply [v]back [h]tree [/]search [n/N]match [[]/[]]hunk [r]refresh [q]quit".to_string()
    }
}

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
