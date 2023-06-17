use ordered_float::NotNan;
use super::decl::Declaration;

#[derive(Debug, Clone)]
pub enum Literal<'src> {
    Integer(i64),
    Float(NotNan<f64>),
    Bool(bool),
    String(&'src str),
    Unit
}

#[derive(Debug, Clone)]
pub struct IfElse<'src> {
    pub cond: Expr<'src>,
    pub if_expr: Expr<'src>,
    pub else_expr: Expr<'src>
}

#[derive(Debug, Clone)]
pub struct Match<'src> {
    pub scrut: Expr<'src>
}

#[derive(Debug, Clone)]
pub struct Tuple<'src> {
    pub fields: Vec<Expr<'src>>
}

#[derive(Debug, Clone)]
pub struct List<'src> {
    pub elems: Vec<Expr<'src>>
}

#[derive(Debug, Clone)]
pub struct Infix<'src> {
    pub lhs: Expr<'src>,
    pub rhs: Vec<(&'src str, Expr<'src>)>
}

#[derive(Debug, Clone)]
pub struct ExprBlock<'src> {
    // An expr block can also
    // have modifiers, but only rec
    pub decls: Vec<Declaration<'src>>,
    pub value: Option<Expr<'src>>
}

#[derive(Debug, Clone)]
pub enum Constructor<'src> {
    Struct(&'src str, Vec<(&'src str, Expr<'src>)>),
    Tuple(&'src str, Vec<Expr<'src>>),
    Empty(&'src str)
}

#[derive(Debug, Clone)]
pub enum Expr<'src> {
    Literal(Literal<'src>),
    Identifier(&'src str),
    Constructor(Constructor<'src>),
    Tuple(Tuple<'src>),
    List(List<'src>),
    IfElse(Box<IfElse<'src>>),
    Match(Box<Match<'src>>),
    Block(Box<ExprBlock<'src>>),
    Unary(&'src str, Box<Expr<'src>>),
    Infix(Box<Infix<'src>>),
    Project(Box<Expr<'src>>, &'src str),
    // Something like foo::bar
    Scope(Vec<&'src str>),
    Index(Box<Expr<'src>>, Box<Expr<'src>>),
    Call(Box<Expr<'src>>, Vec<Expr<'src>>)
}