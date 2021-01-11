use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;
use std::fmt;

use crate::core::lang::{
    Expr, Id, Literal, Bind
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

#[derive(Debug, Clone)]
pub enum TypeNode {
    Prim(PrimitiveType),
}

impl fmt::Display for TypeNode {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            TypeNode::Prim(pt) => write!(f, "{}", pt)
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
    // types
    Star,
    Arrow(NodePtr<'heap>, NodePtr<'heap>),
    Type(TypeNode),

    // reference to a particular arg of a containing combinator
    // substituted on combinator evaluation. Note the combinators
    // can be type combinators too! (as in type 'a foo = 'a list
    // where foo is a type super-combinator!)
    // the handle is a reference back to the associated combinator

    ArgRef(usize, NodePtr<'heap>), 
    Indirection(NodePtr<'heap>),

    // Supercombinators:

    // user-defined supercombinator with arg types, body
    // note that the arg types can be dependent on the args
    // themselves! (i.e as in <'a> -> 'a list -> 'a) where a
    // generic type itself is an argument to the combinator
    Combinator(usize, NodePtr<'heap>),
    PrimOp(PrimitiveOp), // primitive supercombinator
    Pack(u16, usize, NodePtr<'heap>), // tag, arity and resulting type
    Foreign(usize, ForeignFunc<'heap>), // arity, foreignfunc

    // Basic value types:
    Prim(Primitive),
    Data(u16, Vec<NodePtr<'heap>>, NodePtr<'heap>), // tag, values, type

    // The most holy of them all
    App(NodePtr<'heap>, NodePtr<'heap>),

    Panic(String),
    Bad // An invalid node
}

impl<'heap> Node<'heap> {
    // compile into a mutable environment
    pub fn compile<'a>(heap: &mut Heap<'heap>, exp: &Expr,
                           env: &NodeEnv<'a, 'heap>) -> NodePtr<'heap> {
        let result_ptr = heap.add(Node::Bad); // reserve a pointer for this node
        let result = match exp {
            Expr::Type(_) => panic!("Type constructions not compiled"),
            Expr::Var(symb) => {
                return env.get(symb).unwrap()
            }
            Expr::Lit(literal) => Node::Prim(match literal {
                Literal::Unit => Primitive::Unit,
                Literal::Bool(b) => Primitive::Bool(*b),
                Literal::Int(i) => Primitive::Int(*i),
                Literal::Float(f) => Primitive::Float(f.into_inner()),
                Literal::Char(c) => Primitive::Char(*c),
                Literal::String(_) => panic!("String not yet implemented!")
            }),
            Expr::Pack{tag, arity, res_type} => Node::Pack(
                *tag, *arity,
                Node::compile(heap, res_type, env)
            ),
            Expr::Let(binds, body) => {
                let mut sub_env = NodeEnv::child(env);
                match binds {
                    Bind::NonRec(symb, expr) => {
                        sub_env.set(symb.id.clone(), Node::compile(heap, expr, env));
                    },
                    Bind::Rec(bindings) => {
                        let nodes : Vec<NodePtr> = bindings.iter().map(|(symb,_)| {
                            let ptr = heap.add(Node::Bad);
                            sub_env.set(symb.id.clone(), ptr);
                            ptr
                        }).collect();
                        for ((_, value), nptr) in bindings.iter().zip(nodes.iter()) {
                            let value_ptr = Node::compile(heap, value, &sub_env);
                            heap.set(*nptr, Node::Indirection(value_ptr));
                        }
                    }
                }
                Node::Indirection(Node::compile(heap, body, &sub_env))
            },
            Expr::Lam{var, body} => {
                // keep extracting lambdas from the body
                // until you can spit out a combinator
                let mut rbody: &Box<Expr> = body;
                let mut arg : usize = 0;
                let mut sub_env = NodeEnv::child(env);

                sub_env.set(var.id.clone(), heap.add(Node::ArgRef(arg, result_ptr)));
                while let Expr::Lam{ref var, ref body} = **rbody {
                    arg = arg + 1;
                    rbody = body;
                    sub_env.set(var.id.clone(), heap.add(
                        Node::ArgRef(arg, result_ptr)
                    ));
                }
                arg = arg + 1;
                Node::Combinator(arg, Node::compile(heap, rbody, &sub_env))
            },
            Expr::App(left, right) => {
                Node::App(Node::compile(heap, left, env), Node::compile(heap, right, env))
            }
            Expr::Case{expr:_, case_sym:_, alt:_, res_type:_} => panic!("Case not implemented"),
            _ => panic!("Unimplemented node type: {:?}", exp)
        };
        heap.set(result_ptr, result);
        result_ptr
    }

    pub fn direct_arity(&self) -> Option<usize> {
        use Node::*; 
        match self {
            Combinator(arity, _) => Some(*arity),
            PrimOp(op) => Some(op.arity()),
            Pack(_, arity, _) => Some(*arity),
            _ => None
        }
    }
}

impl fmt::Display for Node<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use Node::*;
        match self {
            Star => write!(f, "Star"),
            Arrow(left, right) => write!(f, "Arrow({} -> {})", left, right),
            Type(bt) => write!(f, "Type({})", bt),
            ArgRef(num, ptr) => write!(f, "Arg({} for {})", num, ptr),
            Indirection(ptr) => write!(f, "Ind({})", ptr),
            Combinator(arity, body) => {
                write!(f, "Comb({}, {}", arity, body)
            },
            PrimOp(op) => write!(f, "{:?}", op),
            Pack(tag, _, _) => {
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
            Data(tag, _, _) => write!(f, "Data({})", tag),
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
                Type(bt) => {
                    Type(match bt {
                        TypeNode::Prim(pt) => TypeNode::Prim(*pt)
                    })
                },
                ArgRef(arg, comb) =>
                    ArgRef(*arg, req_copy(comb)),
                Indirection(other) =>
                    Indirection(req_copy(other)),
                Combinator(arity, body) =>
                    Combinator(*arity, req_copy(body)),
                Data(tag, values, dtype) =>
                    Data(*tag, values.iter().map(|x| req_copy(x)).collect(),
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
                Type(bn) => {
                    match bn {
                        TypeNode::Prim(_) => ()
                    }
                },
                Indirection(real) => req_replace(real),
                Combinator(_, body) => req_replace(body),
                Data(_, values, dtype) => {
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
    nodes: HashMap<Id, NodePtr<'heap>>
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
        env.set(Id::new(String::from("+"), 0), 
                heap.add(Node::PrimOp(PrimitiveOp::IAdd)));
        env.set(Id::new(String::from("-"), 0), 
                heap.add(Node::PrimOp(PrimitiveOp::ISub)));
        // Disamb of 1 is used for unary versions of operators
        env.set(Id::new(String::from("~-"), 0), 
                heap.add(Node::PrimOp(PrimitiveOp::INegate)));
        env.set(Id::new(String::from("*"), 0), 
                heap.add(Node::PrimOp(PrimitiveOp::IMul)));
        env.set(Id::new(String::from("/"), 0), 
                heap.add(Node::PrimOp(PrimitiveOp::IDiv)));
        env.set(Id::new(String::from("%"), 0), 
                heap.add(Node::PrimOp(PrimitiveOp::IMod)));
        env
    }

    pub fn set(&mut self, id: Id, n: NodePtr<'heap>) {
        self.nodes.insert(id, n.clone());
    }

    pub fn get(&self, id: &Id) -> Option<NodePtr<'heap>> {
        match self.nodes.get(id) {
            Some(&ptr) => Some(ptr),
            None => match &self.parent {
                Some(parent) => parent.get(id),
                None => None
            }
        }
    }
}