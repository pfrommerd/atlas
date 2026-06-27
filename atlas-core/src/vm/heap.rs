use super::term::{Brand, LabelId, Node, PrimId, Term};
use crate::core::expr::{Expr, Label, Pat, Value as CoreValue};
use crate::util::slab::{ShardedSlab, SharedKey, UniqueKey, UniqueSlot};
use crate::util::{SingleMutex, SingleMutexGuard, U56};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::ops::Deref;
use std::sync::{Arc, Mutex};

pub struct Heap {
    nodes: ShardedSlab<Addr, Node>,
    dups: ShardedSlab<Addr, SingleMutex<DupCell>>,
    sups: ShardedSlab<Addr, SupCell>,
    values: ShardedSlab<Addr, Boxed>,
    // A pack is a contiguous run of node addresses (constructor fields / match
    // branches). Each entry names a first-class node in `nodes`.
    packs: ShardedSlab<Addr, Box<[Addr]>>,
    // Interned constructor names: equal names share one slab entry (and thus one
    // `NamePtr` address), so pattern matching can compare names by address.
    names: ShardedSlab<Addr, Arc<str>>,
    interner: Mutex<HashMap<Arc<str>, Addr>>,
    // Match tables, referenced by a shared `MatchPtr`.
    matches: ShardedSlab<Addr, MatchData>,
    // Addresses of nodes whose owning `extension::Handle` was dropped rather than
    // explicitly consumed. Reclaimed by `Executor::erase_dropped_handles`.
    dropped: Mutex<Vec<Addr>>,
}

/// A boxed heap value, referenced by a [`ValuePtr`]: payloads too large to pack
/// into a node word (strings, byte arrays).
#[derive(Debug, Clone)]
pub enum Boxed {
    Str(Arc<str>),
    Bytes(Arc<[u8]>),
}

/// A pattern-match key: either a constructor (by interned name address) or a
/// primitive value literal (any [`CoreValue`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PatKey {
    Ctr(Addr),
    Val(CoreValue),
}

/// A lowering-time environment entry, indexed by de Bruijn level.
#[derive(Debug, Clone, Copy)]
enum LowerFrame {
    /// A lambda binder slot address.
    Binder(Addr),
    /// An erasing-lambda level (occupies an index, never referenced).
    Erased,
    /// A duplication: its cell key and label.
    Dup { key: Addr, label: LabelId },
}

/// The cases of a `Mat` node. Each value is an index into the match's branch
/// pack (the `Term::Mat`'s `branches: PackPtr`).
#[derive(Debug, Clone)]
pub struct MatchData {
    pub cases: Vec<(PatKey, usize)>,
    pub default: Option<usize>,
}

// The heap scope is the branded version
// of the heap and is used to ensure safety
// of pointers lying outside the heap.
#[repr(transparent)]
pub struct HeapScope<'h> {
    heap: Heap,
    _marker: Brand<'h>,
}

/// A duplication cell, stored behind a slab-level [`SingleMutex`]. `value` is the
/// *address* of the node holding the value to duplicate (read lazily at force
/// time, so a dup over a lambda binder sees the substituted argument), or `None`
/// once the dup has fired. The two projection branches contend for the lock: the
/// first to acquire it reduces and fires the value (writing the other projection
/// slot and setting `value = None`) *while holding the lock*; the second blocks
/// on `lock().await` and, on waking to a `None`, reads its projection. `left`/
/// `right` are the `Dp0`/`Dp1` projection slot node addresses.
pub struct DupCell {
    /// The duplicand node, read lazily at force time; `None` once the dup has
    /// fired (or while it is being reduced, after [`HeapScope::dup_take_value`]).
    pub value: Option<Addr>,
    /// The resolved projection slots, each `None` until the dup fires and again
    /// once that side has been projected. Take-on-read keeps each address from
    /// being forged into more than one live pointer.
    pub left: Option<Addr>,
    pub right: Option<Addr>,
}

struct SupCell {
    left: Addr,
    right: Addr,
}

// The heap only contains functions for branding.
// Any interaction with the heap itself should go through the HeapScope<'h>
impl Heap {
    pub fn new() -> Self {
        Heap {
            nodes: ShardedSlab::new(),
            dups: ShardedSlab::new(),
            sups: ShardedSlab::new(),
            values: ShardedSlab::new(),
            packs: ShardedSlab::new(),
            names: ShardedSlab::new(),
            interner: Mutex::new(HashMap::new()),
            matches: ShardedSlab::new(),
            dropped: Mutex::new(Vec::new()),
        }
    }

    pub unsafe fn forge_brand<'h>(&self) -> &HeapScope<'h> {
        unsafe { &*(self as *const Self as *const HeapScope<'h>) }
    }
    /// Safely brand this slab for the duration of `f`. The branded reference's
    /// lifetime is tied to the brand itself (`&'h HeapScope<'h>`), so handles that
    /// borrow the scope (see [`crate::extension::Handle`]) can carry a single
    /// lifetime.
    pub fn with<R>(&self, f: impl for<'h> FnOnce(&'h HeapScope<'h>) -> R) -> R {
        f(unsafe { self.forge_brand() })
    }
}

impl Default for Heap {
    fn default() -> Self {
        Heap::new()
    }
}

pub type Addr = U56;

/// An affine pointer to a heap node. A `TermPtr` is normally a live `UniqueKey`,
/// but it can also be *null* (`None`): a placeholder that names no slot. Null
/// pointers are safe to construct and hold (e.g. the [`Spine`] swaps one in for a
/// continuation whose node it is reducing elsewhere); they must never be
/// dereferenced, and the dereferencing accessors (`addr`/[`HeapScope::term`]/
/// [`HeapScope::remove`]) panic rather than risk UB if one ever is.
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct TermPtr<'h>(Option<UniqueKey<'h, Addr>>);
pub struct TermSlot<'h>(UniqueSlot<'h, Addr, Node>);

