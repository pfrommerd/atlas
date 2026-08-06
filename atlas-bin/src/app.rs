//! Application state, command dispatch, and terminal event handling.

use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::DefaultTerminal;

use crate::input::InputBox;
use crate::ui;

pub struct Completion {
    pub replacement: String,
    pub label: String,
    pub description: String,
}

pub struct CommandContext<'a> {
    app: &'a mut App,
}

impl CommandContext<'_> {
    /// Append text to the terminal transcript.
    pub(crate) fn write(&mut self, kind: OutKind, text: &str) {
        self.app.push(kind, text);
    }

    /// Open a registered panel and give it focus.
    pub(crate) fn open_panel(&mut self, name: &str) {
        self.app.open_panel(name);
    }

    /// Open a dialogue above the prompt.
    pub(crate) fn open_dialogue(&mut self, dialogue: DialogueSpec) {
        self.app.open_dialogue(dialogue);
    }

    /// Request application shutdown.
    pub(crate) fn quit(&mut self) {
        self.app.should_quit = true;
    }
}

pub struct CommandSpec {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub description: &'static str,
    pub execute: fn(&mut CommandContext<'_>, &str),
    pub complete: fn(&CommandContext<'_>, &str) -> Vec<Completion>,
}

pub struct PanelSpec {
    pub name: &'static str,
    pub title: &'static str,
    pub draw: fn(&mut ratatui::Frame, &mut App, Rect),
    pub handle_key: fn(&mut App, KeyEvent),
}

/// A command-provided overlay drawn immediately above the prompt.
#[derive(Clone, Copy)]
pub struct DialogueSpec {
    pub title: &'static str,
    pub title_style: Style,
    pub height: u16,
    pub draw: fn(&mut ratatui::Frame, &mut App, Rect),
    pub handle_key: fn(&mut App, KeyEvent),
}

/// How a transcript line is styled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutKind {
    Input,
    Error,
    Info,
}

pub struct OutLine {
    pub kind: OutKind,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Input,
    Panel,
}

pub struct Scroll {
    pub offset: usize,
    pub stick: bool,
}

pub struct App {
    pub transcript: Vec<OutLine>,
    pub scroll: Scroll,
    pub input: InputBox,
    pub panel_open: bool,
    pub panel_index: usize,
    pub commands: Vec<CommandSpec>,
    pub panels: Vec<PanelSpec>,
    pub dialogue: Option<DialogueSpec>,
    pub completions: Vec<Completion>,
    pub completion_index: usize,
    pub focus: Focus,
    pub transcript_height: usize,
    input_auto_focused: bool,
    pub should_quit: bool,
}

pub fn run(mut terminal: DefaultTerminal) -> io::Result<()> {
    let mut app = App::new();
    while !app.should_quit {
        terminal.draw(|f| ui::draw(f, &mut app))?;
        if event::poll(Duration::from_millis(100))? {
            app.handle_event(event::read()?);
            while !app.should_quit && event::poll(Duration::ZERO)? {
                app.handle_event(event::read()?);
            }
        }
    }
    app.input.save_history();
    Ok(())
}

impl App {
    pub fn new() -> Self {
        let mut app = App {
            transcript: Vec::new(),
            scroll: Scroll {
                offset: 0,
                stick: true,
            },
            input: InputBox::new(),
            panel_open: false,
            panel_index: 0,
            commands: Vec::new(),
            panels: Vec::new(),
            dialogue: None,
            completions: Vec::new(),
            completion_index: 0,
            focus: Focus::Input,
            transcript_height: 0,
            input_auto_focused: false,
            should_quit: false,
        };
        for command in builtin_commands() {
            app.register_command(command);
        }
        app.push(
            OutKind::Info,
            "Atlas — agent shell ready. /help for commands; Ctrl+D to exit.",
        );
        app
    }

    pub(crate) fn register_command(&mut self, command: CommandSpec) {
        self.commands.push(command);
    }

