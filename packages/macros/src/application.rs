//! Expansion of the application-level discovery and host-module entry points.
//!
//! The generated discovery ABI never unwinds and pairs every owned payload
//! with an explicit free function. Native builds with the `python` feature also
//! expose the Python module, and those with the `node` feature the Node-API
//! module initializer, while Wasm builds expose the registration of a panic
//! listener and a start function that runs constructors once and reports
//! panics. The Wasm module carries no contract: discovery reads it natively.

use bimetal_ir::{discovery, python_names};
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{Ident, Token};

/// Optional crate identifiers linked into the generated link crate.
pub(super) struct ModuleInput {
    /// Crates that must be linked so inventory can discover their exports.
    crates: Vec<Ident>,
}

impl Parse for ModuleInput {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        Ok(Self {
            crates: Punctuated::<Ident, Token![,]>::parse_terminated(input)?
                .into_iter()
                .collect(),
        })
    }
}

/// Emit the native discovery entry points, the Python module, the Node-API
/// module initializer, and the Wasm panic-listener export and start function.
pub(super) fn expand_application(input: ModuleInput) -> TokenStream2 {
    let crates = input.crates;
    let native_module = python_names::NATIVE_MODULE;
    let contract_symbol = discovery::CONTRACT_SYMBOL_PREFIX;
    let free_symbol = discovery::FREE_SYMBOL_PREFIX;
    let name_variable = discovery::APPLICATION_NAME_VARIABLE;
    let version_variable = discovery::APPLICATION_VERSION_VARIABLE;
    quote! {
        #(extern crate #crates as _;)*

        #[cfg(target_arch = "wasm32")]
        use ::bimetal::__private::wasm_bindgen;

        // The application's name and version, which `bimetal build` sets, or
        // else the Cargo package's.
        #[cfg(not(target_arch = "wasm32"))]
        const __BIMETAL_APPLICATION_NAME: &str = match option_env!(#name_variable) {
            ::core::option::Option::Some(name) => name,
            ::core::option::Option::None => env!("CARGO_PKG_NAME"),
        };
        #[cfg(not(target_arch = "wasm32"))]
        const __BIMETAL_APPLICATION_VERSION: &str = match option_env!(#version_variable) {
            ::core::option::Option::Some(version) => version,
            ::core::option::Option::None => env!("CARGO_PKG_VERSION"),
        };

        #[cfg(not(target_arch = "wasm32"))]
        #[unsafe(export_name = concat!(#contract_symbol, env!("CARGO_PKG_NAME")))]
        pub extern "C" fn bimetal_contract() -> ::bimetal::__private::DiscoveryResult {
            ::bimetal::__private::discovery_contract(|| {
                ::bimetal::registry::contract_json(
                    __BIMETAL_APPLICATION_NAME,
                    __BIMETAL_APPLICATION_VERSION,
                )
                .map_err(|__bimetal_error| __bimetal_error.to_string())
            })
        }

        #[cfg(not(target_arch = "wasm32"))]
        #[unsafe(export_name = concat!(#free_symbol, env!("CARGO_PKG_NAME")))]
        pub unsafe extern "C" fn bimetal_contract_free(pointer: *mut ::std::ffi::c_char) {
            unsafe { ::bimetal::__private::discovery_free(pointer) }
        }

        ::bimetal::__python_items! {
            #[::bimetal::__private::pyo3::pymodule]
            #[pyo3(crate = "::bimetal::__private::pyo3", name = #native_module)]
            fn native(
                __bimetal_module: &::bimetal::__private::pyo3::Bound<'_, ::bimetal::__private::pyo3::types::PyModule>,
            ) -> ::bimetal::__private::pyo3::PyResult<()> {
                ::bimetal::runtime::python::register(
                    __bimetal_module,
                    __BIMETAL_APPLICATION_NAME,
                    __BIMETAL_APPLICATION_VERSION,
                )
            }
        }

        // Node.js runs the initializer once in each environment that loads the
        // addon.
        ::bimetal::__node_items! {
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn napi_register_module_v1(
                __bimetal_env: ::bimetal::runtime::node::napi_env,
                __bimetal_exports: ::bimetal::runtime::node::napi_value,
            ) -> ::bimetal::runtime::node::napi_value {
                // SAFETY: Node.js calls the module initializer on the
                // environment's thread, with the environment and its exports.
                unsafe {
                    ::bimetal::runtime::node::register(
                        __bimetal_env,
                        __bimetal_exports,
                        __BIMETAL_APPLICATION_NAME,
                        __BIMETAL_APPLICATION_VERSION,
                    )
                }
            }
        }

        // The generated JavaScript registers its panic listener through this.
        #[cfg(target_arch = "wasm32")]
        #[::bimetal::__private::wasm_bindgen::prelude::wasm_bindgen(
            wasm_bindgen = ::bimetal::__private::wasm_bindgen
        )]
        pub fn bimetal_on_panic(listener: ::bimetal::__private::wasm_bindgen::JsValue) {
            ::bimetal::convert::wasm::on_panic(listener);
        }

        // A dependency may declare linker constructors. Without an explicit
        // call, wasm-ld treats the module as command-style and reruns every
        // constructor at the start of every export, including the glue's
        // allocator calls. Running them once at instantiation gives the
        // module reactor-style linkage.
        #[cfg(target_arch = "wasm32")]
        #[::bimetal::__private::wasm_bindgen::prelude::wasm_bindgen(
            start,
            private,
            wasm_bindgen = ::bimetal::__private::wasm_bindgen
        )]
        fn bimetal_initialize() {
            unsafe extern "C" {
                fn __wasm_call_ctors();
            }
            ::bimetal::convert::wasm::record_panics();
            // SAFETY: wasm-bindgen invokes the start function exactly once,
            // immediately after instantiation and before any other export.
            unsafe { __wasm_call_ctors() }
        }
    }
}
