# diffview

A terminal UI tool for visually reviewing git diffs and interactively staging changes.

```
┌─────────────────┬──────────────────────────────────────────────────┐
│   File Tree     │                   Diff View                       │
│                 │                                                    │
│ ▼ src/          │ @@ -10,7 +10,9 @@                                 │
│   ├─ main.rs  M │  context line                                     │
│   └─ lib.rs   A │ -removed line                                     │
│ ▶ tests/        │ +added line                                       │
│                 │  context line                                     │
└─────────────────┴──────────────────────────────────────────────────┘
 tool:raw  [a]add [r]revert [A]add-all [v]line-select [q]quit
```

## Installation

Requires the Rust toolchain (rustc 1.88 or later).

```bash
# Clone the repository
git clone <repository-url>
cd diffview

# Build in release mode
cargo build --release

# Optionally add to PATH
cp target/release/diffview ~/.local/bin/
```

## Usage

```bash
# Open the git repository in the current directory
diffview

# Open a specific repository
diffview /path/to/repo

# Show only staged changes
diffview --staged

# Specify a diff tool
diffview --tool delta
diffview --tool difftastic
diffview --tool raw        # default
```

## Key Bindings

### Global

| Key   | Action                             |
| ----- | ---------------------------------- |
| `Tab` | Switch focus between tree and diff |
| `?`   | Show key binding help              |
| `q`   | Quit                               |

### File Tree (left pane)

| Key       | Action                                  |
| --------- | --------------------------------------- |
| `j` / `↓` | Move down                               |
| `k` / `↑` | Move up                                 |
| `Enter`   | Show diff for the selected file         |
| `Space`   | Toggle directory fold                   |
| `a`       | `git add` selected file / directory     |
| `r`       | `git restore` selected file / directory |

### Diff View (right pane)

| Key       | Action                               |
| --------- | ------------------------------------ |
| `j` / `↓` | Scroll down one line                 |
| `k` / `↑` | Scroll up one line                   |
| `Ctrl+D`  | Scroll down half a page              |
| `Ctrl+U`  | Scroll up half a page                |
| `g`       | Jump to top                          |
| `G`       | Jump to bottom                       |
| `n`       | Jump to next hunk                    |
| `p`       | Jump to previous hunk                |
| `a`       | Stage current hunk (`git add`)       |
| `r`       | Unstage current hunk (`git restore`) |
| `A`       | Stage all changes in the file        |
| `R`       | Revert all changes in the file       |
| `v`       | Enter line-select mode               |

### Line-Select Mode (started with `v`)

| Key       | Action                                       |
| --------- | -------------------------------------------- |
| `j` / `k` | Move cursor                                  |
| `Space`   | Toggle line selection (`+` / `-` lines only) |
| `a`       | Stage selected lines                         |
| `r`       | Revert selected lines                        |
| `Esc`     | Exit line-select mode                        |

## File Status Indicators

| Symbol | Color    | Meaning             |
| ------ | -------- | ------------------- |
| `M`    | Yellow   | Modified            |
| `A`    | Green    | Added               |
| `D`    | Red      | Deleted             |
| `?`    | Gray     | Untracked           |
| `U`    | Red bold | Unmerged (conflict) |

## Diff Tools

### raw (default)

Displays the raw `git diff HEAD` output with syntax highlighting.
All operations (hunk / line level) are available.

### delta

Requires [delta](https://github.com/dandavison/delta) to be installed.

```bash
brew install git-delta
diffview --tool delta
```

Your `~/.gitconfig` `[delta]` settings (syntax highlighting, themes, etc.) are automatically applied.

### difftastic

Requires [difftastic](https://github.com/wilfred/difftastic) to be installed.

```bash
brew install difftastic
diffview --tool difftastic
```

> **Note:** Since difftastic produces AST-based diffs, hunk / line level staging is not available. Only file-level operations are supported.

## Configuration

Settings can be specified in `~/.config/diffview/config.toml`.

```toml
[diff]
# "raw" | "delta" | "difftastic"
tool = "raw"

[keybindings]
add        = "a"
revert     = "r"
add_all    = "A"
revert_all = "R"
select_mode = "v"
toggle_fold = " "
next_hunk  = "n"
prev_hunk  = "p"
```

Command-line arguments take precedence over the configuration file.

## Tech Stack

| Purpose       | Crate                                                                                                      |
| ------------- | ---------------------------------------------------------------------------------------------------------- |
| TUI           | [ratatui](https://github.com/ratatui-org/ratatui) + [crossterm](https://github.com/crossterm-rs/crossterm) |
| ANSI parsing  | [ansi-to-tui](https://github.com/uttarayan21/ansi-to-tui)                                                  |
| CLI           | [clap](https://github.com/clap-rs/clap)                                                                    |
| Async runtime | [tokio](https://tokio.rs/)                                                                                 |
| Config        | [serde](https://serde.rs/) + [toml](https://github.com/toml-rs/toml)                                       |
