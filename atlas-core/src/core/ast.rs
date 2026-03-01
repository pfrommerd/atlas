use logos::Logos;


// An ast Node

pub enum Node<'src> {
    Identifier(&'src str),
    Error,
}

// The AST token type

#[derive(Logos, Debug, PartialEq, Eq, Clone)]
pub enum Token<'src> {
    #[regex(r"[a-z][a-zA-Z_0-9]*")]
    Identifier(&'src str),
    #[regex(r"[ \t\n\f]*", logos::skip)]
    Whitespace,
    #[regex(r"/\*([^\*]*\*[^/])*[^\*]*\*/", logos::skip)]
    BlockComment,
    #[regex(r"//[^\n]*", logos::skip)]
    LineComment,
    #[error]
    Error,
}

pub struct Lexer<'src> {
    logos_lex : logos::Lexer<'src, Token<'src>>,
}

impl<'src> Lexer<'src> {
    pub fn new(src: &'src str) -> Self {
        Lexer { logos_lex: Token::lexer(src) }
    }
}