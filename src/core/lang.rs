pub struct Symbol {
    s: String
}

pub enum Primitive {
    Int(i64),
    Float(f64),
    Bool(bool),
    Char(char),
    String(String)
}

pub enum Bind {
    Rec(Vec<(Symbol, Expr)>),
    NonRec(Symbol, BExpr)
}

pub struct LetIn {
    bind: Bind,
    body: BExpr
}

pub struct App {
    lam: BExpr,
    arg: BExpr
}

pub struct Builtin {
    op: String,
    args: Vec<Expr>
}

pub enum Case {
    Primitive(Primitive)
}

pub struct Match {
    scrut: BExpr,
    bind: Option<Symbol>,
    cases: Vec<Case>
}

pub enum Expr {
    Primitive(Primitive),
    LetIn(LetIn),
    App(App),
    Call(BExpr),
    Match(Match),
    Builtin(Builtin)
}

type BExpr = Box<Expr>;