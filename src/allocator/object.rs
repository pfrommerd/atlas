
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

use crate::{Error, ErrorKind};
use super::{Storage, AllocPtr, AllocHandle, AllocationType, Allocation, AllocSize};
use num_enum::{IntoPrimitive, TryFromPrimitive};


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

pub struct ObjHandle<'a, Alloc: Storage> {
    alloc: AllocHandle<'a, Alloc>
}

impl<'s, S: Storage> std::cmp::PartialEq for ObjHandle<'s, S> {
    fn eq(&self, rhs : &Self) -> bool {
        self.alloc == rhs.alloc
    }
}

impl<'s, S: Storage> std::cmp::Eq for ObjHandle<'s, S> {}

impl<'s, S: Storage> std::hash::Hash for ObjHandle<'s, S> {
    fn hash<H>(&self, h: &mut H) where H: std::hash::Hasher {
        self.alloc.hash(h);
    }
}

impl<'s, S : Storage> Clone for ObjHandle<'s, S> {
    fn clone(&self) -> Self {
        Self { alloc: self.alloc.clone() }
    }
}

impl<'s, S: Storage> ObjHandle<'s, S> {
    pub fn from_unchecked(alloc: AllocHandle<'s, S>) -> Self {
        Self { alloc }
    }

    pub fn from_unchecked_ptr_bytes(store: &'s S, ptr: &[u8; 8]) -> Self {
        Self { alloc : AllocHandle::new(store, u64::from_le_bytes(*ptr)) }
    }

    pub fn to_ptr_bytes(&self) -> [u8; 8] {
        u64::to_le_bytes(self.alloc.ptr)
    }

    pub fn ptr(&self) -> AllocPtr { self.alloc.ptr }

    pub fn reader(&self) -> Result<ObjectReader<'s, S>, Error> {
        ObjectReader::try_from(self)
    }

    pub fn get_type(&self) -> Result<ObjectType, Error> {
        let cv = self.alloc.get(0, 1)?[0];
        ObjectType::try_from(cv).map_err(|_| { ErrorKind::BadFormat.into() })
    }

    pub fn as_thunk(&self) -> Result<ObjHandle<'s, S>, Error> {
        match self.reader()? {
            ObjectReader::Thunk(r) => Ok(r),
            _ => Err(Error::from(ErrorKind::IncorrectType))
        }
    }
    pub fn as_code(&self) -> Result<CodeReader<'s, S>, Error> {
        match self.reader()? {
            ObjectReader::Code(r) => Ok(r),
            _ => Err(Error::from(ErrorKind::IncorrectType))
        }
    }
    pub fn as_numeric(&self) -> Result<Numeric, Error> {
        match self.reader()? {
            ObjectReader::Numeric(n) => Ok(n),
            _ => Err(Error::from(ErrorKind::IncorrectType))
        }
    }
    pub fn as_string(&self) -> Result<StringReader<'s, S>, Error> {
        match self.reader()? {
            ObjectReader::String(n) => Ok(n),
            _ => Err(Error::from(ErrorKind::IncorrectType))
        }
    }
    pub fn as_record(&self) -> Result<RecordReader<'s, S>, Error> {
        match self.reader()? {
            ObjectReader::Record(r) => Ok(r),
            _ => Err(Error::from(ErrorKind::IncorrectType))
        }
    }

    // This is unsafe since the user must ensure no one has called get()
    // on the same object at the same time
    pub fn set_indirect(&'s self, other: ObjHandle<'s, S>) -> Result<(), Error> {
        // Get the current handle a mutable
        match self.get_type()? {
            ObjectType::Bot => {
                let mut new_bytes : [u8; 9] = [0; 9];
                new_bytes[0] = ObjectType::Indirect.into();
                let ptr_data : &mut [u8; 8] = &mut new_bytes[1..9].try_into().unwrap();
                *ptr_data = other.to_ptr_bytes();
                self.alloc.overwrite_atomic(&new_bytes[..])
            },
            _ => Err(Error::new_const(ErrorKind::IncorrectType, "Can only convert Bot to indirect"))
        }
    }
}

