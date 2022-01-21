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

impl From<usize> for DataPointer {
    fn from(p: usize) -> DataPointer { DataPointer(p as u64) }
}
impl From<u64> for DataPointer {
    fn from(p: u64) -> DataPointer { DataPointer(p) }
}
impl DataPointer {
    pub fn unwrap(&self) -> u64 { self.0 }
}

#[derive(Debug)]
pub struct StorageError {}

// An object storage manages an object's lifetime
pub trait ObjectStorage {
    type EntryRef<'s> : ObjectRef<'s> where Self: 's;

    fn alloc<'s>(&'s self) -> Result<Self::EntryRef<'s>, StorageError>;
    fn get<'s>(&'s self, ptr: ObjPointer) -> Result<Self::EntryRef<'s>, StorageError>;

    fn get_data<'d, D: DataStorage>(&self, ptr: ObjPointer, data: &'d D) 
            -> Result<D::EntryRef<'d>, StorageError> {
        let dptr = self.get(ptr)?.get_current()?.ok_or(StorageError {})?;
        data.get(dptr)
    }
}

pub trait ObjectRef<'s> {
    fn ptr(&self) -> ObjPointer;

    fn get_current(&self) -> Result<Option<DataPointer>, StorageError>;

    fn get_data<'d, D: DataStorage>(&self, data: &'d D) 
                -> Result<D::EntryRef<'d>, StorageError> {
        data.get(self.get_current()?.ok_or(StorageError {})?)
    }

    // Will push a result value over a thunk value
    fn push_result(&self, val: DataPointer);

    // Will restore the old thunk value
    // and return the current value (if it exists)
    fn pop_result(&self) -> Option<DataPointer>;
}

pub trait DataStorage {
    type EntryRef<'s> : DataRef<'s> where Self: 's;

    // Insert may trigger a rearrangement of the
    // underlying memory, meaning we cannot do this in a multithreaded
    // fashion and need mutability
    fn insert<'s>(&'s mut self, val: ValueReader<'_>) 
                -> Result<Self::EntryRef<'s>, StorageError>;

    // however we can do simultaneous get() access
    fn get<'s>(&'s self, ptr : DataPointer)
                -> Result<Self::EntryRef<'s>, StorageError>;
}

pub trait DataRef<'s> {
    fn ptr(&self) -> DataPointer;

    fn value<'r>(&'r self) -> ValueReader<'r>;
}