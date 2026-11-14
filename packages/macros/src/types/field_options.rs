//! Field rules and defaults, with their validation.

use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{ToTokens, quote};
use syn::spanned::Spanned;
use syn::{Attribute, Expr, Field, Lit, LitInt, Type as SynType, UnOp};

use crate::attributes::{self, Boundary, SerdeDefault, SerdeField};
use crate::types::type_ref;

/// Scalar values accepted in field rules and defaults.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ScalarValue {
    /// A `true` or `false` literal.
    Bool(bool),
    /// An integer literal, which must fit in `i64`.
    I64(i64),
    /// A string literal.
    String(String),
}

/// A call that checks one rule of a field, whose value the caller passes
/// first: `function(value, arguments)`.
#[derive(Debug)]
pub(super) struct RuleCall {
    /// The function of `bimetal::runtime::rules` that checks the rule.
    pub(super) function: TokenStream2,
    /// The rule's limits or value, after the field's value.
    pub(super) arguments: TokenStream2,
}

/// Parsed scalar together with its diagnostic source span.
#[derive(Clone, Debug)]
pub(super) struct SpannedScalar {
    value: ScalarValue,
    span: Span,
}

/// Supported Bimetal metadata collected from a model field.
#[derive(Default)]
pub(crate) struct FieldOptions {
    /// Optional direct boundary; rejected for non-transparent model fields.
    pub(crate) boundary: Option<Boundary>,
    /// Exclude one runtime-only field from the generated wire contract.
    pub(super) skip: bool,
    /// Require the field even when its type or a default would allow it to
    /// be absent.
    pub(super) required: bool,
    /// The one value the field may hold (`literal = ...`).
    pub(super) literal: Option<SpannedScalar>,
    /// The minimum length of a string or list (`min_length = ...`).
    pub(super) min_length: Option<u64>,
    /// The maximum length of a string or list (`max_length = ...`).
    pub(super) max_length: Option<u64>,
    /// The inclusive minimum of an integer (`ge = ...`).
    pub(super) ge: Option<i64>,
    /// The inclusive maximum of an integer (`le = ...`).
    pub(super) le: Option<i64>,
}

impl FieldOptions {
    /// Parse field rules.
    pub(crate) fn parse(attrs: &[Attribute]) -> syn::Result<Self> {
        let mut options = Self::default();
        let bimetal_attrs = attrs.iter().filter(|attr| attr.path().is_ident("bimetal"));
        for attr in bimetal_attrs {
            options.parse_attribute(attr)?;
        }
        Ok(options)
    }

    /// Return whether the field declares a rule, which the hosts check.
    pub(super) fn has_rules(&self) -> bool {
        self.required
            || self.literal.is_some()
            || self.min_length.is_some()
            || self.max_length.is_some()
            || self.ge.is_some()
            || self.le.is_some()
    }

    /// Ensure a transparent alias does not silently discard field semantics.
    pub(crate) fn validate_alias(&self, field: &Field) -> syn::Result<()> {
        if self.skip || self.has_rules() {
            return Err(syn::Error::new(
                field.span(),
                "transparent aliases support only `#[bimetal(buffer)]`; field rules would otherwise be discarded",
            ));
        }
        Ok(())
    }

    /// Validate the field's rules against its type and its Serde attributes.
    pub(super) fn validate(&self, field: &Field, serde: &SerdeField) -> syn::Result<()> {
        let default = known_default(&field.ty, serde);
        validate_field_presence(field, self, serde)?;
        if let Some(literal) = self.literal.as_ref()
            && type_ref::option_item(&field.ty).is_some()
        {
            return Err(syn::Error::new(
                literal.span,
                "`literal` cannot apply to an `Option` field, whose `None` the literal would reject",
            ));
        }
        let kind = field_kind(&field.ty);
        validate_constraint_ranges(field, self, kind)?;
        // A `Vec` is empty by default, which the derive knows as it knows a
        // scalar's default.
        if let Some(SerdeDefault {
            span,
            function: None,
        }) = serde.default
            && kind == FieldKind::List
            && type_ref::option_item(&field.ty).is_none()
            && self.min_length.is_some_and(|minimum| minimum > 0)
        {
            return Err(syn::Error::new(
                span,
                "the field's default, the Rust `Default` that `#[serde(default)]` fills in, is shorter than its `min_length` constraint",
            ));
        }
        validate_constraint_values(self, kind, default.as_ref())
    }

