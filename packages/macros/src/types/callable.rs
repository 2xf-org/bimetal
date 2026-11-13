//! The signature of an exported function, method, or constructor, checked
//! once for the contract and for every host's wrapper.

use bimetal_ir::{python_names, typescript_names};
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::ext::IdentExt;
use syn::spanned::Spanned;
use syn::{
    Expr, ExprLit, FnArg, GenericArgument, Ident, Lit, Pat, PathArguments, ReturnType, Safety,
    Type as SynType,
};

use crate::attributes::{self, Boundary};
use crate::types::result_types::ResultTypes;
use crate::types::{DirectType, type_ref, validation};

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
    /// Whether a synchronous call keeps Python's interpreter lock, as
    /// `#[bimetal(brief)]` asks.
    pub(crate) brief: bool,
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
    /// As the type's crossing says, through Serde or as a resource: the call
    /// takes a value of this type, or borrows one when the parameter is `&T`.
    Value(&'syntax SynType),
    /// As a resource of this type, which only a resource can be.
    Resource {
        /// The resource's type.
        value: &'syntax SynType,
        /// How the call borrows it.
        shape: ResourceShape,
    },
    /// Through a direct boundary.
    Direct(DirectType<'syntax>),
    /// As a host function, which the `bimetal::Callback` of this type calls.
    Callback(&'syntax SynType),
}

/// How a parameter borrows a resource in a way that no other value could
/// cross.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResourceShape {
    /// Alone: `&mut T`.
    Exclusive,
    /// When the argument is not `None`: `Option<&T>`.
    Optional,
}

/// What an exported callable returns.
pub(crate) struct Returns<'syntax> {
    /// The error type, when the callable returns `Result`.
    pub(crate) error: Option<&'syntax SynType>,
    /// The direct boundary of a successful value: bytes, or what
    /// `returns(...)` declares.
    pub(crate) direct: Option<DirectType<'syntax>>,
    /// Whether the direct value is inside an `Option`.
    pub(crate) optional: bool,
    /// The successful value, or `None` for `()`.
    pub(crate) value: Option<ReturnValue>,
    /// The crossing of a successful value that crosses by its type, which
    /// the item of an `Option` or a `Vec` decides, so that one of resources
    /// crosses as they do; `None` for `()` and a direct value.
    pub(crate) crossing: Option<TokenStream2>,
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
    /// `return_boundary`, if any, and which is `brief` when its attributes
    /// say so.
    pub(crate) fn parse(
        signature: &'syntax syn::Signature,
        return_boundary: Option<Boundary>,
        brief: bool,
    ) -> syn::Result<Self> {
        use heck::ToLowerCamelCase;

        validation::reject_generics(&signature.generics, signature.ident.span())?;
        if matches!(&signature.safety, Safety::Unsafe(_)) || signature.variadic.is_some() {
            return Err(syn::Error::new(
                signature.span(),
                "unsafe and variadic exports are not supported",
            ));
        }
        if let Some(asyncness) = signature.asyncness.filter(|_| brief) {
            return Err(syn::Error::new(
                asyncness.span(),
                "brief applies to a synchronous call",
            ));
        }
        let mut params = Vec::new();
        for argument in &signature.inputs {
            if let FnArg::Typed(argument) = argument {
                let param = Param::parse(argument)?;
                // A brief call keeps Python's interpreter lock, which a host
                // function or stream that runs on another thread waits for.
                if brief && param.waits_for_host() {
                    return Err(syn::Error::new(
                        argument.ty.span(),
                        "a brief call cannot take a host function or stream, which may wait for the interpreter lock that the call keeps",
                    ));
                }
                params.push(param);
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
            brief,
        })
    }

    /// Render the callable's `bimetal::ir::CallableDef` construction, with
    /// `docs`, the construction of its documentation.
    pub(crate) fn callable_tokens(&self, docs: &TokenStream2) -> TokenStream2 {
        let Self {
            rust_name,
            host_name,
            is_async,
            brief,
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
            brief: #brief,
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
        let boundary = options.boundary.or_else(|| type_ref::bytes_boundary(ty));
        let resource = boundary.is_none().then(|| resource_borrow(ty)).flatten();
        let (borrowed, value, type_ref) = if let Some((value, shape)) = resource {
            reject_nested_references(value)?;
            (
                true,
                ParamValue::Resource { value, shape },
                shape.type_ref_tokens(value),
            )
        } else {
            let (borrowed, value) = match ty {
                SynType::Reference(reference) if reference.mutability.is_some() => {
                    return Err(syn::Error::new(
                        reference.span(),
                        "a direct boundary takes a value or a shared reference",
                    ));
                }
                SynType::Reference(reference) => (true, reference.elem.as_ref()),
                ty => (false, ty),
            };
            reject_nested_references(value)?;
            let (value, type_ref) = match boundary {
                Some(boundary) => {
                    let direct = DirectType::parse(ty, boundary)?;
                    let type_ref = direct.type_ref_tokens();
                    (ParamValue::Direct(direct), type_ref)
                }
                None if type_ref::is_callback(ty) => (
                    ParamValue::Callback(ty),
                    type_ref::type_ref_tokens(ty, None)?,
                ),
                None => (
                    ParamValue::Value(value),
                    type_ref::type_ref_tokens(ty, None)?,
                ),
            };
            (borrowed, value, type_ref)
        };
        // A default is an expression of the parameter's type, which the contract
        // holds as the hosts write it; a string literal also fits a `String`.
        let default = match &options.default {
            Some(default) => {
                let value = match default {
                    Expr::Lit(ExprLit {
                        lit: Lit::Str(_), ..
                    }) => quote!(::core::convert::From::from(#default)),
                    default => quote!(#default),
                };
                quote!(::core::option::Option::Some(
                    ::bimetal::registry::parameter_default(
                        &{
                            let __bimetal_default: #ty = #value;
                            __bimetal_default
                        },
                        &#type_ref,
                    )
                ))
            }
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
            ParamValue::Resource { shape, .. } => match shape {
                ResourceShape::Exclusive => quote!(&mut #ident),
                ResourceShape::Optional => quote!(#ident.as_deref()),
            },
            _ if self.borrowed => quote!(&#ident),
            _ => quote!(#ident),
        }
    }

    /// Return whether the argument can make Rust wait for the host's event
    /// loop: a host function, or a stream whose batches the host makes as
    /// Rust reads them.
    pub(crate) fn waits_for_host(&self) -> bool {
        match &self.value {
            ParamValue::Callback(_) => true,
            ParamValue::Direct(DirectType::Arrow { value }) => {
                type_ref::type_last_ident(value).is_ok_and(|ident| ident == "RecordBatchStream")
            }
            ParamValue::Value(_) | ParamValue::Resource { .. } | ParamValue::Direct(_) => false,
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
        // Bytes, also in an `Option`, need no `returns(...)`.
        let boundary = boundary.or_else(|| {
            let value = success?;
            type_ref::bytes_boundary(type_ref::option_item(value).unwrap_or(value))
        });
        if signature.asyncness.is_some()
            && let Some(success) = success
            && contains_reference(success)
        {
            return Err(syn::Error::new(
                success.span(),
                "async exports must return owned values, because the host awaits them after the call returns",
            ));
        }
        // A direct value may be inside an `Option`.
        let inner = success
            .filter(|_| boundary.is_some())
            .and_then(type_ref::option_item);
        let optional = inner.is_some();
        let direct = match (boundary, inner.or(success)) {
            (Some(boundary), Some(value)) => Some(DirectType::parse(value, boundary)?),
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
        let success = success
            .filter(|success| !matches!(success, SynType::Tuple(tuple) if tuple.elems.is_empty()));
        let value = success.map(|success| ReturnValue {
            borrowed: matches!(success, SynType::Reference(_)),
        });
        let crossing = success.filter(|_| direct.is_none()).map(|success| {
            let item = type_ref::option_item(success)
                .or_else(|| type_ref::vec_item(success))
                .unwrap_or(success);
            quote!(<#item as ::bimetal::ContractType>::Crossing)
        });
        Ok(Self {
            error: result.map(|result| result.error),
            direct,
            optional,
            value,
            crossing,
            type_ref,
        })
    }
}

impl ResourceShape {
    /// Render the `bimetal::ir::TypeRef` construction of a parameter that
    /// borrows a resource of type `value` this way.
    fn type_ref_tokens(self, value: &SynType) -> TokenStream2 {
        let resource = quote!(::bimetal::ir::TypeRef::Resource {
            identity: <#value as ::bimetal::runtime::ResourceType>::identity(),
        });
        match self {
            Self::Exclusive => resource,
            Self::Optional => quote!(::bimetal::ir::TypeRef::Option {
                item: ::std::boxed::Box::new(#resource),
            }),
        }
    }
}

/// Return the resource that a parameter of type `ty` borrows in a way that
/// no other value could cross, `&mut T` or `Option<&T>`, and how.
fn resource_borrow(ty: &SynType) -> Option<(&SynType, ResourceShape)> {
    match ty {
        SynType::Reference(reference) if reference.mutability.is_some() => {
            Some((reference.elem.as_ref(), ResourceShape::Exclusive))
        }
        ty => match type_ref::option_item(ty) {
            Some(SynType::Reference(reference)) if reference.mutability.is_none() => {
                Some((reference.elem.as_ref(), ResourceShape::Optional))
            }
            _ => None,
        },
    }
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