    #[allow(dead_code)] // Reserved for panels supplied by the future agent layer.
    pub(crate) fn register_panel(&mut self, panel: PanelSpec) {
        self.panels.push(panel);
    }

    pub fn push(&mut self, kind: OutKind, text: &str) {
        for line in text.lines() {
            self.transcript.push(OutLine {
                kind,
                text: line.to_string(),
            });
        }
        if text.is_empty() {
            self.transcript.push(OutLine {
                kind,
                text: String::new(),
            });
        }
    }

    pub fn submit_line(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        self.push(OutKind::Input, &format!("you> {line}"));
        match line.strip_prefix('/') {
            Some(cmd) => self.dispatch_command(cmd),
            None => self.push(
                OutKind::Info,
                "Agent backend is not configured yet. Your prompt was received.",
            ),
        }
    }

    fn dispatch_command(&mut self, cmd: &str) {
        let name = cmd.split_whitespace().next().unwrap_or_default();
        let Some(command) = self.commands.iter().find(|command| {
            command.name == name || command.aliases.iter().any(|alias| *alias == name)
        }) else {
            self.push(
                OutKind::Error,
                &format!("unknown command: /{name} (try /help)"),
            );
            return;
        };
        (command.execute)(&mut CommandContext { app: self }, cmd);
    }

    fn open_panel(&mut self, name: &str) {
        let Some(index) = self.panels.iter().position(|panel| panel.name == name) else {
            self.push(OutKind::Error, &format!("unknown panel: {name}"));
            return;
        };
        self.panel_open = true;
        self.panel_index = index;
        self.focus = Focus::Panel;
    }

    pub fn toggle_panel(&mut self) {
        if self.panels.is_empty() {
            self.push(OutKind::Info, "No panels are registered.");
            return;
        }
        self.panel_open = !self.panel_open;
        self.focus = if self.panel_open {
            Focus::Panel
        } else {
            Focus::Input
        };
    }

    fn open_dialogue(&mut self, dialogue: DialogueSpec) {
        self.dialogue = Some(dialogue);
        self.completions.clear();
        self.completion_index = 0;
    }

    pub fn handle_event(&mut self, event: Event) {
        let Event::Key(key) = event else { return };
        if key.kind == KeyEventKind::Release {
            return;
        }
        if self.dialogue.is_some() {
            match (key.code, key.modifiers) {
                (KeyCode::Esc, _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                    self.dialogue = None
                }
                (KeyCode::Char('/'), _) => {
                    self.dialogue = None;
                    self.focus = Focus::Input;
                    self.input_auto_focused = false;
                    self.input.handle_key(key);
                    self.refresh_completions();
                }
                _ => (self.dialogue.expect("dialogue checked above").handle_key)(self, key),
            }
            return;
        }
        match (key.code, key.modifiers) {
            (KeyCode::Char('b'), KeyModifiers::CONTROL) => self.toggle_panel(),
            (KeyCode::Tab, _) if self.panel_open => {
                self.input_auto_focused = false;
                self.focus = match self.focus {
                    Focus::Input => Focus::Panel,
                    Focus::Panel => Focus::Input,
                };
            }
            (KeyCode::PageUp, _) => {
                self.scroll.stick = false;
                self.scroll.offset = self
                    .scroll
                    .offset
                    .saturating_sub(self.transcript_height.max(1));
            }
            (KeyCode::PageDown, _) => {
                let page = self.transcript_height.max(1);
                let max = self.transcript.len().saturating_sub(page);
                self.scroll.offset = (self.scroll.offset + page).min(max);
                if self.scroll.offset >= max {
                    self.scroll.stick = true;
                }
            }
            _ => match self.focus {
                Focus::Input => self.input_key(key),
                Focus::Panel => self.panel_key(key),
            },
        }
    }

