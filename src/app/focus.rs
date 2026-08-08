/// Which broad screen is showing: the tree+preview workspace, or diff full-screen.
/// Bridges the legacy `Focus` enum (which also encodes tree-pane focus and the
/// InlineSelect subview) toward the final render-grouping split — see plan.md
/// section 6-4. `Focus::InlineSelect` maps to `Diff` here: InlineSelect is a subview
/// of the Diff screen, not an independent active view (its own axis — which diff
/// pane content is showing: Patch/Full/InlineSelect — is orthogonal to this one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveView {
    Workspace,
    Diff,
}
