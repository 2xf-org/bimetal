//! The Tokio runtime that native hosts run async exports on, and that their
//! synchronous constructors and factories enter, so Rust code can spawn
//! tasks onto it; and the boundary that keeps a panic from unwinding into a
//! host. Other synchronous calls do not enter it, so a package without
//! resources or async calls never starts its threads.
//!
//! A process that forks keeps only the forking thread, so the child cannot use
//! the parent's runtime, whose workers did not survive; the first call in the
//! child starts a runtime of its own.

use std::any::Any;
#[cfg(all(feature = "node", feature = "arrow-api"))]
use std::cell::Cell;
use std::future::Future;
use std::panic::{self, AssertUnwindSafe};
use std::pin::Pin;
#[cfg(feature = "python")]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::task::{Context, Poll, Waker};
use std::{mem, ptr};

use tokio::runtime::{Builder, EnterGuard, Runtime};

pub use crate::runtime::pace::Pace;
use crate::runtime::pace::Paced;
#[cfg(feature = "python")]
use tokio::task::AbortHandle;
use tokio::task::JoinHandle;

/// The runtime that new calls use: null until the first call starts one,
/// and again in a forked child.
static RUNTIME: AtomicPtr<Runtime> = AtomicPtr::new(ptr::null_mut());

/// How many forks separate this process from the first one that started a
/// runtime: the fork handler counts each child.
#[cfg(feature = "python")]
static FORKS: AtomicUsize = AtomicUsize::new(0);

/// Enter the current process's runtime until the guard drops, so that a
/// synchronous constructor can spawn tasks on it.
///
/// # Panics
///
/// Panics if the operating system cannot start the runtime's threads.
#[doc(hidden)]
pub fn enter() -> EnterGuard<'static> {
    runtime().enter()
}

/// Spawn a future on the current process's runtime.
pub(crate) fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    runtime().spawn(future)
}

/// Where an async call makes its first poll. The macros choose it from the
/// call's parameters.
#[doc(hidden)]
#[derive(Clone, Copy, Debug)]
pub enum Start {
    /// On the caller's thread, so that a call that does not wait needs no
    /// worker, unless its export's [`Pace`] says that it works too long for
    /// that thread.
    Caller(&'static Pace),
    /// On a worker, for a call that takes a `Callback` or a stream that the
    /// host writes: before its first wait, it could wait for the host's event
    /// loop, which runs on the caller's thread.
    Runtime,
}

/// An async call after its first poll.
pub(crate) enum Started<F: Future> {
    /// It finished without a wait.
    Ready(F::Output),
    /// It waits, or its first poll is a worker's. A worker polls it from
    /// here on.
    Pending(Paced<F>),
}

#[cfg(all(feature = "node", feature = "arrow-api"))]
thread_local! {
    /// Whether this thread makes an async call's first poll.
    static FIRST_POLL: Cell<bool> = const { Cell::new(false) };
}

/// Whether this thread makes an async call's first poll, where a future
/// that waits returns `Pending` instead of blocking the thread.
#[cfg(all(feature = "node", feature = "arrow-api"))]
pub(crate) fn in_first_poll() -> bool {
    FIRST_POLL.get()
}

/// Restores the thread's [`FIRST_POLL`] mark when dropped, even in a panic.
#[cfg(all(feature = "node", feature = "arrow-api"))]
pub(crate) struct Mark(bool);

#[cfg(all(feature = "node", feature = "arrow-api"))]
impl Drop for Mark {
    fn drop(&mut self) {
        FIRST_POLL.set(self.0);
    }
}

/// Clear the thread's [`FIRST_POLL`] mark until the returned [`Mark`]
/// drops. A blocking read inside a first poll holds it, because a future
/// that it polls must not return `Pending` to a loop that sleeps on it.
#[cfg(all(feature = "node", feature = "arrow-api"))]
#[must_use = "dropping the guard at once restores the mark"]
pub(crate) fn leave_first_poll() -> Mark {
    Mark(FIRST_POLL.replace(false))
}

/// Poll `future` once on this thread, inside the current process's runtime,
/// unless `start` leaves the first poll to a worker, or the export's pace
/// says that the call works too long for this thread.
///
/// A call that finishes without a wait, as a read from memory does, then
/// costs no thread: only a call that waits goes to a worker, which polls it
/// again and so replaces the waker of this poll, which wakes no one. A
/// future must therefore register its waker on every poll.
pub(crate) fn first_poll<F: Future>(future: F, start: Start) -> Started<F> {
    let pace = match start {
        Start::Caller(pace) => pace,
        Start::Runtime => return Started::Pending(Paced::new(future, None)),
    };
    let mut call = Paced::new(future, Some(pace));
    if call.hands_off() {
        return Started::Pending(call);
    }
    let _runtime = enter();
    #[cfg(all(feature = "node", feature = "arrow-api"))]
    let _mark = Mark(FIRST_POLL.replace(true));
    match Pin::new(&mut call).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(output) => Started::Ready(output),
        Poll::Pending => Started::Pending(call),
    }
}

