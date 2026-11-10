//! Expansion of resource exports: a Rust value that keeps state between host
//! calls. Every host stores it in a `bimetal::runtime::Resource`, which every
//! call shares, so async calls outlive the call that started them. This module
//! classifies the members once and registers the contract; `python`, `node`,
//! and `wasm` emit the host classes.

use bimetal_ir::typescript_names::Callable;
use bimetal_ir::{ExportKind, Protocol};
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::ext::IdentExt;
use syn::spanned::Spanned;
use syn::{Ident, ImplItem, ImplItemFn, ItemImpl, ReceiverKind, Type as SynType, Visibility};

use crate::attributes::{self, CallableKind};
use crate::export::host_backend::{HostBackend, Labels};
use crate::export::wrapper::{self, Access, Wrapper};
use crate::export::{node, python, wasm};
use crate::registration;
use crate::types::{self, Signature};

/// Expand an inherent impl block into resource metadata and every host's
/// class.
pub(super) fn expand_resource(mut item: ItemImpl) -> syn::Result<TokenStream2> {
    if item.trait_.is_some() {
        return Err(syn::Error::new(
            item.impl_token.span,
            "only inherent impl blocks can be exported as resources",
        ));
    }
    types::reject_generics(&item.generics, item.impl_token.span)?;
    let docs = attributes::docs_tokens(&item.attrs);
    let resource_ty: &SynType = &item.self_ty;
    let resource_ident = types::type_last_ident(resource_ty)?;
    let resource_name = resource_ident.unraw().to_string();
    let native_name =
        types::native_export_name(resource_ident.span(), ExportKind::Resource, &resource_name);
    let plan = ResourcePlan::new(&item)?;
    let definition = |member: &Member<'_>| {
        let docs = attributes::docs_tokens(&member.method.attrs);
        member.signature.callable_tokens(&docs)
    };
    let constructor = match &plan.constructor {
        Some(member) => {
            let constructor = definition(member);
            quote!(Some(#constructor))
        }
        None => quote!(None),
    };
    let factories = plan.factories.iter().map(definition);
    let methods = plan.methods.iter().map(|member| {
        let callable = definition(member);
        let returns = &member.signature.returns.type_ref;
        let protocol = match member.protocol {
            Some(protocol) => {
                let variant = format_ident!("{}", protocol.variant());
                quote!(::core::option::Option::Some(::bimetal::ir::Protocol::#variant))
            }
            None => quote!(::core::option::Option::None),
        };
        quote!(::bimetal::ir::MethodDef {
            callable: #callable,
            returns: #returns,
            protocol: #protocol,
        })
    });
    let send_checks: TokenStream2 = plan
        .members()
        .map(|member| {
            let ident = &member.method.sig.ident;
            wrapper::send_check(&member.method.sig, &quote!(<#resource_ty>::#ident))
        })
        .collect();
    let python_wrapper = python::resource(&plan, resource_ty, &resource_name, &native_name);
    let node_wrapper = node::resource(&plan, resource_ty, &resource_name, &native_name);
    let wasm_wrapper = wasm::resource(&plan, resource_ty, &resource_name, &native_name);
    let origin = registration::origin();
    let id = quote!(concat!(module_path!(), "::", #resource_name));
    let registration = registration::submit(
        "ResourceRegistration",
        &quote!(::bimetal::ir::ResourceDef),
        &quote! {
            ::bimetal::ir::ResourceDef {
                #origin
                id: #id.to_owned(),
                name: #resource_name.to_owned(),
                native_name: #native_name.to_owned(),
                docs: #docs,
                constructor: #constructor,
                factories: vec![#(#factories),*],
                methods: vec![#(#methods),*],
            }
        },
    );
    // The plan borrows `item`, so strip the Bimetal attributes only after the
    // plan's last use.
    for impl_item in &mut item.items {
        if let ImplItem::Fn(method) = impl_item {
            attributes::strip_bimetal_attributes(&mut method.attrs, &mut method.sig.inputs);
        }
    }
    Ok(quote! {
        #item
        impl ::bimetal::runtime::ResourceType for #resource_ty {
            fn identity() -> ::bimetal::ir::DefinitionId {
                ::bimetal::ir::DefinitionId::new(env!("CARGO_PKG_NAME"), #id)
            }
        }
        #registration
        #send_checks
        #python_wrapper
        #node_wrapper
        #wasm_wrapper
    })
}

/// The exported members of one resource, classified once for every host.
pub(super) struct ResourcePlan<'item> {
    /// The class constructor: the synchronous `new`, or else the first
    /// synchronous constructor. Host constructors cannot be async.
    constructor: Option<Member<'item>>,
    /// Every other constructor, exposed as a named factory.
    factories: Vec<Member<'item>>,
    /// Exported instance methods, in declaration order.
    methods: Vec<Member<'item>>,
}

impl<'item> ResourcePlan<'item> {
    /// Validate and classify the public, non-skipped functions of an impl block.
    fn new(item: &'item ItemImpl) -> syn::Result<Self> {
        let mut constructors = Vec::new();
        let mut methods = Vec::new();
        for impl_item in &item.items {
            let ImplItem::Fn(method) = impl_item else {
                continue;
            };
            if !matches!(method.vis, Visibility::Public(_)) {
                continue;
            }
            let options = attributes::export_options(&method.attrs, CallableKind::ResourceMethod)?;
            if options.skip {
                continue;
            }
            if let Some(cfg) = method.attrs.iter().find(|attr| attr.path().is_ident("cfg")) {
                return Err(syn::Error::new_spanned(
                    cfg,
                    "an exported method must exist on every host, because Bimetal reads one contract for them all; put the `#[cfg]` inside its body, and return an error where it cannot run, or mark it `#[bimetal(skip)]`",
                ));
            }
            let signature = Signature::parse(&method.sig, options.returns)?;
            if options.constructor {
                if options.returns.is_some() {
                    return Err(syn::Error::new(
                        method.sig.span(),
                        "resource constructors cannot declare a return boundary",
                    ));
                }
                if method.sig.receiver().is_some() {
                    return Err(syn::Error::new(
                        method.sig.span(),
                        "a resource constructor cannot take self",
                    ));
                }
                if options.protocol.is_some() {
                    return Err(syn::Error::new(
                        method.sig.span(),
                        "a resource constructor cannot implement a protocol",
                    ));
                }
                constructors.push(Member {
                    method,
                    signature,
                    role: Role::Factory,
                    protocol: None,
                });
            } else {
                types::reject_reserved_resource_method(method)?;
                methods.push(Member {
                    method,
                    signature,
                    role: Role::Method(method_access(method)?),
                    protocol: options.protocol,
                });
            }
        }
        let primary = constructors
            .iter()
            .position(|member| !member.signature.is_async && member.method.sig.ident == "new")
            .or_else(|| {
                constructors
                    .iter()
                    .position(|member| !member.signature.is_async)
            });
        let constructor = primary.map(|index| Member {
            role: Role::Constructor,
            ..constructors.remove(index)
        });
        Ok(Self {
            constructor,
            factories: constructors,
            methods,
        })
    }

    /// Return every member: the constructor, the factories, then the methods.
    pub(super) fn members(&self) -> impl Iterator<Item = &Member<'item>> {
        self.constructor
            .iter()
            .chain(&self.factories)
            .chain(&self.methods)
    }
}

/// Select read or write access from an exported method's receiver.
fn method_access(method: &ImplItemFn) -> syn::Result<Access> {
    let receiver = method.sig.receiver().ok_or_else(|| {
        syn::Error::new(
            method.sig.span(),
            "exported resource methods must take `&self`, `&mut self`, or `self`",
        )
    })?;
    match &receiver.kind {
        ReceiverKind::Reference(_, _, None) => Ok(Access::Read),
        ReceiverKind::Reference(_, _, Some(_)) => Ok(Access::Write),
        ReceiverKind::Value => Ok(Access::Take),
        _ => Err(syn::Error::new(
            receiver.span(),
            "exported resource methods must take `&self`, `&mut self`, or `self`",
        )),
    }
}

/// What an exported member is to the hosts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Role {
    /// The class constructor.
    Constructor,
    /// A constructor exposed as a named factory.
    Factory,
    /// An instance method, which reaches the value with this access.
    Method(Access),
}

impl Role {
    /// Return the role as the name of a generated member spells it.
    const fn name(self) -> &'static str {
        match self {
            Self::Constructor => "constructor",
            Self::Factory => "factory",
            Self::Method(_) => "method",
        }
    }
}

/// One exported constructor or method.
pub(super) struct Member<'item> {
    /// The Rust function, borrowed from the exported impl block.
    method: &'item ImplItemFn,
    /// The function's signature, checked once for every host.
    pub(super) signature: Signature<'item>,
    /// What the member is to the hosts.
    pub(super) role: Role,
    /// The host language feature that a method implements.
    pub(super) protocol: Option<Protocol>,
}

impl Member<'_> {
    /// Return this member as the host's callers write it, such as
    /// `Counter.add`, `Counter` for the Python class constructor, and
    /// `new Counter` for the JavaScript one.
    fn callable(&self, resource_name: &str, labels: Labels) -> String {
        match (labels, self.role) {
            (Labels::Rust, Role::Constructor) => resource_name.to_owned(),
            (Labels::Rust, _) => format!("{resource_name}.{}", labels.name(&self.signature)),
            (Labels::Host, Role::Constructor) => Callable::Constructor(resource_name).to_string(),
            (Labels::Host, _) => Callable::Member {
                class: resource_name,
                name: labels.name(&self.signature),
            }
            .to_string(),
        }
    }

    /// Plan this member's `backend` wrapper.
    pub(super) fn wrapper(
        &self,
        resource_ty: &SynType,
        resource_name: &str,
        backend: HostBackend,
    ) -> Wrapper {
        let ident = &self.method.sig.ident;
        let (access, callee) = match self.role {
            Role::Constructor | Role::Factory => {
                (Access::Construct, quote!(<#resource_ty>::#ident))
            }
            Role::Method(access) => (access, quote!(__bimetal_inner.#ident)),
        };
        let callable = self.callable(resource_name, backend.labels());
        wrapper::plan(&self.signature, backend, access, &callee, &callable)
    }

    /// Name the generated member after its role and Rust name, so the class
    /// constructor, a factory, and a method of the same name never collide.
    pub(super) fn ident(&self) -> Ident {
        format_ident!(
            "__bimetal_{}_{}",
            self.role.name(),
            self.signature.rust_name
        )
    }

    /// Return the receiver of the generated member, which a method has.
    pub(super) fn receiver(&self) -> Option<TokenStream2> {
        matches!(self.role, Role::Method(_)).then(|| quote!(&self,))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_constructor_collection_preserves_attribute_errors() {
        let implementation: ItemImpl = syn::parse_quote! {
            impl Resource {
                #[bimetal(unknown)]
                pub fn new() -> Self {
                    Self
                }
            }
        };

        let error = match ResourcePlan::new(&implementation) {
            Ok(_) => panic!("invalid constructor attributes must be reported"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("method attributes are"));
    }

    #[test]
    fn exported_methods_cannot_be_conditional() {
        let implementation: ItemImpl = syn::parse_quote! {
            impl Resource {
                #[bimetal(constructor)]
                pub fn new() -> Self { Self }
                #[cfg(not(target_arch = "wasm32"))]
                pub fn open(&self) {}
            }
        };

        let error = match ResourcePlan::new(&implementation) {
            Ok(_) => panic!("a conditional export must be reported"),
            Err(error) => error,
        };

        assert!(
            error
                .to_string()
                .contains("put the `#[cfg]` inside its body")
        );
    }

    #[test]
    fn a_resource_may_have_no_constructor() {
        let implementation: ItemImpl = syn::parse_quote! {
            impl Resource {
                pub fn read(&self) -> u32 { 0 }
                pub fn finish(self) -> u32 { 0 }
            }
        };
        let plan = ResourcePlan::new(&implementation).expect("plan resource");

        assert!(plan.constructor.is_none());
        let roles: Vec<Role> = plan.methods.iter().map(|member| member.role).collect();
        assert_eq!(
            roles,
            [Role::Method(Access::Read), Role::Method(Access::Take)]
        );
    }

    #[test]
    fn the_class_constructor_is_the_first_synchronous_one() {
        let implementation: ItemImpl = syn::parse_quote! {
            impl Resource {
                #[bimetal(constructor)]
                pub async fn new() -> Self { Self }
                #[bimetal(constructor)]
                pub fn open() -> Self { Self }
                pub fn read(&self) -> u32 { 0 }
                pub async fn write(&mut self) {}
            }
        };
        let plan = ResourcePlan::new(&implementation).expect("plan resource");

        let constructor = plan
            .constructor
            .as_ref()
            .expect("a synchronous constructor");
        assert_eq!(constructor.method.sig.ident, "open");
        assert_eq!(constructor.role, Role::Constructor);
        assert_eq!(plan.factories.len(), 1);
        assert_eq!(plan.factories[0].method.sig.ident, "new");
        let roles: Vec<Role> = plan.methods.iter().map(|member| member.role).collect();
        assert_eq!(
            roles,
            [Role::Method(Access::Read), Role::Method(Access::Write)]
        );
    }
}
