//! Application state and the event loop: dispatches keys, drives evaluation
//! slices between redraws, and turns session/eval events into transcript lines.

use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::DefaultTerminal;

use atlas_core::vm::heap::{HeapScope, TermPtr};
use atlas_core::vm::printer::Printer;

use crate::eval::{self, EvalEvent, EvalState};
use crate::explorer::{ExplorerState, RootEntry};
use crate::input::InputBox;
use crate::session::{LangMode, Session, SubmitResult};
use crate::ui;
use crate::Args;

pub struct Completion {
    pub replacement: String,
    pub label: String,
    pub description: String,
}

pub struct CommandContext<'a, 'h> {
    app: &'a mut App<'h>,
}

impl<'a, 'h> CommandContext<'a, 'h> {
    /// Append text to the REPL transcript.
    pub(crate) fn write(&mut self, kind: OutKind, text: &str) {
        self.app.push(kind, text);
    }

    /// Open a registered panel and give it focus.
    pub(crate) fn open_panel(&mut self, name: &str) {
        self.app.open_panel(name);
    }

    /// Request application shutdown.
    pub(crate) fn quit(&mut self) {
        self.app.should_quit = true;
    }

    fn run_builtin(&mut self, cmd: &str) {
        let mut args = cmd.split_whitespace();
        match args.next() {
            Some("lang") => match args.next() {
                Some("core") => self.app.set_mode(LangMode::Core),
                Some("atlas") => self.app.set_mode(LangMode::Atlas),
                Some("agent") => self.app.set_mode(LangMode::Agent),
                _ => self.write(OutKind::Error, "usage: /lang core|atlas|agent"),
            },
            Some("budget") => match args.next().and_then(|s| s.parse::<u64>().ok()) {
                Some(n) => {
                    self.app.session.budget = n;
                    self.write(OutKind::Info, &format!("budget = {n}"));
                }
                None => self.write(OutKind::Error, "usage: /budget <n>"),
            },
            Some("strong") => {
                self.app.session.strong = !self.app.session.strong;
                let strong = self.app.session.strong;
                self.write(OutKind::Info, &format!("strong = {strong}"));
            }
            Some("locals") => self.app.list_locals(),
            Some("show") => match args.next() {
                Some("ast") => {
                    self.app.session.show_ast = !self.app.session.show_ast;
                    let show = self.app.session.show_ast;
                    self.write(OutKind::Info, &format!("show ast = {show}"));
                }
                _ => self.write(OutKind::Error, "usage: /show ast"),
            },
            Some("step") => {
                let expr = cmd.strip_prefix("step").unwrap_or("").trim().to_string();
                if expr.is_empty() {
                    self.write(OutKind::Error, "usage: /step <expr>");
                } else if self.app.mode != LangMode::Core {
                    self.write(OutKind::Error, "stepping is only available in core mode");
                } else {
                    self.app.eval_line(&expr, true);
                }
            }
            Some("source") => {
                let path = cmd.strip_prefix("source").unwrap_or("").trim();
                if path.is_empty() {
                    self.write(OutKind::Error, "usage: /source <file.atc|file.at>");
                } else {
                    self.app.source_file(std::path::Path::new(path));
                }
            }
            Some("panel") => match args.next() {
                None => self.app.toggle_panel(),
                Some(name) => self.open_panel(name),
            },
            Some("abort") => self.app.abort_eval(),
            Some("help") => self.app.help(),
            Some("quit") | Some("exit") => self.quit(),
            Some(other) => self.write(
                OutKind::Error,
                &format!("unknown command: /{other} (try /help)"),
            ),
            None => self.write(OutKind::Error, "usage: /<command> (try /help)"),
        }
    }
}

pub struct CommandSpec {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub description: &'static str,
    pub execute: fn(&mut CommandContext<'_, '_>, &str),
    pub complete: fn(&CommandContext<'_, '_>, &str) -> Vec<Completion>,
}

