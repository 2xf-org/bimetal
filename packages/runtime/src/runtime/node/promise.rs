//! Async calls. Each returns a promise and runs its future on the shared
//! runtime of [`executor`], and the worker that finishes it hands the result
//! back to the JavaScript thread through a thread-safe function of its own,
//! which converts it and settles the promise.
//!
//! A pending call keeps the event loop alive, as a pending timer does. When
//! the environment tears down first, the result is dropped without calling
//! Node-API.

use std::ffi::c_void;
use std::future::Future;
use std::marker::PhantomData;
use std::ptr;
use std::sync::{Arc, Mutex};

use napi_sys as sys;

use super::env::{self, Env, napi};
use super::error::Error;
use super::value::Value;
use crate::runtime::executor;

/// Run an async call on the shared Tokio runtime and return a promise of its
/// result, which the call's [`deferred`] conversion makes on the JavaScript
/// thread.
///
/// The promise rejects with the call's error, the conversion's, or a panic
/// in either as `Error("Rust panicked: …")`.
///
/// # Errors
///
/// Returns [`Error::Pending`] when Node-API cannot create the promise.
#[doc(hidden)]
pub fn awaitable<'env, F, D>(env: Env<'env>, future: F) -> Result<Value<'env>, Error>
where
    F: Future<Output = Result<Deferred<D>, Error>> + Send + 'static,
    D: for<'call> FnOnce(Env<'call>) -> Result<Value<'call>, Error> + Send + 'static,
{
    promise_of(env, future, Hold::EventLoop)
}

/// Return a promise of `future`'s result, as [`awaitable`] does, which does
/// not keep Node.js running while it is pending, as an `unref()`'d timer does
/// not.
///
/// # Errors
///
/// Returns [`Error::Pending`] when Node-API cannot create the promise.
#[doc(hidden)]
pub fn unref_awaitable<'env, F, D>(env: Env<'env>, future: F) -> Result<Value<'env>, Error>
where
    F: Future<Output = Result<Deferred<D>, Error>> + Send + 'static,
    D: for<'call> FnOnce(Env<'call>) -> Result<Value<'call>, Error> + Send + 'static,
{
    promise_of(env, future, Hold::Nothing)
}

/// Whether a pending promise keeps Node.js running.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Hold {
    /// It keeps the event loop alive, as a pending timer does.
    EventLoop,
    /// It does not.
    Nothing,
}

/// Run `future` on the shared runtime, and return a promise of its result.
fn promise_of<'env, F, D>(env: Env<'env>, future: F, hold: Hold) -> Result<Value<'env>, Error>
where
    F: Future<Output = Result<Deferred<D>, Error>> + Send + 'static,
    D: for<'call> FnOnce(Env<'call>) -> Result<Value<'call>, Error> + Send + 'static,
{
    let mut deferred = ptr::null_mut();
    let mut promise = ptr::null_mut();
    napi!(env, napi_create_promise(&mut deferred, &mut promise))?;
    let sender = Sender::<D>::start(env, deferred, hold)?;
    executor::spawn(async move {
        sender.send(executor::catch_panic(future, Error::panic).await);
    });
    Ok(env.value(promise))
}

/// Defer a result conversion until the call returns to the JavaScript thread.
#[doc(hidden)]
pub fn deferred<D>(convert: D) -> Deferred<D>
where
    D: for<'call> FnOnce(Env<'call>) -> Result<Value<'call>, Error>,
{
    Deferred(convert)
}

/// A result whose conversion to JavaScript runs on the JavaScript thread. It
/// does nothing until converted.
#[doc(hidden)]
#[must_use]
pub struct Deferred<D>(D);

/// The reason to expect when locking a [`Slot`]: only a copy of its handle or
/// an assignment to it runs under the lock, and neither can panic.
const UNPOISONED: &str = "no code panics while holding a thread-safe function's slot";

/// The thread-safe function of one call while Node.js keeps it, which its
/// finalizer empties.
type Slot = Mutex<Option<Function>>;

/// A thread-safe function.
#[derive(Clone, Copy)]
struct Function(sys::napi_threadsafe_function);

// SAFETY: Node-API lets any thread call and release a thread-safe function
// until Node.js finalizes it, which empties its slot under the slot's lock.
unsafe impl Send for Function {}

/// The promise of one call.
struct Promise(sys::napi_deferred);

// SAFETY: a worker only carries the promise back to the JavaScript thread,
// which alone settles it.
unsafe impl Send for Promise {}

/// What a finished call hands back to the JavaScript thread.
struct Completion<D> {
    promise: Promise,
    outcome: Result<Deferred<D>, Error>,
}

/// The worker's end of one call's thread-safe function, which it uses once,
/// and releases when it drops, whether or not the call's future finished.
struct Sender<D> {
    slot: Arc<Slot>,
    promise: Option<Promise>,
    /// Whether Node.js answered the call with `napi_closing`, after which
    /// this thread must not release the function.
    closing: bool,
    conversion: PhantomData<fn() -> D>,
}

