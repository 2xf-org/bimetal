//! The signature of an exported function, method, or constructor, checked
//! once for the contract and for every host's wrapper.

use bimetal_ir::{python_names, typescript_names};
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::ext::IdentExt;
use syn::spanned::Spanned;
use syn::{FnArg, GenericArgument, Ident, Pat, PathArguments, ReturnType, Safety, Type as SynType};

use crate::attributes::{self, Boundary};
use crate::types::result_types::ResultTypes;
use crate::types::{DirectType, ResourceShape, type_ref, validation};

/// One exported callable's signature.
pub(crate) struct Signature<'syntax> {
    /// The Rust name, without `r#`.
    pub(crate) rust_name: String,
    /// The lower-camel-case name that the contract and TypeScript use.
    pub(crate) host_name: String,
    /// The parameters after any receiver.
    pub(crate) params: Vec<Param<'syntax>>,
    /// What a call returns.
    pub(crate) returns: Returns<'syntax>,
    /// Whether the callable is an `async fn`.
    pub(crate) is_async: bool,
}

/// One parameter of an exported callable.
pub(crate) struct Param<'syntax> {
    /// The Rust binding, which the wrappers reuse for the argument.
    pub(crate) ident: &'syntax Ident,
    /// Whether the callable borrows the argument, which the wrapper owns.
    pub(crate) borrowed: bool,
    /// How the argument crosses into Rust.
    pub(crate) value: ParamValue<'syntax>,
    /// The name of the parameter in the Python wrapper.
    pub(crate) python_name: String,
    /// The name of the parameter in the JavaScript wrapper.
    pub(crate) javascript_name: String,
    /// The construction of the parameter's `bimetal::ir::ParamDef`.
    definition: TokenStream2,
}

/// How an argument crosses into Rust.
pub(crate) enum ParamValue<'syntax> {
    /// Through Serde, as the owned type that the call takes or borrows:
    /// `String` for `&str`, and `Vec<T>` for `&[T]`.
    Serde(TokenStream2),
    /// Through a direct boundary.
    Direct(DirectType<'syntax>),
    /// As a host function, which the `bimetal::Callback` of this type calls.
    Callback(&'syntax SynType),
}

/// What an exported callable returns.
pub(crate) struct Returns<'syntax> {
    /// The error type, when the callable returns `Result`.
    pub(crate) error: Option<&'syntax SynType>,
    /// The direct boundary of a successful value, from `returns(...)`.
    pub(crate) direct: Option<DirectType<'syntax>>,
    /// Whether the direct value is inside an `Option`, which a resource's
    /// shape says itself.
    pub(crate) optional: bool,
    /// The successful value, or `None` for `()`.
    pub(crate) value: Option<ReturnValue>,
    /// The construction of the successful value's `bimetal::ir::TypeRef`.
    pub(crate) type_ref: TokenStream2,
}

/// The successful value of a callable.
pub(crate) struct ReturnValue {
    /// Whether the callable returns a reference to it.
    pub(crate) borrowed: bool,
}

impl<'syntax> Signature<'syntax> {
    /// Check a signature that Bimetal exports, whose successful value crosses
    /// `return_boundary`, if any.
    pub(crate) fn parse(
        signature: &'syntax syn::Signature,
        return_boundary: Option<Boundary>,
    ) -> syn::Result<Self> {
        use heck::ToLowerCamelCase;

        validation::reject_generics(&signature.generics, signature.ident.span())?;
        if matches!(&signature.safety, Safety::Unsafe(_)) || signature.variadic.is_some() {
            return Err(syn::Error::new(
                signature.span(),
                "unsafe and variadic exports are not supported",
            ));
        }
        let mut params = Vec::new();
        for argument in &signature.inputs {
            if let FnArg::Typed(argument) = argument {
                params.push(Param::parse(argument)?);
            }
        }
        let rust_names: Vec<String> = params
            .iter()
            .map(|param| param.ident.unraw().to_string())
            .collect();
        let python_names = python_names::parameter_names(rust_names.iter().map(String::as_str));
        for (param, python_name) in params.iter_mut().zip(python_names) {
            param.python_name = python_name;
        }
        let rust_name = signature.ident.unraw().to_string();
        Ok(Self {
            host_name: rust_name.to_lower_camel_case(),
            rust_name,
            params,
            returns: Returns::parse(signature, return_boundary)?,
            is_async: signature.asyncness.is_some(),
        })
    }

