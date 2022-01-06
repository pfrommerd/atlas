use std::collections::HashMap;

pub use crate::op_capnp::op::{
    Which as OpWhich,
    Reader as OpReader,
    Builder as OpBuilder
};
pub use crate::op_capnp::primitive_op::{
    OpType as PrimOpWType,
    Reader as PrimOpReader,
    Builder as PrimOpBuilder
};
pub use crate::op_capnp::code::{
    Reader as CodeReader,
    Builder as CodeBuilder
};

use super::arena::Pointer;

pub type RegAddr = u32;
pub type CodeHash = u64;
pub type OpAddr = u32;
pub type TargetID = u32;
pub type ExternalID = u32;
pub type SegmentID = usize;

// an op arg can directly be a literal
pub enum OpPrimitive {
    Unit, Bool(bool), Int(i64),
    Float(f64), Char(char),
    Data(ExternalID), // external IDs map to external data structure constants
                      // this can include strings, buffers, lists, tuples, records, etc.
    EmptyList, EmptyTuple, EmptyRecord
}

pub enum BuiltinOp {
    // These all require the arguments to be ForceRec'd
    Negate { dest: RegAddr, src: RegAddr },
    Add { dest: RegAddr, left: RegAddr, right: RegAddr },
    Mul { dest: RegAddr, left: RegAddr, right: RegAddr },
    Div { dest: RegAddr, left: RegAddr, right: RegAddr },
    Mod { dest: RegAddr, left: RegAddr, right: RegAddr },
    Or  { dest: RegAddr, left: RegAddr, right: RegAddr },
    And { dest: RegAddr, left: RegAddr, right: RegAddr },

    // equality, comparison operators
    Eq {dest: RegAddr, left: RegAddr, right: RegAddr },
    Lt {dest: RegAddr, left: RegAddr, right: RegAddr },
    Gt {dest: RegAddr, left: RegAddr, right: RegAddr },
    Leq {dest: RegAddr, left: RegAddr, right: RegAddr },
    Geq {dest: RegAddr, left: RegAddr, right: RegAddr },

    Type { dest: RegAddr, src: RegAddr },

    // works on both lists and tuples
    Len { dest: RegAddr, src: RegAddr },
    // List methods
    Decons { head_dest: RegAddr, tail_dest: RegAddr, src: RegAddr },
    Cons { dest: RegAddr, head: RegAddr, tail: RegAddr },
    IsCons { dest: RegAddr, head: RegAddr },

    // Tuple methods
    Index { dest: RegAddr, src: RegAddr, index: RegAddr },
    Append { dest: RegAddr, src: RegAddr, item: RegAddr },

    // Variant methods
    Variant { dest: RegAddr, tag: RegAddr, value: RegAddr },
    HasTag { dest: RegAddr, tag: RegAddr, src: RegAddr },
    Extract { dest: RegAddr, src: RegAddr }, // unwrap a variant

    // Record methods
    Insert { dest: RegAddr, record: RegAddr, key: RegAddr, value: RegAddr },
    Has { dest: RegAddr, src: RegAddr, key: RegAddr },
    Lookup { dest: RegAddr, src: RegAddr, key: RegAddr },
}

pub enum UnpackOp {
    Pos(RegAddr),
    Named(RegAddr, RegAddr), // second register is the name
    Optional(RegAddr, RegAddr), // second register is the name
    VarPos(RegAddr),
    VarKey(RegAddr),
    Drop // drops the remaining args
}

pub enum ApplyOp {
    Pos { dest: RegAddr, tgt: RegAddr, arg: RegAddr },
    ByName { dest: RegAddr, tgt: RegAddr, arg: RegAddr, name: RegAddr }, 
    VarPos { dest: RegAddr, tgt: RegAddr, arg: RegAddr },
    VarKey { dest: RegAddr, tgt: RegAddr, arg: RegAddr }
}

// A rusty op representation
// which can be serialized/deserialized
// to the capnproto format.
// Note that the Op has a lifetime, which
// it uses to reference strings in the serialized format
pub enum Op {
    BuiltinOp(BuiltinOp),
    Unpack(UnpackOp),
    Store(RegAddr, OpPrimitive), // dest, src
    Entrypoint(RegAddr, TargetID),
    Apply(ApplyOp),
    Invoke(RegAddr, RegAddr),
    ScopeSet(RegAddr, RegAddr, RegAddr), // thunk/lambda dest, reg, src
    Force(RegAddr), // forces to WHNF
    // For case/if-else
    JmpIf(RegAddr, TargetID),
    Return(RegAddr)
}

// A temporary structure for interacting with a code segment
// core expressions are transpiled into segments, which are then converted
// into the Code values

pub struct Segment {
    ops: Vec<Op>,
    targets: Vec<SegmentID>, // other segment targets
    externals: Vec<Pointer> // external data pointers
}

impl Segment {
    pub fn new() -> Self {
        Segment {
            ops: Vec::new(),
            targets: Vec::new(),
            externals: Vec::new()
        }
    }

    pub fn add_target(&mut self, seg: SegmentID) -> TargetID {
        let id = self.targets.len() as TargetID;
        self.targets.push(seg);
        id
    }

    pub fn append(&mut self, op: Op) {
        self.ops.push(op);
    }
}

pub struct Program {
    segments: HashMap<SegmentID, Segment>,
    // external: HashMap<SegmentID, Pointer>,
    next_id: SegmentID
}

impl Program {
    pub fn register_seg(&mut self, id: SegmentID, seg: Segment) {
        self.segments.insert(id, seg);
    }

    pub fn gen_id(&mut self) -> SegmentID {
        let id = self.next_id;
        self.next_id = self.next_id + 1;
        id
    }
}

// When loading a program for modification
// the load context keeps track of the pointer
// to segment ID map
// pub struct LoadContext {
//     reverse: HashMap<Pointer, SegmentID>
// }


// Wrapping and unwrapping Ops from the underlying program
impl<'e> From<OpReader<'e>> for Op {
    fn from(op: OpReader<'e>) -> Self {
        match op.which().unwrap() {
        _ => panic!("Unimplemented!")
        }
    }
}