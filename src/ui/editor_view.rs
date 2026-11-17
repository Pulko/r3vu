use ratatui::Frame;
use ratatui::layout::{Alignment, Position as TermPos, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::app::App;
use crate::app::state::{
    BlameHit, DiffHit, Focus, HScrollHit, HunkAction, SearchKind, SearchScope, TabHitBox,
};
use crate::editor::{
    Range, display_col, hscroll_bar, line_display_width, line_text, max_line_display_width,
};
use crate::syntax::HighlightKind;
use crate::ui::theme::{Theme, highlight_color, highlight_modifier};
use crate::workspace::{HunkBar, LineChanges, LineMark, VisualRow};

pub fn render(frame: &mut Frame, tabs: Rect, area: Rect, app: &mut App) {
    app.hscroll_hit = None;
    app.diff_hits.clear();
    app.blame_hit = None;
    app.restore_hit = None;
    let theme = app.theme();
    render_tabs(frame, tabs, app, theme);
    let Some(id) = app.active else {
        frame.render_widget(
            Paragraph::new("Open a file from the explorer or press Ctrl+P")
                .style(theme.text())
                .block(content_block(area, app, "Editor")),
            area,
        );
        return;
    };
    if app
        .markdown_preview
        .as_ref()
        .is_some_and(|preview| preview.document_id == id)
    {
        render_markdown(frame, area, app, theme);
        return;
    }
    let Some(doc) = app.documents.get(&id) else {
        return;
    };
    let Some(view) = app.views.get(&id) else {
        return;
    };
    let tab_width = app.config.tab_width;
    let number_width = ((doc.rope.len_lines().max(1).ilog10() as u16) + 2).max(4);
    let rows = app.visual_rows(id);
    let changes = app.inline_changes.get(&id).cloned();
    let hunks = app.present_hunks(id);
    let gutter = number_width + 1;
    app.gutter_width = gutter;
    let inner_h = area.height.saturating_sub(2) as usize;
    let inner_w = area.width.saturating_sub(2 + gutter) as usize;
    let visible_lines: Vec<usize> = rows
        .iter()
        .skip(view.viewport.top)
        .take(inner_h)
        .filter_map(|row| match row {
            VisualRow::Line(line) => Some(*line),
            VisualRow::Removed { .. } | VisualRow::Bar { .. } => None,
        })
        .collect();
    let highlight_start = visible_lines.first().copied().unwrap_or(0);
    let highlight_end = visible_lines.last().copied().unwrap_or(highlight_start);
    let highlights =
        app.syntax
            .highlights(id, doc.language, &doc.rope, highlight_start, highlight_end);
    let sel = view.selection.map(|s| s.range());
    let diag = doc
        .path
        .as_ref()
        .and_then(|p| app.diagnostics.get(p))
        .cloned()
        .unwrap_or_default();
    let search_matches = if let crate::app::state::Overlay::Search {
        kind: SearchKind::Content,
        scope: SearchScope::File,
        file_matches,
        selected,
        ..
    } = &app.overlay
    {
        Some((file_matches.clone(), *selected))
    } else {
        None
    };

    let mut lines = Vec::new();
    let mut bar_hits = Vec::new();
    for row in 0..inner_h.max(1) {
        let Some(visual) = rows.get(view.viewport.top + row).cloned() else {
            lines.push(Line::from(""));
            continue;
        };
        let y = area.y.saturating_add(1).saturating_add(lines.len() as u16);
        if let VisualRow::Bar { anchor } = visual {
            let bar = hunks
                .iter()
                .find(|hunk| hunk.start == anchor)
                .map(|hunk| hunk.bar)
                .unwrap_or(HunkBar::StageDiscard);
            let open = !app.folded_hunks.contains(&(id, anchor));
            let (line, hits) = hunk_bar(
                bar,
                anchor,
                open,
                theme,
                number_width,
                inner_w,
                area.x.saturating_add(1),
                y,
            );
            bar_hits.extend(hits);
            lines.push(line);
            continue;
        }
        if let VisualRow::Removed { text, .. } = &visual {
            lines.push(removed_line(
                text,
                view.viewport.left,
                inner_w,
                number_width,
                tab_width,
                theme,
            ));
            continue;
        }
        let VisualRow::Line(line_idx) = visual else {
            continue;
        };
        if line_idx >= doc.rope.len_lines() {
            lines.push(Line::from(""));
            continue;
        }
        let text = line_text(&doc.rope, line_idx);
        let wash = changes
            .as_ref()
            .and_then(|changes| line_wash(changes, line_idx, theme));
        let mut spans = line_number(line_idx, number_width, theme);
        spans.push(match changes.as_ref() {
            Some(changes) => gutter_span(changes, line_idx, theme),
            None => Span::raw(" "),
        });
        let current = view.cursor.line == line_idx;
        let blame = current
            .then(|| {
                doc.path.as_ref().and_then(|path| {
                    app.blame_cache
                        .get(&(path.clone(), line_idx, text.clone()))
                        .cloned()
                })
            })
            .flatten();
        let content_w = line_display_width(&text, tab_width)
            .saturating_sub(view.viewport.left)
            .min(inner_w);
        let suffix = blame
            .as_ref()
            .and_then(|blame| blame_suffix(blame, inner_w.saturating_sub(content_w)));
        let paint_w = suffix
            .as_ref()
            .map(|suffix| inner_w.saturating_sub(suffix.width))
            .unwrap_or(inner_w);
        spans.extend(paint_line(
            &text,
            line_idx,
            view.viewport.left,
            paint_w,
            tab_width,
            current,
            if current { None } else { wash },
            sel,
            &highlights,
            &diag,
            search_matches.as_ref(),
            theme,
        ));
        if let Some(suffix) = suffix {
            let style = Style::default().fg(theme.fg_dim).bg(theme.bg_current);
            spans.push(Span::styled(suffix.label, style));
            if suffix.arrow {
                spans.push(Span::styled(" ".to_string(), style));
                let arrow_x = area
                    .x
                    .saturating_add(1)
                    .saturating_add(gutter)
                    .saturating_add(inner_w as u16)
                    .saturating_sub(1);
                if let Some(blame) = blame {
                    app.blame_hit = Some(BlameHit {
                        x: arrow_x,
                        y,
                        width: 1,
                        author: blame.author,
                        summary: blame.summary,
                        hash: blame.hash,
                        time: blame.time,
                    });
                }
                spans.push(Span::styled(
                    "↗".to_string(),
                    Style::default().fg(theme.accent).bg(theme.bg_current),
                ));
            }
        }
        lines.push(Line::from(spans));
    }

    let deleted = app.absent.contains(&id);
    let title = doc
        .path
        .as_ref()
        .map(|p| app.relative(p))
        .unwrap_or_else(|| "untitled".into());
    let title = if deleted {
        format!("Deleted {title}")
    } else {
        title
    };
    let cursor = app.views.get(&id).and_then(|view| {
        let text = line_text(&doc.rope, view.cursor.line);
        let col = display_col(&text, view.cursor.column, tab_width);
        let cursor_row = rows
            .iter()
            .position(|row| matches!(row, VisualRow::Line(line) if *line == view.cursor.line))?;
        (cursor_row >= view.viewport.top
            && cursor_row < view.viewport.top + inner_h
            && col >= view.viewport.left
            && col < view.viewport.left + inner_w)
            .then_some(TermPos {
                x: area.x + 1 + gutter + (col - view.viewport.left) as u16,
                y: area.y + 1 + (cursor_row - view.viewport.top) as u16,
            })
    });
    app.diff_hits.extend(bar_hits);
    let block = content_block(area, app, &title);
    let block = restore_title(block, area, app, deleted);
    frame.render_widget(Paragraph::new(lines).style(theme.text()).block(block), area);
    if let Some(cursor) = cursor
        && app.focus == Focus::Editor
        && matches!(
            app.overlay,
            crate::app::state::Overlay::None
                | crate::app::state::Overlay::Completion { .. }
                | crate::app::state::Overlay::Hover { .. }
                | crate::app::state::Overlay::Signature { .. }
        )
    {
        frame.set_cursor_position(cursor);
    }
    paint_hscroll(frame, area, app, theme, inner_w);
}

fn paint_hscroll(frame: &mut Frame, area: Rect, app: &mut App, theme: Theme, inner_w: usize) {
    if area.height < 3 || inner_w == 0 {
        return;
    }
    let Some(id) = app.active else { return };
    let tab_width = app.config.tab_width;
    let gutter = app.gutter_width;
    let (content, left) = {
        let Some(doc) = app.documents.get(&id) else {
            return;
        };
        let Some(view) = app.views.get(&id) else {
            return;
        };
        (
            max_line_display_width(&doc.rope, tab_width),
            view.viewport.left,
        )
    };
    let track_x = area.x.saturating_add(1).saturating_add(gutter);
    let Some(bar) = hscroll_bar(track_x, inner_w as u16, inner_w, content, left) else {
        return;
    };
    let y = area.y + area.height - 1;
    app.hscroll_hit = Some(HScrollHit {
        x: bar.track_x,
        y,
        width: bar.track_w,
        thumb_x: bar.thumb_x,
        thumb_w: bar.thumb_w,
        max_left: bar.max_left,
    });
    let thumb_start = bar.thumb_x.saturating_sub(bar.track_x);
    let thumb_end = thumb_start.saturating_add(bar.thumb_w);
    let mut spans = Vec::with_capacity(bar.track_w as usize);
    for i in 0..bar.track_w {
        let on_thumb = i >= thumb_start && i < thumb_end;
        let (glyph, style) = if on_thumb {
            ("─", Style::default().fg(theme.accent).bg(theme.bg))
        } else {
            ("─", Style::default().fg(theme.border).bg(theme.bg))
        };
        spans.push(Span::styled(glyph, style));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect {
            x: bar.track_x,
            y,
            width: bar.track_w,
            height: 1,
        },
    );
}

fn line_number(line_idx: usize, number_width: u16, theme: Theme) -> Vec<Span<'static>> {
    let ln = format!("{:>width$}", line_idx + 1, width = number_width as usize);
    vec![Span::styled(ln, Style::default().fg(theme.fg_dim))]
}

