use std::alloc::{Layout, alloc, dealloc};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::mem::{ManuallyDrop, MaybeUninit};
use std::ops::{Deref, DerefMut};
use std::ptr;
use std::sync::Mutex;
use std::thread;

use super::U56;

const DEFAULT_SHARD_COUNT: usize = 8;
const DEFAULT_SEGMENT_CAPACITY: usize = 1024;
const SHARD_BITS: u32 = 8;
const MAX_SHARDS: usize = 1 << SHARD_BITS;

/// An invariant slab brand: invariant in `'sh` (neither co- nor contravariant).
pub type Brand<'sh> = PhantomData<fn(&'sh ()) -> &'sh ()>;

/// Configuration for a [`ShardedSlab`].
#[derive(Debug, Clone, Copy)]
pub struct ShardedSlabConfig {
    pub shard_count: usize,
    pub segment_capacity: usize,
}

impl Default for ShardedSlabConfig {
    fn default() -> Self {
        Self {
            shard_count: DEFAULT_SHARD_COUNT,
            segment_capacity: DEFAULT_SEGMENT_CAPACITY,
        }
    }
}

impl ShardedSlabConfig {
    fn validate(self) -> Self {
        assert!(self.shard_count > 0, "shard_count must be > 0");
        assert!(
            self.shard_count <= MAX_SHARDS,
            "shard_count must be <= {MAX_SHARDS}"
        );
        assert!(self.segment_capacity > 0, "segment_capacity must be > 0");
        self
    }
}

/// A slab key that encodes `[shard_idx: SHARD_BITS][local: KEY_BITS - SHARD_BITS]`.
pub trait Key: Copy + Eq + Send + Sync + 'static {
    const KEY_BITS: u32;
    fn from_indices(shard: u8, local: usize) -> Self;
    fn to_indices(self) -> (u8, usize);
}

fn pack_indices<const KEY_BITS: u32>(shard: u8, local: usize) -> usize {
    let local_bits = KEY_BITS - SHARD_BITS;
    let local_mask = (1usize << local_bits) - 1;
    debug_assert!(local <= local_mask);
    (usize::from(shard) << local_bits) | local
}

fn unpack_indices<const KEY_BITS: u32>(key: usize) -> (u8, usize) {
    let local_bits = KEY_BITS - SHARD_BITS;
    let local_mask = (1usize << local_bits) - 1;
    ((key >> local_bits) as u8, key & local_mask)
}

impl Key for usize {
    const KEY_BITS: u32 = usize::BITS as u32;
    fn from_indices(shard: u8, local: usize) -> Self {
        pack_indices::<{ Self::KEY_BITS }>(shard, local)
    }
    fn to_indices(self) -> (u8, usize) {
        unpack_indices::<{ Self::KEY_BITS }>(self)
    }
}

impl Key for u32 {
    const KEY_BITS: u32 = u32::BITS;
    fn from_indices(shard: u8, local: usize) -> Self {
        pack_indices::<{ Self::KEY_BITS }>(shard, local) as u32
    }
    fn to_indices(self) -> (u8, usize) {
        unpack_indices::<{ Self::KEY_BITS }>(self as usize)
    }
}

impl Key for u64 {
    const KEY_BITS: u32 = u64::BITS;
    fn from_indices(shard: u8, local: usize) -> Self {
        pack_indices::<{ Self::KEY_BITS }>(shard, local) as u64
    }
    fn to_indices(self) -> (u8, usize) {
        unpack_indices::<{ Self::KEY_BITS }>(self as usize)
    }
}

impl Key for U56 {
    const KEY_BITS: u32 = U56::BITS;
    fn from_indices(shard: u8, local: usize) -> Self {
        let packed = pack_indices::<{ Self::KEY_BITS }>(shard, local);
        // SAFETY: packed fits in 56 bits.
        unsafe { U56::new_unchecked((packed as u64) & U56::MASK) }
    }
    fn to_indices(self) -> (u8, usize) {
        unpack_indices::<{ Self::KEY_BITS }>((self.to_u64() & U56::MASK) as usize)
    }
}

/// A copyable, branded key for shared immutable slab access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SharedKey<'sh, K>(K, Brand<'sh>);

