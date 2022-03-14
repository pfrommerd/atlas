pub use crate::util::graph::{Graph, NodeRef, Node};
use crate::core::lang::Primitive;
pub type InputIdent = usize;

pub type CodeGraph<H> = Graph<OpNode<H>>;

#[derive(Debug)]
#[derive(Clone)]
pub enum OpCase {
    Tag(String, NodeRef),
    Eq(Primitive, NodeRef),
    Default(NodeRef)
}

impl OpCase {
    pub fn target(&self) -> &NodeRef {
        match self {
        OpCase::Tag(_, r) => r,
        OpCase::Eq(_, r) => r,
        OpCase::Default(r) => r
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

    External(H),
    // An inline code graph so that we don't
    // generate so many objects during transpilation
    // A lot of these will be eliminated during optimization
    // Note that a regular external can point to code, which
    // is also a graph
    ExternalGraph(CodeGraph<H>),

    Builtin(String, Vec<NodeRef>), 
    Match(NodeRef, Vec<OpCase>),
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
            Input(_) | External(_) | ExternalGraph(_) => vec![],
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

use crate::store::value::Code;
use crate::store::Handle;

impl<'s, H: Handle<'s>> Into<Code<'s, H>> for &CodeGraph<H> {
    fn into(self) -> Code<'s, H> {
        todo!()
    }
}

impl<'s, H: Handle<'s>> Into<Code<'s, H>> for CodeGraph<H> {
    fn into(self) -> Code<'s, H> {
        (&self).into()
    }
}