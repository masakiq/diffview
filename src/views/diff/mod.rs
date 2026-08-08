use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::app::{is_plain_g, App, DiffContent, DiffTool, ExternalAction, Focus};
use crate::clipboard;
use crate::components::highlight::{highlight_full_file_text, highlight_text, SEARCH_HIGHLIGHT_BG};
use crate::domain::content::FullFileSource;

mod inline_select;

/// Background tint for full-file view's added/removed line highlight, matching delta's own
/// default `plus-color`/`minus-color` so patch view (under `--tool delta`) and full-file view
/// read as the same diff. Dark and desaturated so it tints bat's syntax-highlighted
/// foreground rather than replacing it.
const FULL_FILE_ADDED_BG: Color = Color::Rgb(0, 40, 0);
const FULL_FILE_REMOVED_BG: Color = Color::Rgb(63, 0, 1);

/// Background for the full-file line-select cursor/range, matching the existing
/// `InlineSelect` cursor's `DarkGray` convention (`build_raw_diff_text`, below).
const FULL_FILE_SELECT_BG: Color = Color::DarkGray;

impl App {
    fn diff_copy_path_to_clipboard(&mut self) {
        let path = match self.diff.current_file.clone() {
            Some(path) => path,
            None => {
                self.error_message = Some("No file selected".to_string());
                return;
            }
        };

        self.copy_path_to_clipboard(&path);
    }

    pub(crate) fn full_file_clipboard_text(&self) -> Option<&str> {
        (self.diff.diff_content.is_full_file()
            && self.diff.full_file_copyable
            && self.diff.current_file.is_some())
        .then_some(self.diff.raw_diff.as_str())
    }

