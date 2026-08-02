use crate::app::{App, DiffContent, FullFileSource};

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
