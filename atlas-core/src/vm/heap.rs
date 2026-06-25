use super::term::{Brand, Node, Term};
use crate::util::slab::{ShardedSlab, UniqueKey, UniqueSlot};
use crate::util::{SingleMutex, U56};
use std::ops::Deref;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

pub struct Heap {
    nodes: ShardedSlab<Addr, Node>,
    dups: ShardedSlab<Addr, DupCell>,
    sups: ShardedSlab<Addr, SupCell>,
    values: ShardedSlab<Addr, ValueCell>,
}

#[derive(Debug, Clone)]
pub enum Value {
    Str(Arc<str>),
    Bytes(Arc<[u8]>),
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

struct ValueCell {
    count: AtomicUsize,
    value: Value,
}

struct SupCell {
    left: Addr,
    right: Addr,
}

// The heap only contains functions for branding.
// Any interaction with the heap itself should go through the HeapScope<'h>
impl Heap {
    pub unsafe fn forge_brand<'h>(&self) -> &HeapScope<'h> {
        unsafe { &*(self as *const Self as *const HeapScope<'h>) }
    }
    /// Safely brand this slab for the duration of `f`.
    pub fn with<R>(&self, f: impl for<'h> FnOnce(&HeapScope<'h>) -> R) -> R {
        f(unsafe { self.forge_brand() })
    }
}

pub type Addr = U56;

#[derive(Debug, PartialEq, Eq, Hash)]
pub struct TermPtr<'h>(UniqueKey<'h, Addr>);
pub struct TermSlot<'h>(UniqueSlot<'h, Addr, Node>);

#[rustfmt::skip]
impl<'h> TermPtr<'h> {
    pub unsafe fn forge(addr: Addr) -> TermPtr<'h> {
        unsafe { TermPtr(UniqueKey::forge(addr)) }
    }
    pub fn into_addr(self) -> Addr { self.0.into_raw() }
}
impl<'h> TermSlot<'h> {
    // SAFETY: The caller must ensure that the term
    // originally contained in this slot is no longer reachable,
    // as otherwise reading from the TermPtr<'h> again may lead to duplicate
    // Terms being created, which is unsound as they should be affine.
    pub unsafe fn unchanged(self) -> TermPtr<'h> {
        TermPtr(self.0.finished())
    }
    pub fn finished(mut self, term: Term<'h>) -> TermPtr<'h> {
        self.0.update(term.pack());
        TermPtr(self.0.finished())
    }
}

#[derive(Debug, PartialEq, Eq, Hash)]
pub struct NamePtr<'h>(Addr, Brand<'h>);
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct MatchPtr<'h>(Addr, Brand<'h>);
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct PackPtr<'h>(Addr, Brand<'h>);
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct BodyPtr<'h> {
    binder: Addr,
    body: Addr,
    brand: Brand<'h>,
}

// one side of a duplication
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DupPtr<'h>(Addr, bool, Brand<'h>);
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct SupPtr<'h>(UniqueKey<'h, Addr>);
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct ValuePtr<'h>(UniqueKey<'h, Addr>);
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct TracePtr<'h>(UniqueKey<'h, Addr>);

impl<'h> HeapScope<'h> {
    // Consumes the pointer, returns the slot for updating
    // the value, as well as the unpacked term.
    pub fn term(&self, ptr: TermPtr<'h>) -> (TermSlot<'h>, Term<'h>) {
        let nodes = unsafe { self.heap.nodes.forge_brand() };
        let slot = nodes.get_unique(ptr.0);
        let term = unsafe { slot.deref().unpack() };
        (TermSlot(slot), term)
    }
}

// Contains the spine of a reduction
pub struct Spine<'h> {
    terms: Vec<(TermSlot<'h>, Term<'h>)>,
}
