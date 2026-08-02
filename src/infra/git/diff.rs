use anyhow::Result;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePreview {
    pub content: String,
    pub uses_ansi: bool,
    /// Number of leading decoration lines (top border, filename, mid border) that `bat`
    /// prepends before the first line of actual file content. Zero when the `cat` fallback
    /// ran instead, since it emits no decorations.
    pub content_offset: usize,
}

/// Fallback header size assumed for `bat` calls that don't force an explicit `--style`
/// (i.e. `get_file_preview`, which relies on the user's own `bat` config/defaults): top
/// border, `File:` line, mid border. Calls that need a reliable offset instead force
/// `FULL_FILE_VIEW_BAT_STYLE`, whose header size is independent of `bat` config and file
/// path length.
const BAT_HEADER_LINE_COUNT: usize = 3;

/// `--style` forced on full-file view's `bat` invocation (`render_content_preview`), with
/// its exact header-row count. `numbers,grid` renders exactly one top border row before
/// content and one after — no `File:` banner (redundant with the app's own pane border),
/// and no dependency on the user's `bat` config or on path-length-driven header wrapping,
/// both of which make the plain default style's offset unreliable. Always paired with the
/// other flags `bat_ansi_args` adds alongside a forced style (`--wrap=never`, `--tabs=1`,
/// `--no-config`) and with removing `BAT_OPTS` from the child's environment — without all
/// of that, a user's own `bat` config or `BAT_OPTS` can still override row-mapping-critical
/// behavior (wrapping, blank-line squeezing) that no single competing CLI flag can force
/// back off on its own.
const FULL_FILE_VIEW_BAT_STYLE: (&str, usize) = ("numbers,grid", 1);

struct TempPreviewFile {
    path: PathBuf,
}

impl TempPreviewFile {
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempPreviewFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Builds the `bat` argument list. Alongside a forced style, three extra flags guarantee
/// full-file view's row mapping regardless of the user's own `bat` config or a `BAT_OPTS`
/// environment override:
/// - `--wrap=never` — an explicit CLI flag overrides both of those, unlike leaving `--wrap`
///   unset, so wrapped lines can't turn one raw line into several display rows.
/// - `--tabs=1` — expands every tab to exactly one space, instead of bat's default elastic
///   tab stops (width depends on the preceding column, so the same raw tab byte can expand
///   to a different number of spaces on different lines). A fixed, position-independent
///   substitution keeps each raw tab byte mapped to exactly one display byte, which full-
///   file search depends on (`App::searchable_lines_for_scope` matches the raw line; the
///   display row is what actually gets highlighted). `--tabs=0` (pass the raw byte through
///   unexpanded) would preserve that mapping too, but a literal tab byte in bat's ANSI
///   output isn't a single renderable cell — `ansi-to-tui` doesn't handle it as plain text,
///   corrupting the rest of that row's rendering.
/// - `--no-config` — bat's own cross-platform flag for ignoring both `~/.config/bat/config`
///   and `BAT_OPTS` entirely. Settings like `--squeeze-blank` have no CLI counterpart that
///   forces them back off (the only flag is the one that turns them on), so a user enabling
///   it via either source could otherwise still collapse consecutive blank lines.
///
/// `bat_env_removals` additionally strips `BAT_OPTS` from the child's environment as a
/// defense-in-depth measure, in case some `bat` version's `--no-config` only covers the
/// config file and not the environment variable.
fn bat_ansi_args(display_name: Option<&str>, forced_style: Option<(&str, usize)>) -> Vec<String> {
    let mut args = vec![
        "--paging=never".to_string(),
        "--color=always".to_string(),
        "--decorations=always".to_string(),
    ];
    if let Some((style, _)) = forced_style {
        args.push(format!("--style={}", style));
        args.push("--wrap=never".to_string());
        args.push("--tabs=1".to_string());
        args.push("--no-config".to_string());
    }
    if let Some(name) = display_name {
        args.push("--file-name".to_string());
        args.push(name.to_string());
    }
    args
}

/// Environment variables to strip from `bat`'s child process alongside a forced style —
/// see `bat_ansi_args`'s doc comment for why `BAT_OPTS` needs removing even with
/// `--no-config` also passed. A pure, deterministic function (no actual env mutation) so
/// the isolation contract can be unit-tested without touching this test process's own
/// environment, which — being process-global — a real mutation could leak into other
/// tests running concurrently on a different thread.
fn bat_env_removals(forced_style: Option<(&str, usize)>) -> &'static [&'static str] {
    if forced_style.is_some() {
        &["BAT_OPTS"]
    } else {
        &[]
    }
}

