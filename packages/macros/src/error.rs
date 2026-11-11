//! Expansion of the `Error` derive.
//!
//! The expansion records the error's type identity and gives every variant a
//! stable error code for hosts. All structural validation is completed before
//! token emission.

use std::collections::BTreeSet;

use heck::ToSnakeCase;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::ext::IdentExt;
use syn::spanned::Spanned;
use syn::{Attribute, Data, DeriveInput, Fields};

use crate::attributes;
use crate::registration;
use crate::types;

/// Expand an `Error` derive into stable codes, identity, and registration.
///
/// A derive that fails also implements `ContractError`, with a body that
/// never runs, so that the compiler reports the derive's error alone rather
/// than also every export that returns the error.
pub(super) fn expand_error(input: &DeriveInput) -> TokenStream2 {
    expand(input).unwrap_or_else(|error| {
        let error = error.into_compile_error();
        let ident = &input.ident;
        let (impl_generics, type_generics, where_clause) = input.generics.split_for_impl();
        quote! {
            #error
            impl #impl_generics ::bimetal::runtime::ContractError for #ident #type_generics #where_clause {
                fn type_identity() -> ::bimetal::ir::DefinitionId {
                    ::core::unreachable!("the `bimetal::Error` derive failed to compile")
                }

                fn code(&self) -> &'static str {
                    ::core::unreachable!("the `bimetal::Error` derive failed to compile")
                }
            }
        }
    })
}

