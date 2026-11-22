use ratatui::Frame;
use ratatui::layout::{Position as TermPos, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::app::App;
use crate::app::state::{Overlay, ScopeHit, SearchKind, SearchScope};
use crate::language::{ServerKind, install_command, server_requirement};
use crate::ui::theme::Theme;

pub fn render(frame: &mut Frame, app: &mut App) {
    let theme = app.theme();
    app.types_copy = None;
    app.prompt_caret = None;
    app.search_scope_hits.clear();
    app.search_rows.clear();
    app.search_row_span = None;
    if !matches!(
        app.overlay,
        Overlay::Themes { .. } | Overlay::Help | Overlay::TypesNote
    ) {
        app.overlay_area = None;
        app.overlay_rows.clear();
    }
    let completion_selected = match &app.overlay {
        Overlay::Completion { selected, .. } => Some(*selected),
        _ => None,
    };
    if let Some(selected) = completion_selected {
        render_completions(frame, app, selected, theme);
        return;
    }
    let themes_selected = match &app.overlay {
        Overlay::Themes { selected } => Some(*selected),
        _ => None,
    };
    if let Some(selected) = themes_selected {
        render_themes(frame, app, selected, theme);
        return;
    }
    if matches!(app.overlay, Overlay::Help) {
        render_help(frame, app, theme);
        return;
    }
    if matches!(app.overlay, Overlay::TypesNote) {
        render_types_note(frame, app, theme);
        return;
    }
    let branch_picker = match &app.overlay {
        Overlay::Branches {
            query,
            branches,
            selected,
            scroll,
            loading,
            error,
            ..
        } => Some((
            query.clone(),
            branches.clone(),
            *selected,
            *scroll,
            *loading,
            error.clone(),
        )),
        _ => None,
    };
    if let Some((query, branches, selected, scroll, loading, error)) = branch_picker {
        render_branches(
            frame,
            app,
            theme,
            &query,
            &branches,
            selected,
            scroll,
            loading,
            error.as_deref(),
        );
        return;
    }
    let blame = match &app.overlay {
        Overlay::Blame {
            author,
            summary,
            hash,
            time,
            x,
            y,
        } => Some((author.clone(), summary.clone(), hash.clone(), *time, *x, *y)),
        _ => None,
    };
    if let Some((author, summary, hash, time, x, y)) = blame {
        render_blame(frame, app, theme, &author, &summary, &hash, time, x, y);
        return;
    }
    let commit = match &app.overlay {
        Overlay::Commit {
            hash,
            author,
            summary,
            body,
            time,
            x,
            y,
            ..
        } => Some((
            hash.clone(),
            author.clone(),
            summary.clone(),
            body.clone(),
            *time,
            *x,
            *y,
        )),
        _ => None,
    };
    if let Some((hash, author, summary, body, time, x, y)) = commit {
        render_commit(
            frame, app, theme, &hash, &author, &summary, &body, time, x, y,
        );
        return;
    }
    match &app.overlay {
        Overlay::Search {
            kind: SearchKind::Content,
            ..
        } => render_content_search(frame, app, theme),
        Overlay::Search {
            kind,
            query,
            files,
            matches,
            selected,
            scroll,
            searching,
            ..
        } => {
            let mut items = files.clone();
            items.extend(matches.iter().map(|m| {
                format!(
                    "{}:{}  {}",
                    crate::app::App::relative_static(app, &m.path),
                    m.line + 1,
                    m.text
                )
            }));
            let title = match (*kind, *searching) {
                (SearchKind::Find, true) => " Open (searching) ",
                _ => " Open ",
            };
            let list_area = render_list(
                frame,
                frame.area(),
                title,
                query,
                &items,
                *selected,
                *scroll,
                true,
                true,
                true,
                theme,
            );
            app.search_rows = list_area.hits;
            app.search_row_span = list_area.span;
            app.overlay_area = Some((list_area.x, list_area.y, list_area.width, list_area.height));
            app.search_list_rows = list_area.rows;
        }
        Overlay::Completion { .. } => {}
        Overlay::Hover {
            title, text, x, y, ..
        } => {
            let content = parse_hover_content(text);
            let width = content
                .iter()
                .map(|(line, _)| UnicodeWidthStr::width(line.as_str()) as u16)
                .max()
                .unwrap_or(20)
                .saturating_add(4)
                .clamp(24, 72)
                .min(frame.area().width.saturating_sub(2));
            let inner_width = (width as usize).saturating_sub(2).max(1);
            let wrapped: Vec<(String, HoverLineKind)> = content
                .iter()
                .flat_map(|(line, kind)| {
                    wrap_to_width(line, inner_width)
                        .into_iter()
                        .map(|row| (row, *kind))
                })
                .collect();
            let height = (wrapped.len() as u16 + 2)
                .clamp(3, 14)
                .min(frame.area().height.saturating_sub(2));
            let mut area = Rect {
                x: x.saturating_add(1),
                y: y.saturating_add(1),
                width,
                height,
            };
            if area.right() > frame.area().right() {
                area.x = frame.area().right().saturating_sub(width);
            }
            if area.bottom() > frame.area().bottom() {
                area.y = y.saturating_sub(height);
            }
            frame.render_widget(Clear, area);
            let lines: Vec<Line> = wrapped
                .into_iter()
                .take(height.saturating_sub(2) as usize)
                .map(|(line, kind)| {
                    let style = match kind {
                        HoverLineKind::Signature => Style::default()
                            .fg(theme.accent)
                            .add_modifier(Modifier::BOLD),
                        HoverLineKind::Tag => Style::default().fg(theme.warning),
                        HoverLineKind::Doc => Style::default().fg(theme.fg_muted),
                    };
                    Line::from(Span::styled(line, style))
                })
                .collect();
            let heading = format!(" {title} ");
            frame.render_widget(
                Paragraph::new(lines)
                    .style(theme.text())
                    .block(theme.panel(&heading, false)),
                area,
            );
        }
        Overlay::Signature { text } => {
            let area = bottom_box(frame.area(), 8, 60);
            frame.render_widget(Clear, area);
            frame.render_widget(
                Paragraph::new(text.as_str())
                    .style(theme.text())
                    .wrap(Wrap { trim: false })
                    .block(theme.panel(" Info ", false)),
                area,
            );
        }
        Overlay::References { items, selected } => {
            let list: Vec<String> = items
                .iter()
                .map(|l| {
                    format!(
                        "{}:{}",
                        crate::app::App::relative_static(app, &l.path),
                        l.position.line + 1
                    )
                })
                .collect();
            let _ = render_list(
                frame,
                frame.area(),
                " References ",
                "",
                &list,
                *selected,
                0,
                false,
                false,
                false,
                theme,
            );
        }
        Overlay::Confirm { message, .. } => {
            let area = center(frame.area(), 6, 50);
            frame.render_widget(Clear, area);
            frame.render_widget(
                Paragraph::new(format!("{message}\n\nEnter = confirm   Esc = cancel"))
                    .style(theme.text())
                    .block(theme.panel(" Confirm ", false)),
                area,
            );
        }
        Overlay::Prompt {
            title,
            input,
            cursor,
            ..
        } => {
            let area = center(frame.area(), 5, 50);
            app.overlay_area = Some((area.x, area.y, area.width, area.height));
            frame.render_widget(Clear, area);
            let prompt_title = format!(" {title} ");
            let room = area.width.saturating_sub(2) as usize;
            let (visible, col, skip) = crate::app::commands::prompt_window(input, *cursor, room);
            let y = area.y.saturating_add(1);
            let x = area.x.saturating_add(1);
            app.prompt_caret = Some((x, y, skip));
            let gap = room.saturating_sub(unicode_width::UnicodeWidthStr::width(visible.as_str()));
            let shown = format!("{visible}{}", " ".repeat(gap));
            frame.render_widget(
                Paragraph::new(shown)
                    .style(theme.text())
                    .block(theme.panel(&prompt_title, false)),
                area,
            );
            if col < room {
                frame.set_cursor_position(TermPos {
                    x: x.saturating_add(col as u16),
                    y,
                });
            }
        }
        Overlay::Themes { .. }
        | Overlay::Help
        | Overlay::TypesNote
        | Overlay::Blame { .. }
        | Overlay::Commit { .. }
        | Overlay::ContextMenu { .. }
        | Overlay::Branches { .. }
        | Overlay::None => {}
    }
}

fn render_themes(
    frame: &mut Frame,
    app: &mut App,
    selected: usize,
    theme: crate::ui::theme::Theme,
) {
    let names = crate::ui::theme::theme_names();
    let width = 18u16.min(frame.area().width.saturating_sub(2)).max(12);
    let height = (names.len() as u16 + 2).min(frame.area().height.saturating_sub(1));
    let anchor = app
        .status_hits
        .iter()
        .find(|hit| hit.action == crate::app::state::StatusAction::Theme);
    let x = anchor
        .map(|hit| hit.x)
        .unwrap_or(frame.area().right().saturating_sub(width))
        .min(frame.area().right().saturating_sub(width));
    let y = frame
        .area()
        .bottom()
        .saturating_sub(height.saturating_add(1));
    let area = Rect {
        x,
        y,
        width,
        height,
    };
    app.overlay_area = Some((area.x, area.y, area.width, area.height));
    app.overlay_rows = names
        .iter()
        .enumerate()
        .map(|(index, _)| (index, area.y + 1 + index as u16))
        .filter(|(_, row)| *row < area.bottom().saturating_sub(1))
        .collect();
    frame.render_widget(Clear, area);
    let items: Vec<ListItem> = names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let current = *name == theme.name;
            let mut style = Style::default().fg(theme.fg);
            if index == selected {
                style = style.bg(theme.bg_selected).add_modifier(Modifier::BOLD);
            }
            let mark = if current { "● " } else { "  " };
            ListItem::new(Span::styled(format!("{mark}{name}"), style))
        })
        .collect();
    frame.render_widget(
        List::new(items)
            .style(theme.text())
            .block(theme.panel(" Theme ", false)),
        area,
    );
}

