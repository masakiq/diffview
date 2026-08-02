use std::collections::HashMap;

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

/// Which Git object a full-file read should come from — a semantic request, not a
/// rev-spec string. `infra::git::diff::get_file_content_at_object` is the only place
/// that turns this into the `git show <rev-spec>` argument (`:path`, `HEAD:path`,
/// `<rev>:path`, `<rev>^:path`); domain code never builds that syntax itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitObjectRef {
    /// The index (staged content).
    Index,
    /// `HEAD`.
    Head,
    /// A specific commit.
    Commit(String),
    /// A commit's first parent.
    ParentOfCommit(String),
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
        object_ref: GitObjectRef,
        path: String,
        content_annotation: Option<ContentAnnotation>,
    },
}

/// Whether `source` has no content to show for `file_state` at all (as opposed to
/// content that exists but can't be displayed, like binary/unmerged — those are
/// checked elsewhere). Domain only signals *that* content is missing; the caller
/// already has `source` in hand and turns that into a message
/// (`FullFileSource::missing_message`, an app-owned UI string).
fn full_file_is_missing(file_state: FileSelectionState, source: FullFileSource) -> bool {
    matches!(
        (source, file_state.status),
        (FullFileSource::Current, 'D') | (FullFileSource::Previous, 'A' | '?')
    )
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
/// judgment over the review target, pane, file state, and requested side. No I/O and no
/// rev-spec syntax: the caller turns the returned `GitObjectRef` into an actual git
/// read (see `infra::git::diff::get_file_content`/`get_file_content_at_object`).
pub fn resolve_full_file_content_target(
    path: &str,
    pane: TreePane,
    file_state: FileSelectionState,
    source: FullFileSource,
    review_target: &ReviewTarget,
    rename_sources: &HashMap<String, String>,
) -> Result<FullFileContentTarget, ()> {
    if full_file_is_missing(file_state, source) {
        return Err(());
    }

    let content_annotation = matches!((source, file_state.status), (FullFileSource::Previous, 'D'))
        .then_some(ContentAnnotation::BeforeDelete);

    if let ReviewTarget::Commit(rev) = review_target {
        let (object_ref, resolved_path) = match source {
            FullFileSource::Current => (GitObjectRef::Commit(rev.clone()), path.to_string()),
            FullFileSource::Previous => (
                GitObjectRef::ParentOfCommit(rev.clone()),
                previous_content_path(rename_sources, path).to_string(),
            ),
        };
        return Ok(FullFileContentTarget::Revision {
            object_ref,
            path: resolved_path,
            content_annotation,
        });
    }

    match (pane, source) {
        (TreePane::Unstaged, FullFileSource::Current) => Ok(FullFileContentTarget::Worktree),
        (TreePane::Unstaged, FullFileSource::Previous) => Ok(FullFileContentTarget::Revision {
            // The index already holds a staged rename/copy under its new path (that's
            // what "staged" means), so unlike the Staged-pane/commit-target cases below,
            // no `rename_sources` lookup is needed here.
            object_ref: GitObjectRef::Index,
            path: path.to_string(),
            content_annotation,
        }),
        (TreePane::Staged, FullFileSource::Current) => Ok(FullFileContentTarget::Revision {
            object_ref: GitObjectRef::Index,
            path: path.to_string(),
            content_annotation: None,
        }),
        (TreePane::Staged, FullFileSource::Previous) => Ok(FullFileContentTarget::Revision {
            object_ref: GitObjectRef::Head,
            path: previous_content_path(rename_sources, path).to_string(),
            content_annotation,
        }),
    }
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

    // Table-driven: (target, pane, source, file status) -> expected object ref / error.
    // Covers the 4 axes resolve_full_file_content_target actually branches on: review
    // target (WorkingTree/Commit), pane (Unstaged/Staged), source (Current/Previous),
    // and file status (tracked/deleted/added/untracked).
    type Case<'a> = (
        &'a str,
        &'a ReviewTarget,
        TreePane,
        FullFileSource,
        FileSelectionState,
        Result<FullFileContentTarget, ()>,
    );

    #[test]
    fn resolves_git_object_ref_across_target_pane_source_and_file_status() {
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
                    object_ref: GitObjectRef::Index,
                    path: "f.txt".to_string(),
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
                    object_ref: GitObjectRef::Index,
                    path: "f.txt".to_string(),
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
                    object_ref: GitObjectRef::Head,
                    path: "f.txt".to_string(),
                    content_annotation: None,
                }),
            ),
            (
                "working tree / unstaged / current / deleted -> missing",
                &working_tree,
                TreePane::Unstaged,
                FullFileSource::Current,
                state('D', false),
                Err(()),
            ),
            (
                "working tree / unstaged / previous / deleted -> index blob, annotated",
                &working_tree,
                TreePane::Unstaged,
                FullFileSource::Previous,
                state('D', false),
                Ok(FullFileContentTarget::Revision {
                    object_ref: GitObjectRef::Index,
                    path: "f.txt".to_string(),
                    content_annotation: Some(ContentAnnotation::BeforeDelete),
                }),
            ),
            (
                "working tree / unstaged / previous / added -> missing",
                &working_tree,
                TreePane::Unstaged,
                FullFileSource::Previous,
                state('A', false),
                Err(()),
            ),
            (
                "working tree / unstaged / previous / untracked -> missing",
                &working_tree,
                TreePane::Unstaged,
                FullFileSource::Previous,
                state('?', true),
                Err(()),
            ),
            (
                "commit / current / modified -> commit blob",
                &commit,
                TreePane::Unstaged,
                FullFileSource::Current,
                state('M', false),
                Ok(FullFileContentTarget::Revision {
                    object_ref: GitObjectRef::Commit("deadbeef".to_string()),
                    path: "f.txt".to_string(),
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
                    object_ref: GitObjectRef::ParentOfCommit("deadbeef".to_string()),
                    path: "f.txt".to_string(),
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
                    object_ref: GitObjectRef::ParentOfCommit("deadbeef".to_string()),
                    path: "f.txt".to_string(),
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
                object_ref: GitObjectRef::Head,
                path: "old.rs".to_string(),
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
                object_ref: GitObjectRef::Index,
                path: "new.rs".to_string(),
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
                object_ref: GitObjectRef::ParentOfCommit("deadbeef".to_string()),
                path: "old.rs".to_string(),
                content_annotation: None,
            })
        );
    }
}
