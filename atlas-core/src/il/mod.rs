pub mod transpile;
pub mod lexer;

pub type Symbol = String;

#[derive(Debug,Clone)]
pub enum Constant {
    Integer(i64),
    Float(f64),
    Bool(bool),
    String(String),
    Unit
}

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
    Arrow(Box<VarType>, Box<VarType>), // ->
    App(Box<VarType>, Box<VarType>),

    // Specifies a data *constructor*
    // i.e struct["x", "y"] i8 i8,
    // enum["Foo", "Bar", "Baz"] () () ()
    Data(Symbol, Vec<Constant>),
}

#[derive(Debug,Clone)]
pub struct Var(Symbol, VarType);

#[derive(Debug,Clone)]
pub enum Expr {
    Id(Symbol),
    Const(Constant),

    Term, // The terminator of an argument
          // list
    App(Box<Expr>, Box<Expr>),
    Lam(Var, Box<Expr>),
    Let(Bind, Box<Expr>),
    Cast(Box<Expr>, Cast),
    Coerce(Box<Expr>, Coercion),
    // For source annotation
    Tick(Tick, Box<Expr>),
    VarType(VarType)
}