//! The contract registration of every exported declaration, which only
//! native builds compile: discovery reads the contract from them, and the
//! Wasm module carries none.

use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};

/// Submit a definition to the registry through `registration`, the
/// registry's type for the kind of declaration, such as
/// `FunctionRegistration`. `definition` is the body of the function that
/// builds it, which returns `returns`, and may use the fields of [`origin`].
pub(crate) fn submit(
    registration: &str,
    returns: &TokenStream2,
    definition: &TokenStream2,
) -> TokenStream2 {
    let registration = format_ident!("{registration}");
    quote! {
        #[cfg(not(target_arch = "wasm32"))]
        const _: () = {
            fn __bimetal_registration() -> #returns {
                #definition
            }
            ::bimetal::__private::inventory::submit! {
                ::bimetal::registry::#registration(__bimetal_registration)
            }
        };
    }
}

/// Submit the definition of the type `name`, whose identity `id` and whose
/// `docs` and `shape` are expressions. `owner`, an expression, is how a
/// problem with the default of a field names the declaration; `shape` reads
/// it as `__bimetal_owner` and adds such problems to `__bimetal_problems`.
pub(crate) fn submit_type(
    name: &str,
    id: &TokenStream2,
    owner: &TokenStream2,
    docs: &TokenStream2,
    shape: &TokenStream2,
) -> TokenStream2 {
    let origin = origin();
    quote! {
        #[cfg(not(target_arch = "wasm32"))]
        const _: () = {
            fn __bimetal_registration(
                __bimetal_problems: &mut ::std::vec::Vec<::std::string::String>,
            ) -> ::bimetal::ir::TypeDef {
                let __bimetal_owner: ::std::string::String = #owner;
                ::bimetal::ir::TypeDef {
                    #origin
                    id: #id,
                    name: #name.to_owned(),
                    docs: #docs,
                    shape: #shape,
                }
            }
            ::bimetal::__private::inventory::submit! {
                ::bimetal::registry::TypeRegistration(__bimetal_registration)
            }
        };
    }
}

/// The fields of a definition that say where it was declared: its Cargo
/// package and its Rust module.
pub(crate) fn origin() -> TokenStream2 {
    quote! {
        owner: ::bimetal::ir::CargoPackageId::new(env!("CARGO_PKG_NAME")),
        rust_module: module_path!().to_owned(),
    }
}
