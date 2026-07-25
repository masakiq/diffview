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
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::clipboard;
use crate::config::Config;
use crate::git::diff::{parse_diff, DiffLine, FileDiff};
use crate::git::status::{get_commit_files, get_status};

// ─── Focus ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum Focus {
    Unstaged,
    Staged,
    DiffView,
    InlineSelect,
}

// ─── TreePane ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TreePane {
    Unstaged,
    Staged,
}

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
pub enum DiffViewMode {
    Patch,
    FullFile(FullFileSource),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FullFileSource {
    Current,
    Previous,
}

impl FullFileSource {
    fn title_label(self) -> &'static str {
        match self {
            FullFileSource::Current => "file",
            FullFileSource::Previous => "file:previous",
        }
    }

    fn status_message(self) -> &'static str {
        match self {
            FullFileSource::Current => "Full file view",
            FullFileSource::Previous => "Previous full file view",
        }
    }

    fn missing_message(self) -> &'static str {
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

impl DiffViewMode {
    pub fn label(self) -> &'static str {
        match self {
            DiffViewMode::Patch => "patch",
            DiffViewMode::FullFile(source) => source.title_label(),
        }
    }

    fn is_full_file(self) -> bool {
        matches!(self, DiffViewMode::FullFile(_))
    }

    fn toggle_full_file(self, source: FullFileSource) -> Self {
        match self {
            DiffViewMode::FullFile(current) if current == source => DiffViewMode::Patch,
            _ => DiffViewMode::FullFile(source),
        }
    }
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
    fn expand_and_enter(&mut self) {
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
    fn fold_parent(&mut self) {
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
    fn files_under_dir(&self, dir_path: &Path) -> Vec<String> {
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

// ─── Line mapping for inline-select ─────────────────────────────────────

#[derive(Debug, Clone)]
pub struct DisplayLineInfo {
    pub hunk_idx: Option<usize>,
    pub line_in_hunk: Option<usize>,
    pub is_selectable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileSelectionState {
    status: char,
    is_unmerged: bool,
    is_untracked: bool,
}

#[derive(Debug, Clone)]
struct PendingTreePreview {
    pane: TreePane,
    ready_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExternalAction {
    Commit,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct DiffCacheKey {
    path: String,
    pane: TreePane,
    tool: DiffTool,
    pane_width: u16,
    commit_revision: Option<String>,
    view_mode: DiffViewMode,
    full_file_show_line_numbers: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct DiffScrollKey {
    path: String,
    pane: TreePane,
}

#[derive(Clone)]
struct CachedDiff {
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FullFileContentTarget {
    Worktree,
    Revision {
        rev_spec: String,
        content_annotation: Option<ContentAnnotation>,
    },
}

const DIFF_CACHE_CAPACITY: usize = 64;
const TREE_PREVIEW_DEBOUNCE_MS: u64 = 100;
const TREE_FAST_MOVE_LINES: usize = 5;
const NULL_COMMIT_OID: &str = "0000000000000000000000000000000000000000";

fn normalize_revision_override(revision_override: Option<String>) -> Option<String> {
    revision_override.filter(|revision| revision != NULL_COMMIT_OID)
}

// ─── App ───────────────────────────────────────────────────────────────────

pub struct App {
    pub should_quit: bool,
    pub focus: Focus,
    #[allow(dead_code)]
    pub config: Config,
    pub tool: DiffTool,
    pub repo_root: PathBuf,
    pub commit_revision: Option<String>,
    tree_pane_percentage: u16,
    commit_key: KeyBinding,
    commit_command: Vec<String>,
    pending_action: Option<ExternalAction>,

    // Tree sections
    pub unstaged: TreeSection,
    pub staged: TreeSection,

    // Diff state
    pub diff_origin: Option<TreePane>,
    pub diff_view_mode: DiffViewMode,
    pub display_diff: String,
    pub raw_diff: String,
    pub file_diff: FileDiff,
    pub diff_scroll: usize,
    pub diff_cursor: usize,
    pub hunk_cursor: usize,
    pub current_file: Option<String>,
    pub line_infos: Vec<DisplayLineInfo>,
    pub display_line_count: usize,
    pub raw_line_count: usize,
    pub cached_display_text: Option<Text<'static>>,
    pub content_annotation: Option<ContentAnnotation>,
    pub full_file_copyable: bool,
    pub full_file_content_offset: usize,
    pub full_file_show_line_numbers: bool,
    pub diff_pane_height: usize,
    pub diff_pane_width: u16,
    pending_tree_preview: Option<PendingTreePreview>,
    tree_preview_debounce: Duration,
    diff_cache: HashMap<DiffCacheKey, CachedDiff>,
    diff_cache_order: VecDeque<DiffCacheKey>,
    diff_cache_capacity: usize,
    diff_scroll_positions: HashMap<DiffScrollKey, usize>,
    search_state: Option<SearchState>,
    search_input: Option<SearchInput>,

    // Status bar
    pub status_message: Option<String>,
    pub error_message: Option<String>,
}

impl App {
    pub fn new(tool_override: Option<String>, revision_override: Option<String>) -> Result<Self> {
        let repo_root = crate::git::get_repo_root()?;
        let commit_revision = match normalize_revision_override(revision_override) {
            Some(rev) => Some(crate::git::resolve_commit(&rev, &repo_root)?),
            None => None,
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
            commit_revision,
            tree_pane_percentage,
            commit_key,
            commit_command,
            pending_action: None,
            unstaged: TreeSection::new(),
            staged: TreeSection::new(),
            diff_origin: None,
            diff_view_mode: DiffViewMode::Patch,
            display_diff: String::new(),
            raw_diff: String::new(),
            file_diff: FileDiff::default(),
            diff_scroll: 0,
            diff_cursor: 0,
            hunk_cursor: 0,
            current_file: None,
            line_infos: Vec::new(),
            display_line_count: 0,
            raw_line_count: 0,
            cached_display_text: None,
            content_annotation: None,
            full_file_copyable: false,
            full_file_content_offset: 0,
            full_file_show_line_numbers: true,
            diff_pane_height: 20,
            diff_pane_width: 0,
            pending_tree_preview: None,
            tree_preview_debounce: Duration::from_millis(TREE_PREVIEW_DEBOUNCE_MS),
            diff_cache: HashMap::new(),
            diff_cache_order: VecDeque::new(),
            diff_cache_capacity: DIFF_CACHE_CAPACITY,
            diff_scroll_positions: HashMap::new(),
            search_state: None,
            search_input: None,
            status_message: None,
            error_message: None,
        };

        let width = crossterm::terminal::size().map(|(w, _)| w).unwrap_or(120);
        app.diff_pane_width = app.compute_diff_pane_width(width);

        app.refresh_trees()?;

        // Auto-focus: if unstaged is empty but staged has items, start in staged
        if !app.is_commit_mode() && app.unstaged.is_empty() && !app.staged.is_empty() {
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
                let _ = self.load_diff(&path, pane, DiffViewMode::Patch);
            }
        }
    }

    pub fn is_commit_mode(&self) -> bool {
        self.commit_revision.is_some()
    }

    pub fn commit_label(&self) -> Option<String> {
        self.commit_revision
            .as_ref()
            .map(|rev| format!("commit {}", rev.chars().take(8).collect::<String>()))
    }

    pub fn tree_title(&self, pane: TreePane) -> &'static str {
        if self.is_commit_mode() {
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

        match (current_scope, search.scope, self.is_commit_mode(), pane) {
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

    pub fn tree_help_text(&self) -> String {
        if self.is_commit_mode() {
            "[l/Enter]open [h]back [c]copy [/]search [n/N]match [Ctrl-U/D]5-lines [j/k]move [r]refresh [?]help [q]quit".to_string()
        } else {
            format!(
                "[l]open [h]back [u]stage/unstage [c]copy [/]search [n/N]match [Ctrl-U/D]5-lines [j/k]move [r]refresh [{}]commit [?]help [q]quit",
                self.commit_key_label()
            )
        }
    }

    pub fn diff_help_text(&self) -> String {
        let mut ops =
            "[j/k]scroll [Ctrl-U/D]jump [h]back [c]copy-path [/]search [n/N]match [r]refresh [q]quit"
                .to_string();

        if self.diff_view_mode == DiffViewMode::Patch {
            ops.push_str(" [[]/[]]hunk");
        }
        if self.diff_view_mode.is_full_file() {
            ops.push_str(" [P]copy-file [n]line-numbers");
        }

        if !self.is_commit_mode() {
            ops.push_str(&format!(" [{}]commit", self.commit_key_label()));
        }
        if self.diff_view_mode == DiffViewMode::Patch
            && !self.is_commit_mode()
            && self.tool.supports_line_ops()
        {
            ops.push_str(" [v]select");
        }
        ops.push_str(match self.diff_view_mode {
            DiffViewMode::Patch => " [f]file [F]prev-file",
            DiffViewMode::FullFile(FullFileSource::Current) => " [f]diff [F]prev-file",
            DiffViewMode::FullFile(FullFileSource::Previous) => " [f]file [F]diff",
        });

        ops
    }

    pub fn inline_select_help_text(&self) -> String {
        "[j/k]move [Ctrl-U/D]jump [u]apply [v]back [h]tree [/]search [n/N]match [[]/[]]hunk [r]refresh [q]quit".to_string()
    }

    fn can_trigger_commit_action(&self, key: KeyEvent) -> bool {
        !self.is_commit_mode()
            && self.commit_key.matches(key)
            && matches!(
                self.focus,
                Focus::Unstaged | Focus::Staged | Focus::DiffView
            )
    }

    // ─── Tree access ────────────────────────────────────────────────────

    pub fn tree(&self, pane: TreePane) -> &TreeSection {
        match pane {
            TreePane::Unstaged => &self.unstaged,
            TreePane::Staged => &self.staged,
        }
    }

    pub fn is_tree_focused(&self, pane: TreePane) -> bool {
        if self.is_commit_mode() && pane == TreePane::Staged {
            return false;
        }
        match pane {
            TreePane::Unstaged => self.focus == Focus::Unstaged,
            TreePane::Staged => self.focus == Focus::Staged,
        }
    }

    fn tree_mut(&mut self, pane: TreePane) -> &mut TreeSection {
        match pane {
            TreePane::Unstaged => &mut self.unstaged,
            TreePane::Staged => &mut self.staged,
        }
    }

    fn focused_pane(&self) -> Option<TreePane> {
        if self.is_commit_mode() {
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
            Focus::Unstaged => Some(if self.is_commit_mode() {
                SearchScope::CommitTree
            } else {
                SearchScope::WorkingTree
            }),
            Focus::Staged if !self.is_commit_mode() => Some(SearchScope::WorkingTree),
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
        if let Some(rev) = self.commit_revision.as_deref() {
            let files = get_commit_files(rev, &self.repo_root)?;
            let commit_files: Vec<(String, char, char)> = files
                .into_iter()
                .map(|f| (f.path, f.staged, f.unstaged))
                .collect();

            build_section(&mut self.unstaged.all_nodes, &commit_files);
            rebuild_section_visible(&mut self.unstaged);

            self.staged.all_nodes.clear();
            self.staged.visible.clear();
            self.staged.cursor = 0;
            if self.focus == Focus::Staged {
                self.focus = Focus::Unstaged;
            }
            return Ok(());
        }

        let files = get_status(&self.repo_root)?;

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

        build_section(&mut self.unstaged.all_nodes, &unstaged_files);
        rebuild_section_visible(&mut self.unstaged);

        build_section(&mut self.staged.all_nodes, &staged_files);
        rebuild_section_visible(&mut self.staged);

        Ok(())
    }

    // ─── Diff loading ────────────────────────────────────────────────────

    fn build_diff_cache_key(
        &self,
        path: &str,
        pane: TreePane,
        view_mode: DiffViewMode,
    ) -> DiffCacheKey {
        DiffCacheKey {
            path: path.to_string(),
            pane,
            tool: self.tool.clone(),
            pane_width: self.diff_pane_width,
            commit_revision: self.commit_revision.clone(),
            view_mode,
            full_file_show_line_numbers: self.full_file_show_line_numbers,
        }
    }

    fn build_diff_scroll_key(&self, path: &str, pane: TreePane) -> DiffScrollKey {
        DiffScrollKey {
            path: path.to_string(),
            pane,
        }
    }

    fn saved_diff_scroll(&self, path: &str, pane: TreePane) -> usize {
        self.diff_scroll_positions
            .get(&self.build_diff_scroll_key(path, pane))
            .copied()
            .unwrap_or(0)
    }

    fn remember_diff_scroll(&mut self, path: &str, pane: TreePane, scroll: usize) {
        self.diff_scroll_positions
            .insert(self.build_diff_scroll_key(path, pane), scroll);
    }

    /// Full file view never remembers its scroll position: it always opens at the top.
    fn remember_current_diff_scroll(&mut self) {
        if self.diff_view_mode.is_full_file() {
            return;
        }
        if let (Some(path), Some(pane)) = (self.current_file.clone(), self.diff_origin) {
            self.remember_diff_scroll(&path, pane, self.diff_scroll);
        }
    }

    fn restore_saved_diff_scroll(&mut self, path: &str, pane: TreePane, view_mode: DiffViewMode) {
        if view_mode.is_full_file() {
            self.diff_scroll = 0;
            return;
        }
        let saved = self.saved_diff_scroll(path, pane);
        self.diff_scroll = saved.min(self.display_line_count.saturating_sub(1));
    }

    fn touch_diff_cache_key(&mut self, key: DiffCacheKey) {
        if let Some(pos) = self
            .diff_cache_order
            .iter()
            .position(|existing| *existing == key)
        {
            self.diff_cache_order.remove(pos);
        }
        self.diff_cache_order.push_back(key);
    }

    fn get_cached_diff(&mut self, key: &DiffCacheKey) -> Option<CachedDiff> {
        let cached = self.diff_cache.get(key).cloned();
        if cached.is_some() {
            self.touch_diff_cache_key(key.clone());
        }
        cached
    }

    fn insert_cached_diff(&mut self, key: DiffCacheKey, value: CachedDiff) {
        if self.diff_cache.contains_key(&key) {
            self.diff_cache.insert(key.clone(), value);
            self.touch_diff_cache_key(key);
            return;
        }

        if self.diff_cache.len() >= self.diff_cache_capacity {
            if let Some(oldest) = self.diff_cache_order.pop_front() {
                self.diff_cache.remove(&oldest);
            }
        }

        self.diff_cache.insert(key.clone(), value);
        self.diff_cache_order.push_back(key);
    }

    fn clear_diff_cache(&mut self) {
        self.diff_cache.clear();
        self.diff_cache_order.clear();
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
        view_mode: DiffViewMode,
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
    ) {
        self.raw_diff = raw_diff;
        self.display_diff = display_diff;
        self.file_diff = file_diff;
        self.line_infos = line_infos;
        self.display_line_count = display_line_count;
        self.raw_line_count = raw_line_count;
        self.cached_display_text = cached_display_text;
        self.diff_view_mode = view_mode;
        self.content_annotation = content_annotation;
        self.full_file_copyable = full_file_copyable;
        self.full_file_content_offset = full_file_content_offset;
        self.current_file = Some(path.to_string());
        self.diff_origin = Some(pane);
        self.diff_scroll = 0;
        self.diff_cursor = 0;
        self.hunk_cursor = 0;
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
        }
    }

    fn file_selection_state(&self, path: &str, pane: TreePane) -> Option<FileSelectionState> {
        self.tree(pane)
            .all_nodes
            .iter()
            .find(|node| !node.is_dir && node.path == Path::new(path))
            .map(|node| FileSelectionState {
                status: node.status_for(pane),
                is_unmerged: !self.is_commit_mode() && node.is_unmerged(),
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
        )
    }

    fn plain_full_file_content(
        &self,
        raw: String,
        content_annotation: Option<ContentAnnotation>,
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
        )
    }

    fn rich_full_file_content(
        &self,
        path: &str,
        raw: String,
        content_annotation: Option<ContentAnnotation>,
    ) -> LoadedContent {
        match crate::git::diff::render_content_preview(
            path,
            &raw,
            &self.repo_root,
            self.full_file_show_line_numbers,
        ) {
            Ok(preview) => self.build_loaded_content(
                raw,
                preview.content,
                FileDiff::default(),
                true,
                preview.uses_ansi,
                content_annotation,
                true,
                preview.content_offset,
            ),
            Err(_) => self.plain_full_file_content(raw, content_annotation),
        }
    }

    fn patch_file_diff_for(&mut self, path: &str, pane: TreePane) -> FileDiff {
        let patch_key = self.build_diff_cache_key(path, pane, DiffViewMode::Patch);
        if let Some(cached) = self.get_cached_diff(&patch_key) {
            return cached.file_diff;
        }

        let raw = if let Some(rev) = self.commit_revision.as_deref() {
            crate::git::diff::get_raw_commit_diff(rev, path, &self.repo_root).unwrap_or_default()
        } else {
            crate::git::diff::get_raw_diff(path, pane.is_staged(), &self.repo_root)
                .unwrap_or_default()
        };

        parse_diff(&raw)
    }

    fn full_file_is_binary(
        &mut self,
        path: &str,
        pane: TreePane,
        file_state: FileSelectionState,
    ) -> bool {
        if file_state.is_untracked {
            return crate::git::diff::is_binary_untracked_file(path, &self.repo_root)
                .unwrap_or(false);
        }

        self.patch_file_diff_for(path, pane).is_binary
    }

    fn full_file_missing_message(
        &self,
        file_state: FileSelectionState,
        source: FullFileSource,
    ) -> Option<&'static str> {
        match (source, file_state.status) {
            (FullFileSource::Current, 'D') => Some(FullFileSource::Current.missing_message()),
            (FullFileSource::Previous, 'A' | '?') => {
                Some(FullFileSource::Previous.missing_message())
            }
            _ => None,
        }
    }

    fn resolve_full_file_content_target(
        &self,
        path: &str,
        pane: TreePane,
        file_state: FileSelectionState,
        source: FullFileSource,
    ) -> Result<FullFileContentTarget, &'static str> {
        if let Some(message) = self.full_file_missing_message(file_state, source) {
            return Err(message);
        }

        let content_annotation =
            matches!((source, file_state.status), (FullFileSource::Previous, 'D'))
                .then_some(ContentAnnotation::BeforeDelete);

        if let Some(rev) = self.commit_revision.as_deref() {
            let rev_spec = match source {
                FullFileSource::Current => format!("{}:{}", rev, path),
                FullFileSource::Previous => format!("{}^:{}", rev, path),
            };
            return Ok(FullFileContentTarget::Revision {
                rev_spec,
                content_annotation,
            });
        }

        match (pane, source) {
            (TreePane::Unstaged, FullFileSource::Current) => Ok(FullFileContentTarget::Worktree),
            (TreePane::Unstaged, FullFileSource::Previous) => Ok(FullFileContentTarget::Revision {
                rev_spec: format!(":{}", path),
                content_annotation,
            }),
            (TreePane::Staged, FullFileSource::Current) => Ok(FullFileContentTarget::Revision {
                rev_spec: format!(":{}", path),
                content_annotation: None,
            }),
            (TreePane::Staged, FullFileSource::Previous) => Ok(FullFileContentTarget::Revision {
                rev_spec: format!("HEAD:{}", path),
                content_annotation,
            }),
        }
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

        let target = match self.resolve_full_file_content_target(path, pane, file_state, source) {
            Ok(target) => target,
            Err(message) => return self.full_file_unavailable_content(message, None),
        };

        if self.full_file_is_binary(path, pane, file_state) {
            return self.full_file_unavailable_content(
                "Full file view unavailable for binary files",
                Some(ContentAnnotation::BinaryUnavailable),
            );
        }

        match target {
            FullFileContentTarget::Worktree => {
                match crate::git::diff::get_file_content(path, &self.repo_root) {
                    Ok(raw) => self.rich_full_file_content(path, raw, None),
                    Err(_) => self.full_file_unavailable_content("File content unavailable", None),
                }
            }
            FullFileContentTarget::Revision {
                rev_spec,
                content_annotation,
            } => match crate::git::diff::get_file_content_at_rev(&rev_spec, &self.repo_root) {
                Ok(raw) => self.rich_full_file_content(path, raw, content_annotation),
                Err(_) => self.full_file_unavailable_content(source.missing_message(), None),
            },
        }
    }

    pub fn load_diff(&mut self, path: &str, pane: TreePane, view_mode: DiffViewMode) -> Result<()> {
        self.remember_current_diff_scroll();

        let cache_key = self.build_diff_cache_key(path, pane, view_mode);
        if let Some(cached) = self.get_cached_diff(&cache_key) {
            self.apply_loaded_diff_state(
                path,
                pane,
                view_mode,
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
            );
            self.restore_saved_diff_scroll(path, pane, view_mode);
            return Ok(());
        }

        let loaded = match view_mode {
            DiffViewMode::Patch => {
                let is_untracked = self.has_untracked_file_in_pane(pane, path);
                let mut force_ansi_rendering = false;
                let (raw, display) = if is_untracked {
                    let preview = crate::git::diff::get_file_preview(path, &self.repo_root)
                        .unwrap_or_else(|_| crate::git::diff::FilePreview {
                            content: String::new(),
                            uses_ansi: false,
                            content_offset: 0,
                        });
                    force_ansi_rendering = preview.uses_ansi;
                    (preview.content.clone(), preview.content)
                } else if let Some(rev) = self.commit_revision.as_deref() {
                    let raw = crate::git::diff::get_raw_commit_diff(rev, path, &self.repo_root)
                        .unwrap_or_default();
                    let display = if self.tool == DiffTool::Raw {
                        raw.clone()
                    } else {
                        crate::git::diff::get_display_commit_diff(
                            rev,
                            path,
                            self.tool.name(),
                            self.diff_pane_width,
                            &self.repo_root,
                        )
                        .unwrap_or_else(|_| raw.clone())
                    };
                    (raw, display)
                } else {
                    let raw =
                        crate::git::diff::get_raw_diff(path, pane.is_staged(), &self.repo_root)
                            .unwrap_or_default();
                    let display = if self.tool == DiffTool::Raw {
                        raw.clone()
                    } else {
                        crate::git::diff::get_display_diff(
                            path,
                            pane.is_staged(),
                            self.tool.name(),
                            self.diff_pane_width,
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
                    0,
                )
            }
            DiffViewMode::FullFile(source) => self.load_full_file_content(path, pane, source),
        };

        self.apply_loaded_diff_state(
            path,
            pane,
            view_mode,
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
        );
        self.restore_saved_diff_scroll(path, pane, view_mode);

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
            },
        );

        Ok(())
    }

    fn clear_diff(&mut self) {
        self.remember_current_diff_scroll();
        self.diff_view_mode = DiffViewMode::Patch;
        self.display_diff.clear();
        self.raw_diff.clear();
        self.file_diff = FileDiff::default();
        self.current_file = None;
        self.diff_origin = None;
        self.diff_scroll = 0;
        self.diff_cursor = 0;
        self.hunk_cursor = 0;
        self.line_infos.clear();
        self.raw_line_count = 0;
        self.display_line_count = 0;
        self.cached_display_text = None;
        self.content_annotation = None;
        self.full_file_copyable = false;
        self.full_file_content_offset = 0;
    }

    /// Reload diff for the current file with the current origin
    fn reload_current_diff(&mut self) -> Result<()> {
        if let (Some(path), Some(pane)) = (self.current_file.clone(), self.diff_origin) {
            let prev_scroll = self.diff_scroll;
            let prev_cursor = self.diff_cursor;
            let view_mode = if self.focus == Focus::InlineSelect {
                DiffViewMode::Patch
            } else {
                self.diff_view_mode
            };
            self.load_diff(&path, pane, view_mode)?;
            let scroll_line_count = if self.focus == Focus::InlineSelect {
                self.raw_line_count
            } else {
                self.display_line_count
            };
            self.diff_scroll = prev_scroll.min(scroll_line_count.saturating_sub(1));
            self.diff_cursor = prev_cursor.min(self.raw_line_count.saturating_sub(1));
        }
        Ok(())
    }

    fn has_untracked_file_in_pane(&self, pane: TreePane, path: &str) -> bool {
        if self.is_commit_mode() {
            return false;
        }
        self.tree(pane)
            .all_nodes
            .iter()
            .any(|n| !n.is_dir && n.path == Path::new(path) && n.is_untracked())
    }

    fn schedule_tree_preview(&mut self, pane: TreePane) {
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
        let prev_scroll = self.diff_scroll;
        let prev_cursor = self.diff_cursor;
        let current = self.current_file.clone().zip(self.diff_origin);
        self.clear_diff_cache();
        self.pending_tree_preview = None;

        self.refresh_trees()?;

        // Keep focus unless the current tree became empty.
        if self.is_commit_mode() {
            self.focus = match prev_focus {
                Focus::DiffView | Focus::InlineSelect => Focus::DiffView,
                _ => Focus::Unstaged,
            };
        } else {
            match prev_focus {
                Focus::Unstaged if self.unstaged.is_empty() && !self.staged.is_empty() => {
                    self.focus = Focus::Staged;
                }
                Focus::Staged if self.staged.is_empty() && !self.unstaged.is_empty() => {
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
                    self.reload_current_diff()?;
                    let line_count = self.raw_line_count;
                    self.diff_scroll = prev_scroll.min(line_count.saturating_sub(1));
                    self.diff_cursor = prev_cursor.min(line_count.saturating_sub(1));
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
            SearchScope::DiffView => {
                if let Some(text) = &self.cached_display_text {
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
                    self.display_diff.lines().map(str::to_string).collect()
                }
            }
            SearchScope::InlineSelect => self.raw_diff.lines().map(str::to_string).collect(),
        }
    }

    fn tree_linear_position(&self, pane: TreePane, node_idx: usize) -> usize {
        match pane {
            TreePane::Unstaged => node_idx,
            TreePane::Staged => self.unstaged.all_nodes.len() + node_idx,
        }
    }

    fn decode_tree_linear_position(
        &self,
        scope: SearchScope,
        position: usize,
    ) -> Option<(TreePane, usize)> {
        match scope {
            SearchScope::WorkingTree => {
                let unstaged_len = self.unstaged.all_nodes.len();
                if position < unstaged_len {
                    Some((TreePane::Unstaged, position))
                } else {
                    let staged_idx = position.checked_sub(unstaged_len)?;
                    (staged_idx < self.staged.all_nodes.len())
                        .then_some((TreePane::Staged, staged_idx))
                }
            }
            SearchScope::CommitTree => {
                (position < self.unstaged.all_nodes.len()).then_some((TreePane::Unstaged, position))
            }
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
                                contains_ignore_case(&node.display_row_text(pane), query)
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
                .filter_map(|(idx, line)| contains_ignore_case(&line, query).then_some(idx))
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
            SearchScope::DiffView => self.diff_scroll,
            SearchScope::InlineSelect => self.diff_cursor,
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
            SearchScope::DiffView => {
                self.diff_scroll = target.min(self.display_line_count.saturating_sub(1));
            }
            SearchScope::InlineSelect => {
                self.diff_cursor = target.min(self.raw_line_count.saturating_sub(1));
                self.sync_hunk_cursor();
                self.ensure_cursor_visible();
            }
        }
    }

    fn begin_search(&mut self) {
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
        if let Some(target) = next_match_from(&matches, current, true) {
            self.apply_search_target(scope, target);
        }
    }

    fn navigate_search(&mut self, forward: bool) {
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
            next_match_from(&matches, current, false)
        } else {
            prev_match_from(&matches, current, false)
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
                let command_result =
                    crate::git::run_interactive_command(&self.commit_command, &self.repo_root);
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
            let diff_width_changed = self.diff_pane_width != next_diff_width;

            self.diff_pane_width = next_diff_width;
            self.diff_pane_height = size.height.saturating_sub(3) as usize;

            if diff_width_changed && self.tool == DiffTool::Delta && self.current_file.is_some() {
                let _ = self.reload_current_diff();
            }

            self.flush_pending_tree_preview_if_due();
            terminal.draw(|f| crate::ui::render(f, self))?;

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

                if saw_resize && self.tool == DiffTool::Delta && self.current_file.is_some() {
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

    fn handle_tree_key(&mut self, key: KeyEvent) -> Result<()> {
        if !matches!(
            key.code,
            KeyCode::Char('j') | KeyCode::Down | KeyCode::Char('k') | KeyCode::Up
        ) {
            self.pending_tree_preview = None;
        }

        if self.can_trigger_commit_action(key) {
            self.pending_action = Some(ExternalAction::Commit);
            return Ok(());
        }

        match key.code {
            KeyCode::Char('q') => {
                self.should_quit = true;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.tree_move_down();
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.tree_move_up();
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.tree_move_half_page_down();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.tree_move_half_page_up();
            }
            KeyCode::Char('l') | KeyCode::Right => {
                self.tree_action_right()?;
            }
            KeyCode::Char('h') | KeyCode::Left => {
                self.tree_action_left();
            }
            KeyCode::Char('u')
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.tree_stage_or_unstage()?;
            }
            KeyCode::Enter if self.is_commit_mode() => {
                self.tree_action_right()?;
            }
            KeyCode::Char('c') => {
                self.tree_copy_path_to_clipboard();
            }
            KeyCode::Char('/') => {
                self.begin_search();
            }
            KeyCode::Char('n') => {
                self.navigate_search(true);
            }
            KeyCode::Char('N') => {
                self.navigate_search(false);
            }
            KeyCode::Char('?') => {
                self.status_message = Some(self.tree_help_text());
            }
            _ => {}
        }
        Ok(())
    }

    fn tree_move_down(&mut self) {
        let moved = self.tree_step_down();
        self.finish_tree_move(moved);
    }

    fn tree_move_up(&mut self) {
        let moved = self.tree_step_up();
        self.finish_tree_move(moved);
    }

    fn tree_move_half_page_down(&mut self) {
        let mut moved = false;
        for _ in 0..TREE_FAST_MOVE_LINES {
            if !self.tree_step_down() {
                break;
            }
            moved = true;
        }
        self.finish_tree_move(moved);
    }

    fn tree_move_half_page_up(&mut self) {
        let mut moved = false;
        for _ in 0..TREE_FAST_MOVE_LINES {
            if !self.tree_step_up() {
                break;
            }
            moved = true;
        }
        self.finish_tree_move(moved);
    }

    fn tree_step_down(&mut self) -> bool {
        let pane = match self.focused_pane() {
            Some(p) => p,
            None => return false,
        };

        let can_move = {
            let tree = self.tree(pane);
            !tree.is_empty() && tree.cursor + 1 < tree.visible.len()
        };

        if can_move {
            self.tree_mut(pane).cursor += 1;
            true
        } else if !self.is_commit_mode() && pane == TreePane::Unstaged && !self.staged.is_empty() {
            self.focus = Focus::Staged;
            self.staged.cursor = 0;
            true
        } else {
            false
        }
    }

    fn tree_step_up(&mut self) -> bool {
        let pane = match self.focused_pane() {
            Some(p) => p,
            None => return false,
        };

        let can_move = {
            let tree = self.tree(pane);
            tree.cursor > 0
        };

        if can_move {
            self.tree_mut(pane).cursor -= 1;
            true
        } else if !self.is_commit_mode() && pane == TreePane::Staged && !self.unstaged.is_empty() {
            self.focus = Focus::Unstaged;
            self.unstaged.cursor = self.unstaged.visible.len().saturating_sub(1);
            true
        } else {
            false
        }
    }

    fn finish_tree_move(&mut self, moved: bool) {
        if moved {
            if let Some(next_pane) = self.focused_pane() {
                self.schedule_tree_preview(next_pane);
            }
        }
    }

    /// l key: expand dir (and move cursor to first child) or open file diff
    fn tree_action_right(&mut self) -> Result<()> {
        let pane = match self.focused_pane() {
            Some(p) => p,
            None => return Ok(()),
        };

        let (is_dir, path) = {
            let section = self.tree(pane);
            match section.current_node() {
                Some(n) => (n.is_dir, n.path.to_string_lossy().to_string()),
                None => return Ok(()),
            }
        };

        if is_dir {
            self.tree_mut(pane).expand_and_enter();
            self.tree_load_preview();
        } else {
            self.load_diff(&path, pane, DiffViewMode::Patch)?;
            self.focus = Focus::DiffView;
        }
        Ok(())
    }

    /// h key: fold the parent directory of the current node
    fn tree_action_left(&mut self) {
        let pane = match self.focused_pane() {
            Some(p) => p,
            None => return,
        };

        self.tree_mut(pane).fold_parent();
    }

    /// Stage or unstage the selected file or directory in working tree mode.
    fn tree_stage_or_unstage(&mut self) -> Result<()> {
        if self.is_commit_mode() {
            return self.tree_action_right();
        }

        let pane = match self.focused_pane() {
            Some(p) => p,
            None => return Ok(()),
        };

        let (is_dir, path) = {
            let section = self.tree(pane);
            match section.current_node() {
                Some(n) => (n.is_dir, n.path.to_string_lossy().to_string()),
                None => return Ok(()),
            }
        };

        match pane {
            TreePane::Unstaged => {
                if is_dir {
                    let files = self.unstaged.files_under_dir(Path::new(&path));
                    for file in &files {
                        let _ = crate::git::apply::stage_file(file, &self.repo_root);
                    }
                    self.status_message = Some(format!("Staged directory: {}", path));
                } else {
                    match crate::git::apply::stage_file(&path, &self.repo_root) {
                        Ok(_) => self.status_message = Some(format!("Staged: {}", path)),
                        Err(e) => {
                            self.error_message = Some(format!("Error: {}", e));
                            return Ok(());
                        }
                    }
                }
            }
            TreePane::Staged => {
                if is_dir {
                    let files = self.staged.files_under_dir(Path::new(&path));
                    for file in &files {
                        let _ = crate::git::apply::unstage_file(file, &self.repo_root);
                    }
                    self.status_message = Some(format!("Unstaged directory: {}", path));
                } else {
                    match crate::git::apply::unstage_file(&path, &self.repo_root) {
                        Ok(_) => self.status_message = Some(format!("Unstaged: {}", path)),
                        Err(e) => {
                            self.error_message = Some(format!("Error: {}", e));
                            return Ok(());
                        }
                    }
                }
            }
        }

        self.refresh_after_tree_op()?;
        Ok(())
    }

    fn tree_copy_path_to_clipboard(&mut self) {
        let pane = match self.focused_pane() {
            Some(p) => p,
            None => return,
        };

        let path = {
            let section = self.tree(pane);
            match section.current_node() {
                Some(n) => n.path.to_string_lossy().to_string(),
                None => return,
            }
        };

        self.copy_path_to_clipboard(&path);
    }

    fn diff_copy_path_to_clipboard(&mut self) {
        let path = match self.current_file.clone() {
            Some(path) => path,
            None => {
                self.error_message = Some("No file selected".to_string());
                return;
            }
        };

        self.copy_path_to_clipboard(&path);
    }

    fn full_file_clipboard_text(&self) -> Option<&str> {
        (self.diff_view_mode.is_full_file()
            && self.full_file_copyable
            && self.current_file.is_some())
        .then_some(self.raw_diff.as_str())
    }

    fn diff_copy_full_file_to_clipboard(&mut self) {
        let path = match self.current_file.clone() {
            Some(path) => path,
            None => {
                self.error_message = Some("No file selected".to_string());
                return;
            }
        };

        let Some(text) = self.full_file_clipboard_text() else {
            self.error_message = Some(
                "Whole-file copy is available only when full file contents are open".to_string(),
            );
            return;
        };

        match clipboard::copy_text(text) {
            Ok(_) => self.status_message = Some(format!("Copied file contents: {}", path)),
            Err(e) => self.error_message = Some(format!("Clipboard error: {}", e)),
        }
    }

    fn diff_search_is_active(&self) -> bool {
        self.search_state
            .as_ref()
            .is_some_and(|search| search.scope == SearchScope::DiffView)
    }

    fn toggle_full_file_line_numbers(&mut self) -> Result<()> {
        let (Some(path), Some(pane)) = (self.current_file.clone(), self.diff_origin) else {
            return Ok(());
        };
        let view_mode = self.diff_view_mode;
        let preserved_scroll = self.diff_scroll;

        self.full_file_show_line_numbers = !self.full_file_show_line_numbers;
        self.load_diff(&path, pane, view_mode)?;
        self.diff_scroll = preserved_scroll.min(self.display_line_count.saturating_sub(1));

        self.status_message = Some(if self.full_file_show_line_numbers {
            "Line numbers: on".to_string()
        } else {
            "Line numbers: off".to_string()
        });

        Ok(())
    }

    fn copy_path_to_clipboard(&mut self, path: &str) {
        match clipboard::copy_text(path) {
            Ok(_) => self.status_message = Some(format!("Copied path: {}", path)),
            Err(e) => self.error_message = Some(format!("Clipboard error: {}", e)),
        }
    }

    /// Load diff preview when cursor moves in tree
    fn tree_load_preview(&mut self) {
        let pane = match self.focused_pane() {
            Some(p) => p,
            None => return,
        };
        self.tree_load_preview_for_pane(pane);
    }

    fn tree_load_preview_for_pane(&mut self, pane: TreePane) {
        let (is_dir, path) = {
            let section = self.tree(pane);
            match section.current_node() {
                Some(n) => (n.is_dir, n.path.to_string_lossy().to_string()),
                None => {
                    self.clear_diff();
                    return;
                }
            }
        };

        if is_dir {
            return;
        }

        let _ = self.load_diff(&path, pane, DiffViewMode::Patch);
    }

    fn refresh_after_tree_op(&mut self) -> Result<()> {
        self.clear_diff_cache();
        self.pending_tree_preview = None;

        let prev_focus = self.focus.clone();
        self.refresh_trees()?;

        match prev_focus {
            Focus::Unstaged if self.unstaged.is_empty() && !self.staged.is_empty() => {
                self.focus = Focus::Staged;
            }
            Focus::Staged if self.staged.is_empty() && !self.unstaged.is_empty() => {
                self.focus = Focus::Unstaged;
            }
            _ => {}
        }

        self.tree_load_preview();
        Ok(())
    }

    /// File line number (1-based) that the patch pane's currently top-displayed row
    /// corresponds to, on the given full-file `source` side. `None` means no reliable
    /// mapping is available (e.g. above the first hunk, or a display the mapping can't
    /// parse), in which case the caller should leave the full-file view at the top.
    fn patch_top_line_target(&self, source: FullFileSource) -> Option<usize> {
        match self.tool {
            DiffTool::Raw => self.raw_patch_top_line_target(source),
            DiffTool::Delta => self.delta_patch_top_line_target(source),
            DiffTool::Difftastic => None,
        }
    }

    /// Exact for `--tool raw`, since `display_diff` is `raw_diff` verbatim there, so
    /// `diff_scroll` indexes directly into `line_infos` (built from the same raw text).
    fn raw_patch_top_line_target(&self, source: FullFileSource) -> Option<usize> {
        let info = self.line_infos.get(self.diff_scroll)?;
        let hunk = self.file_diff.hunks.get(info.hunk_idx?)?;

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
        let lines: Vec<&str> = self.display_diff.lines().collect();
        let total = lines.len();
        let mut row = self.diff_scroll.min(total.checked_sub(1)?);

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

    /// Scroll offset that places `file_line` (1-based) at the very top of the pane.
    fn full_file_scroll_for_line(&self, file_line: usize) -> usize {
        let target_row = self.full_file_content_offset + file_line.saturating_sub(1);
        target_row.min(self.display_line_count.saturating_sub(1))
    }

    fn toggle_full_file_view(&mut self, source: FullFileSource) -> Result<()> {
        let (Some(path), Some(pane)) = (self.current_file.clone(), self.diff_origin) else {
            return Ok(());
        };

        let next_mode = self.diff_view_mode.toggle_full_file(source);
        let target_line = (self.diff_view_mode == DiffViewMode::Patch && next_mode.is_full_file())
            .then(|| self.patch_top_line_target(source))
            .flatten();
        // Switching between FullFile(Current) and FullFile(Previous) (not going through
        // patch view) keeps the same scroll row, since both sides render with the same
        // content offset and are almost always line-aligned for a small diff.
        let preserved_full_file_scroll = (self.diff_view_mode.is_full_file()
            && next_mode.is_full_file())
        .then_some(self.diff_scroll);

        self.load_diff(&path, pane, next_mode)?;

        if let Some(file_line) = target_line {
            self.diff_scroll = self.full_file_scroll_for_line(file_line);
        } else if let Some(scroll) = preserved_full_file_scroll {
            self.diff_scroll = scroll.min(self.display_line_count.saturating_sub(1));
        }

        self.status_message = Some(match next_mode {
            DiffViewMode::Patch => "Patch view".to_string(),
            DiffViewMode::FullFile(source) => source.status_message().to_string(),
        });
        Ok(())
    }

    fn leave_diff_view_to_tree(&mut self) -> Result<()> {
        let target_focus = self
            .diff_origin
            .map(|p| p.to_focus())
            .unwrap_or(Focus::Unstaged);

        if self.diff_view_mode.is_full_file() {
            if let (Some(path), Some(pane)) = (self.current_file.clone(), self.diff_origin) {
                self.load_diff(&path, pane, DiffViewMode::Patch)?;
            } else {
                self.diff_view_mode = DiffViewMode::Patch;
                self.content_annotation = None;
            }
        }

        self.focus = target_focus;
        Ok(())
    }

    // ─── Diff view key handling ─────────────────────────────────────────

    fn handle_diff_key(&mut self, key: KeyEvent) -> Result<()> {
        let line_count = self.display_line_count;
        let half_page = (self.diff_pane_height / 2).max(1);
        let is_full_file_view = self.diff_view_mode.is_full_file();

        if self.can_trigger_commit_action(key) {
            self.pending_action = Some(ExternalAction::Commit);
            return Ok(());
        }

        match key.code {
            KeyCode::Char('q') => {
                self.should_quit = true;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if self.diff_scroll + 1 < line_count {
                    self.diff_scroll += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.diff_scroll > 0 {
                    self.diff_scroll -= 1;
                }
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.diff_scroll = (self.diff_scroll + half_page).min(line_count.saturating_sub(1));
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.diff_scroll = self.diff_scroll.saturating_sub(half_page);
            }
            KeyCode::Char('g') => {
                self.diff_scroll = 0;
            }
            KeyCode::Char('G') => {
                self.diff_scroll = line_count.saturating_sub(1);
            }
            KeyCode::Char('/') => {
                self.begin_search();
            }
            KeyCode::Char('n') => {
                if is_full_file_view && !self.diff_search_is_active() {
                    self.toggle_full_file_line_numbers()?;
                } else {
                    self.navigate_search(true);
                }
            }
            KeyCode::Char('N') => self.navigate_search(false),
            KeyCode::Char(']') if !is_full_file_view => self.jump_next_hunk(),
            KeyCode::Char('[') if !is_full_file_view => self.jump_prev_hunk(),
            KeyCode::Char('c') => {
                self.diff_copy_path_to_clipboard();
            }
            KeyCode::Char('P') => {
                self.diff_copy_full_file_to_clipboard();
            }
            KeyCode::Char('f') => {
                self.toggle_full_file_view(FullFileSource::Current)?;
            }
            KeyCode::Char('F') => {
                self.toggle_full_file_view(FullFileSource::Previous)?;
            }
            KeyCode::Char('h') | KeyCode::Left => {
                self.leave_diff_view_to_tree()?;
            }
            KeyCode::Char('v') => {
                if is_full_file_view {
                    self.error_message =
                        Some("Line selection unavailable in full file view".to_string());
                } else if self.is_commit_mode() {
                    self.error_message = Some("Commit diff is read-only".to_string());
                } else if self.tool.supports_line_ops() {
                    if self.file_diff.hunks.is_empty() {
                        self.error_message = Some("No hunks to select lines from".to_string());
                    } else {
                        self.focus = Focus::InlineSelect;
                        self.diff_cursor = self.diff_scroll;
                        self.status_message =
                            Some("Inline select: j/k move  u apply  v/h exit".to_string());
                    }
                } else {
                    self.error_message =
                        Some("Line selection unavailable with difftastic".to_string());
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn jump_next_hunk(&mut self) {
        let count = self.file_diff.hunks.len();
        if count == 0 {
            return;
        }
        if self.hunk_cursor + 1 < count {
            self.hunk_cursor += 1;
        }
        self.scroll_to_hunk(self.hunk_cursor);
    }

    fn jump_prev_hunk(&mut self) {
        if self.file_diff.hunks.is_empty() {
            return;
        }
        if self.hunk_cursor > 0 {
            self.hunk_cursor -= 1;
        }
        self.scroll_to_hunk(self.hunk_cursor);
    }

    fn scroll_to_hunk(&mut self, hunk_idx: usize) {
        let mut hunk_count = 0usize;
        let content = if self.focus == Focus::InlineSelect {
            &self.raw_diff
        } else {
            &self.display_diff
        };
        for (line_no, line) in content.lines().enumerate() {
            if line.starts_with("@@") {
                if hunk_count == hunk_idx {
                    if self.focus == Focus::InlineSelect {
                        self.diff_cursor = line_no;
                        self.diff_scroll = line_no;
                    } else {
                        self.diff_scroll = line_no;
                    }
                    return;
                }
                hunk_count += 1;
            }
        }
    }

    // ─── Inline select key handling ─────────────────────────────────────

    fn handle_inline_select_key(&mut self, key: KeyEvent) -> Result<()> {
        let line_count = self.raw_line_count;
        let half_page = (self.diff_pane_height / 2).max(1);

        match key.code {
            KeyCode::Char('q') => {
                self.should_quit = true;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if self.diff_cursor + 1 < line_count {
                    self.diff_cursor += 1;
                    self.sync_hunk_cursor();
                    if self.diff_cursor >= self.diff_scroll + self.diff_pane_height {
                        self.diff_scroll = self.diff_cursor + 1 - self.diff_pane_height;
                    }
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.diff_cursor > 0 {
                    self.diff_cursor -= 1;
                    self.sync_hunk_cursor();
                    if self.diff_cursor < self.diff_scroll {
                        self.diff_scroll = self.diff_cursor;
                    }
                }
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.diff_cursor = (self.diff_cursor + half_page).min(line_count.saturating_sub(1));
                self.sync_hunk_cursor();
                if self.diff_cursor >= self.diff_scroll + self.diff_pane_height {
                    self.diff_scroll = self.diff_cursor + 1 - self.diff_pane_height;
                }
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.diff_cursor = self.diff_cursor.saturating_sub(half_page);
                self.sync_hunk_cursor();
                if self.diff_cursor < self.diff_scroll {
                    self.diff_scroll = self.diff_cursor;
                }
            }
            KeyCode::Char('u')
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.apply_current_line()?;
            }
            KeyCode::Char('/') => {
                self.begin_search();
            }
            KeyCode::Char('n') => self.navigate_search(true),
            KeyCode::Char('N') => self.navigate_search(false),
            KeyCode::Char(']') => self.jump_next_hunk(),
            KeyCode::Char('[') => self.jump_prev_hunk(),
            KeyCode::Char('v') => {
                self.focus = Focus::DiffView;
            }
            KeyCode::Char('h') | KeyCode::Left => {
                self.focus = self
                    .diff_origin
                    .map(|p| p.to_focus())
                    .unwrap_or(Focus::Unstaged);
            }
            _ => {}
        }
        Ok(())
    }

    fn sync_hunk_cursor(&mut self) {
        if let Some(info) = self.line_infos.get(self.diff_cursor) {
            if let Some(new_hunk) = info.hunk_idx {
                self.hunk_cursor = new_hunk;
            }
        }
    }

    fn apply_current_line(&mut self) -> Result<()> {
        if self.is_commit_mode() {
            self.error_message = Some("Commit diff is read-only".to_string());
            return Ok(());
        }

        let info = match self.line_infos.get(self.diff_cursor) {
            Some(i) => i.clone(),
            None => return Ok(()),
        };

        if !info.is_selectable {
            self.error_message = Some("Only +/- lines can be applied".to_string());
            return Ok(());
        }

        let hunk_idx = match info.hunk_idx {
            Some(h) => h,
            None => return Ok(()),
        };
        let line_in_hunk = match info.line_in_hunk {
            Some(l) => l,
            None => return Ok(()),
        };

        let file = match &self.current_file {
            Some(f) => f.clone(),
            None => return Ok(()),
        };
        let hunk = match self.file_diff.hunks.get(hunk_idx).cloned() {
            Some(h) => h,
            None => return Ok(()),
        };
        let pane = match self.diff_origin {
            Some(p) => p,
            None => return Ok(()),
        };

        let selected: HashSet<usize> = [line_in_hunk].into_iter().collect();

        let result = match pane {
            TreePane::Unstaged => {
                crate::git::apply::stage_lines(&file, &hunk, &selected, &self.repo_root)
            }
            TreePane::Staged => {
                crate::git::apply::unstage_lines(&file, &hunk, &selected, &self.repo_root)
            }
        };

        match result {
            Ok(_) => {
                let action = if pane.is_staged() {
                    "Unstaged"
                } else {
                    "Staged"
                };
                self.status_message = Some(format!("{} 1 line", action));
                self.clear_diff_cache();
                self.refresh_trees()?;

                let prev_cursor = self.diff_cursor;
                self.reload_current_diff()?;

                if self.file_diff.hunks.is_empty() && self.raw_diff.trim().is_empty() {
                    self.clear_diff();
                    self.focus = pane.to_focus();
                } else {
                    self.move_to_next_selectable(prev_cursor);
                }
            }
            Err(e) => self.error_message = Some(format!("Error: {}", e)),
        }
        Ok(())
    }

    fn move_to_next_selectable(&mut self, from: usize) {
        let line_count = self.line_infos.len();
        for i in from..line_count {
            if let Some(info) = self.line_infos.get(i) {
                if info.is_selectable {
                    self.diff_cursor = i;
                    self.ensure_cursor_visible();
                    return;
                }
            }
        }
        for i in (0..from).rev() {
            if let Some(info) = self.line_infos.get(i) {
                if info.is_selectable {
                    self.diff_cursor = i;
                    self.ensure_cursor_visible();
                    return;
                }
            }
        }
        self.diff_cursor = from.min(line_count.saturating_sub(1));
    }

    fn ensure_cursor_visible(&mut self) {
        if self.diff_cursor < self.diff_scroll {
            self.diff_scroll = self.diff_cursor;
        } else if self.diff_cursor >= self.diff_scroll + self.diff_pane_height {
            self.diff_scroll = self.diff_cursor + 1 - self.diff_pane_height;
        }
    }
}

// Helper to rebuild visible + clamp (avoids borrow issues)
fn rebuild_section_visible(section: &mut TreeSection) {
    section.rebuild_visible();
    section.clamp_cursor();
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

fn contains_ignore_case(text: &str, query: &str) -> bool {
    text.to_lowercase().contains(&query.to_lowercase())
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

fn next_match_from(matches: &[usize], current: usize, inclusive: bool) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }

    let predicate = |candidate: &usize| {
        if inclusive {
            *candidate >= current
        } else {
            *candidate > current
        }
    };

    matches
        .iter()
        .copied()
        .find(predicate)
        .or_else(|| matches.first().copied())
}

fn prev_match_from(matches: &[usize], current: usize, inclusive: bool) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }

    let predicate = |candidate: &usize| {
        if inclusive {
            *candidate <= current
        } else {
            *candidate < current
        }
    };

    matches
        .iter()
        .copied()
        .rev()
        .find(predicate)
        .or_else(|| matches.last().copied())
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
    fn next_match_wraps_forward() {
        let matches = vec![2, 5, 9];

        assert_eq!(next_match_from(&matches, 2, false), Some(5));
        assert_eq!(next_match_from(&matches, 9, false), Some(2));
        assert_eq!(next_match_from(&matches, 5, true), Some(5));
    }

    #[test]
    fn prev_match_wraps_backward() {
        let matches = vec![2, 5, 9];

        assert_eq!(prev_match_from(&matches, 5, false), Some(2));
        assert_eq!(prev_match_from(&matches, 2, false), Some(9));
        assert_eq!(prev_match_from(&matches, 5, true), Some(5));
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
        app.commit_revision = Some("deadbeef".to_string());

        assert!(app.tree_help_text().contains("[l/Enter]open"));
    }

    #[test]
    fn inline_select_help_uses_u_for_apply() {
        let app = make_test_app();

        assert!(app.inline_select_help_text().contains("[u]apply"));
        assert!(!app.inline_select_help_text().contains("[Enter]apply"));
    }

    #[test]
    fn contains_ignore_case_matches_mixed_case() {
        assert!(contains_ignore_case("src/FooBar.rs", "foobar"));
        assert!(contains_ignore_case("src/FooBar.rs", "FOO"));
        assert!(!contains_ignore_case("src/FooBar.rs", "baz"));
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
        app.commit_revision = Some("deadbeef".to_string());
        build_section(
            &mut app.unstaged.all_nodes,
            &[("added.txt".to_string(), 'A', 'A')],
        );
        rebuild_section_visible(&mut app.unstaged);

        let file_state = app
            .file_selection_state("added.txt", TreePane::Unstaged)
            .expect("commit-mode file state should exist");

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
            commit_revision: None,
            tree_pane_percentage: 25,
            commit_key: KeyBinding::default_commit(),
            commit_command: vec!["git".to_string(), "commit".to_string(), "-v".to_string()],
            pending_action: None,
            unstaged: TreeSection::new(),
            staged: TreeSection::new(),
            diff_origin: None,
            diff_view_mode: DiffViewMode::Patch,
            display_diff: String::new(),
            raw_diff: String::new(),
            file_diff: FileDiff::default(),
            diff_scroll: 0,
            diff_cursor: 0,
            hunk_cursor: 0,
            current_file: None,
            line_infos: Vec::new(),
            display_line_count: 0,
            raw_line_count: 0,
            cached_display_text: None,
            content_annotation: None,
            full_file_copyable: false,
            full_file_content_offset: 0,
            full_file_show_line_numbers: true,
            diff_pane_height: 20,
            diff_pane_width: 80,
            pending_tree_preview: None,
            tree_preview_debounce: Duration::from_millis(TREE_PREVIEW_DEBOUNCE_MS),
            diff_cache: HashMap::new(),
            diff_cache_order: VecDeque::new(),
            diff_cache_capacity: DIFF_CACHE_CAPACITY,
            diff_scroll_positions: HashMap::new(),
            search_state: None,
            search_input: None,
            status_message: None,
            error_message: None,
        }
    }

    #[test]
    fn working_tree_search_matches_both_sections() {
        let mut app = make_test_app();
        build_section(
            &mut app.unstaged.all_nodes,
            &[("src/alpha.rs".to_string(), ' ', 'M')],
        );
        rebuild_section_visible(&mut app.unstaged);
        build_section(
            &mut app.staged.all_nodes,
            &[("src/beta.rs".to_string(), 'M', ' ')],
        );
        rebuild_section_visible(&mut app.staged);

        let matches = app.collect_search_matches(SearchScope::WorkingTree, "beta");
        let staged_file_idx = app
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
        build_section(
            &mut app.unstaged.all_nodes,
            &[("src/alpha.rs".to_string(), ' ', 'M')],
        );
        rebuild_section_visible(&mut app.unstaged);
        build_section(
            &mut app.staged.all_nodes,
            &[("src/beta.rs".to_string(), 'M', ' ')],
        );
        rebuild_section_visible(&mut app.staged);

        let staged_file_idx = app
            .staged
            .all_nodes
            .iter()
            .position(|node| node.path == Path::new("src/beta.rs"))
            .unwrap();
        let target = app.tree_linear_position(TreePane::Staged, staged_file_idx);

        app.apply_search_target(SearchScope::WorkingTree, target);

        assert_eq!(app.focus, Focus::Staged);
        assert_eq!(
            app.staged.current_node().unwrap().path,
            Path::new("src/beta.rs")
        );
    }

    #[test]
    fn working_tree_search_matches_directory_nodes() {
        let mut app = make_test_app();
        build_section(
            &mut app.unstaged.all_nodes,
            &[
                ("src/alpha.rs".to_string(), ' ', 'M'),
                ("tests/beta.rs".to_string(), ' ', 'M'),
            ],
        );
        rebuild_section_visible(&mut app.unstaged);

        let matches = app.collect_search_matches(SearchScope::WorkingTree, "src");
        let src_dir_idx = app
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
        build_section(
            &mut app.unstaged.all_nodes,
            &[
                ("src/alpha.rs".to_string(), ' ', 'M'),
                ("tests/beta.rs".to_string(), ' ', 'M'),
            ],
        );
        rebuild_section_visible(&mut app.unstaged);

        let tests_file_vis_idx = app
            .unstaged
            .visible
            .iter()
            .position(|&idx| app.unstaged.all_nodes[idx].path == Path::new("tests/beta.rs"))
            .unwrap();
        app.unstaged.cursor = tests_file_vis_idx;

        app.apply_confirmed_search(SearchScope::WorkingTree, "src".to_string());

        assert_eq!(app.unstaged.current_node().unwrap().path, Path::new("src"));
    }

    #[test]
    fn navigate_search_moves_to_hidden_directory_match() {
        let mut app = make_test_app();
        build_section(
            &mut app.unstaged.all_nodes,
            &[
                ("alpha.txt".to_string(), ' ', 'M'),
                ("src/nested/file.txt".to_string(), ' ', 'M'),
            ],
        );
        rebuild_section_visible(&mut app.unstaged);

        let src_dir_idx = app
            .unstaged
            .all_nodes
            .iter()
            .position(|node| node.is_dir && node.path == Path::new("src"))
            .unwrap();
        app.unstaged.all_nodes[src_dir_idx].expanded = false;
        rebuild_section_visible(&mut app.unstaged);
        app.unstaged.cursor = 0;
        app.search_state = Some(SearchState {
            scope: SearchScope::WorkingTree,
            query: "nested".to_string(),
        });

        app.navigate_search(true);

        let current = app.unstaged.current_node().unwrap();
        assert!(current.is_dir);
        assert_eq!(current.path, Path::new("src/nested"));
        assert!(app.unstaged.all_nodes[src_dir_idx].expanded);
    }

    #[test]
    fn tree_commit_key_sets_pending_action() {
        let mut app = make_test_app();

        app.handle_tree_key(KeyEvent::new(KeyCode::Char('C'), KeyModifiers::NONE))
            .unwrap();

        assert_eq!(app.pending_action, Some(ExternalAction::Commit));
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
        app.line_infos = App::build_preview_line_infos_for(
            "@@ -1 +1 @@\n-looks like diff\n+but is file content\n",
        );

        assert_eq!(app.line_infos.len(), 3);
        assert!(app.line_infos.iter().all(|info| !info.is_selectable));
        assert!(app.line_infos.iter().all(|info| info.hunk_idx.is_none()));
    }

    #[test]
    fn diff_search_uses_cached_display_text_in_raw_mode() {
        let mut app = make_test_app();
        app.display_diff = "\u{1b}[31mprin\u{1b}[0m\u{1b}[32mtln!\u{1b}[0m\n".to_string();
        app.cached_display_text = Some(Text::from("println!\n"));

        let matches = app.collect_search_matches(SearchScope::DiffView, "println!");

        assert_eq!(matches, vec![0]);
    }

    #[test]
    fn files_under_dir_collects_nested_untracked_files() {
        let mut app = make_test_app();
        build_section(
            &mut app.unstaged.all_nodes,
            &[
                ("hoge/a.txt".to_string(), '?', '?'),
                ("hoge/nested/b.txt".to_string(), '?', '?'),
            ],
        );

        let files = app.unstaged.files_under_dir(Path::new("hoge"));

        assert_eq!(
            files,
            vec!["hoge/a.txt".to_string(), "hoge/nested/b.txt".to_string()]
        );
    }

    #[test]
    fn resolve_full_file_target_uses_previous_side_for_deleted_files() {
        let mut app = make_test_app();
        let deleted = FileSelectionState {
            status: 'D',
            is_unmerged: false,
            is_untracked: false,
        };

        assert_eq!(
            app.resolve_full_file_content_target(
                "gone.txt",
                TreePane::Unstaged,
                deleted,
                FullFileSource::Previous,
            ),
            Ok(FullFileContentTarget::Revision {
                rev_spec: ":gone.txt".to_string(),
                content_annotation: Some(ContentAnnotation::BeforeDelete),
            })
        );
        assert_eq!(
            app.resolve_full_file_content_target(
                "gone.txt",
                TreePane::Staged,
                deleted,
                FullFileSource::Previous,
            ),
            Ok(FullFileContentTarget::Revision {
                rev_spec: "HEAD:gone.txt".to_string(),
                content_annotation: Some(ContentAnnotation::BeforeDelete),
            })
        );

        app.commit_revision = Some("deadbeef".to_string());
        assert_eq!(
            app.resolve_full_file_content_target(
                "gone.txt",
                TreePane::Unstaged,
                deleted,
                FullFileSource::Previous,
            ),
            Ok(FullFileContentTarget::Revision {
                rev_spec: "deadbeef^:gone.txt".to_string(),
                content_annotation: Some(ContentAnnotation::BeforeDelete),
            })
        );
    }

    #[test]
    fn resolve_full_file_target_rejects_missing_current_and_previous_states() {
        let app = make_test_app();
        let deleted = FileSelectionState {
            status: 'D',
            is_unmerged: false,
            is_untracked: false,
        };
        let added = FileSelectionState {
            status: 'A',
            is_unmerged: false,
            is_untracked: false,
        };
        let untracked = FileSelectionState {
            status: '?',
            is_unmerged: false,
            is_untracked: true,
        };

        assert_eq!(
            app.resolve_full_file_content_target(
                "gone.txt",
                TreePane::Unstaged,
                deleted,
                FullFileSource::Current,
            ),
            Err(FullFileSource::Current.missing_message())
        );
        assert_eq!(
            app.resolve_full_file_content_target(
                "new.txt",
                TreePane::Staged,
                added,
                FullFileSource::Previous,
            ),
            Err(FullFileSource::Previous.missing_message())
        );
        assert_eq!(
            app.resolve_full_file_content_target(
                "scratch.txt",
                TreePane::Unstaged,
                untracked,
                FullFileSource::Previous,
            ),
            Err(FullFileSource::Previous.missing_message())
        );
    }

    #[test]
    fn diff_view_mode_toggles_requested_full_file_source() {
        assert_eq!(
            DiffViewMode::Patch.toggle_full_file(FullFileSource::Current),
            DiffViewMode::FullFile(FullFileSource::Current)
        );
        assert_eq!(
            DiffViewMode::Patch.toggle_full_file(FullFileSource::Previous),
            DiffViewMode::FullFile(FullFileSource::Previous)
        );
        assert_eq!(
            DiffViewMode::FullFile(FullFileSource::Current)
                .toggle_full_file(FullFileSource::Current),
            DiffViewMode::Patch
        );
        assert_eq!(
            DiffViewMode::FullFile(FullFileSource::Current)
                .toggle_full_file(FullFileSource::Previous),
            DiffViewMode::FullFile(FullFileSource::Previous)
        );
    }

    #[test]
    fn diff_help_text_switches_toggle_labels_in_full_file_views() {
        let mut app = make_test_app();

        assert!(app.diff_help_text().contains("[f]file"));
        assert!(app.diff_help_text().contains("[F]prev-file"));
        assert!(!app.diff_help_text().contains("[P]copy-file"));

        app.diff_view_mode = DiffViewMode::FullFile(FullFileSource::Current);
        assert!(app.diff_help_text().contains("[f]diff"));
        assert!(app.diff_help_text().contains("[F]prev-file"));
        assert!(app.diff_help_text().contains("[P]copy-file"));
        assert!(!app.diff_help_text().contains("[v]select"));

        app.diff_view_mode = DiffViewMode::FullFile(FullFileSource::Previous);
        assert!(app.diff_help_text().contains("[f]file"));
        assert!(app.diff_help_text().contains("[F]diff"));
    }

    #[test]
    fn full_file_clipboard_text_requires_copyable_full_file_state() {
        let mut app = make_test_app();
        app.current_file = Some("src/lib.rs".to_string());
        app.raw_diff = "fn main() {}\n".to_string();

        assert_eq!(app.full_file_clipboard_text(), None);

        app.diff_view_mode = DiffViewMode::FullFile(FullFileSource::Current);
        assert_eq!(app.full_file_clipboard_text(), None);

        app.full_file_copyable = true;
        assert_eq!(app.full_file_clipboard_text(), Some("fn main() {}\n"));

        app.current_file = None;
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
        }
    }

    fn seed_cached_view(
        app: &mut App,
        path: &str,
        pane: TreePane,
        view_mode: DiffViewMode,
        line_count: usize,
    ) {
        let key = app.build_diff_cache_key(path, pane, view_mode);
        app.insert_cached_diff(key, make_cached_diff_with_lines(line_count));
    }

    #[test]
    fn load_diff_restores_saved_scroll_for_patch_but_not_full_file() {
        let mut app = make_test_app();
        for view_mode in [
            DiffViewMode::Patch,
            DiffViewMode::FullFile(FullFileSource::Current),
            DiffViewMode::FullFile(FullFileSource::Previous),
        ] {
            seed_cached_view(&mut app, "file-a.txt", TreePane::Unstaged, view_mode, 120);
            seed_cached_view(&mut app, "file-b.txt", TreePane::Unstaged, view_mode, 120);
        }

        app.load_diff("file-a.txt", TreePane::Unstaged, DiffViewMode::Patch)
            .unwrap();
        app.diff_scroll = 30;

        app.load_diff(
            "file-a.txt",
            TreePane::Unstaged,
            DiffViewMode::FullFile(FullFileSource::Current),
        )
        .unwrap();
        assert_eq!(app.diff_scroll, 0);
        app.diff_scroll = 50;

        app.load_diff("file-b.txt", TreePane::Unstaged, DiffViewMode::Patch)
            .unwrap();
        assert_eq!(app.diff_scroll, 0);
        app.diff_scroll = 7;

        app.load_diff(
            "file-b.txt",
            TreePane::Unstaged,
            DiffViewMode::FullFile(FullFileSource::Current),
        )
        .unwrap();
        assert_eq!(app.diff_scroll, 0);
        app.diff_scroll = 60;

        // Patch scroll is still remembered per file...
        app.load_diff("file-a.txt", TreePane::Unstaged, DiffViewMode::Patch)
            .unwrap();
        assert_eq!(app.diff_scroll, 30);

        // ...but full file view always reopens at the top, regardless of prior scrolling.
        app.load_diff(
            "file-a.txt",
            TreePane::Unstaged,
            DiffViewMode::FullFile(FullFileSource::Previous),
        )
        .unwrap();
        assert_eq!(app.diff_scroll, 0);

        app.load_diff("file-b.txt", TreePane::Unstaged, DiffViewMode::Patch)
            .unwrap();
        assert_eq!(app.diff_scroll, 7);

        app.load_diff(
            "file-b.txt",
            TreePane::Unstaged,
            DiffViewMode::FullFile(FullFileSource::Previous),
        )
        .unwrap();
        assert_eq!(app.diff_scroll, 0);
    }

    #[test]
    fn clear_diff_preserves_saved_scroll_for_patch_but_not_full_file() {
        let mut app = make_test_app();
        seed_cached_view(
            &mut app,
            "file-a.txt",
            TreePane::Unstaged,
            DiffViewMode::Patch,
            120,
        );
        seed_cached_view(
            &mut app,
            "file-a.txt",
            TreePane::Unstaged,
            DiffViewMode::FullFile(FullFileSource::Current),
            120,
        );

        app.load_diff("file-a.txt", TreePane::Unstaged, DiffViewMode::Patch)
            .unwrap();
        app.diff_scroll = 25;

        app.load_diff(
            "file-a.txt",
            TreePane::Unstaged,
            DiffViewMode::FullFile(FullFileSource::Current),
        )
        .unwrap();
        app.diff_scroll = 40;

        app.clear_diff();

        app.load_diff("file-a.txt", TreePane::Unstaged, DiffViewMode::Patch)
            .unwrap();
        assert_eq!(app.diff_scroll, 25);

        app.load_diff(
            "file-a.txt",
            TreePane::Unstaged,
            DiffViewMode::FullFile(FullFileSource::Current),
        )
        .unwrap();
        assert_eq!(app.diff_scroll, 0);
    }

    #[test]
    fn saved_scroll_is_tracked_separately_per_tree_pane_for_patch_only() {
        let mut app = make_test_app();
        for pane in [TreePane::Unstaged, TreePane::Staged] {
            for view_mode in [
                DiffViewMode::Patch,
                DiffViewMode::FullFile(FullFileSource::Current),
                DiffViewMode::FullFile(FullFileSource::Previous),
            ] {
                seed_cached_view(&mut app, "shared.txt", pane, view_mode, 120);
            }
        }

        app.load_diff("shared.txt", TreePane::Unstaged, DiffViewMode::Patch)
            .unwrap();
        app.diff_scroll = 12;

        app.load_diff(
            "shared.txt",
            TreePane::Unstaged,
            DiffViewMode::FullFile(FullFileSource::Current),
        )
        .unwrap();
        assert_eq!(app.diff_scroll, 0);
        app.diff_scroll = 56;

        app.load_diff("shared.txt", TreePane::Staged, DiffViewMode::Patch)
            .unwrap();
        assert_eq!(app.diff_scroll, 0);
        app.diff_scroll = 34;

        app.load_diff(
            "shared.txt",
            TreePane::Staged,
            DiffViewMode::FullFile(FullFileSource::Current),
        )
        .unwrap();
        assert_eq!(app.diff_scroll, 0);
        app.diff_scroll = 78;

        app.load_diff("shared.txt", TreePane::Unstaged, DiffViewMode::Patch)
            .unwrap();
        assert_eq!(app.diff_scroll, 12);

        app.load_diff("shared.txt", TreePane::Staged, DiffViewMode::Patch)
            .unwrap();
        assert_eq!(app.diff_scroll, 34);

        // Full file view never remembers scroll, in either pane.
        app.load_diff(
            "shared.txt",
            TreePane::Unstaged,
            DiffViewMode::FullFile(FullFileSource::Previous),
        )
        .unwrap();
        assert_eq!(app.diff_scroll, 0);

        app.load_diff(
            "shared.txt",
            TreePane::Staged,
            DiffViewMode::FullFile(FullFileSource::Previous),
        )
        .unwrap();
        assert_eq!(app.diff_scroll, 0);
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
    fn raw_patch_top_line_target_maps_context_added_removed_lines() {
        let mut app = make_test_app();
        app.tool = DiffTool::Raw;

        let raw = "diff --git a/file.txt b/file.txt\nindex 111..222 100644\n--- a/file.txt\n+++ b/file.txt\n@@ -10,3 +12,3 @@\n context1\n-removed1\n+added1\n context2\n";
        app.file_diff = parse_diff(raw);
        app.line_infos = App::build_patch_line_infos(raw);

        // Before the first hunk: no mapping.
        app.diff_scroll = 0;
        assert_eq!(app.raw_patch_top_line_target(FullFileSource::Current), None);

        // The "@@" header row itself maps to the hunk's starting lines.
        app.diff_scroll = 4;
        assert_eq!(
            app.raw_patch_top_line_target(FullFileSource::Current),
            Some(12)
        );
        assert_eq!(
            app.raw_patch_top_line_target(FullFileSource::Previous),
            Some(10)
        );

        // " context1" (first content row) — same as the hunk start.
        app.diff_scroll = 5;
        assert_eq!(
            app.raw_patch_top_line_target(FullFileSource::Current),
            Some(12)
        );
        assert_eq!(
            app.raw_patch_top_line_target(FullFileSource::Previous),
            Some(10)
        );

        // "-removed1" — old side advanced past context1, new side unaffected by the removal.
        app.diff_scroll = 6;
        assert_eq!(
            app.raw_patch_top_line_target(FullFileSource::Current),
            Some(13)
        );
        assert_eq!(
            app.raw_patch_top_line_target(FullFileSource::Previous),
            Some(11)
        );

        // "+added1" — new side advanced past context1, old side unaffected by the addition.
        app.diff_scroll = 7;
        assert_eq!(
            app.raw_patch_top_line_target(FullFileSource::Current),
            Some(13)
        );
        assert_eq!(
            app.raw_patch_top_line_target(FullFileSource::Previous),
            Some(12)
        );

        // " context2" — both sides advanced past the added/removed lines.
        app.diff_scroll = 8;
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
    fn delta_patch_top_line_target_finds_nearest_gutter_number_above_blank_rows() {
        let mut app = make_test_app();
        app.tool = DiffTool::Delta;
        app.display_diff = [
            "Δ file.txt",
            "──────────",
            "│ 10 │context1                          │ 12 │context1",
            "│    │                                  │ 13 │added1 that wraps across two rows↴",
            "│    │                                  │    │            …continued",
            "│ 11 │context2                          │ 14 │context2",
        ]
        .join("\n");

        // A row with numbers on both sides: read directly.
        app.diff_scroll = 2;
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
        app.diff_scroll = 4;
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
        app.display_diff = "just some diff text\nwithout a parseable gutter".to_string();
        app.diff_scroll = 1;

        assert_eq!(
            app.delta_patch_top_line_target(FullFileSource::Current),
            None
        );
    }

    #[test]
    fn delta_patch_top_line_target_skips_forward_past_a_single_invalid_row() {
        let mut app = make_test_app();
        app.tool = DiffTool::Delta;
        app.display_diff = [
            "",
            "│ 18 │                                   │ 18 │",
            "│ 19 │use crate::clipboard;              │ 19 │use crate::clipboard;",
        ]
        .join("\n");
        app.diff_scroll = 0; // sitting on delta's leading blank line

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
        app.display_diff = [
            "",
            "",
            "",
            "│ 18 │                                   │ 18 │",
            "│ 19 │use crate::clipboard;              │ 19 │use crate::clipboard;",
        ]
        .join("\n");
        app.diff_scroll = 0;

        assert_eq!(
            app.delta_patch_top_line_target(FullFileSource::Current),
            Some(18)
        );
    }

    #[test]
    fn full_file_scroll_for_line_positions_at_top_and_clamps() {
        let mut app = make_test_app();
        app.full_file_content_offset = 3;
        app.display_line_count = 200;

        assert_eq!(app.full_file_scroll_for_line(50), 52);
        assert_eq!(app.full_file_scroll_for_line(1), 3);

        app.display_line_count = 10;
        assert_eq!(app.full_file_scroll_for_line(1000), 9);
    }

    #[test]
    fn toggle_full_file_view_positions_scroll_from_patch_top_line_for_raw_tool() {
        let mut app = make_test_app();
        app.tool = DiffTool::Raw;
        app.diff_pane_height = 20;

        let raw = "diff --git a/file.txt b/file.txt\nindex 111..222 100644\n--- a/file.txt\n+++ b/file.txt\n@@ -10,3 +12,3 @@\n context1\n-removed1\n+added1\n context2\n";
        app.file_diff = parse_diff(raw);
        app.line_infos = App::build_patch_line_infos(raw);
        app.diff_view_mode = DiffViewMode::Patch;
        app.current_file = Some("file.txt".to_string());
        app.diff_origin = Some(TreePane::Unstaged);
        app.diff_scroll = 8; // " context2" row -> Current(new) file line 14

        let mut full_cached = make_cached_diff_with_lines(500);
        full_cached.full_file_content_offset = 3;
        let key = app.build_diff_cache_key(
            "file.txt",
            TreePane::Unstaged,
            DiffViewMode::FullFile(FullFileSource::Current),
        );
        app.insert_cached_diff(key, full_cached);

        app.toggle_full_file_view(FullFileSource::Current).unwrap();

        assert_eq!(
            app.diff_view_mode,
            DiffViewMode::FullFile(FullFileSource::Current)
        );
        // target_row = content_offset(3) + (file_line 14 - 1) = 16
        assert_eq!(app.diff_scroll, 16);
    }

    #[test]
    fn toggle_full_file_view_opens_at_top_when_no_mapping_available() {
        let mut app = make_test_app();
        app.tool = DiffTool::Difftastic;
        app.diff_view_mode = DiffViewMode::Patch;
        app.current_file = Some("file.txt".to_string());
        app.diff_origin = Some(TreePane::Unstaged);
        app.diff_scroll = 42;

        seed_cached_view(
            &mut app,
            "file.txt",
            TreePane::Unstaged,
            DiffViewMode::FullFile(FullFileSource::Current),
            500,
        );

        app.toggle_full_file_view(FullFileSource::Current).unwrap();

        assert_eq!(app.diff_scroll, 0);
    }

    #[test]
    fn toggle_full_file_view_preserves_scroll_between_current_and_previous() {
        let mut app = make_test_app();
        app.diff_view_mode = DiffViewMode::FullFile(FullFileSource::Current);
        app.current_file = Some("file.txt".to_string());
        app.diff_origin = Some(TreePane::Unstaged);
        app.diff_scroll = 40;

        seed_cached_view(
            &mut app,
            "file.txt",
            TreePane::Unstaged,
            DiffViewMode::FullFile(FullFileSource::Previous),
            500,
        );

        app.toggle_full_file_view(FullFileSource::Previous).unwrap();

        assert_eq!(
            app.diff_view_mode,
            DiffViewMode::FullFile(FullFileSource::Previous)
        );
        assert_eq!(app.diff_scroll, 40);

        seed_cached_view(
            &mut app,
            "file.txt",
            TreePane::Unstaged,
            DiffViewMode::FullFile(FullFileSource::Current),
            500,
        );

        app.toggle_full_file_view(FullFileSource::Current).unwrap();

        assert_eq!(
            app.diff_view_mode,
            DiffViewMode::FullFile(FullFileSource::Current)
        );
        assert_eq!(app.diff_scroll, 40);
    }

    #[test]
    fn toggle_full_file_view_clamps_preserved_scroll_to_shorter_side() {
        let mut app = make_test_app();
        app.diff_view_mode = DiffViewMode::FullFile(FullFileSource::Current);
        app.current_file = Some("file.txt".to_string());
        app.diff_origin = Some(TreePane::Unstaged);
        app.diff_scroll = 40;

        seed_cached_view(
            &mut app,
            "file.txt",
            TreePane::Unstaged,
            DiffViewMode::FullFile(FullFileSource::Previous),
            10,
        );

        app.toggle_full_file_view(FullFileSource::Previous).unwrap();

        assert_eq!(app.diff_scroll, 9);
    }
}
