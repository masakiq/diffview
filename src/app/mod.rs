use ansi_to_tui::IntoText;
use anyhow::Result;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::Backend,
    text::{Line, Span, Text},
    Terminal,
};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

mod focus;

pub use focus::ActiveView;

use crate::clipboard;
use crate::config::Config;
use crate::domain::content::{FullFileSource, TreePane};
use crate::domain::diff::{parse_diff, DiffLine, FileDiff};
use crate::domain::review_target::ReviewTarget;
use crate::domain::status::GitFile;
use crate::infra::git::status::{get_commit_files, get_status};

// ─── Focus ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum Focus {
    Unstaged,
    Staged,
    DiffView,
    InlineSelect,
}

// ─── TreePane ───────────────────────────────────────────────────────────────
// (type defined in `domain::content` — see the `use` above; only the impl below,
// which returns `Focus`, an app-owned type, stays here)

impl TreePane {
    pub fn label(self) -> &'static str {
        match self {
            TreePane::Unstaged => "Unstaged",
            TreePane::Staged => "Staged",
        }
    }

    pub fn to_focus(self) -> Focus {
        match self {
            TreePane::Unstaged => Focus::Unstaged,
            TreePane::Staged => Focus::Staged,
        }
    }

    pub fn is_staged(self) -> bool {
        matches!(self, TreePane::Staged)
    }
}

// ─── Diff tool ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DiffTool {
    Raw,
    Delta,
    Difftastic,
}

impl DiffTool {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "delta" => DiffTool::Delta,
            "difftastic" => DiffTool::Difftastic,
            _ => DiffTool::Raw,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            DiffTool::Raw => "raw",
            DiffTool::Delta => "delta",
            DiffTool::Difftastic => "difftastic",
        }
    }

    pub fn supports_line_ops(&self) -> bool {
        *self != DiffTool::Difftastic
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiffContent {
    Patch,
    FullFile(FullFileSource),
}

// (type defined in `domain::content` — see the `use` above; only the impl below,
// with UI-facing label/message strings, stays here)
impl FullFileSource {
    fn title_label(self) -> &'static str {
        match self {
            FullFileSource::Current => "file",
            FullFileSource::Previous => "file:previous",
        }
    }

    pub(crate) fn status_message(self) -> &'static str {
        match self {
            FullFileSource::Current => "Full file view",
            FullFileSource::Previous => "Previous full file view",
        }
    }

    pub(crate) fn missing_message(self) -> &'static str {
        match self {
            FullFileSource::Current => {
                "Full file view unavailable: file does not exist in current state"
            }
            FullFileSource::Previous => {
                "Full file view unavailable: file does not exist in previous state"
            }
        }
    }
}

impl DiffContent {
    pub fn label(self) -> &'static str {
        match self {
            DiffContent::Patch => "patch",
            DiffContent::FullFile(source) => source.title_label(),
        }
    }

    pub(crate) fn is_full_file(self) -> bool {
        matches!(self, DiffContent::FullFile(_))
    }

    pub(crate) fn toggle_full_file(self, source: FullFileSource) -> Self {
        match self {
            DiffContent::FullFile(current) if current == source => DiffContent::Patch,
            _ => DiffContent::FullFile(source),
        }
    }
}

pub use crate::domain::content::ContentAnnotation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchScope {
    WorkingTree,
    CommitTree,
    DiffView,
    InlineSelect,
}

#[derive(Debug, Clone)]
struct SearchState {
    scope: SearchScope,
    query: String,
}

#[derive(Debug, Clone)]
struct SearchInput {
    scope: SearchScope,
    query: String,
}

#[derive(Debug, Clone)]
struct KeyBinding {
    code: KeyCode,
    modifiers: KeyModifiers,
    label: String,
}

impl KeyBinding {
    fn parse(spec: &str) -> Option<Self> {
        let label = spec.trim();
        if label.is_empty() {
            return None;
        }

        let lowercase = label.to_ascii_lowercase();
        let (modifiers, key_name) =
            if lowercase.starts_with("ctrl+") || lowercase.starts_with("ctrl-") {
                (KeyModifiers::CONTROL, &label[5..])
            } else if lowercase.starts_with("alt+") || lowercase.starts_with("alt-") {
                (KeyModifiers::ALT, &label[4..])
            } else {
                (KeyModifiers::NONE, label)
            };

        let code = match key_name.to_ascii_lowercase().as_str() {
            "enter" => KeyCode::Enter,
            "esc" | "escape" => KeyCode::Esc,
            "tab" => KeyCode::Tab,
            "backspace" => KeyCode::Backspace,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            _ => {
                let mut chars = key_name.chars();
                match (chars.next(), chars.next()) {
                    (Some(ch), None) => KeyCode::Char(ch),
                    _ => return None,
                }
            }
        };

        Some(Self {
            code,
            modifiers,
            label: label.to_string(),
        })
    }

    fn default_commit() -> Self {
        Self::parse("C").expect("default commit key binding should parse")
    }

    fn matches(&self, key: KeyEvent) -> bool {
        if key.code != self.code {
            return false;
        }

        let relevant_modifiers = key.modifiers & (KeyModifiers::CONTROL | KeyModifiers::ALT);
        relevant_modifiers == self.modifiers
    }

    fn label(&self) -> &str {
        &self.label
    }
}

// ─── Tree nodes ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct TreeNode {
    pub path: PathBuf,
    pub name: String,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
    pub staged: char,
    pub unstaged: char,
}

impl TreeNode {
    pub fn is_untracked(&self) -> bool {
        self.staged == '?' && self.unstaged == '?'
    }

    pub fn is_unmerged(&self) -> bool {
        matches!(
            (self.staged, self.unstaged),
            ('U', _) | (_, 'U') | ('A', 'A') | ('D', 'D')
        )
    }

    pub fn status_for(&self, pane: TreePane) -> char {
        match pane {
            TreePane::Unstaged => self.unstaged,
            TreePane::Staged => {
                if self.staged == '?' {
                    ' '
                } else {
                    self.staged
                }
            }
        }
    }

    pub fn display_prefix(&self) -> String {
        let indent = "  ".repeat(self.depth);
        let prefix = if self.is_dir {
            if self.expanded {
                "▼ "
            } else {
                "▶ "
            }
        } else {
            "  "
        };
        format!("{}{}", indent, prefix)
    }

    pub fn display_status_suffix(&self, pane: TreePane) -> String {
        let status_char = self.status_for(pane);
        if self.is_dir || status_char == ' ' {
            String::new()
        } else {
            format!(" {}", status_char)
        }
    }

    pub fn display_row_text(&self, pane: TreePane) -> String {
        format!(
            "{}{}{}",
            self.display_prefix(),
            self.name,
            self.display_status_suffix(pane)
        )
    }
}

// ─── TreeSection ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct TreeSection {
    pub all_nodes: Vec<TreeNode>,
    pub visible: Vec<usize>,
    pub cursor: usize,
}

impl TreeSection {
    pub fn new() -> Self {
        Self {
            all_nodes: Vec::new(),
            visible: Vec::new(),
            cursor: 0,
        }
    }

    pub fn current_node(&self) -> Option<&TreeNode> {
        self.visible
            .get(self.cursor)
            .and_then(|&idx| self.all_nodes.get(idx))
    }

    /// Whether `path` names an untracked, non-directory node in this section — the
    /// tree-search half of `has_untracked_file_in_pane`; the caller still applies the
    /// Commit-target short circuit (an untracked node never appears in that tree in the
    /// first place, but the check is cheap insurance and keeps the "no untracked files
    /// under Commit" invariant visible at the call site rather than buried in here).
    pub fn has_untracked_file(&self, path: &str) -> bool {
        self.all_nodes
            .iter()
            .any(|n| !n.is_dir && n.path == Path::new(path) && n.is_untracked())
    }

    pub fn rebuild_visible(&mut self) {
        let expanded: std::collections::HashMap<PathBuf, bool> = self
            .all_nodes
            .iter()
            .filter(|n| n.is_dir)
            .map(|n| (n.path.clone(), n.expanded))
            .collect();

        self.visible.clear();
        'outer: for (i, node) in self.all_nodes.iter().enumerate() {
            let mut check = node.path.clone();
            loop {
                match check.parent() {
                    Some(p) if p != Path::new("") => {
                        if let Some(&exp) = expanded.get(p) {
                            if !exp {
                                continue 'outer;
                            }
                        }
                        check = p.to_path_buf();
                    }
                    _ => break,
                }
            }
            self.visible.push(i);
        }
    }

    pub fn clamp_cursor(&mut self) {
        if self.visible.is_empty() {
            self.cursor = 0;
        } else if self.cursor >= self.visible.len() {
            self.cursor = self.visible.len() - 1;
        }
    }

    pub fn move_cursor_to_first_file(&mut self) {
        if let Some(position) = self
            .visible
            .iter()
            .position(|&idx| self.all_nodes.get(idx).is_some_and(|node| !node.is_dir))
        {
            self.cursor = position;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.visible.is_empty()
    }

    pub fn file_count(&self) -> usize {
        self.all_nodes.iter().filter(|n| !n.is_dir).count()
    }

    /// Expand a directory and move cursor to its first child
    pub(crate) fn expand_and_enter(&mut self) {
        let cursor_vis_idx = self.cursor;
        let node_idx = match self.visible.get(cursor_vis_idx) {
            Some(&idx) => idx,
            None => return,
        };
        if !self.all_nodes[node_idx].is_dir {
            return;
        }

        self.all_nodes[node_idx].expanded = true;
        self.rebuild_visible();
        self.clamp_cursor();

        // Move cursor to the first child (the next visible item after the dir)
        if cursor_vis_idx + 1 < self.visible.len() {
            self.cursor = cursor_vis_idx + 1;
        }
    }

    /// Fold the parent directory of the current node
    pub(crate) fn fold_parent(&mut self) {
        let current_path = match self.current_node() {
            Some(n) => n.path.clone(),
            None => return,
        };
        if let Some(parent) = current_path.parent() {
            if parent == Path::new("") {
                return;
            }
            for (i, node) in self.all_nodes.iter_mut().enumerate() {
                if node.is_dir && node.path == parent {
                    node.expanded = false;
                    self.rebuild_visible();
                    if let Some(pos) = self.visible.iter().position(|&idx| idx == i) {
                        self.cursor = pos;
                    }
                    self.clamp_cursor();
                    return;
                }
            }
        }
    }

    /// Collect all file paths under a directory node (for batch stage/unstage)
    pub(crate) fn files_under_dir(&self, dir_path: &Path) -> Vec<String> {
        self.all_nodes
            .iter()
            .filter(|n| !n.is_dir && n.path.starts_with(dir_path))
            .map(|n| n.path.to_string_lossy().to_string())
            .collect()
    }

    fn reveal_node(&mut self, node_idx: usize) {
        let Some(target_path) = self.all_nodes.get(node_idx).map(|node| node.path.clone()) else {
            return;
        };

        for node in &mut self.all_nodes {
            if node.is_dir && target_path.starts_with(&node.path) {
                node.expanded = true;
            }
        }

        self.rebuild_visible();
        if let Some(position) = self.visible.iter().position(|&idx| idx == node_idx) {
            self.cursor = position;
        }
        self.clamp_cursor();
    }
}

/// Holds both tree sections. Physically a step toward `views/tree/` (plan.md section 4-1),
/// but the Commit-target "Files" section still reuses `unstaged`/`staged` == empty, same
/// as before this type existed — the `TreeSections` split (Commit's `files` as its own
/// field, not a reuse of `unstaged`) is a separate, not-yet-done step (plan.md section 6-0).
#[derive(Debug, Clone)]
pub struct TreeViewState {
    pub unstaged: TreeSection,
    pub staged: TreeSection,
}

impl TreeViewState {
    pub fn new() -> Self {
        Self {
            unstaged: TreeSection::new(),
            staged: TreeSection::new(),
        }
    }
}

