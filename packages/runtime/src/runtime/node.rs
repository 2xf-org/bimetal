//! The Node.js addon: its module initializer, the classes of its resources,
//! and its async calls, which run on the shared runtime of
//! [`executor`](super::executor).
//!
//! Node.js runs [`register`] in each environment that loads the addon, the
//! main thread's and each worker thread's, and calls every export through
//! one trampoline. A panic throws, or rejects with, `Error("Rust panicked:
//! …")`, and the addon stays usable, as the Python extension does: the
//! futures on other threads keep running.
//!
//! Every `unsafe` call of Node-API is in `env`, `value`, `error`, `class`,
//! `promise`, `callback`, and `courier`.

mod callback;
mod class;
mod courier;
mod env;
mod error;
mod promise;
mod value;

pub(crate) use callback::callback;
pub use class::{Class, ClassDescriptor, Method, Registration, instance, object_of, register};
pub use env::{CallInfo, Callback, Env};
pub use error::Error;
#[doc(hidden)]
pub use napi_sys::{napi_env, napi_value};
pub use promise::{Deferred, awaitable, deferred, unref_awaitable};
pub use value::{Element, Value};
pub(crate) use value::{Gathered, Kind};