    /// Render the callable's `bimetal::ir::CallableDef` construction, with
    /// `docs`, the construction of its documentation.
    pub(crate) fn callable_tokens(&self, docs: &TokenStream2) -> TokenStream2 {
        let Self {
            rust_name,
            host_name,
            is_async,
            ..
        } = self;
        let params = self.params.iter().map(|param| &param.definition);
        let error = match self.returns.error {
            Some(error) => {
                quote!(Some(<#error as ::bimetal::runtime::ContractError>::type_identity()))
            }
            None => quote!(None),
        };
        quote!(::bimetal::ir::CallableDef {
            rust_name: #rust_name.to_owned(),
            host_name: #host_name.to_owned(),
            docs: #docs,
            params: vec![#(#params),*],
            error: #error,
            is_async: #is_async,
        })
    }
}

impl<'syntax> Param<'syntax> {
    /// Check one typed parameter. The caller fills in its Python name, which
    /// depends on the other parameters.
    fn parse(argument: &'syntax syn::PatType) -> syn::Result<Self> {
        use heck::ToLowerCamelCase;

        let Pat::Ident(pattern) = argument.pat.as_ref() else {
            return Err(syn::Error::new(
                argument.pat.span(),
                "exported parameters must be simple identifiers",
            ));
        };
        let ident = &pattern.ident;
        let rust_name = ident.unraw().to_string();
        if rust_name.starts_with(python_names::WRAPPER_LOCAL_PREFIX) {
            return Err(syn::Error::new(
                ident.span(),
                format!(
                    "parameter `{rust_name}` uses the reserved `{}` prefix; rename it because that namespace belongs to generated Bimetal wrapper bindings",
                    python_names::WRAPPER_LOCAL_PREFIX
                ),
            ));
        }
        let ty = argument.ty.as_ref();
        let options = attributes::parameter_options(&argument.attrs)?;
        let resource = options.boundary == Some(Boundary::Resource);
        let borrowed = match ty {
            SynType::Reference(reference) if reference.mutability.is_some() && !resource => {
                return Err(syn::Error::new(
                    reference.span(),
                    "mutable reference parameters cannot cross a Bimetal boundary",
                ));
            }
            SynType::Reference(reference) => {
                reject_nested_references(&reference.elem)?;
                true
            }
            ty => {
                if !resource {
                    reject_nested_references(ty)?;
                }
                false
            }
        };
        let (value, type_ref) = match options.boundary {
            Some(boundary) => {
                let direct = DirectType::parse_param(ty, boundary)?;
                let type_ref = direct.type_ref_tokens();
                (ParamValue::Direct(direct), type_ref)
            }
            None if type_ref::is_callback(ty) => (
                ParamValue::Callback(ty),
                type_ref::type_ref_tokens(ty, None)?,
            ),
            None => (
                ParamValue::Serde(owned_type(ty)),
                type_ref::type_ref_tokens(ty, None)?,
            ),
        };
        let default = match &options.default {
            Some(default) => quote!(::core::option::Option::Some(
                ::bimetal::__private::serde_json::json!(#default)
            )),
            None => quote!(::core::option::Option::None),
        };
        let host_name = rust_name.to_lower_camel_case();
        let javascript_name = typescript_names::parameter_name(&host_name).into_owned();
        Ok(Self {
            ident,
            borrowed,
            value,
            python_name: String::new(),
            javascript_name,
            definition: quote!(::bimetal::ir::ParamDef {
                rust_name: #rust_name.to_owned(),
                host_name: #host_name.to_owned(),
                ty: #type_ref,
                default: #default,
            }),
        })
    }

    /// Return the argument of the Rust call, which lends the wrapper's value
    /// when the callable borrows it.
    pub(crate) fn call(&self) -> TokenStream2 {
        let ident = self.ident;
        match &self.value {
            ParamValue::Direct(DirectType::Resource { shape, .. }) => match shape {
                ResourceShape::Shared => quote!(&#ident),
                ResourceShape::Exclusive => quote!(&mut #ident),
                ResourceShape::OptionalShared => quote!(#ident.as_deref()),
                ResourceShape::Owned | ResourceShape::OptionalOwned | ResourceShape::List => {
                    quote!(#ident)
                }
            },
            _ if self.borrowed => quote!(&#ident),
            _ => quote!(#ident),
        }
    }
}

impl ReturnValue {
    /// Return the call's successful value, `__bimetal_value`, as a reference.
    pub(crate) fn reference(&self) -> TokenStream2 {
        if self.borrowed {
            quote!(__bimetal_value)
        } else {
            quote!(&__bimetal_value)
        }
    }

    /// Return `__bimetal_value` by value, cloning what the call borrows.
    pub(crate) fn owned(&self) -> TokenStream2 {
        if self.borrowed {
            quote!(::core::clone::Clone::clone(__bimetal_value))
        } else {
            quote!(__bimetal_value)
        }
    }

    /// Return the bytes or the buffer in `__bimetal_value` as a vector, copying
    /// what the call borrows.
    pub(crate) fn vector(&self) -> TokenStream2 {
        if self.borrowed {
            quote!(__bimetal_value.to_vec())
        } else {
            quote!(::std::vec::Vec::from(__bimetal_value))
        }
    }
}

impl<'syntax> Returns<'syntax> {
    /// Check what a signature returns.
    fn parse(signature: &'syntax syn::Signature, boundary: Option<Boundary>) -> syn::Result<Self> {
        let output = &signature.output;
        let result = ResultTypes::resolve(output)?;
        let success = match (&result, output) {
            (Some(result), _) => Some(result.ok),
            (None, ReturnType::Type(_, ty)) => Some(ty.as_ref()),
            (None, ReturnType::Default) => None,
        };
        if signature.asyncness.is_some()
            && let Some(success) = success
            && contains_reference(success)
        {
            return Err(syn::Error::new(
                success.span(),
                "async exports must return owned values, because the host awaits them after the call returns",
            ));
        }
        // A direct value other than a resource may be inside an `Option`.
        let inner = success
            .filter(|_| boundary.is_some_and(|boundary| boundary != Boundary::Resource))
            .and_then(type_ref::option_item);
        let optional = inner.is_some();
        let direct = match (boundary, inner.or(success)) {
            (Some(boundary), Some(value)) => Some(DirectType::parse_return(value, boundary)?),
            (Some(boundary), None) => {
                return Err(syn::Error::new_spanned(
                    signature,
                    format!("`returns({})` requires a return value", boundary.name()),
                ));
            }
            (None, _) => None,
        };
        let type_ref = match (&direct, success) {
            (Some(direct), _) => {
                let tokens = direct.type_ref_tokens();
                if optional {
                    quote!(::bimetal::ir::TypeRef::Option {
                        item: ::std::boxed::Box::new(#tokens),
                    })
                } else {
                    tokens
                }
            }
            (None, Some(success)) => type_ref::type_ref_tokens(success, None)?,
            (None, None) => quote!(::bimetal::ir::TypeRef::Unit),
        };
        let value = success
            .filter(|success| !matches!(success, SynType::Tuple(tuple) if tuple.elems.is_empty()))
            .map(|success| ReturnValue {
                borrowed: matches!(success, SynType::Reference(_)),
            });
        Ok(Self {
            error: result.map(|result| result.error),
            direct,
            optional,
            value,
            type_ref,
        })
    }
}

/// Return the owned type that Serde decodes for a parameter of type `ty`,
/// which the type system picks for a reference.
fn owned_type(ty: &SynType) -> TokenStream2 {
    let SynType::Reference(reference) = ty else {
        return quote!(#ty);
    };
    let borrowed = reference.elem.as_ref();
    quote!(<#borrowed as ::bimetal::__private::Owned>::Owned)
}

/// Reject references inside a parameter type, which no value that the
/// wrapper owns can lend.
fn reject_nested_references(ty: &SynType) -> syn::Result<()> {
    if contains_reference(ty) {
        return Err(syn::Error::new(
            ty.span(),
            "references are supported only at the outermost level of an exported parameter; use owned values inside Option, Vec, maps, and tuples",
        ));
    }
    Ok(())
}

/// Return whether any node in a syntax type tree is a reference.
fn contains_reference(ty: &SynType) -> bool {
    match ty {
        SynType::Reference(_) => true,
        SynType::Array(array) => contains_reference(&array.elem),
        SynType::Slice(slice) => contains_reference(&slice.elem),
        SynType::Tuple(tuple) => tuple.elems.iter().any(contains_reference),
        SynType::Paren(paren) => contains_reference(&paren.elem),
        SynType::Group(group) => contains_reference(&group.elem),
        SynType::Path(path) => path.path.segments.iter().any(|segment| {
            let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
                return false;
            };
            arguments.args.iter().any(
                |argument| matches!(argument, GenericArgument::Type(ty) if contains_reference(ty)),
            )
        }),
        _ => false,
    }
}
