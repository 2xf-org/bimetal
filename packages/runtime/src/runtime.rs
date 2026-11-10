//! Runtime registration, typed errors, and shared resource state.
//!
//! Generated wrappers submit native registrations through `inventory`, while
//! [`ContractError`] supplies the stable identity and code carried to hosts.
//! [`Resource`] holds the Rust value behind every generated resource class,
//! and [`rules`] checks the rules of model fields at the boundary and reports
//! a broken rule as a [`ValueError`]. Native hosts run their calls on the
//! runtime of `executor`.

mod callback;
mod contract_error;
#[cfg(all(any(feature = "python", feature = "node"), not(target_arch = "wasm32")))]
#[doc(hidden)]
pub mod executor;
#[cfg(all(feature = "node", not(target_arch = "wasm32")))]
pub mod node;
#[cfg(all(feature = "python", not(target_arch = "wasm32")))]
pub mod python;
mod resource;
#[doc(hidden)]
pub mod rules;
mod value_error;

pub use callback::{Callback, CallbackError};
#[doc(hidden)]
pub use callback::{CallbackArguments, inside_callback};
pub use contract_error::ContractError;
#[doc(hidden)]
pub use resource::{ReachedResources, Resource, ResourceError, ResourceType};
#[doc(hidden)]
pub use value_error::ValueError;

/// What every host reports for a panic whose payload is not a message, as
/// pyo3 does for a synchronous call.
#[cfg(any(target_arch = "wasm32", feature = "python", feature = "node"))]
pub(crate) const PANIC_WITHOUT_MESSAGE: &str = "panic from Rust code";
