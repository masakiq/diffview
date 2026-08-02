# AGENTS.md

This file provides guidance to AI coding agents (Claude Code, Codex, etc.) when working with code in this repository.
`CLAUDE.md` is an alias for this file.

## Build & Test Commands

```bash
cargo build --release          # Release build
cargo build                    # Debug build
cargo test                     # Run all tests (171 tests, spread across domain/, infra/, components/, views/, and app/mod.rs)
cargo test test_parse_hunk     # Run a single test by name
cargo clippy --all-targets     # Lint
cargo fmt                      # Format
cargo run -- --tool raw        # Run with raw diff (default)
cargo run -- --tool delta      # Run with delta renderer
cargo run -- --tool difftastic # Run with difftastic renderer
cargo run -- 891c1b8           # Commit target (read-only)
cargo run -- 0000000000000000000000000000000000000000 # Working tree target (special-cased null OID)
```

Requires rustc 1.88+. If compilation fails with syntax errors, run `rustup update stable`.

## Architecture

Rust TUI application for interacting with git diffs. Uses ratatui + crossterm for the terminal UI, with ANSI color support via ansi-to-tui.

### Module Map

```
src/
├── main.rs          # CLI parsing, terminal setup/restore, App::new() + App::run()
├── app/
│   ├── mod.rs        # App struct (owns TreeViewState + DiffViewState), event loop
│   │                  #   (run/handle_key), and cross-view logic not yet split out:
│   │                  #   diff cache, content-resolution glue, search routing,
│   │                  #   tree-preview debounce
│   └── focus.rs       # ActiveView — Workspace | Diff, derived from Focus
├── domain/            # Pure logic — no process spawning, no file I/O
│   ├── review_target.rs  # ReviewTarget (WorkingTree | Commit)
│   ├── content.rs        # Full-file content resolution policy: resolves target/pane/
│   │                      #   side/file-state to a GitObjectRef (Index|Head|Commit|
│   │                      #   ParentOfCommit) + path — a semantic request, not a git
│   │                      #   rev-spec string (infra/git/diff.rs's get_file_content_at_object
│   │                      #   is the only place that formats "<rev>:path" syntax). Also
│   │                      #   defines FullFileSource and TreePane (data-only — their
│   │                      #   `impl` blocks with app-specific behavior stay in
│   │                      #   app/mod.rs, which imports the types from here)
│   ├── diff.rs             # FileDiff/Hunk/DiffLine + parse_diff and friends
│   ├── patch.rs             # build_hunk_patch/build_partial_patch/build_reverse_partial_patch
│   └── status.rs             # GitFile + parse_status/parse_commit_name_status
├── infra/              # External process / OS boundary
│   ├── git/
│   │   ├── mod.rs       # run_git, run_git_with_stdin, get_repo_root, resolve_commit
│   │   ├── diff.rs        # get_raw_diff/get_display_diff, bat/delta/difftastic invocation
│   │   ├── apply.rs        # stage_file/unstage_file/stage_lines/unstage_lines
│   │   └── status.rs        # get_status/get_commit_files
│   └── (clipboard.rs and config.rs stay at src/ top level — thin enough that
│         moving them here wasn't judged worth it)
├── components/          # UI logic/state shared across more than one view
│   ├── cursor.rs          # Shared viewport-follow math (not the cursor state itself —
│   │                        #   each view owns its own row-space cursor)
│   ├── highlight.rs         # Search-match highlighting, used by both tree and diff views
│   ├── search.rs             # next_match_from/prev_match_from (pure match lookup)
│   └── tree_row.rs            # Renders one tree row (file or directory)
├── views/                # Per-screen key handler + render, as `impl App` blocks plus a
│   │                      #   free `render()` function per module — state itself
│   │                      #   (TreeViewState/DiffViewState) lives on `App` in app/mod.rs
│   ├── mod.rs              # Top-level layout split + render() dispatch
│   ├── tree/mod.rs           # Tree pane: handle_tree_key + render
│   ├── diff/
│   │   ├── mod.rs              # Diff pane: handle_diff_key + render; operates on
│   │   │                        #   App's `diff: DiffViewState` (raw_diff/file_diff/
│   │   │                        #   line_infos — defined in app/mod.rs) via `self.diff.*`
│   │   └── inline_select.rs      # Line-select handler, reached via Focus::InlineSelect;
│   │                              #   also `impl App`, reads/writes the same
│   │                              #   `self.diff.*` fields as diff/mod.rs
│   └── statusbar/mod.rs      # Status bar: help-text builders + render
├── clipboard.rs         # OS clipboard (pbcopy/xclip/etc.)
└── config.rs             # config.toml parsing
```

