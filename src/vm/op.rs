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
pub type OpAddr = u32;
pub type ExternalID = u32;
pub type TargetID = u32;