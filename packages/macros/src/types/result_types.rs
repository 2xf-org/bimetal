//! The success and error types of a callable that returns `Result`.

use syn::spanned::Spanned;
use syn::{GenericArgument, PathArguments, ReturnType, Type as SynType, TypePath};

/// The type arguments of a `Result<T, E>` return, whose error type supplies
/// the callable's typed-error contract.
#[derive(Clone, Copy)]
pub(crate) struct ResultTypes<'syntax> {
    /// The success type, `T`.
    pub(crate) ok: &'syntax SynType,
    /// The error type, `E`, which hosts identify through its `ContractError`
    /// implementation.
    pub(crate) error: &'syntax SynType,
}

impl<'syntax> ResultTypes<'syntax> {
    /// Return the type arguments when `output` names a type whose last path
    /// segment is `Result`, and `None` for any other return.
    ///
    /// The match is by name, so an alias such as `io::Result<T>`, which
    /// declares one type argument, is an error rather than a plain value.
    pub(crate) fn resolve(output: &'syntax ReturnType) -> syn::Result<Option<Self>> {
        let ReturnType::Type(_, ty) = output else {
            return Ok(None);
        };
        let SynType::Path(TypePath { path, .. }) = ty.as_ref() else {
            return Ok(None);
        };
        let Some(segment) = path.segments.last() else {
            return Ok(None);
        };
        if segment.ident != "Result" {
            return Ok(None);
        }
        let types: Vec<&SynType> = match &segment.arguments {
            PathArguments::AngleBracketed(arguments) => arguments
                .args
                .iter()
                .filter_map(|argument| match argument {
                    GenericArgument::Type(ty) => Some(ty),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        let &[ok, error] = types.as_slice() else {
            return Err(syn::Error::new(
                segment.span(),
                "Result must declare its success and error types",
            ));
        };
        Ok(Some(Self { ok, error }))
    }
}