Module boundaries are enforced by Rust's own visibility rules, not just convention:
`views/` is a sibling of `app/`, so anything it needs from `App` (fields or methods)
must be `pub` or `pub(crate)` — see the many `pub(crate)` markers in `app/mod.rs` on
methods that exist mainly for a specific view to call.

### Data Flow

1. CLI: `diffview [--tool TOOL] [REV]` (`REV` omitted or all-zero OID = working tree target, other `REV` = commit target)
2. Working tree target: `git status --porcelain` → parsed into `Vec<GitFile>` (staged/unstaged char pair per file)
3. Working tree target: files split into two `TreeSection`s (unstaged vs staged), each with its own `BTreeMap`-based tree
4. Commit target: `git show --format= --name-status --find-renames <rev>` → single file tree section
5. Selecting a file loads diff:
   - Working tree target: `git diff` / `git diff --cached`
   - Commit target: `git show --format= --patch <rev> -- <path>`
6. Full-file view resolves which git object to show via `domain/content.rs`'s pure policy function, then reads it through `infra/git/diff.rs` — policy and I/O are deliberately separate calls (`domain/` never calls into `infra/`)
7. Line-level staging builds partial patches (`domain/patch.rs`) and applies them via `git apply --cached` on stdin (`infra/git/apply.rs`, working tree target only)

### Key Types & Their Roles