/// A read-only, borrowed view of the [`Term`] at a node (see [`HeapScope::view`]).
/// Owns the unpacked term but ties its lifetime to a borrow of the owner that
/// keeps the node live, and only derefs to `&Term`, so the affine child pointers
/// it holds cannot escape to be duplicated or reclaimed.
pub struct TermView<'r, 'h> {
    term: Term<'h>,
    _owner: PhantomData<&'r ()>,
}

impl<'r, 'h> Deref for TermView<'r, 'h> {
    type Target = Term<'h>;
    fn deref(&self) -> &Term<'h> {
        &self.term
    }
}

#[rustfmt::skip]
impl<'h> TermPtr<'h> {
    pub unsafe fn forge(addr: Addr) -> TermPtr<'h> {
        unsafe { TermPtr(Some(UniqueKey::forge(addr))) }
    }
    /// A null placeholder pointer naming no slot (see [`TermPtr`]). Safe.
    pub fn null() -> TermPtr<'h> { TermPtr(None) }
    pub fn is_null(&self) -> bool { self.0.is_none() }
    fn key(self) -> UniqueKey<'h, Addr> { self.0.expect("dereferenced a null TermPtr") }
    pub fn addr(&self) -> Addr { **self.0.as_ref().expect("dereferenced a null TermPtr") }
    pub fn into_addr(self) -> Addr { self.key().into_raw() }
}
impl<'h> TermSlot<'h> {
    // SAFETY: The caller must ensure that the term
    // originally contained in this slot is no longer reachable,
    // as otherwise reading from the TermPtr<'h> again may lead to duplicate
    // Terms being created, which is unsound as they should be affine.
    pub unsafe fn unchanged(self) -> TermPtr<'h> {
        TermPtr(Some(self.0.finished()))
    }
    pub fn finished(mut self, term: Term<'h>) -> TermPtr<'h> {
        self.0.update(term.pack());
        TermPtr(Some(self.0.finished()))
    }
}

// Shared (Copy) pointers: a constructor name and a match table are read-only and
// referenced from many places, so they wrap a `SharedKey`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NamePtr<'h>(SharedKey<'h, Addr>);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MatchPtr<'h>(SharedKey<'h, Addr>);

// A pack is the field array owned by one Ctr/Mat node, so it is affine.
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct PackPtr<'h>(UniqueKey<'h, Addr>);

// A lambda owns its binder slot (affine) and its body. The body is held as a raw
// `Addr` -- *not* an affine `TermPtr` -- so the body cannot be reached until the
// binder has been substituted. `HeapScope::substitute` is the only way to mint a
// `TermPtr` for the body, and it overrides the binder's `Var` in the process.
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct BodyPtr<'h> {
    binder: UniqueKey<'h, Addr>,
    body: Addr,
}

// An affine handle to a lambda's binder slot, held while its body is reached
// without substituting (see `open_body`/`fresh_binder`). Round-trips back into a
// `BodyPtr` via `close_body`, or is written through via `fill_binder`.
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct BinderHandle<'h>(UniqueKey<'h, Addr>);

// one side of a duplication. Both projections (Dp0/Dp1) name the same cell, so it
// must be Copy -> a SharedKey. The bool selects the projection side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DupPtr<'h>(SharedKey<'h, Addr>, bool);
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct SupPtr<'h>(UniqueKey<'h, Addr>);
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct ValuePtr<'h>(UniqueKey<'h, Addr>);
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct TracePtr<'h>(UniqueKey<'h, Addr>);

#[rustfmt::skip]
impl<'h> NamePtr<'h> {
    pub unsafe fn forge(addr: Addr) -> Self { unsafe { NamePtr(SharedKey::forge(addr)) } }
    pub fn addr(&self) -> Addr { self.0.raw() }
}
#[rustfmt::skip]
impl<'h> MatchPtr<'h> {
    pub unsafe fn forge(addr: Addr) -> Self { unsafe { MatchPtr(SharedKey::forge(addr)) } }
    pub fn addr(&self) -> Addr { self.0.raw() }
}
#[rustfmt::skip]
impl<'h> DupPtr<'h> {
    pub unsafe fn forge(addr: Addr, side: bool) -> Self { unsafe { DupPtr(SharedKey::forge(addr), side) } }
    pub fn addr(&self) -> Addr { self.0.raw() }
    pub fn side(&self) -> bool { self.1 }
}
#[rustfmt::skip]
impl<'h> PackPtr<'h> {
    pub unsafe fn forge(addr: Addr) -> Self { unsafe { PackPtr(UniqueKey::forge(addr)) } }
    pub fn addr(&self) -> Addr { *self.0 }
    pub fn into_addr(self) -> Addr { self.0.into_raw() }
}
#[rustfmt::skip]
impl<'h> SupPtr<'h> {
    pub unsafe fn forge(addr: Addr) -> Self { unsafe { SupPtr(UniqueKey::forge(addr)) } }
    pub fn addr(&self) -> Addr { *self.0 }
    pub fn into_addr(self) -> Addr { self.0.into_raw() }
}
#[rustfmt::skip]
impl<'h> ValuePtr<'h> {
    pub unsafe fn forge(addr: Addr) -> Self { unsafe { ValuePtr(UniqueKey::forge(addr)) } }
    pub fn addr(&self) -> Addr { *self.0 }
    pub fn into_addr(self) -> Addr { self.0.into_raw() }
}
#[rustfmt::skip]
impl<'h> TracePtr<'h> {
    pub unsafe fn forge(addr: Addr) -> Self { unsafe { TracePtr(UniqueKey::forge(addr)) } }
    pub fn addr(&self) -> Addr { *self.0 }
    pub fn into_addr(self) -> Addr { self.0.into_raw() }
}
#[rustfmt::skip]
impl<'h> BodyPtr<'h> {
    pub unsafe fn forge(binder: Addr, body: Addr) -> Self {
        unsafe { BodyPtr { binder: UniqueKey::forge(binder), body } }
    }
    pub fn binder_addr(&self) -> Addr { *self.binder }
    pub fn body_addr(&self) -> Addr { self.body }
}

