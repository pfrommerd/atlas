use bytes::Bytes;

pub struct Symbol(pub String);

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

pub struct App {
    pub lam: BExpr,
    pub arg: BExpr
}

pub struct Builtin {
    pub op: String,
    pub args: Vec<Expr>
}

pub enum Case {
    Primitive(Primitive)
}

pub struct Match {
    pub scrut: BExpr,
    pub bind: Option<Symbol>,
    pub cases: Vec<Case>
}

pub enum Expr {
    Primitive(Primitive),
    LetIn(LetIn),
    App(App),
    Invoke(BExpr),
    Match(Match),
    Builtin(Builtin)
}

type BExpr = Box<Expr>;