//! JavaScript functions that Rust calls.
//!
//! A call on the JavaScript thread, inside a synchronous export, calls the
//! function directly. A call from another thread, such as a Tokio worker that
//! runs an async export, hands its arguments to the JavaScript thread through
//! the environment's courier, and waits for the result.

use std::marker::PhantomData;
use std::sync::{Arc, mpsc};
use std::thread::{self, ThreadId};

use napi_sys as sys;

use super::courier::{Courier, Job};
use super::env::{self, Env, napi};
use super::error::Error;
use super::value::Value;
use crate::runtime::executor;
use crate::runtime::{Callback, CallbackError};

/// The handles of one JavaScript function.
#[derive(Clone, Copy)]
struct Function {
    /// The reference that keeps the JavaScript function alive.
    reference: sys::napi_ref,
    /// The environment that the JavaScript function belongs to.
    env: sys::napi_env,
}

// SAFETY: only the JavaScript thread that `Owner::thread` names, directly or
// through the courier, uses the reference and the environment; others copy handles.
unsafe impl Send for Function {}
// SAFETY: as for `Send`.
unsafe impl Sync for Function {}

/// The state behind a callback, whose last clone lets the function go.
struct Owner {
    courier: Courier,
    function: Function,
    thread: ThreadId,
}

impl Drop for Owner {
    fn drop(&mut self) {
        let function = self.function;
        // The JavaScript thread deletes the reference, at once and with no wake-up when
        // this is that thread; an environment that tears down first frees it anyway.
        if thread::current().id() == self.thread {
            if self.courier.is_live() {
                // SAFETY: this is the live environment's thread, and the reference is
                // this function's, which nothing else uses.
                unsafe { sys::napi_delete_reference(function.env, function.reference) };
            }
            return;
        }
        let release: Job = Box::new(move |env| {
            let function = function;
            // SAFETY: the job runs on the environment's thread, and the
            // reference is this function's, which nothing uses any more.
            unsafe { sys::napi_delete_reference(env.raw, function.reference) };
        });
        if let Err(job) = self.courier.post(release) {
            env::discard(job);
        }
    }
}

/// How one callback converts its values, on the JavaScript thread.
struct Conversions<A, R, P, D> {
    prepare: P,
    decode: D,
    signature: PhantomData<fn(&A) -> R>,
}

/// Keep the JavaScript function `value` as a [`Callback`], which converts its
/// arguments with `prepare` and its result with `decode`.
pub(crate) fn callback<A, R, P, D>(
    value: Value<'_>,
    prepare: P,
    decode: D,
) -> Result<Callback<A, R>, Error>
where
    A: Send + 'static,
    R: Send + 'static,
    P: for<'call> Fn(Env<'call>, &A) -> Result<Value<'call>, Error> + Send + Sync + 'static,
    D: Fn(Value<'_>) -> Result<R, Error> + Send + Sync + 'static,
{
    let env = value.env();
    let courier = env.state()?.station.courier();
    let owner = Owner {
        courier,
        function: Function {
            reference: env.keep(value)?,
            env: env.raw,
        },
        thread: thread::current().id(),
    };
    let conversions = Arc::new(Conversions {
        prepare,
        decode,
        signature: PhantomData,
    });
    Ok(Callback::new(move |arguments: A| {
        if !owner.courier.is_live() {
            return Err(closed());
        }
        let function = owner.function;
        if thread::current().id() == owner.thread {
            // SAFETY: this is the environment's thread, which is live, so it
            // runs Rust only inside a call that Node.js makes.
            let env = unsafe { Env::new(function.env) };
            return call(env, function, &conversions, &arguments);
        }
        let (reply, answer) = mpsc::channel();
        let conversions = Arc::clone(&conversions);
        let job: Job = Box::new(move |env| {
            let function = function;
            let result = executor::catch(|| call(env, function, &conversions, &arguments));
            // A caller that stopped waiting has nothing to hear.
            let _ = reply.send(result.unwrap_or_else(|panic| {
                Err(CallbackError::new(format!("Rust panicked: {panic}")))
            }));
            env::discard((arguments, conversions));
        });
        if let Err(job) = owner.courier.post(job) {
            env::discard(job);
            return Err(closed());
        }
        answer.recv().map_err(|_| closed())?
    }))
}

/// The error of a call after the JavaScript environment closed.
fn closed() -> CallbackError {
    CallbackError::new("the JavaScript environment closed")
}

/// Call the function on the JavaScript thread, inside its own handle scope.
fn call<A, R, P, D>(
    env: Env<'_>,
    function: Function,
    conversions: &Conversions<A, R, P, D>,
    arguments: &A,
) -> Result<R, CallbackError>
where
    P: for<'call> Fn(Env<'call>, &A) -> Result<Value<'call>, Error>,
    D: Fn(Value<'_>) -> Result<R, Error>,
{
    let scope = napi!(env, napi_open_handle_scope() -> out).map_err(|error| failure(env, error))?;
    let result = (|| {
        let function = env.reference(function.reference)?;
        let arguments = (conversions.prepare)(env, arguments)?;
        // A tuple of no items serializes as `null`.
        let arguments = if arguments.is_nullish()? {
            env.array()?
        } else {
            arguments
        };
        let apply = function.named(c"apply")?;
        let result = apply.call(function, [env.undefined(), arguments])?;
        (conversions.decode)(result)
    })()
    .map_err(|error| failure(env, error));
    let closed = napi!(env, napi_close_handle_scope(scope));
    env::discard(closed);
    result
}

/// Turn a failure of a call into text, taking the exception that it left
/// pending.
fn failure(env: Env<'_>, error: Error) -> CallbackError {
    match error {
        Error::Pending => {
            let text = env
                .take_exception()
                .and_then(|exception| exception_text(exception).ok());
            // Reading an exception can throw another, such as for one without
            // a prototype, which must not stay pending.
            discard_exception(env);
            CallbackError::new(text.unwrap_or_else(|| "the JavaScript function threw".to_owned()))
        }
        other => CallbackError::new(other.to_string()),
    }
}

/// Clear a pending exception, if there is one.
fn discard_exception(env: Env<'_>) {
    let _ = env.take_exception();
}

/// Return the `message` of an exception, or else its text.
fn exception_text(exception: Value<'_>) -> Result<String, Error> {
    match exception.named(c"message") {
        Ok(message) if !message.is_nullish()? => message.to_js_string(),
        _ => exception.to_js_string(),
    }
}
