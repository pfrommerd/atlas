use crate::Error;

pub mod op;
//pub mod direct;

// pub mod heap;
pub trait StorageTypes {
    type Handle<'s>: ObjectHandle<'s, Types=Self> where Self : 's;
    type Ptr: ObjectPtr;

    // Associated reader types
    type StringReader<'s> : StringReader<'s> where Self : 's;
    type BufferReader<'s> : BufferReader<'s> where Self : 's;
    type TupleReader<'s> : TupleReader<'s> where Self : 's;
    type RecordReader<'s> : RecordReader<'s> where Self : 's;
    type CodeReader<'s> : CodeReader<'s> where Self : 's;
    type PartialReader<'s> : PartialReader<'s> where Self : 's;
}

pub trait Storage {
    type Types : StorageTypes;
    // Indirect is special
    type IndirectBuilder<'s> : IndirectBuilder<'s, Types=Self> where Self : 's;
    // Indirect is special, since we need
    // to potentially modify the indirect after it is built
    fn build_indirect<'s>(&'s self) -> Result<Self::IndirectBuilder<'s>, Error>;

    // You should be able to build using any reader
    // which has the same pointer type
    fn insert<'s, S: StorageTypes<Ptr=<Self::Types as StorageTypes>::Ptr>>(&'s self, src: &ObjectReader<'s, S>)
        -> Result<<Self::Types as StorageTypes>::Handle<'s>, Error>;

    fn get<'s>(&'s self, ptr: <Self::Types as StorageTypes>::Ptr) 
        -> Result<<Self::Types as StorageTypes>::Handle<'s>, Error>;
}

pub trait ObjectHandle<'s> {
    type Types : StorageTypes;
    fn ptr(&self) -> <Self::Types as StorageTypes>::Ptr;
    fn reader(&self) -> Result<ObjectReader<'s, Self::Types>, Error>;
}

use std::fmt::{Debug, Display};
pub trait ObjectPtr : Debug + Display {}

// Readers
pub enum ObjectReader<'s, S: StorageTypes + 's + ?Sized> {
    Bot, Indirect(S::Handle<'s>),
    Unit,
    Numeric(Numeric), Bool(bool),
    Char(char), String(S::StringReader<'s>),
    Buffer(S::BufferReader<'s>),
    Record(S::RecordReader<'s>),
    Tuple(S::TupleReader<'s>),
    Variant(S::Handle<'s>, S::Handle<'s>),
    Cons(S::Handle<'s>, S::Handle<'s>), Nil,
    Thunk(S::Handle<'s>),
    Code(S::CodeReader<'s>),
    Partial(S::PartialReader<'s>),
}
use std::ops::Deref;

pub trait StringReader<'s> {
    type StringSlice<'r> : Deref<Target=str> where Self : 'r;
    fn slice<'r>(&'r self, start: usize, len: usize) -> Self::StringSlice<'r>;
}

pub trait BufferReader<'s> {
    type BufferSlice<'r> : Deref<Target=[u8]> where Self : 'r;
    fn slice<'r>(&'r self, start: usize, len: usize) -> Self::BufferSlice<'r>;
}

pub trait TupleReader<'s> {
    type Handle<'h> : ObjectHandle<'h>;
    fn len(&self) -> usize;
    fn get(&self, i: usize) -> Option<Self::Handle<'s>>;
}

pub trait RecordReader<'s> {
    type Handle<'h> : ObjectHandle<'h>;
    fn len(&self) -> usize;
    fn get(&self, i: usize) -> Option<(Self::Handle<'s>, Self::Handle<'s>)>;
}

pub trait PartialReader<'s> {
    type Handle<'h> : ObjectHandle<'h>;
    fn args(&self) -> usize;
    fn get_code(&self) -> Self::Handle<'s>;
    fn get_arg(&self, i: usize) -> Self::Handle<'s>;
}

pub trait CodeReader<'s> {

}

pub trait IndirectBuilder<'s> {
    type Types : StorageTypes;
    // Indirections (and only indirections!) allow handles before construction is complete
    fn handle(&self) -> <Self::Types as StorageTypes>::Handle<'s>;
    fn build(self, dest: <Self::Types as StorageTypes>::Ptr) -> <Self::Types as StorageTypes>::Handle<'s>;
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