fn run_preview_commands(
    target_path: &Path,
    display_name: Option<&str>,
    repo_root: &Path,
    forced_style: Option<(&str, usize)>,
) -> Result<FilePreview> {
    let mut last_error = None;

    for program in ["bat", "cat"] {
        let mut command = Command::new(program);
        let uses_ansi = program == "bat";

        if uses_ansi {
            command.args(bat_ansi_args(display_name, forced_style));
            for var in bat_env_removals(forced_style) {
                command.env_remove(var);
            }
        }

        command.arg("--").arg(target_path).current_dir(repo_root);

        match command.output() {
            Ok(output) if output.status.success() => {
                let content_offset = if !uses_ansi {
                    0
                } else {
                    forced_style
                        .map(|(_, offset)| offset)
                        .unwrap_or(BAT_HEADER_LINE_COUNT)
                };
                return Ok(FilePreview {
                    content: String::from_utf8_lossy(&output.stdout).to_string(),
                    uses_ansi,
                    content_offset,
                });
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                last_error = Some(anyhow::anyhow!(
                    "{} preview failed: {}",
                    program,
                    stderr.trim()
                ));
            }
            Err(err) if err.kind() == ErrorKind::NotFound => continue,
            Err(err) => last_error = Some(err.into()),
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("No preview command available")))
}

fn create_temp_preview_file(path: &str, content: &str) -> Result<TempPreviewFile> {
    let file_name = Path::new(path)
        .file_name()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| std::ffi::OsStr::new("preview.txt"));
    let unique = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let temp_path = std::env::temp_dir().join(format!(
        "diffview-preview-{}-{}-{}",
        std::process::id(),
        unique,
        file_name.to_string_lossy()
    ));

    fs::write(&temp_path, content)?;

    Ok(TempPreviewFile { path: temp_path })
}

/// Raw git diff output (used for operations).
/// staged=true  → `git diff --cached -- <path>` (index vs HEAD)
/// staged=false → `git diff -- <path>` (working tree vs index)
pub fn get_raw_diff(path: &str, staged: bool, repo_root: &Path) -> Result<String> {
    let args: Vec<&str> = if staged {
        vec!["diff", "--cached", "--", path]
    } else {
        vec!["diff", "--", path]
    };
    crate::git::run_git(&args, repo_root)
}

/// Raw diff for a specific commit and path.
pub fn get_raw_commit_diff(revision: &str, path: &str, repo_root: &Path) -> Result<String> {
    crate::git::run_git(
        &["show", "--format=", "--patch", revision, "--", path],
        repo_root,
    )
}

/// Git tracks a symlink's blob content as the literal target path string (mode `120000`),
/// never the pointed-to file's own content — `fs::read`/`bat`/`cat` all transparently
/// follow a symlink instead, which would show a worktree symlink's Current full-file view
/// (or an untracked symlink's preview) as the wrong file's body, an external file outside
/// the repo, or "unavailable" for a broken link, none of which match what `git diff` or
/// `git show` display for the same path. Checked via `symlink_metadata`, which — unlike
/// `metadata`/`fs::read` — does not itself follow the link, so this works even when the
/// link is broken or points outside the repository.
fn read_symlink_target(repo_root: &Path, path: &str) -> Option<String> {
    let full_path = repo_root.join(path);
    let metadata = fs::symlink_metadata(&full_path).ok()?;
    if !metadata.file_type().is_symlink() {
        return None;
    }
    let target = fs::read_link(&full_path).ok()?;
    Some(target.to_string_lossy().into_owned())
}

