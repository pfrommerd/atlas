use std::ops::Deref;

use sharded_slab::Slab;

#[derive(Debug)]
#[derive(Clone,Copy, PartialEq,Eq,Hash)]
#[repr(transparent)]
pub struct NodeRef(usize);

#[derive(Debug)]
pub struct Graph<N> {
    nodes: Slab<N>
}

impl<N> Default for Graph<N> {
    fn default() -> Self {
        Self { nodes: Slab::new() } 
    }
}

pub struct Slot<'g, N> {
    s: sharded_slab::VacantEntry<'g, N>
}

impl<'g, N> Slot<'g, N> {
    pub fn get_ref(&self) -> NodeRef {
        NodeRef(self.s.key())
    }
}

pub struct Entry<'g, N> {
    n: sharded_slab::Entry<'g, N>
}

impl<'g, N> Deref for Entry<'g, N> {
    type Target = N;
    fn deref<'s>(&'s self) -> &'s N {
        self.n.deref()
    }
}

impl<N> Graph<N> {
    pub fn slot<'g>(&'g self) -> Slot<'g, N> {
        Slot { s: self.nodes.vacant_entry().unwrap() }
    }

    pub fn insert(&self, node: N) -> NodeRef {
        NodeRef(self.nodes.insert(node).unwrap())
    }

    pub fn get<'g>(&'g self, r: NodeRef) -> Option<Entry<'g, N>> {
        self.nodes.get(r.0).map(|x| Entry { n: x })
    }
}