fn removed_line(
    text: &str,
    left: usize,
    width: usize,
    number_width: u16,
    tab_width: usize,
    theme: Theme,
) -> Line<'static> {
    let body = scrolled_plain(text, left, width, tab_width);
    let shown = UnicodeWidthStr::width(body.as_str()).min(width);
    let pad = " ".repeat(width.saturating_sub(shown));
    let wash = Style::default().fg(theme.deleted).bg(theme.deleted_bg);
    Line::from(vec![
        Span::styled(
            " ".repeat(number_width as usize),
            Style::default().bg(theme.deleted_bg),
        ),
        Span::styled("-", wash),
        Span::styled(body, wash),
        Span::styled(pad, wash),
    ])
}

fn hunk_bar(
    bar: HunkBar,
    anchor: usize,
    open: bool,
    theme: Theme,
    number_width: u16,
    inner_w: usize,
    x: u16,
    y: u16,
) -> (Line<'static>, Vec<DiffHit>) {
    let icon = if open { "▾" } else { "▸" };
    let note = match bar {
        HunkBar::Renamed => "Renamed ",
        HunkBar::Revert => "Unsaved ",
        HunkBar::Unstage | HunkBar::StageUnstageDiscard | HunkBar::StageDiscard => "",
    };
    let actions: &[(&str, HunkAction)] = match bar {
        HunkBar::StageDiscard => &[
            (" Stage ", HunkAction::Stage),
            (" Discard ", HunkAction::Discard),
        ],
        HunkBar::Unstage => &[(" Unstage ", HunkAction::Unstage)],
        HunkBar::StageUnstageDiscard => &[
            (" Stage ", HunkAction::Stage),
            (" Unstage ", HunkAction::Unstage),
            (" Discard ", HunkAction::Discard),
        ],
        HunkBar::Revert => &[(" Revert ", HunkAction::Revert)],
        HunkBar::Renamed => &[],
    };
    let content_w = number_width as usize + 1 + inner_w;
    let fold = format!(" {icon} ");
    let fold_w = UnicodeWidthStr::width(fold.as_str());
    let note_w = UnicodeWidthStr::width(note);
    let actions_w: usize = actions
        .iter()
        .map(|(label, _)| UnicodeWidthStr::width(*label))
        .sum();
    let used = fold_w.saturating_add(note_w).saturating_add(actions_w);
    let gap = content_w.saturating_sub(used);
    let mut spans = vec![Span::styled(
        fit_width(&fold, fold_w.min(content_w)),
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    )];
    let mut hits = Vec::new();
    if fold_w > 0 && fold_w <= content_w {
        hits.push(DiffHit {
            x,
            y,
            width: fold_w as u16,
            anchor,
            action: HunkAction::Fold,
        });
    }
    if !note.is_empty() && fold_w < content_w {
        let note_style = if matches!(bar, HunkBar::Revert) {
            Style::default().fg(theme.modified)
        } else {
            Style::default().fg(theme.fg_dim)
        };
        spans.push(Span::styled(
            fit_width(note, content_w.saturating_sub(fold_w)),
            note_style,
        ));
    }
    if gap > 0 {
        spans.push(Span::raw(" ".repeat(gap)));
    }
    let mut cursor = x.saturating_add(content_w.saturating_sub(actions_w) as u16);
    for (label, action) in actions {
        let width = UnicodeWidthStr::width(*label) as u16;
        if cursor.saturating_add(width) > x.saturating_add(content_w as u16) {
            break;
        }
        hits.push(DiffHit {
            x: cursor,
            y,
            width,
            anchor,
            action: *action,
        });
        let action_style = match action {
            HunkAction::Discard => Style::default()
                .fg(theme.deleted)
                .add_modifier(Modifier::BOLD),
            HunkAction::Revert => Style::default()
                .fg(theme.modified)
                .add_modifier(Modifier::BOLD),
            HunkAction::Stage | HunkAction::Unstage => Style::default()
                .fg(theme.on_accent)
                .bg(theme.accent)
                .add_modifier(Modifier::BOLD),
            HunkAction::Fold => Style::default().fg(theme.accent),
        };
        spans.push(Span::styled((*label).to_string(), action_style));
        cursor = cursor.saturating_add(width);
    }
    (Line::from(spans), hits)
}

