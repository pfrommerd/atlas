use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;
use std::fmt;
use std::iter::FromIterator;

use crate::core::lang::{
    Expr, Body, Symbol, Literal, Bind, Atom, Alter, Format, Primitive
};


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

#[derive(Clone, Debug)]
pub enum CompoundType<'heap> {
    Tag(u16, NodePtr<'heap>),
    Prim(Primitive, NodePtr<'heap>),
    Default(NodePtr<'heap>)
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
pub enum Cond {
    Tag(u16),
    Eq(Primitive),
    Default
}

#[derive(Clone, Debug)]
pub enum Node<'heap> {
    // should only be found in combinator body
    // the Symbol is optinal and for debugging purposes
    Arg(usize),
    Ind(NodePtr<'heap>), // indirection

    // an unsaturated function applation, 
    // where NodePtr is the original node
    // the argument vector cannot contain free variables
    // i.e no Args
    Unsaturated(usize, NodePtr<'heap>, Vec<NodePtr<'heap>>),

    // arity body type. Body cannot contain free vars
    Combinator(usize, NodePtr<'heap>),

    // builtin combinator types
    Foreign(usize, ForeignFunc<'heap>), // arity, foreignfunc
    PrimOp(PrimitiveOp), // to speed up builtins
    Pack(u16, usize, Format), // tag, arity, resulting type

    Case(Vec<(Cond, NodePtr<'heap>)>), // case operation

    As(Format), // reformat operation
    Coerce, // coerce to a particular type
    Unpack(usize), // unpack operation

    // Data types:
    Prim(Primitive),
    // free variables (i.e no Args)
    Data(u16, Vec<NodePtr<'heap>>, Format),

    // The most holy of them all
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
            Pack(_, a, _) => Some(*a),
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
                           _: &NodeEnv<'_, 'heap>) -> NodePtr<'heap> {
        heap.add(Node::Prim(Primitive::from_literal(self.clone())))
    }
}

impl Compile for Atom {
    fn compile<'heap>(&self, heap: &mut Heap<'heap>, 
                           env: &NodeEnv<'_, 'heap>) -> NodePtr<'heap> {
        match self {
            Atom::Id(id) => match env.get(id) {
                Some(ptr) => ptr,
                None => panic!("Missing variable {:?}", id)
            },
            Atom::Lit(lit) => lit.compile(heap, env),
            Atom::Pack(tag, nargs, fmt) => {
                heap.add(Node::Pack(*tag, *nargs, fmt.clone()))
            },
            Atom::Unpack(i) => {
                heap.add(Node::Unpack(*i))
            },
            Atom::As(fmt) => {
                heap.add(Node::As(fmt.clone()))
            },
            // TODO: Handle Coerce, TypeOf
            _ => panic!("Atom type not implemented")
        }
    }
}

impl Compile for Body {
    fn compile<'heap>(&self, heap: &mut Heap<'heap>, 
                           env: &NodeEnv<'_, 'heap>) -> NodePtr<'heap> {
        match self {
            Body::Atom(a) => a.compile(heap, env),
            Body::Expr(e) => e.compile(heap, env)
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
            Let(bind, body) => {
                let sub_env = bind.compile(heap, env);
                body.compile(heap, &sub_env)
            },
            Lam(var, body) => {
                // keep extracting lambdas from the body until
                // we can split out a combinator
                let mut rbody = body;
                let mut args = Vec::new();
                args.push(var.clone());
                loop {
                    if let Body::Expr(r) = rbody {
                        if let Lam(ref var, ref body) = **r {
                            args.push(var.clone());
                            rbody = body;
                        } else {
                            break;
                        }
                    } else {
                        break;
                    }
                }
                // lambda lifting time!
                let ignore = HashSet::from_iter(args.iter().cloned());
                let free = body.free_variables(&ignore);
                let total_args = free.len() + args.len();
                let mut sub_env = NodeEnv::child(env);
                let mut i = 0;
                for s in free.iter() {
                    sub_env.set(s.clone(), heap.add(Node::Arg(i)));
                    i = i + 1;
                }
                // the last arguments are the real ones
                for a in args {
                    sub_env.set(a, heap.add(Node::Arg(i)));
                    i = i + 1;
                }
                let body_ptr = rbody.compile(heap, &sub_env);
                let mut comb = heap.add(Node::Combinator(total_args, body_ptr));
                // add apply nodes to pre-apply all of the free variables
                for s in free {
                    let a = match env.get(&s) {
                        Some(a) => a,
                        None => panic!("unable to find free variable for lambda")
                    };
                    comb = heap.add(Node::App(comb, a))
                }
                // apply the free varibles
                comb
            },
            Case(s, alts, expr) => {
                let mut n  = NodeEnv::child(env);
                let se = match s {
                    Some(s) => {
                        n.set(s.clone(), expr.compile(heap, env));
                        &n
                    },
                    None => env
                };
                let n = Node::Case(alts.iter().map(
                    |(a,e)| {
                        let t = match a {
                            Alter::Data(tag) => Cond::Tag(*tag),
                            Alter::Lit(l) => Cond::Eq(Primitive::from_literal(l.clone())),
                            Alter::Default => Cond::Default
                        };
                        (t, e.compile(heap, se))
                    }
                ).collect());
                heap.add(n)
            },
            Bad => panic!("Compiling bad node!")
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
            Arg(num) => write!(f, "Arg({})", num),
            Ind(p) => write!(f, "Ind({})", p),
            Unsaturated(a, p, args) => write!(f, "Unsat({}, {}, {:?})", a, p, args),
            Combinator(arity, body) => {
                write!(f, "Comb({}, {})", arity, body)
            },
            PrimOp(op) => write!(f, "{:?}", op),
            Pack(tag, arity, format) => {
                match format {
                    Format::Fields(fields) => {
                        write!(f, "PackFields({}, {{", tag)?;
                        let mut first = true;
                        for field in fields.iter() {
                            if !first { write!(f, ", ")?; }
                            write!(f, "{}", field)?;
                            first = false;
                        }
                        write!(f, "}})")
                    },
                    Format::Tuple(_) => {
                        write!(f, "PackTuple({})", arity)
                    },
                    Format::Variant(types) => {
                        write!(f, "PackVar({:?})", types)
                    }
                }
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
                    Char(c) => write!(f, "Char({})", c),
                    String(s) => write!(f, "String({})", s),
                    Buffer(c) => write!(f, "Buffer({})", c.len())
                }
            },
            Case(_) => write!(f, "Case"),
            As(_) => write!(f, "As"),
            Coerce => write!(f, "Coerce"),
            Unpack(i) => write!(f, "Unpack({})", i),
            Data(tag, args, format) => {
                match format {
                    Format::Fields(fields) => {
                        write!(f, "Fields({}, {{", tag)?;
                        let mut first = true;
                        for (i, field) in fields.iter().enumerate() {
                            if !first { write!(f, ", ")?; }
                            write!(f, "{}:{}", field, args[i])?;
                            first = false;
                        }
                        write!(f, "}})")
                    },
                    Format::Tuple(_) => {
                        write!(f, "Tuple(")?;
                        let mut first = true;
                        for arg in args {
                            if !first { write!(f, ", ")?; }
                            write!(f, "{}, ", arg)?;
                            first = false;
                        }
                        write!(f, ")")

                    },
                    Format::Variant(types) => {
                        let t = &types[*tag as usize];
                        let a = &args[0];
                        write!(f, "Var({}, {})", t, a)
                    }
                }
            },
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

    pub fn contains_free_vars(&self, n: NodePtr<'heap>) -> bool {
        use Node::*;
        match self.at(n) {
            Arg(_) => return true,
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
                App(l, r) => App(req_copy(*l), req_copy(*r)),
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