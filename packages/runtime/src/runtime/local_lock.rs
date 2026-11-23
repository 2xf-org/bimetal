//! A dependency-free async read-write lock for targets that do not link Tokio,
//! such as non-Arrow WebAssembly modules and plain Rust contract crates.
//!
//! Readers share the value concurrently while a writer holds it alone. A queued
//! writer blocks later readers so writers do not starve, and dropping a pending
//! or newly granted future releases its claim and wakes the next waiter.

use std::cell::UnsafeCell;
use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

const UNPOISONED: &str = "no code panics while updating lock state";

/// Returned by `try_read` / `try_write` methods when the lock is currently held
/// or has a waiter ahead.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct TryLockError;

struct Waiter {
    id: u64,
    write: bool,
    waker: Waker,
}

#[derive(Default)]
struct LockState {
    readers: usize,
    writer: bool,
    next_id: u64,
    waiters: VecDeque<Waiter>,
    granted_reads: Vec<u64>,
    granted_write: Option<u64>,
}

/// Non-generic lock state machine so generic [`RwLock`] methods do not
/// duplicate queue and wake logic for every resource type.
struct RawRwLock {
    state: Mutex<LockState>,
}

impl RawRwLock {
    fn new() -> Self {
        Self {
            state: Mutex::new(LockState::default()),
        }
    }

    fn try_read(&self) -> bool {
        let mut state = self.state.lock().expect(UNPOISONED);
        if state.writer || !state.waiters.is_empty() {
            return false;
        }
        state.readers += 1;
        true
    }

    fn try_write(&self) -> bool {
        let mut state = self.state.lock().expect(UNPOISONED);
        if state.writer || state.readers > 0 || !state.waiters.is_empty() {
            return false;
        }
        state.writer = true;
        true
    }

    fn release_read(&self) {
        let mut state = self.state.lock().expect(UNPOISONED);
        state.readers -= 1;
        if state.waiters.is_empty() {
            return;
        }
        let wakers = wake_next(&mut state);
        drop(state);
        wake_all(wakers);
    }

    fn release_write(&self) {
        let mut state = self.state.lock().expect(UNPOISONED);
        state.writer = false;
        if state.waiters.is_empty() {
            return;
        }
        let wakers = wake_next(&mut state);
        drop(state);
        wake_all(wakers);
    }

    fn poll_read(&self, id: &mut Option<u64>, waker: &Waker) -> bool {
        let mut state = self.state.lock().expect(UNPOISONED);
        if let Some(current) = *id {
            if let Some(position) = state
                .granted_reads
                .iter()
                .position(|&entry| entry == current)
            {
                state.granted_reads.swap_remove(position);
                *id = None;
                return true;
            }
            if let Some(waiter) = state.waiters.iter_mut().find(|waiter| waiter.id == current) {
                waiter.waker.clone_from(waker);
            }
            return false;
        }
        if !state.writer && state.waiters.is_empty() {
            state.readers += 1;
            return true;
        }
        let assigned = state.next_id;
        state.next_id += 1;
        state.waiters.push_back(Waiter {
            id: assigned,
            write: false,
            waker: waker.clone(),
        });
        *id = Some(assigned);
        false
    }

    fn drop_read(&self, id: u64) {
        let wakers = {
            let mut state = self.state.lock().expect(UNPOISONED);
            if let Some(position) = state.granted_reads.iter().position(|&entry| entry == id) {
                state.granted_reads.swap_remove(position);
                state.readers -= 1;
                wake_next(&mut state)
            } else {
                state.waiters.retain(|waiter| waiter.id != id);
                wake_next(&mut state)
            }
        };
        wake_all(wakers);
    }

    fn poll_write(&self, id: &mut Option<u64>, waker: &Waker) -> bool {
        let mut state = self.state.lock().expect(UNPOISONED);
        if let Some(current) = *id {
            if state.granted_write == Some(current) {
                state.granted_write = None;
                *id = None;
                return true;
            }
            if let Some(waiter) = state.waiters.iter_mut().find(|waiter| waiter.id == current) {
                waiter.waker.clone_from(waker);
            }
            return false;
        }
        if !state.writer && state.readers == 0 && state.waiters.is_empty() {
            state.writer = true;
            return true;
        }
        let assigned = state.next_id;
        state.next_id += 1;
        state.waiters.push_back(Waiter {
            id: assigned,
            write: true,
            waker: waker.clone(),
        });
        *id = Some(assigned);
        false
    }