fn render_types_note(frame: &mut Frame, app: &mut App, theme: Theme) {
    let width = 62u16.min(frame.area().width.saturating_sub(2)).max(24);
    let copy = " Copy ";
    let mut lines = vec![
        Line::from(Span::styled(
            " Editing works without it.",
            Style::default().fg(theme.fg),
        )),
        Line::from(""),
        Line::from(Span::styled(
            " Types, completion, and go to definition need",
            Style::default().fg(theme.fg),
        )),
        Line::from(Span::styled(
            format!(" {}", server_requirement(app.types_note_server)),
            Style::default().fg(theme.fg),
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!(" {}", install_command(app.types_note_server)),
            Style::default().fg(theme.accent),
        )),
        Line::from(Span::styled(
            copy,
            Style::default()
                .fg(theme.on_accent)
                .bg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )),
    ];
    if app.types_note_server == ServerKind::Java {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            " It also needs a JDK 21 or newer.",
            Style::default().fg(theme.fg),
        )));
    }
    lines.extend([
        Line::from(""),
        Line::from(Span::styled(
            " Click ⊘ types off to check again, or restart r3vu.",
            Style::default().fg(theme.fg_muted),
        )),
        Line::from(Span::styled(
            " Enter dismisses this note.",
            Style::default().fg(theme.fg_muted),
        )),
    ]);
    let copy_row = 6u16;
    let height = (lines.len() as u16 + 2)
        .min(frame.area().height.saturating_sub(2))
        .max(3);
    let area = center(frame.area(), height, width);
    app.overlay_area = Some((area.x, area.y, area.width, area.height));
    app.overlay_rows.clear();
    let copy_y = area.y.saturating_add(1).saturating_add(copy_row);
    if copy_y < area.bottom().saturating_sub(1) {
        app.types_copy = Some((area.x.saturating_add(1), copy_y, copy.len() as u16));
    }
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme.text())
            .wrap(Wrap { trim: false })
            .block(theme.panel(" Types ", false)),
        area,
    );
}

