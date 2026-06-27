use chumsky::extra;
use chumsky::input::ValueInput;
use chumsky::pratt::*;
use chumsky::prelude::*;
use chumsky::span::SimpleSpan;

use crate::ast::*;
use crate::lexer::{Lexer, Token};

type ParserError<'tokens, 'src> = extra::Err<Rich<'tokens, Token<'src>>>;

/// Literal atoms (everything except the `()` unit, which is handled inside the
/// parenthesised-expression atom).
fn literal<'tokens, 'src: 'tokens, I>()
-> impl Parser<'tokens, I, Literal<'src>, ParserError<'tokens, 'src>> + Clone
where
    I: ValueInput<'tokens, Token = Token<'src>, Span = SimpleSpan>,
{
    select! {
        Token::Integer(i) => Literal::Integer(i as i64),
        Token::Float(x) => Literal::Float(x),
        Token::String(s) => Literal::String(s),
        Token::True => Literal::Bool(true),
        Token::False => Literal::Bool(false),
    }
}

/// Type expressions. Currently only bare (type-)identifiers, mirroring the
/// original grammar — compound/generic types are out of scope.
fn type_<'tokens, 'src: 'tokens, I>()
-> impl Parser<'tokens, I, Type<'src>, ParserError<'tokens, 'src>> + Clone
where
    I: ValueInput<'tokens, Token = Token<'src>, Span = SimpleSpan>,
{
    select! {
        Token::Identifier(s) => Type::Identifier(s),
        Token::TypeIdentifier(s) => Type::Identifier(s),
    }
}

/// Patterns, used by `let`/`fn` bindings and `match` arms.
fn pattern<'tokens, 'src: 'tokens, I>()
-> impl Parser<'tokens, I, Pattern<'src>, ParserError<'tokens, 'src>> + Clone
where
    I: ValueInput<'tokens, Token = Token<'src>, Span = SimpleSpan>,
{
    recursive(|pattern| {
        let lit = literal().map(Pattern::Literal);
        let wild = just(Token::Underscore).to(Pattern::Wildcard);
        let ident = select! { Token::Identifier(s) => Pattern::Identifier(s) };
        // Constructor: `Foo` or `Foo(p, q)`.
        let ctor = select! { Token::TypeIdentifier(s) => s }
            .then(
                pattern
                    .clone()
                    .separated_by(just(Token::Comma))
                    .allow_trailing()
                    .collect::<Vec<_>>()
                    .delimited_by(just(Token::LParen), just(Token::RParen))
                    .or_not(),
            )
            .map(|(name, args)| Pattern::Constructor(name, args.unwrap_or_default()));
        // Parenthesised: grouping `(p)` or a tuple `(p, q)` / `()`.
        let paren = pattern
            .clone()
            .separated_by(just(Token::Comma))
            .allow_trailing()
            .collect::<Vec<_>>()
            .delimited_by(just(Token::LParen), just(Token::RParen))
            .map(|mut ps| {
                if ps.len() == 1 {
                    ps.pop().unwrap()
                } else {
                    Pattern::Tuple(ps)
                }
            });
        let base = choice((lit, wild, ctor, paren, ident));
        // Optional type ascription: `p : Type`.
        base.then(just(Token::Colon).ignore_then(type_()).or_not())
            .map(|(p, ty)| match ty {
                Some(ty) => Pattern::Typed(Box::new(p), ty),
                None => p,
            })
    })
}

