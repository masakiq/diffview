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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_commit_is_true_only_for_commit_variant() {
        assert!(!ReviewTarget::WorkingTree.is_commit());
        assert!(ReviewTarget::Commit("abc1234".to_string()).is_commit());
    }
}
