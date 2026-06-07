//! The [`Memory`] arena: raw cell storage plus typed allocation.
//!
//! All heap pointers live here. A [`Node`] word's `VAL` is usually a *location*
//! into the arena, addressed by one of the typed pointers below depending on the
//! node's shape:
//!
//! - [`NodePtr`]  — a single cell (a substitution slot, or any one field).
//! - [`PairPtr`]  — two cells (`App`, `Lam`).
//! - [`TriplePtr`] — three cells (`Sup` = `[Label, l, r]`, `Bop` = `[Op, l, r]`).
//! - [`DupPtr`]   — four cells (`[Label, val, sub0, sub1]`).
//! - [`CtrPtr`]   — `2 + arity` cells (`[Name, Arity, fields..]`).
//!
//! Cells are [`AtomicU64`] and every accessor takes `&self`, so the arena can be
//! shared (`Arc<Heap>`) and mutated concurrently from many worker threads. The
//! backing store is a **segmented** arena: a fixed table of lazily-installed,
//! heap-stable segments, with a single atomic bump pointer for allocation. This
//! gives lock-free growth without ever moving a live cell (so a `&AtomicU64`
//! handed out stays valid), and contiguous blocks (allocations never straddle a
//! segment boundary). [`take`](Memory::take) / [`cas`](Memory::cas) are the
//! lock-free "claim a slot" primitives used at contention points (binder
//! substitution, DUP firing).
//!
//! Reclamation ([`free_pair`](Memory::free_pair) etc.) is currently a no-op:
//! the parallel arena is bump-only. Lock-free recycling is future work; it does
//! not affect correctness, only peak memory.

use crate::vm::term::{Label, Node, Term};
use std::ptr;
use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};

// --- typed pointers ---

/// A location into the arena pointing at a *single* cell — a substitution slot
/// or any individual field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodePtr(pub u64);

impl NodePtr {
    /// The cell `n` slots after this one.
    pub fn offset(self, n: u64) -> NodePtr {
        NodePtr(self.0 + n)
    }
}

/// A location pointing at a two-cell binary node (`first`, `second`), as built by
/// `Memory::alloc_pair` (applications, lambdas, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PairPtr(pub u64);

impl PairPtr {
    pub fn first(self) -> NodePtr {
        NodePtr(self.0)
    }
    pub fn second(self) -> NodePtr {
        NodePtr(self.0 + 1)
    }
}

/// A location pointing at a three-cell node. Used by superpositions
/// (`[Label, left, right]`) and binary ops (`[OpMeta, lhs, rhs]`); the leading
/// cell is a meta-term and the two operands follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TriplePtr(pub u64);

impl TriplePtr {
    pub fn first(self) -> NodePtr {
        NodePtr(self.0)
    }
    pub fn second(self) -> NodePtr {
        NodePtr(self.0 + 1)
    }
    pub fn third(self) -> NodePtr {
        NodePtr(self.0 + 2)
    }
}

/// A location pointing at a four-cell duplication node, as built by
/// `Memory::alloc_dup`. The cells are `[Label, val, sub0, sub1]`: a leading
/// label meta-cell, the duplicated value, and the two substitution slots the
/// `Dp0`/`Dp1` projections read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DupPtr(pub u64);

impl DupPtr {
    /// The leading label meta-cell.
    pub fn label(self) -> NodePtr {
        NodePtr(self.0)
    }
    /// The duplicated value.
    pub fn val(self) -> NodePtr {
        NodePtr(self.0 + 1)
    }
    /// The `Dp0` substitution slot.
    pub fn sub0(self) -> NodePtr {
        NodePtr(self.0 + 2)
    }
    /// The `Dp1` substitution slot.
    pub fn sub1(self) -> NodePtr {
        NodePtr(self.0 + 3)
    }
}

/// A location pointing at a constructor node, as built by `Memory::alloc_ctr`.
/// The cells are `[Name, Arity, fields..]`: two leading meta-cells followed by
/// `arity` field slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CtrPtr(pub u64);

impl CtrPtr {
    /// The leading name meta-cell.
    pub fn name(self) -> NodePtr {
        NodePtr(self.0)
    }
    /// The arity meta-cell.
    pub fn arity(self) -> NodePtr {
        NodePtr(self.0 + 1)
    }
    /// The location of field `i` (fields follow the two meta-cells).
    pub fn field(self, i: u64) -> NodePtr {
        NodePtr(self.0 + 2 + i)
    }
}

// --- the arena ---

