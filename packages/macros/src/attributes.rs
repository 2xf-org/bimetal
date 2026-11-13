//! Strict parsing and normalization of supported Serde and Bimetal attributes.
//!
//! A generated API has one public name and one wire representation. Serde
//! features that imply asymmetric serialization or aliases are rejected here
//! rather than approximated by a host.
//! Doc comments are attributes too, so their normalized text comes from here.

use bimetal_ir::Protocol;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;
use syn::meta::ParseNestedMeta;
use syn::punctuated::Punctuated;
use syn::token::Comma;
use syn::{Attribute, Expr, ExprLit, FnArg, Lit, LitStr, Meta, Path, Token};

/// A direct boundary, which carries a value without Serde.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Boundary {
    /// An Arrow value (`arrow`).
    Arrow,
    /// A numeric buffer (`buffer`).
    Buffer,
    /// Bytes, which a byte sequence that is a whole value crosses as.
    Bytes,
}

impl Boundary {
    /// Every boundary that an attribute declares.
    const ALL: [Self; 2] = [Self::Arrow, Self::Buffer];

    /// Return the boundary that an attribute path names, if any.
    fn from_path(path: &Path) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|boundary| path.is_ident(boundary.name()))
    }

    /// Return the attribute that declares this boundary.
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Arrow => "arrow",
            Self::Buffer => "buffer",
            Self::Bytes => "bytes",
        }
    }
}

/// Fail on the second declaration of the attribute that `meta` reads, when
/// `declared` says that an earlier one set it.
pub(super) fn declare_once(meta: &ParseNestedMeta<'_>, declared: bool) -> syn::Result<()> {
    if !declared {
        return Ok(());
    }
    let name = meta.path.require_ident()?;
    Err(meta.error(format!("`{name}` may be declared only once")))
}

/// Read the direct boundary that `meta` names into `slot`, which takes one.
/// Returns `false`, and leaves `slot`, when `meta` names no boundary in
/// `allowed`.
pub(super) fn declare_boundary(
    meta: &ParseNestedMeta<'_>,
    slot: &mut Option<Boundary>,
    allowed: &[Boundary],
) -> syn::Result<bool> {
    if meta.path.is_ident("bytes") {
        return Err(meta.error(
            "`bytes` needs no attribute: a `Vec<u8>`, `&[u8]` or `[u8; N]` that is a whole parameter, return value or alias crosses as bytes",
        ));
    }
    if meta.path.is_ident("resource") {
        return Err(meta.error(
            "`resource` needs no attribute: a parameter or return value of a resource's type crosses as the resource",
        ));
    }
    let Some(boundary) = Boundary::from_path(&meta.path).filter(|found| allowed.contains(found))
    else {
        return Ok(false);
    };
    if slot.replace(boundary).is_some() {
        return Err(meta.error("only one boundary may be declared"));
    }
    Ok(true)
}

/// Serde's supported container rename rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SerdeRenameRule {
    Lower,
    Upper,
    Pascal,
    Camel,
    Snake,
    ScreamingSnake,
    Kebab,
    ScreamingKebab,
}

impl SerdeRenameRule {
    /// Parse the exact spelling accepted by Serde.
    fn parse(value: &LitStr) -> syn::Result<Self> {
        match value.value().as_str() {
            "lowercase" => Ok(Self::Lower),
            "UPPERCASE" => Ok(Self::Upper),
            "PascalCase" => Ok(Self::Pascal),
            "camelCase" => Ok(Self::Camel),
            "snake_case" => Ok(Self::Snake),
            "SCREAMING_SNAKE_CASE" => Ok(Self::ScreamingSnake),
            "kebab-case" => Ok(Self::Kebab),
            "SCREAMING-KEBAB-CASE" => Ok(Self::ScreamingKebab),
            other => Err(syn::Error::new(
                value.span(),
                format!(
                    "unknown serde rename rule `{other}`; expected lowercase, UPPERCASE, PascalCase, camelCase, snake_case, SCREAMING_SNAKE_CASE, kebab-case, or SCREAMING-KEBAB-CASE"
                ),
            )),
        }
    }
}