impl<'sh, K: Key> SharedKey<'sh, K> {
    pub fn raw(self) -> K {
        self.0
    }

    /// SAFETY: The caller must guarantee this is the only live key naming the slot; otherwise
    /// [`SlabScope::remove`] may invalidate other handles.
    pub unsafe fn assert_unique_unchecked(self) -> UniqueKey<'sh, K> {
        UniqueKey(self.0, PhantomData)
    }
}

/// An owned, non-duplicable key handle for exclusive slab access and removal.
pub struct UniqueKey<'sh, K>(K, Brand<'sh>);

impl<'sh, K: Key> UniqueKey<'sh, K> {
    pub fn raw(self) -> K {
        self.0
    }
}

impl<'sh, K: std::fmt::Debug> std::fmt::Debug for UniqueKey<'sh, K> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("UniqueKey").field(&self.0).finish()
    }
}

/// Exclusive mutable access to a slab value. Call [`finished`](Self::finished) to return the key.
pub struct UniqueSlot<'sh, K, V> {
    key: K,
    value: *mut V,
    _brand: Brand<'sh>,
}

impl<'sh, K, V> Deref for UniqueSlot<'sh, K, V> {
    type Target = V;

    fn deref(&self) -> &V {
        // SAFETY: `get_unique` hands out the only `&mut` while the key is consumed.
        unsafe { &*self.value }
    }
}

impl<'sh, K, V> DerefMut for UniqueSlot<'sh, K, V> {
    fn deref_mut(&mut self) -> &mut V {
        // SAFETY: `get_unique` hands out the only `&mut` while the key is consumed.
        unsafe { &mut *self.value }
    }
}

impl<'sh, K: Key, V> UniqueSlot<'sh, K, V> {
    pub fn finished(self) -> UniqueKey<'sh, K> {
        let key = self.key;
        std::mem::forget(self);
        UniqueKey(key, PhantomData)
    }
}

impl<'sh, K, V> Drop for UniqueSlot<'sh, K, V> {
    fn drop(&mut self) {
        panic!("UniqueSlot dropped without calling finished()");
    }
}

struct Segment<V> {
    slots: ManuallyDrop<Box<[MaybeUninit<V>]>>,
}

impl<V> Segment<V> {
    fn new(capacity: usize) -> Self {
        let layout = Layout::array::<MaybeUninit<V>>(capacity).unwrap();
        // SAFETY: capacity matches the allocated layout; slots are written before read.
        let ptr = unsafe { alloc(layout) as *mut MaybeUninit<V> };
        assert!(!ptr.is_null(), "segment allocation failed");
        Self {
            slots: ManuallyDrop::new(unsafe {
                Box::from_raw(ptr::slice_from_raw_parts_mut(ptr, capacity))
            }),
        }
    }

    fn slots(&self) -> &[MaybeUninit<V>] {
        &self.slots
    }

    fn slots_mut(&mut self) -> &mut [MaybeUninit<V>] {
        &mut self.slots
    }
}

impl<V> Drop for Segment<V> {
    fn drop(&mut self) {
        let slots = unsafe { ManuallyDrop::take(&mut self.slots) };
        let len = slots.len();
        let layout = Layout::array::<MaybeUninit<V>>(len).unwrap();
        let ptr = Box::into_raw(slots);
        unsafe {
            dealloc(ptr as *mut u8, layout);
        }
    }
}

struct Shard<V> {
    segments: Vec<Segment<V>>,
    free: Vec<usize>,
    next: usize,
}

impl<V> Shard<V> {
    fn new() -> Self {
        Self {
            segments: Vec::new(),
            free: Vec::new(),
            next: 0,
        }
    }

    fn segment_index(local: usize, segment_capacity: usize) -> usize {
        local / segment_capacity
    }

    fn slot_index(local: usize, segment_capacity: usize) -> usize {
        local % segment_capacity
    }

    fn ensure_segment(&mut self, local: usize, segment_capacity: usize) {
        let seg_idx = Self::segment_index(local, segment_capacity);
        while self.segments.len() <= seg_idx {
            self.segments.push(Segment::new(segment_capacity));
        }
    }

