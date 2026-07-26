use anyhow::Result;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct GitFile {
    pub path: String,
    /// The pre-rename/copy path, when git reports one (status `R`/`C`) — `None` otherwise.
    /// A rename/copy's `path` is always the *new* path; the tree/HEAD blob a `Previous`
    /// full-file view needs to look up lives at this old path instead.
    pub previous_path: Option<String>,
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
    let output = super::run_git(
        &["status", "--porcelain", "--untracked-files=all"],
        repo_root,
    )?;
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

        // Handle renamed/copied files "old -> new" — quotes git might add around either
        // side for special filenames (e.g. ones containing a space) are stripped from both.
        let (path, previous_path) = if let Some((old, new)) = rest.split_once(" -> ") {
            (
                new.trim_matches('"').to_string(),
                Some(old.trim_matches('"').to_string()),
            )
        } else {
            (rest.trim_matches('"').to_string(), None)
        };

        if !path.is_empty() {
            files.push(GitFile {
                path,
                previous_path,
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
        // Rename/copy format: R100\told\tnew, C100\told\tnew
        let (path, previous_path) = match status {
            'R' | 'C' => (
                parts.get(2).copied().unwrap_or(parts[1]),
                Some(parts[1].to_string()),
            ),
            _ => (parts[1], None),
        };

        if !path.is_empty() {
            files.push(GitFile {
                path: path.to_string(),
                previous_path,
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
        assert_eq!(files[0].previous_path.as_deref(), Some("old.rs"));
    }

    #[test]
    fn test_renamed_file_with_a_space_in_the_old_path_is_quoted_and_unquoted() {
        let input = "R  \"old with space.rs\" -> new.rs\n";
        let files = parse_status(input);
        assert_eq!(files[0].path, "new.rs");
        assert_eq!(files[0].previous_path.as_deref(), Some("old with space.rs"));
    }

    #[test]
    fn test_non_renamed_file_has_no_previous_path() {
        let input = " M src/main.rs\n";
        let files = parse_status(input);
        assert_eq!(files[0].previous_path, None);
    }

    #[test]
    fn test_parse_porcelain_with_untracked_files_all() {
        let input = "?? hoge/a.txt\n?? hoge/nested/b.txt\n";
        let files = parse_status(input);

        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "hoge/a.txt");
        assert!(files[0].is_untracked());
        assert_eq!(files[1].path, "hoge/nested/b.txt");
        assert!(files[1].is_untracked());
    }

    #[test]
    fn test_parse_porcelain_mixed_untracked_and_tracked_entries() {
        let input = " M src/main.rs\n?? newdir/file1.txt\n?? newdir/sub/file2.txt\nA  staged.rs\n";
        let files = parse_status(input);

        assert_eq!(files.len(), 4);
        assert_eq!(files[0].path, "src/main.rs");
        assert!(!files[0].is_untracked());
        assert_eq!(files[1].path, "newdir/file1.txt");
        assert!(files[1].is_untracked());
        assert_eq!(files[2].path, "newdir/sub/file2.txt");
        assert!(files[2].is_untracked());
        assert_eq!(files[3].path, "staged.rs");
        assert!(!files[3].is_untracked());
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
        assert_eq!(files[0].previous_path.as_deref(), Some("src/old.rs"));
        assert_eq!(files[0].unstaged, 'R');
    }

    #[test]
    fn test_parse_commit_name_status_non_rename_has_no_previous_path() {
        let input = "M\tsrc/main.rs\n";
        let files = parse_commit_name_status(input);
        assert_eq!(files[0].previous_path, None);
    }
}
