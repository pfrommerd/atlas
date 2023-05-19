mod prompt;
mod env_info;

use prompt::AtlasPrompt;
use reedline::{FileBackedHistory, Reedline, Signal};

use directories::ProjectDirs;

use atlas_parse::grammar::ReplInputParser;
use atlas_parse::ast::ReplInput;
use atlas_parse::lexer::{Token, Lexer};
use atlas_core::il::transpile::Transpile;

fn main() {
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
                let lex = Lexer::new(&buffer);
                let v : Vec<Token<'_>> = lex.collect();
                if v.len() == 0 { continue }
                println!("Tokens: {v:?}");
                let parser = ReplInputParser::new();
                let res = parser.parse(v);
                println!("Parse: {res:?}");
                if let Ok(ReplInput::Expr(e)) = res {
                    let il = e.transpile();
                    println!("IL: {il:?}");
                }
            },
            Signal::CtrlC => continue,
            Signal::CtrlD => break
        }
    }
}