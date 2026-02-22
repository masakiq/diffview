use anyhow::Result;
use std::collections::HashSet;
use std::path::Path;

use super::diff::{DiffLine, Hunk};

// ─── File-level operations ─────────────────────────────────────────────────

pub fn stage_file(path: &str, repo_root: &Path) -> Result<()> {
    super::run_git(&["add", path], repo_root)?;
    Ok(())
}

pub fn unstage_file(path: &str, staged: bool, repo_root: &Path) -> Result<()> {
    if staged {
        // Move from index back to working tree (or discard the staged change)
        super::run_git(&["restore", "--staged", path], repo_root)?;
    } else {
        // Discard unstaged working-tree changes
        super::run_git(&["restore", path], repo_root)?;
    }
    Ok(())
}

// ─── Hunk-level operations ─────────────────────────────────────────────────

pub fn stage_hunk(file_path: &str, hunk: &Hunk, repo_root: &Path) -> Result<()> {
    let patch = build_hunk_patch(file_path, hunk);
    super::run_git_with_stdin(&["apply", "--cached"], &patch, repo_root)?;
    Ok(())
}

pub fn unstage_hunk(file_path: &str, hunk: &Hunk, repo_root: &Path) -> Result<()> {
    let patch = build_hunk_patch(file_path, hunk);
    super::run_git_with_stdin(&["apply", "--cached", "--reverse"], &patch, repo_root)?;
    Ok(())
}

// ─── Line-level operations ─────────────────────────────────────────────────

/// Stage only the selected lines within a hunk.
/// `selected` contains indices into `hunk.lines`.
pub fn stage_lines(
    file_path: &str,
    hunk: &Hunk,
    selected: &HashSet<usize>,
    repo_root: &Path,
) -> Result<()> {
    let patch = build_partial_patch(file_path, hunk, selected);
    super::run_git_with_stdin(&["apply", "--cached"], &patch, repo_root)?;
    Ok(())
}

pub fn unstage_lines(
    file_path: &str,
    hunk: &Hunk,
    selected: &HashSet<usize>,
    repo_root: &Path,
) -> Result<()> {
    let patch = build_partial_patch(file_path, hunk, selected);
    super::run_git_with_stdin(&["apply", "--cached", "--reverse"], &patch, repo_root)?;
    Ok(())
}

// ─── Patch builders ────────────────────────────────────────────────────────

fn build_hunk_patch(file_path: &str, hunk: &Hunk) -> String {
    let mut patch = String::new();
    patch.push_str(&format!("--- a/{}\n", file_path));
    patch.push_str(&format!("+++ b/{}\n", file_path));
    patch.push_str(&format!(
        "@@ -{},{} +{},{} @@\n",
        hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count
    ));
    for line in &hunk.lines {
        match line {
            DiffLine::Context(s) => patch.push_str(&format!(" {}\n", s)),
            DiffLine::Added(s)   => patch.push_str(&format!("+{}\n", s)),
            DiffLine::Removed(s) => patch.push_str(&format!("-{}\n", s)),
        }
    }
    patch
}

/// Build a partial patch that only includes selected +/- lines.
///
/// Rules (matching git add -e semantics):
///   - Selected   `+` lines → kept as `+`
///   - Unselected `+` lines → omitted entirely (not staged)
///   - Selected   `-` lines → kept as `-`
///   - Unselected `-` lines → converted to context ` ` (file unchanged there)
///   - Context lines → always kept as ` `
/// The @@ header counts are recalculated accordingly.
fn build_partial_patch(
    file_path: &str,
    hunk: &Hunk,
    selected: &HashSet<usize>,
) -> String {
    let mut body_lines: Vec<String> = Vec::new();
    let mut old_count: u32 = 0;
    let mut new_count: u32 = 0;

    for (i, line) in hunk.lines.iter().enumerate() {
        match line {
            DiffLine::Context(s) => {
                body_lines.push(format!(" {}", s));
                old_count += 1;
                new_count += 1;
            }
            DiffLine::Added(s) => {
                if selected.contains(&i) {
                    body_lines.push(format!("+{}", s));
                    new_count += 1;
                }
                // unselected Added: omit — not added to index
            }
            DiffLine::Removed(s) => {
                if selected.contains(&i) {
                    body_lines.push(format!("-{}", s));
                    old_count += 1;
                } else {
                    // Treat as context so the file is unchanged there
                    body_lines.push(format!(" {}", s));
                    old_count += 1;
                    new_count += 1;
                }
            }
        }
    }

    let mut patch = String::new();
    patch.push_str(&format!("--- a/{}\n", file_path));
    patch.push_str(&format!("+++ b/{}\n", file_path));
    patch.push_str(&format!(
        "@@ -{},{} +{},{} @@\n",
        hunk.old_start, old_count, hunk.new_start, new_count
    ));
    for line in &body_lines {
        patch.push_str(line);
        patch.push('\n');
    }
    patch
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::diff::{DiffLine, Hunk};

    fn make_hunk() -> Hunk {
        Hunk {
            header: "@@ -1,4 +1,4 @@".to_string(),
            old_start: 1,
            old_count: 4,
            new_start: 1,
            new_count: 4,
            lines: vec![
                DiffLine::Context("ctx_before".to_string()),
                DiffLine::Removed("old_line".to_string()),
                DiffLine::Added("new_line".to_string()),
                DiffLine::Context("ctx_after".to_string()),
            ],
        }
    }

    #[test]
    fn test_full_hunk_patch() {
        let hunk = make_hunk();
        let patch = build_hunk_patch("src/foo.rs", &hunk);
        assert!(patch.contains("--- a/src/foo.rs"));
        assert!(patch.contains("+++ b/src/foo.rs"));
        assert!(patch.contains("@@ -1,4 +1,4 @@"));
        assert!(patch.contains("-old_line"));
        assert!(patch.contains("+new_line"));
    }

    #[test]
    fn test_partial_patch_select_added_only() {
        let hunk = make_hunk();
        // Select the Added line (index 2) but not the Removed (index 1)
        let selected: HashSet<usize> = [2].iter().cloned().collect();
        let patch = build_partial_patch("src/foo.rs", &hunk, &selected);
        // Removed line becomes context (file unchanged there)
        assert!(patch.contains(" old_line"));
        // Added line is kept
        assert!(patch.contains("+new_line"));
        // old: ctx(1) + context-from-removed(1) + ctx(1) = 3
        // new: ctx(1) + context-from-removed(1) + added(1) + ctx(1) = 4
        assert!(patch.contains("@@ -1,3 +1,4 @@"));
    }

    #[test]
    fn test_partial_patch_select_removed_only() {
        let hunk = make_hunk();
        // Select only the Removed line (index 1)
        let selected: HashSet<usize> = [1].iter().cloned().collect();
        let patch = build_partial_patch("src/foo.rs", &hunk, &selected);
        // Removed kept
        assert!(patch.contains("-old_line"));
        // Added omitted, new_count = 3 (ctx + ctx)
        assert!(!patch.contains("+new_line"));
        // @@ -1,4 +1,3 @@ (old: ctx+removed+ctx=4? wait: ctx(1)+removed(1)+ctx(1)=3 old, new: ctx+ctx=2)
        // Actually: old_count = ctx(1)+removed(1)+ctx(1)=3, new_count = ctx(1)+ctx(1)=2
        assert!(patch.contains("@@ -1,3 +1,2 @@"));
    }
}
