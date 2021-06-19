use crate::core::lang::{
    ArgType, ParamType, Primitive, Cond, Symbol
};
use super::tim::TiMachine;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::fmt;

#[derive(Clone, Copy, Eq, PartialEq, Hash, Debug)]
pub enum PrimitiveOp {
    Negate,
    Add, Sub, Mul, Div, Mod,
    Or, And, Xor
}

impl PrimitiveOp {
    pub fn arity(&self) -> usize {
        use PrimitiveOp::*;
        match self {
            Negate => 1, 
            Add => 2, Sub => 2, Mul => 2, Div => 2, Mod => 2,
            Or => 2, And => 2, Xor => 2
        }
    }

    pub fn eval_unary(&self, arg: &Primitive) -> Option<Primitive> {
        use PrimitiveOp::*;
        match self {
            Negate => match arg {
                Primitive::Bool(b) => Some(Primitive::Bool(!b)),
                Primitive::Int(i) => Some(Primitive::Int(-i)),
                Primitive::Float(f) => Some(Primitive::Float(-f)),
                _ => panic!("Invalid argument to negate function")
            }
            _ => panic!("Unary op not implemented!")
        }
    }

    pub fn eval_binary(&self, left: &Primitive, right: &Primitive) -> Option<Primitive> {
        use PrimitiveOp::*;
        match self {
            Add => match (left, right) {
                (Primitive::String(l), Primitive::String(r)) =>
                    Some(Primitive::String(format!("{}{}", l, r))),
                (Primitive::Int(l), Primitive::Int(r)) =>
                    Some(Primitive::Int(l + r)),
                (Primitive::Float(l), Primitive::Float(r)) =>
                    Some(Primitive::Float(l + r))
            },
            Sub => match (left, right) {
                (Primitive::Int(l), Primitive::Int(r)) =>
                    Some(Primitive::Int(l - r)),
                (Primitive::Float(l), Primitive::Float(r)) =>
                    Some(Primitive::Float(l - r))
            },
            Mul =>  match (left, right) {
                (Primitive::Int(l), Primitive::Int(r)) =>
                    Some(Primitive::Int(l * r)),
                (Primitive::Float(l), Primitive::Float(r)) =>
                    Some(Primitive::Float(l * r))
            },
            Div =>  match (left, right) {
                (Primitive::Int(l), Primitive::Int(r)) =>
                    Some(Primitive::Int(l / r)),
                (Primitive::Float(l), Primitive::Float(r)) =>
                    Some(Primitive::Float(l / r))
            },
            _ => panic!("Binary op not implemented!")
        }
    }
} 

// A foreign func also includes a name for debugging purposes
#[derive(Clone)]
pub struct ForeignFunc<'heap>(pub &'static str, pub usize, pub fn(&mut TiMachine<'_, 'heap>, Vec<NodePtr<'heap>>) -> Option<Node<'heap>>);

impl fmt::Debug for ForeignFunc<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "<{}>", self.0)
    }
}

impl fmt::Display for ForeignFunc<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "<{}>", self.0)
    }
}


