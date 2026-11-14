//! The PyO3 item shells of free functions and resources, which the Python
//! extension registers through `inventory`.

use bimetal_ir::ResourceDef;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{Ident, Type as SynType};

use crate::export::host_backend::HostBackend;
use crate::export::resource::{ResourcePlan, Role};
use crate::export::wrapper::{self, Wrapper};
use crate::types::Signature;

/// Emit the PyO3 wrapper and inventory registration for a free function,
/// which carries `docs` when it is brief.
pub(super) fn function(
    signature: &Signature<'_>,
    function_ident: &Ident,
    native_name: &str,
    docs: Option<&str>,
) -> TokenStream2 {
    let backend = HostBackend::Python;
    let wrapper_ident = format_ident!("__bimetal_python_{}", function_ident);
    let register_ident = format_ident!("__bimetal_register_python_{}", function_ident);
    // The wrapper decodes each argument into a local of the parameter's name,
    // so the call goes through a path that such a local cannot shadow.
    let Wrapper {
        declarations,
        return_ty,
        body,
    } = wrapper::plan_free(signature, backend, &quote!(self::#function_ident));
    // The generated module may bind a brief function to this one, which then carries
    // the Rust name and docs; the extension module's attribute keeps the native name.
    let (name, docs) = if signature.brief {
        let docs = docs.map(|docs| quote!(#[doc = #docs]));
        (signature.rust_name.as_str(), docs)
    } else {
        (native_name, None)
    };
    quote! {
        ::bimetal::__python_items! {
            #[::bimetal::__private::pyo3::pyfunction(name = #name)]
            #[pyo3(crate = "::bimetal::__private::pyo3")]
            #docs
            #[allow(
                clippy::too_many_arguments,
                reason = "the generated Python wrapper adds one host-context argument"
            )]
            fn #wrapper_ident<'py>(
                __bimetal_py: ::bimetal::__private::pyo3::Python<'py>,
                #declarations
            ) -> #return_ty {
                #body
            }

            fn #register_ident(
                __bimetal_module: &::bimetal::__private::pyo3::Bound<'_, ::bimetal::__private::pyo3::types::PyModule>,
            ) -> ::bimetal::__private::pyo3::PyResult<()> {
                ::bimetal::__private::pyo3::types::PyModuleMethods::add(
                    __bimetal_module,
                    #native_name,
                    ::bimetal::__private::pyo3::wrap_pyfunction!(#wrapper_ident, __bimetal_module)?,
                )?;
                Ok(())
            }

            ::bimetal::__private::inventory::submit! {
                ::bimetal::runtime::python::Registration(#register_ident)
            }
        }
    }
}

/// Emit the PyO3 class, factories, methods, and registration for a resource.
pub(super) fn resource(
    plan: &ResourcePlan<'_>,
    resource_ty: &SynType,
    resource_name: &str,
    native_name: &str,
) -> TokenStream2 {
    let wrapper_ident = format_ident!("__BimetalPython{}", resource_name);
    let close = format_ident!("{}", ResourceDef::CLOSE);
    let closed = format_ident!("{}", ResourceDef::CLOSED);
    let register_ident = format_ident!("__bimetal_register_python_resource_{}", resource_name);
    let members = plan.members().map(|member| {
        let host_name = &member.signature.host_name;
        let host_attributes = match member.role {
            Role::Constructor => quote!(#[new]),
            Role::Factory => quote!(#[staticmethod] #[pyo3(name = #host_name)]),
            Role::Method(_) => quote!(#[pyo3(name = #host_name)]),
        };
        let ident = member.ident();
        let receiver = member.receiver();
        let Wrapper {
            declarations,
            return_ty,
            body,
        } = member.wrapper(resource_ty, resource_name, HostBackend::Python);
        let gates = &member.gates;
        quote! {
            #(#gates)*
            #host_attributes
            #[allow(
                clippy::too_many_arguments,
                reason = "the generated Python method adds one host-context argument"
            )]
            fn #ident<'py>(
                #receiver
                __bimetal_py: ::bimetal::__private::pyo3::Python<'py>,
                #declarations
            ) -> #return_ty {
                #body
            }
        }
    });

    quote! {
        ::bimetal::__python_items! {
            #[::bimetal::__private::pyo3::pyclass(name = #native_name, frozen)]
            #[pyo3(crate = "::bimetal::__private::pyo3")]
            struct #wrapper_ident {
                inner: ::bimetal::runtime::Resource<#resource_ty>,
            }

            #[::bimetal::__private::pyo3::pymethods]
            #[pyo3(crate = "::bimetal::__private::pyo3")]
            impl #wrapper_ident {
                #(#members)*

                fn #close(&self) {
                    self.inner.close();
                }

                fn #closed(&self) -> ::core::primitive::bool {
                    self.inner.closed()
                }
            }

            impl ::bimetal::convert::python::HostResource for #resource_ty {
                fn into_host(
                    __bimetal_py: ::bimetal::__private::pyo3::Python<'_>,
                    value: Self,
                ) -> ::bimetal::__private::pyo3::PyResult<::bimetal::__private::pyo3::Py<::bimetal::__private::pyo3::PyAny>> {
                    ::bimetal::__private::pyo3::Py::new(
                        __bimetal_py,
                        #wrapper_ident {
                            inner: ::bimetal::runtime::Resource::new(value),
                        },
                    )
                    .map(::bimetal::__private::pyo3::Py::into_any)
                }

                fn with_resource<R>(
                    value: &::bimetal::__private::pyo3::Bound<'_, ::bimetal::__private::pyo3::PyAny>,
                    reach: impl ::core::ops::FnOnce(&::bimetal::runtime::Resource<Self>) -> R,
                ) -> ::core::option::Option<R> {
                    value
                        .cast::<#wrapper_ident>()
                        .ok()
                        .map(|__bimetal_object| reach(&__bimetal_object.get().inner))
                }
            }

            fn #register_ident(
                __bimetal_module: &::bimetal::__private::pyo3::Bound<'_, ::bimetal::__private::pyo3::types::PyModule>,
            ) -> ::bimetal::__private::pyo3::PyResult<()> {
                ::bimetal::__private::pyo3::types::PyModuleMethods::add_class::<#wrapper_ident>(__bimetal_module)
            }

            ::bimetal::__private::inventory::submit! {
                ::bimetal::runtime::python::Registration(#register_ident)
            }
        }
    }
}
