//! Application state, command dispatch, and terminal event handling.

use std::io;
use std::time::Duration;

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::DefaultTerminal;

use crate::client::DaemonClient;
use crate::input::InputBox;
use crate::protocol::SessionListEvent;
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
        self.app.quit();
    }

    pub(crate) fn detach(&mut self) {
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

/// UI state belonging to one ACP session/tab.  Backend synchronization is added
/// at the boundary; the TUI never shares drafts or transcript position between tabs.
pub struct Session {
    pub id: String,
    pub label: String,
    pub name: String,
    pub cwd: String,
    pub additional_directories: Vec<String>,
    pub transcript: Vec<OutLine>,
    pub scroll: Scroll,
    pub input: InputBox,
}

impl Session {
    fn new(number: usize) -> Self {
        Self::from_id(format!("local-{number}"))
    }

    fn from_id(id: String) -> Self {
        const ADJECTIVES: &[&str] = &[
            "amber", "brisk", "calm", "daring", "ember", "frost", "golden", "hidden",
        ];
        const NOUNS: &[&str] = &[
            "badger", "comet", "falcon", "harbor", "juniper", "otter", "raven", "willow",
        ];
        let hash = id.bytes().fold(0usize, |hash, byte| {
            hash.wrapping_mul(0x9e37_79b9).wrapping_add(byte as usize)
        });
        Self {
            id,
            label: format!(
                "{}-{}",
                ADJECTIVES[hash % ADJECTIVES.len()],
                NOUNS[(hash / ADJECTIVES.len()) % NOUNS.len()]
            ),
            name: "New session".into(),
            cwd: std::env::current_dir()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|_| ".".into()),
            additional_directories: Vec::new(),
            transcript: Vec::new(),
            scroll: Scroll {
                offset: 0,
                stick: true,
            },
            input: InputBox::new(),
        }
    }
}

pub struct App {
    pub sessions: Vec<Session>,
    pub session_index: usize,
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
    session_prefix: bool,
    daemon: Option<DaemonClient>,
    close_sessions_on_exit: bool,
    pub should_quit: bool,
}