impl<'s, S: Storage> TryFrom<AllocHandle<'s, S>> for ObjHandle<'s, S> {
    type Error = Error;

    fn try_from(handle: AllocHandle<'s, S>) -> Result<Self, Self::Error> {
        if handle.get_type()? == AllocationType::Object {
            Ok(ObjHandle { alloc: handle })
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

// -------------------------- Readers --------------------------

pub enum ObjectReader<'s, S: Storage + 's> {
}

impl<'s, S: Storage + 's> TryFrom<&ObjHandle<'s, S>> for ObjectReader<'s, S> {
    type Error = Error;

    fn try_from(handle: &ObjHandle<'s, S>) -> Result<ObjectReader<'s, S>, Error> {
        let type_ = handle.get_type()?;
        let alloc = handle.alloc;
        use ObjectType::*;
        Ok(match type_ {
            // Int and Float should be parsed directly
            Unit => ObjectReader::Unit,
            Bot => ObjectReader::Bot,
            Indirect => {
                let data = handle.alloc.get(1, 9)?;
                let bytes : &[u8; 8] = &data[1..9].try_into().unwrap();
                ObjectReader::Indirect(ObjHandle::from_unchecked_ptr_bytes(alloc.store, bytes))
            },
            Int => {
                let data = handle.alloc.get(1, 9)?;
                let bytes : &[u8; 8] = &data[1..9].try_into().unwrap();
                ObjectReader::Numeric(Numeric::Int(i64::from_le_bytes(*bytes)))
            },
            Float => {
                let data = handle.alloc.get(1, 9)?;
                let bytes : &[u8; 8] = &data[1..9].try_into().unwrap();
                ObjectReader::Numeric(Numeric::Float(f64::from_le_bytes(*bytes)))
            },
            _ => panic!()
        })
    }
}

pub struct StringReader<'s, S: Storage> {
    handle: AllocHandle<'s, S>,
    len: AllocSize
}

pub struct StringSlice<'s, S: Storage + 's> {
    seg: S::Segment<'s>
}

impl<'s, S: Storage> StringReader<'s, S>  {
    pub fn new(handle: AllocHandle<'s, S>) -> Result<Self, Error> {
        let seg = handle.get(1, 8)?;
        let len_bytes: [u8; 8] = seg[1..9].try_into().unwrap();
        Ok(Self { handle, len : u64::from_le_bytes(len_bytes) })
    }

    pub fn get(&self) -> Result<StringSlice<'s, S>, Error> {
        Ok(StringSlice { seg: self.handle.get(9, self.len)? })

    }

    pub fn slice(&self, off: u64, len: usize) -> Result<StringSlice<'s, S>, Error> {
        Ok(StringSlice { seg: self.handle.get(9 + off, len as AllocSize)? })
    }
}

pub struct BufferReader<'s, S: Storage> {
    handle: AllocHandle<'s, S>,
    len: AllocSize
}
pub struct BufferSlice<'s, S: Storage + 's> {
    seg: S::Segment<'s>
}

impl<'s, S: Storage> BufferReader<'s, S>  {
    pub fn new(handle: AllocHandle<'s, S>) -> Result<Self, Error> {
        let seg = handle.get(1, 8)?;
        let len_bytes: [u8; 8] = seg[1..9].try_into().unwrap();
        Ok(Self { handle, len : u64::from_le_bytes(len_bytes) })
    }
}

pub struct RecordReader<'s, S: Storage> {
    handle: AllocHandle<'s, S>
}

pub struct TupleReader<'s, S: Storage> {
    handle: AllocHandle<'s, S>
}

pub struct PartialReader<'s, S: Storage> {
    handle: AllocHandle<'s, S>
}

pub struct CodeReader<'s, S: Storage> {
    handle: AllocHandle<'s, S>
}

// -------------------------- Builders --------------------------

trait ObjectBuilder<'s, S: Storage + 's> {
    fn complete(self) -> ObjHandle<'s, S>;
}

struct RecordBuilder<'s, S: Storage + 's> {
    alloc: S::Allocation<'s>
}


impl<'s, S: Storage + 's> RecordBuilder<'s, S> {
    pub fn new(store: &'s S, num_entries: u64) -> Result<Self, Error> {
        let mut alloc = store.alloc(AllocationType::Object, 
                        1 + 8 + 2*8*num_entries)?;
        {
            let mut header = alloc.get_mut(0, 9)?;
            header[0] = ObjectType::Record.into();
            let data : &mut [u8; 8] = &mut header[1..9].try_into().unwrap();
            *data = num_entries.to_le_bytes();
        }
        Ok(Self { alloc })
    }

    pub fn set(&mut self, i: u64, key: &ObjHandle<'s, S>, value: &ObjHandle<'s, S>) -> Result<(), Error> {
        let slice = self.alloc.get_mut(9 + 2*8 * i, 16)?;
        let key_data : &mut [u8; 8] = &mut slice[0..8].try_into().unwrap();
        let value_data : &mut [u8; 8] = &mut slice[9..16].try_into().unwrap();
        *key_data = key.ptr().to_le_bytes();
        *value_data = value.ptr().to_le_bytes();
        Ok(())
    }
}
impl<'s, S: Storage + 's> ObjectBuilder<'s, S> for RecordBuilder<'s, S> {
    fn complete(self) -> ObjHandle<'s, S> {
        ObjHandle::from_unchecked(self.alloc.complete())
    }
}

struct NumericBuilder<'s, S: Storage + 's> {
    alloc: S::Allocation<'s>
}

impl<'s, S: Storage + 's> NumericBuilder<'s, S> {
    pub fn new(store: &'s S, numeric: Numeric) -> Result<Self, Error> {
        let mut alloc = store.alloc(AllocationType::Object, 
                        1 + 8)?;
        {
            let mut header = alloc.get_mut(0, 9)?;
            header[0] = ObjectType::Record.into();
            let data : &mut [u8; 8] = &mut header[1..9].try_into().unwrap();
            *data = match numeric {
                Numeric::Int(i) => i.to_le_bytes(),
                Numeric::Float(f) => f.to_le_bytes()
            };
        }
        Ok(Self { alloc })
    }
}
impl<'s, S: Storage + 's> ObjectBuilder<'s, S> for NumericBuilder<'s, S> {
    fn complete(self) -> ObjHandle<'s, S> {
        ObjHandle::from_unchecked(self.alloc.complete())
    }
}