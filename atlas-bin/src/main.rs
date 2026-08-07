//! The `atlas` terminal application: an extensible agent-oriented TUI shell.

mod app;
mod client;
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

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let args = Args::parse();
    if let Some(Command::Serve { socket }) = args.command {
        let socket = socket.unwrap_or(daemon::default_socket()?);
        return daemon::serve(&socket).await;
    }
    let (client, sessions) = client::DaemonClient::connect_or_start().await?;
    let terminal = ratatui::init();
    let result = app::run(terminal, client, sessions).await;
    ratatui::restore();
    result
}
