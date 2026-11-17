#![allow(dead_code)]

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use crate::agent::{AgentRun, Thread};
use crate::editor::{
    Clipboard, Document, DocumentChange, DocumentId, EditorView, LanguageId, Position, Range,
};
use crate::language::{
    CompletionItem, Diagnostic, LanguageClient, Location, LspCmd, LspStatus, TextEdit,
};
use crate::syntax::SyntaxService;
use crate::workspace::{
    ExplorerEntry, FileBaselines, FileDiffResult, FileFinder, GitDiffResult, GitPathStatus,
    GitWrite, GitWriteResult, LineChanges, ParsedGitDiff, SearchMatch,
};

use super::config::Config;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Editor,
    Explorer,
    Thread,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptKind {
    CreateFile { is_dir: bool },
    RenameFile,
    RenameSymbol,
    SaveAs,
    AgentEdit,
}

#[derive(Clone, Debug)]
pub enum ConfirmAction {
    DeletePath(PathBuf),
    Quit,
    Reload(DocumentId),
    Close(DocumentId),
    CloseMany(Vec<DocumentId>),
    DiscardHunk { patch: String, path: PathBuf },
    RestoreFile(PathBuf),
    DiscardAdded(PathBuf),
}

#[derive(Clone, Debug)]
pub struct MenuItem {
    pub label: String,
    pub action: MenuAction,
    pub enabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuAction {
    GoToDefinition,
    FindReferences,
    Hover,
    RenameSymbol,
    Copy,
    Cut,
    Paste,
    NewFile,
    NewFolder,
    RenameFile,
    CopyPath,
    DeletePath,
    CloseOthers(DocumentId),
    CloseAll,
    AskClaude,
    DiscardFile,
    RestoreFile,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchKind {
    Find,
    Content,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchScope {
    File,
    Global,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderAction {
    Find,
    Search,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeaderHit {
    pub action: HeaderAction,
    pub x: u16,
    pub width: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScopeHit {
    pub scope: SearchScope,
    pub x: u16,
    pub y: u16,
    pub width: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SideTabHit {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub changes: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContentTabHit {
    pub x: u16,
    pub y: u16,
    pub width: u16,
}

#[derive(Clone, Debug)]
pub enum Overlay {
    None,
    Search {
        kind: SearchKind,
        scope: SearchScope,
        query: String,
        files: Vec<String>,
        matches: Vec<SearchMatch>,
        file_matches: Vec<(Position, Position)>,
        selected: usize,
        scroll: usize,
        searching: bool,
    },
    Completion {
        items: Vec<CompletionItem>,
        selected: usize,
        origin: Position,
    },
    Hover {
        title: String,
        text: String,
        origin: Position,
        x: u16,
        y: u16,
    },
    Signature {
        text: String,
    },
    ContextMenu {
        items: Vec<MenuItem>,
        selected: usize,
        x: u16,
        y: u16,
        show_type: bool,
        type_text: String,
        hover_token: u64,
    },
    Confirm {
        message: String,
        action: ConfirmAction,
    },
    Prompt {
        title: String,
        input: String,
        cursor: usize,
        kind: PromptKind,
    },
    References {
        items: Vec<Location>,
        selected: usize,
    },
    Themes {
        selected: usize,
    },
    Help,
    TypesNote,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusAction {
    Theme,
    Help,
    Agent,
    Types,
    Markdown,
    Git,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HunkAction {
    Stage,
    Unstage,
    Discard,
    Revert,
    Fold,
}

#[derive(Clone, Debug)]
pub enum GitClick {
    Message,
    Body,
    BodyFold,
    Pull,
    Push,
    Commit,
    StageAll,
    UnstageAll,
    Toggle { path: PathBuf, staged: bool },
    Open { path: PathBuf, staged: bool },
}

#[derive(Clone, Debug)]
pub struct ThreadHit {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub path: PathBuf,
}

#[derive(Clone, Debug)]
pub struct PendingRename {
    pub token: u64,
    pub from: PathBuf,
    pub to: PathBuf,
}

#[derive(Clone, Debug)]
pub struct GitHit {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub action: GitClick,
}

#[derive(Clone, Copy, Debug)]
pub struct DiffHit {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub anchor: usize,
    pub action: HunkAction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatusHit {
    pub action: StatusAction,
    pub x: u16,
    pub width: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HScrollHit {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub thumb_x: u16,
    pub thumb_w: u16,
    pub max_left: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabHitBox {
    pub document_id: DocumentId,
    pub x: u16,
    pub width: u16,
    pub close_x: u16,
}

#[derive(Clone, Debug)]
pub struct ReviewPatches {
    pub unstaged: Option<ParsedGitDiff>,
    pub staged: Option<ParsedGitDiff>,
    pub renamed: bool,
    token: u64,
}

pub const HOVER_DWELL: Duration = Duration::from_millis(400);
const GIT_STATUS_DEBOUNCE: Duration = Duration::from_secs(1);

#[derive(Clone, Debug)]
pub struct HoverDwell {
    pub document_id: DocumentId,
    pub position: Position,
    pub screen_x: u16,
    pub screen_y: u16,
    pub since: Instant,
}

pub struct App {
    pub root: PathBuf,
    pub config: Config,
    pub documents: HashMap<DocumentId, Document>,
    pub views: HashMap<DocumentId, EditorView>,
    pub order: Vec<DocumentId>,
    pub active: Option<DocumentId>,
    pub next_id: u64,
    pub focus: Focus,
    pub overlay: Overlay,
    pub explorer_visible: bool,
    pub explorer_entries: Vec<ExplorerEntry>,
    pub explorer_selected: usize,
    pub explorer_scroll: usize,
    pub expanded: HashSet<PathBuf>,
    pub clipboard: Clipboard,
    pub syntax: SyntaxService,
    pub lsp: LanguageClient,
    pub lsp_status: LspStatus,
    pub diagnostics: HashMap<PathBuf, Vec<Diagnostic>>,
    pub file_finder: FileFinder,
    pub workspace_files: Vec<String>,
    pub message: Option<String>,
    pub should_quit: bool,
    pub editor_area: (u16, u16, u16, u16),
    pub search_area: (u16, u16, u16, u16),
    pub explorer_area: (u16, u16, u16, u16),
    pub tabs_area: (u16, u16, u16, u16),
    pub status_area: (u16, u16, u16, u16),
    pub status_hits: Vec<StatusHit>,
    pub header_hits: Vec<HeaderHit>,
    pub search_scope_hits: Vec<ScopeHit>,
    pub search_rows: Vec<(usize, u16)>,
    pub search_row_span: Option<(u16, u16)>,
    pub overlay_area: Option<(u16, u16, u16, u16)>,
    pub search_list_rows: usize,
    pub overlay_rows: Vec<(usize, u16)>,
    pub prompt_caret: Option<(u16, u16, usize)>,
    pub tab_hit_boxes: Vec<TabHitBox>,
    pub tab_scroll: usize,
    pub tab_revealed: Option<DocumentId>,
    pub tab_scroll_left: Option<(u16, u16)>,
    pub tab_scroll_right: Option<(u16, u16)>,
    pub pane_mark: Option<(u16, u16, u16)>,
    pub types_copy: Option<(u16, u16, u16)>,
    pub last_click: Option<(Instant, u16, u16)>,
    pub dragging: bool,
    pub hscroll_hit: Option<HScrollHit>,
    pub hscroll_dragging: bool,
    pub explorer_resizing: bool,
    pub hscroll_grab: u16,
    pub lsp_token: u64,
    pub completion_token: u64,
    pub hover_token: u64,
    pub hover_dwell: Option<HoverDwell>,
    pub hover_anchor: Option<(u64, u16, u16, Position)>,
    pub search_generation: u64,
    pub search_tx: mpsc::UnboundedSender<crate::workspace::WorkspaceSearch>,
    pub git_status_tx: mpsc::UnboundedSender<crate::workspace::GitStatusUpdate>,
    pub diff_tx: mpsc::UnboundedSender<GitDiffResult>,
    pub git_status: HashMap<PathBuf, GitPathStatus>,
    git_status_running: bool,
    git_status_again: bool,
    git_status_at: Option<Instant>,
    pub diff_baselines: HashMap<DocumentId, FileBaselines>,
    pub review_patches: HashMap<DocumentId, ReviewPatches>,
    pub reveal_change: Option<DocumentId>,
    pub markdown_preview: Option<crate::markdown::Preview>,
    pub gutter_width: u16,
    pub menu_area: Option<(u16, u16, u16, u16)>,
    pub menu_rows: Vec<(usize, u16)>,
    pub inline_changes: HashMap<DocumentId, LineChanges>,
    pub folded_hunks: HashSet<(DocumentId, usize)>,
    pub thread: Thread,
    pub thread_visible: bool,
    pub thread_area: (u16, u16, u16, u16),
    pub thread_stop: Option<(u16, u16, u16)>,
    pub thread_hits: Vec<ThreadHit>,
    pub absent: HashSet<DocumentId>,
    pub restore_hit: Option<(u16, u16, u16)>,
    turn_status_before: HashMap<PathBuf, GitPathStatus>,
    turn_status_wait: bool,
    turn_status_skip: bool,
    pub pending_rename: Option<PendingRename>,
    pub claude_installed: bool,
    pub agent_tx: mpsc::UnboundedSender<crate::agent::AgentEvent>,
    pub agent_run: Option<AgentRun>,
    pub git_branch: Option<String>,
    pub git_ahead_behind: Option<(u32, u32)>,
    pub git_publish_armed: bool,
    pub git_repo: bool,
    pub git_open: bool,
    pub git_selected: usize,
    pub git_scroll: usize,
    pub git_message: String,
    pub git_message_focus: bool,
    pub git_body: String,
    pub git_body_focus: bool,
    pub git_body_open: bool,
    pub git_hits: Vec<GitHit>,
    pub side_tabs: Vec<SideTabHit>,
    pub content_tabs: Vec<ContentTabHit>,
    pub diff_hits: Vec<DiffHit>,
    pub git_write_tx: mpsc::UnboundedSender<GitWriteResult>,
    git_write_running: bool,
    git_write_queue: VecDeque<GitWrite>,
    pub file_diff_tx: mpsc::UnboundedSender<FileDiffResult>,
    file_diff_token: u64,
}

impl App {
    pub fn new(root: PathBuf, lsp: LanguageClient) -> Self {
        let config = Config::load();
        let mut expanded = HashSet::new();
        expanded.insert(root.clone());
        let (search_tx, _search_rx) = mpsc::unbounded_channel();
        drop(_search_rx);
        let (git_status_tx, git_status_rx) = mpsc::unbounded_channel();
        drop(git_status_rx);
        let (diff_tx, diff_rx) = mpsc::unbounded_channel();
        drop(diff_rx);
        let (git_write_tx, git_write_rx) = mpsc::unbounded_channel();
        drop(git_write_rx);
        let (file_diff_tx, file_diff_rx) = mpsc::unbounded_channel();
        drop(file_diff_rx);
        let (agent_tx, agent_rx) = mpsc::unbounded_channel();
        drop(agent_rx);
        let mut app = Self {
            explorer_visible: config.explorer_visible,
            explorer_entries: Vec::new(),
            explorer_selected: 0,
            explorer_scroll: 0,
            expanded,
            clipboard: Clipboard::new(),
            syntax: SyntaxService::new(),
            lsp,
            lsp_status: LspStatus::Starting,
            diagnostics: HashMap::new(),
            file_finder: FileFinder::new(),
            workspace_files: crate::workspace::list_workspace_files(&root),
            message: None,
            should_quit: false,
            editor_area: (0, 0, 0, 0),
            search_area: (0, 0, 0, 0),
            explorer_area: (0, 0, 0, 0),
            tabs_area: (0, 0, 0, 0),
            status_area: (0, 0, 0, 0),
            status_hits: Vec::new(),
            header_hits: Vec::new(),
            search_scope_hits: Vec::new(),
            search_rows: Vec::new(),
            search_row_span: None,
            overlay_area: None,
            search_list_rows: 0,
            overlay_rows: Vec::new(),
            prompt_caret: None,
            tab_hit_boxes: Vec::new(),
            tab_scroll: 0,
            tab_revealed: None,
            tab_scroll_left: None,
            tab_scroll_right: None,
            pane_mark: None,
            types_copy: None,
            last_click: None,
            dragging: false,
            hscroll_hit: None,
            hscroll_dragging: false,
            explorer_resizing: false,
            hscroll_grab: 0,
            lsp_token: 0,
            completion_token: 0,
            hover_token: 0,
            hover_dwell: None,
            hover_anchor: None,
            search_generation: 0,
            search_tx,
            git_status_tx,
            diff_tx,
            git_status: HashMap::new(),
            git_status_running: false,
            git_status_again: false,
            git_status_at: None,
            diff_baselines: HashMap::new(),
            review_patches: HashMap::new(),
            reveal_change: None,
            markdown_preview: None,
            gutter_width: 4,
            menu_area: None,
            menu_rows: Vec::new(),
            inline_changes: HashMap::new(),
            folded_hunks: HashSet::new(),
            thread: Thread::new(),
            thread_visible: false,
            thread_area: (0, 0, 0, 0),
            thread_stop: None,
            thread_hits: Vec::new(),
            absent: HashSet::new(),
            restore_hit: None,
            turn_status_before: HashMap::new(),
            turn_status_wait: false,
            turn_status_skip: false,
            pending_rename: None,
            claude_installed: which::which("claude").is_ok(),
            agent_tx,
            agent_run: None,
            git_branch: None,
            git_ahead_behind: None,
            git_publish_armed: false,
            git_repo: false,
            git_open: false,
            git_selected: 0,
            git_scroll: 0,
            git_message: String::new(),
            git_message_focus: false,
            git_body: String::new(),
            git_body_focus: false,
            git_body_open: false,
            git_hits: Vec::new(),
            side_tabs: Vec::new(),
            content_tabs: Vec::new(),
            diff_hits: Vec::new(),
            git_write_tx,
            git_write_running: false,
            git_write_queue: VecDeque::new(),
            file_diff_tx,
            file_diff_token: 0,
            documents: HashMap::new(),
            views: HashMap::new(),
            order: Vec::new(),
            active: None,
            next_id: 1,
            focus: Focus::Editor,
            overlay: Overlay::None,
            root,
            config,
        };
        app.file_finder.set_files(&app.workspace_files);
        app.refresh_explorer();
        let _ = app.lsp.tx.send(LspCmd::Initialize {
            root: app.root.clone(),
        });
        app
    }

    pub fn refresh_explorer(&mut self) {
        self.explorer_entries =
            crate::workspace::load_directory(&self.root, &self.root, &self.expanded);
        if self.explorer_selected >= self.explorer_entries.len() {
            self.explorer_selected = self.explorer_entries.len().saturating_sub(1);
        }
        let inner = explorer_inner_rows(self.explorer_area.3);
        self.explorer_scroll =
            clamp_explorer_scroll(self.explorer_scroll, self.explorer_entries.len(), inner);
    }

    pub fn refresh_git_status(&mut self) {
        self.git_status_at = Some(Instant::now());
        self.pump_git_status();
    }

    pub fn schedule_git_status(&mut self) {
        self.git_status_at = Some(Instant::now() + GIT_STATUS_DEBOUNCE);
    }

    pub fn git_status_delay(&self) -> Option<Duration> {
        self.git_status_at
            .map(|at| at.saturating_duration_since(Instant::now()))
    }

    pub fn pump_git_status(&mut self) {
        let due = self.git_status_at.is_some_and(|at| at <= Instant::now());
        if !due {
            return;
        }
        if self.git_status_running {
            self.git_status_again = true;
            self.git_status_at = None;
            return;
        }
        self.git_status_at = None;
        self.git_status_running = true;
        crate::workspace::spawn_git_status(self.root.clone(), self.git_status_tx.clone());
    }

    pub fn begin_turn(&mut self) {
        self.thread.touched.clear();
        self.turn_status_before = self.git_status.clone();
        self.turn_status_wait = false;
        self.turn_status_skip = false;
    }

    pub fn note_turn_path(&mut self, raw: &str) {
        if let Some(path) = crate::workspace::project_path(&self.root, raw) {
            crate::workspace::remember_path(&mut self.thread.touched, path);
        }
    }

    pub fn finish_turn_status(&mut self) {
        self.turn_status_skip = self.git_status_running;
        self.turn_status_wait = true;
        self.refresh_git_status();
    }

    fn absorb_turn_status(&mut self, files: &HashMap<PathBuf, GitPathStatus>) {
        if !self.turn_status_wait {
            return;
        }
        if self.turn_status_skip {
            self.turn_status_skip = false;
            return;
        }
        for path in crate::workspace::status_delta(&self.turn_status_before, files) {
            crate::workspace::remember_path(&mut self.thread.touched, path);
        }
        self.turn_status_wait = false;
        self.open_turn_files();
    }

    pub fn receive_git_status(&mut self, result: crate::workspace::GitStatusUpdate) {
        self.git_status_running = false;
        match result {
            Ok(snapshot) => {
                self.git_repo = true;
                if self.git_status != snapshot.files {
                    self.git_status = snapshot.files;
                }
                self.absorb_turn_status(&self.git_status.clone());
                if self.git_branch != snapshot.branch {
                    self.git_publish_armed = false;
                }
                self.git_branch = snapshot.branch;
                self.git_ahead_behind = snapshot.ahead_behind;
                if self.git_ahead_behind.is_some() {
                    self.git_publish_armed = false;
                }
                let open: Vec<_> = self.order.clone();
                for id in open {
                    self.request_inline_diff(id);
                    self.request_review_patches(id);
                }
            }
            Err(crate::workspace::GitStatusError::NotARepository) => {
                self.absorb_turn_status(&HashMap::new());
                self.hide_git();
            }
            Err(crate::workspace::GitStatusError::Unavailable) => {
                self.absorb_turn_status(&self.git_status.clone());
            }
        }
        if self.git_status_again {
            self.git_status_again = false;
            if self.git_status_at.is_none() {
                self.git_status_at = Some(Instant::now());
            }
            self.pump_git_status();
        }
        let rows = crate::workspace::git_panel_rows(&self.git_status);
        if !rows
            .get(self.git_selected)
            .is_some_and(|row| matches!(row, crate::workspace::GitPanelRow::File { .. }))
        {
            self.git_selected = rows
                .iter()
                .position(|row| matches!(row, crate::workspace::GitPanelRow::File { .. }))
                .unwrap_or(0);
        }
    }

    pub fn toggle_git(&mut self) {
        if self.git_repo {
            self.show_changes(!self.git_open);
        }
    }

    pub fn show_changes(&mut self, changes: bool) {
        if changes && !self.git_repo {
            return;
        }
        self.git_message_focus = false;
        self.git_body_focus = false;
        self.git_open = changes;
        self.explorer_visible = true;
        self.focus = Focus::Explorer;
    }

    pub fn file_content_showing(&self) -> bool {
        self.active.is_some() && !self.markdown_preview_open()
    }

    fn hide_git(&mut self) {
        self.git_repo = false;
        self.git_open = false;
        self.git_message_focus = false;
        self.git_body_focus = false;
        self.git_branch = None;
        self.git_ahead_behind = None;
        self.git_publish_armed = false;
        self.git_status.clear();
        self.git_selected = 0;
        self.inline_changes.clear();
        self.diff_baselines.clear();
        self.review_patches.clear();
        self.folded_hunks.clear();
        self.reveal_change = None;
    }

    pub fn buffer_dirty(&self, path: &Path) -> bool {
        self.documents
            .values()
            .any(|doc| doc.is_dirty() && doc.path.as_deref() == Some(path))
    }

    fn dirty_change(&self) -> bool {
        self.git_status.keys().any(|path| self.buffer_dirty(path))
    }

    pub fn git_busy(&self) -> bool {
        self.git_write_running
    }

    pub fn enqueue_git_write(&mut self, write: GitWrite) {
        if !matches!(write, GitWrite::Publish(_)) {
            self.git_publish_armed = false;
        }
        if let Err(message) = crate::workspace::prepare_git_write(&self.root, &write) {
            self.message = Some(message);
            return;
        }
        if self.git_write_running {
            self.git_write_queue.push_back(write);
            return;
        }
        self.start_git_write(write);
    }

    fn start_git_write(&mut self, write: GitWrite) {
        self.git_write_running = true;
        crate::workspace::spawn_git_write(self.root.clone(), write, self.git_write_tx.clone());
    }

    pub fn note_git_write(&mut self, result: GitWriteResult) {
        self.git_write_running = false;
        if result.ok {
            if let Some(message) = &result.committed {
                let sent_body = result.committed_body.as_deref().unwrap_or("");
                if self.git_message.trim() == message && self.git_body.trim() == sent_body {
                    self.git_message.clear();
                    self.git_body.clear();
                    self.git_message_focus = false;
                    self.git_body_focus = false;
                    self.git_body_open = false;
                }
                self.message = Some("Committed".into());
            } else if let Some(notice) = result.notice {
                self.message = Some(notice);
            }
        } else if !result.message.is_empty() {
            self.message = Some(result.message);
        }
        if let Some(next) = self.git_write_queue.pop_front() {
            self.start_git_write(next);
        }
    }

    pub fn stage_path(&mut self, path: PathBuf) {
        self.git_publish_armed = false;
        if self.buffer_dirty(&path) {
            self.message = Some("Save first".into());
            return;
        }
        self.enqueue_git_write(GitWrite::Stage(path));
    }

    pub fn unstage_path(&mut self, path: PathBuf) {
        self.git_publish_armed = false;
        if self.buffer_dirty(&path) {
            self.message = Some("Save first".into());
            return;
        }
        self.enqueue_git_write(GitWrite::Unstage(path));
    }

    pub fn toggle_stage(&mut self, path: PathBuf, staged: bool) {
        if staged {
            self.unstage_path(path);
        } else {
            self.stage_path(path);
        }
    }

    pub fn stage_all(&mut self) {
        self.git_publish_armed = false;
        if self.dirty_change() {
            self.message = Some("Save first".into());
            return;
        }
        if !self
            .git_status
            .values()
            .any(|status| status.unstaged.is_some())
        {
            self.message = Some("No changes to stage".into());
            return;
        }
        self.enqueue_git_write(GitWrite::StageAll);
    }

    pub fn unstage_all(&mut self) {
        self.git_publish_armed = false;
        if self.dirty_change() {
            self.message = Some("Save first".into());
            return;
        }
        if !self
            .git_status
            .values()
            .any(|status| status.staged.is_some())
        {
            self.message = Some("Nothing staged".into());
            return;
        }
        self.enqueue_git_write(GitWrite::UnstageAll);
    }

    pub fn commit_git(&mut self) {
        self.git_publish_armed = false;
        if !crate::workspace::can_commit(&self.git_message, &self.git_status) {
            self.message = Some(if self.git_message.trim().is_empty() {
                "Write a commit message".into()
            } else {
                "Nothing staged".into()
            });
            return;
        }
        self.enqueue_git_write(GitWrite::Commit {
            message: self.git_message.clone(),
            body: self.git_body.clone(),
        });
    }

    pub fn pull_git(&mut self) {
        if !self.git_repo {
            return;
        }
        self.git_publish_armed = false;
        self.enqueue_git_write(GitWrite::Pull);
    }

    pub fn push_git(&mut self) {
        if !self.git_repo {
            return;
        }
        if self.git_ahead_behind.is_none() {
            if !self.git_publish_armed {
                self.git_publish_armed = true;
                self.message = Some("No upstream. Click Push again to publish.".into());
                return;
            }
            self.git_publish_armed = false;
            let Some(branch) = self.git_branch.clone().filter(|branch| !branch.is_empty()) else {
                self.message = Some("No branch to publish".into());
                return;
            };
            self.enqueue_git_write(GitWrite::Publish(branch));
            return;
        }
        self.git_publish_armed = false;
        self.enqueue_git_write(GitWrite::Push);
    }

    pub fn request_hunk(&mut self, anchor: usize, action: HunkAction) {
        let Some(id) = self.active else {
            return;
        };
        if action == HunkAction::Fold {
            self.toggle_hunk_fold(id, anchor);
            return;
        }
        if action == HunkAction::Revert {
            self.revert_hunk_at_line(id, anchor);
            return;
        }
        let Some(path) = self.documents.get(&id).and_then(|doc| doc.path.clone()) else {
            self.message = Some("Git diff requires a saved file".into());
            return;
        };
        if self.buffer_dirty(&path) {
            self.git_publish_armed = false;
            self.message = Some("Save first".into());
            return;
        }
        let Some(patches) = self.review_patches.get(&id).cloned() else {
            self.git_publish_armed = false;
            self.message = Some("No hunk to apply".into());
            return;
        };
        if patches.renamed {
            self.git_publish_armed = false;
            self.message = Some("Stage the renamed file from Changes".into());
            return;
        }
        let Some(hunk) = self
            .present_hunks(id)
            .into_iter()
            .find(|hunk| hunk.start == anchor)
        else {
            self.git_publish_armed = false;
            return;
        };
        let patch = match action {
            HunkAction::Stage | HunkAction::Discard => patches
                .unstaged
                .as_ref()
                .and_then(|diff| crate::workspace::patch_touching(diff, hunk.start, hunk.end)),
            HunkAction::Unstage => {
                let index_anchor = hunk.index_anchor.unwrap_or(hunk.start);
                let (start, end) =
                    Self::unstage_range(self.inline_changes.get(&id), index_anchor, &hunk);
                patches
                    .staged
                    .as_ref()
                    .and_then(|diff| crate::workspace::patch_touching(diff, start, end))
            }
            HunkAction::Revert | HunkAction::Fold => None,
        };
        let Some(patch) = patch else {
            self.git_publish_armed = false;
            self.message = Some("No hunk to apply".into());
            return;
        };
        self.git_publish_armed = false;
        match action {
            HunkAction::Stage => self.enqueue_git_write(GitWrite::Apply {
                patch,
                cached: true,
                reverse: false,
                reload: None,
            }),
            HunkAction::Unstage => self.enqueue_git_write(GitWrite::Apply {
                patch,
                cached: true,
                reverse: true,
                reload: None,
            }),
            HunkAction::Discard => {
                self.overlay = Overlay::Confirm {
                    message: "Discard this hunk?".into(),
                    action: ConfirmAction::DiscardHunk { patch, path },
                };
            }
            HunkAction::Revert | HunkAction::Fold => {}
        }
    }

    pub fn toggle_hunk_fold(&mut self, document_id: DocumentId, anchor: usize) {
        let key = (document_id, anchor);
        if !self.folded_hunks.insert(key) {
            self.folded_hunks.remove(&key);
        }
    }

    pub fn discard_hunk(&mut self, patch: String, path: PathBuf) {
        self.git_publish_armed = false;
        if self.buffer_dirty(&path) {
            self.message = Some("Save first".into());
            return;
        }
        self.enqueue_git_write(GitWrite::Apply {
            patch,
            cached: false,
            reverse: true,
            reload: Some(path),
        });
    }

    fn unstage_range(
        changes: Option<&LineChanges>,
        index_anchor: usize,
        hunk: &crate::workspace::Hunk,
    ) -> (usize, usize) {
        if hunk.end == hunk.start {
            return (index_anchor, index_anchor);
        }
        let Some(changes) = changes else {
            return (index_anchor, index_anchor.saturating_add(1));
        };
        let mut min = index_anchor;
        let mut max = index_anchor;
        for line in hunk.start..hunk.end {
            if let Some(index) = changes.index_of.get(line).copied().flatten() {
                min = min.min(index);
                max = max.max(index);
            }
        }
        (min, max.saturating_add(1))
    }

    pub fn open_git_entry(&mut self, path: PathBuf, _staged: bool) {
        self.git_message_focus = false;
        self.git_body_focus = false;
        if path.is_file() {
            if let Some(id) = self.document_id(&path) {
                self.absent.remove(&id);
            }
            self.open_path(path.clone());
        } else if self.path_deleted(&path) {
            self.open_deleted(path.clone());
        } else {
            self.message = Some("File is missing".into());
            return;
        }
        self.markdown_preview = None;
        let Some(id) = self.document_id(&path) else {
            return;
        };
        if self.absent.contains(&id) {
            return;
        }
        self.reveal_change = Some(id);
        self.request_inline_diff(id);
        self.request_review_patches(id);
        self.scroll_to_first_change(id);
    }

    fn document_id(&self, path: &Path) -> Option<DocumentId> {
        self.documents
            .iter()
            .find_map(|(id, doc)| (doc.path.as_deref() == Some(path)).then_some(*id))
    }

    fn path_deleted(&self, path: &Path) -> bool {
        self.git_status.get(path).is_some_and(|status| {
            status.staged == Some(crate::workspace::GitFileStatus::Deleted)
                || status.unstaged == Some(crate::workspace::GitFileStatus::Deleted)
        })
    }

    pub fn open_deleted(&mut self, path: PathBuf) {
        if let Some(id) = self.document_id(&path) {
            let dirty = self.documents.get(&id).is_some_and(|doc| doc.is_dirty());
            self.active = Some(id);
            self.focus = Focus::Editor;
            self.markdown_preview = None;
            if !dirty {
                self.mark_absent(id);
            }
            return;
        }
        let doc = Document::new(DocumentId(self.next_id), Some(path.clone()), String::new());
        self.next_id += 1;
        let id = doc.id;
        self.syntax.open(id, doc.language, &doc.rope);
        let _ = self.lsp.tx.send(LspCmd::DidOpen {
            path,
            language: doc.language,
            text: doc.text(),
            version: doc.revision as i64,
        });
        self.views.insert(id, EditorView::new(id));
        self.order.push(id);
        self.documents.insert(id, doc);
        self.active = Some(id);
        self.focus = Focus::Editor;
        self.overlay = Overlay::None;
        self.mark_absent(id);
    }

    fn mark_absent(&mut self, id: DocumentId) {
        self.absent.insert(id);
        self.inline_changes.remove(&id);
        self.review_patches.remove(&id);
        self.folded_hunks.retain(|(doc, _)| *doc != id);
        self.request_inline_diff(id);
    }

    pub fn open_turn_files(&mut self) {
        let paths = self.thread.touched.clone();
        let mut opened = Vec::new();
        for path in paths {
            if let Some(id) = self.document_id(&path) {
                if !path.is_file() && self.path_deleted(&path) {
                    let dirty = self.documents.get(&id).is_some_and(|doc| doc.is_dirty());
                    if !dirty {
                        self.mark_absent(id);
                    }
                }
                continue;
            }
            if path.is_file() {
                self.open_path(path);
                if let Some(id) = self.active {
                    opened.push(id);
                }
            } else if self.path_deleted(&path) {
                self.open_deleted(path);
                if let Some(id) = self.active {
                    opened.push(id);
                }
            }
        }
        let Some(id) = opened.first().copied() else {
            return;
        };
        self.active = Some(id);
        self.focus = Focus::Editor;
        if self.absent.contains(&id) {
            return;
        }
        self.reveal_change = Some(id);
        self.scroll_to_first_change(id);
    }

    pub fn refresh_open_reviews(&mut self) {
        let open = self.order.clone();
        for id in open {
            self.request_review_patches(id);
        }
    }

    pub fn receive_file_diff(&mut self, result: FileDiffResult) {
        let Some(id) = self
            .documents
            .iter()
            .find_map(|(id, doc)| (doc.path.as_ref() == Some(&result.path)).then_some(*id))
        else {
            return;
        };
        let Some(patches) = self.review_patches.get_mut(&id) else {
            return;
        };
        if patches.token != result.token || patches.renamed {
            return;
        }
        if result.staged {
            patches.staged = result.parsed;
        } else {
            patches.unstaged = result.parsed;
        }
    }

    pub fn request_review_patches(&mut self, document_id: DocumentId) {
        if self.absent.contains(&document_id) {
            self.review_patches.remove(&document_id);
            return;
        }
        if !self.git_repo {
            return;
        }
        let Some(path) = self
            .documents
            .get(&document_id)
            .and_then(|doc| doc.path.clone())
        else {
            return;
        };
        use crate::workspace::GitFileStatus;
        self.file_diff_token = self.file_diff_token.saturating_add(1);
        let token = self.file_diff_token;
        let status = self.git_status.get(&path).copied();
        let renamed = status.is_some_and(|item| {
            item.staged == Some(GitFileStatus::Renamed)
                || item.unstaged == Some(GitFileStatus::Renamed)
        });
        let mut patches = ReviewPatches {
            unstaged: None,
            staged: None,
            renamed,
            token,
        };
        if renamed {
            self.review_patches.insert(document_id, patches);
            return;
        }
        if status.and_then(|item| item.unstaged) == Some(GitFileStatus::Untracked) {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let relative = crate::workspace::repo_relative(&self.root, &path).unwrap_or_default();
            patches.unstaged = crate::workspace::untracked_diff(&relative, &text).1;
        } else if status.and_then(|item| item.unstaged).is_some() {
            crate::workspace::spawn_file_diff(
                self.root.clone(),
                path.clone(),
                false,
                true,
                token,
                self.file_diff_tx.clone(),
            );
        }
        if status.is_some_and(|item| {
            item.staged.is_some_and(|kind| {
                kind != GitFileStatus::Untracked && kind != GitFileStatus::Renamed
            })
        }) {
            crate::workspace::spawn_file_diff(
                self.root.clone(),
                path,
                true,
                true,
                token,
                self.file_diff_tx.clone(),
            );
        }
        self.review_patches.insert(document_id, patches);
    }

    pub fn reload_clean_document(&mut self, path: &Path) {
        let Some(id) = self.documents.iter().find_map(|(id, doc)| {
            (doc.path.as_deref() == Some(path) && !doc.is_dirty()).then_some(*id)
        }) else {
            return;
        };
        self.absent.remove(&id);
        let Some(doc) = self.documents.get_mut(&id) else {
            return;
        };
        if doc.replace_from_disk().is_err() {
            if path.is_file() {
                return;
            }
            doc.note_removed();
        }
        let language = doc.language;
        let rope = doc.rope.clone();
        let revision = doc.revision;
        let full = doc.text();
        self.syntax.close(id);
        self.syntax.open(id, language, &rope);
        let _ = self.lsp.tx.send(LspCmd::DidChange {
            path: path.to_path_buf(),
            version: revision as i64,
            change: clean_reload_change(id, revision, full.clone()),
            full_text: full,
            incremental: false,
        });
        if let Some(view) = self.views.get_mut(&id) {
            let len = rope.len_lines().max(1);
            if view.cursor.line >= len {
                view.cursor.line = len - 1;
                view.cursor.column = 0;
            }
            view.selection = None;
        }
        self.request_inline_diff(id);
    }

    pub fn active_doc(&self) -> Option<&Document> {
        self.active.and_then(|id| self.documents.get(&id))
    }

    pub fn active_doc_mut(&mut self) -> Option<&mut Document> {
        self.active.and_then(|id| self.documents.get_mut(&id))
    }

    pub fn active_view(&self) -> Option<&EditorView> {
        self.active.and_then(|id| self.views.get(&id))
    }

    pub fn active_view_mut(&mut self) -> Option<&mut EditorView> {
        self.active.and_then(|id| self.views.get_mut(&id))
    }

    pub fn open_path(&mut self, path: PathBuf) {
        let path = if path.is_absolute() {
            path
        } else {
            self.root.join(path)
        };
        if let Some((id, _)) = self
            .documents
            .iter()
            .find(|(_, d)| d.path.as_ref() == Some(&path))
        {
            let id = *id;
            self.active = Some(id);
            self.focus = Focus::Editor;
            return;
        }
        match Document::load(DocumentId(self.next_id), path.clone()) {
            Ok(doc) => {
                self.next_id += 1;
                let id = doc.id;
                self.syntax.open(id, doc.language, &doc.rope);
                if let Some(p) = doc.path.clone() {
                    let _ = self.lsp.tx.send(LspCmd::DidOpen {
                        path: p,
                        language: doc.language,
                        text: doc.text(),
                        version: doc.revision as i64,
                    });
                }
                self.views.insert(id, EditorView::new(id));
                self.order.push(id);
                self.documents.insert(id, doc);
                self.active = Some(id);
                self.focus = Focus::Editor;
                self.overlay = Overlay::None;
                self.request_inline_diff(id);
                self.request_review_patches(id);
            }
            Err(err) => {
                self.message = Some(format!("open failed: {err}"));
            }
        }
    }

    pub fn open_untitled(&mut self) {
        let doc = Document::new(DocumentId(self.next_id), None, String::new());
        self.next_id += 1;
        let id = doc.id;
        self.views.insert(id, EditorView::new(id));
        self.order.push(id);
        self.syntax.open(id, doc.language, &doc.rope);
        self.documents.insert(id, doc);
        self.active = Some(id);
        self.focus = Focus::Editor;
    }

    pub fn apply_changes(&mut self, changes: Vec<DocumentChange>) {
        let mut edited = Vec::new();
        for change in changes {
            let lsp_change = self
                .documents
                .get(&change.document_id)
                .map(|doc| (doc.path.clone(), doc.language, doc.revision, doc.text()));
            if let Some(doc) = self.documents.get(&change.document_id) {
                self.syntax.apply_edit(&change, doc.language, &doc.rope);
            }
            if let Some((Some(path), _language, revision, text)) = lsp_change {
                let _ = self.lsp.tx.send(LspCmd::DidChange {
                    path,
                    version: revision as i64,
                    change: change.clone(),
                    full_text: text,
                    incremental: true,
                });
                edited.push(change.document_id);
            }
        }
        let mut seen = HashSet::new();
        for document_id in edited {
            if seen.insert(document_id) {
                self.recompute_inline(document_id);
            }
        }
    }

    pub fn close_active(&mut self, force: bool) {
        let Some(id) = self.active else { return };
        if !force {
            if let Some(doc) = self.documents.get(&id) {
                if doc.is_dirty() {
                    self.overlay = Overlay::Confirm {
                        message: "Unsaved changes. Close anyway?".into(),
                        action: ConfirmAction::Close(id),
                    };
                    return;
                }
            }
        }
        if let Some(doc) = self.documents.remove(&id) {
            self.syntax.close(id);
            if let Some(path) = doc.path {
                let _ = self.lsp.tx.send(LspCmd::DidClose { path });
            }
        }
        self.views.remove(&id);
        self.order.retain(|x| *x != id);
        self.inline_changes.remove(&id);
        self.diff_baselines.remove(&id);
        self.review_patches.remove(&id);
        self.folded_hunks.retain(|(doc_id, _)| *doc_id != id);
        self.absent.remove(&id);
        if self.reveal_change == Some(id) {
            self.reveal_change = None;
        }
        self.active = self.order.last().copied();
        if self
            .markdown_preview
            .as_ref()
            .is_some_and(|preview| preview.document_id == id)
        {
            self.markdown_preview = None;
        }
        self.settle_file_search();
    }

    fn settle_file_search(&mut self) {
        let (query, selected) = match &self.overlay {
            Overlay::Search {
                kind: SearchKind::Content,
                scope: SearchScope::File,
                query,
                selected,
                ..
            } => (query.clone(), *selected),
            _ => return,
        };
        if let Some(text) = self.active_doc().map(|doc| doc.text()) {
            let file_matches = crate::workspace::find_in_document(&text, &query, true);
            let selected = selected.min(file_matches.len().saturating_sub(1));
            if let Overlay::Search {
                file_matches: slot,
                selected: selected_slot,
                matches,
                files,
                searching,
                ..
            } = &mut self.overlay
            {
                *slot = file_matches;
                *selected_slot = selected;
                matches.clear();
                files.clear();
                *searching = false;
            }
            return;
        }
        self.search_generation = self.search_generation.wrapping_add(1);
        let searching = !query.trim().is_empty();
        if searching {
            crate::workspace::spawn_workspace_search(
                self.root.clone(),
                query.clone(),
                self.search_generation,
                self.search_tx.clone(),
            );
        }
        self.overlay = Overlay::Search {
            kind: SearchKind::Content,
            scope: SearchScope::Global,
            query,
            files: Vec::new(),
            matches: Vec::new(),
            file_matches: Vec::new(),
            selected: 0,
            scroll: 0,
            searching,
        };
    }

    pub fn close_document(&mut self, id: DocumentId, force: bool) {
        if self.documents.contains_key(&id) {
            self.active = Some(id);
            self.close_active(force);
        }
    }

    pub fn close_documents(&mut self, ids: &[DocumentId], force: bool) {
        let targets: Vec<DocumentId> = ids
            .iter()
            .copied()
            .filter(|id| self.documents.contains_key(id))
            .collect();
        if targets.is_empty() {
            return;
        }
        if !force
            && targets
                .iter()
                .any(|id| self.documents.get(id).is_some_and(|doc| doc.is_dirty()))
        {
            self.overlay = Overlay::Confirm {
                message: "Unsaved changes. Close anyway?".into(),
                action: ConfirmAction::CloseMany(targets),
            };
            return;
        }
        for id in targets {
            self.active = Some(id);
            self.close_active(true);
        }
    }

    pub fn cycle_tab(&mut self, backwards: bool) {
        let Some(active) = self.active else { return };
        let Some(index) = self.order.iter().position(|id| *id == active) else {
            return;
        };
        if self.order.is_empty() {
            return;
        }
        let next = if backwards {
            index.checked_sub(1).unwrap_or(self.order.len() - 1)
        } else {
            (index + 1) % self.order.len()
        };
        self.active = Some(self.order[next]);
        self.focus = Focus::Editor;
        self.overlay = Overlay::None;
    }

    pub fn markdown_preview_open(&self) -> bool {
        self.markdown_preview
            .as_ref()
            .is_some_and(|preview| self.active == Some(preview.document_id))
    }

    pub fn toggle_markdown_preview(&mut self) {
        let Some(id) = self.active else {
            return;
        };
        if self.markdown_preview_open() {
            self.markdown_preview = None;
            return;
        }
        let Some(doc) = self.documents.get(&id) else {
            return;
        };
        if doc.language != LanguageId::Markdown {
            self.message = Some("Markdown preview is for Markdown files".into());
            return;
        }
        self.markdown_preview = Some(crate::markdown::Preview::open(id));
    }

    pub fn scroll_markdown_preview(&mut self, delta: i32) {
        let Some(active) = self.active else {
            return;
        };
        let Some(preview) = self.markdown_preview.as_mut() else {
            return;
        };
        if preview.document_id != active {
            return;
        }
        let max = preview.line_count().saturating_sub(1) as i64;
        let next = preview.scroll as i64 + i64::from(delta);
        preview.scroll = next.clamp(0, max) as usize;
    }

    pub fn jump_markdown_preview(&mut self, to_end: bool) {
        let Some(active) = self.active else {
            return;
        };
        let Some(preview) = self.markdown_preview.as_mut() else {
            return;
        };
        if preview.document_id != active {
            return;
        }
        preview.scroll = if to_end {
            preview.line_count().saturating_sub(1)
        } else {
            0
        };
    }

    pub fn request_inline_diff(&mut self, document_id: DocumentId) {
        if !self.git_repo {
            return;
        }
        let Some(doc) = self.documents.get(&document_id) else {
            return;
        };
        let Some(path) = doc.path.clone() else {
            self.inline_changes.remove(&document_id);
            self.diff_baselines.remove(&document_id);
            return;
        };
        crate::workspace::spawn_git_diff(
            self.root.clone(),
            document_id,
            path,
            self.diff_tx.clone(),
        );
    }

    pub fn recompute_inline(&mut self, document_id: DocumentId) {
        let Some(baselines) = self.diff_baselines.get(&document_id) else {
            return;
        };
        let head = baselines.head.original().to_string();
        let index = baselines.index.original().to_string();
        let Some(text) = self.documents.get(&document_id).map(|doc| doc.text()) else {
            return;
        };
        let changes = crate::workspace::review_changes(&head, &index, &text);
        let valid: HashSet<usize> = crate::workspace::change_hunks(&changes)
            .into_iter()
            .map(|hunk| hunk.start)
            .collect();
        self.folded_hunks
            .retain(|(id, start)| *id != document_id || valid.contains(start));
        self.inline_changes.insert(document_id, changes);
        if self.reveal_change == Some(document_id) {
            self.scroll_to_first_change(document_id);
            if self.reveal_change == Some(document_id) {
                self.reveal_change = None;
            }
        }
    }

    pub fn receive_diff(&mut self, result: GitDiffResult) {
        let document_id = result.document_id;
        if self.absent.contains(&document_id) {
            if let Some(crate::workspace::DiffBaseline::Present(text)) = result.head {
                self.fill_committed(document_id, text);
            }
            self.inline_changes.remove(&document_id);
            self.diff_baselines.remove(&document_id);
            self.folded_hunks.retain(|(id, _)| *id != document_id);
            if self.reveal_change == Some(document_id) {
                self.reveal_change = None;
            }
            return;
        }
        match (result.head, result.index) {
            (Some(head), Some(index)) => {
                self.diff_baselines
                    .insert(document_id, FileBaselines { head, index });
                self.recompute_inline(document_id);
            }
            _ => {
                self.diff_baselines.remove(&document_id);
                self.inline_changes.remove(&document_id);
                self.folded_hunks.retain(|(id, _)| *id != document_id);
                if self.reveal_change == Some(document_id) {
                    self.reveal_change = None;
                }
            }
        }
    }

    fn fill_committed(&mut self, id: DocumentId, text: String) {
        let Some(doc) = self.documents.get(&id) else {
            return;
        };
        if doc.text() == text && !doc.is_dirty() {
            return;
        }
        let Some(doc) = self.documents.get_mut(&id) else {
            return;
        };
        doc.show_committed(text);
        let language = doc.language;
        let rope = doc.rope.clone();
        let revision = doc.revision;
        let path = doc.path.clone();
        let full = doc.text();
        self.syntax.close(id);
        self.syntax.open(id, language, &rope);
        if let Some(path) = path {
            let _ = self.lsp.tx.send(LspCmd::DidChange {
                path,
                version: revision as i64,
                change: clean_reload_change(id, revision, full.clone()),
                full_text: full,
                incremental: false,
            });
        }
        if let Some(view) = self.views.get_mut(&id) {
            let len = rope.len_lines().max(1);
            if view.cursor.line >= len {
                view.cursor.line = len - 1;
                view.cursor.column = 0;
            }
            view.selection = None;
        }
    }

    pub fn present_hunks(&self, document_id: DocumentId) -> Vec<crate::workspace::Hunk> {
        let Some(changes) = self.inline_changes.get(&document_id) else {
            return Vec::new();
        };
        let dirty = self
            .documents
            .get(&document_id)
            .is_some_and(|doc| doc.is_dirty());
        let renamed = self
            .review_patches
            .get(&document_id)
            .is_some_and(|patches| patches.renamed);
        crate::workspace::present_hunks(changes, dirty, renamed)
    }

    pub fn visual_rows(&self, document_id: DocumentId) -> Vec<crate::workspace::VisualRow> {
        let line_count = self
            .documents
            .get(&document_id)
            .map(|doc| doc.rope.len_lines())
            .unwrap_or(0);
        let hunks = self.present_hunks(document_id);
        let folded: Vec<usize> = self
            .folded_hunks
            .iter()
            .filter(|(id, _)| *id == document_id)
            .map(|(_, start)| *start)
            .collect();
        crate::workspace::visual_rows(line_count, &hunks, &folded)
    }

    pub fn scroll_to_first_change(&mut self, document_id: DocumentId) {
        let rows = self.visual_rows(document_id);
        let Some(index) = rows
            .iter()
            .position(|row| matches!(row, crate::workspace::VisualRow::Bar { .. }))
        else {
            return;
        };
        self.reveal_change = None;
        if let Some(view) = self.views.get_mut(&document_id) {
            view.viewport.top = index;
        }
    }

    pub fn revert_hunk_at_line(&mut self, document_id: DocumentId, line: usize) {
        let prepared = self.inline_changes.get(&document_id).and_then(|changes| {
            let line_count = changes.marks.len();
            crate::workspace::change_hunks(changes)
                .into_iter()
                .find(|hunk| line >= hunk.start && line < hunk.end.max(hunk.start + 1))
                .map(|hunk| (hunk, line_count))
        });
        let Some((hunk, line_count)) = prepared else {
            return;
        };
        let Some(doc) = self.documents.get_mut(&document_id) else {
            return;
        };
        let ends_with_newline =
            doc.rope.len_chars() > 0 && doc.rope.char(doc.rope.len_chars() - 1) == '\n';
        let replacement = crate::workspace::hunk_replacement(&hunk, line_count, ends_with_newline);
        let range = crate::workspace::hunk_edit_range(&doc.rope, &replacement);
        let change = doc.apply_replace(range, replacement.text(), false);
        let caret = change.start_position;
        if let Some(view) = self.views.get_mut(&document_id) {
            view.cursor = caret;
            view.selection = None;
            view.preferred_column = None;
        }
        self.apply_changes(vec![change]);
    }

    pub fn theme(&self) -> crate::ui::theme::Theme {
        crate::ui::theme::by_name(&self.config.theme)
    }

    pub fn set_theme(&mut self, name: &str) {
        let theme = crate::ui::theme::by_name(name);
        self.config.theme = theme.name.to_string();
        self.message = if let Err(err) = self.config.save() {
            Some(format!("theme {} (not saved: {err})", theme.name))
        } else {
            Some(format!("theme {}", theme.name))
        };
    }

    pub fn relative(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    }

    pub fn next_token(&mut self) -> u64 {
        self.lsp_token += 1;
        self.lsp_token
    }

    pub fn request_completion(&mut self) {
        let Some((path, position)) = (|| {
            let doc = self.active_doc()?;
            let view = self.active_view()?;
            Some((doc.path.clone()?, utf16_pos(doc, view.cursor)))
        })() else {
            return;
        };
        let token = self.next_token();
        self.completion_token = token;
        let _ = self.lsp.tx.send(LspCmd::Completion {
            token,
            path,
            position,
        });
    }

    pub fn apply_edits(&mut self, edits: Vec<TextEdit>) {
        let mut by_path: HashMap<PathBuf, Vec<TextEdit>> = HashMap::new();
        for edit in edits {
            by_path.entry(edit.path.clone()).or_default().push(edit);
        }
        for (path, mut list) in by_path {
            if !self
                .documents
                .values()
                .any(|d| d.path.as_ref() == Some(&path))
            {
                self.open_path(path.clone());
            }
            let Some((id, _)) = self
                .documents
                .iter()
                .find(|(_, d)| d.path.as_ref() == Some(&path))
                .map(|(id, _)| (*id, ()))
            else {
                self.message = Some(format!("rename skipped missing file {}", path.display()));
                continue;
            };
            list.sort_by(|a, b| b.start.cmp(&a.start));
            let mut changes = Vec::new();
            if let Some(doc) = self.documents.get_mut(&id) {
                for edit in list {
                    let start = crate::language::utf16_position_to_doc(
                        doc,
                        edit.start.line,
                        edit.start.column as u32,
                    );
                    let end = crate::language::utf16_position_to_doc(
                        doc,
                        edit.end.line,
                        edit.end.column as u32,
                    );
                    changes.push(doc.apply_replace(Range::new(start, end), &edit.new_text, false));
                }
                if let Some(view) = self.views.get_mut(&id) {
                    view.cursor = crate::editor::clamp_position(&doc.rope, view.cursor);
                }
            }
            self.apply_changes(changes);
        }
    }
}

fn clean_reload_change(id: DocumentId, revision: u64, text: String) -> DocumentChange {
    let origin = Position::default();
    DocumentChange {
        document_id: id,
        revision,
        start_byte: 0,
        old_end_byte: 0,
        new_end_byte: text.len(),
        start_char: 0,
        old_end_char: 0,
        new_end_char: text.chars().count(),
        start_position: origin,
        old_end_position: origin,
        new_end_position: origin,
        start_row: 0,
        start_byte_column: 0,
        old_end_row: 0,
        old_end_byte_column: 0,
        new_end_row: 0,
        new_end_byte_column: 0,
        inserted_text: text,
        deleted_text: String::new(),
    }
}

fn utf16_pos(doc: &Document, pos: Position) -> Position {
    Position::new(
        pos.line,
        crate::editor::position_to_utf16(&doc.rope, pos) as usize,
    )
}

pub fn decode_diagnostic_pos(doc: &Document, pos: Position) -> Position {
    crate::language::utf16_position_to_doc(doc, pos.line, pos.column as u32)
}

pub fn is_ident_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_' || ch == '$' || ch == '.'
}

pub fn click_was_double(last: Option<(Instant, u16, u16)>, x: u16, y: u16) -> bool {
    last.is_some_and(|(t, lx, ly)| lx == x && ly == y && t.elapsed() < Duration::from_millis(400))
}

pub const GIT_BODY_LINES: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GitFooterLayout {
    pub rows: usize,
    pub sync: bool,
    pub rule: bool,
    pub subject: bool,
    pub body_gap: bool,
    pub body_lines: usize,
}

/// Sync and the subject stay. The rule goes next. Body lines are the first to drop,
/// down to one collapsed row, then none. A blank row between the subject and the
/// body stays while any body line is shown and there is room for it. An open body
/// can use three lines.
pub fn git_footer_layout(inner_h: usize, body_open: bool) -> GitFooterLayout {
    if inner_h == 0 {
        return GitFooterLayout {
            rows: 0,
            sync: false,
            rule: false,
            subject: false,
            body_gap: false,
            body_lines: 0,
        };
    }
    if inner_h == 1 {
        return GitFooterLayout {
            rows: 1,
            sync: true,
            rule: false,
            subject: false,
            body_gap: false,
            body_lines: 0,
        };
    }
    let mut rows = 2;
    let mut spare = inner_h - 2;
    let rule = spare > 0;
    if rule {
        rows += 1;
        spare -= 1;
    }
    let max_body = if body_open { GIT_BODY_LINES } else { 1 };
    let body_gap = spare >= 2;
    if body_gap {
        rows += 1;
        spare -= 1;
    }
    let body_lines = spare.min(max_body);
    rows += body_lines;
    GitFooterLayout {
        rows,
        sync: true,
        rule,
        subject: true,
        body_gap,
        body_lines,
    }
}

pub fn explorer_inner_rows(height: u16) -> usize {
    height.saturating_sub(2) as usize
}

pub fn git_list_rows(height: u16, body_open: bool) -> usize {
    let inner = explorer_inner_rows(height);
    inner.saturating_sub(git_footer_layout(inner, body_open).rows)
}

pub fn max_explorer_scroll(len: usize, inner: usize) -> usize {
    if inner == 0 {
        0
    } else {
        len.saturating_sub(inner)
    }
}

pub fn clamp_explorer_scroll(scroll: usize, len: usize, inner: usize) -> usize {
    scroll.min(max_explorer_scroll(len, inner))
}

pub fn explorer_index_at(
    scroll: usize,
    area_y: u16,
    area_height: u16,
    y: u16,
    len: usize,
) -> Option<usize> {
    let inner = explorer_inner_rows(area_height);
    if inner == 0 || y <= area_y {
        return None;
    }
    let rel = y.saturating_sub(area_y).saturating_sub(1) as usize;
    if rel >= inner {
        return None;
    }
    let idx = scroll + rel;
    (idx < len).then_some(idx)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use tokio::sync::mpsc;

    use crate::editor::{Document, DocumentId};
    use crate::language::LanguageClient;
    use crate::workspace::{
        DiffBaseline, FileBaselines, GitFileStatus, GitPathStatus, GitSnapshot, GitStatusError,
    };

    use super::*;

    fn with_runtime<T>(body: impl FnOnce() -> T) -> T {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        body()
    }

    fn test_app() -> App {
        let (tx, _rx) = mpsc::unbounded_channel();
        App::new(PathBuf::from("."), LanguageClient { tx })
    }

    fn open_saved(app: &mut App, path: PathBuf) {
        let id = DocumentId(app.next_id);
        app.next_id += 1;
        app.documents
            .insert(id, Document::new(id, Some(path), "fn main() {}\n".into()));
        app.order.push(id);
        app.active = Some(id);
    }

    #[test]
    fn inline_review_offers_stage_or_unstage() {
        with_runtime(|| {
            let mut app = test_app();
            app.git_repo = true;
            let path = PathBuf::from("/repo/src/main.rs");
            let id = DocumentId(app.next_id);
            app.next_id += 1;
            app.documents.insert(
                id,
                Document::new(id, Some(path.clone()), "fn main() { changed }\n".into()),
            );
            app.order.push(id);
            app.active = Some(id);
            app.diff_baselines.insert(
                id,
                FileBaselines {
                    head: DiffBaseline::Present("fn main() {}\n".into()),
                    index: DiffBaseline::Present("fn main() { changed }\n".into()),
                },
            );
            app.recompute_inline(id);
            assert_eq!(
                app.present_hunks(id)[0].bar,
                crate::workspace::HunkBar::Unstage
            );

            app.diff_baselines.insert(
                id,
                FileBaselines {
                    head: DiffBaseline::Present("fn main() {}\n".into()),
                    index: DiffBaseline::Present("fn main() {}\n".into()),
                },
            );
            app.recompute_inline(id);
            assert_eq!(
                app.present_hunks(id)[0].bar,
                crate::workspace::HunkBar::StageDiscard
            );
            let rows = app.visual_rows(id);
            assert!(
                rows.iter()
                    .any(|row| matches!(row, crate::workspace::VisualRow::Bar { .. }))
            );
            assert!(
                rows.iter()
                    .any(|row| matches!(row, crate::workspace::VisualRow::Removed { .. }))
            );
            let anchor = app.present_hunks(id)[0].start;
            app.toggle_hunk_fold(id, anchor);
            assert!(
                app.visual_rows(id)
                    .iter()
                    .all(|row| { !matches!(row, crate::workspace::VisualRow::Removed { .. }) })
            );
            assert!(
                app.visual_rows(id)
                    .iter()
                    .any(|row| { matches!(row, crate::workspace::VisualRow::Bar { .. }) })
            );
            app.toggle_hunk_fold(id, anchor);
            assert!(
                app.visual_rows(id)
                    .iter()
                    .any(|row| { matches!(row, crate::workspace::VisualRow::Removed { .. }) })
            );
        });
    }

    #[test]
    fn open_git_entry_opens_the_file_for_review() {
        with_runtime(|| {
            let mut app = test_app();
            app.git_repo = true;
            let file = std::env::temp_dir().join(format!("r3vu-diff-{}", std::process::id()));
            std::fs::write(&file, "hello\n").unwrap();
            app.git_status.insert(
                file.clone(),
                GitPathStatus {
                    staged: Some(GitFileStatus::Modified),
                    unstaged: None,
                },
            );
            app.open_git_entry(file.clone(), true);
            let doc = app
                .documents
                .values()
                .find(|doc| doc.path.as_deref() == Some(file.as_path()))
                .expect("open file");
            assert_eq!(app.active, Some(doc.id));
            assert!(app.review_patches.contains_key(&doc.id));
            assert!(app.markdown_preview.is_none());
            let _ = std::fs::remove_file(file);
        });
    }

    #[test]
    fn deleted_path_opens_committed_text_without_hunks() {
        with_runtime(|| {
            let mut app = test_app();
            app.git_repo = true;
            let path = std::env::temp_dir().join(format!("r3vu-gone-{}", std::process::id()));
            let _ = std::fs::remove_file(&path);
            app.git_status.insert(
                path.clone(),
                GitPathStatus {
                    staged: None,
                    unstaged: Some(GitFileStatus::Deleted),
                },
            );
            app.open_git_entry(path.clone(), false);
            let id = app.document_id(&path).expect("deleted path opens a buffer");
            assert!(app.absent.contains(&id));
            assert_eq!(app.active, Some(id));
            app.receive_diff(crate::workspace::GitDiffResult {
                document_id: id,
                head: Some(DiffBaseline::Present("export const gone = true;\n".into())),
                index: Some(DiffBaseline::Absent),
            });
            let doc = app.documents.get(&id).expect("buffer");
            assert_eq!(doc.text(), "export const gone = true;\n");
            assert!(!doc.is_dirty());
            assert!(app.present_hunks(id).is_empty());
            assert!(app.inline_changes.get(&id).is_none());
        });
    }

    #[test]
    fn a_new_markdown_file_offers_stage_and_discard() {
        with_runtime(|| {
            let root = std::env::temp_dir().join(format!("r3vu-new-md-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("app")).unwrap();
            let path = root.join("app/readme.md");
            std::fs::write(&path, "# r3vu\r\n\r\nA mouse.\r\n").unwrap();
            let (tx, _rx) = mpsc::unbounded_channel();
            let mut app = App::new(root.clone(), LanguageClient { tx });
            app.git_repo = true;
            app.git_status.insert(
                path.clone(),
                GitPathStatus {
                    staged: None,
                    unstaged: Some(GitFileStatus::Untracked),
                },
            );
            app.open_git_entry(path.clone(), false);
            let doc = app
                .documents
                .values()
                .find(|doc| doc.path.as_deref() == Some(path.as_path()))
                .expect("open buffer");
            let patches = app.review_patches.get(&doc.id).expect("review patches");
            let patch =
                crate::workspace::hunk_patch(patches.unstaged.as_ref().unwrap(), 0).unwrap();
            assert!(patch.contains("+# r3vu\r\n+\r\n+A mouse.\r\n"));
            std::fs::remove_file(&path).unwrap();
            app.reload_clean_document(&path);
            let doc = app
                .documents
                .values()
                .find(|doc| doc.path.as_deref() == Some(path.as_path()));
            let doc = doc.expect("open buffer");
            assert!(!doc.is_dirty());
            assert!(doc.text().is_empty());
            let _ = std::fs::remove_dir_all(&root);
        });
    }

    #[test]
    fn a_plain_folder_hides_git_tools() {
        with_runtime(|| {
            let mut app = test_app();
            let path = PathBuf::from("/repo/src/main.rs");
            open_saved(&mut app, path.clone());
            app.git_open = true;
            app.git_branch = Some("main".into());
            app.git_repo = true;
            app.inline_changes
                .insert(app.active.unwrap(), LineChanges::default());
            app.receive_git_status(Err(GitStatusError::NotARepository));
            assert!(!app.git_repo);
            assert!(!app.git_open);
            assert!(app.git_branch.is_none());
            assert!(app.inline_changes.is_empty());
            assert!(app.review_patches.is_empty());
            app.toggle_git();
            assert!(!app.git_open);

            app.git_repo = true;
            app.git_branch = Some("main".into());
            app.receive_git_status(Err(GitStatusError::Unavailable));
            assert!(app.git_repo);
            assert_eq!(app.git_branch.as_deref(), Some("main"));

            app.git_repo = false;
            app.receive_git_status(Ok(GitSnapshot {
                files: HashMap::new(),
                branch: Some("main".into()),
                ahead_behind: None,
            }));
            assert!(app.git_repo);
            assert_eq!(app.git_branch.as_deref(), Some("main"));
        });
    }

    #[test]
    fn push_without_upstream_asks_before_publishing() {
        with_runtime(|| {
            let mut app = test_app();
            app.root = PathBuf::from("/not-a-repo");
            app.git_repo = true;
            app.git_branch = Some("main".into());
            app.push_git();
            assert!(app.git_publish_armed);
            assert_eq!(
                app.message.as_deref(),
                Some("No upstream. Click Push again to publish.")
            );
            app.push_git();
            assert!(!app.git_publish_armed);

            app.git_publish_armed = true;
            app.commit_git();
            assert!(!app.git_publish_armed);
            assert_eq!(app.message.as_deref(), Some("Write a commit message"));

            app.git_publish_armed = true;
            app.stage_all();
            assert!(!app.git_publish_armed);
            assert_eq!(app.message.as_deref(), Some("No changes to stage"));

            app.git_publish_armed = true;
            app.pull_git();
            assert!(!app.git_publish_armed);

            app.git_ahead_behind = Some((2, 1));
            app.git_publish_armed = true;
            app.push_git();
            assert!(!app.git_publish_armed);
        });
    }

    #[test]
    fn explorer_scroll_clamps_to_the_last_page() {
        assert_eq!(clamp_explorer_scroll(100, 10, 4), 6);
        assert_eq!(clamp_explorer_scroll(5, 3, 10), 0);
        assert_eq!(clamp_explorer_scroll(2, 8, 0), 0);
    }

    #[test]
    fn explorer_index_includes_scroll_and_skips_the_border() {
        assert_eq!(explorer_index_at(5, 2, 10, 3, 20), Some(5));
        assert_eq!(explorer_index_at(5, 2, 10, 10, 20), Some(12));
        assert_eq!(explorer_index_at(5, 2, 10, 2, 20), None);
        assert_eq!(explorer_index_at(5, 2, 10, 11, 20), None);
        assert_eq!(explorer_index_at(5, 2, 10, 3, 5), None);
    }

    #[test]
    fn git_footer_drops_body_lines_before_the_rule() {
        let collapsed = git_footer_layout(20, false);
        assert_eq!(collapsed.rows, 5);
        assert!(collapsed.sync && collapsed.rule && collapsed.subject && collapsed.body_gap);
        assert_eq!(collapsed.body_lines, 1);

        let open = git_footer_layout(20, true);
        assert_eq!(open.rows, 7);
        assert!(open.body_gap);
        assert_eq!(open.body_lines, GIT_BODY_LINES);

        let one_body = git_footer_layout(4, true);
        assert!(one_body.sync && one_body.rule && one_body.subject);
        assert_eq!(one_body.body_lines, 1);
        assert_eq!(one_body.rows, 4);

        let rule = git_footer_layout(3, true);
        assert!(rule.sync && rule.rule && rule.subject);
        assert_eq!(rule.body_lines, 0);
        assert_eq!(rule.rows, 3);

        let pair = git_footer_layout(2, false);
        assert!(pair.sync && pair.subject);
        assert!(!pair.rule);
        assert_eq!(pair.body_lines, 0);
        assert_eq!(pair.rows, 2);

        let sync = git_footer_layout(1, true);
        assert!(sync.sync);
        assert!(!sync.subject && !sync.rule);
        assert_eq!(sync.rows, 1);
        assert_eq!(git_footer_layout(0, true).rows, 0);
    }
}
