//! [`Value`], a JavaScript value of one environment during one call into
//! Rust, and what Rust reads from and writes to it.

use std::ffi::{CStr, c_char, c_void};
use std::{mem, ptr};

use bimetal_ir::BufferElement;
use napi_sys as sys;

use super::env::{Env, napi};
use super::error::Error;

/// A JavaScript value of one environment, during one call into Rust.
#[derive(Clone, Copy)]
pub struct Value<'env> {
    pub(super) env: Env<'env>,
    pub(super) raw: sys::napi_value,
}

/// What `typeof` says about a value, with `null` apart from objects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Undefined,
    Null,
    Boolean,
    Number,
    BigInt,
    String,
    Symbol,
    Object,
    Function,
    External,
}

/// The element type of a typed array.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArrayType(pub(super) sys::napi_typedarray_type);

mod sealed {
    pub trait Sealed {}
}

/// A primitive that a Bimetal numeric buffer holds, which crosses as the typed
/// array of its type.
#[doc(hidden)]
pub trait Element: sealed::Sealed + Copy {
    /// The typed array's name, such as `Float64Array`.
    const ARRAY: &'static str;
    /// The typed array's element type.
    #[doc(hidden)]
    const TYPE: ArrayType;
}

macro_rules! element {
    ($($ty:ty => $element:ident, $kind:ident),+ $(,)?) => {
        $(
            impl sealed::Sealed for $ty {}
            impl Element for $ty {
                const ARRAY: &'static str = BufferElement::$element.typed_array();
                const TYPE: ArrayType = ArrayType(sys::TypedarrayType::$kind);
            }
        )+
    };
}

element!(
    u8 => U8, uint8_array,
    i8 => I8, int8_array,
    u16 => U16, uint16_array,
    i16 => I16, int16_array,
    u32 => U32, uint32_array,
    i32 => I32, int32_array,
    u64 => U64, biguint64_array,
    i64 => I64, bigint64_array,
    f32 => F32, float32_array,
    f64 => F64, float64_array,
);

