use anyhow::Result;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub enum DiffLine {
    Context(String),
    Added(String),
    Removed(String),
}

#[derive(Debug, Clone)]
pub struct Hunk {
    #[allow(dead_code)]
    pub header: String,
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, Default)]
pub struct FileDiff {
    #[allow(dead_code)]
    pub path: String,
    pub is_binary: bool,
    pub hunks: Vec<Hunk>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePreview {
    pub content: String,
    pub uses_ansi: bool,
}

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

fn run_preview_commands(
    target_path: &Path,
    display_name: Option<&str>,
    repo_root: &Path,
    show_line_numbers: bool,
) -> Result<FilePreview> {
    let mut last_error = None;

    for program in ["bat", "cat"] {
        let mut command = Command::new(program);
        let uses_ansi = program == "bat";

        if uses_ansi {
            command.args(["--paging=never", "--color=always", "--decorations=always"]);
            if !show_line_numbers {
                command.arg("--style=changes,grid,header-filename,snip");
            }
            if let Some(display_name) = display_name {
                command.args(["--file-name", display_name]);
            }
        }

        command.arg("--").arg(target_path).current_dir(repo_root);

        match command.output() {
            Ok(output) if output.status.success() => {
                return Ok(FilePreview {
                    content: String::from_utf8_lossy(&output.stdout).to_string(),
                    uses_ansi,
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
    super::run_git(&args, repo_root)
}

/// Raw diff for a specific commit and path.
pub fn get_raw_commit_diff(revision: &str, path: &str, repo_root: &Path) -> Result<String> {
    super::run_git(
        &["show", "--format=", "--patch", revision, "--", path],
        repo_root,
    )
}

/// File preview for content that `git diff` cannot render, such as untracked files.
pub fn get_file_preview(path: &str, repo_root: &Path) -> Result<FilePreview> {
    run_preview_commands(Path::new(path), None, repo_root, true)
}

/// Raw working-tree file contents, decoded lossily as UTF-8 for TUI display/copy.
pub fn get_file_content(path: &str, repo_root: &Path) -> Result<String> {
    let bytes = fs::read(repo_root.join(path))?;
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

/// Preview arbitrary content using the same renderer as file previews while preserving the
/// original path for syntax detection and header display.
pub fn render_content_preview(
    path: &str,
    content: &str,
    repo_root: &Path,
    show_line_numbers: bool,
) -> Result<FilePreview> {
    let temp_file = create_temp_preview_file(path, content)?;
    run_preview_commands(temp_file.path(), Some(path), repo_root, show_line_numbers)
}

/// File content at an arbitrary git revision expression such as `HEAD:path` or `:path`.
pub fn get_file_content_at_rev(rev_colon_path: &str, repo_root: &Path) -> Result<String> {
    super::run_git(&["show", rev_colon_path], repo_root)
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

fn is_binary_diff_text(diff_text: &str) -> bool {
    diff_text.lines().any(|line| {
        (line.starts_with("Binary files ") && line.ends_with(" differ"))
            || line == "GIT binary patch"
    })
}

pub fn parse_diff(diff_text: &str) -> FileDiff {
    if is_binary_diff_text(diff_text) {
        return FileDiff {
            path: String::new(),
            is_binary: true,
            hunks: vec![],
        };
    }

    let mut hunks: Vec<Hunk> = Vec::new();
    let mut current_hunk: Option<Hunk> = None;
    let mut path = String::new();

    for line in diff_text.lines() {
        if line.starts_with("+++ b/") {
            path = line[6..].to_string();
        } else if line.starts_with("@@") {
            if let Some(hunk) = current_hunk.take() {
                hunks.push(hunk);
            }
            if let Some(hunk) = parse_hunk_header(line) {
                current_hunk = Some(hunk);
            }
        } else if let Some(ref mut hunk) = current_hunk {
            if line.starts_with('+') {
                hunk.lines.push(DiffLine::Added(line[1..].to_string()));
            } else if line.starts_with('-') {
                hunk.lines.push(DiffLine::Removed(line[1..].to_string()));
            } else if line.starts_with(' ') {
                hunk.lines.push(DiffLine::Context(line[1..].to_string()));
            }
        }
    }

    if let Some(hunk) = current_hunk {
        hunks.push(hunk);
    }

    FileDiff {
        path,
        is_binary: false,
        hunks,
    }
}

fn parse_hunk_header(line: &str) -> Option<Hunk> {
    let parts: Vec<&str> = line.splitn(5, ' ').collect();
    if parts.len() < 3 {
        return None;
    }

    let old = parts[1].trim_start_matches('-');
    let new = parts[2].trim_start_matches('+');

    let (old_start, old_count) = parse_range(old);
    let (new_start, new_count) = parse_range(new);

    Some(Hunk {
        header: line.to_string(),
        old_start,
        old_count,
        new_start,
        new_count,
        lines: Vec::new(),
    })
}

fn parse_range(s: &str) -> (u32, u32) {
    if let Some((start, count)) = s.split_once(',') {
        (start.parse().unwrap_or(1), count.parse().unwrap_or(0))
    } else {
        (s.parse().unwrap_or(1), 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    const SAMPLE_DIFF: &str = r#"diff --git a/src/main.rs b/src/main.rs
index abc..def 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,5 +1,6 @@
 fn main() {
-    println!("hello");
+    println!("hello, world");
+    println!("second line");
 }
"#;

    #[test]
    fn test_parse_hunk() {
        let fd = parse_diff(SAMPLE_DIFF);
        assert_eq!(fd.hunks.len(), 1);
        let hunk = &fd.hunks[0];
        assert_eq!(hunk.old_start, 1);
        assert_eq!(hunk.old_count, 5);
        assert_eq!(hunk.new_start, 1);
        assert_eq!(hunk.new_count, 6);
        assert_eq!(hunk.lines.len(), 5);
        assert!(matches!(hunk.lines[0], DiffLine::Context(_)));
        assert!(matches!(hunk.lines[1], DiffLine::Removed(_)));
        assert!(matches!(hunk.lines[2], DiffLine::Added(_)));
        assert!(matches!(hunk.lines[3], DiffLine::Added(_)));
        assert!(matches!(hunk.lines[4], DiffLine::Context(_)));
    }

    #[test]
    fn test_binary_detection() {
        let fd = parse_diff("Binary files a/img.png and b/img.png differ\n");
        assert!(fd.is_binary);
        assert!(fd.hunks.is_empty());
    }

    #[test]
    fn test_binary_detection_git_binary_patch_format() {
        let diff =
            "diff --git a/img.png b/img.png\nindex abc..def 100644\nGIT binary patch\nliteral 123\nabc\n";
        let fd = parse_diff(diff);
        assert!(fd.is_binary);
        assert!(fd.hunks.is_empty());
    }

    #[test]
    fn test_patch_containing_binary_files_literal_is_not_binary() {
        let diff = r#"diff --git a/src/lib.rs b/src/lib.rs
index abc..def 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1 +1 @@
-fn before() {}
+fn after() { println!("Binary files"); }
"#;

        let fd = parse_diff(diff);
        assert!(!fd.is_binary);
        assert_eq!(fd.hunks.len(), 1);
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
            true,
        )
        .unwrap();

        assert!(preview.content.contains("println!"));
        if preview.uses_ansi {
            assert!(preview.content.contains("\u{1b}["));
            assert!(preview.content.contains("src/sample.rs"));
            // The numbered gutter uses a vertical bar to separate line numbers from content.
            assert!(preview.content.contains('│'));
        }

        let preview_without_numbers = render_content_preview(
            "src/sample.rs",
            "fn main() {\n    println!(\"hello\");\n}\n",
            &dir,
            false,
        )
        .unwrap();

        assert!(preview_without_numbers.content.contains("println!"));
        if preview_without_numbers.uses_ansi {
            assert!(!preview_without_numbers.content.contains('│'));
        }

        fs::remove_dir_all(&dir).unwrap();
    }
}
