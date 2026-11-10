//! Expansion of constant and static exports. Both register their value as
//! JSON, serialized when the contract is built, and need no host wrapper.

use proc_macro2::TokenStream as TokenStream2;
use quote::{ToTokens, quote};
use syn::{Attribute, Ident, Type as SynType, Visibility};

use crate::attributes;
use crate::registration;
use crate::types;

/// Re-emit `item`, a public constant or static with these parts, with the
/// lazy contract registration of its value, which hosts read under the Rust
/// name.
pub(super) fn expand(
    item: &impl ToTokens,
    vis: &Visibility,
    ident: &Ident,
    ty: &SynType,
    attrs: &[Attribute],
) -> syn::Result<TokenStream2> {
    use syn::ext::IdentExt;

    types::ensure_public(vis, ident.span())?;
    let docs = attributes::docs_tokens(attrs);
    let host_name = ident.unraw().to_string();
    let type_ref = types::type_ref_tokens(ty, None)?;
    let origin = registration::origin();
    let registration = registration::submit(
        "ConstantRegistration",
        &quote!(::std::result::Result<::bimetal::ir::ConstantDef, ::std::string::String>),
        &quote! {
            let __bimetal_type = #type_ref;
            ::bimetal::ContractType::check_rules(&self::#ident).map_err(|__bimetal_violation| {
                ::std::format!("constant `{}` breaks a field rule: {__bimetal_violation}", #host_name)
            })?;
            let __bimetal_value = ::bimetal::__private::serde_json::to_value(&self::#ident).map_err(|__bimetal_error| {
                ::std::format!("constant `{}` could not serialize as JSON: {__bimetal_error}", #host_name)
            })?;
            ::std::result::Result::Ok(::bimetal::ir::ConstantDef {
                #origin
                host_name: #host_name.to_owned(),
                docs: #docs,
                ty: __bimetal_type,
                value: __bimetal_value,
            })
        },
    );
    Ok(quote! {
        #item
        #registration
    })
}
