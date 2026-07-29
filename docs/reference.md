# diffview UI Reference

This document defines the names of screens, panes, and features so that users and AI agents share a common vocabulary.

---

## Modes

diffview has two launch modes.

| Mode | How to launch | Description |
|------|--------------|-------------|
| **Working Tree Mode** | `diffview` (no arguments), or `diffview 0000000000000000000000000000000000000000` | Operate on diffs between the working tree and the index. Stage and unstage operations are available. |
| **Commit Mode** | `diffview <REV>` | Browse the changes of a specified commit. Read-only; staging operations are not available. The all-zero object ID is treated as a special case and falls back to Working Tree Mode. |

---

## Screens & Panes

```
┌─────────────────────────────────────────────────────────────────────────┐
│                                                                         │
│  Tree Pane (default 20%)      Diff Pane (default 80%)                   │
│ ┌───────────────────────┐  ┌──────────────────────────────────────────┐ │
│ │ Unstaged (2)          │  │ @@ -10,6 +10,8 @@                        │ │
│ │   src/                │  │  context line                            │ │
│ │     main.rs         M │  │ -old line                                │ │
│ ├───────────────────────┤  │ +new line                                │ │
│ │ Staged (1)            │  │  context line                            │ │
│ │   README.md         M │  │                                          │ │
│ └───────────────────────┘  └──────────────────────────────────────────┘ │
│ ─────────────────────────────── Status Bar ───────────────────────────── │
│ [tool:raw] [j/k]move [u]stage ...              M=modified A=added ...   │
└─────────────────────────────────────────────────────────────────────────┘
```

### Pane Definitions

| Pane | Description | Working Tree | Commit |
|------|-------------|:---:|:---:|
| **Tree Pane** | Left side of the screen. The entire area that displays the file tree. Width defaults to 20% and is configurable via `config.toml`. Hidden when focus is DiffView or InlineSelect — the diff is shown full-screen instead. | ○ | ○ |
| **Unstaged section** | Upper half of the Tree Pane. Shows working tree changes that have not been staged. | ○ | − |
| **Staged section** | Lower half of the Tree Pane. Shows changes already registered in the index. | ○ | − |
| **Files section** | The full Tree Pane area (Commit Mode only). Shows the list of files changed in the commit. | − | ○ |
| **Diff Pane** | Right side of the screen (default 80%). Shows the patch diff of the selected file in normal operation — except an untracked file (Working Tree / Unstaged only), which has no patch of its own and opens directly in full-file view instead. In DiffView focus, `f` opens the current-side full file and `F` opens the previous-side full file; pressing the same key again returns to patch view (blocked for that untracked-file exception, which has no patch view to return to). Both patch view and full-file view always show a line cursor over real content while DiffView is focused (not over an "unavailable" placeholder such as a binary/unmerged/missing/empty file) — see Patch view cursor / Full-file view rules below. Shown full-screen when focus is DiffView. | ○ | ○ |
| **Inline Select Pane** | A line-selection screen that uses the same full-screen area as the DiffView focus. Allows staging/unstaging one line at a time. | ○ | − |
| **Status Bar** | Bottom 1 line of the screen. Displays the current tool name, operation hints, and file status legend. | ○ | ○ |

---

## Layout Details

### Working Tree Mode

```
┌─────────────────────────────────────────────────────────────────────────┐
│ Tree Pane                     Diff Pane                                 │
│ ┌───────────────────────┐  ┌──────────────────────────────────────────┐ │
│ │ Unstaged (N)          │  │                                          │ │
│ │   <file tree>         │  │  <diff of selected file>                 │ │
│ │                       │  │                                          │ │
│ ├───────────────────────┤  │                                          │ │
│ │ Staged (N)            │  │                                          │ │
│ │   <file tree>         │  │                                          │ │
│ │                       │  │                                          │ │
│ └───────────────────────┘  └──────────────────────────────────────────┘ │
│ ──────────────────────────── Status Bar ─────────────────────────────── │
└─────────────────────────────────────────────────────────────────────────┘
```

### Commit Mode