pub struct PanelSpec {
    pub name: &'static str,
    pub title: &'static str,
    pub draw: fn(&mut ratatui::Frame, &mut App, Rect),
    pub handle_key: fn(&mut App, KeyEvent),
}

impl<'h> App<'h> {
    pub(crate) fn register_command(&mut self, command: CommandSpec) {
        self.commands.push(command);
    }

    pub(crate) fn register_panel(&mut self, panel: PanelSpec) {
        self.panels.push(panel);
    }
}

/// How a transcript line is styled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutKind {
    /// An echoed input line (`core> …`).
    Input,
    /// An evaluation result or other primary output.
    Output,
    Error,
    /// Confirmations, listings, hints.
    Info,
    /// A stepper snapshot (one interaction's intermediate term).
    Step,
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

/// Transcript scroll: `stick` keeps the view pinned to the newest line.
pub struct Scroll {
    pub offset: usize,
    pub stick: bool,
}

pub struct App<'h> {
    pub h: &'h HeapScope<'h>,
    pub session: Session<'h>,
    pub mode: LangMode,
    pub eval: EvalState<'h>,
    /// The most recent completed result, kept live as a heap-explorer root.
    pub last_result: Option<TermPtr<'h>>,
    /// Forged pointers to leaked subgraph roots (leak view only).
    pub leaked: Vec<TermPtr<'h>>,
    pub transcript: Vec<OutLine>,
    pub scroll: Scroll,
    pub input: InputBox,
    pub panel_open: bool,
    pub panel_index: usize,
    pub commands: Vec<CommandSpec>,
    pub panels: Vec<PanelSpec>,
    pub completions: Vec<Completion>,
    pub completion_index: usize,
    pub focus: Focus,
    pub explorer: ExplorerState,
    /// Transcript viewport height from the last draw (for paging).
    pub transcript_height: usize,
    /// The input line was auto-focused by typing `:` from the panel; deleting
    /// that `:` (or submitting) hands focus back to the panel.
    input_auto_focused: bool,
    pub should_quit: bool,
}

pub fn run<'h>(h: &'h HeapScope<'h>, args: &Args, mut terminal: DefaultTerminal) -> io::Result<()> {
    let mut app = App::new(h, args);

    for path in &args.source {
        app.source_file(path);
        // Startup sources run to completion before the next file.
        while app.eval.is_active() {
            app.tick();
        }
    }

    while !app.should_quit {
        terminal.draw(|f| ui::draw(f, &mut app))?;
        app.tick();
        // Redraw between slices while evaluating; otherwise wait for input.
        let timeout = if app.eval.is_active() {
            Duration::ZERO
        } else {
            Duration::from_millis(100)
        };
        if event::poll(timeout)? {
            app.handle_event(event::read()?);
            // Drain bursts (e.g. a paste) before redrawing.
            while !app.should_quit && event::poll(Duration::ZERO)? {
                app.handle_event(event::read()?);
            }
        }
    }
    app.input.save_history();
    Ok(())
}

impl<'h> App<'h> {
    pub fn new(h: &'h HeapScope<'h>, args: &Args) -> Self {
        let session = Session::new(h, args.budget, args.strong);
        let mut app = App {
            h,
            session,
            mode: args.lang.into(),
            eval: EvalState::Idle,
            last_result: None,
            leaked: Vec::new(),
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
            completions: Vec::new(),
            completion_index: 0,
            focus: Focus::Input,
            explorer: ExplorerState::new(),
            transcript_height: 0,
            input_auto_focused: false,
            should_quit: false,
        };
        for command in builtin_commands() {
            app.register_command(command);
        }
        for panel in built_in_panels() {
            app.register_panel(panel);
        }
        app.push(
            OutKind::Info,
            "Atlas — /help for commands, Ctrl+B for the heap/stepper panel, Ctrl+D to exit.",
        );
        if !args.no_prelude {
            match app.session.load_prelude() {
                SubmitResult::Output(lines) => {
                    for line in lines {
                        app.push(OutKind::Info, &line);
                    }
                }
                SubmitResult::Error { message, output } => {
                    for line in output {
                        app.push(OutKind::Info, &line);
                    }
                    app.push(OutKind::Error, &format!("prelude error: {message}"));
                }
                SubmitResult::StartEval { root, output } => {
                    for line in output {
                        app.push(OutKind::Info, &line);
                    }
                    eval::erase(&app.session, root);
                    app.push(
                        OutKind::Error,
                        "prelude error: expected declarations, got expression",
                    );
                }
            }
        }
        app
    }

