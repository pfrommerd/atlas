use super::{ValueReader};

// Object pointer and data pointer
// are wrappers

#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ObjPointer(u64);

#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct DataPointer(u64);

impl From<usize> for ObjPointer {
    fn from(p: usize) -> ObjPointer { ObjPointer(p as u64) }
}
impl From<u64> for ObjPointer {
    fn from(p: u64) -> ObjPointer { ObjPointer(p) }
}
impl ObjPointer {
    pub fn unwrap(&self) -> u64 { self.0 }
}

#[derive(Debug)]
pub struct StorageError {}

// An object storage manages an object's lifetime
pub trait Storage {
    type ValueRef<'s> : DataRef<'s> where Self: 's;
    type EntryRef<'s> : ObjectRef<'s, ValueRef=Self::ValueRef<'s>> where Self: 's;

    fn alloc<'s>(&'s self) -> Result<Self::EntryRef<'s>, StorageError>;
    fn get<'s>(&'s self, ptr: ObjPointer) -> Result<Self::EntryRef<'s>, StorageError>;

    fn insert<'s>(&'s self, val : ValueReader<'_>) -> Result<Self::ValueRef<'s>, StorageError>;

    // will skip directly to getting the value reference
    fn get_value<'s>(&'s self, ptr: ObjPointer) 
            -> Result<Self::ValueRef<'s>, StorageError> {
        self.get(ptr)?.get_value()
    }
}

pub trait ObjectRef<'s> {
    type ValueRef : DataRef<'s>;
    fn ptr(&self) -> ObjPointer;

    fn get_value(&self) -> Result<Self::ValueRef, StorageError>;

    // Will push a result value over a thunk value
    // Should panic if there is more than 2 push calls made
    fn push_result(&self, val: Self::ValueRef);

    // Will restore the old thunk value
    // and return the current value (if it exists)
    fn pop_result(&self);
}

pub trait DataRef<'s> {
    fn reader<'r>(&'r self) -> ValueReader<'r>;
}