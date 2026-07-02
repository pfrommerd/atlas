use chumsky::extra;
use chumsky::input::{MappedInput, Stream, ValueInput};
use chumsky::pratt::*;
use chumsky::prelude::*;
use chumsky::span::SimpleSpan;
use logos::Logos;
use ordered_float::OrderedFloat;

use crate::core::ast::{Binding, InfixOp, Literal, Node, Pattern};
use crate::vm::term::UnaryOp;

type ParserError<'tokens, 'src> = extra::Err<Rich<'tokens, Token<'src>>>;

pub fn literal<'tokens, 'src: 'tokens, I>()
-> impl Parser<'tokens, I, Literal<'src>, ParserError<'tokens, 'src>> + Clone
where
    I: ValueInput<'tokens, Token = Token<'src>, Span = SimpleSpan>,
{
    select! {
        Token::Integer(i) => Literal::Integer(i),
        Token::Float(x) => Literal::Float(x),
        Token::Bool(b) => Literal::Bool(b),
        Token::Char(c) => Literal::Char(c),
        Token::String(s) => Literal::String(s),
    }
}

pub fn infix_op<'tokens, 'src: 'tokens, I>()
-> impl Parser<'tokens, I, InfixOp, ParserError<'tokens, 'src>>
where
    I: ValueInput<'tokens, Token = Token<'src>, Span = SimpleSpan>,
{
    select! {
        Token::Plus => InfixOp::Add,
        Token::Minus => InfixOp::Sub,
        Token::Star => InfixOp::Mul,
        Token::Slash => InfixOp::Div,
        Token::TildeSlash => InfixOp::IDiv,
        Token::Percent => InfixOp::Mod,
        Token::AndAnd => InfixOp::And,
        Token::OrOr => InfixOp::Or,
        Token::Shl => InfixOp::Shl,
        Token::Caret => InfixOp::Xor,
    }
}

/// A binder on the LHS of a lambda or let: `x`, `&x` (auto-dup), `&{a b}`
/// (explicit dup) or `_` (hole).
pub fn binding<'tokens, 'src: 'tokens, I>()
-> impl Parser<'tokens, I, Binding<'src>, ParserError<'tokens, 'src>> + Clone
where
    I: ValueInput<'tokens, Token = Token<'src>, Span = SimpleSpan>,
{
    choice((
        // Uppercase names are ordinary variables, so they bind like identifiers.
        select! {
            Token::Identifier(name) => Binding::Var { name, auto_dup: false },
            Token::Constructor(name) => Binding::Var { name, auto_dup: false },
            Token::Underscore => Binding::Hole
        },
        // &{a b c} explicit dup (all names share one duplication)
        just(Token::Ampersand)
            .ignore_then(just(Token::LBrace))
            .ignore_then(
                select! {
                    Token::Identifier(name) => name
                }
                .repeated()
                .collect::<Vec<_>>(),
            )
            .then_ignore(just(Token::RBrace))
            .map(|names| Binding::Dup { names }),
        // &x for auto-dup of x
        just(Token::Ampersand).ignore_then(select! {
            Token::Identifier(name) => Binding::Var { name, auto_dup: true },
            Token::Constructor(name) => Binding::Var { name, auto_dup: true },
        }),
    ))
}

