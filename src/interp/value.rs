use crate::core::lang::{
    ArgType, Primitive, Cond, Symbol
};
use super::machine::Machine;
use std::collections::HashMap;
use std::fmt;

// A foreign func also includes a name for debugging purposes
#[derive(Clone)]
pub struct ForeignFunc(pub &'static str, pub usize, 
        pub fn(&mut Machine, Scope) -> Register);

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

// (+) function in code:
// ParamExPos 0 1 - extract positional argument from slot 0 onto slot 1
// ParamExPos 0 2 - extract positional argument onto slot 2
// ParamEmpty 0 - assert no parameters left to extract
// Exec 1
// Exec 2
// Plus 3 1 2
// Ret 3

// fn f(b, a) { b(a) } in code:
// ParamExPos 0 1 -- extract "b" to 1
// ParamExPos 0 2 -- extract "a" to 2
// ParamEmpty 0
// EmptyArg 3 - push an empty arguments array to 3
// PosArg 3 2 - push "a" onto the array at 3
// Exec 1 - execute b, will evaluate ato an "Entrypoint"
// Thunk 5 1 4 - create a thunk in 5 with 1 as the entrypoint, 4 as the scope
// Push 5 3 - set reg 0 of new scope to 3 (the arg array)
// Ret 4

// Handling recursive bindings:
// let a = (b, 1)
// let b = (a, 2)

// Code for a:
// EmptyTuple 1
// TupleAppend 1 0 // append arg 0
// Prim 2 (1)
// Tuple Append 1 2
// Ret 1

// Code for b:
// EmptyTuple 1
// TupleAppend 1 0 // append arg 0
// Prim 2 (2)
// Tuple Append 1 2
// Ret 1

// 

pub type RegAddr = usize;

#[derive(Debug)]
pub enum Register {
    Value(Value),
    Ptr(ValuePtr)
}

// dst src
#[derive(Clone, Copy, Eq, PartialEq, Hash, Debug)]
pub enum PrimitiveOp {
    Negate(RegAddr, RegAddr),
    Add(RegAddr, RegAddr, RegAddr), Sub(RegAddr, RegAddr, RegAddr), 
    Mul(RegAddr, RegAddr, RegAddr), Div(RegAddr, RegAddr, RegAddr), 
    Mod(RegAddr, RegAddr, RegAddr),
    Or(RegAddr, RegAddr, RegAddr), 
    And(RegAddr, RegAddr, RegAddr), 
    Xor(RegAddr, RegAddr, RegAddr)
}

// Generic
#[derive(Debug)]
pub struct Scope {
    reg: Vec<Register>, // the registers
}

#[derive(Debug)]
pub enum Op {
    Force(RegAddr), // register to eval
    Ret(RegAddr), // register to return

    // will copy the register itself
    Cp(RegAddr, RegAddr), // dest, src

    Reserve(RegAddr), // reserve an address on the heap
    UseReserve(RegAddr, RegAddr), // use a reservation

    // value constructors
    Prim(RegAddr, Primitive), // store primtiive into register
    PrimitiveOp(PrimitiveOp), // executes a primitive op

    ListEmpty(RegAddr), // store empty list into an address
    ListCons(RegAddr, RegAddr, RegAddr), // dest head src

    TupleEmpty(RegAddr), // store empty tuple into an address
    TuplePush(RegAddr, RegAddr),

    // to create a variant
    // first use Variant then call VariantOpt successively
    Variant(RegAddr, u16, RegAddr), // dest, tag, payload
    // add an additional option to a variant
    VariantOpt(RegAddr, String),

    RecordEmpty(RegAddr), // dest
    RecordInsert(RegAddr, RegAddr, RegAddr), // dest key value
    RecordDel(RegAddr, RegAddr), // dest key

