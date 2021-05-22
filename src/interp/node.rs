use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;
use std::fmt;

use crate::core::lang::{
    Expr, Symbol, Literal, Bind, Atom, Type
};


// No string primitive...
// strings literals are converted into
// lists of chars
#[derive(Debug, Copy, Clone)]
pub enum Primitive {
    Unit,
    Bool(bool), Int(i64),
    Float(f64), Char(char)
}

impl Primitive {
    pub fn as_int(&self) -> Option<i64> {
        match self {
            &Primitive::Int(i) => Some(i),
            _ => None
        }
    }
    pub fn as_float(&self) -> Option<f64> {
        match self {
            &Primitive::Float(f) => Some(f),
            _ => None
        }
    }
    pub fn as_char(&self) -> Option<char> {
        match self {
            &Primitive::Char(c) => Some(c),
            _ => None
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq, Hash, Debug)]
pub enum PrimitiveOp {
    BNegate, // flip boolean type
    IAdd, ISub, IMul, IDiv, IMod, INegate, // integer operations 
    FAdd, FSub, FMul, FDiv, FNegate, // float operations
}

impl PrimitiveOp {
    pub fn arity(&self) -> usize {
        use PrimitiveOp::*;
        match self {
            BNegate => 1, 
            IAdd => 2, ISub => 2, IMul => 2, IDiv => 2, IMod => 2, INegate => 1,
            FAdd => 2, FSub => 2, FMul => 2, FDiv => 2, FNegate => 1
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq, Hash, Debug)]
pub enum PrimitiveType {
    Bool, Int, Float, Char, Unit
}

impl fmt::Display for PrimitiveType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use PrimitiveType::*;
        match self {
            Bool => write!(f, "bool"), Int => write!(f, "int"),
            Float => write!(f, "float"), Char => write!(f, "char"),
            Unit => write!(f, "()")
        }
    }
}

#[derive(Clone)]
pub struct ForeignFunc<'heap>(&'static str, fn(&mut Heap<'heap>, Vec<NodePtr<'heap>>) -> NodePtr<'heap>);

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
    // should only be found in combinator body
    // the Symbol is optinal and for debugging
    Arg(usize, Option<Symbol>), 
    Ind(NodePtr<'heap>), // indirection

    // Function types:

    // an unsaturated function applation, 
    // where NodePtr is the original node
    // the argument vector cannot contain free variables
    // i.e no Args
    Unsaturated(usize, NodePtr<'heap>, Vec<NodePtr<'heap>>),

    // Arity, body. The body cannot depend on the outer-combinator
    // scope. i.e no free variables besides the arguments
    Combinator(usize, NodePtr<'heap>),

    PrimOp(PrimitiveOp), // primitive supercombinator
    Pack(u16, usize), // tag, arity, resulting type
    Foreign(usize, ForeignFunc<'heap>), // arity, foreignfunc

    // Data types:
    Prim(Primitive),
    // tag, values. Note that the values cannot have
    // free variables (i.e no Args)
    Data(u16, Vec<NodePtr<'heap>>), 

    // The most holy type
    App(NodePtr<'heap>, NodePtr<'heap>),

    Panic(String),
    Bad
}

impl<'heap> Node<'heap> {
    pub fn arity(&self) -> Option<usize> {
        use Node::*;
        match self {
            Unsaturated(a, _, ap) => Some(*a - ap.len()),
            Combinator(a, _) => Some(*a),
            PrimOp(op) => Some(op.arity()),
            Pack(_, a) => Some(*a),
            Foreign(a, _) => Some(*a),
            _ => None
        }
    }
}

pub trait Compile {
    fn compile<'heap>(&self, heap: &mut Heap<'heap>, env: &NodeEnv<'_, 'heap>) -> NodePtr<'heap>;
}

pub trait CompileEnv {
    fn compile<'heap, 'p>(&self, heap: &mut Heap<'heap>, env: &'p NodeEnv<'_, 'heap>) -> NodeEnv<'p, 'heap>;
}

impl Compile for Literal {
    fn compile<'heap>(&self, heap: &mut Heap<'heap>, 
                           env: &NodeEnv<'_, 'heap>) -> NodePtr<'heap> {
        use Literal::*;
        match self {
            Unit => heap.add(Node::Prim(Primitive::Unit)),
            Bool(b) => heap.add(Node::Prim(Primitive::Bool(*b))),
            Char(c)  => heap.add(Node::Prim(Primitive::Char(*c))),
            Int(i)  => heap.add(Node::Prim(Primitive::Int(*i))),
            Float(f)  => heap.add(Node::Prim(Primitive::Float(f.into_inner()))),
            String(_)  => panic!("String primitives not implemented")
        }
    }
}

