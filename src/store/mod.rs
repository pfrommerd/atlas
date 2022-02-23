pub mod mem;
pub mod op;
pub mod object;

#[cfg(test)]
mod test;

pub use object::{ObjHandle, ObjectType, Numeric};
pub use crate::op_capnp::code::{
    Reader as CodeReader,
    Builder as CodeBuilder
};
use crate::Error;

// We use u64 instead of usize everywhere in order
// to ensure cross-platform binary
// compatibility
pub type AllocPtr = u64;
pub type AllocSize = u64;

#[derive(Clone, Copy, Debug)]
enum AllocationType {
    Object
}

pub trait Storage {
    type Segment<'s> : Segment<'s> where Self : 's;
    type MutSegment<'s> : MutSegment<'s> where Self : 's;
    type Allocation<'s> : Allocation<'s, Self> where Self : 's;

    fn alloc<'s>(&'s self, type_: AllocationType, size: AllocSize) -> Result<Self::Allocation<'s>, Error>;
    fn dealloc(&self, ptr: AllocPtr, size: AllocSize) -> Result<(), Error>;

    fn get_handle<'s>(&'s self, ptr: AllocPtr) -> Result<AllocHandle<'s, Self>, Error>;

    fn get<'s>(&'s self, handle: AllocPtr, off: AllocSize, len: AllocSize) 
                -> Result<Self::Segment<'s>, Error>;

    fn overwrite_atomic(&self, handle: AllocPtr, value: &[u8]) -> Result<(), Error>;
}

use std::ops::{Deref, DerefMut};
use std::convert::{AsMut, AsRef};
use std::borrow::{Borrow, BorrowMut};

pub trait Segment<'s> : Clone + AsRef<[u8]> + Deref<Target=[u8]> + Borrow<[u8]> {}

pub trait MutSegment<'s> : AsMut<[u8]> + DerefMut<Target=[u8]> + BorrowMut<[u8]> {}

pub trait Allocation<'s, S: Storage + 's + ?Sized> {
    fn get_mut<'a>(&'a mut self, off: AllocSize, len: AllocSize) 
            -> Result<S::MutSegment<'a>, Error>;
    fn complete(self) -> AllocHandle<'s, S>;
}


#[derive(Debug)]
pub struct AllocHandle<'s, S: Storage + ?Sized> {
    store: &'s S,
    type_: AllocationType,
    ptr: AllocPtr
}

impl<'s, S: Storage> AllocHandle<'s, S> {
    // This is unsafe since the alloc and the allocptr
    // must be associated
    pub fn new(store: &'s S, type_: AllocationType, ptr: AllocPtr) -> Self {
        AllocHandle { store, type_, ptr }
    }

    pub fn get_type(&self) -> AllocationType {
        self.type_
    }

    pub fn get(&self, off: AllocSize, len: AllocSize) -> Result<S::Segment<'s>, Error> {
        self.store.get(self.ptr, off, len)
    }
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