    fn input_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('d') && key.modifiers == KeyModifiers::CONTROL {
            if self.input.is_empty() {
                self.should_quit = true;
            }
            return;
        }
        if key.code == KeyCode::Esc && !self.completions.is_empty() {
            self.completions.clear();
            return;
        }
        if !self.completions.is_empty() {
            match key.code {
                KeyCode::Up => {
                    self.completion_index = self.completion_index.saturating_sub(1);
                    return;
                }
                KeyCode::Down => {
                    self.completion_index =
                        (self.completion_index + 1).min(self.completions.len().saturating_sub(1));
                    return;
                }
                KeyCode::Enter => {
                    self.input
                        .replace_line(self.completions[self.completion_index].replacement.clone());
                    self.refresh_completions();
                    return;
                }
                _ => {}
            }
        }
        if let Some(line) = self.input.handle_key(key) {
            self.scroll.stick = true;
            self.submit_line(&line);
            self.completions.clear();
            return;
        }
        self.refresh_completions();
    }

    fn panel_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('/') {
            self.focus = Focus::Input;
            self.input_auto_focused = true;
            self.input.handle_key(key);
            self.refresh_completions();
            return;
        }
        if let Some(panel) = self.panels.get(self.panel_index) {
            (panel.handle_key)(self, key);
        }
    }

    fn refresh_completions(&mut self) {
        let line = self.input.line().to_string();
        let Some(command_line) = line.strip_prefix('/') else {
            self.completions.clear();
            self.completion_index = 0;
            return;
        };
        let mut words = command_line.split_whitespace();
        let name = words.next().unwrap_or_default();
        let has_argument = command_line.chars().any(char::is_whitespace);
        self.completions = if !has_argument {
            self.commands
                .iter()
                .filter(|command| command.name.starts_with(name) && command.name != name)
                .map(|command| Completion {
                    replacement: format!("/{}", command.name),
                    label: format!("/{}", command.name),
                    description: command.description.to_string(),
                })
                .collect()
        } else if let Some(command) = self.commands.iter().find(|command| {
            command.name == name || command.aliases.iter().any(|alias| *alias == name)
        }) {
            (command.complete)(&CommandContext { app: self }, command_line)
        } else {
            Vec::new()
        };
        if self.completions.len() == 1 && self.completions[0].replacement == line {
            self.completions.clear();
        }
        self.completion_index = self
            .completion_index
            .min(self.completions.len().saturating_sub(1));
    }
}

fn run_builtin(ctx: &mut CommandContext<'_>, cmd: &str) {
    match cmd.split_whitespace().next() {
        Some("help") => ctx.open_dialogue(DialogueSpec {
            title: "Help",
            title_style: Style::new().fg(Color::Rgb(255, 165, 0)),
            height: 9,
            draw: draw_help_dialogue,
            handle_key: ignore_dialogue_key,
        }),
        Some("panel") => match cmd.split_whitespace().nth(1) {
            Some(name) => ctx.open_panel(name),
            None => ctx.app.toggle_panel(),
        },
        Some("quit") | Some("exit") => ctx.quit(),
        _ => ctx.write(OutKind::Error, "unknown built-in command"),
    }
}

fn ignore_dialogue_key(_: &mut App, _: KeyEvent) {}

fn draw_help_dialogue(f: &mut ratatui::Frame, app: &mut App, area: Rect) {
    use ratatui::text::{Line, Span};
    use ratatui::widgets::Paragraph;

    let mut lines = vec![
        Line::raw(""),
        Line::styled(" Commands", Style::new().fg(Color::Yellow)),
    ];
    lines.extend(app.commands.iter().map(|command| {
        let aliases = if command.aliases.is_empty() {
            String::new()
        } else {
            format!(" ({})", command.aliases.join(", "))
        };
        Line::from(vec![
            Span::raw(format!("   /{}{aliases}", command.name)),
            Span::styled(
                format!("  {}", command.description),
                Style::new().fg(Color::DarkGray),
            ),
        ])
    }));
    lines.push(Line::raw(""));
    lines.push(Line::styled(" Keybindings", Style::new().fg(Color::Blue)));
    lines.push(Line::raw(
        "   PageUp/Down scroll · Ctrl+B toggles a registered panel · Ctrl+D exits",
    ));
    f.render_widget(Paragraph::new(lines), area);
}