    fn drop_write(&self, id: u64) {
        let wakers = {
            let mut state = self.state.lock().expect(UNPOISONED);
            if state.granted_write == Some(id) {
                state.granted_write = None;
                state.writer = false;
                wake_next(&mut state)
            } else {
                state.waiters.retain(|waiter| waiter.id != id);
                wake_next(&mut state)
            }
        };
        wake_all(wakers);
    }
}

/// An async read-write lock backed only by the standard library.
pub struct RwLock<T> {
    raw: RawRwLock,
    value: UnsafeCell<T>,
}

impl<T> fmt::Debug for RwLock<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("RwLock").finish_non_exhaustive()
    }
}

// SAFETY: `RwLock` synchronizes all access to `value` through `raw`, giving
// only shared `&T` references to concurrent readers and `&mut T` to a sole
// writer.
unsafe impl<T: Send> Send for RwLock<T> {}
// SAFETY: as above; readers can observe `&T` from multiple threads only when
// `T: Sync`, and a writer can move `&mut T` across threads when `T: Send`.
unsafe impl<T: Send + Sync> Sync for RwLock<T> {}

impl<T> RwLock<T> {
    /// Wrap `value` in an unlocked [`RwLock`].
    #[must_use]
    pub(crate) fn new(value: T) -> Self {
        Self {
            raw: RawRwLock::new(),
            value: UnsafeCell::new(value),
        }
    }

    /// Consume the lock and return its inner value.
    #[must_use]
    pub(crate) fn into_inner(self) -> T {
        self.value.into_inner()
    }

    /// Acquire a shared read guard immediately, or fail if a writer holds or
    /// waits for the lock.
    #[cfg(any(target_arch = "wasm32", test))]
    pub(crate) fn try_read(&self) -> Result<RwLockReadGuard<'_, T>, TryLockError> {
        self.raw
            .try_read()
            .then(|| RwLockReadGuard { lock: self })
            .ok_or(TryLockError)
    }

    /// Acquire an exclusive write guard immediately, or fail if the lock is
    /// held or has waiters.
    #[cfg(any(target_arch = "wasm32", test))]
    pub(crate) fn try_write(&self) -> Result<RwLockWriteGuard<'_, T>, TryLockError> {
        self.raw
            .try_write()
            .then(|| RwLockWriteGuard { lock: self })
            .ok_or(TryLockError)
    }

    /// Acquire an owned shared read guard immediately, or fail if a writer
    /// holds or waits for the lock.
    pub(crate) fn try_read_owned(self: Arc<Self>) -> Result<OwnedRwLockReadGuard<T>, TryLockError> {
        if self.raw.try_read() {
            Ok(OwnedRwLockReadGuard { lock: self })
        } else {
            Err(TryLockError)
        }
    }

    /// Acquire an owned exclusive write guard immediately, or fail if the lock
    /// is held or has waiters.
    pub(crate) fn try_write_owned(
        self: Arc<Self>,
    ) -> Result<OwnedRwLockWriteGuard<T>, TryLockError> {
        if self.raw.try_write() {
            Ok(OwnedRwLockWriteGuard { lock: self })
        } else {
            Err(TryLockError)
        }
    }

    /// Wait until a shared read guard can be acquired.
    pub(crate) fn read_owned(self: Arc<Self>) -> ReadFuture<T> {
        ReadFuture {
            lock: Some(self),
            id: None,
        }
    }

    /// Wait until an exclusive write guard can be acquired.
    pub(crate) fn write_owned(self: Arc<Self>) -> WriteFuture<T> {
        WriteFuture {
            lock: Some(self),
            id: None,
        }
    }
}

/// Grant the lock to the next eligible waiter(s) and return their wakers.
fn wake_next(state: &mut LockState) -> Vec<Waker> {
    let mut wakers = Vec::new();
    if state.writer {
        return wakers;
    }
    if state.readers == 0 && state.waiters.front().is_some_and(|waiter| waiter.write) {
        let waiter = state.waiters.pop_front().expect("checked front");
        state.writer = true;
        state.granted_write = Some(waiter.id);
        wakers.push(waiter.waker);
        return wakers;
    }
    while state.waiters.front().is_some_and(|waiter| !waiter.write) {
        let waiter = state.waiters.pop_front().expect("checked front");
        state.readers += 1;
        state.granted_reads.push(waiter.id);
        wakers.push(waiter.waker);
    }
    wakers
}

