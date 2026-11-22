#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

use crate::editor::{
    Document, DocumentChange, LanguageId, Position, position_to_utf16, utf16_to_column,
};

#[derive(Clone, Debug)]
pub enum LspStatus {
    Unavailable(String),
    Starting,
    Ready,
    Error(String),
}

#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub path: PathBuf,
    pub range_start: Position,
    pub range_end: Position,
    pub severity: u8,
    pub message: String,
}

pub fn diagnostic_label(severity: u8) -> &'static str {
    match severity {
        2 => "Warning",
        3 => "Info",
        4 => "Hint",
        _ => "Error",
    }
}

pub fn diagnostic_status(item: &Diagnostic) -> String {
    let message = item.message.replace(['\n', '\r'], " ");
    format!("{} {message}", diagnostic_label(item.severity))
}

pub fn diagnostic_at<'a>(items: &'a [Diagnostic], pos: Position) -> Option<&'a Diagnostic> {
    items
        .iter()
        .filter(|item| diagnostic_contains(item, pos))
        .min_by_key(|item| severity_rank(item.severity))
}

fn severity_rank(severity: u8) -> u8 {
    match severity {
        1..=4 => severity,
        _ => 1,
    }
}

fn diagnostic_contains(item: &Diagnostic, pos: Position) -> bool {
    let start = item.range_start;
    let end = if item.range_end > start {
        item.range_end
    } else {
        Position::new(start.line, start.column.saturating_add(1))
    };
    pos >= start && pos < end
}

#[derive(Clone, Debug)]
pub struct CompletionEdit {
    pub start: Position,
    pub end: Position,
    pub new_text: String,
}

#[derive(Clone, Debug)]
pub struct CompletionItem {
    pub label: String,
    pub detail: Option<String>,
    pub insert: String,
    pub filter_text: String,
    pub sort_text: String,
    pub kind: Option<u8>,
    pub edit: Option<CompletionEdit>,
}

#[derive(Clone, Debug)]
pub struct Location {
    pub path: PathBuf,
    pub position: Position,
}

#[derive(Clone, Debug)]
pub struct TextEdit {
    pub path: PathBuf,
    pub start: Position,
    pub end: Position,
    pub new_text: String,
}

#[derive(Debug)]
pub enum LspCmd {
    Initialize {
        root: PathBuf,
    },
    DidOpen {
        path: PathBuf,
        language: LanguageId,
        text: String,
        version: i64,
    },
    DidChange {
        path: PathBuf,
        version: i64,
        change: DocumentChange,
        full_text: String,
        incremental: bool,
    },
    DidClose {
        path: PathBuf,
    },
    Completion {
        token: u64,
        path: PathBuf,
        position: Position,
    },
    Hover {
        token: u64,
        path: PathBuf,
        position: Position,
    },
    Signature {
        token: u64,
        path: PathBuf,
        position: Position,
    },
    Definition {
        token: u64,
        path: PathBuf,
        position: Position,
    },
    References {
        token: u64,
        path: PathBuf,
        position: Position,
    },
    Rename {
        token: u64,
        path: PathBuf,
        position: Position,
        new_name: String,
    },
    WillRename {
        token: u64,
        from: PathBuf,
        to: PathBuf,
    },
    Restart {
        root: PathBuf,
        server: ServerKind,
    },
    Shutdown,
}

#[derive(Debug)]
pub enum LspEvent {
    Status {
        server: ServerKind,
        status: LspStatus,
    },
    Diagnostics {
        path: PathBuf,
        items: Vec<Diagnostic>,
    },
    Completion {
        token: u64,
        items: Vec<CompletionItem>,
    },
    Hover {
        token: u64,
        text: String,
    },
    Signature {
        token: u64,
        text: String,
    },
    Locations {
        token: u64,
        items: Vec<Location>,
    },
    Edits {
        token: u64,
        items: Vec<TextEdit>,
    },
    FileRename {
        token: u64,
        items: Vec<TextEdit>,
        failed: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ServerKind {
    TypeScript,
    Python,
    Java,
}

impl ServerKind {
    pub fn all() -> [Self; 3] {
        [Self::TypeScript, Self::Python, Self::Java]
    }
}

pub const TYPES_INSTALL: &str = "npm install -g typescript typescript-language-server";
pub const PYTHON_INSTALL: &str = "npm install -g pyright";
pub const JAVA_INSTALL: &str = "brew install jdtls";

pub fn server_for(language: LanguageId) -> ServerKind {
    match language {
        LanguageId::Python => ServerKind::Python,
        LanguageId::Java => ServerKind::Java,
        _ => ServerKind::TypeScript,
    }
}

pub fn server_for_path(path: &Path) -> ServerKind {
    server_for(LanguageId::from_path(path))
}

pub fn server_binary(server: ServerKind) -> &'static str {
    match server {
        ServerKind::TypeScript => "typescript-language-server",
        ServerKind::Python => "pyright-langserver",
        ServerKind::Java => "jdtls",
    }
}

pub fn server_requirement(server: ServerKind) -> &'static str {
    match server {
        ServerKind::TypeScript => "typescript-language-server on PATH.",
        ServerKind::Python => "pyright-langserver on PATH.",
        ServerKind::Java => "jdtls on PATH.",
    }
}

pub fn install_command(server: ServerKind) -> &'static str {
    match server {
        ServerKind::TypeScript => TYPES_INSTALL,
        ServerKind::Python => PYTHON_INSTALL,
        ServerKind::Java => JAVA_INSTALL,
    }
}

pub fn server_installed(server: ServerKind) -> bool {
    which::which(server_binary(server)).is_ok()
}

