//! A small interactive REPL for the Atlas interaction calculus.
//!
//! Type an expression to evaluate it, bind a local with `x = expr;` (affine,
//! consumed on first use) or `&x = expr;` (auto-dup, duplicated on every use),
//! or use a ":" command:
//!
//!   :budget <n>   set the reduction budget (max interactions per evaluation)
//!   :verbose      toggle verbose mode: print the lowered expr, then the term
//!                 after every interaction
//!   :locals       list the locals currently in scope
//!   :help         show the command list
//!   :quit         leave the REPL (Ctrl-D also works)
//!
//! Local bindings persist across inputs (they live in one session-long heap) and
//! are evaluated lazily: `x = 1 + 1;` stores the unreduced term and only reduces
//! it when `x` is used. In verbose mode the executor is stepped one interaction
//! at a time (each step halts after a single interaction) so the reduction can
//! be printed line-by-line.

use clap::Parser;
use reedline::{Prompt, PromptEditMode, PromptHistorySearch, Reedline, Signal};
use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{BufRead, IsTerminal};
use std::sync::Mutex;

use atlas_core::core::ast::{Binding, Node, desugar_open};
use atlas_core::core::parse::{ReplInput, parse_repl};
use atlas_core::extension::NoExtensions;
use atlas_core::vm::exec::{ExecPolicy, Executor, FiniteBudget, InteractionType};
use atlas_core::vm::heap::{Heap, HeapScope, TermPtr};
use atlas_core::vm::printer::Printer;
use atlas_core::vm::term::PrimId;

/// The default reduction budget (interactions per evaluation).
const DEFAULT_BUDGET: u64 = 1_000_000;

#[derive(Parser)]
#[command(
    name = "repl",
    about = "An interactive REPL for the Atlas interaction calculus"
)]
struct Args {
    /// Reduction budget: the maximum number of interactions per evaluation.
    #[arg(long, default_value_t = DEFAULT_BUDGET)]
    budget: u64,

    /// Start in verbose mode (print the term after every interaction).
    #[arg(long)]
    verbose: bool,

    /// Evaluate a single expression and exit instead of starting the REPL.
    expr: Option<String>,
}

/// Whether a REPL local is consumed on use or duplicated on every use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalKind {
    /// `x = …`: affine; the term is taken (and the binding removed) on first use.
    Affine,
    /// `&x = …`: auto-dup; each use splices a fresh dup, keeping the binding.
    AutoDup,
}

/// A REPL local binding: its (lazy) value term and how it is consumed.
struct Local<'h> {
    ptr: TermPtr<'h>,
    kind: LocalKind,
}

/// The locals in scope, keyed by name. The stored [`TermPtr`] is a live node in
/// the session heap.
struct Locals<'h> {
    map: HashMap<String, Local<'h>>,
}

impl<'h> Locals<'h> {
    fn new() -> Self {
        Locals {
            map: HashMap::new(),
        }
    }

    /// Bind (or rebind) `name` to `ptr`. A redefinition simply overwrites.
    fn bind(&mut self, name: String, kind: LocalKind, ptr: TermPtr<'h>) {
        self.map.insert(name, Local { ptr, kind });
    }

    /// Resolve a use site. Affine locals are taken and removed; auto-dup locals
    /// are duplicated via [`HeapScope::dup_use`], keeping the `Dp1` branch as the
    /// binding's new value so it survives for the next use.
    fn use_name(&mut self, name: &str, h: &'h HeapScope<'h>) -> Option<TermPtr<'h>> {
        let kind = self.map.get(name)?.kind;
        match kind {
            LocalKind::Affine => Some(self.map.remove(name).unwrap().ptr),
            LocalKind::AutoDup => {
                let ptr = self.map.remove(name).unwrap().ptr;
                let (use_node, keep_node) = h.dup_use(ptr);
                self.bind(name.to_string(), LocalKind::AutoDup, keep_node);
                Some(use_node)
            }
        }
    }
}

/// A policy that performs at most a single interaction before stopping, and
/// remembers which one it was. Verbose mode drives reduction with this one step
/// at a time so each printed snapshot is a real intermediate term (see
/// [`Session::eval_term`]).
#[derive(Default)]
struct StepPolicy {
    // Interior mutability through `&self`. A `Mutex` (rather than a `Cell`) keeps
    // the policy `Sync`, which the async reduction drivers require of `&self`.
    stepped: Mutex<Option<InteractionType>>,
}