/// Local helper enum for left-folding postfix operators onto an atom.
enum Postfix<'src> {
    Call(Vec<Expr<'src>>),
    Project(&'src str),
    Index(Expr<'src>),
}

/// Expression parser. Recursively handles atoms, postfix call/project/index,
/// prefix unary operators, and infix operators (via a pratt precedence table).
pub fn expr<'tokens, 'src: 'tokens, I>()
-> impl Parser<'tokens, I, Expr<'src>, ParserError<'tokens, 'src>> + Clone
where
    I: ValueInput<'tokens, Token = Token<'src>, Span = SimpleSpan>,
{
    recursive(|expr| {
        let comma_exprs = expr
            .clone()
            .separated_by(just(Token::Comma))
            .allow_trailing()
            .collect::<Vec<_>>();

        // Block `{ decl* expr? }`, reused as both an atom and `fn`/`mod` bodies.
        let block = declaration(expr.clone())
            .repeated()
            .collect::<Vec<_>>()
            .then(expr.clone().or_not())
            .delimited_by(just(Token::LBrace), just(Token::RBrace))
            .map(|(decls, value)| ExprBlock { decls, value });

        // --- ATOMS ---
        let lit = literal().map(Expr::Literal);

        // `foo` or a `foo::bar::baz` scope path.
        let path = select! { Token::Identifier(s) => s }
            .then(
                just(Token::ColonColon)
                    .ignore_then(select! { Token::Identifier(s) => s })
                    .repeated()
                    .collect::<Vec<_>>(),
            )
            .map(|(first, rest)| {
                if rest.is_empty() {
                    Expr::Identifier(first)
                } else {
                    let mut path = Vec::with_capacity(rest.len() + 1);
                    path.push(first);
                    path.extend(rest);
                    Expr::Scope(path)
                }
            });

        // Constructor: `Foo`, `Foo(a, b)`, or `Foo { x: a, y: b }`.
        let field = select! { Token::Identifier(s) => s }
            .then_ignore(just(Token::Colon))
            .then(expr.clone());
        let ctor = select! { Token::TypeIdentifier(s) => s }
            .then(
                choice((
                    field
                        .separated_by(just(Token::Comma))
                        .allow_trailing()
                        .collect::<Vec<_>>()
                        .delimited_by(just(Token::LBrace), just(Token::RBrace))
                        .map(CtorBody::Struct),
                    comma_exprs
                        .clone()
                        .delimited_by(just(Token::LParen), just(Token::RParen))
                        .map(CtorBody::Tuple),
                ))
                .or_not(),
            )
            .map(|(name, body)| {
                let ctor = match body {
                    None => Constructor::Empty(name),
                    Some(CtorBody::Struct(fields)) => Constructor::Struct(name, fields),
                    Some(CtorBody::Tuple(args)) => Constructor::Tuple(name, args),
                };
                Expr::Constructor(ctor)
            });

        // Parenthesised: `()` unit, `(e)` grouping, or `(a, b)` tuple.
        let paren = just(Token::LParen)
            .ignore_then(
                expr.clone()
                    .then(
                        just(Token::Comma)
                            .ignore_then(expr.clone())
                            .repeated()
                            .collect::<Vec<_>>(),
                    )
                    .then(just(Token::Comma).or_not())
                    .or_not(),
            )
            .then_ignore(just(Token::RParen))
            .map(|inner| match inner {
                None => Expr::Literal(Literal::Unit),
                Some(((first, rest), trailing)) => {
                    if rest.is_empty() && trailing.is_none() {
                        first
                    } else {
                        let mut fields = Vec::with_capacity(rest.len() + 1);
                        fields.push(first);
                        fields.extend(rest);
                        Expr::Tuple(Tuple { fields })
                    }
                }
            });

        let list = comma_exprs
            .clone()
            .delimited_by(just(Token::LBracket), just(Token::RBracket))
            .map(|elems| Expr::List(List { elems }));

        let if_else = just(Token::If)
            .ignore_then(expr.clone())
            .then(
                expr.clone()
                    .delimited_by(just(Token::LBrace), just(Token::RBrace)),
            )
            .then_ignore(just(Token::Else))
            .then(
                expr.clone()
                    .delimited_by(just(Token::LBrace), just(Token::RBrace)),
            )
            .map(|((cond, if_expr), else_expr)| {
                Expr::IfElse(Box::new(IfElse {
                    cond,
                    if_expr,
                    else_expr,
                }))
            });

        let arm = pattern()
            .then_ignore(just(Token::FatArrow))
            .then(expr.clone())
            .map(|(pattern, body)| MatchArm { pattern, body });
        let match_expr = just(Token::Match)
            .ignore_then(expr.clone())
            .then(
                arm.separated_by(just(Token::Comma))
                    .allow_trailing()
                    .collect::<Vec<_>>()
                    .delimited_by(just(Token::LBrace), just(Token::RBrace)),
            )
            .map(|(scrut, arms)| Expr::Match(Box::new(Match { scrut, arms })));

        let atom = choice((
            lit,
            if_else,
            match_expr,
            ctor,
            block.clone().map(|b| Expr::Block(Box::new(b))),
            paren,
            list,
            path,
        ));

        // --- POSTFIX: call / project / index ---
        let postfix = atom.foldl(
            choice((
                comma_exprs
                    .clone()
                    .delimited_by(just(Token::LParen), just(Token::RParen))
                    .map(Postfix::Call),
                just(Token::Dot)
                    .ignore_then(select! { Token::Identifier(s) => s })
                    .map(Postfix::Project),
                expr.clone()
                    .delimited_by(just(Token::LBracket), just(Token::RBracket))
                    .map(Postfix::Index),
            ))
            .repeated(),
            |lhs, post| match post {
                Postfix::Call(args) => Expr::Call(Box::new(lhs), args),
                Postfix::Project(field) => Expr::Project(Box::new(lhs), field),
                Postfix::Index(idx) => Expr::Index(Box::new(lhs), Box::new(idx)),
            },
        );

        // --- PREFIX UNARY + INFIX (pratt precedence) ---
        let infix_op = |prec: u16, tok: Token<'src>, op: InfixOp| {
            infix(left(prec), just(tok), move |l, _, r, _| Expr::Infix {
                lhs: Box::new(l),
                op,
                rhs: Box::new(r),
            })
        };
        postfix.pratt((
            prefix(10, just(Token::Minus), |_, e, _| Expr::Unary {
                op: UnaryOp::Neg,
                expr: Box::new(e),
            }),
            prefix(10, just(Token::Bang), |_, e, _| Expr::Unary {
                op: UnaryOp::Not,
                expr: Box::new(e),
            }),
            infix_op(8, Token::Caret, InfixOp::Xor),
            infix_op(7, Token::Star, InfixOp::Mul),
            infix_op(7, Token::Slash, InfixOp::Div),
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
        ))
    })
}

enum CtorBody<'src> {
    Struct(Vec<(&'src str, Expr<'src>)>),
    Tuple(Vec<Expr<'src>>),
}

/// Declarations, parameterised over the expression parser so the same grammar
/// is shared by blocks, modules, and the REPL. Self-recursive for nested
/// `fn` bodies and `mod` blocks.
pub fn declaration<'tokens, 'src: 'tokens, I, E>(
    expr: E,
) -> impl Parser<'tokens, I, Declaration<'src>, ParserError<'tokens, 'src>> + Clone
where
    I: ValueInput<'tokens, Token = Token<'src>, Span = SimpleSpan>,
    E: Parser<'tokens, I, Expr<'src>, ParserError<'tokens, 'src>> + Clone + 'tokens,
{
    recursive(|declaration| {
        let modifier = just(Token::Pub).to(Modifier::Pub).or_not();
        let ident = select! { Token::Identifier(s) => s };
        let type_ident = select! { Token::TypeIdentifier(s) => s };

        let block = declaration
            .clone()
            .repeated()
            .collect::<Vec<_>>()
            .then(expr.clone().or_not())
            .delimited_by(just(Token::LBrace), just(Token::RBrace))
            .map(|(decls, value)| ExprBlock { decls, value });

        let let_decl = modifier
            .clone()
            .then_ignore(just(Token::Let))
            .then(pattern())
            .then_ignore(just(Token::Equals))
            .then(expr.clone())
            .then_ignore(just(Token::Semicolon))
            .map(|((modifier, pattern), value)| {
                Declaration::Let(LetDecl {
                    modifier,
                    pattern,
                    value,
                })
            });

        let fn_decl = modifier
            .clone()
            .then_ignore(just(Token::Fn))
            .then(ident)
            .then(
                pattern()
                    .separated_by(just(Token::Comma))
                    .allow_trailing()
                    .collect::<Vec<_>>()
                    .delimited_by(just(Token::LParen), just(Token::RParen)),
            )
            .then(block.clone())
            .map(|(((modifier, name), args), body)| {
                Declaration::Fn(FnDecl {
                    modifier,
                    name,
                    args,
                    body,
                })
            });

        // enum Variants
        let variant = type_ident
            .then(
                choice((
                    ident
                        .then_ignore(just(Token::Colon))
                        .then(type_())
                        .separated_by(just(Token::Comma))
                        .allow_trailing()
                        .collect::<Vec<_>>()
                        .delimited_by(just(Token::LBrace), just(Token::RBrace))
                        .map(VariantBody::Struct),
                    type_()
                        .separated_by(just(Token::Comma))
                        .allow_trailing()
                        .collect::<Vec<_>>()
                        .delimited_by(just(Token::LParen), just(Token::RParen))
                        .map(VariantBody::Tuple),
                ))
                .or_not(),
            )
            .map(|(name, body)| match body {
                None => EnumVariant::Empty(name),
                Some(VariantBody::Struct(fields)) => EnumVariant::Struct(name, fields),
                Some(VariantBody::Tuple(tys)) => EnumVariant::Tuple(name, tys),
            });
        let enum_decl = modifier
            .clone()
            .then_ignore(just(Token::Enum))
            .then(type_ident)
            .then(
                variant
                    .separated_by(just(Token::Comma))
                    .allow_trailing()
                    .collect::<Vec<_>>()
                    .delimited_by(just(Token::LBrace), just(Token::RBrace)),
            )
            .map(|((modifier, name), variants)| {
                Declaration::Enum(EnumDecl {
                    modifier,
                    name,
                    variants,
                })
            });

        let struct_decl = modifier
            .clone()
            .then_ignore(just(Token::Struct))
            .then(type_ident)
            .then(
                ident
                    .then_ignore(just(Token::Colon))
                    .then(type_())
                    .separated_by(just(Token::Comma))
                    .allow_trailing()
                    .collect::<Vec<_>>()
                    .delimited_by(just(Token::LBrace), just(Token::RBrace)),
            )
            .map(|((modifier, name), entries)| {
                Declaration::Struct(StructDecl {
                    modifier,
                    name,
                    entries,
                })
            });

        // Minimal placeholders: the AST only records a name (empty `{}` body).
        let trait_decl = modifier
            .clone()
            .then_ignore(just(Token::Trait))
            .then(type_ident)
            .then_ignore(just(Token::LBrace))
            .then_ignore(just(Token::RBrace))
            .map(|(modifier, name)| Declaration::Trait(TraitDecl { modifier, name }));
        let impl_decl = modifier
            .clone()
            .then_ignore(just(Token::Impl))
            .then(type_ident)
            .then_ignore(just(Token::LBrace))
            .then_ignore(just(Token::RBrace))
            .map(|(modifier, name)| Declaration::Impl(ImplDecl { modifier, name }));

        let alias_decl = modifier
            .clone()
            .then_ignore(just(Token::Type))
            .then(type_())
            .then_ignore(just(Token::Equals))
            .then(type_())
            .then_ignore(just(Token::Semicolon))
            .map(|((modifier, lhs), rhs)| Declaration::Alias(AliasDecl { modifier, lhs, rhs }));

        let mod_decl = modifier
            .clone()
            .then_ignore(just(Token::Mod))
            .then(ident)
            .then(
                declaration
                    .clone()
                    .repeated()
                    .collect::<Vec<_>>()
                    .delimited_by(just(Token::LBrace), just(Token::RBrace))
                    .map(|decls| Module { decls }),
            )
            .map(|((modifier, name), value)| {
                Declaration::Mod(ModDecl {
                    modifier,
                    name,
                    value,
                })
            });

        choice((
            let_decl,
            fn_decl,
            enum_decl,
            struct_decl,
            trait_decl,
            impl_decl,
            alias_decl,
            mod_decl,
        ))
    })
}

