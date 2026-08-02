use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::Path;

use crate::app::{App, ExternalAction, Focus, TreePane, TREE_FAST_MOVE_LINES};

impl App {
    pub(super) fn handle_tree_key(&mut self, key: KeyEvent) -> Result<()> {
        if !matches!(
            key.code,
            KeyCode::Char('j') | KeyCode::Down | KeyCode::Char('k') | KeyCode::Up
        ) {
            self.pending_tree_preview = None;
        }

        if self.can_trigger_commit_action(key) {
            self.pending_action = Some(ExternalAction::Commit);
            return Ok(());
        }

        match key.code {
            KeyCode::Char('q') => {
                self.should_quit = true;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.tree_move_down();
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.tree_move_up();
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.tree_move_half_page_down();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.tree_move_half_page_up();
            }
            KeyCode::Char('l') | KeyCode::Right => {
                self.tree_action_right()?;
            }
            KeyCode::Char('h') | KeyCode::Left => {
                self.tree_action_left();
            }
            KeyCode::Char('u')
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.tree_stage_or_unstage()?;
            }
            KeyCode::Enter if self.is_commit() => {
                self.tree_action_right()?;
            }
            KeyCode::Char('c') => {
                self.tree_copy_path_to_clipboard();
            }
            KeyCode::Char('/') => {
                self.begin_search();
            }
            KeyCode::Char('n') => {
                self.navigate_search(true);
            }
            KeyCode::Char('N') => {
                self.navigate_search(false);
            }
            KeyCode::Char('?') => {
                self.status_message = Some(self.tree_help_text());
            }
            _ => {}
        }
        Ok(())
    }

    fn tree_move_down(&mut self) {
        let moved = self.tree_step_down();
        self.finish_tree_move(moved);
    }

    fn tree_move_up(&mut self) {
        let moved = self.tree_step_up();
        self.finish_tree_move(moved);
    }

    fn tree_move_half_page_down(&mut self) {
        let mut moved = false;
        for _ in 0..TREE_FAST_MOVE_LINES {
            if !self.tree_step_down() {
                break;
            }
            moved = true;
        }
        self.finish_tree_move(moved);
    }

    fn tree_move_half_page_up(&mut self) {
        let mut moved = false;
        for _ in 0..TREE_FAST_MOVE_LINES {
            if !self.tree_step_up() {
                break;
            }
            moved = true;
        }
        self.finish_tree_move(moved);
    }

    fn tree_step_down(&mut self) -> bool {
        let pane = match self.focused_pane() {
            Some(p) => p,
            None => return false,
        };

        let can_move = {
            let tree = self.tree(pane);
            !tree.is_empty() && tree.cursor + 1 < tree.visible.len()
        };

        if can_move {
            self.tree_mut(pane).cursor += 1;
            true
        } else if !self.is_commit() && pane == TreePane::Unstaged && !self.staged.is_empty() {
            self.focus = Focus::Staged;
            self.staged.cursor = 0;
            true
        } else {
            false
        }
    }

    fn tree_step_up(&mut self) -> bool {
        let pane = match self.focused_pane() {
            Some(p) => p,
            None => return false,
        };

        let can_move = {
            let tree = self.tree(pane);
            tree.cursor > 0
        };

        if can_move {
            self.tree_mut(pane).cursor -= 1;
            true
        } else if !self.is_commit() && pane == TreePane::Staged && !self.unstaged.is_empty() {
            self.focus = Focus::Unstaged;
            self.unstaged.cursor = self.unstaged.visible.len().saturating_sub(1);
            true
        } else {
            false
        }
    }

    fn finish_tree_move(&mut self, moved: bool) {
        if moved {
            if let Some(next_pane) = self.focused_pane() {
                self.schedule_tree_preview(next_pane);
            }
        }
    }

    /// l key: expand dir (and move cursor to first child) or open file diff
    pub(super) fn tree_action_right(&mut self) -> Result<()> {
        let pane = match self.focused_pane() {
            Some(p) => p,
            None => return Ok(()),
        };

        let (is_dir, path) = {
            let section = self.tree(pane);
            match section.current_node() {
                Some(n) => (n.is_dir, n.path.to_string_lossy().to_string()),
                None => return Ok(()),
            }
        };

        if is_dir {
            self.tree_mut(pane).expand_and_enter();
            self.tree_load_preview();
        } else {
            let view_mode = self.default_view_mode_for(pane, &path);
            self.load_diff(&path, pane, view_mode)?;
            self.focus = Focus::DiffView;
        }
        Ok(())
    }

    /// h key: fold the parent directory of the current node
    fn tree_action_left(&mut self) {
        let pane = match self.focused_pane() {
            Some(p) => p,
            None => return,
        };

        self.tree_mut(pane).fold_parent();
    }

    /// Stage or unstage the selected file or directory in the working tree target.
    fn tree_stage_or_unstage(&mut self) -> Result<()> {
        if self.is_commit() {
            return self.tree_action_right();
        }

        let pane = match self.focused_pane() {
            Some(p) => p,
            None => return Ok(()),
        };

        let (is_dir, path) = {
            let section = self.tree(pane);
            match section.current_node() {
                Some(n) => (n.is_dir, n.path.to_string_lossy().to_string()),
                None => return Ok(()),
            }
        };

        match pane {
            TreePane::Unstaged => {
                if is_dir {
                    let files = self.unstaged.files_under_dir(Path::new(&path));
                    for file in &files {
                        let _ = crate::infra::git::apply::stage_file(file, &self.repo_root);
                    }
                    self.status_message = Some(format!("Staged directory: {}", path));
                } else {
                    match crate::infra::git::apply::stage_file(&path, &self.repo_root) {
                        Ok(_) => self.status_message = Some(format!("Staged: {}", path)),
                        Err(e) => {
                            self.error_message = Some(format!("Error: {}", e));
                            return Ok(());
                        }
                    }
                }
            }
            TreePane::Staged => {
                if is_dir {
                    let files = self.staged.files_under_dir(Path::new(&path));
                    for file in &files {
                        let _ = crate::infra::git::apply::unstage_file(file, &self.repo_root);
                    }
                    self.status_message = Some(format!("Unstaged directory: {}", path));
                } else {
                    match crate::infra::git::apply::unstage_file(&path, &self.repo_root) {
                        Ok(_) => self.status_message = Some(format!("Unstaged: {}", path)),
                        Err(e) => {
                            self.error_message = Some(format!("Error: {}", e));
                            return Ok(());
                        }
                    }
                }
            }
        }

        self.refresh_after_tree_op()?;
        Ok(())
    }

    fn tree_copy_path_to_clipboard(&mut self) {
        let pane = match self.focused_pane() {
            Some(p) => p,
            None => return,
        };

        let path = {
            let section = self.tree(pane);
            match section.current_node() {
                Some(n) => n.path.to_string_lossy().to_string(),
                None => return,
            }
        };

        self.copy_path_to_clipboard(&path);
    }

    /// Load diff preview when cursor moves in tree
    pub(super) fn tree_load_preview(&mut self) {
        let pane = match self.focused_pane() {
            Some(p) => p,
            None => return,
        };
        self.tree_load_preview_for_pane(pane);
    }

    pub(super) fn tree_load_preview_for_pane(&mut self, pane: TreePane) {
        let (is_dir, path) = {
            let section = self.tree(pane);
            match section.current_node() {
                Some(n) => (n.is_dir, n.path.to_string_lossy().to_string()),
                None => {
                    self.clear_diff();
                    return;
                }
            }
        };

        if is_dir {
            return;
        }

        let view_mode = self.default_view_mode_for(pane, &path);
        let _ = self.load_diff(&path, pane, view_mode);
    }

    fn refresh_after_tree_op(&mut self) -> Result<()> {
        self.clear_diff_cache();
        self.pending_tree_preview = None;

        let prev_focus = self.focus.clone();
        self.refresh_trees()?;

        match prev_focus {
            Focus::Unstaged if self.unstaged.is_empty() && !self.staged.is_empty() => {
                self.focus = Focus::Staged;
            }
            Focus::Staged if self.staged.is_empty() && !self.unstaged.is_empty() => {
                self.focus = Focus::Unstaged;
            }
            _ => {}
        }

        self.tree_load_preview();
        Ok(())
    }
}