impl StepPolicy {
    /// The interaction performed this step, if any.
    fn stepped(&self) -> Option<InteractionType> {
        *self.stepped.lock().unwrap()
    }
}

impl ExecPolicy for StepPolicy {
    fn next_step(&self, interaction: InteractionType) {
        let mut slot = self.stepped.lock().unwrap();
        // Record only the first interaction (reduction stops right after).
        slot.get_or_insert(interaction);
    }
    fn should_continue(&self) -> bool {
        // Keep going only until the first interaction fires.
        self.stepped.lock().unwrap().is_none()
    }
}

/// One REPL session: a branded heap, its async runtime, and the locals in scope.
struct Session<'h> {
    h: &'h HeapScope<'h>,
    runtime: tokio::runtime::Runtime,
    locals: Locals<'h>,
    budget: u64,
    verbose: bool,
}

impl<'h> Session<'h> {
    /// Desugar and lower a surface node into the session heap, resolving any free
    /// names against the locals in scope. In verbose mode the desugared core
    /// expression (the "lowered expr") is printed before it is compiled.
    fn lower_input(&mut self, node: &Node) -> Result<TermPtr<'h>, String> {
        let expr = desugar_open(node)?;
        if self.verbose {
            println!("lowered:\n{expr}");
        }
        // The REPL exposes no host primitives (`%name`), so nothing resolves.
        let prim = |_: &str| -> Option<PrimId> { None };
        // Copy out the (Copy) heap reference so the closure can borrow `locals`.
        let h = self.h;
        let locals = &mut self.locals;
        h.lower(&expr, &prim, &mut |n| locals.use_name(n, h))
    }

    /// Parse and run one line of source: either evaluate an expression or apply
    /// one or more `lhs = rhs;` bindings.
    fn run_line(&mut self, line: &str) {
        match parse_repl(line) {
            Ok(ReplInput::Expr(node)) => self.eval_expr(&node),
            Ok(ReplInput::Decl(bindings)) => self.bind_decl(bindings),
            Err(e) => eprintln!("error: {e}"),
        }
    }

    /// Lower an expression and print its (or each step's) normal form.
    fn eval_expr(&mut self, node: &Node) {
        match self.lower_input(node) {
            Ok(root) => self.eval_term(root),
            Err(e) => eprintln!("error: {e}"),
        }
    }

    /// Lower each `lhs = rhs;` and store it as a local (lazily — the value is not
    /// reduced until used).
    fn bind_decl(&mut self, bindings: Vec<(Binding, Node)>) {
        for (binding, value) in bindings {
            let (name, kind) = match binding {
                Binding::Var { name, auto_dup } => (
                    name.to_string(),
                    if auto_dup {
                        LocalKind::AutoDup
                    } else {
                        LocalKind::Affine
                    },
                ),
                Binding::Hole => {
                    eprintln!("error: `_` bindings are not supported in the REPL");
                    continue;
                }
                Binding::Dup { .. } => {
                    eprintln!(
                        "error: explicit dup bindings (&L{{..}}) are not supported in the REPL"
                    );
                    continue;
                }
            };
            match self.lower_input(&value) {
                Ok(ptr) => self.locals.bind(name, kind, ptr),
                Err(e) => eprintln!("error: {e}"),
            }
        }
    }

    /// Normalize `root` and print the result (or each interaction, in verbose
    /// mode). The reduction engine is async; it is driven on this session's
    /// single-threaded runtime.
    fn eval_term(&self, root: TermPtr<'h>) {
        let h = self.h;
        if !self.verbose {
            let exec = Executor::<_, NoExtensions>::new(h, FiniteBudget::new(self.budget));
            let result = self.runtime.block_on(exec.normalize_at(root));
            println!("{}", Printer::new(h).pretty(&result));
            if exec.policy.interactions() >= self.budget {
                eprintln!("(budget of {} interactions exhausted)", self.budget);
            }
            return;
        }

        // Verbose mode: reduce one interaction at a time. Each step drives the
        // engine with a fresh `StepPolicy` that halts after a single interaction,
        // so every snapshot we print is a genuine intermediate term.
        println!("==========================");
        println!("{}", Printer::new(h).pretty(&root));
        let mut ptr = root;
        let mut steps = 0u64;
        while steps < self.budget {
            let exec = Executor::<_, NoExtensions>::new(h, StepPolicy::default());
            ptr = self.runtime.block_on(exec.whnf_at(ptr));
            let Some(interaction) = exec.policy.stepped() else {
                break; // already in weak head normal form
            };
            steps += 1;
            println!("========================== {:?}", interaction);
            println!("{}", Printer::new(h).pretty(&ptr));
        }
        if steps >= self.budget {
            eprintln!("(budget of {} interactions exhausted)", self.budget);
        }
    }

    /// Handle one line of input. Returns `false` when the REPL should exit.
    fn handle(&mut self, line: &str) -> bool {
        let line = line.trim();
        if line.is_empty() {
            return true;
        }
        let Some(cmd) = line.strip_prefix(':') else {
            self.run_line(line);
            return true;
        };
        let mut args = cmd.split_whitespace();
        match args.next() {
            Some("budget") => match args.next().and_then(|s| s.parse::<u64>().ok()) {
                Some(n) => {
                    self.budget = n;
                    println!("budget = {n}");
                }
                None => eprintln!("usage: :budget <n>"),
            },
            Some("verbose") => {
                self.verbose = !self.verbose;
                println!("verbose = {}", self.verbose);
            }
            Some("locals") => self.print_locals(),
            Some("help") => help(),
            Some("quit") | Some("exit") => return false,
            Some(other) => eprintln!("unknown command: :{other} (try :help)"),
            None => eprintln!("usage: :<command> (try :help)"),
        }
        true
    }

    fn print_locals(&self) {
        if self.locals.map.is_empty() {
            println!("(no locals)");
            return;
        }
        let mut names: Vec<&String> = self.locals.map.keys().collect();
        names.sort();
        for name in names {
            let kind = match self.locals.map[name].kind {
                LocalKind::Affine => "affine",
                LocalKind::AutoDup => "auto-dup",
            };
            println!("  {name} ({kind})");
        }
    }
}

