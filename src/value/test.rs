use super::mem::MemoryAllocator;
use super::owned::{OwnedValue, Numeric, Code};

#[test]
fn test_store_numeric() {
    // Test store + retrieve int
    let alloc = MemoryAllocator::new();
    let handle = OwnedValue::Numeric(Numeric::Int(42)).pack_new(&alloc).unwrap();
    let result = handle.to_owned().unwrap();
    match result {
        OwnedValue::Numeric(n) => assert_eq!(n, Numeric::Int(42)),
        _ => panic!("Expected numeric")
    }
    // Test store + retreive float on the same allocator
    let handle = OwnedValue::Numeric(Numeric::Float(42.314)).pack_new(&alloc).unwrap();
    let result = handle.to_owned().unwrap();
    match result {
        OwnedValue::Numeric(n) => assert_eq!(n, Numeric::Float(42.314)),
        _ => panic!("Expected numeric")
    }
}

#[test]
fn test_store_thunk() {
    // Test store + retrieve int
    let alloc = MemoryAllocator::new();
    let handle = OwnedValue::Numeric(Numeric::Int(42)).pack_new(&alloc).unwrap();
    let thunk = OwnedValue::Thunk(handle.clone()).pack_new(&alloc).unwrap();
    let thunk_target = thunk.as_thunk().unwrap();
    assert_eq!(thunk_target, handle);
}

#[test]
fn test_store_code() {
    // Test store + retrieve int
    let alloc = MemoryAllocator::new();
    let mut code = Code::new();
    let builder = code.builder();
    let e = builder.init_externals(1);
    e.get(0).set_ptr(1);
    let handle = OwnedValue::Code(code.clone()).pack_new(&alloc).unwrap();
    let res_code = handle.as_code().unwrap();
    println!("before: {}", code.reader());
    println!("after: {}", res_code.reader());
    assert_eq!(res_code, code);
}