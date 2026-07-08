use super::term::{Brand, LabelId, Node, PrimId, Term, VariantId};
use crate::core::expr::{Expr, Pat, Value as CoreValue};
use crate::util::slab::{ShardedSlab, SharedKey, UniqueKey, UniqueSlot};
use crate::util::{SingleMutex, SingleMutexGuard, U56};
use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;
use std::ops::Deref;
use std::sync::{Arc, Mutex};

pub struct Heap {
    nodes: ShardedSlab<Addr, Node>,
    dups: ShardedSlab<Addr, DupEntry>,
    sups: ShardedSlab<Addr, SupCell>,
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
    labels: Mutex<HashMap<LabelId, LabelState>>,
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
#[derive(Debug, Clone, Copy)]
enum LowerFrame {
    /// A lambda binder slot address.
    Binder(Addr),
    /// An erasing-lambda level (occupies an index, never referenced).
    Erased,
    /// A binary duplication: its cell key, shared label, and next side to assign.
    Dup { key: Addr, label: LabelId, refs: u8 },
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

/// A duplication cell: two locks with distinct roles.
///
/// * `eval` is the async rendezvous for the two projection branches. The first
///   to acquire it takes the duplicand `value` and reduces it *while holding
///   the lock*; the second blocks on `lock().await` and, on waking to a `None`
///   value, reads its resolved slot out of `meta`. Only the two projection
///   forcers ever `lock().await` it (preserving the [`SingleMutex`]
///   two-contender invariant); every other path uses `try_lock`, which never
///   queues a waker.
/// * `meta` is a **sync leaf lock** guarding the per-side drop flags and the
///   resolved projection slots. Its critical sections are a handful of
///   loads/stores: it is never held across an `.await`, and nothing else is
///   acquired while holding it (in particular, a drop can be recorded while
///   the winner is mid-reduction holding `eval`). The only nesting is
///   eval → meta (the winner publishing its result / checking drop flags).
pub struct DupEntry {
    meta: Mutex<DupMeta>,
    eval: SingleMutex<DupEval>,
}

/// Drop flags and projection parent nodes, behind [`DupEntry`]'s meta lock.
struct DupMeta {
    /// Per-side drop flags ([`DupMeta::side_index`]).
    dropped: [bool; 2],
    /// Set when the winner rewrote the non-forced projection parent node.
    fired: bool,
    /// The heap nodes currently containing the `Dp0` / `Dp1` projections.
    /// Firing rewrites these parent nodes directly instead of storing resolved
    /// values in the dup cell.
    parents: [Addr; 2],
    label: Option<LabelId>,
}

impl DupMeta {
    fn side_index(side: bool) -> usize {
        if side { 0 } else { 1 }
    }
    fn parent(&self, side: bool) -> Addr {
        self.parents[Self::side_index(side)]
    }
}

/// The duplicand, behind [`DupEntry`]'s eval lock (held by the winner across
/// the whole duplicand reduction).
pub struct DupEval {
    /// The duplicand node, read lazily at force time (so a dup over a lambda
    /// binder sees the substituted argument); `None` once the dup has fired
    /// (or while it is being reduced, after [`HeapScope::dup_take_value`]).
    pub value: Option<Addr>,
}

/// What [`HeapScope::dup_drop_side`] tells `erase` to do next.
pub enum DupDrop<'h> {
    /// The drop was handled by rewriting the surviving parent (or was a stale
    /// fired projection). There is no subtree left for the caller to erase.
    Recorded { dead: Vec<TermPtr<'h>> },
    /// This drop made the cell dead: it has been freed, and the caller must
    /// erase the returned subtree (the unfired duplicand, or this side's
    /// already-fired slot).
    Reclaim(TermPtr<'h>),
}

struct SupCell {
    left: Addr,
    right: Addr,
    label: Option<LabelId>,
}

#[derive(Default)]
struct LabelState {
    dups: HashSet<Addr>,
    sups: HashMap<Addr, Addr>,
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
            types: ShardedSlab::new(),
            names: ShardedSlab::new(),
            interner: Mutex::new(HashMap::new()),
            matches: ShardedSlab::new(),
            labels: Mutex::new(HashMap::new()),
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

/// Which heap arena a raw [`Addr`] names, for read-only enumeration and
/// debugging (see [`HeapScope::arena_len`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArenaKind {
    Nodes,
    Dups,
    Sups,
    Values,
    Packs,
    Types,
    Names,
    Matches,
}

impl ArenaKind {
    pub const ALL: [ArenaKind; 8] = [
        ArenaKind::Nodes,
        ArenaKind::Dups,
        ArenaKind::Sups,
        ArenaKind::Values,
        ArenaKind::Packs,
        ArenaKind::Types,
        ArenaKind::Names,
        ArenaKind::Matches,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ArenaKind::Nodes => "nodes",
            ArenaKind::Dups => "dups",
            ArenaKind::Sups => "sups",
            ArenaKind::Values => "values",
            ArenaKind::Packs => "packs",
            ArenaKind::Types => "types",
            ArenaKind::Names => "names",
            ArenaKind::Matches => "matches",
        }
    }
}

/// An affine pointer to a heap node. A `TermPtr` is normally a live `UniqueKey`,
/// but it can also be *null* (`None`): a placeholder that names no slot. Null
/// pointers are safe to construct and hold (e.g. the [`Spine`] swaps one in for a
/// continuation whose node it is reducing elsewhere); they must never be
/// dereferenced, and the dereferencing accessors (`addr`/[`HeapScope::term`]/
/// [`HeapScope::remove`]) panic rather than risk UB if one ever is.
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct TermPtr<'h>(Option<UniqueKey<'h, Addr>>);
pub struct TermSlot<'h> {
    addr: Addr,
    slot: UniqueSlot<'h, Addr, Node>,
}

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
        TermPtr(Some(self.slot.finished()))
    }
    pub fn finished(mut self, term: Term<'h>) -> TermPtr<'h> {
        self.slot.update(term.pack());
        TermPtr(Some(self.slot.finished()))
    }
    pub fn addr(&self) -> Addr {
        self.addr
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
impl<'h> DupPtr<'h> {
    pub unsafe fn forge(addr: Addr, side: bool) -> Self { unsafe { DupPtr(SharedKey::forge(addr), side) } }
    pub fn addr(&self) -> Addr { self.0.raw() }
    pub fn side(&self) -> bool { self.1 }
}
#[rustfmt::skip]
impl<'h> BinderHandle<'h> {
    pub fn addr(&self) -> Addr { *self.0 }
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
        let dup = match term {
            Term::Dup { label, ptr } => Some((label, ptr)),
            _ => None,
        };
        let sup = match term {
            Term::Sup { label, ref ptr } => Some((label, ptr.addr())),
            _ => None,
        };
        let key = nodes.insert_unique(term.pack());
        if let Some((label, dp)) = dup {
            self.register_dup_parent(label, dp, *key);
        }
        if let Some((label, sup)) = sup {
            self.register_sup_parent(label, sup, *key);
        }
        TermPtr(Some(key))
    }

    // Consumes the pointer, returns the slot for updating
    // the value, as well as the unpacked term.
    pub fn term(&self, ptr: TermPtr<'h>) -> (TermSlot<'h>, Term<'h>) {
        let nodes = unsafe { self.heap.nodes.forge_brand() };
        let addr = ptr.addr();
        let slot = nodes.get_unique(ptr.key());
        let term = unsafe { slot.deref().unpack() };
        (TermSlot { addr, slot }, term)
    }

    /// Read-only view of the node at `addr`, its lifetime tied to a borrow of some
    /// live owner (a pointer that keeps the node reachable). This is the sole place
    /// that forges a read handle; everything outside the heap reaches nodes only
    /// through the safe `view*` wrappers below, which lend `&Term` and never hand
    /// back an owned (affine) pointer to duplicate or reclaim.
    fn view_addr<'r, T: ?Sized>(&self, _owner: &'r T, addr: Addr) -> TermView<'r, 'h> {
        let nodes = unsafe { self.heap.nodes.forge_brand() };
        let key = unsafe { SharedKey::forge(addr) };
        TermView {
            term: unsafe { nodes.get(&key).unpack() },
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
        nodes.remove(slot.slot.finished())
    }

    /// Finish an exclusive node slot after writing `term`, registering the slot
    /// as the parent if the written term is a dup projection or superposition.
    pub fn finish_slot(&self, slot: TermSlot<'h>, term: Term<'h>) -> TermPtr<'h> {
        let addr = slot.addr;
        let dup = match term {
            Term::Dup { label, ptr } => Some((label, ptr)),
            _ => None,
        };
        let sup = match term {
            Term::Sup { label, ref ptr } => Some((label, ptr.addr())),
            _ => None,
        };
        let ptr = slot.finished(term);
        if let Some((label, dp)) = dup {
            self.register_dup_parent(label, dp, addr);
        }
        if let Some((label, sup)) = sup {
            self.register_sup_parent(label, sup, addr);
        }
        ptr
    }

    /// Consume a node: free its slot and return its unpacked term.
    pub fn pull(&self, ptr: TermPtr<'h>) -> Term<'h> {
        // SAFETY: the node was a live `nodes` slot for scope `'h`.
        unsafe { self.remove(ptr).unpack() }
    }

    fn swap_node(&self, addr: Addr, term: Term<'h>) {
        let nodes = unsafe { self.heap.nodes.forge_brand() };
        let key = unsafe { SharedKey::forge(addr) };
        nodes.get(&key).swap(term.pack());
    }

    fn register_dup_parent(&self, label: LabelId, dp: DupPtr<'h>, parent: Addr) {
        let mut meta = self.dup_entry(dp).meta.lock().unwrap();
        match meta.label {
            Some(existing) => debug_assert_eq!(existing, label, "dup registered under two labels"),
            None => meta.label = Some(label),
        }
        meta.parents[DupMeta::side_index(dp.side())] = parent;
        drop(meta);
        self.heap
            .labels
            .lock()
            .unwrap()
            .entry(label)
            .or_default()
            .dups
            .insert(dp.addr());
    }

    fn register_sup_parent(&self, label: LabelId, sup: Addr, parent: Addr) {
        let sups = unsafe { self.heap.sups.forge_brand() };
        {
            let mut slot = sups.get_unique(unsafe { UniqueKey::forge(sup) });
            slot.label = Some(label);
            let _ = slot.finished();
        }
        self.heap
            .labels
            .lock()
            .unwrap()
            .entry(label)
            .or_default()
            .sups
            .insert(sup, parent);
    }

    fn unregister_sup_parent(&self, label: LabelId, sup: Addr) {
        let mut labels = self.heap.labels.lock().unwrap();
        if let Some(state) = labels.get_mut(&label) {
            state.sups.remove(&sup);
            if state.dups.is_empty() && state.sups.is_empty() {
                labels.remove(&label);
            }
        }
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

    /// Allocate a superposition cell over two argument node pointers.
    pub fn sup(&self, a: TermPtr<'h>, b: TermPtr<'h>) -> SupPtr<'h> {
        let sups = unsafe { self.heap.sups.forge_brand() };
        SupPtr(sups.insert_unique(SupCell {
            left: a.into_addr(),
            right: b.into_addr(),
            label: None,
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
        let addr = ptr.addr();
        let cell = sups.remove(ptr.0);
        if let Some(label) = cell.label {
            self.unregister_sup_parent(label, addr);
        }
        unsafe { (TermPtr::forge(cell.left), TermPtr::forge(cell.right)) }
    }

    // ====================================================================
    // Lambda binders
    // ====================================================================

    /// Overwrite a node in place with `term` (the slot stays allocated).
    fn write_node(&self, addr: Addr, term: Term<'h>) {
        let dup = match term {
            Term::Dup { label, ptr } => Some((label, ptr)),
            _ => None,
        };
        let sup = match term {
            Term::Sup { label, ref ptr } => Some((label, ptr.addr())),
            _ => None,
        };
        let nodes = unsafe { self.heap.nodes.forge_brand() };
        let mut slot = nodes.get_unique(unsafe { UniqueKey::forge(addr) });
        slot.update(term.pack());
        let _ = slot.finished();
        if let Some((label, dp)) = dup {
            self.register_dup_parent(label, dp, addr);
        }
        if let Some((label, sup)) = sup {
            self.register_sup_parent(label, sup, addr);
        }
    }

    /// APP-LAM's substitution step: write `arg` into the lambda's binder slot
    /// (overriding its `Var`), then mint and return the body pointer. This is the
    /// only way to obtain the body `TermPtr`.
    pub fn substitute(&self, body: BodyPtr<'h>, arg: Term<'h>) -> TermPtr<'h> {
        let dup = match arg {
            Term::Dup { label, ptr } => Some((label, ptr)),
            _ => None,
        };
        let sup = match arg {
            Term::Sup { label, ref ptr } => Some((label, ptr.addr())),
            _ => None,
        };
        let binder_addr = *body.binder;
        let nodes = unsafe { self.heap.nodes.forge_brand() };
        let mut slot = nodes.get_unique(body.binder);
        slot.update(arg.pack());
        let _ = slot.finished();
        if let Some((label, dp)) = dup {
            self.register_dup_parent(label, dp, binder_addr);
        }
        if let Some((label, sup)) = sup {
            self.register_sup_parent(label, sup, binder_addr);
        }
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
    /// projection slots are empty until the dup fires (see [`dup_publish`]).
    pub fn alloc_dup_at(&self, value: Addr) -> (DupPtr<'h>, DupPtr<'h>) {
        let dups = unsafe { self.heap.dups.forge_brand() };
        let key = dups.insert(DupEntry {
            meta: Mutex::new(DupMeta {
                dropped: [false, false],
                fired: false,
                parents: [Addr::new(0), Addr::new(0)],
                label: None,
            }),
            eval: SingleMutex::new(DupEval { value: Some(value) }),
        });
        (DupPtr(key, true), DupPtr(key, false))
    }

    fn dup_entry(&self, dp: DupPtr<'h>) -> &'h DupEntry {
        let dups = unsafe { self.heap.dups.forge_brand() };
        let key = unsafe { SharedKey::forge(dp.addr()) };
        dups.get(&key)
    }

    /// Allocate a duplication over `val`, collapsing one level when possible.
    /// With eager parent rewrites, a half-erased inner dup should not be
    /// reachable here; fired inner dup nodes are stale and are left alone.
    pub fn alloc_dup_collapsing(
        &self,
        val: Term<'h>,
    ) -> (DupPtr<'h>, DupPtr<'h>, bool, Vec<TermPtr<'h>>) {
        let (a, b) = self.alloc_dup(val);
        (a, b, false, Vec::new())
    }

    /// Make an auto-dup *use* of `value` (a REPL auto-dup local read site).
    /// Returns the node to splice in at this occurrence (`Dp0`) and the node to
    /// keep as the local's new value for the next use (`Dp1`), growing its dup
    /// chain by one. Both projections share one label (DUP-SUP annihilation).
    pub fn dup_use(&self, value: TermPtr<'h>) -> (TermPtr<'h>, TermPtr<'h>) {
        let (dp0, dp1) = self.alloc_dup_at(value.into_addr());
        // The cell address makes the `Auto` label unique to this dup.
        let label = self.auto_label(dp0.addr());
        let use_node = self.alloc(Term::Dup { label, ptr: dp0 });
        let keep_node = self.alloc(Term::Dup { label, ptr: dp1 });
        (use_node, keep_node)
    }

    pub fn dup_auto_label(&self, dp: DupPtr<'h>) -> LabelId {
        self.auto_label(dp.addr())
    }

    /// Acquire the duplication cell's eval lock (blocking the other branch until
    /// it is released). The caller inspects `value`: `Some` ⇒ this branch must
    /// reduce and fire it (holding the lock throughout); `None` ⇒ already fired,
    /// read this side's slot with [`HeapScope::dup_project`]. See [`DupEntry`].
    pub async fn dup_lock(&self, dp: DupPtr<'h>) -> SingleMutexGuard<'h, DupEval> {
        self.dup_entry(dp).eval.lock().await
    }

    pub fn dup_try_lock(&self, dp: DupPtr<'h>) -> Option<SingleMutexGuard<'h, DupEval>> {
        self.dup_entry(dp).eval.try_lock()
    }

    /// Take the dup cell's pending value as an owned pointer (the node to reduce),
    /// or `None` if it has already fired. Take-on-read: the cell's `value` is
    /// cleared, so it can never be forged into a second live pointer. If the dup
    /// turns out to be stuck, put it back with [`dup_restore_value`].
    pub fn dup_take_value(&self, guard: &mut SingleMutexGuard<'h, DupEval>) -> Option<TermPtr<'h>> {
        guard
            .value
            .take()
            .map(|addr| unsafe { TermPtr::forge(addr) })
    }

    /// Put a still-stuck duplicand back into the cell (the inverse of
    /// [`dup_take_value`]), consuming the pointer.
    pub fn dup_restore_value(&self, guard: &mut SingleMutexGuard<'h, DupEval>, value: TermPtr<'h>) {
        guard.value = Some(value.into_addr());
    }

    pub fn dup_pending_value(&self, dp: DupPtr<'h>) -> Option<Addr> {
        let guard = self.dup_entry(dp).eval.try_lock()?;
        guard.value
    }

    /// Fire a dup with both sides still live by overwriting only the OTHER
    /// projection's parent node. The forcing branch returns its term directly;
    /// the reduction spine writes it into the active slot on unwind.
    pub fn dup_rewrite_other(
        &self,
        dp: DupPtr<'h>,
        _eval: &SingleMutexGuard<'h, DupEval>,
        other: Term<'h>,
    ) -> Result<bool, Term<'h>> {
        let mut meta = self.dup_entry(dp).meta.lock().unwrap();
        if meta.dropped[DupMeta::side_index(!dp.side())] {
            return Err(other);
        }
        let other_parent = meta.parent(!dp.side());
        debug_assert_ne!(other_parent, Addr::new(0), "dup side parent was never registered");
        meta.fired = true;
        drop(meta);
        self.swap_node(other_parent, other);
        Ok(self.dup_entry(dp).eval.has_waiter())
    }

    /// Whether the OTHER side of `dp` has been dropped (erased). Brief meta
    /// lock; safe to call while holding the eval lock.
    pub fn dup_other_dropped(&self, dp: DupPtr<'h>) -> bool {
        let meta = self.dup_entry(dp).meta.lock().unwrap();
        meta.dropped[DupMeta::side_index(!dp.side())]
    }

    /// Drop side `dp.side()` of a dup. If the other projection is still live,
    /// rewrite its parent to the inner duplicand and free the dup cell.
    pub fn dup_drop_side(&self, dp: DupPtr<'h>) -> DupDrop<'h> {
        let entry = self.dup_entry(dp);
        let mut meta = entry.meta.lock().unwrap();
        let me = DupMeta::side_index(dp.side());
        let other = DupMeta::side_index(!dp.side());
        debug_assert!(!meta.dropped[me], "dup side dropped twice");
        if meta.fired {
            return DupDrop::Recorded { dead: Vec::new() };
        }
        meta.dropped[me] = true;
        let label = meta
            .label
            .expect("dup parent was never registered before drop");
        if !meta.dropped[other] {
            let has_sups = self
                .heap
                .labels
                .lock()
                .unwrap()
                .get(&label)
                .is_some_and(|state| !state.sups.is_empty());
            if has_sups {
                drop(meta);
                return DupDrop::Recorded {
                    dead: self.try_label_cleanup(label),
                };
            }
            let survivor_parent = meta.parent(!dp.side());
            debug_assert_ne!(
                survivor_parent,
                Addr::new(0),
                "dup side parent was never registered"
            );
            drop(meta);
            let mut eval = entry
                .eval
                .try_lock()
                .expect("cannot erase one dup side while the duplicand is being forced");
            let addr = eval
                .value
                .take()
                .expect("unfired dup with one side dropped has no duplicand");
            drop(eval);
            let survivor = self.pull(unsafe { TermPtr::forge(addr) });
            self.swap_node(survivor_parent, survivor);
            self.free_dup(dp);
            return DupDrop::Recorded { dead: Vec::new() };
        }
        drop(meta);
        let mut spins = 0usize;
        let mut eval = loop {
            if let Some(eval) = entry.eval.try_lock() {
                break eval;
            }
            spins += 1;
            debug_assert!(
                spins < 1_000_000,
                "dup eval lock held across a drop of both sides"
            );
            std::hint::spin_loop();
        };
        let addr = eval
            .value
            .take()
            .expect("unfired dup with both sides dropped has no duplicand");
        drop(eval);
        self.free_dup(dp);
        DupDrop::Reclaim(unsafe { TermPtr::forge(addr) })
    }

    fn try_label_cleanup(&self, label: LabelId) -> Vec<TermPtr<'h>> {
        let (dup_addrs, sup_entries) = {
            let labels = self.heap.labels.lock().unwrap();
            let Some(state) = labels.get(&label) else {
                return Vec::new();
            };
            if state.dups.is_empty() || state.sups.is_empty() {
                return Vec::new();
            }
            (
                state.dups.iter().copied().collect::<Vec<_>>(),
                state
                    .sups
                    .iter()
                    .map(|(&sup, &parent)| (sup, parent))
                    .collect::<Vec<_>>(),
            )
        };

        let mut all_dropped = [true, true];
        for dup in &dup_addrs {
            let dp = unsafe { DupPtr::forge(*dup, true) };
            let meta = self.dup_entry(dp).meta.lock().unwrap();
            if meta.fired {
                continue;
            }
            all_dropped[0] &= meta.dropped[0];
            all_dropped[1] &= meta.dropped[1];
        }
        let dropped = match (all_dropped[0], all_dropped[1]) {
            (true, false) => true,
            (false, true) => false,
            (true, true) => true,
            (false, false) => return Vec::new(),
        };

        let mut evals = Vec::with_capacity(dup_addrs.len());
        for dup in &dup_addrs {
            let dp = unsafe { DupPtr::forge(*dup, dropped) };
            let Some(eval) = self.dup_try_lock(dp) else {
                return Vec::new();
            };
            evals.push((*dup, eval));
        }

        let mut dead = Vec::new();
        for (dup, mut eval) in evals {
            let dp = unsafe { DupPtr::forge(dup, dropped) };
            let survivor_parent = {
                let meta = self.dup_entry(dp).meta.lock().unwrap();
                if meta.fired {
                    continue;
                }
                meta.parent(!dropped)
            };
            let Some(addr) = eval.value.take() else {
                continue;
            };
            drop(eval);
            let survivor = self.pull(unsafe { TermPtr::forge(addr) });
            self.swap_node(survivor_parent, survivor);
            self.free_dup(dp);
        }

        for (sup_addr, parent) in sup_entries {
            let sup = unsafe { SupPtr::forge(sup_addr) };
            if matches!(&*self.view_sup(&sup, true), Term::Var)
                || matches!(&*self.view_sup(&sup, false), Term::Var)
            {
                panic!(
                    "label cleanup reached an unsubstituted Var component; strong-normalization invariant violated"
                );
            }
            let (left, right) = self.free_sup(sup);
            let (keep, drop) = if dropped { (right, left) } else { (left, right) };
            let keep = self.pull(keep);
            self.write_node(parent, keep);
            dead.push(drop);
        }
        dead
    }

    /// Read a dup cell's pending value without consuming or firing it.
    /// For readback only; assumes the eval side is uncontended (reduction is
    /// idle).
    pub fn dup_peek(&self, dp: &DupPtr<'h>) -> Option<Addr> {
        let entry = self.dup_entry(*dp);
        let eval = entry
            .eval
            .try_lock()
            .expect("dup cell uncontended at readback");
        eval.value
    }

    /// Read a dup cell's fired flag and per-side drop flags (`[Dp0, Dp1]`).
    pub fn dup_peek_meta(&self, dp: &DupPtr<'h>) -> (bool, [bool; 2]) {
        let meta = self.dup_entry(*dp).meta.lock().unwrap();
        (meta.fired, meta.dropped)
    }

    /// View the duplicand behind an unfired dup projection, borrowed against
    /// the [`DupPtr`].
    pub fn view_dup<'r>(&self, dp: &'r DupPtr<'h>) -> (Addr, TermView<'r, 'h>) {
        let addr = self
            .dup_peek(dp)
            .expect("fired dup projection should have been rewritten");
        (addr, self.view_addr(dp, addr))
    }

    /// Reclaim a fully-consumed dup cell: by the loser after projecting, by the
    /// winner after a drop-elision, by [`dup_drop_side`] when a drop kills the
    /// cell, or by [`alloc_dup_collapsing`] after absorbing it. The caller must
    /// hold no guards into the entry.
    pub fn free_dup(&self, dp: DupPtr<'h>) {
        let label = {
            let meta = self.dup_entry(dp).meta.lock().unwrap();
            meta.label
        };
        if let Some(label) = label {
            let mut labels = self.heap.labels.lock().unwrap();
            if let Some(state) = labels.get_mut(&label) {
                state.dups.remove(&dp.addr());
                if state.dups.is_empty() && state.sups.is_empty() {
                    labels.remove(&label);
                }
            }
        }
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
        local: &mut dyn FnMut(&str) -> Option<TermPtr<'h>>,
    ) -> Result<TermPtr<'h>, String> {
        let mut env: Vec<LowerFrame> = Vec::new();
        self.lower_env(expr, &mut env, resolve, local)
    }

    /// Mint the automatic label for a binary dup/sup family. The owning cell
    /// address makes the label unique, while both projections share it so
    /// DUP-SUP annihilation still recognizes its own superposition.
    fn auto_label(&self, unique: Addr) -> LabelId {
        LabelId::from_u56(self.intern_name(&format!("&auto#{}", unique.to_u64())))
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
            Expr::Dp0(db) => match self.frame(env, db.0)? {
                LowerFrame::Dup { key, label, .. } => self.alloc(Term::Dup {
                    label,
                    ptr: unsafe { DupPtr::forge(key, true) },
                }),
                _ => return Err("Dp0 does not refer to a duplication".into()),
            },
            Expr::Dp1(db) => match self.frame(env, db.0)? {
                LowerFrame::Dup { key, label, .. } => self.alloc(Term::Dup {
                    label,
                    ptr: unsafe { DupPtr::forge(key, false) },
                }),
                _ => return Err("Dp1 does not refer to a duplication".into()),
            },
            Expr::Ref(db) => {
                let i = db.0 as usize;
                if i >= env.len() {
                    return Err(format!("de Bruijn index {i} out of range"));
                }
                let frame = env.len() - 1 - i;
                match &mut env[frame] {
                    LowerFrame::Dup { key, label, refs } => {
                        let side = match *refs {
                            0 => true,
                            1 => false,
                            _ => return Err("binary duplication used more than twice".into()),
                        };
                        *refs += 1;
                        self.alloc(Term::Dup {
                            label: *label,
                            ptr: unsafe { DupPtr::forge(*key, side) },
                        })
                    }
                    _ => return Err("Ref does not refer to a duplication".into()),
                }
            }
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
                let a = self.lower_env(left, env, resolve, local)?;
                let b = self.lower_env(right, env, resolve, local)?;
                let ptr = self.sup(a, b);
                let label = self.auto_label(ptr.addr());
                self.alloc(Term::Sup { label, ptr })
            }
            Expr::Dup { val, body } => {
                // The value is referenced lazily by node address, so a value that
                // is a lambda binder (auto-dup, `\&x -> …`) reads its substituted
                // argument at force time rather than a stale copy.
                let v = self.lower_env(val, env, resolve, local)?;
                let (dp0, _dp1) = self.alloc_dup_at(v.into_addr());
                // The cell address makes an `Auto` label unique to this dup.
                let label = self.auto_label(dp0.addr());
                env.push(LowerFrame::Dup {
                    key: dp0.addr(),
                    label,
                    refs: 0,
                });
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
        Ok(env[env.len() - 1 - i])
    }

    // ====================================================================
    // Arena enumeration / leak detection (readback-only)
    // ====================================================================

    /// Live slot count of an arena. Readback-only (reduction idle), like the
    /// other enumeration APIs: an outstanding reservation is counted as live.
    pub fn arena_len(&self, kind: ArenaKind) -> usize {
        match kind {
            ArenaKind::Nodes => self.heap.nodes.len(),
            ArenaKind::Dups => self.heap.dups.len(),
            ArenaKind::Sups => self.heap.sups.len(),
            ArenaKind::Values => self.heap.values.len(),
            ArenaKind::Packs => self.heap.packs.len(),
            ArenaKind::Types => self.heap.types.len(),
            ArenaKind::Names => self.heap.names.len(),
            ArenaKind::Matches => self.heap.matches.len(),
        }
    }

    /// Append the `nodes`-arena children of the node at `addr` to `out`: every
    /// node address reachable from it in one step, looking through its dup /
    /// sup / pack / type / match cells. Readback-only (dup cells must be
    /// uncontended).
    fn node_children(&self, addr: Addr, out: &mut Vec<Addr>) {
        let view = self.view_at(addr);
        match &*view {
            Term::App { func, arg } => out.extend([func.addr(), arg.addr()]),
            Term::Lam { body } => out.extend([body.binder_addr(), body.body_addr()]),
            Term::Use { body } => out.push(body.addr()),
            Term::Dup { ptr, .. } => {
                out.extend(self.dup_peek(ptr));
            }
            Term::Sup { ptr, .. } => {
                let (l, r) = self.sup_addrs(ptr);
                out.extend([l, r]);
            }
            Term::Ctn { ty, values, .. } => {
                self.type_children(ty.addr(), out);
                out.extend((0..self.pack_len(values)).map(|i| self.pack_addr(values, i)));
            }
            Term::Partial { func, args, .. } => {
                out.push(func.addr());
                out.extend((0..self.pack_len(args)).map(|i| self.pack_addr(args, i)));
            }
            Term::Ctr { ty, .. } => out.push(ty.addr()),
            Term::Type(ty) => self.type_children(ty.addr(), out),
            Term::Mat { matches } => {
                let data = self.match_data(matches);
                out.extend(data.cases.iter().flat_map(|&(key, branch)| [key, branch]));
                out.extend(data.default);
            }
            Term::Bop { lhs, rhs, .. } | Term::And { lhs, rhs } | Term::Or { lhs, rhs } => {
                out.extend([lhs.addr(), rhs.addr()])
            }
            Term::Uop { val, .. } => out.push(val.addr()),
            // Leaves. `Err` backtraces are currently always `None` and ignored
            // by erasure; `Box` points into the values arena, not `nodes`.
            Term::Var
            | Term::VarId(_)
            | Term::Wld
            | Term::Err { .. }
            | Term::Int(_)
            | Term::Float(_)
            | Term::Char(_)
            | Term::Bool(_)
            | Term::Box(_)
            | Term::Pri(_)
            | Term::Null => {}
        }
    }

    /// Append a [`TypeInfo`]'s field / variant-argument node addresses to `out`.
    fn type_children(&self, ty_addr: Addr, out: &mut Vec<Addr>) {
        match self.type_info_at(ty_addr) {
            TypeInfo::Product { fields, .. } => out.extend(fields.iter().copied()),
            TypeInfo::Sum { variants, .. } => {
                out.extend(variants.iter().flat_map(|v| v.args.iter().copied()))
            }
        }
    }

    /// Mark every node reachable from `roots` into `marked`, restricted to
    /// `within` if given (used to walk one leaked subgraph without leaving the
    /// unreachable set).
    fn mark_reachable(
        &self,
        roots: impl IntoIterator<Item = Addr>,
        within: Option<&HashSet<Addr>>,
        marked: &mut HashSet<Addr>,
    ) {
        let mut worklist: Vec<Addr> = roots.into_iter().collect();
        let mut children = Vec::new();
        while let Some(addr) = worklist.pop() {
            if within.is_some_and(|w| !w.contains(&addr)) {
                continue;
            }
            if !marked.insert(addr) {
                continue;
            }
            self.node_children(addr, &mut children);
            worklist.append(&mut children);
        }
    }

    /// Find the terms unreachable from `roots` and return forged owning
    /// pointers to the roots of each leaked subgraph. Every leaked node is
    /// reachable from the returned pointers: subgraphs with no in-edge get
    /// their in-degree-0 head; a leaked *cycle* has none, so an arbitrary
    /// member is elected to represent it (erasing a cycle representative is
    /// not supported — other cycle members still reference it).
    ///
    /// # Safety
    ///
    /// `roots` must include *every* externally held [`TermPtr`] into this heap
    /// (REPL locals, a pending evaluation root, the last result, pointers
    /// previously returned by this function, ...). Reduction must be idle (the
    /// readback precondition). Under that assumption, any live node not
    /// reachable from `roots` has no owner, so forging a pointer to it cannot
    /// alias an existing one.
    pub unsafe fn find_leaked_roots(&self, roots: &[&TermPtr<'h>]) -> Vec<TermPtr<'h>> {
        // Everything reachable from the external roots is owned. The dropped
        // queue owns its addresses until reclaimed, so it counts as a root.
        let mut reachable = HashSet::new();
        let external = roots
            .iter()
            .filter(|p| !p.is_null())
            .map(|p| p.addr())
            .chain(self.heap.dropped.lock().unwrap().iter().copied())
            .collect::<Vec<_>>();
        self.mark_reachable(external, None, &mut reachable);

        let unreachable: HashSet<Addr> = self
            .heap
            .nodes
            .live_keys()
            .into_iter()
            .filter(|a| !reachable.contains(a))
            .collect();

        // Reference edges among the unreachable set (children of reachable
        // nodes are themselves reachable, so only intra-set edges matter).
        let mut referenced = HashSet::new();
        let mut children = Vec::new();
        for &addr in &unreachable {
            self.node_children(addr, &mut children);
            referenced.extend(children.drain(..));
        }

        // In-degree-0 members head the leaked subgraphs; cover any remaining
        // cycles by electing their smallest uncovered member.
        let mut heads: Vec<Addr> = unreachable
            .iter()
            .copied()
            .filter(|a| !referenced.contains(a))
            .collect();
        heads.sort_unstable();
        let mut covered = HashSet::new();
        self.mark_reachable(heads.iter().copied(), Some(&unreachable), &mut covered);
        while covered.len() < unreachable.len() {
            let rep = unreachable
                .iter()
                .copied()
                .filter(|a| !covered.contains(a))
                .min()
                .expect("uncovered leaked node");
            heads.push(rep);
            self.mark_reachable([rep], Some(&unreachable), &mut covered);
        }

        heads
            .into_iter()
            .map(|addr| unsafe { TermPtr::forge(addr) })
            .collect()
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

    #[test]
    fn arena_len_tracks_allocations() {
        let heap = Heap::new();
        heap.with(|h| {
            assert_eq!(h.arena_len(ArenaKind::Nodes), 0);
            let a = h.alloc(Term::Int(1));
            let b = h.alloc(Term::Int(2));
            assert_eq!(h.arena_len(ArenaKind::Nodes), 2);
            let s = h.sup(a, b);
            assert_eq!(h.arena_len(ArenaKind::Sups), 1);
            let (a, b) = h.free_sup(s);
            let _ = h.pull(a);
            let _ = h.pull(b);
            assert_eq!(h.arena_len(ArenaKind::Nodes), 0);
            assert_eq!(h.arena_len(ArenaKind::Sups), 0);
        });
    }

    #[test]
    fn find_leaked_roots_returns_subgraph_heads() {
        let heap = Heap::new();
        heap.with(|h| {
            let kept = h.alloc(Term::Int(1));
            // Leak an `App { Int, Int }` subgraph by discarding its only pointer:
            // just the head should come back, not its children.
            let func = h.alloc(Term::Int(2));
            let arg = h.alloc(Term::Int(3));
            let leaked_addr = h.alloc(Term::App { func, arg }).into_addr();

            let null = TermPtr::null();
            let leaked = unsafe { h.find_leaked_roots(&[&kept, &null]) };
            assert_eq!(leaked.len(), 1);
            assert_eq!(leaked[0].addr(), leaked_addr);

            // Handing the forged pointer back as a root claims the subgraph.
            let again = unsafe { h.find_leaked_roots(&[&kept, &leaked[0]]) };
            assert!(again.is_empty());

            // The forged pointer is a real owner: reclaim through it.
            match h.pull(leaked.into_iter().next().unwrap()) {
                Term::App { func, arg } => {
                    let _ = h.pull(func);
                    let _ = h.pull(arg);
                }
                other => panic!("expected leaked App, got {other:?}"),
            }
            assert_eq!(h.arena_len(ArenaKind::Nodes), 1);
            let _ = h.pull(kept);
        });
    }

    #[test]
    fn dup_drop_one_side_rewrites_survivor_parent() {
        let heap = Heap::new();
        heap.with(|h| {
            let (d0, d1) = h.alloc_dup(Term::Int(1));
            let label = h.dup_auto_label(d0);
            let n0 = h.alloc(Term::Dup { label, ptr: d0 });
            let n1 = h.alloc(Term::Dup { label, ptr: d1 });
            let _ = h.remove(n1);
            assert!(matches!(h.dup_drop_side(d1), DupDrop::Recorded { .. }));
            assert_eq!(*h.view(&n0), Term::Int(1));
            let _ = h.remove(n0);
            assert_eq!(h.arena_len(ArenaKind::Dups), 0);
            assert_eq!(h.arena_len(ArenaKind::Nodes), 0);
        });
    }

    #[test]
    fn dup_drop_after_fire_reclaims_slot() {
        let heap = Heap::new();
        heap.with(|h| {
            let (d0, d1) = h.alloc_dup(Term::Int(9));
            let label = h.dup_auto_label(d0);
            let n0 = h.alloc(Term::Dup { label, ptr: d0 });
            let n1 = h.alloc(Term::Dup { label, ptr: d1 });
            // Simulate the winner: take the value, rewrite only the other parent.
            let mut guard = h.dup_try_lock(d0).unwrap();
            let seed = h.dup_take_value(&mut guard).unwrap();
            let own = h.pull(seed);
            let waiter = h
                .dup_rewrite_other(d0, &guard, Term::Int(9))
                .unwrap();
            drop(guard);
            assert!(!waiter);
            h.free_dup(d0);
            assert_eq!(own, Term::Int(9));
            assert!(matches!(h.pull(n0), Term::Dup { .. }));
            assert_eq!(h.pull(n1), Term::Int(9));
            assert_eq!(h.arena_len(ArenaKind::Dups), 0);
            assert_eq!(h.arena_len(ArenaKind::Nodes), 0);
        });
    }

    #[test]
    fn find_leaked_roots_sees_through_lambdas() {
        let heap = Heap::new();
        heap.with(|h| {
            // Leak an identity lambda: the binder occurrence is referenced by
            // the Lam node, so only the Lam itself is a leaked head.
            let (binder, occ) = h.fresh_binder();
            let body = h.close_body(binder, occ);
            let lam_addr = h.alloc(Term::Lam { body }).into_addr();
            assert_eq!(h.arena_len(ArenaKind::Nodes), 2);

            let leaked = unsafe { h.find_leaked_roots(&[]) };
            assert_eq!(leaked.len(), 1);
            assert_eq!(leaked[0].addr(), lam_addr);
        });
    }
}