enum VariantBody<'src> {
    Struct(Vec<(&'src str, Type<'src>)>),
    Tuple(Vec<Type<'src>>),
}

/// A whole module: a sequence of declarations.
pub fn module<'tokens, 'src: 'tokens, I>()
-> impl Parser<'tokens, I, Module<'src>, ParserError<'tokens, 'src>> + Clone
where
    I: ValueInput<'tokens, Token = Token<'src>, Span = SimpleSpan>,
{
    declaration(expr())
        .repeated()
        .collect::<Vec<_>>()
        .map(|decls| Module { decls })
}

/// A single REPL entry: a declaration or a bare expression.
pub fn repl_input<'tokens, 'src: 'tokens, I>()
-> impl Parser<'tokens, I, ReplInput<'src>, ParserError<'tokens, 'src>> + Clone
where
    I: ValueInput<'tokens, Token = Token<'src>, Span = SimpleSpan>,
{
    choice((
        declaration(expr()).map(ReplInput::Declaration),
        expr().map(ReplInput::Expr),
    ))
}

fn join_errs<'a>(errs: impl IntoIterator<Item = Rich<'a, Token<'a>>>) -> String {
    errs.into_iter()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parse a single source expression.
pub fn parse_expr<'src>(input: &'src str) -> Result<Expr<'src>, String> {
    let stream = Lexer::new(input).into_stream();
    expr()
        .then_ignore(end())
        .parse(stream)
        .into_result()
        .map_err(join_errs)
}