fn fit_width(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    let mut out = String::new();
    let mut width = 0usize;
    for grapheme in text.graphemes(true) {
        let next = UnicodeWidthStr::width(grapheme);
        if width + next > max_width {
            break;
        }
        out.push_str(grapheme);
        width += next;
    }
    out
}

fn scrolled_plain(text: &str, left: usize, width: usize, tab_width: usize) -> String {
    let mut display = 0usize;
    let mut visible = 0usize;
    let mut out = String::new();
    for g in text.graphemes(true) {
        let w = if g == "\t" {
            tab_width - (display % tab_width.max(1))
        } else {
            UnicodeWidthStr::width(g).max(1)
        };
        let start = display;
        display += w;
        if display <= left {
            continue;
        }
        if start >= left + width || visible >= width {
            break;
        }
        if g == "\t" {
            let skip = left.saturating_sub(start);
            let take = w.saturating_sub(skip).min(width - visible);
            out.push_str(&" ".repeat(take));
            visible += take;
        } else {
            out.push_str(g);
            visible += w;
        }
    }
    if out.is_empty() {
        out.push(' ');
    }
    out
}

fn gutter_span(changes: &LineChanges, line_idx: usize, theme: Theme) -> Span<'static> {
    match changes
        .marks
        .get(line_idx)
        .copied()
        .unwrap_or(LineMark::Unchanged)
    {
        LineMark::Added => Span::styled("+", Style::default().fg(theme.added)),
        LineMark::Modified => Span::styled("~", Style::default().fg(theme.modified)),
        LineMark::Unchanged => Span::raw(" "),
    }
}

