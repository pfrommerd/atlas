use super::Numeric;
use bytes::Bytes;
use std::marker::PhantomData;

use super::{StringReader, BufferReader, TupleReader,
            RecordReader, PartialReader, CodeReader, StorageTypes};

pub enum Value<Ptr> {
    Indirect(Ptr),
    Unit,
    Bot,
    Char(char),
    Bool(bool),
    Numeric(Numeric),
    String(String),
    Buffer(Bytes),
    Thunk(Ptr),
    Nil, Cons(Ptr, Ptr),
    Tuple(Vec<Ptr>),
    Record(Vec<(Ptr, Ptr)>),
    Variant(Ptr, Ptr),
    Code(Code<Ptr>)
}

pub struct Code<Ptr> {
    values: Vec<Ptr>
}

use std::borrow::Borrow;

struct DirectStringReader<'s, R: Borrow<str>> {
    s: &'s R,
    _p : PhantomData<&'s ()>
}

impl<'s, R> StringReader<'s> for DirectStringReader<'s, R> 
        where R: Borrow<str> {
    type StringSlice<'r> where Self : 'r = &'r str;

    fn slice<'r>(&'r self, start: usize, len: usize) -> &'r str {
        &self.s.borrow()[start..start+len]
    }
}

struct DirectBufferReader<'s, R: Borrow<[u8]>> {
    s: &'s R,
    _p : PhantomData<&'s ()>
}

impl<'s, R> BufferReader<'s> for DirectBufferReader<'s, R> 
        where R: Borrow<[u8]> {
    type BufferSlice<'r> where Self : 'r = &'r [u8];

    fn slice<'r>(&'r self, start: usize, len: usize) -> &'r [u8] {
        &self.s.borrow()[start..start+len]
    }
}
struct DirectTupleReader<'s, S: StorageTypes, R: Borrow<Vec<S::Ptr>>> {
    s: &'s R,
    _p : PhantomData<&'s S::Ptr>
}

impl<'s, S, R> DirectTupleReader<'s, S, R>
        where S: StorageTypes, R: Borrow<Vec<S::Ptr>> {
    type Handle<'h> =
}