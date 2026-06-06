//! A small interactive REPL for the Atlas interaction calculus.
//!
//! Type an expression to evaluate it, or use a backslash "slash command":
//!
//!   \budget <n>   set the reduction budget (max interactions per evaluation)
//!   \verbose      toggle verbose mode: print the term after every interaction
//!   \help         show the command list
//!   \quit         leave the REPL (Ctrl-D also works)
//!
//! In verbose mode the executor is stepped one interaction at a time (a budget
//! of 1, with the interaction counter reset each step) so the reduction can be
//! printed line-by-line.

use clap::Parser;
use rustyline::error::ReadlineError;

use atlas_core::core::ast::desugar;
use atlas_core::core::parse::parse;
use atlas_core::vm::DEFAULT_BUDGET;
use atlas_core::vm::Printer;
use atlas_core::vm::exec::{Executor, FiniteBudget};
use atlas_core::vm::heap::Heap;
use atlas_core::vm::term::Node;

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

impl Repl {
    /// Parse, desugar and lower `src` into a fresh heap, returning its root.
    fn load(src: &str, heap: &mut Heap) -> Result<Node, String> {
        let node = parse(src)?;
        let expr = desugar(&node)?;
        heap.lower(&expr)
    }

    /// Evaluate one source expression and print the result (or each step, in
    /// verbose mode).
    fn eval(&self, src: &str) {
        let mut heap = Heap::new();
        let root = match Self::load(src, &mut heap) {
            Ok(root) => root,
            Err(e) => {
                eprintln!("error: {e}");
                return;
            }
        };

        if self.verbose {
            // Budget of 1, with the counter reset each step, so the reduction
            // can be printed one interaction at a time.
            let mut exec = Executor::new(&mut heap, FiniteBudget::new(1));
            // Show the starting term, then step one interaction at a time.
            println!("{}", Printer::new(exec.heap).show(root));
            loop {
                exec.policy.itrs = 0;
                let step = exec.normalize(root);
                // No interaction was performed: we have reached normal form.
                if exec.policy.itrs == 0 {
                    break;
                }
                println!("{}", Printer::new(exec.heap).show(step));
            }
        } else {
            let mut exec = Executor::new(&mut heap, FiniteBudget::new(self.budget));
            let result = exec.normalize(root);
            println!("{}", Printer::new(exec.heap).show(result));
            if exec.policy.itrs >= self.budget {
                eprintln!("(budget of {} interactions exhausted)", self.budget);
            }
        }
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
                None => eprintln!("usage: \\budget <n>"),
            },
            Some("verbose") => {
                self.verbose = !self.verbose;
                println!("verbose = {}", self.verbose);
            }
            Some("help") => help(),
            Some("quit") | Some("exit") => return false,
            Some(other) => eprintln!("unknown command: \\{other} (try \\help)"),
            None => eprintln!("usage: \\<command> (try \\help)"),
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
