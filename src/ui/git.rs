use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::app::App;
use crate::app::state::{GitClick, GitHit, git_footer_layout, max_git_scroll};
use crate::ui::theme::Theme;
use crate::workspace::{GitCommit, GitFileStatus, GitPanelRow, whole_file_action};

pub fn render(frame: &mut Frame, area: Rect, app: &mut App) {
    app.git_hits.clear();
    let theme = app.theme();
    let rows = app.git_rows();
    let inner_h = area.height.saturating_sub(2) as usize;
    let inner_w = area.width.saturating_sub(2) as usize;
    let body_open = app.git_body_open;
    let commits = app.git_commits.clone();
    app.clamp_git_log_scroll();
    let log_scroll = app.git_log_scroll;
    let layout = git_footer_layout(inner_h, body_open, commits.len());
    let list_h = inner_h.saturating_sub(layout.rows);
    keep_selection_visible(app, rows.len(), list_h);
    let ready = crate::workspace::can_commit(&app.git_message, &app.git_status);
    let sync_ready = app.git_repo && !app.git_busy();
    let ahead_behind = app.git_ahead_behind;
    let message = app.git_message.clone();
    let message_focus = app.git_message_focus;
    let body = app.git_body.clone();
    let body_focus = app.git_body_focus;
    let scroll = app.git_scroll;
    let origin_x = area.x.saturating_add(1);
    let mut lines = Vec::new();
    for row in rows.iter().skip(scroll).take(list_h) {
        let y = area.y.saturating_add(1).saturating_add(lines.len() as u16);
        lines.push(row_line(app, row, theme, inner_w, origin_x, y));
    }
    while lines.len() < list_h {
        lines.push(Line::from(""));
    }
    let mut footer_y = area.y.saturating_add(1).saturating_add(lines.len() as u16);
    if layout.sync {
        lines.push(sync_footer(
            app,
            theme,
            inner_w,
            origin_x,
            footer_y,
            ahead_behind,
            sync_ready,
        ));
        footer_y = footer_y.saturating_add(1);
    }
    if layout.rule {
        lines.push(footer_rule(theme, inner_w));
        footer_y = footer_y.saturating_add(1);
    }
    if layout.subject {
        lines.push(commit_footer(
            app,
            theme,
            inner_w,
            origin_x,
            footer_y,
            &message,
            message_focus,
            ready,
            body_open,
        ));
        footer_y = footer_y.saturating_add(1);
    }
    if layout.body_gap {
        lines.push(Line::from(""));
        footer_y = footer_y.saturating_add(1);
    }
    let body_rows = body_lines(
        app,
        theme,
        inner_w,
        origin_x,
        footer_y,
        &body,
        body_focus,
        layout.body_lines,
    );
    footer_y = footer_y.saturating_add(body_rows.len() as u16);
    lines.extend(body_rows);
    lines.extend(log_lines(
        app,
        theme,
        inner_w,
        origin_x,
        footer_y,
        &commits,
        log_scroll,
        layout.log_lines,
    ));
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme.text())
            .block(super::explorer::side_block(area, app)),
        area,
    );
}

fn keep_selection_visible(app: &mut App, len: usize, inner: usize) {
    if len == 0 {
        app.git_scroll = 0;
        return;
    }
    if app.git_selected >= len {
        app.git_selected = len - 1;
    }
    let max = max_git_scroll(len, inner);
    if app.git_scroll > max {
        app.git_scroll = max;
    }
}

