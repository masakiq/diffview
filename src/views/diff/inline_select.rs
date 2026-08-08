use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::HashSet;

use crate::app::{App, Focus};
use crate::domain::content::TreePane;

impl App {
    pub(crate) fn handle_inline_select_key(&mut self, key: KeyEvent) -> Result<()> {
        let line_count = self.diff.raw_line_count;
        let half_page = (self.diff.diff_pane_height / 2).max(1);

        match key.code {
            KeyCode::Char('q') => {
                self.should_quit = true;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if self.diff.diff_cursor + 1 < line_count {
                    self.diff.diff_cursor += 1;
                    self.sync_hunk_cursor();
                    crate::components::cursor::follow(
                        self.diff.diff_cursor,
                        &mut self.diff.diff_scroll,
                        self.diff.diff_pane_height,
                    );
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.diff.diff_cursor > 0 {
                    self.diff.diff_cursor -= 1;
                    self.sync_hunk_cursor();
                    crate::components::cursor::follow(
                        self.diff.diff_cursor,
                        &mut self.diff.diff_scroll,
                        self.diff.diff_pane_height,
                    );
                }
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.diff.diff_cursor =
                    (self.diff.diff_cursor + half_page).min(line_count.saturating_sub(1));
                self.sync_hunk_cursor();
                crate::components::cursor::follow(
                    self.diff.diff_cursor,
                    &mut self.diff.diff_scroll,
                    self.diff.diff_pane_height,
                );
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.diff.diff_cursor = self.diff.diff_cursor.saturating_sub(half_page);
                self.sync_hunk_cursor();
                crate::components::cursor::follow(
                    self.diff.diff_cursor,
                    &mut self.diff.diff_scroll,
                    self.diff.diff_pane_height,
                );
            }
            KeyCode::Char('u')
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.apply_current_line()?;
            }
            KeyCode::Char('/') => {
                self.begin_search();
            }
            KeyCode::Char('n') => self.navigate_search(true),
            KeyCode::Char('N') => self.navigate_search(false),
            KeyCode::Char(']') => self.jump_next_hunk(),
            KeyCode::Char('[') => self.jump_prev_hunk(),
            KeyCode::Char('v') => {
                self.focus = Focus::DiffView;
                // `diff_scroll` may have moved far from `patch_cursor` while InlineSelect
                // was scrolling independently — re-follow so the always-on patch cursor
                // doesn't return off-screen (review_8 Finding 3-B).
                self.follow_patch_cursor();
            }
            KeyCode::Char('h') | KeyCode::Left => {
                self.focus = self
                    .diff
                    .diff_origin
                    .map(|p| p.to_focus())
                    .unwrap_or(Focus::Unstaged);
            }
            _ => {}
        }
        Ok(())
    }

    fn apply_current_line(&mut self) -> Result<()> {
        if self.is_commit() {
            self.error_message = Some("Commit diff is read-only".to_string());
            return Ok(());
        }

        let info = match self.diff.line_infos.get(self.diff.diff_cursor) {
            Some(i) => i.clone(),
            None => return Ok(()),
        };

        if !info.is_selectable {
            self.error_message = Some("Only +/- lines can be applied".to_string());
            return Ok(());
        }

        let hunk_idx = match info.hunk_idx {
            Some(h) => h,
            None => return Ok(()),
        };
        let line_in_hunk = match info.line_in_hunk {
            Some(l) => l,
            None => return Ok(()),
        };

        let file = match &self.diff.current_file {
            Some(f) => f.clone(),
            None => return Ok(()),
        };
        let hunk = match self.diff.file_diff.hunks.get(hunk_idx).cloned() {
            Some(h) => h,
            None => return Ok(()),
        };
        let pane = match self.diff.diff_origin {
            Some(p) => p,
            None => return Ok(()),
        };

        let selected: HashSet<usize> = [line_in_hunk].into_iter().collect();

        let result = match pane {
            TreePane::Unstaged => {
                crate::infra::git::apply::stage_lines(&file, &hunk, &selected, &self.repo_root)
            }
            TreePane::Staged => {
                crate::infra::git::apply::unstage_lines(&file, &hunk, &selected, &self.repo_root)
            }
        };

        match result {
            Ok(_) => {
                let action = if pane.is_staged() {
                    "Unstaged"
                } else {
                    "Staged"
                };
                self.status_message = Some(format!("{} 1 line", action));
                self.clear_diff_cache();
                self.refresh_trees()?;

                let prev_cursor = self.diff.diff_cursor;
                self.reload_current_diff()?;

                if self.diff.file_diff.hunks.is_empty() && self.diff.raw_diff.trim().is_empty() {
                    self.clear_diff();
                    self.focus = pane.to_focus();
                } else {
                    self.move_to_next_selectable(prev_cursor);
                }
            }
            Err(e) => self.error_message = Some(format!("Error: {}", e)),
        }
        Ok(())
    }

    fn move_to_next_selectable(&mut self, from: usize) {
        let line_count = self.diff.line_infos.len();
        for i in from..line_count {
            if let Some(info) = self.diff.line_infos.get(i) {
                if info.is_selectable {
                    self.diff.diff_cursor = i;
                    self.ensure_cursor_visible();
                    return;
                }
            }
        }
        for i in (0..from).rev() {
            if let Some(info) = self.diff.line_infos.get(i) {
                if info.is_selectable {
                    self.diff.diff_cursor = i;
                    self.ensure_cursor_visible();
                    return;
                }
            }
        }
        self.diff.diff_cursor = from.min(line_count.saturating_sub(1));
    }

    pub(crate) fn ensure_cursor_visible(&mut self) {
        crate::components::cursor::follow(
            self.diff.diff_cursor,
            &mut self.diff.diff_scroll,
            self.diff.diff_pane_height,
        );
    }
}
