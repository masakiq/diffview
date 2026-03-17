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
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::clipboard;
use crate::config::Config;
use crate::git::diff::{parse_diff, FileDiff};
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
        self.staged == 'U' || self.unstaged == 'U'
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
}

const DIFF_CACHE_CAPACITY: usize = 64;
const TREE_PREVIEW_DEBOUNCE_MS: u64 = 100;
const TREE_FAST_MOVE_LINES: usize = 5;

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
    pub diff_pane_height: usize,
    pub diff_pane_width: u16,
    pending_tree_preview: Option<PendingTreePreview>,
    tree_preview_debounce: Duration,
    diff_cache: HashMap<DiffCacheKey, CachedDiff>,
    diff_cache_order: VecDeque<DiffCacheKey>,
    diff_cache_capacity: usize,
    search_state: Option<SearchState>,
    search_input: Option<SearchInput>,

    // Status bar
    pub status_message: Option<String>,
    pub error_message: Option<String>,
}

impl App {
    pub fn new(tool_override: Option<String>, revision_override: Option<String>) -> Result<Self> {
        let repo_root = crate::git::get_repo_root()?;
        let commit_revision = match revision_override {
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
            diff_pane_height: 20,
            diff_pane_width: 0,
            pending_tree_preview: None,
            tree_preview_debounce: Duration::from_millis(TREE_PREVIEW_DEBOUNCE_MS),
            diff_cache: HashMap::new(),
            diff_cache_order: VecDeque::new(),
            diff_cache_capacity: DIFF_CACHE_CAPACITY,
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
                let _ = self.load_diff(&path, pane);
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
                "[l]open [h]back [Enter]stage/unstage [c]copy [/]search [n/N]match [Ctrl-U/D]5-lines [j/k]move [r]refresh [{}]commit [?]help [q]quit",
                self.commit_key_label()
            )
        }
    }

    pub fn diff_help_text(&self) -> String {
        let mut ops = format!(
            "[j/k]scroll [Ctrl-U/D]jump [h]back [c]copy [/]search [n/N]match [[]/[]]hunk [r]refresh [q]quit"
        );

        if !self.is_commit_mode() {
            ops.push_str(&format!(" [{}]commit", self.commit_key_label()));
        }
        if !self.is_commit_mode() && self.tool.supports_line_ops() {
            ops.push_str(" [v]select");
        }

        ops
    }

    pub fn inline_select_help_text(&self) -> String {
        "[j/k]move [Ctrl-U/D]jump [Enter]apply [v]back [h]tree [/]search [n/N]match [[]/[]]hunk [r]refresh [q]quit".to_string()
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

    fn build_diff_cache_key(&self, path: &str, pane: TreePane) -> DiffCacheKey {
        DiffCacheKey {
            path: path.to_string(),
            pane,
            tool: self.tool.clone(),
            pane_width: self.diff_pane_width,
            commit_revision: self.commit_revision.clone(),
        }
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
        raw_diff: String,
        display_diff: String,
        file_diff: FileDiff,
        line_infos: Vec<DisplayLineInfo>,
        display_line_count: usize,
        raw_line_count: usize,
        cached_display_text: Option<Text<'static>>,
    ) {
        self.raw_diff = raw_diff;
        self.display_diff = display_diff;
        self.file_diff = file_diff;
        self.line_infos = line_infos;
        self.display_line_count = display_line_count;
        self.raw_line_count = raw_line_count;
        self.cached_display_text = cached_display_text;
        self.current_file = Some(path.to_string());
        self.diff_origin = Some(pane);
        self.diff_scroll = 0;
        self.diff_cursor = 0;
        self.hunk_cursor = 0;
    }

    pub fn load_diff(&mut self, path: &str, pane: TreePane) -> Result<()> {
        let cache_key = self.build_diff_cache_key(path, pane);
        if let Some(cached) = self.get_cached_diff(&cache_key) {
            self.apply_loaded_diff_state(
                path,
                pane,
                cached.raw_diff,
                cached.display_diff,
                cached.file_diff,
                cached.line_infos,
                cached.display_line_count,
                cached.raw_line_count,
                cached.cached_display_text,
            );
            return Ok(());
        }

        let is_untracked = self.has_untracked_file_in_pane(pane, path);
        let mut force_ansi_rendering = false;
        let (raw, display) =
            if is_untracked {
                let preview = crate::git::diff::get_file_preview(path, &self.repo_root)
                    .unwrap_or_else(|_| crate::git::diff::FilePreview {
                        content: String::new(),
                        uses_ansi: false,
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
                let raw = crate::git::diff::get_raw_diff(path, pane.is_staged(), &self.repo_root)
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
        let raw_line_count = raw.lines().count();
        let display_line_count = display.lines().count();
        let cached_display_text = self.build_cached_display_text(&display, force_ansi_rendering);

        self.apply_loaded_diff_state(
            path,
            pane,
            raw.clone(),
            display.clone(),
            file_diff.clone(),
            Vec::new(),
            display_line_count,
            raw_line_count,
            cached_display_text.clone(),
        );
        if is_untracked {
            self.build_preview_line_infos();
        } else {
            self.build_line_infos();
        }

        self.insert_cached_diff(
            cache_key,
            CachedDiff {
                raw_diff: raw,
                display_diff: display,
                file_diff,
                line_infos: self.line_infos.clone(),
                display_line_count: self.display_line_count,
                raw_line_count: self.raw_line_count,
                cached_display_text,
            },
        );

        Ok(())
    }

    fn clear_diff(&mut self) {
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
    }

    fn build_line_infos(&mut self) {
        let mut infos: Vec<DisplayLineInfo> = Vec::new();
        let mut hunk_idx: Option<usize> = None;
        let mut line_in_hunk: usize = 0;
        let mut current_hunk_counter = 0usize;

        for line in self.raw_diff.lines() {
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

        self.line_infos = infos;
    }

    fn build_preview_line_infos(&mut self) {
        self.line_infos = self
            .raw_diff
            .lines()
            .map(|_| DisplayLineInfo {
                hunk_idx: None,
                line_in_hunk: None,
                is_selectable: false,
            })
            .collect();
    }

    /// Reload diff for the current file with the current origin
    fn reload_current_diff(&mut self) -> Result<()> {
        if let (Some(path), Some(pane)) = (self.current_file.clone(), self.diff_origin) {
            let prev_scroll = self.diff_scroll;
            let prev_cursor = self.diff_cursor;
            self.load_diff(&path, pane)?;
            let line_count = self.raw_line_count;
            self.diff_scroll = prev_scroll.min(line_count.saturating_sub(1));
            self.diff_cursor = prev_cursor.min(line_count.saturating_sub(1));
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
            KeyCode::Enter => {
                self.tree_enter()?;
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
            self.load_diff(&path, pane)?;
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

    /// Enter key: stage/unstage file or dir
    fn tree_enter(&mut self) -> Result<()> {
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

        let _ = self.load_diff(&path, pane);
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

    // ─── Diff view key handling ─────────────────────────────────────────

    fn handle_diff_key(&mut self, key: KeyEvent) -> Result<()> {
        let line_count = self.display_line_count;
        let half_page = (self.diff_pane_height / 2).max(1);

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
            KeyCode::Char('n') => self.navigate_search(true),
            KeyCode::Char('N') => self.navigate_search(false),
            KeyCode::Char(']') => self.jump_next_hunk(),
            KeyCode::Char('[') => self.jump_prev_hunk(),
            KeyCode::Char('c') => {
                self.diff_copy_path_to_clipboard();
            }
            KeyCode::Char('h') | KeyCode::Left => {
                self.focus = self
                    .diff_origin
                    .map(|p| p.to_focus())
                    .unwrap_or(Focus::Unstaged);
            }
            KeyCode::Char('v') => {
                if self.is_commit_mode() {
                    self.error_message = Some("Commit diff is read-only".to_string());
                } else if self.tool.supports_line_ops() {
                    if self.file_diff.hunks.is_empty() {
                        self.error_message = Some("No hunks to select lines from".to_string());
                    } else {
                        self.focus = Focus::InlineSelect;
                        self.diff_cursor = self.diff_scroll;
                        self.status_message =
                            Some("Inline select: j/k move  Enter apply  v/h exit".to_string());
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
            KeyCode::Char('/') => {
                self.begin_search();
            }
            KeyCode::Char('n') => self.navigate_search(true),
            KeyCode::Char('N') => self.navigate_search(false),
            KeyCode::Char(']') => self.jump_next_hunk(),
            KeyCode::Char('[') => self.jump_prev_hunk(),
            KeyCode::Enter => {
                self.apply_current_line()?;
            }
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

    let mut map: BTreeMap<String, (bool, char, char)> = BTreeMap::new();

    for (path, staged, unstaged) in files {
        let fp = PathBuf::from(path);

        // Insert ancestor directories
        let mut ancestor = PathBuf::new();
        let components: Vec<_> = fp.components().collect();
        for (i, comp) in components.iter().enumerate() {
            ancestor = ancestor.join(comp);
            if i + 1 < components.len() {
                let key = format!("{}/", ancestor.to_string_lossy());
                map.entry(key).or_insert((true, ' ', ' '));
            }
        }

        map.insert(path.clone(), (false, *staged, *unstaged));
    }

    let mut nodes: Vec<TreeNode> = Vec::new();
    for (key, (is_dir, staged, unstaged)) in &map {
        let path = if *is_dir {
            PathBuf::from(key.trim_end_matches('/'))
        } else {
            PathBuf::from(key)
        };

        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| key.clone());

        let depth = path.components().count().saturating_sub(1);
        let expanded = if *is_dir {
            *prev_expanded.get(&path).unwrap_or(&true)
        } else {
            false
        };

        nodes.push(TreeNode {
            path,
            name,
            depth,
            is_dir: *is_dir,
            expanded,
            staged: *staged,
            unstaged: *unstaged,
        });
    }

    *target_nodes = nodes;
}

fn contains_ignore_case(text: &str, query: &str) -> bool {
    text.to_lowercase().contains(&query.to_lowercase())
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
            diff_pane_height: 20,
            diff_pane_width: 80,
            pending_tree_preview: None,
            tree_preview_debounce: Duration::from_millis(TREE_PREVIEW_DEBOUNCE_MS),
            diff_cache: HashMap::new(),
            diff_cache_order: VecDeque::new(),
            diff_cache_capacity: DIFF_CACHE_CAPACITY,
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
        app.raw_diff = "@@ -1 +1 @@\n-looks like diff\n+but is file content\n".to_string();

        app.build_preview_line_infos();

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
}