fn row_line(
    app: &mut App,
    row: &GitPanelRow,
    theme: Theme,
    inner_w: usize,
    x: u16,
    y: u16,
) -> Line<'static> {
    let paint = |color: Color| Style::default().fg(color);
    match row {
        GitPanelRow::Conflicts => {
            let title = "Conflicts ";
            let head_w = UnicodeWidthStr::width(title);
            if head_w >= inner_w {
                return Line::from(Span::styled(
                    fit("Conflicts", inner_w),
                    paint(theme.deleted),
                ));
            }
            let rule = "─".repeat(inner_w - head_w);
            let rule_style = Style::default().fg(theme.deleted);
            Line::from(vec![
                Span::styled(title.to_string(), paint(theme.deleted)),
                Span::styled(rule, rule_style),
            ])
        }
        GitPanelRow::Section { staged } => {
            let (mark, title, action) = if *staged {
                ("−", "Staged", GitClick::UnstageAll)
            } else {
                ("+", "Changes", GitClick::StageAll)
            };
            let mark_w = UnicodeWidthStr::width(mark);
            hit(app, x, y, mark_w as u16, action);
            let head = format!("{mark} {title} ");
            let head_w = UnicodeWidthStr::width(head.as_str());
            if head_w >= inner_w {
                return Line::from(Span::styled(
                    fit(&format!("{mark} {title}"), inner_w),
                    paint(theme.fg),
                ));
            }
            let rule = "─".repeat(inner_w - head_w);
            let rule_style = Style::default().fg(theme.border);
            Line::from(vec![
                Span::styled(mark.to_string(), paint(theme.accent)),
                Span::styled(format!(" {title} "), paint(theme.fg)),
                Span::styled(rule, rule_style),
            ])
        }
        GitPanelRow::Dir {
            path,
            depth,
            expanded,
            deleted,
            staged,
            status,
        } => dir_line(
            app, path, *depth, *expanded, *deleted, *staged, *status, theme, inner_w, x, y,
        ),
        GitPanelRow::File {
            path,
            status,
            staged,
            depth: _,
        } if *status == GitFileStatus::Conflict => {
            let name = app.relative(path);
            let mark = format!(" {}", letter(*status));
            let mark_w = UnicodeWidthStr::width(mark.as_str());
            let name = fit(&name, inner_w.saturating_sub(mark_w));
            let open_w = inner_w as u16;
            if open_w > 0 {
                let action = if app.git_status.get(path).is_some_and(|item| {
                    item.conflict == Some(crate::workspace::ConflictKind::Deleted)
                }) {
                    GitClick::DeleteConflict { path: path.clone() }
                } else {
                    GitClick::Open {
                        path: path.clone(),
                        staged: false,
                    }
                };
                hit(app, x, y, open_w, action);
            }
            let color = super::explorer::git_mark_color(*status, false, theme);
            let name_style = paint(theme.deleted);
            Line::from(vec![
                Span::styled(name, name_style),
                Span::styled(mark, Style::default().fg(color)),
            ])
        }
        GitPanelRow::File {
            path,
            status,
            staged,
            depth,
        } => {
            let indent = " ".repeat(depth.saturating_mul(2));
            let name_style = paint(theme.file);
            let discard = file_discardable(app, path);
            let open = GitClick::Open {
                path: path.clone(),
                staged: *staged,
            };
            entry_line(
                app,
                path,
                &indent,
                None,
                &entry_name(path),
                &format!(" {}", letter(*status)),
                super::explorer::git_mark_color(*status, *staged, theme),
                discard,
                Some(*staged),
                row_hovered(app, path, *staged),
                theme,
                name_style,
                inner_w,
                x,
                y,
                Some(open),
            )
        }
    }
}

fn dir_line(
    app: &mut App,
    path: &std::path::Path,
    depth: usize,
    expanded: bool,
    deleted: bool,
    staged: bool,
    status: GitFileStatus,
    theme: Theme,
    inner_w: usize,
    x: u16,
    y: u16,
) -> Line<'static> {
    let indent = " ".repeat(depth.saturating_mul(2));
    let name_style = Style::default().fg(theme.folder);
    let color = super::explorer::git_mark_color(status, staged, theme);
    let mark = format!(" {}", letter(status));
    let marker = if expanded { "▾ " } else { "▸ " };
    let discard = deleted || folder_discardable(app, path);
    let fold = GitClick::Fold {
        path: path.to_path_buf(),
        staged,
    };
    entry_line(
        app,
        path,
        &indent,
        Some(marker),
        &entry_name(path),
        &mark,
        color,
        discard,
        deleted.then_some(staged),
        row_hovered(app, path, staged),
        theme,
        name_style,
        inner_w,
        x,
        y,
        Some(fold),
    )
}

