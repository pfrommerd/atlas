use ordered_float::NotNan;

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
pub enum Content {
    Ident(String),
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
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Node {
    content: Content
}