    /// Append a (possibly multi-line) block to the transcript.
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

    fn pretty(&self, ptr: &TermPtr<'h>) -> String {
        let printer = Printer::new(self.h);
        let pretty = printer.pretty(ptr);
        pretty.to_string()
    }

    // ================================================================
    // Input lines and commands
    // ================================================================

    pub fn submit_line(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        self.push(OutKind::Input, &format!("{}> {line}", self.mode.label()));
        match line.strip_prefix('/') {
            Some(cmd) => self.dispatch_command(cmd),
            None => self.eval_line(line, false),
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
        let execute = command.execute;
        execute(&mut CommandContext { app: self }, cmd);
    }

    fn eval_line(&mut self, line: &str, paused: bool) {
        if self.eval.is_running() {
            self.push(
                OutKind::Error,
                "an evaluation is already pending (/abort or Ctrl+C to cancel it)",
            );
            return;
        }
        match self.session.submit(self.mode, line) {
            SubmitResult::StartEval { root, output } => {
                for block in output {
                    self.push(OutKind::Info, &block);
                }
                let (budget, strong) = (self.session.budget, self.session.strong);
                self.eval.start(root, strong, budget, paused);
                if paused {
                    self.panel_open = true;
                    self.panel_index = self
                        .panels
                        .iter()
                        .position(|panel| panel.name == "stepper")
                        .unwrap_or(0);
                    self.focus = Focus::Panel;
                    if let Some(root) = self.eval.root_ptr() {
                        let text = self.pretty(root);
                        self.push(OutKind::Step, &text);
                    }
                }
                self.refresh_explorer();
            }
            SubmitResult::Output(blocks) => {
                for block in blocks {
                    self.push(OutKind::Output, &block);
                }
                self.refresh_explorer();
            }
            SubmitResult::Error { message, output } => {
                // Stage dumps that succeeded before the failure (show_ast).
                for block in output {
                    self.push(OutKind::Info, &block);
                }
                self.push(OutKind::Error, &format!("error: {message}"));
            }
        }
    }

    /// Source a file into the session (see [`Session::source_file`]): `.atc`
    /// binds/evaluates core input, `.at` parses an atlas module.
    pub fn source_file(&mut self, path: &std::path::Path) {
        if self.eval.is_running() {
            self.push(
                OutKind::Error,
                "an evaluation is already pending (/abort or Ctrl+C to cancel it)",
            );
            return;
        }
        match self.session.source_file(path) {
            SubmitResult::StartEval { root, output } => {
                for block in output {
                    self.push(OutKind::Info, &block);
                }
                self.push(OutKind::Info, &format!("sourced {}", path.display()));
                let (budget, strong) = (self.session.budget, self.session.strong);
                self.eval.start(root, strong, budget, false);
                self.refresh_explorer();
            }
            SubmitResult::Output(blocks) => {
                for block in blocks {
                    self.push(OutKind::Info, &block);
                }
                self.push(OutKind::Info, &format!("sourced {}", path.display()));
                self.refresh_explorer();
            }
            SubmitResult::Error { message, output } => {
                for block in output {
                    self.push(OutKind::Info, &block);
                }
                self.push(OutKind::Error, &format!("error: {message}"));
            }
        }
    }

    fn set_mode(&mut self, mode: LangMode) {
        self.mode = mode;
        self.push(OutKind::Info, &format!("lang = {}", mode.label()));
    }

    fn list_locals(&mut self) {
        let lines: Vec<String> = self
            .session
            .locals()
            .iter()
            .map(|(name, kind, _)| format!("  {name} ({})", kind.label()))
            .collect();
        if lines.is_empty() {
            self.push(OutKind::Info, "(no locals)");
        } else {
            for line in lines {
                self.push(OutKind::Info, &line);
            }
        }
    }

    fn help(&mut self) {
        self.push(OutKind::Info, "commands:");
        let lines = self
            .commands
            .iter()
            .map(|command| {
                let aliases = if command.aliases.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", command.aliases.join(", "))
                };
                format!("  /{}{aliases}  {}", command.name, command.description)
            })
            .collect::<Vec<_>>();
        for line in lines {
            self.push(OutKind::Info, &line);
        }
        self.push(
            OutKind::Info,
            "panels: /panel [name]  (registered panels provide their own controls)",
        );
        self.push(
            OutKind::Info,
            "keys: Tab switches focus · Shift+Tab cycles language/panel · PageUp/Down scroll · Ctrl+C aborts or quits",
        );
    }

