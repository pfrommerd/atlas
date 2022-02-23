
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
use super::{Storage, AllocPtr, AllocHandle, AllocationType};
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
        self.handle.hash(h);
    }
}

impl<'s, S : Storage> Clone for ObjHandle<'s, S> {
    fn clone(&self) -> Self {
        Self { alloc: self.alloc.clone() }
    }
}

impl<'s, S: Storage> ObjHandle<'s, S> {
    pub fn from_unchecked(&self, alloc: AllocHandle<'s, S>) -> Self {
        Self { alloc }
    }

    pub fn ptr(&self) -> AllocPtr { self.handle.ptr }

    pub fn reader(&self) -> Result<ObjectReader<'s, S>, Error> {
        ObjectReader::try_from(self)
    }

    pub fn get_type(&self) -> Result<ObjectType, Error> {
        let cv = self.handle.get(0, 1)?[0];
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
                let seg = self.handle.get(0, 18)?;
                let new_bytes : [u8; 9] = [0; 9];
                new_bytes[0] = ObjectType::Indirect.into();
                new_bytes[1..9] = other.ptr().to_le_bytes();
                seg.set_atomic(0, &new_bytes[..]);
            },
            _ => Err(Error::new_const(ErrorKind::IncorrectType, "Can only convert Bot to indirect"))
        }
        Ok(())
    }
}

impl<'s, S: Storage> TryFrom<AllocHandle<'s, S>> for ObjHandle<'s, S> {
    type Error = Error;

    fn try_from(handle: AllocHandle<'s, S>) -> Result<Self, Self::Error> {
        if handle.get_type() == AllocationType::Object {
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Numeric {
    Int(i64),
    Float(f64)
}

impl Numeric {
    fn op(l: Numeric, r: Numeric, iop : fn(i64, i64) -> i64, fop : fn(f64, f64) -> f64) -> Numeric {
        match (l, r) {
            (Numeric::Int(l), Numeric::Int(r)) => Numeric::Int(iop(l, r)),
            (Numeric::Int(l), Numeric::Float(r)) => Numeric::Float(fop(l as f64, r)),
            (Numeric::Float(l), Numeric::Int(r)) => Numeric::Float(fop(l,r as f64)),
            (Numeric::Float(l), Numeric::Float(r)) => Numeric::Float(fop(l,r))
        }
    }
    pub fn add(l: Numeric, r: Numeric) -> Numeric {
        Self::op(l, r, |l, r| l + r, |l, r| l + r)
    }

    pub fn sub(l: Numeric, r: Numeric) -> Numeric {
        Self::op(l, r, |l, r| l - r, |l, r| l - r)
    }

    pub fn mul(l: Numeric, r: Numeric) -> Numeric {
        Self::op(l, r, |l, r| l * r, |l, r| l * r)
    }

    pub fn div(l: Numeric, r: Numeric) -> Numeric {
        Self::op(l, r, |l, r| l * r, |l, r| l * r)
    }
}

// -------------------------- Readers --------------------------

pub enum ObjectReader<'s, S: Storage + 's> {
    Bot, Indirect(ObjHandle<'s, S>),
    Unit,
    Numeric(Numeric), Bool(bool),
    Char(char), String(StringReader<'s, S>),
    Buffer(BufferReader<'s, S>),
    Record(RecordReader<'s, S>),
    Tuple(TupleReader<'s, S>),
    Variant(ObjHandle<'s, S>, ObjHandle<'s, S>),
    Cons(ObjHandle<'s, S>, ObjHandle<'s, S>), Nil,
    Code(CodeReader<'s, S>)
}

impl<'s, S: Storage + 's> TryFrom<&ObjHandle<'s, S>> for ObjectReader<'s, S> {
    type Error = Error;

    fn try_from(handle: &ObjHandle<'s, S>) -> Result<ObjectReader<'s, S>, Error> {
        let type_ = handle.get_type()?;
        handle.handle;
        use ObjectType::*;
        Ok(match type_ {
            // Int and Float should be parsed directly
            Unit => ObjectReader::Unit,
            Bot => ObjectReader::Bot,
            Indirect => {

            },
            Int => {
                ObjectReader::Numeric(Numeric::Int())
            },
            Float => {

            },
            _ => panic!()
        })
    }
}

pub struct StringReader<'s, S: Storage> {
    handle: AllocHandle<'s, S>
}

pub struct BufferReader<'s, S: Storage> {
    handle: AllocHandle<'s, S>
}

pub struct RecordReader<'s, S: Storage> {
    handle: AllocHandle<'s, S>
}

pub struct TupleReader<'s, S: Storage> {
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
    fn new(store: &'s S, num_entries: u64) -> Result<Self, Error> {
        let alloc = store.alloc(AllocationType::Object, 
                        1 + 8 + 2*8*num_entries)?;
        let header = alloc.get_mut(0, 9)?;
        header[0] = ObjectType::Record.into();
        header[1..9] = num_entries.to_le_bytes();
        Ok(Self { alloc })
    }

    fn set(&mut self, i: u64, key: &ObjHandle<'s, S>, value: &ObjHandle<'s, S>) -> Result<(), Error> {
        let slice = self.alloc.get_mut(9 + 2*8 * i, 16)?;
        slice[0..8] = key.ptr().to_le_bytes();
        slice[9..16] = value.ptr().to_le_bytes();
    }
}
impl<'s, S: Storage + 's> ObjectBuilder<'s, S> for RecordBuilder<'s, S> {
    fn complete(self) -> ObjHandle<'s, S> {
        self.alloc.complete()
    }
}

struct NumericBuilder<'s, S: Storage + 's> {
    alloc: S::Allocation<'s>
}

impl<'s, S: Storage + 's> NumericBuilder<'s, S> {
    fn new(store: &'s S, numeric: Numeric) -> Result<Self, Error> {
        let alloc = store.alloc(AllocationType::Object, 
                        1 + 8)?;
        let header = alloc.get_mut(0, 9)?;
        header[0] = ObjectType::Record.into();
        header[1..9] = match numeric {
            Numeric::Int(i) => i.to_le_bytes(),
            Numeric::Float(f) => f.to_le_bytes()
        };
        Ok(Self { alloc })
    }
}
impl<'s, S: Storage + 's> ObjectBuilder<'s, S> for NumericBuilder<'s, S> {
    fn complete(self) -> ObjHandle<'s, S> {
        self.alloc.complete()
    }
}