fn render_help(frame: &mut Frame, app: &mut App, theme: crate::ui::theme::Theme) {
    let shortcuts = shortcut_rows(app.git_repo).len();
    let width = 52u16.min(frame.area().width.saturating_sub(2)).max(24);
    let height = (shortcuts as u16 + 6)
        .min(frame.area().height.saturating_sub(2))
        .max(3);
    let area = center(frame.area(), height, width);
    app.overlay_area = Some((area.x, area.y, area.width, area.height));
    let inner = area.height.saturating_sub(2) as usize;
    let rows = visible_help_rows(inner, app.git_repo);
    app.overlay_rows = rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| match row {
            HelpRow::Toggle => Some((0, area.y.saturating_add(1 + index as u16))),
            _ => None,
        })
        .collect();
    frame.render_widget(Clear, area);
    let lines: Vec<Line> = rows
        .into_iter()
        .map(|row| match row {
            HelpRow::Header(title) => Line::from(Span::styled(
                format!(" {title}"),
                Style::default()
                    .fg(theme.fg_muted)
                    .add_modifier(Modifier::BOLD),
            )),
            HelpRow::Gap => Line::from(""),
            HelpRow::Toggle => claude_toggle_line(app.config.claude_enabled, theme),
            HelpRow::Shortcut(keys, action) => Line::from(vec![
                Span::styled(format!(" {keys:<16}"), Style::default().fg(theme.accent)),
                Span::styled(action, Style::default().fg(theme.fg)),
            ]),
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme.text())
            .block(theme.panel(" Help ", false)),
        area,
    );
}

fn claude_toggle_line(enabled: bool, theme: Theme) -> Line<'static> {
    let (state, hint, style) = if enabled {
        (
            "on ",
            "click to turn off",
            Style::default()
                .fg(theme.on_accent)
                .bg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        (
            "off",
            "click to turn on",
            Style::default().fg(theme.fg_muted),
        )
    };
    Line::from(vec![
        Span::styled(
            " Claude          ".to_string(),
            Style::default().fg(theme.accent),
        ),
        Span::styled(format!("{state}  {hint}"), style),
    ])
}

