//! [`Term`]: the inspection enum an extension sees when it opens a [`Handle`].
//!
//! It mirrors the leaves of the engine's [`vm::term::Term`](crate::vm::term::Term)
//! but replaces affine child pointers with owning [`Handle`]s. Only the variants
//! primitives currently need are decomposed (leaves, `App`, `Ctr`); anything else
//! is handed back whole via [`Term::Other`] as an escape hatch.

use ordered_float::OrderedFloat;

use super::handle::Handle;
use crate::vm::heap::{HeapScope, NamePtr};
use crate::vm::term::Term as VmTerm;

/// An opened heap node whose children are [`Handle`]s. See the module docs.
#[rustfmt::skip]
pub enum Term<'h> {
    /// application node `[func, arg]`
    App { func: Handle<'h>, arg: Handle<'h> },
    /// constructor `#Name{ fields.. }`
    Ctr { name: NamePtr<'h>, arity: u8, fields: Vec<Handle<'h>> },
    // basic value leaves
    Int(i64), Float(OrderedFloat<f64>),
    Char(char), Bool(bool),
    /// an unsubstituted variable
    Var,
    /// wildcard (`*` / `_`)
    Wld,
    /// any variant not yet decomposed by this API; still owns its children, so an
    /// extension must consume it (e.g. via `exec.erase`) to avoid leaking.
    Other(VmTerm<'h>),
}

impl<'h> Term<'h> {
    /// Build an [`extension::Term`](Term) from a freshly pulled engine term,
    /// wrapping each affine child pointer as a [`Handle`] borrowing `heap`.
    pub(crate) fn from_raw(raw: VmTerm<'h>, heap: &'h HeapScope<'h>) -> Term<'h> {
        match raw {
            VmTerm::App { func, arg } => Term::App {
                func: Handle::new(func, heap),
                arg: Handle::new(arg, heap),
            },
            VmTerm::Ctr {
                name,
                arity,
                values,
            } => {
                let fields = heap
                    .into_fields(values)
                    .into_iter()
                    .take(arity as usize)
                    .map(|p| Handle::new(p, heap))
                    .collect();
                Term::Ctr {
                    name,
                    arity,
                    fields,
                }
            }
            VmTerm::Int(n) => Term::Int(n),
            VmTerm::Float(x) => Term::Float(x),
            VmTerm::Char(c) => Term::Char(c),
            VmTerm::Bool(b) => Term::Bool(b),
            VmTerm::Var => Term::Var,
            VmTerm::Wld => Term::Wld,
            other => Term::Other(other),
        }
    }
}
