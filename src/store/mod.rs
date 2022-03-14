use crate::Error;

pub mod op;
pub mod value;
pub mod heap;

#[cfg(test)]
pub mod test;


pub trait Storage {
    type Handle<'s> : Handle<'s> where Self: 's;
    // Indirect is special
    type IndirectBuilder<'s> : IndirectBuilder<'s, Self::Handle<'s>> where Self : 's;
    // Indirect is special, since we need
    // to potentially modify the indirect after it is built
    fn indirect<'s>(&'s self) -> Result<Self::IndirectBuilder<'s>, Error>;

    // You should be able to build using any reader
    // which has the same pointer type
    fn insert<'s, 'p, R>(&'s self, src: &R) -> Result<Self::Handle<'s>, Error>
            where R: ObjectReader<'p, 's, Self::Handle<'s>>;

    fn insert_from<'s, 'p, R>(&'s self, src: R) -> Result<Self::Handle<'s>, Error>
            where R: ObjectReader<'p, 's, Self::Handle<'s>> {
        self.insert(&src)
    }
}

use std::fmt::{Debug, Display};

pub trait Handle<'s> : Sized + Clone + Display + Debug {
    type Reader<'p>: ObjectReader<'p, 's, Self> where Self: 'p;
    fn reader<'p>(&'p self) -> Result<Self::Reader<'p>, Error>;
}


pub enum ObjectType {
    Bot, Indirect, Unit, Int, Float, Bool, Char,
    String, Buffer, 
    Record, Tuple, Variant, 
    Cons, Nil, 
    Thunk, Code, Partial
}

pub trait ObjectReader<'p, 's, H: Handle<'s>> {
    type StringReader : StringReader<'p>;
    type BufferReader : BufferReader<'p>;
    type TupleReader : TupleReader<'p, 's, H>;
    type RecordReader : RecordReader<'p, 's, H>;
    type CodeReader : CodeReader<'p, 's, H>;
    type PartialReader : PartialReader<'p, 's, H>;

    // The direct subhandle type
    type Subhandle : Borrow<H>;

    fn get_type(&self) -> ObjectType;
    fn which(&self) -> ReaderWhich<Self::Subhandle,
            Self::StringReader, Self::BufferReader,
            Self::TupleReader, Self::RecordReader,
            Self::CodeReader, Self::PartialReader>;

    fn as_code(&self) -> Self::CodeReader {
        match self.which() {
            ReaderWhich::Code(c) => c,
            _ => panic!("Expected code")
        }
    }
}

pub enum ReaderWhich<H, S, B, T, R, C, P> {
    Bot, Indirect(H),
    Unit,
    Int(i64), Float(f64), Bool(bool),
    Char(char), String(S),
    Buffer(B),
    Record(R),
    Tuple(T),
    Variant(H, H),
    Cons(H, H), Nil,
    Code(C),
    Partial(P),
    Thunk(H)
}

use std::ops::Deref;
use std::borrow::Borrow;

pub trait StringReader<'p> {
    type StringSlice<'r> : Deref<Target=str> where Self : 'r;
    fn slice<'r>(&'r self, start: usize, len: usize) -> Self::StringSlice<'r>;
    fn len(&self) -> usize;
}

pub trait BufferReader<'p> {
    type BufferSlice<'r> : Deref<Target=[u8]> where Self : 'r;
    fn slice<'r>(&'r self, start: usize, len: usize) -> Self::BufferSlice<'r>;
    fn len(&self) -> usize;
}

pub trait TupleReader<'p, 's, H : Handle<'s>> {
    type Subhandle : Borrow<H>;

    type EntryIter<'r> : Iterator<Item=Self::Subhandle> where Self : 'r;
    fn iter<'r>(&'r self) -> Self::EntryIter<'r>;

    fn len(&self) -> usize;
    fn get(&self, i: usize) -> Option<Self::Subhandle>;
}

pub trait RecordReader<'p, 's, H : Handle<'s>> {
    type Subhandle : Borrow<H>;

    type EntryIter<'r> : Iterator<Item=(Self::Subhandle, Self::Subhandle)> where Self : 'r;
    fn iter<'r>(&'r self) -> Self::EntryIter<'r>;

    fn len(&self) -> usize;
    fn get(&self, i: usize) -> Option<(Self::Subhandle, Self::Subhandle)>;
}

pub trait PartialReader<'p, 's, H : Handle<'s>> {
    type Subhandle : Borrow<H>;
    type ArgsIter<'r> : Iterator<Item=Self::Subhandle> where Self : 'r;

    fn get_code(&self) -> Self::Subhandle;
    fn num_args(&self) -> usize;
    fn get_arg(&self, i: usize) -> Option<Self::Subhandle>;

    fn iter_args<'r>(&'r self) -> Self::ArgsIter<'r>;
}

use op::{Op, OpAddr, ValueID};

pub trait CodeReader<'p, 's, H : Handle<'s>> {
    type Subhandle : Borrow<H>;

    type ReadyIter<'r> : Iterator<Item=OpAddr> where Self: 'r;
    type OpIter<'r> : Iterator<Item=Op> where Self : 'r;
    type ValueIter<'r> : Iterator<Item=Self::Subhandle> where Self : 'r;

    fn get_op(&self, o: OpAddr) -> Op;
    fn get_value<'r>(&'r self, value_id: ValueID) -> Option<Self::Subhandle>;

    fn iter_ready<'r>(&'r self) -> Self::ReadyIter<'r>;
    fn iter_ops<'r>(&'r self) -> Self::OpIter<'r>;
    fn iter_values<'r>(&'r self) -> Self::ValueIter<'r>;
}

pub trait IndirectBuilder<'s, H : Handle<'s>> {
    // Indirections (and only indirections!) allow handles before construction is complete
    fn handle(&self) -> H;
    fn build(self, dest: H) -> H;
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