enum HelpRow {
    Header(&'static str),
    Gap,
    Toggle,
    Shortcut(&'static str, &'static str),
}

fn visible_help_rows(limit: usize, git: bool) -> Vec<HelpRow> {
    if limit == 0 {
        return Vec::new();
    }
    let mut rows = Vec::new();
    let mut left = limit;
    if left > 1 {
        rows.push(HelpRow::Header("Settings"));
        left -= 1;
    }
    rows.push(HelpRow::Toggle);
    left -= 1;
    let shortcuts = shortcut_rows(git);
    if left >= 2 {
        if left >= shortcuts.len() + 2 {
            rows.push(HelpRow::Gap);
            left -= 1;
        }
        rows.push(HelpRow::Header("Help"));
        left -= 1;
    }
    rows.extend(
        shortcuts
            .into_iter()
            .take(left)
            .map(|(keys, action)| HelpRow::Shortcut(keys, action)),
    );
    rows
}

pub(crate) fn shortcut_rows(git: bool) -> Vec<(&'static str, &'static str)> {
    let mut rows = vec![
        ("Ctrl+/", "Show this list"),
        ("Ctrl+S", "Save"),
        ("Ctrl+P", "Open file from selection"),
        ("Click Search", "Search the repo"),
        ("Ctrl+F", "Search this file"),
        ("Click File / Global", "Switch search scope"),
        ("Ctrl+Enter", "Go to definition"),
        ("Option+T", "Ask Claude about the selection"),
        ("Option+E", "Edit the selection with Claude"),
        ("Click Claude", "Show the pane, or stop a running turn"),
        ("Ctrl+R", "Rename symbol"),
        ("Ctrl+Z / Y", "Undo / redo"),
        ("Ctrl+Tab", "Next / previous tab"),
    ];
    if git {
        rows.extend([
            ("Ctrl+Shift+G", "Git panel"),
            ("Click Changes", "Stage and commit"),
            ("Space", "Stage or unstage the file or folder"),
            ("Click ↩", "Discard or restore the file or folder"),
            ("Click ▾", "Fold or unfold a change"),
            ("Click Stage", "Stage, unstage, discard, or revert a hunk"),
            ("Click Current", "Keep a side, then save and stage the file"),
        ]);
    }
    rows.extend([
        ("Ctrl+Shift+M", "Preview Markdown"),
        ("Click Markdown", "Preview this file"),
        ("Click ◧", "Show or hide the explorer"),
        ("Ctrl+B", "Toggle explorer"),
        ("Ctrl+W", "Close file"),
        ("Ctrl+N", "New file"),
        ("Ctrl+Q", "Quit"),
        ("Esc", "Close a popup"),
    ]);
    rows
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HoverLineKind {
    Signature,
    Tag,
    Doc,
}

pub(crate) fn parse_hover_content(text: &str) -> Vec<(String, HoverLineKind)> {
    let mut in_fence = false;
    let mut result: Vec<(String, HoverLineKind)> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim_end();
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        let clean = line.replace("**", "").replace('`', "");
        let clean = clean.trim_end();
        if clean.trim().is_empty() {
            if result.last().is_some_and(|(line, _)| !line.is_empty()) {
                result.push((String::new(), HoverLineKind::Doc));
            }
            continue;
        }
        let kind = if in_fence {
            HoverLineKind::Signature
        } else if clean.trim_start().starts_with('@') {
            HoverLineKind::Tag
        } else {
            HoverLineKind::Doc
        };
        result.push((clean.to_string(), kind));
    }
    result
}

pub(crate) fn wrap_to_width(text: &str, width: usize) -> Vec<String> {
    if width == 0 || text.is_empty() {
        return vec![String::new()];
    }
    let indent = leading_indent(text, width);
    let indent_width = UnicodeWidthStr::width(indent.as_str());
    let mut rows = Vec::new();
    let mut line = String::new();
    let mut used = 0usize;
    let mut continuation = false;
    for grapheme in text.graphemes(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme).max(1);
        if line.is_empty()
            && continuation
            && indent_width > 0
            && indent_width + grapheme_width <= width
        {
            line.push_str(&indent);
            used = indent_width;
        }
        if used > 0 && used + grapheme_width > width {
            rows.push(std::mem::take(&mut line));
            continuation = true;
            used = 0;
            if indent_width > 0 && indent_width + grapheme_width <= width {
                line.push_str(&indent);
                used = indent_width;
            }
        }
        line.push_str(grapheme);
        used += grapheme_width;
        if used >= width {
            rows.push(std::mem::take(&mut line));
            used = 0;
            continuation = true;
        }
    }
    if !line.is_empty() {
        rows.push(line);
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

fn leading_indent(text: &str, width: usize) -> String {
    let cap = width.saturating_sub(1);
    if cap == 0 {
        return String::new();
    }
    let mut indent = String::new();
    let mut used = 0usize;
    for grapheme in text.graphemes(true) {
        if !grapheme.chars().all(char::is_whitespace) {
            break;
        }
        let grapheme_width = UnicodeWidthStr::width(grapheme).max(1);
        if used + grapheme_width > cap {
            break;
        }
        indent.push_str(grapheme);
        used += grapheme_width;
    }
    indent
}

impl crate::app::App {
    pub fn relative_static(app: &App, path: &std::path::Path) -> String {
        app.relative(path)
    }
}

fn render_completions(frame: &mut Frame, app: &App, selected: usize, theme: Theme) {
    let items = crate::app::commands::visible_completions(app);
    if items.is_empty() {
        return;
    }
    let selected = selected.min(items.len() - 1);
    let (ex, ey, ew, eh) = app.editor_area;
    let origin = app.active_view().map(|v| v.cursor).unwrap_or_default();
    let y = ey.saturating_add(
        (origin
            .line
            .saturating_sub(app.active_view().map(|v| v.viewport.top).unwrap_or(0)))
            as u16,
    );
    let mut height = (items.len() as u16 + 2).min(12).min(eh.saturating_sub(1));
    let mut rect = Rect {
        x: (ex + app.gutter_width + 1).min(ex + ew.saturating_sub(24)),
        y: (y + 1).min(ey + eh.saturating_sub(height)),
        width: 40.min(ew).max(16),
        height,
    };
    if rect.y + rect.height > ey + eh {
        height = (y.saturating_sub(ey)).min(12).max(3);
        rect.y = y.saturating_sub(height);
        rect.height = height;
    }
    frame.render_widget(Clear, rect);
    let list_items: Vec<ListItem> = items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let mut style = Style::default().fg(theme.fg);
            if i == selected {
                style = style.bg(theme.bg_selected).add_modifier(Modifier::BOLD);
            }
            let tag = crate::language::completion_kind_tag(item.kind);
            let tag_style = if i == selected {
                style.fg(theme.accent)
            } else {
                Style::default().fg(theme.accent)
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{tag} "), tag_style),
                Span::styled(item.label.clone(), style),
            ]))
        })
        .collect();
    frame.render_widget(
        List::new(list_items)
            .style(theme.text())
            .block(theme.panel(" Completions ", false)),
        rect,
    );
}