/// Sentinel word stored by [`Memory::take`] to mark a slot as momentarily
/// claimed by one worker (lock-free hand-off at contention points).
pub const LOCKED: u64 = u64::MAX;

/// Cells per segment (`2^SEG_BITS`).
const SEG_BITS: u32 = 16;
const SEG_SIZE: usize = 1 << SEG_BITS;
const SEG_MASK: u64 = SEG_SIZE as u64 - 1;
/// Maximum number of segments (`MAX_SEGS * SEG_SIZE` cells of address space).
const MAX_SEGS: usize = 1 << 15;

/// The cell arena: a fixed table of lazily-installed segments plus an atomic
/// bump pointer. See the [module docs](self).
pub struct Memory {
    /// `segments[i]` points at the first cell of segment `i`, or null until it
    /// is first touched. Installed lock-free via compare-and-swap.
    segments: Box<[AtomicPtr<AtomicU64>]>,
    /// Next free cell index. Allocation is `fetch`-then-`compare_exchange`.
    bump: AtomicU64,
}

impl Memory {
    pub fn new() -> Self {
        let segments = (0..MAX_SEGS)
            .map(|_| AtomicPtr::new(ptr::null_mut()))
            .collect();
        // Cell 0 is reserved as the null sentinel; start allocating at 1.
        let m = Memory {
            segments,
            bump: AtomicU64::new(1),
        };
        m.cell(0); // materialize segment 0 so the sentinel cell exists
        m
    }

    /// Borrow the [`AtomicU64`] backing cell `index`, installing its segment on
    /// first touch. The reference is valid for `&self` because segments, once
    /// installed, are never moved or freed until the arena is dropped.
    fn cell(&self, index: u64) -> &AtomicU64 {
        let seg = (index >> SEG_BITS) as usize;
        let off = (index & SEG_MASK) as usize;
        let base = self.segments[seg].load(Ordering::Acquire);
        let base = if base.is_null() {
            self.install_segment(seg)
        } else {
            base
        };
        // SAFETY: `base` points at a `SEG_SIZE`-cell array and `off < SEG_SIZE`.
        unsafe { &*base.add(off) }
    }

    /// Allocate and install segment `seg`, returning its base pointer. If a
    /// concurrent caller installs it first, our segment is discarded.
    fn install_segment(&self, seg: usize) -> *mut AtomicU64 {
        let mut cells: Vec<AtomicU64> = Vec::with_capacity(SEG_SIZE);
        cells.resize_with(SEG_SIZE, || AtomicU64::new(0));
        let raw = Box::into_raw(cells.into_boxed_slice()) as *mut AtomicU64;
        match self.segments[seg].compare_exchange(
            ptr::null_mut(),
            raw,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => raw,
            Err(existing) => {
                // lost the race: reclaim the segment we just built.
                let slice = ptr::slice_from_raw_parts_mut(raw, SEG_SIZE);
                // SAFETY: `raw` came from `Box::into_raw` of a `SEG_SIZE` slice.
                unsafe { drop(Box::from_raw(slice)) };
                existing
            }
        }
    }

    // --- cell access ---

    pub fn node(&self, p: NodePtr) -> Node {
        Node::from_raw(self.cell(p.0).load(Ordering::Relaxed))
    }
    pub fn set(&self, p: NodePtr, t: Node) {
        self.cell(p.0).store(t.raw(), Ordering::Relaxed);
    }

    /// Atomically read and clear a slot to [`LOCKED`], returning its previous
    /// word. The lock-free way to *claim* a contended slot (a DUP value): exactly
    /// one caller observes the real word, others observe `LOCKED`.
    pub fn take(&self, p: NodePtr) -> Node {
        Node::from_raw(self.cell(p.0).swap(LOCKED, Ordering::AcqRel))
    }

    /// Compare-and-swap a slot from `expected` to `new`, returning whether it
    /// succeeded. The lock-free "link" primitive.
    pub fn cas(&self, p: NodePtr, expected: Node, new: Node) -> bool {
        self.cell(p.0)
            .compare_exchange(
                expected.raw(),
                new.raw(),
                Ordering::AcqRel,
                Ordering::Relaxed,
            )
            .is_ok()
    }

    // --- allocation ---

    /// Reserve `size` consecutive cells, never straddling a segment boundary
    /// (the tail of a segment is skipped if a block would not fit). Lock-free.
    fn alloc(&self, size: usize) -> u64 {
        let size = size as u64;
        debug_assert!(size as usize <= SEG_SIZE);
        loop {
            let base = self.bump.load(Ordering::Relaxed);
            let off = base & SEG_MASK;
            // skip to the next segment if the block would cross a boundary.
            let start = if off + size <= SEG_SIZE as u64 {
                base
            } else {
                (base - off) + SEG_SIZE as u64
            };
            if self
                .bump
                .compare_exchange(base, start + size, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                return start;
            }
        }
    }

