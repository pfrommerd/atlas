pub mod mem;
pub mod allocator;
pub mod owned;

#[cfg(test)]
mod test;

use num_enum::{IntoPrimitive, TryFromPrimitive};

pub use allocator::{Allocator, AllocHandle, AllocSize, AllocPtr, Segment};

pub use crate::op_capnp::code::{
    Reader as CodeReader,
    Builder as CodeBuilder
};

pub use owned::OwnedValue;

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

pub struct ValueRef<'s, Alloc: Allocator + 's> {
    handle: AllocHandle<'s, Alloc>,
}
impl<'s, Alloc: Allocator + 's> ValueRef<'s, Alloc> {
    pub unsafe fn new(handle: AllocHandle<'s, Alloc>) -> Self {
        Self { handle }
    }

    pub fn get_type(&self) -> Result<ValueType, StorageError> {
        // This operation is safe since we are an object and guaranteed acces
        let cv = unsafe { self.handle.get(0, 1)?.slice()[0] };
        ValueType::try_from(cv).map_err(|_| { StorageError {} })
    }

    pub fn to_owned(&self) -> Result<OwnedValue<'s, Alloc>, StorageError> {
        // This is safe since the handle must be valid + point to an object
        unsafe { OwnedValue::unpack(self.handle) }
    }
}

pub struct MutValueRef<'s, Alloc: Allocator + 's> {
    handle: AllocHandle<'s, Alloc>
}

impl<'s, Alloc: Allocator + 's> MutValueRef<'s, Alloc> {
    pub unsafe fn new(handle: AllocHandle<'s, Alloc>) -> Self {
        Self { handle }
    }

    pub fn get_type(&self) -> Result<ValueType, StorageError> {
        // This operation is safe since we are an object and guaranteed access
        let cv = unsafe { self.handle.get(0, 1)?.slice()[0] };
        ValueType::try_from(cv).map_err(|_| { StorageError {} })
    }

    pub fn to_owned(&self) -> Result<OwnedValue<'s, Alloc>, StorageError> {
        // This is safe since the handle must be valid + point to an object
        unsafe { OwnedValue::unpack(self.handle) }
    }
}

#[derive(IntoPrimitive, TryFromPrimitive)]
#[repr(u64)]
pub enum ValueType {
    Indirect,
    Unit,
    Float, Int, Bool,
    String, Buffer,
    Record, Tuple, Variant,
    Cons, Nil,
    Code, Partial, Thunk
}

impl ValueType {
    // This is unsafe since the user must ensure that the handle
    // corresponds to this value type
    pub unsafe fn payload_size<Alloc : Allocator>(&self, handle: AllocHandle<'_, Alloc>)
            -> Result<AllocSize, StorageError> {
        use ValueType::*;
        Ok(match self {
            Unit | Nil => 0,
            Indirect | Float | Int | Bool | Thunk => 1,
            Variant | Cons => 2,
            String => {
                let len = handle.get(1, 1)?.slice()[0];
                (len + 7)/8
            },
            Buffer => {
                let len = handle.get(1, 1)?.slice()[0];
                (len + 7)/8
            },
            Record => {
                let entries = handle.get(1, 1)?.slice()[0];
                2*entries + 1
            },
            Tuple => {
                let entries = handle.get(1, 1)?.slice()[0];
                entries + 1
            },
            Code => {
                let len = handle.get(1, 1)?.slice()[0];
                len + 1
            },
            Partial => {
                let args = handle.get(2, 1)?.slice()[0];
                args + 2
            },
        })
    }
}

// An object handle wraps an alloc handle

#[derive(Debug)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ObjHandle<'a, Alloc: Allocator> {
    pub handle: AllocHandle<'a, Alloc>
}

impl<'a, Alloc: Allocator> ObjHandle<'a, Alloc> {
    // These are unsafe since the caller must ensure
    // that (1) the handle/ptr are valid and (2) that they point
    // to an object allocation
    pub unsafe fn new(alloc: &'a Alloc, ptr: AllocPtr) -> Self {
        Self { handle : AllocHandle::new(alloc, ptr) }
    }
    pub unsafe fn from(handle: AllocHandle<'a, Alloc>) -> Self {
        Self { handle }
    }
}

impl<'a, Alloc: Allocator> ObjHandle<'a, Alloc> {
    pub fn ptr(&self) -> AllocPtr {
        self.handle.ptr
    }

    // We are guaranteed the allocation is valid and is an object
    // by the new() unsafe conditions, so this is fine
    pub fn get<'s>(&'s self) -> Result<ValueRef<'s, Alloc>, StorageError> {
        unsafe {
            Ok(ValueRef::new(self.handle))
        }
    }

    // This is unsafe since the user must ensure no one has called get()
    // on the same object
    pub unsafe fn get_mut<'s>(&'s self) -> Result<MutValueRef<'s, Alloc>, StorageError> {
        Ok(MutValueRef::new(self.handle))
    }
}