impl<'h> HeapScope<'h> {
    // ====================================================================
    // Nodes
    // ====================================================================

    /// Allocate a node holding `term`, returning an affine pointer to it.
    pub fn alloc(&self, term: Term<'h>) -> TermPtr<'h> {
        let nodes = unsafe { self.heap.nodes.forge_brand() };
        TermPtr(Some(nodes.insert_unique(term.pack())))
    }

    // Consumes the pointer, returns the slot for updating
    // the value, as well as the unpacked term.
    pub fn term(&self, ptr: TermPtr<'h>) -> (TermSlot<'h>, Term<'h>) {
        let nodes = unsafe { self.heap.nodes.forge_brand() };
        let slot = nodes.get_unique(ptr.key());
        let term = unsafe { slot.deref().unpack() };
        (TermSlot(slot), term)
    }

    /// Read-only view of the node at `addr`, its lifetime tied to a borrow of some
    /// live owner (a pointer that keeps the node reachable). This is the sole place
    /// that forges a read handle; everything outside the heap reaches nodes only
    /// through the safe `view*` wrappers below, which lend `&Term` and never hand
    /// back an owned (affine) pointer to duplicate or reclaim.
    fn view_addr<'r, T: ?Sized>(&self, _owner: &'r T, addr: Addr) -> TermView<'r, 'h> {
        let nodes = unsafe { self.heap.nodes.forge_brand() };
        let key = unsafe { SharedKey::forge(addr) };
        let node = *nodes.get(&key);
        TermView {
            term: unsafe { node.unpack() },
            _owner: PhantomData,
        }
    }

