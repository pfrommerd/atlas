use chumsky::input::{Input, MappedInput, Stream};
use chumsky::span::SimpleSpan;
use logos::Logos;
use ordered_float::NotNan;

#[derive(Logos, Debug, PartialEq, Clone)]
#[rustfmt::skip]
pub enum Token<'src> {
    // Identifiers: lowercase start -> value identifier, uppercase -> type/ctor.
    // Two alternatives so a lone `_` is the Underscore token, not an identifier.
    #[regex(r"([a-z][a-zA-Z0-9_]*)|([a-z_][a-zA-Z0-9_]+)")]
    Identifier(&'src str),
    #[regex(r"[A-Z][a-zA-Z0-9_]*")]
    TypeIdentifier(&'src str),

    // Literals.
    // Float before Integer: `1.5` is a single Float (longest match wins over `1`).
    #[regex(r"[0-9]+\.[0-9]+([eE][+-]?[0-9]+)?", |lex| lex.slice().parse::<f64>().ok().and_then(|f| NotNan::new(f).ok()).ok_or(()))]
    Float(NotNan<f64>),
    #[regex(r"[0-9]+", |lex| lex.slice().parse::<u64>().map_err(|_| ()))]
    Integer(u64),
    #[regex(r#"("[^"]*")|('[^']*')"#, |lex| { let s = lex.slice(); &s[1..s.len()-1] })]
    String(&'src str),
    // Keyword tokens beat the identifier regex on equal length (literal > regex).
    #[token("true")]  True,
    #[token("false")] False,

    // Keywords
    #[token("let")]   Let,
    #[token("fn")]    Fn,
    #[token("if")]    If,
    #[token("else")]  Else,
    #[token("match")] Match,
    #[token("enum")]  Enum,
    #[token("struct")]Struct,
    #[token("trait")] Trait,
    #[token("impl")]  Impl,
    #[token("mod")]   Mod,
    #[token("type")]  Type,
    #[token("pub")]   Pub,
    #[token("in")]    In,
    #[token("rec")]   Rec,

    // Delimiters / punctuation
    #[token("{")] LBrace,   #[token("}")] RBrace,
    #[token("(")] LParen,   SpacedLParen, #[token(")")] RParen,
    #[token("[")] LBracket, #[token("]")] RBracket,
    #[token(".")] Dot,      #[token(",")] Comma,
    #[token("::")] ColonColon, #[token(":")] Colon,
    #[token(";")] Semicolon,
    #[token("->")] Arrow,   #[token("=>")] FatArrow,
    #[token("_")] Underscore,

    // Operators
    #[token("==")] EqEq,    #[token("=")] Equals,
    #[token("!=")] Neq,     #[token("!")] Bang,
    #[token("<=")] Lte,     #[token("<<")] Shl, #[token("<")] Lt,
    #[token(">=")] Gte,     #[token(">>")] Shr, #[token(">")] Gt,
    #[token("&&")] AndAnd,  #[token("||")] OrOr,
    #[token("+")] Plus,     #[token("-")] Minus,
    #[token("*")] Star,     #[token("/")] Slash,
    #[token("%")] Percent,  #[token("^")] Caret,

    #[regex(r"\n+")]
    Newline,

    // Skipped trivia
    #[regex(r"[ \t\f\r]+", logos::skip)]
    Whitespace,
    #[regex(r"//[^\n]*", logos::skip, allow_greedy = true)]
    LineComment,
    #[regex(r"/\*([^*]|\*[^/])*\*/", logos::skip)]
    BlockComment,

    Error,
}

pub struct Lexer<'src> {
    src: &'src str,
    lexer: logos::SpannedIter<'src, Token<'src>>,
    paren_depth: usize,
    bracket_depth: usize,
    prev_end: usize,
}

impl<'src> Iterator for Lexer<'src> {
    type Item = (Token<'src>, SimpleSpan);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let (token, span) = self.lexer.next()?;
            let mut token = token.unwrap_or(Token::Error);
            if matches!(token, Token::LParen) && self.prev_end != 0 && span.start > self.prev_end {
                token = Token::SpacedLParen;
            }
            match token {
                Token::LParen | Token::SpacedLParen => self.paren_depth += 1,
                Token::RParen => self.paren_depth = self.paren_depth.saturating_sub(1),
                Token::LBracket => self.bracket_depth += 1,
                Token::RBracket => self.bracket_depth = self.bracket_depth.saturating_sub(1),
                Token::Newline if self.paren_depth > 0 || self.bracket_depth > 0 => continue,
                _ => {}
            }
            self.prev_end = span.end;
            return Some((token, span.into()));
        }
    }
}

