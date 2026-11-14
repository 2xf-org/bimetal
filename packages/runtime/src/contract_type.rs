//! Built-in mappings from Rust types to the host-neutral contract IR.
//!
//! Implementations in this module describe ordinary Serde-shaped values. A
//! resource's own implementation comes from `#[bimetal::export]`, and the
//! procedural macros handle bytes, numeric buffers and Arrow values.

use std::collections::{BTreeMap, HashMap};
use std::hash::BuildHasher;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, FixedOffset, NaiveDate, Utc};
use serde_json::Value;

use crate::ir::TypeRef;
use crate::runtime::ValueError;

/// Describe how a Rust type appears in the generated host APIs.
#[diagnostic::on_unimplemented(
    note = "derive `bimetal::Model` for your own types, or export an impl block of a resource; numeric buffers and Arrow values need `#[bimetal(buffer)]` or `#[bimetal(arrow)]` on the parameter, or `#[bimetal(returns(...))]` on the export"
)]
pub trait ContractType {
    /// How argument errors name the host sequence that this type reads, such
    /// as `a list`, or `None` when it reads none. Python would read a `str`
    /// as the sequence of its characters, so the extension rejects a `str`
    /// for such a type.
    #[doc(hidden)]
    const SEQUENCE: Option<&'static str> = None;

    /// How a whole parameter or return value of this type crosses:
    /// [`crossing::Resource`] for a resource, and [`crossing::Serde`] for
    /// every other type, including an `Option` or a `Vec` of resources, which
    /// the macros ask their item about.
    #[doc(hidden)]
    type Crossing: crossing::Crossing;

    /// Return the host-neutral reference for this Rust type.
    fn type_ref() -> TypeRef;

    /// Check the field rules of this value and of every value inside it.
    /// `#[derive(Model)]` implements it; other types have no rules of their
    /// own, and containers check their items.
    ///
    /// # Errors
    ///
    /// Returns the first rule that the value breaks.
    #[doc(hidden)]
    #[inline]
    fn check_rules(&self) -> Result<(), ValueError> {
        Ok(())
    }
}

macro_rules! integer {
    ($type:ty, $signed:literal, $bits:literal) => {
        impl ContractType for $type {
            type Crossing = crossing::Serde;

            fn type_ref() -> TypeRef {
                TypeRef::Int {
                    signed: $signed,
                    bits: $bits,
                }
            }
        }
    };
}

integer!(u8, false, 8);
integer!(u16, false, 16);
integer!(u32, false, 32);
integer!(u64, false, 64);
integer!(usize, false, 64);
integer!(i8, true, 8);
integer!(i16, true, 16);
integer!(i32, true, 32);
integer!(i64, true, 64);
integer!(isize, true, 64);

impl ContractType for () {
    type Crossing = crossing::Serde;

    fn type_ref() -> TypeRef {
        TypeRef::Unit
    }
}

impl ContractType for bool {
    type Crossing = crossing::Serde;

    fn type_ref() -> TypeRef {
        TypeRef::Bool
    }
}

impl ContractType for f32 {
    type Crossing = crossing::Serde;

    fn type_ref() -> TypeRef {
        TypeRef::Float { bits: 32 }
    }
}

impl ContractType for f64 {
    type Crossing = crossing::Serde;

    fn type_ref() -> TypeRef {
        TypeRef::Float { bits: 64 }
    }
}

impl ContractType for String {
    type Crossing = crossing::Serde;

    fn type_ref() -> TypeRef {
        TypeRef::String
    }
}

impl ContractType for str {
    type Crossing = crossing::Serde;

    fn type_ref() -> TypeRef {
        TypeRef::String
    }
}

impl ContractType for DateTime<Utc> {
    type Crossing = crossing::Serde;

    fn type_ref() -> TypeRef {
        TypeRef::DateTime
    }
}

impl ContractType for NaiveDate {
    type Crossing = crossing::Serde;

    fn type_ref() -> TypeRef {
        TypeRef::Date
    }
}

