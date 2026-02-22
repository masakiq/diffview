use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::Backend, Terminal};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::Config;
use crate::git::diff::{parse_diff, FileDiff, Hunk};
use crate::git::status::{get_status, GitFile};

// ─── Focus / Mode ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum Focus {
    Tree,
    Diff,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AppMode {
    Normal,
    SelectLines,
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
        matches!(
            (self.staged, self.unstaged),
            ('U', _) | (_, 'U') | ('A', 'A') | ('D', 'D')
        )
    }

    pub fn short_status(&self) -> char {
        if self.staged != ' ' && self.staged != '?' {
            self.staged
        } else {
            self.unstaged
        }
    }
}

// ─── Line mapping for select mode ──────────────────────────────────────────

/// Maps a display-line index in the raw diff to its hunk and line position.
/// Only `+` and `-` lines are selectable; context/header lines have is_selectable=false.
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
    pub mode: AppMode,
    #[allow(dead_code)]
    pub config: Config, // kept for future keybinding customization
    pub tool: DiffTool,
    pub staged_only: bool,
    pub repo_root: PathBuf,

    // Tree state
    pub all_nodes: Vec<TreeNode>,
    pub visible: Vec<usize>,
    pub tree_cursor: usize,

    // Diff state
    pub display_diff: String,     // content shown in diff pane (may have ANSI codes)
    pub raw_diff: String,         // always the plain git diff output
    pub file_diff: FileDiff,
    pub diff_scroll: usize,
    pub diff_cursor: usize,       // current line in select mode
    pub hunk_cursor: usize,
    pub selected_lines: HashSet<usize>, // indices into file_diff.hunks[hunk_cursor].lines
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
        staged_only: bool,
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
            focus: Focus::Tree,
            mode: AppMode::Normal,
            config,
            tool,
            staged_only,
            repo_root,
            all_nodes: Vec::new(),
            visible: Vec::new(),
            tree_cursor: 0,
            display_diff: String::new(),
            raw_diff: String::new(),
            file_diff: FileDiff::default(),
            diff_scroll: 0,
            diff_cursor: 0,
            hunk_cursor: 0,
            selected_lines: HashSet::new(),
            current_file: None,
            line_infos: Vec::new(),
            diff_pane_height: 20,
            diff_pane_width: {
                // Use actual terminal size for the initial diff load
                let w = crossterm::terminal::size().map(|(w, _)| w).unwrap_or(120);
                ((w * 3) / 4).saturating_sub(2)
            },
            status_message: None,
            error_message: None,
        };

        app.refresh_tree()?;

        // Auto-select first file
        if let Some(node) = app.first_file_node() {
            if !node.is_dir && !node.is_untracked() {
                let _ = app.load_diff_for_current();
            }
        }

        Ok(app)
    }

    fn first_file_node(&self) -> Option<&TreeNode> {
        self.visible.iter().find_map(|&idx| {
            let n = &self.all_nodes[idx];
            if !n.is_dir { Some(n) } else { None }
        })
    }

    // ─── Tree building ───────────────────────────────────────────────────

    pub fn refresh_tree(&mut self) -> Result<()> {
        let files = get_status(&self.repo_root)?;
        self.build_tree(files);
        Ok(())
    }

    fn build_tree(&mut self, files: Vec<GitFile>) {
        // Preserve existing expansion states
        let prev_expanded: std::collections::HashMap<PathBuf, bool> = self
            .all_nodes
            .iter()
            .filter(|n| n.is_dir)
            .map(|n| (n.path.clone(), n.expanded))
            .collect();

        // Use BTreeMap so keys are sorted lexicographically.
        // Dir keys get a trailing '/' so they sort before their children.
        // Value: (is_dir, staged, unstaged)
        let mut map: BTreeMap<String, (bool, char, char)> = BTreeMap::new();

        for file in &files {
            let fp = PathBuf::from(&file.path);

            // Insert all ancestor directories
            let mut ancestor = PathBuf::new();
            let components: Vec<_> = fp.components().collect();
            for (i, comp) in components.iter().enumerate() {
                ancestor = ancestor.join(comp);
                if i + 1 < components.len() {
                    // It's a directory ancestor
                    let key = format!("{}/", ancestor.to_string_lossy());
                    map.entry(key).or_insert((true, ' ', ' '));
                }
            }

            // Insert the file itself
            map.insert(file.path.clone(), (false, file.staged, file.unstaged));
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

        self.all_nodes = nodes;
        self.rebuild_visible();
        self.clamp_tree_cursor();
    }

    pub fn rebuild_visible(&mut self) {
        // Build a quick lookup: dir path → expanded
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

    fn clamp_tree_cursor(&mut self) {
        if self.visible.is_empty() {
            self.tree_cursor = 0;
        } else if self.tree_cursor >= self.visible.len() {
            self.tree_cursor = self.visible.len() - 1;
        }
    }

    pub fn current_tree_node(&self) -> Option<&TreeNode> {
        self.visible
            .get(self.tree_cursor)
            .and_then(|&idx| self.all_nodes.get(idx))
    }

    // ─── Diff loading ────────────────────────────────────────────────────

    pub fn load_diff_for_current(&mut self) -> Result<()> {
        let (path, is_untracked, is_dir) = match self.current_tree_node() {
            Some(n) => (
                n.path.to_string_lossy().to_string(),
                n.is_untracked(),
                n.is_dir,
            ),
            None => return Ok(()),
        };

        if is_dir {
            self.clear_diff();
            return Ok(());
        }

        if is_untracked {
            self.display_diff = "(untracked file – press 'a' to stage it)".to_string();
            self.raw_diff = String::new();
            self.file_diff = FileDiff::default();
            self.current_file = Some(path);
            self.diff_scroll = 0;
            return Ok(());
        }

        let raw = crate::git::diff::get_raw_diff(&path, self.staged_only, &self.repo_root)
            .unwrap_or_default();

        let display =
            crate::git::diff::get_display_diff(&path, self.staged_only, self.tool.name(), self.diff_pane_width, &self.repo_root)
                .unwrap_or_else(|_| raw.clone());

        self.raw_diff = raw.clone();
        self.display_diff = display;
        self.file_diff = parse_diff(&raw);
        self.current_file = Some(path);
        self.diff_scroll = 0;
        self.diff_cursor = 0;
        self.hunk_cursor = 0;
        self.selected_lines.clear();
        self.build_line_infos();

        Ok(())
    }

    fn clear_diff(&mut self) {
        self.display_diff.clear();
        self.raw_diff.clear();
        self.file_diff = FileDiff::default();
        self.current_file = None;
        self.diff_scroll = 0;
        self.line_infos.clear();
    }

    /// Build a per-display-line mapping for the raw diff (used in select mode).
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

    // ─── Main event loop ─────────────────────────────────────────────────

    pub fn run<B: Backend>(&mut self, terminal: &mut Terminal<B>) -> Result<()> {
        loop {
            let size = terminal.size()?;
            // Subtract 2 for the left/right borders of the diff pane Block,
            // so delta renders content that fits exactly in the inner area.
            self.diff_pane_width = ((size.width * 3) / 4).saturating_sub(2);
            // statusbar = 1, borders = 2 → inner diff height ≈ size.height - 3
            self.diff_pane_height = size.height.saturating_sub(3) as usize;

            terminal.draw(|f| crate::ui::render(f, self))?;

            if crossterm::event::poll(Duration::from_millis(50))? {
                match crossterm::event::read()? {
                    crossterm::event::Event::Key(key) => self.handle_key(key)?,
                    crossterm::event::Event::Resize(_, _) => {
                        if self.tool == DiffTool::Delta && self.current_file.is_some() {
                            let _ = self.load_diff_for_current();
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

        match self.mode {
            AppMode::Normal => self.handle_key_normal(key)?,
            AppMode::SelectLines => self.handle_key_select(key)?,
        }
        Ok(())
    }

    fn handle_key_normal(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Char('q') => {
                self.should_quit = true;
            }
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Tree => Focus::Diff,
                    Focus::Diff => Focus::Tree,
                };
            }
            KeyCode::Char('?') => {
                self.status_message = Some(
                    "Tab:focus  j/k:move  Enter:open  Space:fold  a:add  r:revert  A:add-all  R:revert-all  v:line-select  n/p:hunk  q:quit"
                        .to_string(),
                );
            }
            _ => match self.focus {
                Focus::Tree => self.handle_tree_key(key)?,
                Focus::Diff => self.handle_diff_key(key)?,
            },
        }
        Ok(())
    }

    fn handle_tree_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                if !self.visible.is_empty() && self.tree_cursor + 1 < self.visible.len() {
                    self.tree_cursor += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.tree_cursor > 0 {
                    self.tree_cursor -= 1;
                }
            }
            KeyCode::Enter => {
                let is_dir = self
                    .current_tree_node()
                    .map(|n| n.is_dir)
                    .unwrap_or(false);
                if is_dir {
                    self.toggle_fold();
                } else {
                    self.load_diff_for_current()?;
                    self.focus = Focus::Diff;
                }
            }
            KeyCode::Char(' ') => {
                self.toggle_fold();
            }
            KeyCode::Char('a') => self.tree_stage()?,
            KeyCode::Char('r') => self.tree_revert()?,
            _ => {}
        }
        Ok(())
    }

    fn toggle_fold(&mut self) {
        if let Some(&idx) = self.visible.get(self.tree_cursor) {
            if self.all_nodes[idx].is_dir {
                self.all_nodes[idx].expanded = !self.all_nodes[idx].expanded;
                self.rebuild_visible();
                self.clamp_tree_cursor();
            }
        }
    }

    fn tree_stage(&mut self) -> Result<()> {
        let (path, _is_dir) = match self.current_tree_node() {
            Some(n) => (n.path.to_string_lossy().to_string(), n.is_dir),
            None => return Ok(()),
        };
        match crate::git::apply::stage_file(&path, &self.repo_root) {
            Ok(_) => {
                self.status_message = Some(format!("Staged: {}", path));
                self.refresh_after_op()?;
            }
            Err(e) => self.error_message = Some(format!("Error: {}", e)),
        }
        Ok(())
    }

    fn tree_revert(&mut self) -> Result<()> {
        let (path, staged) = match self.current_tree_node() {
            Some(n) => (
                n.path.to_string_lossy().to_string(),
                n.staged != ' ' && n.staged != '?',
            ),
            None => return Ok(()),
        };
        match crate::git::apply::unstage_file(&path, staged, &self.repo_root) {
            Ok(_) => {
                self.status_message = Some(format!("Reverted: {}", path));
                self.refresh_after_op()?;
            }
            Err(e) => self.error_message = Some(format!("Error: {}", e)),
        }
        Ok(())
    }

    fn handle_diff_key(&mut self, key: KeyEvent) -> Result<()> {
        let line_count = self.diff_line_count();
        let half_page = (self.diff_pane_height / 2).max(1);

        match key.code {
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
            KeyCode::Char('n') => self.jump_next_hunk(),
            KeyCode::Char('p') => self.jump_prev_hunk(),
            KeyCode::Char('a') => self.hunk_stage()?,
            KeyCode::Char('r') => self.hunk_unstage()?,
            KeyCode::Char('A') => self.file_stage_all()?,
            KeyCode::Char('R') => self.file_revert_all()?,
            KeyCode::Char('v') => {
                if self.tool.supports_line_ops() {
                    if self.file_diff.hunks.is_empty() {
                        self.error_message = Some("No hunks to select lines from".to_string());
                    } else {
                        self.mode = AppMode::SelectLines;
                        self.diff_cursor = self.diff_scroll;
                        self.selected_lines.clear();
                        self.status_message = Some(
                            "Line select: j/k move  Space toggle  a stage  r revert  Esc exit"
                                .to_string(),
                        );
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

    fn diff_line_count(&self) -> usize {
        if self.mode == AppMode::SelectLines {
            self.raw_diff.lines().count()
        } else {
            self.display_diff.lines().count()
        }
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
        for (line_no, line) in self.display_diff.lines().enumerate() {
            if line.starts_with("@@") {
                if hunk_count == hunk_idx {
                    self.diff_scroll = line_no;
                    return;
                }
                hunk_count += 1;
            }
        }
    }

    fn current_hunk(&self) -> Option<&Hunk> {
        self.file_diff.hunks.get(self.hunk_cursor)
    }

    fn hunk_stage(&mut self) -> Result<()> {
        if !self.tool.supports_line_ops() {
            self.error_message = Some("Hunk staging unavailable with difftastic".to_string());
            return Ok(());
        }
        let file = match &self.current_file {
            Some(f) => f.clone(),
            None => return Ok(()),
        };
        let hunk = match self.current_hunk().cloned() {
            Some(h) => h,
            None => return Ok(()),
        };
        match crate::git::apply::stage_hunk(&file, &hunk, &self.repo_root) {
            Ok(_) => {
                self.status_message = Some("Hunk staged".to_string());
                self.refresh_after_op()?;
            }
            Err(e) => self.error_message = Some(format!("Error: {}", e)),
        }
        Ok(())
    }

    fn hunk_unstage(&mut self) -> Result<()> {
        if !self.tool.supports_line_ops() {
            self.error_message = Some("Hunk revert unavailable with difftastic".to_string());
            return Ok(());
        }
        let file = match &self.current_file {
            Some(f) => f.clone(),
            None => return Ok(()),
        };
        let hunk = match self.current_hunk().cloned() {
            Some(h) => h,
            None => return Ok(()),
        };
        match crate::git::apply::unstage_hunk(&file, &hunk, &self.repo_root) {
            Ok(_) => {
                self.status_message = Some("Hunk unstaged".to_string());
                self.refresh_after_op()?;
            }
            Err(e) => self.error_message = Some(format!("Error: {}", e)),
        }
        Ok(())
    }

    fn file_stage_all(&mut self) -> Result<()> {
        let file = match &self.current_file {
            Some(f) => f.clone(),
            None => return Ok(()),
        };
        match crate::git::apply::stage_file(&file, &self.repo_root) {
            Ok(_) => {
                self.status_message = Some(format!("Staged all changes in {}", file));
                self.refresh_after_op()?;
            }
            Err(e) => self.error_message = Some(format!("Error: {}", e)),
        }
        Ok(())
    }

    fn file_revert_all(&mut self) -> Result<()> {
        let file = match &self.current_file {
            Some(f) => f.clone(),
            None => return Ok(()),
        };
        let staged = self
            .current_tree_node()
            .map(|n| n.staged != ' ' && n.staged != '?')
            .unwrap_or(false);
        match crate::git::apply::unstage_file(&file, staged, &self.repo_root) {
            Ok(_) => {
                self.status_message = Some(format!("Reverted all changes in {}", file));
                self.refresh_after_op()?;
            }
            Err(e) => self.error_message = Some(format!("Error: {}", e)),
        }
        Ok(())
    }

    fn refresh_after_op(&mut self) -> Result<()> {
        self.refresh_tree()?;
        if self.current_file.is_some() {
            self.load_diff_for_current()?;
        }
        Ok(())
    }

    // ─── Select-lines mode ───────────────────────────────────────────────

    fn handle_key_select(&mut self, key: KeyEvent) -> Result<()> {
        let line_count = self.raw_diff.lines().count();
        match key.code {
            KeyCode::Esc => {
                self.mode = AppMode::Normal;
                self.selected_lines.clear();
                self.status_message = None;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if self.diff_cursor + 1 < line_count {
                    self.diff_cursor += 1;
                    self.sync_hunk_cursor_to_diff_cursor();
                    if self.diff_cursor >= self.diff_scroll + self.diff_pane_height {
                        self.diff_scroll = self.diff_cursor + 1 - self.diff_pane_height;
                    }
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.diff_cursor > 0 {
                    self.diff_cursor -= 1;
                    self.sync_hunk_cursor_to_diff_cursor();
                    if self.diff_cursor < self.diff_scroll {
                        self.diff_scroll = self.diff_cursor;
                    }
                }
            }
            KeyCode::Char(' ') => {
                // Toggle selection if this display line corresponds to a selectable diff line
                if let Some(info) = self.line_infos.get(self.diff_cursor) {
                    if info.is_selectable {
                        if let Some(li) = info.line_in_hunk {
                            if self.selected_lines.contains(&li) {
                                self.selected_lines.remove(&li);
                            } else {
                                self.selected_lines.insert(li);
                            }
                        }
                    } else {
                        self.status_message =
                            Some("Only +/- lines are selectable".to_string());
                    }
                }
            }
            KeyCode::Char('a') => self.apply_selected_lines(false)?,
            KeyCode::Char('r') => self.apply_selected_lines(true)?,
            _ => {}
        }
        Ok(())
    }

    /// When cursor moves in select mode, keep hunk_cursor in sync.
    /// If the cursor crosses into a different hunk, clear the selection.
    fn sync_hunk_cursor_to_diff_cursor(&mut self) {
        if let Some(info) = self.line_infos.get(self.diff_cursor) {
            if let Some(new_hunk) = info.hunk_idx {
                if new_hunk != self.hunk_cursor {
                    self.hunk_cursor = new_hunk;
                    self.selected_lines.clear();
                }
            }
        }
    }

    fn apply_selected_lines(&mut self, reverse: bool) -> Result<()> {
        if self.selected_lines.is_empty() {
            self.error_message = Some("No lines selected (press Space to select)".to_string());
            return Ok(());
        }

        let file = match &self.current_file {
            Some(f) => f.clone(),
            None => return Ok(()),
        };
        let hunk = match self.current_hunk().cloned() {
            Some(h) => h,
            None => return Ok(()),
        };
        let selected = self.selected_lines.clone();

        let result = if reverse {
            crate::git::apply::unstage_lines(&file, &hunk, &selected, &self.repo_root)
        } else {
            crate::git::apply::stage_lines(&file, &hunk, &selected, &self.repo_root)
        };

        match result {
            Ok(_) => {
                self.status_message = Some("Selected lines applied".to_string());
                self.mode = AppMode::Normal;
                self.selected_lines.clear();
                self.refresh_after_op()?;
            }
            Err(e) => self.error_message = Some(format!("Error: {}", e)),
        }
        Ok(())
    }
}