#[derive(Clone, Debug)]
pub struct LspServers {
    pub typescript: LspStatus,
    pub python: LspStatus,
    pub java: LspStatus,
}

impl LspServers {
    pub fn starting() -> Self {
        Self {
            typescript: LspStatus::Starting,
            python: LspStatus::Starting,
            java: LspStatus::Starting,
        }
    }

    pub fn get(&self, server: ServerKind) -> &LspStatus {
        match server {
            ServerKind::TypeScript => &self.typescript,
            ServerKind::Python => &self.python,
            ServerKind::Java => &self.java,
        }
    }

    pub fn get_mut(&mut self, server: ServerKind) -> &mut LspStatus {
        match server {
            ServerKind::TypeScript => &mut self.typescript,
            ServerKind::Python => &mut self.python,
            ServerKind::Java => &mut self.java,
        }
    }
}

impl Default for LspServers {
    fn default() -> Self {
        Self::starting()
    }
}

pub struct LanguageClient {
    pub tx: mpsc::UnboundedSender<LspCmd>,
}

impl LanguageClient {
    pub fn spawn(event_tx: mpsc::UnboundedSender<LspEvent>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(run_client(rx, event_tx));
        Self { tx }
    }
}

struct Server {
    child: Child,
    writer: tokio::process::ChildStdin,
    incremental: bool,
    next_id: i64,
    pending: HashMap<i64, Pending>,
}

struct Session {
    server: Option<Server>,
    open_docs: HashMap<PathBuf, (LanguageId, String, i64)>,
    restarts: u32,
    generation: u64,
    launch_attempted: bool,
}

impl Session {
    fn new() -> Self {
        Self {
            server: None,
            open_docs: HashMap::new(),
            restarts: 0,
            generation: 0,
            launch_attempted: false,
        }
    }
}

struct Incoming {
    kind: ServerKind,
    generation: u64,
    msg: Value,
}

enum Pending {
    Completion(u64),
    Hover(u64),
    Signature(u64),
    Definition(u64),
    References(u64),
    Rename(u64),
    WillRename(u64),
    Initialize,
}

async fn run_client(
    mut rx: mpsc::UnboundedReceiver<LspCmd>,
    events: mpsc::UnboundedSender<LspEvent>,
) {
    let mut sessions: HashMap<ServerKind, Session> = HashMap::new();
    for kind in ServerKind::all() {
        sessions.insert(kind, Session::new());
    }
    let mut last_root: Option<PathBuf> = None;
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Incoming>();

    loop {
        tokio::select! {
            cmd = rx.recv() => {
                let Some(cmd) = cmd else { break; };
                match cmd {
                    LspCmd::Shutdown => {
                        for session in sessions.values_mut() {
                            if let Some(mut server) = session.server.take() {
                                let _ = write_rpc(&mut server.writer, json!({"jsonrpc":"2.0","id":server.next_id,"method":"shutdown","params":null})).await;
                                let _ = server.child.kill().await;
                            }
                        }
                        break;
                    }
                    LspCmd::Initialize { root } => {
                        last_root = Some(root.clone());
                        launch(&mut sessions, ServerKind::TypeScript, &root, &events, &out_tx).await;
                        for kind in [ServerKind::Python, ServerKind::Java] {
                            let pending = sessions.get(&kind).is_some_and(|session| {
                                session.server.is_none()
                                    && !session.launch_attempted
                                    && !session.open_docs.is_empty()
                            });
                            if pending {
                                launch(&mut sessions, kind, &root, &events, &out_tx).await;
                            }
                        }
                    }
                    LspCmd::Restart { root, server } => {
                        last_root = Some(root.clone());
                        if let Some(session) = sessions.get_mut(&server) {
                            session.restarts = 0;
                            if let Some(mut running) = session.server.take() {
                                let _ = running.child.kill().await;
                            }
                        }
                        launch(&mut sessions, server, &root, &events, &out_tx).await;
                    }
                    other => {
                        let kind = kind_of_cmd(&other);
                        let server_up = sessions.get(&kind).is_some_and(|session| session.server.is_some());
                        if !server_up {
                            let start = {
                                let Some(session) = sessions.get_mut(&kind) else { continue };
                                if let LspCmd::DidOpen { path, language, text, version } = &other {
                                    session.open_docs.insert(path.clone(), (*language, text.clone(), *version));
                                }
                                !session.launch_attempted
                                    && last_root.is_some()
                                    && matches!(other, LspCmd::DidOpen { .. })
                            };
                            if start && let Some(root) = last_root.clone() {
                                launch(&mut sessions, kind, &root, &events, &out_tx).await;
                            }
                            continue;
                        }
                        let failed = {
                            let Some(session) = sessions.get_mut(&kind) else { continue };
                            if let Some(server) = session.server.as_mut() {
                                handle_cmd(server, other, &mut session.open_docs).await.err()
                            } else {
                                None
                            }
                        };
                        if let Some(err) = failed {
                            tracing::warn!("lsp command failed: {err}");
                            if let Some(session) = sessions.get_mut(&kind) {
                                if let Some(mut server) = session.server.take() {
                                    let _ = server.child.kill().await;
                                }
                            }
                            let _ = events.send(LspEvent::Status {
                                server: kind,
                                status: LspStatus::Error(err),
                            });
                        }
                    }
                }
            }
            incoming = out_rx.recv() => {
                let Some(incoming) = incoming else { break; };
                let current = sessions.get(&incoming.kind).map(|session| session.generation);
                if current != Some(incoming.generation) {
                    continue;
                }
                let closed = incoming.msg.get("method").and_then(|method| method.as_str()) == Some("exit")
                    || incoming.msg.get("closed").is_some();
                if closed {
                    let attempt = {
                        let Some(session) = sessions.get_mut(&incoming.kind) else { continue };
                        session.server = None;
                        if session.restarts < 5 {
                            session.restarts += 1;
                            Some(session.restarts)
                        } else {
                            None
                        }
                    };
                    let _ = events.send(LspEvent::Status {
                        server: incoming.kind,
                        status: LspStatus::Error("language server exited".into()),
                    });
                    if let Some(attempt) = attempt {
                        if let Some(root) = last_root.clone() {
                            tokio::time::sleep(Duration::from_millis(300 * u64::from(attempt))).await;
                            launch(&mut sessions, incoming.kind, &root, &events, &out_tx).await;
                        }
                    }
                    continue;
                }
                let Some(session) = sessions.get_mut(&incoming.kind) else { continue };
                if incoming.generation != session.generation {
                    continue;
                }
                if let Some(server) = session.server.as_mut() {
                    dispatch_message(server, incoming.msg, &events, &mut session.open_docs, incoming.kind).await;
                }
            }
        }
    }
}

