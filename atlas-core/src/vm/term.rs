//! The packed [`Node`] word and its unpacked, idiomatic counterpart [`Term`].
//!
//! Every node is a single 64-bit word:
//!
//! ```text
//! SUB (1 bit) | TAG (7 bits) | VAL (56 bits)
//! ```
//!
//! `VAL` is usually a *location* into the heap's `mem` array (a [`NodePtr`],
//! [`PairPtr`], [`TriplePtr`], or [`QuadPtr`] depending on the node's shape).
//! Metadata that used to live in a separate `EXT` field — duplication/constructor
//! labels, constructor arity, the binary operator — is instead stored as the
//! leading cells of the node's allocation, each a small meta-term ([`Term::Label`],
//! [`Term::Arity`], [`Term::OpMeta`]). For example a `Sup` allocation is the
//! triple `[Label, left, right]`, a `Dup` allocation the quad
//! `[Label, val, sub0, sub1]`, and a `Ctr` allocation `[Label, Arity, fields..]`.
//!
//! [`Term`] is the structured view of a word — one variant per [`Tag`], with
//! strongly-typed fields ([`NodePtr`], [`DeBruijn`], …). Convert between the two
//! with the [`From`] impls:
//!
//! ```ignore
//! let n: Node = Term::App(ptr).into();
//! let t: Term = n.into();
//! ```
//!
//! [`Node::new`] is intentionally private to this module: nodes are only ever
//! constructed by packing a [`Term`].

use crate::core::expr::DeBruijn;

// --- operators ---

#[rustfmt::skip]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum BinaryOp {
    Add, Sub, Mul, Div, Mod,
    Eq, Neq, Lt, Lte, Gt, Gte,
    And, Or, Xor, Shl, Shr, Invalid
}

impl From<u16> for BinaryOp {
    fn from(mut x: u16) -> Self {
        x = std::cmp::min(x, BinaryOp::Invalid as u16);
        // SAFETY: `x` is now guaranteed to be in the valid range `[0, LAST]`
        unsafe { std::mem::transmute(x) }
    }
}
impl From<BinaryOp> for u16 {
    fn from(op: BinaryOp) -> Self {
        op as u16
    }
}

impl BinaryOp {
    #[rustfmt::skip]
    pub fn symbol(self) -> &'static str {
        use BinaryOp::*;
        match self {
            Add => "+", Sub => "-", Mul => "*", Div => "/",
            Mod => "%", Eq => "==", Neq => "!=",
            Lt => "<",  Lte => "<=", Gt => ">", Gte => ">=",
            And => "&", Or => "|", Xor => "^",
            Shl => "<<", Shr => ">>", Invalid => "INVALID",
        }
    }
}

// --- newtypes ---

/// A location into the heap `mem` array pointing at a *single* cell — a
/// substitution slot or a constructor's field base. Only the lower 40 bits are
/// used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodePtr(pub u64);

impl NodePtr {
    /// The cell `n` slots after this one.
    pub fn offset(self, n: u64) -> NodePtr {
        NodePtr(self.0 + n)
    }
}

/// A location pointing at a two-cell binary node (`first`, `second`), as built
/// by `Heap::pair` (applications, lambdas, superpositions, binary ops, …).
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
/// `Heap::dup_node`. The cells are `[Label, val, sub0, sub1]`: a leading
/// [`Term::Label`] meta-cell, the duplicated value, and the two substitution
/// slots the `Dp0`/`Dp1` projections read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QuadPtr(pub u64);