    /// Read-only view of a node, borrowed against the [`TermPtr`] that names it.
    /// The safe shared-read path for traversal/readback (see [`TermView`]).
    pub fn view<'r>(&self, ptr: &'r TermPtr<'h>) -> TermView<'r, 'h> {
        self.view_addr(ptr, ptr.addr())
    }

    /// Read-only view of the node at `addr`, borrowed against the heap scope. For
    /// readback only (the whole reachable graph is live while reduction is idle),
    /// e.g. rendering a hoisted dup binding's value by its address.
    pub fn view_at<'r>(&'r self, addr: Addr) -> TermView<'r, 'h> {
        self.view_addr(self, addr)
    }

    /// View a lambda's body, borrowed against the owning [`BodyPtr`].
    pub fn view_body<'r>(&self, body: &'r BodyPtr<'h>) -> TermView<'r, 'h> {
        self.view_addr(body, body.body_addr())
    }

    /// The node address of a pack's `i`th field.
    pub fn pack_addr(&self, ptr: &PackPtr<'h>, i: usize) -> Addr {
        let packs = unsafe { self.heap.packs.forge_brand() };
        let key = unsafe { SharedKey::forge(ptr.addr()) };
        packs.get(&key)[i]
    }

    /// View a pack's `i`th field, borrowed against the owning [`PackPtr`].
    pub fn view_pack<'r>(&self, ptr: &'r PackPtr<'h>, i: usize) -> TermView<'r, 'h> {
        self.view_addr(ptr, self.pack_addr(ptr, i))
    }

    /// Reclaim a node, returning its raw packed contents.
    pub fn remove(&self, ptr: TermPtr<'h>) -> Node {
        let nodes = unsafe { self.heap.nodes.forge_brand() };
        nodes.remove(ptr.key())
    }

    /// Reclaim the node behind a held slot.
    pub fn remove_slot(&self, slot: TermSlot<'h>) -> Node {
        let nodes = unsafe { self.heap.nodes.forge_brand() };
        nodes.remove(slot.0.finished())
    }

    /// Consume a node: free its slot and return its unpacked term.
    pub fn pull(&self, ptr: TermPtr<'h>) -> Term<'h> {
        // SAFETY: the node was a live `nodes` slot for scope `'h`.
        unsafe { self.remove(ptr).unpack() }
    }

    // ====================================================================
    // Dropped-handle bookkeeping
    // ====================================================================

    /// Record a node whose owning [`crate::extension::Handle`] was dropped without
    /// being consumed. The pointer is stored (as a raw address) until
    /// [`Executor::erase_dropped_handles`](crate::vm::exec::Executor::erase_dropped_handles)
    /// reclaims it; consuming the `TermPtr` here ensures the address is queued
    /// exactly once.
    pub fn register_dropped(&self, ptr: TermPtr<'h>) {
        self.heap.dropped.lock().unwrap().push(ptr.into_addr());
    }

    /// Drain the pending dropped handles as owned pointers. Sound: each address
    /// was queued by consuming the unique `TermPtr` that owned it (in
    /// `register_dropped`), so forging it back yields exactly one live pointer.
    pub fn take_dropped(&self) -> Vec<TermPtr<'h>> {
        std::mem::take(&mut *self.heap.dropped.lock().unwrap())
            .into_iter()
            .map(|addr| unsafe { TermPtr::forge(addr) })
            .collect()
    }

    // ====================================================================
    // Names (interned)
    // ====================================================================

    /// Intern a constructor name. Equal names return the same address, so
    /// `NamePtr`s can be compared by address during pattern matching.
    pub fn intern_name(&self, name: &str) -> NamePtr<'h> {
        let names = unsafe { self.heap.names.forge_brand() };
        let mut map = self.heap.interner.lock().unwrap();
        if let Some(&addr) = map.get(name) {
            return unsafe { NamePtr::forge(addr) };
        }
        let arc: Arc<str> = Arc::from(name);
        let key = names.insert(arc.clone());
        map.insert(arc, key.raw());
        NamePtr(key)
    }

    pub fn name(&self, ptr: &NamePtr<'h>) -> &'h str {
        let names = unsafe { self.heap.names.forge_brand() };
        names.get(&ptr.0).as_ref()
    }

    /// Resolve an interned name by its address (e.g. a [`PatKey::Ctr`] key).
    pub fn name_at(&self, addr: Addr) -> &'h str {
        self.name(&unsafe { NamePtr::forge(addr) })
    }

    // ====================================================================
    // Packs (constructor fields / match branches)
    // ====================================================================

    /// Allocate a pack from the field node pointers (consuming them).
    pub fn alloc_pack(&self, fields: Vec<TermPtr<'h>>) -> PackPtr<'h> {
        let packs = unsafe { self.heap.packs.forge_brand() };
        let addrs: Box<[Addr]> = fields.into_iter().map(|p| p.into_addr()).collect();
        PackPtr(packs.insert_unique(addrs))
    }

    pub fn pack_len(&self, ptr: &PackPtr<'h>) -> usize {
        let packs = unsafe { self.heap.packs.forge_brand() };
        let key = unsafe { SharedKey::forge(ptr.addr()) };
        packs.get(&key).len()
    }

    /// Forge a pointer to the `i`th field of a pack.
    pub fn pack_field(&self, ptr: &PackPtr<'h>, i: usize) -> TermPtr<'h> {
        let packs = unsafe { self.heap.packs.forge_brand() };
        let key = unsafe { SharedKey::forge(ptr.addr()) };
        let addr = packs.get(&key)[i];
        unsafe { TermPtr::forge(addr) }
    }

    /// Overwrite the `i`th field address of a pack in place.
    pub fn set_pack_field(&self, ptr: &PackPtr<'h>, i: usize, field: TermPtr<'h>) {
        let packs = unsafe { self.heap.packs.forge_brand() };
        let mut slot = packs.get_unique(unsafe { UniqueKey::forge(ptr.addr()) });
        slot[i] = field.into_addr();
        let _ = slot.finished();
    }

    /// Reclaim a pack, returning the field addresses it held.
    pub fn free_pack(&self, ptr: PackPtr<'h>) -> Box<[Addr]> {
        let packs = unsafe { self.heap.packs.forge_brand() };
        packs.remove(ptr.0)
    }

    /// Reclaim a pack, returning an owned [`TermPtr`] for each field. Sound: the
    /// pack was the sole holder of each field address, so freeing it transfers
    /// ownership to exactly one pointer apiece (like [`free_sup`]).
    pub fn into_fields(&self, ptr: PackPtr<'h>) -> Vec<TermPtr<'h>> {
        self.free_pack(ptr)
            .iter()
            .map(|&a| unsafe { TermPtr::forge(a) })
            .collect()
    }

    // ====================================================================
    // Match tables
    // ====================================================================

    pub fn alloc_match(&self, data: MatchData) -> MatchPtr<'h> {
        let matches = unsafe { self.heap.matches.forge_brand() };
        MatchPtr(matches.insert(data))
    }

    pub fn match_data(&self, ptr: &MatchPtr<'h>) -> &'h MatchData {
        let matches = unsafe { self.heap.matches.forge_brand() };
        matches.get(&ptr.0)
    }

    /// Reclaim a match table.
    pub fn free_match(&self, ptr: MatchPtr<'h>) {
        let matches = unsafe { self.heap.matches.forge_brand() };
        let _ = matches.remove(unsafe { UniqueKey::forge(ptr.addr()) });
    }

    // ====================================================================
    // Boxed values
    // ====================================================================

    pub fn value(&self, value: Boxed) -> ValuePtr<'h> {
        let values = unsafe { self.heap.values.forge_brand() };
        ValuePtr(values.insert_unique(value))
    }

    pub fn value_get(&self, ptr: &ValuePtr<'h>) -> &'h Boxed {
        let values = unsafe { self.heap.values.forge_brand() };
        let key = unsafe { SharedKey::forge(ptr.addr()) };
        &values.get(&key)
    }

    /// Duplicate a boxed value: clone its payload (a cheap `Arc` bump) into a
    /// fresh entry, so each projection owns an affine handle.
    pub fn value_dup(&self, ptr: &ValuePtr<'h>) -> ValuePtr<'h> {
        let cloned = self.value_get(ptr).clone();
        self.value(cloned)
    }

    pub fn value_drop(&self, ptr: ValuePtr<'h>) {
        let values = unsafe { self.heap.values.forge_brand() };
        let _ = values.remove(ptr.0);
    }

    // ====================================================================
    // Superpositions
    // ====================================================================

    /// Allocate a superposition cell over two argument node pointers.
    pub fn sup(&self, a: TermPtr<'h>, b: TermPtr<'h>) -> SupPtr<'h> {
        let sups = unsafe { self.heap.sups.forge_brand() };
        SupPtr(sups.insert_unique(SupCell {
            left: a.into_addr(),
            right: b.into_addr(),
        }))
    }

    /// Overwrite a superposition cell's two argument addresses in place.
    pub fn set_sup_args(&self, ptr: &SupPtr<'h>, a: TermPtr<'h>, b: TermPtr<'h>) {
        let sups = unsafe { self.heap.sups.forge_brand() };
        let mut slot = sups.get_unique(unsafe { UniqueKey::forge(ptr.addr()) });
        slot.left = a.into_addr();
        slot.right = b.into_addr();
        let _ = slot.finished();
    }

    pub fn sup_args(&self, ptr: &SupPtr<'h>) -> (TermPtr<'h>, TermPtr<'h>) {
        let sups = unsafe { self.heap.sups.forge_brand() };
        let key = unsafe { SharedKey::forge(ptr.addr()) };
        let cell = sups.get(&key);
        unsafe { (TermPtr::forge(cell.left), TermPtr::forge(cell.right)) }
    }

    /// The two argument node addresses of a superposition.
    pub fn sup_addrs(&self, ptr: &SupPtr<'h>) -> (Addr, Addr) {
        let sups = unsafe { self.heap.sups.forge_brand() };
        let key = unsafe { SharedKey::forge(ptr.addr()) };
        let cell = sups.get(&key);
        (cell.left, cell.right)
    }

    /// View one argument of a superposition, borrowed against the [`SupPtr`].
    pub fn view_sup<'r>(&self, ptr: &'r SupPtr<'h>, left: bool) -> TermView<'r, 'h> {
        let (l, r) = self.sup_addrs(ptr);
        self.view_addr(ptr, if left { l } else { r })
    }

    /// Reclaim a superposition cell, returning its two argument pointers.
    pub fn free_sup(&self, ptr: SupPtr<'h>) -> (TermPtr<'h>, TermPtr<'h>) {
        let sups = unsafe { self.heap.sups.forge_brand() };
        let cell = sups.remove(ptr.0);
        unsafe { (TermPtr::forge(cell.left), TermPtr::forge(cell.right)) }
    }

    // ====================================================================
    // Lambda binders
    // ====================================================================

    /// Overwrite a node in place with `term` (the slot stays allocated).
    fn write_node(&self, addr: Addr, term: Term<'h>) {
        let nodes = unsafe { self.heap.nodes.forge_brand() };
        let mut slot = nodes.get_unique(unsafe { UniqueKey::forge(addr) });
        slot.update(term.pack());
        let _ = slot.finished();
    }

    /// APP-LAM's substitution step: write `arg` into the lambda's binder slot
    /// (overriding its `Var`), then mint and return the body pointer. This is the
    /// only way to obtain the body `TermPtr`.
    pub fn substitute(&self, body: BodyPtr<'h>, arg: Term<'h>) -> TermPtr<'h> {
        let nodes = unsafe { self.heap.nodes.forge_brand() };
        let mut slot = nodes.get_unique(body.binder);
        slot.update(arg.pack());
        let _ = slot.finished();
        unsafe { TermPtr::forge(body.body) }
    }

    /// Open a lambda body *without* substituting: split the affine [`BodyPtr`] into
    /// its binder handle and a pointer to the body node. Pair with [`close_body`]
    /// (rebind a new body) or just reduce/erase the body. Sound: consumes the
    /// `BodyPtr`, handing out each of its two owned halves exactly once.
    pub fn open_body(&self, body: BodyPtr<'h>) -> (BinderHandle<'h>, TermPtr<'h>) {
        (BinderHandle(body.binder), unsafe {
            TermPtr::forge(body.body)
        })
    }

    /// Reassemble a [`BodyPtr`] from a binder handle and a (new) body pointer.
    pub fn close_body(&self, binder: BinderHandle<'h>, body: TermPtr<'h>) -> BodyPtr<'h> {
        BodyPtr {
            binder: binder.0,
            body: body.into_addr(),
        }
    }

    /// Consume a whole lambda body, yielding just the body pointer (for erasure:
    /// the binder `Var` is reclaimed via the body's unique variable occurrence).
    pub fn into_body(&self, body: BodyPtr<'h>) -> TermPtr<'h> {
        unsafe { TermPtr::forge(body.body) }
    }

    /// Allocate a fresh lambda binder: a `Var` node, returned both as a binder
    /// handle (to install into a [`BodyPtr`]) and as its single occurrence pointer.
    pub fn fresh_binder(&self) -> (BinderHandle<'h>, TermPtr<'h>) {
        let occ = self.alloc(Term::Var);
        let binder = unsafe { UniqueKey::forge(occ.addr()) };
        (BinderHandle(binder), occ)
    }

    /// Overwrite a binder slot in place with `term` (like [`substitute`], but the
    /// binder is held as a [`BinderHandle`] and no body pointer is produced).
    pub fn fill_binder(&self, binder: BinderHandle<'h>, term: Term<'h>) {
        self.write_node(*binder.0, term);
    }

    // ====================================================================
    // Duplications
    // ====================================================================

    /// Allocate a duplication over a concrete `val` (stored in a fresh node),
    /// returning the two projections (`Dp0` = side `true`, `Dp1` = side `false`).
    pub fn alloc_dup(&self, val: Term<'h>) -> (DupPtr<'h>, DupPtr<'h>) {
        let value = self.alloc(val).into_addr();
        self.alloc_dup_at(value)
    }

    /// Allocate a duplication whose value is read (lazily) from the node at
    /// `value` — e.g. a lambda binder that will later be substituted. The
    /// projection slots are empty until the dup fires (see [`dup_fire`]).
    pub fn alloc_dup_at(&self, value: Addr) -> (DupPtr<'h>, DupPtr<'h>) {
        let dups = unsafe { self.heap.dups.forge_brand() };
        let key = dups.insert(SingleMutex::new(DupCell {
            value: Some(value),
            left: None,
            right: None,
        }));
        (DupPtr(key, true), DupPtr(key, false))
    }

    /// Acquire the duplication cell's lock (blocking the other branch until it is
    /// released). The caller inspects `value`: `Some` ⇒ this branch must reduce
    /// and fire it (holding the lock throughout); `None` ⇒ already fired, read the
    /// projection slot. See [`DupCell`].
    pub async fn dup_lock(&self, dp: DupPtr<'h>) -> SingleMutexGuard<'h, DupCell> {
        let dups = unsafe { self.heap.dups.forge_brand() };
        let key = unsafe { SharedKey::forge(dp.addr()) };
        dups.get(&key).lock().await
    }

    /// Take the dup cell's pending value as an owned pointer (the node to reduce),
    /// or `None` if it has already fired. Take-on-read: the cell's `value` is
    /// cleared, so it can never be forged into a second live pointer. If the dup
    /// turns out to be stuck, put it back with [`dup_restore_value`].
    pub fn dup_take_value(&self, guard: &mut SingleMutexGuard<'h, DupCell>) -> Option<TermPtr<'h>> {
        guard.value.take().map(|addr| unsafe { TermPtr::forge(addr) })
    }

    /// Put a still-stuck duplicand back into the cell (the inverse of
    /// [`dup_take_value`]), consuming the pointer.
    pub fn dup_restore_value(&self, guard: &mut SingleMutexGuard<'h, DupCell>, value: TermPtr<'h>) {
        guard.value = Some(value.into_addr());
    }

    /// Fire the cell: install the two resolved projection nodes (consuming their
    /// pointers). Each side is read out exactly once by [`dup_project`].
    pub fn dup_fire(
        &self,
        guard: &mut SingleMutexGuard<'h, DupCell>,
        left: TermPtr<'h>,
        right: TermPtr<'h>,
    ) {
        guard.left = Some(left.into_addr());
        guard.right = Some(right.into_addr());
    }

    /// Take this projection's resolved slot (after the cell has fired), returning
    /// its term. Take-on-read empties the slot so its address cannot be forged
    /// twice.
    pub fn dup_project(&self, dp: DupPtr<'h>, guard: &mut SingleMutexGuard<'h, DupCell>) -> Term<'h> {
        let addr = if dp.side() {
            guard.left.take()
        } else {
            guard.right.take()
        }
        .expect("dup projection already taken");
        self.pull(unsafe { TermPtr::forge(addr) })
    }

    /// Read a dup cell's `(value, left, right)` without consuming or firing it.
    /// For readback only; assumes the cell is uncontended (reduction is idle).
    pub fn dup_peek(&self, dp: &DupPtr<'h>) -> (Option<Addr>, Option<Addr>, Option<Addr>) {
        let dups = unsafe { self.heap.dups.forge_brand() };
        let key = unsafe { SharedKey::forge(dp.addr()) };
        let guard = dups
            .get(&key)
            .try_lock()
            .expect("dup cell uncontended at readback");
        (guard.value, guard.left, guard.right)
    }

    /// View what a dup projection reads back as, borrowed against the [`DupPtr`]:
    /// the value being duplicated if unfired, else this side's resolved slot.
    /// Returns the node address (for naming a bare `Var`) alongside the view.
    pub fn view_dup<'r>(&self, dp: &'r DupPtr<'h>) -> (Addr, TermView<'r, 'h>) {
        let (value, left, right) = self.dup_peek(dp);
        let addr = value
            .or(if dp.side() { left } else { right })
            .expect("dup projection has no readback value");
        (addr, self.view_addr(dp, addr))
    }

    /// Reclaim a fired, fully-projected dup cell (called by the second/loser
    /// projection after it has read its substitution): both projection nodes have
    /// been pulled by their own sides, so only the cell entry remains.
    pub fn free_dup(&self, dp: DupPtr<'h>) {
        let dups = unsafe { self.heap.dups.forge_brand() };
        let _ = dups.remove(unsafe { UniqueKey::forge(dp.addr()) });
    }

    // ====================================================================
    // Lowering: desugared core `Expr` -> heap term graph
    // ====================================================================

    /// Lower a desugared [`Expr`] into a heap term, returning a pointer to its
    /// root node. Source-level auto-duplication of a variable (`\&x -> …`, whose
    /// `Dup` value is the lambda binder) needs a lazy dup value and is not yet
    /// supported; dups created during reduction (all with concrete values) are.
    pub fn lower(
        &self,
        expr: &Expr,
        resolve: &dyn Fn(&str) -> Option<PrimId>,
    ) -> Result<TermPtr<'h>, String> {
        let mut env: Vec<LowerFrame> = Vec::new();
        self.lower_env(expr, &mut env, resolve)
    }

    /// Lower a label to its [`LabelId`]. Every label id lives in the name-interner
    /// address space, so equal `Named` labels share an id (needed for DUP-SUP
    /// annihilation) and the `&` prefix keeps labels disjoint from ctr names. An
    /// `Auto` label is generated fresh here, made globally unique via `unique` (the
    /// owning dup cell's address) and kept disjoint from any `Named` label by the
    /// `#` separator (illegal in source identifiers).
    fn lower_label(&self, label: &Label, unique: Addr) -> LabelId {
        let s = match label {
            Label::Named(s) => format!("&{s}"),
            Label::Auto => format!("&auto#{}", unique.to_u64()),
        };
        LabelId::from_u56(self.intern_name(&s).addr())
    }

    /// Lower a builtin [`CoreValue`] into a heap term: scalars become value
    /// leaves; strings and byte arrays become boxed heap [`Boxed`] values.
    fn lower_value(&self, v: &CoreValue) -> TermPtr<'h> {
        match v {
            CoreValue::Int(n) => self.alloc(Term::Int(*n)),
            CoreValue::Float(x) => self.alloc(Term::Float(*x)),
            CoreValue::Char(c) => self.alloc(Term::Char(*c)),
            CoreValue::Bool(b) => self.alloc(Term::Bool(*b)),
            CoreValue::Str(s) => {
                let v = self.value(Boxed::Str(Arc::from(s.as_str())));
                self.alloc(Term::Box(v))
            }
            CoreValue::Bytes(b) => {
                let v = self.value(Boxed::Bytes(Arc::from(b.as_slice())));
                self.alloc(Term::Box(v))
            }
        }
    }

    fn lower_env(
        &self,
        expr: &Expr,
        env: &mut Vec<LowerFrame>,
        resolve: &dyn Fn(&str) -> Option<PrimId>,
    ) -> Result<TermPtr<'h>, String> {
        Ok(match expr {
            Expr::Value(v) => self.lower_value(v),
            Expr::Wld => self.alloc(Term::Wld),
            Expr::Era => self.alloc(Term::Wld),
            Expr::Pri(name) => match resolve(name) {
                Some(id) => self.alloc(Term::Pri(id)),
                None => return Err(format!("unknown primitive %{name}")),
            },
            Expr::Var(db) => match self.frame(env, db.0)? {
                LowerFrame::Binder(addr) => unsafe { TermPtr::forge(addr) },
                _ => return Err("variable does not refer to a lambda binder".into()),
            },
            Expr::Dp0(db) => match self.frame(env, db.0)? {
                LowerFrame::Dup { key, label } => self.alloc(Term::Dup {
                    label,
                    ptr: unsafe { DupPtr::forge(key, true) },
                }),
                _ => return Err("Dp0 does not refer to a duplication".into()),
            },
            Expr::Dp1(db) => match self.frame(env, db.0)? {
                LowerFrame::Dup { key, label } => self.alloc(Term::Dup {
                    label,
                    ptr: unsafe { DupPtr::forge(key, false) },
                }),
                _ => return Err("Dp1 does not refer to a duplication".into()),
            },
            Expr::App { func, arg } => {
                let f = self.lower_env(func, env, resolve)?;
                let a = self.lower_env(arg, env, resolve)?;
                self.alloc(Term::App { func: f, arg: a })
            }
            Expr::Bop { op, left, right } => {
                let l = self.lower_env(left, env, resolve)?;
                let r = self.lower_env(right, env, resolve)?;
                self.alloc(Term::Bop {
                    op: *op,
                    lhs: l,
                    rhs: r,
                })
            }
            Expr::Uop { op, val } => {
                let v = self.lower_env(val, env, resolve)?;
                self.alloc(Term::Uop { op: *op, val: v })
            }
            Expr::Lam { body } => {
                let binder = self.alloc(Term::Var).into_addr();
                env.push(LowerFrame::Binder(binder));
                let b = self.lower_env(body, env, resolve);
                env.pop();
                let body_addr = b?.into_addr();
                self.alloc(Term::Lam {
                    body: unsafe { BodyPtr::forge(binder, body_addr) },
                })
            }
            Expr::Use { body } => {
                // An erasing lambda still occupies a de Bruijn level (kept aligned
                // with `desugar`), but nothing references it.
                env.push(LowerFrame::Erased);
                let b = self.lower_env(body, env, resolve);
                env.pop();
                self.alloc(Term::Use { body: b? })
            }
            Expr::Sup { label, left, right } => {
                let a = self.lower_env(left, env, resolve)?;
                let b = self.lower_env(right, env, resolve)?;
                // Sup labels are always `Named` (auto labels only arise on dups), so
                // the uniqueness source is unused here.
                let label = self.lower_label(label, a.addr());
                let ptr = self.sup(a, b);
                self.alloc(Term::Sup { label, ptr })
            }
            Expr::Dup { label, val, body } => {
                // The value is referenced lazily by node address, so a value that
                // is a lambda binder (auto-dup, `\&x -> …`) reads its substituted
                // argument at force time rather than a stale copy.
                let v = self.lower_env(val, env, resolve)?;
                let (dp0, _dp1) = self.alloc_dup_at(v.into_addr());
                // The cell address makes an `Auto` label unique to this dup.
                let label = self.lower_label(label, dp0.addr());
                env.push(LowerFrame::Dup {
                    key: dp0.addr(),
                    label,
                });
                let b = self.lower_env(body, env, resolve);
                env.pop();
                // A `Dup` expr installs the cell and lowers to its body; the
                // projections reference the cell via the env.
                b?
            }
            Expr::Ctr { name, args } => {
                let nm = self.intern_name(name);
                let mut fields = Vec::with_capacity(args.len());
                for a in args {
                    fields.push(self.lower_env(a, env, resolve)?);
                }
                let arity = fields.len() as u8;
                let pack = self.alloc_pack(fields);
                self.alloc(Term::Ctr {
                    name: nm,
                    arity,
                    values: pack,
                })
            }
            Expr::Mat { cases, default } => {
                let mut branches = Vec::new();
                let mut compiled = Vec::with_capacity(cases.len());
                for (pat, body) in cases {
                    let key = match pat {
                        Pat::Ctr(name) => PatKey::Ctr(self.intern_name(name).addr()),
                        Pat::Val(v) => PatKey::Val(v.clone()),
                    };
                    let idx = branches.len();
                    branches.push(self.lower_env(body, env, resolve)?);
                    compiled.push((key, idx));
                }
                let default = match default {
                    Some(d) => {
                        let idx = branches.len();
                        branches.push(self.lower_env(d, env, resolve)?);
                        Some(idx)
                    }
                    None => None,
                };
                let branches = self.alloc_pack(branches);
                let matches = self.alloc_match(MatchData {
                    cases: compiled,
                    default,
                });
                self.alloc(Term::Mat { matches, branches })
            }
            Expr::Ref(name) => {
                return Err(format!("references (@{name}) are not supported yet"));
            }
        })
    }

    fn frame(&self, env: &[LowerFrame], i: u64) -> Result<LowerFrame, String> {
        let i = i as usize;
        if i >= env.len() {
            return Err(format!("de Bruijn index {i} out of range"));
        }
        Ok(env[env.len() - 1 - i])
    }
}

