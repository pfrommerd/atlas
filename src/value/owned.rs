use super::{ObjHandle, StorageError};
use capnp::message::ScratchSpaceHeapAllocator;
use bytes::Bytes;

use super::allocator::{Allocator, AllocHandle, AllocSize, Segment, Word};

use super::{ValueType, CodeBuilder, CodeReader};

pub enum OwnedValue<'a, Alloc: Allocator> {
    Bot,
    Indirect(ObjHandle<'a, Alloc>),
    Unit,
    Bool(bool), Char(char),
    Numeric(Numeric),
    String(String),
    Buffer(Bytes),

    Record(Vec<(ObjHandle<'a, Alloc>, ObjHandle<'a, Alloc>)>),
    Tuple(Vec<ObjHandle<'a, Alloc>>),
    Variant(ObjHandle<'a, Alloc>, ObjHandle<'a, Alloc>),
    Cons(ObjHandle<'a, Alloc>, ObjHandle<'a, Alloc>),
    Nil,

    Code(Code),
    Partial(ObjHandle<'a, Alloc>, Vec<ObjHandle<'a, Alloc>>),
    Thunk(ObjHandle<'a, Alloc>),
}

// Represents an owned code value
pub struct Code {
    builder: capnp::message::Builder<capnp::message::HeapAllocator>
}

impl Code {
    pub fn new() -> Code {
        let builder = capnp::message::Builder::new_default();
        Code { builder }    
    }

    pub fn from_buffer(buf: &[Word]) -> Code {
        let mut b = capnp::message::Builder::new_default();
        let any_ptr = capnp::any_pointer::Reader::new(
            capnp::private::layout::PointerReader::get_root_unchecked(&crate::util::raw_slice(buf)[0])
        );
        b.set_root_canonical(any_ptr).unwrap();
        Code { builder: b }
    }

    pub fn builder<'s>(&'s mut self) -> CodeBuilder<'s> {
        self.builder.get_root().unwrap()
    }

    pub fn reader<'s>(&'s self) -> CodeReader<'s> {
        self.builder.get_root_as_reader().unwrap()
    }

    pub fn word_size(&self) -> AllocSize {
        self.reader().total_size().unwrap().word_count
    }
}

impl<'a, Alloc : Allocator> OwnedValue<'a, Alloc> {
    pub fn value_type(&self) -> ValueType {
        use OwnedValue::*;
        match self {
            Bot => ValueType::Bot,
            Indirect(_) => ValueType::Indirect,
            Unit => ValueType::Unit,
            Bool(_) => ValueType::Bool,
            Char(_) => ValueType::Char,
            Numeric(n) => match n {
                self::Numeric::Float(_) => ValueType::Float,
                self::Numeric::Int(_) => ValueType::Int
            },
            String(_) => ValueType::String,
            Buffer(_) => ValueType::Buffer,
            Record(_)  => ValueType::Record,
            Tuple(_) => ValueType::Tuple,
            Variant(_, _) => ValueType::Variant,
            Cons(_,_) => ValueType::Cons,
            Nil => ValueType::Nil,
            Code(_) => ValueType::Code,
            Partial(_, _) => ValueType::Partial,
            Thunk(_) => ValueType::Partial
        }
    }

    pub fn word_size(&self) -> AllocSize {
        use OwnedValue::*;
        1 + match self {
            String(s) => 1 + ((s.len() as AllocSize + 7) / 8),
            Record(r) => 2*r.len() as AllocSize + 2,
            Tuple(t) => 1 + t.len() as AllocSize,
            Variant(_, _) => 2,
            Cons(_, _) => 2,
            Code(c) => 1 + c.word_size(),
            Partial(_, v) => 2 + v.len() as AllocSize,
            Thunk(_) => 1,
            _ => 1, // Must be at least 1 so we can fit an indirect
        }
    }

