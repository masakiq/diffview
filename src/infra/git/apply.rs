use anyhow::Result;
use std::collections::HashSet;
use std::path::Path;

use crate::domain::patch::{build_hunk_patch, build_partial_patch, build_reverse_partial_patch};
use crate::git::diff::Hunk;

// ─── File-level operations ─────────────────────────────────────────────────

pub fn stage_file(path: &str, repo_root: &Path) -> Result<()> {
    crate::git::run_git(&["add", path], repo_root)?;
    Ok(())
}

/// Unstage a file from the index (restore --staged)
pub fn unstage_file(path: &str, repo_root: &Path) -> Result<()> {
    crate::git::run_git(&["restore", "--staged", path], repo_root)?;
    Ok(())
}

// ─── Hunk-level operations ─────────────────────────────────────────────────

#[allow(dead_code)]
pub fn stage_hunk(file_path: &str, hunk: &Hunk, repo_root: &Path) -> Result<()> {
    let patch = build_hunk_patch(file_path, hunk);
    crate::git::run_git_with_stdin(&["apply", "--cached"], &patch, repo_root)?;
    Ok(())
}

#[allow(dead_code)]
pub fn unstage_hunk(file_path: &str, hunk: &Hunk, repo_root: &Path) -> Result<()> {
    let patch = build_hunk_patch(file_path, hunk);
    crate::git::run_git_with_stdin(&["apply", "--cached", "--reverse"], &patch, repo_root)?;
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
    crate::git::run_git_with_stdin(&["apply", "--cached"], &patch, repo_root)?;
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
    crate::git::run_git_with_stdin(&["apply", "--cached"], &patch, repo_root)?;
    Ok(())
}
