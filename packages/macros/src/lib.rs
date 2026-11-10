//! Procedural macro implementation for Bimetal contracts.
//!
//! This crate is an implementation detail of `bimetal`; application code
//! should use the macros re-exported by that crate. Expansion is organized by
//! declaration kind: model and error derives produce registry metadata,
//! exports produce host wrappers, and the application macro emits the package
//! discovery entry points.

#![deny(missing_docs, rustdoc::broken_intra_doc_links)]
#![forbid(unsafe_code)]

mod application;
mod attributes;
mod error;
mod export;
mod model;
mod registration;
mod types;

use proc_macro::TokenStream;
use proc_macro2::Span;
use syn::{DeriveInput, Item, parse_macro_input};

use crate::application::ModuleInput;

/// Derive the host-neutral contract for a serializable Rust model.
///
/// The derive supports named structs, string enums, internally and
/// externally tagged enums, and transparent one-field aliases. Unsupported or
/// lossy Serde attributes are rejected during macro expansion.
#[proc_macro_derive(Model, attributes(bimetal, serde))]
pub fn derive_model(input: TokenStream) -> TokenStream {
    model::expand_type(&parse_macro_input!(input as DeriveInput)).into()
}

/// Derive stable host error information for a Rust error.
///
/// Each variant receives a stable snake-case code unless overridden with
/// `#[serde(rename = "...")]`.
#[proc_macro_derive(Error, attributes(bimetal, serde))]
pub fn derive_error(input: TokenStream) -> TokenStream {
    error::expand_error(&parse_macro_input!(input as DeriveInput)).into()
}

/// Export a public function, constant, static, or inherent implementation.
///
/// Function and method wrappers are emitted for Python and the Node.js addon
/// on native targets with the `python` or `node` feature of `bimetal` enabled,
/// and for JavaScript on `wasm32`.
/// Annotated Arrow, byte, and numeric-buffer parameters and returns cross
/// directly rather than through Serde.
#[proc_macro_attribute]
pub fn export(args: TokenStream, input: TokenStream) -> TokenStream {
    if !args.is_empty() {
        return syn::Error::new(Span::call_site(), "`#[bimetal::export]` takes no mode")
            .into_compile_error()
            .into();
    }
    let item = parse_macro_input!(input as Item);
    export::expand_export(item)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Declare the application entry points for all linked Rust packages.
///
/// The link crate that the CLI writes calls this once. An application crate
/// must not: a second call defines each entry point again, which the Linux
/// linker refuses.
///
/// Optional crate identifiers force additional contract crates to be linked
/// into the link crate so their inventory registrations are discoverable.
#[proc_macro]
pub fn application(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as ModuleInput);
    application::expand_application(input).into()
}
