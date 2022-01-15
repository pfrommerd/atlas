use super::{allocator::{VolatileAllocator, AllocHandle, AllocSize, Segment, SegmentMut}, storage::StorageError};
use std::alloc::Layout;
use slab::Slab;
use super::allocator::Word;

pub struct MemoryAllocator {
    slices: Slab<*mut u64>
}

impl MemoryAllocator {
    pub fn new() -> Self {
        MemoryAllocator { slices: Slab::new() }
    }
}

unsafe impl VolatileAllocator for MemoryAllocator {
    type Segment<'s> = MemorySegment<'s>;
    type SegmentMut<'s> = MemorySegmentMut<'s>;

    fn alloc(&mut self, word_size: AllocSize) -> Result<AllocHandle, StorageError> {
        unsafe {
            let res = std::alloc::alloc(
            Layout::from_size_align(8*word_size as usize, 8).unwrap()
            );
            let key = self.slices.insert(res.cast());
            Ok(key as AllocHandle)
        }
    }

    unsafe fn dealloc(&mut self, handle: AllocHandle, word_size: AllocSize) {
        std::alloc::dealloc((*self.slices.get(handle as usize).unwrap()).cast(),
    Layout::from_size_align(8*word_size as usize, 8).unwrap());
    }

    unsafe fn slice<'s>(&'s self, handle: AllocHandle, 
                word_off: AllocSize, word_len: AllocSize) -> Result<Self::Segment<'s>, StorageError> {
        let start = self.slices.get(handle as usize).unwrap();
        let start = start.add(word_off as usize);
        let slice = std::slice::from_raw_parts(start, word_len as usize);
        Ok(MemorySegment { slice })
    }

    unsafe fn slice_mut<'s>(&'s self, handle: AllocHandle, 
                word_off: AllocSize, word_len: AllocSize) -> Result<Self::SegmentMut<'s>, StorageError> {
        let start = self.slices.get(handle as usize).unwrap();
        let start = start.add(word_off as usize);
        let slice = std::slice::from_raw_parts_mut(start, word_len as usize);
        Ok(MemorySegmentMut { slice })
    }
}

pub struct MemorySegment<'s> {
    slice: &'s [u64]
}

pub struct MemorySegmentMut<'s> {
    slice: &'s mut [u64]
}

impl<'s> Segment<'s> for MemorySegment<'s> {
    fn as_slice(&self) -> &[u64] {
        self.slice
    }
    fn as_raw_slice(&self) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(self.slice.as_ptr().cast(), 
                self.slice.len()*std::mem::size_of::<Word>())
        }
    }
}

impl<'s> SegmentMut<'s> for MemorySegmentMut<'s> {
    fn as_slice_mut(&mut self) -> &mut [Word] {
        self.slice
    }
    fn as_raw_slice_mut(&mut self) -> &mut [u8] {
        unsafe {
            std::slice::from_raw_parts_mut(self.slice.as_mut_ptr().cast(), 
                self.slice.len()*std::mem::size_of::<Word>())
        }
    }
}
impl<'s> Segment<'s> for MemorySegmentMut<'s> {
    fn as_slice(&self) -> &[u64] {
        self.slice
    }
    fn as_raw_slice(&self) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(self.slice.as_ptr().cast(), 
                self.slice.len()*std::mem::size_of::<Word>())
        }
    }
}