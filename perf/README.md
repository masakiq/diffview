# Performance Scripts

This directory contains small automation scripts for measuring `diffview` startup time and tree navigation performance.

## Requirements

- `cargo`
- `git`
- `expect`
- `hyperfine`

`expect` is a command-line automation tool for interactive terminal applications. In this setup, the `expect` scripts drive the TUI through a pseudo-terminal. The shell runner builds the release binary, runs the benchmarks, and stores the results.

## Installation on macOS

Install the required benchmarking tools with Homebrew:

```bash
brew install hyperfine expect
```

Notes:

- `hyperfine` is not bundled with macOS, so install it with Homebrew.
- `expect` is often already available on macOS as `/usr/bin/expect`, but Homebrew also provides an `expect` formula if you want to install or manage it explicitly with Homebrew.
- You can verify both tools with:

```bash
hyperfine --version
expect -v
```

## Files

### `perf/scripts/startup.expect`

Purpose:

- Launches `diffview`
- Waits until the initial screen becomes visible
- Sends `q`
- Exits when the process finishes

This is the script used for startup-time measurements.

Usage:

```bash
perf/scripts/startup.expect ./target/release/diffview --tool raw
perf/scripts/startup.expect ./target/release/diffview --tool raw HEAD~1
```

Notes:

- It waits for one of these titles to appear: `Unstaged`, `Staged`, or `Files`.
- `Unstaged` and `Staged` cover the Working Tree target.
- `Files` covers the Commit target.
- If the UI labels change, this script must be updated.

### `perf/scripts/tree_burst.expect`

Purpose:

- Launches `diffview`
- Waits for the first screen
- Sends `j` repeatedly
- Waits for a configurable settle period
- Sends `q`

This is the script used for burst navigation measurements.

Usage:

```bash
perf/scripts/tree_burst.expect ./target/release/diffview --tool raw
perf/scripts/tree_burst.expect --moves 200 --settle-ms 300 -- ./target/release/diffview --tool raw HEAD~1
```

Options:

- `--moves N`: number of `j` key presses to send. Default: `200`
- `--settle-ms N`: wait time after the last key press. Default: `300`

Notes:

- Use this script to measure list navigation throughput.
- The settle delay gives the debounced preview load time to complete.
- The `--` separator is optional when you do not need to disambiguate script options from the command being launched.

### `perf/scripts/tree_single_step.expect`

Purpose:

- Launches `diffview`
- Waits for the first screen
- Sends a single `j`
- Waits for a configurable settle period
- Sends `q`

This is the script used for single-step navigation measurements.

Usage:

```bash
perf/scripts/tree_single_step.expect ./target/release/diffview --tool raw
perf/scripts/tree_single_step.expect --settle-ms 300 -- ./target/release/diffview --tool raw HEAD~1
```

Options:

- `--settle-ms N`: wait time after the single `j` key press. Default: `300`

Notes:

- This script is useful when you want to measure perceived latency after one tree movement.
- The result includes the preview debounce interval and preview loading work.
- The `--` separator is optional when you do not need to disambiguate script options from the command being launched.

### `perf/scripts/run_measurements.sh`

Purpose:

- Builds the release binary unless told not to
- Runs startup, burst, and single-step benchmarks for the Working Tree target
- Optionally runs the same benchmarks for the Commit target
- Stores each benchmark result under `perf/results/<timestamp>/`

This script is a benchmark orchestrator, not a conventional pass/fail integration test. It runs multiple end-to-end performance scenarios and records timing results, but it does not assert thresholds or return success/failure based on performance budgets.

Usage:

```bash
perf/scripts/run_measurements.sh --tool raw --commit-rev HEAD~1
```

Useful examples:

```bash
perf/scripts/run_measurements.sh --tool raw
perf/scripts/run_measurements.sh --tool raw --commit-rev HEAD~1
perf/scripts/run_measurements.sh --tool raw --runs 30 --warmup 5 --moves 500
perf/scripts/run_measurements.sh --dry-run --skip-build --commit-rev HEAD
```

Options:

- `--bin PATH`: path to the `diffview` binary. Default: `./target/release/diffview`
- `--tool TOOL`: diff tool to benchmark. Default: `raw`
- `--commit-rev REV`: revision to benchmark under the Commit target
- `--runs N`: number of `hyperfine` runs. Default: `20`
- `--warmup N`: number of `hyperfine` warmup runs. Default: `3`
- `--moves N`: move count for `tree_burst.expect`. Default: `200`
- `--settle-ms N`: settle delay for burst and single-step benchmarks. Default: `300`
- `--output-dir PATH`: benchmark result root. Default: `perf/results`
- `--skip-build`: skip `cargo build --release`
- `--dry-run`: print benchmark commands without running `hyperfine`

Output files:

- `metadata.txt`: benchmark settings for the run
- `startup_worktree.txt` and `startup_worktree.json`
- `tree_burst_worktree.txt` and `tree_burst_worktree.json`
- `tree_single_step_worktree.txt` and `tree_single_step_worktree.json`
- Commit-target files with the same naming pattern when `--commit-rev` is provided

## Recommended Workflow

1. Build the release binary.
2. Start with `--tool raw`.
3. Run working-tree benchmarks first.
4. Add `--commit-rev` for Commit-target benchmarks.
5. Inspect the JSON files exported by `hyperfine`.
6. Move on to `delta` or `difftastic` only after you have a raw baseline.

## Caveats

- These scripts measure end-to-end behavior, not isolated internal function time.
- The tree navigation scripts include preview debounce and preview loading cost.
- `run_measurements.sh --dry-run` is the safest way to confirm commands before running the full benchmark suite.
