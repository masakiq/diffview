use std::collections::HashMap;
use std::path::Path;

use crate::domain::review_target::ReviewTarget;

/// Which side of a diff a full-file view request is for. Data-only here; the
/// UI-facing methods (`title_label`/`status_message`/`missing_message` callers'
/// formatting, etc.) stay on the `impl` in `app/mod.rs` — inherent impls don't need to
/// live in the same module as the type, just the same crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FullFileSource {
    Current,
    Previous,
}

/// Which working-tree tree section a file belongs to. Data-only here for the same
/// reason as `FullFileSource` — `impl TreePane` (which returns `app::Focus`) stays in
/// `app/mod.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TreePane {
    Unstaged,
    Staged,
}

/// The minimal per-node fact `has_untracked_file` needs. Deliberately not `TreeNode`
/// (a UI/render concept — depth, expansion, display name — owned by `app`), so this
/// module has no dependency on it; callers convert their `TreeNode`s at the call site.
#[derive(Debug, Clone, Copy)]
pub struct NodeTrackingState<'a> {
    pub path: &'a Path,
    pub is_dir: bool,
    pub is_untracked: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentAnnotation {
    BeforeDelete,
    BinaryUnavailable,
    UnmergedUnavailable,
}

impl ContentAnnotation {
    pub fn title_label(self) -> &'static str {
        match self {
            ContentAnnotation::BeforeDelete => "file:before-delete",
            ContentAnnotation::BinaryUnavailable => "binary",
            ContentAnnotation::UnmergedUnavailable => "unmerged",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileSelectionState {
    pub status: char,
    pub is_unmerged: bool,
    pub is_untracked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FullFileContentTarget {
    Worktree,
    Revision {
        rev_spec: String,
        content_annotation: Option<ContentAnnotation>,
    },
}

pub fn full_file_missing_message(
    file_state: FileSelectionState,
    source: FullFileSource,
) -> Option<&'static str> {
    match (source, file_state.status) {
        (FullFileSource::Current, 'D') => Some(FullFileSource::Current.missing_message()),
        (FullFileSource::Previous, 'A' | '?') => Some(FullFileSource::Previous.missing_message()),
        _ => None,
    }
}

/// `path` as it existed before a staged/committed rename or copy, or `path` itself when
/// there was none. A rename/copy's `path` is always the new one, but `HEAD`/`<rev>^`
/// only ever have the file at its old path — `resolve_full_file_content_target`'s two
/// `Previous` branches that read from `HEAD`/`<rev>^` (not the index or worktree, which
/// already reflect the rename) need this instead of `path` directly.
fn previous_content_path<'a>(
    rename_sources: &'a HashMap<String, String>,
    path: &'a str,
) -> &'a str {
    rename_sources.get(path).map(String::as_str).unwrap_or(path)
}

/// Decides which git object a full-file view request resolves to — a pure policy
/// judgment over the review target, pane, file state, and requested side. No I/O: the
/// caller turns the returned `rev_spec` into an actual git read (see
/// `infra::git::diff::get_file_content`/`get_file_content_at_rev`).
pub fn resolve_full_file_content_target(
    path: &str,
    pane: TreePane,
    file_state: FileSelectionState,
    source: FullFileSource,
    review_target: &ReviewTarget,
    rename_sources: &HashMap<String, String>,
) -> Result<FullFileContentTarget, &'static str> {
    if let Some(message) = full_file_missing_message(file_state, source) {
        return Err(message);
    }

    let content_annotation = matches!((source, file_state.status), (FullFileSource::Previous, 'D'))
        .then_some(ContentAnnotation::BeforeDelete);

    if let ReviewTarget::Commit(rev) = review_target {
        let rev_spec = match source {
            FullFileSource::Current => format!("{}:{}", rev, path),
            FullFileSource::Previous => {
                format!("{}^:{}", rev, previous_content_path(rename_sources, path))
            }
        };
        return Ok(FullFileContentTarget::Revision {
            rev_spec,
            content_annotation,
        });
    }

    match (pane, source) {
        (TreePane::Unstaged, FullFileSource::Current) => Ok(FullFileContentTarget::Worktree),
        (TreePane::Unstaged, FullFileSource::Previous) => Ok(FullFileContentTarget::Revision {
            // The index already holds a staged rename/copy under its new path (that's
            // what "staged" means), so unlike the Staged-pane/commit-target cases below,
            // no `rename_sources` lookup is needed here.
            rev_spec: format!(":{}", path),
            content_annotation,
        }),
        (TreePane::Staged, FullFileSource::Current) => Ok(FullFileContentTarget::Revision {
            rev_spec: format!(":{}", path),
            content_annotation: None,
        }),
        (TreePane::Staged, FullFileSource::Previous) => Ok(FullFileContentTarget::Revision {
            rev_spec: format!("HEAD:{}", previous_content_path(rename_sources, path)),
            content_annotation,
        }),
    }
}

