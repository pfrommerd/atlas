pub mod storage;
pub mod mem;
pub mod local;
pub mod allocator;

pub use crate::value_capnp::value::{
    Reader as ValueReader,
    Builder as ValueBuilder,
    Which as ValueWhich
};
pub use crate::value_capnp::arg_value::{
    Which as ArgValueWhich
};

use capnp::message::TypedReader;
use capnp::serialize::SliceSegments;
pub type ValueRootReader<'r> = TypedReader<SliceSegments<'r>, crate::value_capnp::value::Owned>;

pub use crate::value_capnp::primitive::{
    Which as PrimitiveWhich,
    Builder as PrimitiveBuilder,
    Reader as PrimitiveReader
};
pub use crate::op_capnp::param::{
    Which as ParamWhich,
    Reader as ParamReader,
    Builder as ParamBuilder
};
pub use crate::op_capnp::code::{
    Reader as CodeReader
};
pub use storage::{
    Storage, ObjPointer, ObjectRef, DataRef
};

pub trait ExtractValue<'s> {
    fn thunk(&self) -> Option<ObjPointer>;
    fn code(&self) -> Option<CodeReader<'s>>;
}

impl<'s> ExtractValue<'s> for ValueReader<'s> {
    fn thunk(&self) -> Option<ObjPointer> {
        match self.which().ok()? {
            ValueWhich::Thunk(t) => Some(ObjPointer::from(t)),
            _ => None
        }
    }
    fn code(&self) -> Option<CodeReader<'s>> {
        match self.which().ok()? {
            ValueWhich::Code(r) => r.ok(),
            _ => None
        }
    }
}