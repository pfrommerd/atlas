use super::term::{Brand, Node, Term};
use crate::core::expr::Expr;
use crate::util::slab::{SharedKey, ShardedSlab, UniqueKey, UniqueSlot};
use crate::util::{SingleMutex, SingleMutexGuard, U56};
use std::collections::HashMap;
use std::ops::Deref;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};

pub struct Heap {
    nodes: ShardedSlab<Addr, Node>,
    dups: ShardedSlab<Addr, SingleMutex<DupCell>>,
    sups: ShardedSlab<Addr, SupCell>,
    values: ShardedSlab<Addr, ValueCell>,
    // A pack is a contiguous run of node addresses (constructor fields / match
    // branches). Each entry names a first-class node in `nodes`.
    packs: ShardedSlab<Addr, Box<[Addr]>>,
    // Interned constructor names: equal names share one slab entry (and thus one
    // `NamePtr` address), so pattern matching can compare names by address.
    names: ShardedSlab<Addr, Arc<str>>,
    interner: Mutex<HashMap<Arc<str>, Addr>>,
    // Match tables, referenced by a shared `MatchPtr`.
    matches: ShardedSlab<Addr, MatchData>,
}

#[derive(Debug, Clone)]
pub enum Value {
    Str(Arc<str>),
    Bytes(Arc<[u8]>),
}

/// A pattern-match key: either a constructor (by interned name address) or a
/// primitive integer literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PatKey {
    Ctr(Addr),
    Num(u64),
}

/// The cases of a `Mat` node. Each branch is the address of a node in `nodes`.
#[derive(Debug, Clone)]
pub struct MatchData {
    pub cases: Vec<(PatKey, Addr)>,
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

/// A duplication cell, stored behind a slab-level [`SingleMutex`]. The two
/// projection branches contend for the lock: the first to acquire it sees a
/// `Some(value)`, reduces and fires it (writing the projection slots and setting
/// `value = None`) *while holding the lock*; the second blocks on `lock().await`
/// and, on waking to a `None`, reads its already-written projection. `left`/
/// `right` are the `Dp0`/`Dp1` projection slot node addresses.
pub struct DupCell {
    pub value: Option<Node>,
    pub left: Addr,
    pub right: Addr,
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
        }
    }

    pub unsafe fn forge_brand<'h>(&self) -> &HeapScope<'h> {
        unsafe { &*(self as *const Self as *const HeapScope<'h>) }
    }
    /// Safely brand this slab for the duration of `f`.
    pub fn with<R>(&self, f: impl for<'h> FnOnce(&HeapScope<'h>) -> R) -> R {
        f(unsafe { self.forge_brand() })
    }
}

impl Default for Heap {
    fn default() -> Self {
        Heap::new()
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
    pub fn addr(&self) -> Addr { *self.0 }
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
        TermPtr(nodes.insert_unique(term.pack()))
    }

    // Consumes the pointer, returns the slot for updating
    // the value, as well as the unpacked term.
    pub fn term(&self, ptr: TermPtr<'h>) -> (TermSlot<'h>, Term<'h>) {
        let nodes = unsafe { self.heap.nodes.forge_brand() };
        let slot = nodes.get_unique(ptr.0);
        let term = unsafe { slot.deref().unpack() };
        (TermSlot(slot), term)
    }

