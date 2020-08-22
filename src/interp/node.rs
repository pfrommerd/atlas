use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;

use crate::core::lang::{
    Expr, Symbol
};
pub use crate::core::lang::{
    PrimitiveType, PrimitiveOp
};

#[derive(Debug, Clone)]
pub enum BaseTypeNode {
    Primitive(PrimitiveType)
}

// No string primitive...
// strings literals are converted into
// lists of chars
#[derive(Debug, Copy, Clone)]
pub enum Primitive {
    Void,
    Bool(bool), Int(i64),
    Float(f64), Char(char)
}

#[derive(Clone, Debug)]
pub enum Node<'heap> {
    // types
    Star,
    Arrow(NodePtr<'heap>, NodePtr<'heap>),
    BaseType(BaseTypeNode),

    // reference to a particular arg of a containing combinator
    // substituted on combinator evaluation. Note the combinators
    // can be type combinators too! (as in type 'a foo = 'a list
    // where foo is a type super-combinator!)
    // the handle is a reference back to the associated combinator

    ArgRef(u16, NodePtr<'heap>), 
    Indirection(NodePtr<'heap>),

    // Supercombinators:

    // user-defined supercombinator with num args, body
    Combinator(u16, NodePtr<'heap>),
    PrimOp(PrimitiveOp), // primitive supercombinator
    Pack(u16, u16, NodePtr<'heap>), // tag, arity and resulting type info

    // Basic value types:

    Prim(Primitive),
    Data(Vec<NodePtr<'heap>>, NodePtr<'heap>), // values, type

    // The most holy of them all
    App(NodePtr<'heap>, NodePtr<'heap>),

    Panic(String),
    Bad // An invalid node
}

impl<'heap> Node<'heap> {
    // create a Node from a core expression
    // in a given symbol-lookup environment
    /*pub fn compile(exp: &Expr, _env: &Env) -> NodeHandle {
        use Node::*;
        let result = match exp {
        _ => Bad
        };
        NodeHandle::new(GcCell::new(result))
    }*/

    pub fn direct_arity(&self) -> Option<u16> {
        use Node::*; 
        match self {
            Combinator(num_args, _) => Some(*num_args),
            PrimOp(op) => Some(op.arity()),
            Pack(_, arity, _) => Some(*arity),
            _ => None
        }
    }
}

// A node pointer is a heap and a location
// within that heap
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NodePtr<'heap> {
    loc: usize,
    _phantom : PhantomData<&'heap Heap<'heap>>
}

pub struct Heap<'heap> {
    nodes: Vec<Node<'heap>>
}

impl<'heap> Heap<'heap> {
    pub fn new() -> Self {
        Heap { nodes: Vec::new() }
    }

    pub fn ptr(&self, loc: usize) -> NodePtr<'heap> {
        NodePtr { loc: loc, _phantom: PhantomData }
    }

    pub fn at<'a>(&'a self, ptr: NodePtr<'heap>) -> &'a Node<'heap> {
        &self.nodes[ptr.loc]
    }

    pub fn at_mut<'a>(&'a mut self, ptr: NodePtr<'heap>) -> &'a mut Node<'heap> {
        &mut self.nodes[ptr.loc]
    }

    pub fn set(&mut self, ptr: NodePtr<'heap>, node: Node<'heap>) {
        self.nodes[ptr.loc] = node;
    }

    pub fn add(&mut self, node: Node<'heap>) -> NodePtr<'heap> {
        self.nodes.push(node);
        return self.ptr(self.nodes.len() - 1)
    }

    pub fn copy_to(&mut self, src: NodePtr<'heap>, tgt: NodePtr<'heap>) {
        use Node::*;
        let mut remap : HashMap<NodePtr<'heap>, NodePtr<'heap>> = HashMap::new();
        let mut queue : Vec<NodePtr<'heap>> = Vec::new();

        remap.insert(src, tgt);
        queue.push(src);

        while let Some(node_ptr) = queue.pop() {
            let node = self.at(node_ptr).clone();
            let tgt_ptr = *remap.get(&node_ptr).unwrap();

            let mut req_copy = |ptr:&NodePtr<'heap>| -> NodePtr<'heap> {
                if let Some(&nptr) = remap.get(&ptr) {
                    nptr
                } else {
                    let nptr = self.add(Bad);
                    remap.insert(*ptr, nptr);
                    queue.push(*ptr);
                    *ptr
                }
            };

            let copy = match &node {
                Arrow(left, right) =>
                    Arrow(req_copy(left), req_copy(right)),
                BaseType(bt) => {
                    BaseType(match bt {
                        BaseTypeNode::Primitive(pt) => BaseTypeNode::Primitive(*pt)
                    })
                },
                ArgRef(arg, comb) =>
                    ArgRef(*arg, req_copy(comb)),
                Indirection(other) =>
                    Indirection(req_copy(other)),
                Combinator(nargs, body) =>
                    Combinator(*nargs, req_copy(body)),
                Data(values, dtype) =>
                    Data(values.iter().map(|x| req_copy(x)).collect(),
                        req_copy(dtype)),
                App(left, right) =>
                    App(req_copy(left), req_copy(right)),
                x => x.clone()
            };
            self.set(tgt_ptr, copy);
        }
    }

    pub fn copy(&mut self, src: NodePtr<'heap>) -> NodePtr<'heap> {
        self.nodes.push(Node::Bad);
        let ptr = self.ptr(self.nodes.len() - 1);
        self.copy_to(src, ptr);
        ptr
    }

    pub fn replace_args(&mut self, node_ptr: NodePtr<'heap>, 
                        comb_ptr: NodePtr<'heap>, 
                        args: &Vec<NodePtr<'heap>>) {
        use Node::*;
        let mut queue : Vec<NodePtr<'heap>> = Vec::new();
        let mut processed : HashSet<NodePtr<'heap>> = HashSet::new();


        queue.push(node_ptr);
        processed.insert(node_ptr);
        
        while let Some(ptr) = queue.pop() {
            let node = self.at(ptr);
            let mut req_replace = |ptr: &NodePtr<'heap>| {
                if !processed.contains(ptr) {
                    processed.insert(*ptr);
                    queue.push(*ptr);
                }
            };
            match node {
                &ArgRef(arg, combinator) => {
                    if combinator == comb_ptr {
                        // replace with an indirection to the right arg
                        self.set(node_ptr, Indirection(args[arg as usize]))
                    }
                },
                Arrow(left, right) => {
                    req_replace(left); req_replace(right);
                },
                BaseType(bn) => {
                    match bn {
                        BaseTypeNode::Primitive(_) => ()
                    }
                },
                Indirection(real) => req_replace(real),
                Combinator(_, body) => req_replace(body),
                Data(values, dtype) => {
                    values.iter().for_each(&mut req_replace);
                    req_replace(dtype);
                },
                App(left, right) => {
                    req_replace(left); req_replace(right);
                }
                _ => {}
            }
        }
    }
}


pub struct Env<'heap> {
    symbols: HashMap<Symbol, NodePtr<'heap>>
}

impl<'heap> Env<'heap> {
    pub fn set(&mut self, s: &Symbol, n: &NodePtr<'heap>) {
        self.symbols.insert(s.clone(), n.clone());
    }
    pub fn get(&self, s: &Symbol) -> Option<NodePtr<'heap>> {
        self.symbols.get(s).map(|x| (*x).clone())
    }
}