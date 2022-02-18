use super::allocator::{Allocator, AllocPtr, AllocSize, Segment};
use crate::Error;
use slab::Slab;
use std::cell::{RefCell, UnsafeCell};
use super::allocator::Word;
use std::rc::Rc;

pub struct MemoryAllocator {
    slices: RefCell<Slab<Rc<UnsafeCell<Vec<Word>>>>>
}

impl MemoryAllocator {
    pub fn new() -> Self {
        MemoryAllocator { slices: RefCell::new(Slab::new()) }
    }
}

unsafe impl Allocator for MemoryAllocator {
    type Segment<'s> = MemorySegment;

    fn alloc(&self, word_size: AllocSize) -> Result<AllocPtr, Error> {
        let mut data = Vec::new();
        data.resize_with(word_size as usize, || 0);
        let key = self.slices.borrow_mut().insert(Rc::new(UnsafeCell::new(data)));
        Ok(key as AllocPtr)
    }

    unsafe fn dealloc(&self, handle: AllocPtr, _: AllocSize) {
        self.slices.borrow_mut().remove(handle as usize);
    }

    unsafe fn get<'s>(&'s self, handle: AllocPtr, 
                word_off: AllocSize, word_len: AllocSize) -> Result<Self::Segment<'s>, Error> {
        let slab= self.slices.borrow();
        let data = slab.get(handle as usize).unwrap();
        Ok(MemorySegment { data: data.clone(), word_off, word_len })
    }
}

#[derive(Clone)]
pub struct MemorySegment {
    data: Rc<UnsafeCell<Vec<Word>>>,
    word_off: AllocSize,
    word_len: AllocSize
}

impl<'s> Segment<'s> for MemorySegment {
    fn slice<'a>(&'a self) -> &'a [Word] {
        let s = unsafe { &*self.data.get() };
        &s[self.word_off as usize..(self.word_off + self.word_len) as usize]
    }
    unsafe fn slice_mut<'a>(&'a self) -> &'a mut [Word] {
        let s = &mut *self.data.get();
        &mut s[self.word_off as usize..(self.word_off + self.word_len) as usize]
    }
}