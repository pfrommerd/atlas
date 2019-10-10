use ordered_float::NotNan;

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub enum Token<'input> {
    Doc(&'input str), // documentation string

    Identifier(&'input str),
    Operator(&'input str), // these are all infixable operators

    StringLiteral(StringLiteral<'input>),
    CharLiteral(char),
    IntLiteral(i64),
    ByteLiteral(u8),
    FloatLiteral(NotNan<f64>),
    BoolLiteral(bool),

    Let,            // let
    In,             // in

    Export,         // export

    Fun,            // fun

    Type,           // type

    Match,          // match
    With,           // with

    If,             // if
    Then,           // then
    Else,           // else

    Equals,         // =
    Colon,          // :
    Comma,          // ,
    At,             // @
    Dot,            // .
    Pipe,           // |
    RArrow,         // ->
    Exclamation,    // !

    LParen,         // (
    RParen,         // )

    LBrace,         // {
    RBrace,         // }

    LBracket,       // [
    RBracket,       // ]

    Begin,          // begin
    End             // end
}

// different types of literals
// might add more
pub enum StringLiteral {
    Escaped(&'input str),
    Raw(&'input str)
}

// The actual lexer/tokinizer

pub struct Lexer<'input> {
    chars: CharIndices<'input>,
}

impl<'input> Lexer<'input> {
    pub fn new(input: &'input str) -> Self {
        Lexer { chars: input.char_indices() }
    }
}

impl<'input> Iterator for Lexer<'input> {
}