fn row_hovered(app: &App, path: &std::path::Path, staged: bool) -> bool {
    app.git_hover
        .as_ref()
        .is_some_and(|(hover, hover_staged)| hover == path && *hover_staged == staged)
}

fn file_discardable(app: &App, path: &std::path::Path) -> bool {
    app.git_status
        .get(path)
        .copied()
        .is_some_and(status_discardable)
}

fn folder_discardable(app: &App, dir: &std::path::Path) -> bool {
    app.git_status.iter().any(|(path, status)| {
        path.starts_with(dir) && path.as_path() != dir && status_discardable(*status)
    })
}

fn status_discardable(status: crate::workspace::GitPathStatus) -> bool {
    status.conflict.is_none() && whole_file_action(status).is_some()
}

fn entry_line(
    app: &mut App,
    path: &std::path::Path,
    indent: &str,
    marker: Option<&str>,
    name: &str,
    mark: &str,
    mark_color: Color,
    discard: bool,
    staged: Option<bool>,
    show_actions: bool,
    theme: Theme,
    name_style: Style,
    inner_w: usize,
    x: u16,
    y: u16,
    wide: Option<GitClick>,
) -> Line<'static> {
    let indent_w = UnicodeWidthStr::width(indent);
    let marker_w = marker.map(UnicodeWidthStr::width).unwrap_or(0);
    let left_w = indent_w.saturating_add(marker_w);
    let mark_w = UnicodeWidthStr::width(mark);
    let icon = "↩";
    let icon_w = UnicodeWidthStr::width(icon);
    let tick = staged.map(|staged| if staged { "−" } else { "+" });
    let tick_w = tick.map(UnicodeWidthStr::width).unwrap_or(0);
    let discard_w = if discard { icon_w.saturating_add(1) } else { 0 };
    let stage_w = if tick.is_some() {
        tick_w.saturating_add(1)
    } else {
        0
    };
    let right_w = mark_w.saturating_add(discard_w).saturating_add(stage_w);
    let name = fit(name, inner_w.saturating_sub(left_w.saturating_add(right_w)));
    let name_w = UnicodeWidthStr::width(name.as_str());
    let action_w = discard_w.saturating_add(stage_w);
    let gap = inner_w
        .saturating_sub(left_w)
        .saturating_sub(name_w)
        .saturating_sub(mark_w)
        .saturating_sub(action_w);
    let mut spans = vec![Span::styled(indent.to_string(), name_style)];
    if let Some(marker) = marker {
        spans.push(Span::styled(marker.to_string(), name_style));
    }
    spans.push(Span::styled(name, name_style));
    if gap > 0 {
        spans.push(Span::styled(" ".repeat(gap), name_style));
    }
    let mut cursor = left_w.saturating_add(name_w).saturating_add(gap);
    if show_actions {
        if discard {
            cursor = cursor.saturating_add(1);
            hit(
                app,
                x.saturating_add(cursor as u16),
                y,
                icon_w as u16,
                GitClick::Discard {
                    path: path.to_path_buf(),
                },
            );
            spans.push(Span::styled(" ".to_string(), name_style));
            spans.push(Span::styled(
                icon.to_string(),
                Style::default().fg(theme.deleted),
            ));
            cursor = cursor.saturating_add(icon_w);
        }
        if let Some(tick) = tick {
            cursor = cursor.saturating_add(1);
            hit(
                app,
                x.saturating_add(cursor as u16),
                y,
                tick_w as u16,
                GitClick::Toggle {
                    path: path.to_path_buf(),
                    staged: staged.unwrap_or(false),
                },
            );
            spans.push(Span::styled(" ".to_string(), name_style));
            spans.push(Span::styled(
                tick.to_string(),
                Style::default().fg(theme.accent),
            ));
        }
    } else if action_w > 0 {
        spans.push(Span::styled(" ".repeat(action_w), name_style));
    }
    spans.push(Span::styled(
        mark.to_string(),
        Style::default().fg(mark_color),
    ));
    if let Some(action) = wide {
        let before = left_w.saturating_add(name_w).saturating_add(gap);
        if show_actions && action_w > 0 {
            if before > 0 {
                hit(app, x, y, before.min(inner_w) as u16, action.clone());
            }
            let mark_at = before.saturating_add(action_w);
            if mark_w > 0 {
                hit(
                    app,
                    x.saturating_add(mark_at as u16),
                    y,
                    mark_w.min(inner_w.saturating_sub(mark_at)) as u16,
                    action,
                );
            }
        } else {
            let wide_w = before
                .saturating_add(action_w)
                .saturating_add(mark_w)
                .min(inner_w) as u16;
            if wide_w > 0 {
                hit(app, x, y, wide_w, action);
            }
        }
    }
    Line::from(spans)
}

