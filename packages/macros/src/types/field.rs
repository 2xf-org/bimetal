//! Contract and rule-check tokens for one named model field.

use proc_macro2::TokenStream as TokenStream2;
use quote::{ToTokens, format_ident, quote};
use syn::{ExprPath, Field, Ident};

use crate::attributes::{self, SerdeDefault, SerdeRenameRule};
use crate::types::field_options::{self, FieldOptions, RuleCall};
use crate::types::type_ref;

/// One named model field, rendered for the contract and for the rule check
/// of `ContractType::check_rules`.
#[derive(Debug)]
pub(crate) struct ModelField {
    /// The construction of the field's `bimetal::ir::FieldDef`.
    pub(crate) definition: TokenStream2,
    /// The Rust field.
    pub(crate) ident: Ident,
    /// The name that hosts write, which rule violations name.
    wire_name: String,
    /// The checks of the rules that the field declares.
    rules: Vec<RuleCall>,
}

impl ModelField {
    /// Return the name that hosts write.
    pub(crate) fn wire_name(&self) -> &str {
        &self.wire_name
    }

    /// Render the statements that check this field's own rules and the rules
    /// inside its value, which `value`, a reference to the field, reaches.
    pub(crate) fn check(&self, value: &TokenStream2) -> TokenStream2 {
        let wire_name = &self.wire_name;
        let own = self.rules.iter().map(
            |RuleCall {
                 function,
                 arguments,
             }| {
                quote! {
                    #function(#value, #arguments)
                        .map_err(|violation| violation.in_field(#wire_name))?;
                }
            },
        );
        quote! {
            #(#own)*
            ::bimetal::ContractType::check_rules(#value)
                .map_err(|violation| violation.in_field(#wire_name))?;
        }
    }
}

/// Render what an absent field holds in the contract: for a field with
/// `#[serde(default)]`, the Rust `Default` of its type, and for one with
/// `#[serde(default = "path")]`, what that function returns. The
/// registration calls the function, and checks the field's `rules` and the
/// rules inside the value. `variant` names the field's variant in an enum.
fn default_tokens(
    field: &Field,
    default: Option<&SerdeDefault>,
    rules: &[RuleCall],
    variant: Option<&str>,
    rust_name: &str,
) -> syn::Result<TokenStream2> {
    let ty = &field.ty;
    let (function, name) = match default {
        None => return Ok(quote!(::core::option::Option::None)),
        Some(SerdeDefault {
            function: Some(function),
            ..
        }) => (
            function.parse::<ExprPath>()?.into_token_stream(),
            function.value(),
        ),
        // Serde reads an absent `Option` as `None` either way.
        Some(_) if type_ref::option_item(ty).is_some() => {
            return Ok(quote!(::core::option::Option::None));
        }
        Some(_) => (
            quote!(<#ty as ::core::default::Default>::default),
            "Default::default".to_owned(),
        ),
    };
    let owner = match variant {
        Some(variant) => quote!(&::std::format!("{}::{}", __bimetal_owner, #variant)),
        None => quote!(&__bimetal_owner),
    };
    let own = rules.iter().map(
        |RuleCall {
             function,
             arguments,
         }| quote!(#function(__bimetal_value, #arguments)?;),
    );
    // The call and the inferred type repeat Serde's own call of the function,
    // so a missing or mistyped function fails only where Serde reports it.
    Ok(quote!(::bimetal::registry::field_default(
        #owner,
        #rust_name,
        || #function(),
        #name,
        |__bimetal_value| {
            #(#own)*
            ::bimetal::ContractType::check_rules(__bimetal_value)
        },
        __bimetal_problems,
    )))
}

/// Render one named model field and its validated constraints, or `None` for
/// a runtime-only field that both Serde and Bimetal skip. `variant` names
/// the field's variant in an enum.
///
/// # Panics
///
/// Panics if `field` is a tuple field, which has no name.
pub(crate) fn field_tokens(
    field: &Field,
    rename_all: Option<SerdeRenameRule>,
    variant: Option<&str>,
) -> syn::Result<Option<ModelField>> {
    use syn::ext::IdentExt;
    use syn::spanned::Spanned;

    let ident = field
        .ident
        .as_ref()
        .expect("callers pass only the fields of named structs and variants");
    let rust_name = ident.unraw().to_string();
    let serde = attributes::serde_field(&field.attrs)?;
    let docs = attributes::docs_tokens(&field.attrs);
    let options = FieldOptions::parse(&field.attrs)?;
    if serde.skip || options.skip {
        if !serde.skip || !options.skip {
            return Err(syn::Error::new(
                field.span(),
                "runtime-only fields must declare both `#[serde(skip)]` and `#[bimetal(skip)]`",
            ));
        }
        if serde.rename.is_some()
            || serde.default.is_some()
            || serde.skip_serializing_if.is_some()
            || options.boundary.is_some()
            || options.has_rules()
        {
            return Err(syn::Error::new(
                field.span(),
                "a skipped runtime-only field cannot declare wire constraints or defaults",
            ));
        }
        return Ok(None);
    }
    if let Some(boundary) = options.boundary {
        return Err(syn::Error::new(
            field.ty.span(),
            format!(
                "`#[bimetal({})]` is supported only on top-level exported parameters; move large binary values out of models",
                boundary.name()
            ),
        ));
    }
    options.validate(field, &serde)?;
    let required = options.is_required(field, &serde);
    let wire_name = serde
        .rename
        .unwrap_or_else(|| attributes::apply_serde_field_case(&rust_name, rename_all));
    let ty = type_ref::type_ref_tokens(&field.ty, None)?;
    let rules = options.rule_calls();
    let default = default_tokens(field, serde.default.as_ref(), &rules, variant, &rust_name)?;
    let literal = field_options::scalar_option_tokens(options.literal.as_ref());
    let min_length = field_options::option_tokens(options.min_length);
    let max_length = field_options::option_tokens(options.max_length);
    let ge = field_options::option_tokens(options.ge);
    let le = field_options::option_tokens(options.le);
    let definition = quote!(::bimetal::ir::FieldDef {
        rust_name: #rust_name.to_owned(),
        wire_name: #wire_name.to_owned(),
        docs: #docs,
        ty: #ty,
        required: #required,
        default: #default,
        constraints: ::bimetal::ir::FieldConstraints {
            literal: #literal,
            min_length: #min_length,
            max_length: #max_length,
            ge: #ge,
            le: #le,
        },
    });
    Ok(Some(ModelField {
        definition,
        ident: ident.clone(),
        wire_name,
        rules,
    }))
}

/// Bind each of `fields` under a private name, which no constant in the
/// user's scope can shadow. Return the names, in order, and the braced
/// pattern that binds them and ignores every other field.
pub(crate) fn field_bindings(fields: &[ModelField]) -> (Vec<Ident>, TokenStream2) {
    let bindings: Vec<Ident> = (0..fields.len())
        .map(|index| format_ident!("__bimetal_field_{index}"))
        .collect();
    let patterns = fields.iter().zip(&bindings).map(|(field, binding)| {
        let field = &field.ident;
        quote!(#field: #binding)
    });
    let pattern = quote!({ #(#patterns,)* .. });
    (bindings, pattern)
}

#[cfg(test)]
mod tests {
    use syn::parse::Parser;

    use super::*;

    fn named_field(tokens: TokenStream2) -> Field {
        Field::parse_named
            .parse2(tokens)
            .expect("parse named field")
    }

    #[test]
    fn model_binary_fields_are_rejected() {
        let field = named_field(quote!(
            #[bimetal(bytes)]
            payload: Vec<u8>
        ));
        let error = field_tokens(&field, None, None).expect_err("nested bytes must be rejected");

        assert!(error.to_string().contains("top-level exported parameters"));
    }

    #[test]
    fn constraints_fail_closed_for_unknown_field_types() {
        let field = named_field(quote!(
            #[bimetal(ge = 1)]
            timestamp: chrono::DateTime<chrono::Utc>
        ));
        let error =
            field_tokens(&field, None, None).expect_err("unknown constraint must be rejected");

        assert!(error.to_string().contains("apply only to integer fields"));
    }

    #[test]
    fn paired_serde_and_bimetal_skip_excludes_runtime_only_fields() {
        let field = named_field(quote!(
            #[serde(skip)]
            #[bimetal(skip)]
            post_process: Option<fn(f64) -> f64>
        ));

        assert!(field_tokens(&field, None, None).unwrap().is_none());
    }

    #[test]
    fn runtime_only_fields_require_paired_skip_attributes() {
        let field = named_field(quote!(
            #[bimetal(skip)]
            post_process: Option<fn(f64) -> f64>
        ));
        let error = field_tokens(&field, None, None).expect_err("Serde must also skip the field");

        assert!(error.to_string().contains("must declare both"));
    }
}
