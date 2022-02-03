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
    type ObjectRef<'s> : ObjectRef<'s> where Self: 's;
    type Indirect<'s> : Indirect<'s, ObjectRef=Self::ObjectRef<'s>> where Self: 's;

    fn indirection<'s>(&'s self) -> Result<Self::Indirect<'s>, StorageError>;
    fn get<'s>(&'s self, ptr: ObjPointer) -> Result<Self::ObjectRef<'s>, StorageError>;
    fn insert<'s>(&'s self, val : ValueReader<'_>) -> Result<Self::ObjectRef<'s>, StorageError>;

    fn insert_build<'s, E, F: Fn(ValueBuilder<'_>) -> Result<(), E>>(&'s self, f: F) 
                                -> Result<Self::ObjectRef<'s>, E> {
        let mut builder = Builder::new_default();
        let mut root : ValueBuilder = builder.get_root().unwrap();
        f(root.reborrow())?;
        Ok(self.insert(root.into_reader()).unwrap())
    }
}

pub trait ValueRef<'s> {
    fn reader<'r>(&'r self) -> ValueReader<'r>;
}

pub trait ObjectRef<'s> : Clone + std::fmt::Debug {
    type ValueRef : ValueRef<'s>;
    fn ptr(&self) -> ObjPointer;
    // get a reference to the underlying value
    fn value(&self) -> Result<Self::ValueRef, StorageError>;
}

pub trait Indirect<'s> {
    type ObjectRef : ObjectRef<'s>;
    fn ptr(&self) -> ObjPointer;
    fn get_target(&self) -> Self::ObjectRef;
    // An indirection can only be set a single
    // time, after which it becomes immutable
    fn set(self, indirect: Self::ObjectRef) -> Result<Self::ObjectRef, StorageError>;
}