use chumsky::extra;
use chumsky::input::{Stream, ValueInput, MappedInput};
use chumsky::prelude::*;
use chumsky::span::SimpleSpan;
use chumsky::pratt::*;
use logos::Logos;

use crate::core::ast::{InfixOp, Node, Literal, Pattern};

type ParserError<'tokens, 'src> = extra::Err<Rich<'tokens, Token<'src>>>;

pub fn literal<'tokens, 'src: 'tokens, I>() -> impl Parser<'tokens, I, Literal<'src>, ParserError<'tokens, 'src>> + Clone
where
    I: ValueInput<'tokens, Token = Token<'src>, Span = SimpleSpan>,
{
    select! {
        Token::Integer(i) => Literal::Integer(i),
        Token::Char(c) => Literal::Char(c),
        Token::String(s) => Literal::String(s),
    }
}

pub fn infix_op<'tokens, 'src: 'tokens, I>() -> impl Parser<'tokens, I, InfixOp, ParserError<'tokens, 'src>>
where
    I: ValueInput<'tokens, Token = Token<'src>, Span = SimpleSpan>,
{
    select! {
        Token::Plus => InfixOp::Add,
        Token::Minus => InfixOp::Sub,
        Token::Star => InfixOp::Mul,
        Token::Slash => InfixOp::Div,
        Token::Percent => InfixOp::Rem,
        Token::AndAnd => InfixOp::And,
        Token::OrOr => InfixOp::Or,
        Token::Tilde => InfixOp::Not,
        Token::Shl => InfixOp::Shl,
    }
}

