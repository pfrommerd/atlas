use super::allocator::{Allocator, AllocPtr, AllocSize, Segment};
use crate::Error;
use std::alloc::Layout;
use slab::Slab;
use std::cell::RefCell;
use super::allocator::Word;

pub struct MemoryAllocator {
    slices: RefCell<Slab<*mut u64>>
}

impl MemoryAllocator {
    pub fn new() -> Self {
        MemoryAllocator { slices: RefCell::new(Slab::new()) }
    }
}

unsafe impl Allocator for MemoryAllocator {
    type Segment<'s> = MemorySegment;

    fn alloc(&self, word_size: AllocSize) -> Result<AllocPtr, Error> {
        unsafe {
            let res = std::alloc::alloc(
            Layout::from_size_align(8*word_size as usize, 8).unwrap()
            );
            let key = self.slices.borrow_mut().insert(res.cast());
            Ok(key as AllocPtr)
        }
    }

    unsafe fn dealloc(&self, handle: AllocPtr, word_size: AllocSize) {
        std::alloc::dealloc(self.slices.borrow_mut().remove(handle as usize).cast(),
        Layout::from_size_align(8*word_size as usize, 8).unwrap());
    }

    unsafe fn get<'s>(&'s self, handle: AllocPtr, 
                word_off: AllocSize, word_len: AllocSize) -> Result<Self::Segment<'s>, Error> {
        let start = *self.slices.borrow().get(handle as usize).unwrap();
        let start = start.add(word_off as usize);
        Ok(MemorySegment { start, word_len })
    }
}

#[derive(Clone)]
pub struct MemorySegment {
    start: *mut Word,
    word_len: AllocSize
}

impl<'s> Segment<'s> for MemorySegment {
    fn ptr(&self) -> *mut Word {
        self.start
    }
    fn word_len(&self) -> AllocSize {
        self.word_len
    }
    fn slice<'a>(&'a self) -> &'a [Word] {
        unsafe {
            std::slice::from_raw_parts(self.start, self.word_len as usize)
        }
    }
    unsafe fn slice_mut<'a>(&'a self) -> &'a mut [Word] {
        std::slice::from_raw_parts_mut(self.start, self.word_len as usize)
    }
}