//! Serialize Rust values into JavaScript as serde-wasm-bindgen 0.6 does with
//! the settings of the Wasm boundary: `null` for `None` and `()`, a `Map` for
//! each map, a bigint for every 64-bit and 128-bit integer, and a plain
//! `Uint8Array` for bytes.
//!
//! An object, an array or a map is made in one call of a JavaScript function
//! with its values: an object by the function of its keys, whose literal
//! gives every object of those keys one shape, for less than adding each
//! property through Node-API. Objects and arrays are frozen, as the generated
//! `restoreHost` leaves them, so that it returns one whose schema restores
//! nothing else as it is.

use std::cell::{OnceCell, RefCell};
use std::fmt::{Display, Write as _};
use std::ptr;

use serde::ser::{self, Serialize};

use super::names::Names;
use crate::convert::short_text::ShortText;
use crate::runtime::node::{Env, Error, Gathered, Value};

/// The most values that one call takes; a longer array or map is filled one
/// value at a time instead, so that the call's arguments stay small on the
/// stack.
const MOST_ARGUMENTS: usize = 4096;

impl ser::Error for Error {
    fn custom<T: Display>(message: T) -> Self {
        Self::Message(message.to_string())
    }
}

/// A Serde serializer into one environment's values.
pub(super) struct Serializer<'env> {
    env: Env<'env>,
    names: Names<'env>,
    /// The values of the containers that are being made.
    gathered: Gathered<'env>,
    /// The keys of the objects that are being made, a stack as `gathered` is.
    keys: RefCell<Vec<&'static str>>,
    /// The function of each list of keys that this conversion has made an
    /// object of. Serde passes the same `&'static str` keys for every value
    /// of a type.
    objects: RefCell<Vec<ObjectFactory<'env>>>,
    array_of: OnceCell<Value<'env>>,
}

/// The function that makes an object of `keys`, in order.
struct ObjectFactory<'env> {
    keys: Box<[&'static str]>,
    function: Value<'env>,
}

impl<'env> Serializer<'env> {
    /// Serialize into `env`.
    pub(super) fn new(env: Env<'env>) -> Self {
        Self {
            env,
            names: Names::new(env),
            gathered: Gathered::default(),
            keys: RefCell::default(),
            objects: RefCell::default(),
            array_of: OnceCell::new(),
        }
    }

    /// The function that makes an array of its arguments.
    fn array_of(&self) -> Result<Value<'env>, Error> {
        if let Some(array_of) = self.array_of.get() {
            return Ok(*array_of);
        }
        let array_of = self.env.array_of()?;
        Ok(*self.array_of.get_or_init(|| array_of))
    }

    /// Make the object whose keys are on `keys` from `key_mark` on, and whose
    /// values are gathered from `mark` on.
    fn object(&self, key_mark: usize, mark: usize) -> Result<Value<'env>, Error> {
        let factory = {
            let keys = self.keys.borrow();
            let keys = &keys[key_mark..];
            let kept = self.objects.borrow().iter().find_map(|factory| {
                let same = factory.keys.len() == keys.len()
                    && factory
                        .keys
                        .iter()
                        .zip(keys)
                        .all(|(kept, key)| ptr::eq(*kept, *key));
                same.then_some(factory.function)
            });
            match kept {
                Some(function) => function,
                None => {
                    let function = self.env.object_of(keys)?;
                    self.objects.borrow_mut().push(ObjectFactory {
                        keys: keys.into(),
                        function,
                    });
                    function
                }
            }
        };
        self.keys.borrow_mut().truncate(key_mark);
        self.gathered.call(factory, mark)
    }
}

/// Wraps a serialized variant's payload as `{"Variant": payload}`, as
/// serde_json does.
pub(super) struct VariantSerializer<'serializer, 'env, S> {
    serializer: &'serializer Serializer<'env>,
    variant: &'static str,
    inner: S,
}

impl<'env, S> VariantSerializer<'_, 'env, S> {
    /// Wrap `inner`'s value in an object of the variant's name.
    fn wrap(
        self,
        inner: impl FnOnce(S) -> Result<Value<'env>, Error>,
    ) -> Result<Value<'env>, Error> {
        let value = inner(self.inner)?;
        let serializer = self.serializer;
        let (key_mark, mark) = (serializer.keys.borrow().len(), serializer.gathered.mark());
        serializer.gathered.push(value);
        serializer.keys.borrow_mut().push(self.variant);
        serializer.object(key_mark, mark)
    }
}

impl<'env, S> ser::SerializeTupleVariant for VariantSerializer<'_, 'env, S>
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

impl<'env, S> ser::SerializeStructVariant for VariantSerializer<'_, 'env, S>
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
    mark: usize,
}

impl<'serializer, 'env> ArraySerializer<'serializer, 'env> {
    fn new(serializer: &'serializer Serializer<'env>) -> Self {
        Self {
            serializer,
            mark: serializer.gathered.mark(),
        }
    }
}