impl QuadPtr {
    /// The leading [`Term::Label`] cell.
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

/// A duplication / superposition label.
/// Stored in a [`Term::LabelMeta`] meta-cell,
/// so it may use the full 56-bit `VAL` width.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Label(pub u64);

/// Stored in a [`Term::LabelMeta`] meta-cell,
/// so it may use the full 56-bit `VAL` width.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NameId(pub u64);

/// A duplication / superposition label.
/// Stored in the upper half of a Bj0/Bj1
/// so it may use the full 56-bit `VAL` width.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StaticLabel(pub u16);

/// A constructor arity. Stored in a [`Term::Arity`] meta-cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Arity(pub u64);

/// An index into the heap's match table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MatchId(pub u64);

// --- packed term ---

const SUB_BIT: u64 = 1 << 63;
const TAG_SHIFT: u64 = 56;
const VAL_MASK: u64 = (1 << 56) - 1;

/// A packed interaction-calculus term (see the module docs for the layout).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Node(u64);

impl Node {
    /// Pack a tag/val pair. Private: nodes are built from [`Term`].
    fn new(tag: Tag, val: u64) -> Node {
        debug_assert!(val <= VAL_MASK);
        Node(((tag as u64) << TAG_SHIFT) | (val & VAL_MASK))
    }

    /// Reinterpret a stored raw word as a term (the heap storage boundary).
    /// SAFETY: The caller has ensured that raw word is valid.
    pub fn from_raw(raw: u64) -> Node {
        Node(raw)
    }
    /// The underlying raw word, for storing into the heap.
    pub fn raw(self) -> u64 {
        self.0
    }

    /// The null word, used for an empty (unbound) substitution slot.
    pub const NULL: Node = Node(0);

    pub fn is_null(&self) -> bool {
        self.0 == 0
    }

    // The bit-level accessors below are private: the packed layout is an
    // implementation detail of this module. Everything outside `term` inspects a
    // node through [`Node::unpack`] and the typed [`Term`] payloads.

    /// Whether the `SUB` bit is set (this node is a substitution).
    fn is_sub(self) -> bool {
        self.0 & SUB_BIT != 0
    }
    fn with_sub(self) -> Node {
        Node(self.0 | SUB_BIT)
    }
    fn clear_sub(self) -> Node {
        Node(self.0 & !SUB_BIT)
    }

    fn tag(self) -> Tag {
        let tag = ((self.0 >> TAG_SHIFT) & 0x7F) as u8;
        assert!(tag < (Tag::Invalid as u8));
        // SAFETY: tag is checked to be valid above
        unsafe { std::mem::transmute(tag) }
    }
    fn val(self) -> u64 {
        self.0 & VAL_MASK
    }

    /// Unpack into a structured [`Term`].
    pub fn unpack(self) -> Term {
        self.into()
    }

    /// Decode a leading meta-cell as a [`Label`] (the dup/sup label).
    pub fn as_label(self) -> Label {
        match self.unpack() {
            Term::LabelMeta(l) => l,
            _ => unreachable!("expected a Label meta-cell"),
        }
    }
    /// Decode a leading meta-cell as a constructor [`NameId`].
    pub fn as_name(self) -> NameId {
        match self.unpack() {
            Term::NameMeta(n) => n,
            _ => unreachable!("expected a Name meta-cell"),
        }
    }
    /// Decode a meta-cell as a constructor [`Arity`].
    pub fn as_arity(self) -> Arity {
        match self.unpack() {
            Term::ArityMeta(a) => a,
            _ => unreachable!("expected an Arity meta-cell"),
        }
    }
    /// Decode a leading meta-cell as a [`BinaryOp`].
    pub fn as_op(self) -> BinaryOp {
        match self.unpack() {
            Term::OpMeta(op) => op,
            _ => unreachable!("expected an OpMeta meta-cell"),
        }
    }
}

#[rustfmt::skip]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Tag {
    /// invalid (null)
    Null,
    /// primitive types
    Num,
    // builtin nodes
    App, Var,
    Dp0, Dp1,
    Lam, Bjv, Bj0, Bj1,
    Sup, Dup, Ctr,
    Mat, Swi, Use, Bop,
    // Special short-circuit operators
    And, Or,
    Wld, Dsu, Ddu,
    // Meta-cells stored as the leading slots of an allocation.
    LabelMeta, NameMeta, ArityMeta, OpMeta,
    Invalid,
}

// --- quoted dup-variable packing ---
//
// `Bj0`/`Bj1` (static, quoted duplication variables) have no allocation of their
// own, so they pack their label into the high 16 bits of `VAL` above a 40-bit
// de Bruijn index. Unlike a heap-stored [`Label`], a quoted label must therefore
// fit in 16 bits and its index in 40.

