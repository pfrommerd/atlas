//! Ratatui rendering for the transcript, prompt, completions, and optional panels.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem, ListState, Paragraph};

use crate::app::{App, Focus, OutKind};

pub fn draw(f: &mut Frame, app: &mut App) {
    let completion_height = if app.dialogue.is_some() || app.completions.is_empty() {
        0
    } else {
        app.completions.len().min(8) as u16
    };
    let [tab_area, body_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(f.area());
    let available_height = body_area.height.saturating_sub(completion_height + 4);
    let [
        main_area,
        completion_area,
        session_name_area,
        top_rule_area,
        input_area,
        bottom_rule_area,
    ] = Layout::vertical([
        Constraint::Length(available_height),
        Constraint::Length(completion_height),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(body_area);
    draw_tabs(f, app, tab_area);
    let transcript_area = if app.panel_open && !app.panels.is_empty() {
        let [transcript, divider, panel] = Layout::horizontal([
            Constraint::Percentage(60),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .areas(main_area);
        f.render_widget(
            Paragraph::new("│").style(Style::new().fg(Color::DarkGray)),
            divider,
        );
        let panel_spec = &app.panels[app.panel_index];
        let title = panel_spec.title;
        let draw = panel_spec.draw;
        let [title_area, content_area] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(panel);
        f.render_widget(
            Paragraph::new(title).style(Style::new().fg(Color::DarkGray)),
            title_area,
        );
        draw(f, app, content_area);
        transcript
    } else {
        main_area
    };
    draw_transcript(f, app, transcript_area);
    if let Some(dialogue) = app.dialogue {
        let dialogue_height = dialogue
            .height
            .saturating_add(1)
            .min(f.area().height.saturating_sub(tab_area.height));
        let dialogue_area = Rect {
            x: f.area().x,
            y: f.area().bottom().saturating_sub(dialogue_height),
            width: f.area().width,
            height: dialogue_height,
        };
        f.render_widget(Clear, dialogue_area);
        draw_dialogue(f, app, dialogue, dialogue_area);
    } else {
        if !app.completions.is_empty() {
            draw_completions(f, app, completion_area);
        }
        draw_session_name(f, app, session_name_area);
        draw_rule(f, top_rule_area);
        draw_input(f, app, input_area);
        draw_rule(f, bottom_rule_area);
    }
}

fn draw_dialogue(f: &mut Frame, app: &mut App, dialogue: crate::app::DialogueSpec, area: Rect) {
    if area.height == 0 {
        return;
    }
    let [title_area, content_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let width = title_area.width as usize;
    let left_rule: String = "─── ".chars().take(width).collect();
    let mut title = Line::from(left_rule.clone());
    if width > left_rule.chars().count() {
        let visible_title: String = dialogue
            .title
            .chars()
            .take(width - left_rule.chars().count())
            .collect();
        title
            .spans
            .push(Span::styled(visible_title.clone(), dialogue.title_style));
        let remaining = width - left_rule.chars().count() - visible_title.chars().count();
        if remaining > 0 {
            title
                .spans
                .push(Span::raw(format!(" {}", "─".repeat(remaining - 1))));
        }
    }
    f.render_widget(Paragraph::new(title), title_area);
    (dialogue.draw)(f, app, content_area);
}

fn line_style(kind: OutKind) -> Style {
    match kind {
        OutKind::Input => Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        OutKind::Assistant => Style::new(),
        OutKind::Thought | OutKind::Tool => Style::new().fg(Color::DarkGray),
        OutKind::Error => Style::new().fg(Color::Red),
        OutKind::Info => Style::new().fg(Color::DarkGray),
    }
}

fn draw_transcript(f: &mut Frame, app: &mut App, area: Rect) {
    let height = area.height as usize;
    app.transcript_height = height;
    app.transcript_width = area.width as usize;
    let transcript = app.transcript_lines();
    let session = app.active_session_mut();
    let max_offset = transcript.len().saturating_sub(height);
    if session.scroll.stick {
        session.scroll.offset = max_offset;
    } else {
        session.scroll.offset = session.scroll.offset.min(max_offset);
    }
    let lines: Vec<Line> = transcript
        .iter()
        .skip(session.scroll.offset)
        .take(height)
        .map(|out| Line::styled(out.text.clone(), line_style(out.kind)))
        .collect();
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_input(f: &mut Frame, app: &App, area: Rect) {
    let prompt_style = if app.focus == Focus::Input {
        Style::new().fg(Color::Cyan)
    } else {
        Style::new().fg(Color::DarkGray)
    };
    let [prompt_area, text_area] =
        Layout::horizontal([Constraint::Length(3), Constraint::Min(1)]).areas(area);
    f.render_widget(Paragraph::new(" › ").style(prompt_style), prompt_area);
    f.render_widget(app.active_session().input.widget(), text_area);
}

fn draw_session_name(f: &mut Frame, app: &App, area: Rect) {
    let session = app.active_session();
    let mut spans = vec![Span::styled(
        session.name.clone(),
        Style::new().fg(Color::Cyan),
    )];
    if let Some(status) = &session.status {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(status.text.clone(), line_style(status.kind)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_tabs(f: &mut Frame, app: &App, area: Rect) {
    let tabs = app
        .sessions
        .iter()
        .enumerate()
        .map(|(index, session)| {
            let text = format!(" {}:{} ", index + 1, session.label);
            if index == app.session_index {
                Span::styled(text, Style::new().fg(Color::Black).bg(Color::Cyan))
            } else {
                Span::styled(text, Style::new().fg(Color::DarkGray))
            }
        })
        .collect::<Vec<_>>();
    f.render_widget(
        Paragraph::new(Line::from(tabs)).wrap(ratatui::widgets::Wrap { trim: true }),
        area,
    );
}

fn draw_rule(f: &mut Frame, area: Rect) {
    f.render_widget(Paragraph::new("─".repeat(area.width as usize)), area);
}

fn draw_completions(f: &mut Frame, app: &App, area: Rect) {
    if area.height == 0 {
        return;
    }
    let [_prompt_area, list_area] =
        Layout::horizontal([Constraint::Length(3), Constraint::Min(1)]).areas(area);
    let items = app
        .completions
        .iter()
        .map(|completion| {
            ListItem::new(Line::from(format!(
                "{}{}",
                completion.label,
                if completion.description.is_empty() {
                    String::new()
                } else {
                    format!("  {}", completion.description)
                }
            )))
        })
        .collect::<Vec<_>>();
    let list = List::new(items).highlight_style(Style::new().fg(Color::Cyan));
    let mut state = ListState::default();
    state.select(Some(app.completion_index.min(app.completions.len() - 1)));
    f.render_stateful_widget(list, list_area, &mut state);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{DialogueSpec, PanelSpec};
    use crossterm::event::KeyEvent;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn draw_test_dialogue(f: &mut Frame, _: &mut App, area: Rect) {
        f.render_widget(Paragraph::new("dialogue content"), area);
    }

    fn draw_test_panel(f: &mut Frame, _: &mut App, area: Rect) {
        f.render_widget(Paragraph::new("panel content"), area);
    }

    fn ignore_key(_: &mut App, _: KeyEvent) {}

    #[test]
    fn tabs_render_above_the_transcript_and_footer() {
        let mut app = App::new();
        app.active_session_mut().input.replace_line("draft".into());
        let mut terminal = Terminal::new(TestBackend::new(50, 10)).unwrap();

        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let buffer = terminal.backend().buffer();
        let tabs = (0..50).map(|x| buffer[(x, 0)].symbol()).collect::<String>();
        let status = (0..50).map(|x| buffer[(x, 6)].symbol()).collect::<String>();
        let prompt = (0..50).map(|x| buffer[(x, 8)].symbol()).collect::<String>();
        assert!(tabs.contains("1:"));
        assert!(status.contains("New session"));
        assert!(status.contains("Atlas"));
        assert!(prompt.contains("› draft"));
        assert_eq!(app.transcript_height, 5);
    }

    #[test]
    fn dialogue_overlays_the_footer_without_reflowing_main_content() {
        let mut app = App::new();
        app.active_session_mut()
            .input
            .replace_line("footer draft".into());
        app.register_panel(PanelSpec {
            name: "test",
            title: "Panel",
            draw: draw_test_panel,
            handle_key: ignore_key,
        });
        app.panel_open = true;
        let mut terminal = Terminal::new(TestBackend::new(50, 16)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let normal_transcript_height = app.transcript_height;

        app.dialogue = Some(DialogueSpec {
            title: "Test",
            title_style: Style::new().fg(Color::Yellow),
            height: 7,
            draw: draw_test_dialogue,
            handle_key: ignore_key,
        });
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let buffer = terminal.backend().buffer();
        let tabs = (0..50).map(|x| buffer[(x, 0)].symbol()).collect::<String>();
        let panel = (0..50).map(|x| buffer[(x, 1)].symbol()).collect::<String>();
        let title = (0..50).map(|x| buffer[(x, 8)].symbol()).collect::<String>();
        let content = (0..50).map(|x| buffer[(x, 9)].symbol()).collect::<String>();
        let former_status = (0..50)
            .map(|x| buffer[(x, 12)].symbol())
            .collect::<String>();
        let former_prompt = (0..50)
            .map(|x| buffer[(x, 14)].symbol())
            .collect::<String>();
        assert!(tabs.contains("1:"));
        assert!(panel.contains("Panel"));
        assert!(title.contains("Test"));
        assert!(content.contains("dialogue content"));
        assert!(!former_status.contains("New session"));
        assert!(!former_prompt.contains("footer draft"));
        assert_eq!(app.transcript_height, normal_transcript_height);
    }

    #[test]
    fn dialogue_is_clamped_below_tabs_on_short_terminals() {
        let mut app = App::new();
        app.dialogue = Some(DialogueSpec {
            title: "Test",
            title_style: Style::new(),
            height: 20,
            draw: draw_test_dialogue,
            handle_key: ignore_key,
        });
        let mut terminal = Terminal::new(TestBackend::new(20, 2)).unwrap();

        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let buffer = terminal.backend().buffer();
        let tabs = (0..20).map(|x| buffer[(x, 0)].symbol()).collect::<String>();
        let dialogue = (0..20).map(|x| buffer[(x, 1)].symbol()).collect::<String>();
        assert!(tabs.contains("1:"));
        assert!(dialogue.contains("Test"));
    }
}
