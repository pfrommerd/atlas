pub use crate::op_capnp::op::{
    Which as OpWhich,
    Reader as OpReader,
    Builder as OpBuilder
};
pub use crate::op_capnp::code::{
    Reader as CodeReader,
    Builder as CodeBuilder
};

pub type RegAddr = u16;

pub type CodeHash = u64;
pub type OpAddr = u32;
pub type TargetID = u32;
pub type ExternalID = u32;
pub type SegmentID = usize;

#[derive(Clone)]
pub struct OpArg {
    addr: RegAddr,
    consume: bool // if we should move out of this register
}

#[derive(Clone)]
pub enum BuiltinOp {
    // These all require the arguments to be ForceRec'd
    Negate { dest: RegAddr, src: OpArg },
    Add { dest: RegAddr, left: OpArg, right: OpArg },
    Mul { dest: RegAddr, left: OpArg, right: OpArg },
    Div { dest: RegAddr, left: OpArg, right: OpArg },
    Mod { dest: RegAddr, left: OpArg, right: OpArg },
    Or  { dest: RegAddr, left: OpArg, right: OpArg },
    And { dest: RegAddr, left: OpArg, right: OpArg },

    // equality, comparison operators
    Eq {dest: RegAddr, left: OpArg, right: OpArg },
    Lt {dest: RegAddr, left: OpArg, right: OpArg },
    Gt {dest: RegAddr, left: OpArg, right: OpArg },
    Leq {dest: RegAddr, left: OpArg, right: OpArg },
    Geq {dest: RegAddr, left: OpArg, right: OpArg },

    Type { dest: RegAddr, src: OpArg },

    // works on both lists and tuples
    Len { dest: RegAddr, src: OpArg },
    // List methods
    Decons { head_dest: RegAddr, tail_dest: RegAddr, src: OpArg },
    Cons { dest: RegAddr, head: OpArg, tail: OpArg},
    IsCons { dest: RegAddr, head: OpArg },

    // Tuple methods
    Index { dest: RegAddr, src: OpArg, index: OpArg },
    Append { dest: RegAddr, src: OpArg, item: OpArg },

    // Variant methods
    Variant { dest: RegAddr, tag: OpArg, value: OpArg },
    HasTag { dest: RegAddr, tag: OpArg, src: OpArg },
    Extract { dest: RegAddr, src: OpArg }, // unwrap a variant

    // Record methods
    Insert { dest: RegAddr, record: OpArg, key: OpArg, value: OpArg },
    Has { dest: RegAddr, src: OpArg, key: OpArg },
    Lookup { dest: RegAddr, src: OpArg, key: OpArg },

    // trap into rust with a string trap name as argument
    // we use names rather than ints for forward/backward compatiblity
    // of bytecode.
    Trap { dest: RegAddr, trap_name: OpArg }
}


#[derive(Clone)]
pub enum ApplyOp {
    Pos { dest: RegAddr, tgt: RegAddr, arg: RegAddr },
    Key { dest: RegAddr, tgt: RegAddr, arg: RegAddr, name: RegAddr }, 
    VarPos { dest: RegAddr, tgt: RegAddr, arg: RegAddr },
    VarKey { dest: RegAddr, tgt: RegAddr, arg: RegAddr }
}

// A rusty op representation
// which can be serialized/deserialized
// to the capnproto format.
// Note that the Op has a lifetime, which
// it uses to reference strings in the serialized format
#[derive(Clone)]
pub enum Op {
    BuiltinOp(BuiltinOp),
    Store(RegAddr, ExternalID), // dest, src
    Func(RegAddr, TargetID),
    Apply(ApplyOp),
    Invoke(RegAddr, RegAddr),
    ScopeSet(RegAddr, RegAddr, RegAddr), // thunk/lambda dest, reg, src
    Force(RegAddr), // forces to WHNF
    // For case/if-else
    JmpIf(RegAddr, TargetID),
    Return(RegAddr)
}