/// File preview for content that `git diff` cannot render, such as untracked files.
pub fn get_file_preview(path: &str, repo_root: &Path) -> Result<FilePreview> {
    if let Some(target) = read_symlink_target(repo_root, path) {
        return Ok(FilePreview {
            content: target,
            uses_ansi: false,
            content_offset: 0,
        });
    }
    run_preview_commands(Path::new(path), None, repo_root, None)
}

/// Raw working-tree file contents, decoded lossily as UTF-8 for TUI display/copy.
pub fn get_file_content(path: &str, repo_root: &Path) -> Result<String> {
    if let Some(target) = read_symlink_target(repo_root, path) {
        return Ok(target);
    }
    let bytes = fs::read(repo_root.join(path))?;
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

/// Preview arbitrary content using the same renderer as file previews while preserving the
/// original path for syntax detection and header display.
pub fn render_content_preview(path: &str, content: &str, repo_root: &Path) -> Result<FilePreview> {
    let temp_file = create_temp_preview_file(path, content)?;
    run_preview_commands(
        temp_file.path(),
        Some(path),
        repo_root,
        Some(FULL_FILE_VIEW_BAT_STYLE),
    )
}

/// File content at an arbitrary git revision expression such as `HEAD:path` or `:path`.
pub fn get_file_content_at_rev(rev_colon_path: &str, repo_root: &Path) -> Result<String> {
    crate::git::run_git(&["show", rev_colon_path], repo_root)
}

/// Detect whether an untracked file would be shown as a binary diff.
pub fn is_binary_untracked_file(path: &str, repo_root: &Path) -> Result<bool> {
    let output = Command::new("git")
        .args(["diff", "--no-index", "--", "/dev/null", path])
        .current_dir(repo_root)
        .output()?;

    if output.status.success() || output.status.code() == Some(1) {
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout.contains("Binary files"))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(anyhow::anyhow!(
            "git diff --no-index -- /dev/null {} failed: {}",
            path,
            stderr.trim()
        ))
    }
}

/// Display diff (may be colored by delta/difftastic)
pub fn get_display_diff(
    path: &str,
    staged: bool,
    tool: &str,
    pane_width: u16,
    repo_root: &Path,
) -> Result<String> {
    match tool {
        "delta" => get_delta_diff(path, staged, pane_width, repo_root),
        "difftastic" => get_difftastic_diff(path, staged, repo_root),
        _ => get_raw_diff(path, staged, repo_root),
    }
}

/// Display diff for a specific commit and path (may be colored by delta/difftastic).
pub fn get_display_commit_diff(
    revision: &str,
    path: &str,
    tool: &str,
    pane_width: u16,
    repo_root: &Path,
) -> Result<String> {
    match tool {
        "delta" => get_delta_commit_diff(revision, path, pane_width, repo_root),
        "difftastic" => get_difftastic_commit_diff(revision, path, repo_root),
        _ => get_raw_commit_diff(revision, path, repo_root),
    }
}

