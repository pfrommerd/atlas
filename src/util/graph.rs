use aovec::Aovec;
use std::cell::Cell;

#[derive(Clone,Copy, PartialEq,Eq,Hash)]
#[repr(transparent)]
pub struct NodeRef(usize);

pub trait Node {
    //type EdgeIterator : Iterator<Item = NodeRef>;
    fn edge_vec(&self) -> Vec<NodeRef>;

    fn edges(&self) -> <Vec<NodeRef> as IntoIterator>::IntoIter {
        self.edge_vec().into_iter()
    }
}

// TODO: Make more efficient
// than one big RwLock
pub struct Graph<N : Node> {
    nodes: Aovec<Option<N>>,
    // remaps references into the nodes array
    // this way we can atomically replace
    // nodes
    references: Aovec<Cell<usize>>,
}

impl<N : Node> Default for Graph<N> {
    fn default() -> Self {
        Self { nodes: Aovec::new(16), 
            references: Aovec::new(16) }
    }
}

impl<N : Node> Graph<N> {
    pub fn insert(&self, node: N) -> NodeRef {
        let idx = self.nodes.push(Some(node));
        let r = self.references.push(Cell::new(idx + 1));
        NodeRef(r)
    }

    pub fn insert_into(&self, r: NodeRef, node: N) {
        let idx = self.nodes.push(Some(node));
        self.references[r.0].set(idx);
    }

    pub fn empty(&self) -> NodeRef {
        let idx = self.nodes.push(None);
        let r = self.references.push(Cell::new(idx + 1));
        NodeRef(r)
    }

    pub fn get(&self, r: NodeRef) -> Option<&N> {
        let ptr = self.references.get(r.0)?.get();
        let op = self.nodes.get(ptr)?;
        op.as_ref()
    }

    // Will drop this node reference
    // A node will only be considered deleted
    // when all the references to it have been dropped
    pub fn drop_ref(&self, r: NodeRef) {
        // make that reference slot pointer to 0
        self.references[r.0].set(0)
    }

    // sets the node src points to the same node as dest
    // this is useful for cyclical references since it lets
    // you pretend to know the final NodeReference, even if
    // you don't have it yet
    pub fn set_same(&self, dest: NodeRef, src: NodeRef) {
        self.references[dest.0].set(self.references[src.0].get() - 1)
    }
}