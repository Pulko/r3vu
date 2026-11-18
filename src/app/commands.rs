use std::path::PathBuf;

use ropey::Rope;

use crate::agent::{AgentEvent, AgentRun, Anchor, LineKind, PromptStyle, TranscriptLine};
use crate::editor::{
    DocumentChange, DocumentId, EditorView, HScrollBar, LineEnding, Movement, Position, Range,
    Selection, clamp_horizontal_left, delete_backward, delete_forward, hscroll_left_at,
    indent_for_newline, insert_text, line_text, max_line_display_width, move_cursor,
    normalize_line_breaks, select_word_at, selected_text, set_cursor, word_bounds,
};
use crate::language::{
    CompletionItem, LspCmd, LspStatus, TextEdit, completion_prefix, filter_completions,
    strip_snippets, utf16_position_to_doc,
};
use crate::workspace::{
    GitWrite, WatchEvent, WholeFileAction, create_path, delete_path, find_in_document, rename_path,
    whole_file_action,
};

use super::state::{
    App, ConfirmAction, Focus, HScrollHit, MenuAction, MenuItem, Overlay, PendingRename,
    PromptKind, SearchKind, SearchScope, clamp_explorer_scroll, decode_diagnostic_pos,
    explorer_index_at, explorer_inner_rows, git_list_rows, is_ident_char, max_explorer_scroll,
};

pub fn save_active(app: &mut App) {
    let Some(id) = app.active else { return };
    let mut needs_save_as = false;
    let before = app.documents.get(&id).map(|doc| doc.text());
    if let Some(doc) = app.documents.get_mut(&id) {
        if doc.path.is_none() {
            needs_save_as = true;
        } else if let Err(err) = doc.save() {
            app.message = Some(format!("save failed: {err}"));
            return;
        } else {
            app.message = Some("saved".into());
        }
    }
    if let Some(before) = before.filter(|_| !needs_save_as) {
        sync_normalized_buffer(app, id, &before, true);
    }
    if needs_save_as {
        open_prompt(
            app,
            "Save as (relative path)",
            String::new(),
            PromptKind::SaveAs,
        );
    } else {
        app.refresh_git_status();
    }
}

fn sync_normalized_buffer(app: &mut App, id: DocumentId, before: &str, notify_lsp: bool) {
    let Some(doc) = app.documents.get(&id) else {
        return;
    };
    if doc.text() == before {
        return;
    }
    let language = doc.language;
    let rope = doc.rope.clone();
    let revision = doc.revision;
    let path = doc.path.clone();
    let text = doc.text();
    app.syntax.open(id, language, &rope);
    if !notify_lsp {
        return;
    }
    let Some(path) = path else {
        return;
    };
    let _ = app.lsp.tx.send(LspCmd::DidChange {
        path,
        version: revision as i64,
        change: DocumentChange {
            document_id: id,
            revision,
            start_byte: 0,
            old_end_byte: 0,
            new_end_byte: text.len(),
            start_char: 0,
            old_end_char: 0,
            new_end_char: text.chars().count(),
            start_position: Position::default(),
            old_end_position: Position::default(),
            new_end_position: Position::default(),
            start_row: 0,
            start_byte_column: 0,
            old_end_row: 0,
            old_end_byte_column: 0,
            new_end_row: 0,
            new_end_byte_column: 0,
            inserted_text: text.clone(),
            deleted_text: String::new(),
        },
        full_text: text,
        incremental: false,
    });
}

pub fn type_text(app: &mut App, text: &str) {
    if app.markdown_preview_open() {
        app.markdown_preview = None;
    }
    app.hover_dwell = None;
    if !matches!(
        app.overlay,
        Overlay::None
            | Overlay::Completion { .. }
            | Overlay::Signature { .. }
            | Overlay::Hover { .. }
    ) {
        return;
    }
    let Some(id) = app.active else { return };
    let line_ending = app
        .documents
        .get(&id)
        .map(|doc| doc.line_ending)
        .unwrap_or(LineEnding::Lf);
    let text = normalize_line_breaks(text, line_ending);
    let newline = text == "\n" || text == "\r\n";
    let extra = {
        let doc = app.documents.get(&id);
        let view = app.views.get(&id);
        if newline {
            if let (Some(doc), Some(view)) = (doc, view) {
                indent_for_newline(doc, view)
            } else {
                String::new()
            }
        } else {
            String::new()
        }
    };
    let payload = if newline {
        format!(
            "{}{}",
            app.active_doc()
                .map(|d| d.line_ending.as_str())
                .unwrap_or("\n"),
            extra
        )
    } else {
        text
    };
    let changes =
        if let (Some(doc), Some(view)) = (app.documents.get_mut(&id), app.views.get_mut(&id)) {
            let group = payload.chars().count() == 1;
            insert_text(doc, view, &payload, group)
        } else {
            Vec::new()
        };
    app.apply_changes(changes);
    if payload.chars().count() == 1 && payload.chars().next().is_some_and(is_ident_char) {
        app.request_completion();
        refresh_completion_popup(app);
    } else if !matches!(app.overlay, Overlay::Completion { .. }) {
        // keep typing
    } else if payload.chars().next().is_some_and(|c| c == '(') {
        request_signature(app);
    } else {
        dismiss_soft_overlays(app);
    }
}

pub fn move_sel(app: &mut App, movement: Movement, selecting: bool) {
    if app.markdown_preview_open() {
        match movement {
            Movement::Up | Movement::Left => app.scroll_markdown_preview(-1),
            Movement::Down | Movement::Right => app.scroll_markdown_preview(1),
            Movement::PageUp => app.scroll_markdown_preview(-10),
            Movement::PageDown => app.scroll_markdown_preview(10),
            Movement::DocumentStart | Movement::LineStart => app.jump_markdown_preview(false),
            Movement::DocumentEnd | Movement::LineEnd => app.jump_markdown_preview(true),
            _ => {}
        }
        return;
    }
    if matches!(app.overlay, Overlay::Completion { .. })
        && matches!(movement, Movement::Up | Movement::Down)
    {
        let count = visible_completions(app).len();
        if count == 0 {
            return;
        }
        if let Overlay::Completion { selected, .. } = &mut app.overlay {
            if movement == Movement::Up {
                *selected = selected.checked_sub(1).unwrap_or(count - 1);
            } else {
                *selected = (*selected + 1) % count;
            }
        }
        return;
    }
    if !matches!(
        app.overlay,
        Overlay::None
            | Overlay::Completion { .. }
            | Overlay::Hover { .. }
            | Overlay::Signature { .. }
    ) {
        overlay_move(app, movement);
        return;
    }
    let height = app.editor_area.3.saturating_sub(2) as usize;
    let Some(id) = app.active else { return };
    if let (Some(doc), Some(view)) = (app.documents.get(&id), app.views.get_mut(&id)) {
        move_cursor(view, &doc.rope, movement, selecting, height.max(1));
        view.ensure_visible(
            &doc.rope,
            height.max(1),
            app.editor_area.2.saturating_sub(app.gutter_width) as usize,
            app.config.tab_width,
        );
    }
    reveal_cursor(app, id);
    app.settle_conflict_cursor();
}

pub fn backspace(app: &mut App) {
    if overlay_backspace(app) {
        return;
    }
    if app.markdown_preview_open() {
        app.markdown_preview = None;
    }
    let completing = matches!(app.overlay, Overlay::Completion { .. });
    let Some(id) = app.active else { return };
    let changes =
        if let (Some(doc), Some(view)) = (app.documents.get_mut(&id), app.views.get_mut(&id)) {
            delete_backward(doc, view)
        } else {
            Vec::new()
        };
    app.apply_changes(changes);
    if completing {
        app.request_completion();
        refresh_completion_popup(app);
    }
}

pub fn delete_key(app: &mut App) {
    if app.markdown_preview_open() {
        app.markdown_preview = None;
    }
    let Some(id) = app.active else { return };
    let changes =
        if let (Some(doc), Some(view)) = (app.documents.get_mut(&id), app.views.get_mut(&id)) {
            delete_forward(doc, view)
        } else {
            Vec::new()
        };
    app.apply_changes(changes);
}

pub fn undo(app: &mut App) {
    let Some(id) = app.active else { return };
    if let Some(doc) = app.documents.get_mut(&id) {
        if let Some(changes) = doc.undo() {
            if let Some(last) = changes.last() {
                if let Some(view) = app.views.get_mut(&id) {
                    view.cursor = last.start_position;
                    view.selection = None;
                }
            }
            app.apply_changes(changes);
        }
    }
}

pub fn redo(app: &mut App) {
    let Some(id) = app.active else { return };
    if let Some(doc) = app.documents.get_mut(&id) {
        if let Some(changes) = doc.redo() {
            if let Some(last) = changes.last() {
                if let Some(view) = app.views.get_mut(&id) {
                    view.cursor = last.new_end_position;
                    view.selection = None;
                }
            }
            app.apply_changes(changes);
        }
    }
}

fn copy_path(app: &mut App) {
    let Some(path) = app
        .explorer_entries
        .get(app.explorer_selected)
        .map(|entry| entry.path.display().to_string())
    else {
        return;
    };
    app.clipboard.copy(&path);
    app.message = Some("path copied".into());
}

pub fn copy(app: &mut App) {
    let Some(id) = app.active else { return };
    if let (Some(doc), Some(view)) = (app.documents.get(&id), app.views.get(&id)) {
        let text = selected_text(doc, view);
        if !text.is_empty() {
            app.clipboard.copy(&text);
            app.message = Some("copied".into());
        }
    }
}

pub fn cut(app: &mut App) {
    copy(app);
    let Some(id) = app.active else { return };
    let changes =
        if let (Some(doc), Some(view)) = (app.documents.get_mut(&id), app.views.get_mut(&id)) {
            if view.selection.is_some() {
                crate::editor::delete_selection(doc, view)
                    .into_iter()
                    .collect()
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };
    app.apply_changes(changes);
}

pub fn paste(app: &mut App) {
    let text = app.clipboard.paste();
    if !text.is_empty() {
        type_text(app, &text);
    }
}

fn search_seed(text: &str) -> String {
    text.lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().to_string())
        .unwrap_or_default()
}

fn editor_search_seed(app: &App) -> String {
    let Some(id) = app.active else {
        return String::new();
    };
    let (Some(doc), Some(view)) = (app.documents.get(&id), app.views.get(&id)) else {
        return String::new();
    };
    if view.selection.is_none_or(|selection| selection.is_empty()) {
        return String::new();
    }
    search_seed(&selected_text(doc, view))
}

fn search_origin(app: &App) -> Option<Position> {
    let view = app.active_view()?;
    Some(
        view.selection
            .filter(|selection| !selection.is_empty())
            .map(|selection| selection.range().start)
            .unwrap_or(view.cursor),
    )
}

fn match_index_at(pos: Position, matches: &[(Position, Position)]) -> usize {
    matches
        .iter()
        .position(|(start, end)| *start <= pos && pos < *end)
        .or_else(|| matches.iter().position(|(start, _)| *start >= pos))
        .unwrap_or(0)
}

pub fn open_search(app: &mut App) {
    app.workspace_files = crate::workspace::list_workspace_files(&app.root);
    app.file_finder.set_files(&app.workspace_files);
    app.overlay = Overlay::Search {
        kind: SearchKind::Find,
        scope: SearchScope::Global,
        query: editor_search_seed(app),
        files: Vec::new(),
        matches: Vec::new(),
        file_matches: Vec::new(),
        selected: 0,
        scroll: 0,
        searching: false,
    };
    refresh_overlay_query(app);
}

pub fn open_content_search(app: &mut App) {
    if let Overlay::Search {
        kind: SearchKind::Content,
        scope,
        ..
    } = &app.overlay
    {
        if *scope == SearchScope::Global {
            app.overlay = Overlay::None;
            return;
        }
        set_search_scope(app, SearchScope::Global);
        return;
    }
    begin_content_search(app, SearchScope::Global, String::new());
}

pub fn open_search_file(app: &mut App) {
    let scope = if app.active_doc().is_some() {
        SearchScope::File
    } else {
        SearchScope::Global
    };
    let current = match &app.overlay {
        Overlay::Search {
            kind: SearchKind::Content,
            scope,
            ..
        } => Some(*scope),
        _ => None,
    };
    let Some(current) = current else {
        begin_content_search(app, scope, editor_search_seed(app));
        return;
    };
    let seed = editor_search_seed(app);
    if !seed.is_empty() {
        begin_content_search(app, scope, seed);
        return;
    }
    if current == scope {
        app.overlay = Overlay::None;
        return;
    }
    set_search_scope(app, scope);
}

fn begin_content_search(app: &mut App, mut scope: SearchScope, query: String) {
    if scope == SearchScope::File && app.active_doc().is_none() {
        scope = SearchScope::Global;
    }
    let selected = if scope == SearchScope::File {
        let matches = file_match_list(app, &query);
        search_origin(app)
            .map(|pos| match_index_at(pos, &matches))
            .unwrap_or(0)
    } else {
        0
    };
    app.overlay = Overlay::Search {
        kind: SearchKind::Content,
        scope,
        query,
        files: Vec::new(),
        matches: Vec::new(),
        file_matches: Vec::new(),
        selected,
        scroll: 0,
        searching: false,
    };
    refresh_overlay_query(app);
}

fn file_match_list(app: &App, query: &str) -> Vec<(Position, Position)> {
    app.active_doc()
        .map(|doc| find_in_document(&doc.text(), query, true))
        .unwrap_or_default()
}

