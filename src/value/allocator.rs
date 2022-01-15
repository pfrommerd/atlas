use super::storage::StorageError;


// We use u64 instead of usize everywhere in order
// to ensure cross-platform binary
// compatibility e.g if we are on a 32 bit system
// we can use a file produced on a 64 bit system since
// everything uses 64 bit addresses/alignment

// A handle *must* be just a u64
// but note that it doesn't have to correspond to an actual
// offset.
pub type AllocHandle = u64;

pub type AllocSize = u64;
pub type Word = u64;

// All memory returned by this allocator
// must be in terms of 8-byte aligned words
pub unsafe trait VolatileAllocator {
    type Segment<'s> : Segment<'s> where Self : 's;
    type SegmentMut<'s> : SegmentMut<'s> where Self : 's;

    fn alloc(&mut self, word_size: u64) -> Result<AllocHandle, StorageError>;

    // The user is responsible for ensuring that they own the
    // underlying handle previously returnned by alloc(), as well
    // as the correct word_size
    unsafe fn dealloc(&mut self, handle: AllocHandle, word_size: AllocSize);

    // We need to mutably borrow self in order to lock the
    // allocator so that it doesn't potentially get reallocated
    // while we mutate the slice

    // Note that this function is unsafe since the caller
    // must ensure
    // (a) [word_off, word_off + word_len] is in the underlying region
    // (b) the region is currently not being sliced mutably
    unsafe fn slice<'s>(&'s self, handle: AllocHandle,
                word_off: AllocSize, word_len: AllocSize) 
                -> Result<Self::Segment<'s>, StorageError>;

    // Note that this function is unsafe since the caller
    // must ensure
    // (a) [word_off, word_off + word_len] is in the underlying region
    // (b) the region is currently not being sliced
    //     anywhere else, mutably or immutably
    unsafe fn slice_mut<'s>(&'s self, handle: AllocHandle,
                word_off: AllocSize, word_len: AllocSize) 
                -> Result<Self::SegmentMut<'s>, StorageError>;
}

pub trait Segment<'s> {
    fn as_slice(&self) -> &[Word];
    fn as_raw_slice(&self) -> &[u8];
}

pub trait SegmentMut<'s> : Segment<'s> {
    fn as_slice_mut(&mut self) -> &mut [Word];

    // helper methods for getting u8 slices instead
    // of word-aligned slices
    fn as_raw_slice_mut(&mut self) -> &mut [u8];
}