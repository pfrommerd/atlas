use super::{ValueReader, ValueBuilder};

use capnp::message::Builder;

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
    pub fn raw(&self) -> u64 { self.0 }
}

#[derive(Debug)]
pub struct StorageError {}

impl From<capnp::Error> for StorageError {
    fn from(_: capnp::Error) -> Self {
        Self {}
    }
}
impl From<capnp::NotInSchema> for StorageError {
    fn from(_: capnp::NotInSchema) -> Self {
        Self {}
    }
}

// An object storage manages an object's lifetime
pub trait Storage {
    type ValueRef<'s> : DataRef<'s> where Self: 's;
    type EntryRef<'s> : ObjectRef<'s, ValueRef=Self::ValueRef<'s>> where Self: 's;

    fn alloc<'s>(&'s self) -> Result<Self::EntryRef<'s>, StorageError>;
    fn get<'s>(&'s self, ptr: ObjPointer) -> Result<Self::EntryRef<'s>, StorageError>;

    fn insert_value<'s>(&'s self, val : ValueReader<'_>) -> Result<Self::ValueRef<'s>, StorageError>;

    fn insert_value_build<'s, F: Fn(ValueBuilder<'_>) -> Result<(), StorageError>>(&'s self, f: F) 
                                -> Result<Self::ValueRef<'s>, StorageError> {
        let mut builder = Builder::new_default();
        let mut root : ValueBuilder = builder.get_root().unwrap();
        f(root.reborrow())?;
        Ok(self.insert_value(root.into_reader())?)
    }

    fn insert_build<'s, F: Fn(ValueBuilder<'_>) -> Result<(), StorageError>>(&'s self, f: F) 
                                -> Result<Self::EntryRef<'s>, StorageError> {
        let mut builder = Builder::new_default();
        let mut root : ValueBuilder = builder.get_root().unwrap();
        f(root.reborrow())?;
        let entry = self.alloc()?;
        entry.set_value(self.insert_value(root.into_reader())?);
        Ok(entry)
    }

    // will skip directly to getting the value reference
    fn get_value<'s>(&'s self, ptr: ObjPointer) 
            -> Result<Self::ValueRef<'s>, StorageError> {
        self.get(ptr)?.get_value()
    }
}

pub trait ObjectRef<'s> : Clone {
    type ValueRef : DataRef<'s>;
    fn ptr(&self) -> ObjPointer;

    fn get_value(&self) -> Result<Self::ValueRef, StorageError>;

    // Will push a result value over a thunk value
    // Should panic if there is more than 2 push calls made
    fn set_value(&self, val: Self::ValueRef);
}

pub trait DataRef<'s> : Clone {
    fn reader<'r>(&'r self) -> ValueReader<'r>;
}