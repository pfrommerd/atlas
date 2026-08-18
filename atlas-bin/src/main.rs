//! The `atlas` terminal application: an extensible agent-oriented TUI shell.

mod app;
mod bundle;
mod client;
mod input;
mod ui;

use clap::Parser;

#[derive(Parser)]
#[command(name = "atlas", about = "The Atlas agentic terminal")]
pub struct Args {
    /// Restart the local broker before connecting.
    #[arg(short = 'r', long = "reset")]
    reset: bool,
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let args = Args::parse();
    let (client, sessions) = client::DaemonClient::connect_or_start(args.reset).await?;
    let terminal = ratatui::init();
    let result = app::run(terminal, client, sessions).await;
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_flag_accepts_short_and_long_forms() {
        assert!(Args::try_parse_from(["atlas", "-r"]).unwrap().reset);
        assert!(Args::try_parse_from(["atlas", "--reset"]).unwrap().reset);
        assert!(!Args::try_parse_from(["atlas"]).unwrap().reset);
    }
}