fn entry_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.display().to_string())
}

fn footer_rule(theme: Theme, inner_w: usize) -> Line<'static> {
    Line::from(Span::styled(
        "─".repeat(inner_w),
        Style::default().fg(theme.border),
    ))
}

fn sync_footer(
    app: &mut App,
    theme: Theme,
    inner_w: usize,
    x: u16,
    y: u16,
    ahead_behind: Option<(u32, u32)>,
    sync_ready: bool,
) -> Line<'static> {
    if inner_w == 0 {
        return Line::from("");
    }
    let (ahead, behind) = match ahead_behind {
        Some((ahead, behind)) => (Some(ahead), Some(behind)),
        None => (None, None),
    };
    let gap = usize::from(inner_w > 1);
    let pull_w = inner_w.saturating_sub(gap) / 2;
    let push_w = inner_w.saturating_sub(gap).saturating_sub(pull_w);
    let mut spans = Vec::new();
    if pull_w > 0 {
        let label = match behind {
            Some(count) => format!("↓{count} Pull"),
            None => "Pull".to_string(),
        };
        place_button(
            app,
            &mut spans,
            x,
            y,
            0,
            pull_w,
            &label,
            GitClick::Pull,
            button_style(theme, sync_ready, behind.is_some_and(|count| count > 0)),
        );
    }
    if gap > 0 {
        spans.push(Span::raw(" "));
    }
    if push_w > 0 {
        let label = match ahead {
            Some(count) => format!("↑{count} Push"),
            None => "Push".to_string(),
        };
        place_button(
            app,
            &mut spans,
            x,
            y,
            pull_w + gap,
            push_w,
            &label,
            GitClick::Push,
            button_style(theme, sync_ready, ahead.is_some_and(|count| count > 0)),
        );
    }
    Line::from(spans)
}

