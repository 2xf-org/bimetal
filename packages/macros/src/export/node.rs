//! The Node-API item shells of free functions and resources, which the
//! addon's module initializer defines from `inventory`.
//!
//! Each wrapper keeps the planner's parameter list. A callback beside it
//! unpacks the call's arguments into it in one statement, and for a method
//! reads the Rust object of `this` first.

use bimetal_ir::ResourceDef;
use heck::ToShoutySnakeCase;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{Ident, Type as SynType};

use crate::export::host_backend::HostBackend;
use crate::export::resource::{ResourcePlan, Role};
use crate::export::wrapper::{self, Wrapper};
use crate::types::Signature;

/// Emit the Node-API wrapper and inventory registration for a free function.
pub(super) fn function(
    signature: &Signature<'_>,
    function_ident: &Ident,
    native_name: &str,
) -> TokenStream2 {
    let backend = HostBackend::Node;
    let wrapper_ident = format_ident!("__bimetal_node_{}", function_ident);
    let callback_ident = format_ident!("__bimetal_node_callback_{}", function_ident);
    // The wrapper decodes each argument into a local of the parameter's name,
    // so the call goes through a path that such a local cannot shadow.
    let Wrapper {
        declarations,
        return_ty,
        body,
    } = wrapper::plan_free(signature, backend, &quote!(self::#function_ident));
    let arguments = arguments(signature);
    quote! {
        ::bimetal::__node_items! {
            #[allow(
                clippy::too_many_arguments,
                reason = "the generated Node.js wrapper adds one host-context argument"
            )]
            fn #wrapper_ident<'env>(
                __bimetal_env: ::bimetal::runtime::node::Env<'env>,
                #declarations
            ) -> #return_ty {
                #body
            }

            fn #callback_ident<'env>(
                __bimetal_call: ::bimetal::runtime::node::CallInfo<'env>,
            ) -> ::std::result::Result<
                ::bimetal::runtime::node::Value<'env>,
                ::bimetal::runtime::node::Error,
            > {
                let [#(#arguments),*] = __bimetal_call.arguments()?;
                #wrapper_ident(__bimetal_call.env(), #(#arguments),*)
            }

            ::bimetal::__private::inventory::submit! {
                ::bimetal::runtime::node::Registration::Function(::bimetal::runtime::node::Method {
                    name: #native_name,
                    call: #callback_ident,
                })
            }
        }
    }
}