/// Apply Serde's variant-oriented rename semantics.
pub(super) fn apply_serde_variant_case(value: &str, rule: Option<SerdeRenameRule>) -> String {
    let Some(rule) = rule else {
        return value.to_owned();
    };
    match rule {
        SerdeRenameRule::Lower => value.to_ascii_lowercase(),
        SerdeRenameRule::Upper => value.to_ascii_uppercase(),
        SerdeRenameRule::Pascal => value.to_owned(),
        SerdeRenameRule::Camel => lowercase_first(value),
        SerdeRenameRule::Snake => {
            let mut snake = String::new();
            for (index, character) in value.char_indices() {
                if index > 0 && character.is_uppercase() {
                    snake.push('_');
                }
                snake.push(character.to_ascii_lowercase());
            }
            snake
        }
        SerdeRenameRule::ScreamingSnake => {
            apply_serde_variant_case(value, Some(SerdeRenameRule::Snake)).to_ascii_uppercase()
        }
        SerdeRenameRule::Kebab => {
            apply_serde_variant_case(value, Some(SerdeRenameRule::Snake)).replace('_', "-")
        }
        SerdeRenameRule::ScreamingKebab => {
            apply_serde_variant_case(value, Some(SerdeRenameRule::ScreamingSnake)).replace('_', "-")
        }
    }
}

/// Apply Serde's field-oriented rename semantics.
pub(super) fn apply_serde_field_case(value: &str, rule: Option<SerdeRenameRule>) -> String {
    let Some(rule) = rule else {
        return value.to_owned();
    };
    match rule {
        SerdeRenameRule::Lower | SerdeRenameRule::Snake => value.to_owned(),
        SerdeRenameRule::Upper | SerdeRenameRule::ScreamingSnake => value.to_ascii_uppercase(),
        SerdeRenameRule::Pascal => {
            let mut pascal = String::new();
            let mut capitalize = true;
            for character in value.chars() {
                if character == '_' {
                    capitalize = true;
                } else if capitalize {
                    pascal.push(character.to_ascii_uppercase());
                    capitalize = false;
                } else {
                    pascal.push(character);
                }
            }
            pascal
        }
        SerdeRenameRule::Camel => {
            let pascal = apply_serde_field_case(value, Some(SerdeRenameRule::Pascal));
            lowercase_first(&pascal)
        }
        SerdeRenameRule::Kebab => value.replace('_', "-"),
        SerdeRenameRule::ScreamingKebab => value.to_ascii_uppercase().replace('_', "-"),
    }
}

/// Lowercase the first Unicode scalar without slicing through UTF-8.
fn lowercase_first(value: &str) -> String {
    let Some(first) = value.chars().next() else {
        return String::new();
    };
    let mut result: String = first.to_lowercase().collect();
    result.push_str(&value[first.len_utf8()..]);
    result
}

/// Supported Serde metadata collected from a model container.
#[derive(Default)]
pub(super) struct SerdeContainer {
    /// Rename rule for fields or enum variants.
    pub(super) rename_all: Option<SerdeRenameRule>,
    /// Rename rule for fields within enum variants.
    pub(super) rename_all_fields: Option<SerdeRenameRule>,
    /// Internal-tag field used by a tagged enum.
    pub(super) tag: Option<String>,
    /// Whether a one-field model is represented as its field directly.
    pub(super) transparent: bool,
}

