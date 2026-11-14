//! Async calls. Each returns a promise, and runs its future to its first wait
//! on the JavaScript thread, unless it takes a `Callback` or a stream. A call
//! that waits goes on the shared runtime of [`executor`], and the worker that
//! finishes it hands the result back to the JavaScript thread through the
//! environment's courier, which converts it and settles the promise.
//!
//! A pending call keeps the event loop alive, as a pending timer does. When
//! the environment tears down first, the result is dropped without calling
//! Node-API.

use std::future::Future;
use std::ptr;

use napi_sys as sys;

use super::courier::{Courier, Job};
use super::env::{self, Env, napi};
use super::error::Error;
use super::value::Value;
use crate::runtime::executor::{self, Start, Started};

/// Start an async call and return a promise of its result, which the call's
/// [`deferred`] conversion makes on the JavaScript thread.
///
/// The promise rejects with the call's error, the conversion's, or a panic
/// in either as `Error("Rust panicked: …")`.
///
/// # Errors
///
/// Returns [`Error::Pending`] when Node-API cannot create the promise.
#[doc(hidden)]
pub fn awaitable<'env, F, D>(env: Env<'env>, start: Start, future: F) -> Result<Value<'env>, Error>
where
    F: Future<Output = Result<Deferred<D>, Error>> + Send + 'static,
    D: for<'call> FnOnce(Env<'call>) -> Result<Value<'call>, Error> + Send + 'static,
{
    promise_of(env, start, future, Hold::EventLoop)
}

/// Return a promise of `future`'s result, as [`awaitable`] does, which does
/// not keep Node.js running while it is pending, as an `unref()`'d timer does
/// not.
///
/// # Errors
///
/// Returns [`Error::Pending`] when Node-API cannot create the promise.
#[doc(hidden)]
pub fn unref_awaitable<'env, F, D>(
    env: Env<'env>,
    start: Start,
    future: F,
) -> Result<Value<'env>, Error>
where
    F: Future<Output = Result<Deferred<D>, Error>> + Send + 'static,
    D: for<'call> FnOnce(Env<'call>) -> Result<Value<'call>, Error> + Send + 'static,
{
    promise_of(env, start, future, Hold::Nothing)
}

/// Whether a pending promise keeps Node.js running.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Hold {
    /// It keeps the event loop alive, as a pending timer does.
    EventLoop,
    /// It does not.
    Nothing,
}

/// Return a promise of `future`'s result. Unless `start` gives the call to a
/// worker, the future runs to its first wait on this thread, which settles the
/// promise of a call that finishes there without the courier. A call that
/// waits goes on the shared runtime, and the worker that finishes it posts
/// the result to the environment's courier.
fn promise_of<'env, F, D>(
    env: Env<'env>,
    start: Start,
    future: F,
    hold: Hold,
) -> Result<Value<'env>, Error>
where
    F: Future<Output = Result<Deferred<D>, Error>> + Send + 'static,
    D: for<'call> FnOnce(Env<'call>) -> Result<Value<'call>, Error> + Send + 'static,
{
    let mut deferred = ptr::null_mut();
    let mut promise = ptr::null_mut();
    napi!(env, napi_create_promise(&mut deferred, &mut promise))?;
    let call = match executor::first_poll(executor::catch_panic(future, Error::panic), start) {
        Started::Ready(outcome) => {
            resolve(env, Promise(deferred), outcome);
            return Ok(env.value(promise));
        }
        Started::Pending(call) => call,
    };
    // The call may have run up to its first wait, so a failure here rejects
    // its promise, as a failure of the call would, and the call drops
    // unfinished.
    let sender = match Sender::start(env, deferred, hold) {
        Ok(sender) => sender,
        Err(error) => {
            resolve::<D>(env, Promise(deferred), Err(error));
            return Ok(env.value(promise));
        }
    };
    executor::spawn(async move {
        sender.send(call.await);
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

/// The promise of one call.
struct Promise(sys::napi_deferred);

// SAFETY: a worker only carries the promise back to the JavaScript thread,
// which alone settles it.
unsafe impl Send for Promise {}

/// The worker's end of one call, which hands its outcome to the JavaScript
/// thread once. The call's hold on the event loop goes with the outcome, or
/// with the drop of a sender that never sent.
struct Sender {
    courier: Courier,
    promise: Option<Promise>,
    hold: Hold,
}

impl Sender {
    /// Start sending the outcome that settles `promise`, which keeps the
    /// event loop alive as `hold` says.
    fn start(env: Env<'_>, promise: sys::napi_deferred, hold: Hold) -> Result<Self, Error> {
        let station = &env.state()?.station;
        if hold == Hold::EventLoop {
            station.hold(env)?;
        }
        Ok(Self {
            courier: station.courier(),
            promise: Some(Promise(promise)),
            hold,
        })
    }

    /// Hand the call's outcome to the JavaScript thread, or drop it when the
    /// environment has torn down.
    fn send<D>(mut self, outcome: Result<Deferred<D>, Error>)
    where
        D: for<'call> FnOnce(Env<'call>) -> Result<Value<'call>, Error> + Send + 'static,
    {
        let promise = self.promise.take().expect("a call sends its outcome once");
        let hold = self.hold;
        let job: Job = Box::new(move |env| {
            resolve(env, promise, outcome);
            release(env, hold);
        });
        if let Err(job) = self.courier.post(job) {
            env::discard(job);
        }
    }
}

impl Drop for Sender {
    fn drop(&mut self) {
        if self.promise.is_none() {
            return;
        }
        // A call whose future never finished releases its hold too, so the event
        // loop does not wait for it forever; its promise stays pending.
        let hold = self.hold;
        if let Err(job) = self.courier.post(Box::new(move |env| release(env, hold))) {
            env::discard(job);
        }
    }
}

/// Release a finished call's hold on the event loop, if it has one.
fn release(env: Env<'_>, hold: Hold) {
    if hold == Hold::EventLoop
        && let Ok(state) = env.state()
    {
        state.station.release(env);
    }
}

/// Convert a finished call's result on the JavaScript thread, and settle its
/// promise.
fn resolve<D>(env: Env<'_>, promise: Promise, outcome: Result<Deferred<D>, Error>)
where
    D: for<'call> FnOnce(Env<'call>) -> Result<Value<'call>, Error>,
{
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
