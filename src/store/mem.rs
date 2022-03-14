use super::{Storage, AllocPtr, AllocSize, AllocationType,
            Allocation, Segment, MutSegment, AllocHandle};
use crate::{Error, ErrorKind};
use slab::Slab;
use std::borrow::{Borrow, BorrowMut};
use std::ops::{Deref, DerefMut};
use std::convert::{AsMut, AsRef};
use std::cell::{RefCell, Cell};
use std::rc::Rc;

pub struct MemoryStorage {
    slab: RefCell<Slab<(AllocationType, Rc<Vec<u8>>)>>
}

impl MemoryStorage {
    pub fn new() -> Self {
        MemoryStorage { slab: RefCell::new(Slab::new()) }
    }
}

impl MemoryStorage {
    fn _insert(&self, type_: AllocationType, data: Vec<u8>) -> usize {
        let entry = (type_, Rc::new(data));
        self.slab.borrow().insert(entry)
    }
}

impl Storage for MemoryStorage {
    type Segment<'s> = MemorySegment;
    type MutSegment<'s> = MutMemorySegment<'s>;
    type Allocation<'s> = MemoryAllocation<'s>;

    fn alloc<'s>(&'s self, type_: AllocationType, size: AllocSize) 
            -> Result<MemoryAllocation<'s>, Error> {
        let mut data = Vec::new();
        data.resize_with(size as usize, || 0);
        Ok(MemoryAllocation { data, type_, storage: self })
    }

    fn dealloc(&self, handle: AllocPtr, _: AllocSize) -> Result<(), Error> {
        self.slab.borrow_mut().remove(handle as usize);
        Ok(())
    }

    fn get_handle<'s>(&'s self, ptr: AllocPtr) -> Result<AllocHandle<'s, Self>, Error> {
        let entry = self.slab.borrow().get(ptr as usize)
            .ok_or(Error::new_const(ErrorKind::BadPointer, "Tried to get a handle with a bad pointer"))?;
        let (type_, _) = entry.deref();
        Ok(AllocHandle::new(self, *type_, ptr))
    }

    fn get<'s>(&'s self, handle: AllocPtr, 
                word_off: AllocSize, word_len: AllocSize) -> Result<Self::Segment<'s>, Error> {
        let (type_, data)= self.slab.borrow().get(handle as usize).unwrap();
        Ok(MemorySegment { 
            data: data.clone(), word_off, word_len
        })
    }

    fn overwrite_atomic(&self, handle: AllocPtr, value: &[u8]) -> Result<(), Error> {
    }
}

#[derive(Clone)]
pub struct MemoryAllocation<'s> {
    data: Vec<u8>,
    type_: AllocationType,
    storage: &'s MemoryStorage
}

impl<'s> Allocation<'s, MemoryStorage> for MemoryAllocation<'s> {
    fn get_mut<'a>(&'a mut self, off: AllocSize, len: AllocSize)
        -> Result<MutMemorySegment<'a>, Error> {
        let data = self.data.as_mut_slice();
        Ok(MutMemorySegment { data })
    }

    fn complete(self) -> AllocHandle<'s, MemoryStorage> {
        let entry = (self.type_, Rc::new(self.data));
        let key = self.storage.slab.borrow_mut().insert(entry);
        let ptr = key as AllocPtr;
    }
}

#[derive(Clone)]
pub struct MemorySegment {
    data: Rc<Vec<u8>>,
    word_off: AllocSize,
    word_len: AllocSize
}

impl AsRef<[u8]> for MemorySegment {
    fn as_ref(&self) -> &[u8] {
        self.data.as_ref()
    }
}
impl Borrow<[u8]> for MemorySegment {
    fn borrow(&self) -> &[u8] {
        self.data.as_ref()
    }
}
impl Deref for MemorySegment {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.data.as_ref()
    }
}

impl<'s> Segment<'s> for MemorySegment {}


pub struct MutMemorySegment<'a> {
    data: &'a mut [u8]
}

impl AsMut<[u8]> for MutMemorySegment<'_> {
    fn as_mut(&mut self) -> &mut[u8] {
        self.data
    }
}
impl Borrow<[u8]> for MutMemorySegment<'_> {
    fn borrow(&self) -> &[u8] {
        self.data
    }
}
impl BorrowMut<[u8]> for MutMemorySegment<'_> {
    fn borrow_mut(&mut self) -> &mut [u8] {
        self.data
    }
}
impl Deref for MutMemorySegment<'_> {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.data
    }
}
impl DerefMut for MutMemorySegment<'_> {
    fn deref_mut(&mut self) -> &mut [u8] {
        self.data
    }
}

impl<'s> MutSegment<'s> for MutMemorySegment<'s> {}