    // ================================================================
    // Evaluation driving
    // ================================================================

    /// Run one evaluation slice if an eval is active.
    pub fn tick(&mut self) {
        if let Some(event) = self.eval.tick(&self.session) {
            self.handle_eval_event(event);
        }
    }

    fn handle_eval_event(&mut self, event: EvalEvent<'h>) {
        match event {
            EvalEvent::Stepped(interaction) => {
                self.push(OutKind::Step, &format!("── {interaction:?}"));
                if let Some(root) = self.eval.root_ptr() {
                    let text = self.pretty(root);
                    self.push(OutKind::Step, &text);
                }
                self.refresh_explorer();
            }
            EvalEvent::Finished { result, steps } => {
                let text = self.pretty(&result);
                self.push(OutKind::Output, &text);
                self.push(OutKind::Info, &format!("({steps} interactions)"));
                self.set_last_result(result);
                self.refresh_explorer();
            }
            EvalEvent::BudgetExhausted { partial, steps } => {
                self.push(
                    OutKind::Error,
                    &format!("(budget exhausted after {steps} interactions; partial term:)"),
                );
                let text = self.pretty(&partial);
                self.push(OutKind::Output, &text);
                self.set_last_result(partial);
                self.refresh_explorer();
            }
            EvalEvent::Error { message } => {
                self.push(OutKind::Error, &format!("error: {message}"));
                self.refresh_explorer();
            }
        }
    }

    /// Replace the kept result, reclaiming the previous one.
    fn set_last_result(&mut self, ptr: TermPtr<'h>) {
        if let Some(old) = self.last_result.take() {
            eval::erase(&self.session, old);
        }
        self.last_result = Some(ptr);
    }

    fn abort_eval(&mut self) {
        match self.eval.abort() {
            None => self.push(OutKind::Error, "no evaluation to abort"),
            Some((ptr, steps)) => {
                let text = self.pretty(&ptr);
                self.push(
                    OutKind::Info,
                    &format!("aborted after {steps} interactions; current term:"),
                );
                self.push(OutKind::Info, &text);
                eval::erase(&self.session, ptr);
                self.refresh_explorer();
            }
        }
    }

    // ================================================================
    // Heap explorer
    // ================================================================