struct ListArea {
    x: u16,
    y: u16,
    width: u16,
    height: u16,
    rows: usize,
    hits: Vec<(usize, u16)>,
    span: Option<(u16, u16)>,
}

fn render_content_search(frame: &mut Frame, app: &mut App, theme: Theme) {
    let (scope, query, matches, file_matches, selected, scroll, searching) = match &app.overlay {
        Overlay::Search {
            scope,
            query,
            matches,
            file_matches,
            selected,
            scroll,
            searching,
            ..
        } => (
            *scope,
            query.clone(),
            matches.clone(),
            file_matches.clone(),
            *selected,
            *scroll,
            *searching,
        ),
        _ => return,
    };
    let global = scope == SearchScope::Global;
    let height = if global { 16 } else { 3 };
    let area = top_box(frame.area(), height, 70);
    frame.render_widget(Clear, area);
    frame.render_widget(theme.panel(" Search ", false), area);
    draw_scope_chips(frame, app, area, scope, theme);
    let inner = Block::default().borders(Borders::ALL).inner(area);
    if inner.width == 0 || inner.height == 0 {
        app.overlay_area = Some((area.x, area.y, area.width, area.height));
        app.search_list_rows = 0;
        return;
    }
    let query_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: 1,
    };
    let count = if global {
        if searching {
            "searching ".to_string()
        } else {
            String::new()
        }
    } else {
        format!(
            "{}/{} ",
            if file_matches.is_empty() {
                0
            } else {
                selected + 1
            },
            file_matches.len()
        )
    };
    let count_w = UnicodeWidthStr::width(count.as_str()) as u16;
    let query_w = query_area.width.saturating_sub(count_w);
    frame.render_widget(
        Paragraph::new(search_query_line(&query, theme)).style(theme.text()),
        Rect {
            x: query_area.x,
            y: query_area.y,
            width: query_w,
            height: 1,
        },
    );
    if count_w > 0 && query_w < query_area.width {
        frame.render_widget(
            Paragraph::new(count)
                .style(Style::default().fg(theme.fg_muted).bg(theme.bg))
                .alignment(ratatui::layout::Alignment::Right),
            Rect {
                x: query_area.x.saturating_add(query_w),
                y: query_area.y,
                width: query_area.width.saturating_sub(query_w),
                height: 1,
            },
        );
    }
    let ruled = global && inner.height >= 3;
    if ruled {
        draw_rule(frame, area, inner.y.saturating_add(1), theme);
    }
    let list_h = inner.height.saturating_sub(1 + u16::from(ruled));
    app.search_list_rows = if global { list_h as usize } else { 0 };
    if global && list_h > 0 {
        let mut items: Vec<String> = matches
            .iter()
            .map(|item| {
                format!(
                    "{}:{}  {}",
                    crate::app::App::relative_static(app, &item.path),
                    item.line + 1,
                    item.text
                )
            })
            .collect();
        if !searching && items.is_empty() && !query.trim().is_empty() {
            items.push("No matches".to_string());
        }
        let list_area = Rect {
            x: inner.x,
            y: inner.y.saturating_add(if ruled { 2 } else { 1 }),
            width: inner.width,
            height: list_h,
        };
        let height = list_area.height as usize;
        let start = scroll.min(items.len().saturating_sub(height));
        let end = (start + height).min(items.len());
        let list_items: Vec<ListItem> = items[start..end]
            .iter()
            .enumerate()
            .map(|(offset, item)| {
                let mut style = Style::default().fg(theme.fg);
                if !matches.is_empty() && start + offset == selected {
                    style = style.bg(theme.bg_selected);
                }
                ListItem::new(Span::styled(item.clone(), style))
            })
            .collect();
        if list_area.width > 0 {
            app.search_row_span = Some((list_area.x, list_area.width));
            for offset in 0..(end - start) {
                let index = start + offset;
                if index < matches.len() {
                    app.search_rows
                        .push((index, list_area.y.saturating_add(offset as u16)));
                }
            }
        }
        frame.render_widget(List::new(list_items), list_area);
    }
    app.overlay_area = Some((area.x, area.y, area.width, area.height));
}

