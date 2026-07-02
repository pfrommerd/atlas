use ordered_float::OrderedFloat;
use std::marker::PhantomData;

use super::heap::{
    BodyPtr, MatchPtr, PackPtr, RefPtr, SupPtr, TermPtr, TracePtr, TypePtr, ValuePtr,
};
pub use crate::util::U56;
pub use crate::util::slab::UniqueKey;

/// An invariant lifetime brand: invariant in `'h` (neither co- nor
/// contravariant), and `Send + Sync`.
pub type Brand<'h> = PhantomData<fn(&'h ()) -> &'h ()>;

// --- operators ---
#[rustfmt::skip]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BinaryOp {
    Add, Sub, Mul, Div, Mod,
    Eq, Neq, Lt, Lte, Gt, Gte,
    And, Or, Xor, Shl, Shr, IDiv, Invalid
}

impl TryFrom<u8> for BinaryOp {
    type Error = ();
    fn try_from(x: u8) -> Result<Self, ()> {
        if x > BinaryOp::Invalid as u8 {
            return Err(());
        }
        Ok(unsafe { std::mem::transmute(x) })
    }
}
impl From<BinaryOp> for u8 {
    fn from(op: BinaryOp) -> Self {
        op as u8
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
            Shl => "<<", Shr => ">>", IDiv => "~/", Invalid => "INVALID",
        }
    }
}

#[rustfmt::skip]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum UnaryOp {
    Not, Neg, TypeOf, Invalid
}

impl TryFrom<u8> for UnaryOp {
    type Error = ();
    fn try_from(x: u8) -> Result<Self, ()> {
        if x > UnaryOp::Invalid as u8 {
            return Err(());
        }
        Ok(unsafe { std::mem::transmute(x) })
    }
}
impl From<UnaryOp> for u8 {
    fn from(op: UnaryOp) -> Self {
        op as u8
    }
}

impl UnaryOp {
    pub fn symbol(self) -> &'static str {
        use UnaryOp::*;
        match self {
            Not => "~",
            Neg => "-",
            TypeOf => "typeof ",
            Invalid => "INVALID",
        }
    }
}

// --- newtypes ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LabelId(U56);

