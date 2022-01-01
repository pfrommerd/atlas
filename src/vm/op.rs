
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
use crate::core::lang::{
    PrimitiveReader
};

use super::arena::Pointer;

pub type RegAddr = u16;
pub type CodeHash = u64;
pub type OpAddr = u32;

pub enum Target {
    Addr(OpAddr),
    // external targets map into the external array
    External(u32), 
    // internal targets are for use during optimization
    // they are replaced by addr targets in the final pass
    Ingternal(u32) 
}

pub enum PrimitiveOp {
    Negate { dest: RegAddr, src: RegAddr },
    Add { dest: RegAddr, src: RegAddr, arg: RegAddr },
    Mul { dest: RegAddr, src: RegAddr, arg: RegAddr },
    Mod { dest: RegAddr, src: RegAddr, arg: RegAddr },
    Or { dest: RegAddr, src: RegAddr, arg: RegAddr },
    And { dest: RegAddr, src: RegAddr, arg: RegAddr }
}


// Parameter ops are used for unpacking parameters
// arg ops are used for binding arguments

// the dest registers are optional
// so that you can unpack a parameter
// but drop it and not use a register
pub enum ParamOp<'s> {
    Pos(Option<RegAddr>), // a purely positional argument (for infix ops only)
    Named(Option<RegAddr>, &'s str), // a named, positional argument
    Optional(Option<RegAddr>, &'s str), // a named, optional argument
    VarPos(Option<RegAddr>),
    VarKey(Option<RegAddr>),
    Done // drops the remaining parameters
}

// of the form entrypoint register, source register
pub enum ArgOp<'s> {
    Pos(RegAddr, RegAddr),
    Keyword(RegAddr, RegAddr, &'s str), 
    VarPos(RegAddr, RegAddr),
    VarKey(RegAddr, RegAddr)
}

// A rusty op representation
// which can be serialized/deserialized
// to the capnproto format.
// Note that the Op has a lifetime, which
// it uses to reference strings in the serialized format
pub enum Op<'s> {
    Compute(PrimitiveOp),
    Unpack(ParamOp<'s>),
    Apply(ArgOp<'s>),
    Store(RegAddr, PrimitiveReader<'s>),
    Entrypoint(RegAddr, Target),
    Thunk(RegAddr, RegAddr)
}

// A temporary structure for interacting with a code segment
// core expressions are transpiled into segments, which are then converted
// into the Code values
pub struct Segment<'s> {
    ops: Vec<Op<'s>>,
    targets: Vec<Pointer> // the external targets
}

impl<'s> Segment<'s> {
    pub fn new() -> Self {
        Segment { ops: Vec::new(), targets: Vec::new() }
    }

    pub fn append(&mut self, op: Op<'s>) {
        self.ops.push(op);
    }
}