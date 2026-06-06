//! The packed [`Node`] word and its unpacked, idiomatic counterpart [`Term`].
//!
//! Every node is a single 64-bit word:
//!
//! ```text
//! SUB (1 bit) | TAG (7 bits) | EXT (16 bits) | VAL (40 bits)
//! ```
//!
//! `VAL` is usually a *location* into the heap's `mem` array (a [`NodePtr`],
//! [`PairPtr`], or [`TriplePtr`] depending on the node's shape). [`Term`] is the
//! structured view of that word — one variant per [`Tag`], with strongly-typed
//! fields ([`NodePtr`], [`Label`], [`DeBruijn`], …). Convert between the two with
//! the [`From`] impls:
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
/// by `Heap::node2` (applications, lambdas, superpositions, binary ops, …).
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

/// A location pointing at a three-cell duplication node (`first` = value,
/// `second` = sub0, `third` = sub1), as built by `Heap::dup_node`.
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

/// A duplication / superposition label (an interned name id).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Label(pub u16);

/// An interned constructor name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NameId(pub u16);

/// An index into the heap's match table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MatchId(pub u64);

// --- packed term ---

const SUB_BIT: u64 = 1 << 63;
const TAG_SHIFT: u64 = 56;
const EXT_SHIFT: u64 = 40;
const EXT_MASK: u64 = 0xFFFF;
const VAL_MASK: u64 = (1 << 40) - 1;

/// A packed interaction-calculus term (see the module docs for the layout).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Node(u64);

