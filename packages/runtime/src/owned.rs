//! The owned value that a borrowed parameter of an export decodes into.

use std::path::{Path, PathBuf};

/// What an exported function's `&Self` parameter decodes into, and lends: a
/// `String` for `str`, a `Vec<T>` for `[T]`, a `PathBuf` for `Path`, and a
/// sized type itself. The type system picks it, so a type of the
/// application's that shares a standard name keeps its own.
#[diagnostic::on_unimplemented(
    message = "an exported parameter cannot borrow `{Self}`, which has no owned value to decode",
    note = "take it by value, or as a `&str`, a `&[T]`, a `&Path`, or a reference to a sized type"
)]
pub trait Owned {
    /// The owned type, whose reference coerces to `&Self`.
    type Owned;
}

impl<T> Owned for T {
    type Owned = T;
}

impl Owned for str {
    type Owned = String;
}

impl<T> Owned for [T] {
    type Owned = Vec<T>;
}

impl Owned for Path {
    type Owned = PathBuf;
}
