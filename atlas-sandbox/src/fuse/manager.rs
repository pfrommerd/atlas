use dashmap::DashMap;
use std::sync::atomic;
use crate::{File, StorageId, FileId, Error};
use std::io::ErrorKind;
use super::dispatch::INode;


// Contains the INode manager
// and File handle manager

struct Node<F> {
    count: atomic::AtomicU64,
    file: F
}

// TODO: This is not thread safe
// and has some race conditions!
pub struct NodeManager<F> {
    node_map: DashMap<INode, Node<F>>,
    id_map: DashMap<(StorageId, FileId), INode>,
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
    pub fn get(&self, inode: INode) -> Result<F, Error> {
        self.node_map.get(&inode)
            .map(|n| n.file.clone())
            .ok_or(Error::from(ErrorKind::NotFound))
    }

    pub fn release(&self, inode: INode) {
        self.forget(inode, 1);
    }

    pub fn forget(&self, inode : INode, nlookup: u64) {
        let node = self.node_map.get(&inode).unwrap();
        let c = node.count.fetch_sub(nlookup, atomic::Ordering::SeqCst);
        // TODO: Between the fetch_sub and the remove,
        // we have a race condition with request()!
        if c <= 1 {
            self.node_map.remove(&inode);
        }
    }

    pub fn request(&self, f: &F) -> INode {
        let id = (f.storage_id(), f.id());
        match self.id_map.get(&id) {
            Some(id) => {
                let id = *id;
                let node = self.node_map.get(&id).unwrap();
                node.count.fetch_add(1, atomic::Ordering::SeqCst);
                return id
            },
            _ => ()
        }
        let inode = self.counter.fetch_add(1, atomic::Ordering::Relaxed);
        self.id_map.insert(id, inode);
        self.node_map.insert(inode, Node { 
            count: atomic::AtomicU64::new(1),
            file: f.clone() });
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