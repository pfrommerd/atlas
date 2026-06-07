//! Unlike `memo-map`, this version performs **no thread synchronization** — it
//! is meant for single-threaded memoization where the only thing standing in
//! the way is the borrow checker's insistence on `&mut self` for mutation. By
//! storing each value in its own [`Box`], the values get a stable heap address
//! that is independent of the backing [`HashMap`]'s storage, so a `&V` handed
//! out by [`MemoMap::get`] / [`MemoMap::get_or_insert_with`] stays valid for as
//! long as the map is borrowed — even across later inserts that grow and
//! reallocate the table.
//!
//! The map is append-only: entries are never removed or overwritten, which is
//! what makes those borrows sound. The [`UnsafeCell`] also makes `MemoMap`
//! `!Sync`, so it cannot accidentally be shared across threads.

use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::hash::Hash;

/// A `HashMap` that allows insertion behind a shared reference.
///
/// See the [module docs](self) for the rationale and safety argument.
pub struct MemoMap<K, V> {
    inner: UnsafeCell<HashMap<K, Box<V>>>,
}

impl<K, V> Default for MemoMap<K, V> {
    fn default() -> Self {
        MemoMap {
            inner: UnsafeCell::new(HashMap::new()),
        }
    }
}

impl<K: Eq + Hash, V> MemoMap<K, V> {
    /// Create an empty map.
    pub fn new() -> Self {
        Self::default()
    }

    /// A shared view of the backing map.
    ///
    /// SAFETY: We only ever expose shared references into the map (and, below,
    /// into the boxed values). The append-only contract guarantees no entry is
    /// removed or replaced for the lifetime of `&self`.
    fn map(&self) -> &HashMap<K, Box<V>> {
        unsafe { &*self.inner.get() }
    }

    /// Fetch the value for `key`, if present.
    pub fn get(&self, key: &K) -> Option<&V> {
        self.map().get(key).map(|boxed| &**boxed)
    }

    /// Whether the map contains `key`.
    pub fn contains_key(&self, key: &K) -> bool {
        self.map().contains_key(key)
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.map().len()
    }

    /// Whether the map is empty.
    pub fn is_empty(&self) -> bool {
        self.map().is_empty()
    }

    /// Return the value for `key`, inserting the result of `f` if absent.
    ///
    /// The returned reference points into the boxed value, which has a stable
    /// address, so it remains valid across subsequent insertions.
    pub fn get_or_insert_with(&self, key: K, f: impl FnOnce() -> V) -> &V {
        // SAFETY: Taking `&mut` of the backing map here is sound because the
        // only outstanding borrows derived from this map are `&V` references
        // pointing into separately-allocated `Box`es — never into the table's
        // own buckets — so mutating (and reallocating) the table cannot
        // invalidate them. We never remove or overwrite entries, so the box a
        // reference points at lives as long as the map. `MemoMap` is `!Sync`,
        // so there is no concurrent access.
        let map = unsafe { &mut *self.inner.get() };
        let boxed = map.entry(key).or_insert_with(|| Box::new(f()));
        let ptr: *const V = &**boxed;
        unsafe { &*ptr }
    }

    /// Insert `value` for `key` if absent, returning whether it was inserted.
    pub fn insert(&self, key: K, value: V) -> bool {
        // SAFETY: see `get_or_insert_with`.
        let map = unsafe { &mut *self.inner.get() };
        if map.contains_key(&key) {
            false
        } else {
            map.insert(key, Box::new(value));
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_then_get() {
        let m: MemoMap<u32, String> = MemoMap::new();
        assert!(m.insert(1, "one".into()));
        assert!(!m.insert(1, "uno".into()));
        assert_eq!(m.get(&1).map(String::as_str), Some("one"));
        assert_eq!(m.get(&2), None);
        assert!(m.contains_key(&1));
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn get_or_insert_with_memoizes() {
        let m: MemoMap<u32, String> = MemoMap::new();
        let a = m.get_or_insert_with(1, || "first".into());
        assert_eq!(a, "first");
        // a second call returns the memoized value, ignoring the closure
        let b = m.get_or_insert_with(1, || panic!("should not be called"));
        assert_eq!(b, "first");
    }

    #[test]
    fn references_survive_later_inserts() {
        let m: MemoMap<u32, String> = MemoMap::new();
        // hold a reference handed out by an early insert ...
        let first = m.get_or_insert_with(0, || "zero".into());
        // ... then force the table to grow well past its initial capacity.
        for i in 1..1000 {
            m.get_or_insert_with(i, || i.to_string());
        }
        // the original reference must still be valid.
        assert_eq!(first, "zero");
        assert_eq!(m.len(), 1000);
    }
}
