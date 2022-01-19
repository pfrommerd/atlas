use std::cell::RefCell;
use capnp::OutputSegments;

use super::storage::{
    DataPointer, DataStorage, DataRef, 
    ObjectRef, ObjPointer, ObjectStorage,
    StorageError
};
use super::ValueReader;
use super::allocator::{VolatileAllocator, AllocHandle, Segment, SegmentMut};

// The local object storage table

pub struct LocalObjectStorage<Alloc : VolatileAllocator> {
    mem: RefCell<Alloc>
}

impl<Alloc : VolatileAllocator> ObjectStorage for LocalObjectStorage<Alloc> {
    type EntryRef<'s> where Alloc : 's = LocalEntryRef<'s, Alloc>;

    fn alloc<'s>(&'s self) -> Result<Self::EntryRef<'s>, StorageError> {
        let mut alloc = self.mem.borrow_mut();
        let handle : AllocHandle = alloc.alloc(2)?;
        Ok(LocalEntryRef {
            handle, store: self
        })
    }
    fn get<'s>(&'s self, ptr: ObjPointer) -> Result<Self::EntryRef<'s>, StorageError> {
        Ok(LocalEntryRef {
            handle: ptr.unwrap(), store: self
        })
    }
}

pub struct LocalEntryRef<'s, Alloc: VolatileAllocator> {
    handle: AllocHandle,
    // a reference to the original memory object
    store: &'s LocalObjectStorage<Alloc>
}

impl<'s, Alloc: VolatileAllocator> ObjectRef<'s> for LocalEntryRef<'s, Alloc> {
    fn ptr(&self) -> ObjPointer {
        ObjPointer::from(self.handle)
    }
    fn get_value(&self) -> Result<Option<DataPointer>, StorageError> {
        let mem = self.store.mem.borrow();
        // This is unsafe since we are slicing memory
        // However since we own the allocator, we can guarantee
        // that others are not modifying the same slice
        unsafe {
            let seg = mem.slice(self.handle, 0, 2)?;
            let s : &[u64; 2] = seg.as_slice().try_into().unwrap();
            let d = if s[0] != 0 { Some(DataPointer::from(s[0])) } else { None };
            Ok(d)
        }
    }
    fn set_value(&self, val: DataPointer) {
        let mem = self.store.mem.borrow();
        unsafe {
            let mut seg = mem.slice_mut(self.handle, 0, 2).unwrap();
            let s : &mut [u64; 2] = seg.as_slice_mut().try_into().unwrap();
            s[0] = val.unwrap();
        }
    }
    // Will push a result value over a thunk value
    fn push_result(&self, val: DataPointer) {
        let mem = self.store.mem.borrow();
        unsafe {
            let mut seg = mem.slice_mut(self.handle, 0, 2).unwrap();
            let s : &mut [u64; 2] = seg.as_slice_mut().try_into().unwrap();
            s[1] = s[0];
            s[0] = val.unwrap();
        }
    }
    // Will restore the old thunk value
    // and return the current value (if it exists)
    fn pop_result(&self) -> Option<DataPointer> {
        let mem = self.store.mem.borrow();
        unsafe {
            let mut seg = mem.slice_mut(self.handle, 0, 2).unwrap();
            let s : &mut [u64; 2] = seg.as_slice_mut().try_into().unwrap();
            let d = if s[0] != 0 { Some(DataPointer::from(s[0])) } else { None };
            s[0] = s[1];
            s[1] = 0;
            d
        }
    }
}

// A local data storage
pub struct LocalDataStorage<Alloc: VolatileAllocator> {
    alloc: Alloc
}

impl<Alloc: VolatileAllocator> DataStorage for LocalDataStorage<Alloc> {
    type EntryRef<'s> where Alloc: 's = LocalDataRef<'s, Alloc>;

    fn insert<'s>(&'s mut self, val: ValueReader<'_>) 
                -> Result<Self::EntryRef<'s>, StorageError> {
        // TODO: Pre-calculate the size so we don't have to do this extra copying step
        // we can't directly allocate onto the heap since we can't allocate new segments
        // due to the underlying data volatility
        let mut builder = capnp::message::Builder::new_default();
        builder.set_root_canonical(val).unwrap();

        let seg = if let OutputSegments::SingleSegment(s) = builder.get_segments_for_output() {
            s[0]
        } else {
            panic!("Should only have a single segment")
        };
        // allocate a single segment
        let len = ((seg.len() + 7) / 8) as u64;
        let handle= self.alloc.alloc(len + 1)?;
        // This is safe since no once else can have sliced the memory yet
        let mut hdr_slice = unsafe { self.alloc.slice_mut(handle, 0, 1)? };
        let mut slice = unsafe { self.alloc.slice_mut(handle, 1, len)? };

        // set header
        hdr_slice.as_slice_mut()[0] = len + 1;
        let raw_dest = slice.as_raw_slice_mut();
        raw_dest.clone_from_slice(seg);
        self.get(DataPointer::from(handle))
    }

    // however we can do simultaneous get() access
    fn get<'s>(&'s self, ptr : DataPointer)
                -> Result<Self::EntryRef<'s>, StorageError> {
        let handle : AllocHandle = ptr.unwrap();
        let seg = unsafe {
            // once things have been inserted, you can only get them mutably
            // so this is actually safe to slice into this handle
            let len = self.alloc.slice(handle, 0, 1)?.as_slice()[0].to_le();
            self.alloc.slice(handle, 1, len - 1)?
        };
        Ok(LocalDataRef { ptr, seg })
    }
}

pub struct LocalDataRef<'s, Alloc: VolatileAllocator + 's> {
    ptr: DataPointer,
    // a reference to the underlying memory segment
    seg: Alloc::Segment<'s>
}

impl<'s, Alloc: VolatileAllocator> DataRef<'s> for LocalDataRef<'s, Alloc> {
    fn ptr(&self) -> DataPointer {
        self.ptr
    }

    fn value<'r>(&'r self) -> ValueReader<'r> {
        let slice = self.seg.as_slice();
        // convert to u8 slice
        let data = unsafe {
            let n_bytes = slice.len() * std::mem::size_of::<u64>();
            std::slice::from_raw_parts(slice.as_ptr() as *const u8, n_bytes)
        };
        let any_ptr = capnp::any_pointer::Reader::new(
            capnp::private::layout::PointerReader::get_root_unchecked(&data[0])
        );
        any_ptr.get_as().unwrap()
    }
}