impl LabelId {
    pub fn from_u56(x: U56) -> Self {
        LabelId(x)
    }
    pub fn to_u56(self) -> U56 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PrimId(U56);

impl PrimId {
    pub fn new(n: u64) -> Self {
        PrimId(U56::new(n))
    }
    pub fn get(self) -> u64 {
        self.0.to_u64()
    }
}

/// An interned variant name (e.g. `Cons`, `Nil`), addressed by the heap's string
/// interner. Two variant patterns with equal names share an id, so a constructor's
/// variant can be matched by comparing ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VariantId(U56);

impl VariantId {
    pub fn from_u56(x: U56) -> Self {
        VariantId(x)
    }
    pub fn addr(self) -> U56 {
        self.0
    }
}

// --- ptr types ---

// --- unpacked term ---

/// The structured, unpacked view of a heap [`Node`]: one variant per [`Tag`], plus
/// [`Term::Sub`] / [`Term::Null`] for the raw words a substitution slot can hold.
///
/// `Term` is the executor's working currency: the engine deals in `Term`, not the
/// packed [`Node`]. It is `Copy` (every field is an id, an address handle, or a
/// packed word); the no-aliasing invariant for plain cells is the calculus's
/// linearity, upheld by the engine (see [`TermPtr`]).
#[rustfmt::skip]
#[derive(Debug, PartialEq, Eq)]
pub enum Term<'h> {
    /// application node `[func, arg]`
    App { func: TermPtr<'h>, arg: TermPtr<'h>},
    /// an *unsubstituted* variable
    Var,
    /// A lambda body is a combination of a pointer to the body and
    /// a pointer to the binder slot. Before the body TermPtr
    /// can be accessed, the binder slot must be substituted (or not).
    Lam { body: BodyPtr<'h> },
    /// erasing lambda `\_ -> v`: applied, it erases its argument and returns `v`.
    Use { body: TermPtr<'h> },
    /// one projection of a duplication. The [`RefPtr`] names the wire's own
    /// *refcell* entry (which holds this wire's fired result and the handle to
    /// the shared, Arc'd `DupCell` fan — repointed when fans merge) and the
    /// projection's (per-wire) label.
    Ref { ptr: RefPtr<'h>},
    /// superposition node -- names a [`SupPtr`] cell holding the superposed parts,
    /// each keyed by its (per-wire) label. Arises from duplicating a function.
    Sup { ptr: SupPtr<'h>},
    /// a *saturated* construction `(ty)::Ctr{ fields.. }`. `ty` is the (affine)
    /// type value; `values` holds the field nodes and `arity` their count; the
    /// variant (if any) is the values pack's [`name`](crate::vm::heap::Pack).
    Ctn { ty: TypePtr<'h>, arity: u8, values: PackPtr<'h> },
    /// a partially-applied callable. `func` points to the callable node — a
    /// [`Term::Ctr`] (constructor) or a [`Term::Pri`] (primitive); `args` are the
    /// arguments gathered so far (`args.len() < arity`) and `arity` the count
    /// needed to complete. Completing builds a [`Term::Ctn`] or fires the primitive.
    Partial { func: TermPtr<'h>, arity: u8, args: PackPtr<'h> },
    /// a constructor selector `expr :: Name` (a value). `ty` is a *possibly-
    /// unevaluated* type expression; `variant` is `None` for the product
    /// constructor (`::New`) and `Some(name)` for a sum variant. Applying it
    /// accumulates args via [`Term::Partial`]; a nullary constructor reduces
    /// directly to a [`Term::Ctn`].
    Ctr { ty: TermPtr<'h>, variant: Option<VariantId> },
    /// an inert match key naming a constructor variant (the `Cons` in `?{Cons -> ..}`).
    VarId(VariantId),
    /// pattern match against constructors or primitive values. The cases (pattern
    /// keys and branch lambdas) live in the [`MatchData`](crate::vm::heap::MatchData)
    /// behind `matches`.
    Mat { matches: MatchPtr<'h> },
    /// binary operation node `[OpMeta, lhs, rhs]`
    Bop { op: BinaryOp, lhs: TermPtr<'h>, rhs: TermPtr<'h> },
    /// unary operation node `[op, val]`
    Uop { op: UnaryOp, val: TermPtr<'h> },
    /// short-circuit `and` / `or` node `[lhs, rhs]`
    And { lhs: TermPtr<'h>, rhs: TermPtr<'h> },
    Or { lhs: TermPtr<'h>, rhs: TermPtr<'h> },
    /// wildcard (`*` / `_`): an inert atom. Could be anything!
    Wld,
    /// err: a first-class eraser; it annihilates whatever it interacts with.
    Err { immediate: bool, backtrace: Option<TracePtr<'h>> },
    // basic types, all stored in the "val" portion of the packed node
    Int(i64), Float(OrderedFloat<f64>),
    Char(char), Bool(bool),
    // a boxed value, identified by its 'ValuePtr'
    Box(ValuePtr<'h>),
    /// a first-class type value, identified by its [`TypePtr`].
    Type(TypePtr<'h>),
    /// a host-provided primitive function, identified by its [`PrimId`].
    /// The behavior is dependent on the Execution environment,
    /// not the underlying heap and so is not branded.
    Pri(PrimId),
    /// an empty (unbound) slot — the null word
    Null,
}

// --- packed term ---

#[rustfmt::skip]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Tag {
    /// invalid (null)
    Null,
    // builtin nodes
    App, Var, Lam,
    Ref, Sup, Ctn,
    Mat, Swi, Use, Bop, Uop,
    // Special short-circuit operators
    And, Or,
    Wld, Err,
    /// a host-provided primitive function
    Pri,
    /// primitive types
    Int, Float,
    Char, Bool, Box,
    /// a first-class type value
    Type,
    /// a partially-applied callable
    Partial,
    /// a constructor selector `expr :: Name`
    Ctr,
    /// an inert constructor-variant match key
    VarId,
    Invalid,
}

const TAG_SHIFT: u8 = 64 + 56;
const EXT_SHIFT: u8 = 64;
const EXT_MASK: u128 = ((1 << 56) - 1) << 64;
const VAL_MASK: u128 = (1 << 64) - 1;
const VALTAG_MASK: u128 = ((1 << 8) - 1) << 56;
const VALEXT_MASK: u128 = (1 << 56) - 1;

// A packed node is 128 bits
// The first 8 bits are the tag,
// the next 56 are the "ext"
// the next 64 bits are the "val"
//
// SAFETY: The only invariant for the type
// is that the first 8 bits must be a valid Tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct Node(u128);

impl Node {
    pub const NULL: Node = Node(0);

    pub fn from_all(tag: Tag, ext: U56, val_tag: u8, val_ext: U56) -> Node {
        Node(
            ((tag as u128) << TAG_SHIFT)
                | ((ext.to_u64() as u128) << EXT_SHIFT)
                | ((val_tag as u128) << 56)
                | (val_ext.to_u64() as u128),
        )
    }
    pub fn from_tag(tag: Tag) -> Node {
        Node((tag as u128) << TAG_SHIFT)
    }
    pub fn from_tag_val(tag: Tag, val: u64) -> Node {
        Node(((tag as u128) << TAG_SHIFT) | (val as u128))
    }
    pub fn from_tag_valext(tag: Tag, valext: U56) -> Node {
        Node(((tag as u128) << TAG_SHIFT) | (valext.to_u64() as u128))
    }
    pub fn from_tag_ext_val(tag: Tag, ext: U56, val: u64) -> Node {
        Node(((tag as u128) << TAG_SHIFT) | ((ext.to_u64() as u128) << 64) | (val as u128))
    }
    pub fn from_tag_ext_valext(tag: Tag, ext: U56, valext: U56) -> Node {
        Node(
            ((tag as u128) << TAG_SHIFT)
                | ((ext.to_u64() as u128) << EXT_SHIFT)
                | (valext.to_u64() as u128),
        )
    }
    pub unsafe fn from_raw(raw: u128) -> Node {
        Node(raw)
    }
    /// The underlying raw word, for storing into the heap.
    pub fn raw_u128(self) -> u128 {
        self.0
    }
    pub fn is_null(&self) -> bool {
        self.0 == 0
    }
    fn tag(&self) -> Tag {
        let tag = ((self.0 >> TAG_SHIFT) & 0x7F) as u8;
        // SAFETY: Tag portion is valid by
        // the invariants of this type.
        debug_assert!(tag < (Tag::Invalid as u8));
        unsafe { std::mem::transmute(tag) }
    }
    fn ext(&self) -> U56 {
        U56::new(((self.0 & EXT_MASK) >> 64) as u64)
    }
    fn val(&self) -> u64 {
        (self.0 & VAL_MASK) as u64
    }
    fn val_tag(&self) -> u8 {
        ((self.0 & VALTAG_MASK) >> 56) as u8
    }
    fn val_ext(&self) -> U56 {
        U56::new((self.0 & VALEXT_MASK) as u64)
    }

    // SAFETY: In order to use this method, the caller must ensure
    // that the underlying Node is valid for heap scope 'h.
    pub unsafe fn unpack<'h>(self) -> Term<'h> {
        let tag = self.tag();
        let ext = self.ext();
        let val = self.val();
        let valtag = self.val_tag();
        let valext = self.val_ext();
        // SAFETY: The caller guarantees us that
        unsafe {
            match tag {
                Tag::Null => Term::Null,
                Tag::Var => Term::Var,
                Tag::Lam => Term::Lam {
                    body: BodyPtr::forge(ext, valext),
                },
                Tag::App => Term::App {
                    func: TermPtr::forge(ext),
                    arg: TermPtr::forge(valext),
                },
                Tag::Ref => Term::Ref {
                    // ext = the projection's wire label; valext = the dup cell.
                    ptr: RefPtr::forge(valext, LabelId(ext)),
                },
                Tag::Sup => Term::Sup {
                    ptr: SupPtr::forge(valext),
                },
                Tag::Use => Term::Use {
                    body: TermPtr::forge(valext),
                },
                Tag::Ctn => Term::Ctn {
                    ty: TypePtr::forge(ext),
                    arity: valtag,
                    values: PackPtr::forge(valext),
                },
                Tag::Partial => Term::Partial {
                    func: TermPtr::forge(ext),
                    arity: valtag,
                    args: PackPtr::forge(valext),
                },
                Tag::Ctr => Term::Ctr {
                    ty: TermPtr::forge(ext),
                    // `valtag` is the present/absent flag for the optional variant.
                    variant: (valtag != 0).then(|| VariantId::from_u56(valext)),
                },
                Tag::VarId => Term::VarId(VariantId::from_u56(valext)),
                Tag::Mat => Term::Mat {
                    matches: MatchPtr::forge(ext),
                },
                Tag::Bop => Term::Bop {
                    op: BinaryOp::try_from(valtag).unwrap_unchecked(),
                    lhs: TermPtr::forge(ext),
                    rhs: TermPtr::forge(valext),
                },
                Tag::Uop => Term::Uop {
                    op: UnaryOp::try_from(valtag).unwrap_unchecked(),
                    val: TermPtr::forge(ext),
                },
                Tag::And => Term::And {
                    lhs: TermPtr::forge(ext),
                    rhs: TermPtr::forge(valext),
                },
                Tag::Or => Term::Or {
                    lhs: TermPtr::forge(ext),
                    rhs: TermPtr::forge(valext),
                },
                Tag::Wld => Term::Wld,
                Tag::Err => Term::Err {
                    immediate: ext.to_u64() != 0,
                    backtrace: (valext.to_u64() != 0).then(|| TracePtr::forge(valext)),
                },
                Tag::Pri => Term::Pri(PrimId(valext)),
                Tag::Int => Term::Int(val as i64),
                Tag::Float => Term::Float(OrderedFloat(f64::from_bits(val))),
                Tag::Char => Term::Char(char::from_u32(val as u32).unwrap_unchecked()),
                Tag::Bool => Term::Bool(val != 0),
                Tag::Box => Term::Box(ValuePtr::forge(valext)),
                Tag::Type => Term::Type(TypePtr::forge(valext)),
                Tag::Swi | Tag::Invalid => unreachable!(),
            }
        }
    }
}

impl<'h> Term<'h> {
    #[rustfmt::skip]
    pub fn pack(&self) -> Node {
        match self {
            Term::Null => Node::NULL,
            Term::Var => Node::from_tag(Tag::Var),
            Term::Lam { body } => Node::from_tag_ext_valext(Tag::Lam, body.binder_addr(), body.body_addr()),
            Term::App { func, arg } => Node::from_tag_ext_valext(Tag::App, func.addr(), arg.addr()),
            Term::Ref { ptr } => Node::from_tag_ext_valext(Tag::Ref, ptr.label().to_u56(), ptr.addr()),
            Term::Sup { ptr } => Node::from_tag_valext(Tag::Sup, ptr.addr()),
            Term::Use { body } => Node::from_tag_valext(Tag::Use, body.addr()),
            Term::Ctn { ty, arity, values } => Node::from_all(Tag::Ctn, ty.addr(), *arity, values.addr()),
            Term::Partial { func, arity, args } => Node::from_all(Tag::Partial, func.addr(), *arity, args.addr()),
            Term::Ctr { ty, variant } => {
                // Encode the optional variant: `valtag` flags presence, `valext` the id.
                let (flag, ve) = match variant {
                    Some(v) => (1, v.addr()),
                    None => (0, U56::new(0)),
                };
                Node::from_all(Tag::Ctr, ty.addr(), flag, ve)
            }
            Term::VarId(id) => Node::from_tag_valext(Tag::VarId, id.addr()),
            Term::Mat { matches } => Node::from_tag_ext_valext(Tag::Mat, matches.addr(), U56::new(0)),
            Term::Bop { op, lhs, rhs } => Node::from_all(Tag::Bop, lhs.addr(), *op as u8, rhs.addr()),
            Term::Uop { op, val } => Node::from_all(Tag::Uop, val.addr(), *op as u8, U56::new(0)),
            Term::And { lhs, rhs } => Node::from_tag_ext_valext(Tag::And, lhs.addr(), rhs.addr()),
            Term::Or { lhs, rhs } => Node::from_tag_ext_valext(Tag::Or, lhs.addr(), rhs.addr()),
            Term::Wld => Node::from_tag(Tag::Wld),
            Term::Err { immediate, backtrace } => Node::from_tag_ext_valext(Tag::Err, (*immediate as u32).into(), backtrace.as_ref().map_or(U56::new(0), |t| t.addr())),
            Term::Pri(id)  => Node::from_tag_valext(Tag::Pri, id.0),
            Term::Int(val) => Node::from_tag_val(Tag::Int, *val as u64),
            Term::Float(val) => Node::from_tag_val(Tag::Float, val.into_inner().to_bits()),
            Term::Bool(val) =>  Node::from_tag_val(Tag::Bool, *val as u64),
            Term::Char(val) => Node::from_tag_val(Tag::Char, *val as u64),
            Term::Box(val) => Node::from_tag_valext(Tag::Box, val.addr()),
            Term::Type(ty) => Node::from_tag_valext(Tag::Type, ty.addr()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip<'h>(term: &Term<'h>) -> Term<'h> {
        let node = term.pack();
        unsafe { node.unpack() }
    }

    fn assert_round_trip<'h>(term: Term<'h>) {
        assert_eq!(round_trip(&term), term);
    }

    fn addr(n: u64) -> U56 {
        U56::new(n)
    }

    fn term_ptr<'h>(n: u64) -> TermPtr<'h> {
        unsafe { TermPtr::forge(addr(n)) }
    }

    #[test]
    fn round_trip_atoms() {
        assert_round_trip(Term::Null);
        assert_round_trip(Term::Var);
        assert_round_trip(Term::Wld);
    }

    #[test]
    fn round_trip_lam_app_use() {
        assert_round_trip(Term::Lam {
            body: unsafe { BodyPtr::forge(addr(1), addr(2)) },
        });
        assert_round_trip(Term::App {
            func: term_ptr(10),
            arg: term_ptr(20),
        });
        assert_round_trip(Term::Use { body: term_ptr(30) });
    }

    #[test]
    fn round_trip_dup_sup() {
        assert_round_trip(Term::Ref {
            ptr: unsafe { RefPtr::forge(addr(100), LabelId(addr(5))) },
        });
        assert_round_trip(Term::Ref {
            ptr: unsafe { RefPtr::forge(addr(101), LabelId(addr(6))) },
        });
        assert_round_trip(Term::Sup {
            ptr: unsafe { SupPtr::forge(addr(102)) },
        });
    }

    #[test]
    fn round_trip_ctr_mat() {
        assert_round_trip(Term::Ctn {
            ty: unsafe { TypePtr::forge(addr(3)) },
            arity: 4,
            values: unsafe { PackPtr::forge(addr(40)) },
        });
        assert_round_trip(Term::Mat {
            matches: unsafe { MatchPtr::forge(addr(8)) },
        });
        assert_round_trip(Term::Type(unsafe { TypePtr::forge(addr(9)) }));
    }

    #[test]
    fn round_trip_type_nodes() {
        assert_round_trip(Term::Partial {
            func: term_ptr(50),
            arity: 3,
            args: unsafe { PackPtr::forge(addr(51)) },
        });
        assert_round_trip(Term::Ctr {
            ty: term_ptr(52),
            variant: Some(VariantId::from_u56(addr(53))),
        });
        assert_round_trip(Term::Ctr {
            ty: term_ptr(55),
            variant: None,
        });
        assert_round_trip(Term::VarId(VariantId::from_u56(addr(54))));
    }

    #[test]
    fn round_trip_uop_typeof() {
        assert_round_trip(Term::Uop {
            op: UnaryOp::TypeOf,
            val: term_ptr(23),
        });
    }

    #[test]
    fn round_trip_bop_and_or() {
        assert_round_trip(Term::Bop {
            op: BinaryOp::Add,
            lhs: term_ptr(11),
            rhs: term_ptr(12),
        });
        assert_round_trip(Term::And {
            lhs: term_ptr(13),
            rhs: term_ptr(14),
        });
        assert_round_trip(Term::Or {
            lhs: term_ptr(15),
            rhs: term_ptr(16),
        });
    }

    #[test]
    fn round_trip_err() {
        assert_round_trip(Term::Err {
            immediate: false,
            backtrace: Some(unsafe { TracePtr::forge(addr(200)) }),
        });
        assert_round_trip(Term::Err {
            immediate: true,
            backtrace: Some(unsafe { TracePtr::forge(addr(201)) }),
        });
        assert_round_trip(Term::Err {
            immediate: true,
            backtrace: None,
        });
        assert_round_trip(Term::Err {
            immediate: false,
            backtrace: None,
        });
    }

    #[test]
    fn round_trip_uop() {
        assert_round_trip(Term::Uop {
            op: UnaryOp::Not,
            val: term_ptr(21),
        });
        assert_round_trip(Term::Uop {
            op: UnaryOp::Neg,
            val: term_ptr(22),
        });
    }

    #[test]
    fn round_trip_primitives() {
        assert_round_trip(Term::Int(0));
        assert_round_trip(Term::Int(i64::MAX));
        assert_round_trip(Term::Int(42));
        assert_round_trip(Term::Float(OrderedFloat(0.0)));
        assert_round_trip(Term::Char('a'));
        assert_round_trip(Term::Char('🦀'));
        assert_round_trip(Term::Bool(false));
        assert_round_trip(Term::Bool(true));
    }

    #[test]
    fn round_trip_negative_int() {
        assert_round_trip(Term::Int(-1));
        assert_round_trip(Term::Int(-42));
        assert_round_trip(Term::Int(i64::MIN));
        assert_round_trip(Term::Int(-0x7FFF_FFFF_FFFF_FFFF));
    }

    #[test]
    fn round_trip_float() {
        assert_round_trip(Term::Float(OrderedFloat(3.14)));
        assert_round_trip(Term::Float(OrderedFloat(-1.5)));
        assert_round_trip(Term::Float(OrderedFloat(f64::MIN)));
        assert_round_trip(Term::Float(OrderedFloat(f64::MAX)));
        assert_round_trip(Term::Float(OrderedFloat(-0.0)));
        assert_round_trip(Term::Float(OrderedFloat(f64::INFINITY)));
        assert_round_trip(Term::Float(OrderedFloat(f64::NEG_INFINITY)));
        assert_round_trip(Term::Float(OrderedFloat(f64::NAN)));
    }

    #[test]
    fn round_trip_float_precise() {
        assert_round_trip(Term::Float(OrderedFloat(3.141592653589793)));
        assert_round_trip(Term::Float(OrderedFloat(-2.718281828459045)));
    }

    #[test]
    fn round_trip_box_pri() {
        assert_round_trip(Term::Box(unsafe { ValuePtr::forge(addr(99)) }));
        assert_round_trip(Term::Pri(PrimId(addr(77))));
    }

    #[test]
    fn pack_produces_expected_raw_node() {
        let term = Term::App {
            func: term_ptr(1),
            arg: term_ptr(2),
        };
        let node = term.pack();
        assert_eq!(node, Node::from_tag_ext_valext(Tag::App, addr(1), addr(2)));
        assert_eq!(round_trip(&term), term);
    }
}