/// Parse supported Serde container attributes and reject ambiguous features.
pub(super) fn serde_container(attrs: &[Attribute]) -> syn::Result<SerdeContainer> {
    let mut result = SerdeContainer::default();
    let serde_attrs = attrs.iter().filter(|attr| attr.path().is_ident("serde"));
    for attr in serde_attrs {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename_all") {
                declare_once(&meta, result.rename_all.is_some())?;
                result.rename_all =
                    Some(SerdeRenameRule::parse(&single_name(&meta, "rename_all")?)?);
            } else if meta.path.is_ident("rename_all_fields") {
                declare_once(&meta, result.rename_all_fields.is_some())?;
                result.rename_all_fields = Some(SerdeRenameRule::parse(&single_name(
                    &meta,
                    "rename_all_fields",
                )?)?);
            } else if meta.path.is_ident("tag") {
                result.tag = Some(meta.value()?.parse::<LitStr>()?.value());
            } else if meta.path.is_ident("transparent") {
                result.transparent = true;
            } else if meta.path.is_ident("deny_unknown_fields") {
                // The generated hosts are closed by default; this is accepted metadata.
            } else if meta.path.is_ident("rename") {
                single_name(&meta, "rename")?;
            } else {
                return Err(
                    meta.error("unsupported serde container attribute in a Bimetal contract")
                );
            }
            Ok(())
        })?;
    }
    Ok(result)
}

/// Parse a Serde naming value, rejecting the directional form.
fn single_name(meta: &ParseNestedMeta<'_>, attribute: &str) -> syn::Result<LitStr> {
    if !meta.input.peek(Token![=]) {
        return Err(meta.error(format!(
            "directional `{attribute}(serialize = ..., deserialize = ...)` is not supported because Bimetal requires one host name"
        )));
    }
    meta.value()?.parse()
}

/// Parse a field or variant's optional Serde wire-name override.
pub(super) fn serde_rename(attrs: &[Attribute]) -> syn::Result<Option<String>> {
    let mut value = None;
    let serde_attrs = attrs.iter().filter(|attr| attr.path().is_ident("serde"));
    for attr in serde_attrs {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename") {
                declare_once(&meta, value.is_some())?;
                value = Some(single_name(&meta, "rename")?.value());
            } else if meta.path.is_ident("rename_all") {
                return Err(meta.error(
                    "variant-level `rename_all` is not supported; use container `rename_all_fields`",
                ));
            } else if meta.path.is_ident("alias") {
                return Err(meta.error(
                    "`#[serde(alias = ...)]` is not supported because Bimetal exposes one host name",
                ));
            } else {
                return Err(meta.error("unsupported serde field or variant attribute"));
            }
            Ok(())
        })?;
    }
    Ok(value)
}

/// Supported Serde metadata collected from a model field.
#[derive(Default)]
pub(super) struct SerdeField {
    /// Explicit serialized field name.
    pub(super) rename: Option<String>,
    /// Whether this runtime-only field is excluded from the wire model.
    pub(super) skip: bool,
    /// What `#[serde(default)]` or `#[serde(default = "path")]` asks
    /// deserialization to supply for an absent field, if either is declared.
    pub(super) default: Option<SerdeDefault>,
    /// Optional predicate accepted only when it preserves round trips.
    pub(super) skip_serializing_if: Option<LitStr>,
}

/// A field's `#[serde(default)]` or `#[serde(default = "path")]`.
pub(super) struct SerdeDefault {
    /// Where the attribute names `default`.
    pub(super) span: Span,
    /// The path that `default = "path"` names, as written, to the function
    /// that gives the value of an absent field; `None` asks for the Rust
    /// `Default` of its type.
    pub(super) function: Option<LitStr>,
}