impl Default for TreeViewState {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Line mapping for inline-select ─────────────────────────────────────

#[derive(Debug, Clone)]
pub struct DisplayLineInfo {
    pub hunk_idx: Option<usize>,
    pub line_in_hunk: Option<usize>,
    pub is_selectable: bool,
}

pub use crate::domain::content::FileSelectionState;

#[derive(Debug, Clone)]
pub(crate) struct PendingTreePreview {
    pane: TreePane,
    ready_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExternalAction {
    Commit,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct DiffCacheKey {
    path: String,
    pane: TreePane,
    tool: DiffTool,
    pane_width: u16,
    commit_revision: Option<String>,
    diff_content: DiffContent,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct DiffScrollKey {
    path: String,
    pane: TreePane,
}

#[derive(Clone)]
pub(crate) struct CachedDiff {
    raw_diff: String,
    display_diff: String,
    file_diff: FileDiff,
    line_infos: Vec<DisplayLineInfo>,
    display_line_count: usize,
    raw_line_count: usize,
    cached_display_text: Option<Text<'static>>,
    content_annotation: Option<ContentAnnotation>,
    full_file_copyable: bool,
    full_file_content_offset: usize,
    full_file_highlight_lines: Vec<u32>,
}

struct LoadedContent {
    raw: String,
    display: String,
    file_diff: FileDiff,
    line_infos: Vec<DisplayLineInfo>,
    display_line_count: usize,
    raw_line_count: usize,
    cached_display_text: Option<Text<'static>>,
    content_annotation: Option<ContentAnnotation>,
    full_file_copyable: bool,
    full_file_content_offset: usize,
    full_file_highlight_lines: Vec<u32>,
}

pub use crate::domain::content::FullFileContentTarget;

const DIFF_CACHE_CAPACITY: usize = 64;
const TREE_PREVIEW_DEBOUNCE_MS: u64 = 100;
pub(crate) const TREE_FAST_MOVE_LINES: usize = 5;
const NULL_COMMIT_OID: &str = "0000000000000000000000000000000000000000";

fn normalize_revision_override(revision_override: Option<String>) -> Option<String> {
    revision_override.filter(|revision| revision != NULL_COMMIT_OID)
}

/// Everything the Diff screen owns: the loaded document (`raw_diff`/`file_diff`/
/// `line_infos`), the three cursor spaces that read it (`patch_cursor`/`diff_cursor`/
/// `full_file_cursor` — distinct row spaces, see each field's own doc comment), and the
/// diff/scroll caches. Physically a step toward `views/diff/` (plan.md section 4-1) —
/// `views/diff/inline_select.rs` becomes a child module of that same directory so it can
/// keep reading this document without a copy (plan.md section 6-4). The 3-owner cache
/// split (content/render/position, plan.md section 10-11) is a separate, not-yet-done step.
/// Field names are kept identical to their pre-extraction `App` names (rather than
/// dropping the `diff_`/`full_file_` prefixes now redundant under `self.diff.`) so the
/// extraction is a mechanical `self.x` -> `self.diff.x` rename, not a field-rename too.
pub struct DiffViewState {
    pub diff_origin: Option<TreePane>,
    pub diff_content: DiffContent,
    pub display_diff: String,
    pub raw_diff: String,
    pub file_diff: FileDiff,
    pub diff_scroll: usize,
    pub diff_cursor: usize,
    /// The always-on patch-view cursor: a display-row index into `display_diff` (the same
    /// row space `diff_scroll` already occupies for patch view), analogous to
    /// `full_file_cursor` but for `DiffContent::Patch`. Distinct from `diff_cursor`, which
    /// is raw-line-indexed and only meaningful in `Focus::InlineSelect` (partial-patch
    /// staging always operates on raw diff text, regardless of which tool renders the
    /// patch pane).
    pub patch_cursor: usize,
    pub hunk_cursor: usize,
    pub current_file: Option<String>,
    pub line_infos: Vec<DisplayLineInfo>,
    pub display_line_count: usize,
    pub raw_line_count: usize,
    pub cached_display_text: Option<Text<'static>>,
    pub content_annotation: Option<ContentAnnotation>,
    pub full_file_copyable: bool,
    pub full_file_content_offset: usize,
    pub full_file_highlight_lines: Vec<u32>,
    pub full_file_cursor: usize,
    pub full_file_anchor: Option<usize>,
    /// Set by a lone `g` press in `DiffView`, waiting for a second `g` to complete the
    /// vim-style `gg` jump-to-top. Cleared by any other key, or by leaving/switching the
    /// diff view.
    pub pending_g: bool,
    pub diff_pane_height: usize,
    pub diff_pane_width: u16,
    pub diff_cache: HashMap<DiffCacheKey, CachedDiff>,
    pub diff_cache_order: VecDeque<DiffCacheKey>,
    pub diff_cache_capacity: usize,
    /// Patch view's own remembered (scroll, cursor) per file/pane — restored verbatim on
    /// return from full-file view, so navigation made while in full-file view never leaks
    /// back into patch view's own position.
    pub diff_scroll_positions: HashMap<DiffScrollKey, (usize, usize)>,
}

impl DiffViewState {
    fn new() -> Self {
        Self {
            diff_origin: None,
            diff_content: DiffContent::Patch,
            display_diff: String::new(),
            raw_diff: String::new(),
            file_diff: FileDiff::default(),
            diff_scroll: 0,
            diff_cursor: 0,
            patch_cursor: 0,
            hunk_cursor: 0,
            current_file: None,
            line_infos: Vec::new(),
            display_line_count: 0,
            raw_line_count: 0,
            cached_display_text: None,
            content_annotation: None,
            full_file_copyable: false,
            full_file_content_offset: 0,
            full_file_highlight_lines: Vec::new(),
            full_file_cursor: 0,
            full_file_anchor: None,
            pending_g: false,
            diff_pane_height: 20,
            diff_pane_width: 0,
            diff_cache: HashMap::new(),
            diff_cache_order: VecDeque::new(),
            diff_cache_capacity: DIFF_CACHE_CAPACITY,
            diff_scroll_positions: HashMap::new(),
        }
    }
}

impl Default for DiffViewState {
    fn default() -> Self {
        Self::new()
    }
}

// ─── App ───────────────────────────────────────────────────────────────────

pub struct App {
    pub should_quit: bool,
    pub focus: Focus,
    #[allow(dead_code)]
    pub config: Config,
    pub tool: DiffTool,
    pub repo_root: PathBuf,
    pub review_target: ReviewTarget,
    tree_pane_percentage: u16,
    commit_key: KeyBinding,
    commit_command: Vec<String>,
    pub(super) pending_action: Option<ExternalAction>,

    // Tree sections
    pub tree: TreeViewState,
    /// New path → pre-rename/copy path, for every file `refresh_trees` currently reports
    /// with a `previous_path` (status `R`/`C`). A rename/copy's `path` is always the new
    /// one; looking up its `Previous` content needs the tree/HEAD blob at the old path
    /// instead, which only ever exists there before the rename.
    rename_sources: HashMap<String, String>,

    // Diff state
    pub diff: DiffViewState,
    pub(super) pending_tree_preview: Option<PendingTreePreview>,
    tree_preview_debounce: Duration,
    search_state: Option<SearchState>,
    search_input: Option<SearchInput>,

    // Status bar
    pub status_message: Option<String>,
    pub error_message: Option<String>,
}

impl App {
    pub fn new(tool_override: Option<String>, revision_override: Option<String>) -> Result<Self> {
        let repo_root = crate::infra::git::get_repo_root()?;
        let review_target = match normalize_revision_override(revision_override) {
            Some(rev) => ReviewTarget::Commit(crate::infra::git::resolve_commit(&rev, &repo_root)?),
            None => ReviewTarget::WorkingTree,
        };

        let config = Config::load().unwrap_or_default();

        let tool = if let Some(t) = tool_override {
            DiffTool::from_str(&t)
        } else {
            DiffTool::from_str(&config.diff.tool)
        };
        let tree_pane_percentage = config.diff.tree_width_percentage();
        let commit_key =
            KeyBinding::parse(&config.diff.commit.key).unwrap_or_else(KeyBinding::default_commit);
        let commit_command = if config.diff.commit.command.is_empty() {
            vec!["git".to_string(), "commit".to_string(), "-v".to_string()]
        } else {
            config.diff.commit.command.clone()
        };

        let mut app = App {
            should_quit: false,
            focus: Focus::Unstaged,
            config,
            tool,
            repo_root,
            review_target,
            tree_pane_percentage,
            commit_key,
            commit_command,
            pending_action: None,
            tree: TreeViewState::new(),
            rename_sources: HashMap::new(),
            diff: DiffViewState::new(),
            pending_tree_preview: None,
            tree_preview_debounce: Duration::from_millis(TREE_PREVIEW_DEBOUNCE_MS),
            search_state: None,
            search_input: None,
            status_message: None,
            error_message: None,
        };

        let width = crossterm::terminal::size().map(|(w, _)| w).unwrap_or(120);
        app.diff.diff_pane_width = app.compute_diff_pane_width(width);

        app.refresh_trees()?;

        // Auto-focus: if unstaged is empty but staged has items, start in staged
        if !app.is_commit() && app.tree.unstaged.is_empty() && !app.tree.staged.is_empty() {
            app.focus = Focus::Staged;
        }

        if let Some(pane) = app.focused_pane() {
            app.tree_mut(pane).move_cursor_to_first_file();
        }

        // Auto-load diff for the first file in the focused section
        app.auto_load_first_diff();

        Ok(app)
    }

    fn auto_load_first_diff(&mut self) {
        let pane = match self.focus {
            Focus::Unstaged => TreePane::Unstaged,
            Focus::Staged => TreePane::Staged,
            _ => return,
        };
        let section = self.tree(pane);
        if let Some(node) = section.current_node() {
            if !node.is_dir {
                let path = node.path.to_string_lossy().to_string();
                let diff_content = self.default_diff_content_for(pane, &path);
                let _ = self.load_diff(&path, pane, diff_content);
            }
        }
    }

    pub fn target(&self) -> ReviewTarget {
        self.review_target.clone()
    }

    pub fn is_commit(&self) -> bool {
        self.review_target.is_commit()
    }

    pub fn active_view(&self) -> ActiveView {
        match self.focus {
            Focus::Unstaged | Focus::Staged => ActiveView::Workspace,
            Focus::DiffView | Focus::InlineSelect => ActiveView::Diff,
        }
    }

    pub fn commit_label(&self) -> Option<String> {
        self.review_target
            .commit_revision()
            .map(|rev| format!("commit {}", rev.chars().take(8).collect::<String>()))
    }

    pub fn tree_title(&self, pane: TreePane) -> &'static str {
        if self.is_commit() {
            "Files"
        } else {
            pane.label()
        }
    }

    pub fn diff_origin_label(&self, pane: TreePane) -> String {
        if let Some(label) = self.commit_label() {
            label
        } else {
            pane.label().to_lowercase()
        }
    }

    pub fn tree_pane_percentage(&self) -> u16 {
        self.tree_pane_percentage
    }

    pub fn diff_pane_percentage(&self) -> u16 {
        100 - self.tree_pane_percentage
    }

    pub fn commit_key_label(&self) -> &str {
        self.commit_key.label()
    }

    pub fn search_prompt(&self) -> Option<String> {
        self.search_input
            .as_ref()
            .map(|input| format!("/{}", input.query))
    }

    pub fn tree_search_query(&self, pane: TreePane) -> Option<&str> {
        let current_scope = self.current_search_scope()?;
        let search = self.search_state.as_ref()?;

        match (current_scope, search.scope, self.is_commit(), pane) {
            (SearchScope::WorkingTree, SearchScope::WorkingTree, false, _) => {
                Some(search.query.as_str())
            }
            (SearchScope::CommitTree, SearchScope::CommitTree, true, TreePane::Unstaged) => {
                Some(search.query.as_str())
            }
            _ => None,
        }
    }

    pub fn diff_search_query(&self) -> Option<&str> {
        let scope = match self.focus {
            Focus::DiffView => SearchScope::DiffView,
            Focus::InlineSelect => SearchScope::InlineSelect,
            _ => return None,
        };

        self.search_state
            .as_ref()
            .filter(|search| search.scope == scope)
            .map(|search| search.query.as_str())
    }

    pub(super) fn can_trigger_commit_action(&self, key: KeyEvent) -> bool {
        !self.is_commit()
            && self.commit_key.matches(key)
            && matches!(
                self.focus,
                Focus::Unstaged | Focus::Staged | Focus::DiffView
            )
    }

    // ─── Tree access ────────────────────────────────────────────────────

    pub fn tree(&self, pane: TreePane) -> &TreeSection {
        match pane {
            TreePane::Unstaged => &self.tree.unstaged,
            TreePane::Staged => &self.tree.staged,
        }
    }

    pub fn is_tree_focused(&self, pane: TreePane) -> bool {
        if self.is_commit() && pane == TreePane::Staged {
            return false;
        }
        match pane {
            TreePane::Unstaged => self.focus == Focus::Unstaged,
            TreePane::Staged => self.focus == Focus::Staged,
        }
    }

    pub(super) fn tree_mut(&mut self, pane: TreePane) -> &mut TreeSection {
        match pane {
            TreePane::Unstaged => &mut self.tree.unstaged,
            TreePane::Staged => &mut self.tree.staged,
        }
    }

    pub(super) fn focused_pane(&self) -> Option<TreePane> {
        if self.is_commit() {
            return match self.focus {
                Focus::Unstaged => Some(TreePane::Unstaged),
                _ => None,
            };
        }

        match self.focus {
            Focus::Unstaged => Some(TreePane::Unstaged),
            Focus::Staged => Some(TreePane::Staged),
            _ => None,
        }
    }

    fn current_search_scope(&self) -> Option<SearchScope> {
        match self.focus {
            Focus::Unstaged => Some(if self.is_commit() {
                SearchScope::CommitTree
            } else {
                SearchScope::WorkingTree
            }),
            Focus::Staged if !self.is_commit() => Some(SearchScope::WorkingTree),
            Focus::DiffView => Some(SearchScope::DiffView),
            Focus::InlineSelect => Some(SearchScope::InlineSelect),
            _ => None,
        }
    }

    fn compute_diff_pane_width(&self, total_width: u16) -> u16 {
        if matches!(self.focus, Focus::DiffView | Focus::InlineSelect) {
            total_width.saturating_sub(2)
        } else {
            let diff_width =
                (u32::from(total_width) * u32::from(self.diff_pane_percentage())) / 100;
            (diff_width as u16).saturating_sub(2)
        }
    }

    // ─── Tree building ───────────────────────────────────────────────────

    pub fn refresh_trees(&mut self) -> Result<()> {
        if let Some(rev) = self.review_target.commit_revision() {
            let files = get_commit_files(rev, &self.repo_root)?;
            self.rename_sources = build_rename_sources(&files);
            let commit_files: Vec<(String, char, char)> = files
                .into_iter()
                .map(|f| (f.path, f.staged, f.unstaged))
                .collect();

            build_section(&mut self.tree.unstaged.all_nodes, &commit_files);
            rebuild_section_visible(&mut self.tree.unstaged);

            self.tree.staged.all_nodes.clear();
            self.tree.staged.visible.clear();
            self.tree.staged.cursor = 0;
            if self.focus == Focus::Staged {
                self.focus = Focus::Unstaged;
            }
            return Ok(());
        }

        let files = get_status(&self.repo_root)?;
        self.rename_sources = build_rename_sources(&files);

        // Split files into unstaged and staged
        let mut unstaged_files: Vec<(String, char, char)> = Vec::new();
        let mut staged_files: Vec<(String, char, char)> = Vec::new();

        for file in &files {
            // Unstaged: Y column ≠ ' ' (includes '?' for untracked)
            if file.unstaged != ' ' {
                unstaged_files.push((file.path.clone(), file.staged, file.unstaged));
            }
            // Staged: X column ≠ ' ' AND X column ≠ '?'
            if file.staged != ' ' && file.staged != '?' {
                staged_files.push((file.path.clone(), file.staged, file.unstaged));
            }
        }

        build_section(&mut self.tree.unstaged.all_nodes, &unstaged_files);
        rebuild_section_visible(&mut self.tree.unstaged);

        build_section(&mut self.tree.staged.all_nodes, &staged_files);
        rebuild_section_visible(&mut self.tree.staged);

        Ok(())
    }

    // ─── Diff loading ────────────────────────────────────────────────────

    fn build_diff_cache_key(
        &self,
        path: &str,
        pane: TreePane,
        diff_content: DiffContent,
    ) -> DiffCacheKey {
        DiffCacheKey {
            path: path.to_string(),
            pane,
            tool: self.tool.clone(),
            pane_width: self.diff.diff_pane_width,
            commit_revision: self.review_target.commit_revision().map(String::from),
            diff_content,
        }
    }

    fn build_diff_scroll_key(&self, path: &str, pane: TreePane) -> DiffScrollKey {
        DiffScrollKey {
            path: path.to_string(),
            pane,
        }
    }

    fn saved_diff_position(&self, path: &str, pane: TreePane) -> (usize, usize) {
        self.diff
            .diff_scroll_positions
            .get(&self.build_diff_scroll_key(path, pane))
            .copied()
            .unwrap_or((0, 0))
    }

    fn remember_diff_position(&mut self, path: &str, pane: TreePane, scroll: usize, cursor: usize) {
        self.diff
            .diff_scroll_positions
            .insert(self.build_diff_scroll_key(path, pane), (scroll, cursor));
    }

    /// Full file view never remembers its scroll position: it always opens at the top.
    fn remember_current_diff_scroll(&mut self) {
        if self.diff.diff_content.is_full_file() {
            return;
        }
        if let (Some(path), Some(pane)) = (self.diff.current_file.clone(), self.diff.diff_origin) {
            self.remember_diff_position(&path, pane, self.diff.diff_scroll, self.diff.patch_cursor);
        }
    }

    fn restore_saved_diff_scroll(&mut self, path: &str, pane: TreePane, diff_content: DiffContent) {
        if diff_content.is_full_file() {
            self.diff.diff_scroll = 0;
            return;
        }
        let (saved_scroll, saved_cursor) = self.saved_diff_position(path, pane);
        self.diff.diff_scroll = saved_scroll.min(self.diff.display_line_count.saturating_sub(1));
        // Patch view's own cursor is restored independently of scroll — covers a fresh file
        // selection (both default to 0), a cache hit, and returning from full/previous file
        // view to patch (same f/F key), all uniformly, since every one of those goes through
        // `load_diff` and this same restore step. Movement made while in full-file view never
        // reaches this: `remember_current_diff_scroll` only snapshots patch view's own state.
        self.diff.patch_cursor = saved_cursor.min(self.diff.display_line_count.saturating_sub(1));
        // `apply_loaded_diff_state` (already run by this point in `load_diff`) unconditionally
        // reset `hunk_cursor` to 0 — realign it with wherever the restored cursor actually
        // landed, or the hunk title/`]`/`[` jump target would show hunk 1 even when the
        // cursor is sitting deep inside a later one.
        self.sync_hunk_cursor_from_patch_cursor();
        // Neither `saved_scroll` nor the cursor just restored above were validated
        // against the *current* `diff_pane_height` — if the pane shrank while a
        // different file was shown, restoring this file's own (scroll, cursor) pair can
        // still leave the cursor outside the now-smaller viewport until the next
        // cursor-moving key re-clamps it (review_8 Finding 3-A).
        self.follow_active_diff_cursor();
    }

    fn touch_diff_cache_key(&mut self, key: DiffCacheKey) {
        if let Some(pos) = self
            .diff
            .diff_cache_order
            .iter()
            .position(|existing| *existing == key)
        {
            self.diff.diff_cache_order.remove(pos);
        }
        self.diff.diff_cache_order.push_back(key);
    }

    fn get_cached_diff(&mut self, key: &DiffCacheKey) -> Option<CachedDiff> {
        let cached = self.diff.diff_cache.get(key).cloned();
        if cached.is_some() {
            self.touch_diff_cache_key(key.clone());
        }
        cached
    }

    fn insert_cached_diff(&mut self, key: DiffCacheKey, value: CachedDiff) {
        if self.diff.diff_cache.contains_key(&key) {
            self.diff.diff_cache.insert(key.clone(), value);
            self.touch_diff_cache_key(key);
            return;
        }

        if self.diff.diff_cache.len() >= self.diff.diff_cache_capacity {
            if let Some(oldest) = self.diff.diff_cache_order.pop_front() {
                self.diff.diff_cache.remove(&oldest);
            }
        }

        self.diff.diff_cache.insert(key.clone(), value);
        self.diff.diff_cache_order.push_back(key);
    }

    pub(super) fn clear_diff_cache(&mut self) {
        self.diff.diff_cache.clear();
        self.diff.diff_cache_order.clear();
    }

    fn own_text(text: Text<'_>) -> Text<'static> {
        let lines = text
            .lines
            .into_iter()
            .map(|line| Line {
                style: line.style,
                alignment: line.alignment,
                spans: line
                    .spans
                    .into_iter()
                    .map(|span| Span {
                        style: span.style,
                        content: Cow::Owned(span.content.into_owned()),
                    })
                    .collect(),
            })
            .collect();

        Text {
            alignment: text.alignment,
            style: text.style,
            lines,
        }
    }

    fn build_cached_display_text(
        &self,
        display: &str,
        force_ansi_rendering: bool,
    ) -> Option<Text<'static>> {
        if self.tool == DiffTool::Raw && !force_ansi_rendering {
            return None;
        }
        let parsed = display.as_bytes().into_text().ok()?;
        Some(Self::own_text(parsed))
    }

    fn apply_loaded_diff_state(
        &mut self,
        path: &str,
        pane: TreePane,
        diff_content: DiffContent,
        raw_diff: String,
        display_diff: String,
        file_diff: FileDiff,
        line_infos: Vec<DisplayLineInfo>,
        display_line_count: usize,
        raw_line_count: usize,
        cached_display_text: Option<Text<'static>>,
        content_annotation: Option<ContentAnnotation>,
        full_file_copyable: bool,
        full_file_content_offset: usize,
        full_file_highlight_lines: Vec<u32>,
    ) {
        self.diff.raw_diff = raw_diff;
        self.diff.display_diff = display_diff;
        self.diff.file_diff = file_diff;
        self.diff.line_infos = line_infos;
        self.diff.display_line_count = display_line_count;
        self.diff.raw_line_count = raw_line_count;
        self.diff.cached_display_text = cached_display_text;
        self.diff.diff_content = diff_content;
        self.diff.content_annotation = content_annotation;
        self.diff.full_file_copyable = full_file_copyable;
        self.diff.full_file_content_offset = full_file_content_offset;
        self.diff.full_file_highlight_lines = full_file_highlight_lines;
        self.diff.current_file = Some(path.to_string());
        self.diff.diff_origin = Some(pane);
        self.diff.diff_scroll = 0;
        self.diff.diff_cursor = 0;
        self.diff.hunk_cursor = 0;
        if diff_content.is_full_file() {
            // A file selection can now land here directly in full-file view (an
            // untracked file — see `default_diff_content_for`), bypassing the patch-view
            // step that `toggle_full_file_view` normally resets these through. Without
            // this, `full_file_cursor`/`full_file_anchor` from whatever file was
            // previously open in full-file view would carry over — e.g. a cursor deep
            // in a 500-line file left dangling past the end of a 10-line one just
            // selected. `toggle_full_file_view` still overwrites `full_file_cursor`
            // with its own derived value right after calling `load_diff`, and always
            // clears `full_file_anchor` itself, so this doesn't change that path.
            self.diff.full_file_cursor = 0;
            self.diff.full_file_anchor = None;
        }
    }

    fn build_patch_line_infos(raw: &str) -> Vec<DisplayLineInfo> {
        let mut infos: Vec<DisplayLineInfo> = Vec::new();
        let mut hunk_idx: Option<usize> = None;
        let mut line_in_hunk: usize = 0;
        let mut current_hunk_counter = 0usize;

        for line in raw.lines() {
            if line.starts_with("@@") {
                hunk_idx = Some(current_hunk_counter);
                current_hunk_counter += 1;
                line_in_hunk = 0;
                infos.push(DisplayLineInfo {
                    hunk_idx,
                    line_in_hunk: None,
                    is_selectable: false,
                });
            } else if hunk_idx.is_some() && line.starts_with('\\') {
                // "\ No newline at end of file" — parse_diff() doesn't store this marker
                // in Hunk.lines, so it must not consume a line_in_hunk slot: it gets the
                // *current* counter value (same index the next real row would receive)
                // without incrementing it, so mapping this row lands on the same file
                // line the immediately following content resumes at — not reset all the
                // way back to the hunk's start (which is only correct for the "@@" header
                // itself). `is_selectable` stays false, so this never becomes a staging
                // target even though `line_in_hunk` is now `Some`.
                infos.push(DisplayLineInfo {
                    hunk_idx,
                    line_in_hunk: Some(line_in_hunk),
                    is_selectable: false,
                });
            } else if hunk_idx.is_some() {
                let is_sel = line.starts_with('+') || line.starts_with('-');
                infos.push(DisplayLineInfo {
                    hunk_idx,
                    line_in_hunk: Some(line_in_hunk),
                    is_selectable: is_sel,
                });
                line_in_hunk += 1;
            } else {
                infos.push(DisplayLineInfo {
                    hunk_idx: None,
                    line_in_hunk: None,
                    is_selectable: false,
                });
            }
        }

        infos
    }

    fn build_preview_line_infos_for(raw: &str) -> Vec<DisplayLineInfo> {
        raw.lines()
            .map(|_| DisplayLineInfo {
                hunk_idx: None,
                line_in_hunk: None,
                is_selectable: false,
            })
            .collect()
    }

    fn build_loaded_content(
        &self,
        raw: String,
        display: String,
        file_diff: FileDiff,
        is_non_patch: bool,
        force_ansi_rendering: bool,
        content_annotation: Option<ContentAnnotation>,
        full_file_copyable: bool,
        full_file_content_offset: usize,
        full_file_highlight_lines: Vec<u32>,
    ) -> LoadedContent {
        let line_infos = if is_non_patch {
            Self::build_preview_line_infos_for(&raw)
        } else {
            Self::build_patch_line_infos(&raw)
        };

        LoadedContent {
            raw_line_count: raw.lines().count(),
            display_line_count: display.lines().count(),
            cached_display_text: self.build_cached_display_text(&display, force_ansi_rendering),
            raw,
            display,
            file_diff,
            line_infos,
            content_annotation,
            full_file_copyable,
            full_file_content_offset,
            full_file_highlight_lines,
        }
    }

    fn file_selection_state(&self, path: &str, pane: TreePane) -> Option<FileSelectionState> {
        self.tree(pane)
            .all_nodes
            .iter()
            .find(|node| !node.is_dir && node.path == Path::new(path))
            .map(|node| FileSelectionState {
                status: node.status_for(pane),
                is_unmerged: !self.is_commit() && node.is_unmerged(),
                is_untracked: node.is_untracked(),
            })
    }

    fn full_file_unavailable_content(
        &self,
        message: &str,
        content_annotation: Option<ContentAnnotation>,
    ) -> LoadedContent {
        self.build_loaded_content(
            message.to_string(),
            message.to_string(),
            FileDiff::default(),
            true,
            false,
            content_annotation,
            false,
            0,
            Vec::new(),
        )
    }

    fn plain_full_file_content(
        &self,
        raw: String,
        content_annotation: Option<ContentAnnotation>,
        highlight_lines: Vec<u32>,
    ) -> LoadedContent {
        self.build_loaded_content(
            raw.clone(),
            raw,
            FileDiff::default(),
            true,
            false,
            content_annotation,
            true,
            0,
            highlight_lines,
        )
    }

    fn rich_full_file_content(
        &self,
        path: &str,
        raw: String,
        content_annotation: Option<ContentAnnotation>,
        highlight_lines: Vec<u32>,
    ) -> LoadedContent {
        match crate::infra::git::diff::render_content_preview(path, &raw, &self.repo_root) {
            Ok(preview) => self.build_loaded_content(
                raw,
                preview.content,
                FileDiff::default(),
                true,
                preview.uses_ansi,
                content_annotation,
                true,
                preview.content_offset,
                highlight_lines,
            ),
            Err(_) => self.plain_full_file_content(raw, content_annotation, highlight_lines),
        }
    }

    fn patch_file_diff_for(&mut self, path: &str, pane: TreePane) -> FileDiff {
        let patch_key = self.build_diff_cache_key(path, pane, DiffContent::Patch);
        if let Some(cached) = self.get_cached_diff(&patch_key) {
            return cached.file_diff;
        }

        let raw = if let Some(rev) = self.review_target.commit_revision() {
            crate::infra::git::diff::get_raw_commit_diff(rev, path, &self.repo_root)
                .unwrap_or_default()
        } else {
            crate::infra::git::diff::get_raw_diff(path, pane.is_staged(), &self.repo_root)
                .unwrap_or_default()
        };

        parse_diff(&raw)
    }

    fn load_full_file_content(
        &mut self,
        path: &str,
        pane: TreePane,
        source: FullFileSource,
    ) -> LoadedContent {
        let Some(file_state) = self.file_selection_state(path, pane) else {
            return self.full_file_unavailable_content(
                "Full file view unavailable: file metadata not found",
                None,
            );
        };

        if file_state.is_unmerged {
            return self.full_file_unavailable_content(
                "Full file view unavailable for unmerged files",
                Some(ContentAnnotation::UnmergedUnavailable),
            );
        }

        let target = match crate::domain::content::resolve_full_file_content_target(
            path,
            pane,
            file_state,
            source,
            &self.target(),
            &self.rename_sources,
        ) {
            Ok(target) => target,
            Err(()) => return self.full_file_unavailable_content(source.missing_message(), None),
        };

        let file_diff = if file_state.is_untracked {
            FileDiff::default()
        } else {
            self.patch_file_diff_for(path, pane)
        };

        let is_binary = if file_state.is_untracked {
            crate::infra::git::diff::is_binary_untracked_file(path, &self.repo_root)
                .unwrap_or(false)
        } else {
            file_diff.is_binary
        };
        if is_binary {
            return self.full_file_unavailable_content(
                "Full file view unavailable for binary files",
                Some(ContentAnnotation::BinaryUnavailable),
            );
        }

        let highlight_lines = full_file_diff_highlight_lines(&file_diff, source);

        match target {
            FullFileContentTarget::Worktree => {
                match crate::infra::git::diff::get_file_content(path, &self.repo_root) {
                    Ok(raw) => self.rich_full_file_content(path, raw, None, highlight_lines),
                    Err(_) => self.full_file_unavailable_content("File content unavailable", None),
                }
            }
            FullFileContentTarget::Revision {
                object_ref,
                path: object_path,
                content_annotation,
            } => match crate::infra::git::diff::get_file_content_at_object(
                &object_ref,
                &object_path,
                &self.repo_root,
            ) {
                Ok(raw) => {
                    self.rich_full_file_content(path, raw, content_annotation, highlight_lines)
                }
                Err(_) => self.full_file_unavailable_content(source.missing_message(), None),
            },
        }
    }

    pub fn load_diff(
        &mut self,
        path: &str,
        pane: TreePane,
        diff_content: DiffContent,
    ) -> Result<()> {
        self.remember_current_diff_scroll();

        let cache_key = self.build_diff_cache_key(path, pane, diff_content);
        if let Some(cached) = self.get_cached_diff(&cache_key) {
            self.apply_loaded_diff_state(
                path,
                pane,
                diff_content,
                cached.raw_diff,
                cached.display_diff,
                cached.file_diff,
                cached.line_infos,
                cached.display_line_count,
                cached.raw_line_count,
                cached.cached_display_text,
                cached.content_annotation,
                cached.full_file_copyable,
                cached.full_file_content_offset,
                cached.full_file_highlight_lines,
            );
            self.restore_saved_diff_scroll(path, pane, diff_content);
            return Ok(());
        }

        let loaded = match diff_content {
            DiffContent::Patch => {
                let is_untracked = self.has_untracked_file_in_pane(pane, path);
                let mut force_ansi_rendering = false;
                let mut patch_content_offset = 0;
                let (raw, display) = if is_untracked {
                    let preview = crate::infra::git::diff::get_file_preview(path, &self.repo_root)
                        .unwrap_or_else(|_| crate::infra::git::diff::FilePreview {
                            content: String::new(),
                            uses_ansi: false,
                            content_offset: 0,
                        });
                    force_ansi_rendering = preview.uses_ansi;
                    patch_content_offset = preview.content_offset;
                    (preview.content.clone(), preview.content)
                } else if let Some(rev) = self.review_target.commit_revision() {
                    let raw =
                        crate::infra::git::diff::get_raw_commit_diff(rev, path, &self.repo_root)
                            .unwrap_or_default();
                    let display = if self.tool == DiffTool::Raw {
                        raw.clone()
                    } else {
                        crate::infra::git::diff::get_display_commit_diff(
                            rev,
                            path,
                            self.tool.name(),
                            self.diff.diff_pane_width,
                            &self.repo_root,
                        )
                        .unwrap_or_else(|_| raw.clone())
                    };
                    (raw, display)
                } else {
                    let raw = crate::infra::git::diff::get_raw_diff(
                        path,
                        pane.is_staged(),
                        &self.repo_root,
                    )
                    .unwrap_or_default();
                    let display = if self.tool == DiffTool::Raw {
                        raw.clone()
                    } else {
                        crate::infra::git::diff::get_display_diff(
                            path,
                            pane.is_staged(),
                            self.tool.name(),
                            self.diff.diff_pane_width,
                            &self.repo_root,
                        )
                        .unwrap_or_else(|_| raw.clone())
                    };
                    (raw, display)
                };

                let file_diff = if is_untracked {
                    FileDiff::default()
                } else {
                    parse_diff(&raw)
                };

                self.build_loaded_content(
                    raw,
                    display,
                    file_diff,
                    is_untracked,
                    force_ansi_rendering,
                    None,
                    false,
                    patch_content_offset,
                    Vec::new(),
                )
            }
            DiffContent::FullFile(source) => self.load_full_file_content(path, pane, source),
        };

        self.apply_loaded_diff_state(
            path,
            pane,
            diff_content,
            loaded.raw.clone(),
            loaded.display.clone(),
            loaded.file_diff.clone(),
            loaded.line_infos.clone(),
            loaded.display_line_count,
            loaded.raw_line_count,
            loaded.cached_display_text.clone(),
            loaded.content_annotation,
            loaded.full_file_copyable,
            loaded.full_file_content_offset,
            loaded.full_file_highlight_lines.clone(),
        );
        self.restore_saved_diff_scroll(path, pane, diff_content);

        self.insert_cached_diff(
            cache_key,
            CachedDiff {
                raw_diff: loaded.raw,
                display_diff: loaded.display,
                file_diff: loaded.file_diff,
                line_infos: loaded.line_infos,
                display_line_count: loaded.display_line_count,
                raw_line_count: loaded.raw_line_count,
                cached_display_text: loaded.cached_display_text,
                content_annotation: loaded.content_annotation,
                full_file_copyable: loaded.full_file_copyable,
                full_file_content_offset: loaded.full_file_content_offset,
                full_file_highlight_lines: loaded.full_file_highlight_lines,
            },
        );

        Ok(())
    }

    pub(super) fn clear_diff(&mut self) {
        self.remember_current_diff_scroll();
        self.diff.diff_content = DiffContent::Patch;
        self.diff.display_diff.clear();
        self.diff.raw_diff.clear();
        self.diff.file_diff = FileDiff::default();
        self.diff.current_file = None;
        self.diff.diff_origin = None;
        self.diff.diff_scroll = 0;
        self.diff.diff_cursor = 0;
        self.diff.hunk_cursor = 0;
        self.diff.line_infos.clear();
        self.diff.raw_line_count = 0;
        self.diff.display_line_count = 0;
        self.diff.cached_display_text = None;
        self.diff.content_annotation = None;
        self.diff.full_file_copyable = false;
        self.diff.full_file_content_offset = 0;
        self.diff.full_file_highlight_lines.clear();
    }

    /// Reload diff for the current file with the current origin
    pub(crate) fn reload_current_diff(&mut self) -> Result<()> {
        if let (Some(path), Some(pane)) = (self.diff.current_file.clone(), self.diff.diff_origin) {
            let prev_scroll = self.diff.diff_scroll;
            let prev_cursor = self.diff.diff_cursor;
            let prev_patch_cursor = self.diff.patch_cursor;
            // `apply_loaded_diff_state` unconditionally zeroes `full_file_cursor`/
            // `full_file_anchor` on every full-file load (needed for a fresh file
            // selection — see its own comment) — reload is the one full-file load that
            // must instead carry these over from before the call, or a same-file
            // refresh (`r`, or Delta's terminal-resize reflow) would silently snap the
            // cursor back to the top of the file (review_9 Finding 1).
            let prev_full_file_cursor = self.diff.full_file_cursor;
            let prev_full_file_anchor = self.diff.full_file_anchor;
            let prev_diff_content = self.diff.diff_content;
            let diff_content = if self.focus == Focus::InlineSelect {
                DiffContent::Patch
            } else if prev_diff_content == DiffContent::Patch
                && self.has_untracked_file_in_pane(pane, &path)
            {
                // The file's tracked status can change out from under an open Patch view —
                // e.g. an external `git rm --cached` while it's open, picked up by the next
                // `refresh_trees()` this reload follows. An untracked file has no patch of
                // its own to show in Patch view (`load_diff` falls back to the same bat
                // rendering full-file view shows — see `default_diff_content_for`), so
                // normalize it the same way a fresh selection would rather than leaving
                // Patch view displaying that content under the wrong label, with `v`
                // routed to the InlineSelect branch instead of full-file range-select
                // (review_11 Finding 1). Only when the *current* content is Patch — an
                // already-open FullFile view (reached via an explicit `f`/`F` before the
                // file went untracked) is left alone; the branches below already reload it
                // correctly as full-file.
                DiffContent::FullFile(FullFileSource::Current)
            } else {
                prev_diff_content
            };
            // Whether the branch above actually swapped modes (Patch -> FullFile due to
            // the untracked normalization) rather than reloading the same content as before —
            // distinguishes a *fresh* entry into full-file view (full_file_cursor/anchor
            // should stay at the zeroed values `apply_loaded_diff_state` just set, not
            // inherit whatever was last sitting in those fields from Patch view) from an
            // actual same-content full-file reload (which must restore them, see below).
            let content_changed = diff_content != prev_diff_content;
            // Before reflow: capture the patch cursor's semantic file line — and which
            // side (Current/Previous) it was found on — so it can be re-anchored after
            // reload instead of restored by raw index. Delta's side-by-side wrap can
            // renumber which display row a given index means (e.g. a pane-width change),
            // so a plain index restore can silently land on an unrelated file line
            // (review_8 Finding 5). Falling back to Previous when Current comes back
            // empty covers a cursor parked on a removed-only row, which only carries an
            // old-side number.
            let delta_anchor = if self.tool == DiffTool::Delta && diff_content == DiffContent::Patch
            {
                let viewport_offset = prev_patch_cursor.saturating_sub(prev_scroll);
                self.delta_patch_top_line_target(FullFileSource::Current)
                    .map(|line| (line, FullFileSource::Current))
                    .or_else(|| {
                        self.delta_patch_top_line_target(FullFileSource::Previous)
                            .map(|line| (line, FullFileSource::Previous))
                    })
                    .map(|(line, side)| (line, side, viewport_offset))
            } else {
                None
            };

            self.load_diff(&path, pane, diff_content)?;
            if content_changed {
                // The untracked-normalization branch above just swapped Patch for
                // FullFile(Current) — this is a fresh entry into full-file view, not a
                // same-content reload, so leave `diff_scroll`/`full_file_cursor`/
                // `full_file_anchor` at the zeroed values `apply_loaded_diff_state` already
                // set instead of carrying over Patch view's `prev_scroll`/
                // `prev_full_file_cursor`/`prev_full_file_anchor`, which index an entirely
                // different row space (patch display rows vs. full-file content lines).
            } else {
                let scroll_line_count = if self.focus == Focus::InlineSelect {
                    self.diff.raw_line_count
                } else {
                    self.diff.display_line_count
                };
                self.diff.diff_scroll = prev_scroll.min(scroll_line_count.saturating_sub(1));
                self.diff.diff_cursor = prev_cursor.min(self.diff.raw_line_count.saturating_sub(1));
                // `load_diff` already reset `patch_cursor` to the top of whichever viewport
                // `restore_saved_diff_scroll` restored — reconcile it with the pre-reload
                // cursor the same way `diff_scroll`/`diff_cursor` are reconciled above, so a
                // refresh doesn't silently snap the patch cursor to an unrelated row.
                if diff_content == DiffContent::Patch {
                    self.diff.patch_cursor =
                        prev_patch_cursor.min(self.diff.display_line_count.saturating_sub(1));
                    if let Some((line, side, viewport_offset)) = delta_anchor {
                        if let Some(new_row) = self.delta_display_row_for_line(line, side) {
                            self.diff.patch_cursor = new_row;
                            self.diff.diff_scroll = new_row.saturating_sub(viewport_offset);
                        }
                    }
                } else if diff_content.is_full_file() {
                    self.diff.full_file_cursor =
                        prev_full_file_cursor.min(self.diff.raw_line_count.saturating_sub(1));
                    // Only restore the anchor when the reloaded content is actually
                    // copyable/non-empty — i.e. `full_file_cursor_active()` would be true
                    // here (setting focus aside). Otherwise (the file went binary/unmerged/
                    // missing/empty on this reload) there's no real cursor for the anchor to
                    // pair with; keeping it anyway left a hidden `Some(0)` that would spring
                    // back into an unintended range the moment the file became real text
                    // again on a later reload, before the next `v` press (review_10 Finding 1).
                    self.diff.full_file_anchor = if self.diff.full_file_copyable
                        && self.diff.raw_line_count > 0
                    {
                        prev_full_file_anchor.map(|anchor| anchor.min(self.diff.raw_line_count - 1))
                    } else {
                        None
                    };
                }
            }
            // The reconciliation above overwrites whatever `restore_saved_diff_scroll`
            // (called inside `load_diff`) already clamped into view — re-clamp once more
            // against the current `diff_pane_height` so a reload never leaves the
            // always-on cursor rendered off-screen (review_8 Finding 3-A).
            self.follow_active_diff_cursor();
        }
        Ok(())
    }

    fn has_untracked_file_in_pane(&self, pane: TreePane, path: &str) -> bool {
        if self.is_commit() {
            return false;
        }
        self.tree(pane).has_untracked_file(path)
    }

    /// Which diff content a file selection should open in. An untracked file has no hunks
    /// of its own to show in patch view — `load_diff` falls back to `get_file_preview`'s
    /// bat rendering there too, the exact same content full-file view shows — so opening
    /// it in patch view first is just a redundant step before the inevitable `f` press.
    /// Skip straight to full-file view instead, where `v`/`y` line-range select already
    /// works; tracked files still open in patch view as before.
    pub(super) fn default_diff_content_for(&self, pane: TreePane, path: &str) -> DiffContent {
        if self.has_untracked_file_in_pane(pane, path) {
            DiffContent::FullFile(FullFileSource::Current)
        } else {
            DiffContent::Patch
        }
    }

    /// Whether the currently open file is untracked in the Unstaged pane — the only case
    /// `default_diff_content_for` routes into full-file view on its own (Staged never holds
    /// an untracked node; the Commit target has none at all, see `has_untracked_file_in_pane`).
    /// `f` toggles back to `Patch` here, but that's just `get_file_preview`'s bat
    /// rendering again under a different label — the same content full-file view already
    /// shows, minus the range-select cursor — so there is no real patch view to toggle to.
    pub(super) fn current_file_is_untracked_unstaged(&self) -> bool {
        match (self.diff.current_file.as_deref(), self.diff.diff_origin) {
            (Some(path), Some(pane @ TreePane::Unstaged)) => {
                self.has_untracked_file_in_pane(pane, path)
            }
            _ => false,
        }
    }

    pub(super) fn schedule_tree_preview(&mut self, pane: TreePane) {
        self.pending_tree_preview = Some(PendingTreePreview {
            pane,
            ready_at: Instant::now() + self.tree_preview_debounce,
        });
    }

    fn flush_pending_tree_preview_if_due(&mut self) {
        let pending = match self.pending_tree_preview.clone() {
            Some(p) => p,
            None => return,
        };
        if Instant::now() < pending.ready_at {
            return;
        }

        self.pending_tree_preview = None;
        if self.focused_pane() == Some(pending.pane) {
            self.tree_load_preview_for_pane(pending.pane);
        }
    }

    fn event_poll_timeout(&self) -> Duration {
        let base = Duration::from_millis(50);
        let Some(pending) = &self.pending_tree_preview else {
            return base;
        };

        let remaining = pending.ready_at.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            Duration::ZERO
        } else {
            remaining.min(base)
        }
    }

    fn refresh_latest_state(&mut self) -> Result<()> {
        let prev_focus = self.focus.clone();
        let current = self.diff.current_file.clone().zip(self.diff.diff_origin);
        self.clear_diff_cache();
        self.pending_tree_preview = None;

        self.refresh_trees()?;

        // Keep focus unless the current tree became empty.
        if self.is_commit() {
            self.focus = match prev_focus {
                Focus::DiffView | Focus::InlineSelect => Focus::DiffView,
                _ => Focus::Unstaged,
            };
        } else {
            match prev_focus {
                Focus::Unstaged
                    if self.tree.unstaged.is_empty() && !self.tree.staged.is_empty() =>
                {
                    self.focus = Focus::Staged;
                }
                Focus::Staged if self.tree.staged.is_empty() && !self.tree.unstaged.is_empty() => {
                    self.focus = Focus::Unstaged;
                }
                _ => {
                    self.focus = prev_focus;
                }
            }
        }

        match self.focus {
            Focus::Unstaged | Focus::Staged => {
                self.tree_load_preview();
            }
            Focus::DiffView | Focus::InlineSelect => {
                if current.is_some() {
                    // `reload_current_diff` already saves and restores `diff_scroll`/
                    // `diff_cursor`/`patch_cursor`/`full_file_cursor`/`full_file_anchor`
                    // itself (each clamped to the freshly reloaded content, and re-clamped
                    // into the viewport via `follow_active_diff_cursor`) — this used to
                    // redundantly re-clamp `diff_scroll`/`diff_cursor`/`full_file_cursor`
                    // against `raw_line_count` on top of that, and unconditionally zeroed
                    // `full_file_anchor` regardless of what `reload_current_diff` had just
                    // restored, silently dropping an active full-file range on every `r`
                    // refresh (review_9 Finding 1).
                    self.reload_current_diff()?;
                } else {
                    self.clear_diff();
                }
            }
        }

        self.status_message = Some(if let Some(label) = self.commit_label() {
            format!("Refreshed {}", label)
        } else {
            "Refreshed latest state".to_string()
        });
        Ok(())
    }

    fn searchable_lines_for_scope(&self, scope: SearchScope) -> Vec<String> {
        match scope {
            SearchScope::WorkingTree | SearchScope::CommitTree => Vec::new(),
            SearchScope::DiffView if self.full_file_cursor_active() => {
                // Search the raw file content directly rather than the rendered display
                // text, so matches land only on real content — not bat's border/`File:`/
                // gutter-number decoration — and match indices are already in the same
                // raw-line space `full_file_cursor` uses.
                self.diff.raw_diff.lines().map(str::to_string).collect()
            }
            SearchScope::DiffView => {
                if let Some(text) = &self.diff.cached_display_text {
                    text.lines
                        .iter()
                        .map(|line| {
                            line.spans
                                .iter()
                                .map(|span| span.content.as_ref())
                                .collect::<String>()
                        })
                        .collect()
                } else {
                    self.diff.display_diff.lines().map(str::to_string).collect()
                }
            }
            SearchScope::InlineSelect => self.diff.raw_diff.lines().map(str::to_string).collect(),
        }
    }

    fn tree_linear_position(&self, pane: TreePane, node_idx: usize) -> usize {
        match pane {
            TreePane::Unstaged => node_idx,
            TreePane::Staged => self.tree.unstaged.all_nodes.len() + node_idx,
        }
    }

    fn decode_tree_linear_position(
        &self,
        scope: SearchScope,
        position: usize,
    ) -> Option<(TreePane, usize)> {
        match scope {
            SearchScope::WorkingTree => {
                let unstaged_len = self.tree.unstaged.all_nodes.len();
                if position < unstaged_len {
                    Some((TreePane::Unstaged, position))
                } else {
                    let staged_idx = position.checked_sub(unstaged_len)?;
                    (staged_idx < self.tree.staged.all_nodes.len())
                        .then_some((TreePane::Staged, staged_idx))
                }
            }
            SearchScope::CommitTree => (position < self.tree.unstaged.all_nodes.len())
                .then_some((TreePane::Unstaged, position)),
            _ => None,
        }
    }

    fn collect_search_matches(&self, scope: SearchScope, query: &str) -> Vec<usize> {
        if query.is_empty() {
            return Vec::new();
        }

        match scope {
            SearchScope::WorkingTree | SearchScope::CommitTree => {
                let panes: &[TreePane] = if scope == SearchScope::WorkingTree {
                    &[TreePane::Unstaged, TreePane::Staged]
                } else {
                    &[TreePane::Unstaged]
                };

                panes
                    .iter()
                    .flat_map(|&pane| {
                        self.tree(pane).all_nodes.iter().enumerate().filter_map(
                            move |(node_idx, node)| {
                                crate::components::highlight::contains_match(
                                    &node.display_row_text(pane),
                                    query,
                                )
                                .then_some(self.tree_linear_position(pane, node_idx))
                            },
                        )
                    })
                    .collect()
            }
            _ => self
                .searchable_lines_for_scope(scope)
                .into_iter()
                .enumerate()
                .filter_map(|(idx, line)| {
                    crate::components::highlight::contains_match(&line, query).then_some(idx)
                })
                .collect(),
        }
    }

    fn current_search_position(&self, scope: SearchScope) -> usize {
        match scope {
            SearchScope::WorkingTree | SearchScope::CommitTree => {
                let Some(pane) = self.focused_pane() else {
                    return 0;
                };
                let tree = self.tree(pane);
                let current_node_idx = tree.visible.get(tree.cursor).copied().unwrap_or(0);
                self.tree_linear_position(pane, current_node_idx)
            }
            SearchScope::DiffView if self.full_file_cursor_active() => self.diff.full_file_cursor,
            // Patch view's own always-on cursor is the "current position" search advances
            // from, not wherever the viewport happens to be scrolled to — otherwise `n`/`N`
            // can jump backward relative to where the cursor visibly sits.
            SearchScope::DiffView if self.patch_cursor_active() => self.diff.patch_cursor,
            SearchScope::DiffView => self.diff.diff_scroll,
            SearchScope::InlineSelect => self.diff.diff_cursor,
        }
    }

    fn apply_search_target(&mut self, scope: SearchScope, target: usize) {
        match scope {
            SearchScope::WorkingTree | SearchScope::CommitTree => {
                let Some((pane, node_idx)) = self.decode_tree_linear_position(scope, target) else {
                    return;
                };
                self.focus = pane.to_focus();
                self.tree_mut(pane).reveal_node(node_idx);
                self.schedule_tree_preview(pane);
            }
            SearchScope::DiffView if self.full_file_cursor_active() => {
                // `target` is a raw-line index here (searchable_lines_for_scope searched
                // raw_diff directly), so move the cursor and let the existing
                // viewport-follow helper derive the scroll position from it.
                self.diff.full_file_cursor = target.min(self.diff.raw_line_count.saturating_sub(1));
                self.follow_full_file_cursor();
            }
            SearchScope::DiffView if self.patch_cursor_active() => {
                // `target` is a display-row index (searchable_lines_for_scope's plain
                // DiffView branch searches `cached_display_text`/`display_diff`), the same
                // row space `patch_cursor` occupies — move the cursor itself, not just the
                // viewport, so it doesn't visually separate from the match it just jumped to.
                self.diff.patch_cursor = target.min(self.diff.display_line_count.saturating_sub(1));
                self.follow_patch_cursor();
                self.sync_hunk_cursor_from_patch_cursor();
            }
            SearchScope::DiffView => {
                self.diff.diff_scroll = target.min(self.diff.display_line_count.saturating_sub(1));
            }
            SearchScope::InlineSelect => {
                self.diff.diff_cursor = target.min(self.diff.raw_line_count.saturating_sub(1));
                self.sync_hunk_cursor();
                self.ensure_cursor_visible();
            }
        }
    }

    pub(super) fn begin_search(&mut self) {
        let Some(scope) = self.current_search_scope() else {
            return;
        };

        self.pending_tree_preview = None;
        self.search_input = Some(SearchInput {
            scope,
            query: String::new(),
        });
    }

    fn apply_confirmed_search(&mut self, scope: SearchScope, query: String) {
        if query.is_empty() {
            if self
                .search_state
                .as_ref()
                .is_some_and(|search| search.scope == scope)
            {
                self.search_state = None;
            }
            return;
        }

        self.search_state = Some(SearchState {
            scope,
            query: query.clone(),
        });

        let matches = self.collect_search_matches(scope, &query);
        if matches.is_empty() {
            self.error_message = Some(format!("No matches for /{}", query));
            return;
        }

        let current = self.current_search_position(scope);
        if let Some(target) = crate::components::search::next_match_from(&matches, current, true) {
            self.apply_search_target(scope, target);
        }
    }

    pub(super) fn navigate_search(&mut self, forward: bool) {
        let Some(scope) = self.current_search_scope() else {
            return;
        };

        let Some(query) = self
            .search_state
            .as_ref()
            .filter(|search| search.scope == scope)
            .map(|search| search.query.clone())
        else {
            self.error_message = Some("No active search in this pane".to_string());
            return;
        };

        let matches = self.collect_search_matches(scope, &query);
        if matches.is_empty() {
            self.error_message = Some(format!("No matches for /{}", query));
            return;
        }

        let current = self.current_search_position(scope);
        let target = if forward {
            crate::components::search::next_match_from(&matches, current, false)
        } else {
            crate::components::search::prev_match_from(&matches, current, false)
        };

        if let Some(target) = target {
            self.apply_search_target(scope, target);
        }
    }

    fn handle_search_input_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Esc => {
                self.search_input = None;
            }
            KeyCode::Enter => {
                if let Some(input) = self.search_input.take() {
                    self.apply_confirmed_search(input.scope, input.query);
                }
            }
            KeyCode::Backspace => {
                if let Some(input) = self.search_input.as_mut() {
                    input.query.pop();
                }
            }
            KeyCode::Char(ch)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if let Some(input) = self.search_input.as_mut() {
                    input.query.push(ch);
                }
            }
            _ => {}
        }

