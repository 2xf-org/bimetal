//! Expansion of free-function exports: the contract registration of one
//! public function and its host wrappers (`python`, `node`, `wasm`).

use bimetal_ir::ExportKind;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::ItemFn;

use crate::attributes::{self, CallableKind};
use crate::export::{node, python, wasm, wrapper};
use crate::registration;
use crate::types::{self, Signature};

/// Expand one free function into metadata and every host's wrapper.
pub(super) fn expand_function(mut function: ItemFn) -> syn::Result<TokenStream2> {
    types::ensure_public(&function.vis, function.sig.ident.span())?;
    let options = attributes::export_options(&function.attrs, CallableKind::Function)?;
    let signature = Signature::parse(&function.sig, options.returns)?;
    let ident = &function.sig.ident;
    let native_name =
        types::native_export_name(ident.span(), ExportKind::Function, &signature.host_name);
    let docs = attributes::docs_tokens(&function.attrs);
    let callable = signature.callable_tokens(&docs);
    let returns = &signature.returns.type_ref;
    let callee = quote!(#ident);
    let python_wrapper = python::function(&signature, ident, &native_name);
    let node_wrapper = node::function(&signature, ident, &native_name);
    let wasm_wrapper = wasm::function(&signature, ident, &native_name);
    let send_check = wrapper::send_check(&function.sig, &callee);
    attributes::strip_bimetal_attributes(&mut function.attrs, &mut function.sig.inputs);
    let origin = registration::origin();
    let registration = registration::submit(
        "FunctionRegistration",
        &quote!(::bimetal::ir::FunctionDef),
        &quote! {
            ::bimetal::ir::FunctionDef {
                #origin
                native_name: #native_name.to_owned(),
                callable: #callable,
                returns: #returns,
            }
        },
    );
    Ok(quote! {
        #function
        #registration
        #send_check

        #python_wrapper
        #node_wrapper
        #wasm_wrapper
    })
}
