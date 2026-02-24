# AGENTS.md

This file provides guidance to AI coding agents (Claude Code, Codex, etc.) when working with code in this repository.
`CLAUDE.md` is an alias for this file.

## Build & Test Commands

```bash
cargo build --release          # Release build
cargo build                    # Debug build
cargo test                     # Run all tests (11 tests across diff, apply, status modules)
cargo test test_parse_hunk     # Run a single test by name
cargo clippy --all-targets     # Lint
cargo fmt                      # Format
cargo run -- --tool raw        # Run with raw diff (default)
cargo run -- --tool delta      # Run with delta renderer
cargo run -- --tool difftastic # Run with difftastic renderer
cargo run -- 891c1b8           # Commit mode (read-only)
cargo run -- --path /repo 891c1b8  # Commit mode with explicit repo path
```

Requires rustc 1.88+. If compilation fails with syntax errors, run `rustup update stable`.

## Architecture

Rust TUI application for interacting with git diffs. Uses ratatui + crossterm for the terminal UI, with ANSI color support via ansi-to-tui.

### Data Flow

1. CLI: `diffview [OPTIONS] [REV]` (`REV` omitted = working tree mode, provided = commit mode)
2. Working tree mode: `git status --porcelain` → parsed into `Vec<GitFile>` (staged/unstaged char pair per file)
3. Working tree mode: files split into two `TreeSection`s (unstaged vs staged), each with its own `BTreeMap`-based tree
4. Commit mode: `git show --format= --name-status --find-renames <rev>` → single file tree section
5. Selecting a file loads diff:
   - Working tree mode: `git diff` / `git diff --cached`
   - Commit mode: `git show --format= --patch <rev> -- <path>`
6. Line-level staging builds partial patches and applies via `git apply --cached` on stdin (working tree mode only)

### Key Types & Their Roles

- **`App`** (`app.rs`): Central state. Owns both `TreeSection`s, diff state, focus state, commit mode (`commit_revision`), and all key handlers. The `run()` method is the event loop.
- **`TreeSection`**: Manages `all_nodes: Vec<TreeNode>` + `visible: Vec<usize>` (indices into all_nodes). Folding works by filtering visible indices based on ancestor expansion state.
- **`Focus`** enum: `Unstaged | Staged | DiffView | InlineSelect` — determines which key handler runs
- **`TreePane`** enum: `Unstaged | Staged` — identifies which tree section, used for diff origin tracking
- **`FileDiff` / `Hunk` / `DiffLine`** (`git/diff.rs`): Parsed diff structure used for line-level operations

### Layout

The UI splits into: left tree pane (1/4 width) + right diff pane (3/4 width) + bottom status bar (1 line). Rendering is in `ui/mod.rs::render()`.

- Working tree mode: left tree pane is vertically split into unstaged/staged sections
- Commit mode: left tree pane is a single file tree section (`Files`)

### Partial Patch System (`git/apply.rs`)

The trickiest part of the codebase. Two distinct patch builders:
- **`build_partial_patch`** (staging): Selected `+` kept, unselected `+` omitted, selected `-` kept, unselected `-` become context
- **`build_reverse_partial_patch`** (unstaging): Operates on INDEX perspective. Selected `+` become `-` (remove from index), selected `-` become `+` (restore to index). Does NOT use `--reverse` flag — it constructs the forward patch manually with swapped semantics.

### Tree Construction

`build_section()` is a free function (not a method) due to borrow checker constraints. Directories use trailing `/` as BTreeMap keys to sort before their children. Expansion state is preserved across refreshes via `prev_expanded` snapshot.

## Conventions

- Commits follow Conventional Commits: `feat: ...`, `feat(scope): ...`, `docs: ...`, `fix: ...`
- Unit tests live alongside implementation in `#[cfg(test)] mod tests`. When changing `src/git/*`, add or update tests.
- Responsibilities are separated: Git/domain logic in `src/git/`, UI rendering in `src/ui/`, orchestration in `src/app.rs`.
- When a change affects build steps, CLI options, or internal structure (types, modules, data flow), ask the user whether this file (`AGENTS.md` / `CLAUDE.md`) needs to be updated.

## Configuration

`~/.config/diffview/config.toml` — only `diff.tool` setting (`"raw"` | `"delta"` | `"difftastic"`). CLI `--tool` flag overrides config.
Repository path can be specified via CLI `--path <PATH>`.

## Diff Tool Constraints

- `raw`: Full functionality (file/hunk/line staging)
- `delta`: Full functionality, pipes through `delta` binary for display, re-renders on terminal resize
- `difftastic`: File-level staging only — AST-based diffs have no parseable hunk structure, so `supports_line_ops()` returns false
- Commit mode (`diffview <REV>`): read-only for all tools (no stage/unstage, no line apply)