fn kind_of_cmd(cmd: &LspCmd) -> ServerKind {
    match cmd {
        LspCmd::DidOpen { language, .. } => server_for(*language),
        LspCmd::DidChange { path, .. }
        | LspCmd::DidClose { path }
        | LspCmd::Completion { path, .. }
        | LspCmd::Hover { path, .. }
        | LspCmd::Signature { path, .. }
        | LspCmd::Definition { path, .. }
        | LspCmd::References { path, .. }
        | LspCmd::Rename { path, .. } => server_for_path(path),
        LspCmd::WillRename { from, .. } => server_for_path(from),
        LspCmd::Initialize { .. } | LspCmd::Restart { .. } | LspCmd::Shutdown => {
            ServerKind::TypeScript
        }
    }
}

async fn launch(
    sessions: &mut HashMap<ServerKind, Session>,
    kind: ServerKind,
    root: &Path,
    events: &mpsc::UnboundedSender<LspEvent>,
    out_tx: &mpsc::UnboundedSender<Incoming>,
) {
    let (generation, existing) = {
        let Some(session) = sessions.get_mut(&kind) else {
            return;
        };
        session.launch_attempted = true;
        session.generation = session.generation.wrapping_add(1);
        (session.generation, session.server.take())
    };
    if let Some(mut server) = existing {
        let _ = server.child.kill().await;
    }
    match start_server(kind, root, out_tx.clone(), generation).await {
        Ok(server) => {
            let _ = events.send(LspEvent::Status {
                server: kind,
                status: LspStatus::Starting,
            });
            if let Some(session) = sessions.get_mut(&kind) {
                session.server = Some(server);
            }
        }
        Err(err) => {
            let _ = events.send(LspEvent::Status {
                server: kind,
                status: LspStatus::Unavailable(err),
            });
        }
    }
}

async fn start_server(
    kind: ServerKind,
    root: &Path,
    out_tx: mpsc::UnboundedSender<Incoming>,
    generation: u64,
) -> Result<Server, String> {
    let (exe, args) = server_command(kind, root)?;
    let mut command = Command::new(exe);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|err| err.to_string())?;
    let stdout = child.stdout.take().ok_or("missing stdout")?;
    let stderr = child.stderr.take();
    let writer = child.stdin.take().ok_or("missing stdin")?;
    let label = server_binary(kind);
    tokio::spawn(read_lsp(stdout, kind, generation, out_tx));
    if let Some(stderr) = stderr {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!("{label}: {line}");
            }
        });
    }
    let mut server = Server {
        child,
        writer,
        incremental: true,
        next_id: 1,
        pending: HashMap::new(),
    };
    let uri = path_to_uri(root);
    let id = server.next_id;
    server.next_id += 1;
    server.pending.insert(id, Pending::Initialize);
    let init_options = match kind {
        ServerKind::TypeScript => {
            json!({ "preferences": { "includeCompletionsForModuleExports": true } })
        }
        ServerKind::Python => json!({}),
        ServerKind::Java => json!({ "bundles": [] }),
    };
    write_rpc(
        &mut server.writer,
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "rootUri": uri,
                "rootPath": root,
                "capabilities": {
                    "workspace": {
                        "workspaceEdit": { "documentChanges": true },
                        "fileOperations": {
                            "willRename": {
                                "filters": [{
                                    "scheme": "file",
                                    "pattern": { "glob": "**/*" }
                                }]
                            }
                        }
                    },
                    "textDocument": {
                        "synchronization": { "dynamicRegistration": false, "willSave": false, "didSave": true },
                        "completion": { "completionItem": { "snippetSupport": false } },
                        "hover": { "contentFormat": ["plaintext", "markdown"] },
                        "signatureHelp": {},
                        "definition": {},
                        "references": {},
                        "rename": {},
                        "publishDiagnostics": {}
                    }
                },
                "initializationOptions": init_options,
                "workspaceFolders": [{ "uri": uri, "name": root.file_name().and_then(|n| n.to_str()).unwrap_or("workspace") }]
            }
        }),
    )
    .await?;
    Ok(server)
}

fn server_command(kind: ServerKind, root: &Path) -> Result<(PathBuf, Vec<String>), String> {
    let binary = server_binary(kind);
    let exe = which::which(binary).map_err(|_| format!("{binary} not found"))?;
    let args = match kind {
        ServerKind::TypeScript | ServerKind::Python => vec!["--stdio".to_string()],
        ServerKind::Java => {
            let data = jdtls_data_dir(root);
            std::fs::create_dir_all(&data).map_err(|err| err.to_string())?;
            vec!["-data".to_string(), data.display().to_string()]
        }
    };
    Ok((exe, args))
}

