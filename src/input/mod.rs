use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

use crate::app::App;
use crate::app::commands::{self, WheelAction, WheelAxis};
use crate::app::events;
use crate::app::state::{Focus, HeaderAction, Overlay, SearchKind};
use crate::editor::Movement;

pub fn handle_event(app: &mut App, event: Event) {
    match event {
        Event::Key(key) if key.kind != KeyEventKind::Release => handle_key(app, key),
        Event::Mouse(mouse) => handle_mouse(app, mouse),
        Event::Paste(text) => {
            if commands::overlay_insert(app, &text) {
                return;
            }
            if app.git_message_focus && matches!(app.overlay, Overlay::None) {
                app.git_message.push_str(&text.replace(['\n', '\r'], " "));
            } else if app.git_body_focus && matches!(app.overlay, Overlay::None) {
                let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
                if normalized.contains('\n') {
                    app.git_body_open = true;
                }
                app.git_body.push_str(&normalized);
            } else if app.focus == Focus::Thread && matches!(app.overlay, Overlay::None) {
                app.thread
                    .compose
                    .push_str(&text.replace(['\n', '\r'], " "));
            } else if events::editor_has_focus(app) {
                commands::type_text(app, &text);
            }
        }
        Event::Resize(_, _) => {}
        _ => {}
    }
}