fn line_wash(changes: &LineChanges, line_idx: usize, theme: Theme) -> Option<Color> {
    match changes
        .marks
        .get(line_idx)
        .copied()
        .unwrap_or(LineMark::Unchanged)
    {
        LineMark::Added => Some(theme.added_bg),
        LineMark::Modified => Some(theme.modified_bg),
        LineMark::Unchanged => None,
    }
}

fn restore_title(block: Block<'static>, area: Rect, app: &mut App, show: bool) -> Block<'static> {
    if !show {
        return block;
    }
    let label = " Restore ";
    let width = UnicodeWidthStr::width(label) as u16;
    if width.saturating_add(2) >= area.width {
        return block;
    }
    let x = area
        .x
        .saturating_add(area.width)
        .saturating_sub(width)
        .saturating_sub(1);
    let style = Style::default()
        .fg(app.theme().accent)
        .add_modifier(Modifier::BOLD);
    app.restore_hit = Some((x, area.y, width));
    block.title(Line::from(Span::styled(label, style)).alignment(Alignment::Right))
}

fn content_block(area: Rect, app: &mut App, title: &str) -> Block<'static> {
    let theme = app.theme();
    let focused = app.focus == Focus::Editor;
    app.content_tabs.clear();
    let room = area.width.saturating_sub(2) as usize;
    let label = fit_border_title(title, room);
    let border = if focused { theme.accent } else { theme.border };
    let title_style = if focused {
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.fg_muted)
    };
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .style(theme.text())
        .title(Line::from(Span::styled(label, title_style)).alignment(Alignment::Left))
}

fn fit_border_title(title: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    let padded = format!(" {title} ");
    if UnicodeWidthStr::width(padded.as_str()) <= max_width {
        return padded;
    }
    if max_width <= 2 {
        return title.graphemes(true).take(max_width).collect();
    }
    let inner = max_width - 2;
    let mut body = String::new();
    let mut width = 0usize;
    for grapheme in title.graphemes(true) {
        let next = UnicodeWidthStr::width(grapheme);
        if width + next + 1 > inner {
            break;
        }
        body.push_str(grapheme);
        width += next;
    }
    format!(" {body}… ")
}

