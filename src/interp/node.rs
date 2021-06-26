use crate::core::lang::{
    ArgType, ParamType, Primitive, Cond, Symbol
};
use super::tim::TiMachine;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::fmt;


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

// (+) function in code:
// ParamExPos 0 1 - extract positional argument from slot 0 onto slot 1
// ParamExPos 0 2 - extract positional argument onto slot 2
// ParamEmpty 0 - assert no parameters left to extract
// Exec 1
// Exec 2
// Plus 3 1 2
// Ret 3

// fn f(b, a) { b(a) } in code:
// ParamExPos 0 1 -- extract "b" to 1
// ParamExPos 0 2 -- extract "a" to 2
// ParamEmpty 0
// EmptyArg 3 - push an empty arguments array to 3
// PosArg 3 2 - push "a" onto the array at 3
// Exec 1 - execute b, will evaluate ato an "Entrypoint"
// Thunk 5 1 4 - create a thunk in 5 with 1 as the entrypoint, 4 as the scope
// Push 5 3 - set reg 0 of new scope to 3 (the arg array)
// Ret 4

// Handling recursive bindings:
// let a = (b, 1)
// let b = (a, 2)

// Code for a:
// EmptyTuple 1
// TupleAppend 1 0 // append arg 0
// Prim 2 (1)
// Tuple Append 1 2
// Ret 1

// Code for b:
// EmptyTuple 1
// TupleAppend 1 0 // append arg 0
// Prim 2 (2)
// Tuple Append 1 2
// Ret 1

// 

#[derive(Clone, Copy, Eq, PartialEq, Hash, Debug)]
pub enum PrimitiveOp {
    Negate(usize, usize),
    Add(usize, usize, usize), Sub(usize, usize, usize), 
    Mul(usize, usize, usize), Div(usize, usize, usize), 
    Mod(usize, usize, usize),
    Or(usize, usize, usize), And(usize, usize, usize), 
    Xor(usize, usize, usize)
}

// Generic
pub struct Scope {
    reg: Vec<ValuePtr>, // the registers
}

pub enum Op {
    Eval(usize), // address to eval
    Ret(usize), // address to return

    // value constructors
    Prim(usize, Primitive), // store primtiive into register
    PrimitiveOp(PrimitiveOp),

    EmptyArgs(usize), // store empty args into an address
    EmptyList(usize), // store empty list into an address
    EmptyTuple(usize), // store empty tuple into an address

    PosArg(usize, usize), // prepend position arg (beginning of args)
    PosVarArg(usize, usize),
    KeyArg(usize, String, usize),
    ExNameArg(usize, usize, String),
    ExPosArg(usize, usize),
    // extracts the remaining position args to a list
    ExPosVar(usize, usize), 
    // extracts the remaining key args
    ExKeyVar(usize, usize), 

    Entrypoint(usize, usize), // dest, address
    EntrypointRel(usize, i64), // dest, relative address

    JmpIf(usize, usize), // relative jump
    JmpSegIf(usize, usize), // relative jump, conditional

    // NOTE: This kind of entrypoint cannot be executed
    // and should be removed before finalizing a code block
    // it is only used to make compilation easier
    EntrypointSeg(usize, i32), // dest, segment id, debug string

    Thunk(usize, usize), // dest, entrypoint (has an empty stack)
    Push(usize, usize), // dest thunk, reg to push
}

pub struct Code {
    ops: Vec<Op>,
}

pub enum Value {
    Code(Code),
    // codeptr (direct), offset into code, scopeptr (direct)
    Thunk(ValuePtr, usize, Scope),
    // codeptr (direct)
    Entrypoint(ValuePtr, usize),
    // valueptr (direct)
    Partial(ValuePtr, Vec<(ArgType, ValuePtr)>)
}

// A node pointer is a heap and a location
// within that heap
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ValuePtr {
    loc: usize,
}

impl fmt::Display for ValuePtr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "0x{:x}", self.loc)
    }
}