pub async fn run(
    mut terminal: DefaultTerminal,
    daemon: DaemonClient,
    sessions: Vec<atlas_acp::latest::SessionInfo>,
) -> io::Result<()> {
    let mut app = App::with_daemon(daemon, sessions).await;
    let mut events = EventStream::new();
    while !app.should_quit {
        app.apply_daemon_events();
        terminal.draw(|f| ui::draw(f, &mut app))?;
        tokio::select! {
            event = events.next() => match event {
                Some(Ok(event)) => app.handle_event(event).await,
                Some(Err(error)) => return Err(error),
                None => break,
            },
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
    }
    for session in &app.sessions {
        session.input.save_history();
    }
    app.shutdown().await;
    Ok(())
}

impl App {
    pub fn new() -> Self {
        let mut app = App {
            sessions: vec![Session::new(1)],
            session_index: 0,
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
            session_prefix: false,
            daemon: None,
            close_sessions_on_exit: false,
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

    pub async fn with_daemon(
        daemon: DaemonClient,
        sessions: Vec<atlas_acp::latest::SessionInfo>,
    ) -> Self {
        let mut app = Self::new();
        app.daemon = Some(daemon);
        app.sessions.clear();
        app.session_index = 0;
        for session in sessions {
            app.upsert_session(session);
        }
        if app.sessions.is_empty() {
            app.new_session().await;
        } else if let Some(session) = app.sessions.first() {
            if let Err(error) = app
                .daemon
                .as_ref()
                .expect("daemon installed")
                .resume(
                    session.id.clone(),
                    session.cwd.clone(),
                    session.additional_directories.clone(),
                )
                .await
            {
                app.push(OutKind::Error, &format!("session resume failed: {error}"));
            }
        }
        app
    }

    pub(crate) fn register_command(&mut self, command: CommandSpec) {
        self.commands.push(command);
    }

    #[allow(dead_code)] // Reserved for panels supplied by the future agent layer.
    pub(crate) fn register_panel(&mut self, panel: PanelSpec) {
        self.panels.push(panel);
    }

    pub fn active_session(&self) -> &Session {
        &self.sessions[self.session_index]
    }

    pub fn active_session_mut(&mut self) -> &mut Session {
        &mut self.sessions[self.session_index]
    }

    pub fn push(&mut self, kind: OutKind, text: &str) {
        for line in text.lines() {
            self.active_session_mut().transcript.push(OutLine {
                kind,
                text: line.to_string(),
            });
        }
        if text.is_empty() {
            self.active_session_mut().transcript.push(OutLine {
                kind,
                text: String::new(),
            });
        }
    }

    pub async fn submit_line(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        self.push(OutKind::Input, &format!("you> {line}"));
        match line.strip_prefix('/') {
            Some(cmd) => self.dispatch_command(cmd),
            None => {
                if let Some(daemon) = self.daemon.clone() {
                    let session_id = self.active_session().id.clone();
                    if let Err(error) = daemon.prompt(session_id, line.to_string()).await {
                        self.push(OutKind::Error, &format!("prompt failed: {error}"));
                    }
                } else {
                    self.push(
                        OutKind::Info,
                        "Agent backend is not configured yet. Your prompt was received.",
                    );
                }
            }
        }
    }

    async fn new_session(&mut self) {
        if let Some(daemon) = self.daemon.clone() {
            let was_empty = self.sessions.is_empty();
            if was_empty {
                self.sessions.push(Session::from_id("pending".into()));
                self.session_index = 0;
            }
            let cwd = std::env::current_dir()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|_| ".".into());
            match daemon.new_session(cwd.clone()).await {
                Ok(id) => {
                    if was_empty {
                        self.sessions[0] = Session::from_id(id.clone());
                    } else {
                        self.sessions.push(Session::from_id(id.clone()));
                        self.session_index = self.sessions.len() - 1;
                    }
                    if let Err(error) = daemon.resume(id, cwd, Vec::new()).await {
                        self.push(OutKind::Error, &format!("session resume failed: {error}"));
                    }
                }
                Err(error) => self.push(OutKind::Error, &format!("new session failed: {error}")),
            }
            return;
        }
        let number = self.sessions.len() + 1;
        self.sessions.push(Session::new(number));
        self.session_index = self.sessions.len() - 1;
        self.push(OutKind::Info, "New session ready.");
    }

    fn cycle_session(&mut self, forward: bool) {
        if self.sessions.len() < 2 {
            return;
        }
        self.session_index = if forward {
            (self.session_index + 1) % self.sessions.len()
        } else {
            (self.session_index + self.sessions.len() - 1) % self.sessions.len()
        };
    }

    async fn close_session(&mut self) {
        if let Some(daemon) = self.daemon.clone() {
            let session_id = self.active_session().id.clone();
            if let Err(error) = daemon.close(session_id).await {
                self.push(OutKind::Error, &format!("close session failed: {error}"));
                return;
            }
        }
        if self.sessions.len() == 1 {
            self.should_quit = true;
            return;
        }
        self.sessions.remove(self.session_index);
        self.session_index = self.session_index.min(self.sessions.len() - 1);
    }

    fn quit(&mut self) {
        self.close_sessions_on_exit = true;
        self.should_quit = true;
    }

    async fn shutdown(&mut self) {
        if self.close_sessions_on_exit {
            if let Some(daemon) = self.daemon.clone() {
                for session in &self.sessions {
                    let _ = daemon.close(session.id.clone()).await;
                }
            }
        }
    }

    fn upsert_session(&mut self, session: atlas_acp::latest::SessionInfo) {
        let name = session.title.unwrap_or_else(|| "Untitled session".into());
        if let Some(existing) = self
            .sessions
            .iter_mut()
            .find(|existing| existing.id == session.session_id)
        {
            existing.name = name;
            existing.cwd = session.cwd;
            existing.additional_directories = session.additional_directories;
        } else {
            let mut tab = Session::from_id(session.session_id);
            tab.name = name;
            tab.cwd = session.cwd;
            tab.additional_directories = session.additional_directories;
            self.sessions.push(tab);
        }
    }

    fn apply_daemon_events(&mut self) {
        let Some(daemon) = self.daemon.clone() else {
            return;
        };
        for event in daemon.drain_events() {
            match event {
                SessionListEvent::Snapshot { sessions } => {
                    for session in sessions {
                        self.upsert_session(session);
                    }
                }
                SessionListEvent::Added { session } | SessionListEvent::Updated { session } => {
                    self.upsert_session(session);
                }
                SessionListEvent::Removed { session_id } => {
                    if let Some(index) = self
                        .sessions
                        .iter()
                        .position(|session| session.id == session_id)
                    {
                        self.sessions.remove(index);
                        if self.sessions.is_empty() {
                            self.should_quit = true;
                        } else {
                            self.session_index = self.session_index.min(self.sessions.len() - 1);
                        }
                    }
                }
            }
        }
        for update in daemon.drain_updates() {
            if let Some(session) = self
                .sessions
                .iter_mut()
                .find(|session| session.id == update.session_id)
            {
                session.transcript.push(OutLine {
                    kind: OutKind::Info,
                    text: update.update.to_string(),
                });
            }
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

    pub async fn handle_event(&mut self, event: Event) {
        let Event::Key(key) = event else { return };
        if key.kind == KeyEventKind::Release {
            return;
        }
        if self.session_prefix {
            self.session_prefix = false;
            match key.code {
                KeyCode::Char('n') => self.cycle_session(true),
                KeyCode::Char('p') => self.cycle_session(false),
                KeyCode::Char('c') => self.new_session().await,
                KeyCode::Char('q') => self.close_session().await,
                KeyCode::Char('d') => self.should_quit = true,
                _ => {}
            }
            return;
        }
        if key.modifiers == KeyModifiers::ALT {
            if let KeyCode::Char(number @ '1'..='9') = key.code {
                let index = number as usize - '1' as usize;
                if index < self.sessions.len() {
                    self.session_index = index;
                }
                return;
            }
        }
        if key.code == KeyCode::Char('f') && key.modifiers == KeyModifiers::CONTROL {
            self.session_prefix = true;
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
                    self.active_session_mut().input.handle_key(key);
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
                self.active_session_mut().scroll.stick = false;
                self.active_session_mut().scroll.offset = self
                    .active_session()
                    .scroll
                    .offset
                    .saturating_sub(self.transcript_height.max(1));
            }
            (KeyCode::PageDown, _) => {
                let page = self.transcript_height.max(1);
                let max = self.active_session().transcript.len().saturating_sub(page);
                let session = self.active_session_mut();
                session.scroll.offset = (session.scroll.offset + page).min(max);
                if session.scroll.offset >= max {
                    session.scroll.stick = true;
                }
            }
            _ => match self.focus {
                Focus::Input => self.input_key(key).await,
                Focus::Panel => self.panel_key(key),
            },
        }
    }

    async fn input_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('d') && key.modifiers == KeyModifiers::CONTROL {
            if self.active_session().input.is_empty() {
                self.close_session().await;
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
                    let replacement = self.completions[self.completion_index].replacement.clone();
                    self.active_session_mut().input.replace_line(replacement);
                    self.refresh_completions();
                    return;
                }
                _ => {}
            }
        }
        if let Some(line) = self.active_session_mut().input.handle_key(key) {
            self.active_session_mut().scroll.stick = true;
            self.submit_line(&line).await;
            self.completions.clear();
            return;
        }
        self.refresh_completions();
    }

    fn panel_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('/') {
            self.focus = Focus::Input;
            self.input_auto_focused = true;
            self.active_session_mut().input.handle_key(key);
            self.refresh_completions();
            return;
        }
        if let Some(panel) = self.panels.get(self.panel_index) {
            (panel.handle_key)(self, key);
        }
    }

    fn refresh_completions(&mut self) {
        let line = self.active_session().input.line().to_string();
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
        Some("detach") => ctx.detach(),
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
        "   PageUp/Down scroll · Ctrl+F then N/P/C/Q/D manages sessions · Alt+1..9 selects a tab",
    ));
    lines.push(Line::raw(
        "   Ctrl+B toggles a registered panel · Ctrl+D exits",
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
        CommandSpec {
            name: "detach",
            aliases: &[],
            description: "disconnect from Atlas without closing sessions",
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

    #[tokio::test]
    async fn prompts_receive_the_agent_placeholder_response() {
        let mut app = App::new();
        app.submit_line("hello").await;
        assert_eq!(
            app.active_session().transcript.last().unwrap().text,
            "Agent backend is not configured yet. Your prompt was received."
        );
    }

    #[tokio::test]
    async fn no_panels_are_safe_to_toggle() {
        let mut app = App::new();
        app.toggle_panel();
        assert!(!app.panel_open);
        assert_eq!(
            app.active_session().transcript.last().unwrap().text,
            "No panels are registered."
        );
    }

    #[tokio::test]
    async fn commands_are_completed() {
        let mut app = App::new();
        app.active_session_mut()
            .input
            .replace_line("/he".to_string());
        app.refresh_completions();
        assert!(app.completions.iter().any(|item| item.label == "/help"));
    }

    #[tokio::test]
    async fn quit_command_requests_shutdown() {
        let mut app = App::new();
        app.submit_line("/quit").await;
        assert!(app.should_quit);
    }

    #[tokio::test]
    async fn detach_command_requests_shutdown() {
        let mut app = App::new();
        app.submit_line("/detach").await;
        assert!(app.should_quit);
    }

    #[tokio::test]
    async fn registered_commands_and_dialogues_work() {
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
        app.submit_line("/custom").await;
        assert_eq!(
            app.active_session().transcript.last().unwrap().text,
            "custom command ran"
        );
        app.submit_line("/dialogue").await;
        assert_eq!(app.dialogue.unwrap().title, "Test");
    }

    #[tokio::test]
    async fn registered_panels_open_and_receive_focus() {
        let mut app = App::new();
        app.register_panel(PanelSpec {
            name: "test",
            title: "test",
            draw: draw_test_panel,
            handle_key: ignore_key,
        });
        app.submit_line("/panel test").await;
        assert!(app.panel_open);
        assert_eq!(app.focus, Focus::Panel);
    }

    #[tokio::test]
    async fn sessions_keep_their_own_transcript_and_input() {
        let mut app = App::new();
        app.submit_line("first").await;
        app.new_session().await;
        app.active_session_mut().input.replace_line("draft".into());
        assert_eq!(app.sessions.len(), 2);
        app.cycle_session(false);
        assert!(app
            .active_session()
            .transcript
            .iter()
            .any(|line| line.text == "you> first"));
        assert!(app.active_session().input.is_empty());
        app.cycle_session(true);
        assert_eq!(app.active_session().input.line(), "draft");
    }

    #[tokio::test]
    async fn session_prefix_creates_cycles_and_closes_tabs() {
        let mut app = App::new();
        let ctrl_f = KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL);
        app.handle_event(Event::Key(ctrl_f)).await;
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::NONE,
        )))
        .await;
        assert_eq!(app.sessions.len(), 2);
        assert_eq!(app.session_index, 1);
        app.handle_event(Event::Key(ctrl_f)).await;
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('p'),
            KeyModifiers::NONE,
        )))
        .await;
        assert_eq!(app.session_index, 0);
        app.handle_event(Event::Key(ctrl_f)).await;
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('q'),
            KeyModifiers::NONE,
        )))
        .await;
        assert_eq!(app.sessions.len(), 1);
    }

    #[tokio::test]
    async fn alt_number_selects_a_session() {
        let mut app = App::new();
        app.new_session().await;
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('1'),
            KeyModifiers::ALT,
        )))
        .await;
        assert_eq!(app.session_index, 0);
    }

    #[tokio::test]
    async fn session_prefix_d_detaches() {
        let mut app = App::new();
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('f'),
            KeyModifiers::CONTROL,
        )))
        .await;
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('d'),
            KeyModifiers::NONE,
        )))
        .await;
        assert!(app.should_quit);
    }
}