fn jdtls_data_dir(root: &Path) -> PathBuf {
    let base = dirs::data_dir()
        .or_else(dirs::config_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("r3vu").join("jdtls").join(workspace_key(root))
}

fn workspace_key(root: &Path) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in root.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

async fn read_lsp<R: tokio::io::AsyncRead + Unpin>(
    reader: R,
    kind: ServerKind,
    generation: u64,
    tx: mpsc::UnboundedSender<Incoming>,
) {
    let mut reader = BufReader::new(reader);
    loop {
        let mut header = String::new();
        let mut content_length = None;
        loop {
            header.clear();
            match reader.read_line(&mut header).await {
                Ok(0) => {
                    let _ = tx.send(Incoming {
                        kind,
                        generation,
                        msg: json!({"closed": true}),
                    });
                    return;
                }
                Ok(_) => {}
                Err(_) => {
                    let _ = tx.send(Incoming {
                        kind,
                        generation,
                        msg: json!({"closed": true}),
                    });
                    return;
                }
            }
            let line = header.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some(rest) = line.strip_prefix("Content-Length:") {
                content_length = rest.trim().parse().ok();
            }
        }
        let Some(len) = content_length else {
            continue;
        };
        let mut buf = vec![0u8; len];
        if reader.read_exact(&mut buf).await.is_err() {
            let _ = tx.send(Incoming {
                kind,
                generation,
                msg: json!({"closed": true}),
            });
            return;
        }
        if let Ok(value) = serde_json::from_slice(&buf) {
            let _ = tx.send(Incoming {
                kind,
                generation,
                msg: value,
            });
        }
    }
}

async fn write_rpc(writer: &mut tokio::process::ChildStdin, value: Value) -> Result<(), String> {
    let body = serde_json::to_vec(&value).map_err(|e| e.to_string())?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    writer
        .write_all(header.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    writer.write_all(&body).await.map_err(|e| e.to_string())?;
    writer.flush().await.map_err(|e| e.to_string())?;
    Ok(())
}

async fn handle_cmd(
    server: &mut Server,
    cmd: LspCmd,
    open_docs: &mut HashMap<PathBuf, (LanguageId, String, i64)>,
) -> Result<(), String> {
    match cmd {
        LspCmd::DidOpen {
            path,
            language,
            text,
            version,
        } => {
            open_docs.insert(path.clone(), (language, text.clone(), version));
            write_rpc(
                &mut server.writer,
                json!({
                    "jsonrpc": "2.0",
                    "method": "textDocument/didOpen",
                    "params": {
                        "textDocument": {
                            "uri": path_to_uri(&path),
                            "languageId": language.lsp_language_id(),
                            "version": version,
                            "text": text
                        }
                    }
                }),
            )
            .await
        }
        LspCmd::DidChange {
            path,
            version,
            change,
            full_text,
            incremental,
        } => {
            if let Some(entry) = open_docs.get_mut(&path) {
                entry.1 = full_text.clone();
                entry.2 = version;
            }
            let content_change = if incremental && server.incremental {
                json!({
                    "range": {
                        "start": { "line": change.start_position.line, "character": change_to_utf16_start(&full_text, &change) },
                        "end": { "line": change.old_end_position.line, "character": change.old_end_position.column }
                    },
                    "text": change.inserted_text
                })
            } else {
                json!({ "text": full_text })
            };
            // Always send full text to avoid UTF-16 mismatch during v1.
            let _ = (content_change, incremental);
            write_rpc(
                &mut server.writer,
                json!({
                    "jsonrpc": "2.0",
                    "method": "textDocument/didChange",
                    "params": {
                        "textDocument": { "uri": path_to_uri(&path), "version": version },
                        "contentChanges": [{ "text": full_text }]
                    }
                }),
            )
            .await
        }
        LspCmd::DidClose { path } => {
            open_docs.remove(&path);
            write_rpc(
                &mut server.writer,
                json!({
                    "jsonrpc": "2.0",
                    "method": "textDocument/didClose",
                    "params": { "textDocument": { "uri": path_to_uri(&path) } }
                }),
            )
            .await
        }
        LspCmd::Completion {
            token,
            path,
            position,
        } => {
            request(
                server,
                "textDocument/completion",
                json!({
                    "textDocument": { "uri": path_to_uri(&path) },
                    "position": lsp_pos_raw(position),
                    "context": { "triggerKind": 1 }
                }),
                Pending::Completion(token),
            )
            .await
        }
        LspCmd::Hover {
            token,
            path,
            position,
        } => {
            request(
                server,
                "textDocument/hover",
                pos_params(&path, position),
                Pending::Hover(token),
            )
            .await
        }
        LspCmd::Signature {
            token,
            path,
            position,
        } => {
            request(
                server,
                "textDocument/signatureHelp",
                pos_params(&path, position),
                Pending::Signature(token),
            )
            .await
        }
        LspCmd::Definition {
            token,
            path,
            position,
        } => {
            request(
                server,
                "textDocument/definition",
                pos_params(&path, position),
                Pending::Definition(token),
            )
            .await
        }
        LspCmd::References {
            token,
            path,
            position,
        } => {
            request(
                server,
                "textDocument/references",
                json!({
                    "textDocument": { "uri": path_to_uri(&path) },
                    "position": lsp_pos_raw(position),
                    "context": { "includeDeclaration": true }
                }),
                Pending::References(token),
            )
            .await
        }
        LspCmd::Rename {
            token,
            path,
            position,
            new_name,
        } => {
            request(
                server,
                "textDocument/rename",
                json!({
                    "textDocument": { "uri": path_to_uri(&path) },
                    "position": lsp_pos_raw(position),
                    "newName": new_name
                }),
                Pending::Rename(token),
            )
            .await
        }
        LspCmd::WillRename { token, from, to } => {
            request(
                server,
                "workspace/willRenameFiles",
                json!({
                    "files": [{
                        "oldUri": path_to_uri(&from),
                        "newUri": path_to_uri(&to)
                    }]
                }),
                Pending::WillRename(token),
            )
            .await
        }
        LspCmd::Initialize { .. } | LspCmd::Restart { .. } | LspCmd::Shutdown => Ok(()),
    }
}

fn change_to_utf16_start(_text: &str, change: &DocumentChange) -> u32 {
    change.start_position.column as u32
}

fn pos_params(path: &Path, position: Position) -> Value {
    json!({
        "textDocument": { "uri": path_to_uri(path) },
        "position": lsp_pos_raw(position)
    })
}

fn lsp_pos_raw(position: Position) -> Value {
    json!({ "line": position.line, "character": position.column })
}

pub fn lsp_position(doc: &Document, pos: Position) -> Value {
    json!({
        "line": pos.line,
        "character": position_to_utf16(&doc.rope, pos)
    })
}

async fn request(
    server: &mut Server,
    method: &str,
    params: Value,
    pending: Pending,
) -> Result<(), String> {
    let id = server.next_id;
    server.next_id += 1;
    server.pending.insert(id, pending);
    write_rpc(
        &mut server.writer,
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }),
    )
    .await
}