#[derive(Clone, Debug)]
pub enum Node<'heap> {
    // the following 2 are only found in function
    // bodies and should be replaced upon instantiation
    Arg(usize),
    // Free(NodePtr<'heap>),
    Ind(NodePtr<'heap>), // indirection

    // a partially-bound function
    Partial(NodePtr<'heap>, Vec<(ArgType, NodePtr<'heap>)>),

    // arity, body
    Combinator(NodePtr<'heap>, Vec<ParamType>),
    // builtin combinator types
    Foreign(ForeignFunc<'heap>), // arity, foreignfunc
    PrimOp(PrimitiveOp), // to speed up builtins

    ConsVariant(u16, Vec<String>),
    ConsRecord(Vec<String>),
    ConsTuple(usize),

    Case(Vec<(Cond, NodePtr<'heap>)>),

    Idx(usize), // index into tuple
    Project(String), // index into record

    // Data types:
    Prim(Primitive),
    Variant(u16, Option<NodePtr<'heap>>, Vec<String>), 
    Record(HashMap<String, NodePtr<'heap>>),
    Tuple(Vec<NodePtr<'heap>>),

    // applying an argument does not explicitly call
    // you must wrap it in a call node to then make the call
    // this way we can have a function with 0 arguments
    // and arbitrary arity
    App(NodePtr<'heap>, Vec<(ArgType, NodePtr<'heap>)>),
    Call(NodePtr<'heap>, Vec<(ArgType, NodePtr<'heap>)>),

    // If we panicked, will get propagated up the call hierarchy
    Panic,
    Bad
}

impl<'heap> Node<'heap> {
    pub fn is_whnf(&self, heap: &Heap<'heap>) -> bool {
        use Node::*;
        match self {
            Call(_, _) => false,
            Ind(p) => {
                let mut n = *p;
                while let Ind(x) = heap.at(n) {
                    n = *x;
                }
                heap.at(n).is_whnf(heap)
            },
            _ => true
        }
    }
}

impl fmt::Display for Node<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use Node::*;
        match self {
            Arg(num) => write!(f, "#{}", num),
            Ind(p) => write!(f, "&{}", p),
            Partial(lam, args) => write!(f, "Partial({}, {:?})", lam, args),
            Combinator(body, args) => {
                write!(f, "Comb({}, {:?})", body, args)
            },
            Foreign(func) => {
                write!(f, "{}", func)
            },
            PrimOp(op) => write!(f, "{:?}", op),
            ConsVariant(tag, opts) => write!(f, "ConsVar({}, {:?})", tag, opts),
            ConsRecord(fields) => write!(f, "ConsRecord({:?})", fields),
            ConsTuple(a) => write!(f, "ConsTuple({})", a),
            Prim(prim) => {
                use Primitive::*;
                match prim {
                    Unit => write!(f, "()"),
                    Bool(b) => write!(f, "{}", b),
                    Int(i) => write!(f, "{}", i),
                    Float(fl) => write!(f, "{}", fl),
                    Char(c) => write!(f, "{}", c),
                    String(s) => write!(f, "{}", s),
                    Buffer(c) => write!(f, "{}", c.len())
                }
            },
            Case(_) => write!(f, "Case"),
            Idx(i) => write!(f, "Idx({})", i),
            Project(s) => write!(f, "Project({})", s),
            Prim(p) => write!(f, "{}", p),
            Variant(a, d, fmt) => match d {
                Some(s) => write!(f, "{}({}) {:?}", fmt[*a as usize], s, fmt),
                None => write!(f, "{} {:?}", fmt[*a as usize], fmt),
            },
            Record(map) => {
                write!(f, "{{")?;
                let mut first = true;
                for (k, v) in map {
                    if !first { write!(f, ", ")?; }
                    else { first = false; }
                    write!(f, "{}: {}", k, v)?;
                }
                write!(f, "}}")
            },
            Tuple(vals) => {
                write!(f, "(")?;
                let mut first = true;
                for v in vals {
                    if !first { write!(f, ", ")?; }
                    else { first = false; }
                    write!(f, "{}", v)?;
                }
                write!(f, ")")
            },
            App(left, args) => write!(f, "App({}, {:?})", left, args),
            Call(left, args) => write!(f, "Call({}, {:?})", left, args),
            Panic => write!(f, "Panic"),
            Bad => write!(f, "Bad")
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

impl fmt::Display for NodePtr<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "0x{:x}", self.loc)
    }
}

// A NodeRef specifies both a NodePtr and an immutable heap
// This is useful for pretty-printing

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

    pub fn instantiate_at(&mut self, body: NodePtr<'heap>, tgt: NodePtr<'heap>,
                                     args: &Vec<NodePtr<'heap>>) {
        use Node::*;
        let mut remap : HashMap<NodePtr<'heap>, NodePtr<'heap>> = HashMap::new();
        let mut queue : Vec<NodePtr<'heap>> = Vec::new();

        let mut b = body;
        while let Ind(p) = self.at(b) {
            b = *p;
        }
        remap.insert(b, tgt);
        queue.push(b);

        while let Some(node_ptr) = queue.pop() {
            let node = self.at(node_ptr).clone();
            let tgt_ptr = *remap.get(&node_ptr).unwrap();
            let mut req_copy = |ptr: NodePtr<'heap>| -> NodePtr<'heap> {
                // handle the cases where no copy is required
                let mut tptr = ptr; // "true" pointer
                while let Ind(p) = self.at(tptr) {
                    tptr = *p;
                }
                // check if it is something simple we can handle
                match self.at(tptr) {
                    Arg(idx) => return args[*idx],
                    App(_, _) => (),
                    Case (_) => (),
                    // everything else just return the original
                    // pointer
                    _ => return tptr
                }
                if let Some(nptr) = remap.get(&tptr) {
                    *nptr
                } else {
                    let nptr = self.add(Bad);
                    remap.insert(tptr, nptr);
                    queue.push(tptr);
                    nptr
                }
            };

            let copy = match &node {
                Arg(idx) => Ind(args[*idx]), // usually not necessary except at root
                App(l, r) => App(req_copy(*l), r.iter().map(
                    |(c, n)| (c.clone(), req_copy(*n))).collect()),
                Call(l, r) => App(req_copy(*l), r.iter().map(
                    |(c, n)| (c.clone(), req_copy(*n))).collect()),
                Case(alts) => Case(alts.iter().map(|(c, n)| {
                    (c.clone(), req_copy(*n))
                }).collect()),
                Ind(_) => panic!("Should not get an indirection!"),
                _ => Ind(node_ptr) // everything else can be made an indirection
            };
            self.set(tgt_ptr, copy);
        }
    }
}

impl fmt::Display for Heap<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        for (i, n) in self.nodes.iter().enumerate() {
            let ptr = self.ptr(i);
            writeln!(f, "{}: {}", ptr, n)?;
        }
        Ok(())
    }
}

