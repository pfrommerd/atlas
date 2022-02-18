use crate::Error;
// We use u64 instead of usize everywhere in order
// to ensure cross-platform binary
// compatibility e.g if we are on a 32 bit system
// we can use a file produced on a 64 bit system since
// everything uses 64 bit addresses/alignment

// A handle *must* be just a u64
// but note that it doesn't have to correspond to an actual
// offset.
pub type AllocPtr = u64;

pub type AllocSize = u64;
pub type Word = u64;

// All memory returned by this allocator
// must be in terms of 8-byte aligned words
pub unsafe trait Allocator {
    type Segment<'s> : Segment<'s> where Self : 's;

    fn alloc(&self, word_size: u64) -> Result<AllocPtr, Error>;
    unsafe fn dealloc(&self, handle: AllocPtr, word_size: AllocSize);
    // The user must ensure that the handle, word_off, and word_len
    // are all valid
    unsafe fn get<'s>(&'s self, handle: AllocPtr,
                word_off: AllocSize, word_len: AllocSize) 
                -> Result<Self::Segment<'s>, Error>;
}

pub trait Segment<'s> : Clone {
    fn ptr(&self) -> *mut Word;
    fn word_len(&self) -> AllocSize;
    fn slice<'a>(&'a self) -> &'a [Word];
    // The user must ensure no one else is slcing
    // the same segment (even through different get() calls)
    unsafe fn slice_mut<'a> (&'a self) -> &'a mut [Word];
}

#[derive(Debug)]
pub struct AllocHandle<'a, Alloc: Allocator> {
    pub alloc: &'a Alloc,
    pub ptr: AllocPtr
}

impl<'a, A: Allocator> std::cmp::PartialEq for AllocHandle<'a, A> {
    fn eq(&self, rhs : &Self) -> bool {
        self.ptr == rhs.ptr && self.alloc as *const _ == rhs.alloc as *const _
    }
}
impl<'a, A: Allocator> std::cmp::Eq for AllocHandle<'a, A> {}

impl<'a, A: Allocator> std::hash::Hash for AllocHandle<'a, A> {
    fn hash<H>(&self, h: &mut H) where H: std::hash::Hasher {
        self.ptr.hash(h);
        let ptr = self.alloc as *const A;
        ptr.hash(h);
    }
}

impl<'a, Alloc: Allocator> Clone for AllocHandle<'a, Alloc> {
    fn clone(&self) -> Self {
        Self { alloc: self.alloc, ptr: self.ptr }
    }
}

impl<'a, Alloc: Allocator> Copy for AllocHandle<'a, Alloc> {}

impl<'a, Alloc: Allocator> AllocHandle<'a, Alloc> {
    // This is unsafe since the alloc and the allocptr
    // must be associated
    pub unsafe fn new(alloc: &'a Alloc, ptr: AllocPtr) -> Self {
        AllocHandle { alloc, ptr }
    }

    pub fn get(&self, word_off: AllocSize, word_len: AllocSize) -> Result<Alloc::Segment<'a>, Error> {
        unsafe {
            self.alloc.get(self.ptr, word_off, word_len)
        }
    }
}