impl<'env> ser::SerializeSeq for ArraySerializer<'_, 'env> {
    type Ok = Value<'env>;
    type Error = Error;

    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Error> {
        let value = value.serialize(self.serializer)?;
        self.serializer.gathered.push(value);
        Ok(())
    }

    fn end(self) -> Result<Value<'env>, Error> {
        let Self { serializer, mark } = self;
        if serializer.gathered.since(mark) <= MOST_ARGUMENTS {
            return serializer.gathered.call(serializer.array_of()?, mark);
        }
        let array = serializer.env.array()?;
        for (index, value) in (0..).zip(serializer.gathered.take(serializer.env, mark)) {
            array.set_element(index, value)?;
        }
        Ok(array)
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

/// Serializes maps into frozen plain objects with an own property for each
/// key, or a map of more entries into a `Map`, which the generated
/// `restoreHost` turns into such an object.
pub(super) struct MapSerializer<'serializer, 'env> {
    serializer: &'serializer Serializer<'env>,
    /// Where the keys and the values, in turn, start.
    mark: usize,
}

impl<'env> ser::SerializeMap for MapSerializer<'_, 'env> {
    type Ok = Value<'env>;
    type Error = Error;

    fn serialize_key<T: ?Sized + Serialize>(&mut self, key: &T) -> Result<(), Error> {
        let key = key.serialize(self.serializer)?;
        self.serializer.gathered.push(key);
        Ok(())
    }

    fn serialize_value<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Error> {
        let value = value.serialize(self.serializer)?;
        self.serializer.gathered.push(value);
        Ok(())
    }

    fn end(self) -> Result<Value<'env>, Error> {
        let Self { serializer, mark } = self;
        let env = serializer.env;
        if serializer.gathered.since(mark) <= MOST_ARGUMENTS {
            return serializer.gathered.call(env.map_of()?, mark);
        }
        let (map, set) = env.map()?;
        let entries = serializer.gathered.take(env, mark);
        for entry in entries.chunks_exact(2) {
            set.call(map, [entry[0], entry[1]])?;
        }
        Ok(map)
    }
}

/// Serializes structs into plain objects.
pub(super) struct ObjectSerializer<'serializer, 'env> {
    serializer: &'serializer Serializer<'env>,
    key_mark: usize,
    mark: usize,
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
        self.serializer.gathered.push(value);
        self.serializer.keys.borrow_mut().push(key);
        Ok(())
    }

    fn end(self) -> Result<Value<'env>, Error> {
        self.serializer.object(self.key_mark, self.mark)
    }
}

impl<'serializer, 'env> ser::Serializer for &'serializer Serializer<'env> {
    type Ok = Value<'env>;
    type Error = Error;

    type SerializeSeq = ArraySerializer<'serializer, 'env>;
    type SerializeTuple = ArraySerializer<'serializer, 'env>;
    type SerializeTupleStruct = ArraySerializer<'serializer, 'env>;
    type SerializeTupleVariant =
        VariantSerializer<'serializer, 'env, ArraySerializer<'serializer, 'env>>;
    type SerializeMap = MapSerializer<'serializer, 'env>;
    type SerializeStruct = ObjectSerializer<'serializer, 'env>;
    type SerializeStructVariant =
        VariantSerializer<'serializer, 'env, ObjectSerializer<'serializer, 'env>>;

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

    /// Write text that fits on the stack there, such as a date-time's, and
    /// longer text to the heap.
    fn collect_str<T: ?Sized + Display>(self, value: &T) -> Result<Value<'env>, Error> {
        let mut text = ShortText::new();
        if write!(text, "{value}").is_ok() {
            return self.env.string(text.as_str());
        }
        self.env.string(&value.to_string())
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
            serializer: self,
            variant,
            inner,
        }
        .wrap(Ok)
    }

    fn serialize_seq(self, _length: Option<usize>) -> Result<Self::SerializeSeq, Error> {
        Ok(ArraySerializer::new(self))
    }

    fn serialize_tuple(self, _length: usize) -> Result<Self::SerializeTuple, Error> {
        Ok(ArraySerializer::new(self))
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _length: usize,
    ) -> Result<Self::SerializeTupleStruct, Error> {
        Ok(ArraySerializer::new(self))
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        _length: usize,
    ) -> Result<Self::SerializeTupleVariant, Error> {
        Ok(VariantSerializer {
            serializer: self,
            variant,
            inner: ArraySerializer::new(self),
        })
    }

    fn serialize_map(self, _length: Option<usize>) -> Result<Self::SerializeMap, Error> {
        Ok(MapSerializer {
            serializer: self,
            mark: self.gathered.mark(),
        })
    }

    fn serialize_struct(
        self,
        _name: &'static str,
        _length: usize,
    ) -> Result<Self::SerializeStruct, Error> {
        Ok(ObjectSerializer {
            serializer: self,
            key_mark: self.keys.borrow().len(),
            mark: self.gathered.mark(),
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
            serializer: self,
            variant,
            inner: self.serialize_struct(variant, length)?,
        })
    }
}

/// Split a 128-bit magnitude into a bigint's 64-bit words, the least
/// significant first.
fn words(magnitude: u128) -> [u64; 2] {
    [magnitude as u64, (magnitude >> 64) as u64]
}
