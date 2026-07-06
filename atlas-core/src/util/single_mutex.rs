use std::cell::UnsafeCell;
use std::future::Future;
use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll, Waker};

/// An async mutex for at most two concurrent accessors.
///
/// Unlike [`tokio::sync::Mutex`], which queues an unbounded list of waiting
/// tasks, this stores only a single [`Waker`]. That matches `DupCell` usage,
/// where only the two duplication branches may contend.
pub struct SingleMutex<T> {
    locked: AtomicBool,
    has_waiter: AtomicBool,
    waiter: UnsafeCell<Option<Waker>>,
    data: UnsafeCell<T>,
}

/// Future returned by [`SingleMutex::lock`].
pub struct Lock<'a, T> {
    mutex: &'a SingleMutex<T>,
    queued: bool,
}

/// Guard providing exclusive access to the data protected by a [`SingleMutex`].
#[must_use = "if unused the SingleMutex will immediately unlock"]
pub struct SingleMutexGuard<'a, T> {
    mutex: &'a SingleMutex<T>,
}

impl<T> SingleMutex<T> {
    pub fn new(data: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            has_waiter: AtomicBool::new(false),
            waiter: UnsafeCell::new(None),
            data: UnsafeCell::new(data),
        }
    }

    pub fn lock(&self) -> Lock<'_, T> {
        Lock {
            mutex: self,
            queued: false,
        }
    }

    pub fn try_lock(&self) -> Option<SingleMutexGuard<'_, T>> {
        if self.try_acquire() {
            Some(SingleMutexGuard { mutex: self })
        } else {
            None
        }
    }

    pub fn has_waiter(&self) -> bool {
        self.has_waiter.load(Ordering::Acquire)
    }

    fn try_acquire(&self) -> bool {
        !self.locked.swap(true, Ordering::AcqRel)
    }

    unsafe fn unlock(&self) {
        if self.has_waiter.swap(false, Ordering::AcqRel) {
            let waker = unsafe { (*self.waiter.get()).take() };
            if let Some(waker) = waker {
                waker.wake();
            }
        } else {
            self.locked.store(false, Ordering::Release);
        }
    }
}

impl<'a, T> Future for Lock<'a, T> {
    type Output = SingleMutexGuard<'a, T>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.queued {
            self.queued = false;
            return Poll::Ready(SingleMutexGuard { mutex: self.mutex });
        }

        loop {
            if self.mutex.try_acquire() {
                return Poll::Ready(SingleMutexGuard { mutex: self.mutex });
            }

            if self
                .mutex
                .has_waiter
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                debug_assert!(
                    false,
                    "SingleMutex: more than two tasks contended for the same lock"
                );
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }

            let slot = unsafe { &mut *self.mutex.waiter.get() };
            match slot {
                Some(waker) if waker.will_wake(cx.waker()) => {}
                _ => *slot = Some(cx.waker().clone()),
            }

            if !self.mutex.has_waiter.load(Ordering::Acquire)
                && self.mutex.locked.load(Ordering::Acquire)
            {
                *slot = None;
                return Poll::Ready(SingleMutexGuard { mutex: self.mutex });
            }

            if !self.mutex.locked.load(Ordering::Acquire) {
                self.mutex.has_waiter.store(false, Ordering::Release);
                *slot = None;
                continue;
            }

            self.queued = true;
            return Poll::Pending;
        }
    }
}

impl<T> Deref for SingleMutexGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.mutex.data.get() }
    }
}

impl<T> DerefMut for SingleMutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.mutex.data.get() }
    }
}

impl<T> Drop for SingleMutexGuard<'_, T> {
    fn drop(&mut self) {
        unsafe { self.mutex.unlock() };
    }
}

