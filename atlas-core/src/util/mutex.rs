use std::cell::UnsafeCell;
use std::future::Future;
use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use std::sync::Mutex as StdMutex;
use std::task::{Context, Poll, Waker};

/// A general async mutex for an arbitrary number of concurrent accessors.
///
/// Unlike [`super::SingleMutex`] (which stores a single waker and is limited to
/// two contenders), this keeps a queue of waiting tasks. It backs `DupCell`,
/// where a duplication can now have arbitrarily many projection branches
/// contending for the same cell.
///
/// The queue uses a simple wake-all policy: releasing the lock wakes every
/// queued waiter, and the woken tasks re-contend (one wins, the rest re-queue).
/// Wakers are de-duplicated by [`Waker::will_wake`] so a task polled repeatedly
/// while pending does not grow the queue. Dup arities are small, so the linear
/// scan and spurious wakeups are cheap.
pub struct AsyncMutex<T> {
    state: StdMutex<State>,
    data: UnsafeCell<T>,
}

struct State {
    locked: bool,
    waiters: Vec<Waker>,
}

/// Future returned by [`AsyncMutex::lock`].
pub struct AsyncMutexLock<'a, T> {
    mutex: &'a AsyncMutex<T>,
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
                waiters: Vec::new(),
            }),
            data: UnsafeCell::new(data),
        }
    }

    pub fn lock(&self) -> AsyncMutexLock<'_, T> {
        AsyncMutexLock { mutex: self }
    }

    pub fn try_lock(&self) -> Option<AsyncMutexGuard<'_, T>> {
        let mut state = self.state.lock().unwrap();
        if state.locked {
            None
        } else {
            state.locked = true;
            Some(AsyncMutexGuard { mutex: self })
        }
    }

    fn unlock(&self) {
        let wakers = {
            let mut state = self.state.lock().unwrap();
            state.locked = false;
            std::mem::take(&mut state.waiters)
        };
        for waker in wakers {
            waker.wake();
        }
    }
}

impl<'a, T> Future for AsyncMutexLock<'a, T> {
    type Output = AsyncMutexGuard<'a, T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mutex = self.mutex;
        let mut state = mutex.state.lock().unwrap();
        if !state.locked {
            state.locked = true;
            return Poll::Ready(AsyncMutexGuard { mutex });
        }
        if !state.waiters.iter().any(|w| w.will_wake(cx.waker())) {
            state.waiters.push(cx.waker().clone());
        }
        Poll::Pending
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
                for _ in 0..200 {
                    let _guard = mutex.lock().await;
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
                for _ in 0..250 {
                    let mut guard = mutex.lock().await;
                    *guard += 1;
                    drop(guard);
                    tokio::task::yield_now().await;
                }
                barrier.wait().await;
            });
        }

        barrier.wait().await;
        assert_eq!(*mutex.lock().await, (TASKS as u32) * 250);
    }
}