/// Emit the Node-API class, factories, methods, and registration for a
/// resource.
pub(super) fn resource(
    plan: &ResourcePlan<'_>,
    resource_ty: &SynType,
    resource_name: &str,
    native_name: &str,
) -> TokenStream2 {
    let wrapper_ident = format_ident!("__BimetalNode{}", resource_name);
    let class_ident = format_ident!(
        "__BIMETAL_NODE_CLASS_{}",
        resource_name.to_shouty_snake_case()
    );
    let close = ResourceDef::CLOSE;
    let closed = ResourceDef::CLOSED;
    let mut members = Vec::new();
    let mut constructor = quote!(::core::option::Option::None);
    let mut factories = Vec::new();
    let mut methods = Vec::new();
    for member in plan.members() {
        let ident = member.ident();
        let callback_ident = format_ident!("{}_callback", ident);
        let receiver = member.receiver();
        let Wrapper {
            declarations,
            return_ty,
            body,
        } = member.wrapper(resource_ty, resource_name, HostBackend::Node);
        let arguments = arguments(&member.signature);
        let callee = match member.role {
            Role::Constructor | Role::Factory => quote!(Self::#ident),
            Role::Method(_) => quote!(__bimetal_call.this::<Self>()?.#ident),
        };
        members.push(quote! {
            #[allow(
                clippy::too_many_arguments,
                reason = "the generated Node.js method adds one host-context argument"
            )]
            fn #ident<'env>(
                #receiver
                __bimetal_env: ::bimetal::runtime::node::Env<'env>,
                #declarations
            ) -> #return_ty {
                #body
            }

            fn #callback_ident<'env>(
                __bimetal_call: ::bimetal::runtime::node::CallInfo<'env>,
            ) -> ::std::result::Result<
                ::bimetal::runtime::node::Value<'env>,
                ::bimetal::runtime::node::Error,
            > {
                let [#(#arguments),*] = __bimetal_call.arguments()?;
                #callee(__bimetal_call.env(), #(#arguments),*)
            }
        });
        let host_name = &member.signature.host_name;
        let method = quote! {
            ::bimetal::runtime::node::Method {
                name: #host_name,
                call: #wrapper_ident::#callback_ident,
            }
        };
        match member.role {
            Role::Constructor => {
                constructor = quote!(::core::option::Option::Some(#wrapper_ident::#callback_ident));
            }
            Role::Factory => factories.push(method),
            Role::Method(_) => methods.push(method),
        }
    }

    quote! {
        ::bimetal::__node_items! {
            struct #wrapper_ident {
                inner: ::bimetal::runtime::Resource<#resource_ty>,
            }

            impl #wrapper_ident {
                #(#members)*

                fn __bimetal_close<'env>(
                    __bimetal_call: ::bimetal::runtime::node::CallInfo<'env>,
                ) -> ::std::result::Result<
                    ::bimetal::runtime::node::Value<'env>,
                    ::bimetal::runtime::node::Error,
                > {
                    __bimetal_call.this::<Self>()?.inner.close();
                    ::std::result::Result::Ok(__bimetal_call.env().undefined())
                }

                fn __bimetal_closed<'env>(
                    __bimetal_call: ::bimetal::runtime::node::CallInfo<'env>,
                ) -> ::std::result::Result<
                    ::bimetal::runtime::node::Value<'env>,
                    ::bimetal::runtime::node::Error,
                > {
                    let __bimetal_closed = __bimetal_call.this::<Self>()?.inner.closed();
                    __bimetal_call.env().boolean(__bimetal_closed)
                }
            }

            impl ::bimetal::convert::node::HostResource for #resource_ty {
                fn into_host<'env>(
                    __bimetal_env: ::bimetal::runtime::node::Env<'env>,
                    value: Self,
                ) -> ::std::result::Result<
                    ::bimetal::runtime::node::Value<'env>,
                    ::bimetal::runtime::node::Error,
                > {
                    ::bimetal::runtime::node::instance(
                        __bimetal_env,
                        #wrapper_ident {
                            inner: ::bimetal::runtime::Resource::new(value),
                        },
                    )
                }

                fn with_resource<R>(
                    value: ::bimetal::runtime::node::Value<'_>,
                    reach: impl ::core::ops::FnOnce(&::bimetal::runtime::Resource<Self>) -> R,
                ) -> ::std::result::Result<
                    ::core::option::Option<R>,
                    ::bimetal::runtime::node::Error,
                > {
                    ::std::result::Result::Ok(
                        ::bimetal::runtime::node::object_of::<#wrapper_ident>(value)?
                            .map(|__bimetal_object| reach(&__bimetal_object.inner)),
                    )
                }
            }

            static #class_ident: ::bimetal::runtime::node::ClassDescriptor =
                ::bimetal::runtime::node::ClassDescriptor::new::<#wrapper_ident>(
                    #native_name,
                    #constructor,
                    &[#(#factories),*],
                    &[
                        #(#methods,)*
                        ::bimetal::runtime::node::Method {
                            name: #close,
                            call: #wrapper_ident::__bimetal_close,
                        },
                        ::bimetal::runtime::node::Method {
                            name: #closed,
                            call: #wrapper_ident::__bimetal_closed,
                        },
                    ],
                );

            impl ::bimetal::runtime::node::Class for #wrapper_ident {
                fn descriptor() -> &'static ::bimetal::runtime::node::ClassDescriptor {
                    &#class_ident
                }
            }

            ::bimetal::__private::inventory::submit! {
                ::bimetal::runtime::node::Registration::Class(&#class_ident)
            }
        }
    }
}

/// Return the locals that a callback unpacks the arguments of `signature`'s
/// parameters into.
fn arguments(signature: &Signature<'_>) -> Vec<Ident> {
    (0..signature.params.len())
        .map(|index| format_ident!("__bimetal_argument_{}", index))
        .collect()
}
