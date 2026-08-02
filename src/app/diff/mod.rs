use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{is_plain_g, App, DiffContent, DiffTool, ExternalAction, Focus, FullFileSource};
use crate::clipboard;

mod inline_select;

impl App {
    fn diff_copy_path_to_clipboard(&mut self) {
        let path = match self.current_file.clone() {
            Some(path) => path,
            None => {
                self.error_message = Some("No file selected".to_string());
                return;
            }
        };

        self.copy_path_to_clipboard(&path);
    }

    pub(super) fn full_file_clipboard_text(&self) -> Option<&str> {
        (self.diff_content.is_full_file() && self.full_file_copyable && self.current_file.is_some())
            .then_some(self.raw_diff.as_str())
    }

    fn diff_copy_full_file_to_clipboard(&mut self) {
        let path = match self.current_file.clone() {
            Some(path) => path,
            None => {
                self.error_message = Some("No file selected".to_string());
                return;
            }
        };

        let Some(text) = self.full_file_clipboard_text() else {
            self.error_message = Some(
                "Whole-file copy is available only when full file contents are open".to_string(),
            );
            return;
        };

        match clipboard::copy_text(text) {
            Ok(_) => self.status_message = Some(format!("Copied file contents: {}", path)),
            Err(e) => self.error_message = Some(format!("Clipboard error: {}", e)),
        }
    }

    /// Whether the always-on full-file cursor applies right now: full-file view showing
    /// real, copyable content — not patch view, not an "unavailable" placeholder
    /// (binary/unmerged/missing file on the requested side), and not an empty file (no
    /// line for the cursor to sit on).
    ///
    /// Requires `Focus::DiffView`. This used to not check focus at all, on the
    /// assumption that full-file content always resets back to `Patch` before focus can
    /// leave the diff pane — true until an untracked/unstaged file started defaulting
    /// straight into full-file view for its tree preview (`default_view_mode_for`):
    /// that preview renders with `Focus::Unstaged`/`Focus::Staged` still active, so
    /// without this check the tree-preview pane showed an apparently-operable cursor
    /// that `j`/`k`/`v`/`y` (all gated on `Focus::DiffView` in `handle_diff_key`)
    /// couldn't actually move or act on — `j`/`k` moved the tree cursor instead
    /// (review_9 Finding 2).
    pub fn full_file_cursor_active(&self) -> bool {
        self.focus == Focus::DiffView
            && self.diff_content.is_full_file()
            && self.full_file_copyable
            && self.raw_line_count > 0
    }

    /// Whether the always-on patch-view cursor applies right now: `Focus::DiffView`
    /// showing patch content. The focus check also rules out `Focus::InlineSelect`, which
    /// renders its own cursor over `raw_diff` via `diff_cursor` instead — a different
    /// (raw-line) row space than `patch_cursor`'s display-row one, so the two must never
    /// both be active together.
    ///
    /// Patch view routinely stays active while the tree pane merely previews it (same
    /// reason `full_file_cursor_active` now checks focus too, see its own comment), so
    /// the focus check here is load-bearing: without it, an unfocused preview would show
    /// a cursor the user never navigated to.
    pub fn patch_cursor_active(&self) -> bool {
        self.focus == Focus::DiffView
            && self.diff_content == DiffContent::Patch
            && self.display_line_count > 0
    }

    /// Whether full-file search highlighting must skip past a leading gutter (line number,
    /// `│` separator) instead of highlighting from column 0. Only true when `bat` actually
    /// rendered its forced style (`full_file_content_offset > 0`, see `FULL_FILE_VIEW_BAT_STYLE`
    /// in `git/diff.rs`) — on the `cat`/plain-text fallback (`content_offset == 0`) the displayed
    /// rows are the raw content verbatim with no gutter to skip, so the ordinary highlighter is
    /// correct there; the gutter-aware one would instead use each row's own length as its match
    /// floor (no `│` anywhere) and silently suppress every highlight.
    pub fn full_file_search_highlight_uses_gutter(&self) -> bool {
        self.full_file_cursor_active() && self.full_file_content_offset > 0
    }