```
┌─────────────────────────────────────────────────────────────────────────┐
│ Tree Pane                     Diff Pane                                 │
│ ┌───────────────────────┐  ┌──────────────────────────────────────────┐ │
│ │ Files (N)             │  │                                          │ │
│ │   <file tree>         │  │  <diff of selected file (read-only)>     │ │
│ │                       │  │                                          │ │
│ │                       │  │                                          │ │
│ │  (Staged section      │  │                                          │ │
│ │   is not shown)       │  │                                          │ │
│ │                       │  │                                          │ │
│ └───────────────────────┘  └──────────────────────────────────────────┘ │
│ ──────────────────────────── Status Bar ─────────────────────────────── │
└─────────────────────────────────────────────────────────────────────────┘
```

### Inline Select Pane (Working Tree Mode only)

Entered from DiffView by pressing `v`. The Tree Pane is hidden and the diff is shown full-screen.

```
┌─────────────────────────────────────────────────────────────────────────┐
│ Inline Select Pane (full-screen)                                         │
│ ┌─────────────────────────────────────────────────────────────────────┐ │
│ │ @@ -10,6 +10,8 @@                                                   │ │
│ │  context line                                                        │ │
│ │ ██-old line   ← cursor line (selectable)                             │ │
│ │  +new line                                                           │ │
│ │  context line                                                        │ │
│ └─────────────────────────────────────────────────────────────────────┘ │
│ ──────────────────────────── Status Bar ─────────────────────────────── │
│ [SELECT] [j/k]move [u]apply [v]back [h]tree ...                         │
└─────────────────────────────────────────────────────────────────────────┘
```

---

## Focus States

Indicates which part of the screen is the current input target. Key behavior changes depending on the focus.

```
  [Unstaged] <-> [Staged]
       |              |
       v              v
   [DiffView] <-> [InlineSelect]
       ^
  h to go back
```

| Focus | Target pane | How to enter |
|-------|------------|--------------|
| **Unstaged** | Unstaged section | Initial focus on launch (unless Unstaged is empty and Staged has items, in which case Staged is the initial focus). Also entered by pressing `k` past the top of the Staged section. |
| **Staged** | Staged section | Automatically entered by pressing `j` past the bottom of the Unstaged section. |
| **DiffView** | Diff Pane (full-screen) | Press `l` to move focus (Tree Pane is hidden; the selected file is shown full-screen). Moving the cursor in the tree updates the preview on the right (patch view for a tracked file, full-file view for an untracked one — see below) but does not change focus. In Commit Mode, `Enter` works the same as `l`. Press `f` in DiffView to toggle the current-side full file, or `F` to toggle the previous-side full file. Both patch view and full-file view always show a line cursor over real content within this same focus (not over an "unavailable" placeholder) — see Patch view cursor / Full-file view rules below. |
| **InlineSelect** | Inline Select Pane | Press `v` in DiffView. |

---

## Features

### Navigation

| Action | Key | Valid focus | Notes |
|--------|-----|------------|-------|
| Move down 1 line | `j` / `↓` | All | In DiffView (patch or full-file view), moves the cursor instead of just scrolling (the pane follows). |
| Move up 1 line | `k` / `↑` | All | In DiffView (patch or full-file view), moves the cursor instead of just scrolling (the pane follows). |
| Move down 5 lines | `Ctrl-d` | Unstaged, Staged | Fixed 5-line step in the Tree Pane |
| Move up 5 lines | `Ctrl-u` | Unstaged, Staged | Fixed 5-line step in the Tree Pane |
| Scroll down half page | `Ctrl-d` | DiffView, InlineSelect | Scrolls half the pane height. In DiffView (patch or full-file view), moves the cursor by a half page instead (the pane follows). |
| Scroll up half page | `Ctrl-u` | DiffView, InlineSelect | Scrolls half the pane height. In DiffView (patch or full-file view), moves the cursor by a half page instead (the pane follows). |
| Jump to top of diff | `gg` | DiffView | Vim-style two-key press. In DiffView (patch or full-file view), moves the cursor to the first line instead of just scrolling. |
| Jump to bottom of diff | `G` | DiffView | In DiffView (patch or full-file view), moves the cursor to the last line instead of just scrolling. |
| Jump to next hunk | `]` | DiffView, InlineSelect | Available only in patch view. |
| Jump to previous hunk | `[` | DiffView, InlineSelect | Available only in patch view. |

### Tree Operations

