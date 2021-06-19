use crate::core::lang::{
    Expr, Body, Literal, Bind, Atom, Primitive, Cond,
    ArgType, ParamType
};
use super::node::{Heap, Node, NodeEnv, NodePtr};
use std::collections::HashSet;
use std::iter::FromIterator;

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
            Atom::ConsVariant(t, a) => {
                heap.add(Node::ConsVariant(*t, a.clone()))
            },
            Atom::ConsRecord(a) => {
                heap.add(Node::ConsRecord(a.clone()))
            },
            Atom::ConsTuple(s) => {
                heap.add(Node::ConsTuple(*s))
            },
            Atom::Idx(i) => {
                heap.add(Node::Idx(*i))
            },
            Atom::Project(s) => {
                heap.add(Node::Project(s.clone()))
            }
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
                let right = r.iter().map(
                    |(a, x)| (a.clone(), x.compile(heap, env))
                ).collect();
                let app = Node::App(left, right);
                return heap.add(app);
            },
            Call(l, r) => {
                let left = l.compile(heap, env);
                let right = r.iter().map(
                    |(a, x)| (a.clone(), x.compile(heap, env))
                ).collect();
                let app = Node::Call(left, right);
                return heap.add(app);
            },
            Let(bind, body) => {
                let sub_env = bind.compile(heap, env);
                body.compile(heap, &sub_env)
            },
            Lam(args , body) => {
                // lambda lifting time!
                let ignore = HashSet::from_iter(args.iter().map(|(a, s)| s.clone()));
                let free = body.free_variables(&ignore);

                let mut sub_env = NodeEnv::child(env);
                let mut i = 0;
                for s in free.iter() {
                    sub_env.set(s.clone(), heap.add(Node::Arg(i)));
                    i = i + 1;
                }
                // the last arguments are the real ones
                for (_, a) in args {
                    sub_env.set(a.clone(), heap.add(Node::Arg(i)));
                    i = i + 1;
                }
                let body_ptr = body.compile(heap, &sub_env);
                let mut arg_defs = Vec::new();
                for i in [0..free.len()] { arg_defs.push(ParamType::Pos); }
                arg_defs.extend(args.iter().map(|(a, _)| a.clone()));
                let comb = heap.add(Node::Combinator(body_ptr, arg_defs));
                let free_args = free.iter()
                    .map(|x| {
                        match env.get(x) {
                            Some(n) => (ArgType::Pos, n),
                            None => panic!("Unable to find symbol")
                        }
                    }).collect();
                heap.add(Node::App(comb, free_args))
            },
            Case(s, alts, expr) => {
                let mut n  = NodeEnv::child(env);
                let b = expr.compile(heap, env);
                let se = match s {
                    Some(s) => {
                        n.set(s.clone(), b);
                        &n
                    },
                    None => env
                };
                let n = Node::Case(alts.iter().map(
                    |(a,e)| {
                        (a.clone(), e.compile(heap, se))
                    }
                ).collect::<Vec<(Cond, NodePtr<'heap>)>>());
                let ce = heap.add(n);
                heap.add(Node::Call(ce, vec![(ArgType::Pos, b)]))
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