const BJ_LABEL_SHIFT: u64 = 40;
const BJ_INDEX_MASK: u64 = (1 << 40) - 1;
const BJ_LABEL_MASK: u64 = (1 << 16) - 1;

fn bj_val(label: Label, index: DeBruijn) -> u64 {
    debug_assert!(label.0 <= BJ_LABEL_MASK, "quoted Bj label exceeds 16 bits");
    debug_assert!(index.0 <= BJ_INDEX_MASK, "quoted Bj index exceeds 40 bits");
    ((label.0 & BJ_LABEL_MASK) << BJ_LABEL_SHIFT) | (index.0 & BJ_INDEX_MASK)
}
fn bj_label(val: u64) -> Label {
    Label((val >> BJ_LABEL_SHIFT) & BJ_LABEL_MASK)
}
fn bj_index(val: u64) -> DeBruijn {
    DeBruijn(val & BJ_INDEX_MASK)
}

// --- unpacked term ---

/// The structured, unpacked view of a heap [`Node`]: one variant per [`Tag`],
/// plus [`Term::Sub`] / [`Term::Null`] for the raw words a substitution slot can
/// hold. Reading a cell through [`Node::unpack`] is therefore total.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Term {
    /// application node `[func, arg]`
    App(PairPtr),
    /// variable bound by a lambda; points at the binder's substitution slot
    Var(NodePtr),
    /// first / second projection of a duplication node; the label lives in the
    /// node's leading [`Term::Label`] cell ([`QuadPtr::label`]).
    Dp0(QuadPtr),
    Dp1(QuadPtr),
    /// lambda node `[bind, body]`
    Lam(PairPtr),
    /// quoted (static) lambda variable
    Bjv(DeBruijn),
    /// quoted (static) duplication variables (label + index packed into `VAL`)
    Bj0 {
        label: Label,
        index: DeBruijn,
    },
    Bj1 {
        label: Label,
        index: DeBruijn,
    },
    /// superposition node `[Label, left, right]`
    Sup(TriplePtr),
    /// duplication binder node `[Label, val, sub0, sub1]`
    Dup(QuadPtr),
    /// constructor `#Name{ fields.. }`; the allocation is `[Label, Arity, fields..]`
    Ctr(NodePtr),
    /// pattern match
    Mat(MatchId),
    /// numeric switch
    Swi(MatchId),
    /// `use` (strict unbox) node `[fun]`
    Use(NodePtr),
    /// binary operation node `[OpMeta, lhs, rhs]`
    Bop(TriplePtr),
    /// short-circuit `and` / `or` node `[lhs, rhs]`
    And(PairPtr),
    Or(PairPtr),
    /// erasure / wildcard
    Wld,
    /// dynamic superposition (`[left, right]`) / duplication (`[val, sub0, sub1]`)
    Dsu(PairPtr),
    Ddu(TriplePtr),
    /// a label / interned-name id meta-cell (the leading slot of a sup, dup, or
    /// constructor allocation)
    LabelMeta(Label),
    NameMeta(NameId),
    /// a constructor-arity meta-cell
    ArityMeta(Arity),
    /// a binary-operator meta-cell (the leading slot of a `Bop` allocation)
    OpMeta(BinaryOp),
    /// unboxed number
    Num(u64),
    /// a consumed binder slot: holds the substituted term (`SUB` bit cleared)
    Sub(Node),
    /// an empty (unbound) slot — the null word
    Null,
}

impl Term {
    /// Pack this view back into a heap [`Node`] word.
    pub fn pack(self) -> Node {
        self.into()
    }
}

