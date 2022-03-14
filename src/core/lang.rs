use bytes::Bytes;

#[derive(Debug)]

pub struct Symbol {
    pub name: String
}

pub type Var = Symbol;

// Primitive is like literal, but literal includes
// things like empty lists, tuples, records which are
// data structures and not primitives
#[derive(Debug)]
#[derive(Clone)]
pub enum Primitive {
    Unit, Int(i64), Float(f64),
    Bool(bool), Char(char),
    String(String), Buffer(Bytes)
}

#[derive(Debug)]
#[derive(Clone)]
pub enum Literal {
    Unit,
    Int(i64),
    Float(f64),
    Bool(bool),
    Char(char),
    String(String),
    Buffer(Bytes),
}

#[derive(Debug)]

pub enum Bind {
    Rec(Vec<(Symbol, Expr)>),
    NonRec(Symbol, BExpr)
}

#[derive(Debug)]

pub struct LetIn {
    pub bind: Bind,
    pub body: BExpr
}

#[derive(Debug)]

pub struct Lambda {
    pub args: Vec<Symbol>,
    pub body: BExpr
}

#[derive(Debug)]

pub struct App {
    pub lam: BExpr,
    pub args: Vec<Expr>
}

#[derive(Debug)]

pub struct Builtin {
    pub op: String,
    pub args: Vec<Expr>
}

#[derive(Debug)]
pub enum Case {
    Eq(Primitive, Expr),
    Tag(String, Expr),
    Default(Expr)
}

#[derive(Debug)]

pub struct Invoke {
    pub target: BExpr
}

#[derive(Debug)]

pub struct Match {
    pub scrut: BExpr,
    pub bind: Option<Symbol>,
    pub cases: Vec<Case>
}

#[derive(Debug)]

pub enum Expr {
    Var(Var),
    Literal(Literal),
    LetIn(LetIn),
    Lambda(Lambda),
    App(App),
    Invoke(Invoke),
    Match(Match),
    Builtin(Builtin)
}

type BExpr = Box<Expr>;