pub fn expr<'tokens, 'src: 'tokens, I>()
-> impl Parser<'tokens, I, Node<'src>, ParserError<'tokens, 'src>>
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
        let lit = literal().map(|lit| Node::Lit { val: lit });
        let wild = just(Token::Underscore).map(|_| Node::Wild);
        // variables: x, Foo (uppercase names are ordinary variables now), %name
        let var = select! {
            Token::Identifier(name) => Node::Var { name },
            Token::Constructor(name) => Node::Var { name },
            Token::PriId(name) => Node::Primitive { name }
        };
        // type declaration: the delimiter signals the kind.
        //   product/tuple: `type ( typeExpr,* )` — bare field type-expressions
        //   sum/enum:      `type { Variant,* }` — a variant is a bare `Name`
        //                  (nullary) or `Name( typeExpr,* )`
        let product_type = term
            .clone()
            .separated_by(just(Token::Comma))
            .allow_trailing()
            .collect::<Vec<_>>()
            .delimited_by(just(Token::LParen), just(Token::RParen))
            .map(|fields| Node::ProductType { fields });
        let variant = select! { Token::Constructor(name) => name }
            .then(
                term.clone()
                    .separated_by(just(Token::Comma))
                    .allow_trailing()
                    .collect::<Vec<_>>()
                    .delimited_by(just(Token::LParen), just(Token::RParen))
                    .or_not(),
            )
            .map(|(name, args)| (name, args.unwrap_or_default()));
        let sum_type = variant
            .separated_by(just(Token::Comma))
            .allow_trailing()
            .collect::<Vec<_>>()
            .delimited_by(just(Token::LBrace), just(Token::RBrace))
            .map(|variants| Node::SumType { variants });
        let type_decl =
            just(Token::TypeKw).ignore_then(choice((product_type, sum_type)));
        // lambda: \ Binding* . term
        // where Binding = x | &x | &{a b c} | _
        let binding = binding();
        let lambda = just(Token::Backslash)
            .ignore_then(binding.clone().repeated().at_least(1).collect::<Vec<_>>())
            .then_ignore(just(Token::Arrow))
            .then(term.clone())
            .map(|(binders, body)| {
                if binders.len() > 0 {
                    Node::Lambda {
                        binders,
                        body: Box::new(body),
                    }
                } else {
                    body
                }
            });
        // match: ?{ pattern binders* -> term; ... ; _ -> term }
        // The default branch is written `_ -> term` (erasing) or `x -> term` (a
        // lowercase identifier binding the whole scrutinee); both route to the
        // default. There is no bare trailing default after cases (a bare term
        // there is indistinguishable from the start of a new case and is
        // rejected). The bare-term `term.or_not()` below survives only as the
        // zero-case use-form `?{ term }` (and `?{}` is erasure).
        let pattern = choice((
            select! { Token::Constructor(name) => Pattern::Ctr(name) },
            literal().map(|lit| Pattern::Lit(lit)),
            just(Token::Cons).map(|_| Pattern::Cons),
            just(Token::LBracket)
                .ignore_then(just(Token::RBracket))
                .map(|_| Pattern::Nil),
            just(Token::Underscore).map(|_| Pattern::Default),
            // a lowercase identifier arm is the default, binding the scrutinee.
            select! { Token::Identifier(name) => Pattern::Bind(name) },
        ));
        let cases = pattern
            .then(binding.clone().repeated().collect::<Vec<_>>())
            .then_ignore(just(Token::Arrow))
            .then(term.clone())
            .map(|((pat, binders), body)| {
                let body = if binders.is_empty() {
                    body
                } else {
                    Node::Lambda {
                        binders,
                        body: Box::new(body),
                    }
                };
                (pat, body)
            })
            .separated_by(just(Token::Semicolon))
            .collect::<Vec<_>>();
        let mat = just(Token::Question)
            .ignore_then(just(Token::LBrace))
            .ignore_then(cases)
            .then(term.clone().or_not())
            .then_ignore(just(Token::RBrace))
            .map(|(cases, default)| {
                if cases.is_empty() && default.is_none() {
                    Node::Erase
                } else {
                    Node::Match {
                        cases,
                        default: default.map(Box::new),
                    }
                }
            });
        // explicit list constructor [node, node, ...]
        // gets desugared to #Con{node, #Con{node, #Con{node, #Nil}}}
        let list = just(Token::LBracket)
            .ignore_then(
                term.clone()
                    .separated_by(just(Token::Comma))
                    .collect::<Vec<_>>(),
            )
            .then_ignore(just(Token::RBracket))
            .map(|elems| Node::List { elems });
        // sup: `&{a, b}` (and `&{}` for erasure)
        let sup = just(Token::Ampersand)
            .ignore_then(just(Token::LBrace))
            .ignore_then(
                term.clone()
                    .separated_by(just(Token::Comma))
                    .allow_trailing()
                    .collect::<Vec<_>>(),
            )
            .then_ignore(just(Token::RBrace))
            .map(|nodes| {
                if nodes.is_empty() {
                    Node::Erase
                } else {
                    Node::Sup { nodes }
                }
            });
        // fix: the Y-combinator atom. `fix f` reduces to `f (fix f)`.
        let fix = just(Token::Fix).map(|_| Node::Fix);
        // All atoms
        let atom =
            choice((group, lit, wild, type_decl, var, list, mat, sup, lambda, fix));
        // Postfix variant selector: `atom :: Name` (binds tighter than application).
        let selected = atom.foldl(
            just(Token::ColonColon)
                .ignore_then(select! { Token::Constructor(name) => name })
                .repeated(),
            |ty, name| Node::Ctr {
                ty: Box::new(ty),
                // `::New` is the product constructor (no variant); any other
                // name selects a sum variant.
                variant: (name != "New").then_some(name),
            },
        );
        // Handle either atom or application
        let app = selected
            .repeated()
            .at_least(1)
            .collect::<Vec<_>>()
            .map(|mut atoms| {
                let start = atoms.remove(0);
                match atoms.len() {
                    0 => start,
                    _ => Node::App {
                        func: Box::new(start),
                        args: atoms,
                    },
                }
            });
        let infix_op = |prec: u16, tok: Token<'src>, op: InfixOp| {
            infix(left(prec), just(tok), move |l, _, r, _| Node::Infix {
                left: Box::new(l),
                op,
                right: Box::new(r),
            })
        };
        let top_level = app.pratt((
            // prefix unary operators bind tighter than any infix operator
            prefix(10, just(Token::Minus), |_, e, _| Node::Unary {
                op: UnaryOp::Neg,
                expr: Box::new(e),
            }),
            prefix(10, just(Token::Tilde), |_, e, _| Node::Unary {
                op: UnaryOp::Not,
                expr: Box::new(e),
            }),
            prefix(10, just(Token::TypeOf), |_, e, _| Node::Unary {
                op: UnaryOp::TypeOf,
                expr: Box::new(e),
            }),
            infix_op(9, Token::Cons, InfixOp::Cons),
            // `^ is the xor operator
            infix_op(8, Token::Caret, InfixOp::Xor),
            infix_op(7, Token::Star, InfixOp::Mul),
            infix_op(7, Token::Slash, InfixOp::Div),
            infix_op(7, Token::TildeSlash, InfixOp::IDiv),
            infix_op(7, Token::Percent, InfixOp::Mod),
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
        ));
        let let_binding = binding
            .then_ignore(just(Token::Equals))
            .then(top_level.clone())
            .then_ignore(just(Token::Semicolon));
        choice((let_binding
            .repeated()
            .collect::<Vec<_>>()
            .then(top_level.clone())
            .map(|(bindings, body)| {
                if bindings.len() > 0 {
                    Node::Let {
                        bindings,
                        body: Box::new(body),
                    }
                } else {
                    body
                }
            }),))
    })
}

