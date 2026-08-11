//! The `atlas` terminal application: an extensible agent-oriented TUI shell.

mod app;
mod client;
mod input;
mod ui;

use clap::Parser;

#[derive(Parser)]
#[command(name = "atlas", about = "The Atlas agentic terminal")]
pub struct Args {
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let _ = Args::parse();
    let (client, sessions) = client::DaemonClient::connect_or_start().await?;
    let terminal = ratatui::init();
    let result = app::run(terminal, client, sessions).await;
    ratatui::restore();
    result
}