// Contains the spine of a reduction.
//
// The spine is a stack of (slot, term) frames built while descending toward the
// head of a reduction. Each frame is a parent term (today always an `App`) whose
// *spine continuation* -- the child currently being reduced -- has been swapped
// for a null [`TermPtr`]. Holding the parent with a null continuation means the
// frame never aliases the live child handle: while a frame sits on the stack, the
// only owner of its child node is the slot being reduced. `push` performs the
// swap and hands back the displaced child; `unwind` reverses it, minting the
// child pointer from the reduced slot's `finished()` and plugging it back into
// the null hole. The backing `Vec` is private with no indexed access, so two
// affine slots can never be exposed at once. The `Spine` contains no `unsafe`.
pub struct Spine<'h> {
    terms: Vec<(TermSlot<'h>, Term<'h>)>,
}

impl<'h> Spine<'h> {
    pub fn new() -> Self {
        Spine { terms: Vec::new() }
    }

    /// Push `term` as a continuation frame and descend into its spine child,
    /// returning the displaced child pointer. The child slot of the stored frame
    /// is replaced with a null placeholder, so the frame holds no live alias.
    /// (Only `App` carries a spine continuation; pushing anything else is a bug.)
    pub fn push(&mut self, slot: TermSlot<'h>, term: Term<'h>) -> TermPtr<'h> {
        match term {
            Term::App { func, arg } => {
                self.terms.push((
                    slot,
                    Term::App {
                        func: TermPtr::null(),
                        arg,
                    },
                ));
                func
            }
            other => panic!("pushed a non-spine term onto the Spine: {other:?}"),
        }
    }

    /// Re-store an (already nulled) frame as-is, e.g. after inspecting or replacing
    /// its argument. `term`'s continuation is the null pointer obtained from `pop`.
    pub fn repush(&mut self, slot: TermSlot<'h>, term: Term<'h>) {
        self.terms.push((slot, term));
    }

    pub fn pop(&mut self) -> Option<(TermSlot<'h>, Term<'h>)> {
        self.terms.pop()
    }

    /// Read the innermost term without removing it (e.g. to branch on its kind).
    /// The continuation slot reads back null.
    pub fn peek(&self) -> Option<&Term<'h>> {
        self.terms.last().map(|(_, term)| term)
    }

    /// Finalize the current head `(slot, term)` and restore its parent: write the
    /// head back into its slot (minting the child pointer), pop the parent frame,
    /// and plug that pointer into the parent's null continuation. Returns
    /// `Err(root)` when the spine is empty (the head is the final result).
    pub fn unwind(
        &mut self,
        slot: TermSlot<'h>,
        term: Term<'h>,
    ) -> Result<(TermSlot<'h>, Term<'h>), TermPtr<'h>> {
        let child = slot.finished(term);
        match self.terms.pop() {
            None => Err(child),
            Some((pslot, Term::App { func, arg })) => {
                debug_assert!(func.is_null(), "spine frame continuation was not null");
                let _ = func;
                Ok((pslot, Term::App { func: child, arg }))
            }
            Some((_, other)) => unreachable!("non-App spine frame: {other:?}"),
        }
    }

    pub fn len(&self) -> usize {
        self.terms.len()
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }
}

