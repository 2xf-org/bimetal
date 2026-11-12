//! The wasm-bindgen item shells of free functions and resources, each in a
//! module that only `wasm32` builds compile.

use bimetal_ir::ResourceDef;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{Ident, Type as SynType};

use crate::export::host_backend::HostBackend;
use crate::export::resource::{ResourcePlan, Role};
use crate::export::wrapper::{self, Wrapper};
use crate::types::Signature;

/// Emit the wasm-bindgen wrapper for a free function.
pub(super) fn function(
    signature: &Signature<'_>,
    function_ident: &Ident,
    native_name: &str,
) -> TokenStream2 {
    let backend = HostBackend::Wasm;
    let wrapper_ident = format_ident!("__bimetal_wasm_{}", function_ident);
    let module_ident = format_ident!("__bimetal_wasm_function_{}", function_ident);
    let Wrapper {
        declarations,
        return_ty,
        body,
    } = wrapper::plan_free(signature, backend, &quote!(super::#function_ident));
    quote! {
        #[cfg(target_arch = "wasm32")]
        mod #module_ident {
            use super::*;
            use ::bimetal::__private::wasm_bindgen::{self, prelude::wasm_bindgen};

            #[doc(hidden)]
            #[allow(missing_docs)]
            #[wasm_bindgen(
                js_name = #native_name,
                wasm_bindgen = ::bimetal::__private::wasm_bindgen
            )]
            pub fn #wrapper_ident(
                #declarations
            ) -> #return_ty {
                #body
            }
        }
    }
}

/// Emit the wasm-bindgen class, factories, and methods for a resource.
pub(super) fn resource(
    plan: &ResourcePlan<'_>,
    resource_ty: &SynType,
    resource_name: &str,
    native_name: &str,
) -> TokenStream2 {
    use heck::ToSnakeCase;

    let wrapper_ident = format_ident!("BimetalWasm{}", resource_name);
    let close = format_ident!("{}", ResourceDef::CLOSE);
    let closed = format_ident!("{}", ResourceDef::CLOSED);
    let module_ident = format_ident!("__bimetal_wasm_resource_{}", resource_name.to_snake_case());
    let class = quote! {
        #[doc(hidden)]
        #[allow(missing_docs)]
        #[wasm_bindgen(
            js_class = #native_name,
            wasm_bindgen = ::bimetal::__private::wasm_bindgen
        )]
    };
    let mut members = Vec::new();
    let mut gated = Vec::new();
    for member in plan.members() {
        let host_name = &member.signature.host_name;
        let host_attributes = match member.role {
            Role::Constructor => quote!(#[wasm_bindgen(constructor)]),
            Role::Factory | Role::Method(_) => quote!(#[wasm_bindgen(js_name = #host_name)]),
        };
        let ident = member.ident();
        let receiver = member.receiver();
        let Wrapper {
            declarations,
            return_ty,
            body,
        } = member.wrapper(resource_ty, resource_name, HostBackend::Wasm);
        let function = quote! {
            #host_attributes
            pub fn #ident(
                #receiver
                #declarations
            ) -> #return_ty {
                #body
            }
        };
        // A gated member has an impl block of its own, which its gate removes
        // whole, so wasm-bindgen never reads a member that the build leaves
        // out.
        if member.gates.is_empty() {
            members.push(function);
        } else {
            let gates = &member.gates;
            gated.push(quote! {
                #(#gates)*
                #class
                impl #wrapper_ident {
                    #function
                }
            });
        }
    }

    quote! {
        #[cfg(target_arch = "wasm32")]
        mod #module_ident {
            use super::*;
            use ::bimetal::__private::wasm_bindgen::{self, prelude::wasm_bindgen};

            #[doc(hidden)]
            #[allow(missing_docs)]
            #[wasm_bindgen(
                js_name = #native_name,
                wasm_bindgen = ::bimetal::__private::wasm_bindgen
            )]
            pub struct #wrapper_ident {
                inner: ::bimetal::runtime::Resource<#resource_ty>,
            }

            #class
            impl #wrapper_ident {
                #(#members)*

                pub fn #close(&self) {
                    self.inner.close();
                }

                pub fn #closed(&self) -> ::core::primitive::bool {
                    self.inner.closed()
                }

                pub fn __bimetal_share(&self) {
                    ::bimetal::convert::wasm::share(&self.inner);
                }
            }

            #(#gated)*

            impl ::bimetal::convert::wasm::HostResource for #resource_ty {
                fn into_host(value: Self) -> ::bimetal::__private::wasm_bindgen::JsValue {
                    #wrapper_ident {
                        inner: ::bimetal::runtime::Resource::new(value),
                    }
                    .into()
                }
            }
        }
    }
}