/// Expand a valid `Error` derive.
fn expand(input: &DeriveInput) -> syn::Result<TokenStream2> {
    types::reject_generics(&input.generics, input.ident.span())?;
    let ident = &input.ident;
    let error_name = ident.unraw().to_string();
    let docs = attributes::docs_tokens(&input.attrs);
    let id = quote!(concat!(module_path!(), "::", #error_name));
    let mut arms = Vec::new();
    let mut codes = Vec::new();
    let mut seen = BTreeSet::new();
    let mut details_arms = Vec::new();
    let mut details_models = Vec::new();
    let code_body = match &input.data {
        Data::Enum(data) => {
            for variant in &data.variants {
                let variant_ident = &variant.ident;
                let rust_name = variant_ident.unraw().to_string();
                let code = attributes::serde_rename(&variant.attrs)?
                    .unwrap_or_else(|| rust_name.to_snake_case());
                if !seen.insert(code.clone()) {
                    return Err(syn::Error::new(
                        variant_ident.span(),
                        format!("error code `{code}` is used by more than one variant"),
                    ));
                }
                let pattern = match &variant.fields {
                    Fields::Unit => quote!(Self::#variant_ident),
                    Fields::Named(_) => quote!(Self::#variant_ident { .. }),
                    Fields::Unnamed(_) => quote!(Self::#variant_ident ( .. )),
                };
                arms.push(quote!(#pattern => #code));
                let variant_docs = attributes::docs_tokens(&variant.attrs);
                let details = declared_details(
                    &variant.attrs,
                    &variant.fields,
                    &format!("{error_name}{rust_name}"),
                    &format!("{error_name}::{rust_name}"),
                    &variant_docs,
                    &quote!(Self::#variant_ident),
                )?;
                let details_id = details.as_ref().map(|details| {
                    details_arms.push(details.arm.clone());
                    details_models.push(details.registration.clone());
                    details.identity.clone()
                });
                let details_id = option_tokens(details_id.as_ref());
                codes.push(quote!(::bimetal::ir::ErrorCode {
                    code: #code.to_owned(),
                    docs: #variant_docs,
                    details: #details_id,
                }));
            }
            quote!(match self { #(#arms),* })
        }
        Data::Struct(data) => {
            let rust_name = ident.unraw().to_string();
            let code = attributes::serde_rename(&input.attrs)?
                .unwrap_or_else(|| rust_name.to_snake_case());
            let details = declared_details(
                &input.attrs,
                &data.fields,
                &format!("{error_name}Details"),
                &error_name,
                &quote!(None),
                &quote!(Self),
            )?;
            let details_id = details.as_ref().map(|details| {
                details_arms.push(details.arm.clone());
                details_models.push(details.registration.clone());
                details.identity.clone()
            });
            let details_id = option_tokens(details_id.as_ref());
            codes.push(quote!(::bimetal::ir::ErrorCode {
                code: #code.to_owned(),
                docs: None,
                details: #details_id,
            }));
            quote!(#code)
        }
        Data::Union(data) => {
            return Err(syn::Error::new(
                data.union_token.span,
                "unions cannot be Bimetal errors",
            ));
        }
    };

    let details_method = (!details_arms.is_empty()).then(|| {
        quote! {
            fn details(&self) -> ::core::option::Option<::std::string::String> {
                #[allow(unreachable_patterns)]
                match self {
                    #(#details_arms)*
                    _ => ::core::option::Option::None,
                }
            }
        }
    });
    let origin = registration::origin();
    let registration = registration::submit(
        "ErrorRegistration",
        &quote!(::bimetal::ir::ErrorDef),
        &quote! {
            ::bimetal::ir::ErrorDef {
                #origin
                id: #id.to_owned(),
                name: #error_name.to_owned(),
                docs: #docs,
                codes: vec![#(#codes),*],
            }
        },
    );
    Ok(quote! {
        impl ::bimetal::runtime::ContractError for #ident {
            fn type_identity() -> ::bimetal::ir::DefinitionId {
                ::bimetal::ir::DefinitionId::new(env!("CARGO_PKG_NAME"), #id)
            }

            fn code(&self) -> &'static str {
                #code_body
            }

            #details_method
        }

        #registration
        #(#details_models)*
    })
}

/// What `#[bimetal(details)]` on a variant or a struct adds to its error.
struct Details {
    /// The match arm that serializes the fields of a value with them.
    arm: TokenStream2,
    /// The registration of the model that describes them to the hosts.
    registration: TokenStream2,
    /// The construction of that model's `bimetal::ir::DefinitionId`.
    identity: TokenStream2,
}

/// Render an optional `DefinitionId` construction.
fn option_tokens(identity: Option<&TokenStream2>) -> TokenStream2 {
    match identity {
        Some(identity) => quote!(::core::option::Option::Some(#identity)),
        None => quote!(::core::option::Option::None),
    }
}

/// Return what `#[bimetal(details)]` adds to the variant or struct with these
/// attributes and fields, which `path` names, or `None` when it does not
/// declare them. `model`, the name of the model that describes them, is also
/// its host type's name, and `owner` is how the author wrote the variant or
/// struct.
fn declared_details(
    attrs: &[Attribute],
    fields: &Fields,
    model: &str,
    owner: &str,
    docs: &TokenStream2,
    path: &TokenStream2,
) -> syn::Result<Option<Details>> {
    let mut declared = None;
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("bimetal")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("details") {
                attributes::declare_once(&meta, declared.is_some())?;
                declared = Some(meta.path.span());
                Ok(())
            } else {
                Err(meta.error("an error takes `#[bimetal(details)]`, for its named fields"))
            }
        })?;
    }
    let Some(span) = declared else {
        return Ok(None);
    };
    let Fields::Named(named) = fields else {
        return Err(syn::Error::new(
            span,
            "`#[bimetal(details)]` needs named fields, which give the hosts their names",
        ));
    };
    let mut definitions = Vec::new();
    let mut bindings = Vec::new();
    let mut entries = Vec::new();
    for field in &named.named {
        let Some(field) = types::field_tokens(field, None, None)? else {
            continue;
        };
        definitions.push(field.definition.clone());
        let binding = &field.ident;
        let wire_name = field.wire_name();
        bindings.push(quote!(#binding));
        entries.push(quote!(#wire_name: #binding));
    }
    let id = quote!(concat!(module_path!(), "::", #model));
    let registration = registration::submit_type(
        model,
        &quote!(#id.to_owned()),
        &quote!(concat!(module_path!(), "::", #owner).to_owned()),
        docs,
        &quote!(::bimetal::ir::TypeShape::Struct {
            fields: vec![#(#definitions),*],
        }),
    );
    Ok(Some(Details {
        arm: quote! {
            #path { #(#bindings,)* .. } => ::core::option::Option::Some(
                ::bimetal::__private::serde_json::json!({ #(#entries),* }).to_string(),
            ),
        },
        registration,
        identity: quote!(::bimetal::ir::DefinitionId::new(env!("CARGO_PKG_NAME"), #id)),
    }))
}
