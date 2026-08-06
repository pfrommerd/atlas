//! The `atlas` terminal application: an extensible agent-oriented TUI shell.

mod app;
mod input;
mod ui;

use clap::Parser;

#[derive(Parser)]
#[command(name = "atlas", about = "The Atlas agentic terminal")]
pub struct Args {}

fn main() -> std::io::Result<()> {
    let _args = Args::parse();
    let terminal = ratatui::init();
    let result = app::run(terminal);
    ratatui::restore();
    result
}
