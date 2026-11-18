pub(crate) mod context_menu;
mod editor_view;
mod explorer;
mod git;
pub(crate) mod layout;
mod popup;
mod status_bar;
pub(crate) mod theme;
mod thread;

use ratatui::Frame;

use crate::app::App;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let areas = layout::layout(frame.area(), app);
    app.search_area = (
        areas.search.x,
        areas.search.y,
        areas.search.width,
        areas.search.height,
    );
    app.explorer_area = (
        areas.explorer.x,
        areas.explorer.y,
        areas.explorer.width,
        areas.explorer.height,
    );
    app.editor_area = (
        areas.editor.x,
        areas.editor.y,
        areas.editor.width,
        areas.editor.height,
    );
    app.tabs_area = (
        areas.tabs.x,
        areas.tabs.y,
        areas.tabs.width,
        areas.tabs.height,
    );
    app.status_area = (
        areas.status.x,
        areas.status.y,
        areas.status.width,
        areas.status.height,
    );
    app.thread_area = (
        areas.threads.x,
        areas.threads.y,
        areas.threads.width,
        areas.threads.height,
    );
    app.side_tabs.clear();
    app.content_tabs.clear();
    if app.explorer_visible {
        explorer::render_search(frame, areas.search, app);
        if app.git_open {
            git::render(frame, areas.explorer, app);
        } else {
            explorer::render(frame, areas.explorer, app);
        }
    }
    editor_view::render(frame, areas.tabs, areas.editor, app);
    if app.thread_visible {
        thread::render(frame, areas.threads, app);
    }
    status_bar::render(frame, areas.status, app);
    app.note_line_blame();
    popup::render(frame, app);
    context_menu::render(frame, app);
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use tokio::sync::mpsc;

    use crate::app::state::{App, Focus, Overlay};
    use crate::language::LanguageClient;
    use crate::workspace::{ConflictKind, GitFileStatus, GitPathStatus};

    use super::draw;

    #[test]
    fn focused_thread_pane_uses_the_accent_border() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut app = App::new(PathBuf::from("."), LanguageClient { tx });
        app.thread_visible = true;
        app.focus = Focus::Thread;
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let (x, y, _, _) = app.thread_area;
        let accent = Color::Rgb(110, 186, 196);
        assert_eq!(terminal.backend().buffer()[(x, y)].fg, accent);
        assert!(
            buffer_text(terminal.backend().buffer()).contains("Select code and press Option+T")
        );
        app.focus = Focus::Editor;
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let (x, y, _, _) = app.thread_area;
        assert_ne!(terminal.backend().buffer()[(x, y)].fg, accent);
    }

    #[test]
    fn explorer_header_fills_the_row_after_hide() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut app = App::new(PathBuf::from("."), LanguageClient { tx });
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let bg = Color::Rgb(30, 30, 30);
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 0)].bg, bg);
        assert_eq!(buffer[(22, 0)].symbol(), " ");
        assert_eq!(buffer[(22, 0)].bg, bg);
        assert_eq!(buffer[(27, 0)].bg, bg);
    }

    #[test]
    fn pane_mark_stays_when_the_explorer_is_hidden() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut app = App::new(PathBuf::from("."), LanguageClient { tx });
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let (x, y, _) = app.pane_mark.expect("pane mark");
        assert!(x > 0);
        assert_eq!(terminal.backend().buffer()[(x, y)].symbol(), "◧");
        app.explorer_visible = false;
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let (x, y, _) = app.pane_mark.expect("pane mark");
        assert_eq!((x, y), (0, 0));
        assert_eq!(terminal.backend().buffer()[(x, y)].symbol(), "◧");
    }

    #[test]
    fn types_note_shows_the_install_command() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut app = App::new(PathBuf::from("."), LanguageClient { tx });
        app.overlay = Overlay::TypesNote;
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Editing works without it."));
        assert!(text.contains("npm install -g typescript typescript-language-server"));
        assert!(text.contains("typescript-language-server on PATH."));
        assert!(text.contains("Click ⊘ types off to check again"));
        assert!(text.contains("Copy"));
        let (x, y, width) = app.types_copy.expect("copy button");
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(x + 1, y)].symbol(), "C");
        assert!(width >= 4);
        crate::app::commands::click_types_note(&mut app, x, y);
        assert_eq!(app.message.as_deref(), Some("Copied"));
        assert!(matches!(app.overlay, Overlay::TypesNote));
    }

    #[test]
    fn explorer_border_puts_changes_on_the_right() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut app = App::new(PathBuf::from("."), LanguageClient { tx });
        app.git_repo = true;
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let y = app.explorer_area.1;
        let row = (0..app.explorer_area.2)
            .map(|col| {
                terminal.backend().buffer()[(app.explorer_area.0 + col, y)]
                    .symbol()
                    .to_string()
            })
            .collect::<String>();
        let explorer = row.find("Explorer").expect("explorer title");
        let changes = row.find("Changes").expect("changes title");
        assert!(explorer < changes);
        assert_eq!(app.side_tabs.len(), 2);
        assert!(!app.side_tabs[0].changes);
        assert!(app.side_tabs[1].changes);
        assert!(app.side_tabs[0].x < app.side_tabs[1].x);
        let accent = Color::Rgb(110, 186, 196);
        let buffer = terminal.backend().buffer();
        let explorer_cell = &buffer[(app.side_tabs[0].x + 1, y)];
        assert_eq!(explorer_cell.symbol(), "E");
        assert_eq!(explorer_cell.fg, accent);
        let changes_cell = &buffer[(app.side_tabs[1].x + 1, y)];
        assert_eq!(changes_cell.symbol(), "C");
        assert_ne!(changes_cell.fg, accent);
    }

    #[test]
    fn a_plain_folder_has_no_git_controls() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut app = App::new(PathBuf::from("."), LanguageClient { tx });
        app.git_branch = Some("topic".into());
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let explorer = (0..app.explorer_area.2)
            .map(|col| {
                terminal.backend().buffer()[(app.explorer_area.0 + col, app.explorer_area.1)]
                    .symbol()
                    .to_string()
            })
            .collect::<String>();
        let editor = (0..app.editor_area.2)
            .map(|col| {
                terminal.backend().buffer()[(app.editor_area.0 + col, app.editor_area.1)]
                    .symbol()
                    .to_string()
            })
            .collect::<String>();
        let status = (0..app.status_area.2)
            .map(|col| {
                terminal.backend().buffer()[(app.status_area.0 + col, app.status_area.1)]
                    .symbol()
                    .to_string()
            })
            .collect::<String>();
        assert!(explorer.contains("Explorer"));
        assert!(!explorer.contains("Changes"));
        assert!(editor.contains("Editor"));
        assert!(!editor.contains("Diff"));
        assert!(!status.contains("topic"));
        assert_eq!(app.side_tabs.len(), 1);
        assert!(app.content_tabs.is_empty());
    }

    #[test]
    fn editor_border_has_no_diff_tab() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut app = App::new(PathBuf::from("."), LanguageClient { tx });
        app.git_repo = true;
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let y = app.editor_area.1;
        let symbols: Vec<String> = (0..app.editor_area.2)
            .map(|col| {
                terminal.backend().buffer()[(app.editor_area.0 + col, y)]
                    .symbol()
                    .to_string()
            })
            .collect();
        let row = symbols.concat();
        let editor = symbols
            .windows(6)
            .position(|cells| cells.concat() == "Editor")
            .expect("file title");
        assert!(!row.contains("Diff"));
        assert!(app.content_tabs.is_empty());
        let accent = Color::Rgb(110, 186, 196);
        let buffer = terminal.backend().buffer();
        let editor_cell = &buffer[(app.editor_area.0 + editor as u16, y)];
        assert_eq!(editor_cell.symbol(), "E");
        assert_eq!(editor_cell.fg, accent);
        let status = (0..app.status_area.2)
            .map(|col| {
                buffer[(app.status_area.0 + col, app.status_area.1)]
                    .symbol()
                    .to_string()
            })
            .collect::<String>();
        assert!(!status.contains("Diff"));
    }

    #[test]
    fn changed_lines_show_stage_and_discard() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut app = App::new(PathBuf::from("."), LanguageClient { tx });
        app.git_repo = true;
        let id = crate::editor::DocumentId(app.next_id);
        app.next_id += 1;
        app.documents.insert(
            id,
            crate::editor::Document::new(
                id,
                Some(PathBuf::from("/repo/src/main.rs")),
                "fn changed() {}\n".into(),
            ),
        );
        app.views.insert(id, crate::editor::EditorView::new(id));
        app.order.push(id);
        app.active = Some(id);
        app.focus = Focus::Editor;
        app.diff_baselines.insert(
            id,
            crate::workspace::FileBaselines {
                head: crate::workspace::DiffBaseline::Present("fn main() {}\n".into()),
                index: crate::workspace::DiffBaseline::Present("fn main() {}\n".into()),
            },
        );
        app.recompute_inline(id);
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("▾"));
        assert!(text.contains("Stage"));
        assert!(text.contains("Discard"));
        assert!(text.contains("changed"));
        assert!(text.contains("main"));
        assert!(text.contains("~"));
        assert!(!text.contains("Unstaged"));
        assert!(!text.contains("Staged"));
        assert!(!text.contains("Diff"));
        assert!(
            app.diff_hits
                .iter()
                .any(|hit| hit.action == crate::app::state::HunkAction::Fold)
        );
    }

    #[test]
    fn conflict_bar_hides_markers_and_offers_both_sides() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut app = App::new(PathBuf::from("/repo"), LanguageClient { tx });
        app.git_repo = true;
        let path = PathBuf::from("/repo/src/user.ts");
        app.git_status.insert(
            path.clone(),
            GitPathStatus {
                staged: None,
                unstaged: None,
                conflict: Some(ConflictKind::Both),
            },
        );
        let id = crate::editor::DocumentId(app.next_id);
        app.next_id += 1;
        app.documents.insert(
            id,
            crate::editor::Document::new(
                id,
                Some(path),
                "export function greet() {\n<<<<<<< HEAD\n  return \"Hello\";\n=======\n  return \"Hi\";\n>>>>>>> topic\n}\n".into(),
            ),
        );
        app.views.insert(id, crate::editor::EditorView::new(id));
        app.order.push(id);
        app.active = Some(id);
        app.focus = Focus::Editor;
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Current"));
        assert!(text.contains("Incoming"));
        assert!(text.contains("Both"));
        assert!(text.contains("│"));
        assert!(text.contains("return \"Hello\""));
        assert!(text.contains("return \"Hi\""));
        assert!(!text.contains("<<<<<<<"));
        assert!(!text.contains("======="));
        assert!(!text.contains(">>>>>>>"));
        assert!(!text.contains("|||||||"));
        assert!(
            app.diff_hits
                .iter()
                .any(|hit| { hit.action == crate::app::state::HunkAction::Current })
        );
        app.git_open = true;
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let panel = buffer_text(terminal.backend().buffer());
        assert!(panel.contains("Conflicts"));
        assert!(
            !app.git_hits
                .iter()
                .any(|hit| matches!(hit.action, crate::app::state::GitClick::Toggle { .. }))
        );
    }

    #[test]
    fn git_panel_lists_staged_changes_and_commit() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut app = App::new(PathBuf::from("/repo"), LanguageClient { tx });
        app.git_repo = true;
        app.git_open = true;
        app.git_branch = Some("main".into());
        app.git_ahead_behind = Some((2, 1));
        app.git_message = "Keep the review".into();
        app.git_status.insert(
            PathBuf::from("/repo/src/main.rs"),
            GitPathStatus {
                staged: Some(GitFileStatus::Modified),
                unstaged: Some(GitFileStatus::Modified),
                conflict: None,
            },
        );
        app.git_status.insert(
            PathBuf::from("/repo/notes.txt"),
            GitPathStatus {
                staged: None,
                unstaged: Some(GitFileStatus::Untracked),
                conflict: None,
            },
        );
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Staged"));
        assert!(text.contains("Changes"));
        assert!(text.contains("Commit"));
        assert!(text.contains("Pull"));
        assert!(text.contains("Push"));
        assert!(text.contains("main ↑2 ↓1"));
        assert!(text.contains("main.rs"));
        assert!(text.contains("notes.txt"));
        assert!(text.contains("Keep the review"));
        let first = row_text(terminal.backend().buffer(), app.explorer_area.1 + 1);
        assert!(first.contains("Staged"));
        assert!(first.contains("─"));
        assert!(!first.contains("Pull"));
        let pull = hit_for(&app, |action| {
            matches!(action, crate::app::state::GitClick::Pull)
        });
        let push = hit_for(&app, |action| {
            matches!(action, crate::app::state::GitClick::Push)
        });
        let commit = hit_for(&app, |action| {
            matches!(action, crate::app::state::GitClick::Commit)
        });
        let message = hit_for(&app, |action| {
            matches!(action, crate::app::state::GitClick::Message)
        });
        let last_inner = app.explorer_area.1 + app.explorer_area.3 - 2;
        assert_eq!(commit.y, last_inner - 2);
        assert_eq!(pull.y, last_inner - 4);
        assert_eq!(pull.y, push.y);
        assert_eq!(commit.y, message.y);
        assert!(pull.x < push.x);
        assert!(message.x < commit.x);
        let buffer = terminal.backend().buffer();
        let sync = row_text(buffer, pull.y);
        assert!(sync.contains("↓1"));
        assert!(sync.contains("↑2"));
        assert!(sync.contains("Pull"));
        assert!(sync.contains("Push"));
        assert!(!sync.contains("Commit"));
        let between = row_text(buffer, pull.y + 1);
        assert!(between.contains("─"));
        assert!(!between.contains("Pull"));
        assert!(!between.contains("Commit"));
        assert_eq!(commit.y, pull.y + 2);
        let pull_text = slice_text(buffer, pull.x, pull.y, pull.width);
        let push_text = slice_text(buffer, push.x, push.y, push.width);
        assert!(pull_text.contains("↓1"));
        assert!(pull_text.contains("Pull"));
        assert!(push_text.contains("↑2"));
        assert!(push_text.contains("Push"));
        assert!(push.x >= pull.x + pull.width);
        let theme = app.theme();
        assert_eq!(buffer[(pull.x, pull.y)].bg, theme.accent);
        assert_eq!(buffer[(push.x, push.y)].bg, theme.accent);
        assert_eq!(buffer[(message.x, message.y)].bg, theme.bg_status);
        assert_eq!(buffer[(commit.x, commit.y)].bg, theme.accent);
        assert_ne!(
            buffer[(message.x, message.y)].bg,
            buffer[(commit.x, commit.y)].bg
        );
        let commit_row = row_text(buffer, commit.y);
        assert!(commit_row.contains("Commit"));
        assert!(commit_row.contains("Keep the review"));
        assert!(!commit_row.contains("body (optional)"));
        let body = hit_for(&app, |action| {
            matches!(action, crate::app::state::GitClick::Body)
        });
        let fold = hit_for(&app, |action| {
            matches!(action, crate::app::state::GitClick::BodyFold)
        });
        assert_eq!(body.y, commit.y + 2);
        assert_eq!(body.y, last_inner);
        let body_gap = slice_text(
            buffer,
            app.explorer_area.0 + 1,
            commit.y + 1,
            app.explorer_area.2.saturating_sub(2),
        );
        assert!(body_gap.trim().is_empty());
        assert_eq!(fold.y, body.y);
        assert!(fold.x < body.x);
        let body_row = row_text(buffer, body.y);
        assert!(body_row.contains("▸"));
        assert!(body_row.contains("body (optional)"));
        assert_eq!(buffer[(body.x, body.y)].bg, theme.bg_status);
        let inner_right = app.explorer_area.0 + app.explorer_area.2 - 1;
        assert!(push.x + push.width <= inner_right);
        assert!(commit.x + commit.width <= inner_right);
        app.git_ahead_behind = Some((0, 0));
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let quiet = hit_for(&app, |action| {
            matches!(action, crate::app::state::GitClick::Pull)
        });
        assert_eq!(
            terminal.backend().buffer()[(quiet.x, quiet.y)].bg,
            theme.border
        );
        assert!(app.git_hits.iter().any(|hit| matches!(
            hit.action,
            crate::app::state::GitClick::Toggle { staged: true, .. }
        )));
        assert!(app.git_hits.iter().any(|hit| matches!(
            hit.action,
            crate::app::state::GitClick::Toggle { staged: false, .. }
        )));
        app.git_body =
            "Why it stays.\nThe review still holds.\nAnd one more line.\nPast the fold.".into();
        app.git_body_open = true;
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let commit = hit_for(&app, |action| {
            matches!(action, crate::app::state::GitClick::Commit)
        });
        let fold = hit_for(&app, |action| {
            matches!(action, crate::app::state::GitClick::BodyFold)
        });
        let body_hits: Vec<_> = app
            .git_hits
            .iter()
            .filter(|hit| matches!(hit.action, crate::app::state::GitClick::Body))
            .cloned()
            .collect();
        assert_eq!(body_hits.len(), 3);
        assert_eq!(body_hits[0].y, commit.y + 2);
        let open_gap = slice_text(
            buffer,
            app.explorer_area.0 + 1,
            commit.y + 1,
            app.explorer_area.2.saturating_sub(2),
        );
        assert!(open_gap.trim().is_empty());
        assert_eq!(body_hits[1].y, body_hits[0].y + 1);
        assert_eq!(body_hits[2].y, body_hits[1].y + 1);
        assert_eq!(body_hits[2].y, last_inner);
        assert_eq!(fold.y, body_hits[0].y);
        assert!(fold.x < body_hits[0].x);
        let first = row_text(buffer, body_hits[0].y);
        let second = row_text(buffer, body_hits[1].y);
        let third = row_text(buffer, body_hits[2].y);
        assert!(first.contains("▾"));
        assert!(first.contains("The review still holds."));
        assert!(!first.contains("Why it stays."));
        assert!(second.contains("And one more line."));
        assert!(third.contains("Past the fold."));
        assert!(!first.contains("body (optional)"));
        assert_eq!(buffer[(body_hits[0].x, body_hits[0].y)].bg, theme.bg_status);
        app.git_body_focus = true;
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let focused = hit_for(&app, |action| {
            matches!(action, crate::app::state::GitClick::Body)
        });
        assert_eq!(
            terminal.backend().buffer()[(focused.x, focused.y)].bg,
            theme.bg_selected
        );
    }

    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        let mut text = String::new();
        for row in 0..buffer.area.height {
            for col in 0..buffer.area.width {
                text.push_str(buffer[(col, row)].symbol());
            }
        }
        text
    }

    fn row_text(buffer: &ratatui::buffer::Buffer, y: u16) -> String {
        (0..buffer.area.width)
            .map(|col| buffer[(col, y)].symbol().to_string())
            .collect()
    }

    fn slice_text(buffer: &ratatui::buffer::Buffer, x: u16, y: u16, width: u16) -> String {
        (x..x.saturating_add(width))
            .map(|col| buffer[(col, y)].symbol().to_string())
            .collect()
    }

    fn hit_for(
        app: &App,
        pred: impl Fn(&crate::app::state::GitClick) -> bool,
    ) -> crate::app::state::GitHit {
        app.git_hits
            .iter()
            .find(|hit| pred(&hit.action))
            .expect("git hit")
            .clone()
    }
}
