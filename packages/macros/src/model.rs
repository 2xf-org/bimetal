//! Expansion of the `Model` derive.
//!
//! The expansion produces a `ContractType` implementation, which checks the
//! model's field rules, and an inventory registration of the model's shape.
//! All structural validation is completed before token emission. Only the
//! compiled library knows the default of a field, so the registration reads
//! it there.

use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::spanned::Spanned;
use syn::visit_mut::VisitMut;
use syn::{
    Attribute, Data, DataEnum, DataStruct, DeriveInput, Fields, FieldsNamed, Ident, Variant,
};

use crate::attributes::{self, SerdeContainer, SerdeRenameRule};
use crate::registration;
use crate::types::{self, FieldOptions, ModelField};

/// A model's contract shape, and the body of its `check_rules`, or `None` when
/// the model has no fields to check.
struct ModelShape {
    /// The construction of the model's `bimetal::ir::TypeShape`.
    shape: TokenStream2,
    /// The statements of `ContractType::check_rules`.
    check: Option<TokenStream2>,
}

/// Expand a `Model` derive into contract identity and lazy registration.
///
/// A derive that fails also implements `ContractType`, with a body that never
/// runs, so that the compiler reports the derive's error alone rather than
/// also every export that uses the type.
pub(super) fn expand_type(input: &DeriveInput) -> TokenStream2 {
    expand(input).unwrap_or_else(|error| {
        let error = error.into_compile_error();
        let ident = &input.ident;
        let (impl_generics, type_generics, where_clause) = input.generics.split_for_impl();
        quote! {
            #error
            impl #impl_generics ::bimetal::ContractType for #ident #type_generics #where_clause {
                type Crossing = ::bimetal::crossing::Serde;

                fn type_ref() -> ::bimetal::ir::TypeRef {
                    ::core::unreachable!("the `Model` derive failed to compile")
                }
            }
        }
    })
}