fn commit_footer(
    app: &mut App,
    theme: Theme,
    inner_w: usize,
    x: u16,
    y: u16,
    message: &str,
    message_focus: bool,
    ready: bool,
    body_open: bool,
) -> Line<'static> {
    if inner_w == 0 {
        return Line::from("");
    }
    let mark = if body_open { "▾" } else { "▸" };
    let mark_w = UnicodeWidthStr::width(mark).min(inner_w);
    let rest = inner_w.saturating_sub(mark_w);
    let commit_w = UnicodeWidthStr::width("Commit").saturating_add(2).min(rest);
    let gap = usize::from(rest > commit_w);
    let input_w = rest.saturating_sub(commit_w).saturating_sub(gap);
    let mut spans = Vec::new();
    if mark_w > 0 {
        hit(app, x, y, mark_w as u16, GitClick::BodyFold);
        spans.push(Span::styled(
            mark.to_string(),
            Style::default().fg(theme.accent),
        ));
    }
    if input_w > 0 {
        let shown = if message.is_empty() {
            fill(" message", input_w)
        } else {
            let body = tail(message, input_w.saturating_sub(1));
            fill(&format!(" {body}"), input_w)
        };
        let style = if message_focus {
            Style::default()
                .fg(theme.fg)
                .bg(theme.bg_selected)
                .add_modifier(Modifier::BOLD)
        } else if message.is_empty() {
            Style::default().fg(theme.fg_dim).bg(theme.bg_status)
        } else {
            Style::default().fg(theme.fg).bg(theme.bg_status)
        };
        hit(
            app,
            x.saturating_add(mark_w as u16),
            y,
            input_w as u16,
            GitClick::Message,
        );
        spans.push(Span::styled(shown, style));
    }
    if gap > 0 {
        spans.push(Span::raw(" "));
    }
    if commit_w > 0 {
        place_button(
            app,
            &mut spans,
            x,
            y,
            mark_w + input_w + gap,
            commit_w,
            "Commit",
            GitClick::Commit,
            button_style(theme, true, ready),
        );
    }
    Line::from(spans)
}

fn body_lines(
    app: &mut App,
    theme: Theme,
    inner_w: usize,
    x: u16,
    y: u16,
    body: &str,
    focus: bool,
    rows: usize,
) -> Vec<Line<'static>> {
    if rows == 0 || inner_w == 0 {
        return Vec::new();
    }
    let visible = visible_body_lines(body, inner_w.saturating_sub(1), rows);
    let empty = body.is_empty();
    let field = body_field_style(theme, focus, empty);
    let mut out = Vec::with_capacity(rows);
    for (index, line) in visible.into_iter().enumerate() {
        let row_y = y.saturating_add(index as u16);
        let shown = if empty && index == 0 {
            fill(" body (optional)", inner_w)
        } else {
            fill(&format!(" {line}"), inner_w)
        };
        hit(app, x, row_y, inner_w as u16, GitClick::Body);
        out.push(Line::from(Span::styled(shown, field)));
    }
    out
}

fn log_lines(
    app: &mut App,
    theme: Theme,
    inner_w: usize,
    x: u16,
    y: u16,
    commits: &[GitCommit],
    scroll: usize,
    rows: usize,
) -> Vec<Line<'static>> {
    if rows == 0 || inner_w == 0 {
        return Vec::new();
    }
    let start = scroll.min(commits.len().saturating_sub(rows));
    let mut out = Vec::with_capacity(rows);
    for row in 0..rows {
        let Some(commit) = commits.get(start + row) else {
            break;
        };
        let row_y = y.saturating_add(row as u16);
        let last = start + row + 1 == commits.len();
        let prefix = if last { "└─ " } else { "├─ " };
        let prefix_shown = fit(prefix, inner_w);
        let prefix_w = UnicodeWidthStr::width(prefix_shown.as_str());
        let subject = fill(
            &fit(&commit.subject, inner_w - prefix_w),
            inner_w - prefix_w,
        );
        hit(
            app,
            x,
            row_y,
            inner_w as u16,
            GitClick::Log { index: start + row },
        );
        out.push(Line::from(vec![
            Span::styled(prefix_shown, Style::default().fg(theme.border)),
            Span::styled(subject, Style::default().fg(theme.fg)),
        ]));
    }
    out
}

fn body_field_style(theme: Theme, focus: bool, empty: bool) -> Style {
    if focus {
        Style::default()
            .fg(theme.fg)
            .bg(theme.bg_selected)
            .add_modifier(Modifier::BOLD)
    } else if empty {
        Style::default().fg(theme.fg_dim).bg(theme.bg_status)
    } else {
        Style::default().fg(theme.fg).bg(theme.bg_status)
    }
}

