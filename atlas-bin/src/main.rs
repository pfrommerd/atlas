mod prompt;

use prompt::AtlasPrompt;
use reedline::{FileBackedHistory, Reedline, Signal};

use directories::ProjectDirs;

use atlas_parse::grammar::InputParser;
use atlas_parse::ast::{Input, ReplInput};
use atlas_parse::lexer::{Token, Lexer, SrcType};
use atlas_core::il::transpile::Transpile;

use atlas_core::il::grammar::ExprParser as CoreParser;
use atlas_core::il::lexer::{Lexer as CoreLexer, Token as CoreToken};

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "atlas")]
#[command(bin_name = "atlas")]
struct AtlasCli {
    #[clap(subcommand)]
    command: Option<AtlasCommand>
}

#[derive(Subcommand, Debug)]
enum AtlasCommand {
    Interactive,
    Eval {
        #[clap(trailing_var_arg = true)]
        expr: Vec<String>
    },
    Exec {
        file_path: PathBuf
    },
    Core {
        file_path: PathBuf
    },
    Net {
        file_path: PathBuf
    },
    NetInteractive
}

fn interactive() {
    // Setup the history
    let dirs = ProjectDirs::from("org", "atlas", "atlas").unwrap();
    let mut history_file = dirs.cache_dir().to_owned();
    history_file.push("history.txt");
    let history = Box::new(
        FileBackedHistory::with_file(5, history_file).unwrap()
    );

    let mut line_editor = Reedline::create().with_history(history);
    let prompt = AtlasPrompt::default();
    loop {
        let sig = line_editor.read_line(&prompt);
        match sig.unwrap() {
            Signal::Success(buffer) => {
                let lex = Lexer::new(
                    SrcType::Repl, &buffer
                );
                let v : Vec<Token<'_>> = lex.collect();
                if v.len() == 0 { continue }
                println!("Tokens: {v:?}");
                let parser = InputParser::new();
                let res = parser.parse(v);
                println!("Parse: {res:?}");
                if let Ok(Input::Repl(ReplInput::Expr(e))) = res {
                    let il = e.transpile();
                    println!("IL: {il:?}");
                }
            },
            Signal::CtrlC => continue,
            Signal::CtrlD => break
        }
    }
}

fn eval(expr: String) {
    let lex = Lexer::new(
        SrcType::Expr, &expr
    );
    let parser = InputParser::new();
    let res = parser.parse(lex);
    match res {
        Ok(Input::Expr(e)) => {
            let il = e.transpile();
            println!("IL: {il:?}");
        },
        Err(e) => {
            println!("Error: {e:?}");
        },
        _ => panic!("Unexpected parse result")
    }
}

fn exec(file_path : PathBuf) {
    let src = std::fs::read_to_string(file_path).unwrap();
    let lex = Lexer::new(
        SrcType::Module, &src
    );
    let parser = InputParser::new();
    let res = parser.parse(lex);
    match res {
        Ok(Input::Module(e)) => {
            println!("{e:?}");
        },
        Err(e) => {
            println!("Error: {e:?}");
        },
        _ => panic!("Unexpected parse result")
    }
}

fn core(file_path : PathBuf) {
    let src = std::fs::read_to_string(file_path).unwrap();
    let lex = CoreLexer::new(&src);
    let parser = CoreParser::new();
    let v : Vec<CoreToken<'_>> = lex.collect();
    let res = parser.parse(v);
    match res {
        Ok(e) => {
            println!("{e:?}");
        },
        Err(e) => {
            println!("Error: {e:?}");
        },
    }
}

fn net(file_path : PathBuf) {
    let src = std::fs::read_to_string(file_path).unwrap();
    let lex = CoreLexer::new(&src);
    let parser = CoreParser::new();
    let v : Vec<CoreToken<'_>> = lex.collect();
    let res = parser.parse(v);
    match res {
        Ok(e) => {
            println!("{e:?}");
        },
        Err(e) => {
            println!("Error: {e:?}");
        },
    }
}

fn main() {
    let cli = AtlasCli::parse();
    let command = cli.command.unwrap_or(AtlasCommand::Interactive);

    use AtlasCommand::*;
    match command {
        Interactive => interactive(),
        Eval { expr }=> {
            let expr = expr.join(" ");
            eval(expr)
        },
        Exec { file_path } => exec(file_path),
        Core { file_path } => core(file_path),
        Net { file_path } => net(file_path)
    }
}
