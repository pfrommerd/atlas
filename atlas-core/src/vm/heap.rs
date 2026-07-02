use super::term::{Brand, LabelId, Node, PrimId, Term, VariantId};
use crate::core::expr::{Expr, Pat, Value as CoreValue};
use crate::util::slab::{ShardedSlab, SharedKey, UniqueKey, UniqueSlot};
use crate::util::{AsyncMutex, LockKey, OwnedAsyncMutexGuard, RecursiveLock, U56};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::ops::Deref;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub struct Heap {
    nodes: ShardedSlab<Addr, Node>,
    // One entry per duplication projection wire (see `RefEntry`); `Ref` nodes
    // point here. The shared `DupCell` behind each wire is Arc'd, not slabbed.
    refs: ShardedSlab<Addr, Mutex<RefEntry>>,
    sups: ShardedSlab<Addr, SupCell>,
    /// Monotonic source of globally-unique per-wire duplication labels.
    label_counter: AtomicU64,
    /// Monotonic source of stable `DupCell` identities (readback naming).
    dup_ids: AtomicU64,
    /// Live `DupCell` count (each cell holds a clone and decrements on drop);
    /// exposed for leak assertions in tests.
    live_dups: Arc<AtomicU64>,
    values: ShardedSlab<Addr, Boxed>,
    // A pack is a constructor's fields (or a partial's gathered args), tagged with
    // an optional variant name. Each field entry names a first-class node in `nodes`.
    packs: ShardedSlab<Addr, Pack>,
    // First-class type objects (the `TypeInfo` behind an affine `TypePtr`). Each
    // `type { .. }` / builtin mints a fresh, owned entry (no sharing).
    types: ShardedSlab<Addr, TypeInfo>,
    // A bidirectional string interner backing variant names and label ids: the
    // slab maps an address back to its string, the map a string to its address.
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

/// A constructor's field array (or a partial application's gathered args), behind a
/// [`PackPtr`]. `name` tags the pack with a constructor variant (`Some` for a sum
/// constructor, `None` for a product constructor or a plain argument list). `data`
/// names the field/argument nodes in `nodes`.
#[derive(Debug, Clone)]
pub struct Pack {
    pub name: Option<VariantId>,
    pub data: Box<[Addr]>,
}

/// A first-class type object, behind an (affine) [`TypePtr`]. Either a product (an
/// ordered list of field types) or a sum (named variants). Field/argument types are
/// **owned, possibly-unevaluated** child nodes in `nodes` (so a type is a value with
/// lazy structure). `name` is set for builtin/named types and `None` for anonymous
/// `type { .. }` values.
#[derive(Debug, Clone)]
pub enum TypeInfo {
    Product {
        name: Option<Arc<str>>,
        fields: Vec<Addr>,
    },
    Sum {
        name: Option<Arc<str>>,
        variants: Vec<Variant>,
    },
}

/// One variant of a [`TypeInfo::Sum`]: a name plus its argument types (owned child
/// nodes). The argument count is the variant's arity.
#[derive(Debug, Clone)]
pub struct Variant {
    pub name: VariantId,
    pub args: Vec<Addr>,
}

impl TypeInfo {
    /// The display name (builtin/named types only).
    pub fn name(&self) -> Option<&Arc<str>> {
        match self {
            TypeInfo::Product { name, .. } | TypeInfo::Sum { name, .. } => name.as_ref(),
        }
    }
}

/// A lowering-time environment entry, indexed by de Bruijn level.
#[derive(Clone)]
enum LowerFrame {
    /// A lambda binder slot address.
    Binder(Addr),
    /// An erasing-lambda level (occupies an index, never referenced).
    Erased,
    /// A duplication, holding its fan. Each `Ref` to it registers a fresh
    /// projection wire (with a fresh label) on the fan.
    Dup { fan: FanHandle },
}

/// The cases of a `Mat` node. Each case pairs the node address of a pattern
/// *key* (a first-class value or [`Term::Type`](crate::vm::term::Term::Type) the
/// scrutinee is compared against, reduced to WHNF on demand) with the node
/// address of its branch lambda. `default` names the fallback branch's node.
#[derive(Debug, Clone)]
pub struct MatchData {
    pub cases: Vec<(Addr, Addr)>,
    pub default: Option<Addr>,
}

// The heap scope is the branded version
// of the heap and is used to ensure safety
// of pointers lying outside the heap.
#[repr(transparent)]
pub struct HeapScope<'h> {
    heap: Heap,
    _marker: Brand<'h>,
}

/// A duplication cell, stored behind a slab-level [`AsyncMutex`]. `value` is the
/// *address* of the node holding the value to duplicate (read lazily at force
/// time, so a dup over a lambda binder sees the substituted argument), or `None`
/// once the dup has fired. A dup has an arbitrary number of projections, each a
/// *wire* identified by its own globally-unique [`LabelId`]. The projection
/// branches contend for the lock: the first to acquire it reduces and fires the
/// value (filling every projection slot and setting `value = None`) *while
/// holding the lock*; the others block on `lock().await` and, on waking to a
/// `None`, read their own slot.
pub struct DupCell {
    /// Stable identity (readback dedup/naming); cells have no slab address.
    pub id: u64,
    /// The duplicand node, read lazily at force time; `None` once the dup has
    /// fired (or while it is being reduced, after [`HeapScope::dup_take_value`]).
    pub value: Option<Addr>,
    /// One `(label, refcell)` per projection wire, in fringe order (matching
    /// `shape`). The wire's fired result lives in its [`RefEntry`], not here.
    pub wires: Vec<(LabelId, Addr)>,
    /// Wires whose projection will still be taken (dropped wires excluded);
    /// purely diagnostic once the refcells hold the results — the cell itself
    /// is reclaimed when the last `FanHandle` clone drops.
    pub remaining: usize,
    /// Provenance tree over the wires, present only when combination has
    /// inserted groups (see [`LabelTree`]); `None` is the common flat case
    /// (every wire top-level). Invariant: the tree's leaf fringe equals the
    /// labels of `wires`, in order.
    pub shape: Option<Vec<LabelTree>>,
    /// Whether a sup or a sibling fan carrying this cell's wire labels may
    /// exist (true for every runtime-born cell: its labels were shared at the
    /// fire that created it). Combination may only mutate the *top-level*
    /// label set of an unpublished cell; splices into a published cell must
    /// preserve it by nesting a provenance group instead.
    pub published: bool,
    /// Handle on the heap's live-cell counter, decremented on drop.
    live: Arc<AtomicU64>,
}