impl From<Term> for Node {
    fn from(v: Term) -> Node {
        match v {
            Term::App(p) => Node::new(Tag::App, p.0),
            Term::Var(p) => Node::new(Tag::Var, p.0),
            Term::Dp0(p) => Node::new(Tag::Dp0, p.0),
            Term::Dp1(p) => Node::new(Tag::Dp1, p.0),
            Term::Lam(p) => Node::new(Tag::Lam, p.0),
            Term::Bjv(i) => Node::new(Tag::Bjv, i.0),
            Term::Bj0 { label, index } => Node::new(Tag::Bj0, bj_val(label, index)),
            Term::Bj1 { label, index } => Node::new(Tag::Bj1, bj_val(label, index)),
            Term::Sup(p) => Node::new(Tag::Sup, p.0),
            Term::Dup(p) => Node::new(Tag::Dup, p.0),
            Term::Ctr(p) => Node::new(Tag::Ctr, p.0),
            Term::Mat(id) => Node::new(Tag::Mat, id.0),
            Term::Swi(id) => Node::new(Tag::Swi, id.0),
            Term::Use(p) => Node::new(Tag::Use, p.0),
            Term::Bop(p) => Node::new(Tag::Bop, p.0),
            Term::And(p) => Node::new(Tag::And, p.0),
            Term::Or(p) => Node::new(Tag::Or, p.0),
            Term::Wld => Node::new(Tag::Wld, 0),
            Term::Dsu(p) => Node::new(Tag::Dsu, p.0),
            Term::Ddu(p) => Node::new(Tag::Ddu, p.0),
            Term::LabelMeta(l) => Node::new(Tag::LabelMeta, l.0),
            Term::NameMeta(l) => Node::new(Tag::NameMeta, l.0),
            Term::ArityMeta(a) => Node::new(Tag::ArityMeta, a.0),
            Term::OpMeta(op) => Node::new(Tag::OpMeta, op as u64),
            Term::Num(n) => Node::new(Tag::Num, n & VAL_MASK),
            Term::Sub(n) => n.with_sub(),
            Term::Null => Node::NULL,
        }
    }
}

impl From<Node> for Term {
    fn from(t: Node) -> Term {
        // A `SUB`-tagged cell holds a substituted term; surface it as `Sub` so
        // the underlying tag bits aren't mistaken for a live node.
        if t.is_sub() {
            return Term::Sub(t.clear_sub());
        }
        let val = t.val();
        match t.tag() {
            Tag::Null => Term::Null,
            Tag::App => Term::App(PairPtr(val)),
            Tag::Var => Term::Var(NodePtr(val)),
            Tag::Dp0 => Term::Dp0(QuadPtr(val)),
            Tag::Dp1 => Term::Dp1(QuadPtr(val)),
            Tag::Lam => Term::Lam(PairPtr(val)),
            Tag::Bjv => Term::Bjv(DeBruijn(val)),
            Tag::Bj0 => Term::Bj0 {
                label: bj_label(val),
                index: bj_index(val),
            },
            Tag::Bj1 => Term::Bj1 {
                label: bj_label(val),
                index: bj_index(val),
            },
            Tag::Sup => Term::Sup(TriplePtr(val)),
            Tag::Dup => Term::Dup(QuadPtr(val)),
            Tag::Ctr => Term::Ctr(NodePtr(val)),
            Tag::Mat => Term::Mat(MatchId(val)),
            Tag::Swi => Term::Swi(MatchId(val)),
            Tag::Use => Term::Use(NodePtr(val)),
            Tag::Bop => Term::Bop(TriplePtr(val)),
            Tag::And => Term::And(PairPtr(val)),
            Tag::Or => Term::Or(PairPtr(val)),
            Tag::Wld => Term::Wld,
            Tag::Dsu => Term::Dsu(PairPtr(val)),
            Tag::Ddu => Term::Ddu(TriplePtr(val)),
            Tag::LabelMeta => Term::LabelMeta(Label(val)),
            Tag::NameMeta => Term::NameMeta(NameId(val)),
            Tag::ArityMeta => Term::ArityMeta(Arity(val)),
            Tag::OpMeta => Term::OpMeta(BinaryOp::from(val as u16)),
            Tag::Num => Term::Num(val),
            Tag::Invalid => panic!("cannot unpack an Invalid term"),
        }
    }
}
