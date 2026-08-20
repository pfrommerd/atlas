//! Application state, command dispatch, and terminal event handling.

use std::collections::VecDeque;
use std::io;
use std::time::Duration;

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use ratatui::DefaultTerminal;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::client::{DaemonClient, PendingApproval};
use crate::input::InputBox;
use crate::ui;
use atlas_agent::{
    ApprovalOptionKind, ApprovalResponse, ContentBlock, Cursor, QueuedSubmission, ThreadEventKind,
    ThreadItem, ThreadListEvent, ThreadSummary, Turn, TurnStatus,
};

struct SessionPicker {
    sessions: Vec<ThreadSummary>,
    filter: String,
    filtering: bool,
    next_cursor: Option<Cursor>,
    index: usize,
    scroll: usize,
    loading: bool,
    error: Option<String>,
    action: Option<SessionPickerAction>,
}

enum SessionPickerAction {
    Refresh,
    LoadMore,
    Open(ThreadSummary),
}

pub struct Completion {
    pub replacement: String,
    pub label: String,
    pub description: String,
}

pub struct CommandContext<'a> {
    app: &'a mut App,
}

impl CommandContext<'_> {
    /// Show command feedback beside the session name.
    pub(crate) fn write(&mut self, kind: OutKind, text: &str) {
        self.app.set_status(kind, text);
    }

    /// Open a registered panel and give it focus.
    pub(crate) fn open_panel(&mut self, name: &str) {
        self.app.open_panel(name);
    }

    /// Open a bottom-anchored dialogue over the regular footer and main content.
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

/// A command-provided overlay anchored to the bottom of the terminal.
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
    Assistant,
    Thought,
    Tool,
    Error,
    Info,
}

pub struct OutLine {
    pub kind: OutKind,
    pub text: String,
}

fn content_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .map(|content| match content {
            ContentBlock::Text { text } => text.clone(),
            ContentBlock::Image { uri, .. } | ContentBlock::Resource { uri, .. } => uri.clone(),
            ContentBlock::Audio { .. } => "[audio]".into(),
        })
        .collect()
}