fn visible_body_lines(text: &str, width: usize, rows: usize) -> Vec<String> {
    if rows == 0 {
        return Vec::new();
    }
    if width == 0 {
        return vec![String::new(); rows];
    }
    let mut wrapped = wrap_body(text, width);
    if wrapped.len() > rows {
        let start = wrapped.len() - rows;
        wrapped.drain(0..start);
    }
    while wrapped.len() < rows {
        wrapped.push(String::new());
    }
    wrapped
}

fn wrap_body(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for para in text.split('\n') {
        if para.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut current = String::new();
        let mut current_w = 0usize;
        for grapheme in para.graphemes(true) {
            let grapheme_w = UnicodeWidthStr::width(grapheme);
            if grapheme_w > width {
                if !current.is_empty() {
                    lines.push(std::mem::take(&mut current));
                    current_w = 0;
                }
                continue;
            }
            if current_w + grapheme_w > width {
                lines.push(std::mem::take(&mut current));
                current_w = 0;
            }
            current.push_str(grapheme);
            current_w += grapheme_w;
        }
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn button_style(theme: Theme, enabled: bool, hot: bool) -> Style {
    if !enabled {
        return Style::default().fg(theme.fg_dim).bg(theme.bg_current);
    }
    if hot {
        Style::default()
            .fg(theme.on_accent)
            .bg(theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.fg).bg(theme.border)
    }
}

fn place_button(
    app: &mut App,
    spans: &mut Vec<Span<'static>>,
    x: u16,
    y: u16,
    cursor: usize,
    width: usize,
    label: &str,
    action: GitClick,
    style: Style,
) {
    if width == 0 {
        return;
    }
    hit(
        app,
        x.saturating_add(cursor as u16),
        y,
        width as u16,
        action,
    );
    spans.push(Span::styled(center(label, width), style));
}

fn center(text: &str, width: usize) -> String {
    let shown = fit(text, width);
    let extra = width.saturating_sub(UnicodeWidthStr::width(shown.as_str()));
    let left = extra / 2;
    let right = extra - left;
    format!("{}{shown}{}", " ".repeat(left), " ".repeat(right))
}

fn fill(text: &str, width: usize) -> String {
    let shown = fit(text, width);
    let extra = width.saturating_sub(UnicodeWidthStr::width(shown.as_str()));
    format!("{shown}{}", " ".repeat(extra))
}

fn hit(app: &mut App, x: u16, y: u16, width: u16, action: GitClick) {
    if width == 0 {
        return;
    }
    app.git_hits.push(GitHit {
        x,
        y,
        width,
        action,
    });
}

fn letter(status: GitFileStatus) -> &'static str {
    match status {
        GitFileStatus::Modified => "M",
        GitFileStatus::Added => "A",
        GitFileStatus::Deleted => "D",
        GitFileStatus::Renamed => "R",
        GitFileStatus::Untracked => "U",
        GitFileStatus::Conflict => "C",
    }
}

fn fit(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    let ellipsis = "…";
    let ellipsis_width = UnicodeWidthStr::width(ellipsis);
    if max_width <= ellipsis_width {
        return truncate(text, max_width);
    }
    format!("{}{ellipsis}", truncate(text, max_width - ellipsis_width))
}

fn tail(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    let graphemes: Vec<&str> = text.graphemes(true).collect();
    let mut width = 0;
    let mut start = graphemes.len();
    for grapheme in graphemes.iter().rev() {
        let next = width + UnicodeWidthStr::width(*grapheme);
        if next > max_width {
            break;
        }
        width = next;
        start -= 1;
    }
    graphemes[start..].concat()
}

fn truncate(text: &str, max_width: usize) -> String {
    let mut out = String::new();
    let mut width = 0;
    for grapheme in text.graphemes(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if width + grapheme_width > max_width {
            break;
        }
        out.push_str(grapheme);
        width += grapheme_width;
    }
    out
}
