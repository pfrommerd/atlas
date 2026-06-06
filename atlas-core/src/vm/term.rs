//! The packed [`Term`] word and its unpacked, idiomatic counterpart
//! [`TermValue`].
//!
//! Every term is a single 64-bit word:
//!
//! ```text
//! SUB (1 bit) | TAG (7 bits) | EXT (16 bits) | VAL (40 bits)
//! ```
//!
//! `VAL` is usually a *location* into the heap's `mem` array (a [`TermPtr`]).
//! [`TermValue`] is the structured view of that word — one variant per [`Tag`],
//! with strongly-typed fields ([`TermPtr`], [`Label`], [`DeBruijn`], …). Convert
//! between the two with the [`From`] impls:
//!
//! ```ignore
//! let t: Term = TermValue::App(ptr).into();
//! let v: TermValue = t.into();
//! ```
//!
//! [`Term::new`] is intentionally private to this module: terms are only ever
//! constructed by packing a [`TermValue`].

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
            Rem => "%", Eq => "==", Neq => "!=",
            Lt => "<",  Lte => "<=", Gt => ">", Gte => ">=",
            And => "&", Or => "|", Xor => "^",
            Shl => "<<", Shr => ">>", Invalid => "INVALID",
        }
    }
}

// --- newtypes ---

/// A location into the heap `mem` array (a packed term's `VAL`).
/// Only the lower 40 bits are used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TermPtr(pub u64);

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
pub struct Term(u64);

impl Term {
    /// Pack a tag/ext/val triple. Private: terms are built from [`TermValue`].
    fn new(tag: Tag, ext: u16, val: u64) -> Term {
        debug_assert!(val <= VAL_MASK);
        Term(((tag as u64) << TAG_SHIFT) | ((ext as u64) << EXT_SHIFT) | (val & VAL_MASK))
    }

    /// Reinterpret a stored raw word as a term (the heap storage boundary).
    /// SAFETY: The caller has ensured that raw word is valid.
    pub fn from_raw(raw: u64) -> Term {
        Term(raw)
    }
    /// The underlying raw word, for storing into the heap.
    pub fn raw(self) -> u64 {
        self.0
    }

    /// The null word, used for an empty (unbound) substitution slot.
    pub const NULL: Term = Term(0);

    pub fn is_null(&self) -> bool {
        self.0 == 0
    }
    /// Whether the `SUB` bit is set (this term is a substitution).
    pub fn is_sub(&self) -> bool {
        self.0 & SUB_BIT != 0
    }
    pub fn with_sub(self) -> Term {
        Term(self.0 | SUB_BIT)
    }
    pub fn clear_sub(self) -> Term {
        Term(self.0 & !SUB_BIT)
    }

    pub fn tag(&self) -> Tag {
        let tag = ((self.0 >> TAG_SHIFT) & 0x7F) as u8;
        assert!(tag < (Tag::Invalid as u8));
        // SAFETY: tag is checked to be valid above
        unsafe { std::mem::transmute(tag) }
    }
    pub fn ext(&self) -> u16 {
        ((self.0 >> EXT_SHIFT) & EXT_MASK) as u16
    }
    pub fn val(&self) -> u64 {
        self.0 & VAL_MASK
    }

    /// Unpack into a structured [`TermValue`].
    pub fn unpack(self) -> TermValue {
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

/// The structured, unpacked view of a [`Term`]: one variant per [`Tag`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TermValue {
    /// application node `[func, arg]`
    App(TermPtr),
    /// variable bound by a lambda; points at the binder's substitution slot
    Var(TermPtr),
    /// first / second projection of a duplication node
    Dp0 {
        label: Label,
        ptr: TermPtr,
    },
    Dp1 {
        label: Label,
        ptr: TermPtr,
    },
    /// lambda node `[bind, body]`
    Lam(TermPtr),
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
        ptr: TermPtr,
    },
    /// duplication binder node `[expr, body]`
    Dup {
        label: Label,
        ptr: TermPtr,
    },
    /// constructor `#Name{ fields.. }`
    Ctr {
        name: NameId,
        arity: u8,
        ptr: TermPtr,
    },
    /// pattern match
    Mat(MatchId),
    /// numeric switch
    Swi(MatchId),
    /// `use` (strict unbox) node `[fun]`
    Use(TermPtr),
    /// binary operation node `[lhs, rhs]`
    Bop {
        op: BinaryOp,
        ptr: TermPtr,
    },
    /// short-circuit `and` / `or` node `[lhs, rhs]`
    And(TermPtr),
    Or(TermPtr),
    /// erasure / wildcard
    Wld,
    /// dynamic superposition / duplication node
    Dsu(TermPtr),
    Ddu(TermPtr),
    /// unboxed number
    Num(u64),
}

