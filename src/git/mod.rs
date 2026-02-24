use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

pub mod apply;
pub mod diff;
pub mod status;

pub fn run_git(args: &[&str], cwd: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("Failed to run: git {}", args.join(" ")))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        Err(anyhow::anyhow!(
            "git {} failed: {}",
            args.join(" "),
            stderr.trim()
        ))
    }
}

pub fn run_git_with_stdin(args: &[&str], stdin_data: &str, cwd: &Path) -> Result<String> {
    use std::io::Write;
    use std::process::Stdio;

    let mut child = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to spawn: git {}", args.join(" ")))?;

    if let Some(mut pipe) = child.stdin.take() {
        pipe.write_all(stdin_data.as_bytes())?;
    }

    let output = child.wait_with_output()?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        Err(anyhow::anyhow!(
            "git {} failed: {}",
            args.join(" "),
            stderr.trim()
        ))
    }
}

pub fn get_repo_root() -> Result<std::path::PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("Failed to run git rev-parse")?;

    if output.status.success() {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(std::path::PathBuf::from(path))
    } else {
        Err(anyhow::anyhow!(
            "Not in a git repository. Run diffview from inside a git repo."
        ))
    }
}

pub fn resolve_commit(revision: &str, repo_root: &Path) -> Result<String> {
    let rev_expr = format!("{}^{{commit}}", revision);
    let output = run_git(&["rev-parse", "--verify", &rev_expr], repo_root)?;
    Ok(output.trim().to_string())
}
