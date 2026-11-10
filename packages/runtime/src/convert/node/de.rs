//! Deserialize JavaScript values as serde-wasm-bindgen 0.6 does, so that an
//! argument reads alike in the Wasm module and the Node.js addon: the same
//! checks in the same order, own enumerable keys for objects, a bigint or a
//! safe integer for a 64-bit integer, and the same messages.
//!
//! Where serde-wasm-bindgen catches an exception, while it iterates a value,
//! the exception becomes the message of the argument's `TypeError`. Where an
//! exception would trap the Wasm module, such as one that a getter throws, it
//! stays pending here, and the call throws it. A value nested past
//! [`MAX_DEPTH`] fails here, where serde-wasm-bindgen would recurse until the
//! stack overflows.

use std::fmt::Display;
use std::{slice, vec};

use serde::de::value::StrDeserializer;
use serde::de::{self, DeserializeOwned, DeserializeSeed, IntoDeserializer, Unexpected, Visitor};

use super::names::Names;
use crate::runtime::node::{Element, Env, Error, Kind, Value};

impl de::Error for Error {
    fn custom<T: Display>(message: T) -> Self {
        Self::Message(message.to_string())
    }
}

/// How deep arrays and objects may nest in a value that Rust reads or
/// describes, as in serde_json. Deeper recursion could overflow the native
/// stack, which would end the Node.js process.
const MAX_DEPTH: usize = 128;

/// Deserialize `T` from a JavaScript value.
pub(super) fn from_value<T: DeserializeOwned>(value: Value<'_>) -> Result<T, Error> {
    let names = Names::new(value.env());
    let scope = Scope {
        names: &names,
        depth: 0,
    };
    T::deserialize(scope.read(value))
}

/// Where a value sits in the conversion that reads it: the conversion's
/// names, and how many arrays and objects hold the value.
#[derive(Clone, Copy)]
struct Scope<'names, 'env> {
    names: &'names Names<'env>,
    depth: usize,
}

impl<'names, 'env> Scope<'names, 'env> {
    /// Return the deserializer of `value` in this scope.
    fn read(self, value: Value<'env>) -> Deserializer<'names, 'env> {
        Deserializer { value, scope: self }
    }

    /// Return the scope of the values inside an array or object in this
    /// scope, or an error past [`MAX_DEPTH`].
    fn inner(self) -> Result<Self, Error> {
        if self.depth < MAX_DEPTH {
            Ok(Self {
                depth: self.depth + 1,
                ..self
            })
        } else {
            Err(Error::Message(format!(
                "must nest at most {MAX_DEPTH} levels deep"
            )))
        }
    }

    /// Deserialize the map value that waits in `next_value` for its key.
    fn read_next_value<'de, V: DeserializeSeed<'de>>(
        self,
        next_value: &mut Option<Value<'env>>,
        seed: V,
    ) -> Result<V::Value, Error> {
        let value = next_value
            .take()
            .expect("Serde reads each key before its value");
        seed.deserialize(self.read(value))
    }
}

/// A JavaScript value that Serde reads.
struct Deserializer<'names, 'env> {
    value: Value<'env>,
    scope: Scope<'names, 'env>,
}

/// Return whether `value` is an integer that a number holds exactly, as
/// `Number.isSafeInteger` does.
fn is_safe_integer(value: f64) -> bool {
    const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
    value.is_finite() && value.trunc() == value && value.abs() <= MAX_SAFE_INTEGER
}

/// Turn an exception that JavaScript code threw into the message of the
/// argument's error, as serde-wasm-bindgen does where it catches one: the
/// exception's `String()`, without the prefix `Error: `.
fn caught<'env>(env: Env<'env>, result: Result<Value<'env>, Error>) -> Result<Value<'env>, Error> {
    let Err(Error::Pending) = result else {
        return result;
    };
    let Some(exception) = env.take_exception() else {
        return Err(Error::Pending);
    };
    let text = exception.to_js_string()?;
    Err(Error::Message(match text.strip_prefix("Error: ") {
        Some(message) => message.to_owned(),
        None => text,
    }))
}

/// A JavaScript iterator, which serde-wasm-bindgen reads through
/// `js_sys::try_iter`.
struct JsIterator<'env> {
    iterator: Value<'env>,
    next: Value<'env>,
}