    fn insert(&mut self, value: V, segment_capacity: usize) -> usize {
        let (local, reusing) = if let Some(local) = self.free.pop() {
            (local, true)
        } else {
            let local = self.next;
            self.next += 1;
            (local, false)
        };
        self.ensure_segment(local, segment_capacity);
        let seg_idx = Self::segment_index(local, segment_capacity);
        let slot = Self::slot_index(local, segment_capacity);
        let dest = &mut self.segments[seg_idx].slots_mut()[slot];
        if reusing {
            unsafe {
                dest.assume_init_drop();
            }
        }
        dest.write(value);
        local
    }

    fn get_ptr(&self, local: usize, segment_capacity: usize) -> *const V {
        let seg_idx = Self::segment_index(local, segment_capacity);
        let slot = Self::slot_index(local, segment_capacity);
        unsafe { self.segments[seg_idx].slots()[slot].assume_init_ref() as *const V }
    }

    fn remove(&mut self, local: usize, segment_capacity: usize) -> V {
        let seg_idx = Self::segment_index(local, segment_capacity);
        let slot = Self::slot_index(local, segment_capacity);
        let value = unsafe { self.segments[seg_idx].slots_mut()[slot].assume_init_read() };
        self.free.push(local);
        value
    }
}

pub struct ShardedSlab<K, V> {
    shards: Vec<Mutex<Shard<V>>>,
    config: ShardedSlabConfig,
    _marker: PhantomData<fn(K)>,
}

/// A branded view of a [`ShardedSlab`]. Keys and references produced through a
/// scope are tied to `'sh` and cannot be used with a different slab.
#[repr(transparent)]
pub struct SlabScope<'sh, K, V> {
    slab: ShardedSlab<K, V>,
    _brand: Brand<'sh>,
}

impl<K: Key, V> ShardedSlab<K, V> {
    pub fn with_config(config: ShardedSlabConfig) -> Self {
        let config = config.validate();
        Self {
            shards: (0..config.shard_count)
                .map(|_| Mutex::new(Shard::new()))
                .collect(),
            config,
            _marker: PhantomData,
        }
    }
}

impl<K: Key, V> Default for ShardedSlab<K, V> {
    fn default() -> Self {
        Self::with_config(ShardedSlabConfig::default())
    }
}

impl<K, V> Drop for ShardedSlab<K, V> {
    fn drop(&mut self) {
        let cap = self.config.segment_capacity;
        for shard in &mut self.shards {
            let Ok(shard) = shard.get_mut() else {
                continue;
            };
            for index in 0..shard.next {
                if shard.free.contains(&index) {
                    continue;
                }
                let seg_idx = Shard::<V>::segment_index(index, cap);
                let slot = Shard::<V>::slot_index(index, cap);
                if seg_idx < shard.segments.len() {
                    unsafe {
                        shard.segments[seg_idx].slots_mut()[slot].assume_init_drop();
                    }
                }
            }
        }
    }
}

