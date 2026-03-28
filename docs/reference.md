# diffview UI Reference

This document defines the names of screens, panes, and features so that users and AI agents share a common vocabulary.

---

## Modes

diffview has two launch modes.

| Mode | How to launch | Description |
|------|--------------|-------------|
| **Working Tree Mode** | `diffview` (no arguments) | Operate on diffs between the working tree and the index. Stage and unstage operations are available. |
| **Commit Mode** | `diffview <REV>` | Browse the changes of a specified commit. Read-only; staging operations are not available. |

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
│ [tool:raw] [j/k]move [Enter]stage ...          M=modified A=added ...   │
└─────────────────────────────────────────────────────────────────────────┘
```

### Pane Definitions

| Pane | Description | Working Tree | Commit |
|------|-------------|:---:|:---:|
| **Tree Pane** | Left side of the screen. The entire area that displays the file tree. Width defaults to 20% and is configurable via `config.toml`. Hidden when focus is DiffView or InlineSelect — the diff is shown full-screen instead. | ○ | ○ |
| **Unstaged section** | Upper half of the Tree Pane. Shows working tree changes that have not been staged. | ○ | − |
| **Staged section** | Lower half of the Tree Pane. Shows changes already registered in the index. | ○ | − |
| **Files section** | The full Tree Pane area (Commit Mode only). Shows the list of files changed in the commit. | − | ○ |
| **Diff Pane** | Right side of the screen (default 80%). Shows the diff of the selected file. Shown full-screen when focus is DiffView. | ○ | ○ |
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
│ [SELECT] [j/k]move [Enter]apply [v]back [h]tree ...                     │
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
| **DiffView** | Diff Pane (full-screen) | Press `l` to move focus (Tree Pane is hidden; diff is shown full-screen). Moving the cursor in the tree updates the diff preview on the right but does not change focus. In Commit Mode, `Enter` works the same as `l`. |
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
| Jump to next hunk | `]` | DiffView, InlineSelect | |
| Jump to previous hunk | `[` | DiffView, InlineSelect | |

### Tree Operations

| Action | Key | Description |
|--------|-----|-------------|
| Expand directory | `l` / `→` | Opens the directory and shows its children. If a file is selected, opens the Diff Pane. |
| Fold directory | `h` / `←` | Folds the parent directory of the current node and moves the cursor to that parent. |
| Return to tree | `h` / `←` | Returns to the Tree Pane from DiffView or InlineSelect. |

### Staging Operations (Working Tree Mode only)

| Action | Operation | Description |
|--------|-----------|-------------|
| Stage file | Select file in Unstaged section → `Enter` | Adds the entire file to the index. |
| Unstage file | Select file in Staged section → `Enter` | Removes the entire file from the index. |
| Stage directory | Select directory in Unstaged section → `Enter` | Stages all files under the directory. |
| Unstage directory | Select directory in Staged section → `Enter` | Unstages all files under the directory. |
| Stage by line | `v` in DiffView → select line → `Enter` | Stages one line at a time in Inline Select Pane. Raw / Delta tool only. |
| Unstage by line | Open a Staged file in DiffView → `v` → `Enter` | Unstages one line at a time in Inline Select Pane. Raw / Delta tool only. |

### Search

| Action | Key | Description |
|--------|-----|-------------|
| Start search | `/` | Begins entering a search query. |
| Cancel search | `Esc` | Cancels the search input. |
| Next match | `n` | Moves to the next match. |
| Previous match | `N` | Moves to the previous match. |

### Other

| Action | Key | Description |
|--------|-----|-------------|
| Copy file path | `c` | Copies the selected or displayed file path to the clipboard. |
| Commit | Configurable (default `C`) | Runs the configured external commit command. Available in Working Tree Mode only, when focus is Unstaged, Staged, or DiffView. The key can be changed in `config.toml`. |
| Refresh | `r` | In Working Tree Mode: re-fetches `git status` and updates the screen. In Commit Mode: rebuilds the commit file list and reloads the diff. |
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
│ [tool:raw]   │ [j/k]move [Enter]stage ...            │ M=modified A=added  │
└──────────────┴──────────────────────────────────────┴─────────────────────┘
```

Errors and operation results are shown in the center of the Status Bar.

| State | Example | Color |
|-------|---------|-------|
| Normal | `[j/k]move [Enter]stage ...` | White |
| InlineSelect active | `[SELECT] [j/k]move ...` | Black on yellow |
| Searching | `[SEARCH] /query` | Black on yellow |
| Operation success | `Staged: src/main.rs` | Yellow |
| Error | `⚠ failed to apply patch` | Red, bold |