    /// Allocate a single cell (e.g. an evaluation root slot).
    pub fn alloc_cell(&self, value: Node) -> NodePtr {
        let ptr = NodePtr(self.alloc(1));
        self.set(ptr, value);
        ptr
    }

    pub fn alloc_pair(&self, a: Node, b: Node) -> PairPtr {
        let ptr = PairPtr(self.alloc(2));
        self.set(ptr.first(), a);
        self.set(ptr.second(), b);
        ptr
    }
    pub fn alloc_triple(&self, a: Node, b: Node, c: Node) -> TriplePtr {
        let ptr = TriplePtr(self.alloc(3));
        self.set(ptr.first(), a);
        self.set(ptr.second(), b);
        self.set(ptr.third(), c);
        ptr
    }
    pub fn alloc_dup(&self, label: Label, val: Node) -> DupPtr {
        let ptr = DupPtr(self.alloc(4));
        self.set(ptr.label(), Term::LabelMeta(label).into());
        self.set(ptr.val(), val);
        self.set(ptr.sub0(), Node::NULL);
        self.set(ptr.sub1(), Node::NULL);
        ptr
    }
    pub fn alloc_ctr(&self, arity: usize) -> CtrPtr {
        CtrPtr(self.alloc(2 + arity))
    }

    // --- reclamation (currently no-ops: the parallel arena is bump-only) ---

    pub fn free_cell(&self, _p: NodePtr) {}
    pub fn free_pair(&self, _p: PairPtr) {}
    pub fn free_triple(&self, _p: TriplePtr) {}
    pub fn free_dup(&self, _p: DupPtr) {}
    pub fn free_ctr(&self, _p: CtrPtr, _arity: usize) {}
}

impl Drop for Memory {
    fn drop(&mut self) {
        for slot in self.segments.iter() {
            let p = slot.load(Ordering::Relaxed);
            if !p.is_null() {
                let slice = ptr::slice_from_raw_parts_mut(p, SEG_SIZE);
                // SAFETY: installed segments come from `Box::into_raw` of a
                // `SEG_SIZE` slice and are installed at most once.
                unsafe { drop(Box::from_raw(slice)) };
            }
        }
    }
}

impl Default for Memory {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_gives_distinct_blocks() {
        let m = Memory::new();
        let a = m.alloc_pair(Node::NULL, Node::NULL);
        let b = m.alloc_pair(Node::NULL, Node::NULL);
        assert_ne!(a.0, b.0);
        // non-overlapping two-cell blocks
        assert!(b.0 >= a.0 + 2 || a.0 >= b.0 + 2);
    }

    #[test]
    fn read_write_round_trips() {
        let m = Memory::new();
        let p = m.alloc_cell(Node::from_raw(0));
        m.set(p, Node::from_raw(0xDEAD_BEEF));
        assert_eq!(m.node(p).raw(), 0xDEAD_BEEF);
    }

    #[test]
    fn read_write_across_segments() {
        let m = Memory::new();
        // allocate past the first segment, then poke a cell in the second.
        let mut later = NodePtr(0);
        for i in 0..(SEG_SIZE as u64 + 16) {
            let p = m.alloc_cell(Node::from_raw(i));
            if i == SEG_SIZE as u64 + 8 {
                later = p;
            }
        }
        assert!(later.0 >= SEG_SIZE as u64);
        m.set(later, Node::from_raw(0xABCD));
        assert_eq!(m.node(later).raw(), 0xABCD);
    }

    #[test]
    fn blocks_never_straddle_a_segment() {
        let m = Memory::new();
        // allocate enough 7-cell blocks to cross several segment boundaries.
        for _ in 0..(SEG_SIZE / 5) {
            let c = m.alloc_ctr(5); // 7 cells
            let base = c.0;
            assert_eq!(
                base >> SEG_BITS,
                (base + 6) >> SEG_BITS,
                "block straddled a segment boundary"
            );
        }
    }

    #[test]
    fn take_claims_a_slot() {
        let m = Memory::new();
        let p = m.alloc_cell(Node::from_raw(42));
        assert_eq!(m.take(p).raw(), 42);
        assert_eq!(m.node(p).raw(), LOCKED);
    }
}