impl Drop for DupCell {
    fn drop(&mut self) {
        self.live.fetch_sub(1, Ordering::Relaxed);
    }
}

/// The shared *fan*: one duplication's lock-and-data, `Arc`'d into the cell's
/// every wire (and any transient lock holder). Merging repoints wires to
/// another fan; the abandoned `DupCell` is reclaimed automatically when its
/// last `Arc` drops — there is no forwarding and nothing to free by hand.
pub type FanHandle = Arc<AsyncMutex<DupCell>>;

/// An owned guard over a locked fan (see [`HeapScope::lock_fan`]).
pub type DupGuard = OwnedAsyncMutexGuard<DupCell>;

/// One projection wire's entry; `Ref` nodes point here (`RefPtr` = this
/// entry's address + the wire's label). `fan` is the wire's current fan and
/// is repointed on merge (under both fan locks); `result` is the wire's fired
/// projection. All fields are guarded by the fan lock; the std mutex only
/// makes the individual field reads/writes safe (in particular reading `fan`
/// *before* holding the fan lock) and is never held across an await.
struct RefEntry {
    fan: FanHandle,
    result: SlotState,
}

/// A wire's projection slot: empty until the fan fires, `Filled` until the
/// wire projects it, or `Dropped` when the wire's `Ref` was erased before the
/// fire (the fill is then erased instead of delivered).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotState {
    Empty,
    Filled(Addr),
    Dropped,
}

/// What the caller must reclaim after [`HeapScope::dup_drop_wire`].
pub enum WireDrop<'h> {
    /// The fan had fired: this wire's already-made projection.
    Fill(TermPtr<'h>),
    /// The last live wire was dropped before the fire: the duplicand itself.
    Value(TermPtr<'h>),
    /// Nothing to reclaim (other live wires remain).
    Kept,
}

/// One entry in a fan's provenance tree. A `Leaf` is a live projection wire.
/// A `Group` stands where a consumed wire used to be: its children were
/// spliced in by combination and are owed copies of whatever that wire would
/// have received, so DUP-SUP matching must route the consumed label's part to
/// the whole group rather than wiring the children 1:1 (see `exec.rs`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LabelTree {
    Leaf(LabelId),
    Group { label: LabelId, children: Vec<LabelTree> },
}

impl LabelTree {
    /// The label this entry answers to at its own level (a group answers to
    /// the consumed wire it replaced).
    pub fn label(&self) -> LabelId {
        match self {
            LabelTree::Leaf(l) => *l,
            LabelTree::Group { label, .. } => *label,
        }
    }

    /// Append this subtree's leaf labels (the live wires) to `out`.
    pub fn leaves_into(&self, out: &mut Vec<LabelId>) {
        match self {
            LabelTree::Leaf(l) => out.push(*l),
            LabelTree::Group { children, .. } => {
                for c in children {
                    c.leaves_into(out);
                }
            }
        }
    }

    /// Append every label in this subtree — leaves *and* group tags — to `out`.
    pub fn all_labels_into(&self, out: &mut Vec<LabelId>) {
        match self {
            LabelTree::Leaf(l) => out.push(*l),
            LabelTree::Group { label, children } => {
                out.push(*label);
                for c in children {
                    c.all_labels_into(out);
                }
            }
        }
    }

    /// Replace the leaf `consumed` anywhere in `entries` with `wires`: wrapped
    /// as a group when the leaf sits at the top level (preserving the published
    /// top-level label set), spliced in flat when it sits inside a group (the
    /// group's children are unpublished, so copy-of-copy collapses). Returns
    /// whether the leaf was found.
    fn splice_wires(entries: &mut Vec<LabelTree>, consumed: LabelId, wires: &[LabelId], top: bool) -> bool {
        for i in 0..entries.len() {
            match &mut entries[i] {
                LabelTree::Leaf(l) if *l == consumed => {
                    let leaves = wires.iter().map(|l| LabelTree::Leaf(*l));
                    if top {
                        entries[i] = LabelTree::Group {
                            label: consumed,
                            children: leaves.collect(),
                        };
                    } else {
                        entries.splice(i..=i, leaves);
                    }
                    return true;
                }
                LabelTree::Group { children, .. } => {
                    if Self::splice_wires(children, consumed, wires, false) {
                        return true;
                    }
                }
                LabelTree::Leaf(_) => {}
            }
        }
        false
    }
}

/// A duplication fan shape: the flat leaf wires plus (rarely) the provenance
/// tree grouping them. `shape: None` means every wire is top-level.
#[derive(Clone, Debug)]
pub struct Fan {
    pub leaves: Vec<LabelId>,
    pub shape: Option<Vec<LabelTree>>,
}

impl Fan {
    pub fn flat(leaves: Vec<LabelId>) -> Self {
        Fan { leaves, shape: None }
    }

    /// The top-level entries: the explicit tree when grouped, else one leaf
    /// per wire.
    pub fn top(&self) -> Vec<LabelTree> {
        match &self.shape {
            Some(t) => t.clone(),
            None => self.leaves.iter().map(|l| LabelTree::Leaf(*l)).collect(),
        }
    }
}

/// A superposition cell: one part per wire, each keyed by its label. Surface
/// superpositions are binary; wider ones arise from duplicating a lambda (a
/// projection per dup wire) and from DUP-SUP commutation. `shape` mirrors the
/// provenance tree of the fan that spawned this sup (see [`DupCell::shape`]);
/// its leaf fringe equals the labels of `parts`, in order.
struct SupCell {
    parts: Vec<(LabelId, Addr)>,
    shape: Option<Vec<LabelTree>>,
}

