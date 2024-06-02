pub use crate::il_grammar as grammar;
pub use crate::Constant;

pub mod transpile;
pub mod lexer;

pub type Symbol = String;

#[derive(Debug,Clone)]
pub enum Bind {
    Rec(Vec<(Var, Expr)>),
    NonRec(Var, Box<Expr>)
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
pub enum VarType {
    Id(Symbol),
    Inf(Symbol), // A type variable for inference
    Hole, // A placeholder
    Kind, // *
    App(Box<VarType>, Box<VarType>),
    Arrow, // -> operator
    // Specifies a data *constructor* function
    // i.e struct["x", "y"] i8 i8,
    // enum["Foo", "Bar", "Baz"] () () ()
    Data(Vec<Symbol>),
    Enum(Vec<Symbol>),
}

#[derive(Debug,Clone)]
pub struct Var(pub Symbol, pub VarType);

#[derive(Debug,Clone)]
pub enum Expr {
    Id(Symbol),
    Const(Constant),

    App(Box<Expr>, Box<Expr>),
    Lam(Vec<Var>, Box<Expr>),

    // callable/call are inverses
    // of each other
    Callable(Box<Expr>),
    Call(Box<Expr>),

    Let(Bind, Box<Expr>),
    Cast(Box<Expr>, Cast),
    Coerce(Box<Expr>, Coercion),
    // For source annotation
    Tick(Tick, Box<Expr>),
    VarType(VarType)
}