impl<'env> JsIterator<'env> {
    /// Return the iterator of `value`, or `None` when it is not iterable.
    fn of(value: Value<'env>) -> Result<Option<Self>, Error> {
        let env = value.env();
        if !matches!(value.kind()?, Kind::Object | Kind::Function) {
            // `js_sys::try_iter` reads the method with `Reflect.get`, which
            // throws for a primitive.
            return Err(Error::Message(
                "TypeError: Reflect.get called on non-object".to_owned(),
            ));
        }
        let method = caught(env, value.property(env.iterator_symbol()?))?;
        if method.kind()? != Kind::Function {
            return Ok(None);
        }
        let iterator = caught(env, method.call(value, []))?;
        if iterator.kind()? != Kind::Object {
            return Ok(None);
        }
        let next = iterator.named(c"next")?;
        if next.kind()? != Kind::Function {
            return Ok(None);
        }
        Ok(Some(Self { iterator, next }))
    }

    /// Return the next value, or `None` when the iterator is done.
    fn next(&mut self) -> Result<Option<Value<'env>>, Error> {
        let env = self.iterator.env();
        let step = caught(env, self.next.call(self.iterator, []))?;
        let done = step.named(c"done")?;
        // wasm-bindgen reads `done` as a `bool` through an `i32`, which
        // JavaScript converts with `ToInt32`.
        let done = match done.kind()? {
            Kind::Boolean => done.as_bool()?,
            _ => {
                let number = done.to_number()?;
                number.is_finite() && number.trunc() % 4_294_967_296.0 != 0.0
            }
        };
        if done {
            return Ok(None);
        }
        step.named(c"value").map(Some)
    }
}

impl Deserializer<'_, '_> {
    /// Return whether the value is `undefined` or `null`, which `== null`
    /// accepts.
    fn is_nullish(&self) -> Result<bool, Error> {
        Ok(matches!(self.value.kind()?, Kind::Undefined | Kind::Null))
    }

    /// Return a boolean's value.
    fn as_bool(&self) -> Result<Option<bool>, Error> {
        if self.value.kind()? != Kind::Boolean {
            return Ok(None);
        }
        self.value.as_bool().map(Some)
    }

    /// Return a number's value.
    fn as_f64(&self) -> Result<Option<f64>, Error> {
        if self.value.kind()? != Kind::Number {
            return Ok(None);
        }
        self.value.as_f64().map(Some)
    }

    /// Return a string's value.
    fn as_string(&self) -> Result<Option<String>, Error> {
        if self.value.kind()? != Kind::String {
            return Ok(None);
        }
        self.value.as_string().map(Some)
    }

    /// Return the bytes of a `Uint8Array` or an `ArrayBuffer`.
    fn as_bytes(&self) -> Result<Option<Vec<u8>>, Error> {
        match self.value.typed_array_values::<u8>()? {
            Some(bytes) => Ok(Some(bytes)),
            None => self.value.array_buffer_bytes(),
        }
    }

    /// Return whether the value is a `Uint8Array` or an `ArrayBuffer`.
    fn is_bytes(&self) -> Result<bool, Error> {
        Ok(self.value.typed_array_type()? == Some(u8::TYPE) || self.value.is_array_buffer()?)
    }

    /// Return a number's value when it is a safe integer, which an `i64`
    /// holds exactly.
    fn as_safe_integer(&self) -> Result<Option<i64>, Error> {
        Ok(self
            .as_f64()?
            .filter(|value| is_safe_integer(*value))
            .map(|value| value as i64))
    }

    /// Report a value of the wrong type for `expected`.
    #[cold]
    fn invalid_type(&self, expected: &dyn de::Expected) -> Error {
        match self.unexpected(expected) {
            Ok(error) | Err(error) => error,
        }
    }

    /// Describe the value as serde-wasm-bindgen does in the error for a value
    /// of the wrong type: by its type, or as wasm-bindgen debugs it.
    fn unexpected(&self, expected: &dyn de::Expected) -> Result<Error, Error> {
        let text;
        let unexpected = match self.value.kind()? {
            Kind::Undefined | Kind::Null => Unexpected::Unit,
            Kind::Boolean => Unexpected::Bool(self.value.as_bool()?),
            Kind::Number => Unexpected::Float(self.value.as_f64()?),
            Kind::String => {
                text = self.value.as_string()?;
                Unexpected::Str(&text)
            }
            _ if self.is_bytes()? => Unexpected::Bytes(&[]),
            _ => {
                text = format!("JsValue({})", debug_string(self.value, &mut Vec::new())?);
                Unexpected::Other(&text)
            }
        };
        Ok(de::Error::invalid_type(unexpected, expected))
    }