impl<'env> Value<'env> {
    /// The environment that holds the value.
    pub(crate) fn env(self) -> Env<'env> {
        self.env
    }

    /// What `typeof` says about the value.
    pub(crate) fn kind(self) -> Result<Kind, Error> {
        Ok(match napi!(self.env, napi_typeof(self.raw) -> out)? {
            sys::ValueType::napi_undefined => Kind::Undefined,
            sys::ValueType::napi_null => Kind::Null,
            sys::ValueType::napi_boolean => Kind::Boolean,
            sys::ValueType::napi_number => Kind::Number,
            sys::ValueType::napi_bigint => Kind::BigInt,
            sys::ValueType::napi_string => Kind::String,
            sys::ValueType::napi_symbol => Kind::Symbol,
            sys::ValueType::napi_function => Kind::Function,
            sys::ValueType::napi_external => Kind::External,
            _ => Kind::Object,
        })
    }

    /// Return `value === other`.
    pub(crate) fn strict_equals(self, other: Value<'env>) -> Result<bool, Error> {
        napi!(self.env, napi_strict_equals(self.raw, other.raw) -> out)
    }

    pub(crate) fn as_bool(self) -> Result<bool, Error> {
        napi!(self.env, napi_get_value_bool(self.raw) -> out)
    }

    pub(crate) fn as_f64(self) -> Result<f64, Error> {
        napi!(self.env, napi_get_value_double(self.raw) -> out)
    }

    /// Read a string as UTF-8, with every lone surrogate replaced.
    pub(crate) fn as_string(self) -> Result<String, Error> {
        let length = napi!(
            self.env,
            napi_get_value_string_utf8(self.raw, ptr::null_mut(), 0) -> out
        )?;
        let mut bytes: Vec<u8> = Vec::with_capacity(length + 1);
        let written = napi!(
            self.env,
            napi_get_value_string_utf8(
                self.raw,
                bytes.as_mut_ptr().cast::<c_char>(),
                length + 1
            ) -> out
        )?;
        // SAFETY: Node-API wrote `written` bytes, at most `length`, and a NUL
        // after them into the vector's capacity of `length + 1`.
        unsafe { bytes.set_len(written.min(length)) };
        Ok(String::from_utf8(bytes)
            .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned()))
    }

    /// Read a string's UTF-16 code units.
    pub(crate) fn as_utf16(self) -> Result<Vec<u16>, Error> {
        let length = napi!(
            self.env,
            napi_get_value_string_utf16(self.raw, ptr::null_mut(), 0) -> out
        )?;
        let mut units: Vec<u16> = Vec::with_capacity(length + 1);
        let written = napi!(
            self.env,
            napi_get_value_string_utf16(self.raw, units.as_mut_ptr(), length + 1) -> out
        )?;
        // SAFETY: as in `as_string`, with code units.
        unsafe { units.set_len(written.min(length)) };
        Ok(units)
    }

    /// Read a bigint that fits in an `i64`.
    pub(crate) fn as_i64(self) -> Result<Option<i64>, Error> {
        let mut value = 0;
        let mut lossless = false;
        napi!(
            self.env,
            napi_get_value_bigint_int64(self.raw, &mut value, &mut lossless)
        )?;
        Ok(lossless.then_some(value))
    }

    /// Read a bigint that fits in a `u64`.
    pub(crate) fn as_u64(self) -> Result<Option<u64>, Error> {
        let mut value = 0;
        let mut lossless = false;
        napi!(
            self.env,
            napi_get_value_bigint_uint64(self.raw, &mut value, &mut lossless)
        )?;
        Ok(lossless.then_some(value))
    }

    /// Read a bigint as its sign and its 64-bit words, the least significant
    /// first.
    pub(crate) fn as_words(self) -> Result<(bool, Vec<u64>), Error> {
        let mut sign = 0;
        let mut count = 0;
        napi!(
            self.env,
            napi_get_value_bigint_words(self.raw, &mut sign, &mut count, ptr::null_mut())
        )?;
        let mut words = vec![0; count];
        napi!(
            self.env,
            napi_get_value_bigint_words(self.raw, &mut sign, &mut count, words.as_mut_ptr())
        )?;
        words.truncate(count);
        Ok((sign != 0, words))
    }

    /// Return whether the value is an array, as `Array.isArray` does.
    pub(crate) fn is_array(self) -> Result<bool, Error> {
        napi!(self.env, napi_is_array(self.raw) -> out)
    }

    pub(crate) fn array_length(self) -> Result<u32, Error> {
        napi!(self.env, napi_get_array_length(self.raw) -> out)
    }

    /// Return `value[index]`.
    pub(crate) fn element(self, index: u32) -> Result<Value<'env>, Error> {
        napi!(self.env, napi_get_element(self.raw, index) -> value)
    }

    /// Run `value[index] = element`.
    pub(crate) fn set_element(self, index: u32, element: Value<'env>) -> Result<(), Error> {
        napi!(self.env, napi_set_element(self.raw, index, element.raw))
    }

    /// Return `value[key]`.
    pub(crate) fn property(self, key: Value<'env>) -> Result<Value<'env>, Error> {
        napi!(self.env, napi_get_property(self.raw, key.raw) -> value)
    }

    /// Return `value[name]`.
    pub(crate) fn named(self, name: &CStr) -> Result<Value<'env>, Error> {
        napi!(self.env, napi_get_named_property(self.raw, name.as_ptr()) -> value)
    }

    /// Run `value[key] = property`.
    pub(crate) fn set_property(self, key: Value<'env>, property: Value<'env>) -> Result<(), Error> {
        napi!(self.env, napi_set_property(self.raw, key.raw, property.raw))
    }

    /// Run `value[name] = property`.
    pub(crate) fn set_named(self, name: &CStr, property: Value<'env>) -> Result<(), Error> {
        napi!(
            self.env,
            napi_set_named_property(self.raw, name.as_ptr(), property.raw)
        )
    }

    /// Return `key in value`.
    pub(crate) fn has(self, key: Value<'env>) -> Result<bool, Error> {
        napi!(self.env, napi_has_property(self.raw, key.raw) -> out)
    }

    /// Return the keys that `Object.keys` returns: the own, enumerable
    /// string keys.
    pub(crate) fn keys(self) -> Result<Vec<Value<'env>>, Error> {
        let keys = napi!(
            self.env,
            napi_get_all_property_names(
                self.raw,
                sys::KeyCollectionMode::own_only,
                sys::KeyFilter::enumerable | sys::KeyFilter::skip_symbols,
                sys::KeyConversion::numbers_to_strings
            ) -> value
        )?;
        (0..keys.array_length()?)
            .map(|index| keys.element(index))
            .collect()
    }

    /// Return `value instanceof constructor`.
    pub(crate) fn instance_of(self, constructor: Value<'env>) -> Result<bool, Error> {
        napi!(self.env, napi_instanceof(self.raw, constructor.raw) -> out)
    }

    /// Return whether this value is `null` or `undefined`.
    pub(crate) fn is_nullish(self) -> Result<bool, Error> {
        Ok(matches!(self.kind()?, Kind::Null | Kind::Undefined))
    }

    /// Return whether this value is a function.
    pub(crate) fn is_function(self) -> Result<bool, Error> {
        Ok(self.kind()? == Kind::Function)
    }

    /// Call this function with `this` and `arguments`.
    pub(crate) fn call<const N: usize>(
        self,
        this: Value<'env>,
        arguments: [Value<'env>; N],
    ) -> Result<Value<'env>, Error> {
        let arguments = arguments.map(|value| value.raw);
        napi!(
            self.env,
            napi_call_function(this.raw, self.raw, N, arguments.as_ptr()) -> value
        )
    }

    /// Construct an instance of this constructor with `arguments`.
    pub(crate) fn construct<const N: usize>(
        self,
        arguments: [Value<'env>; N],
    ) -> Result<Value<'env>, Error> {
        let arguments = arguments.map(|value| value.raw);
        napi!(
            self.env,
            napi_new_instance(self.raw, N, arguments.as_ptr()) -> value
        )
    }

    /// Return `String(value)`.
    pub(crate) fn to_js_string(self) -> Result<String, Error> {
        napi!(self.env, napi_coerce_to_string(self.raw) -> value)?.as_string()
    }

    /// Return `Number(value)`.
    pub(crate) fn to_number(self) -> Result<f64, Error> {
        napi!(self.env, napi_coerce_to_number(self.raw) -> value)?.as_f64()
    }

    /// Return the element type of a typed array, or `None` for another
    /// value.
    pub(crate) fn typed_array_type(self) -> Result<Option<ArrayType>, Error> {
        Ok(self.typed_array()?.map(|(kind, _, _)| kind))
    }

    /// Return the element type, the length, and the data of a typed array,
    /// or `None` for another value.
    fn typed_array(self) -> Result<Option<(ArrayType, usize, *mut c_void)>, Error> {
        if !napi!(self.env, napi_is_typedarray(self.raw) -> out)? {
            return Ok(None);
        }
        let mut kind = 0;
        let mut length = 0;
        let mut data = ptr::null_mut();
        napi!(
            self.env,
            napi_get_typedarray_info(
                self.raw,
                &mut kind,
                &mut length,
                &mut data,
                ptr::null_mut(),
                ptr::null_mut()
            )
        )?;
        Ok(Some((ArrayType(kind), length, data)))
    }

    /// Copy the elements of a typed array of `T`, or return `None` for
    /// another value.
    pub(crate) fn typed_array_values<T: Element>(self) -> Result<Option<Vec<T>>, Error> {
        let Some((kind, length, data)) = self.typed_array()? else {
            return Ok(None);
        };
        if kind != T::TYPE {
            return Ok(None);
        }
        // SAFETY: a typed array of `T`'s element type holds `length` values
        // at `data`, which no JavaScript code runs to detach during the copy.
        // A `SharedArrayBuffer` can change meanwhile, but every bit pattern is
        // a valid `T`.
        Ok(Some(unsafe { copy_values(data, length) }))
    }

    pub(crate) fn is_array_buffer(self) -> Result<bool, Error> {
        napi!(self.env, napi_is_arraybuffer(self.raw) -> out)
    }

    /// Copy the bytes of an `ArrayBuffer`, or return `None` for another
    /// value.
    pub(crate) fn array_buffer_bytes(self) -> Result<Option<Vec<u8>>, Error> {
        if !self.is_array_buffer()? {
            return Ok(None);
        }
        let mut data = ptr::null_mut();
        let mut length = 0;
        napi!(
            self.env,
            napi_get_arraybuffer_info(self.raw, &mut data, &mut length)
        )?;
        // SAFETY: an `ArrayBuffer` holds `length` bytes at `data`, which no
        // JavaScript code runs to detach during the copy.
        Ok(Some(unsafe { copy_values(data, length) }))
    }
}

/// Copy `length` values of `T` from `data`.
///
/// # Safety
///
/// Unless `length` is 0, `data` must point at `length` values of `T` that
/// stay readable during the copy.
unsafe fn copy_values<T: Element>(data: *const c_void, length: usize) -> Vec<T> {
    if length == 0 {
        return Vec::new();
    }
    let mut values: Vec<T> = Vec::with_capacity(length);
    // SAFETY: the caller guarantees the source. The byte copy needs no
    // alignment, and fills the new vector's capacity, which nothing else
    // references, before `set_len`.
    unsafe {
        ptr::copy_nonoverlapping(
            data.cast::<u8>(),
            values.as_mut_ptr().cast::<u8>(),
            length * mem::size_of::<T>(),
        );
        values.set_len(length);
    }
    values
}
