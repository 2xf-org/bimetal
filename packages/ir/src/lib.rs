//! Stable, host-neutral contract consumed by the package generators.
//!
//! The IR contains only information required to validate and render public
//! Python and TypeScript APIs, without generator state or backend-specific
//! syntax. All collections are sorted before publication so serialized
//! contracts and generated packages remain deterministic.

#![deny(missing_docs, rustdoc::broken_intra_doc_links)]
#![forbid(unsafe_op_in_unsafe_fn)]

mod disambiguate;
pub mod discovery;
pub mod hash;
mod manifest;
mod namespace;
pub mod python_names;
pub mod typescript_names;

pub use manifest::{
    ArrowKind, BufferElement, CallableDef, CargoPackageId, ConstantDef, DefinitionId,
    EnumVariantDef, ErrorCode, ErrorDef, ExportKind, ExternalVariantDef, FieldConstraints,
    FieldDef, FunctionDef, Manifest, MethodDef, ParamDef, Protocol, ResourceDef, ScalarValue,
    TypeDef, TypeRef, TypeShape, VariantPayload,
};
pub use namespace::{Namespace, NamespaceRule};
