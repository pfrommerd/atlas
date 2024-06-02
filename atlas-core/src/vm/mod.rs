use std::collections::BTreeMap;

// Based on interaction networks
// See https://github.com/HigherOrderCO/HVM
// Used under Apache 2.0 License
pub mod parser;

pub use crate::Constant;

#[derive(Clone, Hash, PartialEq, Eq, Debug)]
pub enum Tree {
    Constant(Constant),
    // For wiring
    Var(String),
    // Reference to a net
    Ref(String),
    // Builtin operator
    Operator(String, Vec<Tree>),
    Erase,
    Con(Option<Type>, Vec<Tree>),
    Dup(Vec<Tree>),
    // switch equality cases
    Switch(Vec<(Constant, Tree)>),
}

#[derive(Clone, Hash, PartialEq, Eq, Debug)]
pub struct Redex {
    pub lhs: Tree,
    pub rhs: Tree
}

#[derive(Clone, Hash, PartialEq, Eq, Debug)]
pub struct Net {
  pub root: Tree,
  pub redexs: Vec<Redex>,
}

#[derive(Clone, Hash, PartialEq, Eq, Debug)]
pub struct Book {
    pub defs: BTreeMap<String, Net>,
}
