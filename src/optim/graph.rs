use crate::util::graph::{Graph, NodeRef};
use crate::value::{Storage};

pub type InputIdent = usize;

pub type CompRef = NodeRef;

#[derive(Clone)]
pub enum Primitive<'e> {
    Unit, Int(i64), Float(f64), Bool(bool), Char(char),
    String(&'e str), Buffer(&'e [u8]),
    EmptyList, EmptyTuple, EmptyRecord
}

#[derive(Clone)]
pub enum Case<'e> {
    Tag(&'e str),
    Eq(Primitive<'e>),
    Default
}

pub enum OpNode<'e, 's, S: Storage + 's> {
    // Bind is different from apply in that
    // apply can be called with a thunk, while
    // bind cannot
    Bind(CompRef, Vec<CompRef>),
    Invoke(CompRef),

    // WARNING: A user should never create an input
    // or a ret node and only use create_input() or create_ret()
    Input,
    Ret(CompRef),

    Force(CompRef),
    // an external object in the storage.
    // this could be either a constant
    // or another code block. External objects
    // are always in WHNF.
    External(S::ObjectRef<'s>),
    // builtin type, vector of inputs
    Builtin(&'e str, Vec<CompRef>), 
    Match(CompRef, Vec<Case<'e>>),
    Select(CompRef, Vec<CompRef>)
}

impl<'e, 's, S: Storage + 's> OpNode<'e, 's, S> {
    pub fn children(&self) -> Vec<CompRef> {
        use OpNode::*;
        let mut v : Vec<CompRef> = Vec::new();
        match self {
            Bind(c, a) => { v.push(*c); v.extend(a); },
            Invoke(c) => v.push(*c),
            Input => (),
            Ret(c) => v.push(*c),
            Force(c) => { v.push(*c); },
            External(_) => (),
            Builtin(_, a) => { v.extend(a); },
            Match(c, _) => { v.push(*c); }
            Select(c, a) => { v.push(*c); v.extend(a); }
        }
        v
    }
}

pub struct LamGraph<'e, 's, S: Storage + 's> {
    pub ops: Graph<OpNode<'e, 's, S>>,
    // numeric identifiers for the inputs
    input_idents: Vec<CompRef>,
    output: Option<CompRef>,
}

impl<'e, 's, S: Storage> Default for LamGraph<'e, 's, S> {
    fn default() -> Self {
        Self {
            ops: Graph::default(),
            input_idents: Vec::new(),
            output: None
        }
    }
}

impl<'e, 's, S: Storage> LamGraph<'e, 's, S> {
    pub fn create_input(&mut self) -> CompRef {
        let c = self.ops.insert(OpNode::Input);
        self.input_idents.push(c);
        c
    }

    pub fn create_ret(&mut self, val: CompRef) -> CompRef {
        let c = self.ops.insert(OpNode::Ret(val));
        self.output = Some(c);
        c
    }

    pub fn get_ret(&self) -> Option<CompRef> {
        self.output
    }
}