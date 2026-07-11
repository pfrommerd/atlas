//! All ratatui rendering: the transcript + input column, the collapsible
//! heap/stepper side panel, and the status bar.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use std::collections::HashMap;

use crate::app::{App, Focus, OutKind};
use crate::eval::EvalState;

pub fn draw(f: &mut Frame, app: &mut App) {
    let [main_area, status_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(f.area());

    let column = if app.panel_open {
        let [column, panel] =
            Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)])
                .areas(main_area);
        draw_panel(f, app, panel);
        column
    } else {
        main_area
    };

    let completion_height = if app.completions.is_empty() {
        0
    } else {
        (app.completions.len() + 2).min(10) as u16
    };
    let [transcript_area, completion_area, input_area] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(completion_height),
        Constraint::Length(3),
    ])
    .areas(column);
    draw_transcript(f, app, transcript_area);
    draw_completions(f, app, completion_area);
    draw_input(f, app, input_area);
    draw_status(f, app, status_area);
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
    let border = if focused {
        Style::new().fg(Color::Cyan)
    } else {
        Style::new().fg(Color::DarkGray)
    };
    let block = Block::bordered()
        .border_style(border)
        .title(format!(" {} ", app.mode.label()));
    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(app.input.widget(), inner);
}

fn draw_completions(f: &mut Frame, app: &App, area: Rect) {
    if app.completions.is_empty() || area.height == 0 {
        return;
    }
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
    let list = List::new(items)
        .block(Block::bordered().title(" commands "))
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default();
    state.select(Some(app.completion_index.min(app.completions.len() - 1)));
    f.render_stateful_widget(list, area, &mut state);
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let eval = match &app.eval {
        EvalState::Idle => "idle".to_string(),
        EvalState::Running(run) if run.paused => format!("paused @{}", run.steps),
        EvalState::Running(run) => format!("running @{}", run.steps),
    };
    let strong = if app.session.strong { "on" } else { "off" };
    let left = format!(
        " lang={}  budget={}  strong={strong}  eval={eval}",
        app.mode.label(),
        app.session.budget,
    );
    let right = "^B panel · Tab focus · ^C quit ";
    let pad = (area.width as usize).saturating_sub(left.chars().count() + right.chars().count());
    let line = Line::from(vec![
        Span::raw(left),
        Span::raw(" ".repeat(pad)),
        Span::styled(right, Style::new().fg(Color::DarkGray)),
    ]);
    f.render_widget(
        Paragraph::new(line).style(Style::new().bg(Color::Black).fg(Color::Gray)),
        area,
    );
}

fn draw_panel(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Panel;
    let border = if focused {
        Style::new().fg(Color::Cyan)
    } else {
        Style::new().fg(Color::DarkGray)
    };
    let panel = &app.panels[app.panel_index];
    let block = Block::bordered()
        .border_style(border)
        .title(format!(" {} ", panel.title));
    let inner = block.inner(area);
    f.render_widget(block, area);
    (panel.draw)(f, app, inner);
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
