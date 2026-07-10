//! Application state and the event loop: dispatches keys, drives evaluation
//! slices between redraws, and turns session/eval events into transcript lines.

use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;

use atlas_core::vm::heap::{HeapScope, TermPtr};
use atlas_core::vm::printer::Printer;

use crate::eval::{self, EvalEvent, EvalState};
use crate::explorer::{ExplorerState, RootEntry};
use crate::input::InputBox;
use crate::session::{LangMode, Session, SubmitResult};
use crate::ui;
use crate::Args;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelTab {
    Memory,
    Stepper,
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
    pub panel_tab: PanelTab,
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
            panel_tab: PanelTab::Memory,
            focus: Focus::Input,
            explorer: ExplorerState::new(),
            transcript_height: 0,
            input_auto_focused: false,
            should_quit: false,
        };
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
            Some(cmd) => self.command(cmd),
            None => self.eval_line(line, false),
        }
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
                    self.panel_tab = PanelTab::Stepper;
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

    fn command(&mut self, cmd: &str) {
        let mut args = cmd.split_whitespace();
        match args.next() {
            Some("lang") => match args.next() {
                Some("core") => self.set_mode(LangMode::Core),
                Some("atlas") => self.set_mode(LangMode::Atlas),
                _ => self.push(OutKind::Error, "usage: /lang core|atlas"),
            },
            Some("budget") => match args.next().and_then(|s| s.parse::<u64>().ok()) {
                Some(n) => {
                    self.session.budget = n;
                    self.push(OutKind::Info, &format!("budget = {n}"));
                }
                None => self.push(OutKind::Error, "usage: /budget <n>"),
            },
            Some("strong") => {
                self.session.strong = !self.session.strong;
                let strong = self.session.strong;
                self.push(OutKind::Info, &format!("strong = {strong}"));
            }
            Some("locals") => self.list_locals(),
            Some("show") => match args.next() {
                Some("ast") => {
                    self.session.show_ast = !self.session.show_ast;
                    let show = self.session.show_ast;
                    self.push(OutKind::Info, &format!("show ast = {show}"));
                }
                _ => self.push(OutKind::Error, "usage: /show ast"),
            },
            Some("step") => {
                let expr = cmd.strip_prefix("step").unwrap_or("").trim().to_string();
                if expr.is_empty() {
                    self.push(OutKind::Error, "usage: /step <expr>");
                } else if self.mode != LangMode::Core {
                    self.push(OutKind::Error, "stepping is only available in core mode");
                } else {
                    self.eval_line(&expr, true);
                }
            }
            Some("source") => {
                let path = cmd.strip_prefix("source").unwrap_or("").trim();
                if path.is_empty() {
                    self.push(OutKind::Error, "usage: /source <file.atc|file.at>");
                } else {
                    self.source_file(std::path::Path::new(path));
                }
            }
            Some("panel") => match args.next() {
                None => self.toggle_panel(),
                Some("memory") => self.open_panel(PanelTab::Memory),
                Some("stepper") => self.open_panel(PanelTab::Stepper),
                Some(_) => self.push(OutKind::Error, "usage: /panel [memory|stepper]"),
            },
            Some("abort") => self.abort_eval(),
            Some("help") => self.help(),
            Some("quit") | Some("exit") => self.should_quit = true,
            Some(other) => self.push(
                OutKind::Error,
                &format!("unknown command: /{other} (try /help)"),
            ),
            None => self.push(OutKind::Error, "usage: /<command> (try /help)"),
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
        for line in [
            "commands:",
            "  /lang core|atlas  switch the input language (core evaluates; atlas parses)",
            "  /budget <n>       set the reduction budget (interactions per evaluation)",
            "  /strong           toggle strong (vs weak head) normalization",
            "  /locals           list the locals currently in scope (core mode)",
            "  /show ast         toggle dumping each line's AST (both languages)",
            "  /source <file>    source a file: .atc (core) evals into the locals,",
            "                    .at (atlas) parses (no atlas evaluation yet)",
            "  /step <expr>      start evaluating <expr> paused, one interaction at a time",
            "  /panel [memory|stepper]  toggle the side panel, or open the named tab",
            "  /abort            cancel the pending evaluation (also Ctrl+C)",
            "  /quit             exit (also Ctrl+D on an empty line)",
            "keys: Tab/Shift+Tab switch focus · PageUp/Down scroll ·",
            "  memory: ↑↓ select, ⏎ expand, d leaks, r refresh · stepper: s step,",
            "  c continue, p pause, x abort",
        ] {
            self.push(OutKind::Info, line);
        }
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
    fn open_panel(&mut self, tab: PanelTab) {
        self.panel_open = true;
        self.panel_tab = tab;
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
            (KeyCode::BackTab, _) | (KeyCode::Tab, _) => {
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
        if let Some(line) = self.input.handle_key(key) {
            let auto = std::mem::take(&mut self.input_auto_focused);
            self.scroll.stick = true;
            self.submit_line(&line);
            // A `/`-initiated command hands focus back to the panel afterwards
            // (unless the command itself moved focus, e.g. `/panel stepper`).
            if auto && self.focus == Focus::Input && self.panel_open {
                self.focus = Focus::Panel;
            }
            return;
        }
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
            return;
        }
        match self.panel_tab {
            PanelTab::Memory => match key.code {
                KeyCode::Up => self.explorer.move_selection(-1),
                KeyCode::Down => self.explorer.move_selection(1),
                KeyCode::Enter | KeyCode::Char(' ') => {
                    if self.explorer.toggle_selected() {
                        self.refresh_explorer();
                    }
                }
                KeyCode::Char('d') => {
                    self.explorer.show_leaked = !self.explorer.show_leaked;
                    self.refresh_explorer();
                }
                KeyCode::Char('r') => self.refresh_explorer(),
                _ => {}
            },
            PanelTab::Stepper => match key.code {
                KeyCode::Char('s') => {
                    if self.eval.is_running() {
                        self.eval.set_paused(true);
                        if let Some(event) = self.eval.step(&self.session) {
                            self.handle_eval_event(event);
                        }
                    } else {
                        self.push(OutKind::Info, "nothing to step (/step <expr> starts one)");
                    }
                }
                KeyCode::Char('c') => self.eval.set_paused(false),
                KeyCode::Char('p') => {
                    self.eval.set_paused(true);
                    self.refresh_explorer();
                }
                KeyCode::Char('x') => self.abort_eval(),
                _ => {}
            },
        }
    }
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
            assert_eq!(app.panel_tab, PanelTab::Stepper);
            assert_eq!(app.focus, Focus::Panel);
        });
    }

    #[test]
    fn tab_and_shift_tab_switch_focus() {
        let heap = Heap::new();
        heap.with(|h| {
            let mut app = App::new(h, &args(true));
            assert_eq!(app.panel_tab, PanelTab::Memory);
            app.handle_event(Event::Key(KeyEvent::new(
                KeyCode::BackTab,
                KeyModifiers::SHIFT,
            )));
            assert!(!app.panel_open);
            assert_eq!(app.focus, Focus::Input);

            app.handle_event(Event::Key(KeyEvent::new(
                KeyCode::Char('b'),
                KeyModifiers::CONTROL,
            )));
            assert!(app.panel_open);
            assert_eq!(app.focus, Focus::Input);

            app.handle_event(Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT)));
            assert_eq!(app.focus, Focus::Panel);
            assert_eq!(app.panel_tab, PanelTab::Memory);

            app.handle_event(Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)));
            assert_eq!(app.focus, Focus::Input);
        });
    }
}