    /// Rebuild the explorer tree (and, in leak view, rescan for leaks). Runs
    /// only at slice boundaries: never while a reduction is actively in flight.
    pub fn refresh_explorer(&mut self) {
        if !self.panel_open || self.eval.is_active() {
            return;
        }
        if self.explorer.show_leaked {
            // Forgetting the previously forged pointers reverts those subgraphs
            // to leaked, so the fresh scan re-finds them.
            self.leaked.clear();
            let mut roots: Vec<&TermPtr<'h>> =
                self.session.locals().into_iter().map(|(.., p)| p).collect();
            roots.extend(self.last_result.as_ref());
            roots.extend(self.eval.root_ptr());
            // SAFETY: `roots` is every externally held pointer into this heap:
            // the App owns them all (locals, last result, pending eval root),
            // and the previous leak pointers were just forgotten. Reduction is
            // idle or paused at a slice boundary here.
            self.leaked = unsafe { self.h.find_leaked_roots(&roots) };
        } else {
            // Drop (forget) any held leak pointers; the subgraphs stay leaked.
            self.leaked.clear();
        }

        let mut entries = Vec::new();
        if let Some(ptr) = self.eval.root_ptr() {
            entries.push(RootEntry {
                label: "(pending eval)".to_string(),
                ptr,
                leaked: false,
            });
        }
        if let Some(ptr) = &self.last_result {
            entries.push(RootEntry {
                label: "result".to_string(),
                ptr,
                leaked: false,
            });
        }
        for (name, kind, ptr) in self.session.locals() {
            entries.push(RootEntry {
                label: format!("{name} ({})", kind.label()),
                ptr,
                leaked: false,
            });
        }
        for (i, ptr) in self.leaked.iter().enumerate() {
            entries.push(RootEntry {
                label: format!("leaked[{i}]"),
                ptr,
                leaked: true,
            });
        }
        self.explorer.rebuild(self.h, &entries);
    }

    fn toggle_panel(&mut self) {
        self.panel_open = !self.panel_open;
        if self.panel_open {
            self.refresh_explorer();
        } else if self.focus == Focus::Panel {
            self.focus = Focus::Input;
        }
    }

    /// Open (or keep open) the panel on `tab` and focus it.
    fn open_panel(&mut self, name: &str) {
        let Some(index) = self.panels.iter().position(|panel| panel.name == name) else {
            self.push(OutKind::Error, &format!("unknown panel: {name}"));
            return;
        };
        self.panel_open = true;
        self.panel_index = index;
        self.focus = Focus::Panel;
        self.refresh_explorer();
    }

    // ================================================================
    // Events
    // ================================================================

