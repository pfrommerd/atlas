//! The `atlas` terminal application: a ratatui REPL over both atlas languages
//! (core evaluates; the surface language parses to an AST for now), with a
//! collapsible heap-explorer / reduction-stepper side panel.

mod app;
mod eval;
mod explorer;
mod input;
mod session;
mod ui;

use clap::{Parser, ValueEnum};
use std::path::PathBuf;

use atlas_core::vm::heap::Heap;
use session::LangMode;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum LangArg {
    Core,
    Atlas,
    Agent,
}

impl From<LangArg> for LangMode {
    fn from(lang: LangArg) -> LangMode {
        match lang {
            LangArg::Core => LangMode::Core,
            LangArg::Atlas => LangMode::Atlas,
            LangArg::Agent => LangMode::Agent,
        }
    }
}

#[derive(Parser)]
#[command(name = "atlas", about = "The Atlas interactive terminal")]
pub struct Args {
    /// Startup language mode (switch at runtime with /lang).
    #[arg(long, value_enum, default_value_t = LangArg::Core)]
    lang: LangArg,

    /// Reduction budget: the maximum number of interactions per evaluation.
    #[arg(long, default_value_t = 1_000_000)]
    budget: u64,

    /// Start with strong normalization (reduce under binders) enabled.
    #[arg(long)]
    strong: bool,

    /// Do not load the embedded core prelude at startup.
    #[arg(long)]
    no_prelude: bool,

    /// Source a file at startup (repeatable): `.atc` (core) is evaluated into
    /// the session locals, `.at` (atlas) is parsed.
    #[arg(long, short = 's', value_name = "FILE")]
    source: Vec<PathBuf>,
}

fn main() -> std::io::Result<()> {
    let args = Args::parse();
    // Reduction is driven by recursive `async` combinators, so its stack use
    // grows with term/dup-chain depth. Run the whole app on a thread with a
    // large stack so deep-but-finite reductions don't overflow.
    std::thread::Builder::new()
        .stack_size(512 * 1024 * 1024)
        .spawn(move || run(args))
        .expect("failed to spawn UI thread")
        .join()
        .expect("UI thread panicked")
}

fn run(args: Args) -> std::io::Result<()> {
    // `ratatui::init` installs a panic hook that restores the terminal.
    let terminal = ratatui::init();
    // The whole session runs inside one branded heap so locals, results, and
    // the pending evaluation hold live terms across inputs.
    let heap = Heap::new();
    let result = heap.with(|h| app::run(h, &args, terminal));
    ratatui::restore();
    result
}
