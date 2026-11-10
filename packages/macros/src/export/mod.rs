//! Expansion of callable, resource, and constant exports.
//!
//! Each callable's signature is checked once (`types::Signature`), then
//! rendered four times: as contract metadata, a native Python wrapper
//! (`python`), a Node-API wrapper (`node`), and a wasm-bindgen wrapper
//! (`wasm`). Direct Arrow, byte, and numeric-buffer annotations select
//! specialized conversions (`boundary`); all other values use the shared
//! Serde conversion. One planner (`wrapper`) composes every wrapper from the
//! host's answers (`host_backend`), so sync and async calls, free functions
//! (`function`), and resource members (`resource`) share each step.

mod boundary;
mod constant;
mod function;
mod host_backend;
mod node;
mod python;
mod resource;
mod wasm;
mod wrapper;

use proc_macro2::TokenStream as TokenStream2;
use syn::Item;

/// Dispatch an export attribute to the expander for its kind of declaration.
pub(super) fn expand_export(item: Item) -> syn::Result<TokenStream2> {
    use syn::spanned::Spanned;

    match item {
        Item::Fn(function) => function::expand_function(function),
        Item::Impl(item_impl) => resource::expand_resource(item_impl),
        Item::Const(item) => constant::expand(&item, &item.vis, &item.ident, &item.ty, &item.attrs),
        Item::Static(item) => {
            constant::expand(&item, &item.vis, &item.ident, &item.ty, &item.attrs)
        }
        other => Err(syn::Error::new(
            other.span(),
            "`#[bimetal::export]` supports public functions, inherent impl blocks, consts, and statics",
        )),
    }
}