fn help() {
    println!("commands:");
    println!("  :budget <n>   set the reduction budget (interactions per evaluation)");
    println!("  :verbose      toggle printing the term after every interaction");
    println!("  :locals       list the locals currently in scope");
    println!("  :help         show this message");
    println!("  :quit         exit the REPL");
}

/// A minimal `> ` prompt for reedline (the default prompt is more elaborate).
struct ReplPrompt;

impl Prompt for ReplPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }
    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }
    fn render_prompt_indicator(&self, _mode: PromptEditMode) -> Cow<'_, str> {
        Cow::Borrowed("> ")
    }
    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Borrowed("... ")
    }
    fn render_prompt_history_search_indicator(&self, _search: PromptHistorySearch) -> Cow<'_, str> {
        Cow::Borrowed("")
    }
}

fn main() {
    let args = Args::parse();

    // The whole session runs inside one branded heap so locals can hold live
    // terms across inputs.
    let heap = Heap::new();
    heap.with(|h| {
        // Deterministic single-threaded runtime (no need for the multi-threaded
        // scheduler here).
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio runtime");
        let mut session = Session {
            h,
            runtime,
            locals: Locals::new(),
            budget: args.budget,
            verbose: args.verbose,
        };

        // Non-interactive: evaluate a single line and exit.
        if let Some(expr) = args.expr.as_deref() {
            session.run_line(expr);
            return;
        }

        // Piped input (no TTY): read lines plainly rather than via reedline (which
        // needs a terminal). Handy for scripting and tests.
        if !std::io::stdin().is_terminal() {
            for line in std::io::stdin().lock().lines() {
                let line = match line {
                    Ok(l) => l,
                    Err(e) => {
                        eprintln!("input error: {e}");
                        break;
                    }
                };
                if !session.handle(&line) {
                    break;
                }
            }
            return;
        }

        println!("Atlas REPL — type :help for commands, Ctrl-D to exit.");
        let mut line_editor = Reedline::create();
        let prompt = ReplPrompt;
        loop {
            match line_editor.read_line(&prompt) {
                Ok(Signal::Success(line)) => {
                    if !session.handle(&line) {
                        break;
                    }
                }
                Ok(Signal::CtrlC) => {
                    println!("CTRL-C");
                    break;
                }
                Ok(Signal::CtrlD) => {
                    println!("CTRL-D");
                    break;
                }
                Err(e) => {
                    eprintln!("input error: {e}");
                    break;
                }
            }
        }
    });
}