pub fn set_search_scope(app: &mut App, scope: SearchScope) {
    let Overlay::Search {
        kind: SearchKind::Content,
        scope: current,
        ..
    } = &app.overlay
    else {
        return;
    };
    if *current == scope || (scope == SearchScope::File && app.active_doc().is_none()) {
        return;
    }
    let Overlay::Search { query, .. } = &app.overlay else {
        return;
    };
    let query = query.clone();
    let selected = if scope == SearchScope::File {
        let matches = file_match_list(app, &query);
        search_origin(app)
            .map(|pos| match_index_at(pos, &matches))
            .unwrap_or(0)
    } else {
        0
    };
    app.overlay = Overlay::Search {
        kind: SearchKind::Content,
        scope,
        query,
        files: Vec::new(),
        matches: Vec::new(),
        file_matches: Vec::new(),
        selected,
        scroll: 0,
        searching: false,
    };
    refresh_overlay_query(app);
}

pub fn toggle_search_scope(app: &mut App) {
    let Overlay::Search {
        kind: SearchKind::Content,
        scope,
        ..
    } = app.overlay
    else {
        return;
    };
    let next = match scope {
        SearchScope::File => SearchScope::Global,
        SearchScope::Global => SearchScope::File,
    };
    set_search_scope(app, next);
}

fn lsp_target(app: &App) -> Option<(PathBuf, Position)> {
    let doc = app.active_doc()?;
    let view = app.active_view()?;
    Some((doc.path.clone()?, utf16(doc, view.cursor)))
}

pub fn go_to_definition(app: &mut App) {
    let Some((path, position)) = lsp_target(app) else {
        return;
    };
    let token = app.next_token();
    let _ = app.lsp.tx.send(LspCmd::Definition {
        token,
        path,
        position,
    });
}

pub fn find_references(app: &mut App) {
    let Some((path, position)) = lsp_target(app) else {
        return;
    };
    let token = app.next_token();
    let _ = app.lsp.tx.send(LspCmd::References {
        token,
        path,
        position,
    });
}

pub fn hover(app: &mut App) {
    let Some((path, position)) = lsp_target(app) else {
        return;
    };
    let token = app.next_token();
    app.hover_token = token;
    let origin = app
        .active_view()
        .map(|view| view.cursor)
        .unwrap_or_default();
    let x = app.editor_area.0.saturating_add(app.gutter_width + 2);
    let y = app.editor_area.1.saturating_add(2);
    app.hover_anchor = Some((token, x, y, origin));
    let _ = app.lsp.tx.send(LspCmd::Hover {
        token,
        path,
        position,
    });
}

pub fn schedule_hover(app: &mut App, x: u16, y: u16) {
    if !matches!(app.overlay, Overlay::None | Overlay::Hover { .. }) {
        cancel_hover(app);
        return;
    }
    let Some(document_id) = app.active else {
        cancel_hover(app);
        return;
    };
    let Some(position) = editor_position_at(app, x, y) else {
        cancel_hover(app);
        return;
    };
    let (range, path) = {
        let Some(doc) = app.documents.get(&document_id) else {
            return;
        };
        (word_bounds(&doc.rope, position), doc.path.clone())
    };
    let diagnostic_anchor = path.as_ref().and_then(|path| {
        app.diagnostics.get(path).and_then(|items| {
            crate::language::diagnostic_at(items, position).map(|item| item.range_start)
        })
    });
    if range.is_empty() && diagnostic_anchor.is_none() {
        cancel_hover(app);
        return;
    }
    let symbol_position = diagnostic_anchor.unwrap_or(range.start);
    if app
        .hover_dwell
        .as_ref()
        .is_some_and(|dwell| dwell.document_id == document_id && dwell.position == symbol_position)
    {
        return;
    }
    if matches!(app.overlay, Overlay::Hover { .. }) {
        app.overlay = Overlay::None;
    }
    app.hover_dwell = Some(super::state::HoverDwell {
        document_id,
        position: symbol_position,
        screen_x: x,
        screen_y: y,
        since: std::time::Instant::now(),
    });
}

pub fn cancel_hover(app: &mut App) {
    app.hover_dwell = None;
    app.hover_anchor = None;
    if matches!(app.overlay, Overlay::Hover { .. }) {
        app.overlay = Overlay::None;
    }
}

fn wrap_plain(text: &str, width: usize) -> String {
    let width = width.max(8);
    let mut lines = Vec::new();
    for raw in text.split('\n') {
        let paragraph = raw.trim_end_matches('\r');
        if paragraph.trim().is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            if current.is_empty() {
                take_word(&mut lines, &mut current, word, width);
            } else if current.chars().count() + 1 + word.chars().count() <= width {
                current.push(' ');
                current.push_str(word);
            } else {
                lines.push(std::mem::take(&mut current));
                take_word(&mut lines, &mut current, word, width);
            }
        }
        if !current.is_empty() {
            lines.push(current);
        }
    }
    lines.join("\n")
}

fn take_word(lines: &mut Vec<String>, current: &mut String, word: &str, width: usize) {
    if word.chars().count() <= width {
        *current = word.to_string();
        return;
    }
    let mut rest = word;
    while rest.chars().count() > width {
        let split = rest
            .char_indices()
            .nth(width)
            .map(|(index, _)| index)
            .unwrap_or(rest.len());
        let (head, tail) = rest.split_at(split);
        lines.push(head.to_string());
        rest = tail;
    }
    *current = rest.to_string();
}

pub fn poll_hover(app: &mut App) {
    let Some(dwell) = app.hover_dwell.clone() else {
        return;
    };
    if dwell.since.elapsed() < super::state::HOVER_DWELL {
        return;
    }
    let Some(path) = app
        .documents
        .get(&dwell.document_id)
        .and_then(|doc| doc.path.clone())
    else {
        app.hover_dwell = None;
        return;
    };
    if let Some(diagnostic) = app
        .diagnostics
        .get(&path)
        .and_then(|items| crate::language::diagnostic_at(items, dwell.position))
        .cloned()
    {
        app.hover_dwell = None;
        app.hover_anchor = None;
        app.overlay = Overlay::Hover {
            title: crate::language::diagnostic_label(diagnostic.severity).into(),
            text: wrap_plain(diagnostic.message.trim(), 64),
            origin: dwell.position,
            x: dwell.screen_x,
            y: dwell.screen_y,
        };
        return;
    }
    let Some(doc) = app.documents.get(&dwell.document_id) else {
        app.hover_dwell = None;
        return;
    };
    let position = Position::new(
        dwell.position.line,
        crate::editor::position_to_utf16(&doc.rope, dwell.position) as usize,
    );
    let token = app.next_token();
    app.hover_token = token;
    app.hover_anchor = Some((token, dwell.screen_x, dwell.screen_y, dwell.position));
    app.hover_dwell = None;
    let _ = app.lsp.tx.send(LspCmd::Hover {
        token,
        path,
        position,
    });
}

pub fn request_signature(app: &mut App) {
    let Some((path, position)) = lsp_target(app) else {
        return;
    };
    let token = app.next_token();
    let _ = app.lsp.tx.send(LspCmd::Signature {
        token,
        path,
        position,
    });
}

pub fn start_rename_symbol(app: &mut App) {
    open_prompt(
        app,
        "Rename symbol",
        String::new(),
        PromptKind::RenameSymbol,
    );
}

pub fn visible_completions(app: &App) -> Vec<CompletionItem> {
    let Overlay::Completion { items, .. } = &app.overlay else {
        return Vec::new();
    };
    let prefix = app
        .active_doc()
        .zip(app.active_view())
        .map(|(doc, view)| completion_prefix(doc, view.cursor).1)
        .unwrap_or_default();
    filter_completions(items, &prefix)
}

pub fn refresh_completion_popup(app: &mut App) {
    if !matches!(app.overlay, Overlay::Completion { .. }) {
        return;
    }
    let count = visible_completions(app).len();
    if count == 0 {
        app.overlay = Overlay::None;
        return;
    }
    if let Overlay::Completion { selected, .. } = &mut app.overlay
        && *selected >= count
    {
        *selected = count - 1;
    }
}

pub fn accept_completion(app: &mut App) {
    let visible = visible_completions(app);
    let Overlay::Completion { selected, .. } = &app.overlay else {
        return;
    };
    let Some(item) = visible.get(*selected).cloned() else {
        app.overlay = Overlay::None;
        return;
    };
    app.overlay = Overlay::None;
    let Some(id) = app.active else { return };
    let changes =
        if let (Some(doc), Some(view)) = (app.documents.get_mut(&id), app.views.get_mut(&id)) {
            let (prefix_start, _) = completion_prefix(doc, view.cursor);
            let (start, end, text) = completion_replacement(doc, view.cursor, prefix_start, &item);
            let change = doc.apply_replace(Range::new(start, end), &text, false);
            view.cursor = change.new_end_position;
            view.selection = None;
            vec![change]
        } else {
            Vec::new()
        };
    app.apply_changes(changes);
}

fn completion_replacement(
    doc: &crate::editor::Document,
    cursor: Position,
    prefix_start: Position,
    item: &CompletionItem,
) -> (Position, Position, String) {
    if let Some(edit) = &item.edit {
        let start = utf16_position_to_doc(doc, edit.start.line, edit.start.column as u32);
        let end = utf16_position_to_doc(doc, edit.end.line, edit.end.column as u32);
        if start.line == cursor.line && end.line == cursor.line {
            return (start, end, strip_snippets(&edit.new_text));
        }
    }
    (prefix_start, cursor, strip_snippets(&item.insert))
}

pub fn confirm_yes(app: &mut App) {
    let Overlay::Confirm { action, .. } = app.overlay.clone() else {
        return;
    };
    app.overlay = Overlay::None;
    match action {
        ConfirmAction::Quit => app.should_quit = true,
        ConfirmAction::Close(id) => {
            app.active = Some(id);
            app.close_active(true);
        }
        ConfirmAction::CloseMany(ids) => app.close_documents(&ids, true),
        ConfirmAction::Reload(id) => {
            if let Some(doc) = app.documents.get(&id) {
                if let Some(path) = doc.path.clone() {
                    app.documents.remove(&id);
                    app.views.remove(&id);
                    app.order.retain(|x| *x != id);
                    app.open_path(path);
                }
            }
        }
        ConfirmAction::DiscardHunk { patch, path } => app.discard_hunk(patch, path),
        ConfirmAction::RestoreFile(path) => app.enqueue_git_write(GitWrite::Restore(path)),
        ConfirmAction::DiscardAdded(path) => app.enqueue_git_write(GitWrite::Remove(path)),
        ConfirmAction::ResolveDelete(path) => resolve_delete(app, path),
        ConfirmAction::DeletePath(path) => {
            if let Err(err) = delete_path(&path) {
                app.message = Some(format!("delete failed: {err}"));
            } else {
                let ids: Vec<_> = app
                    .documents
                    .iter()
                    .filter(|(_, d)| {
                        d.path
                            .as_ref()
                            .is_some_and(|p| p == &path || p.starts_with(&path))
                    })
                    .map(|(id, _)| *id)
                    .collect();
                for id in ids {
                    app.active = Some(id);
                    app.close_active(true);
                }
                app.refresh_explorer();
                app.refresh_git_status();
            }
        }
    }
}

pub fn submit_prompt(app: &mut App) {
    let Overlay::Prompt { input, kind, .. } = app.overlay.clone() else {
        return;
    };
    app.overlay = Overlay::None;
    match kind {
        PromptKind::SaveAs => {
            let path = app.root.join(input);
            let mut opened = None;
            let before = app.active_doc().map(|doc| (doc.id, doc.text()));
            if let Some(doc) = app.active_doc_mut() {
                doc.path = Some(path.clone());
                doc.language = crate::editor::LanguageId::from_path(&path);
                if let Err(err) = doc.save() {
                    app.message = Some(format!("save failed: {err}"));
                } else {
                    opened = Some((doc.language, doc.text(), doc.revision as i64));
                }
            }
            if let Some((id, before)) = before {
                sync_normalized_buffer(app, id, &before, false);
            }
            if let Some((language, text, version)) = opened {
                let _ = app.lsp.tx.send(LspCmd::DidOpen {
                    path,
                    language,
                    text,
                    version,
                });
            }
        }
        PromptKind::CreateFile { is_dir } => {
            let dir = app
                .explorer_entries
                .get(app.explorer_selected)
                .map(|e| {
                    if e.is_dir {
                        e.path.clone()
                    } else {
                        e.path.parent().unwrap_or(&app.root).to_path_buf()
                    }
                })
                .unwrap_or_else(|| app.root.clone());
            let path = dir.join(input);
            if let Err(err) = create_path(&path, is_dir) {
                app.message = Some(format!("create failed: {err}"));
            } else {
                if !is_dir {
                    app.open_path(path);
                }
                app.refresh_explorer();
                app.refresh_git_status();
            }
        }
        PromptKind::RenameFile => {
            if let Some(entry) = app.explorer_entries.get(app.explorer_selected).cloned() {
                let to = entry.path.parent().unwrap_or(&app.root).join(&input);
                let from = entry.path;
                if to == from {
                    return;
                }
                if matches!(app.lsp_status, LspStatus::Ready) {
                    let token = app.next_token();
                    let _ = app.lsp.tx.send(LspCmd::WillRename {
                        token,
                        from: from.clone(),
                        to: to.clone(),
                    });
                    app.pending_rename = Some(PendingRename { token, from, to });
                } else {
                    rename_without_imports(app, from, to);
                }
            }
        }
        PromptKind::AgentEdit => {
            let message = input.trim().to_string();
            if message.is_empty() {
                return;
            }
            if app.thread.prior.is_empty() {
                app.thread.style = PromptStyle::Edit;
            }
            app.thread.anchor = capture_anchor(app);
            app.thread_visible = true;
            app.focus = Focus::Thread;
            send_message(app, message);
        }
        PromptKind::RenameSymbol => {
            let Some((path, position)) = lsp_target(app) else {
                return;
            };
            let token = app.next_token();
            let _ = app.lsp.tx.send(LspCmd::Rename {
                token,
                path,
                position,
                new_name: input,
            });
        }
    }
}

