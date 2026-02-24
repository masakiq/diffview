use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::Backend, Terminal};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::clipboard;
use crate::config::Config;
use crate::git::diff::{parse_diff, FileDiff};
use crate::git::status::get_status;

// ─── Focus ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum Focus {
    Unstaged,
    Staged,
    DiffView,
    InlineSelect,
}

// ─── TreePane ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
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

#[derive(Debug, Clone, PartialEq)]
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
                if self.staged == '?' { ' ' } else { self.staged }
            }
        }
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

    pub fn is_empty(&self) -> bool {
        self.visible.is_empty()
    }

    pub fn file_count(&self) -> usize {
        self.all_nodes.iter().filter(|n| !n.is_dir).count()
    }

    /// Expand or collapse a directory node at cursor
    fn set_expanded(&mut self, expanded: bool) {
        if let Some(&idx) = self.visible.get(self.cursor) {
            if self.all_nodes[idx].is_dir {
                self.all_nodes[idx].expanded = expanded;
                self.rebuild_visible();
                self.clamp_cursor();
            }
        }
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
}

// ─── Line mapping for inline-select ─────────────────────────────────────

#[derive(Debug, Clone)]
pub struct DisplayLineInfo {
    pub hunk_idx: Option<usize>,
    pub line_in_hunk: Option<usize>,
    pub is_selectable: bool,
}

// ─── App ───────────────────────────────────────────────────────────────────

pub struct App {
    pub should_quit: bool,
    pub focus: Focus,
    #[allow(dead_code)]
    pub config: Config,
    pub tool: DiffTool,
    pub repo_root: PathBuf,

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
    pub diff_pane_height: usize,
    pub diff_pane_width: u16,

    // Status bar
    pub status_message: Option<String>,
    pub error_message: Option<String>,
}

impl App {
    pub fn new(
        tool_override: Option<String>,
        path_override: Option<String>,
    ) -> Result<Self> {
        let repo_root = if let Some(p) = path_override {
            PathBuf::from(p)
        } else {
            crate::git::get_repo_root()?
        };

        let config = Config::load().unwrap_or_default();

        let tool = if let Some(t) = tool_override {
            DiffTool::from_str(&t)
        } else {
            DiffTool::from_str(&config.diff.tool)
        };

        let mut app = App {
            should_quit: false,
            focus: Focus::Unstaged,
            config,
            tool,
            repo_root,
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
            diff_pane_height: 20,
            diff_pane_width: {
                let w = crossterm::terminal::size().map(|(w, _)| w).unwrap_or(120);
                ((w * 3) / 4).saturating_sub(2)
            },
            status_message: None,
            error_message: None,
        };

        app.refresh_trees()?;

        // Auto-focus: if unstaged is empty but staged has items, start in staged
        if app.unstaged.is_empty() && !app.staged.is_empty() {
            app.focus = Focus::Staged;
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
            if !node.is_dir && !node.is_untracked() {
                let path = node.path.to_string_lossy().to_string();
                let _ = self.load_diff(&path, pane);
            }
        }
    }

    // ─── Tree access ────────────────────────────────────────────────────

    pub fn tree(&self, pane: TreePane) -> &TreeSection {
        match pane {
            TreePane::Unstaged => &self.unstaged,
            TreePane::Staged => &self.staged,
        }
    }

    pub fn is_tree_focused(&self, pane: TreePane) -> bool {
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
        match self.focus {
            Focus::Unstaged => Some(TreePane::Unstaged),
            Focus::Staged => Some(TreePane::Staged),
            _ => None,
        }
    }

    // ─── Tree building ───────────────────────────────────────────────────