    /// Return the calls of `bimetal::runtime::rules` that check the rules the
    /// field declares, and no call for a rule it leaves out.
    pub(super) fn rule_calls(&self) -> Vec<RuleCall> {
        let mut calls = Vec::new();
        if let Some(literal) = self.literal.as_ref() {
            let expected = match &literal.value {
                ScalarValue::Bool(value) => {
                    quote!(::bimetal::runtime::rules::RuleValue::Bool(#value))
                }
                ScalarValue::I64(value) => {
                    quote!(::bimetal::runtime::rules::RuleValue::Integer(#value as i128))
                }
                ScalarValue::String(value) => {
                    quote!(::bimetal::runtime::rules::RuleValue::String(#value))
                }
            };
            calls.push(RuleCall {
                function: quote!(::bimetal::runtime::rules::literal),
                arguments: expected,
            });
        }
        if self.min_length.is_some() || self.max_length.is_some() {
            let minimum = option_tokens(self.min_length);
            let maximum = option_tokens(self.max_length);
            calls.push(RuleCall {
                function: quote!(::bimetal::runtime::rules::length),
                arguments: quote!(#minimum, #maximum),
            });
        }
        if self.ge.is_some() || self.le.is_some() {
            let minimum = option_tokens(self.ge);
            let maximum = option_tokens(self.le);
            calls.push(RuleCall {
                function: quote!(::bimetal::runtime::rules::range),
                arguments: quote!(#minimum, #maximum),
            });
        }
        calls
    }

    /// Return whether hosts must always send the field: it is marked
    /// `required`, or it is not an `Option` and has no default.
    pub(super) fn is_required(&self, field: &Field, serde: &SerdeField) -> bool {
        self.required || (type_ref::option_item(&field.ty).is_none() && serde.default.is_none())
    }

    /// Record the options of one `#[bimetal(...)]` attribute.
    fn parse_attribute(&mut self, attr: &Attribute) -> syn::Result<()> {
        attr.parse_nested_meta(|meta| {
            if attributes::declare_boundary(&meta, &mut self.boundary, &[Boundary::Buffer])? {
                return Ok(());
            }
            if meta.path.is_ident("required") {
                attributes::declare_once(&meta, self.required)?;
                self.required = true;
            } else if meta.path.is_ident("literal") {
                attributes::declare_once(&meta, self.literal.is_some())?;
                self.literal = Some(parse_scalar(meta.value()?.parse::<Expr>()?)?);
            } else if meta.path.is_ident("min_length") {
                attributes::declare_once(&meta, self.min_length.is_some())?;
                self.min_length = Some(parse_u64(meta.value()?.parse::<Expr>()?, "min_length")?);
            } else if meta.path.is_ident("max_length") {
                attributes::declare_once(&meta, self.max_length.is_some())?;
                self.max_length = Some(parse_u64(meta.value()?.parse::<Expr>()?, "max_length")?);
            } else if meta.path.is_ident("ge") {
                attributes::declare_once(&meta, self.ge.is_some())?;
                self.ge = Some(parse_i64(meta.value()?.parse::<Expr>()?, "ge")?);
            } else if meta.path.is_ident("le") {
                attributes::declare_once(&meta, self.le.is_some())?;
                self.le = Some(parse_i64(meta.value()?.parse::<Expr>()?, "le")?);
            } else if meta.path.is_ident("skip") {
                attributes::declare_once(&meta, self.skip)?;
                self.skip = true;
            } else if meta.path.is_ident("default") {
                return Err(meta.error(
                    "remove `#[bimetal(default = ...)]`: `#[serde(default)]` gives an absent field the Rust `Default` of its type, and `#[serde(default = \"path\")]` what that function returns",
                ));
            } else {
                return Err(meta.error(
                    "supported field attributes are buffer, bytes, skip, required, literal, min_length, max_length, ge, and le",
                ));
            }
            Ok(())
        })
    }
}

/// Parse a boolean, signed integer, or string literal with its source span.
fn parse_scalar(expression: Expr) -> syn::Result<SpannedScalar> {
    let span = expression.span();
    let unsupported = || {
        syn::Error::new(
            span,
            "bimetal scalar values must be a boolean, signed 64-bit integer, or string literal",
        )
    };
    let value = match expression {
        Expr::Lit(literal) => match literal.lit {
            Lit::Bool(value) => ScalarValue::Bool(value.value),
            Lit::Int(value) => ScalarValue::I64(parse_positive_i64(&value, "scalar value")?),
            Lit::Str(value) => ScalarValue::String(value.value()),
            _ => return Err(unsupported()),
        },
        Expr::Unary(unary) if matches!(unary.op, UnOp::Neg(_)) => {
            let Expr::Lit(literal) = *unary.expr else {
                return Err(unsupported());
            };
            let Lit::Int(value) = literal.lit else {
                return Err(unsupported());
            };
            let magnitude: i128 = value.base10_parse()?;
            let signed = magnitude.checked_neg().ok_or_else(|| {
                syn::Error::new(value.span(), "scalar integer must fit in signed 64 bits")
            })?;
            ScalarValue::I64(i64::try_from(signed).map_err(|_| {
                syn::Error::new(value.span(), "scalar integer must fit in signed 64 bits")
            })?)
        }
        _ => return Err(unsupported()),
    };
    Ok(SpannedScalar { value, span })
}

/// Parse an integer literal, which has no sign of its own, into the signed
/// scalar domain.
fn parse_positive_i64(value: &LitInt, label: &str) -> syn::Result<i64> {
    let parsed: u128 = value.base10_parse()?;
    i64::try_from(parsed)
        .map_err(|_| syn::Error::new(value.span(), format!("{label} must fit in signed 64 bits")))
}

/// Parse a signed integer expression used by a constraint.
fn parse_i64(expression: Expr, label: &str) -> syn::Result<i64> {
    let scalar = parse_scalar(expression)?;
    match scalar.value {
        ScalarValue::I64(value) => Ok(value),
        _ => Err(syn::Error::new(
            scalar.span,
            format!("`{label}` must be a signed 64-bit integer literal"),
        )),
    }
}

/// Parse a non-negative integer expression used by a length constraint.
fn parse_u64(expression: Expr, label: &str) -> syn::Result<u64> {
    let span = expression.span();
    let Expr::Lit(literal) = expression else {
        return Err(syn::Error::new(
            span,
            format!("`{label}` must be a non-negative integer literal"),
        ));
    };
    let Lit::Int(value) = literal.lit else {
        return Err(syn::Error::new(
            span,
            format!("`{label}` must be a non-negative integer literal"),
        ));
    };
    value.base10_parse::<u64>().map_err(|_| {
        syn::Error::new(
            value.span(),
            format!("`{label}` must fit in an unsigned 64-bit integer"),
        )
    })
}

/// Coarse wire category used to validate field metadata.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FieldKind {
    Bool,
    Integer,
    String,
    List,
    Unknown,
}

/// Classify a field's type, or the item type of an `Option` field.
fn field_kind(ty: &SynType) -> FieldKind {
    if let Some(item) = type_ref::option_item(ty) {
        return field_kind(item);
    }
    if type_ref::vec_item(ty).is_some() {
        return FieldKind::List;
    }
    match type_ref::scalar_name(ty).as_deref() {
        Some("bool") => FieldKind::Bool,
        Some("u8" | "i8" | "u16" | "i16" | "u32" | "i32" | "u64" | "i64") => FieldKind::Integer,
        Some("String") => FieldKind::String,
        _ => FieldKind::Unknown,
    }
}

/// Return the Rust `Default` of an absent `#[serde(default)]` scalar field,
/// which the derive checks against the field's rules. Only a scalar, a
/// string or a list takes rules, so the derive checks every one of them.
/// The derive cannot know what a `default = "path"` function returns.
fn known_default(ty: &SynType, serde: &SerdeField) -> Option<SpannedScalar> {
    let SerdeDefault {
        span,
        function: None,
    } = serde.default.as_ref()?
    else {
        return None;
    };
    let span = *span;
    if type_ref::option_item(ty).is_some() {
        return None;
    }
    let value = match field_kind(ty) {
        FieldKind::Bool => ScalarValue::Bool(false),
        FieldKind::Integer => ScalarValue::I64(0),
        FieldKind::String => ScalarValue::String(String::new()),
        FieldKind::List | FieldKind::Unknown => return None,
    };
    Some(SpannedScalar { value, span })
}

/// Validate `required`, `#[serde(default)]`, and Serde's
/// `skip_serializing_if` together, since each changes when a field may be
/// absent.
fn validate_field_presence(
    field: &Field,
    options: &FieldOptions,
    serde: &SerdeField,
) -> syn::Result<()> {
    if let Some(skip) = serde.skip_serializing_if.as_ref() {
        if options.is_required(field, serde) {
            return Err(syn::Error::new(
                skip.span(),
                "`#[serde(skip_serializing_if = ...)]` cannot be used on a required Bimetal field",
            ));
        }
        if type_ref::option_item(&field.ty).is_none() {
            return Err(syn::Error::new(
                skip.span(),
                "`skip_serializing_if` is supported only on `Option<T>` Bimetal fields",
            ));
        }
        if !matches!(
            skip.value().as_str(),
            "Option::is_none"
                | "std::option::Option::is_none"
                | "core::option::Option::is_none"
                | "::std::option::Option::is_none"
                | "::core::option::Option::is_none"
        ) {
            return Err(syn::Error::new(
                skip.span(),
                "bimetal supports only `Option::is_none` for `skip_serializing_if`",
            ));
        }
        // Serde would read the `None` that it left out as what the function
        // returns, and TypeScript would read it as absent.
        if let Some(SerdeDefault {
            function: Some(function),
            ..
        }) = serde.default.as_ref()
        {
            return Err(syn::Error::new(
                skip.span(),
                format!(
                    "remove `#[serde(skip_serializing_if = ...)]` or `#[serde(default = \"{}\")]`: Serde leaves out a `None`, then reads it back as what the function returns",
                    function.value()
                ),
            ));
        }
    }
    if options.required && type_ref::option_item(&field.ty).is_none() {
        return Err(syn::Error::new(
            field.ty.span(),
            "`required` applies to an `Option` field; a field of any other type is required unless it has a default",
        ));
    }
    // Serde reads an absent `Option` as `None` under `#[serde(default)]`, so
    // only a function can give a `required` field a value that its caller
    // left out.
    if let (true, Some(default)) = (options.required, serde.default.as_ref())
        && let Some(function) = &default.function
    {
        return Err(syn::Error::new(
            default.span,
            format!(
                "a `required` field cannot have `#[serde(default = \"{}\")]`, which the hosts would fill in when it is absent",
                function.value()
            ),
        ));
    }
    Ok(())
}

/// Validate ordering and compatibility of numeric and length ranges.
fn validate_constraint_ranges(
    field: &Field,
    options: &FieldOptions,
    kind: FieldKind,
) -> syn::Result<()> {
    if let (Some(minimum), Some(maximum)) = (options.min_length, options.max_length)
        && minimum > maximum
    {
        return Err(syn::Error::new(
            field.span(),
            "`min_length` cannot exceed `max_length`",
        ));
    }
    if let (Some(minimum), Some(maximum)) = (options.ge, options.le)
        && minimum > maximum
    {
        return Err(syn::Error::new(field.span(), "`ge` cannot exceed `le`"));
    }
    if (options.min_length.is_some() || options.max_length.is_some())
        && !matches!(kind, FieldKind::String | FieldKind::List)
    {
        return Err(syn::Error::new(
            field.ty.span(),
            "`min_length` and `max_length` apply only to string or list fields",
        ));
    }
    if (options.ge.is_some() || options.le.is_some()) && !matches!(kind, FieldKind::Integer) {
        return Err(syn::Error::new(
            field.ty.span(),
            "`ge` and `le` apply only to integer fields",
        ));
    }
    Ok(())
}

/// Validate that the literal suits the field's type, and that the literal
/// and the host default keep the field's other rules.
fn validate_constraint_values(
    options: &FieldOptions,
    kind: FieldKind,
    default: Option<&SpannedScalar>,
) -> syn::Result<()> {
    if let Some(literal) = options.literal.as_ref() {
        let valid = matches!(
            (&literal.value, kind),
            (ScalarValue::Bool(_), FieldKind::Bool)
                | (ScalarValue::I64(_), FieldKind::Integer)
                | (ScalarValue::String(_), FieldKind::String)
        );
        if !valid {
            return Err(syn::Error::new(
                literal.span,
                "the `literal` scalar does not match this field's Rust type",
            ));
        }
        if let Some(default) = default
            && literal.value != default.value
        {
            return Err(syn::Error::new(
                default.span,
                "the field's default, the Rust `Default` that `#[serde(default)]` fills in, differs from its `literal`",
            ));
        }
    }
    let values = [
        ("`literal` value", options.literal.as_ref()),
        (
            "default, the Rust `Default` that `#[serde(default)]` fills in,",
            default,
        ),
    ];
    for (label, scalar) in values {
        let Some(SpannedScalar { value, span }) = scalar else {
            continue;
        };
        let broken = match value {
            // Hosts count a string's length in code points, as Python does.
            ScalarValue::String(value) => {
                let length = value.chars().count() as u64;
                if options.min_length.is_some_and(|minimum| length < minimum) {
                    Some("is shorter than its `min_length`")
                } else if options.max_length.is_some_and(|maximum| length > maximum) {
                    Some("is longer than its `max_length`")
                } else {
                    None
                }
            }
            ScalarValue::I64(value) => {
                if options.ge.is_some_and(|minimum| *value < minimum) {
                    Some("is below its `ge`")
                } else if options.le.is_some_and(|maximum| *value > maximum) {
                    Some("is above its `le`")
                } else {
                    None
                }
            }
            ScalarValue::Bool(_) => None,
        };
        if let Some(broken) = broken {
            return Err(syn::Error::new(
                *span,
                format!("the field's {label} {broken} constraint"),
            ));
        }
    }
    Ok(())
}

/// Render an optional scalar as contract IR construction tokens.
pub(super) fn scalar_option_tokens(value: Option<&SpannedScalar>) -> TokenStream2 {
    match value.map(|value| &value.value) {
        Some(ScalarValue::Bool(value)) => {
            quote!(Some(::bimetal::ir::ScalarValue::Bool(#value)))
        }
        Some(ScalarValue::I64(value)) => {
            quote!(Some(::bimetal::ir::ScalarValue::I64(#value)))
        }
        Some(ScalarValue::String(value)) => {
            quote!(Some(::bimetal::ir::ScalarValue::String(#value.to_owned())))
        }
        None => quote!(None),
    }
}

/// Render an optional number as construction tokens.
pub(super) fn option_tokens(value: Option<impl ToTokens>) -> TokenStream2 {
    value.map_or_else(|| quote!(None), |value| quote!(Some(#value)))
}

#[cfg(test)]
mod tests {
    use syn::parse::Parser;

    use super::*;

    /// Validate a named field, and return the default that its type alone
    /// gives and whether it is required.
    fn presence(tokens: TokenStream2) -> syn::Result<(Option<ScalarValue>, bool)> {
        let field = Field::parse_named
            .parse2(tokens)
            .expect("parse named field");
        let options = FieldOptions::parse(&field.attrs)?;
        let serde = attributes::serde_field(&field.attrs)?;
        options.validate(&field, &serde)?;
        Ok((
            known_default(&field.ty, &serde).map(|default| default.value),
            options.is_required(&field, &serde),
        ))
    }

    #[test]
    fn the_derive_knows_the_default_of_a_scalar_field() {
        let cases = [
            (
                quote!(#[serde(default)] verbose: bool),
                Some(ScalarValue::Bool(false)),
            ),
            (
                quote!(#[serde(default)] limit: u32),
                Some(ScalarValue::I64(0)),
            ),
            (
                quote!(#[serde(default)] prefix: std::string::String),
                Some(ScalarValue::String(String::new())),
            ),
            // Serde reads an absent `Option` as `None` either way.
            (quote!(#[serde(default)] note: Option<String>), None),
            // The derive reads no value for another type's default.
            (quote!(#[serde(default)] tags: Vec<String>), None),
            (quote!(#[serde(default)] limits: Limits), None),
            // Only the compiled library can call a default function.
            (quote!(#[serde(default = "yes")] verbose: bool), None),
            (
                quote!(#[serde(default = "self::label")] note: Option<String>),
                None,
            ),
        ];
        for (tokens, default) in cases {
            assert_eq!(presence(tokens).expect("a valid field"), (default, false));
        }
        assert_eq!(
            presence(quote!(verbose: bool)).expect("a valid field"),
            (None, true)
        );
    }

    #[test]
    fn serde_defaults_must_keep_the_field_rules() {
        let error = |tokens| presence(tokens).expect_err("an invalid field").to_string();

        assert!(
            error(quote!(#[serde(default)] #[bimetal(min_length = 1)] prefix: String))
                .contains("shorter than its `min_length`")
        );
        assert!(
            error(quote!(#[serde(default)] #[bimetal(ge = 1)] count: u32))
                .contains("below its `ge`")
        );
        assert!(
            error(quote!(#[serde(default)] #[bimetal(literal = "v1")] version: String))
                .contains("differs from its `literal`")
        );
        assert!(
            error(quote!(#[serde(default)] #[bimetal(min_length = 1)] tags: Vec<String>))
                .contains("shorter than its `min_length`")
        );
        assert!(
            error(quote!(#[serde(default)] #[bimetal(required)] tags: Vec<String>))
                .contains("`required` applies to an `Option` field")
        );
        assert!(
            error(quote!(#[bimetal(required)] count: u32))
                .contains("`required` applies to an `Option` field")
        );
        // Serde calls the function for an absent `Option` too.
        assert!(
            error(quote!(#[serde(default = "label")] #[bimetal(required)] note: Option<String>))
                .contains("a `required` field cannot have `#[serde(default = \"label\")]`")
        );
    }

    #[test]
    fn a_default_function_takes_no_skip_serializing_if() {
        let error = presence(quote!(
            #[serde(default = "label", skip_serializing_if = "Option::is_none")]
            note: Option<String>
        ))
        .expect_err("an invalid field")
        .to_string();
        assert!(
            error.contains(
                "remove `#[serde(skip_serializing_if = ...)]` or `#[serde(default = \"label\")]`"
            ),
            "{error}"
        );
        // Serde reads the `None` it left out as `None`, the `Default` of an
        // `Option`.
        assert_eq!(
            presence(quote!(
                #[serde(default, skip_serializing_if = "Option::is_none")]
                note: Option<String>
            ))
            .expect("a valid field"),
            (None, false)
        );
    }

    #[test]
    fn the_rules_of_a_default_function_wait_for_the_compiled_library() {
        let cases = [
            quote!(#[serde(default = "empty")] #[bimetal(min_length = 1)] prefix: String),
            quote!(#[serde(default = "empty")] #[bimetal(min_length = 1)] tags: Vec<String>),
            quote!(#[serde(default = "zero")] #[bimetal(ge = 1)] count: u32),
        ];
        for tokens in cases {
            assert_eq!(presence(tokens).expect("a valid field"), (None, false));
        }
    }
}
