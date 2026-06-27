mod prompt;

use prompt::AtlasPrompt;
use reedline::{FileBackedHistory, Reedline, Signal};

use directories::ProjectDirs;

use atlas_parse::parser::{parse_expr, parse_module, parse_repl};

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "atlas")]
#[command(bin_name = "atlas")]
struct AtlasCli {
    #[clap(subcommand)]
    command: Option<AtlasCommand>,
}

#[derive(Subcommand, Debug)]
enum AtlasCommand {
    /// Interactive REPL: parse each line and print the AST.
    Interactive,
    /// Parse a single expression given on the command line.
    Eval {
        #[clap(trailing_var_arg = true)]
        expr: Vec<String>,
    },
    /// Parse a whole module from a file.
    Exec { file_path: PathBuf },
}

fn interactive() {
    // Setup the history
    let dirs = ProjectDirs::from("org", "atlas", "atlas").unwrap();
    let mut history_file = dirs.cache_dir().to_owned();
    history_file.push("history.txt");
    let history = Box::new(FileBackedHistory::with_file(5, history_file).unwrap());

    let mut line_editor = Reedline::create().with_history(history);
    let prompt = AtlasPrompt::default();
    loop {
        let sig = line_editor.read_line(&prompt);
        match sig.unwrap() {
            Signal::Success(buffer) => {
                if buffer.trim().is_empty() {
                    continue;
                }
                match parse_repl(&buffer) {
                    Ok(input) => {
                        println!("{input:#?}");
                        // TODO: lower the parsed AST to an atlas-core Expr here
                        // (lowering is currently unimplemented).
                    }
                    Err(e) => println!("Error:\n{e}"),
                }
            }
            Signal::CtrlC => continue,
            Signal::CtrlD => break,
        }
    }
}

fn eval(expr: String) {
    match parse_expr(&expr) {
        Ok(e) => println!("{e:#?}"),
        Err(e) => println!("Error:\n{e}"),
    }
}

fn exec(file_path: PathBuf) {
    let src = std::fs::read_to_string(file_path).unwrap();
    match parse_module(&src) {
        Ok(m) => println!("{m:#?}"),
        Err(e) => println!("Error:\n{e}"),
    }
}

fn main() {
    let cli = AtlasCli::parse();
    let command = cli.command.unwrap_or(AtlasCommand::Interactive);

    use AtlasCommand::*;
    match command {
        Interactive => interactive(),
        Eval { expr } => eval(expr.join(" ")),
        Exec { file_path } => exec(file_path),
    }
}