fn render_markdown(frame: &mut Frame, area: Rect, app: &mut App, theme: Theme) {
    let width = area.width.saturating_sub(2) as usize;
    let height = area.height.saturating_sub(2) as usize;
    let title = app
        .markdown_preview
        .as_ref()
        .and_then(|preview| app.documents.get(&preview.document_id))
        .and_then(|doc| doc.path.as_ref().map(|path| app.relative(path)))
        .map(|name| format!("Preview {name}"))
        .unwrap_or_else(|| "Preview".into());
    let refresh = app.markdown_preview.as_ref().and_then(|preview| {
        let doc = app.documents.get(&preview.document_id)?;
        Some((
            doc.text(),
            doc.revision,
            preview.is_current(doc.revision, theme.name, width.max(1)),
        ))
    });
    if let Some((text, revision, false)) = refresh {
        let lines = crate::markdown::render(&text, theme, width.max(1), &mut app.syntax);
        if let Some(preview) = app.markdown_preview.as_mut() {
            preview.store(lines, revision, theme.name, width.max(1));
        }
    }
    let lines = app
        .markdown_preview
        .as_ref()
        .map(|preview| preview.visible(height.max(1)))
        .unwrap_or_default();
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme.text())
            .block(content_block(area, app, &title)),
        area,
    );
}

const TAB_CHEVRON: u16 = 2;
const PANE_MARK: &str = "◧";

struct TabPlacement {
    index: usize,
    x: u16,
    width: u16,
    label: String,
}

struct TabStrip {
    scroll: usize,
    left: Option<u16>,
    right: Option<u16>,
    tabs: Vec<TabPlacement>,
}

fn render_tabs(frame: &mut Frame, area: Rect, app: &mut App, theme: Theme) {
    app.tab_hit_boxes.clear();
    app.tab_scroll_left = None;
    app.tab_scroll_right = None;
    app.pane_mark = None;
    let opened: Vec<(crate::editor::DocumentId, String)> = app
        .order
        .iter()
        .filter_map(|id| {
            let doc = app.documents.get(id)?;
            Some((*id, tab_label(doc)))
        })
        .collect();
    let labels: Vec<String> = opened.iter().map(|(_, label)| label.clone()).collect();
    let active_index = app
        .active
        .and_then(|id| opened.iter().position(|(tab_id, _)| *tab_id == id));
    let reveal = app.tab_revealed != app.active;
    let mark_w = UnicodeWidthStr::width(PANE_MARK) as u16;
    let reserved = if area.width > mark_w {
        mark_w.saturating_add(1)
    } else {
        area.width
    };
    let mut strip = layout_tab_strip(
        &labels,
        app.tab_scroll,
        area.width.saturating_sub(reserved),
        active_index,
        reveal,
    );
    app.tab_scroll = strip.scroll;
    app.tab_revealed = app.active;
    if let Some(x) = strip.left.as_mut() {
        *x = x.saturating_add(reserved);
    }
    if let Some(x) = strip.right.as_mut() {
        *x = x.saturating_add(reserved);
    }
    for tab in &mut strip.tabs {
        tab.x = tab.x.saturating_add(reserved);
    }

    let mut spans = Vec::new();
    let mut cursor = 0u16;
    if mark_w > 0 && area.width >= mark_w {
        spans.push(Span::styled(
            PANE_MARK,
            Style::default().fg(theme.accent).bg(theme.bg),
        ));
        cursor = mark_w;
        app.pane_mark = Some((area.x, area.y, mark_w));
        if reserved > mark_w {
            spans.push(Span::styled(" ", Style::default().bg(theme.bg)));
            cursor = reserved;
        }
    }
    if let Some(x) = strip.left {
        push_gap(&mut spans, &mut cursor, x);
        spans.push(Span::styled("<<", Style::default().fg(theme.fg_dim)));
        cursor = cursor.saturating_add(TAB_CHEVRON);
        app.tab_scroll_left = Some((area.x.saturating_add(x), TAB_CHEVRON));
    }
    for placed in &strip.tabs {
        push_gap(&mut spans, &mut cursor, placed.x);
        let id = opened[placed.index].0;
        let active = app.active == Some(id);
        let bg = if active { theme.bg_current } else { theme.bg };
        let mut style = Style::default().fg(theme.fg_muted).bg(bg);
        if active {
            style = Style::default()
                .fg(theme.accent)
                .bg(bg)
                .add_modifier(Modifier::BOLD);
        }
        if let Some((left, right)) = placed.label.split_once('•') {
            spans.push(Span::styled(left.to_string(), style));
            spans.push(Span::styled("•", Style::default().fg(theme.warning).bg(bg)));
            spans.push(Span::styled(right.to_string(), style));
        } else {
            spans.push(Span::styled(placed.label.clone(), style));
        }
        if let Some(hit_box) = tab_hit_box(
            id,
            area.x.saturating_add(placed.x),
            placed.width,
            area.right(),
        ) {
            app.tab_hit_boxes.push(hit_box);
        }
        cursor = cursor.saturating_add(placed.width);
    }
    if let Some(x) = strip.right {
        push_gap(&mut spans, &mut cursor, x);
        spans.push(Span::styled(">>", Style::default().fg(theme.fg_dim)));
        app.tab_scroll_right = Some((area.x.saturating_add(x), TAB_CHEVRON));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)).style(theme.text()), area);
}