/// Parse a single source expression into an AST [`Node`].
pub fn parse<'src>(input: &'src str) -> Result<Node<'src>, String> {
    let lexer = Lexer::new(input);
    let stream = lexer.into_stream();
    expr().parse(stream).into_result().map_err(|errs| {
        errs.iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    })
}

/// A single REPL entry: either a bare expression to evaluate, or one or more
/// bodyless `lhs = rhs;` declarations to bind.
#[derive(Debug, Clone)]
pub enum ReplInput<'src> {
    Expr(Node<'src>),
    Decl(Vec<(Binding<'src>, Node<'src>)>),
}

/// Parser for a [`ReplInput`]. A run of `binding '=' expr`, separated (and
/// optionally terminated) by `';'`, followed by end-of-input is a
/// [`ReplInput::Decl`]; anything else is parsed as a whole expression (which may
/// itself be a `let; body`). Each branch must reach the end of input, so a
/// trailing body (e.g. `x = 1; x`) falls through to `Expr`.
pub fn repl_input<'tokens, 'src: 'tokens, I>()
-> impl Parser<'tokens, I, ReplInput<'src>, ParserError<'tokens, 'src>>
where
    I: ValueInput<'tokens, Token = Token<'src>, Span = SimpleSpan>,
{
    let decl = binding()
        .then_ignore(just(Token::Equals))
        .then(expr())
        .separated_by(just(Token::Semicolon))
        .allow_trailing()
        .at_least(1)
        .collect::<Vec<_>>();
    choice((
        decl.then_ignore(end()).map(ReplInput::Decl),
        expr().then_ignore(end()).map(ReplInput::Expr),
    ))
}