impl From<TermValue> for Term {
    fn from(v: TermValue) -> Term {
        match v {
            TermValue::App(p) => Term::new(Tag::App, 0, p.0),
            TermValue::Var(p) => Term::new(Tag::Var, 0, p.0),
            TermValue::Dp0 { label, ptr } => Term::new(Tag::Dp0, label.0, ptr.0),
            TermValue::Dp1 { label, ptr } => Term::new(Tag::Dp1, label.0, ptr.0),
            TermValue::Lam(p) => Term::new(Tag::Lam, 0, p.0),
            TermValue::Bjv(i) => Term::new(Tag::Bjv, 0, i.0),
            TermValue::Bj0 { label, index } => Term::new(Tag::Bj0, label.0, index.0),
            TermValue::Bj1 { label, index } => Term::new(Tag::Bj1, label.0, index.0),
            TermValue::Sup { label, ptr } => Term::new(Tag::Sup, label.0, ptr.0),
            TermValue::Dup { label, ptr } => Term::new(Tag::Dup, label.0, ptr.0),
            TermValue::Ctr { name, arity, ptr } => {
                Term::new(Tag::Ctr, ctr_ext(name.0, arity), ptr.0)
            }
            TermValue::Mat(id) => Term::new(Tag::Mat, 0, id.0),
            TermValue::Swi(id) => Term::new(Tag::Swi, 0, id.0),
            TermValue::Use(p) => Term::new(Tag::Use, 0, p.0),
            TermValue::Bop { op, ptr } => Term::new(Tag::Bop, op as u16, ptr.0),
            TermValue::And(p) => Term::new(Tag::And, 0, p.0),
            TermValue::Or(p) => Term::new(Tag::Or, 0, p.0),
            TermValue::Wld => Term::new(Tag::Wld, 0, 0),
            TermValue::Dsu(p) => Term::new(Tag::Dsu, 0, p.0),
            TermValue::Ddu(p) => Term::new(Tag::Ddu, 0, p.0),
            TermValue::Num(n) => Term::new(Tag::Num, 0, n & VAL_MASK),
        }
    }
}

impl From<Term> for TermValue {
    fn from(t: Term) -> TermValue {
        let ext = t.ext();
        let val = t.val();
        match t.tag() {
            Tag::Null => panic!("Cannot unpack null term"),
            Tag::App => TermValue::App(TermPtr(val)),
            Tag::Var => TermValue::Var(TermPtr(val)),
            Tag::Dp0 => TermValue::Dp0 {
                label: Label(ext),
                ptr: TermPtr(val),
            },
            Tag::Dp1 => TermValue::Dp1 {
                label: Label(ext),
                ptr: TermPtr(val),
            },
            Tag::Lam => TermValue::Lam(TermPtr(val)),
            Tag::Bjv => TermValue::Bjv(DeBruijn(val)),
            Tag::Bj0 => TermValue::Bj0 {
                label: Label(ext),
                index: DeBruijn(val),
            },
            Tag::Bj1 => TermValue::Bj1 {
                label: Label(ext),
                index: DeBruijn(val),
            },
            Tag::Sup => TermValue::Sup {
                label: Label(ext),
                ptr: TermPtr(val),
            },
            Tag::Dup => TermValue::Dup {
                label: Label(ext),
                ptr: TermPtr(val),
            },
            Tag::Ctr => TermValue::Ctr {
                name: NameId(ctr_name(ext)),
                arity: ctr_arity(ext),
                ptr: TermPtr(val),
            },
            Tag::Mat => TermValue::Mat(MatchId(val)),
            Tag::Swi => TermValue::Swi(MatchId(val)),
            Tag::Use => TermValue::Use(TermPtr(val)),
            Tag::Bop => TermValue::Bop {
                op: BinaryOp::from(ext),
                ptr: TermPtr(val),
            },
            Tag::And => TermValue::And(TermPtr(val)),
            Tag::Or => TermValue::Or(TermPtr(val)),
            Tag::Wld => TermValue::Wld,
            Tag::Dsu => TermValue::Dsu(TermPtr(val)),
            Tag::Ddu => TermValue::Ddu(TermPtr(val)),
            Tag::Num => TermValue::Num(val),
            Tag::Invalid => panic!("cannot unpack an Invalid term"),
        }
    }
}