pub struct NodeEnv<'p, 'heap> {
    parent: Option<&'p NodeEnv<'p, 'heap>>,
    pub nodes: HashMap<Symbol, NodePtr<'heap>>
}

impl<'p, 'heap> NodeEnv<'p, 'heap> {
    pub fn new() -> Self {
        NodeEnv { parent: None, nodes: HashMap::new() }
    }

    pub fn child(parent: &'p NodeEnv<'p, 'heap>) -> Self {
        NodeEnv { parent: Some(parent), nodes: HashMap::new() }
    }

    // Construct all the builtins...
    pub fn default(heap: &mut Heap<'heap>) -> Self {
        let mut env = NodeEnv::new();
        env.set(Symbol::new(String::from("+"), 0), 
                heap.add(Node::PrimOp(PrimitiveOp::Add)));
        env.set(Symbol::new(String::from("-"), 0), 
                heap.add(Node::PrimOp(PrimitiveOp::Sub)));
        env.set(Symbol::new(String::from("*"), 0), 
                heap.add(Node::PrimOp(PrimitiveOp::Mul)));
        env.set(Symbol::new(String::from("/"), 0), 
                heap.add(Node::PrimOp(PrimitiveOp::Div)));
        env.set(Symbol::new(String::from("%"), 0), 
                heap.add(Node::PrimOp(PrimitiveOp::Mod)));
        env
    }

    pub fn extend(&mut self, child: HashMap<Symbol, NodePtr<'heap>>) {
        self.nodes.extend(child)
    }

    pub fn set(&mut self, id: Symbol, n: NodePtr<'heap>) {
        self.nodes.insert(id, n.clone());
    }

    pub fn get(&self, id: &Symbol) -> Option<NodePtr<'heap>> {
        match self.nodes.get(id) {
            Some(&ptr) => Some(ptr),
            None => match &self.parent {
                Some(parent) => parent.get(id),
                None => None
            }
        }
    }
}