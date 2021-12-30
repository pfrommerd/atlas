
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

pub type RegAddr = u8;
pub type CodeHash = u64;
pub type OpAddr = u32;