/// The process that made a value. A forked child must not touch what the
/// parent's runtime runs: the child does not have its threads, and the fork
/// may have copied its locks held.
#[cfg(feature = "python")]
#[derive(Clone, Copy)]
pub(crate) struct Process {
    forks: usize,
}

#[cfg(feature = "python")]
impl Process {
    /// Return this process.
    pub(crate) fn current() -> Self {
        Self {
            forks: FORKS.load(Ordering::Relaxed),
        }
    }

    /// Return whether this is still the process, and not a forked child.
    pub(crate) fn is_current(self) -> bool {
        FORKS.load(Ordering::Relaxed) == self.forks
    }
}

/// Aborts a task that [`spawn`] started, in the process that started it.
#[cfg(feature = "python")]
#[derive(Clone)]
pub(crate) struct Abort {
    task: AbortHandle,
    process: Process,
}

#[cfg(feature = "python")]
impl Abort {
    /// Return the handle that aborts `task`, which this process spawned.
    pub(crate) fn new<T>(task: &JoinHandle<T>) -> Self {
        Self {
            task: task.abort_handle(),
            process: Process::current(),
        }
    }

    /// Abort the task. A forked child does nothing: aborting would schedule
    /// the task on the parent's runtime.
    pub(crate) fn abort(&self) {
        if self.process.is_current() {
            self.task.abort();
        }
    }
}

/// Run a blocking function on the current process's runtime.
#[cfg(feature = "python")]
pub(crate) fn spawn_blocking<F, R>(function: F) -> JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    runtime().spawn_blocking(function)
}

/// Run `call`, and return the message of a panic in it instead of letting
/// the panic unwind into the host. Nothing that `call` captured is used after
/// a panic, other than to drop it.
///
/// The panic's payload is dropped here too, and leaked if its own drop
/// panics, so that no second panic unwinds into the host either.
pub(crate) fn catch<T>(call: impl FnOnce() -> T) -> Result<T, String> {
    panic::catch_unwind(AssertUnwindSafe(call)).map_err(|payload| {
        let message = panic_message(&*payload);
        if let Err(again) = panic::catch_unwind(AssertUnwindSafe(|| drop(payload))) {
            mem::forget(again);
        }
        message
    })
}

/// Run `call`, which may block its thread. Tokio refuses to block one of its
/// workers, as when an async export calls a Python function that calls a
/// synchronous method, so a worker first hands its other work away.
pub fn allow_blocking<T>(call: impl FnOnce() -> T) -> T {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(call)
        }
        _ => call(),
    }
}

/// Run `call`, and return a panic in it as `Err` with `panic_error` of the
/// panic's message: the synchronous form of [`catch_panic`].
pub(crate) fn catch_error<T, E>(
    call: impl FnOnce() -> Result<T, E>,
    panic_error: fn(String) -> E,
) -> Result<T, E> {
    catch(call).unwrap_or_else(|message| Err(panic_error(message)))
}

/// Resolve a future that panics as `Err` with `panic_error` of the panic's
/// message, instead of unwinding into the host's executor. The future drops
/// as soon as it finishes or panics, and a panic in its drop resolves it as
/// `Err` too; it is never polled again.
pub(crate) async fn catch_panic<F, T, E>(future: F, panic_error: fn(String) -> E) -> Result<T, E>
where
    F: Future<Output = Result<T, E>>,
{
    use std::future;
    use std::pin::pin;

    let mut future = pin!(Some(future));
    future::poll_fn(|context| {
        let polled = catch(|| {
            let poll = future
                .as_mut()
                .as_pin_mut()
                .expect("a future that finished is not polled again")
                .poll(context);
            if poll.is_ready() {
                future.set(None);
            }
            poll
        });
        polled.unwrap_or_else(|message| {
            // The future that panicked drops here, where a second panic
            // cannot escape either.
            let _ = catch(|| future.set(None));
            Poll::Ready(Err(panic_error(message)))
        })
    })
    .await
}

/// Return a panic's message, as pyo3 reports it for a synchronous call.
#[cold]
fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else {
        crate::runtime::PANIC_WITHOUT_MESSAGE.to_owned()
    }
}

/// Return the current process's runtime, starting it on first use and again
/// in a forked child.
///
/// # Panics
///
/// Panics if the operating system cannot start the runtime's threads.
fn runtime() -> &'static Runtime {
    let current = RUNTIME.load(Ordering::Acquire);
    if current.is_null() {
        start()
    } else {
        // SAFETY: `start` publishes only runtimes that it leaked, which live
        // until the process ends.
        unsafe { &*current }
    }
}

/// Start a runtime and publish it, unless another thread published one
/// first.
#[cold]
fn start() -> &'static Runtime {
    #[cfg(unix)]
    forget_runtime_in_forked_children();
    let runtime = Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("the operating system must start the threads of the async runtime");
    // Never drop a published runtime: in a forked child, dropping the
    // parent's would wait for its threads, which the child does not have.
    let runtime = Box::into_raw(Box::new(runtime));
    match RUNTIME.compare_exchange(
        ptr::null_mut(),
        runtime,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        // SAFETY: the leaked runtime lives until the process ends.
        Ok(_) => unsafe { &*runtime },
        Err(current) => {
            // SAFETY: the runtime was not published, so this thread owns it.
            unsafe { Box::from_raw(runtime) }.shutdown_background();
            // SAFETY: as in `runtime`.
            unsafe { &*current }
        }
    }
}