// alloy's Serde writes each of these as a `0x` hex string.
#[cfg(feature = "alloy")]
impl ContractType for alloy_primitives::Address {
    type Crossing = crossing::Serde;

    fn type_ref() -> TypeRef {
        TypeRef::String
    }
}

#[cfg(feature = "alloy")]
impl<const N: usize> ContractType for alloy_primitives::FixedBytes<N> {
    type Crossing = crossing::Serde;

    fn type_ref() -> TypeRef {
        TypeRef::String
    }
}

// Unsigned integers, such as `U256` and `U64`, are written in hex and
// signed ones, such as `I256`, in decimal.
#[cfg(feature = "alloy")]
impl<const BITS: usize, const LIMBS: usize> ContractType for alloy_primitives::Uint<BITS, LIMBS> {
    type Crossing = crossing::Serde;

    fn type_ref() -> TypeRef {
        TypeRef::String
    }
}

#[cfg(feature = "alloy")]
impl<const BITS: usize, const LIMBS: usize> ContractType for alloy_primitives::Signed<BITS, LIMBS> {
    type Crossing = crossing::Serde;

    fn type_ref() -> TypeRef {
        TypeRef::String
    }
}

#[cfg(feature = "alloy")]
impl ContractType for alloy_primitives::Bytes {
    type Crossing = crossing::Serde;

    fn type_ref() -> TypeRef {
        TypeRef::String
    }
}

impl ContractType for DateTime<FixedOffset> {
    type Crossing = crossing::Serde;

    fn type_ref() -> TypeRef {
        TypeRef::DateTime
    }
}

impl ContractType for Value {
    type Crossing = crossing::Serde;

    fn type_ref() -> TypeRef {
        TypeRef::Json
    }
}

impl<T: ContractType> ContractType for Option<T> {
    const SEQUENCE: Option<&'static str> = T::SEQUENCE;

    type Crossing = crossing::Serde;

    fn type_ref() -> TypeRef {
        TypeRef::Option {
            item: Box::new(T::type_ref()),
        }
    }

    #[inline]
    fn check_rules(&self) -> Result<(), ValueError> {
        self.as_ref().map_or(Ok(()), ContractType::check_rules)
    }
}

impl<T: ContractType> ContractType for Vec<T> {
    const SEQUENCE: Option<&'static str> = Some("a list");

    type Crossing = crossing::Serde;

    fn type_ref() -> TypeRef {
        TypeRef::List {
            item: Box::new(T::type_ref()),
        }
    }

    #[inline]
    fn check_rules(&self) -> Result<(), ValueError> {
        self.as_slice().check_rules()
    }
}

impl<T: ContractType> ContractType for [T] {
    const SEQUENCE: Option<&'static str> = Some("a list");

    type Crossing = crossing::Serde;

    fn type_ref() -> TypeRef {
        TypeRef::List {
            item: Box::new(T::type_ref()),
        }
    }

    #[inline]
    fn check_rules(&self) -> Result<(), ValueError> {
        for (index, item) in self.iter().enumerate() {
            item.check_rules()
                .map_err(|violation| violation.at_index(index))?;
        }
        Ok(())
    }
}

impl<T: ContractType> ContractType for BTreeMap<String, T> {
    type Crossing = crossing::Serde;

    fn type_ref() -> TypeRef {
        TypeRef::Map {
            value: Box::new(T::type_ref()),
        }
    }

    #[inline]
    fn check_rules(&self) -> Result<(), ValueError> {
        check_map_rules(self)
    }
}

impl<T: ContractType, S: BuildHasher> ContractType for HashMap<String, T, S> {
    type Crossing = crossing::Serde;

    fn type_ref() -> TypeRef {
        TypeRef::Map {
            value: Box::new(T::type_ref()),
        }
    }

    #[inline]
    fn check_rules(&self) -> Result<(), ValueError> {
        check_map_rules(self)
    }
}

/// Check the rules of every value of a map, naming the key of the first that
/// breaks one.
#[inline]
fn check_map_rules<'a, T: ContractType + 'a>(
    entries: impl IntoIterator<Item = (&'a String, &'a T)>,
) -> Result<(), ValueError> {
    for (key, value) in entries {
        value
            .check_rules()
            .map_err(|violation| violation.at_key(key))?;
    }
    Ok(())
}