async fn dispatch_message(
    server: &mut Server,
    msg: Value,
    events: &mpsc::UnboundedSender<LspEvent>,
    open_docs: &mut HashMap<PathBuf, (LanguageId, String, i64)>,
    kind: ServerKind,
) {
    if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
        if msg.get("id").is_some() {
            let id = msg.get("id").cloned().unwrap_or(Value::Null);
            let _ = write_rpc(
                &mut server.writer,
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": client_request_result(method, &msg),
                }),
            )
            .await;
            return;
        }
        if method == "textDocument/publishDiagnostics" {
            if let Some(params) = msg.get("params") {
                let uri = params.get("uri").and_then(|u| u.as_str()).unwrap_or("");
                let path = uri_to_path(uri);
                let items = params
                    .get("diagnostics")
                    .and_then(|d| d.as_array())
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|d| parse_diagnostic(&path, d))
                    .collect();
                let _ = events.send(LspEvent::Diagnostics { path, items });
            }
        }
        return;
    }
    let Some(id) = msg.get("id").and_then(|id| id.as_i64()) else {
        return;
    };
    let Some(pending) = server.pending.remove(&id) else {
        return;
    };
    match pending {
        Pending::Initialize => {
            if msg.get("error").is_some() {
                let _ = events.send(LspEvent::Status {
                    server: kind,
                    status: LspStatus::Error("initialize failed".into()),
                });
                return;
            }
            let caps = msg.pointer("/result/capabilities/textDocumentSync");
            server.incremental = match caps {
                Some(Value::Number(n)) => n.as_u64() == Some(2),
                Some(Value::Object(o)) => o.get("change").and_then(|c| c.as_u64()) == Some(2),
                _ => true,
            };
            let _ = write_rpc(
                &mut server.writer,
                json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
            )
            .await;
            let _ = events.send(LspEvent::Status {
                server: kind,
                status: LspStatus::Ready,
            });
            let docs: Vec<_> = open_docs
                .iter()
                .map(|(p, (l, t, v))| (p.clone(), *l, t.clone(), *v))
                .collect();
            for (path, language, text, version) in docs {
                let _ = write_rpc(
                    &mut server.writer,
                    json!({
                        "jsonrpc": "2.0",
                        "method": "textDocument/didOpen",
                        "params": {
                            "textDocument": {
                                "uri": path_to_uri(&path),
                                "languageId": language.lsp_language_id(),
                                "version": version,
                                "text": text
                            }
                        }
                    }),
                )
                .await;
            }
        }
        Pending::Completion(token) => {
            let items = parse_completions(msg.get("result"));
            let _ = events.send(LspEvent::Completion { token, items });
        }
        Pending::Hover(token) => {
            let text = parse_hover(msg.get("result"));
            let _ = events.send(LspEvent::Hover { token, text });
        }
        Pending::Signature(token) => {
            let text = parse_signature(msg.get("result"));
            let _ = events.send(LspEvent::Signature { token, text });
        }
        Pending::Definition(token) | Pending::References(token) => {
            let items = parse_locations(msg.get("result"));
            let _ = events.send(LspEvent::Locations { token, items });
        }
        Pending::Rename(token) => {
            let items = parse_workspace_edit(msg.get("result"));
            let _ = events.send(LspEvent::Edits { token, items });
        }
        Pending::WillRename(token) => {
            if msg.get("error").is_some() {
                let _ = events.send(LspEvent::FileRename {
                    token,
                    items: Vec::new(),
                    failed: true,
                });
            } else {
                let items = parse_workspace_edit(msg.get("result"));
                let _ = events.send(LspEvent::FileRename {
                    token,
                    items,
                    failed: false,
                });
            }
        }
    }
}

