// use crate::store::ObjHandle;

// use super::mem::MemoryStorage;
// use super::Numeric;

use super::heap::HeapStorage;
use super::value::Value;
use super::{Storage, Handle, ObjectReader, ReaderWhich};

#[test]
fn test_store_numeric() {
    // Test store + retrieve int
    let storage = HeapStorage::new();
    let handle = storage.insert_from(&Value::Int(50)).unwrap();
    // Test store + retreive float on the same storage
    let result = handle.reader().unwrap().which();
    match result {
        ReaderWhich::Int(x) => assert_eq!(x, 50),
        _ => panic!("Expected integer")
    }
    let handle = storage.insert_from(&Value::Float(20.)).unwrap();
    // Test store + retreive float on the same storage
    let result = handle.reader().unwrap().which();
    match result {
        ReaderWhich::Float(x) => assert_eq!(x, 20.),
        _ => panic!("Expected float")
    }
}

// #[test]
// fn test_store_record() {
//     // Test store + retrieve int
//     let alloc = MemoryStorage::new();
//     let empty_record = OwnedValue::Record(Vec::new());
//     let empty_read = empty_record.pack_new(&alloc).unwrap().as_record().unwrap();
//     assert!(empty_read.is_empty());

//     let handle_a = unsafe { ObjHandle::new(&alloc, 0) };
//     let handle_b = unsafe { ObjHandle::new(&alloc, 1) };
//     let full_record = vec![(handle_a.clone(), handle_b.clone()), (handle_b, handle_a)];
//     let read = OwnedValue::Record(full_record.clone()).pack_new(&alloc).unwrap().as_record().unwrap();
//     assert_eq!(full_record, read);
// }

// #[test]
// fn test_store_string() {
//     // Test store + retrieve int
//     let alloc = MemoryStorage::new();
//     let handle = OwnedValue::String("foo".to_string()).pack_new(&alloc).unwrap();
//     assert_eq!("foo", handle.as_str().unwrap());
// }

// #[test]
// fn test_store_thunk() {
//     // Test store + retrieve int
//     let alloc = MemoryStorage::new();
//     let handle = OwnedValue::Numeric(Numeric::Int(42)).pack_new(&alloc).unwrap();
//     let thunk = OwnedValue::Thunk(handle.clone()).pack_new(&alloc).unwrap();
//     let thunk_target = thunk.as_thunk().unwrap();
//     assert_eq!(thunk_target, handle);
// }

// #[test]
// fn test_store_code() {
//     // Test store + retrieve int
//     let alloc = MemoryStorage::new();
//     let mut code = Code::new();
//     let builder = code.builder();
//     let mut e = builder.init_ready(1);
//     e.set(0, 1);
//     let handle = OwnedValue::Code(code.clone()).pack_new(&alloc).unwrap();
//     let res_code = handle.as_code().unwrap();
//     println!("before: {}", code.reader());
//     println!("after: {}", res_code.reader());
//     assert_eq!(res_code, code);
// }