/// Whether `path` is an untracked node among `nodes` — the tree-search half of
/// `has_untracked_file_in_pane`; the caller still applies the Commit-target short
/// circuit (an untracked node never appears in that tree in the first place, but the
/// check is cheap insurance and keeps the "no untracked files under Commit" invariant
/// visible at the call site rather than buried in here).
pub fn has_untracked_file<'a>(
    nodes: impl IntoIterator<Item = NodeTrackingState<'a>>,
    path: &str,
) -> bool {
    nodes
        .into_iter()
        .any(|n| !n.is_dir && n.path == Path::new(path) && n.is_untracked)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(status: char, is_untracked: bool) -> FileSelectionState {
        FileSelectionState {
            status,
            is_unmerged: false,
            is_untracked,
        }
    }

    // Table-driven: (target, pane, source, file status) -> expected rev_spec / error.
    // Covers the 4 axes resolve_full_file_content_target actually branches on: review
    // target (WorkingTree/Commit), pane (Unstaged/Staged), source (Current/Previous),
    // and file status (tracked/deleted/added/untracked).
    type Case<'a> = (
        &'a str,
        &'a ReviewTarget,
        TreePane,
        FullFileSource,
        FileSelectionState,
        Result<FullFileContentTarget, &'static str>,
    );

    #[test]
    fn resolves_rev_spec_across_target_pane_source_and_file_status() {
        let no_renames = HashMap::new();
        let working_tree = ReviewTarget::WorkingTree;
        let commit = ReviewTarget::Commit("deadbeef".to_string());

        let cases: Vec<Case> = vec![
            (
                "working tree / unstaged / current / modified -> worktree file",
                &working_tree,
                TreePane::Unstaged,
                FullFileSource::Current,
                state('M', false),
                Ok(FullFileContentTarget::Worktree),
            ),
            (
                "working tree / unstaged / previous / modified -> index blob",
                &working_tree,
                TreePane::Unstaged,
                FullFileSource::Previous,
                state('M', false),
                Ok(FullFileContentTarget::Revision {
                    rev_spec: ":f.txt".to_string(),
                    content_annotation: None,
                }),
            ),
            (
                "working tree / staged / current / modified -> index blob",
                &working_tree,
                TreePane::Staged,
                FullFileSource::Current,
                state('M', false),
                Ok(FullFileContentTarget::Revision {
                    rev_spec: ":f.txt".to_string(),
                    content_annotation: None,
                }),
            ),
            (
                "working tree / staged / previous / modified -> HEAD blob",
                &working_tree,
                TreePane::Staged,
                FullFileSource::Previous,
                state('M', false),
                Ok(FullFileContentTarget::Revision {
                    rev_spec: "HEAD:f.txt".to_string(),
                    content_annotation: None,
                }),
            ),
            (
                "working tree / unstaged / current / deleted -> missing message",
                &working_tree,
                TreePane::Unstaged,
                FullFileSource::Current,
                state('D', false),
                Err(FullFileSource::Current.missing_message()),
            ),
            (
                "working tree / unstaged / previous / deleted -> index blob, annotated",
                &working_tree,
                TreePane::Unstaged,
                FullFileSource::Previous,
                state('D', false),
                Ok(FullFileContentTarget::Revision {
                    rev_spec: ":f.txt".to_string(),
                    content_annotation: Some(ContentAnnotation::BeforeDelete),
                }),
            ),
            (
                "working tree / unstaged / previous / added -> missing message",
                &working_tree,
                TreePane::Unstaged,
                FullFileSource::Previous,
                state('A', false),
                Err(FullFileSource::Previous.missing_message()),
            ),
            (
                "working tree / unstaged / previous / untracked -> missing message",
                &working_tree,
                TreePane::Unstaged,
                FullFileSource::Previous,
                state('?', true),
                Err(FullFileSource::Previous.missing_message()),
            ),
            (
                "commit / current / modified -> commit blob",
                &commit,
                TreePane::Unstaged,
                FullFileSource::Current,
                state('M', false),
                Ok(FullFileContentTarget::Revision {
                    rev_spec: "deadbeef:f.txt".to_string(),
                    content_annotation: None,
                }),
            ),
            (
                "commit / previous / modified -> parent blob",
                &commit,
                TreePane::Unstaged,
                FullFileSource::Previous,
                state('M', false),
                Ok(FullFileContentTarget::Revision {
                    rev_spec: "deadbeef^:f.txt".to_string(),
                    content_annotation: None,
                }),
            ),
            (
                "commit / previous / deleted -> parent blob, annotated",
                &commit,
                TreePane::Unstaged,
                FullFileSource::Previous,
                state('D', false),
                Ok(FullFileContentTarget::Revision {
                    rev_spec: "deadbeef^:f.txt".to_string(),
                    content_annotation: Some(ContentAnnotation::BeforeDelete),
                }),
            ),
        ];

        for (label, target, pane, source, file_state, expected) in cases {
            let actual = resolve_full_file_content_target(
                "f.txt",
                pane,
                file_state,
                source,
                target,
                &no_renames,
            );
            assert_eq!(actual, expected, "case: {label}");
        }
    }

    #[test]
    fn uses_previous_path_for_renamed_files() {
        let mut renames = HashMap::new();
        renames.insert("new.rs".to_string(), "old.rs".to_string());
        let renamed = state('R', false);

        // Staged/Previous reads from HEAD, which only has the file under its old path.
        assert_eq!(
            resolve_full_file_content_target(
                "new.rs",
                TreePane::Staged,
                renamed,
                FullFileSource::Previous,
                &ReviewTarget::WorkingTree,
                &renames,
            ),
            Ok(FullFileContentTarget::Revision {
                rev_spec: "HEAD:old.rs".to_string(),
                content_annotation: None,
            })
        );

        // Unstaged/Previous reads from the index, which already has the rename staged
        // under the new path — no rename_sources lookup needed, unlike the Staged case.
        assert_eq!(
            resolve_full_file_content_target(
                "new.rs",
                TreePane::Unstaged,
                renamed,
                FullFileSource::Previous,
                &ReviewTarget::WorkingTree,
                &renames,
            ),
            Ok(FullFileContentTarget::Revision {
                rev_spec: ":new.rs".to_string(),
                content_annotation: None,
            })
        );

        // The Commit target's Previous reads from the parent commit, which also only has
        // the file under its old path.
        assert_eq!(
            resolve_full_file_content_target(
                "new.rs",
                TreePane::Unstaged,
                renamed,
                FullFileSource::Previous,
                &ReviewTarget::Commit("deadbeef".to_string()),
                &renames,
            ),
            Ok(FullFileContentTarget::Revision {
                rev_spec: "deadbeef^:old.rs".to_string(),
                content_annotation: None,
            })
        );
    }

    #[test]
    fn has_untracked_file_matches_only_untracked_non_directory_nodes_at_the_given_path() {
        let nodes = [
            NodeTrackingState {
                path: Path::new("a.txt"),
                is_dir: false,
                is_untracked: true,
            },
            NodeTrackingState {
                path: Path::new("dir"),
                is_dir: true,
                is_untracked: true,
            },
            NodeTrackingState {
                path: Path::new("tracked.txt"),
                is_dir: false,
                is_untracked: false,
            },
        ];

        assert!(has_untracked_file(nodes, "a.txt"));
        assert!(!has_untracked_file(nodes, "dir"));
        assert!(!has_untracked_file(nodes, "tracked.txt"));
        assert!(!has_untracked_file(nodes, "missing.txt"));
    }
}
