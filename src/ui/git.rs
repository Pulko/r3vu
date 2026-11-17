use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::app::App;
use crate::app::state::{GitClick, GitHit, git_footer_layout};
use crate::ui::theme::Theme;
use crate::workspace::{GitFileStatus, GitPanelRow, git_panel_rows};

pub fn render(frame: &mut Frame, area: Rect, app: &mut App) {
    app.git_hits.clear();
    let theme = app.theme();
    let rows = git_panel_rows(&app.git_status);
    let inner_h = area.height.saturating_sub(2) as usize;
    let inner_w = area.width.saturating_sub(2) as usize;
    let body_open = app.git_body_open;
    let layout = git_footer_layout(inner_h, body_open);
    let list_h = inner_h.saturating_sub(layout.rows);
    keep_selection_visible(app, rows.len(), list_h);
    let ready = crate::workspace::can_commit(&app.git_message, &app.git_status);
    let sync_ready = app.git_repo && !app.git_busy();
    let ahead_behind = app.git_ahead_behind;
    let message = app.git_message.clone();
    let message_focus = app.git_message_focus;
    let body = app.git_body.clone();
    let body_focus = app.git_body_focus;
    let selected = app.git_selected;
    let scroll = app.git_scroll;
    let origin_x = area.x.saturating_add(1);
    let mut lines = Vec::new();
    for (index, row) in rows.iter().enumerate().skip(scroll).take(list_h) {
        let y = area.y.saturating_add(1).saturating_add(lines.len() as u16);
        let on = index == selected;
        lines.push(row_line(app, row, theme, inner_w, origin_x, y, on));
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
        ));
        footer_y = footer_y.saturating_add(1);
    }
    if layout.body_gap {
        lines.push(Line::from(""));
        footer_y = footer_y.saturating_add(1);
    }
    lines.extend(body_lines(
        app,
        theme,
        inner_w,
        origin_x,
        footer_y,
        &body,
        body_focus,
        body_open,
        layout.body_lines,
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
    if app.git_selected < app.git_scroll {
        app.git_scroll = app.git_selected;
    } else if inner > 0 && app.git_selected >= app.git_scroll + inner {
        app.git_scroll = app.git_selected + 1 - inner;
    }
    let max = len.saturating_sub(inner);
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
    selected: bool,
) -> Line<'static> {
    let selected_style = |color: Color| {
        if selected {
            Style::default()
                .fg(color)
                .bg(theme.bg_selected)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(color)
        }
    };
    match row {
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
                    selected_style(theme.fg),
                ));
            }
            let rule = "─".repeat(inner_w - head_w);
            let rule_style = if selected {
                selected_style(theme.fg_dim)
            } else {
                Style::default().fg(theme.border)
            };
            Line::from(vec![
                Span::styled(mark.to_string(), selected_style(theme.accent)),
                Span::styled(format!(" {title} "), selected_style(theme.fg)),
                Span::styled(rule, rule_style),
            ])
        }
        GitPanelRow::File {
            path,
            status,
            staged,
        } => {
            let tick = if *staged { "−" } else { "+" };
            let tick_w = UnicodeWidthStr::width(tick);
            hit(
                app,
                x,
                y,
                tick_w as u16,
                GitClick::Toggle {
                    path: path.clone(),
                    staged: *staged,
                },
            );
            let name = app.relative(path);
            let mark = format!(" {}", letter(*status));
            let mark_w = UnicodeWidthStr::width(mark.as_str());
            let name_room = inner_w.saturating_sub(tick_w.saturating_add(1).saturating_add(mark_w));
            let name = fit(&name, name_room);
            let open_x = x.saturating_add(tick_w as u16).saturating_add(1);
            let open_w = inner_w.saturating_sub(tick_w.saturating_add(1)) as u16;
            if open_w > 0 {
                hit(
                    app,
                    open_x,
                    y,
                    open_w,
                    GitClick::Open {
                        path: path.clone(),
                        staged: *staged,
                    },
                );
            }
            let color = super::explorer::git_mark_color(*status, *staged, theme);
            let name_style = selected_style(if selected { theme.fg } else { theme.fg_muted });
            Line::from(vec![
                Span::styled(tick.to_string(), Style::default().fg(theme.accent)),
                Span::styled(" ", name_style),
                Span::styled(name, name_style),
                Span::styled(mark, Style::default().fg(color)),
            ])
        }
    }
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
) -> Line<'static> {
    if inner_w == 0 {
        return Line::from("");
    }
    let commit_w = UnicodeWidthStr::width("Commit")
        .saturating_add(2)
        .min(inner_w);
    let gap = usize::from(inner_w > commit_w);
    let input_w = inner_w.saturating_sub(commit_w).saturating_sub(gap);
    let mut spans = Vec::new();
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
        hit(app, x, y, input_w as u16, GitClick::Message);
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
            input_w + gap,
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
    open: bool,
    rows: usize,
) -> Vec<Line<'static>> {
    if rows == 0 || inner_w == 0 {
        return Vec::new();
    }
    let mark = if open { "▾" } else { "▸" };
    let mark_w = UnicodeWidthStr::width(mark);
    let text_w = inner_w.saturating_sub(mark_w.saturating_add(1));
    let visible = visible_body_lines(body, text_w, rows, open);
    let empty = body.is_empty();
    let field = body_field_style(theme, focus, empty);
    let mark_style = field.fg(theme.accent);
    let mut out = Vec::with_capacity(rows);
    for (index, line) in visible.into_iter().enumerate() {
        let row_y = y.saturating_add(index as u16);
        let mut spans = Vec::new();
        let gap = usize::from(inner_w > mark_w);
        if index == 0 && mark_w > 0 {
            hit(app, x, row_y, mark_w as u16, GitClick::BodyFold);
            spans.push(Span::styled(mark.to_string(), mark_style));
        }
        let field_w = if index == 0 {
            inner_w.saturating_sub(mark_w)
        } else {
            inner_w
        };
        if field_w > 0 {
            let shown = if empty && index == 0 {
                fill("body (optional)", text_w)
            } else {
                fill(&line, text_w)
            };
            let indent = if index == 0 { 0 } else { mark_w };
            let content = format!("{}{}{}", " ".repeat(indent), " ".repeat(gap), shown);
            hit(
                app,
                x.saturating_add(if index == 0 { mark_w as u16 } else { 0 }),
                row_y,
                field_w as u16,
                GitClick::Body,
            );
            spans.push(Span::styled(content, field));
        }
        out.push(Line::from(spans));
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

fn visible_body_lines(text: &str, width: usize, rows: usize, open: bool) -> Vec<String> {
    if rows == 0 {
        return Vec::new();
    }
    if !open {
        let first = text.split('\n').next().unwrap_or("");
        return vec![first.to_string()];
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
