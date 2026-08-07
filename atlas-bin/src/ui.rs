//! Ratatui rendering for the transcript, prompt, completions, and optional panels.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::app::{App, Focus, OutKind};

pub fn draw(f: &mut Frame, app: &mut App) {
    let completion_height = if app.completions.is_empty() {
        0
    } else {
        app.completions.len().min(8) as u16
    };
    let available_height = f.area().height.saturating_sub(completion_height + 4);
    let dialogue_height = app
        .dialogue
        .map(|dialogue| (dialogue.height + 1).min(available_height))
        .unwrap_or(0);
    let transcript_height = available_height.saturating_sub(dialogue_height);
    let [main_area, dialogue_area, completion_area, top_rule_area, input_area, bottom_rule_area, tab_area] =
        Layout::vertical([
            Constraint::Length(transcript_height),
            Constraint::Length(dialogue_height),
            Constraint::Length(completion_height),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(f.area());
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
        draw_dialogue(f, app, dialogue, dialogue_area);
    }
    draw_rule(f, top_rule_area);
    draw_input(f, app, input_area);
    draw_rule(f, bottom_rule_area);
    draw_tabs(f, app, tab_area);
    if !app.completions.is_empty() {
        draw_completions(f, app, completion_area);
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
        OutKind::Error => Style::new().fg(Color::Red),
        OutKind::Info => Style::new().fg(Color::DarkGray),
    }
}

fn draw_transcript(f: &mut Frame, app: &mut App, area: Rect) {
    let height = area.height as usize;
    app.transcript_height = height;
    let session = app.active_session_mut();
    let max_offset = session.transcript.len().saturating_sub(height);
    if session.scroll.stick {
        session.scroll.offset = max_offset;
    } else {
        session.scroll.offset = session.scroll.offset.min(max_offset);
    }
    let lines: Vec<Line> = session
        .transcript
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
