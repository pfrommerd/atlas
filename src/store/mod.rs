pub mod mem;
pub mod op;
pub mod owned;
pub mod object;

#[cfg(test)]
mod test;

pub use object::{ObjHandle, ObjectType};
pub use owned::{OwnedValue, Numeric, Code};
pub use crate::op_capnp::code::{
    Reader as CodeReader,
    Builder as CodeBuilder
};


use crate::{Error};
use std::collections::HashMap;
use std::collections::hash_map;

pub struct Env<'s, S: Storage> {
    map: HashMap<String, ObjHandle<'s, S>>
}

impl<'s, S: Storage> Env<'s, S> {
    pub fn new() -> Self {
        Self { map: HashMap::new() }
    }

    pub fn insert(&mut self, key: String, value: ObjHandle<'s, S>) {
        self.map.insert(key, value);
    }

    pub fn iter<'m>(&'m self) -> hash_map::Iter<'m, String, ObjHandle<'s, S>> {
        self.map.iter()
    }
}

// We use u64 instead of usize everywhere in order
// to ensure cross-platform binary
// compatibility e.g if we are on a 32 bit system
// we can use a file produced on a 64 bit system since
// everything uses 64 bit addresses/alignment

// A handle *must* be just a u64
// but note that it doesn't have to correspond to an actual
// offset.
pub type AllocPtr = u64;

pub type AllocSize = u64;
pub type Word = u64;

enum AllocationType {
    Object
}

pub trait Storage {
    type Segment<'s> : Segment<'s, Self> where Self : 's;
    type Allocation<'s> : Allocation<'s, Self> where Self : 's;

    fn alloc<'s>(&'s self, type_: AllocationType, word_size: AllocSize) -> Result<Self::Allocation<'s>, Error>;
    fn dealloc(&self, ptr: AllocPtr, word_size: AllocSize) -> Result<(), Error>;

    fn get_handle<'s>(&'s self, ptr: AllocPtr) -> Result<AllocHandle<'s, Self>, Error>;

    fn segment<'s>(&'s self, handle: AllocPtr,
                word_off: AllocSize, word_len: AllocSize) 
                -> Result<Self::Segment<'s>, Error>;
}

use std::ops::Deref;
use std::borrow::{Borrow, BorrowMut};

pub trait Segment<'s, S: Storage> : Clone + Deref<Target=[u8]> + Borrow<[u8]> {
    fn handle(&self) -> AllocHandle<'s, S>;

    fn offset(&self) -> AllocSize;
    fn length(&self) -> AllocSize;
}

pub trait Allocation<'s, S: Storage> : AsMut<[u8]> + AsRef<[u8]> 
                    + Borrow<[u8]> + BorrowMut<[u8]> {
    fn get(&mut self) -> &mut [u8];
    fn complete(self) -> AllocHandle<'s, S>;
}

#[derive(Debug)]
pub struct AllocHandle<'s, S: Storage> {
    store: &'s S,
    type_: AllocationType,
    ptr: AllocPtr
}

impl<'s, S: Storage> std::cmp::PartialEq for AllocHandle<'s, S> {
    fn eq(&self, rhs : &Self) -> bool {
        self.ptr == rhs.ptr && self.store as *const _ == rhs.store as *const _
    }
}
impl<'a, S: Storage> std::cmp::Eq for AllocHandle<'a, S> {}

impl<'a, S: Storage> std::hash::Hash for AllocHandle<'a, S> {
    fn hash<H>(&self, h: &mut H) where H: std::hash::Hasher {
        self.ptr.hash(h);
        let ptr = self.store as *const S;
        ptr.hash(h);
    }
}

impl<'a, Alloc: Storage> Clone for AllocHandle<'a, Alloc> {
    fn clone(&self) -> Self {
        Self { store: self.store, type_: self.type_, ptr: self.ptr }
    }
}

impl<'s, S: Storage> Copy for AllocHandle<'s, S> {}

impl<'s, S: Storage> AllocHandle<'s, S> {
    // This is unsafe since the alloc and the allocptr
    // must be associated
    pub fn new(store: &'s S, type_: AllocationType, ptr: AllocPtr) -> Self {
        AllocHandle { store, type_, ptr }
    }

    pub fn get_type(&self) -> AllocationType {
        self.type_
    }

    pub fn get(&self, word_off: AllocSize, word_len: AllocSize) -> Result<S::Segment<'s>, Error> {
        self.alloc.get(self.ptr, word_off, word_len)
    }
}