fn open_prompt(app: &mut App, title: &str, input: String, kind: PromptKind) {
    let cursor = input.chars().count();
    app.overlay = Overlay::Prompt {
        title: title.into(),
        input,
        cursor,
        kind,
    };
}

pub fn insert_prompt(app: &mut App, text: &str) -> bool {
    let Overlay::Prompt { input, cursor, .. } = &mut app.overlay else {
        return false;
    };
    let mut chars: Vec<char> = input.chars().collect();
    let at = (*cursor).min(chars.len());
    let extra: Vec<char> = text
        .chars()
        .filter(|ch| *ch != '\n' && *ch != '\r')
        .collect();
    let added = extra.len();
    chars.splice(at..at, extra);
    *input = chars.into_iter().collect();
    *cursor = at + added;
    true
}

fn delete_prompt_before(app: &mut App) -> bool {
    let Overlay::Prompt { input, cursor, .. } = &mut app.overlay else {
        return false;
    };
    let mut chars: Vec<char> = input.chars().collect();
    let at = (*cursor).min(chars.len());
    if at == 0 {
        return true;
    }
    chars.remove(at - 1);
    *input = chars.into_iter().collect();
    *cursor = at - 1;
    true
}

pub fn delete_prompt_forward(app: &mut App) {
    let Overlay::Prompt { input, cursor, .. } = &mut app.overlay else {
        return;
    };
    let mut chars: Vec<char> = input.chars().collect();
    let at = (*cursor).min(chars.len());
    if at >= chars.len() {
        return;
    }
    chars.remove(at);
    *input = chars.into_iter().collect();
}

pub fn move_prompt_cursor(app: &mut App, code: crossterm::event::KeyCode) {
    use crossterm::event::KeyCode;
    let Overlay::Prompt { input, cursor, .. } = &mut app.overlay else {
        return;
    };
    let len = input.chars().count();
    let at = (*cursor).min(len);
    *cursor = match code {
        KeyCode::Left => at.saturating_sub(1),
        KeyCode::Right => (at + 1).min(len),
        KeyCode::Home => 0,
        KeyCode::End => len,
        _ => at,
    };
}

pub fn click_prompt(app: &mut App, x: u16, y: u16) -> bool {
    let inside = app.overlay_area.is_some_and(|(ax, ay, width, height)| {
        x >= ax && x < ax.saturating_add(width) && y >= ay && y < ay.saturating_add(height)
    });
    if !inside {
        return false;
    }
    if let Some((origin_x, origin_y, skip)) = app.prompt_caret
        && y == origin_y
        && let Overlay::Prompt { input, cursor, .. } = &mut app.overlay
    {
        let col = x.saturating_sub(origin_x) as usize;
        let rest: String = input.chars().skip(skip).collect();
        *cursor = skip + index_at_width(&rest, col);
    }
    true
}

pub(crate) fn prompt_window(input: &str, cursor: usize, room: usize) -> (String, usize, usize) {
    if room == 0 {
        return (String::new(), 0, 0);
    }
    let chars: Vec<char> = input.chars().collect();
    let cursor = cursor.min(chars.len());
    let mut start = 0;
    while start < cursor && display_width(&chars[start..cursor]) >= room {
        start += 1;
    }
    let mut end = chars.len();
    while end > start && display_width(&chars[start..end]) > room {
        end -= 1;
    }
    let col = display_width(&chars[start..cursor]);
    let visible: String = chars[start..end].iter().collect();
    (visible, col, start)
}

pub(crate) fn index_at_width(text: &str, width: usize) -> usize {
    let mut used = 0usize;
    for (index, ch) in text.chars().enumerate() {
        let ch_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + ch_width > width {
            return index;
        }
        used += ch_width;
    }
    text.chars().count()
}

fn display_width(chars: &[char]) -> usize {
    chars
        .iter()
        .map(|ch| unicode_width::UnicodeWidthChar::width(*ch).unwrap_or(0))
        .sum()
}

pub fn overlay_char(app: &mut App, ch: char) {
    if insert_prompt(app, &ch.to_string()) {
        return;
    }
    match &mut app.overlay {
        Overlay::Search { query, .. } => {
            query.push(ch);
        }
        Overlay::Branches { query, .. } => {
            query.push(ch);
        }
        _ => {}
    }
    refresh_overlay_query(app);
    sync_branch_query(app);
}

pub fn overlay_insert(app: &mut App, text: &str) -> bool {
    if !matches!(
        app.overlay,
        Overlay::Search { .. } | Overlay::Prompt { .. } | Overlay::Branches { .. }
    ) {
        return false;
    }
    let text = sanitize_overlay_paste(text);
    if text.is_empty() {
        return true;
    }
    if insert_prompt(app, &text) {
        return true;
    }
    match &mut app.overlay {
        Overlay::Search { query, .. } => {
            query.push_str(&text);
        }
        Overlay::Branches { query, .. } => {
            query.push_str(&text);
        }
        _ => return true,
    }
    refresh_overlay_query(app);
    sync_branch_query(app);
    true
}

fn sanitize_overlay_paste(text: &str) -> String {
    text.chars()
        .filter_map(|ch| match ch {
            '\n' | '\r' => Some(' '),
            ch if ch.is_control() => None,
            ch => Some(ch),
        })
        .collect()
}

fn overlay_backspace(app: &mut App) -> bool {
    if delete_prompt_before(app) {
        return true;
    }
    let handled = match &mut app.overlay {
        Overlay::Search { query, .. } => {
            query.pop();
            true
        }
        Overlay::Branches { query, .. } => {
            query.pop();
            true
        }
        Overlay::None
        | Overlay::Completion { .. }
        | Overlay::Hover { .. }
        | Overlay::Signature { .. } => false,
        _ => true,
    };
    if handled {
        refresh_overlay_query(app);
        sync_branch_query(app);
    }
    handled
}

fn sync_branch_query(app: &mut App) {
    let Overlay::Branches {
        query,
        branches,
        loading,
        ..
    } = &app.overlay
    else {
        return;
    };
    let query = query.clone();
    let branches = branches.clone();
    let loading = *loading;
    let len = crate::workspace::branch_choice_count(&branches, &query, loading);
    let selected = if query.trim().is_empty() {
        app.git_branch
            .as_ref()
            .and_then(|name| {
                crate::workspace::filter_branches(&branches, &query)
                    .iter()
                    .position(|branch| branch == name)
            })
            .unwrap_or(0)
    } else {
        0
    };
    let selected = if len == 0 { 0 } else { selected.min(len - 1) };
    if let Overlay::Branches {
        selected: slot,
        scroll,
        error,
        ..
    } = &mut app.overlay
    {
        *slot = selected;
        *scroll = 0;
        *error = None;
    }
}

fn refresh_overlay_query(app: &mut App) {
    let Overlay::Search {
        kind,
        mut scope,
        query,
        selected,
        scroll,
        matches,
        ..
    } = app.overlay.clone()
    else {
        return;
    };
    if kind == SearchKind::Content && scope == SearchScope::File {
        if app.active_doc().is_none() {
            scope = SearchScope::Global;
        } else {
            let file_matches = file_match_list(app, &query);
            let selected = selected.min(file_matches.len().saturating_sub(1));
            app.search_generation = app.search_generation.wrapping_add(1);
            app.overlay = Overlay::Search {
                kind,
                scope,
                query,
                files: Vec::new(),
                matches: Vec::new(),
                file_matches,
                selected,
                scroll: 0,
                searching: false,
            };
            jump_search(app);
            return;
        }
    }
    let files = if kind == SearchKind::Find {
        app.file_finder.query(&query)
    } else {
        Vec::new()
    };
    let searching = !query.trim().is_empty();
    let matches = if searching { matches } else { Vec::new() };
    app.search_generation = app.search_generation.wrapping_add(1);
    if searching {
        crate::workspace::spawn_workspace_search(
            app.root.clone(),
            query.clone(),
            app.search_generation,
            app.search_tx.clone(),
        );
    }
    let len = files.len() + matches.len();
    let (selected, scroll) = fit_search_window(selected, scroll, len, app.search_list_rows, true);
    app.overlay = Overlay::Search {
        kind,
        scope,
        query,
        selected,
        scroll,
        files,
        matches,
        file_matches: Vec::new(),
        searching,
    };
}

pub(crate) fn fit_search_window(
    selected: usize,
    scroll: usize,
    len: usize,
    visible: usize,
    follow_selection: bool,
) -> (usize, usize) {
    let visible = visible.max(1);
    if len == 0 {
        return (0, 0);
    }
    let selected = selected.min(len - 1);
    let max_scroll = len.saturating_sub(visible);
    let scroll = scroll.min(max_scroll);
    if follow_selection {
        let scroll = if selected < scroll {
            selected
        } else if selected >= scroll + visible {
            selected + 1 - visible
        } else {
            scroll
        };
        (selected, scroll.min(max_scroll))
    } else if selected < scroll {
        (scroll, scroll)
    } else if selected >= scroll + visible {
        ((scroll + visible - 1).min(len - 1), scroll)
    } else {
        (selected, scroll)
    }
}

fn overlay_move(app: &mut App, movement: Movement) {
    let down = matches!(movement, Movement::Down | Movement::Right);
    let up = matches!(movement, Movement::Up | Movement::Left);
    let visible = app.search_list_rows;
    match &mut app.overlay {
        Overlay::Search {
            kind,
            scope,
            files,
            matches,
            file_matches,
            selected,
            scroll,
            ..
        } => {
            if *kind == SearchKind::Content && *scope == SearchScope::File {
                if file_matches.is_empty() {
                    return;
                }
                if down {
                    *selected = (*selected + 1) % file_matches.len();
                }
                if up {
                    *selected = selected.checked_sub(1).unwrap_or(file_matches.len() - 1);
                }
                jump_search(app);
                return;
            }
            if down {
                *selected = selected.saturating_add(1);
            }
            if up {
                *selected = selected.saturating_sub(1);
            }
            let len = files.len() + matches.len();
            let (next_selected, next_scroll) =
                fit_search_window(*selected, *scroll, len, visible, true);
            *selected = next_selected;
            *scroll = next_scroll;
        }
        Overlay::References { items, selected } => {
            if down {
                *selected = (*selected + 1).min(items.len().saturating_sub(1));
            }
            if up {
                *selected = selected.saturating_sub(1);
            }
        }
        Overlay::ContextMenu {
            items, selected, ..
        } => {
            if down {
                *selected = (*selected + 1).min(items.len().saturating_sub(1));
            }
            if up {
                *selected = selected.saturating_sub(1);
            }
        }
        Overlay::Themes { selected } => {
            let last = crate::ui::theme::theme_names().len().saturating_sub(1);
            if down {
                *selected = (*selected + 1).min(last);
            }
            if up {
                *selected = selected.saturating_sub(1);
            }
        }
        Overlay::Branches {
            branches,
            query,
            selected,
            scroll,
            loading,
            ..
        } => {
            let len = crate::workspace::branch_choice_count(branches, query, *loading);
            if len == 0 {
                return;
            }
            if down {
                *selected = selected.saturating_add(1);
            }
            if up {
                *selected = selected.saturating_sub(1);
            }
            let (next_selected, next_scroll) =
                fit_search_window(*selected, *scroll, len, visible, true);
            *selected = next_selected;
            *scroll = next_scroll;
        }
        _ => {}
    }
}

pub fn click_search_row(app: &mut App, x: u16, y: u16) -> Option<bool> {
    let Some((row_x, width)) = app.search_row_span else {
        return None;
    };
    if x < row_x || x >= row_x.saturating_add(width) {
        return None;
    }
    let Some(&(index, _)) = app.search_rows.iter().find(|(_, row)| *row == y) else {
        return None;
    };
    let open = app.last_click.is_some_and(|(t, lx, ly)| {
        ly == y
            && lx >= row_x
            && lx < row_x.saturating_add(width)
            && t.elapsed() < std::time::Duration::from_millis(400)
    }) && matches!(&app.overlay, Overlay::Search { selected, .. } if *selected == index);
    let Overlay::Search { selected, .. } = &mut app.overlay else {
        return None;
    };
    *selected = index;
    Some(open)
}

pub fn scroll_search(app: &mut App, delta: i32) {
    let visible = app.search_list_rows;
    let Overlay::Search {
        kind,
        scope,
        files,
        matches,
        selected,
        scroll,
        ..
    } = &mut app.overlay
    else {
        return;
    };
    if *kind == SearchKind::Content && *scope == SearchScope::File {
        return;
    }
    let len = files.len() + matches.len();
    let page = visible.max(1);
    let max_scroll = len.saturating_sub(page);
    let next = (*scroll as i64 + i64::from(delta)).clamp(0, max_scroll as i64) as usize;
    let (next_selected, next_scroll) = fit_search_window(*selected, next, len, visible, false);
    *selected = next_selected;
    *scroll = next_scroll;
}

pub fn click_branch_row(app: &mut App, x: u16, y: u16) -> bool {
    if !matches!(app.overlay, Overlay::Branches { .. }) {
        return false;
    }
    let Some((row_x, width)) = app.search_row_span else {
        return false;
    };
    if x < row_x || x >= row_x.saturating_add(width) {
        return false;
    }
    let Some(&(index, _)) = app.search_rows.iter().find(|(_, row)| *row == y) else {
        return false;
    };
    if let Overlay::Branches { selected, .. } = &mut app.overlay {
        *selected = index;
    }
    true
}

