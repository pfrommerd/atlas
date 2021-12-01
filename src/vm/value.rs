use super::machine::Machine;
use super::op::{Code, OpPtr, RegAddr};
use crate::core::lang::Primitive;

use std::fmt;
use std::rc::Rc;

// A foreign func also includes a name for debugging purposes
#[derive(Clone)]
pub struct ForeignFunc(
    pub &'static str,
    pub usize,
    pub fn(&mut Machine, Scope) -> Register,
);

impl fmt::Debug for ForeignFunc {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "<{}>", self.0)
    }
}

impl fmt::Display for ForeignFunc {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "<{}>", self.0)
    }
}

#[derive(Debug, Clone)]
pub enum Register {
    Empty,
    Value(Value),
    Ptr(ValuePtr),
}

#[derive(Debug, Clone)]
pub struct Scope {
    reg: Vec<Register>, // the registers
}

// returned when we try to get the value
const EMPTY_REG_VALUE: Value = Value::Placeholder;

impl Scope {
    pub fn new() -> Self {
        Scope { reg: Vec::new() }
    }
    pub fn take_reg(&mut self, r: RegAddr) -> Register {
        let x = &mut self.reg[r];
        std::mem::replace(x, Register::Empty)
    }

    pub fn get_heaped<'p>(&'p self, heap: &'p Heap, r: RegAddr) -> &'p Value {
        match &self.reg[r] {
            Register::Value(v) => v,
            Register::Ptr(p) => heap.at(*p),
            Register::Empty => &EMPTY_REG_VALUE,
        }
    }

    pub fn set(&mut self, r: RegAddr, reg: Register) {
        while r >= self.reg.len() {
            self.reg.push(Register::Empty)
        }
        self.reg[r] = reg;
    }

    pub fn set_value(&mut self, r: RegAddr, reg: Value) {
        self.reg[r] = Register::Value(reg);
    }
}

#[derive(Debug, Clone)]
pub enum Value {
    Placeholder, // used for mutual recursion
    Thunk(Rc<Code>, OpPtr, Scope),

    Primitive(Primitive),
    Code(Rc<Code>),
    Entrypoint(Rc<Code>, OpPtr, Scope),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ValuePtr {
    loc: usize,
}

impl fmt::Display for ValuePtr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "0x{:x}", self.loc)
    }
}

pub struct Heap {
    nodes: Vec<Value>,
}

impl Heap {
    pub fn new() -> Self {
        Heap { nodes: Vec::new() }
    }

    pub fn ptr(&self, loc: usize) -> ValuePtr {
        ValuePtr { loc: loc }
    }

    pub fn at<'a>(&'a self, ptr: ValuePtr) -> &'a Value {
        &self.nodes[ptr.loc]
    }

    pub fn set(&mut self, ptr: ValuePtr, node: Value) {
        self.nodes[ptr.loc] = node;
    }

    pub fn set_or_copy(&mut self, ptr: ValuePtr, reg: Register) {
        match reg {
            Register::Ptr(p) => self.nodes[ptr.loc] = self.nodes[p.loc].clone(),
            Register::Value(v) => self.nodes[ptr.loc] = v,
            Register::Empty => panic!("Tried to copy empty register"),
        }
    }

    pub fn add(&mut self, node: Value) -> ValuePtr {
        self.nodes.push(node);
        ValuePtr {
            loc: self.nodes.len() - 1,
        }
    }
}

impl fmt::Display for Heap {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        for (i, n) in self.nodes.iter().enumerate() {
            let ptr = self.ptr(i);
            writeln!(f, "{}: {:?}", ptr, n)?;
        }
        Ok(())
    }
}
