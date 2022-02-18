use ordered_float::NotNan;

use std::fmt;

use super::ast::{ByteIndex, ByteOffset, Span};
use super::slicer::StringSlicer;

use self::LexicalError::*;

// different types of literals
// might add more
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub enum StringLiteral<'input> {
    Escaped(&'input str),
    Raw(&'input str),
}

impl StringLiteral<'_> {
    pub fn unescape(&self) -> String {
        match self {
            StringLiteral::Raw(s) => s.to_string(),
            StringLiteral::Escaped(st) => {
                let mut s: &str = st; // get a mutable reference
                let mut esc = String::new();
                while let Some(i) = s.bytes().position(|ch| ch == b'\\') {
                    let c = match s.as_bytes()[i + 1] {
                        b'\'' => '\'',
                        b'"' => '"',
                        b'\\' => '\\',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        _ => panic!("Bad escape sequence"),
                    };
                    esc.push_str(&s[..i]);
                    esc.push(c);
                    s = &s[i + 2..];
                }
                esc.push_str(s);
                esc
            }
        }
    }
}

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub enum Token<'input> {
    Doc(&'input str), // documentation string

    Identifier(&'input str),
    Constructor(&'input str),
    Macro(&'input str),    // any identifier that ends with an exclamation mark
    Operator(&'input str), // these are all infixable operators
    UnaryOperator(&'input str), // these are all unary operators starting with a !
    Minus,                 // - both prefix and infix

    StringLiteral(StringLiteral<'input>),
    CharLiteral(char),
    IntLiteral(i64),
    FloatLiteral(NotNan<f64>),
    BoolLiteral(bool),

    Let,  // let
    Use,  // use
    As,   // as
    From, // from

    In,  // in
    And, // and

    Pub, // pub
    Rec,
    Cache,

    Fn, // fn

    Match, // match
    With,  // with

    If,   // if
    Then, // then
    Else, // else

    Colon,       // :
    Semicolon,   // ;
    DoubleColon, // ::
    Comma,       // ,
    Dot,         // .
    Star,        // *
    StarStar,    // **
    StarStarStar,// ***
    Hash,        // #
    Cash,        // $
    MatchTo,     // =>
    Equals,      // =
    Pipe,        // |
    RArrow,      // ->
    LArrow,      // <-
    Question,    // ?
    Tilde,       // ~
    At,          // @
    Ampersand,   // &

    Underscore, // _

    LParen, // (
    RParen, // )

    LBrace, // {
    RBrace, // }

    LDoubleBrace, // {
    RDoubleBrace, // }

    LBracket, // [
    RBracket, // ]

    Begin, // begin
    End,   // end
}

impl<'input> fmt::Display for Token<'input> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use self::Token::*;
        match self {
            Doc(s) => write!(f, "Doc(\"{}\")", s),
            Identifier(s) => write!(f, "Id({})", s),
            Macro(s) => write!(f, "Macro({})", s),
            UnaryOperator(s) => write!(f, "UnaryOp({})", s),
            Operator(s) => write!(f, "Op({})", s),
            StringLiteral(s) => write!(f, "Str({})", s.unescape()),
            CharLiteral(c) => write!(f, "Char({})", c),
            IntLiteral(i) => write!(f, "Int({})", i),
            FloatLiteral(n) => write!(f, "Float({})", n),
            BoolLiteral(b) => {
                if *b {
                    write!(f, "True")
                } else {
                    write!(f, "False")
                }
            }
            _ => {
                let s = match self {
                    Let => "Let",
                    Use => "Use",
                    As => "As",
                    From => "From",
                    In => "In",
                    And => "And",
                    Pub => "Pub",
                    Rec => "Rec",
                    Cache => "Cache",
                    Fn => "Fn",
                    Match => "Match",
                    With => "With",
                    If => "If",
                    Then => "Then",
                    Else => "Else",
                    Colon => "Colon",
                    StarStar => "StarStar",
                    StarStarStar => "StarStarStar",
                    Semicolon => "Semicolon",
                    DoubleColon => "DoubleColon",
                    Comma => "Comma",
                    Dot => "Dot",
                    Equals => "Equals",
                    Pipe => "Pipe",
                    RArrow => "RArrow",
                    Question => "Question",
                    MatchTo => "MatchTo",
                    LParen => "LParen",
                    RParen => "RParen",
                    LBrace => "LBrace",
                    RBrace => "RBrace",
                    LBracket => "LBracket",
                    RBracket => "RBracket",
                    Begin => "Begin",
                    End => "End",
                    _ => "Unknown",
                };
                return s.fmt(f);
            }
        }
    }
}

// The actual lexer/tokinizer

#[derive(Clone, Debug)]
pub enum LexicalError {
    Unknown,
    Internal(Span, &'static str),

    UnterminatedStringLiteral(Span),
    UnterminatedCharLiteral(Span),

    UnterminatedComment(Span),

    BadNumericLiteral(Span, &'static str),

    UnexpectedChar(Span),
    InvalidRawStringDelmiter(Span),
}

#[derive(Clone)]
pub struct Lexer<'input> {
    chars: StringSlicer<'input>,
}

type LexerItem<'input> = Result<(ByteIndex, Token<'input>, ByteIndex), LexicalError>;

impl<'input> Lexer<'input> {
    pub fn new(input: &'input str) -> Self {
        Lexer {
            chars: StringSlicer::new(input),
        }
    }

    // TODO: Allow for string literals that span multiple lines like
    //          "foobar\n"
    //          "bar"
    // with just whitespace in between to be read as a single string token
    // and disallow strings that go over multiple lines! (should allow for better errors)

    fn raw_string_literal(&mut self) -> LexerItem<'input> {
        if let Some((start, 'r', _)) = self.chars.next() {
            if let Some((_, '"', content_start)) = self.chars.next() {
                let mut content_end = content_start;
                let mut end = content_start;

                // now that we have r" parsed grab parts of the raw string
                // until we hit the closing "
                while let Some((_, ch, cend)) = self.chars.next() {
                    end = cend;
                    if ch == '"' {
                        break;
                    }
                    content_end = cend;
                }

                if end == content_end {
                    // if we never hit a " the end and content_end will be the same
                    return Err(UnterminatedStringLiteral(Span::new(start, end)));
                }

                // grab the slice from content_start to content_end
                Ok((
                    start,
                    Token::StringLiteral(StringLiteral::Raw(
                        self.chars.slice(content_start, content_end),
                    )),
                    end,
                ))
            } else {
                Err(Internal(
                    Span::new(self.chars.pos(), self.chars.pos()),
                    "Expected \" for starting raw string",
                ))
            }
        } else {
            Err(Internal(
                Span::new(self.chars.pos(), self.chars.pos()),
                "Expected 'r' for raw string",
            ))
        }
    }

    fn string_literal(&mut self) -> LexerItem<'input> {
        if let Some((start, '"', content_start)) = self.chars.next() {
            let mut content_end = content_start;
            let mut end = content_start;
            let mut escaped = false;

            // now that we have " parsed grab parts of the string
            // until we hit the closing "
            while let Some((_, ch, cend)) = self.chars.next() {
                end = cend;
                if ch == '"' && !escaped {
                    break;
                }
                escaped = ch == '\\'; // whether the last character was escaped
                content_end = cend;
            }

            if end == content_end {
                // if we never hit a " the end and content_end will be the same
                return Err(UnterminatedStringLiteral(Span::new(start, end)));
            }

            // grab the slice from content_start to content_end
            Ok((
                start,
                Token::StringLiteral(StringLiteral::Escaped(
                    self.chars.slice(content_start, content_end),
                )),
                end,
            ))
        } else {
            Err(Internal(
                Span::new(self.chars.pos(), self.chars.pos()),
                "Expected \" for starting raw string",
            ))
        }
    }

    fn char_literal(&mut self) -> LexerItem<'input> {
        if let Some((start, '\'', char_pos)) = self.chars.next() {
            if let Some((_, ch, char_end)) = self.chars.next() {
                if let Some((_, '\'', end)) = self.chars.next() {
                    Ok((start, Token::CharLiteral(ch), end))
                } else {
                    Err(UnterminatedCharLiteral(Span::new(start, char_end)))
                }
            } else {
                Err(UnterminatedCharLiteral(Span::new(start, char_pos)))
            }
        } else {
            Err(Internal(
                Span::new(self.chars.pos(), self.chars.pos()),
                "Expected \' for character literal",
            ))
        }
    }

    fn numeric_literal(&mut self) -> LexerItem<'input> {
        let start = self.chars.pos();

        let neg = if let Some((_, '-', _)) = self.chars.peek() {
            self.chars.next();
            true
        } else {
            false
        };

        // take while we have a 0-10

        // save the first character in case we need to check if it is a zero
        let (_, first_ch, _) = self.chars.peek().unwrap();

        let (hdr_start, hdr_end) = self.chars.take_while(|ch| ch.is_digit(10));

        // match on delim
        match self.chars.peek() {
            Some((_, '.', _)) => {
                // we have a float!
                // get the rest of the float
                self.chars.next();
                let (_, float_end) = self.chars.take_while(|ch| ch.is_digit(10));

                // check following character
                match self.chars.peek() {
                    Some((ch_start, ch, ch_end)) if is_ident_start(ch) => {
                        return Err(BadNumericLiteral(
                            Span::new(ch_start, ch_end),
                            "Unexpected character after numeric type",
                        ))
                    }
                    _ => {}
                }

                // parse it
                match self.chars.slice(start, float_end).parse::<f64>() {
                    Ok(val) => {
                        let f = NotNan::new(val).ok().unwrap();
                        Ok((start, Token::FloatLiteral(f), float_end))
                    }
                    Err(_) => Err(BadNumericLiteral(
                        Span::new(start, float_end),
                        "Failed to parse float literal",
                    )),
                }
            }
            Some((b_start, 'b', b_end)) => {
                // we (might!) have a binary integer
                if hdr_end - hdr_start != ByteOffset(2) || first_ch != '0' {
                    return Err(BadNumericLiteral(
                        Span::new(b_start, b_end),
                        "Unexpected b when not a binary literal",
                    ));
                } else {
                    // grab the binary integer
                    let (binary_start, binary_end) = self.chars.take_while(|ch| ch.is_digit(2));

                    // check following character
                    match self.chars.peek() {
                        Some((ch_start, ch, ch_end)) if is_ident_start(ch) => {
                            return Err(BadNumericLiteral(
                                Span::new(ch_start, ch_end),
                                "Unexpected character after numeric type",
                            ))
                        }
                        _ => {}
                    }

                    // parse it
                    match i64::from_str_radix(self.chars.slice(binary_start, binary_end), 2) {
                        Ok(val) => Ok((
                            start,
                            Token::IntLiteral(if neg { -val } else { val }),
                            binary_end,
                        )),
                        Err(_) => Err(BadNumericLiteral(
                            Span::new(start, binary_end),
                            "Failed to parse binary literal",
                        )),
                    }
                }
            }
            Some((x_start, 'x', x_end)) => {
                // we (might!) have a hexidecimal integer
                if hdr_end - hdr_start != ByteOffset(2) || first_ch != '0' {
                    return Err(BadNumericLiteral(
                        Span::new(x_start, x_end),
                        "Unexpected x when not a hexidecimal literal",
                    ));
                } else {
                    // grab the hex integer
                    let (hex_start, hex_end) = self.chars.take_while(|ch| ch.is_digit(16));

                    // check following character
                    match self.chars.peek() {
                        Some((ch_start, ch, ch_end)) if is_ident_start(ch) => {
                            return Err(BadNumericLiteral(
                                Span::new(ch_start, ch_end),
                                "Unexpected character after numeric type",
                            ))
                        }
                        _ => {}
                    }

                    // parse it
                    match i64::from_str_radix(self.chars.slice(hex_start, hex_end), 16) {
                        Ok(val) => Ok((
                            start,
                            Token::IntLiteral(if neg { -val } else { val }),
                            hex_end,
                        )),
                        Err(_) => Err(BadNumericLiteral(
                            Span::new(start, hex_end),
                            "Failed to parse hex literal",
                        )),
                    }
                }
            }
            _ => {
                // check following character
                match self.chars.peek() {
                    Some((ch_start, ch, ch_end)) if is_ident_start(ch) => {
                        return Err(BadNumericLiteral(
                            Span::new(ch_start, ch_end),
                            "Unexpected character after numeric type",
                        ))
                    }
                    _ => {}
                }

                // parse decimal integer
                match i64::from_str_radix(self.chars.slice(hdr_start, hdr_end), 10) {
                    Ok(val) => Ok((
                        start,
                        Token::IntLiteral(if neg { -val } else { val }),
                        hdr_end,
                    )),
                    Err(_) => Err(BadNumericLiteral(
                        Span::new(start, hdr_end),
                        "Failed to parse hex literal",
                    )),
                }
            }
        }
    }

    fn operator(&mut self) -> LexerItem<'input> {
        let (start, end) = self.chars.take_while(is_operator_char);

        let op = self.chars.slice(start, end);
        let first = op.chars().next().unwrap();

        let token = match op {
            ":" => Token::Colon,
            "::" => Token::DoubleColon,
            "." => Token::Dot,
            "**" => Token::StarStar,
            "***" => Token::StarStarStar,
            "=>" => Token::MatchTo,
            "=" => Token::Equals,
            "$" => Token::Cash,
            "#" => Token:: Hash,
            "|" => Token::Pipe,
            "&" => Token::Ampersand,
            "-" => Token::Minus,
            "->" => Token::RArrow,
            "<-" => Token::LArrow,
            op if first == '!' || first == '~' || first == '?' => Token::UnaryOperator(op),
            op => Token::Operator(op),
        };

        Ok((start, token, end))
    }

    fn identifier(&mut self) -> LexerItem<'input> {
        let (_, c, _) = self.chars.peek().unwrap();
    
        let (start, end) = self.chars.take_while(is_ident_continue);

        let ident = self.chars.slice(start, end);

        let token = match ident {
            "let" => Token::Let,
            "use" => Token::Use,
            "as" => Token::As,
            "from" => Token::From,
            "in" => Token::In,
            "and" => Token::And,
            "pub" => Token::Pub,
            "rec" => Token::Rec,
            "cache" => Token::Cache,
            "fn" => Token::Fn,
            "match" => Token::Match,
            "with" => Token::With,
            "if" => Token::If,
            "then" => Token::Then,
            "else" => Token::Else,
            "begin" => Token::Begin,
            "end" => Token::End,
            "true" => Token::BoolLiteral(true),
            "false" => Token::BoolLiteral(false),
            ident => match self.chars.peek() {
                Some((_, '!', true_end)) => return Ok((start, Token::Macro(ident), true_end)),
                _ => {
                    if c.is_uppercase() {
                        Token::Constructor(ident)
                    } else {
                        Token::Identifier(ident)
                    }
                }
            },
        };

        return Ok((start, token, end));
    }

    fn line_comment(&mut self) -> Option<LexerItem<'input>> {
        let (start, end) = self.chars.take_while(|ch| ch != '\n');

        let comment: &'input str = self.chars.slice(start, end);

        if comment.starts_with("///") {
            let skip = if comment.starts_with("/// ") { 4 } else { 3 };
            let doc = Token::Doc(self.chars.slice(start + ByteOffset(skip), end));
            return Some(Ok((start, doc, end)));
        } else {
            return None;
        }
    }

    fn block_comment(&mut self) -> Option<LexerItem<'input>> {
        let start = self.chars.pos();
        self.chars.next(); // skip /
        self.chars.next(); // skip *

        // check whether there are three stars to see if
        // it is a doc comment
        let doc = if let Some((_, '*', _)) = self.chars.peek() {
            true
        } else {
            false
        };

        loop {
            let (cend, _) = self.chars.take_while(|ch| ch != '*');
            match self.chars.next() {
                Some((_, '/', end)) => {
                    // we hit the end
                    let content = self.chars.slice(start + ByteOffset(3), cend);
                    if doc {
                        return Some(Ok((start, Token::Doc(content), end)));
                    } else {
                        return None;
                    }
                }
                None => break,
                _ => continue,
            }
        }
        Some(Err(UnterminatedComment(Span::new(start, self.chars.pos()))))
    }
}

