//! The contract type reference of a Rust type, and the standard library
//! types that the macros recognize by their paths.

use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::spanned::Spanned;
use syn::{GenericArgument, Ident, PathArguments, PathSegment, Type as SynType};

use crate::attributes::Boundary;
use crate::types::DirectType;

/// Analyze a Rust type and render its host-neutral contract construction.
///
/// `boundary` is the direct boundary that an attribute declares for the
/// value, if any; without one, the type's `ContractType` implementation
/// supplies the reference.
pub(crate) fn type_ref_tokens(
    ty: &SynType,
    boundary: Option<Boundary>,
) -> syn::Result<TokenStream2> {
    if let Some(boundary) = boundary {
        return Ok(DirectType::parse(ty, boundary)?.type_ref_tokens());
    }
    // Without `bytes`, an array crosses as Serde writes it, a tuple of its
    // items, as a `Vec<u8>` crosses as a list of integers. Serde writes
    // arrays of at most 32 items.
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
        let message = if is_u8(&array.elem) {
            "Serde writes arrays of at most 32 items; take a longer byte array with `#[bimetal(bytes)]`, return it with `#[bimetal(returns(bytes))]`, or use a `Vec<u8>`"
        } else {
            "Serde writes arrays of at most 32 items; use a `Vec`"
        };
        return Err(syn::Error::new(value.span(), message));
    }
    Ok(quote!(<#ty as ::bimetal::ContractType>::type_ref()))
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
