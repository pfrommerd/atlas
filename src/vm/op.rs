pub use crate::op_capnp::op::{
    Which as OpWhich,
    Reader as OpReader,
    Builder as OpBuilder
};
pub use crate::op_capnp::code::{
    Reader as CodeReader,
    Builder as CodeBuilder
};

pub type ValueID = u16;
pub type OpAddr = u16;
pub type ExternalID = u16;
pub type TargetID = u16;

trait Dependencies {
    fn num_deps(&self) -> Result<usize, capnp::Error>;
}


impl<'s> Dependencies for OpReader<'s> {
    fn num_deps(&self) -> Result<usize, capnp::Error> {
        use OpWhich::*;
        match self.which()? {
        Force(r) => r,
        _ => panic!("Unimplemented")
        }
    }
}