impl<'src> Lexer<'src> {
    pub fn new(input: &'src str) -> Self {
        Self {
            src: input,
            lexer: Token::lexer(input).spanned(),
            paren_depth: 0,
            bracket_depth: 0,
            prev_end: 0,
        }
    }

    pub fn into_stream(
        self,
    ) -> MappedInput<
        'src,
        Token<'src>,
        SimpleSpan,
        Stream<Self>,
        fn((Token<'src>, SimpleSpan)) -> (Token<'src>, SimpleSpan),
    > {
        let len = self.src.len();
        Stream::from_iter(self).map((0..len).into(), |(token, span)| (token, span))
    }
}

impl<'src> std::fmt::Display for Token<'src> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use Token::*;
        match self {
            Identifier(s) | TypeIdentifier(s) | String(s) => write!(f, "{s}"),
            Integer(i) => write!(f, "{i}"),
            Float(x) => write!(f, "{x}"),
            True => write!(f, "true"),
            False => write!(f, "false"),
            Let => write!(f, "let"),
            Fn => write!(f, "fn"),
            If => write!(f, "if"),
            Else => write!(f, "else"),
            Match => write!(f, "match"),
            Enum => write!(f, "enum"),
            Struct => write!(f, "struct"),
            Trait => write!(f, "trait"),
            Impl => write!(f, "impl"),
            Mod => write!(f, "mod"),
            Type => write!(f, "type"),
            Pub => write!(f, "pub"),
            In => write!(f, "in"),
            Rec => write!(f, "rec"),
            LBrace => write!(f, "{{"),
            RBrace => write!(f, "}}"),
            LParen | SpacedLParen => write!(f, "("),
            RParen => write!(f, ")"),
            LBracket => write!(f, "["),
            RBracket => write!(f, "]"),
            Dot => write!(f, "."),
            Comma => write!(f, ","),
            ColonColon => write!(f, "::"),
            Colon => write!(f, ":"),
            Semicolon => write!(f, ";"),
            Arrow => write!(f, "->"),
            FatArrow => write!(f, "=>"),
            Underscore => write!(f, "_"),
            EqEq => write!(f, "=="),
            Equals => write!(f, "="),
            Neq => write!(f, "!="),
            Bang => write!(f, "!"),
            Lte => write!(f, "<="),
            Shl => write!(f, "<<"),
            Lt => write!(f, "<"),
            Gte => write!(f, ">="),
            Shr => write!(f, ">>"),
            Gt => write!(f, ">"),
            AndAnd => write!(f, "&&"),
            OrOr => write!(f, "||"),
            Plus => write!(f, "+"),
            Minus => write!(f, "-"),
            Star => write!(f, "*"),
            Slash => write!(f, "/"),
            Percent => write!(f, "%"),
            Caret => write!(f, "^"),
            Newline => write!(f, "<newline>"),
            Whitespace => write!(f, "<whitespace>"),
            LineComment => write!(f, "<line comment>"),
            BlockComment => write!(f, "<block comment>"),
            Error => write!(f, "<error>"),
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
        assert_eq!(Token::Integer(1), lex.next().unwrap().unwrap());
        assert_eq!(Token::Minus, lex.next().unwrap().unwrap());
        assert_eq!(Token::Integer(2), lex.next().unwrap().unwrap());
    }
}