impl<'input> Iterator for Lexer<'input> {
    type Item = LexerItem<'input>;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some((start, ch, end)) = self.chars.peek() {
            return Some(match ch {
                ',' => {
                    self.chars.next();
                    Ok((start, Token::Comma, end))
                }
                ';' => {
                    self.chars.next();
                    Ok((start, Token::Semicolon, end))
                }

                '[' => {
                    self.chars.next();
                    Ok((start, Token::LBracket, end))
                }
                ']' => {
                    self.chars.next();
                    Ok((start, Token::RBracket, end))
                }
                '(' => {
                    self.chars.next();
                    Ok((start, Token::LParen, end))
                }
                ')' => {
                    self.chars.next();
                    Ok((start, Token::RParen, end))
                }

                '?' if !self.chars.test_peek(|c| is_ident_continue(c)) => {
                    self.chars.next();
                    Ok((start, Token::Question, end))
                }
                '~' if !self.chars.test_peek(|c| is_ident_continue(c)) => {
                    self.chars.next();
                    Ok((start, Token::Tilde, end))
                }
                '@' => {
                    self.chars.next();
                    Ok((start, Token::At, end))
                }

                '_' if !self.chars.test_peek(|c| is_ident_continue(c)) => {
                    self.chars.next();
                    Ok((start, Token::Underscore, end))
                }

                '{' => {
                    self.chars.next();
                    if self.chars.test_peek(|c| c == '{') {
                        Ok((
                            start,
                            Token::LDoubleBrace,
                            end + ByteOffset::from_char_len('{'),
                        ))
                    } else {
                        Ok((start, Token::LBrace, end))
                    }
                }
                '}' => {
                    self.chars.next();
                    if self.chars.test_peek(|c| c == '}') {
                        Ok((
                            start,
                            Token::RDoubleBrace,
                            end + ByteOffset::from_char_len('}'),
                        ))
                    } else {
                        Ok((start, Token::RBrace, end))
                    }
                }
                'r' if self.chars.test_look(2, |c| c == '"') => self.raw_string_literal(),
                '"' => self.string_literal(),

                '\'' if self.chars.test_look(2, |ch| ch == '\'') => self.char_literal(),
                '/' if self.chars.test_look(2, |ch| ch == '/') => match self.line_comment() {
                    Some(item) => item,
                    None => continue,
                },
                '/' if self.chars.test_look(2, |ch| ch == '*') => match self.block_comment() {
                    Some(item) => item,
                    None => continue,
                },

                ch if is_ident_start(ch) => self.identifier(),
                ch if ch.is_digit(10)
                    || (ch == '-' && self.chars.test_peek(|c| c.is_digit(10))) =>
                {
                    self.numeric_literal()
                }
                ch if is_operator_char(ch) => self.operator(),
                ch if ch.is_whitespace() => {
                    self.chars.next();
                    continue;
                }
                _ => Err(UnexpectedChar(Span::new(start, end))),
            });
        }

        // return a None Option
        None
    }
}

