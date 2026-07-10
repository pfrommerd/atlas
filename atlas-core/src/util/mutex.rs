use std::cell::UnsafeCell;
use std::future::Future;
use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::task::{Context, Poll, Waker};

/// Identifies one sequential reduction chain for deadlock detection.
///
/// A root key is minted at each reduction entry point (`whnf_at` /
/// `normalize_at`); concurrency forks (e.g. reducing the two operands of a
/// binary op with `join!`) give each branch a *child* key. Sequential nesting
/// within a chain reuses the same key. A lock attempt finding the current
/// holder anywhere in its own lineage is a self-deadlock: the holder is an
/// ancestor frame of this very chain and can only release after this attempt
/// returns — so [`AsyncMutex::lock`] fails it with [`RecursiveLock`] instead
/// of waiting forever. Sibling branches (neither an ancestor of the other)
/// wait normally.
#[derive(Clone, Debug)]
pub struct LockKey(Arc<KeyNode>);

#[derive(Debug)]
struct KeyNode {
    id: u64,
    parent: Option<LockKey>,
}

static KEY_COUNTER: AtomicU64 = AtomicU64::new(1);

impl LockKey {
    /// A fresh key with no ancestors (one reduction entry point).
    pub fn root() -> Self {
        LockKey(Arc::new(KeyNode {
            id: KEY_COUNTER.fetch_add(1, Ordering::Relaxed),
            parent: None,
        }))
    }

    /// A child key for one branch of a concurrency fork.
    pub fn fork(&self) -> Self {
        LockKey(Arc::new(KeyNode {
            id: KEY_COUNTER.fetch_add(1, Ordering::Relaxed),
            parent: Some(self.clone()),
        }))
    }

    /// Whether `id` names this key or any of its ancestors.
    fn lineage_contains(&self, id: u64) -> bool {
        let mut node = Some(self);
        while let Some(k) = node {
            if k.0.id == id {
                return true;
            }
            node = k.0.parent.as_ref();
        }
        false
    }
}

/// A keyed lock attempt found the lock held by an ancestor of its own
/// reduction chain: waiting would deadlock (the holder resumes only after
/// this attempt returns). Callers treat the target as cyclic/stuck.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecursiveLock;

/// A general async mutex for an arbitrary number of concurrent accessors.
///
/// Unlike [`super::SingleMutex`] (which stores a single waker and is limited to
/// two contenders), this keeps a queue of waiting tasks. It backs `DupCell`,
/// where a duplication can have arbitrarily many projection branches
/// contending for the same cell.
///
/// Locking is *keyed* (see [`LockKey`]): the state records the holder's key,
/// and an attempt whose lineage contains the holder's id fails with
/// [`RecursiveLock`] instead of self-deadlocking. [`try_lock`] is keyless and
/// records no holder; its guards must never be held across an `.await`.
///
/// The queue uses a simple wake-all policy: releasing the lock wakes every
/// queued waiter, and the woken tasks re-contend (one wins, the rest re-queue).
/// Wakers are de-duplicated by [`Waker::will_wake`] so a task polled repeatedly
/// while pending does not grow the queue. Dup arities are small, so the linear
/// scan and spurious wakeups are cheap.
///
/// [`try_lock`]: AsyncMutex::try_lock
pub struct AsyncMutex<T> {
    state: StdMutex<State>,
    data: UnsafeCell<T>,
}

struct State {
    locked: bool,
    /// The holder's key (None for keyless `try_lock` guards).
    holder: Option<LockKey>,
    waiters: Vec<Waker>,
}

/// Future returned by [`AsyncMutex::lock`].
pub struct AsyncMutexLock<'a, T> {
    mutex: &'a AsyncMutex<T>,
    key: LockKey,
}

/// Guard providing exclusive access to the data protected by an [`AsyncMutex`].
#[must_use = "if unused the AsyncMutex will immediately unlock"]
pub struct AsyncMutexGuard<'a, T> {
    mutex: &'a AsyncMutex<T>,
}

impl<T> AsyncMutex<T> {
    pub fn new(data: T) -> Self {
        Self {
            state: StdMutex::new(State {
                locked: false,
                holder: None,
                waiters: Vec::new(),
            }),
            data: UnsafeCell::new(data),
        }
    }

    /// Acquire the lock under `key`, failing with [`RecursiveLock`] if the
    /// current holder is `key` itself or one of its ancestors.
    pub fn lock(&self, key: &LockKey) -> AsyncMutexLock<'_, T> {
        AsyncMutexLock {
            mutex: self,
            key: key.clone(),
        }
    }

    /// Keyless, non-blocking acquire. The guard records no holder key, so it
    /// must never be held across an `.await` (a keyed waiter could not detect
    /// a cycle through it).
    pub fn try_lock(&self) -> Option<AsyncMutexGuard<'_, T>> {
        let mut state = self.state.lock().unwrap();
        if state.locked {
            None
        } else {
            state.locked = true;
            state.holder = None;
            Some(AsyncMutexGuard { mutex: self })
        }
    }

    fn unlock(&self) {
        let wakers = {
            let mut state = self.state.lock().unwrap();
            state.locked = false;
            state.holder = None;
            std::mem::take(&mut state.waiters)
        };
        for waker in wakers {
            waker.wake();
        }
    }
}

