use capnp::OutputSegments;

use super::storage::{
    ObjectRef, ObjPointer, Storage,
    ValueRef,
    Indirect, StorageError
};
use super::ValueReader;
use super::allocator::{SegmentAllocator, AllocHandle, Segment, SegmentMut};
use super::mem::MemoryAllocator;

// The local object storage table

pub struct LocalStorage<Alloc: SegmentAllocator> {
    alloc: Alloc,
}
impl LocalStorage<MemoryAllocator> {
    pub fn new_default() -> Self {
        Self {
            alloc: MemoryAllocator::new(),
        }
    }
}

impl<Alloc: SegmentAllocator> LocalStorage<Alloc> {
    // helper function for getting the underlying data for a
    // given allocation
    fn get_data<'s>(&'s self, mut handle: AllocHandle) -> 
            Result<Option<Alloc::Segment<'s>>, StorageError> {
        // the header is only mutated during creation or when
        // moving an indirection (which happens "atomically"), so this is safe
        let mut s = unsafe { self.alloc.slice(handle, 0, 2)? };
        while s.as_slice()[0] == 0 {
            if s.as_slice()[1] == 0 {
                return Ok(None) // we have an unfilled indirection
            } else {
                handle = s.as_slice()[1];
                s = unsafe { self.alloc.slice(handle, 0, 2)? };
            }
        }
        // we have reached a final slice
        let len = s.as_slice()[0];
        let payload = unsafe { self.alloc.slice(handle, 2, 2 + len)? };
        Ok(Some(payload))
    }
}


impl<Alloc: SegmentAllocator> Storage for LocalStorage<Alloc> {
    type ObjectRef<'s> where Alloc : 's =  LocalObjectRef<'s, Alloc>;
    type Indirect<'s> where Alloc : 's = LocalIndirect<'s, Alloc>;

    fn get<'s>(&'s self, ptr: ObjPointer) -> Result<Self::ObjectRef<'s>, StorageError> {
        Ok(LocalObjectRef {
            handle: ptr.raw(), store: self
        })
    }

    fn indirection<'s>(&'s self) -> Result<Self::Indirect<'s>, StorageError> {
        // just allocate space for the header, with 0 length and a null indirection pointer
        let handle : AllocHandle = self.alloc.alloc(2)?;
        Ok(LocalIndirect {
            handle, store: self
        })
    }

    fn insert<'s>(&'s self, val : ValueReader<'_>) -> Result<Self::ObjectRef<'s>, StorageError> {
        let mut builder = capnp::message::Builder::new_default();
        builder.set_root_canonical(val).unwrap();

        let seg = if let OutputSegments::SingleSegment(s) = builder.get_segments_for_output() {
            s[0]
        } else {
            panic!("Should only have a single segment")
        };
        // length in words of 8 bytes
        let len = ((seg.len() + 7) / 8) as u64;
        // 2 word header (length + indirection pointer) + length of data
        let handle= self.alloc.alloc(len + 2)?;
        // This is safe since no once else can have sliced the memory yet
        let mut hdr_slice = unsafe { self.alloc.slice_mut(handle, 0, 2)? };
        let mut slice = unsafe { self.alloc.slice_mut(handle, 2, len)? };

        // set header
        hdr_slice.as_slice_mut()[0] = len;
        hdr_slice.as_slice_mut()[1] = 0; // no indirection pointer
        let raw_dest = slice.as_raw_slice_mut();
        raw_dest.clone_from_slice(seg);
        self.get(handle.into())
    }
}

pub struct LocalObjectRef<'s, Alloc: SegmentAllocator> {
    handle: AllocHandle,
    // a reference to the original memory object
    store: &'s LocalStorage<Alloc>
}

impl<'s, Alloc : SegmentAllocator> Clone for LocalObjectRef<'s, Alloc> {
    fn clone(&self) -> Self {
        Self { handle: self.handle, store: self.store }
    }
}

impl<'s, Alloc : SegmentAllocator> ObjectRef<'s> for LocalObjectRef<'s, Alloc> {
    type ValueRef = LocalValueRef<'s, Alloc>;

    fn ptr(&self) -> ObjPointer {
        ObjPointer::from(self.handle)
    }

    fn value(&self) -> Result<Self::ValueRef, StorageError> {
        let seg = self.store.get_data(self.handle);
        Ok(Self::ValueRef {
            seg: seg?.unwrap()
        })
    }
}

pub struct LocalIndirect<'s, Alloc: SegmentAllocator> {
    handle: AllocHandle,
    store: &'s LocalStorage<Alloc>
}

impl<'s, Alloc: SegmentAllocator> Indirect<'s> for LocalIndirect<'s, Alloc> {
    type ObjectRef = LocalObjectRef<'s, Alloc>;

    fn ptr(&self) -> ObjPointer {
        self.handle.into()
    }

    fn get_ref(&self) -> Self::ObjectRef {
        Self::ObjectRef {
            handle: self.handle, store: self.store
        }
    }

    fn set(self, indirect: Self::ObjectRef) -> Result<Self::ObjectRef, StorageError> {
        let mut hdr = unsafe { self.store.alloc.slice_mut(self.handle, 0, 2)? };
        hdr.as_slice_mut()[1] = indirect.ptr().raw();
        Ok(Self::ObjectRef {
            handle: self.handle, store: self.store
        })
    }
}

pub struct LocalValueRef<'s, Alloc: SegmentAllocator + 's> {
    seg: Alloc::Segment<'s>
}

impl<'s, Alloc> Clone for LocalValueRef<'s, Alloc>
        where Alloc: SegmentAllocator + 's {
    fn clone(&self) -> Self {
        Self { seg: self.seg.clone() }
    }
}

impl<'s, Alloc: SegmentAllocator> ValueRef<'s> for LocalValueRef<'s, Alloc> {
    fn reader<'r>(&'r self) -> ValueReader<'r> {
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