// utilities

fn is_operator_byte(c: u8) -> bool {
    macro_rules! match_token {
        ($($x: pat),*) => {
            match c {
                $($x)|* => true,
                _ => false,
            }
        }
    }
    match_token! {
        b'!',
        b'#',
        b'$',
        b'%',
        b'&',
        b'*',
        b'+',
        b'-',
        b'.',
        b'/',
        b'<',
        b'=',
        b'>',
        b'?',
        b'@',
        b'\\',
        b'^',
        b'|',
        b'~',
        b':'
    }
}

fn is_operator_char(ch: char) -> bool {
    (ch as u32) < 128 && is_operator_byte(ch as u8)
}

fn is_ident_start(ch: char) -> bool {
    ch.is_alphabetic() || ch == '_'
}

fn is_ident_continue(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

// tests

#[cfg(test)]
mod tests {
    use super::super::ast::ByteIndex;
    use super::Lexer;
    use super::Token;
    use ordered_float::NotNan;

    #[test]
    fn tokenize_basic() {
        let mut lexer = Lexer::new("let a:int=5 + 2.1");

        assert_eq!(
            lexer.next().unwrap().ok().unwrap(),
            (ByteIndex(0), Token::Let, ByteIndex(3))
        );
        assert_eq!(
            lexer.next().unwrap().ok().unwrap(),
            (ByteIndex(4), Token::Identifier("a"), ByteIndex(5))
        );
        assert_eq!(
            lexer.next().unwrap().ok().unwrap(),
            (ByteIndex(5), Token::Colon, ByteIndex(6))
        );
        assert_eq!(
            lexer.next().unwrap().ok().unwrap(),
            (ByteIndex(6), Token::Identifier("int"), ByteIndex(9))
        );
        assert_eq!(
            lexer.next().unwrap().ok().unwrap(),
            (ByteIndex(9), Token::Equals, ByteIndex(10))
        );
        assert_eq!(
            lexer.next().unwrap().ok().unwrap(),
            (ByteIndex(10), Token::IntLiteral(5), ByteIndex(11))
        );
        assert_eq!(
            lexer.next().unwrap().ok().unwrap(),
            (ByteIndex(12), Token::Operator("+"), ByteIndex(13))
        );
        assert_eq!(
            lexer.next().unwrap().ok().unwrap(),
            (
                ByteIndex(14),
                Token::FloatLiteral(NotNan::new(2.1).unwrap()),
                ByteIndex(17)
            )
        );
    }
}