    /// Read-only view of the node at `addr` without consuming a pointer.
    ///
    /// This is the shared-read path used for traversal/readback. The returned
    /// `Term` contains *forged* affine child pointers; the caller must not use
    /// them to mutate or remove nodes that remain reachable elsewhere (doing so
    /// would duplicate an affine handle).
    pub fn view(&self, addr: Addr) -> Term<'h> {
        let nodes = unsafe { self.heap.nodes.forge_brand() };
        let key = unsafe { SharedKey::forge(addr) };
        let node = *nodes.get(&key);
        unsafe { node.unpack() }
    }

    /// Reclaim a node, returning its raw packed contents.
    pub fn remove(&self, ptr: TermPtr<'h>) -> Node {
        let nodes = unsafe { self.heap.nodes.forge_brand() };
        nodes.remove(ptr.0)
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

    /// Overwrite the node at `addr` in place (the slot stays allocated).
    pub fn set(&self, addr: Addr, term: Term<'h>) {
        self.write_node(addr, term);
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

    // ====================================================================
    // Boxed values
    // ====================================================================

    pub fn value(&self, value: Value) -> ValuePtr<'h> {
        let values = unsafe { self.heap.values.forge_brand() };
        ValuePtr(values.insert_unique(ValueCell {
            count: AtomicUsize::new(1),
            value,
        }))
    }

    pub fn value_get(&self, ptr: &ValuePtr<'h>) -> &'h Value {
        let values = unsafe { self.heap.values.forge_brand() };
        let key = unsafe { SharedKey::forge(ptr.addr()) };
        &values.get(&key).value
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

    pub fn sup_args(&self, ptr: &SupPtr<'h>) -> (TermPtr<'h>, TermPtr<'h>) {
        let sups = unsafe { self.heap.sups.forge_brand() };
        let key = unsafe { SharedKey::forge(ptr.addr()) };
        let cell = sups.get(&key);
        unsafe { (TermPtr::forge(cell.left), TermPtr::forge(cell.right)) }
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

    // ====================================================================
    // Duplications
    // ====================================================================

    /// Allocate a duplication over `val`, returning the two projections
    /// (`Dp0` = side `true`, `Dp1` = side `false`).
    pub fn alloc_dup(&self, val: Term<'h>) -> (DupPtr<'h>, DupPtr<'h>) {
        let left = self.alloc(Term::Var).into_addr();
        let right = self.alloc(Term::Var).into_addr();
        let dups = unsafe { self.heap.dups.forge_brand() };
        let key = dups.insert(SingleMutex::new(DupCell {
            value: Some(val.pack()),
            left,
            right,
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

    /// The projection slot address for one side of a dup.
    pub fn dup_slot(&self, dp: DupPtr<'h>, cell: &DupCell) -> Addr {
        if dp.side() { cell.left } else { cell.right }
    }

    /// Reclaim a fired dup cell (called by the second/loser projection after it
    /// has read its substitution): removes the cell and the winner's spent slot.
    pub fn free_dup(&self, dp: DupPtr<'h>) {
        let dups = unsafe { self.heap.dups.forge_brand() };
        let key = unsafe { SharedKey::forge(dp.addr()) };
        let winner_slot = {
            let guard = dups
                .get(&key)
                .try_lock()
                .expect("dup cell uncontended at free");
            if dp.side() { guard.right } else { guard.left }
        };
        let _ = self.remove(unsafe { TermPtr::forge(winner_slot) });
        let _ = dups.remove(unsafe { UniqueKey::forge(dp.addr()) });
    }

    // ====================================================================
    // Lowering: desugared core `Expr` -> heap term graph
    // ====================================================================

    /// Lower a desugared [`Expr`] into a heap term, returning a pointer to its
    /// root node. (v1 supports the affine core; `Dup`/`Sup`/`Mat`/`Pri`/`Ref`
    /// are not yet lowered.)
    pub fn lower(&self, expr: &Expr) -> Result<TermPtr<'h>, String> {
        let mut env: Vec<Addr> = Vec::new();
        self.lower_env(expr, &mut env)
    }

    fn lower_env(&self, expr: &Expr, env: &mut Vec<Addr>) -> Result<TermPtr<'h>, String> {
        Ok(match expr {
            Expr::Num(n) => self.alloc(Term::U64(*n)),
            Expr::Wld => self.alloc(Term::Wld),
            Expr::Era => self.alloc(Term::Wld),
            Expr::Var(db) => {
                let i = db.0 as usize;
                if i >= env.len() {
                    return Err(format!("de Bruijn index {i} out of range"));
                }
                let addr = env[env.len() - 1 - i];
                unsafe { TermPtr::forge(addr) }
            }
            Expr::App { func, arg } => {
                let f = self.lower_env(func, env)?;
                let a = self.lower_env(arg, env)?;
                self.alloc(Term::App { func: f, arg: a })
            }
            Expr::Op2 { op, left, right } => {
                let l = self.lower_env(left, env)?;
                let r = self.lower_env(right, env)?;
                self.alloc(Term::Bop {
                    op: *op,
                    lhs: l,
                    rhs: r,
                })
            }
            Expr::Lam { body } => {
                let binder = self.alloc(Term::Var).into_addr();
                env.push(binder);
                let b = self.lower_env(body, env);
                env.pop();
                let body_addr = b?.into_addr();
                self.alloc(Term::Lam {
                    body: unsafe { BodyPtr::forge(binder, body_addr) },
                })
            }
            Expr::Use { body } => {
                // An erasing lambda still occupies a de Bruijn level (kept aligned
                // with `desugar`), but nothing references it.
                env.push(Addr::new(0));
                let b = self.lower_env(body, env);
                env.pop();
                self.alloc(Term::Use { body: b? })
            }
            Expr::Ctr { name, args } => {
                let nm = self.intern_name(name);
                let mut fields = Vec::with_capacity(args.len());
                for a in args {
                    fields.push(self.lower_env(a, env)?);
                }
                let arity = fields.len() as u8;
                let pack = self.alloc_pack(fields);
                self.alloc(Term::Ctr {
                    name: nm,
                    arity,
                    values: pack,
                })
            }
            Expr::Sup { .. }
            | Expr::Dup { .. }
            | Expr::Dp0(_)
            | Expr::Dp1(_)
            | Expr::Mat { .. }
            | Expr::Pri(_)
            | Expr::Ref(_) => {
                return Err("this construct is not yet supported by the v1 executor".into());
            }
        })
    }
}

// Contains the spine of a reduction.
//
// The spine is a stack of (slot, term) frames built while descending toward the
// head of a reduction. Each frame owns an affine `TermSlot`, so the only safe way
// to expose the contents is to restrict access to the innermost (last-pushed)
// frame -- the "bottom" of the stack where all interactions happen. The backing
// `Vec` is private and there is deliberately no random/indexed access, which
// prevents two affine `TermSlot`s from being aliased at once.
pub struct Spine<'h> {
    terms: Vec<(TermSlot<'h>, Term<'h>)>,
}

impl<'h> Spine<'h> {
    pub fn new() -> Self {
        Spine { terms: Vec::new() }
    }

    pub fn push(&mut self, slot: TermSlot<'h>, term: Term<'h>) {
        self.terms.push((slot, term));
    }

    pub fn pop(&mut self) -> Option<(TermSlot<'h>, Term<'h>)> {
        self.terms.pop()
    }

    /// Read the innermost term without removing it (e.g. to branch on its kind).
    pub fn peek(&self) -> Option<&Term<'h>> {
        self.terms.last().map(|(_, term)| term)
    }

    /// Exclusive access to *only* the innermost frame.
    pub fn bottom_mut(&mut self) -> Option<(&mut TermSlot<'h>, &mut Term<'h>)> {
        self.terms.last_mut().map(|(slot, term)| (slot, term))
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
            let p = h.alloc(Term::U64(42));
            assert_eq!(h.view(p.addr()), Term::U64(42));
            let (_slot, term) = h.term(p);
            assert_eq!(term, Term::U64(42));
        });
    }

    #[test]
    fn pack_field_access() {
        let heap = Heap::new();
        heap.with(|h| {
            let f0 = h.alloc(Term::U64(1));
            let f1 = h.alloc(Term::U64(2));
            let pack = h.alloc_pack(vec![f0, f1]);
            assert_eq!(h.pack_len(&pack), 2);
            assert_eq!(h.view(h.pack_field(&pack, 0).addr()), Term::U64(1));
            assert_eq!(h.view(h.pack_field(&pack, 1).addr()), Term::U64(2));
            let _ = h.free_pack(pack);
        });
    }

    #[test]
    fn value_dup_gives_fresh_entry() {
        let heap = Heap::new();
        heap.with(|h| {
            let v = h.value(Value::Str(Arc::from("hi")));
            match h.value_get(&v) {
                Value::Str(s) => assert_eq!(&**s, "hi"),
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
            let a = h.alloc(Term::U64(7));
            let b = h.alloc(Term::U64(8));
            let s = h.sup(a, b);
            let (x, y) = h.sup_args(&s);
            assert_eq!(h.view(x.addr()), Term::U64(7));
            assert_eq!(h.view(y.addr()), Term::U64(8));
            let _ = h.free_sup(s);
        });
    }
}