fn draw_scope_chips(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    scope: SearchScope,
    theme: Theme,
) {
    let file_label = " File ";
    let global_label = " Global ";
    let file_w = UnicodeWidthStr::width(file_label) as u16;
    let global_w = UnicodeWidthStr::width(global_label) as u16;
    let global_x = area.right().saturating_sub(1 + global_w);
    let file_x = global_x.saturating_sub(1 + file_w);
    let title_w = UnicodeWidthStr::width(" Search ") as u16;
    if file_x <= area.x.saturating_add(title_w) || global_x <= file_x {
        return;
    }
    let file_enabled = app.active.is_some();
    let chip = |on: bool, enabled: bool| {
        if !enabled {
            Style::default().fg(theme.fg_dim).bg(theme.bg)
        } else if on {
            Style::default()
                .fg(theme.on_accent)
                .bg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.accent).bg(theme.bg)
        }
    };
    let y = area.y;
    frame.render_widget(
        Paragraph::new(file_label).style(chip(scope == SearchScope::File, file_enabled)),
        Rect {
            x: file_x,
            y,
            width: file_w,
            height: 1,
        },
    );
    frame.render_widget(
        Paragraph::new(global_label).style(chip(scope == SearchScope::Global, true)),
        Rect {
            x: global_x,
            y,
            width: global_w,
            height: 1,
        },
    );
    if file_enabled {
        app.search_scope_hits.push(ScopeHit {
            scope: SearchScope::File,
            x: file_x,
            y,
            width: file_w,
        });
    }
    app.search_scope_hits.push(ScopeHit {
        scope: SearchScope::Global,
        x: global_x,
        y,
        width: global_w,
    });
}

fn draw_rule(frame: &mut Frame, area: Rect, y: u16, theme: Theme) {
    if area.width < 2 || y < area.y || y >= area.bottom() {
        return;
    }
    let line = ratatui::symbols::line::NORMAL;
    let text = format!(
        "{}{}{}",
        line.vertical_right,
        line.horizontal
            .repeat(area.width.saturating_sub(2) as usize),
        line.vertical_left
    );
    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(theme.border).bg(theme.bg)),
        Rect {
            x: area.x,
            y,
            width: area.width,
            height: 1,
        },
    );
}

fn render_branches(
    frame: &mut Frame,
    app: &mut App,
    theme: Theme,
    query: &str,
    branches: &[String],
    selected: usize,
    scroll: usize,
    loading: bool,
    error: Option<&str>,
) {
    let filtered = crate::workspace::filter_branches(branches, query);
    let creating = !loading && filtered.is_empty() && !query.trim().is_empty();
    let items: Vec<String> = if creating {
        vec![format!("Create {}", query.trim())]
    } else if loading {
        Vec::new()
    } else {
        filtered
    };
    let anchor_x = app
        .status_hits
        .iter()
        .find(|hit| hit.action == crate::app::state::StatusAction::Git)
        .map(|hit| hit.x)
        .unwrap_or(frame.area().x);
    let area = above_status(
        frame.area(),
        anchor_x,
        if error.is_some() { 20 } else { 19 },
        70,
    );
    frame.render_widget(Clear, area);
    frame.render_widget(theme.panel(" Branch ", false), area);
    app.overlay_area = Some((area.x, area.y, area.width, area.height));
    let inner = Block::default().borders(Borders::ALL).inner(area);
    let mut row_y = inner.y;
    if inner.width > 0 && inner.height > 0 {
        frame.render_widget(
            Paragraph::new(search_query_line(query, theme)).style(theme.text()),
            Rect {
                x: inner.x,
                y: row_y,
                width: inner.width,
                height: 1,
            },
        );
        row_y = row_y.saturating_add(1);
    }
    if let Some(error) = error
        && inner.width > 0
        && row_y < inner.bottom()
    {
        let text = fit_width(&error.replace('\n', " "), inner.width as usize);
        frame.render_widget(
            Paragraph::new(text).style(Style::default().fg(theme.deleted)),
            Rect {
                x: inner.x,
                y: row_y,
                width: inner.width,
                height: 1,
            },
        );
        row_y = row_y.saturating_add(1);
    }
    let show_rule = inner.bottom().saturating_sub(row_y) >= 2;
    if show_rule {
        draw_rule(frame, area, row_y, theme);
        row_y = row_y.saturating_add(1);
    }
    let list_area = Rect {
        x: inner.x,
        y: row_y,
        width: inner.width,
        height: inner.bottom().saturating_sub(row_y),
    };
    let height = list_area.height as usize;
    app.search_list_rows = height;
    let start = scroll.min(items.len().saturating_sub(height.max(1)));
    let end = (start + height).min(items.len());
    let mut hits = Vec::new();
    if loading && list_area.width > 0 && list_area.height > 0 {
        frame.render_widget(
            Paragraph::new(" Loading").style(Style::default().fg(theme.fg_muted)),
            list_area,
        );
    }
    let list_items: Vec<ListItem> = items[start..end]
        .iter()
        .enumerate()
        .map(|(offset, item)| {
            if list_area.width > 0 {
                hits.push((start + offset, list_area.y.saturating_add(offset as u16)));
            }
            let on = start + offset == selected;
            let style = if creating {
                Style::default()
                    .fg(theme.on_accent)
                    .bg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else if on {
                Style::default().fg(theme.fg).bg(theme.bg_selected)
            } else {
                Style::default().fg(theme.fg)
            };
            let shown = if creating {
                let width = list_area.width as usize;
                let text_width = UnicodeWidthStr::width(item.as_str());
                if width == 0 || text_width >= width {
                    fit_width(item, width)
                } else {
                    format!("{item}{}", " ".repeat(width - text_width))
                }
            } else {
                item.clone()
            };
            ListItem::new(Span::styled(shown, style))
        })
        .collect();
    if !loading && list_area.width > 0 && list_area.height > 0 {
        frame.render_widget(List::new(list_items), list_area);
    }
    app.search_rows = hits;
    app.search_row_span = (list_area.width > 0).then_some((list_area.x, list_area.width));
}

fn search_query_line(query: &str, theme: Theme) -> Line<'static> {
    let text = theme.text();
    Line::from(vec![
        Span::styled(" ⌕ ", text.fg(theme.fg_muted)),
        Span::styled(query.to_string(), text),
    ])
}