/// Parse supported Serde field attributes and reject lossy features.
pub(super) fn serde_field(attrs: &[Attribute]) -> syn::Result<SerdeField> {
    use syn::spanned::Spanned;

    let mut result = SerdeField::default();
    let serde_attrs = attrs.iter().filter(|attr| attr.path().is_ident("serde"));
    for attr in serde_attrs {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename") {
                declare_once(&meta, result.rename.is_some())?;
                result.rename = Some(single_name(&meta, "rename")?.value());
            } else if meta.path.is_ident("skip") {
                declare_once(&meta, result.skip)?;
                result.skip = true;
            } else if meta.path.is_ident("default") {
                declare_once(&meta, result.default.is_some())?;
                let function = if meta.input.peek(Token![=]) {
                    Some(meta.value()?.parse::<LitStr>()?)
                } else {
                    None
                };
                result.default = Some(SerdeDefault {
                    span: meta.path.span(),
                    function,
                });
            } else if meta.path.is_ident("skip_serializing_if") {
                declare_once(&meta, result.skip_serializing_if.is_some())?;
                result.skip_serializing_if = Some(meta.value()?.parse::<LitStr>()?);
            } else if meta.path.is_ident("alias") {
                return Err(meta.error(
                    "`#[serde(alias = ...)]` is not supported because Bimetal exposes one host name",
                ));
            } else {
                return Err(meta.error("unsupported serde field attribute"));
            }
            Ok(())
        })?;
    }
    Ok(result)
}

/// Reject `#[bimetal(...)]` on a model type, which takes none.
pub(super) fn reject_type_attributes(attrs: &[Attribute]) -> syn::Result<()> {
    match attrs.iter().find(|attr| attr.path().is_ident("bimetal")) {
        Some(attr) => Err(syn::Error::new_spanned(
            attr,
            "a model type takes no `#[bimetal(...)]` attribute; field rules go on its fields, and a `#[serde(transparent)]` struct is already its field's type to the hosts",
        )),
        None => Ok(()),
    }
}

/// Return the `#[cfg(...)]` attributes of an exported method, each of which
/// tests only features. Every host of one build compiles with the same
/// features, so the method is in all of their contracts or in none; the
/// hosts differ by target, so a target cannot gate a method.
pub(super) fn feature_gates(attrs: &[Attribute]) -> syn::Result<Vec<Attribute>> {
    let mut gates = Vec::new();
    for attr in attrs {
        if attr.path().is_ident("cfg") {
            reject_target_predicate(&attr.parse_args::<Meta>()?)?;
            gates.push(attr.clone());
        } else if attr.path().is_ident("cfg_attr") {
            reject_conditional_gate(&attr.meta)?;
        }
    }
    Ok(gates)
}

/// Reject a `cfg` among the attributes that a `cfg_attr` adds, also inside a
/// nested `cfg_attr`. Rustc strips such a method after Bimetal reads it, so
/// the contract would list a method that no host has.
fn reject_conditional_gate(cfg_attr: &Meta) -> syn::Result<()> {
    let Meta::List(list) = cfg_attr else {
        return Ok(());
    };
    let nested = list.parse_args_with(Punctuated::<Meta, Comma>::parse_terminated)?;
    for added in nested.iter().skip(1) {
        if added.path().is_ident("cfg") {
            return Err(syn::Error::new_spanned(
                added,
                "write the gate of an exported method as `#[cfg(feature = \"...\")]`, not inside `#[cfg_attr(...)]`, so that Bimetal sees which builds keep the method",
            ));
        }
        if added.path().is_ident("cfg_attr") {
            reject_conditional_gate(added)?;
        }
    }
    Ok(())
}

/// Reject a `cfg` predicate unless each of its leaves is `feature = "..."`,
/// joined by `all`, `any` and `not`.
fn reject_target_predicate(predicate: &Meta) -> syn::Result<()> {
    match predicate {
        Meta::NameValue(pair)
            if pair.path.is_ident("feature")
                && matches!(
                    &pair.value,
                    Expr::Lit(ExprLit {
                        lit: Lit::Str(_),
                        ..
                    })
                ) =>
        {
            Ok(())
        }
        Meta::List(list)
            if ["all", "any", "not"]
                .iter()
                .any(|name| list.path.is_ident(name)) =>
        {
            let nested = list.parse_args_with(Punctuated::<Meta, Comma>::parse_terminated)?;
            if nested.is_empty() && list.path.is_ident("any") {
                return Err(syn::Error::new_spanned(
                    list,
                    "`any()` holds in no build, so it tests no feature; name the features it tests, or remove it",
                ));
            }
            nested.iter().try_for_each(reject_target_predicate)
        }
        other => Err(syn::Error::new_spanned(
            other,
            "an exported method may be gated only on features: each leaf of its `#[cfg]` must be `feature = \"...\"`, joined by `all`, `any` or `not`, because every host of one build has the same features but not the same target; put a target's `#[cfg]` inside its body and return an error where it cannot run, or mark it `#[bimetal(skip)]`",
        )),
    }
}