fn wake_all(wakers: Vec<Waker>) {
    for waker in wakers {
        waker.wake();
    }
}

/// Borrowed shared guard returned by [`RwLock::try_read`].
#[cfg(any(target_arch = "wasm32", test))]
pub struct RwLockReadGuard<'lock, T> {
    lock: &'lock RwLock<T>,
}

#[cfg(any(target_arch = "wasm32", test))]
impl<T> Deref for RwLockReadGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: `self` holds one counted reader share, so no writer exists.
        unsafe { &*self.lock.value.get() }
    }
}

#[cfg(any(target_arch = "wasm32", test))]
impl<T> Drop for RwLockReadGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.raw.release_read();
    }
}

/// Borrowed exclusive guard returned by [`RwLock::try_write`].
#[cfg(any(target_arch = "wasm32", test))]
pub struct RwLockWriteGuard<'lock, T> {
    lock: &'lock RwLock<T>,
}

#[cfg(any(target_arch = "wasm32", test))]
impl<T> Deref for RwLockWriteGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: `self` holds the sole writer claim.
        unsafe { &*self.lock.value.get() }
    }
}

#[cfg(any(target_arch = "wasm32", test))]
impl<T> DerefMut for RwLockWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: `self` holds the sole writer claim.
        unsafe { &mut *self.lock.value.get() }
    }
}

#[cfg(any(target_arch = "wasm32", test))]
impl<T> Drop for RwLockWriteGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.raw.release_write();
    }
}

/// Owned shared guard returned by [`RwLock::try_read_owned`] and
/// [`RwLock::read_owned`].
pub struct OwnedRwLockReadGuard<T> {
    lock: Arc<RwLock<T>>,
}

impl<T> Deref for OwnedRwLockReadGuard<T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: `self` holds one counted reader share, so no writer exists.
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> Drop for OwnedRwLockReadGuard<T> {
    fn drop(&mut self) {
        self.lock.raw.release_read();
    }
}

/// Owned exclusive guard returned by [`RwLock::try_write_owned`] and
/// [`RwLock::write_owned`].
pub struct OwnedRwLockWriteGuard<T> {
    lock: Arc<RwLock<T>>,
}

impl<T> Deref for OwnedRwLockWriteGuard<T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: `self` holds the sole writer claim.
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> DerefMut for OwnedRwLockWriteGuard<T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: `self` holds the sole writer claim.
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T> Drop for OwnedRwLockWriteGuard<T> {
    fn drop(&mut self) {
        self.lock.raw.release_write();
    }
}

/// Future returned by [`RwLock::read_owned`].
pub(crate) struct ReadFuture<T> {
    lock: Option<Arc<RwLock<T>>>,
    id: Option<u64>,
}

impl<T> Unpin for ReadFuture<T> {}

impl<T> Future for ReadFuture<T> {
    type Output = OwnedRwLockReadGuard<T>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.as_mut().get_mut();
        let lock = this
            .lock
            .as_ref()
            .expect("ReadFuture polled after completion");
        if lock.raw.poll_read(&mut this.id, context.waker()) {
            let lock = this.lock.take().expect("checked lock");
            Poll::Ready(OwnedRwLockReadGuard { lock })
        } else {
            Poll::Pending
        }
    }
}

impl<T> Drop for ReadFuture<T> {
    fn drop(&mut self) {
        if let (Some(id), Some(lock)) = (self.id.take(), self.lock.as_ref()) {
            lock.raw.drop_read(id);
        }
    }
}

/// Future returned by [`RwLock::write_owned`].
pub(crate) struct WriteFuture<T> {
    lock: Option<Arc<RwLock<T>>>,
    id: Option<u64>,
}

impl<T> Unpin for WriteFuture<T> {}

impl<T> Future for WriteFuture<T> {
    type Output = OwnedRwLockWriteGuard<T>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.as_mut().get_mut();
        let lock = this
            .lock
            .as_ref()
            .expect("WriteFuture polled after completion");
        if lock.raw.poll_write(&mut this.id, context.waker()) {
            let lock = this.lock.take().expect("checked lock");
            Poll::Ready(OwnedRwLockWriteGuard { lock })
        } else {
            Poll::Pending
        }
    }
}