unsafe impl<T: Send> Send for SingleMutex<T> {}
unsafe impl<T: Send> Sync for SingleMutex<T> {}
unsafe impl<T: Send> Send for SingleMutexGuard<'_, T> {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use tokio::sync::Barrier;

    #[tokio::test]
    async fn two_tasks_alternate() {
        let mutex = Arc::new(SingleMutex::new(0u32));
        let mutex2 = Arc::clone(&mutex);

        let first = tokio::spawn(async move {
            let mut guard = mutex.lock().await;
            *guard += 1;
        });

        let mut guard = mutex2.lock().await;
        *guard += 1;
        drop(guard);

        first.await.unwrap();
        assert_eq!(*mutex2.lock().await, 2);
    }

    #[tokio::test]
    async fn try_lock_succeeds_when_free() {
        let mutex = SingleMutex::new(7u32);
        let guard = mutex.try_lock().unwrap();
        assert_eq!(*guard, 7);
    }

    #[tokio::test]
    async fn interleaved_mutual_exclusion() {
        let mutex = Arc::new(SingleMutex::new(()));
        let in_critical = Arc::new(AtomicU32::new(0));
        let violations = Arc::new(AtomicU32::new(0));
        let barrier = Arc::new(Barrier::new(3));

        for _ in 0..2 {
            let mutex = Arc::clone(&mutex);
            let in_critical = Arc::clone(&in_critical);
            let violations = Arc::clone(&violations);
            let barrier = Arc::clone(&barrier);

            tokio::spawn(async move {
                for _ in 0..500 {
                    let _guard = mutex.lock().await;
                    let prev = in_critical.fetch_add(1, Ordering::SeqCst);
                    if prev != 0 {
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
    async fn interleaved_ping_pong_increments() {
        let mutex = Arc::new(SingleMutex::new(0u32));
        let barrier = Arc::new(Barrier::new(3));

        for _ in 0..2 {
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
        assert_eq!(*mutex.lock().await, 500);
    }

    #[tokio::test]
    async fn waiter_acquires_after_holder_drops() {
        let mutex = Arc::new(SingleMutex::new(0u32));
        let (holder_ready_tx, mut holder_ready_rx) = tokio::sync::mpsc::channel(1);
        let (release_holder_tx, mut release_holder_rx) = tokio::sync::mpsc::channel(1);
        let (waiter_done_tx, waiter_done_rx) = tokio::sync::oneshot::channel();

        let mutex_holder = Arc::clone(&mutex);
        let holder = tokio::spawn(async move {
            let guard = mutex_holder.lock().await;
            holder_ready_tx.send(()).await.unwrap();
            release_holder_rx.recv().await.unwrap();
            drop(guard);
        });

        holder_ready_rx.recv().await.unwrap();

        let mutex_waiter = Arc::clone(&mutex);
        let waiter = tokio::spawn(async move {
            let mut guard = mutex_waiter.lock().await;
            *guard = 99;
            waiter_done_tx.send(()).unwrap();
        });

        assert!(mutex.try_lock().is_none());

        release_holder_tx.send(()).await.unwrap();
        holder.await.unwrap();

        waiter_done_rx.await.unwrap();
        waiter.await.unwrap();
        assert_eq!(*mutex.lock().await, 99);
    }

    #[tokio::test]
    async fn interleaved_try_lock_while_held() {
        let mutex = Arc::new(SingleMutex::new(0u32));
        let (holder_ready_tx, mut holder_ready_rx) = tokio::sync::mpsc::channel(1);
        let (release_holder_tx, mut release_holder_rx) = tokio::sync::mpsc::channel(1);
        let try_lock_failures = Arc::new(AtomicU32::new(0));

        let mutex_holder = Arc::clone(&mutex);
        let holder = tokio::spawn(async move {
            let guard = mutex_holder.lock().await;
            holder_ready_tx.send(()).await.unwrap();
            release_holder_rx.recv().await.unwrap();
            drop(guard);
        });

        holder_ready_rx.recv().await.unwrap();

        let failures = Arc::clone(&try_lock_failures);
        let mutex_trier = Arc::clone(&mutex);
        let trier = tokio::spawn(async move {
            for _ in 0..100 {
                if mutex_trier.try_lock().is_none() {
                    failures.fetch_add(1, Ordering::SeqCst);
                }
                tokio::task::yield_now().await;
            }
        });

        trier.await.unwrap();
        assert!(try_lock_failures.load(Ordering::SeqCst) > 0);

        release_holder_tx.send(()).await.unwrap();
        holder.await.unwrap();
        assert!(mutex.try_lock().is_some());
    }
}
