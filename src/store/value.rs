use bytes::Bytes;
use std::ops::Deref;

use std::marker::PhantomData;

use super::{Handle, ObjectReader, ReaderWhich, ObjectType,
    StringReader, BufferReader, TupleReader,
    RecordReader, PartialReader, CodeReader};

pub enum Value<'s, H : Handle<'s>> {
    Indirect(H),
    Unit,
    Bot,
    Char(char),
    Bool(bool),
    Float(f64), Int(i64),
    String(String),
    Buffer(Bytes),
    Nil, Cons(H, H),
    Tuple(Vec<H>),
    Record(Vec<(H, H)>),
    Variant(H, H),
    Code(Code<'s, H>),
    Partial(H, Vec<H>),
    Thunk(H),
}

pub struct Code<'s, H: Handle<'s>> {
    ret: OpAddr,
    ready: Vec<OpAddr>,
    ops: Vec<Op>,
    values: Vec<H>,
    phantom: PhantomData<&'s ()>
}

impl<'s, H: Handle<'s>> Code<'s, H> {
    pub fn reader<'p>(&'p self) -> CodeValueReader<'p, 's, H> {
        CodeValueReader { code: self }
    }
}

impl<'p, 's, H: Handle<'s>> ObjectReader<'p, 's> for &'p Value<'s, H> {
    type Handle = H;
    type StringReader = StringValueReader<'p>;
    type BufferReader = BufferValueReader<'p>;
    type TupleReader = TupleValueReader<'p, 's, H>;
    type RecordReader = RecordValueReader<'p, 's, H>;
    type CodeReader = CodeValueReader<'p, 's, H>;
    type PartialReader = PartialValueReader<'p, 's, H>;

    type Subhandle = &'p H;

    fn get_type(&self) -> ObjectType {
        use ObjectType::*;
        match self { 
            Value::Bot => Bot, Value::Indirect(_) => Indirect,
            Value::Unit => Unit, Value::Char(_) => Char, Value::Bool(_) => Bool,
            Value::Int(_) => Int, Value::Float(_) => Float,
            Value::String(_) => String, Value::Buffer(_) => Buffer, 
            Value::Record(_) => Record, Value::Tuple(_) => Tuple,
            Value::Variant(_, _) => Variant, Value::Cons(_, _) => Cons, Value::Nil => Nil,
            Value::Thunk(_) => Thunk, Value::Code(_) => Code, Value::Partial(_, _) => Partial
        }
    }
    fn which(&self) -> ReaderWhich<Self::Subhandle,
            Self::StringReader, Self::BufferReader,
            Self::TupleReader, Self::RecordReader,
            Self::CodeReader, Self::PartialReader> {
        use ReaderWhich::*;
        match self {
            Value::Bot => Bot, Value::Indirect(h) => Indirect(h),
            Value::Unit => Unit, Value::Char(c) => Char(*c), Value::Bool(b) => Bool(*b),
            Value::Int(i) => Int(*i), Value::Float(f) => Float(*f),
            Value::String(b) => String(StringValueReader{ s: b.deref() }),
            Value::Buffer(b) => Buffer(BufferValueReader{ s: b.deref() }),
            Value::Record(record) => Record(RecordValueReader { record, phantom: PhantomData }),
            Value::Tuple(tuple) => Tuple(TupleValueReader { tuple, phantom: PhantomData }),
            Value::Variant(t, v) => Variant(t, v),
            Value::Cons(h, t) => Cons(h, t),
            Value::Nil => Nil,
            Value::Thunk(p) => Thunk(p),
            Value::Code(code) => Code(CodeValueReader { code }),
            Value::Partial(code, args) => Partial(PartialValueReader { code, args, phantom: PhantomData })
        }
    }
}

use super::op::{OpAddr, Op, ValueID};


use std::borrow::Borrow;

pub struct StringValueReader<'p> {
    s: &'p str,
}

impl<'p> StringReader<'p> for StringValueReader<'p> {
    type StringSlice<'sl> where Self : 'sl = &'sl str;

    fn slice<'sl>(&'sl self, start: usize, len: usize) -> &'sl str {
        &self.s[start..start+len]
    }
    fn len(&self) -> usize { self.s.len() }
}

pub struct BufferValueReader<'p> {
    s: &'p [u8],
}

