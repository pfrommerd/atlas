pub mod mem;
pub mod storage;
pub mod op;
pub mod owned;

#[cfg(test)]
mod test;


pub use storage::{Storage, AllocHandle, AllocSize, AllocPtr, Segment};

pub use crate::op_capnp::code::{
    Reader as CodeReader,
    Builder as CodeBuilder
};

pub use owned::{OwnedValue, Numeric, Code};

use std::fmt;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use crate::{Error, ErrorKind};
use std::collections::HashMap;
use std::collections::hash_map;

pub struct Env<'s, S: Storage> {
    map: HashMap<String, ObjHandle<'s, S>>
}

impl<'a, S: Storage> Env<'a, S> {
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

#[derive(IntoPrimitive, TryFromPrimitive)]
#[derive(PartialEq, Eq, Hash, Debug)]
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
    pub unsafe fn payload_size<Alloc : Storage>(&self, handle: AllocHandle<'_, Alloc>)
            -> Result<AllocSize, Error> {
        use ValueType::*;
        Ok(match self {
            Bot | Unit | Nil => 0,
            Indirect | Float | Int | Bool | Char | Thunk => 1,
            Variant | Cons => 2,
            String => {
                let len = handle.get(1, 1)?.slice()[0];
                1 + (len + 7)/8
            },
            Buffer => {
                let len = handle.get(1, 1)?.slice()[0];
                1 + (len + 7)/8
            },
            Record => {
                let entries = handle.get(1, 1)?.slice()[0];
                1 + 2*entries
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

pub struct ObjHandle<'a, Alloc: Storage> {
    pub handle: AllocHandle<'a, Alloc>
}

impl<'a, S: Storage> std::cmp::PartialEq for ObjHandle<'s, S> {
    fn eq(&self, rhs : &Self) -> bool {
        self.handle == rhs.handle
    }
}
impl<'s, S: Storage> std::cmp::Eq for ObjHandle<'s, S> {}

impl<'s, S: Storage> std::hash::Hash for ObjHandle<'s, S> {
    fn hash<H>(&self, h: &mut H) where H: std::hash::Hasher {
        self.handle.hash(h);
    }
}

impl<'s, S : Storage> Clone for ObjHandle<'s, S> {
    fn clone(&self) -> Self {
        Self { handle: self.handle.clone() }
    }
}

impl<'s, S: Storage> ObjHandle<'s, S> {
    // These are unsafe since the caller must ensure
    // that (1) the handle/ptr are valid and (2) that they point
    // to an object allocation
    pub unsafe fn new(store: &'s S, ptr: AllocPtr) -> Self {
        Self { handle : AllocHandle::new(store, ptr) }
    }
    pub unsafe fn from(handle: AllocHandle<'s, S>) -> Self {
        Self { handle }
    }

    pub fn ptr(&self) -> AllocPtr {
        self.handle.ptr
    }

    pub fn get_type(&self) -> Result<ValueType, Error> {
        // This operation is safe since we are an object and guaranteed acces
        let cv = self.handle.get(0, 1)?.slice()[0];
        ValueType::try_from(cv).map_err(|_| { ErrorKind::BadFormat.into() })
    }

    pub fn as_thunk(&self) -> Result<ObjHandle<'s, S>, Error> {
        match self.to_owned()? {
            OwnedValue::Thunk(r) => Ok(r),
            _ => Err(Error::from(ErrorKind::IncorrectType))
        }
    }

    pub fn as_code(&self) -> Result<Code, Error> {
        match self.to_owned()? {
            OwnedValue::Code(r) => Ok(r),
            _ => Err(Error::from(ErrorKind::IncorrectType))
        }
    }

    pub fn as_numeric(&self) -> Result<Numeric, Error> {
        match self.to_owned()? {
            OwnedValue::Numeric(n) => Ok(n),
            _ => Err(Error::from(ErrorKind::IncorrectType))
        }
    }

    pub fn as_str(&self) -> Result<String, Error> {
        match self.to_owned()? {
            OwnedValue::String(n) => Ok(n),
            _ => Err(Error::from(ErrorKind::IncorrectType))
        }
    }

    pub fn as_record(&self) -> Result<Vec<(ObjHandle<'s, S>, ObjHandle<'s, S>)>, Error> {
        match self.to_owned()? {
            OwnedValue::Record(r) => Ok(r),
            _ => Err(Error::from(ErrorKind::IncorrectType))
        }
    }

    pub fn to_owned(&self) -> Result<OwnedValue<'a, S>, Error> {
        // This is safe since the handle must be valid + point to an object
        unsafe { OwnedValue::unpack(self.handle) }
    }

    // This is unsafe since the user must ensure no one has called get()
    // on the same object at the same time
    pub unsafe fn set_indirect<'s>(&'s self, other: ObjHandle<'s, S>) -> Result<(), Error> {
        assert_eq!(other.handle.alloc as *const _, self.handle.alloc as *const _);
        // Get the current handle a mutable
        let seg = self.handle.get(0, 2)?;
        let s=  seg.slice_mut();
        OwnedValue::Indirect(other).pack(s)?;
        Ok(())
    }
}

impl<'a, Alloc: Storage> fmt::Display for ObjHandle<'a, Alloc> {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        write!(fmt, "&{}", self.ptr())
    }
}

impl<'a, Alloc: Storage> fmt::Debug for ObjHandle<'a, Alloc> {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        write!(fmt, "&{}", self.ptr())
    }
}