/// Parse a whole module.
pub fn parse_module<'src>(input: &'src str) -> Result<Module<'src>, String> {
    let stream = Lexer::new(input).into_stream();
    module()
        .then_ignore(end())
        .parse(stream)
        .into_result()
        .map_err(join_errs)
}

/// Parse a single REPL entry (declaration or expression).
pub fn parse_repl<'src>(input: &'src str) -> Result<ReplInput<'src>, String> {
    let stream = Lexer::new(input).into_stream();
    repl_input()
        .then_ignore(end())
        .parse(stream)
        .into_result()
        .map_err(join_errs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_literals() {
        assert!(matches!(
            parse_expr("123"),
            Ok(Expr::Literal(Literal::Integer(123)))
        ));
        assert!(matches!(parse_expr("()"), Ok(Expr::Literal(Literal::Unit))));
        assert!(parse_expr("\"foo\"").is_ok());
        assert!(parse_expr("true").is_ok());
    }

    #[test]
    fn infix_precedence() {
        // 1 + 2 * 3 -> Add(1, Mul(2, 3))
        let e = parse_expr("1 + 2 * 3").unwrap();
        match e {
            Expr::Infix { op: InfixOp::Add, rhs, .. } => {
                assert!(matches!(*rhs, Expr::Infix { op: InfixOp::Mul, .. }));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn postfix_chain() {
        // f(a, b).c[0]
        let e = parse_expr("f(a, b).c[0]").unwrap();
        assert!(matches!(e, Expr::Index(..)));
    }

    #[test]
    fn comments_and_whitespace_skipped() {
        assert!(parse_expr("1 /* block */ + // line\n 2").is_ok());
    }

    #[test]
    fn declarations() {
        assert!(matches!(
            parse_repl("let x = 1;"),
            Ok(ReplInput::Declaration(Declaration::Let(_)))
        ));
        assert!(matches!(
            parse_repl("fn add(a, b) { a + b }"),
            Ok(ReplInput::Declaration(Declaration::Fn(_)))
        ));
        assert!(matches!(
            parse_module("enum Color { Red, Green, Blue }").map(|m| m.decls.len()),
            Ok(1)
        ));
        assert!(parse_module("struct Point { x: Int, y: Int }").is_ok());
    }

    #[test]
    fn match_arms() {
        let r = parse_expr("match x { Foo(a) => a, _ => 0 }");
        assert!(matches!(r, Ok(Expr::Match(_))), "got {r:?}");
    }
}
