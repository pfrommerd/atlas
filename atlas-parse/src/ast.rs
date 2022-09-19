use super::lexer::Token;
use ordered_float::NotNan;

#[derive(Debug, Clone)]
pub enum Literal<'src> {
    Integer(i64),
    Float(NotNan<f64>),
    Bool(bool),
    String(&'src str),
    Unit
}

#[derive(Debug, Clone)]
pub enum Pattern<'src> {
    Identifier(&'src str)
}

#[derive(Debug, Clone)]
pub enum Modifier {
    Pub, Rec
}

#[derive(Debug, Clone)]
pub struct LetBinding<'src> {
    pub pattern: Pattern<'src>,
    pub value: Expr<'src>
}

#[derive(Debug, Clone)]
pub struct IfElse<'src> {
    pub cond: Expr<'src>,
    pub if_expr: Expr<'src>,
    pub else_expr: Expr<'src>
}

#[derive(Debug, Clone)]
pub struct FnDeclaration<'src> {
    pub name: &'src str,
    pub args: Vec<Pattern<'src>>,
    pub body: ExprBlock<'src>
}

#[derive(Debug, Clone)]
pub struct ExprBlock<'src> {
    // An expr block can also
    // have modifiers, but only rec
    pub mods: Vec<Modifier>,
    pub decls: Vec<Declaration<'src>>,
    pub value: Option<Expr<'src>>
}

#[derive(Debug, Clone)]
pub struct DeclBlock<'src> {
    pub mods: Vec<Modifier>,
    // A decl block only has declarations
    pub decls: Vec<Declaration<'src>>,
}

pub struct Record<'src> {
    pub fields: Vec<(Expr<'src>, Expr<'src>)>,
}

pub struct Tuple<'src> {
    pub fields: Vec<Expr<'src>>
}

#[derive(Debug, Clone)]
pub enum Expr<'src> {
    Literal(Literal<'src>),
    Identifier(&'src str),
    Call(Box<Expr<'src>>, Vec<Expr<'src>>),
    IfElse(Box<IfElse<'src>>),
    Block(Box<ExprBlock<'src>>),
    Unary(&'src str, Vec<&'src str>),
    Infix(Box<Expr<'src>>, Vec<(&'src str, Expr<'src>)>)
}

#[derive(Debug, Clone)]
pub enum Declaration<'src> {
    Let(LetBinding<'src>),
    Fn(FnDeclaration<'src>)
}

#[derive(Debug, Clone)]
pub enum ReplInput<'src> {
    // An expr may still be a command if it is
    // a function marked with #[command]
    Expr(Expr<'src>),
    // Invoke a command expr with specified arguments
    CommandInvoke(Expr<'src>, Vec<Token<'src>>),
    Declaration(Declaration<'src>)
}