fn tab_label(doc: &crate::editor::Document) -> String {
    let name = doc
        .path
        .as_ref()
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "untitled".into());
    let dirty = if doc.is_dirty() { " •" } else { "" };
    format!(" {name}{dirty} ×")
}

fn layout_tab_strip(
    labels: &[String],
    mut scroll: usize,
    width: u16,
    active: Option<usize>,
    reveal: bool,
) -> TabStrip {
    if labels.is_empty() || width == 0 {
        return TabStrip {
            scroll: 0,
            left: None,
            right: None,
            tabs: Vec::new(),
        };
    }
    scroll = scroll.min(labels.len() - 1);
    if reveal {
        if let Some(active) = active.filter(|index| *index < labels.len()) {
            if active < scroll {
                scroll = active;
            }
            while scroll < active && !tab_is_visible(labels, scroll, width, active) {
                scroll += 1;
            }
        }
    }
    place_tabs(labels, scroll, width)
}

fn tab_is_visible(labels: &[String], scroll: usize, width: u16, active: usize) -> bool {
    place_tabs(labels, scroll, width)
        .tabs
        .iter()
        .any(|tab| tab.index == active)
}

fn place_tabs(labels: &[String], scroll: usize, width: u16) -> TabStrip {
    if labels.is_empty() || width == 0 {
        return TabStrip {
            scroll: 0,
            left: None,
            right: None,
            tabs: Vec::new(),
        };
    }
    let scroll = scroll.min(labels.len() - 1);
    let show_left = scroll > 0 && width >= TAB_CHEVRON;
    let left_w = if show_left { TAB_CHEVRON } else { 0 };
    let mut tabs = Vec::new();
    let mut x = left_w;
    let mut end = scroll;
    while end < labels.len() {
        let more_after = end + 1 < labels.len();
        let reserve = if more_after && width >= left_w.saturating_add(TAB_CHEVRON) {
            TAB_CHEVRON
        } else {
            0
        };
        let gap = if tabs.is_empty() { 0 } else { 1 };
        let room = width
            .saturating_sub(x)
            .saturating_sub(gap)
            .saturating_sub(reserve);
        if room == 0 {
            break;
        }
        let natural = display_width(&labels[end]) as u16;
        if natural > room && !tabs.is_empty() {
            break;
        }
        let label = fit_tab_label(&labels[end], room as usize);
        let tab_w = display_width(&label) as u16;
        if tab_w == 0 || tab_w > room {
            break;
        }
        x = x.saturating_add(gap);
        tabs.push(TabPlacement {
            index: end,
            x,
            width: tab_w,
            label,
        });
        x = x.saturating_add(tab_w);
        end += 1;
        if natural > room {
            break;
        }
    }
    let show_right = end < labels.len() && width >= left_w.saturating_add(TAB_CHEVRON);
    TabStrip {
        scroll,
        left: show_left.then_some(0),
        right: show_right.then_some(width - TAB_CHEVRON),
        tabs,
    }
}

fn fit_tab_label(label: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if display_width(label) <= max_width {
        return label.to_string();
    }
    let Some(stem) = label.strip_suffix(" ×") else {
        return truncate_width(label, max_width);
    };
    let close_w = display_width(" ×");
    if max_width <= close_w {
        return truncate_width(" ×", max_width);
    }
    let ellipsis = "…";
    let ellipsis_w = display_width(ellipsis);
    let stem_budget = max_width - close_w;
    if stem_budget <= ellipsis_w {
        return format!("{} ×", truncate_width(stem, stem_budget));
    }
    format!(
        "{}{ellipsis} ×",
        truncate_width(stem, stem_budget - ellipsis_w)
    )
}

fn push_gap(spans: &mut Vec<Span<'static>>, cursor: &mut u16, target: u16) {
    if target > *cursor {
        spans.push(Span::raw(" ".repeat((target - *cursor) as usize)));
        *cursor = target;
    }
}

fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

struct BlameSuffix {
    label: String,
    arrow: bool,
    width: usize,
}

fn blame_suffix(blame: &crate::workspace::LineBlame, free: usize) -> Option<BlameSuffix> {
    const MIN_GAP: usize = 2;
    if free < MIN_GAP + 1 {
        return None;
    }
    let name = if blame.uncommitted {
        "Uncommitted"
    } else {
        blame.author.as_str()
    };
    let mut budget = free - MIN_GAP;
    let arrow = !blame.uncommitted && budget >= 3;
    if arrow {
        budget -= 2;
    }
    let label = truncate_width(name, budget);
    if label.is_empty() {
        return None;
    }
    let width = display_width(&label) + usize::from(arrow) * 2;
    (width + MIN_GAP <= free).then_some(BlameSuffix {
        label,
        arrow,
        width,
    })
}

