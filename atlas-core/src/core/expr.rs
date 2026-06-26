//! The desugared core expression IR.
//!
//! [`crate::core::ast::Node`] (the surface AST) is lowered into [`Expr`] before
//! being compiled to heap terms (see [`crate::core::ast::desugar`] and
//! [`crate::vm::heap::Heap::lower`]). `Expr` is deliberately minimal and owns
//! all of its data (it does not borrow from the source text). It follows
//! `docs/core/core.md` closely:
//!
//! - variables are **de Bruijn indices** ([`DeBruijn`]): each `Lam` and each
//!   `Dup` introduces one binder level; `Var` selects a `Lam` binder,
//!   `Dp0`/`Dp1` a `Dup`,
//! - cloned binders (`\&x`) are made **explicit** as `Dup` chains, and cloned
//!   lets are fresh re-instantiations,
//! - list / string / char / cons sugar is fully desugared into constructors.

use ordered_float::OrderedFloat;

use crate::vm::term::BinaryOp;

/// A builtin scalar / boxed value, mirroring the primitive leaves of
/// [`vm::term::Term`](crate::vm::term::Term). Numbers, floats, chars and bools
/// lower to scalar term leaves; strings and byte arrays lower to boxed heap
/// values. `Expr` carries these directly (see [`Expr::Value`]) rather than
/// desugaring strings/chars into constructor lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    U64(u64),
    I64(i64),
    F32(OrderedFloat<f32>),
    F64(OrderedFloat<f64>),
    Char(char),
    Bool(bool),
    Str(String),
    Bytes(Vec<u8>),
}

/// A de Bruijn index (or, for quoted static terms, a level). Counts binders,
/// where each `Lam` and each `Dup` contributes one level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeBruijn(pub u64);

/// A duplication / superposition label. Source labels are preserved by name
/// (for testing purposes, so equal labels annihilate). `Auto` marks a label for
/// an auto-dup; it carries no id because the concrete, globally-unique label is
/// generated at lowering time (per dup cell) — `Expr` dups already identify their
/// binder by de Bruijn index, so the AST needs no distinguishing id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Label {
    Named(String),
    Auto,
}

/// A compiled match-arm key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pat {
    Ctr(String),
    Num(u64),
}

/// A desugared core expression. See the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    /// de Bruijn variable bound by a `Lam`.
    Var(DeBruijn),
    /// first / second projection of the `Dup` at the given de Bruijn index.
    Dp0(DeBruijn),
    Dp1(DeBruijn),
    /// erasure (`&{}`)
    Era,
    /// wildcard (`_`)
    Wld,
    /// a builtin scalar or boxed value (number, float, char, bool, string, bytes)
    Value(Value),
    /// `@name` reference
    Ref(String),
    /// `%name` primitive
    Pri(String),
    /// superposition `&L{a, b}`
    Sup {
        label: Label,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    /// duplication `! &L = val; body` (binds `Dp0`/`Dp1` in `body`)
    Dup {
        label: Label,
        val: Box<Expr>,
        body: Box<Expr>,
    },
    /// lambda `\ -> body` (its binder is always used at least once)
    Lam {
        body: Box<Expr>,
    },
    /// erasing lambda `\_ -> body`: ignores (erases) its argument and returns
    /// `body`. Introduced by desugaring a `Lam` whose binder is never used.
    /// Still occupies one de Bruijn binder level (so outer indices line up), but
    /// nothing in `body` refers to it.
    Use {
        body: Box<Expr>,
    },
    /// application `(func arg)`
    App {
        func: Box<Expr>,
        arg: Box<Expr>,
    },
    /// constructor `#Name{args..}`
    Ctr {
        name: String,
        args: Vec<Expr>,
    },
    /// pattern match / numeric switch / use
    Mat {
        cases: Vec<(Pat, Expr)>,
        default: Option<Box<Expr>>,
    },
    /// binary operation
    Op2 {
        op: BinaryOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
}
