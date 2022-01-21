pub use crate::op_capnp::op::{
    Which as OpWhich,
    Reader as OpReader,
    Builder as OpBuilder
};
pub use crate::op_capnp::op::force::{
    Reader as ForceReader,
    Builder as ForceBuilder
};
pub use crate::op_capnp::param::{
    Which as ParamWhich,
    Reader as ParamReader,
    Builder as ParamBuilder
};
pub use crate::op_capnp::code::{
    Reader as CodeReader,
    Builder as CodeBuilder
};
pub use crate::op_capnp::dest::{
    Reader as DestReader,
    Builder as DestBuilder
};

pub type ObjectID = u16;
pub type OpAddr = u16;
pub type ExternalID = u16;
pub type TargetID = u16;

pub trait Dependent {
    fn num_deps(&self) -> Result<usize, capnp::Error>;
}

impl<'s> Dependent for OpReader<'s> {
    fn num_deps(&self) -> Result<usize, capnp::Error> {
        use OpWhich::*;
        Ok(match self.which()? {
        Ret(_) => 1,
        TailRet(_) => 1,
        Force(_) => 1,
        RecForce(_) => 1,
        Builtin(r) => r.get_args()?.len() as usize,
        Closure(r) => r.get_entries()?.len() as usize,
        Apply(r) => r.get_args()?.len() as usize,
        Invoke(_) => 1
        })
    }
}