pub fn scroll_branches(app: &mut App, delta: i32) {
    let visible = app.search_list_rows;
    let (len, selected, scroll) = match &app.overlay {
        Overlay::Branches {
            branches,
            query,
            selected,
            scroll,
            loading,
            ..
        } => (
            crate::workspace::branch_choice_count(branches, query, *loading),
            *selected,
            *scroll,
        ),
        _ => return,
    };
    let page = visible.max(1);
    let max_scroll = len.saturating_sub(page);
    let next = (scroll as i64 + i64::from(delta)).clamp(0, max_scroll as i64) as usize;
    let (next_selected, next_scroll) = fit_search_window(selected, next, len, visible, false);
    if let Overlay::Branches {
        selected, scroll, ..
    } = &mut app.overlay
    {
        *selected = next_selected;
        *scroll = next_scroll;
    }
}

pub fn overlay_submit(app: &mut App) {
    match app.overlay.clone() {
        Overlay::Search {
            kind: SearchKind::Content,
            scope: SearchScope::File,
            ..
        } => {}
        Overlay::Search {
            files,
            matches,
            selected,
            ..
        } => {
            if selected < files.len() {
                if let Some(path) = files.get(selected) {
                    app.open_path(app.root.join(path));
                }
            } else if let Some(item) = matches.get(selected - files.len()).cloned() {
                app.open_path(item.path);
                if let Some(id) = app.active {
                    jump_to(app, id, Position::new(item.line, item.column));
                }
            }
            app.overlay = Overlay::None;
        }
        Overlay::References { items, selected } => {
            if let Some(item) = items.get(selected).cloned() {
                app.open_path(item.path);
                let Some(id) = app.active else { return };
                if let Some(pos) = app
                    .documents
                    .get(&id)
                    .map(|doc| decode_diagnostic_pos(doc, item.position))
                {
                    jump_to(app, id, pos);
                }
            }
            app.overlay = Overlay::None;
        }
        Overlay::Confirm { .. } => confirm_yes(app),
        Overlay::Prompt { .. } => submit_prompt(app),
        Overlay::Completion { .. } => accept_completion(app),
        Overlay::ContextMenu {
            items, selected, ..
        } => {
            if let Some(item) = items.get(selected).cloned() {
                app.overlay = Overlay::None;
                run_menu(app, item.action);
            }
        }
        Overlay::Themes { selected } => {
            if let Some(name) = crate::ui::theme::theme_names().get(selected) {
                app.set_theme(name);
            }
            app.overlay = Overlay::None;
        }
        Overlay::Help => {}
        Overlay::TypesNote => dismiss_types_note(app),
        Overlay::Branches { .. } => submit_branches(app),
        _ => app.overlay = Overlay::None,
    }
}

fn submit_branches(app: &mut App) {
    if app.git_busy() {
        return;
    }
    let Overlay::Branches {
        query,
        branches,
        selected,
        loading,
        ..
    } = app.overlay.clone()
    else {
        return;
    };
    if loading {
        return;
    }
    let filtered = crate::workspace::filter_branches(&branches, &query);
    if filtered.is_empty() {
        let name = query.trim();
        if name.is_empty() {
            return;
        }
        if let Some(message) = crate::workspace::branch_name_error(name) {
            set_branch_error(app, message);
            return;
        }
        app.enqueue_git_write(GitWrite::CreateBranch(name.to_string()));
        return;
    }
    let name = filtered
        .get(selected)
        .cloned()
        .unwrap_or_else(|| filtered[0].clone());
    if app.git_branch.as_deref() == Some(name.as_str()) {
        app.overlay = Overlay::None;
        return;
    }
    if let Some(message) = crate::workspace::branch_name_error(&name) {
        set_branch_error(app, message);
        return;
    }
    app.enqueue_git_write(GitWrite::Switch(name));
}

fn set_branch_error(app: &mut App, message: &str) {
    if let Overlay::Branches { error, .. } = &mut app.overlay {
        *error = Some(message.to_string());
    }
}

pub fn open_help(app: &mut App) {
    app.overlay = if matches!(app.overlay, Overlay::Help) {
        Overlay::None
    } else {
        Overlay::Help
    };
}

pub fn open_themes(app: &mut App) {
    let names = crate::ui::theme::theme_names();
    let selected = names
        .iter()
        .position(|name| *name == app.theme().name)
        .unwrap_or(0);
    app.overlay = Overlay::Themes { selected };
}

pub fn click_help(app: &mut App, y: u16) {
    if app.overlay_rows.iter().any(|(_, row)| *row == y) {
        toggle_claude(app);
    }
}

pub fn toggle_claude(app: &mut App) {
    app.config.claude_enabled = !app.config.claude_enabled;
    if !app.config.claude_enabled {
        if app.thread.running {
            stop_agent(app);
        }
        app.thread_visible = false;
        if app.focus == Focus::Thread {
            app.focus = Focus::Editor;
        }
    }
    let state = if app.config.claude_enabled {
        "on"
    } else {
        "off"
    };
    app.message = if let Err(err) = app.config.save() {
        Some(format!("Claude {state} (not saved: {err})"))
    } else {
        Some(format!("Claude {state}"))
    };
}

pub fn claude_ready(app: &mut App) -> bool {
    if !app.config.claude_enabled {
        app.message = Some("Claude is off. Turn it on in Settings".into());
        return false;
    }
    if !app.claude_installed {
        app.message = Some("claude is not installed".into());
        return false;
    }
    true
}

pub fn click_themes(app: &mut App, y: u16) {
    let Some(index) = app
        .overlay_rows
        .iter()
        .find(|(_, row)| *row == y)
        .map(|row| row.0)
    else {
        return;
    };
    if let Some(name) = crate::ui::theme::theme_names().get(index) {
        app.set_theme(name);
    }
    app.overlay = Overlay::None;
}

pub fn click_types_status(app: &mut App) {
    if which::which("typescript-language-server").is_ok() {
        refresh_types(app);
    } else {
        app.overlay = Overlay::TypesNote;
    }
}

pub fn refresh_types(app: &mut App) {
    app.lsp_status = LspStatus::Starting;
    let _ = app.lsp.tx.send(LspCmd::Restart {
        root: app.root.clone(),
    });
}

pub const TYPES_INSTALL: &str = "npm install -g typescript typescript-language-server";

pub fn click_types_note(app: &mut App, x: u16, y: u16) {
    let Some((hit_x, hit_y, width)) = app.types_copy else {
        return;
    };
    if y == hit_y && x >= hit_x && x < hit_x.saturating_add(width) {
        app.clipboard.copy(TYPES_INSTALL);
        app.message = Some("Copied".into());
    }
}

pub fn dismiss_types_note(app: &mut App) {
    app.overlay = Overlay::None;
    if app.config.types_note_dismissed {
        return;
    }
    app.config.types_note_dismissed = true;
    if let Err(err) = app.config.save() {
        app.message = Some(format!("note not saved: {err}"));
    }
}

pub fn dismiss_overlay(app: &mut App) {
    if matches!(app.overlay, Overlay::TypesNote) {
        dismiss_types_note(app);
        return;
    }
    if !matches!(app.overlay, Overlay::None) {
        app.overlay = Overlay::None;
    }
}

fn dismiss_soft_overlays(app: &mut App) {
    if matches!(
        app.overlay,
        Overlay::Completion { .. } | Overlay::Hover { .. } | Overlay::Signature { .. }
    ) {
        app.overlay = Overlay::None;
    }
}

pub fn jump_to(app: &mut App, id: crate::editor::DocumentId, pos: Position) {
    place_and_reveal(app, id, |view, rope| set_cursor(view, rope, pos, false));
}

fn place_and_reveal(
    app: &mut App,
    id: crate::editor::DocumentId,
    place: impl FnOnce(&mut EditorView, &Rope),
) {
    let height = app.editor_area.3.saturating_sub(2).max(1) as usize;
    let width = app.editor_area.2.saturating_sub(app.gutter_width) as usize;
    let tab_width = app.config.tab_width;
    if let (Some(doc), Some(view)) = (app.documents.get(&id), app.views.get_mut(&id)) {
        place(view, &doc.rope);
        view.ensure_visible(&doc.rope, height, width, tab_width);
    }
    reveal_cursor(app, id);
}

fn jump_search(app: &mut App) {
    let Overlay::Search {
        kind: SearchKind::Content,
        scope: SearchScope::File,
        file_matches,
        selected,
        ..
    } = app.overlay.clone()
    else {
        return;
    };
    let Some(&(start, end)) = file_matches.get(selected) else {
        return;
    };
    let Some(id) = app.active else { return };
    place_and_reveal(app, id, |view, _rope| {
        view.cursor = end;
        view.selection = Some(Selection {
            anchor: start,
            cursor: end,
        });
    });
}

pub fn explorer_activate(app: &mut App) {
    let Some(entry) = app.explorer_entries.get(app.explorer_selected).cloned() else {
        return;
    };
    if entry.is_dir {
        if app.expanded.contains(&entry.path) {
            app.expanded.remove(&entry.path);
        } else {
            app.expanded.insert(entry.path);
        }
        app.refresh_explorer();
    } else {
        app.open_path(entry.path);
    }
}

pub fn editor_context_labels() -> [&'static str; 3] {
    ["Go to Definition", "Find References", "Ask Claude"]
}

pub fn show_editor_menu(app: &mut App, x: u16, y: u16) {
    let lsp_ok = matches!(app.lsp_status, LspStatus::Ready);
    let token = app.next_token();
    app.hover_token = token;
    app.overlay = Overlay::ContextMenu {
        x,
        y,
        selected: 0,
        show_type: true,
        type_text: if lsp_ok {
            "Loading type information…".into()
        } else {
            "Type information unavailable".into()
        },
        hover_token: token,
        items: vec![
            MenuItem {
                label: editor_context_labels()[0].into(),
                action: MenuAction::GoToDefinition,
                enabled: lsp_ok,
            },
            MenuItem {
                label: editor_context_labels()[1].into(),
                action: MenuAction::FindReferences,
                enabled: lsp_ok,
            },
            MenuItem {
                label: editor_context_labels()[2].into(),
                action: MenuAction::AskClaude,
                enabled: app.config.claude_enabled && app.claude_installed,
            },
        ],
    };
    if lsp_ok {
        if let Some((path, position)) = lsp_target(app) {
            let _ = app.lsp.tx.send(LspCmd::Hover {
                token,
                path,
                position,
            });
        }
    }
}

pub fn show_tab_menu(app: &mut App, document_id: crate::editor::DocumentId, x: u16, y: u16) {
    let others_open = app.order.len() > 1;
    app.overlay = Overlay::ContextMenu {
        x,
        y,
        selected: 0,
        show_type: false,
        type_text: String::new(),
        hover_token: 0,
        items: vec![
            MenuItem {
                label: "Close Others".into(),
                action: MenuAction::CloseOthers(document_id),
                enabled: others_open,
            },
            MenuItem {
                label: "Close All".into(),
                action: MenuAction::CloseAll,
                enabled: true,
            },
        ],
    };
}

pub fn show_explorer_menu(app: &mut App, x: u16, y: u16) {
    app.overlay = Overlay::ContextMenu {
        x,
        y,
        selected: 0,
        show_type: false,
        type_text: String::new(),
        hover_token: 0,
        items: vec![
            MenuItem {
                label: "New File".into(),
                action: MenuAction::NewFile,
                enabled: true,
            },
            MenuItem {
                label: "New Folder".into(),
                action: MenuAction::NewFolder,
                enabled: true,
            },
            MenuItem {
                label: "Rename".into(),
                action: MenuAction::RenameFile,
                enabled: true,
            },
            MenuItem {
                label: "Copy Path".into(),
                action: MenuAction::CopyPath,
                enabled: app.explorer_entries.get(app.explorer_selected).is_some(),
            },
            MenuItem {
                label: "Delete".into(),
                action: MenuAction::DeletePath,
                enabled: true,
            },
        ],
    };
}

pub fn show_git_menu(app: &mut App, x: u16, y: u16) {
    let rows = app.git_rows();
    let Some(crate::workspace::GitPanelRow::File { path, .. }) = rows.get(app.git_selected) else {
        return;
    };
    let Some(status) = app.git_status.get(path).copied() else {
        return;
    };
    if status.conflict.is_some() {
        app.message = Some("Resolve the conflict in the file".into());
        return;
    }
    if whole_file_action(status).is_none() {
        app.message = Some("A rename stays as it is".into());
        return;
    }
    let restore = matches!(whole_file_action(status), Some(WholeFileAction::Restore));
    app.overlay = Overlay::ContextMenu {
        x,
        y,
        selected: 0,
        show_type: false,
        type_text: String::new(),
        hover_token: 0,
        items: vec![MenuItem {
            label: if restore { "Restore" } else { "Discard" }.into(),
            action: if restore {
                MenuAction::RestoreFile
            } else {
                MenuAction::DiscardFile
            },
            enabled: true,
        }],
    };
}

fn resolve_delete(app: &mut App, path: PathBuf) {
    if path.exists() {
        if let Err(err) = delete_path(&path) {
            app.message = Some(format!("delete failed: {err}"));
            return;
        }
        let ids: Vec<_> = app
            .documents
            .iter()
            .filter(|(_, doc)| {
                doc.path
                    .as_ref()
                    .is_some_and(|item| item == &path || item.starts_with(&path))
            })
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            app.active = Some(id);
            app.close_active(true);
        }
        app.refresh_explorer();
    }
    app.git_publish_armed = false;
    app.enqueue_git_write(GitWrite::Stage(path));
    app.refresh_git_status();
}