/// What an exported parameter's attributes declare.
#[derive(Default)]
pub(super) struct ParameterOptions {
    /// Its direct boundary, if any.
    pub(super) boundary: Option<Boundary>,
    /// The literal that hosts pass when the caller omits the argument.
    pub(super) default: Option<Expr>,
}

/// Parse an exported parameter's attributes: a direct boundary, `arrow` or
/// `buffer`, and `default = <expression>`.
pub(super) fn parameter_options(attrs: &[Attribute]) -> syn::Result<ParameterOptions> {
    let mut options = ParameterOptions::default();
    let bimetal_attrs = attrs.iter().filter(|attr| attr.path().is_ident("bimetal"));
    for attr in bimetal_attrs {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("default") {
                declare_once(&meta, options.default.is_some())?;
                options.default = Some(meta.value()?.parse()?);
                Ok(())
            } else if declare_boundary(&meta, &mut options.boundary, &Boundary::ALL)? {
                Ok(())
            } else {
                Err(meta.error("parameter attributes are arrow, buffer, or default = <expression>"))
            }
        })?;
    }
    Ok(options)
}

/// Remove Bimetal attributes from a callable and its parameters before the
/// Rust item is re-emitted.
pub(super) fn strip_bimetal_attributes(
    attrs: &mut Vec<Attribute>,
    inputs: &mut Punctuated<FnArg, Comma>,
) {
    attrs.retain(|attr| !attr.path().is_ident("bimetal"));
    for argument in inputs {
        if let FnArg::Typed(argument) = argument {
            argument
                .attrs
                .retain(|attr| !attr.path().is_ident("bimetal"));
        }
    }
}

/// Options attached to an exported function or resource method.
#[derive(Default)]
pub(super) struct ExportOptions {
    /// Whether a resource method is a constructor, which becomes the class
    /// constructor or a named factory.
    pub(super) constructor: bool,
    /// Whether a resource method is omitted from the host API.
    pub(super) skip: bool,
    /// Optional direct return boundary (`arrow` or `buffer`).
    pub(super) returns: Option<Boundary>,
    /// The host language feature that a resource method implements.
    pub(super) protocol: Option<Protocol>,
    /// Whether a synchronous call is brief and never waits, so Python keeps
    /// its interpreter lock through it.
    pub(super) brief: bool,
}

/// The kind of exported callable whose options are parsed, which decides the
/// options it accepts.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CallableKind {
    /// A free function, which accepts only `returns(...)`.
    Function,
    /// A resource method, which also accepts `constructor`, `skip`, and the
    /// protocols.
    ResourceMethod,
}

