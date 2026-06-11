use ordered_float::OrderedFloat;
use std::marker::PhantomData;

pub use crate::util::U56;
pub use crate::vm::value::{Value, ValueId};

/// An invariant lifetime brand: invariant in `'h` (neither co- nor
/// contravariant), and `Send + Sync`.
pub type Brand<'h> = PhantomData<fn(&'h ()) -> &'h ()>;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LabelId(U56);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PrimId(U56);

// --- ptr types ---
pub type Addr = U56;

#[derive(Debug, PartialEq, Eq, Hash)]
pub struct TermPtr<'h>(Addr, Brand<'h>);

#[derive(Debug, PartialEq, Eq, Hash)]
pub struct NamePtr<'h>(Addr, Brand<'h>);
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct MatchPtr<'h>(Addr, Brand<'h>);
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct ValuesPtr<'h>(Addr, Brand<'h>);
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct BodyPtr<'h> {
    binder: Addr,
    body: Addr,
    brand: Brand<'h>,
}

// one side of a duplication
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DupPtr<'h>(Addr, bool, Brand<'h>);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SupPtr<'h>(Addr, Brand<'h>);

// pointer to a trace type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TracePtr<'h>(Addr, Brand<'h>);

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
    /// points to the left or right side of a duplication
    Dup { label: LabelId, ptr: DupPtr<'h>},
    /// superposition node -- contains the label and pointers to left/right
    /// side arguments arising from duplicating a function.
    Sup { label: LabelId, ptr: SupPtr<'h>},
    /// constructor `#Name{ fields.. }`;
    /// if 'bound' is None, this an "empty construct" used
    /// scrutinee-side to represent a construct of a given name and arity.
    Ctr { name: NamePtr<'h>, arity: u8, values: ValuesPtr<'h> },
    /// pattern match against constructors or primitive values
    Mat { matches: MatchPtr<'h>, branches: ValuesPtr<'h> },
    /// binary operation node `[OpMeta, lhs, rhs]`
    Bop { op: BinaryOp, lhs: TermPtr<'h>, rhs: TermPtr<'h> },
    /// short-circuit `and` / `or` node `[lhs, rhs]`
    And { lhs: TermPtr<'h>, rhs: TermPtr<'h> },
    Or { lhs: TermPtr<'h>, rhs: TermPtr<'h> },
    /// wildcard (`*` / `_`): an inert atom. Could be anything!
    Wld,
    /// err: a first-class eraser; it annihilates whatever it interacts with.
    Err { immediate: bool, backtrace: TracePtr<'h> },
    // basic types, all stored in the "val" portion of the packed node
    U64(u64), I64(i64),
    F32(OrderedFloat<f32>), F64(OrderedFloat<f64>),
    Char(char), Bool(bool),
    // a boxed value, identified by its 'ValueId'
    Box(ValueId<'h>),
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
    Dp0, Dp1, Sup, Ctr,
    Mat, Swi, Use, Bop,
    // Special short-circuit operators
    And, Or,
    Wld, Err,
    /// a host-provided primitive function
    Pri,
    /// primitive types
    U64, I64, F32, F64,
    Char, Bool, Box,
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
    fn val(&self) -> U56 {
        U56::new((self.0 & VAL_MASK) as u64)
    }
    fn val_tag(&self) -> u8 {
        ((self.0 & VALTAG_MASK) >> 56) as u8
    }
    fn val_ext(&self) -> U56 {
        U56::new((self.0 & VALEXT_MASK) as u64)
    }

    // SAFETY: In order to use this method, the caller must ensure
    // that the underlying Node is valid for heap scope 'h.
    pub(crate) unsafe fn unpack<'h>(self) -> Term<'h> {
        let ext = self.ext();
        let val = self.val();
        let valtag = self.val_tag();
        let valext = self.val_ext();
        // SAFETY: The caller guarantees us that
        unsafe {
            // TODO: Implement
            todo!()
        }
    }
}

impl<'h> Term<'h> {
    #[rustfmt::skip]
    pub fn pack(&self) -> Node {
        match self {
            Term::Null => Node::NULL,
            Term::Var => Node::from_tag(Tag::Var),
            Term::Lam { body } => Node::from_tag_ext_valext(Tag::Lam, body.binder, body.body),
            Term::App { func, arg } => Node::from_tag_ext_valext(Tag::App, func.0, arg.0),
            Term::Dup { label, ptr } => Node::from_tag_ext_valext(if ptr.1 { Tag::Dp0 } else { Tag::Dp1 }, label.0, ptr.0),
            Term::Sup { label, ptr } => Node::from_tag_ext_valext(Tag::Sup, label.0, ptr.0),
            Term::Use { body } => Node::from_tag_valext(Tag::Use, body.0),
            Term::Ctr { name, arity, values } => Node::from_all(Tag::Ctr, name.0, *arity, values.0),
            Term::Mat { matches, branches } => Node::from_tag_ext_valext(Tag::Mat, matches.0, branches.0),
            Term::Bop { op, lhs, rhs } => Node::from_all(Tag::Bop, lhs.0, *op as u8, rhs.0),
            Term::And { lhs, rhs } => Node::from_tag_ext_valext(Tag::And, lhs.0, rhs.0),
            Term::Or { lhs, rhs } => Node::from_tag_ext_valext(Tag::Or, lhs.0, rhs.0),
            Term::Wld => Node::from_tag(Tag::Wld),
            Term::Err { immediate, backtrace } => Node::from_tag_ext_valext(Tag::Err, (*immediate as u32).into(), backtrace.0),
            Term::Pri(id)  => Node::from_tag_valext(Tag::Pri, id.0),
            Term::U64(val) => Node::from_tag_val(Tag::U64, *val),
            Term::I64(val) => Node::from_tag_val(Tag::I64, *val as u64),
            Term::F64(val) => Node::from_tag_val(Tag::F64, val.into_inner() as u64),
            Term::F32(val) => Node::from_tag_val(Tag::F32, val.into_inner() as u64),
            Term::Bool(val) =>  Node::from_tag_val(Tag::Bool, *val as u64),
            Term::Char(val) => Node::from_tag_val(Tag::Char, *val as u64),
            Term::Box(val) => Node::from_tag_valext(Tag::Box, val.0),
        }
    }
}