fn truncate_width(text: &str, max_width: usize) -> String {
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

fn tab_hit_box(
    document_id: crate::editor::DocumentId,
    x: u16,
    width: u16,
    right: u16,
) -> Option<TabHitBox> {
    (x.saturating_add(width) <= right).then_some(TabHitBox {
        document_id,
        x,
        width,
        close_x: x + width.saturating_sub(1),
    })
}

#[allow(clippy::too_many_arguments)]
fn paint_line(
    text: &str,
    line_idx: usize,
    left: usize,
    width: usize,
    tab_width: usize,
    current: bool,
    wash: Option<Color>,
    sel: Option<Range>,
    highlights: &[crate::syntax::HighlightSpan],
    diags: &[crate::language::Diagnostic],
    search: Option<&(
        Vec<(crate::editor::Position, crate::editor::Position)>,
        usize,
    )>,
    theme: Theme,
) -> Vec<Span<'static>> {
    let mut chars: Vec<(String, usize)> = Vec::new();
    let mut dcol = 0usize;
    let mut cidx = 0usize;
    for g in text.graphemes(true) {
        let w = if g == "\t" {
            tab_width - (dcol % tab_width.max(1))
        } else {
            UnicodeWidthStr::width(g).max(1)
        };
        chars.push((g.to_string(), w));
        dcol += w;
        cidx += g.chars().count();
        let _ = cidx;
    }
    let mut out = Vec::new();
    let mut display = 0usize;
    let mut col = 0usize;
    let mut painted = 0usize;
    let base_bg = if current {
        Some(theme.bg_current)
    } else {
        wash
    };
    for (g, w) in chars {
        let start_col = col;
        let end_col = col + g.chars().count();
        col = end_col;
        let start_disp = display;
        display += w;
        if display <= left {
            continue;
        }
        if start_disp >= left + width {
            break;
        }
        let mut style = Style::default();
        if let Some(bg) = base_bg {
            style = style.bg(bg);
        }
        if let Some(kind) = highlight_at(highlights, line_idx, start_col) {
            style = style
                .fg(highlight_color(kind, theme))
                .add_modifier(highlight_modifier(kind));
        }
        if in_range(sel, line_idx, start_col) {
            style = style.bg(theme.bg_selected);
        }
        if let Some((matches, current_match)) = search {
            for (i, (a, b)) in matches.iter().enumerate() {
                if a.line == line_idx && start_col >= a.column && start_col < b.column {
                    style = if i == *current_match {
                        style.bg(theme.search_current).fg(theme.on_accent)
                    } else {
                        style.bg(theme.search)
                    };
                }
            }
        }
        if let Some(diag) =
            crate::language::diagnostic_at(diags, crate::editor::Position::new(line_idx, start_col))
        {
            style = style
                .add_modifier(Modifier::UNDERLINED)
                .fg(diagnostic_color(diag.severity, theme));
        }
        let visible = if g == "\t" { " ".repeat(w) } else { g.clone() };
        painted += UnicodeWidthStr::width(visible.as_str());
        out.push(Span::styled(visible, style));
    }
    if painted < width {
        let mut style = Style::default();
        if let Some(bg) = base_bg {
            style = style.bg(bg);
        }
        let fill = if out.is_empty() { " " } else { "" };
        let extra = width - painted;
        if extra > 0 {
            out.push(Span::styled(
                format!("{fill}{}", " ".repeat(extra.saturating_sub(fill.len()))),
                style,
            ));
        }
    }
    if out.is_empty() {
        out.push(Span::styled(" ", Style::default()));
    }
    out
}

fn diagnostic_color(severity: u8, theme: Theme) -> Color {
    match severity {
        2 => theme.warning,
        3 | 4 => theme.fg_dim,
        _ => theme.deleted,
    }
}

fn highlight_at(
    highlights: &[crate::syntax::HighlightSpan],
    line: usize,
    col: usize,
) -> Option<HighlightKind> {
    highlights.iter().rev().find_map(|h| {
        let on_line = if h.start.line == h.end.line {
            h.start.line == line && col >= h.start.column && col < h.end.column
        } else {
            line >= h.start.line && line <= h.end.line
        };
        on_line.then_some(h.kind)
    })
}