/// Parse the Bimetal attributes of an exported callable.
pub(super) fn export_options(
    attrs: &[Attribute],
    kind: CallableKind,
) -> syn::Result<ExportOptions> {
    let method = kind == CallableKind::ResourceMethod;
    let mut options = ExportOptions::default();
    let bimetal_attrs = attrs.iter().filter(|attr| attr.path().is_ident("bimetal"));
    for attr in bimetal_attrs {
        attr.parse_nested_meta(|meta| {
            if method && meta.path.is_ident("constructor") {
                declare_once(&meta, options.constructor)?;
                options.constructor = true;
            } else if method && meta.path.is_ident("skip") {
                declare_once(&meta, options.skip)?;
                options.skip = true;
            } else if meta.path.is_ident("brief") {
                declare_once(&meta, options.brief)?;
                options.brief = true;
            } else if meta.path.is_ident("returns") {
                meta.parse_nested_meta(|boundary| {
                    if declare_boundary(&boundary, &mut options.returns, &Boundary::ALL)? {
                        Ok(())
                    } else {
                        Err(boundary.error("return boundary must be arrow or buffer"))
                    }
                })?;
            } else if method
                && let Some(protocol) = Protocol::ALL
                    .into_iter()
                    .find(|protocol| meta.path.is_ident(protocol.attribute()))
            {
                if options.protocol.replace(protocol).is_some() {
                    return Err(meta.error("a method implements one protocol at most"));
                }
            } else if method {
                return Err(meta.error(
                    "method attributes are constructor, skip, brief, returns(arrow|buffer), or a protocol: enter, exit, next, getter, setter, len, repr, display, getitem, setitem, delitem, contains, eq, hash, bool, or call",
                ));
            } else {
                return Err(meta.error(
                    "function attributes are brief and returns(arrow|buffer)",
                ));
            }
            Ok(())
        })?;
    }
    Ok(options)
}

/// Render normalized Rust documentation as an optional owned string expression.
pub(super) fn docs_tokens(attrs: &[Attribute]) -> TokenStream2 {
    match docs_text(attrs) {
        Some(docs) => quote!(Some(#docs.to_owned())),
        None => quote!(None),
    }
}

/// Extract the lines of the `#[doc = ...]` attributes, without trailing
/// whitespace and without leading and trailing blank lines, or `None` when
/// there is no text.
///
/// Every line break of Python or JavaScript source ends a line here, a lone
/// `\r` and the Unicode line and paragraph separators included, so that the
/// generators can put each line in a comment.
pub(super) fn docs_text(attrs: &[Attribute]) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    for text in attrs.iter().filter_map(doc_text) {
        let text = text.replace("\r\n", "\n");
        // The space that follows `///`.
        let text = text.strip_prefix(' ').unwrap_or(&text);
        lines.extend(
            text.split(['\n', '\r', '\u{2028}', '\u{2029}'])
                .map(|line| line.trim_end().to_owned()),
        );
    }
    let first = lines.iter().position(|line| !line.is_empty())?;
    let last = lines.iter().rposition(|line| !line.is_empty())?;
    Some(lines[first..=last].join("\n"))
}

/// Return the text of a `#[doc = "..."]` attribute, or `None` for any other
/// attribute.
fn doc_text(attr: &Attribute) -> Option<String> {
    if !attr.path().is_ident("doc") {
        return None;
    }
    let Meta::NameValue(name_value) = &attr.meta else {
        return None;
    };
    let Expr::Lit(literal) = &name_value.value else {
        return None;
    };
    let Lit::Str(text) = &literal.lit else {
        return None;
    };
    Some(text.value())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_line_break_in_docs_becomes_a_newline() {
        let attrs: Vec<Attribute> = vec![
            syn::parse_quote!(#[doc = " Summary.\rimport os  "]),
            syn::parse_quote!(#[doc = " Windows\r\nline\u{2028}separated\u{2029}paragraph"]),
            syn::parse_quote!(#[doc = ""]),
        ];

        assert_eq!(
            docs_text(&attrs).as_deref(),
            Some("Summary.\nimport os\nWindows\nline\nseparated\nparagraph")
        );
    }

    #[test]
    fn camel_case_handles_empty_and_unicode_names_without_panicking() {
        assert_eq!(lowercase_first(""), "");
        assert_eq!(lowercase_first("Éclair"), "éclair");
        assert_eq!(
            apply_serde_variant_case("HTTPServer", Some(SerdeRenameRule::Camel)),
            "hTTPServer"
        );
    }
}
