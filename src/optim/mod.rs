use std::collections::HashMap;

use crate::value::{ObjHandle, Allocator, StorageError};
pub mod graph;
pub mod compile;
pub mod pack;

pub struct CompileError {

}

impl From<StorageError> for CompileError {
    fn from(_: StorageError) -> Self {
        Self {}
    }
}

pub struct Env<'a, A: Allocator> {
    map: HashMap<String, ObjHandle<'a, A>>
}

impl<'a, A: Allocator> Env<'a, A> {
    pub fn new() -> Self {
        Self { map: HashMap::new() }
    }

    pub fn insert(&mut self, key: String, value: ObjHandle<'a, A>) {
        self.map.insert(key, value);
    }

    pub fn iter<'m>(&'m self) -> hash_map::Iter<'m, String, ObjHandle<'a, A>> {
        self.map.iter()
    }
}