impl<'a, T> Future for AsyncMutexLock<'a, T> {
    type Output = Result<AsyncMutexGuard<'a, T>, RecursiveLock>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mutex = self.mutex;
        match mutex.poll_acquire(&self.key, cx) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(AsyncMutexGuard { mutex })),
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<T> AsyncMutex<T> {
    /// Shared poll logic for the borrowed and owned lock futures: acquire (and
    /// record the holder key), fail on a lineage cycle, or park the waker.
    fn poll_acquire(&self, key: &LockKey, cx: &mut Context<'_>) -> Poll<Result<(), RecursiveLock>> {
        let mut state = self.state.lock().unwrap();
        if !state.locked {
            state.locked = true;
            state.holder = Some(key.clone());
            return Poll::Ready(Ok(()));
        }
        if let Some(holder) = &state.holder {
            if key.lineage_contains(holder.0.id) {
                return Poll::Ready(Err(RecursiveLock));
            }
        }
        if !state.waiters.iter().any(|w| w.will_wake(cx.waker())) {
            state.waiters.push(cx.waker().clone());
        }
        Poll::Pending
    }

    /// As [`lock`](AsyncMutex::lock), but for an `Arc`'d mutex: the returned
    /// guard *owns* a clone of the `Arc`, so it is not tied to a borrow and
    /// keeps the mutex alive for its own duration (used for dup fan locks,
    /// whose mutex is shared between cells and may be repointed under it).
    pub fn lock_arc(self: &Arc<Self>, key: &LockKey) -> AsyncMutexLockArc<T> {
        AsyncMutexLockArc {
            mutex: Arc::clone(self),
            key: key.clone(),
        }
    }

    /// Keyless, non-blocking owned acquire (see [`try_lock`](AsyncMutex::try_lock)).
    pub fn try_lock_arc(self: &Arc<Self>) -> Option<OwnedAsyncMutexGuard<T>> {
        let mut state = self.state.lock().unwrap();
        if state.locked {
            None
        } else {
            state.locked = true;
            state.holder = None;
            Some(OwnedAsyncMutexGuard {
                mutex: Arc::clone(self),
            })
        }
    }
}

/// Future returned by [`AsyncMutex::lock_arc`].
pub struct AsyncMutexLockArc<T> {
    mutex: Arc<AsyncMutex<T>>,
    key: LockKey,
}

impl<T> Future for AsyncMutexLockArc<T> {
    type Output = Result<OwnedAsyncMutexGuard<T>, RecursiveLock>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.mutex.poll_acquire(&self.key, cx) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(OwnedAsyncMutexGuard {
                mutex: Arc::clone(&self.mutex),
            })),
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Guard that owns a clone of its `Arc`'d [`AsyncMutex`] (see
/// [`AsyncMutex::lock_arc`]).
#[must_use = "if unused the AsyncMutex will immediately unlock"]
pub struct OwnedAsyncMutexGuard<T> {
    mutex: Arc<AsyncMutex<T>>,
}

impl<T> OwnedAsyncMutexGuard<T> {
    /// The mutex this guard holds (e.g. for `Arc::ptr_eq` identity checks).
    pub fn mutex(&self) -> &Arc<AsyncMutex<T>> {
        &self.mutex
    }
}

impl<T> Deref for OwnedAsyncMutexGuard<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY: holding the guard means we hold the logical lock, so no other
        // accessor can reach the data.
        unsafe { &*self.mutex.data.get() }
    }
}

impl<T> DerefMut for OwnedAsyncMutexGuard<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: see `deref`; the guard is held exclusively.
        unsafe { &mut *self.mutex.data.get() }
    }
}

impl<T> Drop for OwnedAsyncMutexGuard<T> {
    fn drop(&mut self) {
        self.mutex.unlock();
    }
}

impl<T> Deref for AsyncMutexGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY: holding the guard means we hold the logical lock, so no other
        // accessor can reach the data.
        unsafe { &*self.mutex.data.get() }
    }
}

impl<T> DerefMut for AsyncMutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: see `deref`; the guard is held exclusively.
        unsafe { &mut *self.mutex.data.get() }
    }
}

impl<T> Drop for AsyncMutexGuard<'_, T> {
    fn drop(&mut self) {
        self.mutex.unlock();
    }
}