    pub fn handle_event(&mut self, event: Event) {
        let Event::Key(key) = event else {
            return;
        };
        if key.kind == KeyEventKind::Release {
            return;
        }
        match (key.code, key.modifiers) {
            (KeyCode::Char('b'), KeyModifiers::CONTROL) => self.toggle_panel(),
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                if self.eval.is_running() {
                    self.abort_eval();
                } else {
                    self.should_quit = true;
                }
            }
            (KeyCode::BackTab, _) => match self.focus {
                Focus::Input => self.set_mode(self.mode.next()),
                Focus::Panel => {
                    self.panel_index = (self.panel_index + 1) % self.panels.len();
                    self.refresh_explorer();
                }
            },
            (KeyCode::Tab, modifiers) if modifiers.contains(KeyModifiers::SHIFT) => {
                match self.focus {
                    Focus::Input => self.set_mode(self.mode.next()),
                    Focus::Panel => {
                        self.panel_index = (self.panel_index + 1) % self.panels.len();
                        self.refresh_explorer();
                    }
                }
            }
            (KeyCode::Tab, _) => {
                if self.panel_open {
                    self.input_auto_focused = false;
                    self.focus = match self.focus {
                        Focus::Input => Focus::Panel,
                        Focus::Panel => Focus::Input,
                    };
                }
            }
            (KeyCode::PageUp, _) => {
                self.scroll.stick = false;
                let page = self.transcript_height.max(1);
                self.scroll.offset = self.scroll.offset.saturating_sub(page);
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
                    let replacement = self.completions[self.completion_index].replacement.clone();
                    self.input.replace_line(replacement);
                    self.refresh_completions();
                    return;
                }
                _ => {}
            }
        }
        if let Some(line) = self.input.handle_key(key) {
            let auto = std::mem::take(&mut self.input_auto_focused);
            self.scroll.stick = true;
            self.submit_line(&line);
            self.completions.clear();
            // A `/`-initiated command hands focus back to the panel afterwards
            // (unless the command itself moved focus, e.g. `/panel stepper`).
            if auto && self.focus == Focus::Input && self.panel_open {
                self.focus = Focus::Panel;
            }
            return;
        }
        self.refresh_completions();
        // The auto-focus lasts only while the line is still a `/` command:
        // deleting the initial `/` reverts focus to the panel.
        if self.input_auto_focused && !self.input.line().starts_with('/') {
            self.input_auto_focused = false;
            self.focus = Focus::Panel;
        }
    }

    fn panel_key(&mut self, key: KeyEvent) {
        // `/` starts a command: jump to the input line with the `/` typed.
        if key.code == KeyCode::Char('/') {
            self.focus = Focus::Input;
            self.input_auto_focused = true;
            self.input.handle_key(key);
            self.refresh_completions();
            return;
        }
        let handle_key = self.panels[self.panel_index].handle_key;
        handle_key(self, key);
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
        let matches = if !has_argument {
            self.commands
                .iter()
                .filter(|command| command.name.starts_with(name) && command.name != name)
                .map(|command| Completion {
                    replacement: format!("/{}", command.name),
                    label: format!("/{}", command.name),
                    description: command.description.to_string(),
                })
                .collect()
        } else {
            let Some(command) = self.commands.iter().find(|command| {
                command.name == name || command.aliases.iter().any(|alias| *alias == name)
            }) else {
                self.completions.clear();
                self.completion_index = 0;
                return;
            };
            let complete = command.complete;
            complete(&CommandContext { app: self }, command_line)
        };
        self.completions = matches;
        if self.completions.len() == 1 && self.completions[0].replacement == line {
            self.completions.clear();
        }
        self.completion_index = self
            .completion_index
            .min(self.completions.len().saturating_sub(1));
    }
}

fn run_builtin(ctx: &mut CommandContext<'_, '_>, cmd: &str) {
    ctx.run_builtin(cmd);
}

fn complete_builtin(ctx: &CommandContext<'_, '_>, cmd: &str) -> Vec<Completion> {
    let mut parts = cmd.split_whitespace();
    let name = parts.next().unwrap_or_default();
    let partial = parts.next().unwrap_or_default();
    if parts.next().is_some() {
        return Vec::new();
    }
    let values: Vec<&str> = match name {
        "lang" => vec!["core", "atlas", "agent"],
        "show" => vec!["ast"],
        "panel" => ctx.app.panels.iter().map(|panel| panel.name).collect(),
        _ => Vec::new(),
    };
    values
        .into_iter()
        .filter(|value| value.starts_with(partial))
        .map(|value| Completion {
            replacement: format!("/{name} {value}"),
            label: value.to_string(),
            description: String::new(),
        })
        .collect()
}

