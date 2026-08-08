/// What diffview is browsing: the working tree/index, or a specific commit.
/// Decided once at startup from the CLI revision argument and never changes
/// during a run (`domain/content.rs` and friends read it, they don't set it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewTarget {
    WorkingTree,
    Commit(String),
}

impl ReviewTarget {
    pub fn is_commit(&self) -> bool {
        matches!(self, ReviewTarget::Commit(_))
    }

    /// The resolved commit-ish, or `None` under `WorkingTree`. A convenience for call
    /// sites (cache keys, `git show`/`git diff` invocations) that only care about the
    /// revision string, not the full `ReviewTarget` shape.
    pub fn commit_revision(&self) -> Option<&str> {
        match self {
            ReviewTarget::Commit(rev) => Some(rev.as_str()),
            ReviewTarget::WorkingTree => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_commit_is_true_only_for_commit_variant() {
        assert!(!ReviewTarget::WorkingTree.is_commit());
        assert!(ReviewTarget::Commit("abc1234".to_string()).is_commit());
    }

    #[test]
    fn commit_revision_is_some_only_for_commit_variant() {
        assert_eq!(ReviewTarget::WorkingTree.commit_revision(), None);
        assert_eq!(
            ReviewTarget::Commit("abc1234".to_string()).commit_revision(),
            Some("abc1234")
        );
    }
}
