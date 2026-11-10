//! Serialize Rust values into JavaScript as serde-wasm-bindgen 0.6 does with
//! the settings of the Wasm boundary: `null` for `None` and `()`, a `Map` for
//! each map, a bigint for every 64-bit and 128-bit integer, and a plain
//! `Uint8Array` for bytes.

use std::fmt::Display;

use serde::ser::{self, Serialize};

use super::names::Names;
use crate::runtime::node::{Env, Error, Value};

impl ser::Error for Error {
    fn custom<T: Display>(message: T) -> Self {
        Self::Message(message.to_string())
    }
}

/// A Serde serializer into one environment's values.
pub(super) struct Serializer<'env> {
    env: Env<'env>,
    names: Names<'env>,
}

impl<'env> Serializer<'env> {
    /// Serialize into `env`.
    pub(super) fn new(env: Env<'env>) -> Self {
        Self {
            env,
            names: Names::new(env),
        }
    }
}

/// Wraps a serialized variant's payload as `{"Variant": payload}`, as
/// serde_json does.
pub(super) struct VariantSerializer<'env, S> {
    variant: Value<'env>,
    inner: S,
}

impl<'env, S> VariantSerializer<'env, S> {
    /// Wrap `inner`'s value in an object of the variant's name.
    fn wrap(
        self,
        inner: impl FnOnce(S) -> Result<Value<'env>, Error>,
    ) -> Result<Value<'env>, Error> {
        let value = inner(self.inner)?;
        let object = value.env().object()?;
        object.set_property(self.variant, value)?;
        Ok(object)
    }
}

impl<'env, S> ser::SerializeTupleVariant for VariantSerializer<'env, S>
where
    S: ser::SerializeTupleStruct<Ok = Value<'env>, Error = Error>,
{
    type Ok = Value<'env>;
    type Error = Error;

    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Error> {
        self.inner.serialize_field(value)
    }

    fn end(self) -> Result<Value<'env>, Error> {
        self.wrap(S::end)
    }
}

impl<'env, S> ser::SerializeStructVariant for VariantSerializer<'env, S>
where
    S: ser::SerializeStruct<Ok = Value<'env>, Error = Error>,
{
    type Ok = Value<'env>;
    type Error = Error;

    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        self.inner.serialize_field(key, value)
    }

    fn end(self) -> Result<Value<'env>, Error> {
        self.wrap(S::end)
    }
}

/// Serializes sequences and tuples into arrays.
pub(super) struct ArraySerializer<'serializer, 'env> {
    serializer: &'serializer Serializer<'env>,
    target: Value<'env>,
    index: u32,
}

impl<'serializer, 'env> ArraySerializer<'serializer, 'env> {
    fn new(serializer: &'serializer Serializer<'env>) -> Result<Self, Error> {
        Ok(Self {
            serializer,
            target: serializer.env.array()?,
            index: 0,
        })
    }
}

impl<'env> ser::SerializeSeq for ArraySerializer<'_, 'env> {
    type Ok = Value<'env>;
    type Error = Error;

    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Error> {
        self.target
            .set_element(self.index, value.serialize(self.serializer)?)?;
        self.index += 1;
        Ok(())
    }

    fn end(self) -> Result<Value<'env>, Error> {
        Ok(self.target)
    }
}

impl<'env> ser::SerializeTuple for ArraySerializer<'_, 'env> {
    type Ok = Value<'env>;
    type Error = Error;

    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Error> {
        ser::SerializeSeq::serialize_element(self, value)
    }

    fn end(self) -> Result<Value<'env>, Error> {
        ser::SerializeSeq::end(self)
    }
}

impl<'env> ser::SerializeTupleStruct for ArraySerializer<'_, 'env> {
    type Ok = Value<'env>;
    type Error = Error;

    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Error> {
        ser::SerializeSeq::serialize_element(self, value)
    }

    fn end(self) -> Result<Value<'env>, Error> {
        ser::SerializeSeq::end(self)
    }
}

/// Serializes maps into `Map`s, which the generated `restoreHost` turns into
/// plain objects with an own property for each key. Assigning the keys to an
/// object here would lose a key such as `__proto__`, because the assignment
/// sets the object's prototype instead.
pub(super) struct MapSerializer<'serializer, 'env> {
    serializer: &'serializer Serializer<'env>,
    target: Value<'env>,
    /// `Map.prototype.set`.
    set: Value<'env>,
    next_key: Option<Value<'env>>,
}

impl<'env> ser::SerializeMap for MapSerializer<'_, 'env> {
    type Ok = Value<'env>;
    type Error = Error;

    fn serialize_key<T: ?Sized + Serialize>(&mut self, key: &T) -> Result<(), Error> {
        self.next_key = Some(key.serialize(self.serializer)?);
        Ok(())
    }

    fn serialize_value<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Error> {
        let key = self
            .next_key
            .take()
            .expect("Serde serializes each map key before its value");
        let value = value.serialize(self.serializer)?;
        self.set.call(self.target, [key, value])?;
        Ok(())
    }

    fn end(self) -> Result<Value<'env>, Error> {
        Ok(self.target)
    }
}

/// Serializes structs into plain objects.
pub(super) struct ObjectSerializer<'serializer, 'env> {
    serializer: &'serializer Serializer<'env>,
    target: Value<'env>,
}

