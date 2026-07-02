//! The desugared core expression IR.
//!
//! [`crate::core::ast::Node`] (the surface AST) is lowered into [`Expr`] before
//! being compiled to heap terms (see [`crate::core::ast::desugar`] and
//! [`crate::vm::heap::Heap::lower`]). `Expr` is deliberately minimal and owns
//! all of its data (it does not borrow from the source text). It follows
//! `docs/core/core.md` closely:
//!
//! - variables are **de Bruijn indices** ([`DeBruijn`]): each `Lam` and each
//!   `Dup` introduces one binder level; `Var` selects a `Lam` binder, `Ref` a
//!   `Dup`. A `Dup` has an arbitrary number of projections: every `Ref`
//!   occurrence to its binder is a distinct projection wire,
//! - cloned binders (`\&x`) are made **explicit** as a single `Dup` (the binder
//!   is referenced once per use), and cloned lets are fresh re-instantiations,
//! - list / string / char / cons sugar is fully desugared into constructors.

use ordered_float::OrderedFloat;

use crate::vm::term::{BinaryOp, UnaryOp};

/// A builtin scalar / boxed value, mirroring the primitive leaves of
/// [`vm::term::Term`](crate::vm::term::Term). Numbers, floats, chars and bools
/// lower to scalar term leaves; strings and byte arrays lower to boxed heap
/// values. `Expr` carries these directly (see [`Expr::Value`]) rather than
/// desugaring strings/chars into constructor lists.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Value {
    Int(i64),
    Float(OrderedFloat<f64>),
    Char(char),
    Bool(bool),
    Str(String),
    Bytes(Vec<u8>),
}

/// A de Bruijn index (or, for quoted static terms, a level). Counts binders,
/// where each `Lam` and each `Dup` contributes one level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeBruijn(pub u64);

/// A compiled match-arm key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pat {
    Ctr(String),
    Val(Value),
}

/// The body of a `type { .. }` declaration: either a product (an ordered list of
/// field type-expressions) or a sum (named variants, each with argument
/// type-expressions). Components are evaluated to type values at runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeDefKind {
    Product(Vec<Expr>),
    Sum(Vec<(String, Vec<Expr>)>),
}

/// A desugared core expression. See the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    /// de Bruijn variable bound by a `Lam`.
    Var(DeBruijn),
    /// a projection of the `Dup` at the given de Bruijn index. Each `Ref`
    /// occurrence is a distinct projection wire of that dup; the projection
    /// count is the number of occurrences and is fixed at lowering time.
    Ref(DeBruijn),
    /// erasure (`&{}`)
    Era,
    /// wildcard (`_`)
    Wld,
    /// a builtin scalar or boxed value (number, float, char, bool, string, bytes)
    Value(Value),
    /// A free name resolved at lowering time (e.g. the prelude or a REPL local).
    Free(String),
    /// `%name` primitive
    Pri(String),
    /// superposition `&{a, b}`. Each part is a distinct wire; the wire labels are
    /// minted at lowering time.
    Sup {
        left: Box<Expr>,
        right: Box<Expr>,
    },
    /// duplication `! & = val; body` (binds `Ref`s in `body`). Each `Ref` to this
    /// binder in `body` is a distinct projection wire.
    Dup {
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
    /// a constructor selector `ty :: Name`, evaluating to a constructor of `ty`.
    /// `variant` is `None` for the product constructor (`::New`) and
    /// `Some(name)` for a sum variant.
    Ctr {
        ty: Box<Expr>,
        variant: Option<String>,
    },
    /// a type declaration `type { .. }`, evaluating to a fresh type value.
    TypeDef {
        kind: TypeDefKind,
    },
    /// pattern match / numeric switch / use
    Mat {
        cases: Vec<(Pat, Expr)>,
        default: Option<Box<Expr>>,
    },
    /// binary operation
    Bop {
        op: BinaryOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    /// unary operation
    Uop {
        op: UnaryOp,
        val: Box<Expr>,
    },
}