/// Make the child of every later `fork` start a runtime of its own, before
/// any runtime is published.
#[cfg(unix)]
fn forget_runtime_in_forked_children() {
    use std::ffi::c_int;
    use std::sync::Once;

    unsafe extern "C" {
        fn pthread_atfork(
            prepare: Option<unsafe extern "C" fn()>,
            parent: Option<unsafe extern "C" fn()>,
            child: Option<unsafe extern "C" fn()>,
        ) -> c_int;
    }

    /// Run in the child, which has no thread of the parent's runtime.
    unsafe extern "C" fn forget_runtime() {
        RUNTIME.store(ptr::null_mut(), Ordering::Release);
        #[cfg(feature = "python")]
        FORKS.fetch_add(1, Ordering::Relaxed);
    }

    static REGISTERED: Once = Once::new();
    REGISTERED.call_once(|| {
        // SAFETY: the handler only updates atomics, which is
        // async-signal-safe, as the child of a threaded process requires.
        let status = unsafe { pthread_atfork(None, None, Some(forget_runtime)) };
        assert_eq!(
            status, 0,
            "the C library must register the fork handler of the async runtime"
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A panic payload whose drop panics again.
    struct PanicsWhenDropped;

    impl Drop for PanicsWhenDropped {
        fn drop(&mut self) {
            panic!("the payload's drop panics");
        }
    }

    fn fail() -> Result<(), String> {
        panic::panic_any(PanicsWhenDropped)
    }

    #[test]
    fn a_payload_whose_drop_panics_is_leaked() {
        assert_eq!(
            catch(fail),
            Err(crate::runtime::PANIC_WITHOUT_MESSAGE.to_owned())
        );
        assert_eq!(catch(|| panic!("plain")), Err::<(), _>("plain".to_owned()));
    }

    #[test]
    fn a_future_whose_payload_panics_when_dropped_still_resolves() {
        use std::pin::pin;

        let future = catch_panic(async { fail() }, |message| message);
        let mut context = Context::from_waker(Waker::noop());
        assert_eq!(
            pin!(future).poll(&mut context),
            Poll::Ready(Err(crate::runtime::PANIC_WITHOUT_MESSAGE.to_owned()))
        );
    }

    #[test]
    fn a_future_whose_drop_panics_resolves_with_the_panic() {
        use std::pin::pin;

        /// A future that finishes at once, and whose drop panics.
        struct FinishesThenPanicsWhenDropped;

        impl Future for FinishesThenPanicsWhenDropped {
            type Output = Result<u32, String>;

            fn poll(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Self::Output> {
                Poll::Ready(Ok(1))
            }
        }

        impl Drop for FinishesThenPanicsWhenDropped {
            fn drop(&mut self) {
                panic!("the future's drop panics");
            }
        }

        let future = catch_panic(FinishesThenPanicsWhenDropped, |message| message);
        let mut context = Context::from_waker(Waker::noop());
        assert_eq!(
            pin!(future).poll(&mut context),
            Poll::Ready(Err("the future's drop panics".to_owned()))
        );
    }

    /// Guards the mark that lets a stream that JavaScript writes return
    /// `Pending` on its thread: it holds only during a first poll, a nested
    /// one leaves it set, and a panic does not leave it set.
    #[cfg(all(feature = "node", feature = "arrow-api"))]
    #[test]
    fn a_first_poll_marks_its_thread_only_while_it_polls() {
        static PACE: Pace = Pace::new();
        let nested = first_poll(
            async {
                let inner = first_poll(async { in_first_poll() }, Start::Caller(&PACE));
                matches!(inner, Started::Ready(true)) && in_first_poll()
            },
            Start::Caller(&PACE),
        );
        assert!(matches!(nested, Started::Ready(true)));
        assert!(!in_first_poll());
        assert!(matches!(
            first_poll(async { in_first_poll() }, Start::Runtime),
            Started::Pending(_)
        ));

        let panicked = catch(|| first_poll(async { fail() }, Start::Caller(&PACE)));
        assert!(panicked.is_err(), "the poll panics");
        assert!(!in_first_poll(), "the panic clears the mark");
    }

    /// Guards the blocking read inside a first poll: it clears the mark while
    /// it runs, so the fed stream fails it instead of hanging, and the mark
    /// comes back after it, also after a panic.
    #[cfg(all(feature = "node", feature = "arrow-api"))]
    #[test]
    fn a_blocking_read_clears_the_mark_until_it_ends() {
        static PACE: Pace = Pace::new();
        let read = first_poll(
            async {
                let inside = {
                    let _mark = leave_first_poll();
                    in_first_poll()
                };
                let panicked = catch(|| {
                    let _mark = leave_first_poll();
                    fail()
                });
                (inside, panicked.is_err(), in_first_poll())
            },
            Start::Caller(&PACE),
        );
        assert!(matches!(read, Started::Ready((false, true, true))));
    }
}