// SAFETY: the data is only ever accessed by the (single) guard holder; the
// waiter queue is protected by the inner std mutex.
unsafe impl<T: Send> Send for AsyncMutex<T> {}
unsafe impl<T: Send> Sync for AsyncMutex<T> {}
unsafe impl<T: Send> Send for AsyncMutexGuard<'_, T> {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use tokio::sync::Barrier;

    #[tokio::test]
    async fn try_lock_succeeds_when_free() {
        let mutex = AsyncMutex::new(7u32);
        let guard = mutex.try_lock().unwrap();
        assert_eq!(*guard, 7);
        assert!(mutex.try_lock().is_none());
    }

    #[tokio::test]
    async fn many_tasks_mutual_exclusion() {
        const TASKS: usize = 8;
        let mutex = Arc::new(AsyncMutex::new(()));
        let in_critical = Arc::new(AtomicU32::new(0));
        let violations = Arc::new(AtomicU32::new(0));
        let barrier = Arc::new(Barrier::new(TASKS + 1));

        for _ in 0..TASKS {
            let mutex = Arc::clone(&mutex);
            let in_critical = Arc::clone(&in_critical);
            let violations = Arc::clone(&violations);
            let barrier = Arc::clone(&barrier);
            tokio::spawn(async move {
                let key = LockKey::root();
                for _ in 0..200 {
                    let _guard = mutex.lock(&key).await.unwrap();
                    if in_critical.fetch_add(1, Ordering::SeqCst) != 0 {
                        violations.fetch_add(1, Ordering::SeqCst);
                    }
                    tokio::task::yield_now().await;
                    in_critical.fetch_sub(1, Ordering::SeqCst);
                }
                barrier.wait().await;
            });
        }

        barrier.wait().await;
        assert_eq!(violations.load(Ordering::SeqCst), 0);
        assert_eq!(in_critical.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn many_tasks_increment() {
        const TASKS: usize = 8;
        let mutex = Arc::new(AsyncMutex::new(0u32));
        let barrier = Arc::new(Barrier::new(TASKS + 1));

        for _ in 0..TASKS {
            let mutex = Arc::clone(&mutex);
            let barrier = Arc::clone(&barrier);
            tokio::spawn(async move {
                let key = LockKey::root();
                for _ in 0..250 {
                    let mut guard = mutex.lock(&key).await.unwrap();
                    *guard += 1;
                    drop(guard);
                    tokio::task::yield_now().await;
                }
                barrier.wait().await;
            });
        }

        barrier.wait().await;
        assert_eq!(
            *mutex.lock(&LockKey::root()).await.unwrap(),
            (TASKS as u32) * 250
        );
    }

    #[tokio::test]
    async fn same_key_relock_errors() {
        let mutex = AsyncMutex::new(());
        let key = LockKey::root();
        let _guard = mutex.lock(&key).await.unwrap();
        // The same chain re-locking its own held mutex is a self-deadlock.
        assert!(matches!(mutex.lock(&key).await, Err(RecursiveLock)));
    }

    #[tokio::test]
    async fn descendant_of_holder_errors() {
        let mutex = AsyncMutex::new(());
        let key = LockKey::root();
        let _guard = mutex.lock(&key).await.unwrap();
        // A fork branch blocking on its suspended ancestor's lock would also
        // deadlock: the ancestor resumes only after the branch completes.
        let child = key.fork();
        assert!(matches!(mutex.lock(&child).await, Err(RecursiveLock)));
        assert!(matches!(
            mutex.lock(&child.fork()).await,
            Err(RecursiveLock)
        ));
    }

    #[tokio::test]
    async fn sibling_keys_wait_not_error() {
        let mutex = Arc::new(AsyncMutex::new(0u32));
        let root = LockKey::root();
        let (k1, k2) = (root.fork(), root.fork());
        // Sibling branches legitimately contend: branch 2 must wait for
        // branch 1, not treat it as a cycle.
        let guard = mutex.lock(&k1).await.unwrap();
        let contender = {
            let mutex = Arc::clone(&mutex);
            tokio::spawn(async move {
                *mutex.lock(&k2).await.unwrap() += 1;
            })
        };
        tokio::task::yield_now().await;
        drop(guard);
        contender.await.unwrap();
        assert_eq!(*mutex.lock(&LockKey::root()).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn unrelated_root_waits_not_error() {
        let mutex = Arc::new(AsyncMutex::new(()));
        let guard = mutex.lock(&LockKey::root()).await.unwrap();
        let other = {
            let mutex = Arc::clone(&mutex);
            tokio::spawn(async move {
                let guard = mutex.lock(&LockKey::root()).await.unwrap();
                drop(guard);
            })
        };
        tokio::task::yield_now().await;
        drop(guard);
        other.await.unwrap();
    }
}
