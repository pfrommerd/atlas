pub use crate::op_capnp::op::{
    Which as OpWhich,
    Reader as OpReader,
    Builder as OpBuilder
};
pub use crate::op_capnp::op::force::{
    Reader as ForceReader,
    Builder as ForceBuilder
};
pub use crate::op_capnp::code::{
    Reader as CodeReader,
    Builder as CodeBuilder
};
pub use crate::op_capnp::op::match_::{
    Reader as MatchReader,
    Builder as MatchBuilder
};
pub use crate::op_capnp::dest::{
    Reader as DestReader,
    Builder as DestBuilder
};

pub type ObjectID = u32;
pub type OpAddr = u32;
pub type OpCount = u32;

pub trait Dependent {
    fn num_deps(&self) -> Result<OpCount, capnp::Error>;
}

impl<'s> Dependent for OpReader<'s> {
    fn num_deps(&self) -> Result<OpCount, capnp::Error> {
        use OpWhich::*;
        Ok(match self.which()? {
        Ret(_) => 1,
        ForceRet(_) => 1,
        Force(_) => 1,
        RecForce(_) => 1,
        Bind(r) => r.get_args()?.len() as u32 + 1,
        Invoke(_) => 1,
        Builtin(r) => r.get_args()?.len() as u32,
        Match(_) => 1,
        Select(r) => r.get_branches()?.len() as u32 + 1, 
        })
    }
}