impl<D> Sender<D>
where
    D: for<'call> FnOnce(Env<'call>) -> Result<Value<'call>, Error> + Send + 'static,
{
    /// Create the thread-safe function that settles `promise`, which keeps
    /// the event loop alive as `hold` says.
    fn start(env: Env<'_>, promise: sys::napi_deferred, hold: Hold) -> Result<Self, Error> {
        let slot: Arc<Slot> = Arc::new(Mutex::new(None));
        let name = env.string("bimetal")?;
        let finalize_data = Arc::into_raw(Arc::clone(&slot));
        let mut function = ptr::null_mut();
        let created = napi!(
            env,
            napi_create_threadsafe_function(
                ptr::null_mut(),
                ptr::null_mut(),
                name.raw,
                0,
                1,
                finalize_data.cast_mut().cast::<c_void>(),
                Some(finalize),
                ptr::null_mut(),
                Some(settle::<D>),
                &mut function
            )
        );
        if let Err(error) = created {
            // SAFETY: Node-API did not take the reference, which `into_raw`
            // made.
            drop(unsafe { Arc::from_raw(finalize_data) });
            return Err(error);
        }
        // The finalizer runs on this thread, so not before this.
        *slot.lock().expect(UNPOISONED) = Some(Function(function));
        let sender = Self {
            slot,
            promise: Some(Promise(promise)),
            closing: false,
            conversion: PhantomData,
        };
        if hold == Hold::Nothing {
            // The function stays callable; only the event loop stops waiting
            // for it. On failure, the sender's drop releases the function.
            napi!(env, napi_unref_threadsafe_function(function))?;
        }
        Ok(sender)
    }

    /// Hand the call's outcome to the JavaScript thread, or drop it when the
    /// environment has torn down.
    fn send(mut self, outcome: Result<Deferred<D>, Error>) {
        let promise = self.promise.take().expect("a call sends its outcome once");
        let completion = Box::into_raw(Box::new(Completion { promise, outcome }));
        let taken = {
            let slot = self.slot.lock().expect(UNPOISONED);
            slot.is_some_and(|function| {
                // SAFETY: Node.js has not finalized the function, whose slot
                // is still full, and cannot while this thread holds the
                // lock. Node.js takes the box only when the call succeeds.
                let status = unsafe {
                    sys::napi_call_threadsafe_function(
                        function.0,
                        completion.cast::<c_void>(),
                        sys::ThreadsafeFunctionCallMode::blocking,
                    )
                };
                self.closing = status == sys::Status::napi_closing;
                status == sys::Status::napi_ok
            })
        };
        if !taken {
            // SAFETY: Node-API did not take the box, which `into_raw` made.
            env::discard(unsafe { Box::from_raw(completion) });
        }
    }
}

impl<D> Drop for Sender<D> {
    fn drop(&mut self) {
        // A call whose future never finished releases the function too, so
        // that the event loop does not wait for it forever.
        if self.closing {
            return;
        }
        if let Some(function) = *self.slot.lock().expect(UNPOISONED) {
            // SAFETY: Node.js finalizes the function only once every thread
            // released it, or when the environment tears down, which empties
            // the slot first; this thread holds the slot's lock, and
            // releases its one reference once.
            unsafe {
                sys::napi_release_threadsafe_function(
                    function.0,
                    sys::ThreadsafeFunctionReleaseMode::release,
                );
            }
        }
    }
}

/// Empty the slot of a thread-safe function that Node.js finalizes.
unsafe extern "C" fn finalize(_env: sys::napi_env, data: *mut c_void, _hint: *mut c_void) {
    // SAFETY: `Sender::start` gave Node.js this reference, which Node.js
    // returns once.
    let slot = unsafe { Arc::from_raw(data.cast_const().cast::<Slot>()) };
    *slot.lock().expect(UNPOISONED) = None;
}

/// Convert a finished call's result on the JavaScript thread, and settle its
/// promise. Without an environment, which is tearing down, drop it.
unsafe extern "C" fn settle<D>(
    env: sys::napi_env,
    _callback: sys::napi_value,
    _context: *mut c_void,
    data: *mut c_void,
) where
    D: for<'call> FnOnce(Env<'call>) -> Result<Value<'call>, Error>,
{
    // SAFETY: `Sender::send` passed this box, which Node.js returns once.
    let completion = unsafe { Box::from_raw(data.cast::<Completion<D>>()) };
    if env.is_null() {
        env::discard(completion);
        return;
    }
    // SAFETY: Node.js calls a thread-safe function's callback on the
    // environment's thread, with a scope open until it returns.
    let env = unsafe { Env::new(env) };
    let Completion { promise, outcome } = *completion;
    let converted = executor::catch_error(
        || outcome.and_then(|Deferred(convert)| convert(env)),
        Error::panic,
    );
    let settled = match converted {
        Ok(value) if !env.pending() => napi!(env, napi_resolve_deferred(promise.0, value.raw)),
        Ok(_) => reject(env, promise, Error::Pending),
        Err(error) => reject(env, promise, error),
    };
    // An exception left pending here would reach Node.js as uncaught.
    if settled.is_err() {
        env.take_exception();
    }
}

/// Reject `promise` with `error`, or with the exception that making it left
/// pending.
fn reject(env: Env<'_>, promise: Promise, error: Error) -> Result<(), Error> {
    let exception = match env.exception(error) {
        Ok(exception) => exception,
        Err(_) => env.exception(Error::Pending)?,
    };
    napi!(env, napi_reject_deferred(promise.0, exception.raw))
}