        Ok(())
    }

    fn execute_pending_action<B: Backend>(&mut self, terminal: &mut Terminal<B>) -> Result<()> {
        let Some(action) = self.pending_action.take() else {
            return Ok(());
        };

        match action {
            ExternalAction::Commit => {
                suspend_terminal(terminal)?;
                let command_result = crate::infra::git::run_interactive_command(
                    &self.commit_command,
                    &self.repo_root,
                );
                resume_terminal(terminal)?;
                terminal.clear()?;

                let refresh_result = self.refresh_latest_state();

                match command_result {
                    Ok(_) => {
                        if let Err(err) = refresh_result {
                            self.error_message = Some(format!("Refresh failed: {}", err));
                        } else {
                            self.status_message = Some("Commit command finished".to_string());
                        }
                    }
                    Err(err) => {
                        if let Err(refresh_err) = refresh_result {
                            self.error_message = Some(format!(
                                "Commit command failed: {}; refresh failed: {}",
                                err, refresh_err
                            ));
                        } else {
                            self.error_message = Some(format!("Commit command failed: {}", err));
                        }
                    }
                }
            }
        }

        Ok(())
    }

    // ─── Main event loop ─────────────────────────────────────────────────

    pub fn run<B: Backend>(&mut self, terminal: &mut Terminal<B>) -> Result<()> {
        loop {
            let size = terminal.size()?;
            let next_diff_width = self.compute_diff_pane_width(size.width);
            let diff_width_changed = self.diff.diff_pane_width != next_diff_width;
            let next_diff_height = size.height.saturating_sub(3) as usize;
            let diff_height_changed = self.diff.diff_pane_height != next_diff_height;

            self.diff.diff_pane_width = next_diff_width;
            self.diff.diff_pane_height = next_diff_height;

            if diff_width_changed
                && self.tool == DiffTool::Delta
                && self.diff.current_file.is_some()
            {
                let _ = self.reload_current_diff();
            }
            // A shrunk pane height isn't otherwise re-validated against either always-on
            // cursor's row until the next cursor-moving key press — without this, a cursor
            // left near the bottom of a tall pane can end up rendered off-screen right
            // after a resize, with nothing on screen to show it's still there.
            if diff_height_changed {
                self.follow_active_diff_cursor();
            }

            self.flush_pending_tree_preview_if_due();
            terminal.draw(|f| crate::views::render(f, self))?;

            let poll_timeout = self.event_poll_timeout();
            if crossterm::event::poll(poll_timeout)? {
                let mut saw_resize = false;
                loop {
                    match crossterm::event::read()? {
                        crossterm::event::Event::Key(key) => self.handle_key(key)?,
                        crossterm::event::Event::Resize(_, _) => {
                            saw_resize = true;
                        }
                        _ => {}
                    }

                    if self.pending_action.is_some() {
                        break;
                    }

                    if !crossterm::event::poll(Duration::ZERO)? {
                        break;
                    }
                }

                if saw_resize && self.tool == DiffTool::Delta && self.diff.current_file.is_some() {
                    let _ = self.reload_current_diff();
                }
            }

            if self.pending_action.is_some() {
                self.execute_pending_action(terminal)?;
            }

            if self.should_quit {
                break;
            }
        }
        Ok(())
    }

    // ─── Key handling ────────────────────────────────────────────────────

    fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        // Clears the `gg` pending state on any key that isn't a plain `g` press, even one
        // that never reaches `handle_diff_key` (global refresh, search input, other panes) —
        // a lone `g` followed by an unrelated key (including a modified `Ctrl+g`/`Alt+g`,
        // e.g. a `commit.key = "ctrl-g"` binding) must not let a later `g` complete a stale
        // sequence.
        if !is_plain_g(key) {
            self.diff.pending_g = false;
        }

        if self.search_input.is_some() {
            return self.handle_search_input_key(key);
        }

        self.error_message = None;
        self.status_message = None;

        if key.code == KeyCode::Char('r')
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
            && !self.can_trigger_commit_action(key)
        {
            self.refresh_latest_state()?;
            return Ok(());
        }

        match self.focus {
            Focus::Unstaged | Focus::Staged => self.handle_tree_key(key)?,
            Focus::DiffView => self.handle_diff_key(key)?,
            Focus::InlineSelect => self.handle_inline_select_key(key)?,
        }
        Ok(())
    }

    // ─── Tree key handling ──────────────────────────────────────────────

    pub(super) fn copy_path_to_clipboard(&mut self, path: &str) {
        match clipboard::copy_text(path) {
            Ok(_) => self.status_message = Some(format!("Copied path: {}", path)),
            Err(e) => self.error_message = Some(format!("Clipboard error: {}", e)),
        }
    }

    /// File line number (1-based) that the patch pane's cursor row (`patch_cursor`)
    /// corresponds to, on the given full-file `source` side. `None` means no reliable
    /// mapping is available (e.g. above the first hunk, or a display the mapping can't
    /// parse), in which case the caller should leave the full-file view at the top.
    /// Not guaranteed to be within the file's actual line range: a row sitting on a
    /// trailing `\ No newline at end of file` marker maps to one line past that side's
    /// last real line (EOF+1), since the marker itself doesn't consume a file line. Every
    /// current caller clamps the result against `raw_line_count`/`display_line_count`
    /// before using it, so this is safe today — but a future caller that skips that clamp
    /// would target a nonexistent line.
    pub(crate) fn patch_top_line_target(&self, source: FullFileSource) -> Option<usize> {
        match self.tool {
            DiffTool::Raw => self.raw_patch_top_line_target(source),
            DiffTool::Delta => self.delta_patch_top_line_target(source),
            DiffTool::Difftastic => None,
        }
    }

    /// Fallback for `patch_top_line_target` on an untracked file: there are no hunks to map
    /// through at all (`file_diff` is `FileDiff::default()`), but patch view isn't showing a
    /// diff either — it's `get_file_preview`'s rendering of the file's own content, the same
    /// content full-file view shows. So the patch cursor's row, minus that rendering's own
    /// leading decoration (threaded into `full_file_content_offset` for exactly this case by
    /// `load_diff`), is directly the file line it sits on.
    pub(crate) fn untracked_patch_line_target(&self, path: &str, pane: TreePane) -> Option<usize> {
        self.has_untracked_file_in_pane(pane, path).then(|| {
            self.diff
                .patch_cursor
                .saturating_sub(self.diff.full_file_content_offset)
                + 1
        })
    }

    /// Exact for `--tool raw`, since `display_diff` is `raw_diff` verbatim there, so
    /// `patch_cursor` indexes directly into `line_infos` (built from the same raw text).
    fn raw_patch_top_line_target(&self, source: FullFileSource) -> Option<usize> {
        let info = self.diff.line_infos.get(self.diff.patch_cursor)?;
        let hunk = self.diff.file_diff.hunks.get(info.hunk_idx?)?;

        let mut old_line = hunk.old_start;
        let mut new_line = hunk.new_start;

        if let Some(target_idx) = info.line_in_hunk {
            for line in hunk.lines.iter().take(target_idx) {
                match line {
                    DiffLine::Context(_) => {
                        old_line += 1;
                        new_line += 1;
                    }
                    DiffLine::Removed(_) => old_line += 1,
                    DiffLine::Added(_) => new_line += 1,
                }
            }
        }

        Some(match source {
            FullFileSource::Current => new_line as usize,
            FullFileSource::Previous => old_line as usize,
        })
    }

    /// Best-effort for `--tool delta` in side-by-side mode with line numbers enabled:
    /// reads the line number delta itself prints in the row's gutter, since delta
    /// reformats/wraps content and its output can't be walked like raw diff text.
    /// Returns `None` (graceful no-op) for any other delta configuration.
    fn delta_patch_top_line_target(&self, source: FullFileSource) -> Option<usize> {
        let lines: Vec<&str> = self.diff.display_diff.lines().collect();
        let total = lines.len();
        let mut row = self.diff.patch_cursor.min(total.checked_sub(1)?);

        // Rows outside any rendered hunk block — delta's leading blank line, the blank
        // gap between hunks, a file banner — don't belong to a file line at all. Skip
        // forward to the first row that's actually part of a hunk.
        while parse_delta_side_by_side_gutter(&strip_ansi_codes(lines[row])).is_none() {
            row += 1;
            if row >= total {
                return None;
            }
        }

        // From there, a blank number on the target side means this exact row is a
        // wrapped continuation, or an added/removed-only row — walk upward for the
        // nearest row within the same hunk block that has one.
        for candidate_row in (0..=row).rev() {
            let stripped = strip_ansi_codes(lines[candidate_row]);
            let Some((old_num, new_num)) = parse_delta_side_by_side_gutter(&stripped) else {
                break; // left the hunk block; don't bleed into an unrelated one above.
            };
            let candidate = match source {
                FullFileSource::Current => new_num,
                FullFileSource::Previous => old_num,
            };
            if let Some(n) = candidate {
                return Some(n as usize);
            }
        }
        None
    }

    /// Inverse of `delta_patch_top_line_target`: the display row whose gutter shows
    /// `target_line` on `source`'s side. Used by `reload_current_diff` to re-anchor
    /// `patch_cursor` after a Delta reflow (e.g. a pane-width change) — the same numeric
    /// row index can mean a different file line once delta rewraps its side-by-side
    /// output, so restoring `patch_cursor` by index alone can silently land it on an
    /// unrelated row.
    fn delta_display_row_for_line(
        &self,
        target_line: usize,
        source: FullFileSource,
    ) -> Option<usize> {
        self.diff
            .display_diff
            .lines()
            .enumerate()
            .find_map(|(row, line)| {
                let stripped = strip_ansi_codes(line);
                let (old_num, new_num) = parse_delta_side_by_side_gutter(&stripped)?;
                let candidate = match source {
                    FullFileSource::Current => new_num,
                    FullFileSource::Previous => old_num,
                };
                (candidate? as usize == target_line).then_some(row)
            })
    }

    // ─── Diff view key handling ─────────────────────────────────────────

    // ─── Inline select key handling ─────────────────────────────────────
}

