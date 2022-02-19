use super::{storage::{Storage, AllocPtr, AllocSize, Segment}, AllocHandle};
use crate::Error;
use slab::Slab;
use std::cell::{RefCell, UnsafeCell};
use super::storage::Word;
use std::rc::Rc;

pub struct MemoryStorage {
    slices: RefCell<Slab<Rc<UnsafeCell<Vec<Word>>>>>
}

impl MemoryStorage {
    pub fn new() -> Self {
        MemoryStorage { slices: RefCell::new(Slab::new()) }
    }
}

unsafe impl Storage for MemoryStorage {
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
        Ok(MemorySegment { 
            data: data.clone(), 
            handle: AllocHandle::new(self, handle),
            word_off, word_len
        })
    }
}

#[derive(Clone)]
pub struct MemorySegment<'s> {
    data: Rc<Vec<Word>>,
    handle: AllocHandle<'s, MemoryStorage>,
    word_off: AllocSize,
    word_len: AllocSize
}

impl<'s> Segment<'s> for MemorySegment<'s> {
    fn handle(&self) -> AllocHandle<'s, MemoryStorage> {
        self.handle.clone()
    }

    fn offset(&self) -> AllocSize {
        self.word_off
    }

    fn length(&self) -> AllocSize {
        self.word_len
    }
}