impl<K: Key, V> ShardedSlab<K, V> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Safely brand this slab for the duration of `f`.
    pub fn with<R>(&self, f: impl for<'sh> FnOnce(&SlabScope<'sh, K, V>) -> R) -> R {
        f(unsafe { self.forge_brand() })
    }

    /// Forge a slab brand. The caller must ensure keys and references from this
    /// scope are not used with a different slab.
    pub unsafe fn forge_brand<'sh>(&self) -> &SlabScope<'sh, K, V> {
        unsafe { &*(self as *const Self as *const SlabScope<'sh, K, V>) }
    }

    fn with_unlocked_shard<R>(&self, f: impl FnOnce(&mut Shard<V>, u8) -> R) -> R {
        let start = {
            let mut hasher = DefaultHasher::new();
            thread::current().id().hash(&mut hasher);
            (hasher.finish() as usize) % self.shards.len()
        };
        let len = self.shards.len();
        for offset in 0..len {
            let shard_idx = (start + offset) % len;
            if let Ok(mut guard) = self.shards[shard_idx].try_lock() {
                return f(&mut guard, shard_idx as u8);
            }
        }
        let mut guard = self.shards[start].lock().unwrap();
        f(&mut guard, start as u8)
    }

    fn insert_value(&self, value: V) -> (u8, usize) {
        self.with_unlocked_shard(|shard, shard_idx| {
            let local = shard.insert(value, self.config.segment_capacity);
            (shard_idx, local)
        })
    }

    fn value_ptr(&self, shard: u8, local: usize) -> *const V {
        let shard = self.shards[usize::from(shard)].lock().unwrap();
        shard.get_ptr(local, self.config.segment_capacity)
    }
}

impl<'sh, K: Key, V> SlabScope<'sh, K, V> {
    pub fn insert(&self, value: V) -> SharedKey<'sh, K> {
        let (shard, local) = self.slab.insert_value(value);
        SharedKey(K::from_indices(shard, local), PhantomData)
    }

    pub fn insert_unique(&self, value: V) -> UniqueKey<'sh, K> {
        let (shard, local) = self.slab.insert_value(value);
        UniqueKey(K::from_indices(shard, local), PhantomData)
    }

    pub fn get(&self, key: &SharedKey<'sh, K>) -> &'sh V {
        let (shard, local) = key.0.to_indices();
        let ptr = self.slab.value_ptr(shard, local);
        // SAFETY: branding ties the key to this slab; the slot was initialized at insert.
        unsafe { &*ptr }
    }

    pub fn get_unique(&self, key: UniqueKey<'sh, K>) -> UniqueSlot<'sh, K, V> {
        let (shard, local) = key.0.to_indices();
        let value = self.slab.value_ptr(shard, local) as *mut V;
        UniqueSlot {
            key: key.0,
            value,
            _brand: PhantomData,
        }
    }

    pub fn remove(&self, key: UniqueKey<'sh, K>) -> V {
        let (shard, local) = key.0.to_indices();
        let mut shard = self.slab.shards[usize::from(shard)].lock().unwrap();
        shard.remove(local, self.slab.config.segment_capacity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_get_round_trip() {
        let slab: ShardedSlab<usize, String> = ShardedSlab::new();
        slab.with(|scope| {
            let key = scope.insert("hello".to_string());
            assert_eq!(scope.get(&key), &"hello".to_string());
        });
    }

    #[test]
    fn unique_slot_mutation_and_remove() {
        let slab: ShardedSlab<u32, i32> = ShardedSlab::new();
        slab.with(|scope| {
            let key = scope.insert_unique(10);
            let mut slot = scope.get_unique(key);
            *slot = 20;
            assert_eq!(*slot, 20);
            let key = slot.finished();
            assert_eq!(scope.remove(key), 20);
        });
    }

    #[test]
    fn shared_remove_via_assert_unique() {
        let slab: ShardedSlab<usize, String> = ShardedSlab::new();
        slab.with(|scope| {
            let key = scope.insert("gone".to_string());
            let unique = unsafe { key.assert_unique_unchecked() };
            assert_eq!(scope.remove(unique), "gone".to_string());
        });
    }

    #[test]
    fn stable_reference_across_growth() {
        let slab: ShardedSlab<usize, usize> = ShardedSlab::new();
        slab.with(|scope| {
            let key = scope.insert(1);
            let reference = scope.get(&key) as *const usize;
            for i in 2..=DEFAULT_SEGMENT_CAPACITY + 10 {
                scope.insert(i);
            }
            assert_eq!(scope.get(&key), &1);
            assert_eq!(scope.get(&key) as *const usize, reference);
        });
    }

    #[test]
    fn key_indices_round_trip() {
        let shard = 3u8;
        let local = 1024usize;
        assert_eq!(
            usize::to_indices(usize::from_indices(shard, local)),
            (shard, local)
        );
        assert_eq!(
            u32::to_indices(u32::from_indices(shard, local)),
            (shard, local)
        );
        assert_eq!(
            U56::to_indices(U56::from_indices(shard, local)),
            (shard, local)
        );
    }

    #[test]
    fn segment_index_from_local_key() {
        let cap = 16;
        assert_eq!(Shard::<i32>::segment_index(0, cap), 0);
        assert_eq!(Shard::<i32>::segment_index(15, cap), 0);
        assert_eq!(Shard::<i32>::segment_index(16, cap), 1);
        assert_eq!(Shard::<i32>::segment_index(31, cap), 1);
    }
}
