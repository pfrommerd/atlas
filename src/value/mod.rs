pub mod mem;
pub mod allocator;
pub mod op;
pub mod owned;

#[cfg(test)]
mod test;

use std::fmt;
use num_enum::{IntoPrimitive, TryFromPrimitive};

pub use allocator::{Allocator, AllocHandle, AllocSize, AllocPtr, Segment};

pub use crate::op_capnp::code::{
    Reader as CodeReader,
    Builder as CodeBuilder
};

pub use owned::{OwnedValue, Numeric, Code};

#[derive(Debug, Default)]
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


#[derive(IntoPrimitive, TryFromPrimitive)]
#[derive(PartialEq, Eq, Hash)]
#[repr(u64)]
pub enum ValueType {
    Bot, Indirect,
    Unit,
    Float, Int, Bool,
    Char, String, Buffer,
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
            Bot | Unit | Nil => 0,
            Indirect | Float | Int | Bool | Char | Thunk => 1,
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
pub struct ObjHandle<'a, Alloc: Allocator> {
    pub handle: AllocHandle<'a, Alloc>
}

impl<'a, A: Allocator> std::cmp::PartialEq for ObjHandle<'a, A> {
    fn eq(&self, rhs : &Self) -> bool {
        self.handle == rhs.handle
    }
}
impl<'a, A: Allocator> std::cmp::Eq for ObjHandle<'a, A> {}

impl<'a, A: Allocator> std::hash::Hash for ObjHandle<'a, A> {
    fn hash<H>(&self, h: &mut H) where H: std::hash::Hasher {
        self.handle.hash(h);
    }
}

impl<'a, Alloc: Allocator> Clone for ObjHandle<'a, Alloc> {
    fn clone(&self) -> Self {
        Self { handle: self.handle.clone() }
    }
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

    pub fn ptr(&self) -> AllocPtr {
        self.handle.ptr
    }

    pub fn get_type(&self) -> Result<ValueType, StorageError> {
        // This operation is safe since we are an object and guaranteed acces
        let cv = self.handle.get(0, 1)?.slice()[0];
        ValueType::try_from(cv).map_err(|_| { StorageError {} })
    }

    pub fn as_thunk(&self) -> Result<ObjHandle<'a, Alloc>, StorageError> {
        match self.to_owned()? {
            OwnedValue::Thunk(r) => Ok(r),
            _ => Err(StorageError::default())
        }
    }

    pub fn as_code(&self) -> Result<Code, StorageError> {
        match self.to_owned()? {
            OwnedValue::Code(r) => Ok(r),
            _ => Err(StorageError::default())
        }
    }

    pub fn as_numeric(&self) -> Result<Numeric, StorageError> {
        match self.to_owned()? {
            OwnedValue::Numeric(n) => Ok(n),
            _ => Err(StorageError::default())
        }
    }

    pub fn as_str(&self) -> Result<String, StorageError> {
        match self.to_owned()? {
            OwnedValue::String(n) => Ok(n),
            _ => Err(StorageError::default())
        }
    }

    pub fn as_record(&self) -> Result<Vec<(ObjHandle<'a, Alloc>, ObjHandle<'a, Alloc>)>, StorageError> {
        match self.to_owned()? {
            OwnedValue::Record(r) => Ok(r),
            _ => Err(StorageError::default())
        }
    }

    pub fn to_owned(&self) -> Result<OwnedValue<'a, Alloc>, StorageError> {
        // This is safe since the handle must be valid + point to an object
        unsafe { OwnedValue::unpack(self.handle) }
    }

    // This is unsafe since the user must ensure no one has called get()
    // on the same object at the same time
    pub unsafe fn set_indirect<'s>(&'s self, other: ObjHandle<'a, Alloc>) -> Result<(), StorageError> {
        assert_eq!(other.handle.alloc as *const _, self.handle.alloc as *const _);
        // Get the current handle a mutable
        let seg = self.handle.get(0, 2)?;
        let s=  seg.slice_mut();
        OwnedValue::Indirect(other).pack(s);
        Ok(())
    }
}

impl<'a, Alloc: Allocator> fmt::Display for ObjHandle<'a, Alloc> {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        write!(fmt, "&{}", self.ptr())
    }
}