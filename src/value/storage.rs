use super::{ValueReader, ValueBuilder};

use capnp::message::{Builder as MessageBuilder, Reader as MessageReader, HeapAllocator};

use std::cell::{RefCell, Cell, RefMut, Ref};
use aovec::Aovec;

#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Pointer(usize);

impl From<usize> for Pointer {
    fn from(p: usize) -> Pointer {
        Pointer(p)
    }
}

impl From<u64> for Pointer {
    fn from(p: u64) -> Pointer {
        Pointer(p as usize)
    }
}

// TODO: Separate entry into data, other types
pub trait Storage : Sync + Send {
    type Initializer<'s> : ValueInitializer<'s> where Self: 's;
    type Entry<'s> : ValueEntry<'s> where Self: 's;

    fn alloc<'s>(&'s self) -> Self::Initializer<'s>;

    // Will return the entry as it is right now.
    // Even if the underlying memory gets set() to
    // another value, the returned entry here will still be valid.
    // Additionally, this method prevents garbage collection 

    // TODO: This functionality should be separated since we only
    // want to prevent garbage collection through get(), and should
    // get a fix on the underlying data separately! Otherwise scope
    // entries are implicitly fixed to a particular value
    fn get<'s>(&'s self, ptr: Pointer) -> Option<Self::Entry<'s>>;

    // This allows for an atomic move of the data from source
    // into dest.
    fn set(&self, dest: Pointer, src: Pointer);
}

pub trait ValueInitializer<'s> {
    fn ptr(&self) -> Pointer;
    fn builder<'a>(&'a mut self) -> ValueBuilder<'a>;
}

pub trait ValueEntry<'s> {
    fn ptr(&self) -> Pointer;
    fn reader<'a>(&'a self) -> ValueReader<'a>;
}

struct Slot {
    reader: MessageReader<MessageBuilder<HeapAllocator>>
}

pub struct HeapStorage {
    heap: Aovec<RefCell<Option<Slot>>>,
    pointers: Aovec<Cell<usize>>
}

impl HeapStorage {
    pub fn new() -> Self {
        Self { 
            heap: Aovec::new(32),
            pointers: Aovec::new(32)
        }
    }
}

impl Storage for HeapStorage {
    type Initializer<'s> = HeapInitializer<'s>;
    type Entry<'s> = HeapEntry<'s>;

    fn alloc<'s>(&'s self) -> Self::Initializer<'s> {
        let idx = self.heap.push(RefCell::new(None));
        let c = &self.heap[idx];
        let r = c.borrow_mut();

        let ptr_idx = self.pointers.push(Cell::new(idx));
        let ptr = Pointer::from(ptr_idx);
        HeapInitializer {
            entry: r,
            ptr,
            message: MessageBuilder::new_default()
        }
    }

    fn get<'s>(&'s self, ptr: Pointer) -> Option<Self::Entry<'s>> {
        let idx = self.pointers.get(ptr.0)?.get();
        let entry = self.heap.get(idx);
        entry.map(|x| {
            HeapEntry {
                entry: x.borrow(), ptr
            }
        })
    }

    fn set(&self, dest: Pointer, src: Pointer) {
        self.pointers[dest.0].set(self.pointers[src.0].get())
    }
}

pub struct HeapInitializer<'s> {
    entry: RefMut<'s, Option<Slot>>,
    ptr: Pointer,
    message: MessageBuilder<HeapAllocator>,
}

impl<'s> ValueInitializer<'s> for HeapInitializer<'s> {
    fn ptr(&self) -> Pointer {
        self.ptr
    }
    fn builder<'a>(&'a mut self) -> ValueBuilder<'a> {
        self.message.get_root().unwrap()
    }
}

impl<'s> Drop for HeapInitializer<'s> {
    fn drop(&mut self) {
        let reader = self.message.get_root_as_reader::<ValueReader>().unwrap();
        let mut can = MessageBuilder::new_default();
        can.set_root_canonical(reader).unwrap();
        let reader = can.into_reader();
        *self.entry = Some(Slot { reader })
    }
}

pub struct HeapEntry<'s> {
    entry: Ref<'s, Option<Slot>>,
    ptr: Pointer
}

impl<'s> ValueEntry<'s> for HeapEntry<'s> {
    fn ptr<'a>(&self) -> Pointer {
        self.ptr
    }
    fn reader<'a>(&'a self) -> ValueReader<'a> {
        let s = self.entry.as_ref().unwrap();
        let r = &s.reader;
        r.get_root().unwrap()
    }
}


// pub struct ValueInitializer<'a> {
//     entry: VacantEntry<'a, Bytes, DefaultConfig>,
//     message: MessageBuilder<HeapAllocator>
// }

// impl<'a> ValueInitializer<'a> {
//     pub fn ptr(&self) -> Pointer {
//         Pointer::from(self.entry.key())
//     }

//     pub fn builder<'r>(&'r mut self) -> ValueBuilder<'r> {
//         self.message.get_root().unwrap()
//     }
// }

// impl<'a> Drop for ValueInitializer<'a> {
//     fn drop(&mut self) {
//         self.entry.insert(self.val);
//     }
// }