// Helper to rebuild visible + clamp (avoids borrow issues)
fn rebuild_section_visible(section: &mut TreeSection) {
    section.rebuild_visible();
    section.clamp_cursor();
}

/// New path → pre-rename/copy path, for every `GitFile` that reports one. See
/// `App::rename_sources`'s doc comment for why `resolve_full_file_content_target` needs
/// this instead of just the (post-rename) `path` every other lookup uses.
fn build_rename_sources(files: &[GitFile]) -> HashMap<String, String> {
    files
        .iter()
        .filter_map(|f| {
            f.previous_path
                .as_ref()
                .map(|prev| (f.path.clone(), prev.clone()))
        })
        .collect()
}

/// Build tree nodes from a list of (path, staged, unstaged) tuples.
/// Preserves existing expansion states from `target_nodes`.
fn build_section(target_nodes: &mut Vec<TreeNode>, files: &[(String, char, char)]) {
    let prev_expanded: std::collections::HashMap<PathBuf, bool> = target_nodes
        .iter()
        .filter(|n| n.is_dir)
        .map(|n| (n.path.clone(), n.expanded))
        .collect();

    let mut file_children: BTreeMap<PathBuf, BTreeMap<PathBuf, (char, char)>> = BTreeMap::new();
    let mut dir_children: BTreeMap<PathBuf, BTreeSet<PathBuf>> = BTreeMap::new();

    for (path, staged, unstaged) in files {
        let fp = PathBuf::from(path);
        let parent = fp.parent().map(Path::to_path_buf).unwrap_or_default();
        file_children
            .entry(parent)
            .or_default()
            .insert(fp.clone(), (*staged, *unstaged));

        // Insert ancestor directories
        let mut ancestor = PathBuf::new();
        let components: Vec<_> = fp.components().collect();
        for (i, comp) in components.iter().enumerate() {
            ancestor = ancestor.join(comp);
            if i + 1 < components.len() {
                let parent = ancestor.parent().map(Path::to_path_buf).unwrap_or_default();
                dir_children
                    .entry(parent)
                    .or_default()
                    .insert(ancestor.clone());
            }
        }
    }

    fn push_nodes(
        parent: &Path,
        nodes: &mut Vec<TreeNode>,
        prev_expanded: &HashMap<PathBuf, bool>,
        file_children: &BTreeMap<PathBuf, BTreeMap<PathBuf, (char, char)>>,
        dir_children: &BTreeMap<PathBuf, BTreeSet<PathBuf>>,
    ) {
        if let Some(files) = file_children.get(parent) {
            for (path, (staged, unstaged)) in files {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.to_string_lossy().to_string());
                let depth = path.components().count().saturating_sub(1);

                nodes.push(TreeNode {
                    path: path.clone(),
                    name,
                    depth,
                    is_dir: false,
                    expanded: false,
                    staged: *staged,
                    unstaged: *unstaged,
                });
            }
        }

        if let Some(dirs) = dir_children.get(parent) {
            for path in dirs {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.to_string_lossy().to_string());
                let depth = path.components().count().saturating_sub(1);
                let expanded = *prev_expanded.get(path).unwrap_or(&true);

                nodes.push(TreeNode {
                    path: path.clone(),
                    name,
                    depth,
                    is_dir: true,
                    expanded,
                    staged: ' ',
                    unstaged: ' ',
                });

                push_nodes(path, nodes, prev_expanded, file_children, dir_children);
            }
        }
    }

    let mut nodes: Vec<TreeNode> = Vec::new();
    push_nodes(
        Path::new(""),
        &mut nodes,
        &prev_expanded,
        &file_children,
        &dir_children,
    );

    *target_nodes = nodes;
}

/// Whether `key` is an unmodified `g` press — the only input that may arm or complete the
/// `gg` jump-to-top sequence. `Ctrl+g`/`Alt+g` share `KeyCode::Char('g')` but are a different
/// keystroke entirely (e.g. a configurable `commit.key = "ctrl-g"` binding): they must neither
/// complete a pending sequence as if they were a second plain `g`, nor leave one armed for a
/// later unrelated `g` to complete.
pub(crate) fn is_plain_g(key: KeyEvent) -> bool {
    key.code == KeyCode::Char('g')
        && !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
}

