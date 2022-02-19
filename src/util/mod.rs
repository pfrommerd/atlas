pub mod graph;

pub fn raw_slice<'s>(slice: &'s [u64]) -> &'s [u8] {
    unsafe {
        std::slice::from_raw_parts(slice.as_ptr() as *const u8,
            slice.len()*std::mem::size_of::<u64>())
    }
}

pub fn raw_mut_slice<'s>(slice: &'s mut [u64]) -> &'s mut [u8] {
    unsafe {
        std::slice::from_raw_parts_mut(slice.as_mut_ptr() as *mut u8,
            slice.len()*std::mem::size_of::<u64>())
    }
}