| Action | Key | Description |
|--------|-----|-------------|
| Expand directory | `l` / `→` | Opens the directory and shows its children. If a file is selected, opens the Diff Pane. |
| Fold directory | `h` / `←` | Folds the parent directory of the current node and moves the cursor to that parent. |
| Return to tree | `h` / `←` | Returns to the Tree Pane from DiffView or InlineSelect. |

### Staging Operations (Working Tree Mode only)

| Action | Operation | Description |
|--------|-----------|-------------|
| Stage file | Select file in Unstaged section → `u` | Adds the entire file to the index. |
| Unstage file | Select file in Staged section → `u` | Removes the entire file from the index. |
| Stage directory | Select directory in Unstaged section → `u` | Stages all files under the directory. |
| Unstage directory | Select directory in Staged section → `u` | Unstages all files under the directory. |
| Stage by line | `v` in DiffView → select line → `u` | Stages one line at a time in Inline Select Pane. Raw / Delta tool only. |
| Unstage by line | Open a Staged file in DiffView → `v` → `u` | Unstages one line at a time in Inline Select Pane. Raw / Delta tool only. |

### Search

| Action | Key | Description |
|--------|-----|-------------|
| Start search | `/` | Begins entering a search query. |
| Cancel search | `Esc` | Cancels the search input. |
| Next match | `n` | Moves to the next match. |
| Previous match | `N` | Moves to the previous match. |

### Diff View Modes

| Action | Key | Description |
|--------|-----|-------------|
| Toggle current-side full file | `f` | Available in DiffView only. Switches between patch view and the current side of the selected file. For an untracked file in Working Tree / Unstaged, pressing `f` from `FullFile(Current)` (the only patch-bound landing this key has for such a file) is a no-op instead — see Full-file view rules below. |
| Toggle previous-side full file | `F` | Available in DiffView only. Switches between patch view and the previous side of the selected file. For an untracked file in Working Tree / Unstaged, the previous side is always unavailable, so `F` shows that message instead of real content; from there, `f` still moves back to `FullFile(Current)` normally, but a second `F` (which would otherwise land on patch view) is a no-op — see Full-file view rules below. |
| Copy opened full file | `P` | Available only in full-file view in DiffView. Copies the opened file contents to the clipboard. |
| Start / cancel line range | `v` | Available only in full-file view in DiffView. First press marks the cursor's current line as the start of a range; pressing `v` again while a range is active cancels it (back to a single-line cursor). |
| Copy selected line(s) | `y` | Available only in full-file view in DiffView. Copies the active range to the clipboard, or just the cursor's line if no range is active. Copied text always ends in a newline — unlike `P`, which copies the raw file verbatim and so omits the trailing newline for a file that doesn't have one. |

Patch view cursor:

- Patch view (not just full-file view) always shows a line cursor over real content too. `j`/`k`, `Ctrl-d`/`Ctrl-u`, and `gg`/`G` move this cursor directly instead of just scrolling the pane, which follows along to keep the cursor visible — the same mechanics as full-file view's cursor (see below), and the same `DarkGray` background + bold styling. Unlike full-file view, there is no range select here: `v` still means "enter InlineSelect for staging" (see Staging Operations), not "start/cancel a range".
- Jumping to a hunk (`]`/`[`) moves the cursor to the hunk's first line, not just the pane's scroll position.
- Entering InlineSelect (`v`) starts its own cursor at the patch cursor's current line under `--tool raw` (where InlineSelect's raw-line indexing and the patch cursor's display-row indexing are the same thing) — not wherever the pane happens to be scrolled to. Under `--tool delta`, where display rows and raw rows diverge, it keeps starting from the pane's scroll position instead, same as before this cursor existed.
- This is the cursor `f`/`F` reads from when switching to full-file view — see the scroll/cursor derivation rule below.

Full-file view rules:

