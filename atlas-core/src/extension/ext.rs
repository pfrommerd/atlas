//! The host-extension trait: translating and running `%name` primitives.

use std::borrow::Cow;
use std::future::Future;
use std::pin::Pin;

use super::handle::Handle;
use crate::vm::exec::{ExecPolicy, Executor};
use crate::vm::term::PrimId;

/// A boxed primitive-application future. The primitive forces the argument
/// handles it needs (through the [`Executor`]) and yields the result handle, which
/// re-enters reduction. It borrows the executor/extension for `'a`; the result
/// node lives in scope `'h`.
pub type PrimReduce<'a, 'h> = Pin<Box<dyn Future<Output = Handle<'h>> + 'a>>;

/// Translates and runs host-provided primitive functions (`%name`). `apply`
/// receives the still-unforced argument [`Handle`]s together with the
/// [`Executor`]; it forces (to WHNF) the inputs it needs itself (e.g. via
/// `exec.whnf_at`), and returns the result handle. Arguments it does not consume
/// may simply be dropped — the executor reclaims them via
/// [`erase_dropped_handles`](Executor::erase_dropped_handles).
pub trait Extensions: Sized {
    fn resolve(&self, name: &str) -> Option<PrimId>;
    fn arity(&self, id: PrimId) -> usize;
    fn name(&self, id: PrimId) -> Option<Cow<'_, str>>;
    fn apply<'a, 'e, 'h, P: ExecPolicy>(
        &'a self,
        exec: &'a Executor<'e, 'h, P, Self>,
        id: PrimId,
        args: Vec<Handle<'h>>,
    ) -> PrimReduce<'a, 'h>;
}

/// The empty extension set: no primitives.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoExtensions;

impl Extensions for NoExtensions {
    #[inline]
    fn resolve(&self, _: &str) -> Option<PrimId> {
        None
    }
    fn arity(&self, _: PrimId) -> usize {
        unreachable!("NoExtensions resolves no primitives")
    }
    fn name(&self, _: PrimId) -> Option<Cow<'_, str>> {
        None
    }
    fn apply<'a, 'e, 'h, P: ExecPolicy>(
        &'a self,
        _: &'a Executor<'e, 'h, P, Self>,
        _: PrimId,
        _: Vec<Handle<'h>>,
    ) -> PrimReduce<'a, 'h> {
        unreachable!("NoExtensions resolves no primitives")
    }
}
