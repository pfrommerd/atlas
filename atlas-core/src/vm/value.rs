use std::{marker::PhantomData, sync::Arc};

use super::term::Brand;
use crate::util::{ShardedSlab, U56};
use std::sync::atomic::AtomicUsize;

/// A boxed builtin value, owned by the [`ValuePool`] and named by a [`ValueId`] in
/// the packed term. `Str`/`Bytes` are `Arc`-backed so duplication is a refcount
/// bump and erasure drops a reference.
#[derive(Debug, Clone)]
pub enum Value {
    Str(Arc<str>),
    Bytes(Arc<[u8]>),
}

pub struct ValueCell {
    count: AtomicUsize,
    value: Value,
}

type ValueAddr = U56;

/// An affine handle into the [`ValuePool`] — one per live boxed term. `Copy` like
/// the other address handles; the executor upholds single-ownership (each id is
/// created once and erased once).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ValueId<'h>(pub(crate) ValueAddr, Brand<'h>);

impl<'h> ValueId<'h> {
    /// SAFETY: `key` must name a live pool entry of heap 'h.
    pub(crate) unsafe fn new_unchecked(key: U56) -> Self {
        ValueId(key, PhantomData)
    }
    /// The raw pool key.
    pub fn key(self) -> U56 {
        self.0
    }
}

/// The reference-counted pool of [`Value`]s. See the [module docs](self).
pub struct ValuePool {
    slab: ShardedSlab<ValueAddr, ValueCell>,
}

impl ValuePool {
    pub fn new() -> Self {
        ValuePool {
            slab: ShardedSlab::default(),
        }
    }
}

impl Default for ValuePool {
    fn default() -> Self {
        Self::new()
    }
}