    fn deserialize_signed<'de, V: Visitor<'de>>(&self, visitor: V) -> Result<V::Value, Error> {
        match self.as_safe_integer()? {
            Some(value) => visitor.visit_i64(value),
            None => Err(self.invalid_type(&visitor)),
        }
    }

    fn deserialize_unsigned<'de, V: Visitor<'de>>(&self, visitor: V) -> Result<V::Value, Error> {
        match self.as_safe_integer()? {
            Some(value) if value >= 0 => visitor.visit_u64(value.unsigned_abs()),
            _ => Err(self.invalid_type(&visitor)),
        }
    }

    /// Read a bigint as a 128-bit integer, or `None` when it is outside
    /// `i128`'s range.
    fn as_i128(&self) -> Result<Option<i128>, Error> {
        let Some((negative, magnitude)) = self.as_u128_magnitude()? else {
            return Ok(None);
        };
        Ok(if negative {
            0_i128.checked_sub_unsigned(magnitude)
        } else {
            i128::try_from(magnitude).ok()
        })
    }

    /// Read a bigint's sign and its magnitude, or `None` when the magnitude
    /// needs more than 128 bits.
    fn as_u128_magnitude(&self) -> Result<Option<(bool, u128)>, Error> {
        let (negative, words) = self.value.as_words()?;
        let mut magnitude = 0_u128;
        for (index, word) in words.iter().enumerate() {
            if *word == 0 {
                continue;
            }
            if index > 1 {
                return Ok(None);
            }
            magnitude |= u128::from(*word) << (64 * index);
        }
        Ok(Some((negative && magnitude != 0, magnitude)))
    }
}

