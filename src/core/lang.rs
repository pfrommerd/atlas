use bytes::Bytes;

pub struct Symbol {
    pub name: String
}

pub type Var = Symbol;

pub enum Primitive {
    Int(i64),
    Float(f64),
    Bool(bool),
    Char(char),
    String(String),
    Buffer(Bytes),
    EmptyList,
    EmptyTuple,
    EmptyRecord
}

pub enum Bind {
    Rec(Vec<(Symbol, Expr)>),
    NonRec(Symbol, BExpr)
}

pub struct LetIn {
    pub bind: Bind,
    pub body: BExpr
}

pub struct Lambda {
    pub args: Vec<Symbol>,
    pub body: BExpr
}

pub struct App {
    pub lam: BExpr,
    pub args: Vec<Expr>
}

pub struct Builtin {
    pub op: String,
    pub args: Vec<Expr>
}

pub enum Case {
    Eq(Expr, Expr),
    Tag(String, Expr),
    Default(Expr)
}

pub struct Invoke {
    pub target: BExpr
}

pub struct Match {
    pub scrut: BExpr,
    pub bind: Option<Symbol>,
    pub cases: Vec<Case>
}

pub enum Expr {
    Var(Var),
    Primitive(Primitive),
    LetIn(LetIn),
    Lambda(Lambda),
    App(App),
    Invoke(Invoke),
    Match(Match),
    Builtin(Builtin)
}

type BExpr = Box<Expr>;