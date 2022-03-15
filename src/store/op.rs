pub type RegID = u32;
pub type ValueID = u32;
pub type InputID = u32;
pub type OpAddr = u32;
pub type OpCount = u32;

use crate::Error;

#[derive(Clone, Copy, Debug)]
pub enum BuiltinOp {
    Add, Mul, Div, Exec, Read
}
impl<'a> TryFrom<&'a str> for BuiltinOp {
    type Error = Error;
    fn try_from(v: &'a str) -> Result<Self, Self::Error> {
        use BuiltinOp::*;
        Ok(match v {
        "add" => Add,
        "mul" => Mul,
        "div" => Div,
        "exec" => Exec,
        "read" => Read,
        _ => return Err(Error::new(format!("Unrecognized op {}", v)))
        })
    }
}
impl Into<&'static str> for BuiltinOp {
    fn into(self) -> &'static str {
        use BuiltinOp::*;
        match self {
        Add => "add",
        Mul => "mul",
        Div => "div",
        Exec => "exec",
        Read => "read"
        }
    }
}

#[derive(Clone, Debug)]
pub struct Dest {
    pub reg: RegID,
    pub uses: Vec<OpAddr>
}

#[derive(Clone, Debug)]
pub enum Op {
    SetValue(Dest, ValueID),
    SetInput(Dest, InputID),
    Force(Dest, RegID), // dest = src
    Bind(Dest, RegID, Vec<RegID>),
    Invoke(Dest, RegID),
    Builtin(Dest, BuiltinOp, Vec<RegID>)
}

impl Op {
    pub fn num_deps(&self) -> OpCount {
        0
    }
}