fn get_delta_diff(path: &str, staged: bool, pane_width: u16, repo_root: &Path) -> Result<String> {
    let diff_args: Vec<&str> = if staged {
        vec!["diff", "--cached", "--", path]
    } else {
        vec!["diff", "--", path]
    };

    let width_str = pane_width.to_string();

    let mut git_proc = Command::new("git")
        .args(&diff_args)
        .current_dir(repo_root)
        .stdout(Stdio::piped())
        .spawn()?;

    let git_stdout = git_proc
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("Failed to capture git diff stdout"))?;

    let output = Command::new("delta")
        .args(["--width", &width_str, "--paging", "never"])
        .env("COLUMNS", &width_str)
        .stdin(git_stdout)
        .output()?;

    let _ = git_proc.wait()?;

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn get_delta_commit_diff(
    revision: &str,
    path: &str,
    pane_width: u16,
    repo_root: &Path,
) -> Result<String> {
    let diff_args: Vec<&str> = vec!["show", "--format=", "--patch", revision, "--", path];
    let width_str = pane_width.to_string();

    let mut git_proc = Command::new("git")
        .args(&diff_args)
        .current_dir(repo_root)
        .stdout(Stdio::piped())
        .spawn()?;

    let git_stdout = git_proc
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("Failed to capture git show stdout"))?;

    let output = Command::new("delta")
        .args(["--width", &width_str, "--paging", "never"])
        .env("COLUMNS", &width_str)
        .stdin(git_stdout)
        .output()?;

    let _ = git_proc.wait()?;

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn get_difftastic_diff(path: &str, staged: bool, repo_root: &Path) -> Result<String> {
    let diff_args: Vec<&str> = if staged {
        vec!["diff", "--cached", "--ext-diff", "--", path]
    } else {
        vec!["diff", "--ext-diff", "--", path]
    };

    let output = Command::new("git")
        .args(&diff_args)
        .env("GIT_EXTERNAL_DIFF", "difft")
        .current_dir(repo_root)
        .output()?;

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn get_difftastic_commit_diff(revision: &str, path: &str, repo_root: &Path) -> Result<String> {
    let output = Command::new("git")
        .args([
            "show",
            "--format=",
            "--patch",
            "--ext-diff",
            revision,
            "--",
            path,
        ])
        .env("GIT_EXTERNAL_DIFF", "difft")
        .current_dir(repo_root)
        .output()?;

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bat_ansi_args_omits_style_and_wrap_without_a_forced_style() {
        let args = bat_ansi_args(None, None);
        assert!(!args.iter().any(|a| a.starts_with("--style")));
        assert!(!args.iter().any(|a| a.starts_with("--wrap")));
        assert!(!args.iter().any(|a| a.starts_with("--tabs")));
        assert!(!args.contains(&"--no-config".to_string()));
        assert!(!args.contains(&"--file-name".to_string()));
    }

    #[test]
    fn bat_ansi_args_forces_row_mapping_flags_alongside_a_forced_style() {
        let args = bat_ansi_args(Some("src/lib.rs"), Some(("numbers,grid", 1)));
        assert!(args.contains(&"--style=numbers,grid".to_string()));
        assert!(args.contains(&"--wrap=never".to_string()));
        assert!(args.contains(&"--tabs=1".to_string()));
        assert!(args.contains(&"--no-config".to_string()));
        assert!(args.contains(&"--file-name".to_string()));
        assert!(args.contains(&"src/lib.rs".to_string()));
    }

    #[test]
    fn bat_env_removals_is_empty_without_a_forced_style() {
        assert!(bat_env_removals(None).is_empty());
    }

    #[test]
    fn bat_env_removals_strips_bat_opts_alongside_a_forced_style() {
        assert_eq!(bat_env_removals(Some(("numbers,grid", 1))), &["BAT_OPTS"]);
    }

    #[test]
    fn test_get_file_preview_reads_plain_file_contents() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "diffview-preview-{}-{}",
            std::process::id(),
            unique
        ));
        fs::create_dir_all(&dir).unwrap();

        let file = dir.join("preview.txt");
        fs::write(&file, "line 1\nline 2\n").unwrap();

        let preview = get_file_preview("preview.txt", &dir).unwrap();
        assert!(preview.content.contains("line 1"));
        assert!(preview.content.contains("line 2"));
        if preview.uses_ansi {
            assert!(preview.content.contains("\u{1b}["));
        }

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_get_file_content_reads_plain_file_contents() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "diffview-file-content-{}-{}",
            std::process::id(),
            unique
        ));
        fs::create_dir_all(&dir).unwrap();

        let file = dir.join("content.txt");
        fs::write(&file, "alpha\nbeta\n").unwrap();

        let content = get_file_content("content.txt", &dir).unwrap();
        assert_eq!(content, "alpha\nbeta\n");

        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn test_get_file_content_reads_a_symlinks_own_target_path_not_the_target_files_body() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "diffview-symlink-content-{}-{}",
            std::process::id(),
            unique
        ));
        fs::create_dir_all(&dir).unwrap();

        fs::write(dir.join("target.txt"), "target file body\n").unwrap();
        std::os::unix::fs::symlink("target.txt", dir.join("link.txt")).unwrap();

        // Git's blob content for a symlink is the literal target path string with no
        // trailing newline (mode 120000) — never the pointed-to file's own content.
        let content = get_file_content("link.txt", &dir).unwrap();
        assert_eq!(content, "target.txt");

        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn test_get_file_content_reads_a_broken_symlinks_target_path_instead_of_failing() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "diffview-broken-symlink-{}-{}",
            std::process::id(),
            unique
        ));
        fs::create_dir_all(&dir).unwrap();

        std::os::unix::fs::symlink("does-not-exist.txt", dir.join("broken.txt")).unwrap();

        let content = get_file_content("broken.txt", &dir).unwrap();
        assert_eq!(content, "does-not-exist.txt");

        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn test_get_file_preview_shows_a_symlinks_target_path_without_shelling_out() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "diffview-symlink-preview-{}-{}",
            std::process::id(),
            unique
        ));
        fs::create_dir_all(&dir).unwrap();

        fs::write(dir.join("target.txt"), "target file body\n").unwrap();
        std::os::unix::fs::symlink("target.txt", dir.join("link.txt")).unwrap();

        let preview = get_file_preview("link.txt", &dir).unwrap();
        assert_eq!(preview.content, "target.txt");
        assert!(!preview.uses_ansi);
        assert_eq!(preview.content_offset, 0);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_get_file_content_at_rev_reads_head_blob() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "diffview-content-at-rev-{}-{}",
            std::process::id(),
            unique
        ));
        fs::create_dir_all(&dir).unwrap();

        Command::new("git")
            .args(["init"])
            .current_dir(&dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Codex"])
            .current_dir(&dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "codex@example.com"])
            .current_dir(&dir)
            .output()
            .unwrap();

        fs::write(dir.join("sample.txt"), "hello\nworld\n").unwrap();
        Command::new("git")
            .args(["add", "sample.txt"])
            .current_dir(&dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(&dir)
            .output()
            .unwrap();

        let content = get_file_content_at_rev("HEAD:sample.txt", &dir).unwrap();
        assert_eq!(content, "hello\nworld\n");

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_render_content_preview_formats_inline_blob_contents() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "diffview-render-content-preview-{}-{}",
            std::process::id(),
            unique
        ));
        fs::create_dir_all(&dir).unwrap();

        let preview = render_content_preview(
            "src/sample.rs",
            "fn main() {\n    println!(\"hello\");\n}\n",
            &dir,
        )
        .unwrap();

        assert!(preview.content.contains("println!"));
        if preview.uses_ansi {
            assert!(preview.content.contains("\u{1b}["));
            // The numbered gutter uses a vertical bar to separate line numbers from content.
            assert!(preview.content.contains('│'));
            // The forced `numbers,grid` style drops bat's `File:` banner line (redundant
            // with the app's own pane border) and gives a single, fixed-size header row.
            assert!(!preview.content.contains("File:"));
            assert_eq!(preview.content_offset, 1);
        }

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_render_content_preview_offset_is_independent_of_path_length() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "diffview-render-content-preview-long-path-{}-{}",
            std::process::id(),
            unique
        ));
        fs::create_dir_all(&dir).unwrap();

        // A path long enough that bat's old `header-filename` banner would wrap onto a
        // second line under its default style — the forced `numbers,grid` style has no
        // such banner at all, so the header size can't grow with path length.
        let long_path = format!("src/{}/sample.rs", "a".repeat(200));

        let preview = render_content_preview(
            &long_path,
            "fn main() {\n    println!(\"hello\");\n}\n",
            &dir,
        )
        .unwrap();

        if preview.uses_ansi {
            assert_eq!(preview.content_offset, 1);
        }

        fs::remove_dir_all(&dir).unwrap();
    }
}
