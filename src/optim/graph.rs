use crate::util::graph::{Graph, Node, NodeRef};

pub type LiftIdx = usize;
pub type InputIdx = usize;


// Both are graphs, but alias
// for clarity
pub type OpNodeRef = NodeRef;
pub type OpGraphRef = NodeRef;

pub enum ApplyType<'e> {
    Lifted, Pos, Key(&'e str),
    VarPos, VarKey
}

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

pub enum OpNode<'e> {
    // a graphptr, as well as the associated
    // lift-in-pointers
    Func(OpGraphRef),
    Apply(OpNodeRef, Vec<(ApplyType<'e>, OpNodeRef)>),
    Invoke(OpNodeRef),
    Input(InputIdx),
    Force(OpNodeRef),
    Primitive(Primitive<'e>),
    // builtin type, vector of inputs
    Builtin(&'e str, Vec<OpNodeRef>), 

    // TODO: How we handle branching should
    // probably be re-evaluated. For now we hardcode
    // two lambda: case takes an input, as well as a bunch
    // of cases to match against, and outputs a number based
    // on which case has been satisfied. Select takes a number (thunk)
    // and when forced will first force the selector number and then
    // force and return the resulting graph ref
    Match(OpGraphRef, Vec<Case<'e>>),
    Select(OpGraphRef, Vec<OpGraphRef>)
}

impl<'e> Node for OpNode<'e> {
    fn edge_vec(&self) -> Vec<NodeRef> {
        use OpNode::*;
        match &self {
            Func(_) => vec![],
            Apply(_, v) => 
                v.iter().map(|(_, r)| *r).collect(),
            Invoke(r) => vec![*r],
            Input(_) => vec![],
            Force(r) => vec![*r],
            Primitive(_) => vec![],
            Builtin(_, v) => v.clone(),
            _ => panic!("")
        }
    }
}

pub enum InputType<'e> {
    Lifted, Pos, Key(&'e str), Optional(&'e str), VarPos, VarKey
}

pub struct OpGraph<'e> {
    pub ops: Graph<OpNode<'e>>,
    input_format: Vec<InputType<'e>>,
    output: Option<NodeRef>,
}

// The opgraph itself is a node
// which can be used in a graph collection
impl<'e> Node for OpGraph<'e> {
    fn edge_vec(&self) -> Vec<OpGraphRef> {
        panic!("cannot take edges of opgraph")
    }
}

impl<'e> Default for OpGraph<'e> {
    fn default() -> Self {
        Self { 
            ops: Graph::default(),
            input_format: Vec::new(),
            output: None
        }
    }
}

impl<'e> OpGraph<'e> {
    pub fn add_input(&mut self, t: InputType<'e>) -> OpNodeRef {
        let input_idx = self.input_format.len();
        self.input_format.push(t);
        let node = self.ops.insert(OpNode::Input(input_idx));
        node
    }

    pub fn set_output(&mut self, n: OpNodeRef) {
        self.output = Some(n)
    }
}

pub struct OpGraphCollection<'e> {
    pub graphs: Graph<OpGraph<'e>>
}

impl Default for OpGraphCollection<'_> {
    fn default() -> Self {
        Self { graphs: Graph::default() }
    }
}