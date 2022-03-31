pub type RegID = u32;
pub type ValueID = u32;
pub type InputID = u32;
pub type OpAddr = u32;
pub type OpCount = u32;

use crate::Error;

#[derive(Clone, Copy, Debug)]
pub enum BuiltinOp {
    Add, Sub, Mul, Div, Neg,
    EmptyRecord, Insert, Project,
    EmptyTuple, Append,
    Nil, Cons, 
    JoinUrl, DecodeUtf8, EncodeUtf8,
    Compile, Fetch, Sys
}
impl<'a> TryFrom<&'a str> for BuiltinOp {
    type Error = Error;
    fn try_from(v: &'a str) -> Result<Self, Self::Error> {
        use BuiltinOp::*;
        Ok(match v {
        "add" => Add,
        "sub" => Sub,
        "mul" => Mul,
        "div" => Div,
        "neg" => Neg,
        "empty_record" => EmptyRecord,
        "insert" => Insert,
        "project" => Project,
        "empty_tuple" => EmptyTuple,
        "append" => Append,
        "nil" => Nil,
        "cons" => Cons,
        "compile" => Compile,
        "fetch" => Fetch,
        "join_url" => JoinUrl,
        "decode_utf8" => DecodeUtf8,
        "encode_utf8" => EncodeUtf8,
        "sys" => Sys,
        _ => return Err(Error::new(format!("Unrecognized op {}", v)))
        })
    }
}
impl Into<&'static str> for BuiltinOp {
    fn into(self) -> &'static str {
        use BuiltinOp::*;
        match self {
        Add => "add",
        Sub => "sub",
        Mul => "mul",
        Div => "div",
        Neg => "neg",
        EmptyRecord => "empty_record",
        Insert => "insert",
        Project => "project",
        EmptyTuple => "empty_tuple",
        Append => "append",
        Nil => "nil",
        Cons => "cons",
        Compile => "compile",
        Fetch => "fetch",
        JoinUrl => "join_url",
        DecodeUtf8 => "decode_utf8",
        EncodeUtf8 => "encode_utf8",
        Sys => "sys"
        }
    }
}

#[derive(Clone, Debug)]
pub struct Dest {
    pub reg: RegID,
    pub uses: Vec<OpAddr>
}

#[derive(Clone, Debug)]
pub enum OpCase {
    Tag(ValueID, RegID),
    Eq(ValueID, RegID),
    Default(RegID)
}

#[derive(Clone, Debug)]
pub enum Op {
    SetValue(Dest, ValueID),
    SetInput(Dest, InputID),
    Force(Dest, RegID), // dest = src
    Bind(Dest, RegID, Vec<RegID>),
    Invoke(Dest, RegID),
    Builtin(Dest, BuiltinOp, Vec<RegID>),
    Match(Dest, RegID, Vec<OpCase>)
}

impl Op {
    pub fn num_deps(&self) -> OpCount {
        use Op::*;
        match self {
            Force(_, _) => 1,
            Bind(_, _, v) => 1 + v.len() as OpCount,
            Invoke(_, _) => 1,
            Builtin(_, _, v) => v.len() as OpCount,
            Match(_, _, _) => 1,
            _ => 0
        }
    }
}