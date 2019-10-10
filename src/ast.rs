use ordered_float::NotNan;


pub use codespan::{
    ByteIndex,
    ColumnIndex,
    LineIndex
}

#[derive(Copy, Clone, Default, Eq, PartialEq, Debug, 
         Hash, Ord, PartialOrd)]
pub struct Location {
    pub line: LineIndex,
    pub col: ColumnIndex,
    pub abs: ByteIndex
}

impl Location {
    pub fn shift(&mut self, c: isize) {
        if 
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Literal {
    Byte(u8),
    Int(i64),
    Float(NotNan<f64>),
    String(String),
    Char(char)
}

// Nodes are for the ast, expr are for 
// the lambda calculus tree

// All nodes can be 
// compiled into expressions though

// All type checking happens on the ast

// A Node contains a value plus a bunch of
// attributes and potentially type information

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Expr {
    Empty,
    Identifier(String),
    Literal(Literal),
    Infix {
        lhs: Box<Node>, 
        op: String, 
        rhs: Box<Node> 
    },
    App {
        func: Box<Node>,
        args: Vec<Box<Node>>
    },
    Let {
        rec: bool,
        defines: Vec<(String, Node)> // each defines is from an *and* block
    },
    Lambda {
    },
    // a module (aka file) is just a vector
    // of let statements
    Module {
        expr: Vec<Node>
    }

    // the pattern types expose variables
    // to outer scopes
}
