use sharded_slab::Slab;

pub use sharded_slab::VacantEntry;
use aovec::Aovec;

#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct NodePtr(usize);
#[derive(Clone, Copy)]
pub struct GraphPtr(usize);

impl GraphPtr {
    pub fn from(u: usize) -> Self { GraphPtr(u) }
}

pub type LiftIdx = usize;
pub type InputIdx = usize;

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

pub enum Node<'e> {
    // a graphptr, as well as the associated
    // lift-in-pointers
    Func(GraphPtr),
    Apply(NodePtr, Vec<(ApplyType<'e>, NodePtr)>),
    Invoke(NodePtr),
    Input(InputIdx),
    Force(NodePtr),
    Primitive(Primitive<'e>),
    Builtin(&'static str, Vec<NodePtr>), // builtin type, vector of inputs
}

pub enum InputType<'e> {
    Lifted, Pos, Key(&'e str), Optional(&'e str), VarPos, VarKey
}

pub struct Graph<'e> {
    nodes: Slab<Node<'e>>,
    inputs: Aovec<InputType<'e>>,
    output: Option<NodePtr>,
}

impl<'e> Default for Graph<'e> {
    fn default() -> Self {
        Self { 
            nodes: Slab::new(), inputs: Aovec::new(8),
            output: None
        }
    }
}

pub type Entry<'s, T> = sharded_slab::Entry<'s, T>;

impl<'e> Graph<'e> {
    pub fn add_input(&self, t: InputType<'e>) -> NodePtr {
        let input_idx = self.inputs.push(t);
        let node = self.insert(Node::Input(input_idx));
        node
    }
    pub fn set_output(&mut self, n: NodePtr) {
        self.output = Some(n)
    }

    pub fn insert(&self, n : Node<'e>) -> NodePtr {
        NodePtr(self.nodes.insert(n).unwrap())
    }

    pub fn get<'s>(&'s self, i: NodePtr) -> Option<Entry<'s, Node<'e>>> {
        self.nodes.get(i.0)
    }
}

pub struct GraphCollection<'e> {
    graphs: Slab<Graph<'e>>,
    root: Option<GraphPtr>
}

impl Default for GraphCollection<'_> {
    fn default() -> Self {
        Self { graphs: Slab::new(), root: None }
    }
}

impl<'e> GraphCollection<'e> {
    pub fn alloc<'a>(&'a self) -> VacantEntry<'a, Graph<'e>> {
        self.graphs.vacant_entry().unwrap()
    }
    pub fn set_root(&mut self, root: GraphPtr) {
        self.root = Some(root);
    }
}