    // for either entrypoints or thunks!
    // must be direct
    PosArg(RegAddr, RegAddr),
    PosVarArg(RegAddr, RegAddr),
    KeyArg(RegAddr, String, RegAddr),
    KeyVarArg(RegAddr, RegAddr),

    // takes a position argument, looks at the key args
    // if no position args are left
    ExPosArg(RegAddr),
    ExNamedArg(RegAddr, String),
    ExOptNamedArg(RegAddr, String),
    // extracts the remaining position args to a list
    ExPosVarArg(RegAddr), 
    // extracts the remaining key args
    // there cannot be any positional args remaining
    ExKeyVarArg(RegAddr), 

    // will be compiled into a regular entrypoint
    // used during compilation because we don't know
    // how big everything will be
    EntrypointSeg(RegAddr, SegmentId), // dest, segment id
    Entrypoint(RegAddr, CodePtr), // dest, address

    JmpSegIf(RegAddr, SegmentId), // register, target segment id
    JmpIf(RegAddr, CodePtr), // register, target address

    Thunk(RegAddr, RegAddr), // dest, entrypoint (must be direct)

    PushReg(RegAddr, RegAddr), // push reg onto entrypoint OR thunk

}

type CodePtr = usize;

#[derive(Debug)]
pub struct Code {
    ops: Vec<Op>,
}

impl Code {
    fn new(c: Vec<Op>) -> Code {
        Code { ops: c }
    }
}

#[derive(Debug)]
pub enum Value {
    Placeholder, // used for mutual recursion
    Primitive(Primitive),
    Code(Code),
    // ValuePtr to code (direct), offset into code, scopeptr (direct)
    Thunk(ValuePtr, CodePtr, Scope),
    // an entrypoint with pre-bound arguments/scope
    Entrypoint(ValuePtr, CodePtr, Scope)
}

// A node pointer is a heap and a location
// within that heap
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ValuePtr {
    loc: usize
}

impl fmt::Display for ValuePtr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "0x{:x}", self.loc)
    }
}

type SegmentId = usize;

pub struct SegmentBuilder {
    pub id: SegmentId,
    code: Code,
    next_reg: RegAddr // next free register for the scope in this segment
}

impl SegmentBuilder {
    pub fn append(&mut self, op: Op) {
        self.code.ops.push(op)
    }
    pub fn extend(&mut self, ops: Vec<Op>) {
        self.code.ops.extend(ops)
    }
    pub fn next_reg(&mut self) -> RegAddr {
        let r = self.next_reg;
        self.next_reg = self.next_reg + 1;
        r
    }
}

pub struct CodeBuilder {
    segs: Vec<Option<SegmentBuilder>> // the segments
}

impl CodeBuilder {
    pub fn next<'a>(&mut self) -> SegmentBuilder {
        let id = self.segs.len();
        self.segs.push(Option::None);
        SegmentBuilder {
            id,
            code: Code { ops: Vec::new() },
            next_reg: 0
        }
    }
    pub fn register(&mut self, seg: SegmentBuilder) {
        let id = seg.id;
        self.segs[id] = Some(seg)
    }

    pub fn build(self) -> Code {
        let mut ops = Vec::new();
        let mut segment_locs = Vec::new();
        for sb in self.segs {
            segment_locs.push(ops.len());
            match sb {
                None => panic!("Unregistered segment referenced!"),
                Some(s) => ops.extend(s.code.ops)
            }
        }
        // now we replace all of the segment jmp/entrypointseg
        Code { ops }
    }
}

pub struct Heap {
    nodes: Vec<Value>
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

    pub fn at_mut<'a>(&'a mut self, ptr: ValuePtr) -> &'a Value {
        &mut self.nodes[ptr.loc]
    }

    pub fn set(&mut self, ptr: ValuePtr, node: Value) {
        self.nodes[ptr.loc] = node;
    }

    pub fn add(&mut self, node: Value) -> ValuePtr {
        self.nodes.push(node);
        ValuePtr { loc: self.nodes.len() - 1 }
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