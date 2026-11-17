use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use ignore::WalkBuilder;
use notify::event::{EventKind, ModifyKind};
use nucleo::pattern::{CaseMatching, Normalization};
use nucleo::{Config, Nucleo};
use tokio::process::Command;
use tokio::sync::mpsc;

use ropey::Rope;

use crate::editor::{Position, Range, editor_content_lines, editor_lines, line_visible_chars};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitFileStatus {
    Modified,
    Added,
    Deleted,
    Untracked,
    Renamed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct GitPathStatus {
    pub staged: Option<GitFileStatus>,
    pub unstaged: Option<GitFileStatus>,
}

impl GitPathStatus {
    pub fn summary(self) -> Option<GitFileStatus> {
        self.unstaged.or(self.staged)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WholeFileAction {
    Restore,
    Remove,
    Delete,
}

pub fn whole_file_action(status: GitPathStatus) -> Option<WholeFileAction> {
    if status.staged == Some(GitFileStatus::Renamed)
        || status.unstaged == Some(GitFileStatus::Renamed)
    {
        return None;
    }
    if status.unstaged == Some(GitFileStatus::Untracked) {
        return Some(WholeFileAction::Delete);
    }
    if status.staged == Some(GitFileStatus::Added) {
        return Some(WholeFileAction::Remove);
    }
    if matches!(
        status.staged,
        Some(GitFileStatus::Modified | GitFileStatus::Deleted)
    ) || matches!(
        status.unstaged,
        Some(GitFileStatus::Modified | GitFileStatus::Deleted)
    ) {
        return Some(WholeFileAction::Restore);
    }
    None
}

pub struct GitSnapshot {
    pub files: HashMap<PathBuf, GitPathStatus>,
    pub branch: Option<String>,
    pub ahead_behind: Option<(u32, u32)>,
}

pub fn branch_chip(branch: &str, ahead_behind: Option<(u32, u32)>) -> String {
    match ahead_behind {
        Some((ahead, behind)) => format!("{branch} ↑{ahead} ↓{behind}"),
        None => branch.to_string(),
    }
}

pub fn parse_ahead_behind(output: &[u8]) -> Option<(u32, u32)> {
    let text = String::from_utf8_lossy(output);
    let mut parts = text.split_whitespace();
    let ahead = parts.next()?.parse().ok()?;
    let behind = parts.next()?.parse().ok()?;
    Some((ahead, behind))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineBlame {
    pub author: String,
    pub summary: String,
    pub time: i64,
    pub hash: String,
    pub uncommitted: bool,
}

pub struct BlameUpdate {
    pub path: PathBuf,
    pub line: usize,
    pub text: String,
    pub epoch: u64,
    pub blame: Option<LineBlame>,
}

pub fn parse_line_blame(output: &str) -> Option<LineBlame> {
    let mut lines = output.lines();
    let header = lines.next()?;
    let hash = header.split_whitespace().next()?.to_string();
    if hash.len() < 4 || !hash.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return None;
    }
    let mut author = None;
    let mut summary = None;
    let mut time = None;
    for line in lines {
        if let Some(rest) = line.strip_prefix("author ") {
            author = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("author-time ") {
            time = rest.parse().ok();
        } else if let Some(rest) = line.strip_prefix("summary ") {
            summary = Some(rest.to_string());
        } else if line == "summary" {
            summary = Some(String::new());
        } else if line.starts_with('\t') || line.starts_with("filename ") {
            break;
        }
    }
    let author = author?;
    let uncommitted = hash.chars().all(|ch| ch == '0') || author == "Not Committed Yet";
    Some(LineBlame {
        author,
        summary: summary.unwrap_or_default(),
        time: time.unwrap_or(0),
        hash,
        uncommitted,
    })
}

pub fn format_unix_time(secs: i64) -> String {
    if secs < 0 {
        return String::new();
    }
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400) as u32;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let hour = tod / 3600;
    let minute = (tod % 3600) / 60;
    format!("{y:04}-{m:02}-{d:02} {hour:02}:{minute:02}")
}

pub fn spawn_line_blame(
    root: PathBuf,
    path: PathBuf,
    line: usize,
    text: String,
    contents: String,
    epoch: u64,
    tx: mpsc::UnboundedSender<BlameUpdate>,
) {
    tokio::spawn(async move {
        let spec = format!("{},{}", line + 1, line + 1);
        let blame = blame_worktree(&root, &path, &spec, &contents).await;
        let _ = tx.send(BlameUpdate {
            path,
            line,
            text,
            epoch,
            blame,
        });
    });
}

async fn blame_worktree(root: &Path, path: &Path, spec: &str, contents: &str) -> Option<LineBlame> {
    use tokio::io::AsyncWriteExt;

    let mut child = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["blame", "-L", spec, "--porcelain", "--contents", "-", "--"])
        .arg(path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(contents.as_bytes()).await;
    }
    let output = child.wait_with_output().await.ok()?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr);
        tracing::warn!("git blame exited {}: {}", output.status, message.trim());
        return None;
    }
    parse_line_blame(&String::from_utf8_lossy(&output.stdout))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitStatusError {
    NotARepository,
    Unavailable,
}

pub type GitStatusUpdate = Result<GitSnapshot, GitStatusError>;

pub fn git_status_error(stderr: &str) -> GitStatusError {
    if stderr.to_ascii_lowercase().contains("not a git repository") {
        GitStatusError::NotARepository
    } else {
        GitStatusError::Unavailable
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchKind {
    Create,
    Remove,
    Modify,
}

impl WatchKind {
    pub fn changes_tree(self) -> bool {
        matches!(self, WatchKind::Create | WatchKind::Remove)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchEvent {
    pub path: PathBuf,
    pub kind: WatchKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffKind {
    Header,
    Equal,
    Insert,
    Delete,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffKind,
    pub text: String,
    pub hunk: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineMark {
    Unchanged,
    Added,
    Modified,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeSide {
    Unstaged,
    Staged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HunkBar {
    StageDiscard,
    Unstage,
    StageUnstageDiscard,
    Revert,
    Renamed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineChanges {
    pub marks: Vec<LineMark>,
    pub sides: Vec<ChangeSide>,
    pub staged_under: Vec<bool>,
    pub deleted_before: Vec<usize>,
    pub removed_before: Vec<Vec<String>>,
    pub removed_sides: Vec<ChangeSide>,
    pub index_of: Vec<Option<usize>>,
    pub index_len: usize,
}

impl Default for LineChanges {
    fn default() -> Self {
        Self {
            marks: Vec::new(),
            sides: Vec::new(),
            staged_under: Vec::new(),
            deleted_before: vec![0],
            removed_before: vec![Vec::new()],
            removed_sides: vec![ChangeSide::Unstaged],
            index_of: Vec::new(),
            index_len: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    pub start: usize,
    pub end: usize,
    pub removed: Vec<String>,
    pub bar: HunkBar,
    pub index_anchor: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VisualRow {
    Bar { anchor: usize },
    Line(usize),
    Removed { text: String, anchor: usize },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HunkReplacement {
    Lines {
        start: usize,
        end: usize,
        text: String,
    },
    ThroughEnd {
        start: usize,
        text: String,
    },
    AtEnd {
        text: String,
    },
}

impl HunkReplacement {
    pub fn text(&self) -> &str {
        match self {
            HunkReplacement::Lines { text, .. }
            | HunkReplacement::ThroughEnd { text, .. }
            | HunkReplacement::AtEnd { text } => text,
        }
    }
}

#[derive(Clone, Debug)]
pub enum DiffBaseline {
    Present(String),
    Absent,
}

impl DiffBaseline {
    pub fn original(&self) -> &str {
        match self {
            DiffBaseline::Present(text) => text,
            DiffBaseline::Absent => "",
        }
    }
}

#[derive(Clone, Debug)]
pub struct FileBaselines {
    pub head: DiffBaseline,
    pub index: DiffBaseline,
}

#[derive(Clone, Debug)]
pub struct GitDiffResult {
    pub document_id: crate::editor::DocumentId,
    pub head: Option<DiffBaseline>,
    pub index: Option<DiffBaseline>,
}

#[derive(Clone, Debug)]
pub struct ExplorerEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub depth: usize,
    pub expanded: bool,
}

#[derive(Clone, Debug)]
pub struct SearchMatch {
    pub path: PathBuf,
    pub line: usize,
    pub column: usize,
    pub text: String,
}

pub struct WorkspaceSearch {
    pub generation: u64,
    pub matches: Vec<SearchMatch>,
    pub message: Option<String>,
}

pub struct FileFinder {
    matcher: Nucleo<String>,
    last_query: String,
}

impl FileFinder {
    pub fn new() -> Self {
        Self {
            matcher: Nucleo::new(Config::DEFAULT, Arc::new(|| {}), Some(1), 1),
            last_query: String::new(),
        }
    }

    pub fn set_files(&mut self, files: &[String]) {
        self.matcher.restart(true);
        let injector = self.matcher.injector();
        for file in files {
            injector.push(file.clone(), |item, cols| {
                cols[0] = item.as_str().into();
            });
        }
        self.last_query.clear();
    }

    pub fn query(&mut self, query: &str) -> Vec<String> {
        let append = query.starts_with(&self.last_query) && !self.last_query.is_empty();
        self.matcher
            .pattern
            .reparse(0, query, CaseMatching::Smart, Normalization::Smart, append);
        self.last_query = query.to_string();
        self.matcher.tick(20);
        let snap = self.matcher.snapshot();
        snap.matched_items(..)
            .take(200)
            .map(|item| item.data.clone())
            .collect()
    }
}

pub fn list_workspace_files(root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .build();
    for entry in walker.flatten() {
        let path = entry.path();
        if path.is_file() {
            if let Ok(rel) = path.strip_prefix(root) {
                let rel = rel.to_string_lossy().replace('\\', "/");
                if rel.starts_with(".git/") {
                    continue;
                }
                files.push(rel);
            }
        }
    }
    files.sort();
    files
}

pub fn load_directory(
    root: &Path,
    dir: &Path,
    expanded: &std::collections::HashSet<PathBuf>,
) -> Vec<ExplorerEntry> {
    let mut entries = Vec::new();
    collect_dir(root, dir, 0, expanded, &mut entries);
    entries
}

fn collect_dir(
    _root: &Path,
    dir: &Path,
    depth: usize,
    expanded: &std::collections::HashSet<PathBuf>,
    out: &mut Vec<ExplorerEntry>,
) {
    let mut children: Vec<_> = WalkBuilder::new(dir)
        .max_depth(Some(1))
        .hidden(false)
        .git_ignore(true)
        .filter_entry(|e| e.file_name() != ".git")
        .build()
        .flatten()
        .filter(|e| e.path() != dir)
        .collect();
    children.sort_by(|a, b| {
        let ad = a.path().is_dir();
        let bd = b.path().is_dir();
        bd.cmp(&ad).then_with(|| a.file_name().cmp(b.file_name()))
    });
    for child in children {
        let path = child.path().to_path_buf();
        let is_dir = path.is_dir();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name == "." || name == ".." {
            continue;
        }
        let is_expanded = is_dir && expanded.contains(&path);
        out.push(ExplorerEntry {
            path: path.clone(),
            name,
            is_dir,
            depth,
            expanded: is_expanded,
        });
        if is_expanded {
            collect_dir(_root, &path, depth + 1, expanded, out);
        }
    }
}

pub fn create_path(path: &Path, is_dir: bool) -> io::Result<()> {
    if is_dir {
        fs::create_dir_all(path)
    } else {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map(|_| ())
    }
}

pub fn rename_path(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)
}

pub fn delete_path(path: &Path) -> io::Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

pub fn spawn_git_status(root: PathBuf, tx: mpsc::UnboundedSender<GitStatusUpdate>) {
    tokio::spawn(async move {
        let output = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
            .output()
            .await;
        let update = match output {
            Ok(output) if output.status.success() => {
                let (branch, ahead_behind) =
                    tokio::join!(git_branch(&root), git_ahead_behind(&root));
                Ok(GitSnapshot {
                    files: parse_git_status(&root, &output.stdout),
                    branch,
                    ahead_behind,
                })
            }
            Ok(output) => {
                let message = String::from_utf8_lossy(&output.stderr);
                tracing::warn!("git status exited {}: {}", output.status, message.trim());
                Err(git_status_error(&message))
            }
            Err(error) => {
                tracing::warn!("git status failed: {error}");
                Err(GitStatusError::Unavailable)
            }
        };
        let _ = tx.send(update);
    });
}

async fn git_branch(root: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if name.is_empty() { None } else { Some(name) }
}

async fn git_ahead_behind(root: &Path) -> Option<(u32, u32)> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-list", "--left-right", "--count", "HEAD...@{upstream}"])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_ahead_behind(&output.stdout)
}

pub fn classify_watch_kind(kind: EventKind) -> Option<WatchKind> {
    match kind {
        EventKind::Create(_) => Some(WatchKind::Create),
        EventKind::Remove(_) => Some(WatchKind::Remove),
        EventKind::Modify(ModifyKind::Name(_)) => Some(WatchKind::Create),
        EventKind::Modify(_) => Some(WatchKind::Modify),
        EventKind::Any => Some(WatchKind::Modify),
        EventKind::Other => Some(WatchKind::Create),
        EventKind::Access(_) => None,
    }
}

pub fn is_watch_noise(path: &Path) -> bool {
    if path
        .components()
        .any(|component| component.as_os_str() == ".git")
    {
        return true;
    }
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".lock"))
}

pub fn accept_watch_path(root: &Path, path: &Path, ignore: &mut ignore::IncrementalIgnore) -> bool {
    if is_watch_noise(path) {
        return false;
    }
    let relative = if let Ok(relative) = path.strip_prefix(root) {
        relative.to_path_buf()
    } else if let Some(relative) = ignore.normalize(path) {
        relative
    } else {
        return false;
    };
    if relative.as_os_str().is_empty() {
        return false;
    }
    !ignore.matched(&relative, path.is_dir()).is_ignore()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirectoryStatus {
    pub kind: GitFileStatus,
    pub staged: bool,
}

pub fn directory_status(
    statuses: &HashMap<PathBuf, GitPathStatus>,
    dir: &Path,
) -> Option<DirectoryStatus> {
    let mut unstaged = None;
    let mut staged = None;
    for (path, status) in statuses {
        if !path.starts_with(dir) {
            continue;
        }
        if let Some(kind) = status.unstaged {
            unstaged = Some(prefer_status(unstaged, kind));
        }
        if let Some(kind) = status.staged {
            staged = Some(prefer_status(staged, kind));
        }
    }
    if let Some(kind) = unstaged {
        Some(DirectoryStatus {
            kind,
            staged: false,
        })
    } else {
        staged.map(|kind| DirectoryStatus { kind, staged: true })
    }
}

fn prefer_status(current: Option<GitFileStatus>, next: GitFileStatus) -> GitFileStatus {
    match current {
        Some(status) if status_rank(status) <= status_rank(next) => status,
        _ => next,
    }
}

fn status_rank(status: GitFileStatus) -> u8 {
    match status {
        GitFileStatus::Modified => 0,
        GitFileStatus::Added => 1,
        GitFileStatus::Deleted => 2,
        GitFileStatus::Renamed => 3,
        GitFileStatus::Untracked => 4,
    }
}

pub fn watch_matcher(root: &Path) -> ignore::IncrementalIgnore {
    let mut matchers = WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .build_matchers();
    matchers
        .pop()
        .expect("watch matcher for the workspace root")
}

pub fn parse_git_status(root: &Path, output: &[u8]) -> HashMap<PathBuf, GitPathStatus> {
    let mut result = HashMap::new();
    let mut records = output
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty());
    while let Some(record) = records.next() {
        if record.len() < 3 {
            continue;
        }
        let x = record[0];
        let y = record[1];
        let path_at = if record.get(2) == Some(&b' ') { 3 } else { 2 };
        let first = String::from_utf8_lossy(&record[path_at..]).into_owned();
        let renamed = matches!(x, b'R' | b'C') || matches!(y, b'R' | b'C');
        let path = if renamed {
            let Some(next) = records.next() else {
                continue;
            };
            String::from_utf8_lossy(next).into_owned()
        } else {
            first
        };
        if path.is_empty() {
            continue;
        }
        let Some(status) = path_status(x, y) else {
            continue;
        };
        result.insert(root.join(path), status);
    }
    result
}

fn path_status(x: u8, y: u8) -> Option<GitPathStatus> {
    if x == b'?' && y == b'?' {
        return Some(GitPathStatus {
            staged: None,
            unstaged: Some(GitFileStatus::Untracked),
        });
    }
    let staged = column_status(x);
    let unstaged = column_status(y);
    if staged.is_none() && unstaged.is_none() {
        None
    } else {
        Some(GitPathStatus { staged, unstaged })
    }
}

fn column_status(byte: u8) -> Option<GitFileStatus> {
    match byte {
        b'M' | b'T' | b'U' => Some(GitFileStatus::Modified),
        b'A' => Some(GitFileStatus::Added),
        b'D' => Some(GitFileStatus::Deleted),
        b'R' | b'C' => Some(GitFileStatus::Renamed),
        _ => None,
    }
}

pub fn project_path(root: &Path, raw: &str) -> Option<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() || raw.contains('\0') {
        return None;
    }
    let raw_path = Path::new(raw);
    let joined = if raw_path.is_absolute() {
        raw_path.to_path_buf()
    } else {
        root.join(raw_path)
    };
    let mut normal = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::Prefix(prefix) => normal.push(prefix.as_os_str()),
            Component::RootDir => normal.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normal.pop() {
                    return None;
                }
            }
            Component::Normal(part) => normal.push(part),
        }
    }
    let relative = normal.strip_prefix(root).ok()?;
    if relative.as_os_str().is_empty() {
        return None;
    }
    Some(normal)
}

pub fn remember_path(touched: &mut Vec<PathBuf>, path: PathBuf) {
    if !touched.iter().any(|item| item == &path) {
        touched.push(path);
    }
}

pub fn status_delta(
    before: &HashMap<PathBuf, GitPathStatus>,
    after: &HashMap<PathBuf, GitPathStatus>,
) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = before.keys().chain(after.keys()).cloned().collect();
    paths.sort();
    paths.dedup();
    paths.retain(|path| before.get(path) != after.get(path));
    paths
}

pub fn repo_relative(root: &Path, path: &Path) -> Option<String> {
    let relative = if path.is_absolute() {
        path.strip_prefix(root).ok()?
    } else {
        path
    };
    if relative.as_os_str().is_empty() || relative.is_absolute() {
        return None;
    }
    if relative
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return None;
    }
    let text = relative.to_string_lossy().replace('\\', "/");
    if text.is_empty() || text.starts_with('/') {
        None
    } else {
        Some(text)
    }
}

pub fn can_commit(message: &str, files: &HashMap<PathBuf, GitPathStatus>) -> bool {
    !message.trim().is_empty() && files.values().any(|status| status.staged.is_some())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitWrite {
    Stage(PathBuf),
    Unstage(PathBuf),
    StageAll,
    UnstageAll,
    Apply {
        patch: String,
        cached: bool,
        reverse: bool,
        reload: Option<PathBuf>,
    },
    Commit {
        message: String,
        body: String,
    },
    Pull,
    Push,
    Publish(String),
    Restore(PathBuf),
    Remove(PathBuf),
}

pub struct GitArgv {
    pub args: Vec<String>,
    pub stdin: Option<String>,
}

pub fn prepare_git_write(root: &Path, write: &GitWrite) -> Result<GitArgv, String> {
    match write {
        GitWrite::Stage(path) => {
            let rel = repo_relative(root, path)
                .ok_or_else(|| "Path is outside the project".to_string())?;
            Ok(GitArgv {
                args: vec!["add".into(), "--".into(), rel],
                stdin: None,
            })
        }
        GitWrite::Unstage(path) => {
            let rel = repo_relative(root, path)
                .ok_or_else(|| "Path is outside the project".to_string())?;
            Ok(GitArgv {
                args: vec!["restore".into(), "--staged".into(), "--".into(), rel],
                stdin: None,
            })
        }
        GitWrite::StageAll => Ok(GitArgv {
            args: vec!["add".into(), "-A".into()],
            stdin: None,
        }),
        GitWrite::UnstageAll => Ok(GitArgv {
            args: vec!["restore".into(), "--staged".into(), ":/".into()],
            stdin: None,
        }),
        GitWrite::Apply {
            patch,
            cached,
            reverse,
            ..
        } => {
            let mut args = vec!["apply".into()];
            if *reverse {
                args.push("--reverse".into());
            }
            if *cached {
                args.push("--cached".into());
            }
            args.push("-".into());
            Ok(GitArgv {
                args,
                stdin: Some(patch.clone()),
            })
        }
        GitWrite::Commit { message, body } => {
            let message = message.trim();
            if message.is_empty() {
                return Err("Write a commit message".into());
            }
            if message.contains('\0') || body.contains('\0') {
                return Err("Commit message is invalid".into());
            }
            let mut args = vec!["commit".into(), "-m".into(), message.to_string()];
            let body = body.trim();
            if !body.is_empty() {
                args.push("-m".into());
                args.push(body.to_string());
            }
            Ok(GitArgv { args, stdin: None })
        }
        GitWrite::Pull => Ok(GitArgv {
            args: vec!["pull".into(), "--ff-only".into()],
            stdin: None,
        }),
        GitWrite::Push => Ok(GitArgv {
            args: vec!["push".into()],
            stdin: None,
        }),
        GitWrite::Publish(branch) => {
            if !publishable_branch(branch) {
                return Err("No branch to publish".into());
            }
            Ok(GitArgv {
                args: vec!["push".into(), "-u".into(), "origin".into(), branch.clone()],
                stdin: None,
            })
        }
        GitWrite::Restore(path) => {
            let rel = repo_relative(root, path)
                .ok_or_else(|| "Path is outside the project".to_string())?;
            Ok(GitArgv {
                args: vec![
                    "restore".into(),
                    "--source=HEAD".into(),
                    "--staged".into(),
                    "--worktree".into(),
                    "--".into(),
                    rel,
                ],
                stdin: None,
            })
        }
        GitWrite::Remove(path) => {
            let rel = repo_relative(root, path)
                .ok_or_else(|| "Path is outside the project".to_string())?;
            Ok(GitArgv {
                args: vec!["rm".into(), "-f".into(), "--".into(), rel],
                stdin: None,
            })
        }
    }
}

fn publishable_branch(branch: &str) -> bool {
    !branch.is_empty()
        && !branch.starts_with('-')
        && !branch.contains([' ', '\0', '\n', '~', '^', ':', '\\', '?', '*', '['])
        && !branch.contains("..")
}

#[derive(Clone, Debug)]
pub struct GitWriteResult {
    pub ok: bool,
    pub message: String,
    pub reload: Option<PathBuf>,
    pub close_path: Option<PathBuf>,
    pub committed: Option<String>,
    pub committed_body: Option<String>,
    pub notice: Option<String>,
}

pub fn spawn_git_write(root: PathBuf, write: GitWrite, tx: mpsc::UnboundedSender<GitWriteResult>) {
    tokio::spawn(async move {
        let prepared = prepare_git_write(&root, &write);
        let mut result = match prepared {
            Ok(argv) => run_git(&root, &argv.args, argv.stdin).await,
            Err(message) => GitWriteResult {
                ok: false,
                message,
                reload: None,
                close_path: None,
                committed: None,
                committed_body: None,
                notice: None,
            },
        };
        if result.ok {
            match &write {
                GitWrite::Commit { message, body } => {
                    result.committed = Some(message.trim().to_string());
                    result.committed_body = Some(body.trim().to_string());
                }
                GitWrite::Pull => result.notice = Some("Pulled".into()),
                GitWrite::Push | GitWrite::Publish(_) => result.notice = Some("Pushed".into()),
                GitWrite::Apply { reload, .. } => result.reload = reload.clone(),
                GitWrite::Restore(path) => result.reload = Some(path.clone()),
                GitWrite::Remove(path) => result.close_path = Some(path.clone()),
                _ => {}
            }
        }
        let _ = tx.send(result);
    });
}

async fn run_git(root: &Path, args: &[String], stdin_data: Option<String>) -> GitWriteResult {
    let mut command = Command::new("git");
    command.arg("-C").arg(root);
    for arg in args {
        command.arg(arg);
    }
    if let Some(data) = stdin_data {
        command.stdin(std::process::Stdio::piped());
        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::piped());
        let spawned = command.spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(error) => return git_failed(format!("Git unavailable: {error}")),
        };
        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            if let Err(error) = stdin.write_all(data.as_bytes()).await {
                return git_failed(format!("Git unavailable: {error}"));
            }
        }
        match child.wait_with_output().await {
            Ok(output) => git_finished(output),
            Err(error) => git_failed(format!("Git unavailable: {error}")),
        }
    } else {
        match command.output().await {
            Ok(output) => git_finished(output),
            Err(error) => git_failed(format!("Git unavailable: {error}")),
        }
    }
}

fn git_finished(output: std::process::Output) -> GitWriteResult {
    if output.status.success() {
        GitWriteResult {
            ok: true,
            message: String::new(),
            reload: None,
            close_path: None,
            committed: None,
            committed_body: None,
            notice: None,
        }
    } else {
        let message = String::from_utf8_lossy(&output.stderr)
            .trim()
            .replace("fatal: ", "");
        git_failed(if message.is_empty() {
            format!("git exited {}", output.status)
        } else {
            message
        })
    }
}

fn git_failed(message: String) -> GitWriteResult {
    GitWriteResult {
        ok: false,
        message,
        reload: None,
        close_path: None,
        committed: None,
        committed_body: None,
        notice: None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitHunk {
    pub header: String,
    pub lines: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedGitDiff {
    pub preamble: Vec<String>,
    pub hunks: Vec<GitHunk>,
}

pub fn parse_git_diff(text: &str) -> ParsedGitDiff {
    let mut preamble = Vec::new();
    let mut hunks = Vec::new();
    let mut current: Option<GitHunk> = None;
    // Split on `\n` only. `str::lines()` also strips a trailing `\r`, and that
    // byte is part of a CR or CRLF markdown line that `git apply` must match.
    for line in git_diff_lines(text) {
        if line.starts_with("@@") {
            if let Some(hunk) = current.take() {
                hunks.push(hunk);
            }
            current = Some(GitHunk {
                header: line.to_string(),
                lines: Vec::new(),
            });
        } else if let Some(hunk) = current.as_mut() {
            hunk.lines.push(line.to_string());
        } else {
            preamble.push(line.to_string());
        }
    }
    if let Some(hunk) = current {
        hunks.push(hunk);
    }
    ParsedGitDiff { preamble, hunks }
}

pub fn hunk_patch(diff: &ParsedGitDiff, index: usize) -> Option<String> {
    let hunk = diff.hunks.get(index)?;
    let mut out = String::new();
    for line in &diff.preamble {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(&hunk.header);
    out.push('\n');
    for line in &hunk.lines {
        out.push_str(line);
        out.push('\n');
    }
    Some(out)
}

/// Diff rows split on LF only. A CR before that LF stays on the row.
fn git_diff_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (index, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            lines.push(&text[start..index]);
            start = index + 1;
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

pub fn lines_from_git_diff(diff: &ParsedGitDiff) -> Vec<DiffLine> {
    let mut lines = Vec::new();
    for line in &diff.preamble {
        lines.push(DiffLine {
            kind: DiffKind::Header,
            text: line.clone(),
            hunk: None,
        });
    }
    for (index, hunk) in diff.hunks.iter().enumerate() {
        let mut body = Vec::new();
        for line in &hunk.lines {
            body.extend(expand_diff_line(line));
        }
        let old_count = body
            .iter()
            .filter(|line| matches!(line.kind, DiffKind::Delete | DiffKind::Equal))
            .count();
        let new_count = body
            .iter()
            .filter(|line| matches!(line.kind, DiffKind::Insert | DiffKind::Equal))
            .count();
        lines.push(DiffLine {
            kind: DiffKind::Header,
            text: displayed_hunk_header(&hunk.header, old_count, new_count),
            hunk: Some(index),
        });
        lines.extend(body);
    }
    lines
}

fn expand_diff_line(line: &str) -> Vec<DiffLine> {
    if line.starts_with('\\') {
        return vec![DiffLine {
            kind: DiffKind::Header,
            text: line.to_string(),
            hunk: None,
        }];
    }
    let (kind, marker, content) = if let Some(content) = line.strip_prefix('+') {
        (DiffKind::Insert, "+", content)
    } else if let Some(content) = line.strip_prefix('-') {
        (DiffKind::Delete, "-", content)
    } else if let Some(content) = line.strip_prefix(' ') {
        (DiffKind::Equal, " ", content)
    } else {
        return vec![DiffLine {
            kind: DiffKind::Equal,
            text: line.to_string(),
            hunk: None,
        }];
    };
    let mut parts = editor_content_lines(content);
    // `+\r` is one empty CRLF row. Splitting treats that lone CR as a break
    // and would otherwise drop the row.
    if parts.is_empty() {
        parts.push(String::new());
    }
    parts
        .into_iter()
        .map(|part| DiffLine {
            kind,
            text: format!("{marker}{part}"),
            hunk: None,
        })
        .collect()
}

fn displayed_hunk_header(header: &str, old_count: usize, new_count: usize) -> String {
    let Some((old_start, parsed_old, new_start, parsed_new, tail)) = parse_hunk_header(header)
    else {
        return header.to_string();
    };
    if parsed_old == old_count && parsed_new == new_count {
        return header.to_string();
    }
    format!("@@ -{old_start},{old_count} +{new_start},{new_count} @@{tail}")
}

fn parse_hunk_header(header: &str) -> Option<(usize, usize, usize, usize, &str)> {
    let rest = header.strip_prefix("@@")?.trim_start();
    let mut tokens = rest.split_whitespace();
    let old = tokens.next()?;
    let new = tokens.next()?;
    let (old_start, old_count) = parse_hunk_ref(old)?;
    let (new_start, new_count) = parse_hunk_ref(new)?;
    let tail_at = header[2..].find("@@")? + 2;
    let tail = header.get(tail_at + 2..)?;
    Some((old_start, old_count, new_start, new_count, tail))
}

fn parse_hunk_ref(token: &str) -> Option<(usize, usize)> {
    let token = token.trim_start_matches(['-', '+']);
    let mut parts = token.split(',');
    let start = parts.next()?.parse().ok()?;
    let count = match parts.next() {
        Some(count) => count.parse().ok()?,
        None => 1,
    };
    Some((start, count))
}

pub fn untracked_diff(relative: &str, text: &str) -> (Vec<DiffLine>, Option<ParsedGitDiff>) {
    if let Some(parsed) = new_file_diff(relative, text) {
        return (lines_from_git_diff(&parsed), Some(parsed));
    }
    let mut lines = vec![
        DiffLine {
            kind: DiffKind::Header,
            text: "--- /dev/null".into(),
            hunk: None,
        },
        DiffLine {
            kind: DiffKind::Header,
            text: "+++ new file".into(),
            hunk: None,
        },
    ];
    for line in editor_content_lines(text) {
        lines.push(DiffLine {
            kind: DiffKind::Insert,
            text: format!("+{line}"),
            hunk: None,
        });
    }
    (lines, None)
}

/// A new-file patch `git apply` can stage, unstage, and discard.
/// Rows stay split on LF only so a trailing CR still matches the file.
fn new_file_diff(relative: &str, text: &str) -> Option<ParsedGitDiff> {
    if text.is_empty()
        || relative.is_empty()
        || relative.contains(['\n', '\r', '\0'])
        || relative.starts_with('/')
    {
        return None;
    }
    let rows = git_diff_lines(text);
    let count = rows.len();
    if count == 0 {
        return None;
    }
    let header = if count == 1 {
        "@@ -0,0 +1 @@".to_string()
    } else {
        format!("@@ -0,0 +1,{count} @@")
    };
    let mut lines: Vec<String> = rows.into_iter().map(|line| format!("+{line}")).collect();
    if !text.ends_with('\n') {
        lines.push("\\ No newline at end of file".into());
    }
    Some(ParsedGitDiff {
        preamble: vec![
            format!("diff --git a/{relative} b/{relative}"),
            "new file mode 100644".into(),
            "--- /dev/null".into(),
            format!("+++ b/{relative}"),
        ],
        hunks: vec![GitHunk { header, lines }],
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitPanelRow {
    Section {
        staged: bool,
    },
    File {
        path: PathBuf,
        status: GitFileStatus,
        staged: bool,
    },
}

pub fn git_panel_rows(files: &HashMap<PathBuf, GitPathStatus>) -> Vec<GitPanelRow> {
    let mut staged = Vec::new();
    let mut changed = Vec::new();
    for (path, status) in files {
        if let Some(kind) = status.staged {
            staged.push((path.clone(), kind));
        }
        if let Some(kind) = status.unstaged {
            changed.push((path.clone(), kind));
        }
    }
    staged.sort_by(|left, right| left.0.cmp(&right.0));
    changed.sort_by(|left, right| left.0.cmp(&right.0));
    let mut rows = vec![GitPanelRow::Section { staged: true }];
    for (path, status) in staged {
        rows.push(GitPanelRow::File {
            path,
            status,
            staged: true,
        });
    }
    rows.push(GitPanelRow::Section { staged: false });
    for (path, status) in changed {
        rows.push(GitPanelRow::File {
            path,
            status,
            staged: false,
        });
    }
    rows
}

#[derive(Clone, Debug)]
pub struct FileDiffResult {
    pub token: u64,
    pub path: PathBuf,
    pub staged: bool,
    pub lines: Vec<DiffLine>,
    pub parsed: Option<ParsedGitDiff>,
    pub allow_hunks: bool,
    pub error: Option<String>,
}

pub fn spawn_file_diff(
    root: PathBuf,
    path: PathBuf,
    staged: bool,
    allow_hunks: bool,
    token: u64,
    tx: mpsc::UnboundedSender<FileDiffResult>,
) {
    tokio::spawn(async move {
        let result = match repo_relative(&root, &path) {
            None => FileDiffResult {
                token,
                path,
                staged,
                lines: Vec::new(),
                parsed: None,
                allow_hunks: false,
                error: Some("Path is outside the project".into()),
            },
            Some(relative) => {
                let mut args = vec![
                    "diff".to_string(),
                    "-U3".into(),
                    "--no-ext-diff".into(),
                    "--no-color".into(),
                ];
                if staged {
                    args.push("--cached".into());
                }
                args.push("--".into());
                args.push(relative);
                match Command::new("git")
                    .arg("-C")
                    .arg(&root)
                    .args(&args)
                    .output()
                    .await
                {
                    Ok(output) if output.status.success() => {
                        let text = String::from_utf8_lossy(&output.stdout).into_owned();
                        let parsed = parse_git_diff(&text);
                        let has_hunks = !parsed.hunks.is_empty();
                        let lines = lines_from_git_diff(&parsed);
                        FileDiffResult {
                            token,
                            path,
                            staged,
                            lines,
                            parsed: (allow_hunks && has_hunks).then_some(parsed),
                            allow_hunks: allow_hunks && has_hunks,
                            error: None,
                        }
                    }
                    Ok(output) => {
                        let message = String::from_utf8_lossy(&output.stderr)
                            .trim()
                            .replace("fatal: ", "");
                        FileDiffResult {
                            token,
                            path,
                            staged,
                            lines: Vec::new(),
                            parsed: None,
                            allow_hunks: false,
                            error: Some(if message.is_empty() {
                                format!("git exited {}", output.status)
                            } else {
                                message
                            }),
                        }
                    }
                    Err(error) => FileDiffResult {
                        token,
                        path,
                        staged,
                        lines: Vec::new(),
                        parsed: None,
                        allow_hunks: false,
                        error: Some(format!("Git unavailable: {error}")),
                    },
                }
            }
        };
        let _ = tx.send(result);
    });
}

pub fn path_absent_from_head(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("exists on disk, but not in") || message.contains("does not exist in")
}

pub fn spawn_git_diff(
    root: PathBuf,
    document_id: crate::editor::DocumentId,
    path: PathBuf,
    tx: mpsc::UnboundedSender<GitDiffResult>,
) {
    tokio::spawn(async move {
        let relative = path.strip_prefix(&root).unwrap_or(&path);
        let spec = relative.to_string_lossy().replace('\\', "/");
        let (head, index) = tokio::join!(
            show_baseline(root.clone(), format!("HEAD:{spec}")),
            show_baseline(root, format!(":{spec}")),
        );
        let _ = tx.send(GitDiffResult {
            document_id,
            head,
            index,
        });
    });
}

async fn show_baseline(root: PathBuf, spec: String) -> Option<DiffBaseline> {
    let output = Command::new("git")
        .arg("-C")
        .arg(&root)
        .arg("show")
        .arg(spec)
        .output()
        .await;
    match output {
        Ok(output) if output.status.success() => Some(DiffBaseline::Present(
            String::from_utf8_lossy(&output.stdout).into_owned(),
        )),
        Ok(output) => {
            let message = String::from_utf8_lossy(&output.stderr)
                .trim()
                .to_string()
                .replace("fatal: ", "");
            path_absent_from_head(&message).then_some(DiffBaseline::Absent)
        }
        Err(_) => None,
    }
}

pub fn classify_lines(original: &str, current: &str) -> LineChanges {
    use similar::{ChangeTag, TextDiff};

    let old_lines = split_lines(original);
    let new_lines = split_lines(current);
    let old_refs: Vec<&str> = old_lines.iter().map(String::as_str).collect();
    let new_refs: Vec<&str> = new_lines.iter().map(String::as_str).collect();
    let diff = TextDiff::from_slices(&old_refs, &new_refs);
    let mut marks = vec![LineMark::Unchanged; new_lines.len()];
    let mut deleted_before = vec![0; new_lines.len() + 1];
    let mut removed_before = vec![Vec::new(); new_lines.len() + 1];
    let mut index = 0;
    let mut pending_deletes = 0usize;
    let mut pending_text: Vec<String> = Vec::new();
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Equal => {
                removed_before[index].append(&mut pending_text);
                deleted_before[index] += pending_deletes;
                pending_deletes = 0;
                index += 1;
            }
            ChangeTag::Delete => {
                pending_text.push(diff_line_text(change.value()));
                pending_deletes += 1;
            }
            ChangeTag::Insert => {
                if pending_deletes > 0 {
                    pending_deletes -= 1;
                    if !pending_text.is_empty() {
                        removed_before[index].push(pending_text.remove(0));
                    }
                    marks[index] = LineMark::Modified;
                } else {
                    marks[index] = LineMark::Added;
                }
                index += 1;
            }
        }
    }
    if index < deleted_before.len() {
        removed_before[index].append(&mut pending_text);
        deleted_before[index] += pending_deletes;
    }
    let len = marks.len();
    LineChanges {
        sides: vec![ChangeSide::Unstaged; len],
        staged_under: vec![false; len],
        removed_sides: vec![ChangeSide::Unstaged; deleted_before.len()],
        index_of: vec![None; len],
        index_len: 0,
        marks,
        deleted_before,
        removed_before,
    }
}

pub fn review_changes(head: &str, index: &str, buffer: &str) -> LineChanges {
    let mut changes = classify_lines(index, buffer);
    let staged = classify_lines(head, index);
    let origin = line_origins(index, buffer);
    changes.index_of = origin.clone();
    changes.index_len = staged.marks.len();
    for buffer_line in 0..changes.marks.len() {
        let Some(index_line) = origin.get(buffer_line).copied().flatten() else {
            continue;
        };
        if changes.marks[buffer_line] != LineMark::Unchanged {
            let staged_mark = staged
                .marks
                .get(index_line)
                .copied()
                .unwrap_or(LineMark::Unchanged);
            let staged_removed = staged
                .removed_before
                .get(index_line)
                .is_some_and(|lines| !lines.is_empty());
            changes.staged_under[buffer_line] =
                staged_mark != LineMark::Unchanged || staged_removed;
            continue;
        }
        project_staged(&mut changes, buffer_line, index_line, &staged);
    }
    let end = changes.marks.len();
    if changes.deleted_before.get(end).copied().unwrap_or(0) == 0 {
        let index_end = staged.marks.len();
        if staged
            .deleted_before
            .get(index_end)
            .copied()
            .is_some_and(|deleted| deleted > 0)
        {
            changes.removed_before[end] = removed_at(&staged, index_end);
            changes.deleted_before[end] = staged.deleted_before[index_end];
            changes.removed_sides[end] = ChangeSide::Staged;
        }
    }
    changes
}

fn project_staged(
    changes: &mut LineChanges,
    buffer_line: usize,
    index_line: usize,
    staged: &LineChanges,
) {
    let mark = staged
        .marks
        .get(index_line)
        .copied()
        .unwrap_or(LineMark::Unchanged);
    match mark {
        LineMark::Added => {
            changes.marks[buffer_line] = LineMark::Added;
            changes.sides[buffer_line] = ChangeSide::Staged;
        }
        LineMark::Modified => {
            changes.marks[buffer_line] = LineMark::Modified;
            changes.sides[buffer_line] = ChangeSide::Staged;
            if changes.removed_before[buffer_line].is_empty() {
                changes.removed_before[buffer_line] = removed_at(staged, index_line);
                changes.removed_sides[buffer_line] = ChangeSide::Staged;
            }
        }
        LineMark::Unchanged => {}
    }
    let pure = staged.deleted_before.get(index_line).copied().unwrap_or(0);
    if pure > 0
        && changes.deleted_before[buffer_line] == 0
        && changes.marks[buffer_line] == LineMark::Unchanged
    {
        changes.removed_before[buffer_line] = removed_at(staged, index_line);
        changes.deleted_before[buffer_line] = pure;
        changes.removed_sides[buffer_line] = ChangeSide::Staged;
    }
}

fn line_origins(original: &str, current: &str) -> Vec<Option<usize>> {
    use similar::{ChangeTag, TextDiff};

    let old_lines = split_lines(original);
    let new_lines = split_lines(current);
    let old_refs: Vec<&str> = old_lines.iter().map(String::as_str).collect();
    let new_refs: Vec<&str> = new_lines.iter().map(String::as_str).collect();
    let diff = TextDiff::from_slices(&old_refs, &new_refs);
    let mut origin = vec![None; new_lines.len()];
    let mut new_index = 0usize;
    let mut old_index = 0usize;
    let mut pending_old: Vec<usize> = Vec::new();
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Equal => {
                pending_old.clear();
                if new_index < origin.len() {
                    origin[new_index] = Some(old_index);
                }
                new_index += 1;
                old_index += 1;
            }
            ChangeTag::Delete => {
                pending_old.push(old_index);
                old_index += 1;
            }
            ChangeTag::Insert => {
                if new_index < origin.len() {
                    origin[new_index] = if pending_old.is_empty() {
                        None
                    } else {
                        Some(pending_old.remove(0))
                    };
                }
                new_index += 1;
            }
        }
    }
    origin
}

fn diff_line_text(value: &str) -> String {
    value.trim_end_matches(['\n', '\r']).to_string()
}

pub fn change_hunks(changes: &LineChanges) -> Vec<Hunk> {
    let mut hunks = Vec::new();
    let mut index = 0;
    while index < changes.marks.len() {
        if changes.deleted_before[index] > 0 && changes.marks[index] == LineMark::Unchanged {
            let removed = removed_at(changes, index);
            hunks.push(make_hunk(changes, index, index, removed));
            index += 1;
            continue;
        }
        if changes.marks[index] != LineMark::Unchanged {
            let start = index;
            let mut removed = removed_at(changes, index);
            index += 1;
            while index < changes.marks.len() && changes.marks[index] != LineMark::Unchanged {
                removed.extend(removed_at(changes, index));
                index += 1;
            }
            hunks.push(make_hunk(changes, start, index, removed));
            continue;
        }
        index += 1;
    }
    if changes
        .deleted_before
        .last()
        .copied()
        .is_some_and(|deleted| deleted > 0)
    {
        let start = changes.marks.len();
        let removed = removed_at(changes, start);
        hunks.push(make_hunk(changes, start, start, removed));
    }
    hunks
}

fn make_hunk(changes: &LineChanges, start: usize, end: usize, removed: Vec<String>) -> Hunk {
    let (bar, index_anchor) = hunk_bar(changes, start, end);
    Hunk {
        start,
        end,
        removed,
        bar,
        index_anchor,
    }
}

fn hunk_bar(changes: &LineChanges, start: usize, end: usize) -> (HunkBar, Option<usize>) {
    let mut unstaged = false;
    let mut staged = false;
    if changes
        .removed_before
        .get(start)
        .is_some_and(|lines| !lines.is_empty())
    {
        if changes.removed_sides.get(start).copied() == Some(ChangeSide::Staged) {
            staged = true;
        } else {
            unstaged = true;
        }
    }
    let scan_end = if end == start { start } else { end };
    for line in start..scan_end {
        if changes.marks.get(line).copied() == Some(LineMark::Unchanged) {
            continue;
        }
        match changes.sides.get(line).copied() {
            Some(ChangeSide::Staged) => staged = true,
            Some(ChangeSide::Unstaged) => {
                unstaged = true;
                if changes.staged_under.get(line).copied().unwrap_or(false) {
                    staged = true;
                }
            }
            None => {}
        }
    }
    let index_anchor = if start >= changes.index_of.len() {
        Some(changes.index_len)
    } else {
        changes.index_of.get(start).copied().flatten().or_else(|| {
            (start..scan_end).find_map(|line| changes.index_of.get(line).copied().flatten())
        })
    };
    let bar = match (unstaged, staged) {
        (true, true) => HunkBar::StageUnstageDiscard,
        (false, true) => HunkBar::Unstage,
        _ => HunkBar::StageDiscard,
    };
    (bar, index_anchor)
}

pub fn present_hunks(changes: &LineChanges, dirty: bool, renamed: bool) -> Vec<Hunk> {
    let mut hunks = change_hunks(changes);
    for hunk in &mut hunks {
        if renamed {
            hunk.bar = HunkBar::Renamed;
        } else if dirty
            && matches!(
                hunk.bar,
                HunkBar::StageDiscard | HunkBar::StageUnstageDiscard
            )
        {
            hunk.bar = HunkBar::Revert;
        }
    }
    hunks
}

pub fn patch_touching(diff: &ParsedGitDiff, start: usize, end: usize) -> Option<String> {
    let mut picked = Vec::new();
    for (index, hunk) in diff.hunks.iter().enumerate() {
        if hunk_overlaps(&hunk.header, start, end) {
            picked.push(index);
        }
    }
    if picked.is_empty() {
        None
    } else {
        Some(join_hunks(diff, &picked))
    }
}

fn hunk_overlaps(header: &str, start: usize, end: usize) -> bool {
    let Some((_, _, new_start, new_count, _)) = parse_hunk_header(header) else {
        return false;
    };
    let new_start = new_start.saturating_sub(1);
    let end = if end == start { start + 1 } else { end };
    if new_count == 0 {
        return new_start >= start && new_start < end;
    }
    let new_end = new_start + new_count;
    start < new_end && new_start < end
}

fn join_hunks(diff: &ParsedGitDiff, indices: &[usize]) -> String {
    let mut out = String::new();
    for line in &diff.preamble {
        out.push_str(line);
        out.push('\n');
    }
    for index in indices {
        let Some(hunk) = diff.hunks.get(*index) else {
            continue;
        };
        out.push_str(&hunk.header);
        out.push('\n');
        for line in &hunk.lines {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn removed_at(changes: &LineChanges, index: usize) -> Vec<String> {
    changes
        .removed_before
        .get(index)
        .cloned()
        .unwrap_or_default()
}

pub fn hunk_replacement(
    hunk: &Hunk,
    line_count: usize,
    ends_with_newline: bool,
) -> HunkReplacement {
    if hunk.start >= line_count {
        let mut text = String::new();
        if line_count > 0 && !ends_with_newline && !hunk.removed.is_empty() {
            text.push('\n');
        }
        if !hunk.removed.is_empty() {
            text.push_str(&hunk.removed.join("\n"));
        }
        return HunkReplacement::AtEnd { text };
    }
    if hunk.end >= line_count {
        return HunkReplacement::ThroughEnd {
            start: hunk.start,
            text: hunk.removed.join("\n"),
        };
    }
    let mut text = String::new();
    for line in &hunk.removed {
        text.push_str(line);
        text.push('\n');
    }
    HunkReplacement::Lines {
        start: hunk.start,
        end: hunk.end,
        text,
    }
}

pub fn hunk_edit_range(rope: &Rope, replacement: &HunkReplacement) -> Range {
    match replacement {
        HunkReplacement::Lines { start, end, .. } => {
            Range::new(Position::new(*start, 0), Position::new(*end, 0))
        }
        HunkReplacement::ThroughEnd { start, .. } => {
            Range::new(Position::new(*start, 0), rope_end(rope))
        }
        HunkReplacement::AtEnd { .. } => {
            let end = rope_end(rope);
            Range::new(end, end)
        }
    }
}

fn rope_end(rope: &Rope) -> Position {
    if rope.len_chars() == 0 {
        return Position::default();
    }
    let line = rope.len_lines() - 1;
    Position::new(line, line_visible_chars(rope, line))
}

pub fn visual_rows(line_count: usize, hunks: &[Hunk], folded: &[usize]) -> Vec<VisualRow> {
    let mut rows = Vec::new();
    let mut index = 0;
    while index < line_count {
        if let Some(hunk) = hunks.iter().find(|hunk| hunk.start == index) {
            rows.push(VisualRow::Bar { anchor: hunk.start });
            if !folded.contains(&hunk.start) {
                push_removed(&mut rows, hunk);
            }
        }
        rows.push(VisualRow::Line(index));
        index += 1;
    }
    if let Some(hunk) = hunks
        .iter()
        .find(|hunk| hunk.start == line_count && hunk.end == line_count)
    {
        rows.push(VisualRow::Bar { anchor: line_count });
        if !folded.contains(&hunk.start) {
            push_removed(&mut rows, hunk);
        }
    }
    rows
}

fn push_removed(rows: &mut Vec<VisualRow>, hunk: &Hunk) {
    for line in &hunk.removed {
        rows.push(VisualRow::Removed {
            text: line.clone(),
            anchor: hunk.start,
        });
    }
}

pub fn visual_index_for_line(rows: &[VisualRow], line: usize) -> Option<usize> {
    rows.iter()
        .position(|row| matches!(row, VisualRow::Line(index) if *index == line))
}

pub fn buffer_line_at(rows: &[VisualRow], visual_index: usize) -> Option<usize> {
    match rows.get(visual_index)? {
        VisualRow::Line(line) => Some(*line),
        VisualRow::Bar { anchor } | VisualRow::Removed { anchor, .. } => Some(*anchor),
    }
}

fn split_lines(text: &str) -> Vec<String> {
    editor_lines(text)
}

const MAX_SEARCH_HITS: usize = 400;
const MAX_SEARCH_FILE: u64 = 2 * 1024 * 1024;

pub fn spawn_workspace_search(
    root: PathBuf,
    query: String,
    generation: u64,
    tx: mpsc::UnboundedSender<WorkspaceSearch>,
) {
    tokio::spawn(async move {
        if query.trim().is_empty() {
            let _ = tx.send(WorkspaceSearch {
                generation,
                matches: Vec::new(),
                message: None,
            });
            return;
        }
        let scanned = tokio::task::spawn_blocking(move || search_workspace(&root, &query)).await;
        let _ = tx.send(WorkspaceSearch {
            generation,
            matches: scanned.unwrap_or_default(),
            message: None,
        });
    });
}

pub fn search_workspace(root: &Path, query: &str) -> Vec<SearchMatch> {
    if query.trim().is_empty() {
        return Vec::new();
    }
    let mut matches = Vec::new();
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .filter_entry(|entry| !skipped_search_name(entry.file_name()))
        .build();
    for entry in walker.flatten() {
        if matches.len() >= MAX_SEARCH_HITS {
            break;
        }
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Ok(meta) = path.metadata() else {
            continue;
        };
        if meta.len() > MAX_SEARCH_FILE {
            continue;
        }
        let Ok(bytes) = fs::read(path) else {
            continue;
        };
        if bytes.contains(&0) {
            continue;
        }
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        let lines: Vec<&str> = text.lines().collect();
        for (start, _) in find_in_document(&text, query, true) {
            if matches.len() >= MAX_SEARCH_HITS {
                break;
            }
            let line_text = lines
                .get(start.line)
                .copied()
                .unwrap_or("")
                .trim_end()
                .to_string();
            matches.push(SearchMatch {
                path: path.to_path_buf(),
                line: start.line,
                column: start.column,
                text: line_text,
            });
        }
    }
    matches
}

fn skipped_search_name(name: &std::ffi::OsStr) -> bool {
    name == ".git" || name == "node_modules"
}

pub fn find_in_document(
    text: &str,
    query: &str,
    case_insensitive: bool,
) -> Vec<(Position, Position)> {
    if query.is_empty() {
        return Vec::new();
    }
    let mut matches = Vec::new();
    let hay = if case_insensitive {
        text.to_lowercase()
    } else {
        text.to_string()
    };
    let needle = if case_insensitive {
        query.to_lowercase()
    } else {
        query.to_string()
    };
    for (line_idx, line) in hay.lines().enumerate() {
        let mut start = 0;
        while let Some(found) = line[start..].find(&needle) {
            let col = start + found;
            let char_start = line[..col].chars().count();
            let char_end = char_start + query.chars().count();
            matches.push((
                Position::new(line_idx, char_start),
                Position::new(line_idx, char_end),
            ));
            start = col + needle.len().max(1);
        }
    }
    matches
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_search_skips_node_modules() {
        let root = std::env::temp_dir().join(format!("p0-search-{}", uuid::Uuid::new_v4()));
        let source = root.join("src");
        let vendor = root.join("node_modules").join("pkg");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&vendor).unwrap();
        std::fs::write(
            source.join("App.tsx"),
            "export const MainButtonProvider = 1;\n",
        )
        .unwrap();
        std::fs::write(
            vendor.join("index.js"),
            "export const MainButtonProvider = 2;\n",
        )
        .unwrap();

        let found = search_workspace(&root, "MainButtonProvider");
        let _ = std::fs::remove_dir_all(&root);

        assert_eq!(found.len(), 1);
        assert!(found[0].path.ends_with("src/App.tsx"));
        assert_eq!(found[0].line, 0);
        assert!(found[0].text.contains("MainButtonProvider"));
    }

    #[test]
    fn finds_unicode_and_multiple() {
        let text = "café café\nnaïve";
        let found = find_in_document(text, "café", true);
        assert_eq!(found.len(), 2);
        let none = find_in_document(text, "zzz", true);
        assert!(none.is_empty());
    }

    #[test]
    fn parses_porcelain_statuses() {
        let root = Path::new("/repo");
        let statuses = parse_git_status(
            root,
            b" M src/main.ts\0M  src/staged.ts\0MM src/both.ts\0A  src/new.ts\0D  old.ts\0 D gone.ts\0?? notes.txt\0R  from.ts\0to.ts\0",
        );
        let main = statuses.get(&root.join("src/main.ts")).copied().unwrap();
        assert_eq!(main.staged, None);
        assert_eq!(main.unstaged, Some(GitFileStatus::Modified));
        assert_eq!(main.summary(), Some(GitFileStatus::Modified));
        let staged = statuses.get(&root.join("src/staged.ts")).copied().unwrap();
        assert_eq!(staged.staged, Some(GitFileStatus::Modified));
        assert_eq!(staged.unstaged, None);
        let both = statuses.get(&root.join("src/both.ts")).copied().unwrap();
        assert_eq!(both.staged, Some(GitFileStatus::Modified));
        assert_eq!(both.unstaged, Some(GitFileStatus::Modified));
        assert_eq!(
            statuses.get(&root.join("src/new.ts")).unwrap().staged,
            Some(GitFileStatus::Added)
        );
        assert_eq!(
            statuses.get(&root.join("old.ts")).unwrap().staged,
            Some(GitFileStatus::Deleted)
        );
        assert_eq!(
            statuses.get(&root.join("gone.ts")).unwrap().unstaged,
            Some(GitFileStatus::Deleted)
        );
        assert_eq!(
            statuses.get(&root.join("notes.txt")).unwrap().unstaged,
            Some(GitFileStatus::Untracked)
        );
        assert!(statuses.get(&root.join("from.ts")).is_none());
        assert_eq!(
            statuses.get(&root.join("to.ts")).unwrap().staged,
            Some(GitFileStatus::Renamed)
        );
    }

    #[test]
    fn hunk_patch_keeps_the_file_header() {
        let text = "\
diff --git a/src/main.rs b/src/main.rs
index 111..222 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,3 +1,3 @@
 keep
-old
+new
 keep
@@ -10,2 +10,2 @@
 a
-b
+c
";
        let parsed = parse_git_diff(text);
        assert_eq!(parsed.hunks.len(), 2);
        let patch = hunk_patch(&parsed, 1).unwrap();
        assert!(patch.contains("diff --git a/src/main.rs b/src/main.rs"));
        assert!(patch.contains("--- a/src/main.rs"));
        assert!(patch.contains("+++ b/src/main.rs"));
        assert!(patch.contains("@@ -10,2 +10,2 @@"));
        assert!(!patch.contains("@@ -1,3 +1,3 @@"));
        assert!(patch.contains("-b\n"));
        assert!(patch.contains("+c\n"));
        assert!(patch.ends_with("+c\n"));
    }

    #[test]
    fn shows_every_markdown_line_hidden_inside_a_git_line() {
        let text = "\
diff --git a/app/readme.md b/app/readme.md
new file mode 100644
--- /dev/null
+++ b/app/readme.md
@@ -0,0 +1,1 @@
+# r3vu\rA mouse-driven terminal IDE\r```bash
\\ No newline at end of file
";
        let parsed = parse_git_diff(text);
        assert_eq!(parsed.hunks[0].header, "@@ -0,0 +1,1 @@");
        let patch = hunk_patch(&parsed, 0).unwrap();
        assert!(patch.contains("@@ -0,0 +1,1 @@"));
        assert!(patch.contains("+# r3vu\rA mouse-driven terminal IDE\r```bash\n"));

        let lines = lines_from_git_diff(&parsed);
        let header = lines.iter().find(|line| line.hunk == Some(0)).unwrap();
        assert_eq!(header.text, "@@ -0,0 +1,3 @@");
        let inserts: Vec<_> = lines
            .iter()
            .filter(|line| line.kind == DiffKind::Insert)
            .map(|line| line.text.as_str())
            .collect();
        assert_eq!(
            inserts,
            vec!["+# r3vu", "+A mouse-driven terminal IDE", "+```bash",]
        );
        assert!(
            lines
                .iter()
                .any(|line| line.text == "\\ No newline at end of file")
        );

        let crlf = ParsedGitDiff {
            preamble: Vec::new(),
            hunks: vec![GitHunk {
                header: "@@ -0,0 +1 @@".into(),
                lines: vec!["+keep\r".into()],
            }],
        };
        let crlf_lines = lines_from_git_diff(&crlf);
        let crlf_inserts: Vec<_> = crlf_lines
            .iter()
            .filter(|line| line.kind == DiffKind::Insert)
            .map(|line| line.text.as_str())
            .collect();
        assert_eq!(crlf_inserts, vec!["+keep"]);
        assert_eq!(
            crlf_lines
                .iter()
                .find(|line| line.hunk == Some(0))
                .unwrap()
                .text,
            "@@ -0,0 +1 @@"
        );
        assert!(hunk_patch(&crlf, 0).unwrap().contains("+keep\r\n"));

        let cr = "\
diff --git a/readme.md b/readme.md
--- a/readme.md
+++ b/readme.md
@@ -1 +1 @@
-# r3vu\r\rA mouse.\r
\\ No newline at end of file
+# r3vu
+
+A mouse.
";
        let parsed_cr = parse_git_diff(cr);
        let cr_patch = hunk_patch(&parsed_cr, 0).unwrap();
        assert!(
            cr_patch.contains("-# r3vu\r\rA mouse.\r\n"),
            "stage patch dropped a carriage return: {cr_patch:?}"
        );
        let blank = ParsedGitDiff {
            preamble: Vec::new(),
            hunks: vec![GitHunk {
                header: "@@ -1,2 +1,2 @@".into(),
                lines: vec![" keep\r".into(), "+\r".into()],
            }],
        };
        let blank_rows: Vec<_> = lines_from_git_diff(&blank)
            .into_iter()
            .filter(|line| line.hunk.is_none() && line.kind != DiffKind::Header)
            .map(|line| line.text)
            .collect();
        assert_eq!(blank_rows, vec![" keep", "+"]);

        let (untracked, parsed_new) = untracked_diff(
            "app/readme.md",
            "# r3vu\rA mouse-driven terminal IDE\r```bash",
        );
        let untracked_inserts: Vec<_> = untracked
            .iter()
            .filter(|line| line.kind == DiffKind::Insert)
            .map(|line| line.text.as_str())
            .collect();
        assert_eq!(untracked_inserts, inserts);
        assert!(untracked.iter().any(|line| line.hunk == Some(0)));
        let new_patch = hunk_patch(parsed_new.as_ref().unwrap(), 0).unwrap();
        assert!(new_patch.contains("+++ b/app/readme.md\n"));
        assert!(new_patch.contains("+# r3vu\rA mouse-driven terminal IDE\r```bash\n"));
        assert!(new_patch.contains("\\ No newline at end of file\n"));

        let (lf_lines, lf_parsed) = untracked_diff("app/readme.md", "# r3vu\n\nA mouse.\n");
        assert!(lf_lines.iter().any(|line| line.hunk == Some(0)));
        let lf_patch = hunk_patch(lf_parsed.as_ref().unwrap(), 0).unwrap();
        assert!(lf_patch.contains("@@ -0,0 +1,3 @@\n+# r3vu\n+\n+A mouse.\n"));
        assert!(!lf_patch.contains("No newline"));
        let (empty_lines, empty_parsed) = untracked_diff("app/readme.md", "");
        assert!(empty_parsed.is_none());
        assert!(empty_lines.iter().all(|line| line.hunk.is_none()));

        let marks = classify_lines("", "# r3vu\rA mouse-driven terminal IDE\r```bash");
        assert_eq!(
            marks.marks,
            vec![LineMark::Added, LineMark::Added, LineMark::Added]
        );
    }

    #[test]
    fn rejects_a_path_that_escapes_the_project() {
        let root = Path::new("/repo");
        assert_eq!(
            repo_relative(root, Path::new("/repo/src/main.rs")).as_deref(),
            Some("src/main.rs")
        );
        assert_eq!(
            repo_relative(root, Path::new("src/main.rs")).as_deref(),
            Some("src/main.rs")
        );
        assert!(repo_relative(root, Path::new("/etc/passwd")).is_none());
        assert!(repo_relative(root, Path::new("")).is_none());
        assert!(repo_relative(root, Path::new("..")).is_none());
        assert!(repo_relative(root, Path::new("src/../../etc/passwd")).is_none());
        assert!(repo_relative(root, root).is_none());
        let escaped =
            prepare_git_write(root, &GitWrite::Stage(PathBuf::from("/repo/../etc/passwd")));
        assert!(escaped.is_err());
        let commit = prepare_git_write(
            root,
            &GitWrite::Commit {
                message: "  ".into(),
                body: "notes".into(),
            },
        );
        assert!(commit.is_err());
        let ok = prepare_git_write(
            root,
            &GitWrite::Commit {
                message: "Keep the review".into(),
                body: "  ".into(),
            },
        )
        .unwrap();
        assert_eq!(ok.args, ["commit", "-m", "Keep the review"]);
        let with_body = prepare_git_write(
            root,
            &GitWrite::Commit {
                message: "Keep the review".into(),
                body: " Explain why.\nSecond line ".into(),
            },
        )
        .unwrap();
        assert_eq!(
            with_body.args,
            [
                "commit",
                "-m",
                "Keep the review",
                "-m",
                "Explain why.\nSecond line"
            ]
        );
        assert!(
            prepare_git_write(
                root,
                &GitWrite::Commit {
                    message: "Keep the review".into(),
                    body: "bad\0body".into(),
                },
            )
            .is_err()
        );
        assert!(!ok.args.iter().any(|arg| arg == "--no-verify"));
        let pull = prepare_git_write(root, &GitWrite::Pull).unwrap();
        assert_eq!(pull.args, ["pull", "--ff-only"]);
        let push = prepare_git_write(root, &GitWrite::Push).unwrap();
        assert_eq!(push.args, ["push"]);
        let publish = prepare_git_write(root, &GitWrite::Publish("main".into())).unwrap();
        assert_eq!(publish.args, ["push", "-u", "origin", "main"]);
        assert!(prepare_git_write(root, &GitWrite::Publish(String::new())).is_err());
        assert!(prepare_git_write(root, &GitWrite::Publish("--force".into())).is_err());
        assert_eq!(branch_chip("main", Some((2, 1))), "main ↑2 ↓1");
        assert_eq!(branch_chip("main", None), "main");
        assert_eq!(parse_ahead_behind(b"2\t1\n"), Some((2, 1)));
        assert_eq!(parse_ahead_behind(b"0 0\n"), Some((0, 0)));
        assert!(parse_ahead_behind(b"nope").is_none());
    }

    #[test]
    fn restore_and_remove_stay_inside_the_project() {
        let root = Path::new("/repo");
        let restore =
            prepare_git_write(root, &GitWrite::Restore(PathBuf::from("/repo/src/a.ts"))).unwrap();
        assert_eq!(
            restore.args,
            [
                "restore",
                "--source=HEAD",
                "--staged",
                "--worktree",
                "--",
                "src/a.ts"
            ]
        );
        let remove =
            prepare_git_write(root, &GitWrite::Remove(PathBuf::from("/repo/src/a.ts"))).unwrap();
        assert_eq!(remove.args, ["rm", "-f", "--", "src/a.ts"]);
        assert!(
            prepare_git_write(
                root,
                &GitWrite::Restore(PathBuf::from("/repo/../etc/passwd"))
            )
            .is_err()
        );
        assert!(
            prepare_git_write(root, &GitWrite::Remove(PathBuf::from("../etc/passwd"))).is_err()
        );
        assert_eq!(
            project_path(root, "src/main.rs").as_deref(),
            Some(Path::new("/repo/src/main.rs"))
        );
        assert!(project_path(root, "../etc/passwd").is_none());
        assert!(project_path(root, "/etc/passwd").is_none());
        assert!(project_path(root, "src/../../etc/passwd").is_none());
    }

    #[test]
    fn status_delta_notices_appeared_and_changed_paths() {
        let before = HashMap::from([(
            PathBuf::from("/repo/old.ts"),
            GitPathStatus {
                staged: None,
                unstaged: Some(GitFileStatus::Modified),
            },
        )]);
        let after = HashMap::from([
            (
                PathBuf::from("/repo/old.ts"),
                GitPathStatus {
                    staged: Some(GitFileStatus::Modified),
                    unstaged: None,
                },
            ),
            (
                PathBuf::from("/repo/new.ts"),
                GitPathStatus {
                    staged: None,
                    unstaged: Some(GitFileStatus::Untracked),
                },
            ),
        ]);
        assert_eq!(
            status_delta(&before, &after),
            vec![PathBuf::from("/repo/new.ts"), PathBuf::from("/repo/old.ts")]
        );
        assert_eq!(
            whole_file_action(GitPathStatus {
                staged: None,
                unstaged: Some(GitFileStatus::Modified),
            }),
            Some(WholeFileAction::Restore)
        );
        assert_eq!(
            whole_file_action(GitPathStatus {
                staged: Some(GitFileStatus::Deleted),
                unstaged: None,
            }),
            Some(WholeFileAction::Restore)
        );
        assert_eq!(
            whole_file_action(GitPathStatus {
                staged: Some(GitFileStatus::Added),
                unstaged: None,
            }),
            Some(WholeFileAction::Remove)
        );
        assert_eq!(
            whole_file_action(GitPathStatus {
                staged: None,
                unstaged: Some(GitFileStatus::Untracked),
            }),
            Some(WholeFileAction::Delete)
        );
        assert_eq!(
            whole_file_action(GitPathStatus {
                staged: Some(GitFileStatus::Renamed),
                unstaged: None,
            }),
            None
        );
    }

    #[test]
    fn commit_needs_a_message_and_a_staged_file() {
        let mut files = HashMap::new();
        assert!(!can_commit("Keep it", &files));
        assert!(!can_commit("  ", &files));
        files.insert(
            PathBuf::from("a.ts"),
            GitPathStatus {
                staged: None,
                unstaged: Some(GitFileStatus::Modified),
            },
        );
        assert!(!can_commit("Keep it", &files));
        files.insert(
            PathBuf::from("b.ts"),
            GitPathStatus {
                staged: Some(GitFileStatus::Added),
                unstaged: None,
            },
        );
        assert!(!can_commit("   ", &files));
        assert!(can_commit("Keep it", &files));
    }

    #[test]
    fn panel_lists_a_partial_file_on_both_sides() {
        let mut files = HashMap::new();
        files.insert(
            PathBuf::from("both.ts"),
            GitPathStatus {
                staged: Some(GitFileStatus::Modified),
                unstaged: Some(GitFileStatus::Modified),
            },
        );
        files.insert(
            PathBuf::from("new.ts"),
            GitPathStatus {
                staged: None,
                unstaged: Some(GitFileStatus::Untracked),
            },
        );
        files.insert(
            PathBuf::from("done.ts"),
            GitPathStatus {
                staged: Some(GitFileStatus::Added),
                unstaged: None,
            },
        );
        let rows = git_panel_rows(&files);
        let staged: Vec<_> = rows
            .iter()
            .filter_map(|row| match row {
                GitPanelRow::File {
                    path, staged: true, ..
                } => Some(path.clone()),
                _ => None,
            })
            .collect();
        let changed: Vec<_> = rows
            .iter()
            .filter_map(|row| match row {
                GitPanelRow::File {
                    path,
                    staged: false,
                    ..
                } => Some(path.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            staged,
            vec![PathBuf::from("both.ts"), PathBuf::from("done.ts")]
        );
        assert_eq!(
            changed,
            vec![PathBuf::from("both.ts"), PathBuf::from("new.ts")]
        );
    }

    #[test]
    fn classifies_and_shows_removed_lines() {
        let changes = classify_lines("alpha\nbeta\ngamma\n", "alpha\nBETA\nextra\ngamma\n");
        assert_eq!(changes.marks[1], LineMark::Modified);
        assert_eq!(changes.marks[2], LineMark::Added);
        let hunks = change_hunks(&changes);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].start, 1);
        assert_eq!(hunks[0].end, 3);
        assert_eq!(hunks[0].removed, vec!["beta".to_string()]);
        let closed = visual_rows(changes.marks.len(), &hunks, &[]);
        assert_eq!(closed[0], VisualRow::Line(0));
        assert_eq!(closed[1], VisualRow::Bar { anchor: 1 });
        assert_eq!(
            closed[2],
            VisualRow::Removed {
                text: "beta".into(),
                anchor: 1,
            }
        );
        assert_eq!(closed[3], VisualRow::Line(1));
        assert_eq!(closed[4], VisualRow::Line(2));
        assert_eq!(buffer_line_at(&closed, 0), Some(0));
        assert_eq!(buffer_line_at(&closed, 2), Some(1));
        assert_eq!(buffer_line_at(&closed, 5), Some(3));
        assert_eq!(visual_index_for_line(&closed, 3), Some(5));
        let folded = visual_rows(changes.marks.len(), &hunks, &[1]);
        assert!(
            folded
                .iter()
                .any(|row| matches!(row, VisualRow::Bar { anchor: 1 }))
        );
        assert!(
            folded
                .iter()
                .all(|row| !matches!(row, VisualRow::Removed { .. }))
        );
        assert!(folded.iter().any(|row| matches!(row, VisualRow::Line(1))));
    }

    #[test]
    fn shows_a_row_for_a_deletion_at_the_end() {
        let changes = classify_lines("keep\ngone", "keep");
        let hunks = change_hunks(&changes);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].start, 1);
        let rows = visual_rows(1, &hunks, &[]);
        assert_eq!(rows[0], VisualRow::Line(0));
        assert_eq!(rows[1], VisualRow::Bar { anchor: 1 });
        assert_eq!(
            rows[2],
            VisualRow::Removed {
                text: "gone".into(),
                anchor: 1,
            }
        );
        assert_eq!(buffer_line_at(&rows, 2), Some(1));
    }

    #[test]
    fn revert_restores_one_hunk() {
        assert_eq!(
            restored("alpha\nbeta\ngamma\n", "alpha\nBETA\ngamma\n"),
            "alpha\nbeta\ngamma\n"
        );
        assert_eq!(restored("alpha\nbeta", "alpha\nBETA"), "alpha\nbeta");
        assert_eq!(
            restored("alpha\ngamma\n", "alpha\nextra\ngamma\n"),
            "alpha\ngamma\n"
        );
        assert_eq!(
            restored("keep\ngone\nstay\n", "keep\nstay\n"),
            "keep\ngone\nstay\n"
        );
        assert_eq!(restored("a\n\nb\n", "a\nb\n"), "a\n\nb\n");
        assert_eq!(restored("keep\ngone", "keep"), "keep\ngone");
    }

    #[test]
    fn revert_edit_undoes_in_one_step() {
        let original = "alpha\nbeta\n";
        let current = "alpha\nBETA\n";
        let mut doc =
            crate::editor::Document::new(crate::editor::DocumentId(1), None, current.into());
        let changes = classify_lines(original, current);
        let hunks = change_hunks(&changes);
        let replacement = hunk_replacement(&hunks[0], changes.marks.len(), true);
        let range = hunk_edit_range(&doc.rope, &replacement);
        doc.apply_replace(range, replacement.text(), false);
        assert_eq!(doc.text(), original);
        doc.undo().unwrap();
        assert_eq!(doc.text(), current);
    }

    fn restored(original: &str, current: &str) -> String {
        let changes = classify_lines(original, current);
        let hunks = change_hunks(&changes);
        assert_eq!(hunks.len(), 1);
        let replacement = hunk_replacement(&hunks[0], changes.marks.len(), current.ends_with('\n'));
        let mut doc =
            crate::editor::Document::new(crate::editor::DocumentId(1), None, current.to_string());
        let range = hunk_edit_range(&doc.rope, &replacement);
        doc.apply_replace(range, replacement.text(), false);
        doc.text()
    }

    #[test]
    fn marks_deleted_lines_before_the_next_line() {
        let changes = classify_lines("keep\ngone\n", "keep\n");
        assert_eq!(changes.marks[0], LineMark::Unchanged);
        assert!(changes.deleted_before.iter().any(|count| *count > 0));
    }

    #[test]
    fn empty_baseline_marks_every_line_added() {
        let changes = classify_lines("", "alpha\nbeta");
        assert_eq!(changes.marks, vec![LineMark::Added, LineMark::Added]);
        assert!(changes.deleted_before.iter().all(|count| *count == 0));
    }

    #[test]
    fn review_keeps_staged_and_unstaged_on_one_bar() {
        let head = "alpha\nbeta\ngamma\n";
        let index = "alpha\nBETA\ngamma\n";
        let staged = review_changes(head, index, index);
        assert_eq!(staged.marks[1], LineMark::Modified);
        assert_eq!(staged.sides[1], ChangeSide::Staged);
        assert_eq!(staged.removed_before[1], vec!["beta".to_string()]);
        let hunks = present_hunks(&staged, false, false);
        assert_eq!(hunks[0].bar, HunkBar::Unstage);

        let mixed = review_changes(head, index, "alpha\nX\ngamma\n");
        assert_eq!(mixed.sides[1], ChangeSide::Unstaged);
        assert!(mixed.staged_under[1]);
        assert_eq!(mixed.removed_before[1], vec!["BETA".to_string()]);
        assert_eq!(
            present_hunks(&mixed, false, false)[0].bar,
            HunkBar::StageUnstageDiscard
        );
        assert_eq!(present_hunks(&mixed, true, false)[0].bar, HunkBar::Revert);

        let edited = review_changes(head, head, "alpha\nBETA\ngamma\n");
        assert_eq!(
            present_hunks(&edited, false, false)[0].bar,
            HunkBar::StageDiscard
        );
        assert_eq!(present_hunks(&edited, true, false)[0].bar, HunkBar::Revert);
        assert_eq!(present_hunks(&edited, false, true)[0].bar, HunkBar::Renamed);
    }

    #[test]
    fn review_shows_a_staged_deletion() {
        let changes = review_changes("keep\ngone\n", "keep\n", "keep\n");
        let hunks = present_hunks(&changes, false, false);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].removed, vec!["gone".to_string()]);
        assert_eq!(hunks[0].bar, HunkBar::Unstage);
        let rows = visual_rows(changes.marks.len(), &hunks, &[]);
        assert!(rows.iter().any(|row| matches!(
            row,
            VisualRow::Removed { text, .. } if text == "gone"
        )));
    }

    #[test]
    fn patch_touching_selects_the_overlapping_hunk() {
        let parsed = parse_git_diff(
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -2,1 +2,1 @@\n-beta\n+BETA\n@@ -8,0 +8,1 @@\n+tail\n",
        );
        let first = patch_touching(&parsed, 1, 2).unwrap();
        assert!(first.contains("@@ -2,1 +2,1 @@"));
        assert!(!first.contains("+tail"));
        let deletion = parse_git_diff("@@ -4,1 +4,0 @@\n-gone\n");
        assert!(patch_touching(&deletion, 3, 3).unwrap().contains("-gone"));
    }

    #[test]
    fn folder_mark_prefers_modified_and_stays_inside_the_directory() {
        let root = Path::new("/repo");
        let mut statuses = HashMap::new();
        statuses.insert(
            root.join("src/notes.txt"),
            GitPathStatus {
                staged: None,
                unstaged: Some(GitFileStatus::Untracked),
            },
        );
        statuses.insert(
            root.join("src/main.ts"),
            GitPathStatus {
                staged: None,
                unstaged: Some(GitFileStatus::Modified),
            },
        );
        statuses.insert(
            root.join("src2/other.ts"),
            GitPathStatus {
                staged: Some(GitFileStatus::Added),
                unstaged: None,
            },
        );
        assert_eq!(
            directory_status(&statuses, &root.join("src")),
            Some(DirectoryStatus {
                kind: GitFileStatus::Modified,
                staged: false,
            })
        );
        assert_eq!(
            directory_status(&statuses, &root.join("src2")),
            Some(DirectoryStatus {
                kind: GitFileStatus::Added,
                staged: true,
            })
        );
        assert_eq!(directory_status(&statuses, &root.join("empty")), None);
    }

    #[test]
    fn classifies_watch_events_and_drops_noise() {
        use notify::event::{AccessKind, CreateKind, DataChange, RemoveKind, RenameMode};

        assert_eq!(
            classify_watch_kind(EventKind::Create(CreateKind::File)),
            Some(WatchKind::Create)
        );
        assert_eq!(
            classify_watch_kind(EventKind::Remove(RemoveKind::File)),
            Some(WatchKind::Remove)
        );
        assert_eq!(
            classify_watch_kind(EventKind::Modify(ModifyKind::Data(DataChange::Content))),
            Some(WatchKind::Modify)
        );
        assert_eq!(
            classify_watch_kind(EventKind::Modify(ModifyKind::Name(RenameMode::Any))),
            Some(WatchKind::Create)
        );
        assert_eq!(classify_watch_kind(EventKind::Any), Some(WatchKind::Modify));
        assert_eq!(
            classify_watch_kind(EventKind::Other),
            Some(WatchKind::Create)
        );
        assert_eq!(
            classify_watch_kind(EventKind::Access(AccessKind::Read)),
            None
        );
        assert!(WatchKind::Create.changes_tree());
        assert!(WatchKind::Remove.changes_tree());
        assert!(!WatchKind::Modify.changes_tree());

        let root = Path::new("/repo");
        assert!(is_watch_noise(&root.join(".git/index")));
        assert!(is_watch_noise(&root.join(".git/index.lock")));
        assert!(is_watch_noise(&root.join("index.lock")));
        assert!(!is_watch_noise(&root.join("src/main.rs")));
    }

    #[test]
    fn watch_filter_skips_gitignored_paths() {
        struct TempDir(PathBuf);
        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        let temp = TempDir(std::env::temp_dir().join(format!("p0-watch-{}", uuid::Uuid::new_v4())));
        std::fs::create_dir_all(temp.0.join(".git")).unwrap();
        std::fs::create_dir_all(temp.0.join("target")).unwrap();
        std::fs::create_dir_all(temp.0.join("src")).unwrap();
        std::fs::write(temp.0.join(".gitignore"), "/target\n*.p0tmp\n").unwrap();
        std::fs::write(temp.0.join("target/out"), "x").unwrap();
        std::fs::write(temp.0.join("src/main.rs"), "x").unwrap();
        std::fs::write(temp.0.join("notes.p0tmp"), "x").unwrap();
        let root = temp.0.canonicalize().unwrap();
        let mut ignore = watch_matcher(&root);
        assert!(accept_watch_path(
            &root,
            &root.join("src/main.rs"),
            &mut ignore
        ));
        assert!(!accept_watch_path(
            &root,
            &root.join("target/out"),
            &mut ignore
        ));
        assert!(!accept_watch_path(
            &root,
            &root.join("notes.p0tmp"),
            &mut ignore
        ));
        assert!(!accept_watch_path(
            &root,
            &root.join(".git/HEAD"),
            &mut ignore
        ));
    }

    #[test]
    fn tells_a_missing_repository_from_other_git_failures() {
        assert_eq!(
            git_status_error(
                "fatal: not a git repository (or any of the parent directories): .git"
            ),
            GitStatusError::NotARepository
        );
        assert_eq!(
            git_status_error("fatal: unable to read current working directory"),
            GitStatusError::Unavailable
        );
    }

    #[test]
    fn detects_paths_missing_from_head() {
        assert!(path_absent_from_head(
            "path 'src/new.ts' exists on disk, but not in 'HEAD'"
        ));
        assert!(path_absent_from_head(
            "path 'src/new.ts' does not exist in 'HEAD'"
        ));
        assert!(path_absent_from_head(
            "path 'src/new.ts' exists on disk, but not in the index"
        ));
        assert!(!path_absent_from_head("not a git repository"));
    }

    #[test]
    fn parses_committed_and_uncommitted_blame() {
        let committed = "\
f1d2d2f1d2d2f1d2d2f1d2d2f1d2d2f1d2d2f1 10 20 1
author Ada Lovelace
author-mail <ada@example.com>
author-time 1700000000
author-tz +0000
committer Ada Lovelace
committer-mail <ada@example.com>
committer-time 1700000000
committer-tz +0000
summary First program
previous abcdefabcdefabcdefabcdefabcdefabcdefabcd src/main.rs
filename src/main.rs
\tlet value = 1;
";
        let blame = parse_line_blame(committed).unwrap();
        assert_eq!(blame.author, "Ada Lovelace");
        assert_eq!(blame.summary, "First program");
        assert_eq!(blame.time, 1_700_000_000);
        assert!(!blame.uncommitted);
        assert_eq!(format_unix_time(blame.time), "2023-11-14 22:13");

        let uncommitted = "\
0000000000000000000000000000000000000000 1 1 1
author Not Committed Yet
author-mail <not.committed.yet>
author-time 1700000000
author-tz +0000
committer Not Committed Yet
committer-mail <not.committed.yet>
committer-time 1700000000
committer-tz +0000
summary
filename src/main.rs
\tlet value = 2;
";
        let blame = parse_line_blame(uncommitted).unwrap();
        assert!(blame.uncommitted);
        assert_eq!(blame.author, "Not Committed Yet");
        assert!(parse_line_blame("not blame").is_none());
        assert_eq!(format_unix_time(0), "1970-01-01 00:00");
    }
}
