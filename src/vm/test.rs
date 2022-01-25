use crate::value::local::LocalStorage;
use crate::value::{UnpackHeap, ObjectRef, DataRef, ExtractValue, Numeric};
use crate::vm::tracer::ForceCache;
use futures_lite::future;
use smol::LocalExecutor;

// Get the test values
use crate::test_capnp::SIMPLE_ADD;

use super::machine::Machine;

#[test]
fn test_add() {
    let store = LocalStorage::new_default();
    let cache = ForceCache::new();
    let roots = SIMPLE_ADD.get().unwrap().unpack_into(&store).unwrap();

    let thunk = roots[1].clone();
    // the machine state has to outlive the executor
    let machine = Machine::new(&store, &cache);
    let exec = LocalExecutor::new();
    future::block_on(exec.run(async {
        machine.force(&thunk).await.unwrap();
        let val = thunk.get_value().unwrap();
        let v = val.reader().numeric().unwrap();
        assert_eq!(v, Numeric::Int(3))
    }));
}