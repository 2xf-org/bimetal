//! The trait behind every `#[derive(bimetal::Error)]`.

use std::fmt::Display;

use crate::ir::DefinitionId;

/// A Rust error with a stable code for generated host exceptions.
pub trait ContractError: Display {
    /// Return the identity registered by the error derive.
    fn type_identity() -> DefinitionId
    where
        Self: Sized;

    /// Return the stable code for this error value.
    fn code(&self) -> &'static str;

    /// Return the fields of this error value as JSON, when it declares
    /// `#[bimetal(details)]`.
    #[doc(hidden)]
    fn details(&self) -> Option<String> {
        None
    }
}
