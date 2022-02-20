use crate::util::graph::{Graph, NodeRef, Slot, Entry};
use crate::store::{Storage, ObjHandle};
use crate::core::lang::Primitive;

pub type InputIdent = usize;

pub type CompRef = NodeRef;

#[derive(Debug)]
#[derive(Clone)]
pub enum OpCase {
    Tag(String, CompRef),
    Eq(Primitive, CompRef),
    Default(CompRef)
}

impl OpCase {
    pub fn target(&self) -> CompRef {
        match self {
        OpCase::Tag(_, r) => *r,
        OpCase::Eq(_, r) => *r,
        OpCase::Default(r) => *r
        }
    }
}

#[derive(derivative::Derivative)]
#[derivative(Debug(bound=""))]
pub enum OpNode<'s, S: Storage> {
    // Bind is different from apply in that
    // apply can be called with a thunk, while
    // bind cannot
    Indirect(CompRef),
    Bind(CompRef, Vec<CompRef>),
    Invoke(CompRef),
    // WARNING: A user should never create an input
    // or a ret node and only use create_input() or create_ret()
    Input(usize),
    Force(CompRef),

    // External objects
    // are always in WHNF.
    External(ObjHandle<'s, S>),
    // An inline code graph so that we don't
    // generate so many objects during transpilation
    // A lot of these will be eliminated during optimization
    // Note that a regular external can point to code, which
    // is also a graph
    ExternalGraph(CodeGraph<'s, S>),

    Builtin(String, Vec<CompRef>), 
    Match(CompRef, Vec<OpCase>),
}

impl<'s, S: Storage> OpNode<'s, S> {
    pub fn children(&self) -> Vec<CompRef> {
        use OpNode::*;
        let mut v : Vec<CompRef> = Vec::new();
        match self {
            Indirect(i) => v.push(*i),
            Bind(c, a) => { v.push(*c); v.extend(a); },
            Invoke(c) => v.push(*c),
            Input(_) => (),
            Force(c) => { v.push(*c); },
            External(_) => (),
            ExternalGraph(_) => (),
            Builtin(_, a) => { v.extend(a); },
            Match(c, cases) => {
                v.push(*c);
                v.extend(cases.iter().map(|x| x.target()));
            }
        }
        v
    }
}


#[derive(derivative::Derivative)]
#[derivative(Debug(bound=""))]
pub struct CodeGraph<'s, S: Storage> {
    ops: Graph<OpNode<'s, S>>,
    // All of the input identifiers
    num_inputs: usize,
    output: Option<CompRef>,
}

impl<'s, S: Storage> Default for CodeGraph<'s, S> {
    fn default() -> Self {
        Self {
            ops: Graph::default(),
            num_inputs: 0,
            output: None
        }
    }
}

impl<'s, S: Storage> CodeGraph<'s, S> {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn insert(&self, node: OpNode<'s, S>) -> CompRef {
        self.ops.insert(node)
    }

    pub fn slot(&self) -> Slot<OpNode<'s, S>> {
        self.ops.slot()
    }

    pub fn create_input(&mut self) -> CompRef {
        let c = self.ops.insert(OpNode::Input(self.num_inputs));
        self.num_inputs = self.num_inputs + 1;
        c
    }

    pub fn set_output(&mut self, out: CompRef) {
        self.output = Some(out)
    }

    pub fn get_output(&self) -> Option<CompRef> {
        self.output
    }

    pub fn get(&self, comp: CompRef) -> Option<Entry<OpNode<'s, S>>> {
        self.ops.get(comp)
    }
}