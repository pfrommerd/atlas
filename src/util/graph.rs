use std::ops::Deref;

use std::cell::RefCell;
use sharded_slab::Slab;

#[derive(Debug)]
#[derive(Clone,Copy, PartialEq,Eq,Hash)]
#[repr(transparent)]
pub struct NodeRef(usize);

pub struct Graph<N> {
    nodes: Slab<N>,
    keys: RefCell<Vec<usize>>
}

impl<N> Default for Graph<N> {
    fn default() -> Self {
        Self { nodes: Slab::new(), keys: RefCell::default() } 
    }
}

pub struct Slot<'g, N> {
    s: sharded_slab::VacantEntry<'g, N>,
}

impl<'g, N> Slot<'g, N> {
    pub fn insert(self, val: N) {
        self.s.insert(val)
    }
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
        let entry = self.nodes.vacant_entry().unwrap();
        self.keys.borrow_mut().push(entry.key());
        Slot { s: self.nodes.vacant_entry().unwrap() }
    }

    pub fn insert(&self, node: N) -> NodeRef {
        let entry = self.nodes.insert(node).unwrap();
        self.keys.borrow_mut().push(entry);
        NodeRef(entry)
    }

    pub fn get<'g>(&'g self, r: NodeRef) -> Option<Entry<'g, N>> {
        self.nodes.get(r.0).map(|x| Entry { n: x })
    }
}

use std::fmt;

impl<N : fmt::Debug> fmt::Debug for Graph<N> {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        write!(fmt, "Graph {{")?;
        let mut first = true;
        for k in self.keys.borrow().iter() {
            if !first { write!(fmt, ", ")? } else { first = false }
            let node = self.nodes.get(*k);
            if let Some(n) = node {
                write!(fmt, "{}: {:?}", k, n)?;
            }
        }
        write!(fmt, "}}")
    }
}