//! How parameters and return values cross into each host: through Serde, or
//! directly as Arrow values, bytes, and numeric buffers.

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
        (ParamValue::Serde(owned), host_value) => {
            let argument = host_value.argument();
            (
                quote!(#ident: #argument),
                quote!(let #ident: #owned = #convert::from_host(#ident, #parameter)?;),
            )
        }
        (ParamValue::Direct(DirectType::Resource { value, shape }), _) => {
            return resource_param(
                param, value, *shape, backend, is_async, callable, &parameter,
            );
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
                DirectType::Resource { .. } => unreachable!("resources return above"),
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
            DirectType::Resource { .. } => unreachable!("resources return above"),
        },
    };
    WrapperParam {
        declaration,
        decode,
        lock: TokenStream2::new(),
        wait: TokenStream2::new(),
    }
}

/// Plan how a resource argument crosses from `backend`: its host object gives
/// up the shared value, which the call locks as `shape` says, or the value
/// itself when the call takes it.
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
    let reach = match (shape, is_async) {
        (ResourceShape::Owned | ResourceShape::OptionalOwned | ResourceShape::List, _) => {
            quote!(take)
        }
        (ResourceShape::Exclusive, true) => quote!(write_owned),
        (ResourceShape::Shared | ResourceShape::OptionalShared, true) => quote!(read_owned),
        (_, false) => quote!(get),
    };
    let reached = quote!(|__bimetal_resource: &::bimetal::runtime::Resource<#value>| {
        __bimetal_reached.add(__bimetal_resource, #callable)?;
        __bimetal_resource.#reach(#callable)
    });
    let translate = quote!(|__bimetal_error| #convert::runtime_error(&__bimetal_error));
    let decode = if shape == ResourceShape::OptionalShared {
        quote! {
            let #ident = #convert::optional_resource_from_host(#ident, #parameter, #reached)?
                .transpose()
                .map_err(#translate)?;
        }
    } else {
        quote! {
            let #ident = #convert::resource_from_host(#ident, #parameter, #reached)?
                .map_err(#translate)?;
        }
    };
    let guard = |shared: TokenStream2| sync_lock(backend, &shared, exclusive, callable);
    let mutability = exclusive.then(|| quote!(mut));
    let (lock, wait) = match shape {
        ResourceShape::Owned | ResourceShape::OptionalOwned | ResourceShape::List => {
            (TokenStream2::new(), TokenStream2::new())
        }
        ResourceShape::OptionalShared => {
            let held = guard(quote!(__bimetal_shared));
            let lock = if is_async {
                TokenStream2::new()
            } else {
                // The shadowed binding keeps the value alive under the guard.
                let held = held.clone();
                quote! {
                    let #ident = match &#ident {
                        ::core::option::Option::Some(__bimetal_shared) => {
                            ::core::option::Option::Some(#held)
                        }
                        ::core::option::Option::None => ::core::option::Option::None,
                    };
                }
            };
            (
                lock,
                quote! {
                    let #ident = match #ident {
                        ::core::option::Option::Some(__bimetal_waiting) => {
                            ::core::option::Option::Some(__bimetal_waiting.await)
                        }
                        ::core::option::Option::None => ::core::option::Option::None,
                    };
                },
            )
        }
        ResourceShape::Shared | ResourceShape::Exclusive => {
            let held = guard(quote!(#ident));
            (
                // The shadowed binding keeps the value alive under the guard.
                quote!(let #mutability #ident = #held;),
                quote!(let #mutability #ident = #ident.await;),
            )
        }
    };
    let (lock, wait) = if is_async {
        (TokenStream2::new(), wait)
    } else {
        (lock, TokenStream2::new())
    };
    WrapperParam {
        declaration: quote!(#ident: #argument),
        decode,
        lock,
        wait,
    }
}

/// The expression that a synchronous call named `callable` locks the value
/// `shared` with, alone if `exclusive`. It evaluates to the guard, or returns
/// the host's error that the resource is busy.
pub(super) fn sync_lock(
    backend: HostBackend,
    shared: &TokenStream2,
    exclusive: bool,
    callable: &str,
) -> TokenStream2 {
    let convert = backend.convert();
    let (attempt, wait) = if exclusive {
        (quote!(try_write), quote!(blocking_write))
    } else {
        (quote!(try_read), quote!(blocking_read))
    };
    let busy = quote! {
        #convert::runtime_error(&::bimetal::runtime::ResourceError::Busy(#callable))
    };
    match backend.sync_locks() {
        SyncLocks::Try => quote!(#shared.#attempt().map_err(|_| #busy)?),
        // A free resource is taken with the host's lock held. Only a busy
        // one makes the call release that lock to wait.
        SyncLocks::Detach => {
            let context = backend.context();
            quote! {
                match #shared.#attempt() {
                    Ok(__bimetal_guard) => __bimetal_guard,
                    Err(_) if ::bimetal::runtime::inside_callback() => return Err(#busy),
                    Err(_) => #context.detach(|| {
                        ::bimetal::runtime::executor::allow_blocking(|| #shared.#wait())
                    }),
                }
            }
        }
    }
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
        (Some(DirectType::Resource { .. }), Some(_)) => match &host_value {
            HostValue::Object { result, .. } => (
                result.clone(),
                quote!(#convert::resource_to_host(#context __bimetal_value)),
            ),
            HostValue::Bindgen => (
                quote!(::bimetal::__private::wasm_bindgen::JsValue),
                quote!(::std::result::Result::Ok(#convert::resource_to_host(__bimetal_value))),
            ),
        },
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
        _ => (
            host_value.result(),
            quote!(#convert::to_host(#context &__bimetal_value, #callable)),
        ),
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