pub fn expr<'tokens, 'src: 'tokens, I>() -> impl Parser<'tokens, I, Node<'src>, ParserError<'tokens, 'src>>
where
    I: ValueInput<'tokens, Token = Token<'src>, Span = SimpleSpan>,
{
    // We will build the parser recursively
    // since terms can contain terms.
    recursive(|term| {
        // --- ATOMS ---
        // grouping: (term)
        let group = just(Token::LParen)
                   .ignore_then(term.clone())
                   .then_ignore(just(Token::RParen));
        // literals: 123, 'a', "foo"
        let lit = literal().map(|lit| { Node::Lit { val: lit } });
        // binders: x, x₀, x₁, @name, %name, ^name
        let binders = choice((
            select! {
                Token::Dup0Id(name) => Node::Dp0 { name },
                Token::Dup1Id(name) => Node::Dp1 { name },
                Token::Identifier(name) => Node::Var { name },
                Token::RefId(name) => Node::Ref { name },
                Token::PriId(name) => Node::Pri { name },
                Token::NamId(name) => Node::Nam { name },
            },
        ));
        // ^(term term)
        let dry = just(Token::Caret).ignore_then(just(Token::LParen))
            .ignore_then(term.clone()).then(term.clone()).then_ignore(just(Token::RParen))
            .map(|(func, arg)| Node::Dry { func: Box::new(func), arg: Box::new(arg) });
        // #name or #name{term,term,...}
        let ctr = select! { Token::CtrId(name) => name }
            .then(
                just(Token::LBrace)
                .ignore_then(
                    term.clone()
                    .separated_by(just(Token::Comma)).collect::<Vec<_>>()
                ).then_ignore(just(Token::RBrace)).or_not()
            ).map(|(name, args)| {
                match args {
                    Some(args) => Node::Ctr { name, args },
                    None => Node::Ctr { name, args: vec![] },
                }
            });
        // λ{#name: term; term; default}
        // note that λ{term} is a special case of this and will be
        // transpiled to a uses case on desugaring.
        let pattern = choice((
            select! { Token::CtrId(name) => Pattern::Ctr(name) },
            literal().map(|lit| { Pattern::Lit(lit) }),
            just(Token::Cons).map(|_| { Pattern::Cons }),
            just(Token::Nil).map(|_| { Pattern::Nil }),
            just(Token::Underscore).map(|_| { Pattern::Default }),
        ));
        let cases = pattern.then_ignore(just(Token::Colon)).then(term.clone())
                    .separated_by(just(Token::Semicolon)).collect::<Vec<_>>();
        let mat = just(Token::Lambda).ignore_then(just(Token::LBrace))
            .ignore_then(cases).then(term.clone().or_not()).then_ignore(just(Token::RBrace))
            .map(|(cases, default)| {
                if cases.is_empty() && default.is_none() {
                    Node::Era
                } else if cases.is_empty() && let Some(default) = default {
                    Node::Use { term: Box::new(default) }
                } else if cases.len() == 1 && cases[0].0 == Pattern::Default && default.is_none() {
                    let case = cases.into_iter().next().unwrap();
                    Node::Use { term: Box::new(case.1) }
                } else {
                    Node::Mat { cases, default: default.map(Box::new) }
                }
            });
        // explicit list constructor [node, node, ...]
        // gets desugared to #Con{node, #Con{node, #Con{node, #Nil}}}
        let list = just(Token::LBracket).ignore_then(term.clone().separated_by(just(Token::Comma)).collect::<Vec<_>>())
            .then_ignore(just(Token::RBracket)).map(|elems| Node::List { elems });
        // [] gets its own token, handle it as a node as well
        let list = list.or(just(Token::Nil).map(|_| Node::List { elems: vec![] }));

        // All atoms
        let atom = choice((lit, binders, group, ctr, list, dry, mat));
        // Handle either atom or application
        let app = choice((
            atom.clone().then_ignore(just(Token::LParen))
                .then(term.clone().separated_by(just(Token::Comma)).collect::<Vec<_>>())
                .then_ignore(just(Token::RParen))
                .map(|(func, args)| Node::App { func: Box::new(func), args }),
            atom,
        ));
        let infix_op = |prec: u16, tok: Token<'src>, op: InfixOp| {
            infix(left(prec), just(tok), move |l, _, r, _| Node::Infix {
                left: Box::new(l), op, right: Box::new(r)
            })
        };
        app.pratt((
            infix_op(9, Token::Cons, InfixOp::Cons),
            // `^ is the xor operator
            infix_op(8, Token::BacktickCaret, InfixOp::Add),
            infix_op(7, Token::Star, InfixOp::Mul),
            infix_op(7, Token::Slash, InfixOp::Div),
            infix_op(7, Token::Percent, InfixOp::Rem),
            infix_op(6, Token::Plus, InfixOp::Add),
            infix_op(6, Token::Minus, InfixOp::Sub),
            infix_op(5, Token::Shl, InfixOp::Shl),
            infix_op(5, Token::Shr, InfixOp::Shr),
            infix_op(4, Token::Lt, InfixOp::Lt),
            infix_op(4, Token::Lte, InfixOp::Lte),
            infix_op(4, Token::Gt, InfixOp::Gt),
            infix_op(4, Token::Gte, InfixOp::Gte),
            infix_op(3, Token::EqEq, InfixOp::Eq),
            infix_op(3, Token::Neq, InfixOp::Neq),
            infix_op(2, Token::AndAnd, InfixOp::And),
            infix_op(1, Token::OrOr, InfixOp::Or),
        ))
    })
}

