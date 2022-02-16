pub use crate::value::op as op;
pub mod machine;
pub mod scope;
pub mod builtin;
pub mod tracer;

#[cfg(test)]
mod test;

#[derive(Debug)]
pub struct ExecError {
    pub msg: &'static str
}

impl ExecError {
    fn new(msg: &'static str) -> Self {
        Self { msg }
    }
}

impl Default for ExecError {
    fn default() -> Self {
        Self { msg: "" }
    }
}

impl From<capnp::Error> for ExecError {
    fn from(_: capnp::Error) -> Self {
        Self::default()
    }
}
impl From<capnp::NotInSchema> for ExecError {
    fn from(_: capnp::NotInSchema) -> Self {
        Self::default()
    }
}

use crate::value::StorageError;
impl From<StorageError> for ExecError {
    fn from(_: StorageError) -> Self {
        Self::default()
    }
}