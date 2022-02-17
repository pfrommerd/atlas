use super::mem::MemoryAllocator;
use super::owned::{OwnedValue, Numeric};

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