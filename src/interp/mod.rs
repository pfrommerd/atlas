use gc::{Gc, GcCell};

use std::collections::HashMap;

use crate::core::lang::{
    Expr, Symbol,
    PrimitiveType, PrimitiveOp
};

// No string primitive...
// strings literals are converted into
// lists of chars
#[derive(Trace, Finalize)]
pub enum Primitive {
    Bool(bool), Int(i64),
    Float(f64), Char(char)
}


#[derive(Trace, Finalize)]
pub enum Node {
    // types
    Star,
    Arrow(NodeHandle, NodeHandle),
    PrimType(PrimitiveType),
    DataType(DataInfoHandle), // type desugaring information

    // values
    Prim(Primitive),
    PrimOp(PrimitiveOp),
    Constr(u16, DataInfoHandle),
    Data(DataHandle), // fully-constructed data object
    App(NodeHandle, NodeHandle),
    Bad
}

type NodeHandle = Gc<GcCell<Node>>;

pub struct Data {
    pub values: Vec<NodeHandle>,
    pub info: DataInfoHandle
}

type DataHandle = Gc<GcCell<Node>>;

// do not trace the packinfo type
unsafe impl gc::Trace for Data {
    custom_trace! (this,{
        mark(&this.values)
    });
}
impl gc::Finalize for Data {}

#[derive(Trace, Finalize)]
pub enum DataInfo {
    Tuple(Vec<NodeHandle>),
    Record(Vec<(String, NodeHandle)>),
    Variant(Vec<(String, Vec<NodeHandle>)>),
    Module(Vec<(String, NodeHandle)>)
}

type DataInfoHandle = Gc<GcCell<DataInfo>>;

// A code-node
impl Node {
    // create a Node from a core expression
    // in a given symbol-lookup environment
    pub fn create(exp: &Expr, _env: &Env) -> Self {
        use Node::*;
        match exp {
        _ => Bad
        }
    }
}

pub struct Env {
    symbols: HashMap<Symbol, NodeHandle>
}

impl Env {
    pub fn set(&mut self, s: &Symbol, n: &NodeHandle) {
        self.symbols.insert(s.clone(), n.clone());
    }
    pub fn get(&self, s: &Symbol) -> Option<NodeHandle> {
        self.symbols.get(s).map(|x| (*x).clone())
    }
}

// graph reduction machine
pub struct Machine {

}