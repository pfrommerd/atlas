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
use crate::vm::ExecError;

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

#[derive(Clone, Copy)]
pub enum Numeric {
    Int(i64),
    Float(f64)
}

impl Numeric {
    pub fn op(l: Numeric, r: Numeric, iop : fn(i64, i64) -> i64, fop : fn(f64, f64) -> f64) -> Numeric {
        match (l, r) {
            (Numeric::Int(l), Numeric::Int(r)) => Numeric::Int(iop(l, r)),
            (Numeric::Int(l), Numeric::Float(r)) => Numeric::Float(fop(l as f64, r)),
            (Numeric::Float(l), Numeric::Int(r)) => Numeric::Float(fop(l,r as f64)),
            (Numeric::Float(l), Numeric::Float(r)) => Numeric::Float(fop(l,r))
        }
    }
    pub fn set(self, mut builder: PrimitiveBuilder<'_>) {
        match self {
            Self::Int(i) => builder.set_int(i),
            Self::Float(f) => builder.set_float(f)
        }
    }
}

pub trait ExtractValue<'s> {
    fn thunk(&self) -> Option<ObjPointer>;
    fn code(&self) -> Option<CodeReader<'s>>;
    fn numeric(&self) -> Result<Numeric, ExecError>;
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

    fn numeric(&self) -> Result<Numeric, ExecError> {
        match self.which()? {
            ValueWhich::Primitive(p) => {
                match p?.which()? {
                    PrimitiveWhich::Float(f) => {
                        Ok(Numeric::Float(f))
                    },
                    PrimitiveWhich::Int(i) => {
                        Ok(Numeric::Int(i))
                    },
                    _ => Err(ExecError {})
                }
            }
            _ => Err(ExecError {})
        }
    }
}