use atlas_core::vm::ast::{Lexer, BookParser};

fn main() {
    let content = include_str!("example.net");
    let lexer = Lexer::new(content);
    let parser = BookParser::new();
    let book = parser.parse(lexer).unwrap();
    println!("Book: {:?}", book);
    println!();
    println!("Pretty-printed:");
    println!();
    println!("{}", book);
}