// The heap only contains functions for branding.
// Any interaction with the heap itself should go through the HeapScope<'h>
impl Heap {
    pub fn new() -> Self {
        Heap {
            nodes: ShardedSlab::new(),
            refs: ShardedSlab::new(),
            sups: ShardedSlab::new(),
            label_counter: AtomicU64::new(1),
            dup_ids: AtomicU64::new(1),
            live_dups: Arc::new(AtomicU64::new(0)),
            values: ShardedSlab::new(),
            packs: ShardedSlab::new(),
            types: ShardedSlab::new(),
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

// A match table is read-only and referenced from many places, so it is a `SharedKey`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MatchPtr<'h>(SharedKey<'h, Addr>);

// A first-class type value is owned by exactly one node (a `Ctr`, a `Type`, or a
// type-expression slot), so it is affine. Reads forge a shared key from its addr.
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct TypePtr<'h>(UniqueKey<'h, Addr>);

// A pack is the field array owned by one Ctr/Partial node, so it is affine.
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

// one projection of a duplication. Many projections name the same cell, so it
// must be Copy -> a SharedKey. The `LabelId` selects this projection's wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RefPtr<'h>(SharedKey<'h, Addr>, LabelId);
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct SupPtr<'h>(UniqueKey<'h, Addr>);
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct ValuePtr<'h>(UniqueKey<'h, Addr>);
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct TracePtr<'h>(UniqueKey<'h, Addr>);

#[rustfmt::skip]
impl<'h> TypePtr<'h> {
    pub unsafe fn forge(addr: Addr) -> Self { unsafe { TypePtr(UniqueKey::forge(addr)) } }
    pub fn addr(&self) -> Addr { *self.0 }
    pub fn into_addr(self) -> Addr { self.0.into_raw() }
}
#[rustfmt::skip]
impl<'h> MatchPtr<'h> {
    pub unsafe fn forge(addr: Addr) -> Self { unsafe { MatchPtr(SharedKey::forge(addr)) } }
    pub fn addr(&self) -> Addr { self.0.raw() }
}
#[rustfmt::skip]
impl<'h> RefPtr<'h> {
    pub unsafe fn forge(addr: Addr, label: LabelId) -> Self { unsafe { RefPtr(SharedKey::forge(addr), label) } }
    pub fn addr(&self) -> Addr { self.0.raw() }
    pub fn label(&self) -> LabelId { self.1 }
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
        packs.get(&key).data[i]
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
    // String interning (variant names, labels)
    // ====================================================================

    /// Get-or-create a stable address for `s`. Equal strings share one address,
    /// so interned names (variant names, labels) can be compared by id.
    pub fn intern_name(&self, s: &str) -> Addr {
        let names = unsafe { self.heap.names.forge_brand() };
        let mut map = self.heap.interner.lock().unwrap();
        if let Some(&addr) = map.get(s) {
            return addr;
        }
        let arc: Arc<str> = Arc::from(s);
        let addr = names.insert(arc.clone()).raw();
        map.insert(arc, addr);
        addr
    }