impl<'env> ser::SerializeStruct for ObjectSerializer<'_, 'env> {
    type Ok = Value<'env>;
    type Error = Error;

    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        let value = value.serialize(self.serializer)?;
        self.target
            .set_property(self.serializer.names.string(key)?, value)
    }

    fn end(self) -> Result<Value<'env>, Error> {
        Ok(self.target)
    }
}

impl<'serializer, 'env> ser::Serializer for &'serializer Serializer<'env> {
    type Ok = Value<'env>;
    type Error = Error;

    type SerializeSeq = ArraySerializer<'serializer, 'env>;
    type SerializeTuple = ArraySerializer<'serializer, 'env>;
    type SerializeTupleStruct = ArraySerializer<'serializer, 'env>;
    type SerializeTupleVariant = VariantSerializer<'env, ArraySerializer<'serializer, 'env>>;
    type SerializeMap = MapSerializer<'serializer, 'env>;
    type SerializeStruct = ObjectSerializer<'serializer, 'env>;
    type SerializeStructVariant = VariantSerializer<'env, ObjectSerializer<'serializer, 'env>>;

    fn serialize_bool(self, value: bool) -> Result<Value<'env>, Error> {
        self.env.boolean(value)
    }

    fn serialize_i8(self, value: i8) -> Result<Value<'env>, Error> {
        self.env.number(f64::from(value))
    }

    fn serialize_i16(self, value: i16) -> Result<Value<'env>, Error> {
        self.env.number(f64::from(value))
    }

    fn serialize_i32(self, value: i32) -> Result<Value<'env>, Error> {
        self.env.number(f64::from(value))
    }

    fn serialize_i64(self, value: i64) -> Result<Value<'env>, Error> {
        self.env.bigint_i64(value)
    }

    fn serialize_i128(self, value: i128) -> Result<Value<'env>, Error> {
        self.env
            .bigint_words(value < 0, &words(value.unsigned_abs()))
    }

    fn serialize_u8(self, value: u8) -> Result<Value<'env>, Error> {
        self.env.number(f64::from(value))
    }

    fn serialize_u16(self, value: u16) -> Result<Value<'env>, Error> {
        self.env.number(f64::from(value))
    }

    fn serialize_u32(self, value: u32) -> Result<Value<'env>, Error> {
        self.env.number(f64::from(value))
    }

    fn serialize_u64(self, value: u64) -> Result<Value<'env>, Error> {
        self.env.bigint_u64(value)
    }

    fn serialize_u128(self, value: u128) -> Result<Value<'env>, Error> {
        self.env.bigint_words(false, &words(value))
    }

    fn serialize_f32(self, value: f32) -> Result<Value<'env>, Error> {
        self.env.number(f64::from(value))
    }

    fn serialize_f64(self, value: f64) -> Result<Value<'env>, Error> {
        self.env.number(value)
    }

    fn serialize_char(self, value: char) -> Result<Value<'env>, Error> {
        self.env.string(value.encode_utf8(&mut [0; 4]))
    }

    fn serialize_str(self, value: &str) -> Result<Value<'env>, Error> {
        self.env.string(value)
    }

    fn serialize_bytes(self, value: &[u8]) -> Result<Value<'env>, Error> {
        self.env.typed_array(value)
    }

    fn serialize_none(self) -> Result<Value<'env>, Error> {
        self.serialize_unit()
    }

    fn serialize_some<T: ?Sized + Serialize>(self, value: &T) -> Result<Value<'env>, Error> {
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<Value<'env>, Error> {
        self.env.null()
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<Value<'env>, Error> {
        self.serialize_unit()
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
    ) -> Result<Value<'env>, Error> {
        self.names.string(variant)
    }

    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<Value<'env>, Error> {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<Value<'env>, Error> {
        let inner = value.serialize(self)?;
        VariantSerializer {
            variant: self.names.string(variant)?,
            inner,
        }
        .wrap(Ok)
    }

    fn serialize_seq(self, _length: Option<usize>) -> Result<Self::SerializeSeq, Error> {
        ArraySerializer::new(self)
    }

    fn serialize_tuple(self, _length: usize) -> Result<Self::SerializeTuple, Error> {
        ArraySerializer::new(self)
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _length: usize,
    ) -> Result<Self::SerializeTupleStruct, Error> {
        ArraySerializer::new(self)
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        _length: usize,
    ) -> Result<Self::SerializeTupleVariant, Error> {
        Ok(VariantSerializer {
            variant: self.names.string(variant)?,
            inner: ArraySerializer::new(self)?,
        })
    }

    fn serialize_map(self, _length: Option<usize>) -> Result<Self::SerializeMap, Error> {
        let (target, set) = self.env.map()?;
        Ok(MapSerializer {
            serializer: self,
            target,
            set,
            next_key: None,
        })
    }

    fn serialize_struct(
        self,
        _name: &'static str,
        _length: usize,
    ) -> Result<Self::SerializeStruct, Error> {
        Ok(ObjectSerializer {
            serializer: self,
            target: self.env.object()?,
        })
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        length: usize,
    ) -> Result<Self::SerializeStructVariant, Error> {
        Ok(VariantSerializer {
            variant: self.names.string(variant)?,
            inner: self.serialize_struct(variant, length)?,
        })
    }
}

/// Split a 128-bit magnitude into a bigint's 64-bit words, the least
/// significant first.
fn words(magnitude: u128) -> [u64; 2] {
    [magnitude as u64, (magnitude >> 64) as u64]
}
