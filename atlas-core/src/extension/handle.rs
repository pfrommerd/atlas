//! [`Handle`]: a heap-linked, self-erasing wrapper around a [`TermPtr`].
//!
//! Unlike a bare [`TermPtr`] (just an address), a `Handle` also carries the
//! `&'h HeapScope<'h>` it came from. That lets it own its node *responsibly*:
//! when a `Handle` is dropped without being consumed, it registers its address on
//! the heap's dropped-handle list (see [`HeapScope::register_dropped`]), so a
//! primitive that ignores an argument no longer has to erase it by hand — the
//! executor reclaims it later via `erase_dropped_handles`.

use std::mem::ManuallyDrop;

use super::term::Term;
use crate::vm::heap::{HeapScope, TermPtr, TermView};

/// An owning, heap-linked pointer handed to extensions. Consume it (force, open,
/// erase, or [`into_term_ptr`](Self::into_term_ptr)) to transfer ownership of the
/// node; otherwise dropping it queues the node for reclamation.
pub struct Handle<'h> {
    ptr: TermPtr<'h>,
    heap: &'h HeapScope<'h>,
}

impl<'h> Handle<'h> {
    /// Wrap an affine `ptr` together with the scope it belongs to.
    pub fn new(ptr: TermPtr<'h>, heap: &'h HeapScope<'h>) -> Self {
        Handle { ptr, heap }
    }

    /// The scope this handle borrows.
    pub fn heap(&self) -> &'h HeapScope<'h> {
        self.heap
    }

    /// Whether this handle names no slot (a null placeholder).
    pub fn is_null(&self) -> bool {
        self.ptr.is_null()
    }

    /// Consume the handle, yielding the underlying affine pointer. The drop
    /// bookkeeping is suppressed: ownership transfers to the returned `TermPtr`,
    /// so the node is *not* queued for reclamation.
    pub fn into_term_ptr(self) -> TermPtr<'h> {
        let me = ManuallyDrop::new(self);
        // SAFETY: `me.ptr` is read out exactly once and `me` is never dropped
        // (ManuallyDrop), so the address is neither double-owned nor registered.
        unsafe { std::ptr::read(&me.ptr) }
    }

    /// Read-only view of the node, for inspecting a leaf without consuming it.
    pub fn view(&self) -> TermView<'_, 'h> {
        self.heap.view(&self.ptr)
    }

    /// Consume the handle and unpack its node into an [`extension::Term`](Term),
    /// whose child pointers are themselves [`Handle`]s.
    pub fn open(self) -> Term<'h> {
        let heap = self.heap;
        let raw = heap.pull(self.into_term_ptr());
        Term::from_raw(raw, heap)
    }
}

impl<'h> Drop for Handle<'h> {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            // SAFETY: `self` is being dropped, so `self.ptr` is not observed again.
            // The bitwise copy transfers ownership to `register_dropped` (which
            // consumes it); the original left behind is a no-op to drop.
            let ptr = unsafe { std::ptr::read(&self.ptr) };
            self.heap.register_dropped(ptr);
        }
    }
}

/// Conversions shared by [`TermPtr`] and [`Handle`], so the executor's reduction
/// entry points can be generic over either: pass a `Handle`, get a `Handle` back;
/// pass a `TermPtr`, get a `TermPtr` back.
pub trait TermPtrLike<'h>: Sized {
    /// Consume `self` into a bare affine pointer.
    fn into_ptr(self) -> TermPtr<'h>;
    /// Reconstruct from a pointer and the scope it belongs to.
    fn from_ptr(ptr: TermPtr<'h>, heap: &'h HeapScope<'h>) -> Self;
}

impl<'h> TermPtrLike<'h> for TermPtr<'h> {
    fn into_ptr(self) -> TermPtr<'h> {
        self
    }
    fn from_ptr(ptr: TermPtr<'h>, _heap: &'h HeapScope<'h>) -> Self {
        ptr
    }
}

impl<'h> TermPtrLike<'h> for Handle<'h> {
    fn into_ptr(self) -> TermPtr<'h> {
        self.into_term_ptr()
    }
    fn from_ptr(ptr: TermPtr<'h>, heap: &'h HeapScope<'h>) -> Self {
        Handle::new(ptr, heap)
    }
}
