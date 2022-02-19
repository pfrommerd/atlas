use crate::Error;
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

    fn alloc(&self, type_: AllocationType, word_size: AllocSize) -> Result<Allocation<'s, Self>, Error>;
    fn dealloc(&self, handle: AllocPtr, word_size: AllocSize);
    // The user must ensure that the handle, word_off, and word_len
    // are all valid
    fn get<'s>(&'s self, handle: AllocPtr,
                word_off: AllocSize, word_len: AllocSize) 
                -> Result<Self::Segment<'s>, Error>;
}

use std::ops::Deref;
use std::borrow::{Borrow, BorrowMut};

pub trait Segment<'s, S: Storage> : Clone + Deref<[u8]> + Borrow<[u8]> {
    fn handle(&self) -> AllocHandle<'s, S>;

    fn offset(&self) -> AllocSize;
    fn length(&self) -> AllocSize;
}

pub trait Allocation<'s, S: Storage> : AsMut<[u8]> + AsRef<[u8]> 
                    + Borrow<[u8]> + BorrowMut<[u8]> {
    fn complete() -> AllocHandle<'s, S>;
}

#[derive(Debug)]
pub struct AllocHandle<'s, S: Storage> {
    pub store: &'s S,
    pub ptr: AllocPtr
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
        Self { store: self.store, ptr: self.ptr }
    }
}

impl<'s, S: Storage> Copy for AllocHandle<'s, S> {}

impl<'s, S: Storage> AllocHandle<'a, S> {
    // This is unsafe since the alloc and the allocptr
    // must be associated
    pub unsafe fn new(store: &'a S, ptr: AllocPtr) -> Self {
        AllocHandle { store, ptr }
    }

    pub fn get(&self, word_off: AllocSize, word_len: AllocSize) -> Result<S::Segment<'a>, Error> {
        unsafe {
            self.alloc.get(self.ptr, word_off, word_len)
        }
    }
}