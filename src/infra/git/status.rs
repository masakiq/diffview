use anyhow::Result;
use std::path::Path;

use crate::domain::status::{parse_commit_name_status, parse_status, GitFile};

pub fn get_status(repo_root: &Path) -> Result<Vec<GitFile>> {
    let output = crate::git::run_git(
        &["status", "--porcelain", "--untracked-files=all"],
        repo_root,
    )?;
    Ok(parse_status(&output))
}

pub fn get_commit_files(revision: &str, repo_root: &Path) -> Result<Vec<GitFile>> {
    let output = crate::git::run_git(
        &[
            "show",
            "--format=",
            "--name-status",
            "--find-renames",
            revision,
        ],
        repo_root,
    )?;
    Ok(parse_commit_name_status(&output))
}
