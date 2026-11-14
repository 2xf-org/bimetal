//! The contract type reference of a Rust type, and the standard library
//! types that the macros recognize by their paths.

use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::spanned::Spanned;
use syn::visit_mut::VisitMut;
use syn::{GenericArgument, Ident, Path, PathArguments, PathSegment, Type as SynType, TypePath};

use crate::attributes::Boundary;
use crate::types::DirectType;

/// Analyze a Rust type and render its host-neutral contract construction.
///
/// `boundary` is the value's direct boundary, if any; without one, the
/// type's `ContractType` implementation supplies the reference.
pub(crate) fn type_ref_tokens(
    ty: &SynType,
    boundary: Option<Boundary>,
) -> syn::Result<TokenStream2> {
    if let Some(boundary) = boundary {
        return Ok(DirectType::parse(ty, boundary)?.type_ref_tokens());
    }
    // An array that does not cross as bytes crosses as Serde writes it, a
    // tuple of its items, and Serde writes arrays of at most 32 items.
    let value = match ty {
        SynType::Reference(reference) => reference.elem.as_ref(),
        ty => ty,
    };
    if let SynType::Array(array) = value
        && let syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Int(length),
            ..
        }) = &array.len
        && length
            .base10_parse::<usize>()
            .is_ok_and(|length| length > 32)
    {
        return Err(syn::Error::new(
            value.span(),
            "Serde writes arrays of at most 32 items; use a `Vec`",
        ));
    }
    Ok(quote!(<#ty as ::bimetal::ContractType>::type_ref()))
}

/// Replaces `Self` in types with the path of the type it names, for code
/// that renders them where `Self` names another type or nothing. The path
/// keeps the span of `Self`, so errors point where the code says it.
pub(crate) struct SelfToPath<'path>(pub(crate) &'path Path);

impl VisitMut for SelfToPath<'_> {
    fn visit_type_path_mut(&mut self, path: &mut TypePath) {
        if path.qself.is_none() && path.path.is_ident("Self") {
            let span = path.path.span();
            let mut named = self.0.clone();
            for segment in &mut named.segments {
                segment.ident.set_span(span);
            }
            path.path = named;
        }
        syn::visit_mut::visit_type_path_mut(self, path);
    }
}

/// Return the terminal identifier of a named Rust type, such as `Vec` in
/// `std::vec::Vec<u8>`.
pub(crate) fn type_last_ident(ty: &SynType) -> syn::Result<&Ident> {
    let SynType::Path(path) = ty else {
        return Err(syn::Error::new(ty.span(), "expected a named Rust type"));
    };
    path.path
        .segments
        .last()
        .map(|segment| &segment.ident)
        .ok_or_else(|| syn::Error::new(ty.span(), "expected a named Rust type"))
}

/// Return whether a type is `bimetal::Callback<A, R>`, which crosses as a host
/// function rather than through Serde.
pub(crate) fn is_callback(ty: &SynType) -> bool {
    type_last_ident(ty).is_ok_and(|ident| ident == "Callback")
}

/// Return the item type of a `Vec<T>`.
pub(crate) fn vec_item(ty: &SynType) -> Option<&SynType> {
    std_segment(ty, "vec")
        .filter(|segment| segment.ident == "Vec")
        .and_then(single_type_argument)
}

/// Return the item type of an `Option<T>`.
pub(crate) fn option_item(ty: &SynType) -> Option<&SynType> {
    std_segment(ty, "option")
        .filter(|segment| segment.ident == "Option")
        .and_then(single_type_argument)
}

/// Return the name of a type without type arguments, for callers that
/// compare it with the primitives and `String`: a bare name, such as `u8`,
/// `String`, or `Point`, or `std::primitive::u8` or `std::string::String`.
pub(crate) fn scalar_name(ty: &SynType) -> Option<String> {
    let segment = std_segment(ty, "primitive")
        .or_else(|| std_segment(ty, "string").filter(|segment| segment.ident == "String"))?;
    segment
        .arguments
        .is_none()
        .then(|| segment.ident.to_string())
}

/// Return whether a type is `u8`.
pub(crate) fn is_u8(ty: &SynType) -> bool {
    scalar_name(ty).is_some_and(|name| name == "u8")
}

/// Return the bytes boundary for a byte sequence that is a whole value:
/// `Vec<u8>`, `[u8]` or `[u8; N]`, or a shared reference to one.
pub(crate) fn bytes_boundary(ty: &SynType) -> Option<Boundary> {
    let value = match ty {
        SynType::Reference(reference) if reference.mutability.is_none() => reference.elem.as_ref(),
        ty => ty,
    };
    let bytes = match value {
        SynType::Slice(slice) => is_u8(&slice.elem),
        SynType::Array(array) => is_u8(&array.elem),
        value => vec_item(value).is_some_and(is_u8),
    };
    bytes.then_some(Boundary::Bytes)
}

/// Return the last segment of a type that names an item of the standard
/// library's `module` as code usually writes it: alone, such as `Vec<T>`, or
/// through `std`, `core`, or `alloc`, such as `std::vec::Vec<T>`.
fn std_segment<'syntax>(ty: &'syntax SynType, module: &str) -> Option<&'syntax PathSegment> {
    let SynType::Path(path) = ty else {
        return None;
    };
    if path.qself.is_some() {
        return None;
    }
    match path.path.segments.iter().collect::<Vec<_>>().as_slice() {
        [segment] => Some(segment),
        [root, middle, segment]
            if ["std", "core", "alloc"]
                .iter()
                .any(|name| root.ident == name)
                && middle.ident == module
                && root.arguments.is_none()
                && middle.arguments.is_none() =>
        {
            Some(segment)
        }
        _ => None,
    }
}

/// Return the one type argument of a path segment, such as `T` in `Vec<T>`.
fn single_type_argument(segment: &PathSegment) -> Option<&SynType> {
    let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return None;
    };
    match arguments.args.iter().collect::<Vec<_>>().as_slice() {
        [GenericArgument::Type(item)] => Some(item),
        _ => None,
    }
}
