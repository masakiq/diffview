use anyhow::Result;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct GitFile {
    pub path: String,
    pub staged: char,
    pub unstaged: char,
}

impl GitFile {
    #[allow(dead_code)]
    pub fn is_untracked(&self) -> bool {
        self.staged == '?' && self.unstaged == '?'
    }

    #[allow(dead_code)]
    pub fn is_unmerged(&self) -> bool {
        matches!(
            (self.staged, self.unstaged),
            ('U', _) | (_, 'U') | ('A', 'A') | ('D', 'D')
        )
    }

    #[allow(dead_code)]
    pub fn display_status(&self) -> String {
        format!("{}{}", self.staged, self.unstaged)
    }

    #[allow(dead_code)]
    pub fn short_status(&self) -> char {
        if self.staged != ' ' && self.staged != '?' {
            self.staged
        } else {
            self.unstaged
        }
    }
}

pub fn get_status(repo_root: &Path) -> Result<Vec<GitFile>> {
    let output = super::run_git(&["status", "--porcelain"], repo_root)?;
    Ok(parse_status(&output))
}

pub fn get_commit_files(revision: &str, repo_root: &Path) -> Result<Vec<GitFile>> {
    let output = super::run_git(
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

pub fn parse_status(output: &str) -> Vec<GitFile> {
    let mut files = Vec::new();

    for line in output.lines() {
        if line.len() < 3 {
            continue;
        }

        let staged = line.chars().next().unwrap_or(' ');
        let unstaged = line.chars().nth(1).unwrap_or(' ');
        let rest = &line[3..];

        // Handle renamed files "old -> new"
        let path = if rest.contains(" -> ") {
            rest.split(" -> ").last().unwrap_or(rest).to_string()
        } else {
            rest.to_string()
        };

        // Strip quotes git might add for special filenames
        let path = path.trim_matches('"').to_string();

        if !path.is_empty() {
            files.push(GitFile {
                path,
                staged,
                unstaged,
            });
        }
    }

    files
}

pub fn parse_commit_name_status(output: &str) -> Vec<GitFile> {
    let mut files = Vec::new();

    for line in output.lines() {
        if line.trim().is_empty() {
            continue;
        }

        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 2 {
            continue;
        }

        let status_token = parts[0];
        let status = status_token.chars().next().unwrap_or(' ');
        let path = match status {
            // Rename/copy format: R100\told\tnew, C100\told\tnew
            'R' | 'C' => parts.get(2).copied().unwrap_or(parts[1]),
            _ => parts[1],
        };

        if !path.is_empty() {
            files.push(GitFile {
                path: path.to_string(),
                staged: status,
                unstaged: status,
            });
        }
    }

    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_porcelain() {
        let input = " M src/main.rs\nMM src/app.rs\n?? tmp/new.txt\nA  added.rs\n";
        let files = parse_status(input);
        assert_eq!(files.len(), 4);
        assert_eq!(files[0].path, "src/main.rs");
        assert_eq!(files[0].staged, ' ');
        assert_eq!(files[0].unstaged, 'M');
        assert_eq!(files[1].staged, 'M');
        assert_eq!(files[1].unstaged, 'M');
        assert!(files[2].is_untracked());
        assert_eq!(files[3].staged, 'A');
    }

    #[test]
    fn test_renamed_file() {
        let input = "R  old.rs -> new.rs\n";
        let files = parse_status(input);
        assert_eq!(files[0].path, "new.rs");
    }

    #[test]
    fn test_parse_commit_name_status_basic() {
        let input = "M\tsrc/main.rs\nA\tsrc/new.rs\nD\tsrc/old.rs\n";
        let files = parse_commit_name_status(input);
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].path, "src/main.rs");
        assert_eq!(files[0].unstaged, 'M');
        assert_eq!(files[1].unstaged, 'A');
        assert_eq!(files[2].unstaged, 'D');
    }

    #[test]
    fn test_parse_commit_name_status_rename() {
        let input = "R100\tsrc/old.rs\tsrc/new.rs\n";
        let files = parse_commit_name_status(input);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/new.rs");
        assert_eq!(files[0].unstaged, 'R');
    }
}