impl Node {
    /// Pack a tag/ext/val triple. Private: nodes are built from [`Term`].
    fn new(tag: Tag, ext: u16, val: u64) -> Node {
        debug_assert!(val <= VAL_MASK);
        Node(((tag as u64) << TAG_SHIFT) | ((ext as u64) << EXT_SHIFT) | (val & VAL_MASK))
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
    fn ext(self) -> u16 {
        ((self.0 >> EXT_SHIFT) & EXT_MASK) as u16
    }
    fn val(self) -> u64 {
        self.0 & VAL_MASK
    }

    /// Unpack into a structured [`Term`].
    pub fn unpack(self) -> Term {
        self.into()
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
    Invalid,
}

// --- constructor metadata packing ---
//
// A `Ctr` term packs `name_id` in the low 12 bits of EXT and the arity in the
// high 4 bits; its `VAL` points to `arity` consecutive field slots.

const CTR_ARITY_SHIFT: u16 = 12;
const CTR_NAME_MASK: u16 = (1 << 12) - 1;

fn ctr_ext(name_id: u16, arity: u8) -> u16 {
    debug_assert!(name_id <= CTR_NAME_MASK);
    debug_assert!(arity < 16);
    ((arity as u16) << CTR_ARITY_SHIFT) | (name_id & CTR_NAME_MASK)
}
fn ctr_name(ext: u16) -> u16 {
    ext & CTR_NAME_MASK
}
fn ctr_arity(ext: u16) -> u8 {
    (ext >> CTR_ARITY_SHIFT) as u8
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
    /// first / second projection of a duplication node
    Dp0 {
        label: Label,
        ptr: TriplePtr,
    },
    Dp1 {
        label: Label,
        ptr: TriplePtr,
    },
    /// lambda node `[bind, body]`
    Lam(PairPtr),
    /// quoted (static) lambda variable
    Bjv(DeBruijn),
    /// quoted (static) duplication variables
    Bj0 {
        label: Label,
        index: DeBruijn,
    },
    Bj1 {
        label: Label,
        index: DeBruijn,
    },
    /// superposition node `[left, right]`
    Sup {
        label: Label,
        ptr: PairPtr,
    },
    /// duplication binder node `[val, sub0, sub1]`
    Dup {
        label: Label,
        ptr: TriplePtr,
    },
    /// constructor `#Name{ fields.. }`
    Ctr {
        name: NameId,
        arity: u8,
        ptr: NodePtr,
    },
    /// pattern match
    Mat(MatchId),
    /// numeric switch
    Swi(MatchId),
    /// `use` (strict unbox) node `[fun]`
    Use(NodePtr),
    /// binary operation node `[lhs, rhs]`
    Bop {
        op: BinaryOp,
        ptr: PairPtr,
    },
    /// short-circuit `and` / `or` node `[lhs, rhs]`
    And(PairPtr),
    Or(PairPtr),
    /// erasure / wildcard
    Wld,
    /// dynamic superposition (`[left, right]`) / duplication (`[val, sub0, sub1]`)
    Dsu(PairPtr),
    Ddu(TriplePtr),
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
            Term::App(p) => Node::new(Tag::App, 0, p.0),
            Term::Var(p) => Node::new(Tag::Var, 0, p.0),
            Term::Dp0 { label, ptr } => Node::new(Tag::Dp0, label.0, ptr.0),
            Term::Dp1 { label, ptr } => Node::new(Tag::Dp1, label.0, ptr.0),
            Term::Lam(p) => Node::new(Tag::Lam, 0, p.0),
            Term::Bjv(i) => Node::new(Tag::Bjv, 0, i.0),
            Term::Bj0 { label, index } => Node::new(Tag::Bj0, label.0, index.0),
            Term::Bj1 { label, index } => Node::new(Tag::Bj1, label.0, index.0),
            Term::Sup { label, ptr } => Node::new(Tag::Sup, label.0, ptr.0),
            Term::Dup { label, ptr } => Node::new(Tag::Dup, label.0, ptr.0),
            Term::Ctr { name, arity, ptr } => Node::new(Tag::Ctr, ctr_ext(name.0, arity), ptr.0),
            Term::Mat(id) => Node::new(Tag::Mat, 0, id.0),
            Term::Swi(id) => Node::new(Tag::Swi, 0, id.0),
            Term::Use(p) => Node::new(Tag::Use, 0, p.0),
            Term::Bop { op, ptr } => Node::new(Tag::Bop, op as u16, ptr.0),
            Term::And(p) => Node::new(Tag::And, 0, p.0),
            Term::Or(p) => Node::new(Tag::Or, 0, p.0),
            Term::Wld => Node::new(Tag::Wld, 0, 0),
            Term::Dsu(p) => Node::new(Tag::Dsu, 0, p.0),
            Term::Ddu(p) => Node::new(Tag::Ddu, 0, p.0),
            Term::Num(n) => Node::new(Tag::Num, 0, n & VAL_MASK),
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
        let ext = t.ext();
        let val = t.val();
        match t.tag() {
            Tag::Null => Term::Null,
            Tag::App => Term::App(PairPtr(val)),
            Tag::Var => Term::Var(NodePtr(val)),
            Tag::Dp0 => Term::Dp0 {
                label: Label(ext),
                ptr: TriplePtr(val),
            },
            Tag::Dp1 => Term::Dp1 {
                label: Label(ext),
                ptr: TriplePtr(val),
            },
            Tag::Lam => Term::Lam(PairPtr(val)),
            Tag::Bjv => Term::Bjv(DeBruijn(val)),
            Tag::Bj0 => Term::Bj0 {
                label: Label(ext),
                index: DeBruijn(val),
            },
            Tag::Bj1 => Term::Bj1 {
                label: Label(ext),
                index: DeBruijn(val),
            },
            Tag::Sup => Term::Sup {
                label: Label(ext),
                ptr: PairPtr(val),
            },
            Tag::Dup => Term::Dup {
                label: Label(ext),
                ptr: TriplePtr(val),
            },
            Tag::Ctr => Term::Ctr {
                name: NameId(ctr_name(ext)),
                arity: ctr_arity(ext),
                ptr: NodePtr(val),
            },
            Tag::Mat => Term::Mat(MatchId(val)),
            Tag::Swi => Term::Swi(MatchId(val)),
            Tag::Use => Term::Use(NodePtr(val)),
            Tag::Bop => Term::Bop {
                op: BinaryOp::from(ext),
                ptr: PairPtr(val),
            },
            Tag::And => Term::And(PairPtr(val)),
            Tag::Or => Term::Or(PairPtr(val)),
            Tag::Wld => Term::Wld,
            Tag::Dsu => Term::Dsu(PairPtr(val)),
            Tag::Ddu => Term::Ddu(TriplePtr(val)),
            Tag::Num => Term::Num(val),
            Tag::Invalid => panic!("cannot unpack an Invalid term"),
        }
    }
}
