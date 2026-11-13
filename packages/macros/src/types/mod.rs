//! Rust type analysis and host-neutral contract token generation.
//!
//! The leaves check exported declarations once, for the contract and for
//! the wrappers of every host: a callable's signature (`callable`, with
//! `result_types`), a value that crosses a direct boundary (`direct`), a
//! model field (`field`, with `field_options`), and the declarations that
//! Bimetal cannot represent (`validation`). They also render the contract of a
//! type (`type_ref`) and derive deterministic native export names
//! (`native_name`). Unsupported shapes fail closed with errors attached to
//! the originating syntax span.

mod callable;
mod direct;
mod field;
mod field_options;
mod native_name;
mod result_types;
mod type_ref;
mod validation;

pub(crate) use callable::{Param, ParamValue, ResourceShape, Returns, Signature};
pub(crate) use direct::DirectType;
pub(crate) use field::{ModelField, field_bindings, field_tokens};
pub(crate) use field_options::FieldOptions;
pub(crate) use native_name::native_export_name;
pub(crate) use type_ref::{SelfToPath, bytes_boundary, type_last_ident, type_ref_tokens};
pub(crate) use validation::{ensure_public, reject_generics, reject_reserved_resource_method};