fn confirm_selected_git_file(app: &mut App) {
    let rows = app.git_rows();
    let Some(crate::workspace::GitPanelRow::File { path, .. }) = rows.get(app.git_selected) else {
        return;
    };
    let path = path.clone();
    let Some(status) = app.git_status.get(&path).copied() else {
        return;
    };
    let name = app.relative(&path);
    let (message, action) = match whole_file_action(status) {
        Some(WholeFileAction::Restore) => {
            (format!("Restore {name}?"), ConfirmAction::RestoreFile(path))
        }
        Some(WholeFileAction::Remove) => (
            format!("Discard {name}?"),
            ConfirmAction::DiscardAdded(path),
        ),
        Some(WholeFileAction::Delete) => {
            (format!("Delete {name}?"), ConfirmAction::DeletePath(path))
        }
        None => return,
    };
    app.overlay = Overlay::Confirm { message, action };
}

pub fn confirm_restore_active(app: &mut App) {
    let Some(id) = app.active else {
        return;
    };
    if !app.absent.contains(&id) {
        return;
    }
    let Some(path) = app.documents.get(&id).and_then(|doc| doc.path.clone()) else {
        return;
    };
    let name = app.relative(&path);
    app.overlay = Overlay::Confirm {
        message: format!("Restore {name}?"),
        action: ConfirmAction::RestoreFile(path),
    };
}

pub fn complete_file_rename(app: &mut App, token: u64, items: Vec<TextEdit>, failed: bool) {
    let Some(pending) = app.pending_rename.clone() else {
        return;
    };
    if pending.token != token {
        return;
    }
    app.pending_rename = None;
    apply_and_rename(app, &pending.from, &pending.to, &items, failed);
}

pub fn abandon_file_rename(app: &mut App) {
    let Some(pending) = app.pending_rename.take() else {
        return;
    };
    apply_and_rename(app, &pending.from, &pending.to, &[], true);
}

fn rename_without_imports(app: &mut App, from: PathBuf, to: PathBuf) {
    apply_and_rename(app, &from, &to, &[], true);
}

fn apply_and_rename(
    app: &mut App,
    from: &std::path::Path,
    to: &std::path::Path,
    items: &[TextEdit],
    failed: bool,
) {
    if !failed && !items.is_empty() {
        let paths: Vec<_> = items.iter().map(|edit| edit.path.clone()).collect();
        app.apply_edits(items.to_vec());
        save_paths(app, &paths);
    }
    if let Err(err) = rename_path(from, to) {
        app.message = Some(format!("rename failed: {err}"));
        return;
    }
    retarget_documents(app, from, to);
    app.refresh_explorer();
    app.refresh_git_status();
    app.message = Some(if failed {
        "Renamed. Imports were not updated.".into()
    } else {
        "Renamed".into()
    });
}

fn save_paths(app: &mut App, paths: &[PathBuf]) {
    let mut seen = std::collections::HashSet::new();
    for path in paths {
        if !seen.insert(path.clone()) {
            continue;
        }
        let Some(id) = app
            .documents
            .iter()
            .find_map(|(id, doc)| (doc.path.as_deref() == Some(path.as_path())).then_some(*id))
        else {
            continue;
        };
        if let Some(doc) = app.documents.get_mut(&id)
            && doc.is_dirty()
            && let Err(err) = doc.save()
        {
            app.message = Some(format!("save failed: {err}"));
        }
    }
}

fn retarget_documents(app: &mut App, from: &std::path::Path, to: &std::path::Path) {
    let ids: Vec<DocumentId> = app
        .documents
        .iter()
        .filter_map(|(id, doc)| {
            let path = doc.path.as_ref()?;
            (path == from || path.starts_with(from)).then_some(*id)
        })
        .collect();
    for id in ids {
        let Some(old) = app.documents.get(&id).and_then(|doc| doc.path.clone()) else {
            continue;
        };
        let new_path = if old == from {
            to.to_path_buf()
        } else {
            to.join(old.strip_prefix(from).unwrap_or(std::path::Path::new("")))
        };
        let language = crate::editor::LanguageId::from_path(&new_path);
        let (text, revision, rope) = {
            let Some(doc) = app.documents.get_mut(&id) else {
                continue;
            };
            doc.path = Some(new_path.clone());
            doc.language = language;
            (doc.text(), doc.revision, doc.rope.clone())
        };
        app.syntax.close(id);
        app.syntax.open(id, language, &rope);
        let _ = app.lsp.tx.send(LspCmd::DidClose { path: old });
        let _ = app.lsp.tx.send(LspCmd::DidOpen {
            path: new_path,
            language,
            text,
            version: revision as i64,
        });
    }
}

pub fn run_menu(app: &mut App, action: MenuAction) {
    match action {
        MenuAction::GoToDefinition => go_to_definition(app),
        MenuAction::FindReferences => find_references(app),
        MenuAction::Hover => hover(app),
        MenuAction::RenameSymbol => start_rename_symbol(app),
        MenuAction::Copy => copy(app),
        MenuAction::Cut => cut(app),
        MenuAction::Paste => paste(app),
        MenuAction::NewFile => {
            open_prompt(
                app,
                "New file",
                String::new(),
                PromptKind::CreateFile { is_dir: false },
            );
        }
        MenuAction::NewFolder => {
            open_prompt(
                app,
                "New folder",
                String::new(),
                PromptKind::CreateFile { is_dir: true },
            );
        }
        MenuAction::RenameFile => {
            let name = app
                .explorer_entries
                .get(app.explorer_selected)
                .map(|e| e.name.clone())
                .unwrap_or_default();
            open_prompt(app, "Rename", name, PromptKind::RenameFile);
        }
        MenuAction::CopyPath => copy_path(app),
        MenuAction::CloseOthers(id) => {
            let ids: Vec<_> = app
                .order
                .iter()
                .copied()
                .filter(|open| *open != id)
                .collect();
            app.close_documents(&ids, false);
        }
        MenuAction::CloseAll => {
            let ids = app.order.clone();
            app.close_documents(&ids, false);
        }
        MenuAction::AskClaude => start_thread(app),
        MenuAction::DiscardFile => confirm_selected_git_file(app),
        MenuAction::RestoreFile => confirm_selected_git_file(app),
        MenuAction::DeletePath => {
            if let Some(entry) = app.explorer_entries.get(app.explorer_selected) {
                app.overlay = Overlay::Confirm {
                    message: format!("Delete {}?", entry.name),
                    action: ConfirmAction::DeletePath(entry.path.clone()),
                };
            }
        }
    }
}

pub fn maybe_quit(app: &mut App) {
    let dirty = app.documents.values().any(|d| d.is_dirty());
    if dirty {
        app.overlay = Overlay::Confirm {
            message: "Unsaved changes. Quit anyway?".into(),
            action: ConfirmAction::Quit,
        };
    } else {
        app.should_quit = true;
    }
}

fn utf16(doc: &crate::editor::Document, pos: Position) -> Position {
    Position::new(
        pos.line,
        crate::editor::position_to_utf16(&doc.rope, pos) as usize,
    )
}

pub fn handle_watch(app: &mut App, event: WatchEvent) {
    app.schedule_git_status();
    if event.kind.changes_tree() {
        app.refresh_explorer();
    }
    let path = event.path;
    let Some((id, dirty, known_mtime)) = app.documents.iter().find_map(|(id, doc)| {
        if doc.path.as_ref() == Some(&path) {
            Some((*id, doc.is_dirty(), doc.mtime))
        } else {
            None
        }
    }) else {
        return;
    };
    if let Ok(meta) = std::fs::metadata(&path) {
        if let (Ok(modified), Some(known)) = (meta.modified(), known_mtime) {
            if modified <= known + std::time::Duration::from_millis(750) {
                return;
            }
        }
    }
    if app.thread.running {
        match refresh_from_disk(app, id) {
            DiskRefresh::Dirty(path) => {
                app.message = Some(format!("file changed on disk: {}", path.display()));
            }
            DiskRefresh::Reloaded | DiskRefresh::Same | DiskRefresh::Skipped => {}
        }
        return;
    }
    if dirty {
        app.message = Some(format!("file changed on disk: {}", path.display()));
    } else {
        app.overlay = Overlay::Confirm {
            message: format!("Reload {}?", path.display()),
            action: ConfirmAction::Reload(id),
        };
    }
}

pub fn click_editor(app: &mut App, x: u16, y: u16, selecting: bool) {
    let Some(id) = app.active else { return };
    if app.markdown_preview_open() {
        return;
    }
    if !selecting {
        if let Some(hit) = app.blame_hit.clone()
            && y == hit.y
            && x >= hit.x
            && x < hit.x.saturating_add(hit.width)
        {
            app.overlay = Overlay::Blame {
                author: hit.author,
                summary: hit.summary,
                hash: hit.hash,
                time: hit.time,
                x: hit.x,
                y: hit.y,
            };
            return;
        }
        if let Some(hit) = app
            .diff_hits
            .iter()
            .find(|hit| y == hit.y && x >= hit.x && x < hit.x.saturating_add(hit.width))
            .copied()
        {
            app.request_hunk(hit.anchor, hit.action);
            if hit.action == crate::app::state::HunkAction::Revert {
                reveal_cursor(app, id);
            }
            return;
        }
    }
    if matches!(
        visual_row_at(app, x, y),
        Some(crate::workspace::VisualRow::Removed { .. } | crate::workspace::VisualRow::Bar { .. })
    ) {
        return;
    }
    let Some(pos) = editor_position_at(app, x, y) else {
        return;
    };
    let eh = app.editor_area.3;
    if let (Some(doc), Some(view)) = (app.documents.get(&id), app.views.get_mut(&id)) {
        set_cursor(view, &doc.rope, pos, selecting);
        view.ensure_visible(
            &doc.rope,
            eh as usize,
            app.editor_area.2.saturating_sub(app.gutter_width) as usize,
            app.config.tab_width,
        );
    }
    reveal_cursor(app, id);
}

fn visual_row_at(app: &App, x: u16, y: u16) -> Option<crate::workspace::VisualRow> {
    let id = app.active?;
    let (ex, ey, _ew, eh) = app.editor_area;
    if y <= ey || x <= ex {
        return None;
    }
    let rel_y = y.saturating_sub(ey).saturating_sub(1);
    if rel_y >= eh.saturating_sub(2) {
        return None;
    }
    let visual = app.views.get(&id)?.viewport.top + rel_y as usize;
    app.visual_rows(id).get(visual).cloned()
}

pub fn editor_position_at(app: &App, x: u16, y: u16) -> Option<Position> {
    let id = app.active?;
    let doc = app.documents.get(&id)?;
    let view = app.views.get(&id)?;
    let (ex, ey, _ew, eh) = app.editor_area;
    if y <= ey || x <= ex {
        return None;
    }
    let rel_x = x.saturating_sub(ex).saturating_sub(1);
    let rel_y = y.saturating_sub(ey).saturating_sub(1);
    if rel_y >= eh.saturating_sub(2) || rel_x < app.gutter_width {
        return None;
    }
    let visual = view.viewport.top + rel_y as usize;
    let line = match app.visual_rows(id).get(visual) {
        Some(crate::workspace::VisualRow::Line(line)) => *line,
        _ => return None,
    };
    if line >= doc.rope.len_lines() {
        return None;
    }
    let display = view.viewport.left + (rel_x - app.gutter_width) as usize;
    Some(crate::editor::position_at_display(
        &doc.rope,
        line,
        display,
        app.config.tab_width,
    ))
}

pub fn double_click_editor(app: &mut App, x: u16, y: u16) {
    click_editor(app, x, y, false);
    let Some(id) = app.active else { return };
    if let (Some(doc), Some(view)) = (app.documents.get(&id), app.views.get_mut(&id)) {
        let pos = view.cursor;
        select_word_at(view, &doc.rope, pos);
    }
}

pub fn reveal_cursor(app: &mut App, id: crate::editor::DocumentId) {
    let line = app.views.get(&id).map(|view| view.cursor.line).unwrap_or(0);
    let rows = app.visual_rows(id);
    let index = crate::workspace::visual_index_for_line(&rows, line).unwrap_or(0);
    let height = app.editor_area.3.saturating_sub(2).max(1) as usize;
    if let Some(view) = app.views.get_mut(&id) {
        if index < view.viewport.top {
            view.viewport.top = index;
        } else if index >= view.viewport.top + height {
            view.viewport.top = index + 1 - height;
        }
    }
}

pub fn scroll_editor(app: &mut App, delta: i32) {
    if app.markdown_preview_open() {
        app.scroll_markdown_preview(delta);
        return;
    }
    let Some(id) = app.active else { return };
    let max = app.visual_rows(id).len().saturating_sub(1);
    if let Some(view) = app.views.get_mut(&id) {
        let next = view.viewport.top as i32 + delta;
        view.viewport.top = next.clamp(0, max as i32) as usize;
    }
}

pub fn scroll_explorer(app: &mut App, delta: i32) {
    if app.git_open {
        let len = app.git_rows().len();
        let inner = git_list_rows(app.explorer_area.3, app.git_body_open);
        let max = max_explorer_scroll(len, inner);
        let next = app.git_scroll as i64 + i64::from(delta);
        app.git_scroll = next.clamp(0, max as i64) as usize;
        return;
    }
    let inner = explorer_inner_rows(app.explorer_area.3);
    let max = max_explorer_scroll(app.explorer_entries.len(), inner);
    let next = app.explorer_scroll as i64 + i64::from(delta);
    app.explorer_scroll = next.clamp(0, max as i64) as usize;
}

