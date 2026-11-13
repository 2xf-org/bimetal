//! The values that cross a direct boundary (bytes, `buffer` or `arrow`)
//! instead of Serde, with their shapes checked once for the contract and for
//! every host.

use bimetal_ir::BufferElement;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{Expr, Ident, Type as SynType};

use crate::attributes::Boundary;
use crate::types::type_ref;

/// What crosses a direct boundary.
pub(crate) enum DirectType<'syntax> {
    /// Bytes of any length: `Vec<u8>` or `[u8]`.
    Bytes,
    /// Exactly `length` bytes: `[u8; N]`.
    FixedBytes {
        /// The array's length expression.
        length: &'syntax Expr,
    },
    /// Numeric values: `Vec<T>` or `[T]`.
    Buffer {
        /// The element type, `T`.
        element: &'syntax SynType,
        /// The `bimetal::ir::BufferElement` variant of `T`, such as `F64`.
        variant: Ident,
    },
    /// An Arrow value, such as `RecordBatch`.
    Arrow {
        /// The value's type.
        value: &'syntax SynType,
    },
}

impl<'syntax> DirectType<'syntax> {
    /// Check that `ty` has a shape that `boundary` carries.
    pub(crate) fn parse(ty: &'syntax SynType, boundary: Boundary) -> syn::Result<Self> {
        let (borrowed, value) = match ty {
            SynType::Reference(reference) if reference.mutability.is_none() => {
                (true, reference.elem.as_ref())
            }
            ty => (false, ty),
        };
        // The element of `Vec<T>`, `&Vec<T>`, or `&[T]`.
        let element = match value {
            SynType::Slice(slice) if borrowed => Some(slice.elem.as_ref()),
            value => type_ref::vec_item(value),
        };
        match (boundary, value) {
            (Boundary::Arrow, value) => Ok(Self::Arrow { value }),
            (Boundary::Bytes, SynType::Array(array)) if type_ref::is_u8(&array.elem) => {
                Ok(Self::FixedBytes { length: &array.len })
            }
            (Boundary::Bytes, _) if element.is_some_and(type_ref::is_u8) => Ok(Self::Bytes),
            (Boundary::Bytes, _) => Err(syn::Error::new_spanned(
                ty,
                "`bytes` requires `Vec<u8>`, `&[u8]`, `[u8; N]`, or `&[u8; N]`",
            )),
            (Boundary::Buffer, _) => {
                let known = element
                    .and_then(type_ref::scalar_name)
                    .and_then(|name| BufferElement::from_rust_name(&name));
                match (element, known) {
                    (Some(element), Some(known)) => Ok(Self::Buffer {
                        element,
                        // A derived `Debug` writes a variant's name.
                        variant: format_ident!("{known:?}"),
                    }),
                    _ => Err(syn::Error::new_spanned(
                        ty,
                        "`buffer` requires `Vec<T>`, `&Vec<T>`, or `&[T]` of a numeric type, from `u8` to `f64`",
                    )),
                }
            }
        }
    }

    /// Return the type of the values that cross: `u8` for bytes and fixed
    /// bytes, a buffer's element type, and `None` for an Arrow value.
    pub(crate) fn element(&self) -> Option<TokenStream2> {
        match self {
            Self::Bytes | Self::FixedBytes { .. } => Some(quote!(::core::primitive::u8)),
            Self::Buffer { element, .. } => Some(quote!(#element)),
            Self::Arrow { .. } => None,
        }
    }

    /// Render the value's `bimetal::ir::TypeRef` construction.
    pub(crate) fn type_ref_tokens(&self) -> TokenStream2 {
        match self {
            Self::Bytes => quote!(::bimetal::ir::TypeRef::Bytes),
            Self::FixedBytes { length } => quote!(::bimetal::ir::TypeRef::FixedBytes {
                length: <::core::primitive::u64 as ::core::convert::TryFrom<
                    ::core::primitive::usize
                >>::try_from(::core::mem::size_of::<[::core::primitive::u8; #length]>())
                    .expect("fixed byte length must fit in the Bimetal IR"),
            }),
            Self::Buffer { variant, .. } => quote!(::bimetal::ir::TypeRef::Buffer {
                element: ::bimetal::ir::BufferElement::#variant,
            }),
            Self::Arrow { value } => quote!(::bimetal::ir::TypeRef::Arrow {
                value: <#value as ::bimetal::arrow::ArrowValue>::KIND,
            }),
        }
    }
}
