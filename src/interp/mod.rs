use gc::Gc;

use crate::core::lang::{
    Expr
};

#[derive(Trace, Finalize)]
pub enum Primitive {
    Bool(bool), Int(i64),
    Float(f64), String(String), Char(char)
}

#[derive(Trace, Finalize)]
pub enum Node {
    Primitive(Primitive),
    App(Gc<Node>, Gc<Node>),
    Bad
}

impl Node {
    // create a Node from a core expression
    pub fn create(exp: &Expr) -> Self {
        use Node::*;
        match exp {
        _ => Bad
        }
    }
}

pub struct Env {

}

// graph reduction machine
pub struct Machine {

}