impl<'p> BufferReader<'p> for BufferValueReader<'p> {
    type BufferSlice<'sl> where Self : 'sl = &'sl [u8];

    fn slice<'sl>(&'sl self, start: usize, len: usize) -> &'sl [u8] {
        &self.s.borrow()[start..start+len]
    }
    fn len(&self) -> usize { self.s.len() }
}

pub struct TupleValueReader<'p, 's, H: Handle<'s>> {
    tuple: &'p Vec<H>,
    phantom : PhantomData<&'s ()>
}

impl<'p,'s, H: Handle<'s>> TupleReader<'p, 's> for TupleValueReader<'p, 's, H> {
    type Handle = H;
    type Subhandle = H;

    type EntryIter<'r> where Self: 'r = 
        std::iter::Cloned<std::slice::Iter<'r, Self::Subhandle>>;

    fn iter<'r>(&'r self) -> Self::EntryIter<'r> {
        self.tuple.iter().cloned()
    }

    fn len(&self) -> usize {
        self.tuple.len()
    }
    fn get(&self, i: usize) -> Option<Self::Subhandle> {
        self.tuple.get(i).cloned()
    }
}

pub struct RecordValueReader<'p, 's, H : Handle<'s>> {
    record: &'p Vec<(H, H)>,
    phantom: PhantomData<&'s ()>
}

impl<'p, 's, H : Handle<'s>> RecordReader<'p, 's> for RecordValueReader<'p, 's, H> {
    type Handle = H;
    type Subhandle = H;

    type EntryIter<'r> where Self: 'r = 
        std::iter::Cloned<std::slice::Iter<'r, (Self::Subhandle, Self::Subhandle)>>;

    fn iter<'r>(&'r self) -> Self::EntryIter<'r> {
        self.record.iter().cloned()
    }

    fn len(&self) -> usize {
        self.record.len()
    }
    fn get(&self, i: usize) -> Option<(Self::Subhandle, Self::Subhandle)> {
        self.record.get(i).cloned()
    }
}

pub struct PartialValueReader<'p, 's, H : Handle<'s>> {
    code: &'p H,
    args: &'p Vec<H>,
    phantom: PhantomData<&'s ()>
}

impl<'p, 's, H: Handle<'s>> PartialReader<'p, 's> for PartialValueReader<'p, 's, H> {
    type Handle = H;
    type Subhandle = H;
    type ArgsIter<'r> where Self: 'r =
        std::iter::Cloned<std::slice::Iter<'r, Self::Subhandle>>;

    fn num_args(&self) -> usize {
        self.args.len()
    }
    fn get_code(&self) -> Self::Subhandle {
        self.code.clone()
    }
    fn get_arg(&self, i: usize) -> Option<Self::Subhandle> {
        self.args.get(i).cloned()
    }
    fn iter_args<'r>(&'r self) -> Self::ArgsIter<'r> {
        self.args.iter().cloned()
    }
}

pub struct CodeValueReader<'p, 's, H: Handle<'s>> {
    code: &'p Code<'s, H>
}


impl<'p, 's, H: Handle<'s>> CodeReader<'p, 's> for CodeValueReader<'p, 's, H> {
    type Handle = H;
    type Subhandle = H;

    type ReadyIter<'h> where Self: 'h = std::iter::Cloned<std::slice::Iter<'h, OpAddr>>;
    type OpIter<'h> where Self: 'h = std::iter::Cloned<std::slice::Iter<'h, Op>>;
    type ValueIter<'h> where Self: 'h = 
        std::iter::Cloned<std::slice::Iter<'h, Self::Subhandle>>;

    fn iter_ready<'h>(&'h self) -> Self::ReadyIter<'h> {
        self.code.ready.iter().cloned()
    }
    fn iter_ops<'h>(&'h self) -> Self::OpIter<'h> {
        self.code.ops.iter().cloned()
    }
    fn iter_values<'h>(&'h self) -> Self::ValueIter<'h> {
        self.code.values.iter().cloned()
    }
    fn get_op(&self, a: OpAddr) -> Op {
        self.code.ops[a as usize].clone()
    }
    fn get_ret(&self) -> OpAddr {
        self.code.ret
    }

    fn get_value<'h>(&'h self, value_id: ValueID) -> Option<Self::Subhandle> {
        self.code.values.get(value_id as usize).cloned()
    }
}