use logos::Logos;
use ordered_float::NotNan;

// Lexer definition

// The token for the core language
#[derive(Logos, Debug, PartialEq, Eq, Clone)]
pub enum Token<'src> {
    #[regex(r"[ \t\n\f]*", logos::skip)]
    Whitespace,
    #[regex(r"/\*([^\*]*\*[^/])*[^\*]*\*/", logos::skip)]
    BlockComment,
    #[regex(r"//[^\n]*", logos::skip)]
    LineComment,

    #[regex(r"[a-z][a-zA-Z_0-9]*")]
    Identifier(&'src str),

    #[regex(r"[A-Z][a-zA-Z_0-9]*(#[A-Z0-9]*)", |x| {
        let s = x.slice();
        (s, Some(s))
    })]
    Type((&'src str,Option<&'src str>)),

    #[regex(r"@[a-zA-Z][a-zA-Z_0-9]*", |x| {let s = x.slice(); &s[1..]})]
    Reference(&'src str),
    #[regex(r"\$[a-zA-Z][a-zA-Z_0-9]*", |x| {let s = x.slice(); &s[1..]})]
    Operator(&'src str),

    #[token("*")]
    Star,
    #[token("=")]
    Equals,
    #[token(",")]
    Comma,
    #[token("~")]
    Tilde,
    #[token("&")]
    Ampersand,

    #[token("(")] LParen, #[token(")")] RParen,
    #[token("{")] LBrace, #[token("}")] RBrace,
    #[token("[")] LBracket, #[token("]")] RBracket,

    #[regex(r"[0-9]+", |x| x.slice().parse())]
    Integer(u64),
    #[regex("\"[^\"]*\"", |x| {let s = x.slice(); &s[1..s.len() - 1]})]
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

pub use crate::net_grammar::{
    BookParser, RedexParser, NetParser
};