//! How parameters and return values cross into each host: as their types'
//! crossings say, through Serde or as resources, or directly as Arrow values,
//! bytes, and numeric buffers.

use proc_macro2::TokenStream as TokenStream2;
use quote::quote;

use crate::export::host_backend::{HostBackend, HostValue, Instance, SyncLocks};
use crate::types::{DirectType, Param, ParamValue, ResourceShape, Returns};

/// One host-wrapper parameter.
pub(super) struct WrapperParam {
    /// The parameter as the wrapper declares it, in the host's type.
    pub(super) declaration: TokenStream2,
    /// Statements that rebind the parameter to its owned Rust value; empty
    /// when the host already passes that value.
    pub(super) decode: TokenStream2,
    /// Statements that lock a resource argument for a synchronous call,
    /// after `decode`; empty for every other parameter.
    pub(super) lock: TokenStream2,
    /// Statements that lock a resource argument inside an async call's
    /// future, after `decode`; empty for every other parameter.
    pub(super) wait: TokenStream2,
}

/// Plan how `param` crosses from `backend` into Rust. Its argument errors name
/// `callable`, as the host's callers write it, and the parameter, and an
/// argument that crosses through Serde is checked against the field rules
/// inside it.
pub(super) fn wrapper_param(
    param: &Param<'_>,
    backend: HostBackend,
    is_async: bool,
    callable: &str,
) -> WrapperParam {
    let ident = param.ident;
    let convert = backend.convert();
    let name = backend.labels().parameter(param);
    let parameter = quote!(&#convert::Parameter {
        callable: #callable,
        name: #name,
    });
    let host_value = backend.host_value();
    let byte_vector = quote!(#ident: ::std::vec::Vec<::core::primitive::u8>);
    let (declaration, decode) = match (&param.value, &host_value) {
        (ParamValue::Value(ty), _) => {
            return value_param(param, ty, backend, is_async, callable, &parameter);
        }
        (ParamValue::Resource { value, shape }, _) => {
            return resource_param(
                param, value, *shape, backend, is_async, callable, &parameter,
            );
        }
        (ParamValue::Callback(ty), host_value) if matches!(backend, HostBackend::Python) => {
            let argument = host_value.argument();
            (
                quote!(#ident: #argument),
                quote! {
                    let #ident: #ty = {
                        static __BIMETAL_PLANS: #convert::CallbackPlans =
                            #convert::CallbackPlans::new();
                        #convert::callback_from_host(#ident, &__BIMETAL_PLANS, #parameter)?
                    };
                },
            )
        }
        (ParamValue::Callback(ty), host_value) => {
            let argument = host_value.argument();
            (
                quote!(#ident: #argument),
                quote!(let #ident: #ty = #convert::callback_from_host(#ident, #parameter)?;),
            )
        }
        (ParamValue::Direct(direct), HostValue::Object { argument, .. }) => {
            let decode = match direct {
                DirectType::Arrow { value } => {
                    quote!(let #ident: #value = #convert::arrow_from_host(#ident, #parameter)?;)
                }
                DirectType::FixedBytes { length } => quote! {
                    let #ident: [::core::primitive::u8; #length] =
                        #convert::fixed_bytes_from_host::<{ #length }>(#ident, #parameter)?;
                },
                DirectType::Bytes => {
                    quote!(let #ident = #convert::bytes_from_host(#ident, #parameter)?;)
                }
                DirectType::Buffer { element, .. } => quote! {
                    let #ident = #convert::buffer_from_host::<#element>(#ident, #parameter)?;
                },
            };
            (quote!(#ident: #argument), decode)
        }
        (ParamValue::Direct(direct), HostValue::Bindgen) => match direct {
            // An Arrow value arrives as an IPC stream in a `Uint8Array`, or a
            // stream as the object that the generated JavaScript made.
            DirectType::Arrow { value } => (
                quote!(#ident: ::bimetal::__private::wasm_bindgen::JsValue),
                quote!(let #ident: #value = #convert::arrow_from_host(#ident, #parameter)?;),
            ),
            DirectType::FixedBytes { length } => (
                byte_vector,
                quote! {
                    let #ident: [::core::primitive::u8; #length] =
                        #convert::fixed_bytes_from_host(#ident, #parameter)?;
                },
            ),
            // wasm-bindgen copies a slice argument into an owned box either
            // way, so taking the vector costs nothing and lets an async call
            // keep it.
            DirectType::Bytes => (byte_vector, TokenStream2::new()),
            DirectType::Buffer { element, .. } => (
                quote!(#ident: ::std::vec::Vec<#element>),
                TokenStream2::new(),
            ),
        },
    };
    WrapperParam {
        declaration,
        decode,
        lock: TokenStream2::new(),
        wait: TokenStream2::new(),
    }
}

/// Plan how an argument of type `ty`, or `&ty` when the call borrows it,
/// crosses from `backend` as its type's crossing says: through Serde, or as a
/// resource, which a synchronous call locks after every argument has decoded,
/// and an async call in its future. The host's `Lend` and `Take` implement
/// each crossing.
fn value_param(
    param: &Param<'_>,
    ty: &syn::Type,
    backend: HostBackend,
    is_async: bool,
    callable: &str,
    parameter: &TokenStream2,
) -> WrapperParam {
    let ident = param.ident;
    let convert = backend.convert();
    let argument = backend.host_value().argument();
    let crossing = quote!(<#ty as ::bimetal::ContractType>::Crossing);
    // Python reads a value that crosses through Serde by a plan of its
    // contract type, built once at each call site.
    let (plan, arguments) = match backend {
        HostBackend::Python => (
            quote!(static __BIMETAL_PLAN: #convert::ValuePlan = #convert::ValuePlan::new();),
            quote!(#ident, &__BIMETAL_PLAN, #parameter, &mut __bimetal_reached),
        ),
        HostBackend::Node | HostBackend::Wasm => (
            TokenStream2::new(),
            quote!(#ident, #parameter, &mut __bimetal_reached),
        ),
    };
    let lend = quote!(<#crossing as #convert::Lend<#ty>>);
    let (decode, lock, wait) = match (param.borrowed, is_async) {
        (false, _) => {
            let take = quote!(<#crossing as #convert::Take<#ty>>);
            (
                quote!(#take::decode),
                TokenStream2::new(),
                TokenStream2::new(),
            )
        }
        (true, false) => {
            let context = lock_context(backend);
            // The shadowed binding keeps the value alive under the guard.
            let lock = quote!(let #ident = #lend::lock(#context &#ident, #callable)?;);
            (quote!(#lend::decode), lock, TokenStream2::new())
        }
        (true, true) => (
            quote!(#lend::decode_async),
            TokenStream2::new(),
            quote!(let #ident = #ident.await;),
        ),
    };
    WrapperParam {
        declaration: quote!(#ident: #argument),
        decode: quote! {
            let #ident = {
                #plan
                #decode(#arguments)?
            };
        },
        lock,
        wait,
    }
}

/// Plan how an argument that only a resource can be crosses from `backend`:
/// its host object gives up the shared value, which a synchronous call locks
/// as `shape` says after every argument has decoded, and an async call in its
/// future.
///
/// A call that reaches one resource twice fails rather than waits for itself.
#[allow(
    clippy::too_many_arguments,
    reason = "the planner's context, spelled out once for this one kind of parameter"
)]
fn resource_param(
    param: &Param<'_>,
    value: &syn::Type,
    shape: ResourceShape,
    backend: HostBackend,
    is_async: bool,
    callable: &str,
    parameter: &TokenStream2,
) -> WrapperParam {
    let ident = param.ident;
    let convert = backend.convert();
    let argument = backend.host_value().argument();
    let exclusive = shape == ResourceShape::Exclusive;
    let reach = match (exclusive, is_async) {
        (true, true) => quote!(write_owned),
        (false, true) => quote!(read_owned),
        (_, false) => quote!(get),
    };
    let from_host = match shape {
        ResourceShape::Exclusive => quote!(resource_from_host),
        ResourceShape::Optional => quote!(optional_resource_from_host),
    };
    let decode = quote! {
        let #ident = #convert::#from_host(
            #ident,
            #parameter,
            &mut __bimetal_reached,
            ::bimetal::runtime::Resource::<#value>::#reach,
        )?;
    };
    let (lock, wait) = match (shape, is_async) {
        // The shadowed binding keeps the value alive under the guard.
        (ResourceShape::Exclusive, false) => {
            let held = sync_lock(backend, &quote!(#ident), true, callable);
            (quote!(let mut #ident = #held;), TokenStream2::new())
        }
        (ResourceShape::Optional, false) => {
            let held = sync_lock(backend, &quote!(__bimetal_shared), false, callable);
            let lock = quote! {
                let #ident = match &#ident {
                    ::core::option::Option::Some(__bimetal_shared) => {
                        ::core::option::Option::Some(#held)
                    }
                    ::core::option::Option::None => ::core::option::Option::None,
                };
            };
            (lock, TokenStream2::new())
        }
        (ResourceShape::Exclusive, true) => {
            (TokenStream2::new(), quote!(let mut #ident = #ident.await;))
        }
        (ResourceShape::Optional, true) => {
            let wait = quote! {
                let #ident = match #ident {
                    ::core::option::Option::Some(__bimetal_waiting) => {
                        ::core::option::Option::Some(__bimetal_waiting.await)
                    }
                    ::core::option::Option::None => ::core::option::Option::None,
                };
            };
            (TokenStream2::new(), wait)
        }
    };
    WrapperParam {
        declaration: quote!(#ident: #argument),
        decode,
        lock,
        wait,
    }
}

/// The expression that a synchronous call named `callable` locks the value
/// `shared` with, alone if `exclusive`, through the host's `read` or `write`.
/// It evaluates to the guard, or returns the host's error that the resource
/// is busy.
pub(super) fn sync_lock(
    backend: HostBackend,
    shared: &TokenStream2,
    exclusive: bool,
    callable: &str,
) -> TokenStream2 {
    let convert = backend.convert();
    let lock = if exclusive {
        quote!(write)
    } else {
        quote!(read)
    };
    let context = lock_context(backend);
    quote!(#convert::#lock(#context &#shared, #callable)?)
}

/// The host context that a synchronous lock takes first, followed by a
/// comma: Python's interpreter token, which a busy lock releases while it
/// waits. Hosts that cannot wait take none.
fn lock_context(backend: HostBackend) -> Option<TokenStream2> {
    matches!(backend.sync_locks(), SyncLocks::Detach).then(|| {
        let context = backend.context();
        quote!(#context,)
    })
}

/// Generated wrapper return type and the statements that produce it from
/// `__bimetal_result`, the value the Rust call returned.
pub(super) struct HostReturnPlan {
    /// The host type of a successful synchronous return.
    pub(super) ty: TokenStream2,
    /// Statements that evaluate to `Ok` with a value of `ty`, or to the
    /// host's error.
    pub(super) body: TokenStream2,
}

/// Plan how a call's result crosses into `backend`. A value that crosses
/// through Serde is checked against the field rules inside it, and a
/// violation names `callable`, as the host's callers write it.
pub(super) fn return_plan(
    returns: &Returns<'_>,
    backend: HostBackend,
    callable: &str,
) -> HostReturnPlan {
    let host_value = backend.host_value();
    let convert = backend.convert();
    let context = backend.context().map(|context| quote!(#context,));
    let (ty, success) = match (&returns.direct, &returns.value) {
        (Some(direct), Some(value)) => {
            let owned = value.owned();
            let reference = value.reference();
            let (ty, success) = match (direct, &host_value) {
                (DirectType::Arrow { .. }, HostValue::Object { result, .. }) => (
                    result.clone(),
                    quote!(#convert::arrow_to_host(#context #owned)),
                ),
                (DirectType::Arrow { .. }, _) => (
                    quote!(::bimetal::__private::wasm_bindgen::JsValue),
                    quote!(#convert::arrow_to_host(#owned)),
                ),
                // Python copies the values once, into its own object.
                (
                    DirectType::Bytes | DirectType::FixedBytes { .. },
                    HostValue::Object { result, .. },
                ) => (
                    result.clone(),
                    quote!(Ok(#convert::bytes_to_host(#context #reference))),
                ),
                (DirectType::Buffer { element, .. }, HostValue::Object { result, .. }) => (
                    result.clone(),
                    quote!(Ok(#convert::buffer_to_host::<#element>(#context #reference))),
                ),
                // wasm-bindgen returns a vector.
                (direct, _) => {
                    let element = direct.element().expect("Arrow values return above");
                    let vector = value.vector();
                    (quote!(::std::vec::Vec<#element>), quote!(Ok(#vector)))
                }
            };
            if returns.optional {
                optional_success(&host_value, &convert, context.as_ref(), &ty, &success)
            } else {
                (ty, success)
            }
        }
        // A value crosses as its type's crossing says, through the host's `Return`;
        // Python's Serde path follows a plan built once at each call site.
        _ => {
            let serde = quote!(::bimetal::crossing::Serde);
            let crossing = returns.crossing.as_ref().unwrap_or(&serde);
            let to_host = quote!(<#crossing as #convert::Return<_>>::to_host);
            let success = match backend {
                HostBackend::Python => quote! {{
                    static __BIMETAL_PLAN: #convert::ValuePlan = #convert::ValuePlan::new();
                    #to_host(#context &__BIMETAL_PLAN, __bimetal_value, #callable)
                }},
                HostBackend::Node | HostBackend::Wasm => {
                    quote!(#to_host(#context __bimetal_value, #callable))
                }
            };
            (host_value.result(), success)
        }
    };
    HostReturnPlan {
        ty,
        body: result_body(returns.error.is_some(), backend, &success),
    }
}

/// Wrap the plan of a direct value in `Option`: `ty` and `success` read
/// `__bimetal_value` as the value inside.
fn optional_success(
    host_value: &HostValue,
    convert: &TokenStream2,
    context: Option<&TokenStream2>,
    ty: &TokenStream2,
    success: &TokenStream2,
) -> (TokenStream2, TokenStream2) {
    match host_value {
        HostValue::Object { .. } => (
            ty.clone(),
            quote! {
                match __bimetal_value {
                    ::core::option::Option::Some(__bimetal_value) => #success,
                    ::core::option::Option::None => #convert::none(#context),
                }
            },
        ),
        HostValue::Bindgen => (
            quote!(::core::option::Option<#ty>),
            quote! {
                match __bimetal_value {
                    ::core::option::Option::Some(__bimetal_value) => {
                        let __bimetal_converted: ::std::result::Result<
                            #ty,
                            ::bimetal::__private::wasm_bindgen::JsValue,
                        > = #success;
                        __bimetal_converted.map(::core::option::Option::Some)
                    }
                    ::core::option::Option::None => {
                        ::std::result::Result::Ok(::core::option::Option::None)
                    }
                }
            },
        ),
    }
}

/// Plan a constructor return, which wraps the new value in the host object.
pub(super) fn constructor_return_plan(
    returns: &Returns<'_>,
    backend: HostBackend,
) -> HostReturnPlan {
    let object = quote!(Self {
        inner: ::bimetal::runtime::Resource::new(__bimetal_value)
    });
    let (ty, success) = match backend.instance() {
        Instance::Class => (quote!(Self), quote!(Ok(#object))),
        Instance::Value { runtime } => {
            let context = backend.context();
            (
                backend.host_value().result(),
                quote!(#runtime::instance(#context, #object)),
            )
        }
    };
    HostReturnPlan {
        ty,
        body: result_body(returns.error.is_some(), backend, &success),
    }
}

/// Bind the Rust call's successful value to `__bimetal_value` and evaluate
/// `success`, translating a typed error first when the call returns `Result`.
fn result_body(fallible: bool, backend: HostBackend, success: &TokenStream2) -> TokenStream2 {
    let convert = backend.convert();
    if fallible {
        quote! {
            match __bimetal_result {
                Ok(__bimetal_value) => #success,
                Err(__bimetal_error) => Err(#convert::contract_error(&__bimetal_error)),
            }
        }
    } else {
        quote! {
            let __bimetal_value = __bimetal_result;
            #success
        }
    }
}
