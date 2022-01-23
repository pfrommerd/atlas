use super::local::LocalStorage;
use super::UnpackHeap;

use crate::test_capnp::SIMPLE_ADD;

#[test]
fn test_unpack() {
    let store = LocalStorage::new_default();
    SIMPLE_ADD.get().unwrap().unpack_into(&store).unwrap();
}