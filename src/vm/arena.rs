pub type Pointer = u64;
pub type TableID = u64;

use super::ValueBuilder;

pub struct Arena<H:HeapStorage> {
    heap: H
}

impl<H:HeapStorage> Arena<H> {
    // Allocate a value on the heap
    pub fn alloc<'a>(&'a mut self) -> (Pointer, ValueBuilder<'a>) {
        self.heap.alloc()
    }
}

// The underlying storage for the heap
pub trait HeapStorage {
    fn alloc<'a>(&'a mut self) -> (Pointer, ValueBuilder<'a>);
}

// Memory based heap storage
// struct MemoryStorage {
// 
// }

/*impl HeapStorage for MemoryStorage {
    fn alloc<'a>(&'a mut self) -> ValueBuilder<'a> {
    }
}*/