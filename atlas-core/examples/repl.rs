//! A small interactive REPL for the Atlas interaction calculus.
//!
//! Type an expression to evaluate it, or use a ":" command:
//!
//!   :budget <n>   set the reduction budget (max interactions per evaluation)
//!   :verbose      toggle verbose mode: print the term after every interaction
//!   :help         show the command list
//!   :quit         leave the REPL (Ctrl-D also works)
//!
//! In verbose mode the executor is stepped one interaction at a time (each step
//! halts after a single interaction) so the reduction can be printed
//! line-by-line.

use clap::Parser;
use rustyline::error::ReadlineError;
use std::sync::Mutex;

use atlas_core::core::ast::desugar;
use atlas_core::core::parse::parse;
use atlas_core::vm::exec::{ExecPolicy, Executor, FiniteBudget, InteractionType, NoExtensions};
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

struct Repl {
    budget: u64,
    verbose: bool,
}

/// A policy that performs at most a single interaction before stopping, and
/// remembers which one it was. Verbose mode drives reduction with this one step
/// at a time so each printed snapshot is a real intermediate term (see
/// [`Repl::eval`]).
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

impl Repl {
    /// Parse, desugar and lower `src` into the branded `heap`, returning its root.
    fn load<'h>(src: &str, heap: &HeapScope<'h>) -> Result<TermPtr<'h>, String> {
        let node = parse(src)?;
        let expr = desugar(&node)?;
        // The REPL exposes no host primitives (`%name`), so nothing resolves.
        let resolve = |_: &str| -> Option<PrimId> { None };
        heap.lower(&expr, &resolve)
    }

    /// Evaluate one source expression and print the result (or each step, in
    /// verbose mode).
    fn eval(&self, src: &str) {
        // The reduction engine is async; drive it on a single-threaded runtime
        // (deterministic, no need for the multi-threaded scheduler here).
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio runtime");

        let heap = Heap::new();
        heap.with(|h| {
            let root = match Self::load(src, h) {
                Ok(root) => root,
                Err(e) => {
                    eprintln!("error: {e}");
                    return;
                }
            };

            if !self.verbose {
                let exec = Executor::<_, NoExtensions>::new(h, FiniteBudget::new(self.budget));
                let result = runtime.block_on(exec.normalize_at(root));
                println!("{}", Printer::new(h).pretty(&result));
                if exec.policy.interactions() >= self.budget {
                    eprintln!("(budget of {} interactions exhausted)", self.budget);
                }
                return;
            }

            // Verbose mode: reduce one interaction at a time. Each step drives the
            // engine with a fresh `StepPolicy` that halts after a single
            // interaction, so every snapshot we print is a genuine intermediate
            // term.
            println!("{}", Printer::new(h).pretty(&root));
            let mut ptr = root;
            let mut steps = 0u64;
            while steps < self.budget {
                let exec = Executor::<_, NoExtensions>::new(h, StepPolicy::default());
                ptr = runtime.block_on(exec.whnf_at(ptr));
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
        });
    }

    /// Handle one line of input. Returns `false` when the REPL should exit.
    fn handle(&mut self, line: &str) -> bool {
        let line = line.trim();
        if line.is_empty() {
            return true;
        }
        let Some(cmd) = line.strip_prefix(':') else {
            self.eval(line);
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
            Some("help") => help(),
            Some("quit") | Some("exit") => return false,
            Some(other) => eprintln!("unknown command: :{other} (try :help)"),
            None => eprintln!("usage: :<command> (try :help)"),
        }
        true
    }
}

fn help() {
    println!("commands:");
    println!("  :budget <n>   set the reduction budget (interactions per evaluation)");
    println!("  :verbose      toggle printing the term after every interaction");
    println!("  :help         show this message");
    println!("  :quit         exit the REPL");
}

fn main() {
    let args = Args::parse();
    let mut repl = Repl {
        budget: args.budget,
        verbose: args.verbose,
    };

    // Non-interactive: evaluate a single expression and exit.
    if let Some(expr) = args.expr {
        repl.eval(&expr);
        return;
    }

    println!("Atlas REPL — type :help for commands, Ctrl-D to exit.");
    let mut rl = rustyline::DefaultEditor::new().unwrap();
    loop {
        match rl.readline("> ") {
            Ok(line) => {
                if !repl.handle(&line) {
                    break;
                }
                rl.add_history_entry(line).ok();
            }
            Err(ReadlineError::Interrupted) => {
                println!("CTRL-C");
                break;
            }
            Err(ReadlineError::Eof) => {
                println!("CTRL-D");
                break;
            }
            Err(e) => {
                eprintln!("input error: {e}");
                break;
            }
        }
    }
}