impl<'de> de::Deserializer<'de> for Deserializer<'_, '_> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        let kind = self.value.kind()?;
        match kind {
            Kind::Undefined | Kind::Null => return visitor.visit_unit(),
            Kind::Boolean => return visitor.visit_bool(self.value.as_bool()?),
            Kind::BigInt => {
                if let Some(value) = self.value.as_i64()? {
                    return visitor.visit_i64(value);
                }
                return match self.value.as_u64()? {
                    Some(value) => visitor.visit_u64(value),
                    None => Err(de::Error::custom(
                        "Couldn't deserialize i64 or u64 from a BigInt outside i64::MIN..u64::MAX bounds",
                    )),
                };
            }
            Kind::Number => {
                let value = self.value.as_f64()?;
                return if is_safe_integer(value) {
                    visitor.visit_i64(value as i64)
                } else {
                    visitor.visit_f64(value)
                };
            }
            Kind::String => return visitor.visit_string(self.value.as_string()?),
            _ => {}
        }
        if self.value.is_array()? {
            return self.deserialize_seq(visitor);
        }
        if let Some(bytes) = self.as_bytes()? {
            // Serde reads an internally tagged enum through `deserialize_any`.
            return visitor.visit_byte_buf(bytes);
        }
        // An object is a map, as an internally tagged enum reads it, unless
        // it is an iterable other than a `Map`, such as a `Set`.
        let env = self.value.env();
        if kind == Kind::Object
            && (!self.value.has(env.iterator_symbol()?)? || env.is_map(self.value)?)
        {
            return self.deserialize_map(visitor);
        }
        Err(self.invalid_type(&visitor))
    }

    fn deserialize_unit<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        if self.is_nullish()? {
            visitor.visit_unit()
        } else {
            Err(self.invalid_type(&visitor))
        }
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        self.deserialize_unit(visitor)
    }

    fn deserialize_bool<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.as_bool()? {
            Some(value) => visitor.visit_bool(value),
            None => Err(self.invalid_type(&visitor)),
        }
    }

    fn deserialize_f32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_f64(visitor)
    }

    fn deserialize_f64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.as_f64()? {
            Some(value) => visitor.visit_f64(value),
            None => Err(self.invalid_type(&visitor)),
        }
    }

    fn deserialize_identifier<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_string(visitor)
    }

    fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_string(visitor)
    }

    fn deserialize_string<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.as_string()? {
            Some(value) => visitor.visit_string(value),
            None => Err(self.invalid_type(&visitor)),
        }
    }

    fn deserialize_i8<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_signed(visitor)
    }

    fn deserialize_i16<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_signed(visitor)
    }

    fn deserialize_i32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_signed(visitor)
    }

    fn deserialize_u8<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_unsigned(visitor)
    }

    fn deserialize_u16<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_unsigned(visitor)
    }

    fn deserialize_u32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_unsigned(visitor)
    }

    fn deserialize_i64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        if self.value.kind()? != Kind::BigInt {
            return self.deserialize_signed(visitor);
        }
        match self.value.as_i64()? {
            Some(value) => visitor.visit_i64(value),
            None => Err(de::Error::custom(
                "Couldn't deserialize i64 from a BigInt outside i64::MIN..i64::MAX bounds",
            )),
        }
    }

    fn deserialize_u64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        if self.value.kind()? != Kind::BigInt {
            return self.deserialize_unsigned(visitor);
        }
        match self.value.as_u64()? {
            Some(value) => visitor.visit_u64(value),
            None => Err(de::Error::custom(
                "Couldn't deserialize u64 from a BigInt outside u64::MIN..u64::MAX bounds",
            )),
        }
    }

    fn deserialize_i128<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        if self.value.kind()? != Kind::BigInt {
            return Err(self.invalid_type(&visitor));
        }
        match self.as_i128()? {
            Some(value) => visitor.visit_i128(value),
            None => Err(de::Error::custom(
                "Couldn't deserialize i128 from a BigInt outside i128::MIN..i128::MAX bounds",
            )),
        }
    }

    fn deserialize_u128<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        if self.value.kind()? != Kind::BigInt {
            return Err(self.invalid_type(&visitor));
        }
        match self.as_u128_magnitude()? {
            Some((false, value)) => visitor.visit_u128(value),
            _ => Err(de::Error::custom(
                "Couldn't deserialize u128 from a BigInt outside u128::MIN..u128::MAX bounds",
            )),
        }
    }

    /// Read a string of one character, as `JsString::as_char` does: by its
    /// first code point, which must make up the whole string.
    fn deserialize_char<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        if self.value.kind()? == Kind::String {
            let units = self.value.as_utf16()?;
            let point = match units.as_slice() {
                [high @ 0xd800..=0xdbff, low @ 0xdc00..=0xdfff, ..] => {
                    0x1_0000 + ((u32::from(*high) - 0xd800) << 10) + (u32::from(*low) - 0xdc00)
                }
                [unit, ..] => u32::from(*unit),
                [] => u32::MAX,
            };
            if let Some(character) = char::from_u32(point)
                && character.len_utf16() == units.len()
            {
                return visitor.visit_char(character);
            }
        }
        Err(self.invalid_type(&visitor))
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        if self.is_nullish()? {
            visitor.visit_none()
        } else {
            visitor.visit_some(self)
        }
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        if self.value.is_array()? {
            return visitor.visit_seq(ArrayAccess {
                array: self.value,
                index: 0,
                length: self.value.array_length()?,
                scope: self.scope.inner()?,
            });
        }
        match JsIterator::of(self.value)? {
            Some(iterator) => visitor.visit_seq(IteratorAccess {
                iterator,
                scope: self.scope.inner()?,
            }),
            None => Err(self.invalid_type(&visitor)),
        }
    }

    fn deserialize_tuple<V: Visitor<'de>>(
        self,
        _length: usize,
        visitor: V,
    ) -> Result<V::Value, Error> {
        self.deserialize_seq(visitor)
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _length: usize,
        visitor: V,
    ) -> Result<V::Value, Error> {
        self.deserialize_seq(visitor)
    }

    /// Read an iterable of `[key, value]` pairs, or else an object's own
    /// enumerable string-keyed properties, as `Object.entries` lists them.
    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        if let Some(iterator) = JsIterator::of(self.value)? {
            return visitor.visit_map(PairsAccess {
                iterator,
                next_value: None,
                scope: self.scope.inner()?,
            });
        }
        if self.value.kind()? != Kind::Object {
            return Err(self.invalid_type(&visitor));
        }
        visitor.visit_map(EntriesAccess {
            entries: entries(self.value)?.into_iter(),
            next_value: None,
            scope: self.scope.inner()?,
        })
    }

    /// Read each field by its name, and skip one that is `undefined` and not
    /// `in` the object.
    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        if self.value.kind()? != Kind::Object {
            return Err(self.invalid_type(&visitor));
        }
        visitor.visit_map(ObjectAccess {
            object: self.value,
            fields: fields.iter(),
            next_value: None,
            scope: self.scope.inner()?,
        })
    }

    /// Read `"Variant"` as a unit variant, and `{"Variant": payload}` as any
    /// variant, as serde_json does.
    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        let kind = self.value.kind()?;
        let access = if kind == Kind::String {
            EnumAccess {
                tag: self.value,
                payload: self.value.env().undefined(),
                scope: self.scope,
            }
        } else if kind == Kind::Object {
            let mut entries = entries(self.value)?;
            if entries.len() != 1 {
                return Err(de::Error::invalid_length(entries.len(), &"1"));
            }
            let (tag, payload) = entries.remove(0);
            EnumAccess {
                tag,
                payload,
                scope: self.scope.inner()?,
            }
        } else {
            return Err(self.invalid_type(&visitor));
        };
        visitor.visit_enum(access)
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_unit()
    }

    fn deserialize_bytes<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_byte_buf(visitor)
    }

    fn deserialize_byte_buf<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        if let Some(bytes) = self.as_bytes()? {
            return visitor.visit_byte_buf(bytes);
        }
        if self.value.is_array()? {
            return self.deserialize_seq(visitor);
        }
        Err(self.invalid_type(&visitor))
    }

    fn is_human_readable(&self) -> bool {
        true
    }
}

