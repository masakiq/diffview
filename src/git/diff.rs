use anyhow::Result;
use std::path::Path;
use std::process::{Command, Stdio};

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

    let git_proc = Command::new("git")
        .args(&diff_args)
        .current_dir(repo_root)
        .stdout(Stdio::piped())
        .spawn()?;

    let output = Command::new("delta")
        .args(["--width", &width_str, "--paging", "never"])
        .env("COLUMNS", &width_str)
        .stdin(git_proc.stdout.unwrap())
        .output()?;

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

    let git_proc = Command::new("git")
        .args(&diff_args)
        .current_dir(repo_root)
        .stdout(Stdio::piped())
        .spawn()?;

    let output = Command::new("delta")
        .args(["--width", &width_str, "--paging", "never"])
        .env("COLUMNS", &width_str)
        .stdin(git_proc.stdout.unwrap())
        .output()?;

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

pub fn parse_diff(diff_text: &str) -> FileDiff {
    if diff_text.contains("Binary files") {
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
}