fn render_list(
    frame: &mut Frame,
    full: Rect,
    title: &str,
    query: &str,
    items: &[String],
    selected: usize,
    scroll: usize,
    rule: bool,
    search: bool,
    at_top: bool,
    theme: Theme,
) -> ListArea {
    let area = if at_top {
        top_box(full, 16, 70)
    } else {
        center(full, if rule { 19 } else { 18 }, 70)
    };
    frame.render_widget(Clear, area);
    frame.render_widget(theme.panel(title, false), area);
    let inner = Block::default().borders(Borders::ALL).inner(area);
    let ruled = rule && inner.height >= 3;
    if inner.width > 0 && inner.height > 0 {
        let line = if search {
            search_query_line(query, theme)
        } else {
            Line::from(Span::styled(query.to_string(), theme.text()))
        };
        frame.render_widget(
            Paragraph::new(line).style(theme.text()),
            Rect {
                x: inner.x,
                y: inner.y,
                width: inner.width,
                height: 1,
            },
        );
    }
    if ruled {
        draw_rule(frame, area, inner.y.saturating_add(1), theme);
    }
    let list_area = Rect {
        x: inner.x,
        y: inner.y.saturating_add(if ruled { 2 } else { 1 }),
        width: inner.width,
        height: inner.height.saturating_sub(if ruled { 2 } else { 1 }),
    };
    let height = list_area.height as usize;
    let start = scroll.min(items.len().saturating_sub(height));
    let end = (start + height).min(items.len());
    let mut hits = Vec::new();
    let list_items: Vec<ListItem> = items[start..end]
        .iter()
        .enumerate()
        .map(|(offset, item)| {
            if list_area.width > 0 {
                hits.push((start + offset, list_area.y.saturating_add(offset as u16)));
            }
            let mut style = Style::default().fg(theme.fg);
            if start + offset == selected {
                style = style.bg(theme.bg_selected);
            }
            ListItem::new(Span::styled(item.clone(), style))
        })
        .collect();
    if list_area.width > 0 && list_area.height > 0 {
        frame.render_widget(List::new(list_items), list_area);
    }
    ListArea {
        x: area.x,
        y: area.y,
        width: area.width,
        height: area.height,
        rows: height,
        span: (list_area.width > 0).then_some((list_area.x, list_area.width)),
        hits,
    }
}

fn render_blame(
    frame: &mut Frame,
    app: &mut App,
    theme: Theme,
    author: &str,
    summary: &str,
    hash: &str,
    time: i64,
    x: u16,
    y: u16,
) {
    let short: String = hash.chars().take(7).collect();
    let summary = if summary.is_empty() {
        "(no message)"
    } else {
        summary
    };
    let when = crate::workspace::format_unix_time(time);
    let rows = [format!("{short}  {summary}"), author.to_string(), when];
    let screen = frame.area();
    let max_inner = screen.width.saturating_sub(4).max(8) as usize;
    let rows: Vec<String> = rows
        .into_iter()
        .map(|row| fit_width(&row, max_inner))
        .collect();
    let inner = rows
        .iter()
        .map(|row| UnicodeWidthStr::width(row.as_str()))
        .max()
        .unwrap_or(8);
    let width = (inner as u16 + 2).clamp(12, screen.width.saturating_sub(1).max(12));
    let height = 5u16.min(screen.height.max(3));
    let x = x.min(screen.right().saturating_sub(width));
    let y = if y >= height {
        y - height
    } else {
        y.saturating_add(1)
            .min(screen.bottom().saturating_sub(height))
    };
    let area = Rect {
        x,
        y,
        width,
        height,
    };
    app.overlay_area = Some((area.x, area.y, area.width, area.height));
    frame.render_widget(Clear, area);
    let lines: Vec<Line> = rows
        .into_iter()
        .map(|row| Line::from(Span::styled(row, Style::default().fg(theme.fg))))
        .collect();
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme.text())
            .block(theme.panel(" Commit ", false)),
        area,
    );
}