impl<'de> de::VariantAccess<'de> for Deserializer<'_, '_> {
    type Error = Error;

    fn unit_variant(self) -> Result<(), Error> {
        de::Deserialize::deserialize(self)
    }

    fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value, Error> {
        seed.deserialize(self)
    }

    fn tuple_variant<V: Visitor<'de>>(self, length: usize, visitor: V) -> Result<V::Value, Error> {
        de::Deserializer::deserialize_tuple(self, length, visitor)
    }

    fn struct_variant<V: Visitor<'de>>(
        self,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        de::Deserializer::deserialize_struct(self, "", fields, visitor)
    }
}

/// Return an object's own enumerable string-keyed properties with their
/// values, as `Object.entries` does.
fn entries<'env>(object: Value<'env>) -> Result<Vec<(Value<'env>, Value<'env>)>, Error> {
    let keys = object.keys()?;
    let mut entries = Vec::with_capacity(keys.len());
    for key in keys {
        entries.push((key, object.property(key)?));
    }
    Ok(entries)
}

/// The elements of an array, by index, up to its length when Rust started
/// reading it.
struct ArrayAccess<'names, 'env> {
    array: Value<'env>,
    index: u32,
    length: u32,
    scope: Scope<'names, 'env>,
}

impl<'de> de::SeqAccess<'de> for ArrayAccess<'_, '_> {
    type Error = Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Error> {
        if self.index >= self.length {
            return Ok(None);
        }
        let element = self.array.element(self.index)?;
        self.index += 1;
        seed.deserialize(self.scope.read(element)).map(Some)
    }

    fn size_hint(&self) -> Option<usize> {
        usize::try_from(self.length - self.index).ok()
    }
}

/// The values of a JavaScript iterator, as a sequence.
struct IteratorAccess<'names, 'env> {
    iterator: JsIterator<'env>,
    scope: Scope<'names, 'env>,
}

impl<'de> de::SeqAccess<'de> for IteratorAccess<'_, '_> {
    type Error = Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Error> {
        match self.iterator.next()? {
            Some(value) => seed.deserialize(self.scope.read(value)).map(Some),
            None => Ok(None),
        }
    }
}

/// The `[key, value]` pairs of a JavaScript iterator, whose value waits for
/// its key.
struct PairsAccess<'names, 'env> {
    iterator: JsIterator<'env>,
    next_value: Option<Value<'env>>,
    scope: Scope<'names, 'env>,
}