impl<T> Drop for WriteFuture<T> {
    fn drop(&mut self) {
        if let (Some(id), Some(lock)) = (self.id.take(), self.lock.as_ref()) {
            lock.raw.drop_write(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Wake;

    use super::*;

    struct CountWaker(AtomicUsize);

    impl Wake for CountWaker {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn waker() -> (Arc<CountWaker>, Waker) {
        let counter = Arc::new(CountWaker(AtomicUsize::new(0)));
        let waker = Waker::from(Arc::clone(&counter));
        (counter, waker)
    }

    #[test]
    fn readers_share_while_writer_waits_and_blocks_later_readers() {
        let lock = Arc::new(RwLock::new(1_u32));
        let first = Arc::clone(&lock).try_read_owned().expect("first reader");
        let second = lock.try_read().expect("second reader");
        assert_eq!(*first, 1);
        assert_eq!(*second, 1);
        assert!(lock.try_write().is_err());

        let (writer_wakes, writer_waker) = waker();
        let mut writer_context = Context::from_waker(&writer_waker);
        let mut writer_future = Arc::clone(&lock).write_owned();
        assert!(
            Pin::new(&mut writer_future)
                .poll(&mut writer_context)
                .is_pending()
        );

        // A queued writer blocks new readers so writers do not starve.
        assert!(lock.try_read().is_err());
        let (reader_wakes, reader_waker) = waker();
        let mut reader_context = Context::from_waker(&reader_waker);
        let mut reader_future = Arc::clone(&lock).read_owned();
        assert!(
            Pin::new(&mut reader_future)
                .poll(&mut reader_context)
                .is_pending()
        );

        drop(first);
        assert_eq!(writer_wakes.0.load(Ordering::Relaxed), 0);
        drop(second);
        assert_eq!(writer_wakes.0.load(Ordering::Relaxed), 1);

        let Poll::Ready(mut writer) = Pin::new(&mut writer_future).poll(&mut writer_context) else {
            panic!("writer should be ready once all readers dropped");
        };
        *writer = 2;
        assert_eq!(reader_wakes.0.load(Ordering::Relaxed), 0);
        drop(writer);
        assert_eq!(reader_wakes.0.load(Ordering::Relaxed), 1);

        let Poll::Ready(reader) = Pin::new(&mut reader_future).poll(&mut reader_context) else {
            panic!("reader should be ready once writer dropped");
        };
        assert_eq!(*reader, 2);
        drop(reader);
        assert_eq!(Arc::try_unwrap(lock).expect("sole owner").into_inner(), 2);
    }

    #[test]
    fn dropping_a_waiting_or_granted_future_releases_the_lock() {
        let lock = Arc::new(RwLock::new(10_u32));
        let reader = Arc::clone(&lock).try_read_owned().expect("initial reader");

        let (_w1, writer_waker) = waker();
        let mut writer_context = Context::from_waker(&writer_waker);
        let mut writer_future = Arc::clone(&lock).write_owned();
        assert!(
            Pin::new(&mut writer_future)
                .poll(&mut writer_context)
                .is_pending()
        );

        let (reader_wakes, reader_waker) = waker();
        let mut reader_context = Context::from_waker(&reader_waker);
        let mut queued_reader = Arc::clone(&lock).read_owned();
        assert!(
            Pin::new(&mut queued_reader)
                .poll(&mut reader_context)
                .is_pending()
        );

        // Dropping the pending writer unblocks the reader queued behind it.
        drop(writer_future);
        assert_eq!(reader_wakes.0.load(Ordering::Relaxed), 1);
        // Dropping the granted reader before polling releases its reader share.
        drop(queued_reader);

        let (second_writer_wakes, second_writer_waker) = waker();
        let mut second_writer_context = Context::from_waker(&second_writer_waker);
        let mut granted_writer = Arc::clone(&lock).write_owned();
        assert!(
            Pin::new(&mut granted_writer)
                .poll(&mut second_writer_context)
                .is_pending()
        );
        drop(reader);
        assert_eq!(second_writer_wakes.0.load(Ordering::Relaxed), 1);
        // Dropping a granted writer before polling releases the write lock.
        drop(granted_writer);

        let mut owned = Arc::clone(&lock).try_write_owned().expect("owned write");
        *owned = 11;
        assert!(lock.try_read().is_err());
        drop(owned);
        assert_eq!(*lock.try_read().expect("read after owned write"), 11);
    }
}
