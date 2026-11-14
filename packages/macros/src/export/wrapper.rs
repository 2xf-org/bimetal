//! The planner that composes every host wrapper: it declares the host
//! parameters, decodes them, reaches the resource value, calls Rust, and
//! converts the result. Sync and async calls, free functions, and resource
//! members share each step, and the host's answers (`host_backend`) fill in
//! what differs.

use proc_macro2::TokenStream as TokenStream2;
use quote::quote;

use crate::export::boundary::{self, HostReturnPlan};
use crate::export::host_backend::{AsyncStyle, HostBackend, SyncLocks};
use crate::types::{Param, ParamValue, Signature};

/// How a wrapper reaches the Rust value of a resource.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Access {
    /// A free function, which has no resource value.
    Free,
    /// A constructor, which makes the resource value that the host object
    /// keeps.
    Construct,
    /// A `&self` method, which shares the value with other readers.
    Read,
    /// A `&mut self` method, which holds the value alone.
    Write,
    /// A `self` method, which takes the value and closes the resource.
    Take,
}

/// The parts of one generated wrapper function.
pub(super) struct Wrapper {
    /// The wrapper's parameters, in the types the host passes, each followed
    /// by a comma.
    pub(super) declarations: TokenStream2,
    /// The wrapper's full Rust return type. Async wrappers return the host
    /// awaitable: an asyncio awaitable or a JavaScript promise.
    pub(super) return_ty: TokenStream2,
    /// The wrapper's body.
    pub(super) body: TokenStream2,
}

/// Plan the `backend` wrapper of an exported free function, which calls
/// `callee` and whose errors name it as the host's callers write it.
pub(super) fn plan_free(
    signature: &Signature<'_>,
    backend: HostBackend,
    callee: &TokenStream2,
) -> Wrapper {
    plan(
        signature,
        backend,
        Access::Free,
        callee,
        backend.labels().name(signature),
    )
}

