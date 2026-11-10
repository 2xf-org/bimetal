//! Checks that reject exported declarations Bimetal cannot represent: private
//! items, generics, and method names that generated code reserves for
//! itself.

use bimetal_ir::ResourceDef;
use proc_macro2::Span;
use syn::ext::IdentExt;
use syn::{Generics, ImplItemFn, Visibility};

/// Require an exported declaration to be public.
pub(crate) fn ensure_public(visibility: &Visibility, span: Span) -> syn::Result<()> {
    if matches!(visibility, Visibility::Public(_)) {
        Ok(())
    } else {
        Err(syn::Error::new(
            span,
            "exported Bimetal items must be public",
        ))
    }
}

/// Reject generic exported declarations, whose host shape is not closed.
pub(crate) fn reject_generics(generics: &Generics, span: Span) -> syn::Result<()> {
    if generics.params.is_empty() && generics.where_clause.is_none() {
        Ok(())
    } else {
        Err(syn::Error::new(
            span,
            "generic Bimetal contracts are not supported",
        ))
    }
}

/// Reserve the lifecycle member names that generated resource wrappers
/// implement: `close`, `closed`, and `free`, which wasm-bindgen adds to every
/// class.
pub(crate) fn reject_reserved_resource_method(method: &ImplItemFn) -> syn::Result<()> {
    let name = method.sig.ident.unraw().to_string();
    if [ResourceDef::CLOSE, ResourceDef::CLOSED, "free"].contains(&name.as_str()) {
        return Err(syn::Error::new(
            method.sig.ident.span(),
            format!("`{name}` is reserved for generated resource lifecycle behavior"),
        ));
    }
    Ok(())
}