/// Expand a valid `Model` derive.
fn expand(input: &DeriveInput) -> syn::Result<TokenStream2> {
    use syn::ext::IdentExt;

    types::reject_generics(&input.generics, input.ident.span())?;
    let ident = &input.ident;
    let type_name = ident.unraw().to_string();
    let docs = attributes::docs_tokens(&input.attrs);
    let id = quote!(concat!(module_path!(), "::", #type_name).to_owned());
    attributes::reject_type_attributes(&input.attrs)?;
    // The registration renders the field types outside the type's `impl`,
    // where `Self` names nothing, so a recursive type names itself.
    let mut data = input.data.clone();
    types::SelfToPath(&ident.clone().into()).visit_data_mut(&mut data);
    let ModelShape { shape, check } = match &data {
        Data::Struct(data) => struct_shape(&input.attrs, ident, data)?,
        Data::Enum(data) => enum_shape(&input.attrs, data)?,
        Data::Union(data) => {
            return Err(syn::Error::new(
                data.union_token.span,
                "unions cannot cross a Bimetal boundary",
            ));
        }
    };
    let check_rules = check.map(|check| {
        quote! {
            fn check_rules(
                &self,
            ) -> ::core::result::Result<(), ::bimetal::runtime::ValueError> {
                #check
                ::core::result::Result::Ok(())
            }
        }
    });

    let registration = registration::submit_type(&type_name, &id, &id, &docs, &shape);
    Ok(quote! {
        impl ::bimetal::ContractType for #ident {
            type Crossing = ::bimetal::crossing::Serde;

            fn type_ref() -> ::bimetal::ir::TypeRef {
                ::bimetal::ir::TypeRef::Named {
                    identity: ::bimetal::ir::DefinitionId::new(
                        env!("CARGO_PKG_NAME"),
                        #id,
                    ),
                }
            }

            #check_rules
        }

        #registration
    })
}

/// Analyze one supported struct shape and render its contract
/// representation and its rule check.
fn struct_shape(attrs: &[Attribute], ident: &Ident, data: &DataStruct) -> syn::Result<ModelShape> {
    let serde = attributes::serde_container(attrs)?;
    if serde.rename_all_fields.is_some() {
        return Err(syn::Error::new(
            ident.span(),
            "`#[serde(rename_all_fields = ...)]` is supported only on enums",
        ));
    }
    if serde.tag.is_some() {
        // Serde would add a field with the struct's name, which the hosts
        // would not declare.
        return Err(syn::Error::new(
            ident.span(),
            "`#[serde(tag = ...)]` is supported only on enums; remove it, or declare the tag as a field",
        ));
    }
    match &data.fields {
        Fields::Named(fields) => {
            if serde.transparent {
                return Err(syn::Error::new(
                    ident.span(),
                    "a transparent struct must have exactly one tuple field",
                ));
            }
            let fields = model_fields(fields, serde.rename_all, None)?;
            let definitions = fields.iter().map(|field| &field.definition);
            let checks = fields.iter().map(|field| {
                let ident = &field.ident;
                field.check(&quote!(&self.#ident))
            });
            Ok(ModelShape {
                shape: quote!(::bimetal::ir::TypeShape::Struct {
                    fields: vec![#(#definitions),*],
                }),
                check: Some(quote!(#(#checks)*)),
            })
        }
        Fields::Unnamed(fields) if fields.unnamed.len() == 1 && serde.transparent => {
            let field = fields
                .unnamed
                .first()
                .expect("the match guard checked for exactly one field");
            let options = FieldOptions::parse(&field.attrs)?;
            options.validate_alias(field)?;
            let boundary = options
                .boundary
                .or_else(|| types::bytes_boundary(&field.ty));
            let target = types::type_ref_tokens(&field.ty, boundary)?;
            Ok(ModelShape {
                shape: quote!(::bimetal::ir::TypeShape::Alias {
                    target: #target,
                }),
                // The alias is its value on the wire, so the path stays. Bytes
                // and buffers hold no models.
                check: boundary.is_none().then(|| {
                    quote! {
                        ::bimetal::ContractType::check_rules(&self.0)?;
                    }
                }),
            })
        }
        Fields::Unnamed(fields) => Err(syn::Error::new(
            fields.span(),
            "tuple structs require `#[serde(transparent)]` and exactly one field",
        )),
        Fields::Unit => Err(syn::Error::new(
            ident.span(),
            "unit structs are not Bimetal contract types",
        )),
    }
}

/// Analyze an enum and render its variants and its rule check. Without
/// `#[serde(tag = ...)]`, an enum of unit variants is a string enum, and one
/// with a variant that carries data is externally tagged, Serde's default.
fn enum_shape(attrs: &[Attribute], data: &DataEnum) -> syn::Result<ModelShape> {
    let serde = attributes::serde_container(attrs)?;
    let carries_data = data
        .variants
        .iter()
        .any(|variant| !matches!(variant.fields, Fields::Unit));
    if serde.tag.is_none() && carries_data {
        return external_enum_shape(&serde, data);
    }
    let variants: Vec<ModelVariant> = data
        .variants
        .iter()
        .map(|variant| variant_tokens(variant, &serde))
        .collect::<syn::Result<_>>()?;
    let definitions = variants.iter().map(|variant| &variant.definition);
    let Some(tag) = serde.tag else {
        return Ok(ModelShape {
            shape: quote!(::bimetal::ir::TypeShape::StringEnum {
                variants: vec![#(#definitions),*],
            }),
            check: None,
        });
    };
    // An enum without variants has no value to match.
    let check = (!variants.is_empty()).then(|| {
        let arms = variants.iter().map(|variant| &variant.check_arm);
        quote! {
            match self {
                #(#arms)*
            }
        }
    });
    Ok(ModelShape {
        shape: quote!(::bimetal::ir::TypeShape::TaggedEnum {
            tag: #tag.to_owned(),
            variants: vec![#(#definitions),*],
        }),
        check,
    })
}

/// One enum variant, rendered for the contract and as the arm of the enum's
/// rule check.
struct ModelVariant {
    /// The construction of the variant's `bimetal::ir::EnumVariantDef`.
    definition: TokenStream2,
    /// The `match` arm that checks the variant's fields.
    check_arm: TokenStream2,
}

/// Render one enum variant. Only an internally tagged enum, whose tag field
/// says which variant a value is, may have variants with fields.
fn variant_tokens(variant: &Variant, serde: &SerdeContainer) -> syn::Result<ModelVariant> {
    use syn::ext::IdentExt;

    reject_variant_rules(variant)?;
    let rust_name = variant.ident.unraw().to_string();
    let wire_name = attributes::serde_rename(&variant.attrs)?
        .unwrap_or_else(|| attributes::apply_serde_variant_case(&rust_name, serde.rename_all));
    let docs = attributes::docs_tokens(&variant.attrs);
    let ident = &variant.ident;
    let (fields, check_arm) = match &variant.fields {
        Fields::Unit => (Vec::new(), quote!(Self::#ident => {})),
        Fields::Named(fields) if serde.tag.is_some() => {
            let fields = model_fields(fields, serde.rename_all_fields, Some(&rust_name))?;
            let (bindings, pattern) = types::field_bindings(&fields);
            let checks = fields
                .iter()
                .zip(&bindings)
                .map(|(field, binding)| field.check(&quote!(#binding)));
            let check_arm = quote!(Self::#ident #pattern => { #(#checks)* });
            (fields, check_arm)
        }
        other => {
            return Err(syn::Error::new(
                other.span(),
                "an internally tagged enum supports unit and named variants",
            ));
        }
    };
    let definitions = fields.iter().map(|field| &field.definition);
    Ok(ModelVariant {
        definition: quote!(::bimetal::ir::EnumVariantDef {
            rust_name: #rust_name.to_owned(),
            wire_name: #wire_name.to_owned(),
            docs: #docs,
            fields: vec![#(#definitions),*],
        }),
        check_arm,
    })
}

/// Analyze an externally tagged enum and render its variants and its rule
/// check.
fn external_enum_shape(serde: &SerdeContainer, data: &DataEnum) -> syn::Result<ModelShape> {
    let variants: Vec<ModelVariant> = data
        .variants
        .iter()
        .map(|variant| external_variant_tokens(variant, serde))
        .collect::<syn::Result<_>>()?;
    let definitions = variants.iter().map(|variant| &variant.definition);
    let arms = variants.iter().map(|variant| &variant.check_arm);
    Ok(ModelShape {
        shape: quote!(::bimetal::ir::TypeShape::ExternalEnum {
            variants: vec![#(#definitions),*],
        }),
        check: Some(quote! {
            match self {
                #(#arms)*
            }
        }),
    })
}

/// Render one variant of an externally tagged enum: a unit variant, a tuple
/// variant of one value, or a struct variant. A problem inside its data is
/// placed under the variant's wire name, the key the data crosses under.
fn external_variant_tokens(variant: &Variant, serde: &SerdeContainer) -> syn::Result<ModelVariant> {
    use syn::ext::IdentExt;

    reject_variant_rules(variant)?;
    let rust_name = variant.ident.unraw().to_string();
    let wire_name = attributes::serde_rename(&variant.attrs)?
        .unwrap_or_else(|| attributes::apply_serde_variant_case(&rust_name, serde.rename_all));
    let docs = attributes::docs_tokens(&variant.attrs);
    let ident = &variant.ident;
    let (payload, check_arm) = match &variant.fields {
        Fields::Unit => (
            quote!(::bimetal::ir::VariantPayload::Unit),
            quote!(Self::#ident => {}),
        ),
        Fields::Unnamed(fields) if fields.unnamed.len() == 1 => {
            let field = fields
                .unnamed
                .first()
                .expect("the match guard checked for exactly one field");
            if let Some(attr) = field
                .attrs
                .iter()
                .find(|attr| attr.path().is_ident("bimetal") || attr.path().is_ident("serde"))
            {
                return Err(syn::Error::new_spanned(
                    attr,
                    "the value of a one-field variant takes no `#[bimetal(...)]` or `#[serde(...)]` attribute; put rules on the fields of its type",
                ));
            }
            let ty = types::type_ref_tokens(&field.ty, None)?;
            (
                quote!(::bimetal::ir::VariantPayload::Value { ty: #ty }),
                quote! {
                    Self::#ident(__bimetal_value) => {
                        ::bimetal::ContractType::check_rules(__bimetal_value)
                            .map_err(|violation| violation.in_field(#wire_name))?;
                    }
                },
            )
        }
        Fields::Unnamed(fields) => {
            return Err(syn::Error::new(
                fields.span(),
                "a tuple variant crosses only with exactly one field; name the fields of a variant that holds more",
            ));
        }
        Fields::Named(fields) => {
            let fields = model_fields(fields, serde.rename_all_fields, Some(&rust_name))?;
            let (bindings, pattern) = types::field_bindings(&fields);
            let checks = fields
                .iter()
                .zip(&bindings)
                .map(|(field, binding)| field.check(&quote!(#binding)));
            let definitions = fields.iter().map(|field| &field.definition);
            (
                quote!(::bimetal::ir::VariantPayload::Fields {
                    fields: vec![#(#definitions),*],
                }),
                quote! {
                    Self::#ident #pattern => {
                        let __bimetal_check =
                            || -> ::core::result::Result<(), ::bimetal::runtime::ValueError> {
                                #(#checks)*
                                ::core::result::Result::Ok(())
                            };
                        __bimetal_check().map_err(|violation| violation.in_field(#wire_name))?;
                    }
                },
            )
        }
    };
    Ok(ModelVariant {
        definition: quote!(::bimetal::ir::ExternalVariantDef {
            rust_name: #rust_name.to_owned(),
            wire_name: #wire_name.to_owned(),
            docs: #docs,
            payload: #payload,
        }),
        check_arm,
    })
}

/// Refuse `#[bimetal(...)]` on a variant, where the derive would read no rule.
fn reject_variant_rules(variant: &Variant) -> syn::Result<()> {
    match variant
        .attrs
        .iter()
        .find(|attr| attr.path().is_ident("bimetal"))
    {
        Some(attr) => Err(syn::Error::new_spanned(
            attr,
            "an enum variant takes no `#[bimetal(...)]` attribute; put rules on the fields of its data",
        )),
        None => Ok(()),
    }
}

/// Render the contract fields of a struct, or of the variant `variant`, in
/// declaration order, leaving out the runtime-only fields that both Serde
/// and Bimetal skip.
fn model_fields(
    fields: &FieldsNamed,
    rename_all: Option<SerdeRenameRule>,
    variant: Option<&str>,
) -> syn::Result<Vec<ModelField>> {
    let mut model_fields = Vec::new();
    for field in &fields.named {
        model_fields.extend(types::field_tokens(field, rename_all, variant)?);
    }
    Ok(model_fields)
}