    pub fn refresh_trees(&mut self) -> Result<()> {
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

    pub fn load_diff(&mut self, path: &str, pane: TreePane) -> Result<()> {
        let raw = crate::git::diff::get_raw_diff(path, pane.is_staged(), &self.repo_root)
            .unwrap_or_default();

        let display = crate::git::diff::get_display_diff(
            path,
            pane.is_staged(),
            self.tool.name(),
            self.diff_pane_width,
            &self.repo_root,
        )
        .unwrap_or_else(|_| raw.clone());

        self.raw_diff = raw.clone();
        self.display_diff = display;
        self.file_diff = parse_diff(&raw);
        self.current_file = Some(path.to_string());
        self.diff_origin = Some(pane);
        self.diff_scroll = 0;
        self.diff_cursor = 0;
        self.hunk_cursor = 0;
        self.build_line_infos();

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
    }

    fn set_untracked_diff_message(&mut self, path: String, pane: TreePane) {
        self.display_diff = "(untracked file – press Enter to stage it)".to_string();
        self.raw_diff = String::new();
        self.file_diff = FileDiff::default();
        self.current_file = Some(path);
        self.diff_origin = Some(pane);
        self.diff_scroll = 0;
        self.diff_cursor = 0;
        self.hunk_cursor = 0;
        self.line_infos.clear();
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

    /// Reload diff for the current file with the current origin
    fn reload_current_diff(&mut self) -> Result<()> {
        if let (Some(path), Some(pane)) = (self.current_file.clone(), self.diff_origin) {
            let prev_scroll = self.diff_scroll;
            let prev_cursor = self.diff_cursor;
            self.load_diff(&path, pane)?;
            let line_count = self.raw_diff.lines().count();
            self.diff_scroll = prev_scroll.min(line_count.saturating_sub(1));
            self.diff_cursor = prev_cursor.min(line_count.saturating_sub(1));
        }
        Ok(())
    }

    fn has_untracked_file_in_pane(&self, pane: TreePane, path: &str) -> bool {
        self.tree(pane).all_nodes.iter().any(|n| {
            !n.is_dir && n.path == Path::new(path) && n.is_untracked()
        })
    }

    fn refresh_latest_state(&mut self) -> Result<()> {
        let prev_focus = self.focus.clone();
        let prev_scroll = self.diff_scroll;
        let prev_cursor = self.diff_cursor;
        let current = self
            .current_file
            .clone()
            .zip(self.diff_origin);

        self.refresh_trees()?;

        // Keep focus unless the current tree became empty.
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

        match self.focus {
            Focus::Unstaged | Focus::Staged => {
                self.tree_load_preview();
            }
            Focus::DiffView | Focus::InlineSelect => {
                if let Some((path, pane)) = current {
                    if self.has_untracked_file_in_pane(pane, &path) {
                        self.set_untracked_diff_message(path, pane);
                    } else {
                        self.reload_current_diff()?;
                        let line_count = self.raw_diff.lines().count();
                        self.diff_scroll = prev_scroll.min(line_count.saturating_sub(1));
                        self.diff_cursor = prev_cursor.min(line_count.saturating_sub(1));
                    }
                } else {
                    self.clear_diff();
                }
            }
        }

        self.status_message = Some("Refreshed latest state".to_string());
        Ok(())
    }

    // ─── Main event loop ─────────────────────────────────────────────────

    pub fn run<B: Backend>(&mut self, terminal: &mut Terminal<B>) -> Result<()> {
        loop {
            let size = terminal.size()?;
            self.diff_pane_width = ((size.width * 3) / 4).saturating_sub(2);
            self.diff_pane_height = size.height.saturating_sub(3) as usize;

            terminal.draw(|f| crate::ui::render(f, self))?;

            if crossterm::event::poll(Duration::from_millis(50))? {
                match crossterm::event::read()? {
                    crossterm::event::Event::Key(key) => self.handle_key(key)?,
                    crossterm::event::Event::Resize(_, _) => {
                        if self.tool == DiffTool::Delta && self.current_file.is_some() {
                            let _ = self.reload_current_diff();
                        }
                    }
                    _ => {}
                }
            }

            if self.should_quit {
                break;
            }
        }
        Ok(())
    }

    // ─── Key handling ────────────────────────────────────────────────────

    fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        self.error_message = None;
        self.status_message = None;

        if key.code == KeyCode::Char('r') {
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
            KeyCode::Char('?') => {
                self.status_message = Some(
                    "j/k:move  l:open  h:back  Enter:stage/unstage  c:copy-path  r:refresh  v:line-select  n/p:hunk  q:quit"
                        .to_string(),
                );
            }
            _ => {}
        }
        Ok(())
    }

    fn tree_move_down(&mut self) {
        let pane = match self.focused_pane() {
            Some(p) => p,
            None => return,
        };

        let can_move = {
            let tree = self.tree(pane);
            !tree.is_empty() && tree.cursor + 1 < tree.visible.len()
        };

        if can_move {
            self.tree_mut(pane).cursor += 1;
        } else if pane == TreePane::Unstaged && !self.staged.is_empty() {
            self.focus = Focus::Staged;
            self.staged.cursor = 0;
        }

        self.tree_load_preview();
    }

    fn tree_move_up(&mut self) {
        let pane = match self.focused_pane() {
            Some(p) => p,
            None => return,
        };

        let can_move = {
            let tree = self.tree(pane);
            tree.cursor > 0
        };

        if can_move {
            self.tree_mut(pane).cursor -= 1;
        } else if pane == TreePane::Staged && !self.unstaged.is_empty() {
            self.focus = Focus::Unstaged;
            self.unstaged.cursor = self.unstaged.visible.len().saturating_sub(1);
        }

        self.tree_load_preview();
    }

    /// l key: expand dir (and move cursor to first child) or open file diff
    fn tree_action_right(&mut self) -> Result<()> {
        let pane = match self.focused_pane() {
            Some(p) => p,
            None => return Ok(()),
        };

        let (is_dir, is_untracked, path) = {
            let section = self.tree(pane);
            match section.current_node() {
                Some(n) => (n.is_dir, n.is_untracked(), n.path.to_string_lossy().to_string()),
                None => return Ok(()),
            }
        };

        if is_dir {
            self.tree_mut(pane).expand_and_enter();
            self.tree_load_preview();
        } else {
            if is_untracked {
                self.set_untracked_diff_message(path, pane);
            } else {
                self.load_diff(&path, pane)?;
            }
            self.focus = Focus::DiffView;
        }
        Ok(())
    }

    /// h key: on dir always close, on file fold parent
    fn tree_action_left(&mut self) {
        let pane = match self.focused_pane() {
            Some(p) => p,
            None => return,
        };

        let is_dir = self
            .tree(pane)
            .current_node()
            .map(|n| n.is_dir)
            .unwrap_or(false);

        if is_dir {
            self.tree_mut(pane).set_expanded(false);
        } else {
            self.tree_mut(pane).fold_parent();
        }
    }

    /// Enter key: stage/unstage file or dir
    fn tree_enter(&mut self) -> Result<()> {
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

        match clipboard::copy_text(&path) {
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

        let (is_dir, is_untracked, path) = {
            let section = self.tree(pane);
            match section.current_node() {
                Some(n) => (n.is_dir, n.is_untracked(), n.path.to_string_lossy().to_string()),
                None => {
                    self.clear_diff();
                    return;
                }
            }
        };

        if is_dir {
            return;
        }

        if is_untracked {
            self.set_untracked_diff_message(path, pane);
        } else {
            let _ = self.load_diff(&path, pane);
        }
    }

    fn refresh_after_tree_op(&mut self) -> Result<()> {
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
        let line_count = self.display_diff.lines().count();
        let half_page = (self.diff_pane_height / 2).max(1);

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
                self.diff_scroll =
                    (self.diff_scroll + half_page).min(line_count.saturating_sub(1));
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
            KeyCode::Char('n') => self.jump_next_hunk(),
            KeyCode::Char('p') => self.jump_prev_hunk(),
            KeyCode::Char('h') | KeyCode::Left => {
                self.focus = self
                    .diff_origin
                    .map(|p| p.to_focus())
                    .unwrap_or(Focus::Unstaged);
            }
            KeyCode::Char('v') => {
                if self.tool.supports_line_ops() {
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
        let line_count = self.raw_diff.lines().count();
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
                self.diff_cursor =
                    (self.diff_cursor + half_page).min(line_count.saturating_sub(1));
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
            KeyCode::Char('n') => self.jump_next_hunk(),
            KeyCode::Char('p') => self.jump_prev_hunk(),
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
                let action = if pane.is_staged() { "Unstaged" } else { "Staged" };
                self.status_message = Some(format!("{} 1 line", action));
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