impl<'de> de::MapAccess<'de> for PairsAccess<'_, '_> {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Error> {
        let Some(pair) = self.iterator.next()? else {
            return Ok(None);
        };
        self.next_value = Some(pair.element(1)?);
        seed.deserialize(self.scope.read(pair.element(0)?))
            .map(Some)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Error> {
        self.scope.read_next_value(&mut self.next_value, seed)
    }
}

/// An object's entries, whose value waits for its key.
struct EntriesAccess<'names, 'env> {
    entries: vec::IntoIter<(Value<'env>, Value<'env>)>,
    next_value: Option<Value<'env>>,
    scope: Scope<'names, 'env>,
}

impl<'de> de::MapAccess<'de> for EntriesAccess<'_, '_> {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Error> {
        let Some((key, value)) = self.entries.next() else {
            return Ok(None);
        };
        self.next_value = Some(value);
        seed.deserialize(self.scope.read(key)).map(Some)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Error> {
        self.scope.read_next_value(&mut self.next_value, seed)
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.entries.len())
    }
}

/// A struct's fields, read by name from an object.
struct ObjectAccess<'names, 'env> {
    object: Value<'env>,
    fields: slice::Iter<'static, &'static str>,
    next_value: Option<Value<'env>>,
    scope: Scope<'names, 'env>,
}

impl<'de> de::MapAccess<'de> for ObjectAccess<'_, '_> {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Error> {
        for field in &mut self.fields {
            let key = self.scope.names.string(field)?;
            let value = self.object.property(key)?;
            // An `undefined` value can be a missing field, which `in` tells.
            if value.kind()? == Kind::Undefined && !self.object.has(key)? {
                continue;
            }
            self.next_value = Some(value);
            let field: StrDeserializer<'_, Error> = field.into_deserializer();
            return seed.deserialize(field).map(Some);
        }
        Ok(None)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Error> {
        self.scope.read_next_value(&mut self.next_value, seed)
    }
}

/// An enum's tag and its payload.
struct EnumAccess<'names, 'env> {
    tag: Value<'env>,
    payload: Value<'env>,
    scope: Scope<'names, 'env>,
}

impl<'de, 'names, 'env> de::EnumAccess<'de> for EnumAccess<'names, 'env> {
    type Error = Error;
    type Variant = Deserializer<'names, 'env>;

    fn variant_seed<V: DeserializeSeed<'de>>(
        self,
        seed: V,
    ) -> Result<(V::Value, Self::Variant), Error> {
        let variant = seed.deserialize(self.scope.read(self.tag))?;
        Ok((variant, self.scope.read(self.payload)))
    }
}

/// Describe a value inside `arrays` as wasm-bindgen's `Debug` for a
/// `JsValue` does, whose `debugString` this follows. An array inside itself,
/// or inside [`MAX_DEPTH`] arrays, reads `[…]`, where `debugString` would
/// recurse until the stack overflows.
fn debug_string<'env>(value: Value<'env>, arrays: &mut Vec<Value<'env>>) -> Result<String, Error> {
    match value.kind()? {
        Kind::Number | Kind::Boolean | Kind::Undefined | Kind::Null => return value.to_js_string(),
        Kind::String => return Ok(format!("\"{}\"", value.as_string()?)),
        Kind::Symbol => {
            let description = value.named(c"description")?;
            return Ok(match description.kind()? {
                Kind::Undefined | Kind::Null => "Symbol".to_owned(),
                _ => format!("Symbol({})", description.to_js_string()?),
            });
        }
        Kind::Function => {
            let name = value.named(c"name")?;
            if name.kind()? == Kind::String {
                let name = name.as_string()?;
                if !name.is_empty() {
                    return Ok(format!("Function({name})"));
                }
            }
            return Ok("Function".to_owned());
        }
        Kind::BigInt | Kind::Object | Kind::External => {}
    }
    if value.is_array()? {
        for array in arrays.iter() {
            if array.strict_equals(value)? {
                return Ok("[…]".to_owned());
            }
        }
        if arrays.len() == MAX_DEPTH {
            return Ok("[…]".to_owned());
        }
        arrays.push(value);
        let mut debug = String::from("[");
        for index in 0..value.array_length()? {
            if index > 0 {
                debug.push_str(", ");
            }
            debug.push_str(&debug_string(value.element(index)?, arrays)?);
        }
        arrays.pop();
        debug.push(']');
        return Ok(debug);
    }
    let env = value.env();
    let global = env.global()?;
    let to_string = global
        .named(c"Object")?
        .named(c"prototype")?
        .named(c"toString")?;
    let described = to_string.call(value, [])?.as_string()?;
    let class_name = described
        .strip_prefix("[object ")
        .and_then(|rest| rest.split(']').next())
        .filter(|name| !name.is_empty());
    let Some(class_name) = class_name else {
        return Ok(described);
    };
    if class_name == "Object" {
        // `JSON.stringify` throws for a cycle, among others.
        let json = global.named(c"JSON")?;
        return match json.named(c"stringify")?.call(json, [value]) {
            Ok(text) => Ok(format!("Object({})", text.to_js_string()?)),
            Err(_) => {
                env.take_exception();
                Ok("Object".to_owned())
            }
        };
    }
    if value.instance_of(global.named(c"Error")?)? {
        return Ok(format!(
            "{}: {}\n{}",
            value.named(c"name")?.to_js_string()?,
            value.named(c"message")?.to_js_string()?,
            value.named(c"stack")?.to_js_string()?
        ));
    }
    Ok(class_name.to_owned())
}
