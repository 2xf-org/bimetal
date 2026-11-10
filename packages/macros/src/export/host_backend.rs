//! The hosts that wrappers are planned for, and what each answers about the
//! wrapper it needs. The planner (`wrapper`) and the boundary rows
//! (`boundary`) ask these questions instead of matching on the host.

use proc_macro2::TokenStream as TokenStream2;
use quote::quote;

use crate::types::{Param, Signature};

/// Host backend for which wrapper tokens are being planned.
#[derive(Clone, Copy)]
pub(super) enum HostBackend {
    /// The PyO3 extension module, on native targets with the `python` feature.
    Python,
    /// The Node-API addon, on native targets with the `node` feature.
    Node,
    /// The wasm-bindgen module, on `wasm32`.
    Wasm,
}

/// Which names a host's callers write, and its errors use.
#[derive(Clone, Copy)]
pub(super) enum Labels {
    /// Rust's names: `Counter.add_many`, the class `Counter` for its
    /// constructor, and each parameter as the Python wrapper names it.
    Rust,
    /// The contract's host names, which both JavaScript hosts use:
    /// `Counter.addMany`, `new Counter` for the constructor, and each
    /// parameter as the JavaScript wrapper names it.
    Host,
}

/// How a synchronous call reaches a resource value that another call holds.
#[derive(Clone, Copy)]
pub(super) enum SyncLocks {
    /// Wait for it with the host context's lock released through the
    /// context's `detach`, and run the Rust call without that lock too:
    /// Python's interpreter lock. Inside a host function that Rust called, it
    /// reports that the resource is busy instead, because the call that holds
    /// the value may be waiting for that function.
    Detach,
    /// Report that the resource is busy, because the host cannot block.
    Try,
}

/// How an async call returns to the host.
pub(super) enum AsyncStyle {
    /// As the awaitable of `runtime::awaitable`, which runs the future on the
    /// shared executor and converts its result with `runtime::deferred` once
    /// the host context is available. Synchronous constructors enter the
    /// executor's runtime.
    Awaitable {
        /// The runtime module that provides `awaitable` and `deferred`.
        runtime: TokenStream2,
    },
    /// As the promise of `convert()::promise`, whose future converts the
    /// result itself.
    Promise,
}

/// How a constructor's new resource value becomes the host's object.
pub(super) enum Instance {
    /// As the wrapper class itself, `Self`, which the host's class machinery
    /// turns into an object: pyo3's and wasm-bindgen's.
    Class,
    /// As the host value that `runtime::instance` makes of `Self`, so that
    /// constructors return host values as other callables do.
    Value {
        /// The runtime module that provides `instance`.
        runtime: TokenStream2,
    },
}

/// How a host passes values to a wrapper and takes them back.
pub(super) enum HostValue {
    /// As host objects: arguments of type `argument`, which `convert()` reads,
    /// and results of type `result`, which it writes.
    Object {
        /// The type of an argument.
        argument: TokenStream2,
        /// The type of a result, and of an async call's awaitable.
        result: TokenStream2,
    },
    /// As wasm-bindgen's types: `JsValue` for Serde values, promises, and
    /// Arrow values, and vectors for bytes and buffers.
    Bindgen,
}

impl HostBackend {
    /// Path of the runtime conversion module for this backend.
    pub(super) fn convert(self) -> TokenStream2 {
        match self {
            Self::Python => quote!(::bimetal::convert::python),
            Self::Node => quote!(::bimetal::convert::node),
            Self::Wasm => quote!(::bimetal::convert::wasm),
        }
    }

    /// Which names this host's callers write.
    pub(super) fn labels(self) -> Labels {
        match self {
            Self::Python => Labels::Rust,
            Self::Node | Self::Wasm => Labels::Host,
        }
    }

    /// How a synchronous call reaches a locked resource value.
    pub(super) fn sync_locks(self) -> SyncLocks {
        match self {
            Self::Python => SyncLocks::Detach,
            Self::Node | Self::Wasm => SyncLocks::Try,
        }
    }

    /// How an async call returns.
    pub(super) fn async_style(self) -> AsyncStyle {
        match self {
            Self::Python => AsyncStyle::Awaitable {
                runtime: quote!(::bimetal::runtime::python),
            },
            Self::Node => AsyncStyle::Awaitable {
                runtime: node_runtime(),
            },
            Self::Wasm => AsyncStyle::Promise,
        }
    }

    /// How a constructor's new value becomes the host's object.
    pub(super) fn instance(self) -> Instance {
        match self {
            Self::Python | Self::Wasm => Instance::Class,
            Self::Node => Instance::Value {
                runtime: node_runtime(),
            },
        }
    }

    /// How values cross.
    pub(super) fn host_value(self) -> HostValue {
        match self {
            Self::Python => HostValue::Object {
                argument: quote!(
                    &::bimetal::__private::pyo3::Bound<'py, ::bimetal::__private::pyo3::PyAny>
                ),
                result: quote!(::bimetal::__private::pyo3::Py<::bimetal::__private::pyo3::PyAny>),
            },
            Self::Node => HostValue::Object {
                argument: quote!(::bimetal::runtime::node::Value<'env>),
                result: quote!(::bimetal::runtime::node::Value<'env>),
            },
            Self::Wasm => HostValue::Bindgen,
        }
    }

    /// The wrapper's binding of the host context, which conversions into the
    /// host take first: Python's interpreter token, or the Node.js
    /// environment.
    pub(super) fn context(self) -> Option<TokenStream2> {
        match self {
            Self::Python => Some(quote!(__bimetal_py)),
            Self::Node => Some(quote!(__bimetal_env)),
            Self::Wasm => None,
        }
    }

    /// The error type that the wrapper returns.
    pub(super) fn error_ty(self) -> TokenStream2 {
        match self {
            Self::Python => quote!(::bimetal::__private::pyo3::PyErr),
            Self::Node => quote!(::bimetal::runtime::node::Error),
            Self::Wasm => quote!(::bimetal::__private::wasm_bindgen::JsValue),
        }
    }
}

/// The runtime module of the Node.js addon, which provides its awaitables
/// and its instances.
fn node_runtime() -> TokenStream2 {
    quote!(::bimetal::runtime::node)
}

impl Labels {
    /// Return a callable's name.
    pub(super) fn name<'signature>(self, signature: &'signature Signature<'_>) -> &'signature str {
        match self {
            Self::Rust => &signature.rust_name,
            Self::Host => &signature.host_name,
        }
    }

    /// Return a parameter's name.
    pub(super) fn parameter<'param>(self, param: &'param Param<'_>) -> &'param str {
        match self {
            Self::Rust => &param.python_name,
            Self::Host => &param.javascript_name,
        }
    }
}

impl HostValue {
    /// The type of an argument that crosses through Serde.
    pub(super) fn argument(&self) -> TokenStream2 {
        match self {
            Self::Object { argument, .. } => argument.clone(),
            Self::Bindgen => quote!(::bimetal::__private::wasm_bindgen::JsValue),
        }
    }

    /// The type of a result that crosses through Serde, and of an async
    /// call's awaitable or promise.
    pub(super) fn result(&self) -> TokenStream2 {
        match self {
            Self::Object { result, .. } => result.clone(),
            Self::Bindgen => quote!(::bimetal::__private::wasm_bindgen::JsValue),
        }
    }
}