fn append_wrapped_lines(
    lines: &mut Vec<OutLine>,
    kind: OutKind,
    prefix: &str,
    text: &str,
    width: usize,
) {
    let prefix_width = UnicodeWidthStr::width(prefix);
    let content_width = width.saturating_sub(prefix_width).max(1);
    let mut first = true;

    for logical_line in text.split('\n') {
        let mut remaining = logical_line;
        loop {
            let line_prefix = if first { prefix } else { "  " };
            first = false;
            if UnicodeWidthStr::width(remaining) <= content_width {
                lines.push(OutLine {
                    kind,
                    text: format!("{line_prefix}{remaining}"),
                });
                break;
            }

            let mut end = 0;
            let mut used = 0;
            let mut word_boundary = None;
            for (index, character) in remaining.char_indices() {
                let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
                if used + character_width > content_width {
                    break;
                }
                used += character_width;
                end = index + character.len_utf8();
                if character.is_whitespace() {
                    word_boundary = Some(index);
                }
            }
            if end == 0 {
                end = remaining
                    .char_indices()
                    .nth(1)
                    .map(|(index, _)| index)
                    .unwrap_or(remaining.len());
            }
            let split = word_boundary
                .filter(|boundary| *boundary > 0)
                .unwrap_or(end);
            lines.push(OutLine {
                kind,
                text: format!("{line_prefix}{}", remaining[..split].trim_end()),
            });
            remaining = remaining[split..].trim_start_matches(char::is_whitespace);
        }
    }
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

/// UI state belonging to one Atlas thread/tab. Backend synchronization is added
/// at the boundary; the TUI never shares drafts or transcript position between tabs.
pub struct Session {
    pub id: String,
    pub label: String,
    pub name: String,
    pub cwd: String,
    pub additional_directories: Vec<String>,
    pub status: Option<OutLine>,
    remote_turns: Vec<Turn>,
    transcript_loaded: bool,
    transcript_loading: bool,
    transcript_watch: u64,
    transcript_revision: u64,
    older_cursor: Option<Cursor>,
    queued_prompts: Vec<QueuedSubmission>,
    queue_paused: bool,
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
            status: None,
            remote_turns: Vec::new(),
            transcript_loaded: false,
            transcript_loading: false,
            transcript_watch: 0,
            transcript_revision: 0,
            older_cursor: None,
            queued_prompts: Vec::new(),
            queue_paused: false,
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
    session_picker: Option<SessionPicker>,
    pub completions: Vec<Completion>,
    pub completion_index: usize,
    pub focus: Focus,
    pub transcript_height: usize,
    pub transcript_width: usize,
    input_auto_focused: bool,
    session_prefix: bool,
    daemon: Option<DaemonClient>,
    unsubscribe_sessions_on_exit: bool,
    delete_requested: bool,
    pending_approvals: VecDeque<PendingApproval>,
    pub should_quit: bool,
}

pub async fn run(
    mut terminal: DefaultTerminal,
    daemon: DaemonClient,
    sessions: Vec<ThreadSummary>,
) -> io::Result<()> {
    let mut app = App::with_daemon(daemon, sessions).await;
    let mut events = EventStream::new();
    while !app.should_quit {
        app.apply_daemon_events();
        if !app.sessions.is_empty()
            && app.daemon.is_some()
            && !app.active_session().transcript_loaded
            && !app.active_session().transcript_loading
        {
            app.load_active_transcript(false).await;
        }
        app.process_session_picker().await;
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
            session_picker: None,
            completions: Vec::new(),
            completion_index: 0,
            focus: Focus::Input,
            transcript_height: 0,
            transcript_width: 80,
            input_auto_focused: false,
            session_prefix: false,
            daemon: None,
            unsubscribe_sessions_on_exit: false,
            delete_requested: false,
            pending_approvals: VecDeque::new(),
            should_quit: false,
        };
        for command in builtin_commands() {
            app.register_command(command);
        }
        app.set_status(
            OutKind::Info,
            "Atlas — agent shell ready. /help for commands; Ctrl+D to exit.",
        );
        app
    }

    pub async fn with_daemon(daemon: DaemonClient, sessions: Vec<ThreadSummary>) -> Self {
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
            let session_id = session.id.clone();
            let cwd = session.cwd.clone();
            let additional_directories = session.additional_directories.clone();
            app.load_active_transcript(false).await;
            if let Err(error) = app
                .daemon
                .as_ref()
                .expect("daemon installed")
                .resume(session_id, cwd, additional_directories)
                .await
            {
                app.set_status(OutKind::Error, &format!("session resume failed: {error}"));
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

    pub fn set_status(&mut self, kind: OutKind, text: &str) {
        self.active_session_mut().status = Some(OutLine {
            kind,
            text: text.lines().collect::<Vec<_>>().join(" "),
        });
    }

    pub fn transcript_lines(&self) -> Vec<OutLine> {
        let mut lines = Vec::new();
        let session = self.active_session();
        let width = self.transcript_width;
        for item in session.remote_turns.iter().flat_map(|turn| &turn.items) {
            match item {
                ThreadItem::UserMessage { content, .. }
                | ThreadItem::AgentMessage { content, .. }
                | ThreadItem::Reasoning { content, .. } => {
                    let text = content_text(content);
                    if !text.is_empty() {
                        let (kind, prefix) = match item {
                            ThreadItem::UserMessage { .. } => (OutKind::Input, "› "),
                            ThreadItem::AgentMessage { .. } => (OutKind::Assistant, "• "),
                            _ => (OutKind::Thought, "• "),
                        };
                        append_wrapped_lines(&mut lines, kind, prefix, &text, width);
                    }
                }
                ThreadItem::ToolCall {
                    title,
                    status,
                    content,
                    ..
                } => {
                    append_wrapped_lines(
                        &mut lines,
                        OutKind::Tool,
                        "• ",
                        &format!("{title} ({status:?})"),
                        width,
                    );
                    for block in content {
                        let text = content_text(std::slice::from_ref(block));
                        append_wrapped_lines(&mut lines, OutKind::Tool, "  ", &text, width);
                    }
                }
                ThreadItem::Plan { text, .. } => {
                    append_wrapped_lines(&mut lines, OutKind::Thought, "• ", text, width)
                }
                ThreadItem::CommandExecution {
                    command,
                    status,
                    output,
                    ..
                } => append_wrapped_lines(
                    &mut lines,
                    OutKind::Tool,
                    "• ",
                    &format!("{command} ({status:?})\n{output}"),
                    width,
                ),
                ThreadItem::FileChange {
                    changes, status, ..
                } => append_wrapped_lines(
                    &mut lines,
                    OutKind::Tool,
                    "• ",
                    &format!("{} file changes ({status:?})", changes.len()),
                    width,
                ),
                ThreadItem::Terminal {
                    title,
                    status,
                    output,
                    ..
                } => append_wrapped_lines(
                    &mut lines,
                    OutKind::Tool,
                    "• ",
                    &format!("{title} ({status:?})\n{output}"),
                    width,
                ),
            }
        }
        if !session.queued_prompts.is_empty() {
            lines.push(OutLine {
                kind: OutKind::Info,
                text: if session.queue_paused {
                    "Queued messages (paused):".into()
                } else {
                    "Queued messages:".into()
                },
            });
            for prompt in &session.queued_prompts {
                append_wrapped_lines(
                    &mut lines,
                    OutKind::Info,
                    "└ ",
                    &content_text(&prompt.input),
                    width,
                );
            }
        }
        lines
    }

    async fn load_active_transcript(&mut self, older: bool) {
        let Some(daemon) = self.daemon.clone() else {
            return;
        };
        let line_count_before = self.transcript_lines().len();
        let (session_id, before, preserve_scroll) = {
            let session = self.active_session_mut();
            if session.transcript_loading || (session.transcript_loaded && !older) {
                return;
            }
            if older && session.older_cursor.is_none() {
                return;
            }
            session.transcript_loading = true;
            session.status = Some(OutLine {
                kind: OutKind::Info,
                text: "Loading transcript…".into(),
            });
            (
                session.id.clone(),
                session.older_cursor.clone(),
                older && session.transcript_loaded && !session.scroll.stick,
            )
        };
        let result = if older {
            daemon
                .thread_history(session_id, before.expect("older cursor checked"))
                .await
                .map(|page| (page.thread.turns, page.older_cursor, None))
        } else {
            daemon
                .watch_thread(session_id)
                .await
                .map(|(snapshot, watch, queue)| {
                    (
                        snapshot.thread.turns,
                        snapshot.older_cursor,
                        Some((watch, snapshot.revision, queue)),
                    )
                })
        };
        match result {
            Ok((items, older_cursor, snapshot)) => {
                {
                    let session = self.active_session_mut();
                    if older {
                        let mut turns = items;
                        turns.append(&mut session.remote_turns);
                        session.remote_turns = turns;
                    } else {
                        session.remote_turns = items;
                        let (watch, revision, queue) =
                            snapshot.expect("new watches return a snapshot");
                        session.transcript_watch = watch;
                        session.transcript_revision = revision;
                        session.queue_paused = !queue.is_empty()
                            && !session
                                .remote_turns
                                .iter()
                                .any(|turn| turn.status == TurnStatus::InProgress);
                        session.queued_prompts = queue;
                    }
                    session.older_cursor = older_cursor;
                    session.transcript_loaded = true;
                    session.transcript_loading = false;
                    session.status = None;
                }
                if preserve_scroll {
                    let added_lines = self
                        .transcript_lines()
                        .len()
                        .saturating_sub(line_count_before);
                    self.active_session_mut().scroll.offset += added_lines;
                }
            }
            Err(error) if older && error.contains("stale transcript cursor") => {
                let session = self.active_session_mut();
                session.remote_turns.clear();
                session.transcript_loaded = false;
                session.transcript_loading = false;
                session.older_cursor = None;
                self.set_status(
                    OutKind::Info,
                    "Transcript changed; load the newest page again.",
                );
            }
            Err(error) => {
                self.active_session_mut().transcript_loading = false;
                self.set_status(OutKind::Error, &format!("transcript load failed: {error}"));
            }
        }
    }

    pub async fn submit_line(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        match line.strip_prefix('/') {
            Some(cmd) if cmd.trim() == "resume" => {
                if let Some(daemon) = self.daemon.clone() {
                    let session_id = self.active_session().id.clone();
                    match daemon.queue_start(session_id, None).await {
                        Ok(_) => {
                            self.active_session_mut().queue_paused = false;
                            self.set_status(OutKind::Info, "Prompt queue resumed.")
                        }
                        Err(error) => self
                            .set_status(OutKind::Error, &format!("queue resume failed: {error}")),
                    }
                } else {
                    self.set_status(OutKind::Info, "Agent backend is not configured yet.");
                }
            }
            Some(cmd) if cmd.trim() == "archive" => self.archive_session().await,
            Some(cmd) if cmd.trim() == "delete" => {
                self.open_dialogue(DialogueSpec {
                    title: "Delete session?",
                    title_style: Style::new().fg(Color::Red),
                    height: 4,
                    draw: draw_delete_dialogue,
                    handle_key: delete_dialogue_key,
                });
            }
            Some(cmd) => {
                self.dispatch_command(cmd);
                self.process_session_picker().await;
            }
            None => {
                if let Some(daemon) = self.daemon.clone() {
                    let session_id = self.active_session().id.clone();
                    let queue = self.active_session().queue_paused
                        || self
                            .active_session()
                            .remote_turns
                            .iter()
                            .any(|turn| turn.status == TurnStatus::InProgress);
                    if let Err(error) = daemon.prompt(session_id, line.to_string(), queue).await {
                        self.set_status(OutKind::Error, &format!("prompt failed: {error}"));
                    }
                } else {
                    self.set_status(
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
            match daemon.new_thread(cwd.clone()).await {
                Ok(id) => {
                    if was_empty {
                        self.sessions[0] = Session::from_id(id.clone());
                    } else {
                        self.sessions.push(Session::from_id(id.clone()));
                        self.session_index = self.sessions.len() - 1;
                    }
                    self.load_active_transcript(false).await;
                    if let Err(error) = daemon.resume(id, cwd, Vec::new()).await {
                        self.set_status(OutKind::Error, &format!("session resume failed: {error}"));
                    }
                }
                Err(error) => {
                    self.set_status(OutKind::Error, &format!("new session failed: {error}"))
                }
            }
            return;
        }
        let number = self.sessions.len() + 1;
        self.sessions.push(Session::new(number));
        self.session_index = self.sessions.len() - 1;
        self.set_status(OutKind::Info, "New session ready.");
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
            if let Err(error) = daemon.unsubscribe(session_id).await {
                self.set_status(OutKind::Error, &format!("unsubscribe failed: {error}"));
                return;
            }
        }
        self.remove_active_session();
    }

    fn remove_active_session(&mut self) {
        if self.sessions.len() == 1 {
            self.should_quit = true;
            return;
        }
        self.sessions.remove(self.session_index);
        self.session_index = self.session_index.min(self.sessions.len() - 1);
    }

    async fn archive_session(&mut self) {
        let Some(daemon) = self.daemon.clone() else {
            self.set_status(OutKind::Info, "Agent backend is not configured yet.");
            return;
        };
        let session_id = self.active_session().id.clone();
        match daemon.archive(session_id).await {
            Ok(()) => self.remove_active_session(),
            Err(error) => self.set_status(OutKind::Error, &format!("archive failed: {error}")),
        }
    }

    async fn delete_session(&mut self) {
        let Some(daemon) = self.daemon.clone() else {
            self.set_status(OutKind::Info, "Agent backend is not configured yet.");
            return;
        };
        let session_id = self.active_session().id.clone();
        match daemon.delete(session_id).await {
            Ok(()) => self.remove_active_session(),
            Err(error) => self.set_status(OutKind::Error, &format!("delete failed: {error}")),
        }
    }

    fn quit(&mut self) {
        self.unsubscribe_sessions_on_exit = true;
        self.should_quit = true;
    }

    async fn shutdown(&mut self) {
        if self.unsubscribe_sessions_on_exit
            && let Some(daemon) = self.daemon.clone()
        {
            for session in &self.sessions {
                let _ = daemon.unsubscribe(session.id.clone()).await;
            }
        }
    }

    fn upsert_session(&mut self, session: ThreadSummary) {
        let name = session.title.unwrap_or_else(|| "Untitled session".into());
        if let Some(existing) = self
            .sessions
            .iter_mut()
            .find(|existing| existing.id == session.id.0)
        {
            existing.name = name;
            existing.cwd = session.cwd;
            existing.additional_directories = session.additional_directories;
        } else {
            let mut tab = Session::from_id(session.id.0);
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
        self.pending_approvals.extend(daemon.drain_approvals());
        if !self.pending_approvals.is_empty() && self.dialogue.is_none() {
            self.open_dialogue(DialogueSpec {
                title: "Approval",
                title_style: Style::new().fg(Color::Yellow),
                height: 10,
                draw: draw_approval_dialogue,
                handle_key: approval_dialogue_key,
            });
        }
        for event in daemon.drain_events() {
            match event {
                ThreadListEvent::Added { thread } | ThreadListEvent::Updated { thread } => {
                    self.upsert_session(thread);
                }
                ThreadListEvent::Removed { thread_id }
                | ThreadListEvent::Archived { thread_id }
                | ThreadListEvent::Deleted { thread_id } => {
                    if let Some(index) = self
                        .sessions
                        .iter()
                        .position(|session| session.id == thread_id.0)
                    {
                        self.sessions.remove(index);
                        if self.sessions.is_empty() {
                            self.should_quit = true;
                        } else {
                            self.session_index = self.session_index.min(self.sessions.len() - 1);
                        }
                    }
                }
                ThreadListEvent::BackendUnavailable { backend, message } => self.set_status(
                    OutKind::Error,
                    &format!("backend {backend} unavailable: {message}"),
                ),
            }
        }
        for (session_id, watch, event) in daemon.drain_thread_events() {
            if let Some(session) = self
                .sessions
                .iter_mut()
                .find(|session| session.id == session_id.0)
            {
                if watch != session.transcript_watch {
                    continue;
                }
                if event.revision != session.transcript_revision.wrapping_add(1) {
                    session.transcript_loaded = false;
                    session.status = Some(OutLine {
                        kind: OutKind::Error,
                        text: "Session event gap detected; resynchronizing…".into(),
                    });
                    continue;
                }
                session.transcript_revision = event.revision;
                match event.event {
                    ThreadEventKind::TurnStarted { turn }
                    | ThreadEventKind::TurnCompleted { turn }
                    | ThreadEventKind::TurnFailed { turn } => {
                        if turn.status == TurnStatus::Interrupted
                            && !session.queued_prompts.is_empty()
                        {
                            session.queue_paused = true;
                        }
                        if let Some(index) = session
                            .remote_turns
                            .iter()
                            .position(|entry| entry.id == turn.id)
                        {
                            session.remote_turns[index] = turn;
                        } else {
                            session.remote_turns.push(turn);
                        }
                    }
                    ThreadEventKind::ItemStarted { turn_id, item }
                    | ThreadEventKind::ItemUpdated { turn_id, item }
                    | ThreadEventKind::ItemCompleted { turn_id, item } => {
                        if let Some(turn) = session
                            .remote_turns
                            .iter_mut()
                            .find(|turn| turn.id == turn_id)
                        {
                            if let Some(index) =
                                turn.items.iter().position(|entry| entry.id() == item.id())
                            {
                                turn.items[index] = item;
                            } else {
                                turn.items.push(item);
                            }
                        }
                    }
                    ThreadEventKind::QueueChanged { .. } => {
                        session.transcript_loaded = false;
                    }
                    ThreadEventKind::Error { message } => {
                        session.status = Some(OutLine {
                            kind: OutKind::Error,
                            text: message,
                        });
                    }
                    ThreadEventKind::ThreadUpdated { thread } => {
                        session.name = thread.title.unwrap_or_else(|| "Untitled session".into());
                        session.cwd = thread.cwd;
                        session.additional_directories = thread.additional_directories;
                    }
                    ThreadEventKind::UsageUpdated { .. } => {}
                }
            }
        }
    }

    fn dispatch_command(&mut self, cmd: &str) {
        let name = cmd.split_whitespace().next().unwrap_or_default();
        let Some(command) = self
            .commands
            .iter()
            .find(|command| command.name == name || command.aliases.contains(&name))
        else {
            self.set_status(
                OutKind::Error,
                &format!("unknown command: /{name} (try /help)"),
            );
            return;
        };
        (command.execute)(&mut CommandContext { app: self }, cmd);
    }

    fn open_panel(&mut self, name: &str) {
        let Some(index) = self.panels.iter().position(|panel| panel.name == name) else {
            self.set_status(OutKind::Error, &format!("unknown panel: {name}"));
            return;
        };
        self.panel_open = true;
        self.panel_index = index;
        self.focus = Focus::Panel;
    }

    pub fn toggle_panel(&mut self) {
        if self.panels.is_empty() {
            self.set_status(OutKind::Info, "No panels are registered.");
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

    fn open_session_picker(&mut self) {
        self.session_picker = Some(SessionPicker {
            sessions: Vec::new(),
            filter: String::new(),
            filtering: false,
            next_cursor: None,
            index: 0,
            scroll: 0,
            loading: true,
            error: None,
            action: Some(SessionPickerAction::Refresh),
        });
        self.open_dialogue(DialogueSpec {
            title: "Sessions",
            title_style: Style::new().fg(Color::Cyan),
            height: 12,
            draw: draw_sessions_dialogue,
            handle_key: session_picker_key,
        });
    }

    async fn process_session_picker(&mut self) {
        let action = self
            .session_picker
            .as_mut()
            .and_then(|picker| picker.action.take());
        let Some(action) = action else { return };
        match action {
            SessionPickerAction::Refresh | SessionPickerAction::LoadMore => {
                let Some(daemon) = self.daemon.clone() else {
                    if let Some(picker) = &mut self.session_picker {
                        picker.loading = false;
                        picker.error = Some("session picker requires a daemon connection".into());
                    }
                    return;
                };
                let (filter, cursor, replace) = {
                    let picker = self
                        .session_picker
                        .as_ref()
                        .expect("picker action requires picker");
                    (
                        if picker.filter.is_empty() {
                            None
                        } else {
                            Some(picker.filter.clone())
                        },
                        if matches!(action, SessionPickerAction::Refresh) {
                            None
                        } else {
                            picker.next_cursor.clone()
                        },
                        matches!(action, SessionPickerAction::Refresh),
                    )
                };
                match daemon.list_threads(filter, cursor).await {
                    Ok((sessions, next_cursor)) => {
                        if let Some(picker) = &mut self.session_picker {
                            if replace {
                                picker.sessions = sessions;
                                picker.index = 0;
                                picker.scroll = 0;
                            } else {
                                picker.sessions.extend(sessions);
                            }
                            if !replace && picker.index + 1 < picker.sessions.len() {
                                picker.index += 1;
                            }
                            picker.next_cursor = next_cursor;
                            picker.loading = false;
                            picker.error = None;
                        }
                    }
                    Err(error) => {
                        if let Some(picker) = &mut self.session_picker {
                            if !replace && error == "stale session list cursor" {
                                picker.action = Some(SessionPickerAction::Refresh);
                            } else {
                                picker.loading = false;
                                picker.error = Some(error);
                            }
                        }
                    }
                }
            }
            SessionPickerAction::Open(session) => {
                self.dialogue = None;
                self.session_picker = None;
                if let Some(index) = self.sessions.iter().position(|tab| tab.id == session.id.0) {
                    self.session_index = index;
                    self.load_active_transcript(false).await;
                    return;
                }
                let Some(daemon) = self.daemon.clone() else {
                    self.set_status(
                        OutKind::Error,
                        "session picker requires a daemon connection",
                    );
                    return;
                };
                self.upsert_session(session.clone());
                if let Some(index) = self.sessions.iter().position(|tab| tab.id == session.id.0) {
                    self.session_index = index;
                    self.load_active_transcript(false).await;
                }
                match daemon
                    .resume(
                        session.id.0.clone(),
                        session.cwd.clone(),
                        session.additional_directories.clone(),
                    )
                    .await
                {
                    Ok(()) => {}
                    Err(error) => {
                        self.set_status(OutKind::Error, &format!("session resume failed: {error}"))
                    }
                }
            }
        }
    }

    pub async fn handle_event(&mut self, event: Event) {
        let Event::Key(key) = event else { return };
        if key.kind == KeyEventKind::Release {
            return;
        }
        if self.session_prefix {
            self.session_prefix = false;
            match key.code {
                KeyCode::Char('n') => {
                    self.cycle_session(true);
                    self.load_active_transcript(false).await;
                }
                KeyCode::Char('p') => {
                    self.cycle_session(false);
                    self.load_active_transcript(false).await;
                }
                KeyCode::Char('c') => self.new_session().await,
                KeyCode::Char('q') => self.close_session().await,
                KeyCode::Char('d') => self.should_quit = true,
                _ => {}
            }
            return;
        }
        if key.modifiers == KeyModifiers::ALT
            && let KeyCode::Char(number @ '1'..='9') = key.code
        {
            let index = number as usize - '1' as usize;
            if index < self.sessions.len() {
                self.session_index = index;
                self.load_active_transcript(false).await;
            }
            return;
        }
        if key.code == KeyCode::Char('f') && key.modifiers == KeyModifiers::CONTROL {
            self.session_prefix = true;
            return;
        }
        if self.dialogue.is_some() {
            if !self.pending_approvals.is_empty() {
                approval_dialogue_key(self, key);
                return;
            }
            match (key.code, key.modifiers) {
                (KeyCode::Esc, _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                    self.dialogue = None;
                    self.session_picker = None;
                }
                (KeyCode::Char('/'), _) => {
                    self.dialogue = None;
                    self.focus = Focus::Input;
                    self.input_auto_focused = false;
                    self.active_session_mut().input.handle_key(key);
                    self.refresh_completions();
                }
                _ => {
                    (self.dialogue.expect("dialogue checked above").handle_key)(self, key);
                    self.process_session_picker().await;
                    if std::mem::take(&mut self.delete_requested) {
                        self.delete_session().await;
                    }
                }
            }
            return;
        }
        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                if let Some(daemon) = self.daemon.clone() {
                    let id = self.active_session().id.clone();
                    if let Err(error) = daemon.interrupt(id).await {
                        self.set_status(OutKind::Error, &format!("interrupt failed: {error}"));
                    }
                }
            }
            (KeyCode::Char('b'), KeyModifiers::CONTROL) => self.toggle_panel(),
            (KeyCode::Tab, _) if self.panel_open => {
                self.input_auto_focused = false;
                self.focus = match self.focus {
                    Focus::Input => Focus::Panel,
                    Focus::Panel => Focus::Input,
                };
            }
            (KeyCode::PageUp, _) => {
                let at_start = self.active_session().scroll.offset == 0;
                self.active_session_mut().scroll.stick = false;
                self.active_session_mut().scroll.offset = self
                    .active_session()
                    .scroll
                    .offset
                    .saturating_sub(self.transcript_height.max(1));
                if at_start {
                    self.load_active_transcript(true).await;
                }
            }
            (KeyCode::PageDown, _) => {
                let page = self.transcript_height.max(1);
                let max = self.transcript_lines().len().saturating_sub(page);
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
        } else if let Some(command) = self
            .commands
            .iter()
            .find(|command| command.name == name || command.aliases.contains(&name))
        {
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
            height: 14,
            draw: draw_help_dialogue,
            handle_key: ignore_dialogue_key,
        }),
        Some("sessions") => ctx.app.open_session_picker(),
        Some("panel") => match cmd.split_whitespace().nth(1) {
            Some(name) => ctx.open_panel(name),
            None => ctx.app.toggle_panel(),
        },
        Some("resume") => ctx.write(OutKind::Info, "Use /resume from the prompt."),
        Some("archive") => ctx.write(OutKind::Info, "Use /archive from the prompt."),
        Some("delete") => ctx.write(OutKind::Info, "Use /delete from the prompt."),
        Some("quit") | Some("exit") => ctx.quit(),
        Some("detach") => ctx.detach(),
        _ => ctx.write(OutKind::Error, "unknown built-in command"),
    }
}

fn ignore_dialogue_key(_: &mut App, _: KeyEvent) {}

fn draw_delete_dialogue(f: &mut ratatui::Frame, app: &mut App, area: Rect) {
    let name = &app.active_session().name;
    f.render_widget(
        Paragraph::new(vec![
            Line::from(format!(" Permanently delete {name}?")),
            Line::styled(" y delete · n/Esc cancel", Style::new().fg(Color::DarkGray)),
        ]),
        area,
    );
}

fn delete_dialogue_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char('y') => {
            app.delete_requested = true;
            app.dialogue = None;
        }
        KeyCode::Char('n') | KeyCode::Esc => app.dialogue = None,
        _ => {}
    }
}

fn session_picker_key(app: &mut App, key: KeyEvent) {
    let Some(picker) = &mut app.session_picker else {
        return;
    };
    let move_down = |picker: &mut SessionPicker| {
        if picker.index + 1 < picker.sessions.len() {
            picker.index += 1;
        } else if picker.next_cursor.is_some() && !picker.loading {
            picker.loading = true;
            picker.action = Some(SessionPickerAction::LoadMore);
        }
    };
    match key.code {
        KeyCode::Char('f') if !picker.filtering && key.modifiers.is_empty() => {
            picker.filtering = true;
        }
        KeyCode::Up | KeyCode::Char('l') if !picker.filtering => {
            picker.index = picker.index.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('k') if !picker.filtering => move_down(picker),
        KeyCode::Up => picker.index = picker.index.saturating_sub(1),
        KeyCode::Down => move_down(picker),
        KeyCode::Enter if !picker.loading => {
            if let Some(session) = picker.sessions.get(picker.index).cloned() {
                picker.action = Some(SessionPickerAction::Open(session));
            }
        }
        KeyCode::Backspace if picker.filtering => {
            if picker.filter.pop().is_some() {
                picker.loading = true;
                picker.action = Some(SessionPickerAction::Refresh);
            }
        }
        KeyCode::Char(character)
            if picker.filtering
                && (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT) =>
        {
            picker.filter.push(character);
            picker.loading = true;
            picker.action = Some(SessionPickerAction::Refresh);
        }
        _ => {}
    }
    picker.index = picker.index.min(picker.sessions.len().saturating_sub(1));
    if picker.index < picker.scroll {
        picker.scroll = picker.index;
    }
    if picker.index >= picker.scroll + 10 {
        picker.scroll = picker.index + 1 - 10;
    }
}

fn draw_sessions_dialogue(f: &mut ratatui::Frame, app: &mut App, area: Rect) {
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{List, ListItem, ListState, Paragraph};

    let Some(picker) = app.session_picker.as_ref() else {
        return;
    };
    let (filter_area, list_area) = if picker.filtering {
        let [filter_area, list_area] = ratatui::layout::Layout::vertical([
            ratatui::layout::Constraint::Length(1),
            ratatui::layout::Constraint::Min(0),
        ])
        .areas(area);
        (Some(filter_area), list_area)
    } else {
        (None, area)
    };
    let status = if picker.loading { " loading…" } else { "" };
    if let Some(filter_area) = filter_area {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" Filter: ", Style::new().fg(Color::Yellow)),
                Span::raw(&picker.filter),
                Span::styled(status, Style::new().fg(Color::DarkGray)),
            ])),
            filter_area,
        );
    }
    if picker.loading && !picker.filtering {
        f.render_widget(Paragraph::new(" Loading sessions…"), list_area);
        return;
    }
    if let Some(error) = &picker.error {
        f.render_widget(Paragraph::new(format!(" {error}")), list_area);
        return;
    }
    if picker.sessions.is_empty() && !picker.loading {
        f.render_widget(Paragraph::new(" No sessions found."), list_area);
        return;
    }
    let items = picker
        .sessions
        .iter()
        .enumerate()
        .skip(picker.scroll)
        .take(10)
        .map(|(index, session)| {
            let active = session.status == atlas_agent::ThreadStatus::Active;
            let state = if active { "active" } else { "previous" };
            let title = session.title.as_deref().unwrap_or("Untitled session");
            let updated = session
                .updated_at
                .as_deref()
                .map(|value| format!("  {value}"))
                .unwrap_or_default();
            let row_style = if active {
                Style::new()
            } else {
                Style::new().fg(Color::DarkGray)
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    if index == picker.index {
                        " › "
                    } else {
                        "   "
                    },
                    Style::new().fg(Color::Cyan),
                ),
                Span::styled(
                    format!("{title}  [{state}]  {}{updated}", session.cwd),
                    row_style,
                ),
            ]))
        })
        .collect::<Vec<_>>();
    let list = List::new(items);
    let mut state = ListState::default();
    state.select((picker.index >= picker.scroll).then_some(picker.index - picker.scroll));
    f.render_stateful_widget(list, list_area, &mut state);
}

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