/// Strips ANSI CSI sequences (e.g. `\x1b[38;2;255;0;0m`) so delta's colored output can be
/// parsed as plain text.
fn strip_ansi_codes(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// Parses a single stripped-ANSI row of `delta --side-by-side --line-numbers` output:
/// `│ OLD │ old content │ NEW │ new content`. Returns `(old_num, new_num)`, where each side
/// is `None` when its gutter is blank (a wrapped continuation row, or a row that only has
/// content on the other side). Returns `None` entirely when the row doesn't match this
/// layout at all (e.g. delta isn't in side-by-side/line-numbers mode, or it's a header row).
fn parse_delta_side_by_side_gutter(line: &str) -> Option<(Option<u32>, Option<u32>)> {
    let mut parts = line.splitn(5, '│');
    let leading = parts.next()?;
    if !leading.trim().is_empty() {
        return None;
    }
    let old_field = parts.next()?.trim();
    let _old_content = parts.next()?;
    let new_field = parts.next()?.trim();

    let parse_field = |field: &str| -> Option<Option<u32>> {
        if field.is_empty() {
            Some(None)
        } else {
            field.parse::<u32>().ok().map(Some)
        }
    };

    Some((parse_field(old_field)?, parse_field(new_field)?))
}

/// File line numbers (1-based) to give an added/removed background highlight in full-file
/// view: new-file line numbers of `+` lines for `Current`, old-file line numbers of `-`
/// lines for `Previous`. Ascending, since hunks and their lines are already in file order.
fn full_file_diff_highlight_lines(file_diff: &FileDiff, source: FullFileSource) -> Vec<u32> {
    let mut lines = Vec::new();
    for hunk in &file_diff.hunks {
        let mut old_line = hunk.old_start;
        let mut new_line = hunk.new_start;
        for line in &hunk.lines {
            match line {
                DiffLine::Context(_) => {
                    old_line += 1;
                    new_line += 1;
                }
                DiffLine::Removed(_) => {
                    if source == FullFileSource::Previous {
                        lines.push(old_line);
                    }
                    old_line += 1;
                }
                DiffLine::Added(_) => {
                    if source == FullFileSource::Current {
                        lines.push(new_line);
                    }
                    new_line += 1;
                }
            }
        }
    }
    lines
}

fn suspend_terminal<B: Backend>(terminal: &mut Terminal<B>) -> Result<()> {
    terminal.show_cursor()?;
    disable_raw_mode()?;

    let mut stdout = io::stdout();
    execute!(stdout, LeaveAlternateScreen, DisableMouseCapture)?;
    Ok(())
}

fn resume_terminal<B: Backend>(terminal: &mut Terminal<B>) -> Result<()> {
    enable_raw_mode()?;

    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    terminal.hide_cursor()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_revision_override_treats_null_commit_oid_as_none() {
        assert_eq!(
            normalize_revision_override(Some(NULL_COMMIT_OID.to_string())),
            None
        );
        assert_eq!(
            normalize_revision_override(Some("deadbeef".to_string())),
            Some("deadbeef".to_string())
        );
        assert_eq!(normalize_revision_override(None), None);
    }

    #[test]
    fn key_binding_parses_modifiers_and_named_keys() {
        let enter = KeyBinding::parse("enter").unwrap();
        assert!(enter.matches(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));

        let ctrl_g = KeyBinding::parse("ctrl-g").unwrap();
        assert!(ctrl_g.matches(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL)));
        assert!(!ctrl_g.matches(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE)));
    }

    #[test]
    fn tree_help_uses_u_for_working_tree_staging() {
        let app = make_test_app();

        assert!(app.tree_help_text().contains("[u]stage/unstage"));
        assert!(!app.tree_help_text().contains("[Enter]stage/unstage"));
    }

    #[test]
    fn tree_help_keeps_enter_for_commit_mode_open() {
        let mut app = make_test_app();
        set_commit(&mut app, "deadbeef");

        assert!(app.tree_help_text().contains("[l/Enter]open"));
    }

    #[test]
    fn inline_select_help_uses_u_for_apply() {
        let app = make_test_app();

        assert!(app.inline_select_help_text().contains("[u]apply"));
        assert!(!app.inline_select_help_text().contains("[Enter]apply"));
    }

    #[test]
    fn reveal_node_expands_hidden_file_path() {
        let mut nodes = Vec::new();
        build_section(&mut nodes, &[("src/nested/file.txt".to_string(), 'M', 'M')]);

        let mut section = TreeSection {
            all_nodes: nodes,
            visible: Vec::new(),
            cursor: 0,
        };
        section.rebuild_visible();

        let nested_dir = section
            .all_nodes
            .iter_mut()
            .find(|node| node.is_dir && node.path == Path::new("src/nested"))
            .unwrap();
        nested_dir.expanded = false;
        section.rebuild_visible();

        let file_idx = section
            .all_nodes
            .iter()
            .position(|node| node.path == Path::new("src/nested/file.txt"))
            .unwrap();
        section.reveal_node(file_idx);

        assert!(section.visible.iter().any(|&idx| idx == file_idx));
        assert_eq!(
            section.current_node().unwrap().path,
            Path::new("src/nested/file.txt")
        );
    }

    #[test]
    fn move_cursor_to_first_file_skips_directory_entries() {
        let mut nodes = Vec::new();
        build_section(
            &mut nodes,
            &[
                ("src/nested/a.txt".to_string(), 'M', 'M'),
                ("src/nested/b.txt".to_string(), 'M', 'M'),
            ],
        );

        let mut section = TreeSection {
            all_nodes: nodes,
            visible: Vec::new(),
            cursor: 0,
        };
        section.rebuild_visible();

        assert!(section.current_node().unwrap().is_dir);

        section.move_cursor_to_first_file();

        assert_eq!(
            section.current_node().unwrap().path,
            Path::new("src/nested/a.txt")
        );
    }

    #[test]
    fn build_rename_sources_maps_new_path_to_old_path_and_skips_non_renames() {
        let files = vec![
            GitFile {
                path: "new.rs".to_string(),
                previous_path: Some("old.rs".to_string()),
                staged: 'R',
                unstaged: 'R',
            },
            GitFile {
                path: "unrelated.rs".to_string(),
                previous_path: None,
                staged: 'M',
                unstaged: 'M',
            },
        ];

        let sources = build_rename_sources(&files);

        assert_eq!(sources.get("new.rs").map(String::as_str), Some("old.rs"));
        assert_eq!(sources.get("unrelated.rs"), None);
    }

    #[test]
    fn build_section_creates_directory_nodes_for_untracked_files() {
        let mut nodes = Vec::new();
        build_section(
            &mut nodes,
            &[
                ("hoge/a.txt".to_string(), '?', '?'),
                ("hoge/nested/b.txt".to_string(), '?', '?'),
            ],
        );

        assert!(nodes
            .iter()
            .any(|node| node.is_dir && node.path == Path::new("hoge")));
        assert!(nodes
            .iter()
            .any(|node| node.is_dir && node.path == Path::new("hoge/nested")));
        assert!(nodes.iter().any(|node| !node.is_dir
            && node.path == Path::new("hoge/a.txt")
            && node.is_untracked()));
        assert!(nodes.iter().any(|node| {
            !node.is_dir && node.path == Path::new("hoge/nested/b.txt") && node.is_untracked()
        }));
    }

    #[test]
    fn build_section_lists_direct_files_before_expanded_subdirectories() {
        let mut nodes = Vec::new();
        build_section(
            &mut nodes,
            &[
                ("aaa/bbb/ppp.txt".to_string(), '?', '?'),
                ("aaa/bbb.txt".to_string(), '?', '?'),
                ("aaa/ccc.txt".to_string(), '?', '?'),
            ],
        );

        let ordered_paths: Vec<_> = nodes
            .iter()
            .map(|node| node.path.to_string_lossy().to_string())
            .collect();

        assert_eq!(
            ordered_paths,
            vec![
                "aaa".to_string(),
                "aaa/bbb.txt".to_string(),
                "aaa/ccc.txt".to_string(),
                "aaa/bbb".to_string(),
                "aaa/bbb/ppp.txt".to_string(),
            ]
        );
    }

    #[test]
    fn tree_node_is_unmerged_covers_aa_and_dd_conflicts() {
        let both_added = TreeNode {
            path: PathBuf::from("aa.txt"),
            name: "aa.txt".to_string(),
            depth: 0,
            is_dir: false,
            expanded: false,
            staged: 'A',
            unstaged: 'A',
        };
        let both_deleted = TreeNode {
            path: PathBuf::from("dd.txt"),
            name: "dd.txt".to_string(),
            depth: 0,
            is_dir: false,
            expanded: false,
            staged: 'D',
            unstaged: 'D',
        };
        let modified = TreeNode {
            path: PathBuf::from("m.txt"),
            name: "m.txt".to_string(),
            depth: 0,
            is_dir: false,
            expanded: false,
            staged: 'M',
            unstaged: ' ',
        };

        assert!(both_added.is_unmerged());
        assert!(both_deleted.is_unmerged());
        assert!(!modified.is_unmerged());
    }

    #[test]
    fn commit_mode_added_file_is_not_treated_as_unmerged_in_full_file_logic() {
        let mut app = make_test_app();
        set_commit(&mut app, "deadbeef");
        seed_unstaged(&mut app, &[("added.txt".to_string(), 'A', 'A')]);

        let file_state = app
            .file_selection_state("added.txt", TreePane::Unstaged)
            .expect("commit-target file state should exist");

        assert_eq!(file_state.status, 'A');
        assert!(!file_state.is_unmerged);
    }

    fn make_test_app() -> App {
        App {
            should_quit: false,
            focus: Focus::Unstaged,
            config: Config::default(),
            tool: DiffTool::Raw,
            repo_root: PathBuf::new(),
            review_target: ReviewTarget::WorkingTree,
            tree_pane_percentage: 25,
            commit_key: KeyBinding::default_commit(),
            commit_command: vec!["git".to_string(), "commit".to_string(), "-v".to_string()],
            pending_action: None,
            tree: TreeViewState::new(),
            rename_sources: HashMap::new(),
            diff: DiffViewState {
                diff_pane_width: 80,
                ..DiffViewState::new()
            },
            pending_tree_preview: None,
            tree_preview_debounce: Duration::from_millis(TREE_PREVIEW_DEBOUNCE_MS),
            search_state: None,
            search_input: None,
            status_message: None,
            error_message: None,
        }
    }

    // Post-construction mutation helpers. These wrap fields whose shape has changed
    // (or may change again) across the app.rs split refactor (focus/diff_content/
    // review_target/tree sections), so a field-shape change updates one function body
    // instead of every call site below.
    fn enter_unstaged_tree(app: &mut App) {
        app.focus = Focus::Unstaged;
    }

    fn enter_staged_tree(app: &mut App) {
        app.focus = Focus::Staged;
    }

    fn enter_diff_view(app: &mut App) {
        app.focus = Focus::DiffView;
    }

    fn enter_inline_select(app: &mut App) {
        app.focus = Focus::InlineSelect;
    }

    fn set_patch_mode(app: &mut App) {
        app.diff.diff_content = DiffContent::Patch;
    }

    fn set_full_file_mode(app: &mut App, source: FullFileSource) {
        app.diff.diff_content = DiffContent::FullFile(source);
    }

    fn set_commit(app: &mut App, revision: &str) {
        app.review_target = ReviewTarget::Commit(revision.to_string());
    }

    fn seed_unstaged(app: &mut App, files: &[(String, char, char)]) {
        build_section(&mut app.tree.unstaged.all_nodes, files);
        rebuild_section_visible(&mut app.tree.unstaged);
    }

    fn seed_staged(app: &mut App, files: &[(String, char, char)]) {
        build_section(&mut app.tree.staged.all_nodes, files);
        rebuild_section_visible(&mut app.tree.staged);
    }

    #[test]
    fn working_tree_search_matches_both_sections() {
        let mut app = make_test_app();
        seed_unstaged(&mut app, &[("src/alpha.rs".to_string(), ' ', 'M')]);
        seed_staged(&mut app, &[("src/beta.rs".to_string(), 'M', ' ')]);

        let matches = app.collect_search_matches(SearchScope::WorkingTree, "beta");
        let staged_file_idx = app
            .tree
            .staged
            .all_nodes
            .iter()
            .position(|node| node.path == Path::new("src/beta.rs"))
            .unwrap();

        assert_eq!(
            matches,
            vec![app.tree_linear_position(TreePane::Staged, staged_file_idx)]
        );
    }

    #[test]
    fn working_tree_search_can_move_focus_to_staged_match() {
        let mut app = make_test_app();
        seed_unstaged(&mut app, &[("src/alpha.rs".to_string(), ' ', 'M')]);
        seed_staged(&mut app, &[("src/beta.rs".to_string(), 'M', ' ')]);

        let staged_file_idx = app
            .tree
            .staged
            .all_nodes
            .iter()
            .position(|node| node.path == Path::new("src/beta.rs"))
            .unwrap();
        let target = app.tree_linear_position(TreePane::Staged, staged_file_idx);

        app.apply_search_target(SearchScope::WorkingTree, target);

        assert_eq!(app.focus, Focus::Staged);
        assert_eq!(
            app.tree.staged.current_node().unwrap().path,
            Path::new("src/beta.rs")
        );
    }

    #[test]
    fn working_tree_search_matches_directory_nodes() {
        let mut app = make_test_app();
        seed_unstaged(
            &mut app,
            &[
                ("src/alpha.rs".to_string(), ' ', 'M'),
                ("tests/beta.rs".to_string(), ' ', 'M'),
            ],
        );

        let matches = app.collect_search_matches(SearchScope::WorkingTree, "src");
        let src_dir_idx = app
            .tree
            .unstaged
            .all_nodes
            .iter()
            .position(|node| node.path == Path::new("src"))
            .unwrap();

        assert_eq!(
            matches,
            vec![app.tree_linear_position(TreePane::Unstaged, src_dir_idx)]
        );
    }

    #[test]
    fn working_tree_search_can_move_to_directory_match() {
        let mut app = make_test_app();
        seed_unstaged(
            &mut app,
            &[
                ("src/alpha.rs".to_string(), ' ', 'M'),
                ("tests/beta.rs".to_string(), ' ', 'M'),
            ],
        );

        let tests_file_vis_idx = app
            .tree
            .unstaged
            .visible
            .iter()
            .position(|&idx| app.tree.unstaged.all_nodes[idx].path == Path::new("tests/beta.rs"))
            .unwrap();
        app.tree.unstaged.cursor = tests_file_vis_idx;

        app.apply_confirmed_search(SearchScope::WorkingTree, "src".to_string());

        assert_eq!(
            app.tree.unstaged.current_node().unwrap().path,
            Path::new("src")
        );
    }

    #[test]
    fn navigate_search_moves_to_hidden_directory_match() {
        let mut app = make_test_app();
        seed_unstaged(
            &mut app,
            &[
                ("alpha.txt".to_string(), ' ', 'M'),
                ("src/nested/file.txt".to_string(), ' ', 'M'),
            ],
        );

        let src_dir_idx = app
            .tree
            .unstaged
            .all_nodes
            .iter()
            .position(|node| node.is_dir && node.path == Path::new("src"))
            .unwrap();
        app.tree.unstaged.all_nodes[src_dir_idx].expanded = false;
        rebuild_section_visible(&mut app.tree.unstaged);
        app.tree.unstaged.cursor = 0;
        app.search_state = Some(SearchState {
            scope: SearchScope::WorkingTree,
            query: "nested".to_string(),
        });

        app.navigate_search(true);

        let current = app.tree.unstaged.current_node().unwrap();
        assert!(current.is_dir);
        assert_eq!(current.path, Path::new("src/nested"));
        assert!(app.tree.unstaged.all_nodes[src_dir_idx].expanded);
    }

    #[test]
    fn tree_commit_key_sets_pending_action() {
        let mut app = make_test_app();

        app.handle_tree_key(KeyEvent::new(KeyCode::Char('C'), KeyModifiers::NONE))
            .unwrap();

        assert_eq!(app.pending_action, Some(ExternalAction::Commit));
    }

    #[test]
    fn active_view_groups_unstaged_staged_as_workspace_and_diffview_inline_select_as_diff() {
        let mut app = make_test_app();

        enter_unstaged_tree(&mut app);
        assert_eq!(app.active_view(), ActiveView::Workspace);

        enter_staged_tree(&mut app);
        assert_eq!(app.active_view(), ActiveView::Workspace);

        enter_diff_view(&mut app);
        assert_eq!(app.active_view(), ActiveView::Diff);

        enter_inline_select(&mut app);
        assert_eq!(app.active_view(), ActiveView::Diff);
    }

    #[test]
    fn tree_title_shows_files_in_commit_mode_and_pane_label_in_working_tree_mode() {
        let mut app = make_test_app();
        assert_eq!(app.tree_title(TreePane::Unstaged), "Unstaged");
        assert_eq!(app.tree_title(TreePane::Staged), "Staged");

        set_commit(&mut app, "abc1234567890");
        assert_eq!(app.tree_title(TreePane::Unstaged), "Files");
        assert_eq!(app.tree_title(TreePane::Staged), "Files");
    }

    #[test]
    fn diff_origin_label_uses_pane_label_in_working_tree_mode_and_commit_label_in_commit_mode() {
        let mut app = make_test_app();
        assert_eq!(app.diff_origin_label(TreePane::Unstaged), "unstaged");
        assert_eq!(app.diff_origin_label(TreePane::Staged), "staged");

        set_commit(&mut app, "abc1234567890");
        // Both panes resolve to the same commit label under the Commit target, since Commit Files
        // is a single logical section rather than a real Staged pane (see refresh_trees).
        assert_eq!(app.diff_origin_label(TreePane::Unstaged), "commit abc12345");
        assert_eq!(app.diff_origin_label(TreePane::Staged), "commit abc12345");
    }

    #[test]
    fn begin_search_starts_with_empty_query() {
        let mut app = make_test_app();
        app.search_state = Some(SearchState {
            scope: SearchScope::WorkingTree,
            query: "hoge".to_string(),
        });

        app.begin_search();

        assert_eq!(app.search_input.as_ref().unwrap().query, "");
    }

    #[test]
    fn preview_line_infos_do_not_treat_diff_like_text_as_selectable() {
        let mut app = make_test_app();
        app.diff.line_infos = App::build_preview_line_infos_for(
            "@@ -1 +1 @@\n-looks like diff\n+but is file content\n",
        );

        assert_eq!(app.diff.line_infos.len(), 3);
        assert!(app.diff.line_infos.iter().all(|info| !info.is_selectable));
        assert!(app
            .diff
            .line_infos
            .iter()
            .all(|info| info.hunk_idx.is_none()));
    }

    #[test]
    fn diff_search_uses_cached_display_text_in_raw_mode() {
        let mut app = make_test_app();
        app.diff.display_diff = "\u{1b}[31mprin\u{1b}[0m\u{1b}[32mtln!\u{1b}[0m\n".to_string();
        app.diff.cached_display_text = Some(Text::from("println!\n"));

        let matches = app.collect_search_matches(SearchScope::DiffView, "println!");

        assert_eq!(matches, vec![0]);
    }

    #[test]
    fn files_under_dir_collects_nested_untracked_files() {
        let mut app = make_test_app();
        build_section(
            &mut app.tree.unstaged.all_nodes,
            &[
                ("hoge/a.txt".to_string(), '?', '?'),
                ("hoge/nested/b.txt".to_string(), '?', '?'),
            ],
        );

        let files = app.tree.unstaged.files_under_dir(Path::new("hoge"));

        assert_eq!(
            files,
            vec!["hoge/a.txt".to_string(), "hoge/nested/b.txt".to_string()]
        );
    }

    #[test]
    fn diff_content_toggles_requested_full_file_source() {
        assert_eq!(
            DiffContent::Patch.toggle_full_file(FullFileSource::Current),
            DiffContent::FullFile(FullFileSource::Current)
        );
        assert_eq!(
            DiffContent::Patch.toggle_full_file(FullFileSource::Previous),
            DiffContent::FullFile(FullFileSource::Previous)
        );
        assert_eq!(
            DiffContent::FullFile(FullFileSource::Current)
                .toggle_full_file(FullFileSource::Current),
            DiffContent::Patch
        );
        assert_eq!(
            DiffContent::FullFile(FullFileSource::Current)
                .toggle_full_file(FullFileSource::Previous),
            DiffContent::FullFile(FullFileSource::Previous)
        );
    }

    #[test]
    fn diff_help_text_switches_toggle_labels_in_full_file_views() {
        let mut app = make_test_app();

        assert!(app.diff_help_text().contains("[f]file"));
        assert!(app.diff_help_text().contains("[F]prev-file"));
        assert!(!app.diff_help_text().contains("[P]copy-file"));

        set_full_file_mode(&mut app, FullFileSource::Current);
        assert!(app.diff_help_text().contains("[f]diff"));
        assert!(app.diff_help_text().contains("[F]prev-file"));
        assert!(app.diff_help_text().contains("[P]copy-file"));
        assert!(app.diff_help_text().contains("[v]select"));
        assert!(app.diff_help_text().contains("[y]copy"));

        set_full_file_mode(&mut app, FullFileSource::Previous);
        assert!(app.diff_help_text().contains("[f]file"));
        assert!(app.diff_help_text().contains("[F]diff"));
    }

    #[test]
    fn diff_help_text_omits_the_f_hint_for_an_untracked_unstaged_file() {
        let mut app = make_test_app();
        seed_unstaged(&mut app, &[("new.txt".to_string(), '?', '?')]);
        app.diff.diff_origin = Some(TreePane::Unstaged);
        app.diff.current_file = Some("new.txt".to_string());
        set_full_file_mode(&mut app, FullFileSource::Current);

        let help = app.diff_help_text();
        assert!(!help.contains("[f]"));
        assert!(help.contains("[F]prev-file"));
    }

    #[test]
    fn diff_help_text_omits_the_capital_f_hint_for_an_untracked_unstaged_file_on_the_previous_side()
    {
        // review_9 Finding 3: after one `F` press (`FullFile(Current)` ->
        // `FullFile(Previous)`), the untracked-file branch above only covered
        // `FullFile(Current)` — this covers the `Previous` side, where a second `F`
        // would otherwise be advertised even though `toggle_full_file_view` blocks it.
        let mut app = make_test_app();
        seed_unstaged(&mut app, &[("new.txt".to_string(), '?', '?')]);
        app.diff.diff_origin = Some(TreePane::Unstaged);
        app.diff.current_file = Some("new.txt".to_string());
        set_full_file_mode(&mut app, FullFileSource::Previous);

        let help = app.diff_help_text();
        assert!(!help.contains("[F]"));
        assert!(help.contains("[f]file"));
    }

    #[test]
    fn full_file_clipboard_text_requires_copyable_full_file_state() {
        let mut app = make_test_app();
        app.diff.current_file = Some("src/lib.rs".to_string());
        app.diff.raw_diff = "fn main() {}\n".to_string();

        assert_eq!(app.full_file_clipboard_text(), None);

        set_full_file_mode(&mut app, FullFileSource::Current);
        assert_eq!(app.full_file_clipboard_text(), None);

        app.diff.full_file_copyable = true;
        assert_eq!(app.full_file_clipboard_text(), Some("fn main() {}\n"));

        app.diff.current_file = None;
        assert_eq!(app.full_file_clipboard_text(), None);
    }

    fn make_cached_diff_with_lines(line_count: usize) -> CachedDiff {
        let raw_diff = (0..line_count)
            .map(|idx| format!("line {}", idx))
            .collect::<Vec<_>>()
            .join("\n");
        let line_infos = App::build_preview_line_infos_for(&raw_diff);

        CachedDiff {
            display_diff: raw_diff.clone(),
            raw_diff: raw_diff.clone(),
            file_diff: FileDiff::default(),
            line_infos,
            display_line_count: raw_diff.lines().count(),
            raw_line_count: raw_diff.lines().count(),
            cached_display_text: None,
            content_annotation: None,
            full_file_copyable: false,
            full_file_content_offset: 0,
            full_file_highlight_lines: Vec::new(),
        }
    }

    fn seed_cached_view(
        app: &mut App,
        path: &str,
        pane: TreePane,
        diff_content: DiffContent,
        line_count: usize,
    ) {
        let key = app.build_diff_cache_key(path, pane, diff_content);
        app.insert_cached_diff(key, make_cached_diff_with_lines(line_count));
    }

    #[test]
    fn load_diff_restores_saved_scroll_for_patch_but_not_full_file() {
        let mut app = make_test_app();
        for diff_content in [
            DiffContent::Patch,
            DiffContent::FullFile(FullFileSource::Current),
            DiffContent::FullFile(FullFileSource::Previous),
        ] {
            seed_cached_view(
                &mut app,
                "file-a.txt",
                TreePane::Unstaged,
                diff_content,
                120,
            );
            seed_cached_view(
                &mut app,
                "file-b.txt",
                TreePane::Unstaged,
                diff_content,
                120,
            );
        }

        app.load_diff("file-a.txt", TreePane::Unstaged, DiffContent::Patch)
            .unwrap();
        app.diff.diff_scroll = 30;

        app.load_diff(
            "file-a.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Current),
        )
        .unwrap();
        assert_eq!(app.diff.diff_scroll, 0);
        app.diff.diff_scroll = 50;

        app.load_diff("file-b.txt", TreePane::Unstaged, DiffContent::Patch)
            .unwrap();
        assert_eq!(app.diff.diff_scroll, 0);
        app.diff.diff_scroll = 7;

        app.load_diff(
            "file-b.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Current),
        )
        .unwrap();
        assert_eq!(app.diff.diff_scroll, 0);
        app.diff.diff_scroll = 60;

        // Patch scroll is still remembered per file...
        app.load_diff("file-a.txt", TreePane::Unstaged, DiffContent::Patch)
            .unwrap();
        assert_eq!(app.diff.diff_scroll, 30);

        // ...but full file view always reopens at the top, regardless of prior scrolling.
        app.load_diff(
            "file-a.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Previous),
        )
        .unwrap();
        assert_eq!(app.diff.diff_scroll, 0);

        app.load_diff("file-b.txt", TreePane::Unstaged, DiffContent::Patch)
            .unwrap();
        assert_eq!(app.diff.diff_scroll, 7);

        app.load_diff(
            "file-b.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Previous),
        )
        .unwrap();
        assert_eq!(app.diff.diff_scroll, 0);
    }

    #[test]
    fn restore_saved_diff_scroll_follows_the_cursor_into_a_shrunk_pane() {
        // File A's cursor was left near the bottom of a 20-row-tall pane; the pane then
        // shrinks to 5 rows while file B is shown. Switching back to file A restores its
        // saved (scroll, cursor) pair as-is — without re-following the always-on cursor
        // into the now-smaller viewport, it would render off-screen until the next
        // cursor-moving key (review_8 Finding 3-A).
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        seed_cached_view(
            &mut app,
            "file-a.txt",
            TreePane::Unstaged,
            DiffContent::Patch,
            120,
        );
        seed_cached_view(
            &mut app,
            "file-b.txt",
            TreePane::Unstaged,
            DiffContent::Patch,
            120,
        );

        app.diff.diff_pane_height = 20;
        app.load_diff("file-a.txt", TreePane::Unstaged, DiffContent::Patch)
            .unwrap();
        app.diff.diff_scroll = 0;
        app.diff.patch_cursor = 15;

        // Switching away remembers file-a's (scroll, cursor) as left above.
        app.load_diff("file-b.txt", TreePane::Unstaged, DiffContent::Patch)
            .unwrap();
        app.diff.diff_pane_height = 5;

        app.load_diff("file-a.txt", TreePane::Unstaged, DiffContent::Patch)
            .unwrap();

        assert_eq!(app.diff.patch_cursor, 15);
        assert!(app.diff.diff_scroll <= app.diff.patch_cursor);
        assert!(app.diff.patch_cursor < app.diff.diff_scroll + app.diff.diff_pane_height);
    }

    /// A row with a search match, plus a non-matching span on the same row — shared by
    /// the three composition tests below, one per `ui::diff` overlay that calls
    /// `tint_line_bg` (review_8 Finding 1: the overlay must preserve the match's own
    /// background, not just `tint_line_bg` in isolation).
    fn row_with_a_search_match() -> Text<'static> {
        Text::from(vec![
            Line::from(Span::raw("no match here")),
            Line::from(vec![
                Span::raw("has a "),
                Span::styled(
                    "needle",
                    ratatui::style::Style::default()
                        .bg(crate::components::highlight::SEARCH_HIGHLIGHT_BG)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ),
                Span::raw(" in it"),
            ]),
            Line::from(Span::raw("no match either")),
        ])
    }

    #[test]
    fn apply_patch_cursor_preserves_a_search_matchs_yellow_background() {
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        set_patch_mode(&mut app);
        app.diff.display_line_count = 3;
        app.diff.patch_cursor = 1;

        let overlaid = crate::views::diff::apply_patch_cursor(row_with_a_search_match(), &app, 40);

        let cursor_line = &overlaid.lines[1];
        let match_span = cursor_line
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "needle")
            .unwrap();
        assert_eq!(
            match_span.style.bg,
            Some(crate::components::highlight::SEARCH_HIGHLIGHT_BG)
        );

        let non_match_span = cursor_line
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "has a ")
            .unwrap();
        assert_eq!(
            non_match_span.style.bg,
            Some(ratatui::style::Color::DarkGray)
        );
    }

    #[test]
    fn apply_full_file_cursor_preserves_a_search_matchs_yellow_background() {
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        set_full_file_mode(&mut app, FullFileSource::Current);
        app.diff.full_file_copyable = true;
        app.diff.raw_line_count = 3;
        app.diff.full_file_cursor = 1;

        let overlaid =
            crate::views::diff::apply_full_file_cursor(row_with_a_search_match(), &app, 40);

        let cursor_line = &overlaid.lines[1];
        let match_span = cursor_line
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "needle")
            .unwrap();
        assert_eq!(
            match_span.style.bg,
            Some(crate::components::highlight::SEARCH_HIGHLIGHT_BG)
        );

        let non_match_span = cursor_line
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "has a ")
            .unwrap();
        assert_eq!(
            non_match_span.style.bg,
            Some(ratatui::style::Color::DarkGray)
        );
    }

    #[test]
    fn apply_full_file_line_bg_preserves_a_search_matchs_yellow_background() {
        let mut app = make_test_app();
        set_full_file_mode(&mut app, FullFileSource::Current);
        app.diff.full_file_content_offset = 0;
        app.diff.full_file_highlight_lines = vec![2]; // 1-based file line 2 == row 1 (offset 0)

        let overlaid =
            crate::views::diff::apply_full_file_line_bg(row_with_a_search_match(), &app, 40);

        let tinted_line = &overlaid.lines[1];
        let match_span = tinted_line
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "needle")
            .unwrap();
        assert_eq!(
            match_span.style.bg,
            Some(crate::components::highlight::SEARCH_HIGHLIGHT_BG)
        );

        // The added/removed tint color itself is private to `ui::diff` — confirming it's
        // some color other than the search highlight's is enough to show the tint was
        // actually applied to the non-matching span.
        let non_match_span = tinted_line
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "has a ")
            .unwrap();
        assert!(non_match_span.style.bg.is_some());
        assert_ne!(
            non_match_span.style.bg,
            Some(crate::components::highlight::SEARCH_HIGHLIGHT_BG)
        );
    }

    #[test]
    fn reload_current_diff_preserves_the_patch_cursor_across_a_refresh() {
        let mut app = make_test_app();
        seed_cached_view(
            &mut app,
            "file.txt",
            TreePane::Unstaged,
            DiffContent::Patch,
            120,
        );

        app.load_diff("file.txt", TreePane::Unstaged, DiffContent::Patch)
            .unwrap();
        // Cursor sits deeper in the viewport than the scroll position — reproduces the
        // bug: `load_diff` (called internally by `reload_current_diff`) remembers and
        // restores only `diff_scroll` (the viewport top), which would reset
        // `patch_cursor` back to that same top-of-viewport row if `reload_current_diff`
        // didn't separately reconcile it against the pre-refresh cursor.
        app.diff.diff_scroll = 2;
        app.diff.patch_cursor = 8;

        app.reload_current_diff().unwrap();

        assert_eq!(app.diff.diff_scroll, 2);
        assert_eq!(app.diff.patch_cursor, 8);
    }

    #[test]
    fn reload_current_diff_preserves_the_full_file_cursor_and_anchor_across_a_refresh() {
        // review_9 Finding 1: `apply_loaded_diff_state` unconditionally zeroes
        // `full_file_cursor`/`full_file_anchor` on every full-file load (needed for a
        // fresh file selection), and `reload_current_diff` never saved/restored either
        // one — so a same-file refresh (`r`, or Delta's terminal-resize reflow, both of
        // which go through this function) silently snapped a deep cursor/range back to
        // the top of the file.
        let mut app = make_test_app();
        // `follow_active_diff_cursor` (exercised below) now requires `Focus::DiffView`
        // (review_9 Finding 2) — set it so the viewport-follow assertions below actually
        // exercise that call instead of it silently no-oping.
        enter_diff_view(&mut app);
        let mut full_cached = make_cached_diff_with_lines(120);
        full_cached.full_file_copyable = true;
        let key = app.build_diff_cache_key(
            "file.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Current),
        );
        app.insert_cached_diff(key, full_cached);

        app.load_diff(
            "file.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Current),
        )
        .unwrap();
        app.diff.full_file_cursor = 100;
        app.diff.full_file_anchor = Some(90);
        app.diff.diff_scroll = 91;

        app.reload_current_diff().unwrap();

        assert_eq!(app.diff.full_file_cursor, 100);
        assert_eq!(app.diff.full_file_anchor, Some(90));
        // `follow_active_diff_cursor` (called at the end of `reload_current_diff`) also
        // re-clamps the viewport so the restored cursor isn't left rendered off-screen.
        assert!(app.diff.diff_scroll <= app.diff.full_file_cursor);
        assert!(app.diff.full_file_cursor < app.diff.diff_scroll + app.diff.diff_pane_height);
    }

    #[test]
    fn reload_current_diff_clamps_the_full_file_cursor_and_anchor_to_a_shrunk_reloaded_file() {
        // Companion to the test above: the reload target can come back shorter than
        // before (e.g. lines were deleted) — the restored cursor/anchor must clamp to
        // the new, smaller `raw_line_count` instead of pointing past its end.
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        let mut full_cached = make_cached_diff_with_lines(120);
        full_cached.full_file_copyable = true;
        let key = app.build_diff_cache_key(
            "file.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Current),
        );
        app.insert_cached_diff(key, full_cached);

        app.load_diff(
            "file.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Current),
        )
        .unwrap();
        app.diff.full_file_cursor = 100;
        app.diff.full_file_anchor = Some(90);

        // `reload_current_diff` itself never clears the cache — a real "file got
        // shorter" reload always goes through `refresh_latest_state`, which calls
        // `clear_diff_cache()` first (see its own call site) so the stale 120-line
        // cache entry can't just get served back unchanged. Reproduce that here with a
        // fresh, shorter cache entry under the same key.
        app.clear_diff_cache();
        let mut shrunk_cached = make_cached_diff_with_lines(10);
        shrunk_cached.full_file_copyable = true;
        let key = app.build_diff_cache_key(
            "file.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Current),
        );
        app.insert_cached_diff(key, shrunk_cached);

        app.reload_current_diff().unwrap();

        assert_eq!(app.diff.full_file_cursor, 9);
        assert_eq!(app.diff.full_file_anchor, Some(9));
    }

    #[test]
    fn reload_current_diff_drops_the_full_file_anchor_when_the_reload_lands_on_unavailable_content()
    {
        // review_10 Finding 1: a reload landing on binary/unmerged/missing/empty content
        // (`full_file_copyable == false`, simulated here the same way
        // `full_file_unavailable_content` produces it) has no real cursor for an old
        // anchor to pair with. The anchor must be dropped, not just clamped — otherwise
        // it survives as a hidden `Some(0)` (`saturating_sub(1)` on a 0 line count) that
        // springs back into an unintended range the moment the file becomes real text
        // again on a *later* reload, before the user ever pressed `v`.
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        let mut full_cached = make_cached_diff_with_lines(120);
        full_cached.full_file_copyable = true;
        let key = app.build_diff_cache_key(
            "file.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Current),
        );
        app.insert_cached_diff(key, full_cached);

        app.load_diff(
            "file.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Current),
        )
        .unwrap();
        app.diff.full_file_cursor = 100;
        app.diff.full_file_anchor = Some(90);

        // Reload lands on unavailable content: `full_file_copyable` stays false, as
        // `make_cached_diff_with_lines` leaves it by default.
        app.clear_diff_cache();
        let unavailable_cached = make_cached_diff_with_lines(0);
        let key = app.build_diff_cache_key(
            "file.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Current),
        );
        app.insert_cached_diff(key, unavailable_cached);

        app.reload_current_diff().unwrap();

        assert!(!app.diff.full_file_copyable);
        assert_eq!(app.diff.full_file_anchor, None);

        // The file becomes real text again on a later reload — the dropped anchor must
        // not resurface; a fresh `v` press is required to start a new range.
        app.clear_diff_cache();
        let mut restored_cached = make_cached_diff_with_lines(120);
        restored_cached.full_file_copyable = true;
        let key = app.build_diff_cache_key(
            "file.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Current),
        );
        app.insert_cached_diff(key, restored_cached);

        app.reload_current_diff().unwrap();

        assert_eq!(app.diff.full_file_anchor, None);
    }

    #[test]
    fn reload_current_diff_drops_the_full_file_anchor_when_the_reload_lands_on_an_empty_file() {
        // Companion to the unavailable-content test above, covering the other half of
        // review_10 Finding 1's guard condition: `full_file_copyable` can stay `true` on
        // an empty file (see `full_file_v_and_y_fall_back_to_unavailable_messages_for_an_empty_file`)
        // while `raw_line_count == 0` — there's still no line for the anchor to pair
        // with, so it must be dropped here too, not just when `full_file_copyable` itself
        // is false.
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        let mut full_cached = make_cached_diff_with_lines(120);
        full_cached.full_file_copyable = true;
        let key = app.build_diff_cache_key(
            "file.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Current),
        );
        app.insert_cached_diff(key, full_cached);

        app.load_diff(
            "file.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Current),
        )
        .unwrap();
        app.diff.full_file_cursor = 100;
        app.diff.full_file_anchor = Some(90);

        // Reload lands on an empty (but copyable) file.
        app.clear_diff_cache();
        let mut empty_cached = make_cached_diff_with_lines(0);
        empty_cached.full_file_copyable = true;
        let key = app.build_diff_cache_key(
            "file.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Current),
        );
        app.insert_cached_diff(key, empty_cached);

        app.reload_current_diff().unwrap();

        assert!(app.diff.full_file_copyable);
        assert_eq!(app.diff.raw_line_count, 0);
        assert_eq!(app.diff.full_file_anchor, None);
    }

    #[test]
    fn reload_current_diff_normalizes_patch_mode_to_full_file_when_the_file_turns_untracked() {
        // review_11 Finding 1: a file open in Patch view can turn untracked out from
        // under it (e.g. an external `git rm --cached` while DiffView is open), picked up
        // by `refresh_trees()` right before this reload — normalize to `FullFile(Current)`
        // the same way a fresh selection of that file would (`default_diff_content_for`),
        // rather than leaving Patch view displaying the untracked bat-rendering fallback
        // under the wrong label, with `v` routed to InlineSelect instead of the full-file
        // range-select untracked files are supposed to get.
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        // Starts tracked and modified — Patch view has real hunks to show.
        seed_unstaged(&mut app, &[("file.txt".to_string(), ' ', 'M')]);

        seed_cached_view(
            &mut app,
            "file.txt",
            TreePane::Unstaged,
            DiffContent::Patch,
            120,
        );
        app.load_diff("file.txt", TreePane::Unstaged, DiffContent::Patch)
            .unwrap();
        app.diff.diff_scroll = 50;
        app.diff.patch_cursor = 55;
        assert_eq!(app.diff.diff_content, DiffContent::Patch);

        // The file turns untracked (simulates `refresh_trees()` picking up an external
        // `git rm --cached`) — same node count, different status pair.
        seed_unstaged(&mut app, &[("file.txt".to_string(), '?', '?')]);

        seed_cached_view(
            &mut app,
            "file.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Current),
            30,
        );

        app.reload_current_diff().unwrap();

        assert_eq!(
            app.diff.diff_content,
            DiffContent::FullFile(FullFileSource::Current)
        );
        // A fresh entry into full-file view, not a same-content reload — starts at the top
        // rather than inheriting Patch view's (diff_scroll=50, patch_cursor=55), which
        // indexed an entirely different row space (patch display rows).
        assert_eq!(app.diff.diff_scroll, 0);
        assert_eq!(app.diff.full_file_cursor, 0);
    }

    #[test]
    fn reload_current_diff_leaves_an_explicitly_opened_full_file_view_alone_when_the_file_turns_untracked(
    ) {
        // Companion to the test above: if the user had already switched to full-file view
        // themselves (`f`/`F`) before the file turned untracked, that choice must be left
        // alone — the Patch-view normalization only applies when the *current* content is
        // actually Patch.
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        seed_unstaged(&mut app, &[("file.txt".to_string(), ' ', 'M')]);

        seed_cached_view(
            &mut app,
            "file.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Current),
            120,
        );
        app.load_diff(
            "file.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Current),
        )
        .unwrap();
        app.diff.full_file_cursor = 100;

        seed_unstaged(&mut app, &[("file.txt".to_string(), '?', '?')]);

        seed_cached_view(
            &mut app,
            "file.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Current),
            120,
        );

        app.reload_current_diff().unwrap();

        assert_eq!(
            app.diff.diff_content,
            DiffContent::FullFile(FullFileSource::Current)
        );
        // Same content as before the reload — this is the ordinary same-content reload path, so
        // the cursor is preserved rather than reset to the top.
        assert_eq!(app.diff.full_file_cursor, 100);
    }

    #[test]
    fn reload_current_diff_reanchors_the_patch_cursor_after_a_delta_reflow() {
        // Delta's side-by-side wrap can renumber which display row a given numeric index
        // means when the pane width changes. These hand-written "wide" and "narrow"
        // gutter fixtures simulate exactly that: file line 2 sits on display row 1 in
        // `wide`, but a wrapped continuation row pushes it down to row 2 in `narrow`. A
        // plain index restore would leave `patch_cursor` on that continuation row instead
        // of file line 2 (review_8 Finding 5). This only exercises the re-anchor lookup
        // via fixtures — it does not run the real `delta` binary or its actual reflow.
        let mut app = make_test_app();
        app.tool = DiffTool::Delta;
        enter_diff_view(&mut app);
        set_patch_mode(&mut app);
        app.diff.diff_pane_height = 20;

        let wide = "│ 1  │old line 1  │ 1  │new line 1  \n\
│ 2  │old line 2  │ 2  │new line 2  \n\
│ 3  │old line 3  │ 3  │new line 3  \n";
        let narrow = "│ 1  │old line 1  │ 1  │new line 1  \n\
│    │            │    │continuation\n\
│ 2  │old line 2  │ 2  │new line 2  \n\
│ 3  │old line 3  │ 3  │new line 3  \n";

        app.diff.diff_pane_width = 80;
        let mut wide_cached = make_cached_diff_with_lines(0);
        wide_cached.display_diff = wide.to_string();
        wide_cached.raw_diff = wide.to_string();
        wide_cached.display_line_count = wide.lines().count();
        wide_cached.raw_line_count = wide.lines().count();
        let wide_key = app.build_diff_cache_key("file.txt", TreePane::Unstaged, DiffContent::Patch);
        app.insert_cached_diff(wide_key, wide_cached);

        app.load_diff("file.txt", TreePane::Unstaged, DiffContent::Patch)
            .unwrap();
        // patch_cursor sits on display row 1 ("new line 2"), one row below scroll's top.
        app.diff.diff_scroll = 0;
        app.diff.patch_cursor = 1;

        app.diff.diff_pane_width = 40;
        let mut narrow_cached = make_cached_diff_with_lines(0);
        narrow_cached.display_diff = narrow.to_string();
        narrow_cached.raw_diff = narrow.to_string();
        narrow_cached.display_line_count = narrow.lines().count();
        narrow_cached.raw_line_count = narrow.lines().count();
        let narrow_key =
            app.build_diff_cache_key("file.txt", TreePane::Unstaged, DiffContent::Patch);
        app.insert_cached_diff(narrow_key, narrow_cached);

        app.reload_current_diff().unwrap();

        // File line 2 now lands on row 2 in the reflowed (narrow) display — the cursor
        // re-anchors there, one row below scroll, preserving the same on-screen position.
        assert_eq!(app.diff.patch_cursor, 2);
        assert_eq!(app.diff.diff_scroll, 1);
    }

    #[test]
    fn clear_diff_preserves_saved_scroll_for_patch_but_not_full_file() {
        let mut app = make_test_app();
        seed_cached_view(
            &mut app,
            "file-a.txt",
            TreePane::Unstaged,
            DiffContent::Patch,
            120,
        );
        seed_cached_view(
            &mut app,
            "file-a.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Current),
            120,
        );

        app.load_diff("file-a.txt", TreePane::Unstaged, DiffContent::Patch)
            .unwrap();
        app.diff.diff_scroll = 25;

        app.load_diff(
            "file-a.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Current),
        )
        .unwrap();
        app.diff.diff_scroll = 40;

        app.clear_diff();

        app.load_diff("file-a.txt", TreePane::Unstaged, DiffContent::Patch)
            .unwrap();
        assert_eq!(app.diff.diff_scroll, 25);

        app.load_diff(
            "file-a.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Current),
        )
        .unwrap();
        assert_eq!(app.diff.diff_scroll, 0);
    }

    #[test]
    fn saved_scroll_is_tracked_separately_per_tree_pane_for_patch_only() {
        let mut app = make_test_app();
        for pane in [TreePane::Unstaged, TreePane::Staged] {
            for diff_content in [
                DiffContent::Patch,
                DiffContent::FullFile(FullFileSource::Current),
                DiffContent::FullFile(FullFileSource::Previous),
            ] {
                seed_cached_view(&mut app, "shared.txt", pane, diff_content, 120);
            }
        }

        app.load_diff("shared.txt", TreePane::Unstaged, DiffContent::Patch)
            .unwrap();
        app.diff.diff_scroll = 12;

        app.load_diff(
            "shared.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Current),
        )
        .unwrap();
        assert_eq!(app.diff.diff_scroll, 0);
        app.diff.diff_scroll = 56;

        app.load_diff("shared.txt", TreePane::Staged, DiffContent::Patch)
            .unwrap();
        assert_eq!(app.diff.diff_scroll, 0);
        app.diff.diff_scroll = 34;

        app.load_diff(
            "shared.txt",
            TreePane::Staged,
            DiffContent::FullFile(FullFileSource::Current),
        )
        .unwrap();
        assert_eq!(app.diff.diff_scroll, 0);
        app.diff.diff_scroll = 78;

        app.load_diff("shared.txt", TreePane::Unstaged, DiffContent::Patch)
            .unwrap();
        assert_eq!(app.diff.diff_scroll, 12);

        app.load_diff("shared.txt", TreePane::Staged, DiffContent::Patch)
            .unwrap();
        assert_eq!(app.diff.diff_scroll, 34);

        // Full file view never remembers scroll, in either pane.
        app.load_diff(
            "shared.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Previous),
        )
        .unwrap();
        assert_eq!(app.diff.diff_scroll, 0);

        app.load_diff(
            "shared.txt",
            TreePane::Staged,
            DiffContent::FullFile(FullFileSource::Previous),
        )
        .unwrap();
        assert_eq!(app.diff.diff_scroll, 0);
    }

    #[test]
    fn strip_ansi_codes_removes_csi_sequences() {
        assert_eq!(
            strip_ansi_codes("\u{1b}[38;2;255;0;0mred\u{1b}[0m plain"),
            "red plain"
        );
        assert_eq!(strip_ansi_codes("no codes here"), "no codes here");
    }

    #[test]
    fn parse_delta_side_by_side_gutter_extracts_numbers_and_blanks() {
        assert_eq!(
            parse_delta_side_by_side_gutter("│ 16 │old content │ 18 │new content"),
            Some((Some(16), Some(18)))
        );
        assert_eq!(
            parse_delta_side_by_side_gutter("│    │            │ 19 │added only"),
            Some((None, Some(19)))
        );
        assert_eq!(
            parse_delta_side_by_side_gutter("│    │            │    │wrapped continuation"),
            Some((None, None))
        );
    }

    #[test]
    fn parse_delta_side_by_side_gutter_rejects_non_matching_rows() {
        assert_eq!(parse_delta_side_by_side_gutter("Δ file.txt"), None);
        assert_eq!(
            parse_delta_side_by_side_gutter("just plain diff text, no gutter"),
            None
        );
    }

    #[test]
    fn full_file_diff_highlight_lines_collects_added_and_removed_line_numbers() {
        let raw = "diff --git a/file.txt b/file.txt\nindex 111..222 100644\n--- a/file.txt\n+++ b/file.txt\n@@ -10,3 +12,3 @@\n context1\n-removed1\n+added1\n context2\n";
        let file_diff = parse_diff(raw);

        assert_eq!(
            full_file_diff_highlight_lines(&file_diff, FullFileSource::Current),
            vec![13]
        );
        assert_eq!(
            full_file_diff_highlight_lines(&file_diff, FullFileSource::Previous),
            vec![11]
        );
    }

    #[test]
    fn full_file_diff_highlight_lines_covers_multiple_hunks_in_order() {
        // Hunk 1 contributes two consecutive additions (Current); hunk 2 contributes two
        // consecutive removals (Previous) — each side's result is a genuine multi-element
        // sorted list, which is what `apply_full_file_line_bg`'s `binary_search` relies on.
        let raw = "diff --git a/file.txt b/file.txt\nindex 111..222 100644\n--- a/file.txt\n+++ b/file.txt\n@@ -1,2 +1,4 @@\n context1\n+added1\n+added2\n context2\n@@ -20,4 +22,2 @@\n context3\n-removed1\n-removed2\n context4\n";
        let file_diff = parse_diff(raw);

        assert_eq!(
            full_file_diff_highlight_lines(&file_diff, FullFileSource::Current),
            vec![2, 3]
        );
        assert_eq!(
            full_file_diff_highlight_lines(&file_diff, FullFileSource::Previous),
            vec![21, 22]
        );
    }

    #[test]
    fn full_file_diff_highlight_lines_is_empty_without_hunks() {
        let file_diff = FileDiff::default();

        assert!(full_file_diff_highlight_lines(&file_diff, FullFileSource::Current).is_empty());
        assert!(full_file_diff_highlight_lines(&file_diff, FullFileSource::Previous).is_empty());
    }

    #[test]
    fn patch_cursor_active_requires_diff_view_focus_patch_mode_and_real_content() {
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        set_patch_mode(&mut app);
        app.diff.display_line_count = 10;
        assert!(app.patch_cursor_active());

        // Full-file view has its own cursor mechanism (full_file_cursor_active).
        set_full_file_mode(&mut app, FullFileSource::Current);
        assert!(!app.patch_cursor_active());

        // InlineSelect renders its own cursor over raw_diff via diff_cursor instead.
        set_patch_mode(&mut app);
        enter_inline_select(&mut app);
        assert!(!app.patch_cursor_active());

        // The tree pane merely previewing patch content (unfocused) must not show a
        // cursor the user never navigated to.
        enter_unstaged_tree(&mut app);
        assert!(!app.patch_cursor_active());

        // No content at all — no line for the cursor to sit on.
        enter_diff_view(&mut app);
        app.diff.display_line_count = 0;
        assert!(!app.patch_cursor_active());
    }

    #[test]
    fn patch_cursor_j_k_move_and_follow_viewport() {
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        set_patch_mode(&mut app);
        app.diff.display_line_count = 100;
        app.diff.diff_pane_height = 5;
        app.diff.patch_cursor = 0;
        app.diff.diff_scroll = 0;

        for _ in 0..6 {
            app.handle_diff_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE))
                .unwrap();
        }
        assert_eq!(app.diff.patch_cursor, 6);
        assert_eq!(app.diff.diff_scroll, 2);

        for _ in 0..6 {
            app.handle_diff_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE))
                .unwrap();
        }
        assert_eq!(app.diff.patch_cursor, 0);
        assert_eq!(app.diff.diff_scroll, 0);
    }

    #[test]
    fn patch_cursor_ctrl_d_u_jump_by_half_page_and_follow_viewport() {
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        set_patch_mode(&mut app);
        app.diff.display_line_count = 100;
        app.diff.diff_pane_height = 10;
        app.diff.patch_cursor = 0;
        app.diff.diff_scroll = 0;

        app.handle_diff_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL))
            .unwrap();
        app.handle_diff_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!(app.diff.patch_cursor, 10);
        assert_eq!(app.diff.diff_scroll, 1);

        app.handle_diff_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL))
            .unwrap();
        app.handle_diff_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!(app.diff.patch_cursor, 0);
        assert_eq!(app.diff.diff_scroll, 0);
    }

    #[test]
    fn patch_cursor_gg_and_shift_g_jump_to_first_and_last_line() {
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        set_patch_mode(&mut app);
        app.diff.display_line_count = 50;
        app.diff.diff_pane_height = 10;
        app.diff.patch_cursor = 25;
        app.diff.diff_scroll = 20;

        app.handle_diff_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE))
            .unwrap();
        app.handle_diff_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.diff.patch_cursor, 0);
        assert_eq!(app.diff.diff_scroll, 0);

        app.handle_diff_key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.diff.patch_cursor, 49);
        assert_eq!(app.diff.diff_scroll, 40);
    }

    #[test]
    fn patch_cursor_v_starts_inline_select_at_the_cursors_own_line_for_raw_tool() {
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        set_patch_mode(&mut app);
        app.tool = DiffTool::Raw;
        app.diff.display_line_count = 20;
        app.diff.diff_scroll = 2;
        app.diff.patch_cursor = 7;
        let raw = "diff --git a/file.txt b/file.txt\nindex 111..222 100644\n--- a/file.txt\n+++ b/file.txt\n@@ -1,3 +1,3 @@\n context\n-old\n+new\n";
        app.diff.file_diff = parse_diff(raw);

        app.handle_diff_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE))
            .unwrap();

        assert_eq!(app.focus, Focus::InlineSelect);
        assert_eq!(app.diff.diff_cursor, 7);
    }

    #[test]
    fn patch_cursor_v_starts_inline_select_at_the_viewport_top_for_delta_tool() {
        // Under --tool delta, display rows and raw rows diverge (side-by-side pairs two
        // raw lines into one row), so patch_cursor (a display-row index) is not a valid
        // raw_diff index — InlineSelect must keep starting from diff_scroll instead, same
        // as before the patch cursor existed, rather than landing on an unrelated line.
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        set_patch_mode(&mut app);
        app.tool = DiffTool::Delta;
        app.diff.display_line_count = 20;
        app.diff.diff_scroll = 2;
        app.diff.patch_cursor = 7;
        let raw = "diff --git a/file.txt b/file.txt\nindex 111..222 100644\n--- a/file.txt\n+++ b/file.txt\n@@ -1,3 +1,3 @@\n context\n-old\n+new\n";
        app.diff.file_diff = parse_diff(raw);

        app.handle_diff_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE))
            .unwrap();

        assert_eq!(app.focus, Focus::InlineSelect);
        assert_eq!(app.diff.diff_cursor, 2);
    }

    #[test]
    fn patch_cursor_v_is_blocked_in_commit_mode() {
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        set_patch_mode(&mut app);
        app.tool = DiffTool::Raw;
        set_commit(&mut app, "abc1234567890");
        app.diff.display_line_count = 20;
        let raw = "diff --git a/file.txt b/file.txt\nindex 111..222 100644\n--- a/file.txt\n+++ b/file.txt\n@@ -1,3 +1,3 @@\n context\n-old\n+new\n";
        app.diff.file_diff = parse_diff(raw);

        app.handle_diff_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE))
            .unwrap();

        assert_eq!(app.focus, Focus::DiffView);
        assert_eq!(
            app.error_message.as_deref(),
            Some("Commit diff is read-only")
        );
    }

    #[test]
    fn patch_cursor_v_is_blocked_when_tool_does_not_support_line_ops() {
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        set_patch_mode(&mut app);
        app.tool = DiffTool::Difftastic;
        app.diff.display_line_count = 20;
        let raw = "diff --git a/file.txt b/file.txt\nindex 111..222 100644\n--- a/file.txt\n+++ b/file.txt\n@@ -1,3 +1,3 @@\n context\n-old\n+new\n";
        app.diff.file_diff = parse_diff(raw);

        app.handle_diff_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE))
            .unwrap();

        assert_eq!(app.focus, Focus::DiffView);
        assert_eq!(
            app.error_message.as_deref(),
            Some("Line selection unavailable with difftastic")
        );
    }

    #[test]
    fn patch_cursor_v_is_blocked_when_there_are_no_hunks_to_select() {
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        set_patch_mode(&mut app);
        app.tool = DiffTool::Raw;
        app.diff.display_line_count = 20;
        app.diff.file_diff = FileDiff::default();

        app.handle_diff_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE))
            .unwrap();

        assert_eq!(app.focus, Focus::DiffView);
        assert_eq!(
            app.error_message.as_deref(),
            Some("No hunks to select lines from")
        );
    }

    #[test]
    fn inline_select_v_exit_follows_patch_cursor_back_into_view() {
        // While InlineSelect scrolled far from where patch_cursor was left, exiting back
        // to Patch view (`v`) must re-follow the always-on patch cursor into the
        // viewport — otherwise it renders off-screen until the next cursor-moving key
        // (review_8 Finding 3-B).
        let mut app = make_test_app();
        enter_inline_select(&mut app);
        app.diff.diff_pane_height = 10;
        app.diff.patch_cursor = 5;
        // InlineSelect scrolled independently, far past where patch_cursor sits.
        app.diff.diff_scroll = 90;
        app.diff.diff_cursor = 95;
        app.diff.raw_line_count = 200;

        app.handle_inline_select_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE))
            .unwrap();

        assert_eq!(app.focus, Focus::DiffView);
        assert!(app.diff.diff_scroll <= app.diff.patch_cursor);
        assert!(app.diff.patch_cursor < app.diff.diff_scroll + app.diff.diff_pane_height);
    }

    #[test]
    fn patch_view_search_advances_from_the_patch_cursor_not_the_viewport_scroll() {
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        set_patch_mode(&mut app);
        app.diff.display_diff = "aaa\nneedle\nccc\nddd\neee\nneedle\n".to_string();
        app.diff.display_line_count = 6;
        // Viewport scrolled to the top, but the cursor itself sits at row 3 — well past
        // the first match. If search used `diff_scroll` (0) as "current position" instead
        // of the cursor, it would jump to the match at row 1 instead of row 5.
        app.diff.diff_scroll = 0;
        app.diff.patch_cursor = 3;

        app.apply_confirmed_search(SearchScope::DiffView, "needle".to_string());
        assert_eq!(app.diff.patch_cursor, 5);

        // `n` (navigate_search) must also advance from the cursor's new position (5), not
        // from `diff_scroll` — under the old bug `diff_scroll` alone tracked search jumps
        // while `patch_cursor` never moved, so this would still show `patch_cursor == 3`.
        app.navigate_search(true);
        assert_eq!(app.diff.patch_cursor, 1);
    }

    #[test]
    fn jump_next_hunk_moves_the_patch_cursor_along_with_the_viewport() {
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        set_patch_mode(&mut app);
        app.tool = DiffTool::Raw;
        let raw = "diff --git a/file.txt b/file.txt\nindex 111..222 100644\n--- a/file.txt\n+++ b/file.txt\n@@ -1,2 +1,2 @@\n context\n-old\n+new\n@@ -20,2 +20,2 @@\n context2\n-old2\n+new2\n";
        app.diff.file_diff = parse_diff(raw);
        app.diff.display_diff = raw.to_string();
        app.diff.patch_cursor = 0;
        app.diff.diff_scroll = 0;

        app.handle_diff_key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::NONE))
            .unwrap();

        let second_hunk_row = raw.lines().position(|l| l == "@@ -20,2 +20,2 @@").unwrap();
        assert_eq!(app.diff.patch_cursor, second_hunk_row);
        assert_eq!(app.diff.diff_scroll, second_hunk_row);
    }

    #[test]
    fn patch_cursor_movement_keeps_hunk_cursor_aligned_so_bracket_keys_dont_jump_backward() {
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        set_patch_mode(&mut app);
        app.tool = DiffTool::Raw;
        app.diff.diff_pane_height = 20;
        let raw = "diff --git a/file.txt b/file.txt\nindex 111..222 100644\n--- a/file.txt\n+++ b/file.txt\n@@ -1,2 +1,2 @@\n context\n-old\n+new\n@@ -20,2 +20,2 @@\n context2\n-old2\n+new2\n@@ -40,2 +40,2 @@\n context3\n-old3\n+new3\n";
        app.diff.file_diff = parse_diff(raw);
        app.diff.display_diff = raw.to_string();
        app.diff.line_infos = App::build_patch_line_infos(raw);
        app.diff.display_line_count = raw.lines().count();
        let third_hunk_row = raw.lines().position(|l| l == "@@ -40,2 +40,2 @@").unwrap();
        let second_hunk_row = raw.lines().position(|l| l == "@@ -20,2 +20,2 @@").unwrap();
        let first_hunk_row = raw.lines().position(|l| l == "@@ -1,2 +1,2 @@").unwrap();
        app.diff.patch_cursor = first_hunk_row;
        app.diff.diff_scroll = 0;
        app.diff.hunk_cursor = 0;

        // Move the cursor with plain `j`, one row at a time, all the way from the first
        // hunk into the third — the same thing a reader does while reviewing a diff.
        for _ in first_hunk_row..third_hunk_row {
            app.handle_diff_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE))
                .unwrap();
        }
        assert_eq!(app.diff.patch_cursor, third_hunk_row);

        // `[` (previous hunk) must jump to the second hunk — the one right before wherever
        // the cursor actually is. Under the bug, `hunk_cursor` never left 0 during the `j`
        // presses above, so `[` would find `hunk_cursor == 0` and do nothing, landing back
        // on the *first* hunk instead — a jump backward past the second hunk entirely, even
        // though the cursor had visibly moved forward through it.
        app.handle_diff_key(KeyEvent::new(KeyCode::Char('['), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.diff.patch_cursor, second_hunk_row);
    }

    #[test]
    fn gg_from_inside_a_later_hunk_realigns_hunk_cursor_to_the_next_hunk_not_stale() {
        // `gg` lands `patch_cursor` on row 0, which is `diff --git` metadata — no hunk of
        // its own (`line_infos[0].hunk_idx == None`). Leaving `hunk_cursor` at its stale
        // value (2, from before `gg`) would make `]` do nothing (already "last hunk") and
        // the title still read "hunk 3/3", even though the cursor visibly jumped to the
        // very top of the file (review_8 Finding 2). Realigning to the *next* hunk at or
        // after the cursor means `hunk_cursor == 0` after `gg`, and `]` from there moves
        // forward to the second hunk — never backward.
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        set_patch_mode(&mut app);
        app.tool = DiffTool::Raw;
        app.diff.diff_pane_height = 20;
        let raw = "diff --git a/file.txt b/file.txt\nindex 111..222 100644\n--- a/file.txt\n+++ b/file.txt\n@@ -1,2 +1,2 @@\n context\n-old\n+new\n@@ -20,2 +20,2 @@\n context2\n-old2\n+new2\n@@ -40,2 +40,2 @@\n context3\n-old3\n+new3\n";
        app.diff.file_diff = parse_diff(raw);
        app.diff.display_diff = raw.to_string();
        app.diff.line_infos = App::build_patch_line_infos(raw);
        app.diff.display_line_count = raw.lines().count();
        let third_hunk_row = raw.lines().position(|l| l == "@@ -40,2 +40,2 @@").unwrap();
        let second_hunk_row = raw.lines().position(|l| l == "@@ -20,2 +20,2 @@").unwrap();
        app.diff.patch_cursor = third_hunk_row;
        app.diff.diff_scroll = 0;
        app.diff.hunk_cursor = 2;

        app.handle_diff_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE))
            .unwrap();
        app.handle_diff_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE))
            .unwrap();

        assert_eq!(app.diff.patch_cursor, 0);
        assert_eq!(app.diff.hunk_cursor, 0);

        app.handle_diff_key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.diff.patch_cursor, second_hunk_row);
    }

    #[test]
    fn returning_to_patch_view_realigns_the_hunk_cursor_with_the_restored_patch_cursor() {
        let mut app = make_test_app();
        app.tool = DiffTool::Raw;
        let raw = "diff --git a/file.txt b/file.txt\nindex 111..222 100644\n--- a/file.txt\n+++ b/file.txt\n@@ -1,2 +1,2 @@\n context\n-old\n+new\n@@ -20,2 +20,2 @@\n context2\n-old2\n+new2\n@@ -40,2 +40,2 @@\n context3\n-old3\n+new3\n";
        let file_diff = parse_diff(raw);
        let line_infos = App::build_patch_line_infos(raw);
        let third_hunk_row = raw.lines().position(|l| l == "@@ -40,2 +40,2 @@").unwrap();

        let key = app.build_diff_cache_key("file-a.txt", TreePane::Unstaged, DiffContent::Patch);
        app.insert_cached_diff(
            key,
            CachedDiff {
                raw_diff: raw.to_string(),
                display_diff: raw.to_string(),
                file_diff,
                line_infos,
                display_line_count: raw.lines().count(),
                raw_line_count: raw.lines().count(),
                cached_display_text: None,
                content_annotation: None,
                full_file_copyable: false,
                full_file_content_offset: 0,
                full_file_highlight_lines: Vec::new(),
            },
        );
        seed_cached_view(
            &mut app,
            "file-b.txt",
            TreePane::Unstaged,
            DiffContent::Patch,
            30,
        );

        app.load_diff("file-a.txt", TreePane::Unstaged, DiffContent::Patch)
            .unwrap();
        app.diff.diff_scroll = 0;
        app.diff.patch_cursor = third_hunk_row;
        app.diff.hunk_cursor = 2; // as if the cursor had genuinely been moved there via `j`

        // Switch away — `load_diff` remembers file-a's (scroll, cursor) — then back.
        // `apply_loaded_diff_state` unconditionally resets `hunk_cursor` to 0 on every
        // load; without realigning it from the restored `patch_cursor`, returning to
        // file-a would show hunk 1's title/jump-target even though the cursor is
        // restored to the third hunk.
        app.load_diff("file-b.txt", TreePane::Unstaged, DiffContent::Patch)
            .unwrap();
        app.load_diff("file-a.txt", TreePane::Unstaged, DiffContent::Patch)
            .unwrap();

        assert_eq!(app.diff.patch_cursor, third_hunk_row);
        assert_eq!(app.diff.hunk_cursor, 2);
    }

    #[test]
    fn follow_active_diff_cursor_reclaims_the_patch_cursor_after_the_pane_shrinks() {
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        set_patch_mode(&mut app);
        app.diff.display_line_count = 100;
        app.diff.diff_scroll = 40;
        app.diff.patch_cursor = 55;
        // A terminal resize shrinks the pane well below where the cursor sits on screen
        // (55 - 40 = 15 rows down) without moving `diff_scroll` or `patch_cursor` at all.
        app.diff.diff_pane_height = 5;

        app.follow_active_diff_cursor();

        assert_eq!(app.diff.diff_scroll, 51);
        assert!(app.diff.patch_cursor < app.diff.diff_scroll + app.diff.diff_pane_height);
    }

    #[test]
    fn follow_active_diff_cursor_reclaims_the_full_file_cursor_after_the_pane_shrinks() {
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        set_full_file_mode(&mut app, FullFileSource::Current);
        app.diff.full_file_copyable = true;
        app.diff.raw_line_count = 100;
        app.diff.full_file_content_offset = 3;
        app.diff.diff_scroll = 40;
        app.diff.full_file_cursor = 55;
        app.diff.diff_pane_height = 5;

        app.follow_active_diff_cursor();

        let cursor_display_row = app.diff.full_file_content_offset + app.diff.full_file_cursor;
        assert!(cursor_display_row < app.diff.diff_scroll + app.diff.diff_pane_height);
    }

    #[test]
    fn raw_patch_top_line_target_maps_context_added_removed_lines() {
        let mut app = make_test_app();
        app.tool = DiffTool::Raw;

        let raw = "diff --git a/file.txt b/file.txt\nindex 111..222 100644\n--- a/file.txt\n+++ b/file.txt\n@@ -10,3 +12,3 @@\n context1\n-removed1\n+added1\n context2\n";
        app.diff.file_diff = parse_diff(raw);
        app.diff.line_infos = App::build_patch_line_infos(raw);

        // Before the first hunk: no mapping.
        app.diff.patch_cursor = 0;
        assert_eq!(app.raw_patch_top_line_target(FullFileSource::Current), None);

        // The "@@" header row itself maps to the hunk's starting lines.
        app.diff.patch_cursor = 4;
        assert_eq!(
            app.raw_patch_top_line_target(FullFileSource::Current),
            Some(12)
        );
        assert_eq!(
            app.raw_patch_top_line_target(FullFileSource::Previous),
            Some(10)
        );

        // " context1" (first content row) — same as the hunk start.
        app.diff.patch_cursor = 5;
        assert_eq!(
            app.raw_patch_top_line_target(FullFileSource::Current),
            Some(12)
        );
        assert_eq!(
            app.raw_patch_top_line_target(FullFileSource::Previous),
            Some(10)
        );

        // "-removed1" — old side advanced past context1, new side unaffected by the removal.
        app.diff.patch_cursor = 6;
        assert_eq!(
            app.raw_patch_top_line_target(FullFileSource::Current),
            Some(13)
        );
        assert_eq!(
            app.raw_patch_top_line_target(FullFileSource::Previous),
            Some(11)
        );

        // "+added1" — new side advanced past context1, old side unaffected by the addition.
        app.diff.patch_cursor = 7;
        assert_eq!(
            app.raw_patch_top_line_target(FullFileSource::Current),
            Some(13)
        );
        assert_eq!(
            app.raw_patch_top_line_target(FullFileSource::Previous),
            Some(12)
        );

        // " context2" — both sides advanced past the added/removed lines.
        app.diff.patch_cursor = 8;
        assert_eq!(
            app.raw_patch_top_line_target(FullFileSource::Current),
            Some(14)
        );
        assert_eq!(
            app.raw_patch_top_line_target(FullFileSource::Previous),
            Some(12)
        );
    }

    #[test]
    fn raw_patch_top_line_target_skips_no_newline_markers_when_counting_hunk_lines() {
        let mut app = make_test_app();
        app.tool = DiffTool::Raw;

        // A one-line file with no trailing newline, changed on both sides: parse_diff()
        // never stores the "\ No newline..." marker rows in Hunk.lines, so counting them
        // as if they were real hunk lines would shift every row after the first marker.
        let raw = "diff --git a/file.txt b/file.txt\nindex 111..222 100644\n--- a/file.txt\n+++ b/file.txt\n@@ -1 +1 @@\n-old\n\\ No newline at end of file\n+new\n\\ No newline at end of file\n";
        app.diff.file_diff = parse_diff(raw);
        app.diff.line_infos = App::build_patch_line_infos(raw);

        // "-old" — nothing precedes it in the hunk yet.
        app.diff.patch_cursor = 5;
        assert_eq!(
            app.raw_patch_top_line_target(FullFileSource::Current),
            Some(1)
        );

        // "+new" — only the preceding "-old" should count; the no-newline marker between
        // them must not add a phantom extra line. Without the fix this maps to file line 2
        // instead of 1.
        app.diff.patch_cursor = 7;
        assert_eq!(
            app.raw_patch_top_line_target(FullFileSource::Current),
            Some(1)
        );

        // The marker row itself (right after "-old") should map the same as the real
        // content it sits next to — old side advanced past "old", new side hasn't reached
        // "new" yet — not reset all the way back to the hunk's start line (1). Only the
        // Previous side (old_line) actually differs from the buggy hunk-start value here.
        // Note this file has exactly one line: `Some(2)` for Previous is one past that
        // side's last real line (EOF+1), not a valid file line by itself — correct only
        // because every caller of this helper clamps against `raw_line_count` before use
        // (see `patch_top_line_target`'s doc comment).
        app.diff.patch_cursor = 6;
        assert_eq!(
            app.raw_patch_top_line_target(FullFileSource::Current),
            Some(1)
        );
        assert_eq!(
            app.raw_patch_top_line_target(FullFileSource::Previous),
            Some(2)
        );

        // The trailing marker row (after "+new") should likewise reflect both sides having
        // advanced past their one real line, not the hunk start — `Some(2)` for both sides
        // is EOF+1 here too, relying on the same caller-side clamp.
        app.diff.patch_cursor = 8;
        assert_eq!(
            app.raw_patch_top_line_target(FullFileSource::Current),
            Some(2)
        );
        assert_eq!(
            app.raw_patch_top_line_target(FullFileSource::Previous),
            Some(2)
        );
    }

    #[test]
    fn delta_patch_top_line_target_finds_nearest_gutter_number_above_blank_rows() {
        let mut app = make_test_app();
        app.tool = DiffTool::Delta;
        app.diff.display_diff = [
            "Δ file.txt",
            "──────────",
            "│ 10 │context1                          │ 12 │context1",
            "│    │                                  │ 13 │added1 that wraps across two rows↴",
            "│    │                                  │    │            …continued",
            "│ 11 │context2                          │ 14 │context2",
        ]
        .join("\n");

        // A row with numbers on both sides: read directly.
        app.diff.patch_cursor = 2;
        assert_eq!(
            app.delta_patch_top_line_target(FullFileSource::Current),
            Some(12)
        );
        assert_eq!(
            app.delta_patch_top_line_target(FullFileSource::Previous),
            Some(10)
        );

        // The wrapped continuation row (row 4) has blank gutters on both sides; search
        // upward finds row 3's new-side number, and row 2's old-side number (since the
        // addition never had an old-side line at all).
        app.diff.patch_cursor = 4;
        assert_eq!(
            app.delta_patch_top_line_target(FullFileSource::Current),
            Some(13)
        );
        assert_eq!(
            app.delta_patch_top_line_target(FullFileSource::Previous),
            Some(10)
        );
    }

    #[test]
    fn delta_patch_top_line_target_is_none_outside_side_by_side_line_number_format() {
        let mut app = make_test_app();
        app.tool = DiffTool::Delta;
        app.diff.display_diff = "just some diff text\nwithout a parseable gutter".to_string();
        app.diff.patch_cursor = 1;

        assert_eq!(
            app.delta_patch_top_line_target(FullFileSource::Current),
            None
        );
    }

    #[test]
    fn delta_patch_top_line_target_skips_forward_past_a_single_invalid_row() {
        let mut app = make_test_app();
        app.tool = DiffTool::Delta;
        app.diff.display_diff = [
            "",
            "│ 18 │                                   │ 18 │",
            "│ 19 │use crate::clipboard;              │ 19 │use crate::clipboard;",
        ]
        .join("\n");
        app.diff.patch_cursor = 0; // sitting on delta's leading blank line

        assert_eq!(
            app.delta_patch_top_line_target(FullFileSource::Current),
            Some(18)
        );
        assert_eq!(
            app.delta_patch_top_line_target(FullFileSource::Previous),
            Some(18)
        );
    }

    #[test]
    fn delta_patch_top_line_target_skips_forward_past_consecutive_invalid_rows() {
        let mut app = make_test_app();
        app.tool = DiffTool::Delta;
        app.diff.display_diff = [
            "",
            "",
            "",
            "│ 18 │                                   │ 18 │",
            "│ 19 │use crate::clipboard;              │ 19 │use crate::clipboard;",
        ]
        .join("\n");
        app.diff.patch_cursor = 0;

        assert_eq!(
            app.delta_patch_top_line_target(FullFileSource::Current),
            Some(18)
        );
    }

    #[test]
    fn full_file_cursor_active_requires_full_file_mode_and_copyable_content() {
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        app.diff.raw_line_count = 10;

        set_patch_mode(&mut app);
        app.diff.full_file_copyable = true;
        assert!(!app.full_file_cursor_active());

        set_full_file_mode(&mut app, FullFileSource::Current);
        app.diff.full_file_copyable = false;
        assert!(!app.full_file_cursor_active());

        app.diff.full_file_copyable = true;
        assert!(app.full_file_cursor_active());

        // An empty file/blob has no line for the cursor to sit on.
        app.diff.raw_line_count = 0;
        assert!(!app.full_file_cursor_active());
    }

    #[test]
    fn full_file_cursor_active_requires_diff_view_focus() {
        // review_9 Finding 2: an untracked/unstaged file's tree preview renders in
        // full-file view (`default_diff_content_for`) while `Focus::Unstaged`/`Focus::Staged`
        // is still active — without this check, that preview showed an apparently
        // operable cursor that `j`/`k`/`v`/`y` (gated on `Focus::DiffView` in
        // `handle_diff_key`) couldn't actually reach.
        let mut app = make_test_app();
        app.diff.raw_line_count = 10;
        set_full_file_mode(&mut app, FullFileSource::Current);
        app.diff.full_file_copyable = true;

        enter_unstaged_tree(&mut app);
        assert!(!app.full_file_cursor_active());

        enter_staged_tree(&mut app);
        assert!(!app.full_file_cursor_active());

        enter_diff_view(&mut app);
        assert!(app.full_file_cursor_active());
    }

    #[test]
    fn full_file_search_highlight_uses_gutter_requires_a_nonzero_content_offset() {
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        set_full_file_mode(&mut app, FullFileSource::Current);
        app.diff.full_file_copyable = true;
        app.diff.raw_line_count = 10;

        // bat's cat/plain-text fallback: no gutter was ever rendered.
        app.diff.full_file_content_offset = 0;
        assert!(!app.full_file_search_highlight_uses_gutter());

        // bat rendered its forced style: a real gutter is on screen.
        app.diff.full_file_content_offset = 1;
        assert!(app.full_file_search_highlight_uses_gutter());

        // Outside full-file view entirely, it's irrelevant regardless of the offset.
        set_patch_mode(&mut app);
        assert!(!app.full_file_search_highlight_uses_gutter());
    }

    #[test]
    fn full_file_select_j_k_move_and_follow_viewport() {
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        set_full_file_mode(&mut app, FullFileSource::Current);
        app.diff.full_file_copyable = true;
        app.diff.raw_line_count = 100;
        app.diff.full_file_content_offset = 3;
        app.diff.diff_pane_height = 5;
        app.diff.full_file_cursor = 0;
        app.diff.diff_scroll = 3;

        for _ in 0..6 {
            app.handle_diff_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE))
                .unwrap();
        }
        assert_eq!(app.diff.full_file_cursor, 6);
        assert_eq!(app.diff.diff_scroll, 5);

        for _ in 0..6 {
            app.handle_diff_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE))
                .unwrap();
        }
        assert_eq!(app.diff.full_file_cursor, 0);
        assert_eq!(app.diff.diff_scroll, 3);
    }

    #[test]
    fn full_file_select_v_toggles_anchor_on_and_off() {
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        set_full_file_mode(&mut app, FullFileSource::Current);
        app.diff.full_file_copyable = true;
        app.diff.raw_line_count = 20;
        app.diff.full_file_cursor = 4;

        app.handle_diff_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.diff.full_file_anchor, Some(4));
        assert_eq!(app.diff.full_file_cursor, 4);

        app.handle_diff_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.diff.full_file_anchor, None);
        assert_eq!(app.diff.full_file_cursor, 4);
    }

    #[test]
    fn full_file_v_and_y_fall_back_to_unavailable_messages_when_not_copyable() {
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        set_full_file_mode(&mut app, FullFileSource::Current);
        app.diff.full_file_copyable = false;

        app.handle_diff_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(
            app.error_message.as_deref(),
            Some("Line selection unavailable in full file view")
        );
        assert_eq!(app.diff.full_file_anchor, None);

        app.error_message = None;
        app.handle_diff_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.error_message.as_deref(), Some("No content to copy"));
    }

    #[test]
    fn full_file_v_and_y_fall_back_to_unavailable_messages_for_an_empty_file() {
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        set_full_file_mode(&mut app, FullFileSource::Current);
        // Copyable content, but there's no line for the cursor to sit on.
        app.diff.full_file_copyable = true;
        app.diff.raw_line_count = 0;

        app.handle_diff_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(
            app.error_message.as_deref(),
            Some("Line selection unavailable in full file view")
        );

        app.error_message = None;
        app.handle_diff_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.error_message.as_deref(), Some("No content to copy"));
    }

    #[test]
    fn current_file_is_untracked_unstaged_is_true_only_for_an_unstaged_untracked_selection() {
        let mut app = make_test_app();
        seed_unstaged(
            &mut app,
            &[
                ("tracked.txt".to_string(), ' ', 'M'),
                ("new.txt".to_string(), '?', '?'),
            ],
        );

        app.diff.diff_origin = Some(TreePane::Unstaged);
        app.diff.current_file = Some("new.txt".to_string());
        assert!(app.current_file_is_untracked_unstaged());

        app.diff.current_file = Some("tracked.txt".to_string());
        assert!(!app.current_file_is_untracked_unstaged());

        // No file open at all.
        app.diff.current_file = None;
        assert!(!app.current_file_is_untracked_unstaged());

        // A Staged-pane selection never carries `is_untracked()` in the first place (see
        // `refresh_trees`: `?` only ever lands in the Unstaged column), so this stays
        // false there even for a same-named file.
        app.diff.current_file = Some("new.txt".to_string());
        app.diff.diff_origin = Some(TreePane::Staged);
        assert!(!app.current_file_is_untracked_unstaged());
    }

    #[test]
    fn default_diff_content_for_opens_untracked_files_directly_in_full_file_view() {
        let mut app = make_test_app();
        seed_unstaged(
            &mut app,
            &[
                ("tracked.txt".to_string(), ' ', 'M'),
                ("new.txt".to_string(), '?', '?'),
            ],
        );

        assert_eq!(
            app.default_diff_content_for(TreePane::Unstaged, "tracked.txt"),
            DiffContent::Patch
        );
        assert_eq!(
            app.default_diff_content_for(TreePane::Unstaged, "new.txt"),
            DiffContent::FullFile(FullFileSource::Current)
        );
    }

    #[test]
    fn l_key_opens_an_untracked_file_directly_in_full_file_view_instead_of_patch() {
        let mut app = make_test_app();
        seed_unstaged(&mut app, &[("new.txt".to_string(), '?', '?')]);
        app.tree.unstaged.move_cursor_to_first_file();
        enter_unstaged_tree(&mut app);

        seed_cached_view(
            &mut app,
            "new.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Current),
            10,
        );

        app.tree_action_right().unwrap();

        assert_eq!(
            app.diff.diff_content,
            DiffContent::FullFile(FullFileSource::Current)
        );
        assert_eq!(app.focus, Focus::DiffView);
    }

    #[test]
    fn tree_preview_of_an_untracked_file_does_not_render_an_operable_full_file_cursor() {
        // review_9 Finding 2: moving the tree cursor over an untracked file (no `l`/Enter,
        // focus stays on the tree) loads it in full-file view for the preview
        // (`default_diff_content_for`) — `full_file_cursor_active` must stay false here, since
        // `j`/`k` in this state move the tree cursor, not this one (see
        // `full_file_cursor_active_requires_diff_view_focus`).
        let mut app = make_test_app();
        seed_unstaged(&mut app, &[("new.txt".to_string(), '?', '?')]);
        app.tree.unstaged.move_cursor_to_first_file();
        enter_unstaged_tree(&mut app);

        seed_cached_view(
            &mut app,
            "new.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Current),
            10,
        );

        app.tree_load_preview_for_pane(TreePane::Unstaged);

        assert_eq!(
            app.diff.diff_content,
            DiffContent::FullFile(FullFileSource::Current)
        );
        assert_eq!(app.focus, Focus::Unstaged);
        assert!(!app.full_file_cursor_active());
    }

    #[test]
    fn selecting_an_untracked_file_resets_a_stale_full_file_cursor_and_anchor_from_a_previous_file()
    {
        // A file previously viewed in full-file view can leave `full_file_cursor`/
        // `full_file_anchor` sitting deep inside its (possibly much longer) content.
        // Opening an untracked file now goes straight to full-file view too, bypassing
        // the patch-view step `toggle_full_file_view` normally resets these through —
        // without `apply_loaded_diff_state`'s own reset, this stale cursor would carry
        // over and could sit past the end of a much shorter file.
        let mut app = make_test_app();
        seed_unstaged(&mut app, &[("new.txt".to_string(), '?', '?')]);
        app.tree.unstaged.move_cursor_to_first_file();
        enter_unstaged_tree(&mut app);
        app.diff.full_file_cursor = 400;
        app.diff.full_file_anchor = Some(350);

        seed_cached_view(
            &mut app,
            "new.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Current),
            10,
        );

        app.tree_action_right().unwrap();

        assert_eq!(app.diff.full_file_cursor, 0);
        assert_eq!(app.diff.full_file_anchor, None);
    }

    #[test]
    fn f_key_is_disabled_for_an_untracked_unstaged_file() {
        let mut app = make_test_app();
        seed_unstaged(&mut app, &[("new.txt".to_string(), '?', '?')]);
        enter_diff_view(&mut app);
        app.diff.diff_origin = Some(TreePane::Unstaged);
        app.diff.current_file = Some("new.txt".to_string());
        set_full_file_mode(&mut app, FullFileSource::Current);
        app.diff.full_file_copyable = true;
        app.diff.raw_line_count = 10;
        app.diff.full_file_cursor = 4;

        app.handle_diff_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE))
            .unwrap();

        // `f` had no effect: still in full-file view, cursor untouched, no error raised
        // either — the key is silently a no-op rather than a fallback to Patch.
        assert_eq!(
            app.diff.diff_content,
            DiffContent::FullFile(FullFileSource::Current)
        );
        assert_eq!(app.diff.full_file_cursor, 4);
        assert_eq!(app.error_message, None);
    }

    #[test]
    fn pressing_shift_f_twice_on_an_untracked_unstaged_file_does_not_fall_through_to_patch_view() {
        // `F` on `FullFile(Current)` first moves to `FullFile(Previous)` (which resolves
        // to the unavailable-previous-side message for an untracked file — useful to see,
        // so not blocked). A *second* `F` from there would normally toggle back to
        // `Patch` (`toggle_full_file` matches `current == source`) — this is the other
        // route besides a direct `f` press that must stay blocked for such a file, since
        // it has no real patch view to land in.
        let mut app = make_test_app();
        seed_unstaged(&mut app, &[("new.txt".to_string(), '?', '?')]);
        enter_diff_view(&mut app);
        app.diff.diff_origin = Some(TreePane::Unstaged);
        app.diff.current_file = Some("new.txt".to_string());
        set_full_file_mode(&mut app, FullFileSource::Current);
        app.diff.full_file_copyable = true;
        app.diff.raw_line_count = 10;

        app.handle_diff_key(KeyEvent::new(KeyCode::Char('F'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(
            app.diff.diff_content,
            DiffContent::FullFile(FullFileSource::Previous)
        );

        app.handle_diff_key(KeyEvent::new(KeyCode::Char('F'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(
            app.diff.diff_content,
            DiffContent::FullFile(FullFileSource::Previous)
        );
    }

    #[test]
    fn f_key_still_toggles_to_patch_view_for_a_tracked_file() {
        let mut app = make_test_app();
        seed_unstaged(&mut app, &[("tracked.txt".to_string(), ' ', 'M')]);
        enter_diff_view(&mut app);
        app.diff.diff_origin = Some(TreePane::Unstaged);
        app.diff.current_file = Some("tracked.txt".to_string());
        set_full_file_mode(&mut app, FullFileSource::Current);

        seed_cached_view(
            &mut app,
            "tracked.txt",
            TreePane::Unstaged,
            DiffContent::Patch,
            10,
        );

        app.handle_diff_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE))
            .unwrap();

        assert_eq!(app.diff.diff_content, DiffContent::Patch);
    }

    #[test]
    fn h_key_keeps_an_untracked_files_tree_preview_in_full_file_view() {
        // Unlike a tracked file (see `full_file_h_clears_anchor_and_returns_to_patch_view`),
        // an untracked file's patch view has nothing of its own to show — it's the same
        // bat-rendered content full-file view shows (see `default_diff_content_for`) — so
        // leaving the diff view back to the tree must not fall back to `Patch` here.
        let mut app = make_test_app();
        seed_unstaged(&mut app, &[("new.txt".to_string(), '?', '?')]);
        enter_diff_view(&mut app);
        app.diff.diff_origin = Some(TreePane::Unstaged);
        app.diff.current_file = Some("new.txt".to_string());
        set_full_file_mode(&mut app, FullFileSource::Current);

        seed_cached_view(
            &mut app,
            "new.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Current),
            10,
        );

        app.leave_diff_view_to_tree().unwrap();

        assert_eq!(
            app.diff.diff_content,
            DiffContent::FullFile(FullFileSource::Current)
        );
        assert_eq!(app.focus, Focus::Unstaged);
        // review_9 Finding 2: this is exactly the state that used to render an
        // apparently-operable cursor over the tree preview — `full_file_cursor_active`
        // must stay false now that focus has left the diff pane.
        assert!(!app.full_file_cursor_active());
    }

    #[test]
    fn full_file_h_clears_anchor_and_returns_to_patch_view() {
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        set_full_file_mode(&mut app, FullFileSource::Current);
        app.diff.diff_origin = Some(TreePane::Unstaged);
        app.diff.full_file_anchor = Some(3);

        app.handle_diff_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE))
            .unwrap();

        assert_eq!(app.diff.full_file_anchor, None);
        assert_eq!(app.focus, Focus::Unstaged);
    }

    #[test]
    fn full_file_selection_text_handles_both_anchor_directions() {
        let mut app = make_test_app();
        app.diff.raw_diff = "line0\nline1\nline2\nline3\nline4".to_string();

        // No anchor: just the cursor's own line.
        app.diff.full_file_cursor = 2;
        app.diff.full_file_anchor = None;
        assert_eq!(app.full_file_selection_text(), "line2\n");

        // Anchor before cursor.
        app.diff.full_file_cursor = 3;
        app.diff.full_file_anchor = Some(1);
        assert_eq!(app.full_file_selection_text(), "line1\nline2\nline3\n");

        // Anchor after cursor — same range regardless of direction.
        app.diff.full_file_cursor = 1;
        app.diff.full_file_anchor = Some(3);
        assert_eq!(app.full_file_selection_text(), "line1\nline2\nline3\n");

        // A lone blank line is a legitimate 1-line selection, not an error case.
        app.diff.raw_diff = "line0\n\nline2".to_string();
        app.diff.full_file_cursor = 1;
        app.diff.full_file_anchor = None;
        assert_eq!(app.full_file_selection_text(), "\n");
    }

    #[test]
    fn full_file_select_ctrl_d_u_jump_by_half_page_and_follow_viewport() {
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        set_full_file_mode(&mut app, FullFileSource::Current);
        app.diff.full_file_copyable = true;
        app.diff.raw_line_count = 100;
        app.diff.full_file_content_offset = 3;
        app.diff.diff_pane_height = 10;
        app.diff.full_file_cursor = 0;
        app.diff.diff_scroll = 3;

        app.handle_diff_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL))
            .unwrap();
        app.handle_diff_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!(app.diff.full_file_cursor, 10);
        assert_eq!(app.diff.diff_scroll, 4);

        app.handle_diff_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL))
            .unwrap();
        app.handle_diff_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!(app.diff.full_file_cursor, 0);
        assert_eq!(app.diff.diff_scroll, 3);
    }

    #[test]
    fn full_file_select_gg_and_shift_g_jump_to_first_and_last_line() {
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        set_full_file_mode(&mut app, FullFileSource::Current);
        app.diff.full_file_copyable = true;
        app.diff.raw_line_count = 50;
        app.diff.full_file_content_offset = 3;
        app.diff.diff_pane_height = 10;
        app.diff.full_file_cursor = 25;
        app.diff.diff_scroll = 20;
        app.diff.full_file_anchor = Some(5);

        app.handle_diff_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE))
            .unwrap();
        app.handle_diff_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.diff.full_file_cursor, 0);
        assert_eq!(app.diff.diff_scroll, 3);
        assert_eq!(app.diff.full_file_anchor, Some(5));

        app.handle_diff_key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.diff.full_file_cursor, 49);
        assert_eq!(app.diff.diff_scroll, 43);
        assert_eq!(app.diff.full_file_anchor, Some(5));
    }

    #[test]
    fn full_file_lone_g_does_not_jump_and_clears_on_other_key() {
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        set_full_file_mode(&mut app, FullFileSource::Current);
        app.diff.full_file_copyable = true;
        app.diff.raw_line_count = 50;
        app.diff.full_file_content_offset = 3;
        app.diff.diff_pane_height = 10;
        app.diff.full_file_cursor = 25;
        app.diff.diff_scroll = 20;

        // A single 'g' only arms the pending state; it does not jump by itself.
        app.handle_diff_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.diff.full_file_cursor, 25);
        assert!(app.diff.pending_g);

        // Any other key clears the pending state, so a later lone 'g' doesn't complete a
        // stale sequence.
        app.handle_diff_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE))
            .unwrap();
        assert!(!app.diff.pending_g);

        app.handle_diff_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.diff.full_file_cursor, 26);
        assert!(app.diff.pending_g);
    }

    #[test]
    fn full_file_modified_g_does_not_complete_or_arm_a_pending_sequence() {
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        set_full_file_mode(&mut app, FullFileSource::Current);
        app.diff.full_file_copyable = true;
        app.diff.raw_line_count = 50;
        app.diff.full_file_cursor = 25;

        // A lone 'g' arms the pending state...
        app.handle_diff_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE))
            .unwrap();
        assert!(app.diff.pending_g);

        // ...but Alt+g is a different keystroke, not a second plain 'g': it must not
        // complete the jump, and it clears the stale pending state like any other key.
        app.handle_diff_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::ALT))
            .unwrap();
        assert_eq!(app.diff.full_file_cursor, 25);
        assert!(!app.diff.pending_g);

        // Ctrl+g must not arm a pending sequence either.
        app.handle_diff_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL))
            .unwrap();
        assert!(!app.diff.pending_g);
    }

    #[test]
    fn ctrl_g_commit_binding_does_not_leave_a_stale_pending_g_for_the_next_plain_g() {
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        set_full_file_mode(&mut app, FullFileSource::Current);
        app.diff.full_file_copyable = true;
        app.diff.raw_line_count = 50;
        app.diff.full_file_cursor = 25;
        // A user who rebinds the commit action onto Ctrl+g — plausible, since 'g' is
        // otherwise unmodified in this pane.
        app.commit_key = KeyBinding::parse("ctrl-g").unwrap();

        app.handle_diff_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE))
            .unwrap();
        assert!(app.diff.pending_g);

        // Ctrl+g triggers the commit action (an early return in handle_diff_key, before
        // the match statement) — without the fix this bypassed the reset entirely and
        // left pending_g armed for whatever unrelated 'g' the user pressed next.
        app.handle_diff_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL))
            .unwrap();
        assert!(!app.diff.pending_g);
        assert_eq!(app.pending_action, Some(ExternalAction::Commit));

        app.pending_action = None;
        app.handle_diff_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.diff.full_file_cursor, 25);
        assert!(app.diff.pending_g);
    }

    #[test]
    fn ctrl_g_commit_binding_clears_pending_g_through_the_real_handle_key_entry_point() {
        // Same scenario as `ctrl_g_commit_binding_does_not_leave_a_stale_pending_g_for_the_next_plain_g`,
        // but driven through `handle_key` — the actual entry point every real keystroke
        // takes, which also runs its own `is_plain_g` reset and the `r`-refresh branch
        // before dispatching to `handle_diff_key`. Confirms nothing on that path re-arms
        // or otherwise disagrees with `handle_diff_key`'s own handling of `Ctrl+g`.
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        set_full_file_mode(&mut app, FullFileSource::Current);
        app.diff.full_file_copyable = true;
        app.diff.raw_line_count = 50;
        app.diff.full_file_cursor = 25;
        app.commit_key = KeyBinding::parse("ctrl-g").unwrap();

        app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE))
            .unwrap();
        assert!(app.diff.pending_g);

        app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL))
            .unwrap();
        assert!(!app.diff.pending_g);
        assert_eq!(app.pending_action, Some(ExternalAction::Commit));

        app.pending_action = None;
        app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.diff.full_file_cursor, 25);
        assert!(app.diff.pending_g);
    }

    #[test]
    fn pending_g_is_cleared_by_global_refresh_even_though_it_bypasses_handle_diff_key() {
        let mut app = make_test_app();
        // `refresh_latest_state()` shells out to `git status`, which needs a real
        // directory as its cwd.
        app.repo_root = std::env::current_dir().unwrap();
        enter_diff_view(&mut app);
        set_full_file_mode(&mut app, FullFileSource::Current);
        app.diff.full_file_copyable = true;
        app.diff.raw_line_count = 50;
        // No current_file/diff_origin, so the 'r' refresh below takes the harmless
        // clear_diff() path instead of attempting a real git reload.

        app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE))
            .unwrap();
        assert!(app.diff.pending_g);

        // `r` (global refresh) is intercepted in handle_key() before the Focus dispatch
        // that would otherwise clear pending_g inside handle_diff_key — it must still
        // clear it, or a later unrelated 'g' would silently complete a stale "gg".
        app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE))
            .unwrap();
        assert!(!app.diff.pending_g);

        // A lone 'g' after the refresh only re-arms; it must not jump immediately.
        app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE))
            .unwrap();
        assert!(app.diff.pending_g);
    }

    #[test]
    fn full_file_select_n_and_shift_n_move_cursor_to_raw_content_matches_only() {
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        set_full_file_mode(&mut app, FullFileSource::Current);
        app.diff.full_file_copyable = true;
        app.diff.raw_diff = "alpha\nneedle\ngamma".to_string();
        app.diff.raw_line_count = 3;
        // The decoration text also contains the query — if search still searched this
        // instead of raw_diff, it would find a second (bogus) match here.
        app.diff.display_diff =
            "header1\nneedle-in-header\nheader3\nalpha\nneedle\ngamma\n".to_string();
        app.diff.display_line_count = 6;
        app.diff.full_file_content_offset = 3;
        app.diff.diff_pane_height = 2;
        app.diff.diff_scroll = 3;
        app.diff.full_file_cursor = 0;
        app.search_state = Some(SearchState {
            scope: SearchScope::DiffView,
            query: "needle".to_string(),
        });

        app.handle_diff_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.diff.full_file_cursor, 1);
        // The match (display row 4) is already inside the current viewport
        // ([3, 5)), so the pane doesn't need to scroll to keep it visible.
        assert_eq!(app.diff.diff_scroll, 3);

        // Only one real match exists (the decoration-text one is excluded), so N wraps
        // back to the same one rather than finding the bogus header match.
        app.handle_diff_key(KeyEvent::new(KeyCode::Char('N'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.diff.full_file_cursor, 1);
        assert_eq!(app.diff.diff_scroll, 3);
    }

    #[test]
    fn full_file_search_position_and_target_use_the_cursor_not_the_scroll() {
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        set_full_file_mode(&mut app, FullFileSource::Current);
        app.diff.full_file_copyable = true;
        app.diff.raw_line_count = 10;

        // Cursor sits mid-viewport, away from the (unmoved) top of the pane.
        app.diff.full_file_cursor = 5;
        app.diff.diff_scroll = 3;
        assert_eq!(
            app.current_search_position(SearchScope::DiffView),
            app.diff.full_file_cursor
        );

        app.apply_search_target(SearchScope::DiffView, 7);
        assert_eq!(app.diff.full_file_cursor, 7);
    }

    #[test]
    fn toggle_full_file_view_preserves_the_patch_cursors_on_screen_row() {
        let mut app = make_test_app();
        app.tool = DiffTool::Raw;
        app.diff.diff_pane_height = 20;

        let raw = "diff --git a/file.txt b/file.txt\nindex 111..222 100644\n--- a/file.txt\n+++ b/file.txt\n@@ -10,3 +12,3 @@\n context1\n-removed1\n+added1\n context2\n";
        app.diff.file_diff = parse_diff(raw);
        app.diff.line_infos = App::build_patch_line_infos(raw);
        set_patch_mode(&mut app);
        app.diff.current_file = Some("file.txt".to_string());
        app.diff.diff_origin = Some(TreePane::Unstaged);
        // Viewport scrolled to the top, cursor sitting 8 rows down the screen at
        // " context2" (file line 14).
        app.diff.diff_scroll = 0;
        app.diff.patch_cursor = 8;

        let mut full_cached = make_cached_diff_with_lines(500);
        full_cached.full_file_content_offset = 3;
        full_cached.full_file_copyable = true;
        let key = app.build_diff_cache_key(
            "file.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Current),
        );
        app.insert_cached_diff(key, full_cached);

        app.toggle_full_file_view(FullFileSource::Current).unwrap();

        assert_eq!(
            app.diff.diff_content,
            DiffContent::FullFile(FullFileSource::Current)
        );
        assert_eq!(app.diff.full_file_cursor, 13);
        // display_row = content_offset(3) + 13 = 16; the viewport is positioned so the
        // cursor lands on the exact same screen row (8) it had in patch view — not pinned
        // to the pane's top, and not pushed to its bottom either.
        assert_eq!(app.diff.diff_scroll, 8);
        let cursor_screen_row =
            app.diff.full_file_content_offset + app.diff.full_file_cursor - app.diff.diff_scroll;
        assert_eq!(cursor_screen_row, 8);
    }

    #[test]
    fn toggle_full_file_view_clamps_the_preserved_row_when_the_target_is_near_the_files_start() {
        let mut app = make_test_app();
        app.tool = DiffTool::Raw;
        app.diff.diff_pane_height = 20;

        let raw = "diff --git a/file.txt b/file.txt\nindex 111..222 100644\n--- a/file.txt\n+++ b/file.txt\n@@ -1,3 +1,3 @@\n context1\n-removed1\n+added1\n context2\n";
        app.diff.file_diff = parse_diff(raw);
        app.diff.line_infos = App::build_patch_line_infos(raw);
        set_patch_mode(&mut app);
        app.diff.current_file = Some("file.txt".to_string());
        app.diff.diff_origin = Some(TreePane::Unstaged);
        // Cursor sits 8 rows down the patch pane, but the mapped target (file line 3) is
        // near the very start of the file — there isn't enough content above it to push
        // the viewport down far enough to reproduce that same screen row.
        app.diff.diff_scroll = 0;
        app.diff.patch_cursor = 8;

        let mut full_cached = make_cached_diff_with_lines(500);
        full_cached.full_file_content_offset = 3;
        full_cached.full_file_copyable = true;
        let key = app.build_diff_cache_key(
            "file.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Current),
        );
        app.insert_cached_diff(key, full_cached);

        app.toggle_full_file_view(FullFileSource::Current).unwrap();

        assert_eq!(app.diff.full_file_cursor, 2);
        // Best effort: clamped to 0 instead of going negative. The cursor ends up higher on
        // screen than its ideal preserved row (8) — unavoidable this close to the top.
        assert_eq!(app.diff.diff_scroll, 0);
    }

    #[test]
    fn toggle_full_file_view_opens_at_top_when_no_mapping_available() {
        let mut app = make_test_app();
        app.tool = DiffTool::Difftastic;
        set_patch_mode(&mut app);
        app.diff.current_file = Some("file.txt".to_string());
        app.diff.diff_origin = Some(TreePane::Unstaged);
        app.diff.diff_scroll = 42;

        seed_cached_view(
            &mut app,
            "file.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Current),
            500,
        );

        app.toggle_full_file_view(FullFileSource::Current).unwrap();

        assert_eq!(app.diff.diff_scroll, 0);
        assert_eq!(app.diff.full_file_cursor, 0);
    }

    #[test]
    fn toggle_full_file_view_carries_the_patch_cursor_row_for_an_untracked_file() {
        let mut app = make_test_app();
        // An untracked file has no hunks for `patch_top_line_target` to map through — its
        // patch view is `get_file_preview`'s rendering of the file's own content instead,
        // so `untracked_patch_line_target` must be the one filling in `target_line`.
        build_section(
            &mut app.tree.unstaged.all_nodes,
            &[("file.txt".to_string(), '?', '?')],
        );

        let patch_key =
            app.build_diff_cache_key("file.txt", TreePane::Unstaged, DiffContent::Patch);
        let mut patch_cached = make_cached_diff_with_lines(30);
        // Simulates the 3 leading decoration rows `get_file_preview`'s bat rendering added
        // ahead of the file's own content (separator, `File:` banner, separator).
        patch_cached.full_file_content_offset = 3;
        app.insert_cached_diff(patch_key, patch_cached);
        seed_cached_view(
            &mut app,
            "file.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Current),
            30,
        );

        app.load_diff("file.txt", TreePane::Unstaged, DiffContent::Patch)
            .unwrap();
        // Display row 12 in the patch pane: 3 header rows + file line 10 (0-indexed 9).
        app.diff.patch_cursor = 12;

        app.toggle_full_file_view(FullFileSource::Current).unwrap();

        assert_eq!(
            app.diff.diff_content,
            DiffContent::FullFile(FullFileSource::Current)
        );
        assert_eq!(app.diff.full_file_cursor, 9);
    }

    #[test]
    fn clear_diff_remembers_the_departing_files_own_patch_cursor_before_wiping_it() {
        let mut app = make_test_app();
        seed_cached_view(
            &mut app,
            "file-a.txt",
            TreePane::Unstaged,
            DiffContent::Patch,
            120,
        );
        seed_cached_view(
            &mut app,
            "file-b.txt",
            TreePane::Unstaged,
            DiffContent::Patch,
            120,
        );

        app.load_diff("file-a.txt", TreePane::Unstaged, DiffContent::Patch)
            .unwrap();
        app.diff.diff_scroll = 20;
        app.diff.patch_cursor = 25;

        // `clear_diff` runs while `current_file`/`patch_cursor` still identify file-a — it
        // must snapshot *that* file's own position, not leave a stale value that could bleed
        // into whichever file gets remembered next.
        app.clear_diff();
        assert_eq!(app.diff.current_file, None);

        app.load_diff("file-b.txt", TreePane::Unstaged, DiffContent::Patch)
            .unwrap();
        // file-b was never visited before: its own remembered position is (0, 0), untouched
        // by file-a's leftover scroll/cursor.
        assert_eq!(app.diff.diff_scroll, 0);
        assert_eq!(app.diff.patch_cursor, 0);

        app.load_diff("file-a.txt", TreePane::Unstaged, DiffContent::Patch)
            .unwrap();
        // Reselecting file-a restores exactly what was remembered for it at `clear_diff` time.
        assert_eq!(app.diff.diff_scroll, 20);
        assert_eq!(app.diff.patch_cursor, 25);
    }

    #[test]
    fn toggle_full_file_view_preserves_scroll_between_current_and_previous() {
        let mut app = make_test_app();
        set_full_file_mode(&mut app, FullFileSource::Current);
        app.diff.current_file = Some("file.txt".to_string());
        app.diff.diff_origin = Some(TreePane::Unstaged);
        app.diff.diff_scroll = 40;
        app.diff.full_file_cursor = 12;

        seed_cached_view(
            &mut app,
            "file.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Previous),
            500,
        );

        app.toggle_full_file_view(FullFileSource::Previous).unwrap();

        assert_eq!(
            app.diff.diff_content,
            DiffContent::FullFile(FullFileSource::Previous)
        );
        assert_eq!(app.diff.diff_scroll, 40);
        assert_eq!(app.diff.full_file_cursor, 12);

        seed_cached_view(
            &mut app,
            "file.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Current),
            500,
        );

        app.toggle_full_file_view(FullFileSource::Current).unwrap();

        assert_eq!(
            app.diff.diff_content,
            DiffContent::FullFile(FullFileSource::Current)
        );
        assert_eq!(app.diff.diff_scroll, 40);
        assert_eq!(app.diff.full_file_cursor, 12);
    }

    #[test]
    fn toggle_full_file_view_reverse_direction_restores_patch_scroll_and_cursor_independently() {
        let mut app = make_test_app();
        seed_cached_view(
            &mut app,
            "file.txt",
            TreePane::Unstaged,
            DiffContent::Patch,
            120,
        );
        seed_cached_view(
            &mut app,
            "file.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Current),
            120,
        );

        app.load_diff("file.txt", TreePane::Unstaged, DiffContent::Patch)
            .unwrap();
        // The scenario from the bug report: viewport top still shows row 1 (scroll
        // untouched), but the cursor itself has moved further down to row 10 — the two
        // must be remembered independently, not collapsed into one value.
        app.diff.diff_scroll = 0;
        app.diff.patch_cursor = 9;

        app.toggle_full_file_view(FullFileSource::Current).unwrap();
        assert_eq!(
            app.diff.diff_content,
            DiffContent::FullFile(FullFileSource::Current)
        );
        // Move around within full-file view (e.g. down to row 15) — this must never leak
        // back into patch view's own remembered position.
        app.diff.full_file_cursor = 14;
        app.diff.diff_scroll = 14;

        // Pressing 'f' again (same source) toggles back to patch view.
        app.toggle_full_file_view(FullFileSource::Current).unwrap();

        assert_eq!(app.diff.diff_content, DiffContent::Patch);
        // Restored from patch view's own remembered scroll AND cursor — not reverse-mapped
        // from wherever the full-file cursor ended up, and not collapsed to a single value.
        assert_eq!(app.diff.diff_scroll, 0);
        assert_eq!(app.diff.patch_cursor, 9);
    }

    #[test]
    fn toggle_full_file_view_clamps_preserved_scroll_to_shorter_side() {
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        set_full_file_mode(&mut app, FullFileSource::Current);
        app.diff.current_file = Some("file.txt".to_string());
        app.diff.diff_origin = Some(TreePane::Unstaged);
        app.diff.diff_scroll = 40;
        app.diff.full_file_cursor = 40;

        // Previous side is shorter but still available/copyable (unlike the
        // review_11 Finding 2 unavailable-placeholder case below) — this is the "genuinely
        // shorter real file" clamp path, distinct from `full_file_cursor_active() == false`.
        let mut shorter_cached = make_cached_diff_with_lines(10);
        shorter_cached.full_file_copyable = true;
        let key = app.build_diff_cache_key(
            "file.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Previous),
        );
        app.insert_cached_diff(key, shorter_cached);

        app.toggle_full_file_view(FullFileSource::Previous).unwrap();

        assert_eq!(app.diff.diff_scroll, 9);
        assert_eq!(app.diff.full_file_cursor, 9);
    }

    #[test]
    fn toggle_full_file_view_preserves_cursor_and_scroll_unclamped_across_an_unavailable_side() {
        // review_11 Finding 2: switching to an unavailable placeholder side (binary/
        // unmerged/missing/empty) used to clamp the outgoing (cursor, scroll) down to the
        // placeholder's own tiny line count (effectively (0, 0)) before that state was
        // ever restored — so switching back discarded the original deep position instead
        // of restoring it. The outgoing position must survive the round trip unclamped.
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        set_full_file_mode(&mut app, FullFileSource::Current);
        app.diff.current_file = Some("file.txt".to_string());
        app.diff.diff_origin = Some(TreePane::Unstaged);
        app.diff.diff_scroll = 91;
        app.diff.full_file_cursor = 100;

        // Previous side is unavailable (not copyable) — `seed_cached_view`'s default.
        seed_cached_view(
            &mut app,
            "file.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Previous),
            0,
        );

        app.toggle_full_file_view(FullFileSource::Previous).unwrap();

        assert!(!app.diff.full_file_copyable);
        // False because the placeholder isn't copyable, not because of focus — focus is
        // `DiffView` throughout this test, isolating the assertion to the condition this
        // test actually exercises.
        assert!(!app.full_file_cursor_active());

        // Current side becomes available again.
        let mut current_cached = make_cached_diff_with_lines(500);
        current_cached.full_file_copyable = true;
        let key = app.build_diff_cache_key(
            "file.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Current),
        );
        app.insert_cached_diff(key, current_cached);

        app.toggle_full_file_view(FullFileSource::Current).unwrap();

        assert_eq!(app.diff.diff_scroll, 91);
        assert_eq!(app.diff.full_file_cursor, 100);
    }

    #[test]
    fn toggle_full_file_view_keeps_cursor_visible_when_switching_into_a_shorter_file() {
        let mut app = make_test_app();
        enter_diff_view(&mut app);
        set_full_file_mode(&mut app, FullFileSource::Current);
        app.diff.current_file = Some("file.txt".to_string());
        app.diff.diff_origin = Some(TreePane::Unstaged);
        app.diff.diff_pane_height = 10;
        app.diff.full_file_content_offset = 3;
        app.diff.full_file_copyable = true;
        app.diff.raw_line_count = 100;
        // Cursor pinned near the bottom of a long file (as `G` would leave it).
        app.diff.diff_scroll = 93;
        app.diff.full_file_cursor = 99;

        let key = app.build_diff_cache_key(
            "file.txt",
            TreePane::Unstaged,
            DiffContent::FullFile(FullFileSource::Previous),
        );
        app.insert_cached_diff(
            key,
            CachedDiff {
                raw_diff: (0..10)
                    .map(|idx| format!("line {}", idx))
                    .collect::<Vec<_>>()
                    .join("\n"),
                display_diff: String::new(),
                file_diff: FileDiff::default(),
                line_infos: Vec::new(),
                display_line_count: 14, // 3-row header + 10 content rows + 1 footer row
                raw_line_count: 10,
                cached_display_text: None,
                content_annotation: None,
                full_file_copyable: true,
                full_file_content_offset: 3,
                full_file_highlight_lines: Vec::new(),
            },
        );

        app.toggle_full_file_view(FullFileSource::Previous).unwrap();

        assert_eq!(
            app.diff.diff_content,
            DiffContent::FullFile(FullFileSource::Previous)
        );
        // Clamped to the shorter file's last line.
        assert_eq!(app.diff.full_file_cursor, 9);
        // The cursor's display row must stay inside the viewport `diff_scroll` defines,
        // not end up above it (as it would without reconciling scroll to the cursor).
        let cursor_display_row = app.diff.full_file_content_offset + app.diff.full_file_cursor;
        assert!(app.diff.diff_scroll <= cursor_display_row);
        assert!(cursor_display_row < app.diff.diff_scroll + app.diff.diff_pane_height);
    }
}