/// Parse a single REPL entry into a [`ReplInput`].
pub fn parse_repl<'src>(input: &'src str) -> Result<ReplInput<'src>, String> {
    let lexer = Lexer::new(input);
    let stream = lexer.into_stream();
    repl_input().parse(stream).into_result().map_err(|errs| {
        errs.iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    })
}

#[derive(Logos, Debug, PartialEq, Eq, Clone)]
#[rustfmt::skip]
pub enum Token<'src> {
    // don't match _ as an identifier
    #[regex(r"([a-z][a-zA-Z0-9_]*)|([a-z_][a-zA-Z0-9_]+)")]
    Identifier(&'src str),
    #[regex(r"[A-Z][a-zA-Z0-9_]*")]
    Constructor(&'src str),
    #[regex("%[a-zA-Z_][a-zA-Z0-9_]*", |lex| &lex.slice()[1..])]
    PriId(&'src str),
    // Literals
    // Float before Integer: `1.5` is a single Float (longest match wins over `1`).
    #[regex(r"[0-9]+\.[0-9]+([eE][+-]?[0-9]+)?", |lex| lex.slice().parse::<f64>().map(OrderedFloat).map_err(|_| ()))]
    Float(OrderedFloat<f64>),
    #[regex(r"[0-9]+", |lex| lex.slice().parse().map_err(|_| ()))]
    Integer(u64),
    // Keyword tokens beat the identifier regex on equal length (literal > regex).
    #[token("true", |_| true)]
    #[token("false", |_| false)]
    Bool(bool),
    #[regex(r"'([^'\\]|\\.)'", |lex| lex.slice()[1..lex.slice().len()-1].chars().next())]
    Char(char),
    #[regex(r#""([^"\\]|\\.)*""#, |lex| &lex.slice()[1..lex.slice().len()-1])]
    String(&'src str),

    // Keywords (literal tokens beat the identifier regex on equal length).
    #[token("typeof")] TypeOf,
    #[token("type")]   TypeKw,
    #[token("fix")]    Fix,

    #[token("%")] Percent,
    #[token("&")] Ampersand,
    #[token("!")] Bang,
    #[token("=")] Equals,
    #[token(";")] Semicolon,
    #[token("#")] Hash,
    #[token(",")] Comma,
    #[token("\\")] Backslash,
    #[token("::")] ColonColon,
    #[token(":")] Colon,
    #[token(".")] Dot,
    #[token("(")] LParen,
    #[token(")")] RParen,
    #[token("{")] LBrace,
    #[token("}")] RBrace,
    #[token("[")] LBracket,
    #[token("]")] RBracket,
    #[token("$")] Dollar,
    #[token("?")] Question,

    #[token("<>")] Cons,
    #[token("_")] Underscore,
    #[token("->")] Arrow,

    // Operators
    #[token("^")] Caret,
    #[token("+")] Plus, #[token("-")] Minus,
    #[token("*")] Star, #[token("/")] Slash,
    #[token("~/")] TildeSlash,
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
            Err(_) => (Token::Error, span.into()),
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
                        .with_config(
                            ariadne::Config::new().with_index_type(ariadne::IndexType::Byte),
                        )
                        .with_code(3)
                        .with_message(err.to_string())
                        .with_label(
                            Label::new(((), err.span().into_range()))
                                .with_message(err.reason().to_string())
                                .with_color(Color::Red),
                        )
                        .finish()
                        .write(Source::from(input), &mut output)
                        .unwrap();
                    let err = String::from_utf8(output).unwrap();
                    println!("{err}");
                }
                Err(())
            }
        }
    }

    #[test]
    fn test_parse_simple() {
        assert_eq!(
            parse("123"),
            Ok(Node::Lit {
                val: Literal::Integer(123)
            })
        );
        assert_eq!(
            parse("'a'"),
            Ok(Node::Lit {
                val: Literal::Char('a')
            })
        );
        assert_eq!(
            parse("\"foo\""),
            Ok(Node::Lit {
                val: Literal::String("foo")
            })
        );
        assert_eq!(
            parse("[1, 2, 3]"),
            Ok(Node::List {
                elems: vec![
                    Node::Lit {
                        val: Literal::Integer(1)
                    },
                    Node::Lit {
                        val: Literal::Integer(2)
                    },
                    Node::Lit {
                        val: Literal::Integer(3)
                    },
                ],
            })
        );
        assert_eq!(parse("x"), Ok(Node::Var { name: "x" }));
        assert_eq!(parse("%foo"), Ok(Node::Primitive { name: "foo" }));
        // Uppercase names are ordinary variables now.
        assert_eq!(parse("Foo"), Ok(Node::Var { name: "Foo" }));
        assert_eq!(
            parse("&{a, b}"),
            Ok(Node::Sup {
                nodes: vec![Node::Var { name: "a" }, Node::Var { name: "b" },],
            })
        );
        assert_eq!(
            parse("2+x"),
            Ok(Node::Infix {
                left: Box::new(Node::Lit {
                    val: Literal::Integer(2)
                }),
                op: InfixOp::Add,
                right: Box::new(Node::Var { name: "x" })
            })
        );
        assert_eq!(
            parse("x <> y"),
            Ok(Node::Infix {
                left: Box::new(Node::Var { name: "x" }),
                op: InfixOp::Cons,
                right: Box::new(Node::Var { name: "y" })
            })
        );
        assert_eq!(parse("_"), Ok(Node::Wild));
        assert_eq!(parse("%foo"), Ok(Node::Primitive { name: "foo" }));
        // assert!(parse("↑x").is_ok());
    }

    #[test]
    fn test_parse_type_decl() {
        // A sum type with one variant carrying args and one nullary variant.
        assert_eq!(
            parse("type { Cons(T, List T), Nil }"),
            Ok(Node::SumType {
                variants: vec![
                    (
                        "Cons",
                        vec![
                            Node::Var { name: "T" },
                            Node::App {
                                func: Box::new(Node::Var { name: "List" }),
                                args: vec![Node::Var { name: "T" }],
                            },
                        ],
                    ),
                    ("Nil", vec![]),
                ],
            })
        );
        // Parenthesized type expressions are product fields, not variants.
        assert_eq!(
            parse("type (Int, Int)"),
            Ok(Node::ProductType {
                fields: vec![
                    Node::Var { name: "Int" },
                    Node::Var { name: "Int" },
                ],
            })
        );
        // Empty product (unit) and empty sum stay distinct.
        assert_eq!(parse("type ()"), Ok(Node::ProductType { fields: vec![] }));
        assert_eq!(parse("type {}"), Ok(Node::SumType { variants: vec![] }));
        // Nullary + payload mix.
        assert_eq!(
            parse("type { A(Int), B }"),
            Ok(Node::SumType {
                variants: vec![
                    ("A", vec![Node::Var { name: "Int" }]),
                    ("B", vec![]),
                ],
            })
        );
    }

    #[test]
    fn test_parse_variant_selector() {
        // `(List Float)::Cons` selects the Cons variant of the list type value.
        assert_eq!(
            parse("(List Float)::Cons"),
            Ok(Node::Ctr {
                ty: Box::new(Node::App {
                    func: Box::new(Node::Var { name: "List" }),
                    args: vec![Node::Var { name: "Float" }],
                }),
                variant: Some("Cons"),
            })
        );
        // `::` binds tighter than application.
        assert_eq!(
            parse("T::Cons 1"),
            Ok(Node::App {
                func: Box::new(Node::Ctr {
                    ty: Box::new(Node::Var { name: "T" }),
                    variant: Some("Cons"),
                }),
                args: vec![Node::Lit {
                    val: Literal::Integer(1)
                }],
            })
        );
    }

    #[test]
    fn test_parse_fix() {
        // `fix` is a bare atom (the Y-combinator).
        assert_eq!(parse("fix"), Ok(Node::Fix));
        // `fix f` applies it.
        assert_eq!(
            parse("fix f"),
            Ok(Node::App {
                func: Box::new(Node::Fix),
                args: vec![Node::Var { name: "f" }],
            })
        );
    }

    #[test]
    fn test_parse_typeof() {
        assert_eq!(
            parse("typeof x"),
            Ok(Node::Unary {
                op: UnaryOp::TypeOf,
                expr: Box::new(Node::Var { name: "x" }),
            })
        );
    }

    #[test]
    fn test_parse_match() {
        assert_eq!(
            parse(r"?{X -> x; Y -> y }"),
            Ok(Node::Match {
                cases: vec![
                    (Pattern::Ctr("X"), Node::Var { name: "x" }),
                    (Pattern::Ctr("Y"), Node::Var { name: "y" }),
                ],
                default: None,
            })
        );
    }

    #[test]
    fn test_match_identifier_default() {
        // A lowercase identifier arm binds the scrutinee and routes to the default.
        assert_eq!(
            parse(r"?{1 -> 0; x -> x + 1}"),
            Ok(Node::Match {
                cases: vec![
                    (
                        Pattern::Lit(Literal::Integer(1)),
                        Node::Lit {
                            val: Literal::Integer(0)
                        }
                    ),
                    (
                        Pattern::Bind("x"),
                        Node::Infix {
                            left: Box::new(Node::Var { name: "x" }),
                            op: InfixOp::Add,
                            right: Box::new(Node::Lit {
                                val: Literal::Integer(1)
                            }),
                        }
                    ),
                ],
                default: None,
            })
        );
    }

    #[test]
    fn test_match_default_is_underscore_arrow() {
        // The default branch is written `_ -> term`.
        assert!(parse(r"?{X -> 1; _ -> 2}").is_ok());
        // A bare trailing term after cases is NOT a default: it is swallowed as
        // the start of a new case (a literal pattern) and rejected for lacking
        // an arrow. Defaults must use `_ ->`.
        assert!(parse(r"?{X -> 1; 2}").is_err());
        // The bare-term body survives only as the zero-case use-form.
        assert!(parse(r"?{\x -> x}").is_ok());
    }

    #[test]
    fn test_parse_lam_app_let() {
        assert_eq!(
            parse(r"\x &y &{a b} _ -> f 1 + y + x"),
            Ok(Node::Lambda {
                binders: vec![
                    Binding::Var {
                        name: "x",
                        auto_dup: false
                    },
                    Binding::Var {
                        name: "y",
                        auto_dup: true
                    },
                    Binding::Dup {
                        names: vec!["a", "b"]
                    },
                    Binding::Hole,
                ],
                body: Box::new(Node::Infix {
                    left: Box::new(Node::Infix {
                        left: Box::new(Node::App {
                            func: Box::new(Node::Var { name: "f" }),
                            args: vec![Node::Lit {
                                val: Literal::Integer(1)
                            }]
                        }),
                        op: InfixOp::Add,
                        right: Box::new(Node::Var { name: "y" }),
                    }),
                    op: InfixOp::Add,
                    right: Box::new(Node::Var { name: "x" }),
                })
            })
        );
        assert_eq!(
            parse(r"\x -> f (1 + x)"),
            Ok(Node::Lambda {
                binders: vec![Binding::Var {
                    name: "x",
                    auto_dup: false
                }],
                body: Box::new(Node::App {
                    func: Box::new(Node::Var { name: "f" }),
                    args: vec![Node::Infix {
                        left: Box::new(Node::Lit {
                            val: Literal::Integer(1)
                        }),
                        op: InfixOp::Add,
                        right: Box::new(Node::Var { name: "x" })
                    }]
                })
            })
        );
        assert_eq!(
            parse(r"(\x -> f) (1 + y)"),
            Ok(Node::App {
                func: Box::new(Node::Lambda {
                    binders: vec![Binding::Var {
                        name: "x",
                        auto_dup: false
                    }],
                    body: Box::new(Node::Var { name: "f" })
                }),
                args: vec![Node::Infix {
                    left: Box::new(Node::Lit {
                        val: Literal::Integer(1)
                    }),
                    op: InfixOp::Add,
                    right: Box::new(Node::Var { name: "y" })
                }]
            })
        );
        assert_eq!(
            parse(r"x = 42; x"),
            Ok(Node::Let {
                bindings: vec![(
                    Binding::Var {
                        name: "x",
                        auto_dup: false
                    },
                    Node::Lit {
                        val: Literal::Integer(42)
                    }
                )],
                body: Box::new(Node::Var { name: "x" })
            })
        );
    }
}

