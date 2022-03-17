use bytes::Bytes;

#[derive(Debug)]

pub struct Symbol {
    pub name: String
}

pub type Var = Symbol;

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
    Eq(Literal, Expr),
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

use crate::store::{Storage, value::Value, Storable};
use crate::Error;

impl<'s, S: Storage> Storable<'s, S> for Literal {
    fn store_in(&self, store: &'s S) -> Result<S::Handle<'s>, Error> {
        use Literal::*;
        let val = match self {
            Unit => Value::Unit,
            Int(i) => Value::Int(*i),
            Float(f) => Value::Float(*f),
            Bool(b) => Value::Bool(*b),
            Char(c) => Value::Char(*c),
            String(s) => Value::String(s.clone()),
            Buffer(b) => Value::Buffer(b.clone())
        };
        store.insert_from(&val)
    }
}
