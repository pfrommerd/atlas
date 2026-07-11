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
pub type PrimReduce<'a, 'h> = Pin<Box<dyn Future<Output = Result<Handle<'h>, String>> + 'a>>;

/// Translates and runs host-provided primitive functions (`%name`). `apply`
/// receives the still-unforced argument [`Handle`]s together with the
/// [`Executor`]; it forces (to WHNF) the inputs it needs itself (e.g. via
/// `exec.whnf_at`), and returns the result handle or an evaluation error.
/// Arguments it does not consume
/// may simply be dropped — the executor reclaims them via
/// [`erase_dropped_handles`](Executor::erase_dropped_handles).
pub trait Extensions: Sized {
    fn resolve(&self, name: &str) -> Option<PrimId>;
    fn arity(&self, id: PrimId) -> usize;
    fn name(&self, id: PrimId) -> Option<Cow<'_, str>>;
    fn apply<'a, 'e, 'h, P: ExecPolicy, X: Extensions>(
        &'a self,
        exec: &'a Executor<'e, 'h, P, X>,
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
    fn apply<'a, 'e, 'h, P: ExecPolicy, X: Extensions>(
        &'a self,
        _: &'a Executor<'e, 'h, P, X>,
        _: PrimId,
        _: Vec<Handle<'h>>,
    ) -> PrimReduce<'a, 'h> {
        unreachable!("NoExtensions resolves no primitives")
    }
}

/// Two extension sets presented as one. Names resolve left-to-right, so the
/// left extension wins when both provide the same primitive name.
pub struct CombinedExtensions<L, R> {
    pub left: L,
    pub right: R,
}

impl<L, R> CombinedExtensions<L, R> {
    pub fn new(left: L, right: R) -> Self {
        CombinedExtensions { left, right }
    }
}

const MAX_CHILD_ID: u64 = (1 << 55) - 1;

impl<L: Extensions, R: Extensions> Extensions for CombinedExtensions<L, R> {
    fn resolve(&self, name: &str) -> Option<PrimId> {
        self.left
            .resolve(name)
            .map(|id| {
                assert!(
                    id.get() <= MAX_CHILD_ID,
                    "combined primitive ID is too large"
                );
                PrimId::new(id.get() << 1)
            })
            .or_else(|| {
                self.right.resolve(name).map(|id| {
                    assert!(
                        id.get() <= MAX_CHILD_ID,
                        "combined primitive ID is too large"
                    );
                    PrimId::new((id.get() << 1) | 1)
                })
            })
    }

    fn arity(&self, id: PrimId) -> usize {
        if id.get() & 1 == 0 {
            self.left.arity(PrimId::new(id.get() >> 1))
        } else {
            self.right.arity(PrimId::new(id.get() >> 1))
        }
    }

    fn name(&self, id: PrimId) -> Option<Cow<'_, str>> {
        if id.get() & 1 == 0 {
            self.left.name(PrimId::new(id.get() >> 1))
        } else {
            self.right.name(PrimId::new(id.get() >> 1))
        }
    }

    fn apply<'a, 'e, 'h, P: ExecPolicy, X: Extensions>(
        &'a self,
        exec: &'a Executor<'e, 'h, P, X>,
        id: PrimId,
        args: Vec<Handle<'h>>,
    ) -> PrimReduce<'a, 'h> {
        if id.get() & 1 == 0 {
            self.left.apply(exec, PrimId::new(id.get() >> 1), args)
        } else {
            self.right.apply(exec, PrimId::new(id.get() >> 1), args)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::term::Term;

    struct Left;
    struct Right;

    impl Extensions for Left {
        fn resolve(&self, name: &str) -> Option<PrimId> {
            match name {
                "left" | "same" => Some(PrimId::new(0)),
                _ => None,
            }
        }
        fn arity(&self, _: PrimId) -> usize {
            0
        }
        fn name(&self, _: PrimId) -> Option<Cow<'_, str>> {
            Some(Cow::Borrowed("left"))
        }
        fn apply<'a, 'e, 'h, P: ExecPolicy, X: Extensions>(
            &'a self,
            exec: &'a Executor<'e, 'h, P, X>,
            _: PrimId,
            _: Vec<Handle<'h>>,
        ) -> PrimReduce<'a, 'h> {
            Box::pin(async move { Ok(Handle::new(exec.heap.alloc(Term::Int(1)), exec.heap)) })
        }
    }

    impl Extensions for Right {
        fn resolve(&self, name: &str) -> Option<PrimId> {
            match name {
                "right" | "same" => Some(PrimId::new(0)),
                _ => None,
            }
        }
        fn arity(&self, _: PrimId) -> usize {
            0
        }
        fn name(&self, _: PrimId) -> Option<Cow<'_, str>> {
            Some(Cow::Borrowed("right"))
        }
        fn apply<'a, 'e, 'h, P: ExecPolicy, X: Extensions>(
            &'a self,
            exec: &'a Executor<'e, 'h, P, X>,
            _: PrimId,
            _: Vec<Handle<'h>>,
        ) -> PrimReduce<'a, 'h> {
            Box::pin(async move { Ok(Handle::new(exec.heap.alloc(Term::Int(2)), exec.heap)) })
        }
    }

    #[test]
    fn combined_extensions_dispatch_and_prefer_left_names() {
        let extensions = CombinedExtensions::new(Left, Right);
        assert_eq!(crate::vm::run_with("%left", &extensions).unwrap(), "1");
        assert_eq!(crate::vm::run_with("%right", &extensions).unwrap(), "2");
        assert_eq!(crate::vm::run_with("%same", &extensions).unwrap(), "1");
        assert_eq!(
            extensions
                .name(extensions.resolve("right").unwrap())
                .as_deref(),
            Some("right")
        );
        let nested = CombinedExtensions::new(CombinedExtensions::new(Left, Right), Left);
        assert_eq!(crate::vm::run_with("%right", &nested).unwrap(), "2");
    }
}