fn builtin_commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            name: "lang",
            aliases: &[],
            description: "switch the input language",
            execute: run_builtin,
            complete: complete_builtin,
        },
        CommandSpec {
            name: "budget",
            aliases: &[],
            description: "set the reduction budget",
            execute: run_builtin,
            complete: complete_builtin,
        },
        CommandSpec {
            name: "strong",
            aliases: &[],
            description: "toggle strong normalization",
            execute: run_builtin,
            complete: complete_builtin,
        },
        CommandSpec {
            name: "locals",
            aliases: &[],
            description: "list locals in scope",
            execute: run_builtin,
            complete: complete_builtin,
        },
        CommandSpec {
            name: "show",
            aliases: &[],
            description: "toggle diagnostic output",
            execute: run_builtin,
            complete: complete_builtin,
        },
        CommandSpec {
            name: "step",
            aliases: &[],
            description: "start a paused evaluation",
            execute: run_builtin,
            complete: complete_builtin,
        },
        CommandSpec {
            name: "source",
            aliases: &[],
            description: "source a file",
            execute: run_builtin,
            complete: complete_builtin,
        },
        CommandSpec {
            name: "panel",
            aliases: &[],
            description: "open or toggle a sub-panel",
            execute: run_builtin,
            complete: complete_builtin,
        },
        CommandSpec {
            name: "abort",
            aliases: &[],
            description: "cancel the pending evaluation",
            execute: run_builtin,
            complete: complete_builtin,
        },
        CommandSpec {
            name: "help",
            aliases: &[],
            description: "show available commands",
            execute: run_builtin,
            complete: complete_builtin,
        },
        CommandSpec {
            name: "quit",
            aliases: &["exit"],
            description: "exit the REPL",
            execute: run_builtin,
            complete: complete_builtin,
        },
    ]
}

fn memory_panel_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Up => app.explorer.move_selection(-1),
        KeyCode::Down => app.explorer.move_selection(1),
        KeyCode::Enter | KeyCode::Char(' ') => {
            if app.explorer.toggle_selected() {
                app.refresh_explorer();
            }
        }
        KeyCode::Char('d') => {
            app.explorer.show_leaked = !app.explorer.show_leaked;
            app.refresh_explorer();
        }
        KeyCode::Char('r') => app.refresh_explorer(),
        _ => {}
    }
}

fn stepper_panel_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char('s') => {
            if app.eval.is_running() {
                app.eval.set_paused(true);
                if let Some(event) = app.eval.step(&app.session) {
                    app.handle_eval_event(event);
                }
            } else {
                app.push(OutKind::Info, "nothing to step (/step <expr> starts one)");
            }
        }
        KeyCode::Char('c') => app.eval.set_paused(false),
        KeyCode::Char('p') => {
            app.eval.set_paused(true);
            app.refresh_explorer();
        }
        KeyCode::Char('x') => app.abort_eval(),
        _ => {}
    }
}