/// Plan the `backend` wrapper of one exported callable, which calls `callee`
/// and reaches the resource value through `access`.
///
/// `callable` is the callable as the host's callers write it, such as `greet`
/// or `Counter.add`, which its errors name.
///
/// A synchronous call holds the resource lock until its result has converted.
/// An async call decodes its arguments and takes the lock when the host makes
/// the call, or waits for the lock in its future if another call holds it,
/// and keeps it as [`async_call`] says.
pub(super) fn plan(
    signature: &Signature<'_>,
    backend: HostBackend,
    access: Access,
    callee: &TokenStream2,
    callable: &str,
) -> Wrapper {
    let params: Vec<_> = signature
        .params
        .iter()
        .map(|param| boundary::wrapper_param(param, backend, signature.is_async, callable))
        .collect();
    let declarations = params.iter().map(|param| &param.declaration);
    let decodes = params.iter().map(|param| &param.decode);
    let locks = params.iter().map(|param| &param.lock);
    let waits: TokenStream2 = params.iter().map(|param| param.wait.clone()).collect();
    let calls = signature.params.iter().map(|param| param.call());
    let invocation = quote!(#callee(#(#calls),*));
    let returns = if access == Access::Construct {
        boundary::constructor_return_plan(&signature.returns, backend)
    } else {
        boundary::return_plan(&signature.returns, backend, callable)
    };
    let reached = reached_resources(signature, access);
    let error_ty = backend.error_ty();
    let (return_ty, call) = if signature.is_async {
        let awaitable = backend.host_value().result();
        // Before its first wait, a call on the caller's thread that waited
        // for the host's event loop would wait for that thread itself.
        let start = if signature.params.iter().any(Param::waits_for_host) {
            quote!(Runtime)
        } else {
            // Each export's pace decides where its calls start.
            quote!(Caller({
                static __BIMETAL_PACE: ::bimetal::runtime::executor::Pace =
                    ::bimetal::runtime::executor::Pace::new();
                &__BIMETAL_PACE
            }))
        };
        (
            quote!(::std::result::Result<#awaitable, #error_ty>),
            async_call(
                backend,
                access,
                &start,
                &waits,
                &invocation,
                &returns,
                callable,
            ),
        )
    } else {
        let ty = &returns.ty;
        (
            quote!(::std::result::Result<#ty, #error_ty>),
            sync_call(
                backend,
                access,
                signature.returns.crossing.as_ref(),
                signature.brief,
                &invocation,
                &returns.body,
                callable,
            ),
        )
    };
    Wrapper {
        declarations: quote!(#(#declarations,)*),
        return_ty,
        body: quote! {
            #reached
            #(#decodes)*
            #(#locks)*
            #call
        },
    }
}

/// Start the record of the resources that the call reaches, with the
/// receiver's, when an argument may be a resource. The record is a few words
/// on the stack, which the optimizer removes when no argument is one.
fn reached_resources(signature: &Signature<'_>, access: Access) -> TokenStream2 {
    let takes_resources = signature.params.iter().any(|param| {
        matches!(
            param.value,
            ParamValue::Value(_) | ParamValue::Resource { .. }
        )
    });
    if !takes_resources {
        return TokenStream2::new();
    }
    let reached = quote!(::bimetal::runtime::ReachedResources);
    let start = match access {
        Access::Read | Access::Write | Access::Take => quote!(#reached::with_receiver(&self.inner)),
        Access::Free | Access::Construct => quote!(#reached::default()),
    };
    quote!(let mut __bimetal_reached = #start;)
}

/// Reach the resource value, call Rust, and convert the result.
///
/// On a host whose async calls run on the shared executor, a call that makes
/// a resource, a constructor, a factory, or one whose result's `crossing`
/// is a resource's, enters its runtime, so that the resource can spawn tasks.
/// Other calls do not, so a package without resources or async calls never
/// starts the runtime's threads.
fn sync_call(
    backend: HostBackend,
    access: Access,
    crossing: Option<&TokenStream2>,
    brief: bool,
    invocation: &TokenStream2,
    result_body: &TokenStream2,
    callable: &str,
) -> TokenStream2 {
    let convert = backend.convert();
    let context = backend.context();
    let shared = quote!(__bimetal_shared);
    let guard = match access {
        Access::Free | Access::Construct | Access::Take => None,
        Access::Read => Some(boundary::sync_lock(backend, &shared, false, callable)),
        Access::Write => Some(boundary::sync_lock(backend, &shared, true, callable)),
    };
    let lock = guard.map(|guard| {
        let inner = bind_inner(access, &guard);
        quote! {
            let __bimetal_shared = self.inner.get(#callable).map_err(|__bimetal_error| {
                #convert::runtime_error(&__bimetal_error)
            })?;
            #inner
        }
    });
    let take = (access == Access::Take).then(|| take_inner(backend, callable));
    // Decoded arguments are owned, so a detached call lets other host threads,
    // such as an event loop, run. A brief call keeps the interpreter lock instead.
    let call = match backend.sync_locks() {
        SyncLocks::Detach if !brief => quote!(#context.detach(|| #invocation)),
        SyncLocks::Detach | SyncLocks::Try => invocation.clone(),
    };
    let enter = quote!(::bimetal::runtime::executor::enter);
    let runtime = match (backend.async_style(), crossing) {
        (AsyncStyle::Promise, _) => TokenStream2::new(),
        (AsyncStyle::Awaitable { .. }, _) if access == Access::Construct => {
            quote!(let __bimetal_runtime = #enter();)
        }
        (AsyncStyle::Awaitable { .. }, Some(crossing)) => quote! {
            let __bimetal_runtime =
                <#crossing as ::bimetal::crossing::Crossing>::RESOURCE.then(#enter);
        },
        (AsyncStyle::Awaitable { .. }, None) => TokenStream2::new(),
    };
    quote! {
        #lock
        #take
        let __bimetal_result = {
            #runtime
            #call
        };
        #result_body
    }
}

/// Take the resource lock, start the Rust future, and convert its result:
/// inside the future for a promise, and for an awaitable, whose first poll is
/// where the `executor::Start` variant `start` says, in the deferred
/// conversion on the host's thread.
///
/// On a host whose synchronous calls fail on a busy lock, the lock holds until
/// the result has converted. On one whose synchronous calls wait for it, the
/// lock goes when the Rust call returns, because the conversion runs on the
/// thread that such a call would block, and reads only the call's owned result.
fn async_call(
    backend: HostBackend,
    access: Access,
    start: &TokenStream2,
    waits: &TokenStream2,
    invocation: &TokenStream2,
    returns: &HostReturnPlan,
    callable: &str,
) -> TokenStream2 {
    let convert = backend.convert();
    let take = (access == Access::Take).then(|| take_inner(backend, callable));
    let lock = match access {
        Access::Free | Access::Construct | Access::Take => None,
        Access::Read => Some(quote!(read_owned)),
        Access::Write => Some(quote!(write_owned)),
    }
    .map(|method| {
        quote! {
            let __bimetal_lock = self.inner.#method(#callable).map_err(|__bimetal_error| {
                #convert::runtime_error(&__bimetal_error)
            })?;
        }
    });
    let release = lock
        .is_some()
        .then(|| quote!(::core::mem::drop(__bimetal_guard);));
    let (released, converted_then_released) = match backend.sync_locks() {
        SyncLocks::Detach => (release, None),
        SyncLocks::Try => (None, release),
    };
    let inner = bind_inner(access, &quote!(__bimetal_lock.await));
    let result_body = &returns.body;
    let converted = quote! {
        let __bimetal_output = { #result_body };
        #converted_then_released
        __bimetal_output
    };
    match backend.async_style() {
        AsyncStyle::Awaitable { runtime } => {
            let context = backend.context();
            quote! {
                #lock
                #take
                #runtime::awaitable(#context, ::bimetal::runtime::executor::Start::#start, async move {
                    #waits
                    #inner
                    let __bimetal_result = #invocation.await;
                    #released
                    #[allow(unused_variables)]
                    let __bimetal_output = #runtime::deferred(move |#context| { #converted });
                    Ok(__bimetal_output)
                })
            }
        }
        AsyncStyle::Promise => {
            let ty = &returns.ty;
            let error_ty = backend.error_ty();
            quote! {
                #lock
                #take
                Ok(#convert::promise(async move {
                    #waits
                    #inner
                    let __bimetal_result = #invocation.await;
                    #released
                    let __bimetal_output: ::std::result::Result<#ty, #error_ty> = { #converted };
                    __bimetal_output.map(::core::convert::Into::into)
                }))
            }
        }
    }
}

/// Bind `__bimetal_inner` to the resource value itself, which closes the
/// resource, for a `self` method named `callable`.
fn take_inner(backend: HostBackend, callable: &str) -> TokenStream2 {
    let convert = backend.convert();
    quote! {
        let __bimetal_inner = self.inner.take(#callable).map_err(|__bimetal_error| {
            #convert::runtime_error(&__bimetal_error)
        })?;
    }
}

/// Bind `__bimetal_inner` to the resource value that `guard` locks, as
/// `access` reaches it; nothing for a call without a resource value, or for a
/// `self` method, which holds it already.
fn bind_inner(access: Access, guard: &TokenStream2) -> TokenStream2 {
    match access {
        Access::Free | Access::Construct | Access::Take => TokenStream2::new(),
        Access::Read => quote! {
            let __bimetal_guard = #guard;
            let __bimetal_inner = &*__bimetal_guard;
        },
        Access::Write => quote! {
            let mut __bimetal_guard = #guard;
            let __bimetal_inner = &mut *__bimetal_guard;
        },
    }
}

/// Check, in every native build, that an async export's future is `Send`, so
/// a missing bound is reported where the export is declared rather than when
/// the Python extension or the Node.js addon compiles.
pub(super) fn send_check(signature: &syn::Signature, callee: &TokenStream2) -> TokenStream2 {
    if signature.asyncness.is_none() {
        return TokenStream2::new();
    }
    let arguments = signature
        .inputs
        .iter()
        .map(|_| quote!(::core::unreachable!()));
    quote! {
        #[cfg(not(target_arch = "wasm32"))]
        const _: () = {
            #[allow(dead_code, unreachable_code, clippy::diverging_sub_expression)]
            fn __bimetal_assert_send() {
                fn __bimetal_send<F: ::core::marker::Send>(_: F) {}
                __bimetal_send(#callee(#(#arguments),*));
            }
        };
    }
}
