pub mod op;
pub mod machine;
pub mod compile;
pub mod scope;

#[derive(Debug)]
pub struct ExecError {}

impl From<capnp::Error> for ExecError {
    fn from(_: capnp::Error) -> Self {
        Self {}
    }
}
impl From<capnp::NotInSchema> for ExecError {
    fn from(_: capnp::NotInSchema) -> Self {
        Self {}
    }
}

use crate::value::storage::StorageError;
impl From<StorageError> for ExecError {
    fn from(_: StorageError) -> Self {
        Self {}
    }
}