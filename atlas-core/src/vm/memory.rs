//! The [`Memory`] arena: raw cell storage plus typed allocation and reclamation.
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
//! [`Memory`] owns the backing `Vec<u64>` and a per-size free list, so each shape
//! of allocation is recycled independently. The interaction rules return cells to
//! the arena as redexes are consumed (the calculus is affine), and [`Memory`]
//! hands those cells back out on the next allocation of the same size.

use crate::vm::term::{Label, Node, Term};

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

/// The cell arena: a flat `Vec<u64>` of packed [`Node`] words, with a free list
/// per allocation size so each shape is recycled independently.
pub struct Memory {
    mem: Vec<u64>,
    /// `free[size]` holds the base locations of reclaimed blocks of that size.
    free: Vec<Vec<u64>>,
}

impl Memory {
    pub fn new() -> Self {
        // Cell 0 is reserved as the null sentinel.
        Memory {
            mem: vec![0],
            free: Vec::new(),
        }
    }

    // --- cell access ---

    pub fn node(&self, p: NodePtr) -> Node {
        Node::from_raw(self.mem[p.0 as usize])
    }
    pub fn set(&mut self, p: NodePtr, t: Node) {
        self.mem[p.0 as usize] = t.raw();
    }

    // --- allocation ---

    /// Allocate `size` consecutive cells, reusing a reclaimed block when one of
    /// that size is available. The cells are *not* zeroed; callers overwrite
    /// every cell of the shape they build.
    fn alloc(&mut self, size: usize) -> u64 {
        if let Some(list) = self.free.get_mut(size)
            && let Some(loc) = list.pop()
        {
            return loc;
        }
        let loc = self.mem.len() as u64;
        self.mem.resize(self.mem.len() + size, 0);
        loc
    }

    /// Return a `size`-cell block at `loc` to the free list.
    fn free_block(&mut self, loc: u64, size: usize) {
        if self.free.len() <= size {
            self.free.resize(size + 1, Vec::new());
        }
        self.free[size].push(loc);
    }

    /// Allocate a single cell (e.g. an evaluation root slot).
    pub fn alloc_cell(&mut self, value: Node) -> NodePtr {
        let ptr = NodePtr(self.alloc(1));
        self.set(ptr, value);
        ptr
    }

    pub fn alloc_pair(&mut self, a: Node, b: Node) -> PairPtr {
        let ptr = PairPtr(self.alloc(2));
        self.set(ptr.first(), a);
        self.set(ptr.second(), b);
        ptr
    }
    pub fn alloc_triple(&mut self, a: Node, b: Node, c: Node) -> TriplePtr {
        let ptr = TriplePtr(self.alloc(3));
        self.set(ptr.first(), a);
        self.set(ptr.second(), b);
        self.set(ptr.third(), c);
        ptr
    }
    pub fn alloc_dup(&mut self, label: Label, val: Node) -> DupPtr {
        let ptr = DupPtr(self.alloc(4));
        self.set(ptr.label(), Term::LabelMeta(label).into());
        self.set(ptr.val(), val);
        self.set(ptr.sub0(), Node::NULL);
        self.set(ptr.sub1(), Node::NULL);
        ptr
    }
    pub fn alloc_ctr(&mut self, arity: usize) -> CtrPtr {
        CtrPtr(self.alloc(2 + arity))
    }

    // --- reclamation ---

    pub fn free_cell(&mut self, p: NodePtr) {
        self.free_block(p.0, 1);
    }
    pub fn free_pair(&mut self, p: PairPtr) {
        self.free_block(p.0, 2);
    }
    pub fn free_triple(&mut self, p: TriplePtr) {
        self.free_block(p.0, 3);
    }
    pub fn free_dup(&mut self, p: DupPtr) {
        self.free_block(p.0, 4);
    }
    pub fn free_ctr(&mut self, p: CtrPtr, arity: usize) {
        self.free_block(p.0, 2 + arity);
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
    fn reuses_freed_block() {
        let mut m = Memory::new();
        let a = m.alloc_pair(Node::NULL, Node::NULL);
        let b = m.alloc_pair(Node::NULL, Node::NULL);
        assert_ne!(a.0, b.0);
        m.free_pair(a);
        // the next pair allocation reuses the reclaimed block
        let c = m.alloc_pair(Node::NULL, Node::NULL);
        assert_eq!(a.0, c.0);
        assert_ne!(b.0, c.0);
    }

    #[test]
    fn free_lists_are_size_segregated() {
        let mut m = Memory::new();
        let pair = m.alloc_pair(Node::NULL, Node::NULL);
        m.free_pair(pair);
        // a different shape does not draw from the pair free list
        let dup = m.alloc_dup(Label(0), Node::NULL);
        assert_ne!(pair.0, dup.0);
        // but a pair allocation does
        assert_eq!(m.alloc_pair(Node::NULL, Node::NULL).0, pair.0);
    }

    #[test]
    fn ctr_reuse_matches_on_arity() {
        let mut m = Memory::new();
        let c2 = m.alloc_ctr(2);
        m.free_ctr(c2, 2);
        // same arity reuses; different arity does not
        assert_ne!(m.alloc_ctr(3).0, c2.0);
        assert_eq!(m.alloc_ctr(2).0, c2.0);
    }
}