    pub(super) fn toggle_full_file_view(&mut self, source: FullFileSource) -> Result<()> {
        self.pending_g = false;
        let (Some(path), Some(pane)) = (self.current_file.clone(), self.diff_origin) else {
            return Ok(());
        };

        let next_mode = self.diff_content.toggle_full_file(source);
        // An untracked/unstaged file has no patch view of its own to fall back to (see
        // `current_file_is_untracked_unstaged`) — it's `get_file_preview`'s bat rendering
        // either way, just without the range-select cursor. Block only the `Patch`
        // landing specifically: `f`/`F` still need to work for moving *between*
        // `FullFile(Current)`/`FullFile(Previous)` (the latter always resolves to the
        // unavailable-previous-side message for such a file, which is itself useful to
        // see). Checked here rather than by guarding the `f`/`F` key match arms so every
        // route to `Patch` — including a second `F` press while already on `Previous` —
        // is covered by the same single check.
        if next_mode == DiffContent::Patch && self.current_file_is_untracked_unstaged() {
            return Ok(());
        }
        let entering_full_file_from_patch =
            self.diff_content == DiffContent::Patch && next_mode.is_full_file();
        let target_line = entering_full_file_from_patch
            .then(|| {
                self.patch_top_line_target(source)
                    .or_else(|| self.untracked_patch_line_target(&path, pane))
            })
            .flatten();
        // The patch cursor's row *on screen* (relative to the patch pane's own viewport,
        // not its absolute row in `display_diff`) — full-file view's own viewport is set up
        // to reproduce this same on-screen offset below, so the switch doesn't itself
        // relocate the cursor to the top (or bottom) of the pane. Clamped against the
        // current pane height: `follow_patch_cursor` normally keeps `patch_cursor` inside
        // the viewport already, but a terminal resize can shrink `diff_pane_height` without
        // re-clamping it, and an unclamped value here would make `follow_full_file_cursor`
        // below bottom-align the cursor instead of leaving this positioning alone.
        let patch_screen_row = entering_full_file_from_patch.then(|| {
            self.patch_cursor
                .saturating_sub(self.diff_scroll)
                .min(self.diff_pane_height.saturating_sub(1))
        });
        // Switching between FullFile(Current) and FullFile(Previous) (not going through
        // patch view) keeps the same scroll row and cursor line, since both sides render
        // with the same content offset and are almost always line-aligned for a small diff.
        let was_full_file = self.diff_content.is_full_file() && next_mode.is_full_file();
        let preserved_full_file_scroll = was_full_file.then_some(self.diff_scroll);
        let preserved_full_file_cursor = was_full_file.then_some(self.full_file_cursor);

        self.load_diff(&path, pane, next_mode)?;

        if let Some(file_line) = target_line {
            self.full_file_cursor = file_line
                .saturating_sub(1)
                .min(self.raw_line_count.saturating_sub(1));
            // Reproduce the patch cursor's on-screen row: place the viewport so the mapped
            // line sits at the same offset from the top it had in patch view. Clamped to 0
            // when there isn't enough content above the target line to fill that offset
            // (e.g. the target is near the start of the file) — the closest achievable
            // result, since the viewport can't scroll to a negative row.
            let display_row = self.full_file_content_offset + self.full_file_cursor;
            self.diff_scroll = display_row
                .saturating_sub(patch_screen_row.unwrap_or(0))
                .min(self.display_line_count.saturating_sub(1));
        } else if was_full_file && !self.full_file_cursor_active() {
            // Switching between full-file sides but landing on an unavailable placeholder
            // (binary/unmerged/missing/empty — `full_file_cursor_active()` is false here
            // for exactly that reason). The placeholder's own tiny line count is
            // meaningless for the outgoing side's position, so restore it unclamped rather
            // than through the `raw_line_count`/`display_line_count` clamp below — that
            // clamp would otherwise crush a deep (cursor, scroll) down to (0, 0) on the
            // placeholder, discarding it before a later switch back to an available side
            // could ever restore it (review_11 Finding 2). `follow_full_file_cursor` below
            // is also skipped for the same `full_file_cursor_active()` reason, so nothing
            // re-clamps this against the placeholder's viewport either.
            self.diff_scroll = preserved_full_file_scroll.unwrap_or(0);
            self.full_file_cursor = preserved_full_file_cursor.unwrap_or(0);
        } else if let Some(scroll) = preserved_full_file_scroll {
            self.diff_scroll = scroll.min(self.display_line_count.saturating_sub(1));
            self.full_file_cursor = preserved_full_file_cursor
                .unwrap_or(0)
                .min(self.raw_line_count.saturating_sub(1));
        } else if next_mode.is_full_file() {
            self.full_file_cursor = 0;
        }
        if self.full_file_cursor_active() {
            // The scroll and cursor above are clamped independently against the new
            // file's (possibly much shorter) line counts, which can leave the cursor's
            // display row outside the clamped scroll window — pull the viewport back to
            // the cursor so it's never left off-screen after the switch. Skipped when
            // there's no real cursor to keep visible (binary/unmerged/missing/empty),
            // so switching into an unavailable placeholder doesn't yank the scroll to a
            // stale cursor position.
            self.follow_full_file_cursor();
        }
        // A range never survives a side switch, or a drop back into patch view.
        self.full_file_anchor = None;

        self.status_message = Some(match next_mode {
            DiffContent::Patch => "Patch view".to_string(),
            DiffContent::FullFile(source) => source.status_message().to_string(),
        });
        Ok(())
    }

