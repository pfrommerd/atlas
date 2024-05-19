use dashmap::DashMap;
use std::sync::atomic;
use crate::fs::{File, Id};
use super::dispatch::INode;

// Contains the INode manager
// and File handle manager

struct Node<F> {
    count: atomic::AtomicI64,
    file: F
}

// TODO: This is not thread safe
// and has some race conditions!
pub struct NodeManager<F> {
    node_map: DashMap<INode, Node<F>>,
    id_map: DashMap<Id, INode>,
    counter: atomic::AtomicU64
}

impl<'fs, F : File<'fs>> NodeManager<F> {
    pub fn new() -> Self {
        Self {
            node_map: DashMap::new(),
            id_map: DashMap::new(),
            counter: atomic::AtomicU64::new(1)
        }
    }
    pub fn get(&self, inode: INode) -> Option<F> {
        self.node_map.get(&inode)
            .map(|n| n.file.clone())
    }

    pub fn forget(&self, inode : INode, nlookup: u64) -> u64 {
        let mut h = 0;
        self.node_map.remove_if(&inode, |_, node| {
            let c = node.count.fetch_sub(nlookup as i64, atomic::Ordering::SeqCst);
            let c = std::cmp::max(c - (nlookup as i64), 0) as u64;
            h = c;
            if c == 0 {
                self.id_map.remove(&node.file.id());
            }
            c == 0
        });
        h
    }

    pub fn lookup(&self, f: &F) -> INode {
        let id = f.id();
        match self.id_map.get(&id) {
            Some(nid) => {
                let nid = *nid;
                let node = self.node_map.get(&nid).unwrap();
                node.count.fetch_add(1, atomic::Ordering::SeqCst);
                return nid
            },
            _ => ()
        }
        let inode = self.counter.fetch_add(1, atomic::Ordering::Relaxed);
        self.node_map.insert(inode, Node { 
            count: atomic::AtomicI64::new(1),
            file: f.clone() });
        self.id_map.insert(id, inode);
        inode
    }
}

pub struct HandleManager<H> {
    handle_map: DashMap<u64, H>,
    counter: atomic::AtomicU64
}

impl<H> HandleManager<H> {
    pub fn new() -> Self {
        Self {
            handle_map: DashMap::new(),
            counter: atomic::AtomicU64::new(0)
        }
    }
    pub fn get<'r>(&'r self, handle: u64) 
            -> Option<dashmap::mapref::one::Ref<'r, u64, H>> {
        self.handle_map.get(&handle)
    }
    pub fn insert(&self, handle: H) -> u64 {
        let id = self.counter.fetch_add(
            1, atomic::Ordering::Relaxed
        );
        self.handle_map.insert(id, handle);
        id
    }
    pub fn remove(&self, handle: u64) {
        self.handle_map.remove(&handle);
    }
}