    /// The string behind an interned address.
    pub fn name_of(&self, addr: Addr) -> &'h str {
        let names = unsafe { self.heap.names.forge_brand() };
        let key = unsafe { SharedKey::forge(addr) };
        names.get(&key).as_ref()
    }

    /// Intern a variant name into a [`VariantId`].
    pub fn intern_variant(&self, name: &str) -> VariantId {
        VariantId::from_u56(self.intern_name(name))
    }

    /// The display name of a variant id.
    pub fn variant_name(&self, id: VariantId) -> &'h str {
        self.name_of(id.addr())
    }

    // ====================================================================
    // Types (affine, owned values)
    // ====================================================================

    /// Allocate a fresh type value (affine). Each call mints a distinct identity;
    /// the [`TypeInfo`] owns its (possibly-unevaluated) sub-type child nodes.
    pub fn alloc_type(&self, info: TypeInfo) -> TypePtr<'h> {
        let types = unsafe { self.heap.types.forge_brand() };
        TypePtr(types.insert_unique(info))
    }

    /// The [`TypeInfo`] behind a [`TypePtr`] (shared read; does not consume it).
    pub fn type_info(&self, ptr: &TypePtr<'h>) -> &'h TypeInfo {
        self.type_info_at(ptr.addr())
    }

    /// The [`TypeInfo`] at a `types`-slab address (e.g. a `Ctr`'s `ty`).
    pub fn type_info_at(&self, addr: Addr) -> &'h TypeInfo {
        let types = unsafe { self.heap.types.forge_brand() };
        types.get(&unsafe { SharedKey::forge(addr) })
    }

    /// A type's display name (builtin/named types only).
    pub fn type_name(&self, addr: Addr) -> Option<Arc<str>> {
        self.type_info_at(addr).name().cloned()
    }

    /// Reclaim a type value, returning its [`TypeInfo`] so the caller can erase its
    /// child nodes.
    pub fn free_type(&self, ptr: TypePtr<'h>) -> TypeInfo {
        let types = unsafe { self.heap.types.forge_brand() };
        types.remove(ptr.0)
    }

    /// A fresh opaque named type (e.g. `Int`, `Float`, `Type`), used by `typeof`
    /// on primitive leaves. Proper builtin type definitions will come from a
    /// prelude; for now these are empty named products.
    pub fn builtin_type(&self, name: &str) -> TypePtr<'h> {
        self.alloc_type(TypeInfo::Product {
            name: Some(Arc::from(name)),
            fields: vec![],
        })
    }

    // ====================================================================
    // Packs (constructor fields / partial-application args)
    // ====================================================================

    /// Allocate a pack tagged with an optional constructor variant, from the
    /// field/argument node pointers (consuming them).
    pub fn alloc_pack(&self, name: Option<VariantId>, fields: Vec<TermPtr<'h>>) -> PackPtr<'h> {
        let packs = unsafe { self.heap.packs.forge_brand() };
        let data: Box<[Addr]> = fields.into_iter().map(|p| p.into_addr()).collect();
        PackPtr(packs.insert_unique(Pack { name, data }))
    }

    /// The pack's constructor variant tag (shared read).
    pub fn pack_name(&self, ptr: &PackPtr<'h>) -> Option<VariantId> {
        let packs = unsafe { self.heap.packs.forge_brand() };
        packs.get(&unsafe { SharedKey::forge(ptr.addr()) }).name
    }

    pub fn pack_len(&self, ptr: &PackPtr<'h>) -> usize {
        let packs = unsafe { self.heap.packs.forge_brand() };
        let key = unsafe { SharedKey::forge(ptr.addr()) };
        packs.get(&key).data.len()
    }

    /// Forge a pointer to the `i`th field of a pack.
    pub fn pack_field(&self, ptr: &PackPtr<'h>, i: usize) -> TermPtr<'h> {
        let packs = unsafe { self.heap.packs.forge_brand() };
        let key = unsafe { SharedKey::forge(ptr.addr()) };
        let addr = packs.get(&key).data[i];
        unsafe { TermPtr::forge(addr) }
    }

    /// Overwrite the `i`th field address of a pack in place.
    pub fn set_pack_field(&self, ptr: &PackPtr<'h>, i: usize, field: TermPtr<'h>) {
        let packs = unsafe { self.heap.packs.forge_brand() };
        let mut slot = packs.get_unique(unsafe { UniqueKey::forge(ptr.addr()) });
        slot.data[i] = field.into_addr();
        let _ = slot.finished();
    }

    /// Reclaim a pack, returning the [`Pack`] (variant tag + field addresses).
    pub fn free_pack(&self, ptr: PackPtr<'h>) -> Pack {
        let packs = unsafe { self.heap.packs.forge_brand() };
        packs.remove(ptr.0)
    }

    /// Reclaim a pack, returning an owned [`TermPtr`] for each field. Sound: the
    /// pack was the sole holder of each field address, so freeing it transfers
    /// ownership to exactly one pointer apiece (like [`free_sup`]).
    pub fn into_fields(&self, ptr: PackPtr<'h>) -> Vec<TermPtr<'h>> {
        self.free_pack(ptr)
            .data
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

    /// Allocate a flat superposition cell over its labelled parts (one wire each).
    pub fn alloc_sup_n(&self, parts: Vec<(LabelId, TermPtr<'h>)>) -> SupPtr<'h> {
        self.alloc_sup_shaped(parts, None)
    }

    /// Allocate a superposition cell carrying the provenance tree of the fan
    /// that spawned it. `shape`'s leaf fringe must equal the part labels in
    /// order; `None` is the flat case.
    pub fn alloc_sup_shaped(
        &self,
        parts: Vec<(LabelId, TermPtr<'h>)>,
        shape: Option<Vec<LabelTree>>,
    ) -> SupPtr<'h> {
        let sups = unsafe { self.heap.sups.forge_brand() };
        debug_assert!(shape.as_ref().is_none_or(|s| {
            let mut fringe = Vec::new();
            for t in s {
                t.leaves_into(&mut fringe);
            }
            fringe.iter().eq(parts.iter().map(|(l, _)| l))
        }));
        let parts = parts.into_iter().map(|(l, p)| (l, p.into_addr())).collect();
        SupPtr(sups.insert_unique(SupCell { parts, shape }))
    }

    /// The provenance tree of a superposition's parts (`None` = flat), without
    /// consuming it.
    pub fn sup_shape(&self, ptr: &SupPtr<'h>) -> Option<Vec<LabelTree>> {
        let sups = unsafe { self.heap.sups.forge_brand() };
        let key = unsafe { SharedKey::forge(ptr.addr()) };
        sups.get(&key).shape.clone()
    }

    /// The labels (wires) of a superposition, without consuming it.
    pub fn sup_labels(&self, ptr: &SupPtr<'h>) -> Vec<LabelId> {
        let sups = unsafe { self.heap.sups.forge_brand() };
        let key = unsafe { SharedKey::forge(ptr.addr()) };
        sups.get(&key).parts.iter().map(|(l, _)| *l).collect()
    }

    /// The number of superposed parts.
    pub fn sup_len(&self, ptr: &SupPtr<'h>) -> usize {
        let sups = unsafe { self.heap.sups.forge_brand() };
        let key = unsafe { SharedKey::forge(ptr.addr()) };
        sups.get(&key).parts.len()
    }

    /// The node address of one part of a superposition (by index).
    pub fn sup_part_addr(&self, ptr: &SupPtr<'h>, idx: usize) -> Addr {
        let sups = unsafe { self.heap.sups.forge_brand() };
        let key = unsafe { SharedKey::forge(ptr.addr()) };
        sups.get(&key).parts[idx].1
    }

    /// Overwrite one part's node address in place (its label is unchanged).
    pub fn set_sup_part(&self, ptr: &SupPtr<'h>, idx: usize, addr: Addr) {
        let sups = unsafe { self.heap.sups.forge_brand() };
        let mut slot = sups.get_unique(unsafe { UniqueKey::forge(ptr.addr()) });
        slot.parts[idx].1 = addr;
        let _ = slot.finished();
    }

    /// View one part of a superposition (by index), borrowed against the [`SupPtr`].
    pub fn view_sup_at<'r>(&self, ptr: &'r SupPtr<'h>, idx: usize) -> TermView<'r, 'h> {
        let addr = {
            let sups = unsafe { self.heap.sups.forge_brand() };
            let key = unsafe { SharedKey::forge(ptr.addr()) };
            sups.get(&key).parts[idx].1
        };
        self.view_addr(ptr, addr)
    }

    /// Reclaim a superposition cell, returning its labelled part pointers.
    pub fn free_sup(&self, ptr: SupPtr<'h>) -> Vec<(LabelId, TermPtr<'h>)> {
        let sups = unsafe { self.heap.sups.forge_brand() };
        let cell = sups.remove(ptr.0);
        cell.parts
            .into_iter()
            .map(|(l, a)| (l, unsafe { TermPtr::forge(a) }))
            .collect()
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

    /// Mint a fresh, globally-unique projection-wire [`LabelId`].
    pub fn fresh_label(&self) -> LabelId {
        let n = self.heap.label_counter.fetch_add(1, Ordering::Relaxed);
        LabelId::from_u56(U56::new(n))
    }

    /// The number of live [`DupCell`]s on this heap (leak assertions in tests).
    pub fn live_dup_cells(&self) -> u64 {
        self.heap.live_dups.load(Ordering::Relaxed)
    }

    /// Mint a fresh fan over the node at `value`, with no wires yet.
    fn new_dup_cell(&self, value: Addr, published: bool, shape: Option<Vec<LabelTree>>) -> FanHandle {
        let live = Arc::clone(&self.heap.live_dups);
        live.fetch_add(1, Ordering::Relaxed);
        Arc::new(AsyncMutex::new(DupCell {
            id: self.heap.dup_ids.fetch_add(1, Ordering::Relaxed),
            value: Some(value),
            wires: Vec::new(),
            remaining: 0,
            shape,
            published,
            live,
        }))
    }

    /// Allocate one projection-wire entry pointing at `fan`.
    fn new_wire(&self, fan: &FanHandle) -> Addr {
        let refs = unsafe { self.heap.refs.forge_brand() };
        refs.insert(Mutex::new(RefEntry {
            fan: Arc::clone(fan),
            result: SlotState::Empty,
        }))
        .raw()
    }

    /// Reclaim a projection-wire entry (dropping its fan `Arc`).
    fn free_wire(&self, rc: Addr) {
        let refs = unsafe { self.heap.refs.forge_brand() };
        let _ = refs.remove(unsafe { UniqueKey::forge(rc) });
    }

    /// Run `f` on a wire's entry (the std lock is held only for the call).
    fn with_wire<R>(&self, rc: Addr, f: impl FnOnce(&mut RefEntry) -> R) -> R {
        let refs = unsafe { self.heap.refs.forge_brand() };
        let key = unsafe { SharedKey::forge(rc) };
        f(&mut refs.get(&key).lock().unwrap())
    }

    /// The fan a projection wire currently belongs to (repointed by merges).
    pub fn fan_of(&self, rc: Addr) -> FanHandle {
        self.with_wire(rc, |e| Arc::clone(&e.fan))
    }

    /// Allocate a *runtime-born* fan over a concrete `val` (stored in a fresh
    /// node) with the given fan shape; runtime cells are `published` (their
    /// labels were shared at the fire or commute that created them). Returns
    /// one `(label, refcell)` wire per leaf, in fringe order.
    pub fn alloc_dup_fan(&self, val: Term<'h>, fan: &Fan) -> Vec<(LabelId, Addr)> {
        let value = self.alloc(val).into_addr();
        self.alloc_dup_at_fan(value, fan)
    }

    /// As [`alloc_dup_fan`], but reading the value (lazily) from the node at
    /// `value` (used by DUP-SUP peel, whose value is an existing part node).
    pub fn alloc_dup_at_fan(&self, value: Addr, fan: &Fan) -> Vec<(LabelId, Addr)> {
        let handle = self.new_dup_cell(value, true, fan.shape.clone());
        let wires: Vec<(LabelId, Addr)> = fan
            .leaves
            .iter()
            .map(|l| (*l, self.new_wire(&handle)))
            .collect();
        let mut guard = handle.try_lock().expect("fresh fan uncontended");
        guard.wires = wires.clone();
        guard.remaining = wires.len();
        wires
    }

    /// Allocate an empty *lowering-born* (unpublished) fan over the node at
    /// `value`. Wires are added by [`dup_register`] as they are discovered (at
    /// lowering, one per `Ref` occurrence).
    pub fn alloc_dup_at(&self, value: Addr) -> FanHandle {
        self.new_dup_cell(value, false, None)
    }

    /// Register a fresh projection wire (`label`) on the (uncontended) fan,
    /// returning the wire's refcell address (to pack into its `Ref` node).
    pub fn dup_register(&self, fan: &FanHandle, label: LabelId) -> Addr {
        let rc = self.new_wire(fan);
        let mut guard = fan.try_lock().expect("lowering fan uncontended");
        guard.wires.push((label, rc));
        guard.remaining += 1;
        rc
    }

    /// Make an auto-dup *use* of `value` (a REPL auto-dup local read site):
    /// allocate a fresh 2-wire fan over it, returning the node to splice at this
    /// occurrence and the node to keep as the local's value for the next use.
    /// Successive uses chain; combination flattens the chain lazily at force time.
    pub fn dup_use(&self, value: TermPtr<'h>) -> (TermPtr<'h>, TermPtr<'h>) {
        let fan = self.alloc_dup_at(value.into_addr());
        let mk = |label: LabelId| {
            let rc = self.dup_register(&fan, label);
            self.alloc(Term::Ref {
                ptr: unsafe { RefPtr::forge(rc, label) },
            })
        };
        let use_node = mk(self.fresh_label());
        let keep_node = mk(self.fresh_label());
        (use_node, keep_node)
    }

    /// Acquire the fan lock through one of its wires, under `key`. The lock is
    /// `Arc`-shared between the cell and all its wires, and a merge may repoint
    /// the wire to another fan while we wait — the `Arc` keeps the old mutex
    /// alive for exactly this case — so validate after acquiring and retry
    /// against the new fan. `Err(RecursiveLock)` means an *ancestor* of this
    /// reduction chain already holds the fan: waiting would self-deadlock (a
    /// cyclic value), so the caller treats the projection as stuck.
    pub async fn lock_fan(&self, rc: Addr, key: &LockKey) -> Result<DupGuard, RecursiveLock> {
        loop {
            let fan = self.fan_of(rc);
            let guard = fan.lock_arc(key).await?;
            if Arc::ptr_eq(&self.fan_of(rc), guard.mutex()) {
                return Ok(guard);
            }
        }
    }

    /// Keyless, non-blocking [`lock_fan`] (readback and erase).
    pub fn try_lock_fan(&self, rc: Addr) -> Option<DupGuard> {
        loop {
            let fan = self.fan_of(rc);
            let guard = fan.try_lock_arc()?;
            if Arc::ptr_eq(&self.fan_of(rc), guard.mutex()) {
                return Some(guard);
            }
        }
    }

    /// Take the fan's pending value as an owned pointer (the node to reduce),
    /// or `None` if it has already fired. Take-on-read: the cell's `value` is
    /// cleared, so it can never be forged into a second live pointer. If the dup
    /// turns out to be stuck, put it back with [`dup_restore_value`].
    pub fn dup_take_value(&self, guard: &mut DupGuard) -> Option<TermPtr<'h>> {
        guard
            .value
            .take()
            .map(|addr| unsafe { TermPtr::forge(addr) })
    }

    /// Put a still-stuck duplicand back into the cell (the inverse of
    /// [`dup_take_value`]), consuming the pointer.
    pub fn dup_restore_value(&self, guard: &mut DupGuard, value: TermPtr<'h>) {
        guard.value = Some(value.into_addr());
    }

    /// Merge (combination): absorb the `outer` fan's wires into `inner` in
    /// place of inner's consumed wire `consumed` (outer's duplicand was that
    /// wire's projection — its `Ref` node has been pulled by the caller). The
    /// outer fan must be unpublished (its labels are nobody else's business,
    /// so they may move freely). When `inner` is unpublished too the splice is
    /// flat; when `inner` is published its top-level label set must be
    /// preserved, so the wires nest as a provenance [`LabelTree`] group
    /// (top-level consumed wire) or join their enclosing group flat (nested
    /// consumed wire). Every moved wire's refcell is repointed at the inner
    /// fan; the emptied outer cell is reclaimed automatically when the guards
    /// and repointed `Arc`s drop.
    pub fn dup_merge(&self, inner: &mut DupGuard, consumed: LabelId, outer: &mut DupGuard) {
        debug_assert!(!outer.published, "combination never moves published wires");
        debug_assert!(outer.shape.is_none(), "unpublished cells are flat");
        debug_assert!(outer.value.is_none(), "outer duplicand consumed by the caller");
        let pos = inner
            .wires
            .iter()
            .position(|(l, _)| *l == consumed)
            .expect("combination: consumed wire missing in inner cell");
        let consumed_rc = inner.wires[pos].1;
        // Materialize the tree before mutating the wires (its fringe must
        // mirror the pre-splice labels); only published cells carry one.
        let mut shape = if inner.published {
            Some(inner.shape.take().unwrap_or_else(|| {
                inner.wires.iter().map(|(l, _)| LabelTree::Leaf(*l)).collect()
            }))
        } else {
            debug_assert!(inner.shape.is_none(), "unpublished cells are flat");
            None
        };
        let outer_wires = std::mem::take(&mut outer.wires);
        let added = outer_wires.len();
        if let Some(shape) = shape.as_mut() {
            let labels: Vec<LabelId> = outer_wires.iter().map(|(l, _)| *l).collect();
            let found = LabelTree::splice_wires(shape, consumed, &labels, true);
            debug_assert!(found, "combination: consumed wire missing in inner shape");
        }
        // Repoint the moved wires at the inner fan: their `Ref` nodes (and any
        // racer awaiting the outer lock) find the merged fan from now on.
        for (_, rc) in &outer_wires {
            self.with_wire(*rc, |e| e.fan = Arc::clone(inner.mutex()));
        }
        // Splice the wires positionally so the tree fringe stays aligned.
        inner.wires.splice(pos..=pos, outer_wires);
        inner.remaining = inner.remaining - 1 + added;
        inner.shape = shape;
        self.free_wire(consumed_rc);
        outer.remaining = 0;
    }

    /// Fire the fan: write each wire's projection into its refcell (consuming
    /// the pointers). Every wire must be supplied. Wires whose `Ref` was erased
    /// before the fire take no fill: their refcells are reclaimed here and
    /// their fills returned for the caller to erase.
    pub fn dup_fire(
        &self,
        guard: &mut DupGuard,
        fills: Vec<(LabelId, TermPtr<'h>)>,
    ) -> Vec<TermPtr<'h>> {
        let mut dropped = Vec::new();
        for (label, ptr) in fills {
            let idx = guard
                .wires
                .iter()
                .position(|(l, _)| *l == label)
                .expect("dup_fire: unknown wire label");
            let rc = guard.wires[idx].1;
            match self.with_wire(rc, |e| e.result) {
                SlotState::Empty => {
                    self.with_wire(rc, |e| e.result = SlotState::Filled(ptr.into_addr()));
                }
                SlotState::Dropped => {
                    guard.wires.remove(idx);
                    self.free_wire(rc);
                    dropped.push(ptr);
                }
                SlotState::Filled(_) => unreachable!("dup_fire: wire already filled"),
            }
        }
        dropped
    }

    /// Take this wire's fired projection, consuming the wire: its refcell is
    /// reclaimed and its entry removed from the fan. The `DupCell` itself is
    /// reclaimed automatically when the last wire (and any transient lock
    /// holder) drops its `Arc`.
    pub fn dup_project(&self, dp: RefPtr<'h>, guard: &mut DupGuard) -> Term<'h> {
        let rc = dp.addr();
        let idx = guard
            .wires
            .iter()
            .position(|(_, r)| *r == rc)
            .expect("dup_project: unknown wire");
        guard.wires.remove(idx);
        let taken = self.with_wire(rc, |e| std::mem::replace(&mut e.result, SlotState::Empty));
        let SlotState::Filled(addr) = taken else {
            panic!("dup projection not filled");
        };
        self.free_wire(rc);
        guard.remaining -= 1;
        self.pull(unsafe { TermPtr::forge(addr) })
    }

    /// Drop one projection wire without projecting it (its `Ref` node was
    /// erased rather than forced). Returns what the caller must erase in turn.
    pub fn dup_drop_wire(&self, guard: &mut DupGuard, rc: Addr) -> WireDrop<'h> {
        let idx = guard
            .wires
            .iter()
            .position(|(_, r)| *r == rc)
            .expect("dup_drop_wire: unknown wire");
        if guard.value.is_none() {
            // Fired: reclaim this wire's already-made projection.
            guard.wires.remove(idx);
            let taken = self.with_wire(rc, |e| std::mem::replace(&mut e.result, SlotState::Empty));
            let SlotState::Filled(addr) = taken else {
                panic!("dropped wire not filled");
            };
            self.free_wire(rc);
            guard.remaining -= 1;
            return WireDrop::Fill(unsafe { TermPtr::forge(addr) });
        }
        if !guard.published {
            // Unfired root: the label may simply disappear from the fan.
            debug_assert!(guard.shape.is_none(), "unpublished cells are flat");
            guard.wires.remove(idx);
            self.free_wire(rc);
        } else {
            // Unfired published fan: the label must survive for sup matching —
            // mark the wire dropped; its fill is erased at fire time.
            self.with_wire(rc, |e| e.result = SlotState::Dropped);
        }
        guard.remaining -= 1;
        if guard.remaining == 0 {
            // No live wire remains: reclaim every (dropped) refcell and hand
            // the unforced duplicand back for erasure.
            for (_, rc) in std::mem::take(&mut guard.wires) {
                self.free_wire(rc);
            }
            let value = guard.value.take().expect("unfired fan has a value");
            return WireDrop::Value(unsafe { TermPtr::forge(value) });
        }
        WireDrop::Kept
    }

    /// The fan's pending duplicand address, or `None` once fired. Uncontended
    /// peek (readback / readiness checks).
    pub fn dup_value(&self, dp: &RefPtr<'h>) -> Option<Addr> {
        self.try_lock_fan(dp.addr())
            .expect("readback fan uncontended")
            .value
    }

    /// The fan behind a projection (readback identity + label/shape access).
    pub fn dup_fan(&self, dp: &RefPtr<'h>) -> FanHandle {
        self.fan_of(dp.addr())
    }

    /// A fan's stable identity (uncontended; readback dedup).
    pub fn fan_id(&self, fan: &FanHandle) -> u64 {
        fan.try_lock().expect("readback fan uncontended").id
    }

    /// The labels of a fan's projection wires (uncontended; readback).
    pub fn fan_labels(&self, fan: &FanHandle) -> Vec<LabelId> {
        fan.try_lock()
            .expect("readback fan uncontended")
            .wires
            .iter()
            .map(|(l, _)| *l)
            .collect()
    }

    /// The provenance tree of a fan's wires (`None` = flat; readback).
    pub fn fan_shape(&self, fan: &FanHandle) -> Option<Vec<LabelTree>> {
        fan.try_lock()
            .expect("readback fan uncontended")
            .shape
            .clone()
    }

    /// View what a dup projection reads back as, borrowed against the [`RefPtr`]:
    /// the value being duplicated if unfired, else this wire's resolved slot.
    /// Returns the node address (for naming a bare `Var`) alongside the view.
    pub fn view_dup<'r>(&self, dp: &'r RefPtr<'h>) -> (Addr, TermView<'r, 'h>) {
        let addr = {
            let guard = self
                .try_lock_fan(dp.addr())
                .expect("readback fan uncontended");
            guard.value.or_else(|| {
                match self.with_wire(dp.addr(), |e| e.result) {
                    SlotState::Filled(addr) => Some(addr),
                    _ => None,
                }
            })
        }
        .expect("dup projection has no readback value");
        (addr, self.view_addr(dp, addr))
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
        local: &mut dyn FnMut(&str) -> Option<TermPtr<'h>>,
    ) -> Result<TermPtr<'h>, String> {
        let mut env: Vec<LowerFrame> = Vec::new();
        self.lower_env(expr, &mut env, resolve, local)
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
        local: &mut dyn FnMut(&str) -> Option<TermPtr<'h>>,
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
            Expr::Ref(db) => match self.frame(env, db.0)? {
                LowerFrame::Dup { fan } => {
                    // Each occurrence is a distinct projection wire: register a
                    // fresh label on the fan and name its refcell from this node.
                    let label = self.fresh_label();
                    let rc = self.dup_register(&fan, label);
                    self.alloc(Term::Ref {
                        ptr: unsafe { RefPtr::forge(rc, label) },
                    })
                }
                _ => return Err("Ref does not refer to a duplication".into()),
            },
            Expr::App { func, arg } => {
                let f = self.lower_env(func, env, resolve, local)?;
                let a = self.lower_env(arg, env, resolve, local)?;
                self.alloc(Term::App { func: f, arg: a })
            }
            Expr::Bop { op, left, right } => {
                let l = self.lower_env(left, env, resolve, local)?;
                let r = self.lower_env(right, env, resolve, local)?;
                self.alloc(Term::Bop {
                    op: *op,
                    lhs: l,
                    rhs: r,
                })
            }
            Expr::Uop { op, val } => {
                let v = self.lower_env(val, env, resolve, local)?;
                self.alloc(Term::Uop { op: *op, val: v })
            }
            Expr::Lam { body } => {
                let binder = self.alloc(Term::Var).into_addr();
                env.push(LowerFrame::Binder(binder));
                let b = self.lower_env(body, env, resolve, local);
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
                let b = self.lower_env(body, env, resolve, local);
                env.pop();
                self.alloc(Term::Use { body: b? })
            }
            Expr::Sup { left, right } => {
                // Each part is its own wire with a fresh, globally-unique label.
                let a = self.lower_env(left, env, resolve, local)?;
                let b = self.lower_env(right, env, resolve, local)?;
                let ptr = self.alloc_sup_n(vec![(self.fresh_label(), a), (self.fresh_label(), b)]);
                self.alloc(Term::Sup { ptr })
            }
            Expr::Dup { val, body } => {
                // The value is referenced lazily by node address, so a value that
                // is a lambda binder (auto-dup, `\&x -> …`) reads its substituted
                // argument at force time rather than a stale copy. The cell starts
                // with no wires; each `Ref` in the body registers one.
                let v = self.lower_env(val, env, resolve, local)?;
                let fan = self.alloc_dup_at(v.into_addr());
                env.push(LowerFrame::Dup { fan });
                let b = self.lower_env(body, env, resolve, local);
                env.pop();
                // A `Dup` expr installs the cell and lowers to its body; the
                // projections reference the cell via the env.
                b?
            }
            Expr::Ctr { ty, variant } => {
                let t = self.lower_env(ty, env, resolve, local)?;
                self.alloc(Term::Ctr {
                    ty: t,
                    variant: variant.as_ref().map(|n| self.intern_variant(n)),
                })
            }
            Expr::TypeDef { kind } => {
                // A `type { .. }` lowers directly to a fresh type *value* whose
                // field/arg sub-types are owned, *unevaluated* child nodes.
                let info = match kind {
                    crate::core::expr::TypeDefKind::Product(members) => {
                        let mut fields = Vec::with_capacity(members.len());
                        for m in members {
                            fields.push(self.lower_env(m, env, resolve, local)?.into_addr());
                        }
                        TypeInfo::Product { name: None, fields }
                    }
                    crate::core::expr::TypeDefKind::Sum(variants) => {
                        let mut vs = Vec::with_capacity(variants.len());
                        for (name, args) in variants {
                            let mut aa = Vec::with_capacity(args.len());
                            for a in args {
                                aa.push(self.lower_env(a, env, resolve, local)?.into_addr());
                            }
                            vs.push(Variant {
                                name: self.intern_variant(name),
                                args: aa,
                            });
                        }
                        TypeInfo::Sum {
                            name: None,
                            variants: vs,
                        }
                    }
                };
                self.alloc(Term::Type(self.alloc_type(info)))
            }
            Expr::Mat { cases, default } => {
                let mut compiled = Vec::with_capacity(cases.len());
                for (pat, body) in cases {
                    // The key is a first-class node: a `VarId` for a constructor
                    // (variant) pattern, or the literal value for a value pattern.
                    // Both are compared (after WHNF) against the scrutinee at fire
                    // time.
                    let key = match pat {
                        Pat::Ctr(name) => self.alloc(Term::VarId(self.intern_variant(name))),
                        Pat::Val(v) => self.lower_value(v),
                    };
                    let branch = self.lower_env(body, env, resolve, local)?;
                    compiled.push((key.into_addr(), branch.into_addr()));
                }
                let default = match default {
                    Some(d) => Some(self.lower_env(d, env, resolve, local)?.into_addr()),
                    None => None,
                };
                let matches = self.alloc_match(MatchData {
                    cases: compiled,
                    default,
                });
                self.alloc(Term::Mat { matches })
            }
            Expr::Free(name) => match local(name) {
                Some(ptr) => ptr,
                None => return Err(format!("unbound variable `{name}`")),
            },
        })
    }

    fn frame(&self, env: &[LowerFrame], i: u64) -> Result<LowerFrame, String> {
        let i = i as usize;
        if i >= env.len() {
            return Err(format!("de Bruijn index {i} out of range"));
        }
        Ok(env[env.len() - 1 - i].clone())
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
    fn variant_interning_dedups_by_address() {
        let heap = Heap::new();
        heap.with(|h| {
            let a = h.intern_variant("Cons");
            let b = h.intern_variant("Cons");
            let c = h.intern_variant("Nil");
            assert_eq!(a.addr(), b.addr());
            assert_ne!(a.addr(), c.addr());
            assert_eq!(h.variant_name(a), "Cons");
            assert_eq!(h.variant_name(c), "Nil");
        });
    }

    #[test]
    fn pack_carries_variant_tag() {
        let heap = Heap::new();
        heap.with(|h| {
            let cons = h.intern_variant("Cons");
            let f0 = h.alloc(Term::Int(1));
            let tagged = h.alloc_pack(Some(cons), vec![f0]);
            assert_eq!(h.pack_name(&tagged), Some(cons));
            let untagged = h.alloc_pack(None, vec![]);
            assert_eq!(h.pack_name(&untagged), None);
            let _ = h.free_pack(tagged);
            let _ = h.free_pack(untagged);
        });
    }

    #[test]
    fn fresh_types_have_distinct_addresses() {
        let heap = Heap::new();
        heap.with(|h| {
            // Affine types are never shared: each builtin call mints a fresh value.
            let a = h.builtin_type("Int");
            let b = h.builtin_type("Int");
            assert_ne!(a.addr(), b.addr());
            assert_eq!(h.type_name(a.addr()).as_deref(), Some("Int"));
            let _ = h.free_type(a);
            let _ = h.free_type(b);
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
            let pack = h.alloc_pack(None, vec![f0, f1]);
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
    fn sup_parts_round_trip() {
        let heap = Heap::new();
        heap.with(|h| {
            let a = h.alloc(Term::Int(7));
            let b = h.alloc(Term::Int(8));
            let (la, lb) = (h.fresh_label(), h.fresh_label());
            let s = h.alloc_sup_n(vec![(la, a), (lb, b)]);
            assert_eq!(h.sup_len(&s), 2);
            assert_eq!(*h.view_sup_at(&s, 0), Term::Int(7));
            assert_eq!(*h.view_sup_at(&s, 1), Term::Int(8));
            assert_eq!(h.sup_labels(&s), vec![la, lb]);
            let _ = h.free_sup(s);
        });
    }
}
