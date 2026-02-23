use anyhow::Result;
use std::collections::HashSet;
use std::path::Path;

use super::diff::{DiffLine, Hunk};

// ─── File-level operations ─────────────────────────────────────────────────

pub fn stage_file(path: &str, repo_root: &Path) -> Result<()> {
    super::run_git(&["add", path], repo_root)?;
    Ok(())
}

/// Unstage a file from the index (restore --staged)
pub fn unstage_file(path: &str, repo_root: &Path) -> Result<()> {
    super::run_git(&["restore", "--staged", path], repo_root)?;
    Ok(())
}

// ─── Hunk-level operations ─────────────────────────────────────────────────

#[allow(dead_code)]
pub fn stage_hunk(file_path: &str, hunk: &Hunk, repo_root: &Path) -> Result<()> {
    let patch = build_hunk_patch(file_path, hunk);
    super::run_git_with_stdin(&["apply", "--cached"], &patch, repo_root)?;
    Ok(())
}

#[allow(dead_code)]
pub fn unstage_hunk(file_path: &str, hunk: &Hunk, repo_root: &Path) -> Result<()> {
    let patch = build_hunk_patch(file_path, hunk);
    super::run_git_with_stdin(&["apply", "--cached", "--reverse"], &patch, repo_root)?;
    Ok(())
}

// ─── Line-level operations ─────────────────────────────────────────────────

/// Stage selected lines within a hunk.
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

/// Unstage selected lines within a hunk.
///
/// Builds a reverse partial patch directly (not using --reverse flag)
/// because partial patch semantics require different handling for
/// selected/non-selected lines in reverse direction.
pub fn unstage_lines(
    file_path: &str,
    hunk: &Hunk,
    selected: &HashSet<usize>,
    repo_root: &Path,
) -> Result<()> {
    let patch = build_reverse_partial_patch(file_path, hunk, selected);
    super::run_git_with_stdin(&["apply", "--cached"], &patch, repo_root)?;
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

/// Build a partial patch for staging (forward direction).
///
/// Rules (matching `git add -e` semantics):
///   - Selected   `+` lines → kept as `+`
///   - Unselected `+` lines → omitted entirely (not staged)
///   - Selected   `-` lines → kept as `-`
///   - Unselected `-` lines → converted to context ` ` (file unchanged there)
///   - Context lines → always kept as ` `
///
/// Applied with: `git apply --cached`
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
            }
            DiffLine::Removed(s) => {
                if selected.contains(&i) {
                    body_lines.push(format!("-{}", s));
                    old_count += 1;
                } else {
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

/// Build a reverse partial patch for unstaging.
///
/// The input hunk comes from `git diff --cached` (HEAD vs INDEX).
/// We construct a patch that operates on the INDEX and moves selected
/// lines back toward HEAD.
///
/// Perspective: old = current INDEX (hunk.new_start), new = desired state
///
/// Rules:
///   - Context lines → context ` `  (both old/new count)
///   - Selected   `+` (Added to INDEX)   → `-` to remove from INDEX  (old_count only)
///   - Unselected `+` (Added to INDEX)   → context ` ` to keep in INDEX (both)
///   - Selected   `-` (Removed from HEAD) → `+` to restore into INDEX (new_count only)
///   - Unselected `-` (Removed from HEAD) → omitted (don't restore)
///
/// Applied with: `git apply --cached` (no --reverse flag)
fn build_reverse_partial_patch(
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
                    // Remove this line from INDEX
                    body_lines.push(format!("-{}", s));
                    old_count += 1;
                } else {
                    // Keep this line in INDEX (treat as context)
                    body_lines.push(format!(" {}", s));
                    old_count += 1;
                    new_count += 1;
                }
            }
            DiffLine::Removed(s) => {
                if selected.contains(&i) {
                    // Restore this line into INDEX
                    body_lines.push(format!("+{}", s));
                    new_count += 1;
                }
                // Non-selected: omit (don't restore)
            }
        }
    }

    let mut patch = String::new();
    patch.push_str(&format!("--- a/{}\n", file_path));
    patch.push_str(&format!("+++ b/{}\n", file_path));
    // old side = current INDEX state, so use hunk.new_start
    // new side = desired state after unstaging, so use hunk.new_start as base
    patch.push_str(&format!(
        "@@ -{},{} +{},{} @@\n",
        hunk.new_start, old_count, hunk.new_start, new_count
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
        let selected: HashSet<usize> = [2].iter().cloned().collect();
        let patch = build_partial_patch("src/foo.rs", &hunk, &selected);
        assert!(patch.contains(" old_line"));
        assert!(patch.contains("+new_line"));
        assert!(patch.contains("@@ -1,3 +1,4 @@"));
    }

    #[test]
    fn test_partial_patch_select_removed_only() {
        let hunk = make_hunk();
        let selected: HashSet<usize> = [1].iter().cloned().collect();
        let patch = build_partial_patch("src/foo.rs", &hunk, &selected);
        assert!(patch.contains("-old_line"));
        assert!(!patch.contains("+new_line"));
        assert!(patch.contains("@@ -1,3 +1,2 @@"));
    }

    /// Reverse partial patch: unstage a `+` (Added) line from INDEX.
    /// Given a staged hunk with ctx/Removed/Added/ctx, selecting the Added line
    /// should produce a patch that removes it from the INDEX.
    #[test]
    fn test_reverse_partial_patch_unstage_added() {
        let hunk = make_hunk();
        // Select index 2 = Added("new_line")
        let selected: HashSet<usize> = [2].iter().cloned().collect();
        let patch = build_reverse_partial_patch("src/foo.rs", &hunk, &selected);
        // The Added line becomes `-` (remove from INDEX)
        assert!(patch.contains("-new_line"));
        // The Removed line (index 1) is non-selected, so omitted
        assert!(!patch.contains("old_line"));
        // Context lines are present
        assert!(patch.contains(" ctx_before"));
        assert!(patch.contains(" ctx_after"));
        // old=INDEX: ctx_before + (-new_line) + ctx_after = 3 lines
        // new=desired: ctx_before + ctx_after = 2 lines
        assert!(patch.contains("@@ -1,3 +1,2 @@"));
    }

    /// Reverse partial patch: unstage a `-` (Removed) line,
    /// i.e. restore it into INDEX.
    #[test]
    fn test_reverse_partial_patch_unstage_removed() {
        let hunk = make_hunk();
        // Select index 1 = Removed("old_line")
        let selected: HashSet<usize> = [1].iter().cloned().collect();
        let patch = build_reverse_partial_patch("src/foo.rs", &hunk, &selected);
        // The Removed line becomes `+` (restore into INDEX)
        assert!(patch.contains("+old_line"));
        // The Added line (index 2) is non-selected, so kept as context
        assert!(patch.contains(" new_line"));
        // old=INDEX: ctx_before + (space)new_line + ctx_after = 3 lines
        // new=desired: ctx_before + (+old_line) + (space)new_line + ctx_after = 4 lines
        assert!(patch.contains("@@ -1,3 +1,4 @@"));
    }
}