impl Compile for Atom {
    fn compile<'heap>(&self, heap: &mut Heap<'heap>, 
                           env: &NodeEnv<'_, 'heap>) -> NodePtr<'heap> {
        match self {
            Atom::Id(id) => match env.get(id) {
                Some(ptr) => ptr,
                None => panic!("Missing variable")
            },
            Atom::Lit(lit) => lit.compile(heap, env),
            Atom::Expr(exp) => exp.compile(heap, env),
            _ => panic!("Unimplemented atom Type!")
        }
    }
}

impl Compile for Expr {
    fn compile<'heap>(&self, heap: &mut Heap<'heap>, 
                           env: &NodeEnv<'_, 'heap>) -> NodePtr<'heap> {
        use Expr::*;
        match self {
            Atom(a) => a.compile(heap, env),
            App(l, r) => {
                let left = l.compile(heap, env);
                let right = r.compile(heap, env);
                let app = Node::App(left, right);
                return heap.add(app);
            },
            _ => heap.add(Node::Bad)
        }
    }
}
impl CompileEnv for Bind {
    fn compile<'p, 'heap>(&self, heap: &mut Heap<'heap>, 
                           env: &'p NodeEnv<'_, 'heap>) -> NodeEnv<'p, 'heap> {
        let mut sub_env = NodeEnv::child(env);
        match self {
            Bind::NonRec(symb, expr) => {
                sub_env.set(symb.clone(), expr.compile(heap, env));
            },
            Bind::Rec(bindings) => {
                let nodes : Vec<NodePtr> = bindings.iter().map(|(symb,_)| {
                    let ptr = heap.add(Node::Bad);
                    sub_env.set(symb.clone(), ptr);
                    ptr
                }).collect();
                for ((_, value), nptr) in bindings.iter().zip(nodes.iter()) {
                    let value_ptr = value.compile(heap, &sub_env);
                    heap.set(*nptr, Node::Ind(value_ptr));
                }
            }
        }
        sub_env
    }
}

impl fmt::Display for Node<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use Node::*;
        match self {
            Arg(num, s) => write!(f, "Arg({}, {:?})", num, s),
            Ind(p) => write!(f, "Ind({})", p),
            Unsaturated(a, p, args) => write!(f, "Unsat({}, {}, {:?})", a, p, args),
            Combinator(arity, body) => {
                write!(f, "Comb({}, {})", arity, body)
            },
            PrimOp(op) => write!(f, "{:?}", op),
            Pack(tag, _) => {
                write!(f, "Pack({})", tag)
            },
            Foreign(arity, func) => {
                write!(f, "Foreign({}, {})", arity, func)
            },
            Prim(prim) => {
                use Primitive::*;
                match prim {
                    Unit => write!(f, "Unit"),
                    Bool(b) => write!(f, "Bool({})", b),
                    Int(i) => write!(f, "Int({})", i),
                    Float(fl) => write!(f, "Float({})", fl),
                    Char(c) => write!(f, "Char({})", c)
                }
            },
            Data(tag, _) => write!(f, "Data({})", tag),
            App(left, right) => write!(f, "App({} $ {})", left, right),
            Panic(s) => write!(f, "{}", s),
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

    pub fn contains_free_vars(&self, n: NodePtr<'heap>) -> bool {
        use Node::*;
        match self.at(n) {
            Arg(_, _) => return true,
            Ind(p) => return self.contains_free_vars(*p),
            App(l, r) => return self.contains_free_vars(*l) || self.contains_free_vars(*r),
            _ => return false
        }
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
                    Arg(idx, _) => return args[*idx],
                    App(_, _) => (),
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
                Arg(idx, _) => Ind(args[*idx]), // usually not necessary except at root
                App(l, r) => App(req_copy(*l), req_copy(*r)),
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
                heap.add(Node::PrimOp(PrimitiveOp::IAdd)));
        env.set(Symbol::new(String::from("-"), 0), 
                heap.add(Node::PrimOp(PrimitiveOp::ISub)));
        // Disamb of 1 is used for unary versions of operators
        env.set(Symbol::new(String::from("~-"), 0), 
                heap.add(Node::PrimOp(PrimitiveOp::INegate)));
        env.set(Symbol::new(String::from("*"), 0), 
                heap.add(Node::PrimOp(PrimitiveOp::IMul)));
        env.set(Symbol::new(String::from("/"), 0), 
                heap.add(Node::PrimOp(PrimitiveOp::IDiv)));
        env.set(Symbol::new(String::from("%"), 0), 
                heap.add(Node::PrimOp(PrimitiveOp::IMod)));
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