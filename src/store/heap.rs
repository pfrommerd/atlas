use super::{Storage, ObjectHandle, ObjectPtr, 
    ObjectReader,
    Numeric
};
use sharded_slab::Slab;
use std::cell::Cell;
use std::ops::Deref;
use bytes::Bytes;
use crate::{Error, ErrorKind};

type HeapPtr = usize;

impl ObjectPtr for HeapPtr {}

struct Code {

}

struct DirectStringReader<'s> {
    str: &'s String
}

struct HeapHandle<'s> {
    entry: sharded_slab::Entry<'s, Value>,
    heap: &'s HeapStorage
}

impl<'s> HeapHandle<'s> {
    fn new(entry: sharded_slab::Entry<'s, Value>, heap: &'s HeapStorage) -> Self {
        Self { entry, heap }
    }
}

impl<'s> ObjectHandle<'s> for HeapHandle<'s> {
    type Storage = HeapStorage;
    fn ptr(&self) -> HeapPtr { self.entry.key() + 1 }
    fn storage(&self) -> &'s Self::Storage { self.heap }
    fn reader(&self) -> Result<ObjectReader<'s, Self::Storage>, Error> {
        Ok(ObjectReader::Bot)
    }
}

pub struct HeapStorage {
    storage: Slab<Value>
}

impl HeapStorage {
    fn do_build<'s>(&'s self, v: Value) -> Result<HeapHandle<'s>, Error> {
        let key = self.storage.insert(v).unwrap();
        Ok(HeapHandle::new(self.storage.get(key).unwrap(), self))
    }
}

impl Storage for HeapStorage {
    type Handle<'s> = HeapHandle<'s>;
    type Ptr = HeapPtr;

    type StringReader<'s> = HeapStringReader<'s>;
    type BufferReader<'s> = HeapBufferReader<'s>;
    type TupleReader<'s> = HeapTupleReader<'s>;
    type RecordReader<'s> = HeapRecordReader<'s>;
    type CodeReader<'s> = HeapCodeReader<'s>;
    type PartialReader<'s> = HeapPartialReader<'s>;

    type IndirectBuilder<'s> = HeapIndirectBuilder<'s>;
    type StringBuilder<'s> = HeapStringBuilder<'s>;
    type BufferBuilder<'s> = HeapBufferBuilder<'s>;

    type TupleBuilder<'s> = HeapTupleBuilder<'s>;
    type RecordBuilder<'s> = HeapRecordBuilder<'s>;
    type CodeBuilder<'s> = HeapCodeBuilder<'s>;
    type PartialBuilder<'s> = HeapPartialBuilder<'s>;
    // the directly buildable

    fn build_unit<'s>(&'s self) -> Result<HeapHandle<'s>, Error> {
        self.do_build(Value::Unit)
    }
    fn build_bot<'s>(&'s self) -> Result<Self::Handle<'s>, Error> {
        self.do_build(Value::Bot)
    }
    fn build_numeric<'s>(&'s self, n: Numeric) -> Result<Self::Handle<'s>, Error> {
        self.do_build(Value::Numeric(n))
    }
    fn build_char<'s>(&'s self, c: char) -> Result<Self::Handle<'s>, Error> {
        self.do_build(Value::Char(c))
    }
    fn build_bool<'s>(&'s self, b: bool) -> Result<Self::Handle<'s>, Error> {
        self.do_build(Value::Bool(b))
    }

    fn build_nil<'s>(&'s self) -> Result<Self::Handle<'s>, Error> {
        self.do_build(Value::Nil)
    }
    fn build_cons<'s>(&'s self, head: Self::Ptr, tail: Self::Ptr) 
            -> Result<Self::Handle<'s>, Error> {
        self.do_build(Value::Cons(head, tail))
    }

    fn build_thunk<'s>(&'s self, target: Self::Ptr)
            -> Result<Self::Handle<'s>, Error> {
        self.do_build(Value::Thunk(target))
    }

    // builders
    fn indirect_builder<'s>(&'s self)
            -> Result<Self::IndirectBuilder<'s>, Error> {
        Ok(HeapIndirectBuilder::new(self,
            self.storage.vacant_entry().unwrap()))
    }
    fn string_builder<'s>(&'s self, len: usize)
            -> Result<Self::StringBuilder<'s>, Error> {
        Ok(HeapStringBuilder::new(self,
            self.storage.vacant_entry().unwrap()))
    }

    fn get<'s>(&'s self, ptr: Self::Ptr) -> Result<Self::Handle<'s>, Error> {
        let entry = self.storage.get(ptr - 1).ok_or(
            Error::new_const(ErrorKind::BadPointer, "Invalid pointer"))?;
        Ok(HeapHandle { entry, heap: self })
    }
}


// Builders
use super::{Builder, IndirectBuilder};

// Indirect builder
pub struct HeapIndirectBuilder<'s> {
    entry: sharded_slab::Entry<'s, Value>,
    heap: &'s HeapStorage,
    dest_ptr: HeapPtr
}

impl<'s> HeapIndirectBuilder<'s> {
    fn new(heap: &'s HeapStorage, vacant: sharded_slab::VacantEntry<'s, Value>) -> Self {
        let key = vacant.key();
        vacant.insert(Value::Indirect(Cell::new(0)));
        Self { entry: heap.storage.get(key).unwrap(), heap, dest_ptr: 0 }
    }
}
impl<'s> Builder<'s> for HeapIndirectBuilder<'s> {
    type Storage = HeapStorage;
}
impl<'s> IndirectBuilder<'s> for HeapIndirectBuilder<'s> {
    fn handle(&self) -> HeapHandle<'s> {
        HeapHandle::new(self.entry, self.heap)
    }
    fn set(&mut self, dest: <Self::Storage as Storage>::Ptr) {
        self.dest_ptr = dest
    }
    fn build(self) -> HeapHandle<'s> {
        match self.entry.deref() {
            Value::Indirect(c) => c.set(self.dest_ptr),
            _ => panic!("Bad value")
        }
        HeapHandle::new(self.entry, self.heap)
    }
}