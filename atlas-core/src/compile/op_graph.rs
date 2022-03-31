pub use crate::util::graph::{Graph, NodeRef, Node};
use crate::core::lang::Literal;
use crate::Error;
use crate::store::op::BuiltinOp;
pub type InputIdent = usize;

pub type CodeGraph<H> = Graph<OpNode<H>>;

#[derive(Debug)]
#[derive(Clone)]
pub enum MatchCase {
    Tag(String, NodeRef),
    Eq(Literal, NodeRef),
    Default(NodeRef)
}

impl MatchCase {
    pub fn target(&self) -> &NodeRef {
        match self {
        MatchCase::Tag(_, r) => r,
        MatchCase::Eq(_, r) => r,
        MatchCase::Default(r) => r
        }
    }
}


#[derive(Debug)]
pub enum OpNode<H> {
    // Bind is different from apply in that
    // apply can be called with a thunk, while
    // bind cannot
    Bind(NodeRef, Vec<NodeRef>),
    Invoke(NodeRef),
    // WARNING: A user should never create an input
    // or a ret node and only use create_input() or create_ret()
    Input(usize),
    Force(NodeRef),

    Value(H),
    // An inline code graph so that we don't
    // generate so many objects during transpilation
    // A lot of these will be eliminated during optimization
    // Note that a regular external can point to code, which
    // is also a graph
    Graph(CodeGraph<H>),
    Match(NodeRef, Vec<MatchCase>),
    Builtin(BuiltinOp, Vec<NodeRef>), 
}

impl<H> Node for OpNode<H> {
    fn out_edges(&self) -> Vec<NodeRef> {
        use OpNode::*;
        match self {
            Bind(c, v) => {
                let mut vec = Vec::new();
                vec.push(c.clone());
                vec.extend(v.iter().cloned());
                vec
            },
            Invoke(c) => vec![c.clone()],
            Input(_) | Value(_) | Graph(_) => vec![],
            Force(c) => vec![c.clone()],
            Builtin(_, r) => r.clone(),
            Match(c, cases) => {
                let mut vec = Vec::new();
                vec.push(c.clone());
                vec.extend(cases.iter().map(|x| x.target().clone()));
                vec
            }
        }
    }
}

use crate::store::value::{Value, Code};
use crate::store::op::{Dest, Op, OpAddr, OpCase, RegID, ValueID, InputID};
use crate::store::{Storage, Handle, Storable};
use crate::util::graph::Flattened;
use std::collections::HashMap;

impl<'s, H: Handle<'s>> CodeGraph<H> {
    pub fn to_code<S: Storage<Handle<'s>=H>>(&self, s: &'s S) -> Result<Code<'s, S::Handle<'s>>, Error> {
        let Flattened { in_edges, mut order } = self.flatten()?;
        order.reverse();

        let mut regs : HashMap<NodeRef, RegID> = HashMap::new();
        for (i, nr) in order.iter().enumerate() {
            regs.insert(nr.clone(), i as RegID);
        }

        let mut addrs = HashMap::new();
        order.iter().enumerate().for_each(|(i, x)| {addrs.insert(x, i as OpAddr);} );

        let make_dest = |nr: &NodeRef| {
            let reg = *regs.get(nr).unwrap();
            let empty = vec![];
            let dests = in_edges.get(nr).unwrap_or(&empty);
            let uses = dests.iter().map(|x| *addrs.get(x).unwrap()).collect();
            Dest { reg, uses }
        };
        let get_reg = |nr: &NodeRef| *regs.get(nr).unwrap();

        let mut ops = Vec::new();
        let mut values = Vec::new();
        let mut ready = Vec::new();
        for nr in order.iter() {
            use OpNode::*;
            let op = match self.get(nr).unwrap() {
                Bind(l, args) =>
                    Op::Bind(make_dest(nr), get_reg(l), args.iter().map(get_reg).collect()),
                Invoke(i) =>
                    Op::Invoke(make_dest(nr), get_reg(i)),
                Input(i) => {
                    ready.push(*addrs.get(nr).unwrap());
                    Op::SetInput(make_dest(nr), *i as InputID)
                },
                Force(f) =>
                    Op::Force(make_dest(nr), get_reg(f)),
                Value(v) => {
                    let id = values.len() as ValueID;
                    values.push(v.clone());
                    ready.push(*addrs.get(nr).unwrap());
                    Op::SetValue(make_dest(nr), id)
                },
                Graph(g) => {
                    let h = g.store_in(s)?;
                    let id = values.len() as ValueID;
                    values.push(h);
                    ready.push(*addrs.get(nr).unwrap());
                    Op::SetValue(make_dest(nr), id)
                },
                Match(scrut, cases) => {
                    Op::Match(make_dest(nr), get_reg(scrut),
                        cases.iter().map(|case| {
                            use MatchCase::*;
                            Ok(match case {
                                Tag(val, n) => {
                                    let h = Literal::String(val.clone()).store_in(s)?;
                                    let id = values.len() as ValueID;
                                    values.push(h);
                                    OpCase::Tag(id, get_reg(&n))
                                }
                                Eq(val, n) => {
                                    let h = val.store_in(s)?;
                                    let id = values.len() as ValueID;
                                    values.push(h);
                                    OpCase::Eq(id, get_reg(&n))
                                },
                                Default(n) => OpCase::Default(get_reg(&n))
                            })
                        }).collect::<Result<Vec<OpCase>, Error>>()?)
                },
                Builtin(op, args) => {
                    if args.len() == 0 { ready.push(*addrs.get(nr).unwrap()) }
                    Op::Builtin(make_dest(nr), *op, args.iter().map(get_reg).collect())
                }
            };
            ops.push(op);
        }
        Ok(Code::new(
            get_reg(self.get_root().unwrap()),
            ready, ops, values
        ))
    }
}

impl<'s, S: Storage> Storable<'s, S> for CodeGraph<S::Handle<'s>> {
    fn store_in(&self, s: &'s S) -> Result<S::Handle<'s>, Error> {
        s.insert_from(&Value::Code(self.to_code(s)?))
    }
}