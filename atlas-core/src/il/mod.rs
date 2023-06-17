pub mod transpile;

type Symbol = String;

#[derive(Debug,Clone)]
pub enum Constant {
    Integer(i64),
    Float(f64),
    Bool(bool),
    String(String),
    Unit
}

type Idx = u32;

#[derive(Debug,Clone)]
pub enum Bind {
    Rec(Vec<(Symbol, Expr)>),
    NonRec(Symbol, Box<Expr>)
}

#[derive(Debug,Clone)]
pub enum Tick {
}
#[derive(Debug,Clone)]
pub enum Cast {
}
#[derive(Debug,Clone)]
pub enum Coercion {
}

#[derive(Debug,Clone)]
pub enum Expr {
    Var(Symbol),
    Const(Constant),
    App(Box<Expr>, Box<Expr>),
    Call(Box<Expr>),
    Lam(Symbol, Box<Expr>),
    Let(Bind, Box<Expr>),
    Cast(Box<Expr>, Cast),
    Coerce(Box<Expr>, Coercion),
    // For source annotation
    Tick(Tick, Box<Expr>),
}