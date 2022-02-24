use crate::Error;

pub trait Storage {
    type Handle<'s>: ObjectHandle<'s, Storage=Self> where Self : 's;
    type Ptr: ObjectPtr;

    // Associated reader types
    type StringReader<'s> : StringReader<'s> where Self : 's;
    type BufferReader<'s> : BufferReader<'s> where Self : 's;
    type TupleReader<'s> : TupleReader<'s> where Self : 's;
    type RecordReader<'s> : RecordReader<'s> where Self : 's;
    type CodeReader<'s> : CodeReader<'s> where Self : 's;
    type PartialReader<'s> : PartialReader<'s> where Self : 's;

    // Associated builder types
    type IndirectBuilder<'s> : IndirectBuilder<'s, Storage=Self> where Self : 's;

    type NumericBuilder<'s> : IndirectBuilder<'s, Storage=Self> where Self : 's;
    type CharBuilder<'s> : IndirectBuilder<'s, Storage=Self> where Self : 's;
    type BoolBuilder<'s> : IndirectBuilder<'s, Storage=Self> where Self : 's;
    type StringBuilder<'s> : StringBuilder<'s> where Self : 's;
    type BufferBuilder<'s> : StringBuilder<'s> where Self : 's;

    type TupleBuilder<'s>  : TupleBuilder<'s, Storage=Self> where Self : 's;
    type RecordBuilder<'s> : RecordBuilder<'s, Storage=Self> where Self : 's;
    type CodeBuilder<'s>   : CodeBuilder<'s, Storage=Self> where Self : 's;
    type PartialBuilder<'s>: PartialBuilder<'s, Storage=Self> where Self : 's;

    fn build<'s>(&'s self, spec: ObjectSpec) -> Result<ObjectBuilder<'s, Self>, Error>;
    fn get<'s>(&'s self, ptr: Self::Ptr) -> Result<Self::Handle<'s>, Error>;
}

pub trait ObjectHandle<'s> {
    type Storage : Storage;
    fn storage(&self) -> &'s Self::Storage;
    fn reader(&self) -> ObjectReader<'s, Self::Storage>;
}

use std::fmt::{Debug, Display};
pub trait ObjectPtr : Debug + Display {}


// Readers
pub enum ObjectReader<'s, S: Storage + 's> {
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

pub trait StringReader<'s> {

}

pub trait BufferReader<'s> {

}

pub trait TupleReader<'s> {

}

pub trait RecordReader<'s> {

}

pub trait PartialReader<'s> {

}

pub trait CodeReader<'s> {

}


// Builder

pub enum ObjectSpec {
    Indirect, Unit, Bot,
    Numeric, Bool,
    Char, String(usize),
    Buffer(usize),
    Nil, Cons,
    Tuple(usize), Record(usize),
    Variant,
    Code(usize)
}

pub enum ObjectBuilder<'s, S: Storage + 's + ?Sized> {
    Code(S::CodeBuilder<'s>)
}

pub trait IndirectBuilder<'s> {
    type Storage : Storage;

    // Indirections allow handles before construction
    // is complete
    fn handle(&self)
        -> <Self::Storage as Storage>::Handle<'s>;
    fn build(self, dest: <Self::Storage as Storage>::Ptr)
        -> <Self::Storage as Storage>::Handle<'s>;
}

pub trait NumericBuilder<'s> {
}

pub trait CharBuilder<'s> {
}

pub trait BoolBuilder<'s> {
}


pub trait StringBuilder<'s> {
    fn slice(&mut self, start: usize, len: usize) -> &mut str;
}

pub trait BufferBuilder<'s> {
    fn slice(&mut self, start: usize, len: usize) -> &mut u8;

}

pub trait TupleBuilder<'s> {
    type Storage : Storage;
}

pub trait RecordBuilder<'s> {
    type Storage : Storage;
}

pub trait CodeBuilder<'s> {
    type Storage : Storage;
}

pub trait PartialBuilder<'s> {
    type Storage : Storage;
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