impl<'src> std::fmt::Display for Token<'src> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Token::Identifier(id) => write!(f, "{}", id),
            Token::Constructor(id) => write!(f, "{}", id),
            Token::PriId(id) => write!(f, "%{}", id),
            Token::Integer(i) => write!(f, "{}", i),
            Token::Float(x) => write!(f, "{}", x),
            Token::Bool(b) => write!(f, "{}", b),
            Token::Percent => write!(f, "%"),
            Token::Ampersand => write!(f, "&"),
            Token::Bang => write!(f, "!"),
            Token::Equals => write!(f, "="),
            Token::Semicolon => write!(f, ";"),
            Token::Hash => write!(f, "#"),
            Token::Comma => write!(f, ","),
            Token::Backslash => write!(f, "\\"),
            Token::TypeOf => write!(f, "typeof"),
            Token::TypeKw => write!(f, "type"),
            Token::Fix => write!(f, "fix"),
            Token::ColonColon => write!(f, "::"),
            Token::Colon => write!(f, ":"),
            Token::Dot => write!(f, "."),
            Token::LParen => write!(f, "("),
            Token::RParen => write!(f, ")"),
            Token::LBrace => write!(f, "{{"),
            Token::RBrace => write!(f, "}}"),
            Token::LBracket => write!(f, "["),
            Token::RBracket => write!(f, "]"),
            Token::Cons => write!(f, "<>"),
            Token::Underscore => write!(f, "_"),
            Token::Dollar => write!(f, "$"),
            Token::Question => write!(f, "?"),
            Token::Arrow => write!(f, "->"),
            Token::Char(c) => write!(f, "'{}'", c),
            Token::String(s) => write!(f, "\"{}\"", s),
            Token::Plus => write!(f, "+"),
            Token::Minus => write!(f, "-"),
            Token::Star => write!(f, "*"),
            Token::Slash => write!(f, "/"),
            Token::TildeSlash => write!(f, "~/"),
            Token::Caret => write!(f, "^"),
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