fn in_range(sel: Option<Range>, line: usize, col: usize) -> bool {
    let Some(sel) = sel else { return false };
    let pos = crate::editor::Position::new(line, col);
    pos >= sel.start && pos < sel.end
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::DocumentId;

    #[test]
    fn tab_hit_box_separates_body_and_close() {
        let hit = tab_hit_box(DocumentId(7), 10, 12, 30).unwrap();
        assert_eq!(hit.x, 10);
        assert_eq!(hit.close_x, 21);
        assert!(15 >= hit.x && 15 < hit.x + hit.width);
        assert_ne!(15, hit.close_x);
        assert!(tab_hit_box(DocumentId(8), 22, 12, 30).is_none());
    }

    fn sample_labels(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| format!(" {name} ×")).collect()
    }

    fn assert_strip_inside(strip: &TabStrip, width: u16) {
        if let Some(x) = strip.left {
            assert!(x + TAB_CHEVRON <= width);
        }
        if let Some(x) = strip.right {
            assert!(x + TAB_CHEVRON <= width);
            for tab in &strip.tabs {
                assert!(tab.x + tab.width <= x);
            }
        }
        for tab in &strip.tabs {
            assert!(tab.x + tab.width <= width);
            assert_eq!(UnicodeWidthStr::width(tab.label.as_str()) as u16, tab.width);
            if strip.left.is_some() {
                assert!(tab.x >= TAB_CHEVRON);
            }
        }
    }

    #[test]
    fn tab_strip_hides_chevrons_when_everything_fits() {
        let strip = layout_tab_strip(&sample_labels(&["a", "b", "c"]), 0, 40, None, false);
        assert!(strip.left.is_none());
        assert!(strip.right.is_none());
        assert_eq!(strip.tabs.len(), 3);
        assert_strip_inside(&strip, 40);
    }

    #[test]
    fn tab_strip_shows_the_right_chevron_on_the_first_page() {
        let strip = layout_tab_strip(
            &sample_labels(&["abcd", "abcd", "abcd"]),
            0,
            12,
            None,
            false,
        );
        assert!(strip.left.is_none());
        assert!(strip.right.is_some());
        assert!(strip.tabs.len() < 3);
        assert_strip_inside(&strip, 12);
    }

    #[test]
    fn tab_strip_shows_both_chevrons_after_scrolling() {
        let strip = layout_tab_strip(
            &sample_labels(&["abcd", "abcd", "abcd"]),
            1,
            12,
            None,
            false,
        );
        assert!(strip.left.is_some());
        assert!(strip.right.is_some());
        assert_eq!(strip.scroll, 1);
        assert_strip_inside(&strip, 12);
    }

    #[test]
    fn tab_strip_shows_only_the_left_chevron_at_the_end() {
        let strip = layout_tab_strip(
            &sample_labels(&["abcd", "abcd", "abcd"]),
            2,
            12,
            None,
            false,
        );
        assert!(strip.left.is_some());
        assert!(strip.right.is_none());
        assert_eq!(strip.tabs.len(), 1);
        assert_eq!(strip.tabs[0].index, 2);
        assert_strip_inside(&strip, 12);
    }

    #[test]
    fn tab_strip_truncates_a_tab_wider_than_the_strip() {
        let strip = layout_tab_strip(
            &sample_labels(&["abcdefghijklmnopqrstuvwxyz"]),
            0,
            10,
            None,
            false,
        );
        assert_eq!(strip.tabs.len(), 1);
        assert!(strip.tabs[0].width <= 10);
        assert!(strip.tabs[0].label.ends_with('×'));
        assert!(strip.tabs[0].label.contains('…'));
        assert_strip_inside(&strip, 10);
    }

    #[test]
    fn changing_the_active_tab_reveals_it() {
        let labels = sample_labels(&["abcd", "abcd", "abcd", "abcd", "abcd"]);
        let hidden = layout_tab_strip(&labels, 0, 12, Some(4), false);
        assert!(hidden.tabs.iter().all(|tab| tab.index != 4));
        let revealed = layout_tab_strip(&labels, 0, 12, Some(4), true);
        assert!(revealed.tabs.iter().any(|tab| tab.index == 4));
        assert!(revealed.scroll > 0);
        assert_strip_inside(&revealed, 12);
    }

    fn sample_blame(uncommitted: bool) -> crate::workspace::LineBlame {
        crate::workspace::LineBlame {
            author: "Ada Lovelace".into(),
            summary: "Note".into(),
            time: 1,
            hash: "abcd".into(),
            uncommitted,
        }
    }

    #[test]
    fn blame_suffix_keeps_the_name_and_arrow_in_free_space() {
        let suffix = blame_suffix(&sample_blame(false), 16).unwrap();
        assert_eq!(suffix.label, "Ada Lovelace");
        assert!(suffix.arrow);
        assert_eq!(suffix.width, 14);
        assert!(blame_suffix(&sample_blame(false), 2).is_none());
    }

    #[test]
    fn uncommitted_blame_has_no_arrow() {
        let suffix = blame_suffix(&sample_blame(true), 40).unwrap();
        assert_eq!(suffix.label, "Uncommitted");
        assert!(!suffix.arrow);
    }
}
