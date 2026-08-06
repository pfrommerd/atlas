//! All ratatui rendering: the editor-style transcript and prompt, the
//! collapsible heap/stepper inspector, and the prompt chrome.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use std::collections::HashMap;

use crate::app::{App, Focus, OutKind};
use crate::eval::EvalState;

pub fn draw(f: &mut Frame, app: &mut App) {
    let completion_height = if app.completions.is_empty() {
        0
    } else {
        app.completions.len().min(8) as u16
    };
    let mode_height = if app.dialogue.is_none() && app.completions.is_empty() {
        1
    } else {
        0
    };
    // Keep the prompt's two rules and blank lower row visible even on small
    // terminals; the dialogue is clipped before it can consume that chrome.
    let available_height = f
        .area()
        .height
        .saturating_sub(completion_height + mode_height + 4);
    let dialogue_height = app
        .dialogue
        .map(|dialogue| (dialogue.height + 1).min(available_height))
        .unwrap_or(0);
    let transcript_height = available_height.saturating_sub(dialogue_height);
    let [main_area, dialogue_area, completion_area, mode_area, top_rule_area, input_area, bottom_rule_area, _empty_area] =
        Layout::vertical([
            Constraint::Length(transcript_height),
            Constraint::Length(dialogue_height),
            Constraint::Length(completion_height),
            Constraint::Length(mode_height),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(f.area());

    let transcript_area = if app.panel_open {
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
        draw_panel(f, app, panel);
        transcript
    } else {
        main_area
    };

    draw_transcript(f, app, transcript_area);
    if let Some(dialogue) = app.dialogue {
        draw_dialogue(f, app, dialogue, dialogue_area);
    }
    if app.completions.is_empty() {
        let mode_label = match app.mode {
            crate::session::LangMode::Core => " Core",
            crate::session::LangMode::Atlas => " Atlas",
            crate::session::LangMode::Agent => "",
        };
        f.render_widget(
            Paragraph::new(mode_label).style(Style::new().fg(Color::DarkGray)),
            mode_area,
        );
    }
    draw_rule(f, top_rule_area);
    draw_input(f, app, input_area);
    draw_rule(f, bottom_rule_area);
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
        let available_title_width = width - left_rule.chars().count();
        let visible_title: String = dialogue.title.chars().take(available_title_width).collect();
        let remaining_width = available_title_width - visible_title.chars().count();
        title
            .spans
            .push(Span::styled(visible_title, dialogue.title_style));
        if remaining_width > 0 {
            title
                .spans
                .push(Span::raw(format!(" {}", "─".repeat(remaining_width - 1))));
        }
    }
    f.render_widget(Paragraph::new(title), title_area);
    (dialogue.draw)(f, app, content_area);
}

fn line_style(kind: OutKind) -> Style {
    match kind {
        OutKind::Input => Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        OutKind::Output => Style::new(),
        OutKind::Error => Style::new().fg(Color::Red),
        OutKind::Info => Style::new().fg(Color::DarkGray),
        OutKind::Step => Style::new().fg(Color::Yellow),
    }
}

fn draw_transcript(f: &mut Frame, app: &mut App, area: Rect) {
    let height = area.height as usize;
    app.transcript_height = height;
    let total = app.transcript.len();
    let max_offset = total.saturating_sub(height);
    if app.scroll.stick {
        app.scroll.offset = max_offset;
    } else {
        app.scroll.offset = app.scroll.offset.min(max_offset);
    }

    let lines: Vec<Line> = app
        .transcript
        .iter()
        .skip(app.scroll.offset)
        .take(height)
        .map(|out| Line::styled(out.text.clone(), line_style(out.kind)))
        .collect();
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_input(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Input;
    let prompt_style = if focused {
        Style::new().fg(Color::Cyan)
    } else {
        Style::new().fg(Color::DarkGray)
    };
    let [prompt_area, text_area] =
        Layout::horizontal([Constraint::Length(3), Constraint::Min(1)]).areas(area);
    f.render_widget(Paragraph::new(" › ").style(prompt_style), prompt_area);
    f.render_widget(app.input.widget(), text_area);
}

fn draw_rule(f: &mut Frame, area: Rect) {
    f.render_widget(Paragraph::new("─".repeat(area.width as usize)), area);
}

fn draw_completions(f: &mut Frame, app: &App, area: Rect) {
    if app.completions.is_empty() || area.height == 0 {
        return;
    }
    let [_prompt_area, list_area] =
        Layout::horizontal([Constraint::Length(3), Constraint::Min(1)]).areas(area);
    let items = app
        .completions
        .iter()
        .map(|completion| {
            let description = if completion.description.is_empty() {
                String::new()
            } else {
                format!("  {}", completion.description)
            };
            ListItem::new(Line::from(format!("{}{}", completion.label, description)))
        })
        .collect::<Vec<_>>();
    let list = List::new(items).highlight_style(Style::new().fg(Color::Cyan));
    let mut state = ListState::default();
    state.select(Some(app.completion_index.min(app.completions.len() - 1)));
    f.render_stateful_widget(list, list_area, &mut state);
}

fn draw_panel(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Panel;
    let border = if focused {
        Style::new().fg(Color::Cyan)
    } else {
        Style::new().fg(Color::DarkGray)
    };
    let panel = &app.panels[app.panel_index];
    let [title_area, content_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
    f.render_widget(Paragraph::new(panel.title).style(border), title_area);
    (panel.draw)(f, app, content_area);
}

pub(crate) fn draw_memory_panel(f: &mut Frame, app: &mut App, area: Rect) {
    let [stats_area, detail_area, list_area, hint_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area);

    // Per-arena live counts.
    let stats = app
        .explorer
        .stats
        .iter()
        .map(|(kind, len)| format!("{} {len}", kind.label()))
        .collect::<Vec<_>>()
        .join("  ");
    let mode = if app.explorer.show_leaked {
        "full dump (leaks shown)"
    } else {
        "reachable from roots"
    };
    f.render_widget(
        Paragraph::new(vec![
            Line::styled(stats, Style::new().fg(Color::DarkGray)),
            Line::styled(mode, Style::new().fg(Color::DarkGray).italic()),
        ]),
        stats_area,
    );

    let address_to_id: HashMap<u64, usize> = app
        .explorer
        .nodes
        .iter()
        .map(|node| (node.addr.to_u64(), node.id))
        .collect();
    let selected = app.explorer.selected_node();
    let detail_lines = selected
        .map(|node| {
            let roots = if node.roots.is_empty() {
                "-".to_string()
            } else {
                node.roots.join(", ")
            };
            let incoming = if node.incoming.is_empty() {
                "-".to_string()
            } else {
                node.incoming.join(", ")
            };
            let ports = if !node.expanded {
                "press Enter to show ports".to_string()
            } else if node.edges.is_empty() {
                "-".to_string()
            } else {
                node.edges
                    .iter()
                    .map(|(edge, addr)| {
                        let target = address_to_id
                            .get(&addr.to_u64())
                            .map(|id| format!("n{id}"))
                            .unwrap_or_else(|| format!("@{}", addr.to_u64()));
                        format!("{edge}→{target}")
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            vec![
                Line::from(format!(
                    "n{} @{}  {}",
                    node.id,
                    node.addr.to_u64(),
                    node.summary
                )),
                Line::styled(
                    format!("roots: {roots}  incoming: {incoming}"),
                    Style::new().fg(Color::DarkGray),
                ),
                Line::styled(format!("ports: {ports}"), Style::new().fg(Color::DarkGray)),
            ]
        })
        .unwrap_or_else(|| {
            vec![Line::styled(
                "no reachable nodes",
                Style::new().fg(Color::DarkGray),
            )]
        });
    f.render_widget(Paragraph::new(detail_lines), detail_area);

    let items: Vec<ListItem> = app
        .explorer
        .nodes
        .iter()
        .map(|node| {
            let marker = if node.expanded { "▾" } else { "·" };
            let root = if node.roots.is_empty() { " " } else { "◆" };
            let style = if node.leaked {
                Style::new().fg(Color::Red)
            } else {
                Style::new()
            };
            let edge_count = node.edges.len();
            let incoming_count = node.incoming.len();
            ListItem::new(Line::from(vec![
                Span::styled(format!("{root}{marker} "), style),
                Span::styled(format!("n{} ", node.id), style.bold()),
                Span::styled(node.summary.clone(), style),
                Span::styled(
                    format!("  @{}  ↔{incoming_count} →{edge_count}", node.addr.to_u64()),
                    Style::new().fg(Color::DarkGray),
                ),
            ]))
        })
        .collect();
    let empty = items.is_empty();
    let list = List::new(items).highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default();
    if !empty {
        state.select(Some(app.explorer.selected));
    }
    f.render_stateful_widget(list, list_area, &mut state);

    f.render_widget(
        Paragraph::new("↑↓ select · ⏎ details · d leaks · r refresh")
            .style(Style::new().fg(Color::DarkGray)),
        hint_area,
    );
}

pub(crate) fn draw_stepper_panel(f: &mut Frame, app: &mut App, area: Rect) {
    let [status_area, history_area, hint_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area);

    let mut lines = Vec::new();
    match &app.eval {
        EvalState::Idle => {
            lines.push(Line::styled(
                "no evaluation pending",
                Style::new().fg(Color::DarkGray),
            ));
            lines.push(Line::styled(
                "start one paused with /step <expr>",
                Style::new().fg(Color::DarkGray),
            ));
        }
        EvalState::Running(run) => {
            let state = if run.paused { "paused" } else { "running" };
            let strength = if run.strong { "strong" } else { "weak head" };
            lines.push(Line::from(format!(
                "{state} · {} / {} interactions · {strength}",
                run.steps, run.budget
            )));
        }
    }
    f.render_widget(Paragraph::new(lines), status_area);

    let history = app
        .eval
        .run_state()
        .map(|run| &run.history)
        .into_iter()
        .flatten();
    let items: Vec<ListItem> = history
        .map(|(step, interaction)| ListItem::new(Line::from(format!("{step:>6}  {interaction:?}"))))
        .collect();
    let len = items.len();
    let list = List::new(items).highlight_style(Style::new().add_modifier(Modifier::BOLD));
    let mut state = ListState::default();
    if len > 0 {
        // Keep the newest interaction in view.
        state.select(Some(len - 1));
    }
    f.render_stateful_widget(list, history_area, &mut state);

    f.render_widget(
        Paragraph::new("s step · c continue · p pause · x abort")
            .style(Style::new().fg(Color::DarkGray)),
        hint_area,
    );
}

#[cfg(test)]
mod tests {
    use atlas_core::vm::heap::Heap;
    use ratatui::{backend::TestBackend, Terminal};

    use super::*;
    use crate::app::{Completion, DialogueSpec};
    use crate::{Args, LangArg};

    fn render(app: &mut App<'_>, width: u16, height: u16) -> Terminal<TestBackend> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        terminal
    }

    #[test]
    fn prompt_uses_mode_rules_marker_and_empty_lower_row() {
        Heap::new().with(|h| {
            let args = Args {
                lang: LangArg::Core,
                budget: 1_000,
                strong: false,
                no_prelude: true,
                source: Vec::new(),
            };
            let mut app = App::new(h, &args);
            app.input.replace_line("test".to_string());
            let terminal = render(&mut app, 40, 12);
            let buffer = terminal.backend().buffer();

            assert_eq!(buffer[(0, 7)].symbol(), " ");
            assert_eq!(buffer[(1, 7)].symbol(), "C");
            assert_eq!(buffer[(0, 8)].symbol(), "─");
            assert_eq!(buffer[(0, 9)].symbol(), " ");
            assert_eq!(buffer[(1, 9)].symbol(), "›");
            assert_eq!(buffer[(3, 9)].symbol(), "t");
            assert_eq!(buffer[(0, 10)].symbol(), "─");
            assert_eq!(buffer[(1, 11)].symbol(), " ");
        });
    }

    #[test]
    fn completions_hide_the_mode_row_above_the_prompt() {
        Heap::new().with(|h| {
            let args = Args {
                lang: LangArg::Core,
                budget: 1_000,
                strong: false,
                no_prelude: true,
                source: Vec::new(),
            };
            let mut app = App::new(h, &args);
            app.completions = vec![
                Completion {
                    replacement: "/help".to_string(),
                    label: "/help".to_string(),
                    description: "show help".to_string(),
                },
                Completion {
                    replacement: "/quit".to_string(),
                    label: "/quit".to_string(),
                    description: "exit".to_string(),
                },
            ];
            let terminal = render(&mut app, 40, 12);
            let buffer = terminal.backend().buffer();

            assert_eq!(buffer[(3, 6)].symbol(), "/");
            assert_eq!(buffer[(4, 6)].symbol(), "h");
            assert_eq!(buffer[(3, 6)].fg, Color::Cyan);
            assert_eq!(buffer[(0, 8)].symbol(), "─");
            assert_eq!(buffer[(1, 9)].symbol(), "›");
            assert_eq!(buffer[(0, 10)].symbol(), "─");
            assert_eq!(buffer[(1, 11)].symbol(), " ");
        });
    }

    #[test]
    fn open_panel_uses_a_divider_and_plain_heading() {
        Heap::new().with(|h| {
            let args = Args {
                lang: LangArg::Core,
                budget: 1_000,
                strong: false,
                no_prelude: true,
                source: Vec::new(),
            };
            let mut app = App::new(h, &args);
            app.panel_open = true;
            let terminal = render(&mut app, 40, 12);
            let buffer = terminal.backend().buffer();

            assert_eq!(buffer[(24, 0)].symbol(), "│");
            assert_eq!(buffer[(25, 0)].symbol(), "m");
        });
    }

    #[test]
    fn agent_mode_omits_the_prompt_label() {
        Heap::new().with(|h| {
            let args = Args {
                lang: LangArg::Agent,
                budget: 1_000,
                strong: false,
                no_prelude: true,
                source: Vec::new(),
            };
            let mut app = App::new(h, &args);
            let terminal = render(&mut app, 40, 12);
            let buffer = terminal.backend().buffer();

            assert_eq!(buffer[(0, 7)].symbol(), " ");
        });
    }

    fn draw_test_dialogue(f: &mut Frame, _: &mut App, area: Rect) {
        f.render_widget(Paragraph::new("dialogue body"), area);
    }

    fn ignore_test_dialogue_key(_: &mut App, _: crossterm::event::KeyEvent) {}

    #[test]
    fn dialogue_renders_above_prompt_and_hides_mode_row() {
        Heap::new().with(|h| {
            let args = Args {
                lang: LangArg::Core,
                budget: 1_000,
                strong: false,
                no_prelude: true,
                source: Vec::new(),
            };
            let mut app = App::new(h, &args);
            app.dialogue = Some(DialogueSpec {
                title: "Test",
                title_style: Style::new(),
                height: 3,
                draw: draw_test_dialogue,
                handle_key: ignore_test_dialogue_key,
            });
            let terminal = render(&mut app, 40, 12);
            let buffer = terminal.backend().buffer();

            assert_eq!(buffer[(0, 4)].symbol(), "─");
            assert_eq!(buffer[(4, 4)].symbol(), "T");
            assert_eq!(buffer[(0, 5)].symbol(), "d");
            assert_ne!(buffer[(1, 4)].symbol(), "C");
            assert_eq!(buffer[(0, 8)].symbol(), "─");
            assert_eq!(buffer[(1, 9)].symbol(), "›");
            assert_eq!(buffer[(0, 10)].symbol(), "─");
            assert_eq!(buffer[(1, 11)].symbol(), " ");
        });
    }

    #[test]
    fn help_dialogue_uses_its_configured_title_and_heading_styles() {
        Heap::new().with(|h| {
            let args = Args {
                lang: LangArg::Core,
                budget: 1_000,
                strong: false,
                no_prelude: true,
                source: Vec::new(),
            };
            let mut app = App::new(h, &args);
            app.submit_line("/help");
            let terminal = render(&mut app, 40, 24);
            let buffer = terminal.backend().buffer();

            assert_eq!(buffer[(4, 2)].symbol(), "H");
            assert_eq!(buffer[(4, 2)].fg, Color::Rgb(255, 165, 0));
            assert_eq!(buffer[(0, 2)].fg, Color::Reset);
            assert_eq!(buffer[(0, 3)].symbol(), " ");
            assert_eq!(buffer[(1, 4)].symbol(), "C");
            assert_eq!(buffer[(1, 4)].fg, Color::Yellow);
            assert_eq!(buffer[(0, 16)].symbol(), " ");
            assert_eq!(buffer[(1, 17)].symbol(), "K");
            assert_eq!(buffer[(1, 17)].fg, Color::Blue);
            assert_eq!(buffer[(3, 18)].fg, Color::Reset);
            assert_eq!(buffer[(0, 19)].symbol(), " ");
        });
    }
}