- While the tree pane is focused, the right pane stays in the file's default preview mode: patch view for a tracked file, full-file view (current side) for an untracked one — see below. `f` and `F` have no effect there.
- An untracked file opens directly in full-file view (`FullFile(Current)`) rather than patch view — selecting it in the tree, pressing `l`/`→` on it (or moving the tree cursor over it, which updates the preview the same way without changing focus) all open it this way. An untracked file can only ever appear in Working Tree / Unstaged (see File Status Symbols below), where `Enter` isn't wired to anything — `l`/`→` open it, same as any other file (see Tree Operations above). Patch view has nothing of its own to show for an untracked file (it would just be `get_file_preview`'s bat rendering again, see above), so this skips straight to where the always-on cursor and `v`/`y` line-range select already work — but that cursor and `v`/`y` themselves still require `DiffView` focus (below); moving the tree cursor over such a file, or leaving full-file view back to the tree with `h`, shows this same full-file preview without them. This also applies retroactively: if a file open in patch view turns untracked out from under it (e.g. an external `git rm --cached` picked up by `r`), the next refresh normalizes it into `FullFile(Current)` the same way, rather than leaving patch view stuck showing that same bat-rendering fallback. Only the specific step *back* to patch view is blocked for such a file — `f` from `FullFile(Current)` is a no-op (not an error), since there's no separate patch view to land in; `F` still moves to `FullFile(Previous)`, showing the unavailable-previous-side message (untracked content never existed on a previous side), and from there `f` moves back to `FullFile(Current)` normally — only a *second* `F` press, which would otherwise land on patch view, is also a no-op.
- `[` and `]` are available only in patch view (not full-file view). `v` is available in both, but means something different in each: in patch view it enters InlineSelect for staging; in full-file view it starts/cancels a line range for copying (see below) — it never enters a separate focus.
- `P` works in either full-file view and copies the raw contents of the currently displayed file.
- Full-file view always shows a line cursor over real content while `DiffView` is focused (not shown over an "unavailable" placeholder such as a binary/unmerged/missing file, and not shown at all when the tree pane merely has this file open as a preview — see above). `j`/`k`, `Ctrl-d`/`Ctrl-u`, and `gg`/`G` move this cursor directly instead of just scrolling the pane, which follows along to keep the cursor visible. The cursor's own line gets a `DarkGray` background extended to the full pane width; within an active range (`v`), every line in the range gets the same tint, with the exact cursor line additionally bolded. This overlays (and takes precedence over) the added/removed diff tint described below.
- `/`, `n`, and `N` search full-file content exactly as they do in patch view; moving to a match moves the cursor there too, not just the pane.
- Patch-view scroll position and cursor line are remembered per file. Full-file view does not remember scroll position across visits — instead, every time you switch from patch view to full-file view (`f`/`F` while in patch view), the opening scroll position (and cursor line) is derived fresh from the patch pane's cursor line — not just wherever the pane happens to be scrolled to:
  - For a tracked file's real diff, the derivation maps through the diff's hunk structure:
    - `--tool raw`: exact — the patch cursor's line maps precisely to a file line via the parsed hunk data.
    - `--tool delta`: best-effort — only when delta is configured for `side-by-side` output with `line-numbers` enabled; the mapping reads the line number delta itself prints in the row's gutter (ANSI-stripped). Other delta configurations (unified mode, line numbers off) fall back to opening at the top. If the patch cursor's row isn't part of any hunk at all (e.g. delta's leading blank line before the first hunk, or the gap between hunks), the mapping skips forward to the next row that is.
    - `--tool difftastic`: always opens at the top — difftastic's structural diff has no parseable line-number correspondence.
  - For an untracked file, patch view has no hunks to map through at all — it's showing the file's own content directly (`get_file_preview`'s rendering, the same content full-file view shows), so the patch cursor's row carries straight across as the file line, regardless of `--tool`. In practice this mapping is rarely exercised for an untracked file, since selecting one opens straight into full-file view already (above) rather than going through patch view first.
  - Any other case where the mapping can't determine a line opens at the top instead.
  - In all cases, the viewport is positioned so the mapped line lands on the exact same on-screen row the patch cursor had — not the top of the pane, and not just "scrolled into view" at whichever edge is nearest. When there isn't enough content above the mapped line to reproduce that row (it's close to the start of the file), the viewport scrolls as close as it can (as far as row 0) instead of going negative.
