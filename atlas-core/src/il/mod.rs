pub mod transpile;

type Symbol = String;

#[derive(Debug,Clone)]
pub enum Literal {
    Integer(i64),
    Float(f64),
    Bool(bool),
    String(String),
    Unit
}

#[derive(Debug,Clone)]
pub struct Let {
    pub sym: Symbol,
    pub val: Expr,
    pub body: Expr
}

#[derive(Debug,Clone)]
pub struct Bind {
    pub lam: Expr,
    pub args: Vec<Expr>
}

#[derive(Debug,Clone)]
pub struct Invoke {
    pub lam: Expr,
    pub args: Vec<Expr>
}

#[derive(Debug,Clone)]
pub enum Expr {
    Literal(Literal),
    Var(Symbol),
    Let(Box<Let>),
    Bind(Box<Bind>),
    Invoke(Box<Invoke>)
}