fn handle_key(app: &mut App, key: KeyEvent) {
    if app.hover_dwell.is_some() || matches!(app.overlay, Overlay::Hover { .. }) {
        commands::cancel_hover(app);
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    if matches!(key.code, KeyCode::Esc) {
        if !matches!(app.overlay, Overlay::None) {
            commands::dismiss_overlay(app);
            return;
        }
        if app.git_message_focus || app.git_body_focus {
            app.git_message_focus = false;
            app.git_body_focus = false;
            return;
        }
        if app.markdown_preview.is_some() {
            app.markdown_preview = None;
            return;
        }
    }

    if ctrl {
        match key.code {
            KeyCode::Tab => {
                app.cycle_tab(shift);
                return;
            }
            KeyCode::Char('s') | KeyCode::Char('S') => return commands::save_active(app),
            KeyCode::Char('p') | KeyCode::Char('P') => return commands::open_search(app),
            KeyCode::Char('g') | KeyCode::Char('G') if shift => {
                app.toggle_git();
                return;
            }
            KeyCode::Char('m') | KeyCode::Char('M') if shift => {
                app.toggle_markdown_preview();
                return;
            }
            KeyCode::Char('f') | KeyCode::Char('F') => return commands::open_search_file(app),
            KeyCode::Char('z') | KeyCode::Char('Z') => return commands::undo(app),
            KeyCode::Char('y') | KeyCode::Char('Y') => return commands::redo(app),
            KeyCode::Char('b') | KeyCode::Char('B') => {
                app.explorer_visible = !app.explorer_visible;
                return;
            }
            KeyCode::Char('q') | KeyCode::Char('Q') => return commands::maybe_quit(app),
            KeyCode::Char('w') | KeyCode::Char('W') => return app.close_active(),
            KeyCode::Char('c') | KeyCode::Char('C') => return commands::copy(app),
            KeyCode::Char('x') | KeyCode::Char('X') => return commands::cut(app),
            KeyCode::Char('v') | KeyCode::Char('V') => {
                if matches!(
                    app.overlay,
                    Overlay::Search { .. } | Overlay::Prompt { .. } | Overlay::Branches { .. }
                ) {
                    let text = app.clipboard.paste();
                    commands::overlay_insert(app, &text);
                } else {
                    commands::paste(app);
                }
                return;
            }
            KeyCode::Char('n') | KeyCode::Char('N') => return app.open_untitled(),
            KeyCode::Char('/') | KeyCode::Char('_') | KeyCode::Char('?') => {
                return commands::open_help(app);
            }
            KeyCode::Enter => return commands::go_to_definition(app),
            KeyCode::Char('r') | KeyCode::Char('R') if shift => return,
            KeyCode::Char('r') | KeyCode::Char('R') => return commands::start_rename_symbol(app),
            _ => {}
        }
    }

    let modal = !matches!(
        app.overlay,
        Overlay::None
            | Overlay::Completion { .. }
            | Overlay::Hover { .. }
            | Overlay::Signature { .. }
    );
    if alt && !modal {
        match key.code {
            KeyCode::Char('t') | KeyCode::Char('T') => return commands::start_thread(app),
            KeyCode::Char('e') | KeyCode::Char('E') => return commands::prompt_agent_edit(app),
            _ => {}
        }
    }

    if modal {
        match key.code {
            KeyCode::Enter => return commands::overlay_submit(app),
            KeyCode::Up | KeyCode::Down if matches!(app.overlay, Overlay::TypesNote) => return,
            KeyCode::Backspace => {
                commands::backspace(app);
                return;
            }
            KeyCode::Delete if matches!(app.overlay, Overlay::Prompt { .. }) => {
                commands::delete_prompt_forward(app);
                return;
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End
                if matches!(app.overlay, Overlay::Prompt { .. }) =>
            {
                commands::move_prompt_cursor(app, key.code);
                return;
            }
            KeyCode::Tab
                if matches!(
                    app.overlay,
                    Overlay::Search {
                        kind: SearchKind::Content,
                        ..
                    }
                ) =>
            {
                commands::toggle_search_scope(app);
                return;
            }
            KeyCode::Up => return commands::move_sel(app, Movement::Up, false),
            KeyCode::Down => return commands::move_sel(app, Movement::Down, false),
            KeyCode::Char(ch) => {
                commands::overlay_char(app, ch);
                return;
            }
            _ => return,
        }
    }

    if matches!(app.overlay, Overlay::Completion { .. }) {
        match key.code {
            KeyCode::Enter | KeyCode::Tab => return commands::accept_completion(app),
            KeyCode::Esc => {
                commands::dismiss_overlay(app);
                return;
            }
            _ => {}
        }
    }

    if app.focus == Focus::Thread && matches!(app.overlay, Overlay::None) {
        match key.code {
            KeyCode::Esc => {
                if app.thread.compose.is_empty() {
                    app.focus = Focus::Editor;
                } else {
                    app.thread.compose.clear();
                }
            }
            KeyCode::Enter => commands::send_thread(app),
            KeyCode::Backspace => {
                app.thread.compose.pop();
            }
            KeyCode::Up => commands::scroll_thread(app, -1),
            KeyCode::Down => commands::scroll_thread(app, 1),
            KeyCode::Char(ch) => app.thread.compose.push(ch),
            _ => {}
        }
        return;
    }

    if app.git_open && app.focus == Focus::Explorer && matches!(app.overlay, Overlay::None) {
        if app.git_message_focus {
            match key.code {
                KeyCode::Enter => commands::commit_git(app),
                KeyCode::Backspace => {
                    app.git_message.pop();
                }
                KeyCode::Char(ch) => app.git_message.push(ch),
                KeyCode::Tab => {
                    app.git_message_focus = false;
                    app.focus = Focus::Editor;
                }
                _ => {}
            }
            return;
        }
        if app.git_body_focus {
            match key.code {
                KeyCode::Enter => {
                    app.git_body.push('\n');
                    app.git_body_open = true;
                }
                KeyCode::Backspace => {
                    app.git_body.pop();
                }
                KeyCode::Char(ch) => app.git_body.push(ch),
                KeyCode::Tab => {
                    app.git_body_focus = false;
                    app.focus = Focus::Editor;
                }
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Up => {
                commands::move_git(app, -1);
                return;
            }
            KeyCode::Down => {
                commands::move_git(app, 1);
                return;
            }
            KeyCode::Enter => {
                commands::open_selected_git(app);
                return;
            }
            KeyCode::Char(' ') => {
                commands::toggle_selected_git(app);
                return;
            }
            KeyCode::Tab => {
                app.focus = Focus::Editor;
                return;
            }
            _ => return,
        }
    }

    if app.focus == Focus::Explorer && matches!(app.overlay, Overlay::None) {
        match key.code {
            KeyCode::Up => {
                app.explorer_selected = app.explorer_selected.saturating_sub(1);
                commands::reveal_explorer_selection(app);
                return;
            }
            KeyCode::Down => {
                app.explorer_selected =
                    (app.explorer_selected + 1).min(app.explorer_entries.len().saturating_sub(1));
                commands::reveal_explorer_selection(app);
                return;
            }
            KeyCode::Enter => {
                commands::explorer_activate(app);
                return;
            }
            KeyCode::Tab => {
                app.focus = Focus::Editor;
                return;
            }
            _ => {}
        }
    }

    match key.code {
        KeyCode::Backspace => commands::backspace(app),
        KeyCode::Delete => commands::delete_key(app),
        KeyCode::Enter => commands::type_text(app, "\n"),
        KeyCode::Tab => commands::type_text(app, "    "),
        KeyCode::Left if ctrl => commands::move_sel(app, Movement::WordLeft, shift),
        KeyCode::Right if ctrl => commands::move_sel(app, Movement::WordRight, shift),
        KeyCode::Left => commands::move_sel(app, Movement::Left, shift),
        KeyCode::Right => commands::move_sel(app, Movement::Right, shift),
        KeyCode::Up => commands::move_sel(app, Movement::Up, shift),
        KeyCode::Down => commands::move_sel(app, Movement::Down, shift),
        KeyCode::Home if ctrl => commands::move_sel(app, Movement::DocumentStart, shift),
        KeyCode::End if ctrl => commands::move_sel(app, Movement::DocumentEnd, shift),
        KeyCode::Home => commands::move_sel(app, Movement::LineStart, shift),
        KeyCode::End => commands::move_sel(app, Movement::LineEnd, shift),
        KeyCode::PageUp => commands::move_sel(app, Movement::PageUp, shift),
        KeyCode::PageDown => commands::move_sel(app, Movement::PageDown, shift),
        KeyCode::Char(ch) => commands::type_text(app, &ch.to_string()),
        _ => {}
    }
}

fn scroll_tabs(app: &mut App, x: u16) -> bool {
    if let Some((hit_x, width)) = app.tab_scroll_left
        && x >= hit_x
        && x < hit_x.saturating_add(width)
    {
        app.tab_scroll = app.tab_scroll.saturating_sub(1);
        return true;
    }
    if let Some((hit_x, width)) = app.tab_scroll_right
        && x >= hit_x
        && x < hit_x.saturating_add(width)
    {
        app.tab_scroll = app.tab_scroll.saturating_add(1);
        return true;
    }
    false
}

fn handle_mouse(app: &mut App, mouse: MouseEvent) {
    let x = mouse.column;
    let y = mouse.row;
    let in_search = app.explorer_visible
        && x >= app.search_area.0
        && x < app.search_area.0 + app.search_area.2
        && y >= app.search_area.1
        && y < app.search_area.1 + app.search_area.3;
    let in_explorer = app.explorer_visible
        && x >= app.explorer_area.0
        && x < app.explorer_area.0 + app.explorer_area.2
        && y >= app.explorer_area.1
        && y < app.explorer_area.1 + app.explorer_area.3;
    let in_editor = x >= app.editor_area.0
        && x < app.editor_area.0 + app.editor_area.2
        && y >= app.editor_area.1
        && y < app.editor_area.1 + app.editor_area.3;
    let in_thread = app.thread_visible
        && x >= app.thread_area.0
        && x < app.thread_area.0 + app.thread_area.2
        && y >= app.thread_area.1
        && y < app.thread_area.1 + app.thread_area.3;
    let in_tabs = x >= app.tabs_area.0
        && x < app.tabs_area.0 + app.tabs_area.2
        && y >= app.tabs_area.1
        && y < app.tabs_area.1 + app.tabs_area.3;
    let status_hit = (y == app.status_area.1).then(|| {
        app.status_hits
            .iter()
            .find(|hit| x >= hit.x && x < hit.x.saturating_add(hit.width))
            .map(|hit| hit.action)
    });

    match mouse.kind {
        MouseEventKind::Moved => {
            if in_editor {
                commands::schedule_hover(app, x, y);
            } else {
                commands::cancel_hover(app);
            }
            if in_explorer && app.git_open {
                commands::hover_git_row(app, y);
            } else {
                app.git_hover = None;
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            commands::cancel_hover(app);
            if matches!(app.overlay, Overlay::Blame { .. }) {
                let inside = app.overlay_area.is_some_and(|(ax, ay, width, height)| {
                    x >= ax
                        && x < ax.saturating_add(width)
                        && y >= ay
                        && y < ay.saturating_add(height)
                });
                if inside {
                    return;
                }
                commands::dismiss_overlay(app);
            }
            if matches!(app.overlay, Overlay::Branches { .. }) {
                if commands::click_branch_row(app, x, y) {
                    commands::overlay_submit(app);
                    return;
                }
                if in_rect(app.overlay_area, x, y) {
                    return;
                }
                commands::dismiss_overlay(app);
                if let Some(Some(action)) = status_hit
                    && action == crate::app::state::StatusAction::Git
                {
                    return;
                }
            }
            if let Some(scope) = app
                .search_scope_hits
                .iter()
                .find(|hit| y == hit.y && x >= hit.x && x < hit.x.saturating_add(hit.width))
                .map(|hit| hit.scope)
            {
                commands::set_search_scope(app, scope);
                return;
            }
            if let Some(open) = commands::click_search_row(app, x, y) {
                let _ = events::note_click(app, x, y);
                if open {
                    commands::overlay_submit(app);
                }
                return;
            }
            if matches!(app.overlay, Overlay::Search { .. }) && in_rect(app.overlay_area, x, y) {
                return;
            }
            if matches!(app.overlay, Overlay::Prompt { .. }) {
                if !commands::click_prompt(app, x, y) {
                    commands::dismiss_overlay(app);
                }
                return;
            }
            if matches!(
                app.overlay,
                Overlay::Themes { .. } | Overlay::Help | Overlay::TypesNote
            ) {
                let was_themes = matches!(app.overlay, Overlay::Themes { .. });
                let was_help = matches!(app.overlay, Overlay::Help);
                let was_note = matches!(app.overlay, Overlay::TypesNote);
                let inside = app.overlay_area.is_some_and(|(ax, ay, width, height)| {
                    x >= ax
                        && x < ax.saturating_add(width)
                        && y >= ay
                        && y < ay.saturating_add(height)
                });
                if inside {
                    if was_themes {
                        commands::click_themes(app, y);
                    } else if was_help {
                        commands::click_help(app, y);
                    } else if was_note {
                        commands::click_types_note(app, x, y);
                    }
                    return;
                }
                commands::dismiss_overlay(app);
                if let Some(Some(action)) = status_hit {
                    let same = (was_themes && action == crate::app::state::StatusAction::Theme)
                        || (was_help && action == crate::app::state::StatusAction::Help)
                        || (was_note && action == crate::app::state::StatusAction::Types);
                    if same {
                        return;
                    }
                }
            }
            if matches!(app.overlay, Overlay::ContextMenu { .. }) {
                if let Some(action) = crate::ui::context_menu::action_at(app, x, y) {
                    app.overlay = Overlay::None;
                    commands::run_menu(app, action);
                } else if app.menu_area.is_some_and(|(ax, ay, width, height)| {
                    x >= ax
                        && x < ax.saturating_add(width)
                        && y >= ay
                        && y < ay.saturating_add(height)
                }) {
                } else {
                    commands::dismiss_overlay(app);
                }
                return;
            }
            let dbl = events::note_click(app, x, y);
            if commands::begin_explorer_resize(app, x, y) {
                return;
            }
            if let Some(Some(action)) = status_hit {
                match action {
                    crate::app::state::StatusAction::Markdown => app.toggle_markdown_preview(),
                    crate::app::state::StatusAction::Theme => commands::open_themes(app),
                    crate::app::state::StatusAction::Help => commands::open_help(app),
                    crate::app::state::StatusAction::Types => commands::click_types_status(app),
                    crate::app::state::StatusAction::Git => app.open_branch_picker(),
                    crate::app::state::StatusAction::Agent => {
                        if commands::claude_ready(app) {
                            if app.thread.running {
                                commands::stop_agent(app);
                            } else {
                                app.thread_visible = !app.thread_visible;
                                if app.thread_visible {
                                    app.focus = Focus::Thread;
                                } else if app.focus == Focus::Thread {
                                    app.focus = Focus::Editor;
                                }
                            }
                        }
                    }
                }
            } else if app.pane_mark.is_some_and(|(hit_x, hit_y, width)| {
                y == hit_y && x >= hit_x && x < hit_x.saturating_add(width)
            }) {
                app.explorer_visible = !app.explorer_visible;
            } else if in_tabs && !scroll_tabs(app, x) {
                if let Some(tab) = app
                    .tab_hit_boxes
                    .iter()
                    .find(|tab| x >= tab.x && x < tab.x + tab.width)
                    .copied()
                {
                    if x == tab.close_x {
                        app.close_document(tab.document_id);
                    } else {
                        app.active = Some(tab.document_id);
                        app.focus = Focus::Editor;
                        app.overlay = Overlay::None;
                        app.reveal_active_in_explorer();
                    }
                }
            } else if in_search {
                if let Some(action) = app
                    .header_hits
                    .iter()
                    .find(|hit| x >= hit.x && x < hit.x.saturating_add(hit.width))
                    .map(|hit| hit.action)
                {
                    match action {
                        HeaderAction::Find => {
                            if matches!(
                                app.overlay,
                                Overlay::Search {
                                    kind: SearchKind::Find,
                                    ..
                                }
                            ) {
                                commands::dismiss_overlay(app);
                            } else {
                                commands::open_search(app);
                            }
                        }
                        HeaderAction::Search => commands::open_content_search(app),
                    }
                }
            } else if in_explorer {
                if let Some(changes) = side_tab_at(app, x, y) {
                    app.show_changes(changes);
                } else if app.git_open {
                    commands::click_git(app, x, y);
                } else {
                    app.focus = Focus::Explorer;
                    commands::click_explorer(app, y);
                }
            } else if in_thread {
                if let Some((hit_x, hit_y, width)) = app.thread_stop
                    && y == hit_y
                    && x >= hit_x
                    && x < hit_x.saturating_add(width)
                {
                    commands::stop_agent(app);
                } else if let Some(path) = app
                    .thread_hits
                    .iter()
                    .find(|hit| y == hit.y && x >= hit.x && x < hit.x.saturating_add(hit.width))
                    .map(|hit| hit.path.clone())
                {
                    app.open_git_entry(path, false);
                } else {
                    app.focus = Focus::Thread;
                    app.overlay = Overlay::None;
                }
            } else if in_editor {
                app.git_message_focus = false;
                app.git_body_focus = false;
                app.focus = Focus::Editor;
                if y == app.editor_area.1 {
                    if let Some((hit_x, hit_y, width)) = app.restore_hit
                        && y == hit_y
                        && x >= hit_x
                        && x < hit_x.saturating_add(width)
                    {
                        commands::restore_active(app);
                    }
                    return;
                }
                if !commands::begin_hscroll_drag(app, x, y) {
                    if mouse.modifiers.contains(KeyModifiers::CONTROL) {
                        commands::click_editor(app, x, y, false);
                        commands::go_to_definition(app);
                    } else if dbl {
                        commands::double_click_editor(app, x, y);
                    } else {
                        commands::click_editor(app, x, y, false);
                        app.dragging = true;
                    }
                }
            }
        }
        MouseEventKind::Drag(MouseButton::Left) if app.explorer_resizing => {
            commands::drag_explorer_resize(app, x);
        }
        MouseEventKind::Drag(MouseButton::Left) if app.hscroll_dragging => {
            commands::drag_hscroll(app, x);
        }
        MouseEventKind::Drag(MouseButton::Left) if app.dragging => {
            commands::click_editor(app, x, y, true);
        }
        MouseEventKind::Up(MouseButton::Left) => {
            commands::end_explorer_resize(app);
            app.dragging = false;
            app.hscroll_dragging = false;
        }
        MouseEventKind::Down(MouseButton::Middle) if in_tabs => {
            if let Some(tab) = app
                .tab_hit_boxes
                .iter()
                .find(|tab| x >= tab.x && x < tab.x + tab.width)
                .copied()
            {
                app.close_document(tab.document_id);
            }
        }
        MouseEventKind::Down(MouseButton::Right) => {
            commands::cancel_hover(app);
            if in_tabs {
                if let Some(tab) = app
                    .tab_hit_boxes
                    .iter()
                    .find(|tab| x >= tab.x && x < tab.x + tab.width)
                    .copied()
                {
                    commands::show_tab_menu(app, tab.document_id, x, y);
                }
            } else if in_explorer && app.git_open && y != app.explorer_area.1 {
                if let Some(index) = commands::git_file_index_at(app, y) {
                    app.git_selected = index;
                    app.focus = Focus::Explorer;
                }
            } else if in_explorer && !app.git_open && y != app.explorer_area.1 {
                app.focus = Focus::Explorer;
                if let Some(idx) = crate::app::state::explorer_index_at(
                    app.explorer_scroll,
                    app.explorer_area.1,
                    app.explorer_area.3,
                    y,
                    app.explorer_entries.len(),
                ) {
                    app.explorer_selected = idx;
                }
                commands::show_explorer_menu(app, x, y);
            } else if in_editor && y != app.editor_area.1 && app.file_content_showing() {
                app.focus = Focus::Editor;
                commands::click_editor(app, x, y, false);
                commands::show_editor_menu(app, x, y);
            }
        }
        MouseEventKind::ScrollDown => {
            dispatch_wheel(
                app,
                mouse,
                x,
                y,
                in_explorer,
                in_editor,
                in_thread,
                WheelAxis::Vertical(3),
            );
        }
        MouseEventKind::ScrollUp => {
            dispatch_wheel(
                app,
                mouse,
                x,
                y,
                in_explorer,
                in_editor,
                in_thread,
                WheelAxis::Vertical(-3),
            );
        }
        MouseEventKind::ScrollLeft => {
            dispatch_wheel(
                app,
                mouse,
                x,
                y,
                in_explorer,
                in_editor,
                in_thread,
                WheelAxis::Horizontal(-4),
            );
        }
        MouseEventKind::ScrollRight => {
            dispatch_wheel(
                app,
                mouse,
                x,
                y,
                in_explorer,
                in_editor,
                in_thread,
                WheelAxis::Horizontal(4),
            );
        }
        _ => {}
    }
}

fn dispatch_wheel(
    app: &mut App,
    mouse: MouseEvent,
    x: u16,
    y: u16,
    in_explorer: bool,
    in_editor: bool,
    in_thread: bool,
    axis: WheelAxis,
) {
    commands::cancel_hover(app);
    if matches!(app.overlay, Overlay::Search { .. }) && in_rect(app.overlay_area, x, y) {
        if let WheelAxis::Vertical(delta) = axis {
            commands::scroll_search(app, delta);
        }
        return;
    }
    if matches!(app.overlay, Overlay::Branches { .. }) && in_rect(app.overlay_area, x, y) {
        if let WheelAxis::Vertical(delta) = axis {
            commands::scroll_branches(app, delta);
        }
        return;
    }
    let in_popup = in_rect(app.overlay_area, x, y) || in_rect(app.menu_area, x, y);
    let shift = mouse.modifiers.contains(KeyModifiers::SHIFT);
    match commands::wheel_action(in_popup, in_explorer, in_editor, in_thread, shift, axis) {
        WheelAction::Explorer(delta) => commands::scroll_explorer(app, delta),
        WheelAction::EditorVertical(delta) => commands::scroll_editor(app, delta),
        WheelAction::EditorHorizontal(delta) => commands::scroll_editor_horizontal(app, delta),
        WheelAction::Thread(delta) => commands::scroll_thread(app, delta),
        WheelAction::None => {}
    }
}

fn side_tab_at(app: &App, x: u16, y: u16) -> Option<bool> {
    app.side_tabs
        .iter()
        .find(|hit| y == hit.y && x >= hit.x && x < hit.x.saturating_add(hit.width))
        .map(|hit| hit.changes)
}

fn in_rect(area: Option<(u16, u16, u16, u16)>, x: u16, y: u16) -> bool {
    area.is_some_and(|(ax, ay, w, h)| {
        x >= ax && x < ax.saturating_add(w) && y >= ay && y < ay.saturating_add(h)
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crossterm::event::{
        Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use tokio::sync::mpsc;

    use crate::app::commands;
    use crate::app::state::{App, HeaderAction, HeaderHit, Overlay, SearchKind, SearchScope};
    use crate::language::LanguageClient;
    use crate::workspace::SearchMatch;

    use super::handle_event;

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
        let mut app = App::new(PathBuf::from("."), LanguageClient { tx });
        app.open_untitled();
        app.editor_area = (0, 0, 80, 24);
        app
    }

    fn left_click(column: u16, row: u16) -> Event {
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::empty(),
        })
    }

    fn right_click(column: u16, row: u16) -> Event {
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column,
            row,
            modifiers: KeyModifiers::empty(),
        })
    }

    fn symbol_menu(app: &App) -> bool {
        matches!(
            app.overlay,
            Overlay::ContextMenu {
                show_type: true,
                ..
            }
        )
    }

    #[test]
    fn symbol_menu_opens_on_file_content() {
        with_runtime(|| {
            let mut app = test_app();
            handle_event(&mut app, right_click(10, 5));
            assert!(symbol_menu(&app));
        });
    }

    fn overlay_query(app: &App) -> String {
        match &app.overlay {
            Overlay::Search { query, .. } => query.clone(),
            other => panic!("expected a search query, got {other:?}"),
        }
    }

    fn editor_text(app: &App) -> String {
        app.active_doc().map(|doc| doc.text()).unwrap_or_default()
    }

    fn ctrl_v() -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL))
    }

    fn arm_header(app: &mut App) {
        app.explorer_visible = true;
        app.search_area = (0, 0, 40, 1);
        app.header_hits = vec![
            HeaderHit {
                action: HeaderAction::Find,
                x: 0,
                width: 6,
            },
            HeaderHit {
                action: HeaderAction::Search,
                x: 7,
                width: 8,
            },
        ];
    }

    fn search_kind(app: &App) -> Option<SearchKind> {
        match app.overlay {
            Overlay::Search { kind, .. } => Some(kind),
            _ => None,
        }
    }

    fn search_window(app: &App) -> (usize, usize) {
        match &app.overlay {
            Overlay::Search {
                selected, scroll, ..
            } => (*selected, *scroll),
            other => panic!("expected a search list, got {other:?}"),
        }
    }

    fn long_find(app: &mut App) {
        app.search_list_rows = 10;
        app.overlay_area = Some((0, 0, 40, 18));
        app.overlay = Overlay::Search {
            kind: SearchKind::Find,
            scope: SearchScope::Global,
            query: String::new(),
            files: (0..40).map(|index| format!("file-{index:02}")).collect(),
            matches: Vec::new(),
            file_matches: Vec::new(),
            selected: 0,
            scroll: 0,
            searching: false,
        };
    }

    fn scroll_down(column: u16, row: u16) -> Event {
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column,
            row,
            modifiers: KeyModifiers::empty(),
        })
    }

    #[test]
    fn find_list_scrolls_with_arrows_and_the_wheel() {
        with_runtime(|| {
            let mut app = test_app();
            long_find(&mut app);

            for _ in 0..12 {
                handle_event(
                    &mut app,
                    Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::empty())),
                );
            }
            let (selected, scroll) = search_window(&app);
            assert_eq!((selected, scroll), (12, 3));
            assert!(selected >= scroll && selected < scroll + 10);

            handle_event(&mut app, scroll_down(5, 5));
            let (selected, scroll) = search_window(&app);
            assert_eq!(scroll, 6);
            assert!(selected >= scroll && selected < scroll + 10);

            long_find(&mut app);
            handle_event(&mut app, scroll_down(5, 5));
            let (selected, scroll) = search_window(&app);
            assert_eq!((selected, scroll), (3, 3));
            assert!(selected >= scroll && selected < scroll + 10);
        });
    }

    #[test]
    fn clicking_the_open_find_or_search_button_closes_it() {
        with_runtime(|| {
            let mut app = test_app();
            arm_header(&mut app);

            handle_event(&mut app, left_click(1, 0));
            assert_eq!(search_kind(&app), Some(SearchKind::Find));
            handle_event(&mut app, left_click(1, 0));
            assert!(matches!(app.overlay, Overlay::None));

            handle_event(&mut app, left_click(8, 0));
            assert_eq!(search_kind(&app), Some(SearchKind::Content));
            handle_event(&mut app, left_click(8, 0));
            assert!(matches!(app.overlay, Overlay::None));

            commands::open_search(&mut app);
            handle_event(&mut app, left_click(8, 0));
            assert_eq!(search_kind(&app), Some(SearchKind::Content));
        });
    }

    #[test]
    fn paste_fills_find_and_search_without_touching_the_editor() {
        with_runtime(|| {
            let mut app = test_app();
            let before = editor_text(&app);

            commands::open_search(&mut app);
            handle_event(&mut app, Event::Paste("src/main.rs".into()));
            assert_eq!(overlay_query(&app), "src/main.rs");

            commands::open_content_search(&mut app);
            handle_event(&mut app, Event::Paste("foo\nbar\u{1}".into()));
            assert_eq!(overlay_query(&app), "foo bar");

            app.clipboard.copy("file-query");
            commands::open_search(&mut app);
            handle_event(&mut app, ctrl_v());
            assert_eq!(overlay_query(&app), "file-query");

            app.clipboard.copy("search-query");
            commands::open_content_search(&mut app);
            handle_event(&mut app, ctrl_v());
            assert_eq!(overlay_query(&app), "search-query");

            assert_eq!(editor_text(&app), before);
        });
    }

    #[test]
    fn double_clicking_a_search_row_opens_it() {
        with_runtime(|| {
            let dir = std::env::temp_dir().join(format!("p0-click-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("a.txt"), "alpha\n").unwrap();
            std::fs::write(dir.join("b.txt"), "beta\nsecond\n").unwrap();
            let (tx, _rx) = mpsc::unbounded_channel();
            let mut app = App::new(dir.clone(), LanguageClient { tx });
            app.overlay = Overlay::Search {
                kind: SearchKind::Find,
                scope: SearchScope::Global,
                query: String::new(),
                files: vec!["a.txt".into(), "b.txt".into()],
                matches: Vec::new(),
                file_matches: Vec::new(),
                selected: 0,
                scroll: 0,
                searching: false,
            };
            app.overlay_area = Some((2, 4, 40, 12));
            app.search_row_span = Some((4, 20));
            app.search_rows = vec![(0, 8), (1, 9)];

            handle_event(&mut app, left_click(3, 9));
            assert!(matches!(app.overlay, Overlay::Search { selected: 0, .. }));

            handle_event(&mut app, left_click(6, 9));
            assert!(matches!(app.overlay, Overlay::Search { selected: 1, .. }));
            assert!(app.active_doc().is_none());

            handle_event(&mut app, left_click(10, 9));
            assert!(matches!(app.overlay, Overlay::None));
            assert!(
                app.active_doc()
                    .and_then(|doc| doc.path.as_ref())
                    .is_some_and(|path| path.ends_with("b.txt"))
            );

            app.overlay = Overlay::Search {
                kind: SearchKind::Content,
                scope: SearchScope::Global,
                query: "second".into(),
                files: Vec::new(),
                matches: vec![SearchMatch {
                    path: dir.join("b.txt"),
                    line: 1,
                    column: 0,
                    text: "second".into(),
                }],
                file_matches: Vec::new(),
                selected: 0,
                scroll: 0,
                searching: false,
            };
            app.search_row_span = Some((4, 20));
            app.search_rows = vec![(0, 7)];
            handle_event(&mut app, left_click(8, 7));
            assert!(matches!(app.overlay, Overlay::Search { selected: 0, .. }));
            assert_eq!(app.active_view().unwrap().cursor.line, 0);
            handle_event(&mut app, left_click(12, 7));
            assert!(matches!(app.overlay, Overlay::None));
            assert_eq!(app.active_view().unwrap().cursor.line, 1);
            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    #[test]
    fn symbol_menu_stays_closed_in_markdown_preview() {
        with_runtime(|| {
            let mut app = test_app();
            let id = app.active.expect("open file");
            app.markdown_preview = Some(crate::markdown::Preview::open(id));
            handle_event(&mut app, right_click(10, 5));
            assert!(matches!(app.overlay, Overlay::None));
        });
    }
}