    fn diff_copy_full_file_to_clipboard(&mut self) {
        let path = match self.diff.current_file.clone() {
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
    /// straight into full-file view for its tree preview (`default_diff_content_for`):
    /// that preview renders with `Focus::Unstaged`/`Focus::Staged` still active, so
    /// without this check the tree-preview pane showed an apparently-operable cursor
    /// that `j`/`k`/`v`/`y` (all gated on `Focus::DiffView` in `handle_diff_key`)
    /// couldn't actually move or act on — `j`/`k` moved the tree cursor instead
    /// (review_9 Finding 2).
    pub fn full_file_cursor_active(&self) -> bool {
        self.focus == Focus::DiffView
            && self.diff.diff_content.is_full_file()
            && self.diff.full_file_copyable
            && self.diff.raw_line_count > 0
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
            && self.diff.diff_content == DiffContent::Patch
            && self.diff.display_line_count > 0
    }

    /// Whether full-file search highlighting must skip past a leading gutter (line number,
    /// `│` separator) instead of highlighting from column 0. Only true when `bat` actually
    /// rendered its forced style (`full_file_content_offset > 0`, see `FULL_FILE_VIEW_BAT_STYLE`
    /// in `git/diff.rs`) — on the `cat`/plain-text fallback (`content_offset == 0`) the displayed
    /// rows are the raw content verbatim with no gutter to skip, so the ordinary highlighter is
    /// correct there; the gutter-aware one would instead use each row's own length as its match
    /// floor (no `│` anywhere) and silently suppress every highlight.
    pub fn full_file_search_highlight_uses_gutter(&self) -> bool {
        self.full_file_cursor_active() && self.diff.full_file_content_offset > 0
    }

    pub(crate) fn toggle_full_file_view(&mut self, source: FullFileSource) -> Result<()> {
        self.diff.pending_g = false;
        let (Some(path), Some(pane)) = (self.diff.current_file.clone(), self.diff.diff_origin)
        else {
            return Ok(());
        };

        let next_content = self.diff.diff_content.toggle_full_file(source);
        // An untracked/unstaged file has no patch view of its own to fall back to (see
        // `current_file_is_untracked_unstaged`) — it's `get_file_preview`'s bat rendering
        // either way, just without the range-select cursor. Block only the `Patch`
        // landing specifically: `f`/`F` still need to work for moving *between*
        // `FullFile(Current)`/`FullFile(Previous)` (the latter always resolves to the
        // unavailable-previous-side message for such a file, which is itself useful to
        // see). Checked here rather than by guarding the `f`/`F` key match arms so every
        // route to `Patch` — including a second `F` press while already on `Previous` —
        // is covered by the same single check.
        if next_content == DiffContent::Patch && self.current_file_is_untracked_unstaged() {
            return Ok(());
        }
        let entering_full_file_from_patch =
            self.diff.diff_content == DiffContent::Patch && next_content.is_full_file();
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
            self.diff
                .patch_cursor
                .saturating_sub(self.diff.diff_scroll)
                .min(self.diff.diff_pane_height.saturating_sub(1))
        });
        // Switching between FullFile(Current) and FullFile(Previous) (not going through
        // patch view) keeps the same scroll row and cursor line, since both sides render
        // with the same content offset and are almost always line-aligned for a small diff.
        let was_full_file = self.diff.diff_content.is_full_file() && next_content.is_full_file();
        let preserved_full_file_scroll = was_full_file.then_some(self.diff.diff_scroll);
        let preserved_full_file_cursor = was_full_file.then_some(self.diff.full_file_cursor);

        self.load_diff(&path, pane, next_content)?;

        if let Some(file_line) = target_line {
            self.diff.full_file_cursor = file_line
                .saturating_sub(1)
                .min(self.diff.raw_line_count.saturating_sub(1));
            // Reproduce the patch cursor's on-screen row: place the viewport so the mapped
            // line sits at the same offset from the top it had in patch view. Clamped to 0
            // when there isn't enough content above the target line to fill that offset
            // (e.g. the target is near the start of the file) — the closest achievable
            // result, since the viewport can't scroll to a negative row.
            let display_row = self.diff.full_file_content_offset + self.diff.full_file_cursor;
            self.diff.diff_scroll = display_row
                .saturating_sub(patch_screen_row.unwrap_or(0))
                .min(self.diff.display_line_count.saturating_sub(1));
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
            self.diff.diff_scroll = preserved_full_file_scroll.unwrap_or(0);
            self.diff.full_file_cursor = preserved_full_file_cursor.unwrap_or(0);
        } else if let Some(scroll) = preserved_full_file_scroll {
            self.diff.diff_scroll = scroll.min(self.diff.display_line_count.saturating_sub(1));
            self.diff.full_file_cursor = preserved_full_file_cursor
                .unwrap_or(0)
                .min(self.diff.raw_line_count.saturating_sub(1));
        } else if next_content.is_full_file() {
            self.diff.full_file_cursor = 0;
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
        self.diff.full_file_anchor = None;

        self.status_message = Some(match next_content {
            DiffContent::Patch => "Patch view".to_string(),
            DiffContent::FullFile(source) => source.status_message().to_string(),
        });
        Ok(())
    }

    pub(crate) fn leave_diff_view_to_tree(&mut self) -> Result<()> {
        self.diff.pending_g = false;
        let target_focus = self
            .diff
            .diff_origin
            .map(|p| p.to_focus())
            .unwrap_or(Focus::Unstaged);

        if self.diff.diff_content.is_full_file() {
            if let (Some(path), Some(pane)) =
                (self.diff.current_file.clone(), self.diff.diff_origin)
            {
                // An untracked file's tree-preview content is full-file view itself (see
                // `default_diff_content_for`), so falling back to `Patch` here would
                // needlessly reload it into the same bat-rendered content under a
                // different label — go to whichever content the tree would open it in.
                let tree_preview_content = self.default_diff_content_for(pane, &path);
                self.load_diff(&path, pane, tree_preview_content)?;
            } else {
                self.diff.diff_content = DiffContent::Patch;
                self.diff.content_annotation = None;
            }
        }

        self.focus = target_focus;
        Ok(())
    }

    pub(crate) fn handle_diff_key(&mut self, key: KeyEvent) -> Result<()> {
        let line_count = self.diff.display_line_count;
        let half_page = (self.diff.diff_pane_height / 2).max(1);
        let is_full_file_view = self.diff.diff_content.is_full_file();

        if !is_plain_g(key) {
            self.diff.pending_g = false;
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
                    if self.diff.full_file_cursor + 1 < self.diff.raw_line_count {
                        self.diff.full_file_cursor += 1;
                        self.follow_full_file_cursor();
                    }
                } else if self.patch_cursor_active() {
                    if self.diff.patch_cursor + 1 < line_count {
                        self.diff.patch_cursor += 1;
                        self.follow_patch_cursor();
                        self.sync_hunk_cursor_from_patch_cursor();
                    }
                } else if self.diff.diff_scroll + 1 < line_count {
                    self.diff.diff_scroll += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.full_file_cursor_active() {
                    if self.diff.full_file_cursor > 0 {
                        self.diff.full_file_cursor -= 1;
                        self.follow_full_file_cursor();
                    }
                } else if self.patch_cursor_active() {
                    if self.diff.patch_cursor > 0 {
                        self.diff.patch_cursor -= 1;
                        self.follow_patch_cursor();
                        self.sync_hunk_cursor_from_patch_cursor();
                    }
                } else if self.diff.diff_scroll > 0 {
                    self.diff.diff_scroll -= 1;
                }
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.full_file_cursor_active() {
                    self.diff.full_file_cursor = (self.diff.full_file_cursor + half_page)
                        .min(self.diff.raw_line_count.saturating_sub(1));
                    self.follow_full_file_cursor();
                } else if self.patch_cursor_active() {
                    self.diff.patch_cursor =
                        (self.diff.patch_cursor + half_page).min(line_count.saturating_sub(1));
                    self.follow_patch_cursor();
                    self.sync_hunk_cursor_from_patch_cursor();
                } else {
                    self.diff.diff_scroll =
                        (self.diff.diff_scroll + half_page).min(line_count.saturating_sub(1));
                }
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.full_file_cursor_active() {
                    self.diff.full_file_cursor =
                        self.diff.full_file_cursor.saturating_sub(half_page);
                    self.follow_full_file_cursor();
                } else if self.patch_cursor_active() {
                    self.diff.patch_cursor = self.diff.patch_cursor.saturating_sub(half_page);
                    self.follow_patch_cursor();
                    self.sync_hunk_cursor_from_patch_cursor();
                } else {
                    self.diff.diff_scroll = self.diff.diff_scroll.saturating_sub(half_page);
                }
            }
            KeyCode::Char('g') if is_plain_g(key) => {
                if self.diff.pending_g {
                    self.diff.pending_g = false;
                    if self.full_file_cursor_active() {
                        self.diff.full_file_cursor = 0;
                        self.follow_full_file_cursor();
                    } else if self.patch_cursor_active() {
                        self.diff.patch_cursor = 0;
                        self.follow_patch_cursor();
                        self.sync_hunk_cursor_from_patch_cursor();
                    } else {
                        self.diff.diff_scroll = 0;
                    }
                } else {
                    self.diff.pending_g = true;
                }
            }
            KeyCode::Char('G') => {
                if self.full_file_cursor_active() {
                    self.diff.full_file_cursor = self.diff.raw_line_count.saturating_sub(1);
                    self.follow_full_file_cursor();
                } else if self.patch_cursor_active() {
                    self.diff.patch_cursor = line_count.saturating_sub(1);
                    self.follow_patch_cursor();
                    self.sync_hunk_cursor_from_patch_cursor();
                } else {
                    self.diff.diff_scroll = line_count.saturating_sub(1);
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
                self.diff.full_file_anchor = None;
                self.leave_diff_view_to_tree()?;
            }
            KeyCode::Char('v') => {
                if self.full_file_cursor_active() {
                    self.diff.full_file_anchor = match self.diff.full_file_anchor {
                        Some(_) => None,
                        None => Some(self.diff.full_file_cursor),
                    };
                } else if is_full_file_view {
                    self.error_message =
                        Some("Line selection unavailable in full file view".to_string());
                } else if self.is_commit() {
                    self.error_message = Some("Commit diff is read-only".to_string());
                } else if self.tool.supports_line_ops() {
                    if self.diff.file_diff.hunks.is_empty() {
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
                        self.diff.diff_cursor = if self.tool == DiffTool::Raw {
                            self.diff.patch_cursor
                        } else {
                            self.diff.diff_scroll
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
            KeyCode::Char('Y') if is_full_file_view => {
                if self.full_file_cursor_active() {
                    self.copy_full_file_selection_with_location();
                } else {
                    self.error_message = Some("No content to copy".to_string());
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn jump_next_hunk(&mut self) {
        let count = self.diff.file_diff.hunks.len();
        if count == 0 {
            return;
        }
        if self.diff.hunk_cursor + 1 < count {
            self.diff.hunk_cursor += 1;
        }
        self.scroll_to_hunk(self.diff.hunk_cursor);
    }

    fn jump_prev_hunk(&mut self) {
        if self.diff.file_diff.hunks.is_empty() {
            return;
        }
        if self.diff.hunk_cursor > 0 {
            self.diff.hunk_cursor -= 1;
        }
        self.scroll_to_hunk(self.diff.hunk_cursor);
    }

    fn scroll_to_hunk(&mut self, hunk_idx: usize) {
        let mut hunk_count = 0usize;
        let content = if self.focus == Focus::InlineSelect {
            &self.diff.raw_diff
        } else {
            &self.diff.display_diff
        };
        for (line_no, line) in content.lines().enumerate() {
            if line.starts_with("@@") {
                if hunk_count == hunk_idx {
                    if self.focus == Focus::InlineSelect {
                        self.diff.diff_cursor = line_no;
                        self.diff.diff_scroll = line_no;
                    } else {
                        // Only ever reached from Patch-content DiffView (full-file view
                        // guards `]`/`[` out entirely) — keep the always-on patch cursor
                        // in sync with the jump, not just the viewport.
                        self.diff.diff_scroll = line_no;
                        self.diff.patch_cursor = line_no;
                    }
                    return;
                }
                hunk_count += 1;
            }
        }
    }

    /// Keeps the cursor's display row within the visible viewport, mirroring
    /// `handle_inline_select_key`'s own j/k viewport-follow logic.
    pub(crate) fn follow_full_file_cursor(&mut self) {
        let display_row = self.diff.full_file_content_offset + self.diff.full_file_cursor;
        crate::components::cursor::follow(
            display_row,
            &mut self.diff.diff_scroll,
            self.diff.diff_pane_height,
        );
    }

    /// Same viewport-follow as `follow_full_file_cursor`, for the patch-view cursor —
    /// `patch_cursor` is already a display row itself, with no content-offset to add.
    pub(crate) fn follow_patch_cursor(&mut self) {
        crate::components::cursor::follow(
            self.diff.patch_cursor,
            &mut self.diff.diff_scroll,
            self.diff.diff_pane_height,
        );
    }

    /// Re-clamps whichever always-on cursor is active into the current viewport. Called
    /// whenever `diff_pane_height` changes (a terminal resize): neither `full_file_cursor`
    /// nor `patch_cursor` is otherwise re-validated against a shrunk pane height until the
    /// next cursor-moving key press, so a cursor left near the bottom of a tall pane can
    /// silently render off-screen right after the resize.
    pub(crate) fn follow_active_diff_cursor(&mut self) {
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
    pub(crate) fn full_file_selection_text(&self) -> String {
        let (lo, hi) = self.full_file_selection_range();
        let mut text = self
            .diff
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
        let cursor = self.diff.full_file_cursor;
        match self.diff.full_file_anchor {
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

    /// `full_file_selection_text()` prefixed with `path:line` (or `path:lo-hi` for a
    /// multi-line range), 1-indexed to match how editors/GitHub display line numbers —
    /// `full_file_cursor`/`full_file_anchor` are 0-indexed positions into `raw_diff`.
    /// `None` when no file is open, matching `full_file_clipboard_text`'s own guard.
    pub(crate) fn full_file_selection_location_text(&self) -> Option<String> {
        let path = self.diff.current_file.as_deref()?;
        let (lo, hi) = self.full_file_selection_range();
        let location = if lo == hi {
            format!("{}:{}", path, lo + 1)
        } else {
            format!("{}:{}-{}", path, lo + 1, hi + 1)
        };
        Some(format!(
            "{}\n\n{}",
            location,
            self.full_file_selection_text()
        ))
    }

    fn copy_full_file_selection_with_location(&mut self) {
        let (lo, hi) = self.full_file_selection_range();
        let Some(text) = self.full_file_selection_location_text() else {
            self.error_message = Some("No file selected".to_string());
            return;
        };

        match clipboard::copy_text(&text) {
            Ok(_) => {
                self.status_message = Some(format!("Copied {} line(s) with location", hi - lo + 1));
            }
            Err(e) => self.error_message = Some(format!("Clipboard error: {}", e)),
        }
    }

    pub(crate) fn sync_hunk_cursor(&mut self) {
        if let Some(info) = self.diff.line_infos.get(self.diff.diff_cursor) {
            if let Some(new_hunk) = info.hunk_idx {
                self.diff.hunk_cursor = new_hunk;
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
    pub(crate) fn sync_hunk_cursor_from_patch_cursor(&mut self) {
        if self.tool != DiffTool::Raw {
            return;
        }
        if self.diff.file_diff.hunks.is_empty() {
            return;
        }
        if let Some(info) = self.diff.line_infos.get(self.diff.patch_cursor) {
            if let Some(new_hunk) = info.hunk_idx {
                self.diff.hunk_cursor = new_hunk;
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
            .diff
            .line_infos
            .get(self.diff.patch_cursor..)
            .and_then(|rest| rest.iter().find_map(|info| info.hunk_idx));
        self.diff.hunk_cursor = next_hunk.unwrap_or(self.diff.file_diff.hunks.len() - 1);
    }
}

pub fn render(f: &mut Frame, app: &App, area: Rect) {
    let focused = matches!(app.focus, Focus::DiffView | Focus::InlineSelect);

    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let origin_label = match app.diff.diff_origin {
        Some(pane) => app.diff_origin_label(pane),
        None => String::new(),
    };

    let title = match &app.diff.current_file {
        Some(path) => {
            if matches!(app.diff.diff_content, DiffContent::FullFile(_)) {
                let content_label = app
                    .diff
                    .content_annotation
                    .map(|annotation| annotation.title_label())
                    .unwrap_or_else(|| app.diff.diff_content.label());
                format!(" {} [{}] [{}] ", path, origin_label, content_label)
            } else if app.diff.file_diff.is_binary {
                format!(" {} [{}][binary] ", path, origin_label)
            } else if !app.diff.file_diff.hunks.is_empty() {
                format!(
                    " {} [{}] [{}] (hunk {}/{}) ",
                    path,
                    origin_label,
                    app.diff.diff_content.label(),
                    app.diff.hunk_cursor + 1,
                    app.diff.file_diff.hunks.len()
                )
            } else {
                format!(
                    " {} [{}] [{}] ",
                    path,
                    origin_label,
                    app.diff.diff_content.label()
                )
            }
        }
        None => " Diff ".to_string(),
    };

    let inner = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(title);

    let inner_area = inner.inner(area);
    f.render_widget(inner, area);

    if app.diff.current_file.is_none() {
        let hint = Paragraph::new("Select a file and press 'l' to view its diff.")
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(hint, inner_area);
        return;
    }

    let (content, use_raw_renderer) = if app.focus == Focus::InlineSelect {
        (&app.diff.raw_diff, true)
    } else {
        match app.tool {
            DiffTool::Raw => (
                &app.diff.display_diff,
                app.diff.cached_display_text.is_none(),
            ),
            _ => (&app.diff.display_diff, false),
        }
    };

    let text = if use_raw_renderer {
        build_raw_diff_text(app, content)
    } else {
        app.diff
            .cached_display_text
            .clone()
            .unwrap_or_else(|| build_raw_diff_text(app, content))
    };
    // Search highlighting runs *before* the cursor/diff-tint overlays below, not after —
    // `apply_full_file_line_bg`/`apply_full_file_cursor`/`apply_patch_cursor` each pad their
    // tinted rows with trailing blank spans out to the pane width (`tint_line_bg`), and a
    // query search running afterward would scan those padding spans too, occasionally
    // matching decoration that isn't part of any real content (`n`/`N` can never navigate to
    // such a match, since it doesn't exist in `searchable_lines_for_scope`'s search text).
    // Highlighting first, then tinting, still shows a match on a tinted row — the tint call's
    // `Span::bg` only replaces the background, so a match's `search_highlight_style` modifier
    // (bold) survives being tinted over, even though its yellow background doesn't.
    //
    // Full-file search matches only real file content (App::searchable_lines_for_scope),
    // not bat's line-number gutter or border rows — the highlight must skip those too, or
    // a query that happens to also appear there would visually highlight a "match" `n`/`N`
    // can never navigate to. See `full_file_search_highlight_uses_gutter`'s doc comment for
    // why that gutter-skipping logic must not run on the cat/plain-text fallback.
    let text = if app.full_file_search_highlight_uses_gutter() {
        highlight_full_file_text(
            text,
            app.diff_search_query(),
            &app.diff.raw_diff,
            app.diff.full_file_content_offset,
        )
    } else {
        highlight_text(text, app.diff_search_query())
    };
    let text = apply_full_file_line_bg(text, app, inner_area.width);
    let text = apply_full_file_cursor(text, app, inner_area.width);
    let text = apply_patch_cursor(text, app, inner_area.width);
    let text = window_text_rows(text, app.diff.diff_scroll, inner_area.height as usize);
    let para = Paragraph::new(text);
    f.render_widget(para, inner_area);
}

/// Slices `text` down to just the rows a `visible_rows`-tall viewport starting at `start`
/// would show, in place of `Paragraph::scroll`. That widget's scroll offset is a `u16`,
/// which wraps for files beyond 65,535 lines — `App`'s own scroll/cursor state is `usize`
/// with no such limit, so rendering must not reintroduce one by casting down to render.
fn window_text_rows(text: Text<'_>, start: usize, visible_rows: usize) -> Text<'_> {
    let start = start.min(text.lines.len());
    let end = start.saturating_add(visible_rows).min(text.lines.len());
    let lines = text
        .lines
        .into_iter()
        .skip(start)
        .take(end - start)
        .collect();

    Text {
        alignment: text.alignment,
        style: text.style,
        lines,
    }
}

fn build_raw_diff_text<'a>(app: &App, content: &'a str) -> Text<'a> {
    let inline_select = app.focus == Focus::InlineSelect;

    let lines: Vec<Line<'a>> = content
        .lines()
        .enumerate()
        .map(|(display_idx, line)| {
            let base_style = diff_line_style(line);

            let style = if inline_select && display_idx == app.diff.diff_cursor {
                base_style.bg(Color::DarkGray).add_modifier(Modifier::BOLD)
            } else {
                base_style
            };

            Line::from(Span::styled(line.to_string(), style))
        })
        .collect();

    Text::from(lines)
}

/// Recolors every span's background to `bg` and pads with blank, `bg`-styled cells out
/// to `width`, so the tint reaches the right edge of the pane instead of stopping at
/// the end of the line's own text.
///
/// A span already carrying `SEARCH_HIGHLIGHT_BG` (a search match, applied earlier in
/// `render`'s pipeline) keeps its own background instead of being overwritten — without
/// this, every cursor/diff-tint overlay (added/removed tint, full-file cursor, patch
/// cursor) would silently erase a match's yellow highlight on any row it also covers,
/// leaving only its bold modifier — indistinguishable from the rest of that tinted row,
/// which is also bolded on the cursor's own line. Padding cells are synthetic (never part
/// of a match) and always get `bg` unconditionally.
fn tint_line_bg<'a>(spans: Vec<Span<'a>>, bg: Color, width: usize) -> Vec<Span<'a>> {
    let mut spans: Vec<Span<'a>> = spans
        .into_iter()
        .map(|span| {
            let style = if span.style.bg == Some(SEARCH_HIGHLIGHT_BG) {
                span.style
            } else {
                span.style.bg(bg)
            };
            Span {
                style,
                content: span.content,
            }
        })
        .collect();

    let pad = width.saturating_sub(spans.iter().map(Span::width).sum());
    if pad > 0 {
        spans.push(Span::styled(" ".repeat(pad), Style::default().bg(bg)));
    }
    spans
}

/// Overlays an added/removed background tint on full-file view rows that the currently
/// loaded diff marks as changed (`app.diff.full_file_highlight_lines`, 1-based file line numbers).
/// A no-op outside full-file view, or once no lines are marked (e.g. patch view, or an
/// unchanged file). `app.diff.full_file_content_offset` accounts for bat's leading decoration
/// rows, so row indices line up with file line numbers the same way scroll targeting does.
pub(crate) fn apply_full_file_line_bg<'a>(text: Text<'a>, app: &App, width: u16) -> Text<'a> {
    let bg = match app.diff.diff_content {
        DiffContent::FullFile(FullFileSource::Current) => FULL_FILE_ADDED_BG,
        DiffContent::FullFile(FullFileSource::Previous) => FULL_FILE_REMOVED_BG,
        DiffContent::Patch => return text,
    };
    if app.diff.full_file_highlight_lines.is_empty() {
        return text;
    }

    let offset = app.diff.full_file_content_offset;
    let width = width as usize;
    let lines = text
        .lines
        .into_iter()
        .enumerate()
        .map(|(row, line)| {
            let Some(file_line) = row.checked_sub(offset).map(|n| n as u32 + 1) else {
                return line;
            };
            if app
                .diff
                .full_file_highlight_lines
                .binary_search(&file_line)
                .is_err()
            {
                return line;
            }

            Line {
                style: line.style,
                alignment: line.alignment,
                spans: tint_line_bg(line.spans, bg, width),
            }
        })
        .collect();

    Text {
        alignment: text.alignment,
        style: text.style,
        lines,
    }
}

/// Overlays the always-on full-file cursor/range: every row within `[anchor, cursor]`
/// (or just the cursor's own row when no range is active) gets `FULL_FILE_SELECT_BG`,
/// with the exact cursor row additionally bolded to mark it within a multi-row range.
/// A no-op unless `app.full_file_cursor_active()` — i.e. outside full-file view, or
/// over an "unavailable" placeholder (binary/unmerged/missing) rather than real
/// content. Wins over the add/removed diff tint on overlapping rows, since it's
/// applied afterward.
pub(crate) fn apply_full_file_cursor<'a>(text: Text<'a>, app: &App, width: u16) -> Text<'a> {
    if !app.full_file_cursor_active() {
        return text;
    }

    let offset = app.diff.full_file_content_offset;
    let cursor = app.diff.full_file_cursor;
    let (lo, hi) = match app.diff.full_file_anchor {
        Some(anchor) => (anchor.min(cursor), anchor.max(cursor)),
        None => (cursor, cursor),
    };
    let width = width as usize;

    let lines = text
        .lines
        .into_iter()
        .enumerate()
        .map(|(row, line)| {
            let Some(file_idx) = row.checked_sub(offset) else {
                return line;
            };
            if file_idx < lo || file_idx > hi {
                return line;
            }

            let mut spans = tint_line_bg(line.spans, FULL_FILE_SELECT_BG, width);
            if file_idx == cursor {
                spans = spans
                    .into_iter()
                    .map(|span| Span {
                        style: span.style.add_modifier(Modifier::BOLD),
                        content: span.content,
                    })
                    .collect();
            }

            Line {
                style: line.style,
                alignment: line.alignment,
                spans,
            }
        })
        .collect();

    Text {
        alignment: text.alignment,
        style: text.style,
        lines,
    }
}

/// Overlays the always-on patch-view cursor: the single display row `app.diff.patch_cursor`
/// points at gets `FULL_FILE_SELECT_BG` and bold, the same style full-file view's own
/// cursor uses, so the two read as the same navigation primitive. Unlike
/// `apply_full_file_cursor`, there's no anchor/range — patch view's `v` key still means
/// "enter InlineSelect", not "extend a copy range". A no-op unless
/// `app.patch_cursor_active()` — i.e. outside patch view, or during `Focus::InlineSelect`,
/// which renders its own cursor over `raw_diff` in `build_raw_diff_text` instead.
pub(crate) fn apply_patch_cursor<'a>(text: Text<'a>, app: &App, width: u16) -> Text<'a> {
    if !app.patch_cursor_active() {
        return text;
    }

    let cursor = app.diff.patch_cursor;
    let width = width as usize;

    let lines = text
        .lines
        .into_iter()
        .enumerate()
        .map(|(row, line)| {
            if row != cursor {
                return line;
            }

            let spans = tint_line_bg(line.spans, FULL_FILE_SELECT_BG, width)
                .into_iter()
                .map(|span| Span {
                    style: span.style.add_modifier(Modifier::BOLD),
                    content: span.content,
                })
                .collect();

            Line {
                style: line.style,
                alignment: line.alignment,
                spans,
            }
        })
        .collect();

    Text {
        alignment: text.alignment,
        style: text.style,
        lines,
    }
}

fn diff_line_style(line: &str) -> Style {
    if line.starts_with('+') && !line.starts_with("+++") {
        Style::default().fg(Color::Green)
    } else if line.starts_with('-') && !line.starts_with("---") {
        Style::default().fg(Color::Red)
    } else if line.starts_with("@@") {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else if line.starts_with("diff ")
        || line.starts_with("--- ")
        || line.starts_with("+++ ")
        || line.starts_with("index ")
    {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tint_line_bg_preserves_a_search_matchs_yellow_background() {
        let spans = vec![
            Span::styled(
                "abc",
                Style::default()
                    .bg(SEARCH_HIGHLIGHT_BG)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("def"),
        ];

        let tinted = tint_line_bg(spans, Color::DarkGray, 10);

        assert_eq!(tinted[0].style.bg, Some(SEARCH_HIGHLIGHT_BG));
        assert_eq!(tinted[1].style.bg, Some(Color::DarkGray));
    }

    #[test]
    fn tint_line_bg_pads_with_the_tint_color_regardless_of_search_highlight() {
        // Padding cells are synthetic (`tint_line_bg`'s own addition to reach `width`) —
        // they can never be part of a real search match, so they always get the tint
        // color even on a row that does contain one.
        let spans = vec![Span::styled("ab", Style::default().bg(SEARCH_HIGHLIGHT_BG))];

        let tinted = tint_line_bg(spans, Color::DarkGray, 5);

        assert_eq!(tinted.len(), 2);
        assert_eq!(tinted[1].content.as_ref(), "   ");
        assert_eq!(tinted[1].style.bg, Some(Color::DarkGray));
    }

    fn numbered_text(count: usize) -> Text<'static> {
        Text::from(
            (0..count)
                .map(|i| Line::from(Span::raw(i.to_string())))
                .collect::<Vec<_>>(),
        )
    }

    fn line_values(text: &Text) -> Vec<usize> {
        text.lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    .parse()
                    .unwrap()
            })
            .collect()
    }

    #[test]
    fn window_text_rows_slices_to_the_visible_range() {
        let text = numbered_text(100);
        let windowed = window_text_rows(text, 10, 5);
        assert_eq!(line_values(&windowed), vec![10, 11, 12, 13, 14]);
    }

    #[test]
    fn window_text_rows_clamps_start_and_end_at_the_last_line() {
        let text = numbered_text(10);
        let windowed = window_text_rows(text, 8, 5);
        assert_eq!(line_values(&windowed), vec![8, 9]);

        let text = numbered_text(10);
        let windowed = window_text_rows(text, 50, 5);
        assert!(line_values(&windowed).is_empty());
    }

    /// The reason this fix exists at all: with the old `Paragraph::scroll` approach, a
    /// `usize` scroll offset above `u16::MAX` would wrap when cast down for rendering.
    /// Slicing the `Text` directly has no such limit.
    #[test]
    fn window_text_rows_handles_offsets_beyond_u16_max() {
        let count = u16::MAX as usize + 50;
        let text = numbered_text(count);
        let start = u16::MAX as usize + 10;
        let windowed = window_text_rows(text, start, 3);
        assert_eq!(line_values(&windowed), vec![start, start + 1, start + 2]);
    }
}
