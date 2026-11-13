//! Define one Rust API and expose it as a native Python package and a
//! TypeScript package that runs it as WebAssembly or as a Node.js addon.
//!
//! The crate separates four responsibilities:
//!
//! - [`ir`] is the serialized, host-neutral contract.
//! - `registry` discovers and validates linked exports, in native builds.
//! - [`convert`] converts values at the Python, Node.js, and Wasm boundaries.
//! - [`runtime`] holds what generated wrappers share at run time: typed
//!   errors, resource state, field rules, the Tokio runtime that native hosts
//!   run calls on, and the registration and async calls of the Python
//!   extension and the Node.js addon.
//!
//! Application authors normally interact with the [`Model`] and [`Error`]
//! derives and the [`export`] attribute. The CLI consumes the discovery ABI
//! emitted by those macros, and its link crate calls the [`application`]
//! macro; its implementation details are re-exported through [`__private`]
//! solely for generated code.
//!
//! The native Python and Node.js boundaries compile only with the non-default
//! `python` and `node` cargo features. The build CLI enables them for the
//! generated link crate, so application crates and their test binaries never
//! link libpython or need Node.js.

#![deny(missing_docs, rustdoc::broken_intra_doc_links)]
#![forbid(unsafe_op_in_unsafe_fn)]

// Each Arrow feature names its version's crates the same way, so the rest of
// the runtime is one code path.
#[cfg(all(feature = "arrow", feature = "arrow-59"))]
compile_error!(
    "the `arrow` and `arrow-59` features of `bimetal` each choose a version of arrow-rs; enable one"
);
#[cfg(all(feature = "arrow-59", not(feature = "arrow")))]
extern crate arrow_array_59 as arrow_array;
#[cfg(all(feature = "arrow-59", not(feature = "arrow")))]
extern crate arrow_buffer_59 as arrow_buffer;
#[cfg(all(feature = "arrow-59", not(feature = "arrow")))]
extern crate arrow_ipc_59 as arrow_ipc;
#[cfg(all(feature = "arrow-59", not(feature = "arrow")))]
extern crate arrow_schema_59 as arrow_schema;
#[cfg(all(feature = "arrow-59", not(feature = "arrow")))]
extern crate arrow_select_59 as arrow_select;

#[cfg(feature = "arrow-api")]
pub mod arrow;
mod contract_type;
pub mod convert;
#[cfg(any(
    target_arch = "wasm32",
    all(any(feature = "python", feature = "node"), not(target_arch = "wasm32"))
))]
mod owned;
#[cfg(not(target_arch = "wasm32"))]
pub mod registry;
pub mod runtime;

/// Without the `arrow` feature, no type is an Arrow value, and the compiler
/// says which feature `#[bimetal(arrow)]` needs.
#[cfg(not(feature = "arrow-api"))]
#[doc(hidden)]
pub mod arrow {
    use crate::ir::ArrowKind;

    /// A Rust value that an Arrow boundary can carry.
    #[diagnostic::on_unimplemented(
        message = "`#[bimetal(arrow)]` needs the `arrow` feature of `bimetal`",
        note = "add `features = [\"arrow\"]` to the `bimetal` dependency"
    )]
    pub trait ArrowValue {
        /// The host value that this Rust value becomes.
        const KIND: ArrowKind;
    }

    /// Declare uninhabited stand-ins for the feature's types, so that
    /// `use bimetal::arrow::RecordBatch` resolves and the compiler reaches the
    /// `ArrowValue` message above instead of an unresolved import. Each is
    /// deprecated, so that the compiler says which feature to enable at every
    /// use, before it reports the methods that a stand-in lacks.
    macro_rules! stand_ins {
        ($($name:ident),+) => {
            $(
                #[doc = concat!("Stands in for `", stringify!($name), "`; enable the `arrow` feature.")]
                #[deprecated(
                    note = "enable the `arrow` feature of `bimetal`: add `features = [\"arrow\"]` to the `bimetal` dependency"
                )]
                pub enum $name {}
            )+
        };
    }

    stand_ins!(
        ArrayRef,
        ArrowError,
        BlockingBatches,
        DataType,
        Field,
        RecordBatch,
        RecordBatchStream,
        Schema,
        SchemaRef,
        Table
    );
}

pub use bimetal_macros::{Error, Model, application, export};
pub use contract_type::{ContractType, crossing};
pub use runtime::{Callback, CallbackError};

/// Serialized, host-neutral contract shared with the build CLI.
pub use bimetal_ir as ir;

/// Implementation dependencies re-exported for macro-generated code.
///
/// This module is not a stable user-facing API.
#[doc(hidden)]
pub mod __private {
    pub use bimetal_ir::discovery::{
        DiscoveryResult, contract as discovery_contract, free as discovery_free,
    };
    #[cfg(not(target_arch = "wasm32"))]
    pub use inventory;
    pub use serde_json;

    #[cfg(all(feature = "python", not(target_arch = "wasm32")))]
    pub use pyo3;
    #[cfg(target_arch = "wasm32")]
    pub use wasm_bindgen;
}

/// Expand generated Python-host items only in native `python` builds.
///
/// The definition is selected by this crate's own feature resolution, so code
/// emitted into application crates by the Bimetal macros compiles out of every
/// build that does not enable the `python` feature. The applications
/// themselves declare no feature and see no cfg.
#[cfg(all(feature = "python", not(target_arch = "wasm32")))]
#[doc(hidden)]
#[macro_export]
macro_rules! __python_items {
    ($($item:tt)*) => { $($item)* };
}

/// Discard generated Python-host items outside native `python` builds.
#[cfg(not(all(feature = "python", not(target_arch = "wasm32"))))]
#[doc(hidden)]
#[macro_export]
macro_rules! __python_items {
    ($($item:tt)*) => {};
}

/// Expand generated Node.js-host items only in native `node` builds, as
/// [`__python_items!`] does for Python.
#[cfg(all(feature = "node", not(target_arch = "wasm32")))]
#[doc(hidden)]
#[macro_export]
macro_rules! __node_items {
    ($($item:tt)*) => { $($item)* };
}

/// Discard generated Node.js-host items outside native `node` builds.
#[cfg(not(all(feature = "node", not(target_arch = "wasm32"))))]
#[doc(hidden)]
#[macro_export]
macro_rules! __node_items {
    ($($item:tt)*) => {};
}