fn client_request_result(method: &str, msg: &Value) -> Value {
    match method {
        "workspace/configuration" => {
            let count = msg
                .pointer("/params/items")
                .and_then(|items| items.as_array())
                .map(|items| items.len())
                .unwrap_or(0);
            Value::Array(vec![Value::Null; count])
        }
        "workspace/applyEdit" => json!({ "applied": false }),
        _ => Value::Null,
    }
}

fn parse_diagnostic(path: &Path, d: Value) -> Option<Diagnostic> {
    let message = d.get("message")?.as_str()?.to_string();
    let severity = d.get("severity").and_then(|s| s.as_u64()).unwrap_or(1) as u8;
    let start = d.pointer("/range/start")?;
    let end = d.pointer("/range/end")?;
    Some(Diagnostic {
        path: path.to_path_buf(),
        range_start: Position::new(
            start.get("line")?.as_u64()? as usize,
            start.get("character")?.as_u64()? as usize,
        ),
        range_end: Position::new(
            end.get("line")?.as_u64()? as usize,
            end.get("character")?.as_u64()? as usize,
        ),
        severity,
        message,
    })
}

pub fn completion_kind_tag(kind: Option<u8>) -> &'static str {
    match kind {
        Some(2..=4) => "fn",
        Some(5 | 6 | 10 | 21) => "var",
        Some(7 | 8 | 13 | 22 | 25) => "type",
        Some(14) => "kw",
        Some(9 | 17) => "mod",
        Some(15) => "snip",
        _ => "•",
    }
}

pub fn completion_prefix(doc: &Document, cursor: Position) -> (Position, String) {
    let text = crate::editor::line_text(&doc.rope, cursor.line);
    let chars: Vec<char> = text.chars().collect();
    let end = cursor.column.min(chars.len());
    let mut start = end;
    while start > 0 && is_completion_char(chars[start - 1]) {
        start -= 1;
    }
    let prefix: String = chars[start..end].iter().collect();
    (Position::new(cursor.line, start), prefix)
}

fn is_completion_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_' || ch == '$'
}

pub fn filter_completions(items: &[CompletionItem], prefix: &str) -> Vec<CompletionItem> {
    let needle = prefix.to_lowercase();
    let mut matched: Vec<CompletionItem> = items
        .iter()
        .filter(|item| needle.is_empty() || item.filter_text.to_lowercase().starts_with(&needle))
        .cloned()
        .collect();
    matched.sort_by(|a, b| {
        a.sort_text
            .cmp(&b.sort_text)
            .then_with(|| a.label.cmp(&b.label))
    });
    matched.truncate(80);
    matched
}

pub fn strip_snippets(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '$' && index + 1 < chars.len() {
            if chars[index + 1] == '{' {
                if let Some(end) = matching_brace(&chars, index + 1) {
                    let inner: String = chars[index + 2..end].iter().collect();
                    out.push_str(&snippet_default(&inner));
                    index = end + 1;
                    continue;
                }
            } else if chars[index + 1].is_ascii_digit() {
                index += 2;
                while index < chars.len() && chars[index].is_ascii_digit() {
                    index += 1;
                }
                continue;
            }
        }
        out.push(chars[index]);
        index += 1;
    }
    out
}

fn matching_brace(chars: &[char], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (idx, ch) in chars.iter().enumerate().skip(open) {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(idx);
                }
            }
            _ => {}
        }
    }
    None
}

fn snippet_default(inner: &str) -> String {
    let rest = inner.trim_start_matches(|ch: char| ch.is_ascii_digit());
    if let Some(body) = rest.strip_prefix(':') {
        strip_snippets(body)
    } else if let Some(body) = rest.strip_prefix('|') {
        let body = body.strip_suffix('|').unwrap_or(body);
        body.split(',').next().unwrap_or("").to_string()
    } else {
        String::new()
    }
}

fn parse_completions(result: Option<&Value>) -> Vec<CompletionItem> {
    let items = match result {
        Some(Value::Array(a)) => a.clone(),
        Some(Value::Object(o)) => o
            .get("items")
            .and_then(|i| i.as_array())
            .cloned()
            .unwrap_or_default(),
        _ => return Vec::new(),
    };
    items
        .into_iter()
        .filter_map(|item| {
            let label = item.get("label")?.as_str()?.to_string();
            let edit = parse_completion_edit(&item);
            let insert = edit
                .as_ref()
                .map(|edit| edit.new_text.clone())
                .or_else(|| {
                    item.get("insertText")
                        .and_then(|text| text.as_str())
                        .map(|text| text.to_string())
                })
                .unwrap_or_else(|| label.clone());
            let filter_text = item
                .get("filterText")
                .and_then(|text| text.as_str())
                .unwrap_or(&label)
                .to_string();
            let sort_text = item
                .get("sortText")
                .and_then(|text| text.as_str())
                .unwrap_or(&filter_text)
                .to_string();
            Some(CompletionItem {
                detail: item
                    .get("detail")
                    .and_then(|d| d.as_str())
                    .map(|s| s.to_string()),
                filter_text,
                sort_text,
                kind: item.get("kind").and_then(|k| k.as_u64()).map(|k| k as u8),
                label,
                insert,
                edit,
            })
        })
        .collect()
}

fn parse_completion_edit(item: &Value) -> Option<CompletionEdit> {
    let edit = item.get("textEdit")?;
    let new_text = edit.get("newText")?.as_str()?.to_string();
    let range = edit.get("range").or_else(|| edit.get("insert"))?;
    let start = range.get("start")?;
    let end = range.get("end")?;
    Some(CompletionEdit {
        start: Position::new(
            start.get("line")?.as_u64()? as usize,
            start.get("character")?.as_u64()? as usize,
        ),
        end: Position::new(
            end.get("line")?.as_u64()? as usize,
            end.get("character")?.as_u64()? as usize,
        ),
        new_text,
    })
}

