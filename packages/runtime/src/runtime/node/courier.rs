//! The one thread-safe function of an environment, which carries jobs from
//! other threads to its JavaScript thread: the results of async calls, and
//! the calls of `Callback`s from Tokio workers.
//!
//! The module initializer makes it, so a call that waits, or a callback
//! argument, makes no thread-safe function of its own. Node.js runs up to 999
//! waiting jobs each time the event loop wakes (`kMaxIterationCount` in its
//! `node_api.cc`), so results that finish together settle in one turn. The
//! function does not keep the event loop alive by itself: each pending call
//! that should, as a pending timer does, holds it until its job runs.

use std::cell::Cell;
use std::ffi::c_void;
use std::ptr;
use std::sync::{Arc, Mutex};

use napi_sys as sys;

use super::env::{self, Env, napi};
use super::error::Error;

/// What another thread asks the JavaScript thread to do. It drops without
/// running when the environment tears down first.
pub(super) type Job = Box<dyn for<'env> FnOnce(Env<'env>) + Send>;

/// The reason to expect when locking a [`Slot`]: only a copy of its handle,
/// a call of Node-API that cannot panic, or an assignment runs under it.
const UNPOISONED: &str = "no code panics while holding the courier's slot";

/// The thread-safe function while Node.js keeps it, which its finalizer
/// empties when the environment tears down.
type Slot = Mutex<Option<Function>>;

/// A thread-safe function.
#[derive(Clone, Copy)]
struct Function(sys::napi_threadsafe_function);

// SAFETY: Node-API lets any thread call a thread-safe function until Node.js
// finalizes it, which empties its slot under the slot's lock.
unsafe impl Send for Function {}

/// The sending end of an environment's courier, which any thread may hold
/// and use.
#[derive(Clone)]
pub(super) struct Courier {
    slot: Arc<Slot>,
}

/// What the environment's state keeps of its courier, on the JavaScript
/// thread.
pub(super) struct Station {
    courier: Courier,
    function: sys::napi_threadsafe_function,
    /// The pending calls that keep the event loop alive.
    holds: Cell<usize>,
}

impl Station {
    /// Make the courier of `env`, which does not keep the event loop alive.
    pub(super) fn start(env: Env<'_>) -> Result<Self, Error> {
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
                Some(deliver),
                &mut function
            )
        );
        if let Err(error) = created {
            // SAFETY: Node-API did not take the reference, which `into_raw`
            // made.
            drop(unsafe { Arc::from_raw(finalize_data) });
            return Err(error);
        }
        // The finalizer runs on this thread, so it cannot empty the slot before this
        // fills it.
        *slot.lock().expect(UNPOISONED) = Some(Function(function));
        napi!(env, napi_unref_threadsafe_function(function))?;
        Ok(Self {
            courier: Courier { slot },
            function,
            holds: Cell::new(0),
        })
    }

    /// The courier, for a thread that posts jobs.
    pub(super) fn courier(&self) -> Courier {
        self.courier.clone()
    }

    /// Keep the event loop alive until a matching [`Station::release`].
    pub(super) fn hold(&self, env: Env<'_>) -> Result<(), Error> {
        if self.holds.get() == 0 {
            napi!(env, napi_ref_threadsafe_function(self.function))?;
        }
        self.holds.set(self.holds.get() + 1);
        Ok(())
    }

    /// Let the event loop exit once no other hold remains.
    pub(super) fn release(&self, env: Env<'_>) {
        let holds = self.holds.get().saturating_sub(1);
        self.holds.set(holds);
        if holds == 0 {
            // Unreferencing fails only for an environment that tears down,
            // which no longer waits for the loop.
            let _ = napi!(env, napi_unref_threadsafe_function(self.function));
        }
    }
}

impl Courier {
    /// Hand `job` to the JavaScript thread, from any thread, or return it
    /// when the environment has torn down.
    pub(super) fn post(&self, job: Job) -> Result<(), Job> {
        let job = Box::into_raw(Box::new(job));
        let slot = self.slot.lock().expect(UNPOISONED);
        let status = slot.map(|function| {
            // SAFETY: Node.js cannot finalize the function while this thread holds the
            // full slot's lock; the unbounded queue never blocks, and takes the box on success.
            unsafe {
                sys::napi_call_threadsafe_function(
                    function.0,
                    job.cast::<c_void>(),
                    sys::ThreadsafeFunctionCallMode::blocking,
                )
            }
        });
        drop(slot);
        if status == Some(sys::Status::napi_ok) {
            Ok(())
        } else {
            // SAFETY: Node-API did not take the box, which `into_raw` made.
            Err(*unsafe { Box::from_raw(job) })
        }
    }

    /// Return whether the environment is still live, as it is on its own
    /// thread during any call that it makes into Rust.
    pub(super) fn is_live(&self) -> bool {
        self.slot.lock().expect(UNPOISONED).is_some()
    }
}

/// Empty the slot of the courier that Node.js finalizes as its environment
/// tears down.
unsafe extern "C" fn finalize(_env: sys::napi_env, data: *mut c_void, _hint: *mut c_void) {
    // SAFETY: `Station::start` gave Node.js this reference, which Node.js
    // returns once.
    let slot = unsafe { Arc::from_raw(data.cast_const().cast::<Slot>()) };
    *slot.lock().expect(UNPOISONED) = None;
}

/// Run a job on the JavaScript thread. Without an environment, which is
/// tearing down, drop it.
unsafe extern "C" fn deliver(
    env: sys::napi_env,
    _callback: sys::napi_value,
    _context: *mut c_void,
    data: *mut c_void,
) {
    // SAFETY: `Courier::post` passed this box, which Node.js returns once.
    let job = unsafe { Box::from_raw(data.cast::<Job>()) };
    if env.is_null() {
        env::discard(job);
        return;
    }
    // SAFETY: Node.js calls a thread-safe function's callback on the
    // environment's thread, with a scope open until it returns.
    let env = unsafe { Env::new(env) };
    // A job catches its own panics; this one keeps any other from unwinding
    // into Node.js.
    let _ = crate::runtime::executor::catch(|| job(env));
}