impl<T: ContractType + ?Sized> ContractType for &T {
    const SEQUENCE: Option<&'static str> = T::SEQUENCE;

    type Crossing = crossing::Serde;

    fn type_ref() -> TypeRef {
        T::type_ref()
    }

    #[inline]
    fn check_rules(&self) -> Result<(), ValueError> {
        (**self).check_rules()
    }
}

// Serde writes a path as its text, and refuses to write one that is not UTF-8.
impl ContractType for Path {
    type Crossing = crossing::Serde;

    fn type_ref() -> TypeRef {
        TypeRef::String
    }
}

impl ContractType for PathBuf {
    type Crossing = crossing::Serde;

    fn type_ref() -> TypeRef {
        TypeRef::String
    }
}

impl ContractType for Duration {
    type Crossing = crossing::Serde;

    fn type_ref() -> TypeRef {
        TypeRef::Duration
    }
}

// Serde writes an array as a tuple of its items.
impl<T: ContractType, const N: usize> ContractType for [T; N] {
    const SEQUENCE: Option<&'static str> = Some("a tuple");

    type Crossing = crossing::Serde;

    fn type_ref() -> TypeRef {
        TypeRef::Tuple {
            items: vec![T::type_ref(); N],
        }
    }

    #[inline]
    fn check_rules(&self) -> Result<(), ValueError> {
        for (index, item) in self.iter().enumerate() {
            item.check_rules()
                .map_err(|violation| violation.at_index(index))?;
        }
        Ok(())
    }
}

// Serde writes a box as the value inside it, such as the boxed payload of a
// large enum variant.
impl<T: ContractType + ?Sized> ContractType for Box<T> {
    const SEQUENCE: Option<&'static str> = T::SEQUENCE;

    type Crossing = crossing::Serde;

    fn type_ref() -> TypeRef {
        T::type_ref()
    }

    #[inline]
    fn check_rules(&self) -> Result<(), ValueError> {
        (**self).check_rules()
    }
}

macro_rules! tuple {
    ($($index:tt $name:ident),+) => {
        impl<$($name: ContractType),+> ContractType for ($($name,)+) {
            const SEQUENCE: Option<&'static str> = Some("a tuple");

            type Crossing = crossing::Serde;

            fn type_ref() -> TypeRef {
                TypeRef::Tuple {
                    items: vec![$($name::type_ref()),+],
                }
            }

            #[inline]
            fn check_rules(&self) -> Result<(), ValueError> {
                $(
                    self.$index
                        .check_rules()
                        .map_err(|violation| violation.at_index($index))?;
                )+
                Ok(())
            }
        }
    };
}

tuple!(0 A, 1 B);
tuple!(0 A, 1 B, 2 C);
tuple!(0 A, 1 B, 2 C, 3 D);
tuple!(0 A, 1 B, 2 C, 3 D, 4 E);
tuple!(0 A, 1 B, 2 C, 3 D, 4 E, 5 F);
tuple!(0 A, 1 B, 2 C, 3 D, 4 E, 5 F, 6 G);
tuple!(0 A, 1 B, 2 C, 3 D, 4 E, 5 F, 6 G, 7 H);

/// How a whole parameter or return value crosses each host, which
/// [`ContractType::Crossing`] says for its type. Each host's conversions
/// implement their traits for both.
#[doc(hidden)]
pub mod crossing {
    /// What a host knows of a crossing before it has a value.
    pub trait Crossing {
        /// Whether the values are resources. A synchronous call that makes
        /// one enters the shared runtime, so that the resource can spawn
        /// tasks.
        const RESOURCE: bool;
    }

    /// Through Serde, as every value but a resource crosses.
    pub enum Serde {}

    impl Crossing for Serde {
        const RESOURCE: bool = false;
    }

    /// As a resource, which the host holds as an object of its generated
    /// class.
    pub enum Resource {}

    impl Crossing for Resource {
        const RESOURCE: bool = true;
    }
}
