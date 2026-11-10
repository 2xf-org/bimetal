//! Node.js conversion primitives used by generated addon wrappers.
//!
//! Values cross as they cross the Wasm module, so one generated JavaScript
//! package serves both: Serde values with serde-wasm-bindgen's conventions
//! (`ser`, `de`, which share the strings of names through `names`), bytes
//! and buffers as plain typed arrays, and Arrow values as IPC streams
//! (`arrow`).
//! Failures are the same JavaScript exceptions: an untyped
//! failure is an `Error`, a `TypeError` when an argument has the wrong shape,
//! or a `RangeError` when it breaks a field rule, and a typed contract error
//! is a plain payload object that the wrapper turns into its error class.

#[cfg(feature = "arrow-api")]
mod arrow;
mod boundary;
mod de;
mod names;
mod ser;

#[cfg(feature = "arrow-api")]
pub use arrow::{arrow_from_host, arrow_to_host};
pub use boundary::*;