impl<'h> Default for Spine<'h> {
    fn default() -> Self {
        Spine::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_interning_dedups_by_address() {
        let heap = Heap::new();
        heap.with(|h| {
            let a = h.intern_name("Con");
            let b = h.intern_name("Con");
            let c = h.intern_name("Nil");
            assert_eq!(a.addr(), b.addr());
            assert_ne!(a.addr(), c.addr());
            assert_eq!(h.name(&a), "Con");
            assert_eq!(h.name(&c), "Nil");
        });
    }

    #[test]
    fn alloc_view_round_trip() {
        let heap = Heap::new();
        heap.with(|h| {
            let p = h.alloc(Term::Int(42));
            assert_eq!(*h.view(&p), Term::Int(42));
            let (_slot, term) = h.term(p);
            assert_eq!(term, Term::Int(42));
        });
    }

    #[test]
    fn pack_field_access() {
        let heap = Heap::new();
        heap.with(|h| {
            let f0 = h.alloc(Term::Int(1));
            let f1 = h.alloc(Term::Int(2));
            let pack = h.alloc_pack(vec![f0, f1]);
            assert_eq!(h.pack_len(&pack), 2);
            assert_eq!(*h.view_pack(&pack, 0), Term::Int(1));
            assert_eq!(*h.view_pack(&pack, 1), Term::Int(2));
            let _ = h.free_pack(pack);
        });
    }

    #[test]
    fn value_dup_gives_fresh_entry() {
        let heap = Heap::new();
        heap.with(|h| {
            let v = h.value(Boxed::Str(Arc::from("hi")));
            match h.value_get(&v) {
                Boxed::Str(s) => assert_eq!(&**s, "hi"),
                _ => panic!("expected Str"),
            }
            let v2 = h.value_dup(&v);
            assert_ne!(v.addr(), v2.addr());
            h.value_drop(v);
            h.value_drop(v2);
        });
    }

    #[test]
    fn sup_args_round_trip() {
        let heap = Heap::new();
        heap.with(|h| {
            let a = h.alloc(Term::Int(7));
            let b = h.alloc(Term::Int(8));
            let s = h.sup(a, b);
            assert_eq!(*h.view_sup(&s, true), Term::Int(7));
            assert_eq!(*h.view_sup(&s, false), Term::Int(8));
            let _ = h.free_sup(s);
        });
    }
}