pub fn reveal_explorer_selection(app: &mut App) {
    let inner = explorer_inner_rows(app.explorer_area.3);
    if inner == 0 || app.explorer_entries.is_empty() {
        return;
    }
    let sel = app
        .explorer_selected
        .min(app.explorer_entries.len().saturating_sub(1));
    if sel < app.explorer_scroll {
        app.explorer_scroll = sel;
    } else if sel >= app.explorer_scroll + inner {
        app.explorer_scroll = sel + 1 - inner;
    }
    app.explorer_scroll =
        clamp_explorer_scroll(app.explorer_scroll, app.explorer_entries.len(), inner);
}

pub fn scroll_editor_horizontal(app: &mut App, delta: i32) {
    if app.markdown_preview_open() {
        return;
    }
    let Some(id) = app.active else { return };
    let current = app
        .views
        .get(&id)
        .map(|view| view.viewport.left)
        .unwrap_or(0);
    assign_horizontal(app, current as i32 + delta);
}

pub fn begin_explorer_resize(app: &mut App, x: u16, y: u16) -> bool {
    if !app.explorer_visible || app.explorer_area.2 == 0 {
        return false;
    }
    let edge = app.explorer_area.0 + app.explorer_area.2 - 1;
    let top = app.search_area.1.min(app.explorer_area.1);
    let bottom = app.explorer_area.1.saturating_add(app.explorer_area.3);
    if x != edge || y < top || y >= bottom {
        return false;
    }
    app.explorer_resizing = true;
    true
}

pub fn drag_explorer_resize(app: &mut App, x: u16) {
    if !app.explorer_resizing {
        return;
    }
    app.config.explorer_width = crate::ui::layout::stored_explorer_width(
        app.status_area.2,
        app.thread_visible,
        app.explorer_area.0,
        x,
    );
}

pub fn end_explorer_resize(app: &mut App) {
    if !app.explorer_resizing {
        return;
    }
    app.explorer_resizing = false;
    if app.config.explorer_width < crate::app::config::EXPLORER_MIN_WIDTH {
        app.config.explorer_width = crate::app::config::EXPLORER_MIN_WIDTH;
    }
    if let Err(err) = app.config.save() {
        app.message = Some(format!("explorer width not saved: {err}"));
    }
}

pub fn begin_hscroll_drag(app: &mut App, x: u16, y: u16) -> bool {
    let Some(hit) = app.hscroll_hit else {
        return false;
    };
    if y != hit.y || x < hit.x || x >= hit.x.saturating_add(hit.width) {
        return false;
    }
    let on_thumb = x >= hit.thumb_x && x < hit.thumb_x.saturating_add(hit.thumb_w);
    let grab = if on_thumb {
        x.saturating_sub(hit.thumb_x)
    } else {
        hit.thumb_w / 2
    };
    let left = hscroll_left_at(bar_from_hit(hit), x, grab);
    app.hscroll_grab = grab;
    app.hscroll_dragging = true;
    assign_horizontal(app, left as i32);
    true
}

pub fn drag_hscroll(app: &mut App, x: u16) {
    let Some(hit) = app.hscroll_hit else {
        return;
    };
    let left = hscroll_left_at(bar_from_hit(hit), x, app.hscroll_grab);
    assign_horizontal(app, left as i32);
}

fn assign_horizontal(app: &mut App, left: i32) {
    let Some(id) = app.active else { return };
    let width = app.editor_area.2.saturating_sub(2 + app.gutter_width) as usize;
    let tab_width = app.config.tab_width;
    let content = app
        .documents
        .get(&id)
        .map(|doc| max_line_display_width(&doc.rope, tab_width))
        .unwrap_or(0);
    if let Some(view) = app.views.get_mut(&id) {
        view.viewport.left = clamp_horizontal_left(left, content, width);
    }
}