    pub(super) fn leave_diff_view_to_tree(&mut self) -> Result<()> {
        self.pending_g = false;
        let target_focus = self
            .diff_origin
            .map(|p| p.to_focus())
            .unwrap_or(Focus::Unstaged);

        if self.diff_content.is_full_file() {
            if let (Some(path), Some(pane)) = (self.current_file.clone(), self.diff_origin) {
                // An untracked file's tree-preview content is full-file view itself (see
                // `default_view_mode_for`), so falling back to `Patch` here would
                // needlessly reload it into the same bat-rendered content under a
                // different label — go to whichever content the tree would open it in.
                let tree_preview_mode = self.default_view_mode_for(pane, &path);
                self.load_diff(&path, pane, tree_preview_mode)?;
            } else {
                self.diff_content = DiffContent::Patch;
                self.content_annotation = None;
            }
        }

        self.focus = target_focus;
        Ok(())
    }

    pub(super) fn handle_diff_key(&mut self, key: KeyEvent) -> Result<()> {
        let line_count = self.display_line_count;
        let half_page = (self.diff_pane_height / 2).max(1);
        let is_full_file_view = self.diff_content.is_full_file();

        if !is_plain_g(key) {
            self.pending_g = false;
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
                if self.full_file_cursor_active() {
                    if self.full_file_cursor + 1 < self.raw_line_count {
                        self.full_file_cursor += 1;
                        self.follow_full_file_cursor();
                    }
                } else if self.patch_cursor_active() {
                    if self.patch_cursor + 1 < line_count {
                        self.patch_cursor += 1;
                        self.follow_patch_cursor();
                        self.sync_hunk_cursor_from_patch_cursor();
                    }
                } else if self.diff_scroll + 1 < line_count {
                    self.diff_scroll += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.full_file_cursor_active() {
                    if self.full_file_cursor > 0 {
                        self.full_file_cursor -= 1;
                        self.follow_full_file_cursor();
                    }
                } else if self.patch_cursor_active() {
                    if self.patch_cursor > 0 {
                        self.patch_cursor -= 1;
                        self.follow_patch_cursor();
                        self.sync_hunk_cursor_from_patch_cursor();
                    }
                } else if self.diff_scroll > 0 {
                    self.diff_scroll -= 1;
                }
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.full_file_cursor_active() {
                    self.full_file_cursor = (self.full_file_cursor + half_page)
                        .min(self.raw_line_count.saturating_sub(1));
                    self.follow_full_file_cursor();
                } else if self.patch_cursor_active() {
                    self.patch_cursor =
                        (self.patch_cursor + half_page).min(line_count.saturating_sub(1));
                    self.follow_patch_cursor();
                    self.sync_hunk_cursor_from_patch_cursor();
                } else {
                    self.diff_scroll =
                        (self.diff_scroll + half_page).min(line_count.saturating_sub(1));
                }
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.full_file_cursor_active() {
                    self.full_file_cursor = self.full_file_cursor.saturating_sub(half_page);
                    self.follow_full_file_cursor();
                } else if self.patch_cursor_active() {
                    self.patch_cursor = self.patch_cursor.saturating_sub(half_page);
                    self.follow_patch_cursor();
                    self.sync_hunk_cursor_from_patch_cursor();
                } else {
                    self.diff_scroll = self.diff_scroll.saturating_sub(half_page);
                }
            }
            KeyCode::Char('g') if is_plain_g(key) => {
                if self.pending_g {
                    self.pending_g = false;
                    if self.full_file_cursor_active() {
                        self.full_file_cursor = 0;
                        self.follow_full_file_cursor();
                    } else if self.patch_cursor_active() {
                        self.patch_cursor = 0;
                        self.follow_patch_cursor();
                        self.sync_hunk_cursor_from_patch_cursor();
                    } else {
                        self.diff_scroll = 0;
                    }
                } else {
                    self.pending_g = true;
                }
            }
            KeyCode::Char('G') => {
                if self.full_file_cursor_active() {
                    self.full_file_cursor = self.raw_line_count.saturating_sub(1);
                    self.follow_full_file_cursor();
                } else if self.patch_cursor_active() {
                    self.patch_cursor = line_count.saturating_sub(1);
                    self.follow_patch_cursor();
                    self.sync_hunk_cursor_from_patch_cursor();
                } else {
                    self.diff_scroll = line_count.saturating_sub(1);
                }
            }
            KeyCode::Char('/') => {
                self.begin_search();
            }
            KeyCode::Char('n') => {
                self.navigate_search(true);
            }
            KeyCode::Char('N') => self.navigate_search(false),
            KeyCode::Char(']') if !is_full_file_view => self.jump_next_hunk(),
            KeyCode::Char('[') if !is_full_file_view => self.jump_prev_hunk(),
            KeyCode::Char('c') => {
                self.diff_copy_path_to_clipboard();
            }
            KeyCode::Char('P') => {
                self.diff_copy_full_file_to_clipboard();
            }
            KeyCode::Char('f') => {
                self.toggle_full_file_view(FullFileSource::Current)?;
            }
            KeyCode::Char('F') => {
                self.toggle_full_file_view(FullFileSource::Previous)?;
            }
            KeyCode::Char('h') | KeyCode::Left => {
                self.full_file_anchor = None;
                self.leave_diff_view_to_tree()?;
            }
            KeyCode::Char('v') => {
                if self.full_file_cursor_active() {
                    self.full_file_anchor = match self.full_file_anchor {
                        Some(_) => None,
                        None => Some(self.full_file_cursor),
                    };
                } else if is_full_file_view {
                    self.error_message =
                        Some("Line selection unavailable in full file view".to_string());
                } else if self.is_commit() {
                    self.error_message = Some("Commit diff is read-only".to_string());
                } else if self.tool.supports_line_ops() {
                    if self.file_diff.hunks.is_empty() {
                        self.error_message = Some("No hunks to select lines from".to_string());
                    } else {
                        self.focus = Focus::InlineSelect;
                        // `patch_cursor` is a display-row index; InlineSelect's `diff_cursor`
                        // indexes `raw_diff` instead (it always renders raw content,
                        // regardless of tool). For `--tool raw` those two row spaces are
                        // identical (display_diff is raw_diff verbatim), so starting from
                        // the patch cursor is a strict improvement over the viewport top.
                        // For delta, display rows and raw rows diverge (side-by-side pairs
                        // two raw lines into one row), so `patch_cursor` isn't a valid raw
                        // index there — keep today's `diff_scroll`-based start instead of
                        // landing on an unrelated raw line.
                        self.diff_cursor = if self.tool == DiffTool::Raw {
                            self.patch_cursor
                        } else {
                            self.diff_scroll
                        };
                        self.sync_hunk_cursor();
                        self.status_message =
                            Some("Inline select: j/k move  u apply  v/h exit".to_string());
                    }
                } else {
                    self.error_message =
                        Some("Line selection unavailable with difftastic".to_string());
                }
            }
            KeyCode::Char('y') if is_full_file_view => {
                if self.full_file_cursor_active() {
                    self.copy_full_file_selection();
                } else {
                    self.error_message = Some("No content to copy".to_string());
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn jump_next_hunk(&mut self) {
        let count = self.file_diff.hunks.len();
        if count == 0 {
            return;
        }
        if self.hunk_cursor + 1 < count {
            self.hunk_cursor += 1;
        }
        self.scroll_to_hunk(self.hunk_cursor);
    }

    fn jump_prev_hunk(&mut self) {
        if self.file_diff.hunks.is_empty() {
            return;
        }
        if self.hunk_cursor > 0 {
            self.hunk_cursor -= 1;
        }
        self.scroll_to_hunk(self.hunk_cursor);
    }

    fn scroll_to_hunk(&mut self, hunk_idx: usize) {
        let mut hunk_count = 0usize;
        let content = if self.focus == Focus::InlineSelect {
            &self.raw_diff
        } else {
            &self.display_diff
        };
        for (line_no, line) in content.lines().enumerate() {
            if line.starts_with("@@") {
                if hunk_count == hunk_idx {
                    if self.focus == Focus::InlineSelect {
                        self.diff_cursor = line_no;
                        self.diff_scroll = line_no;
                    } else {
                        // Only ever reached from Patch-content DiffView (full-file view
                        // guards `]`/`[` out entirely) — keep the always-on patch cursor
                        // in sync with the jump, not just the viewport.
                        self.diff_scroll = line_no;
                        self.patch_cursor = line_no;
                    }
                    return;
                }
                hunk_count += 1;
            }
        }
    }

    /// Keeps the cursor's display row within the visible viewport, mirroring
    /// `handle_inline_select_key`'s own j/k viewport-follow logic.
    pub(super) fn follow_full_file_cursor(&mut self) {
        let display_row = self.full_file_content_offset + self.full_file_cursor;
        crate::components::cursor::follow(
            display_row,
            &mut self.diff_scroll,
            self.diff_pane_height,
        );
    }

    /// Same viewport-follow as `follow_full_file_cursor`, for the patch-view cursor —
    /// `patch_cursor` is already a display row itself, with no content-offset to add.
    pub(super) fn follow_patch_cursor(&mut self) {
        crate::components::cursor::follow(
            self.patch_cursor,
            &mut self.diff_scroll,
            self.diff_pane_height,
        );
    }

    /// Re-clamps whichever always-on cursor is active into the current viewport. Called
    /// whenever `diff_pane_height` changes (a terminal resize): neither `full_file_cursor`
    /// nor `patch_cursor` is otherwise re-validated against a shrunk pane height until the
    /// next cursor-moving key press, so a cursor left near the bottom of a tall pane can
    /// silently render off-screen right after the resize.
    pub(super) fn follow_active_diff_cursor(&mut self) {
        if self.full_file_cursor_active() {
            self.follow_full_file_cursor();
        } else if self.patch_cursor_active() {
            self.follow_patch_cursor();
        }
    }

    /// Raw lines covered by the active selection (or just the cursor's own line when
    /// no range is active), joined and always ending in a trailing newline — unlike `P`'s
    /// whole-file copy, which copies `raw_diff` verbatim and so omits the trailing newline
    /// for a file that doesn't have one.
    pub(super) fn full_file_selection_text(&self) -> String {
        let (lo, hi) = self.full_file_selection_range();
        let mut text = self
            .raw_diff
            .lines()
            .skip(lo)
            .take(hi - lo + 1)
            .collect::<Vec<_>>()
            .join("\n");
        text.push('\n');
        text
    }

    fn full_file_selection_range(&self) -> (usize, usize) {
        let cursor = self.full_file_cursor;
        match self.full_file_anchor {
            Some(anchor) => (anchor.min(cursor), anchor.max(cursor)),
            None => (cursor, cursor),
        }
    }

    fn copy_full_file_selection(&mut self) {
        let (lo, hi) = self.full_file_selection_range();
        let text = self.full_file_selection_text();

        match clipboard::copy_text(&text) {
            Ok(_) => {
                self.status_message = Some(format!("Copied {} line(s)", hi - lo + 1));
            }
            Err(e) => self.error_message = Some(format!("Clipboard error: {}", e)),
        }
    }

    pub(super) fn sync_hunk_cursor(&mut self) {
        if let Some(info) = self.line_infos.get(self.diff_cursor) {
            if let Some(new_hunk) = info.hunk_idx {
                self.hunk_cursor = new_hunk;
            }
        }
    }

    /// Same realignment as `sync_hunk_cursor`, but for `patch_cursor` instead of
    /// `diff_cursor` — keeps the hunk title and `]`/`[` jump target aligned with whichever
    /// hunk the always-on patch cursor actually sits inside, for every path that moves or
    /// restores it (plain `j`/`k`/half-page/`gg`/`G`, search, and the scroll/cursor restore
    /// on entering patch view). Only valid under `--tool raw`, where `patch_cursor`'s
    /// display-row space and `line_infos`' raw-line space coincide (same precondition the
    /// `'v'` key's `patch_cursor`-as-`diff_cursor` handoff already relies on) — a no-op for
    /// delta/difftastic, which have no equivalent display-row-to-hunk mapping today, so
    /// `hunk_cursor` there keeps its prior (still only approximately accurate) value.
    pub(super) fn sync_hunk_cursor_from_patch_cursor(&mut self) {
        if self.tool != DiffTool::Raw {
            return;
        }
        if self.file_diff.hunks.is_empty() {
            return;
        }
        if let Some(info) = self.line_infos.get(self.patch_cursor) {
            if let Some(new_hunk) = info.hunk_idx {
                self.hunk_cursor = new_hunk;
                return;
            }
        }
        // `patch_cursor` sits on a row with no hunk of its own (pre-first-hunk metadata,
        // `gg`'s row 0, a hunk header line, or past the end of `line_infos`) — leaving
        // `hunk_cursor` at its previous value would make `]`/`[` and the hunk title
        // disagree with where a cursor move from here actually goes. Realign to the
        // nearest hunk at or after the cursor, matching the direction `]` would jump;
        // fall back to the last hunk if the cursor is past every hunk's metadata.
        let next_hunk = self
            .line_infos
            .get(self.patch_cursor..)
            .and_then(|rest| rest.iter().find_map(|info| info.hunk_idx));
        self.hunk_cursor = next_hunk.unwrap_or(self.file_diff.hunks.len() - 1);
    }
}