fn no_completions(_: &CommandContext<'_>, _: &str) -> Vec<Completion> {
    Vec::new()
}

fn builtin_commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            name: "help",
            aliases: &[],
            description: "show available commands",
            execute: run_builtin,
            complete: no_completions,
        },
        CommandSpec {
            name: "panel",
            aliases: &[],
            description: "open or toggle a registered panel",
            execute: run_builtin,
            complete: no_completions,
        },
        CommandSpec {
            name: "quit",
            aliases: &["exit"],
            description: "exit the terminal",
            execute: run_builtin,
            complete: no_completions,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::widgets::Paragraph;

    fn custom_command(ctx: &mut CommandContext<'_>, _: &str) {
        ctx.write(OutKind::Info, "custom command ran");
    }

    fn draw_test_dialogue(f: &mut ratatui::Frame, _: &mut App, area: Rect) {
        f.render_widget(Paragraph::new("test dialogue"), area);
    }

    fn draw_test_panel(f: &mut ratatui::Frame, _: &mut App, area: Rect) {
        f.render_widget(Paragraph::new("test panel"), area);
    }

    fn ignore_key(_: &mut App, _: KeyEvent) {}

    fn open_test_dialogue(ctx: &mut CommandContext<'_>, _: &str) {
        ctx.open_dialogue(DialogueSpec {
            title: "Test",
            title_style: Style::new(),
            height: 1,
            draw: draw_test_dialogue,
            handle_key: ignore_key,
        });
    }

    #[test]
    fn prompts_receive_the_agent_placeholder_response() {
        let mut app = App::new();
        app.submit_line("hello");
        assert_eq!(
            app.transcript.last().unwrap().text,
            "Agent backend is not configured yet. Your prompt was received."
        );
    }

    #[test]
    fn no_panels_are_safe_to_toggle() {
        let mut app = App::new();
        app.toggle_panel();
        assert!(!app.panel_open);
        assert_eq!(
            app.transcript.last().unwrap().text,
            "No panels are registered."
        );
    }

    #[test]
    fn commands_are_completed() {
        let mut app = App::new();
        app.input.replace_line("/he".to_string());
        app.refresh_completions();
        assert!(app.completions.iter().any(|item| item.label == "/help"));
    }

    #[test]
    fn quit_command_requests_shutdown() {
        let mut app = App::new();
        app.submit_line("/quit");
        assert!(app.should_quit);
    }

    #[test]
    fn registered_commands_and_dialogues_work() {
        let mut app = App::new();
        app.register_command(CommandSpec {
            name: "custom",
            aliases: &[],
            description: "test command",
            execute: custom_command,
            complete: no_completions,
        });
        app.register_command(CommandSpec {
            name: "dialogue",
            aliases: &[],
            description: "test dialogue",
            execute: open_test_dialogue,
            complete: no_completions,
        });
        app.submit_line("/custom");
        assert_eq!(app.transcript.last().unwrap().text, "custom command ran");
        app.submit_line("/dialogue");
        assert_eq!(app.dialogue.unwrap().title, "Test");
    }

    #[test]
    fn registered_panels_open_and_receive_focus() {
        let mut app = App::new();
        app.register_panel(PanelSpec {
            name: "test",
            title: "test",
            draw: draw_test_panel,
            handle_key: ignore_key,
        });
        app.submit_line("/panel test");
        assert!(app.panel_open);
        assert_eq!(app.focus, Focus::Panel);
    }
}
