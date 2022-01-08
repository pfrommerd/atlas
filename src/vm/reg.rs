use crate::value::{
    Pointer,
};
use bytes::Bytes;
pub type RegAddr = u16;

pub enum Arg {
    Lifted, Pos, Key(Pointer),
    Optional(Pointer), VarPos, VarKey
}

pub struct Partial {
    lamda: Pointer,
    args: Option<Vec<Arg>>
}

pub struct Record {
    rec: Vec<(Pointer, Pointer)>
}

pub enum RegValue {
    Indirect(Pointer),

    Unit,
    Float(f64),
    Int(i64),
    Bool(bool),
    Char(char),
    String(String),
    Buffer(Bytes),
    // an external type, annotated with
    // a identifier string and a blob
    External(&'static str, Bytes),

    Partial(Partial),
    Thunk(Partial),

    // head, tail
    Nil,
    Cons(Pointer, Pointer),
    Record(Record),
    Tuple(Vec<Pointer>),
    Variant(Pointer, Pointer)
}