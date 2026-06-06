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

use crate::vm::term::BinaryOp;

/// A de Bruijn index (or, for quoted static terms, a level). Counts binders,
/// where each `Lam` and each `Dup` contributes one level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeBruijn(pub u64);

/// A duplication / superposition label. Source labels are preserved by name
/// (so equal labels annihilate); `Auto` labels are generated for auto-dups and
/// are globally unique within a desugared program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Label {
    Named(String),
    Auto(u32),
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
    /// wildcard (`*`)
    Wld,
    /// number literal
    Num(u64),
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
    /// lambda `\ -> body`
    Lam {
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
