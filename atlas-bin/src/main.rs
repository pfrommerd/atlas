//! The `atlas` terminal application: an extensible agent-oriented TUI shell.

mod app;
mod daemon;
mod input;
mod protocol;
mod ui;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "atlas", about = "The Atlas agentic terminal")]
pub struct Args {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    Serve {
        #[arg(long)]
        socket: Option<std::path::PathBuf>,
    },
}

fn main() -> std::io::Result<()> {
    let args = Args::parse();
    if let Some(Command::Serve { socket }) = args.command {
        let socket = socket.unwrap_or(daemon::default_socket()?);
        let runtime = tokio::runtime::Runtime::new()?;
        return runtime.block_on(daemon::serve(&socket));
    }
    let terminal = ratatui::init();
    let result = app::run(terminal);
    ratatui::restore();
    result
}
