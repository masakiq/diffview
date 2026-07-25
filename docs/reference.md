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
| **Diff Pane** | Right side of the screen (default 80%). Shows the patch diff of the selected file in normal operation. In DiffView focus, `f` opens the current-side full file and `F` opens the previous-side full file; pressing the same key again returns to patch view. Shown full-screen when focus is DiffView. | ○ | ○ |
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
| **DiffView** | Diff Pane (full-screen) | Press `l` to move focus (Tree Pane is hidden; the selected file is shown full-screen). Moving the cursor in the tree updates the patch preview on the right but does not change focus. In Commit Mode, `Enter` works the same as `l`. Press `f` in DiffView to toggle the current-side full file, or `F` to toggle the previous-side full file. |
| **InlineSelect** | Inline Select Pane | Press `v` in DiffView. |

---

## Features

### Navigation

| Action | Key | Valid focus | Notes |
|--------|-----|------------|-------|
| Move down 1 line | `j` / `↓` | All | |
| Move up 1 line | `k` / `↑` | All | |
| Move down 5 lines | `Ctrl-d` | Unstaged, Staged | Fixed 5-line step in the Tree Pane |
| Move up 5 lines | `Ctrl-u` | Unstaged, Staged | Fixed 5-line step in the Tree Pane |
| Scroll down half page | `Ctrl-d` | DiffView, InlineSelect | Scrolls half the pane height |
| Scroll up half page | `Ctrl-u` | DiffView, InlineSelect | Scrolls half the pane height |
| Jump to top of diff | `g` | DiffView | |
| Jump to bottom of diff | `G` | DiffView | |
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
| Next match | `n` | Moves to the next match. In full-file view, `n` instead toggles line-number display when no search is active — see Diff View Modes. |
| Previous match | `N` | Moves to the previous match. |

### Diff View Modes

| Action | Key | Description |
|--------|-----|-------------|
| Toggle current-side full file | `f` | Available in DiffView only. Switches between patch view and the current side of the selected file. |
| Toggle previous-side full file | `F` | Available in DiffView only. Switches between patch view and the previous side of the selected file. |
| Copy opened full file | `P` | Available only in full-file view in DiffView. Copies the opened file contents to the clipboard. |
| Toggle line numbers | `n` | Available only in full-file view in DiffView, and only when no search is active (otherwise `n` moves to the next search match — see Search). Toggles the line-number gutter in the file preview and preserves the current scroll position. |

Full-file view rules:

- While the tree pane is focused, the right pane always stays in patch preview mode. `f` and `F` have no effect there.
- `v`, `[`, and `]` are available only in patch view.
- `P` works in either full-file view and copies the raw contents of the currently displayed file.
- Patch-view scroll position is remembered per file. Full-file view does not remember scroll position across visits — instead, every time you switch from patch view to full-file view (`f`/`F` while in patch view), the opening scroll position is derived fresh from the patch pane's top-displayed line:
  - `--tool raw`: exact — the patch pane's top line maps precisely to a file line via the parsed hunk data.
  - `--tool delta`: best-effort — only when delta is configured for `side-by-side` output with `line-numbers` enabled; the mapping reads the line number delta itself prints in the row's gutter (ANSI-stripped). Other delta configurations (unified mode, line numbers off) fall back to opening at the top. If the patch pane's top row isn't part of any hunk at all (e.g. delta's leading blank line before the first hunk, or the gap between hunks), the mapping skips forward to the next row that is.
  - `--tool difftastic`: always opens at the top — difftastic's structural diff has no parseable line-number correspondence.
  - In all cases, the mapped line is positioned at the very top of the pane.
- Switching between `FullFile(Current)` and `FullFile(Previous)` while already in full-file view (`f`/`F` pressed there, not from patch view) keeps the current scroll row as-is instead of recomputing from the patch pane — both directions (`f`→`F` and `F`→`f`) preserve it.
- Lines the underlying diff marks as changed get a background tint, independent of `--tool` (full-file content is always rendered through `bat`, not the selected diff tool): the current-side view (`f`) tints added lines dark green, the previous-side view (`F`) tints removed lines dark red. Syntax-highlighted foreground colors are preserved; only the background changes. Unchanged files, or a source where the diff has no hunks to show (e.g. an untracked file), have no tinted lines.
- Toggling line numbers with `n` re-renders the current full-file view in place and keeps the current scroll position (it does not jump to the top or recompute the patch-relative position).
- When you return to patch view, the file's patch-specific scroll position is restored.
- `f` opens the current side.
  - Working Tree / Unstaged: working tree file
  - Working Tree / Staged: index blob
  - Commit Mode: selected commit blob
- `F` opens the previous side.
  - Working Tree / Unstaged: index blob
  - Working Tree / Staged: `HEAD` blob
  - Commit Mode: first parent blob
- If the file does not exist on the requested side, an unavailable message is shown.
  - Example: pressing `f` on a deleted file
  - Example: pressing `F` on an added or untracked file
- Binary files show `Full file view unavailable for binary files`.
- Unmerged files show `Full file view unavailable for unmerged files`.

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