- Switching between `FullFile(Current)` and `FullFile(Previous)` while already in full-file view (`f`/`F` pressed there, not from patch view) keeps the current scroll row and cursor line as-is instead of recomputing from the patch pane — both directions (`f`→`F` and `F`→`f`) preserve it. Any active range is cleared on the switch.
- Lines the underlying diff marks as changed get a background tint, independent of `--tool` (full-file content is rendered via `bat` — falling back to `cat`, or to plain unstyled text if neither is available — never the selected diff tool): the current-side view (`f`) tints added lines dark green, the previous-side view (`F`) tints removed lines dark red. Syntax-highlighted foreground colors are preserved; only the background changes. Unchanged files, or a source where the diff has no hunks to show (e.g. an untracked file), have no tinted lines.
- When you return to patch view (`f`/`F` pressed while already in the matching full-file view), the file's own remembered scroll position and cursor line are both restored exactly as they were before entering full-file view — not the top of the restored viewport, and not reverse-mapped from wherever the full-file cursor ended up. Navigation made while in full-file view never overwrites this remembered position.
- `f` opens the current side.
  - Working Tree / Unstaged: working tree file
  - Working Tree / Staged: index blob
  - Commit Mode: selected commit blob
  - Exception: an untracked file in Working Tree / Unstaged is already open in full-file view by default (above) and has no patch view to toggle back to — `f` pressed from `FullFile(Current)` is disabled there (a no-op). `f` pressed from `FullFile(Previous)` still works normally, moving back to `FullFile(Current)`.
- `F` opens the previous side.
  - Working Tree / Unstaged: index blob
  - Working Tree / Staged: `HEAD` blob
  - Commit Mode: first parent blob
- If the file does not exist on the requested side, an unavailable message is shown.
  - Example: pressing `f` on a deleted file
  - Example: pressing `F` on an added or untracked file
- Binary files show `Full file view unavailable for binary files`.
- Unmerged files show `Full file view unavailable for unmerged files`.
- `v` and `y` are read-only operations, so they work over full-file content in Commit Mode too, not just Working Tree Mode.

### Other

| Action | Key | Description |
|--------|-----|-------------|
| Copy file path | `c` | Copies the selected or displayed file path to the clipboard. |
| Commit | Configurable (default `C`) | Runs the configured external commit command. Available in Working Tree Mode only, when focus is Unstaged, Staged, or DiffView. The key can be changed in `config.toml`. |
| Refresh | `r` | In Working Tree Mode: re-fetches `git status` and updates the screen. In Commit Mode: rebuilds the commit file list and reloads the current DiffView content. |
| Help | `?` | Displays a key binding list in the Status Bar. Available in Unstaged / Staged focus only. |
| Quit | `q` | Exits the application. |

---

## Diff Tools

Specified with the `--tool` flag or `diff.tool` in `~/.config/diffview/config.toml`.

| Tool | Description | Line-level staging |
|------|-------------|:---:|
| **raw** | Standard Git diff output (default) | ○ |
| **delta** | Color-enhanced output via the `delta` binary | ○ |
| **difftastic** | AST-based diff via `difftastic` | − |

---

## File Status Symbols

Symbols shown to the right of file names in the Tree Pane.

| Symbol | Name | Description | Working Tree | Commit |
|--------|------|-------------|:---:|:---:|
| `M` | Modified | File was modified | ○ | ○ |
| `A` | Added | File was newly added | ○ | ○ |
| `D` | Deleted | File was deleted | ○ | ○ |
| `R` | Renamed | File was renamed | ○ | ○ |
| `C` | Copied | File was copied | − | ○ |
| `?` | Untracked | New file not tracked by Git | ○ | − |
| `U` | Unmerged | Merge conflict exists | ○ | − |

---

## Status Bar Layout

```
┌──────────────┬──────────────────────────────────────┬─────────────────────┐
│ Tool name    │ Operation hints (changes with focus)  │ File status legend  │
│ [tool:raw]   │ [j/k]move [u]stage ...                │ M=modified A=added  │
└──────────────┴──────────────────────────────────────┴─────────────────────┘
```

Errors and operation results are shown in the center of the Status Bar.

| State | Example | Color |
|-------|---------|-------|
| Normal | `[j/k]move [u]stage ...` | White |
| InlineSelect active | `[SELECT] [j/k]move ...` | Black on yellow |
| Searching | `[SEARCH] /query` | Black on yellow |
| Operation success | `Staged: src/main.rs` | Yellow |
| Error | `⚠ failed to apply patch` | Red, bold |
