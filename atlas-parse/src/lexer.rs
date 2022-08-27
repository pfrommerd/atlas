use logos::Logos;

#[derive(Logos, Debug, PartialEq)]
pub enum Token<'src> {
    #[regex("[ \t\n]*")]
    Whitespace,

    #[token("enum")]
    Enum,
    #[token("fn")]
    Fn,
    #[token("let")]
    Let,
    #[token("if")]
    If,
    #[token("else")]
    Else,

    #[token("{")]
    LBrace,
    #[token("}")]
    RBrace,
    #[token("(")]
    LParen,
    #[token(")")]
    RParen,

    #[token(",")]
    Comma,
    #[token(":")]
    Colon,
    #[token(";")]
    Semicolon,

    #[token("=")]
    Equals,

    #[token("-")]
    Minus,
    #[regex("[+*/@][+*/@-]*")]
    Operator,
    #[regex(r"[a-zA-Z]\W*")]
    Identifier(&'src str),
    #[error]
    Error
}