use super::term::{Addr, Brand, Node};
use crate::util::{ShardedSlab, SingleMutex};
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

pub struct Heap {
    terms: ShardedSlab<Addr, Node>,
    dups: ShardedSlab<Addr, DupCell>,
    values: ShardedSlab<Addr, ValueCell>,
}

#[derive(Debug, Clone)]
pub enum Value {
    Str(Arc<str>),
    Bytes(Arc<[u8]>),
}

pub struct ValueCell {
    count: AtomicUsize,
    value: Value,
}
// The heap scope is the branded version
// of the heap and is used to ensure safety
// of pointers lying outside the heap.
#[repr(transparent)]
pub struct HeapScope<'h> {
    heap: Heap,
    _marker: Brand<'h>,
}

struct DupCell {
    value: SingleMutex<Node>,
    left: Addr,
    right: Addr,
}

// For branding the heap/scope

impl Heap {
    pub unsafe fn forge_brand<'h>(&self) -> &HeapScope<'h> {
        unsafe { &*(self as *const Self as *const HeapScope<'h>) }
    }
    /// Safely brand this slab for the duration of `f`.
    pub fn with<R>(&self, f: impl for<'h> FnOnce(&HeapScope<'h>) -> R) -> R {
        f(unsafe { self.forge_brand() })
    }
}
