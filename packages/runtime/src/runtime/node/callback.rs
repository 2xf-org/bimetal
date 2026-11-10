//! JavaScript functions that Rust calls.
//!
//! A call on the JavaScript thread, inside a synchronous export, calls the
//! function directly. A call from another thread, such as a Tokio worker that
//! runs an async export, hands its arguments to the JavaScript thread through
//! a thread-safe function, and waits for the result.

use std::ffi::c_void;
use std::marker::PhantomData;
use std::ptr;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, ThreadId};

use napi_sys as sys;

use super::env::{self, Env, napi};
use super::error::Error;
use super::value::Value;
use crate::runtime::{Callback, CallbackError};

/// The reason to expect when locking a [`Slot`]: only a copy of its handle or
/// an assignment to it runs under the lock, and neither can panic.
const UNPOISONED: &str = "no code panics while holding a callback's slot";

/// What the callback holds of one JavaScript function while Node.js keeps its
/// thread-safe function, which the finalizer empties.
type Slot = Mutex<Option<Function>>;

/// The handles of one JavaScript function.
#[derive(Clone, Copy)]
struct Function {
    /// The thread-safe function that other threads call through.
    threadsafe: sys::napi_threadsafe_function,
    /// The reference that keeps the JavaScript function alive.
    reference: sys::napi_ref,
    /// The environment that the JavaScript function belongs to.
    env: sys::napi_env,
}

// SAFETY: Node-API lets any thread call and release a thread-safe function
// until Node.js finalizes it, which empties its slot under the slot's lock.
// Only the JavaScript thread, which `Owner::thread` names, uses the reference
// and the environment.
unsafe impl Send for Function {}

/// The state behind a callback, whose last clone releases the function.
struct Owner {
    slot: Arc<Slot>,
    thread: ThreadId,
}

impl Drop for Owner {
    fn drop(&mut self) {
        if let Some(function) = *self.slot.lock().expect(UNPOISONED) {
            // SAFETY: the slot is full, so Node.js has not finalized the
            // function, and cannot while this thread holds the slot's lock;
            // the owner releases its one reference once.
            unsafe {
                sys::napi_release_threadsafe_function(
                    function.threadsafe,
                    sys::ThreadsafeFunctionReleaseMode::release,
                );
            }
        }
    }
}

/// How one callback converts its values, on the JavaScript thread.
struct Conversions<A, R, P, D> {
    prepare: P,
    decode: D,
    signature: PhantomData<fn(&A) -> R>,
}

/// One call from another thread, which waits for `reply`.
struct Job<A, R, P, D> {
    arguments: A,
    function: Function,
    conversions: Arc<Conversions<A, R, P, D>>,
    reply: Sender<Result<R, CallbackError>>,
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
    let reference = env.keep(value)?;
    let slot: Arc<Slot> = Arc::new(Mutex::new(None));
    let finalize_data = Arc::into_raw(Arc::clone(&slot));
    let name = env.string("bimetal")?;
    let mut threadsafe = ptr::null_mut();
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
            Some(run::<A, R, P, D>),
            &mut threadsafe
        )
    );
    if let Err(error) = created {
        // SAFETY: Node-API did not take the reference, which `into_raw` made.
        drop(unsafe { Arc::from_raw(finalize_data) });
        return Err(error);
    }
    // A function that Rust keeps must not keep the event loop alive.
    napi!(env, napi_unref_threadsafe_function(threadsafe))?;
    let function = Function {
        threadsafe,
        reference,
        env: env.raw,
    };
    *slot.lock().expect(UNPOISONED) = Some(function);
    let owner = Owner {
        slot,
        thread: thread::current().id(),
    };
    let conversions = Arc::new(Conversions {
        prepare,
        decode,
        signature: PhantomData,
    });
    Ok(Callback::new(move |arguments: A| {
        let slot = owner.slot.lock().expect(UNPOISONED);
        let function = (*slot).ok_or_else(closed)?;
        if thread::current().id() == owner.thread {
            drop(slot);
            // SAFETY: this is the environment's thread, inside the call of an
            // export, or the slot would be empty.
            let env = unsafe { Env::new(function.env) };
            return call(env, function.reference, &conversions, &arguments);
        }
        let (reply, answer) = mpsc::channel();
        let job = Box::into_raw(Box::new(Job {
            arguments,
            function,
            conversions: Arc::clone(&conversions),
            reply,
        }));
        // SAFETY: the slot is full, so Node.js has not finalized the function,
        // and cannot while this thread holds the slot's lock. The queue has no
        // limit, so a blocking call does not wait under the lock.
        let status = unsafe {
            sys::napi_call_threadsafe_function(
                function.threadsafe,
                job.cast::<c_void>(),
                sys::ThreadsafeFunctionCallMode::blocking,
            )
        };
        drop(slot);
        if status != sys::Status::napi_ok {
            // SAFETY: Node-API did not take the box, which `into_raw` made.
            env::discard(unsafe { Box::from_raw(job) });
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
    reference: sys::napi_ref,
    conversions: &Conversions<A, R, P, D>,
    arguments: &A,
) -> Result<R, CallbackError>
where
    P: for<'call> Fn(Env<'call>, &A) -> Result<Value<'call>, Error>,
    D: Fn(Value<'_>) -> Result<R, Error>,
{
    let scope = napi!(env, napi_open_handle_scope() -> out).map_err(|error| failure(env, error))?;
    let result = (|| {
        let function = env.reference(reference)?;
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

/// Empty the slot of a thread-safe function that Node.js finalizes, and let
/// the JavaScript function go.
unsafe extern "C" fn finalize(env: sys::napi_env, data: *mut c_void, _hint: *mut c_void) {
    // SAFETY: `callback` gave Node.js this reference, which Node.js returns
    // once.
    let slot = unsafe { Arc::from_raw(data.cast_const().cast::<Slot>()) };
    let function = slot.lock().expect(UNPOISONED).take();
    if let Some(function) = function {
        // SAFETY: Node.js runs a finalizer on the environment's thread, and
        // the reference is this function's, which nothing uses any more.
        unsafe { sys::napi_delete_reference(env, function.reference) };
    }
}

/// Run a call from another thread on the JavaScript thread. Without an
/// environment, which is tearing down, drop it, which tells the caller.
unsafe extern "C" fn run<A, R, P, D>(
    env: sys::napi_env,
    _callback: sys::napi_value,
    _context: *mut c_void,
    data: *mut c_void,
) where
    P: for<'call> Fn(Env<'call>, &A) -> Result<Value<'call>, Error>,
    D: Fn(Value<'_>) -> Result<R, Error>,
{
    // SAFETY: the thread that made the call passed this box, which Node.js
    // returns once.
    let job = unsafe { Box::from_raw(data.cast::<Job<A, R, P, D>>()) };
    if env.is_null() {
        env::discard(job);
        return;
    }
    // SAFETY: Node.js calls this on the environment's thread, with a scope
    // open until it returns.
    let env = unsafe { Env::new(env) };
    let Job {
        arguments,
        function,
        conversions,
        reply,
    } = *job;
    let result =
        crate::runtime::executor::catch(|| call(env, function.reference, &conversions, &arguments));
    // A caller that stopped waiting has nothing to hear.
    let _ =
        reply
            .send(result.unwrap_or_else(|panic| {
                Err(CallbackError::new(format!("Rust panicked: {panic}")))
            }));
    env::discard((arguments, conversions));
}