fn render_commit(
    frame: &mut Frame,
    app: &mut App,
    theme: Theme,
    hash: &str,
    author: &str,
    summary: &str,
    body: &str,
    time: i64,
    x: u16,
    y: u16,
) {
    let short: String = hash.chars().take(7).collect();
    let summary = if summary.is_empty() {
        "(no message)"
    } else {
        summary
    };
    let when = crate::workspace::format_unix_time(time);
    let screen = frame.area();
    let max_inner = (screen.width.saturating_sub(4).max(8) as usize).min(48);
    let mut rows = vec![
        fit_width(&format!("{short}  {summary}"), max_inner),
        fit_width(author, max_inner),
        fit_width(&when, max_inner),
    ];
    let body = body.trim();
    if !body.is_empty() {
        for line in body.split('\n') {
            let wrapped = wrap_to_width(line, max_inner);
            if wrapped.is_empty() {
                rows.push(String::new());
            } else {
                rows.extend(wrapped);
            }
        }
    }
    let inner = rows
        .iter()
        .map(|row| UnicodeWidthStr::width(row.as_str()))
        .max()
        .unwrap_or(8)
        .min(max_inner);
    let width = (inner as u16 + 2).clamp(12, screen.width.saturating_sub(1).max(12));
    let max_height = 14u16.min(screen.height.max(3));
    let height = (rows.len() as u16 + 2).clamp(3, max_height);
    let x = x.min(screen.right().saturating_sub(width));
    let y = if y >= height {
        y - height
    } else {
        y.saturating_add(1)
            .min(screen.bottom().saturating_sub(height))
    };
    let area = Rect {
        x,
        y,
        width,
        height,
    };
    app.overlay_area = Some((area.x, area.y, area.width, area.height));
    frame.render_widget(Clear, area);
    let lines: Vec<Line> = rows
        .into_iter()
        .take(height.saturating_sub(2) as usize)
        .map(|row| Line::from(Span::styled(row, Style::default().fg(theme.fg))))
        .collect();
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme.text())
            .block(theme.panel(" Commit ", false)),
        area,
    );
}

fn fit_width(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    let mut out = String::new();
    let mut width = 0;
    for ch in text.chars() {
        let ch_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width + 1 > max_width {
            break;
        }
        out.push(ch);
        width += ch_width;
    }
    out.push('…');
    out
}

fn above_status(area: Rect, anchor_x: u16, height: u16, width: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(2)).max(10);
    let height = height.min(area.height.saturating_sub(2)).max(3);
    Rect {
        x: anchor_x.min(area.right().saturating_sub(width)),
        y: area
            .bottom()
            .saturating_sub(height.saturating_add(1))
            .max(area.y),
        width,
        height,
    }
}

fn center(area: Rect, height: u16, width: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(2)).max(10);
    let height = height.min(area.height.saturating_sub(2)).max(3);
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 3,
        width,
        height,
    }
}

fn top_box(area: Rect, height: u16, width: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(2)).max(10);
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + 1,
        width,
        height: height.min(area.height),
    }
}

fn bottom_box(area: Rect, height: u16, width: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(4)).max(12);
    let height = height.min(area.height.saturating_sub(3)).max(3);
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.bottom().saturating_sub(height + 2),
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_popup_opens_with_settings_then_shortcuts() {
        let rows = visible_help_rows(100, true);
        assert!(matches!(rows.first(), Some(HelpRow::Header("Settings"))));
        assert!(matches!(rows.get(1), Some(HelpRow::Toggle)));
        assert!(matches!(rows.get(2), Some(HelpRow::Gap)));
        assert!(matches!(rows.get(3), Some(HelpRow::Header("Help"))));
        assert!(matches!(
            rows.get(4),
            Some(HelpRow::Shortcut("Ctrl+/", "Show this list"))
        ));
        assert!(matches!(
            visible_help_rows(1, true).as_slice(),
            [HelpRow::Toggle]
        ));
        assert!(
            !shortcut_rows(false)
                .iter()
                .any(|(keys, _)| *keys == "Click Changes" || *keys == "Click Diff")
        );
    }

    #[test]
    fn splits_hover_signature_docs_and_tags() {
        let parsed = parse_hover_content(
            "```typescript\ninterface User {\n  name: string\n  address: {\n    city: string\n  }\n}\n```\nA user record.\n@param name display name",
        );
        assert!(parsed.iter().any(|(line, kind)| {
            line == "interface User {" && *kind == HoverLineKind::Signature
        }));
        assert!(
            parsed.iter().any(|(line, kind)| {
                line == "  address: {" && *kind == HoverLineKind::Signature
            })
        );
        assert!(parsed.iter().any(|(line, kind)| {
            line == "    city: string" && *kind == HoverLineKind::Signature
        }));
        assert!(
            parsed
                .iter()
                .any(|(line, kind)| { line == "A user record." && *kind == HoverLineKind::Doc })
        );
        assert!(
            parsed
                .iter()
                .any(|(line, kind)| { line.starts_with("@param") && *kind == HoverLineKind::Tag })
        );
    }

    #[test]
    fn wrapped_signature_keeps_leading_indent() {
        let rows = wrap_to_width("    city: VeryLongCityName", 16);
        assert!(rows.len() > 1);
        assert!(rows[0].starts_with("    "));
        for row in &rows {
            assert!(row.starts_with("    "), "{row}");
            assert!(UnicodeWidthStr::width(row.as_str()) <= 16);
        }
    }
}
