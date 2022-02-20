use crate::{Error, ErrorKind};
use super::{Storage, AllocSize, AllocHandle};
use num_enum::{IntoPrimitive, TryFromPrimitive};

/*
 * The object byte format is as follows (ranges are inclusive)
 * 
 * All values have the following header:
 * BYTE_LENGTH  | VALUE
 *  1           | The type tag.
 * 
 * Nil, Unit: Just the header
 * 
 * Bot:
 * | BYTE_LENGTH | VALUE
 * |          1  | Header
 * |          8  | Reserved for conversion to indirect
 * 
 * Indirect:
 * | BYTE_LENGTH | VALUE
 * |          1  | Header
 * |          8  | The target pointer
 * 
 * Bool:
 * | BYTE_LENGTH | VALUE
 * |          1  | HEADER
 * |          1  | The bool value
 * 
 * Char:
 * | BYTE_LENGTH | VALUE
 * |          1  | Header
 * |          4  | The full UTF-8 codepoint
 * 
 * Int, Float:
 * | BYTE_LENGTH | VALUE
 * |          1  | Header
 * |          4  | The full UTF-8 codepoint
 * |          8  | The i64, f64 value
 * 
 * String, Buffer:
 * | BYTE_LENGTH | VALUE
 * |          1  | Header
 * |          8  | The byte length of the payload
 * |         len | The payload (either UTF or just the bytes)
 *
 * Cons:
 * | BYTE_LENGTH | VALUE
 * |          1  | HEADER
 * |          8  | Pointer to head
 * |          8  | Pointer to tail
 * 
 * Record:
 * | BYTE_LENGTH | VALUE
 * |          1  | Header
 * |          8  | The number of record entries
 * |     2*8*len | List of record entries of the format [ptr_to_key, ptr_to_value]
 *
 * Tuple:
 * | BYTE_LENGTH | VALUE
 * |          1  | Header
 * |          8  | The number of tuple entries
 * |      8*len  | List of tuple entry pointers
 *
 * Variant:
 * | BYTE_LENGTH | VALUE
 * |          1  | Header
 * |          8  | Pointer to the type string
 * |          8  | Pointer to the value
 * 
 * 
 * Now we have the partial, thunk types
 * 
 * Partial:
 * | BYTE_LENGTH | VALUE
 * |          1  | Header
 * |          8  | Pointer to the code block
 * |          8  | Number of arguments
 * |     8*args  | List of arguments
 * 
 * Thunk:
 * | BYTE_LENGTH | VALUE
 * |          1  | Header
 * |          8  | Pointer to either the code or partial block to jump into
 * 
 * And last (and most complicated type) is the code type
 * 
 * 
 * Code:
 * | BYTE_LENGTH | VALUE
 * |          1  | Header
 * |          8  | Op buffer length
 * |          8  | Ready count
 * |  8*rdy_cnt  | List of indices into the op buffer of ready ops
 * |    buf_len  | The op buffer
 */


#[derive(IntoPrimitive, TryFromPrimitive)]
#[derive(PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
pub enum ObjectType {
    Bot, Indirect,
    Unit,
    Float, Int, Bool,
    Char, String, Buffer,
    Record, Tuple, Variant,
    Cons, Nil,
    Code, Partial, Thunk
}

impl ObjectType {
    // This is unsafe since the user must ensure that the handle
    // corresponds to this value type
    pub unsafe fn payload_size<S: Storage>(&self, handle: AllocHandle<'_, S>)
            -> Result<AllocSize, Error> {
        use ObjectType::*;
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

pub struct ObjHandle<'a, Alloc: Storage> {
    pub handle: AllocHandle<'a, Alloc>
}

impl<'s, S: Storage> std::cmp::PartialEq for ObjHandle<'s, S> {
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
    pub fn try_new(store: &'s S, ptr: AllocPtr) -> Result<Self, Error> {
        Self::try_from(store.get_handle(ptr))
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
    pub fn set_indirect<'s>(&'s self, other: ObjHandle<'s, S>) -> Result<(), Error> {
        assert_eq!(other.handle.alloc as *const _, self.handle.alloc as *const _);
        // Get the current handle a mutable
        let seg = self.handle.get(0, 2)?;
        let s=  seg.slice_mut();
        OwnedValue::Indirect(other).pack(s)?;
        Ok(())
    }
}

impl<'s, S: Storage> TryFrom<AllocHandle> for ObjHandle<'s, S> {
    type Error = Error;

    fn try_from(handle: AllocHandle) -> Result<Self, Self::Error> {
        if handle.get_type() == AllocationType::Object {
            Ok(ObjHandle { handle })
        } else {
            Err(Error::new_const(ErrorKind::Internal, 
                "Tried to construct object handle from non-object allocation"))
        }
    }
}

use std::fmt;
impl<'s, S: Storage> fmt::Display for ObjHandle<'s, S> {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        write!(fmt, "&{}", self.ptr())
    }
}

impl<'s, S: Storage> fmt::Debug for ObjHandle<'s, S> {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        write!(fmt, "&{}", self.ptr())
    }
}