pub struct Segment {
    id: usize,
    code: Code,
    next_reg: usize // next free register in this segment
}

pub struct CodeBuilder {
    seg: Vec<Segment> // the segments
}

impl CodeBuilder {
    pub fn new_segment<'a>(&'a mut self) -> &'a mut Segment {
    }
}

#[derive(Clone, Debug)]
pub enum Node<'heap> {
    Bad,
    // applying an argument does not explicitly call
    // you must wrap it in a call node to then make the call
    // this way we can have a function with 0 arguments
    // and arbitrary arity
    App(NodePtr<'heap>, Vec<(ArgType, NodePtr<'heap>)>),
    Call(NodePtr<'heap>, Vec<(ArgType, NodePtr<'heap>)>),

    // If we panicked, will get propagated up the call hierarchy
    Panic,

    // The following is found only when compiling a function body
    Arg(usize),

    // When compiling a CoreExpr, an Ind
    // node may not point to anything containing any Arg() nodes
    // as during function instantiation, the NodePtr is copied in directly
    // and the ptr not followed for copying
    Ind(NodePtr<'heap>), // indirection

    Case(Vec<(Cond, NodePtr<'heap>)>),

    // arity, body
    Combinator(NodePtr<'heap>, Vec<ParamType>),
    // builtin combinator types
    Foreign(ForeignFunc<'heap>), // arity, foreignfunc
    PrimOp(PrimitiveOp), // to speed up builtins

    // The following cannot have any free variables
    // i.e Arg() variables:

    ConsVariant(u16, Vec<String>),
    ConsRecord(Vec<String>),
    ConsTuple(usize),
    ConsList, // list cons operator

    // when applied to a record
    // will produce a new record with the
    // following keys deleted
    DelKeys(Vec<String>),

    Idx(usize), // index into tuple
    Project(String), // index into record

    // a partially-bound function
    Partial(NodePtr<'heap>, Vec<(ArgType, NodePtr<'heap>)>),

    // Data types:
    Prim(Primitive),

    // The following 
    ListCons(NodePtr<'heap>, NodePtr<'heap>),
    ListEmpty,

    Variant(u16, Option<NodePtr<'heap>>, Vec<String>), 
    Record(HashMap<String, NodePtr<'heap>>),
    Tuple(Vec<NodePtr<'heap>>),

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
            ConsList => write!(f, "ConsList"),
            DelKeys(k) => write!(f, "DelKeys({:?})", k),
            Case(_) => write!(f, "Case"),
            Idx(i) => write!(f, "Idx({})", i),
            Project(s) => write!(f, "Project({})", s),
            Prim(p) => write!(f, "{}", p),
            ListCons(hd, tl) => write!(f, "[{}, ..{}]", hd, tl),
            ListEmpty => write!(f, "[]"),
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
                // check if it is something simple we can handle
                match self.at(ptr) {
                    Ind(p) => return *p,
                    Arg(idx) => return args[*idx],
                    App(_, _) => (),
                    Case (_) => (),
                    // everything else just return the original
                    // pointer
                    _ => return ptr
                }
                if let Some(nptr) = remap.get(&ptr) {
                    *nptr
                } else {
                    let nptr = self.add(Bad);
                    remap.insert(ptr, nptr);
                    queue.push(ptr);
                    nptr
                }
            };

            let copy = match &node {
                Ind(p) => Ind(*p),
                Arg(idx) => Ind(args[*idx]), // usually not necessary except at root
                App(l, r) => App(req_copy(*l), r.iter().map(
                    |(c, n)| (c.clone(), req_copy(*n))).collect()),
                Call(l, r) => App(req_copy(*l), r.iter().map(
                    |(c, n)| (c.clone(), req_copy(*n))).collect()),
                Case(alts) => Case(alts.iter().map(|(c, n)| {
                    (c.clone(), req_copy(*n))
                }).collect()),
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
        /*
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
                 */
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