pub use logos::Logos;
use ordered_float::NotNan;

// The token for the core language
#[derive(Logos, Debug, PartialEq, Eq, Clone)]
pub enum Token<'src> {
    #[regex(r"[ \t\n\f]*", logos::skip)]
    Whitespace,
    #[regex(r"/\*([^\*]*\*[^/])*[^\*]*\*/", logos::skip)]
    BlockComment,
    #[regex(r"//[^\n]*", logos::skip)]
    LineComment,

    #[regex(r"@?[a-zA-Z][a-zA-Z_0-9]*")]
    Identifier(&'src str),

    #[token("*")]
    Star,
    #[token(":")]
    Colon,
    #[token("_")]
    Hole,
    #[token(r"\")]
    Lam,
    #[token("$")]
    Dollar,
    #[token(r"#")]
    Pound,
    #[token("->")]
    Arrow,
    #[token(".")]
    Dot,
    #[token(",")]
    Comma,
    #[token("and")]
    And,
    #[token("in")]
    In,
    #[token("=")]
    Equals,
    #[token("(")]
    LParen,
    #[token(")")]
    RParen,

    #[regex(r"[0-9]+", |x| x.slice().parse())]
    Integer(u64),
    #[regex("(\"[^\"]*\")|('[^\']*')", |x| {let s = x.slice(); &s[1..s.len() - 1]})]
    String(&'src str),
    #[regex(r"[0-9]+\.[0-9]+", |x| x.slice().parse())]
    Float(NotNan<f64>),
    #[token("true")]
    True,
    #[token("false")]
    False,

    #[error]
    Error
}

use logos::Lexer as LogosLexer;

pub struct Lexer<'src> {
    logos_lex : LogosLexer<'src, Token<'src>>,
}

impl<'src> Lexer<'src> {
    pub fn new(src: &'src str) -> Lexer {
        Lexer { 
            logos_lex: Token::lexer(src)
        }
    }
}

impl<'src> Iterator for Lexer<'src> {
    type Item = Token<'src>;
    
    fn next(&mut self) -> Option<Self::Item> {
        self.logos_lex.next()
    }
}