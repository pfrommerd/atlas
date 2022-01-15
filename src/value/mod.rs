pub mod storage;
pub mod mem;
pub mod local;
pub mod allocator;

pub use crate::value_capnp::value::{
    Reader as ValueReader,
    Builder as ValueBuilder,
    Which as ValueWhich
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
pub use storage::{
    ObjectStorage, ObjPointer, 
    DataStorage, DataPointer
};