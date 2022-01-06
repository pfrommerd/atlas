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


// An external reference
// to something inside the arena
// While the arena ref exists, the object
// is protected from garbage collection
pub struct ArenaRef<T> {

}

// An internal reference to
// something inside the arena
// This is a mutable reference and can only
// be retrieved via an unsafe api, since technically
// objects in the arena should be immutable
pub struct ArenaMutRef<T> {

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