fn draw_approval_dialogue(f: &mut ratatui::Frame, app: &mut App, area: Rect) {
    use ratatui::text::{Line, Span};
    use ratatui::widgets::Paragraph;

    let Some(pending) = app.pending_approvals.front() else {
        return;
    };
    let mut lines = vec![Line::styled(
        format!(" {}", pending.request.title),
        Style::new().fg(Color::Yellow),
    )];
    if let Some(description) = &pending.request.description {
        lines.push(Line::raw(format!(" {description}")));
    }
    lines.push(Line::raw(""));
    lines.extend(
        pending
            .request
            .options
            .iter()
            .enumerate()
            .map(|(index, option)| {
                Line::from(vec![
                    Span::styled(format!(" {}. ", index + 1), Style::new().fg(Color::Cyan)),
                    Span::raw(&option.label),
                ])
            }),
    );
    lines.push(Line::styled(
        " Choose 1-9 · y allow · n reject · Esc cancel",
        Style::new().fg(Color::DarkGray),
    ));
    f.render_widget(Paragraph::new(lines), area);
}

fn approval_dialogue_key(app: &mut App, key: KeyEvent) {
    let selected = app
        .pending_approvals
        .front()
        .and_then(|pending| match key.code {
            KeyCode::Char(number @ '1'..='9') => pending
                .request
                .options
                .get(number as usize - '1' as usize)
                .map(|option| option.id.clone()),
            KeyCode::Char('y') => pending
                .request
                .options
                .iter()
                .find(|option| {
                    matches!(
                        option.kind,
                        ApprovalOptionKind::AllowOnce | ApprovalOptionKind::AllowAlways
                    )
                })
                .map(|option| option.id.clone()),
            KeyCode::Char('n') => pending
                .request
                .options
                .iter()
                .find(|option| {
                    matches!(
                        option.kind,
                        ApprovalOptionKind::RejectOnce | ApprovalOptionKind::RejectAlways
                    )
                })
                .map(|option| option.id.clone()),
            _ => None,
        });
    let cancelled = matches!(key.code, KeyCode::Esc)
        || key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL;
    let Some(response) = selected
        .map(|option_id| ApprovalResponse::Selected { option_id })
        .or(cancelled.then_some(ApprovalResponse::Cancelled))
    else {
        return;
    };
    if let Some(pending) = app.pending_approvals.pop_front() {
        pending.respond(response);
    }
    app.dialogue = None;
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
            name: "sessions",
            aliases: &[],
            description: "browse and open sessions",
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
            name: "resume",
            aliases: &[],
            description: "resume a paused prompt queue",
            execute: run_builtin,
            complete: no_completions,
        },
        CommandSpec {
            name: "archive",
            aliases: &[],
            description: "archive the current session",
            execute: run_builtin,
            complete: no_completions,
        },
        CommandSpec {
            name: "delete",
            aliases: &[],
            description: "permanently delete the current session",
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
            app.active_session().status.as_ref().unwrap().text,
            "Agent backend is not configured yet. Your prompt was received."
        );
        assert!(app.transcript_lines().is_empty());
    }

    #[tokio::test]
    async fn no_panels_are_safe_to_toggle() {
        let mut app = App::new();
        app.toggle_panel();
        assert!(!app.panel_open);
        assert_eq!(
            app.active_session().status.as_ref().unwrap().text,
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
        app.active_session_mut()
            .input
            .replace_line("/s".to_string());
        app.refresh_completions();
        assert!(app.completions.iter().any(|item| item.label == "/sessions"));
    }

    #[tokio::test]
    async fn sessions_command_opens_a_picker() {
        let mut app = App::new();
        app.submit_line("/sessions").await;
        assert_eq!(app.dialogue.unwrap().title, "Sessions");
        assert!(app.session_picker.is_some());
    }

    #[test]
    fn session_picker_filters_and_navigates_loaded_sessions() {
        let mut app = App::new();
        app.session_picker = Some(SessionPicker {
            sessions: vec![
                ThreadSummary {
                    id: atlas_agent::ThreadId("one".into()),
                    backend: atlas_agent::BackendId("test".into()),
                    cwd: "/one".into(),
                    additional_directories: Vec::new(),
                    title: Some("One".into()),
                    updated_at: None,
                    status: atlas_agent::ThreadStatus::Idle,
                },
                ThreadSummary {
                    id: atlas_agent::ThreadId("two".into()),
                    backend: atlas_agent::BackendId("test".into()),
                    cwd: "/two".into(),
                    additional_directories: Vec::new(),
                    title: Some("Two".into()),
                    updated_at: None,
                    status: atlas_agent::ThreadStatus::Idle,
                },
            ],
            filter: String::new(),
            filtering: false,
            next_cursor: None,
            index: 0,
            scroll: 0,
            loading: false,
            error: None,
            action: None,
        });
        session_picker_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
        );
        assert_eq!(app.session_picker.as_ref().unwrap().index, 1);
        session_picker_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE),
        );
        assert!(app.session_picker.as_ref().unwrap().filtering);
        session_picker_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE),
        );
        let picker = app.session_picker.as_ref().unwrap();
        assert_eq!(picker.filter, "t");
        assert!(matches!(picker.action, Some(SessionPickerAction::Refresh)));
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
            app.active_session().status.as_ref().unwrap().text,
            "custom command ran"
        );
        assert!(app.transcript_lines().is_empty());
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
    async fn sessions_keep_their_own_status_and_input() {
        let mut app = App::new();
        app.submit_line("first").await;
        app.new_session().await;
        app.active_session_mut().input.replace_line("draft".into());
        assert_eq!(app.sessions.len(), 2);
        app.cycle_session(false);
        assert_eq!(
            app.active_session().status.as_ref().unwrap().text,
            "Agent backend is not configured yet. Your prompt was received."
        );
        assert!(app.transcript_lines().is_empty());
        assert!(app.active_session().input.is_empty());
        app.cycle_session(true);
        assert_eq!(app.active_session().input.line(), "draft");
    }

    #[test]
    fn structured_updates_render_like_a_codex_transcript() {
        let mut app = App::new();
        app.active_session_mut().remote_turns.push(Turn {
            id: atlas_agent::TurnId("turn".into()),
            status: atlas_agent::TurnStatus::Completed,
            stop_reason: None,
            error: None,
            items: vec![
                ThreadItem::UserMessage {
                    id: atlas_agent::ItemId("user".into()),
                    content: vec![ContentBlock::Text {
                        text: "hello".into(),
                    }],
                },
                ThreadItem::AgentMessage {
                    id: atlas_agent::ItemId("agent".into()),
                    content: vec![ContentBlock::Text { text: "hi".into() }],
                },
                ThreadItem::ToolCall {
                    id: atlas_agent::ItemId("tool".into()),
                    title: "Ran cargo test".into(),
                    status: atlas_agent::ItemStatus::Completed,
                    content: Vec::new(),
                },
            ],
        });

        let lines = app.transcript_lines();
        assert!(
            lines
                .iter()
                .any(|line| line.text == "› hello" && line.kind == OutKind::Input)
        );
        assert!(
            lines
                .iter()
                .any(|line| line.text == "• hi" && line.kind == OutKind::Assistant)
        );
        assert!(
            lines
                .iter()
                .any(|line| line.text == "• Ran cargo test (Completed)"
                    && line.kind == OutKind::Tool)
        );
        assert!(!lines.iter().any(|line| line.text.contains("future_update")));
    }

    #[test]
    fn queued_messages_render_below_the_transcript() {
        let mut app = App::new();
        app.transcript_width = 40;
        let session = app.active_session_mut();
        session.remote_turns.push(Turn {
            id: atlas_agent::TurnId("active".into()),
            status: atlas_agent::TurnStatus::InProgress,
            stop_reason: None,
            error: None,
            items: vec![ThreadItem::AgentMessage {
                id: atlas_agent::ItemId("agent".into()),
                content: vec![ContentBlock::Text {
                    text: "working".into(),
                }],
            }],
        });
        session.queued_prompts = vec![QueuedSubmission {
            id: atlas_agent::QueuedSubmissionId("queued".into()),
            input: vec![ContentBlock::Text {
                text: "follow up".into(),
            }],
            client_user_message_id: "queued-user".into(),
        }];

        let lines = app.transcript_lines();
        assert_eq!(lines[0].text, "• working");
        assert_eq!(lines[1].text, "Queued messages:");
        assert_eq!(lines[2].text, "└ follow up");
        assert_eq!(lines[2].kind, OutKind::Info);
    }

    #[test]
    fn paused_queue_is_labeled_and_kept_out_of_transcript_state() {
        let mut app = App::new();
        let session = app.active_session_mut();
        session.queued_prompts = vec![QueuedSubmission {
            id: atlas_agent::QueuedSubmissionId("queued".into()),
            input: vec![ContentBlock::Text {
                text: "wait".into(),
            }],
            client_user_message_id: "queued-user".into(),
        }];
        session.queue_paused = true;

        let lines = app.transcript_lines();
        assert_eq!(lines[0].text, "Queued messages (paused):");
        assert_eq!(lines[1].text, "└ wait");
        assert!(app.active_session().remote_turns.is_empty());
    }

    #[test]
    fn transcript_rows_use_hanging_indents_for_hard_and_soft_wraps() {
        let mut app = App::new();
        app.transcript_width = 10;
        app.active_session_mut().remote_turns.push(Turn {
            id: atlas_agent::TurnId("turn".into()),
            status: atlas_agent::TurnStatus::Completed,
            stop_reason: None,
            error: None,
            items: vec![ThreadItem::UserMessage {
                id: atlas_agent::ItemId("user".into()),
                content: vec![ContentBlock::Text {
                    text: "hello world\nagain".into(),
                }],
            }],
        });

        let lines = app.transcript_lines();
        assert_eq!(
            lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            vec!["› hello", "  world", "  again"]
        );
    }

    #[test]
    fn transcript_wrapping_measures_unicode_display_width() {
        let mut lines = Vec::new();
        append_wrapped_lines(&mut lines, OutKind::Assistant, "• ", "界界界界界", 10);
        assert_eq!(
            lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            vec!["• 界界界界", "  界"]
        );
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

    #[tokio::test]
    async fn delete_requires_confirmation() {
        let mut app = App::new();
        app.submit_line("/delete").await;
        assert_eq!(
            app.dialogue.map(|dialogue| dialogue.title),
            Some("Delete session?")
        );

        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
            .await;
        assert!(app.dialogue.is_none());
        assert!(!app.delete_requested);
    }
}