- **`App`** (`app/mod.rs`): Central state. Owns `tree: TreeViewState`, `diff: DiffViewState`, focus state, `review_target: ReviewTarget`, and the event loop (`run()`/`handle_key()`). Per-view key handlers and render functions are declared in `views/*.rs` — still `impl App` blocks (i.e. still methods of `App`), just organized by screen instead of all living in `app/mod.rs`, plus a free `render()` function per module.
- **`TreeViewState`** (`app/mod.rs`): Wraps the `unstaged`/`staged` `TreeSection`s. The Commit target still reuses `unstaged` for its "Files" section rather than having its own field — see `TreeViewState`'s doc comment for the not-yet-done follow-up.
- **`DiffViewState`** (`app/mod.rs`): The loaded diff document (`raw_diff`/`file_diff`/`line_infos`), the three cursor spaces that read it (`patch_cursor`/`diff_cursor`/`full_file_cursor` — distinct row spaces, see each field's doc comment), and the diff/scroll caches. All fields are `pub`. It lives on `App` (`app.diff`), not on `views/diff/`; both `views/diff/mod.rs` and `views/diff/inline_select.rs` reach it the same way, via `self.diff.*` on `App`.
- **`TreeSection`**: Manages `all_nodes: Vec<TreeNode>` + `visible: Vec<usize>` (indices into all_nodes). Folding works by filtering visible indices based on ancestor expansion state.
- **`Focus`** enum: `Unstaged | Staged | DiffView | InlineSelect` — determines which key handler runs.
- **`ActiveView`** (`app/focus.rs`): `Workspace | Diff` — a coarser grouping derived from `Focus` (`App::active_view()`), used for the top-level render split. `InlineSelect` maps to `Diff`: it's a subview of the Diff screen, not an independent active view.
- **`TreePane`** (`domain/content.rs`): `Unstaged | Staged` — identifies which tree section, used for diff origin tracking. Its `impl` (label/`to_focus`/`is_staged`) stays in `app/mod.rs`, since `to_focus` returns `Focus`, an app-owned type.
- **`ReviewTarget`** (`domain/review_target.rs`): `WorkingTree | Commit(String)`. Stored directly as `App::review_target`; `App::target()` clones it and `App::is_commit()` is the common shorthand.
- **`FileDiff` / `Hunk` / `DiffLine`** (`domain/diff.rs`): Parsed diff structure used for line-level operations.

### Layout

The UI splits into: left tree pane + right diff pane + bottom status bar (1 line). The tree pane defaults to 20% width and is configurable via `diff.tree_width_percentage` (clamped to 10–90, see `config.rs`'s `DiffConfig::tree_width_percentage()`). Rendering is in `views/mod.rs::render()`, which dispatches to `views::tree::render`, `views::diff::render`, and `views::statusbar::render`.

- Working tree target: left tree pane is vertically split into unstaged/staged sections
- Commit target: left tree pane is a single file tree section (`Files`)

### Interaction Notes

- Working tree target tree operations use `u` for stage/unstage on files and directories
- InlineSelect uses `u` to apply the selected lines
- Full-file view (`f`/`F`, still `Focus::DiffView`) always shows a line cursor over real content; `v` starts/cancels a line range and `y` copies it — read-only, so it works under the Commit target too, unlike InlineSelect
- The Commit target keeps `Enter` as an open shortcut in the tree, equivalent to `l`

### Partial Patch System (`domain/patch.rs`)

The trickiest part of the codebase. Two distinct patch builders:
- **`build_partial_patch`** (staging): Selected `+` kept, unselected `+` omitted, selected `-` kept, unselected `-` become context
- **`build_reverse_partial_patch`** (unstaging): Operates on INDEX perspective. Selected `+` become `-` (remove from index), selected `-` become `+` (restore to index). Does NOT use `--reverse` flag — it constructs the forward patch manually with swapped semantics.

### Tree Construction

`build_section()` (in `app/mod.rs`) is a free function (not a method) due to borrow checker constraints. Directories use trailing `/` as BTreeMap keys to sort before their children. Expansion state is preserved across refreshes via `prev_expanded` snapshot.

## Conventions

- Commits follow Conventional Commits: `feat: ...`, `feat(scope): ...`, `docs: ...`, `fix: ...`
- Unit tests live alongside implementation in `#[cfg(test)] mod tests`. When changing `src/domain/*` or `src/infra/*`, add or update tests there; when changing a view's handler or render logic, add tests in `app/mod.rs`'s test module (most view-level tests still live there, exercised through `App`'s public/`pub(crate)` methods) or the relevant `views/*` file.
- Responsibilities are separated: pure logic in `src/domain/` (no I/O), external process/OS boundaries in `src/infra/`, per-screen key handler+render in `src/views/`, UI logic shared across views in `src/components/`, orchestration and state ownership (event loop, `TreeViewState`/`DiffViewState`, cross-view state) in `src/app/`.
- When a change affects build steps, CLI options, or internal structure (types, modules, data flow), ask the user whether this file (`AGENTS.md` / `CLAUDE.md`) needs to be updated.
- **REQUIRED**: When adding or removing any screen, pane, focus state, key binding, or feature, update `docs/reference.md` in the same PR/commit. This document is the shared vocabulary between users and AI agents — keeping it accurate is mandatory.

## Configuration

`~/.config/diffview/config.toml`, under a `[diff]` table:
- `tool` (`"raw"` | `"delta"` | `"difftastic"`, default `"raw"`) — CLI `--tool` flag overrides this
- `tree_width_percentage` (default `20`, clamped to 10–90 at read time) — see [Layout](#layout)
- `[diff.commit]` sub-table: `key` (default `"C"`) and `command` (default `["git", "commit", "-v"]`) — the commit-action keybinding and the command it runs

## Diff Tool Constraints

- `raw`: Full functionality (file/hunk/line staging)
- `delta`: Full functionality, pipes through `delta` binary for display, re-renders on terminal resize
- `difftastic`: File-level staging only — AST-based diffs have no parseable hunk structure, so `supports_line_ops()` returns false
- The Commit target (`diffview <REV>`): read-only for all tools (no stage/unstage, no line apply). The all-zero object ID is a special case and opens the working tree target instead.