    pub fn pack_new(&self, alloc: &'a Alloc) -> Result<ObjHandle<'a, Alloc>, StorageError> {
        let size = self.word_size();
        let ptr= alloc.alloc(size)?;
        let seg = unsafe { alloc.get(ptr, 0, size)? };
        let slice = unsafe { seg.slice_mut() };
        self.pack(slice);
        unsafe {
            Ok(ObjHandle::new(alloc, ptr))
        }
    }

    pub fn pack(&self, slice: &mut [Word]) {
        slice[0] = self.value_type().into();
        match self {
            Self::Indirect(i) => slice[1] = i.ptr(),
            Self::Bot => slice[1] = 0,
            Self::Unit => slice[1] = 0,
            Self::Bool(b) => slice[1] = if *b { 1 } else { 0 },
            Self::Char(c) => slice[1] = *c as u64,
            Self::Numeric(Numeric::Int(i)) => slice[1] = *i as u64,
            Self::Numeric(Numeric::Float(f)) => slice[1] = f.to_bits(),
            Self::Buffer(b) => {
                slice[1] = b.len() as Word;
                let mut r = crate::util::raw_mut_slice(&mut slice[2..]);
                r = &mut r[0..b.len()];
                r.copy_from_slice(b.as_ref());
            },
            Self::String(s) => {
                // The second element in the slice is the length of the string
                slice[1] = s.len() as Word;
                // get a u8 slice after the type and length Words
                let mut r = crate::util::raw_mut_slice(&mut slice[2..]);
                r = &mut r[0..s.len()];
                r.copy_from_slice(s.as_bytes());
            },
            Self::Record(m) => {
                // store number of entries
                slice[1] = m.len() as Word;
                for (i, (k, v)) in m.iter().enumerate() {
                    slice[2 + 2*i] = k.ptr();
                    slice[2 + 2*i + 1] = v.ptr();
                }
            },
            Self::Tuple(t) => {
                slice[1] = t.len() as Word;
                let contents = &mut slice[1..];
                for (i, v) in t.iter().enumerate() {
                    contents[i] = v.handle.ptr;
                }
            },
            Self::Variant(k, v) => {
                slice[1] = k.ptr();
                slice[2] = v.ptr();
            },
            Self::Cons(k, v) => {
                slice[1] = k.ptr();
                slice[2] = v.ptr();
            },
            Self::Nil => slice[1] = 0,
            Self::Code(c) => {
                let size = c.word_size();
                slice[1] = size;
                // copy into the buffer
                let mut b = capnp::message::Builder::new(
                ScratchSpaceHeapAllocator::new(crate::util::raw_mut_slice(&mut slice[1..]))
                );
                b.set_root_canonical(c.reader()).unwrap();
            },
            Self::Partial(c, v) => {
                slice[1] = c.ptr();
                slice[2] = v.len() as Word;
                for (i, v) in v.iter().enumerate() {
                    slice[3 + i] = v.ptr();
                }
            },
            Self::Thunk(v) => {
                slice[1] = v.ptr();
            }
        }
    }

    pub unsafe fn unpack(handle: AllocHandle<'a, Alloc>) -> Result<Self, StorageError> {
        let t = ValueType::try_from(handle.get(0, 1)?.slice()[0]).unwrap();
        let payload_size = t.payload_size(handle)?;
        let payload = handle.get(1, payload_size)?;
        let payload = payload.slice();
        use ValueType::*;
        Ok(match t {
            Indirect => OwnedValue::Indirect(ObjHandle::new(handle.alloc, payload[0])),
            Unit => OwnedValue::Unit,
            Bot => OwnedValue::Bot,
            Nil => OwnedValue::Nil,
            Bool => OwnedValue::Bool(payload[0] == 1),
            Float => OwnedValue::Numeric(Numeric::Float(f64::from_bits(payload[0]))),
            Int => OwnedValue::Numeric(Numeric::Int(payload[0] as i64)),
            Char => OwnedValue::Char(char::from_u32(payload[0] as u32).unwrap()),
            String => {
                let len = payload[0];
                let slice = &crate::util::raw_slice(&payload[1..])[0..len as usize];
                OwnedValue::String(std::str::from_utf8(slice).unwrap().to_string())
            },
            Buffer => {
                let len = payload[0];
                let slice = &crate::util::raw_slice(&payload[1..])[0..len as usize];
                OwnedValue::Buffer(Bytes::copy_from_slice(slice))
            },
            Record => {
                let entries = payload[0];
                let mut vec = Vec::with_capacity(entries as usize);
                for i in 0..entries {
                    let key = ObjHandle::new(handle.alloc, payload[1 + 2*i as usize]);
                    let value = ObjHandle::new(handle.alloc, payload[2 + 2*i as usize]);
                    vec.push((key, value));
                }
                OwnedValue::Record(vec)
            },
            Tuple => {
                let len = payload[0];
                let mut vec = Vec::with_capacity(len as usize);
                for i in 0..len {
                    vec.push(ObjHandle::new(handle.alloc, payload[1 + i as usize]));
                }
                OwnedValue::Tuple(vec)
            },
            Variant => {
                let case = ObjHandle::new(handle.alloc, payload[0]);
                let value = ObjHandle::new(handle.alloc, payload[1]);
                OwnedValue::Variant(case, value)
            },
            Cons => {
                let head = ObjHandle::new(handle.alloc, payload[0]);
                let tail = ObjHandle::new(handle.alloc, payload[1]);
                OwnedValue::Cons(head, tail)
            },
            Code => OwnedValue::Code(self::Code::from_buffer(&payload[1..])),
            Partial => {
                let code= ObjHandle::new(handle.alloc, payload[0]);
                let len = payload[1];
                let mut args = Vec::with_capacity(len as usize);
                for i in 0..len {
                    args.push(ObjHandle::new(handle.alloc, payload[2 + i as usize]));
                }
                OwnedValue::Partial(code, args)
            },
            Thunk => {
                let target = ObjHandle::new(handle.alloc, payload[0]);
                OwnedValue::Thunk(target)
            }
        })
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