fn built_in_panels() -> Vec<PanelSpec> {
    vec![
        PanelSpec {
            name: "memory",
            title: "memory",
            draw: ui::draw_memory_panel,
            handle_key: memory_panel_key,
        },
        PanelSpec {
            name: "stepper",
            title: "stepper",
            draw: ui::draw_stepper_panel,
            handle_key: stepper_panel_key,
        },
    ]
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use atlas_core::vm::heap::Heap;

    use super::*;
    use crate::LangArg;

    fn args(no_prelude: bool) -> Args {
        Args {
            lang: LangArg::Core,
            budget: 1_000,
            strong: false,
            no_prelude,
            source: Vec::<PathBuf>::new(),
        }
    }

    #[test]
    fn app_loads_prelude_unless_disabled() {
        let heap = Heap::new();
        heap.with(|h| {
            let app = App::new(h, &args(false));
            assert!(app.session.locals().iter().any(|(name, ..)| *name == "fib"));
        });

        let heap = Heap::new();
        heap.with(|h| {
            let app = App::new(h, &args(true));
            assert!(!app.session.locals().iter().any(|(name, ..)| *name == "fib"));
        });
    }

    #[test]
    fn slash_panel_command_opens_named_tab() {
        let heap = Heap::new();
        heap.with(|h| {
            let mut app = App::new(h, &args(true));
            app.submit_line("/panel stepper");
            assert!(app.panel_open);
            assert_eq!(app.panels[app.panel_index].name, "stepper");
            assert_eq!(app.focus, Focus::Panel);
        });
    }

    #[test]
    fn tab_switches_focus_and_shift_tab_cycles_language_or_panel() {
        let heap = Heap::new();
        heap.with(|h| {
            let mut app = App::new(h, &args(true));
            assert_eq!(app.panels[app.panel_index].name, "memory");
            app.handle_event(Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT)));
            assert!(!app.panel_open);
            assert_eq!(app.focus, Focus::Input);
            assert_eq!(app.mode, LangMode::Atlas);

            app.handle_event(Event::Key(KeyEvent::new(
                KeyCode::BackTab,
                KeyModifiers::SHIFT,
            )));
            assert_eq!(app.mode, LangMode::Agent);

            app.handle_event(Event::Key(KeyEvent::new(
                KeyCode::Char('b'),
                KeyModifiers::CONTROL,
            )));
            assert!(app.panel_open);
            assert_eq!(app.focus, Focus::Input);

            app.handle_event(Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)));
            assert_eq!(app.focus, Focus::Panel);
            assert_eq!(app.panels[app.panel_index].name, "memory");

            app.handle_event(Event::Key(KeyEvent::new(
                KeyCode::BackTab,
                KeyModifiers::SHIFT,
            )));
            assert_eq!(app.focus, Focus::Panel);
            assert_eq!(app.panels[app.panel_index].name, "stepper");
        });
    }

    fn custom_command(ctx: &mut CommandContext<'_, '_>, _: &str) {
        ctx.write(OutKind::Info, "custom command ran");
    }

    fn no_custom_completions(_: &CommandContext<'_, '_>, _: &str) -> Vec<Completion> {
        Vec::new()
    }

    fn draw_custom_panel(_: &mut ratatui::Frame, _: &mut App, _: Rect) {}

    fn custom_panel_key(app: &mut App, key: KeyEvent) {
        if key.code == KeyCode::Char('k') {
            app.push(OutKind::Info, "custom panel handled key");
        }
    }

    #[test]
    fn registered_commands_are_executable_and_completable() {
        let heap = Heap::new();
        heap.with(|h| {
            let mut app = App::new(h, &args(true));
            app.register_command(CommandSpec {
                name: "custom",
                aliases: &[],
                description: "a test command",
                execute: custom_command,
                complete: no_custom_completions,
            });
            app.handle_event(Event::Key(KeyEvent::new(
                KeyCode::Char('/'),
                KeyModifiers::NONE,
            )));
            assert!(app.completions.iter().any(|item| item.label == "/custom"));
            app.submit_line("/custom");
            assert!(app
                .transcript
                .iter()
                .any(|line| line.text == "custom command ran"));
        });
    }

    #[test]
    fn registered_panels_are_openable_and_receive_keys() {
        let heap = Heap::new();
        heap.with(|h| {
            let mut app = App::new(h, &args(true));
            app.register_panel(PanelSpec {
                name: "custom",
                title: "custom",
                draw: draw_custom_panel,
                handle_key: custom_panel_key,
            });

            app.submit_line("/panel custom");
            assert!(app.panel_open);
            assert_eq!(app.panels[app.panel_index].name, "custom");
            assert_eq!(app.focus, Focus::Panel);

            app.handle_event(Event::Key(KeyEvent::new(
                KeyCode::Char('k'),
                KeyModifiers::NONE,
            )));
            assert!(app
                .transcript
                .iter()
                .any(|line| line.text == "custom panel handled key"));
        });
    }

    #[test]
    fn completion_enter_inserts_then_allows_submission() {
        let heap = Heap::new();
        heap.with(|h| {
            let mut app = App::new(h, &args(true));
            app.handle_event(Event::Key(KeyEvent::new(
                KeyCode::Char('/'),
                KeyModifiers::NONE,
            )));
            app.handle_event(Event::Key(KeyEvent::new(
                KeyCode::Char('p'),
                KeyModifiers::NONE,
            )));
            app.handle_event(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )));
            assert_eq!(app.input.line(), "/panel");
            assert!(app.completions.is_empty());
        });
    }
}