fn bar_from_hit(hit: HScrollHit) -> HScrollBar {
    HScrollBar {
        track_x: hit.x,
        track_w: hit.width,
        thumb_x: hit.thumb_x,
        thumb_w: hit.thumb_w,
        max_left: hit.max_left,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WheelAxis {
    Vertical(i32),
    Horizontal(i32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WheelAction {
    Explorer(i32),
    EditorVertical(i32),
    EditorHorizontal(i32),
    Thread(i32),
    None,
}

pub fn wheel_action(
    in_popup: bool,
    in_explorer: bool,
    in_editor: bool,
    in_thread: bool,
    shift: bool,
    axis: WheelAxis,
) -> WheelAction {
    if in_popup {
        return WheelAction::None;
    }
    match axis {
        WheelAxis::Horizontal(delta) => {
            if in_editor {
                WheelAction::EditorHorizontal(delta)
            } else {
                WheelAction::None
            }
        }
        WheelAxis::Vertical(delta) if shift => {
            if in_editor {
                WheelAction::EditorHorizontal(if delta > 0 { 4 } else { -4 })
            } else {
                WheelAction::None
            }
        }
        WheelAxis::Vertical(delta) if in_thread => WheelAction::Thread(delta),
        WheelAxis::Vertical(delta) if in_explorer => WheelAction::Explorer(delta),
        WheelAxis::Vertical(delta) if in_editor => WheelAction::EditorVertical(delta),
        WheelAxis::Vertical(_) => WheelAction::None,
    }
}

pub fn start_thread(app: &mut App) {
    if !claude_ready(app) {
        return;
    }
    if app.thread.running {
        app.message = Some("Claude is still working".into());
        return;
    }
    app.thread.anchor = capture_anchor(app);
    if app.thread.prior.is_empty() {
        app.thread.style = PromptStyle::Chat;
    }
    app.thread_visible = true;
    app.focus = Focus::Thread;
    app.overlay = Overlay::None;
}

pub fn prompt_agent_edit(app: &mut App) {
    if !claude_ready(app) {
        return;
    }
    if app.thread.running {
        app.message = Some("Claude is still working".into());
        return;
    }
    open_prompt(
        app,
        "Edit with Claude",
        String::new(),
        PromptKind::AgentEdit,
    );
}

pub fn send_thread(app: &mut App) {
    let message = app.thread.compose.trim().to_string();
    if message.is_empty() {
        return;
    }
    app.thread.compose.clear();
    send_message(app, message);
}

pub fn stop_agent(app: &mut App) {
    if let Some(run) = app.agent_run.take() {
        run.stop();
    }
}

pub fn scroll_thread(app: &mut App, delta: i32) {
    if delta > 0 {
        app.thread.scroll = app.thread.scroll.saturating_sub(delta as usize);
    } else {
        app.thread.scroll = app
            .thread
            .scroll
            .saturating_add(delta.unsigned_abs() as usize);
    }
}

fn send_message(app: &mut App, message: String) {
    if !claude_ready(app) {
        return;
    }
    if app.thread.running {
        app.message = Some("Claude is still working".into());
        return;
    }
    app.begin_turn();
    let replay = !app.thread.prior.is_empty() && !app.thread.live;
    let prompt = crate::agent::compose_prompt(
        app.thread.style,
        app.thread.anchor.as_ref(),
        &app.thread.prior,
        &message,
        replay,
    );
    let resume = app.thread.live;
    let session_id = app.thread.session_id.clone();
    app.thread.lines.push(TranscriptLine {
        kind: LineKind::User,
        text: message.clone(),
    });
    app.thread.pending_user = message;
    app.thread.reply_buf.clear();
    app.thread.failure = None;
    app.thread.saw_session = false;
    app.thread.running = true;
    app.thread.scroll = 0;
    app.thread_visible = true;
    app.focus = Focus::Thread;
    if let Some(run) = app.agent_run.take() {
        run.stop();
    }
    let run = AgentRun::new();
    app.agent_run = Some(run.clone());
    crate::agent::spawn_turn(
        app.root.clone(),
        prompt,
        session_id,
        resume,
        app.agent_tx.clone(),
        run,
    );
}

pub fn handle_agent(app: &mut App, event: AgentEvent) {
    app.thread.spinner = app.thread.spinner.wrapping_add(1);
    match event {
        AgentEvent::Session => app.thread.saw_session = true,
        AgentEvent::Text { text, block } => append_reply(app, &text, block),
        AgentEvent::Tool { label, path } => {
            app.thread.reply_buf.clear();
            app.thread.lines.push(TranscriptLine {
                kind: LineKind::Tool,
                text: label,
            });
            if let Some(path) = path {
                app.note_turn_path(&path);
            }
        }
        AgentEvent::Failure { text } => app.thread.failure = Some(text),
        AgentEvent::Result { text } => {
            if app.thread.reply_buf.trim().is_empty() {
                append_reply(app, &text, true);
            }
        }
        AgentEvent::Finished { success, stderr } => finish_turn(app, success, stderr),
    }
}

fn append_reply(app: &mut App, text: &str, block: bool) {
    if block && !app.thread.reply_buf.is_empty() && !app.thread.reply_buf.ends_with("\n\n") {
        app.thread.reply_buf.push_str("\n\n");
    }
    app.thread.reply_buf.push_str(text);
    let reply = app.thread.reply_buf.clone();
    if let Some(line) = app.thread.lines.last_mut()
        && line.kind == LineKind::Reply
    {
        line.text = reply;
    } else {
        app.thread.lines.push(TranscriptLine {
            kind: LineKind::Reply,
            text: reply,
        });
    }
}

fn finish_turn(app: &mut App, success: bool, stderr: String) {
    let failure = app.thread.failure.clone();
    let saw_session = app.thread.saw_session;
    app.thread.running = false;
    app.agent_run = None;
    if success || saw_session {
        app.thread.live = true;
    }
    let failed = !success || failure.is_some();
    if failed && app.thread.reply_buf.trim().is_empty() {
        let mut text = failure.unwrap_or_default();
        let stderr = stderr.trim();
        if !stderr.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(stderr);
        }
        if text.is_empty() {
            text = "Claude stopped".into();
        }
        app.thread.lines.push(TranscriptLine {
            kind: LineKind::Error,
            text,
        });
    }
    let user = std::mem::take(&mut app.thread.pending_user);
    let reply = app.thread.reply_buf.clone();
    if !user.is_empty() {
        app.thread.prior.push((user, reply));
    }
    app.thread.reply_buf.clear();
    settle_agent_edits(app);
    app.finish_turn_status();
}

pub fn settle_agent_edits(app: &mut App) {
    let ids: Vec<DocumentId> = app.documents.keys().copied().collect();
    let mut dirty_note = None;
    for id in ids {
        match refresh_from_disk(app, id) {
            DiskRefresh::Dirty(path) => {
                dirty_note = Some(format!("file changed on disk: {}", path.display()));
            }
            DiskRefresh::Reloaded | DiskRefresh::Same | DiskRefresh::Skipped => {}
        }
    }
    if let Some(note) = dirty_note {
        app.message = Some(note);
    }
}

enum DiskRefresh {
    Same,
    Dirty(PathBuf),
    Reloaded,
    Skipped,
}

fn refresh_from_disk(app: &mut App, id: DocumentId) -> DiskRefresh {
    let Some((path, current, dirty)) = app
        .documents
        .get(&id)
        .and_then(|doc| Some((doc.path.clone()?, doc.text(), doc.is_dirty())))
    else {
        return DiskRefresh::Skipped;
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return DiskRefresh::Skipped;
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return DiskRefresh::Skipped;
    };
    if text == current {
        if let Some(doc) = app.documents.get_mut(&id) {
            doc.mtime = std::fs::metadata(&path)
                .and_then(|meta| meta.modified())
                .ok();
        }
        return DiskRefresh::Same;
    }
    if dirty {
        return DiskRefresh::Dirty(path);
    }
    let reloaded = {
        let Some(doc) = app.documents.get_mut(&id) else {
            return DiskRefresh::Skipped;
        };
        if doc.replace_from_disk().is_err() {
            return DiskRefresh::Skipped;
        }
        (doc.language, doc.rope.clone(), doc.revision, doc.text())
    };
    let (language, rope, revision, full) = reloaded;
    app.syntax.close(id);
    app.syntax.open(id, language, &rope);
    let _ = app.lsp.tx.send(LspCmd::DidChange {
        path: path.clone(),
        version: revision as i64,
        change: full_document_change(id, revision, full.clone()),
        full_text: full,
        incremental: false,
    });
    if let Some(view) = app.views.get_mut(&id) {
        let len = rope.len_lines().max(1);
        if view.cursor.line >= len {
            view.cursor.line = len - 1;
            view.cursor.column = 0;
        }
        view.selection = None;
    }
    app.request_inline_diff(id);
    DiskRefresh::Reloaded
}

fn full_document_change(id: DocumentId, revision: u64, text: String) -> DocumentChange {
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

fn capture_anchor(app: &App) -> Option<Anchor> {
    let doc = app.active_doc()?;
    let view = app.active_view()?;
    let path = app.relative(doc.path.as_ref()?);
    let (start, end) =
        if let Some(selection) = view.selection.filter(|selection| !selection.is_empty()) {
            let range = selection.range();
            (range.start.line, range.end.line)
        } else {
            (view.cursor.line, view.cursor.line)
        };
    let mut snippet = Vec::new();
    for line in start..=end {
        if snippet.len() >= 40 {
            break;
        }
        snippet.push(line_text(&doc.rope, line));
    }
    Some(Anchor {
        path,
        start_line: start + 1,
        end_line: end.min(start + snippet.len().saturating_sub(1)) + 1,
        snippet: snippet.join("\n"),
    })
}

pub fn commit_git(app: &mut App) {
    app.commit_git();
}

pub fn move_git(app: &mut App, delta: i32) {
    let rows = app.git_rows();
    let files: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| {
            matches!(row, crate::workspace::GitPanelRow::File { .. }).then_some(index)
        })
        .collect();
    if files.is_empty() {
        return;
    }
    let pos = files
        .iter()
        .position(|index| *index == app.git_selected)
        .unwrap_or(0);
    let next = if delta < 0 {
        pos.saturating_sub(1)
    } else {
        (pos + 1).min(files.len() - 1)
    };
    app.git_selected = files[next];
    let inner = git_list_rows(app.explorer_area.3, app.git_body_open);
    if app.git_selected < app.git_scroll {
        app.git_scroll = app.git_selected;
    } else if inner > 0 && app.git_selected >= app.git_scroll + inner {
        app.git_scroll = app.git_selected + 1 - inner;
    }
}

pub fn open_selected_git(app: &mut App) {
    let rows = app.git_rows();
    let Some(crate::workspace::GitPanelRow::File { path, staged, .. }) = rows.get(app.git_selected)
    else {
        return;
    };
    let path = path.clone();
    let staged = *staged;
    if deleted_conflict(app, &path) {
        confirm_resolve_delete(app, path);
        return;
    }
    app.open_git_entry(path, staged);
}

pub fn toggle_selected_git(app: &mut App) {
    let rows = app.git_rows();
    let Some(crate::workspace::GitPanelRow::File { path, staged, .. }) = rows.get(app.git_selected)
    else {
        return;
    };
    let path = path.clone();
    let staged = *staged;
    if deleted_conflict(app, &path) {
        confirm_resolve_delete(app, path);
        return;
    }
    app.toggle_stage(path, staged);
}

fn deleted_conflict(app: &App, path: &std::path::Path) -> bool {
    app.git_status
        .get(path)
        .is_some_and(|status| status.conflict == Some(crate::workspace::ConflictKind::Deleted))
}

fn confirm_resolve_delete(app: &mut App, path: PathBuf) {
    let name = app.relative(&path);
    app.overlay = Overlay::Confirm {
        message: format!("Delete {name}?"),
        action: ConfirmAction::ResolveDelete(path),
    };
}

pub fn git_file_index_at(app: &App, y: u16) -> Option<usize> {
    let rows = app.git_rows();
    let inner = git_list_rows(app.explorer_area.3, app.git_body_open);
    if inner == 0 || y <= app.explorer_area.1 {
        return None;
    }
    let rel = y.saturating_sub(app.explorer_area.1).saturating_sub(1) as usize;
    if rel >= inner {
        return None;
    }
    let index = app.git_scroll + rel;
    matches!(
        rows.get(index),
        Some(crate::workspace::GitPanelRow::File { .. })
    )
    .then_some(index)
}

pub fn click_git(app: &mut App, x: u16, y: u16) {
    app.focus = Focus::Explorer;
    if let Some(action) = app
        .git_hits
        .iter()
        .find(|hit| y == hit.y && x >= hit.x && x < hit.x.saturating_add(hit.width))
        .map(|hit| hit.action.clone())
    {
        match action {
            crate::app::state::GitClick::Message => {
                app.git_body_focus = false;
                app.git_message_focus = true;
            }
            crate::app::state::GitClick::Body => {
                app.git_message_focus = false;
                app.git_body_focus = true;
                app.git_body_open = true;
            }
            crate::app::state::GitClick::BodyFold => {
                app.git_body_open = !app.git_body_open;
                if !app.git_body_open {
                    app.git_body_focus = false;
                }
            }
            crate::app::state::GitClick::Pull => app.pull_git(),
            crate::app::state::GitClick::Push => app.push_git(),
            crate::app::state::GitClick::Commit => app.commit_git(),
            crate::app::state::GitClick::StageAll => app.stage_all(),
            crate::app::state::GitClick::UnstageAll => app.unstage_all(),
            crate::app::state::GitClick::Toggle { path, staged } => app.toggle_stage(path, staged),
            crate::app::state::GitClick::DeleteConflict { path } => {
                confirm_resolve_delete(app, path);
            }
            crate::app::state::GitClick::Open { path, staged } => {
                app.git_selected = app
                    .git_rows()
                    .iter()
                    .position(|row| {
                        matches!(
                            row,
                            crate::workspace::GitPanelRow::File {
                                path: row_path,
                                staged: row_staged,
                                ..
                            } if row_path == &path && *row_staged == staged
                        )
                    })
                    .unwrap_or(app.git_selected);
                app.open_git_entry(path, staged);
            }
        }
        return;
    }
    app.git_message_focus = false;
    app.git_body_focus = false;
    let top = app.explorer_area.1.saturating_add(1);
    let list_h = git_list_rows(app.explorer_area.3, app.git_body_open);
    let bottom = top.saturating_add(list_h as u16);
    if y >= top && y < bottom {
        let index = app.git_scroll + usize::from(y - top);
        let len = app.git_rows().len();
        if index < len {
            app.git_selected = index;
        }
    }
}

pub fn receive_git_write(app: &mut App, result: crate::workspace::GitWriteResult) {
    let reload = result.reload.clone();
    let close_path = result.close_path.clone();
    let ok = result.ok;
    let branch = result.branch;
    let message = result.message.clone();
    app.note_git_write(result);
    if branch {
        if ok {
            if matches!(app.overlay, Overlay::Branches { .. }) {
                app.overlay = Overlay::None;
            }
        } else if let Overlay::Branches { error, .. } = &mut app.overlay {
            *error = Some(if message.is_empty() {
                "Branch change failed".into()
            } else {
                message
            });
        }
    }
    if ok {
        if let Some(path) = reload {
            app.reload_clean_document(&path);
        }
        if let Some(path) = close_path {
            let ids: Vec<_> = app
                .documents
                .iter()
                .filter(|(_, doc)| doc.path.as_deref() == Some(path.as_path()))
                .map(|(id, _)| *id)
                .collect();
            for id in ids {
                app.close_document(id, true);
            }
        }
        let open = app.order.clone();
        for id in open {
            app.request_inline_diff(id);
        }
    }
    app.refresh_git_status();
    app.refresh_open_reviews();
}

pub fn click_explorer(app: &mut App, y: u16) {
    let (_x, ey, _w, eh) = app.explorer_area;
    let Some(idx) = explorer_index_at(app.explorer_scroll, ey, eh, y, app.explorer_entries.len())
    else {
        return;
    };
    app.explorer_selected = idx;
    app.focus = Focus::Explorer;
    explorer_activate(app);
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tokio::sync::mpsc;

    use ropey::Rope;

    use crate::app::state::{App, ConfirmAction, Overlay, SearchKind, SearchScope};
    use crate::editor::{DocumentId, Position, Selection};
    use crate::language::LanguageClient;

    use super::{
        MenuAction, WheelAction, WheelAxis, confirm_yes, editor_context_labels, editor_search_seed,
        open_search, open_search_file, run_menu, search_seed, set_search_scope, wheel_action,
    };

    fn with_runtime<T>(body: impl FnOnce() -> T) -> T {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        body()
    }

    fn app_with_tabs(count: usize) -> (App, Vec<DocumentId>) {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut app = App::new(PathBuf::from("."), LanguageClient { tx });
        for _ in 0..count {
            app.open_untitled();
        }
        let ids = app.order.clone();
        (app, ids)
    }

    #[test]
    fn claude_stays_closed_until_it_is_enabled() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut app = App::new(PathBuf::from("."), LanguageClient { tx });
        app.config.claude_enabled = false;
        super::start_thread(&mut app);
        assert!(!app.thread_visible);
        assert_eq!(
            app.message.as_deref(),
            Some("Claude is off. Turn it on in Settings")
        );
    }

    #[test]
    fn editor_menu_keeps_navigation_actions_only() {
        assert_eq!(
            editor_context_labels(),
            ["Go to Definition", "Find References", "Ask Claude"]
        );
    }

    #[test]
    fn search_seed_uses_the_first_nonblank_line() {
        assert_eq!(search_seed("  alpha  "), "alpha");
        assert_eq!(search_seed("\n  keep me  \nsecond"), "keep me");
        assert_eq!(search_seed("   \n\t"), "");
        assert_eq!(search_seed(""), "");
    }

    #[test]
    fn search_uses_the_selected_occurrence() {
        let (mut app, ids) = app_with_tabs(1);
        let id = ids[0];
        app.documents.get_mut(&id).unwrap().rope = Rope::from("widget here\nnope\n  widget  \n");
        assert_eq!(editor_search_seed(&app), "");
        app.views.get_mut(&id).unwrap().selection = Some(Selection {
            anchor: Position::new(2, 2),
            cursor: Position::new(2, 8),
        });
        assert_eq!(editor_search_seed(&app), "widget");
        open_search_file(&mut app);
        match &app.overlay {
            Overlay::Search {
                kind,
                scope,
                query,
                file_matches,
                selected,
                ..
            } => {
                assert_eq!(*kind, SearchKind::Content);
                assert_eq!(*scope, SearchScope::File);
                assert_eq!(query, "widget");
                assert_eq!(file_matches.len(), 2);
                assert_eq!(*selected, 1);
            }
            other => panic!("expected file search, got {other:?}"),
        }
        let view = app.views.get(&id).unwrap();
        assert_eq!(
            view.selection,
            Some(Selection {
                anchor: Position::new(2, 2),
                cursor: Position::new(2, 8),
            })
        );
    }

    #[test]
    fn open_search_file_reseeds_an_open_search_from_the_selection() {
        let (mut app, ids) = app_with_tabs(1);
        let id = ids[0];
        app.documents.get_mut(&id).unwrap().rope =
            Rope::from("alpha here\nbeta\n  alpha  \nbeta again\n");
        app.views.get_mut(&id).unwrap().selection = Some(Selection {
            anchor: Position::new(2, 2),
            cursor: Position::new(2, 7),
        });
        open_search_file(&mut app);

        app.views.get_mut(&id).unwrap().selection = Some(Selection {
            anchor: Position::new(3, 0),
            cursor: Position::new(3, 4),
        });
        open_search_file(&mut app);
        match &app.overlay {
            Overlay::Search {
                kind,
                scope,
                query,
                file_matches,
                selected,
                ..
            } => {
                assert_eq!(*kind, SearchKind::Content);
                assert_eq!(*scope, SearchScope::File);
                assert_eq!(query, "beta");
                assert_eq!(file_matches.len(), 2);
                assert_eq!(*selected, 1);
            }
            other => panic!("expected file search, got {other:?}"),
        }

        app.views.get_mut(&id).unwrap().selection = None;
        open_search_file(&mut app);
        assert!(matches!(app.overlay, Overlay::None));
    }

    #[test]
    fn global_search_keeps_the_file_query() {
        with_runtime(|| {
            let (mut app, ids) = app_with_tabs(1);
            let id = ids[0];
            app.documents.get_mut(&id).unwrap().rope = Rope::from("widget here\n");
            app.views.get_mut(&id).unwrap().selection = Some(Selection {
                anchor: Position::new(0, 0),
                cursor: Position::new(0, 6),
            });
            open_search_file(&mut app);
            set_search_scope(&mut app, SearchScope::Global);
            match &app.overlay {
                Overlay::Search {
                    scope,
                    query,
                    searching,
                    file_matches,
                    ..
                } => {
                    assert_eq!(*scope, SearchScope::Global);
                    assert_eq!(query, "widget");
                    assert!(*searching);
                    assert!(file_matches.is_empty());
                }
                other => panic!("expected global search, got {other:?}"),
            }
        });
    }

    #[test]
    fn file_scope_needs_an_open_buffer() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut app = App::new(PathBuf::from("."), LanguageClient { tx });
        open_search_file(&mut app);
        match &app.overlay {
            Overlay::Search {
                kind, scope, query, ..
            } => {
                assert_eq!(*kind, SearchKind::Content);
                assert_eq!(*scope, SearchScope::Global);
                assert!(query.is_empty());
            }
            other => panic!("expected global search, got {other:?}"),
        }
        set_search_scope(&mut app, SearchScope::File);
        assert!(matches!(
            app.overlay,
            Overlay::Search {
                scope: SearchScope::Global,
                ..
            }
        ));
    }

    #[test]
    fn find_starts_from_the_selection() {
        with_runtime(|| {
            let (mut app, ids) = app_with_tabs(1);
            let id = ids[0];
            app.documents.get_mut(&id).unwrap().rope = Rope::from("commands.rs\n");
            app.views.get_mut(&id).unwrap().selection = Some(Selection {
                anchor: Position::new(0, 0),
                cursor: Position::new(0, 11),
            });
            open_search(&mut app);
            match &app.overlay {
                Overlay::Search {
                    kind,
                    query,
                    searching,
                    ..
                } => {
                    assert_eq!(*kind, SearchKind::Find);
                    assert_eq!(query, "commands.rs");
                    assert!(*searching);
                }
                other => panic!("expected find, got {other:?}"),
            }
        });
    }

    #[test]
    fn wheel_follows_the_pane_under_the_pointer() {
        assert_eq!(
            wheel_action(false, true, false, false, false, WheelAxis::Vertical(3)),
            WheelAction::Explorer(3)
        );
        assert_eq!(
            wheel_action(false, false, true, false, false, WheelAxis::Vertical(-3)),
            WheelAction::EditorVertical(-3)
        );
        assert_eq!(
            wheel_action(false, false, false, true, false, WheelAxis::Vertical(3)),
            WheelAction::Thread(3)
        );
        assert_eq!(
            wheel_action(false, false, false, false, false, WheelAxis::Vertical(3)),
            WheelAction::None
        );
        assert_eq!(
            wheel_action(true, true, true, true, false, WheelAxis::Vertical(3)),
            WheelAction::None
        );
        assert_eq!(
            wheel_action(false, true, false, false, true, WheelAxis::Vertical(3)),
            WheelAction::None
        );
        assert_eq!(
            wheel_action(false, false, true, false, true, WheelAxis::Vertical(3)),
            WheelAction::EditorHorizontal(4)
        );
        assert_eq!(
            wheel_action(false, false, true, false, false, WheelAxis::Horizontal(-4)),
            WheelAction::EditorHorizontal(-4)
        );
        assert_eq!(
            wheel_action(false, true, false, false, false, WheelAxis::Horizontal(4)),
            WheelAction::None
        );
    }

    #[test]
    fn close_others_keeps_the_clicked_tab_even_when_it_is_not_active() {
        let (mut app, ids) = app_with_tabs(3);
        app.active = Some(ids[0]);
        run_menu(&mut app, MenuAction::CloseOthers(ids[1]));
        assert_eq!(app.order, vec![ids[1]]);
        assert_eq!(app.active, Some(ids[1]));
    }

    #[test]
    fn close_all_clears_every_tab() {
        let (mut app, _) = app_with_tabs(3);
        run_menu(&mut app, MenuAction::CloseAll);
        assert!(app.order.is_empty());
        assert_eq!(app.active, None);
    }

    #[test]
    fn close_others_confirms_before_dropping_a_dirty_tab() {
        let (mut app, ids) = app_with_tabs(3);
        app.documents.get_mut(&ids[2]).unwrap().revision = 1;
        run_menu(&mut app, MenuAction::CloseOthers(ids[1]));
        match &app.overlay {
            Overlay::Confirm {
                action: ConfirmAction::CloseMany(targets),
                ..
            } => {
                assert_eq!(targets, &vec![ids[0], ids[2]]);
            }
            other => panic!("expected confirm, got {other:?}"),
        }
        assert_eq!(app.order, ids);
        confirm_yes(&mut app);
        assert_eq!(app.order, vec![ids[1]]);
        assert_eq!(app.active, Some(ids[1]));
    }

    #[test]
    fn close_others_skips_the_prompt_when_only_the_kept_tab_is_dirty() {
        let (mut app, ids) = app_with_tabs(2);
        app.documents.get_mut(&ids[0]).unwrap().revision = 1;
        run_menu(&mut app, MenuAction::CloseOthers(ids[0]));
        assert!(matches!(app.overlay, Overlay::None));
        assert_eq!(app.order, vec![ids[0]]);
        assert_eq!(app.active, Some(ids[0]));
    }

    #[test]
    fn a_running_turn_reloads_a_clean_buffer_quietly() {
        let dir = std::env::temp_dir().join(format!("p0-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("file.ts");
        std::fs::write(&path, "const a = 1;\n").unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut app = with_runtime(|| {
            let mut app = App::new(dir, LanguageClient { tx });
            app.open_path(path.clone());
            app
        });
        app.documents.values_mut().next().unwrap().mtime = Some(std::time::SystemTime::UNIX_EPOCH);
        std::fs::write(&path, "const a = 2;\n").unwrap();
        app.thread.running = true;
        super::handle_watch(
            &mut app,
            crate::workspace::WatchEvent {
                path: path.clone(),
                kind: crate::workspace::WatchKind::Modify,
            },
        );
        assert!(matches!(app.overlay, Overlay::None));
        assert_eq!(
            app.documents.values().next().unwrap().text(),
            "const a = 2;\n"
        );
        app.thread.running = false;
        app.documents.values_mut().next().unwrap().mtime = Some(std::time::SystemTime::UNIX_EPOCH);
        std::fs::write(&path, "const a = 3;\n").unwrap();
        super::handle_watch(
            &mut app,
            crate::workspace::WatchEvent {
                path: path.clone(),
                kind: crate::workspace::WatchKind::Modify,
            },
        );
        assert!(matches!(
            app.overlay,
            Overlay::Confirm {
                action: ConfirmAction::Reload(_),
                ..
            }
        ));
        let _ = std::fs::remove_dir_all(app.root.clone());
    }

    #[test]
    fn settle_reloads_clean_buffers_and_keeps_dirty_ones() {
        let dir = std::env::temp_dir().join(format!("p0-settle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let clean = dir.join("clean.ts");
        let dirty = dir.join("dirty.ts");
        std::fs::write(&clean, "export const clean = 1;\n").unwrap();
        std::fs::write(&dirty, "export const dirty = 1;\n").unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut app = with_runtime(|| {
            let mut app = App::new(dir, LanguageClient { tx });
            app.open_path(clean.clone());
            app.open_path(dirty.clone());
            app
        });
        let dirty_id = app
            .documents
            .iter()
            .find(|(_, doc)| doc.path.as_ref() == Some(&dirty))
            .map(|(id, _)| *id)
            .unwrap();
        app.documents.get_mut(&dirty_id).unwrap().revision = 1;
        std::fs::write(&clean, "export const clean = 2;\n").unwrap();
        std::fs::write(&dirty, "export const dirty = 2;\n").unwrap();
        with_runtime(|| super::settle_agent_edits(&mut app));
        let clean_text = app
            .documents
            .values()
            .find(|doc| doc.path.as_ref() == Some(&clean))
            .unwrap()
            .text();
        let dirty_text = app.documents.get(&dirty_id).unwrap().text();
        assert_eq!(clean_text, "export const clean = 2;\n");
        assert_eq!(dirty_text, "export const dirty = 1;\n");
        assert!(
            app.message
                .as_deref()
                .unwrap_or("")
                .contains("changed on disk")
        );
        let _ = std::fs::remove_dir_all(app.root.clone());
    }

    #[test]
    fn failed_import_request_still_renames_the_file() {
        let dir = std::env::temp_dir().join(format!("r3vu-rename-fail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let from = dir.join("a.ts");
        let to = dir.join("b.ts");
        std::fs::write(&from, "export const a = 1;\n").unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut app = with_runtime(|| {
            let mut app = App::new(dir, LanguageClient { tx });
            app.open_path(from.clone());
            app
        });
        app.pending_rename = Some(crate::app::state::PendingRename {
            token: 7,
            from: from.clone(),
            to: to.clone(),
        });
        with_runtime(|| super::complete_file_rename(&mut app, 7, Vec::new(), true));
        assert!(to.is_file(), "rename still moves the file");
        assert!(!from.exists());
        assert!(
            app.message
                .as_deref()
                .unwrap_or("")
                .contains("Imports were not updated")
        );
        assert!(
            app.documents
                .values()
                .any(|doc| doc.path.as_ref() == Some(&to))
        );
        let _ = std::fs::remove_dir_all(&app.root);
    }

    #[test]
    fn rename_applies_an_import_edit_before_the_move() {
        let dir = std::env::temp_dir().join(format!("r3vu-rename-edit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let from = dir.join("a.ts");
        let to = dir.join("b.ts");
        let importer = dir.join("use.ts");
        let source = "import { a } from \"./a\";\n";
        std::fs::write(&from, "export const a = 1;\n").unwrap();
        std::fs::write(&importer, source).unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut app = with_runtime(|| {
            let mut app = App::new(dir, LanguageClient { tx });
            app.open_path(from.clone());
            app.open_path(importer.clone());
            app
        });
        let start = source.find("\"./a\"").unwrap();
        app.pending_rename = Some(crate::app::state::PendingRename {
            token: 9,
            from: from.clone(),
            to: to.clone(),
        });
        with_runtime(|| {
            super::complete_file_rename(
                &mut app,
                9,
                vec![crate::language::TextEdit {
                    path: importer.clone(),
                    start: crate::editor::Position::new(0, start),
                    end: crate::editor::Position::new(0, start + 5),
                    new_text: "\"./b\"".into(),
                }],
                false,
            );
        });
        assert_eq!(
            std::fs::read_to_string(&importer).unwrap(),
            "import { a } from \"./b\";\n"
        );
        assert!(to.is_file());
        assert!(!from.exists());
        assert_eq!(app.message.as_deref(), Some("Renamed"));
        let _ = std::fs::remove_dir_all(&app.root);
    }

    #[test]
    fn prompt_window_keeps_the_cursor_on_screen() {
        let (visible, col, skip) = super::prompt_window("abcdefghij", 10, 4);
        assert_eq!((visible.as_str(), col, skip), ("hij", 3, 7));
        assert_eq!(super::index_at_width("reader.ts", 0), 0);
        assert_eq!(super::index_at_width("reader.ts", 6), 6);
        assert_eq!(super::index_at_width("あa", 0), 0);
        assert_eq!(super::index_at_width("あa", 1), 0);
        assert_eq!(super::index_at_width("あa", 2), 1);
    }

    #[test]
    fn rename_prompt_keeps_a_cursor_inside_the_name() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut app = App::new(PathBuf::from("."), LanguageClient { tx });
        app.explorer_entries = vec![crate::workspace::ExplorerEntry {
            path: PathBuf::from("reader.ts"),
            name: "reader.ts".into(),
            is_dir: false,
            depth: 0,
            expanded: false,
        }];
        super::run_menu(&mut app, MenuAction::RenameFile);
        let Overlay::Prompt { input, cursor, .. } = &app.overlay else {
            panic!("rename opens a prompt");
        };
        assert_eq!(input, "reader.ts");
        assert_eq!(*cursor, 9);
        super::move_prompt_cursor(&mut app, crossterm::event::KeyCode::Left);
        super::move_prompt_cursor(&mut app, crossterm::event::KeyCode::Left);
        super::move_prompt_cursor(&mut app, crossterm::event::KeyCode::Left);
        assert!(super::insert_prompt(&mut app, "2"));
        app.overlay_area = Some((10, 5, 20, 5));
        app.prompt_caret = Some((11, 6, 0));
        assert!(super::click_prompt(&mut app, 14, 6));
        let Overlay::Prompt { input, cursor, .. } = &app.overlay else {
            panic!("prompt stays open");
        };
        assert_eq!(input, "reader2.ts");
        assert_eq!(*cursor, 3);
        assert!(!super::click_prompt(&mut app, 0, 0));
    }

    fn branch_overlay(query: &str, branches: &[&str], selected: usize) -> Overlay {
        Overlay::Branches {
            query: query.into(),
            branches: branches.iter().map(|name| (*name).to_string()).collect(),
            selected,
            scroll: 0,
            generation: 1,
            loading: false,
            error: Some("old".into()),
        }
    }

    #[test]
    fn invalid_branch_name_stays_in_the_modal() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut app = App::new(PathBuf::from("."), LanguageClient { tx });
        app.overlay = branch_overlay("bad name", &[], 0);
        super::overlay_submit(&mut app);
        let Overlay::Branches {
            error: Some(message),
            ..
        } = &app.overlay
        else {
            panic!("modal stays open");
        };
        assert_eq!(message, "Branch name is invalid");
        assert!(!app.git_busy());
    }

    #[test]
    fn switching_to_the_current_branch_closes() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut app = App::new(PathBuf::from("."), LanguageClient { tx });
        app.git_branch = Some("main".into());
        app.overlay = branch_overlay("", &["main", "topic"], 0);
        super::overlay_submit(&mut app);
        assert!(matches!(app.overlay, Overlay::None));
        assert!(!app.git_busy());
    }

    #[test]
    fn typing_clears_a_branch_error() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut app = App::new(PathBuf::from("."), LanguageClient { tx });
        app.overlay = branch_overlay("bad", &[], 0);
        super::overlay_char(&mut app, 'x');
        let Overlay::Branches { query, error, .. } = &app.overlay else {
            panic!("modal stays open");
        };
        assert_eq!(query, "badx");
        assert!(error.is_none());
    }

    #[test]
    fn branch_failure_stays_open_and_success_closes() {
        with_runtime(|| {
            let (tx, _rx) = mpsc::unbounded_channel();
            let mut app = App::new(PathBuf::from("."), LanguageClient { tx });
            app.overlay = branch_overlay("topic", &[], 0);
            super::receive_git_write(
                &mut app,
                crate::workspace::GitWriteResult {
                    ok: false,
                    message: "a branch named 'topic' already exists".into(),
                    reload: None,
                    close_path: None,
                    committed: None,
                    committed_body: None,
                    notice: None,
                    branch: true,
                },
            );
            let Overlay::Branches {
                error: Some(message),
                ..
            } = &app.overlay
            else {
                panic!("failure keeps the modal");
            };
            assert_eq!(message, "a branch named 'topic' already exists");
            assert_eq!(
                app.message.as_deref(),
                Some("a branch named 'topic' already exists")
            );
            app.overlay = Overlay::None;
            super::receive_git_write(
                &mut app,
                crate::workspace::GitWriteResult {
                    ok: false,
                    message: "a branch named 'topic' already exists".into(),
                    reload: None,
                    close_path: None,
                    committed: None,
                    committed_body: None,
                    notice: None,
                    branch: true,
                },
            );
            assert!(matches!(app.overlay, Overlay::None));
            super::receive_git_write(
                &mut app,
                crate::workspace::GitWriteResult {
                    ok: true,
                    message: String::new(),
                    reload: None,
                    close_path: None,
                    committed: None,
                    committed_body: None,
                    notice: Some("Created topic".into()),
                    branch: true,
                },
            );
            assert!(matches!(app.overlay, Overlay::None));
            assert_eq!(app.message.as_deref(), Some("Created topic"));
            app.overlay = branch_overlay("", &["main", "topic"], 1);
            super::receive_git_write(
                &mut app,
                crate::workspace::GitWriteResult {
                    ok: true,
                    message: String::new(),
                    reload: None,
                    close_path: None,
                    committed: None,
                    committed_body: None,
                    notice: Some("Switched to topic".into()),
                    branch: true,
                },
            );
            assert!(matches!(app.overlay, Overlay::None));
            assert_eq!(app.message.as_deref(), Some("Switched to topic"));
        });
    }
}