#[derive(Logos, Debug, PartialEq, Eq, Clone)]
#[rustfmt::skip]
pub enum Token<'src> {
    // don't match _ as an identifier
    #[regex(r"([a-zA-Z][a-zA-Z0-9_]*)|([a-zA-Z_][a-zA-Z0-9_]+)")]
    Identifier(&'src str),
    // match dup variables as separate tokens
    #[regex(r"([a-zA-Z_][a-zA-Z0-9_]*)₀", |lex| &lex.slice()[..lex.slice().len()-3])]
    Dup0Id(&'src str),
    #[regex(r"([a-zA-Z_][a-zA-Z0-9_]*)₁", |lex| &lex.slice()[..lex.slice().len()-3])]
    Dup1Id(&'src str),

    #[regex("%[a-zA-Z_][a-zA-Z0-9_]*", |lex| &lex.slice()[1..])]
    PriId(&'src str),
    #[regex("@[a-zA-Z_][a-zA-Z0-9_]*", |lex| &lex.slice()[1..])]
    RefId(&'src str),
    #[regex("#[a-zA-Z_][a-zA-Z0-9_]*", |lex| &lex.slice()[1..])]
    CtrId(&'src str),
    #[regex(r"\^[a-zA-Z_][a-zA-Z0-9_]*", |lex| &lex.slice()[1..])]
    NamId(&'src str),

    // Literals
    #[regex(r"[0-9]+", |lex| lex.slice().parse().map_err(|_| ()))]
    Integer(u64),
    #[regex(r"'([^'\\]|\\.)'", |lex| lex.slice()[1..lex.slice().len()-1].chars().next())]
    Char(char),
    #[regex(r#""([^"\\]|\\.)*""#, |lex| &lex.slice()[1..lex.slice().len()-1])]
    String(&'src str),

    #[token("@")] At,
    #[token("%")] Percent,
    #[token("^")] Caret,
    #[token("`^")] BacktickCaret,
    #[token("&")] Ampersand,
    #[token("!")] Bang,
    #[token("=")] Equals,
    #[token(";")] Semicolon,
    #[token("#")] Hash,
    #[token(",")] Comma,
    #[token("λ")] Lambda,
    #[token(":")] Colon,
    #[token(".")] Dot,
    #[token("(")] LParen,
    #[token(")")] RParen,
    #[token("{")] LBrace,
    #[token("}")] RBrace,
    #[token("[")] LBracket,
    #[token("]")] RBracket,
    #[token("↑")] UpArrow,
    #[token("$")] Dollar,

    #[token("<>")] Cons, #[token("[]")] Nil,
    #[token("_")] Underscore,

    // Operators
    #[token("+")] Plus, #[token("-")] Minus,
    #[token("*")] Star, #[token("/")] Slash,
    #[token("&&")] AndAnd, #[token("||")] OrOr,
    #[token("<<")] Shl, #[token(">>")] Shr,
    #[token("===")] EqEqEq, #[token("==")] EqEq,
    #[token("~")] Tilde, #[token("!=")] Neq,
    #[token("<")] Lt, #[token("<=")] Lte,
    #[token(">")] Gt, #[token(">=")] Gte,
    #[token(".&.")] DotAndDot, #[token(".|.")] DotOrDot,
    #[regex(r"[ \t\n\f\r]+", logos::skip)]
    Whitespace,
    #[regex(r"//[^\n]*", logos::skip, allow_greedy = true)]
    LineComment,
    Error,
}

pub struct Lexer<'src> {
    src: &'src str,
    lexer: logos::SpannedIter<'src, Token<'src>>,
}

impl<'src> Iterator for Lexer<'src> {
    type Item = (Token<'src>, SimpleSpan);

    fn next(&mut self) -> Option<Self::Item> {
        self.lexer.next().map(|(token, span)| match token {
            Ok(token) => (token, span.into()),
            Err(_) => (Token::Error, span.into())
        })
    }
}


impl<'src> Lexer<'src> {
    pub fn new(input: &'src str) -> Self {
        Self {
            src: input,
            lexer: Token::lexer(input).spanned(),
        }
    }
    pub fn into_stream(self) -> MappedInput<'src, Token<'src>, SimpleSpan, Stream<Self>,
                        fn((Token<'src>, SimpleSpan)) -> (Token<'src>, SimpleSpan)> {
        let len = self.src.len();
        Stream::from_iter(self).map((0..len).into(), |(token, span)| (token, span))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ariadne::{Color, Label, Report, ReportKind, Source};

    fn parse<'src>(input: &'src str) -> Result<Node<'src>, ()> {
        let lexer = Lexer::new(input);
        let stream = lexer.into_stream();
        let result = expr().parse(stream).into_result();
        match result {
            Ok(term) => Ok(term),
            Err(errs) => {
                for err in errs {
                    let mut output = Vec::new();
                    Report::build(ReportKind::Error, ((), err.span().into_range()))
                        .with_config(ariadne::Config::new().with_index_type(ariadne::IndexType::Byte))
                        .with_code(3)
                        .with_message(err.to_string())
                        .with_label(
                            Label::new(((), err.span().into_range()))
                                .with_message(err.reason().to_string())
                                .with_color(Color::Red),
                        )
                        .finish()
                        .write(Source::from(input), &mut output).unwrap();
                    let err = String::from_utf8(output).unwrap();
                    println!("{err}");
                }
                Err(())
            }
        }
    }

    #[test]
    fn test_parse_atom() {
        assert_eq!(parse("123"), Ok(Node::Lit { val: Literal::Integer(123) }));
        assert_eq!(parse("'a'"), Ok(Node::Lit { val: Literal::Char('a') }));
        assert_eq!(parse("\"foo\""), Ok(Node::Lit { val: Literal::String("foo") }));
        assert_eq!(parse("[1, 2, 3]"), Ok(Node::List {
            elems: vec![
                Node::Lit { val: Literal::Integer(1) },
                Node::Lit { val: Literal::Integer(2) },
                Node::Lit { val: Literal::Integer(3) },
            ],
        }));
        assert_eq!(parse("x"), Ok(Node::Var { name: "x" }));
        assert_eq!(parse("x₀"), Ok(Node::Dp0 { name: "x" }));
        assert_eq!(parse("x₁"), Ok(Node::Dp1 { name: "x" }));
        assert_eq!(parse("@foo"), Ok(Node::Ref { name: "foo" }));
        assert_eq!(parse("%foo"), Ok(Node::Pri { name: "foo" }));
        assert_eq!(parse("^foo"), Ok(Node::Nam { name: "foo" }));
        assert_eq!(parse("#Foo{a, b}"), Ok(Node::Ctr { name: "Foo", args: vec![
            Node::Var { name: "a" },
            Node::Var { name: "b" },
        ] }));
        assert_eq!(parse("&{a, b}"), Ok(Node::Sup { label: "L", left: Box::new(Node::Lit { val: Literal::String("a") }), right: Box::new(Node::Lit { val: Literal::String("b") }) }));
        assert_eq!(parse("2+x"), Ok(Node::Infix {
            left: Box::new(Node::Lit { val: Literal::Integer(2) }),
            op: InfixOp::Add,
            right: Box::new(Node::Var { name: "x" })
        }));
        assert_eq!(parse("x <> y"), Ok(Node::Infix { left: Box::new(Node::Var { name: "x" }), op: InfixOp::Cons, right: Box::new(Node::Var { name: "y" }) }));
        assert_eq!(parse("*"), Ok(Node::Any));
        assert_eq!(parse("@foo"), Ok(Node::Ref { name: "foo" }));
        assert_eq!(parse("%foo"), Ok(Node::Pri { name: "foo" }));
        assert_eq!(parse("^foo"), Ok(Node::Nam { name: "foo" }));
        assert_eq!(parse("#Foo{a, b}"), Ok(Node::Ctr { name: "Foo", args: vec![Node::Lit { val: Literal::String("a") }, Node::Lit { val: Literal::String("b") }] }));
        // assert!(parse("↑x").is_ok());
    }

    #[test]
    fn test_parse_app() {
        assert_eq!(parse("f(a)"), Ok(Node::App { func: Box::new(Node::Var { name: "f" }), args: vec![Node::Var { name: "a" }] }));
        assert_eq!(parse("f(a, b)"), Ok(Node::App { func: Box::new(Node::Var { name: "f" }), args: vec![Node::Var { name: "a" }, Node::Var { name: "b" }] }));
    }

    // #[test]
    // fn test_parse_ops() {
    //     assert!(parse("a + b * c").is_ok());
    //     assert!(parse("a === b").is_ok());
    //     assert!(parse("a .&. b").is_ok());
    // }

    // #[test]
    // fn test_parse_binders() {
    //     assert!(parse("λx. x").is_ok());
    //     assert!(parse("λx,y. x").is_ok());
    //     assert!(parse("!x = y; x").is_ok());
    //     assert!(parse("!!x = y; x").is_ok());
    //     assert!(parse("!x& = y; x").is_ok());
    //     assert!(parse("!x&L = y; x").is_ok());
    // }

    // #[test]
    // fn test_parse_superposition() {
    //     assert!(parse("&{a, b}").is_ok());
    //     assert!(parse("&L{a, b}").is_ok());
    //     assert!(parse("&{}").is_ok());
    // }

    // #[test]
    // fn test_parse_match() {
    //     assert!(parse("λ{ #Foo: a; _ : b }").is_ok());
    //     assert!(parse("λ{ 0: a; 1: b; _ : c }").is_ok());
    //     assert!(parse("λ{ a }").is_ok());
    //     assert!(parse("λ{}").is_ok());
    // }
}

impl<'src> std::fmt::Display for Token<'src> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Token::Identifier(id) => write!(f, "{}", id),
            Token::Dup0Id(id) => write!(f, "{}₀", id),
            Token::Dup1Id(id) => write!(f, "{}₁", id),
            Token::PriId(id) => write!(f, "%{}", id),
            Token::RefId(id) => write!(f, "@{}", id),
            Token::CtrId(id) => write!(f, "#{}", id),
            Token::NamId(id) => write!(f, "^{}", id),
            Token::Integer(i) => write!(f, "{}", i),
            Token::At => write!(f, "@"),
            Token::Percent => write!(f, "%"),
            Token::Caret => write!(f, "^"),
            Token::BacktickCaret => write!(f, "`^"),
            Token::Ampersand => write!(f, "&"),
            Token::Bang => write!(f, "!"),
            Token::Equals => write!(f, "="),
            Token::Semicolon => write!(f, ";"),
            Token::Hash => write!(f, "#"),
            Token::Comma => write!(f, ","),
            Token::Lambda => write!(f, "λ"),
            Token::Colon => write!(f, ":"),
            Token::Dot => write!(f, "."),
            Token::LParen => write!(f, "("),
            Token::RParen => write!(f, ")"),
            Token::LBrace => write!(f, "{{"),
            Token::RBrace => write!(f, "}}"),
            Token::LBracket => write!(f, "["),
            Token::RBracket => write!(f, "]"),
            Token::Cons => write!(f, "<>"),
            Token::Nil => write!(f, "[]"),
            Token::Underscore => write!(f, "_"),
            Token::UpArrow => write!(f, "↑"),
            Token::Dollar => write!(f, "$"),
            Token::Char(c) => write!(f, "'{}'", c),
            Token::String(s) => write!(f, "\"{}\"", s),
            Token::Plus => write!(f, "+"),
            Token::Minus => write!(f, "-"),
            Token::Star => write!(f, "*"),
            Token::Slash => write!(f, "/"),
            Token::AndAnd => write!(f, "&&"),
            Token::OrOr => write!(f, "||"),
            Token::Tilde => write!(f, "~"),
            Token::Shl => write!(f, "<<"),
            Token::Shr => write!(f, ">>"),
            Token::EqEqEq => write!(f, "==="),
            Token::EqEq => write!(f, "=="),
            Token::Neq => write!(f, "!="),
            Token::Lt => write!(f, "<"),
            Token::Lte => write!(f, "<="),
            Token::Gt => write!(f, ">"),
            Token::Gte => write!(f, ">="),
            Token::DotAndDot => write!(f, ".&."),
            Token::DotOrDot => write!(f, ".|."),
            Token::Whitespace => write!(f, "<whitespace>"),
            Token::LineComment => write!(f, "<line comment>"),
            Token::Error => write!(f, "<error>"),
        }
    }
}