fn parse_hover(result: Option<&Value>) -> String {
    let Some(result) = result else {
        return String::new();
    };
    match result.get("contents") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Object(o)) => o
            .get("value")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|v| match v {
                Value::String(s) => Some(s.clone()),
                Value::Object(o) => o
                    .get("value")
                    .and_then(|x| x.as_str())
                    .map(|s| s.to_string()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn parse_signature(result: Option<&Value>) -> String {
    let Some(result) = result else {
        return String::new();
    };
    let sigs = result.get("signatures").and_then(|s| s.as_array());
    let Some(sigs) = sigs else {
        return String::new();
    };
    sigs.iter()
        .filter_map(|s| {
            s.get("label")
                .and_then(|l| l.as_str())
                .map(|s| s.to_string())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_locations(result: Option<&Value>) -> Vec<Location> {
    let values = match result {
        Some(Value::Array(a)) => a.clone(),
        Some(obj) if obj.get("uri").is_some() => vec![obj.clone()],
        _ => return Vec::new(),
    };
    values
        .into_iter()
        .filter_map(|v| {
            let uri = v
                .get("uri")
                .or_else(|| v.pointer("/targetUri"))
                .and_then(|u| u.as_str())?;
            let pos = v
                .pointer("/range/start")
                .or_else(|| v.pointer("/targetSelectionRange/start"))?;
            Some(Location {
                path: uri_to_path(uri),
                position: Position::new(
                    pos.get("line")?.as_u64()? as usize,
                    pos.get("character")?.as_u64()? as usize,
                ),
            })
        })
        .collect()
}

fn parse_workspace_edit(result: Option<&Value>) -> Vec<TextEdit> {
    let Some(result) = result else {
        return Vec::new();
    };
    let mut edits = Vec::new();
    if let Some(changes) = result.get("changes").and_then(|c| c.as_object()) {
        for (uri, list) in changes {
            let path = uri_to_path(uri);
            if let Some(list) = list.as_array() {
                for edit in list {
                    if let Some(parsed) = parse_text_edit(&path, edit) {
                        edits.push(parsed);
                    }
                }
            }
        }
    }
    if let Some(docs) = result.get("documentChanges").and_then(|c| c.as_array()) {
        for doc in docs {
            let uri = doc
                .pointer("/textDocument/uri")
                .and_then(|u| u.as_str())
                .unwrap_or("");
            let path = uri_to_path(uri);
            if let Some(list) = doc.get("edits").and_then(|e| e.as_array()) {
                for edit in list {
                    if let Some(parsed) = parse_text_edit(&path, edit) {
                        edits.push(parsed);
                    }
                }
            }
        }
    }
    edits
}

fn parse_text_edit(path: &Path, edit: &Value) -> Option<TextEdit> {
    let start = edit.pointer("/range/start")?;
    let end = edit.pointer("/range/end")?;
    Some(TextEdit {
        path: path.to_path_buf(),
        start: Position::new(
            start.get("line")?.as_u64()? as usize,
            start.get("character")?.as_u64()? as usize,
        ),
        end: Position::new(
            end.get("line")?.as_u64()? as usize,
            end.get("character")?.as_u64()? as usize,
        ),
        new_text: edit.get("newText")?.as_str()?.to_string(),
    })
}

pub fn path_to_uri(path: &Path) -> String {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    format!("file://{}", path.display())
}

pub fn uri_to_path(uri: &str) -> PathBuf {
    let stripped = uri.strip_prefix("file://").unwrap_or(uri);
    PathBuf::from(stripped)
}

pub fn utf16_position_to_doc(doc: &Document, line: usize, character: u32) -> Position {
    Position::new(
        line.min(doc.rope.len_lines().saturating_sub(1)),
        utf16_to_column(&doc.rope, line, character),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample(label: &str, filter: &str, sort: &str) -> CompletionItem {
        CompletionItem {
            label: label.into(),
            detail: None,
            insert: label.into(),
            filter_text: filter.into(),
            sort_text: sort.into(),
            kind: Some(3),
            edit: None,
        }
    }

    #[test]
    fn filters_and_sorts_by_prefix() {
        let items = vec![
            sample("zebra", "zebra", "b"),
            sample("console", "console", "a"),
            sample("const", "Const", "0"),
            sample("log", "log", "c"),
        ];
        let labels: Vec<_> = filter_completions(&items, "con")
            .into_iter()
            .map(|item| item.label)
            .collect();
        assert_eq!(labels, vec!["const".to_string(), "console".to_string()]);
    }

    #[test]
    fn empty_prefix_keeps_the_sorted_list() {
        let items = vec![
            sample("zebra", "zebra", "b"),
            sample("console", "console", "a"),
            sample("const", "const", "0"),
            sample("log", "log", "c"),
        ];
        let labels: Vec<_> = filter_completions(&items, "")
            .into_iter()
            .map(|item| item.label)
            .collect();
        assert_eq!(
            labels,
            vec![
                "const".to_string(),
                "console".to_string(),
                "zebra".to_string(),
                "log".to_string()
            ]
        );
    }

    #[test]
    fn strips_snippet_placeholders() {
        assert_eq!(strip_snippets("foo$0"), "foo");
        assert_eq!(strip_snippets("a${1}b$2"), "ab");
        assert_eq!(strip_snippets("${1:name}"), "name");
        assert_eq!(strip_snippets("fn ${1:name}($2)"), "fn name()");
    }

    #[test]
    fn parses_filter_sort_and_text_edit() {
        let result = json!({
            "items": [{
                "label": "console",
                "filterText": "con",
                "sortText": "1",
                "kind": 3,
                "insertText": "console.log($0)",
                "textEdit": {
                    "range": {
                        "start": {"line": 1, "character": 2},
                        "end": {"line": 1, "character": 5}
                    },
                    "newText": "console"
                }
            }]
        });
        let items = parse_completions(Some(&result));
        assert_eq!(items[0].filter_text, "con");
        assert_eq!(items[0].sort_text, "1");
        assert_eq!(items[0].kind, Some(3));
        assert_eq!(items[0].insert, "console");
        let edit = items[0].edit.as_ref().unwrap();
        assert_eq!(edit.start, Position::new(1, 2));
        assert_eq!(edit.end, Position::new(1, 5));
        assert_eq!(edit.new_text, "console");
        assert_eq!(completion_kind_tag(Some(3)), "fn");
        assert_eq!(completion_kind_tag(Some(6)), "var");
        assert_eq!(completion_kind_tag(Some(7)), "type");
        assert_eq!(completion_kind_tag(Some(14)), "kw");
    }

    fn diag(start: (usize, usize), end: (usize, usize), severity: u8, message: &str) -> Diagnostic {
        Diagnostic {
            path: PathBuf::from("a.ts"),
            range_start: Position::new(start.0, start.1),
            range_end: Position::new(end.0, end.1),
            severity,
            message: message.into(),
        }
    }

    #[test]
    fn diagnostic_at_covers_start_middle_and_end_lines() {
        let items = vec![diag((1, 4), (3, 2), 1, "broken")];
        assert_eq!(
            diagnostic_at(&items, Position::new(1, 4)).map(|item| item.message.as_str()),
            Some("broken")
        );
        assert_eq!(
            diagnostic_at(&items, Position::new(2, 0)).map(|item| item.message.as_str()),
            Some("broken")
        );
        assert_eq!(
            diagnostic_at(&items, Position::new(3, 1)).map(|item| item.message.as_str()),
            Some("broken")
        );
        assert!(diagnostic_at(&items, Position::new(1, 3)).is_none());
        assert!(diagnostic_at(&items, Position::new(3, 2)).is_none());
        assert!(diagnostic_at(&items, Position::new(4, 0)).is_none());
    }

    #[test]
    fn diagnostic_at_prefers_the_error_when_ranges_overlap() {
        let items = vec![
            diag((0, 0), (0, 8), 2, "careful"),
            diag((0, 2), (0, 6), 1, "broken"),
        ];
        let hit = diagnostic_at(&items, Position::new(0, 3)).unwrap();
        assert_eq!(hit.message, "broken");
        assert_eq!(diagnostic_label(hit.severity), "Error");
        assert_eq!(diagnostic_status(&items[0]), "Warning careful");
    }

    #[test]
    fn will_rename_keeps_text_edits_and_skips_the_file_rename() {
        let value = json!({
            "documentChanges": [
                {
                    "kind": "rename",
                    "oldUri": "file:///repo/a.ts",
                    "newUri": "file:///repo/b.ts"
                },
                {
                    "textDocument": { "uri": "file:///repo/use.ts" },
                    "edits": [{
                        "range": {
                            "start": { "line": 0, "character": 0 },
                            "end": { "line": 0, "character": 1 }
                        },
                        "newText": "x"
                    }]
                }
            ]
        });
        let edits = parse_workspace_edit(Some(&value));
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].new_text, "x");
        assert_eq!(edits[0].path, PathBuf::from("/repo/use.ts"));
    }

    #[test]
    fn python_and_java_commands_do_not_use_the_typescript_server() {
        let python = LspCmd::DidOpen {
            path: PathBuf::from("app.py"),
            language: LanguageId::Python,
            text: String::new(),
            version: 1,
        };
        let java = LspCmd::DidOpen {
            path: PathBuf::from("App.java"),
            language: LanguageId::Java,
            text: String::new(),
            version: 1,
        };
        assert_eq!(kind_of_cmd(&python), ServerKind::Python);
        assert_eq!(kind_of_cmd(&java), ServerKind::Java);
        assert_ne!(kind_of_cmd(&python), ServerKind::TypeScript);
        assert_eq!(
            kind_of_cmd(&LspCmd::Completion {
                token: 1,
                path: PathBuf::from("app.py"),
                position: Position::default(),
            }),
            ServerKind::Python
        );
        assert_eq!(
            kind_of_cmd(&LspCmd::WillRename {
                token: 1,
                from: PathBuf::from("A.java"),
                to: PathBuf::from("B.java"),
            }),
            ServerKind::Java
        );
        assert_eq!(
            kind_of_cmd(&LspCmd::DidOpen {
                path: PathBuf::from("a.ts"),
                language: LanguageId::TypeScript,
                text: String::new(),
                version: 1,
            }),
            ServerKind::TypeScript
        );
    }

    #[test]
    fn server_requests_get_an_empty_result() {
        let msg = json!({
            "id": 3,
            "method": "workspace/configuration",
            "params": { "items": [{ "section": "python" }, { "section": "python.analysis" }] }
        });
        assert_eq!(
            client_request_result("workspace/configuration", &msg),
            json!([null, null])
        );
        assert_eq!(
            client_request_result("workspace/applyEdit", &json!({})),
            json!({ "applied": false })
        );
        assert_eq!(
            client_request_result("client/registerCapability", &json!({})),
            Value::Null
        );
        assert_eq!(
            client_request_result("window/workDoneProgress/create", &json!({})),
            Value::Null
        );
    }
}
