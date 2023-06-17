pub use logos::Logos;
use ordered_float::NotNan;

#[derive(Logos, Debug, PartialEq, Eq, Clone)]
pub enum Token<'src> {
    #[regex(r"[ \t\n\f]*")]
    Whitespace(&'src str),
    #[regex(r"/\*([^\*]*\*[^/])*[^\*]*\*/")]
    BlockComment(&'src str),
    #[regex(r"//[^\n]*")]
    LineComment(&'src str),


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
    #[token("match")]
    Match,

    #[token("rec")]
    Rec,
    #[token("pub")]
    Pub,

    #[token("{")]
    LBrace,
    #[token("}")]
    RBrace,
    #[token("(")]
    LParen,
    #[token(")")]
    RParen,
    #[token("[")]
    LBracket,
    #[token("]")]
    RBracket,

    #[token(".")]
    Dot,
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
    #[regex(r"([~+/*@][+*@-]*|[-=][=~+*/@-]+)")]
    Operator(&'src str),
    #[regex(r"[a-z][a-zA-Z_0-9]*")]
    Identifier(&'src str),
    #[regex(r"[A-Z][a-zA-Z_0-9]*")]
    TypeIdentifier(&'src str),

    Unknown(&'src str),

    // Tokens to differentiate
    // the kind of thing we want to parse
    // This is a trick to make Lalrpop
    // only generate one parser
    StartExpr,
    StartModule,
    StartRepl,

    #[error]
    Error
}

use logos::Lexer as LogosLexer;

pub struct Lexer<'src> {
    logos_lex : LogosLexer<'src, Token<'src>>,
    start_token : Option<Token<'src>>
}

pub enum SrcType {
    Expr,
    Module,
    Repl
}

impl<'src> Lexer<'src> {
    pub fn new(src_type: SrcType, src: &'src str) -> Lexer {
        let start_token = match src_type {
            SrcType::Expr => Token::StartExpr,
            SrcType::Module => Token::StartModule,
            SrcType::Repl => Token::StartRepl
        };
        Lexer { 
            logos_lex: Token::lexer(src),
            start_token: Some(start_token)
        }
    }
}

impl<'src> Iterator for Lexer<'src> {
    type Item = Token<'src>;
    
    fn next(&mut self) -> Option<Self::Item> {
        if let Some(t) = self.start_token.take() {
            return Some(t);
        }
        match self.logos_lex.next() {
            // Remap error to unknown for better printouts
            Some(Token::Error) => Some(Token::Unknown(self.logos_lex.slice())),
            x => x
        }
    }
}


#[cfg(test)]
mod test {
    use super::Token;
    use logos::Logos;

    #[test]
    fn test_subtract() {
        let mut lex = Token::lexer("1-2");
        assert_eq!(Token::Integer(1), lex.next().unwrap());
        assert_eq!(Token::Minus, lex.next().unwrap());
        assert_eq!(Token::Integer(2), lex.next().unwrap());
    }
}