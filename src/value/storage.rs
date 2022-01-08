use super::{ValueReader, ValueBuilder};

use sharded_slab::{VacantEntry, Entry, Slab, DefaultConfig};
use capnp::message::{Builder as MessageBuilder, Reader as MessageReader, HeapAllocator};

#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct Pointer(usize);

impl Pointer {
    pub fn wrap(p: usize) -> Pointer {
        Pointer(p)
    }
    pub fn unwrap(self) -> usize {
        self.0
    }
}

pub trait Storage {
    type Initializer<'s> : ValueInitializer<'s> where Self: 's;
    type Entry<'s> : ValueEntry<'s> where Self: 's;

    fn alloc<'s>(&'s self) -> Self::Initializer<'s>;
    fn get<'s>(&'s self, ptr: Pointer) -> Option<Self::Entry<'s>>;
}

pub trait ValueInitializer<'s> {
    fn builder<'a>(&'a mut self) -> ValueBuilder<'a>;
}

pub trait ValueEntry<'s> {
    fn reader<'a>(&'a self) -> ValueReader<'a>;
}

struct Slot {
    reader: MessageReader<MessageBuilder<HeapAllocator>>
}

pub struct HeapStorage {
    heap: Slab<Slot>,
}

impl HeapStorage {
    pub fn new() -> Self {
        Self { heap: Slab::new() }
    }
}

impl Storage for HeapStorage {
    type Initializer<'s> = HeapInitializer<'s>;
    type Entry<'s> = HeapEntry<'s>;

    fn alloc<'s>(&'s self) -> Self::Initializer<'s> {
        panic!("Can't alloc")
    }

    fn get<'s>(&'s self, ptr: Pointer) -> Option<Self::Entry<'s>> {
        let entry = self.heap.get(ptr.unwrap());
        entry.map(|x| {
            HeapEntry::new(x)
        })
    }
}

pub struct HeapInitializer<'s> {
    entry: Option<VacantEntry<'s, Slot, DefaultConfig>>,
    message: MessageBuilder<HeapAllocator>
}

impl<'s> ValueInitializer<'s> for HeapInitializer<'s> {
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
        let mut entry = None;
        std::mem::swap(&mut self.entry, &mut entry);
        let e = entry.unwrap();
        e.insert(Slot { reader });
    }
}

pub struct HeapEntry<'s> {
    entry: Entry<'s, Slot>
}

impl<'s> HeapEntry<'s> {
    fn new(entry: Entry<'s, Slot>) -> Self {
        HeapEntry { entry }
    }
}

impl<'s> ValueEntry<'s> for HeapEntry<'s> {
    fn reader<'a>(&'a self) -> ValueReader<'a> {
        let s = &*self.entry;
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